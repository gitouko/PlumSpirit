//! Running a kernel and committing its result through the undo history.
//!
//! Preview and Apply share everything here: [`run_kernel`] produces the result
//! the preview draws; [`commit`] writes exactly that result (when the source is
//! unchanged) as one undo step: per track, the notes in scope are replaced by the
//! track's results (`ReplaceSelected`, compact undo records), grouped all or
//! nothing.

use super::output::Output;
use super::preflight::{self, Estimate, Limits, Verdict};
use super::{Boundary, Kernel, OutStats, Source, SourceTrack, TNote, TransformError};
use crate::editor::PreparedEdit;
use crate::history::{History, HistoryError, Operation};
use crate::model::PlumDocument;
use crate::model::note::NoteRecord;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// The result of one kernel run (what the preview shows and Apply commits).
#[derive(Clone, Debug)]
pub struct TransformRun {
    /// Output notes sorted by (track slot, start, pitch, …).
    pub notes: Vec<TNote>,
    pub stats: OutStats,
    pub tracks: Vec<SourceTrack>,
    pub source_count: usize,
    pub estimate: Estimate,
    /// Preflight warning (Apply allowed).
    pub warning: Option<String>,
    /// The kernel's own message (chord names, …).
    pub message: Option<String>,
    /// Controller points to write (sorted by track, lane, channel, tick; one
    /// per tick).
    pub events: Vec<super::output::TEvent>,
    /// The new note-group table, when the tool changes groups.
    pub groups: Option<crate::model::GroupTable>,
    /// Notes for a new subtrack of the slot's track (sorted by start).
    pub new_subtrack: Option<(u16, Vec<TNote>)>,
    /// The pitch range the boundary policy kept the result in (0–127, or
    /// 0–255 in a 256-Key Mode project).
    pub pitch_range: (u8, u8),
    /// v0.9 V09-S39: notes outside the source to delete in the same undo
    /// step ([`super::Output::erase`]), sorted by (track slot, start, …).
    pub erase: Vec<TNote>,
    /// B2-F16: [`Source::shift`] of the run's source.
    pub shift: i128,
}

impl TransformRun {
    /// Number of output notes that differ from the source (by multiset) — a
    /// cheap "changed" figure for the summary line.
    pub fn summary(&self) -> String {
        let g = crate::midi::group_digits;
        let mut s = format!(
            "{} note(s) in, {} out.",
            g(self.source_count as u64),
            g(self.notes.len() as u64)
        );
        if let Some(m) = &self.message {
            s.push(' ');
            s.push_str(m);
        }
        let st = &self.stats;
        let (lo, hi) = self.pitch_range;
        if st.pitch_dropped > 0 {
            s.push_str(&format!(
                " {} outside {lo}–{hi} removed.",
                g(st.pitch_dropped)
            ));
        }
        if st.pitch_changed > 0 {
            s.push_str(&format!(" {} moved into {lo}–{hi}.", g(st.pitch_changed)));
        }
        if st.tick_clamped > 0 {
            s.push_str(&format!(" {} clamped to tick 0.", g(st.tick_clamped)));
        }
        if st.tick_dropped > 0 {
            s.push_str(&format!(" {} before tick 0 removed.", g(st.tick_dropped)));
        }
        if st.shifted_by > 0 {
            s.push_str(&format!(
                " Everything shifted by {} ticks.",
                g(st.shifted_by)
            ));
        }
        if st.len_fixed > 0 {
            s.push_str(&format!(" {} lengthened to 1 tick.", g(st.len_fixed)));
        }
        if st.len_dropped > 0 {
            s.push_str(&format!(" {} zero-length removed.", g(st.len_dropped)));
        }
        if st.collapsed > 0 {
            s.push_str(&format!(
                " {} duplicate(s) the shrink created removed.",
                g(st.collapsed)
            ));
        }
        if st.overlaps_removed > 0 || st.overlaps_trimmed > 0 {
            s.push_str(&format!(
                " Overlaps: {} removed, {} shortened.",
                g(st.overlaps_removed),
                g(st.overlaps_trimmed)
            ));
        }
        if st.overflow_dropped > 0 {
            s.push_str(&format!(
                " {} beyond the last tick removed.",
                g(st.overflow_dropped)
            ));
        }
        if let Some(w) = &self.warning {
            s.push(' ');
            s.push_str(w);
        }
        s
    }

