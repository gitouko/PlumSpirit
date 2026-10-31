//! Closed shapes rasterized row by row (V09-S53, `docs/GENERATORS.md`
//! "Shared drawing: regions"): the plumbing Rectangle uses first and the
//! other closed-shape generators (Polygons, Circles, Custom Shape) reuse.
//!
//! Shapes live in **grid space** — `u` = ticks / step (one unit = a
//! resolution step), `v` = row coordinate (row r covers [r, r + 1)) — so
//! offsets, radii and angles are measured on the grid. A [`Poly`] is a
//! convex polygon, counter-clockwise, each edge tagged with a *side*
//! (bottom, right, top, left for a rectangle); it can be offset (inset by
//! half-plane clipping, outset by mitres with bevels), chamfered and
//! rounded, staying convex.
//!
//! A [`Scene`] is a set of convex rings with roles — the outline band's
//! outer and inner boundary, a hole's, nested outlines, walls — plus line
//! features. Each row is scanned at its centre (the pen's rule): every ring
//! gives one interval, the intervals' counts classify the row's pieces as
//! **edge** (the outline band, nested outlines, walls), **interior** or
//! nothing. Edge pieces take a brush by the side of the shape's nearest
//! edge and an optional dash pattern along that edge; interior pieces are
//! drawn where the fill patterns are (stripes, crosshatch, grid, lines) —
//! or, filled, where they are not. Pieces are rounded to whole grid cells
//! (`round`, the pen's), kept disjoint, cut by the gradients and written
//! with [`Draw::note`]; touching pieces with one brush merge.
//!
//! Every loop is bounded or draws: rows are clipped to the pen's band,
//! each ring crosses a row at most twice, dash / stripe windows are
//! visited lazily from the piece's start (each one draws), a union of
//! stripe patterns that covers a whole period jumps to the piece's end,
//! a line feature skips the stripes it covers, and a changing brush cuts a
//! piece into at most [`MAX_PIECES`](super::draw::MAX_PIECES) parts.

use super::super::Curve;
use super::area::{self, Geom, RowBuf, RowState};
use super::draw::{Grad, pieces};
use super::pen::{Band, Brush, Draw};
use std::cmp::Ordering;
use std::f64::consts::TAU;

/// A point in grid space: `u` = ticks / step, `v` = row coordinate.
pub type P2 = (f64, f64);

/// Most segments of one rounded corner.
pub const ARC_CAP: usize = 32;
/// The longest segment (grid units) of a rounded corner's arc.
const ARC_STEP: f64 = 0.5;
/// Polygons with a smaller area (grid units²) are empty.
const EPS_AREA: f64 = 1e-9;

/// A convex polygon in grid space, counter-clockwise; `sides[i]` tags edge
/// `pts[i] → pts[i + 1]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Poly {
    pub pts: Vec<P2>,
    pub sides: Vec<u8>,
}

fn sub(a: P2, b: P2) -> P2 {
    (a.0 - b.0, a.1 - b.1)
}

fn unit(d: P2) -> P2 {
    let l = d.0.hypot(d.1);
    if l > 0.0 {
        (d.0 / l, d.1 / l)
    } else {
        (0.0, 0.0)
    }
}

/// Whether `a` and `b` are the same point (each coordinate within a
/// relative tolerance of its own size: u can be 2⁶⁴ while v is a row).
fn near(a: P2, b: P2) -> bool {
    let tol = |v: f64| 1e-9 * v.abs().max(1.0);
    (a.0 - b.0).abs() <= tol(a.0) && (a.1 - b.1).abs() <= tol(a.1)
}

/// Distance from `p` to segment `a → b`.
fn seg_dist(p: P2, a: P2, b: P2) -> f64 {
    let d = sub(b, a);
    let l2 = d.0 * d.0 + d.1 * d.1;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * d.0).hypot(p.1 - a.1 - t * d.1)
}

impl Poly {
    /// The rectangle `[u0, u1] × [v0, v1]`, counter-clockwise from its
    /// bottom-left corner: sides 0 bottom, 1 right, 2 top, 3 left.
    pub fn rect(u0: f64, v0: f64, u1: f64, v1: f64) -> Poly {
        Poly {
            pts: vec![(u0, v0), (u1, v0), (u1, v1), (u0, v1)],
            sides: vec![0, 1, 2, 3],
        }
        .tidy()
    }

    /// The signed area (positive counter-clockwise).
    pub fn area(&self) -> f64 {
        let n = self.pts.len();
        let mut a = 0.0;
        for i in 0..n {
            let (p, q) = (self.pts[i], self.pts[(i + 1) % n]);
            a += p.0 * q.1 - q.0 * p.1;
        }
        a / 2.0
    }

    /// Whether the polygon has no area.
    pub fn is_empty(&self) -> bool {
        self.pts.len() < 3 || self.area().abs().partial_cmp(&EPS_AREA) != Some(Ordering::Greater)
    }

    /// Whether every turn is to the left (or straight): a convex polygon.
    pub fn is_convex(&self) -> bool {
        let n = self.pts.len();
        if n < 3 {
            return false;
        }
        let b = self.bbox();
        let tol = 1e-12 * ((b[2] - b[0]).powi(2) + (b[3] - b[1]).powi(2)).max(1.0);
        (0..n).all(|i| {
            let (a, p, c) = (self.pts[i], self.pts[(i + 1) % n], self.pts[(i + 2) % n]);
            (p.0 - a.0) * (c.1 - p.1) - (p.1 - a.1) * (c.0 - p.0) >= -tol
        })
    }

    /// Edge `i`: its ends.
    pub fn edge(&self, i: usize) -> (P2, P2) {
        (self.pts[i], self.pts[(i + 1) % self.pts.len()])
    }

    /// Coincident neighbours merged (the later one's edge kept), then
    /// counter-clockwise; empty when degenerate or not finite.
    fn tidy(self) -> Poly {
        if self
            .pts
            .iter()
            .any(|p| !(p.0.is_finite() && p.1.is_finite()))
        {
            return Poly::default();
        }
        let mut pts: Vec<P2> = Vec::with_capacity(self.pts.len());
        let mut sides: Vec<u8> = Vec::with_capacity(self.pts.len());
        for (i, &p) in self.pts.iter().enumerate() {
            let s = self.sides.get(i).copied().unwrap_or(0);
            match pts.last() {
                Some(&l) if near(l, p) => *sides.last_mut().unwrap() = s,
                _ => {
                    pts.push(p);
                    sides.push(s);
                }
            }
        }
        while pts.len() > 1 && near(pts[pts.len() - 1], pts[0]) {
            pts.pop();
            sides.pop();
        }
        let mut out = Poly { pts, sides };
        if out.is_empty() {
            return Poly::default();
        }
        if out.area() < 0.0 {
            let n = out.pts.len();
            out.pts.reverse();
            let old = std::mem::take(&mut out.sides);
            out.sides = (0..n).map(|k| old[(2 * n - 2 - k) % n]).collect();
        }
        out
    }

    /// Every point mapped by `f` (sides kept; counter-clockwise again).
    pub fn map(&self, f: impl Fn(P2) -> P2) -> Poly {
        Poly {
            pts: self.pts.iter().map(|&p| f(p)).collect(),
            sides: self.sides.clone(),
        }
        .tidy()
    }

    /// `[umin, vmin, umax, vmax]`.
    pub fn bbox(&self) -> [f64; 4] {
        self.pts.iter().fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)],
        )
    }

    /// The polygon moved `d` grid units outwards (`d` > 0) or inwards
    /// (`d` < 0; empty once it vanishes). Sides are not kept.
    pub fn offset(&self, d: f64) -> Poly {
        if self.is_empty() || !d.is_finite() {
            return Poly::default();
        }
        if d > 0.0 {
            self.outset(d)
        } else if d < 0.0 {
            self.inset(-d)
        } else {
            self.clone()
        }
    }

    /// Inwards by `d` (> 0): the polygon clipped by each edge's half-plane
    /// moved in (Sutherland–Hodgman; exact for a convex polygon).
    fn inset(&self, d: f64) -> Poly {
        let n = self.pts.len();
        let mut cur = self.pts.clone();
        let mut next: Vec<P2> = Vec::with_capacity(n + 4);
        for i in 0..n {
            let (a, b) = self.edge(i);
            let dir = unit(sub(b, a));
            if dir == (0.0, 0.0) {
                continue;
            }
            let nrm = (-dir.1, dir.0);
            let f = |p: P2| (p.0 - a.0) * nrm.0 + (p.1 - a.1) * nrm.1 - d;
            next.clear();
            for k in 0..cur.len() {
                let (p, q) = (cur[k], cur[(k + 1) % cur.len()]);
                let (fp, fq) = (f(p), f(q));
                if fp >= 0.0 {
                    next.push(p);
                }
                if (fp >= 0.0) != (fq >= 0.0) {
                    let t = fp / (fp - fq);
                    next.push((p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t));
                }
            }
            std::mem::swap(&mut cur, &mut next);
            if cur.len() < 3 {
                return Poly::default();
            }
        }
        let sides = vec![0; cur.len()];
        Poly { pts: cur, sides }.tidy()
    }

    /// Outwards by `d` (> 0): mitred corners, bevelled where a mitre would
    /// reach more than twice `d` from the corner.
    fn outset(&self, d: f64) -> Poly {
        let n = self.pts.len();
        let mut pts = Vec::with_capacity(2 * n);
        for i in 0..n {
            let (prev, p, next) = (
                self.pts[(i + n - 1) % n],
                self.pts[i],
                self.pts[(i + 1) % n],
            );
            let (d1, d2) = (unit(sub(p, prev)), unit(sub(next, p)));
            let (n1, n2) = ((d1.1, -d1.0), (d2.1, -d2.0));
            let dot = n1.0 * n2.0 + n1.1 * n2.1;
            if dot > 1.0 - 1e-12 {
                pts.push((p.0 + n1.0 * d, p.1 + n1.1 * d));
            } else if ((1.0 + dot) / 2.0).sqrt() < 0.5 {
                pts.push((p.0 + n1.0 * d, p.1 + n1.1 * d));
                pts.push((p.0 + n2.0 * d, p.1 + n2.1 * d));
            } else {
                let k = d / (1.0 + dot);
                pts.push((p.0 + (n1.0 + n2.0) * k, p.1 + (n1.1 + n2.1) * k));
            }
        }
        let sides = vec![0; pts.len()];
        Poly { pts, sides }.tidy()
    }

    /// Each corner cut `c` grid units back along both edges (at most half
    /// of each); the cut takes the incoming edge's side.
    pub fn chamfer(&self, c: f64) -> Poly {
        let n = self.pts.len();
        if n < 3 || c.partial_cmp(&0.0) != Some(Ordering::Greater) {
            return self.clone();
        }
        let mut pts = Vec::with_capacity(2 * n);
        let mut sides = Vec::with_capacity(2 * n);
        for i in 0..n {
            let (prev, p, next) = (
                self.pts[(i + n - 1) % n],
                self.pts[i],
                self.pts[(i + 1) % n],
            );
            let (l1, l2) = (sub(p, prev), sub(next, p));
            let cc = c.min(l1.0.hypot(l1.1) / 2.0).min(l2.0.hypot(l2.1) / 2.0);
            if cc.partial_cmp(&1e-12) != Some(Ordering::Greater) {
                pts.push(p);
                sides.push(self.sides[i]);
                continue;
            }
            let (d1, d2) = (unit(l1), unit(l2));
            pts.push((p.0 - d1.0 * cc, p.1 - d1.1 * cc));
            sides.push(self.sides[(i + n - 1) % n]);
            pts.push((p.0 + d2.0 * cc, p.1 + d2.1 * cc));
            sides.push(self.sides[i]);
        }
        Poly { pts, sides }.tidy()
    }

    /// Each convex corner replaced by a circular arc of radius `r` grid
    /// units tangent to both edges (smaller where the edges are shorter
    /// than its tangents); arcs of at most [`ARC_CAP`] segments, the first
    /// half tagged with the incoming edge's side.
    pub fn round(&self, r: f64) -> Poly {
        let n = self.pts.len();
        if n < 3 || r.partial_cmp(&0.0) != Some(Ordering::Greater) {
            return self.clone();
        }
        let mut pts = Vec::with_capacity(n * 8);
        let mut sides = Vec::with_capacity(n * 8);
        for i in 0..n {
            let (prev, p, next) = (
                self.pts[(i + n - 1) % n],
                self.pts[i],
                self.pts[(i + 1) % n],
            );
            let (l1, l2) = (sub(p, prev), sub(next, p));
            let (d1, d2) = (unit(l1), unit(l2));
            let cross = d1.0 * d2.1 - d1.1 * d2.0;
            let turn = (d1.0 * d2.0 + d1.1 * d2.1).clamp(-1.0, 1.0).acos();
            let (s_in, s_out) = (self.sides[(i + n - 1) % n], self.sides[i]);
            if !(cross > 1e-12 && turn > 1e-6 && turn < std::f64::consts::PI - 1e-6) {
                pts.push(p);
                sides.push(s_out);
                continue;
            }
            let th = (turn / 2.0).tan();
            let t = (r * th)
                .min(l1.0.hypot(l1.1) / 2.0)
                .min(l2.0.hypot(l2.1) / 2.0);
            let re = t / th;
            let t1 = (p.0 - d1.0 * t, p.1 - d1.1 * t);
            let t2 = (p.0 + d2.0 * t, p.1 + d2.1 * t);
            let c = (t1.0 - d1.1 * re, t1.1 + d1.0 * re);
            let a1 = (t1.1 - c.1).atan2(t1.0 - c.0);
            let k = ((turn * re / ARC_STEP).ceil() as usize).clamp(1, ARC_CAP);
            for j in 0..=k {
                let q = if j == 0 {
                    t1
                } else if j == k {
                    t2
                } else {
                    let a = a1 + turn * j as f64 / k as f64;
                    (c.0 + re * a.cos(), c.1 + re * a.sin())
                };
                pts.push(q);
                sides.push(if j < k && 2 * j + 1 < k { s_in } else { s_out });
            }
        }
        Poly { pts, sides }.tidy()
    }

    /// The crossings of the row whose centre is `yc` with the polygon's
    /// edges (the pen's rule: an edge crosses when one end is at or below
    /// `yc` and the other above), appended to `out` unsorted.
    fn crossings(&self, yc: f64, out: &mut Vec<f64>) {
        let n = self.pts.len();
        for k in 0..n {
            let (a, b) = (self.pts[k], self.pts[(k + 1) % n]);
            if (a.1 <= yc && yc < b.1) || (b.1 <= yc && yc < a.1) {
                out.push(a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1));
            }
        }
    }

    /// The crossing of edge `k` with the row centre `yc` (the pen's
    /// formula).
    #[inline]
    fn cross_x(&self, k: usize, yc: f64) -> f64 {
        let (a, b) = self.edge(k);
        a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1)
    }

    /// A polygon from points and sides (coincident neighbours merged,
    /// counter-clockwise; empty when degenerate).
    pub fn from_points(pts: Vec<P2>, sides: Vec<u8>) -> Poly {
        Poly { pts, sides }.tidy()
    }

    /// The polygon moved `d` grid units out (`d` > 0) or in along the rays
    /// from `c` (V09-S56; the offset of a ring that is not convex): each
    /// vertex by `d / cos φ`, cos φ the mean of its ray's cosines with its
    /// two edges' normals (so each edge's line moves by `d` where the two
    /// agree; at most 8 · |d|), never past `c` — a polygon whose vertices
    /// go round `c` in order (star-shaped about it) stays a simple
    /// star-shaped polygon. Exact for a regular polygon or star; sides kept.
    pub fn polar_offset(&self, c: P2, d: f64) -> Poly {
        let n = self.pts.len();
        if self.is_empty() || !d.is_finite() || !(c.0.is_finite() && c.1.is_finite()) {
            return Poly::default();
        }
        if d == 0.0 {
            return self.clone();
        }
        let pts = (0..n)
            .map(|i| {
                let (prev, p, next) = (
                    self.pts[(i + n - 1) % n],
                    self.pts[i],
                    self.pts[(i + 1) % n],
                );
                let (d1, d2) = (unit(sub(p, prev)), unit(sub(next, p)));
                // Outward normals (counter-clockwise polygon).
                let (n1, n2) = ((d1.1, -d1.0), (d2.1, -d2.0));
                let ray = sub(p, c);
                let r = ray.0.hypot(ray.1);
                if r <= 0.0 {
                    return p;
                }
                let dir = (ray.0 / r, ray.1 / r);
                let cos =
                    ((dir.0 * n1.0 + dir.1 * n1.1 + dir.0 * n2.0 + dir.1 * n2.1) / 2.0).max(0.125);
                let rr = (r + d / cos).max(0.0);
                (c.0 + dir.0 * rr, c.1 + dir.1 * rr)
            })
            .collect();
        Poly {
            pts,
            sides: self.sides.clone(),
        }
        .tidy()
    }
}

