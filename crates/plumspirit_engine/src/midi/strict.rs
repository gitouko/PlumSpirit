//! Strict no-edit export (v1.0-beta1 B1-0121 / B1-0123, contract §019).
//!
//! A project imported from a MIDI file and not edited since (see
//! [`super::provenance`]) is written from the original file instead of from
//! the editable model: every chunk in its order, every event of every track
//! in its order with its tick — duplicate tempo events, a tempo on another
//! track than the first, same-tick order, Note On velocity 0 against Note
//! Off, names, SysEx, unknown chunks — exactly as the source has them. Only
//! the encoding of status bytes follows the setting:
//!
//! * [`StatusBytes::Running`] ("Use Running Status Compression" on): a
//!   channel message omits its status byte when it repeats the previous one;
//!   the status is written again after every meta, SysEx or system event
//!   (SMF 1.0) and for a key above 127 in a 256-key file. A compact,
//!   standard source comes out byte for byte.
//! * [`StatusBytes::Explicit`] (off): every channel message carries its
//!   status byte.
//!
//! Nothing of the file is held in memory: it is streamed through the shared
//! tokenizer ([`super::reader`]) while its size and hash are checked against
//! the record made at the import, so a file that changed is never written
//! as if it were the project's. The output goes to a temporary file, is read
//! back by the same tokenizer and must decode to the same ordered events
//! (a digest of every token, End of Track and chunk) before it replaces the
//! destination.
//!
//! What a writer cannot keep — bytes after an End of Track, a header longer
//! than 6 bytes, a wrong declared track count, trailing garbage, a missing
//! End of Track (added at the track's last event), non-standard running
//! status across meta events — is normalised and listed in
//! [`StrictReport::normalised`].

use super::error::{ImportWarning, MidiError, MidiErrorKind, group_digits};
use super::export::{ExportControl, ExportError, ExportStage, TempGuard, temp_path_for};
use super::provenance::{HashingReader, SourceStatus, source_status};
use super::reader::{ByteSource, SmfHandler, Token, decode_track_bytes, read_smf};
use super::vlq::write_vlq;
use super::workers::{self, Pipeline};
use crate::codec::WordHash;
use crate::model::PlumDocument;
use crate::model::events::{channel_data_len, system_data_len};
use crate::model::project::KeyRange;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How channel status bytes are written (B1-0123).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StatusBytes {
    /// Running status: a repeated status byte is omitted (the compact form).
    #[default]
    Running,
    /// Every channel message carries its status byte.
    Explicit,
}

/// Outcome of a successful strict export.
#[derive(Clone, Debug)]
pub struct StrictReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub tracks: usize,
    /// Events written (End of Track included).
    pub events: u64,
    /// Note Ons with a velocity above 0.
    pub notes: u64,
    /// Status bytes running status left out (0 with [`StatusBytes::Explicit`]).
    pub status_bytes_saved: u64,
    /// `WordHash` of the written file (a strict export over the source file
    /// keeps the project's provenance with it).
    pub hash: u64,
    /// The file is byte for byte the source.
    pub identical_to_source: bool,
    /// What the source had that a writer cannot keep, as sentences.
    pub normalised: Vec<String>,
    pub write_duration: Duration,
    pub validate_duration: Duration,
}

impl StrictReport {
    /// One line for the status bar.
    pub fn summary(&self) -> String {
        let how = if self.identical_to_source {
            "byte for byte the original file".to_owned()
        } else {
            "the original file's events in their order".to_owned()
        };
        let mut s = format!(
            "Strict export: {} tracks, {} events, {} bytes — {how}.",
            group_digits(self.tracks as u64),
            group_digits(self.events),
            group_digits(self.bytes)
        );
        if self.status_bytes_saved > 0 {
            s.push_str(&format!(
                " Running status saved {} bytes.",
                group_digits(self.status_bytes_saved)
            ));
        }
        s
    }
}

