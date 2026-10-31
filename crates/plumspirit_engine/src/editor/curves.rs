//! Curve drawing for the event graphs: shapes, event spacing, free-hand strokes
//! and "Only Before the Note".
//!
//! A curve runs from a start anchor `(tick₀, v₀)` to an end anchor
//! `(tick₁, v₁)`. At each generated tick the position `u = (tick − tick₀) /
//! (tick₁ − tick₀)` (0…1) goes through the shape function `f`, and the value is
//! `v₀ + f(u) · (v₁ − v₀)`, rounded to the nearest integer of the target (half
//! away from zero). Floating point is used only for this sampling; the result
//! is an exact MIDI integer.
//!
//! | Shape | `f(u)` (ease-out; ease-in is `1 − f(1 − u)`) |
//! |---|---|
//! | Line | `u` |
//! | Gentle Curve | `1 − (1 − u)²` (quadratic: moves quickly, then settles) |
//! | Fast Curve | `(1 − e^(−5u)) / (1 − e^(−5))` (exponential: most of the change happens early) |
//! | S Curve | `3u² − 2u³` (smoothstep: slow start, fast middle, slow end; monotonic) |
//! | Random | a seeded pseudo-random value in the random range at every step |
//! | Custom | the user's formula in `t` (`editor::expr`), `t = u` |
//!
//! Every built-in shape has `f(0) = 0` and `f(1) = 1` exactly, so the anchors'
//! values are reproduced exactly. A custom formula may leave that range: its
//! samples are then refused (listing where) unless the user chose to clamp
//! them, and "Normalize Endpoints" rescales `f` to `(f(u) − f(0)) / (f(1) −
//! f(0))` without changing the formula text.
//!
//! Random values come from SplitMix64 seeded with the curve's seed and the
//! step index, so the same seed always gives the same events (the edit stores
//! the events, so Redo reproduces them exactly anyway).
//!
//! Every generator checks its event count against a budget *before*
//! allocating, and refuses (never truncates) when it would exceed it.

use super::EditError;
use super::expr::Expr;
use crate::model::note_store::NoteStore;
use crate::model::patch::RankSet;
use crate::view::grid::MeterMap;

/// Built-in and custom curve shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    Line,
    Gentle,
    Fast,
    SCurve,
    Random,
    Custom,
}

impl Shape {
    pub const ALL: [Shape; 6] = [
        Shape::Line,
        Shape::Gentle,
        Shape::Fast,
        Shape::SCurve,
        Shape::Random,
        Shape::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Shape::Line => "Line",
            Shape::Gentle => "Gentle Curve",
            Shape::Fast => "Fast Curve",
            Shape::SCurve => "S Curve",
            Shape::Random => "Random",
            Shape::Custom => "Custom Function",
        }
    }

    /// Gentle and Fast bend one way or the other.
    pub fn has_bend(self) -> bool {
        matches!(self, Shape::Gentle | Shape::Fast)
    }
}

/// Which end of a Gentle / Fast curve moves quickly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Bend {
    /// Fast change at the start, settling at the end.
    #[default]
    EaseOut,
    /// Slow start, fast change at the end.
    EaseIn,
}

const FAST_K: f64 = 5.0;

/// The normalised shape `f(u)` for `u` in 0…1 (built-in shapes only).
pub fn shape_value(shape: Shape, bend: Bend, u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    let out = |g: fn(f64) -> f64, u: f64| match bend {
        Bend::EaseOut => g(u),
        Bend::EaseIn => 1.0 - g(1.0 - u),
    };
    match shape {
        Shape::Line | Shape::Random | Shape::Custom => u,
        Shape::Gentle => out(|u| 1.0 - (1.0 - u) * (1.0 - u), u),
        Shape::Fast => out(|u| (1.0 - (-FAST_K * u).exp()) / (1.0 - (-FAST_K).exp()), u),
        Shape::SCurve => u * u * (3.0 - 2.0 * u),
    }
}

