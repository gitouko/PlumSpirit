//! Circles (§154, `gen.circles`; `docs/GENERATORS.md` "Circles"): the
//! ellipse inscribed in the generation box, its outline one cell wide, and
//! the contract's 25 functions (catalog ids `c01_…` … `c25_…`, switch = id,
//! controls `<id>.<name>`), drawn by the shared region rasterizer
//! (`generator/region.rs`).
//!
//! Shapes are **polar** in grid space (u = ticks / step, v = rows — a circle
//! is round on the grid): a radius r(α) at each polar angle α about the
//! centre (an ellipse's, times the waves and the roughness), turned by the
//! rotation. A band of thickness d inside it is the curve moved in along
//! each ray by d / cos φ (φ between the ray and the normal; exact for a
//! circle), never past the centre — so every ring stays a simple
//! star-shaped polygon, scanned even–odd when it is not convex.
//!
//! The pipeline (per centre; c21 places several):
//!
//! 1. **Shape** — c01 Center, c02 Radius (a circle), c03 Ellipse X/Y
//!    Radius, c04 Ellipse Rotation, c19 Wave-Modulated Radius, c20 Seeded
//!    Radius Roughness.
//! 2. **Outline and rings** — c05 Ring Thickness, c23 the band's outer and
//!    inner half on their own subchannels, c07 Concentric Rings / c08 Ring
//!    Spacing Graph; c22 Tangent Copy Pattern and c25 Recursive Circle
//!    Packing add circles (outlines) around / inside it.
//! 3. **Interior** — c06 Filled Disc; c09 Radial Spokes, c10 Spiral Fill,
//!    c17 Sector Fill, c18 Chord-Line Fill drawn where they are (filled:
//!    where they are not).
//! 4. **Brushes and masks** — c11 / c12 Angular / Radial Density Graphs,
//!    c13 / c14 Velocity / Gate by Radius, c15 Subchannel by Angle, c16
//!    Dashed Arc Pattern, c24 Polar Wrap (only an angular range is drawn).
//!
//! c20 and c25 draw random values (`gen.seed`).

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::Rng;
use super::super::{Curve, Kernel, TransformError};
use super::draw::Grad;
use super::pen::Draw;
use super::region::{CopySpec, Density, GDir, P2, Poly, RingSpec, Role, Scene, Style};
use super::{Frame, MAX_LENGTH, Shapes};
use crate::midi::group_digits;
use std::f64::consts::{PI, TAU};
use std::sync::OnceLock;

pub const CIRCLES: ToolDef = ToolDef {
    id: "gen.circles",
    spec: circles_spec,
    build: circles_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_center", "Center", 10),
    ("c02_radius", "Radius", 20),
    ("c03_ellipse_x_y_radius", "Ellipse X/Y Radius", 30),
    ("c04_ellipse_rotation", "Ellipse Rotation", 40),
    ("c05_ring_thickness", "Ring Thickness", 50),
    ("c06_filled_disc", "Filled Disc", 60),
    ("c07_concentric_rings", "Concentric Rings", 70),
    ("c08_ring_spacing_graph", "Ring Spacing Graph", 80),
    ("c09_radial_spokes", "Radial Spokes", 90),
    ("c10_spiral_fill", "Spiral Fill", 100),
    ("c11_angular_density_graph", "Angular Density Graph", 110),
    ("c12_radial_density_graph", "Radial Density Graph", 120),
    ("c13_velocity_by_radius", "Velocity by Radius", 130),
    ("c14_gate_by_radius", "Gate by Radius", 140),
    ("c15_subchannel_by_angle", "Subchannel by Angle", 150),
    ("c16_dashed_arc_pattern", "Dashed Arc Pattern", 160),
    ("c17_sector_fill", "Sector Fill", 170),
    ("c18_chord_line_fill", "Chord-Line Fill", 180),
    ("c19_wave_modulated_radius", "Wave-Modulated Radius", 190),
    (
        "c20_seeded_radius_roughness",
        "Seeded Radius Roughness",
        200,
    ),
    ("c21_multiple_centers", "Multiple Centers", 210),
    ("c22_tangent_copy_pattern", "Tangent Copy Pattern", 220),
    (
        "c23_inner_outer_separate_subchannels",
        "Inner/Outer Separate Subchannels",
        230,
    ),
    ("c24_polar_wrap", "Polar Wrap", 240),
    (
        "c25_recursive_circle_packing",
        "Recursive Circle Packing",
        250,
    ),
];

const LAYOUTS: &[&str] = &[
    "In a row across the box",
    "On a ring around the center",
    "In a grid over the box",
];
const SPIN: &[&str] = &["Counter-clockwise", "Clockwise"];

