//! Vortex Trails' v0.9 kernel (V09-S40 step 6, `docs/TRANSFORM_ENGINE.md`
//! "Vortex Trails"): the functions declared by id beside the v0.8 kernel
//! (`xgeom.rs` `vortex_build`, untouched — a module of its own so the v0.8
//! closure compiles as before), with Control Sweeps per note.
//!
//! Every trail is v0.8's twirl of the selection (a1's count, a3's turns,
//! a4 / a9's radius, a8's reverse trails, a10's second vortex — or n04's
//! two vortices that approach and merge into one of a final strength),
//! tilted in depth about the vortex's row (n06), receding toward its centre
//! (n08), delayed (n01) and transposed (n02) trail by trail, mirrored in
//! time to precede the motif (n09) and placed on a rhythm grid (n05); each
//! trail plays a moving window of the motif (n07), every Nth trail is kept
//! (n10) and the motif morphs toward its inversion or retrograde along the
//! trails (n11); n03 fills the vortex's core with a cluster.

use super::super::geom::Geom;
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::rng::{Rng, salt};
use super::super::shared::metric_position::bar_bounds;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::note_value;
use super::xgeom::{rot, sub_stepped, vel_scaled};
use std::f64::consts::PI;

/// The switches of the v0.9 functions.
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];

pub(super) const SIDES: &[&str] = &["After", "Before", "Both"];
pub(super) const TARGETS: &[&str] = &["Inversion", "Retrograde", "Both"];

/// The farthest a mapped point is placed (unit coordinates of the box): the
/// rasterizer's row arithmetic stays in i64.
const FAR: f64 = 1e12;

/// One copy: the trail (0 = the original), its turning direction (a8) and
/// side (n09: false = after, true = before).
#[derive(Clone, Copy, Debug)]
struct Trail {
    j: usize,
    dir: f64,
    before: bool,
}

/// One note's settings, per Control Sweep level.
#[derive(Clone, Debug)]
struct VCfg {
    // v0.8 (`vortex_build`).
    c1: (f64, f64),
    rx: f64,
    ry: f64,
    trails: usize,
    spacing: f64,
    keep: Option<f64>,
    curve_spacing: Option<(Curve, f64)>,
    dr: f64,
    gd: f64,
    vd: f64,
    sub_step: i64,
    logk: Option<f64>,
    c2: Option<(f64, f64)>,
    seed: u64,
    copies: Vec<Trail>,
    // v0.9.
    /// n01: ticks per trail.
    delay: f64,
    /// n02: semitones per trail.
    semis: f64,
    /// n03: core radius (fraction of the vortex radius), notes per beat.
    core: Option<(f64, f64)>,
    /// n04: merge point (fraction of the selection's time), final strength,
    /// the second vortex.
    merger: Option<(f64, f64, (f64, f64))>,
    /// n05: grid ticks, trails per grid step.
    rhythm: Option<(u64, u64)>,
    /// n06: radians per trail.
    tilt: f64,
    /// n07: window, step between trails' windows (fractions of the time).
    comet: Option<(f64, f64)>,
    /// n08: depth per trail, velocity fade.
    depth: Option<(f64, f64)>,
    /// n10: keep every, offset (< every).
    strobe: Option<(usize, usize)>,
    /// n11: target (0 inversion, 1 retrograde, 2 both), progress per trail.
    morph: Option<(usize, f64)>,
}

