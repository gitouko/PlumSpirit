//! Polygons (§156, `gen.polygons`; `docs/GENERATORS.md` "Polygons"): the
//! regular hexagon inscribed in the generation box's ellipse (a vertex at
//! the top), its outline one cell wide inside it, and the contract's 25
//! functions (catalog ids `c01_…` … `c25_…`, switch = id, controls
//! `<id>.<name>`), drawn by the shared region rasterizer
//! (`generator/region.rs`).
//!
//! Shapes live in grid space (u = ticks / step, v = rows): a polygon is
//! built about its centre ("local" coordinates), then mapped and placed.
//! The pipeline, in this order:
//!
//! 1. **Vertices** — c01 Side Count, c02 Regular Polygon (equal sides on
//!    the grid) or c04 Radius, c03 Irregular Polygon (vertices in ‰ of
//!    the box); c15 Per-Vertex Angle, c14 Per-Vertex Radius, c06 Star /
//!    Inner-Radius Mode (a vertex between every two at an inner radius),
//!    c25 seeded vertex moves, c05 Rotation.
//! 2. **Corners and morph** — c16 Chamfer Corners, c17 Rounded Corners (a
//!    star's reflex corners stay sharp), c25's morph towards the polygon's
//!    circle (or ellipse).
//! 3. **Maps** — c23 Perspective (a keystone over the shape), c22 Skew and
//!    Twist (each point turned by an angle growing with its radius).
//! 4. **Outline and rings** — c07 places the outline (outside by default
//!    with the function, centred, or inside as without it), c09 thicker,
//!    c13 Concentric Polygons inside; a convex shape is offset exactly, any
//!    other along the rays from its centre (star-shaped rings, scanned
//!    even–odd).
//! 5. **Interior** — nothing, or (c08) filled; c10 Radial Spokes, c11
//!    Triangulated Fill (a fan of triangles, subdivided), c12 Scanline Fill
//!    drawn where they are (filled: where they are not).
//! 6. **Brushes** — c18 a subchannel per edge (a list, cycled), c20 every
//!    Nth cell per edge (a list, cycled), c21 velocity / gate gradients
//!    along each edge, c19 accented cells round the vertices.
//! 7. **Copies** — c24 tiles the box: the polygon's own tiling (triangles,
//!    squares, hexagons edge to edge), hexagonal packing or a square grid.
//!
//! Only c25 draws random values (`gen.seed`; each tile its own).

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::Rng;
use super::super::{Curve, Kernel, TransformError};
use super::draw::Grad;
use super::pen::Draw;
use super::region::{CellAccent, CopySpec, GDir, P2, Poly, RingSpec, Role, Scene, Style};
use super::{Frame, Shapes};
use crate::midi::group_digits;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::sync::OnceLock;

pub const POLYGONS: ToolDef = ToolDef {
    id: "gen.polygons",
    spec: polygons_spec,
    build: polygons_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_side_count", "Side Count", 10),
    ("c02_regular_polygon", "Regular Polygon", 20),
    ("c03_irregular_polygon", "Irregular Polygon", 30),
    ("c04_radius", "Radius", 40),
    ("c05_rotation", "Rotation", 50),
    ("c06_star_inner_radius_mode", "Star / Inner-Radius Mode", 60),
    ("c07_outline", "Outline", 70),
    ("c08_filled_polygon", "Filled Polygon", 80),
    ("c09_border_thickness", "Border Thickness", 90),
    ("c10_radial_spokes", "Radial Spokes", 100),
    ("c11_triangulated_fill", "Triangulated Fill", 110),
    ("c12_scanline_fill", "Scanline Fill", 120),
    ("c13_concentric_polygons", "Concentric Polygons", 130),
    ("c14_per_vertex_radius", "Per-Vertex Radius", 140),
    ("c15_per_vertex_angle", "Per-Vertex Angle", 150),
    ("c16_chamfer_corners", "Chamfer Corners", 160),
    ("c17_rounded_corners", "Rounded Corners", 170),
    ("c18_per_edge_subchannels", "Per-Edge Subchannels", 180),
    ("c19_vertex_accents", "Vertex Accents", 190),
    ("c20_per_edge_density", "Per-Edge Density", 200),
    (
        "c21_velocity_gate_edge_gradients",
        "Velocity/Gate Edge Gradients",
        210,
    ),
    ("c22_twist_skew", "Twist / Skew", 220),
    ("c23_perspective", "Perspective", 230),
    ("c24_tessellation_tiling", "Tessellation / Tiling", 240),
    (
        "c25_seeded_vertex_perturbation_shape_morph",
        "Seeded Vertex Perturbation / Shape Morph",
        250,
    ),
];

const FIT: &[&str] = &[
    "Vertices on the box's inscribed circle",
    "As large as fits the box",
];
const PLACEMENT: &[&str] = &[
    "Outside the shape",
    "Centered on its edge",
    "Inside the shape",
];
const FAN: &[&str] = &["The first vertex", "The center"];
const TILES: &[&str] = &[
    "The polygon's own tiling",
    "Hexagonal packing",
    "Square grid",
];

