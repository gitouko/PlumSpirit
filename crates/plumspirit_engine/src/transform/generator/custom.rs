//! Custom Shape (§157, `gen.custom`; `docs/GENERATORS.md` "Custom Shape"):
//! the first Advanced generator — a shape of the owner's own, drawn by the
//! shared area rasterizer (`generator/area.rs` through the region scene).
//! The base: an arrow pointing to the box's end (vertices in ‰ of the
//! generation box), its outline one cell wide inside it.
//!
//! Shapes are built in local grid units about the box's (or a tile's)
//! centre, then placed. The pipeline, in this order:
//!
//! 1. **Contours** — the main one (the base arrow, c01's vertices, or c25's
//!    named shape from the library), c05's extra contours, c06's holes,
//!    the operands of c19 – c21 (ellipse, rectangle or polygon). Edges are
//!    numbered in that order (main, extra, holes, operands) for the per-edge
//!    and per-vertex functions.
//! 2. **Curves and morph** — c02 Bézier edges (handles automatic or listed),
//!    flattened in grid space; c24 the main contour moved by arc length
//!    towards its target (several steps: several shapes).
//! 3. **The area** — main + extra contours filled by c10's rule (even–odd
//!    without it), less the holes, then the operands in order (union,
//!    subtract, intersection); c17 / c18 its union with mirrored / turned
//!    copies of itself.
//! 4. **Maps** — c23 a mesh warp over the box, c16 the full transform
//!    (scale, shear, rotation, move).
//! 5. **Drawn** — the area's outline band (c07 placement, c09 thickness) or
//!    the paths themselves (c03 the main one open, c04 every contour as a
//!    closed stroke with joins); the interior (c08 filled, c10 patterns).
//! 6. **Brushes along the edges** — c11 per-edge density, c12 per-vertex
//!    density, c13 per-edge subchannel, c14 per-vertex velocity, c15 a gate
//!    profile.
//! 7. **Copies** — c22 tiles (the shape built for a tile, every other one
//!    mirrored if asked), c24's steps.
//!
//! Nothing is random: the generator has no seed.

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::tools::geometry::Mesh;
use super::super::{Curve, Kernel, TransformError};
use super::area::{self, Cap, Contour, Figure, Geom, GeomSpec, Join, Op, Place, Rule, SEG_CAP};
use super::draw::{Grad, Pt, flatten, resample};
use super::pen::Draw;
use super::region::{AreaCopy, GDir, P2, Scene, Style, XPat};
use super::{Frame, Shapes};
use crate::midi::group_digits;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::sync::OnceLock;

pub const CUSTOM: ToolDef = ToolDef {
    id: "gen.custom",
    spec: custom_spec,
    build: custom_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    (
        "c01_arbitrary_vertex_placement",
        "Arbitrary Vertex Placement",
        10,
    ),
    (
        "c02_bezier_handles_per_vertex",
        "Bezier Handles Per Vertex",
        20,
    ),
    ("c03_open_path", "Open Path", 30),
    ("c04_closed_path", "Closed Path", 40),
    ("c05_multiple_contours", "Multiple Contours", 50),
    ("c06_interior_holes", "Interior Holes", 60),
    ("c07_outline_generation", "Outline Generation", 70),
    ("c08_filled_generation", "Filled Generation", 80),
    ("c09_stroke_thickness", "Stroke Thickness", 90),
    (
        "c10_configurable_fill_strategy",
        "Configurable Fill Strategy",
        100,
    ),
    ("c11_per_edge_density", "Per-Edge Density", 110),
    ("c12_per_vertex_density", "Per-Vertex Density", 120),
    ("c13_per_edge_subchannel", "Per-Edge Subchannel", 130),
    ("c14_per_vertex_velocity", "Per-Vertex Velocity", 140),
    ("c15_gate_profile", "Gate Profile", 150),
    ("c16_full_transform", "Full Transform", 160),
    ("c17_mirror_symmetry", "Mirror Symmetry", 170),
    ("c18_radial_symmetry", "Radial Symmetry", 180),
    ("c19_shape_union", "Shape Union", 190),
    ("c20_shape_subtract", "Shape Subtract", 200),
    ("c21_shape_intersection", "Shape Intersection", 210),
    ("c22_repeat_tile", "Repeat / Tile", 220),
    ("c23_mesh_warp", "Mesh Warp", 230),
    ("c24_shape_a_b_morph", "Shape A→B Morph", 240),
    (
        "c25_save_load_custom_shape_presets",
        "Save/Load Custom Shape Presets",
        250,
    ),
];

/// The base shape: an arrow pointing to the box's end (x, y pairs, ‰).
pub const BASE: &[i64] = &[
    0, 250, 600, 250, 600, 0, 1000, 500, 600, 1000, 600, 750, 0, 750,
];
/// Most vertices of the main contour.
pub const MAX_MAIN: usize = 64;
/// Most vertices of the extra contours together (c05), of the holes (c06).
pub const MAX_EXTRA: usize = 48;
/// Most vertices of a boolean operand's polygon.
pub const MAX_OPERAND: usize = 32;
/// Most copies (tiles × morph steps).
pub const TILE_CAP: u64 = 2048;
/// Points of a morph by arc length.
pub const MORPH_POINTS: usize = 256;
/// Points of an ellipse (operands, morph target).
const ELLIPSE_POINTS: usize = 64;
/// A warped edge is cut into pieces of at most this (grid units), at most
/// [`WARP_MOST`] per edge.
const WARP_STEP: f64 = 1.0;
const WARP_MOST: usize = 64;

/// The library c25 starts with (names, then contours `x,y` in ‰, `|`
/// between contours, `;` between shapes).
pub const LIBRARY: &str = "Heart: 500,60 820,380 900,600 840,820 680,920 500,800 320,920 160,820 100,600 180,380; \
Star: 500,1000 382,662 24,655 310,438 206,95 500,300 794,95 690,438 976,655 618,662; \
Cross: 350,0 650,0 650,350 1000,350 1000,650 650,650 650,1000 350,1000 350,650 0,650 0,350 350,350; \
Ring: 300,0 700,0 1000,300 1000,700 700,1000 300,1000 0,700 0,300 | 400,250 600,250 750,400 750,600 600,750 400,750 250,600 250,400; \
Arrow: 0,250 600,250 600,0 1000,500 600,1000 600,750 0,750";

const CAPS: &[&str] = &["Butt", "Round", "Square"];
const JOINS: &[&str] = &["Round", "Mitre", "Bevel"];
const HANDLES: &[&str] = &["Smooth (automatic)", "From the list"];
const PLACEMENT: &[&str] = &[
    "Outside the shape",
    "Centered on its edge",
    "Inside the shape",
];
const RULES: &[&str] = &["Even–odd", "Non-zero"];
const PATTERNS: &[&str] = &["Solid", "Rows", "Columns", "Diagonal lines", "Crosshatch"];
const PROFILE_BY: &[&str] = &[
    "Around the shape",
    "Along each edge",
    "Start to end",
    "Bottom to top",
    "From the center out",
];
const AXES: &[&str] = &[
    "Left–right (a vertical axis)",
    "Bottom–top (a horizontal axis)",
    "Both axes",
];
const OPERANDS: &[&str] = &["Ellipse", "Rectangle", "Polygon (the points)"];
const TILES: &[&str] = &[
    "Grid",
    "Brick (every other row moved half a tile)",
    "Grid, every other tile mirrored",
];
const TARGETS: &[&str] = &["Ellipse", "Star", "Polygon (the points)"];

/// The three boolean functions: id, operation, default operand (kind, x, y,
/// w, h in ‰).
/// An operand's defaults: kind, centre x, y, width, height (‰).
type OperandDefault = (u32, i64, i64, i64, i64);

const BOOLEANS: [(&str, Op, OperandDefault); 3] = [
    ("c19_shape_union", Op::Union, (0, 250, 500, 500, 800)),
    ("c20_shape_subtract", Op::Subtract, (0, 800, 500, 300, 300)),
    (
        "c21_shape_intersection",
        Op::Intersect,
        (0, 500, 500, 1000, 1000),
    ),
];