impl VCfg {
    fn from(p: &Params, b: &BuildCtx) -> VCfg {
        let c1 = (p.float("cx") / 100.0, p.float("cy") / 100.0);
        let (rx, ry) = (
            (p.float("rx") / 100.0).max(1e-3),
            (p.float("ry") / 100.0).max(1e-3),
        );
        let trails = if p.adv(1) {
            p.int("a1.count").clamp(1, 32) as usize
        } else {
            3
        };
        let both = p.adv(8);
        let sides = if p.flag("n09") {
            p.choice("n09.side").min(2)
        } else {
            0
        };
        // v0.8's copy order (the original, trails 1..=T, then −1..=−T when
        // reversed), each trail on n09's sides.
        let mut copies = vec![Trail {
            j: 0,
            dir: 1.0,
            before: false,
        }];
        for dir in if both { &[1.0, -1.0][..] } else { &[1.0][..] } {
            for j in 1..=trails {
                for before in match sides {
                    0 => &[false][..],
                    1 => &[true][..],
                    _ => &[false, true][..],
                } {
                    copies.push(Trail {
                        j,
                        dir: *dir,
                        before: *before,
                    });
                }
            }
        }
        VCfg {
            c1,
            rx,
            ry,
            trails,
            spacing: p.float("spacing") * PI / 180.0,
            keep: p.adv(2).then(|| p.float("a2.keep") / 100.0),
            curve_spacing: p
                .adv(3)
                .then(|| (p.curve("a3.curve"), p.float("a3.total") * PI / 180.0)),
            dr: if p.adv(4) {
                p.float("a4.step") / 100.0
            } else {
                0.0
            },
            gd: if p.adv(5) {
                p.float("a5.decay") / 100.0
            } else {
                1.0
            },
            vd: if p.adv(6) {
                p.float("a6.decay") / 100.0
            } else {
                1.0
            },
            sub_step: if p.adv(7) { p.int("a7.step") } else { 0 },
            logk: p.adv(9).then(|| p.float("a9.k") / 100.0),
            c2: p
                .adv(10)
                .then(|| (p.float("a10.x") / 100.0, p.float("a10.y") / 100.0)),
            seed: p.seed("seed"),
            copies,
            delay: if p.flag("n01") {
                p.ticks("n01.delay") as f64
            } else {
                0.0
            },
            semis: if p.flag("n02") {
                p.int("n02.semis").clamp(-24, 24) as f64
            } else {
                0.0
            },
            core: p.flag("n03").then(|| {
                (
                    (p.float("n03.radius") / 100.0).clamp(0.01, 1.0),
                    p.float("n03.density").clamp(0.1, 64.0),
                )
            }),
            merger: p.flag("n04").then(|| {
                (
                    (p.float("n04.at") / 100.0).clamp(0.0, 1.0),
                    (p.float("n04.final") / 100.0).clamp(0.0, 2.0),
                    (p.float("a10.x") / 100.0, p.float("a10.y") / 100.0),
                )
            }),
            rhythm: p.flag("n05").then(|| {
                (
                    note_value(p.choice("n05.grid"), b.ctx.snap, b.ctx).max(1),
                    p.int("n05.per_beat").clamp(1, 16) as u64,
                )
            }),
            tilt: if p.flag("n06") {
                p.float("n06.tilt").clamp(-90.0, 90.0) * PI / 180.0
            } else {
                0.0
            },
            comet: p
                .flag("n07")
                .then(|| {
                    let w = (p.float("n07.window") / 100.0).clamp(0.01, 1.0);
                    let ov = (p.float("n07.overlap") / 100.0).clamp(0.0, 1.0);
                    (w, w * (1.0 - ov))
                })
                // A window of the whole time keeps every note.
                .filter(|&(w, _)| w < 1.0),
            depth: p.flag("n08").then(|| {
                (
                    (p.float("n08.depth") / 100.0).clamp(0.0, 0.5),
                    (p.float("n08.fade") / 100.0).clamp(0.0, 1.0),
                )
            }),
            strobe: p.flag("n10").then(|| {
                let every = p.int("n10.every").clamp(1, 16) as usize;
                (every, p.int("n10.offset").clamp(0, 15) as usize % every)
            }),
            morph: p.flag("n11").then(|| {
                (
                    p.choice("n11.target").min(2),
                    (p.float("n11.progress") / 100.0).clamp(0.0, 1.0),
                )
            }),
        }
    }

    /// The most copies one note can make (the original and every trail
    /// direction / side of the trails it can play in): n10 keeps the trails
    /// of its residue; n07's windows advance by δ = W·(1 − overlap) — a
    /// point of the box lies in at most ⌈W / δ⌉ of them per turn of the
    /// cycle, and T trails make ⌈(T − 1)·δ + W⌉ turns.
    fn copy_bound(&self) -> u64 {
        let t = self.trails.max(1);
        let groups = (self.copies.len().saturating_sub(1) / t) as u64;
        let strobe = (1..=t)
            .filter(|&j| self.strobe.is_none_or(|(e, o)| (j - 1) % e == o))
            .count();
        let comet = match self.comet {
            Some((w, step)) if step > 0.0 => {
                let turns = ((t - 1) as f64 * step + w + 1e-9).ceil();
                let per = (w / step + 1e-9).ceil();
                (turns * per).min(t as f64) as usize
            }
            _ => t,
        };
        1 + groups * strobe.min(comet) as u64
    }

