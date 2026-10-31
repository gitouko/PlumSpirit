//! Standard MIDI File → [`PlumDocument`].
//!
//! # Note pairing rule (documented and tested)
//!
//! Notes are paired per track, per `(channel, pitch)`, **first-in-first-out**: a note
//! termination (Note Off, or Note On with velocity 0) ends the *oldest* still-active
//! note with the same channel and pitch. Example:
//!
//! ```text
//! ON C4 @0, ON C4 @10, OFF C4 @20, OFF C4 @30   =>   C4 0→20, C4 10→30
//! ```
//!
//! Notes are appended to the track's note store at their Note On, so the store is in
//! (start tick, Note On order) order without any sort. The duration is patched in when
//! the matching termination arrives.
//!
//! # Unmatched events
//!
//! A termination with no active note, and a Note On still active at End of Track, are
//! *not* turned into invented notes. They are kept as raw channel events at their
//! original position, so export writes them back unchanged.
//!
//! # Same-tick ordering
//!
//! Each preserved event records how many note on/off events preceded it at the same
//! tick (its `anchor`), which lets the exporter reproduce the original interleaving.
//!
//! # Conductor
//!
//! After all tracks are read, tempo, time-signature, Marker/Cue Point events and
//! PlumSpirit chapter records move from the tracks into the document's single
//! Conductor (see `Conductor::adopt`); PlumSpirit gap fillers are dropped as they
//! are read.

use super::error::{ImportWarning, MidiError, MidiErrorKind};
use super::reader::{ByteSource, SmfHandler, Token, decode_track_bytes, read_smf};
use crate::model::document::{ExtraChunk, PlumDocument, SmfFormat};
use crate::model::events::{EventStore, EventStoreError, RawEvent, RawKind};
use crate::model::note::{NoteAttrs, NoteOffKind};
use crate::model::note_store::{NoteStore, NoteStoreBuilder, NoteStoreError};
use crate::model::project::KeyRange;
use crate::model::timing::{Division, TimingKind};
use crate::model::track::{Track, TrackId};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

/// Optional progress reporting and cancellation for background imports.
#[derive(Default, Clone, Copy)]
pub struct ImportControl<'a> {
    /// Set to true to abort the import at the next check (every 256 KiB of input).
    pub cancel: Option<&'a AtomicBool>,
    /// Receives the number of input bytes consumed so far.
    pub progress_bytes: Option<&'a AtomicU64>,
    /// v1.0-Launch L-D05: the compatibility reading (deterministic repairs
    /// of out-of-spec files, each reported; `reader` module documentation).
    /// Off: the strict reading.
    pub compatibility: bool,
    /// L-H02: receives the [`ImportStage`] the import is in (`as u8`), so a
    /// progress display never claims "done" while chunks are still decoded,
    /// the document assembled or checked.
    pub stage: Option<&'a AtomicU8>,
}

/// L-H02: the stages of an import, in order (`ImportControl::stage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ImportStage {
    /// Reading the file (`progress_bytes` counts the bytes).
    Reading = 0,
    /// Every byte read; the last track chunks are being decoded.
    Decoding = 1,
    /// Building the document from the tracks (and its structure).
    Assembling = 2,
    /// The strict-export fingerprint and the counts.
    Checking = 3,
    /// The document is ready.
    Done = 4,
}

impl ImportStage {
    pub fn from_u8(v: u8) -> ImportStage {
        match v {
            0 => ImportStage::Reading,
            1 => ImportStage::Decoding,
            2 => ImportStage::Assembling,
            3 => ImportStage::Checking,
            _ => ImportStage::Done,
        }
    }

    fn set(self, control: &ImportControl<'_>) {
        if let Some(s) = control.stage {
            s.store(self as u8, Ordering::Relaxed);
        }
    }
}

/// Summary produced alongside the document (logged, and shown in the status bar).
#[derive(Clone, Debug)]
pub struct ImportSummary {
    pub source_format: SmfFormat,
    pub division: Division,
    pub track_count: usize,
    pub note_count: usize,
    pub preserved_event_count: usize,
    pub source_bytes: u64,
    pub duration: Duration,
    pub warnings: Vec<ImportWarning>,
    /// `MTrk` chunks in the file (before PlumSpirit layers are merged back).
    pub source_chunks: usize,
    /// Bit `c` set when a note uses MIDI channel `c` (0–15).
    pub channels_used: u16,
    /// v0.9: how the key bytes were read — [`KeyRange::Extended256`] for a
    /// 256-key import (the document is a 256-Key Mode project).
    pub keys: KeyRange,
    /// Notes above key 127 (only a 256-key import has them).
    pub wide_notes: usize,
    /// v1.0-Launch L-D05: read in Compatibility Mode (the repairs are in
    /// `warnings`).
    pub compatibility: bool,
    /// The resulting structure: subchannels over all tracks, and tracks that have
    /// a parent (subtracks).
    pub subchannel_count: usize,
    pub subtrack_count: usize,
    /// v1.0-beta1 (B1-0161): wall time of each import stage, for the log and
    /// `examples/import_profile.rs`.
    pub stages: ImportStages,
}

