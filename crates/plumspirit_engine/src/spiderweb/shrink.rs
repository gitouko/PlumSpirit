//! The outline gate's even band (shrink.py): the shape shrunk inward, a
//! smaller copy of its own outline, worked out exactly on `SAMPLES` lines
//! across each key row. (The piano roll's preview line, `inner_lines`, is
//! drawn by the app.)

use super::areas::{npmax, npmin};
use super::paths::P;
use std::cmp::Ordering;

pub const SAMPLES: usize = 8;

/// An edge `(ax, ay, bx, by)`.
pub type Seg = [f64; 4];

/// Python float ordering for sorting (NaN last, like NumPy; −0 == 0).
#[inline]
pub fn fcmp(a: f64, b: f64) -> Ordering {
    match a.partial_cmp(&b) {
        Some(o) => o,
        None => a.is_nan().cmp(&b.is_nan()),
    }
}

/// The loops' edges.
pub fn segments(polys: &[Vec<P>]) -> Vec<Seg> {
    let mut out = Vec::new();
    for p in polys {
        if p.len() > 1 {
            for w in p.windows(2) {
                out.push([w[0][0], w[0][1], w[1][0], w[1][1]]);
            }
        }
    }
    out
}

/// Beats per key that make the shape as wide as it's tall (1 when flat).
pub fn proportion(polys: &[Vec<P>]) -> f64 {
    let (mut xl, mut xh, mut yl, mut yh) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for p in polys {
        for q in p {
            xl = npmin(xl, q[0]);
            xh = npmax(xh, q[0]);
            yl = npmin(yl, q[1]);
            yh = npmax(yh, q[1]);
        }
    }
    let (w, h) = (xh - xl, yh - yl);
    if w > 0.0 && h > 0.0 { w / h } else { 1.0 }
}

/// `seg * [1, k, 1, k]`.
pub fn scaled(seg: &[Seg], k: f64) -> Vec<Seg> {
    seg.iter()
        .map(|s| [s[0], s[1] * k, s[2], s[3] * k])
        .collect()
}

/// Where the line at height y is inside (even-odd within each group, any
/// group counts), merged.
pub fn inside_at(groups: &[Vec<Seg>], y: f64) -> Vec<[f64; 2]> {
    let mut spans = Vec::new();
    for seg in groups {
        let mut x: Vec<f64> = seg
            .iter()
            .filter(|s| (s[1] <= y) != (s[3] <= y))
            .map(|s| s[0] + (s[2] - s[0]) * (y - s[1]) / (s[3] - s[1]))
            .collect();
        if x.is_empty() {
            continue;
        }
        x.sort_by(|a, b| fcmp(*a, *b));
        for c in x.chunks_exact(2) {
            spans.push([c[0], c[1]]);
        }
    }
    merge(spans)
}

/// Spans sorted, touching / overlapping ones joined.
pub fn merge(mut spans: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    spans.sort_by(|a, b| fcmp(a[0], b[0]).then(fcmp(a[1], b[1])));
    let mut out: Vec<[f64; 2]> = Vec::new();
    for [a, b] in spans {
        if let Some(l) = out.last_mut()
            && a <= l[1]
        {
            l[1] = if b > l[1] { b } else { l[1] };
            continue;
        }
        out.push([a, b]);
    }
    out
}