/// Most rings a scene may have (every centre's outline, nested rings,
/// sectors, masks and copies).
pub const RING_CAP: usize = 4096;
/// Most line segments (spokes, spiral, chords over every centre).
pub const LINE_CAP: usize = 65_536;
/// Most circles Recursive Circle Packing places.
pub const PACK_CAP: u64 = 512;
/// The longest chord between a polygon's points and its curve (grid
/// units) — the tessellation's tolerance.
const SAG: f64 = 0.05;
/// Most points of one ring.
const MAX_POINTS: usize = 1024;
/// Most points of every ring together.
pub const POINT_CAP: u64 = 1 << 20;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CIRCLES_HELP: &[(&str, &str)] = &[
    (
        "c01_center",
        "Puts the shape's center at an exact time and pitch instead of the middle of the box.",
    ),
    (
        "c01_center.time",
        "When the center is, in ticks from the box's start.",
    ),
    ("c01_center.pitch", "The pitch of the center."),
    (
        "c02_radius",
        "Draws a true circle of a set radius instead of the ellipse that fills the box.",
    ),
    (
        "c03_ellipse_x_y_radius",
        "Sets the ellipse's two radii, across and up, instead of filling the box.",
    ),
    (
        "c03_ellipse_x_y_radius.x",
        "How far the ellipse reaches across in time, in resolution steps.",
    ),
    (
        "c03_ellipse_x_y_radius.y",
        "How far the ellipse reaches up and down, in rows.",
    ),
    (
        "c04_ellipse_rotation",
        "Turns the ellipse about its center.",
    ),
    (
        "c05_ring_thickness",
        "Makes the outline thicker, growing inward from the edge.",
    ),
    (
        "c05_ring_thickness.size",
        "How thick the outline is, in grid units.",
    ),
    (
        "c06_filled_disc",
        "Fills the whole inside of the shape with notes instead of drawing only its outline.",
    ),
    (
        "c07_concentric_rings",
        "Adds rings inside the outline, like the rings of a target.",
    ),
    (
        "c08_ring_spacing_graph",
        "Places the inner rings by a curve instead of evenly.",
    ),
    (
        "c08_ring_spacing_graph.count",
        "How many inner rings are drawn when Concentric Rings is off.",
    ),
    (
        "c09_radial_spokes",
        "Draws spokes from the center out to the outline, like a wheel.",
    ),
    (
        "c09_radial_spokes.count",
        "How many spokes are drawn, evenly around.",
    ),
    (
        "c09_radial_spokes.angle",
        "The angle of the first spoke, in degrees (0 = pointing later in time).",
    ),
    (
        "c10_spiral_fill",
        "Draws a spiral from the center out to the outline.",
    ),
    (
        "c10_spiral_fill.turns",
        "How many times the spiral goes around.",
    ),
    (
        "c11_angular_density_graph",
        "Thins the notes around the shape by a curve, so some angles are fuller than others.",
    ),
    (
        "c12_radial_density_graph",
        "Thins the notes from the center out by a curve.",
    ),
    (
        "c12_radial_density_graph.curve",
        "The share of cells kept from the center (left) to the outline (right).",
    ),
    (
        "c13_velocity_by_radius",
        "Changes the velocity from the center to the outline.",
    ),
    (
        "c13_velocity_by_radius.centre",
        "The velocity of notes at the center.",
    ),
    (
        "c13_velocity_by_radius.edge",
        "The velocity of notes at the outline.",
    ),
    (
        "c14_gate_by_radius",
        "Changes the note lengths from the center to the outline.",
    ),
    (
        "c14_gate_by_radius.centre",
        "The gate (share of the cell) at the center.",
    ),
    (
        "c14_gate_by_radius.edge",
        "The gate (share of the cell) at the outline.",
    ),
    (
        "c15_subchannel_by_angle",
        "Steps through the track's subchannels (colors) once around the shape.",
    ),
    (
        "c15_subchannel_by_angle.from",
        "The subchannel at the angle 0, by its position in the track's list (0 = the first).",
    ),
    (
        "c15_subchannel_by_angle.to",
        "The subchannel reached once around, by its position in the track's list.",
    ),
    ("c16_dashed_arc_pattern", "Breaks the outline into dashes."),
    (
        "c16_dashed_arc_pattern.dash",
        "How many cells long each dash is.",
    ),
    (
        "c16_dashed_arc_pattern.gap",
        "How many cells long each gap is.",
    ),
    (
        "c16_dashed_arc_pattern.phase",
        "Shifts where the dashes start along the outline, in cells.",
    ),
    (
        "c17_sector_fill",
        "Fills a pie slice of the inside (or, with Filled Disc, leaves it empty).",
    ),
    (
        "c17_sector_fill.start",
        "The angle the slice starts at, in degrees.",
    ),
    (
        "c18_chord_line_fill",
        "Fills the inside with parallel lines (or, with Filled Disc, cuts them out).",
    ),
    (
        "c18_chord_line_fill.spacing",
        "How far apart the lines are, in grid units.",
    ),
    (
        "c19_wave_modulated_radius",
        "Makes the outline wavy, like a flower.",
    ),
    (
        "c19_wave_modulated_radius.waves",
        "How many waves go around the shape.",
    ),
    (
        "c19_wave_modulated_radius.amount",
        "How deep the waves are, as a share of the radius.",
    ),
    (
        "c19_wave_modulated_radius.phase",
        "Turns the waves around the shape, in degrees.",
    ),
    (
        "c20_seeded_radius_roughness",
        "Makes the outline bumpy at random (seeded).",
    ),
    (
        "c20_seeded_radius_roughness.amount",
        "How big the bumps are, as a share of the radius.",
    ),
    (
        "c21_multiple_centers",
        "Draws the shape several times, each in its own part of the box.",
    ),
    ("c21_multiple_centers.count", "How many shapes are drawn."),
    (
        "c21_multiple_centers.layout",
        "How the shapes are arranged: in a row across the box, on a ring around the center, or in a grid.",
    ),
    (
        "c22_tangent_copy_pattern",
        "Adds smaller circles around the outside of the shape, each touching it.",
    ),
    (
        "c22_tangent_copy_pattern.count",
        "How many smaller circles go around the shape.",
    ),
    (
        "c23_inner_outer_separate_subchannels",
        "Puts the outer and inner half of a thick outline on subchannels (colors) of their own.",
    ),
    (
        "c23_inner_outer_separate_subchannels.outer",
        "The subchannel of the outline's outer half, by its position in the track's list.",
    ),
    (
        "c23_inner_outer_separate_subchannels.inner",
        "The subchannel of the outline's inner half, by its position in the track's list.",
    ),
    (
        "c24_polar_wrap",
        "Draws only part of the shape, from one angle around to another, as an arc.",
    ),
    (
        "c24_polar_wrap.start",
        "The angle the drawn part starts at, in degrees.",
    ),
    (
        "c25_recursive_circle_packing",
        "Fills the shape with smaller circles that do not touch (seeded), and circles inside those.",
    ),
    (
        "c25_recursive_circle_packing.count",
        "How many circles are placed inside each circle.",
    ),
    (
        "c25_recursive_circle_packing.depth",
        "How many levels of circles inside circles are drawn.",
    ),
];

