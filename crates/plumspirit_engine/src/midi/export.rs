//! [`PlumDocument`] → Standard MIDI File.
//!
//! # Event order (deterministic)
//!
//! For each track, events are produced in tick order. At a single tick `T`:
//!
//! 1. Note terminations of notes that started before `T` and end at `T`, ordered by
//!    the note's start (then stored order). Ending first means a note that ends at
//!    `T` never cuts off a same-pitch note that starts at `T`.
//! 2. Note starts at `T` in stored order, except that notes of the same channel
//!    and key that start together are written shortest first
//!    ([`NoteWriteOrder`](super::overlap::NoteWriteOrder)). A zero-duration note
//!    writes its Note On immediately followed by its termination.
//! 3. Preserved events are interleaved using their `anchor` (the number of note
//!    events that preceded them at this tick in the source file); an anchor larger
//!    than the number of note events now at `T` places the event after all of them.
//! 4. The first track also carries the Conductor's events (tempo, time signatures,
//!    markers), merged with its own events as the [`ExportPlan`] places them.
//!
//! Because the importer pairs notes first-in-first-out and this order ends older
//! notes first, an unchanged document re-imports to exactly the same notes —
//! unless a note starts after another of the same channel and key and ends
//! before it (nested), which no Standard MIDI file can express for such a
//! reader. The policy, what other readers (Kiva) get, and the disclosure:
//! [`super::overlap`], `docs/EXPORT_CERTIFICATION.md`.
//!
//! # Memory
//!
//! Export streams: note starts come from the (already sorted) note store and pending
//! note ends live in a min-heap whose size is the track's maximum polyphony, not its
//! note count. Track chunk lengths are patched by seeking back, so no track is
//! buffered in memory.
//!
//! # Limits
//!
//! A delta time (the gap between consecutive events in a track) cannot exceed
//! `0x0FFF_FFFF` ticks in a Standard MIDI File. Longer gaps (very long notes or
//! silences) are bridged with PlumSpirit *gap filler* events — Sequencer-Specific
//! meta events that other programs ignore and PlumSpirit drops on import — so any
//! tick position can be exported exactly. A track chunk cannot exceed 4 GiB and a
//! file cannot hold more than 65,535 tracks; export reports these as errors (and the
//! plan refuses spans that would need an absurd number of fillers) instead of
//! writing an invalid file.
//!
//! # Running status
//!
//! By default a channel message omits its status byte when it repeats the
//! previous one (MIDI running status, the compact form some tools call
//! "compressed"); the status is written again after every meta, SysEx or
//! system event (SMF 1.0) and for every note above key 127 in the
//! experimental 256-key file. With [`ExportOptions::explicit_status`]
//! ("Maximum Compatibility") every channel message carries its status byte.
//! Both are valid Standard MIDI and hold the same events
//! (`tests/v09_export_certification.rs`).
//!
//! [`ExportOptions::explicit_status`]: super::plan::ExportOptions::explicit_status

use super::error::group_digits;
use super::overlap::{NoteWriteOrder, SameKeyOverlaps};
use super::plan::{ExportEvents, ExportPlan, OutputTrack, PlanEvent, PlanKind, plan_export};
use super::seqspec;
use super::validate::{ValidationReport, validate_written_file};
use super::vlq::write_vlq;
use crate::codec::WordHash;
use crate::model::document::{PlumDocument, SmfFormat};
use crate::model::events::{RawEvent, RawKind, channel_data_len, system_data_len};
use crate::model::note::{NoteAttrs, NoteOffKind};
use crate::model::note_store::NoteView;
use crate::model::timing::MAX_DELTA_TICKS;
use crate::model::track::Track;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A single event produced for export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutEvent<'a> {
    NoteOn {
        channel: u8,
        pitch: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        pitch: u8,
        velocity: u8,
        kind: NoteOffKind,
    },
    Raw(&'a RawKind),
    /// A Conductor tempo event (`FF 51 03 tt tt tt`).
    Tempo(u32),
    /// A meta event whose payload is not in the track's arena (Conductor
    /// signatures and markers, gap fillers).
    Meta {
        meta_type: u8,
        payload: &'a [u8],
    },
    /// A SysEx event (`F0 len payload`) that is not in the track's arena
    /// (Conductor Master Volume).
    SysEx {
        payload: &'a [u8],
    },
}

impl OutEvent<'_> {
    /// Channel status byte for channel messages, `None` otherwise.
    pub fn channel_status(&self) -> Option<u8> {
        match *self {
            OutEvent::NoteOn { channel, .. } => Some(0x90 | channel),
            OutEvent::NoteOff { channel, kind, .. } => Some(match kind {
                NoteOffKind::NoteOff => 0x80 | channel,
                NoteOffKind::NoteOnZero => 0x90 | channel,
            }),
            OutEvent::Raw(RawKind::Channel { status, .. }) => Some(*status),
            OutEvent::Raw(_)
            | OutEvent::Tempo(_)
            | OutEvent::Meta { .. }
            | OutEvent::SysEx { .. } => None,
        }
    }

    /// Data bytes of channel messages.
    pub fn channel_data(&self) -> [u8; 2] {
        match *self {
            OutEvent::NoteOn {
                pitch, velocity, ..
            } => [pitch, velocity],
            OutEvent::NoteOff {
                pitch,
                velocity,
                kind,
                ..
            } => match kind {
                NoteOffKind::NoteOff => [pitch, velocity],
                NoteOffKind::NoteOnZero => [pitch, 0],
            },
            OutEvent::Raw(RawKind::Channel { data, .. }) => *data,
            OutEvent::Raw(_)
            | OutEvent::Tempo(_)
            | OutEvent::Meta { .. }
            | OutEvent::SysEx { .. } => [0, 0],
        }
    }
}

/// Encodes a tempo meta event (`FF 51 03` + 24-bit big-endian value).
/// Marks the End of Track in a chunk digest ([`hash_out_event`]).
pub(crate) const EOT_DIGEST_MARK: u64 = 0xE0F_2F00;

/// v1.0-beta1 (B1-0162): mixes `ev` at `tick` into a chunk digest the way a
/// reader decodes it back (`compare::hash_token`'s encoding of the token the
/// written bytes give: unused data bytes zero, tempo as `FF 51`). The writer
/// keeps one digest per chunk; validation re-reads the file and must get the
/// same digest from the decoded tokens ([`super::validate::validate_written_file`]).
pub(crate) fn hash_out_event(h: &mut WordHash, tick: u64, ev: &OutEvent<'_>, track: &Track) {
    use super::compare::hash_token;
    use super::reader::Token;
    if let Some(status) = ev.channel_status() {
        let mut data = ev.channel_data();
        if channel_data_len(status) < 2 {
            data[1] = 0;
        }
        hash_token(h, &Token::Channel { tick, status, data });
        return;
    }
    match *ev {
        OutEvent::Raw(kind) => match *kind {
            RawKind::Meta { meta_type, payload } => hash_token(
                h,
                &Token::Meta {
                    tick,
                    meta_type,
                    payload: track.events.payload(payload),
                },
            ),
            RawKind::SysEx { payload } => hash_token(
                h,
                &Token::SysEx {
                    tick,
                    payload: track.events.payload(payload),
                },
            ),
            RawKind::SysExEscape { payload } => hash_token(
                h,
                &Token::SysExEscape {
                    tick,
                    payload: track.events.payload(payload),
                },
            ),
            RawKind::System { status, data } => {
                let n = system_data_len(status).unwrap_or(0).min(2);
                let mut d = [0u8; 2];
                d[..n].copy_from_slice(&data[..n]);
                hash_token(
                    h,
                    &Token::System {
                        tick,
                        status,
                        data: d,
                    },
                );
            }
            // Channel messages have a status (handled above).
            RawKind::Channel { .. } => {}
        },
        OutEvent::Tempo(us) => {
            let b = us.to_be_bytes();
            hash_token(
                h,
                &Token::Meta {
                    tick,
                    meta_type: 0x51,
                    payload: &[b[1], b[2], b[3]],
                },
            );
        }
        OutEvent::Meta { meta_type, payload } => hash_token(
            h,
            &Token::Meta {
                tick,
                meta_type,
                payload,
            },
        ),
        OutEvent::SysEx { payload } => hash_token(h, &Token::SysEx { tick, payload }),
        OutEvent::NoteOn { .. } | OutEvent::NoteOff { .. } => {}
    }
}