    /// v0.8's turn of trail `j`.
    #[inline]
    fn angle_of(&self, j: usize) -> f64 {
        match &self.curve_spacing {
            Some((c, total)) => total * c.eval(j as f64 / self.trails as f64),
            None => self.spacing * j as f64,
        }
    }

    /// Trail `j`'s vortex at unit time `u`: centre, turning sign, strength.
    /// v0.8: the even trails turn the other way around a10's second vortex.
    /// n04: both vortices approach their midpoint until the merge point,
    /// then one vortex there turns every trail at the final strength.
    #[inline]
    fn vortex(&self, j: usize, u: f64) -> ((f64, f64), f64, f64) {
        match self.merger {
            None => match self.c2 {
                Some(c) if j.is_multiple_of(2) => (c, -1.0, 1.0),
                _ => (self.c1, 1.0, 1.0),
            },
            Some((at, fin, c2)) => {
                let m = ((self.c1.0 + c2.0) / 2.0, (self.c1.1 + c2.1) / 2.0);
                let q = if at <= 0.0 {
                    1.0
                } else {
                    (u / at).clamp(0.0, 1.0)
                };
                if q >= 1.0 {
                    return (m, 1.0, fin);
                }
                let (c, sign) = if j.is_multiple_of(2) {
                    (c2, -1.0)
                } else {
                    (self.c1, 1.0)
                };
                ((c.0 + (m.0 - c.0) * q, c.1 + (m.1 - c.1) * q), sign, 1.0)
            }
        }
    }

    /// Copy `c`'s map of a unit point (`width` ticks, `rows` rows in the
    /// box): v0.8's twirl, then n08, n06, n01, n02 and n09.
    #[inline]
    fn map(&self, c: Trail, u: f64, v: f64, width: f64, rows: f64) -> (f64, f64) {
        let j = c.j;
        if j == 0 {
            return (u, v);
        }
        let (centre, sign, strength) = self.vortex(j, u);
        let (x, y) = ((u - centre.0) / self.rx, (v - centre.1) / self.ry);
        let r = (x * x + y * y).sqrt();
        let w = (1.0 - r).max(0.0);
        let mut a = self.angle_of(j) * c.dir * sign * w;
        if self.merger.is_some() {
            a *= strength;
        }
        let (mut x2, mut y2) = rot(x, y, a);
        let mut s = 1.0 + self.dr * j as f64;
        if let Some(kk) = self.logk {
            s *= (kk * a.abs()).exp();
        }
        x2 *= s;
        y2 *= s;
        if let Some((d, _)) = self.depth {
            // n08: trail j recedes to depth j·d (perspective 1 / (1 + z)).
            let f = 1.0 / (1.0 + d * j as f64);
            x2 *= f;
            y2 *= f;
        }
        if self.tilt != 0.0 {
            // n06: trail j's plane tilted by j·θ about the vortex's row.
            y2 *= (self.tilt * j as f64).cos();
        }
        let (mut u2, mut v2) = (centre.0 + x2 * self.rx, centre.1 + y2 * self.ry);
        if self.delay != 0.0 {
            u2 += self.delay * j as f64 / width;
        }
        if self.semis != 0.0 {
            v2 += self.semis * j as f64 / rows;
        }
        if c.before {
            // n09: the time mirror of the after-trail around the source.
            u2 = 2.0 * u - u2;
        }
        (u2, v2)
    }