/// Wall time of the stages of one import (B1-0161). `parse` covers reading,
/// hashing and decoding the track chunks into note / event stores (with
/// note pairing); the rest runs once over the finished tracks.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImportStages {
    pub parse: Duration,
    /// `PlumDocument::from_parts_adopting` (Conductor adoption).
    pub assemble: Duration,
    /// PlumSpirit structure records (subtracks / subchannels).
    pub structure: Duration,
    /// Strict-export provenance: trailing-byte hash + export fingerprint.
    pub provenance: Duration,
    /// Channels / subchannels / subtracks counted for the summary.
    pub counts: Duration,
}

impl ImportStages {
    /// One line for the log: "parse 2.81 s, assemble 0.01 s, …".
    pub fn describe(&self) -> String {
        let f = |d: Duration| format!("{:.3} s", d.as_secs_f64());
        format!(
            "parse {}, assemble {}, structure {}, provenance {}, counts {}",
            f(self.parse),
            f(self.assemble),
            f(self.structure),
            f(self.provenance),
            f(self.counts)
        )
    }
}

impl ImportSummary {
    pub fn describe(&self) -> String {
        let mut s = format!(
            "Format {}, {}, {} track(s), {} note(s), {} other event(s)",
            self.source_format.word(),
            self.division.describe(),
            self.track_count,
            super::error::group_digits(self.note_count as u64),
            super::error::group_digits(self.preserved_event_count as u64),
        );
        if self.keys == KeyRange::Extended256 {
            s.push_str(&format!(
                ", read as a non-standard 256-key file ({} note(s) above key 127)",
                super::error::group_digits(self.wide_notes as u64)
            ));
        }
        s
    }

    /// The structural mapping (contract §72): what the file contains and what it
    /// became in PlumSpirit. Two lines, shown with the opened file's details.
    pub fn structure_lines(&self) -> [String; 2] {
        let g = |n: usize| super::error::group_digits(n as u64);
        let top = self.track_count - self.subtrack_count.min(self.track_count);
        [
            format!(
                "Source MIDI: {} track(s), {} MIDI channel(s) used, {} note(s).",
                g(self.source_chunks),
                self.channels_used.count_ones(),
                g(self.note_count)
            ),
            format!(
                "PlumSpirit result: {} top-level track(s), {} subchannel(s), {} subtrack(s).",
                g(top),
                g(self.subchannel_count),
                g(self.subtrack_count)
            ),
        ]
    }

    /// Short form for the status line: "5 subchannel(s), 0 subtrack(s)".
    pub fn structure_short(&self) -> String {
        let g = |n: usize| super::error::group_digits(n as u64);
        format!(
            "{} subchannel(s), {} subtrack(s)",
            g(self.subchannel_count),
            g(self.subtrack_count)
        )
    }
}

/// Channels used, subchannels and subtracks of a freshly imported document.
/// The channel scan reads only the one-byte attribute column of each note and
/// stops as soon as all 16 channels have been seen.
fn structure_counts(doc: &PlumDocument) -> (u16, usize, usize) {
    let mut channels = 0u16;
    let (mut subs, mut subtracks) = (0usize, 0usize);
    for t in doc.tracks() {
        subs += t.subchannels().len();
        subtracks += usize::from(t.parent().is_some());
        if channels != u16::MAX {
            channels |= t.notes.channels_used();
        }
    }
    (channels, subs, subtracks)
}

/// Imports a MIDI file from disk. The source file is only read, never modified.
pub fn import_file(
    path: &Path,
    control: ImportControl<'_>,
) -> Result<(PlumDocument, ImportSummary), MidiError> {
    import_file_with(path, control, KeyRange::Standard)
}

/// [`import_file`] with a key reading: [`KeyRange::Extended256`] reads a
/// non-standard 256-key file (v0.9 §062, Kiva's rule: the key byte of a Note
/// On / Off with an explicit status byte is unmasked, 0x80–0xFF = keys
/// 128–255) into a 256-Key Mode project. [`KeyRange::Standard`] refuses such a
/// file with [`MidiErrorKind::WideKeyNote`] instead of misreading it.
pub fn import_file_with(
    path: &Path,
    control: ImportControl<'_>,
    keys: KeyRange,
) -> Result<(PlumDocument, ImportSummary), MidiError> {
    let file = File::open(path)?;
    let len = file.metadata().ok().map(|m| m.len());
    let (mut doc, summary) = import_reader_with(file, len, control, keys)?;
    // v1.0-beta1 (B1-0121): the file strict export may rewrite. A path that
    // is not valid Unicode is not recorded (no strict export for it).
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let (Some(src), Some(p)) = (doc.project_mut().source.as_mut(), abs.to_str()) {
        src.path = p.to_owned();
    }
    Ok((doc, summary))
}

