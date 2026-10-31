//! Peak statistics: the highest poly and NPS reached up to a position, and the
//! highest poly inside the trailing window.
//!
//! Peaks need the whole timeline before the position, so they are answered
//! from a [`PeakProfile`]: one sweep over every note start and end in tick
//! order (streamed window by window, never holding all events at once) records,
//! for blocks of [`BLOCK_TICKS`] consecutive distinct event ticks, the block's
//! first tick and its maximum poly and NPS. A query combines the running maxima
//! of the complete blocks before the position with an exact sweep of the one
//! partial block. The profile is optional: it is built in the background only
//! when a peak is shown, and rebuilt when notes or the tempo map change.
//!
//! Without a profile, the window peak is computed exactly by sweeping the
//! events of the window ([`window_peak_poly`]).
//!
//! With [`Peaks::WithPps`] (v0.9 V09-S19, `docs/SELECTION_PEAKS.md`) the same
//! sweep also finds the highest PPS at **any** tick — exactly, from a finite
//! candidate set (event ticks and the two ticks around each window exit,
//! pruned by the slope of the window area) — and the Poly Area in
//! note·nanoseconds.

use super::canonical::{PolySweep, window_lo};
use super::{AnalysisIndex, Scope, TickDelta, Timing};
use crate::model::tempo::TempoCursor;
use crate::wide::U256;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

/// Distinct event ticks per profile block.
pub const BLOCK_TICKS: usize = 256;
/// Upper bound on note starts collected per sweep window while building.
const SWEEP_STARTS: u64 = 1 << 18;

/// Which peaks a profile keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Peaks {
    /// Peak Poly and Peak NPS (the Progressor, the Inspector).
    #[default]
    PolyNps,
    /// Also Peak PPS and the Poly Area in note·ns (Selection Analytics).
    WithPps,
}

/// A PPS value as an exact ratio `num / den` (v0.9 V09-S19): the window's
/// Poly Area in note·frames × 2^32 over the window's length in frames ×
/// 2^32 — or the poly over 1 at time 0. Compared exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    pub num: u128,
    pub den: u128,
}

impl Ratio {
    pub const ZERO: Ratio = Ratio { num: 0, den: 1 };

    /// `self > other`, exactly (256-bit cross products).
    #[inline]
    pub fn exceeds(&self, other: &Ratio) -> bool {
        if self.den == other.den {
            // Every full window has the same length: the common case.
            return self.num > other.num;
        }
        U256::mul_u128_u128(self.num, other.den) > U256::mul_u128_u128(other.num, self.den)
    }

    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Block {
    first: u64,
    max_poly: u64,
    max_nps: u64,
    /// Highest PPS at any tick of the block's range `[first, next first)`
    /// and the first tick reaching it ([`Peaks::WithPps`]; else zero).
    max_pps: Ratio,
    pps_tick: u64,
}

impl Block {
    fn new(first: u64, poly: u64, nps: u64) -> Block {
        Block {
            first,
            max_poly: poly,
            max_nps: nps,
            max_pps: Ratio::ZERO,
            pps_tick: first,
        }
    }
}

/// Peak summary of one scope at one tempo map (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeakProfile {
    blocks: Vec<Block>,
    /// Running maxima over blocks `0..=i`.
    run_poly: Vec<u64>,
    run_nps: Vec<u64>,
    window_ns: u64,
    peaks: Peaks,
    /// Poly Area in note·ns (`Σ nanos(end) − nanos(start)`; WithPps).
    area_ns: u128,
    pub events: u64,
}

/// Distinct ticks of `starts` and `ends` (both sorted) in ascending order with
/// the number of starts and ends at each: `(tick, starts, ends)`.
fn merge_ticks(starts: &[u64], ends: &[u64], mut f: impl FnMut(u64, u64, u64)) {
    let (mut i, mut j) = (0, 0);
    while i < starts.len() || j < ends.len() {
        let t = match (starts.get(i), ends.get(j)) {
            (Some(&a), Some(&b)) => a.min(b),
            (Some(&a), None) => a,
            (None, Some(&b)) => b,
            (None, None) => unreachable!(),
        };
        let (mut s, mut e) = (0u64, 0u64);
        while starts.get(i) == Some(&t) {
            s += 1;
            i += 1;
        }
        while ends.get(j) == Some(&t) {
            e += 1;
            j += 1;
        }
        f(t, s, e);
    }
}

/// Sorted `plus` minus the sorted sub-multiset `minus`.
fn without(plus: Vec<u64>, minus: &[u64]) -> Vec<u64> {
    if minus.is_empty() {
        return plus;
    }
    let mut out = Vec::with_capacity(plus.len() - minus.len().min(plus.len()));
    let mut j = 0;
    for x in plus {
        if minus.get(j) == Some(&x) {
            j += 1;
        } else {
            out.push(x);
        }
    }
    out
}

/// The scope's start ticks in `(lo, hi]`, sorted (weights resolved).
fn starts_sorted(index: &AnalysisIndex, scope: &Scope, lo: Option<u64>, hi: u64) -> Vec<u64> {
    let (mut p, mut m) = (Vec::new(), Vec::new());
    index.for_each_start_in(
        scope,
        lo,
        hi,
        |x, w| if w > 0 { p.push(x) } else { m.push(x) },
    );
    p.sort_unstable();
    m.sort_unstable();
    without(p, &m)
}