/// SplitMix64 (deterministic, platform-independent).
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform value in `[0, 1)` for step `k` of seed `seed`.
pub fn random_unit(seed: u64, k: u64) -> f64 {
    (splitmix64(seed ^ splitmix64(k)) >> 11) as f64 / (1u64 << 53) as f64
}

/// A curve between two anchors.
#[derive(Clone, Debug)]
pub struct CurveSpec<'a> {
    pub shape: Shape,
    pub bend: Bend,
    /// Start anchor (tick, value in target units).
    pub from: (u64, f64),
    /// End anchor.
    pub to: (u64, f64),
    /// Random: seed and the (inclusive) value range.
    pub seed: u64,
    pub random_range: (f64, f64),
    /// Custom: the parsed formula.
    pub custom: Option<&'a Expr>,
    /// Custom: clamp samples outside the target range instead of refusing them.
    pub clamp: bool,
    /// Custom: rescale so the formula starts at the start value and ends at the
    /// end value.
    pub normalize: bool,
    /// Inclusive integer range of the target (velocity 1–127, CC 0–127, …).
    pub limits: (i64, i64),
}

impl CurveSpec<'_> {
    /// Normalised position of `tick` along the curve (0 at `from`, 1 at `to`).
    fn u(&self, tick: u64) -> f64 {
        let (a, b) = (self.from.0, self.to.0);
        if a == b {
            return 0.0;
        }
        let d = tick as f64 - a as f64;
        (d / (b as f64 - a as f64)).clamp(0.0, 1.0)
    }

    /// Custom normalisation: `(f(0), f(1))` when normalising.
    fn custom_ends(&self) -> Result<Option<(f64, f64)>, EditError> {
        match (self.shape, self.custom, self.normalize) {
            (Shape::Custom, Some(e), true) => {
                let f0 = e.eval(0.0).map_err(|x| EditError::Curve(x.to_string()))?;
                let f1 = e.eval(1.0).map_err(|x| EditError::Curve(x.to_string()))?;
                if (f1 - f0).abs() < 1e-12 {
                    return Err(EditError::Curve(
                        "Normalize Endpoints needs a formula whose value at t = 1 differs from its value at t = 0.".into(),
                    ));
                }
                Ok(Some((f0, f1)))
            }
            _ => Ok(None),
        }
    }

    /// The (unrounded) value at step `k`, tick `tick`.
    fn raw(&self, k: u64, tick: u64, ends: Option<(f64, f64)>) -> Result<f64, EditError> {
        let (v0, v1) = (self.from.1, self.to.1);
        match self.shape {
            Shape::Random => {
                let (lo, hi) = (
                    self.random_range.0.min(self.random_range.1),
                    self.random_range.0.max(self.random_range.1),
                );
                // Inclusive integer range: floor(lo + r · (hi − lo + 1)).
                let r = random_unit(self.seed, k);
                Ok((lo + r * (hi - lo + 1.0)).floor().min(hi))
            }
            Shape::Custom => {
                let e = self.custom.ok_or_else(|| {
                    EditError::Curve("Choose or enter a custom function first.".into())
                })?;
                let u = self.u(tick);
                let mut f = e.eval(u).map_err(|x| EditError::Curve(x.to_string()))?;
                if let Some((f0, f1)) = ends {
                    f = (f - f0) / (f1 - f0);
                }
                Ok(v0 + f * (v1 - v0))
            }
            s => Ok(v0 + shape_value(s, self.bend, self.u(tick)) * (v1 - v0)),
        }
    }

    /// Integer value at a tick; refuses out-of-range custom samples unless
    /// clamping.
    fn quantize(&self, v: f64, tick: u64) -> Result<i64, EditError> {
        if !v.is_finite() {
            return Err(EditError::Curve(format!(
                "The curve has no finite value at tick {tick}."
            )));
        }
        let r = v.round();
        let (lo, hi) = self.limits;
        if r < lo as f64 || r > hi as f64 {
            if self.clamp || self.shape != Shape::Custom {
                return Ok((r.clamp(lo as f64, hi as f64)) as i64);
            }
            return Err(EditError::Curve(format!(
                "The custom function goes outside the allowed values ({lo} to {hi}) at tick {tick} (value {r}). Adjust the formula or anchors, or turn on Clamp."
            )));
        }
        Ok(r as i64)
    }

    /// Samples the curve at `ticks` (sorted); one `(tick, value)` per tick.
    pub fn sample(&self, ticks: &[u64]) -> Result<Vec<(u64, i64)>, EditError> {
        let ends = self.custom_ends()?;
        let mut out = Vec::new();
        out.try_reserve_exact(ticks.len())
            .map_err(|_| EditError::OutOfMemory {
                bytes: ticks.len().saturating_mul(16),
            })?;
        for (k, &t) in ticks.iter().enumerate() {
            let v = self.raw(k as u64, t, ends)?;
            out.push((t, self.quantize(v, t)?));
        }
        Ok(out)
    }

    /// `n + 1` evenly spaced `(u, value)` samples for the preview, from the same
    /// evaluator as [`CurveSpec::sample`] (unrounded values; an evaluation error
    /// is returned as-is).
    pub fn preview(&self, n: usize) -> Result<Vec<(f64, f64)>, EditError> {
        let ends = self.custom_ends()?;
        let (a, b) = (self.from.0, self.to.0);
        let n = n.max(1);
        (0..=n)
            .map(|i| {
                let u = i as f64 / n as f64;
                let tick = if b >= a {
                    a + ((b - a) as f64 * u) as u64
                } else {
                    a - ((a - b) as f64 * u) as u64
                };
                self.raw(i as u64, tick, ends).map(|v| (u, v))
            })
            .collect()
    }
}