    /// Copy `c`'s template of note `n` (None drops it): n10's strobe, n07's
    /// window (`us`: the note's start in unit time), a2's thinning, a7 / a6 /
    /// n08 / a5, then n11's morph of the motif.
    #[inline]
    fn template(
        &self,
        n: &TNote,
        c: Trail,
        us: f64,
        tracks: &[SourceTrack],
        bx: (u64, u64),
        pitches: (u8, u8),
    ) -> Option<TNote> {
        let j = c.j;
        if j > 0 {
            if let Some((every, off)) = self.strobe
                && (j - 1) % every != off
            {
                return None;
            }
            if let Some((win, step)) = self.comet {
                let s = ((j - 1) as f64 * step).rem_euclid(1.0);
                if (us - s).rem_euclid(1.0) >= win {
                    return None;
                }
            }
            if let Some(kp) = self.keep
                && !Rng::for_note(self.seed, salt::KEEP ^ j as u64, n).chance(kp.powi(j as i32))
            {
                return None;
            }
        }
        let mut t = sub_stepped(tracks, n, self.sub_step * j as i64);
        let mut vf = self.vd.powi(j as i32);
        if let Some((d, fade)) = self.depth {
            vf *= 1.0 - fade * (1.0 - 1.0 / (1.0 + d * j as f64));
        }
        t.vel = vel_scaled(n, vf);
        t.dur = ((n.dur as f64 * self.gd.powi(j as i32)).round() as u64).max(1);
        if j > 0
            && let Some((target, prog)) = self.morph
        {
            let pj = (prog * j as f64).min(1.0);
            if target != 1 {
                // Inversion between the selection's lowest and highest pitch.
                let inv = f64::from(pitches.0) + f64::from(pitches.1) - f64::from(n.pitch);
                let p = f64::from(n.pitch) + pj * (inv - f64::from(n.pitch));
                t.pitch = p.round().clamp(0.0, 255.0) as u8;
            }
            if target != 0 {
                // Retrograde in the selection's box: the note ends where it
                // started from the box's end.
                // (In wide integers: the box may lie at the far end of the
                // tick range.)
                let r = i128::from(bx.0) + i128::from(bx.1) - i128::from(n.end());
                let d = (pj * (r - i128::from(n.start)) as f64).round() as i128;
                t.start = (i128::from(n.start) + d).clamp(0, i128::from(u64::MAX)) as u64;
            }
        }
        Some(t)
    }
}

pub(super) struct Vor9 {
    base: VCfg,
    rebuild: Option<std::sync::Arc<Rebuild>>,
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    growth: Growth,
    estimate: u64,
}

/// n03's core: the cells of `dt` ticks and the rows inside the ellipse of
/// half axes `hx` ticks × `hy` rows around (`xc`, `yc`) — an upper bound of
/// their count (the estimate) or each cell (`emit`; false stops).
fn core_cells(
    (xc, yc): (f64, f64),
    (hx, hy): (f64, f64),
    dt: f64,
    top: i64,
    mut emit: Option<&mut dyn FnMut(f64, i64) -> bool>,
) -> u64 {
    if !(hx > 0.0 && hy > 0.0 && dt >= 1.0 && xc.is_finite() && yc.is_finite()) {
        return 0;
    }
    let cols = (2.0 * hx / dt).floor() + 2.0;
    let rows = (2.0 * hy).floor().min(top as f64) + 2.0;
    let bound = cols * rows;
    let Some(emit) = emit.as_mut() else {
        return if bound >= u64::MAX as f64 {
            u64::MAX
        } else {
            bound as u64
        };
    };
    // Cells on the grid k·dt whose middle lies inside the ellipse's time
    // span; each column's rows whose centre lies inside it.
    // (Counted in integers: beyond 2^53 a float cell index no longer steps
    // by one — far-end selections — and the count never exceeds the bound.)
    let k0 = ((xc - hx - dt / 2.0) / dt).ceil();
    let k1 = ((xc + hx - dt / 2.0) / dt).floor();
    let count = if k1 >= k0 {
        (k1 - k0 + 1.0).min(cols) as u64
    } else {
        0
    };
    let mut n = 0u64;
    for i in 0..count {
        let s = (k0 + i as f64) * dt;
        let ex = (s + dt / 2.0 - xc) / hx;
        if ex.abs() <= 1.0 {
            let h = hy * (1.0 - ex * ex).sqrt();
            let r0 = ((yc - h - 0.5).ceil() as i64).max(0);
            let r1 = ((yc + h - 0.5).floor() as i64).min(top);
            for r in r0..=r1 {
                n += 1;
                if !emit(s, r) {
                    return n;
                }
            }
        }
    }
    n
}

/// n03's core in absolute ticks and rows: centre, half axes, cell ticks.
type CoreGeometry = ((f64, f64), (f64, f64), f64);

impl VCfg {
    /// n03's core in absolute ticks and rows: centre, half axes, cell ticks.
    fn core_geometry(&self, g: &Geom, ppq: u64) -> Option<CoreGeometry> {
        let (cr, density) = self.core?;
        let b = &g.bbox;
        let centre = b.from_unit(self.c1.0, self.c1.1);
        let half = (cr * self.rx * b.width(), cr * self.ry * b.rows());
        let dt = (ppq as f64 / density).round().max(1.0);
        Some((centre, half, dt))
    }
}

