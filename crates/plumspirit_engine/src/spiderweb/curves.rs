//! Curves and arcs (ports of Spiderweb `bezier.py` sampling and `arc.py`,
//! MIT © Kanade Tachibana), and velocity envelopes (`envelope.py`).
//! Points are `(beat, pitch)`.

use super::paths::P;
use std::f64::consts::PI;

// ------------------------------------------------------------------ Bézier

/// Anchors of a flat Bézier list (anchor, handle, handle, anchor, …).
pub fn anchor_count(n: usize) -> usize {
    (n.saturating_sub(1)) / 3 + 1
}

#[inline]
pub fn seg_point(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
    let mt = 1.0 - t;
    let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
    [
        a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0],
        a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1],
    ]
}

/// Points along the curve, `n` per segment (the first anchor, then i/n for
/// i = 1..=n of every segment).
pub fn sample(pts: &[P], n: usize) -> Vec<P> {
    let mut out = vec![pts[0]];
    let mut i = 0;
    while i + 3 < pts.len() {
        let s = &pts[i..i + 4];
        for k in 1..=n {
            out.push(seg_point(s[0], s[1], s[2], s[3], k as f64 / n as f64));
        }
        i += 3;
    }
    out
}

// ------------------------------------------------------------------ arcs

/// One sample point per 1.5 degrees.
pub const ARC_STEP: f64 = 1.5 * PI / 180.0;

#[inline]
fn dist(a: P, b: P) -> f64 {
    super::np::dist(a, b) // math.dist
}

/// `(centre, radius)` of the circle through a, b, c (None when in a line).
pub fn circle(a: P, b: P, c: P) -> Option<(P, f64)> {
    let a_sq = (b[0] - c[0]).powi(2) + (b[1] - c[1]).powi(2);
    let b_sq = (a[0] - c[0]).powi(2) + (a[1] - c[1]).powi(2);
    let c_sq = (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2);
    let size = a_sq.max(b_sq).max(c_sq);
    if size == 0.0 || a_sq.min(b_sq).min(c_sq) < 1e-12 * size {
        return None;
    }
    let s = a_sq * (b_sq + c_sq - a_sq);
    let t = b_sq * (a_sq + c_sq - b_sq);
    let u = c_sq * (a_sq + b_sq - c_sq);
    let total = s + t + u;
    if total.abs() < 1e-9 * size * size {
        return None;
    }
    let centre = [
        (s * a[0] + t * b[0] + u * c[0]) / total,
        (s * a[1] + t * b[1] + u * c[1]) / total,
    ];
    Some((centre, dist(a, centre)))
}

fn angles(a: P, b: P, c: P, centre: P) -> (f64, f64) {
    let t0 = (a[1] - centre[1]).atan2(a[0] - centre[0]);
    let t1 = (c[1] - centre[1]).atan2(c[0] - centre[0]);
    let mut span = (t1 - t0).rem_euclid(2.0 * PI);
    if (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0]) > 0.0 {
        span -= 2.0 * PI;
    }
    (t0, span)
}

/// `(centre, radius, start angle, signed span)` of the arc through m.
fn arc_of(m: [P; 3]) -> Option<(P, f64, f64, f64)> {
    let [a, b, c] = m;
    if a == c && a != b {
        let centre = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        return Some((
            centre,
            dist(a, centre),
            (a[1] - centre[1]).atan2(a[0] - centre[0]),
            2.0 * PI,
        ));
    }
    let (centre, r) = circle(a, b, c)?;
    let (t0, span) = angles(a, b, c, centre);
    Some((centre, r, t0, span))
}

/// The arc from `pts[0]` through `pts[1]` to `pts[2]` as points (beats divided
/// by `k` while computing the circle).
pub fn arc_points(pts: &[P], k: f64) -> Vec<P> {
    arc_points_step(pts, k, ARC_STEP)
}

/// [`arc_points`] with its own angle step.
pub fn arc_points_step(pts: &[P], k: f64, step: f64) -> Vec<P> {
    if pts.len() < 3 {
        return vec![pts[0], pts[pts.len() - 1]];
    }
    let (a, b, c) = (pts[0], pts[1], pts[2]);
    let m = [[a[0] / k, a[1]], [b[0] / k, b[1]], [c[0] / k, c[1]]];
    let Some((centre, r, t0, span)) = arc_of(m) else {
        return if b != a && b != c {
            vec![a, b, c]
        } else {
            vec![a, c]
        };
    };
    let n = ((span.abs() / step).ceil() as usize).max(2);
    let mut out = vec![a];
    for i in 1..n {
        let t = t0 + span * i as f64 / n as f64;
        out.push([(centre[0] + r * t.cos()) * k, centre[1] + r * t.sin()]);
    }
    out.push(c);
    out
}

/// A shape's `k` (1 when missing or broken).
pub fn arc_k(k: Option<f64>) -> f64 {
    match k {
        Some(k) if k > 1e-9 && k < 1e9 && k.is_finite() => k,
        _ => 1.0,
    }
}

// ------------------------------------------------------------------ envelopes

/// `[u, velocity]` points sorted by u (two on one u make a jump).
pub type Env = Vec<[f64; 2]>;

/// The envelope value at each u (`env_values`).
pub fn env_value(env: &[[f64; 2]], u: f64) -> f64 {
    let n = env.len();
    // searchsorted right
    let i = env.partition_point(|p| p[0] <= u);
    if i == 0 {
        return env[0][1];
    }
    if i == n {
        return env[n - 1][1];
    }
    let (i0, i1) = (i - 1, i);
    let (u0, u1, v0, v1) = (env[i0][0], env[i1][0], env[i0][1], env[i1][1]);
    if u1 == u0 {
        v0
    } else {
        v0 + (v1 - v0) * (u - u0) / (u1 - u0)
    }
}

/// Round half to even (Python `round`, NumPy `np.round`).
#[inline]
pub fn rne(x: f64) -> f64 {
    x.round_ties_even()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_and_envelopes_follow_python() {
        assert_eq!(
            (rne(0.5), rne(1.5), rne(2.5), rne(-0.5), rne(2.4)),
            (0.0, 2.0, 2.0, -0.0, 2.0)
        );
        let env = vec![[0.0, 10.0], [0.5, 20.0], [0.5, 100.0], [1.0, 50.0]];
        assert_eq!(env_value(&env, -1.0), 10.0);
        assert_eq!(env_value(&env, 0.25), 15.0);
        assert_eq!(env_value(&env, 0.5), 100.0);
        assert_eq!(env_value(&env, 2.0), 50.0);
    }

    #[test]
    fn arcs_pass_through_their_points_and_circles_close() {
        let a = arc_points(&[[0.0, 60.0], [1.0, 61.0], [2.0, 60.0]], 1.0);
        assert_eq!(a[0], [0.0, 60.0]);
        assert_eq!(*a.last().unwrap(), [2.0, 60.0]);
        assert!(a.len() > 100);
        let c = arc_points(&[[0.0, 60.0], [2.0, 60.0], [0.0, 60.0]], 1.0);
        assert_eq!(c.first(), c.last());
        assert_eq!(
            arc_points(&[[0.0, 60.0], [1.0, 61.0], [2.0, 62.0]], 1.0).len(),
            3
        );
    }
}
