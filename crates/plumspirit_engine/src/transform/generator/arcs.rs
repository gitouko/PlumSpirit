//! Arcs / Curves (§155, `gen.arcs`; `docs/GENERATORS.md` "Arcs / Curves"):
//! open curves through the generation box, drawn by the shared path
//! renderers (`generator/path.rs`), and the contract's 25 functions (catalog
//! ids `c01_…` … `c25_…`, switch = id, controls `<id>.<name>`).
//!
//! Curves are built in **grid space** (u = ticks / step, v = rows — a circle
//! is round on the grid) and flattened adaptively (chords at most
//! [`SAG`] grid units off the curve, at most [`FLAT_CAP`] points), then
//! carried to note space for drawing. The pipeline:
//!
//! 1. **Curve** — the base: a quarter of an ellipse rising across the box
//!    (from its start at the lowest row, along the bottom, up to its end at
//!    the highest row). The last of c01 Circular Arc, c02 Elliptical Arc,
//!    c03 Quadratic Bezier, c04 Cubic Bezier, c05 Multi-Point Spline (a
//!    clamped cubic B-spline) and c06 Catmull-Rom switched on replaces it;
//!    c07 Curvature Tension pulls it towards (or pushes it away from) the
//!    straight line between its ends.
//! 2. **Reshaping** — c22 Loop / Cusp Mode (a trochoid rolling along it),
//!    c21 Seeded Normal Jitter, c16 Reverse Direction (backwards in time).
//! 3. **Curves** — c23 Curve A→B Morph (in-betweens to a second curve), c09
//!    Parallel Curves (offset along the normal), then c15 Dash Pattern
//!    (dashes by arc length).
//! 4. **Copies** (affine maps on the grid, applied while drawing) — c24
//!    Recursive Branch Curves, c18 Tangent-Aligned Duplication, c19
//!    Normal-Offset Duplication, c17 Mirror.
//! 5. **Drawing** — c08 Stroke Thickness (a band), c10 Adaptive Note
//!    Sampling, c11 Density Along Curve, c12 – c14 velocity, gate and
//!    subchannel along the curve, c20 Curvature-Driven Modulation; c25
//!    Ribbon Fill From Curve fills a ribbon along it or to the box's bottom
//!    or top.
//!
//! c21 and c24 draw random values (`gen.seed`).

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::Rng;
use super::super::{Curve, Kernel, TransformError};
use super::draw::{Aff, BoxG, MAX_PIECES, Pt, flatten, length, resample};
pub use super::draw::{FLAT_CAP, SAG};
use super::path::{FillTo, PathStyle, Scratch, Stats, by_curvature, measure};
use super::pen::{Brush, Draw};
use super::{Frame, Shapes};
use crate::midi::group_digits;
use std::f64::consts::TAU;
use std::sync::OnceLock;

pub const ARCS: ToolDef = ToolDef {
    id: "gen.arcs",
    spec: arcs_spec,
    build: arcs_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_circular_arc", "Circular Arc", 10),
    ("c02_elliptical_arc", "Elliptical Arc", 20),
    ("c03_quadratic_bezier", "Quadratic Bezier", 30),
    ("c04_cubic_bezier", "Cubic Bezier", 40),
    ("c05_multi_point_spline", "Multi-Point Spline", 50),
    (
        "c06_catmull_rom_smooth_path",
        "Catmull-Rom / Smooth Path",
        60,
    ),
    ("c07_curvature_tension", "Curvature Tension", 70),
    ("c08_stroke_thickness", "Stroke Thickness", 80),
    ("c09_parallel_curves", "Parallel Curves", 90),
    ("c10_adaptive_note_sampling", "Adaptive Note Sampling", 100),
    ("c11_density_along_curve", "Density Along Curve", 110),
    ("c12_velocity_along_curve", "Velocity Along Curve", 120),
    ("c13_gate_along_curve", "Gate Along Curve", 130),
    ("c14_subchannel_along_curve", "Subchannel Along Curve", 140),
    ("c15_dash_pattern", "Dash Pattern", 150),
    ("c16_reverse_direction", "Reverse Direction", 160),
    ("c17_mirror", "Mirror", 170),
    (
        "c18_tangent_aligned_duplication",
        "Tangent-Aligned Duplication",
        180,
    ),
    (
        "c19_normal_offset_duplication",
        "Normal-Offset Duplication",
        190,
    ),
    (
        "c20_curvature_driven_modulation",
        "Curvature-Driven Modulation",
        200,
    ),
    ("c21_seeded_normal_jitter", "Seeded Normal Jitter", 210),
    ("c22_loop_cusp_mode", "Loop / Cusp Mode", 220),
    ("c23_curve_a_b_morph", "Curve A→B Morph", 230),
    (
        "c24_recursive_branch_curves",
        "Recursive Branch Curves",
        240,
    ),
    ("c25_ribbon_fill_from_curve", "Ribbon Fill From Curve", 250),
];

const SIDES: &[&str] = &[
    "Left of its travel (above)",
    "Centered",
    "Right of its travel (below)",
];
const MIRRORS: &[&str] = &[
    "Across the box's middle pitch",
    "Across the box's middle time",
    "Both (through the box's center)",
];
const MIRROR_MODES: &[&str] = &["Keep the curve and add its mirror", "Mirror only"];
const LOOPS: &[&str] = &["Loops", "Cusps"];
const MORPH_TO: &[&str] = &[
    "Its mirror across the box's middle pitch",
    "The straight line between its ends",
    "The box's diagonal (start low, end high)",
];
const RIBBON_TO: &[&str] = &[
    "A ribbon along the curve",
    "The box's bottom",
    "The box's top",
];

