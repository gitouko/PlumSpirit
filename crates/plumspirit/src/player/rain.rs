//! The rainfall's time axis (v0.9 V09-S17, `docs/PLAYER.md` P6 / P9): the
//! window from the playhead to `fall` seconds later, measured in song time,
//! so a note falls at the same speed whatever the tempo. Tiles are in ticks,
//! so the tick → time map is piecewise linear here — exact at every tempo
//! point (and at the pieces a tempo ramp is cut into) — and the rainfall
//! splits its quads at those bends.

use plumspirit_engine::model::tempo::TempoMap;

/// Pieces a tempo ramp inside the window is cut into, at most.
pub const MAX_RAMP_PIECES: usize = 64;

/// B2-F20: breakpoints per ramp piece (one piece is about 12 pixels) before
/// a window is sampled instead of following every tempo segment — two per
/// pixel row.
const ROWS_PER_PIECE: usize = 24;

/// Song time (nanoseconds) of ticks in a window, piecewise linear.
#[derive(Clone, Debug, PartialEq)]
pub struct RainTime {
    /// Breakpoints (tick, nanos), ticks ascending, covering the window.
    points: Vec<(f64, f64)>,
}

impl RainTime {
    /// The time of ticks `lo ..= hi` from `map` (nanosecond frames): exact
    /// for constant tempos (affine between tempo points), a ramp cut into
    /// `ramp_pieces` pieces (1 ..= [`MAX_RAMP_PIECES`]).
    pub fn new(map: &TempoMap, lo: u64, hi: u64, ramp_pieces: usize) -> RainTime {
        let hi = hi.max(lo + 1);
        let pieces = ramp_pieces.clamp(1, MAX_RAMP_PIECES) as u64;
        // B2-F20: a window over more tempo segments than the rain has rows
        // (an hours-long falling time, a song with a tempo event per beat) is
        // sampled at exact ticks about one row of time apart instead — the
        // per-frame work stays bounded by the view, the error under a row.
        let rows = pieces as usize * ROWS_PER_PIECE;
        if map.span_count(lo, hi) > rows {
            return RainTime::sampled(map, lo, hi, rows);
        }
        let mut points: Vec<(f64, f64)> = Vec::new();
        let mut push = |t: u64, f: f64| {
            if points.last().is_none_or(|&(lt, _)| (t as f64) > lt) {
                points.push((t as f64, f));
            }
        };
        for span in map.spans(lo, hi) {
            let end = span.hi.saturating_add(1);
            match span.linear {
                Some((seg_tick, seg_frames, per_tick)) => {
                    let at = |t: u64| seg_frames + (t as f64 - seg_tick as f64) * per_tick;
                    push(span.lo, at(span.lo));
                    push(end, at(end));
                }
                None => {
                    let n = pieces.min(end - span.lo).max(1);
                    for k in 0..=n {
                        let t = span.lo + (end - span.lo) * k / n;
                        push(t, map.frames_at_tick_f64(t));
                    }
                }
            }
        }
        if points.len() < 2 {
            points = vec![
                (lo as f64, map.frames_at_tick_f64(lo)),
                (hi as f64, map.frames_at_tick_f64(hi)),
            ];
        }
        RainTime { points }
    }

    /// `rows` + 1 exact breakpoints evenly spaced in time over `lo ..= hi`
    /// (fewer where several rows fall in one tick).
    fn sampled(map: &TempoMap, lo: u64, hi: u64, rows: usize) -> RainTime {
        let (n_lo, n_hi) = (map.frames_at_tick_f64(lo), map.frames_at_tick_f64(hi));
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(rows + 1);
        for k in 0..=rows {
            let t = if k == rows {
                hi
            } else {
                let n = n_lo + (n_hi - n_lo) * k as f64 / rows as f64;
                map.tick_at_frame(n as u64).clamp(lo, hi)
            };
            if points.last().is_none_or(|&(lt, _)| (t as f64) > lt) {
                points.push((t as f64, map.frames_at_tick_f64(t)));
            }
        }
        RainTime { points }
    }