fn circles_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        // c01
        flag("c01_center", "Center", false),
        ticks("c01_center.time", "Center time (from the box's start)", 0, MAX_LENGTH, 960),
        pitch("c01_center.pitch", "Center pitch", 60),
        // c02
        flag("c02_radius", "Radius", false),
        float("c02_radius.radius", "Radius (grid units)", 0.5, 1024.0, 6.0, "")
            .help("A circle on the grid: as many steps across as rows up (instead of the ellipse inscribed in the box)."),
        // c03
        flag("c03_ellipse_x_y_radius", "Ellipse X/Y Radius", false),
        float("c03_ellipse_x_y_radius.x", "Radius across (steps)", 0.5, 1024.0, 8.0, ""),
        float("c03_ellipse_x_y_radius.y", "Radius up (rows)", 0.5, 1024.0, 4.0, ""),
        // c04
        flag("c04_ellipse_rotation", "Ellipse Rotation", false),
        float("c04_ellipse_rotation.angle", "Angle", -180.0, 180.0, 30.0, "°")
            .help("The ellipse turned about its center, on the grid: one step across per row up is 45°."),
        // c05
        flag("c05_ring_thickness", "Ring Thickness", false),
        int("c05_ring_thickness.size", "Thickness (grid units)", 1, 64, 2),
        // c06
        flag("c06_filled_disc", "Filled Disc", false),
        // c07
        flag("c07_concentric_rings", "Concentric Rings", false),
        int("c07_concentric_rings.count", "Rings inside", 1, 64, 3)
            .help("Rings inside the outline, evenly spaced towards the center (or by the Ring Spacing Graph)."),
        // c08
        flag("c08_ring_spacing_graph", "Ring Spacing Graph", false),
        curve(
            "c08_ring_spacing_graph.curve",
            "Ring placement",
            Curve::new(vec![(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]),
        )
        .help("Ring k of n lies curve(k / (n + 1)) of the way from the outline to the center."),
        int("c08_ring_spacing_graph.count", "Rings (without Concentric Rings)", 1, 64, 4),
        // c09
        flag("c09_radial_spokes", "Radial Spokes", false),
        int("c09_radial_spokes.count", "Spokes", 2, 64, 8),
        float("c09_radial_spokes.angle", "First spoke's angle", -180.0, 180.0, 0.0, "°"),
        // c10
        flag("c10_spiral_fill", "Spiral Fill", false),
        float("c10_spiral_fill.turns", "Turns", 0.5, 64.0, 3.0, ""),
        choice("c10_spiral_fill.direction", "Direction", SPIN, 0)
            .help("An Archimedean spiral from the center out to the outline."),
        // c11
        flag("c11_angular_density_graph", "Angular Density Graph", false),
        curve(
            "c11_angular_density_graph.curve",
            "Share kept around the shape",
            Curve::new(vec![(0.0, 1.0), (0.5, 0.2), (1.0, 1.0)]),
        )
        .help("Left to right: once round counter-clockwise from the angle 0 (later in time). Cells are kept evenly by the share."),
        // c12
        flag("c12_radial_density_graph", "Radial Density Graph", false),
        curve(
            "c12_radial_density_graph.curve",
            "Share kept from the center out",
            Curve::new(vec![(0.0, 1.0), (1.0, 0.4)]),
        ),
        // c13
        flag("c13_velocity_by_radius", "Velocity by Radius", false),
        int("c13_velocity_by_radius.centre", "Velocity at the center", 1, 127, 127),
        int("c13_velocity_by_radius.edge", "Velocity at the outline", 1, 127, 40),
        // c14
        flag("c14_gate_by_radius", "Gate by Radius", false),
        float("c14_gate_by_radius.centre", "Gate at the center", 1.0, 100.0, 100.0, "%"),
        float("c14_gate_by_radius.edge", "Gate at the outline", 1.0, 100.0, 25.0, "%"),
        // c15
        flag("c15_subchannel_by_angle", "Subchannel by Angle", false),
        int("c15_subchannel_by_angle.from", "Subchannel at the angle 0 (position in the track)", 0, 4095, 0),
        int("c15_subchannel_by_angle.to", "Subchannel once round (position in the track)", 0, 4095, 3),
        // c16
        flag("c16_dashed_arc_pattern", "Dashed Arc Pattern", false),
        int("c16_dashed_arc_pattern.dash", "Dash (cells)", 1, 256, 2),
        int("c16_dashed_arc_pattern.gap", "Gap (cells)", 1, 256, 2),
        float("c16_dashed_arc_pattern.phase", "Phase (cells along the outline)", 0.0, 256.0, 0.0, ""),
        // c17
        flag("c17_sector_fill", "Sector Fill", false),
        float("c17_sector_fill.start", "Start angle", -180.0, 180.0, 0.0, "°"),
        float("c17_sector_fill.sweep", "Sweep", 1.0, 360.0, 90.0, "°")
            .help("A pie slice of the interior, counter-clockwise from the start angle."),
        // c18
        flag("c18_chord_line_fill", "Chord-Line Fill", false),
        int("c18_chord_line_fill.spacing", "Spacing (grid units)", 2, 64, 3),
        float("c18_chord_line_fill.angle", "Angle", -90.0, 90.0, 0.0, "°")
            .help("Parallel chords across the interior: rows at 0°, turned on the grid otherwise."),
        // c19
        flag("c19_wave_modulated_radius", "Wave-Modulated Radius", false),
        int("c19_wave_modulated_radius.waves", "Waves", 1, 64, 6),
        float("c19_wave_modulated_radius.amount", "Depth (% of the radius)", 0.0, 100.0, 15.0, "%"),
        float("c19_wave_modulated_radius.phase", "Phase", -180.0, 180.0, 0.0, "°"),
        // c20
        flag("c20_seeded_radius_roughness", "Seeded Radius Roughness", false),
        float("c20_seeded_radius_roughness.amount", "Roughness (% of the radius)", 0.0, 100.0, 10.0, "%"),
        int("c20_seeded_radius_roughness.detail", "Detail", 1, 8, 4)
            .help("Seeded bumps round the outline: each level twice as many, half as deep."),
        // c21
        flag("c21_multiple_centers", "Multiple Centers", false),
        int("c21_multiple_centers.count", "Centers", 2, 64, 3),
        choice("c21_multiple_centers.layout", "Layout", LAYOUTS, 0),
        float("c21_multiple_centers.size", "Size (% of each one's room)", 10.0, 100.0, 100.0, "%")
            .help("The shape repeated: each center gets its part of the box (its own radius unless Radius / Ellipse X/Y Radius set one)."),
        // c22
        flag("c22_tangent_copy_pattern", "Tangent Copy Pattern", false),
        int("c22_tangent_copy_pattern.count", "Copies", 1, 64, 6),
        float("c22_tangent_copy_pattern.size", "Copy size (% of the shape)", 10.0, 200.0, 50.0, "%")
            .help("Smaller circles around the shape, each touching its outline from outside."),
        // c23
        flag(
            "c23_inner_outer_separate_subchannels",
            "Inner/Outer Separate Subchannels",
            false,
        ),
        int(
            "c23_inner_outer_separate_subchannels.outer",
            "Outer half (subchannel position)",
            0,
            4095,
            1,
        ),
        int(
            "c23_inner_outer_separate_subchannels.inner",
            "Inner half (subchannel position)",
            0,
            4095,
            2,
        ),
        // c24
        flag("c24_polar_wrap", "Polar Wrap", false),
        float("c24_polar_wrap.start", "Start angle", -180.0, 180.0, 0.0, "°"),
        float("c24_polar_wrap.sweep", "Sweep", 1.0, 359.0, 270.0, "°")
            .help("Only the part of the shape between the start angle and the sweep (counter-clockwise) is drawn: an arc, open along its rays."),
        // c25
        flag("c25_recursive_circle_packing", "Recursive Circle Packing", false),
        int("c25_recursive_circle_packing.count", "Circles per circle", 1, 16, 5),
        int("c25_recursive_circle_packing.depth", "Depth", 1, 4, 2),
        float(
            "c25_recursive_circle_packing.smallest",
            "Smallest (% of the circle it is in)",
            2.0,
            45.0,
            10.0,
            "%",
        )
        .help("Seeded circles inside the shape, the largest first, none touching; with depth, circles inside those."),
    ];
    with_help(&mut own, CIRCLES_HELP);
    super::spec(
        "gen.circles",
        "Circles",
        "Generates circles and ellipses of notes: the ellipse inscribed in the generation box (where, length and pitch range above), its outline one cell wide on the resolution grid.",
        own,
        FUNCTIONS,
    )
}

