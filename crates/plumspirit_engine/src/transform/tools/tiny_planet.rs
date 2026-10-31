//! Tiny Planet (V09-S48, contract §144 – §145): an original MIDI adaptation
//! of a "tiny planet" polar projection (the parameter accounting is in
//! `docs/references/v0_9/TINY_PLANET.md`): **time → angle**, **pitch →
//! radius**, drawn back into the selection's box through the shared
//! geometry engine ([`Geom`]).
//!
//! For a point (u, v) of the selection's unit box (u = time, v = pitch):
//!
//! * turns a = (ticks from the box's start) / turn length − seam, the turn
//!   length being the whole selection or N note values (**time wrapping**:
//!   later turns overlay the first, or move outwards with **Spiral**);
//!   θ = **Longitude** + direction · 2π · frac(a), 0° pointing up;
//! * v' = v, or 1 − v with **Radial inversion**; r = v' − c with c =
//!   **Latitude** / 180° (0°: the lowest row at the centre — a planet;
//!   180°: the highest row there with the rest folded through it — a
//!   "hollow earth"; negative: a hole of |c| in the middle); + Spiral × the
//!   turn; mapped **Linear** or **Stereographic** (tan(π r / 4): rows crowd
//!   at the centre); × **Radius scale** / **Z** (zoom distance);
//! * the point = **Centre** + r · (sin θ, cos θ) · half the box.
//!
//! **Arc density**: Transform Existing (each note one note: its start and
//! end mapped, its row from its middle), Follow the arcs (the note split
//! along the circle, `Geom`'s Split raster with the arc segments), or
//! Follow and fill the rings (+ the shared Fill). **Subchannel phase**: the
//! note's subchannel stepped by its angle (steps around a circle) or by its
//! turn. Boundary policies as every Geometry tool.

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Fill, Geom, Raster, geom_specs};
use super::super::param::*;
use super::super::preflight::Growth;
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::sweep::note_places;
use super::super::{Ctx, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::xgeom::sub_stepped;
use super::{NOTE_VALUES, boxed, note_value};

pub const TINY_PLANET: ToolDef = ToolDef {
    id: "tiny_planet",
    spec: tp_spec,
    build: tp_build,
};

const TURNS: &[&str] = &["The whole selection is one turn", "Every N note values"];
const DIRS: &[&str] = &["Clockwise", "Counter-clockwise"];
const RADII: &[&str] = &["Linear", "Stereographic (rows crowd at the center)"];
const DENSITIES: &[&str] = &[
    "Transform existing notes (one note each)",
    "Follow the arcs (split along the circle)",
    "Follow the arcs and fill the rings",
];
const PHASES: &[&str] = &[
    "Off",
    "By angle (steps around the circle)",
    "By turn (each turn a step)",
];
/// Most arc segments a note is split into.
pub const MAX_SEGMENTS: i64 = 64;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const TINY_PLANET_HELP: &[(&str, &str)] = &[
    (
        "latitude",
        "Which row sits at the planet's center: 0° puts the lowest row there (a planet), 180° the highest (a hollow earth); negative leaves a hole in the middle.",
    ),
    (
        "longitude",
        "Turns the planet around its center, in degrees.",
    ),
    (
        "zoom",
        "How far away the planet is seen from: larger values make it smaller.",
    ),
    (
        "cx",
        "Where the planet's center lies in time, as a share of the selection's box.",
    ),
    (
        "cy",
        "Where the planet's center lies in pitch, as a share of the selection's box.",
    ),
    ("radius", "How big the planet is, as a share of the box."),
    (
        "turn",
        "How much time makes one full turn around the circle: the whole selection, or a number of note values (later turns lie over the first).",
    ),
    (
        "turn.value",
        "The note value used to measure one turn, with Every N Note Values.",
    ),
    ("turn.n", "How many of those note values make one turn."),
    (
        "seam",
        "Where on the circle the selection's start lies (the seam), as a share of a turn.",
    ),
    (
        "dir",
        "Whether time runs clockwise or counterclockwise around the circle.",
    ),
    (
        "invert",
        "Puts the highest row at the center and the lowest at the rim instead of the other way around.",
    ),
    (
        "radius.map",
        "Linear spaces the rows evenly from the center out; Stereographic crowds them toward the center.",
    ),
    (
        "spiral",
        "Moves each later turn outward (or inward) by this much, so the turns form a spiral instead of lying on top of each other.",
    ),
    (
        "density",
        "Transform Existing keeps one note per note; Follow the Arcs splits notes along the circle; the last choice also fills the rings.",
    ),
    (
        "density.segments",
        "How many pieces each note is split into along its arc.",
    ),
    (
        "phase",
        "Steps the subchannel (color) around the circle: off, by the angle, or once per turn.",
    ),
    (
        "phase.steps",
        "How many subchannel positions the color steps around one circle (or per turn).",
    ),
];

fn tp_spec() -> ToolSpec {
    let mut p = vec![
        float("latitude", "Latitude", -180.0, 180.0, 0.0, "°"),
        float("longitude", "Longitude (rotation)", -360.0, 360.0, 0.0, "°"),
        float("zoom", "Z (zoom distance)", 0.1, 10.0, 1.0, ""),
        float(
            "cx",
            "Center in time (of the box)",
            -100.0,
            200.0,
            50.0,
            "%",
        ),
        float(
            "cy",
            "Center in pitch (of the box)",
            -100.0,
            200.0,
            50.0,
            "%",
        ),
        float("radius", "Radius scale", 1.0, 400.0, 100.0, "%"),
        choice("turn", "Time wrapping", TURNS, 0),
        choice("turn.value", "Turn: note value", NOTE_VALUES, 0),
        int("turn.n", "Turn: note values a turn", 1, 1024, 1),
        float("seam", "Seam position (of a turn)", 0.0, 100.0, 0.0, "%"),
        choice("dir", "Direction", DIRS, 0),
        flag(
            "invert",
            "Radial inversion (the highest row at the center)",
            false,
        ),
        choice("radius.map", "Radius mapping", RADII, 0),
        float(
            "spiral",
            "Spiral (radius added a turn)",
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        choice("density", "Arc density", DENSITIES, 0),
        int(
            "density.segments",
            "Arc segments a note",
            2,
            MAX_SEGMENTS,
            8,
        ),
        choice("phase", "Subchannel phase", PHASES, 0),
        int(
            "phase.steps",
            "Subchannel steps (a circle, or a turn)",
            -64,
            64,
            4,
        ),
    ];
    p.extend(geom_specs());
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, TINY_PLANET_HELP);
    ToolSpec {
        id: "tiny_planet",
        name: "Tiny Planet",
        family: Family::Geometry,
        summary: "Bends the selection into a little planet: time runs around a circle and pitch becomes the distance from its center (latitude, longitude, zoom, center, wrapping, inversion, spiral, arcs and subchannel phase).",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// One note's projection (Control Sweeps vary it per note).
#[derive(Clone, Copy, Debug)]
struct Proj {
    /// Latitude as the centre row c (fraction of the box).
    c: f64,
    /// Longitude (radians).
    lon: f64,
    zoom: f64,
    centre: (f64, f64),
    radius: f64,
    /// Seam (turns).
    seam: f64,
    /// Spiral (radius a turn).
    spiral: f64,
    /// Subchannel steps.
    steps: i64,
    /// Ticks a turn.
    turn: f64,
}

#[derive(Debug)]
struct Planet {
    g: Geom,
    tracks: Vec<SourceTrack>,
    base: Proj,
    /// Ticks of the turn's note value (`None`: the whole selection is a
    /// turn).
    unit: Option<f64>,
    dir: f64,
    invert: bool,
    stereo: bool,
    phase: usize,
    ctx: Ctx,
    growth: Growth,
}

impl Planet {
    /// Turns from the box's start at unit time `u`.
    #[inline]
    fn turns(&self, pr: &Proj, u: f64) -> f64 {
        u * self.g.bbox.width() / pr.turn - pr.seam
    }

    /// The polar map of the unit point (u, v).
    #[inline]
    fn map(&self, pr: &Proj, u: f64, v: f64) -> (f64, f64) {
        let a = self.turns(pr, u);
        let turn = a.floor();
        let theta = pr.lon + self.dir * std::f64::consts::TAU * (a - turn);
        let vv = if self.invert { 1.0 - v } else { v };
        let mut r = vv - pr.c + pr.spiral * turn;
        if self.stereo {
            r = r.signum() * (std::f64::consts::FRAC_PI_4 * r.abs().min(1.9)).tan();
        }
        r *= pr.radius / pr.zoom;
        let (s, c) = theta.sin_cos();
        (pr.centre.0 + 0.5 * r * s, pr.centre.1 + 0.5 * r * c)
    }

    /// The subchannel steps of a note from the angle / turn at its middle.
    fn phase_steps(&self, pr: &Proj, n: &TNote) -> i64 {
        if self.phase == 0 || pr.steps == 0 {
            return 0;
        }
        let mid = (n.start as f64 + n.end() as f64) / 2.0;
        let (u, _) = self.g.bbox.to_unit(mid, f64::from(n.pitch));
        let a = self.turns(pr, u);
        match self.phase {
            1 => {
                ((a - a.floor()) * pr.steps.unsigned_abs() as f64).floor() as i64
                    * pr.steps.signum()
            }
            _ => a.floor() as i64 * pr.steps,
        }
    }

    fn projs(&self, src: &Source) -> Vec<Proj> {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return vec![self.base; src.notes.len()];
        };
        let places = note_places(src);
        let b = self.base;
        src.notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let f = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v, n, pl)
                    } else {
                        v
                    }
                };
                Proj {
                    c: f("latitude", b.c * 180.0) / 180.0,
                    lon: f("longitude", b.lon.to_degrees()).to_radians(),
                    zoom: f("zoom", b.zoom).clamp(0.1, 10.0),
                    centre: (
                        f("cx", b.centre.0 * 100.0) / 100.0,
                        f("cy", b.centre.1 * 100.0) / 100.0,
                    ),
                    radius: f("radius", b.radius * 100.0).max(0.0) / 100.0,
                    seam: f("seam", b.seam * 100.0) / 100.0,
                    spiral: f("spiral", b.spiral * 100.0) / 100.0,
                    steps: if sw.drives("phase.steps") {
                        sw.int("phase.steps", b.steps, n, pl)
                    } else {
                        b.steps
                    },
                    turn: match self.unit {
                        Some(unit) if sw.drives("turn.n") => {
                            let n0 = (b.turn / unit).round() as i64;
                            (unit * sw.int("turn.n", n0, n, pl).max(1) as f64).max(1.0)
                        }
                        _ => b.turn,
                    },
                }
            })
            .collect()
    }
}