fn operand_specs(
    id: &'static str,
    label: &'static str,
    def: (u32, i64, i64, i64, i64),
) -> Vec<ParamSpec> {
    let k = |s: &str| -> &'static str { Box::leak(format!("{id}.{s}").into_boxed_str()) };
    vec![
        flag(id, label, false),
        choice(k("kind"), "Operand", OPERANDS, def.0),
        int(k("x"), "Center x (‰ of the box)", 0, 1000, def.1),
        int(k("y"), "Center y (‰ of the box)", 0, 1000, def.2),
        int(k("w"), "Width (‰ of the box)", 1, 2000, def.3),
        int(k("h"), "Height (‰ of the box)", 1, 2000, def.4).help(
            "The ellipse's or rectangle's center and size; a polygon uses the points below instead.",
        ),
        list(
            k("points"),
            "Polygon (x, y pairs, ‰ of the box)",
            0,
            1000,
            2 * MAX_OPERAND,
            &[500, 100, 900, 500, 500, 900, 100, 500],
        ),
    ]
}

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CUSTOM_HELP: &[(&str, &str)] = &[
    (
        "c01_arbitrary_vertex_placement",
        "Draws a shape through corners you place anywhere in the box instead of the arrow.",
    ),
    (
        "c02_bezier_handles_per_vertex",
        "Curves the shape's edges into smooth Bézier curves.",
    ),
    (
        "c02_bezier_handles_per_vertex.mode",
        "Smooth sets the handles automatically; From the List uses the handles you type below.",
    ),
    (
        "c03_open_path",
        "Leaves the main shape open, drawing it as a line instead of a closed outline.",
    ),
    (
        "c03_open_path.caps",
        "How the open path's ends look: cut off flat, rounded, or squared past the end.",
    ),
    (
        "c04_closed_path",
        "Draws every contour as a closed line centered on its edge instead of the area's outline.",
    ),
    (
        "c05_multiple_contours",
        "Adds more contours that join the shape.",
    ),
    (
        "c05_multiple_contours.points",
        "The extra contours' corners as x, y pairs in thousandths of the box (x from its start, y from its bottom).",
    ),
    (
        "c06_interior_holes",
        "Cuts holes of your own shape out of the inside.",
    ),
    (
        "c06_interior_holes.points",
        "The holes' corners as x, y pairs in thousandths of the box.",
    ),
    (
        "c07_outline_generation",
        "Chooses where the outline's cells lie against the shape's edge.",
    ),
    (
        "c08_filled_generation",
        "Fills the inside of the shape with notes.",
    ),
    (
        "c09_stroke_thickness",
        "Makes the outline (or the path) thicker.",
    ),
    (
        "c10_configurable_fill_strategy",
        "Chooses how overlapping contours count as inside and how the inside is filled.",
    ),
    (
        "c10_configurable_fill_strategy.pattern",
        "How the inside is drawn: solid, in rows, in columns, in diagonal lines, or crosshatched.",
    ),
    (
        "c11_per_edge_density",
        "Draws only every Nth cell along each edge, set per edge from a list.",
    ),
    (
        "c12_per_vertex_density",
        "Thins the outline by how close it is to each corner, set per corner from a list.",
    ),
    (
        "c13_per_edge_subchannel",
        "Gives each edge of the outline a subchannel (color) from a list.",
    ),
    (
        "c14_per_vertex_velocity",
        "Sets the velocity at each corner and blends it along the edges between them.",
    ),
    (
        "c15_gate_profile",
        "Changes the note lengths by their position on the shape, with a curve.",
    ),
    (
        "c15_gate_profile.by",
        "What the curve's left-to-right position follows: around the shape, along each edge, from start to end, from bottom to top, or from the center out.",
    ),
    (
        "c16_full_transform",
        "Moves, turns, scales and slants the whole shape.",
    ),
    (
        "c16_full_transform.dx",
        "How far the shape moves in time, as a share of the box's width.",
    ),
    (
        "c16_full_transform.dy",
        "How far the shape moves in pitch, as a share of the box's height.",
    ),
    (
        "c16_full_transform.angle",
        "How far the shape turns about the box's center, in degrees (counterclockwise).",
    ),
    (
        "c16_full_transform.sx",
        "How wide the shape is, as a share of its own width.",
    ),
    (
        "c16_full_transform.sy",
        "How tall the shape is, as a share of its own height.",
    ),
    (
        "c17_mirror_symmetry",
        "Joins the shape with its mirror image.",
    ),
    (
        "c17_mirror_symmetry.axis",
        "The mirror: left to right (a vertical axis), bottom to top (a horizontal axis), or both.",
    ),
    (
        "c18_radial_symmetry",
        "Joins the shape with copies turned evenly around the box's center, like a kaleidoscope.",
    ),
    (
        "c18_radial_symmetry.count",
        "How many copies go around the center, the shape included.",
    ),
    (
        "c19_shape_union",
        "Adds a second shape to the area (a union).",
    ),
    (
        "c19_shape_union.kind",
        "The second shape: an ellipse, a rectangle, or a polygon through the points below.",
    ),
    (
        "c19_shape_union.x",
        "Where the second shape's center lies in time, in thousandths of the box.",
    ),
    (
        "c19_shape_union.y",
        "Where the second shape's center lies in pitch, in thousandths of the box.",
    ),
    (
        "c19_shape_union.w",
        "How wide the second shape is, in thousandths of the box.",
    ),
    (
        "c19_shape_union.points",
        "The polygon's corners as x, y pairs in thousandths of the box, used when the second shape is a polygon.",
    ),
    ("c20_shape_subtract", "Cuts a second shape out of the area."),
    (
        "c20_shape_subtract.kind",
        "The second shape: an ellipse, a rectangle, or a polygon through the points below.",
    ),
    (
        "c20_shape_subtract.x",
        "Where the second shape's center lies in time, in thousandths of the box.",
    ),
    (
        "c20_shape_subtract.y",
        "Where the second shape's center lies in pitch, in thousandths of the box.",
    ),
    (
        "c20_shape_subtract.w",
        "How wide the second shape is, in thousandths of the box.",
    ),
    (
        "c20_shape_subtract.points",
        "The polygon's corners as x, y pairs in thousandths of the box, used when the second shape is a polygon.",
    ),
    (
        "c21_shape_intersection",
        "Keeps only the part of the area that a second shape also covers (an intersection).",
    ),
    (
        "c21_shape_intersection.kind",
        "The second shape: an ellipse, a rectangle, or a polygon through the points below.",
    ),
    (
        "c21_shape_intersection.x",
        "Where the second shape's center lies in time, in thousandths of the box.",
    ),
    (
        "c21_shape_intersection.y",
        "Where the second shape's center lies in pitch, in thousandths of the box.",
    ),
    (
        "c21_shape_intersection.w",
        "How wide the second shape is, in thousandths of the box.",
    ),
    (
        "c21_shape_intersection.points",
        "The polygon's corners as x, y pairs in thousandths of the box, used when the second shape is a polygon.",
    ),
    (
        "c22_repeat_tile",
        "Repeats the shape over the box like tiles.",
    ),
    (
        "c22_repeat_tile.layout",
        "How the tiles are laid out: a grid, bricks (every other row moved half a tile), or a grid with every other tile mirrored.",
    ),
    ("c22_repeat_tile.w", "How wide each tile is, in grid units."),
    ("c22_repeat_tile.h", "How tall each tile is, in rows."),
    (
        "c23_mesh_warp",
        "Bends the shape with a mesh over the box, moving each part by the mesh's offsets there.",
    ),
    (
        "c23_mesh_warp.smooth",
        "Bends smoothly between the mesh's points instead of in straight pieces.",
    ),
    (
        "c24_shape_a_b_morph",
        "Changes the shape gradually toward a second shape.",
    ),
    (
        "c24_shape_a_b_morph.target",
        "The second shape: an ellipse, a star, or a polygon through the points below.",
    ),
    (
        "c24_shape_a_b_morph.points",
        "The second shape's corners as x, y pairs in thousandths of the box, used with Polygon.",
    ),
    (
        "c24_shape_a_b_morph.amount",
        "How far the shape changes toward the second shape.",
    ),
    (
        "c25_save_load_custom_shape_presets",
        "Loads a named shape from a library you can edit and save with the tool's presets.",
    ),
];