/// The functions' settings (`None` / neutral = off).
#[derive(Clone, Debug, Default)]
struct Cfg {
    /// c01: ticks from the box's start, pitch.
    centre: Option<(f64, f64)>,
    /// c02 radius; c03 radii.
    radius: Option<f64>,
    radii: Option<(f64, f64)>,
    /// c04: radians.
    rotation: f64,
    /// c05: grid units (1 = off).
    thick: f64,
    /// c06.
    filled: bool,
    /// c07 count; c08 curve and its own count.
    rings: Option<u32>,
    spacing: Option<(Curve, u32)>,
    /// c09: count, first angle; c10: turns, clockwise.
    spokes: Option<(u32, f64)>,
    spiral: Option<(f64, bool)>,
    /// c11, c12.
    angular: Option<Curve>,
    radial: Option<Curve>,
    /// c13 velocity, c14 gate, c15 subchannel: (centre / start, edge / end).
    vel: Option<(f64, f64)>,
    gate: Option<(f64, f64)>,
    sub: Option<(f64, f64)>,
    /// c16: dash, gap, phase (cells).
    dash: Option<(f64, f64, f64)>,
    /// c17: start, sweep (radians).
    sector: Option<(f64, f64)>,
    /// c18: spacing, angle (radians).
    chords: Option<(f64, f64)>,
    /// c19: waves, depth, phase.
    wave: Option<(f64, f64, f64)>,
    /// c20: depth, detail.
    rough: Option<(f64, u32)>,
    /// c21: count, layout, size.
    centres: Option<(u32, usize, f64)>,
    /// c22: count, size.
    tangent: Option<(u32, f64)>,
    /// c23: outer, inner.
    halves: Option<(u32, u32)>,
    /// c24: start, sweep (radians).
    wrap: Option<(f64, f64)>,
    /// c25: count, depth, smallest.
    pack: Option<(u32, u32, f64)>,
}

impl Cfg {
    fn read(p: &Params) -> Cfg {
        let on = |id: &str| p.flag(id);
        let mut c = Cfg {
            thick: 1.0,
            ..Cfg::default()
        };
        if on("c01_center") {
            c.centre = Some((
                p.ticks("c01_center.time") as f64,
                p.int("c01_center.pitch") as f64,
            ));
        }
        if on("c02_radius") {
            c.radius = Some(p.float("c02_radius.radius").max(0.5));
        }
        if on("c03_ellipse_x_y_radius") {
            c.radii = Some((
                p.float("c03_ellipse_x_y_radius.x").max(0.5),
                p.float("c03_ellipse_x_y_radius.y").max(0.5),
            ));
        }
        if on("c04_ellipse_rotation") {
            c.rotation = p.float("c04_ellipse_rotation.angle").to_radians();
        }
        if on("c05_ring_thickness") {
            c.thick = p.int("c05_ring_thickness.size").clamp(1, 64) as f64;
        }
        c.filled = on("c06_filled_disc");
        if on("c07_concentric_rings") {
            c.rings = Some(p.int("c07_concentric_rings.count").clamp(1, 64) as u32);
        }
        if on("c08_ring_spacing_graph") {
            c.spacing = Some((
                p.curve("c08_ring_spacing_graph.curve"),
                p.int("c08_ring_spacing_graph.count").clamp(1, 64) as u32,
            ));
        }
        if on("c09_radial_spokes") {
            c.spokes = Some((
                p.int("c09_radial_spokes.count").clamp(2, 64) as u32,
                p.float("c09_radial_spokes.angle").to_radians(),
            ));
        }
        if on("c10_spiral_fill") {
            c.spiral = Some((
                p.float("c10_spiral_fill.turns").clamp(0.5, 64.0),
                p.choice("c10_spiral_fill.direction") == 1,
            ));
        }
        if on("c11_angular_density_graph") {
            c.angular = Some(p.curve("c11_angular_density_graph.curve"));
        }
        if on("c12_radial_density_graph") {
            c.radial = Some(p.curve("c12_radial_density_graph.curve"));
        }
        if on("c13_velocity_by_radius") {
            c.vel = Some((
                p.int("c13_velocity_by_radius.centre") as f64,
                p.int("c13_velocity_by_radius.edge") as f64,
            ));
        }
        if on("c14_gate_by_radius") {
            c.gate = Some((
                p.float("c14_gate_by_radius.centre") / 100.0,
                p.float("c14_gate_by_radius.edge") / 100.0,
            ));
        }
        if on("c15_subchannel_by_angle") {
            c.sub = Some((
                p.int("c15_subchannel_by_angle.from") as f64,
                p.int("c15_subchannel_by_angle.to") as f64,
            ));
        }
        if on("c16_dashed_arc_pattern") {
            c.dash = Some((
                p.int("c16_dashed_arc_pattern.dash").max(1) as f64,
                p.int("c16_dashed_arc_pattern.gap").max(1) as f64,
                p.float("c16_dashed_arc_pattern.phase").max(0.0),
            ));
        }
        if on("c17_sector_fill") {
            c.sector = Some((
                p.float("c17_sector_fill.start").to_radians(),
                p.float("c17_sector_fill.sweep")
                    .clamp(1.0, 360.0)
                    .to_radians(),
            ));
        }
        if on("c18_chord_line_fill") {
            c.chords = Some((
                p.int("c18_chord_line_fill.spacing").max(2) as f64,
                p.float("c18_chord_line_fill.angle")
                    .clamp(-90.0, 90.0)
                    .to_radians(),
            ));
        }
        if on("c19_wave_modulated_radius") {
            c.wave = Some((
                p.int("c19_wave_modulated_radius.waves").clamp(1, 64) as f64,
                p.float("c19_wave_modulated_radius.amount")
                    .clamp(0.0, 100.0)
                    / 100.0,
                p.float("c19_wave_modulated_radius.phase").to_radians(),
            ));
        }
        if on("c20_seeded_radius_roughness") {
            c.rough = Some((
                p.float("c20_seeded_radius_roughness.amount")
                    .clamp(0.0, 100.0)
                    / 100.0,
                p.int("c20_seeded_radius_roughness.detail").clamp(1, 8) as u32,
            ));
        }
        if on("c21_multiple_centers") {
            c.centres = Some((
                p.int("c21_multiple_centers.count").clamp(2, 64) as u32,
                p.choice("c21_multiple_centers.layout").min(2),
                p.float("c21_multiple_centers.size").clamp(10.0, 100.0) / 100.0,
            ));
        }
        if on("c22_tangent_copy_pattern") {
            c.tangent = Some((
                p.int("c22_tangent_copy_pattern.count").clamp(1, 64) as u32,
                p.float("c22_tangent_copy_pattern.size").clamp(10.0, 200.0) / 100.0,
            ));
        }
        if on("c23_inner_outer_separate_subchannels") {
            let k = "c23_inner_outer_separate_subchannels";
            c.halves = Some((
                p.int(&format!("{k}.outer")).clamp(0, 4095) as u32,
                p.int(&format!("{k}.inner")).clamp(0, 4095) as u32,
            ));
        }
        if on("c24_polar_wrap") {
            c.wrap = Some((
                p.float("c24_polar_wrap.start").to_radians(),
                p.float("c24_polar_wrap.sweep")
                    .clamp(1.0, 359.0)
                    .to_radians(),
            ));
        }
        if on("c25_recursive_circle_packing") {
            c.pack = Some((
                p.int("c25_recursive_circle_packing.count").clamp(1, 16) as u32,
                p.int("c25_recursive_circle_packing.depth").clamp(1, 4) as u32,
                p.float("c25_recursive_circle_packing.smallest")
                    .clamp(2.0, 45.0)
                    / 100.0,
            ));
        }
        c
    }

