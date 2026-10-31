//! The project-wide merged timeline: every note start and every note end of
//! the whole project (as it plays) in one sorted structure, so a Project-scope
//! query costs `O(log n)` no matter how many tracks the notes are spread over.
//!
//! The tick axis is cut into consecutive segments `(last[i-1], last[i]]` of
//! about [`SEG_TARGET`] events each; a segment keeps its starts and ends as two
//! [`SortedTicks`], and running counts / sums before each segment answer prefix
//! queries with one binary search over segments plus one inside a segment.
//!
//! Poly needs no interval search here: every note has `start <= end`, so the
//! notes sounding at `k` are exactly `#(start <= k) − #(end <= k)`.
//!
//! Segments are shared (`Arc`) between index versions. An edit is applied as a
//! *delta* (the start / end ticks removed and added, computed exactly from the
//! per-track pieces that changed): only the segments those ticks fall in are
//! decoded, merged and re-encoded.

use super::ticks::SortedTicks;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Events (starts + ends) per segment when segments are cut.
pub const SEG_TARGET: usize = 16_384;
/// A segment that grows past this many events is split when it is rewritten.
const SEG_MAX: usize = 4 * SEG_TARGET;

/// Ticks `(prev.last, last]` of the timeline.
#[derive(Debug, Default)]
pub(crate) struct Seg {
    pub last: u64,
    pub starts: SortedTicks,
    pub ends: SortedTicks,
}

/// Merged starts / ends of every note of the project.
#[derive(Clone, Debug)]
pub struct Timeline {
    segs: Vec<Arc<Seg>>,
    /// Count and tick sum of starts / ends in segments before `i`
    /// (`len = segs.len() + 1`).
    cum_s: Vec<(u64, u128)>,
    cum_e: Vec<(u64, u128)>,
    max_tick: u64,
}

impl Default for Timeline {
    fn default() -> Self {
        Timeline::from_segs(vec![Arc::new(Seg {
            last: u64::MAX,
            ..Seg::default()
        })])
    }
}

/// Removes the sorted multiset `remove` from sorted `values` and merges in the
/// sorted `add` (`remove` must be contained in `values`).
fn patch(values: Vec<u64>, remove: &[u64], add: &[u64]) -> Vec<u64> {
    let mut kept = Vec::with_capacity(values.len() + add.len());
    let mut r = 0;
    for v in values {
        if remove.get(r) == Some(&v) {
            r += 1;
        } else {
            kept.push(v);
        }
    }
    debug_assert_eq!(r, remove.len(), "removed ticks must be present");
    if add.is_empty() {
        return kept;
    }
    let mut out = Vec::with_capacity(kept.len() + add.len());
    let (mut i, mut j) = (0, 0);
    while i < kept.len() || j < add.len() {
        if j == add.len() || (i < kept.len() && kept[i] <= add[j]) {
            out.push(kept[i]);
            i += 1;
        } else {
            out.push(add[j]);
            j += 1;
        }
    }
    out
}

/// Cuts the sorted events of `(lo, hi]` into segments of about
/// [`SEG_TARGET`] events, the last one ending at `hi`; all events of one tick
/// stay in one segment.
fn cut(hi: u64, starts: &[u64], ends: &[u64], out: &mut Vec<Arc<Seg>>) {
    let (mut i, mut j) = (0usize, 0usize);
    let (mut si, mut sj) = (0usize, 0usize);
    loop {
        if (i - si) + (j - sj) >= SEG_TARGET {
            // Cut after the last consumed tick (with every event on it).
            let t = match (i.checked_sub(1), j.checked_sub(1)) {
                (Some(a), Some(b)) if a >= si && b >= sj => starts[a].max(ends[b]),
                (Some(a), _) if a >= si => starts[a],
                (_, Some(b)) => ends[b],
                _ => unreachable!("events were consumed"),
            };
            if t >= hi {
                break;
            }
            while starts.get(i) == Some(&t) {
                i += 1;
            }
            while ends.get(j) == Some(&t) {
                j += 1;
            }
            out.push(Arc::new(Seg {
                last: t,
                starts: SortedTicks::from_sorted(&starts[si..i]),
                ends: SortedTicks::from_sorted(&ends[sj..j]),
            }));
            si = i;
            sj = j;
        }
        match (starts.get(i), ends.get(j)) {
            (Some(&a), Some(&b)) => {
                if a <= b {
                    i += 1;
                } else {
                    j += 1;
                }
            }
            (Some(_), None) => i += 1,
            (None, Some(_)) => j += 1,
            (None, None) => break,
        }
    }
    out.push(Arc::new(Seg {
        last: hi,
        starts: SortedTicks::from_sorted(&starts[si..]),
        ends: SortedTicks::from_sorted(&ends[sj..]),
    }));
}

