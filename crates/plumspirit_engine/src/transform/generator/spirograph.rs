//! Harmonic / Spirograph (§160, `gen.spirograph`; `docs/GENERATORS.md`
//! "Harmonic / Spirograph"): the fourth Advanced generator — curves traced
//! by a pen on a circle rolling along a line or around a fixed circle,
//! Lissajous figures and sums of harmonics, drawn by the shared path pen
//! (`generator/path.rs`, as Lines, Arcs, Parametric and the L-system).
//! The base: the **fixed circle unrolled along time** — a circle of radius
//! r = 3 rolling along the time axis with its pen at d = 5 from its centre
//! (a prolate cycloid: loops), for as many passes of the unrolled fixed
//! circle (R = 5) as close the curve (3: five turns), stretched over the
//! box.
//!
//! A curve is the rolling centre plus the **pen's offset** (in Lissajous /
//! Fourier mode the curve is the offset alone). The offset is a sum of
//! harmonics of the pen's angle: Σ aₖ · (k · f · θ + phase + ψₖ) per axis,
//! with frequency multipliers f (c10 / c11), phases (c07, c12, c13) and the
//! harmonics' amplitudes and phases (c15, c16) — without them one harmonic
//! of amplitude 1. In pipeline order:
//!
//! 1. **Mode** — the base (line), c01 epitrochoid (rolling outside the fixed
//!    circle), c02 hypotrochoid (inside), c09 Lissajous, c14 a Fourier sum
//!    of circles (epicycles); precedence c14 > c09 > c01 > c02 > base.
//! 2. **Sizes** — c03 R, c04 r, c05 d, c06 passes (else as many as close
//!    the curve, at most 64), c07 the pen's phase, c08 rotation.
//! 3. **Shaping** — c17 radial / c18 angular modulation of the offset along
//!    the curve, c23 several orbits (pen distance and phase stepped), c25 a
//!    morph towards a named preset, c24 sector copies; then the whole
//!    stretched over the box.
//! 4. **Drawing** — strokes along each piece (c22: one piece per layer — the
//!    pen circle's turn, or the orbit — each its own subchannel); c19 notes
//!    kept by curvature, c20 velocity by the distance from the box's
//!    centre, c21 gate by the pen's phase.

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::{Curve, Kernel, TransformError};
use super::draw::{Aff, BoxG, Gb, Pt, flatten_t, positions};
use super::path::{
    PIECE_CAP, PathStyle, Scratch, VISIT_CAP, by_curvature, curvature, curve_along, measure,
    too_many_pieces, too_many_visits,
};
use super::pen::{Brush, Draw};
use super::{Frame, Shapes};
use crate::midi::group_digits;
use std::f64::consts::TAU;
use std::sync::OnceLock;

pub const SPIROGRAPH: ToolDef = ToolDef {
    id: "gen.spirograph",
    spec: spirograph_spec,
    build: spirograph_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_epitrochoid_mode", "Epitrochoid Mode", 10),
    ("c02_hypotrochoid_mode", "Hypotrochoid Mode", 20),
    ("c03_spirograph_r", "Spirograph R", 30),
    ("c04_spirograph_r", "Spirograph r", 40),
    ("c05_pen_distance_d", "Pen Distance d", 50),
    ("c06_revolutions", "Revolutions", 60),
    ("c07_phase", "Phase", 70),
    ("c08_rotation", "Rotation", 80),
    ("c09_lissajous_mode", "Lissajous Mode", 90),
    ("c10_x_frequency", "X Frequency", 100),
    ("c11_y_frequency", "Y Frequency", 110),
    ("c12_x_phase", "X Phase", 120),
    ("c13_y_phase", "Y Phase", 130),
    ("c14_fourier_harmonic_sum", "Fourier Harmonic Sum", 140),
    ("c15_harmonic_amplitudes", "Harmonic Amplitudes", 150),
    ("c16_harmonic_phases", "Harmonic Phases", 160),
    ("c17_radial_modulation", "Radial Modulation", 170),
    ("c18_angular_modulation", "Angular Modulation", 180),
    ("c19_density_by_curvature", "Density by Curvature", 190),
    ("c20_velocity_by_radius", "Velocity by Radius", 200),
    ("c21_gate_by_harmonic_phase", "Gate by Harmonic Phase", 210),
    (
        "c22_subchannel_by_harmonic_layer",
        "Subchannel by Harmonic Layer",
        220,
    ),
    (
        "c23_multi_orbit_superposition",
        "Multi-Orbit Superposition",
        230,
    ),
    (
        "c24_symmetry_sector_replication",
        "Symmetry / Sector Replication",
        240,
    ),
    ("c25_harmonic_preset_morph", "Harmonic Preset Morph", 250),
];

/// The base: fixed circle R, rolling circle r, pen distance d.
pub const BASE_R: f64 = 5.0;
pub const BASE_SMALL_R: f64 = 3.0;
pub const BASE_D: f64 = 5.0;
/// Most passes (around the fixed circle, or of its unrolled length).
pub const MAX_PASSES: u32 = 64;
/// Most turns of the pen's circle in one curve.
pub const TURN_CAP: f64 = 4096.0;
/// Most flattened pieces (quarter turns) over all curves drawn.
pub const CHUNK_CAP: f64 = 65_536.0;
/// Most points over all curves drawn.
pub const POINT_CAP: usize = 1 << 21;
/// Raw samples per quarter turn for the drawing's extent.
const PROBE: usize = 64;
/// The presets c25 starts with: `Name: mode R r d`.
pub const LIBRARY: &str = "Star: hypo 5 3 5; Rose: epi 4 1 2; Loops: line 5 2 3; Knot: lissajous 3 4 0; Flower: hypo 8 5 3";

