//! Bounded pure geometry shared by plugin workers and native drawing services.
//! Legacy generator flattening retains its original fixed-grid behavior.
use super::Pt;
type P2 = (f64, f64);

pub const MAX_FLATTEN_POINTS: usize = 4096;
pub const MAX_FLATTEN_DEPTH: usize = 24;

/// Nearest point on a finite segment, its parameter and Euclidean distance.
/// Finite coordinates in +/-1e12 are required by callers.
pub fn nearest_segment(a: P2, b: P2, p: P2) -> (P2, f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    let t = if len == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * (dx / len) + (p.1 - a.1) * (dy / len)) / len).clamp(0.0, 1.0)
    };
    let q = if t == 0.0 {
        a
    } else if t == 1.0 {
        b
    } else {
        (a.0 + dx * t, a.1 + dy * t)
    };
    (q, t, (p.0 - q.0).hypot(p.1 - q.1))
}

#[derive(Debug, PartialEq)]
pub enum FlattenError<E> {
    Invalid,
    Precision,
    Depth,
    Points,
    Allocation,
    Stopped(E),
}
#[derive(Clone, Copy)]
struct Node {
    p: [P2; 4],
    t0: f64,
    t1: f64,
    depth: usize,
}
pub const FLATTEN_WORKSPACE_BYTES: usize = std::mem::size_of::<Node>() * (MAX_FLATTEN_DEPTH + 1);

fn midpoint(a: P2, b: P2) -> P2 {
    (a.0 * 0.5 + b.0 * 0.5, a.1 * 0.5 + b.1 * 0.5)
}

/// Ordered de Casteljau flattening. Each accepted subcurve's control hull lies
/// within `tolerance` of its finite endpoint chord, including a conservative
/// floating arithmetic allowance (128 epsilon times the input coordinate scale).
/// Unlike midpoint-only flatness this catches loops, inflections and backtracking.
/// On any unmet tolerance/limit/cancellation, the private output is discarded.
/// `Pt.s` is the original cubic parameter. No user evaluator is called.
pub fn flatten_cubic<E>(
    p: [P2; 4],
    tolerance: f64,
    max_points: usize,
    max_depth: usize,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Vec<Pt>, FlattenError<E>> {
    use FlattenError::*;
    check().map_err(Stopped)?;
    if !(2..=MAX_FLATTEN_POINTS).contains(&max_points)
        || max_depth > MAX_FLATTEN_DEPTH
        || !tolerance.is_finite()
        || tolerance <= 0.0
        || tolerance > 1e12
        || p.iter()
            .any(|p| !p.0.is_finite() || !p.1.is_finite() || p.0.abs() > 1e12 || p.1.abs() > 1e12)
    {
        return Err(Invalid);
    }
    let scale = p
        .iter()
        .fold(1e-300f64, |s, p| s.max(p.0.abs()).max(p.1.abs()));
    let allowance = 128.0 * f64::EPSILON * scale;
    if tolerance <= allowance {
        return Err(Precision);
    }
    let root = Node {
        p,
        t0: 0.0,
        t1: 1.0,
        depth: 0,
    };
    let mut stack = [root; MAX_FLATTEN_DEPTH + 1];
    let mut len = 1;
    let mut out = Vec::new();
    out.try_reserve_exact(max_points).map_err(|_| Allocation)?;
    out.push(Pt {
        x: p[0].0,
        y: p[0].1,
        s: 0.0,
    });
    let mut visited = 0usize;
    while len != 0 {
        if visited.is_multiple_of(64) {
            check().map_err(Stopped)?;
        }
        visited += 1;
        len -= 1;
        let n = stack[len];
        let [a, b, c, d] = n.p;
        let flatness = nearest_segment(a, d, b).2.max(nearest_segment(a, d, c).2);
        if flatness <= tolerance - allowance {
            out.push(Pt {
                x: d.0,
                y: d.1,
                s: n.t1,
            });
            continue;
        }
        if n.depth == max_depth {
            return Err(Depth);
        }
        // Every pending node needs at least one endpoint. Splitting adds two.
        if out.len() + len + 2 > max_points {
            return Err(Points);
        }
        let (ab, bc, cd) = (midpoint(a, b), midpoint(b, c), midpoint(c, d));
        let (abc, bcd) = (midpoint(ab, bc), midpoint(bc, cd));
        let m = midpoint(abc, bcd);
        let t = (n.t0 + n.t1) * 0.5;
        stack[len] = Node {
            p: [m, bcd, cd, d],
            t0: t,
            t1: n.t1,
            depth: n.depth + 1,
        };
        stack[len + 1] = Node {
            p: [a, ab, abc, m],
            t0: n.t0,
            t1: t,
            depth: n.depth + 1,
        };
        len += 2;
    }
    check().map_err(Stopped)?;
    Ok(out)
}

#[cfg(test)]
#[path = "draw_geometry_tests.rs"]
mod tests;
