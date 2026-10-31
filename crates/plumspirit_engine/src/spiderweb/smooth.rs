//! Freehand strokes made perfect (port of Spiderweb `smooth.py`, MIT © Kanade
//! Tachibana): a sensitivity 0–100 turns a wobbly stroke into straight lines
//! and smooth curves, and a loop into a circle / ellipse / square / rectangle /
//! polygon. Python sums are compensated (`np::py_sum`).

use super::curves::{rne, seg_point};
use super::fit::fit;
use super::np::{dist, hypot, py_sum};
use super::paths::P;
use std::f64::consts::PI;

const CORNER: f64 = 40.0 * PI / 180.0;
const MAX_POLYGON: usize = 6;
const LOOP: f64 = 0.1;
const SHAPE_SAMPLES: usize = 120;
const PIECE_SAMPLES: (usize, usize) = (600, 6000);
const CORNER_COST: f64 = 0.012;

pub fn tolerance(level: f64, size: f64) -> f64 {
    0.2 * size * (level.clamp(0.0, 100.0) / 100.0).powf(2.5)
}

/// A saved smoothing level (0–100; bad values → 0).
pub fn clean_level(v: Option<f64>) -> f64 {
    match v {
        Some(x) if x.is_finite() => rne(x).clamp(0.0, 100.0),
        _ => 0.0,
    }
}

/// The stroke made perfect at this sensitivity; 0 = as drawn.
pub fn smooth_path(pts: &[P], level: f64, k: f64) -> Vec<P> {
    if level <= 0.0 || pts.len() < 3 || k <= 0.0 {
        return pts.to_vec();
    }
    let q = dedupe(&pts.iter().map(|p| [p[0] / k, p[1]]).collect::<Vec<_>>());
    if q.len() < 3 {
        return pts.to_vec();
    }
    let (mut x0, mut x1, mut y0, mut y1) = (q[0][0], q[0][0], q[0][1], q[0][1]);
    for p in &q {
        x0 = x0.min(p[0]);
        x1 = x1.max(p[0]);
        y0 = y0.min(p[1]);
        y1 = y1.max(p[1]);
    }
    let size = hypot(x1 - x0, y1 - y0);
    if size < 1e-9 {
        return pts.to_vec();
    }
    let tol = tolerance(level, size);
    let gap = dist(q[0], q[q.len() - 1]);
    let mut out = None;
    if gap <= LOOP * size {
        out = shape_ladder(&q)
            .into_iter()
            .find(|(_, dev)| *dev <= tol)
            .map(|(s, _)| s);
    }
    let out = out.unwrap_or_else(|| pieces(&q, tol, gap <= tol));
    out.iter().map(|p| [p[0] * k, p[1]]).collect()
}

// ------------------------------------------------------------------ loops

#[derive(Clone, Copy, PartialEq)]
enum LoopKind {
    Round,
    Box,
    Poly(usize),
}

fn shape_ladder(q: &[P]) -> Vec<(Vec<P>, f64)> {
    let mut closed = q.to_vec();
    closed.push(q[0]);
    let r = resample(&closed, SHAPE_SAMPLES);
    let every: Vec<P> = r.iter().step_by(6).copied().collect();
    let mut size = f64::NEG_INFINITY;
    for a in &every {
        for b in &every {
            size = size.max(dist(*a, *b));
        }
    }
    let mut kinds: Vec<(LoopKind, f64, f64)> = Vec::new();
    let el = ellipse(&r);
    if let Some(e) = &el {
        kinds.push((LoopKind::Round, dev(&r, e), 5.0));
    }
    let bx = boxed(&r);
    if let Some(b) = &bx {
        kinds.push((LoopKind::Box, dev(&r, &box_points(b.0, b.1, b.2, b.3)), 5.0));
    }
    let mut polygons: Vec<(usize, Vec<P>)> = Vec::new();
    for n in 3..=MAX_POLYGON {
        if let Some(poly) = best_polygon(&r, n)
            && poly.len() - 1 == n
        {
            kinds.push((LoopKind::Poly(n), dev(&r, &poly), 2.0 * n as f64));
            polygons.push((n, poly));
        }
    }
    if kinds.is_empty() {
        return Vec::new();
    }
    let score = |k: &(LoopKind, f64, f64)| k.1 + CORNER_COST * size * k.2;
    let mut kind = kinds[0];
    for k in &kinds[1..] {
        if score(k) < score(&kind) {
            kind = *k;
        }
    }
    let poly = |n: usize| polygons.iter().find(|p| p.0 == n).map(|p| p.1.clone());
    let shapes: Vec<Option<Vec<P>>> = match kind.0 {
        LoopKind::Round => vec![Some(circle(&r)), el],
        LoopKind::Box | LoopKind::Poly(4) => vec![
            bx.map(|b| {
                let h = (b.2 + b.3) / 2.0;
                box_points(b.0, b.1, h, h)
            }),
            bx.map(|b| box_points(b.0, b.1, b.2, b.3)),
            poly(4),
        ],
        LoopKind::Poly(n) => vec![poly(n)],
    };
    shapes
        .into_iter()
        .flatten()
        .map(|s| {
            let d = dev(&r, &s);
            (s, d)
        })
        .collect()
}