    /// Nanoseconds at tick `t` (linear between breakpoints; outside the
    /// window the end pieces continue).
    pub fn nanos(&self, t: f64) -> f64 {
        let p = &self.points;
        let i = p.partition_point(|&(pt, _)| pt <= t).clamp(1, p.len() - 1);
        let (t0, f0) = p[i - 1];
        let (t1, f1) = p[i];
        if t1 <= t0 {
            return f0;
        }
        f0 + (t - t0) * (f1 - f0) / (t1 - t0)
    }

    /// The interior breakpoint ticks (where quads split), ascending.
    pub fn bends(&self) -> Vec<f64> {
        let n = self.points.len();
        self.points[1..n.saturating_sub(1)]
            .iter()
            .map(|&(t, _)| t)
            .collect()
    }
}

/// v1.0-beta2 (B2-F11, contract §010): what equal heights of the rainfall
/// stand for. Only the vertical position of the notes changes — never the
/// events, counts, note starts, keyboard state, statistics or export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mapping {
    /// Equal song seconds, equal heights through the tempo map (the Player
    /// before v1.0-beta2): a note falls at the same speed whatever the tempo.
    #[default]
    Time,
    /// Equal ticks, equal heights whatever the tempo: a note's body is as
    /// long as its ticks; the falling speed follows the tempo.
    Tick,
}

impl Mapping {
    pub fn key(self) -> &'static str {
        match self {
            Mapping::Time => "time",
            Mapping::Tick => "tick",
        }
    }

    pub fn from_key(k: &str) -> Option<Mapping> {
        match k {
            "time" => Some(Mapping::Time),
            "tick" => Some(Mapping::Tick),
            _ => None,
        }
    }
}

/// The rainfall's vertical axis for one frame: the ticks of the window and
/// the height (0 = the hit line, 1 = the top) of any tick in it.
#[derive(Clone, Debug, PartialEq)]
pub struct RainAxis {
    /// First tick of the window (at or below the hit line).
    pub lo: u64,
    /// Tick just above the top of the window.
    pub hi: u64,
    kind: AxisKind,
}

#[derive(Clone, Debug, PartialEq)]
enum AxisKind {
    /// Song nanoseconds `now` at the hit line, `fall` of them to the top.
    Time { time: RainTime, now: f64, fall: f64 },
    /// The fractional tick `at` at the hit line, `span` ticks to the top.
    Tick { at: f64, span: f64 },
}

impl RainAxis {
    /// Time-Based (the Player before v1.0-beta2, unchanged): the window from
    /// song nanoseconds `now` to `now + fall`, ramps cut into `ramp_pieces`.
    pub fn time(map: &TempoMap, now: f64, fall: f64, ramp_pieces: usize) -> RainAxis {
        let lo = map.tick_at_frame(now.max(0.0) as u64);
        let hi = map
            .tick_at_frame((now + fall).ceil() as u64)
            .saturating_add(1)
            .max(lo + 1);
        RainAxis {
            lo,
            hi,
            kind: AxisKind::Time {
                time: RainTime::new(map, lo, hi, ramp_pieces),
                now,
                fall,
            },
        }
    }

