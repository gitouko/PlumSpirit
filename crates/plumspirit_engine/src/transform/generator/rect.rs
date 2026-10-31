//! Rectangle / Square (§153, `gen.rect`; `docs/GENERATORS.md` "Rectangle /
//! Square"): the generation box's outline as whole grid cells, and the
//! contract's 25 functions (catalog ids `c01_…` … `c25_…`, switch = id,
//! controls `<id>.<name>`), drawn by the shared region rasterizer
//! (`generator/region.rs`).
//!
//! The pipeline, in this order (grid space: u = ticks / step, v = rows):
//!
//! 1. **Shape** — the box (c01 Width / c02 Height with their alignment, c03
//!    a square on the grid), its corners chamfered (c08) and rounded (c07),
//!    then perspective (c11, a keystone homography: the top edge scaled
//!    about the centre), shear (c10) and rotation (c09) about the centre.
//! 2. **Outline** — a band of whole cells along the shape's edge, one grid
//!    unit wide (c06 thicker), inside the shape (c04: outside it or centred
//!    on the edge); c25 cuts a hole (a smaller copy of the shape) with its
//!    own outline; c12 / c13 nested outlines inside; c24 seeded walls
//!    splitting the interior.
//! 3. **Interior** — nothing, or (c05) filled; c14 / c15 stripes, c16
//!    crosshatch, c17 grid, c18 the corner diagonals: drawn where the
//!    patterns are — filled, where they are not.
//! 4. **Brushes** — c19 a subchannel per side, c20 every Nth cell per side,
//!    c23 dashes along the outline with a phase, c21 / c22 velocity and
//!    gate gradients (start to end, bottom to top, or around the shape).
//!
//! Only c24 draws random values (`gen.seed`).

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::Rng;
use super::super::{Kernel, TransformError};
use super::draw::{Aff, Grad};
use super::pen::Draw;
use super::region::{GDIRS, GDir, P2, Poly, Role, Scene, Style, XPat};
use super::{Frame, MAX_LENGTH, Shapes};
use std::sync::OnceLock;

pub const RECT: ToolDef = ToolDef {
    id: "gen.rect",
    spec: rect_spec,
    build: rect_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_width", "Width", 10),
    ("c02_height", "Height", 20),
    ("c03_square_aspect_lock", "Square Aspect Lock", 30),
    ("c04_outline_mode", "Outline Mode", 40),
    ("c05_filled_mode", "Filled Mode", 50),
    ("c06_border_thickness", "Border Thickness", 60),
    ("c07_rounded_corners", "Rounded Corners", 70),
    ("c08_chamfered_corners", "Chamfered Corners", 80),
    ("c09_rotation", "Rotation", 90),
    ("c10_shear", "Shear", 100),
    ("c11_perspective", "Perspective", 110),
    ("c12_nested_rectangles", "Nested Rectangles", 120),
    ("c13_concentric_spacing", "Concentric Spacing", 130),
    ("c14_horizontal_stripe_fill", "Horizontal Stripe Fill", 140),
    ("c15_vertical_stripe_fill", "Vertical Stripe Fill", 150),
    ("c16_crosshatch_fill", "Crosshatch Fill", 160),
    ("c17_grid_fill", "Grid Fill", 170),
    ("c18_diagonal_corner_lines", "Diagonal Corner Lines", 180),
    ("c19_per_edge_subchannels", "Per-Edge Subchannels", 190),
    ("c20_per_edge_density", "Per-Edge Density", 200),
    ("c21_velocity_gradient", "Velocity Gradient", 210),
    ("c22_gate_gradient", "Gate Gradient", 220),
    ("c23_dashed_border_phase", "Dashed Border Phase", 230),
    ("c24_recursive_subdivision", "Recursive Subdivision", 240),
    (
        "c25_hole_negative_interior_mask",
        "Hole / Negative Interior Mask",
        250,
    ),
];