/// The base's side count.
pub const BASE_SIDES: u32 = 6;
/// Most vertices of an irregular polygon.
pub const MAX_VERTICES: usize = 64;
/// Most tiles (c24).
pub const TILE_CAP: u64 = 2048;
/// Most rings a scene may have.
pub const RING_CAP: u64 = 8192;
/// Most line segments (spokes, triangulation) over all tiles.
pub const LINE_CAP: u64 = 65_536;
/// Most outline points over all rings.
pub const POINT_CAP: u64 = 1 << 21;
/// Most pieces an edge is cut into for a morph or a twist.
const EDGE_SEGS: usize = 64;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const POLYGONS_HELP: &[(&str, &str)] = &[
    (
        "c01_side_count",
        "Sets how many sides the polygon has instead of six.",
    ),
    (
        "c01_side_count.sides",
        "How many sides (and corners) the polygon has.",
    ),
    (
        "c02_regular_polygon",
        "Makes every side and angle equal on the grid instead of stretching the polygon to the box.",
    ),
    (
        "c03_irregular_polygon",
        "Draws a polygon through corners you place instead of a regular one.",
    ),
    (
        "c04_radius",
        "Sets the polygon's size by its radius instead of fitting it to the box.",
    ),
    ("c05_rotation", "Turns the polygon about its center."),
    (
        "c06_star_inner_radius_mode",
        "Turns the polygon into a star by adding an inner corner between every two corners.",
    ),
    (
        "c07_outline",
        "Chooses where the outline's cells lie against the shape's edge.",
    ),
    (
        "c08_filled_polygon",
        "Fills the whole inside of the polygon with notes instead of drawing only its outline.",
    ),
    ("c09_border_thickness", "Makes the outline thicker."),
    (
        "c09_border_thickness.size",
        "How thick the outline is, in grid units.",
    ),
    (
        "c10_radial_spokes",
        "Draws lines from the center to the corners.",
    ),
    (
        "c11_triangulated_fill",
        "Divides the inside into triangles and draws their edges.",
    ),
    (
        "c11_triangulated_fill.from",
        "Where the triangles fan out from: the first corner or the center.",
    ),
    (
        "c12_scanline_fill",
        "Fills the inside with whole rows of notes at a spacing (or, with Filled Polygon, cuts them out).",
    ),
    (
        "c12_scanline_fill.spacing",
        "How many rows apart the filled rows are.",
    ),
    (
        "c13_concentric_polygons",
        "Draws outlines inside the polygon, one inside the other.",
    ),
    (
        "c13_concentric_polygons.count",
        "How many outlines are drawn inside the polygon.",
    ),
    (
        "c14_per_vertex_radius",
        "Gives each corner its own distance from the center, set by a curve.",
    ),
    (
        "c15_per_vertex_angle",
        "Turns each corner a little about the center, set by a curve, so the corners are spaced unevenly.",
    ),
    (
        "c15_per_vertex_angle.most",
        "The farthest a corner may turn, as a share of the angle between corners.",
    ),
    ("c16_chamfer_corners", "Cuts the corners off at an angle."),
    (
        "c16_chamfer_corners.size",
        "How much of each corner is cut off, in grid units.",
    ),
    ("c17_rounded_corners", "Rounds off the corners."),
    (
        "c18_per_edge_subchannels",
        "Gives each side of the outline a subchannel (color) from a list.",
    ),
    (
        "c19_vertex_accents",
        "Accents the notes around each corner with a louder velocity.",
    ),
    (
        "c19_vertex_accents.velocity",
        "The velocity of the accented notes at the corners.",
    ),
    (
        "c20_per_edge_density",
        "Draws only every Nth cell along each side, set per side from a list.",
    ),
    (
        "c21_velocity_gate_edge_gradients",
        "Changes velocity and gate along every side, from its start corner to its end corner.",
    ),
    (
        "c21_velocity_gate_edge_gradients.vel_from",
        "The velocity where each side starts.",
    ),
    (
        "c21_velocity_gate_edge_gradients.vel_to",
        "The velocity where each side ends.",
    ),
    (
        "c21_velocity_gate_edge_gradients.gate_from",
        "The gate (share of the cell) where each side starts.",
    ),
    (
        "c22_twist_skew",
        "Slants the polygon and twists it, bending straight sides into spirals.",
    ),
    (
        "c23_perspective",
        "Narrows (or widens) the top of the polygon as if it is seen in perspective.",
    ),
    (
        "c24_tessellation_tiling",
        "Fills the box with copies of the polygon, like tiles.",
    ),
    (
        "c24_tessellation_tiling.radius",
        "The size of each tile, from its center to its farthest corner, in grid units.",
    ),
    (
        "c24_tessellation_tiling.gap",
        "The space left between neighboring tiles, in grid units.",
    ),
    (
        "c25_seeded_vertex_perturbation_shape_morph",
        "Moves the corners at random (seeded) and can round the polygon toward a circle.",
    ),
];

