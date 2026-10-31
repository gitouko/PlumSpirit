//! Lines (§152, `gen.lines`; `docs/GENERATORS.md` "Lines"): straight-line
//! note geometry. V09-S51 built the base shape — one line across the
//! generation box, from its lowest pitch at its start to its highest at its
//! end — on the shared engine; V09-S52 adds the contract's 25 functions
//! (catalog ids `c01_…` … `c25_…`, switch = id, controls `<id>.<name>`).
//!
//! The pipeline, in this order (frame units: x = ticks, y = pitch rows):
//!
//! 1. **Base path** — the box diagonal; c01 a line through the box centre
//!    at an angle on the grid (one step across per row up = 45°), clipped to
//!    the box; c03 a polyline at even times with listed heights (it replaces
//!    the base or c01's line); c02 the first / last point at exact times and
//!    pitches.
//! 2. **Shape** — c25 seeded midpoint displacement; c23 / c24 the pitch
//!    travel extended and folded back into the box by the pen's row map
//!    (wrap / reflect; both on: Reflect), time extended and folded too when
//!    asked (split at the seams); c19 the box split into passes, every
//!    other one reversed.
//! 3. **Variants** (affine, applied while drawing, never stored) — c17 a
//!    family of parallel lines every S rows across the box and their time
//!    mirror (clipped to the box), c16 a fan rotated about the start point
//!    on the grid, c05 parallel copies, c14 / c15 mirrors about the box's
//!    centre time / pitch.
//! 4. **Per path** — c04 thickness and c18 the region to the box's bottom,
//!    top or the mirror line, filled by scanline (band mode); c06 / c07 /
//!    c12 sampling instead of strokes (a note every N grid units, a spacing
//!    curve, a dot pattern); c11 dashes along time; c08 / c09 / c10 gate,
//!    velocity and subchannel gradients along the path; c13 the ends
//!    tapered (velocity, and thickness when thicker than one row).
//! 5. **Maps and accents** (the shared pen) — c20 a row map onto a scale,
//!    c21 starts pulled to a musical grid, c22 accents where lines cross
//!    each other and the beat grid.
//!
//! Only c25 draws random values (`gen.seed`). The kernel's cheap bound is
//! analytic (rotation-invariant per segment: a segment's rows ≤ its grid
//! length); the materialized path, the copies, the segments drawn and the
//! crossing search are capped with sentences before anything is drawn.

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::Rng;
use super::super::shared::scale_lattice::Lattice;
use super::super::theory::{KEY_NAMES, Scale, scale_names};
use super::super::tools::{art_grid_values, note_value};
use super::super::{Curve, Kernel, TransformError};
use super::draw::{Aff, BoxG, MAX_PIECES, Pt};
use super::path::{FillTo, PathStyle, Scratch, Stats, clip_t, measure};
use super::pen::{Accent, Band, Draw, Fold, Maps, RowMap, TimeMap};
use super::{Frame, MAX_LENGTH, Shapes};
use crate::midi::group_digits;
use std::sync::{Arc, OnceLock};

pub const LINES: ToolDef = ToolDef {
    id: "gen.lines",
    spec: lines_spec,
    build: lines_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_free_angle", "Free Angle", 10),
    ("c02_exact_start_end_anchors", "Exact Start/End Anchors", 20),
    ("c03_polyline_segments", "Polyline Segments", 30),
    ("c04_line_thickness", "Line Thickness", 40),
    ("c05_parallel_copies", "Parallel Copies", 50),
    ("c06_note_spacing_density", "Note Spacing / Density", 60),
    ("c07_variable_spacing_graph", "Variable Spacing Graph", 70),
    ("c08_gate_gradient", "Gate Gradient", 80),
    ("c09_velocity_gradient", "Velocity Gradient", 90),
    ("c10_subchannel_gradient", "Subchannel Gradient", 100),
    ("c11_dash_pattern", "Dash Pattern", 110),
    ("c12_dot_pattern", "Dot Pattern", 120),
    ("c13_endpoint_taper", "Endpoint Taper", 130),
    ("c14_mirror_x", "Mirror X", 140),
    ("c15_mirror_y", "Mirror Y", 150),
    ("c16_radial_fan", "Radial Fan", 160),
    ("c17_crosshatch", "Crosshatch", 170),
    (
        "c18_scanline_fill_between_boundaries",
        "Scanline Fill Between Boundaries",
        180,
    ),
    ("c19_alternating_direction", "Alternating Direction", 190),
    ("c20_scale_pitch_snap", "Scale/Pitch Snap", 200),
    ("c21_time_grid_snap", "Time/Grid Snap", 210),
    ("c22_intersection_accent", "Intersection Accent", 220),
    ("c23_wrap_boundary", "Wrap Boundary", 230),
    ("c24_reflect_boundary", "Reflect Boundary", 240),
    (
        "c25_recursive_line_subdivision",
        "Recursive Line Subdivision",
        250,
    ),
];

const MIRROR_MODES: &[&str] = &["Keep the line and add its mirror", "Mirror only"];
const FILL_TO: &[&str] = &["The box's bottom", "The box's top", "The mirror line"];