/// Most points of every materialized curve together.
pub const PATH_CAP: u64 = 1 << 18;
/// Most copies (branches × tangent copies × normal copies × mirrors).
pub const COPY_CAP: u64 = 4096;
/// Most segments drawn (copies × the curves' segments, and the pieces a
/// changing brush cuts them into): 2²² — about 1,000 copies of a curve at
/// full resolution, at most ~0.4 s of counting (S55: 2²⁴ let one request
/// take 9 s).
pub const WORK_CAP: u64 = 1 << 22;
/// Most row scans of bands, ribbons and fills (their segments and pieces
/// times the rows each one scans): 2²⁶.
pub const SCAN_CAP: u64 = 1 << 26;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const ARCS_HELP: &[(&str, &str)] = &[
    (
        "c01_circular_arc",
        "Draws part of a circle around the box's center instead of the base curve.",
    ),
    (
        "c01_circular_arc.start",
        "The angle the arc starts at, in degrees (0 = later in time from the center, 90 = above it).",
    ),
    (
        "c01_circular_arc.sweep",
        "How far around the arc goes, in degrees; negative goes clockwise.",
    ),
    (
        "c02_elliptical_arc",
        "Draws part of an ellipse around the box's center instead of the base curve.",
    ),
    (
        "c02_elliptical_arc.x",
        "How far the ellipse reaches across in time, in resolution steps.",
    ),
    (
        "c02_elliptical_arc.y",
        "How far the ellipse reaches up and down, in rows.",
    ),
    (
        "c02_elliptical_arc.rotation",
        "Turns the ellipse about the box's center, in degrees.",
    ),
    (
        "c02_elliptical_arc.start",
        "The angle the arc starts at, in degrees.",
    ),
    (
        "c03_quadratic_bezier",
        "Draws a smooth curve bent toward one control point instead of the base curve.",
    ),
    (
        "c03_quadratic_bezier.x",
        "Where the control point lies in time, as a share of the box (it may lie outside it).",
    ),
    (
        "c04_cubic_bezier",
        "Draws a smooth curve shaped by two control points instead of the base curve.",
    ),
    (
        "c04_cubic_bezier.x1",
        "Where the first control point lies in time, as a share of the box.",
    ),
    (
        "c04_cubic_bezier.y1",
        "Where the first control point lies in pitch, as a share of the box.",
    ),
    (
        "c04_cubic_bezier.x2",
        "Where the second control point lies in time, as a share of the box.",
    ),
    (
        "c05_multi_point_spline",
        "Draws a smooth curve pulled toward several points you place.",
    ),
    (
        "c06_catmull_rom_smooth_path",
        "Draws a smooth curve that passes through every point you place.",
    ),
    (
        "c07_curvature_tension",
        "Straightens the curve toward the line between its ends, or bends it further.",
    ),
    (
        "c08_stroke_thickness",
        "Draws the curve several rows thick.",
    ),
    (
        "c08_stroke_thickness.rows",
        "How many rows thick the curve is.",
    ),
    (
        "c09_parallel_curves",
        "Draws several curves that keep the same distance from each other.",
    ),
    (
        "c09_parallel_curves.count",
        "How many curves are drawn in all, the original included.",
    ),
    (
        "c09_parallel_curves.spacing",
        "The distance between neighboring curves, in grid units.",
    ),
    (
        "c10_adaptive_note_sampling",
        "Places separate notes along the curve, closer together where it bends and farther apart where it is straight.",
    ),
    (
        "c10_adaptive_note_sampling.widest",
        "The widest spacing between notes on straight parts, in grid units.",
    ),
    (
        "c11_density_along_curve",
        "Thins the notes along the curve by a curve, so some parts are fuller than others.",
    ),
    (
        "c12_velocity_along_curve",
        "Changes the velocity from the curve's start to its end.",
    ),
    (
        "c12_velocity_along_curve.from",
        "The velocity at the curve's start.",
    ),
    (
        "c12_velocity_along_curve.to",
        "The velocity at the curve's end.",
    ),
    (
        "c12_velocity_along_curve.curve",
        "How the velocity changes from the start (left) to the end (right).",
    ),
    (
        "c13_gate_along_curve",
        "Changes the note lengths from the curve's start to its end.",
    ),
    (
        "c13_gate_along_curve.from",
        "The gate (share of each note's length) at the curve's start.",
    ),
    (
        "c13_gate_along_curve.to",
        "The gate (share of each note's length) at the curve's end.",
    ),
    (
        "c13_gate_along_curve.curve",
        "How the gate changes from the start (left) to the end (right).",
    ),
    (
        "c14_subchannel_along_curve",
        "Steps through the track's subchannels (colors) from the curve's start to its end.",
    ),
    (
        "c14_subchannel_along_curve.from",
        "The subchannel at the curve's start, by its position in the track's list (0 = the first).",
    ),
    (
        "c14_subchannel_along_curve.to",
        "The subchannel at the curve's end, by its position in the track's list.",
    ),
    (
        "c14_subchannel_along_curve.curve",
        "How the subchannel changes from the start (left) to the end (right).",
    ),
    (
        "c15_dash_pattern",
        "Breaks the curve into dashes measured along its length.",
    ),
    (
        "c15_dash_pattern.dash",
        "How long each dash is along the curve, in grid units.",
    ),
    (
        "c15_dash_pattern.gap",
        "How long each gap is along the curve, in grid units.",
    ),
    (
        "c15_dash_pattern.phase",
        "Shifts where the dashes start along the curve, in grid units.",
    ),
    ("c17_mirror", "Mirrors the curve across the box's middle."),
    (
        "c17_mirror.across",
        "Mirror across the middle pitch (upside down), the middle time (backward), or both.",
    ),
    (
        "c17_mirror.mode",
        "Keep the Curve and Add Its Mirror draws both; Mirror Only draws just the mirrored curve.",
    ),
    (
        "c18_tangent_aligned_duplication",
        "Chains copies of the curve end to start, each turned to continue the previous one's direction.",
    ),
    (
        "c19_normal_offset_duplication",
        "Adds whole copies of the curve moved sideways from it.",
    ),
    (
        "c19_normal_offset_duplication.count",
        "How many curves are drawn in all, the original included.",
    ),
    (
        "c20_curvature_driven_modulation",
        "Makes the notes louder (and a thick stroke thicker) where the curve bends.",
    ),
    (
        "c20_curvature_driven_modulation.floor",
        "The velocity (and thickness) on straight parts, as a share of the full value.",
    ),
    (
        "c21_seeded_normal_jitter",
        "Shakes the curve sideways at random points along it (seeded), keeping its ends.",
    ),
    (
        "c21_seeded_normal_jitter.amount",
        "The largest sideways move, in grid units.",
    ),
    (
        "c22_loop_cusp_mode",
        "Rolls a circle along the curve and draws a point on it, giving loops or cusps.",
    ),
    (
        "c22_loop_cusp_mode.mode",
        "Loops draws a loop each turn; Cusps draws a sharp point where the circle touches the curve.",
    ),
    (
        "c22_loop_cusp_mode.turns",
        "How many times the circle rolls around along the curve.",
    ),
    (
        "c23_curve_a_b_morph",
        "Draws in-between curves that turn the curve gradually into a second one.",
    ),
    (
        "c23_curve_a_b_morph.count",
        "How many curves are drawn, the first and the second included.",
    ),
    (
        "c23_curve_a_b_morph.to",
        "The second curve: the first one's mirror, the straight line between its ends, or the box's diagonal.",
    ),
    (
        "c24_recursive_branch_curves",
        "Grows smaller copies of the curve from points along it, like branches (seeded).",
    ),
    (
        "c24_recursive_branch_curves.branches",
        "How many branches grow from each curve.",
    ),
    (
        "c24_recursive_branch_curves.depth",
        "How many levels of branches on branches are drawn.",
    ),
    (
        "c24_recursive_branch_curves.size",
        "Each branch's size as a share of the curve it grows from.",
    ),
    (
        "c25_ribbon_fill_from_curve",
        "Fills an area along the curve with notes.",
    ),
    (
        "c25_ribbon_fill_from_curve.to",
        "What is filled: a ribbon along the curve, or everything down to the box's bottom or up to its top.",
    ),
];