impl Kernel for Planet {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let projs = self.projs(src);
        for (n, pr) in src.notes.iter().zip(&projs) {
            let mut t = *n;
            let steps = self.phase_steps(pr, n);
            if steps != 0 {
                let s = sub_stepped(&self.tracks, n, steps);
                (t.sub, t.ch) = (s.sub, s.ch);
            }
            if !self
                .g
                .rasterize(&t, &|u, v| self.map(pr, u, v), &self.tracks, out)
            {
                return out.check();
            }
        }
        out.check()
    }
}

fn tp_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let mut g = Geom::from_params(p, b.stats);
    match p.choice("density") {
        0 => {}
        d => {
            g.raster = Raster::Split;
            g.segments = p.int("density.segments").clamp(2, MAX_SEGMENTS) as u32;
            if d == 2 && g.fill.is_none() {
                // The shared Fill with the g.fill controls' settings.
                g.fill = Some(Fill {
                    res: p.int("g.fill_res").clamp(1, 12) as u8,
                    fade: p.choice("g.fill_policy") == 1,
                    keep_sub: p.flag("g.keep_sub"),
                    merge: p.choice("g.overlap") == 0,
                    mode: super::super::geom::FillMode::Rows,
                });
            }
        }
    }
    let width = g.bbox.width().max(1.0);
    let unit = (p.choice("turn") == 1).then(|| note_value(p.choice("turn.value"), 0, b.ctx) as f64);
    let turn = unit.map_or(width, |u| (u * p.int("turn.n").max(1) as f64).max(1.0));
    let base = Proj {
        c: p.float("latitude").clamp(-180.0, 180.0) / 180.0,
        lon: p.float("longitude").to_radians(),
        zoom: p.float("zoom").clamp(0.1, 10.0),
        centre: (p.float("cx") / 100.0, p.float("cy") / 100.0),
        radius: p.float("radius").max(0.0) / 100.0,
        seam: p.float("seam") / 100.0,
        spiral: p.float("spiral") / 100.0,
        steps: p.int("phase.steps").clamp(-64, 64),
        turn,
    };
    let growth = match g.per_note() {
        0 | 1 => Growth::Map,
        k => Growth::Expand(k),
    };
    boxed(Planet {
        g,
        tracks: b.tracks.to_vec(),
        base,
        unit,
        dir: if p.choice("dir") == 1 { -1.0 } else { 1.0 },
        invert: p.flag("invert"),
        stereo: p.choice("radius.map") == 1,
        phase: p.choice("phase").min(PHASES.len() - 1),
        ctx: b.ctx.clone(),
        growth,
    })
}
