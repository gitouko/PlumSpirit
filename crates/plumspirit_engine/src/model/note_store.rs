//! Chunked structure-of-arrays note storage.
//!
//! # Layout
//!
//! A [`NoteStore`] owns an ordered list of [`NoteChunk`]s. Each chunk holds a few
//! thousand notes as parallel primitive arrays (`start: Vec<u64>`, `dur: Vec<u64>`,
//! `attrs: Vec<NoteAttrs>`) plus a selection bitset. Per note this is 20 bytes of
//! payload + 1 bit of selection; there are no per-note heap objects.
//!
//! A fourth column, `sub: Vec<u32>` (the note's PlumSpirit subchannel), exists only
//! in chunks that hold a note outside the main subchannel: an empty column means
//! "every note is in [`SubchannelId::MAIN`]". Tracks that use one subchannel keep
//! 20 bytes per note; chunks that mix subchannels cost 24.
//!
//! # Invariants (checked by [`NoteStore::validate`])
//!
//! 1. No chunk is empty.
//! 2. Within a chunk, `start` is non-decreasing.
//! 3. Across chunks, the last start of chunk `i` is `<=` the first start of chunk `i+1`.
//!    Notes with equal starts keep their insertion order ("stable"), which the MIDI
//!    exporter relies on for deterministic same-tick ordering.
//! 4. `start + dur` never overflows `u64`.
//! 5. `chunk.max_end` / `chunk.max_dur` equal the maxima of the chunk's notes.
//! 6. `prefix_max_end[i] = max(chunk[0..=i].max_end)` (monotone, used by the range
//!    query to skip chunks that end before the viewport).
//! 7. Selection bitset length equals the chunk length and cached selection counts are
//!    exact.
//!
//! # Shared chunks (copy-on-write)
//!
//! Each chunk's note values are reference counted. [`NoteStore::snapshot`] clones
//! only the pointers (a few hundred for a million notes), so playback can read an
//! immutable view of the notes while the user keeps editing: an edit copies just
//! the chunk it touches (`Arc::make_mut`), never the whole track. Selection bits
//! are kept outside the shared values, so selecting notes never copies anything.
//! Without an outstanding snapshot every chunk is uniquely owned and edits happen
//! in place.
//!
//! # Range query
//!
//! [`NoteStore::query`] finds notes with `start <= t1 && end >= t0` without scanning
//! notes that begin long before the viewport: chunks are excluded via `prefix_max_end`
//! and `max_end`, and inside a chunk only notes with `start >= t0 - max_dur` can reach
//! the viewport, so a binary search bounds the scan. Work therefore depends on the
//! visible range, not on the total note count.

use super::bitset::BitSet;
use super::note::{NoteAttrs, NoteAttrsError, NoteOffKind, NoteRecord, SubchannelId};
use super::patch::{CompactRecords, RankSet, RankSetBuilder};
use super::sub_column::{SubColumn, SubRef};
use std::collections::TryReserveError;
use std::fmt;
use std::sync::Arc;

/// Default number of notes per chunk. Chosen by the chunk-size benchmark in
/// `tests/stress.rs` (see docs/PERFORMANCE.md): large enough that chunk metadata is
/// tiny compared with note data, small enough that single-note insertion only moves a
/// few tens of KiB.
pub const DEFAULT_CHUNK_TARGET: usize = 4096;
const _: () = assert!(DEFAULT_CHUNK_TARGET.is_power_of_two());

/// Location of a note inside a store. Only valid for the store revision it was
/// obtained from; any mutation may move notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoteLoc {
    pub chunk: u32,
    pub index: u32,
}

/// Read-only view of one stored note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteView {
    pub start_tick: u64,
    pub duration_ticks: u64,
    pub attrs: NoteAttrs,
    pub sub: SubchannelId,
    /// Group (v0.7; 0 = none).
    pub group: u32,
    pub selected: bool,
}

impl NoteView {
    /// End tick. Cannot overflow because stores reject notes whose end overflows.
    #[inline]
    pub fn end_tick(&self) -> u64 {
        self.start_tick + self.duration_ticks
    }

    #[inline]
    pub fn record(&self) -> NoteRecord {
        NoteRecord {
            start_tick: self.start_tick,
            duration_ticks: self.duration_ticks,
            attrs: self.attrs,
            sub: self.sub,
            group: self.group,
        }
    }
}

/// Aggregate bounds of the selected notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionBounds {
    pub count: usize,
    pub min_start: u64,
    pub max_end: u64,
    pub min_pitch: u8,
    pub max_pitch: u8,
    /// Smallest duration among selected notes.
    pub min_duration: u64,
}

/// [`NoteStore::selection_uniformity`]: what the selected notes share.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionUniformity {
    /// Selected notes that are muted.
    pub muted: usize,
    /// The selected notes' common duration (`None`: no selection or not all equal).
    pub gate: Option<u64>,
    /// The selected notes' common velocity (`None`: no selection or not all equal).
    pub velocity: Option<u8>,
}

/// [`NoteStore::overlapping`]: notes as records (content ticks), in stored
/// order.
pub struct Overlapping<'a, L> {
    chunks: &'a [NoteChunk],
    /// The next chunk to enter, and the end of the chunks to visit.
    ci: usize,
    hi: usize,
    /// The note cursor inside chunk `ci - 1`, and its end.
    i: usize,
    end: usize,
    t0: u64,
    /// Inclusive; already limited to `s1 - 1`.
    t1: u64,
    s0: u64,
    len: L,
}

impl<L: Fn(u64) -> u64> Iterator for Overlapping<'_, L> {
    type Item = NoteRecord;

    #[inline]
    fn next(&mut self) -> Option<NoteRecord> {
        loop {
            if self.i < self.end {
                let c = &self.chunks[self.ci - 1].data;
                let i = self.i;
                self.i += 1;
                let (s, d) = (c.start[i], c.dur[i]);
                if s.saturating_add((self.len)(d)) < self.t0 {
                    continue;
                }
                return Some(NoteRecord {
                    start_tick: s,
                    duration_ticks: d,
                    attrs: c.attrs[i],
                    sub: SubchannelId(c.sub_raw(i)),
                    group: c.grp_raw(i),
                });
            }
            if self.ci >= self.hi {
                return None;
            }
            let c = &self.chunks[self.ci].data;
            self.ci += 1;
            let longest = (self.len)(c.max_dur);
            let reach = c.max_end.saturating_add(longest.saturating_sub(c.max_dur));
            if reach < self.t0 {
                self.end = 0;
                self.i = 0;
                continue;
            }
            let lower = self.t0.saturating_sub(longest).max(self.s0);
            self.i = c.start.partition_point(|&s| s < lower);
            self.end = c.start.partition_point(|&s| s <= self.t1);
        }
    }
}

/// Instrumentation returned by range queries (development metrics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryStats {
    pub chunks_total: usize,
    pub chunks_visited: usize,
    pub notes_scanned: usize,
    pub notes_matched: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteStoreError {
    /// `start + duration` does not fit in a `u64` tick.
    EndOverflow {
        start_tick: u64,
        duration_ticks: u64,
    },
    /// Batch records were not sorted by start tick.
    UnsortedBatch,
    /// A duration change would make a note negative or overflow.
    DurationOutOfRange,
    /// Allocation failed (the system is out of memory).
    OutOfMemory { bytes: usize },
}

impl fmt::Display for NoteStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoteStoreError::EndOverflow {
                start_tick,
                duration_ticks,
            } => write!(
                f,
                "a note starting at tick {start_tick} with duration {duration_ticks} would end beyond the largest representable tick"
            ),
            NoteStoreError::UnsortedBatch => write!(f, "internal error: batch not sorted"),
            NoteStoreError::DurationOutOfRange => {
                write!(
                    f,
                    "the requested duration change is outside the valid range"
                )
            }
            NoteStoreError::OutOfMemory { bytes } => {
                write!(
                    f,
                    "not enough memory (needed about {} MB)",
                    bytes / (1024 * 1024) + 1
                )
            }
        }
    }
}

impl std::error::Error for NoteStoreError {}

impl From<TryReserveError> for NoteStoreError {
    fn from(_: TryReserveError) -> Self {
        NoteStoreError::OutOfMemory { bytes: 0 }
    }
}

/// The note values of one chunk. Reference counted: a [`NoteSnapshot`] shares it
/// with the store, and an edit copies it only while such a snapshot exists.
#[derive(Clone, Debug, Default)]
pub(crate) struct ChunkData {
    start: Vec<u64>,
    dur: Vec<u64>,
    attrs: Vec<NoteAttrs>,
    /// Subchannel ids; empty when every note of the chunk is in the main
    /// subchannel, otherwise exactly one entry per note (1, 2 or 4 bytes each,
    /// see [`SubColumn`]).
    sub: SubColumn,
    /// Note groups (v0.7); empty when no note of the chunk is grouped, otherwise
    /// one entry per note (same narrow encoding as the subchannel column).
    grp: SubColumn,
    max_end: u64,
    max_dur: u64,
}

/// Read access for the analysis engine (same crate): the columns of one chunk.
impl ChunkData {
    #[inline]
    pub(crate) fn starts(&self) -> &[u64] {
        &self.start
    }
    #[inline]
    pub(crate) fn durs(&self) -> &[u64] {
        &self.dur
    }
    #[inline]
    pub(crate) fn attrs_col(&self) -> &[NoteAttrs] {
        &self.attrs
    }
    #[inline]
    pub(crate) fn sub_at(&self, i: usize) -> u32 {
        self.sub.get(i)
    }
    #[inline]
    pub(crate) fn group_at(&self, i: usize) -> u32 {
        self.grp.get(i)
    }
    #[inline]
    pub(crate) fn chunk_max_end(&self) -> u64 {
        self.max_end
    }
    #[inline]
    pub(crate) fn chunk_max_dur(&self) -> u64 {
        self.max_dur
    }
}

impl ChunkData {
    /// Subchannel of note `i`.
    #[inline]
    fn sub_raw(&self, i: usize) -> u32 {
        self.sub.get(i)
    }

    /// Records the subchannel of a note about to be pushed (call before pushing
    /// its start). Materializes the column the first time a non-main note arrives.
    #[inline]
    fn push_sub(&mut self, sub: u32) {
        if !self.sub.is_empty() {
            self.sub.push(sub);
        } else if sub != 0 {
            let n = self.start.len();
            self.sub.reserve_exact(self.start.capacity().max(n + 1));
            self.sub.resize_zero(n);
            self.sub.push(sub);
        }
    }

    /// Like [`ChunkData::push_sub`] for an insertion at `i`.
    fn insert_sub(&mut self, i: usize, sub: u32) {
        if !self.sub.is_empty() {
            self.sub.insert(i, sub);
        } else if sub != 0 {
            let n = self.start.len();
            self.sub.reserve_exact(self.start.capacity().max(n + 1));
            self.sub.resize_zero(n);
            self.sub.insert(i, sub);
        }
    }

    /// Group of note `i` (0 = none).
    #[inline]
    fn grp_raw(&self, i: usize) -> u32 {
        self.grp.get(i)
    }

    /// Like [`ChunkData::push_sub`] for the group column.
    #[inline]
    fn push_grp(&mut self, g: u32) {
        if !self.grp.is_empty() {
            self.grp.push(g);
        } else if g != 0 {
            let n = self.start.len();
            self.grp.reserve_exact(self.start.capacity().max(n + 1));
            self.grp.resize_zero(n);
            self.grp.push(g);
        }
    }

    /// Like [`ChunkData::insert_sub`] for the group column.
    fn insert_grp(&mut self, i: usize, g: u32) {
        if !self.grp.is_empty() {
            self.grp.insert(i, g);
        } else if g != 0 {
            let n = self.start.len();
            self.grp.reserve_exact(self.start.capacity().max(n + 1));
            self.grp.resize_zero(n);
            self.grp.insert(i, g);
        }
    }

    /// Drops the columns that hold only defaults (main subchannel, no group).
    fn compact_sub(&mut self) {
        if !self.sub.is_empty() && self.sub.all_zero() {
            self.sub.clear();
        }
        if !self.grp.is_empty() && self.grp.all_zero() {
            self.grp.clear();
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.start.len()
    }

    #[inline]
    fn first_start(&self) -> u64 {
        self.start[0]
    }

    fn recompute_meta(&mut self) {
        let mut max_end = 0;
        let mut max_dur = 0;
        for (s, d) in self.start.iter().zip(&self.dur) {
            max_end = max_end.max(s + d);
            max_dur = max_dur.max(*d);
        }
        self.max_end = max_end;
        self.max_dur = max_dur;
        self.compact_sub();
    }

    fn heap_bytes(&self) -> usize {
        self.start.capacity() * 8
            + self.dur.capacity() * 8
            + self.attrs.capacity() * 4
            + self.sub.heap_bytes()
            + self.grp.heap_bytes()
    }
}

/// One chunk of a store: shared note values plus this store's own selection bits.
/// Selection lives outside the shared data, so selecting notes never copies notes
/// even while playback holds a snapshot.
#[derive(Clone, Debug, Default)]
struct NoteChunk {
    data: Arc<ChunkData>,
    sel: BitSet,
    selected: usize,
}

impl std::ops::Deref for NoteChunk {
    type Target = ChunkData;

    #[inline]
    fn deref(&self) -> &ChunkData {
        &self.data
    }
}

/// Builds a chunk note by note without touching the shared reference count on
/// every push (bulk merges, splits and batch inserts).
struct ChunkBuilder {
    data: ChunkData,
    sel: BitSet,
    selected: usize,
}

impl ChunkBuilder {
    fn with_capacity(cap: usize) -> Self {
        ChunkBuilder {
            data: ChunkData {
                start: Vec::with_capacity(cap),
                dur: Vec::with_capacity(cap),
                attrs: Vec::with_capacity(cap),
                sub: SubColumn::default(),
                grp: SubColumn::default(),
                max_end: 0,
                max_dur: 0,
            },
            sel: BitSet::with_capacity(cap),
            selected: 0,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.data.start.len()
    }

    #[inline]
    fn push(&mut self, start: u64, dur: u64, attrs: NoteAttrs, sub: u32, grp: u32, selected: bool) {
        let d = &mut self.data;
        d.push_sub(sub);
        d.push_grp(grp);
        d.start.push(start);
        d.dur.push(dur);
        d.attrs.push(attrs);
        let end = start + dur;
        if end > d.max_end {
            d.max_end = end;
        }
        if dur > d.max_dur {
            d.max_dur = dur;
        }
        self.sel.push(selected);
        if selected {
            self.selected += 1;
        }
    }

    fn finish(mut self) -> NoteChunk {
        self.data.compact_sub();
        NoteChunk {
            data: Arc::new(self.data),
            sel: self.sel,
            selected: self.selected,
        }
    }
}

impl NoteChunk {
    fn with_capacity(cap: usize) -> Self {
        NoteChunk {
            data: Arc::new(ChunkData {
                start: Vec::with_capacity(cap),
                dur: Vec::with_capacity(cap),
                attrs: Vec::with_capacity(cap),
                sub: SubColumn::default(),
                grp: SubColumn::default(),
                max_end: 0,
                max_dur: 0,
            }),
            sel: BitSet::with_capacity(cap),
            selected: 0,
        }
    }

    /// Mutable note values (copied first if a snapshot shares them).
    #[inline]
    fn data_mut(&mut self) -> &mut ChunkData {
        Arc::make_mut(&mut self.data)
    }

    #[inline]
    fn push(&mut self, start: u64, dur: u64, attrs: NoteAttrs, sub: u32, grp: u32, selected: bool) {
        let d = self.data_mut();
        d.push_sub(sub);
        d.push_grp(grp);
        d.start.push(start);
        d.dur.push(dur);
        d.attrs.push(attrs);
        let end = start + dur;
        if end > d.max_end {
            d.max_end = end;
        }
        if dur > d.max_dur {
            d.max_dur = dur;
        }
        self.sel.push(selected);
        if selected {
            self.selected += 1;
        }
    }