fn polygons_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        // c01
        flag("c01_side_count", "Side Count", false),
        int("c01_side_count.sides", "Sides", 3, 64, 5),
        // c02
        flag("c02_regular_polygon", "Regular Polygon", false),
        choice("c02_regular_polygon.fit", "Size", FIT, 0).help(
            "Equal sides and angles on the grid (a resolution step across is a row up) instead of the box's stretched polygon: its vertices on the circle of the box's smaller side, or as large as fits the box.",
        ),
        // c03
        flag("c03_irregular_polygon", "Irregular Polygon", false),
        list(
            "c03_irregular_polygon.points",
            "Vertices (x, y pairs, ‰ of the box)",
            0,
            1000,
            2 * MAX_VERTICES,
            &[500, 1000, 1000, 620, 780, 0, 180, 80, 0, 700],
        )
        .help("Each pair is a vertex: x from the box's start, y from its bottom, in per mille. Three or more vertices replace the regular polygon (fewer draw it); Side Count is then theirs."),
        // c04
        flag("c04_radius", "Radius", false),
        float("c04_radius.radius", "Radius (grid units)", 1.0, 4096.0, 8.0, "")
            .help("From the center to the farthest vertex, on the grid: a regular polygon of that size (an irregular one scaled to it)."),
        // c05
        flag("c05_rotation", "Rotation", false),
        float("c05_rotation.angle", "Angle", -180.0, 180.0, 15.0, "°")
            .help("About the center, counter-clockwise, on the grid."),
        // c06
        flag("c06_star_inner_radius_mode", "Star / Inner-Radius Mode", false),
        float(
            "c06_star_inner_radius_mode.inner",
            "Inner radius (% of the outer)",
            5.0,
            95.0,
            50.0,
            "%",
        )
        .help("A vertex between every two, at their middle angle and this share of their radius: a star with as many points as sides."),
        // c07
        flag("c07_outline", "Outline", false),
        choice("c07_outline.placement", "Outline", PLACEMENT, 0).help(
            "Where the outline's cells lie: around the shape, half in and half out, or inside it (as without this function).",
        ),
        // c08
        flag("c08_filled_polygon", "Filled Polygon", false),
        // c09
        flag("c09_border_thickness", "Border Thickness", false),
        int("c09_border_thickness.size", "Thickness (grid units)", 1, 64, 2),
        // c10
        flag("c10_radial_spokes", "Radial Spokes", false),
        int("c10_radial_spokes.every", "A spoke to every (vertices)", 1, 64, 1)
            .help("Lines from the center to the vertices (a star's inner vertices count), drawn inside the outline."),
        // c11
        flag("c11_triangulated_fill", "Triangulated Fill", false),
        choice("c11_triangulated_fill.from", "Fan from", FAN, 0),
        int("c11_triangulated_fill.levels", "Subdivisions", 0, 4, 1).help(
            "The polygon cut into triangles fanning out from its first vertex (or its center); each subdivision cuts every triangle into four. Their edges are drawn inside the outline.",
        ),
        // c12
        flag("c12_scanline_fill", "Scanline Fill", false),
        int("c12_scanline_fill.spacing", "A line every (rows)", 2, 127, 2).keyed(),
        int("c12_scanline_fill.phase", "Phase (rows)", 0, 126, 0)
            .keyed()
            .help("Whole rows of the interior, counted from the shape's lowest row plus the phase."),
        // c13
        flag("c13_concentric_polygons", "Concentric Polygons", false),
        int("c13_concentric_polygons.count", "Polygons inside", 1, 64, 3),
        int("c13_concentric_polygons.spacing", "Spacing (grid units; 0 = even)", 0, 256, 0)
            .help("Outlines inside the shape, each this far inside the last (0: evenly spaced towards the center)."),
        // c14
        flag("c14_per_vertex_radius", "Per-Vertex Radius", false),
        curve(
            "c14_per_vertex_radius.curve",
            "Radius by vertex",
            Curve::new(vec![(0.0, 1.0), (0.5, 0.45), (1.0, 1.0)]),
        )
        .help("x: the vertex, from the first (0) round to the last; y: its radius, as a share of the full radius (at least 5 %)."),
        // c15
        flag("c15_per_vertex_angle", "Per-Vertex Angle", false),
        curve(
            "c15_per_vertex_angle.curve",
            "Shift by vertex",
            Curve::new(vec![(0.0, 0.5), (0.25, 1.0), (0.75, 0.0), (1.0, 0.5)]),
        )
        .help("x: the vertex; y: its turn about the center — 0.5 none, 1 the most counter-clockwise, 0 the most clockwise."),
        float("c15_per_vertex_angle.most", "Most shift (% of the vertex spacing)", 0.0, 45.0, 30.0, "%"),
        // c16
        flag("c16_chamfer_corners", "Chamfer Corners", false),
        float("c16_chamfer_corners.size", "Cut (grid units)", 0.5, 256.0, 2.0, ""),
        // c17
        flag("c17_rounded_corners", "Rounded Corners", false),
        float("c17_rounded_corners.radius", "Radius (grid units)", 0.5, 256.0, 3.0, "")
            .help("Each outward corner rounded by a circular arc tangent to its edges (a star's inward corners stay sharp)."),
        // c18
        flag("c18_per_edge_subchannels", "Per-Edge Subchannels", false),
        list(
            "c18_per_edge_subchannels.subs",
            "Subchannel positions by edge (cycled)",
            0,
            4095,
            128,
            &[0, 1, 2, 3],
        )
        .help("Edge k (counter-clockwise from the first vertex) takes entry k of the list, cycling; outline cells take their nearest edge's."),
        // c19
        flag("c19_vertex_accents", "Vertex Accents", false),
        int("c19_vertex_accents.velocity", "Accent velocity", 1, 127, 127),
        float("c19_vertex_accents.radius", "Radius (grid units)", 0.5, 64.0, 1.0, "")
            .help("Cells whose center lies this close to a vertex (a step across, a row up) get the accent velocity."),
        // c20
        flag("c20_per_edge_density", "Per-Edge Density", false),
        list(
            "c20_per_edge_density.every",
            "A note every (cells) by edge (cycled)",
            1,
            64,
            128,
            &[1, 2],
        )
        .help("Along each edge of the outline, a cell every so many; edge k takes entry k of the list, cycling."),
        // c21
        flag(
            "c21_velocity_gate_edge_gradients",
            "Velocity/Gate Edge Gradients",
            false,
        ),
        int("c21_velocity_gate_edge_gradients.vel_from", "Velocity at an edge's start", 1, 127, 127),
        int("c21_velocity_gate_edge_gradients.vel_to", "Velocity at its end", 1, 127, 40),
        float("c21_velocity_gate_edge_gradients.gate_from", "Gate at an edge's start", 1.0, 100.0, 100.0, "%"),
        float("c21_velocity_gate_edge_gradients.gate_to", "Gate at its end", 1.0, 100.0, 50.0, "%")
            .help("Along every edge, counter-clockwise from its start vertex to its end; outline cells take their nearest edge's position."),
        // c22
        flag("c22_twist_skew", "Twist / Skew", false),
        float("c22_twist_skew.skew", "Skew", -75.0, 75.0, 0.0, "°")
            .help("Each row above the center moves later in time (earlier below it)."),
        float("c22_twist_skew.twist", "Twist (at the outermost point)", -720.0, 720.0, 90.0, "°")
            .help("Each point turned about the center by this angle times its share of the largest radius: straight edges bend into spirals."),
        // c23
        flag("c23_perspective", "Perspective", false),
        float("c23_perspective.top", "Top width (% of the bottom)", 1.0, 400.0, 50.0, "%")
            .help("The shape's top scaled about its center, its bottom kept, rows drawn closer towards the narrower end."),
        // c24
        flag("c24_tessellation_tiling", "Tessellation / Tiling", false),
        choice("c24_tessellation_tiling.layout", "Tiles", TILES, 0).help(
            "Copies of the polygon whose centers lie in the box. Triangles, squares and hexagons tile edge to edge (triangles alternately turned); other shapes take hexagonal packing.",
        ),
        float("c24_tessellation_tiling.radius", "Tile radius (grid units)", 1.0, 4096.0, 4.0, ""),
        float("c24_tessellation_tiling.gap", "Gap (grid units)", 0.0, 256.0, 0.0, ""),
        // c25
        flag(
            "c25_seeded_vertex_perturbation_shape_morph",
            "Seeded Vertex Perturbation / Shape Morph",
            false,
        ),
        float(
            "c25_seeded_vertex_perturbation_shape_morph.amount",
            "Perturbation (% of the radius)",
            0.0,
            50.0,
            15.0,
            "%",
        )
        .help("Each vertex moved by a seeded share of its radius in and out and of the vertex spacing round the center (each tile its own)."),
        float(
            "c25_seeded_vertex_perturbation_shape_morph.morph",
            "Morph towards a circle",
            0.0,
            100.0,
            0.0,
            "%",
        )
        .help("Every point of the outline moved this share of the way to the polygon's circle (the box's ellipse for the base): 100 % is round."),
    ];
    with_help(&mut own, POLYGONS_HELP);
    super::spec(
        "gen.polygons",
        "Polygons",
        "Generates polygons of notes: a regular hexagon inscribed in the generation box (where, length and pitch range above), its outline one cell wide on the resolution grid.",
        own,
        FUNCTIONS,
    )
}

/// The functions' settings (`None` / neutral = off).
#[derive(Clone, Debug, Default)]
struct Cfg {
    /// c01 (the base: 6).
    sides: u32,
    /// c02: the size choice.
    regular: Option<usize>,
    /// c03: vertices as box fractions (x, y).
    irregular: Option<Vec<P2>>,
    /// c04: grid units.
    radius: Option<f64>,
    /// c05: radians (0 = off).
    rotate: f64,
    /// c06: the inner radius' share.
    star: Option<f64>,
    /// c07: 0 outside, 1 centred, 2 inside (the base).
    placement: usize,
    /// c08.
    filled: bool,
    /// c09: grid units (1 = off).
    thick: f64,
    /// c10: a spoke to every Nth vertex.
    spokes: Option<usize>,
    /// c11: fan from the centre?, subdivisions.
    tri: Option<(bool, u32)>,
    /// c12: spacing, phase (rows).
    scan: Option<(i64, i64)>,
    /// c13: count, spacing (0 = even).
    nest: Option<(u32, f64)>,
    /// c14, c15 (with the most shift as a share of the spacing).
    vradius: Option<Curve>,
    vangle: Option<(Curve, f64)>,
    /// c16 cut, c17 radius.
    chamfer: Option<f64>,
    round: Option<f64>,
    /// c18, c20: per edge, cycled.
    subs: Option<Vec<u32>>,
    density: Option<Vec<f64>>,
    /// c19: velocity, radius.
    accent: Option<(u8, f64)>,
    /// c21: velocity from / to, gate from / to.
    grad: Option<(f64, f64, f64, f64)>,
    /// c22: skew, twist (radians).
    twist: Option<(f64, f64)>,
    /// c23: the top's scale.
    persp: Option<f64>,
    /// c24: layout, tile radius, gap.
    tile: Option<(usize, f64, f64)>,
    /// c25: perturbation share, morph share.
    perturb: Option<(f64, f64)>,
}