fn arcs_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        // c01
        flag("c01_circular_arc", "Circular Arc", false),
        float("c01_circular_arc.radius", "Radius (grid units)", 0.5, 4096.0, 8.0, "")
            .help("A circle's arc about the box's center, round on the grid (as many steps across as rows up)."),
        float("c01_circular_arc.start", "Start angle", -360.0, 360.0, 180.0, "°"),
        float("c01_circular_arc.sweep", "Sweep (negative: clockwise)", -360.0, 360.0, -180.0, "°"),
        // c02
        flag("c02_elliptical_arc", "Elliptical Arc", false),
        float("c02_elliptical_arc.x", "Radius across (steps)", 0.5, 4096.0, 16.0, ""),
        float("c02_elliptical_arc.y", "Radius up (rows)", 0.5, 4096.0, 6.0, ""),
        float("c02_elliptical_arc.rotation", "Rotation", -180.0, 180.0, 0.0, "°"),
        float("c02_elliptical_arc.start", "Start angle", -360.0, 360.0, 180.0, "°"),
        float("c02_elliptical_arc.sweep", "Sweep (negative: clockwise)", -360.0, 360.0, -180.0, "°")
            .help("An ellipse's arc about the box's center, turned on the grid."),
        // c03
        flag("c03_quadratic_bezier", "Quadratic Bezier", false),
        float("c03_quadratic_bezier.x", "Control time (% of the box)", -100.0, 200.0, 0.0, "%"),
        float("c03_quadratic_bezier.y", "Control pitch (% of the box)", -100.0, 200.0, 100.0, "%")
            .help("From the box's start at its lowest pitch to its end at its highest, pulled towards the control point."),
        // c04
        flag("c04_cubic_bezier", "Cubic Bezier", false),
        float("c04_cubic_bezier.x1", "First control time (%)", -100.0, 200.0, 50.0, "%"),
        float("c04_cubic_bezier.y1", "First control pitch (%)", -100.0, 200.0, 0.0, "%"),
        float("c04_cubic_bezier.x2", "Second control time (%)", -100.0, 200.0, 50.0, "%"),
        float("c04_cubic_bezier.y2", "Second control pitch (%)", -100.0, 200.0, 100.0, "%")
            .help("From the box's start at its lowest pitch to its end at its highest, shaped by two control points."),
        // c05
        flag("c05_multi_point_spline", "Multi-Point Spline", false),
        curve(
            "c05_multi_point_spline.points",
            "Points (time across, pitch up)",
            Curve::new(vec![(0.0, 0.0), (0.25, 0.8), (0.5, 0.2), (0.75, 0.9), (1.0, 0.4)]),
        )
        .help("A smooth B-spline pulled towards the points, through the first and the last."),
        // c06
        flag("c06_catmull_rom_smooth_path", "Catmull-Rom / Smooth Path", false),
        curve(
            "c06_catmull_rom_smooth_path.points",
            "Points (time across, pitch up)",
            Curve::new(vec![(0.0, 0.1), (0.33, 0.9), (0.66, 0.2), (1.0, 0.8)]),
        )
        .help("A smooth curve through every point."),
        // c07
        flag("c07_curvature_tension", "Curvature Tension", false),
        float("c07_curvature_tension.amount", "Tension", -100.0, 100.0, 50.0, "%")
            .help("100 %: the straight line between the curve's ends; negative: bent further (−100 % doubles the bend)."),
        // c08
        flag("c08_stroke_thickness", "Stroke Thickness", false),
        int("c08_stroke_thickness.rows", "Thickness (rows)", 2, 32, 3),
        // c09
        flag("c09_parallel_curves", "Parallel Curves", false),
        int("c09_parallel_curves.count", "Curves", 2, 32, 3),
        float("c09_parallel_curves.spacing", "Spacing (grid units)", 0.5, 256.0, 2.0, ""),
        choice("c09_parallel_curves.side", "Side", SIDES, 0)
            .help("Curves offset along the normal: every point the same distance from the curve."),
        // c10
        flag("c10_adaptive_note_sampling", "Adaptive Note Sampling", false),
        float("c10_adaptive_note_sampling.widest", "Widest spacing (grid units)", 1.0, 256.0, 6.0, ""),
        float("c10_adaptive_note_sampling.radius", "Dense below this bend radius (grid units)", 0.5, 4096.0, 4.0, "")
            .help("A one-step note at points along the curve: every unit where it bends tightly, up to the widest spacing where it is straight."),
        // c11
        flag("c11_density_along_curve", "Density Along Curve", false),
        curve(
            "c11_density_along_curve.curve",
            "Share kept along the curve",
            Curve::new(vec![(0.0, 1.0), (0.5, 0.2), (1.0, 1.0)]),
        )
        .help("Notes at points along the curve (every unit, or the adaptive spacing), kept evenly by the share."),
        // c12
        flag("c12_velocity_along_curve", "Velocity Along Curve", false),
        int("c12_velocity_along_curve.from", "Velocity at the start", 1, 127, 127),
        int("c12_velocity_along_curve.to", "Velocity at the end", 1, 127, 40),
        curve("c12_velocity_along_curve.curve", "Shape (start → end)", Curve::identity()),
        // c13
        flag("c13_gate_along_curve", "Gate Along Curve", false),
        float("c13_gate_along_curve.from", "Gate at the start", 1.0, 100.0, 100.0, "%"),
        float("c13_gate_along_curve.to", "Gate at the end", 1.0, 100.0, 25.0, "%"),
        curve("c13_gate_along_curve.curve", "Shape (start → end)", Curve::identity()),
        // c14
        flag("c14_subchannel_along_curve", "Subchannel Along Curve", false),
        int("c14_subchannel_along_curve.from", "Subchannel at the start (position in the track)", 0, 4095, 0),
        int("c14_subchannel_along_curve.to", "Subchannel at the end (position in the track)", 0, 4095, 3),
        curve("c14_subchannel_along_curve.curve", "Shape (start → end)", Curve::identity()),
        // c15
        flag("c15_dash_pattern", "Dash Pattern", false),
        float("c15_dash_pattern.dash", "Dash (grid units along the curve)", 0.5, 4096.0, 3.0, ""),
        float("c15_dash_pattern.gap", "Gap (grid units)", 0.5, 4096.0, 2.0, ""),
        float("c15_dash_pattern.phase", "Phase (grid units)", 0.0, 4096.0, 0.0, ""),
        // c16
        flag("c16_reverse_direction", "Reverse Direction", false)
            .help("The curve played backwards: mirrored in time about the box's middle, its start (gradients, dashes) now at its end in time."),
        // c17
        flag("c17_mirror", "Mirror", false),
        choice("c17_mirror.across", "Across", MIRRORS, 0),
        choice("c17_mirror.mode", "Mode", MIRROR_MODES, 0),
        // c18
        flag("c18_tangent_aligned_duplication", "Tangent-Aligned Duplication", false),
        int("c18_tangent_aligned_duplication.count", "Curves in the chain", 2, 32, 4)
            .help("The curve repeated end to start, each copy turned so it continues the previous one's direction."),
        // c19
        flag("c19_normal_offset_duplication", "Normal-Offset Duplication", false),
        int("c19_normal_offset_duplication.count", "Copies", 2, 32, 3),
        float("c19_normal_offset_duplication.distance", "Distance (grid units)", 0.5, 4096.0, 4.0, "")
            .help("Whole copies moved across the line between the curve's ends (to its left), every distance."),
        // c20
        flag("c20_curvature_driven_modulation", "Curvature-Driven Modulation", false),
        float("c20_curvature_driven_modulation.floor", "On straight parts", 0.0, 100.0, 30.0, "%"),
        float("c20_curvature_driven_modulation.radius", "Full at this bend radius (grid units)", 0.5, 4096.0, 4.0, "")
            .help("Velocity (and a thick stroke's thickness) follows the bend: the floor where the curve is straight, full where it bends at the radius or tighter."),
        // c21
        flag("c21_seeded_normal_jitter", "Seeded Normal Jitter", false),
        float("c21_seeded_normal_jitter.amount", "Amount (grid units)", 0.0, 256.0, 1.5, ""),
        int("c21_seeded_normal_jitter.points", "Points", 4, 4096, 48)
            .help("Seeded: the curve moved along its normal at points evenly along it (its ends kept), straight between them."),
        // c22
        flag("c22_loop_cusp_mode", "Loop / Cusp Mode", false),
        choice("c22_loop_cusp_mode.mode", "Mode", LOOPS, 0),
        int("c22_loop_cusp_mode.turns", "Turns along the curve", 1, 256, 8),
        float("c22_loop_cusp_mode.size", "Loop size (% of the rolling circle)", 101.0, 400.0, 160.0, "%")
            .help("A point on a circle rolling along the curve (on its left): a loop each turn, or a cusp where it touches the curve."),
        // c23
        flag("c23_curve_a_b_morph", "Curve A→B Morph", false),
        int("c23_curve_a_b_morph.count", "Curves (A and B included)", 2, 32, 5),
        choice("c23_curve_a_b_morph.to", "B", MORPH_TO, 0),
        // c24
        flag("c24_recursive_branch_curves", "Recursive Branch Curves", false),
        int("c24_recursive_branch_curves.branches", "Branches per curve", 1, 8, 2),
        int("c24_recursive_branch_curves.depth", "Depth", 1, 4, 2),
        float("c24_recursive_branch_curves.size", "Size (% of the curve it grows from)", 10.0, 90.0, 55.0, "%"),
        float("c24_recursive_branch_curves.angle", "Angle", 0.0, 90.0, 35.0, "°")
            .help("Seeded: smaller copies of the curve grow from points along it, turned left and right, and from those."),
        // c25
        flag("c25_ribbon_fill_from_curve", "Ribbon Fill From Curve", false),
        choice("c25_ribbon_fill_from_curve.to", "Fill", RIBBON_TO, 0),
        float("c25_ribbon_fill_from_curve.width", "Ribbon width (grid units)", 0.5, 256.0, 4.0, "")
            .help("Filled by scanline: a ribbon along the curve (to its left), or the region down to the box's bottom or up to its top."),
    ];
    with_help(&mut own, ARCS_HELP);
    super::spec(
        "gen.arcs",
        "Arcs / Curves",
        "Generates curves of notes: a quarter ellipse rising across the generation box (where, length and pitch range above), one note per row it crosses on the resolution grid.",
        own,
        FUNCTIONS,
    )
}

/// Which curve is drawn (grid units).
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// Centre, radii, rotation, start angle, sweep (radians).
    Arc {
        c: (f64, f64),
        r: (f64, f64),
        rot: f64,
        a0: f64,
        sweep: f64,
    },
    /// Control points as box fractions.
    Quad((f64, f64)),
    Cubic((f64, f64), (f64, f64)),
    /// Points as box fractions.
    BSpline(Vec<(f64, f64)>),
    CatmullRom(Vec<(f64, f64)>),
}

