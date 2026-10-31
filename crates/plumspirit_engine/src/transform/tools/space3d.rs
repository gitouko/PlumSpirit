//! 3D (V09-S43, contract §120 – §123): a true projected-note space — not a
//! renamed Perspective. Every selected note is a segment in continuous
//! **X = time** and **Y = pitch space** (the selection's unit box, see
//! `transform/geom.rs`) and **Z = a synthetic depth** taken from a chosen
//! source (note index, pitch, time, velocity, gate, subchannel, a graph, a
//! wave, noise, the radial distance or a custom field formula) plus a depth
//! field. Its points are rotated in 3D about a pivot (X, then Y, then Z),
//! seen by a camera turned about the box's centre (yaw, then pitch, then
//! roll), projected back to the plane — in perspective from the focal depth
//! in front of the box, or orthographically — and rasterised by the shared
//! geometry engine (sampled at the middle by default, or split along the
//! projected path; Duplicate / Fill keep the density of stretched rows). A note's **depth** is its
//! centre's camera-space z, normalised over the selection (0 nearest … 1
//! farthest); it drives the depth-based scale (about the note's projected
//! centre), gate, velocity and subchannel, and the occlusion policy (nearer —
//! or farther — notes hide what they cover of the others on their key, the
//! shared `hide` of Perspective's n08). Every value is a pure function of
//! the notes, the parameters and the seed, so Preview and Apply agree.

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Geom, geom_specs};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::shared::seeded_stream::salt_of;
use super::super::sweep::note_places;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TransformError};
use super::geometry::{hide, sub_pos};
use super::xgeom::{noise2, sub_stepped, vel_scaled};
use super::{Contextual, boxed};
use crate::editor::expr::{Expr, ExprError};
use std::cell::RefCell;
use std::f64::consts::TAU;

pub const TOOL3D: ToolDef = ToolDef {
    id: "tool3d",
    spec: tool3d_spec,
    build: tool3d_build,
};