    /// How many circles packing places at most (Σ count^level).
    fn packed(&self) -> u64 {
        self.pack.map_or(0, |(n, d, _)| {
            (1..=d).fold(0u64, |s, l| {
                s.saturating_add(u64::from(n).saturating_pow(l))
            })
        })
    }
}

/// A polar shape: centre, radii along its axes, rotation, and the radius
/// factor round it (waves, roughness).
#[derive(Clone, Debug)]
struct Shape {
    c: P2,
    rx: f64,
    ry: f64,
    rot: f64,
    /// c19: waves, depth, phase.
    wave: Option<(f64, f64, f64)>,
    /// c20: the roughness at evenly spaced angles (periodic), depth.
    rough: Option<(Vec<f64>, f64)>,
}

impl Shape {
    fn circle(c: P2, r: f64) -> Shape {
        Shape {
            c,
            rx: r,
            ry: r,
            rot: 0.0,
            wave: None,
            rough: None,
        }
    }

    /// Whether the shape is an ellipse (convex rings).
    fn plain(&self) -> bool {
        self.wave.is_none() && self.rough.is_none()
    }

    /// The radius factor at local angle `a`.
    fn factor(&self, a: f64) -> f64 {
        let mut k = 1.0;
        if let Some((n, d, ph)) = self.wave {
            k *= 1.0 + d * (n * a + ph).sin();
        }
        if let Some((v, d)) = &self.rough {
            let m = v.len() as f64;
            let t = a.rem_euclid(TAU) / TAU * m;
            let i = t.floor();
            let f = t - i;
            let (p, q) = (v[i as usize % v.len()], v[(i as usize + 1) % v.len()]);
            // Smooth between the points (cosine).
            let s = (1.0 - (f * PI).cos()) / 2.0;
            k *= 1.0 + d * (p + (q - p) * s);
        }
        k.max(0.0)
    }

    /// The radius at local polar angle `a` (the ellipse's, times the
    /// factor).
    fn r(&self, a: f64) -> f64 {
        let (s, co) = a.sin_cos();
        let e = (self.rx * self.ry) / ((self.ry * co).hypot(self.rx * s)).max(1e-12);
        e * self.factor(a)
    }

    /// The largest radius (a bound).
    fn r_max(&self) -> f64 {
        let mut k = 1.0;
        if let Some((_, d, _)) = self.wave {
            k *= 1.0 + d;
        }
        if let Some((_, d)) = &self.rough {
            k *= 1.0 + d;
        }
        self.rx.max(self.ry) * k
    }

    /// The smallest radius (a bound).
    fn r_min(&self) -> f64 {
        let mut k = 1.0;
        if let Some((_, d, _)) = self.wave {
            k *= (1.0 - d).max(0.0);
        }
        if let Some((_, d)) = &self.rough {
            k *= (1.0 - d).max(0.0);
        }
        self.rx.min(self.ry) * k
    }

    /// Points round the curve.
    fn points(&self) -> usize {
        let r = self.r_max().max(0.5);
        let by_sag = (PI / (1.0 - SAG / r).clamp(-1.0, 1.0).acos()).ceil();
        let by_wave = self.wave.map_or(0.0, |(n, _, _)| 16.0 * n);
        let by_rough = self
            .rough
            .as_ref()
            .map_or(0.0, |(v, _)| 4.0 * v.len() as f64);
        // Never more than four points per grid unit of the perimeter.
        let by_size = (4.0 * TAU * r).ceil().max(16.0);
        let n = by_sag
            .max(by_wave)
            .max(by_rough)
            .min(by_size)
            .max(16.0)
            .min(MAX_POINTS as f64) as usize;
        n.div_ceil(4) * 4
    }

    /// The point at local polar angle `a` and radius `r` (grid space).
    fn at(&self, a: f64, r: f64) -> P2 {
        let (s, co) = (a + self.rot).sin_cos();
        (self.c.0 + r * co, self.c.1 + r * s)
    }

    /// The curve's samples: for each of [`Shape::points`] angles, the
    /// ray's direction (turned), the radius and the factor `1 / cos φ`
    /// (φ between the ray and the normal) that moves the curve along its
    /// normal.
    fn table(&self) -> Vec<(f64, f64, f64, f64)> {
        let n = self.points();
        let h = TAU / n as f64;
        (0..n)
            .map(|k| {
                let a = k as f64 * h;
                let r = self.r(a);
                let dr = (self.r(a + h / 2.0) - self.r(a - h / 2.0)) / h;
                let f = if r > 1e-9 { r.hypot(dr) / r } else { 1.0 };
                let (s, co) = (a + self.rot).sin_cos();
                (co, s, r, f)
            })
            .collect()
    }

    /// The curve moved `d` grid units in (d < 0) or out along each ray
    /// (by d / cos φ; never past the centre): a simple star-shaped polygon.
    fn ring_t(&self, tab: &[(f64, f64, f64, f64)], d: f64) -> Poly {
        let pts = tab
            .iter()
            .map(|&(co, s, r, f)| {
                let rr = (r + d * f).max(0.0);
                (self.c.0 + rr * co, self.c.1 + rr * s)
            })
            .collect();
        Poly::from_points(pts, vec![0; tab.len()])
    }

    /// The region's frame for this shape.
    fn copy(&self, tab: &[(f64, f64, f64, f64)]) -> CopySpec {
        CopySpec {
            base: self.ring_t(tab, 0.0),
            hole: None,
            centre: self.c,
            corner: self.at(0.0, self.rx.max(1e-9)),
            radii: (self.rx, self.ry, self.rot),
            side_segs: Vec::new(),
        }
    }
}

/// A wedge from `c` between angles `a0` and `a0 + sweep` (≤ π) out to
/// radius `r` (grid space; convex).
fn wedge(c: P2, a0: f64, sweep: f64, r: f64) -> Poly {
    let k = ((sweep / (PI / 32.0)).ceil() as usize).clamp(1, 64);
    let mut pts = vec![c];
    for j in 0..=k {
        let a = a0 + sweep * j as f64 / k as f64;
        pts.push((c.0 + r * a.cos(), c.1 + r * a.sin()));
    }
    Poly::from_points(pts, vec![0; k + 2])
}

/// Wedges covering the angles `a0 … a0 + sweep` (each ≤ π).
fn wedges(c: P2, a0: f64, sweep: f64, r: f64) -> Vec<Poly> {
    let n = (sweep / PI).ceil().max(1.0) as usize;
    (0..n)
        .map(|i| wedge(c, a0 + sweep * i as f64 / n as f64, sweep / n as f64, r))
        .collect()
}