/// The ordered content of a MIDI file as the tokenizer reads it: header,
/// every track chunk (the digest of its tokens and End of Track, in order),
/// every other chunk in place. Equal digests = the same events in the same
/// order. Chunk digests are made separately so chunks can be read on worker
/// threads (B1-0162).
#[derive(Clone)]
struct Digest {
    h: WordHash,
}

impl Digest {
    fn new() -> Self {
        Digest { h: WordHash::new() }
    }
    fn header(&mut self, format: u16, division: u16) {
        self.h
            .words(&[0x4D54_6864, u64::from(format), u64::from(division)]);
    }
    fn chunk(&mut self, digest: u64) {
        self.h.pair(0x4D54_726B, digest);
    }
    fn extra_chunk(&mut self, chunk_type: [u8; 4], data: &[u8], after: usize) {
        self.h
            .pair(u64::from(u32::from_be_bytes(chunk_type)), after as u64);
        self.h.field(data);
    }
    fn finish(&self, tracks: usize) -> u64 {
        let mut h = self.h.clone();
        h.word(tracks as u64);
        h.finish()
    }
}

/// Folds a chunk's End of Track into its token digest.
fn chunk_digest(mut h: WordHash, eot: u64) -> u64 {
    h.pair(0x454F_5400, eot);
    h.finish()
}

/// Encodes one track chunk's tokens (with the status-byte setting) and digests
/// them. Shared by the in-place writer and the workers.
struct ChunkEnc {
    buf: Vec<u8>,
    explicit: bool,
    last_tick: u64,
    running: Option<u8>,
    hash: WordHash,
    events: u64,
    notes: u64,
    saved: u64,
}

impl ChunkEnc {
    fn new(explicit: bool, capacity: usize) -> Self {
        ChunkEnc {
            buf: Vec::with_capacity(capacity),
            explicit,
            last_tick: 0,
            running: None,
            hash: WordHash::new(),
            events: 0,
            notes: 0,
            saved: 0,
        }
    }

    /// Starts a new chunk (the counters keep running).
    fn begin(&mut self) {
        self.last_tick = 0;
        self.running = None;
        self.hash = WordHash::new();
    }

    fn delta(&mut self, tick: u64) {
        write_vlq(tick - self.last_tick, &mut self.buf);
        self.last_tick = tick;
    }

    fn token(&mut self, t: &Token<'_>) {
        super::compare::hash_token(&mut self.hash, t);
        self.events += 1;
        match *t {
            Token::Channel { tick, status, data } => {
                self.delta(tick);
                // A key above 127 (256-key file) always gets its status, and
                // the next message too (Kiva's rule; see `export.rs`).
                let wide_key = data[0] > 0x7F;
                if wide_key || self.explicit || self.running != Some(status) {
                    self.buf.push(status);
                    self.running = if wide_key { None } else { Some(status) };
                } else {
                    self.saved += 1;
                }
                self.buf
                    .extend_from_slice(&data[..channel_data_len(status)]);
                if status & 0xF0 == 0x90 && data[1] > 0 {
                    self.notes += 1;
                }
            }
            Token::Meta {
                tick,
                meta_type,
                payload,
            } => {
                self.delta(tick);
                self.running = None;
                self.buf.extend_from_slice(&[0xFF, meta_type]);
                write_vlq(payload.len() as u64, &mut self.buf);
                self.buf.extend_from_slice(payload);
            }
            Token::SysEx { tick, payload } | Token::SysExEscape { tick, payload } => {
                let status = if matches!(t, Token::SysEx { .. }) {
                    0xF0
                } else {
                    0xF7
                };
                self.delta(tick);
                self.running = None;
                self.buf.push(status);
                write_vlq(payload.len() as u64, &mut self.buf);
                self.buf.extend_from_slice(payload);
            }
            Token::System { tick, status, data } => {
                self.delta(tick);
                self.running = None;
                self.buf.push(status);
                let n = system_data_len(status).unwrap_or(0);
                self.buf.extend_from_slice(&data[..n]);
            }
        }
    }

    /// Ends the chunk at `eot` (the last event's tick when the source had
    /// none); returns the chunk's digest.
    fn end(&mut self, eot: u64) -> u64 {
        self.events += 1;
        self.delta(eot);
        self.buf.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        chunk_digest(std::mem::take(&mut self.hash), eot)
    }
}

