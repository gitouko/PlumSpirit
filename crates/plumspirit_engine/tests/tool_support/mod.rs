//! Shared helpers of the tool test suites (`v07_tools`, `v09_tool_definitions`):
//! schema-driven random parameter values and the pointer stroke the stroke
//! tools (Draw, Paint, Select lassos) read. One copy, used by both.

#![allow(dead_code)]

use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::transform::param::{ParamKind, ParamSpec, ParamValue};
use plumspirit_engine::transform::rng::Rng;
use plumspirit_engine::transform::{Curve, Outside, Stroke, StrokePoint};

/// A random value for a spec in a project of key range `keys` (moderate
/// magnitudes so most runs fit; keyed ranges reach 255 in 256-Key Mode).
/// `None` for kinds without a meaningful random value (free text).
pub fn random_value(spec: &ParamSpec, r: &mut Rng, keys: KeyRange) -> Option<ParamValue> {
    let range = spec.range_in(keys);
    Some(match &spec.kind {
        ParamKind::Int { .. } => {
            let (min, max) = range.unwrap();
            let (lo, hi) = (min.max(-200), max.min(200));
            if lo > hi {
                return None;
            }
            ParamValue::Int(r.range_i64(lo, hi))
        }
        ParamKind::Float { min, max, .. } => ParamValue::Float(r.range_f64(*min, *max)),
        ParamKind::Bool => ParamValue::Bool(r.chance(0.5)),
        ParamKind::Choice(o) => ParamValue::Choice(r.range_i64(0, o.len() as i64 - 1) as u32),
        ParamKind::Ticks { min, max } => {
            let hi = (*max).min(3840).max(*min);
            ParamValue::Int(r.range_i64(*min as i64, hi as i64))
        }
        ParamKind::Pitch => {
            let (lo, hi) = range.unwrap();
            ParamValue::Int(r.range_i64(lo, hi))
        }
        ParamKind::Seed => ParamValue::Int(r.range_i64(0, 1 << 30)),
        ParamKind::List { max_len, .. } => {
            let (min, max) = range.unwrap();
            let n = r.range_i64(1, (*max_len as i64).min(6));
            let (lo, hi) = (min.max(-24), max.min(24));
            if lo > hi {
                return None;
            }
            ParamValue::List((0..n).map(|_| r.range_i64(lo, hi)).collect())
        }
        ParamKind::Curve => {
            // B1-0112 / B1-0113: a third of the draws overshoot (y beyond 0 – 1;
            // parameters that do not allow it read the curve pinned) and
            // give some points their own smoothness.
            let over = [0.0, 0.0, 0.0, 0.0, 0.5, 2.0][r.range_i64(0, 5) as usize];
            let y = |r: &mut Rng| -over + r.unit() * (1.0 + 2.0 * over);
            let mut pts: Vec<(f64, f64)> =
                (0..r.range_i64(2, 5)).map(|_| (r.unit(), y(r))).collect();
            // v0.9 §104: ends are free, so half the draws leave them inside.
            if r.chance(0.5) {
                pts.push((0.0, y(r)));
                pts.push((1.0, y(r)));
            }
            let n = pts.len();
            let mut c = Curve::new(pts)
                .with_smoothness([0.0, 0.5, 1.0][r.range_i64(0, 2) as usize])
                .with_outside(Outside::ALL[r.range_i64(0, 3) as usize])
                .with_overshoot(over);
            if over > 0.0 {
                for i in 0..n {
                    if r.chance(0.3) {
                        c = c.with_local(i, Some([0.0, 0.5, 1.0][r.range_i64(0, 2) as usize]));
                    }
                }
            }
            ParamValue::Curve(c.sanitized())
        }
        // B1-S35: Note Properties' expression fields get a formula (some read
        // the seeded rnd, some leave the field empty); other text keeps its
        // default.
        ParamKind::Text { .. } if spec.key.starts_with("n12.") && spec.key != "n12.consts" => {
            const FORMULAS: [&str; 8] = [
                "",
                "round(rnd * 127)",
                "k + round(rnd * 6) - 3",
                "v * (0.5 + rnd)",
                "index % 16 + 1",
                "g + floor(rnd * 120)",
                "(t + index * 7) % 3840",
                "math.max(1, v - 10)",
            ];
            ParamValue::Text(FORMULAS[r.range_i64(0, 7) as usize].to_owned())
        }
        // B1-S36: Note Properties' range rows — up to three random rows.
        ParamKind::Text { .. } if spec.key == "n13.rows" => {
            use plumspirit_engine::editor::ranges::{self, Across, Blend, RangeRow, Target};
            let rows: Vec<RangeRow> = (0..r.range_i64(0, 3))
                .map(|_| {
                    let mut t = Target::ALL[r.range_i64(0, 7) as usize];
                    if let Target::Controller(_) = t {
                        t = Target::Controller(r.range_i64(0, 127) as u8);
                    }
                    let (a, b) = (r.unit(), r.unit());
                    let (f, to) = t.defaults();
                    RangeRow {
                        on: r.chance(0.9),
                        target: t,
                        across: Across::ALL[r.range_i64(0, 3) as usize],
                        span: (a.min(b), a.max(b)),
                        from: f + r.range_i64(-8, 8),
                        to: to + r.range_i64(-8, 8),
                        blend: Blend::ALL[r.range_i64(0, 4) as usize],
                        graph: Curve::new(vec![(0.0, r.unit()), (1.0, r.unit())]),
                    }
                })
                .collect();
            ParamValue::Text(ranges::to_text(&rows))
        }
        ParamKind::Text { .. } => return None,
    })
}

/// A pointer stroke for the stroke tools: a closed zig-zag around key 64
/// (moved up by `shift` semitones) over ticks 0–15,600, drawn at varying
/// speed.
pub fn stroke(shift: u8) -> Stroke {
    let mut points = Vec::new();
    for k in 0..40u64 {
        let x = (k as f64 / 39.0 * std::f64::consts::TAU).sin();
        points.push(StrokePoint {
            tick: k * 400,
            pitch: 64.0 + f64::from(shift) + 14.0 * x,
            t: (k * k) as f64 * 0.004,
        });
    }
    points.push(StrokePoint {
        tick: 0,
        pitch: 40.0 + f64::from(shift),
        t: 7.0,
    });
    Stroke { points }
}