/// The functions' settings (`None` / neutral = off).
#[derive(Clone, Debug, Default)]
struct Cfg {
    /// c01: radius, start, sweep (radians).
    circular: Option<(f64, f64, f64)>,
    /// c02: radii, rotation, start, sweep.
    elliptical: Option<(f64, f64, f64, f64, f64)>,
    /// c03, c04 (box fractions).
    quad: Option<(f64, f64)>,
    cubic: Option<((f64, f64), (f64, f64))>,
    /// c05, c06.
    bspline: Option<Vec<(f64, f64)>>,
    catmull: Option<Vec<(f64, f64)>>,
    /// The last of c01 – c06 switched on (0 = the base).
    kind: u8,
    /// c07: fraction (−1 … 1).
    tension: Option<f64>,
    /// c08: rows.
    thick: Option<f64>,
    /// c09: count, spacing, side (0 left, 1 centred, 2 right).
    parallel: Option<(u32, f64, usize)>,
    /// c10: widest spacing, radius.
    adaptive: Option<(f64, f64)>,
    /// c11.
    density: Option<Curve>,
    /// c12 velocity, c13 gate (fractions), c14 subchannel: from, to, shape.
    vel: Option<(f64, f64, Curve)>,
    gate: Option<(f64, f64, Curve)>,
    sub: Option<(f64, f64, Curve)>,
    /// c15: dash, gap, phase (grid units).
    dash: Option<(f64, f64, f64)>,
    /// c16.
    reverse: bool,
    /// c17: across (0 pitch, 1 time, 2 both), keep the curve.
    mirror: Option<(usize, bool)>,
    /// c18: curves in the chain.
    chain: Option<u32>,
    /// B2-F01: the copy expansions c18 (chain), c19 (normal offsets), c17
    /// (mirror) in the user's order (`order::ARCS_COPIES`; empty = declared).
    copy_order: Vec<&'static str>,
    /// c19: copies, distance.
    normal: Option<(u32, f64)>,
    /// c20: floor (fraction), radius.
    modulate: Option<(f64, f64)>,
    /// c21: amount, points.
    jitter: Option<(f64, usize)>,
    /// c22: cusps, turns, size (fraction).
    trochoid: Option<(bool, u32, f64)>,
    /// c23: curves, to (0 mirror, 1 chord, 2 diagonal).
    morph: Option<(u32, usize)>,
    /// c24: branches, depth, size (fraction), angle (radians).
    branch: Option<(u32, u32, f64, f64)>,
    /// c25: to (0 ribbon, 1 bottom, 2 top), width.
    ribbon: Option<(usize, f64)>,
}

impl Cfg {
    fn read(p: &Params) -> Cfg {
        let on = |id: &str| p.flag(id);
        let pct = |k: &str| p.float(k) / 100.0;
        let pts = |k: &str| -> Vec<(f64, f64)> { p.curve(k).points };
        let mut c = Cfg::default();
        if on("c01_circular_arc") {
            c.circular = Some((
                p.float("c01_circular_arc.radius"),
                p.float("c01_circular_arc.start").to_radians(),
                p.float("c01_circular_arc.sweep").to_radians(),
            ));
            c.kind = 1;
        }
        if on("c02_elliptical_arc") {
            c.elliptical = Some((
                p.float("c02_elliptical_arc.x"),
                p.float("c02_elliptical_arc.y"),
                p.float("c02_elliptical_arc.rotation").to_radians(),
                p.float("c02_elliptical_arc.start").to_radians(),
                p.float("c02_elliptical_arc.sweep").to_radians(),
            ));
            c.kind = 2;
        }
        if on("c03_quadratic_bezier") {
            c.quad = Some((pct("c03_quadratic_bezier.x"), pct("c03_quadratic_bezier.y")));
            c.kind = 3;
        }
        if on("c04_cubic_bezier") {
            c.cubic = Some((
                (pct("c04_cubic_bezier.x1"), pct("c04_cubic_bezier.y1")),
                (pct("c04_cubic_bezier.x2"), pct("c04_cubic_bezier.y2")),
            ));
            c.kind = 4;
        }
        if on("c05_multi_point_spline") {
            c.bspline = Some(pts("c05_multi_point_spline.points"));
            c.kind = 5;
        }
        if on("c06_catmull_rom_smooth_path") {
            c.catmull = Some(pts("c06_catmull_rom_smooth_path.points"));
            c.kind = 6;
        }
        if on("c07_curvature_tension") {
            c.tension = Some(pct("c07_curvature_tension.amount").clamp(-1.0, 1.0));
        }
        if on("c08_stroke_thickness") {
            c.thick = Some(p.int("c08_stroke_thickness.rows").clamp(2, 32) as f64);
        }
        if on("c09_parallel_curves") {
            c.parallel = Some((
                p.int("c09_parallel_curves.count").clamp(2, 32) as u32,
                p.float("c09_parallel_curves.spacing").max(0.5),
                p.choice("c09_parallel_curves.side").min(2),
            ));
        }
        if on("c10_adaptive_note_sampling") {
            c.adaptive = Some((
                p.float("c10_adaptive_note_sampling.widest").max(1.0),
                p.float("c10_adaptive_note_sampling.radius").max(0.5),
            ));
        }
        if on("c11_density_along_curve") {
            c.density = Some(p.curve("c11_density_along_curve.curve"));
        }
        if on("c12_velocity_along_curve") {
            c.vel = Some((
                p.int("c12_velocity_along_curve.from") as f64,
                p.int("c12_velocity_along_curve.to") as f64,
                p.curve("c12_velocity_along_curve.curve"),
            ));
        }
        if on("c13_gate_along_curve") {
            c.gate = Some((
                pct("c13_gate_along_curve.from"),
                pct("c13_gate_along_curve.to"),
                p.curve("c13_gate_along_curve.curve"),
            ));
        }
        if on("c14_subchannel_along_curve") {
            c.sub = Some((
                p.int("c14_subchannel_along_curve.from") as f64,
                p.int("c14_subchannel_along_curve.to") as f64,
                p.curve("c14_subchannel_along_curve.curve"),
            ));
        }
        if on("c15_dash_pattern") {
            c.dash = Some((
                p.float("c15_dash_pattern.dash").max(0.5),
                p.float("c15_dash_pattern.gap").max(0.5),
                p.float("c15_dash_pattern.phase").max(0.0),
            ));
        }
        c.reverse = on("c16_reverse_direction");
        if on("c17_mirror") {
            c.mirror = Some((
                p.choice("c17_mirror.across").min(2),
                p.choice("c17_mirror.mode") == 0,
            ));
        }
        if on("c18_tangent_aligned_duplication") {
            c.chain = Some(p.int("c18_tangent_aligned_duplication.count").clamp(2, 32) as u32);
        }
        c.copy_order = p.sequence(crate::transform::order::ARCS_COPIES.members);
        if on("c19_normal_offset_duplication") {
            c.normal = Some((
                p.int("c19_normal_offset_duplication.count").clamp(2, 32) as u32,
                p.float("c19_normal_offset_duplication.distance").max(0.5),
            ));
        }
        if on("c20_curvature_driven_modulation") {
            c.modulate = Some((
                pct("c20_curvature_driven_modulation.floor").clamp(0.0, 1.0),
                p.float("c20_curvature_driven_modulation.radius").max(0.5),
            ));
        }
        if on("c21_seeded_normal_jitter") {
            c.jitter = Some((
                p.float("c21_seeded_normal_jitter.amount").max(0.0),
                p.int("c21_seeded_normal_jitter.points").clamp(4, 4096) as usize,
            ));
        }
        if on("c22_loop_cusp_mode") {
            c.trochoid = Some((
                p.choice("c22_loop_cusp_mode.mode") == 1,
                p.int("c22_loop_cusp_mode.turns").clamp(1, 256) as u32,
                pct("c22_loop_cusp_mode.size").clamp(1.01, 4.0),
            ));
        }
        if on("c23_curve_a_b_morph") {
            c.morph = Some((
                p.int("c23_curve_a_b_morph.count").clamp(2, 32) as u32,
                p.choice("c23_curve_a_b_morph.to").min(2),
            ));
        }
        if on("c24_recursive_branch_curves") {
            c.branch = Some((
                p.int("c24_recursive_branch_curves.branches").clamp(1, 8) as u32,
                p.int("c24_recursive_branch_curves.depth").clamp(1, 4) as u32,
                pct("c24_recursive_branch_curves.size").clamp(0.1, 0.9),
                p.float("c24_recursive_branch_curves.angle").to_radians(),
            ));
        }
        if on("c25_ribbon_fill_from_curve") {
            c.ribbon = Some((
                p.choice("c25_ribbon_fill_from_curve.to").min(2),
                p.float("c25_ribbon_fill_from_curve.width").max(0.5),
            ));
        }
        c
    }
}

/// The curves' box in grid units: from the box's start to just short of
/// its end (10⁻⁶ of a step, or a millionth of a millionth of the position
/// when that is more — a curve ending upright there draws in the box's
/// last step, the pen putting an upright piece in the cell containing it),
/// from the lowest row's centre to the highest's.
#[derive(Clone, Copy, Debug)]
struct Gbox {
    u0: f64,
    u1: f64,
    v0: f64,
    v1: f64,
}

