//! Trails (V09-S45, contract §132 – §135): an original MIDI adaptation of a
//! video trails effect (the parameter accounting is in
//! `docs/references/v0_9/TRAILS.md`). The selection is the **source motif**;
//! generation 1, 2, … (and, with Trails Before Start, −1, −2, …) are copies
//! of it, each **Trail Separation** later (earlier), each one step further
//! along every progression:
//!
//! * geometry in the selection's unit box (u = time, v = pitch rows) and a
//!   depth: Scale Trails X / Y about the Pivot, Rotate (in the plane), Tumble
//!   (about the time axis), Spin (about the pitch axis), Position X / Y / Z —
//!   seen from a focal depth in front of the box when anything leaves the
//!   plane, rasterised by the shared geometry engine;
//! * **Feedback** compounds each quantity's step generation by generation
//!   (Scale, Displacement, Z, Rotation Feedback: the next step is the last
//!   one × the feedback) and **Acceleration** changes the feedback itself
//!   (+ the acceleration per generation): step_k = s · Π_{j=2..k} (f + a ·
//!   (j − 2)), factors never below 0; scale steps multiply (in the log
//!   domain), the others add; generations before the start take the
//!   progression backwards (negated, scales inverted);
//! * MIDI attributes: **Trail Intensity** × **Auto Fade** (even, to nothing
//!   after the last trail) or **Manual Fade** (a ratio per trail) on the
//!   velocity — and on the gate when asked —, a gate factor and a subchannel
//!   step per trail;
//! * **Generation state**: Source time (each trail its own generation's
//!   state, the progressions above) or Current time (every trail one step:
//!   the trails differ only in time and fade);
//! * **Jitter**: a seeded variation of time, pitch, scale, velocity or
//!   subchannel over a range of trails (per generation and note);
//! * the source kept, or Trails Only.
//!
//! **Preflight (§135)**: notes × (generations + the source) × the raster's
//! share is the bound; when it passes the cap, the notes are counted (the
//! filled rows each generation's copy covers) unless notes × generations
//! alone already passes it — a 5M-note × 100-trail request is refused with
//! its notes, memory, undo and runtime before anything is allocated.

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Geom, Raster, geom_specs};
use super::super::param::*;
use super::super::preflight::{Estimate, Growth};
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::shared::seeded_stream::{note_key, salt_of, stream};
use super::super::{Kernel, Output, Source, SourceStats, SourceTrack, TNote, TransformError};
use super::boxed;
use super::xgeom::{sub_stepped, vel_scaled};

pub const TRAILS: ToolDef = ToolDef {
    id: "trails",
    spec: trails_spec,
    build: trails_build,
};