/// Most points of the materialized path (after subdivision, folding and
/// passes).
pub const PATH_CAP: u64 = 1 << 18;
/// Most copies of the path (crosshatch × fan × parallel × mirrors).
pub const COPY_CAP: u64 = 4096;
/// Most segments drawn (copies × the path's segments).
pub const WORK_CAP: u64 = 1 << 24;
/// Most segments the crossing search of Intersection Accent takes.
pub const SEGMENT_CAP: u64 = 1 << 14;
/// The crossing search stops after this many segment pairs (deterministic:
/// always the same pairs first).
pub const PAIR_CAP: u64 = 1 << 22;
/// Most crossing points kept.
pub const CROSSING_CAP: usize = 1 << 18;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const LINES_HELP: &[(&str, &str)] = &[
    (
        "c01_free_angle",
        "Draws the line through the box's center at an angle you choose instead of along the box's diagonal.",
    ),
    (
        "c02_exact_start_end_anchors",
        "Starts and ends the line at exact times and pitches instead of the box's corners.",
    ),
    (
        "c02_exact_start_end_anchors.start",
        "When the line starts, in ticks from the box's start.",
    ),
    (
        "c02_exact_start_end_anchors.start_pitch",
        "The pitch the line starts on.",
    ),
    (
        "c02_exact_start_end_anchors.end",
        "When the line ends, in ticks from the box's start.",
    ),
    (
        "c02_exact_start_end_anchors.end_pitch",
        "The pitch the line ends on.",
    ),
    (
        "c03_polyline_segments",
        "Draws a line through several points at listed heights instead of one straight line.",
    ),
    (
        "c04_line_thickness",
        "Draws the line several rows thick instead of one note per row.",
    ),
    (
        "c04_line_thickness.rows",
        "How many rows thick the line is.",
    ),
    (
        "c05_parallel_copies",
        "Draws several copies of the line side by side.",
    ),
    (
        "c05_parallel_copies.count",
        "How many lines are drawn in all, the original included.",
    ),
    (
        "c05_parallel_copies.pitch",
        "How many rows each copy sits above the previous one (negative = below).",
    ),
    (
        "c05_parallel_copies.time",
        "How many ticks each copy is moved from the previous one (negative = earlier).",
    ),
    (
        "c06_note_spacing_density",
        "Places separate notes along the line instead of drawing it continuously.",
    ),
    (
        "c06_note_spacing_density.length",
        "How long each note is, in resolution steps.",
    ),
    (
        "c07_variable_spacing_graph",
        "Changes the spacing between notes along the line with a curve.",
    ),
    (
        "c07_variable_spacing_graph.max",
        "The spacing at the top of the curve, in grid units.",
    ),
    (
        "c08_gate_gradient",
        "Changes the note lengths from the start of the line to its end.",
    ),
    (
        "c08_gate_gradient.from",
        "The gate (share of each note's length) at the line's start.",
    ),
    (
        "c08_gate_gradient.to",
        "The gate (share of each note's length) at the line's end.",
    ),
    (
        "c09_velocity_gradient",
        "Fades the velocity from the start of the line to its end.",
    ),
    (
        "c09_velocity_gradient.from",
        "The velocity at the line's start.",
    ),
    (
        "c09_velocity_gradient.to",
        "The velocity at the line's end.",
    ),
    (
        "c10_subchannel_gradient",
        "Steps through the track's subchannels (colors) from the start of the line to its end.",
    ),
    (
        "c10_subchannel_gradient.from",
        "The subchannel at the line's start, by its position in the track's list (0 = the first).",
    ),
    (
        "c10_subchannel_gradient.to",
        "The subchannel at the line's end, by its position in the track's list.",
    ),
    (
        "c11_dash_pattern",
        "Breaks the line into dashes along time.",
    ),
    ("c11_dash_pattern.dash", "How long each dash is, in ticks."),
    (
        "c12_dot_pattern",
        "Draws the line as a repeating pattern of dots and rests.",
    ),
    (
        "c13_endpoint_taper",
        "Tapers both ends of the line: softer (and thinner, when thick) toward the ends.",
    ),
    (
        "c13_endpoint_taper.length",
        "How much of the line tapers at each end.",
    ),
    (
        "c13_endpoint_taper.floor",
        "How soft (and thin) the very ends get, as a share of the full velocity and thickness.",
    ),
    (
        "c14_mirror_x",
        "Mirrors the line in time around the box's center.",
    ),
    (
        "c14_mirror_x.mode",
        "Keep the Line and Add Its Mirror draws both; Mirror Only draws just the mirrored line.",
    ),
    (
        "c15_mirror_y",
        "Mirrors the line in pitch around the box's center.",
    ),
    (
        "c15_mirror_y.mode",
        "Keep the Line and Add Its Mirror draws both; Mirror Only draws just the mirrored line.",
    ),
    (
        "c16_radial_fan",
        "Draws a fan of lines turned around the line's start point.",
    ),
    ("c16_radial_fan.count", "How many lines the fan has."),
    (
        "c17_crosshatch",
        "Fills the box with a crosshatch of lines parallel to the line and to its mirror.",
    ),
    (
        "c18_scanline_fill_between_boundaries",
        "Fills the area between the line and an edge with notes.",
    ),
    (
        "c18_scanline_fill_between_boundaries.to",
        "The edge the fill reaches: the box's bottom, its top, or the mirror line.",
    ),
    (
        "c19_alternating_direction",
        "Splits the box into passes and reverses every other one, for a continuous zigzag.",
    ),
    (
        "c20_scale_pitch_snap",
        "Moves every drawn row onto the notes of a scale.",
    ),
    ("c20_scale_pitch_snap.key", "The key of the scale."),
    (
        "c20_scale_pitch_snap.scale",
        "The scale the rows move onto.",
    ),
    (
        "c21_time_grid_snap",
        "Pulls the note starts toward a musical grid.",
    ),
    (
        "c21_time_grid_snap.grid",
        "The grid the starts are pulled to.",
    ),
    (
        "c22_intersection_accent",
        "Accents the notes where the line crosses a beat or another line.",
    ),
    (
        "c22_intersection_accent.velocity",
        "The velocity of the accented notes.",
    ),
    (
        "c22_intersection_accent.radius",
        "How close to a crossing a note must be to be accented, in grid units.",
    ),
    (
        "c22_intersection_accent.beats",
        "Accents the notes where the line crosses a beat line.",
    ),
    (
        "c22_intersection_accent.lines",
        "Accents the notes where drawn lines cross each other.",
    ),
    (
        "c23_wrap_boundary",
        "Lets the line run past the box and come back in at the other side.",
    ),
    (
        "c23_wrap_boundary.time",
        "Also wraps the line in time at the box's start and end.",
    ),
    (
        "c24_reflect_boundary",
        "Lets the line run past the box and bounce back off its edges.",
    ),
    (
        "c24_reflect_boundary.time",
        "Also reflects the line in time at the box's start and end.",
    ),
    (
        "c25_recursive_line_subdivision",
        "Roughens the line by splitting it again and again and moving each midpoint at random (seeded), like a mountain ridge.",
    ),
    (
        "c25_recursive_line_subdivision.depth",
        "How many times the line is split.",
    ),
    (
        "c25_recursive_line_subdivision.displacement",
        "How far the first midpoints may move, as a share of the box's rows.",
    ),
];