/// The two monotone chains of a ring (edge indices from its lowest vertex
/// up and from its highest down) and their vertices' rows, for a crossing
/// by binary search.
#[derive(Clone, Debug, Default)]
struct Chains {
    up: Vec<usize>,
    up_v: Vec<f64>,
    down: Vec<usize>,
    down_v: Vec<f64>,
}

impl Chains {
    /// The chains of `p` when both are monotone (every convex polygon's).
    fn of(p: &Poly) -> Option<Chains> {
        let n = p.pts.len();
        if n < 3 {
            return None;
        }
        let i0 = (0..n).min_by(|&a, &b| p.pts[a].1.total_cmp(&p.pts[b].1).then(a.cmp(&b)))?;
        let i1 = (0..n).min_by(|&a, &b| p.pts[b].1.total_cmp(&p.pts[a].1).then(a.cmp(&b)))?;
        let mut c = Chains::default();
        let mut k = i0;
        c.up_v.push(p.pts[k].1);
        while k != i1 {
            c.up.push(k);
            k = (k + 1) % n;
            if p.pts[k].1 < *c.up_v.last().unwrap() {
                return None;
            }
            c.up_v.push(p.pts[k].1);
        }
        c.down_v.push(p.pts[k].1);
        while k != i0 {
            c.down.push(k);
            k = (k + 1) % n;
            if p.pts[k].1 > *c.down_v.last().unwrap() {
                return None;
            }
            c.down_v.push(p.pts[k].1);
        }
        Some(c)
    }
}

/// What a ring of a [`Scene`] bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The outline band's outer boundary.
    Outer = 0,
    /// The outline band's inner boundary (the interior's).
    Inner,
    /// A hole's boundary on the outline band's side (inside it, nothing).
    HoleOuter,
    /// A hole's boundary on the interior's side.
    HoleInner,
    /// A nested outline's outer boundary.
    NestOuter,
    /// A nested outline's inner boundary.
    NestInner,
    /// A wall (a band across the interior).
    Wall,
    /// The outline band's middle (inside it: the band's inner half).
    Mid,
    /// An interior region drawn as a pattern (a sector, …).
    Fill,
    /// Where anything is drawn (when a scene has mask rings, nothing
    /// outside them is).
    Mask,
}

const ROLES: usize = 10;

/// A ring to build a scene with: its polygon, role, the copy of the shape
/// it belongs to, and whether it is convex (one interval per row — the
/// outermost crossings) or a general (star-shaped) ring scanned even–odd.
#[derive(Clone, Debug)]
pub struct RingSpec {
    pub poly: Poly,
    pub role: Role,
    pub copy: u16,
    pub convex: bool,
}

impl RingSpec {
    /// A convex ring of copy 0.
    pub fn convex(poly: Poly, role: Role) -> RingSpec {
        RingSpec {
            poly,
            role,
            copy: 0,
            convex: true,
        }
    }
}

/// A ring of a scene.
#[derive(Clone, Debug)]
struct Ring {
    poly: Poly,
    role: Role,
    copy: u16,
    convex: bool,
    chains: Option<Chains>,
    /// A general ring's edges by their lowest row (the active edge
    /// table's order).
    order: Vec<u32>,
    vmin: f64,
    vmax: f64,
}

/// A general ring's active edge table while rows are scanned upwards: the
/// next edge of `order` to enter, the edges crossing the current row band.
#[derive(Clone, Debug, Default)]
struct Aet {
    next: usize,
    active: Vec<u32>,
    /// The row centre last scanned (rows must ascend; otherwise restart).
    last: f64,
}

impl Ring {
    /// The ring's intervals on the row whose centre is `yc`, appended to
    /// `out` as (start, end) pairs: a convex ring's outermost crossings
    /// (by binary search on its monotone chains), a general ring's
    /// even–odd pairs.
    fn spans(&self, yc: f64, xs: &mut Vec<f64>, out: &mut Vec<(f64, f64)>, aet: Option<&mut Aet>) {
        if yc < self.vmin || yc >= self.vmax {
            return;
        }
        if self.convex {
            let (lo, hi) = match &self.chains {
                Some(c) => {
                    let p = c.up_v.partition_point(|&v| v <= yc);
                    let q = c.down_v.partition_point(|&v| v > yc);
                    if p == 0 || p >= c.up_v.len() || q == 0 || q >= c.down_v.len() {
                        return;
                    }
                    let (x1, x2) = (
                        self.poly.cross_x(c.up[p - 1], yc),
                        self.poly.cross_x(c.down[q - 1], yc),
                    );
                    (x1.min(x2), x1.max(x2))
                }
                None => {
                    xs.clear();
                    self.poly.crossings(yc, xs);
                    xs.iter()
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &x| {
                            (l.min(x), h.max(x))
                        })
                }
            };
            if hi > lo {
                out.push((lo, hi));
            }
            return;
        }
        xs.clear();
        match aet {
            Some(st) if !self.order.is_empty() => {
                // Edges enter when their lowest row is reached and leave
                // after their highest: only the active ones are tested
                // (the same crossings as a full scan).
                if yc.partial_cmp(&st.last).is_none_or(|o| o == Ordering::Less) {
                    st.next = 0;
                    st.active.clear();
                }
                st.last = yc;
                let pts = &self.poly.pts;
                let n = pts.len();
                let lo = |k: usize| pts[k].1.min(pts[(k + 1) % n].1);
                let hi = |k: usize| pts[k].1.max(pts[(k + 1) % n].1);
                while st.next < self.order.len() && lo(self.order[st.next] as usize) <= yc {
                    st.active.push(self.order[st.next]);
                    st.next += 1;
                }
                st.active.retain(|&k| hi(k as usize) > yc);
                for &k in &st.active {
                    let (a, b) = self.poly.edge(k as usize);
                    if (a.1 <= yc && yc < b.1) || (b.1 <= yc && yc < a.1) {
                        xs.push(a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1));
                    }
                }
            }
            _ => self.poly.crossings(yc, xs),
        }
        xs.sort_by(f64::total_cmp);
        for p in xs.chunks_exact(2) {
            if p[1] > p[0] {
                out.push((p[0], p[1]));
            }
        }
    }

    /// Most intervals the ring gives on a row.
    fn per_row(&self) -> f64 {
        if self.convex {
            1.0
        } else {
            (self.poly.pts.len() / 2).max(1) as f64
        }
    }
}

/// What a piece of a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    None,
    /// The outline, a nested outline or a wall; `true` = the outline
    /// band's inner half (inside its middle ring).
    Edge(bool),
    /// The interior.
    Interior,
    /// The interior inside a fill region (a pattern covering it).
    Full,
}

/// A piece of a row: `[a, b]`, its class and the copy it belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Piece {
    a: f64,
    b: f64,
    class: Class,
    copy: u16,
}

/// One edge of a copy's shape (or its hole) for edge brushes and dashes.
#[derive(Clone, Copy, Debug)]
struct EdgeRef {
    a: P2,
    b: P2,
    side: u8,
    /// Unit direction (grid space).
    dir: P2,
    /// Length of the outline before it (grid units).
    off: f64,
    /// The chord of the edge it belongs to, for positions along it (V09-S57
    /// areas; otherwise the copy's side segments).
    chord: Option<(P2, P2)>,
}