fn custom_spec() -> ToolSpec {
    let mut own = vec![
        // c01
        flag("c01_arbitrary_vertex_placement", "Arbitrary Vertex Placement", false),
        list(
            "c01_arbitrary_vertex_placement.points",
            "Vertices (x, y pairs, ‰ of the box)",
            0,
            1000,
            2 * MAX_MAIN,
            &[400, 1000, 150, 450, 450, 450, 250, 0, 850, 600, 550, 600, 750, 1000],
        )
        .help("Each pair is a vertex anywhere in the box: x from its start, y from its bottom, in per mille. Three or more replace the arrow; they are joined in order (a path may cross itself — the fill rule decides what is inside)."),
        // c02
        flag("c02_bezier_handles_per_vertex", "Bezier Handles Per Vertex", false),
        choice("c02_bezier_handles_per_vertex.mode", "Handles", HANDLES, 0),
        float("c02_bezier_handles_per_vertex.smooth", "Smoothness", 0.0, 200.0, 100.0, "%")
            .help("Automatic handles along the line through each vertex's neighbors (100 %: a Catmull-Rom curve through the vertices)."),
        list(
            "c02_bezier_handles_per_vertex.handles",
            "Outgoing handle by vertex (x, y pairs, ‰ of the box; cycled)",
            -1000,
            1000,
            2 * MAX_MAIN,
            &[120, 0, 0, 120],
        )
        .help("Each vertex's outgoing handle (its incoming one mirrored, a smooth vertex); every edge becomes a cubic Bézier curve, flattened on the grid."),
        // c03
        flag("c03_open_path", "Open Path", false),
        int("c03_open_path.after", "Open after vertex", 1, MAX_MAIN as i64, MAX_MAIN as i64)
            .help("The main contour drawn as a path, the edge after this vertex left out (past the last vertex: the closing edge). An open path has no inside: it is a stroke the Stroke Thickness wide."),
        choice("c03_open_path.caps", "Ends", CAPS, 1),
        // c04
        flag("c04_closed_path", "Closed Path", false),
        choice("c04_closed_path.joins", "Corners", JOINS, 1).help(
            "Every contour drawn as a closed path: a stroke centered on its own line (where it crosses itself both crossings are drawn), instead of the area's outline. Mitres longer than 4 half-widths are bevelled.",
        ),
        // c05
        flag("c05_multiple_contours", "Multiple Contours", false),
        list(
            "c05_multiple_contours.points",
            "Extra contours' vertices (x, y pairs, ‰ of the box)",
            0,
            1000,
            2 * MAX_EXTRA,
            &[50, 820, 250, 820, 250, 980, 50, 980, 100, 30, 320, 30, 210, 200],
        ),
        list(
            "c05_multiple_contours.sizes",
            "Vertices per contour",
            3,
            MAX_EXTRA as i64,
            16,
            &[4, 3],
        )
        .help("The points are cut into contours of these sizes, in order (the rest, three or more, one more contour). They join the shape's area under its fill rule."),
        // c06
        flag("c06_interior_holes", "Interior Holes", false),
        list(
            "c06_interior_holes.points",
            "Holes' vertices (x, y pairs, ‰ of the box)",
            0,
            1000,
            2 * MAX_EXTRA,
            &[300, 380, 420, 500, 300, 620, 180, 500],
        ),
        list("c06_interior_holes.sizes", "Vertices per hole", 3, MAX_EXTRA as i64, 16, &[4])
            .help("Contours cut out of the shape whatever the fill rule; their edges are outlined like the shape's."),
        // c07
        flag("c07_outline_generation", "Outline Generation", false),
        choice("c07_outline_generation.placement", "Outline", PLACEMENT, 0).help(
            "Where the outline's cells lie about the area's boundary (holes and booleans included): around it, centered on it, or inside it (as without this function).",
        ),
        // c08
        flag("c08_filled_generation", "Filled Generation", false),
        flag("c08_filled_generation.outline", "Keep the outline", true)
            .help("The area filled; without the outline the whole area is one fill (an open path is still drawn)."),
        // c09
        flag("c09_stroke_thickness", "Stroke Thickness", false),
        int("c09_stroke_thickness.size", "Thickness (grid units)", 1, 64, 2)
            .help("The outline band's width, or a path's (centered on it)."),
        // c10
        flag("c10_configurable_fill_strategy", "Configurable Fill Strategy", false),
        choice("c10_configurable_fill_strategy.rule", "Fill rule", RULES, 1).help(
            "Even–odd: inside where a ray crosses the contours an odd number of times (overlaps cancel); non-zero: wherever the contours wind round (overlaps add; a contour drawn the other way round cuts a hole).",
        ),
        choice("c10_configurable_fill_strategy.pattern", "Interior", PATTERNS, 3),
        int("c10_configurable_fill_strategy.spacing", "Pattern spacing (cells)", 2, 64, 4)
            .help("The interior drawn whole or as rows, columns, diagonal lines or a crosshatch; with Filled Generation, between them."),
        // c11
        flag("c11_per_edge_density", "Per-Edge Density", false),
        list(
            "c11_per_edge_density.every",
            "A note every (cells) by edge (cycled)",
            1,
            64,
            128,
            &[1, 2, 3],
        )
        .help("Along each edge's outline, a cell every so many; edge k (the main contour's from its first vertex, then the extra contours', holes', operands') takes entry k, cycling."),
        // c12
        flag("c12_per_vertex_density", "Per-Vertex Density", false),
        list(
            "c12_per_vertex_density.share",
            "Share kept at each vertex (%, cycled)",
            0,
            100,
            128,
            &[100, 20],
        )
        .help("Each vertex's share of the outline's cells, blended along its edges to the next vertex's; cells are kept by an even dither."),
        // c13
        flag("c13_per_edge_subchannel", "Per-Edge Subchannel", false),
        list(
            "c13_per_edge_subchannel.subs",
            "Subchannel positions by edge (cycled)",
            0,
            4095,
            128,
            &[0, 1, 2, 3],
        )
        .help("Outline cells take their nearest edge's entry (edge k, entry k, cycling)."),
        // c14
        flag("c14_per_vertex_velocity", "Per-Vertex Velocity", false),
        list(
            "c14_per_vertex_velocity.vel",
            "Velocity at each vertex (cycled)",
            1,
            127,
            128,
            &[127, 30],
        )
        .help("Outline cells take the velocity blended along their nearest edge from its start vertex's to its end vertex's."),
        // c15
        flag("c15_gate_profile", "Gate Profile", false),
        curve(
            "c15_gate_profile.curve",
            "Gate by position",
            Curve::new(vec![(0.0, 1.0), (0.5, 0.2), (1.0, 1.0)]),
        )
        .help("x: the position (see below); y: the gate, from 1 % (0) to 100 % (1)."),
        choice("c15_gate_profile.by", "Position", PROFILE_BY, 0),
        // c16
        flag("c16_full_transform", "Full Transform", false),
        float("c16_full_transform.dx", "Move x (% of the box's width)", -100.0, 100.0, 10.0, "%"),
        float("c16_full_transform.dy", "Move y (% of the box's height)", -100.0, 100.0, 0.0, "%"),
        float("c16_full_transform.angle", "Rotation", -180.0, 180.0, 20.0, "°"),
        float("c16_full_transform.sx", "Scale x", 5.0, 400.0, 80.0, "%"),
        float("c16_full_transform.sy", "Scale y", 5.0, 400.0, 80.0, "%"),
        float("c16_full_transform.shear", "Shear", -75.0, 75.0, 0.0, "°").help(
            "About the box's center on the grid (a step across is a row up): scaled, sheared (rows above the center later), turned counter-clockwise, then moved.",
        ),
        // c17
        flag("c17_mirror_symmetry", "Mirror Symmetry", false),
        choice("c17_mirror_symmetry.axis", "Mirror", AXES, 0),
        float("c17_mirror_symmetry.offset", "Axis from the center (% of the box)", -50.0, 50.0, 0.0, "%")
            .help("The shape united with its mirror image (two axes: three images)."),
        // c18
        flag("c18_radial_symmetry", "Radial Symmetry", false),
        int("c18_radial_symmetry.count", "Copies round the center", 2, 32, 4),
        flag("c18_radial_symmetry.alternate", "Mirror every other copy", false)
            .help("The shape united with copies turned evenly about the box's center on the grid (a kaleidoscope with mirroring)."),
    ];
    for (id, _, def) in BOOLEANS {
        let label = FUNCTIONS
            .iter()
            .find(|f| f.0 == id)
            .map_or("Shape", |f| f.1);
        own.extend(operand_specs(id, label, def));
    }
    own.extend([
        // c22
        flag("c22_repeat_tile", "Repeat / Tile", false),
        choice("c22_repeat_tile.layout", "Layout", TILES, 0),
        float("c22_repeat_tile.w", "Tile width (grid units)", 1.0, 4096.0, 8.0, ""),
        float("c22_repeat_tile.h", "Tile height (rows)", 1.0, 256.0, 8.0, ""),
        float("c22_repeat_tile.gap", "Gap (grid units)", 0.0, 256.0, 1.0, "").help(
            "The shape built for a tile of this size (its per mille of the tile), repeated over the box: as many tiles as fit, centered.",
        ),
        // c23
        flag("c23_mesh_warp", "Mesh Warp", false),
        list(
            "c23_mesh_warp.nodes",
            "Node offsets ‰ (x, y per node, row by row from the bottom)",
            -1000,
            1000,
            162,
            &[0, 0, 0, 0, 0, 0, 0, 0, 150, 100, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .help("A square mesh over the box (its side from the list's length, 2 – 9 nodes); each point of the shape moves by the mesh's offset there (Warp's mesh)."),
        flag("c23_mesh_warp.smooth", "Smooth mesh (Catmull-Rom)", false),
        // c24
        flag("c24_shape_a_b_morph", "Shape A→B Morph", false),
        choice("c24_shape_a_b_morph.target", "Shape B", TARGETS, 0),
        list(
            "c24_shape_a_b_morph.points",
            "Shape B polygon (x, y pairs, ‰ of the box)",
            0,
            1000,
            2 * MAX_MAIN,
            &[200, 200, 800, 200, 800, 800, 200, 800],
        ),
        float("c24_shape_a_b_morph.amount", "Morph", 0.0, 100.0, 50.0, "%"),
        int("c24_shape_a_b_morph.steps", "Shapes drawn", 1, 16, 1).help(
            "The main contour moved towards shape B point by point along their lengths (Arcs' morph by arc length). One shape: at the morph's share; more: evenly from the shape (0 %) to that share, each its own outline.",
        ),
        // c25
        flag("c25_save_load_custom_shape_presets", "Save/Load Custom Shape Presets", false),
        text("c25_save_load_custom_shape_presets.library", "Shape library", 4096, LIBRARY).help(
            "Named shapes: `Name: x,y x,y x,y …` in per mille of the box, `|` between a shape's contours, `;` between shapes. Saved and loaded with the tool's presets; add your own shapes here.",
        ),
        text("c25_save_load_custom_shape_presets.name", "Shape to load", 64, "Heart").help(
            "The library's shape that replaces the arrow (and Arbitrary Vertex Placement's vertices): its first contour is the main one, the others extra contours.",
        ),
    ]);
    with_help(&mut own, CUSTOM_HELP);
    super::spec(
        "gen.custom",
        "Custom Shape",
        "Generates a shape of your own: an arrow in the generation box to start from (where, length and pitch range above), its outline one cell wide on the resolution grid — vertices, curves, contours, holes, booleans, symmetry, tiles, warps and morphs below.",
        own,
        FUNCTIONS,
    )
}

/// A boolean operand (box fractions).
#[derive(Clone, Debug)]
struct Operand {
    kind: usize,
    centre: P2,
    size: P2,
    pts: Vec<P2>,
}

/// Bézier handles.
#[derive(Clone, Debug)]
enum Handles {
    /// Catmull-Rom-like, scaled.
    Auto(f64),
    /// Outgoing handles in box fractions, cycled.
    List(Vec<P2>),
}

/// The functions' settings (`None` / neutral = off).
#[derive(Clone, Debug, Default)]
struct Cfg {
    /// The main contour, extra contours, holes (box fractions).
    main: Vec<P2>,
    extra: Vec<Vec<P2>>,
    holes: Vec<Vec<P2>>,
    bezier: Option<Handles>,
    /// c03: open after vertex (0-based, clamped later), the caps.
    open: Option<(usize, Cap)>,
    /// c04.
    closed: Option<Join>,
    place: Place,
    /// c08: keep the outline?
    filled: Option<bool>,
    thick: f64,
    rule: Rule,
    /// c10: pattern, spacing.
    pattern: Option<(usize, i64)>,
    every: Option<Vec<f64>>,
    vshare: Option<Vec<f64>>,
    subs: Option<Vec<u32>>,
    vvel: Option<Vec<f64>>,
    gate: Option<(Curve, GDir)>,
    /// c16: move (fractions), angle, scale x, y, shear (radians).
    xform: Option<[f64; 6]>,
    /// c17: axis, offset (fraction).
    mirror: Option<(usize, f64)>,
    /// c18: copies, alternate mirrors.
    radial: Option<(u32, bool)>,
    ops: Vec<(Op, Operand)>,
    /// c22: layout, width, height, gap.
    tile: Option<(usize, f64, f64, f64)>,
    /// c23: the mesh, smooth.
    warp: Option<(Mesh, bool)>,
    /// B2-F01: c18's turned copies before c17's mirrors
    /// (`order::CUSTOM_SYMMETRY`; declared: the mirrors first).
    radial_first: bool,
    /// B2-F01: c16's transform before c23's mesh warp (`order::CUSTOM_MAPS`;
    /// declared: the warp first).
    xform_first: bool,
    /// c24: target kind, its points, amount, steps.
    morph: Option<(usize, Vec<P2>, f64, u32)>,
}

fn frac(q: &[i64]) -> P2 {
    (
        q[0].clamp(0, 1000) as f64 / 1000.0,
        q[1].clamp(0, 1000) as f64 / 1000.0,
    )
}

/// Points cut into contours of `sizes` (the rest, three or more, one more).
fn split(points: &[i64], sizes: &[i64], most: usize) -> Vec<Vec<P2>> {
    let pts: Vec<P2> = points.chunks_exact(2).take(most).map(frac).collect();
    let mut out = Vec::new();
    let (mut i, mut k) = (0, 0);
    while i < pts.len() {
        let left = pts.len() - i;
        let n = sizes
            .get(k)
            .map_or(left, |&s| s.clamp(3, most as i64) as usize)
            .min(left);
        if n >= 3 {
            out.push(pts[i..i + n].to_vec());
        }
        i += n;
        k += 1;
    }
    out
}

/// The library's shapes: names and contours (box fractions), or the
/// sentence saying what is wrong.
/// Named shapes: each name with its contours (box fractions).
pub type Library = Vec<(String, Vec<Vec<P2>>)>;

pub fn parse_library(text: &str) -> Result<Library, String> {
    let mut out = Vec::new();
    for (k, entry) in text.split(';').enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((name, body)) = entry.split_once(':') else {
            return Err(format!(
                "Shape {} of the library has no name — write it as Name: x,y x,y x,y.",
                k + 1
            ));
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(format!("Shape {} of the library has an empty name.", k + 1));
        }
        let mut contours = Vec::new();
        for c in body.split('|') {
            let mut pts = Vec::new();
            for tok in c.split_whitespace() {
                let parsed = tok.split_once(',').and_then(|(x, y)| {
                    Some((x.trim().parse::<i64>().ok()?, y.trim().parse::<i64>().ok()?))
                });
                let Some((x, y)) = parsed else {
                    return Err(format!(
                        "In the library's shape “{name}”, “{tok}” is not a point (write x,y in per mille)."
                    ));
                };
                pts.push(frac(&[x, y]));
            }
            if pts.len() < 3 {
                return Err(format!(
                    "In the library's shape “{name}”, each contour needs three or more points."
                ));
            }
            contours.push(pts);
        }
        out.push((name, contours));
    }
    Ok(out)
}

impl Cfg {
    fn read(p: &Params) -> Result<Cfg, TransformError> {
        let on = |id: &str| p.flag(id);
        let mut c = Cfg {
            main: BASE.chunks_exact(2).map(frac).collect(),
            thick: 1.0,
            ..Cfg::default()
        };
        if on("c01_arbitrary_vertex_placement") {
            let v = p.list("c01_arbitrary_vertex_placement.points");
            let pts: Vec<P2> = v.chunks_exact(2).take(MAX_MAIN).map(frac).collect();
            if pts.len() >= 3 {
                c.main = pts;
            }
        }
        if on("c25_save_load_custom_shape_presets") {
            let k = "c25_save_load_custom_shape_presets";
            let lib =
                parse_library(&p.text(&format!("{k}.library"))).map_err(TransformError::Invalid)?;
            let want = p.text(&format!("{k}.name"));
            let want = want.trim();
            let Some((_, cs)) = lib.iter().find(|(n, _)| n.eq_ignore_ascii_case(want)) else {
                let names: Vec<&str> = lib.iter().map(|(n, _)| n.as_str()).collect();
                return Err(TransformError::Invalid(if names.is_empty() {
                    "The shape library is empty — add a shape as Name: x,y x,y x,y.".into()
                } else {
                    format!(
                        "The shape library has no shape named “{want}” — it has {}.",
                        names.join(", ")
                    )
                }));
            };
            c.main = cs[0].iter().copied().take(MAX_MAIN).collect();
            let mut left = MAX_EXTRA;
            for x in &cs[1..] {
                if x.len() > left {
                    break;
                }
                left -= x.len();
                c.extra.push(x.clone());
            }
        }
        if on("c02_bezier_handles_per_vertex") {
            let k = "c02_bezier_handles_per_vertex";
            c.bezier = Some(if p.choice(&format!("{k}.mode")) == 1 {
                let v: Vec<P2> = p
                    .list(&format!("{k}.handles"))
                    .chunks_exact(2)
                    .map(|q| {
                        (
                            q[0].clamp(-1000, 1000) as f64 / 1000.0,
                            q[1].clamp(-1000, 1000) as f64 / 1000.0,
                        )
                    })
                    .collect();
                if v.is_empty() {
                    Handles::Auto(1.0)
                } else {
                    Handles::List(v)
                }
            } else {
                Handles::Auto((p.float(&format!("{k}.smooth")) / 100.0).clamp(0.0, 2.0))
            });
        }
        if on("c03_open_path") {
            let cap = match p.choice("c03_open_path.caps") {
                0 => Cap::Butt,
                2 => Cap::Square,
                _ => Cap::Round,
            };
            let after = p.int("c03_open_path.after").clamp(1, MAX_MAIN as i64) as usize - 1;
            c.open = Some((after, cap));
        }
        if on("c04_closed_path") {
            c.closed = Some(match p.choice("c04_closed_path.joins") {
                0 => Join::Round,
                2 => Join::Bevel,
                _ => Join::Mitre,
            });
        }
        if on("c05_multiple_contours") {
            let left = MAX_EXTRA - c.extra.iter().map(Vec::len).sum::<usize>();
            c.extra.extend(split(
                &p.list("c05_multiple_contours.points"),
                &p.list("c05_multiple_contours.sizes"),
                left,
            ));
        }
        if on("c06_interior_holes") {
            c.holes = split(
                &p.list("c06_interior_holes.points"),
                &p.list("c06_interior_holes.sizes"),
                MAX_EXTRA,
            );
        }
        if on("c07_outline_generation") {
            c.place = match p.choice("c07_outline_generation.placement") {
                0 => Place::Outside,
                1 => Place::Centred,
                _ => Place::Inside,
            };
        }
        if on("c08_filled_generation") {
            c.filled = Some(p.flag("c08_filled_generation.outline"));
        }
        if on("c09_stroke_thickness") {
            c.thick = p.int("c09_stroke_thickness.size").clamp(1, 64) as f64;
        }
        if on("c10_configurable_fill_strategy") {
            let k = "c10_configurable_fill_strategy";
            if p.choice(&format!("{k}.rule")) == 1 {
                c.rule = Rule::NonZero;
            }
            c.pattern = Some((
                p.choice(&format!("{k}.pattern")).min(4),
                p.int(&format!("{k}.spacing")).clamp(2, 64),
            ));
        }
        let cycled = |key: &str, lo: i64, hi: i64, empty: f64| -> Vec<f64> {
            let v = p.list(key);
            if v.is_empty() {
                vec![empty]
            } else {
                v.iter().map(|&x| x.clamp(lo, hi) as f64).collect()
            }
        };
        if on("c11_per_edge_density") {
            c.every = Some(cycled("c11_per_edge_density.every", 1, 64, 1.0));
        }
        if on("c12_per_vertex_density") {
            c.vshare = Some(
                cycled("c12_per_vertex_density.share", 0, 100, 100.0)
                    .into_iter()
                    .map(|x| x / 100.0)
                    .collect(),
            );
        }
        if on("c13_per_edge_subchannel") {
            c.subs = Some(
                cycled("c13_per_edge_subchannel.subs", 0, 4095, 0.0)
                    .into_iter()
                    .map(|x| x as u32)
                    .collect(),
            );
        }
        if on("c14_per_vertex_velocity") {
            c.vvel = Some(cycled("c14_per_vertex_velocity.vel", 1, 127, 100.0));
        }
        if on("c15_gate_profile") {
            let by = match p.choice("c15_gate_profile.by") {
                1 => GDir::Edge,
                2 => GDir::LeftRight,
                3 => GDir::BottomTop,
                4 => GDir::Radial,
                _ => GDir::Around,
            };
            c.gate = Some((p.curve("c15_gate_profile.curve"), by));
        }
        if on("c16_full_transform") {
            let k = "c16_full_transform";
            let f = |s: &str, lo: f64, hi: f64| p.float(&format!("{k}.{s}")).clamp(lo, hi);
            c.xform = Some([
                f("dx", -100.0, 100.0) / 100.0,
                f("dy", -100.0, 100.0) / 100.0,
                f("angle", -180.0, 180.0).to_radians(),
                f("sx", 5.0, 400.0) / 100.0,
                f("sy", 5.0, 400.0) / 100.0,
                f("shear", -75.0, 75.0).to_radians(),
            ]);
        }
        if on("c17_mirror_symmetry") {
            c.mirror = Some((
                p.choice("c17_mirror_symmetry.axis").min(2),
                (p.float("c17_mirror_symmetry.offset") / 100.0).clamp(-0.5, 0.5),
            ));
        }
        if on("c18_radial_symmetry") {
            c.radial = Some((
                p.int("c18_radial_symmetry.count").clamp(2, 32) as u32,
                p.flag("c18_radial_symmetry.alternate"),
            ));
        }
        for (id, op, _) in BOOLEANS {
            if !on(id) {
                continue;
            }
            let g = |s: &str| p.int(&format!("{id}.{s}"));
            let pts: Vec<P2> = p
                .list(&format!("{id}.points"))
                .chunks_exact(2)
                .take(MAX_OPERAND)
                .map(frac)
                .collect();
            c.ops.push((
                op,
                Operand {
                    kind: p.choice(&format!("{id}.kind")).min(2),
                    centre: (
                        g("x").clamp(0, 1000) as f64 / 1000.0,
                        g("y").clamp(0, 1000) as f64 / 1000.0,
                    ),
                    size: (
                        g("w").clamp(1, 2000) as f64 / 1000.0,
                        g("h").clamp(1, 2000) as f64 / 1000.0,
                    ),
                    pts,
                },
            ));
        }
        if on("c22_repeat_tile") {
            c.tile = Some((
                p.choice("c22_repeat_tile.layout").min(2),
                p.float("c22_repeat_tile.w").clamp(1.0, 4096.0),
                p.float("c22_repeat_tile.h").clamp(1.0, 256.0),
                p.float("c22_repeat_tile.gap").clamp(0.0, 256.0),
            ));
        }
        if on("c23_mesh_warp") {
            let v = p.list("c23_mesh_warp.nodes");
            let side = (((v.len() / 2) as f64).sqrt() as usize).clamp(2, 9);
            c.warp = Some((
                Mesh::from_list(side, side, &v),
                p.flag("c23_mesh_warp.smooth"),
            ));
        }
        c.radial_first = p.sequence(crate::transform::order::CUSTOM_SYMMETRY.members)[0]
            == "c18_radial_symmetry";
        c.xform_first =
            p.sequence(crate::transform::order::CUSTOM_MAPS.members)[0] == "c16_full_transform";
        if on("c24_shape_a_b_morph") {
            let k = "c24_shape_a_b_morph";
            let pts: Vec<P2> = p
                .list(&format!("{k}.points"))
                .chunks_exact(2)
                .take(MAX_MAIN)
                .map(frac)
                .collect();
            c.morph = Some((
                p.choice(&format!("{k}.target")).min(2),
                pts,
                (p.float(&format!("{k}.amount")) / 100.0).clamp(0.0, 1.0),
                p.int(&format!("{k}.steps")).clamp(1, 16) as u32,
            ));
        }
        Ok(c)
    }

    /// The morph's shares, one per shape drawn (none without c24).
    fn shares(&self) -> Vec<Option<f64>> {
        match &self.morph {
            None => vec![None],
            Some((_, _, amt, steps)) if *steps <= 1 => vec![Some(*amt)],
            Some((_, _, amt, steps)) => (0..*steps)
                .map(|k| Some(amt * f64::from(k) / f64::from(steps - 1)))
                .collect(),
        }
    }

    /// The operand's vertex count (its edges for numbering).
    fn operand_edges(o: &Operand) -> usize {
        if o.kind == 2 && o.pts.len() >= 3 {
            o.pts.len()
        } else {
            4
        }
    }

    /// Each edge's start and end vertex (global numbering: the main
    /// contour, extra contours, holes, operands).
    fn edge_ends(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut add = |n: usize| {
            let base = out.len();
            for i in 0..n {
                out.push((base + i, base + (i + 1) % n));
            }
        };
        add(self.main.len());
        self.extra.iter().for_each(|c| add(c.len()));
        self.holes.iter().for_each(|c| add(c.len()));
        self.ops
            .iter()
            .for_each(|(_, o)| add(Cfg::operand_edges(o)));
        out
    }
}

fn cubic(p0: P2, p1: P2, p2: P2, p3: P2, t: f64) -> P2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
    )
}

