//! Exact tick ↔ audio-frame conversion (the tempo map).
//!
//! Playback needs to know at which audio sample frame a tick sounds. That depends
//! on the PPQN, every tempo change in the song, the project tempo and the output
//! sample rate. All of it is centralised here, in integer arithmetic:
//!
//! * The song is split into segments at every tempo change. Within a segment the
//!   rate "frames per tick" is an exact fraction `num / den` (from microseconds per
//!   quarter note, or from the exact decimal project BPM), reduced by their GCD.
//! * Segment start times are held in 96.32 fixed-point frames (`u128`), computed
//!   from the previous segment with 256-bit intermediates. The only rounding is
//!   below 2^-32 of a frame per tempo change, so there is no audible drift even
//!   after millions of tempo changes or billions of ticks.
//! * Results saturate instead of wrapping: a tick beyond what `u64` frames can
//!   express maps to `u64::MAX`.
//!
//! * **Linear BPM ramps** (Conductor tempo points with a linear segment) are the
//!   one place where time is not a rational function of ticks: elapsed time is
//!   the integral of 1/BPM, `t(Δ) = 60/PPQN · Δ/b0 · ln(1+x)/x` with
//!   `x = (b1−b0)/b0 · Δ/L`. It is evaluated in `f64` with `ln_1p` / `expm1` (and
//!   their series near zero, the constant-tempo limit), so there is no
//!   cancellation; the relative error is ~1e-15, i.e. far below one audio frame
//!   even for hour-long ramps. Segments after a ramp continue exactly from its
//!   end point, so errors never accumulate across the song.
//!
//! Floating point is otherwise only used for display (`seconds_at_tick`).

use super::conductor::{Interp, TempoPoint, tempo_bpm_f64};
use super::document::PlumDocument;
use super::project::Bpm;
use super::timing::TimingKind;
use crate::wide::U256;

/// Default MIDI tempo: 500,000 µs per quarter note (120 BPM).
pub const DEFAULT_MICROS_PER_QUARTER: u32 = 500_000;

pub const FX_SHIFT: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Rate {
    /// Frames per tick = num / den (reduced, both non-zero).
    Exact { num: u128, den: u128 },
    /// Linear BPM ramp from `b0` to `b1` over `len` ticks; `k` = frames per
    /// (tick × BPM), i.e. `sample_rate · 60 / PPQN`.
    Ramp { b0: f64, b1: f64, len: u64, k: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    tick: u64,
    /// Start time in frames × 2^32.
    fx: u128,
    rate: Rate,
}

/// `ln(1+x)/x`, accurate near `x = 0` (the constant-tempo limit).
fn lnrel(x: f64) -> f64 {
    if x.abs() < 1e-4 {
        1.0 - x / 2.0 + x * x / 3.0 - x * x * x / 4.0
    } else {
        x.ln_1p() / x
    }
}

/// `(e^y − 1)/y`, accurate near `y = 0`.
fn expm1rel(y: f64) -> f64 {
    if y.abs() < 1e-4 {
        1.0 + y / 2.0 + y * y / 6.0 + y * y * y / 24.0
    } else {
        y.exp_m1() / y
    }
}

/// `∫₀^Δ dt / BPM(t)` for a linear ramp from `b0` to `b1` over `len` ticks, in
/// ticks per BPM (multiply by `60 / PPQN` for seconds).
pub(crate) fn ramp_integral(b0: f64, b1: f64, len: u64, d: f64) -> f64 {
    if len == 0 || d <= 0.0 {
        return 0.0;
    }
    let x = (b1 - b0) / b0 * (d / len as f64);
    d / b0 * lnrel(x)
}

/// Inverse of [`ramp_integral`]: the (fractional) tick offset whose integral is `i`.
pub(crate) fn ramp_inverse(b0: f64, b1: f64, len: u64, i: f64) -> f64 {
    if len == 0 || i <= 0.0 {
        return 0.0;
    }
    let y = (b1 - b0) * i / len as f64;
    b0 * i * expm1rel(y)
}

/// Seconds from tick `a` to tick `b` (`a <= b`) inside a linear ramp from `b0` to
/// `b1` over `len` ticks, measured from the ramp start. Stable for short spans.
pub(crate) fn ramp_span_seconds(ppqn: u64, b0: f64, b1: f64, len: u64, a: f64, b: f64) -> f64 {
    if b <= a {
        return 0.0;
    }
    // BPM at `a`, then the integral over [a, b] of a ramp starting there.
    let slope = (b1 - b0) / len.max(1) as f64;
    let ba = b0 + slope * a;
    let z = slope * (b - a) / ba;
    60.0 / ppqn.max(1) as f64 * (b - a) / ba * lnrel(z)
}

/// Playback speed: an audition multiplier (1 = the project's tempo, 2 = twice
/// as fast). It scales the tick → frame conversion of playback only; the
/// Conductor's tempo map, the project and exported files never change.
///
/// Stored as an exact decimal (`mantissa × 10^exponent`, at most 18
/// significant digits) from 0.000001× to 1,000,000,000×. These are limits of
/// the representation; whether a speed can be played in real time depends on
/// the song (see the playback report).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Speed {
    mantissa: u64,
    exponent: i8,
}

/// Most significant digits of a playback speed.
pub const MAX_SPEED_DIGITS: usize = super::project::MAX_BPM_DIGITS;

impl Speed {
    pub const NORMAL: Speed = Speed {
        mantissa: 1,
        exponent: 0,
    };