const MIRRORS: &[&str] = &["Turned copies only", "Every other copy mirrored"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SPIROGRAPH_HELP: &[(&str, &str)] = &[
    (
        "c01_epitrochoid_mode",
        "Rolls the circle around the outside of a fixed circle, like a classic spirograph's outer ring.",
    ),
    (
        "c02_hypotrochoid_mode",
        "Rolls the circle around the inside of a fixed circle, like a classic spirograph gear in a ring.",
    ),
    ("c03_spirograph_r", "Sets the fixed circle's radius."),
    ("c04_spirograph_r", "Sets the rolling circle's radius."),
    (
        "c04_spirograph_r.r",
        "The radius of the circle that rolls; its ratio to the fixed circle decides how many loops the curve has.",
    ),
    (
        "c05_pen_distance_d",
        "Sets how far the pen is from the rolling circle's center.",
    ),
    (
        "c06_revolutions",
        "Sets how many times the circle rolls around instead of until the curve closes.",
    ),
    (
        "c07_phase",
        "Starts the pen at another angle on the rolling circle.",
    ),
    (
        "c07_phase.angle",
        "The pen's starting angle on the rolling circle, in degrees.",
    ),
    ("c08_rotation", "Turns the whole figure."),
    (
        "c09_lissajous_mode",
        "Draws a Lissajous figure: time and pitch swing back and forth at different frequencies.",
    ),
    ("c10_x_frequency", "Sets how fast the pen swings in time."),
    ("c11_y_frequency", "Sets how fast the pen swings in pitch."),
    (
        "c11_y_frequency.f",
        "The pen's frequency in pitch (Lissajous: b; elsewhere a multiple of the pen's angle).",
    ),
    ("c12_x_phase", "Shifts the pen's swing in time."),
    (
        "c12_x_phase.angle",
        "How far the swing in time is shifted, in degrees.",
    ),
    ("c13_y_phase", "Shifts the pen's swing in pitch."),
    (
        "c13_y_phase.angle",
        "How far the swing in pitch is shifted, in degrees.",
    ),
    (
        "c14_fourier_harmonic_sum",
        "Draws the curve as circles turning on circles (epicycles), one per listed frequency.",
    ),
    (
        "c15_harmonic_amplitudes",
        "Sets the size of each harmonic (or epicycle) yourself.",
    ),
    (
        "c16_harmonic_phases",
        "Sets the starting angle of each harmonic (or epicycle) yourself.",
    ),
    (
        "c16_harmonic_phases.phases",
        "The starting angle of each harmonic in turn, in degrees.",
    ),
    (
        "c17_radial_modulation",
        "Makes the pen's distance swell and shrink along the curve.",
    ),
    (
        "c17_radial_modulation.depth",
        "How much the pen's distance swells and shrinks.",
    ),
    (
        "c18_angular_modulation",
        "Makes the pen swing back and forth around its path along the curve.",
    ),
    (
        "c18_angular_modulation.depth",
        "How far the pen swings around, in degrees.",
    ),
    (
        "c19_density_by_curvature",
        "Places separate notes along the curve, closer together where it bends.",
    ),
    (
        "c19_density_by_curvature.strength",
        "How strongly the bends decide where the notes go (0 % = evenly).",
    ),
    (
        "c20_velocity_by_radius",
        "Changes the velocity by the distance from the box's center.",
    ),
    (
        "c20_velocity_by_radius.inner",
        "The velocity of the points nearest the center.",
    ),
    (
        "c21_gate_by_harmonic_phase",
        "Changes the note lengths with the pen's angle as it turns.",
    ),
    (
        "c22_subchannel_by_harmonic_layer",
        "Gives each layer of the curve (each turn of the pen circle, or each orbit) its own subchannel.",
    ),
    (
        "c22_subchannel_by_harmonic_layer.layers",
        "How many subchannels the layers cycle through.",
    ),
    (
        "c23_multi_orbit_superposition",
        "Draws several orbits on top of each other, each with the pen closer in and turned.",
    ),
    (
        "c23_multi_orbit_superposition.count",
        "How many orbits are drawn.",
    ),
    (
        "c23_multi_orbit_superposition.factor",
        "Each orbit's pen distance as a share of the previous orbit's.",
    ),
    (
        "c23_multi_orbit_superposition.phase",
        "How much each orbit is turned from the previous one, in degrees.",
    ),
    (
        "c24_symmetry_sector_replication",
        "Adds copies of the figure turned evenly around its center.",
    ),
    (
        "c24_symmetry_sector_replication.sectors",
        "How many copies go around the center, the figure included.",
    ),
    (
        "c25_harmonic_preset_morph",
        "Changes the curve gradually toward a named preset curve.",
    ),
    (
        "c25_harmonic_preset_morph.name",
        "The preset curve the figure changes toward.",
    ),
    (
        "c25_harmonic_preset_morph.amount",
        "How far the figure changes toward the preset curve.",
    ),
];

fn spirograph_spec() -> ToolSpec {
    let mut own = vec![
        flag("c01_epitrochoid_mode", "Epitrochoid Mode", false),
        flag("c02_hypotrochoid_mode", "Hypotrochoid Mode", false),
        flag("c03_spirograph_r", "Spirograph R", false),
        float("c03_spirograph_r.r", "Fixed circle R", 0.1, 100.0, 7.0, "")
            .help("The fixed circle's radius (the base unrolls it along time)."),
        flag("c04_spirograph_r", "Spirograph r", false),
        float("c04_spirograph_r.r", "Rolling circle r", 0.1, 100.0, 2.0, ""),
        flag("c05_pen_distance_d", "Pen Distance d", false),
        float("c05_pen_distance_d.d", "Pen distance d", 0.0, 200.0, 1.0, "")
            .help("From the rolling circle's center: more than r makes loops, less a wave."),
        flag("c06_revolutions", "Revolutions", false),
        float("c06_revolutions.n", "Passes", 0.05, f64::from(MAX_PASSES), 1.5, "")
            .help("Passes around the fixed circle (or of its unrolled length); without this function, as many as close the curve."),
        flag("c07_phase", "Phase", false),
        float("c07_phase.angle", "Pen phase", -360.0, 360.0, 90.0, "°"),
        flag("c08_rotation", "Rotation", false),
        float("c08_rotation.angle", "Rotation", -180.0, 180.0, 30.0, "°")
            .help("The figure turned before it is stretched over the box."),
        flag("c09_lissajous_mode", "Lissajous Mode", false),
        flag("c10_x_frequency", "X Frequency", false),
        float("c10_x_frequency.f", "X frequency", 0.05, 64.0, 5.0, "")
            .help("The pen's x frequency (Lissajous: a, 3 without this function; elsewhere a multiple of the pen's angle, 1 without it)."),
        flag("c11_y_frequency", "Y Frequency", false),
        float("c11_y_frequency.f", "Y frequency", 0.05, 64.0, 4.0, ""),
        flag("c12_x_phase", "X Phase", false),
        float("c12_x_phase.angle", "X phase", -360.0, 360.0, 45.0, "°"),
        flag("c13_y_phase", "Y Phase", false),
        float("c13_y_phase.angle", "Y phase", -360.0, 360.0, 30.0, "°"),
        flag("c14_fourier_harmonic_sum", "Fourier Harmonic Sum", false),
        list("c14_fourier_harmonic_sum.freqs", "Frequencies", -64, 64, 16, &[1, -3, 5])
            .help("Circles turning these many times per pass (negative: the other way), radii 1 / |frequency| unless Harmonic Amplitudes."),
        flag("c15_harmonic_amplitudes", "Harmonic Amplitudes", false),
        list("c15_harmonic_amplitudes.amps", "Amplitudes (%)", -400, 400, 16, &[100, 40, 20])
            .help("Harmonic k's amplitude (the pen's offset as a sum of harmonics 1, 2, …; in Fourier mode the circles')."),
        flag("c16_harmonic_phases", "Harmonic Phases", false),
        list("c16_harmonic_phases.phases", "Phases (°)", -360, 360, 16, &[30, 90, 45]),
        flag("c17_radial_modulation", "Radial Modulation", false),
        float("c17_radial_modulation.depth", "Depth", 0.0, 100.0, 30.0, "%"),
        int("c17_radial_modulation.count", "Cycles along the curve", 1, 256, 6)
            .help("The pen's offset scaled by 1 + depth · sin(2π · cycles · position along the curve)."),
        flag("c18_angular_modulation", "Angular Modulation", false),
        float("c18_angular_modulation.depth", "Depth", 0.0, 180.0, 20.0, "°"),
        int("c18_angular_modulation.count", "Cycles along the curve", 1, 256, 6)
            .help("The pen's offset turned by depth · sin(2π · cycles · position along the curve)."),
        flag("c19_density_by_curvature", "Density by Curvature", false),
        float("c19_density_by_curvature.strength", "Strength", 0.0, 200.0, 100.0, "%"),
        float("c19_density_by_curvature.floor", "Least share", 0.0, 100.0, 10.0, "%")
            .help("A note every grid unit, kept by least + (1 − least) · (curvature / the most) ^ strength."),
        flag("c20_velocity_by_radius", "Velocity by Radius", false),
        int("c20_velocity_by_radius.inner", "Velocity at the center", 1, 127, 40),
        int("c20_velocity_by_radius.outer", "Velocity at the edge", 1, 127, 127)
            .help("By the distance from the box's center (box shares), the nearest point to the farthest."),
        flag("c21_gate_by_harmonic_phase", "Gate by Harmonic Phase", false),
        float("c21_gate_by_harmonic_phase.least", "Least gate", 1.0, 100.0, 25.0, "%")
            .help("100 % where the pen's angle is 0, the least at half a turn: least + (100 % − least) · (1 + cos angle) / 2."),
        flag("c22_subchannel_by_harmonic_layer", "Subchannel by Harmonic Layer", false),
        int("c22_subchannel_by_harmonic_layer.layers", "Layers", 1, 16, 4),
        int("c22_subchannel_by_harmonic_layer.step", "Subchannel step per layer", 0, 64, 1)
            .help("A layer: the orbit (Multi-Orbit) or the pen circle's turn, cycled over the layers."),
        flag("c23_multi_orbit_superposition", "Multi-Orbit Superposition", false),
        int("c23_multi_orbit_superposition.count", "Orbits", 2, 8, 3),
        float("c23_multi_orbit_superposition.factor", "Pen distance per orbit", 10.0, 200.0, 70.0, "%"),
        float("c23_multi_orbit_superposition.phase", "Phase per orbit", -180.0, 180.0, 15.0, "°"),
        flag("c24_symmetry_sector_replication", "Symmetry / Sector Replication", false),
        int("c24_symmetry_sector_replication.sectors", "Sectors", 2, 12, 4),
        choice("c24_symmetry_sector_replication.mirror", "Copies", MIRRORS, 0)
            .help("Copies turned evenly about the figure's center."),
        flag("c25_harmonic_preset_morph", "Harmonic Preset Morph", false),
        text("c25_harmonic_preset_morph.library", "Presets", 4096, LIBRARY)
            .help("Named curves: `Name: mode R r d` (mode line, epi, hypo or lissajous — a, b for R, r), `;` between them; saved with the tool's presets."),
        text("c25_harmonic_preset_morph.name", "Morph towards", 64, "Rose"),
        float("c25_harmonic_preset_morph.amount", "Morph", 0.0, 100.0, 50.0, "%"),
        int("c25_harmonic_preset_morph.steps", "Curves drawn", 1, 16, 1)
            .help("Each point moved this share of the way to the preset's point at the same position along the curve (both stretched to one square first). More curves: evenly from 0 %."),
    ];
    with_help(&mut own, SPIROGRAPH_HELP);
    super::spec(
        "gen.spirograph",
        "Harmonic / Spirograph",
        "Generates notes along a spirograph curve: a circle rolling along time with its pen outside it to start from, stretched over the generation box (where, length and pitch range above).",
        own,
        FUNCTIONS,
    )
}

/// How the curve is traced.
#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Line,
    Epi,
    Hypo,
    Lissajous,
    /// Circles: frequency, amplitude, phase (radians).
    Fourier(Vec<(f64, f64, f64)>),
}

