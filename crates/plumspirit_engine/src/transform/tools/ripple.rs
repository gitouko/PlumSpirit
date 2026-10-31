//! Ripple (V09-S49, contract §146 – §148): an original MIDI adaptation of
//! a video ripple as **radial note-space displacement** (the parameter
//! accounting is in `docs/references/v0_9/RIPPLE.md`), drawn through the
//! shared geometry engine ([`Geom`]).
//!
//! In the selection's unit box (u = time, v = pitch), for a point P and a
//! source centre C: r = P − C, its distance d measured with the **Stretch**
//! (the axis at Stretch angle scaled), n̂ = r / |r| and t̂ its perpendicular;
//!
//! * the envelope A(d): 0 inside the **Inside Radius**, 1 up to the
//!   **Radius**, then (1 − (d − R) / R)^(1 + Falloff / 25) clamped at 0 —
//!   with Falloff 0 nothing moves beyond 2 × Radius;
//! * the phase φ = 2π (d / **Wave Width** − **Speed** · u) − **Phase**
//!   (MIDI has no frames: Speed is waves across the selection; negative
//!   runs inwards; 0 stands still);
//! * the waveform w(φ) ∈ [−1, 1], each with its own formula ([`wave`]):
//!   Sine, Triangle, Square, Sawtooth, Circle (alternating half discs),
//!   Semi-Circle (upper half discs), Uncircle (the complement of a half
//!   disc), Half-Sine (the positive half of a sine);
//! * **Chaos** blends w with a seeded value per ring and source;
//! * the displacement A · Intensity · (Height · w(φ) · n̂ + Perpendicular
//!   Height · w(φ) · (1 + Twirl · d / R) · t̂) — Height along the wave's
//!   motion, Perpendicular Height across it, Twirl (Off / On / Super / Hyper:
//!   0 / 1 / 3 / 8) twisting with the distance, Intensity Low / Normal /
//!   Super (×0.1 / ×1 / ×10);
//! * several **sources** (a PlumSpirit extension): the centre and up to 7
//!   more on a ring of Spread around it, combined by Sum, Strongest or
//!   Average;
//! * **Light**: the note's velocity scaled by 1 + Light · the wave's slope
//!   at its middle (shading, no geometry).

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Geom, geom_specs};
use super::super::param::*;
use super::super::preflight::Growth;
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::shared::seeded_stream::{salt_of, stream};
use super::super::sweep::note_places;
use super::super::{Ctx, Kernel, Output, Source, SourceTrack, TransformError};
use super::boxed;
use super::xgeom::vel_scaled;
use std::f64::consts::{PI, TAU};

pub const RIPPLE: ToolDef = ToolDef {
    id: "ripple",
    spec: ripple_spec,
    build: ripple_build,
};

/// The waveforms (§148), each its own formula.
pub const WAVES: &[&str] = &[
    "Sine",
    "Triangle",
    "Square",
    "Sawtooth",
    "Circle",
    "Semi-Circle",
    "Uncircle",
    "Half-Sine",
];
const TWIRLS: &[&str] = &["Off", "On", "Super", "Hyper"];
const INTENSITIES: &[&str] = &["Normal", "Low (×0.1)", "Super (×10)"];
const COMBINE: &[&str] = &["Sum", "Strongest wins", "Average"];
/// Most ripple sources.
pub const MAX_SOURCES: i64 = 8;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const RIPPLE_HELP: &[(&str, &str)] = &[
    (
        "cx",
        "Where the ripple's center lies in time, as a share of the selection's box.",
    ),
    (
        "cy",
        "Where the ripple's center lies in pitch, as a share of the selection's box.",
    ),
    (
        "radius",
        "How far the ripple reaches at full strength, as a share of the box; beyond it the ripple fades.",
    ),
    (
        "inside",
        "Notes closer to the center than this do not move, as a share of the box.",
    ),
    (
        "height",
        "How far notes move toward and away from the center as the wave passes, as a share of the box.",
    ),
    (
        "perp",
        "How far notes move sideways around the center as the wave passes, as a share of the box.",
    ),
    (
        "width",
        "The distance from one wave to the next, as a share of the box.",
    ),
    (
        "speed",
        "How many waves travel outward over the selection's time (negative = inward, 0 = still).",
    ),
    ("phase", "Shifts the waves outward or inward, in degrees."),
    (
        "falloff",
        "How fast the ripple fades beyond the radius (0 fades evenly to nothing at twice the radius).",
    ),
    (
        "wave",
        "The shape of each wave: sine, triangle, square, sawtooth, circle, semicircle, uncircle or half sine.",
    ),
    (
        "twirl",
        "Twists the sideways movement more with distance from the center: off, on, super or hyper.",
    ),
    (
        "intensity",
        "Multiplies the whole movement: normal, low (a tenth) or super (ten times).",
    ),
    (
        "stretch",
        "Stretches the rings into ovals: the distance along the stretch angle is scaled by this.",
    ),
    (
        "stretch.angle",
        "The direction the rings are stretched in, in degrees.",
    ),
    (
        "chaos",
        "Mixes a random value (seeded) into each ring, so the waves become uneven.",
    ),
    (
        "light",
        "Shades the notes: the velocity rises on one slope of each wave and falls on the other (negative swaps them).",
    ),
    (
        "sources",
        "How many ripple centers there are; the extra ones sit on a ring around the center.",
    ),
    (
        "sources.spread",
        "The radius of the ring the extra centers sit on, as a share of the box.",
    ),
    (
        "sources.angle",
        "Turns the ring of extra centers, in degrees.",
    ),
    (
        "sources.mode",
        "How the ripples of several centers combine: added up, the strongest one, or their average.",
    ),
];

