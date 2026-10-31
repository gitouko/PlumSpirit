//! Shared drawing geometry of the generators (V09-S53, lifted from Lines so
//! every generator uses one copy; `docs/GENERATORS.md` "Shared drawing"):
//! points along a path ([`Pt`]), the generation box in note space
//! ([`BoxG`]), affine maps measured on the grid ([`Aff`]), dashes on the
//! resolution grid ([`Dash`]), the pieces a changing brush cuts a range into
//! ([`pieces`]) and brush gradients along a position ([`Grad`]).
//!
//! Note space: x = ticks, y = pitch rows (row r covers [r, r + 1), its
//! centre is r + ½). **Grid units**: a resolution step across, a row up —
//! lengths, angles and offsets "on the grid" are measured in them.

use super::Frame;
use super::pen::Brush;

/// The length (grid units) of the pieces a range is cut into when its brush
/// changes along it (gradients, taper).
pub const UNIT: f64 = 1.0;

/// Most pieces a drawn range is cut into when the brush changes along it:
/// one per grid unit up to this many, longer pieces beyond — a gradient has
/// at most a few hundred distinct brushes, so a long range does not cost
/// its length.
pub const MAX_PIECES: usize = 1024;

/// How many pieces a drawn range of `len` grid units is cut into.
pub fn pieces(len: f64) -> usize {
    if len.is_finite() {
        ((len / UNIT).ceil() as usize).clamp(1, MAX_PIECES)
    } else {
        1
    }
}

/// A point of a path: note space and its position `s` along the path
/// (0 – 1, for the gradients).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
    pub s: f64,
}

impl Pt {
    pub fn lerp(a: Pt, b: Pt, t: f64) -> Pt {
        Pt {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            s: a.s + (b.s - a.s) * t,
        }
    }

    /// Whether `o` is the same point (relative tolerance).
    pub fn near(self, o: Pt) -> bool {
        let tol = |v: f64| 1e-9 * v.abs().max(1.0);
        (self.x - o.x).abs() <= tol(self.x) && (self.y - o.y).abs() <= tol(self.y)
    }
}

/// The box in note space: ticks `[x0, x1]`, row centres `y0 … y1`, row
/// edges `lo … hi`, the grid step (ticks).
#[derive(Clone, Copy, Debug)]
pub struct BoxG {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
    pub lo: f64,
    pub hi: f64,
    pub step: f64,
}

impl BoxG {
    pub fn of(f: &Frame) -> BoxG {
        BoxG {
            x0: f.x(0.0),
            x1: f.x(1.0),
            y0: f.y(0.0),
            y1: f.y(1.0),
            lo: f64::from(f.lo),
            hi: f64::from(f.hi) + 1.0,
            step: f.step as f64,
        }
    }

    pub fn w(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn rows(&self) -> f64 {
        self.hi - self.lo
    }

    /// Grid length of a segment (a step across, a row up).
    pub fn len(&self, a: Pt, b: Pt) -> f64 {
        ((b.x - a.x) / self.step).hypot(b.y - a.y)
    }
}

/// An affine map of note space: `x' = a x + b y + e`, `y' = c x + d y + f`.
#[derive(Clone, Copy, Debug)]
pub struct Aff {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Aff {
    pub const ID: Aff = Aff {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(dx: f64, dy: f64) -> Aff {
        Aff {
            e: dx,
            f: dy,
            ..Aff::ID
        }
    }

    /// `self`, then `o`.
    pub fn then(self, o: Aff) -> Aff {
        Aff {
            a: o.a * self.a + o.b * self.c,
            b: o.a * self.b + o.b * self.d,
            c: o.c * self.a + o.d * self.c,
            d: o.c * self.b + o.d * self.d,
            e: o.a * self.e + o.b * self.f + o.e,
            f: o.c * self.e + o.d * self.f + o.f,
        }
    }

    /// Rotation by `t` radians about `(px, py)`, measured on the grid
    /// (`step` ticks across = one row up).
    pub fn rotate(t: f64, px: f64, py: f64, step: f64) -> Aff {
        let (s, c) = t.sin_cos();
        let r = Aff {
            a: c,
            b: -s * step,
            c: s / step,
            d: c,
            e: 0.0,
            f: 0.0,
        };
        Aff::translate(-px, -py)
            .then(r)
            .then(Aff::translate(px, py))
    }

    /// A horizontal shear by `t` radians about row coordinate `py`, on the
    /// grid: each row up moves `tan t` steps across.
    pub fn shear(t: f64, py: f64, step: f64) -> Aff {
        let k = t.tan() * step;
        Aff {
            b: k,
            e: -k * py,
            ..Aff::ID
        }
    }

    #[inline]
    pub fn ap(&self, p: Pt) -> Pt {
        Pt {
            x: self.a * p.x + self.b * p.y + self.e,
            y: self.c * p.x + self.d * p.y + self.f,
            s: p.s,
        }
    }

    /// The map applied to a point `(x, y)`.
    #[inline]
    pub fn xy(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            self.a * x + self.b * y + self.e,
            self.c * x + self.d * y + self.f,
        )
    }
}

/// The dashes on the resolution grid: a dash of `on` ticks every `period`
/// ticks from `x0` (both whole steps, the dash at least one, the gap at
/// least one — a gap rounding to nothing means no dashes), so consecutive
/// dashes never merge.
#[derive(Clone, Copy, Debug)]
pub struct Dash {
    pub on: f64,
    pub period: f64,
    pub x0: f64,
}

impl Dash {
    /// The dashes of `dash` / `gap` ticks from the frame's start on its
    /// grid, `None` without a gap.
    pub fn on_grid(dash: f64, gap: f64, f: &Frame) -> Option<Dash> {
        let step = f.step as f64;
        let on = (dash / step).round().max(1.0);
        let off = (gap / step).round();
        (off >= 1.0).then(|| Dash {
            on: on * step,
            period: (on + off) * step,
            x0: f.x(0.0),
        })
    }