pub fn encode_tempo(micros: u32, out: &mut Vec<u8>) {
    let b = micros.to_be_bytes();
    out.extend_from_slice(&[0xFF, 0x51, 0x03, b[1], b[2], b[3]]);
}

/// End-of-Track tick of track `index` in document ticks: the explicit end or
/// the (corrected) content end, whichever is later, and for the first track
/// also the last Conductor event.
pub fn planned_eot_unmapped(track: &Track, index: usize, plan: &ExportPlan) -> u64 {
    let mut end = match plan.corrected_content_end(index) {
        Some(c) => track.end_of_track_tick.unwrap_or(0).max(c),
        None => plan.content_end(track, index),
    };
    if index == 0 {
        end = end.max(plan.last_tick());
    }
    end
}

/// End-of-Track tick written for track `index` (in file ticks).
pub fn planned_eot(track: &Track, index: usize, plan: &ExportPlan) -> u64 {
    plan.map_tick(planned_eot_unmapped(track, index, plan))
}

/// The notes of track `index` as they are written: corrected (see
/// `model::correction`) when the plan applies the track's corrections, and
/// (v0.9) with keys above 127 handled by the plan's [`WideKeys`](super::plan::WideKeys) policy.
pub fn export_notes<'a>(
    track: &'a Track,
    index: usize,
    plan: &'a ExportPlan,
) -> Box<dyn Iterator<Item = NoteView> + 'a> {
    keyed(export_notes_as_stored(track, index, plan), plan)
}

/// Applies the plan's [`WideKeys`](super::plan::WideKeys) policy to notes above key 127 (nothing to
/// do — and no extra work — when the export meets none).
fn keyed<'a>(
    notes: Box<dyn Iterator<Item = NoteView> + 'a>,
    plan: &ExportPlan,
) -> Box<dyn Iterator<Item = NoteView> + 'a> {
    if plan.wide.notes == 0 {
        return notes;
    }
    let policy = plan.wide.policy;
    Box::new(notes.filter_map(move |mut n| {
        let p = n.attrs.pitch();
        let to = policy.map(p)?;
        if to != p {
            n.attrs = n.attrs.with_pitch(to).ok()?;
        }
        Some(n)
    }))
}

/// [`export_notes`] before the [`WideKeys`](super::plan::WideKeys) policy: corrected, keys as they
/// would sound (0–255 in a 256-Key Mode project).
pub fn export_notes_as_stored<'a>(
    track: &'a Track,
    index: usize,
    plan: &'a ExportPlan,
) -> Box<dyn Iterator<Item = NoteView> + 'a> {
    use crate::model::correction::{CorrectedNotes, CorrectedRuns};
    use crate::model::placed::PlacedNotes;
    match (plan.correction(index), plan.placed(index)) {
        (Some(c), None) => Box::new(CorrectedNotes::new(track.notes.snapshot(), *c).map(view)),
        (None, None) => Box::new(track.notes.iter_audible()),
        // v0.9 clips: the placements' notes, read in place.
        (None, Some(p)) => Box::new(
            PlacedNotes::new(&p.lanes)
                .filter(|r| !r.attrs.muted())
                .map(view),
        ),
        (Some(c), Some(p)) => Box::new(
            CorrectedRuns::from_runs(
                PlacedNotes::range(&p.lanes, 0, c.from_tick),
                PlacedNotes::range(&p.lanes, c.from_tick, u64::MAX),
                *c,
            )
            .map(view),
        ),
    }
}

/// A note record as an export note (never selected).
#[inline]
fn view(r: crate::model::NoteRecord) -> NoteView {
    NoteView {
        start_tick: r.start_tick,
        duration_ticks: r.duration_ticks,
        attrs: r.attrs,
        sub: r.sub,
        group: r.group,
        selected: false,
    }
}

/// The notes one output chunk carries: [`export_notes`] limited to the chunk's
/// (MIDI channel, subchannel) pairs.
pub fn output_notes<'a>(
    track: &'a Track,
    out: &'a OutputTrack,
    plan: &'a ExportPlan,
) -> Box<dyn Iterator<Item = NoteView> + 'a> {
    if out.slots.is_none() {
        return export_notes(track, out.track, plan);
    }
    if plan.correction(out.track).is_none() && plan.placed(out.track).is_none() {
        // Uncorrected: test the channel/subchannel columns before building
        // each note (one cheap pass per layer).
        return keyed(
            Box::new(
                track
                    .notes
                    .iter_where_audible(move |ch, sub| out.carries(ch, sub)),
            ),
            plan,
        );
    }
    let all = export_notes(track, out.track, plan);
    Box::new(all.filter(move |n| out.carries(n.attrs.channel(), n.sub.0)))
}

/// A track's events as they are written to the file: [`TrackEventStream`] with the
/// [`ExportPlan`] applied (ticks mapped to the export resolution, Conductor events
/// merged into the first track) and gap fillers inserted wherever consecutive
/// events — or the last event and End of Track — are further apart than a delta
/// time can express. Events keep their order; only ticks change.
pub struct PlannedStream<'a> {
    inner: TrackEventStream<'a>,
    plan: &'a ExportPlan,
    /// Tick of the last event yielded (file ticks).
    last: u64,
    eot: u64,
    pending: Option<(u64, OutEvent<'a>)>,
    /// Track Name written first (extra layer chunks).
    prefix: Option<&'a [u8]>,
    /// PlumSpirit structure record written last, just before End of Track.
    suffix: Option<&'a [u8]>,
}

impl<'a> PlannedStream<'a> {
    /// The stream of track `index` written as one chunk with every note.
    pub fn new(track: &'a Track, index: usize, plan: &'a ExportPlan) -> Self {
        let extra: &'a [PlanEvent] = if index == 0 { &plan.events } else { &[] };
        PlannedStream {
            inner: TrackEventStream::with_parts(
                export_notes(track, index, plan),
                plan.events_of(track, index),
                extra,
                &plan.arena,
            ),
            plan,
            last: 0,
            eot: planned_eot(track, index, plan),
            pending: None,
            prefix: None,
            suffix: None,
        }
    }