/// The depth sources (§123).
pub const SOURCES: &[&str] = &[
    "Note index",
    "Pitch",
    "Time",
    "Velocity",
    "Gate",
    "Subchannel",
    "Graph",
    "Wave",
    "Noise",
    "Radial distance",
    "Custom field",
];
const WAVES: &[&str] = &["Sine", "Triangle", "Square", "Saw"];
/// The depth field added to the source (§122).
const FIELDS: &[&str] = &[
    "None",
    "Tilt along time",
    "Tilt along pitch",
    "Bowl",
    "Dome",
    "Saddle",
    "Ripple",
];
const PROJECTIONS: &[&str] = &["Perspective", "Orthographic"];
const OCCLUSION: &[&str] = &[
    "Keep every note",
    "Nearer notes hide farther ones",
    "Farther notes hide nearer ones",
];
const OCC_KEY: &[&str] = &["Key", "Key and subchannel"];
/// The custom field's variables: time and pitch place in the box (0 – 1),
/// the note's index, velocity, gate (length over the longest) and
/// subchannel place (0 – 1), the radial distance and seeded noise (0 – 1).
pub const FIELD_VARS: [&str; 8] = ["u", "v", "i", "vel", "gate", "sub", "r", "noise"];
/// The custom field's default.
pub const FIELD_DEFAULT: &str = "0.5 + 0.5*sin(2*pi*u)*cos(2*pi*v)";
/// The perspective divisor never falls below this share of the focal depth
/// (points at or behind the camera stay finite and in front of it).
const NEAR_CLIP: f64 = 0.05;
/// The depth-based scale's widest factor.
const MAX_SCALE: f64 = 4.0;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const TOOL3D_HELP: &[(&str, &str)] = &[
    (
        "z.source",
        "What gives each note its depth: its place in the selection, its pitch, time, velocity, gate, subchannel, a graph, a wave, noise, its distance from the center, or a formula.",
    ),
    (
        "z.scale",
        "How deep the space is: the depth range as a share of the box (negative turns it around).",
    ),
    (
        "z.offset",
        "Moves every note forward or back in depth, as a share of the box.",
    ),
    (
        "z.graph",
        "The depth over the selection's time, used when the depth comes from Graph.",
    ),
    (
        "z.wave",
        "The wave's shape, used when the depth comes from Wave.",
    ),
    (
        "z.cycles",
        "How many waves fit across the box, used with Wave.",
    ),
    ("z.phase", "Shifts the wave, in degrees, used with Wave."),
    (
        "z.noise",
        "How many noise cells fit across the box, used with Noise (more cells = busier depth).",
    ),
    (
        "field",
        "A shape added to the depth: none, a tilt along time or pitch, a bowl, a dome, a saddle or ripples.",
    ),
    (
        "field.amount",
        "How strongly the depth shape is added (negative turns it inside out).",
    ),
    (
        "rot.x",
        "Turns the notes around the time axis, tipping pitch into depth, in degrees.",
    ),
    (
        "rot.y",
        "Turns the notes around the pitch axis, swinging time into depth, in degrees.",
    ),
    (
        "rot.z",
        "Turns the notes around the depth axis, like a flat rotation, in degrees.",
    ),
    (
        "pivot.x",
        "The point the notes turn around: its place in time, as a share of the box.",
    ),
    (
        "pivot.y",
        "The point the notes turn around: its place in pitch, as a share of the box.",
    ),
    (
        "pivot.z",
        "The point the notes turn around: its depth, as a share of the box.",
    ),
    (
        "cam.yaw",
        "Turns the camera left or right around the box's center, in degrees.",
    ),
    (
        "cam.pitch",
        "Tips the camera up or down around the box's center, in degrees.",
    ),
    ("cam.roll", "Tilts the camera sideways, in degrees."),
    (
        "proj",
        "Perspective makes far notes smaller; Orthographic keeps every size and only flattens the depth.",
    ),
    (
        "focal",
        "How far the camera is in front of the box, as a share of the box; nearer exaggerates the perspective.",
    ),
    ("d.scale", "Makes near notes bigger and far notes smaller."),
    (
        "d.scale.near",
        "The size of the nearest notes, as a share of their own.",
    ),
    (
        "d.scale.far",
        "The size of the farthest notes, as a share of their own.",
    ),
    ("d.gate", "Changes the note lengths by depth."),
    (
        "d.gate.near",
        "The gate (share of each note's length) of the nearest notes.",
    ),
    (
        "d.gate.far",
        "The gate (share of each note's length) of the farthest notes.",
    ),
    (
        "d.vel",
        "Changes the velocity by depth, so far notes can sound softer.",
    ),
    (
        "d.vel.near",
        "The velocity of the nearest notes, as a share of their own.",
    ),
    (
        "d.vel.far",
        "The velocity of the farthest notes, as a share of their own.",
    ),
    (
        "d.sub",
        "Colors the notes in bands by depth, each band on another subchannel.",
    ),
    (
        "d.sub.bands",
        "How many depth bands the notes are divided into.",
    ),
    (
        "d.sub.step",
        "How many subchannel positions each band moves from the previous one.",
    ),
    (
        "occl",
        "Whether notes hide each other where they cover the same key: none hide, nearer ones hide farther ones, or the other way around.",
    ),
    (
        "occl.key",
        "What counts as the same key for hiding: only the pitch, or the pitch and the subchannel.",
    ),
];