fn lines_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        // c01
        flag("c01_free_angle", "Free Angle", false),
        float("c01_free_angle.angle", "Angle", -90.0, 90.0, 30.0, "°").help(
            "A line through the box's center, clipped to the box; measured on the grid: one resolution step across per row up is 45°.",
        ),
        // c02
        flag("c02_exact_start_end_anchors", "Exact Start/End Anchors", false),
        ticks(
            "c02_exact_start_end_anchors.start",
            "Start time (from the box's start)",
            0,
            MAX_LENGTH,
            0,
        ),
        pitch("c02_exact_start_end_anchors.start_pitch", "Start pitch", 60),
        ticks(
            "c02_exact_start_end_anchors.end",
            "End time (from the box's start)",
            0,
            MAX_LENGTH,
            3840,
        ),
        pitch("c02_exact_start_end_anchors.end_pitch", "End pitch", 72),
        // c03
        flag("c03_polyline_segments", "Polyline Segments", false),
        list(
            "c03_polyline_segments.heights",
            "Heights (% of the box)",
            0,
            100,
            64,
            &[0, 100, 25, 75],
        )
        .help("Points at even times across the box, each at a height in percent of the box's pitch range; replaces the base line (and Free Angle's)."),
        // c04
        flag("c04_line_thickness", "Line Thickness", false),
        int("c04_line_thickness.rows", "Thickness (rows)", 2, 32, 3),
        // c05
        flag("c05_parallel_copies", "Parallel Copies", false),
        int("c05_parallel_copies.count", "Lines", 2, 32, 3),
        int(
            "c05_parallel_copies.pitch",
            "Pitch offset of each copy (rows)",
            -127,
            127,
            12,
        )
        .keyed(),
        int(
            "c05_parallel_copies.time",
            "Time offset of each copy (ticks)",
            -(1 << 24),
            1 << 24,
            0,
        ),
        // c06
        flag("c06_note_spacing_density", "Note Spacing / Density", false),
        float(
            "c06_note_spacing_density.every",
            "A note every (grid units)",
            1.0,
            1024.0,
            2.0,
            "",
        )
        .help("Notes instead of a continuous line, along the path: one grid unit is a resolution step across or a row up."),
        int(
            "c06_note_spacing_density.length",
            "Note length (steps)",
            1,
            1024,
            1,
        ),
        // c07
        flag("c07_variable_spacing_graph", "Variable Spacing Graph", false),
        curve(
            "c07_variable_spacing_graph.curve",
            "Spacing along the line",
            Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
        )
        .help("From the line's start (left) to its end: the bottom is one grid unit between notes, the top the widest spacing."),
        float(
            "c07_variable_spacing_graph.max",
            "Widest spacing (grid units)",
            1.0,
            1024.0,
            8.0,
            "",
        ),
        // c08
        flag("c08_gate_gradient", "Gate Gradient", false),
        float("c08_gate_gradient.from", "Gate at the start", 1.0, 100.0, 100.0, "%"),
        float("c08_gate_gradient.to", "Gate at the end", 1.0, 100.0, 25.0, "%"),
        // c09
        flag("c09_velocity_gradient", "Velocity Gradient", false),
        int("c09_velocity_gradient.from", "Velocity at the start", 1, 127, 127),
        int("c09_velocity_gradient.to", "Velocity at the end", 1, 127, 40),
        // c10
        flag("c10_subchannel_gradient", "Subchannel Gradient", false),
        int(
            "c10_subchannel_gradient.from",
            "Subchannel at the start (position in the track)",
            0,
            4095,
            0,
        ),
        int(
            "c10_subchannel_gradient.to",
            "Subchannel at the end (position in the track)",
            0,
            4095,
            3,
        ),
        // c11
        flag("c11_dash_pattern", "Dash Pattern", false),
        ticks("c11_dash_pattern.dash", "Dash (ticks)", 1, 1 << 24, 480),
        ticks("c11_dash_pattern.gap", "Gap (ticks)", 1, 1 << 24, 480)
            .help("Dashes and gaps along time, from the box's start."),
        // c12
        flag("c12_dot_pattern", "Dot Pattern", false),
        list(
            "c12_dot_pattern.pattern",
            "Dots and rests",
            0,
            1,
            64,
            &[1, 0],
        )
        .help("Notes along the path, one per grid unit (or Note Spacing's), each a dot (1) or a rest (0) of the repeating pattern."),
        // c13
        flag("c13_endpoint_taper", "Endpoint Taper", false),
        float(
            "c13_endpoint_taper.length",
            "Taper length (% of the line at each end)",
            1.0,
            50.0,
            25.0,
            "%",
        ),
        float(
            "c13_endpoint_taper.floor",
            "Narrowest (% of the full velocity / thickness)",
            0.0,
            100.0,
            30.0,
            "%",
        ),
        // c14
        flag("c14_mirror_x", "Mirror X", false),
        choice("c14_mirror_x.mode", "Mirror about the box's center time", MIRROR_MODES, 0),
        // c15
        flag("c15_mirror_y", "Mirror Y", false),
        choice("c15_mirror_y.mode", "Mirror about the box's center pitch", MIRROR_MODES, 0),
        // c16
        flag("c16_radial_fan", "Radial Fan", false),
        int("c16_radial_fan.count", "Lines", 2, 32, 5),
        float("c16_radial_fan.spread", "Spread", 0.0, 360.0, 60.0, "°")
            .help("Lines rotated about the line's start point, on the grid, evenly over the spread."),
        // c17
        flag("c17_crosshatch", "Crosshatch", false),
        int("c17_crosshatch.spacing", "Spacing (rows)", 1, 127, 6)
            .keyed()
            .help("Lines parallel to the path every so many rows across the box, and their time mirror, clipped to the box."),
        // c18
        flag(
            "c18_scanline_fill_between_boundaries",
            "Scanline Fill Between Boundaries",
            false,
        ),
        choice(
            "c18_scanline_fill_between_boundaries.to",
            "Fill between the line and",
            FILL_TO,
            0,
        ),
        // c19
        flag("c19_alternating_direction", "Alternating Direction", false),
        int("c19_alternating_direction.passes", "Passes", 2, 64, 4)
            .help("The box split into passes, the line drawn in each, every other one reversed — a continuous zig-zag."),
        // c20
        flag("c20_scale_pitch_snap", "Scale/Pitch Snap", false),
        choice("c20_scale_pitch_snap.key", "Key", &KEY_NAMES, 0),
        choice("c20_scale_pitch_snap.scale", "Scale", scale_names(), 0),
        // c21
        flag("c21_time_grid_snap", "Time/Grid Snap", false),
        choice("c21_time_grid_snap.grid", "Grid", art_grid_values(), 2),
        float("c21_time_grid_snap.strength", "Strength", 0.0, 100.0, 100.0, "%")
            .help("Each note's start pulled towards the nearest grid line; lengths are kept."),
        // c22
        flag("c22_intersection_accent", "Intersection Accent", false),
        int("c22_intersection_accent.velocity", "Accent velocity", 1, 127, 127),
        float("c22_intersection_accent.radius", "Radius (steps)", 0.0, 16.0, 1.0, ""),
        flag("c22_intersection_accent.beats", "Where the line crosses a beat", true),
        flag("c22_intersection_accent.lines", "Where lines cross each other", true),
        // c23
        flag("c23_wrap_boundary", "Wrap Boundary", false),
        float("c23_wrap_boundary.amount", "Extra travel", 0.0, 800.0, 100.0, "%")
            .help("The line's pitch travel lengthened by this much and wrapped back into the box (leaving the top, it re-enters at the bottom)."),
        flag("c23_wrap_boundary.time", "Wrap time too", false),
        // c24
        flag("c24_reflect_boundary", "Reflect Boundary", false),
        float("c24_reflect_boundary.amount", "Extra travel", 0.0, 800.0, 100.0, "%")
            .help("The line's pitch travel lengthened by this much and reflected back into the box at its edges."),
        flag("c24_reflect_boundary.time", "Reflect time too", false),
        // c25
        flag("c25_recursive_line_subdivision", "Recursive Line Subdivision", false),
        int("c25_recursive_line_subdivision.depth", "Depth", 1, 12, 4),
        float(
            "c25_recursive_line_subdivision.displacement",
            "Displacement (% of the box's rows)",
            0.0,
            100.0,
            25.0,
            "%",
        ),
        float(
            "c25_recursive_line_subdivision.roughness",
            "Roughness",
            0.0,
            100.0,
            50.0,
            "%",
        )
        .help("How much each level's displacement keeps of the level above's."),
    ];
    with_help(&mut own, LINES_HELP);
    super::spec(
        "gen.lines",
        "Lines",
        "Generates straight-line note geometry: a line across the generation box (where, length and pitch range above), each row it crosses one note on the resolution grid.",
        own,
        FUNCTIONS,
    )
}