/// One copy of the shape in a scene: its edges, centre and frame.
#[derive(Clone, Debug)]
pub struct CopySpec {
    /// The shape (sides tagged) and its hole.
    pub base: Poly,
    pub hole: Option<Poly>,
    /// The centre and a point at the angle where "around" starts.
    pub centre: P2,
    pub corner: P2,
    /// Radii along its own axes and their rotation (radians): the
    /// "from the centre out" position is 1 on this ellipse.
    pub radii: (f64, f64, f64),
    /// The shape's sides as segments by side tag (V09-S56: a polygon's
    /// edges before its corners are cut), for [`GDir::Edge`]; empty = each
    /// edge of `base` is its own.
    pub side_segs: Vec<(P2, P2)>,
}

/// A copy, scanned.
#[derive(Clone, Debug)]
struct Copy2 {
    edges: std::ops::Range<usize>,
    centre: P2,
    theta0: f64,
    radii: (f64, f64, f64),
    side_segs: Vec<(P2, P2)>,
    /// A copy with many edges: its edges by row, for the nearest edge.
    index: Option<EdgeIndex>,
    /// An area copy (V09-S57): its index in the scene's areas.
    area: Option<u32>,
}

/// Copies with more edges than this find their nearest edge with an
/// [`EdgeIndex`] instead of trying every edge.
const INDEX_FROM: usize = 64;
/// Edges per leaf of an [`EdgeIndex`].
const LEAF: usize = 8;

/// A bounding-box tree over a copy's edges (V09-S56: a dense polygon's edge
/// pieces asked every edge for the nearest one — billions of tests on a
/// large twisted star with nested outlines). Exact: the same edge as
/// trying every edge in order (the lowest index of equals).
#[derive(Clone, Debug)]
struct EdgeIndex {
    /// Edge indices, each node's a contiguous run.
    ids: Vec<u32>,
    /// `[umin, vmin, umax, vmax]`, the run `[from, to)`, children (0 = a
    /// leaf; the root is node 0, never a child).
    nodes: Vec<([f64; 4], u32, u32, [u32; 2])>,
}

impl EdgeIndex {
    fn of(edges: &[EdgeRef], range: std::ops::Range<usize>) -> EdgeIndex {
        let bb = |e: &EdgeRef| {
            [
                e.a.0.min(e.b.0),
                e.a.1.min(e.b.1),
                e.a.0.max(e.b.0),
                e.a.1.max(e.b.1),
            ]
        };
        let mut ix = EdgeIndex {
            ids: range.map(|i| i as u32).collect(),
            nodes: Vec::new(),
        };
        // Built top-down with an explicit stack: (node, from, to).
        ix.nodes.push(([0.0; 4], 0, ix.ids.len() as u32, [0, 0]));
        let mut stack = vec![0usize];
        while let Some(k) = stack.pop() {
            let (_, from, to, _) = ix.nodes[k];
            let run = &mut ix.ids[from as usize..to as usize];
            let mut b = [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ];
            for &i in run.iter() {
                let q = bb(&edges[i as usize]);
                b = [
                    b[0].min(q[0]),
                    b[1].min(q[1]),
                    b[2].max(q[2]),
                    b[3].max(q[3]),
                ];
            }
            ix.nodes[k].0 = b;
            if run.len() <= LEAF {
                continue;
            }
            // Split at the median along the longer side.
            let axis = usize::from(b[3] - b[1] > b[2] - b[0]);
            let mid = run.len() / 2;
            let key = |i: &u32| {
                let e = &edges[*i as usize];
                if axis == 0 {
                    e.a.0 + e.b.0
                } else {
                    e.a.1 + e.b.1
                }
            };
            run.select_nth_unstable_by(mid, |x, y| key(x).total_cmp(&key(y)));
            let m = from + mid as u32;
            let (l, r) = (ix.nodes.len(), ix.nodes.len() + 1);
            ix.nodes.push(([0.0; 4], from, m, [0, 0]));
            ix.nodes.push(([0.0; 4], m, to, [0, 0]));
            ix.nodes[k].3 = [l as u32, r as u32];
            stack.push(l);
            stack.push(r);
        }
        ix
    }

    /// The edge nearest to `p`, the lowest index of equals. A node is
    /// skipped only when its box lies farther than the best edge so far.
    fn nearest(&self, edges: &[EdgeRef], p: P2) -> Option<usize> {
        let far = |b: &[f64; 4]| {
            let dx = (b[0] - p.0).max(p.0 - b[2]).max(0.0);
            let dy = (b[1] - p.1).max(p.1 - b[3]).max(0.0);
            dx.hypot(dy)
        };
        let mut best: Option<(f64, usize)> = None;
        let mut stack = vec![0u32];
        while let Some(k) = stack.pop() {
            let (b, from, to, kids) = &self.nodes[k as usize];
            if best.is_some_and(|(d, _)| far(b) > d) {
                continue;
            }
            if kids[0] == 0 {
                for &i in &self.ids[*from as usize..*to as usize] {
                    let e = &edges[i as usize];
                    let d = seg_dist(p, e.a, e.b);
                    let i = i as usize;
                    if best.is_none_or(|(bd, bi)| d < bd || (d == bd && i < bi)) {
                        best = Some((d, i));
                    }
                }
                continue;
            }
            // The nearer child last (popped first).
            let (l, r) = (kids[0], kids[1]);
            let (dl, dr) = (
                far(&self.nodes[l as usize].0),
                far(&self.nodes[r as usize].0),
            );
            if dl <= dr {
                stack.push(r);
                stack.push(l);
            } else {
                stack.push(l);
                stack.push(r);
            }
        }
        best.map(|(_, i)| i)
    }
}

/// A periodic pattern of windows along `u = alpha · x + beta` (grid units):
/// on where `u mod period` is in `[0, on)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XPat {
    pub alpha: f64,
    pub beta: f64,
    pub on: f64,
    pub period: f64,
}

/// The windows of an [`XPat`] inside `[a, b]`, in ascending x, lazily.
struct XIter {
    p: XPat,
    a: f64,
    b: f64,
    umin: f64,
    umax: f64,
    m: f64,
    m_end: f64,
    fwd: bool,
    last: f64,
    done: bool,
}

impl XIter {
    fn new(p: XPat, a: f64, b: f64) -> XIter {
        let (ua, ub) = (p.alpha * a + p.beta, p.alpha * b + p.beta);
        let (umin, umax) = (ua.min(ub), ua.max(ub));
        let m0 = (umin / p.period).floor();
        let m1 = (umax / p.period).floor();
        let fwd = p.alpha > 0.0;
        XIter {
            p,
            a,
            b,
            umin,
            umax,
            m: if fwd { m0 } else { m1 },
            m_end: if fwd { m1 } else { m0 },
            fwd,
            last: f64::NAN,
            done: !(m0.is_finite() && m1.is_finite() && b >= a),
        }
    }

    fn next(&mut self) -> Option<(f64, f64)> {
        loop {
            if self.done || (self.fwd && self.m > self.m_end) || (!self.fwd && self.m < self.m_end)
            {
                self.done = true;
                return None;
            }
            let ws = self.m * self.p.period;
            // Positions this far out no longer advance (or a window no
            // longer has a width) in floating point: the rest is not drawn
            // — the same in both passes.
            if ws == self.last || (ws + self.p.on).partial_cmp(&ws) != Some(Ordering::Greater) {
                self.done = true;
                return None;
            }
            self.last = ws;
            self.m += if self.fwd { 1.0 } else { -1.0 };
            let (w0, w1) = (ws.max(self.umin), (ws + self.p.on).min(self.umax));
            if w1 > w0 {
                let (x0, x1) = (
                    (w0 - self.p.beta) / self.p.alpha,
                    (w1 - self.p.beta) / self.p.alpha,
                );
                let (x0, x1) = (x0.min(x1).max(self.a), x0.max(x1).min(self.b));
                if x1 > x0 {
                    return Some((x0, x1));
                }
            }
        }
    }
}

/// Where a gradient's (or a density's) position runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GDir {
    /// Across the drawn extent, start to end.
    LeftRight,
    /// Across the drawn extent, bottom to top.
    BottomTop,
    /// Around the copy's centre, counter-clockwise from its first corner.
    Around,
    /// From the copy's centre (0) out to its radii (1).
    Radial,
    /// Along the nearest side of the copy, from its start (0) to its end
    /// (1) going round the shape counter-clockwise (V09-S56).
    Edge,
}

/// The directions' names (a gradient's choice control).
pub const GDIRS: &[&str] = &["Start to end", "Bottom to top", "Around the shape"];

impl GDir {
    pub fn of(i: usize) -> GDir {
        match i {
            1 => GDir::BottomTop,
            2 => GDir::Around,
            3 => GDir::Radial,
            4 => GDir::Edge,
            _ => GDir::LeftRight,
        }
    }
}

/// A density: the share of cells kept (0 – 1) along a direction, by a
/// curve; cells are kept by an even dither (outline cells by their
/// position along the outline, interior cells by their column and row).
#[derive(Clone, Debug)]
pub struct Density {
    pub curve: Curve,
    pub by: GDir,
}

/// How a scene is drawn.
#[derive(Clone, Debug, Default)]
pub struct Style {
    /// The interior drawn where the patterns are not (all of it without
    /// patterns).
    pub filled: bool,
    /// Rows `(r0 + k · s)` of the interior drawn whole: (r0, s).
    pub row_stripes: Vec<(i64, i64)>,
    /// Interior window patterns (`alpha` = 1, `beta` may depend on the row:
    /// `beta + row_k · (row − r0)`): (pattern, r0, row_k).
    pub x_stripes: Vec<(XPat, i64, f64)>,
    /// Edge pieces: per side, the subchannel position (side `s` takes entry
    /// `s mod len`; V09-S56: any number of sides).
    pub subs: Option<Vec<u32>>,
    /// Edge pieces of the outline band: the subchannel position of its
    /// outer and inner half (a scene with middle rings).
    pub half_subs: Option<(u32, u32)>,
    /// Edge pieces: dashes along the edge — on (cells), period per side
    /// (cells; side `s` takes entry `s mod len`, never empty), phase
    /// (cells along the outline).
    pub edge_dash: Option<(f64, Vec<f64>, f64)>,
    /// Velocity / gate / subchannel gradients and their directions.
    pub grads: Vec<(Grad, GDir)>,
    /// Densities (multiplied) keeping a share of the cells.
    pub density: Vec<Density>,
    /// Edge cells: dashes along their copy's outline by its arc (the
    /// angle round its centre times its mean radius) — on (cells), period
    /// (cells), phase (cells); for round shapes, cell by cell.
    pub arc_dash: Option<(f64, f64, f64)>,
    /// Cells whose centre lies within a radius of a point take a velocity
    /// (V09-S56: a polygon's vertex accents).
    pub accent: Option<CellAccent>,
    /// Edge pieces: per side (entry `side mod len`), a gradient along that
    /// side from its start to its end (V09-S57: per-vertex values).
    pub side_grads: Option<Vec<Grad>>,
    /// Edge cells: per side (entry `side mod len`), the share kept at its
    /// start and at its end (V09-S57: per-vertex densities; even dither).
    pub side_density: Option<Vec<(f64, f64)>>,
    /// Gradients whose position goes through a curve first (V09-S57: a
    /// gate profile).
    pub shaped: Vec<(Grad, GDir, Curve)>,
}

/// Accented cells: those whose centre lies within `radius` grid units of
/// one of the points get velocity `vel` (over the gradients).
#[derive(Clone, Debug, Default)]
pub struct CellAccent {
    pub vel: u8,
    pub radius: f64,
    /// The points, by row coordinate ascending.
    pts: Vec<P2>,
}

