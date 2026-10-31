//! Particle Burst's v0.9 kernel (V09-S41, `docs/TRANSFORM_ENGINE.md`
//! "Particle Burst"): the functions declared by id beside the v0.8 kernel
//! (`xfield.rs` `particle_build`, untouched — it hands over when a new switch
//! or a Control Sweep is on), with Control Sweeps per note.
//!
//! v0.8's rule: every note emits k fragments (a03: k from its velocity) at
//! angles across a cone (a01 all around, a02 a direction and width; a08 a
//! seeded jitter of angle and speed), each landing `Distance × speed` away
//! in unit coordinates of the selection's box (a07 drag shortens it, a06
//! gravity drops it), a fragment of the fragment length there (a04 / a05
//! velocity and length decay with the distance, a09 subchannel cycling, a10
//! secondary bursts from each landing). The v0.9 functions:
//! n01 lets only the notes on chosen grid positions emit, n02 emits over a
//! duration at a rate (fountains), n10 places the particles on a ring, star,
//! heart or spiral, n09 reverses the burst (particles converge on the note),
//! n06 adds a wind and n11 curl noise to the flight, n05 pulls the landings
//! toward the notes of a target, n03 bounces them off a floor key, n04
//! leaves trails behind them, n07 colours them by age, speed or direction,
//! n08 lands them on a key's scale.

use super::super::geom::{GeomBox, rem1};
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::rng::{Rng, salt};
use super::super::shared::metric_position::metric_position;
use super::super::shared::reference::reference_notes;
use super::super::shared::scale_lattice::Lattice;
use super::super::shared::seeded_stream::salt_of;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::Scale;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::geometry::sub_at;
use super::note_value;
use super::xgeom::{noise2, sub_stepped, vel_scaled};
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

/// The switches of the v0.9 functions.
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
pub(super) const TARGETS: &[&str] = &["A track", "A subchannel"];
pub(super) const COLOUR_BY: &[&str] = &["Age", "Speed", "Direction"];
pub(super) const ARRIVAL: &[&str] = &["At the note start", "Before it"];
pub(super) const SHAPES: &[&str] = &["Ring", "Star", "Heart", "Spiral"];

/// The most emissions of n02 a note may make (beyond, the bound refuses).
const MAX_EMISSIONS: u64 = 1 << 20;

#[derive(Clone, Debug)]
struct QCfg {
    // v0.8 (`particle_build`).
    dist: f64,
    frag: f64,
    keep: bool,
    seed: u64,
    dir: f64,
    width: f64,
    full: bool,
    count_max: usize,
    by_vel: bool,
    vdec: Option<f64>,
    gdec: Option<f64>,
    grav: Option<f64>,
    drag: Option<f64>,
    jitter: Option<f64>,
    cycle: bool,
    second: Option<usize>,
    // v0.9.
    /// n01: grid (ticks), emit (≠ 0) per grid position of the bar.
    metric: Option<(u64, Arc<Vec<i64>>)>,
    /// n02: emissions over the duration: (count, ticks apart).
    emission: Option<(u64, f64)>,
    /// n03: the floor key, restitution, bounces.
    bounce: Option<(u8, f64, u32)>,
    /// n04: trail length (ticks), notes per trail.
    trail: Option<(f64, u32)>,
    /// n05: strength (the targets are layer 0 of `Source::aux`).
    attract: Option<f64>,
    /// n06: wind in time and in pitch (unit of the box per flight).
    wind: (f64, f64),
    /// n07: palette (subchannel positions), 0 age, 1 speed, 2 direction.
    colours: Option<(Arc<Vec<i64>>, usize)>,
    /// n08: the key's lattice.
    lattice: Option<Lattice>,
    /// n09: ring radius (unit), arrive before the note.
    implode: Option<(f64, bool)>,
    /// n10: 0 ring, 1 star, 2 heart, 3 spiral; size (unit).
    shape: Option<(usize, f64)>,
    /// n11: amount, scale (unit).
    turb: Option<(f64, f64)>,
}