impl Cfg {
    fn read(p: &Params) -> Cfg {
        let on = |id: &str| p.flag(id);
        let mut c = Cfg {
            sides: BASE_SIDES,
            placement: 2,
            thick: 1.0,
            ..Cfg::default()
        };
        if on("c01_side_count") {
            c.sides = p.int("c01_side_count.sides").clamp(3, 64) as u32;
        }
        if on("c02_regular_polygon") {
            c.regular = Some(p.choice("c02_regular_polygon.fit").min(1));
        }
        if on("c03_irregular_polygon") {
            let v = p.list("c03_irregular_polygon.points");
            let pts: Vec<P2> = v
                .chunks_exact(2)
                .take(MAX_VERTICES)
                .map(|q| {
                    (
                        q[0].clamp(0, 1000) as f64 / 1000.0,
                        q[1].clamp(0, 1000) as f64 / 1000.0,
                    )
                })
                .collect();
            c.irregular = Some(pts);
        }
        if on("c04_radius") {
            c.radius = Some(p.float("c04_radius.radius").clamp(1.0, 4096.0));
        }
        if on("c05_rotation") {
            c.rotate = p
                .float("c05_rotation.angle")
                .clamp(-180.0, 180.0)
                .to_radians();
        }
        if on("c06_star_inner_radius_mode") {
            c.star = Some((p.float("c06_star_inner_radius_mode.inner") / 100.0).clamp(0.05, 0.95));
        }
        if on("c07_outline") {
            c.placement = p.choice("c07_outline.placement").min(2);
        }
        c.filled = on("c08_filled_polygon");
        if on("c09_border_thickness") {
            c.thick = p.int("c09_border_thickness.size").clamp(1, 64) as f64;
        }
        if on("c10_radial_spokes") {
            c.spokes = Some(p.int("c10_radial_spokes.every").clamp(1, 64) as usize);
        }
        if on("c11_triangulated_fill") {
            c.tri = Some((
                p.choice("c11_triangulated_fill.from") == 1,
                p.int("c11_triangulated_fill.levels").clamp(0, 4) as u32,
            ));
        }
        if on("c12_scanline_fill") {
            c.scan = Some((
                p.int("c12_scanline_fill.spacing").max(2),
                p.int("c12_scanline_fill.phase").max(0),
            ));
        }
        if on("c13_concentric_polygons") {
            c.nest = Some((
                p.int("c13_concentric_polygons.count").clamp(1, 64) as u32,
                p.int("c13_concentric_polygons.spacing").clamp(0, 256) as f64,
            ));
        }
        if on("c14_per_vertex_radius") {
            c.vradius = Some(p.curve("c14_per_vertex_radius.curve"));
        }
        if on("c15_per_vertex_angle") {
            c.vangle = Some((
                p.curve("c15_per_vertex_angle.curve"),
                (p.float("c15_per_vertex_angle.most") / 100.0).clamp(0.0, 0.45),
            ));
        }
        if on("c16_chamfer_corners") {
            c.chamfer = Some(p.float("c16_chamfer_corners.size").max(0.0));
        }
        if on("c17_rounded_corners") {
            c.round = Some(p.float("c17_rounded_corners.radius").max(0.0));
        }
        if on("c18_per_edge_subchannels") {
            let v = p.list("c18_per_edge_subchannels.subs");
            c.subs = Some(if v.is_empty() {
                vec![0]
            } else {
                v.iter().map(|&s| s.clamp(0, 4095) as u32).collect()
            });
        }
        if on("c19_vertex_accents") {
            c.accent = Some((
                p.int("c19_vertex_accents.velocity").clamp(1, 127) as u8,
                p.float("c19_vertex_accents.radius").clamp(0.5, 64.0),
            ));
        }
        if on("c20_per_edge_density") {
            let v = p.list("c20_per_edge_density.every");
            c.density = Some(if v.is_empty() {
                vec![1.0]
            } else {
                v.iter().map(|&k| k.clamp(1, 64) as f64).collect()
            });
        }
        if on("c21_velocity_gate_edge_gradients") {
            let k = "c21_velocity_gate_edge_gradients";
            c.grad = Some((
                p.int(&format!("{k}.vel_from")).clamp(1, 127) as f64,
                p.int(&format!("{k}.vel_to")).clamp(1, 127) as f64,
                p.float(&format!("{k}.gate_from")).clamp(1.0, 100.0) / 100.0,
                p.float(&format!("{k}.gate_to")).clamp(1.0, 100.0) / 100.0,
            ));
        }
        if on("c22_twist_skew") {
            c.twist = Some((
                p.float("c22_twist_skew.skew")
                    .clamp(-75.0, 75.0)
                    .to_radians(),
                p.float("c22_twist_skew.twist")
                    .clamp(-720.0, 720.0)
                    .to_radians(),
            ));
        }
        if on("c23_perspective") {
            c.persp = Some((p.float("c23_perspective.top") / 100.0).clamp(0.01, 4.0));
        }
        if on("c24_tessellation_tiling") {
            c.tile = Some((
                p.choice("c24_tessellation_tiling.layout").min(2),
                p.float("c24_tessellation_tiling.radius").clamp(1.0, 4096.0),
                p.float("c24_tessellation_tiling.gap").clamp(0.0, 256.0),
            ));
        }
        if on("c25_seeded_vertex_perturbation_shape_morph") {
            let k = "c25_seeded_vertex_perturbation_shape_morph";
            c.perturb = Some((
                (p.float(&format!("{k}.amount")) / 100.0).clamp(0.0, 0.5),
                (p.float(&format!("{k}.morph")) / 100.0).clamp(0.0, 1.0),
            ));
        }
        c
    }

    /// The irregular polygon's vertices when it has three or more.
    fn irregular(&self) -> Option<&[P2]> {
        self.irregular.as_deref().filter(|v| v.len() >= 3)
    }

    /// The vertices of the polygon before the star (c01 / c03).
    fn corners(&self) -> usize {
        self.irregular().map_or(self.sides as usize, <[P2]>::len)
    }

    /// The vertices of the shape (a star doubles them).
    fn vertices(&self) -> usize {
        self.corners() * if self.star.is_some() { 2 } else { 1 }
    }

    /// Whether the shape's points are cut finer (a morph or a twist).
    fn dense(&self) -> bool {
        self.perturb.is_some_and(|(_, m)| m > 0.0) || self.twist.is_some_and(|(_, t)| t != 0.0)
    }
}

/// A shape about its centre (local grid units): its vertices (counter-
/// clockwise, the first at the top for a regular polygon), its circle's
/// radii (for the morph), and the size it was built at.
#[derive(Clone, Debug)]
struct Local {
    verts: Vec<P2>,
    rx: f64,
    ry: f64,
}