/// A source track chunk handed to a worker.
struct ChunkJob {
    index: usize,
    data_start: u64,
    bytes: Vec<u8>,
}

/// A chunk a worker re-encoded: its data, digest, counts, whether it had no
/// End of Track, and the warnings its decoding gave.
struct Encoded {
    data: Vec<u8>,
    digest: u64,
    events: u64,
    notes: u64,
    saved: u64,
    missing_eot: bool,
    warnings: Vec<ImportWarning>,
}

/// Decodes and re-encodes one source chunk on a worker.
struct ChunkWorker<'a> {
    enc: ChunkEnc,
    wide: bool,
    cancel: Option<&'a std::sync::atomic::AtomicBool>,
    since_check: u32,
    warnings: Vec<ImportWarning>,
}

impl SmfHandler for ChunkWorker<'_> {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        self.enc.token(&t);
        self.since_check += 1;
        if self.since_check >= 65_536 {
            self.since_check = 0;
            if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(MidiError::new(MidiErrorKind::Cancelled));
            }
        }
        Ok(())
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
        self.wide
    }
}

fn encode_job(
    job: ChunkJob,
    file_len: Option<u64>,
    explicit: bool,
    wide: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Encoded, MidiError> {
    let mut w = ChunkWorker {
        enc: ChunkEnc::new(explicit, job.bytes.len() + job.bytes.len() / 8 + 16),
        wide,
        cancel,
        since_check: 0,
        warnings: Vec::new(),
    };
    let eot = decode_track_bytes(&job.bytes, job.data_start, file_len, job.index, &mut w)?;
    drop(job.bytes);
    let missing_eot = eot.is_none();
    let eot = eot.unwrap_or(w.enc.last_tick);
    let digest = w.enc.end(eot);
    Ok(Encoded {
        data: w.enc.buf,
        digest,
        events: w.enc.events,
        notes: w.enc.notes,
        saved: w.enc.saved,
        missing_eot,
        warnings: w.warnings,
    })
}

/// Writes the source's chunks as the tokenizer reads them.
struct Writer<'a, 'p, W: Write + Seek> {
    w: &'a mut W,
    wide: bool,
    control: ExportControl<'a>,
    enc: ChunkEnc,
    chunk_start: u64,
    chunk_len: u64,
    digest: Digest,
    tracks: usize,
    since_check: u32,
    missing_eot: Vec<usize>,
    warnings: Vec<ImportWarning>,
    /// A write failed (the tokenizer only carries `MidiError`s).
    write_error: Option<io::Error>,
    cancelled: bool,
    /// A chunk decoded on a worker failed (the first one in file order).
    deferred_error: Option<MidiError>,
    pipe: Option<&'p mut Pipeline<ChunkJob, Result<Encoded, MidiError>>>,
}

impl<W: Write + Seek> Writer<'_, '_, W> {
    fn io<T>(&mut self, r: io::Result<T>) -> Result<T, MidiError> {
        r.map_err(|e| {
            self.write_error = Some(e);
            MidiError::new(MidiErrorKind::Cancelled)
        })
    }

    fn flush(&mut self) -> Result<(), MidiError> {
        let r = self.w.write_all(&self.enc.buf);
        self.io(r)?;
        self.chunk_len += self.enc.buf.len() as u64;
        self.enc.buf.clear();
        Ok(())
    }

    fn report_notes(&self) {
        if let Some(p) = self.control.progress_notes {
            p.store(self.enc.notes, Ordering::Relaxed);
        }
    }

    /// Writes the oldest chunk the workers re-encoded (chunks are written in
    /// file order).
    fn write_oldest(&mut self) -> Result<(), MidiError> {
        let cancel = self.control.cancel;
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let Some(index) = pipe.oldest() else {
            return Ok(());
        };
        let Some(r) = pipe.take(index, cancel) else {
            self.cancelled = true;
            return Err(MidiError::new(MidiErrorKind::Cancelled));
        };
        let e = match r {
            Ok(e) => e,
            Err(err) => {
                if err.is_cancelled() {
                    self.cancelled = true;
                } else if self.deferred_error.is_none() {
                    self.deferred_error = Some(err);
                }
                return Err(MidiError::new(MidiErrorKind::Cancelled));
            }
        };
        self.warnings.extend(e.warnings);
        if e.missing_eot {
            self.missing_eot.push(index);
        }
        self.digest.chunk(e.digest);
        self.enc.events += e.events;
        self.enc.notes += e.notes;
        self.enc.saved += e.saved;
        let Ok(len) = u32::try_from(e.data.len()) else {
            return self.io(Err(io::Error::other("a track chunk is larger than 4 GiB")));
        };
        let r = self
            .w
            .write_all(b"MTrk")
            .and_then(|()| self.w.write_all(&len.to_be_bytes()))
            .and_then(|()| self.w.write_all(&e.data));
        self.io(r)?;
        self.tracks += 1;
        self.report_notes();
        if self
            .control
            .cancel
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            self.cancelled = true;
            return Err(MidiError::new(MidiErrorKind::Cancelled));
        }
        Ok(())
    }

    /// Writes every chunk still with the workers.
    fn drain(&mut self) -> Result<(), MidiError> {
        while self.pipe.as_ref().is_some_and(|p| p.oldest().is_some()) {
            self.write_oldest()?;
        }
        Ok(())
    }
}

