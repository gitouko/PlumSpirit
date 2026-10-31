//! Flow Field's v0.9 kernel (V09-S41, `docs/TRANSFORM_ENGINE.md` "Flow
//! Field"): the functions declared by id beside the v0.8 kernel (`xfield.rs`
//! `flow_build`, untouched — it hands over when a new switch or a Control
//! Sweep is on), with Control Sweeps per note.
//!
//! v0.8's rule: every point of a note (its start, end and middle on the
//! shared raster) is advected through a 2-D field in unit coordinates of the
//! selection's box for the note's share of the flow time (a07's trail copies
//! for 1 / k … k / k of it), the field the sum of a01 – a04's (else a
//! meandering shear), a10's blend, a05's envelope at the note's time, a09's
//! turn per subchannel and the strength ([`Flow::sum`] / [`Flow::finish`],
//! shared with the v0.8 kernel). The v0.9 functions add fields (n01 the
//! slopes of a reference melody, n08 shear layers, n10 a drawn path), shape
//! it (n07 symmetry, n02 turning and drifting with time, n05 obstacles it
//! flows around, n09 a radial fade), stretch notes along it (n06), colour
//! notes by where the current brought them from (n03), draw streamlines
//! from a grid of seeds (n04) and thin where the flow piles notes up (n11).

use super::super::geom::{Geom, rem1};
use super::super::param::*;
use super::super::preflight::Estimate;
use super::super::registry::BuildCtx;
use super::super::shared::reference::reference_notes;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::{
    Ctx, Growth, Kernel, Output, Source, SourceStats, SourceTrack, TNote, TransformError,
};
use super::geometry::{permille, sub_at};
use super::xfield::{Flow, flow_of};
use super::xgeom::{mid_unit, rot};
use std::f64::consts::{FRAC_1_SQRT_2, TAU};
use std::sync::Arc;

/// The switches of the v0.9 functions.
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
pub(super) const REFERENCES: &[&str] = &["A track", "A subchannel"];
pub(super) const SYMMETRY: &[&str] = &["None", "Mirror in time", "Mirror in pitch", "Rotational"];

/// n01's melody table (samples across the box).
const CONTOUR_SAMPLES: usize = 512;

/// n10's drawn path (unit points).
type Path = Vec<(f64, f64)>;

#[derive(Clone, Debug)]
struct FCfg {
    base: Arc<Flow>,
    copies: usize,
    /// n01: the reference's smoothed melody slope (dv / du per sample).
    contour: Option<f64>,
    /// n02: turns over the selection, drift (unit).
    evolve: Option<(f64, f64)>,
    /// n03: palette (subchannel positions), steps upstream.
    advect_colour: Option<(Arc<Vec<i64>>, u32)>,
    /// n04: seed spacing, line length (flow time), notes per line.
    streams: Option<(f64, f64, u32)>,
    /// n05: obstacle radius (rows).
    obstacles: Option<f64>,
    /// n06: streak.
    streak: f64,
    /// n07: 1 time, 2 pitch, 3 rotational; order.
    symmetry: Option<(usize, u32)>,
    /// n08: layers, curl.
    layers: Option<(u32, f64)>,
    /// n09: radius (of the half diagonal), fade curve.
    fade: Option<(f64, Curve)>,
    /// n10: the path's points (unit), width (unit).
    path: Option<(Arc<Path>, f64)>,
    /// n11: most notes per cell, cell (rows).
    thin: Option<(u32, u32)>,
}