fn mean(pts: &[P]) -> P {
    let n = pts.len() as f64;
    [
        py_sum(pts.iter().map(|p| p[0])) / n,
        py_sum(pts.iter().map(|p| p[1])) / n,
    ]
}

fn circle(r: &[P]) -> Vec<P> {
    let c = mean(r);
    let rad = py_sum(r.iter().map(|p| dist(*p, c))) / r.len() as f64;
    ellipse_points(c[0], c[1], rad, rad, 0.0)
}

fn ellipse(r: &[P]) -> Option<Vec<P>> {
    let [cx, cy] = mean(r);
    let n = r.len() as f64;
    let sxx = py_sum(r.iter().map(|p| (p[0] - cx) * (p[0] - cx))) / n;
    let syy = py_sum(r.iter().map(|p| (p[1] - cy) * (p[1] - cy))) / n;
    let sxy = py_sum(r.iter().map(|p| (p[0] - cx) * (p[1] - cy))) / n;
    let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let half = hypot((sxx - syy) / 2.0, sxy);
    let l1 = (sxx + syy) / 2.0 + half;
    let l2 = ((sxx + syy) / 2.0 - half).max(0.0);
    let (a, b) = ((2.0 * l1).sqrt(), (2.0 * l2).sqrt());
    if b < 1e-9 {
        return None;
    }
    let (ca, sa) = (angle.cos(), angle.sin());
    let rho = py_sum(r.iter().map(|p| {
        let (x, y) = (p[0], p[1]);
        hypot(
            ((x - cx) * ca + (y - cy) * sa) / a,
            (-(x - cx) * sa + (y - cy) * ca) / b,
        )
    })) / n;
    Some(ellipse_points(cx, cy, a * rho, b * rho, angle))
}

fn ellipse_points(cx: f64, cy: f64, a: f64, b: f64, angle: f64) -> Vec<P> {
    let n = 96;
    let (ca, sa) = (angle.cos(), angle.sin());
    let pts: Vec<P> = (0..n)
        .map(|i| {
            let t = 2.0 * PI * i as f64 / n as f64;
            [
                cx + a * t.cos() * ca - b * t.sin() * sa,
                cy + a * t.cos() * sa + b * t.sin() * ca,
            ]
        })
        .collect();
    closed_from_left(pts)
}

type BoxFit = (P, P, f64, f64);

fn boxed(r: &[P]) -> Option<BoxFit> {
    let h = hull(r);
    let mut best: Option<(f64, P, [f64; 4])> = None;
    for i in 0..h.len() {
        let (a, b) = (h[i], h[(i + 1) % h.len()]);
        let d = dist(a, b);
        if d < 1e-12 {
            continue;
        }
        let (ux, uy) = ((b[0] - a[0]) / d, (b[1] - a[1]) / d);
        let us: Vec<f64> = h.iter().map(|p| p[0] * ux + p[1] * uy).collect();
        let vs: Vec<f64> = h.iter().map(|p| -p[0] * uy + p[1] * ux).collect();
        let mx = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mn = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
        let area = (mx(&us) - mn(&us)) * (mx(&vs) - mn(&vs));
        if best.as_ref().is_none_or(|b| area < b.0) {
            best = Some((area, [ux, uy], [mn(&us), mx(&us), mn(&vs), mx(&vs)]));
        }
    }
    let (_, [ux, uy], edges) = best?;
    let mut sides: [Vec<f64>; 4] = Default::default();
    for p in r {
        let (u, v) = (p[0] * ux + p[1] * uy, -p[0] * uy + p[1] * ux);
        let d = [u - edges[0], edges[1] - u, v - edges[2], edges[3] - v];
        let mut i = 0;
        for k in 1..4 {
            if d[k] < d[i] {
                i = k;
            }
        }
        sides[i].push(if i < 2 { u } else { v });
    }
    let m: Vec<f64> = (0..4)
        .map(|i| {
            if sides[i].is_empty() {
                edges[i]
            } else {
                py_sum(sides[i].iter().copied()) / sides[i].len() as f64
            }
        })
        .collect();
    let (u0, u1, v0, v1) = (m[0], m[1], m[2], m[3]);
    if u1 - u0 < 1e-9 || v1 - v0 < 1e-9 {
        return None;
    }
    let (cu, cv) = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
    Some((
        [cu * ux - cv * uy, cu * uy + cv * ux],
        [ux, uy],
        (u1 - u0) / 2.0,
        (v1 - v0) / 2.0,
    ))
}