    /// The stream of one output chunk (see [`OutputTrack`]).
    pub fn for_output(track: &'a Track, out: &'a OutputTrack, plan: &'a ExportPlan) -> Self {
        let index = out.track;
        let own = out.layer == 0;
        let extra: &'a [PlanEvent] = if own && index == 0 { &plan.events } else { &[] };
        let raw = if own {
            plan.events_of(track, index)
        } else {
            ExportEvents::Slice([].iter())
        };
        PlannedStream {
            inner: TrackEventStream::with_parts(
                output_notes(track, out, plan),
                raw,
                extra,
                &plan.arena,
            ),
            plan,
            last: 0,
            eot: planned_eot(track, index, plan),
            pending: None,
            prefix: out.name.as_deref(),
            suffix: out.record.as_deref(),
        }
    }

    pub fn peak_polyphony(&self) -> usize {
        self.inner.peak_polyphony()
    }

    /// Also counts same-key overlaps (the writer does; readers of the
    /// events need not).
    pub fn counting_overlaps(self) -> Self {
        PlannedStream {
            inner: self.inner.counting_overlaps(),
            ..self
        }
    }

    /// Same-key overlaps in this chunk ([`SameKeyOverlaps`]; complete once
    /// the stream is exhausted, zero unless
    /// [`PlannedStream::counting_overlaps`]).
    pub fn same_key_overlaps(&self) -> SameKeyOverlaps {
        self.inner.same_key_overlaps()
    }

    /// The End-of-Track tick to write after the last event.
    pub fn end_of_track(&self) -> u64 {
        self.eot.max(self.last)
    }

    fn filler(&mut self) -> (u64, OutEvent<'a>) {
        self.last += MAX_DELTA_TICKS;
        (
            self.last,
            OutEvent::Meta {
                meta_type: seqspec::META_SEQUENCER_SPECIFIC,
                payload: &seqspec::GAP_FILLER,
            },
        )
    }
}

impl<'a> Iterator for PlannedStream<'a> {
    type Item = (u64, OutEvent<'a>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(name) = self.prefix.take() {
            return Some((
                0,
                OutEvent::Meta {
                    meta_type: crate::model::events::meta::TRACK_NAME,
                    payload: name,
                },
            ));
        }
        let next = match self.pending.take() {
            Some(p) => Some(p),
            None => self
                .inner
                .next()
                .map(|(tick, ev)| (self.plan.map_tick(tick), ev)),
        };
        match next {
            Some((tick, ev)) => {
                if tick - self.last > MAX_DELTA_TICKS {
                    self.pending = Some((tick, ev));
                    return Some(self.filler());
                }
                self.last = tick;
                Some((tick, ev))
            }
            None => {
                // Bridge the silence up to End of Track as well.
                if self.eot > self.last && self.eot - self.last > MAX_DELTA_TICKS {
                    return Some(self.filler());
                }
                let record = self.suffix.take()?;
                self.last = self.eot.max(self.last);
                Some((
                    self.last,
                    OutEvent::Meta {
                        meta_type: seqspec::META_SEQUENCER_SPECIFIC,
                        payload: record,
                    },
                ))
            }
        }
    }
}

fn off_event(attrs: NoteAttrs) -> OutEvent<'static> {
    OutEvent::NoteOff {
        channel: attrs.channel(),
        pitch: attrs.pitch(),
        velocity: attrs.off_velocity(),
        kind: attrs.off_kind(),
    }
}

/// A track's events for [`RawSource`]: a slice read by index (a track's own
/// events, or the plan's corrected copy — the common case, as cheap as
/// before clips) or a clip track's streamed events.
enum RawEvents<'a> {
    Slice {
        ev: &'a [RawEvent],
        i: usize,
    },
    /// Boxed: keeps the stream as small as before clips (the common case).
    Placed(Box<std::iter::Peekable<crate::model::placed::PlacedEvents<'a>>>),
}

impl<'a> RawEvents<'a> {
    fn new(e: ExportEvents<'a>) -> Self {
        match e {
            ExportEvents::Slice(it) => RawEvents::Slice {
                ev: it.as_slice(),
                i: 0,
            },
            ExportEvents::Placed(p) => RawEvents::Placed(Box::new(p.peekable())),
        }
    }

    /// Tick of the next event.
    #[inline]
    fn peek_tick(&mut self) -> Option<u64> {
        match self {
            RawEvents::Slice { ev, i } => ev.get(*i).map(|e| e.tick),
            RawEvents::Placed(p) => p.peek().map(|(t, _)| *t),
        }
    }

    /// The next event if it is at tick `t`.
    #[inline]
    fn next_at(&mut self, t: u64) -> Option<&'a RawEvent> {
        match self {
            RawEvents::Slice { ev, i } => {
                let ev: &'a [RawEvent] = ev;
                let e = ev.get(*i).filter(|e| e.tick == t)?;
                *i += 1;
                Some(e)
            }
            RawEvents::Placed(p) => p.next_if(|(et, _)| *et == t).map(|(_, e)| e),
        }
    }
}

/// A track's preserved events merged with planned (Conductor) events, in the
/// order they are written. Within one tick the order is by anchor, then planned
/// events before the track event they were ranked in front of.
struct RawSource<'a> {
    /// The track's events as written: (document tick, event).
    raw: RawEvents<'a>,
    plan: &'a [PlanEvent],
    plan_i: usize,
    arena: &'a [u8],
    /// Merged events of the current tick: (tick, anchor, event).
    buf: Vec<(u64, u32, OutEvent<'a>)>,
    buf_i: usize,
    /// Every event has been taken.
    done: bool,
}