/// c22's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AccentCfg {
    vel: u8,
    radius: f64,
    beats: bool,
    lines: bool,
}

/// The functions' settings (`None` / neutral = off).
#[derive(Clone, Debug, Default)]
struct Cfg {
    /// c01: radians.
    angle: Option<f64>,
    /// c02: start offset (ticks), start pitch, end offset, end pitch.
    anchors: Option<[f64; 4]>,
    /// c03: heights, 0 – 1.
    heights: Option<Vec<f64>>,
    /// c04 thickness, c06 / c07 / c12 sampling, c08 – c10 gradients, c11
    /// dashes, c13 taper, c18 fill: how each path is drawn.
    path: PathStyle,
    /// c05: lines, pitch rows and ticks between copies.
    parallel: Option<(u32, f64, f64)>,
    /// c14 / c15: keep the line.
    mirror_x: Option<bool>,
    mirror_y: Option<bool>,
    /// c16: lines, spread (radians).
    fan: Option<(u32, f64)>,
    /// c17: spacing (rows).
    hatch: Option<f64>,
    /// B2-F01: the variant maps (c17, c16, c05, c14, c15) in the order the
    /// user chose (`order::LINES_VARIANTS`; empty = the declared order).
    variants_order: Vec<&'static str>,
    /// c19 (1 = off).
    passes: u32,
    /// c20.
    scale: Option<Lattice>,
    /// c21: grid (ticks), strength (0 – 1).
    snap: Option<(u64, f64)>,
    /// c22.
    accent: Option<AccentCfg>,
    /// c23 / c24: the pitch fold and its extra travel (fraction), the time
    /// fold and its.
    pitch_fold: Option<(Fold, f64)>,
    time_fold: Option<(Fold, f64)>,
    /// c25: depth, displacement (fraction of the rows), roughness.
    subdiv: Option<(u32, f64, f64)>,
    /// The beat (ticks): c22's beat grid.
    beat: u64,
}

impl Cfg {
    fn read(p: &Params, ctx: &super::super::Ctx) -> Cfg {
        let on = |id: &str| p.flag(id);
        let mut c = Cfg {
            passes: 1,
            beat: ctx.ppq.max(1),
            ..Cfg::default()
        };
        if on("c01_free_angle") {
            c.angle = Some(p.float("c01_free_angle.angle").to_radians());
        }
        if on("c02_exact_start_end_anchors") {
            let k = "c02_exact_start_end_anchors";
            c.anchors = Some([
                p.ticks(&format!("{k}.start")) as f64,
                p.int(&format!("{k}.start_pitch")) as f64,
                p.ticks(&format!("{k}.end")) as f64,
                p.int(&format!("{k}.end_pitch")) as f64,
            ]);
        }
        if on("c03_polyline_segments") {
            c.heights = Some(
                p.list("c03_polyline_segments.heights")
                    .iter()
                    .map(|&h| h.clamp(0, 100) as f64 / 100.0)
                    .collect(),
            );
        }
        if on("c04_line_thickness") {
            c.path.thick = p.int("c04_line_thickness.rows").clamp(2, 32) as f64;
        }
        if on("c05_parallel_copies") {
            c.parallel = Some((
                p.int("c05_parallel_copies.count").clamp(2, 32) as u32,
                p.int("c05_parallel_copies.pitch") as f64,
                p.int("c05_parallel_copies.time") as f64,
            ));
        }
        if on("c06_note_spacing_density") {
            c.path.every = Some((
                p.float("c06_note_spacing_density.every").max(1.0),
                p.int("c06_note_spacing_density.length").max(1) as f64,
            ));
        }
        if on("c07_variable_spacing_graph") {
            c.path.spacing = Some((
                p.curve("c07_variable_spacing_graph.curve"),
                p.float("c07_variable_spacing_graph.max").max(1.0),
            ));
        }
        if on("c08_gate_gradient") {
            c.path.grad.gate = Some((
                p.float("c08_gate_gradient.from") / 100.0,
                p.float("c08_gate_gradient.to") / 100.0,
            ));
        }
        if on("c09_velocity_gradient") {
            c.path.grad.vel = Some((
                p.int("c09_velocity_gradient.from") as f64,
                p.int("c09_velocity_gradient.to") as f64,
            ));
        }
        if on("c10_subchannel_gradient") {
            c.path.grad.sub = Some((
                p.int("c10_subchannel_gradient.from") as f64,
                p.int("c10_subchannel_gradient.to") as f64,
            ));
        }
        if on("c11_dash_pattern") {
            c.path.dash = Some((
                p.ticks("c11_dash_pattern.dash").max(1) as f64,
                p.ticks("c11_dash_pattern.gap").max(1) as f64,
            ));
        }
        if on("c12_dot_pattern") {
            let v: Vec<bool> = p
                .list("c12_dot_pattern.pattern")
                .iter()
                .map(|&d| d != 0)
                .collect();
            // An empty pattern is all dots.
            c.path.dots = Some(if v.is_empty() { vec![true] } else { v });
        }
        if on("c13_endpoint_taper") {
            c.path.taper = Some((
                p.float("c13_endpoint_taper.length") / 100.0,
                p.float("c13_endpoint_taper.floor") / 100.0,
            ));
        }
        if on("c14_mirror_x") {
            c.mirror_x = Some(p.choice("c14_mirror_x.mode") == 0);
        }
        if on("c15_mirror_y") {
            c.mirror_y = Some(p.choice("c15_mirror_y.mode") == 0);
        }
        if on("c16_radial_fan") {
            c.fan = Some((
                p.int("c16_radial_fan.count").clamp(2, 32) as u32,
                p.float("c16_radial_fan.spread").to_radians(),
            ));
        }
        if on("c17_crosshatch") {
            c.hatch = Some(p.int("c17_crosshatch.spacing").max(1) as f64);
        }
        if on("c18_scanline_fill_between_boundaries") {
            c.path.fill = Some(match p.choice("c18_scanline_fill_between_boundaries.to") {
                0 => FillTo::Bottom,
                1 => FillTo::Top,
                _ => FillTo::Mirror,
            });
        }
        if on("c19_alternating_direction") {
            c.passes = p.int("c19_alternating_direction.passes").clamp(2, 64) as u32;
        }
        if on("c20_scale_pitch_snap") {
            let s = Scale::new(
                p.choice("c20_scale_pitch_snap.key"),
                p.choice("c20_scale_pitch_snap.scale"),
            );
            c.scale = Some(Lattice::of_scale(&s));
        }
        if on("c21_time_grid_snap") {
            c.snap = Some((
                note_value(p.choice("c21_time_grid_snap.grid"), 0, ctx),
                p.float("c21_time_grid_snap.strength") / 100.0,
            ));
        }
        if on("c22_intersection_accent") {
            c.accent = Some(AccentCfg {
                vel: p.int("c22_intersection_accent.velocity").clamp(1, 127) as u8,
                radius: p.float("c22_intersection_accent.radius"),
                beats: p.flag("c22_intersection_accent.beats"),
                lines: p.flag("c22_intersection_accent.lines"),
            });
        }
        let wrap = on("c23_wrap_boundary").then(|| {
            (
                p.float("c23_wrap_boundary.amount") / 100.0,
                p.flag("c23_wrap_boundary.time"),
            )
        });
        let reflect = on("c24_reflect_boundary").then(|| {
            (
                p.float("c24_reflect_boundary.amount") / 100.0,
                p.flag("c24_reflect_boundary.time"),
            )
        });
        // Pitch: Reflect when both are on; time: the function asking for it
        // (Reflect's first).
        c.pitch_fold = match (wrap, reflect) {
            (_, Some((a, _))) => Some((Fold::Reflect, a)),
            (Some((a, _)), None) => Some((Fold::Wrap, a)),
            _ => None,
        };
        c.time_fold = match (wrap, reflect) {
            (_, Some((a, true))) => Some((Fold::Reflect, a)),
            (Some((a, true)), _) => Some((Fold::Wrap, a)),
            _ => None,
        };
        c.variants_order = p.sequence(crate::transform::order::LINES_VARIANTS.members);
        if on("c25_recursive_line_subdivision") {
            c.subdiv = Some((
                p.int("c25_recursive_line_subdivision.depth").clamp(1, 12) as u32,
                p.float("c25_recursive_line_subdivision.displacement") / 100.0,
                p.float("c25_recursive_line_subdivision.roughness") / 100.0,
            ));
        }
        c
    }