impl<W: Write + Seek> SmfHandler for Writer<'_, '_, W> {
    fn header(&mut self, format: u16, declared: u16, division: u16) -> Result<(), MidiError> {
        self.digest.header(format, division);
        let mut h = [0u8; 14];
        h[..4].copy_from_slice(b"MThd");
        h[4..8].copy_from_slice(&6u32.to_be_bytes());
        h[8..10].copy_from_slice(&format.to_be_bytes());
        h[10..12].copy_from_slice(&declared.to_be_bytes());
        h[12..14].copy_from_slice(&division.to_be_bytes());
        let r = self.w.write_all(&h);
        self.io(r)
    }

    fn begin_track(&mut self, _index: usize) -> Result<(), MidiError> {
        self.drain()?;
        let r = self.w.stream_position();
        self.chunk_start = self.io(r)?;
        let r = self.w.write_all(b"MTrk\0\0\0\0");
        self.io(r)?;
        self.chunk_len = 0;
        self.enc.begin();
        Ok(())
    }

    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        self.enc.token(&t);
        if self.enc.buf.len() >= 60 * 1024 {
            self.flush()?;
        }
        self.since_check += 1;
        if self.since_check >= 65_536 {
            self.since_check = 0;
            self.report_notes();
            if self
                .control
                .cancel
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                self.cancelled = true;
                return Err(MidiError::new(MidiErrorKind::Cancelled));
            }
        }
        Ok(())
    }

    fn end_track(&mut self, index: usize, end_of_track_tick: Option<u64>) -> Result<(), MidiError> {
        let eot = match end_of_track_tick {
            Some(t) => t,
            None => {
                self.missing_eot.push(index);
                self.enc.last_tick
            }
        };
        let d = self.enc.end(eot);
        self.digest.chunk(d);
        self.flush()?;
        let Ok(len) = u32::try_from(self.chunk_len) else {
            return self.io(Err(io::Error::other("a track chunk is larger than 4 GiB")));
        };
        let end = self.chunk_start + 8 + self.chunk_len;
        let start = self.chunk_start;
        let r = self.w.seek(SeekFrom::Start(start + 4));
        self.io(r)?;
        let r = self.w.write_all(&len.to_be_bytes());
        self.io(r)?;
        let r = self.w.seek(SeekFrom::Start(end));
        self.io(r)?;
        self.tracks += 1;
        Ok(())
    }

    fn extra_chunk(
        &mut self,
        chunk_type: [u8; 4],
        data: Vec<u8>,
        after_track_chunks: usize,
    ) -> Result<(), MidiError> {
        self.drain()?;
        self.digest
            .extra_chunk(chunk_type, &data, after_track_chunks);
        let mut head = [0u8; 8];
        head[..4].copy_from_slice(&chunk_type);
        let Ok(len) = u32::try_from(data.len()) else {
            return self.io(Err(io::Error::other("a chunk is larger than 4 GiB")));
        };
        head[4..].copy_from_slice(&len.to_be_bytes());
        let r = self
            .w
            .write_all(&head)
            .and_then(|()| self.w.write_all(&data));
        self.io(r)
    }

    fn warning(&mut self, warning: ImportWarning) {
        // Warnings keep their file order: the workers' chunks come first.
        if self.drain().is_ok() {
            self.warnings.push(warning);
        }
    }

    fn extended_keys(&self) -> bool {
        self.wide
    }

    fn defer_track(&mut self, _index: usize, len: u64) -> bool {
        self.pipe.as_ref().is_some_and(|p| len <= p.window())
    }

    fn track_bytes(
        &mut self,
        index: usize,
        data_start: u64,
        bytes: Vec<u8>,
    ) -> Result<(), MidiError> {
        let len = bytes.len() as u64;
        while self.pipe.as_ref().is_some_and(|p| !p.has_room(len)) {
            self.write_oldest()?;
        }
        if let Some(pipe) = self.pipe.as_mut() {
            pipe.submit(
                index,
                ChunkJob {
                    index,
                    data_start,
                    bytes,
                },
                len,
            );
        }
        Ok(())
    }
}