    pub fn on_at(&self, x: f64) -> bool {
        (x - self.x0).rem_euclid(self.period) < self.on
    }
}

/// Brush gradients along a position `s` (0 = the start, 1 = the end):
/// gate (fractions; in whole percent, so equal neighbours merge),
/// velocity, subchannel position — `None` = the base brush's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Grad {
    pub gate: Option<(f64, f64)>,
    pub vel: Option<(f64, f64)>,
    pub sub: Option<(f64, f64)>,
}

impl Grad {
    /// Whether any gradient is on.
    pub fn varies(&self) -> bool {
        self.gate.is_some() || self.vel.is_some() || self.sub.is_some()
    }

    /// The brush at position `s` (clamped; not finite = 0) with the
    /// velocity scaled by `k` (a taper; 1 = none).
    pub fn brush(&self, s: f64, base: Brush, k: f64) -> Brush {
        self.brush_each([s; 3], base, k)
    }

    /// [`Grad::brush`] with each gradient at its own position: gate,
    /// velocity, subchannel.
    pub fn brush_each(&self, s: [f64; 3], base: Brush, k: f64) -> Brush {
        let at = |s: f64| {
            if s.is_finite() {
                s.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let mut b = base;
        if let Some((g0, g1)) = self.gate {
            let s = at(s[0]);
            // In whole percent, so equal neighbours merge.
            b.gate = ((g0 + (g1 - g0) * s) * 100.0).round().clamp(1.0, 100.0) / 100.0;
        }
        let mut v = match self.vel {
            Some((v0, v1)) => v0 + (v1 - v0) * at(s[1]),
            None => f64::from(base.vel),
        };
        v *= k;
        b.vel = v.round().clamp(1.0, 127.0) as u8;
        if let Some((a, z)) = self.sub {
            b.sub = (a + (z - a) * at(s[2])).round().clamp(0.0, 4095.0) as u32;
        }
        b
    }
}

// ----- curves (lifted from Arcs, V09-S57: Custom Shape flattens its
// Bézier edges and morphs by arc length with the same code)

/// The furthest a flattened curve's chord lies from the curve (grid units).
pub const SAG: f64 = 0.05;
/// Most points of one flattened curve.
pub const FLAT_CAP: usize = 4096;

fn lerp2(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// The curve flattened: parameters refined level by level (from 16
/// segments) where a segment's middle lies more than [`SAG`] from its chord,
/// at most [`FLAT_CAP`] points.
pub fn flatten(curve: &dyn Fn(f64) -> (f64, f64)) -> Vec<(f64, f64)> {
    flatten_t(curve).1
}

/// [`flatten`] with each point's parameter (V09-S60: the spirograph's
/// brushes read the pen's angle at each point).
pub fn flatten_t(curve: &dyn Fn(f64) -> (f64, f64)) -> (Vec<f64>, Vec<(f64, f64)>) {
    let mut ts: Vec<f64> = (0..=16).map(|i| f64::from(i) / 16.0).collect();
    let mut ps: Vec<(f64, f64)> = ts.iter().map(|&t| curve(t)).collect();
    for _ in 0..12 {
        let mut nt = Vec::with_capacity(ts.len() * 2);
        let mut np = Vec::with_capacity(ts.len() * 2);
        nt.push(ts[0]);
        np.push(ps[0]);
        let mut split = false;
        for i in 0..ts.len() - 1 {
            let left = ts.len() - 1 - i;
            if nt.len() + left < FLAT_CAP {
                let m = (ts[i] + ts[i + 1]) / 2.0;
                let pm = curve(m);
                let mid = lerp2(ps[i], ps[i + 1], 0.5);
                if dist(pm, mid) > SAG {
                    nt.push(m);
                    np.push(pm);
                    split = true;
                }
            }
            nt.push(ts[i + 1]);
            np.push(ps[i + 1]);
        }
        ts = nt;
        ps = np;
        if !split {
            break;
        }
    }
    (ts, ps)
}

/// A polyline's length (grid units).
pub fn length(p: &[Pt]) -> f64 {
    p.windows(2)
        .map(|w| dist((w[0].x, w[0].y), (w[1].x, w[1].y)))
        .sum()
}

/// `n + 1` points evenly along the polyline by length (positions kept).
pub fn resample(p: &[Pt], n: usize) -> Vec<Pt> {
    if p.len() < 2 || n == 0 {
        return p.to_vec();
    }
    let total = length(p);
    let mut out = Vec::with_capacity(n + 1);
    let mut i = 0;
    let mut acc = 0.0;
    for k in 0..=n {
        let want = total * k as f64 / n as f64;
        loop {
            let l = dist((p[i].x, p[i].y), (p[i + 1].x, p[i + 1].y));
            if acc + l >= want || i + 2 >= p.len() {
                let t = if l > 0.0 {
                    ((want - acc) / l).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                out.push(Pt::lerp(p[i], p[i + 1], t));
                break;
            }
            acc += l;
            i += 1;
        }
    }
    out
}

// ----- box shares (lifted from Parametric, V09-S59: the L-system places
// and cuts its drawing the same way)

/// Whether point `p` lies in window `w` (min x, min y, max x, max y).
pub fn inside(w: &[f64; 4], p: (f64, f64)) -> bool {
    p.0 >= w[0] && p.0 <= w[2] && p.1 >= w[1] && p.1 <= w[3]
}

/// The part `[a, b]` (0 – 1) of segment `p → q` inside window `w`.
pub fn clip_seg(w: &[f64; 4], p: (f64, f64), q: (f64, f64)) -> Option<(f64, f64)> {
    let (mut a, mut b) = (0.0f64, 1.0f64);
    let d = (q.0 - p.0, q.1 - p.1);
    for (pv, dv, lo, hi) in [(p.0, d.0, w[0], w[2]), (p.1, d.1, w[1], w[3])] {
        if dv == 0.0 {
            if pv < lo || pv > hi {
                return None;
            }
        } else {
            let (t0, t1) = ((lo - pv) / dv, (hi - pv) / dv);
            a = a.max(t0.min(t1));
            b = b.min(t0.max(t1));
        }
    }
    (b > a).then_some((a, b))
}

/// The box in grid space and the rotation about its centre.
#[derive(Clone, Copy, Debug)]
pub struct Gb {
    pub u0: f64,
    pub u1: f64,
    pub v0: f64,
    pub v1: f64,
    pub rot: (f64, f64),
}

impl Gb {
    pub fn of(g: &BoxG, angle: f64) -> Gb {
        let u0 = g.x0 / g.step;
        let end = g.x1 / g.step;
        Gb {
            u0,
            u1: (end - (end.abs() * 1e-12).max(1e-6)).max(u0),
            v0: g.y0,
            v1: g.y1,
            rot: angle.sin_cos(),
        }
    }

    /// Box shares on the grid.
    pub fn at(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            self.u0 + (self.u1 - self.u0) * x,
            self.v0 + (self.v1 - self.v0) * y,
        )
    }

    /// Turned about the box's centre.
    pub fn rot(&self, p: (f64, f64)) -> (f64, f64) {
        let (s, co) = self.rot;
        if s == 0.0 {
            return p;
        }
        let c = ((self.u0 + self.u1) / 2.0, (self.v0 + self.v1) / 2.0);
        let (dx, dy) = (p.0 - c.0, p.1 - c.1);
        (c.0 + dx * co - dy * s, c.1 + dx * s + dy * co)
    }
}

/// Positions along a piece by length (0 – 1).
pub fn positions(pts: &mut [Pt]) {
    let n = pts.len();
    let mut cum = vec![0.0; n];
    for i in 1..n {
        let (a, b) = (pts[i - 1], pts[i]);
        cum[i] = cum[i - 1] + (b.x - a.x).hypot(b.y - a.y);
    }
    let total = cum.last().copied().unwrap_or(0.0);
    for (i, q) in pts.iter_mut().enumerate() {
        q.s = if total > 0.0 {
            cum[i] / total
        } else {
            i as f64 / (n - 1).max(1) as f64
        };
    }
}