fn box_points(c: P, d: P, hw: f64, hh: f64) -> Vec<P> {
    let ([cx, cy], [ux, uy]) = (c, d);
    let pts = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .iter()
        .map(|&(su, sv): &(f64, f64)| {
            [
                cx + su * hw * ux - sv * hh * uy,
                cy + su * hw * uy + sv * hh * ux,
            ]
        })
        .collect();
    closed_from_left(pts)
}

fn best_polygon(r: &[P], n: usize) -> Option<Vec<P>> {
    let lp = &r[..r.len() - 1];
    let c = mean(lp);
    let mut i = 0;
    for j in 1..lp.len() {
        if dist(lp[j], c) > dist(lp[i], c) {
            i = j;
        }
    }
    let mut lp2: Vec<P> = lp[i..].to_vec();
    lp2.extend_from_slice(&lp[..i]);
    lp2.push(lp[i]);
    let (mut lo, mut hi) = (
        0.0,
        lp2.iter()
            .map(|p| dist(lp2[0], *p))
            .fold(f64::NEG_INFINITY, f64::max),
    );
    let mut best: Option<Vec<P>> = None;
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        let mut idx = rdp(&lp2, mid);
        idx.pop();
        if idx.len() > n {
            lo = mid;
        } else {
            hi = mid;
            if idx.len() >= 3 {
                best = Some(idx.iter().map(|&j| lp2[j]).collect());
            }
        }
    }
    best.map(closed_from_left)
}

fn dev(r: &[P], shape: &[P]) -> f64 {
    let a = r
        .iter()
        .map(|p| dist_to_path(*p, shape))
        .fold(f64::NEG_INFINITY, f64::max);
    let b = resample(shape, 60)
        .iter()
        .map(|p| dist_to_path(*p, r))
        .fold(f64::NEG_INFINITY, f64::max);
    a.max(b)
}

fn closed_from_left(mut pts: Vec<P>) -> Vec<P> {
    let mut i = 0;
    for j in 1..pts.len() {
        if (pts[j][0], pts[j][1]) < (pts[i][0], pts[i][1]) {
            i = j;
        }
    }
    pts.rotate_left(i);
    pts.push(pts[0]);
    pts
}

// ------------------------------------------------------------------ lines and curves

fn pieces(q: &[P], tol: f64, closed: bool) -> Vec<P> {
    let mut q = q.to_vec();
    if closed && dist(q[0], q[q.len() - 1]) > 1e-12 {
        q.push(q[0]);
    }
    let count = (path_len(&q) / (tol * 2.0).max(1e-12)).ceil();
    let count = (count.min(PIECE_SAMPLES.1 as f64) as usize).max(PIECE_SAMPLES.0);
    let mut r = resample(&q, count);
    let mut corners = corners(&r, tol, closed);
    if closed && !corners.is_empty() {
        let c = corners[0];
        let mut r2 = r[c..].to_vec();
        r2.extend_from_slice(&r[1..=c]);
        r = r2;
        corners = corners
            .iter()
            .map(|&i| (i as i64 - c as i64).rem_euclid(count as i64) as usize)
            .collect();
    }
    let mut cut: Vec<usize> = vec![0];
    cut.extend(&corners);
    cut.push(count);
    cut.sort_unstable();
    cut.dedup();
    let step = dist(r[0], r[1]);
    let mut out = vec![r[0]];
    for w in cut.windows(2) {
        let part = &r[w[0]..=w[1]];
        if part.len() < 3
            || part
                .iter()
                .map(|p| seg_dist(*p, part[0], part[part.len() - 1]))
                .fold(f64::NEG_INFINITY, f64::max)
                <= tol
        {
            out.push(part[part.len() - 1]);
            continue;
        }
        let f = fit(part, tol / 2.0, None, None);
        let mut i = 0;
        while i + 3 < f.len() {
            let seg = &f[i..i + 4];
            let n = rne(path_len(seg) / (step * 4.0).max(tol)).clamp(2.0, 24.0) as usize;
            for k in 1..=n {
                out.push(seg_point(
                    seg[0],
                    seg[1],
                    seg[2],
                    seg[3],
                    k as f64 / n as f64,
                ));
            }
            i += 3;
        }
    }
    if closed {
        let first = out[0];
        let l = out.len();
        out[l - 1] = first;
    }
    out
}