/// Re-reads a written file into a [`Digest`].
struct Reread<'p> {
    digest: Digest,
    chunk: WordHash,
    wide: bool,
    tracks: usize,
    /// A chunk decoded on a worker failed (the first one in file order).
    deferred_error: Option<MidiError>,
    pipe: Option<&'p mut Pipeline<ChunkJob, Result<u64, MidiError>>>,
}

impl Reread<'_> {
    fn take_oldest(&mut self) -> Result<(), MidiError> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let Some(index) = pipe.oldest() else {
            return Ok(());
        };
        match pipe.take(index, None) {
            Some(Ok(d)) => {
                self.digest.chunk(d);
                self.tracks += 1;
                Ok(())
            }
            Some(Err(e)) => {
                if self.deferred_error.is_none() {
                    self.deferred_error = Some(e.clone());
                }
                Err(e)
            }
            None => Err(MidiError::new(MidiErrorKind::Cancelled)),
        }
    }

    fn drain(&mut self) -> Result<(), MidiError> {
        while self.pipe.as_ref().is_some_and(|p| p.oldest().is_some()) {
            self.take_oldest()?;
        }
        Ok(())
    }
}

/// Digests one written chunk on a worker.
struct ChunkHasher {
    hash: WordHash,
    wide: bool,
}

impl SmfHandler for ChunkHasher {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        super::compare::hash_token(&mut self.hash, &t);
        Ok(())
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn warning(&mut self, _warning: ImportWarning) {}
    fn extended_keys(&self) -> bool {
        self.wide
    }
}

impl SmfHandler for Reread<'_> {
    fn header(&mut self, format: u16, _declared: u16, division: u16) -> Result<(), MidiError> {
        self.digest.header(format, division);
        Ok(())
    }
    fn begin_track(&mut self, _index: usize) -> Result<(), MidiError> {
        self.drain()?;
        self.chunk = WordHash::new();
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        super::compare::hash_token(&mut self.chunk, &t);
        Ok(())
    }
    fn end_track(&mut self, _index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        // The writer always writes one; a missing one fails the comparison.
        let d = chunk_digest(std::mem::take(&mut self.chunk), eot.unwrap_or(u64::MAX));
        self.digest.chunk(d);
        self.tracks += 1;
        Ok(())
    }
    fn extra_chunk(&mut self, t: [u8; 4], data: Vec<u8>, after: usize) -> Result<(), MidiError> {
        self.drain()?;
        self.digest.extra_chunk(t, &data, after);
        Ok(())
    }
    fn warning(&mut self, _warning: ImportWarning) {}
    fn extended_keys(&self) -> bool {
        self.wide
    }
    fn defer_track(&mut self, _index: usize, len: u64) -> bool {
        self.pipe.as_ref().is_some_and(|p| len <= p.window())
    }
    fn track_bytes(
        &mut self,
        index: usize,
        data_start: u64,
        bytes: Vec<u8>,
    ) -> Result<(), MidiError> {
        let len = bytes.len() as u64;
        while self.pipe.as_ref().is_some_and(|p| !p.has_room(len)) {
            self.take_oldest()?;
        }
        if let Some(pipe) = self.pipe.as_mut() {
            pipe.submit(
                index,
                ChunkJob {
                    index,
                    data_start,
                    bytes,
                },
                len,
            );
        }
        Ok(())
    }
}

