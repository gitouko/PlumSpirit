//! `tempo_time`: real time ↔ ticks through the song's tempo map, ramps
//! included — milliseconds, Hz rates and frame grids for the tool functions
//! that must sound the same at any tempo (real-time gaps, rates, strums,
//! frame grids, NPS windows).
//!
//! The map is [`Ctx::tempo`] at [`TEMPO_RATE`] (1 MHz: one frame = one
//! microsecond); without one, 120 BPM. Ticks are the tool's ticks: an opened
//! clip's content ticks are shifted by [`Ctx::shift`] to song time first.
//! Lookups go through a [`TempoCursor`], O(1) amortised while the ticks
//! asked do not decrease (any order is answered exactly).

use super::super::{Ctx, TEMPO_RATE};
use crate::model::tempo::{TempoCursor, TempoMap};

/// Microseconds per quarter note without a tempo map (120 BPM).
const DEFAULT_QUARTER_US: u128 = 500_000;
/// One microsecond in the map's fixed point (frames × 2³²).
const FX_ONE: f64 = 4_294_967_296.0;
/// Rounding allowance of a real-valued grid time (1 ns).
const SNAP_SLACK_US: f64 = 0.001;
/// Rates accepted by [`TempoTime::rate_steps`] (Hz).
pub const MIN_HZ: f64 = 0.001;
pub const MAX_HZ: f64 = 10_000.0;

/// Real-time lookups for one tool run.
pub struct TempoTime<'a> {
    ppq: u64,
    shift: i128,
    map: Option<&'a TempoMap>,
    cursor: Option<TempoCursor<'a>>,
}

