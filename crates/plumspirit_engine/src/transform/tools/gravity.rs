//! Gravity Well's v0.9 kernel (V09-S40 step 1, `docs/TRANSFORM_ENGINE.md`
//! "Gravity Well"): the functions declared by id beside the v0.8 kernel
//! (`xgeom.rs` `gravity_build`, untouched — a module of its own so the v0.8
//! closure compiles as before), with Control Sweeps per note.
//!
//! The wells are v0.8's (Well X / Y, a1's, a2's signs, a10's path) unless a
//! later stage places them: n07 at the densest windows, n08 at bars,
//! markers or chord roots, n09 as a pair orbiting each other (the latest
//! stage wins). Every point of a note then passes, in catalog order: n01's
//! N-body move (the notes attract each other on a coarse grid), n02's lens
//! (pushed out to the Einstein ring), n03's tidal stretch and n04's
//! accretion spiral around the nearest well, the v0.8 pull (n10's force
//! profile replaces its falloff), n05's escape (loud notes fly straight off
//! instead of being pulled), n11's time dilation; n06 sets the velocity
//! from the potential at the note's new place.

use super::super::geom::{Fill, Geom};
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::rng::{Rng, salt};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::metric_position::bar_bounds;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::geometry::{permille, sub_pos};
use super::xgeom::{area_scale, mid_unit, rot};
use super::{Contextual, boxed, per_track};
use std::f64::consts::PI;

/// The switches of the v0.9 functions.
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];

/// n08: where the musical wells sit.
pub(super) const MUSICAL_AT: &[&str] = &["Every bar", "Every marker", "Every chord root"];

/// n10's default profile: pull at the core, a ring of push, a ring of pull,
/// nothing at the reach.
pub(super) fn default_profile() -> Curve {
    Curve::new(vec![(0.0, 1.0), (0.35, 0.2), (0.7, 0.8), (1.0, 0.5)])
}

/// n01: cells per side of the N-body grid (the forces are summed between
/// cell masses, not note pairs).
const NBODY_GRID: usize = 32;

/// n08: the musical wells a note feels on each side of it in time (the
/// nearest; a song has thousands of bars or chords).
const NEAREST: usize = 2;

/// n08: at most this many musical wells.
const MAX_MUSICAL: usize = 1 << 20;

/// A well: x, y (box units), sign (1 attracts, −1 repels).
type Well = (f64, f64, f64);

/// One note's values of the parameters the Control Sweeps drive (box
/// units; per note inside the kernel, `definition::kernel_sweeps`).
#[derive(Clone, Copy, Debug)]
struct GVals {
    strength: f64,
    radius: f64,
    /// n02: Einstein radius.
    einstein: f64,
    /// n03: tidal strength (0 – 1).
    tidal: f64,
    /// n04: turns.
    turns: f64,
    /// n05: escape velocity.
    escape: i64,
    /// n06: gain at the core (−1 – 2).
    gain: f64,
    /// n09: separation.
    sep: f64,
    /// n10: reach.
    reach: f64,
    /// n11: dilation (0 – 1).
    dilation: f64,
}

/// Where the wells come from (the latest stage that is on).
#[derive(Clone, Debug)]
enum Source9 {
    /// v0.8's wells (a10's path moves them).
    V08,
    /// n07: fixed wells at the densest windows.
    Density(Vec<Well>),
    /// n08: wells in time order; each note feels the nearest.
    Musical(Vec<Well>),
    /// n09: two wells orbiting the main well by `turns` over the selection.
    Binary(f64),
}

#[derive(Clone)]
struct Grav9 {
    g: Geom,
    /// a9's filling geometry.
    gf: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    seed: u64,
    // ----- the v0.8 rule (`xgeom.rs` `Gravity`, re-read unchanged)
    wells: Vec<Well>,
    /// a2's signs (cycled over the new wells too).
    signs: Vec<f64>,
    exp: f64,
    capture: Option<(f64, f64)>,
    horizon: Option<f64>,
    pitch_only: bool,
    time_only: bool,
    mass: Vec<f64>,
    path: Option<(f64, f64)>,
    density: bool,
    base: GVals,
    // ----- v0.9 functions
    /// n01: steps, coupling, softening.
    nbody: Option<(usize, f64, f64)>,
    /// n02: ring velocity factor.
    lens: Option<f64>,
    /// n03: length stretch factor.
    tidal: Option<f64>,
    /// n04: infall (0 – 1).
    accretion: Option<f64>,
    /// n05: flight length.
    escape: Option<f64>,
    /// n06: reach.
    potential: Option<f64>,
    /// n07: wells, window (ticks).
    dense: Option<(usize, u64)>,
    /// n08: at, pitch.
    musical: Option<(usize, i64)>,
    /// n09: orbit turns.
    binary: Option<f64>,
    /// n10: the force profile.
    profile: Option<Curve>,
    /// n11: reach.
    dilation: Option<f64>,
}

