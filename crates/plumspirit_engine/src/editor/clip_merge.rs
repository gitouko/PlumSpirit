//! Clip merge (v0.9 V09-S12, contract §047 – §049, §218; design
//! `docs/CLIP_MERGE.md`).
//!
//! Two commands, never one ambiguous "Merge": [`MergeKind::Sequence`]
//! (horizontal: clips on one row) and [`MergeKind::Stack`] (vertical: clips
//! on several rows overlapping in time). [`plan_merge`] answers everything the
//! Preview shows — order, destination, span, counts, the subchannel mapping,
//! dropped duplicate events, the memory estimate — from counts and one pass
//! over the subchannel columns, without building anything. [`merge_clips`]
//! builds the merged content (in blocks of song time, so the temporary memory
//! is one block) and returns one undoable operation: the source placements
//! go (owned contents left without a placement into the history's stash, a
//! track's primary becomes a removed primary), one new owned content and its
//! placement come, and the destination gets the subchannels the mapping adds
//! — all in one step that Undo reverses exactly.

use super::clips::{
    ClipEditError, clip_span, new_clip_id, new_content_id, placement, put, take, track_id,
};
use crate::history::Operation;
use crate::model::events::{EventStore, RawEvent, RawKind};
use crate::model::track::{MAX_SUBCHANNEL_NAME_CHARS, Subchannel};
use crate::model::{
    ClipId, ClipPlacement, ContentRef, NoteRecord, NoteStore, OwnedContent, PlumDocument,
    SubchannelId,
};
use std::collections::{BTreeSet, HashMap};

/// Which merge (§047): two distinct commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeKind {
    /// Horizontal: clips on one row become one clip, timing kept relative to
    /// the earliest.
    Sequence,
    /// Vertical: clips on several rows that overlap in time become one clip on
    /// a destination row.
    Stack,
}

impl MergeKind {
    /// The command's name (menus, the Preview's title, the undo label).
    pub fn label(self) -> &'static str {
        match self {
            MergeKind::Sequence => "Merge Clips in Sequence",
            MergeKind::Stack => "Merge Clips Stacked",
        }
    }
}

/// One source clip, in merge order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeSource {
    pub id: ClipId,
    /// The row (track index) the clip sits on.
    pub row: usize,
    /// The track whose subchannel table its notes use: the content's own
    /// track for a track's content, the clip's row for an owned content.
    pub table: usize,
    pub content: ContentRef,
    /// Song ticks `[start, end)` the clip occupies.
    pub start: u64,
    pub end: u64,
    /// Content ticks `[a, b)` whose notes and events it plays.
    pub window: (u64, u64),
    /// Song tick − content tick.
    pub shift: i128,
    /// Notes it plays (starting in its window).
    pub notes: usize,
    /// Channel events in its window.
    pub events: usize,
}

/// One row of the subchannel mapping (§049).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubMap {
    /// Track whose table `from` belongs to.
    pub table: usize,
    pub from: SubchannelId,
    pub to: SubchannelId,
    /// `to` is a subchannel the merge adds to the destination.
    pub added: bool,
}

/// Everything the Preview shows (§048), and what [`merge_clips`] builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergePlan {
    pub kind: MergeKind,
    /// Sources in merge order (later wins duplicate events; equal ticks keep
    /// this order).
    pub sources: Vec<MergeSource>,
    /// Destination row (track index).
    pub dest: usize,
    /// Rows the destination may be (Stack: every source row, in track order;
    /// Sequence: the one row).
    pub rows: Vec<usize>,
    /// Song ticks `[start, end)` of the merged clip.
    pub start: u64,
    pub end: u64,
    pub notes: usize,
    /// Channel events kept.
    pub events: usize,
    /// Events dropped because a later source has one of the same kind on the
    /// same channel at the same tick (no duplicate controllers).
    pub dropped_events: usize,
    /// Every (table, subchannel) the merged notes use, in (track, id) order;
    /// rows of the destination's own table map to themselves.
    pub mapping: Vec<SubMap>,
    /// Subchannels added to the destination (in id order).
    pub new_subs: Vec<Subchannel>,
    /// Estimated memory of the merged clip (its notes and events), bytes; it
    /// is also what the undo history keeps while the step is in it.
    pub bytes: u64,
    /// The document state the plan was made for ([`merge_clips`] refuses a
    /// stale plan).
    key: u64,
}