const H_ALIGN: &[&str] = &["The box's start", "The box's center", "The box's end"];
const V_ALIGN: &[&str] = &["The box's bottom", "The box's center", "The box's top"];
const FOLLOW: &[&str] = &["The smaller side", "The width", "The height"];
const PLACEMENT: &[&str] = &[
    "Outside the shape",
    "Centered on its edge",
    "Inside the shape",
];
const DIAGONALS: &[&str] = &[
    "Both diagonals",
    "Bottom-left to top-right",
    "Top-left to bottom-right",
];
/// The sides' names (control suffixes and labels), in side order.
const SIDES: [(&str, &str); 4] = [
    ("bottom", "Bottom"),
    ("right", "Right"),
    ("top", "Top"),
    ("left", "Left"),
];
/// Most nested outlines.
pub const NEST_CAP: u32 = 64;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const RECT_HELP: &[(&str, &str)] = &[
    (
        "c01_width",
        "Sets the rectangle's width instead of filling the whole box.",
    ),
    ("c01_width.width", "How wide the rectangle is, in ticks."),
    (
        "c01_width.align",
        "Where the narrower rectangle sits in the box: at its start, its center or its end.",
    ),
    (
        "c02_height",
        "Sets the rectangle's height instead of filling the box's whole pitch range.",
    ),
    ("c02_height.rows", "How many rows tall the rectangle is."),
    (
        "c02_height.align",
        "Where the lower rectangle sits in the box: at its bottom, its center or its top.",
    ),
    (
        "c03_square_aspect_lock",
        "Makes the shape a square on the grid.",
    ),
    (
        "c04_outline_mode",
        "Chooses where the outline's cells lie against the shape's edge.",
    ),
    (
        "c05_filled_mode",
        "Fills the whole inside of the shape with notes instead of drawing only its outline.",
    ),
    ("c06_border_thickness", "Makes the outline thicker."),
    ("c07_rounded_corners", "Rounds off the corners."),
    (
        "c07_rounded_corners.radius",
        "The corners' radius, in grid units.",
    ),
    ("c08_chamfered_corners", "Cuts the corners off at an angle."),
    (
        "c08_chamfered_corners.size",
        "How much of each corner is cut off, in grid units.",
    ),
    ("c09_rotation", "Turns the shape about its center."),
    ("c10_shear", "Slants the shape like a parallelogram."),
    (
        "c11_perspective",
        "Narrows (or widens) the top edge as if the shape is seen in perspective.",
    ),
    (
        "c12_nested_rectangles",
        "Draws outlines inside the shape, one inside the other.",
    ),
    (
        "c13_concentric_spacing",
        "Spaces the inner outlines a set distance apart.",
    ),
    (
        "c14_horizontal_stripe_fill",
        "Fills the inside with horizontal stripes (or, with Filled Mode, cuts them out).",
    ),
    (
        "c14_horizontal_stripe_fill.spacing",
        "How many rows apart the stripes are.",
    ),
    (
        "c15_vertical_stripe_fill",
        "Fills the inside with vertical stripes (or, with Filled Mode, cuts them out).",
    ),
    (
        "c15_vertical_stripe_fill.spacing",
        "How many resolution steps apart the stripes are.",
    ),
    (
        "c16_crosshatch_fill",
        "Fills the inside with diagonal lines both ways (or, with Filled Mode, cuts them out).",
    ),
    (
        "c17_grid_fill",
        "Fills the inside with a grid of lines (or, with Filled Mode, cuts them out).",
    ),
    (
        "c17_grid_fill.spacing",
        "How far apart the grid lines are, in steps across and rows up.",
    ),
    (
        "c18_diagonal_corner_lines",
        "Draws lines from corner to corner across the inside.",
    ),
    (
        "c18_diagonal_corner_lines.which",
        "Which diagonals are drawn: both, the rising one, or the falling one.",
    ),
    (
        "c19_per_edge_subchannels",
        "Gives each edge of the outline a subchannel (color) of its own.",
    ),
    (
        "c19_per_edge_subchannels.bottom",
        "The bottom edge's subchannel, by its position in the track's list (0 = the first).",
    ),
    (
        "c19_per_edge_subchannels.right",
        "The right edge's subchannel, by its position in the track's list.",
    ),
    (
        "c19_per_edge_subchannels.top",
        "The top edge's subchannel, by its position in the track's list.",
    ),
    (
        "c20_per_edge_density",
        "Draws only every Nth cell along each edge, set per edge.",
    ),
    (
        "c20_per_edge_density.bottom",
        "How many cells apart the notes on the bottom edge are (1 = every cell).",
    ),
    (
        "c20_per_edge_density.right",
        "How many cells apart the notes on the right edge are (1 = every cell).",
    ),
    (
        "c20_per_edge_density.top",
        "How many cells apart the notes on the top edge are (1 = every cell).",
    ),
    (
        "c20_per_edge_density.left",
        "How many cells apart the notes on the left edge are (1 = every cell).",
    ),
    (
        "c21_velocity_gradient",
        "Fades the velocity across the shape.",
    ),
    (
        "c21_velocity_gradient.from",
        "The velocity where the gradient starts.",
    ),
    (
        "c21_velocity_gradient.to",
        "The velocity where the gradient ends.",
    ),
    (
        "c21_velocity_gradient.direction",
        "How the gradient runs: from the shape's start to its end, from its bottom to its top, or around it.",
    ),
    (
        "c22_gate_gradient",
        "Changes the note lengths across the shape.",
    ),
    (
        "c22_gate_gradient.from",
        "The gate (share of the cell) where the gradient starts.",
    ),
    (
        "c22_gate_gradient.to",
        "The gate (share of the cell) where the gradient ends.",
    ),
    (
        "c22_gate_gradient.direction",
        "How the gradient runs: from the shape's start to its end, from its bottom to its top, or around it.",
    ),
    ("c23_dashed_border_phase", "Breaks the outline into dashes."),
    (
        "c23_dashed_border_phase.dash",
        "How many cells long each dash is.",
    ),
    (
        "c23_dashed_border_phase.gap",
        "How many cells long each gap is.",
    ),
    (
        "c24_recursive_subdivision",
        "Splits the inside with walls again and again at random places (seeded), like a floor plan.",
    ),
    (
        "c25_hole_negative_interior_mask",
        "Cuts a hole in the middle of the shape.",
    ),
];