fn tool3d_spec() -> ToolSpec {
    let mut p = vec![
        choice("z.source", "Depth (Z) from", SOURCES, 9),
        float("z.scale", "Z scale (depth range, of the box)", -400.0, 400.0, 60.0, "%"),
        float("z.offset", "Z offset (of the box)", -200.0, 200.0, 0.0, "%"),
        curve("z.graph", "Depth graph over time", Curve::line(0.0, 1.0)),
        choice("z.wave", "Depth wave", WAVES, 0),
        float("z.cycles", "Wave cycles over the box", 0.0, 64.0, 2.0, ""),
        float("z.phase", "Wave phase", 0.0, 360.0, 0.0, "°"),
        float("z.noise", "Noise cells across the box", 0.1, 64.0, 4.0, ""),
        seed("seed"),
        text("z.field", "Custom depth field", 1000, FIELD_DEFAULT).help(
            "A formula in u (time, 0 – 1), v (pitch, 0 – 1), i (note index, 0 – 1), vel, gate, sub (0 – 1), r (radial distance, 0 – 1) and noise (0 – 1).",
        ),
        choice("field", "Depth field", FIELDS, 0),
        float("field.amount", "Depth field amount", -200.0, 200.0, 50.0, "%"),
        float("rot.x", "Rotation X (about time)", -360.0, 360.0, 0.0, "°"),
        float("rot.y", "Rotation Y (about pitch)", -360.0, 360.0, 0.0, "°"),
        float("rot.z", "Rotation Z (about depth)", -360.0, 360.0, 0.0, "°"),
        float("pivot.x", "Pivot X (of the box)", -100.0, 200.0, 50.0, "%"),
        float("pivot.y", "Pivot Y (of the box)", -100.0, 200.0, 50.0, "%"),
        float("pivot.z", "Pivot Z (depth, of the box)", -200.0, 200.0, 0.0, "%"),
        float("cam.yaw", "Camera yaw", -180.0, 180.0, 25.0, "°"),
        float("cam.pitch", "Camera pitch", -90.0, 90.0, 15.0, "°"),
        float("cam.roll", "Camera roll", -180.0, 180.0, 0.0, "°"),
        choice("proj", "Projection", PROJECTIONS, 0),
        float("focal", "Focal depth (of the box)", 20.0, 2000.0, 200.0, "%"),
        flag("d.scale", "Depth-based scale", false),
        float("d.scale.near", "Nearest scale", 0.0, 400.0, 120.0, "%"),
        float("d.scale.far", "Farthest scale", 0.0, 400.0, 60.0, "%"),
        flag("d.gate", "Depth-based gate", false),
        float("d.gate.near", "Nearest gate", 1.0, 100.0, 100.0, "%"),
        float("d.gate.far", "Farthest gate", 1.0, 100.0, 40.0, "%"),
        flag("d.vel", "Depth-based velocity", false),
        float("d.vel.near", "Nearest velocity", 0.0, 200.0, 100.0, "%"),
        float("d.vel.far", "Farthest velocity", 0.0, 200.0, 40.0, "%"),
        flag("d.sub", "Depth-based subchannel", false),
        int("d.sub.bands", "Depth bands", 2, 64, 4),
        int("d.sub.step", "Subchannel steps per band", -64, 64, 1),
        choice("occl", "Occlusion", OCCLUSION, 0),
        choice("occl.key", "Same key means", OCC_KEY, 0),
    ];
    // The shared geometry controls; rasterisation samples the middle by
    // default (one note per note: millions of notes stay millions — "Split
    // along the path" makes each note follow its projected line).
    p.extend(geom_specs());
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, TOOL3D_HELP);
    ToolSpec {
        id: "tool3d",
        name: "3D",
        family: Family::Experimental,
        summary: "Places the selection in a 3D space (time, pitch, a synthetic depth), turns it and a camera around it, projects it back with perspective or orthographically and rasterises the result.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// A point in the projected-note space (unit box x, y; depth z).
type V3 = [f64; 3];

/// The rotation by `a` radians in the plane of axes `i`, `j` (from `i`
/// toward `j`).
#[inline]
fn turn(p: V3, i: usize, j: usize, a: f64) -> V3 {
    if a == 0.0 {
        return p;
    }
    let (s, c) = a.sin_cos();
    let mut q = p;
    q[i] = p[i] * c - p[j] * s;
    q[j] = p[i] * s + p[j] * c;
    q
}

/// One note's values of the swept parameters.
#[derive(Clone, Copy, Debug)]
struct Vals {
    /// Rotations X, Y, Z and camera yaw, pitch, roll (radians).
    rot: [f64; 3],
    cam: [f64; 3],
    /// Focal depth, Z scale, Z offset, field amount (box units).
    focal: f64,
    zscale: f64,
    zoff: f64,
    amount: f64,
    /// Depth-based factors at the far end.
    vel_far: f64,
    gate_far: f64,
    scale_far: f64,
}

/// The settings.
#[derive(Clone)]
struct Space {
    g: Geom,
    tracks: Vec<SourceTrack>,
    ctx: Ctx,
    source: usize,
    graph: Curve,
    wave: (usize, f64, f64),
    noise: f64,
    seed: u64,
    custom: Option<Expr>,
    field: usize,
    pivot: V3,
    perspective: bool,
    base: Vals,
    /// Near factors of the depth-based scale, gate and velocity.
    scale: Option<f64>,
    gate: Option<f64>,
    vel: Option<f64>,
    /// Depth bands, subchannel steps per band.
    sub: Option<(u32, i64)>,
    /// Far wins, the key includes the subchannel.
    occl: Option<(bool, bool)>,
}

/// What a note's depth source reads (besides the point's place).
#[derive(Clone, Copy, Debug)]
struct NoteIn {
    index: f64,
    vel: f64,
    gate: f64,
    sub: f64,
}

/// A sentence for the custom field's error `e`.
fn say(e: &ExprError) -> String {
    let at =
        e.at.map(|a| format!(" (at character {})", a + 1))
            .unwrap_or_default();
    format!(
        "The custom depth field cannot be used: {}{at}.",
        e.message.trim_end_matches('.')
    )
}

impl Space {
    /// The depth source's raw value (about 0 – 1) at unit point (u, v).
    fn source(&self, u: f64, v: f64, n: &NoteIn) -> Result<f64, TransformError> {
        let r = (u - 0.5).hypot(v - 0.5) / 0.5f64.hypot(0.5);
        let noise = || {
            0.5 + 0.5
                * noise2(
                    self.seed,
                    salt_of("tool3d.noise"),
                    u * self.noise,
                    v * self.noise,
                )
        };
        Ok(match self.source {
            0 => n.index,
            1 => v,
            2 => u,
            3 => n.vel,
            4 => n.gate,
            5 => n.sub,
            6 => self.graph.eval(u.clamp(0.0, 1.0)),
            7 => {
                let (shape, cycles, phase) = self.wave;
                let x = cycles * u + phase;
                let f = x - x.floor();
                match shape {
                    1 => 1.0 - (2.0 * f - 1.0).abs(),
                    2 => {
                        if f < 0.5 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    3 => f,
                    _ => 0.5 + 0.5 * (TAU * x).sin(),
                }
            }
            8 => noise(),
            10 => match &self.custom {
                Some(e) => {
                    let x = e
                        .eval_vars(&[u, v, n.index, n.vel, n.gate, n.sub, r, noise()])
                        .map_err(|e| TransformError::Invalid(say(&e)))?;
                    if x.is_finite() { x } else { 0.0 }
                }
                None => 0.0,
            },
            _ => r,
        })
    }

    /// The depth field at (u, v).
    fn field_at(&self, u: f64, v: f64) -> f64 {
        let (x, y) = (u - 0.5, v - 0.5);
        match self.field {
            1 => x,
            2 => y,
            3 => 2.0 * (x * x + y * y),
            4 => -2.0 * (x * x + y * y),
            5 => 2.0 * (x * x - y * y),
            6 => 0.25 * (TAU * 3.0 * x.hypot(y)).sin(),
            _ => 0.0,
        }
    }

    /// Unit point (u, v) of a note in camera space.
    fn camera(&self, u: f64, v: f64, n: &NoteIn, w: &Vals) -> Result<V3, TransformError> {
        let z = (self.source(u, v, n)? - 0.5) * w.zscale + w.zoff + w.amount * self.field_at(u, v);
        let pv = self.pivot;
        let mut p = [u - pv[0], v - pv[1], z - pv[2]];
        // Object: X (about time: pitch toward depth), Y (about pitch: depth
        // toward time), Z (in the plane: time toward pitch).
        p = turn(p, 1, 2, w.rot[0]);
        p = turn(p, 2, 0, w.rot[1]);
        p = turn(p, 0, 1, w.rot[2]);
        // The camera turns about the box's centre: yaw (about pitch), pitch
        // (about time), roll (about the view axis).
        let mut q = [p[0] + pv[0] - 0.5, p[1] + pv[1] - 0.5, p[2] + pv[2]];
        q = turn(q, 2, 0, w.cam[0]);
        q = turn(q, 1, 2, w.cam[1]);
        q = turn(q, 0, 1, w.cam[2]);
        Ok(q)
    }

    /// Camera point `q` projected back to the unit plane.
    #[inline]
    fn project(&self, q: V3, w: &Vals) -> (f64, f64) {
        let k = if self.perspective {
            w.focal / (w.focal + q[2]).max(NEAR_CLIP * w.focal)
        } else {
            1.0
        };
        (0.5 + q[0] * k, 0.5 + q[1] * k)
    }

    /// Every note's swept values (the settings' without sweeps).
    fn vals(&self, src: &Source) -> Vec<Vals> {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return Vec::new();
        };
        let places = note_places(src);
        let b = self.base;
        src.notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let deg = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v.to_degrees(), n, pl).to_radians()
                    } else {
                        v
                    }
                };
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                Vals {
                    rot: [
                        deg("rot.x", b.rot[0]),
                        deg("rot.y", b.rot[1]),
                        deg("rot.z", b.rot[2]),
                    ],
                    cam: [
                        deg("cam.yaw", b.cam[0]),
                        deg("cam.pitch", b.cam[1]),
                        deg("cam.roll", b.cam[2]),
                    ],
                    focal: pct("focal", b.focal).max(0.2),
                    zscale: pct("z.scale", b.zscale),
                    zoff: pct("z.offset", b.zoff),
                    amount: pct("field.amount", b.amount),
                    vel_far: pct("d.vel.far", b.vel_far).max(0.0),
                    gate_far: pct("d.gate.far", b.gate_far).clamp(0.01, 1.0),
                    scale_far: pct("d.scale.far", b.scale_far).clamp(0.0, MAX_SCALE),
                }
            })
            .collect()
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let n = src.notes.len();
        if n == 0 {
            return Ok(());
        }
        let per = self.vals(src);
        let w_of = |i: usize| per.get(i).copied().unwrap_or(self.base);
        let longest = src.notes.iter().map(|t| t.dur).max().unwrap_or(1).max(1) as f64;
        let ins: Vec<NoteIn> = src
            .notes
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let subs = self
                    .tracks
                    .get(usize::from(t.track))
                    .map_or(1, |tr| tr.subs.len().max(1));
                NoteIn {
                    index: if n > 1 {
                        i as f64 / (n - 1) as f64
                    } else {
                        0.5
                    },
                    vel: f64::from(t.vel) / 127.0,
                    gate: t.dur as f64 / longest,
                    sub: if subs > 1 {
                        sub_pos(&self.tracks, t) as f64 / (subs - 1) as f64
                    } else {
                        0.0
                    },
                }
            })
            .collect();
        // Each note's centre in camera space; depth = its z over the range.
        let mut mid: Vec<V3> = Vec::with_capacity(n);
        for (i, t) in src.notes.iter().enumerate() {
            let (u, v) = self.g.bbox.to_unit(
                (t.start as f64 + t.end() as f64) / 2.0,
                f64::from(t.pitch) + 0.5,
            );
            mid.push(self.camera(u, v, &ins[i], &w_of(i))?);
        }
        let (lo, hi) = mid
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| {
                (a.min(q[2]), b.max(q[2]))
            });
        let depth = |q: &V3| {
            if hi > lo {
                ((q[2] - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.5
            }
        };
        let base = out.notes.len();
        let mut info: Vec<(f32, bool)> = Vec::new();
        for (i, t) in src.notes.iter().enumerate() {
            let w = w_of(i);
            let d = depth(&mid[i]);
            let lerp = |near: f64, far: f64| near + (far - near) * d;
            let mut e = *t;
            if let Some(near) = self.vel {
                e.vel = vel_scaled(t, lerp(near, w.vel_far));
            }
            if let Some((bands, step)) = self.sub {
                let k = ((d * f64::from(bands)) as i64).min(i64::from(bands) - 1);
                let s = sub_stepped(&self.tracks, t, k * step);
                (e.sub, e.ch) = (s.sub, s.ch);
            }
            let centre = self.project(mid[i], &w);
            let k = self
                .scale
                .map_or(1.0, |near| lerp(near, w.scale_far).clamp(0.0, MAX_SCALE));
            // A point of the note: its camera point projected, scaled about
            // the projected centre (the custom field's first evaluation
            // error is kept and refuses the run).
            let fail: RefCell<Option<TransformError>> = RefCell::new(None);
            let from = out.notes.len();
            let map = |u: f64, v: f64| -> (f64, f64) {
                match self.camera(u, v, &ins[i], &w) {
                    Ok(q) => {
                        let (x, y) = self.project(q, &w);
                        (centre.0 + (x - centre.0) * k, centre.1 + (y - centre.1) * k)
                    }
                    Err(err) => {
                        fail.borrow_mut().get_or_insert(err);
                        (u, v)
                    }
                }
            };
            let ok = self.g.rasterize(&e, &map, &self.tracks, out);
            if let Some(err) = fail.into_inner() {
                return Err(err);
            }
            if let Some(near) = self.gate {
                let f = lerp(near, w.gate_far).clamp(0.01, 1.0);
                for o in &mut out.notes[from..] {
                    o.dur = ((o.dur as f64 * f).round() as u64).max(1);
                }
            }
            for _ in from..out.notes.len() {
                info.push((d as f32, false));
            }
            if !ok {
                return out.check();
            }
            out.check()?;
        }
        if self.occl.is_some() {
            hide(&mut out.notes, base, &info, self.occl);
        }
        Ok(())
    }
}

