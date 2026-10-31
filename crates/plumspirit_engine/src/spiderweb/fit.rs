//! Curve fitting (port of the fitting part of Spiderweb `bezier.py`, Philip
//! Schneider's algorithm as Spiderweb implements it; MIT © Kanade Tachibana).
//! NumPy sums are pairwise (`np::pw_sum`), cumulative sums sequential.

use super::curves::anchor_count;
use super::np::{dist, hypot, interp, np_hypot, pw_sum};
use super::paths::P;

const FIT_SAMPLES: (f64, usize) = (300.0, 20_000);
const FIT_CORNER: f64 = 50.0 * std::f64::consts::PI / 180.0;

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}

fn unit_row(v: P) -> P {
    let n = np_hypot(v[0], v[1]); // np.hypot (_unit_rows)
    if n > 1e-12 {
        [v[0] / n, v[1] / n]
    } else {
        [0.0, 0.0]
    }
}

/// `bezier.resample`: n + 1 points evenly along the polyline.
pub fn resample(points: &[P], n: usize) -> Vec<P> {
    let mut lens = vec![0.0];
    for w in points.windows(2) {
        let l = lens[lens.len() - 1] + dist(w[0], w[1]);
        lens.push(l);
    }
    let total = lens[lens.len() - 1];
    if total == 0.0 {
        return vec![points[0], points[points.len() - 1]];
    }
    let mut out = Vec::with_capacity(n + 1);
    let mut j = 0;
    for i in 0..=n {
        let l = total * i as f64 / n as f64;
        while j < lens.len() - 2 && lens[j + 1] < l {
            j += 1;
        }
        let seg = lens[j + 1] - lens[j];
        let t = if seg == 0.0 { 0.0 } else { (l - lens[j]) / seg };
        let (a, b) = (points[j], points[j + 1]);
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    out
}

fn gaps(pts: &[P]) -> Vec<f64> {
    pts.windows(2)
        .map(|w| np_hypot(w[1][0] - w[0][0], w[1][1] - w[0][1]))
        .collect()
}

fn cum(pts: &[P]) -> Vec<f64> {
    let mut s = vec![0.0];
    let mut acc = 0.0;
    for g in gaps(pts) {
        acc += g;
        s.push(acc);
    }
    s
}

fn fill_in(pts: &[P], tol: f64) -> Vec<P> {
    let g = gaps(pts);
    let total = pw_sum(&g);
    let step = (tol * 2.0)
        .min(total / FIT_SAMPLES.0)
        .max(total / FIT_SAMPLES.1 as f64);
    let cuts: Vec<usize> = g
        .iter()
        .map(|x| ((x / step).ceil() as i64).max(1) as usize)
        .collect();
    if cuts.iter().sum::<usize>() + 1 > FIT_SAMPLES.1 {
        return resample(pts, FIT_SAMPLES.1);
    }
    let mut out = Vec::new();
    for (i, &c) in cuts.iter().enumerate() {
        let (a, b) = (pts[i], pts[i + 1]);
        for k in 0..c {
            let t = k as f64 / c as f64;
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

fn directions(pts: &[P], w: f64) -> (Vec<P>, Vec<P>) {
    let s = cum(pts);
    let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p[1]).collect();
    let last = s[s.len() - 1];
    let along = |i: usize, d: f64| -> P {
        let t = (s[i] + d).clamp(0.0, last);
        [interp(t, &s, &xs), interp(t, &s, &ys)]
    };
    let n = pts.len();
    let mut come: Vec<P> = (0..n)
        .map(|i| unit_row(sub(pts[i], along(i, -w))))
        .collect();
    let mut go: Vec<P> = (0..n).map(|i| unit_row(sub(along(i, w), pts[i]))).collect();
    come[0] = go[0];
    go[n - 1] = come[n - 1];
    (come, go)
}

fn corners(come: &[P], go: &[P], pts: &[P], w: f64) -> Vec<usize> {
    let n = pts.len();
    let turn: Vec<f64> = (0..n)
        .map(|i| {
            let d = come[i][0] * go[i][0] + come[i][1] * go[i][1];
            d.clamp(-1.0, 1.0).acos()
        })
        .collect();
    let s = cum(pts);
    let mut order: Vec<usize> = (0..n).collect();
    // np.argsort(-turn) (ties: lower index first).
    order.sort_by(|&a, &b| {
        (-turn[a])
            .partial_cmp(&-turn[b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let mut out: Vec<usize> = Vec::new();
    for i in order {
        if turn[i] <= FIT_CORNER {
            break;
        }
        if 0 < i && i < n - 1 && out.iter().all(|&j| (s[i] - s[j]).abs() > w) {
            out.push(i);
        }
    }
    out.sort_unstable();
    out
}

#[inline]
fn basis(u: f64) -> [f64; 4] {
    let mt = 1.0 - u;
    [
        mt.powf(3.0),
        3.0 * mt * mt * u,
        3.0 * mt * u * u,
        u.powf(3.0),
    ]
}

#[inline]
fn at(bez: &[P; 4], u: f64) -> P {
    let b = basis(u);
    [
        b[0] * bez[0][0] + b[1] * bez[1][0] + b[2] * bez[2][0] + b[3] * bez[3][0],
        b[0] * bez[0][1] + b[1] * bez[1][1] + b[2] * bez[2][1] + b[3] * bez[3][1],
    ]
}

fn chord_params(pts: &[P]) -> Vec<f64> {
    let s = cum(pts);
    let l = s[s.len() - 1];
    s.iter().map(|v| v / l).collect()
}

fn generate(pts: &[P], u: &[f64], tl: P, tr: P) -> [P; 4] {
    let (p0, p3) = (pts[0], pts[pts.len() - 1]);
    let n = pts.len();
    let mut a00 = Vec::with_capacity(2 * n);
    let mut a01 = Vec::with_capacity(2 * n);
    let mut a11 = Vec::with_capacity(2 * n);
    let mut x0v = Vec::with_capacity(2 * n);
    let mut x1v = Vec::with_capacity(2 * n);
    for (i, &ui) in u.iter().enumerate() {
        let b = basis(ui);
        let a0 = [b[1] * tl[0], b[1] * tl[1]];
        let a1 = [b[2] * tr[0], b[2] * tr[1]];
        let c0 = b[0] + b[1];
        let c3 = b[2] + b[3];
        let tmp = [
            pts[i][0] - (c0 * p0[0] + c3 * p3[0]),
            pts[i][1] - (c0 * p0[1] + c3 * p3[1]),
        ];
        for k in 0..2 {
            a00.push(a0[k] * a0[k]);
            a01.push(a0[k] * a1[k]);
            a11.push(a1[k] * a1[k]);
            x0v.push(a0[k] * tmp[k]);
            x1v.push(a1[k] * tmp[k]);
        }
    }
    let (c00, c01, c11) = (pw_sum(&a00), pw_sum(&a01), pw_sum(&a11));
    let (x0, x1) = (pw_sum(&x0v), pw_sum(&x1v));
    let det = c00 * c11 - c01 * c01;
    let length = dist(p0, p3);
    let path = pw_sum(&gaps(pts));
    let (mut al, mut ar) = (0.0, 0.0);
    if det.abs() > 1e-12 {
        al = (x0 * c11 - x1 * c01) / det;
        ar = (c00 * x1 - c01 * x0) / det;
    }
    if al < 1e-6 * length || ar < 1e-6 * length || al.max(ar) > path {
        let v = length.max(path / 2.0) / 3.0;
        al = v;
        ar = v;
    }
    [
        p0,
        [p0[0] + tl[0] * al, p0[1] + tl[1] * al],
        [p3[0] + tr[0] * ar, p3[1] + tr[1] * ar],
        p3,
    ]
}

fn max_error(pts: &[P], bez: &[P; 4], u: &[f64]) -> f64 {
    // d[1:-1].max() (argmax: the first largest)
    let mut best: Option<f64> = None;
    for i in 1..pts.len() - 1 {
        let q = at(bez, u[i]);
        let d = np_hypot(q[0] - pts[i][0], q[1] - pts[i][1]);
        if best.is_none_or(|b| d > b) {
            best = Some(d);
        }
    }
    best.unwrap_or(0.0)
}

fn newton(bez: &[P; 4], pts: &[P], u: &[f64]) -> Vec<f64> {
    let [p0, p1, p2, p3] = *bez;
    u.iter()
        .zip(pts)
        .map(|(&ui, p)| {
            let c = at(bez, ui);
            let q = [c[0] - p[0], c[1] - p[1]];
            let mt = 1.0 - ui;
            let t = ui;
            let mut d1 = [0.0; 2];
            let mut d2 = [0.0; 2];
            for k in 0..2 {
                d1[k] = 3.0
                    * (mt * mt * (p1[k] - p0[k])
                        + 2.0 * mt * t * (p2[k] - p1[k])
                        + t * t * (p3[k] - p2[k]));
                d2[k] =
                    6.0 * (mt * (p2[k] - 2.0 * p1[k] + p0[k]) + t * (p3[k] - 2.0 * p2[k] + p1[k]));
            }
            let num = q[0] * d1[0] + q[1] * d1[1];
            let den = (d1[0] * d1[0] + d1[1] * d1[1]) + (q[0] * d2[0] + q[1] * d2[1]);
            let v = if den == 0.0 { ui } else { ui - num / den };
            v.clamp(0.0, 1.0)
        })
        .collect()
}

fn fit_one(part: &[P], tl: P, tr: P, tol: f64) -> ([P; 4], f64) {
    let mut u = chord_params(part);
    let mut bez = generate(part, &u, tl, tr);
    let mut err = max_error(part, &bez, &u);
    if tol < err && err < tol * 16.0 {
        for _ in 0..8 {
            u = newton(&bez, part, &u);
            bez = generate(part, &u, tl, tr);
            let before = err;
            err = max_error(part, &bez, &u);
            if err <= tol || err > before * 0.9 {
                break;
            }
        }
    }
    (bez, err)
}

/// A curve (flat Bézier list) through the first and last point within `tol`
/// of all points, with as few anchors as it can. Sharp corners' anchor numbers
/// are added to `sharp`.
pub fn fit(
    points: &[P],
    tol: f64,
    mut sharp: Option<&mut Vec<usize>>,
    end_dir: Option<P>,
) -> Vec<P> {
    let mut pts: Vec<P> = vec![points[0]];
    for &p in &points[1..] {
        if dist(p, pts[pts.len() - 1]) > 1e-9 {
            pts.push(p);
        }
    }
    if pts.len() < 2 {
        let p = pts[0];
        return vec![p, p, p, p];
    }
    let pts = fill_in(&pts, tol);
    let w = tol * 4.0;
    let (mut come, go) = directions(&pts, w);
    let n = pts.len();
    if let Some(d) = end_dir {
        let h = hypot(d[0], d[1]);
        if h > 1e-12 {
            come[n - 1] = [d[0] / h, d[1] / h];
        }
    }
    let cs = corners(&come, &go, &pts, w);
    let mut ends = vec![0usize];
    ends.extend(&cs);
    ends.push(n - 1);
    let smooth: Vec<P> = come
        .iter()
        .zip(&go)
        .map(|(c, g)| unit_row([c[0] + g[0], c[1] + g[1]]))
        .collect();
    let mut out: Vec<P> = vec![pts[0]];
    let mut reach = 4usize;
    for e in ends.windows(2) {
        let (first, last) = (e[0], e[1]);
        let mut a = first;
        while a < last {
            let tl = if a == first { go[a] } else { smooth[a] };
            let attempt = |b: usize| -> ([P; 4], f64) {
                let r = if b == last { come[b] } else { smooth[b] };
                let tr = [-r[0], -r[1]];
                if b - a == 1 {
                    let d = dist(pts[a], pts[b]) / 3.0;
                    return (
                        [
                            pts[a],
                            [pts[a][0] + tl[0] * d, pts[a][1] + tl[1] * d],
                            [pts[b][0] + tr[0] * d, pts[b][1] + tr[1] * d],
                            pts[b],
                        ],
                        0.0,
                    );
                }
                fit_one(&pts[a..=b], tl, tr, tol)
            };
            let mut good = a + 1;
            let mut got: Option<[P; 4]> = None;
            let mut bad: Option<usize> = None;
            let mut b = (a + reach).min(last);
            loop {
                let piece = attempt(b);
                if piece.1 <= tol {
                    good = b;
                    got = Some(piece.0);
                    if b == last || bad.is_some() {
                        break;
                    }
                    b = (a + (b - a) * 2).min(last);
                } else {
                    bad = Some(b);
                    if b - a <= 1 || good > a + 1 {
                        break;
                    }
                    b = a + ((b - a) / 2).max(1);
                    if b == a + 1 {
                        break;
                    }
                }
            }
            let mut got = got.unwrap_or_else(|| attempt(a + 1).0);
            while let Some(bd) = bad {
                if bd - good <= ((good - a) / 10).max(1) {
                    break;
                }
                let b = (good + bd) / 2;
                let piece = attempt(b);
                if piece.1 <= tol {
                    good = b;
                    got = piece.0;
                } else {
                    bad = Some(b);
                }
            }
            out.push(got[1]);
            out.push(got[2]);
            out.push(pts[good]);
            reach = (good - a).max(2);
            a = good;
        }
        if last != n - 1
            && let Some(s) = sharp.as_deref_mut()
        {
            s.push(anchor_count(out.len()) - 1);
        }
    }
    out
}