fn rect_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        // c01
        flag("c01_width", "Width", false),
        ticks("c01_width.width", "Width (ticks)", 1, MAX_LENGTH, 1920),
        choice("c01_width.align", "Aligned with", H_ALIGN, 0),
        // c02
        flag("c02_height", "Height", false),
        int("c02_height.rows", "Height (rows)", 1, 127, 12).keyed(),
        choice("c02_height.align", "Aligned with", V_ALIGN, 0),
        // c03
        flag("c03_square_aspect_lock", "Square Aspect Lock", false),
        choice("c03_square_aspect_lock.follow", "Side", FOLLOW, 0).help(
            "A square on the grid — as many rows as resolution steps — whose side is the rectangle's smaller side, its width or its height.",
        ),
        // c04
        flag("c04_outline_mode", "Outline Mode", false),
        choice("c04_outline_mode.placement", "Outline", PLACEMENT, 0).help(
            "Where the outline's cells lie: around the shape, half in and half out, or inside it (as without this function).",
        ),
        // c05
        flag("c05_filled_mode", "Filled Mode", false),
        // c06
        flag("c06_border_thickness", "Border Thickness", false),
        int("c06_border_thickness.size", "Thickness (grid units)", 1, 64, 2)
            .help("The outline's width: steps across on the sides, rows up on the top and bottom."),
        // c07
        flag("c07_rounded_corners", "Rounded Corners", false),
        float("c07_rounded_corners.radius", "Radius (grid units)", 0.5, 256.0, 3.0, ""),
        // c08
        flag("c08_chamfered_corners", "Chamfered Corners", false),
        float("c08_chamfered_corners.size", "Cut (grid units)", 0.5, 256.0, 3.0, ""),
        // c09
        flag("c09_rotation", "Rotation", false),
        float("c09_rotation.angle", "Angle", -180.0, 180.0, 30.0, "°")
            .help("About the shape's center, on the grid: one resolution step across per row up is 45°."),
        // c10
        flag("c10_shear", "Shear", false),
        float("c10_shear.angle", "Angle", -75.0, 75.0, 30.0, "°")
            .help("Each row above the center moves later in time (earlier below it), on the grid."),
        // c11
        flag("c11_perspective", "Perspective", false),
        float("c11_perspective.top", "Top width (% of the bottom)", 1.0, 400.0, 50.0, "%")
            .help("The top edge scaled about the center, the sides straight, rows drawn closer towards the narrower end."),
        // c12
        flag("c12_nested_rectangles", "Nested Rectangles", false),
        int("c12_nested_rectangles.count", "Rectangles inside", 1, 64, 3)
            .help("Outlines inside the shape, evenly spaced towards its center (or Concentric Spacing's)."),
        // c13
        flag("c13_concentric_spacing", "Concentric Spacing", false),
        int("c13_concentric_spacing.spacing", "Spacing (grid units)", 1, 256, 3)
            .help("Outlines every so many grid units inside the shape, as many as fit (or Nested Rectangles' count)."),
        // c14
        flag("c14_horizontal_stripe_fill", "Horizontal Stripe Fill", false),
        int("c14_horizontal_stripe_fill.spacing", "A stripe every (rows)", 2, 127, 2).keyed(),
        // c15
        flag("c15_vertical_stripe_fill", "Vertical Stripe Fill", false),
        int("c15_vertical_stripe_fill.spacing", "A stripe every (steps)", 2, 64, 2),
        // c16
        flag("c16_crosshatch_fill", "Crosshatch Fill", false),
        int("c16_crosshatch_fill.spacing", "Line spacing (steps)", 2, 64, 4)
            .help("Diagonal lines both ways, one step across per row up, every so many steps."),
        // c17
        flag("c17_grid_fill", "Grid Fill", false),
        int("c17_grid_fill.spacing", "Spacing (steps and rows)", 2, 64, 4),
        // c18
        flag("c18_diagonal_corner_lines", "Diagonal Corner Lines", false),
        choice("c18_diagonal_corner_lines.which", "Lines", DIAGONALS, 0),
        // c19
        flag("c19_per_edge_subchannels", "Per-Edge Subchannels", false),
        int("c19_per_edge_subchannels.bottom", "Bottom edge (subchannel position)", 0, 4095, 0),
        int("c19_per_edge_subchannels.right", "Right edge (subchannel position)", 0, 4095, 1),
        int("c19_per_edge_subchannels.top", "Top edge (subchannel position)", 0, 4095, 2),
        int("c19_per_edge_subchannels.left", "Left edge (subchannel position)", 0, 4095, 3)
            .help("Each outline cell takes the subchannel of the shape's nearest edge (the corners go with the top and bottom)."),
        // c20
        flag("c20_per_edge_density", "Per-Edge Density", false),
        int("c20_per_edge_density.bottom", "Bottom edge: a note every (cells)", 1, 64, 2),
        int("c20_per_edge_density.right", "Right edge: a note every (cells)", 1, 64, 2),
        int("c20_per_edge_density.top", "Top edge: a note every (cells)", 1, 64, 2),
        int("c20_per_edge_density.left", "Left edge: a note every (cells)", 1, 64, 2),
        // c21
        flag("c21_velocity_gradient", "Velocity Gradient", false),
        int("c21_velocity_gradient.from", "Velocity at the start", 1, 127, 127),
        int("c21_velocity_gradient.to", "Velocity at the end", 1, 127, 40),
        choice("c21_velocity_gradient.direction", "Direction", GDIRS, 0),
        // c22
        flag("c22_gate_gradient", "Gate Gradient", false),
        float("c22_gate_gradient.from", "Gate at the start", 1.0, 100.0, 100.0, "%"),
        float("c22_gate_gradient.to", "Gate at the end", 1.0, 100.0, 25.0, "%"),
        choice("c22_gate_gradient.direction", "Direction", GDIRS, 0),
        // c23
        flag("c23_dashed_border_phase", "Dashed Border Phase", false),
        int("c23_dashed_border_phase.dash", "Dash (cells)", 1, 256, 2),
        int("c23_dashed_border_phase.gap", "Gap (cells)", 1, 256, 2),
        float("c23_dashed_border_phase.phase", "Phase (cells along the outline)", 0.0, 256.0, 0.0, "")
            .help("Dashes along the outline, continuing round the corners; the phase moves them along it."),
        // c24
        flag("c24_recursive_subdivision", "Recursive Subdivision", false),
        int("c24_recursive_subdivision.depth", "Depth", 1, 8, 3).help(
            "Walls splitting the interior in two, again and again: across the longer side, at a seeded place between 30 and 70 %.",
        ),
        // c25
        flag(
            "c25_hole_negative_interior_mask",
            "Hole / Negative Interior Mask",
            false,
        ),
        float(
            "c25_hole_negative_interior_mask.size",
            "Hole size (% of the shape)",
            5.0,
            95.0,
            50.0,
            "%",
        )
        .help("A smaller copy of the shape at its center is cut out: nothing is drawn inside it, and it has its own outline."),
    ];
    with_help(&mut own, RECT_HELP);
    super::spec(
        "gen.rect",
        "Rectangle / Square",
        "Generates rectangles and squares of notes: the generation box's outline (where, length and pitch range above), one cell wide on the resolution grid.",
        own,
        FUNCTIONS,
    )
}