/// Measured bytes per stored note of a merged content (start, duration,
/// attributes, subchannel and group columns, chunk overhead): ~22 B in the
/// 1M-note test, rounded up.
pub const STORED_BYTES_PER_NOTE: u64 = 24;
/// Bytes per stored channel event.
pub const BYTES_PER_EVENT: u64 = 16;
/// Notes gathered per block while building (temporary memory bound).
const BLOCK_NOTES: usize = 1 << 16;

fn err<T>(m: &str) -> Result<T, ClipEditError> {
    Err(ClipEditError(m.into()))
}

/// The stores a content plays from.
fn stores(doc: &PlumDocument, c: ContentRef) -> Option<(&NoteStore, &EventStore)> {
    match c {
        ContentRef::Track(t) => doc.track_by_id(t).map(|t| (&t.notes, &t.events)),
        ContentRef::Owned(o) => doc.clips().content(o).map(|o| (&o.notes, &o.events)),
    }
}

impl MergeSource {
    /// Content ticks `[a, b)` of song ticks `[s0, s1)` inside the window
    /// (`None`: empty).
    fn content_range(&self, s0: u64, s1: u64) -> Option<(u64, u64)> {
        let a = (i128::from(s0) - self.shift).max(i128::from(self.window.0));
        let b = (i128::from(s1) - self.shift).min(i128::from(self.window.1));
        (b > a).then_some((a as u64, b as u64))
    }

    /// Notes starting in song ticks `[s0, s1)` (no note visited).
    fn count_in(&self, notes: &NoteStore, s0: u64, s1: u64) -> usize {
        self.content_range(s0, s1)
            .map_or(0, |(a, b)| notes.count_starting_in(a, b - 1))
    }