/// Imports from an in-memory byte slice (tests, fixtures).
pub fn import_bytes(bytes: &[u8]) -> Result<(PlumDocument, ImportSummary), MidiError> {
    import_bytes_with(bytes, KeyRange::Standard)
}

/// [`import_bytes`] with a key reading (see [`import_file_with`]).
pub fn import_bytes_with(
    bytes: &[u8],
    keys: KeyRange,
) -> Result<(PlumDocument, ImportSummary), MidiError> {
    import_reader_with(
        bytes,
        Some(bytes.len() as u64),
        ImportControl::default(),
        keys,
    )
}

pub fn import_reader<R: std::io::Read>(
    reader: R,
    total_len: Option<u64>,
    control: ImportControl<'_>,
) -> Result<(PlumDocument, ImportSummary), MidiError> {
    import_reader_with(reader, total_len, control, KeyRange::Standard)
}

/// [`import_reader`] with a key reading (see [`import_file_with`]).
pub fn import_reader_with<R: std::io::Read>(
    reader: R,
    total_len: Option<u64>,
    control: ImportControl<'_>,
    keys: KeyRange,
) -> Result<(PlumDocument, ImportSummary), MidiError> {
    let threads = decode_threads(total_len);
    import_reader_threads(
        reader,
        total_len,
        control,
        keys,
        threads,
        super::workers::window_for(threads),
    )
}

/// [`import_reader_with`] with `threads` decoding workers (0 = in place) and
/// a chunk-data budget (see [`decode_smf`]).
pub(crate) fn import_reader_threads<R: std::io::Read>(
    reader: R,
    total_len: Option<u64>,
    control: ImportControl<'_>,
    keys: KeyRange,
    threads: usize,
    budget: u64,
) -> Result<(PlumDocument, ImportSummary), MidiError> {
    let started = Instant::now();
    ImportStage::Reading.set(&control);
    // Everything read is hashed on the way (B1-0121, strict export); bytes
    // after the last chunk are read and hashed afterwards.
    let mut hashing = super::provenance::HashingReader::new(reader);
    let mut src = ByteSource::new(&mut hashing, total_len);
    let mut h = ImportHandler::new(control, keys);
    decode_smf(&mut src, &mut h, threads, budget)?;
    let source_bytes = src.position();
    drop(src);
    let mut stages = ImportStages {
        parse: started.elapsed(),
        ..ImportStages::default()
    };
    let mark = Instant::now();
    let digest = hashing.finish().ok();
    stages.provenance = mark.elapsed();
    if let Some(p) = control.progress_bytes {
        p.store(source_bytes, Ordering::Relaxed);
    }
    let format = SmfFormat::from_word(h.format)
        .ok_or_else(|| MidiError::new(MidiErrorKind::UnsupportedFormat(h.format)))?;
    let division = Division::from_raw(h.division);
    match division.kind() {
        TimingKind::Ppqn(0) => h.warnings.push(ImportWarning::ZeroPpqn),
        TimingKind::Smpte {
            frames_per_second, ..
        } if !matches!(frames_per_second, 24 | 25 | 29 | 30) => {
            h.warnings.push(ImportWarning::UnusualSmpteRate {
                fps: frames_per_second,
            });
        }
        _ => {}
    }
    if format == SmfFormat::SingleTrack && h.tracks.len() > 1 {
        h.warnings.push(ImportWarning::Format0WithSeveralTracks {
            tracks: h.tracks.len(),
        });
    }
    let source_chunks = h.tracks.len();
    ImportStage::Assembling.set(&control);
    let mark = Instant::now();
    let (mut doc, adopted) = PlumDocument::from_parts_adopting(format, division, h.tracks, h.extra);
    // A 256-key reading opens a 256-Key Mode project (its notes may be above
    // 127, which a standard project never holds).
    doc.project_mut().keys = keys;
    h.warnings
        .extend(adopted.notes().into_iter().map(ImportWarning::Conductor));
    stages.assemble = mark.elapsed();
    let mark = Instant::now();
    if let Some(m) = super::structure::restore_structure(&mut doc).message() {
        h.warnings.push(ImportWarning::Structure(m));
    }
    stages.structure = mark.elapsed();
    // The project as imported: strict export is offered while it is unchanged
    // (the path is filled in by `import_file_with`).
    ImportStage::Checking.set(&control);
    let mark = Instant::now();
    // A file read in Compatibility Mode is not a source the strict export
    // may re-read as standard MIDI (L-D05).
    if let Some((size, hash)) = digest.filter(|_| !control.compatibility) {
        doc.project_mut().source = Some(super::provenance::record(&doc, "", size, hash));
    }
    stages.provenance += mark.elapsed();
    let mark = Instant::now();
    let (channels_used, subchannel_count, subtrack_count) = structure_counts(&doc);
    stages.counts = mark.elapsed();
    let summary = ImportSummary {
        source_chunks,
        channels_used,
        keys,
        wide_notes: h.wide_notes,
        compatibility: control.compatibility,
        subchannel_count,
        subtrack_count,
        source_format: format,
        division,
        track_count: doc.track_count(),
        note_count: doc.note_count(),
        preserved_event_count: doc.preserved_event_count(),
        source_bytes,
        duration: started.elapsed(),
        warnings: h.warnings,
        stages,
    };
    ImportStage::Done.set(&control);
    Ok((doc, summary))
}