/// The functions' settings (`None` / neutral = off).
#[derive(Clone, Debug, Default)]
struct Cfg {
    /// c01: ticks, alignment.
    width: Option<(f64, usize)>,
    /// c02: rows, alignment.
    height: Option<(f64, usize)>,
    /// c03: which side the square follows.
    square: Option<usize>,
    /// c04: 0 outside, 1 centred, 2 inside (the base).
    placement: usize,
    /// c05.
    filled: bool,
    /// c06: grid units (1 = off).
    thick: f64,
    /// c07 radius, c08 cut (grid units).
    round: Option<f64>,
    chamfer: Option<f64>,
    /// c09, c10: radians; c11: the top's scale.
    rotate: Option<f64>,
    shear: Option<f64>,
    /// B2-F01: c09's rotation before c10's shear (`order::RECT_MAPS`;
    /// declared: the shear first).
    rotate_first: bool,
    persp: Option<f64>,
    /// c12 count, c13 spacing.
    nest_count: Option<u32>,
    nest_spacing: Option<f64>,
    /// c14 – c17: spacings.
    h_stripes: Option<i64>,
    v_stripes: Option<f64>,
    hatch: Option<f64>,
    grid: Option<f64>,
    /// c18.
    diagonals: Option<usize>,
    /// c19, c20: per side.
    subs: Option<[u32; 4]>,
    density: Option<[f64; 4]>,
    /// c21, c22: from, to, direction.
    vel: Option<(f64, f64, usize)>,
    gate: Option<(f64, f64, usize)>,
    /// c23: dash, gap, phase (cells).
    dash: Option<(f64, f64, f64)>,
    /// c24: depth.
    subdiv: Option<u32>,
    /// c25: the hole's size (fraction).
    hole: Option<f64>,
}