impl CellAccent {
    /// Accents at `pts` (grid space; not finite ones dropped).
    pub fn new(vel: u8, radius: f64, pts: impl IntoIterator<Item = P2>) -> CellAccent {
        let mut pts: Vec<P2> = pts
            .into_iter()
            .filter(|p| p.0.is_finite() && p.1.is_finite())
            .collect();
        pts.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));
        CellAccent {
            vel: vel.clamp(1, 127),
            radius: if radius.is_finite() {
                radius.max(0.0)
            } else {
                0.0
            },
            pts,
        }
    }

    /// How many points accent.
    pub fn points(&self) -> usize {
        self.pts.len()
    }

    /// The accented cells of the row whose centre is `yc`, as disjoint
    /// whole-cell windows `[s, e)` ascending, into `out`.
    fn windows(&self, yc: f64, out: &mut Vec<(f64, f64)>) {
        out.clear();
        let r = self.radius;
        let from = self.pts.partition_point(|p| p.1 < yc - r);
        for &(x, y) in &self.pts[from..] {
            if y > yc + r {
                break;
            }
            let hw = (r * r - (y - yc) * (y - yc)).max(0.0).sqrt();
            // Cells c with |c + ½ − x| ≤ hw.
            let s = (x - hw - 0.5).ceil();
            let e = (x + hw - 0.5).floor() + 1.0;
            if e > s && s.is_finite() && e.is_finite() {
                out.push((s, e));
            }
        }
        out.sort_unstable_by(|p, q| p.0.total_cmp(&q.0));
        let mut k = 0;
        for i in 0..out.len() {
            let w = out[i];
            if k > 0 && w.0 <= out[k - 1].1 {
                out[k - 1].1 = out[k - 1].1.max(w.1);
            } else {
                out[k] = w;
                k += 1;
            }
        }
        out.truncate(k);
    }
}

impl Style {
    fn varies_along_x(&self) -> bool {
        self.grads
            .iter()
            .any(|(g, d)| g.varies() && *d != GDir::BottomTop)
            || self
                .shaped
                .iter()
                .any(|(g, d, _)| g.varies() && *d != GDir::BottomTop)
            || self.side_grads.is_some()
    }

    /// Whether any gradient changes a brush.
    fn brushes(&self) -> bool {
        !self.grads.is_empty() || !self.shaped.is_empty() || self.side_grads.is_some()
    }
}

/// Most parts a piece is cut into for the densities and arc dashes: one
/// per cell up to this many (every piece of a round shape of radius ≤ 1,024
/// units is filtered cell by cell), longer parts beyond — so a huge shape
/// costs this many tests per piece, not its width.
pub const DENSITY_PARTS: usize = 4096;

/// Rings, the shape's edges and line features: a closed shape ready to be
/// scanned (see the module's documentation).
#[derive(Clone, Debug, Default)]
pub struct Scene {
    rings: Vec<Ring>,
    edges: Vec<EdgeRef>,
    copies: Vec<Copy2>,
    /// Line features of the interior (grid space).
    lines: Vec<(P2, P2)>,
    /// The lines by their lowest row (their active table's order).
    line_order: Vec<u32>,
    /// The drawn extent `[umin, vmin, umax, vmax]` (the outer rings').
    bbox: [f64; 4],
    /// Whether mask rings limit what is drawn.
    masked: bool,
    /// General areas instead of rings (V09-S57).
    areas: Option<Box<Areas>>,
}

/// One copy of an area geometry (V09-S57): the geometry, where its local
/// origin lies and whether it is mirrored left–right about it; its centre,
/// first corner and radii (local) for gradients round it.
#[derive(Clone, Debug)]
pub struct AreaCopy {
    pub geom: usize,
    pub at: P2,
    pub flip: bool,
    pub centre: P2,
    pub corner: P2,
    pub radii: (f64, f64, f64),
}

impl AreaCopy {
    fn to_local(&self, p: P2) -> P2 {
        let x = p.0 - self.at.0;
        (if self.flip { -x } else { x }, p.1 - self.at.1)
    }

    fn to_global(&self, q: P2) -> P2 {
        (
            self.at.0 + if self.flip { -q.0 } else { q.0 },
            self.at.1 + q.1,
        )
    }

    /// Local intervals of a row as global ones, ascending, into `out`.
    fn place(&self, local: &[(f64, f64)], out: &mut Vec<(f64, f64)>) {
        out.clear();
        if self.flip {
            out.extend(
                local
                    .iter()
                    .rev()
                    .map(|&(a, b)| (self.at.0 - b, self.at.0 - a)),
            );
        } else {
            out.extend(local.iter().map(|&(a, b)| (self.at.0 + a, self.at.0 + b)));
        }
    }
}

/// A scene's areas: the geometries, their copies (by lowest row), each
/// geometry's edges for brushes and their index.
#[derive(Clone, Debug)]
struct Areas {
    geoms: Vec<Geom>,
    copies: Vec<AreaCopy>,
    /// Copies by their lowest row, and each one's rows.
    order: Vec<u32>,
    vr: Vec<(f64, f64)>,
    edges: Vec<Vec<EdgeRef>>,
    index: Vec<Option<EdgeIndex>>,
}

/// Reused buffers of a row scan.
#[derive(Default)]
struct ScanBuf {
    aet: Vec<Aet>,
    /// The lines' active table (rows ascend).
    lines: Aet,
    ev: Vec<(f64, usize, i32, u16)>,
    xs: Vec<f64>,
    spans: Vec<(f64, f64)>,
    open: Vec<u16>,
    /// Areas (V09-S57): the copies' active table and scan states, buffers.
    a_act: Aet,
    a_st: Vec<RowState>,
    a_buf: RowBuf,
    a_e: Vec<(f64, f64)>,
    a_i: Vec<(f64, f64)>,
    a_g: Vec<(f64, f64)>,
    a_done: Vec<(f64, f64)>,
    a_in: Vec<(f64, f64)>,
    a_t: Vec<(f64, f64)>,
    a_ints: Vec<(u16, f64, f64)>,
}

impl Scene {
    /// A scene of one copy — `base` (the shape, sides tagged) and `hole` —
    /// with convex rings: see [`Scene::with_copies`].
    pub fn new(
        base: &Poly,
        hole: Option<&Poly>,
        rings: Vec<(Poly, Role)>,
        lines: Vec<(P2, P2)>,
        centre: P2,
        corner: P2,
    ) -> Scene {
        Scene::with_copies(
            vec![CopySpec {
                base: base.clone(),
                hole: hole.cloned(),
                centre,
                corner,
                radii: (1.0, 1.0, 0.0),
                side_segs: Vec::new(),
            }],
            rings
                .into_iter()
                .map(|(p, r)| RingSpec::convex(p, r))
                .collect(),
            lines,
        )
    }