    /// Tick-Based: the playhead at song nanoseconds `now` as a fractional
    /// tick (so it moves smoothly between ticks) and [`tick_span`] ticks to
    /// the top.
    pub fn tick(map: &TempoMap, now: f64, fall: f64) -> RainAxis {
        let now = now.max(0.0);
        let lo = map.tick_at_frame(now as u64);
        let (n0, n1) = (
            map.frames_at_tick_f64(lo),
            map.frames_at_tick_f64(lo.saturating_add(1)),
        );
        let part = if n1 > n0 {
            ((now - n0) / (n1 - n0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let at = lo as f64 + part;
        let span = tick_span(map, fall);
        // `as` saturates: an hours-long window ends at the last tick.
        let hi = ((at + span).ceil() as u64).saturating_add(1).max(lo + 1);
        RainAxis {
            lo,
            hi,
            kind: AxisKind::Tick { at, span },
        }
    }

    /// The height of tick `t` (0 = the hit line, 1 = the top).
    pub fn frac(&self, t: f64) -> f64 {
        match &self.kind {
            AxisKind::Time { time, now, fall } => (time.nanos(t) - now) / fall,
            AxisKind::Tick { at, span } => (t - at) / span,
        }
    }

    /// Ticks where the quads split (ramps in Time-Based; none in
    /// Tick-Based, where the axis is linear).
    pub fn bends(&self) -> Vec<f64> {
        match &self.kind {
            AxisKind::Time { time, .. } => time.bends(),
            AxisKind::Tick { .. } => Vec::new(),
        }
    }
}

/// Tick-Based: the ticks between the hit line and the top — the falling
/// time at the song's opening tempo (the tempo at tick 0), the same for the
/// whole song so equal ticks are equal heights everywhere (B2-F11).
pub fn tick_span(map: &TempoMap, fall: f64) -> f64 {
    let per_tick = map.frames_at_tick_f64(1) - map.frames_at_tick_f64(0);
    // B2-F20: any positive falling time, a fraction of a tick included.
    (fall / per_tick.max(f64::MIN_POSITIVE)).max(f64::MIN_POSITIVE)
}

/// The song time the rainfall is drawn at (P9). The transport's position
/// moves once per audio period (about 10 ms) while a 60 Hz frame lasts
/// 16.7 ms, so drawing at the position would stutter. While playing, the
/// clock anchors at the transport and runs on with the wall clock × speed;
/// it snaps back to the transport whenever the two differ by more than one
/// period (a seek, a loop wrap, a stall), so it never drifts. Paused or
/// stopped, it is exactly the transport's position.
#[derive(Clone, Copy, Debug, Default)]
pub struct SmoothClock {
    /// (song nanoseconds, when, speed) of the anchor.
    anchor: Option<(f64, std::time::Instant, f64)>,
}

impl SmoothClock {
    /// The song nanoseconds to draw at `now`, for the transport at
    /// `transport` song nanoseconds. `period`: the audio period in song
    /// nanoseconds (wall period × speed).
    pub fn time(
        &mut self,
        transport: f64,
        running: bool,
        speed: f64,
        period: f64,
        now: std::time::Instant,
    ) -> f64 {
        if !running {
            self.anchor = None;
            return transport;
        }
        if let Some((n0, t0, s0)) = self.anchor
            && s0 == speed
        {
            let t = n0 + now.saturating_duration_since(t0).as_secs_f64() * 1e9 * speed;
            if (t - transport).abs() <= period {
                return t;
            }
        }
        self.anchor = Some((transport, now, speed));
        transport
    }
}

/// Pieces for a ramp over a rainfall `height_px` tall: about one per 12
/// pixels, so the curve is smooth on screen.
pub fn ramp_pieces(height_px: f32) -> usize {
    ((height_px / 12.0).ceil() as usize).clamp(1, MAX_RAMP_PIECES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::conductor::Interp;
    use plumspirit_engine::model::{Bpm, PlumDocument, TempoValue};

    fn tempo(d: &mut PlumDocument, tick: u64, bpm: &str, interp: Interp) {
        let edit = d
            .conductor()
            .prepare_set_tempo(
                None,
                tick,
                TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                interp,
            )
            .unwrap();
        History::default()
            .apply(
                d,
                Operation::Conductor {
                    edit,
                    label: "Tempo",
                },
            )
            .unwrap();
    }

    #[test]
    fn the_smooth_clock_runs_between_periods_and_snaps_to_the_transport() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let period = 10e6; // 10 ms of song time
        let mut c = SmoothClock::default();
        // Paused: exactly the transport.
        assert_eq!(c.time(5e9, false, 1.0, period, ms(0)), 5e9);
        // Playing: anchored, then running with the wall clock between the
        // transport's steps (the transport says 5.000 s until it steps).
        assert_eq!(c.time(5e9, true, 1.0, period, ms(0)), 5e9);
        let a = c.time(5e9, true, 1.0, period, ms(6));
        assert!((a - 5.006e9).abs() < 1e3, "{a}");
        // The transport steps to 5.010 s at 10 ms: no jump, the clock runs on.
        let b = c.time(5.010e9, true, 1.0, period, ms(12));
        assert!((b - 5.012e9).abs() < 1e3, "{b}");
        // Never more than one period from the transport.
        for k in 0..200u64 {
            let tr = 5e9 + (k / 10 * 10) as f64 * 1e6; // steps every 10 ms
            let t = c.time(tr, true, 1.0, period, ms(k));
            assert!((t - tr).abs() <= period + 1.0, "{k}: {t} vs {tr}");
        }
        // A loop wrap / seek: snaps at once.
        assert_eq!(c.time(1e9, true, 1.0, period, ms(201)), 1e9);
        // Speed 2: song time runs twice as fast (re-anchored on the change).
        assert_eq!(c.time(1.5e9, true, 2.0, 2.0 * period, ms(300)), 1.5e9);
        let f = c.time(1.5e9, true, 2.0, 2.0 * period, ms(305));
        assert!((f - 1.510e9).abs() < 1e3, "{f}");
        // A stalled transport: the clock stops one period ahead, then snaps.
        let g = c.time(1.5e9, true, 2.0, 2.0 * period, ms(400));
        assert_eq!(g, 1.5e9, "more than a period off: back to the transport");
        // Paused again: exact.
        assert_eq!(c.time(1.234e9, false, 2.0, period, ms(401)), 1.234e9);
    }

    #[test]
    fn rain_time_is_exact_at_tempo_points_and_linear_between() {
        let mut d = PlumDocument::new_project("t".into(), Bpm::parse("100").unwrap(), 960);
        let map = TempoMap::for_document(&d, 1_000_000_000);
        let rt = RainTime::new(&map, 1_000, 50_000, 16);
        // 100 BPM at 960 PPQN: 625,000 ns per tick, fractional ticks too.
        assert!((rt.nanos(1_500.5) - rt.nanos(1_000.0) - 500.5 * 625_000.0).abs() < 1.0);
        assert!(rt.bends().is_empty(), "one tempo: no bends");
        // A step to 200 BPM at 9,600, a ramp from 20,000 to 60 BPM at 30,000.
        tempo(&mut d, 9_600, "200", Interp::Step);
        tempo(&mut d, 20_000, "150", Interp::Linear);
        tempo(&mut d, 30_000, "60", Interp::Step);
        let map = TempoMap::for_document(&d, 1_000_000_000);
        let rt = RainTime::new(&map, 1_000, 50_000, 16);
        let bends = rt.bends();
        assert!(bends.contains(&9_600.0) && bends.contains(&20_000.0) && bends.contains(&30_000.0));
        assert!(bends.windows(2).all(|w| w[0] < w[1]), "ascending");
        // The ramp is cut into 16 pieces: 15 bends inside it.
        assert_eq!(
            bends
                .iter()
                .filter(|&&t| t > 20_000.0 && t < 30_000.0)
                .count(),
            15
        );
        // Exact at every breakpoint, monotone everywhere.
        for &t in bends.iter().chain([1_000.0, 50_000.0].iter()) {
            let want = map.frames_at_tick_f64(t as u64);
            assert!((rt.nanos(t) - want).abs() < 1.0, "{t}");
        }
        let mut last = f64::MIN;
        for k in 0..=4_900 {
            let v = rt.nanos(1_000.0 + k as f64 * 10.0);
            assert!(v > last);
            last = v;
        }
        // The step's two sides: 625,000 then 312,500 ns per tick.
        assert!((rt.nanos(9_000.0) - rt.nanos(8_000.0) - 625_000_000.0).abs() < 1.0);
        assert!((rt.nanos(11_000.0) - rt.nanos(10_000.0) - 312_500_000.0).abs() < 1.0);
        assert_eq!(ramp_pieces(0.0), 1);
        assert_eq!(ramp_pieces(600.0), 50);
        assert_eq!(ramp_pieces(10_000.0), MAX_RAMP_PIECES);
    }

    /// A song at 60 BPM switching to 240 BPM at 4 beats, ramping 240 -> 90
    /// from 8 to 12 beats, at `ppq`.
    fn switching(ppq: u32) -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::parse("60").unwrap(), ppq);
        let q = u64::from(ppq);
        tempo(&mut d, 4 * q, "240", Interp::Step);
        tempo(&mut d, 8 * q, "240", Interp::Linear);
        tempo(&mut d, 12 * q, "90", Interp::Step);
        d
    }

    /// B2-F11: Time-Based keeps the Player's axis exactly (the window, every
    /// height bit for bit, the bends) — what the view computed inline before.
    #[test]
    fn time_based_rainfall_keeps_the_players_old_axis_exactly() {
        for ppq in [96u32, 960, 15_360] {
            let map = TempoMap::for_document(&switching(ppq), 1_000_000_000);
            for now in [0.0, 1.7e9, 3.999e9, 4.5e9, 6.25e9, 3_600e9] {
                for fall in [0.25e9, 2e9, 20e9] {
                    let axis = RainAxis::time(&map, now, fall, 37);
                    // The pre-beta2 computation.
                    let tick = map.tick_at_frame(now as u64);
                    let top = map
                        .tick_at_frame((now + fall).ceil() as u64)
                        .saturating_add(1)
                        .max(tick + 1);
                    let time = RainTime::new(&map, tick, top, 37);
                    assert_eq!((axis.lo, axis.hi), (tick, top));
                    assert_eq!(axis.bends(), time.bends());
                    let (lo, hi) = (tick as f64, top as f64);
                    for k in 0..=64 {
                        let t = lo + (hi - lo) * f64::from(k) / 64.0;
                        let old = (time.nanos(t) - now) / fall;
                        assert_eq!(
                            axis.frac(t).to_bits(),
                            old.to_bits(),
                            "{ppq} {now} {fall} {t}"
                        );
                    }
                }
            }
        }
    }

    /// B2-F11: Tick-Based — equal ticks are equal heights through a 60 ->
    /// 240 BPM switch and a ramp, at odd PPQN, from tick 0, far into a long
    /// song and with no tempo event; the window is the falling time at the
    /// opening tempo; the playhead moves smoothly and four times as fast at
    /// 240 BPM as at 60.
    #[test]
    fn tick_based_rainfall_gives_equal_ticks_equal_heights_through_tempo_changes() {
        let fall = 2e9;
        for ppq in [96u32, 960, 15_360] {
            let q = f64::from(ppq);
            let map = TempoMap::for_document(&switching(ppq), 1_000_000_000);
            // 60 BPM at tick 0: two seconds are two beats.
            let span = tick_span(&map, fall);
            assert!((span - 2.0 * q).abs() < 1e-6 * q, "{ppq}: {span}");
            // From tick 0, before / across / after the switch, in the ramp,
            // and ten hours in.
            for now in [0.0, 1.0e9, 3.5e9, 4.0e9, 4.2e9, 5.3e9, 36_000e9] {
                let axis = RainAxis::tick(&map, now, fall);
                let AxisKind::Tick { at, span: s } = axis.kind else {
                    panic!("tick axis")
                };
                assert_eq!(s, span, "one window for the whole song");
                assert_eq!(axis.lo, at.floor() as u64);
                assert!(
                    axis.frac(at).abs() < 1e-12,
                    "the playhead is at the hit line"
                );
                assert!(
                    axis.frac(axis.hi as f64) >= 1.0,
                    "the window reaches the top"
                );
                assert!(axis.frac(axis.lo as f64) <= 0.0);
                assert!(axis.bends().is_empty(), "linear in ticks");
                // Equal ticks, equal heights, anywhere in the window.
                let step = span / 7.0;
                let h0 = axis.frac(at + step) - axis.frac(at);
                for k in 1..7 {
                    let t = at + step * f64::from(k);
                    let h = axis.frac(t + step) - axis.frac(t);
                    assert!((h - h0).abs() < 1e-9, "{ppq} {now} {k}: {h} vs {h0}");
                }
                // The playhead's tick is the song's (exact at whole ticks).
                let whole = map.frames_at_tick_f64(axis.lo);
                let at_whole = RainAxis::tick(&map, whole, fall);
                assert_eq!(at_whole.lo, axis.lo);
                assert!(at_whole.frac(axis.lo as f64).abs() < 1e-9);
            }
            // Smooth and tempo-aware: the playhead's ticks per second.
            let rate = |now: f64| {
                let a = RainAxis::tick(&map, now, fall);
                let b = RainAxis::tick(&map, now + 1e6, fall);
                (b.frac(0.0) - a.frac(0.0)) * -span / 1e-3
            };
            let (slow, fast) = (rate(1.0e9), rate(4.5e9));
            assert!((slow - q).abs() < 1e-6 * q * 100.0, "60 BPM: {slow}");
            assert!(
                (fast / slow - 4.0).abs() < 1e-6,
                "240 BPM is four times as fast"
            );
        }
        // No tempo event: the default 120 BPM.
        let d = PlumDocument::new_project("t".into(), Bpm::parse("120").unwrap(), 480);
        let map = TempoMap::for_document(&d, 1_000_000_000);
        assert!((tick_span(&map, fall) - 4.0 * 480.0).abs() < 1e-6);
        // Hours-long windows and the end of time never overflow.
        let axis = RainAxis::tick(&map, 1e9, 1e30);
        assert_eq!(axis.hi, u64::MAX);
        assert!(axis.frac(axis.lo as f64 + 1.0) > 0.0);
    }

    /// B2-F20: a window over thousands of tempo changes (an hours-long
    /// falling time) follows at most two breakpoints per pixel row, each
    /// exact, and stays within about a row of the true time everywhere.
    #[test]
    fn a_window_over_dense_tempo_changes_is_sampled_within_a_row() {
        let tempos: Vec<(u64, TempoValue)> = (1..5_000u64)
            .map(|k| {
                let bpm = if k % 2 == 0 { "90" } else { "250" };
                (k * 96, TempoValue::Bpm(Bpm::parse(bpm).unwrap()))
            })
            .collect();
        let map = TempoMap::from_tempos(1_000_000_000, 960, &tempos);
        let (lo, hi) = (0u64, 5_000 * 96);
        assert!(map.span_count(lo, hi) > 4_000);
        let pieces = 50;
        let rt = RainTime::new(&map, lo, hi, pieces);
        let rows = pieces * ROWS_PER_PIECE;
        assert!(rt.points.len() <= rows + 1, "{}", rt.points.len());
        for &(t, n) in &rt.points {
            assert_eq!(n, map.frames_at_tick_f64(t as u64), "exact breakpoints");
        }
        let total = map.frames_at_tick_f64(hi) - map.frames_at_tick_f64(lo);
        let slowest_tick = 60e9 / 90.0 / 960.0;
        let bound = total / rows as f64 + 2.0 * slowest_tick;
        for k in 0..20_000u64 {
            let t = k * hi / 20_000;
            let err = (rt.nanos(t as f64) - map.frames_at_tick_f64(t)).abs();
            assert!(err <= bound, "{t}: {err} > {bound}");
        }
        // Fewer segments than rows: every segment, as before.
        let few = RainTime::new(&map, 0, 960, pieces);
        assert!(
            few.points
                .iter()
                .all(|&(t, _)| (t as u64).is_multiple_of(96) || t as u64 == 961)
        );
    }

    /// B2-F20: from a microsecond to ten hours (and the smallest positive
    /// float), in both mappings, the axis stays finite where it matters, in
    /// order and inside the tick range.
    #[test]
    fn tiny_and_hours_long_falling_times_keep_the_axis_finite_and_ordered() {
        let mut d = switching(960);
        for k in 1..400u64 {
            tempo(
                &mut d,
                20_000 + k * 480,
                if k % 2 == 0 { "80" } else { "200" },
                Interp::Step,
            );
        }
        let map = TempoMap::for_document(&d, 1_000_000_000);
        for fall in [f64::MIN_POSITIVE, 1e3, 1e9, 3.6e13] {
            for now in [0.0, 2.5e9, 600e9] {
                for axis in [
                    RainAxis::time(&map, now, fall, 50),
                    RainAxis::tick(&map, now, fall),
                ] {
                    assert!(axis.lo < axis.hi);
                    let mut last = f64::NEG_INFINITY;
                    for k in 0..=100u64 {
                        let t = axis.lo as f64
                            + (axis.hi.min(axis.lo + 10_000_000) - axis.lo) as f64 * k as f64
                                / 100.0;
                        let f = axis.frac(t);
                        assert!(!f.is_nan(), "{fall} {now} {t}");
                        assert!(f >= last, "in order");
                        last = f;
                    }
                    assert!(axis.bends().len() <= 50 * ROWS_PER_PIECE + 1);
                }
            }
        }
    }

    #[test]
    fn the_mapping_reads_and_writes_its_keys() {
        for m in [Mapping::Time, Mapping::Tick] {
            assert_eq!(Mapping::from_key(m.key()), Some(m));
        }
        assert_eq!(Mapping::from_key("seconds"), None);
        assert_eq!(Mapping::default(), Mapping::Time);
    }
}