impl Cfg {
    fn read(p: &Params) -> Cfg {
        let on = |id: &str| p.flag(id);
        let mut c = Cfg {
            placement: 2,
            thick: 1.0,
            ..Cfg::default()
        };
        if on("c01_width") {
            c.width = Some((
                p.ticks("c01_width.width").max(1) as f64,
                p.choice("c01_width.align"),
            ));
        }
        if on("c02_height") {
            c.height = Some((
                p.int("c02_height.rows").max(1) as f64,
                p.choice("c02_height.align"),
            ));
        }
        if on("c03_square_aspect_lock") {
            c.square = Some(p.choice("c03_square_aspect_lock.follow"));
        }
        if on("c04_outline_mode") {
            c.placement = p.choice("c04_outline_mode.placement").min(2);
        }
        c.filled = on("c05_filled_mode");
        if on("c06_border_thickness") {
            c.thick = p.int("c06_border_thickness.size").clamp(1, 64) as f64;
        }
        if on("c07_rounded_corners") {
            c.round = Some(p.float("c07_rounded_corners.radius").max(0.0));
        }
        if on("c08_chamfered_corners") {
            c.chamfer = Some(p.float("c08_chamfered_corners.size").max(0.0));
        }
        if on("c09_rotation") {
            c.rotate = Some(p.float("c09_rotation.angle").to_radians());
        }
        if on("c10_shear") {
            c.shear = Some(p.float("c10_shear.angle").clamp(-75.0, 75.0).to_radians());
        }
        c.rotate_first =
            p.sequence(crate::transform::order::RECT_MAPS.members)[0] == "c09_rotation";
        if on("c11_perspective") {
            c.persp = Some((p.float("c11_perspective.top") / 100.0).clamp(0.01, 4.0));
        }
        if on("c12_nested_rectangles") {
            c.nest_count = Some(p.int("c12_nested_rectangles.count").clamp(1, 64) as u32);
        }
        if on("c13_concentric_spacing") {
            c.nest_spacing = Some(p.int("c13_concentric_spacing.spacing").max(1) as f64);
        }
        if on("c14_horizontal_stripe_fill") {
            c.h_stripes = Some(p.int("c14_horizontal_stripe_fill.spacing").max(2));
        }
        if on("c15_vertical_stripe_fill") {
            c.v_stripes = Some(p.int("c15_vertical_stripe_fill.spacing").max(2) as f64);
        }
        if on("c16_crosshatch_fill") {
            c.hatch = Some(p.int("c16_crosshatch_fill.spacing").max(2) as f64);
        }
        if on("c17_grid_fill") {
            c.grid = Some(p.int("c17_grid_fill.spacing").max(2) as f64);
        }
        if on("c18_diagonal_corner_lines") {
            c.diagonals = Some(p.choice("c18_diagonal_corner_lines.which"));
        }
        let per_side = |k: &str, lo: i64, hi: i64| -> [i64; 4] {
            SIDES.map(|(s, _)| p.int(&format!("{k}.{s}")).clamp(lo, hi))
        };
        if on("c19_per_edge_subchannels") {
            c.subs = Some(per_side("c19_per_edge_subchannels", 0, 4095).map(|v| v as u32));
        }
        if on("c20_per_edge_density") {
            c.density = Some(per_side("c20_per_edge_density", 1, 64).map(|v| v as f64));
        }
        if on("c21_velocity_gradient") {
            c.vel = Some((
                p.int("c21_velocity_gradient.from") as f64,
                p.int("c21_velocity_gradient.to") as f64,
                p.choice("c21_velocity_gradient.direction"),
            ));
        }
        if on("c22_gate_gradient") {
            c.gate = Some((
                p.float("c22_gate_gradient.from") / 100.0,
                p.float("c22_gate_gradient.to") / 100.0,
                p.choice("c22_gate_gradient.direction"),
            ));
        }
        if on("c23_dashed_border_phase") {
            c.dash = Some((
                p.int("c23_dashed_border_phase.dash").max(1) as f64,
                p.int("c23_dashed_border_phase.gap").max(1) as f64,
                p.float("c23_dashed_border_phase.phase").max(0.0),
            ));
        }
        if on("c24_recursive_subdivision") {
            c.subdiv = Some(p.int("c24_recursive_subdivision.depth").clamp(1, 8) as u32);
        }
        if on("c25_hole_negative_interior_mask") {
            c.hole = Some(
                p.float("c25_hole_negative_interior_mask.size")
                    .clamp(5.0, 95.0)
                    / 100.0,
            );
        }
        c
    }
}