pub(super) fn grav9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    // A function whose setting does nothing is skipped — unless a Control
    // Sweep drives that setting (its value then varies per note).
    let swept = |k: &str| b.ctx.sweeps.as_ref().is_some_and(|s| s.drives(k));
    let live = |id: &str, key: &str, neutral: bool| p.flag(id) && (!neutral || swept(key));
    // The v0.8 wells, signs and path (`gravity_build`, unchanged).
    let mut pts = vec![(p.float("x") / 100.0, p.float("y") / 100.0)];
    if p.adv(1) {
        pts.extend(
            permille(&p.list("a1.wells"))
                .chunks_exact(2)
                .map(|c| (c[0], c[1])),
        );
    }
    let signs: Vec<f64> = if p.adv(2) {
        p.list("a2.signs")
            .iter()
            .map(|&s| if s < 0 { -1.0 } else { 1.0 })
            .collect()
    } else {
        Vec::new()
    };
    let wells: Vec<Well> = pts
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| {
            let s = if signs.is_empty() {
                1.0
            } else {
                signs[i % signs.len()]
            };
            (x, y, s)
        })
        .collect();
    let path = p.adv(10).then(|| {
        (
            p.float("a10.x") / 100.0 - pts[0].0,
            p.float("a10.y") / 100.0 - pts[0].1,
        )
    });
    let base = GVals {
        strength: p.float("strength") / 100.0,
        radius: (p.float("radius") / 100.0).max(1e-3),
        einstein: p.float("n02.radius") / 100.0,
        tidal: p.float("n03.tidal") / 100.0,
        turns: p.float("n04.turns"),
        escape: p.int("n05.vel"),
        gain: p.float("n06.gain") / 100.0,
        sep: p.float("n09.sep") / 100.0,
        reach: (p.float("n10.reach") / 100.0).max(1e-3),
        dilation: p.float("n11.dilation") / 100.0,
    };
    let mut gf = g;
    gf.fill = Some(Fill::default());
    let k = Grav9 {
        wells,
        signs,
        exp: if p.adv(3) { p.float("a3.exp") } else { 2.0 },
        capture: p.adv(4).then(|| {
            (
                (p.float("a4.radius") / 100.0).max(1e-3),
                p.float("a4.angle") * PI / 180.0,
            )
        }),
        horizon: p.adv(5).then(|| p.float("a5.radius") / 100.0),
        pitch_only: p.adv(6),
        time_only: p.adv(7),
        mass: if p.adv(8) {
            p.list("a8.mass")
                .iter()
                .map(|&m| m as f64 / 100.0)
                .collect()
        } else {
            Vec::new()
        },
        path,
        density: p.adv(9),
        seed: p.seed("seed"),
        nbody: live("n01", "n01.coupling", p.float("n01.coupling") == 0.0).then(|| {
            (
                p.int("n01.steps").clamp(1, 64) as usize,
                p.float("n01.coupling") / 100.0,
                p.float("n01.soft").clamp(1.0, 100.0) / 100.0,
            )
        }),
        lens: p
            .flag("n02")
            .then(|| p.float("n02.ring").clamp(0.0, 200.0) / 100.0),
        tidal: live(
            "n03",
            "n03.tidal",
            base.tidal == 0.0 && p.float("n03.length") == 0.0,
        )
        .then(|| p.float("n03.length").clamp(0.0, 400.0) / 100.0),
        accretion: live(
            "n04",
            "n04.turns",
            base.turns == 0.0 && p.float("n04.infall") == 0.0,
        )
        .then(|| p.float("n04.infall").clamp(0.0, 100.0) / 100.0),
        escape: live("n05", "n05.vel", base.escape >= 127)
            .then(|| p.float("n05.flight").clamp(0.0, 200.0) / 100.0),
        potential: live("n06", "n06.gain", base.gain == 0.0)
            .then(|| (p.float("n06.reach") / 100.0).max(1e-3)),
        dense: p.flag("n07").then(|| {
            (
                p.int("n07.wells").clamp(1, 8) as usize,
                p.ticks("n07.window").max(1),
            )
        }),
        musical: p
            .flag("n08")
            .then(|| (p.choice("n08.at").min(2), p.int("n08.pitch"))),
        binary: p.flag("n09").then(|| p.float("n09.turns").clamp(0.0, 16.0)),
        profile: p.flag("n10").then(|| p.curve("n10.curve")),
        dilation: live("n11", "n11.dilation", base.dilation == 0.0)
            .then(|| (p.float("n11.reach") / 100.0).max(1e-3)),
        base,
        ctx: b.ctx.clone(),
        g,
        gf,
        tracks: b.tracks.to_vec(),
    };
    let per = if k.density {
        gf.per_note()
    } else {
        g.per_note()
    };
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