/// Window boundaries for a full build: every ~`per` notes of the
/// `(first start, notes)` samples (sorted by first start).
pub(crate) fn windows(mut samples: Vec<(u64, u64)>, per: u64) -> Vec<u64> {
    samples.sort_unstable();
    let mut out = Vec::new();
    let mut acc = 0u64;
    for (first, n) in samples {
        if acc >= per && first > 0 && out.last().is_none_or(|&b| first - 1 > b) {
            out.push(first - 1);
            acc = 0;
        }
        acc += n;
    }
    out
}

impl Timeline {
    fn from_segs(segs: Vec<Arc<Seg>>) -> Timeline {
        let mut cum_s = Vec::with_capacity(segs.len() + 1);
        let mut cum_e = Vec::with_capacity(segs.len() + 1);
        let (mut s, mut e) = ((0u64, 0u128), (0u64, 0u128));
        let mut max_tick = 0;
        for g in &segs {
            cum_s.push(s);
            cum_e.push(e);
            s = (s.0 + g.starts.len() as u64, s.1 + g.starts.total());
            e = (e.0 + g.ends.len() as u64, e.1 + g.ends.total());
            if !g.ends.is_empty() {
                max_tick = max_tick.max(g.ends.max());
            }
        }
        cum_s.push(s);
        cum_e.push(e);
        Timeline {
            segs,
            cum_s,
            cum_e,
            max_tick,
        }
    }