    /// The variant counts: crosshatch, fan, parallel, mirror X, mirror Y.
    fn variants(&self, rows: f64) -> [u64; 5] {
        let h = self
            .hatch
            .map_or(1, |s| 2 * (2 * (rows / s).ceil() as u64 + 1));
        [
            h,
            self.fan.map_or(1, |f| u64::from(f.0)),
            self.parallel.map_or(1, |p| u64::from(p.0)),
            self.mirror_x.map_or(1, |k| if k { 2 } else { 1 }),
            self.mirror_y.map_or(1, |k| if k { 2 } else { 1 }),
        ]
    }
}

/// The materialized path and the accents (built once per kernel, on the
/// worker: the counting pass builds it, the emitting pass reuses it).
struct Built {
    paths: Vec<Vec<Pt>>,
    accent: Option<Arc<Accent>>,
}

/// The Lines generator.
struct Lines {
    cfg: Cfg,
    built: OnceLock<Built>,
}

/// The box diagonal / Free Angle / Polyline, then the anchors (note space,
/// `s` along the grid length).
fn base_path(c: &Cfg, g: &BoxG) -> Vec<Pt> {
    let diag = vec![(g.x0, g.y0), (g.x1, g.y1)];
    let yv = |h: f64| g.y0 + h * (g.y1 - g.y0);
    let mut pts: Vec<(f64, f64)> = if let Some(h) = &c.heights {
        match h.len() {
            0 => diag,
            1 => vec![(g.x0, yv(h[0])), (g.x1, yv(h[0]))],
            k => (0..k)
                .map(|i| (g.x0 + g.w() * i as f64 / (k - 1) as f64, yv(h[i])))
                .collect(),
        }
    } else if let Some(t) = c.angle {
        free_angle(t, g)
    } else {
        diag
    };
    if let Some([s, sp, e, ep]) = c.anchors {
        if pts.len() == 1 {
            pts.push(pts[0]);
        }
        let n = pts.len();
        pts[0] = (g.x0 + s, sp + 0.5);
        pts[n - 1] = (g.x0 + e, ep + 0.5);
    }
    with_s(&pts, g)
}

/// c01: the line through the box's centre at `t` radians on the grid,
/// clipped to the box (Liang–Barsky; the centre is inside).
fn free_angle(t: f64, g: &BoxG) -> Vec<(f64, f64)> {
    let (cx, cy) = ((g.x0 + g.x1) / 2.0, (g.y0 + g.y1) / 2.0);
    let (dx, dy) = (t.cos() * g.step, t.sin());
    let (mut ta, mut tb) = (f64::NEG_INFINITY, f64::INFINITY);
    for (p, q) in [
        (-dx, cx - g.x0),
        (dx, g.x1 - cx),
        (-dy, cy - g.y0),
        (dy, g.y1 - cy),
    ] {
        if p.abs() < 1e-12 {
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            ta = ta.max(r);
        } else {
            tb = tb.min(r);
        }
    }
    if !(ta.is_finite() && tb.is_finite()) || ta > tb {
        return vec![(cx, cy)];
    }
    vec![(cx + ta * dx, cy + ta * dy), (cx + tb * dx, cy + tb * dy)]
}