const FADES: &[&str] = &[
    "None",
    "Auto Fade (even, to nothing after the last trail)",
    "Manual Fade (a ratio per trail)",
];
const STATES: &[&str] = &[
    "Source time (each trail its own generation's state)",
    "Current time (every trail one step)",
];
/// The jitter's destinations.
pub const JITTER: &[&str] = &["Time", "Pitch", "Scale", "Velocity", "Subchannel"];
/// Most generations on each side of the source.
pub const MAX_TRAILS: i64 = 1000;
const NEAR_CLIP: f64 = 0.05;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const TRAILS_HELP: &[(&str, &str)] = &[
    ("count", "How many trails follow the selection."),
    (
        "separation",
        "How much later each trail starts than the previous one, in ticks.",
    ),
    (
        "before",
        "How many trails come before the selection, taking the changes backward.",
    ),
    (
        "intensity",
        "The trails' velocity as a share of the selection's.",
    ),
    (
        "fade",
        "How the trails fade: not at all, evenly down to nothing after the last trail, or by a ratio per trail.",
    ),
    (
        "fade.ratio",
        "Each trail's velocity as a share of the previous trail's, used with Manual Fade.",
    ),
    ("fade.gate", "Also shortens the notes as the trails fade."),
    (
        "state",
        "Source Time lets each trail change one step further than the last; Current Time gives every trail the same one step, so trails differ only in time and fade.",
    ),
    (
        "scale_x",
        "Each trail's length in time as a share of the previous trail's.",
    ),
    (
        "scale_y",
        "Each trail's pitch range as a share of the previous trail's.",
    ),
    (
        "pos_x",
        "How far each trail moves in time from the previous one, as a share of the box.",
    ),
    (
        "pos_y",
        "How many semitones each trail moves from the previous one.",
    ),
    (
        "pos_z",
        "How far each trail moves in depth from the previous one, as a share of the box (away is smaller).",
    ),
    (
        "rotate",
        "How far each trail turns in the plane from the previous one, in degrees.",
    ),
    (
        "tumble",
        "How far each trail tips over around the time axis from the previous one, in degrees.",
    ),
    (
        "spin",
        "How far each trail spins around the pitch axis from the previous one, in degrees.",
    ),
    (
        "pivot_x",
        "The point the trails scale and turn around: its place in time, as a share of the box.",
    ),
    (
        "pivot_y",
        "The point the trails scale and turn around: its place in pitch, as a share of the box.",
    ),
    (
        "focal",
        "How far the viewer is in front of the box when trails leave the plane; nearer exaggerates the depth.",
    ),
    (
        "fb.scale",
        "Multiplies each scale step by this for the next trail, so the scaling speeds up (over 100 %) or slows down.",
    ),
    (
        "fb.disp",
        "Multiplies each movement step by this for the next trail, so the movement speeds up (over 100 %) or slows down.",
    ),
    (
        "fb.z",
        "Multiplies each depth step by this for the next trail, so the depth movement speeds up (over 100 %) or slows down.",
    ),
    (
        "fb.rot",
        "Multiplies each rotation step by this for the next trail, so the turning speeds up (over 100 %) or slows down.",
    ),
    (
        "acc.scale",
        "Changes the scale feedback itself a little more each trail.",
    ),
    (
        "acc.disp",
        "Changes the movement feedback itself a little more each trail.",
    ),
    (
        "acc.z",
        "Changes the depth feedback itself a little more each trail.",
    ),
    (
        "acc.rot",
        "Changes the rotation feedback itself a little more each trail.",
    ),
    (
        "gate",
        "Each trail's note lengths as a share of the previous trail's.",
    ),
    (
        "sub_step",
        "How many subchannel positions each trail moves from the previous one.",
    ),
    ("jitter", "Varies the trails a little at random (seeded)."),
    (
        "jitter.dest",
        "What the jitter varies: time, pitch, scale, velocity or subchannel.",
    ),
    ("jitter.amount", "How strong the random variation is."),
    ("jitter.from", "The first trail that gets jitter."),
    ("jitter.to", "The last trail that gets jitter."),
    (
        "keep_source",
        "Keeps the selection itself; off leaves only the trails.",
    ),
];

