//! General closed areas (V09-S57, `docs/GENERATORS.md` "Shared drawing:
//! areas"): what Custom Shape needs that the ring scenes of `region.rs` do
//! not have — contours of any shape (self-crossing, several, with holes)
//! filled by a rule, boolean operations, unions of transformed instances
//! (symmetry), the exact boundary of the result, and strokes along paths.
//! A [`Scene`](super::region::Scene) built with
//! [`Scene::with_areas`](super::region::Scene::with_areas) scans these
//! instead of rings; everything after the row's pieces (brushes, dashes,
//! patterns, gradients, densities, the pen) is the region rasterizer's.
//!
//! * An area is the union of [`Figure`]s (instances). A figure is its
//!   layers (each a set of contours filled by its [`Rule`]) united, less
//!   its holes (even–odd), then combined with operands in order
//!   ([`Op`]: union, subtract, intersection; operands even–odd).
//! * **Rows**: the crossings of the row's centre with every contour
//!   segment (half-open in v, the pen's rule) are swept left to right
//!   with a winding count per contour set; the expression evaluated
//!   between crossings gives the area's intervals.
//! * **Boundary** (built once): every segment is split where another one
//!   crosses or touches it, and each piece is kept when the area lies on
//!   one side of it only (tested a hair to its left and right). The pieces
//!   keep their edge's tag, the chord of the edge they came from and their
//!   distance along their contour.
//! * **Band**: the cells within the thickness of the boundary — per row the
//!   union of the intervals of capsules round the boundary pieces — inside
//!   the area, outside it, or centred on the boundary.
//! * **Strokes**: paths (open or closed) as unions of convex parts —
//!   capsules for round joins and caps, quads for butt bodies, mitres,
//!   bevels and square caps.
//!
//! Every loop is bounded: segments are capped by the caller
//! ([`SEG_CAP`]), the pairs tested for crossings by [`PAIR_CAP`], and each
//! row visits only the segments and parts reaching it (active tables).

use std::cmp::Ordering;

/// A point in grid space (see `region.rs`).
pub type P2 = (f64, f64);

/// Most segments of one geometry (contours after flattening, warping and
/// symmetry, strokes included).
pub const SEG_CAP: usize = 1 << 16;
/// Most segment pairs tested for crossings while a boundary is found.
pub const PAIR_CAP: u64 = 1 << 25;
/// The longest a mitre may reach, in half-widths, before it is bevelled
/// (the SVG default).
pub const MITRE_LIMIT: f64 = 4.0;

/// The sentence refusing a shape of `n` segments (over [`SEG_CAP`]).
pub fn too_many(n: usize) -> String {
    format!(
        "The shape would have {} segments; at most {} are allowed — use fewer vertices, contours, symmetry copies, Bézier detail or warp detail.",
        crate::midi::group_digits(n as u64),
        crate::midi::group_digits(SEG_CAP as u64)
    )
}

/// How a set of contours fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rule {
    /// Inside where a ray crosses the contours an odd number of times.
    #[default]
    EvenOdd,
    /// Inside where the contours wind round the point (any direction).
    NonZero,
}

/// A boolean operation with an operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Union,
    Subtract,
    Intersect,
}

/// Where a band lies about the area's boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// Inside the area (cells within the thickness of its boundary).
    #[default]
    Inside,
    /// Outside it.
    Outside,
    /// Centred on the boundary (half the thickness each side).
    Centred,
}

/// How a stroke's segments meet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Join {
    #[default]
    Round,
    /// Mitred up to [`MITRE_LIMIT`] half-widths, bevelled beyond.
    Mitre,
    Bevel,
}

/// How an open stroke ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cap {
    Butt,
    #[default]
    Round,
    /// Extended by half the width.
    Square,
}

/// A contour: points, each edge's tag and the chord of the edge it came
/// from (edge `i` runs `pts[i] → pts[i + 1]`, a closed contour's last one
/// back to the first; a flattened curve's pieces share their edge's tag
/// and chord).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    pub pts: Vec<P2>,
    pub tags: Vec<u8>,
    pub chords: Vec<(P2, P2)>,
    pub closed: bool,
}

impl Contour {
    /// A polygon whose edge `i` is tagged `tag0 + i` (wrapping) with its own
    /// chord.
    pub fn polygon(pts: Vec<P2>, tag0: usize, closed: bool) -> Contour {
        let n = pts.len();
        let m = if closed { n } else { n.saturating_sub(1) };
        let chords = (0..m).map(|i| (pts[i], pts[(i + 1) % n])).collect();
        Contour {
            tags: (0..m).map(|i| ((tag0 + i) % 256) as u8).collect(),
            chords,
            pts,
            closed,
        }
    }

    /// How many edges it has.
    pub fn edges(&self) -> usize {
        if self.closed {
            self.pts.len()
        } else {
            self.pts.len().saturating_sub(1)
        }
    }

    /// Every point (and chord) mapped by `f`.
    pub fn map(&self, f: impl Fn(P2) -> P2) -> Contour {
        Contour {
            pts: self.pts.iter().map(|&p| f(p)).collect(),
            tags: self.tags.clone(),
            chords: self.chords.iter().map(|&(a, b)| (f(a), f(b))).collect(),
            closed: self.closed,
        }
    }

    /// Edge `i`: its ends.
    pub fn edge(&self, i: usize) -> (P2, P2) {
        let n = self.pts.len();
        (self.pts[i], self.pts[(i + 1) % n])
    }