impl<'a> RawSource<'a> {
    /// Loads the next tick's events into `buf` (called when `buf` is used up;
    /// leaves it empty when no event remains).
    #[inline(never)]
    fn fill(&mut self) {
        self.buf.clear();
        self.buf_i = 0;
        let a = self.raw.peek_tick();
        let b = self.plan.get(self.plan_i).map(|e| e.tick);
        let Some(t) = [a, b].into_iter().flatten().min() else {
            return;
        };
        if self.plan.get(self.plan_i).is_none_or(|p| p.tick != t) {
            // Fast path: no planned events at this tick.
            while let Some(e) = self.raw.next_at(t) {
                self.buf.push((t, e.anchor, OutEvent::Raw(&e.kind)));
            }
            return;
        }
        // (anchor, position key, event); raw event j gets key 2j + 1, a planned
        // event ranked before raw event k gets 2k.
        let mut items: Vec<(u32, u64, OutEvent<'a>)> = Vec::new();
        let mut j = 0u64;
        while let Some(e) = self.raw.next_at(t) {
            items.push((e.anchor, 2 * j + 1, OutEvent::Raw(&e.kind)));
            j += 1;
        }
        let arena = self.arena;
        while let Some(p) = self.plan.get(self.plan_i).filter(|p| p.tick == t) {
            let ev = match p.kind {
                PlanKind::Tempo(us) => OutEvent::Tempo(us),
                PlanKind::Meta {
                    meta_type,
                    offset,
                    len,
                } => OutEvent::Meta {
                    meta_type,
                    payload: &arena[offset as usize..offset as usize + len as usize],
                },
                PlanKind::SysEx { offset, len } => OutEvent::SysEx {
                    payload: &arena[offset as usize..offset as usize + len as usize],
                },
                PlanKind::DefaultProgram(ch) => {
                    OutEvent::Raw(&super::plan::DEFAULT_PROGRAM[usize::from(ch & 0x0F)])
                }
                PlanKind::VolumeZero(ch) => {
                    OutEvent::Raw(&super::plan::VOLUME_ZERO[usize::from(ch & 0x0F)])
                }
                PlanKind::ReferenceOn { channel, key } => OutEvent::NoteOn {
                    channel,
                    pitch: key,
                    velocity: 127,
                },
                PlanKind::ReferenceOff { channel, key } => OutEvent::NoteOff {
                    channel,
                    pitch: key,
                    velocity: 0,
                    kind: NoteOffKind::NoteOff,
                },
            };
            let key = if p.rank == u32::MAX {
                u64::MAX
            } else {
                2 * u64::from(p.rank)
            };
            items.push((p.anchor, key, ev));
            self.plan_i += 1;
        }
        // Stable: planned events with equal keys keep the plan's order.
        items.sort_by_key(|(anchor, key, _)| (*anchor, *key));
        self.buf
            .extend(items.into_iter().map(|(anchor, _, ev)| (t, anchor, ev)));
    }

    /// Tick and anchor of the next event. Called several times per note event,
    /// so the common cases (events buffered, or none left) return at once.
    #[inline]
    fn peek(&mut self) -> Option<(u64, u32)> {
        if self.buf_i >= self.buf.len() {
            // Once every event is written, a track's remaining notes pay one
            // test per call.
            if self.done {
                return None;
            }
            self.fill();
            if self.buf.is_empty() {
                self.done = true;
                return None;
            }
        }
        self.buf.get(self.buf_i).map(|(t, a, _)| (*t, *a))
    }

    #[inline]
    fn take(&mut self) -> OutEvent<'a> {
        let ev = self.buf[self.buf_i].2;
        self.buf_i += 1;
        ev
    }
}

/// Produces a track's events in export order (see module docs).
pub struct TrackEventStream<'a> {
    notes: NoteWriteOrder<'a>,
    /// Pending note ends: (end tick, sequence, packed attrs), min-heap.
    ends: BinaryHeap<Reverse<(u64, u64, u32)>>,
    seq: u64,
    raw: RawSource<'a>,
    tick: Option<u64>,
    note_events_at_tick: u32,
    pending_zero_off: Option<NoteAttrs>,
    max_heap: usize,
}

impl<'a> TrackEventStream<'a> {
    pub fn new(track: &'a Track) -> Self {
        TrackEventStream::with_plan(track, &[], &[])
    }

    /// A stream that also writes `plan` events (sorted by tick) in this track.
    pub fn with_plan(track: &'a Track, plan: &'a [PlanEvent], arena: &'a [u8]) -> Self {
        TrackEventStream::with_parts(
            Box::new(track.notes.iter_audible()),
            ExportEvents::Slice(track.events.events().iter()),
            plan,
            arena,
        )
    }

    /// A stream over `notes` (sorted by start) and `raw` events (sorted by tick)
    /// plus planned events.
    pub fn with_parts(
        notes: Box<dyn Iterator<Item = NoteView> + 'a>,
        raw: ExportEvents<'a>,
        plan: &'a [PlanEvent],
        arena: &'a [u8],
    ) -> Self {
        TrackEventStream {
            notes: NoteWriteOrder::new(notes),
            ends: BinaryHeap::new(),
            seq: 0,
            raw: RawSource {
                raw: RawEvents::new(raw),
                plan,
                plan_i: 0,
                arena,
                buf: Vec::new(),
                buf_i: 0,
                done: false,
            },
            tick: None,
            note_events_at_tick: 0,
            pending_zero_off: None,
            max_heap: 0,
        }
    }

    /// Largest number of simultaneously pending note ends seen so far.
    pub fn peak_polyphony(&self) -> usize {
        self.max_heap
    }

    /// Also counts same-key overlaps ([`NoteWriteOrder::counting`]).
    pub fn counting_overlaps(self) -> Self {
        TrackEventStream {
            notes: self.notes.counting(),
            ..self
        }
    }

    /// Same-key overlaps among the notes read so far (all of them once the
    /// stream is exhausted; zero unless [`TrackEventStream::counting_overlaps`]).
    pub fn same_key_overlaps(&self) -> SameKeyOverlaps {
        self.notes.overlaps()
    }

    fn next_tick_candidate(&mut self) -> Option<u64> {
        let a = self.notes.peek_start();
        let b = self.ends.peek().map(|Reverse((e, _, _))| *e);
        let c = self.raw.peek().map(|(t, _)| t);
        [a, b, c].into_iter().flatten().min()
    }

    fn count_note_event(&mut self) {
        self.note_events_at_tick = self.note_events_at_tick.saturating_add(1);
    }
}

impl<'a> Iterator for TrackEventStream<'a> {
    type Item = (u64, OutEvent<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let Some(t) = self.tick else {
                let t = self.next_tick_candidate()?;
                self.tick = Some(t);
                self.note_events_at_tick = 0;
                continue;
            };
            // (a) preserved event anchored before the next note event at this tick
            if let Some((et, anchor)) = self.raw.peek()
                && et == t
                && anchor <= self.note_events_at_tick
            {
                return Some((t, self.raw.take()));
            }
            // (b) terminations of earlier notes ending here
            if let Some(Reverse((end, _, attrs))) = self.ends.peek().copied()
                && end == t
            {
                self.ends.pop();
                self.count_note_event();
                let attrs = NoteAttrs::from_raw(attrs).unwrap_or_default();
                return Some((t, off_event(attrs)));
            }
            // (c) termination of a zero-duration note just started
            if let Some(attrs) = self.pending_zero_off.take() {
                self.count_note_event();
                return Some((t, off_event(attrs)));
            }
            // (d) note starts at this tick in stored order
            if self.notes.peek_start() == Some(t)
                && let Some(n) = self.notes.next()
            {
                self.count_note_event();
                if n.duration_ticks == 0 {
                    self.pending_zero_off = Some(n.attrs);
                } else {
                    self.ends
                        .push(Reverse((n.end_tick(), self.seq, n.attrs.raw())));
                    self.max_heap = self.max_heap.max(self.ends.len());
                }
                self.seq += 1;
                let a = n.attrs;
                return Some((
                    t,
                    OutEvent::NoteOn {
                        channel: a.channel(),
                        pitch: a.pitch(),
                        velocity: a.velocity(),
                    },
                ));
            }
            // (e) remaining preserved events at this tick
            if let Some((et, _)) = self.raw.peek()
                && et == t
            {
                return Some((t, self.raw.take()));
            }
            // (f) nothing left at this tick
            self.tick = None;
        }
    }
}

/// Where an export failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportStage {
    CreateTemp,
    Write,
    Flush,
    Validate,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    TooManyTracks(usize),
    /// The plan found something no MIDI file can hold (see `ExportPlan::blockers`).
    Blocked(String),
    /// A track's data exceeds the 4 GiB chunk limit.
    TrackTooLarge {
        track: usize,
    },
    Io {
        stage: ExportStage,
        kind: io::ErrorKind,
        message: String,
    },
    /// The written file did not decode to the expected events.
    Validation {
        detail: String,
    },
    Cancelled,
}