fn trails_spec() -> ToolSpec {
    let mut p = vec![
        int("count", "Trail Count", 0, MAX_TRAILS, 6),
        ticks("separation", "Trail Separation", 0, 1 << 40, 240),
        int("before", "Trails Before Start", 0, MAX_TRAILS, 0),
        float("intensity", "Trail Intensity", 0.0, 200.0, 100.0, "%"),
        choice("fade", "Fade", FADES, 1),
        float(
            "fade.ratio",
            "Manual Fade ratio per trail",
            1.0,
            100.0,
            80.0,
            "%",
        ),
        flag("fade.gate", "Fade the gate too", false),
        choice("state", "Generation state", STATES, 0),
        float(
            "scale_x",
            "Scale Trails X (per trail)",
            10.0,
            400.0,
            100.0,
            "%",
        ),
        float(
            "scale_y",
            "Scale Trails Y (per trail)",
            10.0,
            400.0,
            100.0,
            "%",
        ),
        float(
            "pos_x",
            "Position X Trails (of the box, per trail)",
            -200.0,
            200.0,
            0.0,
            "%",
        ),
        float(
            "pos_y",
            "Position Y Trails (semitones per trail)",
            -127.0,
            127.0,
            0.0,
            "",
        ),
        float(
            "pos_z",
            "Position Z Trails (depth, of the box, per trail)",
            -200.0,
            200.0,
            0.0,
            "%",
        ),
        float(
            "rotate",
            "Rotate Trails (in the plane, per trail)",
            -360.0,
            360.0,
            0.0,
            "°",
        ),
        float(
            "tumble",
            "Tumble Trails (about time, per trail)",
            -360.0,
            360.0,
            0.0,
            "°",
        ),
        float(
            "spin",
            "Spin Trails (about pitch, per trail)",
            -360.0,
            360.0,
            0.0,
            "°",
        ),
        float("pivot_x", "Pivot X (of the box)", -100.0, 200.0, 50.0, "%"),
        float("pivot_y", "Pivot Y (of the box)", -100.0, 200.0, 50.0, "%"),
        float(
            "focal",
            "Focal depth (3D trails, of the box)",
            20.0,
            2000.0,
            200.0,
            "%",
        ),
        float("fb.scale", "Scale Feedback", 0.0, 400.0, 100.0, "%"),
        float("fb.disp", "Displacement Feedback", 0.0, 400.0, 100.0, "%"),
        float("fb.z", "Z Feedback", 0.0, 400.0, 100.0, "%"),
        float("fb.rot", "Rotation Feedback", 0.0, 400.0, 100.0, "%"),
        float("acc.scale", "Scale Acceleration", -100.0, 100.0, 0.0, "%"),
        float(
            "acc.disp",
            "Displacement Acceleration",
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        float("acc.z", "Z Acceleration", -100.0, 100.0, 0.0, "%"),
        float("acc.rot", "Rotation Acceleration", -100.0, 100.0, 0.0, "%"),
        float("gate", "Gate per trail", 10.0, 400.0, 100.0, "%"),
        int("sub_step", "Subchannel steps per trail", -64, 64, 0),
        flag("jitter", "Jitter", false),
        choice("jitter.dest", "Jitter destination", JITTER, 0),
        float("jitter.amount", "Jitter amount", 0.0, 100.0, 25.0, "%"),
        int("jitter.from", "Jitter from trail", 1, MAX_TRAILS, 1),
        int("jitter.to", "Jitter to trail", 1, MAX_TRAILS, MAX_TRAILS),
        seed("seed"),
        flag("keep_source", "Keep the source (off: Trails Only)", true),
    ];
    p.extend(geom_specs());
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, TRAILS_HELP);
    ToolSpec {
        id: "trails",
        name: "Trails",
        family: Family::Geometry,
        summary: "Repeats the selection as trails — each generation later (or earlier), scaled, moved, turned in 3D and faded a step further, with feedback, acceleration and jitter.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// Largest magnitude of a progression's sum: a feedback above 100 % over
/// hundreds of generations would otherwise reach infinity (4^1000), and an
/// infinite scale, shift or angle has no note. Far beyond any keyboard or
/// song (box units / rows / radians), so the documented values below it are
/// exact.
pub const PROGRESSION_CAP: f64 = 1e15;
/// Largest magnitude of a generation's log scale (e^±40: a note stretched
/// past every tick, or shrunk to nothing — the boundary policy decides).
const LOG_SCALE_CAP: f64 = 40.0;

/// The sum of the first `n` steps of a progression of step `s`, feedback `f`
/// and acceleration `a` (fractions): step_1 = s, step_k = step_{k−1} · max(0,
/// f + a · (k − 2)); kept within ±[`PROGRESSION_CAP`].
pub fn progression(s: f64, f: f64, a: f64, n: u64) -> f64 {
    let mut step = s;
    let mut total = 0.0f64;
    for k in 1..=n {
        if k > 1 {
            step =
                (step * (f + a * (k - 2) as f64).max(0.0)).clamp(-PROGRESSION_CAP, PROGRESSION_CAP);
            if step == 0.0 {
                break;
            }
        }
        total = (total + step).clamp(-PROGRESSION_CAP, PROGRESSION_CAP);
    }
    total
}

/// One generation's state.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Gen {
    /// The generation (negative before the start).
    g: i64,
    scale: (f64, f64),
    /// Box units (x), rows (y), box units (z).
    disp: (f64, f64, f64),
    /// Radians: rotate, tumble, spin.
    rot: (f64, f64, f64),
    vel: f64,
    gate: f64,
    sub: i64,
}

#[derive(Clone, Debug)]
struct Trails {
    g: Geom,
    tracks: Vec<SourceTrack>,
    gens: Vec<Gen>,
    keep_source: bool,
    separation: f64,
    pivot: (f64, f64),
    focal: f64,
    /// Destination, amount (fraction), first and last trail, seed.
    jitter: Option<(usize, f64, i64, i64, u64)>,
    /// The loose bound before the notes are known.
    growth: Growth,
}

impl Trails {
    /// Unit point (u, v) of a note through generation `ge` (with `js` the
    /// jitter's scale factor): scale about the pivot, rotate, tumble, spin,
    /// move; projected from the focal depth when out of the plane.
    #[inline]
    fn map(&self, ge: &Gen, js: f64, u: f64, v: f64) -> (f64, f64) {
        let (px, py) = self.pivot;
        let rows = self.g.bbox.rows();
        let (mut x, mut y, mut z) = ((u - px) * ge.scale.0 * js, (v - py) * ge.scale.1 * js, 0.0);
        let (s, c) = ge.rot.0.sin_cos();
        (x, y) = (x * c - y * s, x * s + y * c);
        let (s, c) = ge.rot.1.sin_cos();
        (y, z) = (y * c - z * s, y * s + z * c);
        let (s, c) = ge.rot.2.sin_cos();
        (z, x) = (z * c - x * s, z * s + x * c);
        x += px + ge.disp.0;
        y += py + ge.disp.1 / rows;
        z += ge.disp.2;
        if z != 0.0 {
            let k = self.focal / (self.focal + z).max(NEAR_CLIP * self.focal);
            x = 0.5 + (x - 0.5) * k;
            y = 0.5 + (y - 0.5) * k;
        }
        (x, y)
    }

    /// The jitter of note `n` in generation `g` (−1 … 1), when it applies.
    fn jitter_of(&self, n: &TNote, g: i64) -> Option<(usize, f64)> {
        let (dest, amount, from, to, seed) = self.jitter?;
        let k = g.unsigned_abs() as i64;
        if g == 0 || k < from || k > to || amount == 0.0 {
            return None;
        }
        let key = if dest == 2 { 0 } else { note_key(n) };
        let mut r = stream(seed, salt_of("trails.jitter"), &[key, g as u64]);
        Some((dest, amount * (2.0 * r.unit() - 1.0)))
    }

    /// Emits note `n` of generation `ge` (the source: generation 0, the
    /// identity); false once the output is full.
    fn emit(&self, n: &TNote, ge: &Gen, out: &mut Output) -> bool {
        let jit = self.jitter_of(n, ge.g);
        let mut t = *n;
        let mut dt = ge.g as f64 * self.separation;
        let mut js = 1.0;
        let mut dy = 0.0;
        let mut vel = ge.vel;
        let mut sub = ge.sub;
        if let Some((dest, j)) = jit {
            match dest {
                0 => {
                    let span = if self.separation > 0.0 {
                        self.separation
                    } else {
                        self.g.bbox.width()
                    };
                    dt += j * span;
                }
                1 => dy = (j * 12.0).round(),
                2 => js = (1.0 + j).max(0.05),
                3 => vel *= (1.0 + j).max(0.0),
                _ => sub += (j * 8.0).round() as i64,
            }
        }
        if vel != 1.0 {
            t.vel = vel_scaled(n, vel);
        }
        if sub != 0 {
            let s = sub_stepped(&self.tracks, n, sub);
            (t.sub, t.ch) = (s.sub, s.ch);
        }
        let w = self.g.bbox.width();
        let rows = self.g.bbox.rows();
        let from = out.mark();
        // V09-0143: the generation's distance from the source (0 = the
        // source, 1 = the nearest generation before or after it …).
        out.set_dim(u32::try_from(ge.g.unsigned_abs()).unwrap_or(u32::MAX));
        let ok = if ge.g == 0 {
            self.g.rasterize(&t, &|u, v| (u, v), &self.tracks, out)
        } else {
            self.g.rasterize(
                &t,
                &|u, v| {
                    let (x, y) = self.map(ge, js, u, v);
                    (x + dt / w, y + dy / rows)
                },
                &self.tracks,
                out,
            )
        };
        if ge.gate != 1.0 {
            // Every note this copy made (also those waiting for the Shift
            // policy); a length the gate pushes past the last tick is
            // dropped like any overflowing note.
            out.adjust_since(from, |_, o, _, stats| {
                let d = ((o.dur as f64 * ge.gate).round() as u64).max(1);
                if o.start.checked_add(d).is_none() {
                    stats.overflow_dropped += 1;
                    return false;
                }
                o.dur = d;
                true
            });
        }
        ok
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let id = Gen {
            g: 0,
            scale: (1.0, 1.0),
            disp: (0.0, 0.0, 0.0),
            rot: (0.0, 0.0, 0.0),
            vel: 1.0,
            gate: 1.0,
            sub: 0,
        };
        for n in &src.notes {
            if self.keep_source && !self.emit(n, &id, out) {
                return out.check();
            }
            for ge in &self.gens {
                if !self.emit(n, ge, out) {
                    return out.check();
                }
            }
            out.check()?;
        }
        Ok(())
    }

    /// The copies a note makes (generations and the source).
    fn copies(&self) -> u64 {
        self.gens.len() as u64 + u64::from(self.keep_source)
    }

    /// An upper bound of the notes from these source notes: the loose one
    /// while it fits under the cap; else, unless notes × copies already
    /// passes it, counted per note and copy (the raster's segments × the
    /// rows the filled row covers).
    fn bound(&self, src: &Source, cap: u64) -> u64 {
        let n = src.notes.len() as u64;
        let loose = n
            .saturating_mul(self.copies())
            .saturating_mul(self.g.per_note());
        if loose <= cap || self.g.fill.is_none() {
            return loose;
        }
        let least = n.saturating_mul(self.copies());
        if least > cap {
            return least.saturating_mul(2);
        }
        let segs = if self.g.raster == Raster::Split {
            u64::from(self.g.segments.max(1))
        } else {
            1
        };
        let rows = self.g.bbox.rows();
        let p0 = f64::from(self.g.bbox.p0);
        let id = Gen {
            g: 0,
            scale: (1.0, 1.0),
            disp: (0.0, 0.0, 0.0),
            rot: (0.0, 0.0, 0.0),
            vel: 1.0,
            gate: 1.0,
            sub: 0,
        };
        let mut total = 0u64;
        for t in &src.notes {
            let xm = (t.start as f64 + t.end() as f64) / 2.0;
            let (u, v0) = self.g.bbox.to_unit(xm, f64::from(t.pitch));
            let (_, v1) = self.g.bbox.to_unit(xm, f64::from(t.pitch) + 1.0);
            let gens = self
                .keep_source
                .then_some(&id)
                .into_iter()
                .chain(&self.gens);
            for ge in gens {
                let js = match self.jitter_of(t, ge.g) {
                    Some((2, j)) => (1.0 + j).max(0.05),
                    _ => 1.0,
                };
                // Pitch jitter moves the row, never its height.
                let y = |v: f64| {
                    let (_, y) = if ge.g == 0 {
                        (u, v)
                    } else {
                        self.map(ge, js, u, v)
                    };
                    p0 + y * rows
                };
                // Geom's affine part scales rows by |scale y|; a wrapped or
                // clamped undefined area keeps them inside the box.
                let k = self.g.scale.1.abs();
                let (a, b) = (y(v0) * k, y(v1) * k);
                let height = if self.g.undefined == 0 {
                    (a - b).abs()
                } else {
                    rows
                };
                let filled =
                    ((height.min(self.g.top as f64 + 1.0)).ceil() as u64).saturating_add(2);
                total = total.saturating_add(segs.saturating_mul(filled));
            }
        }
        total
    }
}

impl Kernel for Trails {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate(&self, stats: &SourceStats) -> Estimate {
        Estimate::from_growth(self.growth, stats)
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        let cap = super::super::Limits::default().max_notes;
        Estimate::for_notes(src.notes.len() as u64, self.bound(src, cap))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        Trails::run(self, src, out)?;
        out.check()
    }
}

fn trails_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let pct = |k: &str| p.float(k) / 100.0;
    let count = p.int("count").clamp(0, MAX_TRAILS);
    let before = p.int("before").clamp(0, MAX_TRAILS);
    let current = p.choice("state") == 1;
    let (fbs, fbd, fbz, fbr) = (pct("fb.scale"), pct("fb.disp"), pct("fb.z"), pct("fb.rot"));
    let (acs, acd, acz, acr) = (
        pct("acc.scale"),
        pct("acc.disp"),
        pct("acc.z"),
        pct("acc.rot"),
    );
    let (lsx, lsy) = (pct("scale_x").max(0.1).ln(), pct("scale_y").max(0.1).ln());
    let (dx, dy, dz) = (pct("pos_x"), p.float("pos_y"), pct("pos_z"));
    let (rr, rt, rs) = (
        p.float("rotate").to_radians(),
        p.float("tumble").to_radians(),
        p.float("spin").to_radians(),
    );
    let intensity = pct("intensity");
    let fade = p.choice("fade");
    let ratio = pct("fade.ratio");
    let fade_gate = p.flag("fade.gate");
    let gate_step = pct("gate");
    let sub_step = p.int("sub_step");
    let mut gens = Vec::with_capacity((count + before) as usize);
    let side = |g: i64, total: i64| -> Gen {
        let n = if current { 1 } else { g.unsigned_abs() };
        let sign = if g < 0 { -1.0 } else { 1.0 };
        let prog = |s: f64, f: f64, a: f64| sign * progression(s, f, a, n);
        let k = g.unsigned_abs();
        let fade_k = match fade {
            1 => 1.0 - k as f64 / (total + 1) as f64,
            2 => ratio.powi(k.min(i32::MAX as u64) as i32),
            _ => 1.0,
        };
        let mut gate = gate_step.powi(k.min(i32::MAX as u64) as i32);
        if fade_gate {
            gate *= fade_k;
        }
        let lscale = |l: f64| prog(l, fbs, acs).clamp(-LOG_SCALE_CAP, LOG_SCALE_CAP).exp();
        // Angles are periodic: reduced, so sin / cos stay exact.
        let angle = |r: f64| prog(r, fbr, acr).rem_euclid(std::f64::consts::TAU);
        Gen {
            g,
            scale: (lscale(lsx), lscale(lsy)),
            disp: (prog(dx, fbd, acd), prog(dy, fbd, acd), prog(dz, fbz, acz)),
            rot: (angle(rr), angle(rt), angle(rs)),
            vel: intensity * fade_k,
            gate: gate.clamp(0.0, 1e6),
            sub: sub_step.saturating_mul(k as i64),
        }
    };
    for k in (1..=before).rev() {
        gens.push(side(-k, before));
    }
    for k in 1..=count {
        gens.push(side(k, count));
    }
    let k = Trails {
        g,
        tracks: b.tracks.to_vec(),
        gens,
        keep_source: p.flag("keep_source"),
        separation: p.ticks("separation") as f64,
        pivot: (pct("pivot_x"), pct("pivot_y")),
        focal: pct("focal").max(0.2),
        jitter: p.flag("jitter").then(|| {
            (
                p.choice("jitter.dest").min(JITTER.len() - 1),
                pct("jitter.amount").clamp(0.0, 1.0),
                p.int("jitter.from").max(1),
                p.int("jitter.to").max(1),
                p.seed("seed"),
            )
        }),
        growth: Growth::Map,
    };
    let per = k.copies().saturating_mul(k.g.per_note());
    let mut k = k;
    k.growth = match per {
        0 => Growth::Thin,
        1 => Growth::Map,
        _ => Growth::Expand(per),
    };
    boxed(k)
}