/// One curve's settings.
#[derive(Clone, Debug)]
struct Spec {
    mode: Mode,
    big: f64,
    small: f64,
    d: f64,
    passes: f64,
    /// Pen phase (radians) and per-axis frequency multipliers and phases.
    phase: f64,
    fx: f64,
    fy: f64,
    phx: f64,
    phy: f64,
    /// Harmonics of the pen's offset: amplitude, phase.
    harm: Vec<(f64, f64)>,
    radial: Option<(f64, f64)>,
    angular: Option<(f64, f64)>,
}

/// A rational's denominator for `x` (to 1 / 100).
fn den(x: f64) -> u64 {
    let n = (x.abs() * 100.0).round() as u64;
    if n == 0 {
        return 1;
    }
    100 / gcd(n, 100)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

impl Spec {
    /// Passes that close the curve (≤ [`MAX_PASSES`]).
    fn closing(&self) -> f64 {
        let p = match &self.mode {
            Mode::Line | Mode::Epi | Mode::Hypo => {
                let (a, b) = (
                    (self.big * 100.0).round() as u64,
                    (self.small * 100.0).round() as u64,
                );
                if a == 0 || b == 0 { 1 } else { b / gcd(a, b) }
            }
            Mode::Lissajous => {
                let (a, b) = (den(self.fx), den(self.fy));
                a / gcd(a, b) * b
            }
            Mode::Fourier(c) => c.iter().map(|x| den(x.0)).fold(1, |l, d| l / gcd(l, d) * d),
        };
        (p.clamp(1, u64::from(MAX_PASSES))) as f64
    }

    /// Turns of the pen's circle per pass.
    fn ratio(&self) -> f64 {
        match self.mode {
            Mode::Line => self.big / self.small,
            Mode::Epi => (self.big + self.small) / self.small,
            Mode::Hypo => (self.big - self.small) / self.small,
            _ => 1.0,
        }
    }

    /// Turns of the pen's angle over the whole curve.
    fn turns(&self) -> f64 {
        let h = self.harm.len().max(1) as f64;
        let f = self.fx.abs().max(self.fy.abs()).max(1.0);
        let k = match &self.mode {
            Mode::Lissajous => f * h,
            Mode::Fourier(c) => c.iter().map(|x| x.0.abs()).fold(1.0, f64::max),
            _ => self.ratio().abs().max(1.0) * f * h,
        };
        // Modulations along the curve need their own resolution.
        let m = self
            .radial
            .map_or(0.0, |r| r.1)
            .max(self.angular.map_or(0.0, |a| a.1));
        (self.passes * k).max(m)
    }

    /// The pen's angle at `u` (0 – 1 along the curve).
    fn theta(&self, u: f64) -> f64 {
        TAU * self.passes * u * self.ratio()
    }

    /// The offset's harmonic sums: Σ aₖ cos / sin (k f θ + φ + ψₖ) per axis.
    fn sums(&self, th: f64) -> (f64, f64, f64, f64) {
        let (mut cx, mut sx, mut cy, mut sy) = (0.0, 0.0, 0.0, 0.0);
        for (k, &(a, psi)) in self.harm.iter().enumerate() {
            let k = (k + 1) as f64;
            let ax = k * self.fx * th + self.phase + self.phx + psi;
            let ay = k * self.fy * th + self.phase + self.phy + psi;
            let (s1, c1) = ax.sin_cos();
            let (s2, c2) = ay.sin_cos();
            cx += a * c1;
            sx += a * s1;
            cy += a * c2;
            sy += a * s2;
        }
        (cx, sx, cy, sy)
    }

    /// The rolling centre and the pen's offset at `u`.
    fn parts(&self, u: f64) -> ((f64, f64), (f64, f64)) {
        let t = TAU * self.passes * u;
        let th = self.theta(u);
        let (r, d) = (self.small, self.d);
        match &self.mode {
            Mode::Line => {
                // About the middle of the rolled line (the figure's centre).
                let (_, sx, cy, _) = self.sums(th);
                ((r * (th - self.theta(0.5)), 0.0), (-d * sx, -d * cy))
            }
            Mode::Epi => {
                let (cx, _, _, sy) = self.sums(th);
                let c = (self.big + r) * 1.0;
                ((c * t.cos(), c * t.sin()), (-d * cx, -d * sy))
            }
            Mode::Hypo => {
                let (cx, _, _, sy) = self.sums(th);
                let c = self.big - r;
                ((c * t.cos(), c * t.sin()), (d * cx, -d * sy))
            }
            Mode::Lissajous => {
                let (_, sx, _, sy) = self.sums(t);
                ((0.0, 0.0), (sx, sy))
            }
            Mode::Fourier(circles) => {
                let (mut x, mut y) = (0.0, 0.0);
                for &(f, a, psi) in circles {
                    let ang = f * t + psi + self.phase;
                    x += a * ang.cos();
                    y += a * ang.sin();
                }
                ((0.0, 0.0), (x, y))
            }
        }
    }

    /// The point at `u` with the offset modulated.
    fn at(&self, u: f64) -> (f64, f64) {
        let (c, mut o) = self.parts(u);
        if let Some((m, k)) = self.radial {
            let s = 1.0 + m * (TAU * k * u).sin();
            o = (o.0 * s, o.1 * s);
        }
        if let Some((m, k)) = self.angular {
            let (sn, cs) = (m * (TAU * k * u).sin()).sin_cos();
            o = (o.0 * cs - o.1 * sn, o.0 * sn + o.1 * cs);
        }
        (c.0 + o.0, c.1 + o.1)
    }

    /// The pen's angle (for c21) at `u`.
    fn pen_angle(&self, u: f64) -> f64 {
        let th = match self.mode {
            Mode::Lissajous | Mode::Fourier(_) => TAU * self.passes * u,
            _ => self.theta(u),
        };
        self.fx * th + self.phase + self.phx
    }

    /// The layer (pen circle's turn) at `u`.
    fn turn_at(&self, u: f64) -> u64 {
        let th = match self.mode {
            Mode::Lissajous | Mode::Fourier(_) => TAU * self.passes * u,
            _ => self.theta(u),
        };
        (th / TAU).floor().max(0.0) as u64
    }
}

/// The library's presets: names and their curves (mode, R, r, d).
pub type Library = Vec<(String, Mode, f64, f64, f64)>;

/// Parses c25's library, or the sentence saying what is wrong.
pub fn parse_library(text: &str) -> Result<Library, String> {
    let mut out = Vec::new();
    for (k, entry) in text.split(';').enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((name, body)) = entry.split_once(':') else {
            return Err(format!(
                "Preset {} of the library has no name — write it as Name: mode R r d.",
                k + 1
            ));
        };
        let name = name.trim().to_string();
        let w: Vec<&str> = body.split_whitespace().collect();
        let mode = match w.first().map(|m| m.to_ascii_lowercase()).as_deref() {
            Some("line") => Mode::Line,
            Some("epi") => Mode::Epi,
            Some("hypo") => Mode::Hypo,
            Some("lissajous") => Mode::Lissajous,
            _ => {
                return Err(format!(
                    "The library's preset “{name}” needs a mode first: line, epi, hypo or lissajous."
                ));
            }
        };
        if w.len() != 4 {
            return Err(format!(
                "The library's preset “{name}” needs three numbers after its mode (R r d)."
            ));
        }
        let mut v = [0.0; 3];
        for (i, s) in w[1..].iter().enumerate() {
            v[i] = match s.parse::<f64>() {
                Ok(x) if x.is_finite() && (0.0..=200.0).contains(&x) => x,
                _ => {
                    return Err(format!(
                        "The library's preset “{name}” has “{s}” where a number from 0 to 200 belongs."
                    ));
                }
            };
        }
        if mode != Mode::Lissajous && (v[0] < 0.1 || v[1] < 0.1) {
            return Err(format!(
                "The library's preset “{name}” needs circles of radius 0.1 or more."
            ));
        }
        out.push((name, mode, v[0], v[1], v[2]));
    }
    Ok(out)
}