impl Gbox {
    fn of(g: &BoxG) -> Gbox {
        let u0 = g.x0 / g.step;
        let end = g.x1 / g.step;
        Gbox {
            u0,
            u1: (end - (end.abs() * 1e-12).max(1e-6)).max(u0),
            v0: g.y0,
            v1: g.y1,
        }
    }

    /// A point given as box fractions.
    fn at(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            self.u0 + (self.u1 - self.u0) * x,
            self.v0 + (self.v1 - self.v0) * y,
        )
    }

    fn centre(&self) -> (f64, f64) {
        ((self.u0 + self.u1) / 2.0, (self.v0 + self.v1) / 2.0)
    }
}

type P2 = (f64, f64);

fn lerp2(a: P2, b: P2, t: f64) -> P2 {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn dist(a: P2, b: P2) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// The curve of kind `k` at parameter `t` (0 – 1), grid units.
fn eval(k: &Kind, b: &Gbox, t: f64) -> P2 {
    match k {
        Kind::Arc {
            c,
            r,
            rot,
            a0,
            sweep,
        } => {
            let a = a0 + sweep * t;
            let (x, y) = (r.0 * a.cos(), r.1 * a.sin());
            let (s, co) = rot.sin_cos();
            (c.0 + x * co - y * s, c.1 + x * s + y * co)
        }
        Kind::Quad(q) => {
            let (p0, p1, p2) = (b.at((0.0, 0.0)), b.at(*q), b.at((1.0, 1.0)));
            let u = 1.0 - t;
            (
                u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
                u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
            )
        }
        Kind::Cubic(q1, q2) => {
            let (p0, p1, p2, p3) = (b.at((0.0, 0.0)), b.at(*q1), b.at(*q2), b.at((1.0, 1.0)));
            let u = 1.0 - t;
            let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            (
                w0 * p0.0 + w1 * p1.0 + w2 * p2.0 + w3 * p3.0,
                w0 * p0.1 + w1 * p1.1 + w2 * p2.1 + w3 * p3.1,
            )
        }
        Kind::BSpline(pts) => bspline(&pts.iter().map(|&p| b.at(p)).collect::<Vec<_>>(), t),
        Kind::CatmullRom(pts) => catmull(&pts.iter().map(|&p| b.at(p)).collect::<Vec<_>>(), t),
    }
}

/// The clamped uniform cubic B-spline of control points `p` at `t`
/// (through the first and the last).
/// Uses the native generator's repeated end controls and uniform parameter spans.
/// Callers exposing untrusted input must bound/validate controls and parameter.
pub fn bspline(p: &[(f64, f64)], t: f64) -> (f64, f64) {
    match p.len() {
        0 => (0.0, 0.0),
        1 => p[0],
        _ => {
            // The ends tripled: the curve starts and ends on them.
            let n = p.len();
            let q = |i: isize| p[i.clamp(0, n as isize - 1) as usize];
            let segs = n + 1;
            let x = t.clamp(0.0, 1.0) * segs as f64;
            let i = (x.floor() as usize).min(segs - 1);
            let u = x - i as f64;
            let (a, b, c, d) = (
                q(i as isize - 2),
                q(i as isize - 1),
                q(i as isize),
                q(i as isize + 1),
            );
            let u2 = u * u;
            let u3 = u2 * u;
            let w0 = (1.0 - u).powi(3) / 6.0;
            let w1 = (3.0 * u3 - 6.0 * u2 + 4.0) / 6.0;
            let w2 = (-3.0 * u3 + 3.0 * u2 + 3.0 * u + 1.0) / 6.0;
            let w3 = u3 / 6.0;
            (
                w0 * a.0 + w1 * b.0 + w2 * c.0 + w3 * d.0,
                w0 * a.1 + w1 * b.1 + w2 * c.1 + w3 * d.1,
            )
        }
    }
}

/// The uniform Catmull-Rom spline through `p` at `t` (the ends extended by
/// reflection).
/// Uses the native generator's reflected end controls and uniform parameter spans.
/// Callers exposing untrusted input must bound/validate controls and parameter.
pub fn catmull(p: &[(f64, f64)], t: f64) -> (f64, f64) {
    match p.len() {
        0 => (0.0, 0.0),
        1 => p[0],
        n => {
            let q = |i: isize| -> P2 {
                if i < 0 {
                    (2.0 * p[0].0 - p[1].0, 2.0 * p[0].1 - p[1].1)
                } else if i as usize >= n {
                    (2.0 * p[n - 1].0 - p[n - 2].0, 2.0 * p[n - 1].1 - p[n - 2].1)
                } else {
                    p[i as usize]
                }
            };
            let segs = n - 1;
            let x = t.clamp(0.0, 1.0) * segs as f64;
            let i = (x.floor() as usize).min(segs - 1);
            let u = x - i as f64;
            let (a, b, c, d) = (
                q(i as isize - 1),
                q(i as isize),
                q(i as isize + 1),
                q(i as isize + 2),
            );
            let u2 = u * u;
            let u3 = u2 * u;
            let f = |a: f64, b: f64, c: f64, d: f64| {
                0.5 * (2.0 * b
                    + (-a + c) * u
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * u2
                    + (-a + 3.0 * b - 3.0 * c + d) * u3)
            };
            (f(a.0, b.0, c.0, d.0), f(a.1, b.1, c.1, d.1))
        }
    }
}

/// Positions along a polyline (0 – 1 by length; by index when it has none).
fn with_s(pts: &[P2]) -> Vec<Pt> {
    let mut cum = vec![0.0; pts.len()];
    for i in 1..pts.len() {
        cum[i] = cum[i - 1] + dist(pts[i - 1], pts[i]);
    }
    let total = cum.last().copied().unwrap_or(0.0);
    let n = pts.len();
    pts.iter()
        .enumerate()
        .map(|(i, &(x, y))| Pt {
            x,
            y,
            s: if total > 0.0 {
                cum[i] / total
            } else if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            },
        })
        .collect()
}