/// A contour through `pts` (local), edges tagged from `tag0`, its edges
/// Bézier curves when `handles` (box `w` × `h` for listed handles).
fn contour(
    pts: &[P2],
    closed: bool,
    tag0: usize,
    handles: Option<&Handles>,
    w: f64,
    h: f64,
) -> Contour {
    let Some(hs) = handles else {
        return Contour::polygon(pts.to_vec(), tag0, closed);
    };
    let n = pts.len();
    let hd: Vec<P2> = (0..n)
        .map(|i| match hs {
            Handles::Auto(s) => {
                let (a, b) = if closed {
                    (pts[(i + n - 1) % n], pts[(i + 1) % n])
                } else {
                    (pts[i.saturating_sub(1)], pts[(i + 1).min(n - 1)])
                };
                ((b.0 - a.0) * s / 6.0, (b.1 - a.1) * s / 6.0)
            }
            Handles::List(v) => {
                let q = v[i % v.len()];
                (q.0 * w, q.1 * h)
            }
        })
        .collect();
    let m = if closed { n } else { n - 1 };
    let mut c = Contour {
        closed,
        ..Contour::default()
    };
    for i in 0..m {
        let j = (i + 1) % n;
        let (p0, p3) = (pts[i], pts[j]);
        let p1 = (p0.0 + hd[i].0, p0.1 + hd[i].1);
        let p2 = (p3.0 - hd[j].0, p3.1 - hd[j].1);
        let fl = flatten(&|t| cubic(p0, p1, p2, p3, t));
        let tag = ((tag0 + i) % 256) as u8;
        for &q in &fl[..fl.len() - 1] {
            c.pts.push(q);
            c.tags.push(tag);
            c.chords.push((p0, p3));
        }
    }
    if !closed {
        c.pts.push(pts[n - 1]);
    }
    c
}