impl QCfg {
    fn from(p: &Params, b: &BuildCtx) -> QCfg {
        let (dir, width) = if p.adv(1) {
            (0.0, 2.0 * PI)
        } else if p.adv(2) {
            (
                p.float("a2.dir") * PI / 180.0,
                p.float("a2.width") * PI / 180.0,
            )
        } else {
            (0.0, PI / 3.0)
        };
        let pc = |k: &str| p.float(k) / 100.0;
        QCfg {
            dist: p.float("dist") / 100.0,
            frag: p.ticks("frag").max(1) as f64,
            keep: p.flag("keep"),
            seed: p.seed("seed"),
            dir,
            width,
            full: p.adv(1),
            count_max: if p.adv(3) {
                p.int("a3.max").clamp(1, 256) as usize
            } else {
                6
            },
            by_vel: p.adv(3),
            vdec: p.adv(4).then(|| p.float("a4.decay") / 100.0),
            gdec: p.adv(5).then(|| p.float("a5.decay") / 100.0),
            grav: p.adv(6).then(|| p.float("a6.g") / 100.0),
            drag: p.adv(7).then(|| p.float("a7.k")),
            jitter: p.adv(8).then(|| p.float("a8.angle") * PI / 180.0),
            cycle: p.adv(9),
            second: p.adv(10).then(|| p.int("a10.count").clamp(1, 16) as usize),
            metric: p.flag("n01").then(|| {
                (
                    note_value(p.choice("n01.grid"), p.ticks("n01.ticks"), b.ctx).max(1),
                    Arc::new(p.list("n01.pattern")),
                )
            }),
            emission: p.flag("n02").then(|| {
                let dur = p.ticks("n02.duration") as f64;
                let every = b.ctx.ppq.max(1) as f64 / p.float("n02.rate").clamp(0.25, 256.0);
                let count = (dur / every).floor().min(MAX_EMISSIONS as f64) as u64 + 1;
                (count, every)
            }),
            bounce: p.flag("n03").then(|| {
                (
                    p.int("n03.floor").clamp(0, 255) as u8,
                    pc("n03.restitution").clamp(0.0, 1.0),
                    p.int("n03.bounces").clamp(0, 8) as u32,
                )
            }),
            trail: p
                .flag("n04")
                .then(|| {
                    (
                        p.ticks("n04.length") as f64,
                        p.int("n04.density").clamp(1, 64) as u32,
                    )
                })
                .filter(|t| t.0 > 0.0),
            attract: p.flag("n05").then(|| pc("n05.strength").clamp(0.0, 1.0)),
            wind: if p.flag("n06") {
                (pc("n06.time"), pc("n06.pitch"))
            } else {
                (0.0, 0.0)
            },
            colours: p
                .flag("n07")
                .then(|| (Arc::new(p.list("n07.palette")), p.choice("n07.by").min(2))),
            lattice: p.flag("n08").then(|| {
                Lattice::of_scale(&Scale::new(p.choice("n08.key"), p.choice("n08.scale")))
            }),
            implode: p.flag("n09").then(|| {
                (
                    pc("n09.radius").clamp(0.01, 1.0),
                    p.choice("n09.arrival") == 1,
                )
            }),
            shape: p.flag("n10").then(|| {
                (
                    p.choice("n10.shape").min(3),
                    pc("n10.size").clamp(0.01, 1.0),
                )
            }),
            turb: p.flag("n11").then(|| {
                (
                    pc("n11.amount").clamp(0.0, 1.0),
                    pc("n11.scale").clamp(0.01, 1.0),
                )
            }),
        }
    }

    /// Notes one source note can make: the anchor and every fragment (each
    /// emission's particles, their secondaries, bounces and trails).
    fn per_note(&self) -> u64 {
        let emissions = self.emission.map_or(1, |e| e.0);
        let each = 1
            + self.bounce.map_or(0, |b| u64::from(b.2))
            + self.trail.map_or(0, |t| u64::from(t.1));
        1u64.saturating_add(
            (self.count_max as u64)
                .saturating_mul(1 + self.second.unwrap_or(0) as u64)
                .saturating_mul(emissions)
                .saturating_mul(each),
        )
    }
}