    /// Appends unselected notes (already validated and sorted) with one
    /// copy-on-write check for the whole batch (bulk loading).
    fn extend_unselected(&mut self, records: &[NoteRecord]) {
        let d = Arc::make_mut(&mut self.data);
        let n0 = d.start.len();
        d.start.extend(records.iter().map(|r| r.start_tick));
        d.dur.extend(records.iter().map(|r| r.duration_ticks));
        d.attrs.extend(records.iter().map(|r| r.attrs));
        if !d.sub.is_empty() {
            for r in records {
                d.sub.push(r.sub.0);
            }
        } else if let Some(first) = records.iter().position(|r| r.sub.0 != 0) {
            d.sub
                .reserve_exact(d.start.capacity().max(n0 + records.len()));
            d.sub.resize_zero(n0 + first);
            for r in &records[first..] {
                d.sub.push(r.sub.0);
            }
        }
        if !d.grp.is_empty() {
            for r in records {
                d.grp.push(r.group);
            }
        } else if let Some(first) = records.iter().position(|r| r.group != 0) {
            d.grp
                .reserve_exact(d.start.capacity().max(n0 + records.len()));
            d.grp.resize_zero(n0 + first);
            for r in &records[first..] {
                d.grp.push(r.group);
            }
        }
        for r in records {
            let end = r.start_tick + r.duration_ticks;
            if end > d.max_end {
                d.max_end = end;
            }
            if r.duration_ticks > d.max_dur {
                d.max_dur = r.duration_ticks;
            }
        }
        self.sel.push_zeros(records.len());
    }

    #[inline]
    fn view(&self, i: usize) -> NoteView {
        NoteView {
            start_tick: self.start[i],
            duration_ticks: self.dur[i],
            attrs: self.attrs[i],
            sub: SubchannelId(self.sub_raw(i)),
            group: self.grp_raw(i),
            selected: self.sel.get(i),
        }
    }

    fn insert_at(&mut self, i: usize, rec: NoteRecord, selected: bool) {
        let d = self.data_mut();
        // Grow in small steps instead of doubling: chunks are bounded in size, so
        // the extra copies are cheap, and doubling would leave up to 50% unused
        // capacity in every chunk after scattered edits.
        if d.start.len() == d.start.capacity() {
            let extra = (d.start.len() / 8).max(16);
            d.start.reserve_exact(extra);
            d.dur.reserve_exact(extra);
            d.attrs.reserve_exact(extra);
            if !d.sub.is_empty() {
                d.sub.reserve_exact(extra);
            }
            if !d.grp.is_empty() {
                d.grp.reserve_exact(extra);
            }
        }
        d.insert_sub(i, rec.sub.0);
        d.insert_grp(i, rec.group);
        d.start.insert(i, rec.start_tick);
        d.dur.insert(i, rec.duration_ticks);
        d.attrs.insert(i, rec.attrs);
        let end = rec.start_tick + rec.duration_ticks;
        d.max_end = d.max_end.max(end);
        d.max_dur = d.max_dur.max(rec.duration_ticks);
        self.sel.insert(i, selected);
        if selected {
            self.selected += 1;
        }
    }

    /// Splits into pieces of roughly `target` notes. Returns the pieces in order.
    fn split_into(self, target: usize) -> Vec<NoteChunk> {
        let n = self.len();
        if n <= target * 2 {
            return vec![self];
        }
        let pieces = n.div_ceil(target);
        let base = n / pieces;
        let extra = n % pieces;
        let mut out = Vec::with_capacity(pieces);
        let mut offset = 0;
        for p in 0..pieces {
            let size = base + usize::from(p < extra);
            let mut c = ChunkBuilder::with_capacity(size);
            for i in offset..offset + size {
                c.push(
                    self.start[i],
                    self.dur[i],
                    self.attrs[i],
                    self.sub_raw(i),
                    self.grp_raw(i),
                    self.sel.get(i),
                );
            }
            offset += size;
            out.push(c.finish());
        }
        out
    }

    fn append(&mut self, other: &NoteChunk) {
        debug_assert!(self.start.last().copied().unwrap_or(0) <= other.start[0]);
        let d = self.data_mut();
        if !d.sub.is_empty() || !other.sub.is_empty() {
            let n = d.start.len();
            d.sub.resize_zero(n);
            if other.sub.is_empty() {
                d.sub.resize_zero(n + other.len());
            } else {
                d.sub.extend_from(&other.sub);
            }
        }
        if !d.grp.is_empty() || !other.grp.is_empty() {
            let n = d.start.len();
            d.grp.resize_zero(n);
            if other.grp.is_empty() {
                d.grp.resize_zero(n + other.len());
            } else {
                d.grp.extend_from(&other.grp);
            }
        }
        d.start.extend_from_slice(&other.start);
        d.dur.extend_from_slice(&other.dur);
        d.attrs.extend_from_slice(&other.attrs);
        d.max_end = d.max_end.max(other.max_end);
        d.max_dur = d.max_dur.max(other.max_dur);
        for i in 0..other.len() {
            self.sel.push(other.sel.get(i));
        }
        self.selected += other.selected;
    }

    /// Removes selected notes, pushing them into `out` if provided.
    fn remove_selected_into(&mut self, out: Option<&mut Vec<NoteRecord>>) -> usize {
        if self.selected == 0 {
            return 0;
        }
        let n = self.len();
        let sel = std::mem::take(&mut self.sel);
        let d = Arc::make_mut(&mut self.data);
        if let Some(o) = out {
            let subs = d.sub.as_ref();
            let grps = d.grp.as_ref();
            for r in sel.iter_ones() {
                o.push(NoteRecord {
                    start_tick: d.start[r],
                    duration_ticks: d.dur[r],
                    attrs: d.attrs[r],
                    sub: SubchannelId(subs.get(r)),
                    group: grps.get(r),
                });
            }
        }
        let mut w = 0;
        for r in 0..n {
            if !sel.get(r) {
                d.start[w] = d.start[r];
                d.dur[w] = d.dur[r];
                d.attrs[w] = d.attrs[r];
                w += 1;
            }
        }
        d.sub.retain_indices(|r| !sel.get(r));
        d.grp.retain_indices(|r| !sel.get(r));
        let removed = n - w;
        d.start.truncate(w);
        d.dur.truncate(w);
        d.attrs.truncate(w);
        d.recompute_meta();
        // Every remaining note is unselected by construction.
        self.sel = BitSet::filled(w, false);
        self.selected = 0;
        removed
    }

    fn heap_bytes(&self) -> usize {
        self.data.heap_bytes() + self.sel.heap_bytes()
    }
}

/// Ordered, chunked note storage for one track.
#[derive(Clone, Debug)]
pub struct NoteStore {
    chunks: Vec<NoteChunk>,
    prefix_max_end: Vec<u64>,
    len: usize,
    selected: usize,
    chunk_target: usize,
    revision: u64,
    content_revision: u64,
}

impl Default for NoteStore {
    fn default() -> Self {
        NoteStore::new()
    }
}

/// The next content revision (v0.9 V09-S15): one process-wide sequence, so a
/// content revision names one state of one store's notes. Per-store counters
/// started at 0 for every new store, and caches keyed by (track / content
/// id, content revision) — the clip mini notes, the analysis, subchannel
/// and render caches — returned a removed store's data for a new store that
/// got the same id back after an Undo (an undone Paste Repeat's content id
/// reused by a merge: its mini notes drew the old content). A clone keeps
/// its revision (the same notes); every new store and every change of note
/// values takes a fresh one.
fn fresh_content_revision() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl NoteStore {
    pub fn new() -> Self {
        NoteStore::with_chunk_target(DEFAULT_CHUNK_TARGET)
    }

    /// Creates a store with a custom chunk size (used by benchmarks).
    pub fn with_chunk_target(target: usize) -> Self {
        NoteStore {
            chunks: Vec::new(),
            prefix_max_end: Vec::new(),
            len: 0,
            selected: 0,
            chunk_target: target.max(16),
            revision: 0,
            content_revision: fresh_content_revision(),
        }
    }

    /// Merges stores into one, in start order (notes with the same start keep
    /// the order of `parts`, then their own order). One pass over every note
    /// (k-way merge), no intermediate list of records; selection is dropped.
    pub fn merge_sorted(parts: &[&NoteStore]) -> Result<NoteStore, NoteStoreError> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        let mut iters: Vec<_> = parts.iter().map(|p| p.iter()).collect();
        let mut heap = BinaryHeap::with_capacity(parts.len());
        let mut heads: Vec<Option<NoteView>> = Vec::with_capacity(parts.len());
        for (k, it) in iters.iter_mut().enumerate() {
            let h = it.next();
            if let Some(v) = &h {
                heap.push(Reverse((v.start_tick, k)));
            }
            heads.push(h);
        }
        let mut b = NoteStoreBuilder::new();
        while let Some(Reverse((_, k))) = heap.pop() {
            let v = heads[k].take().expect("head of a queued part");
            let idx = b.push(v.start_tick, v.attrs)?;
            b.set_duration(idx, v.duration_ticks)?;
            if v.sub.0 != 0 {
                b.set_sub(idx, v.sub);
            }
            if v.group != 0 {
                b.set_group(idx, v.group);
            }
            if let Some(n) = iters[k].next() {
                heap.push(Reverse((n.start_tick, k)));
                heads[k] = Some(n);
            }
        }
        Ok(b.finish(&[]))
    }

    /// Builds a store from records sorted by start tick (stable order is kept).
    pub fn from_sorted_records(
        records: &[NoteRecord],
        selected: bool,
    ) -> Result<Self, NoteStoreError> {
        let mut s = NoteStore::new();
        s.insert_sorted_batch(records, selected)?;
        Ok(s)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn selected_count(&self) -> usize {
        self.selected
    }

    /// Monotonic counter incremented by every mutation (including selection changes).
    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The shared note values of every chunk, in order (analysis engine).
    pub(crate) fn chunk_data(&self) -> impl ExactSizeIterator<Item = &Arc<ChunkData>> + '_ {
        self.chunks.iter().map(|c| &c.data)
    }

    /// A 64-bit hash of the note values in store order — start, duration,
    /// attributes and subchannel — that does not depend on how the notes are
    /// split into chunks (v1.0-beta1 B1-0121, `PlumDocument::export_fingerprint`).
    /// Selection and note groups are not part of it. The three columns are
    /// hashed as independent chains (they overlap in the CPU); subchannels
    /// only as (index, id) pairs of the notes outside the main subchannel.
    pub fn content_hash(&self) -> u64 {
        use crate::codec::WordHash;
        let (mut s, mut d, mut a, mut sub) = (
            WordHash::new(),
            WordHash::new(),
            WordHash::new(),
            WordHash::new(),
        );
        let mut base = 0u64;
        for c in self.chunk_data() {
            let (st, du, at) = (c.starts(), c.durs(), c.attrs_col());
            let n = st.len();
            // Notes pair up by their index in the whole store (each column
            // hasher has taken one word per note so far).
            let mut i = 0;
            if base & 1 == 1 && n > 0 {
                s.word(st[0]);
                d.word(du[0]);
                a.word(u64::from(at[0].raw()));
                i = 1;
            }
            while i + 1 < n {
                s.pair(st[i], st[i + 1]);
                d.pair(du[i], du[i + 1]);
                a.pair(u64::from(at[i].raw()), u64::from(at[i + 1].raw()));
                i += 2;
            }
            if i < n {
                s.word(st[i]);
                d.word(du[i]);
                a.word(u64::from(at[i].raw()));
            }
            if !c.sub.is_empty() {
                for i in 0..st.len() {
                    let id = c.sub_at(i);
                    if id != 0 {
                        sub.word(base + i as u64);
                        sub.word(u64::from(id));
                    }
                }
            }
            base += st.len() as u64;
        }
        let mut h = WordHash::new();
        h.words(&[base, s.finish(), d.finish(), a.finish(), sub.finish()]);
        h.finish()
    }

    /// Every chunk that holds selected notes, with its selection bits, in
    /// order (selection analytics, v0.7).
    pub(crate) fn selected_chunks(&self) -> impl Iterator<Item = (&Arc<ChunkData>, &BitSet)> + '_ {
        self.chunks
            .iter()
            .filter(|c| c.selected > 0)
            .map(|c| (&c.data, &c.sel))
    }

