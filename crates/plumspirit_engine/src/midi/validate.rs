//! Export validation: re-reads a written file and proves it encodes the document.
//!
//! Two levels are checked while streaming the file once, with memory proportional
//! to the maximum polyphony (never a second copy of the document):
//!
//! * **Event level (authoritative).** Every decoded event must equal the event the
//!   exporter intended to write (same tick, same bytes), the header must match, and
//!   every track must end with End of Track at the expected tick. Any difference
//!   fails the export. v1.0-beta1 (B1-0162): after an export the writer's
//!   per-chunk digest of the events it wrote (`WriteStats::chunk_digests`) is
//!   compared with the digest of the decoded tokens ([`validate_written_file`],
//!   chunks on worker threads for large files) instead of generating the
//!   expected events a second time; a chunk that differs is re-checked event
//!   by event ([`validate_exported_file`]) for the message.
//! * **Note level (informational).** The decoded events are re-paired into notes
//!   with the importer's FIFO rule, and the multiset of resulting notes is compared
//!   with the document's notes using an order-independent 128-bit hash. Standard
//!   MIDI Files cannot express every overlap of same-pitch, same-channel notes:
//!   with the export order of [`super::overlap`], exactly the tracks with a note
//!   nested inside a longer one of the same key pair differently when the file
//!   is reopened. That is reported to the user, not treated as corruption.

use super::error::{ImportWarning, MidiError, MidiErrorKind};
use super::export::{
    EOT_DIGEST_MARK, ExportError, ExportStage, OutEvent, PlannedStream, planned_eot,
};
use super::plan::{ExportPlan, OutputTrack};
use super::reader::{ByteSource, SmfHandler, Token, decode_track_bytes, read_smf};
use super::workers::{self, Pipeline};
use crate::codec::WordHash;
use crate::model::document::PlumDocument;
use crate::model::events::{RawKind, channel_data_len, system_data_len};
use crate::model::note::{NoteAttrs, NoteOffKind};
use crate::model::track::Track;
use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub events_checked: u64,
    /// Notes re-paired from the written file.
    pub notes_checked: u64,
    /// Tracks whose notes will pair differently when the file is reopened
    /// (overlapping notes with the same pitch and channel).
    pub tracks_with_different_pairing: Vec<usize>,
}

impl ValidationReport {
    pub fn pairing_differs(&self) -> bool {
        !self.tracks_with_different_pairing.is_empty()
    }
}

pub fn validate_exported_file(
    doc: &PlumDocument,
    plan: &ExportPlan,
    path: &Path,
) -> Result<ValidationReport, ExportError> {
    let file = File::open(path).map_err(|e| ExportError::Io {
        stage: ExportStage::Validate,
        kind: e.kind(),
        message: e.to_string(),
    })?;
    let len = file.metadata().ok().map(|m| m.len());
    validate_reader(doc, plan, file, len)
}

pub fn validate_exported_bytes(
    doc: &PlumDocument,
    plan: &ExportPlan,
    bytes: &[u8],
) -> Result<ValidationReport, ExportError> {
    validate_reader(doc, plan, bytes, Some(bytes.len() as u64))
}

/// v1.0-beta1 (B1-0162): the validation [`super::export_to_path`] runs. The
/// file is re-read once and every chunk's decoded tokens and End of Track
/// must give the digest the writer recorded for the events it wrote
/// (`WriteStats::chunk_digests`) — the event-level check without generating
/// the expected events a second time — plus the same header, extra-chunk,
/// import-condition and note-level checks as [`validate_exported_file`].
/// Anything that does not match is re-checked with [`validate_exported_file`],
/// which names the first differing event; the report is the same either way.
pub fn validate_written_file(
    doc: &PlumDocument,
    plan: &ExportPlan,
    path: &Path,
    digests: &[u64],
) -> Result<ValidationReport, ExportError> {
    let fast = File::open(path).ok().and_then(|file| {
        let len = file.metadata().ok().map(|m| m.len());
        check_digests(doc, plan, file, len, digests)
    });
    match fast {
        Some(report) => Ok(report),
        None => {
            validate_exported_file(doc, plan, path)?;
            Err(ExportError::Validation {
                detail: "the written file does not decode to the events that were written".into(),
            })
        }
    }
}