/// The Circles generator.
struct Circles {
    cfg: Cfg,
    built: OnceLock<(Scene, Style)>,
}

impl Circles {
    fn new(cfg: Cfg) -> Circles {
        Circles {
            cfg,
            built: OnceLock::new(),
        }
    }

    fn built(&self, f: &Frame) -> &(Scene, Style) {
        self.built.get_or_init(|| (self.scene(f), self.style(f)))
    }

    /// The shapes at every centre (c01 – c04, c19 – c21).
    fn shapes(&self, f: &Frame) -> Vec<Shape> {
        let c = &self.cfg;
        let step = f.step as f64;
        let (u0, u1) = (f.t0 as f64 / step, f.t1 as f64 / step);
        let (v0, v1) = (f64::from(f.lo), f64::from(f.hi) + 1.0);
        let (w, h) = (u1 - u0, v1 - v0);
        let centre = c
            .centre
            .map_or(((u0 + u1) / 2.0, (v0 + v1) / 2.0), |(t, p)| {
                ((f.t0 as f64 + t) / step, p + 0.5)
            });
        let fixed = c.radii.or(c.radius.map(|r| (r, r)));
        let rough = c.rough.map(|(d, levels)| {
            let mut rng = Rng::new(f.seed ^ 0x4349_5243_4C45);
            // Periodic midpoint displacement: 4 points, doubled per level,
            // each level half as deep.
            let mut v: Vec<f64> = (0..4).map(|_| rng.range_f64(-1.0, 1.0)).collect();
            let mut amp = 0.5;
            for _ in 1..levels {
                let n = v.len();
                let mut next = Vec::with_capacity(2 * n);
                for i in 0..n {
                    next.push(v[i]);
                    next.push((v[i] + v[(i + 1) % n]) / 2.0 + rng.range_f64(-amp, amp));
                }
                v = next;
                amp /= 2.0;
            }
            let m = v.iter().fold(0.0f64, |a, x| a.max(x.abs())).max(1e-9);
            (v.iter().map(|x| x / m).collect(), d)
        });
        let make = |cx: P2, rx: f64, ry: f64| Shape {
            c: cx,
            rx: rx.max(0.25),
            ry: ry.max(0.25),
            rot: c.rotation,
            wave: c.wave,
            rough: rough.clone(),
        };
        let Some((n, layout, size)) = c.centres else {
            let (rx, ry) = fixed.unwrap_or((w / 2.0, h / 2.0));
            return vec![make(centre, rx, ry)];
        };
        let nf = f64::from(n);
        match layout {
            1 => {
                // On a ring: the centres half-way to the outline.
                let (rx, ry) = fixed.unwrap_or((w / 2.0, h / 2.0));
                let room = (PI / nf).sin() / (1.0 + (PI / nf).sin());
                (0..n)
                    .map(|i| {
                        let a = TAU * f64::from(i) / nf;
                        let p = (centre.0 + rx / 2.0 * a.cos(), centre.1 + ry / 2.0 * a.sin());
                        match fixed {
                            Some((fx, fy)) => make(p, fx * size, fy * size),
                            None => make(p, rx * room * size, ry * room * size),
                        }
                    })
                    .collect()
            }
            2 => {
                let cols = (nf.sqrt().ceil()).max(1.0);
                let rows = (nf / cols).ceil();
                (0..n)
                    .map(|i| {
                        let (ci, ri) = (f64::from(i) % cols, (f64::from(i) / cols).floor());
                        let p = (u0 + (ci + 0.5) * w / cols, v0 + (ri + 0.5) * h / rows);
                        let (rx, ry) = fixed.unwrap_or((w / cols / 2.0, h / rows / 2.0));
                        make(p, rx * size, ry * size)
                    })
                    .collect()
            }
            _ => (0..n)
                .map(|i| {
                    let p = (u0 + (f64::from(i) + 0.5) * w / nf, centre.1);
                    let (rx, ry) = fixed.unwrap_or((w / nf / 2.0, h / 2.0));
                    make(p, rx * size, ry * size)
                })
                .collect(),
        }
    }

    /// The nested rings' insets (grid units) for shape `s` (c07 / c08).
    fn insets(&self, s: &Shape) -> Vec<f64> {
        let c = &self.cfg;
        let n = match (c.rings, &c.spacing) {
            (Some(n), _) => n,
            (None, Some((_, n))) => *n,
            (None, None) => return Vec::new(),
        };
        let r = s.r_min().max(0.5);
        (1..=n)
            .map(|k| {
                let t = f64::from(k) / f64::from(n + 1);
                let q = c
                    .spacing
                    .as_ref()
                    .map_or(t, |(cv, _)| cv.eval(t).clamp(0.0, 1.0));
                (r * q).round().max(1.0)
            })
            .collect()
    }

    /// Circles packed inside circle (centre, radius) — seeded, the largest
    /// first, none touching (c25).
    fn pack(&self, rng: &mut Rng, c: P2, r: f64, out: &mut Vec<(P2, f64)>) {
        let Some((n, depth, smallest)) = self.cfg.pack else {
            return;
        };
        let gap = self.cfg.thick + 1.0;
        let mut level = vec![(c, r)];
        for _ in 0..depth {
            let mut next = Vec::new();
            for &(pc, pr) in &level {
                let mut placed: Vec<(P2, f64)> = Vec::new();
                for j in 0..n {
                    // Radii from 45 % of the container down to the smallest.
                    let t = if n > 1 {
                        f64::from(j) / f64::from(n - 1)
                    } else {
                        0.0
                    };
                    let q = 0.45 * (smallest / 0.45).powf(t);
                    let cr = pr * q;
                    if cr < 0.5 {
                        continue;
                    }
                    for _ in 0..64 {
                        let a = rng.range_f64(0.0, TAU);
                        let d = (pr - cr - gap).max(0.0) * rng.unit().sqrt();
                        let p = (pc.0 + d * a.cos(), pc.1 + d * a.sin());
                        let fits = placed
                            .iter()
                            .all(|&(o, orr)| (p.0 - o.0).hypot(p.1 - o.1) >= cr + orr + gap);
                        if fits && d + cr <= pr - gap {
                            placed.push((p, cr));
                            break;
                        }
                    }
                }
                next.extend(placed.iter().copied());
            }
            out.extend(next.iter().copied());
            level = next;
        }
    }

