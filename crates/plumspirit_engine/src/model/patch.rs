//! Compact encodings for reversible note edits.
//!
//! Undo/redo never copies a track. A note edit is remembered as *which positions*
//! changed (a [`RankSet`]: global positions in the track's stored order) and, where
//! needed, the note values themselves (a [`CompactRecords`] list). Both are
//! variable-length encoded:
//!
//! * `RankSet` stores runs of consecutive positions as `(gap, length)` varint pairs,
//!   so "every note" or a contiguous selection costs a few bytes regardless of size,
//!   and a scattered selection about two bytes per note.
//! * `CompactRecords` stores notes sorted by start as varint start deltas, varint
//!   durations and the packed 4-byte attributes: typically 6–9 bytes per note
//!   instead of the 20 bytes of a [`NoteRecord`]. A note outside the main
//!   subchannel sets bit 31 of the attribute word (always zero in real
//!   attributes) and appends its subchannel id as a varint, so main-subchannel
//!   notes cost nothing extra.

use super::note::{NoteAttrs, NoteRecord, SubchannelId};
use crate::codec::{get_uvarint, put_uvarint};

/// Bit 31 of a stored attribute word: a subchannel id (varint) follows. Real
/// attribute words never set it (bits 26–31 are reserved and zero).
pub const SUB_FOLLOWS: u32 = 1 << 31;
/// Set in the attribute word when a group id (varint) follows (after the
/// subchannel varint, if any). v0.7.
pub const GROUP_FOLLOWS: u32 = 1 << 30;

/// A strictly increasing set of note positions (ranks) within one track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RankSet {
    /// `(gap from the end of the previous run, run length)` pairs as varints.
    data: Vec<u8>,
    count: usize,
    /// One past the largest rank (0 when empty).
    end: usize,
}

impl RankSet {
    pub fn new() -> Self {
        RankSet::default()
    }

    /// The encoded runs (history journal).
    pub(crate) fn raw(&self) -> &[u8] {
        &self.data
    }

    /// Rebuilds a set from [`RankSet::raw`] bytes, validating them (checked
    /// arithmetic; `None` for damaged data).
    pub(crate) fn from_raw(data: Vec<u8>) -> Option<RankSet> {
        let (mut pos, mut next, mut count) = (0usize, 0usize, 0usize);
        while pos < data.len() {
            let gap = usize::try_from(get_uvarint(&data, &mut pos)?).ok()?;
            let len = usize::try_from(get_uvarint(&data, &mut pos)?).ok()?;
            let start = next.checked_add(gap)?;
            next = start.checked_add(len)?;
            count = count.checked_add(len)?;
        }
        Some(RankSet {
            data,
            count,
            end: next,
        })
    }

    /// All positions `0..n`.
    pub fn all(n: usize) -> Self {
        let mut b = RankSetBuilder::default();
        b.push_run(0, n);
        b.finish()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// One past the largest position (0 for an empty set).
    #[inline]
    pub fn end(&self) -> usize {
        self.end
    }

    pub fn heap_bytes(&self) -> usize {
        self.data.capacity()
    }

    /// Runs as `(first position, length)`, in increasing order.
    pub fn runs(&self) -> RankRuns<'_> {
        RankRuns {
            data: &self.data,
            pos: 0,
            next_start: 0,
        }
    }

    /// Every position, in increasing order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.runs().flat_map(|(s, l)| s..s + l)
    }
}

pub struct RankRuns<'a> {
    data: &'a [u8],
    pos: usize,
    next_start: usize,
}

impl Iterator for RankRuns<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        if self.pos >= self.data.len() {
            return None;
        }
        // Encoded by this module only; a decode failure would be an internal bug, so
        // stop iterating instead of panicking.
        let gap = get_uvarint(self.data, &mut self.pos)? as usize;
        let len = get_uvarint(self.data, &mut self.pos)? as usize;
        let start = self.next_start + gap;
        self.next_start = start + len;
        Some((start, len))
    }
}

/// Builds a [`RankSet`] from strictly increasing positions.
#[derive(Debug, Default)]
pub struct RankSetBuilder {
    data: Vec<u8>,
    count: usize,
    prev_end: usize,
    run_start: usize,
    run_len: usize,
}