/// The rectangle in grid space before its maps: `[u0, u0 + w] × [v0, v0 + h]`.
#[derive(Clone, Copy, Debug)]
struct Frame2 {
    u0: f64,
    v0: f64,
    w: f64,
    h: f64,
}

/// The shape's maps: perspective, shear, rotation (grid space).
#[derive(Clone, Copy, Debug)]
struct Maps2 {
    /// Top scale, bottom row edge, height, centre u.
    persp: Option<(f64, f64, f64, f64)>,
    aff: Aff,
}

impl Maps2 {
    fn ap(&self, p: P2) -> P2 {
        let mut q = p;
        if let Some((k, v0, h, cx)) = self.persp {
            // The keystone homography: the bottom kept, the top scaled by k
            // about the centre, straight lines kept straight.
            let y = (q.1 - v0) / h;
            let den = 1.0 + (1.0 / k - 1.0) * y;
            q = (cx + (q.0 - cx) / den, v0 + h * y / (k * den));
        }
        self.aff.xy(q)
    }
}

/// The Rectangle / Square generator.
struct Rect {
    cfg: Cfg,
    built: OnceLock<(Scene, Style)>,
}

impl Rect {
    fn new(cfg: Cfg) -> Rect {
        Rect {
            cfg,
            built: OnceLock::new(),
        }
    }

    fn built(&self, f: &Frame) -> &(Scene, Style) {
        self.built.get_or_init(|| (self.scene(f), self.style(f)))
    }

    /// The rectangle in the box (c01 – c03).
    fn frame2(&self, f: &Frame) -> Frame2 {
        let c = &self.cfg;
        let step = f.step as f64;
        let (u0, u1) = (f.t0 as f64 / step, f.t1 as f64 / step);
        let (v0, v1) = (f64::from(f.lo), f64::from(f.hi) + 1.0);
        let (bw, bh) = (u1 - u0, v1 - v0);
        let mut w = c.width.map_or(bw, |(t, _)| t / step);
        let mut h = c.height.map_or(bh, |(r, _)| r);
        if let Some(follow) = c.square {
            let s = match follow {
                1 => w,
                2 => h,
                _ => w.min(h),
            }
            .round()
            .max(1.0);
            (w, h) = (s, s);
        }
        let ru0 = match c.width.map_or(0, |x| x.1) {
            1 => u0 + (bw - w) / 2.0,
            2 => u1 - w,
            _ => u0,
        };
        let rv0 = match c.height.map_or(0, |x| x.1) {
            1 => v0 + ((bh - h) / 2.0).floor(),
            2 => v1 - h,
            _ => v0,
        };
        Frame2 {
            u0: ru0,
            v0: rv0,
            w,
            h,
        }
    }

    fn maps(&self, r: &Frame2) -> Maps2 {
        let c = &self.cfg;
        let (cx, cy) = (r.u0 + r.w / 2.0, r.v0 + r.h / 2.0);
        let shear = c.shear.map_or(Aff::ID, |t| Aff::shear(t, cy, 1.0));
        let rotate = c.rotate.map_or(Aff::ID, |t| Aff::rotate(t, cx, cy, 1.0));
        // B2-F01: in the user's order (declared: shear, then rotation).
        let aff = if c.rotate_first {
            Aff::ID.then(rotate).then(shear)
        } else {
            Aff::ID.then(shear).then(rotate)
        };
        Maps2 {
            persp: c.persp.filter(|_| r.h > 0.0).map(|k| (k, r.v0, r.h, cx)),
            aff,
        }
    }

    /// The corners of `[u0, u0 + w] × [v0, v0 + h]` (c08, then c07).
    fn cornered(&self, p: Poly) -> Poly {
        let mut p = p;
        if let Some(cut) = self.cfg.chamfer {
            p = p.chamfer(cut);
        }
        if let Some(r) = self.cfg.round {
            p = p.round(r);
        }
        p
    }

