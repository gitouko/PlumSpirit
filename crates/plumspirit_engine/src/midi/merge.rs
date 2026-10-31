//! Importing a MIDI file **into one track** of an open project (community merges).
//!
//! The source file is imported on its own first (the ordinary importer, including
//! PlumSpirit structure records), then mapped into the destination:
//!
//! * [`MergeMode::AsSubtracks`] — every source track (with content) becomes a new
//!   subtrack of the destination track, keeping its notes, events, name and
//!   subchannels (one per MIDI channel it uses).
//! * [`MergeMode::AsSubchannels`] — every source track is flattened into the
//!   destination track; each distinct (source track, MIDI channel) gets its own new
//!   subchannel, so "Track A / Ch 1" and "Track B / Ch 1" stay apart while both
//!   notes keep MIDI channel 1.
//!
//! Timing: source ticks are converted to the project's PPQN with exact integer
//! arithmetic, `round_half_up(tick × target / source)`, start and end separately
//! (so notes that touched still touch), then shifted by the insertion position.
//! SMPTE timing on only one side, different SMPTE rates or Format 2 sequences are
//! refused with an explanation rather than converted incorrectly.
//!
//! Global events follow a [`ConductorPolicy`]; the project's Conductor is never
//! replaced silently. Everything is prepared first (nothing in the project
//! changes) and applied as **one** undo step through [`Operation`]; cancelling
//! simply drops the prepared result.

use crate::editor::PreparedEdit;
use crate::editor::controllers::EventsEdit;
use crate::history::Operation;
use crate::midi::group_digits;
use crate::model::conductor::{ConductorEdit, ConductorScope, Marker, TempoPoint, TimeSignature};
use crate::model::document::PlumDocument;
use crate::model::events::{ArenaRange, EventStore, RawEvent, RawKind, display_text, meta};
use crate::model::note::{NoteRecord, SubchannelId};
use crate::model::note_store::NoteStore;
use crate::model::patch::ListPatch;
use crate::model::timing::TimingKind;
use crate::model::track::{Subchannel, Track, TrackId};

/// How source tracks are placed into the destination track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MergeMode {
    /// Source tracks → subtracks of the destination.
    #[default]
    AsSubtracks,
    /// Source tracks / channels → subchannels inside the destination.
    AsSubchannels,
}

impl MergeMode {
    pub fn label(self) -> &'static str {
        match self {
            MergeMode::AsSubtracks => "Source Tracks as Subtracks",
            MergeMode::AsSubchannels => "Source Tracks and Channels as Subchannels",
        }
    }
}

/// What happens to the source file's tempo, time signatures and markers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ConductorPolicy {
    /// The project's tempo and meter stay; the source's markers are added.
    #[default]
    Keep,
    /// Source tempo points, signatures and markers are added; where the project
    /// already has a tempo or signature at the same tick, the project's wins
    /// (listed as a conflict).
    Merge,
    /// From the insertion position on, the project's tempo, signatures and
    /// markers are replaced by the source's.
    Replace,
    /// Nothing from the source's Conductor is used.
    Ignore,
}