    /// Each edge cut into pieces of at most `len` (grid units), at most
    /// `most` per edge (tags and chords kept): before a map that bends
    /// straight lines.
    pub fn densify(&self, len: f64, most: usize) -> Contour {
        let m = self.edges();
        let mut out = Contour {
            closed: self.closed,
            ..Contour::default()
        };
        for i in 0..m {
            let (a, b) = self.edge(i);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            let k = if l.is_finite() && len > 0.0 {
                ((l / len).ceil() as usize).clamp(1, most.max(1))
            } else {
                1
            };
            for j in 0..k {
                let t = j as f64 / k as f64;
                out.pts.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
                out.tags.push(self.tags[i]);
                out.chords.push(self.chords[i]);
            }
        }
        if !self.closed
            && let Some(&last) = self.pts.last()
        {
            out.pts.push(last);
        }
        out
    }

    /// The signed area (positive counter-clockwise).
    pub fn area(&self) -> f64 {
        let n = self.pts.len();
        (0..n)
            .map(|i| {
                let (p, q) = (self.pts[i], self.pts[(i + 1) % n]);
                p.0 * q.1 - q.0 * p.1
            })
            .sum::<f64>()
            / 2.0
    }
}

/// One instance of an area: layers united, less the holes, then the
/// operands in order.
#[derive(Clone, Debug, Default)]
pub struct Figure {
    pub layers: Vec<(Vec<Contour>, Rule)>,
    pub holes: Vec<Contour>,
    pub ops: Vec<(Op, Vec<Contour>)>,
}

impl Figure {
    /// Every contour mapped by `f`.
    pub fn map(&self, f: &dyn Fn(P2) -> P2) -> Figure {
        let m = |cs: &Vec<Contour>| cs.iter().map(|c| c.map(f)).collect::<Vec<_>>();
        Figure {
            layers: self.layers.iter().map(|(cs, r)| (m(cs), *r)).collect(),
            holes: m(&self.holes),
            ops: self.ops.iter().map(|(o, cs)| (*o, m(cs))).collect(),
        }
    }

    /// Every contour of the figure.
    pub fn contours(&self) -> impl Iterator<Item = &Contour> {
        self.layers
            .iter()
            .flat_map(|(cs, _)| cs.iter())
            .chain(self.holes.iter())
            .chain(self.ops.iter().flat_map(|(_, cs)| cs.iter()))
    }

    /// Every contour, mutably.
    pub fn contours_mut(&mut self) -> impl Iterator<Item = &mut Contour> {
        self.layers
            .iter_mut()
            .flat_map(|(cs, _)| cs.iter_mut())
            .chain(self.holes.iter_mut())
            .chain(self.ops.iter_mut().flat_map(|(_, cs)| cs.iter_mut()))
    }
}

/// A boundary piece or a stroke's segment: its ends, tag, distance along
/// its contour at `a` (grid units) and the chord of its edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub a: P2,
    pub b: P2,
    pub tag: u8,
    pub off: f64,
    pub chord: (P2, P2),
}

/// A convex part of a band or stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    /// The points within `r` of segment `a → b` (a disc when `a == b`).
    Capsule(P2, P2, f64),
    /// A convex polygon of 3 or 4 points.
    Quad([P2; 4], u8),
}

impl Part {
    fn vrange(&self) -> (f64, f64) {
        match *self {
            Part::Capsule(a, b, r) => (a.1.min(b.1) - r, a.1.max(b.1) + r),
            Part::Quad(p, n) => p[..usize::from(n)]
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), q| {
                    (l.min(q.1), h.max(q.1))
                }),
        }
    }

    fn urange(&self) -> (f64, f64) {
        match *self {
            Part::Capsule(a, b, r) => (a.0.min(b.0) - r, a.0.max(b.0) + r),
            Part::Quad(p, n) => p[..usize::from(n)]
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), q| {
                    (l.min(q.0), h.max(q.0))
                }),
        }
    }

    /// The part's interval on the line `v = y` (closed), if it meets it.
    fn span(&self, y: f64) -> Option<(f64, f64)> {
        match *self {
            Part::Capsule(a, b, r) => capsule_span(a, b, r, y),
            Part::Quad(p, n) => {
                let n = usize::from(n);
                // Half-open like a capsule: a row on the lowest point or
                // edge does not count, one on the highest does.
                let (vlo, vhi) = self.vrange();
                if y <= vlo || y > vhi {
                    return None;
                }
                let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                for i in 0..n {
                    let (s, e) = (p[i], p[(i + 1) % n]);
                    if (s.1 - y) * (e.1 - y) > 0.0 {
                        continue;
                    }
                    if s.1 == e.1 {
                        lo = lo.min(s.0.min(e.0));
                        hi = hi.max(s.0.max(e.0));
                    } else {
                        let x = s.0 + (y - s.1) * (e.0 - s.0) / (e.1 - s.1);
                        lo = lo.min(x);
                        hi = hi.max(x);
                    }
                }
                (hi >= lo).then_some((lo, hi))
            }
        }
    }
}