/// Imports a single track's events into a note store + event store. Shared by the
/// document importer and by export validation.
pub(crate) struct TrackBuilder {
    notes: NoteStoreBuilder,
    events: EventStore,
    /// Active note-ons per `channel << key_shift | pitch`, oldest first.
    active: Vec<VecDeque<PendingOn>>,
    /// 7 (keys 0–127) or 8 (256-key reading: keys 0–255).
    key_shift: u32,
    /// Notes above key 127 (256-key reading).
    wide_notes: usize,
    active_count: usize,
    current_tick: u64,
    note_events_at_tick: u32,
    unmatched_off: usize,
    track: usize,
}

#[derive(Clone, Copy)]
struct PendingOn {
    global: usize,
    /// Index in the event list at the time of the Note On (for unmatched notes).
    raw_pos: usize,
    anchor: u32,
    tick: u64,
    status: u8,
    pitch: u8,
    velocity: u8,
}

fn store_err(e: NoteStoreError) -> MidiError {
    match e {
        NoteStoreError::EndOverflow { .. } => MidiError::new(MidiErrorKind::TickOverflow),
        _ => MidiError::new(MidiErrorKind::OutOfMemory),
    }
}

fn event_err(e: EventStoreError) -> MidiError {
    match e {
        EventStoreError::ArenaFull => MidiError::new(MidiErrorKind::PayloadTooLarge),
        EventStoreError::Unsorted => MidiError::new(MidiErrorKind::TickOverflow),
        EventStoreError::OutOfMemory => MidiError::new(MidiErrorKind::OutOfMemory),
    }
}

impl TrackBuilder {
    pub(crate) fn new(track: usize, keys: KeyRange) -> Self {
        let key_shift = if keys == KeyRange::Extended256 { 8 } else { 7 };
        TrackBuilder {
            notes: NoteStoreBuilder::new(),
            events: EventStore::new(),
            active: (0..16usize << key_shift).map(|_| VecDeque::new()).collect(),
            key_shift,
            wide_notes: 0,
            active_count: 0,
            current_tick: 0,
            note_events_at_tick: 0,
            unmatched_off: 0,
            track,
        }
    }

    fn advance(&mut self, tick: u64) {
        if tick != self.current_tick {
            self.current_tick = tick;
            self.note_events_at_tick = 0;
        }
    }

    fn push_raw(&mut self, tick: u64, kind: RawKind) -> Result<(), MidiError> {
        self.events
            .push(RawEvent {
                tick,
                anchor: self.note_events_at_tick,
                kind,
            })
            .map_err(event_err)
    }