    /// A scene of copies of a shape (each with its edges for edge brushes
    /// and dashes, its centre and frame for gradients around / out from
    /// it), rings with roles and copies, and line features. Empty rings are
    /// dropped.
    pub fn with_copies(copies: Vec<CopySpec>, rings: Vec<RingSpec>, lines: Vec<(P2, P2)>) -> Scene {
        let rings: Vec<Ring> = rings
            .into_iter()
            .filter(|r| !r.poly.is_empty())
            .map(|r| {
                let b = r.poly.bbox();
                Ring {
                    chains: if r.convex { Chains::of(&r.poly) } else { None },
                    order: if r.convex {
                        Vec::new()
                    } else {
                        let p = &r.poly.pts;
                        let lo = |k: usize| p[k].1.min(p[(k + 1) % p.len()].1);
                        let mut o: Vec<u32> = (0..p.len() as u32).collect();
                        o.sort_by(|&a, &b| {
                            lo(a as usize).total_cmp(&lo(b as usize)).then(a.cmp(&b))
                        });
                        o
                    },
                    poly: r.poly,
                    role: r.role,
                    copy: r.copy,
                    convex: r.convex,
                    vmin: b[1],
                    vmax: b[3],
                }
            })
            .collect();
        let mut edges = Vec::new();
        let mut cs = Vec::new();
        for c in &copies {
            let from = edges.len();
            for p in std::iter::once(&c.base).chain(c.hole.as_ref()) {
                let mut off = 0.0;
                for i in 0..p.pts.len() {
                    let (a, b) = p.edge(i);
                    let l = sub(b, a);
                    edges.push(EdgeRef {
                        a,
                        b,
                        side: p.sides[i],
                        dir: unit(l),
                        off,
                        chord: None,
                    });
                    off += l.0.hypot(l.1);
                }
            }
            let range = from..edges.len();
            cs.push(Copy2 {
                index: (range.len() > INDEX_FROM).then(|| EdgeIndex::of(&edges, range.clone())),
                edges: range,
                centre: c.centre,
                theta0: (c.corner.1 - c.centre.1).atan2(c.corner.0 - c.centre.0),
                radii: c.radii,
                side_segs: c.side_segs.clone(),
                area: None,
            });
        }
        let bbox = rings.iter().filter(|r| r.role == Role::Outer).fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |b, r| {
                let q = r.poly.bbox();
                [
                    b[0].min(q[0]),
                    b[1].min(q[1]),
                    b[2].max(q[2]),
                    b[3].max(q[3]),
                ]
            },
        );
        let mut line_order: Vec<u32> = (0..lines.len() as u32).collect();
        line_order.sort_by(|&a, &b| {
            let (p, q) = (lines[a as usize], lines[b as usize]);
            p.0.1
                .min(p.1.1)
                .total_cmp(&q.0.1.min(q.1.1))
                .then(a.cmp(&b))
        });
        Scene {
            masked: rings.iter().any(|r| r.role == Role::Mask),
            areas: None,
            rings,
            edges,
            copies: cs,
            lines,
            line_order,
            bbox,
        }
    }

    /// Whether nothing can be drawn (no outer ring).
    pub fn is_empty(&self) -> bool {
        !self.bbox[0].is_finite()
    }

    /// How many rings the scene has.
    pub fn rings(&self) -> usize {
        self.rings.len()
    }

    /// A scene of general areas (V09-S57): `geoms` placed by `copies`
    /// (scanned instead of rings; brushes by each geometry's boundary
    /// pieces and strokes, gradients round each copy's centre).
    pub fn with_areas(geoms: Vec<Geom>, copies: Vec<AreaCopy>) -> Scene {
        let edges: Vec<Vec<EdgeRef>> = geoms
            .iter()
            .map(|g| {
                g.edges
                    .iter()
                    .map(|e| {
                        let d = sub(e.b, e.a);
                        EdgeRef {
                            a: e.a,
                            b: e.b,
                            side: e.tag,
                            dir: unit(d),
                            off: e.off,
                            chord: Some(e.chord),
                        }
                    })
                    .collect()
            })
            .collect();
        let index = edges
            .iter()
            .map(|es| (es.len() > INDEX_FROM).then(|| EdgeIndex::of(es, 0..es.len())))
            .collect();
        let mut bbox = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let mut cs = Vec::with_capacity(copies.len());
        let mut vr = Vec::with_capacity(copies.len());
        for (k, c) in copies.iter().enumerate() {
            let b = geoms.get(c.geom).map_or([f64::NAN; 4], |g| g.bbox);
            if b[0].is_finite() {
                let (p, q) = (c.to_global((b[0], b[1])), c.to_global((b[2], b[3])));
                bbox = [
                    bbox[0].min(p.0.min(q.0)),
                    bbox[1].min(p.1),
                    bbox[2].max(p.0.max(q.0)),
                    bbox[3].max(q.1),
                ];
                vr.push((p.1, q.1));
            } else {
                vr.push((f64::INFINITY, f64::NEG_INFINITY));
            }
            let (centre, corner) = (c.to_global(c.centre), c.to_global(c.corner));
            cs.push(Copy2 {
                edges: 0..0,
                centre,
                theta0: (corner.1 - centre.1).atan2(corner.0 - centre.0),
                radii: c.radii,
                side_segs: Vec::new(),
                index: None,
                area: Some(k as u32),
            });
        }
        let mut order: Vec<u32> = (0..copies.len() as u32).collect();
        order.sort_by(|&a, &b| {
            vr[a as usize]
                .0
                .total_cmp(&vr[b as usize].0)
                .then(a.cmp(&b))
        });
        Scene {
            rings: Vec::new(),
            edges: Vec::new(),
            copies: cs,
            lines: Vec::new(),
            line_order: Vec::new(),
            bbox,
            masked: false,
            areas: Some(Box::new(Areas {
                geoms,
                copies,
                order,
                vr,
                edges,
                index,
            })),
        }
    }

    /// The row's pieces of an area scene: each active copy's edge and
    /// interior intervals; an edge covers everything else and a copy's
    /// pieces cover the later copies' (in copy order).
    fn area_pieces(&self, ar: &Areas, row: i64, sb: &mut ScanBuf, out: &mut Vec<Piece>) {
        let yc = row as f64 + 0.5;
        if sb.a_st.len() != ar.copies.len() {
            sb.a_st = vec![RowState::default(); ar.copies.len()];
            sb.a_act = Aet {
                last: f64::NEG_INFINITY,
                ..Aet::default()
            };
        }
        let st = &mut sb.a_act;
        if yc.partial_cmp(&st.last).is_none_or(|o| o == Ordering::Less) {
            st.next = 0;
            st.active.clear();
        }
        st.last = yc;
        while st.next < ar.order.len() && ar.vr[ar.order[st.next] as usize].0 <= yc {
            st.active.push(ar.order[st.next]);
            st.next += 1;
        }
        st.active.retain(|&k| ar.vr[k as usize].1 >= yc);
        st.active.sort_unstable();
        let active = std::mem::take(&mut st.active);
        sb.a_done.clear();
        sb.a_ints.clear();
        for &k in &active {
            let c = &ar.copies[k as usize];
            let Some(g) = ar.geoms.get(c.geom) else {
                continue;
            };
            g.row(
                yc - c.at.1,
                &mut sb.a_st[k as usize],
                &mut sb.a_buf,
                &mut sb.a_e,
                &mut sb.a_i,
            );
            // Edges: what earlier copies' edges left.
            c.place(&sb.a_e, &mut sb.a_g);
            area::subtract(&sb.a_g, &sb.a_done, &mut sb.a_t);
            for &(a, b) in &sb.a_t {
                out.push(Piece {
                    a,
                    b,
                    class: Class::Edge(false),
                    copy: k as u16,
                });
            }
            area::union(&sb.a_done, &sb.a_g, &mut sb.a_t);
            std::mem::swap(&mut sb.a_done, &mut sb.a_t);
            c.place(&sb.a_i, &mut sb.a_g);
            for &(a, b) in &sb.a_g {
                sb.a_ints.push((k as u16, a, b));
            }
        }
        // Interiors: what no edge and no earlier copy's interior covers.
        sb.a_in.clear();
        let mut i = 0;
        while i < sb.a_ints.len() {
            let k = sb.a_ints[i].0;
            sb.a_g.clear();
            while i < sb.a_ints.len() && sb.a_ints[i].0 == k {
                sb.a_g.push((sb.a_ints[i].1, sb.a_ints[i].2));
                i += 1;
            }
            area::subtract(&sb.a_g, &sb.a_done, &mut sb.a_t);
            area::subtract(&sb.a_t, &sb.a_in, &mut sb.a_e);
            for &(a, b) in &sb.a_e {
                out.push(Piece {
                    a,
                    b,
                    class: Class::Interior,
                    copy: k,
                });
            }
            area::union(&sb.a_in, &sb.a_g, &mut sb.a_t);
            std::mem::swap(&mut sb.a_in, &mut sb.a_t);
        }
        sb.a_act.active = active;
        out.sort_by(|p, q| p.a.total_cmp(&q.a));
        out.retain(|p| p.b > p.a);
    }

    /// The rows the scene covers inside `band`: first, last (inclusive).
    fn rows(&self, band: Band) -> Option<(i64, i64)> {
        if self.is_empty() {
            return None;
        }
        // Rows whose centre r + ½ lies in [vmin, vmax).
        let r0 = ((self.bbox[1] - 0.5).ceil()).max(band.lo as f64);
        let r1 = ((self.bbox[3] - 0.5).ceil() - 1.0).min((band.hi - 1) as f64);
        (r0.is_finite() && r1.is_finite() && r0 <= r1).then_some((r0 as i64, r1 as i64))
    }

    /// The row's pieces (merged by class and copy), ascending.
    fn pieces(&self, row: i64, sb: &mut ScanBuf, out: &mut Vec<Piece>) {
        out.clear();
        if let Some(ar) = &self.areas {
            return self.area_pieces(ar, row, sb, out);
        }
        sb.ev.clear();
        let yc = row as f64 + 0.5;
        if sb.aet.len() != self.rings.len() {
            sb.aet = vec![
                Aet {
                    last: f64::NEG_INFINITY,
                    ..Aet::default()
                };
                self.rings.len()
            ];
        }
        for (r, st) in self.rings.iter().zip(sb.aet.iter_mut()) {
            sb.spans.clear();
            r.spans(yc, &mut sb.xs, &mut sb.spans, Some(st));
            for &(a, b) in &sb.spans {
                sb.ev.push((a, r.role as usize, 1, r.copy));
                sb.ev.push((b, r.role as usize, -1, r.copy));
            }
        }
        if sb.ev.is_empty() {
            return;
        }
        sb.ev.sort_unstable_by(|p, q| p.0.total_cmp(&q.0));
        sb.open.clear();
        let mut cnt = [0i32; ROLES];
        let mut i = 0;
        let ev = &sb.ev;
        while i < ev.len() {
            let x = ev[i].0;
            while i < ev.len() && ev[i].0 == x {
                let (_, role, d, copy) = ev[i];
                cnt[role] += d;
                if role == Role::Outer as usize {
                    if d > 0 {
                        sb.open.push(copy);
                    } else if let Some(k) = sb.open.iter().rposition(|&c| c == copy) {
                        sb.open.remove(k);
                    }
                }
                i += 1;
            }
            let Some(&(nx, _, _, _)) = ev.get(i) else {
                break;
            };
            let n = |r: Role| cnt[r as usize];
            let in_hole = n(Role::HoleOuter) > 0;
            let band = n(Role::Outer) > n(Role::Inner);
            let hole_band = n(Role::HoleInner) > n(Role::HoleOuter) && n(Role::Outer) > 0;
            let inner = n(Role::Inner) > 0 && n(Role::HoleInner) <= 0;
            let half = n(Role::Mid) > 0;
            let c = if in_hole || (self.masked && n(Role::Mask) <= 0) {
                Class::None
            } else if band || hole_band {
                Class::Edge(half)
            } else if inner {
                if n(Role::NestOuter) > n(Role::NestInner) || n(Role::Wall) > 0 {
                    Class::Edge(false)
                } else if n(Role::Fill) > 0 {
                    Class::Full
                } else {
                    Class::Interior
                }
            } else {
                Class::None
            };
            let copy = sb.open.last().copied().unwrap_or(0);
            match out.last_mut() {
                Some(l) if l.class == c && l.copy == copy && l.b == x => l.b = nx,
                _ => out.push(Piece {
                    a: x,
                    b: nx,
                    class: c,
                    copy,
                }),
            }
        }
        out.retain(|p| p.class != Class::None && p.b > p.a);
    }

    /// The edge of copy `copy` nearest to `p`.
    fn nearest(&self, copy: u16, p: P2) -> Option<EdgeRef> {
        let c = self.copies.get(usize::from(copy))?;
        if let (Some(k), Some(ar)) = (c.area, &self.areas) {
            let ac = &ar.copies[k as usize];
            let es = ar.edges.get(ac.geom)?;
            let q = ac.to_local(p);
            let i = match &ar.index[ac.geom] {
                Some(ix) => ix.nearest(es, q)?,
                None => (0..es.len()).min_by(|&i, &j| {
                    seg_dist(q, es[i].a, es[i].b)
                        .partial_cmp(&seg_dist(q, es[j].a, es[j].b))
                        .unwrap_or(Ordering::Equal)
                })?,
            };
            let e = es[i];
            return Some(EdgeRef {
                a: ac.to_global(e.a),
                b: ac.to_global(e.b),
                dir: if ac.flip { (-e.dir.0, e.dir.1) } else { e.dir },
                chord: e.chord.map(|(a, b)| (ac.to_global(a), ac.to_global(b))),
                ..e
            });
        }
        if let Some(ix) = &c.index {
            return ix.nearest(&self.edges, p).map(|i| self.edges[i]);
        }
        let r = c.edges.clone();
        self.edges[r]
            .iter()
            .min_by(|e, f| {
                seg_dist(p, e.a, e.b)
                    .partial_cmp(&seg_dist(p, f.a, f.b))
                    .unwrap_or(Ordering::Equal)
            })
            .copied()
    }

    /// The line features' windows on row `row`, each at least one cell,
    /// ascending (clipped to `[a, b]` later). Only the lines reaching the
    /// row are visited (an active table: rows ascend; out of order it
    /// restarts).
    fn line_windows(&self, row: i64, st: &mut Aet, out: &mut Vec<(f64, f64)>) {
        out.clear();
        let (y0, y1) = (row as f64, row as f64 + 1.0);
        if y0.partial_cmp(&st.last).is_none_or(|o| o == Ordering::Less) {
            st.next = 0;
            st.active.clear();
        }
        st.last = y0;
        let lo = |k: u32| {
            let (p, q) = self.lines[k as usize];
            p.1.min(q.1)
        };
        let hi = |k: u32| {
            let (p, q) = self.lines[k as usize];
            p.1.max(q.1)
        };
        while st.next < self.line_order.len() && lo(self.line_order[st.next]) <= y1 {
            st.active.push(self.line_order[st.next]);
            st.next += 1;
        }
        st.active.retain(|&k| hi(k) >= y0);
        for &k in &st.active {
            let (p, q) = self.lines[k as usize];
            let dy = q.1 - p.1;
            let (ta, tb) = if dy == 0.0 {
                if p.1 >= y0 && p.1 < y1 {
                    (0.0, 1.0)
                } else {
                    continue;
                }
            } else {
                let (u, v) = ((y0 - p.1) / dy, (y1 - p.1) / dy);
                (u.min(v).max(0.0), u.max(v).min(1.0))
            };
            if !matches!(
                tb.partial_cmp(&ta),
                Some(Ordering::Greater | Ordering::Equal)
            ) {
                continue;
            }
            let (xa, xb) = (p.0 + (q.0 - p.0) * ta, p.0 + (q.0 - p.0) * tb);
            let s = xa.min(xb).round();
            let e = xa.max(xb).round().max(s + 1.0);
            if s.is_finite() && e.is_finite() {
                out.push((s, e));
            }
        }
        out.sort_unstable_by(|p, q| p.0.total_cmp(&q.0).then(p.1.total_cmp(&q.1)));
        // Their union (what the interior draws of them): disjoint, ascending.
        let mut k = 0;
        for i in 0..out.len() {
            let w = out[i];
            if k > 0 && w.0 <= out[k - 1].1 {
                out[k - 1].1 = out[k - 1].1.max(w.1);
            } else {
                out[k] = w;
                k += 1;
            }
        }
        out.truncate(k);
    }

    /// The position of point `(u, yc)` of copy `copy` along direction `d`
    /// (0 – 1; outside the extent beyond).
    fn pos(&self, d: GDir, copy: u16, u: f64, yc: f64) -> f64 {
        let b = &self.bbox;
        let r = |v: f64, lo: f64, hi: f64| if hi > lo { (v - lo) / (hi - lo) } else { 0.0 };
        let Some(c) = self.copies.get(usize::from(copy)) else {
            return 0.0;
        };
        match d {
            GDir::LeftRight => r(u, b[0], b[2]),
            GDir::BottomTop => r(yc, b[1], b[3]),
            GDir::Around => {
                let a = (yc - c.centre.1).atan2(u - c.centre.0);
                (a - c.theta0).rem_euclid(TAU) / TAU
            }
            GDir::Radial => {
                let (rx, ry, rot) = c.radii;
                let (s, co) = rot.sin_cos();
                let (dx, dy) = (u - c.centre.0, yc - c.centre.1);
                let (x, y) = (dx * co + dy * s, -dx * s + dy * co);
                let q = (x / rx.max(1e-9)).hypot(y / ry.max(1e-9));
                if q.is_finite() { q } else { 0.0 }
            }
            GDir::Edge => match self.nearest(copy, (u, yc)) {
                Some(e) => Scene::along(c, &e, u, yc),
                None => 0.0,
            },
        }
    }

    /// The position of point `(u, yc)` along edge `e`'s side of copy `c`
    /// (its chord, or the copy's side segment, or the edge itself), from
    /// its start (0) to its end (1).
    fn along(c: &Copy2, e: &EdgeRef, u: f64, yc: f64) -> f64 {
        let (a, b) = e
            .chord
            .or_else(|| c.side_segs.get(usize::from(e.side)).copied())
            .unwrap_or((e.a, e.b));
        let d = sub(b, a);
        let l2 = d.0 * d.0 + d.1 * d.1;
        if l2 > 0.0 {
            ((u - a.0) * d.0 + (yc - a.1) * d.1) / l2
        } else {
            0.0
        }
    }

    /// The nearest edge of copy `copy` to `(u, yc)` and the position along
    /// its side (one search for both).
    fn edge_at(&self, copy: u16, u: f64, yc: f64) -> Option<(EdgeRef, f64)> {
        let c = self.copies.get(usize::from(copy))?;
        let e = self.nearest(copy, (u, yc))?;
        let s = Scene::along(c, &e, u, yc);
        Some((e, s))
    }

    /// The share of cells kept at point `(u, yc)` of copy `copy`.
    fn kept(&self, style: &Style, copy: u16, u: f64, yc: f64) -> f64 {
        style.density.iter().fold(1.0, |k, d| {
            k * d
                .curve
                .eval(self.pos(d.by, copy, u, yc).clamp(0.0, 1.0))
                .clamp(0.0, 1.0)
        })
    }

    /// Position of an outline cell along its copy's outline (cells), for
    /// the density dither: the angle around the copy times its mean radius.
    fn arc_pos(&self, copy: u16, u: f64, yc: f64) -> f64 {
        let Some(c) = self.copies.get(usize::from(copy)) else {
            return u;
        };
        let a = (yc - c.centre.1).atan2(u - c.centre.0);
        (a - c.theta0).rem_euclid(TAU) * (c.radii.0.abs() + c.radii.1.abs()) / 2.0
    }

    /// Draws the scene with `style` and the base brush `base` (`step`:
    /// ticks per grid unit; rows clipped to `band`); false as soon as the
    /// pen is.
    pub fn draw(
        &self,
        style: &Style,
        base: Brush,
        step: f64,
        band: Band,
        pen: &mut dyn Draw,
    ) -> bool {
        let Some((r0, r1)) = self.rows(band) else {
            return true;
        };
        let mut sb = ScanBuf::default();
        let mut ps = Vec::new();
        let mut lw = Vec::new();
        let mut aw = Vec::new();
        let mut it: Vec<XIter> = Vec::new();
        for row in r0..=r1 {
            self.pieces(row, &mut sb, &mut ps);
            if ps.is_empty() {
                continue;
            }
            let yc = row as f64 + 0.5;
            if let Some(acc) = &style.accent {
                acc.windows(yc, &mut aw);
            }
            lw.clear();
            if !self.lines.is_empty() && ps.iter().any(|p| p.class == Class::Interior) {
                self.line_windows(row, &mut sb.lines, &mut lw);
            }
            let full_row = style
                .row_stripes
                .iter()
                .any(|&(a, s)| s > 0 && (row - a).rem_euclid(s) == 0);
            let mut out = RowOut {
                pen: &mut *pen,
                scene: self,
                style,
                row,
                yc,
                step,
                end: f64::NEG_INFINITY,
                pend: None,
                copy: 0,
                edge: false,
                acc: &aw,
                acc_vel: None,
                fixed: None,
                side_off: false,
            };
            for p in &ps {
                let (a, b) = (p.a, p.b);
                out.copy = p.copy;
                out.edge = matches!(p.class, Class::Edge(_));
                let ok = match p.class {
                    Class::Edge(half) => out.edge_piece(a, b, base, half),
                    Class::Full => style.filled || out.put(a, b, base),
                    Class::Interior => {
                        let patterned = full_row || !lw.is_empty() || !style.x_stripes.is_empty();
                        if !patterned {
                            !style.filled || out.put(a, b, base)
                        } else if full_row {
                            // The pattern covers the piece.
                            style.filled || out.put(a, b, base)
                        } else {
                            out.interior(a, b, base, &lw, &mut it)
                        }
                    }
                    Class::None => true,
                };
                if !ok {
                    return false;
                }
            }
            if !out.flush() {
                return false;
            }
        }
        true
    }

    /// The least common period (cells) of the stripe patterns (a union of
    /// them repeats after it), capped.
    fn lcm(style: &Style) -> f64 {
        fn gcd(a: u64, b: u64) -> u64 {
            if b == 0 { a } else { gcd(b, a % b) }
        }
        let mut l: u64 = 1;
        for (p, _, _) in &style.x_stripes {
            let q = p.period.round().max(1.0) as u64;
            l = (l / gcd(l, q)).saturating_mul(q).min(1 << 40);
        }
        l as f64
    }

    /// An upper bound of the notes [`Scene::draw`] makes (cheap: no row is
    /// scanned). Per row: the pieces the rings give, the windows of the
    /// patterns over the drawn extent — and never more than the disjoint
    /// whole cells of the extent.
    pub fn bound(&self, style: &Style, band: Band) -> f64 {
        let Some((r0, r1)) = self.rows(band) else {
            return 0.0;
        };
        let rows = (r1 - r0 + 1) as f64;
        let w = (self.bbox[2] - self.bbox[0]).max(0.0) + 3.0;
        let mut e = 2.0 * self.rings.iter().map(Ring::per_row).sum::<f64>() + 1.0;
        if let Some(ar) = &self.areas {
            e += 2.0
                * ar.copies
                    .iter()
                    .map(|c| ar.geoms.get(c.geom).map_or(0.0, Geom::per_row))
                    .sum::<f64>();
        }
        let mut per = e;
        if let Some((_, periods, _)) = &style.edge_dash {
            let p = periods
                .iter()
                .fold(f64::INFINITY, |a, &b| a.min(b))
                .max(1.0);
            per += w / p + 2.0 * e;
        }
        for (p, _, _) in &style.x_stripes {
            per += w / p.period.max(1.0) + 2.0 * e;
        }
        per += 2.0 * self.lines.len() as f64 + e;
        if let Some(acc) = &style.accent {
            // Each accented window cuts a piece in at most three.
            per += 2.0 * acc.points() as f64;
        }
        let per = if style.varies_along_x()
            || !style.density.is_empty()
            || style.arc_dash.is_some()
            || style.side_density.is_some()
        {
            w
        } else {
            per.min(w)
        };
        rows * per
    }

    /// A lower bound of the notes [`Scene::draw`] makes, for the patterns
    /// whose output grows with the shape's width: on every row, each
    /// piece's windows of a single pattern lying wholly inside it (less a
    /// margin for the neighbours and line features they may join). None
    /// with a density (it removes cells).
    pub fn floor(&self, style: &Style, band: Band) -> f64 {
        let Some((r0, r1)) = self.rows(band) else {
            return 0.0;
        };
        if !style.density.is_empty() || style.arc_dash.is_some() || style.side_density.is_some() {
            return 0.0;
        }
        let interior_single = match style.x_stripes.len() {
            1 => Some(style.x_stripes[0].0.period),
            // Crosshatch (two diagonal families of one spacing ≥ 3): the
            // windows of one family stay apart.
            2 if style.x_stripes[0].0.period == style.x_stripes[1].0.period
                && style.x_stripes[0].0.period >= 3.0
                && style.x_stripes[0].0.alpha == style.x_stripes[1].0.alpha =>
            {
                Some(style.x_stripes[0].0.period)
            }
            _ => None,
        };
        let mut sb = ScanBuf::default();
        let mut ps = Vec::new();
        let mut lw = Vec::new();
        let mut total = 0.0;
        for row in r0..=r1 {
            if style
                .row_stripes
                .iter()
                .any(|&(a, s)| s > 0 && (row - a).rem_euclid(s) == 0)
            {
                continue;
            }
            self.pieces(row, &mut sb, &mut ps);
            lw.clear();
            if interior_single.is_some()
                && !self.lines.is_empty()
                && ps.iter().any(|p| p.class == Class::Interior)
            {
                self.line_windows(row, &mut sb.lines, &mut lw);
            }
            let yc = row as f64 + 0.5;
            for p in &ps {
                let (a, b) = (p.a, p.b);
                let n = match p.class {
                    Class::Interior => interior_single.map_or(0.0, |per| {
                        // What the line features cover joins the stripes
                        // under it.
                        let covered: f64 = meeting(&lw, a, b)
                            .iter()
                            .map(|&(s, e)| (e.min(b) - s.max(a)).max(0.0) + 2.0)
                            .sum();
                        ((b - a - covered) / per).floor() - 8.0
                    }),
                    Class::Edge(_) => match &style.edge_dash {
                        Some((on, periods, _)) => {
                            let Some(e) = self.nearest(p.copy, ((a + b) / 2.0, yc)) else {
                                continue;
                            };
                            let per = periods
                                .get(usize::from(e.side) % periods.len().max(1))
                                .copied()
                                .unwrap_or(1.0);
                            if per > *on && e.dir.0.abs() >= e.dir.1.abs() {
                                ((b - a) * e.dir.0.abs() / per).floor() - 2.0
                            } else {
                                0.0
                            }
                        }
                        None => 0.0,
                    },
                    Class::Full | Class::None => 0.0,
                };
                total += n.max(0.0);
            }
        }
        total
    }
}