impl ExportError {
    pub(crate) fn io(stage: ExportStage, e: io::Error) -> Self {
        ExportError::Io {
            stage,
            kind: e.kind(),
            message: e.to_string(),
        }
    }

    /// Concise user-facing explanation. Always states whether the destination was
    /// touched (it never is on failure).
    pub fn user_message(&self) -> String {
        match self {
            ExportError::TooManyTracks(n) => format!(
                "A MIDI file can hold at most 65,535 tracks; this project has {}.",
                group_digits(*n as u64)
            ),
            ExportError::Blocked(why) => format!("{why} Nothing was written."),
            ExportError::TrackTooLarge { track } => format!(
                "Track {} is larger than the 4 GB a MIDI track can hold.",
                track + 1
            ),
            ExportError::Io { stage, message, .. } => match stage {
                ExportStage::CreateTemp => format!(
                    "PlumSpirit could not create a temporary file next to the destination: {message}. Nothing was changed."
                ),
                ExportStage::Write | ExportStage::Flush => format!(
                    "Writing the MIDI failed: {message}. Your previous destination file was not replaced."
                ),
                ExportStage::Validate => format!(
                    "PlumSpirit wrote the temporary MIDI but could not read it back: {message}. Your previous destination file was not replaced."
                ),
                ExportStage::Replace => format!(
                    "The MIDI was written and validated, but it could not replace the destination: {message}. Your previous destination file was not replaced."
                ),
            },
            ExportError::Validation { detail } => format!(
                "PlumSpirit wrote the temporary MIDI, but validation failed ({detail}). Your previous destination file was not replaced."
            ),
            ExportError::Cancelled => {
                "Export was canceled. Your previous destination file was not replaced.".into()
            }
        }
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for ExportError {}

/// Statistics about the written data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteStats {
    pub bytes: u64,
    /// v1.0-beta1 (B1-0162): per output chunk, the digest of the events as
    /// written ([`hash_out_event`] + End of Track), for validation.
    pub chunk_digests: Vec<u64>,
    pub tracks: usize,
    pub notes: usize,
    pub preserved_events: usize,
    pub format_word: u16,
    /// Most note-offs one track's writer held pending at once (a bound on
    /// the export buffer, per track, as written) — not the Poly metric of
    /// `analysis::canonical`.
    pub peak_polyphony: usize,
    /// (v0.9 V09-0102) Document tracks whose chunks hold notes that start
    /// inside a note of the same channel and key, with their counts (summed
    /// over a track's chunks), by track. See [`super::overlap`].
    pub same_key: Vec<(usize, SameKeyOverlaps)>,
    /// v1.0-beta1 (B1-0123): status bytes running status left out (0 with
    /// `ExportOptions::explicit_status`).
    pub status_bytes_saved: u64,
    /// v1.0-Launch L-C02: the highest Note On velocity written (0 without
    /// notes). Piano From Above scales every velocity by up to 127 ÷ this
    /// (its volume correction), so a file whose loudest note is quiet plays
    /// loud there although its bytes are exact
    /// (`docs/references/v1_0_launch/KIVA_NOTES.md`).
    pub max_note_velocity: u8,
}

/// Progress/cancellation hooks for background exports.
#[derive(Default, Clone, Copy)]
pub struct ExportControl<'a> {
    pub cancel: Option<&'a AtomicBool>,
    /// Receives the number of notes written so far.
    pub progress_notes: Option<&'a AtomicU64>,
}

/// Checks document-level limits that do not require walking events.
pub fn check_document_limits(doc: &PlumDocument) -> Result<(), ExportError> {
    if doc.track_count() > usize::from(u16::MAX) {
        return Err(ExportError::TooManyTracks(doc.track_count()));
    }
    Ok(())
}

fn check_plan(doc: &PlumDocument, plan: &ExportPlan) -> Result<(), ExportError> {
    if let Some(b) = plan.blockers.first() {
        return Err(ExportError::Blocked(b.clone()));
    }
    // v0.9 clips: a plan reads clip tracks through `ExportPlan::placed`; one
    // made for another state of the project would write the wrong notes.
    if !doc.clips_are_identity() && plan.placed.len() != doc.track_count() {
        return Err(ExportError::Blocked(
            "The export plan was made for a different state of the project. Export again.".into(),
        ));
    }
    Ok(())
}

/// Serializes `doc` into `w` according to `plan` (see [`plan_export`]). `w` must be
/// seekable because track chunk lengths are patched after each track is streamed.
pub fn write_document<W: Write + Seek>(
    doc: &PlumDocument,
    plan: &ExportPlan,
    w: &mut W,
    control: ExportControl<'_>,
) -> Result<WriteStats, ExportError> {
    // About the size of the file: what decides whether workers pay off.
    let estimate = (2 * doc.note_count() as u64 + doc.preserved_event_count() as u64) * 4;
    let threads = super::workers::threads_for(Some(estimate));
    write_document_threads(
        doc,
        plan,
        w,
        control,
        threads,
        super::workers::window_for(threads),
    )
}

/// Upper bound of one output chunk's data bytes: every event at most 7 bytes
/// (a 4-byte delta, status, two data bytes) plus the payload arena; drives
/// the worker window only.
fn chunk_estimate(track: &Track, out: &OutputTrack, plan: &ExportPlan) -> u64 {
    let notes = plan.stored_notes(track, out.track) as u64;
    let events = plan.event_count(track, out.track) as u64
        + if out.track == 0 {
            plan.events.len() as u64
        } else {
            0
        };
    (2 * notes + events + 16) * 7 + track.events.heap_bytes() as u64
}

/// The extra chunks and the statistics that come before output chunk `oi`.
fn before_chunk<W: Write>(
    doc: &PlumDocument,
    plan: &ExportPlan,
    out: &OutputTrack,
    w: &mut W,
    stats: &mut WriteStats,
) -> Result<(), ExportError> {
    let ti = out.track;
    let track = doc.track(ti).ok_or(ExportError::Blocked(
        "The export plan refers to a track that no longer exists.".into(),
    ))?;
    if out.layer == 0 {
        stats.bytes += write_extra_chunks(doc, ti, w)?;
        stats.notes += plan.stored_notes(track, ti);
        stats.preserved_events +=
            plan.event_count(track, ti) + if ti == 0 { plan.events.len() } else { 0 };
    }
    Ok(())
}

/// Adds a written chunk's statistics.
fn after_chunk(stats: &mut WriteStats, ti: usize, n: TrackWriteStats) {
    stats.bytes += n.bytes;
    stats.peak_polyphony = stats.peak_polyphony.max(n.peak_polyphony);
    stats.status_bytes_saved += n.status_bytes_saved;
    stats.max_note_velocity = stats.max_note_velocity.max(n.max_velocity);
    stats.chunk_digests.push(n.digest);
    if n.same_key.any() {
        stats.same_key.push((ti, n.same_key));
    }
}

/// A chunk encoded on a worker: its data, statistics and notes written.
type Encoded = Result<(Vec<u8>, TrackWriteStats, u64), ExportError>;