fn ripple_spec() -> ToolSpec {
    let mut p = vec![
        float(
            "cx",
            "Center Point: time (of the box)",
            -100.0,
            200.0,
            50.0,
            "%",
        ),
        float(
            "cy",
            "Center Point: pitch (of the box)",
            -100.0,
            200.0,
            50.0,
            "%",
        ),
        float("radius", "Radius", 0.0, 400.0, 25.0, "%"),
        float("inside", "Inside Radius", 0.0, 400.0, 0.0, "%"),
        float("height", "Height (along the wave)", -100.0, 100.0, 5.0, "%"),
        float("perp", "Perpendicular Height", -100.0, 100.0, 0.0, "%"),
        float("width", "Wave Width", 0.5, 200.0, 10.0, "%"),
        float(
            "speed",
            "Speed (waves across the selection)",
            -64.0,
            64.0,
            0.0,
            "",
        ),
        float("phase", "Phase", -360.0, 360.0, 0.0, "°"),
        float("falloff", "Falloff", 0.0, 100.0, 0.0, ""),
        choice("wave", "Waveform", WAVES, 0),
        choice("twirl", "Twirl", TWIRLS, 0),
        choice("intensity", "Intensity", INTENSITIES, 0),
        float("stretch", "Stretch", 10.0, 400.0, 100.0, "%"),
        float("stretch.angle", "Stretch Angle", -180.0, 180.0, 0.0, "°"),
        float("chaos", "Chaos (seeded rings)", 0.0, 100.0, 0.0, "%"),
        seed("seed"),
        float(
            "light",
            "Light (velocity by the wave's slope)",
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        int("sources", "Ripple sources", 1, MAX_SOURCES, 1),
        float(
            "sources.spread",
            "Sources: ring around the center",
            0.0,
            200.0,
            25.0,
            "%",
        ),
        float(
            "sources.angle",
            "Sources: ring rotation",
            -360.0,
            360.0,
            0.0,
            "°",
        ),
        choice("sources.mode", "Sources combine", COMBINE, 0),
    ];
    p.extend(geom_specs());
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, RIPPLE_HELP);
    ToolSpec {
        id: "ripple",
        name: "Ripple",
        family: Family::Geometry,
        summary: "Moves the selected notes like ripples on water around one or more centers — radius, height, perpendicular height, wave width, speed, phase, inside radius, falloff and eight waveforms.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// The waveform `kind` at phase `phi` (radians), in [−1, 1].
pub fn wave(kind: usize, phi: f64) -> f64 {
    let x = (phi / TAU).rem_euclid(1.0);
    // A half disc over [0, 1): 0 at the ends, 1 in the middle.
    let disc = |t: f64| (1.0 - (2.0 * t - 1.0).powi(2)).max(0.0).sqrt();
    match kind {
        0 => phi.sin(),
        1 => 2.0 / PI * phi.sin().asin(),
        2 => {
            if x < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        3 => 2.0 * x - 1.0,
        // Circle: an upper half disc, then a lower one.
        4 => {
            if x < 0.5 {
                disc(2.0 * x)
            } else {
                -disc(2.0 * x - 1.0)
            }
        }
        // Semi-Circle: upper half discs end to end (0 … 1 → −1 … 1).
        5 => 2.0 * disc(x) - 1.0,
        // Uncircle: what a half disc leaves of its square.
        6 => 1.0 - 2.0 * disc(x),
        // Half-Sine: the positive half of a sine, flat between.
        _ => phi.sin().max(0.0),
    }
}

/// One note's ripple (Control Sweeps vary it per note).
#[derive(Clone, Copy, Debug)]
struct Rip {
    centre: (f64, f64),
    radius: f64,
    inside: f64,
    height: f64,
    perp: f64,
    width: f64,
    speed: f64,
    phase: f64,
    falloff: f64,
    light: f64,
}

#[derive(Debug)]
struct Ripple {
    g: Geom,
    tracks: Vec<SourceTrack>,
    base: Rip,
    wave: usize,
    twirl: f64,
    intensity: f64,
    /// Stretch factor and its axis (cos, sin).
    stretch: (f64, f64, f64),
    chaos: f64,
    seed: u64,
    /// Source offsets from the centre (unit box).
    sources: Vec<(f64, f64)>,
    combine: usize,
    ctx: Ctx,
    growth: Growth,
}

impl Ripple {
    /// The stretched distance of an offset.
    #[inline]
    fn dist(&self, dx: f64, dy: f64) -> f64 {
        let (k, c, s) = self.stretch;
        let a = dx * c + dy * s;
        let b = -dx * s + dy * c;
        (a / k).hypot(b)
    }

    /// The waveform value at distance `d` and time `u` for source `i`.
    #[inline]
    fn w(&self, r: &Rip, i: usize, d: f64, u: f64) -> (f64, f64) {
        let phi = TAU * (d / r.width - r.speed * u) - r.phase;
        let mut w = wave(self.wave, phi);
        if self.chaos > 0.0 {
            let ring = (phi / TAU).floor() as i64 as u64;
            let mut g = stream(self.seed, salt_of("ripple.chaos"), &[i as u64, ring]);
            w = (1.0 - self.chaos) * w + self.chaos * (2.0 * g.unit() - 1.0);
        }
        (w, phi)
    }

    #[inline]
    fn envelope(r: &Rip, d: f64) -> f64 {
        if d < r.inside {
            return 0.0;
        }
        if d <= r.radius {
            return 1.0;
        }
        if r.radius <= 0.0 {
            return 0.0;
        }
        (1.0 - (d - r.radius) / r.radius)
            .max(0.0)
            .powf(1.0 + r.falloff / 25.0)
    }

    /// The displacement (unit box) at a point.
    fn displace(&self, r: &Rip, u: f64, v: f64) -> (f64, f64) {
        let mut sum = (0.0, 0.0);
        let mut best = (0.0, 0.0, -1.0);
        for (i, off) in self.sources.iter().enumerate() {
            let (cx, cy) = (r.centre.0 + off.0, r.centre.1 + off.1);
            let (dx, dy) = (u - cx, v - cy);
            let len = dx.hypot(dy);
            let d = self.dist(dx, dy);
            let a = Self::envelope(r, d) * self.intensity;
            if a == 0.0 || len == 0.0 {
                continue;
            }
            let (w, _) = self.w(r, i, d, u);
            let (nx, ny) = (dx / len, dy / len);
            let along = r.height * w;
            let twist = if r.radius > 0.0 {
                1.0 + self.twirl * d / r.radius
            } else {
                1.0
            };
            let across = r.perp * w * twist;
            let (x, y) = (
                a * (along * nx - across * ny),
                a * (along * ny + across * nx),
            );
            sum = (sum.0 + x, sum.1 + y);
            let m = x.hypot(y);
            if m > best.2 {
                best = (x, y, m);
            }
        }
        match self.combine {
            1 => (best.0, best.1),
            2 => (
                sum.0 / self.sources.len() as f64,
                sum.1 / self.sources.len() as f64,
            ),
            _ => sum,
        }
    }

    /// The wave's slope (−1 … 1) at a point: the nearest covering source's.
    fn slope(&self, r: &Rip, u: f64, v: f64) -> f64 {
        let mut best: Option<(f64, f64)> = None;
        for (i, off) in self.sources.iter().enumerate() {
            let (dx, dy) = (u - r.centre.0 - off.0, v - r.centre.1 - off.1);
            let d = self.dist(dx, dy);
            if Self::envelope(r, d) == 0.0 {
                continue;
            }
            let e = 1e-3;
            let (a, _) = self.w(r, i, d - e * r.width, u);
            let (b, _) = self.w(r, i, d + e * r.width, u);
            let s = ((b - a) / (2.0 * e * TAU)).clamp(-1.0, 1.0);
            if best.is_none_or(|x| d < x.0) {
                best = Some((d, s));
            }
        }
        best.map_or(0.0, |b| b.1)
    }

    fn rips(&self, src: &Source) -> Vec<Rip> {
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
                let pct = |k: &str, v: f64| f(k, v * 100.0) / 100.0;
                Rip {
                    centre: (pct("cx", b.centre.0), pct("cy", b.centre.1)),
                    radius: pct("radius", b.radius).max(0.0),
                    inside: pct("inside", b.inside).max(0.0),
                    height: pct("height", b.height),
                    perp: pct("perp", b.perp),
                    width: pct("width", b.width).max(0.005),
                    speed: f("speed", b.speed),
                    phase: f("phase", b.phase.to_degrees()).to_radians(),
                    falloff: f("falloff", b.falloff).clamp(0.0, 100.0),
                    light: pct("light", b.light).clamp(-1.0, 1.0),
                }
            })
            .collect()
    }
}

impl Kernel for Ripple {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let rips = self.rips(src);
        for (n, r) in src.notes.iter().zip(&rips) {
            let mut t = *n;
            if r.light != 0.0 {
                let mid = (n.start as f64 + n.end() as f64) / 2.0;
                let (u, v) = self.g.bbox.to_unit(mid, f64::from(n.pitch) + 0.5);
                t.vel = vel_scaled(n, (1.0 + r.light * self.slope(r, u, v)).max(0.0));
            }
            let ok = self.g.rasterize(
                &t,
                &|u, v| {
                    let (dx, dy) = self.displace(r, u, v);
                    (u + dx, v + dy)
                },
                &self.tracks,
                out,
            );
            if !ok {
                return out.check();
            }
        }
        out.check()
    }
}

