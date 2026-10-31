//! Tumours (V09-S50, contract §149, §150, §255): an original Experimental
//! tool that grows organic, tumour-like masses out of the selected note art
//! — exactly the contract's 25 functions (catalog ids `c01_…` … `c25_…`,
//! switch = id, controls `<id>.<name>`). Distinct from Spiderweb's
//! `tumour.rs` (bumps along a line): here the art is an area and the
//! growth is a field around it.
//!
//! Per track slot and evolution stage (progress t, 1 without c25):
//!
//! 1. **Art** — the shared [`Grid`] (a cell of Resolution ticks × one
//!    row, padded by the reach the settings can give, at most
//!    [`MAX_CELLS`] cells: a huge span coarsens the cell).
//! 2. **Seeds** — every art cell, or c14's Contour / Dense regions /
//!    Anchors / Random sample / Note starts; each seed has an art note.
//! 3. **Field** — the exact Euclidean distance d and nearest seed of every
//!    cell ([`feature_transform`]; a time cell is `ry / rx` row units), and
//!    the base signed field sd₀ = d(q) − R(q) with R = the radius (3 rows
//!    and 3 cells, or c01's) × Growth scale × t × the modulators at the
//!    nearest seed: c02 lobes ((1 + cos k(θ − θ₀)) / 2)^s about the seed
//!    component's centroid, c03 uneven lobes and a seeded one-sided skew,
//!    c15 the windowed note density, c16 the windowed fill fraction
//!    (convex / concave), c17 / c18 graphs over the art's time / pitch
//!    span, c23 attractor / repulsor Gaussians; q = p except under c04,
//!    whose domain warp pulls the field towards the density peaks (the
//!    growth swells around dense regions).
//! 4. **Notes** — the growth cells outside the art (so nothing overlaps
//!    it) as runs per row broken where the attributes change (or one note
//!    per cell), each from its nearest seed's art note; the art is kept or
//!    dropped.
//!
//! **Preflight (§255)**: a loose bound (grid cells × stages + the kept
//! art) is used while it is under the warning size; above it the output is
//! counted exactly from the plan, which the run that follows reuses.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};
use std::sync::Mutex;

use super::super::param::*;
use super::super::preflight::{Estimate, Growth};
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::shared::seeded_stream::{salt_of, stream};
use super::super::spatial::{Grid, feature_transform};
use super::super::sweep::note_places;
use super::super::{Ctx, Kernel, Limits, Output, Source, SourceTrack, TNote, TransformError};
use super::xgeom::sub_stepped;
use super::xgrid::{by_slot, span_of};
use super::{NOTE_VALUES, boxed, note_value};

pub const TUMOURS: ToolDef = ToolDef {
    id: "tumours",
    spec: tumours_spec,
    build: tumours_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_growth_radius", "Growth Radius", 10),
    ("c02_lobe_count", "Lobe Count", 20),
    ("c03_lobe_asymmetry", "Lobe Asymmetry", 30),
    ("c04_bulge_pressure", "Bulge Pressure", 40),
    ("c05_branch_probability", "Branch Probability", 50),
    ("c06_branch_depth", "Branch Depth", 60),
    ("c07_nodule_frequency", "Nodule Frequency", 70),
    ("c08_nodule_size_range", "Nodule Size Range", 80),
    ("c09_edge_roughness", "Edge Roughness", 90),
    ("c10_vein_density", "Vein Density", 100),
    ("c11_vein_direction_field", "Vein Direction Field", 110),
    (
        "c12_cavity_void_generation",
        "Cavity / Void Generation",
        120,
    ),
    ("c13_fusion", "Fusion", 130),
    ("c14_growth_seed_mode", "Growth Seed Mode", 140),
    (
        "c15_density_reactive_growth",
        "Density-Reactive Growth",
        150,
    ),
    (
        "c16_curvature_reactive_growth",
        "Curvature-Reactive Growth",
        160,
    ),
    ("c17_time_biased_growth", "Time-Biased Growth", 170),
    ("c18_pitch_biased_growth", "Pitch-Biased Growth", 180),
    ("c19_subchannel_mutation", "Subchannel Mutation", 190),
    ("c20_velocity_mutation", "Velocity Mutation", 200),
    ("c21_gate_mutation", "Gate Mutation", 210),
    ("c22_daughter_growth", "Daughter Growth", 220),
    (
        "c23_attractor_repulsor_field",
        "Attractor / Repulsor Field",
        230,
    ),
    (
        "c24_mask_boundary_constraint",
        "Mask / Boundary Constraint",
        240,
    ),
    ("c25_evolution_morph", "Evolution Morph", 250),
];

/// Most cells of one slot's grid (2 M: ~50 MB of working fields).
pub const MAX_CELLS: usize = 2 << 20;
/// Most padding cells on each side of the art.
const PAD_CAP: f64 = 8192.0;
/// The base radius (rows and time cells) without c01.
const BASE_RADIUS: f64 = 3.0;