impl ConductorPolicy {
    pub const ALL: [ConductorPolicy; 4] = [
        ConductorPolicy::Keep,
        ConductorPolicy::Merge,
        ConductorPolicy::Replace,
        ConductorPolicy::Ignore,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConductorPolicy::Keep => "Keep Current Conductor",
            ConductorPolicy::Merge => "Merge Source Global Events",
            ConductorPolicy::Replace => "Replace Conductor With Source",
            ConductorPolicy::Ignore => "Ignore Source Global Events",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            ConductorPolicy::Keep => {
                "The project's tempo and time signatures stay; the source's markers are added."
            }
            ConductorPolicy::Merge => {
                "The source's tempo changes, time signatures and markers are added; where both have one at the same position, the project's is kept."
            }
            ConductorPolicy::Replace => {
                "From the insertion position on, the project's tempo, time signatures and markers are replaced by the source's."
            }
            ConductorPolicy::Ignore => {
                "Nothing from the source's tempo, time signatures or markers is used."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeOptions {
    pub mode: MergeMode,
    pub policy: ConductorPolicy,
    /// Destination tick where the source's tick 0 lands.
    pub at: u64,
}

/// What the import will do (shown before Apply).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergeSummary {
    pub mode: MergeMode,
    pub source_tracks: usize,
    pub empty_tracks_skipped: usize,
    /// Distinct MIDI channels used by the source's notes.
    pub source_channels: usize,
    pub source_notes: usize,
    pub source_events: usize,
    pub new_subtracks: usize,
    pub new_subchannels: usize,
    /// `(source PPQN, project PPQN)` when they differ.
    pub ppqn: Option<(u32, u32)>,
    /// Note starts or ends that are not on an exact tick at the project's PPQN.
    pub notes_rounded: u64,
    pub events_rounded: u64,
    /// Notes that became zero ticks long.
    pub notes_collapsed: u64,
    /// Largest rounding, in project ticks (≤ 0.5).
    pub max_rounding_ticks: f64,
    pub tempo_added: usize,
    pub signatures_added: usize,
    pub markers_added: usize,
    pub entries_replaced: usize,
    /// Same-tick conflicts (Merge policy), as readable lines.
    pub conflicts: Vec<String>,
    pub conflict_count: usize,
}

impl MergeSummary {
    /// Plain-language lines for the preview dialog.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!(
            "Source MIDI: {} track(s), {} MIDI channel(s) used, {} note(s), {} other event(s).",
            group_digits(self.source_tracks as u64),
            self.source_channels,
            group_digits(self.source_notes as u64),
            group_digits(self.source_events as u64)
        )];
        out.push(match self.mode {
            MergeMode::AsSubtracks => format!(
                "Result: {} new subtrack(s) under the chosen track, with {} subchannel(s) in total.",
                group_digits(self.new_subtracks as u64),
                group_digits(self.new_subchannels as u64)
            ),
            MergeMode::AsSubchannels => format!(
                "Result: {} new subchannel(s) in the chosen track (one per source track and MIDI channel); every note keeps its MIDI channel.",
                group_digits(self.new_subchannels as u64)
            ),
        });
        if self.empty_tracks_skipped > 0 {
            out.push(format!(
                "{} source track(s) without notes or events are skipped.",
                self.empty_tracks_skipped
            ));
        }
        if let Some((s, d)) = self.ppqn {
            if self.notes_rounded == 0 && self.events_rounded == 0 {
                out.push(format!(
                    "The source uses {} PPQN and the project {} PPQN; every position converts exactly.",
                    group_digits(u64::from(s)),
                    group_digits(u64::from(d))
                ));
            } else {
                out.push(format!(
                    "The source uses {} PPQN and the project {} PPQN: {} note(s) and {} event(s) are rounded to the nearest project tick (at most {:.3} tick).",
                    group_digits(u64::from(s)),
                    group_digits(u64::from(d)),
                    group_digits(self.notes_rounded),
                    group_digits(self.events_rounded),
                    self.max_rounding_ticks
                ));
            }
            if self.notes_collapsed > 0 {
                out.push(format!(
                    "{} very short note(s) become zero ticks long at the project's resolution.",
                    group_digits(self.notes_collapsed)
                ));
            }
        }
        let added = self.tempo_added + self.signatures_added + self.markers_added;
        if added > 0 || self.entries_replaced > 0 {
            out.push(format!(
                "Conductor: {} tempo change(s), {} time signature(s) and {} marker(s) added{}.",
                self.tempo_added,
                self.signatures_added,
                self.markers_added,
                if self.entries_replaced > 0 {
                    format!(", {} project entr(ies) replaced", self.entries_replaced)
                } else {
                    String::new()
                }
            ));
        }
        if self.conflict_count > 0 {
            out.push(format!(
                "{} same-position conflict(s); the project's entry is kept:",
                self.conflict_count
            ));
            out.extend(self.conflicts.iter().map(|c| format!("  {c}")));
        }
        out
    }
}

/// A fully prepared merge: nothing in the project has changed yet.
#[derive(Debug)]
pub struct PreparedMerge {
    pub summary: MergeSummary,
    /// Apply with `History::apply` (one undo step).
    pub operation: Operation,
}