/// The functions' settings.
#[derive(Clone, Debug)]
struct Cfg {
    a: Spec,
    rotate: f64,
    /// c19: strength, least share.
    curv: Option<(f64, f64)>,
    /// c20: inner, outer velocity.
    vel: Option<(f64, f64)>,
    /// c21: least gate (share).
    gate: Option<f64>,
    /// c22: layers, step.
    layers: Option<(u64, u32)>,
    /// c23: orbits, pen distance factor, phase step (radians).
    orbits: Option<(u32, f64, f64)>,
    /// c24: sectors, mirrored?
    sectors: Option<(u32, bool)>,
    /// c25: the preset, amount, steps.
    morph: Option<(Spec, f64, u32)>,
}

impl Cfg {
    fn read(p: &Params) -> Result<Cfg, TransformError> {
        let on = |id: &str| p.flag(id);
        let pct = |k: &str| p.float(k) / 100.0;
        let deg = |k: &str| p.float(k).to_radians();
        let mut a = Spec {
            mode: Mode::Line,
            big: BASE_R,
            small: BASE_SMALL_R,
            d: BASE_D,
            passes: 1.0,
            phase: 0.0,
            fx: 1.0,
            fy: 1.0,
            phx: 0.0,
            phy: 0.0,
            harm: vec![(1.0, 0.0)],
            radial: None,
            angular: None,
        };
        if on("c14_fourier_harmonic_sum") {
            let freqs: Vec<i64> = p
                .list("c14_fourier_harmonic_sum.freqs")
                .into_iter()
                .filter(|&f| f != 0)
                .collect();
            if freqs.is_empty() {
                return Err(TransformError::Invalid(
                    "Fourier Harmonic Sum needs at least one frequency other than 0.".into(),
                ));
            }
            a.mode = Mode::Fourier(
                freqs
                    .iter()
                    .map(|&f| (f as f64, 1.0 / (f as f64).abs(), 0.0))
                    .collect(),
            );
        } else if on("c09_lissajous_mode") {
            a.mode = Mode::Lissajous;
            a.fx = 3.0;
            a.fy = 2.0;
            a.phx = TAU / 4.0;
        } else if on("c01_epitrochoid_mode") {
            a.mode = Mode::Epi;
        } else if on("c02_hypotrochoid_mode") {
            a.mode = Mode::Hypo;
        }
        if on("c03_spirograph_r") {
            a.big = p.float("c03_spirograph_r.r").clamp(0.1, 100.0);
        }
        if on("c04_spirograph_r") {
            a.small = p.float("c04_spirograph_r.r").clamp(0.1, 100.0);
        }
        if on("c05_pen_distance_d") {
            a.d = p.float("c05_pen_distance_d.d").clamp(0.0, 200.0);
        }
        if on("c07_phase") {
            a.phase = deg("c07_phase.angle");
        }
        if on("c10_x_frequency") {
            a.fx = p.float("c10_x_frequency.f").clamp(0.05, 64.0);
        }
        if on("c11_y_frequency") {
            a.fy = p.float("c11_y_frequency.f").clamp(0.05, 64.0);
        }
        if on("c12_x_phase") {
            a.phx = deg("c12_x_phase.angle");
        }
        if on("c13_y_phase") {
            a.phy = deg("c13_y_phase.angle");
        }
        let amps: Option<Vec<f64>> = on("c15_harmonic_amplitudes").then(|| {
            p.list("c15_harmonic_amplitudes.amps")
                .iter()
                .map(|&v| v.clamp(-400, 400) as f64 / 100.0)
                .collect()
        });
        let phases: Option<Vec<f64>> = on("c16_harmonic_phases").then(|| {
            p.list("c16_harmonic_phases.phases")
                .iter()
                .map(|&v| (v.clamp(-360, 360) as f64).to_radians())
                .collect()
        });
        match &mut a.mode {
            Mode::Fourier(circles) => {
                for (k, c) in circles.iter_mut().enumerate() {
                    if let Some(am) = &amps {
                        c.1 = am.get(k).copied().unwrap_or(0.0);
                    }
                    if let Some(ph) = &phases {
                        c.2 = ph.get(k).copied().unwrap_or(0.0);
                    }
                }
            }
            _ => {
                let n = amps
                    .as_ref()
                    .map_or(1, |v| v.len())
                    .max(phases.as_ref().map_or(1, |v| v.len()))
                    .clamp(1, 16);
                a.harm = (0..n)
                    .map(|k| {
                        (
                            amps.as_ref().map_or(if k == 0 { 1.0 } else { 0.0 }, |v| {
                                v.get(k).copied().unwrap_or(0.0)
                            }),
                            phases
                                .as_ref()
                                .map_or(0.0, |v| v.get(k).copied().unwrap_or(0.0)),
                        )
                    })
                    .collect();
            }
        }
        if on("c17_radial_modulation") {
            a.radial = Some((
                pct("c17_radial_modulation.depth").clamp(0.0, 1.0),
                p.int("c17_radial_modulation.count").clamp(1, 256) as f64,
            ));
        }
        if on("c18_angular_modulation") {
            a.angular = Some((
                deg("c18_angular_modulation.depth"),
                p.int("c18_angular_modulation.count").clamp(1, 256) as f64,
            ));
        }
        a.passes = if on("c06_revolutions") {
            p.float("c06_revolutions.n")
                .clamp(0.05, f64::from(MAX_PASSES))
        } else {
            a.closing()
        };
        let mut c = Cfg {
            a,
            rotate: 0.0,
            curv: None,
            vel: None,
            gate: None,
            layers: None,
            orbits: None,
            sectors: None,
            morph: None,
        };
        if on("c08_rotation") {
            c.rotate = deg("c08_rotation.angle");
        }
        if on("c19_density_by_curvature") {
            c.curv = Some((
                pct("c19_density_by_curvature.strength").clamp(0.0, 2.0),
                pct("c19_density_by_curvature.floor").clamp(0.0, 1.0),
            ));
        }
        if on("c20_velocity_by_radius") {
            c.vel = Some((
                p.int("c20_velocity_by_radius.inner").clamp(1, 127) as f64,
                p.int("c20_velocity_by_radius.outer").clamp(1, 127) as f64,
            ));
        }
        if on("c21_gate_by_harmonic_phase") {
            c.gate = Some(pct("c21_gate_by_harmonic_phase.least").clamp(0.01, 1.0));
        }
        if on("c22_subchannel_by_harmonic_layer") {
            c.layers = Some((
                p.int("c22_subchannel_by_harmonic_layer.layers")
                    .clamp(1, 16) as u64,
                p.int("c22_subchannel_by_harmonic_layer.step").clamp(0, 64) as u32,
            ));
        }
        if on("c23_multi_orbit_superposition") {
            let k = "c23_multi_orbit_superposition";
            c.orbits = Some((
                p.int(&format!("{k}.count")).clamp(2, 8) as u32,
                pct(&format!("{k}.factor")).clamp(0.1, 2.0),
                deg(&format!("{k}.phase")),
            ));
        }
        if on("c24_symmetry_sector_replication") {
            let k = "c24_symmetry_sector_replication";
            c.sectors = Some((
                p.int(&format!("{k}.sectors")).clamp(2, 12) as u32,
                p.choice(&format!("{k}.mirror")) == 1,
            ));
        }
        if on("c25_harmonic_preset_morph") {
            let k = "c25_harmonic_preset_morph";
            let lib =
                parse_library(&p.text(&format!("{k}.library"))).map_err(TransformError::Invalid)?;
            let want = p.text(&format!("{k}.name"));
            let want = want.trim();
            let Some((_, mode, r1, r2, d)) = lib.iter().find(|e| e.0.eq_ignore_ascii_case(want))
            else {
                let names: Vec<&str> = lib.iter().map(|e| e.0.as_str()).collect();
                return Err(TransformError::Invalid(if names.is_empty() {
                    "The preset library is empty — add one as Name: mode R r d.".into()
                } else {
                    format!(
                        "The preset library has no preset named “{want}” — it has {}.",
                        names.join(", ")
                    )
                }));
            };
            let mut b = Spec {
                mode: mode.clone(),
                big: *r1,
                small: *r2,
                d: *d,
                passes: 1.0,
                phase: 0.0,
                fx: 1.0,
                fy: 1.0,
                phx: 0.0,
                phy: 0.0,
                harm: vec![(1.0, 0.0)],
                radial: None,
                angular: None,
            };
            if b.mode == Mode::Lissajous {
                (b.fx, b.fy, b.phx) = (r1.max(0.05), r2.max(0.05), TAU / 4.0);
            }
            b.passes = b.closing();
            c.morph = Some((
                b,
                pct(&format!("{k}.amount")).clamp(0.0, 1.0),
                p.int(&format!("{k}.steps")).clamp(1, 16) as u32,
            ));
        }
        Ok(c)
    }