/// The unit left normals at the points (the travel turned a quarter
/// counter-clockwise; the neighbours' chord, the ends' own segment).
fn normals(p: &[Pt]) -> Vec<P2> {
    let n = p.len();
    (0..n)
        .map(|i| {
            let (a, b) = (p[i.saturating_sub(1)], p[(i + 1).min(n - 1)]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l = dx.hypot(dy);
            if l > 0.0 && l.is_finite() {
                (-dy / l, dx / l)
            } else {
                (0.0, 1.0)
            }
        })
        .collect()
}

/// The polyline moved by `d` along its normals.
fn offset(p: &[Pt], d: f64) -> Vec<Pt> {
    normals(p)
        .iter()
        .zip(p)
        .map(|(&(nx, ny), q)| Pt {
            x: q.x + nx * d,
            y: q.y + ny * d,
            s: q.s,
        })
        .collect()
}

/// The direction (radians) of the first / last segment with a length.
fn ends(p: &[Pt]) -> (f64, f64) {
    let dir = |a: Pt, b: Pt| (b.y - a.y).atan2(b.x - a.x);
    let first = p
        .windows(2)
        .find(|w| !w[0].near(w[1]))
        .map_or(0.0, |w| dir(w[0], w[1]));
    let last = p
        .windows(2)
        .rev()
        .find(|w| !w[0].near(w[1]))
        .map_or(first, |w| dir(w[0], w[1]));
    (first, last)
}

/// The total turn of a polyline (radians).
fn turning(p: &[Pt]) -> f64 {
    let mut total = 0.0;
    for w in p.windows(3) {
        let t0 = (w[1].y - w[0].y).atan2(w[1].x - w[0].x);
        let t1 = (w[2].y - w[1].y).atan2(w[2].x - w[1].x);
        total += ((t1 - t0 + TAU / 2.0).rem_euclid(TAU) - TAU / 2.0).abs();
    }
    total
}

/// A copy of the curves: its map (grid space → note space), whether it
/// turns them, how much it scales them.
#[derive(Clone, Copy, Debug)]
struct Copy {
    m: Aff,
    rotated: bool,
    scale: f64,
}

/// What is built once per kernel (on the worker): the curves in grid space,
/// the copies, how the curves are drawn.
struct Built {
    paths: Vec<Vec<Pt>>,
    copies: Vec<Copy>,
    style: PathStyle,
}

/// The Arcs / Curves generator.
struct Arcs {
    cfg: Cfg,
    built: OnceLock<Built>,
}

/// A similarity on the grid: `p ↦ to + k · R(t) (p − from)`.
fn similar(from: P2, to: P2, t: f64, k: f64) -> Aff {
    let (s, c) = t.sin_cos();
    Aff::translate(-from.0, -from.1)
        .then(Aff {
            a: k * c,
            b: -k * s,
            c: k * s,
            d: k * c,
            e: 0.0,
            f: 0.0,
        })
        .then(Aff::translate(to.0, to.1))
}

impl Arcs {
    fn new(cfg: Cfg) -> Arcs {
        Arcs {
            cfg,
            built: OnceLock::new(),
        }
    }

    /// The curve's kind (the last of c01 – c06 switched on; the base).
    fn kind(&self, b: &Gbox) -> Kind {
        let c = &self.cfg;
        let centre = b.centre();
        match c.kind {
            1 => {
                let (r, a0, sweep) = c.circular.unwrap_or((8.0, 0.0, 0.0));
                Kind::Arc {
                    c: centre,
                    r: (r, r),
                    rot: 0.0,
                    a0,
                    sweep,
                }
            }
            2 => {
                let (rx, ry, rot, a0, sweep) = c.elliptical.unwrap_or_default();
                Kind::Arc {
                    c: centre,
                    r: (rx, ry),
                    rot,
                    a0,
                    sweep,
                }
            }
            3 => Kind::Quad(c.quad.unwrap_or((0.0, 1.0))),
            4 => {
                let (a, z) = c.cubic.unwrap_or(((0.5, 0.0), (0.5, 1.0)));
                Kind::Cubic(a, z)
            }
            5 => Kind::BSpline(c.bspline.clone().unwrap_or_default()),
            6 => Kind::CatmullRom(c.catmull.clone().unwrap_or_default()),
            // The base: the quarter of the ellipse centred on the box's
            // top-left corner from its bottom to its right.
            _ => Kind::Arc {
                c: (b.u0, b.v1),
                r: (b.u1 - b.u0, b.v1 - b.v0),
                rot: 0.0,
                a0: -TAU / 4.0,
                sweep: TAU / 4.0,
            },
        }
    }

    /// The trunk: the curve (with its tension) flattened, reshaped (c22,
    /// c21, c16), with positions (grid units); and its directions at its
    /// start and end (radians: the curve's own tangents unless reshaped).
    fn trunk(&self, f: &Frame) -> (Vec<Pt>, (f64, f64)) {
        let c = &self.cfg;
        let b = Gbox::of(&BoxG::of(f));
        let k = self.kind(&b);
        let (p0, p1) = (eval(&k, &b, 0.0), eval(&k, &b, 1.0));
        let tension = c.tension.unwrap_or(0.0);
        let curve = |t: f64| {
            let p = eval(&k, &b, t);
            if tension == 0.0 {
                p
            } else {
                lerp2(p, lerp2(p0, p1, t), tension)
            }
        };
        let mut line = with_s(&flatten(&curve));
        let tangent = |a: f64, b: f64| {
            let (p, q) = (curve(a), curve(b));
            (q.1 - p.1).atan2(q.0 - p.0)
        };
        let h = 1e-7;
        let mut dirs = (tangent(0.0, h), tangent(1.0 - h, 1.0));
        if c.trochoid.is_some() || c.jitter.is_some() {
            dirs = ends(&line);
        }
        if let Some((cusps, turns, size)) = c.trochoid {
            line = trochoid(&line, cusps, turns, size);
        }
        if let Some((amount, points)) = c.jitter {
            let mut rng = Rng::new(f.seed ^ 0x4152_4353_4A49);
            let mut q = resample(&line, points - 1);
            let nrm = normals(&q);
            let n = q.len();
            for (i, p) in q.iter_mut().enumerate() {
                let d = rng.range_f64(-amount, amount);
                if i > 0 && i + 1 < n {
                    p.x += nrm[i].0 * d;
                    p.y += nrm[i].1 * d;
                }
            }
            line = q;
        }
        if c.reverse {
            let mirror = b.u0 + b.u1;
            for p in &mut line {
                p.x = mirror - p.x;
            }
            // Mirrored in time: a direction θ becomes π − θ.
            dirs = (TAU / 2.0 - dirs.0, TAU / 2.0 - dirs.1);
        }
        (line, dirs)
    }

    /// The curves in grid space: the trunk's morphs (c23), each one's
    /// parallels (c09), cut into dashes (c15).
    fn curves(&self, f: &Frame, trunk: &[Pt]) -> Vec<Vec<Pt>> {
        let c = &self.cfg;
        let b = Gbox::of(&BoxG::of(f));
        let mut out: Vec<Vec<Pt>> = Vec::new();
        let morphs: Vec<Vec<Pt>> = match c.morph {
            None => vec![trunk.to_vec()],
            Some((n, to)) => {
                let (a, z) = (
                    trunk.first().copied().unwrap_or(Pt {
                        x: b.u0,
                        y: b.v0,
                        s: 0.0,
                    }),
                    trunk.last().copied().unwrap_or(Pt {
                        x: b.u1,
                        y: b.v1,
                        s: 1.0,
                    }),
                );
                let target = |p: &Pt| -> P2 {
                    match to {
                        0 => (p.x, b.v0 + b.v1 - p.y),
                        1 => lerp2((a.x, a.y), (z.x, z.y), p.s),
                        _ => lerp2((b.u0, b.v0), (b.u1, b.v1), p.s),
                    }
                };
                (0..n)
                    .map(|i| {
                        let w = f64::from(i) / f64::from(n - 1);
                        trunk
                            .iter()
                            .map(|p| {
                                let (x, y) = lerp2((p.x, p.y), target(p), w);
                                Pt { x, y, s: p.s }
                            })
                            .collect()
                    })
                    .collect()
            }
        };
        for m in &morphs {
            match c.parallel {
                None => out.push(m.clone()),
                Some((n, sp, side)) => {
                    let shift = match side {
                        0 => 0.0,
                        1 => -(f64::from(n - 1)) * sp / 2.0,
                        _ => -(f64::from(n - 1)) * sp,
                    };
                    for i in 0..n {
                        out.push(offset(m, shift + f64::from(i) * sp));
                    }
                }
            }
        }
        if let Some((on, gap, phase)) = c.dash {
            out = out
                .iter()
                .flat_map(|p| dashes(p, on, on + gap, phase))
                .collect();
        }
        out
    }

    /// The copies: the branches (c24), each one's tangent chain (c18), each
    /// one's normal copies (c19), each one's mirrors (c17).
    fn copies(&self, f: &Frame, trunk: &[Pt], dirs: (f64, f64)) -> Vec<Copy> {
        let c = &self.cfg;
        let gb = BoxG::of(f);
        let b = Gbox::of(&gb);
        let to_note = Aff {
            a: gb.step,
            ..Aff::ID
        };
        let first = trunk.first().map_or((b.u0, b.v0), |p| (p.x, p.y));
        let last = trunk.last().map_or((b.u1, b.v1), |p| (p.x, p.y));
        let (alpha, beta) = dirs;
        // (map, rotated, scale)
        let mut set: Vec<(Aff, bool, f64)> = vec![(Aff::ID, false, 1.0)];
        if let Some((n, depth, size, angle)) = c.branch {
            let mut rng = Rng::new(f.seed ^ 0x4152_4353_4252);
            let mut level = vec![(Aff::ID, 1.0)];
            for _ in 0..depth {
                let mut next = Vec::new();
                for &(parent, k) in &level {
                    let side0 = rng.below(2);
                    for j in 0..n {
                        let s = rng.range_f64(0.25, 0.85);
                        let (at, dir) = point_at(trunk, s);
                        let side = if (u64::from(j) + side0).is_multiple_of(2) {
                            1.0
                        } else {
                            -1.0
                        };
                        let t = similar(first, at, dir + side * angle - alpha, size).then(parent);
                        next.push((t, k * size));
                    }
                }
                set.extend(next.iter().map(|&(m, k)| (m, true, k)));
                level = next;
            }
        }
        // B2-F01: the copy expansions in the user's order (declared: chain,
        // normal offsets, mirror — each composes onto every copy so far).
        let order: &[&str] = if c.copy_order.is_empty() {
            crate::transform::order::ARCS_COPIES.members
        } else {
            &c.copy_order
        };
        for id in order {
            match *id {
                "c18_tangent_aligned_duplication" => {
                    if let Some(n) = c.chain {
                        let step = similar(first, last, beta - alpha, 1.0);
                        let mut out = Vec::new();
                        for &(m, rot, k) in &set {
                            let mut s = Aff::ID;
                            for i in 0..n {
                                out.push((m.then(s), rot || i > 0, k));
                                s = s.then(step);
                            }
                        }
                        set = out;
                    }
                }
                "c19_normal_offset_duplication" => {
                    if let Some((n, d)) = c.normal {
                        let (dx, dy) = (last.0 - first.0, last.1 - first.1);
                        let l = dx.hypot(dy);
                        let nrm = if l > 0.0 && l.is_finite() {
                            (-dy / l, dx / l)
                        } else {
                            (0.0, 1.0)
                        };
                        let mut out = Vec::new();
                        for &(m, rot, k) in &set {
                            for i in 0..n {
                                let o = f64::from(i) * d;
                                out.push((m.then(Aff::translate(nrm.0 * o, nrm.1 * o)), rot, k));
                            }
                        }
                        set = out;
                    }
                }
                "c17_mirror" => {
                    if let Some((across, keep)) = c.mirror {
                        let mx = Aff {
                            a: -1.0,
                            e: b.u0 + b.u1,
                            ..Aff::ID
                        };
                        let my = Aff {
                            d: -1.0,
                            f: b.v0 + b.v1,
                            ..Aff::ID
                        };
                        let mir = match across {
                            0 => my,
                            1 => mx,
                            _ => mx.then(my),
                        };
                        let mut out = Vec::new();
                        for &(m, rot, k) in &set {
                            if keep {
                                out.push((m, rot, k));
                            }
                            out.push((m.then(mir), rot, k));
                        }
                        set = out;
                    }
                }
                _ => {}
            }
        }
        set.into_iter()
            .map(|(m, rotated, scale)| Copy {
                m: m.then(to_note),
                rotated,
                scale,
            })
            .collect()
    }

    /// How the curves are drawn.
    fn style(&self, trunk: &[Pt]) -> PathStyle {
        let c = &self.cfg;
        let mut st = PathStyle {
            centred: true,
            ..PathStyle::default()
        };
        if let Some(t) = c.thick {
            st.thick = t;
        }
        if let Some((widest, r)) = c.adaptive {
            st.every = Some((1.0, 1.0));
            st.spacing = Some((by_curvature(trunk, |k| 1.0 - (k * r).min(1.0)), widest));
        }
        if let Some(d) = &c.density {
            st.keep = Some(d.clone());
        }
        let mut shape = [Curve::identity(), Curve::identity(), Curve::identity()];
        let mut shaped = false;
        if let Some((a, z, cv)) = &c.gate {
            st.grad.gate = Some((*a, *z));
            shape[0] = cv.clone();
            shaped = true;
        }
        if let Some((a, z, cv)) = &c.vel {
            st.grad.vel = Some((*a, *z));
            shape[1] = cv.clone();
            shaped = true;
        }
        if let Some((a, z, cv)) = &c.sub {
            st.grad.sub = Some((*a, *z));
            shape[2] = cv.clone();
            shaped = true;
        }
        if shaped {
            st.shape = Some(Box::new(shape));
        }
        if let Some((floor, r)) = c.modulate {
            st.modulate = Some(by_curvature(trunk, |k| {
                floor + (1.0 - floor) * (k * r).min(1.0)
            }));
        }
        if let Some((to, _)) = c.ribbon {
            st.fill = match to {
                1 => Some(FillTo::Bottom),
                2 => Some(FillTo::Top),
                _ => None,
            };
        }
        st
    }

    fn built(&self, f: &Frame) -> &Built {
        self.built.get_or_init(|| {
            let (trunk, dirs) = self.trunk(f);
            Built {
                paths: self.curves(f, &trunk),
                copies: self.copies(f, &trunk, dirs),
                style: self.style(&trunk),
            }
        })
    }

    /// Whether the curves are drawn as ribbons along them.
    fn ribbon(&self) -> Option<f64> {
        self.cfg.ribbon.and_then(|(to, w)| (to == 0).then_some(w))
    }

    /// The caps, with sentences (before anything else is built): the trunk
    /// is built (at most [`FLAT_CAP`] points, or a trochoid's / the
    /// jitter's), the rest counted.
    fn check(&self, f: &Frame) -> Result<(), TransformError> {
        let c = &self.cfg;
        let (trunk, _) = self.trunk(f);
        let big = |v: f64| group_digits(v.min(u64::MAX as f64) as u64);
        let curves =
            f64::from(c.morph.map_or(1, |m| m.0)) * f64::from(c.parallel.map_or(1, |p| p.0));
        let mut pts = trunk.len() as f64 * curves;
        let mut lines = curves;
        if let Some((on, gap, _)) = c.dash {
            // A morph is no longer than the longer of its ends; an offset
            // curve adds at most twice its distance times the turn.
            let b = Gbox::of(&BoxG::of(f));
            let l = length(&trunk);
            let lb = match c.morph.map(|m| m.1) {
                None | Some(0) => l,
                Some(1) => l,
                Some(_) => dist((b.u0, b.v0), (b.u1, b.v1)),
            };
            let reach = c.parallel.map_or(0.0, |(n, sp, _)| f64::from(n) * sp);
            let per = l.max(lb) + 2.0 * reach * turning(&trunk);
            let dashes = curves * (per / (on + gap) + 2.0);
            pts += 2.0 * dashes;
            lines = dashes;
        }
        if pts > PATH_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Arcs / Curves would build {} points; at most {} are allowed — use fewer parallel or morphed curves, longer dashes, fewer loop turns or jitter points.",
                big(pts),
                group_digits(PATH_CAP)
            )));
        }
        let mut copies = 1.0f64;
        if let Some((n, depth, _, _)) = c.branch {
            copies = (0..=depth).map(|d| f64::from(n).powi(d as i32)).sum();
        }
        copies *= f64::from(c.chain.unwrap_or(1)) * f64::from(c.normal.map_or(1, |n| n.0));
        if let Some((_, keep)) = c.mirror
            && keep
        {
            copies *= 2.0;
        }
        if copies > COPY_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Arcs / Curves would draw {} copies of the curve; at most {} are allowed — use fewer branches, a smaller depth, fewer tangent or normal copies.",
                big(copies),
                group_digits(COPY_CAP)
            )));
        }
        let segs = pts - lines;
        let varies = c.vel.is_some() || c.gate.is_some() || c.sub.is_some() || c.modulate.is_some();
        let pieces = if varies {
            let len = length(&trunk) * curves;
            len.min(MAX_PIECES as f64 * segs)
        } else {
            0.0
        };
        let work = copies * (segs + lines + pieces);
        if work > WORK_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Arcs / Curves would draw {} segments; at most {} are allowed — use fewer copies or curves.",
                big(work),
                group_digits(WORK_CAP)
            )));
        }
        // A band piece scans the rows of its thickness, a ribbon's twice
        // its width, a fill to the box's edge the whole drawable band.
        let band = f.band();
        let all_rows = (band.hi - band.lo) as f64;
        let h = match (c.ribbon, c.thick) {
            (Some((0, w)), _) => (2.0 * w + 3.0).min(all_rows),
            (Some(_), _) => all_rows,
            (None, Some(t)) => (t + 2.0).min(all_rows),
            (None, None) => 1.0,
        };
        let scan = work * h;
        if h > 1.0 && scan > SCAN_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Arcs / Curves would scan {} rows of bands or fills; at most {} are allowed — use fewer copies or curves, a thinner stroke or one brush along the curve.",
                big(scan),
                group_digits(SCAN_CAP)
            )));
        }
        Ok(())
    }

    /// The size of the curves carried into note space by `m`.
    fn stats(&self, b: &Built, m: &Aff, g: &BoxG) -> Stats {
        let mut s = Stats::default();
        for p in &b.paths {
            let q: Vec<Pt> = p.iter().map(|&x| m.ap(x)).collect();
            let t = measure(&q, g);
            s.pts += t.pts;
            s.segs += t.segs;
            s.lines += t.lines;
            s.dy += t.dy;
            s.len += t.len;
            s.dx += t.dx;
        }
        s
    }

    /// Draws a ribbon of `width` grid units to the left of curve `p` (grid
    /// space) with map `m`: one polygon when the brush is constant, else a
    /// quad per segment.
    fn draw_ribbon(
        &self,
        p: &[Pt],
        width: f64,
        m: &Aff,
        style: &PathStyle,
        base: Brush,
        pen: &mut dyn Draw,
    ) -> bool {
        if p.len() < 2 {
            return true;
        }
        let edge = offset(p, width);
        let at = |q: Pt| {
            let r = m.ap(q);
            (r.x, r.y)
        };
        if !style.varies() {
            let mut poly: Vec<(f64, f64)> = p.iter().map(|&q| at(q)).collect();
            poly.extend(edge.iter().rev().map(|&q| at(q)));
            return pen.fill(&poly, base);
        }
        for i in 0..p.len() - 1 {
            let quad = [at(p[i]), at(p[i + 1]), at(edge[i + 1]), at(edge[i])];
            if !pen.fill(&quad, style.brush((p[i].s + p[i + 1].s) / 2.0, base)) {
                return false;
            }
        }
        true
    }
}