    /// The seeded walls of c24 inside `cell` (frame coordinates; each a
    /// band `thick` wide across the longer side of what it splits).
    fn walls(&self, f: &Frame, cell: [f64; 4], depth: u32, thick: f64) -> Vec<[f64; 4]> {
        let mut rng = Rng::new(f.seed ^ 0x5245_4354_5331);
        let mut out = Vec::new();
        let min = 2.0 * thick + 2.0;
        let mut stack = vec![(cell, depth)];
        while let Some(([x0, y0, x1, y1], d)) = stack.pop() {
            let (cw, ch) = (x1 - x0, y1 - y0);
            if d == 0 || !(cw >= min || ch >= min) {
                continue;
            }
            let vertical = if cw >= min && ch >= min {
                if cw > ch {
                    true
                } else if ch > cw {
                    false
                } else {
                    rng.chance(0.5)
                }
            } else {
                cw >= min
            };
            let t = rng.range_f64(0.3, 0.7);
            if vertical {
                let x = x0 + ((cw - thick) * t).round().clamp(1.0, cw - thick - 1.0);
                out.push([x, y0, x + thick, y1]);
                stack.push(([x + thick, y0, x1, y1], d - 1));
                stack.push(([x0, y0, x, y1], d - 1));
            } else {
                let y = y0 + ((ch - thick) * t).round().clamp(1.0, ch - thick - 1.0);
                out.push([x0, y, x1, y + thick]);
                stack.push(([x0, y + thick, x1, y1], d - 1));
                stack.push(([x0, y0, x1, y], d - 1));
            }
        }
        out
    }

    /// The scene: the outline band's rings, the hole's, the nested
    /// outlines, the walls and the corner diagonals.
    fn scene(&self, f: &Frame) -> Scene {
        let c = &self.cfg;
        let r = self.frame2(f);
        let m = self.maps(&r);
        let t = |p: P2| m.ap(p);
        let (u1, v1) = (r.u0 + r.w, r.v0 + r.h);
        let base = self.cornered(Poly::rect(r.u0, r.v0, u1, v1)).map(t);
        let hole = c.hole.map(|k| {
            let (hw, hh) = ((r.w * k).round().max(1.0), (r.h * k).round().max(1.0));
            let hu0 = r.u0 + ((r.w - hw) / 2.0).floor();
            let hv0 = r.v0 + ((r.h - hh) / 2.0).floor();
            self.cornered(Poly::rect(hu0, hv0, hu0 + hw, hv0 + hh))
                .map(t)
        });
        let th = c.thick;
        // How far the outline's outer edge lies outside the shape.
        let a = match c.placement {
            0 => th,
            1 => th / 2.0,
            _ => 0.0,
        };
        let mut rings = vec![
            (base.offset(a), Role::Outer),
            (base.offset(a - th), Role::Inner),
        ];
        if let Some(h) = &hole {
            rings.push((h.offset(-a), Role::HoleOuter));
            rings.push((h.offset(th - a), Role::HoleInner));
        }
        // c12 / c13: nested outlines.
        let inr = r.w.min(r.h) / 2.0;
        let nest = match (c.nest_count, c.nest_spacing) {
            (Some(n), Some(sp)) => Some((n, sp)),
            (Some(n), None) => Some((n, (inr / f64::from(n + 1)).floor().max(1.0))),
            (None, Some(sp)) => {
                let n = (inr / sp).floor().clamp(0.0, f64::from(NEST_CAP)) as u32;
                (n > 0).then_some((n, sp))
            }
            (None, None) => None,
        };
        if let Some((n, sp)) = nest {
            for k in 1..=n.min(NEST_CAP) {
                let d = f64::from(k) * sp;
                let out = base.offset(a - d);
                if out.is_empty() {
                    break;
                }
                rings.push((out, Role::NestOuter));
                rings.push((base.offset(a - th - d), Role::NestInner));
            }
        }
        // c24: walls across the interior.
        if let Some(depth) = c.subdiv {
            let d = th - a;
            let cell = [r.u0 + d, r.v0 + d, u1 - d, v1 - d];
            for [x0, y0, x1, y1] in self.walls(f, cell, depth, th) {
                rings.push((Poly::rect(x0, y0, x1, y1).map(t), Role::Wall));
            }
        }
        // c18: the corner diagonals.
        let (bl, br, tr, tl) = (t((r.u0, r.v0)), t((u1, r.v0)), t((u1, v1)), t((r.u0, v1)));
        let lines = match c.diagonals {
            Some(1) => vec![(bl, tr)],
            Some(2) => vec![(tl, br)],
            Some(_) => vec![(bl, tr), (tl, br)],
            None => Vec::new(),
        };
        let centre = t((r.u0 + r.w / 2.0, r.v0 + r.h / 2.0));
        Scene::new(&base, hole.as_ref(), rings, lines, centre, bl)
    }