    /// The curves drawn: (orbit, morph share, sector).
    fn curves(&self) -> Vec<(u32, Option<f64>, u32)> {
        let orbits = self.orbits.map_or(1, |o| o.0);
        let shares: Vec<Option<f64>> = match &self.morph {
            None => vec![None],
            Some((_, amt, steps)) if *steps <= 1 => vec![Some(*amt)],
            Some((_, amt, steps)) => (0..*steps)
                .map(|k| Some(amt * f64::from(k) / f64::from(steps - 1)))
                .collect(),
        };
        let sectors = self.sectors.map_or(1, |s| s.0);
        let mut out = Vec::new();
        for o in 0..orbits {
            for &m in &shares {
                for s in 0..sectors {
                    out.push((o, m, s));
                }
            }
        }
        out
    }

    /// Orbit `o`'s curve.
    fn orbit(&self, o: u32) -> Spec {
        let mut s = self.a.clone();
        if let Some((_, f, ph)) = self.orbits {
            let k = f64::from(o);
            match &mut s.mode {
                Mode::Lissajous => {
                    s.harm.iter_mut().for_each(|h| h.0 *= f.powf(k));
                }
                Mode::Fourier(c) => c.iter_mut().for_each(|x| x.1 *= f.powf(k)),
                _ => s.d *= f.powf(k),
            }
            s.phase += ph * k;
        }
        s
    }
}