/// [`validate_written_file`] over bytes in memory (tests).
pub fn validate_written_bytes(
    doc: &PlumDocument,
    plan: &ExportPlan,
    bytes: &[u8],
    digests: &[u64],
) -> Result<ValidationReport, ExportError> {
    match check_digests(doc, plan, bytes, Some(bytes.len() as u64), digests) {
        Some(report) => Ok(report),
        None => {
            validate_exported_bytes(doc, plan, bytes)?;
            Err(ExportError::Validation {
                detail: "the written file does not decode to the events that were written".into(),
            })
        }
    }
}

/// The digest pass of [`validate_written_file`]: `None` as soon as anything
/// differs (the caller then runs the full validation for its message).
/// Chunks are checked on worker threads when the file is large
/// ([`super::workers`]); the report is the same.
fn check_digests<R: std::io::Read>(
    doc: &PlumDocument,
    plan: &ExportPlan,
    reader: R,
    len: Option<u64>,
    digests: &[u64],
) -> Option<ValidationReport> {
    check_digests_threads(
        doc,
        plan,
        reader,
        len,
        digests,
        workers::threads_for(len),
        workers::window_for(workers::threads_for(len)),
    )
}

pub(crate) fn check_digests_threads<R: std::io::Read>(
    doc: &PlumDocument,
    plan: &ExportPlan,
    reader: R,
    len: Option<u64>,
    digests: &[u64],
    threads: usize,
    window: u64,
) -> Option<ValidationReport> {
    let layout = plan.layout(doc);
    if digests.len() != layout.len() {
        return None;
    }
    let mut primary = vec![layout.len(); doc.track_count() + 1];
    for (oi, o) in layout.iter().enumerate().rev() {
        if o.layer == 0 {
            primary[o.track] = oi;
        }
    }
    let wide = plan.wide.policy.writes_wide();
    let layout_ref: &[OutputTrack] = &layout;
    let check = |job: ChunkJob| -> Option<ChunkCheck> {
        let mut t = TrackDigest {
            hash: WordHash::new(),
            pairing: Pairing::new(wide),
            events: 0,
            failed: false,
            wide,
        };
        let eot = decode_track_bytes(&job.bytes, job.data_start, len, job.index, &mut t).ok()?;
        drop(job.bytes);
        t.finish(job.index, eot, doc, plan, layout_ref, digests)
    };
    let mut src = ByteSource::new(reader, len);
    let mut body = move |pipe: Option<&mut Pipeline<ChunkJob, Option<ChunkCheck>>>| {
        let mut h = DigestHandler {
            doc,
            plan,
            layout: layout_ref,
            primary: &primary,
            digests,
            report: ValidationReport::default(),
            track: TrackDigest {
                hash: WordHash::new(),
                pairing: Pairing::new(wide),
                events: 0,
                failed: false,
                wide,
            },
            failed: false,
            tracks_read: 0,
            extra_seen: 0,
            pipe,
        };
        let res = read_smf(&mut src, &mut h);
        let drained = h.drain();
        let complete = res.is_ok()
            && drained
            && !h.failed
            && h.tracks_read == layout_ref.len()
            && h.extra_seen == doc.extra_chunks().len();
        complete.then_some(h.report)
    };
    if threads == 0 {
        body(None)
    } else {
        workers::run(threads, window, check, |pipe| body(Some(pipe)))
    }
}

/// A chunk handed to a worker for its digest check.
struct ChunkJob {
    index: usize,
    data_start: u64,
    bytes: Vec<u8>,
}

/// What a chunk's digest check found (only when it matched).
struct ChunkCheck {
    events: u64,
    report: ValidationReport,
}

/// The digest, pairing and token count of one chunk being decoded.
struct TrackDigest {
    hash: WordHash,
    pairing: Pairing,
    events: u64,
    failed: bool,
    wide: bool,
}

impl TrackDigest {
    /// Closes chunk `index`: End of Track, digest and the note-level check.
    fn finish(
        &mut self,
        index: usize,
        eot: Option<u64>,
        doc: &PlumDocument,
        plan: &ExportPlan,
        layout: &[OutputTrack],
        digests: &[u64],
    ) -> Option<ChunkCheck> {
        let eot = eot?;
        if self.failed {
            return None;
        }
        self.hash.pair(eot, EOT_DIGEST_MARK);
        if self.hash.finish() != *digests.get(index)? {
            return None;
        }
        let out = layout.get(index)?;
        let track = doc.track(out.track)?;
        let mut report = ValidationReport::default();
        note_level(track, out, plan, self.pairing.digest, &mut report);
        Some(ChunkCheck {
            events: self.events,
            report,
        })
    }
}