impl FCfg {
    fn from(p: &Params, b: &BuildCtx) -> FCfg {
        let pc = |k: &str| p.float(k) / 100.0;
        FCfg {
            base: Arc::new(flow_of(p, b)),
            copies: if p.adv(7) {
                p.int("a7.count").clamp(2, 32) as usize
            } else {
                1
            },
            contour: p.flag("n01").then(|| p.ticks("n01.smoothing") as f64),
            evolve: p.flag("n02").then(|| {
                (
                    p.float("n02.turns").clamp(-8.0, 8.0),
                    pc("n02.drift").clamp(0.0, 1.0),
                )
            }),
            advect_colour: p.flag("n03").then(|| {
                (
                    Arc::new(p.list("n03.palette")),
                    p.int("n03.steps").clamp(1, 64) as u32,
                )
            }),
            streams: p.flag("n04").then(|| {
                (
                    pc("n04.spacing").clamp(0.01, 1.0),
                    pc("n04.length").clamp(0.01, 1.0),
                    p.int("n04.notes").clamp(2, 256) as u32,
                )
            }),
            obstacles: p
                .flag("n05")
                .then(|| p.int("n05.radius").clamp(1, 16) as f64),
            streak: if p.flag("n06") {
                pc("n06.streak").clamp(0.0, 4.0)
            } else {
                0.0
            },
            symmetry: p
                .flag("n07")
                .then(|| {
                    (
                        p.choice("n07.symmetry").min(3),
                        p.int("n07.order").clamp(2, 12) as u32,
                    )
                })
                .filter(|s| s.0 > 0),
            layers: p.flag("n08").then(|| {
                (
                    p.int("n08.layers").clamp(2, 16) as u32,
                    pc("n08.curl").clamp(0.0, 1.0),
                )
            }),
            fade: p
                .flag("n09")
                .then(|| (pc("n09.radius").clamp(0.01, 2.0), p.curve("n09.fade"))),
            path: p.flag("n10").then(|| {
                let v = permille(&p.list("n10.path"));
                let pts: Vec<(f64, f64)> = v.chunks_exact(2).map(|c| (c[0], c[1])).collect();
                (Arc::new(pts), pc("n10.width").clamp(0.01, 1.0))
            }),
            thin: p.flag("n11").then(|| {
                (
                    p.int("n11.most").clamp(1, 1000) as u32,
                    p.int("n11.cell").clamp(1, 16) as u32,
                )
            }),
        }
    }

    /// n04: the streamlines' notes (0 without).
    fn stream_notes(&self) -> u64 {
        self.streams.map_or(0, |(s, _, m)| {
            let side = (1.0 / s).ceil() as u64;
            (side * side).saturating_mul(u64::from(m))
        })
    }
}

/// The reference layers: n01's melody (aux 0) and n05's obstacles (aux 1).
const AUX_CONTOUR: usize = 0;
const AUX_OBSTACLES: usize = 1;

/// The per-run tables the field reads: n01's slope table, n05's obstacle
/// buckets.
#[derive(Default)]
struct Tables {
    slope: Vec<f64>,
    obstacles: Vec<(f64, f64)>,
    /// Buckets of `obstacles` on a grid of `cell` (unit): (column, row) →
    /// indices, flattened by `side`.
    cell: f64,
    side: usize,
    buckets: Vec<Vec<u32>>,
}

pub(super) struct Flow9 {
    base: FCfg,
    rebuild: Option<Arc<Rebuild>>,
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    per: u64,
    extra: u64,
}

pub(super) fn flow9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let base = FCfg::from(p, b);
    let rebuild = Rebuild::of(p, b);
    let corners = match &rebuild {
        Some(r) => r.corners(b.ctx, FCfg::from),
        None => vec![base.clone()],
    };
    let per = corners
        .iter()
        .map(|c| c.copies as u64)
        .max()
        .unwrap_or(1)
        .saturating_mul(g.per_note());
    // The streamlines are drawn with the base settings: at most 100 × 100
    // seeds × 256 notes (the controls' ranges), counted in the bound.
    let extra = base.stream_notes();
    Ok(Box::new(Flow9 {
        base,
        rebuild,
        g,
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        per,
        extra,
    }))
}

/// n01 / n05: reads the reference melody (aux 0) and the obstacles (aux 1).
pub fn prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
) -> Result<(), TransformError> {
    if !(p.flag("n01") || p.flag("n05")) || src.stats.is_empty() {
        return Ok(());
    }
    let read = |on: &str, r: &str, num: &str, what: &str| -> Result<Vec<TNote>, TransformError> {
        if !p.flag(on) {
            return Ok(Vec::new());
        }
        reference_notes(
            doc,
            src,
            p.choice(r) == 1,
            p.int(num),
            0,
            u64::MAX,
            limit,
            what,
        )
    };
    let contour = read("n01", "n01.ref", "n01.num", "Contour field")?;
    let obstacles = read("n05", "n05.ref", "n05.num", "Obstacles")?;
    src.aux = Arc::new(vec![contour, obstacles]);
    Ok(())
}