fn ripple_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let pct = |k: &str| p.float(k) / 100.0;
    let n = p.int("sources").clamp(1, MAX_SOURCES) as usize;
    let spread = pct("sources.spread");
    let rot = p.float("sources.angle").to_radians();
    // The centre, then the others evenly on a ring around it (0° up).
    let sources = (0..n)
        .map(|i| {
            if i == 0 {
                (0.0, 0.0)
            } else {
                let a = rot + TAU * (i - 1) as f64 / (n - 1) as f64;
                (spread * a.sin(), spread * a.cos())
            }
        })
        .collect();
    let ang = p.float("stretch.angle").to_radians();
    let growth = match g.per_note() {
        0 | 1 => Growth::Map,
        k => Growth::Expand(k),
    };
    boxed(Ripple {
        g,
        tracks: b.tracks.to_vec(),
        base: Rip {
            centre: (pct("cx"), pct("cy")),
            radius: pct("radius").max(0.0),
            inside: pct("inside").max(0.0),
            height: pct("height"),
            perp: pct("perp"),
            width: pct("width").max(0.005),
            speed: p.float("speed"),
            phase: p.float("phase").to_radians(),
            falloff: p.float("falloff").clamp(0.0, 100.0),
            light: pct("light").clamp(-1.0, 1.0),
        },
        wave: p.choice("wave").min(WAVES.len() - 1),
        twirl: [0.0, 1.0, 3.0, 8.0][p.choice("twirl").min(3)],
        intensity: [1.0, 0.1, 10.0][p.choice("intensity").min(2)],
        stretch: (pct("stretch").max(0.1), ang.cos(), ang.sin()),
        chaos: pct("chaos").clamp(0.0, 1.0),
        seed: p.seed("seed"),
        sources,
        combine: p.choice("sources.mode").min(COMBINE.len() - 1),
        ctx: b.ctx.clone(),
        growth,
    })
}