/// Positions along the grid length (0 – 1).
fn with_s(pts: &[(f64, f64)], g: &BoxG) -> Vec<Pt> {
    let mut v: Vec<Pt> = pts.iter().map(|&(x, y)| Pt { x, y, s: 0.0 }).collect();
    let mut acc = 0.0;
    let mut cum = vec![0.0; v.len()];
    for i in 1..v.len() {
        acc += g.len(v[i - 1], v[i]);
        cum[i] = acc;
    }
    let n = v.len();
    for (i, p) in v.iter_mut().enumerate() {
        p.s = if acc > 0.0 {
            cum[i] / acc
        } else if n > 1 {
            i as f64 / (n - 1) as f64
        } else {
            0.0
        };
    }
    v
}

/// c25: midpoint displacement, level by level, left to right.
fn subdivide(line: Vec<Pt>, depth: u32, amp0: f64, rough: f64, rng: &mut Rng) -> Vec<Pt> {
    let mut pts = line;
    let mut amp = amp0;
    for _ in 0..depth {
        if pts.len() < 2 {
            break;
        }
        let mut next = Vec::with_capacity(pts.len() * 2 - 1);
        for w in pts.windows(2) {
            next.push(w[0]);
            let mut m = Pt::lerp(w[0], w[1], 0.5);
            if amp > 0.0 {
                m.y += rng.range_f64(-amp, amp);
            }
            next.push(m);
        }
        next.push(*pts.last().unwrap());
        pts = next;
        amp *= rough;
    }
    pts
}