/// The sentences for what the writer normalised.
fn normalised(warnings: &[ImportWarning], missing_eot: &[usize]) -> Vec<String> {
    let mut out: Vec<String> = warnings
        .iter()
        .filter(|w| {
            matches!(
                w,
                ImportWarning::DataAfterEndOfTrack { .. }
                    | ImportWarning::TrackCountMismatch { .. }
                    | ImportWarning::TrailingBytes { .. }
                    | ImportWarning::LongHeader { .. }
                    | ImportWarning::ChunkLengthPastEof { .. }
                    | ImportWarning::RunningStatusAcrossMeta { .. }
            )
        })
        .map(ImportWarning::user_message)
        .collect();
    for t in missing_eot {
        out.push(format!(
            "Track {} had no End of Track; one was written at its last event.",
            t + 1
        ));
    }
    out
}

/// What the writing pass ended with.
struct WriterOutcome {
    read: Result<(), MidiError>,
    write_error: Option<io::Error>,
    cancelled: bool,
    tracks: usize,
    events: u64,
    notes: u64,
    saved: u64,
    expected: u64,
    normalised: Vec<String>,
}

/// Writes the project's source file to `dest` (see the module docs). Refused
/// — with the reason, nothing written — unless [`source_status`] is
/// [`SourceStatus::Unchanged`].
pub fn export_strict(
    doc: &PlumDocument,
    dest: &Path,
    status: StatusBytes,
    control: ExportControl<'_>,
) -> Result<StrictReport, ExportError> {
    export_strict_threads(doc, dest, status, control, None, workers::WINDOW)
}