/// A curve normalised into the unit square by its extent (for the morph).
struct Norm {
    lo: (f64, f64),
    size: (f64, f64),
}

impl Norm {
    fn of(s: &Spec, chunks: usize) -> Norm {
        let n = chunks * PROBE;
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for i in 0..=n {
            let p = s.at(i as f64 / n as f64);
            lo = (lo.0.min(p.0), lo.1.min(p.1));
            hi = (hi.0.max(p.0), hi.1.max(p.1));
        }
        let size = (hi.0 - lo.0, hi.1 - lo.1);
        let side = size.0.max(size.1);
        Norm {
            lo: (lo.0 - (side - size.0) / 2.0, lo.1 - (side - size.1) / 2.0),
            size: (side.max(1e-12), side.max(1e-12)),
        }
    }

    fn at(&self, p: (f64, f64)) -> (f64, f64) {
        (
            (p.0 - self.lo.0) / self.size.0,
            (p.1 - self.lo.1) / self.size.1,
        )
    }
}

/// One curve drawn: its spec, its morph target, sector turn and mirror.
struct Drawn {
    a: Spec,
    b: Option<(Spec, Norm, Norm, f64)>,
    turn: (f64, f64),
    mirror: bool,
    orbit: u32,
    chunks: usize,
}

impl Drawn {
    /// The point at `u` before the box: morphed, turned (sector and c08).
    fn at(&self, u: f64, rot: (f64, f64)) -> (f64, f64) {
        let mut p = self.a.at(u);
        if let Some((b, na, nb, share)) = &self.b {
            let (pa, pb) = (na.at(p), nb.at(b.at(u)));
            p = (pa.0 + (pb.0 - pa.0) * share, pa.1 + (pb.1 - pa.1) * share);
        }
        if self.mirror {
            p.1 = -p.1;
        }
        let r = |p: (f64, f64), (s, c): (f64, f64)| (p.0 * c - p.1 * s, p.0 * s + p.1 * c);
        r(r(p, self.turn), rot)
    }
}

/// What is drawn: the pieces (grid units) with their styles and brushes.
#[derive(Debug, Default)]
struct Built {
    paths: Vec<(Vec<Pt>, PathStyle, Brush)>,
}

struct Spirograph {
    cfg: Cfg,
    built: OnceLock<Result<Built, TransformError>>,
}

impl Spirograph {
    fn new(cfg: Cfg) -> Spirograph {
        Spirograph {
            cfg,
            built: OnceLock::new(),
        }
    }

    /// The curves with their quarter-turn pieces, or a cap's sentence.
    fn drawn(&self) -> Result<Vec<Drawn>, TransformError> {
        let c = &self.cfg;
        let mut total = 0.0;
        let mut out = Vec::new();
        for (o, m, s) in c.curves() {
            let a = c.orbit(o);
            let turns = a.turns();
            if !turns.is_finite() || turns > TURN_CAP {
                return Err(TransformError::TooLarge(format!(
                    "The pen's circle would turn {} times in one curve; at most {} are allowed — use fewer passes, a larger r or lower frequencies.",
                    group_digits(turns.min(1e18) as u64),
                    group_digits(TURN_CAP as u64)
                )));
            }
            let chunks = ((turns * 4.0).ceil() as usize).max(4);
            total += chunks as f64;
            if total > CHUNK_CAP {
                return Err(TransformError::TooLarge(format!(
                    "The curves would take more than {} quarter turns to draw; use fewer orbits, sectors, morph curves or passes.",
                    group_digits(CHUNK_CAP as u64)
                )));
            }
            let b = match (&c.morph, m) {
                (Some((bs, _, _)), Some(share)) => {
                    let bc = ((bs.turns() * 4.0).ceil() as usize).clamp(4, 4096);
                    Some((bs.clone(), Norm::of(&a, chunks), Norm::of(bs, bc), share))
                }
                _ => None,
            };
            let (n, mirror) = c.sectors.unwrap_or((1, false));
            out.push(Drawn {
                a,
                b,
                turn: (TAU * f64::from(s) / f64::from(n)).sin_cos(),
                mirror: mirror && s % 2 == 1,
                orbit: o,
                chunks,
            });
        }
        Ok(out)
    }