/// The point at position `s` along `p` and its direction (radians).
fn point_at(p: &[Pt], s: f64) -> (P2, f64) {
    if p.len() < 2 {
        return (p.first().map_or((0.0, 0.0), |q| (q.x, q.y)), 0.0);
    }
    let i = p.partition_point(|q| q.s < s).clamp(1, p.len() - 1);
    let (a, b) = (p[i - 1], p[i]);
    let t = if b.s > a.s {
        (s - a.s) / (b.s - a.s)
    } else {
        0.0
    };
    let q = Pt::lerp(a, b, t.clamp(0.0, 1.0));
    ((q.x, q.y), (b.y - a.y).atan2(b.x - a.x))
}

/// c22: a point on a circle rolling along `p` on its left, `turns` times
/// over its length (32 points a turn): on the rim (cusps) or `size` times
/// its radius out (loops).
fn trochoid(p: &[Pt], cusps: bool, turns: u32, size: f64) -> Vec<Pt> {
    let total = length(p);
    let n = 32 * turns as usize;
    let q = resample(p, n);
    if total <= 0.0 || q.len() < 2 {
        return q;
    }
    let rho = total / (TAU * f64::from(turns));
    let r = if cusps { rho } else { rho * size };
    let nrm = normals(&q);
    q.iter()
        .zip(&nrm)
        .map(|(pt, &(nx, ny))| {
            let phi = pt.s * TAU * f64::from(turns);
            let (tx, ty) = (ny, -nx);
            let (cx, cy) = (pt.x + rho * nx, pt.y + rho * ny);
            Pt {
                x: cx - r * (phi.sin() * tx + phi.cos() * nx),
                y: cy - r * (phi.sin() * ty + phi.cos() * ny),
                s: pt.s,
            }
        })
        .collect()
}