/// [`write_document`] with `threads` encoding workers (0 = every chunk in
/// place). Chunks whose estimate fits the `window` are encoded on the
/// workers into memory and written in chunk order by this thread; a larger
/// chunk is encoded in place, streaming. The bytes, the statistics and the
/// first error are the same as with one thread (v1.0-beta1 B1-0162).
pub(crate) fn write_document_threads<W: Write + Seek>(
    doc: &PlumDocument,
    plan: &ExportPlan,
    w: &mut W,
    control: ExportControl<'_>,
    threads: usize,
    window: u64,
) -> Result<WriteStats, ExportError> {
    check_document_limits(doc)?;
    check_plan(doc, plan)?;
    let wr = |e: io::Error| ExportError::io(ExportStage::Write, e);
    let format = plan.format(doc);
    let layout = plan.layout(doc);
    if layout.len() > usize::from(u16::MAX) {
        return Err(ExportError::TooManyTracks(layout.len()));
    }
    let mut stats = WriteStats {
        format_word: format.word(),
        tracks: layout.len(),
        ..WriteStats::default()
    };
    w.write_all(b"MThd").map_err(wr)?;
    w.write_all(&6u32.to_be_bytes()).map_err(wr)?;
    w.write_all(&format.word().to_be_bytes()).map_err(wr)?;
    w.write_all(&(layout.len() as u16).to_be_bytes())
        .map_err(wr)?;
    w.write_all(&plan.division_word.to_be_bytes()).map_err(wr)?;
    stats.bytes = 14;

    let mut notes_done: u64 = 0;
    if threads == 0 {
        for (oi, out) in layout.iter().enumerate() {
            before_chunk(doc, plan, out, w, &mut stats)?;
            let track = &doc.tracks()[out.track];
            let n = write_track(track, out, oi, plan, w, &mut notes_done, control)?;
            after_chunk(&mut stats, out.track, n);
        }
    } else {
        let cancel = control.cancel;
        let layout_ref: &[OutputTrack] = &layout;
        let encode = |oi: usize| -> Encoded {
            let out = &layout_ref[oi];
            let track = &doc.tracks()[out.track];
            let mut data: Vec<u8> = Vec::new();
            let mut done = 0u64;
            let quiet = ExportControl {
                cancel,
                progress_notes: None,
            };
            let n = encode_chunk(track, out, plan, &mut done, quiet, &mut |b: &[u8]| {
                data.try_reserve(b.len())
                    .map_err(|_| ExportError::TrackTooLarge { track: oi })?;
                data.extend_from_slice(b);
                Ok(())
            })?;
            Ok((data, n, done))
        };
        super::workers::run(threads, window, encode, |pipe| {
            // Writes the oldest submitted chunk (they are written in order).
            let write_oldest = |pipe: &mut super::workers::Pipeline<usize, Encoded>,
                                w: &mut W,
                                stats: &mut WriteStats,
                                notes_done: &mut u64|
             -> Result<(), ExportError> {
                let Some(oi) = pipe.oldest() else {
                    return Ok(());
                };
                let r = pipe.take(oi, cancel).ok_or_else(|| {
                    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                        ExportError::Cancelled
                    } else {
                        ExportError::Blocked(
                            "An export worker stopped unexpectedly. Nothing was written.".into(),
                        )
                    }
                })?;
                let (data, mut n, done) = r?;
                // The chunk header, as `write_track` counts it.
                n.bytes += 8;
                let out = &layout_ref[oi];
                before_chunk(doc, plan, out, w, stats)?;
                let len32 = u32::try_from(data.len())
                    .map_err(|_| ExportError::TrackTooLarge { track: oi })?;
                w.write_all(b"MTrk").map_err(wr)?;
                w.write_all(&len32.to_be_bytes()).map_err(wr)?;
                w.write_all(&data).map_err(wr)?;
                *notes_done += done;
                if let Some(p) = control.progress_notes {
                    p.store(*notes_done, Ordering::Relaxed);
                }
                after_chunk(stats, out.track, n);
                Ok(())
            };
            for (oi, out) in layout_ref.iter().enumerate() {
                let track = &doc.tracks()[out.track];
                let est = chunk_estimate(track, out, plan);
                if est <= pipe.window() {
                    while !pipe.has_room(est) {
                        write_oldest(pipe, w, &mut stats, &mut notes_done)?;
                    }
                    pipe.submit(oi, oi, est);
                } else {
                    while pipe.oldest().is_some() {
                        write_oldest(pipe, w, &mut stats, &mut notes_done)?;
                    }
                    before_chunk(doc, plan, out, w, &mut stats)?;
                    let n = write_track(track, out, oi, plan, w, &mut notes_done, control)?;
                    after_chunk(&mut stats, out.track, n);
                }
            }
            while pipe.oldest().is_some() {
                write_oldest(pipe, w, &mut stats, &mut notes_done)?;
            }
            Ok::<(), ExportError>(())
        })?;
    }
    stats.same_key = merge_by_track(std::mem::take(&mut stats.same_key));
    stats.bytes += write_extra_chunks(doc, doc.track_count(), w)?;
    Ok(stats)
}

/// Sorts per-chunk overlap counts by document track and sums each track's.
pub(crate) fn merge_by_track(
    mut v: Vec<(usize, SameKeyOverlaps)>,
) -> Vec<(usize, SameKeyOverlaps)> {
    v.sort_by_key(|&(t, _)| t);
    let mut out: Vec<(usize, SameKeyOverlaps)> = Vec::with_capacity(v.len());
    for (t, o) in v {
        match out.last_mut() {
            Some((last, sum)) if *last == t => sum.add(o),
            _ => out.push((t, o)),
        }
    }
    out
}

fn write_extra_chunks<W: Write>(
    doc: &PlumDocument,
    before_track: usize,
    w: &mut W,
) -> Result<u64, ExportError> {
    let wr = |e: io::Error| ExportError::io(ExportStage::Write, e);
    let mut bytes = 0;
    for c in doc.extra_chunks() {
        let pos = c.after_track_chunks.min(doc.track_count());
        if pos != before_track {
            continue;
        }
        let len = u32::try_from(c.data.len()).map_err(|_| ExportError::TrackTooLarge {
            track: before_track,
        })?;
        w.write_all(&c.chunk_type).map_err(wr)?;
        w.write_all(&len.to_be_bytes()).map_err(wr)?;
        w.write_all(&c.data).map_err(wr)?;
        bytes += 8 + c.data.len() as u64;
    }
    Ok(bytes)
}

struct TrackWriteStats {
    bytes: u64,
    digest: u64,
    peak_polyphony: usize,
    same_key: SameKeyOverlaps,
    status_bytes_saved: u64,
    max_velocity: u8,
}

fn write_track<W: Write + Seek>(
    track: &Track,
    out: &OutputTrack,
    index: usize,
    plan: &ExportPlan,
    w: &mut W,
    notes_done: &mut u64,
    control: ExportControl<'_>,
) -> Result<TrackWriteStats, ExportError> {
    let wr = |e: io::Error| ExportError::io(ExportStage::Write, e);
    w.write_all(b"MTrk").map_err(wr)?;
    let len_pos = w.stream_position().map_err(wr)?;
    w.write_all(&[0, 0, 0, 0]).map_err(wr)?;
    let mut n = encode_chunk(track, out, plan, notes_done, control, &mut |b: &[u8]| {
        w.write_all(b).map_err(wr)
    })?;
    let data_len = n.bytes;
    let len32 = u32::try_from(data_len).map_err(|_| ExportError::TrackTooLarge { track: index })?;
    let end_pos = w.stream_position().map_err(wr)?;
    w.seek(SeekFrom::Start(len_pos)).map_err(wr)?;
    w.write_all(&len32.to_be_bytes()).map_err(wr)?;
    w.seek(SeekFrom::Start(end_pos)).map_err(wr)?;
    n.bytes = 8 + data_len;
    Ok(n)
}