    /// Chunks from the first one that holds a note starting after `tick`
    /// (`None`: from the first chunk), in order.
    pub(crate) fn chunk_data_after_start(
        &self,
        tick: Option<u64>,
    ) -> impl Iterator<Item = &Arc<ChunkData>> + '_ {
        let first = match tick {
            Some(t) => self
                .chunks
                .partition_point(|c| c.start.last().is_some_and(|&l| l <= t)),
            None => 0,
        };
        self.chunks[first..].iter().map(|c| &c.data)
    }

    /// The latest end of the notes starting in `[lo, hi)` (`None`: none).
    /// O(chunks in the window), plus a scan of at most one partial chunk at
    /// each edge — never per note of whole chunks (v0.9 clip windows).
    pub fn max_end_of_starts_in(&self, lo: u64, hi: u64) -> Option<u64> {
        if lo >= hi {
            return None;
        }
        let mut best: Option<u64> = None;
        for c in self.chunk_data_after_start(lo.checked_sub(1)) {
            let st = c.starts();
            if st[0] >= hi {
                break;
            }
            let a = st.partition_point(|&s| s < lo);
            let b = st.partition_point(|&s| s < hi);
            let end = if a == 0 && b == st.len() {
                c.chunk_max_end()
            } else {
                (a..b)
                    .map(|i| st[i].saturating_add(c.durs()[i]))
                    .max()
                    .unwrap_or(0)
            };
            if a < b {
                best = Some(best.map_or(end, |x| x.max(end)));
            }
        }
        best
    }

    /// Chunks from the first one whose running maximum end exceeds `tick`, in
    /// order (every earlier chunk ends at or before `tick`).
    pub(crate) fn chunk_data_ending_after(
        &self,
        tick: u64,
    ) -> impl Iterator<Item = &Arc<ChunkData>> + '_ {
        let first = self.prefix_max_end.partition_point(|&m| m <= tick);
        self.chunks[first..].iter().map(|c| &c.data)
    }

    /// Changes only when note values change (not on selection changes), and
    /// is never the same for two different note states in this process
    /// (`fresh_content_revision`). Playback and the caches use it to know when
    /// a snapshot is out of date.
    #[inline]
    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    #[inline]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn chunk_target(&self) -> usize {
        self.chunk_target
    }

    /// Latest end tick of any note, or `None` for an empty store. O(1).
    pub fn max_end(&self) -> Option<u64> {
        self.prefix_max_end.last().copied()
    }

    /// Earliest start tick, or `None` for an empty store. O(1).
    /// Longest note duration (O(chunks)).
    pub fn max_duration(&self) -> u64 {
        self.chunks.iter().map(|c| c.max_dur).max().unwrap_or(0)
    }

    pub fn min_start(&self) -> Option<u64> {
        self.chunks.first().map(|c| c.first_start())
    }

    pub fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.heap_bytes()).sum::<usize>()
            + self.chunks.capacity() * std::mem::size_of::<NoteChunk>()
            + self.chunks.len() * std::mem::size_of::<ChunkData>()
            + self.prefix_max_end.capacity() * 8
    }

    pub fn get(&self, loc: NoteLoc) -> Option<NoteView> {
        let c = self.chunks.get(loc.chunk as usize)?;
        let i = loc.index as usize;
        (i < c.len()).then(|| c.view(i))
    }

    /// Iterates all notes in stored (start-tick) order. Intended for export, copy and
    /// tests. Rendering must use [`NoteStore::query`] instead.
    pub fn iter(&self) -> NoteIter<'_> {
        NoteIter::new(&self.chunks)
    }

    /// Like [`NoteStore::iter`] without muted notes (v0.7): what playback and
    /// export hear. Chunks without a muted note pay nothing per note.
    pub fn iter_audible(&self) -> NoteIter<'_, true> {
        NoteIter::new(&self.chunks)
    }

    /// Iterates the notes whose (MIDI channel, subchannel) satisfy `keep`, in
    /// stored order. The test reads only the channel and subchannel columns, so
    /// skipping notes is much cheaper than filtering [`NoteStore::iter`]
    /// (export writes each layer chunk with one such pass).
    pub fn iter_where<'a, F: Fn(u8, u32) -> bool + Copy + 'a>(
        &'a self,
        keep: F,
    ) -> impl Iterator<Item = NoteView> + 'a {
        self.chunks.iter().flat_map(move |c| {
            (0..c.len())
                .filter(move |&i| keep(c.attrs[i].channel(), c.sub_raw(i)))
                .map(move |i| c.view(i))
        })
    }

    /// [`NoteStore::iter_where`] without muted notes (export).
    pub fn iter_where_audible<'a, F: Fn(u8, u32) -> bool + Copy + 'a>(
        &'a self,
        keep: F,
    ) -> impl Iterator<Item = NoteView> + 'a {
        self.chunks.iter().flat_map(move |c| {
            (0..c.len())
                .filter(move |&i| {
                    let a = c.attrs[i];
                    !a.muted() && keep(a.channel(), c.sub_raw(i))
                })
                .map(move |i| c.view(i))
        })
    }

    /// Iterates notes with their locations.
    pub fn iter_with_loc(&self) -> impl Iterator<Item = (NoteLoc, NoteView)> + '_ {
        self.chunks.iter().enumerate().flat_map(|(ci, c)| {
            (0..c.len()).map(move |i| {
                (
                    NoteLoc {
                        chunk: ci as u32,
                        index: i as u32,
                    },
                    c.view(i),
                )
            })
        })
    }

    /// B1-0103: a store that replaces one whose revision was `prev` (Change
    /// PPQN swaps whole stores) continues after it, so a cache keyed on the
    /// local revision never sees two states under one number.
    pub(crate) fn continue_revision(&mut self, prev: u64) {
        self.revision = prev.wrapping_add(1).max(self.revision);
    }

    /// Records a selection-only change.
    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Records a change of note values.
    fn bump_content(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.content_revision = fresh_content_revision();
    }

    fn refresh_index(&mut self) {
        self.prefix_max_end.clear();
        let mut m = 0;
        for c in &self.chunks {
            m = m.max(c.max_end);
            self.prefix_max_end.push(m);
        }
        self.len = self.chunks.iter().map(|c| c.len()).sum();
        self.selected = self.chunks.iter().map(|c| c.selected).sum();
    }

    /// Merges chunks that became very small and splits oversized ones.
    fn normalize_chunks(&mut self) {
        let target = self.chunk_target;
        let small = target / 4;
        let mut out: Vec<NoteChunk> = Vec::with_capacity(self.chunks.len());
        for c in self.chunks.drain(..) {
            if c.len() == 0 {
                continue;
            }
            if let Some(last) = out.last_mut()
                && (last.len() < small || c.len() < small)
                && last.len() + c.len() <= target * 2
            {
                last.append(&c);
                continue;
            }
            if c.len() > target * 2 {
                out.extend(c.split_into(target));
            } else {
                out.push(c);
            }
        }
        self.chunks = out;
    }

    // ------------------------------------------------------------------ queries

    /// Calls `visit` for every note overlapping the inclusive tick range
    /// `[t0, t1]` (i.e. `start <= t1 && end >= t0`), in stored order.
    pub fn query<F: FnMut(NoteLoc, NoteView)>(&self, t0: u64, t1: u64, visit: F) -> QueryStats {
        query_chunks(&self.chunks, &self.prefix_max_end, t0, t1, visit)
    }

    /// [`NoteStore::query`]'s notes in stored order until `visit` returns
    /// `false` (L-S46: a preview that has drawn its budget stops reading — the
    /// Import Into preview of 224M notes walked every visible note per frame,
    /// 12.7 s a frame).
    pub fn query_while<F: FnMut(NoteLoc, NoteView) -> bool>(&self, t0: u64, t1: u64, mut visit: F) {
        let chunks = &self.chunks;
        if chunks.is_empty() || t0 > t1 {
            return;
        }
        let hi = chunks.partition_point(|c| c.data.first_start() <= t1);
        let lo = self.prefix_max_end.partition_point(|&m| m < t0);
        for (ci, ch) in chunks.iter().enumerate().take(hi).skip(lo) {
            let c = &ch.data;
            if c.max_end < t0 {
                continue;
            }
            let a = c
                .start
                .partition_point(|&s| s < t0.saturating_sub(c.max_dur));
            let b = c.start.partition_point(|&s| s <= t1);
            for i in a..b {
                let (s, d) = (c.start[i], c.dur[i]);
                if s + d < t0 {
                    continue;
                }
                let v = NoteView {
                    start_tick: s,
                    duration_ticks: d,
                    attrs: c.attrs[i],
                    sub: SubchannelId(c.sub.as_ref().get(i)),
                    group: c.grp.as_ref().get(i),
                    selected: ch.sel.get(i),
                };
                let loc = NoteLoc {
                    chunk: ci as u32,
                    index: i as u32,
                };
                if !visit(loc, v) {
                    return;
                }
            }
        }
    }

    /// [`NoteStore::query`]'s notes in reverse stored order (latest start
    /// first), until `visit` returns `false` (v1.0-beta1 B1-0145: the
    /// Player's lit keys stop once every key is decided).
    pub fn query_rev<F: FnMut(NoteLoc, NoteView) -> bool>(&self, t0: u64, t1: u64, mut visit: F) {
        let chunks = &self.chunks;
        if chunks.is_empty() || t0 > t1 {
            return;
        }
        let hi = chunks.partition_point(|c| c.data.first_start() <= t1);
        let lo = self.prefix_max_end.partition_point(|&m| m < t0);
        for ci in (lo..hi).rev() {
            let ch = &chunks[ci];
            let c = &ch.data;
            if c.max_end < t0 {
                continue;
            }
            let a = c
                .start
                .partition_point(|&s| s < t0.saturating_sub(c.max_dur));
            let b = c.start.partition_point(|&s| s <= t1);
            for i in (a..b).rev() {
                let (s, d) = (c.start[i], c.dur[i]);
                if s + d < t0 {
                    continue;
                }
                let v = NoteView {
                    start_tick: s,
                    duration_ticks: d,
                    attrs: c.attrs[i],
                    sub: SubchannelId(c.sub.as_ref().get(i)),
                    group: c.grp.as_ref().get(i),
                    selected: ch.sel.get(i),
                };
                let loc = NoteLoc {
                    chunk: ci as u32,
                    index: i as u32,
                };
                if !visit(loc, v) {
                    return;
                }
            }
        }
    }

    /// The pitches the store's notes use, as a 256-bit set (`[u64; 4]`, bit
    /// `p % 64` of word `p / 64`). O(notes), reading only the attribute
    /// column.
    pub fn pitch_mask(&self) -> [u64; 4] {
        let mut m = [0u64; 4];
        for c in &self.chunks {
            for a in &c.data.attrs {
                let p = usize::from(a.pitch());
                m[p / 64] |= 1 << (p % 64);
            }
        }
        m
    }

    /// [`NoteStore::pitch_mask`] in pieces (L-S45: a frame never scans a
    /// 224M-note project at once): ORs the pitches of whole chunks from chunk
    /// `from` into `mask` until at least `max_notes` notes were read, and
    /// returns the next chunk to read (`chunk_count()` when done) and the
    /// notes read. Calls from 0 to the end give exactly `pitch_mask()`.
    pub fn pitch_mask_part(
        &self,
        mask: &mut [u64; 4],
        from: usize,
        max_notes: usize,
    ) -> (usize, usize) {
        let mut read = 0usize;
        let mut ci = from;
        while ci < self.chunks.len() && read < max_notes {
            for a in &self.chunks[ci].data.attrs {
                let p = usize::from(a.pitch());
                mask[p / 64] |= 1 << (p % 64);
            }
            read += self.chunks[ci].data.attrs.len();
            ci += 1;
        }
        (ci, read)
    }

    /// The notes that start in `[s0, s1)` and at or before `t1`, and that
    /// reach `t0` once their length `d` is replaced by `len(d)`
    /// (`start + len(d) >= t0`), in stored order, as an iterator borrowing the
    /// chunks (v0.9 V09-S17: the corrected tile fill merges two such runs, as
    /// export does — `docs/PLAYER.md` P3). `len` is an upper bound of what a
    /// length becomes (a gate of 100 % or more: `len(d) >= d`, and `len(d) - d`
    /// never decreases as `d` grows); with `|d| d` this is
    /// [`NoteStore::query`]'s rule limited to the starts `[s0, s1)`. Chunks
    /// are skipped by their own longest note, so one long note costs only its
    /// own chunk.
    pub fn overlapping<L: Fn(u64) -> u64>(
        &self,
        t0: u64,
        t1: u64,
        s0: u64,
        s1: u64,
        len: L,
    ) -> Overlapping<'_, L> {
        let t1 = t1.min(s1.saturating_sub(1));
        let mut it = Overlapping {
            chunks: &self.chunks,
            ci: 0,
            hi: 0,
            i: 0,
            end: 0,
            t0,
            t1,
            s0,
            len,
        };
        if self.chunks.is_empty() || s0 >= s1 || s0 > t1 {
            return it;
        }
        // The longest growth of a length (0 for `|d| d`) bounds how far back a
        // note can start and still reach `t0`.
        let max_dur = self.max_duration();
        let grow = (it.len)(max_dur).saturating_sub(max_dur);
        let by_end = self
            .prefix_max_end
            .partition_point(|&m| m.saturating_add(grow) < t0);
        let by_start = self
            .chunks
            .partition_point(|c| c.start.last().is_some_and(|&s| s < s0));
        it.ci = by_end.max(by_start);
        it.hi = self.chunks.partition_point(|c| c.first_start() <= t1);
        it
    }

    /// Upper bound on the number of notes overlapping `[t0, t1]`: per overlapping
    /// chunk, the notes starting in `[t0 - longest duration, t1]`, found with two
    /// binary searches (no note is visited). Used to pick the rendering level of
    /// detail before doing any per-note work.
    pub fn estimate_in_range(&self, t0: u64, t1: u64) -> usize {
        if self.chunks.is_empty() || t0 > t1 {
            return 0;
        }
        let hi = self.chunks.partition_point(|c| c.first_start() <= t1);
        let lo = self.prefix_max_end.partition_point(|&m| m < t0);
        (lo..hi)
            .map(|ci| {
                let c = &self.chunks[ci];
                if c.max_end < t0 {
                    return 0;
                }
                let lower = t0.saturating_sub(c.max_dur);
                // Chunks entirely inside the range (the common case when zoomed out)
                // need no search.
                if c.start[0] >= lower && c.start[c.len() - 1] <= t1 {
                    return c.len();
                }
                let a = c.start.partition_point(|&s| s < lower);
                let b = c.start.partition_point(|&s| s <= t1);
                b - a
            })
            .sum()
    }

    /// Chunks overlapping `[t0, t1]` as (first start, latest end, notes) —
    /// O(chunks), no note visited: the coarsest occupancy picture (v0.8
    /// dense-view fallback while finer tiles are built).
    pub fn chunk_spans(&self, t0: u64, t1: u64) -> impl Iterator<Item = (u64, u64, usize)> + '_ {
        let hi = self.chunks.partition_point(|c| c.first_start() <= t1);
        let lo = self.prefix_max_end.partition_point(|&m| m < t0).min(hi);
        self.chunks[lo..hi]
            .iter()
            .filter(move |c| c.max_end >= t0)
            .map(|c| (c.first_start(), c.max_end, c.len()))
    }

    /// Number of notes overlapping `[t0, t1]` (convenience for tests and LOD decisions).
    pub fn count_in_range(&self, t0: u64, t1: u64) -> usize {
        let mut n = 0;
        self.query(t0, t1, |_, _| n += 1);
        n
    }

    /// Bounds of the current selection, or `None` if nothing is selected.
    /// O(selected notes); chunks without selected notes are skipped.
    pub fn selection_bounds(&self) -> Option<SelectionBounds> {
        if self.selected == 0 {
            return None;
        }
        let mut b = SelectionBounds {
            count: 0,
            min_start: u64::MAX,
            max_end: 0,
            min_pitch: u8::MAX,
            max_pitch: 0,
            min_duration: u64::MAX,
        };
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            let (st, du, at) = (&c.start[..], &c.dur[..], &c.attrs[..]);
            if c.selected == c.len() {
                // L-S38: a fully selected chunk (Select All) is read column
                // by column, without its bits — the first frame after
                // selecting 10M notes walked them one bit at a time (97 ms).
                b.count += st.len();
                // Starts are sorted; the chunk keeps its notes' largest end
                // (invariant 5).
                b.min_start = b.min_start.min(st[0]);
                b.max_end = b.max_end.max(c.max_end);
                b.min_duration = du.iter().fold(b.min_duration, |m, &d| m.min(d));
                let (mut lo, mut hi) = (b.min_pitch, b.max_pitch);
                for a in at {
                    lo = lo.min(a.pitch());
                    hi = hi.max(a.pitch());
                }
                (b.min_pitch, b.max_pitch) = (lo, hi);
                continue;
            }
            for i in c.sel.iter_ones() {
                let (s, d, p) = (st[i], du[i], at[i].pitch());
                b.count += 1;
                b.min_start = b.min_start.min(s);
                b.max_end = b.max_end.max(s + d);
                b.min_pitch = b.min_pitch.min(p);
                b.max_pitch = b.max_pitch.max(p);
                b.min_duration = b.min_duration.min(d);
            }
        }
        Some(b)
    }

    /// The muted count and the common gate / velocity of the selected notes
    /// (L-S38: the toolbar's numeric fields after Select All of 10M notes —
    /// one note-by-note walk was 85 ms of the first frame). Fully selected
    /// chunks are read by column, and a value stops being compared at its
    /// first difference.
    pub fn selection_uniformity(&self) -> SelectionUniformity {
        let mut u = SelectionUniformity::default();
        let (mut gate, mut vel): (Option<Option<u64>>, Option<Option<u8>>) = (None, None);
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            let (du, at) = (&c.dur[..], &c.attrs[..]);
            if c.selected == c.len() {
                u.muted += at.iter().filter(|a| a.muted()).count();
                let g = *gate.get_or_insert(Some(du[0]));
                if let Some(g) = g
                    && du.iter().any(|&d| d != g)
                {
                    gate = Some(None);
                }
                let v = *vel.get_or_insert(Some(at[0].velocity()));
                if let Some(v) = v
                    && at.iter().any(|a| a.velocity() != v)
                {
                    vel = Some(None);
                }
                continue;
            }
            for i in c.sel.iter_ones() {
                let a = at[i];
                u.muted += usize::from(a.muted());
                let g = *gate.get_or_insert(Some(du[i]));
                if g.is_some_and(|g| g != du[i]) {
                    gate = Some(None);
                }
                let v = *vel.get_or_insert(Some(a.velocity()));
                if v.is_some_and(|v| v != a.velocity()) {
                    vel = Some(None);
                }
            }
        }
        u.gate = gate.flatten();
        u.velocity = vel.flatten();
        u
    }

    /// Visits selected notes in stored order.
    pub fn for_each_selected<F: FnMut(NoteView)>(&self, mut f: F) {
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            let subs = c.sub.as_ref();
            let grps = c.grp.as_ref();
            let (st, du, at) = (&c.start[..], &c.dur[..], &c.attrs[..]);
            let main = matches!((subs, grps), (SubRef::Main, SubRef::Main));
            if c.selected == c.len() && main {
                // L-S38: a fully selected chunk without subchannel or group
                // columns is visited in order without reading its bits.
                for ((&s, &d), &a) in st.iter().zip(du).zip(at) {
                    f(NoteView {
                        start_tick: s,
                        duration_ticks: d,
                        attrs: a,
                        sub: SubchannelId::MAIN,
                        group: 0,
                        selected: true,
                    });
                }
            } else if main {
                // The common case: no subchannel or group column.
                for i in c.sel.iter_ones() {
                    f(NoteView {
                        start_tick: st[i],
                        duration_ticks: du[i],
                        attrs: at[i],
                        sub: SubchannelId::MAIN,
                        group: 0,
                        selected: true,
                    });
                }
            } else {
                for i in c.sel.iter_ones() {
                    f(NoteView {
                        start_tick: st[i],
                        duration_ticks: du[i],
                        attrs: at[i],
                        sub: SubchannelId(subs.get(i)),
                        group: grps.get(i),
                        selected: true,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------ insertion

    fn check_record(rec: &NoteRecord) -> Result<(), NoteStoreError> {
        if rec.end_tick().is_none() {
            return Err(NoteStoreError::EndOverflow {
                start_tick: rec.start_tick,
                duration_ticks: rec.duration_ticks,
            });
        }
        Ok(())
    }

    /// Inserts one note after any existing notes with the same start tick.
    pub fn insert(&mut self, rec: NoteRecord, selected: bool) -> Result<NoteLoc, NoteStoreError> {
        Self::check_record(&rec)?;
        let target = self.chunk_target;
        let (ci, idx) = if self.chunks.is_empty() {
            // Grow on demand: an empty track must not reserve a full chunk up front.
            let mut c = NoteChunk::with_capacity(4);
            c.push(
                rec.start_tick,
                rec.duration_ticks,
                rec.attrs,
                rec.sub.0,
                rec.group,
                selected,
            );
            self.chunks.push(c);
            (0, 0)
        } else {
            let ci = self
                .chunks
                .partition_point(|c| c.first_start() <= rec.start_tick)
                .saturating_sub(1);
            let chunk = &mut self.chunks[ci];
            let idx = chunk.start.partition_point(|&s| s <= rec.start_tick);
            chunk.insert_at(idx, rec, selected);
            if chunk.len() > target * 2 {
                let c = std::mem::take(&mut self.chunks[ci]);
                let pieces = c.split_into(target);
                self.chunks.splice(ci..=ci, pieces);
                // Locate the note again in the split pieces.
                let mut remaining = idx;
                let mut cj = ci;
                while remaining >= self.chunks[cj].len() {
                    remaining -= self.chunks[cj].len();
                    cj += 1;
                }
                (cj, remaining)
            } else {
                (ci, idx)
            }
        };
        self.refresh_index();
        self.bump_content();
        Ok(NoteLoc {
            chunk: ci as u32,
            index: idx as u32,
        })
    }

    /// Inserts many notes (sorted by start tick) with a per-chunk merge. Each affected
    /// chunk is merged once, so the cost is O(affected chunk sizes + records) rather
    /// than one array shift per note. Validation happens before any mutation.
    pub fn insert_sorted_batch(
        &mut self,
        records: &[NoteRecord],
        selected: bool,
    ) -> Result<(), NoteStoreError> {
        for w in records.windows(2) {
            if w[0].start_tick > w[1].start_tick {
                return Err(NoteStoreError::UnsortedBatch);
            }
        }
        for r in records {
            Self::check_record(r)?;
        }
        if records.is_empty() {
            return Ok(());
        }
        let target = self.chunk_target;
        let old = std::mem::take(&mut self.chunks);
        let mut out: Vec<NoteChunk> = Vec::with_capacity(old.len() + records.len() / target + 1);
        if old.is_empty() {
            let mut c = ChunkBuilder::with_capacity(records.len().min(target));
            for r in records {
                if c.len() == target {
                    out.push(
                        std::mem::replace(&mut c, ChunkBuilder::with_capacity(target)).finish(),
                    );
                }
                c.push(
                    r.start_tick,
                    r.duration_ticks,
                    r.attrs,
                    r.sub.0,
                    r.group,
                    selected,
                );
            }
            out.push(c.finish());
        } else {
            let first_starts: Vec<u64> = old.iter().map(|c| c.first_start()).collect();
            let n_old = old.len();
            let mut r = 0usize;
            for (ci, chunk) in old.into_iter().enumerate() {
                let end_r = if ci + 1 < n_old {
                    let limit = first_starts[ci + 1];
                    r + records[r..].partition_point(|x| x.start_tick < limit)
                } else {
                    records.len()
                };
                if end_r == r {
                    out.push(chunk);
                    continue;
                }
                out.extend(merge_chunk_pieces(
                    &chunk,
                    &records[r..end_r],
                    selected,
                    target,
                ));
                r = end_r;
            }
            debug_assert_eq!(r, records.len());
        }
        self.chunks = out;
        self.refresh_index();
        self.bump_content();
        Ok(())
    }

    /// Appends notes that start at or after every stored note (bulk loading of
    /// native projects). `remaining_hint` is how many notes are still expected
    /// including `records`; it sizes the last chunk exactly so no slack is left.
    /// Validation happens before any mutation.
    pub fn append_sorted(
        &mut self,
        records: &[NoteRecord],
        remaining_hint: usize,
    ) -> Result<(), NoteStoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let last = self.chunks.last().and_then(|c| c.start.last().copied());
        if last.is_some_and(|l| l > records[0].start_tick)
            || records
                .windows(2)
                .any(|w| w[0].start_tick > w[1].start_tick)
        {
            return Err(NoteStoreError::UnsortedBatch);
        }
        for r in records {
            Self::check_record(r)?;
        }
        let target = self.chunk_target;
        let hint = remaining_hint.max(records.len());
        let mut i = 0;
        while i < records.len() {
            if self.chunks.last().is_none_or(|c| c.len() >= target) {
                let cap = target.min(hint - i).max(1);
                self.chunks.push(NoteChunk::with_capacity(cap));
            }
            let c = self.chunks.last_mut().expect("pushed above");
            let take = (target - c.len()).min(records.len() - i);
            c.extend_unselected(&records[i..i + take]);
            i += take;
        }
        self.refresh_index();
        self.bump_content();
        Ok(())
    }

    // -------------------------------------------------------------- removal

    /// Deletes all selected notes with batch compaction. Returns the number removed.
    pub fn remove_selected(&mut self) -> usize {
        if self.selected == 0 {
            return 0;
        }
        let mut removed = 0;
        for c in &mut self.chunks {
            if c.selected > 0 {
                removed += c.remove_selected_into(None);
            }
        }
        self.normalize_chunks();
        self.refresh_index();
        self.bump_content();
        removed
    }

    /// Removes selected notes and returns them in stored order.
    pub fn extract_selected(&mut self) -> Result<Vec<NoteRecord>, NoteStoreError> {
        let mut out = Vec::new();
        out.try_reserve_exact(self.selected)
            .map_err(|_| NoteStoreError::OutOfMemory {
                bytes: self
                    .selected
                    .saturating_mul(std::mem::size_of::<NoteRecord>()),
            })?;
        if self.selected == 0 {
            return Ok(out);
        }
        for c in &mut self.chunks {
            if c.selected > 0 {
                c.remove_selected_into(Some(&mut out));
            }
        }
        self.normalize_chunks();
        self.refresh_index();
        self.bump_content();
        Ok(out)
    }

    /// Removes the notes at the given locations (used by the importer to drop
    /// unmatched note-ons). Locations must be from the current revision.
    /// Clears any existing selection.
    pub(crate) fn remove_locs(&mut self, locs: &[NoteLoc]) -> usize {
        if locs.is_empty() {
            return 0;
        }
        // Reuse selection-based compaction: clear selection, mark targets, remove.
        self.clear_selection();
        for loc in locs {
            let c = &mut self.chunks[loc.chunk as usize];
            if c.sel.set(loc.index as usize, true) {
                c.selected += 1;
            }
        }
        self.refresh_index();
        self.remove_selected()
    }

    // ------------------------------------------------------------- selection

    pub fn select_all(&mut self) {
        for c in &mut self.chunks {
            if c.selected < c.len() {
                c.selected = c.sel.fill(true);
            }
        }
        self.selected = self.len;
        self.bump();
    }

    pub fn clear_selection(&mut self) {
        if self.selected == 0 {
            return;
        }
        for c in &mut self.chunks {
            if c.selected > 0 {
                c.sel.fill(false);
                c.selected = 0;
            }
        }
        self.selected = 0;
        self.bump();
    }

    /// Sets the selection state of one note. Returns false if `loc` is invalid.
    pub fn set_selected(&mut self, loc: NoteLoc, value: bool) -> bool {
        let Some(c) = self.chunks.get_mut(loc.chunk as usize) else {
            return false;
        };
        let i = loc.index as usize;
        if i >= c.len() {
            return false;
        }
        if c.sel.get(i) == value {
            return true;
        }
        if c.sel.set(i, value) {
            if value {
                c.selected += 1;
                self.selected += 1;
            } else {
                c.selected -= 1;
                self.selected -= 1;
            }
            self.bump();
        }
        true
    }

    pub fn toggle_selected(&mut self, loc: NoteLoc) -> bool {
        match self.get(loc) {
            Some(v) => self.set_selected(loc, !v.selected),
            None => false,
        }
    }

    /// Selects every note overlapping ticks `[t0, t1]` whose pitch is within
    /// `[pitch_lo, pitch_hi]`. Uses the same bounded search as [`NoteStore::query`].
    /// Returns the number of notes newly selected.
    pub fn select_in_region(&mut self, t0: u64, t1: u64, pitch_lo: u8, pitch_hi: u8) -> usize {
        if self.chunks.is_empty() || t0 > t1 || pitch_lo > pitch_hi {
            return 0;
        }
        let hi = self.chunks.partition_point(|c| c.first_start() <= t1);
        let lo = self.prefix_max_end.partition_point(|&m| m < t0);
        let mut newly = 0;
        for ci in lo..hi {
            if self.chunks[ci].max_end < t0 || self.chunks[ci].selected == self.chunks[ci].len() {
                continue;
            }
            let c = &mut self.chunks[ci];
            let lower = t0.saturating_sub(c.max_dur);
            let a = c.start.partition_point(|&s| s < lower);
            let b = c.start.partition_point(|&s| s <= t1);
            for i in a..b {
                let p = c.attrs[i].pitch();
                if c.start[i] + c.dur[i] >= t0
                    && p >= pitch_lo
                    && p <= pitch_hi
                    && c.sel.set(i, true)
                {
                    c.selected += 1;
                    newly += 1;
                }
            }
        }
        if newly > 0 {
            self.selected += newly;
            self.bump();
        }
        newly
    }

    /// Adds to the selection the notes in the half-open tick range `[t0, t1)`:
    /// by onset (`t0 <= start < t1`), or, when `intersecting`, every note that
    /// sounds inside it (`start < t1` and `end > t0`; a zero-length note counts
    /// when it starts inside). Bounded like [`NoteStore::query`]; returns the
    /// number of notes newly selected.
    pub fn select_range(&mut self, t0: u64, t1: u64, intersecting: bool) -> usize {
        if self.chunks.is_empty() || t0 >= t1 {
            return 0;
        }
        let hi = self.chunks.partition_point(|c| c.first_start() < t1);
        let lo = if intersecting {
            self.prefix_max_end.partition_point(|&m| m <= t0)
        } else {
            self.chunks
                .partition_point(|c| c.start.last().is_some_and(|&l| l < t0))
        };
        let mut newly = 0;
        for ci in lo..hi {
            let c = &mut self.chunks[ci];
            if c.selected == c.len() {
                continue;
            }
            let len = c.len();
            if len == 0 || (!intersecting && c.start[len - 1] < t0) {
                continue;
            }
            let lower = if intersecting {
                t0.saturating_sub(c.max_dur)
            } else {
                t0
            };
            let a = c.start.partition_point(|&s| s < lower);
            let b = c.start.partition_point(|&s| s < t1);
            for i in a..b {
                let s = c.start[i];
                let inside = s >= t0 || (intersecting && s.saturating_add(c.dur[i]) > t0);
                if inside && c.sel.set(i, true) {
                    c.selected += 1;
                    newly += 1;
                }
            }
        }
        if newly > 0 {
            self.selected += newly;
            self.bump();
        }
        newly
    }

    // ------------------------------------------------------- in-place edits

    /// Rewrites attributes of selected notes in place. Pitch-only transforms do not
    /// change time ordering, so no index rebuild is needed.
    pub fn map_selected_attrs<F: FnMut(NoteAttrs) -> NoteAttrs>(&mut self, mut f: F) {
        if self.selected == 0 {
            return;
        }
        for c in &mut self.chunks {
            if c.selected == 0 {
                continue;
            }
            let NoteChunk { data, sel, .. } = c;
            let d = Arc::make_mut(data);
            for i in sel.iter_ones() {
                d.attrs[i] = f(d.attrs[i]);
            }
        }
        self.bump_content();
    }

    /// Like [`NoteStore::map_selected_attrs`], but also passes the note's position
    /// among the selected notes (0, 1, 2, … in stored order).
    pub fn map_selected_attrs_indexed<F: FnMut(usize, NoteAttrs) -> NoteAttrs>(
        &mut self,
        mut f: F,
    ) {
        if self.selected == 0 {
            return;
        }
        let mut k = 0usize;
        for c in &mut self.chunks {
            if c.selected == 0 {
                continue;
            }
            let NoteChunk { data, sel, .. } = c;
            let d = Arc::make_mut(data);
            for i in sel.iter_ones() {
                d.attrs[i] = f(k, d.attrs[i]);
                k += 1;
            }
        }
        self.bump_content();
    }

    /// Adds `delta` ticks to the duration of every selected note. Fails without
    /// modifying anything if any resulting duration would be negative or overflow.
    pub fn add_selected_duration(&mut self, delta: i64) -> Result<(), NoteStoreError> {
        if self.selected == 0 || delta == 0 {
            return Ok(());
        }
        let apply = |s: u64, d: u64| -> Option<u64> {
            let nd = if delta >= 0 {
                d.checked_add(delta as u64)?
            } else {
                d.checked_sub(delta.unsigned_abs())?
            };
            s.checked_add(nd)?;
            Some(nd)
        };
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            for i in c.sel.iter_ones() {
                if apply(c.start[i], c.dur[i]).is_none() {
                    return Err(NoteStoreError::DurationOutOfRange);
                }
            }
        }
        for c in &mut self.chunks {
            if c.selected == 0 {
                continue;
            }
            let NoteChunk { data, sel, .. } = c;
            let d = Arc::make_mut(data);
            for i in sel.iter_ones() {
                // Validated above; cannot fail.
                if let Some(nd) = apply(d.start[i], d.dur[i]) {
                    d.dur[i] = nd;
                }
            }
            d.recompute_meta();
        }
        self.refresh_index();
        self.bump_content();
        Ok(())
    }

    /// The durations of the selected notes (stored order) as LEB128 varints, for
    /// an undo entry.
    pub fn selected_durations_encoded(&self) -> Result<Vec<u8>, NoteStoreError> {
        let mut out = Vec::new();
        out.try_reserve(self.selected.saturating_mul(2))
            .map_err(|_| NoteStoreError::OutOfMemory {
                bytes: self.selected.saturating_mul(2),
            })?;
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            for i in c.sel.iter_ones() {
                crate::codec::put_uvarint(&mut out, c.dur[i]);
            }
        }
        out.shrink_to_fit();
        Ok(out)
    }

    /// Sets the duration of every selected note to `duration`. Validated first:
    /// fails without changing anything if any note's end would overflow. Returns
    /// whether anything changed.
    pub fn set_selected_durations(&mut self, duration: u64) -> Result<bool, NoteStoreError> {
        if self.selected == 0 {
            return Ok(false);
        }
        let mut changes = false;
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            for i in c.sel.iter_ones() {
                if c.start[i].checked_add(duration).is_none() {
                    return Err(NoteStoreError::EndOverflow {
                        start_tick: c.start[i],
                        duration_ticks: duration,
                    });
                }
                changes |= c.dur[i] != duration;
            }
        }
        if !changes {
            return Ok(false);
        }
        for c in &mut self.chunks {
            if c.selected == 0 {
                continue;
            }
            let NoteChunk { data, sel, .. } = c;
            let d = Arc::make_mut(data);
            for i in sel.iter_ones() {
                d.dur[i] = duration;
            }
            d.recompute_meta();
        }
        self.refresh_index();
        self.bump_content();
        Ok(true)
    }

    /// Restores the durations recorded by [`NoteStore::selected_durations_encoded`]
    /// onto the selected notes (undo). Validated before anything changes.
    pub fn restore_selected_durations(&mut self, encoded: &[u8]) -> Result<(), PatchError> {
        let mut pos = 0usize;
        let mut values: Vec<u64> = Vec::new();
        values
            .try_reserve_exact(self.selected)
            .map_err(|_| PatchError::Store(NoteStoreError::OutOfMemory { bytes: 0 }))?;
        for c in &self.chunks {
            if c.selected == 0 {
                continue;
            }
            for i in c.sel.iter_ones() {
                let d = crate::codec::get_uvarint(encoded, &mut pos)
                    .ok_or(PatchError::CountMismatch)?;
                if c.start[i].checked_add(d).is_none() {
                    return Err(PatchError::OrderViolation);
                }
                values.push(d);
            }
        }
        if pos != encoded.len() {
            return Err(PatchError::CountMismatch);
        }
        let mut k = 0usize;
        for c in &mut self.chunks {
            if c.selected == 0 {
                continue;
            }
            let NoteChunk { data, sel, .. } = c;
            let d = Arc::make_mut(data);
            for i in sel.iter_ones() {
                d.dur[i] = values[k];
                k += 1;
            }
            d.recompute_meta();
        }
        self.refresh_index();
        self.bump_content();
        Ok(())
    }

    /// Removes the notes at `ranks` and returns them, keeping the selection of
    /// the other notes (the Erase tool).
    pub fn remove_ranks_keeping_selection(
        &mut self,
        ranks: &RankSet,
    ) -> Result<CompactRecords, PatchError> {
        if ranks.end() > self.len {
            return Err(PatchError::RankOutOfRange);
        }
        let previously = self.selected_ranks();
        self.select_ranks(ranks)?;
        let (_, removed) = self.try_selected_with_ranks().map_err(PatchError::Store)?;
        self.remove_selected();
        // Re-select the surviving notes of the old selection at their new ranks.
        if !previously.is_empty() {
            let mut kept = RankSetBuilder::default();
            let mut erased = ranks.iter().peekable();
            let mut shift = 0usize;
            for r in previously.iter() {
                while let Some(&e) = erased.peek() {
                    if e < r {
                        shift += 1;
                        erased.next();
                    } else {
                        break;
                    }
                }
                if erased.peek() == Some(&r) {
                    continue; // this selected note was erased
                }
                kept.push(r - shift);
            }
            self.select_ranks(&kept.finish())?;
        }
        Ok(removed)
    }

    /// Notes that **start** in `[t0, t1]` (optionally only selected ones, or only
    /// one channel): their ranks and start ticks, in stored order. Visits only
    /// the chunks that overlap the range (plus an O(chunks) walk of lengths).
    pub fn starting_in(
        &self,
        t0: u64,
        t1: u64,
        selected_only: bool,
        channel: Option<u8>,
    ) -> (RankSet, Vec<u64>) {
        let mut ranks = RankSetBuilder::default();
        let mut starts = Vec::new();
        let mut base = 0usize;
        for c in &self.chunks {
            let len = c.len();
            if len == 0 || c.start[len - 1] < t0 {
                base += len;
                continue;
            }
            if c.start[0] > t1 || t0 > t1 {
                break;
            }
            let a = c.start.partition_point(|&s| s < t0);
            let b = c.start.partition_point(|&s| s <= t1);
            for i in a..b {
                if selected_only && !c.sel.get(i) {
                    continue;
                }
                if channel.is_some_and(|ch| c.attrs[i].channel() != ch) {
                    continue;
                }
                ranks.push(base + i);
                starts.push(c.start[i]);
            }
            base += len;
        }
        (ranks.finish(), starts)
    }

    /// Global rank of the note at `loc` (its position in stored order). O(chunks).
    pub fn rank_of(&self, loc: NoteLoc) -> Option<usize> {
        let ci = loc.chunk as usize;
        let c = self.chunks.get(ci)?;
        if loc.index as usize >= c.len() {
            return None;
        }
        Some(self.chunks[..ci].iter().map(|c| c.len()).sum::<usize>() + loc.index as usize)
    }

    // ------------------------------------------------------------ subchannels

    /// Puts every note into `map[channel]` (its MIDI channel's subchannel). Chunks
    /// whose notes all map to the main subchannel keep no column. O(notes).
    pub fn assign_subs_by_channel(&mut self, map: &[u32; 16]) {
        let max = map.iter().copied().max().unwrap_or(0);
        for c in &mut self.chunks {
            let needs =
                !c.sub.is_empty() || c.attrs.iter().any(|a| map[usize::from(a.channel())] != 0);
            if !needs {
                continue;
            }
            let d = c.data_mut();
            d.sub = SubColumn::from_values(
                d.attrs.iter().map(|a| map[usize::from(a.channel())]),
                d.attrs.len(),
                max,
            );
            d.compact_sub();
        }
        self.bump_content();
    }

    /// Bit `c` is set when a note uses MIDI channel `c` (reads only the
    /// attribute column; stops early once all 16 are seen).
    pub fn channels_used(&self) -> u16 {
        let mut used = 0u16;
        for c in &self.chunks {
            for a in &c.attrs {
                used |= 1 << a.channel();
            }
            if used == u16::MAX {
                break;
            }
        }
        used
    }

    /// Number of notes in subchannel `sub`, and whether any of them is selected.
    /// O(notes); used for confirmations, not per frame.
    pub fn count_in_sub(&self, sub: SubchannelId) -> usize {
        let mut n = 0;
        for c in &self.chunks {
            if c.sub.is_empty() {
                if sub.0 == 0 {
                    n += c.len();
                }
            } else {
                n += c.sub.count_eq(sub.0);
            }
        }
        n
    }

    /// Every subchannel id used by a note (sorted, unique). O(notes).
    pub fn used_subs(&self) -> Vec<SubchannelId> {
        let mut out: Vec<u32> = Vec::new();
        let mut zero = false;
        // Ids below 256 (one-byte columns): a bit table instead of sorting.
        let mut small = [0u64; 4];
        let mut local: Vec<u32> = Vec::new();
        for c in &self.chunks {
            match c.sub.as_ref() {
                SubRef::Main => zero |= c.len() > 0,
                SubRef::W8(v) => {
                    for &x in v {
                        small[usize::from(x) / 64] |= 1 << (x % 64);
                    }
                }
                _ => {
                    local.clear();
                    local.extend(c.sub.iter());
                    local.sort_unstable();
                    local.dedup();
                    out.extend_from_slice(&local);
                }
            }
        }
        if zero {
            out.push(0);
        }
        for (w, &word) in small.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                out.push((w * 64) as u32 + bits.trailing_zeros());
                bits &= bits - 1;
            }
        }
        out.sort_unstable();
        out.dedup();
        out.into_iter().map(SubchannelId).collect()
    }

    /// Exact number of notes that **start** in `[t0, t1]`: two binary searches in
    /// the boundary chunks, whole lengths in between. O(log chunks + chunks in
    /// range); no note is visited. (Onion Skin density ranking.)
    pub fn count_starting_in(&self, t0: u64, t1: u64) -> usize {
        if t0 > t1 || self.chunks.is_empty() {
            return 0;
        }
        // First chunk whose last start is >= t0.
        let first = self
            .chunks
            .partition_point(|c| c.start.last().is_some_and(|&l| l < t0));
        let mut n = 0;
        for c in &self.chunks[first..] {
            if c.first_start() > t1 {
                break;
            }
            let a = if c.first_start() >= t0 {
                0
            } else {
                c.start.partition_point(|&s| s < t0)
            };
            let b = if c.start.last().is_some_and(|&l| l <= t1) {
                c.len()
            } else {
                c.start.partition_point(|&s| s <= t1)
            };
            n += b - a;
        }
        n
    }

    /// Every (MIDI channel, subchannel) combination used by a note, sorted and
    /// unique. O(notes); chunks without a subchannel column cost one pass over
    /// their attributes.
    pub fn channel_sub_pairs(&self) -> Vec<(u8, u32)> {
        channel_sub_pairs_of(self.chunks.iter().map(|c| (&**c, 0, c.len())))
    }

    /// Replaces the selection with the notes of subchannel `sub`. Returns how
    /// many notes are selected.
    pub fn select_sub(&mut self, sub: SubchannelId) -> usize {
        let mut total = 0;
        for c in &mut self.chunks {
            let len = c.len();
            let mut sel = BitSet::filled(len, false);
            let mut k = 0;
            if c.sub.is_empty() {
                if sub.0 == 0 {
                    sel = BitSet::filled(len, true);
                    k = len;
                }
            } else {
                for i in 0..len {
                    if c.sub.get(i) == sub.0 {
                        sel.set(i, true);
                        k += 1;
                    }
                }
            }
            c.sel = sel;
            c.selected = k;
            total += k;
        }
        self.selected = total;
        self.bump();
        total
    }

    /// Adds to the selection every note whose group id satisfies `f` (v0.7:
    /// clicking a grouped note selects its whole group). Chunks without a group
    /// column are skipped unless `f(0)`. Returns the number of notes added.
    pub fn select_groups(&mut self, f: impl Fn(u32) -> bool) -> usize {
        let zero = f(0);
        let mut added = 0;
        for c in &mut self.chunks {
            let len = c.len();
            if c.grp.is_empty() && !zero {
                continue;
            }
            let mut k = 0;
            for i in 0..len {
                if !c.sel.get(i) && f(c.grp.get(i)) {
                    c.sel.set(i, true);
                    k += 1;
                }
            }
            c.selected += k;
            added += k;
        }
        if added > 0 {
            self.selected += added;
            self.bump();
        }
        added
    }

    /// The subchannel of every selected note, in stored order (undo data).
    pub fn selected_subs(&self) -> Result<Vec<u32>, NoteStoreError> {
        let mut out = Vec::new();
        out.try_reserve_exact(self.selected)
            .map_err(|_| NoteStoreError::OutOfMemory {
                bytes: self.selected.saturating_mul(4),
            })?;
        for c in &self.chunks {
            if c.selected > 0 {
                for i in c.sel.iter_ones() {
                    out.push(c.sub_raw(i));
                }
            }
        }
        Ok(out)
    }

    /// Sets the subchannel of the selected notes: `value(k)` for the k-th
    /// selected note in stored order. Only chunks with selected notes change.
    pub fn map_selected_subs<F: FnMut(usize) -> u32>(&mut self, mut value: F) {
        let mut k = 0usize;
        for c in &mut self.chunks {
            if c.selected == 0 {
                continue;
            }
            let ones: Vec<usize> = c.sel.iter_ones().collect();
            let d = c.data_mut();
            for i in ones {
                let v = value(k);
                k += 1;
                if d.sub.is_empty() {
                    if v == 0 {
                        continue;
                    }
                    let n = d.start.len();
                    d.sub.reserve_exact(n);
                    d.sub.resize_zero(n);
                }
                d.sub.set(i, v);
            }
            d.compact_sub();
        }
        self.bump_content();
    }

    // ------------------------------------------------------------ snapshots

    /// Immutable view of the notes that shares chunk memory with this store (O(chunks)
    /// pointer copies). Later edits copy only the chunks they change.
    pub fn snapshot(&self) -> NoteSnapshot {
        NoteSnapshot {
            chunks: self.chunks.iter().map(|c| Arc::clone(&c.data)).collect(),
            prefix_max_end: self.prefix_max_end.clone(),
            len: self.len,
        }
    }

    // ------------------------------------------------- rank-addressed patches
    //
    // A note's *rank* is its global position in stored order. Undo/redo records
    // edits by rank, which is exact (it also restores the relative order of notes
    // that start on the same tick) and needs no per-note identifiers.

    /// The selected notes and their ranks, in stored order.
    pub fn selected_with_ranks(&self) -> (RankSet, CompactRecords) {
        self.try_selected_with_ranks()
            .unwrap_or_else(|_| (RankSet::new(), CompactRecords::new()))
    }

    /// Like [`NoteStore::selected_with_ranks`], but reports allocation failure.
    pub fn try_selected_with_ranks(&self) -> Result<(RankSet, CompactRecords), NoteStoreError> {
        let mut ranks = RankSetBuilder::default();
        let mut recs = CompactRecords::try_with_capacity_notes(self.selected).map_err(|_| {
            NoteStoreError::OutOfMemory {
                bytes: self.selected.saturating_mul(7),
            }
        })?;
        let mut offset = 0usize;
        for c in &self.chunks {
            if c.selected > 0 {
                let subs = c.sub.as_ref();
                let grps = c.grp.as_ref();
                let plain = matches!((subs, grps), (SubRef::Main, SubRef::Main));
                for i in c.sel.iter_ones() {
                    ranks.push(offset + i);
                    let (sub, grp) = if plain {
                        (0, 0)
                    } else {
                        (subs.get(i), grps.get(i))
                    };
                    // Stored order is sorted by start, so this cannot fail.
                    let ok = recs.push_parts(c.start[i], c.dur[i], c.attrs[i], sub, grp);
                    debug_assert!(ok);
                }
            }
            offset += c.len();
        }
        recs.shrink();
        Ok((ranks.finish(), recs))
    }

    /// One byte per selected note (in stored order) computed by `f`, e.g. the
    /// pitches or velocities that an undo entry must remember.
    pub fn selected_attr_bytes<F: Fn(NoteAttrs) -> u8>(
        &self,
        f: F,
    ) -> Result<Vec<u8>, NoteStoreError> {
        let mut out = Vec::new();
        out.try_reserve_exact(self.selected)
            .map_err(|_| NoteStoreError::OutOfMemory {
                bytes: self.selected,
            })?;
        for c in &self.chunks {
            if c.selected > 0 {
                for i in c.sel.iter_ones() {
                    out.push(f(c.attrs[i]));
                }
            }
        }
        Ok(out)
    }

    /// Ranks of the selected notes.
    pub fn selected_ranks(&self) -> RankSet {
        let mut ranks = RankSetBuilder::default();
        let mut offset = 0usize;
        for c in &self.chunks {
            if c.selected == c.len() {
                ranks.push_run(offset, c.len());
            } else if c.selected > 0 {
                for i in c.sel.iter_ones() {
                    ranks.push(offset + i);
                }
            }
            offset += c.len();
        }
        ranks.finish()
    }

    /// Makes exactly the notes at `ranks` selected.
    pub fn select_ranks(&mut self, ranks: &RankSet) -> Result<(), PatchError> {
        if ranks.end() > self.len {
            return Err(PatchError::RankOutOfRange);
        }
        self.clear_selection();
        let mut ci = 0usize;
        let mut offset = 0usize;
        for (start, len) in ranks.runs() {
            let mut r = start;
            let end = start + len;
            while r < end {
                while offset + self.chunks[ci].len() <= r {
                    offset += self.chunks[ci].len();
                    ci += 1;
                }
                let chunk_end = offset + self.chunks[ci].len();
                let stop = end.min(chunk_end);
                let c = &mut self.chunks[ci];
                for i in (r - offset)..(stop - offset) {
                    if c.sel.set(i, true) {
                        c.selected += 1;
                    }
                }
                r = stop;
            }
        }
        self.selected = self.chunks.iter().map(|c| c.selected).sum();
        self.bump();
        Ok(())
    }

    /// Removes the notes at `ranks`.
    pub fn remove_ranks(&mut self, ranks: &RankSet) -> Result<usize, PatchError> {
        self.select_ranks(ranks)?;
        Ok(self.remove_selected())
    }

    /// Inserts `records` so that the k-th record ends up at global position
    /// `ranks[k]`, as the only selected notes. This is the exact inverse of
    /// [`NoteStore::remove_ranks`]: it restores notes at their original positions,
    /// including the order of notes that start on the same tick.
    ///
    /// Validated before anything changes: counts must match, positions must fit, and
    /// the result must stay sorted by start tick.
    pub fn insert_at_ranks(
        &mut self,
        ranks: &RankSet,
        records: &CompactRecords,
    ) -> Result<(), PatchError> {
        if ranks.len() != records.len() {
            return Err(PatchError::CountMismatch);
        }
        let total = self.len + ranks.len();
        if ranks.end() > total {
            return Err(PatchError::RankOutOfRange);
        }
        if ranks.is_empty() {
            self.clear_selection();
            return Ok(());
        }
        let target = self.chunk_target;
        // (position among existing notes before which the record goes, record)
        let mut inserts: Vec<(usize, NoteRecord)> = Vec::new();
        inserts
            .try_reserve_exact(ranks.len())
            .map_err(|_| PatchError::Store(NoteStoreError::OutOfMemory { bytes: 0 }))?;
        for (k, (rank, rec)) in ranks.iter().zip(records.iter()).enumerate() {
            Self::check_record(&rec).map_err(PatchError::Store)?;
            inserts.push((rank - k, rec));
        }
        if inserts.len() != ranks.len() {
            return Err(PatchError::CountMismatch);
        }
        let mut out: Vec<NoteChunk> = Vec::with_capacity(self.chunks.len() + 1);
        let mut j = 0usize;
        if self.chunks.is_empty() {
            let mut c = ChunkBuilder::with_capacity(inserts.len().min(target));
            for (_, r) in &inserts {
                if c.len() == target {
                    out.push(
                        std::mem::replace(&mut c, ChunkBuilder::with_capacity(target)).finish(),
                    );
                }
                c.push(
                    r.start_tick,
                    r.duration_ticks,
                    r.attrs,
                    r.sub.0,
                    r.group,
                    true,
                );
            }
            out.push(c.finish());
            j = inserts.len();
        } else {
            let n_chunks = self.chunks.len();
            let mut offset = 0usize;
            for (ci, chunk) in self.chunks.iter().enumerate() {
                let len = chunk.len();
                let last = ci + 1 == n_chunks;
                let limit = if last { offset + len } else { offset + len - 1 };
                let from = j;
                while j < inserts.len() && inserts[j].0 <= limit {
                    j += 1;
                }
                if from == j {
                    // Nothing goes into this chunk: share its notes and only
                    // drop its selection (the inserted notes become the
                    // selection) instead of rebuilding it (B1-0164).
                    let mut c = chunk.clone();
                    if c.selected > 0 {
                        c.sel.fill(false);
                        c.selected = 0;
                    }
                    out.push(c);
                    offset += len;
                    continue;
                }
                let mut merged = Pieces::new(len + (j - from), target);
                let mut k = from;
                for i in 0..len {
                    while k < j && inserts[k].0 == offset + i {
                        let r = inserts[k].1;
                        merged.push(
                            r.start_tick,
                            r.duration_ticks,
                            r.attrs,
                            r.sub.0,
                            r.group,
                            true,
                        );
                        k += 1;
                    }
                    merged.push(
                        chunk.start[i],
                        chunk.dur[i],
                        chunk.attrs[i],
                        chunk.sub_raw(i),
                        chunk.grp_raw(i),
                        false,
                    );
                }
                while k < j {
                    let r = inserts[k].1;
                    merged.push(
                        r.start_tick,
                        r.duration_ticks,
                        r.attrs,
                        r.sub.0,
                        r.group,
                        true,
                    );
                    k += 1;
                }
                out.extend(merged.finish());
                offset += len;
            }
        }
        if j != inserts.len() {
            return Err(PatchError::RankOutOfRange);
        }
        // The result must still be sorted by start (a mismatched patch would break
        // the store's invariants); nothing has been modified yet.
        let mut prev = 0u64;
        for c in &out {
            if c.first_start() < prev || !c.start.is_sorted() {
                return Err(PatchError::OrderViolation);
            }
            prev = *c.start.last().unwrap_or(&prev);
        }
        self.chunks = out;
        self.normalize_chunks();
        self.refresh_index();
        self.bump_content();
        Ok(())
    }

    /// Notes at `ranks` (tests and verification).
    pub fn records_at_ranks(&self, ranks: &RankSet) -> Vec<NoteRecord> {
        let mut out = Vec::with_capacity(ranks.len());
        let mut it = self.iter().enumerate();
        for r in ranks.iter() {
            for (i, v) in it.by_ref() {
                if i == r {
                    out.push(v.record());
                    break;
                }
            }
        }
        out
    }

    // --------------------------------------------------------- verification

    /// Checks every structural invariant. O(n); used by tests and debug tooling.
    pub fn validate(&self) -> Result<(), String> {
        let mut prev_last: Option<u64> = None;
        let mut total = 0usize;
        let mut total_sel = 0usize;
        let mut pm = 0u64;
        if self.prefix_max_end.len() != self.chunks.len() {
            return Err("prefix index length mismatch".into());
        }
        for (ci, c) in self.chunks.iter().enumerate() {
            if c.len() == 0 {
                return Err(format!("chunk {ci} is empty"));
            }
            if c.dur.len() != c.len()
                || c.attrs.len() != c.len()
                || c.sel.len() != c.len()
                || (!c.sub.is_empty() && c.sub.len() != c.len())
            {
                return Err(format!("chunk {ci} array length mismatch"));
            }
            if let Some(p) = prev_last
                && p > c.first_start()
            {
                return Err(format!("chunk {ci} starts before previous chunk ends"));
            }
            let mut max_end = 0;
            let mut max_dur = 0;
            for i in 0..c.len() {
                if i > 0 && c.start[i - 1] > c.start[i] {
                    return Err(format!("chunk {ci} unsorted at {i}"));
                }
                let end = c.start[i]
                    .checked_add(c.dur[i])
                    .ok_or_else(|| format!("chunk {ci} note {i} end overflows"))?;
                max_end = max_end.max(end);
                max_dur = max_dur.max(c.dur[i]);
                if NoteAttrs::from_raw(c.attrs[i].raw()).is_none() {
                    return Err(format!("chunk {ci} note {i} has invalid attrs"));
                }
            }
            if max_end != c.max_end || max_dur != c.max_dur {
                return Err(format!("chunk {ci} metadata stale"));
            }
            let sel = c.sel.count_ones();
            if sel != c.selected {
                return Err(format!(
                    "chunk {ci} selection count {} != {sel}",
                    c.selected
                ));
            }
            pm = pm.max(c.max_end);
            if self.prefix_max_end[ci] != pm {
                return Err(format!("prefix max end stale at chunk {ci}"));
            }
            prev_last = c.start.last().copied();
            total += c.len();
            total_sel += sel;
        }
        if total != self.len || total_sel != self.selected {
            return Err("cached totals stale".into());
        }
        Ok(())
    }
}

