//! `density_window`: how dense the music is around each note, on the
//! canonical definitions of `analysis/canonical.rs` — **NPS** at `t` counts
//! the starts in the trailing window `(t − W, t]` (all starts `≤ t` while
//! less than `W` has passed), **Poly** at `t` counts the notes sounding over
//! half-open `[start, end)` — so a tool's "notes per second" means what the
//! Inspector and the Progressor show. Times are whatever the caller measures
//! in (microseconds through [`super::tempo_time`] for NPS, ticks for tick
//! windows).

use super::super::TNote;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

/// For starts `times` sorted ascending: the number of starts in the
/// trailing window `(t − window, t]` at each start `t` — ties included (every
/// start at the same time counts). O(n).
pub fn trailing_counts(times: &[u64], window: u64) -> Vec<u32> {
    let mut out = vec![0; times.len()];
    let mut lo = 0;
    let mut hi = 0;
    for (k, &t) in times.iter().enumerate() {
        while hi < times.len() && times[hi] <= t {
            hi += 1;
        }
        while lo < hi && t.checked_sub(window).is_some_and(|edge| times[lo] <= edge) {
            lo += 1;
        }
        out[k] = (hi - lo) as u32;
    }
    out
}

/// For starts `times` sorted ascending: the times `t` whose trailing window
/// `(t − window, t]` holds at least `n` starts (the NPS count of
/// [`trailing_counts`] at any `t`, not only at starts), as sorted disjoint
/// half-open ranges, touching ranges merged (V09-S38, Clipping Mask n07:
/// `n` starts lie in the window exactly when some run of `n` consecutive
/// starts `s_i … s_{i+n−1}` has `s_{i+n−1} ≤ t < s_i + window`). O(n);
/// `n` = 0 counts as 1.
pub fn dense_ranges(times: &[u64], window: u64, n: usize) -> Vec<(u64, u64)> {
    let n = n.max(1);
    let mut out: Vec<(u64, u64)> = Vec::new();
    if window == 0 || times.len() < n {
        return out;
    }
    for i in 0..=times.len() - n {
        let (a, b) = (times[i + n - 1], times[i].saturating_add(window));
        if a >= b {
            continue;
        }
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// A trailing-window counter for greedy decisions made in time order: how
/// many accepted starts lie in `(t − window, t]`, then accept `t` or not.
#[derive(Clone, Debug)]
pub struct TrailingWindow {
    window: u64,
    times: VecDeque<u64>,
}

impl TrailingWindow {
    pub fn new(window: u64) -> TrailingWindow {
        TrailingWindow {
            window,
            times: VecDeque::new(),
        }
    }

    /// Accepted starts in `(t − window, t]` (times asked never decrease).
    pub fn count_at(&mut self, t: u64) -> usize {
        while self
            .times
            .front()
            .is_some_and(|&f| t.checked_sub(self.window).is_some_and(|edge| f <= edge))
        {
            self.times.pop_front();
        }
        self.times.len()
    }

    /// Accepts a start at `t` (not before the last one).
    pub fn push(&mut self, t: u64) {
        debug_assert!(self.times.back().is_none_or(|&b| b <= t));
        self.times.push_back(t);
    }
}

/// Range add / range max over trailing-window counts (the windows a start
/// lies in are a contiguous run of window ends): a bottom-up segment tree,
/// 32-bit, 2n + n entries, no recursion.
struct LoadTree {
    n: usize,
    h: u32,
    /// `t[n + i]` = value `i`; `t[p]` = max of its children + `d[p]`.
    t: Vec<i32>,
    /// Pending additions of internal nodes.
    d: Vec<i32>,
}

impl LoadTree {
    fn new(values: &[i32]) -> LoadTree {
        let n = values.len().max(1);
        let mut t = vec![i32::MIN; 2 * n];
        t[n..n + values.len()].copy_from_slice(values);
        for p in (1..n).rev() {
            t[p] = t[2 * p].max(t[2 * p + 1]);
        }
        LoadTree {
            n,
            h: usize::BITS - n.leading_zeros(),
            t,
            d: vec![0; n],
        }
    }

    /// A tree over `values` padded to a power of two (every node's leaves
    /// contiguous and in order, which [`LoadTree::first_at_least`] needs).
    fn padded(values: &[i32]) -> LoadTree {
        let n = values.len().max(1).next_power_of_two();
        let mut v = values.to_vec();
        v.resize(n, i32::MIN);
        LoadTree::new(&v)
    }

    /// The first position in `[a, b)` whose value is at least `v` (a
    /// [`LoadTree::padded`] tree): the covering nodes left to right, then a
    /// descent into the first one that reaches `v`.
    fn first_at_least(&mut self, a: usize, b: usize, v: i32) -> Option<usize> {
        if a >= b {
            return None;
        }
        let (mut l, mut r) = (a + self.n, b + self.n);
        self.push(l);
        self.push(r - 1);
        let (mut left, mut nl) = ([0usize; 64], 0usize);
        let (mut right, mut nr) = ([0usize; 64], 0usize);
        while l < r {
            if l & 1 == 1 {
                left[nl] = l;
                nl += 1;
                l += 1;
            }
            if r & 1 == 1 {
                r -= 1;
                right[nr] = r;
                nr += 1;
            }
            l >>= 1;
            r >>= 1;
        }
        let cover = left[..nl].iter().chain(right[..nr].iter().rev());
        for &p in cover {
            if self.t[p] >= v {
                let (mut p, mut acc) = (p, 0i32);
                while p < self.n {
                    acc = acc.saturating_add(self.d[p]);
                    let c = 2 * p;
                    p = if self.t[c].saturating_add(acc) >= v {
                        c
                    } else {
                        c + 1
                    };
                }
                return Some(p - self.n);
            }
        }
        None
    }

    fn apply(&mut self, p: usize, v: i32) {
        self.t[p] = self.t[p].saturating_add(v);
        if p < self.n {
            self.d[p] += v;
        }
    }

    /// Recomputes the ancestors of leaf-level node `p`.
    fn build(&mut self, mut p: usize) {
        while p > 1 {
            p >>= 1;
            self.t[p] = self.t[2 * p]
                .max(self.t[2 * p + 1])
                .saturating_add(self.d[p]);
        }
    }

    /// Pushes the pending additions above node `p` down to it.
    fn push(&mut self, p: usize) {
        for s in (1..=self.h).rev() {
            let i = p >> s;
            if i >= 1 && self.d[i] != 0 {
                let v = self.d[i];
                self.apply(2 * i, v);
                self.apply(2 * i + 1, v);
                self.d[i] = 0;
            }
        }
    }

    /// Adds `v` to positions `[a, b)`.
    fn range_add(&mut self, a: usize, b: usize, v: i32) {
        if a >= b {
            return;
        }
        let (mut l, mut r) = (a + self.n, b + self.n);
        let (l0, r0) = (l, r - 1);
        while l < r {
            if l & 1 == 1 {
                self.apply(l, v);
                l += 1;
            }
            if r & 1 == 1 {
                r -= 1;
                self.apply(r, v);
            }
            l >>= 1;
            r >>= 1;
        }
        self.build(l0);
        self.build(r0);
    }

    /// The largest value in `[a, b)` (`i32::MIN` when empty).
    fn range_max(&mut self, a: usize, b: usize) -> i32 {
        if a >= b {
            return i32::MIN;
        }
        let (mut l, mut r) = (a + self.n, b + self.n);
        self.push(l);
        self.push(r - 1);
        let mut m = i32::MIN;
        while l < r {
            if l & 1 == 1 {
                m = m.max(self.t[l]);
                l += 1;
            }
            if r & 1 == 1 {
                r -= 1;
                m = m.max(self.t[r]);
            }
            l >>= 1;
            r >>= 1;
        }
        m
    }
}

/// For starts `times` sorted ascending, the run of window ends `[lo, hi)`
/// whose trailing window `(t − window, t]` holds each start: two sweeps,
/// O(n).
fn holding_ranges(times: &[u64], window: u64) -> Vec<(u32, u32)> {
    let mut out = Vec::with_capacity(times.len());
    let mut hi = 0usize;
    for (q, &t) in times.iter().enumerate() {
        let lo = match out.last() {
            Some(&(l, _)) if q > 0 && times[q - 1] == t => l as usize,
            _ => q,
        };
        let edge = u128::from(t) + u128::from(window);
        hi = hi.max(q);
        while hi < times.len() && u128::from(times[hi]) < edge {
            hi += 1;
        }
        out.push((lo as u32, hi as u32));
    }
    out
}

/// The largest of `counts` over each range of `ranges` — both ends never
/// decrease from one range to the next — by a monotone deque. O(n).
fn sliding_max(counts: &[u32], ranges: &[(u32, u32)]) -> Vec<u32> {
    let mut out = Vec::with_capacity(ranges.len());
    let mut dq: VecDeque<usize> = VecDeque::new();
    let mut next = 0usize;
    for &(lo, hi) in ranges {
        let (lo, hi) = (lo as usize, hi as usize);
        while next < hi {
            while dq.back().is_some_and(|&b| counts[b] <= counts[next]) {
                dq.pop_back();
            }
            dq.push_back(next);
            next += 1;
        }
        while dq.front().is_some_and(|&f| f < lo) {
            dq.pop_front();
        }
        out.push(dq.front().map_or(0, |&f| counts[f]));
    }
    out
}

/// For starts `times` sorted ascending: at each start, the count of the
/// busiest trailing window `(t − window, t]` that holds it (≥ its own
/// window's count). O(n).
pub fn holding_loads(times: &[u64], window: u64) -> Vec<u32> {
    let counts = trailing_counts(times, window);
    sliding_max(&counts, &holding_ranges(times, window))
}

/// A budget on starts per trailing window (V09-S35, Flam n11 and the later
/// budget functions): which of the starts `times` (sorted ascending) to keep
/// so that every window `(t − window, t]` holds at most `cap` kept starts,
/// removing only the positions listed in `removable` — in that order (the
/// caller's priority), each one only while some window holding it is still
/// over `cap`. Positions not listed are always kept, so a window of them
/// alone may stay over. Deterministic; O(n log n) (O(n) when no window is
/// over: counts only fall, so a start in no over-full window stays).
pub fn budget_keep(times: &[u64], window: u64, cap: usize, removable: &[usize]) -> Vec<bool> {
    budget_keep_if(times, window, cap, removable, |_| true)
}

/// [`budget_keep`] where a start goes only when `allow(q)` agrees — asked
/// once for each listed start at the moment a window holding it is over the
/// budget (V09-S36, Join n04: a start goes by joining its fragment into the
/// note before it, which the caller's gates may refuse; a refused start
/// stays and counts).
#[inline]
pub fn budget_keep_if(
    times: &[u64],
    window: u64,
    cap: usize,
    removable: &[usize],
    allow: impl FnMut(usize) -> bool,
) -> Vec<bool> {
    budget_keep_if_by(times, window, |_| cap, removable, allow)
}

/// [`budget_keep_if`] with a budget per start (`cap(q)`; Join n04 under a
/// Control Sweep, E0422): start `q` goes while a window holding it holds
/// more than its own budget.
pub fn budget_keep_if_by(
    times: &[u64],
    window: u64,
    cap: impl Fn(usize) -> usize,
    removable: &[usize],
    mut allow: impl FnMut(usize) -> bool,
) -> Vec<bool> {
    let mut keep = vec![true; times.len()];
    if times.is_empty() {
        return keep;
    }
    let counts = trailing_counts(times, window);
    let ranges = holding_ranges(times, window);
    let loads = sliding_max(&counts, &ranges);
    let cap32 = |q: usize| u32::try_from(cap(q)).unwrap_or(u32::MAX);
    if !removable
        .iter()
        .any(|&q| q < times.len() && loads[q] > cap32(q))
    {
        return keep;
    }
    let counts: Vec<i32> = counts
        .into_iter()
        .map(|c| i32::try_from(c).unwrap_or(i32::MAX))
        .collect();
    let mut tree = LoadTree::new(&counts);
    for &q in removable {
        if q >= times.len() || !keep[q] || loads[q] <= cap32(q) {
            continue;
        }
        let cap = i32::try_from(cap(q)).unwrap_or(i32::MAX);
        let (lo, hi) = (ranges[q].0 as usize, ranges[q].1 as usize);
        if tree.range_max(lo, hi) > cap && allow(q) {
            keep[q] = false;
            tree.range_add(lo, hi, -1);
        }
    }
    keep
}

/// A cap on starts per trailing window kept by a sweep (V09-S35, Slide n11's
/// event rate): the starts `times` (sorted ascending) join in time order;
/// when a start makes its window `(t − window, t]` hold more than `cap` kept
/// starts, the kept removable start of that window with the lowest
/// `priority` goes (ties: the latest), so every window ends within the cap
/// unless only fixed starts are left in it. Unlike [`budget_keep`] (removals
/// in the caller's order, which may empty early windows), each removal is
/// forced by a window that is over at that moment, so dense stretches keep
/// `cap` per window. O(n log n).
pub fn sweep_cap_keep(
    times: &[u64],
    window: u64,
    cap: usize,
    priority: &[u64],
    removable: &[bool],
) -> Vec<bool> {
    use std::cmp::Reverse;
    use std::collections::{BTreeSet, VecDeque};
    let mut keep = vec![true; times.len()];
    let mut in_window: VecDeque<usize> = VecDeque::new();
    let mut kept = 0usize;
    let mut candidates: BTreeSet<(u64, Reverse<usize>)> = BTreeSet::new();
    for (k, &t) in times.iter().enumerate() {
        while let Some(&j) = in_window.front() {
            if t - times[j] < window {
                break;
            }
            in_window.pop_front();
            if keep[j] {
                kept -= 1;
                candidates.remove(&(priority[j], Reverse(j)));
            }
        }
        in_window.push_back(k);
        kept += 1;
        if removable[k] {
            candidates.insert((priority[k], Reverse(k)));
        }
        if kept > cap
            && let Some(&(p, Reverse(j))) = candidates.first()
        {
            candidates.remove(&(p, Reverse(j)));
            keep[j] = false;
            kept -= 1;
        }
    }
    keep
}

/// A budget per trailing window kept by a sweep (V09-S39, Score Stretch
/// n04): as [`sweep_cap_keep`], the starts `times` (sorted ascending) join
/// in time order, but the window `(t − window, t]` ending at start `k` (the
/// starts up to `k` in order) may hold at most `cap(k)` kept starts (the
/// budget is the window's, not the
/// removed start's: a stretch's windows each keep their source's density);
/// while it holds more, the kept removable start of that window with the
/// lowest `priority` goes (ties: the latest). Every window ending at a kept
/// start ends within its budget unless only fixed starts are left in it.
/// O(n log n).
pub fn sweep_budget_keep(
    times: &[u64],
    window: u64,
    cap: impl Fn(usize) -> usize,
    priority: &[u64],
    removable: &[bool],
) -> Vec<bool> {
    use std::cmp::Reverse;
    use std::collections::{BTreeSet, VecDeque};
    let mut keep = vec![true; times.len()];
    let mut in_window: VecDeque<usize> = VecDeque::new();
    let mut kept = 0usize;
    let mut candidates: BTreeSet<(u64, Reverse<usize>)> = BTreeSet::new();
    for (k, &t) in times.iter().enumerate() {
        while let Some(&j) = in_window.front() {
            if t - times[j] < window {
                break;
            }
            in_window.pop_front();
            if keep[j] {
                kept -= 1;
                candidates.remove(&(priority[j], Reverse(j)));
            }
        }
        in_window.push_back(k);
        kept += 1;
        if removable[k] {
            candidates.insert((priority[k], Reverse(k)));
        }
        let budget = cap(k);
        while kept > budget {
            let Some(&(p, Reverse(j))) = candidates.first() else {
                break;
            };
            candidates.remove(&(p, Reverse(j)));
            keep[j] = false;
            kept -= 1;
        }
    }
    keep
}

/// Poly at each note's start — the notes sounding at that tick, the note
/// itself included when it is not zero-length (`starts ≤ t − ends ≤ t`) —
/// for `idx` sorted by start. O(n log n).
pub fn poly_at_starts(notes: &[TNote], idx: &[usize]) -> Vec<u32> {
    let mut out = vec![0; idx.len()];
    let mut ends: BinaryHeap<Reverse<u64>> = BinaryHeap::new();
    let mut k = 0;
    while k < idx.len() {
        let t = notes[idx[k]].start;
        // Every note starting at t is counted at t.
        let mut j = k;
        while j < idx.len() && notes[idx[j]].start == t {
            ends.push(Reverse(notes[idx[j]].end()));
            j += 1;
        }
        while ends.peek().is_some_and(|Reverse(e)| *e <= t) {
            ends.pop();
        }
        for o in &mut out[k..j] {
            *o = ends.len() as u32;
        }
        k = j;
    }
    out
}

/// A polyphony ceiling reached by trimming ends, never by removing a note
/// (V09-S34, Articulate n11 and the later polyphony-bounded functions): for
/// `idx` sorted by start, the end each note keeps so that at most `cap`
/// notes sound at any tick (half-open `[start, end)`, as Poly counts) —
/// wherever trimming can do it. Poly only rises at an onset, so at each
/// onset tick where more than `cap` sound, sounding notes are cut to end
/// at that tick, smallest `rank` first, among those already at least
/// `shortest` long there (at least 1: a note starting at the tick is never
/// cut to nothing). Where no note may be cut the onset stays over the cap.
/// `rank(i)` orders note `i` (ties by index). Returns the ends in `idx`
/// order. O(n log n), no allocation per note beyond the tables.
pub fn poly_ceiling_ends<K: Ord + Copy>(
    notes: &[TNote],
    idx: &[usize],
    cap: usize,
    shortest: u64,
    rank: impl Fn(usize) -> K,
) -> Vec<u64> {
    poly_ceiling_ends_by(notes, idx, |_| cap, shortest, rank)
}

/// [`poly_ceiling_ends`] with a cap per note (`cap(i)` for note `i`;
/// Articulate n11 under a Control Sweep, E0422): at each onset tick the
/// smallest cap of the notes starting there holds.
pub fn poly_ceiling_ends_by<K: Ord + Copy>(
    notes: &[TNote],
    idx: &[usize],
    cap_of: impl Fn(usize) -> usize,
    shortest: u64,
    rank: impl Fn(usize) -> K,
) -> Vec<u64> {
    let mut end: Vec<u64> = idx.iter().map(|&i| notes[i].end()).collect();
    let mut gone = vec![false; idx.len()];
    // (end, position) of every sounding note; stale entries skipped.
    let mut ends: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::new();
    // (tick a note becomes long enough to cut, position).
    let mut pending: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::new();
    let mut ready: BinaryHeap<Reverse<(K, usize)>> = BinaryHeap::new();
    let shortest = shortest.max(1);
    let mut sounding = 0usize;
    let mut k = 0;
    while k < idx.len() {
        let t = notes[idx[k]].start;
        while let Some(&Reverse((e, j))) = ends.peek() {
            if e > t {
                break;
            }
            ends.pop();
            if !gone[j] {
                gone[j] = true;
                sounding -= 1;
            }
        }
        let mut cap = usize::MAX;
        while k < idx.len() && notes[idx[k]].start == t {
            cap = cap.min(cap_of(idx[k]));
            if end[k] > t {
                sounding += 1;
                ends.push(Reverse((end[k], k)));
                pending.push(Reverse((t.saturating_add(shortest), k)));
            } else {
                gone[k] = true;
            }
            k += 1;
        }
        while let Some(&Reverse((at, j))) = pending.peek() {
            if at > t {
                break;
            }
            pending.pop();
            if !gone[j] {
                ready.push(Reverse((rank(idx[j]), j)));
            }
        }
        while sounding > cap {
            let Some(Reverse((_, j))) = ready.pop() else {
                break;
            };
            if gone[j] {
                continue;
            }
            end[j] = t;
            gone[j] = true;
            sounding -= 1;
        }
    }
    end
}

/// Which notes stay so that at most `cap` sound at any tick (half-open
/// `[start, end)`, as Poly counts), per track or over all (V09-S35: Limit
/// n03, Chord Stamp n11): at each onset where more sound, sounding notes go
/// in `order` — the largest [`PolyOrder::Static`] key first (ties: the later
/// start, then the later note), or the middle of the sounding pitches (the
/// lower of two middles, so the outer voices stay longest and the top last;
/// a Fenwick tree over the 256 keys). Muted and zero-length notes never
/// sound and always stay. O(n log n).
pub enum PolyOrder<'a> {
    /// One key per note.
    Static(&'a [u64]),
    MiddlePitch,
}

pub fn poly_cap_keep(
    notes: &[TNote],
    cap: usize,
    per_track: bool,
    order: PolyOrder<'_>,
) -> Vec<bool> {
    poly_cap_keep_by(notes, |_| cap, per_track, order)
}

/// [`poly_cap_keep`] with a cap per note (`cap(i)`; Limit n03 / Chord
/// Stamp n11 under a Control Sweep, E0422): at each onset the smallest cap
/// of the notes starting there holds.
pub fn poly_cap_keep_by(
    notes: &[TNote],
    cap_of: impl Fn(usize) -> usize,
    per_track: bool,
    order: PolyOrder<'_>,
) -> Vec<bool> {
    use std::collections::BTreeSet;
    let n = notes.len();
    let mut keep = vec![true; n];
    let mut by: Vec<(u16, u64, u32)> = notes
        .iter()
        .enumerate()
        .filter(|(_, x)| x.dur > 0 && x.flags & super::super::FLAG_MUTED == 0)
        .map(|(i, x)| (if per_track { x.track } else { 0 }, x.start, i as u32))
        .collect();
    by.sort_unstable();
    let (inner, keys) = match order {
        PolyOrder::Static(k) => (false, k),
        PolyOrder::MiddlePitch => (true, &[][..]),
    };
    // 0 waiting, 1 sounding, 2 ended or removed.
    let mut state = vec![0u8; n];
    let mut ends: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
    // The static order: the top goes first.
    let mut prio: BinaryHeap<(u64, u64, u32)> = BinaryHeap::new();
    // The middle pitch: sounding notes per key (a Fenwick tree over the 256
    // keys) and, per key, by (start, index) — the newest goes first.
    let mut fen = [0i32; 257];
    let mut at_key: Vec<BTreeSet<(u64, u32)>> = if inner {
        vec![BTreeSet::new(); 256]
    } else {
        Vec::new()
    };
    let mut count = 0usize;
    let fen_add = |fen: &mut [i32; 257], p: u8, d: i32| {
        let mut k = usize::from(p) + 1;
        while k <= 256 {
            fen[k] += d;
            k += k & k.wrapping_neg();
        }
    };
    // The key holding the `r`-th sounding note (0-based) in pitch order.
    let fen_kth = |fen: &[i32; 257], r: usize| {
        let (mut at, mut left) = (0usize, r as i32 + 1);
        let mut step = 256;
        while step > 0 {
            if at + step <= 256 && fen[at + step] < left {
                at += step;
                left -= fen[at];
            }
            step /= 2;
        }
        at
    };
    let mut k = 0;
    while k < by.len() {
        let (scope, t, _) = by[k];
        if k > 0 && by[k - 1].0 != scope {
            // A new track: the previous one's notes have all ended.
            while let Some(Reverse((_, i))) = ends.pop() {
                if state[i as usize] == 1 {
                    state[i as usize] = 2;
                    if inner {
                        let x = &notes[i as usize];
                        fen_add(&mut fen, x.pitch, -1);
                        at_key[usize::from(x.pitch)].remove(&(x.start, i));
                    }
                }
            }
            prio.clear();
            count = 0;
        }
        while let Some(&Reverse((e, i))) = ends.peek() {
            if e > t {
                break;
            }
            ends.pop();
            if state[i as usize] == 1 {
                state[i as usize] = 2;
                count -= 1;
                if inner {
                    let x = &notes[i as usize];
                    fen_add(&mut fen, x.pitch, -1);
                    at_key[usize::from(x.pitch)].remove(&(x.start, i));
                }
            }
        }
        let mut cap = usize::MAX;
        while k < by.len() && by[k].0 == scope && by[k].1 == t {
            let i = by[k].2;
            let x = &notes[i as usize];
            cap = cap.min(cap_of(i as usize));
            state[i as usize] = 1;
            count += 1;
            ends.push(Reverse((x.end(), i)));
            if inner {
                fen_add(&mut fen, x.pitch, 1);
                at_key[usize::from(x.pitch)].insert((x.start, i));
            } else {
                prio.push((keys[i as usize], x.start, i));
            }
            k += 1;
        }
        while count > cap {
            let v = if inner {
                let p = fen_kth(&fen, (count - 1) / 2);
                let &(s, i) = at_key[p].iter().next_back().expect("a sounding note");
                at_key[p].remove(&(s, i));
                fen_add(&mut fen, p as u8, -1);
                i
            } else {
                loop {
                    let (_, _, i) = prio.pop().expect("a sounding note");
                    if state[i as usize] == 1 {
                        break i;
                    }
                }
            };
            state[v as usize] = 2;
            keep[v as usize] = false;
            count -= 1;
        }
    }
    keep
}

/// Extensions granted in a priority order while at most `cap` notes sound
/// (half-open, as Poly counts; V09-S36: Legato's polyphony-bounded legato).
/// `base`: intervals `[s, e)` that sound in any case; `ext[k] = (a, b)`:
/// note k's wanted extension `[a, b)` (none when `b <= a`), tried in
/// `order`. A granted extension sounds at every point of it, and is refused
/// wherever `cap` notes already sound: with `partial` it is granted up to
/// the first such point (nothing when that is `a`), else whole or not at
/// all. Returns each extension's granted end (`b` when `b <= a`, `a` when
/// refused). A range-add / range-max tree over the distinct times:
/// O((n + m) log(n + m)).
pub fn capped_extensions(
    base: &[(u64, u64)],
    ext: &[(u64, u64)],
    order: &[usize],
    cap: u32,
    partial: bool,
) -> Vec<u64> {
    capped_extensions_by(base, ext, order, |_| cap, partial)
}

/// [`capped_extensions`] with a cap per extension (`cap(k)` for `ext[k]`;
/// Legato n04 under a Control Sweep, E0422): each is granted while fewer
/// than its own cap sound.
pub fn capped_extensions_by(
    base: &[(u64, u64)],
    ext: &[(u64, u64)],
    order: &[usize],
    cap: impl Fn(usize) -> u32,
    partial: bool,
) -> Vec<u64> {
    let mut xs: Vec<u64> = base
        .iter()
        .filter(|iv| iv.1 > iv.0)
        .flat_map(|&(s, e)| [s, e])
        .chain(
            ext.iter()
                .filter(|iv| iv.1 > iv.0)
                .flat_map(|&(a, b)| [a, b]),
        )
        .collect();
    xs.sort_unstable();
    xs.dedup();
    let at = |t: u64| xs.partition_point(|&x| x < t);
    let m = xs.len().saturating_sub(1);
    let mut diff = vec![0i64; m + 1];
    for &(s, e) in base.iter().filter(|iv| iv.1 > iv.0) {
        diff[at(s)] += 1;
        diff[at(e)] -= 1;
    }
    let mut run = 0i64;
    let loads: Vec<i32> = diff[..m]
        .iter()
        .map(|d| {
            run += d;
            run.min(i64::from(i32::MAX)) as i32
        })
        .collect();
    let mut tree = LoadTree::padded(&loads);
    let mut out: Vec<u64> = ext.iter().map(|iv| iv.1).collect();
    for &k in order {
        let (a, b) = ext[k];
        if b <= a {
            continue;
        }
        let cap = cap(k).min(i32::MAX as u32) as i32;
        let (i0, i1) = (at(a), at(b));
        if partial {
            match tree.first_at_least(i0, i1, cap) {
                Some(j) => {
                    tree.range_add(i0, j, 1);
                    out[k] = xs[j];
                }
                None => tree.range_add(i0, i1, 1),
            }
        } else if tree.range_max(i0, i1) < cap {
            tree.range_add(i0, i1, 1);
        } else {
            out[k] = a;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dense_ranges` against the trailing count at every tick of small
    /// random cases (ties, windows shorter and longer than the gaps).
    #[test]
    fn dense_ranges_equal_the_trailing_count_at_every_tick() {
        let mut x: u64 = 0xD5E_0038;
        let mut next = |m: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m.max(1)
        };
        for case in 0..400 {
            let len = next(12) as usize;
            let mut times: Vec<u64> = (0..len).map(|_| next(60)).collect();
            times.sort_unstable();
            let window = next(25);
            let n = 1 + next(4) as usize;
            let got = dense_ranges(&times, window, n);
            for w in got.windows(2) {
                assert!(w[0].1 < w[1].0, "case {case}: not disjoint {got:?}");
            }
            for t in 0..100u64 {
                let count = times
                    .iter()
                    .filter(|&&s| s <= t && (t < window || s > t - window))
                    .count();
                let inside = got.iter().any(|&(a, b)| a <= t && t < b);
                assert_eq!(
                    inside,
                    window > 0 && count >= n,
                    "case {case}: t {t}, times {times:?}, W {window}, n {n}, {got:?}"
                );
            }
        }
    }

    /// `capped_extensions` against a tick-by-tick replay of its rule on
    /// small random cases (both modes), and the padded tree's first-at-least
    /// against a scan.
    #[test]
    fn capped_extensions_equal_a_tick_by_tick_replay() {
        let mut x: u64 = 0xCA9_0036;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for case in 0..400 {
            let nb = next(12) as usize;
            let ne = 1 + next(10) as usize;
            let base: Vec<(u64, u64)> = (0..nb)
                .map(|_| {
                    let s = next(60);
                    (s, s + next(20))
                })
                .collect();
            let ext: Vec<(u64, u64)> = (0..ne)
                .map(|_| {
                    let a = next(60);
                    (a, a + next(30))
                })
                .collect();
            let mut order: Vec<usize> = (0..ne).collect();
            order.sort_by_key(|&k| (next(1000), k));
            let cap = 1 + next(4) as u32;
            for partial in [true, false] {
                let got = capped_extensions(&base, &ext, &order, cap, partial);
                let mut load = [0u32; 200];
                for &(s, e) in &base {
                    for t in s..e {
                        load[t as usize] += 1;
                    }
                }
                let mut want = vec![0u64; ne];
                for (k, iv) in ext.iter().enumerate() {
                    want[k] = iv.1;
                }
                for &k in &order {
                    let (a, b) = ext[k];
                    if b <= a {
                        continue;
                    }
                    let blocked = (a..b).find(|&t| load[t as usize] >= cap);
                    let end = match (partial, blocked) {
                        (_, None) => b,
                        (true, Some(t)) => t,
                        (false, Some(_)) => a,
                    };
                    for t in a..end {
                        load[t as usize] += 1;
                    }
                    want[k] = end;
                }
                assert_eq!(got, want, "case {case} partial {partial}");
            }
        }
        // The padded tree's descent on uneven sizes.
        for len in [1usize, 2, 3, 5, 7, 13, 33] {
            let vals: Vec<i32> = (0..len).map(|_| next(5) as i32).collect();
            let mut t = LoadTree::padded(&vals);
            t.range_add(1.min(len), len, 1);
            let mut v2 = vals.clone();
            for x in v2.iter_mut().skip(1) {
                *x += 1;
            }
            for a in 0..len {
                for b in a..=len {
                    for thr in 0..7 {
                        let want = (a..b).find(|&i| v2[i] >= thr);
                        assert_eq!(t.first_at_least(a, b, thr), want, "{len} {a} {b} {thr}");
                    }
                }
            }
        }
    }

    #[test]
    fn trailing_counts_equal_the_canonical_window_definition() {
        let times = [0u64, 0, 5, 9, 10, 10, 19, 20, 35, 100];
        for w in [1u64, 5, 10, 11, 50, 1000] {
            let c = trailing_counts(&times, w);
            for (k, &t) in times.iter().enumerate() {
                let brute = times
                    .iter()
                    .filter(|&&s| s <= t && (t < w || s > t - w))
                    .count() as u32;
                assert_eq!(c[k], brute, "w {w} t {t}");
            }
        }
        assert!(trailing_counts(&[], 10).is_empty());
        // Times at the far end of the range never overflow.
        let far = [u64::MAX - 5, u64::MAX - 1, u64::MAX];
        assert_eq!(trailing_counts(&far, 10), vec![1, 2, 3]);
        let mut tw = TrailingWindow::new(u64::MAX);
        tw.push(u64::MAX - 1);
        assert_eq!(tw.count_at(u64::MAX), 1);
        // The greedy counter: keep at most 2 starts per 10.
        let mut tw = TrailingWindow::new(10);
        let mut kept = Vec::new();
        for &t in &times {
            if tw.count_at(t) < 2 {
                tw.push(t);
                kept.push(t);
            }
        }
        assert_eq!(kept, vec![0, 0, 10, 10, 20, 35, 100]);
        let c = trailing_counts(&kept, 10);
        assert!(c.iter().all(|&n| n <= 2), "{c:?}");
    }

    /// Brute force: the kept starts in the trailing window ending at `t`.
    fn kept_in(times: &[u64], keep: &[bool], t: u64, w: u64) -> usize {
        times
            .iter()
            .zip(keep)
            .filter(|&(&s, &k)| k && s <= t && (t < w || s > t - w))
            .count()
    }

    #[test]
    fn holding_loads_are_the_busiest_window_holding_each_start() {
        let times = [0u64, 0, 5, 9, 10, 10, 19, 20, 35, 100, 101, 101];
        for w in [1u64, 5, 10, 11, 50, 1000] {
            let l = holding_loads(&times, w);
            let all = vec![true; times.len()];
            for (q, &t) in times.iter().enumerate() {
                let brute = times
                    .iter()
                    .filter(|&&e| e >= t && e - t < w)
                    .map(|&e| kept_in(&times, &all, e, w))
                    .max()
                    .unwrap() as u32;
                assert_eq!(l[q], brute, "w {w} q {q}");
            }
        }
        assert!(holding_loads(&[], 10).is_empty());
    }

    #[test]
    fn sweep_cap_keep_keeps_every_window_within_the_cap_by_forced_removals() {
        // A seeded sweep against a brute-force replay of the rule (in time
        // order a start joins; while its window holds more than the cap, the
        // lowest-priority removable kept start of the window goes, ties the
        // latest); every window ends within the cap unless only fixed starts
        // are left in it.
        let mut x: u64 = 0x05EE_9CA9;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for round in 0..200 {
            let n = 1 + next(60) as usize;
            let mut times: Vec<u64> = (0..n).map(|_| next(200)).collect();
            times.sort_unstable();
            let w = 1 + next(40);
            let cap = 1 + next(5) as usize;
            let prio: Vec<u64> = (0..n).map(|_| next(10)).collect();
            let removable: Vec<bool> = (0..n).map(|_| next(4) != 0).collect();
            let keep = sweep_cap_keep(&times, w, cap, &prio, &removable);
            let mut brute = vec![true; n];
            for k in 0..n {
                let win: Vec<usize> = (0..=k).filter(|&j| times[k] - times[j] < w).collect();
                if win.iter().filter(|&&j| brute[j]).count() > cap
                    && let Some(&j) = win
                        .iter()
                        .filter(|&&j| brute[j] && removable[j])
                        .min_by_key(|&&j| (prio[j], std::cmp::Reverse(j)))
                {
                    brute[j] = false;
                }
            }
            assert_eq!(keep, brute, "round {round}");
            for k in 0..n {
                let win = (0..n).filter(|&j| times[j] <= times[k] && times[k] - times[j] < w);
                let (mut kept, mut fixed) = (0, 0);
                for j in win {
                    if keep[j] {
                        kept += 1;
                        fixed += usize::from(!removable[j]);
                    }
                }
                assert!(
                    kept <= cap.max(fixed),
                    "round {round}: window at {}",
                    times[k]
                );
            }
        }
        // A dense uniform stretch keeps the cap per window, not the cap in
        // all: 512 starts over two windows, cap 100.
        let times: Vec<u64> = (0..512u64).map(|k| k * 3906).collect();
        let keep = sweep_cap_keep(&times, 1_000_000, 100, &[0; 512], &[true; 512]);
        let n = keep.iter().filter(|&&k| k).count();
        assert!((195..=205).contains(&n), "{n}");
    }

    #[test]
    fn sweep_budget_keep_keeps_every_window_within_its_own_budget() {
        // A seeded sweep against a brute-force replay (in time order a start
        // joins; while the window ending at it holds more than its budget,
        // the lowest-priority removable kept start of the window goes, ties
        // the latest); a constant budget whose windows never go over by more
        // than one equals `sweep_cap_keep`.
        let mut x: u64 = 0x0B0D_6E75;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for round in 0..200 {
            let n = 1 + next(60) as usize;
            let mut times: Vec<u64> = (0..n).map(|_| next(200)).collect();
            times.sort_unstable();
            let w = 1 + next(40);
            let caps: Vec<usize> = (0..n).map(|_| 1 + next(5) as usize).collect();
            let prio: Vec<u64> = (0..n).map(|_| next(10)).collect();
            let removable: Vec<bool> = (0..n).map(|_| next(4) != 0).collect();
            let keep = sweep_budget_keep(&times, w, |k| caps[k], &prio, &removable);
            let mut brute = vec![true; n];
            for k in 0..n {
                let win: Vec<usize> = (0..=k).filter(|&j| times[k] - times[j] < w).collect();
                while win.iter().filter(|&&j| brute[j]).count() > caps[k] {
                    let Some(&j) = win
                        .iter()
                        .filter(|&&j| brute[j] && removable[j])
                        .min_by_key(|&&j| (prio[j], std::cmp::Reverse(j)))
                    else {
                        break;
                    };
                    brute[j] = false;
                }
            }
            assert_eq!(keep, brute, "round {round}");
            // The window ending at start k: the starts up to k in order
            // (later starts at the same tick join the windows ending at
            // them).
            for k in (0..n).filter(|&k| keep[k]) {
                let win = (0..=k).filter(|&j| times[k] - times[j] < w);
                let (mut kept, mut fixed) = (0, 0);
                for j in win {
                    if keep[j] {
                        kept += 1;
                        fixed += usize::from(!removable[j]);
                    }
                }
                assert!(kept <= caps[k].max(fixed), "round {round}");
            }
            let all = vec![true; n];
            assert_eq!(
                sweep_budget_keep(&times, w, |_| 3, &prio, &all),
                sweep_cap_keep(&times, w, 3, &prio, &all),
                "round {round}"
            );
        }
    }

    #[test]
    fn budget_keep_removes_only_listed_starts_in_order_until_every_window_fits() {
        // A seeded sweep against a brute-force replay of the rule (in the
        // listed order, a start goes exactly when a window holding it is over
        // the cap at that moment); after the run every window is within the
        // cap unless no removable start is left in it.
        let mut x: u64 = 0xB0D6E7;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for round in 0..200 {
            let n = 1 + next(60) as usize;
            let mut times: Vec<u64> = (0..n).map(|_| next(200)).collect();
            times.sort_unstable();
            let w = 1 + next(40);
            let cap = 1 + next(5) as usize;
            let mut removable: Vec<usize> = (0..n).filter(|_| next(3) != 0).collect();
            // Some caller priority: reversed, then rotated.
            removable.reverse();
            let r = removable.len().min(next(5) as usize);
            removable.rotate_left(r);
            let keep = budget_keep(&times, w, cap, &removable);
            let mut brute = vec![true; n];
            for &q in &removable {
                let over = times
                    .iter()
                    .filter(|&&e| e >= times[q] && e - times[q] < w)
                    .any(|&e| kept_in(&times, &brute, e, w) > cap);
                if over {
                    brute[q] = false;
                }
            }
            assert_eq!(keep, brute, "round {round}");
            for &e in &times {
                if kept_in(&times, &keep, e, w) > cap {
                    // Over the cap only with no removable start left in it.
                    let left = times.iter().enumerate().any(|(q, &s)| {
                        keep[q] && removable.contains(&q) && s <= e && (e < w || s > e - w)
                    });
                    assert!(!left, "round {round}: window at {e} still over");
                }
            }
        }
        // The order decides which go: cap 1 in one window, the first listed
        // removable starts go first.
        assert_eq!(
            budget_keep(&[0, 1, 2, 3], 10, 1, &[3, 2, 1, 0]),
            vec![true, false, false, false]
        );
        assert_eq!(
            budget_keep(&[0, 1, 2, 3], 10, 2, &[0, 1, 2, 3]),
            vec![false, false, true, true]
        );
        assert_eq!(budget_keep(&[5, 5], 1, 1, &[]), vec![true, true]);
    }

    /// V09-S36 (Join n04): `budget_keep_if` is `budget_keep` with a veto —
    /// asked only for a start a window holding it is over for at that
    /// moment, a vetoed start stays and counts; brute-force replays on 200
    /// seeded cases, the veto every third question.
    #[test]
    fn budget_keep_if_asks_only_when_over_and_keeps_vetoed_starts() {
        let mut x: u64 = 0x0005_EED0_F70B;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for round in 0..200 {
            let n = 1 + next(60) as usize;
            let mut times: Vec<u64> = (0..n).map(|_| next(200)).collect();
            times.sort_unstable();
            let w = 1 + next(40);
            let cap = 1 + next(5) as usize;
            let removable: Vec<usize> = (0..n).rev().filter(|_| next(3) != 0).collect();
            let mut asked = Vec::new();
            let keep = budget_keep_if(&times, w, cap, &removable, |q| {
                asked.push(q);
                !asked.len().is_multiple_of(3)
            });
            let mut brute = vec![true; n];
            let mut questions = 0usize;
            let mut brute_asked = Vec::new();
            for &q in &removable {
                let over = times
                    .iter()
                    .filter(|&&e| e >= times[q] && e - times[q] < w)
                    .any(|&e| kept_in(&times, &brute, e, w) > cap);
                if over {
                    questions += 1;
                    brute_asked.push(q);
                    if !questions.is_multiple_of(3) {
                        brute[q] = false;
                    }
                }
            }
            assert_eq!(asked, brute_asked, "round {round}");
            assert_eq!(keep, brute, "round {round}");
        }
        // Always allowing is `budget_keep`.
        assert_eq!(
            budget_keep_if(&[0, 1, 2, 3], 10, 1, &[3, 2, 1, 0], |_| true),
            budget_keep(&[0, 1, 2, 3], 10, 1, &[3, 2, 1, 0])
        );
        // A veto keeps the start: the next listed one goes instead.
        assert_eq!(
            budget_keep_if(&[0, 1, 2, 3], 10, 3, &[3, 2], |q| q != 3),
            vec![true, true, false, true]
        );
    }

    #[test]
    fn poly_at_starts_counts_half_open_sounding_notes() {
        let n = |start, dur| TNote {
            start,
            dur,
            pitch: 60,
            ..Default::default()
        };
        let notes = vec![n(0, 10), n(0, 0), n(5, 10), n(10, 5), n(10, 5), n(20, 1)];
        let idx: Vec<usize> = (0..notes.len()).collect();
        let p = poly_at_starts(&notes, &idx);
        for (k, &i) in idx.iter().enumerate() {
            let t = notes[i].start;
            let brute = notes.iter().filter(|m| m.start <= t && t < m.end()).count() as u32;
            assert_eq!(p[k], brute, "note {i}");
        }
        assert_eq!(p, vec![1, 1, 2, 3, 3, 1]);
    }

    /// Poly at `t` of notes with the given ends (brute force).
    fn poly(notes: &[TNote], end: &[u64], t: u64) -> usize {
        notes
            .iter()
            .zip(end)
            .filter(|(m, e)| m.start <= t && t < **e)
            .count()
    }

    #[test]
    fn poly_ceiling_trims_ends_to_the_cap_in_rank_order() {
        let n = |start, dur, vel| TNote {
            start,
            dur,
            vel,
            pitch: 60,
            ..Default::default()
        };
        // Four notes: at tick 30 four sound; cap 2.
        let notes = vec![n(0, 100, 50), n(10, 40, 90), n(20, 200, 70), n(30, 10, 60)];
        let idx: Vec<usize> = (0..notes.len()).collect();
        // Oldest first: at 20 three sound → note 0 cut at 20; at 30 note 1
        // is the oldest still sounding → cut at 30.
        let e = poly_ceiling_ends(&notes, &idx, 2, 1, |i| notes[i].start);
        assert_eq!(e, vec![20, 30, 220, 40]);
        // Longest first: at 20 note 2 (200) is too new (shortest 15), note 0
        // (100) goes; at 30 note 2 (now 10 long) still too new, note 1 goes.
        let e = poly_ceiling_ends(&notes, &idx, 2, 15, |i| std::cmp::Reverse(notes[i].dur));
        assert_eq!(e, vec![20, 30, 220, 40]);
        // Quietest first, shortest 5: at 20 note 0 (50) goes; at 30 note 2
        // (70) is quieter than note 1 (90) and 10 long → cut.
        let e = poly_ceiling_ends(&notes, &idx, 2, 5, |i| notes[i].vel);
        assert_eq!(e, vec![20, 50, 30, 40]);
        // A shortest length nobody reaches leaves the overload.
        let e = poly_ceiling_ends(&notes, &idx, 1, 1000, |i| notes[i].start);
        assert_eq!(e, notes.iter().map(|m| m.end()).collect::<Vec<_>>());
    }

    #[test]
    fn poly_ceiling_holds_everywhere_trimming_can_and_never_removes_a_note() {
        // A deterministic pseudo-random stack; every result note keeps a
        // positive length, never grows, and Poly ≤ cap at every onset where
        // some sounding note was cuttable.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut r = |m: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m
        };
        for cap in [1usize, 2, 3, 7] {
            for shortest in [1u64, 4, 30] {
                let mut notes: Vec<TNote> = (0..300)
                    .map(|_| TNote {
                        start: r(2000),
                        dur: 1 + r(300),
                        vel: r(128) as u8,
                        pitch: 60,
                        ..Default::default()
                    })
                    .collect();
                notes.sort_by_key(|m| m.start);
                let idx: Vec<usize> = (0..notes.len()).collect();
                let e = poly_ceiling_ends(&notes, &idx, cap, shortest, |i| notes[i].vel);
                for (i, m) in notes.iter().enumerate() {
                    assert!(e[i] > m.start && e[i] <= m.end());
                    assert!(e[i] == m.end() || e[i] - m.start >= shortest);
                }
                for m in &notes {
                    let t = m.start;
                    if poly(&notes, &e, t) > cap {
                        // Every note still sounding at t is too new to cut.
                        assert!(
                            notes
                                .iter()
                                .zip(&e)
                                .filter(|(o, oe)| o.start <= t && t < **oe)
                                .all(|(o, _)| t - o.start < shortest),
                            "cap {cap} shortest {shortest} at {t}"
                        );
                    }
                }
            }
        }
    }

    /// `poly_cap_keep` against a brute-force replay of its rule: at each
    /// onset, while more than the cap sound, remove the largest key (ties:
    /// the later start, then the later note) or the newest note at the
    /// lower middle pitch.
    #[test]
    fn poly_cap_keep_equals_a_brute_force_replay() {
        let mut x: u64 = 0x9013_55AB;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for case in 0..200 {
            let n = 1 + next(60) as usize;
            let notes: Vec<TNote> = (0..n)
                .map(|_| TNote {
                    start: next(400),
                    dur: next(120),
                    pitch: next(30) as u8 + 50,
                    vel: 1 + next(127) as u8,
                    track: next(2) as u16,
                    flags: if next(15) == 0 { 2 } else { 0 },
                    ..TNote::default()
                })
                .collect();
            let keys: Vec<u64> = (0..n).map(|_| next(5)).collect();
            let cap = 1 + next(4) as usize;
            let per_track = case % 2 == 0;
            for middle in [false, true] {
                let order = if middle {
                    PolyOrder::MiddlePitch
                } else {
                    PolyOrder::Static(&keys)
                };
                let got = poly_cap_keep(&notes, cap, per_track, order);
                // Brute force.
                let mut keep = vec![true; n];
                let mut onsets: Vec<(u16, u64)> = notes
                    .iter()
                    .map(|x| (if per_track { x.track } else { 0 }, x.start))
                    .collect();
                onsets.sort_unstable();
                onsets.dedup();
                for (scope, t) in onsets {
                    loop {
                        let sounding: Vec<usize> = (0..n)
                            .filter(|&i| {
                                let x = &notes[i];
                                keep[i]
                                    && x.flags & 2 == 0
                                    && x.dur > 0
                                    && (!per_track || x.track == scope)
                                    && x.start <= t
                                    && t < x.end()
                            })
                            .collect();
                        if sounding.len() <= cap {
                            break;
                        }
                        let v = if middle {
                            let mut ps: Vec<u8> =
                                sounding.iter().map(|&i| notes[i].pitch).collect();
                            ps.sort_unstable();
                            let p = ps[(ps.len() - 1) / 2];
                            *sounding
                                .iter()
                                .filter(|&&i| notes[i].pitch == p)
                                .max_by_key(|&&i| (notes[i].start, i))
                                .unwrap()
                        } else {
                            *sounding
                                .iter()
                                .max_by_key(|&&i| (keys[i], notes[i].start, i))
                                .unwrap()
                        };
                        keep[v] = false;
                    }
                }
                assert_eq!(got, keep, "case {case} middle {middle}");
            }
        }
    }
}