/// Distance of (u, v) to well `w`.
#[inline(always)]
fn dist(w: &Well, u: f64, v: f64) -> f64 {
    ((u - w.0).powi(2) + (v - w.1).powi(2)).sqrt()
}

/// The softened inverse-square weight 1 / (1 + (d / r)²).
#[inline(always)]
fn soft(d: f64, r: f64) -> f64 {
    1.0 / (1.0 + (d / r).powi(2))
}

impl Grav9 {
    /// a2's sign of well `i` (1 without a2).
    fn sign(&self, i: usize) -> f64 {
        if self.signs.is_empty() {
            1.0
        } else {
            self.signs[i % self.signs.len()]
        }
    }

    /// a10's offset of the v0.8 wells at the note's time `nu`.
    #[inline(always)]
    fn path_at(&self, nu: f64) -> (f64, f64) {
        self.path.map_or((0.0, 0.0), |(dx, dy)| (dx * nu, dy * nu))
    }

    /// The wells a note at time `nu` feels (into `buf`).
    fn wells_for(&self, src9: &Source9, nu: f64, tv: &GVals, buf: &mut Vec<Well>) {
        buf.clear();
        match src9 {
            Source9::V08 => {
                let (ox, oy) = self.path_at(nu);
                buf.extend(self.wells.iter().map(|&(x, y, s)| (x + ox, y + oy, s)));
            }
            Source9::Density(w) => buf.extend_from_slice(w),
            Source9::Musical(w) => {
                let k = w.partition_point(|x| x.0 < nu);
                let (a, e) = (k.saturating_sub(NEAREST), (k + NEAREST).min(w.len()));
                buf.extend_from_slice(&w[a..e]);
            }
            Source9::Binary(turns) => {
                let (ox, oy) = self.path_at(nu);
                let (cx, cy) = (self.wells[0].0 + ox, self.wells[0].1 + oy);
                let (s, c) = (2.0 * PI * turns * nu).sin_cos();
                let h = tv.sep / 2.0;
                buf.push((cx + h * c, cy + h * s, self.sign(0)));
                buf.push((cx - h * c, cy - h * s, self.sign(1)));
            }
        }
    }

    /// The v0.8 pull (`Gravity::map`, the same arithmetic) by `wells` with
    /// the note's strength and radius; n10's profile replaces the falloff.
    #[inline(always)]
    fn pull(&self, n: &TNote, u: f64, v: f64, wells: &[Well], tv: &GVals) -> (f64, f64) {
        let m = if self.mass.is_empty() {
            1.0
        } else {
            self.mass[sub_pos(&self.tracks, n) % self.mass.len()]
        };
        let (mut du, mut dv) = (0.0, 0.0);
        for &(wx, wy, sign) in wells {
            let pull = |dist: f64| -> f64 {
                let f = match &self.profile {
                    None => 1.0 / (1.0 + (dist / tv.radius).powf(self.exp)),
                    Some(c) => 2.0 * c.eval((dist / tv.reach).min(1.0)) - 1.0,
                };
                let k = tv.strength * sign * m * f;
                k.clamp(-3.0, 1.0)
            };
            if let Some((cr, ang)) = self.capture {
                let d = ((u - wx).powi(2) + (v - wy).powi(2)).sqrt();
                if d < cr {
                    let (x, y) = rot(u - wx, v - wy, ang * (1.0 - d / cr) * sign);
                    du += wx + x - u;
                    dv += wy + y - v;
                    continue;
                }
            }
            if !self.pitch_only && !self.time_only {
                let (dx, dy) = (wx - u, wy - v);
                let k = pull((dx * dx + dy * dy).sqrt());
                du += dx * k;
                dv += dy * k;
            }
            if self.pitch_only {
                dv += (wy - v) * pull((wy - v).abs());
            }
            if self.time_only {
                du += (wx - u) * pull((wx - u).abs());
            }
        }
        (u + du, v + dv)
    }