    /// The copies and rings and line features, or the cap's sentence.
    #[allow(clippy::type_complexity)]
    fn parts(
        &self,
        f: &Frame,
    ) -> Result<(Vec<CopySpec>, Vec<RingSpec>, Vec<(P2, P2)>), TransformError> {
        let c = &self.cfg;
        let shapes = self.shapes(f);
        let th = c.thick;
        let mut copies: Vec<CopySpec> = Vec::new();
        let mut rings: Vec<RingSpec> = Vec::new();
        let mut lines: Vec<(P2, P2)> = Vec::new();
        let mut rng = Rng::new(f.seed ^ 0x5041_434B_3235);
        let ring_of = |s: &Shape, tab: &[(f64, f64, f64, f64)], d: f64, role: Role, copy: u16| {
            let poly = s.ring_t(tab, d);
            RingSpec {
                convex: s.plain() && poly.is_convex(),
                poly,
                role,
                copy,
            }
        };
        let push_copy = |copies: &mut Vec<CopySpec>,
                         rings: &mut Vec<RingSpec>,
                         s: &Shape,
                         tab: &[(f64, f64, f64, f64)]| {
            let id = copies.len() as u16;
            copies.push(s.copy(tab));
            rings.push(ring_of(s, tab, 0.0, Role::Outer, id));
            rings.push(ring_of(s, tab, -th, Role::Inner, id));
            id
        };
        for s in &shapes {
            let tab = s.table();
            let id = push_copy(&mut copies, &mut rings, s, &tab);
            if c.halves.is_some() {
                rings.push(ring_of(s, &tab, -th / 2.0, Role::Mid, id));
            }
            for d in self.insets(s) {
                let out = ring_of(s, &tab, -d, Role::NestOuter, id);
                if out.poly.is_empty() {
                    break;
                }
                rings.push(out);
                rings.push(ring_of(s, &tab, -d - th, Role::NestInner, id));
            }
            let big = s.r_max() * 2.0 + 2.0;
            if let Some((a0, sweep)) = c.sector {
                for w in wedges(s.c, a0 + s.rot, sweep, big) {
                    rings.push(RingSpec {
                        poly: w,
                        role: Role::Fill,
                        copy: id,
                        convex: true,
                    });
                }
            }
            if let Some((a0, sweep)) = c.wrap {
                for w in wedges(s.c, a0 + s.rot, sweep, big) {
                    rings.push(RingSpec {
                        poly: w,
                        role: Role::Mask,
                        copy: id,
                        convex: true,
                    });
                }
            }
            // Lines end on the circle 2 units outside the shape (what lies
            // beyond meets no cell of its interior).
            let reach = s.r_max() + 2.0;
            if let Some((n, a0)) = c.spokes {
                for i in 0..n {
                    let a = a0 + s.rot + TAU * f64::from(i) / f64::from(n);
                    lines.push((s.c, (s.c.0 + reach * a.cos(), s.c.1 + reach * a.sin())));
                }
            }
            if let Some((turns, cw)) = c.spiral {
                let segs = Self::spiral_segments(s, turns);
                let total = TAU * turns;
                let sign = if cw { -1.0 } else { 1.0 };
                let pt = |j: usize| {
                    let t = j as f64 / segs as f64;
                    let a = sign * total * t;
                    s.at(a, s.r(a) * t)
                };
                for j in 0..segs {
                    lines.push((pt(j), pt(j + 1)));
                }
            }
            if let Some((sp, a)) = c.chords
                && a != 0.0
            {
                let (dir, nrm) = ((a.cos(), a.sin()), (-a.sin(), a.cos()));
                let k = Self::chord_reach(reach, sp);
                for i in -k..=k {
                    let d = sp * i as f64;
                    let half = (reach * reach - d * d).max(0.0).sqrt();
                    let o = (s.c.0 + nrm.0 * d, s.c.1 + nrm.1 * d);
                    lines.push((
                        (o.0 - dir.0 * half, o.1 - dir.1 * half),
                        (o.0 + dir.0 * half, o.1 + dir.1 * half),
                    ));
                }
            }
            if lines.len() > LINE_CAP {
                return Err(TransformError::TooLarge(format!(
                    "Circles would draw {} line segments; at most {} are allowed — use fewer centers, spokes, spiral turns or chords.",
                    group_digits(lines.len() as u64),
                    group_digits(LINE_CAP as u64)
                )));
            }
        }
        // c22: tangent copies round each shape.
        if let Some((n, size)) = c.tangent {
            for s in &shapes {
                for i in 0..n {
                    let a = TAU * f64::from(i) / f64::from(n);
                    let r0 = s.r(a);
                    let cr = r0 * size;
                    let p = s.at(a, r0 + cr);
                    let sc = Shape::circle(p, cr);
                    push_copy(&mut copies, &mut rings, &sc, &sc.table());
                }
            }
        }
        // c25: circles packed inside each shape's inscribed circle.
        if c.pack.is_some() {
            for s in &shapes {
                let mut out = Vec::new();
                self.pack(&mut rng, s.c, s.r_min() - th, &mut out);
                for (p, r) in out {
                    let sc = Shape::circle(p, r);
                    push_copy(&mut copies, &mut rings, &sc, &sc.table());
                }
            }
        }
        if rings.len() > RING_CAP {
            return Err(TransformError::TooLarge(format!(
                "Circles would draw {} rings; at most {} are allowed — use fewer centers, rings, copies or packed circles.",
                group_digits(rings.len() as u64),
                group_digits(RING_CAP as u64)
            )));
        }
        Ok((copies, rings, lines))
    }

    /// The caps, with sentences (before anything is built).
    fn check(&self, f: &Frame) -> Result<(), TransformError> {
        let c = &self.cfg;
        let packed = c.packed();
        let centres = u64::from(c.centres.map_or(1, |c| c.0));
        if packed.saturating_mul(centres) > PACK_CAP {
            return Err(TransformError::TooLarge(format!(
                "Recursive Circle Packing could place {} circles; at most {} are allowed — use fewer circles per circle, less depth or fewer centers.",
                group_digits(packed.saturating_mul(centres)),
                group_digits(PACK_CAP)
            )));
        }
        // The rings at most, counted before any is built.
        let nested = u64::from(match (c.rings, &c.spacing) {
            (Some(n), _) => n,
            (None, Some((_, n))) => *n,
            _ => 0,
        });
        let wedges = |s: Option<(f64, f64)>| s.map_or(0, |(_, w)| (w / PI).ceil() as u64);
        let per_centre = 2
            + u64::from(c.halves.is_some())
            + 2 * nested
            + wedges(c.sector)
            + wedges(c.wrap)
            + 2 * u64::from(c.tangent.map_or(0, |t| t.0))
            + 2 * packed;
        let rings = per_centre.saturating_mul(centres);
        // Points: the centres' rings at their tessellation (tangent copies
        // and packed circles are smaller than their shape).
        let points: u64 = self
            .shapes(f)
            .iter()
            .map(|s| s.points() as u64)
            .sum::<u64>()
            .saturating_mul(per_centre);
        if points > POINT_CAP {
            return Err(TransformError::TooLarge(format!(
                "Circles would build {} outline points; at most {} are allowed — use fewer or smaller circles, fewer rings, waves or less roughness detail.",
                group_digits(points),
                group_digits(POINT_CAP)
            )));
        }
        if rings > RING_CAP as u64 {
            return Err(TransformError::TooLarge(format!(
                "Circles would draw {} rings; at most {} are allowed — use fewer centers, rings, copies or packed circles.",
                group_digits(rings),
                group_digits(RING_CAP as u64)
            )));
        }
        let lines: u64 = self.shapes(f).iter().map(|s| self.line_count(s)).sum();
        if lines > LINE_CAP as u64 {
            return Err(TransformError::TooLarge(format!(
                "Circles would draw {} line segments; at most {} are allowed — use fewer centers, spokes, spiral turns or chords.",
                group_digits(lines),
                group_digits(LINE_CAP as u64)
            )));
        }
        Ok(())
    }