    /// Parses "1", "1.5", "0.25", "2x", "100 ×", "1e3" (a trailing x or × is
    /// allowed). Refuses zero, negative values, NaN/infinity and anything outside
    /// 0.000001–1,000,000,000.
    pub fn parse(input: &str) -> Result<Speed, String> {
        use crate::model::project::BpmError;
        let t = input.trim().trim_end_matches(['x', 'X', '\u{d7}']).trim();
        let example = "Enter a playback speed such as 0.5, 1, 2 or 100.";
        let b = match Bpm::parse(t) {
            Ok(b) => b,
            Err(BpmError::Negative) => {
                return Err(format!("The playback speed must be positive. {example}"));
            }
            Err(BpmError::Zero) => {
                return Err(format!(
                    "The playback speed must be greater than 0. {example}"
                ));
            }
            Err(BpmError::TooSmall) => {
                return Err("The playback speed can be as low as 0.000001×.".into());
            }
            Err(BpmError::TooLarge) => {
                return Err("The playback speed can be at most 1,000,000,000×.".into());
            }
            Err(BpmError::DecimalComma) => {
                return Err("Use a dot as the decimal separator, for example 1.5.".into());
            }
            Err(_) => {
                return Err(format!(
                    "\"{}\" is not a number. {example}",
                    input.trim().chars().take(30).collect::<String>()
                ));
            }
        };
        let (m, e) = (b.mantissa(), b.exponent());
        let digits = m.ilog10() as i32 + 1;
        if digits as usize > MAX_SPEED_DIGITS {
            return Err(format!(
                "PlumSpirit keeps up to {MAX_SPEED_DIGITS} significant digits of a playback speed."
            ));
        }
        let magnitude = e + digits - 1;
        if magnitude > 9 || (magnitude == 9 && m != 1) {
            return Err("The playback speed can be at most 1,000,000,000×.".into());
        }
        if magnitude < -6 {
            return Err("The playback speed can be as low as 0.000001×.".into());
        }
        Ok(Speed {
            mantissa: m,
            exponent: e as i8,
        })
    }

    /// `(numerator, denominator)` of the exact value.
    pub fn as_ratio(self) -> (u128, u128) {
        let m = u128::from(self.mantissa);
        if self.exponent >= 0 {
            (m * 10u128.pow(self.exponent as u32), 1)
        } else {
            (m, 10u128.pow(u32::from(self.exponent.unsigned_abs())))
        }
    }

    pub fn to_f64(self) -> f64 {
        self.mantissa as f64 * 10f64.powi(i32::from(self.exponent))
    }

    pub fn is_normal(self) -> bool {
        self == Speed::NORMAL
    }
}

impl Default for Speed {
    fn default() -> Self {
        Speed::NORMAL
    }
}

impl std::fmt::Display for Speed {
    /// Plain decimal ("0.25", "1", "1000").
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let digits = self.mantissa.to_string();
        if self.exponent >= 0 {
            f.write_str(&digits)?;
            for _ in 0..self.exponent {
                f.write_str("0")?;
            }
            Ok(())
        } else {
            let frac = self.exponent.unsigned_abs() as usize;
            if digits.len() > frac {
                let (i, d) = digits.split_at(digits.len() - frac);
                write!(f, "{i}.{d}")
            } else {
                write!(f, "0.{}{digits}", "0".repeat(frac - digits.len()))
            }
        }
    }
}

impl std::fmt::Debug for Speed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Speed({self})")
    }
}

/// Frames-per-tick `num / den` at playback `speed` (frames shrink by the
/// speed). Exact when it fits 128 bits; otherwise a 64-bit-mantissa
/// approximation (relative error ~1e-19, far below one frame).
fn apply_speed(num: u128, den: u128, speed: Speed) -> (u128, u128) {
    if speed.is_normal() {
        return (num, den);
    }
    let (sn, sd) = speed.as_ratio();
    let g1 = gcd(num, sn).max(1);
    let g2 = gcd(den, sd).max(1);
    let (n1, s1) = (num / g1, sn / g1);
    let (d1, t1) = (den / g2, sd / g2);
    match (n1.checked_mul(t1), d1.checked_mul(s1)) {
        (Some(x), Some(y)) => reduce(x, y),
        _ => {
            let f = (n1 as f64 / d1 as f64) * (t1 as f64 / s1 as f64);
            if f >= 1.0 {
                reduce(f.round().max(1.0) as u128, 1)
            } else {
                let scale = (1u128 << 64) as f64;
                reduce(((f * scale).round() as u128).max(1), 1u128 << 64)
            }
        }
    }
}

/// A tempo that applies from a tick onwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempoValue {
    /// Microseconds per quarter note (from a MIDI tempo event).
    Micros(u32),
    /// Exact project BPM.
    Bpm(Bpm),
}

/// Ticks `lo..=hi` inside one tempo segment (see [`TempoMap::spans`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoSpan {
    pub lo: u64,
    pub hi: u64,
    /// Constant rate: (segment tick, segment start in frames, frames per tick).
    pub linear: Option<(u64, f64, f64)>,
}

/// Exact conversion between ticks and frames at one sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct TempoMap {
    sample_rate: u32,
    segments: Vec<Segment>,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn reduce(num: u128, den: u128) -> (u128, u128) {
    let g = gcd(num, den).max(1);
    ((num / g).max(1), (den / g).max(1))
}

/// `ceil(r · den / (num · 2^32))`, saturating at `u128::MAX`.
fn ticks_for_fx(r: u128, num: u128, den: u128) -> u128 {
    if let (Some(top), Some(bottom)) = (r.checked_mul(den), num.checked_mul(1u128 << FX_SHIFT)) {
        return top.div_ceil(bottom);
    }
    let top = U256::mul_u128_u128(r, den);
    let Some(bottom) = U256::from_u128(num).checked_mul_u64(1u64 << FX_SHIFT) else {
        return 0;
    };
    let (q, rem) = top.div_rem(&bottom);
    match q.to_u128() {
        Some(v) if rem.is_zero() => v,
        Some(v) => v.saturating_add(1),
        None => u128::MAX,
    }
}

impl TempoMap {
    /// Builds the map for a metrical division (`ppqn` ticks per quarter note) from
    /// tempo changes `(tick, value)` sorted by tick. Several tempos at one tick: the
    /// last wins. Without a tempo at tick 0 the MIDI default (120 BPM) applies until
    /// the first change.
    pub fn from_tempos(sample_rate: u32, ppqn: u32, tempos: &[(u64, TempoValue)]) -> Self {
        TempoMap::from_tempos_at(sample_rate, ppqn, tempos, Speed::NORMAL)
    }