    /// Full build. `gather(lo, hi, starts, ends)` must push every start and
    /// end tick in `(lo, hi]` (any order); `bounds` are the window ends.
    ///
    /// L-S38: the windows are independent (each is cut on its own), so with
    /// `threads` >= 2 they are gathered, sorted and cut on that many scoped
    /// threads and their segments joined in window order — the same segments
    /// the one-thread loop makes (64 % of a full index build was this loop).
    pub(crate) fn build(
        bounds: &[u64],
        cancel: Option<&AtomicBool>,
        threads: usize,
        gather: impl Fn(Option<u64>, u64, &mut Vec<u64>, &mut Vec<u64>) + Sync,
    ) -> Option<Timeline> {
        // The windows `(lo, hi]` in the order the sequential loop visits them.
        let mut wins: Vec<(Option<u64>, u64)> = Vec::with_capacity(bounds.len() + 1);
        let mut lo: Option<u64> = None;
        for &hi in bounds.iter().chain(std::iter::once(&u64::MAX)) {
            if lo.is_some_and(|l| hi <= l) {
                continue;
            }
            wins.push((lo, hi));
            lo = Some(hi);
        }
        let per_window = super::par_map(
            &wins,
            threads,
            || (Vec::new(), Vec::new()),
            |(s, e): &mut (Vec<u64>, Vec<u64>), &(lo, hi)| {
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return None;
                }
                s.clear();
                e.clear();
                gather(lo, hi, s, e);
                s.sort_unstable();
                e.sort_unstable();
                let mut segs = Vec::new();
                cut(hi, s, e, &mut segs);
                Some(segs)
            },
        )?;
        Some(Timeline::from_segs(
            per_window.into_iter().flatten().collect(),
        ))
    }

    /// The timeline with the sorted multisets `del_*` removed and `add_*`
    /// added. Returns it and the number of segments rewritten.
    pub(crate) fn apply(
        &self,
        del_s: &[u64],
        add_s: &[u64],
        del_e: &[u64],
        add_e: &[u64],
    ) -> (Timeline, usize) {
        let mut segs = Vec::with_capacity(self.segs.len());
        let mut rewritten = 0;
        let (mut a, mut b, mut c, mut d) = (0, 0, 0, 0);
        // Slice of a sorted delta inside the segment `(lo, last]`.
        let take = |v: &[u64], at: &mut usize, last: u64| -> std::ops::Range<usize> {
            let from = *at;
            *at += v[from..].partition_point(|&x| x <= last);
            from..*at
        };
        for g in &self.segs {
            let (ra, rb, rc, rd) = (
                take(del_s, &mut a, g.last),
                take(add_s, &mut b, g.last),
                take(del_e, &mut c, g.last),
                take(add_e, &mut d, g.last),
            );
            if ra.is_empty() && rb.is_empty() && rc.is_empty() && rd.is_empty() {
                segs.push(Arc::clone(g));
                continue;
            }
            rewritten += 1;
            let starts = patch(g.starts.to_vec(), &del_s[ra], &add_s[rb]);
            let ends = patch(g.ends.to_vec(), &del_e[rc], &add_e[rd]);
            if starts.len() + ends.len() > SEG_MAX {
                cut(g.last, &starts, &ends, &mut segs);
            } else {
                segs.push(Arc::new(Seg {
                    last: g.last,
                    starts: SortedTicks::from_sorted(&starts),
                    ends: SortedTicks::from_sorted(&ends),
                }));
            }
        }
        (Timeline::from_segs(segs), rewritten)
    }

    pub fn segments(&self) -> usize {
        self.segs.len()
    }

    /// L-S48: how many segments a patch with ticks inside `spans` (closed
    /// tick ranges, any order) would rewrite at most: the segments the spans'
    /// union reaches.
    pub(crate) fn touched(&self, spans: &mut [(u64, u64)]) -> usize {
        spans.sort_unstable();
        let mut touched = 0;
        // Segment ranges `[a, b]` of the spans, merged as they come sorted.
        let mut open: Option<(usize, usize)> = None;
        for &(lo, hi) in spans.iter() {
            let a = self.seg_of(lo);
            let b = self.seg_of(hi).min(self.segs.len().saturating_sub(1));
            match open {
                Some((_, ob)) if a <= ob + 1 => open = open.map(|(oa, ob)| (oa, ob.max(b))),
                Some((oa, ob)) => {
                    touched += ob + 1 - oa;
                    open = Some((a, b));
                }
                None => open = Some((a, b)),
            }
        }
        if let Some((oa, ob)) = open {
            touched += ob + 1 - oa;
        }
        touched.min(self.segs.len())
    }

    /// Notes (starts) on the timeline.
    pub fn notes(&self) -> u64 {
        self.cum_s.last().map_or(0, |x| x.0)
    }

    /// Largest end tick (0 when empty).
    pub fn max_tick(&self) -> u64 {
        self.max_tick
    }

    pub fn heap_bytes(&self) -> usize {
        self.segs
            .iter()
            .map(|g| g.starts.heap_bytes() + g.ends.heap_bytes() + std::mem::size_of::<Seg>())
            .sum::<usize>()
            + self.segs.capacity() * 8
            + (self.cum_s.capacity() + self.cum_e.capacity()) * 32
    }

    /// Segment holding tick `k` (the first whose `last >= k`).
    #[inline]
    fn seg_of(&self, k: u64) -> usize {
        self.segs.partition_point(|g| g.last < k)
    }

    /// Count and sum of starts `<= k`.
    #[inline]
    pub fn starts_le(&self, k: u64) -> (u64, u128) {
        let i = self.seg_of(k);
        let (c, s) = self.cum_s[i];
        let (c2, s2) = self.segs[i].starts.prefix_le(k);
        (c + c2 as u64, s + s2)
    }

    /// Count and sum of ends `<= k`.
    #[inline]
    pub fn ends_le(&self, k: u64) -> (u64, u128) {
        let i = self.seg_of(k);
        let (c, s) = self.cum_e[i];
        let (c2, s2) = self.segs[i].ends.prefix_le(k);
        (c + c2 as u64, s + s2)
    }

    /// Notes sounding at `k` (`start <= k < end`).
    #[inline]
    pub fn poly(&self, k: u64) -> u64 {
        self.starts_le(k).0 - self.ends_le(k).0
    }

    fn range(f: impl Fn(u64) -> (u64, u128), lo: Option<u64>, hi: u64) -> (u64, u128) {
        let (ch, sh) = f(hi);
        match lo {
            Some(l) if l < hi => {
                let (cl, sl) = f(l);
                (ch - cl, sh - sl)
            }
            Some(_) => (0, 0),
            None => (ch, sh),
        }
    }

    /// Count and sum of starts in `(lo, hi]`.
    pub fn starts_range(&self, lo: Option<u64>, hi: u64) -> (u64, u128) {
        Self::range(|k| self.starts_le(k), lo, hi)
    }

    /// Count and sum of ends in `(lo, hi]`.
    pub fn ends_range(&self, lo: Option<u64>, hi: u64) -> (u64, u128) {
        Self::range(|k| self.ends_le(k), lo, hi)
    }

    fn segs_in(&self, lo: Option<u64>, hi: u64) -> &[Arc<Seg>] {
        let a = lo.map_or(0, |l| self.seg_of(l.saturating_add(1)));
        let b = self.seg_of(hi);
        if lo.is_some_and(|l| l >= hi) {
            return &[];
        }
        &self.segs[a..=b]
    }

    /// Every segment as (last, starts, ends): tests compare builds exactly.
    #[cfg(test)]
    pub(crate) fn dump(&self) -> Vec<(u64, Vec<u64>, Vec<u64>)> {
        self.segs
            .iter()
            .map(|g| (g.last, g.starts.to_vec(), g.ends.to_vec()))
            .collect()
    }

    /// Every start in `(lo, hi]`, ascending.
    pub fn for_each_start_in(&self, lo: Option<u64>, hi: u64, f: &mut dyn FnMut(u64)) {
        for g in self.segs_in(lo, hi) {
            g.starts.for_each_in(lo, hi, &mut *f);
        }
    }

    /// Every end in `(lo, hi]`, ascending.
    pub fn for_each_end_in(&self, lo: Option<u64>, hi: u64, f: &mut dyn FnMut(u64)) {
        for g in self.segs_in(lo, hi) {
            g.ends.for_each_in(lo, hi, &mut *f);
        }
    }

    /// All starts and ends (tests).
    #[cfg(test)]
    fn all(&self) -> (Vec<u64>, Vec<u64>) {
        let mut s = Vec::new();
        let mut e = Vec::new();
        for g in &self.segs {
            s.extend(g.starts.to_vec());
            e.extend(g.ends.to_vec());
        }
        (s, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> u64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *seed >> 33
    }

    fn notes(seed: u64, n: usize) -> Vec<(u64, u64)> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                let a = lcg(&mut s) % 200_000;
                let d = if lcg(&mut s).is_multiple_of(10) {
                    0
                } else {
                    lcg(&mut s) % 3000
                };
                (a, a + d)
            })
            .collect()
    }

    fn build(ns: &[(u64, u64)], bounds: &[u64]) -> Timeline {
        build_on(ns, bounds, 0)
    }

    fn build_on(ns: &[(u64, u64)], bounds: &[u64], threads: usize) -> Timeline {
        Timeline::build(bounds, None, threads, |lo, hi, s, e| {
            for &(a, b) in ns {
                if lo.is_none_or(|l| a > l) && a <= hi {
                    s.push(a);
                }
                if lo.is_none_or(|l| b > l) && b <= hi {
                    e.push(b);
                }
            }
        })
        .unwrap()
    }

    #[test]
    fn prefix_queries_match_a_brute_force_count() {
        let ns = notes(7, 100_000);
        for bounds in [vec![], vec![1000, 50_000, 120_000]] {
            let t = build(&ns, &bounds);
            assert!(t.segments() > 5);
            assert_eq!(t.notes(), ns.len() as u64);
            let mut seed = 3;
            for _ in 0..300 {
                let k = lcg(&mut seed) % 210_000;
                let s = ns.iter().filter(|n| n.0 <= k).count() as u64;
                let p = ns.iter().filter(|n| n.0 <= k && k < n.1).count() as u64;
                assert_eq!(t.starts_le(k).0, s);
                assert_eq!(t.poly(k), p);
                let sum: u128 = ns
                    .iter()
                    .filter(|n| n.1 <= k)
                    .map(|n| u128::from(n.1))
                    .sum();
                assert_eq!(t.ends_le(k).1, sum);
                let lo = k / 2;
                let mut seen = 0u64;
                t.for_each_start_in(Some(lo), k, &mut |x| {
                    assert!(x > lo && x <= k);
                    seen += 1;
                });
                assert_eq!(seen, t.starts_range(Some(lo), k).0);
            }
        }
    }

    /// L-S38: windows cut on several threads give exactly the segments
    /// (and running counts) of the one-thread loop.
    #[test]
    fn parallel_windows_cut_the_same_segments() {
        let ns = notes(5, 120_000);
        let bounds = vec![10_000, 10_000, 30_000, 31_000, 90_000, 150_000];
        let one = build_on(&ns, &bounds, 0);
        for threads in [2, 3, 8] {
            let many = build_on(&ns, &bounds, threads);
            assert_eq!(many.dump(), one.dump(), "{threads} threads");
            assert_eq!(
                (many.cum_s.clone(), many.cum_e.clone()),
                (one.cum_s.clone(), one.cum_e.clone())
            );
            assert_eq!(many.max_tick, one.max_tick);
        }
        assert!(one.segments() > 8);
    }

    #[test]
    fn deltas_rewrite_only_the_touched_segments() {
        let mut ns = notes(11, 80_000);
        let t0 = build(&ns, &[]);
        // Remove 3 notes, add 4.
        let gone: Vec<(u64, u64)> = ns.drain(10..13).collect();
        let new = [(5, 9), (100_000, 100_000), (150_000, 160_000), (5, 7)];
        ns.extend_from_slice(&new);
        let sorted = |mut v: Vec<u64>| {
            v.sort_unstable();
            v
        };
        let (t1, rewritten) = t0.apply(
            &sorted(gone.iter().map(|n| n.0).collect()),
            &sorted(new.iter().map(|n| n.0).collect()),
            &sorted(gone.iter().map(|n| n.1).collect()),
            &sorted(new.iter().map(|n| n.1).collect()),
        );
        assert!(rewritten <= 14 && rewritten < t0.segments(), "{rewritten}");
        let fresh = build(&ns, &[]);
        assert_eq!(t1.all(), fresh.all());
        assert_eq!(t1.notes(), fresh.notes());
        for k in [0, 5, 6, 9, 99_999, 100_000, 155_000, 300_000] {
            assert_eq!(t1.poly(k), fresh.poly(k), "{k}");
            assert_eq!(t1.starts_le(k), fresh.starts_le(k));
        }
    }

    #[test]
    fn huge_stacks_on_one_tick_stay_in_one_segment() {
        let ns: Vec<(u64, u64)> = (0..50_000).map(|i| (100, 200 + (i % 3))).collect();
        let t = build(&ns, &[]);
        assert_eq!(t.poly(100), 50_000);
        assert_eq!(t.poly(200), 33_333);
        assert_eq!(t.poly(201), 16_666);
        assert_eq!(t.poly(202), 0);
        // Adding many more on the same tick splits nothing apart wrongly.
        let add: Vec<u64> = vec![100; 70_000];
        let add_e: Vec<u64> = vec![150; 70_000];
        let (t2, _) = t.apply(&[], &add, &[], &add_e);
        assert_eq!(t2.poly(100), 120_000);
        assert_eq!(t2.poly(150), 50_000);
        assert_eq!(Timeline::default().poly(5), 0);
    }

    #[test]
    fn window_bounds_follow_the_note_distribution() {
        let b = windows(vec![(0, 10), (100, 10), (200, 10), (300, 10), (300, 5)], 15);
        assert_eq!(b, vec![199, 299]);
        assert!(windows(Vec::new(), 10).is_empty());
    }
}