/// Builds the chunks [`NoteChunk::split_into`] cuts a merged run of `n` notes
/// into, without building the merged run first (v1.0-beta1 B1-0164: a paste
/// or redo that appends a million notes copied them twice).
struct Pieces {
    base: usize,
    extra: usize,
    piece: usize,
    cur: ChunkBuilder,
    out: Vec<NoteChunk>,
}

impl Pieces {
    fn new(n: usize, target: usize) -> Self {
        let pieces = if n <= target * 2 {
            1
        } else {
            n.div_ceil(target)
        };
        let (base, extra) = (n / pieces, n % pieces);
        Pieces {
            base,
            extra,
            piece: 0,
            cur: ChunkBuilder::with_capacity(base + usize::from(extra > 0)),
            out: Vec::with_capacity(pieces),
        }
    }

    fn size(&self, p: usize) -> usize {
        self.base + usize::from(p < self.extra)
    }

    #[inline]
    fn push(&mut self, start: u64, dur: u64, attrs: NoteAttrs, sub: u32, grp: u32, sel: bool) {
        if self.cur.len() == self.size(self.piece) {
            self.piece += 1;
            let next = ChunkBuilder::with_capacity(self.size(self.piece));
            self.out
                .push(std::mem::replace(&mut self.cur, next).finish());
        }
        self.cur.push(start, dur, attrs, sub, grp, sel);
    }