/// One copy of the shape, placed: its outline (sides tagged by edge), its
/// vertices and edges after the maps, centre.
#[derive(Clone, Debug)]
struct Placed {
    shape: Poly,
    verts: Vec<P2>,
    centre: P2,
    lines: Vec<(P2, P2)>,
    radii: (f64, f64),
}

/// Twist / skew and perspective, in local coordinates.
#[derive(Clone, Copy, Debug)]
struct Maps2 {
    /// Top scale, bottom, height.
    persp: Option<(f64, f64, f64)>,
    skew: f64,
    /// Radians at the largest radius, the largest radius.
    twist: Option<(f64, f64)>,
}

impl Maps2 {
    fn ap(&self, p: P2) -> P2 {
        let mut q = p;
        if let Some((k, v0, h)) = self.persp {
            let y = (q.1 - v0) / h;
            let den = 1.0 + (1.0 / k - 1.0) * y;
            q = (q.0 / den, v0 + h * y / (k * den));
        }
        if self.skew != 0.0 {
            q = (q.0 + q.1 * self.skew.tan(), q.1);
        }
        if let Some((t, rmax)) = self.twist {
            let a = t * q.0.hypot(q.1) / rmax;
            let (s, c) = a.sin_cos();
            q = (q.0 * c - q.1 * s, q.0 * s + q.1 * c);
        }
        q
    }
}

/// The signed area of a closed point list.
fn area(pts: &[P2]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (p, q) = (pts[i], pts[(i + 1) % n]);
            p.0 * q.1 - q.0 * p.1
        })
        .sum::<f64>()
        / 2.0
}

/// Each edge of `poly` (sides kept) cut into pieces of at most one grid
/// unit, at most [`EDGE_SEGS`] per edge.
fn densify(poly: &Poly) -> Poly {
    let n = poly.pts.len();
    let mut pts = Vec::with_capacity(n * 4);
    let mut sides = Vec::with_capacity(n * 4);
    for i in 0..n {
        let (a, b) = poly.edge(i);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        let k = if len.is_finite() {
            (len.ceil() as usize).clamp(1, EDGE_SEGS)
        } else {
            1
        };
        for j in 0..k {
            let t = j as f64 / k as f64;
            pts.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            sides.push(poly.sides[i]);
        }
    }
    Poly::from_points(pts, sides)
}

/// The points per ring of a shape with `cfg`'s corners and maps (a bound).
fn ring_points(cfg: &Cfg) -> u64 {
    let v = cfg.vertices() as u64;
    let mut per = v * if cfg.chamfer.is_some() { 2 } else { 1 };
    if cfg.round.is_some() {
        per = v * (super::region::ARC_CAP as u64 + 1);
    }
    if cfg.dense() {
        per *= EDGE_SEGS as u64;
    }
    per
}

/// The Polygons generator.
struct Polygons {
    cfg: Cfg,
    built: OnceLock<(Scene, Style)>,
}

impl Polygons {
    fn new(cfg: Cfg) -> Polygons {
        Polygons {
            cfg,
            built: OnceLock::new(),
        }
    }