/// How far apart generated events are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interval {
    /// A fixed number of ticks (≥ 1).
    Ticks(u64),
    /// One event per beat of the time signature in effect.
    Beat,
    /// One event per measure (at each measure start).
    Measure,
}

impl Interval {
    pub fn label(self, ppqn: Option<u64>) -> String {
        match self {
            Interval::Beat => "Every beat".into(),
            Interval::Measure => "Every measure".into(),
            Interval::Ticks(t) => match ppqn {
                Some(p) if p > 0 => match subdivision_name(t, p) {
                    Some(n) => format!("{n} ({t} ticks)"),
                    None => format!("{t} ticks"),
                },
                _ => format!("{t} ticks"),
            },
        }
    }
}

/// "1/16", "1/8 triplet", … when `ticks` is such a note value at `ppqn`.
pub fn subdivision_name(ticks: u64, ppqn: u64) -> Option<&'static str> {
    const NAMES: [(u64, u64, &str); 13] = [
        (4, 1, "1/1"),
        (2, 1, "1/2"),
        (1, 1, "1/4"),
        (1, 2, "1/8"),
        (1, 4, "1/16"),
        (1, 8, "1/32"),
        (1, 16, "1/64"),
        (1, 32, "1/128"),
        (2, 3, "1/4 triplet"),
        (1, 3, "1/8 triplet"),
        (1, 6, "1/16 triplet"),
        (1, 12, "1/32 triplet"),
        (1, 24, "1/64 triplet"),
    ];
    NAMES
        .iter()
        .find(|(n, d, _)| (ppqn * n).is_multiple_of(*d) && ppqn * n / d == ticks)
        .map(|x| x.2)
}