impl<'a> TempoTime<'a> {
    pub fn new(ctx: &'a Ctx) -> TempoTime<'a> {
        debug_assert!(
            ctx.tempo
                .as_ref()
                .is_none_or(|t| t.sample_rate() == TEMPO_RATE)
        );
        let map = ctx.tempo.as_deref();
        TempoTime {
            ppq: ctx.ppq.max(1),
            shift: ctx.shift,
            map,
            cursor: map.map(TempoCursor::new),
        }
    }

    fn song(&self, tick: u64) -> u64 {
        (i128::from(tick) + self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }

    fn local(&self, song: u64) -> u64 {
        (i128::from(song) - self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }

    /// The song time of `tick` in microseconds (floor).
    pub fn micros_at(&mut self, tick: u64) -> u64 {
        let song = self.song(tick);
        match &mut self.cursor {
            Some(c) => u64::try_from(c.fx_at_tick(song) >> 32).unwrap_or(u64::MAX),
            None => u64::try_from(u128::from(song) * DEFAULT_QUARTER_US / u128::from(self.ppq))
                .unwrap_or(u64::MAX),
        }
    }

    /// The first tick that sounds at or after `us` microseconds of song time
    /// (ticks before the tool's tick 0 clamp to 0).
    pub fn tick_at_micros(&mut self, us: u64) -> u64 {
        let song = match &mut self.cursor {
            Some(c) => c.first_tick_at_or_after_frame(us),
            None => {
                u64::try_from((u128::from(us) * u128::from(self.ppq)).div_ceil(DEFAULT_QUARTER_US))
                    .unwrap_or(u64::MAX)
            }
        };
        self.local(song)
    }

    /// Ticks from `tick` until `us` microseconds have passed (a real-time
    /// length starting at `tick`).
    pub fn ticks_after(&mut self, tick: u64, us: u64) -> u64 {
        let at = self.micros_at(tick).saturating_add(us);
        self.tick_at_micros(at).saturating_sub(tick)
    }

    /// The tick `us` microseconds before `tick` (the first tick sounding at
    /// or after that time; 0 at the start).
    pub fn tick_before(&mut self, tick: u64, us: u64) -> u64 {
        let at = self.micros_at(tick).saturating_sub(us);
        self.tick_at_micros(at).min(tick)
    }

    /// The slowest tempo of the song in BPM (120 without a map): the most
    /// real time a tick can take, for preflight bounds.
    pub fn min_bpm(&self) -> f64 {
        match self.map {
            Some(m) => m.min_bpm(self.ppq),
            None => 120.0,
        }
    }

    /// The fastest tempo of the song in BPM (120 without a map): the most
    /// ticks a stretch of real time can hold (reach of real-time grids).
    pub fn max_bpm(&self) -> f64 {
        match self.map {
            Some(m) => m.max_bpm(self.ppq),
            None => 120.0,
        }
    }

    /// The most ticks `us` microseconds can span anywhere in the song (at
    /// least 1): the reach of a real-time grid of that period.
    pub fn max_ticks_in(&self, us: f64) -> u64 {
        let t = (us / 1e6 * self.max_bpm() * self.ppq as f64 / 60.0).ceil() + 1.0;
        if t.is_finite() && t >= 1.0 {
            (t as u64).max(1)
        } else {
            u64::MAX
        }
    }

    /// The fewest ticks between two steps of a rate up to `max_hz` anywhere
    /// in the song (at least 1): every [`TempoTime::rate_steps`] step is at
    /// least this far from the previous one.
    pub fn min_ticks_per_step(&self, max_hz: f64) -> u64 {
        let ticks_per_second = self.min_bpm() * self.ppq as f64 / 60.0;
        let t = (ticks_per_second / max_hz.clamp(MIN_HZ, MAX_HZ)).floor() - 1.0;
        if t.is_finite() && t >= 1.0 {
            t as u64
        } else {
            1
        }
    }

    /// The tempo at `tick` in BPM (a ramp's BPM at that tick).
    pub fn bpm_at(&self, tick: u64) -> f64 {
        match self.map {
            Some(m) => m.bpm_at_tick(self.song(tick), self.ppq),
            None => 120.0,
        }
    }

    /// Steps from `lo` (the first) up to but not including `hi`, spaced in
    /// real time at `rate(pos)` Hz, `pos` = the step's place in `[lo, hi)`
    /// measured in real time (0 … 1), clamped to [`MIN_HZ`] … [`MAX_HZ`]. A
    /// step that would land on the previous one's tick is skipped (at most
    /// one step per tick). At most [`TempoTime::rate_bound`] steps.
    pub fn rate_steps<'t, F: FnMut(f64) -> f64>(
        &'t mut self,
        lo: u64,
        hi: u64,
        rate: F,
    ) -> RateSteps<'t, 'a, F> {
        let lo_us = self.micros_at(lo) as f64;
        let hi_us = self.micros_at(hi) as f64;
        RateSteps {
            tt: self,
            at_us: lo_us,
            lo_us,
            span_us: (hi_us - lo_us).max(1.0),
            lo,
            hi,
            rate,
            last: None,
        }
    }

    /// Most steps [`TempoTime::rate_steps`] yields over `[lo, hi)` when the
    /// rate never exceeds `max_hz` (preflight bound).
    pub fn rate_bound(&mut self, lo: u64, hi: u64, max_hz: f64) -> u64 {
        if hi <= lo {
            return 0;
        }
        let secs = (self.micros_at(hi) - self.micros_at(lo)) as f64 / 1e6;
        (secs * max_hz.clamp(MIN_HZ, MAX_HZ)).ceil() as u64 + 1
    }

    /// The song time of `tick` in microseconds with the map's sub-µs
    /// precision (2⁻³² µs): strictly increasing in `tick` (Humanize n01's
    /// monotone real-time warp, V09-S36).
    pub fn micros_f(&mut self, tick: u64) -> f64 {
        let song = self.song(tick);
        match &mut self.cursor {
            Some(c) => c.fx_at_tick(song) as f64 / FX_ONE,
            None => song as f64 * DEFAULT_QUARTER_US as f64 / self.ppq as f64,
        }
    }

    /// The first tick that sounds at or after `us` microseconds (a real
    /// number), allowing [`SNAP_SLACK_US`] for the rounding of `us` itself —
    /// a time that falls exactly on a tick gives that tick.
    pub fn tick_at_micros_f(&mut self, us: f64) -> u64 {
        let us = (us - SNAP_SLACK_US).max(0.0);
        let song = match &mut self.cursor {
            Some(c) => {
                let fx = (us * FX_ONE).ceil();
                c.first_tick_at_or_after_fx(if fx >= u128::MAX as f64 {
                    u128::MAX
                } else {
                    fx as u128
                })
            }
            None => {
                let t = (us * self.ppq as f64 / DEFAULT_QUARTER_US as f64).ceil();
                if t >= u64::MAX as f64 {
                    u64::MAX
                } else {
                    t as u64
                }
            }
        };
        self.local(song)
    }

    /// The tick of the real-time grid point nearest to `tick` — grid points
    /// every `period_us` microseconds from `phase_us` (frames of a video at
    /// 24 / 25 / 30 … fps, or any milliseconds) — as the first tick that
    /// sounds at or after that point (a point falling exactly on a tick,
    /// like 60 fps at 120 BPM / 960 PPQN, gives that tick).
    pub fn snap_to_grid(&mut self, tick: u64, period_us: f64, phase_us: f64) -> u64 {
        if !(period_us.is_finite() && period_us > 0.0 && phase_us.is_finite()) {
            return tick;
        }
        let us = self.micros_f(tick);
        let k = ((us - phase_us) / period_us).round();
        let target = (phase_us + k * period_us).max(0.0);
        self.tick_at_micros_f(target)
    }
}

/// The iterator of [`TempoTime::rate_steps`].
pub struct RateSteps<'t, 'a, F> {
    tt: &'t mut TempoTime<'a>,
    at_us: f64,
    lo_us: f64,
    span_us: f64,
    lo: u64,
    hi: u64,
    rate: F,
    last: Option<u64>,
}

impl<F: FnMut(f64) -> f64> Iterator for RateSteps<'_, '_, F> {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        loop {
            let tick = match self.last {
                None => self.lo,
                // Past the representable song time no step lands on a new
                // tick: the steps end (far-end ticks would spin forever).
                Some(_) if self.at_us.is_nan() || self.at_us >= u64::MAX as f64 => return None,
                Some(_) => self
                    .tt
                    .tick_at_micros(self.at_us.round() as u64)
                    .max(self.lo),
            };
            if tick >= self.hi {
                return None;
            }
            let pos = ((self.at_us - self.lo_us) / self.span_us).clamp(0.0, 1.0);
            let r = (self.rate)(pos);
            let r = if r.is_finite() {
                r.clamp(MIN_HZ, MAX_HZ)
            } else {
                MIN_HZ
            };
            let before = self.at_us;
            self.at_us += 1e6 / r;
            if self.at_us <= before {
                // A step too small to move a time this far out: stop next.
                self.at_us = f64::INFINITY;
            }
            if self.last == Some(tick) {
                continue;
            }
            self.last = Some(tick);
            return Some(tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::conductor::{Interp, TempoPoint};
    use crate::model::tempo::TempoValue;
    use std::sync::Arc;

    fn ctx_with(map: Option<TempoMap>) -> Ctx {
        let mut c = Ctx::simple(960);
        c.tempo = map.map(Arc::new);
        c
    }

    /// 120 BPM until tick 1920, then 60 BPM.
    fn stepped() -> TempoMap {
        TempoMap::from_tempos(
            TEMPO_RATE,
            960,
            &[
                (0, TempoValue::Micros(500_000)),
                (1920, TempoValue::Micros(1_000_000)),
            ],
        )
    }

    /// 60 → 240 BPM linear over 4 bars, then 240.
    fn ramped() -> TempoMap {
        let p = |tick, bpm: u32, interp| TempoPoint {
            id: tick as u32,
            tick,
            value: TempoValue::Micros(60_000_000 / bpm),
            interp,
            implicit: false,
            origin: None,
        };
        TempoMap::from_points(
            TEMPO_RATE,
            960,
            &[p(0, 60, Interp::Linear), p(15_360, 240, Interp::Step)],
        )
    }

    #[test]
    fn without_a_tempo_map_the_song_runs_at_120_bpm() {
        let ctx = ctx_with(None);
        let mut t = TempoTime::new(&ctx);
        assert_eq!(t.micros_at(960), 500_000);
        assert_eq!(t.tick_at_micros(500_000), 960);
        assert_eq!(t.tick_at_micros(500_001), 961);
        assert_eq!(t.ticks_after(0, 250_000), 480);
        assert_eq!(t.tick_before(960, 250_000), 480);
        assert_eq!(t.tick_before(100, 10_000_000), 0);
        assert_eq!(t.bpm_at(123), 120.0);
    }

    #[test]
    fn steps_follow_tempo_changes_exactly() {
        let ctx = ctx_with(Some(stepped()));
        let mut t = TempoTime::new(&ctx);
        assert_eq!(t.micros_at(1920), 1_000_000);
        assert_eq!(t.micros_at(2880), 2_000_000);
        assert_eq!(t.ticks_after(1920, 1_000_000), 960);
        // Across the change: 0.5 s before tick 2880 (2.0 s) is tick 2400.
        assert_eq!(t.tick_before(2880, 500_000), 2400);
        assert_eq!(t.tick_before(2880, 1_500_000), 960);
        assert!((t.bpm_at(100) - 120.0).abs() < 1e-9);
        assert!((t.bpm_at(2000) - 60.0).abs() < 1e-9);
    }

    /// Against the map itself at every 97th tick of a ramp, in decreasing
    /// and increasing order (the cursor answers both), and the inverse is
    /// the smallest tick reaching the time.
    #[test]
    fn ramps_match_the_tempo_map_and_invert_exactly() {
        let map = ramped();
        let ctx = ctx_with(Some(map.clone()));
        let mut t = TempoTime::new(&ctx);
        let ticks: Vec<u64> = (0..400).map(|i| i * 97).collect();
        for &k in ticks.iter().chain(ticks.iter().rev()) {
            let us = t.micros_at(k);
            assert_eq!(us, map.frame_at_tick(k), "tick {k}");
            let back = t.tick_at_micros(us);
            assert!(back <= k && t.micros_at(back) >= us, "tick {k}");
            assert!(back == 0 || t.micros_at(back - 1) < us, "tick {k}");
        }
        assert!((t.bpm_at(0) - 60.0).abs() < 1e-6);
        assert!((t.bpm_at(7680) - 150.0).abs() < 1e-6);
        assert!((t.bpm_at(20_000) - 240.0).abs() < 1e-6);
    }

    #[test]
    fn a_clip_shift_reads_the_song_tempo_at_its_position() {
        let mut ctx = ctx_with(Some(stepped()));
        ctx.shift = 1920;
        let mut t = TempoTime::new(&ctx);
        assert_eq!(t.micros_at(0), 1_000_000);
        assert_eq!(t.ticks_after(0, 1_000_000), 960);
        assert!((t.bpm_at(0) - 60.0).abs() < 1e-9);
        // Times before the clip's tick 0 clamp to 0.
        assert_eq!(t.tick_at_micros(0), 0);
    }

    #[test]
    fn rate_steps_keep_real_time_through_tempo_changes() {
        // 4 Hz over 2 s: 0.25 s = 480 ticks at 120 BPM (until tick 1920 =
        // 1 s), 240 ticks at 60 BPM after it.
        let ctx = ctx_with(Some(stepped()));
        let mut t = TempoTime::new(&ctx);
        let hi = 2880; // 2.0 s
        let steps: Vec<u64> = t.rate_steps(0, hi, |_| 4.0).collect();
        assert_eq!(steps, [0, 480, 960, 1440, 1920, 2160, 2400, 2640]);
        assert!(steps.len() as u64 <= t.rate_bound(0, hi, 4.0));
        // A glide 1 → 8 Hz: strictly increasing ticks, shrinking real gaps.
        let g: Vec<u64> = t.rate_steps(0, 1920, |p| 1.0 + 7.0 * p).collect();
        assert!(g.windows(2).all(|w| w[0] < w[1]));
        let us: Vec<u64> = g.iter().map(|&k| t.micros_at(k)).collect();
        let gaps: Vec<u64> = us.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|w| w[1] <= w[0] + 1), "{gaps:?}");
        assert!(g.len() as u64 <= t.rate_bound(0, 1920, 8.0));
        // Faster than the tick grid: at most one step per tick.
        let dense: Vec<u64> = t.rate_steps(0, 10, |_| MAX_HZ).collect();
        assert_eq!(dense, (0..10).collect::<Vec<_>>());
        assert_eq!(t.rate_steps(5, 5, |_| 1.0).count(), 0);
        // At the far end of the tick range the song time saturates: the
        // steps end instead of spinning on one tick.
        let far: Vec<u64> = t
            .rate_steps(u64::MAX - 1_000_000, u64::MAX, |_| MAX_HZ)
            .take(10_000)
            .collect();
        assert!(far.len() < 10_000 && far.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn frame_grids_snap_to_the_first_tick_of_the_nearest_frame() {
        let ctx = ctx_with(None);
        let mut t = TempoTime::new(&ctx);
        // 25 fps = 40 ms frames; a tick is 520.83 µs.
        assert_eq!(t.snap_to_grid(100, 40_000.0, 0.0), 77); // 52.1 ms → 40 ms
        assert_eq!(t.snap_to_grid(140, 40_000.0, 0.0), 154); // 72.9 ms → 80 ms
        assert_eq!(t.snap_to_grid(0, 40_000.0, 0.0), 0);
        // A phase moves the grid; a bad period leaves the tick alone.
        assert_eq!(t.snap_to_grid(100, 40_000.0, 10_000.0), 96); // 50 ms
        assert_eq!(t.snap_to_grid(100, 0.0, 0.0), 100);
        assert_eq!(t.snap_to_grid(100, f64::NAN, 0.0), 100);
        // 60 fps at 120 BPM / 960 PPQN is exactly 32 ticks: frames that fall
        // on a tick give that tick (with and without a map).
        for k in 0..200u64 {
            assert_eq!(t.snap_to_grid(k * 32 + 9, 1e6 / 60.0, 0.0), k * 32);
        }
        let map = ctx_with(Some(TempoMap::from_tempos(
            TEMPO_RATE,
            960,
            &[(0, TempoValue::Micros(500_000))],
        )));
        let mut m = TempoTime::new(&map);
        for k in 0..200u64 {
            assert_eq!(m.snap_to_grid(k * 32 + 9, 1e6 / 60.0, 0.0), k * 32);
            assert_eq!(m.snap_to_grid(k * 80 + 30, 1e6 / 24.0, 0.0), k * 80);
        }
    }
}