/// c23 / c24 in time: `paths` folded into the box's time span (split at
/// the seams; a wrap starts a new polyline at each seam).
fn fold_time(paths: Vec<Vec<Pt>>, kind: Fold, g: &BoxG) -> Vec<Vec<Pt>> {
    let w = g.w().max(1.0);
    let band = |x: f64| ((x - g.x0) / w).floor();
    let place = |p: Pt, k: f64| -> Pt {
        let local = p.x - g.x0 - k * w;
        let x = match kind {
            Fold::Reflect if k.rem_euclid(2.0) != 0.0 => g.x0 + w - local,
            _ => g.x0 + local,
        };
        Pt { x, ..p }
    };
    let mut out: Vec<Vec<Pt>> = Vec::new();
    let mut ts: Vec<f64> = Vec::new();
    for line in paths {
        if line.len() == 1 {
            out.push(vec![place(line[0], band(line[0].x))]);
            continue;
        }
        let mut cur: Vec<Pt> = Vec::new();
        for s in line.windows(2) {
            let (a, b) = (s[0], s[1]);
            // Seams strictly inside the segment, in order along it.
            ts.clear();
            ts.push(0.0);
            let (ka, kb) = (band(a.x), band(b.x));
            // (An integer count: the stats check keeps the seams under
            // PATH_CAP, so the loop is short.)
            if ka != kb && b.x != a.x && (ka - kb).abs() <= PATH_CAP as f64 {
                let (k0, k1) = (ka.min(kb) as i64 + 1, ka.max(kb) as i64);
                for k in k0..=k1 {
                    let t = (g.x0 + k as f64 * w - a.x) / (b.x - a.x);
                    if t > 0.0 && t < 1.0 {
                        ts.push(t);
                    }
                }
                if b.x < a.x {
                    ts[1..].reverse();
                }
            }
            ts.push(1.0);
            for q in ts.windows(2) {
                let (p0, p1) = (Pt::lerp(a, b, q[0]), Pt::lerp(a, b, q[1]));
                let k = band((p0.x + p1.x) / 2.0);
                let (p0, p1) = (place(p0, k), place(p1, k));
                match cur.last() {
                    Some(l) if l.near(p0) => cur.push(p1),
                    _ => {
                        if cur.len() > 1 {
                            out.push(std::mem::take(&mut cur));
                        }
                        cur.clear();
                        cur.push(p0);
                        cur.push(p1);
                    }
                }
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// c19: `n` passes across the box, every other one reversed (time keeps
/// running forwards); positions along the whole zig-zag.
fn alternate(paths: &[Vec<Pt>], n: u32, g: &BoxG) -> Vec<Vec<Pt>> {
    let w = g.w();
    let nf = f64::from(n);
    let mut out: Vec<Vec<Pt>> = Vec::new();
    for k in 0..n {
        let rev = k % 2 == 1;
        let kf = f64::from(k);
        let map = |p: Pt| -> Pt {
            let u = if w > 0.0 { (p.x - g.x0) / w } else { 0.0 };
            let (u, s) = if rev { (1.0 - u, 1.0 - p.s) } else { (u, p.s) };
            Pt {
                x: g.x0 + (kf + u) / nf * w,
                y: p.y,
                s: (kf + s) / nf,
            }
        };
        let mut push = |line: Vec<Pt>| match out.last_mut() {
            Some(l) if l.last().is_some_and(|q| q.near(line[0])) => {
                l.extend_from_slice(&line[1..]);
            }
            _ => out.push(line),
        };
        if rev {
            for line in paths.iter().rev() {
                push(line.iter().rev().map(|&p| map(p)).collect());
            }
        } else {
            for line in paths {
                push(line.iter().map(|&p| map(p)).collect());
            }
        }
    }
    out
}

impl Lines {
    fn new(cfg: Cfg) -> Lines {
        Lines {
            cfg,
            built: OnceLock::new(),
        }
    }

    /// The materialized path (and accents), built once.
    fn built(&self, f: &Frame) -> &Built {
        self.built.get_or_init(|| {
            let paths = self.path(f);
            let accent = self.cfg.accent.map(|a| {
                let rows = self.row_map(f);
                let pts = if a.lines {
                    self.crossings(f, &paths)
                } else {
                    Vec::new()
                };
                let beat = if a.beats { self.cfg.beat } else { 0 };
                Arc::new(Accent::new(a.vel, a.radius, beat, pts, &rows))
            });
            Built { paths, accent }
        })
    }

    /// The path: base, subdivision, folds, passes.
    fn path(&self, f: &Frame) -> Vec<Vec<Pt>> {
        let c = &self.cfg;
        let g = BoxG::of(f);
        let mut line = base_path(c, &g);
        if let Some((depth, disp, rough)) = c.subdiv {
            let mut rng = Rng::new(f.seed ^ 0x4C49_4E45_5331);
            line = subdivide(line, depth, disp * g.rows(), rough, &mut rng);
        }
        if let Some((_, a)) = c.pitch_fold {
            for p in &mut line {
                p.y = g.y0 + (p.y - g.y0) * (1.0 + a);
            }
        }
        let mut paths = vec![line];
        if let Some((kind, a)) = c.time_fold {
            for p in &mut paths[0] {
                p.x = g.x0 + (p.x - g.x0) * (1.0 + a);
            }
            paths = fold_time(paths, kind, &g);
        }
        if c.passes > 1 {
            paths = alternate(&paths, c.passes, &g);
        }
        paths
    }

    /// Upper bounds of [`Lines::path`]'s size, without building it.
    fn stats(&self, f: &Frame) -> Stats {
        let c = &self.cfg;
        let g = BoxG::of(f);
        let base = base_path(c, &g);
        let mut s = measure(&base, &g);
        if let Some((depth, disp, rough)) = c.subdiv
            && s.segs > 0.0
        {
            let segs0 = s.segs;
            let mut amp = disp * g.rows();
            for k in 0..depth {
                // Each midpoint's displacement adds at most twice itself.
                let mids = segs0 * f64::powi(2.0, k as i32);
                s.dy += mids * 2.0 * amp;
                s.len += mids * 2.0 * amp;
                amp *= rough;
            }
            s.segs = segs0 * f64::powi(2.0, depth as i32);
            s.pts = s.segs + 1.0;
        }
        if let Some((_, a)) = c.pitch_fold {
            s.dy *= 1.0 + a;
            s.len *= 1.0 + a;
        }
        if let Some((_, a)) = c.time_fold {
            s.dx *= 1.0 + a;
            s.len *= 1.0 + a;
            // A segment crosses at most |Δx| / width + 1 seams; a seam splits
            // a segment and (wrapping) starts a polyline.
            let seams = s.dx / g.w().max(1.0) + s.segs;
            s.segs += seams;
            s.pts += 2.0 * seams;
            s.lines += seams;
        }
        if c.passes > 1 {
            let n = f64::from(c.passes);
            s.pts *= n;
            s.segs *= n;
            s.lines *= n;
            s.dy *= n;
            s.len *= n;
        }
        s
    }

    /// The pen's row map: the pitch fold into the box's rows, the scale.
    fn row_map(&self, f: &Frame) -> RowMap {
        RowMap {
            fold: self.cfg.pitch_fold.map_or(Fold::None, |p| p.0),
            lo: i64::from(f.lo),
            hi: i64::from(f.hi),
            scale: self.cfg.scale,
        }
    }

    /// How many copies of the path are drawn.
    fn copies(&self, f: &Frame) -> u64 {
        self.cfg
            .variants(BoxG::of(f).rows())
            .iter()
            .fold(1u64, |a, &b| a.saturating_mul(b))
    }

    /// Copy `i`: its map and whether it is clipped to the box (c17).
    fn copy(&self, i: u64, g: &BoxG, pivot: Pt) -> (Aff, bool) {
        let c = &self.cfg;
        let [nh, nf, np, nx, _] = c.variants(g.rows());
        let (ih, i) = (i % nh, i / nh);
        let (iff, i) = (i % nf, i / nf);
        let (ip, i) = (i % np, i / np);
        let (ix, iy) = (i % nx, i / nx);
        let mirror_x = Aff {
            a: -1.0,
            e: g.x0 + g.x1,
            ..Aff::ID
        };
        let mirror_y = Aff {
            d: -1.0,
            f: g.y0 + g.y1,
            ..Aff::ID
        };
        // B2-F01: the variant maps composed in the user's order (declared:
        // crosshatch, fan, parallel copies, mirror X, mirror Y).
        let order: &[&str] = if c.variants_order.is_empty() {
            crate::transform::order::LINES_VARIANTS.members
        } else {
            &c.variants_order
        };
        let mut m = Aff::ID;
        for id in order {
            match *id {
                "c17_crosshatch" => {
                    if let Some(sp) = c.hatch {
                        let j = (ih / 2) as f64 - (g.rows() / sp).ceil();
                        m = m.then(Aff::translate(0.0, j * sp));
                        if ih % 2 == 1 {
                            m = m.then(mirror_x);
                        }
                    }
                }
                "c16_radial_fan" => {
                    if let Some((n, spread)) = c.fan {
                        let t = -spread / 2.0 + iff as f64 * spread / f64::from(n - 1);
                        m = m.then(Aff::rotate(t, pivot.x, pivot.y, g.step));
                    }
                }
                "c05_parallel_copies" => {
                    if let Some((_, dp, dt)) = c.parallel {
                        m = m.then(Aff::translate(ip as f64 * dt, ip as f64 * dp));
                    }
                }
                "c14_mirror_x" => {
                    if let Some(keep) = c.mirror_x
                        && (!keep || ix == 1)
                    {
                        m = m.then(mirror_x);
                    }
                }
                "c15_mirror_y" => {
                    if let Some(keep) = c.mirror_y
                        && (!keep || iy == 1)
                    {
                        m = m.then(mirror_y);
                    }
                }
                _ => {}
            }
        }
        (m, c.hatch.is_some())
    }

    /// c22: where the drawn lines cross each other (an x sweep over every
    /// copy's segments; adjacent segments of one polyline excepted).
    fn crossings(&self, f: &Frame, paths: &[Vec<Pt>]) -> Vec<(f64, f64)> {
        let g = BoxG::of(f);
        let pivot = paths
            .first()
            .and_then(|l| l.first())
            .copied()
            .unwrap_or(Pt {
                x: g.x0,
                y: g.y0,
                s: 0.0,
            });
        // In grid units.
        let mut segs: Vec<SweepSeg> = Vec::new();
        let n = self.copies(f);
        let mut id = 0u32;
        for i in 0..n {
            let (m, clip) = self.copy(i, &g, pivot);
            for line in paths {
                for (k, w) in line.windows(2).enumerate() {
                    let (mut a, mut b) = (m.ap(w[0]), m.ap(w[1]));
                    if clip {
                        let Some((ta, tb)) = clip_t(a, b, &g) else {
                            continue;
                        };
                        (a, b) = (Pt::lerp(a, b, ta), Pt::lerp(a, b, tb));
                    }
                    let (pa, pb) = ((a.x / g.step, a.y), (b.x / g.step, b.y));
                    segs.push((pa.0.min(pb.0), pa.0.max(pb.0), pa, pb, id, k as u32));
                }
                id += 1;
            }
        }
        segs.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.4.cmp(&q.4)).then(p.5.cmp(&q.5)));
        let mut out = Vec::new();
        let mut active: Vec<usize> = Vec::new();
        let mut pairs = 0u64;
        'sweep: for i in 0..segs.len() {
            let s = segs[i];
            active.retain(|&j| segs[j].1 >= s.0);
            for &j in &active {
                pairs += 1;
                if pairs > PAIR_CAP {
                    break 'sweep;
                }
                let t = segs[j];
                if t.4 == s.4 && t.5.abs_diff(s.5) <= 1 {
                    continue;
                }
                if let Some((x, y)) = cross(s.2, s.3, t.2, t.3) {
                    out.push((x * g.step, y));
                    if out.len() >= CROSSING_CAP {
                        break 'sweep;
                    }
                }
            }
            active.push(i);
        }
        out
    }

    /// Draws one copy of polyline `line` (map `m`, clipped to the box when
    /// `clip`); `rows` is the pen's clip band.
    #[allow(clippy::too_many_arguments)]
    fn draw_line(
        &self,
        line: &[Pt],
        m: &Aff,
        clip: bool,
        f: &Frame,
        rows: &Band,
        pen: &mut dyn Draw,
        sc: &mut Scratch,
    ) -> bool {
        let c = &self.cfg;
        let g = BoxG::of(f);
        sc.line.clear();
        sc.line.extend(line.iter().map(|&p| m.ap(p)));
        let mut pts = std::mem::take(&mut sc.line);
        let ok = c.path.draw(&mut pts, clip, &g, f, rows, pen, sc);
        sc.line = pts;
        ok
    }

    /// A cheap lower bound of the notes [`Shapes::draw`] makes, for the
    /// cases whose output grows with the box's length (samples, dashes):
    /// one copy alone — the first, or with Crosshatch the family's unshifted
    /// line — when it lies wholly in the drawable rows (and in the box when
    /// clipped to it): every sample (or every dash it fully crosses,
    /// halved) draws a note there.
    fn lower(&self, f: &Frame) -> f64 {
        let c = &self.cfg;
        let dash = c.path.dash_grid(f);
        if !(c.path.sampled() || dash.is_some()) {
            return 0.0;
        }
        let b = self.built(f);
        let g = BoxG::of(f);
        let Some(&pivot) = b.paths.first().and_then(|l| l.first()) else {
            return 0.0;
        };
        // With Crosshatch, the family's line at offset 0 (not mirrored).
        let i0 = c.hatch.map_or(0, |sp| 2 * (g.rows() / sp).ceil() as u64);
        let (m, clip) = self.copy(i0, &g, pivot);
        let rows = self.row_map(f).clip(f.band());
        c.path.lower(&b.paths, &m, clip, &g, &rows, f)
    }

    /// The caps, with sentences (before anything is built).
    fn check(&self, f: &Frame) -> Result<(), TransformError> {
        let s = self.stats(f);
        let big = |v: f64| group_digits(v.min(u64::MAX as f64) as u64);
        if s.pts > PATH_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Lines would build a path of {} points; at most {} are allowed — lower Recursive Line Subdivision's depth, Alternating Direction's passes or the folds' extra travel.",
                big(s.pts),
                group_digits(PATH_CAP)
            )));
        }
        let n = self.copies(f);
        if n > COPY_CAP {
            return Err(TransformError::TooLarge(format!(
                "Lines would draw {} copies of the line; at most {} are allowed — use fewer parallel copies, fan lines or crosshatch lines.",
                group_digits(n),
                group_digits(COPY_CAP)
            )));
        }
        // Segments drawn, and the pieces a changing brush cuts them into.
        let per = s.segs
            + s.lines
            + if self.cfg.path.varies() {
                s.len.min(MAX_PIECES as f64 * s.segs)
            } else {
                0.0
            };
        let work = n as f64 * per;
        if work > WORK_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Lines would draw {} segments; at most {} are allowed — use fewer copies or a simpler path.",
                big(work),
                group_digits(WORK_CAP)
            )));
        }
        if self.cfg.accent.is_some_and(|a| a.lines) && n as f64 * s.segs > SEGMENT_CAP as f64 {
            return Err(TransformError::TooLarge(format!(
                "Intersection Accent searches at most {} segments for crossings; these settings draw {} — use fewer copies or a simpler path, or accent only the beats.",
                group_digits(SEGMENT_CAP),
                big(n as f64 * s.segs)
            )));
        }
        Ok(())
    }
}