    pub(crate) fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        self.advance(token.tick());
        match token {
            Token::Channel { tick, status, data } => {
                let kind = status & 0xF0;
                let channel = status & 0x0F;
                let key = (usize::from(channel) << self.key_shift) | usize::from(data[0]);
                let is_on = kind == 0x90 && data[1] > 0;
                let is_off = kind == 0x80 || (kind == 0x90 && data[1] == 0);
                if is_on {
                    // A key above 127 arrives only from a 256-key reading.
                    let attrs = if data[0] > 0x7F {
                        self.wide_notes += 1;
                        NoteAttrs::new_wide(data[0], data[1], channel, 0, NoteOffKind::NoteOff)
                    } else {
                        NoteAttrs::new(data[0], data[1], channel, 0, NoteOffKind::NoteOff)
                    }
                    .map_err(|_| MidiError::new(MidiErrorKind::StatusInData(data[0])))?;
                    let global = self.notes.push(tick, attrs).map_err(store_err)?;
                    self.active[key].push_back(PendingOn {
                        global,
                        raw_pos: self.events.len(),
                        anchor: self.note_events_at_tick,
                        tick,
                        status,
                        pitch: data[0],
                        velocity: data[1],
                    });
                    self.active_count += 1;
                    self.note_events_at_tick = self.note_events_at_tick.saturating_add(1);
                } else if is_off {
                    if let Some(p) = self.active[key].pop_front() {
                        self.active_count -= 1;
                        let (off_vel, off_kind) = if kind == 0x80 {
                            (data[1], NoteOffKind::NoteOff)
                        } else {
                            (0, NoteOffKind::NoteOnZero)
                        };
                        // Ticks only grow within a track: the end is never
                        // before the start.
                        self.notes
                            .end_note(p.global, tick, off_vel, off_kind)
                            .map_err(|_| MidiError::new(MidiErrorKind::StatusInData(off_vel)))?;
                        self.note_events_at_tick = self.note_events_at_tick.saturating_add(1);
                    } else {
                        // Unmatched termination: preserve verbatim.
                        self.unmatched_off += 1;
                        self.push_raw(tick, RawKind::Channel { status, data })?;
                        self.note_events_at_tick = self.note_events_at_tick.saturating_add(1);
                    }
                } else {
                    self.push_raw(tick, RawKind::Channel { status, data })?;
                }
            }
            Token::Meta {
                tick,
                meta_type,
                payload,
            } => {
                // PlumSpirit gap fillers only bridge long delta times; the ticks of
                // the surrounding events already carry that time.
                if super::seqspec::is_gap_filler(meta_type, payload) {
                    return Ok(());
                }
                let r = self.events.store_payload(payload).map_err(event_err)?;
                self.push_raw(
                    tick,
                    RawKind::Meta {
                        meta_type,
                        payload: r,
                    },
                )?;
            }
            Token::SysEx { tick, payload } => {
                let r = self.events.store_payload(payload).map_err(event_err)?;
                self.push_raw(tick, RawKind::SysEx { payload: r })?;
            }
            Token::SysExEscape { tick, payload } => {
                let r = self.events.store_payload(payload).map_err(event_err)?;
                self.push_raw(tick, RawKind::SysExEscape { payload: r })?;
            }
            Token::System { tick, status, data } => {
                self.push_raw(tick, RawKind::System { status, data })?;
            }
        }
        Ok(())
    }

    /// Finishes the track. Returns the stores, the unmatched-event counts and
    /// the number of notes above key 127 (256-key reading).
    pub(crate) fn finish(mut self) -> (NoteStore, EventStore, usize, usize, usize) {
        // Note-ons still active at End of Track become raw events at their original
        // positions instead of invented notes.
        let mut pending: Vec<PendingOn> = Vec::with_capacity(self.active_count);
        for q in &mut self.active {
            pending.extend(q.drain(..));
        }
        pending.sort_by_key(|p| p.global);
        let unmatched_on = pending.len();
        let wide_notes = self.wide_notes - pending.iter().filter(|p| p.pitch > 0x7F).count();
        let removed: Vec<usize> = pending.iter().map(|p| p.global).collect();
        let inserts: Vec<(usize, RawEvent)> = pending
            .iter()
            .map(|p| {
                (
                    p.raw_pos,
                    RawEvent {
                        tick: p.tick,
                        anchor: p.anchor,
                        kind: RawKind::Channel {
                            status: p.status,
                            data: [p.pitch, p.velocity],
                        },
                    },
                )
            })
            .collect();
        self.events.insert_at_positions(&inserts);
        self.events.shrink_to_fit();
        let notes = self.notes.finish(&removed);
        (
            notes,
            self.events,
            self.unmatched_off,
            unmatched_on,
            wide_notes,
        )
    }

    pub(crate) fn track_index(&self) -> usize {
        self.track
    }
}

/// Chunk data held for the workers at once (see [`super::workers`]).
#[cfg(test)]
pub(crate) const DEFER_BUDGET: u64 = super::workers::WINDOW;

fn decode_threads(total_len: Option<u64>) -> usize {
    super::workers::threads_for(total_len)
}