    fn built(&self, f: &Frame) -> &(Scene, Style) {
        self.built.get_or_init(|| match self.copies(f) {
            Ok(cs) => (self.scene(&cs), self.style(f, &cs)),
            Err(_) => (Scene::default(), Style::default()),
        })
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

    /// The shape about its centre for a box of `w` × `h` grid units (or a
    /// tile of radius `tile`), with `rng` for the perturbation; and the
    /// centre's offset from the box's centre (c02's fit).
    fn local(&self, w: f64, h: f64, tile: Option<f64>, rng: &mut Rng) -> (Local, P2) {
        let c = &self.cfg;
        // The radii the regular vertices lie on.
        let fit_box = tile.is_none() && c.radius.is_none() && c.regular == Some(1);
        let (rx, ry) = match (tile, c.radius, c.regular) {
            (Some(r), _, _) => (r, r),
            (None, Some(r), _) => (r, r),
            (None, None, Some(0)) => (w.min(h) / 2.0, w.min(h) / 2.0),
            (None, None, Some(_)) => (1.0, 1.0),
            (None, None, None) => (w / 2.0, h / 2.0),
        };
        // The stretch of the box's ellipse (1, 1 for a regular polygon):
        // angles round the centre (c15, c06, c25) are taken on the circle
        // it maps to, so a vertex moves along the ellipse.
        let mut stretch = (rx.max(1e-12), ry.max(1e-12));
        let mut verts: Vec<P2> = match c.irregular() {
            Some(v) => {
                // Box fractions about the centre; with a size (a tile, c04,
                // c02): in a square, its farthest vertex at that radius.
                let target = tile
                    .or(c.radius)
                    .or((c.regular == Some(0)).then_some(w.min(h) / 2.0));
                let (sw, sh) = if c.regular.is_some() || target.is_some() {
                    (w.min(h), w.min(h))
                } else {
                    (w, h)
                };
                stretch = ((sw / 2.0).max(1e-12), (sh / 2.0).max(1e-12));
                let mut pts: Vec<P2> = v
                    .iter()
                    .map(|&(x, y)| ((x - 0.5) * sw, (y - 0.5) * sh))
                    .collect();
                if let Some(r) = target {
                    let far = pts.iter().fold(0.0f64, |a, p| a.max(p.0.hypot(p.1)));
                    if far > 0.0 {
                        pts.iter_mut()
                            .for_each(|p| *p = (p.0 * r / far, p.1 * r / far));
                    }
                }
                pts
            }
            None => {
                let n = c.sides.max(3) as usize;
                (0..n)
                    .map(|i| {
                        let a = FRAC_PI_2 + TAU * i as f64 / n as f64;
                        (rx * a.cos(), ry * a.sin())
                    })
                    .collect()
            }
        };
        if area(&verts) < 0.0 {
            verts.reverse();
        }
        let n = verts.len();
        let spacing = TAU / n as f64;
        let (sx, sy) = stretch;
        // Turned about the centre by `a` on the ellipse's circle.
        let turn = |p: P2, a: f64| {
            let (s, co) = a.sin_cos();
            let (x, y) = (p.0 / sx, p.1 / sy);
            ((x * co - y * s) * sx, (x * s + y * co) * sy)
        };
        // c15: turned about the centre; c14: scaled along the ray.
        if let Some((cv, most)) = &c.vangle {
            for (i, p) in verts.iter_mut().enumerate() {
                let t = i as f64 / n as f64;
                let a = (cv.eval(t).clamp(0.0, 1.0) - 0.5) * 2.0 * most * spacing;
                *p = turn(*p, a);
            }
        }
        if let Some(cv) = &c.vradius {
            for (i, p) in verts.iter_mut().enumerate() {
                let k = cv.eval(i as f64 / n as f64).clamp(0.05, 1.0);
                *p = (p.0 * k, p.1 * k);
            }
        }
        // c06: a vertex between every two (on the ellipse's circle).
        if let Some(k) = c.star {
            let mut out = Vec::with_capacity(2 * n);
            for i in 0..n {
                let (p0, q0) = (verts[i], verts[(i + 1) % n]);
                out.push(p0);
                let (p, q) = ((p0.0 / sx, p0.1 / sy), (q0.0 / sx, q0.1 / sy));
                let (rp, rq) = (p.0.hypot(p.1), q.0.hypot(q.1));
                let (up, uq) = if rp > 0.0 && rq > 0.0 {
                    ((p.0 / rp, p.1 / rp), (q.0 / rq, q.1 / rq))
                } else {
                    ((0.0, 0.0), (0.0, 0.0))
                };
                let m = (up.0 + uq.0, up.1 + uq.1);
                let ml = m.0.hypot(m.1);
                let r = k * (rp + rq) / 2.0;
                let inner = if ml > 1e-12 {
                    (m.0 / ml * r, m.1 / ml * r)
                } else {
                    // Opposite vertices: the edge's middle, pulled in.
                    ((p.0 + q.0) / 2.0 * k, (p.1 + q.1) / 2.0 * k)
                };
                out.push((inner.0 * sx, inner.1 * sy));
            }
            verts = out;
        }
        // c25: seeded moves in and out and round the centre.
        if let Some((amt, _)) = c.perturb
            && amt > 0.0
        {
            let m = verts.len() as f64;
            for p in verts.iter_mut() {
                let k = 1.0 + amt * rng.range_f64(-1.0, 1.0);
                let a = amt * rng.range_f64(-1.0, 1.0) * 0.9 * PI / m;
                let q = turn(*p, a);
                *p = (q.0 * k, q.1 * k);
            }
        }
        // c05: the rotation.
        if c.rotate != 0.0 {
            let (s, co) = c.rotate.sin_cos();
            for p in verts.iter_mut() {
                *p = (p.0 * co - p.1 * s, p.0 * s + p.1 * co);
            }
        }
        let mut shift = (0.0, 0.0);
        let (mut rx, mut ry) = (rx, ry);
        if fit_box {
            // As large as fits the box, centred in it.
            let b = verts.iter().fold(
                [
                    f64::INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::NEG_INFINITY,
                ],
                |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)],
            );
            let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
            let s = (w / bw.max(1e-12)).min(h / bh.max(1e-12));
            if s.is_finite() && s > 0.0 {
                verts.iter_mut().for_each(|p| *p = (p.0 * s, p.1 * s));
                (rx, ry) = (rx * s, ry * s);
                shift = (-(b[0] + b[2]) / 2.0 * s, -(b[1] + b[3]) / 2.0 * s);
            }
        }
        if c.irregular().is_some() {
            // The morph's circle: the vertices' mean radius.
            let m = verts.iter().map(|p| p.0.hypot(p.1)).sum::<f64>() / verts.len() as f64;
            (rx, ry) = (m, m);
        }
        (Local { verts, rx, ry }, shift)
    }

    /// The maps for shape `l` (c22, c23).
    fn maps(&self, l: &Local) -> Maps2 {
        let c = &self.cfg;
        let b = l
            .verts
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |b, p| {
                (b.0.min(p.1), b.1.max(p.1))
            });
        let rmax = l
            .verts
            .iter()
            .fold(0.0f64, |a, p| a.max(p.0.hypot(p.1)))
            .max(l.rx.max(l.ry))
            .max(1e-9);
        Maps2 {
            persp: c.persp.filter(|_| b.1 > b.0).map(|k| (k, b.0, b.1 - b.0)),
            skew: c.twist.map_or(0.0, |t| t.0),
            twist: c.twist.filter(|t| t.1 != 0.0).map(|t| (t.1, rmax)),
        }
    }

    /// Shape `l` placed at `centre` (grid space): corners, morph, maps,
    /// line features.
    fn place(&self, l: &Local, centre: P2) -> Placed {
        let c = &self.cfg;
        let n = l.verts.len();
        let sides: Vec<u8> = (0..n).map(|i| i as u8).collect();
        let mut p = Poly::from_points(l.verts.clone(), sides);
        if let Some(cut) = c.chamfer {
            p = p.chamfer(cut);
        }
        if let Some(r) = c.round {
            p = p.round(r);
        }
        if c.dense() {
            p = densify(&p);
        }
        if let Some((_, m)) = c.perturb
            && m > 0.0
        {
            let (rx, ry) = (l.rx.max(1e-9), l.ry.max(1e-9));
            p = p.map(|q| {
                let rho = q.0.hypot(q.1);
                if rho <= 0.0 {
                    return q;
                }
                let (s, co) = (q.1 / rho, q.0 / rho);
                let target = rx * ry / (ry * co).hypot(rx * s);
                let r = rho + m * (target - rho);
                (co * r, s * r)
            });
        }
        let maps = self.maps(l);
        let at = |q: P2| {
            let m = maps.ap(q);
            (m.0 + centre.0, m.1 + centre.1)
        };
        let shape = p.map(at);
        let verts: Vec<P2> = l.verts.iter().map(|&q| at(q)).collect();
        // Line features in local coordinates, their ends mapped.
        let mut lines = Vec::new();
        if let Some(every) = c.spokes {
            for q in l.verts.iter().step_by(every.max(1)) {
                lines.push((at((0.0, 0.0)), at(*q)));
            }
        }
        if let Some((centred, levels)) = c.tri {
            let fan: Vec<[P2; 3]> = if centred {
                (0..n)
                    .map(|i| [(0.0, 0.0), l.verts[i], l.verts[(i + 1) % n]])
                    .collect()
            } else {
                (1..n.saturating_sub(1))
                    .map(|i| [l.verts[0], l.verts[i], l.verts[i + 1]])
                    .collect()
            };
            let m = 1u32 << levels;
            let lerp = |a: P2, b: P2, t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            for [a, b, cc] in fan {
                // The triangle's own edges (the outline's are covered by it).
                lines.push((at(a), at(b)));
                lines.push((at(b), at(cc)));
                lines.push((at(cc), at(a)));
                // Its subdivision: lines parallel to each side.
                for k in 1..m {
                    let t = f64::from(k) / f64::from(m);
                    lines.push((at(lerp(a, b, t)), at(lerp(a, cc, t))));
                    lines.push((at(lerp(b, cc, t)), at(lerp(b, a, t))));
                    lines.push((at(lerp(cc, a, t)), at(lerp(cc, b, t))));
                }
            }
        }
        Placed {
            shape,
            verts,
            centre: at((0.0, 0.0)),
            lines,
            radii: (l.rx, l.ry),
        }
    }

    /// The line segments one copy draws.
    fn lines_per_copy(&self) -> u64 {
        let c = &self.cfg;
        let v = self.cfg.vertices() as u64;
        let spokes = c.spokes.map_or(0, |k| v.div_ceil(k.max(1) as u64));
        let tri = c.tri.map_or(0, |(centred, levels)| {
            let t = if centred { v } else { v.saturating_sub(2) };
            t * (3 + 3 * ((1u64 << levels) - 1))
        });
        spokes + tri
    }

    /// The tiles' centres (and whether each is turned half round) for box
    /// `b`, or the cap's sentence.
    fn tiles(&self, b: [f64; 4]) -> Result<Vec<(P2, bool)>, TransformError> {
        let c = &self.cfg;
        let Some((layout, r, gap)) = c.tile else {
            return Ok(vec![(((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0), false)]);
        };
        let n = c.sides as usize;
        let own =
            layout == 0 && c.irregular().is_none() && c.star.is_none() && matches!(n, 3 | 4 | 6);
        let too_many = |k: u64| {
            TransformError::TooLarge(format!(
                "Tessellation / Tiling would place {} tiles; at most {} are allowed — use a larger tile radius or gap, or a smaller box.",
                group_digits(k),
                group_digits(TILE_CAP)
            ))
        };
        // The lattice: two vectors and an extra turned copy (triangles).
        let (t1, t2, turned) = if own {
            // The tiles' outward normals and apothem, the gap shared.
            let a = r * (PI / n as f64).cos() + gap / 2.0;
            let normal = |k: usize| {
                let ang = FRAC_PI_2 + TAU * (k as f64 + 0.5) / n as f64 + c.rotate;
                (ang.cos(), ang.sin())
            };
            let (n0, n1) = (normal(0), normal(1));
            match n {
                3 => {
                    // Up triangles on the lattice of their sides; a turned
                    // one across edge 0.
                    let s = 2.0 * a * 3f64.sqrt();
                    let e = |k: usize| {
                        let ang = c.rotate + TAU * k as f64 / 3.0;
                        (s * ang.cos(), s * ang.sin())
                    };
                    (e(0), e(1), Some((2.0 * a * n0.0, 2.0 * a * n0.1)))
                }
                _ => (
                    (2.0 * a * n0.0, 2.0 * a * n0.1),
                    (2.0 * a * n1.0, 2.0 * a * n1.1),
                    None,
                ),
            }
        } else {
            let rr = r + gap / 2.0;
            if layout == 2 {
                ((2.0 * rr, 0.0), (0.0, 2.0 * rr), None)
            } else {
                ((2.0 * rr, 0.0), (rr, 3f64.sqrt() * rr), None)
            }
        };
        // The row vector: the most horizontal of t1, t2, t1 − t2.
        let cands = [t1, t2, (t1.0 - t2.0, t1.1 - t2.1)];
        let slope = |t: P2| t.1.abs() / t.0.hypot(t.1).max(1e-300);
        let i_row = (0..3)
            .min_by(|&x, &y| slope(cands[x]).total_cmp(&slope(cands[y])))
            .unwrap_or(0);
        let row = cands[i_row];
        let col = if i_row == 0 { t2 } else { t1 };
        let det = row.0 * col.1 - row.1 * col.0;
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        let per_cell = if turned.is_some() { 2.0 } else { 1.0 };
        let est = w * h / det.abs().max(1e-300) * per_cell;
        if !(est.is_finite() && det.abs() > 1e-12) || est > (4 * TILE_CAP) as f64 {
            return Err(too_many(if est.is_finite() && est < 1e18 {
                est as u64
            } else {
                u64::MAX
            }));
        }
        let origin = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        let offsets: Vec<(P2, bool)> = match turned {
            Some(d) => vec![((0.0, 0.0), false), (d, true)],
            None => vec![((0.0, 0.0), false)],
        };
        // Lattice coordinates (j along `col`) of the box's corners.
        let jof = |p: P2| ((p.1 - origin.1) * row.0 - (p.0 - origin.0) * row.1) / det;
        let corners = [(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])];
        let mut out = Vec::new();
        for &(off, flip) in &offsets {
            let js = corners.iter().map(|&q| jof((q.0 - off.0, q.1 - off.1)));
            let (jlo, jhi) = js.fold((f64::INFINITY, f64::NEG_INFINITY), |a, j| {
                (a.0.min(j), a.1.max(j))
            });
            let (jlo, jhi) = (jlo.ceil() - 1.0, jhi.floor() + 1.0);
            let mut j = jlo;
            while j <= jhi {
                // Centres origin + off + i·row + j·col inside the box: an
                // interval of i from each coordinate.
                let base = (origin.0 + off.0 + j * col.0, origin.1 + off.1 + j * col.1);
                let mut lo = f64::NEG_INFINITY;
                let mut hi = f64::INFINITY;
                for (bv, rv, a0, a1) in [(base.0, row.0, b[0], b[2]), (base.1, row.1, b[1], b[3])] {
                    if rv.abs() < 1e-300 {
                        if !(bv >= a0 && bv < a1) {
                            lo = f64::INFINITY;
                        }
                    } else {
                        let (x, y) = ((a0 - bv) / rv, (a1 - bv) / rv);
                        lo = lo.max(x.min(y));
                        hi = hi.min(x.max(y));
                    }
                }
                let (ilo, ihi) = (lo.ceil(), hi.floor());
                if ilo <= ihi {
                    let k = ihi - ilo + 1.0;
                    if out.len() as f64 + k > TILE_CAP as f64 {
                        return Err(too_many((out.len() as f64 + k) as u64));
                    }
                    let mut i = ilo;
                    while i <= ihi {
                        let p = (base.0 + i * row.0, base.1 + i * row.1);
                        // Inside [u0, u1) × [v0, v1).
                        if p.0 >= b[0] && p.0 < b[2] && p.1 >= b[1] && p.1 < b[3] {
                            out.push((p, flip));
                        }
                        i += 1.0;
                    }
                }
                j += 1.0;
            }
        }
        Ok(out)
    }

    /// The placed copies, or a cap's sentence.
    fn copies(&self, f: &Frame) -> Result<Vec<Placed>, TransformError> {
        let c = &self.cfg;
        let b = Self::bx(f);
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        let tiles = self.tiles(b)?;
        let mut out = Vec::with_capacity(tiles.len());
        for (k, &(centre, flip)) in tiles.iter().enumerate() {
            let mut rng = Rng::new(
                f.seed ^ 0x504F_4C59_3235 ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            );
            let (mut l, shift) = self.local(w, h, c.tile.map(|t| t.1), &mut rng);
            if flip {
                l.verts.iter_mut().for_each(|p| *p = (-p.0, -p.1));
            }
            out.push(self.place(&l, (centre.0 + shift.0, centre.1 + shift.1)));
        }
        Ok(out)
    }

    /// The caps, with sentences (before anything is drawn).
    fn check(&self, f: &Frame) -> Result<(), TransformError> {
        let c = &self.cfg;
        let tiles = self.tiles(Self::bx(f))?.len() as u64;
        let per = 2 + 2 * u64::from(c.nest.map_or(0, |n| n.0));
        let rings = tiles.saturating_mul(per);
        if rings > RING_CAP {
            return Err(TransformError::TooLarge(format!(
                "Polygons would draw {} rings; at most {} are allowed — use fewer tiles or concentric polygons.",
                group_digits(rings),
                group_digits(RING_CAP)
            )));
        }
        let points = rings.saturating_mul(ring_points(c));
        if points > POINT_CAP {
            return Err(TransformError::TooLarge(format!(
                "Polygons would build {} outline points; at most {} are allowed — use fewer tiles, sides or concentric polygons, or no rounding, morph or twist.",
                group_digits(points),
                group_digits(POINT_CAP)
            )));
        }
        let lines = tiles.saturating_mul(self.lines_per_copy());
        if lines > LINE_CAP {
            return Err(TransformError::TooLarge(format!(
                "Polygons would draw {} line segments; at most {} are allowed — use fewer tiles, sides, spokes or subdivisions.",
                group_digits(lines),
                group_digits(LINE_CAP)
            )));
        }
        Ok(())
    }

    /// The scene: per copy its outline band's rings and the concentric
    /// outlines', its edges, its line features.
    fn scene(&self, copies: &[Placed]) -> Scene {
        let c = &self.cfg;
        let th = c.thick;
        // How far the outline's outer edge lies outside the shape.
        let a = match c.placement {
            0 => th,
            1 => th / 2.0,
            _ => 0.0,
        };
        let mut specs = Vec::with_capacity(copies.len());
        let mut rings = Vec::new();
        let mut lines = Vec::new();
        for (k, p) in copies.iter().enumerate() {
            let id = k as u16;
            let convex = p.shape.is_convex();
            let off = |d: f64| {
                if convex {
                    p.shape.offset(d)
                } else {
                    p.shape.polar_offset(p.centre, d)
                }
            };
            let ring = |poly: Poly, role: Role| RingSpec {
                convex: poly.is_convex(),
                poly,
                role,
                copy: id,
            };
            rings.push(ring(off(a), Role::Outer));
            rings.push(ring(off(a - th), Role::Inner));
            if let Some((n, sp)) = c.nest {
                let sp = if sp > 0.0 {
                    sp
                } else {
                    // Evenly towards the centre: the inradius shared.
                    let n_edges = p.shape.pts.len();
                    let inr = (0..n_edges)
                        .map(|i| {
                            let (u, v) = p.shape.edge(i);
                            seg_dist(p.centre, u, v)
                        })
                        .fold(f64::INFINITY, f64::min);
                    (inr / f64::from(n + 1)).floor().max(1.0)
                };
                for j in 1..=n {
                    let d = f64::from(j) * sp;
                    let out = off(a - d);
                    if out.is_empty() {
                        break;
                    }
                    rings.push(ring(out, Role::NestOuter));
                    rings.push(ring(off(a - th - d), Role::NestInner));
                }
            }
            let nv = p.verts.len();
            specs.push(CopySpec {
                base: p.shape.clone(),
                hole: None,
                centre: p.centre,
                corner: p.verts.first().copied().unwrap_or(p.centre),
                radii: (p.radii.0, p.radii.1, 0.0),
                side_segs: (0..nv)
                    .map(|i| (p.verts[i], p.verts[(i + 1) % nv]))
                    .collect(),
            });
            lines.extend(p.lines.iter().copied());
        }
        Scene::with_copies(specs, rings, lines)
    }

    /// How the scene is drawn: fills, edge brushes, gradients, accents.
    fn style(&self, f: &Frame, copies: &[Placed]) -> Style {
        let c = &self.cfg;
        let mut st = Style {
            filled: c.filled,
            subs: c.subs.clone(),
            ..Style::default()
        };
        if let Some((sp, ph)) = c.scan {
            // Counted from the lowest row any copy reaches.
            let low = copies
                .iter()
                .map(|p| p.shape.bbox()[1])
                .fold(f64::INFINITY, f64::min);
            let r0 = if low.is_finite() {
                (low.floor() as i64).saturating_add(ph)
            } else {
                i64::from(f.lo)
            };
            st.row_stripes.push((r0, sp));
        }
        if let Some(d) = &c.density {
            st.edge_dash = Some((1.0, d.clone(), 0.0));
        }
        if let Some((v0, v1, g0, g1)) = c.grad {
            st.grads.push((
                Grad {
                    vel: Some((v0, v1)),
                    gate: Some((g0, g1)),
                    ..Grad::default()
                },
                GDir::Edge,
            ));
        }
        if let Some((vel, radius)) = c.accent {
            let pts = copies.iter().flat_map(|p| p.verts.iter().copied());
            st.accent = Some(CellAccent::new(vel, radius, pts));
        }
        st
    }
}