/// For each height in `ys` (ascending, in the shape's proportions): the
/// stretches of the line closer than r to an edge, merged.
pub fn near_rows(seg: &[Seg], ys: &[f64], r: f64) -> Vec<Vec<[f64; 2]>> {
    let mut out: Vec<Vec<[f64; 2]>> = vec![Vec::new(); ys.len()];
    if seg.is_empty() || ys.is_empty() {
        return out;
    }
    let ss = |v: f64, right: bool| -> usize {
        if right {
            ys.partition_point(|y| fcmp(*y, v) != Ordering::Greater)
        } else {
            ys.partition_point(|y| fcmp(*y, v) == Ordering::Less)
        }
    };
    let mut hits: Vec<(usize, f64, f64)> = Vec::new();
    for s in seg {
        let (ax, ay, bx, by) = (s[0], s[1], s[2], s[3]);
        let s_lo = npmin(ay, by) - r;
        let s_hi = npmax(ay, by) + r;
        let first = ss(s_lo, true);
        let last = ss(s_hi, false);
        if last <= first {
            continue;
        }
        let dx = bx - ax;
        let dy = by - ay;
        let length = dx.hypot(dy);
        for (row, &y) in ys.iter().enumerate().take(last).skip(first) {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for (px, py) in [(ax, ay), (bx, by)] {
                let t = y - py;
                let d = r * r - t * t;
                let w = npmax(d, 0.0).sqrt();
                if d > 0.0 {
                    lo = npmin(lo, px - w);
                    hi = npmax(hi, px + w);
                }
            }
            let flat = dy.abs() < 1e-12;
            let c = ax + dx * (y - ay) / dy;
            let w = r * length / dy.abs();
            let (mut b_lo, mut b_hi) = if flat {
                (npmin(ax, bx), npmax(ax, bx))
            } else {
                (c - w, c + w)
            };
            let mut ok = if flat { (y - ay).abs() < r } else { true };
            let upright = dx.abs() < 1e-12;
            let t = (y - ay) * dy / if length > 0.0 { length * length } else { 1.0 };
            let x0 = ax - (y - ay) * dy / dx;
            let x1 = x0 + length * length / dx;
            let p_lo = if upright {
                f64::NEG_INFINITY
            } else {
                npmin(x0, x1)
            };
            let p_hi = if upright {
                f64::INFINITY
            } else {
                npmax(x0, x1)
            };
            ok &= if upright {
                (0.0..=1.0).contains(&t)
            } else {
                true
            };
            b_lo = npmax(b_lo, p_lo);
            b_hi = npmin(b_hi, p_hi);
            ok &= b_hi > b_lo && length > 0.0;
            if ok {
                lo = npmin(lo, b_lo);
                hi = npmax(hi, b_hi);
            }
            if hi > lo {
                hits.push((row, lo, hi));
            }
        }
    }
    // lexsort((hi, lo, row)): stable
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(fcmp(a.1, b.1)).then(fcmp(a.2, b.2)));
    for (k, a, b) in hits {
        let o = &mut out[k];
        if let Some(l) = o.last_mut()
            && a <= l[1]
        {
            l[1] = if b > l[1] { b } else { l[1] };
            continue;
        }
        o.push([a, b]);
    }
    out
}

/// Spans with the cuts taken out (cuts sorted).
pub fn minus(spans: &[[f64; 2]], cuts: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    for &[a0, b] in spans {
        let mut a = a0;
        for &[c, d] in cuts {
            if d <= a || c >= b {
                continue;
            }
            if c > a {
                out.push([a, c]);
            }
            a = if d > a { d } else { a };
            if a >= b {
                break;
            }
        }
        if a < b {
            out.push([a, b]);
        }
    }
    out
}

/// The shrunk inside on each of these key rows as (start, end, key) ticks;
/// r in beats.
pub fn inner_rows(polys: &[Vec<P>], union: bool, keys: &[i64], r: f64, ppq: u32) -> Vec<[i64; 3]> {
    let k = proportion(polys);
    let seg = scaled(&segments(polys), k);
    let groups: Vec<Vec<Seg>> = if union {
        polys
            .iter()
            .map(|p| scaled(&segments(std::slice::from_ref(p)), k))
            .collect()
    } else {
        vec![seg.clone()]
    };
    let ys = sample_heights(keys, k);
    let near = near_rows(&seg, &ys, r);
    let ppq = f64::from(ppq);
    let mut out = Vec::new();
    for (n, &q) in keys.iter().enumerate() {
        let mut got = Vec::new();
        for j in 0..SAMPLES {
            let y = ys[n * SAMPLES + j];
            got.extend(minus(&inside_at(&groups, y), &near[n * SAMPLES + j]));
        }
        for [a, b] in merge(got) {
            let s = (a * ppq + 0.5).floor() as i64;
            let e = (b * ppq + 0.5).floor() as i64;
            if e > s {
                out.push([s, e, q]);
            }
        }
    }
    out
}

/// `[(q - 0.5 + (j + 0.5) / SAMPLES) * k for q in keys for j in range(SAMPLES)]`.
pub fn sample_heights(keys: &[i64], k: f64) -> Vec<f64> {
    let mut ys = Vec::with_capacity(keys.len() * SAMPLES);
    for &q in keys {
        for j in 0..SAMPLES {
            ys.push((q as f64 - 0.5 + (j as f64 + 0.5) / SAMPLES as f64) * k);
        }
    }
    ys
}