impl SmfHandler for TrackDigest {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        super::compare::hash_token(&mut self.hash, &token);
        self.events += 1;
        if let Token::Channel { tick, status, data } = token {
            self.pairing.channel(tick, status, data);
        }
        Ok(())
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn extended_keys(&self) -> bool {
        self.wide
    }
    fn warning(&mut self, warning: ImportWarning) {
        // As in the full validation: only these conditions are expected.
        if !matches!(
            warning,
            ImportWarning::UnknownChunk { .. } | ImportWarning::SystemMessages { .. }
        ) {
            self.failed = true;
        }
    }
}

struct DigestHandler<'d, 'p> {
    doc: &'d PlumDocument,
    plan: &'d ExportPlan,
    layout: &'d [OutputTrack],
    primary: &'d [usize],
    digests: &'d [u64],
    report: ValidationReport,
    /// The chunk being decoded in place.
    track: TrackDigest,
    failed: bool,
    tracks_read: usize,
    extra_seen: usize,
    pipe: Option<&'p mut Pipeline<ChunkJob, Option<ChunkCheck>>>,
}

impl DigestHandler<'_, '_> {
    fn fail(&mut self) -> MidiError {
        self.failed = true;
        MidiError::new(MidiErrorKind::Cancelled)
    }

    /// Adds a matching chunk's counts (in chunk order).
    fn merge(&mut self, c: ChunkCheck) {
        self.report.events_checked += c.events;
        self.report.notes_checked += c.report.notes_checked;
        for t in c.report.tracks_with_different_pairing {
            if !self.report.tracks_with_different_pairing.contains(&t) {
                self.report.tracks_with_different_pairing.push(t);
            }
        }
    }

    /// Takes back the oldest chunk a worker checked; false if it differed.
    fn take_oldest(&mut self) -> bool {
        let Some(pipe) = self.pipe.as_mut() else {
            return true;
        };
        let Some(i) = pipe.oldest() else {
            return true;
        };
        match pipe.take(i, None).flatten() {
            Some(c) => {
                self.merge(c);
                true
            }
            None => {
                self.failed = true;
                false
            }
        }
    }

    /// Takes back every chunk still with the workers; false if one differed.
    fn drain(&mut self) -> bool {
        while self.pipe.as_ref().is_some_and(|p| p.oldest().is_some()) {
            if !self.take_oldest() {
                return false;
            }
        }
        true
    }
}

impl SmfHandler for DigestHandler<'_, '_> {
    fn header(&mut self, format: u16, declared: u16, division: u16) -> Result<(), MidiError> {
        if format != self.plan.format(self.doc).word()
            || usize::from(declared) != self.layout.len()
            || division != self.plan.division_word
        {
            return Err(self.fail());
        }
        Ok(())
    }

    fn begin_track(&mut self, index: usize) -> Result<(), MidiError> {
        // Counts merge in chunk order: the workers' chunks come first.
        if !self.drain() || index >= self.layout.len() || index != self.tracks_read {
            return Err(self.fail());
        }
        self.track.hash = WordHash::new();
        self.track.pairing.reset();
        self.track.events = 0;
        self.track.failed = false;
        Ok(())
    }

    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        self.track.token(token)
    }

    fn end_track(&mut self, index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        match self
            .track
            .finish(index, eot, self.doc, self.plan, self.layout, self.digests)
        {
            Some(c) => self.merge(c),
            None => return Err(self.fail()),
        }
        self.tracks_read += 1;
        Ok(())
    }

    fn extra_chunk(
        &mut self,
        chunk_type: [u8; 4],
        data: Vec<u8>,
        after: usize,
    ) -> Result<(), MidiError> {
        let doc = self.doc;
        let expected = doc.extra_chunks().get(self.extra_seen);
        let primary = self.primary;
        let ok = expected.is_some_and(|c| {
            c.chunk_type == chunk_type
                && c.data == data
                && primary[c.after_track_chunks.min(doc.track_count())] == after
        });
        if !ok {
            return Err(self.fail());
        }
        self.extra_seen += 1;
        Ok(())
    }

    fn extended_keys(&self) -> bool {
        self.track.wide
    }

    fn warning(&mut self, warning: ImportWarning) {
        self.track.warning(warning);
        if self.track.failed {
            self.failed = true;
        }
    }

    fn defer_track(&mut self, index: usize, len: u64) -> bool {
        self.pipe.as_ref().is_some_and(|p| len <= p.window())
            && index == self.tracks_read
            && index < self.layout.len()
    }

    fn track_bytes(
        &mut self,
        index: usize,
        data_start: u64,
        bytes: Vec<u8>,
    ) -> Result<(), MidiError> {
        let len = bytes.len() as u64;
        while self.pipe.as_ref().is_some_and(|p| !p.has_room(len)) {
            if !self.take_oldest() {
                return Err(self.fail());
            }
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
        self.tracks_read += 1;
        Ok(())
    }
}