/// Encodes output chunk `index`'s data (its events and End of Track, no
/// chunk header), handing it to `sink` in pieces of about 60 KiB. The
/// returned `bytes` counts the data only; a writer adds the 8 header bytes.
fn encode_chunk(
    track: &Track,
    out: &OutputTrack,
    plan: &ExportPlan,
    notes_done: &mut u64,
    control: ExportControl<'_>,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), ExportError>,
) -> Result<TrackWriteStats, ExportError> {
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut data_len: u64 = 0;
    let mut last_tick: u64 = 0;
    let mut running: Option<u8> = None;
    let mut saved: u64 = 0;
    // v0.9 §062: keys above 127 are written only by the experimental file.
    let wide = plan.wide.policy.writes_wide();
    // v0.9 (V09-0050): Maximum Compatibility writes every status byte.
    let explicit = plan.explicit_status;
    let mut stream = PlannedStream::for_output(track, out, plan).counting_overlaps();
    let mut since_check: u32 = 0;
    let mut scratch = Vec::with_capacity(64);
    let mut digest = WordHash::new();
    let mut max_velocity: u8 = 0;

    let mut flush = |buf: &mut Vec<u8>, data_len: &mut u64| -> Result<(), ExportError> {
        sink(buf)?;
        *data_len += buf.len() as u64;
        buf.clear();
        Ok(())
    };
    for (tick, ev) in stream.by_ref() {
        let delta = tick - last_tick;
        debug_assert!(delta <= MAX_DELTA_TICKS, "gap fillers bound every delta");
        write_vlq(delta, &mut buf);
        last_tick = tick;
        if let Some(status) = ev.channel_status() {
            let data = ev.channel_data();
            // `hash_out_event` for a channel message, without decoding it again.
            let d1 = if channel_data_len(status) < 2 {
                0
            } else {
                data[1]
            };
            digest.pair(
                tick,
                u64::from_le_bytes([status, data[0], d1, 0, 0, 0, 0, 1]),
            );
            if data[0] > 0x7F {
                // v0.9: a key above 127. A Standard MIDI file never gets one
                // (the plan's WideKeys policy maps or drops such notes before
                // they get here: this is the last guard). The experimental
                // file writes it Kiva's way: the unmasked key byte right after
                // an explicit Note On / Off status — never running status, which
                // a reader detects by a first byte below 0x80 — and the next
                // event repeats its status too.
                if !(wide && status & 0xE0 == 0x80) {
                    return Err(ExportError::Blocked(format!(
                        "Track {}: a note at key {} is above 127 and cannot be written to a Standard MIDI file. Nothing was written.",
                        out.track + 1,
                        data[0]
                    )));
                }
                buf.push(status);
                running = None;
            } else if running != Some(status) || explicit {
                buf.push(status);
                running = Some(status);
            } else {
                saved += 1;
            }
            buf.extend_from_slice(&data[..channel_data_len(status)]);
            if let OutEvent::NoteOn { velocity, .. } = ev {
                *notes_done += 1;
                max_velocity = max_velocity.max(velocity);
            }
        } else if let OutEvent::Raw(kind) = ev {
            hash_out_event(&mut digest, tick, &ev, track);
            // Meta, SysEx and system messages cancel running status (SMF 1.0).
            running = None;
            scratch.clear();
            track.events.encode_event(kind, &mut scratch);
            buf.extend_from_slice(&scratch);
        } else if let OutEvent::Tempo(us) = ev {
            hash_out_event(&mut digest, tick, &ev, track);
            running = None;
            encode_tempo(us, &mut buf);
        } else if let OutEvent::Meta { meta_type, payload } = ev {
            hash_out_event(&mut digest, tick, &ev, track);
            running = None;
            buf.push(0xFF);
            buf.push(meta_type);
            write_vlq(payload.len() as u64, &mut buf);
            buf.extend_from_slice(payload);
        } else if let OutEvent::SysEx { payload } = ev {
            hash_out_event(&mut digest, tick, &ev, track);
            running = None;
            buf.push(0xF0);
            write_vlq(payload.len() as u64, &mut buf);
            buf.extend_from_slice(payload);
        }
        if buf.len() >= 60 * 1024 {
            flush(&mut buf, &mut data_len)?;
        }
        since_check += 1;
        if since_check >= 65_536 {
            since_check = 0;
            if let Some(p) = control.progress_notes {
                p.store(*notes_done, Ordering::Relaxed);
            }
            if control.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(ExportError::Cancelled);
            }
        }
    }
    // End of Track at the preserved original position (or the last Conductor
    // event), or at the last event if notes now extend beyond it.
    // The digest takes the End of Track validation expects (the stream's);
    // a written one that differs fails the comparison.
    digest.pair(stream.end_of_track(), EOT_DIGEST_MARK);
    let eot = stream.end_of_track().max(last_tick);
    let delta = eot - last_tick;
    debug_assert!(delta <= MAX_DELTA_TICKS, "gap fillers bound every delta");
    write_vlq(delta, &mut buf);
    buf.extend_from_slice(&[0xFF, 0x2F, 0x00]);
    flush(&mut buf, &mut data_len)?;

    if let Some(p) = control.progress_notes {
        p.store(*notes_done, Ordering::Relaxed);
    }
    Ok(TrackWriteStats {
        bytes: data_len,
        digest: digest.finish(),
        peak_polyphony: stream.peak_polyphony(),
        same_key: stream.same_key_overlaps(),
        status_bytes_saved: saved,
        max_velocity,
    })
}

/// Serializes into memory with the automatic plan (tests and fixtures).
pub fn export_to_bytes(doc: &PlumDocument) -> Result<Vec<u8>, ExportError> {
    export_to_bytes_with(doc, &plan_export(doc))
}

/// Serializes into memory with an explicit plan.
pub fn export_to_bytes_with(doc: &PlumDocument, plan: &ExportPlan) -> Result<Vec<u8>, ExportError> {
    let mut cur = io::Cursor::new(Vec::new());
    write_document(doc, plan, &mut cur, ExportControl::default())?;
    Ok(cur.into_inner())
}

/// Outcome of a successful, validated export.
#[derive(Clone, Debug)]
pub struct ExportReport {
    pub path: PathBuf,
    pub write: WriteStats,
    pub validation: ValidationReport,
    /// The conversions that were applied (tempo, resolution).
    pub plan: ExportPlan,
    /// True when a Format 0 source was written as Format 1 because it now has
    /// several tracks.
    pub promoted_to_format1: bool,
    pub write_duration: Duration,
    pub validate_duration: Duration,
}

/// Removes the temporary file unless disarmed.
pub(crate) struct TempGuard {
    pub(crate) path: Option<PathBuf>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            // Best effort: if removal fails the temp file is left behind, but the
            // destination is untouched either way.
            let _ = fs::remove_file(p);
        }
    }
}