/// Exact tick conversion `at + round_half_up(t × num / den)`.
#[derive(Clone, Copy, Debug)]
struct TickMap {
    num: u64,
    den: u64,
    at: u64,
}

impl TickMap {
    fn new(src_ppqn: u64, dst_ppqn: u64, at: u64) -> Self {
        let g = gcd(src_ppqn, dst_ppqn).max(1);
        TickMap {
            num: dst_ppqn / g,
            den: src_ppqn / g,
            at,
        }
    }

    #[inline]
    fn exact(&self) -> bool {
        self.num == self.den
    }

    /// Converted tick and whether it was rounded.
    #[inline]
    fn map(&self, t: u64) -> Option<(u64, bool)> {
        if self.exact() {
            return self.at.checked_add(t).map(|v| (v, false));
        }
        let p = u128::from(t) * u128::from(self.num);
        let q = u128::from(self.den);
        let v = (2 * p + q) / (2 * q);
        let rounded = p % q != 0;
        let v = u64::try_from(v).ok()?;
        self.at.checked_add(v).map(|v| (v, rounded))
    }

    /// Rounding error of `t`, in destination ticks.
    fn error(&self, t: u64) -> f64 {
        if self.exact() {
            return 0.0;
        }
        let p = u128::from(t) * u128::from(self.num);
        let q = u128::from(self.den);
        let r = (p % q) as f64 / q as f64;
        if r >= 0.5 { 1.0 - r } else { r }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn overflow() -> String {
    "Some of the imported notes would land beyond the largest possible position. Choose an earlier insertion position.".into()
}

/// Checks that the two timings can be combined and returns the tick map.
fn timing(
    dest: &PlumDocument,
    source: &PlumDocument,
    at: u64,
) -> Result<(TickMap, Option<(u32, u32)>), String> {
    if dest.conductor().scope() == ConductorScope::Sequences
        || source.conductor().scope() == ConductorScope::Sequences
    {
        return Err("MIDI Format 2 files hold independent sequences, each with its own tempo, so they cannot be merged into one track. Open the file as its own project instead.".into());
    }
    match (dest.division().kind(), source.division().kind()) {
        (TimingKind::Ppqn(d), TimingKind::Ppqn(s)) => {
            if d == 0 || s == 0 {
                return Err("A timing resolution of 0 ticks per quarter note cannot be converted.".into());
            }
            let map = TickMap::new(u64::from(s), u64::from(d), at);
            Ok((map, (s != d).then_some((s, d))))
        }
        (a @ TimingKind::Smpte { .. }, b @ TimingKind::Smpte { .. }) if a == b => {
            Ok((TickMap::new(1, 1, at), None))
        }
        (TimingKind::Smpte { .. }, _) | (_, TimingKind::Smpte { .. }) => Err(
            "One of the two uses SMPTE (frame-based) timing and the other musical (PPQN) timing, or two different frame rates. Converting between them needs the exact time of every tick, which PlumSpirit does not do for merges yet, so nothing was imported. Open the file as its own project instead."
                .into(),
        ),
    }
}

/// Prepares importing `source` into the track at `dest_index` of `dest`.
/// Nothing changes until the returned operation is applied.
pub fn prepare_merge(
    dest: &PlumDocument,
    dest_index: usize,
    source: &PlumDocument,
    options: MergeOptions,
) -> Result<PreparedMerge, String> {
    let dt = dest
        .track(dest_index)
        .ok_or("The destination track no longer exists.")?;
    let (map, ppqn) = timing(dest, source, options.at)?;
    let mut summary = MergeSummary {
        mode: options.mode,
        source_tracks: source.track_count(),
        ppqn,
        ..MergeSummary::default()
    };
    let mut channels = [false; 16];
    for t in source.tracks() {
        summary.source_notes += t.notes.len();
        summary.source_events += t.events.len();
        for (ch, _) in t.notes.channel_sub_pairs() {
            channels[usize::from(ch)] = true;
        }
    }
    summary.source_channels = channels.iter().filter(|c| **c).count();
    let conductor = conductor_edit(dest, source, &map, options, &mut summary)?;
    let mut ops = Vec::new();
    match options.mode {
        MergeMode::AsSubtracks => {
            let parent = dt.id();
            let mut next = dest.next_track_id();
            let mut block = Vec::new();
            for st in source.tracks() {
                if st.notes.is_empty() && st.events.is_empty() {
                    summary.empty_tracks_skipped += 1;
                    continue;
                }
                let id = TrackId(next);
                next = next.checked_add(1).ok_or("Too many tracks.")?;
                let t = convert_track(st, id, parent, &map, &mut summary)?;
                summary.new_subchannels += t.subchannels().len();
                block.push(t);
            }
            summary.new_subtracks = block.len();
            if dest.track_count() + block.len() > usize::from(u16::MAX) {
                return Err("The project would have more than 65,535 tracks and subtracks (the limit of a MIDI file).".into());
            }
            if !block.is_empty() {
                ops.push(Operation::InsertTracks {
                    index: dest.subtree_end(dest_index),
                    tracks: block,
                    label: "Import MIDI Into Track",
                });
            }
        }
        MergeMode::AsSubchannels => {
            let (subs, records, events) = flatten(dt, source, &map, &mut summary)?;
            summary.new_subchannels = subs.len();
            if !subs.is_empty() {
                ops.push(Operation::AddSubchannels {
                    track: dest_index,
                    subs,
                });
            }
            if !records.is_empty() {
                ops.push(Operation::Prepared {
                    edit: PreparedEdit::InsertSelected {
                        host: crate::model::ContentRef::Track(dt.id()),
                        base_revision: dt.notes.revision(),
                        records,
                        subs_track: dt.id(),
                        new_subs: Vec::new(),
                    },
                    label: "Import MIDI Into Track",
                });
            }
            if let Some(e) = events {
                ops.push(Operation::Events {
                    edit: e,
                    label: "Import MIDI Into Track",
                });
            }
        }
    }
    if let Some(c) = conductor {
        ops.push(Operation::Conductor {
            edit: c,
            label: "Import MIDI Into Track",
        });
    }
    Ok(PreparedMerge {
        summary,
        operation: Operation::Group {
            ops,
            label: "Import MIDI Into Track",
        },
    })
}

/// Notes of `st` with converted ticks, in stored order, in batches.
fn convert_notes(
    st: &NoteStore,
    map: &TickMap,
    summary: &mut MergeSummary,
    mut sink: impl FnMut(&[NoteRecord]) -> Result<(), String>,
) -> Result<(), String> {
    const BATCH: usize = 64 * 1024;
    let mut batch: Vec<NoteRecord> = Vec::with_capacity(BATCH.min(st.len()));
    for v in st.iter() {
        let (s, rs) = map.map(v.start_tick).ok_or_else(overflow)?;
        let (e, re) = map.map(v.end_tick()).ok_or_else(overflow)?;
        if rs || re {
            summary.notes_rounded += 1;
            summary.max_rounding_ticks = summary
                .max_rounding_ticks
                .max(map.error(v.start_tick))
                .max(map.error(v.end_tick()));
        }
        if e == s && v.duration_ticks > 0 {
            summary.notes_collapsed += 1;
        }
        batch.push(NoteRecord {
            start_tick: s,
            duration_ticks: e - s,
            ..v.record()
        });
        if batch.len() == BATCH {
            sink(&batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        sink(&batch)?;
    }
    Ok(())
}

fn convert_events(
    ev: &EventStore,
    map: &TickMap,
    summary: &mut MergeSummary,
) -> Result<EventStore, String> {
    let mut out = ev.clone();
    let mut err = None;
    out.map_ticks(|t| match map.map(t) {
        Some((v, r)) => {
            if r {
                summary.events_rounded += 1;
            }
            v
        }
        None => {
            err = Some(overflow());
            t
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// A source track as a new subtrack of `parent` (ticks converted; the notes are
/// shared, not copied, when no conversion is needed).
fn convert_track(
    st: &Track,
    id: TrackId,
    parent: TrackId,
    map: &TickMap,
    summary: &mut MergeSummary,
) -> Result<Track, String> {
    let notes = if map.exact() && map.at == 0 {
        let mut n = st.notes.clone();
        n.clear_selection();
        n
    } else {
        let mut n = NoteStore::new();
        let mut left = st.notes.len();
        convert_notes(&st.notes, map, summary, |b| {
            left -= b.len();
            n.append_sorted(b, left + b.len())
                .map_err(|e| e.to_string())
        })?;
        n
    };
    let events = if map.exact() && map.at == 0 {
        st.events.clone()
    } else {
        convert_events(&st.events, map, summary)?
    };
    let eot = match st.end_of_track_tick {
        Some(t) => Some(map.map(t).ok_or_else(overflow)?.0),
        None => None,
    };
    let mut t = Track::from_parts_exact(id, notes, events, eot);
    t.set_parent(Some(parent));
    t.set_subchannels(st.subchannels().clone());
    Ok(t)
}

type Flattened = (Vec<Subchannel>, Vec<NoteRecord>, Option<EventsEdit>);

/// All source notes as new subchannels of `dt` (one per source track and MIDI
/// channel), plus the source events merged into `dt`'s events.
fn flatten(
    dt: &Track,
    source: &PlumDocument,
    map: &TickMap,
    summary: &mut MergeSummary,
) -> Result<Flattened, String> {
    // (source track, channel) → new subchannel id, in source order.
    let mut keys: Vec<(usize, u8)> = Vec::new();
    for (i, t) in source.tracks().iter().enumerate() {
        let mut used = [false; 16];
        for (ch, _) in t.notes.channel_sub_pairs() {
            used[usize::from(ch)] = true;
        }
        keys.extend((0..16u8).filter(|c| used[usize::from(*c)]).map(|c| (i, c)));
    }
    let ids = dt
        .subchannels()
        .free_ids(keys.len())
        .ok_or("The track cannot hold that many more subchannels.")?;
    let mut subs = Vec::with_capacity(keys.len());
    for (&(i, ch), &id) in keys.iter().zip(&ids) {
        let st = &source.tracks()[i];
        let name = st
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Track {}", i + 1));
        let mut sc = Subchannel::new(id, ch);
        let label = format!("{name} · Ch {}", u16::from(ch) + 1);
        sc.name = Some(
            label
                .chars()
                .take(crate::model::track::MAX_SUBCHANNEL_NAME_CHARS)
                .collect(),
        );
        subs.push(sc);
    }
    let sub_of = |i: usize, ch: u8| -> SubchannelId {
        let k = keys.partition_point(|&(a, b)| (a, b) < (i, ch));
        ids[k]
    };
    let mut records: Vec<NoteRecord> = Vec::new();
    records
        .try_reserve_exact(summary.source_notes)
        .map_err(|_| "Not enough memory to import that many notes into one track.".to_string())?;
    for (i, st) in source.tracks().iter().enumerate() {
        convert_notes(&st.notes, map, summary, |b| {
            records.extend(b.iter().map(|r| NoteRecord {
                sub: sub_of(i, r.attrs.channel()),
                ..*r
            }));
            Ok(())
        })?;
    }
    // Stable: at equal starts, source track order is kept.
    records.sort_by_key(|r| r.start_tick);
    for t in source.tracks() {
        if t.notes.is_empty() && t.events.is_empty() {
            summary.empty_tracks_skipped += 1;
        }
    }
    let events = merge_events(dt, source, map, summary)?;
    Ok((subs, records, events))
}

/// The source tracks' events (except their Track Names, which become subchannel
/// names) merged after the destination's events at each tick.
fn merge_events(
    dt: &Track,
    source: &PlumDocument,
    map: &TickMap,
    summary: &mut MergeSummary,
) -> Result<Option<EventsEdit>, String> {
    let arena_at = dt.events.arena_len();
    let mut append: Vec<u8> = Vec::new();
    let mut add: Vec<RawEvent> = Vec::new();
    for st in source.tracks() {
        for e in st.events.events() {
            if let RawKind::Meta {
                meta_type: meta::TRACK_NAME,
                ..
            } = e.kind
            {
                continue;
            }
            let (tick, r) = map.map(e.tick).ok_or_else(overflow)?;
            if r {
                summary.events_rounded += 1;
            }
            let mut copy = |p: ArenaRange| -> Result<ArenaRange, String> {
                let bytes = st.events.payload(p);
                let offset = u32::try_from(arena_at + append.len()).map_err(|_| {
                    "The track's text and SysEx data would exceed 4 GB.".to_string()
                })?;
                append.extend_from_slice(bytes);
                Ok(ArenaRange { offset, len: p.len })
            };
            let kind = match e.kind {
                RawKind::Meta { meta_type, payload } => RawKind::Meta {
                    meta_type,
                    payload: copy(payload)?,
                },
                RawKind::SysEx { payload } => RawKind::SysEx {
                    payload: copy(payload)?,
                },
                RawKind::SysExEscape { payload } => RawKind::SysExEscape {
                    payload: copy(payload)?,
                },
                k => k,
            };
            add.push(RawEvent {
                tick,
                anchor: 0,
                kind,
            });
        }
    }
    if add.is_empty() {
        return Ok(None);
    }
    add.sort_by_key(|e| e.tick);
    let arena_at = u32::try_from(arena_at)
        .map_err(|_| "The track's text and SysEx data would exceed 4 GB.".to_string())?;
    let old = dt.events.events();
    let mut patch = ListPatch::new();
    // New events go after the track's own events at the same tick.
    for (k, e) in add.into_iter().enumerate() {
        let before = old.partition_point(|o| o.tick <= e.tick);
        patch.insert(before + k, e);
    }
    Ok(Some(EventsEdit {
        host: crate::model::ContentRef::Track(dt.id()),
        base_revision: dt.events_revision(),
        patch,
        append,
        append_at: arena_at,
    }))
}

/// The Conductor change for `policy` (`None`: nothing changes).
fn conductor_edit(
    dest: &PlumDocument,
    source: &PlumDocument,
    map: &TickMap,
    options: MergeOptions,
    summary: &mut MergeSummary,
) -> Result<Option<ConductorEdit>, String> {
    let c = dest.conductor();
    let s = source.conductor();
    let policy = options.policy;
    if policy == ConductorPolicy::Ignore {
        return Ok(None);
    }
    let timing = c.timing_editable() && s.timing_editable();
    let mut next_id = c.next_id();
    let mut id = || -> Result<u32, String> {
        let v = next_id;
        next_id = next_id
            .checked_add(1)
            .ok_or("The Conductor has too many entries.")?;
        Ok(v)
    };
    let conv = |t: u64| map.map(t).map(|v| v.0).ok_or_else(overflow);
    // Final lists.
    let mut tempo: Vec<TempoPoint> = c.tempo_points().to_vec();
    let mut meter: Vec<TimeSignature> = c.time_signatures().to_vec();
    let mut markers: Vec<Marker> = c.markers().to_vec();
    let at = options.at;
    if timing && matches!(policy, ConductorPolicy::Merge | ConductorPolicy::Replace) {
        if policy == ConductorPolicy::Replace {
            let before = tempo.len() + meter.len();
            tempo.retain(|p| p.tick < at || p.tick == 0);
            meter.retain(|m| m.tick < at || m.tick == 0);
            summary.entries_replaced += before - tempo.len() - meter.len();
        }
        for p in s.tempo_points() {
            if p.implicit && p.tick == 0 && policy == ConductorPolicy::Merge {
                continue; // an assumed 120 BPM is not the source's tempo
            }
            let tick = conv(p.tick)?;
            match tempo.binary_search_by_key(&tick, |q| q.tick) {
                Ok(i) => {
                    if policy == ConductorPolicy::Replace {
                        tempo[i].value = p.value;
                        tempo[i].interp = p.interp;
                        tempo[i].implicit = false;
                        summary.entries_replaced += 1;
                    } else if crate::model::conductor::tempo_bpm_f64(tempo[i].value)
                        != crate::model::conductor::tempo_bpm_f64(p.value)
                    {
                        summary.conflict_count += 1;
                        if summary.conflicts.len() < 8 {
                            summary.conflicts.push(format!(
                                "tempo at tick {}: project {} BPM, source {} BPM",
                                group_digits(tick),
                                crate::model::conductor::format_tempo(tempo[i].value),
                                crate::model::conductor::format_tempo(p.value)
                            ));
                        }
                    }
                }
                Err(i) => {
                    tempo.insert(
                        i,
                        TempoPoint {
                            id: id()?,
                            tick,
                            origin: None,
                            implicit: false,
                            ..*p
                        },
                    );
                    summary.tempo_added += 1;
                }
            }
        }
        for m in s.time_signatures() {
            if m.implicit && m.tick == 0 && policy == ConductorPolicy::Merge {
                continue;
            }
            let tick = conv(m.tick)?;
            match meter.binary_search_by_key(&tick, |q| q.tick) {
                Ok(i) => {
                    let same =
                        meter[i].numerator == m.numerator && meter[i].denom_pow == m.denom_pow;
                    if policy == ConductorPolicy::Replace {
                        meter[i] = TimeSignature {
                            id: meter[i].id,
                            tick,
                            origin: None,
                            implicit: false,
                            ..*m
                        };
                        summary.entries_replaced += 1;
                    } else if !same {
                        summary.conflict_count += 1;
                        if summary.conflicts.len() < 8 {
                            summary.conflicts.push(format!(
                                "time signature at tick {}: project {}, source {}",
                                group_digits(tick),
                                meter[i].label(),
                                m.label()
                            ));
                        }
                    }
                }
                Err(i) => {
                    meter.insert(
                        i,
                        TimeSignature {
                            id: id()?,
                            tick,
                            origin: None,
                            implicit: false,
                            ..*m
                        },
                    );
                    summary.signatures_added += 1;
                }
            }
        }
    }
    if c.markers_editable() && policy != ConductorPolicy::Ignore {
        if policy == ConductorPolicy::Replace {
            let before = markers.len();
            markers.retain(|m| m.start < at);
            summary.entries_replaced += before - markers.len();
        }
        for m in s.markers() {
            let start = conv(m.start)?;
            let end = match m.end {
                Some(e) => Some(conv(e)?.max(start + 1)),
                None => None,
            };
            markers.push(Marker {
                id: id()?,
                start,
                end,
                origin: None,
                ..m.clone()
            });
            summary.markers_added += 1;
        }
        markers.sort_by_key(|m| (m.start, m.id));
    }
    let edit = ConductorEdit {
        tempo: diff(c.tempo_points(), &tempo, |a| a.id),
        meter: diff(c.time_signatures(), &meter, |a| a.id),
        markers: diff(c.markers(), &markers, |a| a.id),
        master: ListPatch::new(),
        next_id: (c.next_id(), next_id),
        base_revision: c.revision(),
    };
    if edit.is_empty() {
        return Ok(None);
    }
    // Validate on a copy: an invalid result must be refused before anything
    // changes.
    let mut probe = c.clone();
    probe.apply(&edit).map_err(|e| {
        format!("The source's tempo map cannot be combined with the project's: {e}")
    })?;
    Ok(Some(edit))
}

/// A patch turning `old` into `new`; items are matched by id (changed items are
/// removed and re-inserted). O(n) with a hash map.
fn diff<T: Clone + PartialEq>(old: &[T], new: &[T], id: impl Fn(&T) -> u32) -> ListPatch<T> {
    use std::collections::HashMap;
    let by_id_old: HashMap<u32, &T> = old.iter().map(|x| (id(x), x)).collect();
    let by_id_new: HashMap<u32, &T> = new.iter().map(|x| (id(x), x)).collect();
    let mut p = ListPatch::new();
    for (i, x) in old.iter().enumerate() {
        if by_id_new.get(&id(x)) != Some(&x) {
            p.remove(i, x.clone());
        }
    }
    for (i, y) in new.iter().enumerate() {
        if by_id_old.get(&id(y)) != Some(&y) {
            p.insert(i, y.clone());
        }
    }
    p
}

/// Readable name of a source track for summaries.
pub fn source_track_label(t: &Track, index: usize) -> String {
    t.events
        .track_name()
        .map(display_text)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Track {}", index + 1))
}
