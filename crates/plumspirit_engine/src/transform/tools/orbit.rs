//! Orbit's v0.9 kernel (V09-S40 step 5, `docs/TRANSFORM_ENGINE.md`
//! "Orbit"): the functions declared by id beside the v0.8 kernel (`xgeom.rs`
//! `orbit_build`, untouched — a module of its own so the v0.8 closure
//! compiles as before), with Control Sweeps per note.
//!
//! The copies sit on v0.8's rings (a1's superellipse, a2's arc, a3's rings,
//! a4's spiral, a9's eccentricity curve, a10's Lissajous path) — or on n01's
//! drawn path — at n06's constant arc length instead of a count, moved by
//! Kepler's second law (n03), turned by n09's precession and tilted in depth
//! (n02: the far side smaller and softer, hidden behind n08's planet); each
//! spins by n04, has n10's moons and n05's Lagrange points; n07 varies the
//! motif from copy to copy; n11 draws the orbit itself.

use super::super::geom::Geom;
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TransformError};
use super::xgeom::{rot, sub_stepped, vel_scaled};
use std::f64::consts::PI;
use std::sync::Arc;

/// The switches of the v0.9 functions.
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];

pub(super) const VARIATIONS: &[&str] =
    &["Rotate pitches", "Every other note", "Alternate inversion"];
pub(super) const ALONG: &[&str] = &["Rings", "Time"];

/// n06 / n11: the most copies (or trail notes) of one ring.
const MAX_PER_RING: usize = 4096;
/// The farthest a copy is placed (unit coordinates of the box).
const FAR: f64 = 1e12;

/// n11's trail points: (u, v, length in unit time).
type Trail = Vec<(f64, f64, f64)>;

/// One placed copy (`orbit_build`'s `Placed`) and its place on its ring
/// (n07's copy number).
#[derive(Clone, Copy, Debug)]
struct Pl {
    cx: f64,
    cy: f64,
    rot: f64,
    scale: f64,
    vel: f64,
    gate: f64,
    sub: i64,
    /// n07: the copy's number on its ring (`None` for the kept original).
    var: Option<usize>,
}

/// One note's settings: the placed copies and n07's variation, per Control
/// Sweep level; n11's trail (once per run).
#[derive(Clone, Debug)]
struct OCfg {
    placed: Arc<Vec<Pl>>,
    variation: Option<usize>,
    /// n11: points (u, v, length in unit time) and the velocity.
    trail: Option<(Arc<Trail>, u8)>,
}