/// Distance from `p` to segment `a → b`.
fn seg_dist(p: P2, a: P2, b: P2) -> f64 {
    let d = (b.0 - a.0, b.1 - a.1);
    let l2 = d.0 * d.0 + d.1 * d.1;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * d.0).hypot(p.1 - a.1 - t * d.1)
}

impl Shapes for Polygons {
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

fn polygons_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let polygons = Polygons::new(Cfg::read(p));
    polygons.check(&f)?;
    super::kernel(p, b, polygons)
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
        let spec = polygons_spec();
        let mut rng = Rng::new(0x5345_3536);
        for draw in 0..300 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 300)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            p.set("gen.seed", ParamValue::Int(rng.range_i64(1, 1000)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            for (k, lo, hi) in [
                ("c01_side_count.sides", 3, 12),
                ("c09_border_thickness.size", 1, 6),
                ("c10_radial_spokes.every", 1, 4),
                ("c11_triangulated_fill.levels", 0, 3),
                ("c12_scanline_fill.spacing", 2, 6),
                ("c13_concentric_polygons.count", 1, 6),
                ("c13_concentric_polygons.spacing", 0, 4),
            ] {
                p.set(k, ParamValue::Int(rng.range_i64(lo, hi)));
            }
            for (k, lo, hi) in [
                ("c05_rotation.angle", -180.0, 180.0),
                ("c06_star_inner_radius_mode.inner", 5.0, 95.0),
                ("c16_chamfer_corners.size", 0.5, 4.0),
                ("c17_rounded_corners.radius", 0.5, 6.0),
                ("c19_vertex_accents.radius", 0.5, 4.0),
                ("c22_twist_skew.skew", -60.0, 60.0),
                ("c22_twist_skew.twist", -360.0, 360.0),
                ("c23_perspective.top", 10.0, 300.0),
                ("c24_tessellation_tiling.radius", 2.0, 12.0),
                ("c24_tessellation_tiling.gap", 0.0, 3.0),
                (
                    "c25_seeded_vertex_perturbation_shape_morph.amount",
                    0.0,
                    50.0,
                ),
                (
                    "c25_seeded_vertex_perturbation_shape_morph.morph",
                    0.0,
                    100.0,
                ),
            ] {
                p.set(k, ParamValue::Float(rng.range_f64(lo, hi)));
            }
            p.set(
                "c07_outline.placement",
                ParamValue::Choice(rng.below(3) as u32),
            );
            p.set(
                "c24_tessellation_tiling.layout",
                ParamValue::Choice(rng.below(3) as u32),
            );
            p.set(
                "c11_triangulated_fill.from",
                ParamValue::Choice(rng.below(2) as u32),
            );
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let g = Polygons::new(Cfg::read(&params));
            if g.check(&f).is_err() {
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