const FORMS: &[&str] = &[
    "Runs (one note per run of cells)",
    "Cells (one note per cell)",
];
pub const SEED_MODES: &[&str] = &[
    "All notes",
    "Contour",
    "Dense regions",
    "Anchors",
    "Random sample",
    "Note starts",
];
const VEIN_STYLES: &[&str] = &["Accent", "Carve (gaps)", "Veins only"];
pub const VEIN_FIELDS: &[&str] = &[
    "Radial (outward)",
    "Along time",
    "Along pitch",
    "Fixed angle",
    "Swirl",
    "Flow noise",
    "Toward attractors",
];
const CAVITY_MODES: &[&str] = &["Round cavities", "Noise voids"];
const FUSION_MODES: &[&str] = &["Bridge (closing)", "Melt (smooth union)"];
const SUB_BY: &[&str] = &["Generation", "Structure kind", "Generation and kind"];
pub const MASK_MODES: &[&str] = &[
    "Selection box",
    "Region",
    "Track notes",
    "Picture",
    "Key range",
];
const STAGE_OUTPUTS: &[&str] = &[
    "One stage (at the position)",
    "Stages in sequence along time",
    "Growth rings (each stage's new cells)",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const TUMOURS_HELP: &[(&str, &str)] = &[
    (
        "res",
        "The length of one growth cell along time, as a note value; each cell is one key high, and a very long selection coarsens the cell to stay within the working size.",
    ),
    (
        "res.ticks",
        "For Custom: the length of one growth cell in ticks.",
    ),
    (
        "scale",
        "Multiplies every growth radius (0 % = no growth, 100 % = as set, up to 400 %).",
    ),
    (
        "ring",
        "How many rows of distance from the art make one generation: generation 1 touches the art, the next ring out is generation 2 … (used by the mutations).",
    ),
    (
        "form",
        "Runs: one note for each run of growth cells in a row (broken where velocity, gate, channel or subchannel change); Cells: one note per cell.",
    ),
    (
        "keep",
        "Keeps the selected art in the result; off outputs only the growth around it.",
    ),
    (
        "c01_growth_radius",
        "Sets how far the growth reaches from the art, across pitch and along time separately (3 rows and 3 cells without it).",
    ),
    (
        "c01_growth_radius.time",
        "How many time cells the growth reaches from the art along time.",
    ),
    (
        "c02_lobe_count",
        "Shapes each mass into lobes: bulges spaced evenly around its center.",
    ),
    (
        "c02_lobe_count.amount",
        "How much the lobes stick out beyond the radius (0 % = round, 100 % = up to twice the radius at a lobe's tip).",
    ),
    (
        "c02_lobe_count.angle",
        "Turns the lobes around the mass's center.",
    ),
    (
        "c02_lobe_count.sharpness",
        "How pointed the lobes are (higher = narrower lobes with deeper gaps between them).",
    ),
    (
        "c03_lobe_asymmetry",
        "Makes the lobes uneven (seeded): each gets its own size, place and width, and the mass leans to one side.",
    ),
    (
        "c03_lobe_asymmetry.skew",
        "How strongly the whole mass bulges toward one seeded side (0 % = centered).",
    ),
    (
        "c04_bulge_pressure",
        "Swells the growth around the art's densest regions, as if pushed out from inside.",
    ),
    (
        "c04_bulge_pressure.exponent",
        "How sharply the pressure follows density (above 1 only the densest regions push hard).",
    ),
    (
        "c04_bulge_pressure.window",
        "The size of the square, in cells and rows, over which density is counted.",
    ),
    (
        "c04_bulge_pressure.reach",
        "How far, in rows, a density peak's push reaches before fading out.",
    ),
    (
        "c04_bulge_pressure.peaks",
        "At most this many density peaks push the growth (the densest first).",
    ),
    (
        "c05_branch_probability",
        "Grows tapered branches out of the growth's contour, pointing outward (seeded).",
    ),
    (
        "c05_branch_probability.spacing",
        "How many contour cells one branch site covers; the chance applies once per site.",
    ),
    (
        "c05_branch_probability.length",
        "How long each branch grows, in rows.",
    ),
    (
        "c05_branch_probability.width",
        "How thick each branch is at its base, in rows.",
    ),
    (
        "c05_branch_probability.taper",
        "How much a branch thins toward its tip (0 % = even thickness, 100 % = to a point).",
    ),
    (
        "c05_branch_probability.wiggle",
        "How much a branch may turn at each step (0° = straight).",
    ),
    (
        "c05_branch_probability.spread",
        "How far a branch's direction may lean away from the contour's outward normal.",
    ),
    (
        "c06_branch_depth",
        "Lets branches fork: every branch end grows children, each generation shorter and thinner.",
    ),
    (
        "c06_branch_depth.children",
        "How many children grow from each fork.",
    ),
    (
        "c06_branch_depth.fork",
        "The angle between a child and its parent's direction.",
    ),
    (
        "c06_branch_depth.decay",
        "Each generation's length and width as a share of its parent's (65 % = about two thirds).",
    ),
    (
        "c07_nodule_frequency",
        "Attaches small round clusters (nodules) to the growth's contour (seeded).",
    ),
    (
        "c07_nodule_frequency.cluster",
        "How many discs make up one nodule cluster.",
    ),
    (
        "c07_nodule_frequency.offset",
        "How far outward from the contour a nodule sits, as a share of its radius (0 % = centered on the edge).",
    ),
    (
        "c08_nodule_size_range",
        "Sets how large nodules may be; each one's radius is drawn between the smallest and the largest.",
    ),
    (
        "c08_nodule_size_range.max",
        "The largest nodule radius, in rows.",
    ),
    (
        "c08_nodule_size_range.bias",
        "Leans the drawn sizes: above 1 favors small nodules, below 1 large ones.",
    ),
    (
        "c09_edge_roughness",
        "Roughens the growth's edge with seeded fractal noise; the inside stays solid.",
    ),
    (
        "c09_edge_roughness.scale",
        "The size of the roughness features, in rows (small = fine grain, large = broad bumps).",
    ),
    (
        "c09_edge_roughness.octaves",
        "How many layers of finer detail are added on top of the main roughness.",
    ),
    (
        "c10_vein_density",
        "Traces vein lines through the inside of the growth (seeded); they flow along the Vein Direction Field, or outward without it.",
    ),
    (
        "c10_vein_density.length",
        "How long each vein runs, in rows.",
    ),
    (
        "c10_vein_density.fork",
        "The chance at each step that a vein splits in two.",
    ),
    (
        "c10_vein_density.style",
        "Accent: veins become louder notes; Carve: veins are cut out as gaps; Veins Only: only the veins remain of the growth.",
    ),
    (
        "c10_vein_density.accent",
        "For Accent: the veins' velocity as a share of the note's (140 % = 40 % louder).",
    ),
    (
        "c11_vein_direction_field",
        "Chooses the direction the veins flow in.",
    ),
    (
        "c11_vein_direction_field.angle",
        "For Fixed Angle: the direction the veins follow (0° = along time, 90° = up in pitch).",
    ),
    (
        "c11_vein_direction_field.noise",
        "How much seeded flow noise bends the chosen direction (0 % = none).",
    ),
    (
        "c11_vein_direction_field.scale",
        "The size of the flow noise's swirls, in rows.",
    ),
    (
        "c12_cavity_void_generation",
        "Cuts holes inside the growth, always keeping a wall to its edge.",
    ),
    (
        "c12_cavity_void_generation.density",
        "For Round Cavities: how many holes, per 1,000 cells of the growth's body.",
    ),
    (
        "c12_cavity_void_generation.min",
        "For Round Cavities: the smallest hole radius, in rows.",
    ),
    (
        "c12_cavity_void_generation.max",
        "For Round Cavities: the largest hole radius, in rows.",
    ),
    (
        "c12_cavity_void_generation.wall",
        "The thinnest wall, in rows, left between any hole and the growth's edge.",
    ),
    (
        "c12_cavity_void_generation.threshold",
        "For Noise Voids: cells whose noise lies above this level become holes (higher = fewer, smaller voids).",
    ),
    (
        "c12_cavity_void_generation.scale",
        "For Noise Voids: the size of the noise features, in rows.",
    ),
    (
        "c13_fusion",
        "Merges masses that grow close to each other into one.",
    ),
    (
        "c13_fusion.mode",
        "Bridge closes the gaps between nearby masses exactly; Melt blends branches, nodules and daughters smoothly into the body.",
    ),
    (
        "c14_growth_seed_mode",
        "Chooses which parts of the art the growth starts from.",
    ),
    (
        "c14_growth_seed_mode.threshold",
        "For Dense Regions: a cell seeds growth when its surroundings are at least this full, relative to the fullest spot.",
    ),
    (
        "c14_growth_seed_mode.window",
        "For Dense Regions: the size of the square, in cells and rows, over which fullness is measured.",
    ),
    (
        "c14_growth_seed_mode.anchors",
        "For Anchors: pairs of positions in thousandths of the art's box (time, pitch; 0 = start / lowest, 1000 = end / highest); each pair seeds growth there.",
    ),
    (
        "c14_growth_seed_mode.count",
        "For Random Sample: how many art cells are picked as seeds.",
    ),
    (
        "c15_density_reactive_growth",
        "Grows more where the art is dense and less where it is sparse (or the reverse with a negative gain).",
    ),
    (
        "c15_density_reactive_growth.window",
        "The size of the square, in cells and rows, over which local density is counted.",
    ),
    (
        "c16_curvature_reactive_growth",
        "Grows by the art's shape: more (or less) at convex tips and in concave bays.",
    ),
    (
        "c16_curvature_reactive_growth.concave",
        "How much more growth in concave bays (negative = less: bays fill in less).",
    ),
    (
        "c16_curvature_reactive_growth.window",
        "The size of the square, in cells and rows, over which the art's shape is measured.",
    ),
    (
        "c17_time_biased_growth",
        "Changes the growth's size across time, following a graph over the art's time span.",
    ),
    (
        "c17_time_biased_growth.from",
        "The radius share where the graph is 0 (100 % = unchanged).",
    ),
    (
        "c17_time_biased_growth.to",
        "The radius share where the graph is 1 (100 % = unchanged).",
    ),
    (
        "c18_pitch_biased_growth",
        "Changes the growth's size across pitch, following a graph from the art's lowest key to its highest.",
    ),
    (
        "c18_pitch_biased_growth.from",
        "The radius share where the graph is 0 (100 % = unchanged).",
    ),
    (
        "c18_pitch_biased_growth.to",
        "The radius share where the graph is 1 (100 % = unchanged).",
    ),
    (
        "c19_subchannel_mutation",
        "Moves the growth's notes to other subchannels by generation and / or structure, so the layers can be colored apart.",
    ),
    (
        "c19_subchannel_mutation.by",
        "What counts the steps: the generation (rings out from the art), the structure kind (base, branch, nodule, daughter, vein), or both added.",
    ),
    (
        "c19_subchannel_mutation.chance",
        "The seeded share of generations that jump a further Jump Size subchannels up or down.",
    ),
    (
        "c19_subchannel_mutation.spread",
        "How many extra subchannels a seeded jump moves.",
    ),
    (
        "c20_velocity_mutation",
        "Changes velocity with each generation outward from the art.",
    ),
    (
        "c20_velocity_mutation.drift",
        "A seeded random change of up to this share per structure and generation (0 % = none).",
    ),
    (
        "c20_velocity_mutation.floor",
        "No growth note gets a velocity below this.",
    ),
    (
        "c21_gate_mutation",
        "Shortens the growth's notes with each generation outward from the art.",
    ),
    (
        "c21_gate_mutation.decay",
        "Each generation's gate as a share of the previous one's.",
    ),
    (
        "c21_gate_mutation.floor",
        "No growth note is shortened below this share of its cell.",
    ),
    (
        "c22_daughter_growth",
        "Adds smaller secondary masses beside the main growth (seeded), joined to it by stalks or set apart.",
    ),
    (
        "c22_daughter_growth.distance",
        "The gap, in rows, between the main body and each daughter.",
    ),
    (
        "c22_daughter_growth.scale",
        "Each daughter's size as a share of the growth radius.",
    ),
    (
        "c22_daughter_growth.levels",
        "1: daughters only; 2 – 3: daughters grow daughters of their own.",
    ),
    (
        "c22_daughter_growth.stalk",
        "The width, in rows, of the stalk joining a daughter to its parent (0 = detached).",
    ),
    (
        "c23_attractor_repulsor_field",
        "Places points that pull the growth toward them or push it away.",
    ),
    (
        "c23_attractor_repulsor_field.radius",
        "How far a point's influence reaches, as a share of the art's box diagonal.",
    ),
    (
        "c23_attractor_repulsor_field.bias",
        "How strongly branches, veins and daughters turn toward the attractors (and away from repulsors).",
    ),
    (
        "c24_mask_boundary_constraint",
        "Lets the growth appear only inside a mask; the art itself is never cut.",
    ),
    (
        "c24_mask_boundary_constraint.margin",
        "For Selection Box and Picture: how far the box reaches beyond the selection, as a share of its size.",
    ),
    (
        "c24_mask_boundary_constraint.t0",
        "For Region: where the region starts along time, as a share of the art's time span.",
    ),
    (
        "c24_mask_boundary_constraint.t1",
        "For Region: where the region ends along time, as a share of the art's time span.",
    ),
    (
        "c24_mask_boundary_constraint.p0",
        "For Region: the region's lowest pitch, as a share of the art's pitch range.",
    ),
    (
        "c24_mask_boundary_constraint.p1",
        "For Region: the region's highest pitch, as a share of the art's pitch range.",
    ),
    (
        "c24_mask_boundary_constraint.tracks",
        "For Track Notes: the tracks (by number) whose notes form the mask; empty uses the track after the edited one.",
    ),
    (
        "c24_mask_boundary_constraint.dilate",
        "For Track Notes: widens the mask notes by this many cells and rows on every side.",
    ),
    (
        "c24_mask_boundary_constraint.picture",
        "For Picture: the image file whose bright areas, stretched over the box, allow growth.",
    ),
    (
        "c24_mask_boundary_constraint.threshold",
        "For Picture: the brightness a pixel needs for growth to be allowed there.",
    ),
    (
        "c24_mask_boundary_constraint.lo",
        "For Key Range: the lowest key growth may reach.",
    ),
    (
        "c24_mask_boundary_constraint.hi",
        "For Key Range: the highest key growth may reach.",
    ),
    (
        "c24_mask_boundary_constraint.invert",
        "Swaps inside and outside: growth only where the mask is not.",
    ),
    (
        "c25_evolution_morph",
        "Shows the growth part way through its development, or several stages of it.",
    ),
    (
        "c25_evolution_morph.stages",
        "How many stages the development is divided into.",
    ),
    (
        "c25_evolution_morph.output",
        "One Stage: the growth at Position; Stages in Sequence: each stage one after another along time; Growth Rings: each stage's new cells, layered in one place.",
    ),
    (
        "c25_evolution_morph.position",
        "For One Stage: how far through its development the growth is shown (0 % = start, 100 % = fully grown).",
    ),
    (
        "c25_evolution_morph.gap",
        "For Stages in Sequence: the silence, in ticks, between one stage and the next.",
    ),
];

fn tumours_spec() -> ToolSpec {
    let mut p = vec![
        choice("res", "Resolution (one cell)", NOTE_VALUES, 5),
        ticks("res.ticks", "Custom cell (ticks)", 1, 1 << 24, 120),
        float("scale", "Growth scale", 0.0, 400.0, 100.0, "%"),
        float("ring", "Generation ring (rows)", 0.5, 64.0, 2.0, ""),
        choice("form", "Notes", FORMS, 0),
        flag("keep", "Keep the source art", true),
        seed("seed"),
        // c01
        flag("c01_growth_radius", "Growth Radius", false),
        float("c01_growth_radius.rows", "Radius across pitch (rows)", 0.0, 64.0, 5.0, "")
            .help("How far the growth expands from the art: rows across pitch and cells along time (the field's metric is stretched so both radii are the same distance)."),
        float("c01_growth_radius.time", "Radius along time (cells)", 0.25, 1024.0, 5.0, ""),
        // c02
        flag("c02_lobe_count", "Lobe Count", false),
        int("c02_lobe_count.count", "Lobes", 1, 32, 5)
            .help("Major bulges around each mass's center: the radius × (1 + Amount · ((1 + cos k(θ − θ₀)) / 2)^Sharpness)."),
        float("c02_lobe_count.amount", "Amount", 0.0, 400.0, 80.0, "%"),
        float("c02_lobe_count.angle", "Rotation", -180.0, 180.0, 0.0, "°"),
        float("c02_lobe_count.sharpness", "Sharpness", 0.5, 16.0, 2.0, ""),
        // c03
        flag("c03_lobe_asymmetry", "Lobe Asymmetry", false),
        float("c03_lobe_asymmetry.amount", "Unevenness (lobe sizes, places and widths)", 0.0, 100.0, 50.0, "%")
            .help("Seeded per mass: each lobe gets its own size, angle and width, and the whole mass bulges towards one side (the skew)."),
        float("c03_lobe_asymmetry.skew", "One-sided skew", 0.0, 100.0, 35.0, "%"),
        // c04
        flag("c04_bulge_pressure", "Bulge Pressure", false),
        float("c04_bulge_pressure.pressure", "Pressure", 0.0, 400.0, 100.0, "%")
            .help("Dense regions push the growth outwards: the field is pulled towards each density peak by 1 + Pressure · (density / the densest)^Exponent, fading with the reach."),
        float("c04_bulge_pressure.exponent", "Exponent", 0.25, 4.0, 2.0, ""),
        int("c04_bulge_pressure.window", "Density window (cells and rows)", 1, 64, 6),
        float("c04_bulge_pressure.reach", "Reach (rows)", 1.0, 128.0, 8.0, ""),
        int("c04_bulge_pressure.peaks", "Most peaks", 1, 64, 8),
        // c05
        flag("c05_branch_probability", "Branch Probability", false),
        float("c05_branch_probability.probability", "Chance per contour site", 0.0, 100.0, 20.0, "%")
            .help("Seeded: tapered branches grow out of the contour along its outward normal."),
        float("c05_branch_probability.spacing", "Site spacing (contour cells)", 1.0, 256.0, 6.0, ""),
        float("c05_branch_probability.length", "Length (rows)", 1.0, 256.0, 10.0, ""),
        float("c05_branch_probability.width", "Width (rows)", 0.5, 32.0, 1.5, ""),
        float("c05_branch_probability.taper", "Taper", 0.0, 100.0, 60.0, "%"),
        float("c05_branch_probability.wiggle", "Wiggle per step", 0.0, 90.0, 18.0, "°"),
        float("c05_branch_probability.spread", "Spread from the normal", 0.0, 90.0, 25.0, "°"),
        // c06
        flag("c06_branch_depth", "Branch Depth", false),
        int("c06_branch_depth.depth", "Generations", 1, 6, 3)
            .help("Recursive branches: every branch end forks into children, shorter and thinner by the decay each generation."),
        int("c06_branch_depth.children", "Children per fork", 1, 4, 2),
        float("c06_branch_depth.fork", "Fork angle", 5.0, 90.0, 35.0, "°"),
        float("c06_branch_depth.decay", "Length and width decay", 10.0, 100.0, 65.0, "%"),
        // c07
        flag("c07_nodule_frequency", "Nodule Frequency", false),
        float("c07_nodule_frequency.frequency", "Nodules per 100 contour cells", 0.0, 100.0, 6.0, "")
            .help("Seeded small clusters attached to the contour."),
        int("c07_nodule_frequency.cluster", "Discs per cluster", 1, 6, 3),
        float("c07_nodule_frequency.offset", "Outward offset", 0.0, 150.0, 60.0, "%"),
        // c08
        flag("c08_nodule_size_range", "Nodule Size Range", false),
        float("c08_nodule_size_range.min", "Smallest radius (rows)", 0.5, 32.0, 1.0, "")
            .help("Each nodule's radius is drawn between the smallest and the largest (Bias > 1 favours small ones)."),
        float("c08_nodule_size_range.max", "Largest radius (rows)", 0.5, 32.0, 3.0, ""),
        float("c08_nodule_size_range.bias", "Bias", 0.25, 4.0, 1.0, ""),
        // c09
        flag("c09_edge_roughness", "Edge Roughness", false),
        float("c09_edge_roughness.amount", "Amount (rows)", 0.0, 32.0, 1.5, "")
            .help("Seeded fractal noise added to the field: the contour becomes irregular, interior cells stay."),
        float("c09_edge_roughness.scale", "Feature size (rows)", 1.0, 256.0, 6.0, ""),
        int("c09_edge_roughness.octaves", "Octaves", 1, 6, 3),
        // c10
        flag("c10_vein_density", "Vein Density", false),
        float("c10_vein_density.density", "Veins per 1,000 body cells", 0.0, 100.0, 6.0, "")
            .help("Internal lines traced through the body along the vein direction field (c11; radial without it)."),
        float("c10_vein_density.length", "Vein length (rows)", 1.0, 512.0, 24.0, ""),
        float("c10_vein_density.fork", "Fork chance per step", 0.0, 50.0, 4.0, "%"),
        choice("c10_vein_density.style", "Veins", VEIN_STYLES, 0),
        float("c10_vein_density.accent", "Accent velocity", 0.0, 200.0, 140.0, "%"),
        // c11
        flag("c11_vein_direction_field", "Vein Direction Field", false),
        choice("c11_vein_direction_field.field", "Field", VEIN_FIELDS, 4)
            .help("Where the veins flow: out from the mass, along time or pitch, at an angle, around the mass, along seeded flow noise or toward the attractors (c23)."),
        float("c11_vein_direction_field.angle", "Angle", -180.0, 180.0, 0.0, "°"),
        float("c11_vein_direction_field.noise", "Flow noise mix", 0.0, 100.0, 20.0, "%"),
        float("c11_vein_direction_field.scale", "Noise feature size (rows)", 1.0, 256.0, 16.0, ""),
        // c12
        flag("c12_cavity_void_generation", "Cavity / Void Generation", false),
        choice("c12_cavity_void_generation.mode", "Cavities", CAVITY_MODES, 0)
            .help("Holes cut inside the body, never closer to its contour than the wall."),
        float("c12_cavity_void_generation.density", "Cavities per 1,000 body cells", 0.0, 100.0, 8.0, ""),
        float("c12_cavity_void_generation.min", "Smallest radius (rows)", 0.5, 32.0, 1.0, ""),
        float("c12_cavity_void_generation.max", "Largest radius (rows)", 0.5, 32.0, 2.5, ""),
        float("c12_cavity_void_generation.wall", "Wall (rows)", 0.0, 32.0, 1.0, ""),
        float("c12_cavity_void_generation.threshold", "Noise threshold", 0.0, 100.0, 60.0, "%"),
        float("c12_cavity_void_generation.scale", "Noise feature size (rows)", 1.0, 256.0, 5.0, ""),
        // c13
        flag("c13_fusion", "Fusion", false),
        float("c13_fusion.distance", "Fusion distance (rows)", 0.5, 64.0, 3.0, "")
            .help("Nearby masses merge: Bridge closes gaps narrower than twice the distance (exactly, by distance fields); Melt blends the branches, nodules and daughters into the body (a smooth union)."),
        choice("c13_fusion.mode", "Fusion", FUSION_MODES, 0),
        // c14
        flag("c14_growth_seed_mode", "Growth Seed Mode", false),
        choice("c14_growth_seed_mode.mode", "Grow from", SEED_MODES, 2)
            .help("Where the growth starts: every art cell, the art's outer contour (holes and enclosed gaps stay as they are), its dense regions, anchor points, a seeded sample of its cells or the notes' starts."),
        float("c14_growth_seed_mode.threshold", "Dense: fill at least (of the fullest)", 0.0, 100.0, 60.0, "%"),
        int("c14_growth_seed_mode.window", "Dense: window (cells and rows)", 1, 64, 4),
        list("c14_growth_seed_mode.anchors", "Anchors (time ‰, pitch ‰ of the art's box)", -1000, 2000, 16, &[250, 500, 750, 500]),
        int("c14_growth_seed_mode.count", "Random sample: seeds", 1, 65_536, 24),
        // c15
        flag("c15_density_reactive_growth", "Density-Reactive Growth", false),
        float("c15_density_reactive_growth.gain", "Gain (negative: sparse grows more)", -100.0, 400.0, 100.0, "%")
            .help("The radius × (1 + Gain · (local density / the mean − 1)), at most 4×; density = notes per cell in the window around the nearest seed."),
        int("c15_density_reactive_growth.window", "Window (cells and rows)", 1, 64, 6),
        // c16
        flag("c16_curvature_reactive_growth", "Curvature-Reactive Growth", false),
        float("c16_curvature_reactive_growth.convex", "Convex gain (tips)", -100.0, 400.0, 150.0, "%")
            .help("By the art's fill in the window around the nearest seed against a straight edge's: less is a convex tip (a lone cell: −1), more a concave bay (filled: +1); the radius × (1 + Convex · tip + Concave · bay)."),
        float("c16_curvature_reactive_growth.concave", "Concave gain (bays)", -100.0, 400.0, -60.0, "%"),
        int("c16_curvature_reactive_growth.window", "Window (cells and rows)", 1, 32, 3),
        // c17
        flag("c17_time_biased_growth", "Time-Biased Growth", false),
        curve("c17_time_biased_growth.graph", "Strength over time", Curve::line(0.0, 1.0))
            .help("The radius × the graph over the art's time span (0 → From, 1 → To)."),
        float("c17_time_biased_growth.from", "From", 0.0, 400.0, 20.0, "%"),
        float("c17_time_biased_growth.to", "To", 0.0, 400.0, 200.0, "%"),
        // c18
        flag("c18_pitch_biased_growth", "Pitch-Biased Growth", false),
        curve("c18_pitch_biased_growth.graph", "Strength over pitch", Curve::line(0.0, 1.0))
            .help("The radius × the graph over the art's pitch span, low to high (0 → From, 1 → To)."),
        float("c18_pitch_biased_growth.from", "From", 0.0, 400.0, 20.0, "%"),
        float("c18_pitch_biased_growth.to", "To", 0.0, 400.0, 200.0, "%"),
        // c19
        flag("c19_subchannel_mutation", "Subchannel Mutation", false),
        int("c19_subchannel_mutation.step", "Subchannel step", -64, 64, 1)
            .help("Generated cells move by Step subchannels per generation and / or structure kind (base, branch, nodule, daughter, vein); a seeded share jumps further."),
        choice("c19_subchannel_mutation.by", "By", SUB_BY, 0),
        float("c19_subchannel_mutation.chance", "Seeded jump chance", 0.0, 100.0, 0.0, "%"),
        int("c19_subchannel_mutation.spread", "Jump size", 1, 16, 2),
        // c20
        flag("c20_velocity_mutation", "Velocity Mutation", false),
        float("c20_velocity_mutation.decay", "Per generation", 10.0, 200.0, 85.0, "%")
            .help("Velocity × Per generation^(generation − 1), then a seeded drift per structure."),
        float("c20_velocity_mutation.drift", "Seeded drift per structure", 0.0, 100.0, 15.0, "%"),
        int("c20_velocity_mutation.floor", "Lowest velocity", 1, 127, 8),
        // c21
        flag("c21_gate_mutation", "Gate Mutation", false),
        float("c21_gate_mutation.start", "Gate of the first generation", 5.0, 100.0, 100.0, "%")
            .help("Each note's length × Start · Decay^(generation − 1), never under the floor."),
        float("c21_gate_mutation.decay", "Per generation", 10.0, 100.0, 80.0, "%"),
        float("c21_gate_mutation.floor", "Shortest gate", 1.0, 100.0, 10.0, "%"),
        // c22
        flag("c22_daughter_growth", "Daughter Growth", false),
        int("c22_daughter_growth.count", "Daughters", 1, 64, 3)
            .help("Seeded secondary masses beside the primary growth (with their own daughters for more levels), joined by stalks or detached."),
        float("c22_daughter_growth.distance", "Gap from the body (rows)", 0.0, 64.0, 4.0, ""),
        float("c22_daughter_growth.scale", "Size (of the growth radius)", 5.0, 200.0, 60.0, "%"),
        int("c22_daughter_growth.levels", "Levels", 1, 3, 1),
        float("c22_daughter_growth.stalk", "Stalk width (rows, 0: detached)", 0.0, 16.0, 0.75, ""),
        // c23
        flag("c23_attractor_repulsor_field", "Attractor / Repulsor Field", false),
        list("c23_attractor_repulsor_field.points", "Points (time ‰, pitch ‰ of the art's box, strength %)", -4000, 4000, 24, &[800, 800, 100])
            .help("Positions over the art's box: a positive strength attracts (more growth nearby, branches, veins and daughters turn toward it), a negative one repels."),
        float("c23_attractor_repulsor_field.radius", "Influence radius (of the box)", 1.0, 200.0, 40.0, "%"),
        float("c23_attractor_repulsor_field.bias", "Direction bias", 0.0, 100.0, 50.0, "%"),
        // c24
        flag("c24_mask_boundary_constraint", "Mask / Boundary Constraint", false),
        choice("c24_mask_boundary_constraint.mode", "Restrict to", MASK_MODES, 0)
            .help("Growth only inside the mask (the art itself is never cut)."),
        float("c24_mask_boundary_constraint.margin", "Box margin", 0.0, 200.0, 0.0, "%"),
        float("c24_mask_boundary_constraint.t0", "Region: time from", -100.0, 200.0, 0.0, "%"),
        float("c24_mask_boundary_constraint.t1", "Region: time to", -100.0, 200.0, 60.0, "%"),
        float("c24_mask_boundary_constraint.p0", "Region: pitch from", -100.0, 200.0, 0.0, "%"),
        float("c24_mask_boundary_constraint.p1", "Region: pitch to", -100.0, 200.0, 100.0, "%"),
        list("c24_mask_boundary_constraint.tracks", "Mask tracks (numbers; empty: the next one)", 1, 4096, 16, &[]),
        int("c24_mask_boundary_constraint.dilate", "Track mask: widen (cells and rows)", 0, 64, 2),
        text("c24_mask_boundary_constraint.picture", "Mask picture", 1024, ""),
        float("c24_mask_boundary_constraint.threshold", "Picture: brightness at least", 0.0, 100.0, 50.0, "%"),
        pitch("c24_mask_boundary_constraint.lo", "Key range: lowest", 36),
        pitch("c24_mask_boundary_constraint.hi", "Key range: highest", 84),
        flag("c24_mask_boundary_constraint.invert", "Invert the mask", false),
        // c25
        flag("c25_evolution_morph", "Evolution Morph", false),
        curve("c25_evolution_morph.graph", "Evolution (progress per stage)", Curve::smooth(vec![(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]))
            .help("The growth through its stages: the graph maps stage position (0 … 1) to growth progress (radius, branch length and generations, nodules, daughters, veins, cavities)."),
        int("c25_evolution_morph.stages", "Stages", 1, 16, 4),
        choice("c25_evolution_morph.output", "Output", STAGE_OUTPUTS, 1),
        float("c25_evolution_morph.position", "One stage: position", 0.0, 100.0, 50.0, "%"),
        ticks("c25_evolution_morph.gap", "Sequence: gap between stages", 0, 1 << 40, 0),
    ];
    with_help(&mut p, TUMOURS_HELP);
    ToolSpec {
        id: "tumours",
        name: "Tumours",
        family: Family::Experimental,
        summary: "Grows organic, tumour-like masses out of the selected note art — radius, lobes, bulges, branches, nodules, rough edges, veins, cavities, fusion, mutation, daughters, attractors, masks and an evolution graph.",
        params: p,
        advanced: Vec::new(),
        functions: FUNCTIONS
            .iter()
            .map(|&(id, name, order)| FunctionSpec { id, name, order })
            .collect(),
        needs_selection: true,
        context: false,
    }
}

// ================================================================ settings

#[derive(Clone, Debug)]
struct Lobes {
    k: u32,
    amount: f64,
    angle: f64,
    sharp: f64,
}

#[derive(Clone, Debug)]
struct Asym {
    amount: f64,
    skew: f64,
}

#[derive(Clone, Debug)]
struct Bulge {
    pressure: f64,
    gamma: f64,
    window: usize,
    reach: f64,
    peaks: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum Seeds {
    All,
    Contour,
    Dense { fill: f64, window: usize },
    Anchors(Vec<(f64, f64)>),
    Random(usize),
    Starts,
}

#[derive(Clone, Debug)]
struct Bias {
    graph: Curve,
    from: f64,
    to: f64,
}

#[derive(Clone, Debug)]
struct Attract {
    /// (time, pitch) as box fractions and strength (−4 … 4).
    pts: Vec<(f64, f64, f64)>,
    /// Influence radius as a fraction of the box's diagonal.
    radius: f64,
    /// How much branches, veins and daughters turn toward the field (0 – 1).
    bias: f64,
}

/// c05 / c06: branches.
#[derive(Clone, Debug)]
struct Branch {
    /// Chance per contour cell (the site chance over the spacing).
    p: f64,
    /// Contour cells per site (a swept site chance is divided by it).
    spacing: f64,
    /// c05 is on: its parameters and Control Sweeps apply (c06 alone uses
    /// c05's declared defaults and no sweep of them).
    own: bool,
    length: f64,
    width: f64,
    taper: f64,
    wiggle: f64,
    spread: f64,
    depth: u32,
    children: u32,
    fork: f64,
    decay: f64,
}

/// c07 / c08: nodule clusters.
#[derive(Clone, Debug)]
struct Nodules {
    /// Chance per contour cell.
    p: f64,
    /// c07 is on: its parameters and Control Sweeps apply (c08 alone uses
    /// c07's declared defaults and no sweep of them).
    own: bool,
    cluster: u32,
    offset: f64,
    min: f64,
    max: f64,
    bias: f64,
}

/// c09: fractal noise on the field.
#[derive(Clone, Debug)]
struct Rough {
    amount: f64,
    scale: f64,
    octaves: u32,
}

/// c12: cavities.
#[derive(Clone, Debug)]
struct Cavity {
    noise: bool,
    /// Chance per body cell.
    p: f64,
    min: f64,
    max: f64,
    wall: f64,
    threshold: f64,
    scale: f64,
}

/// c22: daughter masses.
#[derive(Clone, Debug)]
struct Daughters {
    count: usize,
    gap: f64,
    scale: f64,
    levels: u32,
    stalk: f64,
}

/// c10 / c11: veins.
#[derive(Clone, Debug)]
struct Veins {
    /// Chance per body cell.
    p: f64,
    length: f64,
    fork: f64,
    /// 0 Accent, 1 Carve, 2 Veins only.
    style: usize,
    accent: f64,
    /// The direction field (VEIN_FIELDS), its angle, flow-noise mix and size.
    field: usize,
    angle: f64,
    noise: f64,
    scale: f64,
}

/// c19 / c20 / c21: mutation by generation.
#[derive(Clone, Debug)]
struct Mutation {
    sub: Option<(i64, usize, f64, i64)>,
    vel: Option<(f64, f64, i64)>,
    gate: Option<(f64, f64, f64)>,
}

/// c25: evolution stages.
#[derive(Clone, Debug)]
struct Evo {
    graph: Curve,
    stages: usize,
    output: usize,
    position: f64,
    gap: u64,
}

/// c24: where growth may lie.
#[derive(Clone, Debug)]
struct Mask {
    mode: usize,
    margin: f64,
    region: (f64, f64, f64, f64),
    dilate: usize,
    threshold: f64,
    keys: (u8, u8),
    invert: bool,
}

/// Everything the build reads.
#[derive(Clone, Debug)]
struct Cfg {
    cell: u64,
    keep: bool,
    cells: bool,
    ring: f64,
    scale: f64,
    seed: u64,
    /// Radius across pitch (rows) and along time (cells).
    ry: f64,
    rx: f64,
    lobes: Option<Lobes>,
    asym: Option<Asym>,
    bulge: Option<Bulge>,
    seeds: Seeds,
    density: Option<(f64, usize)>,
    curv: Option<(f64, f64, usize)>,
    tbias: Option<Bias>,
    pbias: Option<Bias>,
    attract: Option<Attract>,
    branch: Option<Branch>,
    nodules: Option<Nodules>,
    rough: Option<Rough>,
    cavity: Option<Cavity>,
    /// c13: distance (rows) and whether it melts (else bridges).
    fusion: Option<(f64, bool)>,
    daughters: Option<Daughters>,
    mask: Option<Mask>,
    veins: Option<Veins>,
    mutation: Mutation,
    evo: Option<Evo>,
}

impl Cfg {
    fn read(p: &Params, ctx: &Ctx) -> Cfg {
        let on = |id: &str| p.flag(id);
        let pc = |k: &str| p.float(k) / 100.0;
        // A parameter of function `f`: its value while `f` is on, else its
        // declared default (a function that is off changes nothing).
        let of = |f: &str, k: &str| -> f64 {
            if on(f) {
                return p.float(k);
            }
            match p.specs.iter().find(|s| s.key == k).map(|s| &s.default) {
                Some(ParamValue::Float(v)) => *v,
                Some(ParamValue::Int(v)) => *v as f64,
                _ => 0.0,
            }
        };
        let (ry, rx) = if on("c01_growth_radius") {
            (
                p.float("c01_growth_radius.rows").clamp(0.0, 64.0),
                p.float("c01_growth_radius.time").clamp(0.25, 1024.0),
            )
        } else {
            (BASE_RADIUS, BASE_RADIUS)
        };
        let pairs = |k: &str, per: usize| -> Vec<Vec<f64>> {
            p.list(k)
                .chunks(per)
                .filter(|c| c.len() == per)
                .map(|c| c.iter().map(|&v| v as f64 / 1000.0).collect())
                .collect()
        };
        let bias = |id: &str| {
            on(id).then(|| Bias {
                graph: p.curve(&format!("{id}.graph")).sanitized(),
                from: pc(&format!("{id}.from")).max(0.0),
                to: pc(&format!("{id}.to")).max(0.0),
            })
        };
        Cfg {
            cell: note_value(p.choice("res"), p.ticks("res.ticks"), ctx).max(1),
            keep: p.flag("keep"),
            cells: p.choice("form") == 1,
            ring: p.float("ring").clamp(0.5, 64.0),
            scale: pc("scale").clamp(0.0, 4.0),
            seed: p.seed("seed"),
            ry,
            rx,
            lobes: on("c02_lobe_count").then(|| Lobes {
                k: p.int("c02_lobe_count.count").clamp(1, 32) as u32,
                amount: pc("c02_lobe_count.amount").clamp(0.0, 4.0),
                angle: p.float("c02_lobe_count.angle").to_radians(),
                sharp: p.float("c02_lobe_count.sharpness").clamp(0.5, 16.0),
            }),
            asym: on("c03_lobe_asymmetry").then(|| Asym {
                amount: pc("c03_lobe_asymmetry.amount").clamp(0.0, 1.0),
                skew: pc("c03_lobe_asymmetry.skew").clamp(0.0, 1.0),
            }),
            bulge: on("c04_bulge_pressure").then(|| Bulge {
                pressure: pc("c04_bulge_pressure.pressure").clamp(0.0, 4.0),
                gamma: p.float("c04_bulge_pressure.exponent").clamp(0.25, 4.0),
                window: p.int("c04_bulge_pressure.window").clamp(1, 64) as usize,
                reach: p.float("c04_bulge_pressure.reach").clamp(1.0, 128.0),
                peaks: p.int("c04_bulge_pressure.peaks").clamp(1, 64) as usize,
            }),
            seeds: if on("c14_growth_seed_mode") {
                match p.choice("c14_growth_seed_mode.mode") {
                    0 => Seeds::All,
                    1 => Seeds::Contour,
                    2 => Seeds::Dense {
                        fill: pc("c14_growth_seed_mode.threshold").clamp(0.0, 1.0),
                        window: p.int("c14_growth_seed_mode.window").clamp(1, 64) as usize,
                    },
                    3 => Seeds::Anchors(
                        pairs("c14_growth_seed_mode.anchors", 2)
                            .into_iter()
                            .map(|v| (v[0], v[1]))
                            .collect(),
                    ),
                    4 => {
                        Seeds::Random(p.int("c14_growth_seed_mode.count").clamp(1, 65_536) as usize)
                    }
                    _ => Seeds::Starts,
                }
            } else {
                Seeds::All
            },
            density: on("c15_density_reactive_growth").then(|| {
                (
                    pc("c15_density_reactive_growth.gain").clamp(-1.0, 4.0),
                    p.int("c15_density_reactive_growth.window").clamp(1, 64) as usize,
                )
            }),
            curv: on("c16_curvature_reactive_growth").then(|| {
                (
                    pc("c16_curvature_reactive_growth.convex").clamp(-1.0, 4.0),
                    pc("c16_curvature_reactive_growth.concave").clamp(-1.0, 4.0),
                    p.int("c16_curvature_reactive_growth.window").clamp(1, 32) as usize,
                )
            }),
            tbias: bias("c17_time_biased_growth"),
            pbias: bias("c18_pitch_biased_growth"),
            attract: on("c23_attractor_repulsor_field").then(|| Attract {
                pts: pairs("c23_attractor_repulsor_field.points", 3)
                    .into_iter()
                    .take(8)
                    // Positions in ‰, the strength in %.
                    .map(|v| (v[0], v[1], (v[2] * 10.0).clamp(-4.0, 4.0)))
                    .collect(),
                radius: pc("c23_attractor_repulsor_field.radius").clamp(0.01, 2.0),
                bias: pc("c23_attractor_repulsor_field.bias").clamp(0.0, 1.0),
            }),
            branch: (on("c05_branch_probability") || on("c06_branch_depth")).then(|| {
                let deep = on("c06_branch_depth");
                // Branch Depth alone: c05's declared defaults.
                let c05 = |k: &str| of("c05_branch_probability", k);
                let spacing = c05("c05_branch_probability.spacing").clamp(1.0, 256.0);
                Branch {
                    p: (c05("c05_branch_probability.probability") / 100.0).clamp(0.0, 1.0)
                        / spacing,
                    spacing,
                    own: on("c05_branch_probability"),
                    length: c05("c05_branch_probability.length").clamp(1.0, 256.0),
                    width: c05("c05_branch_probability.width").clamp(0.5, 32.0),
                    taper: (c05("c05_branch_probability.taper") / 100.0).clamp(0.0, 1.0),
                    wiggle: c05("c05_branch_probability.wiggle")
                        .clamp(0.0, 90.0)
                        .to_radians(),
                    spread: c05("c05_branch_probability.spread")
                        .clamp(0.0, 90.0)
                        .to_radians(),
                    depth: if deep {
                        p.int("c06_branch_depth.depth").clamp(1, 6) as u32
                    } else {
                        1
                    },
                    children: p.int("c06_branch_depth.children").clamp(1, 4) as u32,
                    fork: p
                        .float("c06_branch_depth.fork")
                        .clamp(5.0, 90.0)
                        .to_radians(),
                    decay: pc("c06_branch_depth.decay").clamp(0.1, 1.0),
                }
            }),
            nodules: (on("c07_nodule_frequency") || on("c08_nodule_size_range")).then(|| {
                let (min, max) = if on("c08_nodule_size_range") {
                    let a = p.float("c08_nodule_size_range.min").clamp(0.5, 32.0);
                    let b = p.float("c08_nodule_size_range.max").clamp(0.5, 32.0);
                    (a.min(b), a.max(b))
                } else {
                    (1.0, 2.5)
                };
                Nodules {
                    p: if on("c07_nodule_frequency") {
                        pc("c07_nodule_frequency.frequency").clamp(0.0, 1.0)
                    } else {
                        0.06
                    },
                    own: on("c07_nodule_frequency"),
                    cluster: if on("c07_nodule_frequency") {
                        p.int("c07_nodule_frequency.cluster").clamp(1, 6) as u32
                    } else {
                        1
                    },
                    offset: (of("c07_nodule_frequency", "c07_nodule_frequency.offset") / 100.0)
                        .clamp(0.0, 1.5),
                    min,
                    max,
                    bias: if on("c08_nodule_size_range") {
                        p.float("c08_nodule_size_range.bias").clamp(0.25, 4.0)
                    } else {
                        1.0
                    },
                }
            }),
            rough: on("c09_edge_roughness").then(|| Rough {
                amount: p.float("c09_edge_roughness.amount").clamp(0.0, 32.0),
                scale: p.float("c09_edge_roughness.scale").clamp(1.0, 256.0),
                octaves: p.int("c09_edge_roughness.octaves").clamp(1, 6) as u32,
            }),
            cavity: on("c12_cavity_void_generation").then(|| {
                let a = p.float("c12_cavity_void_generation.min").clamp(0.5, 32.0);
                let b = p.float("c12_cavity_void_generation.max").clamp(0.5, 32.0);
                Cavity {
                    noise: p.choice("c12_cavity_void_generation.mode") == 1,
                    p: (p.float("c12_cavity_void_generation.density") / 1000.0).clamp(0.0, 0.1),
                    min: a.min(b),
                    max: a.max(b),
                    wall: p.float("c12_cavity_void_generation.wall").clamp(0.0, 32.0),
                    threshold: pc("c12_cavity_void_generation.threshold").clamp(0.0, 1.0),
                    scale: p
                        .float("c12_cavity_void_generation.scale")
                        .clamp(1.0, 256.0),
                }
            }),
            fusion: on("c13_fusion").then(|| {
                (
                    p.float("c13_fusion.distance").clamp(0.5, 64.0),
                    p.choice("c13_fusion.mode") == 1,
                )
            }),
            daughters: on("c22_daughter_growth").then(|| Daughters {
                count: p.int("c22_daughter_growth.count").clamp(1, 64) as usize,
                gap: p.float("c22_daughter_growth.distance").clamp(0.0, 64.0),
                scale: pc("c22_daughter_growth.scale").clamp(0.05, 2.0),
                levels: p.int("c22_daughter_growth.levels").clamp(1, 3) as u32,
                stalk: p.float("c22_daughter_growth.stalk").clamp(0.0, 16.0),
            }),
            mask: on("c24_mask_boundary_constraint").then(|| {
                let k = "c24_mask_boundary_constraint";
                let lo = p.int(&format!("{k}.lo")).clamp(0, 255) as u8;
                let hi = p.int(&format!("{k}.hi")).clamp(0, 255) as u8;
                Mask {
                    mode: p.choice(&format!("{k}.mode")).min(MASK_MODES.len() - 1),
                    margin: pc(&format!("{k}.margin")).clamp(0.0, 2.0),
                    region: (
                        pc(&format!("{k}.t0")),
                        pc(&format!("{k}.t1")),
                        pc(&format!("{k}.p0")),
                        pc(&format!("{k}.p1")),
                    ),
                    dilate: p.int(&format!("{k}.dilate")).clamp(0, 64) as usize,
                    threshold: pc(&format!("{k}.threshold")).clamp(0.0, 1.0),
                    keys: (lo.min(hi), lo.max(hi)),
                    invert: p.flag(&format!("{k}.invert")),
                }
            }),
            veins: (on("c10_vein_density") || on("c11_vein_direction_field")).then(|| {
                let field = on("c11_vein_direction_field");
                Veins {
                    p: if on("c10_vein_density") {
                        (p.float("c10_vein_density.density") / 1000.0).clamp(0.0, 0.1)
                    } else {
                        0.006
                    },
                    length: p.float("c10_vein_density.length").clamp(1.0, 512.0),
                    fork: pc("c10_vein_density.fork").clamp(0.0, 0.5),
                    style: if on("c10_vein_density") {
                        p.choice("c10_vein_density.style").min(2)
                    } else {
                        0
                    },
                    accent: pc("c10_vein_density.accent").clamp(0.0, 2.0),
                    field: if field {
                        p.choice("c11_vein_direction_field.field")
                            .min(VEIN_FIELDS.len() - 1)
                    } else {
                        0
                    },
                    angle: p.float("c11_vein_direction_field.angle").to_radians(),
                    noise: if field {
                        pc("c11_vein_direction_field.noise").clamp(0.0, 1.0)
                    } else {
                        0.0
                    },
                    scale: p.float("c11_vein_direction_field.scale").clamp(1.0, 256.0),
                }
            }),
            mutation: Mutation {
                sub: on("c19_subchannel_mutation").then(|| {
                    (
                        p.int("c19_subchannel_mutation.step").clamp(-64, 64),
                        p.choice("c19_subchannel_mutation.by").min(2),
                        pc("c19_subchannel_mutation.chance").clamp(0.0, 1.0),
                        p.int("c19_subchannel_mutation.spread").clamp(1, 16),
                    )
                }),
                vel: on("c20_velocity_mutation").then(|| {
                    (
                        pc("c20_velocity_mutation.decay").clamp(0.1, 2.0),
                        pc("c20_velocity_mutation.drift").clamp(0.0, 1.0),
                        p.int("c20_velocity_mutation.floor").clamp(1, 127),
                    )
                }),
                gate: on("c21_gate_mutation").then(|| {
                    (
                        pc("c21_gate_mutation.start").clamp(0.05, 1.0),
                        pc("c21_gate_mutation.decay").clamp(0.1, 1.0),
                        pc("c21_gate_mutation.floor").clamp(0.01, 1.0),
                    )
                }),
            },
            evo: on("c25_evolution_morph").then(|| Evo {
                graph: p.curve("c25_evolution_morph.graph").sanitized(),
                stages: p.int("c25_evolution_morph.stages").clamp(1, 16) as usize,
                output: p
                    .choice("c25_evolution_morph.output")
                    .min(STAGE_OUTPUTS.len() - 1),
                position: pc("c25_evolution_morph.position").clamp(0.0, 1.0),
                gap: p.ticks("c25_evolution_morph.gap"),
            }),
        }
    }

    /// (progress, stage) of every stage the output shows, and whether
    /// they are rings of one output.
    fn stages(&self) -> (Vec<(f64, u16)>, bool) {
        match &self.evo {
            None => (vec![(1.0, 0)], false),
            Some(e) => match e.output {
                0 => (vec![(e.graph.eval(e.position).clamp(0.0, 1.0), 0)], false),
                k => {
                    let n = e.stages;
                    let v = (0..n)
                        .map(|s| {
                            let x = if n > 1 {
                                s as f64 / (n - 1) as f64
                            } else {
                                1.0
                            };
                            (e.graph.eval(x).clamp(0.0, 1.0), s as u16)
                        })
                        .collect();
                    (v, k == 2)
                }
            },
        }
    }

    /// How many copies of the growth (and of the kept art) the output has.
    fn copies(&self) -> u64 {
        match &self.evo {
            Some(e) if e.output == 1 => e.stages as u64,
            _ => 1,
        }
    }

    /// The farthest the primitives reach beyond the base radius (rows).
    fn reach_extra(&self, r: f64) -> f64 {
        let mut e = 0.0;
        if let Some(b) = &self.branch {
            let mut len = 0.0;
            let mut l = b.length;
            for _ in 0..b.depth {
                len += l;
                l *= b.decay;
            }
            e += len + b.width;
        }
        if let Some(n) = &self.nodules {
            e = f64::max(e, n.max * (2.2 + n.offset));
        }
        if let Some(d) = &self.daughters {
            let mut s = r * d.scale;
            let mut reach = 0.0;
            for _ in 0..d.levels {
                reach += d.gap + 2.0 * s;
                s *= d.scale;
            }
            e += reach;
        }
        if let Some(rg) = &self.rough {
            e += rg.amount;
        }
        if let Some((k, _)) = self.fusion {
            e += k;
        }
        e
    }

    /// Row units per time cell (a time radius of `rx` cells is `ry` rows).
    fn ax(&self) -> f64 {
        if self.ry > 0.0 {
            self.ry / self.rx
        } else {
            1.0 / self.rx
        }
    }

    /// The largest radius factor the modulators can give (for the padding).
    fn reach_factor(&self) -> f64 {
        let mut f = 1.0;
        if let Some(l) = &self.lobes {
            f *= 1.0 + l.amount * (1.0 + self.asym.as_ref().map_or(0.0, |a| a.amount));
        }
        if let Some(a) = &self.asym {
            f *= 1.0 + a.skew;
        }
        if let Some(b) = &self.bulge {
            f *= 1.0 + b.pressure;
        }
        if self.density.is_some() {
            f *= 4.0;
        }
        if let Some((cx, cc, _)) = self.curv {
            f *= (1.0 + cx.max(cc).max(0.0)).min(5.0);
        }
        for b in [&self.tbias, &self.pbias].into_iter().flatten() {
            f *= b.from.max(b.to);
        }
        if let Some(a) = &self.attract {
            f *= 1.0 + a.pts.iter().map(|p| p.2.max(0.0)).sum::<f64>();
        }
        f
    }
}

// ================================================================ kernel

/// One note the plan makes: row, cells `[c0, c1)`, its art note, its
/// attributes and its stage's time shift.
#[derive(Clone, Copy, Debug)]
struct Piece {
    row: u16,
    c0: u32,
    c1: u32,
    rep: u32,
    sub: u32,
    vel: u8,
    ch: u8,
    /// Gate in ten-thousandths.
    gate: u16,
    stage: u16,
}

/// A slot's plan: its grid geometry and pieces.
#[derive(Debug)]
struct SlotPlan {
    idx: Vec<usize>,
    t0: u64,
    cell: u64,
    /// The art's span (the shift between stages in sequence).
    span: u64,
    pieces: Vec<Piece>,
}

/// The plans counted by the preflight (or their refusal), keyed by the
/// source's fingerprint, for the run that follows.
type PlanCache = Mutex<Option<(u64, Result<Vec<SlotPlan>, TransformError>)>>;

/// One mass's uneven lobes: (amplitude, centre angle, width) per lobe.
type LobeTable = Vec<(f64, f64, f64)>;

#[derive(Debug)]
struct Tumours {
    cfg: Cfg,
    tracks: Vec<SourceTrack>,
    ctx: Ctx,
    growth: Growth,
    cache: PlanCache,
}

/// FNV-1a over the notes' places and slots (the cached plan is reused
/// only for the very same notes).
fn fingerprint(src: &Source) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    eat(src.notes.len() as u64);
    for n in &src.notes {
        eat(n.start);
        eat(n.dur);
        eat(u64::from(n.pitch) | u64::from(n.track) << 16 | u64::from(n.vel) << 32);
    }
    h
}

/// A summed-area table of `v` (w × h), (w + 1) × (h + 1).
fn sat(w: usize, h: usize, v: impl Fn(usize) -> u64) -> Vec<u64> {
    let mut s = vec![0u64; (w + 1) * (h + 1)];
    for r in 0..h {
        let mut acc = 0u64;
        for c in 0..w {
            acc += v(r * w + c);
            s[(r + 1) * (w + 1) + c + 1] = s[r * (w + 1) + c + 1] + acc;
        }
    }
    s
}

/// The sum of a summed-area table over the window of half-size `k` around
/// (c, r), clipped to the grid, and the window's cell count.
fn window(s: &[u64], w: usize, h: usize, c: usize, r: usize, k: usize) -> (u64, u64) {
    let (c0, c1) = (c.saturating_sub(k), (c + k + 1).min(w));
    let (r0, r1) = (r.saturating_sub(k), (r + k + 1).min(h));
    let at = |cc: usize, rr: usize| s[rr * (w + 1) + cc];
    let sum = at(c1, r1) + at(c0, r0) - at(c0, r1) - at(c1, r0);
    (sum, ((c1 - c0) * (r1 - r0)) as u64)
}

/// The lobe shape of angle `theta` for a mass (the radius × 1 + Amount ·
/// it): the symmetric closed form, or (uneven) the strongest of its seeded
/// lobes.
/// One mass's uneven lobes (c03): amplitude, centre angle and width per
/// lobe, seeded by the mass.
fn lobe_table(l: &Lobes, a: &Asym, seed: u64, comp: u64) -> LobeTable {
    let k = f64::from(l.k);
    (0..l.k)
        .map(|j| {
            let mut g = stream(seed, salt_of("tumours.lobes"), &[comp, u64::from(j)]);
            let amp = (1.0 + a.amount * (2.0 * g.unit() - 1.0)).max(0.0);
            let off = a.amount * (g.unit() - 0.5) * (PI / k);
            let width = 1.0 + a.amount * (g.unit() - 0.5);
            (amp, l.angle + TAU * f64::from(j) / k + off, width)
        })
        .collect()
}

/// The lobe profile at angle `theta`: the symmetric rose, or (c03, a
/// mass's [`lobe_table`]) the strongest uneven lobe.
fn lobe_shape(l: &Lobes, table: Option<&[(f64, f64, f64)]>, theta: f64) -> f64 {
    let k = f64::from(l.k);
    let Some(t) = table else {
        let x = (1.0 + (k * (theta - l.angle)).cos()) / 2.0;
        return x.powf(l.sharp);
    };
    let lobe = |&(amp, centre, width): &(f64, f64, f64)| {
        let d = (theta - centre + PI).rem_euclid(TAU) - PI;
        let x = (d.abs() * k / width).min(PI);
        amp * ((1.0 + x.cos()) / 2.0).powf(l.sharp)
    };
    if l.k <= 4 {
        return t.iter().map(lobe).fold(0.0, f64::max);
    }
    // A lobe reaches at most 0.75 of a spacing from its place (width ≤
    // 1.5) and lies at most 0.25 of a spacing off it: only the lobes j with
    // |u − j| < 1 (u = the angle in spacings) give more than 0 (sharpness ≥
    // 0.5, so 0^s = 0) — the four around u cover them.
    let u = ((theta - l.angle) * k / TAU).floor() as i64;
    (-1..=2)
        .map(|dj| lobe(&t[(u + dj).rem_euclid(i64::from(l.k)) as usize]))
        .fold(0.0, f64::max)
}

/// Control Sweep values per source note (each `None` unless swept).
#[derive(Default)]
struct Swept {
    scale: Option<Vec<f32>>,
    ring: Option<Vec<f32>>,
    lobe: Option<Vec<f32>>,
    bprob: Option<Vec<f32>>,
    blen: Option<Vec<f32>>,
    nfreq: Option<Vec<f32>>,
    rough: Option<Vec<f32>>,
    sub: Option<Vec<f32>>,
    vdecay: Option<Vec<f32>>,
    gstart: Option<Vec<f32>>,
}

/// A value per source note when `sw` is set, else `base`.
#[inline]
fn at(sw: &Option<Vec<f32>>, k: u32, base: f64) -> f64 {
    match sw {
        Some(v) => f64::from(v[k as usize]),
        None => base,
    }
}

/// One slot's fields for one stage (what the batches build on).
struct Field {
    g: Grid,
    ax: f64,
    /// Art occupancy.
    art: Vec<bool>,
    /// Distance to the nearest seed (row units) and its cell.
    dist: Vec<f32>,
    feat: Vec<u32>,
    /// The signed growth field at full progress (≤ 0 inside the body), the
    /// distance it was read at (after the warp) and the radius there.
    sd: Vec<f32>,
    dq: Vec<f32>,
    rad: Vec<f32>,
    /// Anchor seeds' art notes (cells that are not art).
    anchors: HashMap<u32, u32>,
    /// Per seed cell: its radius before the lobes (row units).
    rfac: Vec<f32>,
    /// The art's box: ticks [t0, t1), pitches lo ..= hi.
    abox: (u64, u64, u8, u8),
    /// Attractors / repulsors in row units (x, y, strength).
    att: Vec<(f64, f64, f64)>,
    /// The art's start column and lowest row in grid cells: the origin of
    /// the row-unit coordinates, so nothing depends on the padding.
    ox: f64,
    oy: f64,
}

impl Field {
    /// Row-unit coordinates of cell `i` (from the art's start and lowest
    /// row).
    #[inline]
    fn xy(&self, i: usize) -> (f64, f64) {
        let w = self.g.w;
        (
            ((i % w) as f64 - self.ox) * self.ax,
            (i / w) as f64 - self.oy,
        )
    }

    /// The (fractional) column and row of a point.
    #[inline]
    fn cr(&self, x: f64, y: f64) -> (f64, f64) {
        (x / self.ax + self.ox, y + self.oy)
    }

    /// The base field at progress `t` (the radius × t).
    fn sd_at(&self, t: f64) -> Vec<f32> {
        if t >= 1.0 {
            return self.sd.clone();
        }
        self.dq
            .iter()
            .zip(&self.rad)
            .map(|(&d, &r)| {
                if d.is_finite() {
                    d - r * t as f32
                } else {
                    f32::INFINITY
                }
            })
            .collect()
    }

    /// The art note (index) of seed cell `f`.
    fn rep_of(&self, f: u32) -> u32 {
        match self.g.rep[f as usize] {
            0 => self.anchors.get(&f).copied().unwrap_or(0),
            r => r - 1,
        }
    }
}

impl Tumours {
    /// The Control Sweeps' values per source note.
    fn swept(&self, src: &Source) -> Swept {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return Swept::default();
        };
        let places = note_places(src);
        let one = |key: &str, base: f64, lo: f64, hi: f64| -> Option<Vec<f32>> {
            sw.drives(key).then(|| {
                src.notes
                    .iter()
                    .zip(&places)
                    .map(|(n, &pl)| sw.float(key, base, n, pl).clamp(lo, hi) as f32)
                    .collect()
            })
        };
        let c = &self.cfg;
        let pc =
            |v: Option<Vec<f32>>| v.map(|v| v.into_iter().map(|x| x / 100.0).collect::<Vec<f32>>());
        Swept {
            scale: pc(one("scale", c.scale * 100.0, 0.0, 400.0)),
            ring: one("ring", c.ring, 0.5, 64.0),
            lobe: pc(c
                .lobes
                .as_ref()
                .and_then(|l| one("c02_lobe_count.amount", l.amount * 100.0, 0.0, 400.0))),
            // The swept chance is per site (%), applied per contour cell
            // like the static one: over the spacing.
            bprob: c.branch.as_ref().filter(|b| b.own).and_then(|b| {
                one(
                    "c05_branch_probability.probability",
                    b.p * b.spacing * 100.0,
                    0.0,
                    100.0,
                )
                .map(|v| {
                    v.into_iter()
                        .map(|x| (f64::from(x) / 100.0 / b.spacing) as f32)
                        .collect()
                })
            }),
            blen: c
                .branch
                .as_ref()
                .filter(|b| b.own)
                .and_then(|b| one("c05_branch_probability.length", b.length, 1.0, 256.0)),
            nfreq: pc(c
                .nodules
                .as_ref()
                .filter(|n| n.own)
                .and_then(|n| one("c07_nodule_frequency.frequency", n.p * 100.0, 0.0, 100.0))),
            rough: c
                .rough
                .as_ref()
                .and_then(|r| one("c09_edge_roughness.amount", r.amount, 0.0, 32.0)),
            sub: c.mutation.sub.and_then(|s| {
                let key = "c19_subchannel_mutation.step";
                sw.drives(key).then(|| {
                    src.notes
                        .iter()
                        .zip(&places)
                        .map(|(n, &pl)| sw.int(key, s.0, n, pl).clamp(-64, 64) as f32)
                        .collect()
                })
            }),
            vdecay: pc(c
                .mutation
                .vel
                .and_then(|v| one("c20_velocity_mutation.decay", v.0 * 100.0, 10.0, 200.0))),
            gstart: pc(c
                .mutation
                .gate
                .and_then(|g| one("c21_gate_mutation.start", g.0 * 100.0, 5.0, 100.0))),
        }
    }

    /// The padding (cells) the settings can reach along time.
    fn pad_cells(&self, scale_hi: f64) -> usize {
        (self.reach(scale_hi) / self.cfg.ax()).ceil().min(PAD_CAP) as usize + 2
    }

    /// How far (row units) the growth can reach from the art.
    fn reach(&self, scale_hi: f64) -> f64 {
        let c = &self.cfg;
        let base = c.ry.max(c.rx * c.ax()) * scale_hi * c.reach_factor();
        (base + c.reach_extra(base) + 2.0).min(PAD_CAP)
    }

    /// The rows a slot's grid needs above and below the art (and the band's
    /// height before the keyboard's ends cut it, which sizes the cell — so
    /// a standard selection gets the same grid on the 256-key keyboard).
    fn band(&self, lo: u8, hi: u8, top: u8, scale_hi: f64) -> ((u8, u8), usize) {
        let r = self.reach(scale_hi).ceil() as usize + 1;
        let a = usize::from(lo).saturating_sub(r) as u8;
        let b = (usize::from(hi) + r).min(usize::from(top)) as u8;
        ((a, b), usize::from(hi - lo) + 1 + 2 * r)
    }

    /// The cell width for a span (coarsened so the grid fits MAX_CELLS).
    fn cell_for(&self, span: u64, rows: usize, pad: usize) -> u64 {
        let max_w = (MAX_CELLS / rows.max(1)).max(8);
        let mut cell = self.cfg.cell;
        let usable = max_w.saturating_sub(2 * pad.min(max_w / 4)).max(1) as u64;
        if span.div_ceil(cell) > usable {
            cell = span.div_ceil(usable).max(cell);
        }
        cell
    }

    /// The seed cells of a grid (and the anchors' art notes).
    fn seed_cells(
        &self,
        src: &Source,
        idx: &[usize],
        g: &Grid,
        art: &[bool],
    ) -> (Vec<bool>, HashMap<u32, u32>) {
        let (w, h) = (g.w, g.h);
        let mut anchors = HashMap::new();
        let seeds = match &self.cfg.seeds {
            Seeds::All => art.to_vec(),
            Seeds::Contour => {
                // The outer contour: art cells beside the outside (empty
                // cells connected to the grid's border) — the rims of holes
                // and enclosed gaps are not seeds.
                let out = g.outside(&g.rep);
                (0..w * h)
                    .map(|i| {
                        if !art[i] {
                            return false;
                        }
                        let (c, r) = (i % w, i / w);
                        c == 0
                            || c + 1 == w
                            || r == 0
                            || r + 1 == h
                            || out[i - 1]
                            || out[i + 1]
                            || out[i - w]
                            || out[i + w]
                    })
                    .collect()
            }
            Seeds::Dense { fill, window: k } => {
                // Art cells whose window is at least `fill` as full as the
                // fullest art cell's.
                let s = sat(w, h, |i| u64::from(art[i]));
                let share: Vec<f64> = (0..w * h)
                    .map(|i| {
                        if !art[i] {
                            return 0.0;
                        }
                        let (sum, n) = window(&s, w, h, i % w, i / w, *k);
                        sum as f64 / n as f64
                    })
                    .collect();
                let top = share.iter().copied().fold(0.0, f64::max);
                share
                    .iter()
                    .map(|&v| v > 0.0 && v >= fill * top - 1e-9)
                    .collect()
            }
            Seeds::Anchors(pts) => {
                let (t0, t1) = span_of(&src.notes, idx);
                let (lo, hi) = pitch_span(&src.notes, idx);
                let mut v = vec![false; w * h];
                let art_cells: Vec<usize> = (0..w * h).filter(|&i| art[i]).collect();
                for &(u, p) in pts {
                    let tick = t0 as f64 + u * (t1 - t0) as f64;
                    let c = ((tick - g.t0 as f64) / g.cell as f64)
                        .floor()
                        .clamp(0.0, (w - 1) as f64) as usize;
                    let r = (f64::from(lo) + p * f64::from(hi - lo) - g.row0 as f64)
                        .round()
                        .clamp(0.0, (h - 1) as f64) as usize;
                    let i = r * w + c;
                    v[i] = true;
                    if !art[i] {
                        // The anchor's art note: the nearest art cell.
                        let ax = self.cfg.ax();
                        let near = art_cells.iter().min_by(|&&a, &&b| {
                            let d = |j: usize| {
                                let dx = ((j % w) as f64 - c as f64) * ax;
                                let dy = (j / w) as f64 - r as f64;
                                dx * dx + dy * dy
                            };
                            d(a).total_cmp(&d(b)).then(a.cmp(&b))
                        });
                        if let Some(&j) = near {
                            anchors.insert(i as u32, g.rep[j] - 1);
                        }
                    }
                }
                v
            }
            Seeds::Random(count) => {
                // The `count` art cells with the smallest seeded hashes.
                let mut keyed: Vec<(u64, usize)> = (0..w * h)
                    .filter(|&i| art[i])
                    .map(|i| {
                        let (c, r) = (i % w, i / w);
                        let t = g.tick_of(c);
                        (
                            stream(
                                self.cfg.seed,
                                salt_of("tumours.sample"),
                                &[t, (r + g.row0) as u64],
                            )
                            .next_u64(),
                            i,
                        )
                    })
                    .collect();
                let k = (*count).min(keyed.len());
                if k < keyed.len() {
                    keyed.select_nth_unstable(k);
                }
                let mut v = vec![false; w * h];
                for &(_, i) in &keyed[..k] {
                    v[i] = true;
                }
                v
            }
            Seeds::Starts => {
                let mut v = vec![false; w * h];
                for &i in idx {
                    let n = &src.notes[i];
                    if let Some((c0, _)) = g.cols(n.start, n.end()) {
                        v[g.row_of(n.pitch) * w + c0] = true;
                    }
                }
                v
            }
        };
        (seeds, anchors)
    }

    /// The base field of one slot at progress `t`.
    fn field(&self, src: &Source, idx: &[usize], sw: &Swept) -> Field {
        let scales = sw.scale.as_deref();
        let c = &self.cfg;
        let ax = c.ax();
        let (t0, t1) = span_of(&src.notes, idx);
        let keys = src.keys;
        let scale_hi = scales.map_or(c.scale, |s| s.iter().fold(0.0f32, |a, &b| a.max(b)) as f64);
        let pad = self.pad_cells(scale_hi);
        let (plo, phi) = pitch_span(&src.notes, idx);
        let (band, rows) = self.band(plo, phi, keys.max_pitch(), scale_hi);
        let cell = self.cell_for(t1 - t0, rows, pad);
        let g = Grid::build_band(keys, &src.notes, idx, t0, t1, cell, pad, band, MAX_CELLS);
        let (w, h) = (g.w, g.h);
        let n = w * h;
        let art: Vec<bool> = g.rep.iter().map(|&r| r != 0).collect();
        let (seeds, anchors) = self.seed_cells(src, idx, &g, &art);
        let (dist, feat) = feature_transform(w, h, &seeds, ax, 1.0);
        let mut f = Field {
            g,
            ax,
            art,
            dist,
            feat,
            sd: vec![f32::INFINITY; n],
            dq: vec![f32::INFINITY; n],
            rad: vec![0.0; n],
            anchors,
            rfac: Vec::new(),
            abox: (
                t0,
                t1,
                pitch_span(&src.notes, idx).0,
                pitch_span(&src.notes, idx).1,
            ),
            att: Vec::new(),
            ox: 0.0,
            oy: 0.0,
        };
        f.ox = (t0 - f.g.t0) as f64 / f.g.cell as f64;
        f.oy = f.abox.2 as f64 - f.g.row0 as f64;
        if let Some(a) = &c.attract {
            let span = f64::from(f.abox.3 - f.abox.2);
            f.att = a
                .pts
                .iter()
                .map(|&(u, v, s)| (u * (t1 - t0) as f64 / f.g.cell as f64 * ax, v * span, s))
                .collect();
        }
        let rfac = self.radius_factors(src, idx, &f, &seeds, scales);
        let comps = (c.lobes.is_some() || c.asym.is_some()).then(|| components(&f, &seeds));
        let peaks = c.bulge.as_ref().map(|b| density_peaks(&f, b));
        let none = u32::MAX;
        // The last mass's lobe table and skew direction (cells in raster
        // order mostly share their nearest mass).
        let mut mass: Option<(usize, LobeTable, f64)> = None;
        for i in 0..n {
            let (x, y) = f.xy(i);
            let (qx, qy) = match &peaks {
                Some(pk) if !pk.is_empty() => warp(pk, x, y),
                _ => (x, y),
            };
            let (d, fi) = if (qx, qy) == (x, y) {
                (f64::from(f.dist[i]), f.feat[i])
            } else {
                sample(&f, qx, qy)
            };
            if fi == none || !d.is_finite() {
                continue;
            }
            let mut r = f64::from(rfac[fi as usize]);
            if let Some((comp, cent)) = &comps {
                let k = comp[fi as usize] as usize;
                let (cx, cy) = cent[k];
                let theta = (qy - cy).atan2(qx - cx);
                if mass.as_ref().is_none_or(|m| m.0 != k) {
                    let table = match (&c.lobes, c.asym.as_ref().filter(|a| a.amount > 0.0)) {
                        (Some(l), Some(a)) => lobe_table(l, a, c.seed, k as u64),
                        _ => Vec::new(),
                    };
                    let psi = stream(c.seed, salt_of("tumours.skew"), &[k as u64]).unit() * TAU;
                    mass = Some((k, table, psi));
                }
                let (_, table, psi) = mass.as_ref().unwrap();
                if let Some(l) = &c.lobes {
                    let amount = at(&sw.lobe, f.rep_of(fi), l.amount);
                    let t = (!table.is_empty()).then_some(table.as_slice());
                    r *= 1.0 + amount * lobe_shape(l, t, theta);
                }
                if let Some(a) = c.asym.as_ref().filter(|a| a.skew > 0.0) {
                    r *= 1.0 + a.skew * (theta - psi).cos();
                }
            }
            f.sd[i] = (d - r) as f32;
            f.dq[i] = d as f32;
            f.rad[i] = r as f32;
        }
        f.rfac = rfac;
        f
    }

    /// Per seed cell: the radius (row units) before the angle-dependent
    /// lobes: base × scale × density × curvature × time / pitch bias ×
    /// attractors (0 for cells that are not seeds).
    fn radius_factors(
        &self,
        src: &Source,
        idx: &[usize],
        f: &Field,
        seeds: &[bool],
        scales: Option<&[f32]>,
    ) -> Vec<f32> {
        let c = &self.cfg;
        let g = &f.g;
        let (w, h) = (g.w, g.h);
        let (t0, t1) = span_of(&src.notes, idx);
        let (lo, hi) = pitch_span(&src.notes, idx);
        let dens = c.density.map(|(gain, k)| {
            let s = sat(w, h, |i| u64::from(g.count[i]));
            let mut v = vec![0f64; w * h];
            let (mut sum, mut n) = (0.0, 0usize);
            for i in 0..w * h {
                if seeds[i] {
                    let (a, cells) = window(&s, w, h, i % w, i / w, k);
                    v[i] = a as f64 / cells as f64;
                    sum += v[i];
                    n += 1;
                }
            }
            (gain, v, if n > 0 { sum / n as f64 } else { 0.0 })
        });
        let fill = c
            .curv
            .map(|(_, _, k)| (sat(w, h, |i| u64::from(f.art[i])), k));
        let diag = {
            let dx = (t1 - t0) as f64 / g.cell as f64 * f.ax;
            let dy = f64::from(hi - lo).max(1.0);
            dx.hypot(dy)
        };
        let a_radius = c.attract.as_ref().map_or(1.0, |a| a.radius);
        let mut out = vec![0f32; w * h];
        for i in 0..w * h {
            if !seeds[i] {
                continue;
            }
            let (col, row) = (i % w, i / w);
            let k = f.rep_of(i as u32) as usize;
            let mut r = c.ry.max(0.0) * scales.map_or(c.scale, |s| f64::from(s[k]));
            if let Some((gain, v, mean)) = &dens
                && *mean > 0.0
            {
                r *= (1.0 + gain * (v[i] / mean - 1.0)).clamp(0.0, 4.0);
            }
            if let (Some((convex, concave, _)), Some((s, k))) = (c.curv, &fill) {
                // The fill around the seed against a straight edge's
                // (k + 1 of 2k + 1 rows): −1 a lone cell's tip … 0 straight
                // … 1 a filled bay.
                let (a, cells) = window(s, w, h, col, row, *k);
                let fill = a as f64 / cells as f64;
                let flat = (*k + 1) as f64 / (2 * *k + 1) as f64;
                let bend = if fill < flat {
                    (fill - flat) / flat
                } else {
                    (fill - flat) / (1.0 - flat)
                };
                r *= (1.0 + convex * (-bend).max(0.0) + concave * bend.max(0.0)).clamp(0.0, 5.0);
            }
            let u = ((g.tick_of(col) as f64 - t0 as f64) / (t1 - t0).max(1) as f64).clamp(0.0, 1.0);
            let v = if hi > lo {
                ((f64::from(g.pitch_of(row)) - f64::from(lo)) / f64::from(hi - lo)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            for (b, x) in [(&c.tbias, u), (&c.pbias, v)] {
                if let Some(b) = b {
                    r *= b.from + (b.to - b.from) * b.graph.eval(x);
                }
            }
            if !f.att.is_empty() {
                let (x, y) = f.xy(i);
                let mut m = 1.0;
                for &(px, py, s) in &f.att {
                    let rr = (a_radius * diag).max(1e-6);
                    m += s * (-((x - px).powi(2) + (y - py).powi(2)) / (rr * rr)).exp();
                }
                r *= m.max(0.0);
            }
            out[i] = r as f32;
        }
        out
    }

    /// One slot's plan: its field once, a body per stage (rings merged
    /// into one), the pieces with their attributes.
    fn slot_plan(
        &self,
        src: &Source,
        idx: Vec<usize>,
        sw: &Swept,
    ) -> Result<SlotPlan, TransformError> {
        let f = self.field(src, &idx, sw);
        let n = f.g.w * f.g.h;
        let none = u32::MAX;
        let (stages, rings) = self.cfg.stages();
        let mut pieces = Vec::new();
        let mut budget = Budget::default();
        if rings {
            // Each cell belongs to the first stage that grows it; the stage
            // is its generation.
            let mut merged: Option<Body> = None;
            for &(t, s) in &stages {
                let b = self.body(src, &f, f.sd_at(t), t, sw, &mut budget)?;
                let stage_gen = s + 1;
                match &mut merged {
                    None => {
                        let mut b = b;
                        for i in 0..n {
                            b.gk[i] = (b.gk[i] & 0xff00) | stage_gen.min(255);
                        }
                        merged = Some(b);
                    }
                    Some(m) => {
                        for i in 0..n {
                            if m.own[i] == none && b.own[i] != none {
                                m.own[i] = b.own[i];
                                m.gk[i] = (b.gk[i] & 0xff00) | stage_gen.min(255);
                                m.sid[i] = b.sid[i];
                                m.vein[i] = b.vein[i];
                            }
                        }
                    }
                }
            }
            if let Some(m) = merged {
                self.pieces(src, &f, &m, sw, 0, &mut pieces);
            }
        } else {
            for &(t, s) in &stages {
                let b = self.body(src, &f, f.sd_at(t), t, sw, &mut budget)?;
                self.pieces(src, &f, &b, sw, s, &mut pieces);
            }
        }
        let (t0, t1) = span_of(&src.notes, &idx);
        Ok(SlotPlan {
            idx,
            t0: f.g.t0,
            cell: f.g.cell,
            span: t1 - t0,
            pieces,
        })
    }

    /// A growth cell's attributes after c19 – c21 (and the veins' accent):
    /// subchannel, velocity, channel, gate (ten-thousandths).
    fn attrs(&self, src: &Source, b: &Body, sw: &Swept, i: usize) -> (u32, u8, u8, u16) {
        let c = &self.cfg;
        let m = &c.mutation;
        let k = b.own[i];
        let n = &src.notes[k as usize];
        let vein = b.vein[i];
        let g = i64::from(b.gk[i] & 0xff) + i64::from(vein);
        let kind = if vein {
            KIND_VEIN
        } else {
            (b.gk[i] >> 8) as u8
        };
        let key = [u64::from(b.sid[i]), g as u64];
        let (mut sub, mut ch) = (n.sub, n.ch);
        if let Some((step, by, chance, spread)) = m.sub {
            let step = at(&sw.sub, k, step as f64).round() as i64;
            let kind = i64::from(kind);
            let mut steps = match by {
                0 => step * (g - 1),
                1 => step * kind,
                _ => step * (g - 1 + kind),
            };
            if chance > 0.0 {
                let mut r = stream(c.seed, salt_of("tumours.sub"), &key);
                if r.unit() < chance {
                    steps += if r.unit() < 0.5 { spread } else { -spread };
                }
            }
            if steps != 0 {
                let s = sub_stepped(&self.tracks, n, steps);
                (sub, ch) = (s.sub, s.ch);
            }
        }
        let mut vel = f64::from(n.vel);
        if let Some((decay, drift, floor)) = m.vel {
            vel *= at(&sw.vdecay, k, decay).powi((g - 1) as i32);
            if drift > 0.0 {
                let u = stream(c.seed, salt_of("tumours.vel"), &key).unit();
                vel *= 1.0 + drift * (2.0 * u - 1.0);
            }
            vel = vel.clamp(floor as f64, 127.0);
        }
        if vein && let Some(v) = c.veins.as_ref().filter(|v| v.style == 0) {
            vel *= v.accent;
        }
        let mut gate = 1.0;
        if let Some((start, decay, floor)) = m.gate {
            gate = (at(&sw.gstart, k, start) * decay.powi((g - 1) as i32)).clamp(floor, 1.0);
        }
        (
            sub,
            vel.round().clamp(1.0, 127.0) as u8,
            ch,
            (gate * 10_000.0).round() as u16,
        )
    }

    /// A body's notes: runs per row broken where the attributes change (or
    /// one note per cell).
    fn pieces(
        &self,
        src: &Source,
        f: &Field,
        b: &Body,
        sw: &Swept,
        stage: u16,
        out: &mut Vec<Piece>,
    ) {
        let g = &f.g;
        let w = g.w;
        let none = u32::MAX;
        for r in 0..g.h {
            let pitch = g.pitch_of(r);
            let mut c = 0;
            while c < w {
                let i = r * w + c;
                if b.own[i] == none {
                    c += 1;
                    continue;
                }
                let a = self.attrs(src, b, sw, i);
                let start = c;
                c += 1;
                if !self.cfg.cells {
                    while c < w
                        && b.own[r * w + c] != none
                        && self.attrs(src, b, sw, r * w + c) == a
                    {
                        c += 1;
                    }
                }
                out.push(Piece {
                    row: u16::from(pitch),
                    c0: start as u32,
                    c1: c as u32,
                    rep: b.own[i],
                    sub: a.0,
                    vel: a.1,
                    ch: a.2,
                    gate: a.3,
                    stage,
                });
            }
        }
    }

    /// The growth body of one slot at progress `t`: the base field united
    /// with the primitives (branches, nodules, daughters), roughened,
    /// fused, hollowed by cavities and masked — every growth cell's art
    /// note, generation, kind and structure.
    fn body(
        &self,
        src: &Source,
        f: &Field,
        sd0: Vec<f32>,
        t: f64,
        sw: &Swept,
        budget: &mut Budget,
    ) -> Result<Body, TransformError> {
        let c = &self.cfg;
        let g = &f.g;
        let (w, h) = (g.w, g.h);
        let n = w * h;
        let none = u32::MAX;
        let ax = f.ax;
        let mut own = vec![none; n];
        let mut gk = vec![0u16; n];
        let mut sid = vec![none; n];
        // The base's attributes: the nearest seed's note, its generation
        // by distance rings.
        for i in 0..n {
            let fi = f.feat[i];
            if fi != none {
                own[i] = f.rep_of(fi);
                let ring = at(&sw.ring, own[i], c.ring).max(0.5);
                gk[i] = gen_kind(1.0 + (f64::from(f.dist[i]) / ring).floor(), KIND_BASE);
                // The base is one structure (seeded mutations by its rings).
                sid[i] = 0;
            }
        }
        let mut sd = sd0;
        // Primitives on the base's contour.
        let mut prims: Vec<Prim> = Vec::new();
        let sites = contour(w, h, &sd, &f.art);
        let site_info = |i: usize| -> Option<Site> {
            let fi = f.feat[i];
            if fi == none {
                return None;
            }
            let (x, y) = f.xy(i);
            let (sx, sy) = f.xy(fi as usize);
            let (mut nx, mut ny) = (x - sx, y - sy);
            let len = nx.hypot(ny);
            if len > 1e-9 {
                (nx, ny) = (nx / len, ny / len);
            } else {
                (nx, ny) = gradient(&sd, w, h, i, ax).unwrap_or((0.0, 1.0));
            }
            Some(Site {
                x,
                y,
                nx,
                ny,
                rep: own[i],
                g: (gk[i] & 0xff) as u8,
                key: [g.tick_of(i % w), (i / w + g.row0) as u64],
                r: f64::from(f.rfac[fi as usize]),
            })
        };
        let mut segs = 0usize;
        if let Some(b) = &c.branch {
            for &i in &sites {
                let Some(s) = site_info(i) else { continue };
                let mut rng = stream(c.seed, salt_of("tumours.branch"), &s.key);
                if rng.unit() >= at(&sw.bprob, s.rep, b.p) {
                    continue;
                }
                let a0 = s.ny.atan2(s.nx) + b.spread * (2.0 * rng.unit() - 1.0);
                let id = prims.len() as u32;
                let length = at(&sw.blen, s.rep, b.length);
                self.grow_branch(
                    b,
                    &s,
                    f,
                    t,
                    1,
                    (s.x, s.y),
                    a0,
                    (length, b.width),
                    &mut rng,
                    id,
                    &mut prims,
                    &mut segs,
                )?;
            }
        }
        if let Some(nd) = &c.nodules {
            for &i in &sites {
                let Some(s) = site_info(i) else { continue };
                let mut rng = stream(c.seed, salt_of("tumours.nodule"), &s.key);
                if rng.unit() >= at(&sw.nfreq, s.rep, nd.p) {
                    continue;
                }
                let r = (nd.min + (nd.max - nd.min) * rng.unit().powf(nd.bias)) * t;
                if r <= 0.0 {
                    continue;
                }
                let (cx, cy) = (s.x + s.nx * r * nd.offset, s.y + s.ny * r * nd.offset);
                let id = prims.len() as u32;
                let gn = s.g.saturating_add(1);
                prims.push(Prim::disc(cx, cy, r, s.rep, gn, KIND_NODULE, id));
                for _ in 1..nd.cluster {
                    let a = rng.unit() * TAU;
                    let d = r * (0.5 + 0.6 * rng.unit());
                    let rr = r * (0.35 + 0.4 * rng.unit());
                    prims.push(Prim::disc(
                        cx + a.cos() * d,
                        cy + a.sin() * d,
                        rr,
                        s.rep,
                        gn,
                        KIND_NODULE,
                        id,
                    ));
                }
                if prims.len() > PRIM_CAP {
                    return Err(cap_error());
                }
            }
        }
        let (mut psd, mut pid) = (vec![f32::INFINITY; n], vec![none; n]);
        raster(f, &prims, 0, &mut psd, &mut pid, budget)?;
        if let Some(d) = &c.daughters {
            // Daughters beside the primary body (base, branches, nodules).
            let primary: Vec<f32> = (0..n).map(|i| sd[i].min(psd[i])).collect();
            let rim = contour(w, h, &primary, &f.art);
            let from = prims.len();
            self.daughters(d, &rim, &site_info, f, t, &mut prims, &pid)?;
            raster(f, &prims, from, &mut psd, &mut pid, budget)?;
        }
        // Union (or c13 Melt), the owner the smaller of the two.
        let melt = c.fusion.filter(|f| f.1).map(|f| f.0);
        for i in 0..n {
            if pid[i] == none {
                continue;
            }
            let (a, b) = (sd[i], psd[i]);
            if b < a {
                let p = &prims[pid[i] as usize];
                own[i] = p.rep;
                gk[i] = gen_kind(f64::from(p.g), p.kind);
                sid[i] = 1 + p.id;
            }
            sd[i] = match melt {
                Some(k) => smin(f64::from(a), f64::from(b), k) as f32,
                None => a.min(b),
            };
        }
        if let Some(rg) = &c.rough {
            let salt = salt_of("tumours.rough");
            for i in 0..n {
                if !sd[i].is_finite() {
                    continue;
                }
                let (x, y) = f.xy(i);
                let (x, y) = (
                    x + f.ox * ax + g.t0 as f64 / g.cell as f64 * ax,
                    y + f.oy + g.row0 as f64,
                );
                let a = if own[i] != none {
                    at(&sw.rough, own[i], rg.amount)
                } else {
                    rg.amount
                };
                // |fbm| ≤ 1: a cell farther than the amount from the
                // contour keeps its side (only the side is read below).
                let reach = a.abs() as f32 * 1.001 + 1e-6;
                if sd[i] > reach || sd[i] < -reach {
                    continue;
                }
                sd[i] += (a * fbm(c.seed, salt, x / rg.scale, y / rg.scale, rg.octaves)) as f32;
            }
        }
        let mut inside: Vec<bool> = (0..n).map(|i| f.art[i] || sd[i] <= 0.0).collect();
        if let Some((k, false)) = c.fusion {
            // Bridge: an exact closing by distance fields; new cells take
            // the nearest body cell's attributes.
            let (d1, f1) = feature_transform(w, h, &inside, ax, 1.0);
            let outside: Vec<bool> = d1.iter().map(|&d| f64::from(d) > k).collect();
            let (d2, _) = feature_transform(w, h, &outside, ax, 1.0);
            for i in 0..n {
                if !inside[i] && f64::from(d1[i]) <= k && f64::from(d2[i]) > k && f1[i] != none {
                    let j = f1[i] as usize;
                    inside[i] = true;
                    (own[i], gk[i], sid[i]) = (own[j], gk[j], sid[j]);
                }
            }
        }
        if let Some(cv) = &c.cavity {
            self.cavities(cv, g, ax, t, &f.art, &mut inside);
        }
        if let Some(m) = &c.mask {
            let allowed = self.mask(m, src, f)?;
            for i in 0..n {
                if !allowed[i] {
                    inside[i] = false;
                }
            }
        }
        let mut vein = vec![false; n];
        if let Some(v) = &c.veins {
            self.veins(v, f, &inside, t, &mut vein, budget)?;
            for i in 0..n {
                match v.style {
                    1 if vein[i] => inside[i] = false,
                    2 if !vein[i] => inside[i] = false,
                    _ => {}
                }
            }
        }
        for i in 0..n {
            if f.art[i] || !inside[i] {
                own[i] = none;
            }
        }
        Ok(Body { own, gk, sid, vein })
    }

    /// c10 / c11: veins traced inside the body from seeded starts along
    /// the direction field (half-row steps, seeded forks), marking the
    /// growth cells they cross.
    fn veins(
        &self,
        v: &Veins,
        f: &Field,
        inside: &[bool],
        t: f64,
        vein: &mut [bool],
        budget: &mut Budget,
    ) -> Result<(), TransformError> {
        let g = &f.g;
        let (w, h) = (g.w, g.h);
        let ax = f.ax;
        let seed = self.cfg.seed;
        let len = v.length * t;
        if len <= 0.0 {
            return Ok(());
        }
        let nsalt = salt_of("tumours.flow");
        // The art box's centre: where radial veins point away from on the
        // seeds themselves.
        let (t0, t1, lo, hi) = f.abox;
        let cx = (t1 - t0) as f64 / 2.0 / g.cell as f64 * ax;
        let cy = f64::from(hi - lo) / 2.0;
        // Noise on the absolute lattice (tick / cell, pitch).
        let flow = |x: f64, y: f64| {
            let (c, r) = f.cr(x, y);
            let ux = (c + g.t0 as f64 / g.cell as f64) * ax;
            let uy = r + g.row0 as f64;
            2.0 * TAU * fbm(seed, nsalt, ux / v.scale, uy / v.scale, 2)
        };
        let dir = |x: f64, y: f64| -> f64 {
            let (cf, rf) = f.cr(x, y);
            let c = cf.round().clamp(0.0, (w - 1) as f64) as usize;
            let r = rf.round().clamp(0.0, (h - 1) as f64) as usize;
            let fi = f.feat[r * w + c];
            let radial = {
                let (mut dx, mut dy) = (x - cx, y - cy);
                if fi != u32::MAX {
                    let (sx, sy) = f.xy(fi as usize);
                    if (x - sx).hypot(y - sy) > 0.5 {
                        (dx, dy) = (x - sx, y - sy);
                    }
                }
                if dx.hypot(dy) < 1e-9 {
                    0.0
                } else {
                    dy.atan2(dx)
                }
            };
            let a = match v.field {
                0 => radial,
                1 => 0.0,
                2 => std::f64::consts::FRAC_PI_2,
                3 => v.angle,
                4 => radial + std::f64::consts::FRAC_PI_2,
                5 => flow(x, y),
                _ => steer(&f.att, x, y, radial, 1.0),
            };
            if v.noise > 0.0 && v.field != 5 {
                let b = flow(x, y);
                let (ux, uy) = (
                    (1.0 - v.noise) * a.cos() + v.noise * b.cos(),
                    (1.0 - v.noise) * a.sin() + v.noise * b.sin(),
                );
                if ux.hypot(uy) > 1e-12 {
                    return uy.atan2(ux);
                }
            }
            a
        };
        // The direction field is evaluated once per cell (at its centre) and
        // cached as a unit step: the walks revisit cells many times.
        let mut step: Vec<(f32, f32)> = vec![(f32::NAN, 0.0); w * h];
        let salt = salt_of("tumours.vein");
        let total = &mut budget.steps;
        for i in 0..w * h {
            if !inside[i] {
                continue;
            }
            let key = [g.tick_of(i % w), (i / w + g.row0) as u64];
            let mut r0 = stream(seed, salt, &key);
            if r0.unit() >= v.p {
                continue;
            }
            let (x0, y0) = f.xy(i);
            let mut stack = vec![(x0, y0, len, r0.next_u64())];
            while let Some((mut x, mut y, mut left, rs)) = stack.pop() {
                let mut rng = super::super::rng::Rng::new(rs);
                loop {
                    let (c, r) = f.cr(x, y);
                    let (c, r) = (c.round(), r.round());
                    if c < 0.0 || r < 0.0 || c >= w as f64 || r >= h as f64 {
                        break;
                    }
                    let j = r as usize * w + c as usize;
                    if !inside[j] {
                        break;
                    }
                    if !f.art[j] {
                        vein[j] = true;
                    }
                    if left <= 0.0 {
                        break;
                    }
                    let (sx, sy) = {
                        let e = &mut step[j];
                        if e.0.is_nan() {
                            let (cx, cy) = f.xy(j);
                            let a = dir(cx, cy);
                            *e = ((a.cos() * 0.5) as f32, (a.sin() * 0.5) as f32);
                        }
                        (f64::from(e.0), f64::from(e.1))
                    };
                    x += sx;
                    y += sy;
                    left -= 0.5;
                    *total += 1;
                    if *total > VEIN_CAP {
                        return Err(TransformError::TooLarge(
                            "The veins would take more than sixteen million steps. Lower Vein Density, the vein length or the forks, or select fewer notes.".into(),
                        ));
                    }
                    if v.fork > 0.0 && rng.unit() < v.fork {
                        let a = sy.atan2(sx);
                        let b = a + if rng.unit() < 0.5 { 0.6 } else { -0.6 };
                        stack.push((
                            x + b.cos() * 0.5,
                            y + b.sin() * 0.5,
                            left * 0.7,
                            rng.next_u64(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// One branch and its children (c05 / c06): tapered capsules stepping
    /// out from `at` with seeded wiggle, turned toward the attractors.
    #[allow(clippy::too_many_arguments)]
    fn grow_branch(
        &self,
        b: &Branch,
        s: &Site,
        f: &Field,
        t: f64,
        level: u32,
        at: (f64, f64),
        angle: f64,
        size: (f64, f64),
        rng: &mut super::super::rng::Rng,
        id: u32,
        prims: &mut Vec<Prim>,
        segs: &mut usize,
    ) -> Result<(), TransformError> {
        // Generation `level` grows during its share of the progress.
        let tau = (t * f64::from(b.depth) - f64::from(level - 1)).clamp(0.0, 1.0);
        if tau <= 0.0 {
            return Ok(());
        }
        let (len, wid) = size;
        let total = len * tau;
        let step = wid.max(0.5);
        let steps = (total / step).ceil().clamp(1.0, 256.0) as usize;
        let seg = total / steps as f64;
        let (mut x, mut y) = at;
        let mut a = angle;
        let gn = s.g.saturating_add(level as u8);
        let bias = self.cfg.attract.as_ref().map_or(0.0, |a| a.bias);
        for k in 0..steps {
            a += b.wiggle * (2.0 * rng.unit() - 1.0);
            if bias > 0.0 {
                a = steer(&f.att, x, y, a, bias);
            }
            let (nx, ny) = (x + a.cos() * seg, y + a.sin() * seg);
            let r0 = (wid * (1.0 - b.taper * k as f64 / steps as f64)).max(0.3);
            let r1 = (wid * (1.0 - b.taper * (k + 1) as f64 / steps as f64)).max(0.3);
            prims.push(Prim {
                shape: Shape::Capsule(x, y, nx, ny, r0, r1),
                rep: s.rep,
                g: gn,
                kind: KIND_BRANCH,
                id,
            });
            *segs += 1;
            if *segs > SEG_CAP || prims.len() > PRIM_CAP {
                return Err(cap_error());
            }
            (x, y) = (nx, ny);
        }
        if level < b.depth {
            let n = b.children;
            for ch in 0..n {
                let turn = if n > 1 {
                    b.fork * (2.0 * f64::from(ch) / f64::from(n - 1) - 1.0)
                } else if rng.unit() < 0.5 {
                    b.fork
                } else {
                    -b.fork
                };
                self.grow_branch(
                    b,
                    s,
                    f,
                    t,
                    level + 1,
                    (x, y),
                    a + turn,
                    (len * b.decay, wid * b.decay),
                    rng,
                    id,
                    prims,
                    segs,
                )?;
            }
        }
        Ok(())
    }

    /// c22: `count` daughters on the primary rim (the sites with the
    /// smallest seeded keys), then their own daughters for more levels.
    #[allow(clippy::too_many_arguments)]
    fn daughters(
        &self,
        d: &Daughters,
        rim: &[usize],
        site_info: &dyn Fn(usize) -> Option<Site>,
        f: &Field,
        t: f64,
        prims: &mut Vec<Prim>,
        _pid: &[u32],
    ) -> Result<(), TransformError> {
        let c = &self.cfg;
        let salt = salt_of("tumours.daughter");
        let mut keyed: Vec<(u64, usize)> = rim
            .iter()
            .map(|&i| {
                let key = [f.g.tick_of(i % f.g.w), (i / f.g.w + f.g.row0) as u64];
                (stream(c.seed, salt, &key).next_u64(), i)
            })
            .collect();
        let k = d.count.min(keyed.len());
        if k < keyed.len() {
            keyed.select_nth_unstable(k);
        }
        let mut chosen: Vec<(u64, usize)> = keyed[..k].to_vec();
        chosen.sort_unstable();
        let bias = c.attract.as_ref().map_or(0.0, |a| a.bias);
        let mut made = 0usize;
        for (hk, i) in chosen {
            let Some(s) = site_info(i) else { continue };
            let mut rng = stream(c.seed, salt, &[hk]);
            let base_r = if s.r > 0.0 { s.r } else { c.ry * c.scale };
            // (centre, radius, direction, level) of the mass to place next.
            let mut todo = vec![((s.x, s.y), 0.0, s.ny.atan2(s.nx), 1u32)];
            while let Some(((px, py), pr, dir, level)) = todo.pop() {
                let rd = base_r * d.scale.powi(level as i32) * t;
                if rd <= 0.25 {
                    continue;
                }
                let a = if bias > 0.0 {
                    steer(&f.att, px, py, dir, bias)
                } else {
                    dir
                };
                let dist = pr + d.gap * t + rd;
                let (cx, cy) = (px + a.cos() * dist, py + a.sin() * dist);
                let id = prims.len() as u32;
                let gn = s.g.saturating_add(level as u8);
                prims.push(Prim::disc(cx, cy, rd, s.rep, gn, KIND_DAUGHTER, id));
                if d.stalk > 0.0 {
                    let w = d.stalk / 2.0;
                    prims.push(Prim {
                        shape: Shape::Capsule(px, py, cx, cy, w, w),
                        rep: s.rep,
                        g: gn,
                        kind: KIND_DAUGHTER,
                        id,
                    });
                }
                made += 1;
                if made > DAUGHTER_CAP || prims.len() > PRIM_CAP {
                    return Err(cap_error());
                }
                if level < d.levels {
                    let kids = (d.count / 2).max(1);
                    for _ in 0..kids {
                        let turn = (2.0 * rng.unit() - 1.0) * 70f64.to_radians();
                        todo.push(((cx, cy), rd, a + turn, level + 1));
                    }
                }
            }
        }
        Ok(())
    }

    /// c12: round cavities deeper than the wall, or noise voids.
    fn cavities(&self, cv: &Cavity, g: &Grid, ax: f64, t: f64, art: &[bool], inside: &mut [bool]) {
        let (w, h) = (g.w, g.h);
        let n = w * h;
        let outside: Vec<bool> = inside.iter().map(|&b| !b).collect();
        let (depth, _) = feature_transform(w, h, &outside, ax, 1.0);
        let seed = self.cfg.seed;
        if cv.noise {
            let salt = salt_of("tumours.voids");
            for i in 0..n {
                if !inside[i] || art[i] || f64::from(depth[i]) < cv.wall + 1.0 {
                    continue;
                }
                let x = g.tick_of(i % w) as f64 / g.cell as f64 * ax;
                let y = (i / w + g.row0) as f64;
                let v = (fbm(seed, salt, x / cv.scale, y / cv.scale, 3) + 1.0) / 2.0;
                if v >= cv.threshold + (1.0 - cv.threshold) * (1.0 - t) {
                    inside[i] = false;
                }
            }
            return;
        }
        let salt = salt_of("tumours.cavity");
        // Seeded centres deep enough for their radius; each cell then
        // belongs to its nearest centre's cavity (a disc cut by the
        // centres' Voronoi cells: neighbouring cavities stay apart) —
        // O(cells) whatever the density.
        let mut centre = vec![false; n];
        let mut radius = vec![0f32; n];
        let mut any = false;
        for i in 0..n {
            if !inside[i] {
                continue;
            }
            let key = [g.tick_of(i % w), (i / w + g.row0) as u64];
            let mut rng = stream(seed, salt, &key);
            if rng.unit() >= cv.p {
                continue;
            }
            let r = (cv.min + (cv.max - cv.min) * rng.unit()) * t;
            if r > 0.0 && f64::from(depth[i]) >= cv.wall + r + 0.5 {
                centre[i] = true;
                radius[i] = r as f32;
                any = true;
            }
        }
        if !any {
            return;
        }
        let (d, near) = feature_transform(w, h, &centre, ax, 1.0);
        for j in 0..n {
            let k = near[j];
            if inside[j] && !art[j] && k != u32::MAX && d[j] <= radius[k as usize] {
                inside[j] = false;
            }
        }
        let _ = h;
    }

    /// The mask's inputs (checked before any grid is built, so a missing
    /// picture or an empty mask track is refused at once): the picture of
    /// the picture mode.
    fn mask_ready(
        &self,
        m: &Mask,
        src: &Source,
    ) -> Result<Option<std::sync::Arc<super::super::image_midi::Rgba>>, TransformError> {
        match m.mode {
            3 => match &self.ctx.picture {
                Some(Ok(p)) => Ok(Some(p.clone())),
                Some(Err(e)) => Err(TransformError::Invalid(e.clone())),
                None => Err(TransformError::Invalid(
                    "Choose a picture file for the mask (PNG, JPEG, BMP, GIF, WebP or TIFF)."
                        .into(),
                )),
            },
            2 if src.aux.first().is_none_or(Vec::is_empty) => Err(TransformError::Invalid(
                "The mask track has no notes. Choose another track (Mask tracks) or another mask."
                    .into(),
            )),
            _ => Ok(None),
        }
    }

    /// c24: the cells growth may cover.
    fn mask(&self, m: &Mask, src: &Source, f: &Field) -> Result<Vec<bool>, TransformError> {
        let g = &f.g;
        let (w, h) = (g.w, g.h);
        let (t0, t1, lo, hi) = f.abox;
        let span = (t1 - t0) as f64;
        let prange = f64::from(hi - lo);
        let mid = |c: usize| g.tick_of(c) as f64 + g.cell as f64 / 2.0;
        let mut v = vec![false; w * h];
        match m.mode {
            // The selection's box widened by the margin.
            0 | 3 => {
                let (a, b) = (t0 as f64 - m.margin * span, t1 as f64 + m.margin * span);
                let (pa, pb) = (
                    f64::from(lo) - m.margin * (prange + 1.0),
                    f64::from(hi) + m.margin * (prange + 1.0),
                );
                let pic = self.mask_ready(m, src)?;
                for r in 0..h {
                    let p = f64::from(g.pitch_of(r));
                    if p < pa || p > pb {
                        continue;
                    }
                    for c in 0..w {
                        let x = mid(c);
                        if x < a || x >= b {
                            continue;
                        }
                        v[r * w + c] = match &pic {
                            None => true,
                            Some(pic) => {
                                let u = (x - a) / (b - a);
                                let vv = (p - pa) / (pb - pa).max(1e-9);
                                let px = ((u * f64::from(pic.w)).floor() as u32).min(pic.w - 1);
                                let py =
                                    (((1.0 - vv) * f64::from(pic.h)).floor() as u32).min(pic.h - 1);
                                let q = pic.px[(py as usize) * pic.w as usize + px as usize];
                                let luma = (0.2126 * f64::from(q[0])
                                    + 0.7152 * f64::from(q[1])
                                    + 0.0722 * f64::from(q[2]))
                                    * f64::from(q[3])
                                    / (255.0 * 255.0);
                                luma >= m.threshold
                            }
                        };
                    }
                }
            }
            1 => {
                let (ra, rb) = (m.region.0.min(m.region.1), m.region.0.max(m.region.1));
                let (pa, pb) = (m.region.2.min(m.region.3), m.region.2.max(m.region.3));
                let (a, b) = (t0 as f64 + ra * span, t0 as f64 + rb * span);
                let (qa, qb) = (f64::from(lo) + pa * prange, f64::from(lo) + pb * prange);
                for r in 0..h {
                    let p = f64::from(g.pitch_of(r));
                    if p + 1e-9 < qa || p - 1e-9 > qb {
                        continue;
                    }
                    for c in 0..w {
                        let x = mid(c);
                        v[r * w + c] = x >= a && x < b;
                    }
                }
            }
            2 => {
                self.mask_ready(m, src)?;
                let aux: &[TNote] = src.aux.first().map_or(&[], Vec::as_slice);
                for n in aux {
                    let row = usize::from(n.pitch);
                    if row < g.row0 || row >= g.row0 + h {
                        continue;
                    }
                    if let Some((c0, c1)) = g.cols(n.start, n.end()) {
                        let r = g.row_of(n.pitch);
                        for cell in &mut v[r * w + c0..=r * w + c1] {
                            *cell = true;
                        }
                    }
                }
                if m.dilate > 0 {
                    // A box of the widening (cells along time, rows).
                    let k = m.dilate;
                    let s = sat(w, h, |i| u64::from(v[i]));
                    v = (0..w * h)
                        .map(|i| window(&s, w, h, i % w, i / w, k).0 > 0)
                        .collect();
                }
            }
            _ => {
                for r in 0..h {
                    let p = g.pitch_of(r);
                    if p >= m.keys.0 && p <= m.keys.1 {
                        for cell in &mut v[r * w..(r + 1) * w] {
                            *cell = true;
                        }
                    }
                }
            }
        }
        if m.invert {
            for x in &mut v {
                *x = !*x;
            }
        }
        Ok(v)
    }

    /// Every slot's plan.
    fn plan(&self, src: &Source) -> Result<Vec<SlotPlan>, TransformError> {
        if let Some(m) = &self.cfg.mask {
            self.mask_ready(m, src)?;
        }
        let sw = self.swept(src);
        by_slot(src)
            .into_iter()
            .map(|(_, idx)| self.slot_plan(src, idx, &sw))
            .collect()
    }

    fn count(&self, plans: &[SlotPlan]) -> u64 {
        let kept = if self.cfg.keep { self.cfg.copies() } else { 0 };
        plans
            .iter()
            .map(|p| p.pieces.len() as u64 + kept * p.idx.len() as u64)
            .sum()
    }
}

/// The lowest and highest pitch of some notes.
fn pitch_span(ns: &[TNote], idx: &[usize]) -> (u8, u8) {
    let lo = idx.iter().map(|&i| ns[i].pitch).min().unwrap_or(60);
    let hi = idx.iter().map(|&i| ns[i].pitch).max().unwrap_or(60);
    (lo, hi)
}

/// 8-connected components of the seed cells and their centroids (row
/// units): (component per cell — `u32::MAX` off the seeds —, centroids).
fn components(f: &Field, seeds: &[bool]) -> (Vec<u32>, Vec<(f64, f64)>) {
    let (w, h) = (f.g.w, f.g.h);
    let n = w * h;
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    for i in 0..n {
        if !seeds[i] {
            continue;
        }
        let (c, r) = (i % w, i / w);
        let join = |j: usize, p: &mut Vec<u32>| {
            if seeds[j] {
                let (a, b) = (find(p, i as u32), find(p, j as u32));
                if a != b {
                    p[a.max(b) as usize] = a.min(b);
                }
            }
        };
        if c > 0 {
            join(i - 1, &mut parent);
        }
        if r > 0 {
            join(i - w, &mut parent);
            if c > 0 {
                join(i - w - 1, &mut parent);
            }
            if c + 1 < w {
                join(i - w + 1, &mut parent);
            }
        }
    }
    let mut label = vec![u32::MAX; n];
    let mut ids: HashMap<u32, u32> = HashMap::new();
    let mut sums: Vec<(f64, f64, f64)> = Vec::new();
    for i in 0..n {
        if !seeds[i] {
            continue;
        }
        let root = find(&mut parent, i as u32);
        let next = ids.len() as u32;
        let k = *ids.entry(root).or_insert(next);
        if k as usize == sums.len() {
            sums.push((0.0, 0.0, 0.0));
        }
        let s = &mut sums[k as usize];
        let (x, y) = f.xy(i);
        s.0 += x;
        s.1 += y;
        s.2 += 1.0;
        label[i] = k;
    }
    (
        label,
        sums.into_iter().map(|s| (s.0 / s.2, s.1 / s.2)).collect(),
    )
}

/// A density peak for Bulge Pressure: centre (row units), pressure, reach.
#[derive(Clone, Copy, Debug)]
struct Peak {
    x: f64,
    y: f64,
    p: f64,
    sigma: f64,
}

/// The density peaks of the art: the densest cell of each block of the
/// window's size that is at least as dense as its neighbouring blocks'
/// and a quarter of the densest; the strongest `peaks` of them.
fn density_peaks(f: &Field, b: &Bulge) -> Vec<Peak> {
    let g = &f.g;
    let (w, h) = (g.w, g.h);
    let s = sat(w, h, |i| u64::from(g.count[i]));
    let bs = 2 * b.window + 1;
    let (bw, bh) = (w.div_ceil(bs), h.div_ceil(bs));
    // Per block: (density, cell).
    let mut best = vec![(0f64, usize::MAX); bw * bh];
    for i in 0..w * h {
        if g.count[i] == 0 {
            continue;
        }
        let (c, r) = (i % w, i / w);
        let (sum, n) = window(&s, w, h, c, r, b.window);
        let d = sum as f64 / n as f64;
        let k = (r / bs) * bw + c / bs;
        if d > best[k].0 {
            best[k] = (d, i);
        }
    }
    let top = best.iter().map(|b| b.0).fold(0.0, f64::max);
    if top <= 0.0 {
        return Vec::new();
    }
    let mut cands: Vec<(f64, usize)> = Vec::new();
    for by in 0..bh {
        for bx in 0..bw {
            let (d, i) = best[by * bw + bx];
            if i == usize::MAX || d < 0.25 * top {
                continue;
            }
            let mut peak = true;
            for (dx, dy) in [
                (-1i64, -1i64),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let (nx, ny) = (bx as i64 + dx, by as i64 + dy);
                if nx < 0 || ny < 0 || nx >= bw as i64 || ny >= bh as i64 {
                    continue;
                }
                let o = best[ny as usize * bw + nx as usize];
                // Ties go to the earlier block.
                if o.0 > d || (o.0 == d && o.1 < i) {
                    peak = false;
                    break;
                }
            }
            if peak {
                cands.push((d, i));
            }
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    cands.truncate(b.peaks);
    cands
        .into_iter()
        .map(|(d, i)| Peak {
            x: f.xy(i).0,
            y: f.xy(i).1,
            p: b.pressure * (d / top).powf(b.gamma),
            sigma: b.reach,
        })
        .collect()
}

/// Bulge Pressure's domain warp: the point the field is read at.
fn warp(peaks: &[Peak], x: f64, y: f64) -> (f64, f64) {
    let (mut qx, mut qy) = (x, y);
    for k in peaks {
        let (dx, dy) = (x - k.x, y - k.y);
        let d2 = dx * dx + dy * dy;
        if d2 > 16.0 * k.sigma * k.sigma {
            continue;
        }
        let e = (-d2 / (2.0 * k.sigma * k.sigma)).exp();
        let pull = 1.0 - 1.0 / (1.0 + k.p * e);
        qx -= dx * pull;
        qy -= dy * pull;
    }
    (qx, qy)
}

/// The distance at a point between cells (bilinear; the nearest cell's
/// where a corner has no seed) and the nearest cell's seed.
fn sample(f: &Field, x: f64, y: f64) -> (f64, u32) {
    let (w, h) = (f.g.w, f.g.h);
    let (cx, cy) = f.cr(x, y);
    let cx = cx.clamp(0.0, (w - 1) as f64);
    let cy = cy.clamp(0.0, (h - 1) as f64);
    let (c0, r0) = (cx.floor() as usize, cy.floor() as usize);
    let (c1, r1) = ((c0 + 1).min(w - 1), (r0 + 1).min(h - 1));
    let (fx, fy) = (cx - c0 as f64, cy - r0 as f64);
    let near = cy.round() as usize * w + cx.round() as usize;
    let d = |c: usize, r: usize| f64::from(f.dist[r * w + c]);
    let ds = [d(c0, r0), d(c1, r0), d(c0, r1), d(c1, r1)];
    if ds.iter().any(|v| !v.is_finite()) {
        return (f64::from(f.dist[near]), f.feat[near]);
    }
    let top = ds[0] + (ds[1] - ds[0]) * fx;
    let bot = ds[2] + (ds[3] - ds[2]) * fx;
    (top + (bot - top) * fy, f.feat[near])
}

/// Most primitives (branch segments, nodule discs, daughters) of a slot.
const PRIM_CAP: usize = 1 << 20;
/// Most branch segments of a slot.
const SEG_CAP: usize = 1 << 20;
/// Most daughters of a slot.
const DAUGHTER_CAP: usize = 4096;
/// Most cells the primitives' rasterisation may visit in a slot (all its
/// stages).
const AREA_CAP: u64 = 1 << 27;

const KIND_BASE: u8 = 0;
const KIND_BRANCH: u8 = 1;
const KIND_NODULE: u8 = 2;
const KIND_DAUGHTER: u8 = 3;
const KIND_VEIN: u8 = 4;
/// Most vein steps of a slot (all its stages).
const VEIN_CAP: usize = 1 << 24;

/// The work one slot may still do (all its stages together).
#[derive(Default)]
struct Budget {
    /// Cells the primitives' rasterisation visited.
    visits: u64,
    /// Vein steps taken.
    steps: usize,
}

fn cap_error() -> TransformError {
    TransformError::TooLarge(
        "The tumour would build more than a million branch segments, nodules or daughters (or a thousand daughters). Lower Branch Probability, Branch Depth, Nodule Frequency or the daughters, or select fewer notes.".into(),
    )
}

/// Generation (1 … 255) and kind packed.
fn gen_kind(g: f64, kind: u8) -> u16 {
    (g.clamp(1.0, 255.0) as u16) | u16::from(kind) << 8
}

/// Every growth cell's result.
struct Body {
    /// The art note (index) of each growth cell (`u32::MAX`: none).
    own: Vec<u32>,
    /// Generation (low byte) and kind (high byte).
    gk: Vec<u16>,
    /// The structure a cell belongs to: 0 the base, 1 + a primitive's
    /// structure id.
    sid: Vec<u32>,
    /// Vein cells (c10 / c11).
    vein: Vec<bool>,
}

/// A contour site primitives grow from.
#[derive(Clone, Copy, Debug)]
struct Site {
    x: f64,
    y: f64,
    /// Outward normal.
    nx: f64,
    ny: f64,
    rep: u32,
    g: u8,
    /// Its absolute place (tick, row): what its seeded decisions hang on.
    key: [u64; 2],
    /// The radius at its seed.
    r: f64,
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    Disc(f64, f64, f64),
    /// From (x, y) to (x, y), radius at each end.
    Capsule(f64, f64, f64, f64, f64, f64),
}

#[derive(Clone, Copy, Debug)]
struct Prim {
    shape: Shape,
    rep: u32,
    g: u8,
    kind: u8,
    id: u32,
}

impl Prim {
    fn disc(x: f64, y: f64, r: f64, rep: u32, g: u8, kind: u8, id: u32) -> Prim {
        Prim {
            shape: Shape::Disc(x, y, r),
            rep,
            g,
            kind,
            id,
        }
    }

    /// Signed distance from (x, y).
    #[inline]
    fn sd(&self, x: f64, y: f64) -> f64 {
        match self.shape {
            Shape::Disc(cx, cy, r) => ((x - cx) * (x - cx) + (y - cy) * (y - cy)).sqrt() - r,
            Shape::Capsule(ax, ay, bx, by, ra, rb) => {
                let (dx, dy) = (bx - ax, by - ay);
                let l2 = dx * dx + dy * dy;
                let k = if l2 > 0.0 {
                    (((x - ax) * dx + (y - ay) * dy) / l2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (ex, ey) = (x - ax - k * dx, y - ay - k * dy);
                (ex * ex + ey * ey).sqrt() - (ra + (rb - ra) * k)
            }
        }
    }

    /// Bounding box (row units).
    fn bbox(&self) -> (f64, f64, f64, f64) {
        match self.shape {
            Shape::Disc(cx, cy, r) => (cx - r, cx + r, cy - r, cy + r),
            Shape::Capsule(ax, ay, bx, by, ra, rb) => {
                let r = ra.max(rb);
                (
                    ax.min(bx) - r,
                    ax.max(bx) + r,
                    ay.min(by) - r,
                    ay.max(by) + r,
                )
            }
        }
    }
}

/// Rasterises `prims[from..]` into the smallest signed distance per cell
/// and the primitive giving it.
fn raster(
    f: &Field,
    prims: &[Prim],
    from: usize,
    psd: &mut [f32],
    pid: &mut [u32],
    budget: &mut Budget,
) -> Result<(), TransformError> {
    let (w, h) = (f.g.w, f.g.h);
    // Each primitive's box clipped to the grid (cells), counted before any
    // is visited: a request over the cap is refused before the work.
    let cells = |p: &Prim| -> Option<(usize, usize, usize, usize)> {
        let (x0, x1, y0, y1) = p.bbox();
        let (c0, r0) = f.cr(x0, y0);
        let (c1, r1) = f.cr(x1, y1);
        if c1 < 0.0 || r1 < 0.0 || c0 > (w - 1) as f64 || r0 > (h - 1) as f64 {
            return None;
        }
        Some((
            c0.floor().max(0.0) as usize,
            (c1.ceil() as usize).min(w - 1),
            r0.floor().max(0.0) as usize,
            (r1.ceil() as usize).min(h - 1),
        ))
    };
    let mut visits = budget.visits;
    for p in &prims[from.min(prims.len())..] {
        if let Some((c0, c1, r0, r1)) = cells(p) {
            visits += ((c1 + 1 - c0) * (r1 + 1 - r0)) as u64;
            if visits > AREA_CAP {
                return Err(TransformError::TooLarge(
                    "The tumour's branches, nodules and daughters would cover too many cells to compute. Lower their sizes or counts, or select fewer notes.".into(),
                ));
            }
        }
    }
    budget.visits = visits;
    for (k, p) in prims.iter().enumerate().skip(from) {
        let Some((c0, c1, r0, r1)) = cells(p) else {
            continue;
        };
        // The cells' coordinates as Field::xy gives them, row by row.
        for r in r0..=r1 {
            let y = r as f64 - f.oy;
            let row = r * w;
            for c in c0..=c1 {
                let x = (c as f64 - f.ox) * f.ax;
                let d = p.sd(x, y) as f32;
                let i = row + c;
                if d < psd[i] {
                    psd[i] = d;
                    pid[i] = k as u32;
                }
            }
        }
    }
    Ok(())
}

/// Cells inside the field (≤ 0, or art) with a 4-neighbour outside: the
/// contour, in raster order.
fn contour(w: usize, h: usize, sd: &[f32], art: &[bool]) -> Vec<usize> {
    let ins = |i: usize| art[i] || sd[i] <= 0.0;
    (0..w * h)
        .filter(|&i| {
            if !ins(i) {
                return false;
            }
            let (c, r) = (i % w, i / w);
            (c > 0 && !ins(i - 1))
                || (c + 1 < w && !ins(i + 1))
                || (r > 0 && !ins(i - w))
                || (r + 1 < h && !ins(i + w))
        })
        .collect()
}

/// The field's outward gradient at cell `i` (unit), if it has one.
fn gradient(sd: &[f32], w: usize, h: usize, i: usize, ax: f64) -> Option<(f64, f64)> {
    let (c, r) = (i % w, i / w);
    let at = |j: usize| {
        let v = f64::from(sd[j]);
        if v.is_finite() { v } else { 1e9 }
    };
    let gx = if c > 0 && c + 1 < w {
        (at(i + 1) - at(i - 1)) / (2.0 * ax)
    } else {
        0.0
    };
    let gy = if r > 0 && r + 1 < h {
        (at(i + w) - at(i - w)) / 2.0
    } else {
        0.0
    };
    let l = gx.hypot(gy);
    (l > 1e-12 && l.is_finite()).then(|| (gx / l, gy / l))
}

/// A direction turned toward the attractors (away from repulsors) by
/// `bias` (0 – 1).
fn steer(att: &[(f64, f64, f64)], x: f64, y: f64, angle: f64, bias: f64) -> f64 {
    let (mut vx, mut vy, mut wsum) = (0.0, 0.0, 0.0);
    for &(px, py, s) in att {
        let (dx, dy) = (px - x, py - y);
        let l = dx.hypot(dy);
        if l > 1e-9 {
            vx += s * dx / l;
            vy += s * dy / l;
            wsum += s.abs();
        }
    }
    if wsum <= 0.0 {
        return angle;
    }
    let (ux, uy) = (
        angle.cos() * (1.0 - bias) + bias * vx / wsum,
        angle.sin() * (1.0 - bias) + bias * vy / wsum,
    );
    if ux.hypot(uy) < 1e-12 {
        angle
    } else {
        uy.atan2(ux)
    }
}

/// The polynomial smooth minimum of `a` and `b` with blend `k`.
fn smin(a: f64, b: f64, k: f64) -> f64 {
    if !a.is_finite() || !b.is_finite() {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k / 4.0
}

/// Seeded value noise in [−1, 1] (smoothstep between hashed lattice
/// values).
fn vnoise(seed: u64, salt: u64, x: f64, y: f64) -> f64 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let s = |v: f64| v * v * (3.0 - 2.0 * v);
    let at = |i: f64, j: f64| {
        let v = super::super::shared::seeded_stream::hash(&[
            seed,
            salt,
            i as i64 as u64,
            j as i64 as u64,
        ]);
        (v >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let (a, b, c, d) = (
        at(x0, y0),
        at(x0 + 1.0, y0),
        at(x0, y0 + 1.0),
        at(x0 + 1.0, y0 + 1.0),
    );
    let (sx, sy) = (s(fx), s(fy));
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

/// Fractal noise: `octaves` of [`vnoise`], each twice as fine and half as
/// strong, normalised to [−1, 1].
fn fbm(seed: u64, salt: u64, x: f64, y: f64, octaves: u32) -> f64 {
    let (mut sum, mut norm, mut amp, mut fq) = (0.0, 0.0, 1.0, 1.0);
    for o in 0..octaves.max(1) {
        sum += amp * vnoise(seed, salt.wrapping_add(u64::from(o)), x * fq, y * fq);
        norm += amp;
        amp *= 0.5;
        fq *= 2.0;
    }
    sum / norm
}

/// Reads the mask tracks' notes (c24 Track notes) as auxiliary layer 0.
pub fn prepare(
    doc: &crate::model::PlumDocument,
    scopes: &[super::super::TrackScope],
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("c24_mask_boundary_constraint") || p.choice("c24_mask_boundary_constraint.mode") != 2
    {
        return Ok(());
    }
    // B2-F16: counted from the track the edited clip sits on, read as the
    // song shows it on the edited notes' ticks.
    let Some((active, shift)) = super::super::source::edit_place(scopes, ctx) else {
        return Ok(());
    };
    let list = p.list("c24_mask_boundary_constraint.tracks");
    let idx: Vec<usize> = if list.is_empty() {
        vec![active + 1]
    } else {
        list.iter()
            .filter_map(|&n| usize::try_from(n - 1).ok())
            .collect()
    };
    let idx: Vec<usize> = idx
        .into_iter()
        .filter(|&i| doc.track(i).is_some())
        .collect();
    if idx.is_empty() {
        return Err(TransformError::Invalid(
            "Choose the mask tracks: no track with those numbers exists (empty: the track after the edited one).".into(),
        ));
    }
    let notes: Vec<TNote> = idx
        .into_iter()
        .flat_map(|i| Source::placed_layer(doc, i, u16::MAX, limit, shift))
        .collect();
    src.aux = std::sync::Arc::new(vec![notes]);
    Ok(())
}

impl Kernel for Tumours {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        let n = src.notes.len() as u64;
        let loose = match self.growth {
            Growth::Generate(m) => m,
            _ => u64::MAX,
        };
        if loose <= Limits::default().warn_notes {
            return Estimate::for_notes(n, loose);
        }
        // The plan (or its refusal) is kept for the run that follows.
        let plan = self.plan(src);
        let count = match &plan {
            Ok(plans) => self.count(plans).min(loose),
            Err(_) => n,
        };
        if let Ok(mut c) = self.cache.lock() {
            *c = Some((fingerprint(src), plan));
        }
        Estimate::for_notes(n, count)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let cached = self.cache.lock().ok().and_then(|mut c| c.take());
        let plans = match cached {
            Some((key, plans)) if key == fingerprint(src) => plans?,
            _ => self.plan(src)?,
        };
        let gap = self.cfg.evo.as_ref().map_or(0, |e| e.gap);
        for plan in &plans {
            // Stages in sequence: each one after the previous, the art's
            // span plus the gap apart.
            let every = i128::from(plan.span) + i128::from(gap);
            if self.cfg.keep {
                for k in 0..self.cfg.copies() {
                    // V09-0143: the evolution stage (the kept art's copy k
                    // stands beside stage k).
                    out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
                    for &i in &plan.idx {
                        let n = &src.notes[i];
                        let ok = if k == 0 {
                            out.push(*n)
                        } else {
                            out.emit(
                                i128::from(n.start) + i128::from(k) * every,
                                i128::from(n.dur),
                                i64::from(n.pitch),
                                i64::from(n.vel),
                                n,
                            )
                        };
                        if !ok {
                            return out.check();
                        }
                    }
                }
            }
            for p in &plan.pieces {
                out.set_dim(u32::from(p.stage));
                let n = &src.notes[p.rep as usize];
                let mut t = *n;
                (t.sub, t.ch) = (p.sub, p.ch);
                let s = i128::from(plan.t0)
                    + i128::from(p.c0) * i128::from(plan.cell)
                    + i128::from(p.stage) * every;
                let len = i128::from(p.c1 - p.c0) * i128::from(plan.cell);
                let len = ((len as f64) * f64::from(p.gate) / 10_000.0)
                    .round()
                    .max(1.0) as i128;
                if !out.emit(s, len, i64::from(p.row), i64::from(p.vel), &t) {
                    return out.check();
                }
            }
        }
        out.check()
    }
}

fn tumours_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let cfg = Cfg::read(p, b.ctx);
    let mut k = Tumours {
        cfg,
        tracks: b.tracks.to_vec(),
        ctx: b.ctx.clone(),
        growth: Growth::Map,
        cache: Mutex::new(None),
    };
    // The loose bound: every slot's grid fully grown, plus the kept art.
    let s = b.stats;
    let rows = b.ctx.keys.keys();
    let scale_hi = match b.ctx.sweeps.as_deref() {
        Some(sw) if sw.drives("scale") => sw.float_bounds("scale", k.cfg.scale * 100.0).1 / 100.0,
        _ => k.cfg.scale,
    };
    let pad = k.pad_cells(scale_hi.clamp(0.0, 4.0));
    let span = s.max_end.saturating_sub(s.min_start).max(1);
    let cell = k.cell_for(span, rows, pad);
    let cols = (span.div_ceil(cell) as usize + 2 * pad).min((MAX_CELLS / rows.max(1)).max(8));
    let slots = b.tracks.len().max(1) as u64;
    let cells = (cols * rows) as u64;
    let kept = if k.cfg.keep { s.count as u64 } else { 0 };
    let copies = k.cfg.copies();
    k.growth = Growth::Generate(
        cells
            .saturating_mul(slots)
            .saturating_add(kept)
            .saturating_mul(copies),
    );
    boxed(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uneven_lobes_pruned_to_their_neighbours_equal_the_maximum_over_all_lobes() {
        for k in 1..=32u32 {
            for (amount, sharp) in [(1.0, 0.5), (0.4, 2.0), (1.0, 16.0), (0.01, 1.0)] {
                let l = Lobes {
                    k,
                    amount: 1.0,
                    angle: 0.7,
                    sharp,
                };
                let a = Asym { amount, skew: 0.0 };
                for comp in 0..6u64 {
                    let t = lobe_table(&l, &a, 99, comp);
                    for s in 0..720 {
                        let theta = -PI + TAU * f64::from(s) / 720.0 + 1e-3 * comp as f64;
                        let full = t
                            .iter()
                            .map(|&(amp, centre, width)| {
                                let d = (theta - centre + PI).rem_euclid(TAU) - PI;
                                let x = (d.abs() * f64::from(k) / width).min(PI);
                                amp * ((1.0 + x.cos()) / 2.0).powf(sharp)
                            })
                            .fold(0.0, f64::max);
                        let got = lobe_shape(&l, Some(&t), theta);
                        assert_eq!(got.to_bits(), full.to_bits(), "k {k} a {amount} θ {theta}");
                    }
                }
            }
        }
    }
}