    /// [`TempoMap::from_tempos`] at a playback speed.
    pub fn from_tempos_at(
        sample_rate: u32,
        ppqn: u32,
        tempos: &[(u64, TempoValue)],
        speed: Speed,
    ) -> Self {
        let sr = u128::from(sample_rate.max(1));
        let ppqn = u128::from(ppqn.max(1));
        let rate = |v: TempoValue| -> (u128, u128) {
            let (n, d) = match v {
                // frames/tick = sr · µs / (1e6 · ppqn)
                TempoValue::Micros(us) => reduce(sr * u128::from(us.max(1)), 1_000_000 * ppqn),
                // frames/tick = sr · 60 / (bpm · ppqn) = sr · 60 · den / (num · ppqn)
                TempoValue::Bpm(b) => {
                    let (n, d) = b.as_ratio();
                    reduce(sr * 60 * d, n * ppqn)
                }
            };
            apply_speed(n, d, speed)
        };
        let (num, den) = rate(TempoValue::Micros(DEFAULT_MICROS_PER_QUARTER));
        let mut segments = vec![Segment {
            tick: 0,
            fx: 0,
            rate: Rate::Exact { num, den },
        }];
        for &(tick, value) in tempos {
            let (num, den) = rate(value);
            let last = *segments.last().expect("non-empty");
            if tick == last.tick {
                let l = segments.last_mut().expect("non-empty");
                l.rate = Rate::Exact { num, den };
                continue;
            }
            if tick < last.tick {
                continue; // unsorted input is ignored rather than trusted
            }
            let fx = last.fx.saturating_add(delta_fx(&last, tick - last.tick));
            segments.push(Segment {
                tick,
                fx,
                rate: Rate::Exact { num, den },
            });
        }
        TempoMap {
            sample_rate,
            segments,
        }
    }

    /// Builds the map from Conductor tempo points (sorted, the first at tick 0):
    /// step points give exact segments, linear points ramp to the next point.
    pub fn from_points(sample_rate: u32, ppqn: u32, points: &[TempoPoint]) -> Self {
        TempoMap::from_points_at(sample_rate, ppqn, points, Speed::NORMAL)
    }

    /// [`TempoMap::from_points`] at a playback speed.
    pub fn from_points_at(
        sample_rate: u32,
        ppqn: u32,
        points: &[TempoPoint],
        speed: Speed,
    ) -> Self {
        let sr = u128::from(sample_rate.max(1));
        let ppq = u128::from(ppqn.max(1));
        let exact = |v: TempoValue| -> Rate {
            let (num, den) = match v {
                TempoValue::Micros(us) => reduce(sr * u128::from(us.max(1)), 1_000_000 * ppq),
                TempoValue::Bpm(b) => {
                    let (n, d) = b.as_ratio();
                    reduce(sr * 60 * d, n * ppq)
                }
            };
            let (num, den) = apply_speed(num, den, speed);
            Rate::Exact { num, den }
        };
        let k = f64::from(sample_rate.max(1)) * 60.0 / f64::from(ppqn.max(1)) / speed.to_f64();
        let mut segments: Vec<Segment> = Vec::with_capacity(points.len().max(1));
        for (i, p) in points.iter().enumerate() {
            let rate = match (p.interp, points.get(i + 1)) {
                (Interp::Linear, Some(n)) if n.tick > p.tick => {
                    let (b0, b1) = (tempo_bpm_f64(p.value), tempo_bpm_f64(n.value));
                    if b0 == b1 || !(b0 > 0.0 && b1 > 0.0 && b0.is_finite() && b1.is_finite()) {
                        exact(p.value)
                    } else {
                        Rate::Ramp {
                            b0,
                            b1,
                            len: n.tick - p.tick,
                            k,
                        }
                    }
                }
                _ => exact(p.value),
            };
            let fx = match segments.last() {
                None => 0,
                Some(last) => last.fx.saturating_add(delta_fx(last, p.tick - last.tick)),
            };
            if segments.is_empty() && p.tick != 0 {
                // Defensive: the conductor always starts at tick 0.
                let (num, den) =
                    reduce(sr * u128::from(DEFAULT_MICROS_PER_QUARTER), 1_000_000 * ppq);
                let (num, den) = apply_speed(num, den, speed);
                segments.push(Segment {
                    tick: 0,
                    fx: 0,
                    rate: Rate::Exact { num, den },
                });
                let last = segments[0];
                segments.push(Segment {
                    tick: p.tick,
                    fx: delta_fx(&last, p.tick),
                    rate,
                });
                continue;
            }
            segments.push(Segment {
                tick: p.tick,
                fx,
                rate,
            });
        }
        if segments.is_empty() {
            return TempoMap::from_tempos_at(sample_rate, ppqn, &[], speed);
        }
        TempoMap {
            sample_rate,
            segments,
        }
    }

    /// Constant-rate map (SMPTE timing, or no timing information).
    fn constant(sample_rate: u32, num: u128, den: u128, speed: Speed) -> Self {
        let (num, den) = reduce(num.max(1), den.max(1));
        let (num, den) = apply_speed(num, den, speed);
        TempoMap {
            sample_rate,
            segments: vec![Segment {
                tick: 0,
                fx: 0,
                rate: Rate::Exact { num, den },
            }],
        }
    }

    /// The tempo map playback uses for `doc`: the Conductor's tempo map for
    /// metrical projects. Format 2 files (independent sequences, whose tempo events
    /// stay in their tracks) merge those events in tick order. SMPTE divisions
    /// ignore tempo (ticks are fractions of frames of real time).
    pub fn for_document(doc: &PlumDocument, sample_rate: u32) -> Self {
        TempoMap::for_document_at(doc, sample_rate, Speed::NORMAL)
    }