    /// The pieces and styles, or a cap's sentence.
    fn build(&self, f: &Frame) -> Result<Built, TransformError> {
        let c = &self.cfg;
        let drawn = self.drawn()?;
        let rot = c.rotate.sin_cos();
        // The extent of every curve (raw samples per quarter turn).
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for d in &drawn {
            let n = d.chunks * PROBE;
            for i in 0..=n {
                let p = d.at(i as f64 / n as f64, rot);
                lo = (lo.0.min(p.0), lo.1.min(p.1));
                hi = (hi.0.max(p.0), hi.1.max(p.1));
            }
        }
        let (w, h) = (hi.0 - lo.0, hi.1 - lo.1);
        let gb = Gb::of(&BoxG::of(f), 0.0);
        let share = |p: (f64, f64)| {
            (
                if w > 0.0 {
                    ((p.0 - lo.0) / w).clamp(0.0, 1.0)
                } else {
                    0.5
                },
                if h > 0.0 {
                    ((p.1 - lo.1) / h).clamp(0.0, 1.0)
                } else {
                    0.5
                },
            )
        };
        // Pieces: (grid points, the u of each, orbit, layer, curve).
        type Piece = (Vec<Pt>, Vec<f64>, u32, u64, usize);
        let mut pieces: Vec<Piece> = Vec::new();
        let mut points = 0usize;
        for (di, d) in drawn.iter().enumerate() {
            let mut cur: (Vec<Pt>, Vec<f64>, u64) = (Vec::new(), Vec::new(), u64::MAX);
            for k in 0..d.chunks {
                let u0 = k as f64 / d.chunks as f64;
                let du = 1.0 / d.chunks as f64;
                let curve = |t: f64| gb.at(share(d.at(u0 + du * t, rot)));
                let (ts, ps) = flatten_t(&curve);
                let layer = match c.layers {
                    Some((n, _)) if c.orbits.is_some() => u64::from(d.orbit) % n,
                    Some((n, _)) => d.a.turn_at(u0 + du * 0.5) % n,
                    None => 0,
                };
                if layer != cur.2 && !cur.0.is_empty() {
                    let (p, u, l) = std::mem::take(&mut cur);
                    pieces.push((p, u, d.orbit, l, di));
                }
                if cur.0.is_empty() {
                    cur.2 = layer;
                }
                for (i, (&t, &(x, y))) in ts.iter().zip(&ps).enumerate() {
                    // Chunks share their ends.
                    if i == 0 && !cur.0.is_empty() {
                        continue;
                    }
                    cur.0.push(Pt { x, y, s: 0.0 });
                    cur.1.push(u0 + du * t);
                }
                points += ps.len();
                if points > POINT_CAP {
                    return Err(TransformError::TooLarge(format!(
                        "The curves would take more than {} points to draw; use fewer orbits, sectors, morph curves or passes.",
                        group_digits(POINT_CAP as u64)
                    )));
                }
            }
            if cur.0.len() >= 2 {
                let (p, u, l) = cur;
                pieces.push((p, u, d.orbit, l, di));
            }
        }
        // c20: the distance from the box's centre (box shares), its range.
        let rad = |q: &Pt| {
            let (fx, fy) = (
                (q.x - gb.u0) / (gb.u1 - gb.u0).max(1e-12),
                if gb.v1 > gb.v0 {
                    (q.y - gb.v0) / (gb.v1 - gb.v0)
                } else {
                    0.5
                },
            );
            (fx - 0.5).hypot(fy - 0.5)
        };
        let (mut rmin, mut rmax) = (f64::INFINITY, 0.0f64);
        if c.vel.is_some() {
            for q in pieces.iter().flat_map(|p| p.0.iter()) {
                let r = rad(q);
                rmin = rmin.min(r);
                rmax = rmax.max(r);
            }
        }
        let m = Aff {
            a: BoxG::of(f).step,
            ..Aff::ID
        };
        let (mut visits, mut cut) = (0.0, 0.0);
        let mut b = Built::default();
        for (mut pts, us, _, layer, di) in pieces {
            if pts.len() < 2 {
                continue;
            }
            positions(&mut pts);
            let d = &drawn[di];
            let mut st = PathStyle::default();
            if let Some((k, least)) = c.curv {
                let most = curvature(&pts).iter().copied().fold(0.0f64, f64::max);
                st.every = Some((1.0, 1.0));
                st.keep = Some(by_curvature(&pts, |x| {
                    let r = if most > 0.0 { (x / most).powf(k) } else { 1.0 };
                    least + (1.0 - least) * r
                }));
                let q: Vec<Pt> = pts.iter().map(|&x| m.ap(x)).collect();
                visits += st.visits(&measure(&q, &BoxG::of(f)));
                if visits > VISIT_CAP {
                    return Err(too_many_visits(visits));
                }
            }
            if c.vel.is_some() || c.gate.is_some() {
                let gate = |i: usize| match c.gate {
                    Some(least) => least + (1.0 - least) * (1.0 + d.a.pen_angle(us[i]).cos()) / 2.0,
                    None => 1.0,
                };
                let vel = |i: usize| match c.vel {
                    Some((v0, v1)) => {
                        let s = if rmax > rmin {
                            (rad(&pts[i]) - rmin) / (rmax - rmin)
                        } else {
                            0.0
                        };
                        (v0 + (v1 - v0) * s) / 127.0
                    }
                    None => 1.0,
                };
                if c.gate.is_some() {
                    st.grad.gate = Some((0.0, 1.0));
                }
                if c.vel.is_some() {
                    st.grad.vel = Some((0.0, 127.0));
                }
                st.shape = Some(Box::new([
                    curve_along(&pts, gate),
                    curve_along(&pts, vel),
                    Curve::new(vec![(0.0, 0.0), (1.0, 0.0)]),
                ]));
            }
            if st.varies() {
                // V09-S60: the gate / velocity along the curve cuts it
                // into pieces (a gradient over a huge box: ~10⁹).
                let q: Vec<Pt> = pts.iter().map(|&x| m.ap(x)).collect();
                cut += st.pieces(&measure(&q, &BoxG::of(f)), false, f, &f.band());
                if cut > PIECE_CAP {
                    return Err(too_many_pieces(cut));
                }
            }
            let mut br = f.brush;
            if let Some((_, step)) = c.layers {
                br.sub = (u64::from(br.sub) + u64::from(step) * layer).min(4095) as u32;
            }
            b.paths.push((pts, st, br));
        }
        Ok(b)
    }