pub(crate) fn temp_path_for(dest: &Path, attempt: u32) -> PathBuf {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut name = OsString::from(".");
    name.push(
        dest.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("export")),
    );
    name.push(format!(
        ".plumspirit-{}-{}.tmp",
        std::process::id(),
        attempt
    ));
    dir.join(name)
}

/// Exports safely:
///
/// 1. write to a new temporary file in the destination directory,
/// 2. flush and sync it,
/// 3. re-open and validate it event by event against the document,
/// 4. only then atomically replace the destination (rename).
///
/// On any failure the temporary file is removed and the destination is untouched.
pub fn export_to_path(
    doc: &PlumDocument,
    plan: &ExportPlan,
    dest: &Path,
    control: ExportControl<'_>,
) -> Result<ExportReport, ExportError> {
    check_document_limits(doc)?;
    check_plan(doc, plan)?;
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
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let stats = write_document(doc, plan, &mut w, control)?;
    let file: File = w
        .into_inner()
        .map_err(|e| ExportError::io(ExportStage::Flush, e.into_error()))?;
    file.sync_all()
        .map_err(|e| ExportError::io(ExportStage::Flush, e))?;
    drop(file);
    let write_duration = started.elapsed();

    let v_start = Instant::now();
    if control.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(ExportError::Cancelled);
    }
    let validation = validate_written_file(doc, plan, &temp, &stats.chunk_digests)?;
    let validate_duration = v_start.elapsed();

    fs::rename(&temp, dest).map_err(|e| ExportError::io(ExportStage::Replace, e))?;
    guard.path = None;
    Ok(ExportReport {
        path: dest.to_path_buf(),
        write: stats,
        validation,
        plan: plan.clone(),
        promoted_to_format1: doc.source_format() == SmfFormat::SingleTrack
            && doc.export_format() == SmfFormat::Parallel,
        write_duration,
        validate_duration,
    })
}

/// V09-S07: a document with clips exports what plays — the same notes and
/// controllers as the flattened twin — and validates; without clips (or with
/// only identity records) the bytes are exactly v0.8's.
#[cfg(test)]
mod clip_export_tests {
    use super::*;
    use crate::model::ClipPlacement;
    use crate::model::clip::test_support::{base, clipped, flatten};
    use crate::model::events::RawKind;

    /// (start, length, key, velocity, channel) of every note; (tick, status,
    /// data) of every channel message.
    type Music = (Vec<(u64, u64, u8, u8, u8)>, Vec<(u64, u8, u8, u8)>);

    /// Every note and every channel message of a document, sorted.
    fn music(d: &PlumDocument) -> Music {
        let mut notes = Vec::new();
        let mut cc = Vec::new();
        for t in d.tracks() {
            for n in t.notes.iter() {
                let a = n.attrs;
                notes.push((
                    n.start_tick,
                    n.duration_ticks,
                    a.pitch(),
                    a.velocity(),
                    a.channel(),
                ));
            }
            for e in t.events.events() {
                if let RawKind::Channel { status, data } = e.kind {
                    cc.push((e.tick, status, data[0], data[1]));
                }
            }
        }
        notes.sort_unstable();
        cc.sort_unstable();
        (notes, cc)
    }

    #[test]
    fn clips_export_what_plays_and_validate() {
        let d = clipped();
        let bytes = export_to_bytes(&d).unwrap();
        let (back, _) = crate::midi::import_bytes(&bytes).unwrap();
        assert_eq!(music(&back), music(&flatten(&d)));
        assert_ne!(music(&back), music(&base()), "clips change what is written");
        // The transactional export validates against what it wrote.
        let dir = std::env::temp_dir().join(format!("plumspirit-clipexp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clips.mid");
        let plan = crate::midi::plan_export(&d);
        let report = export_to_path(&d, &plan, &path, ExportControl::default()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(report.write.notes > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn identity_clips_export_exactly_as_before() {
        let d = base();
        let plain = export_to_bytes(&d).unwrap();
        let mut named = d.clone();
        let t0 = named.track(0).unwrap().id();
        named.clips_mut().set_placement(ClipPlacement {
            name: Some("Intro".into()),
            color: Some(0x123456),
            ..ClipPlacement::primary(t0)
        });
        assert!(!named.clips_are_identity());
        assert_eq!(export_to_bytes(&named).unwrap(), plain);
        assert!(matches!(
            d.materialize_placements(),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}

/// B1-0162: writing with workers gives the bytes and statistics of one
/// thread; the digest validation gives the full validation's report, on
/// workers too, and the full validation's error for a damaged file.
#[cfg(test)]
mod worker_tests {
    use super::*;
    use crate::midi::test_smf::{mutations, rich};
    use crate::midi::validate::{
        check_digests_threads, validate_exported_bytes, validate_written_bytes,
    };
    use crate::model::clip::test_support::{base, clipped};

    fn docs() -> Vec<PlumDocument> {
        let mut out = vec![base(), clipped()];
        let mut files = vec![rich()];
        files.extend(mutations(&rich(), 400));
        out.extend(
            files
                .iter()
                .filter_map(|f| crate::midi::import_bytes(f).ok().map(|(d, _)| d)),
        );
        out
    }

    fn write(
        doc: &PlumDocument,
        plan: &ExportPlan,
        threads: usize,
        window: u64,
    ) -> Result<(Vec<u8>, WriteStats), ExportError> {
        let mut cur = io::Cursor::new(Vec::new());
        let stats = write_document_threads(
            doc,
            plan,
            &mut cur,
            ExportControl::default(),
            threads,
            window,
        )?;
        Ok((cur.into_inner(), stats))
    }

    #[test]
    fn workers_write_the_same_bytes_and_the_digest_check_reports_the_same() {
        let mut written = 0;
        for doc in docs() {
            let plan = plan_export(&doc);
            let one = write(&doc, &plan, 0, super::super::workers::WINDOW);
            for (threads, window) in [(1, super::super::workers::WINDOW), (2, 1), (3, 40)] {
                assert_eq!(write(&doc, &plan, threads, window), one);
            }
            let Ok((bytes, stats)) = one else { continue };
            written += 1;
            let full = validate_exported_bytes(&doc, &plan, &bytes);
            assert_eq!(
                validate_written_bytes(&doc, &plan, &bytes, &stats.chunk_digests),
                full
            );
            let Ok(report) = full else { continue };
            for (threads, window) in [(0, 0), (2, 1), (3, 64)] {
                let fast = check_digests_threads(
                    &doc,
                    &plan,
                    &bytes[..],
                    Some(bytes.len() as u64),
                    &stats.chunk_digests,
                    threads,
                    window,
                );
                assert_eq!(fast.as_ref(), Some(&report), "{threads} worker(s)");
            }
            // A damaged copy fails with the full validation's message.
            for at in [bytes.len() / 3, bytes.len() / 2, bytes.len() - 5] {
                let mut bad = bytes.clone();
                bad[at] ^= 0x01;
                if let Err(e) = validate_exported_bytes(&doc, &plan, &bad) {
                    assert_eq!(
                        validate_written_bytes(&doc, &plan, &bad, &stats.chunk_digests),
                        Err(e)
                    );
                }
            }
        }
        assert!(written > 100, "the cases exercise exports ({written})");
    }
}