/// Runs [`read_smf`] with `threads` decoding workers (0 = everything in place,
/// the sequential reader). Chunks whose data fits `budget` are read by this
/// thread (which also hashes the stream) and decoded on the workers; the
/// outcome — tracks, their order, every warning in file order, and on failure
/// the error of the first failing track — is the same as decoding in place.
pub(crate) fn decode_smf<R: std::io::Read>(
    src: &mut ByteSource<R>,
    h: &mut ImportHandler<'_>,
    threads: usize,
    budget: u64,
) -> Result<(), MidiError> {
    if threads == 0 {
        return read_smf(src, h);
    }
    let cancel = h.control.cancel;
    let compat = h.control.compatibility;
    let keys = h.keys;
    let file_len = src.total_len();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let job_rx = Mutex::new(job_rx);
    let (done_tx, done_rx) = mpsc::channel::<(usize, u64, Result<TrackDone, MidiError>)>();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let done_tx = done_tx.clone();
            let job_rx = &job_rx;
            scope.spawn(move || {
                loop {
                    let job = match job_rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let len = job.bytes.len() as u64;
                    let r = decode_job(job, file_len, keys, compat, cancel);
                    if done_tx.send((r.0, len, r.1)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(done_tx);
        h.par = Some(Parallel {
            jobs: Some(job_tx),
            done: done_rx,
            in_flight: 0,
            budget,
            finished: BTreeMap::new(),
            slots: Vec::new(),
        });
        let read = read_smf(src, h);
        // Every byte is read; the workers finish the chunks in flight.
        ImportStage::Decoding.set(&h.control);
        let mut par = h.par.take().expect("set above");
        // No more jobs: the workers end once the queue is empty.
        par.jobs = None;
        for (index, _, r) in par.done.iter() {
            par.finished.insert(index, r);
        }
        h.replay(par, read)
    })
}

/// One deferred track chunk.
struct Job {
    index: usize,
    data_start: u64,
    bytes: Vec<u8>,
}

/// A decoded track: the track, the warnings its decoding gave (in order;
/// empty for a track decoded in place, whose warnings were recorded as they
/// came), and its unmatched-event / wide-note counts.
struct TrackDone {
    track: Track,
    warnings: Vec<ImportWarning>,
    unmatched_off: usize,
    unmatched_on: usize,
    wide_notes: usize,
}

fn finish_track(b: TrackBuilder, index: usize, eot: Option<u64>) -> Result<TrackDone, MidiError> {
    let (notes, events, unmatched_off, unmatched_on, wide_notes) = b.finish();
    let id = TrackId(u32::try_from(index).map_err(|_| MidiError::new(MidiErrorKind::OutOfMemory))?);
    Ok(TrackDone {
        track: Track::from_parts(id, notes, events, eot),
        warnings: Vec::new(),
        unmatched_off,
        unmatched_on,
        wide_notes,
    })
}

/// Decodes one deferred chunk on a worker thread.
fn decode_job(
    job: Job,
    file_len: Option<u64>,
    keys: KeyRange,
    compat: bool,
    cancel: Option<&AtomicBool>,
) -> (usize, Result<TrackDone, MidiError>) {
    let index = job.index;
    let mut w = TrackWorker {
        b: TrackBuilder::new(index, keys),
        warnings: Vec::new(),
        cancel,
        extended: keys == KeyRange::Extended256,
        compat,
    };
    let r = decode_track_bytes(&job.bytes, job.data_start, file_len, index, &mut w);
    drop(job.bytes);
    let r = r.and_then(|eot| {
        let mut done = finish_track(w.b, index, eot)?;
        done.warnings = w.warnings;
        Ok(done)
    });
    (index, r)
}

/// The handler a worker decodes one track chunk with.
struct TrackWorker<'a> {
    b: TrackBuilder,
    warnings: Vec<ImportWarning>,
    cancel: Option<&'a AtomicBool>,
    extended: bool,
    compat: bool,
}

impl SmfHandler for TrackWorker<'_> {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        self.b.token(token)
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn warning(&mut self, warning: ImportWarning) {
        self.warnings.push(warning);
    }
    fn extended_keys(&self) -> bool {
        self.extended
    }
    fn compatibility(&self) -> bool {
        self.compat
    }
    fn progress(&mut self, _position: u64) -> Result<(), MidiError> {
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(MidiError::new(MidiErrorKind::Cancelled));
        }
        Ok(())
    }
}

/// What happened, in file order, while decoding with workers.
enum Slot {
    Warning(ImportWarning),
    /// A track decoded on a worker (its result is in `Parallel::finished`).
    Deferred(usize),
    /// A track decoded in place (its warnings are the slots before it).
    InPlace(Box<TrackDone>),
}

struct Parallel {
    jobs: Option<mpsc::Sender<Job>>,
    done: mpsc::Receiver<(usize, u64, Result<TrackDone, MidiError>)>,
    in_flight: u64,
    budget: u64,
    finished: BTreeMap<usize, Result<TrackDone, MidiError>>,
    slots: Vec<Slot>,
}

pub(crate) struct ImportHandler<'a> {
    control: ImportControl<'a>,
    keys: KeyRange,
    wide_notes: usize,
    format: u16,
    division: u16,
    tracks: Vec<Track>,
    extra: Vec<ExtraChunk>,
    warnings: Vec<ImportWarning>,
    current: Option<TrackBuilder>,
    /// Set while decoding with workers ([`decode_smf`]).
    par: Option<Parallel>,
}

impl<'a> ImportHandler<'a> {
    pub(crate) fn new(control: ImportControl<'a>, keys: KeyRange) -> Self {
        ImportHandler {
            control,
            keys,
            wide_notes: 0,
            format: 1,
            division: 96,
            tracks: Vec::new(),
            extra: Vec::new(),
            warnings: Vec::new(),
            current: None,
            par: None,
        }
    }