    fn built(&self, f: &Frame) -> Option<&Built> {
        self.built.get_or_init(|| self.build(f)).as_ref().ok()
    }
}

impl Shapes for Spirograph {
    fn draw(&self, f: &Frame, pen: &mut dyn Draw) -> bool {
        let Some(b) = self.built(f) else {
            return true;
        };
        let g = BoxG::of(f);
        let rows = f.band();
        let m = Aff {
            a: g.step,
            ..Aff::ID
        };
        let mut sc = Scratch::default();
        for (p, st, br) in &b.paths {
            let mut fl = f.clone();
            fl.brush = *br;
            let mut pts: Vec<Pt> = p.iter().map(|&q| m.ap(q)).collect();
            if !st.draw(&mut pts, false, &g, &fl, &rows, pen, &mut sc) {
                return false;
            }
        }
        true
    }

    fn bound(&self, f: &Frame) -> u64 {
        let Some(b) = self.built(f) else {
            return 0;
        };
        let g = BoxG::of(f);
        let rows = f.band();
        let m = Aff {
            a: g.step,
            ..Aff::ID
        };
        let mut total = 0.0;
        for (p, st, _) in &b.paths {
            let q: Vec<Pt> = p.iter().map(|&x| m.ap(x)).collect();
            total += st.bound(&measure(&q, &g), false, f, &rows) + 1.0;
        }
        if total.is_finite() && total < u64::MAX as f64 {
            total.ceil() as u64
        } else {
            u64::MAX
        }
    }

    fn floor(&self, f: &Frame) -> u64 {
        let Some(b) = self.built(f) else {
            return 0;
        };
        let g = BoxG::of(f);
        let rows = f.band();
        let m = Aff {
            a: g.step,
            ..Aff::ID
        };
        // Loops, orbits and sectors overlap (their notes merge): the most
        // any one piece is sure to draw.
        let mut most = 0.0f64;
        for (p, st, br) in &b.paths {
            let mut fl = f.clone();
            fl.brush = *br;
            most = most.max(st.lower(std::slice::from_ref(p), &m, false, &g, &rows, &fl));
        }
        if most.is_finite() && most < u64::MAX as f64 {
            most as u64
        } else {
            0
        }
    }
}

fn spirograph_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let g = Spirograph::new(Cfg::read(p)?);
    let built = g.build(&f)?;
    let _ = g.built.set(Ok(built));
    super::kernel(p, b, g)
}

#[cfg(test)]
mod tests {
    use super::super::super::param::{ParamSet, ParamValue};
    use super::super::super::rng::Rng;
    use super::super::super::{Ctx, SourceStats};
    use super::super::pen::{Counter, Pen};
    use super::*;

    #[test]
    fn the_default_library_parses_and_bad_entries_are_named() {
        let lib = parse_library(LIBRARY).unwrap();
        let names: Vec<&str> = lib.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(names, ["Star", "Rose", "Loops", "Knot", "Flower"]);
        for (bad, has) in [
            ("A hypo 5 3 5", "no name"),
            ("A: spiral 5 3 5", "mode"),
            ("A: hypo 5 3", "three numbers"),
            ("A: hypo 5 x 5", "“x”"),
            ("A: epi 0 3 5", "radius"),
        ] {
            let e = parse_library(bad).unwrap_err();
            assert!(e.contains(has) && e.ends_with('.'), "{bad}: {e}");
        }
    }

    #[test]
    fn curves_close_after_the_rational_passes() {
        let s = |mode: Mode, big: f64, small: f64| Spec {
            mode,
            big,
            small,
            d: 1.0,
            passes: 1.0,
            phase: 0.0,
            fx: 1.0,
            fy: 1.0,
            phx: 0.0,
            phy: 0.0,
            harm: vec![(1.0, 0.0)],
            radial: None,
            angular: None,
        };
        for (mode, big, small, want) in [
            (Mode::Hypo, 5.0, 3.0, 3.0),
            (Mode::Epi, 4.0, 1.0, 1.0),
            (Mode::Hypo, 8.0, 5.0, 5.0),
            (Mode::Epi, 2.5, 1.0, 2.0),
        ] {
            let mut c = s(mode, big, small);
            assert_eq!(c.closing(), want);
            c.passes = want;
            let (a, b) = (c.at(0.0), c.at(1.0));
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        }
    }

    /// The lower bound never passes the count and the cheap bound never
    /// falls below it, over seeded random settings in small boxes.
    #[test]
    fn the_lower_bound_never_passes_the_count_and_the_bound_never_falls_below_it() {
        let spec = spirograph_spec();
        let mut rng = Rng::new(0x5345_3630);
        for draw in 0..200 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 200)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            for (k, lo, hi) in [
                ("c03_spirograph_r.r", 0.5, 12.0),
                ("c04_spirograph_r.r", 0.5, 8.0),
                ("c05_pen_distance_d.d", 0.0, 10.0),
                ("c06_revolutions.n", 0.2, 4.0),
                ("c10_x_frequency.f", 0.5, 6.0),
                ("c11_y_frequency.f", 0.5, 6.0),
                ("c08_rotation.angle", -180.0, 180.0),
            ] {
                p.set(k, ParamValue::Float(rng.range_f64(lo, hi)));
            }
            p.set(
                "c24_symmetry_sector_replication.sectors",
                ParamValue::Int(rng.range_i64(2, 6)),
            );
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let Ok(cfg) = Cfg::read(&params) else {
                continue;
            };
            let g = Spirograph::new(cfg);
            if g.built.get_or_init(|| g.build(&f)).is_err() {
                continue;
            }
            let mut pen = Pen::new(Counter::default(), f.step, f.band());
            g.draw(&f, &mut pen);
            let n = pen.finish().notes;
            assert!(g.floor(&f) <= n, "draw {draw}: floor {} > {n}", g.floor(&f));
            assert!(g.bound(&f) >= n, "draw {draw}: bound {} < {n}", g.bound(&f));
        }
    }
}