fn validate_reader<R: std::io::Read>(
    doc: &PlumDocument,
    plan: &ExportPlan,
    reader: R,
    len: Option<u64>,
) -> Result<ValidationReport, ExportError> {
    let mut src = ByteSource::new(reader, len);
    let layout = plan.layout(doc);
    // Output position of each track's own chunk (and of the end).
    let mut primary = vec![layout.len(); doc.track_count() + 1];
    for (oi, o) in layout.iter().enumerate().rev() {
        if o.layer == 0 {
            primary[o.track] = oi;
        }
    }
    let mut h = ValidateHandler {
        doc,
        plan,
        layout,
        primary,
        report: ValidationReport::default(),
        stream: None,
        pairing: Pairing::new(plan.wide.policy.writes_wide()),
        mismatch: None,
        tracks_read: 0,
        extra_seen: 0,
    };
    let res = read_smf(&mut src, &mut h);
    if let Some(m) = h.mismatch.take() {
        return Err(ExportError::Validation { detail: m });
    }
    res.map_err(|e| ExportError::Validation {
        detail: format!("the file could not be re-read: {}", e.user_message()),
    })?;
    if h.tracks_read != h.layout.len() {
        return Err(ExportError::Validation {
            detail: format!(
                "expected {} tracks, found {}",
                h.layout.len(),
                h.tracks_read
            ),
        });
    }
    if h.extra_seen != doc.extra_chunks().len() {
        return Err(ExportError::Validation {
            detail: "preserved extra chunks are missing".into(),
        });
    }
    Ok(h.report)
}

/// Order-independent multiset digest of notes: two independent 64-bit sums of
/// mixed note values plus the count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NoteDigest {
    count: u64,
    a: u64,
    b: u64,
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl NoteDigest {
    fn add(&mut self, start: u64, dur: u64, attrs: u32) {
        let h1 = splitmix(start ^ splitmix(dur ^ splitmix(u64::from(attrs))));
        let h2 = splitmix(h1 ^ 0xA5A5_5A5A_1234_5678 ^ start.rotate_left(17) ^ dur.rotate_left(41));
        self.count += 1;
        self.a = self.a.wrapping_add(h1);
        self.b = self.b.wrapping_add(h2);
    }
}

/// Streaming FIFO note pairing, mirroring the importer's rule.
struct Pairing {
    active: Vec<VecDeque<(u64, NoteAttrs)>>,
    /// 7, or 8 for the experimental 256-key file (keys 0–255).
    key_shift: u32,
    digest: NoteDigest,
}

impl Pairing {
    fn new(wide: bool) -> Self {
        let key_shift = if wide { 8 } else { 7 };
        Pairing {
            active: (0..16usize << key_shift).map(|_| VecDeque::new()).collect(),
            key_shift,
            digest: NoteDigest::default(),
        }
    }

    fn reset(&mut self) {
        for q in &mut self.active {
            q.clear();
        }
        self.digest = NoteDigest::default();
    }

    fn channel(&mut self, tick: u64, status: u8, data: [u8; 2]) {
        let kind = status & 0xF0;
        let ch = status & 0x0F;
        let key = (usize::from(ch) << self.key_shift) | usize::from(data[0]);
        if kind == 0x90 && data[1] > 0 {
            if let Ok(a) = NoteAttrs::new_wide(data[0], data[1], ch, 0, NoteOffKind::NoteOff) {
                self.active[key].push_back((tick, a));
            }
        } else if (kind == 0x80 || kind == 0x90)
            && let Some((start, a)) = self.active[key].pop_front()
        {
            let (off_vel, off_kind) = if kind == 0x80 {
                (data[1], NoteOffKind::NoteOff)
            } else {
                (0, NoteOffKind::NoteOnZero)
            };
            if let Ok(full) =
                NoteAttrs::new_wide(a.pitch(), a.velocity(), a.channel(), off_vel, off_kind)
            {
                self.digest.add(start, tick - start, full.raw());
            }
        }
    }
}