impl OCfg {
    fn from(p: &Params, _b: &BuildCtx) -> OCfg {
        let k = p.int("copies").clamp(1, 64) as usize;
        let (cx, cy) = (p.float("cx") / 100.0, p.float("cy") / 100.0);
        let (rx, ry) = (p.float("rx") / 100.0, p.float("ry") / 100.0);
        let size = p.float("size") / 100.0;
        let sexp = p.adv(1).then(|| p.float("a1.exp").max(0.05));
        let (a0, a1, full) = if p.adv(2) {
            (
                p.float("a2.from") * PI / 180.0,
                p.float("a2.to") * PI / 180.0,
                false,
            )
        } else {
            (0.0, 2.0 * PI, true)
        };
        let (rings, rscale) = if p.adv(3) {
            (
                p.int("a3.rings").clamp(2, 8) as usize,
                p.float("a3.scale") / 100.0,
            )
        } else {
            (1, 1.0)
        };
        let spiral = p.adv(4).then(|| p.float("a4.end") / 100.0);
        let tangent = p.adv(5);
        let vcurve = p.adv(6).then(|| p.curve("a6.curve"));
        let gcurve = p.adv(7).then(|| p.curve("a7.curve"));
        let sectors = p.adv(8).then(|| p.int("a8.sectors").max(1) as usize);
        let ecc = p.adv(9).then(|| p.curve("a9.curve"));
        let lissa = p
            .adv(10)
            .then(|| (p.int("a10.p").max(1) as f64, p.int("a10.q").max(1) as f64));
        // v0.9.
        let path = p.flag("n01").then(|| (p.curve("n01.x"), p.curve("n01.y")));
        let tilt = p.flag("n02").then(|| {
            (
                p.float("n02.tilt").clamp(-89.0, 89.0) * PI / 180.0,
                (p.float("n02.depth") / 100.0).clamp(0.0, 1.0),
            )
        });
        let kepler = p.flag("n03").then(|| {
            (
                (p.float("n03.e") / 100.0).clamp(0.0, 0.95),
                p.float("n03.peri") * PI / 180.0,
            )
        });
        let spin = if p.flag("n04") {
            p.float("n04.spin") * PI / 180.0
        } else {
            0.0
        };
        let lagrange = p.flag("n05").then(|| {
            let mut pts: Vec<usize> = p
                .list("n05.points")
                .iter()
                .filter(|&&x| (1..=5).contains(&x))
                .map(|&x| x as usize)
                .collect();
            pts.dedup();
            (pts, (p.float("n05.mass") / 100.0).clamp(0.0001, 1.0))
        });
        let arc = p
            .flag("n06")
            .then(|| (p.float("n06.spacing") / 100.0).max(1e-3));
        let variation = p.flag("n07").then(|| p.choice("n07.mode").min(2));
        let body = p
            .flag("n08")
            .then(|| (p.float("n08.radius") / 100.0).max(0.0));
        let prec = p.flag("n09").then(|| {
            (
                p.float("n09.angle") * PI / 180.0,
                p.choice("n09.along") == 1,
            )
        });
        let moons = p.flag("n10").then(|| {
            (
                p.int("n10.moons").clamp(1, 8) as usize,
                p.float("n10.radius") / 100.0,
                p.float("n10.size") / 100.0,
            )
        });
        let line = p.flag("n11").then(|| {
            (
                p.int("n11.density").clamp(1, MAX_PER_RING as i64) as usize,
                p.int("n11.vel").clamp(1, 127) as u8,
            )
        });

        // The point of ring `rf` at parameter f (v0.8's arithmetic, then the
        // functions): offset from the centre, the angle, the depth z.
        let point = |f: f64, rf: f64, ring: usize| -> (f64, f64, f64, f64) {
            let mut th = a0 + (a1 - a0) * f;
            let mut r = rf;
            if let Some(end) = spiral {
                r *= 1.0 + (end - 1.0) * f;
            }
            let ryk = ecc.as_ref().map_or(ry, |c| ry * c.eval(f) * 2.0);
            if let Some((e, peri)) = kepler {
                // n03: equal times — the mean anomaly runs uniformly; the
                // radius and the true anomaly follow Kepler's equation.
                if e > 0.0 {
                    let m = th;
                    let mut ea = m;
                    for _ in 0..32 {
                        let d = (ea - e * ea.sin() - m) / (1.0 - e * ea.cos());
                        ea -= d;
                        if d.abs() < 1e-14 {
                            break;
                        }
                    }
                    let nu = 2.0
                        * ((1.0 + e).sqrt() * (ea / 2.0).sin())
                            .atan2((1.0 - e).sqrt() * (ea / 2.0).cos());
                    r *= 1.0 - e * ea.cos();
                    th = nu;
                }
                th += peri;
            }
            let (c, s) = match lissa {
                Some((pp, qq)) => ((pp * th).cos(), (qq * th).sin()),
                None => (th.cos(), th.sin()),
            };
            let (c, s) = match sexp {
                Some(e) => (
                    c.signum() * c.abs().powf(2.0 / e),
                    s.signum() * s.abs().powf(2.0 / e),
                ),
                None => (c, s),
            };
            let (c, s) = match &path {
                // n01: the drawn path (0 – 1 in the curves = −1 – 1 around
                // the centre), its direction for a5.
                Some((px, py)) => {
                    let (x, y) = (px.eval(f) * 2.0 - 1.0, py.eval(f) * 2.0 - 1.0);
                    let h = 1e-4;
                    let (x2, y2) = (
                        px.eval((f + h).min(1.0)) * 2.0 - 1.0,
                        py.eval((f + h).min(1.0)) * 2.0 - 1.0,
                    );
                    let dir = (y2 - y).atan2(x2 - x);
                    th = dir - PI / 2.0;
                    (x, y)
                }
                None => (c, s),
            };
            let (mut ox, mut oy) = (rx * r * c, ryk * r * s);
            if let Some((angle, along_time)) = prec {
                let phi = if along_time {
                    angle * f
                } else {
                    angle * ring as f64
                };
                (ox, oy) = rot(ox, oy, phi);
            }
            let mut z = 0.0;
            if let Some((t, _)) = tilt {
                z = s * t.sin();
                oy *= t.cos();
            }
            (ox, oy, th, z)
        };

        let mut placed: Vec<Pl> = Vec::new();
        if p.flag("keep") {
            placed.push(Pl {
                cx: 0.5,
                cy: 0.5,
                rot: 0.0,
                scale: 1.0,
                vel: 1.0,
                gate: 1.0,
                sub: 0,
                var: None,
            });
        }
        let mut trail: Vec<(f64, f64, f64)> = Vec::new();
        for ring in 0..rings {
            let rf = rscale.powi(ring as i32);
            let count = match arc {
                // n06: the ring's length (a polyline of 256 points) / the
                // spacing.
                Some(sp) => {
                    let mut len = 0.0;
                    let mut prev = point(0.0, rf, ring);
                    for j in 1..=256 {
                        let q = point(j as f64 / 256.0, rf, ring);
                        len += ((q.0 - prev.0).powi(2) + (q.1 - prev.1).powi(2)).sqrt();
                        prev = q;
                    }
                    ((len / sp).round() as usize).clamp(1, MAX_PER_RING)
                }
                None => k,
            };
            for i in 0..count {
                let f = if full {
                    i as f64 / count as f64
                } else if count == 1 {
                    0.0
                } else {
                    i as f64 / (count - 1) as f64
                };
                let (ox, oy, th, z) = point(f, rf, ring);
                if let Some(radius) = body {
                    // n08: behind the planet — the far side (with n02's tilt,
                    // else the upper half seen edge-on) within its radius.
                    // (1e-12: the copies on the line through the centre —
                    // sin π is 1.2e-16, not 0 — are beside it, not behind.)
                    let far = if tilt.is_some() {
                        z < -1e-12
                    } else {
                        oy > 1e-12
                    };
                    if far && (ox * ox + oy * oy).sqrt() < radius {
                        continue;
                    }
                }
                let depth = tilt.map_or(1.0, |(_, d)| (1.0 + d * z).max(0.01));
                let base = Pl {
                    cx: cx + ox,
                    cy: cy + oy,
                    rot: (if tangent { th + PI / 2.0 } else { 0.0 }) + spin * i as f64,
                    scale: size * depth,
                    vel: vcurve.as_ref().map_or(1.0, |cv| cv.eval(f)) * depth,
                    gate: gcurve.as_ref().map_or(1.0, |cv| cv.eval(f) * 2.0),
                    sub: sectors.map_or(0, |n| ((f * n as f64) as i64).min(n as i64 - 1)),
                    var: Some(i),
                };
                placed.push(base);
                if let Some((pts, mu)) = &lagrange {
                    // n05: the copy is the second body of a pair around the
                    // centre; its Lagrange points (the collinear ones by
                    // Hill's approximation, L4 / L5 at ±60°).
                    let d = (ox * ox + oy * oy).sqrt();
                    if d > 0.0 {
                        let (ux, uy) = (ox / d, oy / d);
                        let hill = (mu / 3.0).cbrt();
                        for &l in pts {
                            let (lx, ly) = match l {
                                1 => (ux * d * (1.0 - hill), uy * d * (1.0 - hill)),
                                2 => (ux * d * (1.0 + hill), uy * d * (1.0 + hill)),
                                3 => {
                                    let r3 = -d * (1.0 + 5.0 * mu / 12.0);
                                    (ux * r3, uy * r3)
                                }
                                4 => rot(ox, oy, PI / 3.0),
                                _ => rot(ox, oy, -PI / 3.0),
                            };
                            placed.push(Pl {
                                cx: cx + lx,
                                cy: cy + ly,
                                ..base
                            });
                        }
                    }
                }
                if let Some((n, mr, ms)) = moons {
                    // n10: N smaller copies around the copy.
                    for j in 0..n {
                        let a = 2.0 * PI * j as f64 / n as f64 + th;
                        placed.push(Pl {
                            cx: base.cx + mr * a.cos(),
                            cy: base.cy + mr * a.sin(),
                            scale: base.scale * ms,
                            ..base
                        });
                    }
                }
            }
            if let Some((density, _)) = line {
                // n11: the ring itself as a trail of `density` points.
                for j in 0..density {
                    let f = j as f64 / density as f64;
                    let f2 = (j + 1) as f64 / density as f64;
                    let (ox, oy, _, _) = point(f, rf, ring);
                    let (ox2, _, _, _) = point(f2, rf, ring);
                    trail.push((cx + ox, cy + oy, (ox2 - ox).abs()));
                }
            }
        }
        OCfg {
            placed: Arc::new(placed),
            variation,
            trail: line.map(|(_, v)| (Arc::new(trail), v)),
        }
    }
}