/// Ticks from `t0` to `t1` (both included, in order) at `interval`: `t0`, then
/// every interval step, and always `t1` (so the end anchor is written exactly).
/// Refused when more than `budget` ticks would be generated.
pub fn interval_ticks(
    t0: u64,
    t1: u64,
    interval: Interval,
    meter: &MeterMap,
    budget: usize,
) -> Result<Vec<u64>, EditError> {
    let (t0, t1) = (t0.min(t1), t0.max(t1));
    let too_many = |n: u128| EditError::TooManyEvents {
        requested: usize::try_from(n).unwrap_or(usize::MAX),
        limit: budget,
    };
    let mut out: Vec<u64> = Vec::new();
    match interval {
        Interval::Ticks(step) => {
            let step = step.max(1);
            let n = u128::from((t1 - t0) / step) + 2;
            if n > budget as u128 + 1 {
                return Err(too_many(n));
            }
            out.try_reserve_exact(n as usize)
                .map_err(|_| EditError::OutOfMemory {
                    bytes: (n as usize).saturating_mul(8),
                })?;
            let mut t = t0;
            loop {
                out.push(t);
                match t.checked_add(step) {
                    Some(n) if n < t1 => t = n,
                    _ => break,
                }
            }
        }
        Interval::Beat | Interval::Measure => {
            if !meter.is_musical() {
                return Err(EditError::Curve(
                    "This project has no beats or measures (SMPTE timing); choose a tick interval."
                        .into(),
                ));
            }
            out.push(t0);
            let mut t = t0;
            loop {
                let next = match interval {
                    Interval::Measure => meter.next_bar(t),
                    _ => next_beat(meter, t),
                };
                match next {
                    Some(n) if n < t1 && n > t => {
                        out.push(n);
                        if out.len() > budget {
                            return Err(too_many(out.len() as u128 + 1));
                        }
                        t = n;
                    }
                    _ => break,
                }
            }
        }
    }
    if out.last() != Some(&t1) {
        out.push(t1);
    }
    if out.len() > budget {
        return Err(too_many(out.len() as u128));
    }
    Ok(out)
}

/// The first beat boundary after `tick`.
fn next_beat(meter: &MeterMap, tick: u64) -> Option<u64> {
    let (_, dpow) = meter.signature_at(tick)?;
    let ppqn = meter.ppqn()?;
    let bar = meter.bar_start_at_or_before(tick);
    // Beat k of this bar starts at bar + floor(k · 4P / 2^dpow).
    let num = u128::from(ppqn) * 4;
    let den = 1u128 << dpow;
    let within = u128::from(tick - bar);
    let k = (within * den / num) + 1;
    let candidate = u128::from(bar) + k * num / den;
    let next_bar = meter.next_bar(tick).map_or(u128::MAX, u128::from);
    u64::try_from(candidate.min(next_bar)).ok()
}

/// A free-hand stroke: pointer samples `(tick, value)` in the order they were
/// drawn become one value per `interval` grid tick between the stroke's first
/// and last tick, linearly interpolated between consecutive samples (so fast
/// movements leave no gaps); where the stroke passes a tick again, the later
/// pass wins. Values are rounded and clamped to `limits`.
pub fn freehand(
    samples: &[(u64, f64)],
    interval: u64,
    limits: (i64, i64),
    budget: usize,
) -> Result<Vec<(u64, i64)>, EditError> {
    let step = interval.max(1);
    let (Some(lo), Some(hi)) = (
        samples.iter().map(|s| s.0).min(),
        samples.iter().map(|s| s.0).max(),
    ) else {
        return Ok(Vec::new());
    };
    let first = lo / step * step;
    let slots = ((hi - first) / step) as u128 + 1;
    if slots > budget as u128 {
        return Err(EditError::TooManyEvents {
            requested: usize::try_from(slots).unwrap_or(usize::MAX),
            limit: budget,
        });
    }
    let mut values: Vec<Option<f64>> = vec![None; slots as usize];
    let slot = |t: u64| ((t - first) / step) as usize;
    let mut put = |t: u64, v: f64| {
        if t >= first {
            let i = slot(t);
            if i < values.len() && t.is_multiple_of(step) {
                values[i] = Some(v);
            }
        }
    };
    if samples.len() == 1 {
        let (t, v) = samples[0];
        put(t / step * step, v);
    }
    for w in samples.windows(2) {
        let ((ta, va), (tb, vb)) = (w[0], w[1]);
        let (a, b) = (ta.min(tb), ta.max(tb));
        // Grid ticks between the two samples (inclusive).
        let mut g = a.div_ceil(step) * step;
        if a == b {
            put(a / step * step, vb);
            continue;
        }
        while g <= b {
            let f = (g - ta.min(tb)) as f64 / (b - a) as f64;
            let v = if ta <= tb {
                va + (vb - va) * f
            } else {
                vb + (va - vb) * f
            };
            put(g, v);
            match g.checked_add(step) {
                Some(n) => g = n,
                None => break,
            }
        }
    }
    Ok(values
        .into_iter()
        .enumerate()
        .filter_map(|(i, v)| {
            v.map(|v| {
                (
                    first + i as u64 * step,
                    (v.round() as i64).clamp(limits.0, limits.1),
                )
            })
        })
        .collect())
}