fn collect(index: &AnalysisIndex, scope: &Scope, lo: Option<u64>, hi: u64) -> (Vec<u64>, Vec<u64>) {
    let starts = starts_sorted(index, scope, lo, hi);
    let (mut p, mut m) = (Vec::new(), Vec::new());
    index.for_each_end_in(
        scope,
        lo,
        hi,
        |x, w| if w > 0 { p.push(x) } else { m.push(x) },
    );
    p.sort_unstable();
    m.sort_unstable();
    (starts, without(p, &m))
}

/// Highest poly at any tick in `(lo, hi]`, also counting the poly at `lo`
/// itself when `lo` is given (it holds until the first event after it).
/// Exact; costs O(events in the range · log).
pub fn window_peak_poly(index: &AnalysisIndex, scope: &Scope, lo: Option<u64>, hi: u64) -> u64 {
    let mut sweep = PolySweep::starting_at(lo.map_or(0, |l| index.poly(scope, l)));
    let (starts, ends) = collect(index, scope, lo, hi);
    merge_ticks(&starts, &ends, |t, s, e| {
        sweep.tick(t, s, e);
    });
    sweep.peak
}

/// NPS (window `window_ns`) at `tick`: the starts in its trailing window.
fn nps_at(index: &AnalysisIndex, scope: &Scope, timing: &Timing, window_ns: u64, tick: u64) -> u64 {
    let lo = window_lo(timing, timing.nanos_at(tick), window_ns);
    index.starts_in(scope, lo, tick).0
}

/// Highest NPS (window `window_ns`) at any tick in `(lo, hi]`, also counting
/// the NPS at `lo` itself when `lo` is given (between note starts NPS only
/// falls, so the maximum is at `lo` or at a start). Exact; costs O(starts in
/// the range · log).
pub fn window_peak_nps(
    index: &AnalysisIndex,
    scope: &Scope,
    timing: &Timing,
    window_ns: u64,
    lo: Option<u64>,
    hi: u64,
) -> u64 {
    let mut best = lo.map_or(0, |l| nps_at(index, scope, timing, window_ns, l));
    if lo.is_some_and(|l| l >= hi) {
        return best;
    }
    let mut onsets = starts_sorted(index, scope, lo, hi);
    onsets.dedup();
    for x in onsets {
        best = best.max(nps_at(index, scope, timing, window_ns, x));
    }
    best
}