/// `n` points evenly along a contour by length (closed: round it), each
/// with the tag and chord of the edge it lies on.
fn along(c: &Contour, n: usize) -> Contour {
    let k = c.edges();
    let lens: Vec<f64> = (0..k)
        .map(|i| {
            let (a, b) = c.edge(i);
            (b.0 - a.0).hypot(b.1 - a.1)
        })
        .collect();
    let total: f64 = lens.iter().sum();
    let mut out = Contour {
        closed: c.closed,
        ..Contour::default()
    };
    if k == 0 || !(total > 0.0 && total.is_finite()) {
        return c.clone();
    }
    let count = if c.closed { n } else { n + 1 };
    let (mut i, mut acc) = (0, 0.0);
    for j in 0..count {
        let want = total * j as f64 / n as f64;
        while i + 1 < k && acc + lens[i] < want {
            acc += lens[i];
            i += 1;
        }
        let t = if lens[i] > 0.0 {
            ((want - acc) / lens[i]).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (a, b) = c.edge(i);
        out.pts.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        if j < n {
            out.tags.push(c.tags[i]);
            out.chords.push(c.chords[i]);
        }
    }
    out
}

/// An ellipse (`n` points, counter-clockwise from its right) whose
/// quarters are tagged `tag0 … tag0 + 3`.
fn ellipse(centre: P2, r: P2, n: usize, tag0: usize) -> Contour {
    let q = |k: usize| {
        let a = TAU * k as f64 / 4.0;
        (centre.0 + r.0 * a.cos(), centre.1 + r.1 * a.sin())
    };
    let mut c = Contour {
        closed: true,
        ..Contour::default()
    };
    for i in 0..n {
        let a = TAU * i as f64 / n as f64;
        let quarter = i * 4 / n;
        c.pts
            .push((centre.0 + r.0 * a.cos(), centre.1 + r.1 * a.sin()));
        c.tags.push(((tag0 + quarter) % 256) as u8);
        c.chords.push((q(quarter), q(quarter + 1)));
    }
    c
}

/// A five-pointed star inscribed in the box (`w` × `h`, local).
fn star(w: f64, h: f64) -> Vec<P2> {
    (0..10)
        .map(|i| {
            let a = FRAC_PI_2 + TAU * i as f64 / 10.0;
            let r = if i % 2 == 0 { 0.5 } else { 0.2 };
            (r * w * a.cos(), r * h * a.sin())
        })
        .collect()
}

/// The main contour morphed share `t` of the way to `target` (local
/// points) by arc length.
fn morph(main: &Contour, target: &[P2], t: f64) -> Contour {
    let a = along(main, MORPH_POINTS);
    // The target round its loop, the same count, same orientation, its
    // start nearest the shape's.
    let mut ring: Vec<P2> = target.to_vec();
    if let Some(&f) = ring.first() {
        ring.push(f);
    }
    let pts: Vec<Pt> = ring.iter().map(|&(x, y)| Pt { x, y, s: 0.0 }).collect();
    let mut b: Vec<P2> = resample(&pts, MORPH_POINTS)
        .iter()
        .map(|p| (p.x, p.y))
        .collect();
    b.pop();
    if b.len() != MORPH_POINTS || a.pts.is_empty() {
        return main.clone();
    }
    let signed = |v: &[P2]| Contour::polygon(v.to_vec(), 0, true).area();
    if main.closed && (signed(&a.pts) > 0.0) != (signed(&b) > 0.0) {
        b.reverse();
    }
    let a0 = a.pts[0];
    let r = (0..b.len())
        .min_by(|&i, &j| {
            let d = |q: P2| (q.0 - a0.0).hypot(q.1 - a0.1);
            d(b[i]).total_cmp(&d(b[j]))
        })
        .unwrap_or(0);
    let mut out = a.clone();
    let m = b.len();
    for (j, p) in out.pts.iter_mut().enumerate() {
        let q = b[(r + j) % m];
        *p = (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
    }
    out
}

/// The Custom Shape generator.
struct Custom {
    cfg: Cfg,
    built: OnceLock<Result<(Scene, Style), TransformError>>,
}

/// One shape, local: its area (instances), strokes, centre and first
/// corner.
struct Local {
    spec: GeomSpec,
    centre: P2,
    corner: P2,
}

impl Custom {
    fn new(cfg: Cfg) -> Custom {
        Custom {
            cfg,
            built: OnceLock::new(),
        }
    }

    /// The box in grid space: `[u0, v0, u1, v1]`.
    fn bx(f: &Frame) -> [f64; 4] {
        let step = f.step as f64;
        [
            f.t0 as f64 / step,
            f64::from(f.lo),
            f.t1 as f64 / step,
            f64::from(f.hi) + 1.0,
        ]
    }

    /// The point maps after the shape is built: the mesh warp over the box
    /// (`w` × `h`), then the full transform.
    fn map(&self, q: P2, w: f64, h: f64) -> P2 {
        let c = &self.cfg;
        let warp = |q: P2| match &c.warp {
            Some((m, smooth)) => {
                let (dx, dy) = m.at(q.0 / w + 0.5, q.1 / h + 0.5, *smooth);
                (q.0 + dx * w, q.1 + dy * h)
            }
            None => q,
        };
        let xform = |q: P2| match c.xform {
            Some([dx, dy, a, sx, sy, sh]) => {
                let (x, y) = (q.0 * sx, q.1 * sy);
                let x = x + y * sh.tan();
                let (s, co) = a.sin_cos();
                (x * co - y * s + dx * w, x * s + y * co + dy * h)
            }
            None => q,
        };
        // B2-F01: in the user's order (declared: the warp, then the transform).
        if c.xform_first {
            warp(xform(q))
        } else {
            xform(warp(q))
        }
    }

    /// The shape for a box of `w` × `h` grid units about (0, 0), morphed
    /// by share `t`.
    fn local(&self, w: f64, h: f64, t: Option<f64>) -> Result<Local, TransformError> {
        let c = &self.cfg;
        let at = |f: P2| ((f.0 - 0.5) * w, (f.1 - 0.5) * h);
        let hs = c.bezier.as_ref();
        let mut tag = 0usize;
        // The main contour (open: from the vertex after the gap).
        let n = c.main.len();
        let mut main_pts: Vec<P2> = c.main.iter().map(|&f| at(f)).collect();
        let open = c.open.map(|(after, _)| after.min(n - 1));
        let mut main = match open {
            Some(k) => {
                main_pts.rotate_left((k + 1) % n);
                let mut m = contour(&main_pts, false, 0, hs, w, h);
                // Tags as on the closed contour.
                for t in m.tags.iter_mut() {
                    *t = ((usize::from(*t) + k + 1) % n % 256) as u8;
                }
                m
            }
            None => contour(&main_pts, true, 0, hs, w, h),
        };
        tag += n;
        if let (Some(share), Some((kind, pts, _, _))) = (t, &c.morph) {
            let target: Vec<P2> = match kind {
                1 => star(w, h),
                2 if pts.len() >= 3 => pts.iter().map(|&f| at(f)).collect(),
                _ => (0..ELLIPSE_POINTS)
                    .map(|i| {
                        let a = FRAC_PI_2 + TAU * i as f64 / ELLIPSE_POINTS as f64;
                        (w / 2.0 * a.cos(), h / 2.0 * a.sin())
                    })
                    .collect(),
            };
            if share > 0.0 {
                main = morph(&main, &target, share);
            }
        }
        let corner = main.pts.first().copied().unwrap_or((0.0, 0.0));
        let mut layer = Vec::new();
        let mut strokes = Vec::new();
        if main.closed {
            layer.push(main);
        } else {
            strokes.push(main);
        }
        for x in &c.extra {
            let pts: Vec<P2> = x.iter().map(|&f| at(f)).collect();
            layer.push(contour(&pts, true, tag, hs, w, h));
            tag += x.len();
        }
        let mut holes = Vec::new();
        for x in &c.holes {
            let pts: Vec<P2> = x.iter().map(|&f| at(f)).collect();
            holes.push(contour(&pts, true, tag, hs, w, h));
            tag += x.len();
        }
        let mut ops = Vec::new();
        for (op, o) in &c.ops {
            let (cx, cy) = at(o.centre);
            let (sw, sh) = (o.size.0 * w / 2.0, o.size.1 * h / 2.0);
            let ct = match o.kind {
                2 if o.pts.len() >= 3 => {
                    let pts: Vec<P2> = o.pts.iter().map(|&f| at(f)).collect();
                    Contour::polygon(pts, tag, true)
                }
                1 => Contour::polygon(
                    vec![
                        (cx - sw, cy - sh),
                        (cx + sw, cy - sh),
                        (cx + sw, cy + sh),
                        (cx - sw, cy + sh),
                    ],
                    tag,
                    true,
                ),
                _ => ellipse((cx, cy), (sw, sh), ELLIPSE_POINTS, tag),
            };
            tag += Cfg::operand_edges(o);
            ops.push((*op, vec![ct]));
        }
        let fig = Figure {
            layers: if layer.is_empty() {
                Vec::new()
            } else {
                vec![(layer, c.rule)]
            },
            holes,
            ops,
        };
        // Symmetry: the instances' maps — c17's mirrors and c18's turned
        // copies, each over the instances so far, in the user's order
        // (B2-F01; declared: the mirrors first).
        type PMap = std::rc::Rc<dyn Fn(P2) -> P2>;
        let mirror = |base: Vec<PMap>| -> Vec<PMap> {
            let Some((axis, off)) = c.mirror else {
                return base;
            };
            let (ax, ay) = (off * w, off * h);
            let mx = move |q: P2| (2.0 * ax - q.0, q.1);
            let my = move |q: P2| (q.0, 2.0 * ay - q.1);
            let mut out: Vec<PMap> = Vec::with_capacity(base.len() * 4);
            for m in base {
                out.push(m.clone());
                let (m1, m2, m3) = (m.clone(), m.clone(), m);
                match axis {
                    0 => out.push(std::rc::Rc::new(move |q| mx(m1(q)))),
                    1 => out.push(std::rc::Rc::new(move |q| my(m1(q)))),
                    _ => {
                        out.push(std::rc::Rc::new(move |q| mx(m1(q))));
                        out.push(std::rc::Rc::new(move |q| my(m2(q))));
                        out.push(std::rc::Rc::new(move |q| my(mx(m3(q)))));
                    }
                }
            }
            out
        };
        let radial = |base: Vec<PMap>| -> Vec<PMap> {
            let Some((count, alt)) = c.radial else {
                return base;
            };
            let mut out: Vec<PMap> = Vec::with_capacity(base.len() * count as usize);
            for m in base {
                for j in 0..count {
                    let a = TAU * f64::from(j) / f64::from(count);
                    let (s, co) = a.sin_cos();
                    let flip = alt && j % 2 == 1;
                    let m = m.clone();
                    out.push(std::rc::Rc::new(move |q| {
                        let q = m(q);
                        let q = if flip { (-q.0, q.1) } else { q };
                        (q.0 * co - q.1 * s, q.0 * s + q.1 * co)
                    }));
                }
            }
            out
        };
        let id: Vec<PMap> = vec![std::rc::Rc::new(|q| q)];
        let maps = if c.radial_first {
            mirror(radial(id))
        } else {
            radial(mirror(id))
        };
        let warp = c.warp.is_some();
        // The segments the instances will have (a warped edge cut into unit
        // pieces, at most 64), refused before anything is copied or warped.
        let pieces = |ct: &Contour| -> usize {
            if !warp {
                return ct.edges();
            }
            (0..ct.edges())
                .map(|i| {
                    let (a, b) = ct.edge(i);
                    let l = (b.0 - a.0).hypot(b.1 - a.1);
                    if l.is_finite() {
                        ((l / WARP_STEP).ceil() as usize).clamp(1, WARP_MOST)
                    } else {
                        1
                    }
                })
                .sum()
        };
        let per = fig.contours().map(pieces).sum::<usize>()
            * if c.closed.is_some() { 2 } else { 1 }
            + strokes.iter().map(pieces).sum::<usize>();
        let total = per.saturating_mul(maps.len());
        if total > SEG_CAP {
            return Err(TransformError::TooLarge(area::too_many(total)));
        }
        let place = |q: P2| self.map(q, w, h);
        let finish = |ct: &Contour, m: &dyn Fn(P2) -> P2| {
            let ct = ct.map(m);
            if warp {
                ct.densify(WARP_STEP, WARP_MOST).map(place)
            } else {
                ct.map(place)
            }
        };
        let mut area = Vec::new();
        let mut paths = Vec::new();
        for m in &maps {
            let mut f = fig.clone();
            for ct in f.contours_mut() {
                *ct = finish(ct, m.as_ref());
            }
            area.push(f);
            for s in &strokes {
                paths.push(finish(s, m.as_ref()));
            }
        }
        let outline = c.filled != Some(false);
        if c.closed.is_some() && outline {
            for f in &area {
                paths.extend(f.contours().cloned());
            }
        }
        let band = (c.closed.is_none() && outline).then_some((c.place, c.thick));
        Ok(Local {
            spec: GeomSpec {
                area: if fig.layers.is_empty() && fig.ops.iter().all(|(o, _)| *o != Op::Union) {
                    Vec::new()
                } else {
                    area
                },
                band,
                strokes: paths,
                pen: (
                    c.thick / 2.0,
                    c.closed.unwrap_or(Join::Round),
                    c.open.map_or(Cap::Round, |o| o.1),
                ),
            },
            centre: place((0.0, 0.0)),
            corner: place(corner),
        })
    }

    /// The tiles' centres (and whether each is mirrored) for box `b`, or
    /// the cap's sentence.
    fn tiles(&self, b: [f64; 4]) -> Result<Vec<(P2, bool)>, TransformError> {
        let centre = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        let Some((layout, w, h, gap)) = self.cfg.tile else {
            return Ok(vec![(centre, false)]);
        };
        let (px, py) = (w + gap, h + gap);
        let count = |len: f64, p: f64| ((len + gap) / p).floor().max(1.0);
        let (nx, ny) = (count(b[2] - b[0], px), count(b[3] - b[1], py));
        let shares = self.cfg.shares().len() as f64;
        let total = nx * ny * shares;
        if !(total.is_finite() && total <= TILE_CAP as f64) {
            return Err(TransformError::TooLarge(format!(
                "Repeat / Tile would draw {} shapes; at most {} are allowed — use larger tiles or gaps, a smaller box or fewer morph shapes.",
                group_digits(if total.is_finite() && total < 1e18 {
                    total as u64
                } else {
                    u64::MAX
                }),
                group_digits(TILE_CAP)
            )));
        }
        let (nx, ny) = (nx as usize, ny as usize);
        let mut out = Vec::with_capacity(nx * ny);
        for j in 0..ny {
            let y = centre.1 + (j as f64 - (ny as f64 - 1.0) / 2.0) * py;
            let shift = if layout == 1 && j % 2 == 1 {
                px / 2.0
            } else {
                0.0
            };
            for i in 0..nx {
                let x = centre.0 + (i as f64 - (nx as f64 - 1.0) / 2.0) * px + shift;
                out.push(((x, y), layout == 2 && (i + j) % 2 == 1));
            }
        }
        Ok(out)
    }

    /// The scene and style, or a cap's sentence.
    fn build(&self, f: &Frame) -> Result<(Scene, Style), TransformError> {
        let c = &self.cfg;
        let b = Self::bx(f);
        let tiles = self.tiles(b)?;
        let (w, h) = match c.tile {
            Some((_, tw, th, _)) => (tw, th),
            None => (b[2] - b[0], b[3] - b[1]),
        };
        let mut geoms = Vec::new();
        let mut copies = Vec::new();
        let (sx, sy, rot) = c.xform.map_or((1.0, 1.0, 0.0), |x| (x[3], x[4], x[2]));
        for t in c.shares() {
            let l = self.local(w, h, t)?;
            let g = Geom::build(&l.spec).map_err(TransformError::TooLarge)?;
            let k = geoms.len();
            geoms.push(g);
            for &(at, flip) in &tiles {
                copies.push(AreaCopy {
                    geom: k,
                    at,
                    flip,
                    centre: l.centre,
                    corner: l.corner,
                    radii: (w / 2.0 * sx, h / 2.0 * sy, rot),
                });
            }
        }
        let scene = Scene::with_areas(geoms, copies);
        Ok((scene, self.style(f, b)))
    }

    fn built(&self, f: &Frame) -> Option<&(Scene, Style)> {
        self.built.get_or_init(|| self.build(f)).as_ref().ok()
    }

    /// How the scene is drawn.
    fn style(&self, f: &Frame, b: [f64; 4]) -> Style {
        let c = &self.cfg;
        let mut st = Style {
            filled: c.filled.is_some(),
            subs: c.subs.clone(),
            ..Style::default()
        };
        if let Some((pat, s)) = c.pattern {
            let r0 = i64::from(f.lo);
            let stripes = |k: f64| {
                (
                    XPat {
                        alpha: 1.0,
                        beta: -b[0],
                        on: 1.0,
                        period: s as f64,
                    },
                    r0,
                    k,
                )
            };
            match pat {
                0 => st.filled = true,
                1 => st.row_stripes.push((r0, s)),
                2 => st.x_stripes.push(stripes(0.0)),
                3 => st.x_stripes.push(stripes(1.0)),
                _ => {
                    st.x_stripes.push(stripes(-1.0));
                    st.x_stripes.push(stripes(1.0));
                }
            }
        }
        if let Some(e) = &c.every {
            st.edge_dash = Some((1.0, e.clone(), 0.0));
        }
        let ends = c.edge_ends();
        if let Some(v) = &c.vshare {
            st.side_density = Some(
                ends.iter()
                    .map(|&(s, e)| (v[s % v.len()], v[e % v.len()]))
                    .collect(),
            );
        }
        if let Some(v) = &c.vvel {
            st.side_grads = Some(
                ends.iter()
                    .map(|&(s, e)| Grad {
                        vel: Some((v[s % v.len()], v[e % v.len()])),
                        ..Grad::default()
                    })
                    .collect(),
            );
        }
        if let Some((cv, by)) = &c.gate {
            st.shaped.push((
                Grad {
                    gate: Some((0.0, 1.0)),
                    ..Grad::default()
                },
                *by,
                cv.clone(),
            ));
        }
        st
    }
}

impl Shapes for Custom {
    fn draw(&self, f: &Frame, pen: &mut dyn Draw) -> bool {
        match self.built(f) {
            Some((scene, style)) => scene.draw(style, f.brush, f.step as f64, f.band(), pen),
            None => true,
        }
    }

    fn bound(&self, f: &Frame) -> u64 {
        let Some((scene, style)) = self.built(f) else {
            return 0;
        };
        let b = scene.bound(style, f.band());
        if b.is_finite() && b < u64::MAX as f64 {
            b.ceil() as u64
        } else {
            u64::MAX
        }
    }

    fn floor(&self, f: &Frame) -> u64 {
        let Some((scene, style)) = self.built(f) else {
            return 0;
        };
        let v = scene.floor(style, f.band());
        if v.is_finite() && v < u64::MAX as f64 {
            v as u64
        } else {
            0
        }
    }
}

fn custom_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let custom = Custom::new(Cfg::read(p)?);
    let built = custom.build(&f)?;
    let _ = custom.built.set(Ok(built));
    super::kernel(p, b, custom)
}

#[cfg(test)]
mod tests {
    use super::super::super::param::{ParamSet, ParamValue};
    use super::super::super::rng::Rng;
    use super::super::super::{Ctx, SourceStats};
    use super::super::pen::{Counter, Pen};
    use super::*;

    #[test]
    fn the_default_library_parses_and_names_every_shape() {
        let lib = parse_library(LIBRARY).unwrap();
        let names: Vec<&str> = lib.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Heart", "Star", "Cross", "Ring", "Arrow"]);
        assert_eq!(lib[3].1.len(), 2);
        assert!(parse_library("Bad 1,2 3,4 5,6").is_err());
        assert!(parse_library("A: 1,2 3,4").is_err());
        assert!(parse_library("A: 1,2 x 5,6").is_err());
    }

    /// The lower bound never passes the count and the cheap bound never
    /// falls below it, over seeded random settings (functions on with
    /// probability ⅓) in small boxes; refused settings are refused before
    /// anything is drawn.
    #[test]
    fn the_lower_bound_never_passes_the_count_and_the_bound_never_falls_below_it() {
        let spec = custom_spec();
        let mut rng = Rng::new(0x5345_3537);
        for draw in 0..200 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 200)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            for (k, lo, hi) in [
                ("c09_stroke_thickness.size", 1, 6),
                ("c10_configurable_fill_strategy.spacing", 2, 6),
                ("c18_radial_symmetry.count", 2, 8),
                ("c24_shape_a_b_morph.steps", 1, 4),
                ("c03_open_path.after", 1, 10),
            ] {
                p.set(k, ParamValue::Int(rng.range_i64(lo, hi)));
            }
            for (k, lo, hi) in [
                ("c16_full_transform.angle", -180.0, 180.0),
                ("c16_full_transform.shear", -60.0, 60.0),
                ("c17_mirror_symmetry.offset", -30.0, 30.0),
                ("c22_repeat_tile.w", 3.0, 20.0),
                ("c22_repeat_tile.h", 3.0, 20.0),
                ("c22_repeat_tile.gap", 0.0, 3.0),
                ("c24_shape_a_b_morph.amount", 0.0, 100.0),
            ] {
                p.set(k, ParamValue::Float(rng.range_f64(lo, hi)));
            }
            for (k, n) in [
                ("c07_outline_generation.placement", 3),
                ("c10_configurable_fill_strategy.pattern", 5),
                ("c10_configurable_fill_strategy.rule", 2),
                ("c04_closed_path.joins", 3),
                ("c03_open_path.caps", 3),
                ("c15_gate_profile.by", 5),
                ("c17_mirror_symmetry.axis", 3),
                ("c22_repeat_tile.layout", 3),
                ("c24_shape_a_b_morph.target", 3),
                ("c19_shape_union.kind", 3),
                ("c20_shape_subtract.kind", 3),
                ("c21_shape_intersection.kind", 3),
            ] {
                p.set(k, ParamValue::Choice(rng.below(n) as u32));
            }
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let g = Custom::new(Cfg::read(&params).unwrap());
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