/// "Only Before the Note": the notes a curve applies to in `[t0, t1]` (inclusive)
/// — the selected ones when `selected_only`, otherwise every note starting in
/// the range, optionally only on `channel`. Returns their ranks and start ticks
/// (one per note, in stored order). Refused beyond `budget` notes.
pub fn notes_in_scope(
    store: &NoteStore,
    t0: u64,
    t1: u64,
    selected_only: bool,
    channel: Option<u8>,
    budget: usize,
) -> Result<(RankSet, Vec<u64>), EditError> {
    let (t0, t1) = (t0.min(t1), t0.max(t1));
    // Cheap upper bound before visiting notes.
    if !selected_only && store.estimate_in_range(t0, t1) > budget.saturating_mul(4) {
        return Err(EditError::TooManyEvents {
            requested: store.estimate_in_range(t0, t1),
            limit: budget,
        });
    }
    let (ranks, starts) = store.starting_in(t0, t1, selected_only, channel);
    if starts.len() > budget {
        return Err(EditError::TooManyEvents {
            requested: starts.len(),
            limit: budget,
        });
    }
    Ok((ranks, starts))
}

/// Distinct note start ticks (for controller events written before notes).
pub fn distinct_ticks(starts: &[u64]) -> Vec<u64> {
    let mut v = starts.to_vec();
    v.dedup();
    v
}