    /// Adds a finished track (and its warnings, in order) to the result.
    fn accept(&mut self, index: usize, done: TrackDone) {
        self.warnings.extend(done.warnings);
        self.wide_notes += done.wide_notes;
        if done.unmatched_off > 0 {
            self.warnings.push(ImportWarning::UnmatchedNoteOff {
                track: index,
                count: done.unmatched_off,
            });
        }
        if done.unmatched_on > 0 {
            self.warnings.push(ImportWarning::UnmatchedNoteOn {
                track: index,
                count: done.unmatched_on,
            });
        }
        self.tracks.push(done.track);
    }

    /// Puts the outcome of a decode with workers together in file order. The
    /// first deferred track that failed decides the error (decoding in place
    /// would have stopped there), before an error `read_smf` met later.
    fn replay(&mut self, mut par: Parallel, read: Result<(), MidiError>) -> Result<(), MidiError> {
        if let Some(e) = par.finished.values().find_map(|r| r.as_ref().err()) {
            return Err(e.clone());
        }
        read?;
        for slot in std::mem::take(&mut par.slots) {
            match slot {
                Slot::Warning(w) => self.warnings.push(w),
                Slot::InPlace(done) => {
                    let index = done.track.id().0 as usize;
                    self.accept(index, *done);
                }
                Slot::Deferred(index) => {
                    let done = par
                        .finished
                        .remove(&index)
                        .expect("every deferred track reports back")?;
                    self.accept(index, done);
                }
            }
        }
        Ok(())
    }
}