/// The ticks a delta can change a profile at (merged, inclusive spans):
/// every changed tick, the ranges where the poly differs (the running sum of
/// the delta's starts minus ends is not 0) and, for every changed start, the
/// ticks whose trailing window (`window_ns`) holds it — up to
/// `tick_at_nanos(nanos_at(s) + window)`, after which `window_lo >= s`.
/// With `pps` (V09-S19), a poly-difference span `[a, b]` reaches one window
/// further, to the first tick whose window starts after `b` (+1): PPS
/// integrates the poly over the trailing window.
fn dirty_spans(delta: &TickDelta, timing: &Timing, window_ns: u64, pps: bool) -> Vec<(u64, u64)> {
    let mut ev: Vec<(u64, i64)> = Vec::with_capacity(delta.len());
    ev.extend(delta.added_starts.iter().map(|&t| (t, 1)));
    ev.extend(delta.removed_starts.iter().map(|&t| (t, -1)));
    ev.extend(delta.added_ends.iter().map(|&t| (t, -1)));
    ev.extend(delta.removed_ends.iter().map(|&t| (t, 1)));
    ev.sort_unstable_by_key(|e| e.0);
    let mut spans = Vec::new();
    let (mut d, mut open, mut i) = (0i64, None, 0);
    while i < ev.len() {
        let t = ev[i].0;
        let from = *open.get_or_insert(t);
        while ev.get(i).is_some_and(|e| e.0 == t) {
            d += ev[i].1;
            i += 1;
        }
        if d == 0 {
            let to = if pps {
                let exit = timing
                    .map()
                    .first_tick_at_or_after_frame(timing.nanos_at(t).saturating_add(window_ns));
                exit.saturating_add(1).max(t)
            } else {
                t
            };
            spans.push((from, to));
            open = None;
        }
    }
    if let Some(from) = open {
        spans.push((from, u64::MAX));
    }
    for &st in delta.added_starts.iter().chain(&delta.removed_starts) {
        let end = timing.tick_at_nanos(timing.nanos_at(st).saturating_add(window_ns));
        spans.push((st, end.max(st)));
    }
    spans.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        match merged.last_mut() {
            Some(m) if a <= m.1.saturating_add(1) => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// A distinct event tick still inside the trailing window: the first tick
/// whose window no longer holds it (`exit`), its net poly change `d`
/// (starts − ends) and `d × fx` (its time in frames × 2^32).
#[derive(Clone, Copy, Debug)]
struct Held {
    exit: u64,
    d: i64,
    dfx: i128,
}

/// The Peak PPS sweep (`docs/SELECTION_PEAKS.md`). Event ticks are fed in
/// order; window exits and the switch to a full window are boundaries in
/// between. The window area (frames × 2^32, exact)
/// `A(c) = p(kₐ)·W + (P − p(kₐ))·fx(c) − Σ_held d·fx` is linear on every
/// stretch between boundaries, so it is evaluated only at the stretch ends
/// the slope rule selects (the end of a rising stretch, the start of any
/// other) and at forced ticks; `out(tick, value)` gets each evaluation.
struct PpsSweep<'t> {
    w: u64,
    wfx: i128,
    /// First tick whose window is full (`nanos >= W`).
    tw: u64,
    tw_passed: bool,
    ticks: TempoCursor<'t>,
    frames: TempoCursor<'t>,
    held: VecDeque<Held>,
    /// Poly after the events fed, and poly at `kₐ` (the events that left).
    p: i64,
    pa: i64,
    /// Σ d·fx over `held` (before the window is full: every event fed).
    dsum: i128,
    /// −Σ d·nanos over the events fed: Poly Area in note·ns (a full build).
    area: i128,
    /// Ticks evaluated whatever the slope (a ranged update's range ends).
    forced: VecDeque<u64>,
}

impl<'t> PpsSweep<'t> {
    fn new(timing: &'t Timing, window_ns: u64) -> PpsSweep<'t> {
        let map = timing.map();
        PpsSweep {
            w: window_ns,
            wfx: i128::from(window_ns) << crate::model::tempo::FX_SHIFT,
            tw: map.first_tick_at_or_after_frame(window_ns),
            tw_passed: false,
            ticks: TempoCursor::new(map),
            frames: TempoCursor::new(map),
            held: VecDeque::new(),
            p: 0,
            pa: 0,
            dsum: 0,
            area: 0,
            forced: VecDeque::new(),
        }
    }

    /// The state at tick `lo` — every event `<= lo` fed, every boundary
    /// `<= lo` passed — from the index: where a ranged update starts. Reads
    /// the events of one window, `(kₐ(lo), lo]`.
    fn seed(&mut self, index: &AnalysisIndex, scope: &Scope, timing: &Timing, lo: u64) {
        let ka = window_lo(timing, timing.nanos_at(lo), self.w);
        self.pa = ka.map_or(0, |k| index.poly(scope, k)) as i64;
        self.tw_passed = lo >= self.tw;
        let (starts, ends) = collect(index, scope, ka, lo);
        merge_ticks(&starts, &ends, |x, s, e| {
            self.feed(x, s as i64 - e as i64);
        });
        // Fed from p(kₐ): the poly at lo.
        self.p += self.pa;
        debug_assert_eq!(self.p, index.poly(scope, lo) as i64);
    }

    /// Feeds the events of tick `x` (net `d`) into the window; returns
    /// `fx(x)`.
    #[inline]
    fn feed(&mut self, x: u64, d: i64) -> u128 {
        let fx = self.ticks.fx_at_tick(x);
        if d != 0 {
            let nanos = u64::try_from(fx >> crate::model::tempo::FX_SHIFT).unwrap_or(u64::MAX);
            let exit = self
                .frames
                .first_tick_at_or_after_frame(nanos.saturating_add(self.w));
            let dfx = i128::from(d) * fx as i128;
            self.p += d;
            self.dsum += dfx;
            self.area -= i128::from(d) * i128::from(nanos);
            self.held.push_back(Held { exit, d, dfx });
        }
        fx
    }

    /// The stretch holding tick `c` (in the current state) rises.
    #[inline]
    fn rising(&self, c: u64) -> bool {
        if c < self.tw {
            // PPS = P − dsum / t before the window is full.
            self.dsum > 0
        } else {
            self.p > self.pa
        }
    }

    /// PPS at tick `c` (time `fx`) in the current state.
    fn value(&self, c: u64, fx: u128) -> Ratio {
        let fx = fx as i128;
        if c < self.tw {
            if fx == 0 {
                return Ratio {
                    num: self.p.max(0) as u128,
                    den: 1,
                };
            }
            let a = i128::from(self.p) * fx - self.dsum;
            Ratio {
                num: a.max(0) as u128,
                den: fx as u128,
            }
        } else {
            let a = i128::from(self.pa) * self.wfx + i128::from(self.p - self.pa) * fx - self.dsum;
            Ratio {
                num: a.max(0) as u128,
                den: self.wfx as u128,
            }
        }
    }

    fn eval(&mut self, c: u64, out: &mut impl FnMut(u64, Ratio)) {
        let fx = self.ticks.fx_at_tick(c);
        out(c, self.value(c, fx));
    }

    fn next_boundary(&self) -> Option<u64> {
        let mut b = self.held.front().map(|h| h.exit);
        if !self.tw_passed {
            b = Some(b.map_or(self.tw, |x| x.min(self.tw)));
        }
        if let Some(&f) = self.forced.front() {
            b = Some(b.map_or(f, |x| x.min(f)));
        }
        b
    }

    /// An exit or the full-window switch happens at `b`.
    fn changes_at(&self, b: u64) -> bool {
        self.held.front().is_some_and(|h| h.exit <= b) || (!self.tw_passed && self.tw <= b)
    }

    /// Applies every exit and the switch at or before `b`.
    fn pass(&mut self, b: u64) {
        while let Some(h) = self.held.front().copied() {
            if h.exit > b {
                break;
            }
            self.held.pop_front();
            self.pa += h.d;
            self.dsum -= h.dfx;
        }
        if self.tw <= b {
            self.tw_passed = true;
        }
    }

    fn take_forced(&mut self, b: u64) -> bool {
        let mut hit = false;
        while self.forced.front().is_some_and(|&f| f <= b) {
            self.forced.pop_front();
            hit = true;
        }
        hit
    }

    /// Every boundary before tick `t`, in order.
    fn before(&mut self, t: u64, out: &mut impl FnMut(u64, Ratio)) {
        while let Some(b) = self.next_boundary() {
            if b >= t {
                break;
            }
            self.boundary(b, out);
        }
    }

    /// A boundary without events: exits / the switch at `b`, or a forced
    /// tick. Exits are not continuous (the window start moves in whole
    /// nanoseconds), so both sides are candidates.
    fn boundary(&mut self, b: u64, out: &mut impl FnMut(u64, Ratio)) {
        let change = self.changes_at(b);
        if change && b > 0 && self.rising(b - 1) {
            self.eval(b - 1, out);
        }
        self.pass(b);
        let forced = self.take_forced(b);
        if forced || (change && !self.rising(b)) {
            self.eval(b, out);
        }
    }

    /// Event tick `t` (after [`PpsSweep::before`]), net `d` = starts − ends.
    /// The stretch before an event tick, extended to it, gives exactly
    /// `A(t)` (the new events weigh `t − τ = 0`), so a rising stretch ending
    /// here is evaluated at `t` itself — unless an exit happens here too.
    fn event(&mut self, t: u64, d: i64, out: &mut impl FnMut(u64, Ratio)) {
        let change = self.changes_at(t);
        let rising_before = t > 0 && self.rising(t - 1);
        if change && rising_before {
            self.eval(t - 1, out);
        }
        self.pass(t);
        let fx = self.feed(t, d);
        let forced = self.take_forced(t);
        let at_t = if change {
            !self.rising(t)
        } else if d != 0 {
            rising_before || !self.rising(t)
        } else {
            false // nothing changed: inside a stretch
        };
        if forced || at_t {
            out(t, self.value(t, fx));
        }
    }

    /// A new block begins at event tick `t` (call before [`PpsSweep::event`]
    /// and before the block is added): a rising stretch through `t − 1`
    /// peaks at the last tick of the block before; `t`, the first tick of
    /// the new block's range, is evaluated whatever the slope (a falling
    /// stretch may pass through it, also at a tick where starts and ends
    /// cancel out).
    fn open_block(&mut self, t: u64, has_previous: bool, out: &mut impl FnMut(u64, Ratio)) {
        if has_previous && t > 0 && self.rising(t - 1) {
            self.eval(t - 1, out);
        }
        self.forced.push_front(t);
    }

    /// Every boundary left (after the last event).
    fn finish(&mut self, out: &mut impl FnMut(u64, Ratio)) {
        while let Some(b) = self.next_boundary() {
            self.boundary(b, out);
        }
    }
}

/// Records PPS candidate `(c, v)` in the block whose range holds `c`: the
/// last block, or the one before it when `c` precedes the last block's
/// first tick (a stretch that ends where a block begins). Ticks before the
/// first block have no notes before them: PPS 0, dropped.
fn record_pps(blocks: &mut [Block], c: u64, v: Ratio) {
    let n = blocks.len();
    let j = if n > 0 && blocks[n - 1].first <= c {
        n - 1
    } else if n > 1 && blocks[n - 2].first <= c {
        n - 2
    } else {
        return;
    };
    let b = &mut blocks[j];
    if v.exceeds(&b.max_pps) || (!b.max_pps.exceeds(&v) && c < b.pps_tick) {
        b.max_pps = v;
        b.pps_tick = c;
    }
}

impl PeakProfile {
    /// Sweeps the whole timeline of `scope` (Peak Poly and Peak NPS).
    /// Returns `None` when cancelled.
    pub fn build(
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        window_ns: u64,
        cancel: Option<&AtomicBool>,
    ) -> Option<PeakProfile> {
        PeakProfile::build_with(index, scope, timing, window_ns, Peaks::PolyNps, cancel)
    }

    /// [`PeakProfile::build`] keeping `peaks` (v0.9 V09-S19: with
    /// [`Peaks::WithPps`] also Peak PPS and the Poly Area in note·ns).
    pub fn build_with(
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        window_ns: u64,
        peaks: Peaks,
        cancel: Option<&AtomicBool>,
    ) -> Option<PeakProfile> {
        let window_ns = window_ns.max(1);
        let mut prof = PeakProfile {
            window_ns,
            peaks,
            ..PeakProfile::default()
        };
        // Last tick with an event: the largest end (ends are >= starts).
        let last = index.last_tick(scope);
        if index.notes_in(scope) == 0 {
            return Some(prof);
        }
        let mut pps = (peaks == Peaks::WithPps).then(|| PpsSweep::new(timing, window_ns));
        let mut sweep = PolySweep::default();
        let mut cum: u64 = 0; // starts at ticks <= current
        // Onset ticks of the last window: (tick, cum after it).
        let mut recent: VecDeque<(u64, u64)> = VecDeque::new();
        let mut base: u64 = 0; // cum at the last onset tick that left the window
        let mut in_block = 0usize;
        let mut lo: Option<u64> = None;
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            let from = lo.map_or(0, |l| l + 1);
            if lo.is_some_and(|l| l >= last) {
                break;
            }
            // Largest `hi` with at most SWEEP_STARTS starts in (lo, hi] (at
            // least one tick, so huge stacks on one tick still progress).
            let (mut a, mut b) = (from, last);
            while a < b {
                let mid = a + (b - a).div_ceil(2);
                if index.starts_in(scope, lo, mid).0 <= SWEEP_STARTS {
                    a = mid;
                } else {
                    b = mid - 1;
                }
            }
            let hi = a.max(from);
            let (starts, ends) = collect(index, scope, lo, hi);
            prof.events += (starts.len() + ends.len()) as u64;
            merge_ticks(&starts, &ends, |t, s, e| {
                let poly = sweep.tick(t, s, e);
                let mut nps = 0;
                if s > 0 {
                    cum += s;
                    if let Some(k_lo) = window_lo(timing, timing.nanos_at(t), window_ns) {
                        while let Some(&(tk, c)) = recent.front() {
                            if tk > k_lo {
                                break;
                            }
                            base = c;
                            recent.pop_front();
                        }
                    }
                    recent.push_back((t, cum));
                    nps = cum - base;
                }
                let blocks = &mut prof.blocks;
                if let Some(ps) = pps.as_mut() {
                    ps.before(t, &mut |c, v| record_pps(blocks, c, v));
                }
                if in_block == 0 {
                    if let Some(ps) = pps.as_mut() {
                        let has_previous = !blocks.is_empty();
                        ps.open_block(t, has_previous, &mut |c, v| record_pps(blocks, c, v));
                    }
                    blocks.push(Block::new(t, poly, nps));
                } else {
                    let b = blocks.last_mut().expect("open block");
                    b.max_poly = b.max_poly.max(poly);
                    b.max_nps = b.max_nps.max(nps);
                }
                if let Some(ps) = pps.as_mut() {
                    ps.event(t, s as i64 - e as i64, &mut |c, v| record_pps(blocks, c, v));
                }
                in_block = (in_block + 1) % BLOCK_TICKS;
            });
            lo = Some(hi);
        }
        if let Some(ps) = pps.as_mut() {
            let blocks = &mut prof.blocks;
            ps.finish(&mut |c, v| record_pps(blocks, c, v));
            prof.area_ns = ps.area.max(0) as u128;
        }
        let (mut mp, mut mn) = (0, 0);
        for b in &prof.blocks {
            mp = mp.max(b.max_poly);
            mn = mn.max(b.max_nps);
            prof.run_poly.push(mp);
            prof.run_nps.push(mn);
        }
        prof.blocks.shrink_to_fit();
        Some(prof)
    }

    /// The profile of `index` after `delta` (what `index` gained and lost
    /// since the index this profile was built for; same scope, tempo map and
    /// window): re-sweeps only the blocks the change can reach
    /// ([`dirty_spans`]: changed ticks, where the poly differs, the windows
    /// of changed starts), seeding the poly from the index, and keeps every
    /// other block — the answers equal [`PeakProfile::build`]'s exactly
    /// (v0.9 V09-S18, `docs/INSPECTOR.md` I6). Blocks may be split
    /// differently; queries never depend on that. Falls back to a full build
    /// when the change reaches half of the blocks. `None` when cancelled.
    pub fn update(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        delta: &TickDelta,
        cancel: Option<&AtomicBool>,
    ) -> Option<PeakProfile> {
        let w = self.window_ns;
        if delta.is_empty() {
            return Some(self.clone());
        }
        if self.blocks.is_empty() || index.notes_in(scope) == 0 {
            return PeakProfile::build_with(index, scope, timing, w, self.peaks, cancel);
        }
        let Some(ranges) = self.sweep_ranges(delta, timing) else {
            return PeakProfile::build_with(index, scope, timing, w, self.peaks, cancel);
        };
        let with_pps = self.peaks == Peaks::WithPps;
        let last = index.last_tick(scope);
        let mut blocks = Vec::with_capacity(self.blocks.len() + ranges.len() * 2);
        let mut next = 0;
        for (j0, j1) in ranges {
            blocks.extend_from_slice(&self.blocks[next..j0]);
            next = j1 + 1;
            // The ticks of old blocks j0..=j1: from block j0's first tick (the
            // song start for block 0) to the tick before block j1 + 1.
            let lo = (j0 > 0).then(|| self.blocks[j0].first - 1);
            let tail = self.blocks.get(j1 + 1).is_none();
            let hi = match self.blocks.get(j1 + 1) {
                Some(b) => b.first - 1,
                None => last,
            };
            let mut sweep = PolySweep::starting_at(lo.map_or(0, |l| index.poly(scope, l)));
            // PPS: the window state at lo, and the range's first and last
            // ticks evaluated whatever the slope (a stretch may cross them).
            let mut pps = with_pps.then(|| {
                let mut ps = PpsSweep::new(timing, w);
                if let Some(l) = lo {
                    ps.seed(index, scope, timing, l);
                    ps.forced.push_back(l + 1);
                }
                if !tail {
                    ps.forced.push_back(hi.max(lo.map_or(0, |l| l + 1)));
                }
                ps
            });
            let mut in_block = 0usize;
            let mut cur = lo;
            loop {
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return None;
                }
                if cur.is_some_and(|c| c >= hi) {
                    break;
                }
                let from = cur.map_or(0, |c| c + 1);
                let (mut a, mut b) = (from, hi.max(from));
                while a < b {
                    let mid = a + (b - a).div_ceil(2);
                    if index.starts_in(scope, cur, mid).0 <= SWEEP_STARTS {
                        a = mid;
                    } else {
                        b = mid - 1;
                    }
                }
                let h = a.max(from);
                let (starts, ends) = collect(index, scope, cur, h);
                merge_ticks(&starts, &ends, |t, s, e| {
                    let poly = sweep.tick(t, s, e);
                    let nps = if s > 0 {
                        nps_at(index, scope, timing, w, t)
                    } else {
                        0
                    };
                    let blocks = &mut blocks;
                    if let Some(ps) = pps.as_mut() {
                        ps.before(t, &mut |c, v| record_pps(blocks, c, v));
                    }
                    if in_block == 0 {
                        if let Some(ps) = pps.as_mut() {
                            let has_previous = !blocks.is_empty();
                            ps.open_block(t, has_previous, &mut |c, v| record_pps(blocks, c, v));
                        }
                        blocks.push(Block::new(t, poly, nps));
                    } else {
                        let b = blocks.last_mut().expect("open block");
                        b.max_poly = b.max_poly.max(poly);
                        b.max_nps = b.max_nps.max(nps);
                    }
                    if let Some(ps) = pps.as_mut() {
                        ps.event(t, s as i64 - e as i64, &mut |c, v| record_pps(blocks, c, v));
                    }
                    in_block = (in_block + 1) % BLOCK_TICKS;
                });
                cur = Some(h);
            }
            if let Some(ps) = pps.as_mut() {
                let blocks = &mut blocks;
                if tail {
                    ps.finish(&mut |c, v| record_pps(blocks, c, v));
                } else {
                    ps.before(hi.saturating_add(1), &mut |c, v| record_pps(blocks, c, v));
                }
            }
        }
        blocks.extend_from_slice(&self.blocks[next..]);
        // Poly Area is a sum over events: the delta moves it.
        let area_ns = if with_pps {
            let n = |v: &[u64]| -> i128 { v.iter().map(|&x| i128::from(timing.nanos_at(x))).sum() };
            (self.area_ns as i128 + n(&delta.added_ends)
                - n(&delta.removed_ends)
                - n(&delta.added_starts)
                + n(&delta.removed_starts))
            .max(0) as u128
        } else {
            0
        };
        let mut prof = PeakProfile {
            blocks,
            run_poly: Vec::new(),
            run_nps: Vec::new(),
            window_ns: w,
            peaks: self.peaks,
            area_ns,
            events: 2 * index.notes_in(scope),
        };
        let (mut mp, mut mn) = (0, 0);
        prof.run_poly.reserve_exact(prof.blocks.len());
        prof.run_nps.reserve_exact(prof.blocks.len());
        for b in &prof.blocks {
            mp = mp.max(b.max_poly);
            mn = mn.max(b.max_nps);
            prof.run_poly.push(mp);
            prof.run_nps.push(mn);
        }
        Some(prof)
    }

    /// The old block ranges (inclusive) a ranged update re-sweeps for
    /// `delta`; `None` when it would rebuild (no blocks yet, or the change
    /// reaches half of them).
    fn sweep_ranges(&self, delta: &TickDelta, timing: &Timing) -> Option<Vec<(usize, usize)>> {
        if self.blocks.is_empty() {
            return None;
        }
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (a, b) in dirty_spans(delta, timing, self.window_ns, self.peaks == Peaks::WithPps) {
            let j0 = self.block_of(a).unwrap_or(0);
            let j1 = self.block_of(b).unwrap_or(0);
            match ranges.last_mut() {
                Some(r) if j0 <= r.1 => r.1 = r.1.max(j1),
                _ => ranges.push((j0, j1)),
            }
        }
        let swept: usize = ranges.iter().map(|r| r.1 - r.0 + 1).sum();
        (swept * 2 <= self.blocks.len()).then_some(ranges)
    }

    /// How many blocks [`PeakProfile::update`] would re-sweep for `delta`
    /// (`usize::MAX`: it would rebuild) — lets a caller run small updates at
    /// once and send large ones to a worker. O(delta · log).
    pub fn update_blocks(&self, delta: &TickDelta, timing: &Timing) -> usize {
        if delta.is_empty() {
            return 0;
        }
        self.sweep_ranges(delta, timing)
            .map_or(usize::MAX, |r| r.iter().map(|r| r.1 - r.0 + 1).sum())
    }

    /// The tick spans (merged, inclusive) where `delta` can change NPS (this
    /// profile's window) or poly; outside them every value is unchanged.
    pub fn dirty_ranges(&self, delta: &TickDelta, timing: &Timing) -> Vec<(u64, u64)> {
        dirty_spans(delta, timing, self.window_ns, self.peaks == Peaks::WithPps)
    }

    /// The peaks this profile keeps.
    pub fn peaks(&self) -> Peaks {
        self.peaks
    }

    /// Highest poly at any tick of the scope.
    pub fn peak_poly(&self) -> u64 {
        self.run_poly.last().copied().unwrap_or(0)
    }

    /// Highest NPS (window as built) at any tick of the scope.
    pub fn peak_nps(&self) -> u64 {
        self.run_nps.last().copied().unwrap_or(0)
    }

    /// Highest PPS at **any** tick and the first tick reaching it, exactly
    /// ([`Peaks::WithPps`]; `None` otherwise or without notes). V09-S19.
    pub fn peak_pps(&self) -> Option<(Ratio, u64)> {
        if self.peaks != Peaks::WithPps {
            return None;
        }
        let mut best: Option<(Ratio, u64)> = None;
        for b in &self.blocks {
            if best.is_none_or(|(r, _)| b.max_pps.exceeds(&r)) {
                best = Some((b.max_pps, b.pps_tick));
            }
        }
        // PPS is never negative: a peak of 0 is first reached at tick 0.
        best.map(|(r, t)| if r.num == 0 { (Ratio::ZERO, 0) } else { (r, t) })
    }

    /// Every block's first tick, highest PPS over its range and the first
    /// tick reaching it (tests check each block against a brute force).
    #[doc(hidden)]
    pub fn pps_blocks(&self) -> Vec<(u64, Ratio, u64)> {
        self.blocks
            .iter()
            .map(|b| (b.first, b.max_pps, b.pps_tick))
            .collect()
    }

    /// Poly Area in note·ns, `Σ nanos(end) − nanos(start)` over the notes
    /// ([`Peaks::WithPps`]).
    pub fn area_ns(&self) -> Option<u128> {
        (self.peaks == Peaks::WithPps).then_some(self.area_ns)
    }

    /// The first block whose running maximum reaches `peak`, and its ticks
    /// `(lo, hi]` for an exact sweep.
    fn block_range_of(&self, run: &[u64], peak: u64, last: u64) -> (Option<u64>, u64) {
        let j = run.partition_point(|&m| m < peak);
        let lo = (j > 0).then(|| self.blocks[j].first - 1);
        let hi = self.blocks.get(j + 1).map_or(last, |b| b.first - 1);
        (lo, hi)
    }

    /// Peak Poly and the first tick reaching it (0, 0 without notes).
    pub fn peak_poly_first(&self, index: &AnalysisIndex, scope: &Scope) -> (u64, u64) {
        let peak = self.peak_poly();
        if peak == 0 {
            return (0, 0);
        }
        let (lo, hi) = self.block_range_of(&self.run_poly, peak, index.last_tick(scope));
        let mut sweep = PolySweep::starting_at(lo.map_or(0, |l| index.poly(scope, l)));
        let (starts, ends) = collect(index, scope, lo, hi);
        let mut first = None;
        merge_ticks(&starts, &ends, |t, s, e| {
            if sweep.tick(t, s, e) >= peak && first.is_none() {
                first = Some(t);
            }
        });
        (peak, first.unwrap_or(0))
    }

    /// Peak NPS and the first note start reaching it (0, 0 without notes).
    pub fn peak_nps_first(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
    ) -> (u64, u64) {
        let peak = self.peak_nps();
        if peak == 0 {
            return (0, 0);
        }
        let (lo, hi) = self.block_range_of(&self.run_nps, peak, index.last_tick(scope));
        let mut onsets = starts_sorted(index, scope, lo, hi);
        onsets.dedup();
        let first = onsets
            .into_iter()
            .find(|&x| nps_at(index, scope, timing, self.window_ns, x) >= peak);
        (peak, first.unwrap_or(0))
    }

    /// [`PeakProfile::chart_columns`] after an edit, from the columns of the
    /// same `bounds` before it: only the columns a dirty range
    /// ([`PeakProfile::dirty_ranges`]) touches are computed again — the
    /// others cannot have changed. `prev` must hold the columns of `bounds`.
    pub fn chart_columns_update(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        bounds: &[u64],
        prev: &[(u64, u64)],
        dirty: &[(u64, u64)],
    ) -> Vec<(u64, u64)> {
        let n = bounds.len().saturating_sub(1);
        debug_assert_eq!(prev.len(), n);
        (0..n)
            .map(|k| {
                let lo = bounds[k];
                let hi = if k + 1 == n {
                    bounds[k + 1].max(lo)
                } else {
                    bounds[k + 1].saturating_sub(1).max(lo)
                };
                let i = dirty.partition_point(|r| r.1 < lo);
                let touched = dirty.get(i).is_some_and(|r| r.0 <= hi);
                match prev.get(k) {
                    Some(&c) if !touched => c,
                    _ => (
                        self.peak_nps_between(index, scope, timing, Some(lo), hi),
                        self.peak_poly_between(index, scope, Some(lo), hi),
                    ),
                }
            })
            .collect()
    }

    pub fn window_ns(&self) -> u64 {
        self.window_ns
    }

    pub fn heap_bytes(&self) -> usize {
        self.blocks.capacity() * std::mem::size_of::<Block>()
            + (self.run_poly.capacity() + self.run_nps.capacity()) * 8
    }

    /// Block containing `tick` (last block whose first event tick is <= it).
    fn block_of(&self, tick: u64) -> Option<usize> {
        self.blocks
            .partition_point(|b| b.first <= tick)
            .checked_sub(1)
    }

    /// Highest poly at any tick <= `tick`.
    pub fn peak_poly_so_far(&self, index: &AnalysisIndex, scope: &Scope, tick: u64) -> u64 {
        let Some(j) = self.block_of(tick) else {
            return 0;
        };
        let before = if j > 0 { self.run_poly[j - 1] } else { 0 };
        let first = self.blocks[j].first;
        let local = window_peak_poly(index, scope, first.checked_sub(1), tick);
        before.max(local)
    }

    /// Highest NPS (window as built) at any note start <= `tick`.
    pub fn peak_nps_so_far(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        tick: u64,
    ) -> u64 {
        let Some(j) = self.block_of(tick) else {
            return 0;
        };
        let mut best = if j > 0 { self.run_nps[j - 1] } else { 0 };
        let first = self.blocks[j].first;
        let mut onsets = starts_sorted(index, scope, first.checked_sub(1), tick);
        onsets.dedup();
        for x in onsets {
            let lo = window_lo(timing, timing.nanos_at(x), self.window_ns);
            let n = index.starts_in(scope, lo, x).0;
            best = best.max(n);
        }
        best
    }

    /// Highest poly in `(lo, hi]` (plus the poly at `lo`), using whole blocks
    /// in between.
    pub fn peak_poly_between(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        lo: Option<u64>,
        hi: u64,
    ) -> u64 {
        let start = lo.map_or(0, |l| l + 1);
        // Blocks entirely inside (lo, hi]: those after the block containing
        // `start` that end before the block containing `hi`.
        let (Some(jb), ja) = (self.block_of(hi), self.block_of(start)) else {
            return window_peak_poly(index, scope, lo, hi);
        };
        let first_full = match ja {
            Some(j) if self.blocks[j].first < start => j + 1,
            Some(j) => j,
            None => 0,
        };
        if first_full >= jb {
            return window_peak_poly(index, scope, lo, hi);
        }
        let mut best = match self.blocks[first_full].first.checked_sub(1) {
            Some(end) if lo.is_none_or(|l| end > l) => window_peak_poly(index, scope, lo, end),
            _ => lo.map_or(0, |l| index.poly(scope, l)),
        };
        for b in &self.blocks[first_full..jb] {
            best = best.max(b.max_poly);
        }
        best.max(window_peak_poly(
            index,
            scope,
            Some(self.blocks[jb].first - 1),
            hi,
        ))
    }

    /// Highest NPS (window as built) in `(lo, hi]` (plus the NPS at `lo`),
    /// using whole blocks where possible — the NPS counterpart of
    /// [`PeakProfile::peak_poly_between`] (v0.9 V09-S18, `docs/INSPECTOR.md`
    /// I5). Exact.
    pub fn peak_nps_between(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        lo: Option<u64>,
        hi: u64,
    ) -> u64 {
        let w = self.window_ns;
        let start = lo.map_or(0, |l| l + 1);
        if lo.is_some_and(|l| l >= hi) {
            return window_peak_nps(index, scope, timing, w, lo, hi);
        }
        let (Some(jb), ja) = (self.block_of(hi), self.block_of(start)) else {
            return window_peak_nps(index, scope, timing, w, lo, hi);
        };
        let first_full = match ja {
            Some(j) if self.blocks[j].first < start => j + 1,
            Some(j) => j,
            None => 0,
        };
        if first_full >= jb {
            return window_peak_nps(index, scope, timing, w, lo, hi);
        }
        let mut best = match self.blocks[first_full].first.checked_sub(1) {
            Some(end) if lo.is_none_or(|l| end > l) => {
                window_peak_nps(index, scope, timing, w, lo, end)
            }
            _ => lo.map_or(0, |l| nps_at(index, scope, timing, w, l)),
        };
        for b in &self.blocks[first_full..jb] {
            best = best.max(b.max_nps);
        }
        best.max(window_peak_nps(
            index,
            scope,
            timing,
            w,
            Some(self.blocks[jb].first - 1),
            hi,
        ))
    }

    /// The Inspector's chart columns (v0.9 V09-S18, `docs/INSPECTOR.md` I5):
    /// for consecutive ticks `bounds` (non-decreasing, at least two), column
    /// `k` covers the ticks `[bounds[k], bounds[k + 1])` — the last one also
    /// `bounds[last]` itself — and holds `(highest NPS, highest poly)` over
    /// them, exactly: the value holding at the column's first tick and at
    /// every event inside. So no peak hides inside a column, and when the
    /// columns cover the song the highest column is the song's peak.
    pub fn chart_columns(
        &self,
        index: &AnalysisIndex,
        scope: &Scope,
        timing: &Timing,
        bounds: &[u64],
    ) -> Vec<(u64, u64)> {
        let n = bounds.len().saturating_sub(1);
        (0..n)
            .map(|k| {
                let lo = bounds[k];
                let hi = if k + 1 == n {
                    bounds[k + 1].max(lo)
                } else {
                    bounds[k + 1].saturating_sub(1).max(lo)
                };
                (
                    self.peak_nps_between(index, scope, timing, Some(lo), hi),
                    self.peak_poly_between(index, scope, Some(lo), hi),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bpm, PlumDocument};

    fn rng(seed: u64) -> impl FnMut(u64) -> u64 {
        let mut x = seed | 1;
        move |n| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n.max(1)
        }
    }

    /// (NPS, poly) of the notes at `t`, by definition.
    fn at(timing: &Timing, notes: &[(u64, u64)], t: u64) -> (u64, u64) {
        let lo = window_lo(timing, timing.nanos_at(t), 1_000_000_000);
        let nps = notes
            .iter()
            .filter(|n| n.0 <= t && lo.is_none_or(|l| n.0 > l))
            .count() as u64;
        let poly = notes.iter().filter(|n| n.0 <= t && t < n.1).count() as u64;
        (nps, poly)
    }

    #[test]
    fn dirty_spans_cover_every_tick_where_nps_or_poly_changes_and_end_where_a_start_leaves_the_window()
     {
        for seed in 1..40u64 {
            let mut r = rng(seed);
            let mut doc = PlumDocument::new_project("t".into(), Bpm::parse("120").unwrap(), 96);
            crate::fixtures::set_dense_tempo(&mut doc, 12, 96, seed);
            let timing = Timing::for_document(&doc);
            let old: Vec<(u64, u64)> = (0..30)
                .map(|_| {
                    let s = r(3_000);
                    (s, s + r(400))
                })
                .collect();
            let mut new = old.clone();
            let mut delta = TickDelta::default();
            for _ in 0..r(3) {
                let i = r(new.len() as u64) as usize;
                let (s, e) = new.remove(i);
                delta.removed_starts.push(s);
                delta.removed_ends.push(e);
            }
            for _ in 0..1 + r(3) {
                let s = r(3_000);
                let e = s + r(900);
                new.push((s, e));
                delta.added_starts.push(s);
                delta.added_ends.push(e);
            }
            for v in [
                &mut delta.removed_starts,
                &mut delta.added_starts,
                &mut delta.removed_ends,
                &mut delta.added_ends,
            ] {
                v.sort_unstable();
            }
            let spans = dirty_spans(&delta, &timing, 1_000_000_000, false);
            let inside = |t: u64| spans.iter().any(|&(a, b)| a <= t && t <= b);
            for t in 0..6_000u64 {
                if at(&timing, &old, t) != at(&timing, &new, t) {
                    assert!(inside(t), "seed {seed}: tick {t} changed outside {spans:?}");
                }
            }
            // Tight at a window's end: an added start alone still counts at
            // its span's end (or the tick before, when the second ends exactly
            // on a tick) and never one tick later.
            let s = delta.added_starts[0];
            let one = [(s, s)];
            let end = timing.tick_at_nanos(timing.nanos_at(s) + 1_000_000_000);
            assert!(
                at(&timing, &one, end).0 == 1 || at(&timing, &one, end - 1).0 == 1,
                "seed {seed}"
            );
            assert_eq!(at(&timing, &one, end + 1).0, 0, "seed {seed}");
        }
    }
}