pub(super) struct Orb9 {
    base: OCfg,
    rebuild: Option<Arc<Rebuild>>,
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    growth: Growth,
    estimate: u64,
}

pub(super) fn orb9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let base = OCfg::from(p, b);
    let rebuild = Rebuild::of(p, b);
    let corners = match &rebuild {
        Some(r) => r.corners(b.ctx, OCfg::from),
        None => vec![base.clone()],
    };
    // The bound per note over every swept corner: each placed copy
    // rasterized; the trail once.
    let per = corners
        .iter()
        .map(|c| c.placed.len() as u64)
        .max()
        .unwrap_or(1)
        .max(1)
        .saturating_mul(g.per_note());
    let trail = corners
        .iter()
        .map(|c| c.trail.as_ref().map_or(0, |t| t.0.len() as u64))
        .max()
        .unwrap_or(0);
    let n = b.stats.count as u64;
    Ok(Box::new(Orb9 {
        base,
        rebuild,
        g,
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        growth: Growth::Expand(per),
        estimate: n.saturating_mul(per).saturating_add(trail),
    }))
}

impl Orb9 {
    fn levels(&self, src: &Source) -> PerLevel<OCfg> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, OCfg::from),
            None => PerLevel::one(self.base.clone()),
        }
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let lv = self.levels(src);
        let notes = &src.notes;
        // n07: each note's place in time order and the motif's pitches in
        // that order (rotate pitches, every other note).
        let (rank, seq) = if lv.all().iter().any(|c| c.variation.is_some()) {
            let mut idx: Vec<usize> = (0..notes.len()).collect();
            idx.sort_unstable_by_key(|&i| (notes[i].start, notes[i].pitch, i));
            let mut rank = vec![0usize; notes.len()];
            for (r, &i) in idx.iter().enumerate() {
                rank[i] = r;
            }
            (
                rank,
                idx.iter().map(|&i| notes[i].pitch).collect::<Vec<u8>>(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let (lo, hi) = (src.stats.min_pitch, src.stats.max_pitch);
        let g = &self.g;
        for (i, n) in notes.iter().enumerate() {
            let c = lv.at(i);
            for (pi, pl) in c.placed.iter().enumerate() {
                // V09-0143: the orbit copy (ring by ring).
                out.set_dim(u32::try_from(pi).unwrap_or(u32::MAX));
                let mut t = sub_stepped(&self.tracks, n, pl.sub);
                t.vel = vel_scaled(n, pl.vel);
                t.dur = ((n.dur as f64 * pl.gate).round() as u64).max(1);
                if let (Some(mode), Some(k)) = (c.variation, pl.var) {
                    match mode {
                        0 => t.pitch = seq[(rank[i] + k) % seq.len()],
                        1 => {
                            if rank[i] % 2 != k % 2 {
                                continue;
                            }
                        }
                        _ => {
                            if k % 2 == 1 {
                                t.pitch = (u16::from(lo) + u16::from(hi))
                                    .saturating_sub(u16::from(t.pitch))
                                    .min(255) as u8;
                            }
                        }
                    }
                }
                let ok = g.rasterize(
                    &t,
                    &|u, v| {
                        let (x, y) = rot((u - 0.5) * pl.scale, (v - 0.5) * pl.scale, pl.rot);
                        ((pl.cx + x).clamp(-FAR, FAR), (pl.cy + y).clamp(-FAR, FAR))
                    },
                    &self.tracks,
                    out,
                );
                if !ok {
                    return out.check();
                }
            }
            out.check()?;
        }
        // n11: the orbit line, once, from the first note's place (track,
        // subchannel, channel).
        if let (Some(first), Some((trail, vel))) = (notes.first(), lv.first().trail.as_ref()) {
            out.set_dim(0);
            let b = &g.bbox;
            for &(u, v, du) in trail.iter() {
                let (x, y) = b.from_unit(u.clamp(-FAR, FAR), v.clamp(-FAR, FAR));
                let len = (du * b.width()).round().max(1.0);
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                if !out.emit(
                    x.round() as i128,
                    len as i128,
                    y.floor() as i64,
                    i64::from(*vel),
                    first,
                ) {
                    return out.check();
                }
            }
        }
        Ok(())
    }
}

impl Kernel for Orb9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate(&self, stats: &super::super::SourceStats) -> super::super::Estimate {
        super::super::Estimate::for_notes(stats.count as u64, self.estimate)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)?;
        out.check()
    }
}