    /// The playback tempo map at a playback speed (frames are audition frames:
    /// `speed` 2 plays every tick in half the time). The Conductor is unchanged.
    pub fn for_document_at(doc: &PlumDocument, sample_rate: u32, speed: Speed) -> Self {
        let sr = u128::from(sample_rate.max(1));
        match doc.division().kind() {
            TimingKind::Ppqn(0) => TempoMap::constant(sample_rate, sr, 1000, speed),
            TimingKind::Ppqn(ppqn) if doc.conductor().timing_editable() => {
                TempoMap::from_points_at(sample_rate, ppqn, doc.conductor().tempo_points(), speed)
            }
            TimingKind::Ppqn(ppqn) => {
                let mut tempos: Vec<(u64, TempoValue)> = doc
                    .tracks()
                    .iter()
                    .flat_map(|t| t.events.tempos())
                    .map(|(t, us)| (t, TempoValue::Micros(us)))
                    .collect();
                // Stable: at equal ticks, later tracks win (track order).
                tempos.sort_by_key(|e| e.0);
                TempoMap::from_tempos_at(sample_rate, ppqn, &tempos, speed)
            }
            TimingKind::Smpte {
                frames_per_second,
                ticks_per_frame,
            } => {
                if ticks_per_frame == 0 {
                    return TempoMap::constant(sample_rate, sr, 1000, speed);
                }
                // 29 = 29.97 fps drop-frame (30000/1001).
                let (fps_num, fps_den) = match frames_per_second {
                    29 => (30_000u128, 1001u128),
                    0 => (25, 1),
                    f => (u128::from(f), 1),
                };
                // frames/tick = sr / (fps · tpf)
                TempoMap::constant(
                    sample_rate,
                    sr * fps_den,
                    fps_num * u128::from(ticks_per_frame),
                    speed,
                )
            }
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of tempo segments (tests, diagnostics).
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    fn segment_for_tick(&self, tick: u64) -> &Segment {
        let i = self
            .segments
            .partition_point(|s| s.tick <= tick)
            .saturating_sub(1);
        &self.segments[i]
    }

    /// Start time of `tick` in frames × 2^32.
    pub fn fx_at_tick(&self, tick: u64) -> u128 {
        let s = self.segment_for_tick(tick);
        s.fx.saturating_add(delta_fx(s, tick - s.tick))
    }

    /// v0.9 (V09-S34 `tempo_time`): the tempo in BPM at `tick` for a song of
    /// `ppqn` ticks per quarter note — a constant segment's exact rate, or a
    /// ramp's linear BPM at that tick (its end tempo past the ramp).
    pub fn bpm_at_tick(&self, tick: u64, ppqn: u64) -> f64 {
        let s = self.segment_for_tick(tick);
        match s.rate {
            // frames / tick = num / den; BPM = sample_rate · 60 / (ppqn · frames / tick).
            Rate::Exact { num, den } => {
                f64::from(self.sample_rate) * 60.0 * den as f64 / (ppqn.max(1) as f64 * num as f64)
            }
            Rate::Ramp { b0, b1, len, .. } => {
                let d = (tick - s.tick).min(len);
                if len == 0 {
                    b1
                } else {
                    b0 + (b1 - b0) * d as f64 / len as f64
                }
            }
        }
    }

    /// v0.9 (V09-S34): the slowest tempo anywhere in the map (BPM; a ramp's
    /// lower end) — preflight bounds of real-time steps.
    pub fn min_bpm(&self, ppqn: u64) -> f64 {
        self.segments
            .iter()
            .map(|s| match s.rate {
                Rate::Exact { num, den } => {
                    f64::from(self.sample_rate) * 60.0 * den as f64
                        / (ppqn.max(1) as f64 * num as f64)
                }
                Rate::Ramp { b0, b1, .. } => b0.min(b1),
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// The fastest tempo of the map in BPM (the most ticks a second can
    /// hold — preflight and preview reach of real-time grids).
    pub fn max_bpm(&self, ppqn: u64) -> f64 {
        self.segments
            .iter()
            .map(|s| match s.rate {
                Rate::Exact { num, den } => {
                    f64::from(self.sample_rate) * 60.0 * den as f64
                        / (ppqn.max(1) as f64 * num as f64)
                }
                Rate::Ramp { b0, b1, .. } => b0.max(b1),
            })
            .fold(0.0, f64::max)
    }

    /// The audio frame at which `tick` sounds (floor), saturating at `u64::MAX`.
    pub fn frame_at_tick(&self, tick: u64) -> u64 {
        u64::try_from(self.fx_at_tick(tick) >> FX_SHIFT).unwrap_or(u64::MAX)
    }

    /// The first tick that sounds at or after `frame`: the smallest `t` with
    /// `frame_at_tick(t) >= frame`. Saturates at `u64::MAX`.
    pub fn first_tick_at_or_after_frame(&self, frame: u64) -> u64 {
        self.first_tick_at_or_after_fx(u128::from(frame) << FX_SHIFT)
    }

    /// The first tick whose start time is at or after `target` (frames ×
    /// 2^32, as [`TempoMap::fx_at_tick`]): the smallest `t` with
    /// `fx_at_tick(t) >= target`. Saturates at `u64::MAX`. v0.9 (V09-S13):
    /// exact physical-time steps (Paste Repeat) without rounding the start
    /// to a whole frame first.
    pub fn first_tick_at_or_after_fx(&self, target: u128) -> u64 {
        let j = self.segments.partition_point(|s| s.fx < target);
        self.first_tick_in(j, target)
    }

    /// [`TempoMap::first_tick_at_or_after_fx`] given `j` = the number of
    /// segments that start before `target` (shared with [`TempoCursor`]).
    fn first_tick_in(&self, j: usize, target: u128) -> u64 {
        if j == 0 {
            return 0;
        }
        let s = &self.segments[j - 1];
        let r = target - s.fx;
        let d = match s.rate {
            // smallest d with floor(d · num · 2^32 / den) >= r ⇔ d >= r · den / (num · 2^32)
            Rate::Exact { num, den } => ticks_for_fx(r, num, den),
            Rate::Ramp { len, .. } => u128::from(ramp_ticks_for_fx(s, r, len)),
        };
        let t = u128::from(s.tick).saturating_add(d);
        let t = match self.segments.get(j) {
            Some(n) => t.min(u128::from(n.tick)),
            None => t,
        };
        u64::try_from(t).unwrap_or(u64::MAX)
    }

    /// The last tick that has started by `frame` (for the playhead): the largest `t`
    /// with `frame_at_tick(t) <= frame`.
    pub fn tick_at_frame(&self, frame: u64) -> u64 {
        match frame.checked_add(1) {
            Some(f) => self.first_tick_at_or_after_frame(f).saturating_sub(1),
            None => u64::MAX,
        }
    }

    /// Start time of `tick` in (fractional) frames.
    pub fn frames_at_tick_f64(&self, tick: u64) -> f64 {
        self.fx_at_tick(tick) as f64 / (1u128 << FX_SHIFT) as f64
    }

    /// The tempo segments covering ticks `lo..=hi`, clipped to that range, for
    /// integrating over time (analysis). A constant-rate span carries
    /// `(segment tick, segment start in frames, frames per tick)`, so the time of
    /// any tick in it is affine; a tempo ramp carries `None` (evaluate each tick
    /// with [`TempoMap::frames_at_tick_f64`]).
    pub fn spans(&self, lo: u64, hi: u64) -> Vec<TempoSpan> {
        let mut out = Vec::new();
        if lo > hi {
            return out;
        }
        let first = self
            .segments
            .partition_point(|s| s.tick <= lo)
            .saturating_sub(1);
        for (i, s) in self.segments.iter().enumerate().skip(first) {
            if s.tick > hi {
                break;
            }
            let seg_hi = self
                .segments
                .get(i + 1)
                .map_or(u64::MAX, |n| n.tick.saturating_sub(1));
            let (a, b) = (s.tick.max(lo), seg_hi.min(hi));
            if a > b {
                continue;
            }
            let linear = match s.rate {
                Rate::Exact { num, den } => Some((
                    s.tick,
                    s.fx as f64 / (1u128 << FX_SHIFT) as f64,
                    num as f64 / den as f64,
                )),
                Rate::Ramp { .. } => None,
            };
            out.push(TempoSpan {
                lo: a,
                hi: b,
                linear,
            });
        }
        out
    }

    /// How many tempo segments cover ticks `lo..=hi` — at least as many as
    /// [`TempoMap::spans`] returns — without building them (two binary
    /// searches). v1.0-beta2: the MIDI Player bounds its per-frame work by it.
    pub fn span_count(&self, lo: u64, hi: u64) -> usize {
        if lo > hi {
            return 0;
        }
        let first = self
            .segments
            .partition_point(|s| s.tick <= lo)
            .saturating_sub(1);
        let end = self.segments.partition_point(|s| s.tick <= hi);
        end.saturating_sub(first)
    }

    /// Seconds from the start to `tick` (display only).
    pub fn seconds_at_tick(&self, tick: u64) -> f64 {
        let fx = self.fx_at_tick(tick);
        (fx as f64 / (1u128 << FX_SHIFT) as f64) / f64::from(self.sample_rate.max(1))
    }
}

/// Monotone lookups on a [`TempoMap`] (v0.9 V09-S19,
/// `docs/SELECTION_PEAKS.md`): while the ticks (or frame targets) asked
/// never decrease, each answer costs O(1) amortised instead of a binary
/// search over the segments. The answers equal [`TempoMap::fx_at_tick`] and
/// [`TempoMap::first_tick_at_or_after_fx`] exactly; a smaller argument than
/// the previous one is answered too (by a search that resets the cursor).
#[derive(Clone, Debug)]
pub struct TempoCursor<'a> {
    map: &'a TempoMap,
    /// Segment holding the last tick asked.
    seg: usize,
    last_tick: u64,
    /// Segments that start before the last frame target asked.
    before: usize,
    last_fx: u128,
}

impl<'a> TempoCursor<'a> {
    pub fn new(map: &'a TempoMap) -> TempoCursor<'a> {
        // The first lookup binary-searches (as a backward one does) instead
        // of walking forward from the first segment: a cursor made for one
        // lookup (a per-note rule in a lazy preview) costs O(log segments),
        // not O(segments). Every answer is the same either way.
        TempoCursor {
            map,
            seg: 0,
            last_tick: u64::MAX,
            before: 0,
            last_fx: u128::MAX,
        }
    }

    /// [`TempoMap::fx_at_tick`].
    #[inline]
    pub fn fx_at_tick(&mut self, tick: u64) -> u128 {
        let segs = &self.map.segments;
        if tick < self.last_tick {
            self.seg = segs.partition_point(|s| s.tick <= tick).saturating_sub(1);
        } else {
            while segs.get(self.seg + 1).is_some_and(|n| n.tick <= tick) {
                self.seg += 1;
            }
        }
        self.last_tick = tick;
        let s = &segs[self.seg];
        s.fx.saturating_add(delta_fx(s, tick - s.tick))
    }

    /// [`TempoMap::first_tick_at_or_after_fx`].
    #[inline]
    pub fn first_tick_at_or_after_fx(&mut self, target: u128) -> u64 {
        let segs = &self.map.segments;
        if target < self.last_fx {
            self.before = segs.partition_point(|s| s.fx < target);
        } else {
            while segs.get(self.before).is_some_and(|s| s.fx < target) {
                self.before += 1;
            }
        }
        self.last_fx = target;
        self.map.first_tick_in(self.before, target)
    }

    /// [`TempoMap::first_tick_at_or_after_frame`].
    #[inline]
    pub fn first_tick_at_or_after_frame(&mut self, frame: u64) -> u64 {
        self.first_tick_at_or_after_fx(u128::from(frame) << FX_SHIFT)
    }
}

/// Frames × 2^32 covered by `ticks` ticks from the segment start (floor),
/// saturating.
fn delta_fx(s: &Segment, ticks: u64) -> u128 {
    match s.rate {
        Rate::Exact { num, den } => exact_delta_fx(num, den, ticks),
        Rate::Ramp { b0, b1, len, k } => {
            // Past the ramp's end (only when asked defensively) the end tempo holds.
            let (inside, beyond) = if ticks > len {
                (len, ticks - len)
            } else {
                (ticks, 0)
            };
            let frames = k * ramp_integral(b0, b1, len, inside as f64) + k * beyond as f64 / b1;
            let fx = frames * (1u64 << FX_SHIFT) as f64;
            if fx.is_finite() && fx >= 0.0 {
                // `as` saturates for values beyond u128.
                fx as u128
            } else {
                u128::MAX
            }
        }
    }
}

fn exact_delta_fx(num: u128, den: u128, ticks: u64) -> u128 {
    let a = u128::from(ticks);
    if let Some(p) = a.checked_mul(num)
        && p.leading_zeros() >= FX_SHIFT
    {
        return (p << FX_SHIFT) / den;
    }
    // a < 2^64 and num < 2^128, so a · num · 2^32 < 2^224 fits in 256 bits.
    match U256::mul_u128_u128(a, num).checked_mul_u64(1u64 << FX_SHIFT) {
        Some(p) => p
            .div_rem(&U256::from_u128(den))
            .0
            .to_u128()
            .unwrap_or(u128::MAX),
        None => u128::MAX,
    }
}

/// Smallest tick offset `d` in `[0, len]` of a ramp segment with
/// `delta_fx(d) >= r`: an analytic estimate refined against the forward
/// function, so it is consistent with `frame_at_tick` by construction.
fn ramp_ticks_for_fx(s: &Segment, r: u128, len: u64) -> u64 {
    let Rate::Ramp { b0, b1, k, .. } = s.rate else {
        return 0;
    };
    let frames = r as f64 / (1u64 << FX_SHIFT) as f64;
    let est = ramp_inverse(b0, b1, len, frames / k);
    let mut d = if est.is_finite() && est > 0.0 {
        (est.floor() as u64).min(len)
    } else {
        0
    };
    let ok = |d: u64| delta_fx(s, d) >= r;
    // Local correction (the estimate is within a tick or two), then a binary
    // search as a fallback for extreme values.
    let mut steps = 0;
    while d > 0 && ok(d - 1) && steps < 64 {
        d -= 1;
        steps += 1;
    }
    if steps == 64 {
        let (mut lo, mut hi) = (0u64, d);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if ok(mid) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        return lo;
    }
    steps = 0;
    while d < len && !ok(d) && steps < 64 {
        d += 1;
        steps += 1;
    }
    if steps == 64 {
        let (mut lo, mut hi) = (d, len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if ok(mid) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        return lo;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::project::Bpm;

    /// v1.0-beta2: `span_count` never undercounts `spans` and equals it on
    /// ordinary maps, without building them.
    #[test]
    fn span_count_bounds_the_spans_without_building_them() {
        let tempos: Vec<(u64, TempoValue)> = (1..200u64)
            .map(|k| {
                let bpm = if k % 2 == 0 { "100" } else { "173.5" };
                (k * 97, TempoValue::Bpm(Bpm::parse(bpm).unwrap()))
            })
            .collect();
        for m in [
            TempoMap::from_tempos(1_000_000_000, 960, &[]),
            TempoMap::from_tempos(1_000_000_000, 960, &tempos),
        ] {
            for (lo, hi) in [
                (0, 0),
                (0, 96),
                (0, 97),
                (5, 20_000),
                (970, 971),
                (19_303, 19_400),
                (19_400, u64::MAX),
                (0, u64::MAX),
            ] {
                assert_eq!(m.span_count(lo, hi), m.spans(lo, hi).len(), "{lo}..={hi}");
            }
            assert_eq!(m.span_count(10, 9), 0);
        }
    }

    #[test]
    fn constant_tempo_is_exact() {
        // 120 BPM, 960 PPQN, 48 kHz: one quarter = 0.5 s = 24,000 frames.
        let m = TempoMap::from_tempos(48_000, 960, &[]);
        assert_eq!(m.frame_at_tick(0), 0);
        assert_eq!(m.frame_at_tick(960), 24_000);
        assert_eq!(m.frame_at_tick(960 * 1_000_000), 24_000_000_000);
        assert_eq!(m.frame_at_tick(1), 25); // 25 frames per tick exactly
        assert_eq!(m.first_tick_at_or_after_frame(24_000), 960);
        assert_eq!(m.first_tick_at_or_after_frame(24_001), 961);
        assert_eq!(m.tick_at_frame(24_024), 960);
        assert_eq!(m.tick_at_frame(24_025), 961);
        assert!((m.seconds_at_tick(960) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn tempo_changes_are_piecewise() {
        // 120 BPM for one quarter, then 60 BPM (1,000,000 µs).
        let m = TempoMap::from_tempos(48_000, 480, &[(480, TempoValue::Micros(1_000_000))]);
        assert_eq!(m.frame_at_tick(480), 24_000);
        assert_eq!(m.frame_at_tick(960), 24_000 + 48_000);
        assert_eq!(m.segment_count(), 2);
        assert_eq!(m.first_tick_at_or_after_frame(72_000), 960);
        // Several tempos at one tick: the last one wins.
        let m2 = TempoMap::from_tempos(
            48_000,
            480,
            &[
                (0, TempoValue::Micros(250_000)),
                (0, TempoValue::Micros(1_000_000)),
            ],
        );
        assert_eq!(m2.frame_at_tick(480), 48_000);
    }

    #[test]
    fn no_drift_over_many_changes() {
        // 100,000 alternating tempo changes whose frame lengths are fractional;
        // the result must match the exact rational sum within one frame.
        let ppqn = 97u32;
        let sr = 44_100u32;
        let mut tempos = Vec::new();
        let mut exact_num: u128 = 0; // Σ ticks·µs·sr, over (1e6·ppqn)
        let mut tick = 0u64;
        for i in 0..100_000u64 {
            let us = if i % 2 == 0 { 333_333 } else { 499_999 };
            tempos.push((tick, TempoValue::Micros(us)));
            exact_num += 13 * u128::from(us) * u128::from(sr);
            tick += 13;
        }
        let m = TempoMap::from_tempos(sr, ppqn, &tempos);
        let exact = exact_num / (1_000_000 * u128::from(ppqn));
        let got = u128::from(m.frame_at_tick(tick));
        assert!(got.abs_diff(exact) <= 1, "{got} vs {exact}");
    }

    #[test]
    fn exact_decimal_bpm_and_extremes() {
        // 123.456 BPM at 1000 PPQN, 48 kHz: one quarter = 60/123.456 s.
        let b = Bpm::parse("123.456").unwrap();
        let m = TempoMap::from_tempos(48_000, 1000, &[(0, TempoValue::Bpm(b))]);
        // 1000 quarters = 1000 · 60 / 123.456 s = 486.00311… s = 23,328,149.3 frames
        assert_eq!(m.frame_at_tick(1_000_000), 23_328_149);
        // Enormous BPM: millions of ticks pass per frame, nothing overflows.
        let fast = Bpm::parse("1e12").unwrap();
        let m = TempoMap::from_tempos(48_000, 2_147_483_647, &[(0, TempoValue::Bpm(fast))]);
        assert_eq!(m.frame_at_tick(0), 0);
        let t = m.first_tick_at_or_after_frame(1);
        assert!(t > 1_000_000_000, "{t}");
        assert_eq!(m.frame_at_tick(u64::MAX), m.frame_at_tick(u64::MAX));
        // Tiny BPM: frames saturate instead of wrapping.
        let slow = Bpm::parse("0.000001").unwrap();
        let m = TempoMap::from_tempos(192_000, 1, &[(0, TempoValue::Bpm(slow))]);
        assert_eq!(m.frame_at_tick(u64::MAX), u64::MAX);
        assert!(m.frame_at_tick(1) > 0);
        assert_eq!(
            m.first_tick_at_or_after_frame(u64::MAX),
            m.first_tick_at_or_after_frame(u64::MAX)
        );
    }

    #[test]
    fn monotonic_and_inverse_consistent() {
        let m = TempoMap::from_tempos(
            44_100,
            96,
            &[
                (0, TempoValue::Micros(612_345)),
                (1000, TempoValue::Micros(123)),
                (5000, TempoValue::Bpm(Bpm::parse("33.3").unwrap())),
            ],
        );
        let mut prev = 0;
        for t in (0..20_000u64).step_by(7) {
            let f = m.frame_at_tick(t);
            assert!(f >= prev);
            prev = f;
            // Inverse: first tick sounding at or after f is <= t, and ticks before
            // it sound earlier than f.
            let a = m.first_tick_at_or_after_frame(f);
            assert!(a <= t);
            assert!(m.frame_at_tick(a) >= f);
            if a > 0 {
                assert!(m.frame_at_tick(a - 1) < f);
            }
        }
    }

    #[test]
    fn document_tempo_sources() {
        let d = PlumDocument::new_project("x".into(), Bpm::parse("60").unwrap(), 480);
        let m = TempoMap::for_document(&d, 48_000);
        assert_eq!(m.frame_at_tick(480), 48_000);
        let e = PlumDocument::new_empty(crate::model::Division::ppqn(480));
        let m = TempoMap::for_document(&e, 48_000);
        assert_eq!(m.frame_at_tick(480), 24_000); // MIDI default 120 BPM
        let s = PlumDocument::new_empty(crate::model::Division::from_raw(0xE728)); // 25 fps × 40
        let m = TempoMap::for_document(&s, 48_000);
        assert_eq!(m.frame_at_tick(1000), 48_000); // 1000 ticks = 1 s
    }

    fn point(tick: u64, bpm: &str, interp: Interp) -> TempoPoint {
        TempoPoint {
            id: tick as u32 + 1,
            tick,
            value: TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
            interp,
            implicit: false,
            origin: None,
        }
    }

    /// Reference: midpoint rule with Kahan summation over `steps` sub-intervals.
    fn reference_seconds(ppqn: f64, b0: f64, b1: f64, len: f64, d: f64, steps: u64) -> f64 {
        let h = d / steps as f64;
        let (mut sum, mut c) = (0.0f64, 0.0f64);
        for i in 0..steps {
            let t = (i as f64 + 0.5) * h;
            let y = h * 60.0 / (ppqn * (b0 + (b1 - b0) * t / len)) - c;
            let s2 = sum + y;
            c = (s2 - sum) - y;
            sum = s2;
        }
        sum
    }

    #[test]
    fn linear_ramps_match_a_high_precision_reference() {
        // 100 → 200 BPM over 16 bars at 960 PPQN, then 200 BPM steady.
        let ppqn = 960u32;
        let len = 960 * 4 * 16;
        let m = TempoMap::from_points(
            48_000,
            ppqn,
            &[
                point(0, "100", Interp::Linear),
                point(len, "200", Interp::Step),
            ],
        );
        assert_eq!(m.segment_count(), 2);
        for d in [1u64, 7, 960, len / 3, len / 2, len - 1, len] {
            let exact = reference_seconds(960.0, 100.0, 200.0, len as f64, d as f64, 2_000_000);
            let got = m.seconds_at_tick(d);
            assert!((got - exact).abs() < 1e-9, "tick {d}: {got} vs {exact}");
        }
        // Closed form: 60/PPQN · L/(b1−b0) · ln(b1/b0).
        let closed = 60.0 / 960.0 * len as f64 / 100.0 * 2f64.ln();
        assert!((m.seconds_at_tick(len) - closed).abs() < 1e-9);
        // After the ramp the tempo is exactly 200 BPM: one beat = 0.3 s.
        let beat = m.seconds_at_tick(len + 960) - m.seconds_at_tick(len);
        assert!((beat - 0.3).abs() < 1e-6, "{beat}");
        // A linear time approximation would be wrong: the ramp is not the mean.
        let naive_start_tempo = len as f64 / 960.0 * 0.6;
        assert!((m.seconds_at_tick(len) - naive_start_tempo).abs() > 1.0);
    }

    #[test]
    fn descending_tiny_and_extreme_ramps_stay_monotonic_and_invertible() {
        for (b0, b1, len) in [
            ("300", "60", 7_680u64),
            ("120", "120.000001", 960),
            ("0.5", "1000000", 100_000),
            ("1e9", "1", 3),
            ("60", "61", 1_000_000_000_000),
        ] {
            let m = TempoMap::from_points(
                44_100,
                96,
                &[point(0, b0, Interp::Linear), point(len, b1, Interp::Step)],
            );
            let mut prev = 0u64;
            let n = 400u64;
            for i in 0..=n {
                let t = (len as u128 * u128::from(i) / u128::from(n)) as u64;
                let f = m.frame_at_tick(t);
                assert!(f >= prev, "{b0}->{b1}: frame went backwards at {t}");
                prev = f;
                let a = m.first_tick_at_or_after_frame(f);
                assert!(a <= t, "{b0}->{b1}: inverse {a} > {t}");
                assert!(m.frame_at_tick(a) >= f);
                if a > 0 {
                    assert!(m.frame_at_tick(a - 1) < f, "{b0}->{b1} at {t}");
                }
            }
        }
    }

    #[test]
    fn a_ramp_between_equal_tempos_is_exact_constant() {
        let m = TempoMap::from_points(
            48_000,
            960,
            &[
                point(0, "120", Interp::Linear),
                point(960, "120", Interp::Step),
            ],
        );
        assert_eq!(m.frame_at_tick(960), 24_000);
        assert_eq!(m.frame_at_tick(1920), 48_000);
    }
}

#[cfg(test)]
mod speed_tests {
    use super::*;
    use crate::model::conductor::{Interp, TempoPoint};
    use crate::model::project::Bpm;

    fn pt(tick: u64, bpm: &str, interp: Interp) -> TempoPoint {
        TempoPoint {
            id: tick as u32 + 1,
            tick,
            value: TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
            interp,
            implicit: false,
            origin: None,
        }
    }

    #[test]
    fn speeds_parse_exactly_and_refuse_invalid_values() {
        for (t, shown) in [
            ("1", "1"),
            ("0.25", "0.25"),
            ("2x", "2"),
            ("100 ×", "100"),
            ("1e3", "1000"),
            ("1000000000", "1000000000"),
            ("0.000001", "0.000001"),
            ("1.5X", "1.5"),
            ("0.123456789012345", "0.123456789012345"),
            ("1.0000000000001", "1.0000000000001"),
        ] {
            assert_eq!(Speed::parse(t).unwrap().to_string(), shown, "{t}");
        }
        for bad in [
            "",
            "0",
            "-2",
            "abc",
            "NaN",
            "inf",
            "1e10",
            "0.0000001",
            "1,5",
            "1.0000000000000000001",
        ] {
            assert!(Speed::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn speed_divides_audition_time_over_steps_and_ramps() {
        let points = [
            pt(0, "120", Interp::Step),
            pt(960, "60", Interp::Linear),
            pt(4800, "240", Interp::Step),
        ];
        let normal = TempoMap::from_points(48_000, 480, &points);
        for (s, factor) in [("2", 2.0), ("0.5", 0.5), ("1000", 1000.0), ("0.001", 0.001)] {
            let fast = TempoMap::from_points_at(48_000, 480, &points, Speed::parse(s).unwrap());
            for tick in [0u64, 480, 960, 2000, 4800, 100_000] {
                let a = normal.seconds_at_tick(tick);
                let b = fast.seconds_at_tick(tick);
                assert!(
                    (a / factor - b).abs() <= 1e-9 * a.max(1.0),
                    "speed {s} tick {tick}: {a} / {factor} vs {b}"
                );
                // Round trip tick ↔ frame stays consistent at every speed.
                let f = fast.frame_at_tick(tick);
                assert!(fast.first_tick_at_or_after_frame(f) <= tick);
            }
        }
        // Speed 1 is exactly the old map.
        assert_eq!(
            TempoMap::from_points_at(48_000, 480, &points, Speed::NORMAL),
            normal
        );
    }

    #[test]
    fn a_tempo_cursor_answers_exactly_like_the_map_in_any_order() {
        // Steps and ramps, ticks and frames asked ascending, with repeats and
        // with jumps back (V09-S19).
        let names = ["120", "61.5", "333", "40", "200", "97"];
        let points: Vec<_> = (0..60u64)
            .map(|i| {
                let interp = if i % 3 == 0 {
                    Interp::Linear
                } else {
                    Interp::Step
                };
                pt(
                    i * 700 + (i % 4) * 13,
                    names[i as usize % names.len()],
                    interp,
                )
            })
            .collect();
        let map = TempoMap::from_points(1_000_000_000, 960, &points);
        let mut x = 0x9E37_79B9u64;
        let mut r = move |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n.max(1)
        };
        let end = 60 * 700 + 5_000;
        let end_fx = map.fx_at_tick(end);
        for round in 0..4 {
            let mut c = TempoCursor::new(&map);
            let mut ticks: Vec<u64> = (0..3_000).map(|_| r(end)).collect();
            let mut fxs: Vec<u128> = (0..3_000)
                .map(|_| u128::from(r(u64::MAX)) % end_fx)
                .collect();
            if round < 2 {
                ticks.sort_unstable();
                fxs.sort_unstable();
            }
            ticks.extend([0, 0, end, 1, end]);
            fxs.extend([0, 1, end_fx, end_fx + 7, 0]);
            for (&t, &f) in ticks.iter().zip(&fxs) {
                assert_eq!(c.fx_at_tick(t), map.fx_at_tick(t), "tick {t}");
                assert_eq!(
                    c.first_tick_at_or_after_fx(f),
                    map.first_tick_at_or_after_fx(f),
                    "fx {f}"
                );
                let fr = (f >> FX_SHIFT) as u64;
                assert_eq!(
                    c.first_tick_at_or_after_frame(fr),
                    map.first_tick_at_or_after_frame(fr),
                    "frame {fr}"
                );
            }
        }
    }
}