/// n10: the point of the shape at `frac` (unit radius about the emitter).
fn shape_point(shape: usize, frac: f64) -> (f64, f64) {
    let t = TAU * frac;
    match shape {
        // A five-pointed star: the radius between 0.4 and 1.
        1 => {
            let r = 0.4 + 0.6 * (2.5 * t).cos().abs();
            (r * t.cos(), r * t.sin())
        }
        // The classic heart curve, normalised to about one.
        2 => {
            let x = 16.0 * t.sin().powi(3);
            let y =
                13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos();
            (x / 17.0, y / 17.0)
        }
        // Two turns of an Archimedean spiral.
        3 => {
            let a = 2.0 * t;
            (frac * a.cos(), frac * a.sin())
        }
        _ => (t.cos(), t.sin()),
    }
}

/// One particle in flight: its landing point, the flight (1 = Distance),
/// the angle and speed.
#[derive(Clone, Copy, Debug)]
struct Fly {
    x: f64,
    y: f64,
    t: f64,
    a: f64,
    speed: f64,
}

pub(super) struct Part9 {
    base: QCfg,
    rebuild: Option<Arc<Rebuild>>,
    bx: GeomBox,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    growth: Growth,
}

pub(super) fn part9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let base = QCfg::from(p, b);
    let rebuild = Rebuild::of(p, b);
    let corners = match &rebuild {
        Some(r) => r.corners(b.ctx, QCfg::from),
        None => vec![base.clone()],
    };
    let per = corners.iter().map(QCfg::per_note).max().unwrap_or(1);
    Ok(Box::new(Part9 {
        base,
        rebuild,
        bx: GeomBox::of(b.stats),
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        growth: Growth::Expand(per),
    }))
}

/// n05: reads the target notes (layer 0 of `Source::aux`) over the whole
/// track or subchannel.
pub fn prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
) -> Result<(), TransformError> {
    if !p.flag("n05") || src.stats.is_empty() {
        return Ok(());
    }
    let notes = reference_notes(
        doc,
        src,
        p.choice("n05.target") == 1,
        p.int("n05.num"),
        0,
        u64::MAX,
        limit,
        "Target attraction",
    )?;
    src.aux = Arc::new(vec![notes]);
    Ok(())
}