/// The windows of a row's line union (disjoint, ascending) meeting
/// `[a, b]`, found by binary search.
fn meeting(lw: &[(f64, f64)], a: f64, b: f64) -> &[(f64, f64)] {
    let from = lw.partition_point(|w| w.1 <= a);
    let to = lw.partition_point(|w| w.0 < b).max(from);
    &lw[from..to]
}

/// One row's output: pieces rounded to whole cells, kept disjoint, cut by
/// the gradients, thinned by the densities, merged when touching with one
/// brush.
struct RowOut<'a> {
    pen: &'a mut dyn Draw,
    scene: &'a Scene,
    style: &'a Style,
    row: i64,
    yc: f64,
    step: f64,
    /// The end (cells) of the last piece.
    end: f64,
    pend: Option<(f64, f64, Brush)>,
    /// The copy the current piece belongs to, and whether it is an edge.
    copy: u16,
    edge: bool,
    /// The row's accented cells (disjoint, ascending) and, while a part
    /// inside one is put, the accent's velocity.
    acc: &'a [(f64, f64)],
    acc_vel: Option<u8>,
    /// While a dashed edge piece is drawn: its nearest edge, which the side
    /// gradients and densities follow (V09-S57: one search per piece).
    fixed: Option<EdgeRef>,
    /// While a kept group of dashes is drawn: side densities already
    /// decided (V09-S57).
    side_off: bool,
}