/// The largest number of events one operation may generate for an undo
/// budget of `undo_bytes`: an event costs about 56 bytes in the undo record
/// and the event list together; at most a quarter of the budget is used, and
/// never more than 4,000,000 events.
pub fn event_budget(undo_bytes: usize) -> usize {
    (undo_bytes / 4 / 56).clamp(1_000, 4_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord};
    use crate::model::timing::Division;

    fn spec(shape: Shape) -> CurveSpec<'static> {
        CurveSpec {
            shape,
            bend: Bend::EaseOut,
            from: (0, 0.0),
            to: (960, 127.0),
            seed: 7,
            random_range: (10.0, 20.0),
            custom: None,
            clamp: false,
            normalize: false,
            limits: (0, 127),
        }
    }

    fn meter() -> MeterMap {
        MeterMap::new(Division::ppqn(480), &[(0, 4, 2)])
    }

    #[test]
    fn every_built_in_shape_keeps_exact_endpoints_and_its_character() {
        for s in [Shape::Line, Shape::Gentle, Shape::Fast, Shape::SCurve] {
            for b in [Bend::EaseOut, Bend::EaseIn] {
                assert_eq!(shape_value(s, b, 0.0), 0.0, "{s:?} {b:?}");
                assert_eq!(shape_value(s, b, 1.0), 1.0, "{s:?} {b:?}");
                // Monotonic.
                let mut last = 0.0;
                for i in 0..=100 {
                    let v = shape_value(s, b, f64::from(i) / 100.0);
                    assert!(v >= last - 1e-15, "{s:?} not monotonic");
                    last = v;
                }
            }
        }
        let mid = |s, b| shape_value(s, b, 0.5);
        assert_eq!(mid(Shape::Line, Bend::EaseOut), 0.5);
        assert_eq!(mid(Shape::SCurve, Bend::EaseOut), 0.5);
        assert_eq!(mid(Shape::Gentle, Bend::EaseOut), 0.75);
        assert!(
            mid(Shape::Fast, Bend::EaseOut) > 0.9,
            "fast is steeper than gentle"
        );
        assert!(mid(Shape::Gentle, Bend::EaseIn) < 0.5);
        assert!(mid(Shape::Fast, Bend::EaseIn) < mid(Shape::Gentle, Bend::EaseIn));
        // S curve is slow at both ends.
        assert!(shape_value(Shape::SCurve, Bend::EaseOut, 0.1) < 0.1);
        assert!(shape_value(Shape::SCurve, Bend::EaseOut, 0.9) > 0.9);
    }

    #[test]
    fn sampling_quantizes_and_reproduces_anchor_values_in_both_directions() {
        let ticks = interval_ticks(0, 960, Interval::Ticks(120), &meter(), 1000).unwrap();
        assert_eq!(ticks, vec![0, 120, 240, 360, 480, 600, 720, 840, 960]);
        for shape in [Shape::Line, Shape::Gentle, Shape::Fast, Shape::SCurve] {
            let s = spec(shape);
            let v = s.sample(&ticks).unwrap();
            assert_eq!(v.first(), Some(&(0, 0)));
            assert_eq!(v.last(), Some(&(960, 127)));
            // Reverse direction (value falling).
            let mut r = spec(shape);
            r.from = (0, 127.0);
            r.to = (960, 0.0);
            let v = r.sample(&ticks).unwrap();
            assert_eq!((v[0].1, v[8].1), (127, 0));
            // Anchors given right to left: the curve starts at the start anchor,
            // so symmetric shapes give the same events, Gentle/Fast mirror.
            let mut back = spec(shape);
            back.from = (960, 127.0);
            back.to = (0, 0.0);
            let got = back.sample(&ticks).unwrap();
            assert_eq!((got[0].1, got[8].1), (0, 127));
            if matches!(shape, Shape::Line | Shape::SCurve) {
                assert_eq!(got, spec(shape).sample(&ticks).unwrap());
            } else {
                let mut mirrored = spec(shape);
                mirrored.bend = Bend::EaseIn;
                assert_eq!(got, mirrored.sample(&ticks).unwrap());
            }
        }
        let line = spec(Shape::Line).sample(&ticks).unwrap();
        assert_eq!(line[4], (480, 64)); // 63.5 rounds half away from zero
        // The end tick is always included, even off the grid.
        assert_eq!(
            interval_ticks(0, 1000, Interval::Ticks(480), &meter(), 100).unwrap(),
            vec![0, 480, 960, 1000]
        );
        assert_eq!(
            interval_ticks(5, 5, Interval::Ticks(480), &meter(), 100).unwrap(),
            vec![5]
        );
    }

    #[test]
    fn random_is_deterministic_per_seed_and_stays_in_range() {
        let ticks = interval_ticks(0, 96_000, Interval::Ticks(30), &meter(), 10_000).unwrap();
        let a = spec(Shape::Random).sample(&ticks).unwrap();
        let b = spec(Shape::Random).sample(&ticks).unwrap();
        assert_eq!(a, b, "same seed, same values");
        let mut other = spec(Shape::Random);
        other.seed = 8;
        assert_ne!(other.sample(&ticks).unwrap(), a);
        assert!(a.iter().all(|&(_, v)| (10..=20).contains(&v)));
        // Every value of the inclusive range occurs.
        for v in 10..=20 {
            assert!(a.iter().any(|p| p.1 == v), "{v} never drawn");
        }
        assert_eq!(
            splitmix64(0),
            0xE220_A839_7B1D_CDAF,
            "SplitMix64 reference value"
        );
    }

    #[test]
    fn custom_functions_use_the_real_evaluator_and_refuse_bad_samples() {
        let ticks = interval_ticks(0, 960, Interval::Ticks(240), &meter(), 100).unwrap();
        let e = Expr::parse("t*t").unwrap();
        let mut s = spec(Shape::Custom);
        s.custom = Some(&e);
        let v = s.sample(&ticks).unwrap();
        assert_eq!(v, vec![(0, 0), (240, 8), (480, 32), (720, 71), (960, 127)]);
        // The preview comes from the same evaluator.
        let p = s.preview(4).unwrap();
        assert_eq!(p[2], (0.5, 31.75));
        // Out of range: refused, or clamped when asked.
        let big = Expr::parse("2*t").unwrap();
        s.custom = Some(&big);
        assert!(matches!(s.sample(&ticks), Err(EditError::Curve(_))));
        s.clamp = true;
        assert_eq!(s.sample(&ticks).unwrap()[4], (960, 127));
        // Normalize Endpoints maps f(0) → start value and f(1) → end value.
        let off = Expr::parse("t + 5").unwrap();
        s.custom = Some(&off);
        s.clamp = false;
        s.normalize = true;
        let v = s.sample(&ticks).unwrap();
        assert_eq!((v[0].1, v[4].1), (0, 127));
        // A formula that fails somewhere fails as a whole (nothing partial).
        let bad = Expr::parse("1/(t-0.5)").unwrap();
        s.custom = Some(&bad);
        s.normalize = false;
        assert!(s.sample(&ticks).is_err());
        let flat = Expr::parse("3").unwrap();
        s.custom = Some(&flat);
        s.normalize = true;
        assert!(s.sample(&ticks).is_err());
    }

    #[test]
    fn budgets_are_checked_before_generating() {
        assert!(matches!(
            interval_ticks(0, 10_000_000_000, Interval::Ticks(1), &meter(), 1000),
            Err(EditError::TooManyEvents { .. })
        ));
        assert!(matches!(
            freehand(&[(0, 1.0), (10_000_000, 2.0)], 1, (0, 127), 1000),
            Err(EditError::TooManyEvents { .. })
        ));
        assert!(event_budget(64 << 20) >= 1000);
        assert!(event_budget(usize::MAX) <= 4_000_000);
    }

    #[test]
    fn beats_and_measures_follow_the_meter() {
        let m = MeterMap::new(Division::ppqn(480), &[(0, 4, 2), (1920, 3, 3)]);
        assert_eq!(
            interval_ticks(0, 3000, Interval::Measure, &m, 100).unwrap(),
            vec![0, 1920, 2640, 3000]
        );
        assert_eq!(
            interval_ticks(1440, 2400, Interval::Beat, &m, 100).unwrap(),
            vec![1440, 1920, 2160, 2400]
        );
    }

    #[test]
    fn freehand_fills_gaps_and_later_passes_win() {
        // Fast drag from (0, 0) to (960, 96): every 1/8 filled in.
        let v = freehand(&[(0, 0.0), (960, 96.0)], 240, (0, 127), 100).unwrap();
        assert_eq!(v, vec![(0, 0), (240, 24), (480, 48), (720, 72), (960, 96)]);
        // Going back over a tick overwrites it.
        let v = freehand(&[(0, 10.0), (480, 10.0), (240, 100.0)], 240, (0, 127), 100).unwrap();
        assert_eq!(v, vec![(0, 10), (240, 100), (480, 10)]);
        assert!(freehand(&[], 240, (0, 127), 100).unwrap().is_empty());
        let v = freehand(&[(250, 500.0)], 240, (0, 127), 100).unwrap();
        assert_eq!(v, vec![(240, 127)]);
    }

    #[test]
    fn only_before_the_note_uses_an_explicit_scope() {
        let mut s = NoteStore::new();
        let recs: Vec<NoteRecord> = (0..10u64)
            .map(|i| NoteRecord {
                start_tick: i * 100,
                duration_ticks: 50,
                attrs: NoteAttrs::new(60, 100, (i % 2) as u8, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        s.insert_sorted_batch(&recs, false).unwrap();
        let (ranks, starts) = notes_in_scope(&s, 200, 600, false, None, 100).unwrap();
        assert_eq!(starts, vec![200, 300, 400, 500, 600]);
        assert_eq!(ranks.iter().collect::<Vec<_>>(), vec![2, 3, 4, 5, 6]);
        let (_, starts) = notes_in_scope(&s, 0, 1000, false, Some(1), 100).unwrap();
        assert_eq!(starts, vec![100, 300, 500, 700, 900]);
        s.select_in_region(300, 400, 0, 127);
        let (ranks, _) = notes_in_scope(&s, 0, 1000, true, None, 100).unwrap();
        assert_eq!(ranks.len(), s.selected_count());
        assert!(notes_in_scope(&s, 0, 1000, false, None, 3).is_err());
    }
}