    fn finish(mut self) -> Vec<NoteChunk> {
        self.out.push(self.cur.finish());
        self.out
    }
}

/// The chunk's notes merged with `recs` (stable: a stored note before a new
/// one at the same start), cut as [`NoteChunk::split_into`] would cut the
/// merged run.
fn merge_chunk_pieces(
    chunk: &NoteChunk,
    recs: &[NoteRecord],
    selected: bool,
    target: usize,
) -> Vec<NoteChunk> {
    let mut out = Pieces::new(chunk.len() + recs.len(), target);
    let mut i = 0;
    let mut j = 0;
    while i < chunk.len() || j < recs.len() {
        let take_old = j >= recs.len() || (i < chunk.len() && chunk.start[i] <= recs[j].start_tick);
        if take_old {
            out.push(
                chunk.start[i],
                chunk.dur[i],
                chunk.attrs[i],
                chunk.sub_raw(i),
                chunk.grp_raw(i),
                chunk.sel.get(i),
            );
            i += 1;
        } else {
            let r = recs[j];
            out.push(
                r.start_tick,
                r.duration_ticks,
                r.attrs,
                r.sub.0,
                r.group,
                selected,
            );
            j += 1;
        }
    }
    out.finish()
}

/// Why a rank-addressed patch could not be applied. Nothing is modified when one of
/// these is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchError {
    RankOutOfRange,
    CountMismatch,
    /// Applying the patch would break the sorted order (the patch does not belong to
    /// this state of the track).
    OrderViolation,
    Store(NoteStoreError),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::RankOutOfRange => write!(f, "note position out of range"),
            PatchError::CountMismatch => write!(f, "note count mismatch"),
            PatchError::OrderViolation => write!(f, "notes would be out of order"),
            PatchError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// Read access shared by store chunks (with selection) and snapshot chunks