pub(super) fn vor9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let base = VCfg::from(p, b);
    let rebuild = Rebuild::of(p, b);
    let corners = match &rebuild {
        Some(r) => r.corners(b.ctx, VCfg::from),
        None => vec![base.clone()],
    };
    // The bound per note over every swept corner: each copy rasterized; the
    // core once (the first level's, as the run takes it).
    let per = corners
        .iter()
        .map(VCfg::copy_bound)
        .max()
        .unwrap_or(1)
        .max(1)
        .saturating_mul(g.per_note());
    let core = corners
        .iter()
        .filter_map(|c| c.core_geometry(&g, b.ctx.ppq))
        .map(|(c, h, dt)| core_cells(c, h, dt, g.top, None))
        .max()
        .unwrap_or(0);
    let n = b.stats.count as u64;
    Ok(Box::new(Vor9 {
        base,
        rebuild,
        g,
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        growth: Growth::Expand(per),
        estimate: n.saturating_mul(per).saturating_add(core),
    }))
}

impl Vor9 {
    fn levels(&self, src: &Source) -> PerLevel<VCfg> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, VCfg::from),
            None => PerLevel::one(self.base.clone()),
        }
    }

    /// n05: the shift (unit time) that puts trail note `t` (copy `c`) on
    /// its slot of the grid: the trails of one grid step sound one after
    /// another, (j − 1) mod N slots of step / N into it (cells restart at
    /// every bar line).
    fn rhythm_shift(&self, cfg: &VCfg, c: Trail, t: &TNote) -> f64 {
        let Some((grid, per)) = cfg.rhythm else {
            return 0.0;
        };
        let b = &self.g.bbox;
        let (width, rows) = (b.width(), b.rows());
        let y = f64::from(t.pitch) + 0.5;
        let at = |x: f64| {
            let (u, v) = b.to_unit(x, y);
            let (u, v) = cfg.map(c, u, v, width, rows);
            b.from_unit(u, v).0
        };
        let xs = at(t.start as f64).min(at(t.end() as f64));
        if !xs.is_finite() {
            return 0.0;
        }
        let tick = xs.round().clamp(0.0, 1e18) as u64;
        let (bar_start, _) = bar_bounds(&self.ctx.meter, tick);
        let cell = (tick - bar_start.min(tick)) / grid;
        let slot = ((c.j - 1) as u64 % per) as f64 * grid as f64 / per as f64;
        let target = bar_start as f64 + (cell * grid) as f64 + slot.round();
        (target - xs) / width
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let lv = self.levels(src);
        let notes = &src.notes;
        let g = &self.g;
        let b = &g.bbox;
        let (width, rows) = (b.width(), b.rows());
        let bx = (b.t0, b.t1);
        let pitches = (src.stats.min_pitch, src.stats.max_pitch);
        for (i, n) in notes.iter().enumerate() {
            let cfg = lv.at(i);
            let us = (n.start as f64 - b.t0 as f64) / width;
            for &c in cfg.copies.iter() {
                let Some(t) = cfg.template(n, c, us, &self.tracks, bx, pitches) else {
                    continue;
                };
                // V09-0143: the trail (0 = the original).
                out.set_dim(u32::try_from(c.j).unwrap_or(u32::MAX));
                let du = if c.j > 0 {
                    self.rhythm_shift(cfg, c, &t)
                } else {
                    0.0
                };
                let ok = g.rasterize(
                    &t,
                    &|u, v| {
                        let (mut u2, v2) = cfg.map(c, u, v, width, rows);
                        if du != 0.0 {
                            u2 += du;
                        }
                        (u2.clamp(-FAR, FAR), v2.clamp(-FAR, FAR))
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
        // n03: the core, once, from the first note's place (track,
        // subchannel, channel, velocity); the shared affine part applies.
        if let (Some(first), Some((centre, half, dt))) =
            (notes.first(), lv.first().core_geometry(g, self.ctx.ppq))
        {
            out.set_dim(0);
            let mut full = false;
            let mut emit = |s: f64, r: i64| -> bool {
                let y = r as f64 + 0.5;
                let (a, row) = g.affine(s, y);
                let (e, _) = g.affine(s + dt, y);
                if !(a.is_finite() && e.is_finite() && row.is_finite()) {
                    return true;
                }
                let (a, e) = (a.min(e).round() as i128, a.max(e).round() as i128);
                if out.emit(
                    a,
                    (e - a).max(1),
                    row.floor() as i64,
                    i64::from(first.vel),
                    first,
                ) {
                    true
                } else {
                    full = true;
                    false
                }
            };
            core_cells(centre, half, dt, g.top, Some(&mut emit));
            if full {
                return out.check();
            }
        }
        Ok(())
    }
}

impl Kernel for Vor9 {
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