impl RankSetBuilder {
    /// Adds `rank`, which must be greater than every rank added before.
    #[inline]
    pub fn push(&mut self, rank: usize) {
        if self.run_len > 0 && rank == self.run_start + self.run_len {
            self.run_len += 1;
        } else {
            debug_assert!(
                self.run_len == 0 || rank > self.run_start + self.run_len,
                "ranks must increase"
            );
            self.flush();
            self.run_start = rank;
            self.run_len = 1;
        }
        self.count += 1;
    }

    pub fn push_run(&mut self, start: usize, len: usize) {
        if len == 0 {
            return;
        }
        if self.run_len > 0 && start == self.run_start + self.run_len {
            self.run_len += len;
        } else {
            self.flush();
            self.run_start = start;
            self.run_len = len;
        }
        self.count += len;
    }

    fn flush(&mut self) {
        if self.run_len == 0 {
            return;
        }
        put_uvarint(&mut self.data, (self.run_start - self.prev_end) as u64);
        put_uvarint(&mut self.data, self.run_len as u64);
        self.prev_end = self.run_start + self.run_len;
        self.run_len = 0;
    }

    pub fn finish(mut self) -> RankSet {
        self.flush();
        self.data.shrink_to_fit();
        RankSet {
            data: self.data,
            count: self.count,
            end: self.prev_end,
        }
    }
}

/// Notes sorted by start tick, compactly encoded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactRecords {
    data: Vec<u8>,
    count: usize,
    last_start: u64,
}

impl CompactRecords {
    pub fn new() -> Self {
        CompactRecords::default()
    }

    /// The encoded notes and their count (history journal).
    pub(crate) fn raw(&self) -> (&[u8], usize) {
        (&self.data, self.count)
    }

    /// Rebuilds records from [`CompactRecords::raw`], validating every note
    /// (`None` for damaged data).
    pub(crate) fn from_raw(data: Vec<u8>, count: usize) -> Option<CompactRecords> {
        let mut r = CompactRecords {
            data,
            count,
            last_start: 0,
        };
        let mut it = r.iter();
        let mut last = 0u64;
        for _ in 0..count {
            last = it.next()?.start_tick;
        }
        if it.pos != r.data.len() {
            return None;
        }
        r.last_start = last;
        Some(r)
    }

    pub fn with_capacity_notes(n: usize) -> Self {
        CompactRecords {
            data: Vec::with_capacity(n.saturating_mul(7)),
            count: 0,
            last_start: 0,
        }
    }

    /// Like [`CompactRecords::with_capacity_notes`], but reports allocation failure
    /// instead of aborting.
    pub fn try_with_capacity_notes(n: usize) -> Result<Self, std::collections::TryReserveError> {
        let mut data = Vec::new();
        data.try_reserve(n.saturating_mul(7))?;
        Ok(CompactRecords {
            data,
            count: 0,
            last_start: 0,
        })
    }

    /// Encodes records sorted by start; `None` if they are not sorted or memory is
    /// short.
    pub fn try_from_sorted(records: &[NoteRecord]) -> Option<Self> {
        let mut c = CompactRecords::try_with_capacity_notes(records.len()).ok()?;
        for r in records {
            if !c.push(r) {
                return None;
            }
        }
        c.shrink();
        Some(c)
    }

    /// Appends a note. Starts must be non-decreasing; returns `false` (and adds
    /// nothing) otherwise.
    #[inline]
    pub fn push(&mut self, r: &NoteRecord) -> bool {
        let NoteRecord {
            start_tick,
            duration_ticks,
            attrs,
            sub,
            group,
        } = *r;
        self.push_parts(start_tick, duration_ticks, attrs, sub.0, group)
    }

    /// [`CompactRecords::push`] from the note's columns (no record built).
    #[inline(always)]
    pub fn push_parts(
        &mut self,
        start: u64,
        dur: u64,
        attrs: NoteAttrs,
        sub: u32,
        group: u32,
    ) -> bool {
        if start < self.last_start {
            return false;
        }
        let d = &mut self.data;
        put_uvarint(d, start - self.last_start);
        put_uvarint(d, dur);
        let mut word = attrs.raw();
        if sub != 0 {
            word |= SUB_FOLLOWS;
        }
        if group != 0 {
            word |= GROUP_FOLLOWS;
        }
        d.extend_from_slice(&word.to_le_bytes());
        if sub != 0 {
            put_uvarint(d, u64::from(sub));
        }
        if group != 0 {
            put_uvarint(d, u64::from(group));
        }
        self.last_start = start;
        self.count += 1;
        true
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn shrink(&mut self) {
        self.data.shrink_to_fit();
    }

    pub fn heap_bytes(&self) -> usize {
        self.data.capacity()
    }

    pub fn iter(&self) -> CompactIter<'_> {
        CompactIter {
            data: &self.data,
            pos: 0,
            start: 0,
            remaining: self.count,
        }
    }
}