impl Flow9 {
    fn levels(&self, src: &Source) -> PerLevel<FCfg> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, FCfg::from),
            None => PerLevel::one(self.base.clone()),
        }
    }

    /// n01's slope table and n05's obstacle buckets for `src`.
    fn tables(&self, src: &Source, lv: &PerLevel<FCfg>) -> Tables {
        let mut t = Tables::default();
        let b = &self.g.bbox;
        let aux = |k: usize| src.aux.get(k).map_or(&[][..], Vec::as_slice);
        if let Some(smooth) = lv.all().iter().find_map(|c| c.contour) {
            let refs = aux(AUX_CONTOUR);
            if !refs.is_empty() {
                // The melody: a line through the onsets (a chord's mean
                // pitch), flat before the first and after the last, averaged
                // over the smoothing window (16 points), sampled across the
                // box.
                let mut on: Vec<(f64, f64)> = Vec::new();
                for (tk, p) in refs.iter().map(|n| (n.start as f64, f64::from(n.pitch))) {
                    on.push((tk, p));
                }
                on.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut line: Vec<(f64, f64)> = Vec::new();
                let mut i = 0;
                while i < on.len() {
                    let mut j = i;
                    let mut sum = 0.0;
                    while j < on.len() && on[j].0 == on[i].0 {
                        sum += on[j].1;
                        j += 1;
                    }
                    line.push((on[i].0, sum / (j - i) as f64));
                    i = j;
                }
                let at = |tk: f64| -> f64 {
                    let j = line.partition_point(|q| q.0 <= tk);
                    if j == 0 {
                        line[0].1
                    } else if j == line.len() {
                        line[j - 1].1
                    } else {
                        let ((t0, p0), (t1, p1)) = (line[j - 1], line[j]);
                        p0 + (p1 - p0) * (tk - t0) / (t1 - t0)
                    }
                };
                let k = CONTOUR_SAMPLES;
                let melody: Vec<f64> = (0..k)
                    .map(|i| {
                        let tick = b.t0 as f64 + b.width() * i as f64 / (k - 1) as f64;
                        if smooth <= 0.0 {
                            return at(tick);
                        }
                        (0..16)
                            .map(|q| at(tick + smooth * ((f64::from(q) + 0.5) / 16.0 - 0.5)))
                            .sum::<f64>()
                            / 16.0
                    })
                    .collect();
                let du = 1.0 / (k - 1) as f64;
                t.slope = (0..k)
                    .map(|i| {
                        let (a, c) = (i.saturating_sub(1), (i + 1).min(k - 1));
                        let dv = (melody[c] - melody[a]) / b.rows();
                        dv / (du * (c - a).max(1) as f64)
                    })
                    .collect();
            }
        }
        if let Some(radius) = lv.all().iter().find_map(|c| c.obstacles) {
            let obs = aux(AUX_OBSTACLES);
            if !obs.is_empty() {
                let r = radius / b.rows();
                t.obstacles = obs
                    .iter()
                    .map(|n| {
                        b.to_unit(
                            (n.start as f64 + n.end() as f64) / 2.0,
                            f64::from(n.pitch) + 0.5,
                        )
                    })
                    .filter(|p| p.0.is_finite() && p.1.is_finite())
                    .collect();
                // Buckets of three radii over [-1, 2]² (beyond, ignored).
                t.cell = (3.0 * r).max(1e-3);
                t.side = ((3.0 / t.cell).ceil() as usize).clamp(1, 1024);
                t.buckets = vec![Vec::new(); t.side * t.side];
                for (i, &(x, y)) in t.obstacles.iter().enumerate() {
                    if let Some(k) = Self::bucket(&t, x, y) {
                        t.buckets[k].push(i as u32);
                    }
                }
            }
        }
        t
    }

    fn bucket(t: &Tables, x: f64, y: f64) -> Option<usize> {
        let (c, r) = ((x + 1.0) / t.cell, (y + 1.0) / t.cell);
        if !(c >= 0.0 && r >= 0.0) {
            return None;
        }
        let (c, r) = (c as usize, r as usize);
        (c < t.side && r < t.side).then_some(r * t.side + c)
    }

    /// The field at (u, v) for a note at time `t` (0 – 1) with turn `turn`.
    fn field(&self, c: &FCfg, tb: &Tables, u: f64, v: f64, t: f64, turn: f64) -> (f64, f64) {
        let fl = &c.base;
        // n02: the field drifts with time.
        let (mut u, mut v) = match c.evolve {
            Some((_, drift)) if drift != 0.0 => (u - drift * t, v),
            _ => (u, v),
        };
        // n07: the point in the fundamental part, and how to bring the
        // vector back.
        let mut back: Option<(bool, bool, f64)> = None;
        if let Some((kind, order)) = c.symmetry {
            match kind {
                1 if u > 0.5 => {
                    u = 1.0 - u;
                    back = Some((true, false, 0.0));
                }
                2 if v > 0.5 => {
                    v = 1.0 - v;
                    back = Some((false, true, 0.0));
                }
                3 => {
                    let sector = TAU / f64::from(order);
                    let a = (v - 0.5).atan2(u - 0.5);
                    let k = (a.rem_euclid(TAU) / sector).floor();
                    let (x, y) = rot(u - 0.5, v - 0.5, -k * sector);
                    (u, v) = (0.5 + x, 0.5 + y);
                    back = Some((false, false, k * sector));
                }
                _ => {}
            }
        }
        let (mut fx, mut fy, mut any) = fl.sum(u, v);
        if let Some(_smooth) = c.contour
            && !tb.slope.is_empty()
        {
            let k = tb.slope.len();
            let x = u.clamp(0.0, 1.0) * (k - 1) as f64;
            let i = (x.floor() as usize).min(k - 1);
            let j = (i + 1).min(k - 1);
            let s = tb.slope[i] + (tb.slope[j] - tb.slope[i]) * (x - i as f64);
            let n = 1.0f64.hypot(s);
            fx += 1.0 / n;
            fy += s / n;
            any = true;
        }
        if let Some((n, curl)) = c.layers {
            // Bands of opposite flow; their interfaces roll up.
            let q = v * f64::from(n);
            let band = q.floor();
            let sign = if band.rem_euclid(2.0) == 0.0 {
                1.0
            } else {
                -1.0
            };
            let d = (q - band).min(1.0 - (q - band));
            fx += sign;
            fy += curl * (TAU * 2.0 * f64::from(n) * u).sin() * (-(4.0 * d).powi(2)).exp();
            any = true;
        }
        if let Some((pts, width)) = &c.path
            && pts.len() >= 2
        {
            // The tangent of the nearest segment, by a Gaussian of the
            // distance.
            let mut best = (f64::INFINITY, 0.0, 0.0);
            for w in pts.windows(2) {
                let ((ax, ay), (bx, by)) = (w[0], w[1]);
                let (dx, dy) = (bx - ax, by - ay);
                let l2 = dx * dx + dy * dy;
                if l2 <= 0.0 {
                    continue;
                }
                let k = (((u - ax) * dx + (v - ay) * dy) / l2).clamp(0.0, 1.0);
                let d = (u - ax - k * dx).hypot(v - ay - k * dy);
                if d < best.0 {
                    let l = l2.sqrt();
                    best = (d, dx / l, dy / l);
                }
            }
            if best.0.is_finite() {
                let g = (-(best.0 / width).powi(2)).exp();
                fx += best.1 * g;
                fy += best.2 * g;
            }
            any = true;
        }
        let (mut fx, mut fy) = fl.finish((fx, fy, any), u, v, t, turn);
        if let Some((tm, pm, angle)) = back {
            if tm {
                fx = -fx;
            }
            if pm {
                fy = -fy;
            }
            if angle != 0.0 {
                (fx, fy) = rot(fx, fy, angle);
            }
            // Back to the point itself (the obstacles and the fade below).
            (u, v) = match c.symmetry {
                Some((1, _)) => (1.0 - u, v),
                Some((2, _)) => (u, 1.0 - v),
                Some((_, _)) => {
                    let (x, y) = rot(u - 0.5, v - 0.5, angle);
                    (0.5 + x, 0.5 + y)
                }
                None => (u, v),
            };
        }
        if let Some((turns, _)) = c.evolve
            && turns != 0.0
        {
            (fx, fy) = rot(fx, fy, TAU * turns * t);
        }
        if c.obstacles.is_some() && !tb.obstacles.is_empty() {
            let r = c.obstacles.unwrap_or(1.0) / self.g.bbox.rows();
            if let Some(k) = Self::bucket(tb, u, v) {
                let (bc, br) = (k % tb.side, k / tb.side);
                for rr in br.saturating_sub(1)..=(br + 1).min(tb.side - 1) {
                    for cc in bc.saturating_sub(1)..=(bc + 1).min(tb.side - 1) {
                        for &i in &tb.buckets[rr * tb.side + cc] {
                            let (ox, oy) = tb.obstacles[i as usize];
                            let (dx, dy) = (u - ox, v - oy);
                            let d = dx.hypot(dy);
                            if d > 3.0 * r || d <= 1e-12 {
                                continue;
                            }
                            let (nx, ny) = (dx / d, dy / d);
                            let vn = fx * nx + fy * ny;
                            if d < r {
                                // Inside: pushed out at the flow's speed.
                                let s = fx.hypot(fy);
                                fx = nx * s;
                                fy = ny * s;
                            } else if vn < 0.0 {
                                // Approaching: the normal part removed by
                                // (r / d)², so the flow slides around it.
                                let w = (r / d).powi(2);
                                fx -= w * vn * nx;
                                fy -= w * vn * ny;
                            }
                        }
                    }
                }
            }
        }
        if let Some((radius, curve)) = &c.fade {
            let d = (u - 0.5).hypot(v - 0.5) / FRAC_1_SQRT_2;
            let m = curve.eval((d / radius).min(1.0));
            fx *= m;
            fy *= m;
        }
        (fx, fy)
    }

    /// v0.8's integration of (u, v) for `frac` of the flow time (midpoint
    /// steps) through the v0.9 field; `dir` −1 integrates upstream.
    #[allow(clippy::too_many_arguments)]
    fn advect(
        &self,
        c: &FCfg,
        tb: &Tables,
        turn: f64,
        u: f64,
        v: f64,
        t: f64,
        frac: f64,
        dir: f64,
    ) -> (f64, f64) {
        let fl = &c.base;
        let steps = ((fl.steps as f64 * frac).round() as usize).max(1);
        let dt = frac / steps as f64 * dir;
        let (mut x, mut y) = (u, v);
        for _ in 0..steps {
            if fl.steps == 1 {
                let (fx, fy) = self.field(c, tb, x, y, t, turn);
                x += fx * dt;
                y += fy * dt;
            } else {
                let (fx, fy) = self.field(c, tb, x, y, t, turn);
                let (mx, my) = (x + fx * dt / 2.0, y + fy * dt / 2.0);
                let (gx, gy) = self.field(c, tb, mx, my, t, turn);
                x += gx * dt;
                y += gy * dt;
            }
            if fl.wrap {
                x = rem1(x);
                y = rem1(y);
            }
        }
        (x, y)
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let lv = self.levels(src);
        let tb = self.tables(src, &lv);
        let g = &self.g;
        let start = out.notes.len();
        for (i, n) in src.notes.iter().enumerate() {
            let c = lv.at(i);
            let fl = &c.base;
            let turn = fl.turn_of(n);
            let (mu, mv) = mid_unit(g, n);
            let t = mu.clamp(0.0, 1.0);
            let mut tpl = *n;
            if let Some((palette, steps)) = &c.advect_colour {
                // n03: the band of the place the current brought the note
                // from, `steps` steps upstream.
                let mut p = (mu, mv);
                for _ in 0..*steps {
                    p = self.advect(c, &tb, turn, p.0, p.1, t, 1.0 / f64::from(*steps), -1.0);
                }
                let q = p.0.clamp(0.0, 1.0);
                let len = palette.len().max(1);
                let band = ((q * len as f64) as usize).min(len - 1);
                let pos = palette.get(band).copied().unwrap_or(band as i64);
                (tpl.sub, tpl.ch) = sub_at(&self.tracks, n, pos);
            }
            let (us, ue) = (
                g.bbox.to_unit(n.start as f64, 0.0).0,
                g.bbox.to_unit(n.end() as f64, 0.0).0,
            );
            for k in 0..c.copies {
                // V09-0143: the trail copy (0 = the least advected).
                out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
                let frac = (k + 1) as f64 / c.copies as f64;
                let map = |u: f64, v: f64| {
                    // n06: points further along the note flow longer.
                    let f = if c.streak > 0.0 && ue > us {
                        frac * (1.0 + c.streak * ((u - us) / (ue - us)).clamp(0.0, 1.0))
                    } else {
                        frac
                    };
                    self.advect(c, &tb, turn, u, v, t, f, 1.0)
                };
                if !g.rasterize(&tpl, &map, &self.tracks, out) {
                    return out.check();
                }
            }
            out.check()?;
        }
        // n04: streamlines from a grid of seeds (the base settings).
        if let Some((spacing, length, m)) = self.base.streams
            && !src.notes.is_empty()
        {
            let c = &self.base;
            let b = &g.bbox;
            let side = (1.0 / spacing).ceil() as usize;
            let step = length / f64::from(m - 1);
            let each = (b.width() * length / f64::from(m)).round().max(1.0);
            for gy in 0..side {
                for gx in 0..side {
                    let seed = ((gx as f64 + 0.5) * spacing, (gy as f64 + 0.5) * spacing);
                    let tick = b.from_unit(seed.0, seed.1).0;
                    // The nearest source note in time gives the attributes.
                    let j = src
                        .notes
                        .partition_point(|q| (q.start as f64) < tick)
                        .min(src.notes.len() - 1);
                    let tpl = src.notes[j];
                    let turn = c.base.turn_of(&tpl);
                    // Grid streamlines: dimension 0.
                    out.set_dim(0);
                    let mut p = seed;
                    for q in 0..m {
                        if q > 0 {
                            p = self.advect(
                                c,
                                &tb,
                                turn,
                                p.0,
                                p.1,
                                seed.0.clamp(0.0, 1.0),
                                step,
                                1.0,
                            );
                        }
                        let (x, y) = b.from_unit(p.0, p.1);
                        if !out.emit(
                            x.round() as i128,
                            each as i128,
                            y.floor() as i64,
                            i64::from(tpl.vel),
                            &tpl,
                        ) {
                            return out.check();
                        }
                    }
                }
            }
        }
        // n11: at most `most` notes a cell (`cell` rows high, as many rows'
        // worth of ticks wide); the first ones kept.
        if let Some((most, cell)) = lv.all().iter().find_map(|c| c.thin) {
            let b = &g.bbox;
            let w = (b.width() / b.rows() * f64::from(cell)).max(1.0);
            let mut seen: std::collections::HashMap<(i64, i64), u32> =
                std::collections::HashMap::new();
            out.retain_notes_from(start, |nt| {
                let key = (
                    (nt.start as f64 / w).floor() as i64,
                    i64::from(nt.pitch) / i64::from(cell),
                );
                let c = seen.entry(key).or_insert(0);
                if *c < most {
                    *c += 1;
                    true
                } else {
                    false
                }
            });
        }
        Ok(())
    }
}

impl Kernel for Flow9 {
    fn growth(&self) -> Growth {
        Growth::Expand(self.per)
    }

    fn estimate(&self, stats: &SourceStats) -> Estimate {
        let n = stats.count as u64;
        Estimate::for_notes(n, n.saturating_mul(self.per).saturating_add(self.extra))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)?;
        out.check()
    }
}