    /// How the scene is drawn: fills, edge brushes, gradients.
    fn style(&self, f: &Frame) -> Style {
        let c = &self.cfg;
        let r = self.frame2(f);
        let r0 = r.v0.floor() as i64;
        let stripes = |s: f64, k: f64| {
            (
                XPat {
                    alpha: 1.0,
                    beta: -r.u0,
                    on: 1.0,
                    period: s,
                },
                r0,
                k,
            )
        };
        let mut st = Style {
            filled: c.filled,
            subs: c.subs.map(|s| s.to_vec()),
            ..Style::default()
        };
        if let Some(s) = c.h_stripes {
            st.row_stripes.push((r0, s));
        }
        if let Some(s) = c.grid {
            st.row_stripes.push((r0, s as i64));
            st.x_stripes.push(stripes(s, 0.0));
        }
        if let Some(s) = c.v_stripes {
            st.x_stripes.push(stripes(s, 0.0));
        }
        if let Some(s) = c.hatch {
            st.x_stripes.push(stripes(s, -1.0));
            st.x_stripes.push(stripes(s, 1.0));
        }
        if c.dash.is_some() || c.density.is_some() {
            let (on, period, phase) = c.dash.map_or((1.0, 1.0, 0.0), |(d, g, ph)| (d, d + g, ph));
            let n = c.density.unwrap_or([1.0; 4]);
            st.edge_dash = Some((on, n.map(|k| period * k).to_vec(), phase));
        }
        if let Some((a, z, d)) = c.vel {
            st.grads.push((
                Grad {
                    vel: Some((a, z)),
                    ..Grad::default()
                },
                GDir::of(d),
            ));
        }
        if let Some((a, z, d)) = c.gate {
            st.grads.push((
                Grad {
                    gate: Some((a, z)),
                    ..Grad::default()
                },
                GDir::of(d),
            ));
        }
        st
    }
}

impl Shapes for Rect {
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

fn rect_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    super::kernel(p, b, Rect::new(Cfg::read(p)))
}

#[cfg(test)]
mod tests {
    use super::super::super::param::{ParamSet, ParamValue};
    use super::super::super::rng::Rng;
    use super::super::super::{Ctx, SourceStats};
    use super::super::pen::{Counter, Pen};
    use super::*;

    /// The lower bound may refuse a run outright (past the counting cap),
    /// so it must never pass the exact count; the cheap bound never falls
    /// below it. Seeded random settings (functions on with probability ⅓,
    /// sizes and spacings drawn), small boxes.
    #[test]
    fn the_lower_bound_never_passes_the_count_and_the_bound_never_falls_below_it() {
        let spec = rect_spec();
        let mut rng = Rng::new(0x5345_3533);
        for draw in 0..400 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 400)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            p.set("gen.seed", ParamValue::Int(rng.range_i64(1, 1000)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            for (k, lo, hi) in [
                ("c06_border_thickness.size", 1, 6),
                ("c14_horizontal_stripe_fill.spacing", 2, 6),
                ("c15_vertical_stripe_fill.spacing", 2, 6),
                ("c16_crosshatch_fill.spacing", 2, 7),
                ("c17_grid_fill.spacing", 2, 6),
                ("c20_per_edge_density.bottom", 1, 4),
                ("c20_per_edge_density.right", 1, 4),
                ("c23_dashed_border_phase.dash", 1, 4),
                ("c23_dashed_border_phase.gap", 1, 4),
            ] {
                p.set(k, ParamValue::Int(rng.range_i64(lo, hi)));
            }
            p.set(
                "c09_rotation.angle",
                ParamValue::Float(rng.range_f64(-180.0, 180.0)),
            );
            p.set(
                "c04_outline_mode.placement",
                ParamValue::Choice(rng.below(3) as u32),
            );
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let r = Rect::new(Cfg::read(&params));
            let mut pen = Pen::new(Counter::default(), f.step, f.band());
            r.draw(&f, &mut pen);
            let n = pen.finish().notes;
            assert!(r.floor(&f) <= n, "draw {draw}: floor {} > {n}", r.floor(&f));
            assert!(r.bound(&f) >= n, "draw {draw}: bound {} < {n}", r.bound(&f));
        }
    }
}
