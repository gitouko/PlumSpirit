//! Polygons and stars (polygon.py): a custom shape made by the Polygon tool
//! keeps its settings in `sh["polygon"]` and its strokes are made from them.

use super::np::pyround;
use super::paths::P;
use super::{fnum, inum, pattern, pts_value};
use crate::json::Value;
use std::f64::consts::PI;

pub const STYLES: [&str; 3] = ["polygon", "star", "cross"];
pub const MAX_POINTS: i64 = 200;

/// Polygon settings from a file made valid, or None.
pub fn clean_polygon(d: Option<&Value>) -> Option<Value> {
    let d = d?;
    if !matches!(d, Value::Obj(_)) {
        return None;
    }
    let points = match d.get("points") {
        None => 5,
        Some(v) => inum(v)?,
    }
    .clamp(3, MAX_POINTS);
    let style = match d.get("style").and_then(Value::as_str) {
        Some(s) if STYLES.contains(&s) => s,
        _ => "polygon",
    };
    let inner = match d.get("inner") {
        None => 50.0,
        Some(v) => fnum(v)?,
    };
    // max(0.0, min(1000.0, inner)) with Python's NaN behaviour
    let inner = if inner < 1000.0 { inner } else { 1000.0 };
    let inner = if inner > 0.0 { inner } else { 0.0 };
    let skip = match d.get("skip") {
        None => 2,
        Some(v) => inum(v)?,
    }
    .max(1);
    let mut out = Value::obj();
    out.set("points", Value::Num(points as f64));
    out.set("style", Value::Str(style.to_owned()));
    out.set("inner", Value::Num(inner));
    out.set("skip", Value::Num(skip as f64));
    if let Some(p) = pattern::clean_pattern(d.get("pattern")) {
        out.set("pattern", p);
    }
    if let Some(f) = pattern::clean_shape_formula(d.get("shape")) {
        out.set("shape", f);
    }
    Some(out)
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The outline(s) as closed point lists round the unit circle, clockwise.
pub fn polygon_loops(d: &Value) -> Vec<Vec<P>> {
    let n = d.get("points").and_then(inum).unwrap_or(5);
    let nf = n as f64;
    match d.get("style").and_then(Value::as_str).unwrap_or("polygon") {
        "star" => {
            let r = d.get("inner").and_then(fnum).unwrap_or(50.0) / 100.0;
            let mut pts = Vec::new();
            for i in 0..n {
                for (j, rad) in [(0.0, 1.0), (0.5, r)] {
                    let a = PI / 2.0 - 2.0 * PI * (i as f64 + j) / nf;
                    pts.push([rad * a.cos(), rad * a.sin()]);
                }
            }
            pts.push(pts[0]);
            vec![pts]
        }
        "cross" => {
            let skip = d.get("skip").and_then(inum).unwrap_or(2);
            let k = match skip.rem_euclid(n) {
                0 => 1,
                v => v,
            };
            let corners: Vec<P> = (0..n)
                .map(|i| {
                    let a = PI / 2.0 - 2.0 * PI * i as f64 / nf;
                    [a.cos(), a.sin()]
                })
                .collect();
            let g = gcd(n, k);
            let mut loops = Vec::new();
            for first in 0..g {
                let mut lp: Vec<P> = (0..n / g)
                    .map(|j| corners[((first + j * k) % n) as usize])
                    .collect();
                lp.push(lp[0]);
                loops.push(lp);
            }
            loops
        }
        _ => {
            let start = -PI / 2.0 - PI / nf;
            let mut pts: Vec<P> = (0..n)
                .map(|i| {
                    let a = start - 2.0 * PI * i as f64 / nf;
                    [a.cos(), a.sin()]
                })
                .collect();
            pts.push(pts[0]);
            vec![pts]
        }
    }
}

/// The custom shape strokes for polygon settings d.
pub fn polygon_strokes(d: &Value) -> Vec<Value> {
    let loops = polygon_loops(d);
    let pts: Vec<P> = loops.iter().flatten().copied().collect();
    let x0 = pts
        .iter()
        .map(|p| p[0])
        .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
    let y0 = pts
        .iter()
        .map(|p| p[1])
        .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
    let x1 = pts
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
    let y1 = pts
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
    let w = (x1 - x0).max(1e-9);
    let h = (y1 - y0).max(1e-9);
    let mut out = Vec::new();
    for lp in loops {
        let mut q: Vec<P> = lp
            .iter()
            .map(|p| [pyround((p[0] - x0) / w, 9), pyround((p[1] - y0) / h, 9)])
            .collect();
        let last = q.len() - 1;
        q[last] = q[0];
        let mut st = Value::obj();
        st.set("kind", Value::Str("poly".into()));
        st.set("pts", pts_value(&q));
        st.set("sides", Value::Bool(true));
        for layer in ["shape", "pattern"] {
            if let Some(v) = d.get(layer).filter(|v| v.truthy()) {
                st.set(layer, v.clone());
            }
        }
        out.push(st);
    }
    out
}

/// A polygon stroke's sides, each `[start, end]`.
pub fn side_paths(pts: &[P]) -> Vec<Vec<P>> {
    pts.windows(2).map(|w| vec![w[0], w[1]]).collect()
}