fn tool3d_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let source = p.choice("z.source").min(SOURCES.len() - 1);
    let custom = if source == 10 {
        Some(
            Expr::parse_vars(&p.text("z.field"), &FIELD_VARS)
                .map_err(|e| TransformError::Invalid(say(&e)))?,
        )
    } else {
        None
    };
    let rad = |k: &str| p.float(k).to_radians();
    let pct = |k: &str| p.float(k) / 100.0;
    let k = Space {
        g,
        tracks: b.tracks.to_vec(),
        ctx: b.ctx.clone(),
        source,
        graph: p.curve("z.graph"),
        wave: (
            p.choice("z.wave").min(3),
            p.float("z.cycles").clamp(0.0, 64.0),
            p.float("z.phase") / 360.0,
        ),
        noise: p.float("z.noise").clamp(0.1, 64.0),
        seed: p.seed("seed"),
        custom,
        field: p.choice("field").min(FIELDS.len() - 1),
        pivot: [pct("pivot.x"), pct("pivot.y"), pct("pivot.z")],
        perspective: p.choice("proj") == 0,
        base: Vals {
            rot: [rad("rot.x"), rad("rot.y"), rad("rot.z")],
            cam: [rad("cam.yaw"), rad("cam.pitch"), rad("cam.roll")],
            focal: pct("focal").max(0.2),
            zscale: pct("z.scale"),
            zoff: pct("z.offset"),
            amount: pct("field.amount"),
            vel_far: pct("d.vel.far").max(0.0),
            gate_far: pct("d.gate.far").clamp(0.01, 1.0),
            scale_far: pct("d.scale.far").clamp(0.0, MAX_SCALE),
        },
        scale: p
            .flag("d.scale")
            .then(|| pct("d.scale.near").clamp(0.0, MAX_SCALE)),
        gate: p
            .flag("d.gate")
            .then(|| pct("d.gate.near").clamp(0.01, 1.0)),
        vel: p.flag("d.vel").then(|| pct("d.vel.near").max(0.0)),
        sub: p.flag("d.sub").then(|| {
            (
                p.int("d.sub.bands").clamp(2, 64) as u32,
                p.int("d.sub.step").clamp(-64, 64),
            )
        }),
        occl: match p.choice("occl") {
            1 => Some((false, p.choice("occl.key") == 1)),
            2 => Some((true, p.choice("occl.key") == 1)),
            _ => None,
        },
    };
    let per = k.g.per_note();
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