pub struct CompactIter<'a> {
    data: &'a [u8],
    pos: usize,
    start: u64,
    remaining: usize,
}

impl Iterator for CompactIter<'_> {
    type Item = NoteRecord;

    fn next(&mut self) -> Option<NoteRecord> {
        if self.remaining == 0 {
            return None;
        }
        let delta = get_uvarint(self.data, &mut self.pos)?;
        let dur = get_uvarint(self.data, &mut self.pos)?;
        let raw = self.data.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        let word = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let attrs = NoteAttrs::from_raw(word & !(SUB_FOLLOWS | GROUP_FOLLOWS))?;
        let sub = if word & SUB_FOLLOWS != 0 {
            SubchannelId(u32::try_from(get_uvarint(self.data, &mut self.pos)?).ok()?)
        } else {
            SubchannelId::MAIN
        };
        let group = if word & GROUP_FOLLOWS != 0 {
            u32::try_from(get_uvarint(self.data, &mut self.pos)?).ok()?
        } else {
            0
        };
        self.start = self.start.checked_add(delta)?;
        self.remaining -= 1;
        Some(NoteRecord {
            start_tick: self.start,
            duration_ticks: dur,
            attrs,
            sub,
            group,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining))
    }
}

/// A reversible change to an ordered list: items removed from it (indices in the
/// list *before* the change, ascending) and items inserted (indices in the list
/// *after* the change, ascending). Applying and reverting are exact inverses and
/// verify that the list is in the expected state, so a patch can never be applied
/// to the wrong list. Cost is O(list length + patch size).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListPatch<T> {
    removed: Vec<(usize, T)>,
    inserted: Vec<(usize, T)>,
}

impl<T> Default for ListPatch<T> {
    fn default() -> Self {
        ListPatch {
            removed: Vec::new(),
            inserted: Vec::new(),
        }
    }
}

impl<T: Clone + PartialEq> ListPatch<T> {
    pub fn new() -> Self {
        ListPatch::default()
    }

    /// Records that `item` at `index` (in the list before the change) is removed.
    pub fn remove(&mut self, index: usize, item: T) {
        let pos = self.removed.partition_point(|r| r.0 < index);
        self.removed.insert(pos, (index, item));
    }

    /// Records that `item` ends up at `index` (in the list after the change).
    pub fn insert(&mut self, index: usize, item: T) {
        let pos = self.inserted.partition_point(|r| r.0 < index);
        self.inserted.insert(pos, (index, item));
    }

    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.inserted.is_empty()
    }

    pub fn removed(&self) -> &[(usize, T)] {
        &self.removed
    }

    pub fn inserted(&self) -> &[(usize, T)] {
        &self.inserted
    }

    /// Every item the patch holds (removed and inserted).
    pub fn items(&self) -> impl Iterator<Item = &T> + '_ {
        self.removed
            .iter()
            .chain(self.inserted.iter())
            .map(|(_, t)| t)
    }

    pub fn heap_bytes(&self) -> usize {
        (self.removed.capacity() + self.inserted.capacity()) * std::mem::size_of::<(usize, T)>()
    }

    /// Before → after. Returns false (leaving `list` unchanged) when `list` is not
    /// in the patch's "before" state.
    pub fn apply(&self, list: &mut Vec<T>) -> bool {
        transform(list, &self.removed, &self.inserted)
    }

    /// After → before.
    pub fn revert(&self, list: &mut Vec<T>) -> bool {
        transform(list, &self.inserted, &self.removed)
    }
}

fn strictly_ascending<T>(v: &[(usize, T)]) -> bool {
    v.windows(2).all(|w| w[0].0 < w[1].0)
}