impl RowOut<'_> {
    fn flush(&mut self) -> bool {
        match self.pend.take() {
            Some((s, e, b)) => self.pen.note(s * self.step, e * self.step, self.row, b),
            None => true,
        }
    }

    fn push(&mut self, s: f64, e: f64, br: Brush) -> bool {
        let mut br = br;
        if let Some(v) = self.acc_vel {
            br.vel = v;
        }
        if let Some(p) = &mut self.pend
            && p.1 == s
            && p.2 == br
        {
            p.1 = e;
            self.end = e;
            return true;
        }
        if !self.flush() {
            return false;
        }
        self.pend = Some((s, e, br));
        self.end = e;
        true
    }

    /// Whether gradient direction `d` applies to the current piece (one
    /// along the edges only to edge pieces).
    fn applies(&self, d: GDir) -> bool {
        d != GDir::Edge || self.edge
    }

    /// The brush at cell centre `u` (the gradients over `base`).
    fn brush(&self, u: f64, base: Brush) -> Brush {
        let mut b = base;
        for (g, d) in &self.style.grads {
            if self.applies(*d) {
                b = g.brush(self.scene.pos(*d, self.copy, u, self.yc), b, 1.0);
            }
        }
        for (g, d, cv) in &self.style.shaped {
            if self.applies(*d) {
                let s = self.scene.pos(*d, self.copy, u, self.yc);
                let s = if s.is_finite() {
                    s.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                b = g.brush(cv.eval(s), b, 1.0);
            }
        }
        if self.edge
            && let Some(sg) = &self.style.side_grads
            && !sg.is_empty()
            && let Some((e, s)) = self.edge_at(u)
        {
            b = sg[usize::from(e.side) % sg.len()].brush(s, b, 1.0);
        }
        b
    }

    /// The share of cells an edge cell keeps by its side's densities at
    /// `u`, and the cell's position along the outline (cells) for their
    /// dither — none without side densities.
    fn side_kept(&self, u: f64) -> Option<(f64, f64)> {
        let sd = self
            .style
            .side_density
            .as_ref()
            .filter(|v| self.edge && !self.side_off && !v.is_empty())?;
        let (e, s) = self.edge_at(u)?;
        let (d0, d1) = sd[usize::from(e.side) % sd.len()];
        let s = if s.is_finite() {
            s.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let at = e.off + (u - e.a.0) * e.dir.0 + (self.yc - e.a.1) * e.dir.1;
        Some(((d0 + (d1 - d0) * s).clamp(0.0, 1.0), at))
    }

    /// The nearest edge at `u` on the row and the position along its side:
    /// the dashed piece's edge while one is drawn.
    fn edge_at(&self, u: f64) -> Option<(EdgeRef, f64)> {
        match (self.fixed, self.scene.copies.get(usize::from(self.copy))) {
            (Some(e), Some(c)) => Some((e, Scene::along(c, &e, u, self.yc))),
            _ => self.scene.edge_at(self.copy, u, self.yc),
        }
    }

    /// Whether a gradient on the current piece changes along x.
    fn varies_along_x(&self) -> bool {
        self.style
            .grads
            .iter()
            .any(|(g, d)| g.varies() && *d != GDir::BottomTop && self.applies(*d))
            || self
                .style
                .shaped
                .iter()
                .any(|(g, d, _)| g.varies() && *d != GDir::BottomTop && self.applies(*d))
            || (self.edge && self.style.side_grads.is_some())
    }

    /// Piece `[a, b]` (grid units) with `base`: rounded to whole cells (at
    /// least one), after the last piece, cut by the gradients, thinned by
    /// the densities; the accented cells' parts with the accent's velocity.
    fn put(&mut self, a: f64, b: f64, base: Brush) -> bool {
        let s = a.round().max(self.end);
        let e = b.round().max(a.round() + 1.0);
        if !(s.is_finite() && e.is_finite()) || e <= s {
            return true;
        }
        if self.acc.is_empty() {
            return self.put_cells(s, e, base);
        }
        let vel = self.style.accent.as_ref().map_or(127, |x| x.vel);
        let acc = self.acc;
        let from = acc.partition_point(|w| w.1 <= s);
        let mut at = s;
        for &(ws, we) in &acc[from..] {
            if ws >= e {
                break;
            }
            let (ws, we) = (ws.max(at), we.min(e));
            if ws > at && !self.put_cells(at, ws, base) {
                return false;
            }
            if we > ws {
                self.acc_vel = Some(vel);
                let ok = self.put_cells(ws, we, base);
                self.acc_vel = None;
                if !ok {
                    return false;
                }
                at = we;
            }
        }
        at >= e || self.put_cells(at, e, base)
    }

    /// Whole cells `[s, e)` (s after the last piece) with `base`: cut by
    /// the gradients, thinned by the densities.
    fn put_cells(&mut self, s: f64, e: f64, base: Brush) -> bool {
        if !self.style.density.is_empty()
            || (self.edge
                && (self.style.arc_dash.is_some()
                    || (self.style.side_density.is_some() && !self.side_off)))
        {
            return self.thinned(s, e, base);
        }
        if !self.style.brushes() {
            return self.push(s, e, base);
        }
        if !self.varies_along_x() {
            let br = self.brush((s + e) / 2.0, base);
            return self.push(s, e, br);
        }
        let k = pieces(e - s);
        let mut prev = s;
        for j in 1..=k {
            let q = if j == k {
                e
            } else {
                (s + (e - s) * j as f64 / k as f64).round()
            };
            if q > prev {
                let br = self.brush((prev + q) / 2.0, base);
                if !self.push(prev, q, br) {
                    return false;
                }
                prev = q;
            }
        }
        true
    }

    /// Cells `[s, e)` cell by cell (cut into at most [`DENSITY_PARTS`]
    /// parts: longer parts on a longer piece): each part kept by its arc
    /// dash (edges) and the dither of its density — `frac(t · d) ≥ 1 − d`
    /// with `t` the part's position along its copy's outline (edges) or its
    /// first cell's column and row (interior) — with its brush.
    fn thinned(&mut self, s: f64, e: f64, base: Brush) -> bool {
        let dash = if self.edge { self.style.arc_dash } else { None };
        let n = ((e - s).ceil() as usize).clamp(1, DENSITY_PARTS);
        let mut prev = s;
        for j in 1..=n {
            let q = if j == n {
                e
            } else {
                (s + (e - s) * j as f64 / n as f64).round()
            };
            if q <= prev {
                continue;
            }
            let (c, u) = (prev, (prev + q) / 2.0);
            prev = q;
            let arc = || self.scene.arc_pos(self.copy, u, self.yc);
            if let Some((on, period, phase)) = dash
                && (arc() + phase).rem_euclid(period) >= on
            {
                continue;
            }
            let side = self.side_kept(u);
            let d = self.scene.kept(self.style, self.copy, u, self.yc) * side.map_or(1.0, |x| x.0);
            // Side densities dither by the position along the outline (an
            // edge far from the centre barely turns round it).
            let t = if let Some((_, at)) = side {
                at
            } else if self.edge {
                arc()
            } else {
                c + self.row as f64 * 0.618_033_988_749_895
            };
            let keep = d >= 1.0 || (d > 0.0 && (t * d).rem_euclid(1.0) >= 1.0 - d);
            if keep {
                let br = if self.style.brushes() {
                    self.brush(u, base)
                } else {
                    base
                };
                if !self.push(c, q, br) {
                    return false;
                }
            }
        }
        true
    }

    /// An edge piece: the brush of its nearest edge's side (or of the
    /// outline band's half), its dashes.
    fn edge_piece(&mut self, a: f64, b: f64, base: Brush, half: bool) -> bool {
        let st = self.style;
        let mut base = base;
        if let Some((outer, inner)) = st.half_subs {
            base.sub = if half { inner } else { outer };
        }
        if st.subs.is_none() && st.edge_dash.is_none() {
            return self.put(a, b, base);
        }
        let Some(e) = self.scene.nearest(self.copy, ((a + b) / 2.0, self.yc)) else {
            return self.put(a, b, base);
        };
        let side = usize::from(e.side);
        let mut br = base;
        if let Some(s) = &st.subs
            && !s.is_empty()
        {
            br.sub = s[side % s.len()];
        }
        let Some((on, periods, phase)) = &st.edge_dash else {
            return self.put(a, b, br);
        };
        let (on, phase) = (*on, *phase);
        let Some(&period) = periods.get(side % periods.len().max(1)) else {
            return self.put(a, b, br);
        };
        if period.partial_cmp(&on) != Some(Ordering::Greater) {
            return self.put(a, b, br);
        }
        // Position along the outline: u = off + (p − a) · dir + phase.
        let k = e.off + (self.yc - e.a.1) * e.dir.1 - e.a.0 * e.dir.0 + phase;
        if e.dir.0.abs() >= e.dir.1.abs() {
            let pat = XPat {
                alpha: e.dir.0,
                beta: k,
                on,
                period,
            };
            if st.side_density.is_some() || st.side_grads.is_some() {
                // V09-S57: side brushes follow the piece's edge (as its dashes
                // do); side densities decide groups of dashes on a long piece.
                self.fixed = Some(e);
                let ok = self.dashes_by_side(pat, a, b, br);
                self.fixed = None;
                return ok;
            }
            let mut w = XIter::new(pat, a, b);
            while let Some((x0, x1)) = w.next() {
                if !self.put(x0, x1, br) {
                    return false;
                }
            }
            true
        } else {
            let u = k + (a + b) / 2.0 * e.dir.0;
            if u.rem_euclid(period) < on {
                self.put(a, b, br)
            } else {
                true
            }
        }
    }

    /// The dashes of `pat` in `[a, b]` with side brushes (V09-S57): each one
    /// thinned by its share when the piece has at most [`DENSITY_PARTS`] of
    /// them; on a longer piece the dashes are cut into that many groups of
    /// whole dashes, each group kept or dropped by its share (an even dither
    /// by group) and a kept group drawn whole — so a huge piece costs that
    /// many tests, and every dash visited draws.
    fn dashes_by_side(&mut self, pat: XPat, a: f64, b: f64, br: Brush) -> bool {
        let (ua, ub) = (pat.alpha * a + pat.beta, pat.alpha * b + pat.beta);
        let (m0, m1) = (
            (ua.min(ub) / pat.period).floor(),
            (ua.max(ub) / pat.period).floor(),
        );
        let n = m1 - m0 + 1.0;
        if self.style.side_density.is_none()
            || n.partial_cmp(&(DENSITY_PARTS as f64)) != Some(Ordering::Greater)
        {
            let mut w = XIter::new(pat, a, b);
            while let Some((x0, x1)) = w.next() {
                if !self.put(x0, x1, br) {
                    return false;
                }
            }
            return true;
        }
        let groups = DENSITY_PARTS;
        let per = (n / groups as f64).ceil();
        // Group g holds dashes m0 + g·per … ; its x range from its first
        // dash's start to the next group's (ascending x: reversed when the
        // edge runs right to left).
        let x_at = |m: f64| (m * pat.period - pat.beta) / pat.alpha;
        for j in 0..groups {
            let g = if pat.alpha > 0.0 { j } else { groups - 1 - j };
            let (s0, s1) = (m0 + g as f64 * per, m0 + (g as f64 + 1.0) * per);
            if s0 > m1 {
                continue;
            }
            let (p, q) = (x_at(s0), x_at(s1));
            let (xa, xb) = (p.min(q).max(a), p.max(q).min(b));
            if xb.partial_cmp(&xa) != Some(Ordering::Greater) {
                continue;
            }
            let Some((d, _)) = self.side_kept((xa + xb) / 2.0) else {
                continue;
            };
            let t = g as f64;
            if !(d >= 1.0 || (d > 0.0 && (t * d).rem_euclid(1.0) >= 1.0 - d)) {
                continue;
            }
            self.side_off = true;
            let mut w = XIter::new(pat, xa, xb);
            let mut ok = true;
            while let Some((x0, x1)) = w.next() {
                if !self.put(x0, x1, br) {
                    ok = false;
                    break;
                }
            }
            self.side_off = false;
            if !ok {
                return false;
            }
        }
        true
    }

    /// An interior piece: the union of the stripes and line features (or,
    /// filled, what lies between them).
    fn interior(
        &mut self,
        a: f64,
        b: f64,
        base: Brush,
        lw: &[(f64, f64)],
        it: &mut Vec<XIter>,
    ) -> bool {
        let st = self.style;
        let row = self.row;
        if st.x_stripes.len() == 1 && lw.is_empty() {
            // One pattern alone: its windows (or the gaps between them).
            let (p, r0, k) = st.x_stripes[0];
            let mut w = XIter::new(
                XPat {
                    beta: p.beta + k * (row - r0) as f64,
                    ..p
                },
                a,
                b,
            );
            let mut from = a;
            while let Some((s, e)) = w.next() {
                let ok = if st.filled {
                    let ok = s <= from || self.put(from, s, base);
                    from = e;
                    ok
                } else {
                    self.put(s, e, base)
                };
                if !ok {
                    return false;
                }
            }
            return !(st.filled && b > from) || self.put(from, b, base);
        }
        let mk = |x: f64, it: &mut Vec<XIter>| {
            it.clear();
            for &(p, r0, k) in &st.x_stripes {
                let q = XPat {
                    beta: p.beta + k * (row - r0) as f64,
                    ..p
                };
                it.push(XIter::new(q, x, b));
            }
        };
        mk(a, it);
        let mut heads: Vec<Option<(f64, f64)>> = it.iter_mut().map(|w| w.next()).collect();
        let lines: Vec<(f64, f64)> = meeting(lw, a, b)
            .iter()
            .filter_map(|&(s, e)| {
                let (s, e) = (s.max(a), e.min(b));
                (e > s).then_some((s, e))
            })
            .collect();
        let mut li = 0;
        let period = Scene::lcm(st);
        let filled = st.filled;
        // The union's current window and where its stripes-only run began.
        let mut cur: Option<(f64, f64)> = None;
        let mut run = f64::NAN;
        let mut gap_from = a;
        let mut emit = |out: &mut RowOut, s: f64, e: f64| -> bool {
            if filled {
                let ok = s <= gap_from || out.put(gap_from, s, base);
                gap_from = gap_from.max(e);
                ok
            } else {
                out.put(s, e, base)
            }
        };
        loop {
            // The next window: the earliest stripe head, or the next line.
            let mut best: Option<(usize, (f64, f64))> = None;
            for (i, h) in heads.iter().enumerate() {
                if let Some(w) = h
                    && best.is_none_or(|(_, bw)| w.0 < bw.0)
                {
                    best = Some((i, *w));
                }
            }
            let line = lines.get(li).copied();
            let (w, is_line) = match (best, line) {
                (Some((_, bw)), Some(lw)) if lw.0 <= bw.0 => (lw, true),
                (None, Some(lw)) => (lw, true),
                (Some((i, bw)), _) => {
                    heads[i] = it[i].next();
                    (bw, false)
                }
                (None, None) => break,
            };
            if is_line {
                li += 1;
            }
            match &mut cur {
                Some(c) if w.0 <= c.1 => {
                    c.1 = c.1.max(w.1);
                    if !is_line && run.is_finite() && c.1 - run >= period {
                        // The stripes cover a whole period from `run`: all
                        // of the rest.
                        c.1 = b;
                        heads.iter_mut().for_each(|h| *h = None);
                        li = lines.len();
                    }
                }
                _ => {
                    if let Some((s, e)) = cur.take()
                        && !emit(self, s, e)
                    {
                        return false;
                    }
                    cur = Some(w);
                    run = if is_line { f64::NAN } else { w.0 };
                }
            }
            if is_line {
                // The line covers the stripes under it: skip them.
                let e = cur.map_or(w.1, |c| c.1);
                run = e;
                mk(e, it);
                heads = it.iter_mut().map(|x| x.next()).collect();
            }
        }
        if let Some((s, e)) = cur
            && !emit(self, s, e)
        {
            return false;
        }
        if filled && b > gap_from {
            return self.put(gap_from, b, base);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[P2], b: &[P2]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(p, q)| (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9)
    }

    #[test]
    fn polygons_offset_chamfer_and_round_staying_convex_and_counter_clockwise() {
        let r = Poly::rect(0.0, 0.0, 10.0, 4.0);
        assert_eq!(r.area(), 40.0);
        // Inset: the half-planes moved in; outset: mitred corners.
        assert!(close(
            &r.offset(-1.0).pts,
            &[(1.0, 1.0), (9.0, 1.0), (9.0, 3.0), (1.0, 3.0)]
        ));
        assert!(close(
            &r.offset(1.0).pts,
            &[(-1.0, -1.0), (11.0, -1.0), (11.0, 5.0), (-1.0, 5.0)]
        ));
        assert!(r.offset(-2.0).is_empty() && r.offset(-3.0).is_empty());
        // A clockwise outline comes back counter-clockwise with each edge's
        // side kept.
        let cw = Poly {
            pts: vec![(0.0, 0.0), (0.0, 4.0), (10.0, 4.0), (10.0, 0.0)],
            sides: vec![3, 2, 1, 0],
        }
        .map(|p| p);
        assert!(cw.area() > 0.0);
        for i in 0..4 {
            let (a, b) = cw.edge(i);
            let want = match (a, b) {
                ((0.0, 0.0), (10.0, 0.0)) => 0,
                ((10.0, 0.0), (10.0, 4.0)) => 1,
                ((10.0, 4.0), (0.0, 4.0)) => 2,
                _ => 3,
            };
            assert_eq!(cw.sides[i], want, "{a:?} → {b:?}");
        }
        // Chamfer 1: eight corners, the cuts tagged with the incoming side.
        let c = r.chamfer(1.0);
        assert_eq!(c.pts.len(), 8);
        assert!((c.area() - (40.0 - 4.0 * 0.5)).abs() < 1e-9);
        assert_eq!(c.sides, vec![3, 0, 0, 1, 1, 2, 2, 3]);
        // Rounding, radius 2: the rectangle less four corners of r² − πr²/4
        // each, a little less (the arcs are inscribed); convex (every turn
        // left).
        let q = r.round(2.0);
        let arcs = 4.0 * (4.0 - std::f64::consts::PI);
        assert!(q.area() < 40.0 - arcs && q.area() > 40.0 - arcs - 0.2);
        let n = q.pts.len();
        for i in 0..n {
            let (a, b, c) = (q.pts[i], q.pts[(i + 1) % n], q.pts[(i + 2) % n]);
            assert!((b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0) > -1e-9);
        }
        // A radius larger than half the short side is clamped to it.
        assert!(r.round(100.0).bbox() == [0.0, 0.0, 10.0, 4.0]);
    }

    #[test]
    fn windows_are_visited_in_ascending_time_either_way() {
        let p = XPat {
            alpha: 1.0,
            beta: 0.0,
            on: 1.0,
            period: 3.0,
        };
        let mut w = XIter::new(p, 0.5, 10.0);
        let mut v = Vec::new();
        while let Some(x) = w.next() {
            v.push(x);
        }
        assert_eq!(v, vec![(0.5, 1.0), (3.0, 4.0), (6.0, 7.0), (9.0, 10.0)]);
        // Descending u (a leftwards edge): the same windows mirrored.
        let mut w = XIter::new(
            XPat {
                alpha: -1.0,
                beta: 10.0,
                ..p
            },
            0.0,
            10.0,
        );
        let mut v = Vec::new();
        while let Some(x) = w.next() {
            v.push(x);
        }
        assert_eq!(v, vec![(0.0, 1.0), (3.0, 4.0), (6.0, 7.0), (9.0, 10.0)]);
    }
    #[test]
    fn convex_rings_cross_by_binary_search_and_star_rings_even_odd() {
        // A rounded, rotated rectangle and an octagon: the chains give the
        // full scan's outermost crossings on every row centre.
        let shapes = [
            Poly::rect(0.0, 0.0, 30.0, 12.0)
                .round(4.0)
                .map(|(u, v)| (u * 0.8 - v * 0.6, u * 0.6 + v * 0.8)),
            Poly::rect(0.0, 0.0, 9.0, 9.0).chamfer(3.0),
        ];
        for p in shapes {
            let ring = Ring {
                chains: Chains::of(&p),
                order: Vec::new(),
                vmin: p.bbox()[1],
                vmax: p.bbox()[3],
                poly: p.clone(),
                role: Role::Outer,
                copy: 0,
                convex: true,
            };
            assert!(ring.chains.is_some());
            let (mut xs, mut out) = (Vec::new(), Vec::new());
            for k in -40..80 {
                let yc = f64::from(k) * 0.37 + 0.5;
                out.clear();
                ring.spans(yc, &mut xs, &mut out, None);
                xs.clear();
                p.crossings(yc, &mut xs);
                let want = xs
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &x| {
                        (l.min(x), h.max(x))
                    });
                if want.1 > want.0 {
                    assert_eq!(out, vec![want], "yc {yc}");
                } else {
                    assert!(out.is_empty(), "yc {yc}");
                }
            }
        }
        // A five-pointed star (not convex): a row through its two lower
        // points gives two intervals.
        let star = Poly::from_points(
            (0..10)
                .map(|k| {
                    let a = std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * f64::from(k) / 5.0;
                    let r = if k % 2 == 0 { 10.0 } else { 4.0 };
                    (r * a.cos(), r * a.sin())
                })
                .collect(),
            vec![0; 10],
        );
        let ring = Ring {
            chains: None,
            order: Vec::new(),
            vmin: star.bbox()[1],
            vmax: star.bbox()[3],
            poly: star,
            role: Role::Outer,
            copy: 0,
            convex: false,
        };
        let (mut xs, mut out) = (Vec::new(), Vec::new());
        ring.spans(-6.0, &mut xs, &mut out, None);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].1 < out[1].0);
        assert_eq!(ring.per_row(), 5.0);
    }

    #[test]
    fn the_active_edge_table_gives_the_full_scans_intervals() {
        // A wavy star (not convex) scanned upwards row by row, then a row
        // out of order (the table restarts).
        let n = 400;
        let star = Poly::from_points(
            (0..n)
                .map(|k| {
                    let a = std::f64::consts::TAU * k as f64 / n as f64;
                    let r = 20.0 * (1.0 + 0.3 * (7.0 * a).sin());
                    (r * a.cos() + 3.3, r * a.sin() - 1.7)
                })
                .collect(),
            vec![0; n],
        );
        let scene = Scene::with_copies(
            Vec::new(),
            vec![RingSpec {
                poly: star.clone(),
                role: Role::Outer,
                copy: 0,
                convex: false,
            }],
            Vec::new(),
        );
        let ring = &scene.rings[0];
        assert!(!ring.order.is_empty());
        let mut st = Aet {
            last: f64::NEG_INFINITY,
            ..Aet::default()
        };
        let (mut xs, mut a, mut b) = (Vec::new(), Vec::new(), Vec::new());
        let rows: Vec<i64> = (-30..30).chain([-5, 2, 29]).collect();
        for r in rows {
            let yc = r as f64 + 0.5;
            a.clear();
            b.clear();
            ring.spans(yc, &mut xs, &mut a, Some(&mut st));
            ring.spans(yc, &mut xs, &mut b, None);
            assert_eq!(a, b, "row {r}");
        }
    }

    #[test]
    fn a_rows_line_windows_are_their_union_and_pieces_find_theirs() {
        // Crossing, overlapping and touching segments: per row the windows
        // are disjoint, ascending, and cover exactly the cells the separate
        // segments cover; a piece's binary search finds what a filter does.
        let mut lines = Vec::new();
        for k in 0..40 {
            let a = f64::from(k) * 0.37;
            let o = f64::from(k % 7) * 3.1 - 9.0;
            lines.push(((o - 30.0 * a.cos(), -20.0), (o + 30.0 * a.cos(), 20.0)));
        }
        lines.push(((-50.0, 0.25), (50.0, 0.25)));
        lines.push(((-3.0, -40.0), (-3.0, 40.0)));
        let scene = Scene::with_copies(Vec::new(), Vec::new(), lines.clone());
        let mut st = Aet {
            last: f64::NEG_INFINITY,
            ..Aet::default()
        };
        let mut lw = Vec::new();
        for row in -25..25 {
            scene.line_windows(row, &mut st, &mut lw);
            assert!(lw.windows(2).all(|w| w[0].1 < w[1].0), "row {row}: {lw:?}");
            let cell = |x: f64| lw.iter().any(|w| x > w.0 && x < w.1);
            // Each segment alone, the way the windows are found.
            let (y0, y1) = (row as f64, row as f64 + 1.0);
            let mut each = Vec::new();
            for &(p, q) in &lines {
                let dy = q.1 - p.1;
                let (ta, tb) = if dy == 0.0 {
                    if p.1 >= y0 && p.1 < y1 {
                        (0.0, 1.0)
                    } else {
                        continue;
                    }
                } else {
                    let (u, v) = ((y0 - p.1) / dy, (y1 - p.1) / dy);
                    (u.min(v).max(0.0), u.max(v).min(1.0))
                };
                if tb < ta {
                    continue;
                }
                let (xa, xb) = (p.0 + (q.0 - p.0) * ta, p.0 + (q.0 - p.0) * tb);
                let s = xa.min(xb).round();
                each.push((s, xa.max(xb).round().max(s + 1.0)));
            }
            for c in -60..60 {
                let x = f64::from(c) + 0.5;
                assert_eq!(
                    cell(x),
                    each.iter().any(|w| x > w.0 && x < w.1),
                    "row {row} cell {c}"
                );
            }
            for (a, b) in [(-60.0, 60.0), (-7.0, -2.0), (0.0, 1.0), (13.0, 40.0)] {
                let want: Vec<_> = lw.iter().copied().filter(|w| w.1 > a && w.0 < b).collect();
                assert_eq!(meeting(&lw, a, b), &want[..], "row {row} piece {a}..{b}");
            }
        }
    }
}