/// [`export_strict`] with a worker count (`None`: by the file's size, see
/// [`super::workers`]; `Some(0)`: one thread) and a chunk window. Chunks are
/// re-encoded and re-read on the workers and written in file order: the
/// output, the report and the first error are the same (B1-0162).
pub(crate) fn export_strict_threads(
    doc: &PlumDocument,
    dest: &Path,
    status: StatusBytes,
    control: ExportControl<'_>,
    threads: Option<usize>,
    window: u64,
) -> Result<StrictReport, ExportError> {
    match source_status(doc) {
        SourceStatus::Unchanged => {}
        other => return Err(ExportError::Blocked(other.sentence())),
    }
    let src = doc
        .project()
        .source
        .clone()
        .expect("Unchanged has a source");
    let wide = doc.project().keys == KeyRange::Extended256;
    let started = Instant::now();

    let mut guard = TempGuard { path: None };
    let mut file = None;
    for attempt in 0..16 {
        let p = temp_path_for(dest, attempt);
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(f) => {
                guard.path = Some(p);
                file = Some(f);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ExportError::io(ExportStage::CreateTemp, e)),
        }
    }
    let Some(file) = file else {
        return Err(ExportError::Io {
            stage: ExportStage::CreateTemp,
            kind: io::ErrorKind::AlreadyExists,
            message: "too many leftover temporary files".into(),
        });
    };
    let temp = guard.path.clone().expect("set with file");
    let mut out = BufWriter::with_capacity(1 << 20, file);

    let source = File::open(&src.path)
        .map_err(|e| ExportError::Blocked(SourceStatus::SourceMissing(e.to_string()).sentence()))?;
    let mut hashing = HashingReader::new(BufReader::with_capacity(1 << 20, source));
    let explicit = status == StatusBytes::Explicit;
    let file_len = Some(src.size);
    let cancel = control.cancel;
    let encode = |job: ChunkJob| encode_job(job, file_len, explicit, wide, cancel);
    let written = {
        let out = &mut out;
        let hashing = &mut hashing;
        let mut body = move |pipe: Option<&mut Pipeline<ChunkJob, Result<Encoded, MidiError>>>| {
            let mut w = Writer {
                w: out,
                wide,
                control,
                enc: ChunkEnc::new(explicit, 64 * 1024),
                chunk_start: 0,
                chunk_len: 0,
                digest: Digest::new(),
                tracks: 0,
                since_check: 0,
                missing_eot: Vec::new(),
                warnings: Vec::new(),
                write_error: None,
                cancelled: false,
                deferred_error: None,
                pipe,
            };
            let mut read = {
                let mut bs = ByteSource::new(&mut *hashing, file_len);
                read_smf(&mut bs, &mut w)
            };
            // The chunks still with the workers, in order: an earlier chunk's
            // failure comes before a later one's.
            let drained = w.drain();
            if read.is_ok() {
                read = drained;
            }
            if let Some(e) = w.deferred_error.take() {
                read = Err(e);
            }
            WriterOutcome {
                read,
                write_error: w.write_error.take(),
                cancelled: w.cancelled,
                tracks: w.tracks,
                events: w.enc.events,
                notes: w.enc.notes,
                saved: w.enc.saved,
                expected: w.digest.finish(w.tracks),
                normalised: normalised(&w.warnings, &w.missing_eot),
            }
        };
        let threads = threads.unwrap_or_else(|| workers::threads_for(file_len));
        if threads == 0 {
            body(None)
        } else {
            workers::run(threads, window, encode, |pipe| body(Some(pipe)))
        }
    };
    if let Some(e) = written.write_error {
        return Err(ExportError::io(ExportStage::Write, e));
    }
    if written.cancelled {
        return Err(ExportError::Cancelled);
    }
    if let Err(e) = written.read {
        return Err(ExportError::Blocked(format!(
            "The original MIDI file could not be read again ({}).",
            e.user_message().trim_end_matches('.')
        )));
    }
    let changed = !matches!(hashing.finish(), Ok((n, h)) if n == src.size && h == src.hash);
    if changed {
        return Err(ExportError::Blocked(SourceStatus::SourceChanged.sentence()));
    }
    let (tracks, events, notes, saved) =
        (written.tracks, written.events, written.notes, written.saved);
    let expected = written.expected;
    let notes_list = written.normalised;
    // The declared track count: what was written.
    let count = u16::try_from(tracks).map_err(|_| ExportError::TooManyTracks(tracks))?;
    out.seek(SeekFrom::Start(10))
        .and_then(|_| out.write_all(&count.to_be_bytes()))
        .map_err(|e| ExportError::io(ExportStage::Write, e))?;
    let file = out
        .into_inner()
        .map_err(|e| ExportError::io(ExportStage::Flush, e.into_error()))?;
    file.sync_all()
        .map_err(|e| ExportError::io(ExportStage::Flush, e))?;
    drop(file);
    let write_duration = started.elapsed();

    let v_start = Instant::now();
    let f = File::open(&temp).map_err(|e| ExportError::io(ExportStage::Validate, e))?;
    let back_len = f.metadata().ok().map(|m| m.len());
    let mut back_hash = HashingReader::new(BufReader::with_capacity(1 << 20, f));
    let digest = |job: ChunkJob| -> Result<u64, MidiError> {
        let mut h = ChunkHasher {
            hash: WordHash::new(),
            wide,
        };
        let eot = decode_track_bytes(&job.bytes, job.data_start, back_len, job.index, &mut h)?;
        Ok(chunk_digest(h.hash, eot.unwrap_or(u64::MAX)))
    };
    let (back_tracks, back_digest) = {
        let back_hash = &mut back_hash;
        let mut body = move |pipe: Option<&mut Pipeline<ChunkJob, Result<u64, MidiError>>>| {
            let mut back = Reread {
                digest: Digest::new(),
                chunk: WordHash::new(),
                wide,
                tracks: 0,
                deferred_error: None,
                pipe,
            };
            let mut r = {
                let mut bs = ByteSource::new(&mut *back_hash, back_len);
                read_smf(&mut bs, &mut back)
            };
            let drained = back.drain();
            if r.is_ok() {
                r = drained;
            }
            if let Some(e) = back.deferred_error.take() {
                r = Err(e);
            }
            r.map(|()| (back.tracks, back.digest.finish(back.tracks)))
        };
        let threads = threads.unwrap_or_else(|| workers::threads_for(back_len));
        let r = if threads == 0 {
            body(None)
        } else {
            workers::run(threads, window, digest, |pipe| body(Some(pipe)))
        };
        r.map_err(|e| ExportError::Validation {
            detail: e.user_message().trim_end_matches('.').to_owned(),
        })?
    };
    let (bytes, out_hash) = back_hash
        .finish()
        .map_err(|e| ExportError::io(ExportStage::Validate, e))?;
    if back_tracks != tracks || back_digest != expected {
        return Err(ExportError::Validation {
            detail: "the written file does not hold the original events in their order".into(),
        });
    }
    let validate_duration = v_start.elapsed();
    fs::rename(&temp, dest).map_err(|e| ExportError::io(ExportStage::Replace, e))?;
    guard.path = None;
    Ok(StrictReport {
        path: dest.to_path_buf(),
        bytes,
        tracks,
        events,
        notes,
        status_bytes_saved: saved,
        hash: out_hash,
        identical_to_source: bytes == src.size && out_hash == src.hash,
        normalised: notes_list,
        write_duration,
        validate_duration,
    })
}