impl SmfHandler for ImportHandler<'_> {
    fn header(&mut self, format: u16, _declared: u16, division: u16) -> Result<(), MidiError> {
        self.format = format;
        self.division = division;
        Ok(())
    }

    fn begin_track(&mut self, index: usize) -> Result<(), MidiError> {
        self.current = Some(TrackBuilder::new(index, self.keys));
        Ok(())
    }

    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        match self.current.as_mut() {
            Some(b) => b.token(token),
            None => Ok(()),
        }
    }

    fn end_track(&mut self, index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        let Some(b) = self.current.take() else {
            return Ok(());
        };
        debug_assert_eq!(b.track_index(), index);
        let done = finish_track(b, index, eot)?;
        match self.par.as_mut() {
            Some(par) => par.slots.push(Slot::InPlace(Box::new(done))),
            None => self.accept(index, done),
        }
        Ok(())
    }

    fn extra_chunk(
        &mut self,
        chunk_type: [u8; 4],
        data: Vec<u8>,
        after: usize,
    ) -> Result<(), MidiError> {
        self.extra.push(ExtraChunk {
            chunk_type,
            data,
            after_track_chunks: after,
        });
        Ok(())
    }

    fn warning(&mut self, warning: ImportWarning) {
        match self.par.as_mut() {
            Some(par) => par.slots.push(Slot::Warning(warning)),
            None => self.warnings.push(warning),
        }
    }

    fn extended_keys(&self) -> bool {
        self.keys == KeyRange::Extended256
    }

    fn compatibility(&self) -> bool {
        self.control.compatibility
    }

    fn progress(&mut self, position: u64) -> Result<(), MidiError> {
        if let Some(p) = self.control.progress_bytes {
            p.store(position, Ordering::Relaxed);
        }
        if let Some(c) = self.control.cancel
            && c.load(Ordering::Relaxed)
        {
            return Err(MidiError::new(MidiErrorKind::Cancelled));
        }
        Ok(())
    }

    fn defer_track(&mut self, _index: usize, len: u64) -> bool {
        self.par.as_ref().is_some_and(|p| len <= p.budget)
    }

    fn track_bytes(
        &mut self,
        index: usize,
        data_start: u64,
        bytes: Vec<u8>,
    ) -> Result<(), MidiError> {
        let len = bytes.len() as u64;
        let cancel = self.control.cancel;
        let par = self.par.as_mut().expect("only offered with workers");
        // Hold at most `budget` bytes of chunk data: wait for a worker to
        // finish before reading further.
        while par.in_flight > 0 && par.in_flight + len > par.budget {
            match par.done.recv_timeout(Duration::from_millis(50)) {
                Ok((i, n, r)) => {
                    par.in_flight -= n;
                    par.finished.insert(i, r);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                        return Err(MidiError::new(MidiErrorKind::Cancelled));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // Results that are already in free their bytes too.
        while let Ok((i, n, r)) = par.done.try_recv() {
            par.in_flight -= n;
            par.finished.insert(i, r);
        }
        par.in_flight += len;
        par.slots.push(Slot::Deferred(index));
        if let Some(jobs) = par.jobs.as_ref() {
            // A send fails only if every worker is gone; the replay then
            // reports the missing track.
            let _ = jobs.send(Job {
                index,
                data_start,
                bytes,
            });
        }
        self.progress(data_start + len)
    }
}

/// B1-0161: decoding track chunks on worker threads gives exactly what
/// decoding them in place gives — the document, every warning in order, and
/// on failure the same error — whatever the number of workers and however
/// small the chunk-data budget.
#[cfg(test)]
mod parallel_tests {
    use super::*;
    use crate::midi::test_smf::{EOT, chunk, mutations, rich, smf};

    type Outcome = Result<(u64, usize, usize, Vec<ImportWarning>, usize), MidiError>;

    fn outcome(bytes: &[u8], keys: KeyRange, threads: usize, budget: u64) -> Outcome {
        import_reader_threads(
            bytes,
            Some(bytes.len() as u64),
            ImportControl::default(),
            keys,
            threads,
            budget,
        )
        .map(|(doc, s)| {
            (
                doc.export_fingerprint(),
                doc.note_count(),
                doc.preserved_event_count(),
                s.warnings,
                s.wide_notes,
            )
        })
    }

    fn assert_same(bytes: &[u8]) {
        for keys in [KeyRange::Standard, KeyRange::Extended256] {
            let in_place = outcome(bytes, keys, 0, 0);
            for (threads, budget) in [(1, DEFER_BUDGET), (2, 1), (2, 24), (3, DEFER_BUDGET)] {
                assert_eq!(
                    outcome(bytes, keys, threads, budget),
                    in_place,
                    "{threads} worker(s), budget {budget}, {keys:?}"
                );
            }
        }
    }

    #[test]
    fn workers_give_the_same_document_and_warnings_as_decoding_in_place() {
        let f = rich();
        let r = outcome(&f, KeyRange::Standard, 0, 0).expect("the rich file imports");
        assert!(
            r.1 > 0 && r.3.len() >= 6,
            "the case exercises notes and warnings"
        );
        assert_same(&f);
    }

    #[test]
    fn the_first_failing_track_decides_the_error() {
        let ok = chunk(
            b"MTrk",
            &[(0, &[0x90, 60, 100]), (1, &[0x80, 60, 0]), (0, EOT)],
        );
        let bad = chunk(b"MTrk", &[(0, &[0x90, 60, 0x90]), (0, EOT)]);
        let worse = chunk(b"MTrk", &[(0, &[0xF4]), (0, EOT)]);
        let mut truncated = chunk(b"MTrk", &[(0, &[0x90, 60, 100]), (100, &[0x80, 60, 0])]);
        truncated.truncate(truncated.len() - 2);
        for f in [
            smf(4, &[ok.clone(), bad.clone(), worse.clone(), ok.clone()]),
            smf(4, &[ok.clone(), worse.clone(), bad.clone()]),
            smf(3, &[ok.clone(), bad.clone(), truncated.clone()]),
            smf(2, &[ok.clone(), truncated]),
            smf(3, &[bad, ok.clone(), ok]),
        ] {
            assert!(outcome(&f, KeyRange::Standard, 0, 0).is_err());
            assert_same(&f);
        }
    }

    #[test]
    fn mutated_files_decode_alike_with_and_without_workers() {
        for f in mutations(&rich(), 1500) {
            assert_same(&f);
        }
    }

    #[test]
    fn a_set_cancel_flag_stops_a_decode_with_workers() {
        let cancel = AtomicBool::new(true);
        let f = rich();
        let r = import_reader_threads(
            &f[..],
            Some(f.len() as u64),
            ImportControl {
                cancel: Some(&cancel),
                progress_bytes: None,
                compatibility: false,
                stage: None,
            },
            KeyRange::Standard,
            2,
            DEFER_BUDGET,
        );
        assert!(r.err().is_some_and(|e| e.is_cancelled()));
    }

    /// L-H02: the stage reaches Done only with the document; a cancelled
    /// import stops before it.
    #[test]
    fn the_import_stage_reaches_done_only_with_the_document() {
        let f = rich();
        for threads in [0, 2] {
            let stage = AtomicU8::new(9);
            let r = import_reader_threads(
                &f[..],
                Some(f.len() as u64),
                ImportControl {
                    stage: Some(&stage),
                    ..ImportControl::default()
                },
                KeyRange::Standard,
                threads,
                DEFER_BUDGET,
            );
            assert!(r.is_ok());
            assert_eq!(
                stage.load(Ordering::Relaxed),
                ImportStage::Done as u8,
                "{threads}"
            );
            if threads == 0 {
                continue; // (in place, cancelling is checked every 256 KiB)
            }
            let cancel = AtomicBool::new(true);
            let stage = AtomicU8::new(9);
            let r = import_reader_threads(
                &f[..],
                Some(f.len() as u64),
                ImportControl {
                    cancel: Some(&cancel),
                    stage: Some(&stage),
                    ..ImportControl::default()
                },
                KeyRange::Standard,
                threads,
                DEFER_BUDGET,
            );
            assert!(r.is_err());
            assert!(
                stage.load(Ordering::Relaxed) < ImportStage::Done as u8,
                "{threads}"
            );
        }
        assert_eq!(ImportStage::from_u8(2), ImportStage::Assembling);
        assert_eq!(ImportStage::from_u8(200), ImportStage::Done);
    }
}