    /// The line segments of shape `s` (spokes, spiral, turned chords).
    fn line_count(&self, s: &Shape) -> u64 {
        let c = &self.cfg;
        let reach = s.r_max() + 2.0;
        let spokes = c.spokes.map_or(0, |(n, _)| u64::from(n));
        let spiral = c
            .spiral
            .map_or(0, |(turns, _)| Self::spiral_segments(s, turns) as u64);
        let chords = match c.chords {
            Some((sp, a)) if a != 0.0 => 2 * Self::chord_reach(reach, sp).unsigned_abs() + 1,
            _ => 0,
        };
        spokes + spiral + chords
    }

    /// Chords run at offsets `-k ..= k` spacings from the centre: those
    /// within `reach`.
    fn chord_reach(reach: f64, sp: f64) -> i64 {
        // Saturating: a huge count is refused by the line cap.
        (reach / sp).floor().min(1e15) as i64
    }

    /// The segments of a spiral of `turns` in shape `s`.
    fn spiral_segments(s: &Shape, turns: f64) -> usize {
        let per_turn = ((TAU * s.r_max()).ceil()).clamp(16.0, 256.0);
        (per_turn * turns).clamp(8.0, 1024.0) as usize
    }

    fn scene(&self, f: &Frame) -> Scene {
        match self.parts(f) {
            Ok((copies, rings, lines)) => Scene::with_copies(copies, rings, lines),
            Err(_) => Scene::default(),
        }
    }

    /// How the scene is drawn: fills, densities, brushes.
    fn style(&self, f: &Frame) -> Style {
        let c = &self.cfg;
        let mut st = Style {
            filled: c.filled,
            half_subs: c.halves,
            ..Style::default()
        };
        if let Some((sp, a)) = c.chords
            && a == 0.0
        {
            let shapes = self.shapes(f);
            let r0 = shapes.first().map_or(0, |s| s.c.1.floor() as i64);
            st.row_stripes.push((r0, sp as i64));
        }
        if let Some((d, g, ph)) = c.dash {
            st.arc_dash = Some((d, d + g, ph));
        }
        if let Some(cv) = &c.angular {
            st.density.push(Density {
                curve: cv.clone(),
                by: GDir::Around,
            });
        }
        if let Some(cv) = &c.radial {
            st.density.push(Density {
                curve: cv.clone(),
                by: GDir::Radial,
            });
        }
        if let Some((a, z)) = c.vel {
            st.grads.push((
                Grad {
                    vel: Some((a, z)),
                    ..Grad::default()
                },
                GDir::Radial,
            ));
        }
        if let Some((a, z)) = c.gate {
            st.grads.push((
                Grad {
                    gate: Some((a, z)),
                    ..Grad::default()
                },
                GDir::Radial,
            ));
        }
        if let Some((a, z)) = c.sub {
            st.grads.push((
                Grad {
                    sub: Some((a, z)),
                    ..Grad::default()
                },
                GDir::Around,
            ));
        }
        st
    }
}

impl Shapes for Circles {
    fn draw(&self, f: &Frame, pen: &mut dyn Draw) -> bool {
        let (scene, style) = self.built(f);
        scene.draw(style, f.brush, f.step as f64, f.band(), pen)
    }

    fn bound(&self, f: &Frame) -> u64 {
        let (scene, style) = self.built(f);
        let b = scene.bound(style, f.band());
        if b.is_finite() && b < u64::MAX as f64 {
            b.ceil() as u64
        } else {
            u64::MAX
        }
    }

    fn floor(&self, f: &Frame) -> u64 {
        let (scene, style) = self.built(f);
        let v = scene.floor(style, f.band());
        if v.is_finite() && v < u64::MAX as f64 {
            v as u64
        } else {
            0
        }
    }
}

fn circles_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let circles = Circles::new(Cfg::read(p));
    circles.check(&f)?;
    super::kernel(p, b, circles)
}

#[cfg(test)]
mod tests {
    use super::super::super::param::{ParamSet, ParamValue};
    use super::super::super::{Ctx, SourceStats};
    use super::super::pen::{Counter, Pen};
    use super::*;

    /// The lower bound never passes the count and the cheap bound never
    /// falls below it, over seeded random settings (functions on with
    /// probability ⅓, sizes drawn) in small boxes; refused settings are
    /// refused before anything is built.
    #[test]
    fn the_lower_bound_never_passes_the_count_and_the_bound_never_falls_below_it() {
        let spec = circles_spec();
        let mut rng = Rng::new(0x5345_3534);
        let mut drawn = 0;
        for draw in 0..300 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 64)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            p.set("gen.seed", ParamValue::Int(rng.range_i64(1, 1000)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            for (k, lo, hi) in [
                ("c05_ring_thickness.size", 1, 4),
                ("c07_concentric_rings.count", 1, 6),
                ("c09_radial_spokes.count", 2, 12),
                ("c16_dashed_arc_pattern.dash", 1, 4),
                ("c16_dashed_arc_pattern.gap", 1, 4),
                ("c18_chord_line_fill.spacing", 2, 6),
                ("c19_wave_modulated_radius.waves", 1, 12),
                ("c21_multiple_centers.count", 2, 5),
                ("c22_tangent_copy_pattern.count", 1, 8),
                ("c25_recursive_circle_packing.count", 1, 5),
                ("c25_recursive_circle_packing.depth", 1, 2),
            ] {
                p.set(k, ParamValue::Int(rng.range_i64(lo, hi)));
            }
            p.set(
                "c02_radius.radius",
                ParamValue::Float(rng.range_f64(1.0, 30.0)),
            );
            p.set(
                "c18_chord_line_fill.angle",
                ParamValue::Float(rng.range_f64(-60.0, 60.0)),
            );
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let k = Circles::new(Cfg::read(&params));
            if k.check(&f).is_err() {
                continue;
            }
            drawn += 1;
            let mut pen = Pen::new(Counter::default(), f.step, f.band());
            k.draw(&f, &mut pen);
            let n = pen.finish().notes;
            assert!(k.floor(&f) <= n, "draw {draw}: floor {} > {n}", k.floor(&f));
            assert!(k.bound(&f) >= n, "draw {draw}: bound {} < {n}", k.bound(&f));
        }
        assert!(drawn > 200, "{drawn}");
    }
}