fn transform<T: Clone + PartialEq>(
    list: &mut Vec<T>,
    take: &[(usize, T)],
    put: &[(usize, T)],
) -> bool {
    if !strictly_ascending(take) || !strictly_ascending(put) {
        return false;
    }
    if take.iter().any(|(i, t)| list.get(*i) != Some(t)) {
        return false;
    }
    let final_len = list.len() - take.len() + put.len();
    if put.last().is_some_and(|(i, _)| *i >= final_len) {
        return false;
    }
    // Small patches edit in place; larger ones rebuild the list once.
    if take.len() + put.len() <= 8 {
        for (i, _) in take.iter().rev() {
            list.remove(*i);
        }
        for (i, t) in put {
            list.insert(*i, t.clone());
        }
        return true;
    }
    // The kept items are moved, not cloned.
    let mut out: Vec<T> = Vec::with_capacity(final_len);
    let (mut ti, mut pi) = (0usize, 0usize);
    for (i, x) in std::mem::take(list).into_iter().enumerate() {
        if ti < take.len() && take[ti].0 == i {
            ti += 1;
            continue;
        }
        while pi < put.len() && put[pi].0 == out.len() {
            out.push(put[pi].1.clone());
            pi += 1;
        }
        out.push(x);
    }
    while pi < put.len() && put[pi].0 == out.len() {
        out.push(put[pi].1.clone());
        pi += 1;
    }
    debug_assert_eq!(pi, put.len());
    *list = out;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::NoteOffKind;

    #[test]
    fn rank_sets_encode_runs() {
        let mut b = RankSetBuilder::default();
        for r in [0usize, 1, 2, 5, 9, 10, 11, 1_000_000] {
            b.push(r);
        }
        let s = b.finish();
        assert_eq!(s.len(), 8);
        assert_eq!(s.end(), 1_000_001);
        assert_eq!(
            s.runs().collect::<Vec<_>>(),
            vec![(0, 3), (5, 1), (9, 3), (1_000_000, 1)]
        );
        assert_eq!(
            s.iter().collect::<Vec<_>>(),
            vec![0, 1, 2, 5, 9, 10, 11, 1_000_000]
        );
        let all = RankSet::all(5_000_000);
        assert_eq!(all.len(), 5_000_000);
        assert!(all.heap_bytes() < 16, "{}", all.heap_bytes());
        assert!(RankSet::new().is_empty());
        assert_eq!(RankSet::all(0), RankSet::new());
    }

    #[test]
    fn compact_records_roundtrip() {
        let a = |p, v, c| NoteAttrs::new(p, v, c, 64, NoteOffKind::NoteOff).unwrap();
        let recs = vec![
            NoteRecord {
                start_tick: 0,
                duration_ticks: 0,
                attrs: a(0, 1, 0),
                sub: Default::default(),
                group: 0,
            },
            NoteRecord {
                start_tick: 0,
                duration_ticks: u64::MAX,
                attrs: a(127, 127, 15),
                sub: Default::default(),
                group: 0,
            },
            NoteRecord {
                start_tick: 1_000_000_000_000,
                duration_ticks: 480,
                attrs: a(60, 100, 9),
                sub: Default::default(),
                group: 0,
            },
        ];
        let mut c = CompactRecords::new();
        for r in &recs {
            assert!(c.push(r));
        }
        assert!(!c.push(&recs[0]), "unsorted push must be refused");
        assert_eq!(c.len(), 3);
        assert_eq!(c.iter().collect::<Vec<_>>(), recs);
    }

    #[test]
    fn list_patches_are_exact_inverses() {
        let base: Vec<u32> = (0..100).collect();
        for (removed, inserted) in [
            (vec![3usize], vec![(3usize, 1000u32)]),
            (vec![0, 1, 50, 99], vec![(0, 7), (60, 8)]),
            (
                (0..40).map(|i| i * 2).collect::<Vec<_>>(),
                (0..20).map(|i| (i * 3, 5000 + i as u32)).collect(),
            ),
            (vec![], vec![(100, 1)]),
        ] {
            let mut p = ListPatch::new();
            for &i in &removed {
                p.remove(i, base[i]);
            }
            for &(i, v) in &inserted {
                p.insert(i, v);
            }
            let mut l = base.clone();
            assert!(p.apply(&mut l));
            for &(i, v) in &inserted {
                assert_eq!(l[i], v);
            }
            assert_eq!(l.len(), base.len() - removed.len() + inserted.len());
            // Applying twice is refused (the list is no longer in the "before" state).
            if !removed.is_empty() {
                let snapshot = l.clone();
                assert!(!p.apply(&mut l) || l != snapshot);
            }
            let mut l2 = base.clone();
            assert!(p.apply(&mut l2));
            assert!(p.revert(&mut l2));
            assert_eq!(l2, base);
        }
        // A patch for another list is refused and changes nothing.
        let mut p = ListPatch::new();
        p.remove(2, 999u32);
        let mut l = base.clone();
        assert!(!p.apply(&mut l));
        assert_eq!(l, base);
    }
}