/// B1-0162: strict export with workers writes the same file and report as
/// with one thread, for well-formed and quirky sources and 1,000 mutated ones.
#[cfg(test)]
mod worker_tests {
    use super::*;
    use crate::midi::test_smf::{mutations, rich};
    use crate::midi::{ImportControl, import_file};

    type Outcome = Result<(Vec<u8>, usize, u64, u64, u64, u64, bool, Vec<String>), ExportError>;

    fn outcome(
        doc: &PlumDocument,
        dir: &Path,
        status: StatusBytes,
        threads: usize,
        window: u64,
    ) -> Outcome {
        let dest = dir.join(format!("out-{threads}-{window}.mid"));
        let r = export_strict_threads(
            doc,
            &dest,
            status,
            ExportControl::default(),
            Some(threads),
            window,
        )?;
        let bytes = fs::read(&dest).expect("written");
        let _ = fs::remove_file(&dest);
        Ok((
            bytes,
            r.tracks,
            r.events,
            r.notes,
            r.status_bytes_saved,
            r.hash,
            r.identical_to_source,
            r.normalised,
        ))
    }

    #[test]
    fn workers_write_the_same_strict_file_and_report() {
        let dir =
            std::env::temp_dir().join(format!("plumspirit-strict-workers-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.mid");
        let mut files = vec![rich()];
        files.extend(mutations(&rich(), 1000));
        let mut exported = 0;
        for f in files {
            fs::write(&src, &f).unwrap();
            let Ok((doc, _)) = import_file(&src, ImportControl::default()) else {
                continue;
            };
            for status in [StatusBytes::Running, StatusBytes::Explicit] {
                let one = outcome(&doc, &dir, status, 0, 0);
                exported += usize::from(one.is_ok());
                for (threads, window) in [(1, WINDOW_ALL), (2, 1), (3, 40)] {
                    assert_eq!(
                        outcome(&doc, &dir, status, threads, window),
                        one,
                        "{threads} worker(s), window {window}"
                    );
                }
            }
        }
        let _ = fs::remove_dir_all(&dir);
        assert!(
            exported > 100,
            "the cases exercise strict exports ({exported})"
        );
    }

    const WINDOW_ALL: u64 = workers::WINDOW;
}