/// The points of the line `v = y` within `r` of segment `a → b`: the union
/// of the two end discs' chords and the body's (the strip about the line
/// within the slab between the ends) — one interval, a capsule being convex.
/// Rows exactly `r` away count above the segment, not below it (the pen's
/// "the cell after a grid line": a stroke one unit wide on a row boundary
/// is one row).
fn capsule_span(a: P2, b: P2, r: f64, y: f64) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let within = |dy: f64| -r < dy && dy <= r;
    for c in [a, b] {
        let dy = y - c.1;
        if within(dy) {
            let w = (r * r - dy * dy).max(0.0).sqrt();
            lo = lo.min(c.0 - w);
            hi = hi.max(c.0 + w);
        }
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    if l2 > 0.0 {
        let l = l2.sqrt();
        let ey = y - a.1;
        // Strip: |(x − a.x)·dy − ey·dx| ≤ r·l.
        let strip = if dy != 0.0 {
            let x0 = a.0 + ey * dx / dy;
            let w = r * l / dy.abs();
            Some((x0 - w, x0 + w))
        } else if within(ey) {
            Some((f64::NEG_INFINITY, f64::INFINITY))
        } else {
            None
        };
        // Slab: 0 ≤ (x − a.x)·dx + ey·dy ≤ l².
        let slab = if dx != 0.0 {
            let (p, q) = (a.0 - ey * dy / dx, a.0 + (l2 - ey * dy) / dx);
            Some((p.min(q), p.max(q)))
        } else if (0.0..=l2).contains(&(ey * dy)) {
            Some((f64::NEG_INFINITY, f64::INFINITY))
        } else {
            None
        };
        if let (Some(s), Some(t)) = (strip, slab) {
            let (p, q) = (s.0.max(t.0), s.1.min(t.1));
            if q >= p {
                lo = lo.min(p);
                hi = hi.max(q);
            }
        }
    }
    (hi >= lo && lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// The parts of a stroke of half-width `h` along `pts` (closed: back to
/// the first point).
pub fn stroke(pts: &[P2], closed: bool, h: f64, join: Join, cap: Cap, out: &mut Vec<Part>) {
    // Consecutive equal points dropped.
    let mut p: Vec<P2> = Vec::with_capacity(pts.len());
    for &q in pts {
        if q.0.is_finite() && q.1.is_finite() && p.last() != Some(&q) {
            p.push(q);
        }
    }
    if closed {
        while p.len() > 1 && p.first() == p.last() {
            p.pop();
        }
    }
    let n = p.len();
    if n == 0 || h <= 0.0 {
        return;
    }
    if n == 1 {
        match cap {
            Cap::Round => out.push(Part::Capsule(p[0], p[0], h)),
            Cap::Square => out.push(Part::Quad(
                [
                    (p[0].0 - h, p[0].1 - h),
                    (p[0].0 + h, p[0].1 - h),
                    (p[0].0 + h, p[0].1 + h),
                    (p[0].0 - h, p[0].1 + h),
                ],
                4,
            )),
            Cap::Butt => {}
        }
        return;
    }
    let segs = if closed { n } else { n - 1 };
    let unit = |a: P2, b: P2| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = dx.hypot(dy);
        (dx / l, dy / l)
    };
    if join == Join::Round && (closed || cap == Cap::Round) {
        for i in 0..segs {
            out.push(Part::Capsule(p[i], p[(i + 1) % n], h));
        }
        return;
    }
    // Butt bodies.
    for i in 0..segs {
        let (a, b) = (p[i], p[(i + 1) % n]);
        let d = unit(a, b);
        let nv = (-d.1 * h, d.0 * h);
        out.push(Part::Quad(
            [
                (a.0 + nv.0, a.1 + nv.1),
                (b.0 + nv.0, b.1 + nv.1),
                (b.0 - nv.0, b.1 - nv.1),
                (a.0 - nv.0, a.1 - nv.1),
            ],
            4,
        ));
    }
    // Joins at the inner vertices (every vertex of a closed path).
    let joins: Vec<usize> = if closed {
        (0..n).collect()
    } else {
        (1..n - 1).collect()
    };
    for i in joins {
        let (prev, v, next) = (p[(i + n - 1) % n], p[i], p[(i + 1) % n]);
        let (d1, d2) = (unit(prev, v), unit(v, next));
        let turn = d1.0 * d2.1 - d1.1 * d2.0;
        if turn.abs() < 1e-12 && d1.0 * d2.0 + d1.1 * d2.1 > 0.0 {
            continue;
        }
        if join == Join::Round {
            out.push(Part::Capsule(v, v, h));
            continue;
        }
        // The outer side: right of a left turn.
        let s = if turn > 0.0 { -h } else { h };
        let (n1, n2) = ((-d1.1, d1.0), (-d2.1, d2.0));
        let (e1, e2) = (
            (v.0 + s * n1.0, v.1 + s * n1.1),
            (v.0 + s * n2.0, v.1 + s * n2.1),
        );
        let cos = n1.0 * n2.0 + n1.1 * n2.1;
        let ratio2 = 2.0 / (1.0 + cos).max(1e-12);
        if join == Join::Mitre && ratio2 <= MITRE_LIMIT * MITRE_LIMIT {
            let k = s / (1.0 + cos);
            let m = (v.0 + k * (n1.0 + n2.0), v.1 + k * (n1.1 + n2.1));
            out.push(Part::Quad([v, e1, m, e2], 4));
        } else {
            out.push(Part::Quad([v, e1, e2, v], 3));
        }
    }
    if !closed {
        for (end, d) in [
            (p[0], unit(p[1], p[0])),
            (p[n - 1], unit(p[n - 2], p[n - 1])),
        ] {
            match cap {
                Cap::Butt => {}
                Cap::Round => out.push(Part::Capsule(end, end, h)),
                Cap::Square => {
                    let nv = (-d.1 * h, d.0 * h);
                    let f = (end.0 + d.0 * h, end.1 + d.1 * h);
                    out.push(Part::Quad(
                        [
                            (end.0 + nv.0, end.1 + nv.1),
                            (f.0 + nv.0, f.1 + nv.1),
                            (f.0 - nv.0, f.1 - nv.1),
                            (end.0 - nv.0, end.1 - nv.1),
                        ],
                        4,
                    ));
                }
            }
        }
    }
}

/// A region segment: its ends (in the contour's direction) and set.
#[derive(Clone, Copy, Debug)]
struct Seg {
    a: P2,
    b: P2,
    set: u16,
}

impl Seg {
    fn lo(&self) -> f64 {
        self.a.1.min(self.b.1)
    }
    fn hi(&self) -> f64 {
        self.a.1.max(self.b.1)
    }
    /// Whether the line `v = y` crosses it (half-open: the pen's rule).
    #[inline]
    fn crosses(&self, y: f64) -> bool {
        (self.a.1 <= y && y < self.b.1) || (self.b.1 <= y && y < self.a.1)
    }
    #[inline]
    fn x_at(&self, y: f64) -> f64 {
        self.a.0 + (y - self.a.1) * (self.b.0 - self.a.0) / (self.b.1 - self.a.1)
    }
    #[inline]
    fn dir(&self) -> i32 {
        if self.b.1 > self.a.1 { 1 } else { -1 }
    }
}

/// One instance's expression: its layers' sets, its holes' set, its
/// operations.
#[derive(Clone, Debug, Default)]
struct Plan {
    layers: Vec<u16>,
    holes: Option<u16>,
    ops: Vec<(Op, u16)>,
}

/// What a geometry is built from.
#[derive(Clone, Debug, Default)]
pub struct GeomSpec {
    /// The area: the union of these instances.
    pub area: Vec<Figure>,
    /// The band about the area's boundary and its thickness (none: no band).
    pub band: Option<(Place, f64)>,
    /// Paths stroked centred on their lines.
    pub strokes: Vec<Contour>,
    /// The strokes' half-width, joins and caps.
    pub pen: (f64, Join, Cap),
}

/// An area, its boundary band and strokes, ready to be scanned (local
/// coordinates; copies place it).
#[derive(Clone, Debug, Default)]
pub struct Geom {
    segs: Vec<Seg>,
    seg_order: Vec<u32>,
    rules: Vec<Rule>,
    plans: Vec<Plan>,
    /// Band parts (the placement applies) and stroke parts (always drawn).
    band: Vec<Part>,
    band_order: Vec<u32>,
    strokes: Vec<Part>,
    stroke_order: Vec<u32>,
    place: Place,
    /// The boundary pieces and the strokes' segments (for brushes).
    pub edges: Vec<Edge>,
    /// `[umin, vmin, umax, vmax]` of everything drawn.
    pub bbox: [f64; 4],
}

/// An active table over items sorted by their lowest row: rows ascend
/// (out of order it restarts).
#[derive(Clone, Debug, Default)]
pub struct Table {
    next: usize,
    active: Vec<u32>,
    last: f64,
    started: bool,
}

impl Table {
    fn advance(&mut self, y: f64, order: &[u32], lo: impl Fn(u32) -> f64, hi: impl Fn(u32) -> f64) {
        if !self.started
            || y.partial_cmp(&self.last)
                .is_none_or(|o| o == Ordering::Less)
        {
            self.next = 0;
            self.active.clear();
            self.started = true;
        }
        self.last = y;
        while self.next < order.len() && lo(order[self.next]) <= y {
            self.active.push(order[self.next]);
            self.next += 1;
        }
        self.active.retain(|&k| hi(k) >= y);
    }
}

/// A copy's scan state: the active tables of its segments, band and
/// strokes.
#[derive(Clone, Debug, Default)]
pub struct RowState {
    segs: Table,
    band: Table,
    strokes: Table,
}

/// Reused buffers of a row.
#[derive(Clone, Debug, Default)]
pub struct RowBuf {
    xs: Vec<(f64, u16, i32)>,
    counts: Vec<i32>,
    region: Vec<(f64, f64)>,
    near: Vec<(f64, f64)>,
    strk: Vec<(f64, f64)>,
    tmp: Vec<(f64, f64)>,
    tmp2: Vec<(f64, f64)>,
}

fn order_by(n: usize, lo: impl Fn(usize) -> f64) -> Vec<u32> {
    let mut o: Vec<u32> = (0..n as u32).collect();
    o.sort_by(|&a, &b| lo(a as usize).total_cmp(&lo(b as usize)).then(a.cmp(&b)));
    o
}

/// A segment with what its boundary pieces carry.
#[derive(Clone, Copy, Debug)]
struct Src {
    seg: Seg,
    tag: u8,
    off: f64,
    chord: (P2, P2),
}

impl Geom {
    /// Builds the geometry of `spec`, or the cap's sentence.
    pub fn build(spec: &GeomSpec) -> Result<Geom, String> {
        let mut rules = Vec::new();
        let mut plans = Vec::new();
        let mut src: Vec<Src> = Vec::new();
        let add = |cs: &[Contour], set: u16, src: &mut Vec<Src>| {
            for c in cs.iter().filter(|c| c.pts.len() >= 2) {
                let mut off = 0.0;
                // Region segments close every contour.
                let n = c.pts.len();
                for i in 0..n {
                    let (a, b) = (c.pts[i], c.pts[(i + 1) % n]);
                    let k = i.min(c.tags.len().saturating_sub(1));
                    let l = (b.0 - a.0).hypot(b.1 - a.1);
                    if a != b && l.is_finite() {
                        src.push(Src {
                            seg: Seg { a, b, set },
                            tag: c.tags.get(k).copied().unwrap_or(0),
                            off,
                            chord: c.chords.get(k).copied().unwrap_or((a, b)),
                        });
                    }
                    off += if l.is_finite() { l } else { 0.0 };
                }
            }
        };
        for f in &spec.area {
            let mut plan = Plan::default();
            for (cs, rule) in &f.layers {
                let set = rules.len() as u16;
                rules.push(*rule);
                plan.layers.push(set);
                add(cs, set, &mut src);
            }
            if !f.holes.is_empty() {
                let set = rules.len() as u16;
                rules.push(Rule::EvenOdd);
                plan.holes = Some(set);
                add(&f.holes, set, &mut src);
            }
            for (op, cs) in &f.ops {
                let set = rules.len() as u16;
                rules.push(Rule::EvenOdd);
                plan.ops.push((*op, set));
                add(cs, set, &mut src);
            }
            plans.push(plan);
            if rules.len() > u16::MAX as usize {
                return Err("The shape has too many contour sets.".into());
            }
        }
        let stroke_segs: usize = spec.strokes.iter().map(Contour::edges).sum();
        if src.len() + stroke_segs > SEG_CAP {
            return Err(too_many(src.len() + stroke_segs));
        }
        let mut g = Geom {
            segs: src.iter().map(|s| s.seg).collect(),
            rules,
            plans,
            place: spec.band.map_or(Place::Inside, |b| b.0),
            ..Geom::default()
        };
        g.seg_order = order_by(g.segs.len(), |i| g.segs[i].lo());
        if let Some((_, t)) = spec.band
            && t > 0.0
            && !g.segs.is_empty()
        {
            let r = if g.place == Place::Centred {
                t / 2.0
            } else {
                t
            };
            for e in g.boundary(&src)? {
                g.band.push(Part::Capsule(e.a, e.b, r));
                g.edges.push(e);
            }
        }
        let (h, join, cap) = spec.pen;
        for c in &spec.strokes {
            stroke(&c.pts, c.closed, h, join, cap, &mut g.strokes);
            let mut off = 0.0;
            for i in 0..c.edges() {
                let (a, b) = c.edge(i);
                let l = (b.0 - a.0).hypot(b.1 - a.1);
                if a != b && l.is_finite() {
                    g.edges.push(Edge {
                        a,
                        b,
                        tag: c.tags.get(i).copied().unwrap_or(0),
                        off,
                        chord: c.chords.get(i).copied().unwrap_or((a, b)),
                    });
                    off += l;
                }
            }
        }
        g.band_order = order_by(g.band.len(), |i| g.band[i].vrange().0);
        g.stroke_order = order_by(g.strokes.len(), |i| g.strokes[i].vrange().0);
        let mut bb = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let mut grow = |u: (f64, f64), v: (f64, f64)| {
            bb = [
                bb[0].min(u.0),
                bb[1].min(v.0),
                bb[2].max(u.1),
                bb[3].max(v.1),
            ];
        };
        for s in &g.segs {
            grow((s.a.0.min(s.b.0), s.a.0.max(s.b.0)), (s.lo(), s.hi()));
        }
        for p in g.band.iter().chain(&g.strokes) {
            grow(p.urange(), p.vrange());
        }
        g.bbox = bb;
        Ok(g)
    }

    /// Whether nothing can be drawn.
    pub fn is_empty(&self) -> bool {
        !self.bbox[0].is_finite()
    }

    /// An upper bound of the intervals one row gives (region, band and
    /// strokes together, before the classification at most doubles them).
    pub fn per_row(&self) -> f64 {
        (self.segs.len() + self.band.len() + self.strokes.len()) as f64 + 1.0
    }

    fn inside(&self, counts: &[i32]) -> bool {
        let ins = |s: u16| {
            let c = counts[usize::from(s)];
            match self.rules[usize::from(s)] {
                Rule::EvenOdd => c & 1 != 0,
                Rule::NonZero => c != 0,
            }
        };
        self.plans.iter().any(|p| {
            let mut v = p.layers.iter().any(|&s| ins(s));
            if let Some(h) = p.holes {
                v &= !ins(h);
            }
            for &(op, s) in &p.ops {
                match op {
                    Op::Union => v |= ins(s),
                    Op::Subtract => v &= !ins(s),
                    Op::Intersect => v &= ins(s),
                }
            }
            v
        })
    }

    /// Whether point `p` is in the area (a ray to its right; `index`: the
    /// segments by row bucket).
    fn contains(&self, p: P2, index: &Buckets, counts: &mut Vec<i32>) -> bool {
        counts.clear();
        counts.resize(self.rules.len(), 0);
        for &k in index.at(p.1) {
            let s = &self.segs[k as usize];
            if s.crosses(p.1) && s.x_at(p.1) > p.0 {
                counts[usize::from(s.set)] += s.dir();
            }
        }
        self.inside(counts)
    }

    /// The boundary: the pieces of the segments with the area on one side
    /// only.
    fn boundary(&self, src: &[Src]) -> Result<Vec<Edge>, String> {
        let n = self.segs.len();
        let mut splits: Vec<Vec<f64>> = vec![Vec::new(); n];
        let scale = self.segs.iter().fold(1.0f64, |m, s| {
            m.max(s.a.0.abs())
                .max(s.a.1.abs())
                .max(s.b.0.abs())
                .max(s.b.1.abs())
        });
        // Crossings: a sweep over the segments by their lowest row.
        let mut active: Vec<u32> = Vec::new();
        let mut pairs: u64 = 0;
        for &i in &self.seg_order {
            let si = self.segs[i as usize];
            let lo = si.lo();
            active.retain(|&j| self.segs[j as usize].hi() >= lo);
            let (ua, ub) = (si.a.0.min(si.b.0), si.a.0.max(si.b.0));
            for &j in &active {
                let sj = self.segs[j as usize];
                if sj.a.0.max(sj.b.0) < ua || sj.a.0.min(sj.b.0) > ub {
                    continue;
                }
                pairs += 1;
                if pairs > PAIR_CAP {
                    return Err(format!(
                        "The shape's outline would test more than {} pairs of segments for crossings — use fewer vertices, contours, symmetry copies or less detail.",
                        crate::midi::group_digits(PAIR_CAP)
                    ));
                }
                crossing(si, sj, scale, |t, u| {
                    splits[i as usize].push(t);
                    splits[j as usize].push(u);
                });
            }
            active.push(i);
        }
        let index = Buckets::of(&self.segs);
        let mut counts = Vec::new();
        let mut out = Vec::new();
        for (k, s) in self.segs.iter().enumerate() {
            let ts = &mut splits[k];
            ts.retain(|t| *t > 1e-12 && *t < 1.0 - 1e-12);
            ts.sort_by(f64::total_cmp);
            ts.dedup_by(|a, b| (*a - *b).abs() <= 1e-12);
            let at = |t: f64| (s.a.0 + (s.b.0 - s.a.0) * t, s.a.1 + (s.b.1 - s.a.1) * t);
            let mut prev = 0.0;
            for &t in ts.iter().chain(std::iter::once(&1.0)) {
                let (p, q) = (at(prev), at(t));
                let t0 = prev;
                prev = t;
                let (dx, dy) = (q.0 - p.0, q.1 - p.1);
                let l = dx.hypot(dy);
                if !(l > 0.0 && l.is_finite()) {
                    continue;
                }
                let m = ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
                let eps = (l * 1e-6).max(scale * 1e-13);
                let nv = (-dy / l * eps, dx / l * eps);
                let left = self.contains((m.0 + nv.0, m.1 + nv.1), &index, &mut counts);
                let right = self.contains((m.0 - nv.0, m.1 - nv.1), &index, &mut counts);
                if left != right {
                    let full = (s.b.0 - s.a.0).hypot(s.b.1 - s.a.1);
                    out.push(Edge {
                        a: p,
                        b: q,
                        tag: src[k].tag,
                        off: src[k].off + full * t0,
                        chord: src[k].chord,
                    });
                }
            }
        }
        Ok(out)
    }

    /// The edge and interior intervals of the row at local `y`, ascending
    /// and disjoint (`edge`: the band by its placement and the strokes;
    /// `interior`: the area less them).
    pub fn row(
        &self,
        y: f64,
        st: &mut RowState,
        b: &mut RowBuf,
        edge: &mut Vec<(f64, f64)>,
        interior: &mut Vec<(f64, f64)>,
    ) {
        edge.clear();
        interior.clear();
        // The area.
        b.region.clear();
        if !self.segs.is_empty() {
            let segs = &self.segs;
            st.segs.advance(
                y,
                &self.seg_order,
                |k| segs[k as usize].lo(),
                |k| segs[k as usize].hi(),
            );
            b.xs.clear();
            for &k in &st.segs.active {
                let s = &segs[k as usize];
                if s.crosses(y) {
                    b.xs.push((s.x_at(y), s.set, s.dir()));
                }
            }
            if !b.xs.is_empty() {
                b.xs.sort_unstable_by(|p, q| p.0.total_cmp(&q.0));
                b.counts.clear();
                b.counts.resize(self.rules.len(), 0);
                let mut open: Option<f64> = None;
                let mut i = 0;
                while i < b.xs.len() {
                    let x = b.xs[i].0;
                    while i < b.xs.len() && b.xs[i].0 == x {
                        b.counts[usize::from(b.xs[i].1)] += b.xs[i].2;
                        i += 1;
                    }
                    let inside = self.inside(&b.counts);
                    match (open, inside) {
                        (None, true) => open = Some(x),
                        (Some(s), false) => {
                            if x > s {
                                b.region.push((s, x));
                            }
                            open = None;
                        }
                        _ => {}
                    }
                }
            }
        }
        // The band and the strokes.
        spans(
            y,
            &self.band,
            &self.band_order,
            &mut st.band,
            &mut b.near,
            &mut b.tmp,
        );
        spans(
            y,
            &self.strokes,
            &self.stroke_order,
            &mut st.strokes,
            &mut b.strk,
            &mut b.tmp,
        );
        match self.place {
            Place::Inside => intersect(&b.region, &b.near, &mut b.tmp),
            Place::Outside => subtract(&b.near, &b.region, &mut b.tmp),
            Place::Centred => {
                b.tmp.clear();
                b.tmp.extend_from_slice(&b.near);
            }
        }
        union(&b.tmp, &b.strk, edge);
        if self.place == Place::Outside {
            subtract(&b.region, &b.strk, interior);
        } else {
            subtract(&b.region, &b.near, &mut b.tmp2);
            subtract(&b.tmp2, &b.strk, interior);
        }
    }
}

/// The union of the parts' intervals on row `y` into `out` (ascending,
/// disjoint).
fn spans(
    y: f64,
    parts: &[Part],
    order: &[u32],
    st: &mut Table,
    out: &mut Vec<(f64, f64)>,
    tmp: &mut Vec<(f64, f64)>,
) {
    out.clear();
    if parts.is_empty() {
        return;
    }
    st.advance(
        y,
        order,
        |k| parts[k as usize].vrange().0,
        |k| parts[k as usize].vrange().1,
    );
    tmp.clear();
    for &k in &st.active {
        if let Some(s) = parts[k as usize].span(y) {
            tmp.push(s);
        }
    }
    tmp.sort_unstable_by(|p, q| p.0.total_cmp(&q.0));
    for &(s, e) in tmp.iter() {
        match out.last_mut() {
            Some(l) if s <= l.1 => l.1 = l.1.max(e),
            _ => out.push((s, e)),
        }
    }
}

/// Calls `hit(t, u)` with the parameters along `a` and `b` where they cross
/// or touch (an overlap of collinear segments gives each one's ends lying
/// on the other).
fn crossing(a: Seg, b: Seg, scale: f64, mut hit: impl FnMut(f64, f64)) {
    let r = (a.b.0 - a.a.0, a.b.1 - a.a.1);
    let s = (b.b.0 - b.a.0, b.b.1 - b.a.1);
    let cr = |p: P2, q: P2| p.0 * q.1 - p.1 * q.0;
    let qp = (b.a.0 - a.a.0, b.a.1 - a.a.1);
    let den = cr(r, s);
    let (lr, ls) = (r.0.hypot(r.1), s.0.hypot(s.1));
    if !(lr > 0.0 && ls > 0.0) {
        return;
    }
    let tol = 1e-12;
    if den.abs() > tol * lr * ls {
        let t = cr(qp, s) / den;
        let u = cr(qp, r) / den;
        let (et, eu) = (tol * scale / lr, tol * scale / ls);
        if t >= -et && t <= 1.0 + et && u >= -eu && u <= 1.0 + eu {
            hit(t.clamp(0.0, 1.0), u.clamp(0.0, 1.0));
        }
        return;
    }
    // Parallel: collinear when b's start lies on a's line.
    if cr(qp, r).abs() > 1e-9 * scale * lr {
        return;
    }
    let proj = |p: P2, o: P2, d: P2, l2: f64| ((p.0 - o.0) * d.0 + (p.1 - o.1) * d.1) / l2;
    let (r2, s2) = (lr * lr, ls * ls);
    for p in [b.a, b.b] {
        let t = proj(p, a.a, r, r2);
        if t > 0.0 && t < 1.0 {
            hit(t, if p == b.a { 0.0 } else { 1.0 });
        }
    }
    for p in [a.a, a.b] {
        let u = proj(p, b.a, s, s2);
        if u > 0.0 && u < 1.0 {
            hit(if p == a.a { 0.0 } else { 1.0 }, u);
        }
    }
}

/// Segments by row bucket (for point tests).
struct Buckets {
    v0: f64,
    dv: f64,
    lists: Vec<Vec<u32>>,
}

impl Buckets {
    fn of(segs: &[Seg]) -> Buckets {
        let (lo, hi) = segs
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), s| {
                (l.min(s.lo()), h.max(s.hi()))
            });
        let k = (segs.len() / 4).clamp(1, 256);
        let dv = if hi > lo { (hi - lo) / k as f64 } else { 1.0 };
        let mut lists = vec![Vec::new(); k];
        if lo.is_finite() {
            for (i, s) in segs.iter().enumerate() {
                let a = (((s.lo() - lo) / dv).floor().max(0.0) as usize).min(k - 1);
                let b = (((s.hi() - lo) / dv).floor().max(0.0) as usize).min(k - 1);
                for l in &mut lists[a..=b] {
                    l.push(i as u32);
                }
            }
        }
        Buckets { v0: lo, dv, lists }
    }

    fn at(&self, y: f64) -> &[u32] {
        if !(y.is_finite() && self.v0.is_finite()) {
            return &[];
        }
        let k = ((y - self.v0) / self.dv).floor();
        if k < 0.0 || k >= self.lists.len() as f64 {
            return &[];
        }
        &self.lists[k as usize]
    }
}