impl Part9 {
    fn levels(&self, src: &Source) -> PerLevel<QCfg> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, QCfg::from),
            None => PerLevel::one(self.base.clone()),
        }
    }

    /// v0.8's flight of particle j of k from (u, v) at scale s (`salt2`
    /// tells secondaries apart), then n10 / n09 / n06 / n11 (n05 and n03
    /// act on the landing in [`Part9::land`]).
    #[allow(clippy::too_many_arguments)]
    fn fly(
        &self,
        c: &QCfg,
        n: &TNote,
        u: f64,
        v: f64,
        j: usize,
        k: usize,
        s: f64,
        salt2: u64,
    ) -> Fly {
        let mut r = Rng::for_note(c.seed, salt::SHAPE ^ salt2 ^ (j as u64) << 8, n);
        let frac = if c.full {
            j as f64 / k as f64
        } else if k == 1 {
            0.5
        } else {
            j as f64 / (k - 1) as f64
        };
        let mut a = c.dir - c.width / 2.0 + c.width * frac;
        // n10's shape point (its angle and its radius), n09's ring radius in
        // place of the distance.
        let (mut reach, mut mag) = (c.dist, 1.0);
        if let Some((shape, size)) = c.shape {
            let (px, py) = shape_point(shape, j as f64 / k as f64);
            a = py.atan2(px);
            mag = px.hypot(py);
            reach = size;
        }
        if let Some((radius, _)) = c.implode {
            reach = radius;
        }
        reach *= mag;
        let mut speed = 1.0;
        if let Some(jt) = c.jitter {
            a += r.range_f64(-jt, jt);
            speed = r.range_f64(0.5, 1.0);
        }
        let mut d = reach * speed * s;
        if let Some(kd) = c.drag {
            d *= (1.0 - (-kd).exp()) / kd;
        }
        // n09: the particles start on the ring and converge on the note —
        // placed before it in time, mirrored.
        let dx = if c.implode.is_some() {
            -a.cos().abs() * d
        } else {
            a.cos() * d
        };
        let (mut x, mut y) = (u + dx, v + a.sin() * d);
        if let Some((_, true)) = c.implode {
            // Arriving before the note: the fragments end where it starts.
            x -= c.frag / self.bx.width();
        }
        if let Some(gv) = c.grav {
            y -= gv * speed * speed * s;
        }
        let t = d / c.dist.max(1e-9);
        let flight = if c.dist > 0.0 { d / c.dist } else { 0.0 };
        x += c.wind.0 * flight;
        y += c.wind.1 * flight;
        if let Some((amount, scale)) = c.turb
            && amount > 0.0
        {
            // Curl of seeded smooth noise: divergence-free swirls.
            let salt = salt_of("particle_burst.n11");
            let h = 1e-3;
            let f = |px: f64, py: f64| noise2(c.seed, salt, px / scale, py / scale);
            let cx = (f(x, y + h) - f(x, y - h)) / (2.0 * h) * scale;
            let cy = -(f(x + h, y) - f(x - h, y)) / (2.0 * h) * scale;
            let m = amount * c.dist.max(0.05) * flight.max(0.25);
            x += cx * m;
            y += cy * m;
        }
        x = x.max(-1.0);
        y = y.clamp(-1.0, 2.0);
        Fly { x, y, t, a, speed }
    }

    /// v0.8's fragment at (x, y) after a flight `t`, with n07 / n08 and the
    /// velocity factor `vf` (n03's bounces, n04's trail): `len` = None for
    /// the fragment length.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        c: &QCfg,
        out: &mut Output,
        n: &TNote,
        f: &Fly,
        (x, y): (f64, f64),
        j: usize,
        vf: f64,
        len: Option<f64>,
    ) -> bool {
        let (ax, ay) = self.bx.from_unit(x, y);
        let mut tpl = if c.cycle {
            sub_stepped(&self.tracks, n, j as i64 + 1)
        } else {
            *n
        };
        tpl.vel = vel_scaled(n, c.vdec.map_or(1.0, |e| 1.0 - (1.0 - e) * f.t.min(1.0)));
        if vf != 1.0 {
            tpl.vel = vel_scaled(&tpl, vf);
        }
        if let Some((palette, by)) = &c.colours {
            let q = match by {
                0 => f.t.clamp(0.0, 1.0),
                1 => ((f.speed - 0.5) / 0.5).clamp(0.0, 1.0),
                _ => rem1(f.a / TAU),
            };
            let pos = if palette.is_empty() {
                (q * 4.0).floor().min(3.0) as i64
            } else {
                palette[((q * palette.len() as f64) as usize).min(palette.len() - 1)]
            };
            (tpl.sub, tpl.ch) = sub_at(&self.tracks, n, pos);
        }
        let length =
            len.unwrap_or_else(|| c.frag * c.gdec.map_or(1.0, |e| 1.0 - (1.0 - e) * f.t.min(1.0)));
        let mut row = ay.floor();
        if let Some(lat) = c.lattice
            && row.abs() < 1e9
        {
            row = lat.nearest(row as i64) as f64;
        }
        out.emit(
            ax.round() as i128,
            length.round().max(1.0) as i128,
            row as i64,
            i64::from(tpl.vel),
            &tpl,
        )
    }

    /// A landing: n05's pull, n03's floor (the hit and the bounces), n04's
    /// trail, then the fragment.
    #[allow(clippy::too_many_arguments)]
    fn land(
        &self,
        c: &QCfg,
        out: &mut Output,
        n: &TNote,
        (u, v): (f64, f64),
        mut f: Fly,
        j: usize,
        target: Option<(f64, f64)>,
    ) -> bool {
        if let (Some(s), Some((tx, ty))) = (c.attract, target) {
            f.x += s * (tx - f.x);
            f.y += s * (ty - f.y);
        }
        let mut hits: Vec<((f64, f64), f64)> = Vec::new();
        if let Some((floor, e, bounces)) = c.bounce {
            let yf = self.bx.to_unit(0.0, f64::from(floor) + 0.5).1;
            if v >= yf && f.y < yf {
                let k = (v - yf) / (v - f.y);
                let hx = u + (f.x - u) * k;
                let rest = (f.x - u) * (1.0 - k);
                f.x = hx;
                f.y = yf;
                let (mut gone, mut amp) = (0.0, 1.0);
                for _ in 0..bounces {
                    amp *= e;
                    if amp <= 0.0 {
                        break;
                    }
                    gone += rest * amp;
                    hits.push(((hx + gone, yf), amp));
                }
            }
        }
        if let Some((length, density)) = c.trail {
            // Notes back along the flight from the landing: spanning the
            // trail length (or the whole flight), each a share of it, fading.
            let w = self.bx.width();
            let span_ticks = ((f.x - u) * w).abs();
            let reach = if span_ticks > 0.0 {
                (length / span_ticks).min(1.0)
            } else {
                0.0
            };
            let each = (length / f64::from(density)).max(1.0);
            for i in 1..=density {
                let q = reach * f64::from(i) / f64::from(density);
                let p = (f.x + (u - f.x) * q, f.y + (v - f.y) * q);
                let fade = 1.0 - f64::from(i) / f64::from(density + 1);
                if !self.emit(c, out, n, &f, p, j, fade, Some(each)) {
                    return false;
                }
            }
        }
        if !self.emit(c, out, n, &f, (f.x, f.y), j, 1.0, None) {
            return false;
        }
        for (p, amp) in hits {
            if !self.emit(c, out, n, &f, p, j, amp, None) {
                return false;
            }
        }
        true
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let lv = self.levels(src);
        let targets: Vec<(f64, f64)> = if lv.all().iter().any(|c| c.attract.is_some()) {
            src.aux
                .first()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(|t| self.bx.to_unit(t.start as f64, f64::from(t.pitch) + 0.5))
                .collect()
        } else {
            Vec::new()
        };
        let w = self.bx.width();
        let mut order = 0usize;
        for (i, n) in src.notes.iter().enumerate() {
            let c = lv.at(i);
            out.set_dim(0);
            if c.keep && !out.push(*n) {
                return out.check();
            }
            if let Some((grid, pattern)) = &c.metric
                && !pattern.is_empty()
            {
                let m = metric_position(&self.ctx.meter, n.start);
                let (slot, _) = m.slot(n.start, *grid);
                if pattern[slot as usize % pattern.len()] == 0 {
                    continue;
                }
            }
            let (u0, v) = self.bx.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
            let k = if c.by_vel {
                ((c.count_max as f64 * f64::from(n.vel) / 127.0).round() as usize).max(1)
            } else {
                c.count_max
            };
            let (emissions, every) = c.emission.unwrap_or((1, 0.0));
            for e in 0..emissions {
                let u = u0 + e as f64 * every / w;
                // Emission e's particles have their own seeds (e = 0 is
                // v0.8's).
                let es = e << 40;
                for j in 0..k {
                    // V09-0143: the particle's number in its note's burst (0
                    // = the first; a fountain's later emissions count on;
                    // second-generation fragments take their parent's).
                    out.set_dim(
                        u32::try_from(e.saturating_mul(k as u64).saturating_add(j as u64))
                            .unwrap_or(u32::MAX),
                    );
                    let target = (!targets.is_empty()).then(|| targets[order % targets.len()]);
                    order += 1;
                    let f = self.fly(c, n, u, v, j, k, 1.0, es);
                    if !self.land(c, out, n, (u, v), f, j, target) {
                        return out.check();
                    }
                    if let Some(m) = c.second {
                        for q in 0..m {
                            let mut f2 =
                                self.fly(c, n, f.x, f.y, q, m, 0.5, (0x5EC0 + j as u64) ^ es);
                            f2.t += f.t;
                            if !self.land(c, out, n, (f.x, f.y), f2, j + q + 1, None) {
                                return out.check();
                            }
                        }
                    }
                }
                out.check()?;
            }
        }
        Ok(())
    }
}

impl Kernel for Part9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)?;
        out.check()
    }
}