/// (without).
trait ChunkRead {
    fn data(&self) -> &ChunkData;
    fn is_selected(&self, i: usize) -> bool;
}

impl ChunkRead for NoteChunk {
    #[inline]
    fn data(&self) -> &ChunkData {
        &self.data
    }
    #[inline]
    fn is_selected(&self, i: usize) -> bool {
        self.sel.get(i)
    }
}

impl ChunkRead for Arc<ChunkData> {
    #[inline]
    fn data(&self) -> &ChunkData {
        self
    }
    #[inline]
    fn is_selected(&self, _i: usize) -> bool {
        false
    }
}

/// Overlap query shared by [`NoteStore`] and [`NoteSnapshot`].
fn query_chunks<C: ChunkRead, F: FnMut(NoteLoc, NoteView)>(
    chunks: &[C],
    prefix_max_end: &[u64],
    t0: u64,
    t1: u64,
    mut visit: F,
) -> QueryStats {
    let mut stats = QueryStats {
        chunks_total: chunks.len(),
        ..QueryStats::default()
    };
    if chunks.is_empty() || t0 > t1 {
        return stats;
    }
    let hi = chunks.partition_point(|c| c.data().first_start() <= t1);
    let lo = prefix_max_end.partition_point(|&m| m < t0);
    for (ci, ch) in chunks.iter().enumerate().take(hi).skip(lo) {
        let c = ch.data();
        if c.max_end < t0 {
            continue;
        }
        stats.chunks_visited += 1;
        let lower = t0.saturating_sub(c.max_dur);
        let a = c.start.partition_point(|&s| s < lower);
        let b = c.start.partition_point(|&s| s <= t1);
        stats.notes_scanned += b.saturating_sub(a);
        // The subchannel column's width is resolved once per chunk; the loop is
        // monomorphized per width (no per-note branch on it).
        // The group column is almost always absent: that case gets its own
        // loop too (no per-note branch); grouped chunks read it generically.
        let matched = match (c.sub.as_ref(), c.grp.as_ref()) {
            (SubRef::Main, SubRef::Main) => scan_chunk(ch, ci, a, b, t0, |_| 0, |_| 0, &mut visit),
            (SubRef::W8(v), SubRef::Main) => {
                scan_chunk(ch, ci, a, b, t0, |i| u32::from(v[i]), |_| 0, &mut visit)
            }
            (SubRef::W16(v), SubRef::Main) => {
                scan_chunk(ch, ci, a, b, t0, |i| u32::from(v[i]), |_| 0, &mut visit)
            }
            (SubRef::W32(v), SubRef::Main) => {
                scan_chunk(ch, ci, a, b, t0, |i| v[i], |_| 0, &mut visit)
            }
            (s, g) => scan_chunk(ch, ci, a, b, t0, |i| s.get(i), |i| g.get(i), &mut visit),
        };
        stats.notes_matched += matched;
    }
    stats
}

/// Visits notes `a..b` of one chunk that end at or after `t0`; returns how many.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn scan_chunk<
    C: ChunkRead,
    S: Fn(usize) -> u32,
    G: Fn(usize) -> u32,
    F: FnMut(NoteLoc, NoteView),
>(
    ch: &C,
    ci: usize,
    a: usize,
    b: usize,
    t0: u64,
    sub: S,
    grp: G,
    visit: &mut F,
) -> usize {
    let c = ch.data();
    let (start, dur, attrs) = (&c.start[a..b], &c.dur[a..b], &c.attrs[a..b]);
    let mut matched = 0;
    for (k, ((&s, &d), &at)) in start.iter().zip(dur).zip(attrs).enumerate() {
        if s + d >= t0 {
            let i = a + k;
            matched += 1;
            visit(
                NoteLoc {
                    chunk: ci as u32,
                    index: i as u32,
                },
                NoteView {
                    start_tick: s,
                    duration_ticks: d,
                    attrs: at,
                    sub: SubchannelId(sub(i)),
                    group: grp(i),
                    selected: ch.is_selected(i),
                },
            );
        }
    }
    matched
}

/// The (MIDI channel, subchannel) combinations of the notes `lo..hi` of each
/// chunk, sorted and unique ([`NoteStore::channel_sub_pairs`],
/// [`NoteSnapshot::channel_sub_pairs_in`]).
fn channel_sub_pairs_of<'a>(
    parts: impl Iterator<Item = (&'a ChunkData, usize, usize)>,
) -> Vec<(u8, u32)> {
    let mut main = [false; 16];
    // Subchannels below 256 (one-byte columns): a 16 × 256 bit table, no sort.
    let mut small = [0u64; 64];
    let mut keys: Vec<u64> = Vec::new();
    let mut local: Vec<u64> = Vec::new();
    for (c, lo, hi) in parts {
        match c.sub.as_ref() {
            SubRef::Main => {
                for a in &c.attrs[lo..hi] {
                    main[usize::from(a.channel())] = true;
                }
            }
            SubRef::W8(subs) => {
                for (a, &s) in c.attrs[lo..hi].iter().zip(&subs[lo..hi]) {
                    let bit = usize::from(a.channel()) * 256 + usize::from(s);
                    small[bit / 64] |= 1 << (bit % 64);
                }
            }
            _ => {
                local.clear();
                local.extend(
                    c.attrs[lo..hi]
                        .iter()
                        .zip(c.sub.iter().skip(lo))
                        .map(|(a, s)| (u64::from(a.channel()) << 32) | u64::from(s)),
                );
                local.sort_unstable();
                local.dedup();
                keys.extend_from_slice(&local);
            }
        }
    }
    for (w, &word) in small.iter().enumerate() {
        let mut bits = word;
        while bits != 0 {
            let b = w * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            keys.push(((b / 256) as u64) << 32 | (b % 256) as u64);
        }
    }
    for (ch, used) in main.iter().enumerate() {
        if *used {
            keys.push((ch as u64) << 32);
        }
    }
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter()
        .map(|k| ((k >> 32) as u8, k as u32))
        .collect()
}

/// Read-only, cheaply cloneable view of one track's notes at one moment. Shares
/// chunk memory with the [`NoteStore`] it came from; used by playback so it never
/// copies notes and never sees a half-applied edit.
#[derive(Clone, Debug, Default)]
pub struct NoteSnapshot {
    chunks: Vec<Arc<ChunkData>>,
    prefix_max_end: Vec<u64>,
    len: usize,
}

/// Position inside a [`NoteSnapshot`] (chunk, index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SnapshotPos {
    pub chunk: usize,
    pub index: usize,
}

impl NoteSnapshot {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn max_end(&self) -> Option<u64> {
        self.prefix_max_end.last().copied()
    }

    /// Longest note duration (O(chunks)).
    pub fn max_duration(&self) -> u64 {
        self.chunks.iter().map(|c| c.max_dur).max().unwrap_or(0)
    }

    /// First position whose note starts at or after `tick`.
    pub fn lower_bound(&self, tick: u64) -> SnapshotPos {
        let ci = self.chunks.partition_point(|c| c.first_start() < tick);
        // Notes starting at `tick` may begin in the previous chunk.
        let ci = ci.saturating_sub(1);
        let mut pos = SnapshotPos {
            chunk: ci,
            index: 0,
        };
        if let Some(c) = self.chunks.get(ci) {
            pos.index = c.start.partition_point(|&s| s < tick);
            if pos.index == c.len() {
                pos.chunk += 1;
                pos.index = 0;
            }
        }
        pos
    }

    /// The note at `pos` and the position after it, or `None` at the end.
    #[inline]
    pub fn next_at(&self, pos: SnapshotPos) -> Option<(NoteRecord, SnapshotPos)> {
        let c = self.chunks.get(pos.chunk)?;
        let rec = NoteRecord {
            start_tick: *c.start.get(pos.index)?,
            duration_ticks: c.dur[pos.index],
            attrs: c.attrs[pos.index],
            sub: SubchannelId(c.sub_raw(pos.index)),
            group: c.grp_raw(pos.index),
        };
        let next = if pos.index + 1 < c.len() {
            SnapshotPos {
                chunk: pos.chunk,
                index: pos.index + 1,
            }
        } else {
            SnapshotPos {
                chunk: pos.chunk + 1,
                index: 0,
            }
        };
        Some((rec, next))
    }

    /// Start tick at `pos` without advancing.
    #[inline]
    pub fn start_at(&self, pos: SnapshotPos) -> Option<u64> {
        self.chunks.get(pos.chunk)?.start.get(pos.index).copied()
    }

    /// The positions of the notes starting in `[lo, hi)`: from the first one
    /// to the one after the last (v0.9 clip windows).
    pub fn window(&self, lo: u64, hi: u64) -> (SnapshotPos, SnapshotPos) {
        let a = self.lower_bound(lo);
        let b = if hi <= lo { a } else { self.lower_bound(hi) };
        (a, b)
    }

    /// Each chunk touched by `[a, b)` with its index range.
    fn parts(
        &self,
        a: SnapshotPos,
        b: SnapshotPos,
    ) -> impl Iterator<Item = (&ChunkData, usize, usize)> {
        let last = b.chunk.min(self.chunks.len());
        let tail = (b.chunk < self.chunks.len() && b.index > 0).then_some(b.chunk);
        (a.chunk..last).chain(tail).filter_map(move |ci| {
            let c: &ChunkData = self.chunks.get(ci)?;
            let lo = if ci == a.chunk { a.index } else { 0 };
            let hi = if ci == b.chunk { b.index } else { c.len() };
            (lo < hi).then_some((c, lo, hi))
        })
    }

    /// Number of notes starting in `[lo, hi)` (muted ones included).
    /// O(log chunks + chunks in the window).
    pub fn count_in(&self, lo: u64, hi: u64) -> usize {
        let (a, b) = self.window(lo, hi);
        self.parts(a, b).map(|(_, l, h)| h - l).sum()
    }

    /// Latest end of the notes starting in `[lo, hi)` (muted ones included),
    /// `None` without such notes. Chunks inside the window use their stored
    /// maximum; only the two boundary chunks are scanned.
    pub fn max_end_in(&self, lo: u64, hi: u64) -> Option<u64> {
        let (a, b) = self.window(lo, hi);
        self.parts(a, b)
            .map(|(c, l, h)| {
                if l == 0 && h == c.len() {
                    c.max_end
                } else {
                    (l..h).map(|i| c.start[i] + c.dur[i]).max().unwrap_or(0)
                }
            })
            .max()
    }