/// c15: the dashes of `p` along its length — `on` of every `period` grid
/// units from `phase` back — each its own polyline (positions kept).
fn dashes(p: &[Pt], on: f64, period: f64, phase: f64) -> Vec<Vec<Pt>> {
    let mut out = Vec::new();
    if p.len() < 2 {
        return vec![p.to_vec()];
    }
    let mut cur: Vec<Pt> = Vec::new();
    let mut acc = -phase;
    let inside = |t: f64| t.rem_euclid(period) < on;
    for w in p.windows(2) {
        let (a, b) = (w[0], w[1]);
        let l = dist((a.x, a.y), (b.x, b.y));
        // The dash edges inside this segment, in order.
        let mut cuts: Vec<f64> = Vec::new();
        if l > 0.0 {
            let k0 = (acc / period).floor();
            let mut k = k0;
            loop {
                let start = k * period;
                for e in [start, start + on] {
                    if e > acc && e < acc + l {
                        cuts.push((e - acc) / l);
                    }
                }
                if start + period >= acc + l {
                    break;
                }
                k += 1.0;
            }
        }
        let mut t0 = 0.0;
        for &t in cuts.iter().chain(std::iter::once(&1.0)) {
            let mid = acc + (t0 + t) / 2.0 * l;
            if inside(mid) {
                let (pa, pb) = (Pt::lerp(a, b, t0), Pt::lerp(a, b, t));
                if cur.last().is_none_or(|q| !q.near(pa)) {
                    if cur.len() > 1 {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    cur.push(pa);
                }
                cur.push(pb);
            } else if cur.len() > 1 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            t0 = t;
        }
        acc += l;
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

impl Shapes for Arcs {
    fn draw(&self, f: &Frame, pen: &mut dyn Draw) -> bool {
        let b = self.built(f);
        let g = BoxG::of(f);
        let rows = f.band();
        let ribbon = self.ribbon();
        let mut sc = Scratch::default();
        for cp in &b.copies {
            for p in &b.paths {
                let ok = match ribbon {
                    Some(w) => self.draw_ribbon(p, w, &cp.m, &b.style, f.brush, pen),
                    None => {
                        sc.line.clear();
                        sc.line.extend(p.iter().map(|&q| cp.m.ap(q)));
                        let mut pts = std::mem::take(&mut sc.line);
                        let ok = b.style.draw(&mut pts, false, &g, f, &rows, pen, &mut sc);
                        sc.line = pts;
                        ok
                    }
                };
                if !ok {
                    return false;
                }
            }
        }
        true
    }

    fn bound(&self, f: &Frame) -> u64 {
        let b = self.built(f);
        let g = BoxG::of(f);
        let rows = f.band();
        let clip_rows = (rows.hi - rows.lo) as f64;
        // The copies' sizes differ only by their scale and turn: measure
        // the curves once (identity-scaled, in note space).
        let unit = Aff {
            a: g.step,
            ..Aff::ID
        };
        let s0 = self.stats(b, &unit, &g);
        let mut total = 0.0;
        for cp in &b.copies {
            // A mirrored or moved copy keeps the curves' size; a turned one
            // is bounded by its grid length (its scale times theirs).
            let s = if cp.rotated {
                Stats {
                    len: s0.len * cp.scale,
                    dy: s0.dy * cp.scale,
                    dx: s0.dx * cp.scale,
                    ..s0
                }
            } else {
                s0
            };
            total += match self.ribbon() {
                Some(w) => {
                    // Per curve: each row meets at most every edge of the
                    // ribbon (or each quad twice).
                    let ext = if cp.rotated { s.len } else { s.dy } + 2.0 * w * cp.scale + 2.0;
                    2.0 * (s.segs + s.lines) * (ext.min(clip_rows) + 1.0)
                }
                None => b.style.bound(&s, cp.rotated, f, &rows),
            } + 1.0;
        }
        if total.is_finite() && total < u64::MAX as f64 {
            total.ceil() as u64
        } else {
            u64::MAX
        }
    }

    fn floor(&self, f: &Frame) -> u64 {
        if self.ribbon().is_some() {
            return 0;
        }
        let b = self.built(f);
        let Some(cp) = b.copies.first() else {
            return 0;
        };
        let g = BoxG::of(f);
        let rows = f.band();
        let v = b.style.lower(&b.paths, &cp.m, false, &g, &rows, f);
        if v.is_finite() && v < u64::MAX as f64 {
            v as u64
        } else {
            0
        }
    }
}

fn arcs_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let arcs = Arcs::new(Cfg::read(p));
    arcs.check(&f)?;
    super::kernel(p, b, arcs)
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
        let spec = arcs_spec();
        let mut rng = Rng::new(0x4152_4353);
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
                ("c08_stroke_thickness.rows", 2, 6),
                ("c09_parallel_curves.count", 2, 5),
                ("c18_tangent_aligned_duplication.count", 2, 5),
                ("c19_normal_offset_duplication.count", 2, 4),
                ("c21_seeded_normal_jitter.points", 4, 64),
                ("c22_loop_cusp_mode.turns", 1, 12),
                ("c22_loop_cusp_mode.mode", 0, 1),
                ("c23_curve_a_b_morph.count", 2, 6),
                ("c24_recursive_branch_curves.branches", 1, 3),
                ("c24_recursive_branch_curves.depth", 1, 2),
            ] {
                if k.ends_with(".mode") {
                    p.set(k, ParamValue::Choice(rng.range_i64(lo, hi) as u32));
                } else {
                    p.set(k, ParamValue::Int(rng.range_i64(lo, hi)));
                }
            }
            p.set(
                "c25_ribbon_fill_from_curve.to",
                ParamValue::Choice(rng.below(3) as u32),
            );
            p.set(
                "c01_circular_arc.radius",
                ParamValue::Float(rng.range_f64(1.0, 30.0)),
            );
            p.set(
                "c15_dash_pattern.dash",
                ParamValue::Float(rng.range_f64(0.5, 6.0)),
            );
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let k = Arcs::new(Cfg::read(&params));
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