fn corners(r: &[P], tol: f64, closed: bool) -> Vec<usize> {
    let n = r.len() - 1;
    let step = {
        let s = dist(r[0], r[1]);
        if s == 0.0 { 1e-12 } else { s }
    };
    let m = (rne((4.0 * tol).max(0.03 * step * n as f64) / step) as i64).max(3);
    let (n_i, m_u) = (n as i64, m as usize);
    if !closed && 2 * m_u >= n {
        return Vec::new();
    }
    let idx: Vec<i64> = if closed {
        (0..n_i).collect()
    } else {
        (m..=n_i - m).collect()
    };
    let at = |i: i64| -> P {
        if closed {
            r[i.rem_euclid(n_i) as usize]
        } else {
            r[i as usize]
        }
    };
    let mut turn: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for &i in &idx {
        turn.insert(i, turn_at(at(i - m), at(i), at(i + m)));
    }
    let mut out = Vec::new();
    for &i in &idx {
        let ti = turn[&i];
        if ti <= CORNER || turn_at(at(i - m / 3), at(i), at(i + m / 3)) < 0.6 * ti {
            continue;
        }
        let ok = (i - m..=i + m).filter(|&j| j != i).all(|j| {
            let jj = if closed { j.rem_euclid(n_i) } else { j };
            match turn.get(&jj) {
                Some(&tj) => ti > tj || (ti == tj && i < jj),
                None => ti > 0.0,
            }
        });
        if ok {
            out.push(i as usize);
        }
    }
    out
}

// ------------------------------------------------------------------ helpers

fn dedupe(pts: &[P]) -> Vec<P> {
    let mut out = vec![pts[0]];
    for &p in &pts[1..] {
        if dist(p, out[out.len() - 1]) > 1e-9 {
            out.push(p);
        }
    }
    out
}

fn path_len(pts: &[P]) -> f64 {
    py_sum(pts.windows(2).map(|w| dist(w[0], w[1])))
}

/// `smooth._resample`: n + 1 points evenly along the path.
fn resample(pts: &[P], n: usize) -> Vec<P> {
    let mut lens = vec![0.0];
    for w in pts.windows(2) {
        let l = lens[lens.len() - 1] + dist(w[0], w[1]);
        lens.push(l);
    }
    let total = lens[lens.len() - 1];
    if total < 1e-12 {
        return vec![pts[0]; n + 1];
    }
    let mut out = Vec::with_capacity(n + 1);
    let mut j = 0;
    for i in 0..=n {
        let s = total * i as f64 / n as f64;
        while j + 2 < pts.len() && lens[j + 1] < s {
            j += 1;
        }
        let seg = lens[j + 1] - lens[j];
        let t = if seg < 1e-12 {
            0.0
        } else {
            (s - lens[j]) / seg
        };
        let (a, b) = (pts[j], pts[j + 1]);
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    out
}

fn seg_dist(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let ll = dx * dx + dy * dy;
    let t = if ll < 1e-24 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / ll).clamp(0.0, 1.0)
    };
    hypot(p[0] - a[0] - t * dx, p[1] - a[1] - t * dy)
}

fn dist_to_path(p: P, path: &[P]) -> f64 {
    path.windows(2)
        .map(|w| seg_dist(p, w[0], w[1]))
        .fold(f64::INFINITY, f64::min)
}

fn turn_at(a: P, b: P, c: P) -> f64 {
    let (d1, d2) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
    let (l1, l2) = (hypot(d1[0], d1[1]), hypot(d2[0], d2[1]));
    if l1 < 1e-12 || l2 < 1e-12 {
        return 0.0;
    }
    ((d1[0] * d2[0] + d1[1] * d2[1]) / (l1 * l2))
        .clamp(-1.0, 1.0)
        .acos()
}

fn rdp(pts: &[P], tol: f64) -> Vec<usize> {
    let mut keep = std::collections::BTreeSet::new();
    keep.insert(0);
    keep.insert(pts.len() - 1);
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let mut worst = -1.0;
        let mut at = None;
        for i in a + 1..b {
            let d = seg_dist(pts[i], pts[a], pts[b]);
            if d > worst {
                worst = d;
                at = Some(i);
            }
        }
        if let Some(i) = at
            && worst > tol
        {
            keep.insert(i);
            stack.push((a, i));
            stack.push((i, b));
        }
    }
    keep.into_iter().collect()
}

fn hull(pts: &[P]) -> Vec<P> {
    let mut p: Vec<P> = pts.to_vec();
    p.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: P, a: P, b: P| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<P> = Vec::new();
    for &q in &p {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], q) <= 0.0 {
            lower.pop();
        }
        lower.push(q);
    }
    let mut upper: Vec<P> = Vec::new();
    for &q in p.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], q) <= 0.0 {
            upper.pop();
        }
        upper.push(q);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}