/// `a ∪ b` (both ascending and disjoint) into `out`.
pub fn union(a: &[(f64, f64)], b: &[(f64, f64)], out: &mut Vec<(f64, f64)>) {
    out.clear();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let w = if j >= b.len() || (i < a.len() && a[i].0 <= b[j].0) {
            i += 1;
            a[i - 1]
        } else {
            j += 1;
            b[j - 1]
        };
        match out.last_mut() {
            Some(l) if w.0 <= l.1 => l.1 = l.1.max(w.1),
            _ => out.push(w),
        }
    }
}

/// `a ∩ b` into `out`.
pub fn intersect(a: &[(f64, f64)], b: &[(f64, f64)], out: &mut Vec<(f64, f64)>) {
    out.clear();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (s, e) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if e > s {
            out.push((s, e));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
}

/// `a \ b` into `out`.
pub fn subtract(a: &[(f64, f64)], b: &[(f64, f64)], out: &mut Vec<(f64, f64)>) {
    out.clear();
    let mut j = 0;
    for &(s0, e) in a {
        let mut s = s0;
        while j < b.len() && b[j].1 <= s {
            j += 1;
        }
        let mut k = j;
        while k < b.len() && b[k].0 < e {
            if b[k].0 > s {
                out.push((s, b[k].0));
            }
            s = s.max(b[k].1);
            if s >= e {
                break;
            }
            k += 1;
        }
        if e > s {
            out.push((s, e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Contour {
        Contour::polygon(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)], 0, true)
    }

    fn geom(f: Figure, band: Option<(Place, f64)>) -> Geom {
        Geom::build(&GeomSpec {
            area: vec![f],
            band,
            ..GeomSpec::default()
        })
        .unwrap()
    }

    type Ivs = Vec<(f64, f64)>;

    fn row(g: &Geom, y: f64) -> (Ivs, Ivs) {
        let (mut e, mut i) = (Vec::new(), Vec::new());
        g.row(
            y,
            &mut RowState::default(),
            &mut RowBuf::default(),
            &mut e,
            &mut i,
        );
        (e, i)
    }

    #[test]
    fn interval_sets_combine_exactly() {
        let a = [(0.0, 4.0), (6.0, 9.0)];
        let b = [(2.0, 7.0), (8.0, 10.0)];
        let mut o = Vec::new();
        union(&a, &b, &mut o);
        assert_eq!(o, vec![(0.0, 10.0)]);
        intersect(&a, &b, &mut o);
        assert_eq!(o, vec![(2.0, 4.0), (6.0, 7.0), (8.0, 9.0)]);
        subtract(&a, &b, &mut o);
        assert_eq!(o, vec![(0.0, 2.0), (7.0, 8.0)]);
    }

    #[test]
    fn a_hole_and_a_subtraction_cut_the_area_and_its_boundary_follows() {
        let f = Figure {
            layers: vec![(vec![square(0.0, 0.0, 10.0, 10.0)], Rule::EvenOdd)],
            holes: vec![square(3.0, 3.0, 5.0, 5.0)],
            ops: vec![(Op::Subtract, vec![square(8.0, -1.0, 12.0, 11.0)])],
        };
        let g = geom(f, Some((Place::Inside, 1.0)));
        let (e, i) = row(&g, 4.0);
        // Area [0, 3] ∪ [5, 8]; band within 1 of x = 0, 3, 5, 8.
        assert_eq!(e, vec![(0.0, 1.0), (2.0, 3.0), (5.0, 6.0), (7.0, 8.0)]);
        assert_eq!(i, vec![(1.0, 2.0), (6.0, 7.0)]);
        // The boundary has no piece where the subtracted box covers it.
        assert!(
            g.edges
                .iter()
                .all(|e| e.a.0 <= 8.0 + 1e-9 && e.b.0 <= 8.0 + 1e-9)
        );
    }

    #[test]
    fn a_union_draws_only_the_outer_boundary() {
        let f = Figure {
            layers: vec![(vec![square(0.0, 0.0, 6.0, 6.0)], Rule::NonZero)],
            holes: Vec::new(),
            ops: vec![(Op::Union, vec![square(4.0, 0.0, 10.0, 6.0)])],
        };
        let g = geom(f, Some((Place::Inside, 1.0)));
        let (e, i) = row(&g, 3.0);
        assert_eq!(e, vec![(0.0, 1.0), (9.0, 10.0)]);
        assert_eq!(i, vec![(1.0, 9.0)]);
    }

    #[test]
    fn rules_differ_on_overlapping_contours() {
        let cs = vec![square(0.0, 0.0, 6.0, 6.0), square(3.0, 0.0, 9.0, 6.0)];
        let even = geom(
            Figure {
                layers: vec![(cs.clone(), Rule::EvenOdd)],
                ..Figure::default()
            },
            None,
        );
        let nonzero = geom(
            Figure {
                layers: vec![(cs, Rule::NonZero)],
                ..Figure::default()
            },
            None,
        );
        assert_eq!(row(&even, 3.0).1, vec![(0.0, 3.0), (6.0, 9.0)]);
        assert_eq!(row(&nonzero, 3.0).1, vec![(0.0, 9.0)]);
    }

    #[test]
    fn a_capsule_meets_a_row_in_its_exact_interval() {
        // Horizontal: the body's full width; past the ends, the discs.
        assert_eq!(
            capsule_span((0.0, 0.0), (4.0, 0.0), 1.0, 0.0),
            Some((-1.0, 5.0))
        );
        let s = capsule_span((0.0, 0.0), (4.0, 0.0), 1.0, 0.6).unwrap();
        assert!((s.0 + 0.8).abs() < 1e-12 && (s.1 - 4.8).abs() < 1e-12);
        assert_eq!(capsule_span((0.0, 0.0), (4.0, 0.0), 1.0, 1.5), None);
        // Exactly r away: above counts, below does not.
        assert_eq!(
            capsule_span((0.0, 0.0), (4.0, 0.0), 0.5, 0.5),
            Some((0.0, 4.0))
        );
        assert_eq!(capsule_span((0.0, 0.0), (4.0, 0.0), 0.5, -0.5), None);
        // Vertical: one unit wide over its length.
        assert_eq!(
            capsule_span((2.0, 0.0), (2.0, 9.0), 0.5, 4.0),
            Some((1.5, 2.5))
        );
        // Diagonal: brute force on a fine grid.
        let (a, b, r) = ((0.0, 0.0), (6.0, 3.0), 1.3);
        for k in 0..60 {
            let y = -2.0 + k as f64 * 0.1;
            let d = |x: f64| {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                (x - a.0 - t * dx).hypot(y - a.1 - t * dy)
            };
            let xs: Vec<f64> = (0..20000)
                .map(|i| -3.0 + i as f64 * 0.0006)
                .filter(|&x| d(x) <= r)
                .collect();
            match capsule_span(a, b, r, y) {
                Some((lo, hi)) => {
                    assert!((xs[0] - lo).abs() < 1e-3 && (xs[xs.len() - 1] - hi).abs() < 1e-3);
                }
                None => assert!(xs.is_empty()),
            }
        }
    }

    #[test]
    fn strokes_join_and_cap_as_asked() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let width = |join, cap, y| {
            let mut parts = Vec::new();
            stroke(&pts, false, 1.0, join, cap, &mut parts);
            let mut o = Vec::new();
            let mut t = Table::default();
            let order = order_by(parts.len(), |i| parts[i].vrange().0);
            spans(y, &parts, &order, &mut t, &mut o, &mut Vec::new());
            o
        };
        // Butt: the first end stops at x = 0; square: one more half-width.
        assert_eq!(width(Join::Mitre, Cap::Butt, 0.0), vec![(0.0, 11.0)]);
        assert_eq!(width(Join::Mitre, Cap::Square, 0.0), vec![(-1.0, 11.0)]);
        // The mitre fills the outer corner; a bevel cuts it.
        assert_eq!(width(Join::Mitre, Cap::Butt, -0.9), vec![(0.0, 11.0)]);
        let bev = width(Join::Bevel, Cap::Butt, -0.9);
        assert!(bev[0].1 < 10.2, "{bev:?}");
    }
}