    /// n02: (u, v) seen through each well's lens — at distance d it appears
    /// at (d + √(d² + 4 θE²)) / 2 (the outer image of a point lens), so the
    /// art near a well opens into a ring of radius θE.
    #[inline(always)]
    fn lensed(&self, wells: &[Well], tv: &GVals, mut u: f64, mut v: f64) -> (f64, f64) {
        let e2 = 4.0 * tv.einstein * tv.einstein;
        for w in wells {
            let (dx, dy) = (u - w.0, v - w.1);
            let d = (dx * dx + dy * dy).sqrt();
            let d2 = (d + (d * d + e2).sqrt()) / 2.0;
            if d > 1e-12 {
                u = w.0 + dx * d2 / d;
                v = w.1 + dy * d2 / d;
            } else {
                u = w.0 + d2;
                v = w.1;
            }
        }
        (u, v)
    }

    /// The wells' potential at (u, v): Σ sign / (1 + (d / reach)²).
    fn potential_at(&self, wells: &[Well], reach: f64, u: f64, v: f64) -> f64 {
        wells.iter().map(|w| w.2 * soft(dist(w, u, v), reach)).sum()
    }

    /// Every note's swept values (the settings', with the Control Sweeps
    /// applied at each note's place).
    fn vals(&self, src: &Source) -> Vec<GVals> {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return Vec::new();
        };
        let b = self.base;
        let places = super::super::sweep::note_places(src);
        src.notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let mut d = b;
                if sw.drives("strength") {
                    d.strength = sw.float("strength", b.strength * 100.0, n, pl) / 100.0;
                }
                if sw.drives("radius") {
                    d.radius = (sw.float("radius", b.radius * 100.0, n, pl) / 100.0).max(1e-3);
                }
                d.einstein = pct("n02.radius", b.einstein);
                d.tidal = pct("n03.tidal", b.tidal).clamp(0.0, 1.0);
                if sw.drives("n04.turns") {
                    d.turns = sw.float("n04.turns", b.turns, n, pl);
                }
                if sw.drives("n05.vel") {
                    d.escape = sw.int("n05.vel", b.escape, n, pl);
                }
                d.gain = pct("n06.gain", b.gain);
                d.sep = pct("n09.sep", b.sep);
                if sw.drives("n10.reach") {
                    d.reach = (sw.float("n10.reach", b.reach * 100.0, n, pl) / 100.0).max(1e-3);
                }
                d.dilation = pct("n11.dilation", b.dilation).clamp(0.0, 1.0);
                d
            })
            .collect()
    }

    /// n01: each note's move after `steps` steps of mutual attraction — the
    /// notes' masses binned on a 32 × 32 grid of the box, the softened
    /// inverse-square force summed between cells, read at each note by
    /// bilinear interpolation; a step moves a note by coupling · s² · F
    /// (at most the softening s).
    fn nbody_shifts(&self, src: &Source) -> Vec<(f64, f64)> {
        let Some((steps, coupling, s)) = self.nbody else {
            return Vec::new();
        };
        let notes = &src.notes;
        if notes.is_empty() {
            return Vec::new();
        }
        const G: usize = NBODY_GRID;
        let gf = G as f64;
        let s2 = s * s;
        let start: Vec<(f64, f64)> = notes.iter().map(|n| mid_unit(&self.g, n)).collect();
        let mut pos = start.clone();
        let cell = |x: f64| ((x.clamp(0.0, 1.0) * gf) as usize).min(G - 1);
        let w = 1.0 / notes.len() as f64;
        let mut mass = vec![0.0f64; G * G];
        let mut field = vec![(0.0f64, 0.0f64); G * G];
        for _ in 0..steps {
            mass.iter_mut().for_each(|m| *m = 0.0);
            for &(u, v) in &pos {
                mass[cell(v) * G + cell(u)] += w;
            }
            let occ: Vec<(f64, f64, f64)> = (0..G * G)
                .filter(|&k| mass[k] > 0.0)
                .map(|k| {
                    (
                        ((k % G) as f64 + 0.5) / gf,
                        ((k / G) as f64 + 0.5) / gf,
                        mass[k],
                    )
                })
                .collect();
            for (k, f) in field.iter_mut().enumerate() {
                let (cx, cy) = (((k % G) as f64 + 0.5) / gf, ((k / G) as f64 + 0.5) / gf);
                let (mut fx, mut fy) = (0.0, 0.0);
                for &(ox, oy, m) in &occ {
                    let (dx, dy) = (ox - cx, oy - cy);
                    let r2 = dx * dx + dy * dy + s2;
                    let inv = m / (r2 * r2.sqrt());
                    fx += dx * inv;
                    fy += dy * inv;
                }
                *f = (fx, fy);
            }
            for p in pos.iter_mut() {
                let gx = (p.0 * gf - 0.5).clamp(0.0, gf - 1.0);
                let gy = (p.1 * gf - 0.5).clamp(0.0, gf - 1.0);
                let (i0, j0) = (gx.floor() as usize, gy.floor() as usize);
                let (i1, j1) = ((i0 + 1).min(G - 1), (j0 + 1).min(G - 1));
                let (tx, ty) = (gx - i0 as f64, gy - j0 as f64);
                let at = |i: usize, j: usize| field[j * G + i];
                let lerp = |a: (f64, f64), b: (f64, f64), t: f64| {
                    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
                };
                let f0 = lerp(at(i0, j0), at(i1, j0), tx);
                let f1 = lerp(at(i0, j1), at(i1, j1), tx);
                let (fx, fy) = lerp(f0, f1, ty);
                let (mut dx, mut dy) = (coupling * s2 * fx, coupling * s2 * fy);
                let len = (dx * dx + dy * dy).sqrt();
                if len > s {
                    dx *= s / len;
                    dy *= s / len;
                }
                p.0 += dx;
                p.1 += dy;
            }
        }
        pos.iter()
            .zip(&start)
            .map(|(p, q)| (p.0 - q.0, p.1 - q.1))
            .collect()
    }

    /// n07: up to `count` wells at the densest windows of `window` ticks
    /// (most starts; ties to the earliest; windows never overlap), each at
    /// the window's middle and its notes' mean pitch.
    fn density_wells(&self, src: &Source, count: usize, window: u64) -> Vec<Well> {
        let notes = &src.notes;
        let mut idx: Vec<usize> = (0..notes.len()).collect();
        idx.sort_by_key(|&i| (notes[i].start, i));
        let starts: Vec<u64> = idx.iter().map(|&i| notes[i].start).collect();
        let mut pre = vec![0u64; idx.len() + 1];
        for (k, &i) in idx.iter().enumerate() {
            pre[k + 1] = pre[k] + u64::from(notes[i].pitch);
        }
        // counts[k]: starts in [starts[k], starts[k] + window).
        let mut counts = vec![0usize; starts.len()];
        let mut e = 0;
        for k in 0..starts.len() {
            let end = starts[k].saturating_add(window);
            e = e.max(k);
            while e < starts.len() && starts[e] < end {
                e += 1;
            }
            counts[k] = e - k;
        }
        let b = &self.g.bbox;
        let mut chosen: Vec<(u64, u64)> = Vec::new();
        let mut out = Vec::new();
        for _ in 0..count {
            let mut best: Option<usize> = None;
            for k in 0..starts.len() {
                let (a, z) = (starts[k], starts[k].saturating_add(window));
                if chosen.iter().any(|&(ca, cz)| a < cz && ca < z) {
                    continue;
                }
                if best.is_none_or(|bk| counts[k] > counts[bk]) {
                    best = Some(k);
                }
            }
            let Some(k) = best else { break };
            let (a, z) = (starts[k], starts[k].saturating_add(window));
            chosen.push((a, z));
            let c = counts[k].max(1);
            let mean = (pre[k + c] - pre[k]) as f64 / c as f64;
            let (u, v) = b.to_unit(a as f64 + window as f64 / 2.0, mean + 0.5);
            out.push((u, v, self.sign(out.len())));
        }
        out
    }

    /// n08: wells in time order at every bar line, marker or chord root of
    /// the selection's span (bars and markers at the Pitch control, a chord
    /// root at its own pitch).
    fn musical_wells(&self, src: &Source, at: usize, pitch: i64) -> Vec<Well> {
        let b = &self.g.bbox;
        let (t0, t1) = (b.t0, b.t1);
        let pv = b.to_unit(t0 as f64, pitch as f64 + 0.5).1;
        let mut pts: Vec<(f64, f64)> = Vec::new();
        match at {
            0 => {
                let mut t = bar_bounds(&self.ctx.meter, t0).0;
                while t <= t1 && pts.len() < MAX_MUSICAL {
                    pts.push((b.to_unit(t as f64, 0.0).0, pv));
                    let (_, len) = bar_bounds(&self.ctx.meter, t);
                    t = t.saturating_add(len.max(1));
                    if t == u64::MAX {
                        break;
                    }
                }
            }
            1 => {
                for &m in self.ctx.markers.iter() {
                    if (t0..=t1).contains(&m) && pts.len() < MAX_MUSICAL {
                        pts.push((b.to_unit(m as f64, 0.0).0, pv));
                    }
                }
            }
            _ => {
                let notes = &src.notes;
                let tol = (self.ctx.ppq / 32).max(1);
                for idx in per_track(src) {
                    for c in ChordGroups::of(notes, &idx, tol).iter() {
                        let onset = notes[c[0]].start;
                        let root = c.iter().map(|&i| notes[i].pitch).min().unwrap_or(60);
                        if pts.len() < MAX_MUSICAL {
                            pts.push(b.to_unit(onset as f64, f64::from(root) + 0.5));
                        }
                    }
                }
            }
        }
        pts.sort_by(|a, z| a.partial_cmp(z).unwrap_or(std::cmp::Ordering::Equal));
        pts.dedup();
        pts.iter()
            .enumerate()
            .map(|(i, &(u, v))| (u, v, self.sign(i)))
            .collect()
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let b = &self.g.bbox;
        let w = b.width();
        let per = self.vals(src);
        let shifts = self.nbody_shifts(src);
        // The latest stage that places wells wins.
        let src9 = if let Some(turns) = self.binary {
            Source9::Binary(turns)
        } else if let Some((at, pitch)) = self.musical {
            Source9::Musical(self.musical_wells(src, at, pitch))
        } else if let Some((count, window)) = self.dense {
            Source9::Density(self.density_wells(src, count, window))
        } else {
            Source9::V08
        };
        let mut wells: Vec<Well> = Vec::new();
        for (i, n) in src.notes.iter().enumerate() {
            let tv = per.get(i).copied().unwrap_or(self.base);
            let (nu, nv) = mid_unit(&self.g, n);
            self.wells_for(&src9, nu, &tv, &mut wells);
            if self.horizon.is_some_and(|h| {
                wells
                    .iter()
                    .any(|&(x, y, _)| ((nu - x).powi(2) + (nv - y).powi(2)).sqrt() < h)
            }) {
                continue;
            }
            let wl: &[Well] = &wells;
            let sh = shifts.get(i).copied().unwrap_or((0.0, 0.0));
            // The note's middle after n01 / n02: its nearest well and the
            // velocity of n02's ring.
            let (mut mu, mut mv) = (nu + sh.0, nv + sh.1);
            if self.lens.is_some() {
                (mu, mv) = self.lensed(wl, &tv, mu, mv);
            }
            let near: Option<Well> = wl.iter().copied().min_by(|a, z| {
                dist(a, mu, mv)
                    .partial_cmp(&dist(z, mu, mv))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut vel = f64::from(n.vel);
            if let (Some(ring), Some(nw)) = (self.lens, near)
                && tv.einstein > 0.0
            {
                let d = dist(&nw, mu, mv).max(1e-12);
                let c = (tv.einstein / d).min(1.0).powi(2);
                vel *= 1.0 + (ring - 1.0) * c;
            }
            let escaping = self.escape.is_some() && i64::from(n.vel) > tv.escape;
            let us = (n.start as f64 - b.t0 as f64) / w;
            let ue = (n.end() as f64 - b.t0 as f64) / w;
            // n03 / n04 around the nearest well, at the note's middle.
            let tidal = match (self.tidal, near) {
                (Some(ls), Some(nw)) if !escaping => {
                    let (dx, dy) = (mu - nw.0, mv - nw.1);
                    let d = (dx * dx + dy * dy).sqrt();
                    let f = soft(d, tv.radius);
                    let s = 1.0 + tv.tidal * f;
                    let e = if d > 1e-12 {
                        (dx / d, dy / d)
                    } else {
                        (1.0, 0.0)
                    };
                    Some((nw, e, s, ls * tv.tidal * f * (ue - us)))
                }
                _ => None,
            };
            let spiral = match (self.accretion, near) {
                (Some(infall), Some(nw)) if !escaping => {
                    let d = dist(&nw, mu, mv);
                    let f = soft(d, tv.radius);
                    let t = nu.clamp(0.0, 1.0);
                    let (s, c) = (2.0 * PI * tv.turns * t * f).sin_cos();
                    Some((nw, s, c, (1.0 - infall * t * f).max(0.0)))
                }
                _ => None,
            };
            let flight = match (self.escape, near) {
                (Some(len), Some(nw)) if escaping => {
                    let (dx, dy) = (mu - nw.0, mv - nw.1);
                    let d = (dx * dx + dy * dy).sqrt();
                    let e = if d > 1e-12 {
                        (dx / d, dy / d)
                    } else {
                        (1.0, 0.0)
                    };
                    Some((e.0 * len, e.1 * len))
                }
                (Some(_), None) if escaping => Some((0.0, 0.0)),
                _ => None,
            };
            let dil = match (self.dilation, near) {
                (Some(reach), Some(nw)) => Some((nw, reach)),
                _ => None,
            };
            let map = |u0: f64, v0: f64| -> (f64, f64) {
                let (mut u, mut v) = (u0, v0);
                if self.nbody.is_some() {
                    u += sh.0;
                    v += sh.1;
                }
                if self.lens.is_some() {
                    (u, v) = self.lensed(wl, &tv, u, v);
                }
                if let Some((fu, fv)) = flight {
                    (u, v) = (u + fu, v + fv);
                } else {
                    if let Some((nw, e, s, dl)) = tidal {
                        let (rx, ry) = (u - nw.0, v - nw.1);
                        let along = rx * e.0 + ry * e.1;
                        let (px, py) = (rx - along * e.0, ry - along * e.1);
                        u = nw.0 + along * s * e.0 + px / s;
                        v = nw.1 + along * s * e.1 + py / s;
                        if dl != 0.0 {
                            let span = ue - us;
                            let lam = if span > 0.0 { (u0 - us) / span } else { 0.0 };
                            u += lam * dl;
                        }
                    }
                    if let Some((nw, s, c, k)) = spiral {
                        let (rx, ry) = (u - nw.0, v - nw.1);
                        u = nw.0 + (rx * c - ry * s) * k;
                        v = nw.1 + (rx * s + ry * c) * k;
                    }
                    (u, v) = self.pull(n, u, v, wl, &tv);
                }
                if let Some((nw, reach)) = dil {
                    let g = soft(dist(&nw, u, v), reach);
                    u -= (u - nw.0) * tv.dilation * g;
                }
                (u, v)
            };
            if let Some(reach) = self.potential {
                let (fu, fv) = map(nu, nv);
                let phi = self.potential_at(wl, reach, fu, fv);
                vel *= (1.0 + tv.gain * phi).max(0.0);
            }
            let mut geo = &self.g;
            if self.density {
                let j = area_scale(&map, nu, nv);
                if j < 1.0 {
                    if !Rng::for_note(self.seed, salt::KEEP, n).chance(j) {
                        continue;
                    }
                } else if j > 1.05 {
                    geo = &self.gf;
                }
            }
            let ok = if vel != f64::from(n.vel) {
                let mut t = *n;
                t.vel = vel.round().clamp(1.0, 127.0) as u8;
                geo.rasterize(&t, &map, &self.tracks, out)
            } else {
                geo.rasterize(n, &map, &self.tracks, out)
            };
            if !ok {
                return out.check();
            }
            out.check()?;
        }
        Ok(())
    }
}