    /// [`NoteStore::channel_sub_pairs`] of the notes starting in `[lo, hi)`.
    pub fn channel_sub_pairs_in(&self, lo: u64, hi: u64) -> Vec<(u8, u32)> {
        let (a, b) = self.window(lo, hi);
        channel_sub_pairs_of(self.parts(a, b))
    }

    /// Notes overlapping `[t0, t1]` (same rule as [`NoteStore::query`]).
    pub fn query<F: FnMut(NoteLoc, NoteView)>(&self, t0: u64, t1: u64, visit: F) -> QueryStats {
        query_chunks(&self.chunks, &self.prefix_max_end, t0, t1, visit)
    }

    /// The shared chunks (analysis engine).
    pub(crate) fn chunk_arcs(&self) -> &[Arc<ChunkData>] {
        &self.chunks
    }

    /// True when both views share every chunk (no note changed in between).
    pub fn same_as(&self, other: &NoteSnapshot) -> bool {
        self.chunks.len() == other.chunks.len()
            && self
                .chunks
                .iter()
                .zip(&other.chunks)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

/// Iterator over every note of a store in stored order ([`NoteStore::iter`]).
/// The subchannel column's width is resolved once per chunk.
pub struct NoteIter<'a, const AUDIBLE: bool = false> {
    chunks: std::slice::Iter<'a, NoteChunk>,
    // The current chunk's columns as plain slices (no per-note deref through
    // the chunk's `Arc`), all of the same length.
    st: &'a [u64],
    du: &'a [u64],
    at: &'a [NoteAttrs],
    /// Selection bits, when the chunk has selected notes.
    sel: Option<&'a BitSet>,
    i: usize,
    sub: SubRef<'a>,
    grp: SubRef<'a>,
    /// The current chunk has neither a subchannel nor a group column (the
    /// common case: one predictable branch per note instead of two lookups).
    plain: bool,
    /// `AUDIBLE` and the current chunk holds a muted note (checked once per
    /// chunk; `AUDIBLE` = false compiles the test away).
    chunk_muted: bool,
}

impl<'a, const AUDIBLE: bool> NoteIter<'a, AUDIBLE> {
    fn new(chunks: &'a [NoteChunk]) -> Self {
        NoteIter {
            chunks: chunks.iter(),
            st: &[],
            du: &[],
            at: &[],
            sel: None,
            i: 0,
            sub: SubRef::Main,
            grp: SubRef::Main,
            plain: true,
            chunk_muted: false,
        }
    }
}

impl<const AUDIBLE: bool> Iterator for NoteIter<'_, AUDIBLE> {
    type Item = NoteView;

    #[inline]
    fn next(&mut self) -> Option<NoteView> {
        loop {
            let i = self.i;
            if i < self.st.len() {
                self.i = i + 1;
                let attrs = self.at[i];
                if AUDIBLE && self.chunk_muted && attrs.muted() {
                    continue;
                }
                let (sub, group) = if self.plain {
                    (0, 0)
                } else {
                    (self.sub.get(i), self.grp.get(i))
                };
                return Some(NoteView {
                    start_tick: self.st[i],
                    duration_ticks: self.du[i],
                    attrs,
                    sub: SubchannelId(sub),
                    group,
                    selected: self.sel.is_some_and(|s| s.get(i)),
                });
            }
            let c = self.chunks.next()?;
            let n = c.len();
            self.st = &c.start[..n];
            self.du = &c.dur[..n];
            self.at = &c.attrs[..n];
            self.sel = (c.selected > 0).then_some(&c.sel);
            self.i = 0;
            self.sub = c.sub.as_ref();
            self.grp = c.grp.as_ref();
            self.plain = matches!((self.sub, self.grp), (SubRef::Main, SubRef::Main));
            self.chunk_muted = AUDIBLE && self.at.iter().any(|a| a.muted());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let here = self.st.len() - self.i;
        (here, None)
    }
}

/// Append-only builder used by the MIDI importer. Notes must be pushed in
/// non-decreasing start order; durations may be patched later by global index
/// (a note's duration is only known when its Note Off arrives).
#[derive(Debug)]
pub struct NoteStoreBuilder {
    chunks: Vec<ChunkData>,
    target: usize,
    len: usize,
    last_start: u64,
}

impl Default for NoteStoreBuilder {
    fn default() -> Self {
        NoteStoreBuilder::new()
    }
}

impl NoteStoreBuilder {
    pub fn new() -> Self {
        NoteStoreBuilder {
            chunks: Vec::new(),
            target: DEFAULT_CHUNK_TARGET,
            len: 0,
            last_start: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends a note with a provisional zero duration; returns its global index.
    pub fn push(&mut self, start_tick: u64, attrs: NoteAttrs) -> Result<usize, NoteStoreError> {
        if start_tick < self.last_start {
            return Err(NoteStoreError::UnsortedBatch);
        }
        if self.chunks.last().is_none_or(|c| c.len() >= self.target) {
            let mut c = ChunkData::default();
            c.start.try_reserve_exact(self.target)?;
            c.dur.try_reserve_exact(self.target)?;
            c.attrs.try_reserve_exact(self.target)?;
            self.chunks.try_reserve(1)?;
            self.chunks.push(c);
        }
        let c = self.chunks.last_mut().expect("chunk pushed above");
        if !c.sub.is_empty() {
            c.sub.push(0);
        }
        if !c.grp.is_empty() {
            c.grp.push(0);
        }
        c.start.push(start_tick);
        c.dur.push(0);
        c.attrs.push(attrs);
        self.last_start = start_tick;
        let idx = self.len;
        self.len += 1;
        Ok(idx)
    }

    /// Sets the duration of a previously pushed note.
    pub fn set_duration(
        &mut self,
        index: usize,
        duration_ticks: u64,
    ) -> Result<(), NoteStoreError> {
        let c = &mut self.chunks[index / DEFAULT_CHUNK_TARGET];
        let i = index % DEFAULT_CHUNK_TARGET;
        if c.start[i].checked_add(duration_ticks).is_none() {
            return Err(NoteStoreError::EndOverflow {
                start_tick: c.start[i],
                duration_ticks,
            });
        }
        c.dur[i] = duration_ticks;
        Ok(())
    }

    /// Replaces the attributes of a previously pushed note (the importer fills in the
    /// note-off velocity and encoding when the termination arrives).
    pub fn set_attrs(&mut self, index: usize, attrs: NoteAttrs) {
        self.chunks[index / DEFAULT_CHUNK_TARGET].attrs[index % DEFAULT_CHUNK_TARGET] = attrs;
    }

    /// Ends a previously pushed note at `end_tick` (not before its start):
    /// its duration and its note-off velocity / encoding, with one lookup
    /// (v1.0-beta1 B1-0161, the importer's note-off path).
    pub fn end_note(
        &mut self,
        index: usize,
        end_tick: u64,
        off_velocity: u8,
        off_kind: NoteOffKind,
    ) -> Result<(), NoteAttrsError> {
        let c = &mut self.chunks[index / DEFAULT_CHUNK_TARGET];
        let i = index % DEFAULT_CHUNK_TARGET;
        let attrs = c.attrs[i].with_note_off(off_velocity, off_kind)?;
        debug_assert!(end_tick >= c.start[i]);
        c.dur[i] = end_tick - c.start[i];
        c.attrs[i] = attrs;
        Ok(())
    }

    pub fn start_of(&self, index: usize) -> u64 {
        self.chunks[index / DEFAULT_CHUNK_TARGET].start[index % DEFAULT_CHUNK_TARGET]
    }

    pub fn attrs_of(&self, index: usize) -> NoteAttrs {
        self.chunks[index / DEFAULT_CHUNK_TARGET].attrs[index % DEFAULT_CHUNK_TARGET]
    }

    /// Puts a previously pushed note into subchannel `sub` (native `.lup` loading
    /// streams the subchannel with each note).
    pub fn set_sub(&mut self, index: usize, sub: SubchannelId) {
        let c = &mut self.chunks[index / DEFAULT_CHUNK_TARGET];
        let i = index % DEFAULT_CHUNK_TARGET;
        if sub.0 != 0 || !c.sub.is_empty() {
            if c.sub.is_empty() {
                c.sub.reserve_exact(self.target);
                c.sub.resize_zero(c.start.len());
            }
            c.sub.set(i, sub.0);
        }
    }

    /// Sets the group of the note at `index` (v0.7).
    pub fn set_group(&mut self, index: usize, group: u32) {
        let c = &mut self.chunks[index / DEFAULT_CHUNK_TARGET];
        let i = index % DEFAULT_CHUNK_TARGET;
        if group != 0 || !c.grp.is_empty() {
            if c.grp.is_empty() {
                c.grp.reserve_exact(self.target);
                c.grp.resize_zero(c.start.len());
            }
            c.grp.set(i, group);
        }
    }

    /// Finishes the store, dropping the notes whose global indices are listed in
    /// `removed` (sorted ascending; used for unmatched note-ons).
    pub fn finish(mut self, removed: &[usize]) -> NoteStore {
        for c in &mut self.chunks {
            c.start.shrink_to_fit();
            c.dur.shrink_to_fit();
            c.attrs.shrink_to_fit();
            c.recompute_meta();
            c.sub.shrink_to_fit();
            c.grp.shrink_to_fit();
        }
        let mut store = NoteStore {
            chunks: self
                .chunks
                .into_iter()
                .map(|d| NoteChunk {
                    sel: BitSet::filled(d.len(), false),
                    data: Arc::new(d),
                    selected: 0,
                })
                .collect(),
            prefix_max_end: Vec::new(),
            len: 0,
            selected: 0,
            chunk_target: self.target,
            revision: 0,
            content_revision: 0,
        };
        store.refresh_index();
        if !removed.is_empty() {
            let locs: Vec<NoteLoc> = removed
                .iter()
                .map(|&g| NoteLoc {
                    chunk: (g / DEFAULT_CHUNK_TARGET) as u32,
                    index: (g % DEFAULT_CHUNK_TARGET) as u32,
                })
                .collect();
            store.remove_locs(&locs);
        }
        store.revision = 0;
        store.content_revision = fresh_content_revision();
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(p: u8) -> NoteAttrs {
        NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap()
    }

    fn rec(s: u64, d: u64, p: u8) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: attrs(p),
            sub: Default::default(),
            group: 0,
        }
    }

    fn brute(store: &NoteStore, t0: u64, t1: u64) -> Vec<NoteView> {
        store
            .iter()
            .filter(|v| v.start_tick <= t1 && v.end_tick() >= t0)
            .collect()
    }

    fn xorshift(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    /// L-S38: fully selected chunks are read column by column — the bounds
    /// and the visit order equal a brute-force walk of the selected notes,
    /// for every mix of full, partial and empty chunks, with and without a
    /// subchannel column.
    #[test]
    fn selection_bounds_and_visits_of_full_chunks_equal_a_brute_force_walk() {
        let mut seed = 0x5EED_u64;
        for round in 0..24u64 {
            let mut st = NoteStore::with_chunk_target(64);
            let mut t = 0u64;
            let recs: Vec<NoteRecord> = (0..700u64)
                .map(|_| {
                    t += xorshift(&mut seed) % 5;
                    let mut r = rec(
                        t,
                        xorshift(&mut seed) % 900,
                        (xorshift(&mut seed) % 128) as u8,
                    );
                    if round.is_multiple_of(3) && xorshift(&mut seed).is_multiple_of(7) {
                        r.sub = SubchannelId(1 + (xorshift(&mut seed) % 3) as u32);
                    }
                    r
                })
                .collect();
            st.insert_sorted_batch(&recs, false).unwrap();
            match round % 4 {
                0 => st.select_all(),
                1 => {
                    st.select_range(0, t / 3, false);
                }
                2 => {
                    st.select_all();
                    let index = (xorshift(&mut seed) % 30) as u32;
                    st.set_selected(NoteLoc { chunk: 1, index }, false);
                }
                _ => {
                    st.select_range(t / 4, t / 2, false);
                    st.select_range(3 * t / 4, u64::MAX, false);
                }
            }
            let want: Vec<NoteView> = st.iter().filter(|v| v.selected).collect();
            let mut got = Vec::new();
            st.for_each_selected(|v| got.push(v));
            assert_eq!(got, want, "round {round}");
            let b = st.selection_bounds();
            let brute = (!want.is_empty()).then(|| SelectionBounds {
                count: want.len(),
                min_start: want.iter().map(|v| v.start_tick).min().unwrap(),
                max_end: want.iter().map(|v| v.end_tick()).max().unwrap(),
                min_pitch: want.iter().map(|v| v.attrs.pitch()).min().unwrap(),
                max_pitch: want.iter().map(|v| v.attrs.pitch()).max().unwrap(),
                min_duration: want.iter().map(|v| v.duration_ticks).min().unwrap(),
            });
            assert_eq!(b, brute, "round {round}");
            let same = |f: &dyn Fn(&NoteView) -> u64| {
                want.first()
                    .map(f)
                    .filter(|x| want.iter().all(|v| f(v) == *x))
            };
            assert_eq!(
                st.selection_uniformity(),
                SelectionUniformity {
                    muted: want.iter().filter(|v| v.attrs.muted()).count(),
                    gate: same(&|v| v.duration_ticks),
                    velocity: same(&|v| u64::from(v.attrs.velocity())).map(|x| x as u8),
                },
                "round {round}"
            );
        }
        // Uniform values survive full and partial chunks alike.
        let mut st = NoteStore::with_chunk_target(64);
        let recs: Vec<NoteRecord> = (0..300u64)
            .map(|k| {
                let mut r = rec(k * 2, 40, 60);
                r.attrs = r.attrs.with_muted(k % 9 == 0);
                r
            })
            .collect();
        st.insert_sorted_batch(&recs, false).unwrap();
        st.select_range(10, 500, false);
        let u = st.selection_uniformity();
        assert_eq!((u.gate, u.velocity), (Some(40), Some(100)));
        assert_eq!(u.muted, (5..250).filter(|k| k % 9 == 0).count());
        st.clear_selection();
        assert_eq!(st.selection_uniformity(), SelectionUniformity::default());
    }

    #[test]
    fn appended_batches_keep_every_subchannel_and_need_no_column_for_main_notes() {
        // Batches of mixed widths, a first non-main note in the middle of a batch,
        // batches that span chunk boundaries, and a main-only store.
        let mut st = NoteStore::with_chunk_target(64);
        let mut want = Vec::new();
        let mut t = 0u64;
        let subs = [0u32, 0, 0, 7, 0, 300, 0, 70_000, 1, 0];
        for (b, len) in [5usize, 70, 1, 130, 64, 3].into_iter().enumerate() {
            let batch: Vec<NoteRecord> = (0..len)
                .map(|k| {
                    t += 3;
                    let sub = if b == 0 {
                        0
                    } else {
                        subs[(k + b) % subs.len()]
                    };
                    NoteRecord {
                        sub: SubchannelId(sub),
                        ..rec(t, 2 + (k as u64 % 5), 40 + (k % 50) as u8)
                    }
                })
                .collect();
            st.append_sorted(&batch, batch.len()).unwrap();
            want.extend(batch);
        }
        let got: Vec<NoteRecord> = st.iter().map(|v| v.record()).collect();
        assert_eq!(got, want);
        assert_eq!(st.len(), want.len());
        assert_eq!(st.selected_count(), 0);
        // Chunk metadata is right (queries find every note).
        for (t0, t1) in [(0, 10), (100, 400), (0, u64::MAX)] {
            let mut n = 0;
            st.query(t0, t1, |_, _| n += 1);
            assert_eq!(n, brute(&st, t0, t1).len(), "{t0}..{t1}");
        }
        // Only main notes: no column at all.
        let mut main = NoteStore::with_chunk_target(64);
        let batch: Vec<NoteRecord> = (0..200u64).map(|i| rec(i, 1, 60)).collect();
        main.append_sorted(&batch, 200).unwrap();
        assert!(main.chunks.iter().all(|c| c.sub.is_empty()));
    }

    #[test]
    fn count_starting_in_matches_a_scan() {
        let mut st = NoteStore::with_chunk_target(64);
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let recs: Vec<NoteRecord> = (0..5_000u64)
            .map(|i| NoteRecord::new(i / 3 * 7, 5, a))
            .collect();
        st.insert_sorted_batch(&recs, false).unwrap();
        for (t0, t1) in [
            (0, 0),
            (0, 10),
            (7, 7),
            (100, 5_000),
            (3_000, 20_000),
            (11_669, 11_669),
            (50_000, 60_000),
            (9, 3),
        ] {
            let want = st
                .iter()
                .filter(|v| v.start_tick >= t0 && v.start_tick <= t1)
                .count();
            assert_eq!(st.count_starting_in(t0, t1), want, "{t0}..{t1}");
        }
    }

    #[test]
    fn estimate_is_an_upper_bound_that_stays_tight_for_small_windows() {
        let mut seed = 77u64;
        let recs: Vec<NoteRecord> = (0..100_000u64)
            .map(|i| rec(i * 10, 5 + xorshift(&mut seed) % 95, (i % 128) as u8))
            .collect();
        let s = NoteStore::from_sorted_records(&recs, false).unwrap();
        for _ in 0..500 {
            let t0 = xorshift(&mut seed) % 1_000_000;
            let t1 = t0 + xorshift(&mut seed) % 5_000;
            let exact = s.count_in_range(t0, t1);
            let est = s.estimate_in_range(t0, t1);
            assert!(est >= exact, "{est} < {exact} for [{t0}, {t1}]");
            // Only notes within one longest duration before t0 can be over-counted.
            assert!(est <= exact + 2 * 10 + 2, "{est} vs {exact}");
        }
        // A long note keeps the bound correct (it may widen it).
        let mut s2 = s.clone();
        s2.insert(rec(0, 2_000_000, 60), false).unwrap();
        for t0 in [0u64, 500_000, 999_000] {
            assert!(s2.estimate_in_range(t0, t0 + 100) >= s2.count_in_range(t0, t0 + 100));
        }
    }

    #[test]
    fn range_selection_is_half_open_by_onset_or_intersection() {
        let mut seed = 5u64;
        let mut s = NoteStore::with_chunk_target(16);
        for i in 0..3_000u64 {
            let d = xorshift(&mut seed) % 400;
            s.insert(rec((i * 37) % 20_000, d, (i % 128) as u8), false)
                .unwrap();
        }
        s.insert(rec(0, 1_000_000, 1), false).unwrap(); // one very long note
        for _ in 0..200 {
            let t0 = xorshift(&mut seed) % 21_000;
            let t1 = t0 + xorshift(&mut seed) % 3_000;
            for intersecting in [false, true] {
                let mut a = s.clone();
                a.clear_selection();
                let n = a.select_range(t0, t1, intersecting);
                let expect: Vec<bool> = s
                    .iter()
                    .map(|v| {
                        let st = v.start_tick;
                        if intersecting {
                            st < t1 && (st >= t0 || v.end_tick() > t0)
                        } else {
                            st >= t0 && st < t1
                        }
                    })
                    .collect();
                let got: Vec<bool> = a.iter().map(|v| v.selected).collect();
                assert_eq!(got, expect, "[{t0}, {t1}) intersecting={intersecting}");
                assert_eq!(n, expect.iter().filter(|&&x| x).count());
                a.validate().unwrap();
            }
        }
        // Adjacent ranges never select a note twice by onset.
        let mut a = s.clone();
        a.clear_selection();
        let n1 = a.select_range(0, 10_000, false);
        let n2 = a.select_range(10_000, 30_000, false);
        assert_eq!(n1 + n2, s.len());
        // Empty and inverted ranges select nothing.
        let mut e = s.clone();
        e.clear_selection();
        assert_eq!(e.select_range(50, 50, true), 0);
        assert_eq!(e.select_range(60, 50, false), 0);
        assert_eq!(e.select_range(u64::MAX - 1, u64::MAX, false), 0);
    }

    #[test]
    fn insert_keeps_order_and_ties_stable() {
        let mut s = NoteStore::with_chunk_target(16);
        for i in 0..200u64 {
            s.insert(rec((i * 7919) % 50, 3, (i % 128) as u8), false)
                .unwrap();
            s.validate().unwrap();
        }
        // Equal starts: later insertions come after earlier ones.
        let mut s2 = NoteStore::with_chunk_target(16);
        for p in 0..40u8 {
            s2.insert(rec(10, 1, p), false).unwrap();
        }
        let pitches: Vec<u8> = s2.iter().map(|v| v.attrs.pitch()).collect();
        assert_eq!(pitches, (0..40).collect::<Vec<_>>());
        s2.validate().unwrap();
    }

    #[test]
    fn query_matches_brute_force_including_long_notes() {
        let mut seed = 42u64;
        let mut s = NoteStore::with_chunk_target(32);
        let mut recs = Vec::new();
        for _ in 0..3000 {
            let st = xorshift(&mut seed) % 100_000;
            let d = if xorshift(&mut seed).is_multiple_of(50) {
                xorshift(&mut seed) % 60_000 // occasional very long note
            } else {
                xorshift(&mut seed) % 200
            };
            recs.push(rec(st, d, (xorshift(&mut seed) % 128) as u8));
        }
        recs.sort_by_key(|r| r.start_tick);
        s.insert_sorted_batch(&recs, false).unwrap();
        s.validate().unwrap();
        for _ in 0..300 {
            let a = xorshift(&mut seed) % 110_000;
            let b = a + xorshift(&mut seed) % 5_000;
            let mut got = Vec::new();
            s.query(a, b, |_, v| got.push(v));
            assert_eq!(got, brute(&s, a, b), "range {a}..{b}");
            // v1.0-beta1 B1-0145: the reverse query visits the same notes
            // backwards and stops when asked.
            let mut rev = Vec::new();
            s.query_rev(a, b, |_, v| {
                rev.push(v);
                true
            });
            rev.reverse();
            assert_eq!(rev, got, "reverse range {a}..{b}");
            let stop = got.len() / 2;
            let mut first = Vec::new();
            s.query_rev(a, b, |_, v| {
                first.push(v);
                first.len() < stop.max(1)
            });
            assert_eq!(first.len(), stop.max(1).min(got.len()));
            // L-S46: the forward query that stops visits the same notes in
            // the same order, and only as many as asked.
            let mut fwd = Vec::new();
            s.query_while(a, b, |_, v| {
                fwd.push(v);
                true
            });
            assert_eq!(fwd, got, "forward range {a}..{b}");
            let mut head = Vec::new();
            s.query_while(a, b, |_, v| {
                head.push(v);
                head.len() < stop.max(1)
            });
            assert_eq!(head[..], got[..stop.max(1).min(got.len())]);
        }
        let mut m = [0u64; 4];
        for r in &recs {
            let p = usize::from(r.attrs.pitch());
            m[p / 64] |= 1 << (p % 64);
        }
        assert_eq!(s.pitch_mask(), m);
        // L-S45: the mask read in pieces of any size is the same mask.
        for step in [1, 7, 4096, usize::MAX] {
            let (mut part, mut at, mut calls) = ([0u64; 4], 0, 0);
            let mut total = 0;
            while at < s.chunk_count() {
                let (next, read) = s.pitch_mask_part(&mut part, at, step);
                assert!(next > at && (read >= step.min(1)), "progress");
                (at, total) = (next, total + read);
                calls += 1;
            }
            assert_eq!(total, s.len());
            assert_eq!(part, m, "pieces of {step}");
            assert!(calls <= s.chunk_count().max(1), "{calls}");
        }
    }

    #[test]
    fn overlapping_is_the_brute_force_rule_for_any_start_range_and_growth() {
        // V09-S17: notes starting in [s0, s1), at or before t1, reaching t0
        // with their length replaced by len(d), in stored order — with the
        // stored lengths, gated growth (×2.5) and long notes in the store.
        let mut seed = 0x0517u64;
        let mut s = NoteStore::with_chunk_target(32);
        let mut recs = Vec::new();
        for _ in 0..3000 {
            let st = xorshift(&mut seed) % 100_000;
            let d = match xorshift(&mut seed) % 60 {
                0 => xorshift(&mut seed) % 60_000,
                1 => 0,
                _ => xorshift(&mut seed) % 200,
            };
            recs.push(rec(st, d, (xorshift(&mut seed) % 128) as u8));
        }
        recs.sort_by_key(|r| r.start_tick);
        s.insert_sorted_batch(&recs, false).unwrap();
        let grown = |d: u64| (d * 5).div_ceil(2);
        for k in 0..600 {
            let t0 = xorshift(&mut seed) % 110_000;
            let t1 = t0 + xorshift(&mut seed) % 5_000;
            let s0 = xorshift(&mut seed) % 110_000;
            let s1 = match k % 4 {
                0 => u64::MAX,
                1 => s0,
                _ => s0 + xorshift(&mut seed) % 50_000,
            };
            let expect = |len: &dyn Fn(u64) -> u64| -> Vec<NoteRecord> {
                s.iter()
                    .filter(|v| {
                        (s0..s1).contains(&v.start_tick)
                            && v.start_tick <= t1
                            && v.start_tick.saturating_add(len(v.duration_ticks)) >= t0
                    })
                    .map(|v| v.record())
                    .collect()
            };
            let got: Vec<NoteRecord> = s.overlapping(t0, t1, s0, s1, |d| d).collect();
            assert_eq!(got, expect(&|d| d), "plain {t0}..={t1} starts {s0}..{s1}");
            let got: Vec<NoteRecord> = s.overlapping(t0, t1, s0, s1, grown).collect();
            assert_eq!(got, expect(&grown), "grown {t0}..={t1} starts {s0}..{s1}");
            if s0 == 0 && s1 == u64::MAX {
                let mut q = Vec::new();
                s.query(t0, t1, |_, v| q.push(v.record()));
                assert_eq!(q, expect(&|d| d));
            }
        }
        // The query's rule exactly, over the whole start range.
        let all: Vec<NoteRecord> = s.overlapping(500, 900, 0, u64::MAX, |d| d).collect();
        let mut q = Vec::new();
        s.query(500, 900, |_, v| q.push(v.record()));
        assert_eq!(all, q);
        assert!(
            NoteStore::new()
                .overlapping(0, 9, 0, 9, |d| d)
                .next()
                .is_none()
        );
    }

    #[test]
    fn long_note_before_viewport_is_found() {
        let mut s = NoteStore::with_chunk_target(16);
        s.insert(rec(0, 1_000_000, 60), false).unwrap();
        for i in 0..1000u64 {
            s.insert(rec(10 + i, 1, 61), false).unwrap();
        }
        let mut found = Vec::new();
        let stats = s.query(900_000, 900_100, |_, v| found.push(v));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].attrs.pitch(), 60);
        // Only the long note's chunk needs scanning, not all 1001 notes.
        assert!(stats.notes_scanned < 100, "{stats:?}");
    }