    /// Song tick of content tick `c`.
    fn song(&self, c: u64) -> u64 {
        (i128::from(c) + self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }
}

/// The identity of a channel event's slot for duplicate removal: two events
/// of one kind on one channel at one tick (Control Change and Poly Pressure
/// per controller / key; Program, Channel Pressure and Pitch Bend per
/// channel). Other events are never treated as duplicates.
fn slot(kind: &RawKind) -> Option<(u8, u8)> {
    match kind {
        RawKind::Channel { status, data } => match status & 0xF0 {
            0xA0 | 0xB0 => Some((*status, data[0])),
            0xC0 | 0xD0 | 0xE0 => Some((*status, 0)),
            _ => None,
        },
        _ => None,
    }
}

/// The merged channel events: each source's events in its window, moved to
/// ticks relative to `start`, in (tick, merge order, stored order); at one
/// tick a slot used by a later source drops the earlier sources' events of
/// that slot. Returns the events and the number dropped.
fn merged_events(
    doc: &PlumDocument,
    sources: &[MergeSource],
    start: u64,
) -> Result<(Vec<RawEvent>, usize), ClipEditError> {
    let mut all: Vec<(u64, usize, usize, RawEvent)> = Vec::new();
    for (si, s) in sources.iter().enumerate() {
        let (_, events) = stores(doc, s.content)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        for (k, e) in events.events().iter().enumerate() {
            if !matches!(e.kind, RawKind::Channel { .. })
                || e.tick < s.window.0
                || e.tick >= s.window.1
            {
                continue;
            }
            let t = s.song(e.tick).saturating_sub(start);
            all.push((t, si, k, RawEvent { tick: t, ..*e }));
        }
    }
    all.sort_by_key(|&(t, si, k, _)| (t, si, k));
    let mut out = Vec::with_capacity(all.len());
    let mut dropped = 0;
    let mut i = 0;
    while i < all.len() {
        let mut j = i;
        while j < all.len() && all[j].0 == all[i].0 {
            j += 1;
        }
        // The last source using each slot at this tick.
        let mut last: HashMap<(u8, u8), usize> = HashMap::new();
        for e in &all[i..j] {
            if let Some(k) = slot(&e.3.kind) {
                last.insert(k, e.1);
            }
        }
        for e in &all[i..j] {
            match slot(&e.3.kind) {
                Some(k) if last[&k] != e.1 => dropped += 1,
                _ => out.push(e.3),
            }
        }
        i = j;
    }
    Ok((out, dropped))
}

/// The placements `ids` (each once, in the given order): one lookup each for
/// a few clips, one map of the clip table for many (a box selection).
fn lookup(doc: &PlumDocument, ids: &[ClipId]) -> Result<Vec<ClipPlacement>, ClipEditError> {
    let mut seen: BTreeSet<ClipId> = BTreeSet::new();
    let ids: Vec<ClipId> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    if ids.len() <= 16 {
        return ids.iter().map(|&id| placement(doc, id)).collect();
    }
    let clips = doc.clips();
    let mut map: HashMap<ClipId, &ClipPlacement> = clips
        .explicit_placements()
        .iter()
        .map(|p| (p.id, p))
        .collect();
    let removed: BTreeSet<_> = clips.removed_primaries().iter().copied().collect();
    ids.iter()
        .map(|&id| match (map.remove(&id), id) {
            (Some(p), _) => Ok(p.clone()),
            (None, ClipId::Primary(t)) if !removed.contains(&t) && doc.track_by_id(t).is_some() => {
                Ok(ClipPlacement::primary(t))
            }
            _ => err("That clip no longer exists."),
        })
        .collect()
}

/// Whether clips `ids` can be merged this way (§047): `Err` holds the
/// sentence saying why not. Cheap — no note is visited (menus ask it).
pub fn check_merge(
    doc: &PlumDocument,
    ids: &[ClipId],
    kind: MergeKind,
) -> Result<(), ClipEditError> {
    eligible(doc, ids, kind).map(|_| ())
}

/// The eligible sources in merge order, their counts not yet filled.
fn eligible(
    doc: &PlumDocument,
    ids: &[ClipId],
    kind: MergeKind,
) -> Result<Vec<MergeSource>, ClipEditError> {
    let mut sources: Vec<MergeSource> = Vec::new();
    for p in lookup(doc, ids)? {
        if p.muted {
            return err(
                "Muted clips are not merged: unmute them or leave them out of the selection.",
            );
        }
        let row = doc
            .index_of(p.track)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        let table = match p.content {
            ContentRef::Track(t) => doc
                .index_of(t)
                .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?,
            ContentRef::Owned(_) => row,
        };
        stores(doc, p.content)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        let (start, end) = clip_span(doc, &p);
        sources.push(MergeSource {
            id: p.id,
            row,
            table,
            content: p.content,
            start,
            end,
            window: p.content_window(),
            shift: i128::from(p.start) - i128::from(p.offset),
            notes: 0,
            events: 0,
        });
    }
    if sources.len() < 2 {
        return err("Select at least two clips to merge.");
    }
    let rows = rows_of(&sources);
    match kind {
        MergeKind::Sequence => {
            if rows.len() != 1 {
                return err(
                    "Merge Clips in Sequence joins clips on one track: select clips on a single row (clips on several rows that play together: Merge Clips Stacked).",
                );
            }
            // By start, then the Track List's drawing order (primary first,
            // then id).
            sources.sort_by_key(|s| (s.start, !matches!(s.id, ClipId::Primary(_)), s.id));
        }
        MergeKind::Stack => {
            if rows.len() < 2 {
                return err(
                    "Merge Clips Stacked joins clips on several tracks that play at the same time: select clips on two or more rows (clips on one row: Merge Clips in Sequence).",
                );
            }
            // Every clip overlaps a clip of another row: sweep the clips by
            // start, keeping the latest end of each row seen so far (no
            // quadratic scan for box selections).
            if !every_overlaps_another_row(&sources) {
                return err(
                    "Merge Clips Stacked needs every clip to overlap in time with a clip on another row.",
                );
            }
            sources.sort_by_key(|s| (s.row, s.start, !matches!(s.id, ClipId::Primary(_)), s.id));
        }
    }
    Ok(sources)
}

fn rows_of(sources: &[MergeSource]) -> Vec<usize> {
    let mut rows: Vec<usize> = sources.iter().map(|s| s.row).collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// Whether each clip overlaps (in song time) a clip on another row: two
/// sweeps (O(n log n) for box selections). Forward by start: the clips
/// started no later that end after my start — the latest-ending one of
/// another row is enough. Backward: the clips starting no earlier that start
/// before my end — the earliest-starting one of another row is enough.
fn every_overlaps_another_row(sources: &[MergeSource]) -> bool {
    let n = sources.len();
    let mut ok = vec![false; n];
    let mut idx: Vec<usize> = (0..n).collect();
    let row = |i: usize| sources[i].row;
    // `best` is the best clip so far, `second` the best on a row other than
    // `best`'s; together they hold the best clip of any row but one.
    let push = |best: &mut Option<usize>,
                second: &mut Option<usize>,
                i: usize,
                better: &dyn Fn(usize, usize) -> bool| match *best {
        None => *best = Some(i),
        Some(b) if better(i, b) => {
            if row(b) != row(i) {
                *second = Some(b);
            }
            *best = Some(i);
        }
        Some(b) => {
            if row(b) != row(i) && second.is_none_or(|c| better(i, c)) {
                *second = Some(i);
            }
        }
    };
    let other = |best: Option<usize>, second: Option<usize>, i: usize| {
        if best.is_some_and(|b| row(b) != row(i)) {
            best
        } else {
            second
        }
    };
    idx.sort_by_key(|&i| (sources[i].start, sources[i].end));
    let (mut best, mut second) = (None, None);
    let later_end = |a: usize, b: usize| sources[a].end > sources[b].end;
    for &i in &idx {
        if other(best, second, i).is_some_and(|c| sources[c].end > sources[i].start) {
            ok[i] = true;
        }
        push(&mut best, &mut second, i, &later_end);
    }
    let (mut best, mut second) = (None, None);
    let earlier_start = |a: usize, b: usize| sources[a].start < sources[b].start;
    for &i in idx.iter().rev() {
        if other(best, second, i).is_some_and(|c| sources[c].start < sources[i].end) {
            ok[i] = true;
        }
        push(&mut best, &mut second, i, &earlier_start);
    }
    ok.into_iter().all(|v| v)
}

/// Plans a merge of clips `ids` (§047 – §049): eligibility (an error
/// sentence when the command cannot run), merge order, destination (`dest`:
/// a row index, else the first eligible), span, counts and the subchannel
/// mapping. Builds nothing.
pub fn plan_merge(
    doc: &PlumDocument,
    ids: &[ClipId],
    kind: MergeKind,
    dest: Option<usize>,
) -> Result<MergePlan, ClipEditError> {
    let mut sources = eligible(doc, ids, kind)?;
    for s in &mut sources {
        let (notes, events) = stores(doc, s.content)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        let w = s.window;
        s.notes = if w.1 > w.0 {
            notes.count_starting_in(w.0, w.1 - 1)
        } else {
            0
        };
        s.events = events
            .events()
            .iter()
            .filter(|e| matches!(e.kind, RawKind::Channel { .. }) && e.tick >= w.0 && e.tick < w.1)
            .count();
    }
    let rows = rows_of(&sources);
    let dest = match dest {
        Some(d) if rows.contains(&d) => d,
        Some(_) => return err("The merged clip goes on one of the merged clips' rows."),
        None => rows[0],
    };
    track_id(doc, dest)?;
    let start = sources.iter().map(|s| s.start).min().unwrap_or(0);
    let end = sources
        .iter()
        .map(|s| s.end)
        .max()
        .unwrap_or(start)
        .max(start + 1);
    // The subchannel mapping (§049): the destination's own table maps to
    // itself; every other (table, subchannel) used keeps its id when the
    // destination neither has nor received it, else takes the next id above
    // all of them, in (track, id) order.
    let dest_table = doc
        .track(dest)
        .ok_or_else(|| ClipEditError("That track does not exist.".into()))?
        .subchannels();
    let mut used: BTreeSet<(usize, SubchannelId)> = BTreeSet::new();
    for s in &sources {
        let (notes, _) = stores(doc, s.content)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        if s.window.1 <= s.window.0 || s.notes == 0 {
            continue;
        }
        let (a, b) = s.window;
        notes.query(a, b, |_, v| {
            if v.start_tick >= a && v.start_tick < b {
                used.insert((s.table, v.sub));
            }
        });
    }
    let mut taken: BTreeSet<SubchannelId> = dest_table.entries().iter().map(|e| e.id).collect();
    let mut next = taken
        .iter()
        .next_back()
        .map_or(0, |id| id.0.saturating_add(1));
    let mut mapping = Vec::with_capacity(used.len());
    let mut new_subs = Vec::new();
    for &(table, sub) in &used {
        if table == dest {
            mapping.push(SubMap {
                table,
                from: sub,
                to: sub,
                added: false,
            });
            continue;
        }
        let to = if taken.contains(&sub) {
            let id = SubchannelId(next);
            next = next
                .checked_add(1)
                .ok_or_else(|| ClipEditError("No more subchannels can be added.".into()))?;
            id
        } else {
            sub
        };
        taken.insert(to);
        next = next.max(to.0.saturating_add(1));
        let t = doc
            .track(table)
            .ok_or_else(|| ClipEditError("That track does not exist.".into()))?;
        let src = t.subchannels().get(sub);
        let name = format!(
            "{} · {}",
            t.display_name(table),
            src.map_or_else(
                || format!("Subchannel {}", sub.number()),
                |s| s.display_name()
            )
        )
        .chars()
        .take(MAX_SUBCHANNEL_NAME_CHARS)
        .collect::<String>();
        new_subs.push(Subchannel {
            id: to,
            midi_channel: src.map_or(0, |s| s.midi_channel),
            name: Some(name),
        });
        mapping.push(SubMap {
            table,
            from: sub,
            to,
            added: true,
        });
    }
    new_subs.sort_by_key(|s| s.id);
    let (events, dropped) = merged_events(doc, &sources, start)?;
    let notes: usize = sources.iter().map(|s| s.notes).sum();
    let bytes = (notes as u64)
        .saturating_mul(STORED_BYTES_PER_NOTE)
        .saturating_add((events.len() as u64).saturating_mul(BYTES_PER_EVENT));
    let key = state_key(doc, &sources, dest);
    Ok(MergePlan {
        kind,
        sources,
        dest,
        rows,
        start,
        end,
        notes,
        events: events.len(),
        dropped_events: dropped,
        mapping,
        new_subs,
        bytes,
        key,
    })
}

/// What a plan depends on: the clip layout, the track list, the sources'
/// contents and the subchannel tables involved.
fn state_key(doc: &PlumDocument, sources: &[MergeSource], dest: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(doc.clips().revision());
    mix(doc.structure_revision());
    mix(dest as u64);
    for s in sources {
        let (n, e) = doc
            .content_revision(s.content)
            .unwrap_or((u64::MAX, u64::MAX));
        mix(n);
        mix(e);
        mix(doc
            .track(s.table)
            .map_or(u64::MAX, |t| t.subchannels_revision()));
    }
    mix(doc
        .track(dest)
        .map_or(u64::MAX, |t| t.subchannels_revision()));
    h
}

/// Builds the merge `plan` describes (its notes, events and subchannels) as
/// one undoable operation — the plan must be current (`plan_merge` on this
/// document state; a stale one is refused with a sentence).
pub fn merge_clips(doc: &PlumDocument, plan: &MergePlan) -> Result<Operation, ClipEditError> {
    let ids: Vec<ClipId> = plan.sources.iter().map(|s| s.id).collect();
    let fresh = plan_merge(doc, &ids, plan.kind, Some(plan.dest))?;
    if fresh != *plan {
        return err("The clips changed while the merge was being prepared: check the merge again.");
    }
    let maps: Vec<Option<HashMap<SubchannelId, SubchannelId>>> = plan
        .sources
        .iter()
        .map(|s| {
            (s.table != plan.dest).then(|| {
                plan.mapping
                    .iter()
                    .filter(|m| m.table == s.table)
                    .map(|m| (m.from, m.to))
                    .collect()
            })
        })
        .collect();
    let notes = merged_notes(doc, plan, &maps)?;
    let (events, _) = merged_events(doc, &plan.sources, plan.start)?;
    let mut ev = EventStore::new();
    for e in events {
        ev.push(e)
            .map_err(|_| ClipEditError("The clips' events could not be merged.".into()))?;
    }
    ev.shrink_to_fit();
    let mut layout = doc.clips().layout();
    for s in &plan.sources {
        take(&mut layout, s.id);
    }
    let cid = new_content_id(&mut layout)?;
    let id = new_clip_id(&mut layout)?;
    put(
        &mut layout,
        ClipPlacement {
            id,
            track: track_id(doc, plan.dest)?,
            content: ContentRef::Owned(cid),
            start: plan.start,
            offset: 0,
            length: Some(plan.end - plan.start),
            muted: false,
            name: None,
            color: None,
            revision: 0,
        },
    );
    let mut content = OwnedContent::new(cid);
    content.notes = notes;
    content.events = ev;
    let label = plan.kind.label();
    let set = Operation::SetClips {
        layout: Box::new(layout),
        contents: vec![content],
        label,
    };
    Ok(if plan.new_subs.is_empty() {
        set
    } else {
        Operation::Group {
            ops: vec![
                Operation::AddSubchannels {
                    track: plan.dest,
                    subs: plan.new_subs.clone(),
                },
                set,
            ],
            label,
        }
    })
}

/// The merged notes: song time cut into blocks of at most ~`BLOCK_NOTES`
/// starts (split by counting, no note visited); each block's notes gathered
/// in merge order, stably sorted by start and appended — the result store is
/// the only full copy.
fn merged_notes(
    doc: &PlumDocument,
    plan: &MergePlan,
    maps: &[Option<HashMap<SubchannelId, SubchannelId>>],
) -> Result<NoteStore, ClipEditError> {
    let gone = || ClipEditError("That clip no longer exists.".into());
    let srcs: Vec<&NoteStore> = plan
        .sources
        .iter()
        .map(|s| stores(doc, s.content).map(|(n, _)| n).ok_or_else(gone))
        .collect::<Result<_, _>>()?;
    let count = |a: u64, b: u64| -> usize {
        plan.sources
            .iter()
            .zip(&srcs)
            .map(|(s, n)| s.count_in(n, a, b))
            .sum()
    };
    let mut out = NoteStore::new();
    let mut remaining = plan.notes;
    let mut buf: Vec<NoteRecord> = Vec::new();
    // Blocks in ascending order: the right half is pushed first.
    let mut stack: Vec<(u64, u64)> = vec![(plan.start, plan.end)];
    while let Some((a, b)) = stack.pop() {
        let n = count(a, b);
        if n == 0 {
            continue;
        }
        if n > BLOCK_NOTES && b - a > 1 {
            let m = a + (b - a) / 2;
            stack.push((m, b));
            stack.push((a, m));
            continue;
        }
        buf.clear();
        buf.try_reserve(n)
            .map_err(|_| ClipEditError("Not enough memory to merge these clips.".into()))?;
        for (si, (s, notes)) in plan.sources.iter().zip(&srcs).enumerate() {
            let Some((ca, cb)) = s.content_range(a, b) else {
                continue;
            };
            let map = maps[si].as_ref();
            notes.query(ca, cb, |_, v| {
                if v.start_tick < ca || v.start_tick >= cb {
                    return;
                }
                buf.push(NoteRecord {
                    start_tick: s.song(v.start_tick) - plan.start,
                    duration_ticks: v.duration_ticks,
                    attrs: v.attrs,
                    sub: map.and_then(|m| m.get(&v.sub).copied()).unwrap_or(v.sub),
                    group: v.group,
                });
            });
        }
        // Stable: equal starts keep merge order, then stored order.
        buf.sort_by_key(|r| r.start_tick);
        out.append_sorted(&buf, remaining)
            .map_err(|_| ClipEditError("Not enough memory to merge these clips.".into()))?;
        remaining = remaining.saturating_sub(buf.len());
    }
    if out.len() != plan.notes {
        return err("The clips changed while the merge was being prepared: check the merge again.");
    }
    Ok(out)
}