/// The note-level check of one written chunk: the notes re-paired from the
/// file (`got`) against the document's notes as planned for the chunk.
fn note_level(
    track: &Track,
    out: &OutputTrack,
    plan: &ExportPlan,
    got: NoteDigest,
    report: &mut ValidationReport,
) {
    let mut expected = NoteDigest::default();
    for n in super::export::output_notes(track, out, plan) {
        let s = plan.map_tick(n.start_tick);
        let e = plan.map_tick(n.end_tick());
        expected.add(s, e - s, n.attrs.raw());
    }
    // v1.0-Launch L-S51: the first track's own chunk also holds the silent
    // loudness reference (a zero-length velocity-127 note).
    if out.track == 0
        && out.layer == 0
        && let Some(r) = plan.loudness_reference
        && let Ok(a) = crate::model::note::NoteAttrs::new(
            r.key,
            127,
            r.channel,
            0,
            crate::model::note::NoteOffKind::NoteOff,
        )
    {
        expected.add(plan.map_tick(r.tick), 0, a.raw());
    }
    report.notes_checked += got.count;
    if got != expected && !report.tracks_with_different_pairing.contains(&out.track) {
        report.tracks_with_different_pairing.push(out.track);
    }
}

struct ValidateHandler<'d> {
    doc: &'d PlumDocument,
    plan: &'d ExportPlan,
    layout: std::borrow::Cow<'d, [OutputTrack]>,
    primary: Vec<usize>,
    report: ValidationReport,
    stream: Option<PlannedStream<'d>>,
    pairing: Pairing,
    mismatch: Option<String>,
    tracks_read: usize,
    extra_seen: usize,
}

impl ValidateHandler<'_> {
    /// Records the first mismatch and aborts parsing.
    fn fail(&mut self, msg: String) -> MidiError {
        if self.mismatch.is_none() {
            self.mismatch = Some(msg);
        }
        MidiError::new(MidiErrorKind::Cancelled)
    }
}

impl<'d> SmfHandler for ValidateHandler<'d> {
    fn header(&mut self, format: u16, declared: u16, division: u16) -> Result<(), MidiError> {
        let doc = self.doc;
        let want = self.plan.format(doc).word();
        if format != want {
            return Err(self.fail(format!("format {format} written, {want} expected")));
        }
        if usize::from(declared) != self.layout.len() {
            return Err(self.fail(format!(
                "header declares {declared} tracks, {} expected",
                self.layout.len()
            )));
        }
        if division != self.plan.division_word {
            return Err(self.fail(format!(
                "timing division {division} written, {} expected",
                self.plan.division_word
            )));
        }
        Ok(())
    }