    #[test]
    fn batch_insert_equals_individual_inserts() {
        let mut seed = 7u64;
        let mut base = Vec::new();
        for _ in 0..500 {
            base.push(rec(xorshift(&mut seed) % 1000, xorshift(&mut seed) % 10, 1));
        }
        base.sort_by_key(|r| r.start_tick);
        let mut extra = Vec::new();
        for _ in 0..300 {
            extra.push(rec(xorshift(&mut seed) % 1200, 5, 2));
        }
        extra.sort_by_key(|r| r.start_tick);

        let mut a = NoteStore::with_chunk_target(16);
        a.insert_sorted_batch(&base, false).unwrap();
        let mut b = a.clone();
        a.insert_sorted_batch(&extra, true).unwrap();
        for r in &extra {
            b.insert(*r, true).unwrap();
        }
        a.validate().unwrap();
        b.validate().unwrap();
        let va: Vec<_> = a.iter().collect();
        let vb: Vec<_> = b.iter().collect();
        assert_eq!(va, vb);
        assert_eq!(a.selected_count(), 300);
    }

    #[test]
    fn remove_and_extract_selected() {
        let recs: Vec<_> = (0..1000u64).map(|i| rec(i, 2, (i % 100) as u8)).collect();
        let mut s = NoteStore::from_sorted_records(&recs, false).unwrap();
        let n = s.select_in_region(100, 199, 0, 127);
        // Notes 98 and 99 end at 100/101 and overlap tick 100 as well.
        assert_eq!(n, 102);
        let got = s.extract_selected().unwrap();
        assert_eq!(got.len(), 102);
        assert_eq!(s.len(), 898);
        assert_eq!(s.selected_count(), 0);
        s.validate().unwrap();
        s.select_all();
        assert_eq!(s.selected_count(), 898);
        assert_eq!(s.remove_selected(), 898);
        assert!(s.is_empty());
        s.validate().unwrap();
    }

    #[test]
    fn duration_changes_are_atomic() {
        let recs = vec![rec(0, 5, 1), rec(10, 1, 2)];
        let mut s = NoteStore::from_sorted_records(&recs, true).unwrap();
        assert!(s.add_selected_duration(-2).is_err());
        assert_eq!(
            s.iter().map(|v| v.duration_ticks).collect::<Vec<_>>(),
            vec![5, 1]
        );
        s.add_selected_duration(-1).unwrap();
        assert_eq!(
            s.iter().map(|v| v.duration_ticks).collect::<Vec<_>>(),
            vec![4, 0]
        );
        s.validate().unwrap();
        let mut big = NoteStore::new();
        big.insert(rec(u64::MAX - 10, 5, 1), true).unwrap();
        assert!(big.add_selected_duration(6).is_err());
        big.add_selected_duration(5).unwrap();
        assert!(big.insert(rec(u64::MAX, 1, 1), false).is_err());
    }

    #[test]
    fn snapshots_share_chunks_and_selection_never_copies() {
        let mut s = NoteStore::new();
        let recs: Vec<NoteRecord> = (0..50_000u64).map(|i| rec(i, 3, (i % 128) as u8)).collect();
        s.insert_sorted_batch(&recs, false).unwrap();
        let snap = s.snapshot();
        assert!(snap.same_as(&s.snapshot()));
        let content = s.content_revision();
        // Selection changes: no chunk is copied, content revision unchanged.
        s.select_all();
        s.clear_selection();
        s.select_in_region(100, 200, 0, 127);
        assert!(snap.same_as(&s.snapshot()));
        assert_eq!(s.content_revision(), content);
        // A note edit copies only the touched chunk; the snapshot keeps old values.
        s.insert(rec(25_000, 1, 1), false).unwrap();
        assert!(s.content_revision() > content);
        let now = s.snapshot();
        assert!(!snap.same_as(&now));
        let shared = snap
            .chunks
            .iter()
            .filter(|a| now.chunks.iter().any(|b| Arc::ptr_eq(a, b)))
            .count();
        assert!(
            shared >= snap.chunks.len() - 2,
            "{shared} of {}",
            snap.chunks.len()
        );
        assert_eq!(snap.len(), 50_000);
        assert_eq!(now.len(), 50_001);
        s.validate().unwrap();
    }

    #[test]
    fn rank_patches_round_trip() {
        let mut s = NoteStore::with_chunk_target(64);
        let recs: Vec<NoteRecord> = (0..5_000u64)
            .map(|i| rec(i / 4, 1 + i % 5, (i % 90) as u8))
            .collect();
        s.insert_sorted_batch(&recs, false).unwrap();
        let before: Vec<NoteRecord> = s.iter().map(|v| v.record()).collect();
        let mut seed = 7u64;
        let mut b = RankSetBuilder::default();
        for r in 0..5_000usize {
            if xorshift(&mut seed).is_multiple_of(3) {
                b.push(r);
            }
        }
        let ranks = b.finish();
        s.select_ranks(&ranks).unwrap();
        let (r2, removed) = s.selected_with_ranks();
        assert_eq!(r2, ranks);
        assert_eq!(s.remove_ranks(&ranks).unwrap(), ranks.len());
        s.validate().unwrap();
        s.insert_at_ranks(&ranks, &removed).unwrap();
        s.validate().unwrap();
        let after: Vec<NoteRecord> = s.iter().map(|v| v.record()).collect();
        assert_eq!(after, before, "exact positions restored, including ties");
        assert_eq!(s.selected_ranks(), ranks);
        // Mismatched patches are refused without changing anything.
        let fp: Vec<NoteRecord> = s.iter().map(|v| v.record()).collect();
        assert_eq!(
            s.insert_at_ranks(&RankSet::all(3), &CompactRecords::new()),
            Err(PatchError::CountMismatch)
        );
        let mut bad = CompactRecords::new();
        bad.push(&rec(10_000, 1, 1));
        let mut rb = RankSetBuilder::default();
        rb.push(0);
        assert_eq!(
            s.insert_at_ranks(&rb.finish(), &bad),
            Err(PatchError::OrderViolation)
        );
        assert_eq!(s.iter().map(|v| v.record()).collect::<Vec<_>>(), fp);
    }

    #[test]
    fn builder_with_removed() {
        let mut b = NoteStoreBuilder::new();
        for i in 0..10_000u64 {
            let g = b.push(i / 3, attrs((i % 128) as u8)).unwrap();
            b.set_duration(g, 4).unwrap();
        }
        assert!(b.push(0, attrs(1)).is_err());
        let removed: Vec<usize> = (0..10_000).step_by(7).collect();
        let s = b.finish(&removed);
        assert_eq!(s.len(), 10_000 - removed.len());
        assert_eq!(s.selected_count(), 0);
        s.validate().unwrap();
    }
}