/// A segment of the crossing sweep: x min, x max, its ends (grid units),
/// its polyline id and its index in it.
type SweepSeg = (f64, f64, (f64, f64), (f64, f64), u32, u32);

/// Where segments `p0 → p1` and `q0 → q1` cross (not when parallel).
fn cross(p0: (f64, f64), p1: (f64, f64), q0: (f64, f64), q1: (f64, f64)) -> Option<(f64, f64)> {
    let r = (p1.0 - p0.0, p1.1 - p0.1);
    let s = (q1.0 - q0.0, q1.1 - q0.1);
    let d = r.0 * s.1 - r.1 * s.0;
    if d.abs() < 1e-12 * (r.0.hypot(r.1) * s.0.hypot(s.1)).max(1e-300) {
        return None;
    }
    let qp = (q0.0 - p0.0, q0.1 - p0.1);
    let t = (qp.0 * s.1 - qp.1 * s.0) / d;
    let u = (qp.0 * r.1 - qp.1 * r.0) / d;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u))
        .then_some((p0.0 + t * r.0, p0.1 + t * r.1))
}

impl Shapes for Lines {
    fn draw(&self, f: &Frame, pen: &mut dyn Draw) -> bool {
        let b = self.built(f);
        let g = BoxG::of(f);
        let Some(&pivot) = b.paths.first().and_then(|l| l.first()) else {
            return true;
        };
        let rows = self.row_map(f).clip(f.band());
        let mut sc = Scratch::default();
        for i in 0..self.copies(f) {
            let (m, clip) = self.copy(i, &g, pivot);
            for line in &b.paths {
                if !self.draw_line(line, &m, clip, f, &rows, pen, &mut sc) {
                    return false;
                }
            }
        }
        true
    }

    fn bound(&self, f: &Frame) -> u64 {
        let s = self.stats(f);
        let n = self.copies(f) as f64;
        let clip = self.row_map(f).clip(f.band());
        let b = n * (self.cfg.path.bound(&s, self.cfg.fan.is_some(), f, &clip) + 1.0);
        if b.is_finite() && b < u64::MAX as f64 {
            b.ceil() as u64
        } else {
            u64::MAX
        }
    }

    fn floor(&self, f: &Frame) -> u64 {
        let v = self.lower(f);
        if v.is_finite() && v < u64::MAX as f64 {
            v as u64
        } else {
            0
        }
    }

    fn maps(&self, f: &Frame) -> Maps {
        let c = &self.cfg;
        Maps {
            rows: self.row_map(f),
            time: c
                .snap
                .map_or(TimeMap::default(), |(grid, strength)| TimeMap {
                    grid,
                    strength,
                }),
            accent: if c.accent.is_some() {
                self.built(f).accent.clone()
            } else {
                None
            },
        }
    }
}

fn lines_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let lines = Lines::new(Cfg::read(p, b.ctx));
    lines.check(&f)?;
    super::kernel(p, b, lines)
}