    fn begin_track(&mut self, index: usize) -> Result<(), MidiError> {
        let doc = self.doc;
        let plan = self.plan;
        let Some(out) = self.layout.get(index).cloned() else {
            return Err(self.fail(format!("unexpected extra track {}", index + 1)));
        };
        let Some(track) = doc.track(out.track) else {
            return Err(self.fail(format!("unexpected extra track {}", index + 1)));
        };
        // The stream borrows the chunk description from the plan; a plan without
        // a layout writes each track as one plain chunk.
        let stream = match &self.layout {
            std::borrow::Cow::Borrowed(v) => {
                let v: &'d [OutputTrack] = v;
                PlannedStream::for_output(track, &v[index], plan)
            }
            std::borrow::Cow::Owned(_) => PlannedStream::new(track, out.track, plan),
        };
        self.stream = Some(stream);
        self.pairing.reset();
        Ok(())
    }

    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        let doc = self.doc;
        let index = self.tracks_read;
        let track = &doc.tracks()[self.layout[index].track];
        let expected = self.stream.as_mut().and_then(Iterator::next);
        let ok = match &expected {
            Some((tick, ev)) => *tick == token.tick() && events_equal(ev, track, &token),
            None => false,
        };
        if !ok {
            return Err(self.fail(match expected {
                Some((tick, _)) => format!(
                    "track {}: event at tick {} differs from the expected event at tick {}",
                    index + 1,
                    token.tick(),
                    tick
                ),
                None => format!(
                    "track {}: unexpected extra event at tick {}",
                    index + 1,
                    token.tick()
                ),
            }));
        }
        self.report.events_checked += 1;
        if let Token::Channel { tick, status, data } = token {
            self.pairing.channel(tick, status, data);
        }
        Ok(())
    }

    fn end_track(&mut self, index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        let doc = self.doc;
        let out = self.layout[index].clone();
        let track = &doc.tracks()[out.track];
        let mut expected_eot = planned_eot(track, out.track, self.plan);
        if let Some(mut s) = self.stream.take() {
            if let Some((tick, _)) = s.next() {
                return Err(self.fail(format!(
                    "track {}: event at tick {tick} is missing",
                    index + 1
                )));
            }
            expected_eot = s.end_of_track();
        }
        if eot != Some(expected_eot) {
            return Err(self.fail(format!(
                "track {}: End of Track at {:?}, expected {expected_eot}",
                index + 1,
                eot
            )));
        }
        note_level(
            track,
            &out,
            self.plan,
            self.pairing.digest,
            &mut self.report,
        );
        self.tracks_read += 1;
        Ok(())
    }

    fn extra_chunk(
        &mut self,
        chunk_type: [u8; 4],
        data: Vec<u8>,
        after: usize,
    ) -> Result<(), MidiError> {
        let doc = self.doc;
        let expected = doc.extra_chunks().get(self.extra_seen);
        let primary = &self.primary;
        let ok = expected.is_some_and(|c| {
            c.chunk_type == chunk_type
                && c.data == data
                && primary[c.after_track_chunks.min(doc.track_count())] == after
        });
        if !ok {
            return Err(self.fail("an extra chunk differs".into()));
        }
        self.extra_seen += 1;
        Ok(())
    }

    /// The experimental 256-key file is read back the way it was written
    /// (v0.9 §062): unmasked key bytes after an explicit Note On / Off status.
    fn extended_keys(&self) -> bool {
        self.plan.wide.policy.writes_wide()
    }

    fn warning(&mut self, warning: ImportWarning) {
        // A file PlumSpirit wrote must not trigger import warnings, except the
        // informational ones about content the document legitimately contains.
        match warning {
            ImportWarning::UnknownChunk { .. } | ImportWarning::SystemMessages { .. } => {}
            other => {
                let _ = self.fail(format!("unexpected condition: {}", other.user_message()));
            }
        }
    }
}

fn events_equal(expected: &OutEvent<'_>, track: &Track, token: &Token<'_>) -> bool {
    match (expected.channel_status(), token) {
        (
            Some(status),
            Token::Channel {
                status: s, data, ..
            },
        ) => {
            if status != *s {
                return false;
            }
            let e = expected.channel_data();
            let n = channel_data_len(status);
            e[..n] == data[..n]
        }
        (Some(_), _) => false,
        (None, _) => {
            if let OutEvent::Tempo(us) = expected {
                let b = us.to_be_bytes();
                return matches!(token, Token::Meta { meta_type: 0x51, payload, .. } if *payload == [b[1], b[2], b[3]]);
            }
            if let OutEvent::Meta { meta_type, payload } = expected {
                return matches!(token, Token::Meta { meta_type: t, payload: p, .. } if t == meta_type && p == payload);
            }
            if let OutEvent::SysEx { payload } = expected {
                return matches!(token, Token::SysEx { payload: p, .. } if p == payload);
            }
            let OutEvent::Raw(kind) = expected else {
                return false;
            };
            match (kind, token) {
                (
                    RawKind::Meta { meta_type, payload },
                    Token::Meta {
                        meta_type: t,
                        payload: p,
                        ..
                    },
                ) => meta_type == t && track.events.payload(*payload) == *p,
                (RawKind::SysEx { payload }, Token::SysEx { payload: p, .. }) => {
                    track.events.payload(*payload) == *p
                }
                (RawKind::SysExEscape { payload }, Token::SysExEscape { payload: p, .. }) => {
                    track.events.payload(*payload) == *p
                }
                (
                    RawKind::System { status, data },
                    Token::System {
                        status: s, data: d, ..
                    },
                ) => {
                    let n = system_data_len(*status).unwrap_or(0);
                    status == s && data[..n] == d[..n]
                }
                _ => false,
            }
        }
    }
}