    /// The output notes of track slot `slot` (a sorted sub-slice).
    pub fn track_notes(&self, slot: u16) -> &[TNote] {
        let a = self.notes.partition_point(|n| n.track < slot);
        let b = self.notes.partition_point(|n| n.track <= slot);
        &self.notes[a..b]
    }
}

/// Runs `kernel` on `src` after the preflight; nothing is allocated for an
/// operation the preflight refuses.
pub fn run_kernel(
    kernel: &dyn Kernel,
    src: &Source,
    boundary: Boundary,
    limits: &Limits,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<TransformRun, TransformError> {
    let estimate = kernel.estimate_source(src);
    let warning = match preflight::check(&estimate, limits) {
        Verdict::Refuse(m) => return Err(TransformError::TooLarge(m)),
        Verdict::Warn(w) => Some(w),
        Verdict::Ok => None,
    };
    let boundary = boundary.within(src.keys);
    let mut out = Output::new(boundary, limits.cap());
    if let Some(c) = cancel {
        out = out.with_cancel(c);
    }
    out.reserve(estimate.notes.min(1 << 22) as usize)?;
    kernel.run(src, &mut out)?;
    out.check()?;
    // Locked group notes stay as they are (a replacing commit re-inserts them).
    for n in &src.locked {
        if src
            .tracks
            .get(usize::from(n.track))
            .is_some_and(|t| !t.insert_only)
        {
            out.notes.push(*n);
        }
    }
    let mut events = std::mem::take(&mut out.events);
    let mut groups = out.groups.take();
    let new_parent = out.new_subtrack_parent;
    let mut erase = std::mem::take(&mut out.erase);
    let (mut notes, mut stats, message) = out.finish_with_message()?;
    // v0.9 §231: the platform's Remove Note Overlaps runs on the finished,
    // sorted result (locked group notes are never touched).
    if let Some(policy) = kernel.overlaps() {
        let r = super::overlap::apply(&mut notes, &src.notes, &src.locked, policy);
        stats.overlaps_removed = r.removed;
        stats.overlaps_trimmed = r.trimmed;
    }
    // Notes for a new subtrack are kept apart (they sort last: slot u16::MAX).
    let new_subtrack = match new_parent {
        Some(p) if usize::from(p) < src.tracks.len() => {
            let at = notes.partition_point(|n| n.track < super::output::NEW_SUBTRACK);
            let mut v = notes.split_off(at);
            for n in &mut v {
                n.track = p;
            }
            Some((p, v))
        }
        _ => None,
    };
    register_new_groups(&mut notes, &mut groups, &src.groups);
    // One point per (track, lane, channel, tick): the last emitted wins.
    events.reverse();
    events.sort_by_key(|e| (e.track, e.lane, e.ch, e.tick));
    events.dedup_by_key(|e| (e.track, e.lane, e.ch, e.tick));
    if let Some(e) = events
        .iter()
        .find(|e| usize::from(e.track) >= src.tracks.len())
    {
        return Err(TransformError::Invalid(format!(
            "Internal error: a controller point was sent to unknown track slot {}.",
            e.track
        )));
    }
    if let Some(g) = &groups {
        g.validate().map_err(|m| {
            TransformError::Invalid(format!("Internal error: invalid note groups ({m})."))
        })?;
    }
    erase.sort_unstable_by_key(|n| {
        (
            n.track, n.start, n.pitch, n.dur, n.sub, n.vel, n.ch, n.rel, n.group, n.flags,
        )
    });
    erase.dedup();
    if let Some(n) = erase
        .iter()
        .find(|n| usize::from(n.track) >= src.tracks.len())
    {
        return Err(TransformError::Invalid(format!(
            "Internal error: a note to remove was in unknown track slot {}.",
            n.track
        )));
    }
    // Results must stay in tracks that exist in the source.
    if let Some(n) = notes
        .iter()
        .find(|n| usize::from(n.track) >= src.tracks.len())
    {
        return Err(TransformError::Invalid(format!(
            "Internal error: a result was sent to unknown track slot {}.",
            n.track
        )));
    }
    Ok(TransformRun {
        notes,
        stats,
        tracks: src.tracks.clone(),
        source_count: src.notes.len(),
        estimate,
        warning,
        message,
        events,
        groups,
        new_subtrack,
        pitch_range: (boundary.lo, boundary.hi.max(boundary.lo)),
        erase,
        shift: src.shift,
    })
}

/// Group ids that kernels generate for their own output (Arpeggiator / Chopper
/// / Flam "Group notes": ids hashed from the source note) are registered as
/// fresh groups of the table, numbered in output order (deterministic), so
/// they are saved and select as groups.
fn register_new_groups(
    notes: &mut [TNote],
    groups: &mut Option<crate::model::GroupTable>,
    base: &crate::model::GroupTable,
) {
    use std::collections::HashMap;
    let known = |t: &Option<crate::model::GroupTable>, id: u32| match t {
        Some(t) => t.get(id).is_some(),
        None => base.get(id).is_some(),
    };
    if notes.iter().all(|n| n.group == 0 || known(groups, n.group)) {
        return;
    }
    let mut table = groups.take().unwrap_or_else(|| base.clone());
    let original = table.clone();
    let mut map: HashMap<u32, u32> = HashMap::new();
    for n in notes.iter_mut() {
        if n.group == 0 {
            continue;
        }
        if let Some(&m) = map.get(&n.group) {
            n.group = m;
            continue;
        }
        if original.get(n.group).is_some() {
            continue;
        }
        let m = table.add(crate::model::GroupInfo::default());
        map.insert(n.group, m);
        n.group = m;
    }
    *groups = Some(table);
}

/// Per-track records of a run (sorted by start), validated against each
/// track's subchannel table.
pub type Prepared = Vec<(SourceTrack, Vec<NoteRecord>)>;

/// Subchannels `records` use that `table` lacks (sorted by id), each with the
/// MIDI channel of its first note.
fn missing_subchannels(
    table: &crate::model::SubchannelTable,
    records: &[NoteRecord],
) -> Vec<crate::model::Subchannel> {
    let mut out: Vec<crate::model::Subchannel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in records {
        if !table.contains(r.sub) && seen.insert(r.sub) {
            out.push(crate::model::Subchannel::new(r.sub, r.attrs.channel()));
        }
    }
    out.sort_by_key(|s| s.id);
    out
}

pub fn prepare_records(run: &TransformRun) -> Result<Prepared, TransformError> {
    prepare(run, true)
}

/// [`prepare_records`]; with `strict` false a result may use subchannels the
/// track lacks ([`build_operation`] creates them in the same undo step).
fn prepare(run: &TransformRun, strict: bool) -> Result<Prepared, TransformError> {
    let mut out = Vec::with_capacity(run.tracks.len());
    for (slot, t) in run.tracks.iter().enumerate() {
        let notes = run.track_notes(slot as u16);
        let mut recs = Vec::new();
        recs.try_reserve_exact(notes.len()).map_err(|_| {
            TransformError::TooLarge("Not enough memory to apply the result.".into())
        })?;
        for n in notes {
            if strict && !t.subs.iter().any(|s| s.0 == n.sub) {
                return Err(TransformError::Invalid(format!(
                    "A result uses subchannel {} which track {} does not have.",
                    u64::from(n.sub) + 1,
                    t.index + 1
                )));
            }
            // B2-F16: in the host's own ticks (a destination clip elsewhere
            // in the song).
            recs.push(t.host_record(n)?);
        }
        // `notes` are sorted by start within the slot (track-major order).
        out.push((t.clone(), recs));
    }
    Ok(out)
}

/// Whether `records` are exactly the selected notes of `store` as a multiset.
/// Both are in start order, so they are compared one start tick at a time
/// (notes sharing a start may come in any order): one streaming pass that
/// stops at the first difference, sorting only the notes of each shared tick.
fn same_as_selection(store: &crate::model::NoteStore, records: &[NoteRecord]) -> bool {
    type Key = (u64, u64, u32, u32, u32);
    fn key(r: &NoteRecord) -> Key {
        let (s, d, a, sub, g) = (
            r.start_tick,
            r.duration_ticks,
            r.attrs.raw(),
            r.sub.0,
            r.group,
        );
        (s, d, a, sub, g)
    }
    /// Compares the store's notes of one start tick (`group`) with the next
    /// records; advances `j`.
    fn settle(
        group: &mut Vec<Key>,
        other: &mut Vec<Key>,
        records: &[NoteRecord],
        j: &mut usize,
    ) -> bool {
        let n = group.len();
        if n == 0 {
            return true;
        }
        let start = group[0].0;
        let Some(slice) = records.get(*j..*j + n) else {
            return false;
        };
        if records.get(*j + n).is_some_and(|r| r.start_tick == start) {
            return false;
        }
        *j += n;
        let same = if n == 1 {
            key(&slice[0]) == group[0]
        } else {
            other.clear();
            other.extend(slice.iter().map(key));
            group.sort_unstable();
            other.sort_unstable();
            group == other
        };
        group.clear();
        same
    }
    if records.len() != store.selected_count() {
        return false;
    }
    let (mut group, mut other) = (Vec::new(), Vec::new());
    let mut j = 0;
    let mut same = true;
    store.for_each_selected(|v| {
        if !same {
            return;
        }
        let k = key(&v.record());
        if group.first().is_some_and(|g: &Key| g.0 != k.0) {
            same = settle(&mut group, &mut other, records, &mut j);
        }
        group.push(k);
    });
    same && settle(&mut group, &mut other, records, &mut j) && j == records.len()
}

/// The undo operation that writes `run` into `doc` (selects every note of
/// whole-track scopes first, so the replacement covers them). Refuses with
/// [`TransformError::Stale`] when a track changed since the source was taken.
pub fn build_operation(
    doc: &mut PlumDocument,
    run: &TransformRun,
    label: &'static str,
) -> Result<Operation, TransformError> {
    let mut prepared = prepare(run, false)?;
    // B2-F16: destination slots that share one host (two clips of one
    // content on a track a tool writes to) are one insertion, at the first
    // slot: their records are already in the host's ticks.
    for i in 0..prepared.len() {
        if !prepared[i].0.insert_only {
            continue;
        }
        let host = prepared[i].0.host();
        let mut more = Vec::new();
        for (t, records) in prepared.iter_mut().skip(i + 1) {
            if t.insert_only && t.host() == host {
                more.append(records);
            }
        }
        if !more.is_empty() {
            prepared[i].1.append(&mut more);
            prepared[i].1.sort_by_key(|r| r.start_tick);
        }
    }
    let mut ops = Vec::with_capacity(prepared.len() + 2);
    if let Some(g) = &run.groups
        && *g != doc.project().groups
    {
        ops.push(Operation::SetGroups {
            table: Box::new(g.clone()),
            label,
        });
    }
    for (slot, (t, records)) in prepared.into_iter().enumerate() {
        let notes = doc.notes_of_mut(t.host()).ok_or(TransformError::Stale)?;
        if notes.revision() != t.revision {
            return Err(TransformError::Stale);
        }
        if t.all {
            if t.frame < 0 {
                // B2-A02: a scoped clip that begins before the edited notes'
                // tick 0 — its notes before there were not in scope and stay
                // as they are.
                notes.clear_selection();
                notes.select_range(u64::try_from(-t.frame).unwrap_or(u64::MAX), u64::MAX, false);
            } else {
                notes.select_all();
            }
        }
        let base_revision = notes.revision();
        let track = doc.track_by_id(t.id).ok_or(TransformError::Stale)?;
        let notes = doc.notes_of(t.host()).ok_or(TransformError::Stale)?;
        // v0.8: subchannels the result uses that the track lacks (a subchannel
        // offset, a paint past the table) are created in the same undo step,
        // each with the MIDI channel of its first note. (An owned content's
        // subchannels are the track's it is edited on.)
        let missing = missing_subchannels(track.subchannels(), &records);
        let edit = if t.insert_only {
            if records.is_empty() {
                continue;
            }
            PreparedEdit::InsertSelected {
                host: t.host(),
                base_revision,
                records,
                subs_track: t.id,
                new_subs: missing,
            }
        } else {
            // v0.9 V09-S39: unselected notes the kernel removed on this
            // track (Draw "replace under the stroke") go in the same edit.
            let erase: Vec<NoteRecord> = run
                .erase
                .iter()
                .filter(|n| usize::from(n.track) == slot)
                .map(|n| t.host_record(n))
                .collect::<Result<_, _>>()?;
            // A result identical to the selection (the same notes, in any
            // order) records no undo step.
            if erase.is_empty() && same_as_selection(notes, &records) {
                continue;
            }
            if !missing.is_empty() {
                let index = doc.index_of(t.id).ok_or(TransformError::Stale)?;
                ops.push(Operation::AddSubchannels {
                    track: index,
                    subs: missing,
                });
            }
            if erase.is_empty() {
                PreparedEdit::ReplaceSelected {
                    host: t.host(),
                    base_revision,
                    records,
                    shift: None,
                }
            } else {
                PreparedEdit::ReplaceSelectedErasing {
                    host: t.host(),
                    base_revision,
                    records,
                    erase,
                }
            }
        };
        ops.push(Operation::Prepared { edit, label });
    }
    // v0.9 V09-S39: notes the kernel removed on tracks it only writes to
    // (Draw "replace under the stroke" on the Onion Skin tracks) — found by
    // value when applied (the edited tracks' are in their own edit).
    let mut i = 0;
    while i < run.erase.len() {
        let slot = run.erase[i].track;
        let j = i + run.erase[i..].partition_point(|n| n.track == slot);
        let t = &run.tracks[usize::from(slot)];
        if t.insert_only {
            ops.push(Operation::EraseRecords {
                track: t.target(),
                records: run.erase[i..j]
                    .iter()
                    .map(|n| t.host_record(n))
                    .collect::<Result<_, _>>()?,
            });
        }
        i = j;
    }
    for edit in event_edits(doc, run)? {
        ops.push(Operation::Events { edit, label });
    }
    if let Some((slot, notes)) = &run.new_subtrack
        && !notes.is_empty()
    {
        let st = &run.tracks[usize::from(*slot)];
        let parent_index = doc.index_of(st.id).ok_or(TransformError::Stale)?;
        let parent = doc.track(parent_index).ok_or(TransformError::Stale)?;
        let subs = parent.subchannels().clone();
        let mut store = crate::model::NoteStore::new();
        // B2-F16: a new track starts at the song's tick 0: the notes in
        // song time.
        let new = SourceTrack {
            frame: -run.shift,
            ..st.clone()
        };
        let recs: Vec<NoteRecord> = notes
            .iter()
            .map(|n| new.host_record(n))
            .collect::<Result<_, _>>()?;
        store
            .insert_sorted_batch(&recs, false)
            .map_err(|e| TransformError::Invalid(e.to_string()))?;
        let mut t = crate::model::Track::from_parts_exact(
            crate::model::TrackId(doc.next_track_id()),
            store,
            crate::model::EventStore::new(),
            None,
        );
        t.set_parent(Some(st.id));
        t.set_subchannels(subs);
        ops.push(Operation::InsertTracks {
            index: doc.subtree_end(parent_index),
            tracks: vec![t],
            label,
        });
    }
    Ok(match ops.len() {
        0 => return Err(TransformError::Unchanged),
        1 => ops.pop().expect("one"),
        _ => Operation::Group { ops, label },
    })
}

/// Events edits writing a run's controller points (one per lane and channel,
/// prepared in sequence on a working copy of each track's events).
fn event_edits(
    doc: &PlumDocument,
    run: &TransformRun,
) -> Result<Vec<crate::editor::EventsEdit>, TransformError> {
    use crate::editor::controllers::{ControllerKind, WritePolicy, prepare_points};
    let mut out = Vec::new();
    let mut i = 0;
    let ev = &run.events;
    while i < ev.len() {
        let slot = ev[i].track;
        let t = &run.tracks[usize::from(slot)];
        // A working copy of the events (prepared in sequence): the track's
        // own, or the owned content's.
        let view = crate::editor::note_view(doc, t.target()).map_err(|_| TransformError::Stale)?;
        let mut work = crate::editor::EventsWork::of(view);
        while i < ev.len() && ev[i].track == slot {
            let (lane, ch) = (ev[i].lane, ev[i].ch);
            let mut pts: Vec<(u64, u16)> = Vec::new();
            while i < ev.len() && ev[i].track == slot && ev[i].lane == lane && ev[i].ch == ch {
                pts.push((ev[i].tick, ev[i].value));
                i += 1;
            }
            let kind = match lane {
                super::output::Lane::Cc(n) => ControllerKind::Cc(n),
                super::output::Lane::PitchBend => ControllerKind::PitchBend,
            };
            let edit = prepare_points(
                work.view(),
                kind,
                ch,
                &pts,
                WritePolicy::Merge,
                super::output::MAX_EVENTS,
            )
            .map_err(|e| TransformError::Invalid(e.user_message()))?;
            if !work.apply(&edit) {
                return Err(TransformError::Stale);
            }
            out.push(edit);
        }
    }
    Ok(out)
}

/// Builds and applies the operation through `history` (one undo step).
pub fn commit(
    history: &mut History,
    doc: &mut PlumDocument,
    run: &TransformRun,
    label: &'static str,
) -> Result<(), TransformError> {
    let op = build_operation(doc, run, label)?;
    history
        .apply(doc, op)
        .map(|_| ())
        .map_err(|e: HistoryError| TransformError::History(e.user_message()))
}

/// [`collect_for`] with the tool's parameters: tools that read other tracks
/// (Clipping Mask) add their auxiliary layers and destination here.
pub fn collect_with(
    def: &super::ToolDef,
    doc: &PlumDocument,
    scopes: &[super::TrackScope],
    values: &super::ParamSet,
    limit: usize,
) -> Result<Source, TransformError> {
    collect_in(def, doc, scopes, values, limit, None)
}

/// [`collect_with`] knowing the run's context (v0.9 V09-S39: Draw reads the
/// notes around its pointer stroke, `Ctx::stroke`). The app passes it;
/// without it a tool reads only what its parameters say.
pub fn collect_in(
    def: &super::ToolDef,
    doc: &PlumDocument,
    scopes: &[super::TrackScope],
    values: &super::ParamSet,
    limit: usize,
    ctx: Option<&super::Ctx>,
) -> Result<Source, TransformError> {
    let spec = (def.spec)();
    // B2-A02: the scopes' frames, when the app resolved them.
    let frames = ctx
        .map(|c| c.scope_frames.clone())
        .filter(|f| f.len() == scopes.len())
        .unwrap_or_default();
    let mut src = Source::collect_framed(doc, scopes, &frames, limit)?;
    src.shift = ctx.map_or(0, |c| c.shift);
    if spec.context {
        src = src.with_context(doc);
    }
    let p = super::Params::new(&spec.params, values).with_keys(doc.project().keys);
    super::tools::prepare(def.id, doc, scopes, &p, &mut src, limit, ctx)?;
    Ok(src)
}

/// Collects the source a tool needs (with context notes when its spec asks).
/// (Tools that read other tracks use their default parameters here; the app
/// calls [`collect_with`] with the window's values.)
pub fn collect_for(
    def: &super::ToolDef,
    doc: &PlumDocument,
    scopes: &[super::TrackScope],
    limit: usize,
) -> Result<Source, TransformError> {
    let set = super::ParamSet::defaults(&(def.spec)().params);
    collect_with(def, doc, scopes, &set, limit)
}
