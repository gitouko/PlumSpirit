//! Procedural Growth / Topology (§161, `gen.growth`; `docs/GENERATORS.md`
//! "Procedural Growth / Topology"): the fifth Advanced generator — branches
//! grown tip by tip in a unit square (the domain), step by step, steered by
//! noise, fields and their neighbours, drawn by the shared path pen. The
//! base: one seed at the domain's left edge, middle height, heading along
//! time; 60 iterations of a 1.5 % step, ±12° of seeded jitter a step, a branch
//! with 8 % probability a step (20° – 50° to either side); a tip leaving the
//! domain stops. The drawing is stretched over the box (the fields and the
//! mask of c21 – c23 lay the domain on the box instead).
//!
//! Each iteration visits the live tips in birth order: it steers the tip
//! (noise, attraction, repulsion, neighbours, the track and image fields),
//! moves it a step (slowed by the fields), checks the domain / mask, the
//! collisions, merges and loops, records the segment, may branch (density
//! target, mutation, species), ages it. Afterwards decay and pruning remove
//! old segments and short branches; copies (c24) and stages (c25) are laid
//! out; everything is drawn as strokes, a piece per tip, its thickness,
//! velocity and gate evolving with age.
//!
//! **Bounded**: at most [`TIP_CAP`] live tips (branches beyond are not
//! born), [`MAX_ITER`] iterations, [`QUERY_CAP`] points examined per
//! neighbour query — the work is at most iterations × tips × queries.

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::hash;
use super::super::source::Source;
use super::super::{Kernel, TransformError};
use super::draw::{Aff, BoxG, Gb, Pt, positions};
use super::path::{PIECE_CAP, PathStyle, Scratch, curve_along, measure, too_many_pieces};
use super::pen::{Brush, Draw};
use super::{Frame, Shapes};
use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};
use std::sync::OnceLock;

pub const GROWTH: ToolDef = ToolDef {
    id: "gen.growth",
    spec: growth_spec,
    build: growth_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_seed_points", "Seed Points", 10),
    ("c02_iterations", "Iterations", 20),
    ("c03_growth_step", "Growth Step", 30),
    ("c04_branch_probability", "Branch Probability", 40),
    ("c05_branch_angle_range", "Branch Angle Range", 50),
    ("c06_attraction_field", "Attraction Field", 60),
    ("c07_repulsion_field", "Repulsion Field", 70),
    ("c08_neighbor_radius", "Neighbor Radius", 80),
    ("c09_density_target", "Density Target", 90),
    ("c10_collision_avoidance", "Collision Avoidance", 100),
    ("c11_branch_merge", "Branch Merge", 110),
    ("c12_loop_formation", "Loop Formation", 120),
    ("c13_aging_decay", "Aging / Decay", 130),
    ("c14_branch_pruning", "Branch Pruning", 140),
    ("c15_thickness_evolution", "Thickness Evolution", 150),
    ("c16_velocity_evolution", "Velocity Evolution", 160),
    ("c17_gate_evolution", "Gate Evolution", 170),
    ("c18_subchannel_species", "Subchannel Species", 180),
    ("c19_mutation_rate", "Mutation Rate", 190),
    ("c20_directional_noise", "Directional Noise", 200),
    ("c21_external_track_field", "External Track Field", 210),
    ("c22_image_field", "Image Field", 220),
    ("c23_boundary_mask", "Boundary / Mask", 230),
    (
        "c24_symmetry_mirrored_growth",
        "Symmetry / Mirrored Growth",
        240,
    ),
    (
        "c25_evolution_timeline_saved_growth_stages",
        "Evolution Timeline / Saved Growth Stages",
        250,
    ),
];

/// The base.
pub const BASE_ITER: u32 = 60;
pub const BASE_STEP: f64 = 0.015;
pub const BASE_BRANCH: f64 = 0.08;
pub const BASE_ANGLES: (f64, f64) = (20.0, 50.0);
pub const BASE_JITTER: f64 = 12.0;
/// Most live tips (branches beyond are not born).
pub const TIP_CAP: usize = 1024;
/// Most iterations.
pub const MAX_ITER: u32 = 1000;
/// Most points examined by one neighbour query.
pub const QUERY_CAP: usize = 256;
/// The density target's radius without c08 (domain shares).
const DENSITY_RADIUS: f64 = 0.06;
/// A child's step stays within this factor of the base step.
const STEP_SPAN: f64 = 4.0;
/// The field grid's cells per side (c21).
const FIELD: usize = 64;
/// The image c22 starts with: brightness digits, rows top to bottom.
pub const IMAGE: &str = "0000000000000000/0000111111110000/0011100000011100/0110000000000110/0110000000000110/0011100000011100/0000111111110000/0000000000000000";

const MASKS: &[&str] = &["Ellipse", "Rectangle", "Diamond"];
const SYMS: &[&str] = &[
    "Pitch mirror",
    "Time mirror",
    "Both mirrors",
    "Radial copies",
];
const STAGES: &[&str] = &["Every stage along time", "One saved stage"];

// Random streams (tags).
const T_JIT: u64 = 0x4A49_5454;
const T_BR: u64 = 0x4252_414E;
const T_ANG: u64 = 0x414E_474C;
const T_SIDE: u64 = 0x5349_4445;
const T_MUT: u64 = 0x4D55_5441;
const T_SPEC: u64 = 0x5350_4543;
const T_NOISE: u64 = 0x4E4F_4953;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const GROWTH_HELP: &[(&str, &str)] = &[
    (
        "c01_seed_points",
        "Starts growth from points you place instead of one seed at the box's left edge.",
    ),
    (
        "c02_iterations",
        "Sets how many growth steps every tip takes instead of 60.",
    ),
    (
        "c02_iterations.n",
        "How many growth steps are taken; more steps grow a bigger network.",
    ),
    ("c03_growth_step", "Sets how far a tip moves each step."),
    (
        "c03_growth_step.step",
        "How far a tip moves each step, as a share of the box.",
    ),
    (
        "c04_branch_probability",
        "Sets how often a tip splits off a new branch.",
    ),
    (
        "c04_branch_probability.p",
        "The chance that a tip branches at each step.",
    ),
    (
        "c05_branch_angle_range",
        "Sets the range of angles new branches turn by.",
    ),
    (
        "c05_branch_angle_range.min",
        "The smallest angle a new branch turns by, in degrees.",
    ),
    (
        "c06_attraction_field",
        "Makes the tips grow toward points you place.",
    ),
    (
        "c06_attraction_field.points",
        "The attracting points as x, y pairs in thousandths of the box.",
    ),
    (
        "c07_repulsion_field",
        "Makes the tips grow away from points you place.",
    ),
    (
        "c07_repulsion_field.points",
        "The repelling points as x, y pairs in thousandths of the box.",
    ),
    (
        "c07_repulsion_field.strength",
        "How strongly the tips turn away from the repelling points.",
    ),
    (
        "c07_repulsion_field.radius",
        "How far the repelling points reach, as a share of the box.",
    ),
    (
        "c08_neighbor_radius",
        "Makes the tips keep away from other branches nearby.",
    ),
    (
        "c08_neighbor_radius.strength",
        "How strongly the tips turn away from nearby branches.",
    ),
    (
        "c09_density_target",
        "Keeps the growth near a target density: tips branch less where it is crowded and more where it is sparse.",
    ),
    (
        "c10_collision_avoidance",
        "Steers tips around other branches, stopping them when there is no way past.",
    ),
    (
        "c11_branch_merge",
        "Joins a tip into another branch it comes close to, and stops it there.",
    ),
    (
        "c12_loop_formation",
        "Bridges a tip once to another branch it comes close to, making loops, and lets it grow on.",
    ),
    (
        "c13_aging_decay",
        "Makes tips stop growing after a while and old parts of the growth fade away.",
    ),
    (
        "c13_aging_decay.lifespan",
        "How many steps a tip grows before it stops.",
    ),
    (
        "c13_aging_decay.decay",
        "How many steps a segment lasts before it is removed.",
    ),
    (
        "c14_branch_pruning",
        "Removes branches that are too short or branch off too many times.",
    ),
    (
        "c14_branch_pruning.min",
        "The shortest branch kept, in steps.",
    ),
    (
        "c14_branch_pruning.order",
        "How many times branches may split from branches and still be kept (0 = only the main stems).",
    ),
    (
        "c15_thickness_evolution",
        "Draws older growth thicker than newer growth.",
    ),
    (
        "c15_thickness_evolution.oldest",
        "How many rows thick the oldest growth is.",
    ),
    (
        "c15_thickness_evolution.newest",
        "How many rows thick the newest growth is.",
    ),
    (
        "c16_velocity_evolution",
        "Changes the velocity from the oldest growth to the newest.",
    ),
    (
        "c16_velocity_evolution.oldest",
        "The velocity of the oldest growth.",
    ),
    (
        "c16_velocity_evolution.newest",
        "The velocity of the newest growth.",
    ),
    (
        "c17_gate_evolution",
        "Changes the note lengths from the oldest growth to the newest.",
    ),
    (
        "c17_gate_evolution.oldest",
        "The gate (share of each note's length) of the oldest growth.",
    ),
    (
        "c17_gate_evolution.newest",
        "The gate (share of each note's length) of the newest growth.",
    ),
    (
        "c18_subchannel_species",
        "Divides the branches into species, each drawn on its own subchannel (color).",
    ),
    (
        "c18_subchannel_species.step",
        "How many subchannel positions each species moves from the previous one.",
    ),
    (
        "c19_mutation_rate",
        "Lets new branches change their step, wobble and species at random (seeded).",
    ),
    (
        "c20_directional_noise",
        "Makes the tips meander smoothly instead of wobbling step by step.",
    ),
    (
        "c20_directional_noise.amplitude",
        "How far the meandering turns the tips, in degrees.",
    ),
    (
        "c21_external_track_field",
        "Makes the tips grow toward the notes of tracks, and slows them away from those notes.",
    ),
    (
        "c21_external_track_field.strength",
        "How strongly the tips turn toward the tracks' notes.",
    ),
    (
        "c21_external_track_field.slow",
        "How fast the tips grow far from any note, as a share of their normal speed.",
    ),
    (
        "c22_image_field",
        "Makes the tips grow toward the bright parts of a picture, and faster there.",
    ),
    (
        "c22_image_field.strength",
        "How strongly the tips turn toward brighter parts.",
    ),
    (
        "c23_boundary_mask",
        "Keeps the growth inside a shape laid over the box.",
    ),
    (
        "c23_boundary_mask.shape",
        "The shape the growth stays in: an ellipse, a rectangle or a diamond.",
    ),
    (
        "c24_symmetry_mirrored_growth",
        "Adds mirrored or turned copies of the growth.",
    ),
    (
        "c24_symmetry_mirrored_growth.mode",
        "Mirror the growth in pitch, in time, both ways, or copy it turned around the center.",
    ),
    (
        "c24_symmetry_mirrored_growth.copies",
        "How many turned copies go around the center, used with Radial Copies.",
    ),
    (
        "c25_evolution_timeline_saved_growth_stages",
        "Shows the growth at several stages, side by side or one saved stage.",
    ),
    (
        "c25_evolution_timeline_saved_growth_stages.stages",
        "How many stages the growth is divided into.",
    ),
    (
        "c25_evolution_timeline_saved_growth_stages.mode",
        "Every Stage Along Time draws the stages one after another; One Saved Stage draws only the one chosen below.",
    ),
    (
        "c25_evolution_timeline_saved_growth_stages.stage",
        "Which stage is drawn with One Saved Stage.",
    ),
];

fn growth_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        flag("c01_seed_points", "Seed Points", false),
        list("c01_seed_points.points", "Seeds (x, y in ‰ of the box)", 0, 1000, 64, &[20, 250, 20, 750, 500, 500])
            .help("Pairs of x, y: each seed starts a tip heading along time."),
        flag("c02_iterations", "Iterations", false),
        int("c02_iterations.n", "Iterations", 1, i64::from(MAX_ITER), 120),
        flag("c03_growth_step", "Growth Step", false),
        float("c03_growth_step.step", "Step (% of the domain)", 0.1, 20.0, 5.0, "%"),
        flag("c04_branch_probability", "Branch Probability", false),
        float("c04_branch_probability.p", "Branch probability per step", 0.0, 100.0, 20.0, "%"),
        flag("c05_branch_angle_range", "Branch Angle Range", false),
        float("c05_branch_angle_range.min", "From", 0.0, 180.0, 30.0, "°"),
        float("c05_branch_angle_range.max", "To", 0.0, 180.0, 90.0, "°")
            .help("A branch turns by a seeded angle in this range, to either side."),
        flag("c06_attraction_field", "Attraction Field", false),
        list("c06_attraction_field.points", "Attractors (x, y in ‰)", 0, 1000, 32, &[950, 500]),
        float("c06_attraction_field.strength", "Strength", 0.0, 100.0, 30.0, "%")
            .help("Each step the heading turns towards the nearest attractor by strength × sin(angle between)."),
        flag("c07_repulsion_field", "Repulsion Field", false),
        list("c07_repulsion_field.points", "Repellers (x, y in ‰)", 0, 1000, 32, &[500, 500]),
        float("c07_repulsion_field.strength", "Strength", 0.0, 100.0, 40.0, "%"),
        float("c07_repulsion_field.radius", "Reach (% of the domain)", 1.0, 100.0, 40.0, "%"),
        flag("c08_neighbor_radius", "Neighbor Radius", false),
        float("c08_neighbor_radius.radius", "Radius (% of the domain)", 0.5, 30.0, 6.0, "%")
            .help("Tips turn away from other branches' points within this radius (and the density target counts within it)."),
        float("c08_neighbor_radius.strength", "Separation", 0.0, 100.0, 50.0, "%"),
        flag("c09_density_target", "Density Target", false),
        int("c09_density_target.target", "Points per neighborhood", 1, 256, 6)
            .help("Above it a tip stops branching (above twice it, it stops); below half of it, it branches twice as often."),
        flag("c10_collision_avoidance", "Collision Avoidance", false),
        float("c10_collision_avoidance.distance", "Closest approach (% of the domain)", 0.1, 20.0, 2.0, "%")
            .help("A step ending nearer another branch turns ±30°, ±60° to avoid it, else the tip stops."),
        flag("c11_branch_merge", "Branch Merge", false),
        float("c11_branch_merge.distance", "Merge within (% of the domain)", 0.1, 20.0, 3.0, "%")
            .help("A tip this near another branch joins it (a segment to its point) and stops."),
        flag("c12_loop_formation", "Loop Formation", false),
        float("c12_loop_formation.distance", "Bridge within (% of the domain)", 0.1, 20.0, 4.0, "%")
            .help("A tip this near another branch bridges to it once (a loop) and grows on."),
        flag("c13_aging_decay", "Aging / Decay", false),
        int("c13_aging_decay.lifespan", "Tip lifespan (iterations)", 1, i64::from(MAX_ITER), 30),
        int("c13_aging_decay.decay", "Segments decay after (iterations)", 1, i64::from(MAX_ITER), 40),
        flag("c14_branch_pruning", "Branch Pruning", false),
        int("c14_branch_pruning.min", "Shortest branch kept (steps)", 1, 1000, 5),
        int("c14_branch_pruning.order", "Highest branch order kept", 0, 64, 3),
        flag("c15_thickness_evolution", "Thickness Evolution", false),
        float("c15_thickness_evolution.oldest", "Oldest thickness (rows)", 1.0, 16.0, 4.0, ""),
        float("c15_thickness_evolution.newest", "Newest thickness (rows)", 1.0, 16.0, 1.0, ""),
        flag("c16_velocity_evolution", "Velocity Evolution", false),
        int("c16_velocity_evolution.oldest", "Oldest velocity", 1, 127, 127),
        int("c16_velocity_evolution.newest", "Newest velocity", 1, 127, 40),
        flag("c17_gate_evolution", "Gate Evolution", false),
        float("c17_gate_evolution.oldest", "Oldest gate", 1.0, 100.0, 100.0, "%"),
        float("c17_gate_evolution.newest", "Newest gate", 1.0, 100.0, 30.0, "%"),
        flag("c18_subchannel_species", "Subchannel Species", false),
        int("c18_subchannel_species.count", "Species", 1, 16, 3)
            .help("A tip's species: its seed plus its branch order (cycled), or a mutation's; each species its subchannel step."),
        int("c18_subchannel_species.step", "Subchannel step per species", 0, 64, 1),
        flag("c19_mutation_rate", "Mutation Rate", false),
        float("c19_mutation_rate.rate", "Mutation", 0.0, 100.0, 20.0, "%")
            .help("A branch's step and jitter vary by up to this share (seeded) and its species changes with this probability."),
        flag("c20_directional_noise", "Directional Noise", false),
        float("c20_directional_noise.amplitude", "Amplitude", 0.0, 180.0, 35.0, "°"),
        int("c20_directional_noise.period", "Steps per wander", 1, 256, 8)
            .help("Smooth seeded noise of the tip's age turns its heading (meanders) instead of the base's step-by-step jitter."),
        flag("c21_external_track_field", "External Track Field", false),
        list("c21_external_track_field.tracks", "Field tracks", 1, 65_535, 16, &[])
            .help("Their notes in the box (none listed: the track generated into) attract the tips; growth slows away from them."),
        float("c21_external_track_field.strength", "Attraction", 0.0, 100.0, 40.0, "%"),
        float("c21_external_track_field.slow", "Speed away from notes", 1.0, 100.0, 30.0, "%"),
        flag("c22_image_field", "Image Field", false),
        text("c22_image_field.image", "Image (digits 0 – 9, rows top to bottom, / between)", 8192, IMAGE)
            .help("Brightness over the box: tips turn towards brighter cells and grow faster there (an image converted to digits pastes here)."),
        float("c22_image_field.strength", "Attraction", 0.0, 100.0, 40.0, "%"),
        flag("c23_boundary_mask", "Boundary / Mask", false),
        choice("c23_boundary_mask.shape", "Mask", MASKS, 0),
        float("c23_boundary_mask.inset", "Inset (% of the box)", 0.0, 45.0, 0.0, "%")
            .help("Tips stop where they leave the mask (the domain laid on the box)."),
        flag("c24_symmetry_mirrored_growth", "Symmetry / Mirrored Growth", false),
        choice("c24_symmetry_mirrored_growth.mode", "Symmetry", SYMS, 0),
        int("c24_symmetry_mirrored_growth.copies", "Radial copies", 2, 12, 6),
        flag(
            "c25_evolution_timeline_saved_growth_stages",
            "Evolution Timeline / Saved Growth Stages",
            false,
        ),
        int("c25_evolution_timeline_saved_growth_stages.stages", "Stages", 2, 12, 4),
        choice("c25_evolution_timeline_saved_growth_stages.mode", "Show", STAGES, 0),
        int("c25_evolution_timeline_saved_growth_stages.stage", "Saved stage", 1, 12, 2),
        float("c25_evolution_timeline_saved_growth_stages.gap", "Gap", 0.0, 200.0, 10.0, "%")
            .help("Stage k: the growth after k / stages of the iterations; every stage side by side along time, or one."),
    ];
    with_help(&mut own, GROWTH_HELP);
    super::spec(
        "gen.growth",
        "Procedural Growth / Topology",
        "Generates notes along grown branches: one seed branching as it grows along time to start from, stretched over the generation box (where, length and pitch range above).",
        own,
        FUNCTIONS,
    )
}

/// Parses c22's image: brightness 0 – 1 by row (top first), all rows as
/// long as the longest (missing cells dark), or a sentence.
pub fn parse_image(text: &str) -> Result<Vec<Vec<f64>>, String> {
    let mut rows = Vec::new();
    for row in text.split(['/', '\n']) {
        let r: String = row.chars().filter(|c| !c.is_whitespace()).collect();
        if r.is_empty() {
            continue;
        }
        if let Some(c) = r.chars().find(|c| !c.is_ascii_digit()) {
            return Err(format!(
                "The image may hold digits 0 – 9 only (rows separated by /), not “{c}”."
            ));
        }
        rows.push(
            r.bytes()
                .map(|b| f64::from(b - b'0') / 9.0)
                .collect::<Vec<_>>(),
        );
    }
    if rows.is_empty() {
        return Err("The image is empty — write rows of digits 0 – 9 separated by /.".into());
    }
    if rows.len() > 256 || rows.iter().any(|r| r.len() > 256) {
        return Err("The image may be at most 256 × 256 digits.".into());
    }
    let w = rows.iter().map(Vec::len).max().unwrap_or(1);
    for r in &mut rows {
        r.resize(w, 0.0);
    }
    Ok(rows)
}

/// Points from a list of ‰ pairs (an odd last value ignored).
fn pairs(v: &[i64]) -> Vec<(f64, f64)> {
    v.chunks_exact(2)
        .map(|p| {
            (
                p[0].clamp(0, 1000) as f64 / 1000.0,
                p[1].clamp(0, 1000) as f64 / 1000.0,
            )
        })
        .collect()
}

/// A point of the unit domain.
type P2 = (f64, f64);

/// A map of domain points (placement, mirrors).
type PointMap = Box<dyn Fn(P2) -> P2>;

/// The functions' settings.
#[derive(Clone, Debug)]
struct Cfg {
    seeds: Vec<(f64, f64)>,
    iters: u32,
    step: f64,
    branch: f64,
    angles: (f64, f64),
    attract: Option<(Vec<(f64, f64)>, f64)>,
    /// c07: points, strength, reach.
    repel: Option<(Vec<P2>, f64, f64)>,
    /// c08: radius, separation.
    neighbor: Option<(f64, f64)>,
    density: Option<usize>,
    collide: Option<f64>,
    merge: Option<f64>,
    loops: Option<f64>,
    /// c13: lifespan, decay.
    aging: Option<(u32, u32)>,
    /// c14: shortest branch (segments), highest order.
    prune: Option<(usize, u32)>,
    thick: Option<(f64, f64)>,
    vel: Option<(f64, f64)>,
    gate: Option<(f64, f64)>,
    /// c18: species, step.
    species: Option<(u32, u32)>,
    mutation: Option<f64>,
    /// c20: amplitude (radians), period (steps).
    wander: Option<(f64, f64)>,
    /// c21: the field tracks (1-based), attraction, speed away.
    track: Option<(Vec<i64>, f64, f64)>,
    image: Option<(Vec<Vec<f64>>, f64)>,
    /// c23: shape, inset.
    mask: Option<(usize, f64)>,
    /// c24: mode, radial copies.
    sym: Option<(usize, u32)>,
    /// c25: stages, one stage?, gap.
    stages: Option<(u32, Option<u32>, f64)>,
}

impl Cfg {
    fn read(p: &Params) -> Result<Cfg, TransformError> {
        let on = |id: &str| p.flag(id);
        let pct = |k: &str| p.float(k) / 100.0;
        let mut c = Cfg {
            seeds: vec![(0.02, 0.5)],
            iters: BASE_ITER,
            step: BASE_STEP,
            branch: BASE_BRANCH,
            angles: (BASE_ANGLES.0.to_radians(), BASE_ANGLES.1.to_radians()),
            attract: None,
            repel: None,
            neighbor: None,
            density: None,
            collide: None,
            merge: None,
            loops: None,
            aging: None,
            prune: None,
            thick: None,
            vel: None,
            gate: None,
            species: None,
            mutation: None,
            wander: None,
            track: None,
            image: None,
            mask: None,
            sym: None,
            stages: None,
        };
        if on("c01_seed_points") {
            let s = pairs(&p.list("c01_seed_points.points"));
            if s.is_empty() {
                return Err(TransformError::Invalid(
                    "Seed Points needs at least one pair of x, y values.".into(),
                ));
            }
            c.seeds = s;
        }
        if on("c02_iterations") {
            c.iters = p.int("c02_iterations.n").clamp(1, i64::from(MAX_ITER)) as u32;
        }
        if on("c03_growth_step") {
            c.step = pct("c03_growth_step.step").clamp(0.001, 0.2);
        }
        if on("c04_branch_probability") {
            c.branch = pct("c04_branch_probability.p").clamp(0.0, 1.0);
        }
        if on("c05_branch_angle_range") {
            let (a, b) = (
                p.float("c05_branch_angle_range.min").clamp(0.0, 180.0),
                p.float("c05_branch_angle_range.max").clamp(0.0, 180.0),
            );
            c.angles = (a.min(b).to_radians(), a.max(b).to_radians());
        }
        if on("c06_attraction_field") {
            let pts = pairs(&p.list("c06_attraction_field.points"));
            if !pts.is_empty() {
                c.attract = Some((pts, pct("c06_attraction_field.strength").clamp(0.0, 1.0)));
            }
        }
        if on("c07_repulsion_field") {
            let pts = pairs(&p.list("c07_repulsion_field.points"));
            if !pts.is_empty() {
                c.repel = Some((
                    pts,
                    pct("c07_repulsion_field.strength").clamp(0.0, 1.0),
                    pct("c07_repulsion_field.radius").clamp(0.01, 1.0),
                ));
            }
        }
        if on("c08_neighbor_radius") {
            c.neighbor = Some((
                pct("c08_neighbor_radius.radius").clamp(0.005, 0.3),
                pct("c08_neighbor_radius.strength").clamp(0.0, 1.0),
            ));
        }
        if on("c09_density_target") {
            c.density = Some(p.int("c09_density_target.target").clamp(1, 256) as usize);
        }
        if on("c10_collision_avoidance") {
            c.collide = Some(pct("c10_collision_avoidance.distance").clamp(0.001, 0.2));
        }
        if on("c11_branch_merge") {
            c.merge = Some(pct("c11_branch_merge.distance").clamp(0.001, 0.2));
        }
        if on("c12_loop_formation") {
            c.loops = Some(pct("c12_loop_formation.distance").clamp(0.001, 0.2));
        }
        if on("c13_aging_decay") {
            c.aging = Some((
                p.int("c13_aging_decay.lifespan")
                    .clamp(1, i64::from(MAX_ITER)) as u32,
                p.int("c13_aging_decay.decay").clamp(1, i64::from(MAX_ITER)) as u32,
            ));
        }
        if on("c14_branch_pruning") {
            c.prune = Some((
                p.int("c14_branch_pruning.min").clamp(1, 1000) as usize,
                p.int("c14_branch_pruning.order").clamp(0, 64) as u32,
            ));
        }
        if on("c15_thickness_evolution") {
            c.thick = Some((
                p.float("c15_thickness_evolution.oldest").clamp(1.0, 16.0),
                p.float("c15_thickness_evolution.newest").clamp(1.0, 16.0),
            ));
        }
        if on("c16_velocity_evolution") {
            c.vel = Some((
                p.int("c16_velocity_evolution.oldest").clamp(1, 127) as f64,
                p.int("c16_velocity_evolution.newest").clamp(1, 127) as f64,
            ));
        }
        if on("c17_gate_evolution") {
            c.gate = Some((
                pct("c17_gate_evolution.oldest").clamp(0.01, 1.0),
                pct("c17_gate_evolution.newest").clamp(0.01, 1.0),
            ));
        }
        if on("c18_subchannel_species") {
            c.species = Some((
                p.int("c18_subchannel_species.count").clamp(1, 16) as u32,
                p.int("c18_subchannel_species.step").clamp(0, 64) as u32,
            ));
        }
        if on("c19_mutation_rate") {
            c.mutation = Some(pct("c19_mutation_rate.rate").clamp(0.0, 1.0));
        }
        if on("c20_directional_noise") {
            c.wander = Some((
                p.float("c20_directional_noise.amplitude")
                    .clamp(0.0, 180.0)
                    .to_radians(),
                p.int("c20_directional_noise.period").clamp(1, 256) as f64,
            ));
        }
        if on("c21_external_track_field") {
            c.track = Some((
                p.list("c21_external_track_field.tracks"),
                pct("c21_external_track_field.strength").clamp(0.0, 1.0),
                pct("c21_external_track_field.slow").clamp(0.01, 1.0),
            ));
        }
        if on("c22_image_field") {
            c.image = Some((
                parse_image(&p.text("c22_image_field.image")).map_err(TransformError::Invalid)?,
                pct("c22_image_field.strength").clamp(0.0, 1.0),
            ));
        }
        if on("c23_boundary_mask") {
            c.mask = Some((
                p.choice("c23_boundary_mask.shape").min(MASKS.len() - 1),
                pct("c23_boundary_mask.inset").clamp(0.0, 0.45),
            ));
        }
        if on("c24_symmetry_mirrored_growth") {
            c.sym = Some((
                p.choice("c24_symmetry_mirrored_growth.mode")
                    .min(SYMS.len() - 1),
                p.int("c24_symmetry_mirrored_growth.copies").clamp(2, 12) as u32,
            ));
        }
        if on("c25_evolution_timeline_saved_growth_stages") {
            let k = "c25_evolution_timeline_saved_growth_stages";
            let n = p.int(&format!("{k}.stages")).clamp(2, 12) as u32;
            let one = (p.choice(&format!("{k}.mode")) == 1)
                .then(|| (p.int(&format!("{k}.stage")).clamp(1, i64::from(n))) as u32);
            c.stages = Some((n, one, pct(&format!("{k}.gap")).clamp(0.0, 2.0)));
        }
        Ok(c)
    }

    /// Whether the domain is laid on the box (fields and masks are over it).
    fn on_box(&self) -> bool {
        self.track.is_some() || self.image.is_some() || self.mask.is_some()
    }

    /// Whether `p` is inside the domain and the mask.
    fn inside(&self, p: (f64, f64)) -> bool {
        if !(0.0..=1.0).contains(&p.0) || !(0.0..=1.0).contains(&p.1) {
            return false;
        }
        match self.mask {
            None => true,
            Some((shape, inset)) => {
                let r = 0.5 - inset;
                let (dx, dy) = ((p.0 - 0.5).abs(), (p.1 - 0.5).abs());
                match shape {
                    1 => dx <= r && dy <= r,
                    2 => dx + dy <= r,
                    _ => (dx / r).powi(2) + (dy / r).powi(2) <= 1.0,
                }
            }
        }
    }

    /// The radius neighbour queries need (0: none).
    fn query_radius(&self) -> f64 {
        let mut r: f64 = 0.0;
        if let Some((nr, _)) = self.neighbor {
            r = r.max(nr);
        }
        if self.density.is_some() {
            r = r.max(self.neighbor.map_or(DENSITY_RADIUS, |n| n.0));
        }
        for d in [self.collide, self.merge, self.loops].into_iter().flatten() {
            r = r.max(d);
        }
        r
    }
}

/// The notes of the field tracks as a grid of occupied cells over the box.
#[derive(Clone, Debug, Default)]
struct Field {
    cells: Vec<bool>,
    any: bool,
}

impl Field {
    fn of(notes: &[super::super::TNote], f: &Frame) -> Field {
        let mut cells = vec![false; FIELD * FIELD];
        let (w, rows) = ((f.t1 - f.t0) as f64, f64::from(f.hi - f.lo) + 1.0);
        let mut any = false;
        for n in notes {
            if n.pitch < f.lo || n.pitch > f.hi || n.start >= f.t1 || n.start + n.dur <= f.t0 {
                continue;
            }
            let y = ((f64::from(n.pitch - f.lo) + 0.5) / rows * FIELD as f64) as usize;
            let a = n.start.max(f.t0) - f.t0;
            let b = (n.start + n.dur).min(f.t1) - f.t0;
            let (x0, x1) = (
                (a as f64 / w * FIELD as f64) as usize,
                ((b as f64 / w * FIELD as f64).ceil() as usize).max(1),
            );
            for x in x0.min(FIELD - 1)..x1.min(FIELD) {
                cells[y.min(FIELD - 1) * FIELD + x] = true;
                any = true;
            }
        }
        Field { cells, any }
    }

    fn at(&self, p: (f64, f64)) -> bool {
        let x = ((p.0 * FIELD as f64) as usize).min(FIELD - 1);
        let y = ((p.1 * FIELD as f64) as usize).min(FIELD - 1);
        self.any && self.cells[y * FIELD + x]
    }

    /// The nearest occupied cell's centre within 8 cells, if any.
    fn nearest(&self, p: (f64, f64)) -> Option<(f64, f64)> {
        if !self.any {
            return None;
        }
        let cx = ((p.0 * FIELD as f64) as i64).clamp(0, FIELD as i64 - 1);
        let cy = ((p.1 * FIELD as f64) as i64).clamp(0, FIELD as i64 - 1);
        let mut best: Option<((f64, f64), f64)> = None;
        for dy in -8..=8i64 {
            for dx in -8..=8i64 {
                let (x, y) = (cx + dx, cy + dy);
                if x < 0 || y < 0 || x >= FIELD as i64 || y >= FIELD as i64 {
                    continue;
                }
                if self.cells[y as usize * FIELD + x as usize] {
                    let q = (
                        (x as f64 + 0.5) / FIELD as f64,
                        (y as f64 + 0.5) / FIELD as f64,
                    );
                    let d = (q.0 - p.0).hypot(q.1 - p.1);
                    if best.is_none_or(|b| d < b.1) {
                        best = Some((q, d));
                    }
                }
            }
        }
        best.map(|b| b.0)
    }
}

/// Brightness of image `img` at `p` (bilinear; row 0 at the top).
fn bright(img: &[Vec<f64>], p: (f64, f64)) -> f64 {
    let (h, w) = (img.len(), img[0].len());
    let x = (p.0.clamp(0.0, 1.0) * w as f64 - 0.5).clamp(0.0, (w - 1) as f64);
    let y = ((1.0 - p.1.clamp(0.0, 1.0)) * h as f64 - 0.5).clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let a = img[y0][x0] + (img[y0][x1] - img[y0][x0]) * fx;
    let b = img[y1][x0] + (img[y1][x1] - img[y1][x0]) * fx;
    a + (b - a) * fy
}

/// Smooth seeded noise (−1 – 1).
fn noise(seed: u64, u: f64) -> f64 {
    let k = u.floor();
    let f = u - k;
    let r = |i: f64| {
        (hash(&[seed, T_NOISE, i as i64 as u64]) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let w = f * f * (3.0 - 2.0 * f);
    r(k) + (r(k + 1.0) - r(k)) * w
}

/// A turn towards direction `to` from heading `h` by `k` × sin(between).
fn towards(h: f64, to: f64, k: f64) -> f64 {
    k * (to - h).sin()
}

#[derive(Clone, Copy, Debug)]
struct Tip {
    id: u32,
    parent: u32,
    pos: (f64, f64),
    h: f64,
    age: u32,
    order: u32,
    seed: u32,
    step: f64,
    jitter: f64,
    alive: bool,
}

/// One segment: its ends, the iteration it grew in, its tip (or a join).
#[derive(Clone, Copy, Debug)]
struct Seg {
    a: (f64, f64),
    b: (f64, f64),
    it: u32,
    tip: u32,
    join: bool,
}

/// The grown drawing: segments and the tips' order and species.
#[derive(Clone, Debug, Default)]
struct Grown {
    segs: Vec<Seg>,
    tips: Vec<(u32, u32)>,
}

/// Points of the grown branches by grid cell (neighbour queries).
struct Grid {
    cell: f64,
    map: HashMap<(i64, i64), Vec<(P2, u32)>>,
}

impl Grid {
    fn key(&self, p: (f64, f64)) -> (i64, i64) {
        (
            (p.0 / self.cell).floor() as i64,
            (p.1 / self.cell).floor() as i64,
        )
    }

    fn add(&mut self, p: (f64, f64), tip: u32) {
        if self.cell > 0.0 {
            let k = self.key(p);
            self.map.entry(k).or_default().push((p, tip));
        }
    }

    /// Up to [`QUERY_CAP`] points within `r` of `p`, of tips `other` says.
    fn near(&self, p: (f64, f64), r: f64, other: impl Fn(u32) -> bool) -> Vec<((f64, f64), u32)> {
        let mut out = Vec::new();
        if self.cell <= 0.0 {
            return out;
        }
        let (kx, ky) = self.key(p);
        let reach = (r / self.cell).ceil() as i64;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                if let Some(v) = self.map.get(&(kx + dx, ky + dy)) {
                    for &(q, t) in v {
                        if out.len() >= QUERY_CAP {
                            return out;
                        }
                        if other(t) && (q.0 - p.0).hypot(q.1 - p.1) <= r {
                            out.push((q, t));
                        }
                    }
                }
            }
        }
        out
    }
}

impl Cfg {
    /// Grows the branches (the field when the track field is on).
    fn grow(&self, seed: u64, field: &Field) -> Grown {
        let unit = |tag: u64, a: u64, b: u64| {
            (hash(&[seed, tag, a, b]) >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut g = Grown::default();
        let mut tips: Vec<Tip> = Vec::new();
        for (k, &p) in self.seeds.iter().enumerate() {
            let species = self.species.map_or(0, |s| k as u32 % s.0);
            tips.push(Tip {
                id: k as u32,
                parent: u32::MAX,
                pos: p,
                h: 0.0,
                age: 0,
                order: 0,
                seed: k as u32,
                step: self.step,
                jitter: BASE_JITTER.to_radians(),
                alive: self.inside(p),
            });
            g.tips.push((0, species));
        }
        let qr = self.query_radius();
        let mut grid = Grid {
            cell: qr,
            map: HashMap::new(),
        };
        let mut bridged: HashSet<(u32, u32)> = HashSet::new();
        for t in &tips {
            grid.add(t.pos, t.id);
        }
        for it in 0..self.iters {
            let n = tips.len();
            let live = tips.iter().filter(|t| t.alive).count();
            let mut born = 0usize;
            for i in 0..n {
                if !tips[i].alive {
                    continue;
                }
                let mut t = tips[i];
                let id = t.id;
                // Other branches: not the tip, nor its parent while young.
                let (parent, young) = (t.parent, t.age < 3);
                let other = move |o: u32| o != id && !(o == parent && young);
                // Steering.
                let mut h = t.h;
                match self.wander {
                    Some((amp, period)) => {
                        // The heading wanders: amp × smooth noise of the age.
                        let s = seed ^ u64::from(id);
                        let (u0, u1) = (f64::from(t.age) / period, f64::from(t.age + 1) / period);
                        h += amp * (noise(s, u1) - noise(s, u0));
                    }
                    None => h += t.jitter * (2.0 * unit(T_JIT, u64::from(id), u64::from(it)) - 1.0),
                }
                if let Some((pts, k)) = &self.attract {
                    let q = pts
                        .iter()
                        .min_by(|a, b| {
                            let da = (a.0 - t.pos.0).hypot(a.1 - t.pos.1);
                            let db = (b.0 - t.pos.0).hypot(b.1 - t.pos.1);
                            da.total_cmp(&db)
                        })
                        .copied()
                        .unwrap_or(t.pos);
                    if q != t.pos {
                        h += towards(h, (q.1 - t.pos.1).atan2(q.0 - t.pos.0), *k);
                    }
                }
                if let Some((pts, k, reach)) = &self.repel {
                    for q in pts {
                        let d = (q.0 - t.pos.0).hypot(q.1 - t.pos.1);
                        if d < *reach && d > 0.0 {
                            let away = (t.pos.1 - q.1).atan2(t.pos.0 - q.0);
                            h += towards(h, away, k * (1.0 - d / reach));
                        }
                    }
                }
                if let Some((r, k)) = self.neighbor {
                    let nb = grid.near(t.pos, r, other);
                    if !nb.is_empty() {
                        let c = nb
                            .iter()
                            .fold((0.0, 0.0), |s, (q, _)| (s.0 + q.0, s.1 + q.1));
                        let c = (c.0 / nb.len() as f64, c.1 / nb.len() as f64);
                        if c != t.pos {
                            h += towards(h, (t.pos.1 - c.1).atan2(t.pos.0 - c.0), k);
                        }
                    }
                }
                let mut speed = 1.0;
                if let Some((_, k, slow)) = &self.track {
                    if let Some(q) = field.nearest(t.pos)
                        && q != t.pos
                    {
                        h += towards(h, (q.1 - t.pos.1).atan2(q.0 - t.pos.0), *k);
                    }
                    if !field.at(t.pos) {
                        speed *= slow;
                    }
                }
                if let Some((img, k)) = &self.image {
                    let e = 0.02;
                    let gx =
                        bright(img, (t.pos.0 + e, t.pos.1)) - bright(img, (t.pos.0 - e, t.pos.1));
                    let gy =
                        bright(img, (t.pos.0, t.pos.1 + e)) - bright(img, (t.pos.0, t.pos.1 - e));
                    if gx != 0.0 || gy != 0.0 {
                        h += towards(h, gy.atan2(gx), *k);
                    }
                    speed *= 0.3 + 0.7 * bright(img, t.pos);
                }
                // The step, its checks.
                let len = t.step * speed;
                let mut next = (t.pos.0 + len * h.cos(), t.pos.1 + len * h.sin());
                if let Some(d) = self.collide {
                    let hit = |p: (f64, f64)| !grid.near(p, d, other).is_empty();
                    if hit(next) {
                        let mut ok = false;
                        for turn in [30.0f64, -30.0, 60.0, -60.0] {
                            let hh = h + turn.to_radians();
                            let p = (t.pos.0 + len * hh.cos(), t.pos.1 + len * hh.sin());
                            if !hit(p) {
                                (h, next, ok) = (hh, p, true);
                                break;
                            }
                        }
                        if !ok {
                            tips[i].alive = false;
                            continue;
                        }
                    }
                }
                if !self.inside(next) {
                    tips[i].alive = false;
                    continue;
                }
                g.segs.push(Seg {
                    a: t.pos,
                    b: next,
                    it,
                    tip: id,
                    join: false,
                });
                grid.add(next, id);
                t.pos = next;
                t.h = h;
                // Merges and loops.
                if let Some(d) = self.merge
                    && let Some(&(q, _)) = grid.near(next, d, other).first()
                {
                    g.segs.push(Seg {
                        a: next,
                        b: q,
                        it,
                        tip: id,
                        join: true,
                    });
                    t.alive = false;
                }
                if t.alive
                    && let Some(d) = self.loops
                    && let Some(&(q, o)) = grid
                        .near(next, d, other)
                        .iter()
                        .find(|(_, o)| !bridged.contains(&(id.min(*o), id.max(*o))))
                {
                    bridged.insert((id.min(o), id.max(o)));
                    g.segs.push(Seg {
                        a: next,
                        b: q,
                        it,
                        tip: id,
                        join: true,
                    });
                }
                // Branching.
                let mut p_branch = self.branch;
                if let Some(target) = self.density {
                    let r = self.neighbor.map_or(DENSITY_RADIUS, |n| n.0);
                    let count = grid.near(next, r, |_| true).len();
                    if count > 2 * target {
                        t.alive = false;
                    }
                    if count > target {
                        p_branch = 0.0;
                    } else if count * 2 < target {
                        p_branch = (p_branch * 2.0).min(1.0);
                    }
                }
                t.age += 1;
                if let Some((life, _)) = self.aging
                    && t.age >= life
                {
                    t.alive = false;
                }
                tips[i] = t;
                if t.alive
                    && live + born < TIP_CAP
                    && unit(T_BR, u64::from(id), u64::from(it)) < p_branch
                {
                    let side = if unit(T_SIDE, u64::from(id), u64::from(it)) < 0.5 {
                        1.0
                    } else {
                        -1.0
                    };
                    let ang = self.angles.0
                        + (self.angles.1 - self.angles.0)
                            * unit(T_ANG, u64::from(id), u64::from(it));
                    let cid = tips.len() as u32;
                    let mut step = t.step;
                    let mut jitter = t.jitter;
                    let order = t.order + 1;
                    let mut species = self.species.map_or(0, |s| (t.seed + order) % s.0);
                    if let Some(m) = self.mutation {
                        let u = 2.0 * unit(T_MUT, u64::from(cid), 0) - 1.0;
                        step = (step * (1.0 + m * u))
                            .clamp(self.step / STEP_SPAN, self.step * STEP_SPAN);
                        jitter = (jitter * (1.0 - m * u)).clamp(0.0, PI);
                        if let Some((n, _)) = self.species
                            && unit(T_SPEC, u64::from(cid), 0) < m
                        {
                            species = (unit(T_SPEC, u64::from(cid), 1) * f64::from(n)) as u32 % n;
                        }
                    }
                    tips.push(Tip {
                        id: cid,
                        parent: id,
                        pos: next,
                        h: h + side * ang,
                        age: 0,
                        order,
                        seed: t.seed,
                        step,
                        jitter,
                        alive: true,
                    });
                    g.tips.push((order, species));
                    born += 1;
                }
            }
            if !tips.iter().any(|t| t.alive) {
                break;
            }
        }
        g
    }
}

/// One piece to draw: points (domain), the iteration of each, its tip.
#[derive(Clone, Debug)]
struct Piece {
    pts: Vec<(f64, f64)>,
    its: Vec<u32>,
    tip: u32,
}

/// The grown segments up to iteration `upto` as pieces (a tip's run, joins
/// alone), less decayed segments and pruned branches.
fn pieces(c: &Cfg, g: &Grown, upto: u32) -> Vec<Piece> {
    let last = upto.saturating_sub(1);
    let keep_seg = |s: &Seg| {
        s.it < upto
            && c.aging
                .is_none_or(|(_, decay)| last.saturating_sub(s.it) < decay)
    };
    let mut by_tip: Vec<Vec<&Seg>> = vec![Vec::new(); g.tips.len()];
    let mut out = Vec::new();
    for s in &g.segs {
        if !keep_seg(s) {
            continue;
        }
        if s.join {
            out.push(Piece {
                pts: vec![s.a, s.b],
                its: vec![s.it, s.it],
                tip: s.tip,
            });
        } else if let Some(v) = by_tip.get_mut(s.tip as usize) {
            v.push(s);
        }
    }
    for (tip, segs) in by_tip.iter().enumerate() {
        if let Some((min, order)) = c.prune
            && (segs.len() < min || g.tips[tip].0 > order)
        {
            continue;
        }
        let mut cur: Option<Piece> = None;
        for s in segs {
            match &mut cur {
                Some(p) if p.pts.last() == Some(&s.a) => {
                    p.pts.push(s.b);
                    p.its.push(s.it);
                }
                _ => {
                    if let Some(p) = cur.take() {
                        out.push(p);
                    }
                    cur = Some(Piece {
                        pts: vec![s.a, s.b],
                        its: vec![s.it, s.it],
                        tip: tip as u32,
                    });
                }
            }
        }
        if let Some(p) = cur {
            out.push(p);
        }
    }
    out
}

/// What is drawn: the pieces (grid units) with their styles and brushes.
#[derive(Debug, Default)]
struct Built {
    paths: Vec<(Vec<Pt>, PathStyle, Brush)>,
}

struct Growth {
    cfg: Cfg,
    /// The track field's notes, once the source is known (c21).
    field: OnceLock<Field>,
    /// The pieces and styles, or the piece cap's sentence.
    built: OnceLock<Result<Built, TransformError>>,
}

impl Growth {
    fn new(cfg: Cfg) -> Growth {
        Growth {
            cfg,
            field: OnceLock::new(),
            built: OnceLock::new(),
        }
    }

    /// The pieces and styles, or a cap's sentence.
    fn build(&self, f: &Frame) -> Result<Built, TransformError> {
        let c = &self.cfg;
        let empty = Field::default();
        let field = self.field.get().unwrap_or(&empty);
        let g = c.grow(f.seed, field);
        // Stages: the growth up to k / n of the iterations.
        let stages: Vec<u32> = match c.stages {
            None => vec![c.iters],
            Some((n, one, _)) => {
                let at = |k: u32| (c.iters * k).div_ceil(n).max(1);
                match one {
                    Some(k) => vec![at(k)],
                    None => (1..=n).map(at).collect(),
                }
            }
        };
        let mut items: Vec<Vec<Piece>> = Vec::new();
        for upto in stages {
            let mut ps = pieces(c, &g, upto);
            // c24: copies about the domain's centre.
            if let Some((mode, n)) = c.sym {
                let one = ps.clone();
                let map = |ps: &mut Vec<Piece>, m: &dyn Fn((f64, f64)) -> (f64, f64)| {
                    for p in &one {
                        let mut q = p.clone();
                        for x in &mut q.pts {
                            *x = m(*x);
                        }
                        ps.push(q);
                    }
                };
                match mode {
                    0 => map(&mut ps, &|p| (p.0, 1.0 - p.1)),
                    1 => map(&mut ps, &|p| (1.0 - p.0, p.1)),
                    2 => {
                        map(&mut ps, &|p| (p.0, 1.0 - p.1));
                        map(&mut ps, &|p| (1.0 - p.0, p.1));
                        map(&mut ps, &|p| (1.0 - p.0, 1.0 - p.1));
                    }
                    _ => {
                        for k in 1..n {
                            let (s, co) = (TAU * f64::from(k) / f64::from(n)).sin_cos();
                            map(&mut ps, &|p| {
                                let (dx, dy) = (p.0 - 0.5, p.1 - 0.5);
                                (0.5 + dx * co - dy * s, 0.5 + dx * s + dy * co)
                            });
                        }
                    }
                }
            }
            items.push(ps);
        }
        // Stages side by side along time.
        let gap = c.stages.map_or(0.0, |s| s.2);
        let ext = |ps: &[Piece]| {
            let mut e: Option<[f64; 4]> = None;
            for p in ps.iter().flat_map(|p| p.pts.iter()) {
                let b = e.get_or_insert([p.0, p.1, p.0, p.1]);
                *b = [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)];
            }
            e
        };
        let mut all: Vec<Piece> = Vec::new();
        if items.len() == 1 {
            all = items.pop().unwrap_or_default();
        } else {
            // On the box: each stage a slot of the domain's width.
            let mut at = 0.0;
            for ps in items {
                for mut p in ps {
                    for x in &mut p.pts {
                        x.0 += at;
                    }
                    all.push(p);
                }
                at += 1.0 + gap;
            }
        }
        // Placement: the domain on the box, or the drawing stretched over it.
        let g0 = BoxG::of(f);
        let gb = Gb::of(&g0, 0.0);
        let m = Aff {
            a: g0.step,
            ..Aff::ID
        };
        let map: PointMap = if c.on_box() {
            let w = match c.stages {
                Some((n, None, g)) => f64::from(n) + f64::from(n - 1) * g,
                _ => 1.0,
            };
            Box::new(move |p| ((p.0 / w).clamp(0.0, 1.0), p.1.clamp(0.0, 1.0)))
        } else {
            match ext(&all) {
                None => Box::new(|p| p),
                Some(e) => {
                    let (w, h) = (e[2] - e[0], e[3] - e[1]);
                    Box::new(move |p| {
                        (
                            if w > 0.0 { (p.0 - e[0]) / w } else { 0.5 },
                            if h > 0.0 { (p.1 - e[1]) / h } else { 0.5 },
                        )
                    })
                }
            }
        };
        let last = f64::from(c.iters.max(1) - 1).max(1.0);
        let mut b = Built::default();
        let mut cut = 0.0;
        for p in all {
            if p.pts.len() < 2 {
                continue;
            }
            let mut pts: Vec<Pt> = p
                .pts
                .iter()
                .map(|&q| {
                    let (u, v) = gb.at(map(q));
                    Pt { x: u, y: v, s: 0.0 }
                })
                .collect();
            positions(&mut pts);
            // Age 1 at the first iteration, 0 at the last.
            let age = |i: usize| 1.0 - f64::from(p.its[i]) / last;
            let mut st = PathStyle::default();
            if let Some((old, new)) = c.thick {
                st.thick = (new + (old - new) * age(0)).max(1.0);
            }
            if c.vel.is_some() || c.gate.is_some() {
                let gate = |i: usize| c.gate.map_or(1.0, |(o, n)| n + (o - n) * age(i));
                let vel = |i: usize| c.vel.map_or(1.0, |(o, n)| (n + (o - n) * age(i)) / 127.0);
                if c.gate.is_some() {
                    st.grad.gate = Some((0.0, 1.0));
                }
                if c.vel.is_some() {
                    st.grad.vel = Some((0.0, 127.0));
                }
                st.shape = Some(Box::new([
                    curve_along(&pts, gate),
                    curve_along(&pts, vel),
                    curve_along(&pts, |_| 0.0),
                ]));
                // The evolving brush cuts the strokes into pieces (V09-S60's
                // cap: up to 1,024 a segment over a huge box).
                let q: Vec<Pt> = pts.iter().map(|&x| m.ap(x)).collect();
                cut += st.pieces(&measure(&q, &g0), false, f, &f.band());
                if cut > PIECE_CAP {
                    return Err(too_many_pieces(cut));
                }
            }
            let mut br = f.brush;
            if let Some((_, step)) = c.species {
                let sp = g.tips.get(p.tip as usize).map_or(0, |t| t.1);
                br.sub = (u64::from(br.sub) + u64::from(step) * u64::from(sp)).min(4095) as u32;
            }
            b.paths.push((pts, st, br));
        }
        Ok(b)
    }

    fn built(&self, f: &Frame) -> Option<&Built> {
        self.built.get_or_init(|| self.build(f)).as_ref().ok()
    }

    /// A bound for any field (c21, before the source is known): every
    /// segment at most a few steps long.
    fn loose_bound(&self, f: &Frame) -> u64 {
        let c = &self.cfg;
        let g = BoxG::of(f);
        let (w, h) = (g.w() / g.step, g.rows());
        let tips = (TIP_CAP + c.seeds.len()) as f64;
        let segs = f64::from(c.iters) * tips * 2.0;
        let copies = match c.sym {
            None => 1.0,
            Some((2, _)) => 4.0,
            Some((3, n)) => f64::from(n),
            Some(_) => 2.0,
        };
        let stages = c
            .stages
            .map_or(1.0, |s| if s.1.is_some() { 1.0 } else { f64::from(s.0) });
        let len = (c.step * STEP_SPAN)
            .max(c.merge.unwrap_or(0.0))
            .max(c.loops.unwrap_or(0.0));
        let band = c.thick.map_or(1.0, |t| t.0.max(t.1));
        // A stroke: its rows plus two notes a piece, one piece a grid unit
        // when the brush varies.
        let per = (len * (w + h) + 2.0) * (band + 2.0) * 2.0 + 4.0;
        let total = segs * copies * stages * per;
        if total.is_finite() && total < u64::MAX as f64 {
            total.ceil() as u64
        } else {
            u64::MAX
        }
    }
}

impl Shapes for Growth {
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
        if self.cfg.track.is_some() && self.field.get().is_none() {
            return self.loose_bound(f);
        }
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

    fn source(&self, src: &Source, f: &Frame) -> Result<(), TransformError> {
        if self.cfg.track.is_some() {
            let _ = self
                .field
                .get_or_init(|| Field::of(src.aux.first().map_or(&[][..], |v| v), f));
        }
        match self.built.get_or_init(|| self.build(f)) {
            Ok(_) => Ok(()),
            Err(e) => Err(e.clone()),
        }
    }
}

/// Reads the field tracks' notes for c21 (none listed: the track generated
/// into) as auxiliary layer 0.
pub fn prepare(
    doc: &crate::model::PlumDocument,
    scopes: &[super::super::TrackScope],
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("c21_external_track_field") {
        return Ok(());
    }
    // B2-F16: the field tracks as the song shows them on the edited notes'
    // ticks; empty = the track the edited clip sits on.
    let Some((active, shift)) = super::super::source::edit_place(scopes, ctx) else {
        return Ok(());
    };
    let list = p.list("c21_external_track_field.tracks");
    let idx: Vec<usize> = if list.is_empty() {
        vec![active]
    } else {
        list.iter()
            .filter_map(|&n| usize::try_from(n - 1).ok())
            .filter(|&i| doc.track(i).is_some())
            .collect()
    };
    let notes: Vec<super::super::TNote> = idx
        .into_iter()
        .flat_map(|i| Source::placed_layer(doc, i, u16::MAX, limit, shift))
        .collect();
    src.aux = std::sync::Arc::new(vec![notes]);
    Ok(())
}

fn growth_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Growth::new(Cfg::read(p)?);
    if g.cfg.track.is_none() {
        // Grown now (no source needed): a cap refuses before anything runs.
        let built = g.build(&Frame::of(p, b.ctx, b.stats))?;
        let _ = g.built.set(Ok(built));
    }
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
    fn images_parse_and_bad_ones_are_named() {
        let img = parse_image(IMAGE).unwrap();
        assert_eq!((img.len(), img[0].len()), (8, 16));
        assert_eq!(bright(&img, (0.5, 0.97)), 0.0);
        assert!(parse_image("12/3a").unwrap_err().contains("“a”"));
        assert!(parse_image(" / ").is_err());
        assert_eq!(parse_image("9/99").unwrap()[0], vec![1.0, 0.0]);
    }

    #[test]
    fn growth_is_seeded_and_tips_stay_in_the_domain() {
        let spec = growth_spec();
        let p = ParamSet::defaults(&spec.params);
        let params = Params::new(&spec.params, &p);
        let c = Cfg::read(&params).unwrap();
        let a = c.grow(5, &Field::default());
        let b = c.grow(5, &Field::default());
        assert_eq!(a.segs.len(), b.segs.len());
        assert!(a.segs.len() >= 20, "{}", a.segs.len());
        assert_ne!(a.segs.len(), c.grow(6, &Field::default()).segs.len());
        for s in &a.segs {
            assert!(c.inside(s.b), "{s:?}");
            assert!(((s.b.0 - s.a.0).hypot(s.b.1 - s.a.1) - BASE_STEP).abs() < 1e-9);
        }
    }

    fn grown(kv: &[(&str, ParamValue)], seed: u64) -> (Cfg, Grown) {
        let spec = growth_spec();
        let mut p = ParamSet::defaults(&spec.params);
        p.set("c04_branch_probability", ParamValue::Bool(true));
        p.set("c02_iterations", ParamValue::Bool(true));
        for (k, v) in kv {
            p.set(k, v.clone());
        }
        let c = Cfg::read(&Params::new(&spec.params, &p)).unwrap();
        let g = c.grow(seed, &Field::default());
        (c, g)
    }

    #[test]
    fn collisions_keep_other_branches_apart_and_lifespans_end_tips() {
        let d = 0.02;
        let (_, g) = grown(&[("c10_collision_avoidance", ParamValue::Bool(true))], 9);
        assert!(g.segs.len() > 100);
        // Each tip's points in order, its parent, its first three exempt
        // from its parent.
        let mut pts: Vec<Vec<(f64, f64)>> = vec![Vec::new(); g.tips.len()];
        for s in &g.segs {
            pts[s.tip as usize].push(s.b);
        }
        let parent: HashMap<u32, u32> = {
            // A child starts where its parent's segment ended.
            let mut m = HashMap::new();
            for s in g.segs.iter() {
                if !m.contains_key(&s.tip)
                    && let Some(p) = g.segs.iter().find(|q| q.tip != s.tip && q.b == s.a)
                {
                    m.insert(s.tip, p.tip);
                }
            }
            m
        };
        for (a, pa) in pts.iter().enumerate() {
            for (b, pb) in pts.iter().enumerate().skip(a + 1) {
                for (i, p) in pa.iter().enumerate() {
                    for (j, q) in pb.iter().enumerate() {
                        let young = (parent.get(&(b as u32)) == Some(&(a as u32)) && j < 3)
                            || (parent.get(&(a as u32)) == Some(&(b as u32)) && i < 3);
                        if !young {
                            let dist = (p.0 - q.0).hypot(p.1 - q.1);
                            assert!(dist >= d - 1e-9, "tips {a} / {b}: {dist}");
                        }
                    }
                }
            }
        }
        // c13: no tip grows more than its lifespan.
        let (_, g) = grown(&[("c13_aging_decay", ParamValue::Bool(true))], 9);
        let mut n = vec![0u32; g.tips.len()];
        for s in &g.segs {
            n[s.tip as usize] += 1;
        }
        assert!(n.iter().all(|&k| k <= 30), "{n:?}");
    }

    #[test]
    fn merges_end_tips_and_loops_bridge_each_pair_once() {
        let (c, g) = grown(&[("c11_branch_merge", ParamValue::Bool(true))], 4);
        let joins: Vec<&Seg> = g.segs.iter().filter(|s| s.join).collect();
        assert!(!joins.is_empty());
        for j in &joins {
            assert!((j.b.0 - j.a.0).hypot(j.b.1 - j.a.1) <= c.merge.unwrap() + 1e-12);
            // Nothing grows on that tip afterwards.
            assert!(!g.segs.iter().any(|s| s.tip == j.tip && s.it > j.it));
        }
        let (c, g) = grown(&[("c12_loop_formation", ParamValue::Bool(true))], 4);
        let mut seen = HashSet::new();
        let ends: HashMap<(u64, u64), u32> = g
            .segs
            .iter()
            .filter(|s| !s.join)
            .map(|s| ((s.b.0.to_bits(), s.b.1.to_bits()), s.tip))
            .collect();
        let bridges: Vec<&Seg> = g.segs.iter().filter(|s| s.join).collect();
        assert!(!bridges.is_empty());
        for b in bridges {
            assert!((b.b.0 - b.a.0).hypot(b.b.1 - b.a.1) <= c.loops.unwrap() + 1e-12);
            let other = ends
                .get(&(b.b.0.to_bits(), b.b.1.to_bits()))
                .copied()
                .unwrap_or(u32::MAX);
            assert!(
                seen.insert((b.tip.min(other), b.tip.max(other))),
                "bridged twice"
            );
        }
    }

    #[test]
    fn pruning_keeps_long_low_order_branches_only() {
        let (c, g) = grown(&[("c14_branch_pruning", ParamValue::Bool(true))], 2);
        let (min, order) = c.prune.unwrap();
        let ps = pieces(&c, &g, c.iters);
        assert!(!ps.is_empty());
        for p in ps {
            let n = g.segs.iter().filter(|s| s.tip == p.tip && !s.join).count();
            assert!(n >= min && g.tips[p.tip as usize].0 <= order);
        }
    }

    /// The lower bound never passes the count and the cheap bound never
    /// falls below it, over seeded random settings in small boxes.
    #[test]
    fn the_bound_never_falls_below_the_count() {
        let spec = growth_spec();
        let mut rng = Rng::new(0x5345_3631);
        for draw in 0..120 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 200)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            p.set("gen.seed", ParamValue::Int(rng.range_i64(1, 1000)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            p.set("c02_iterations.n", ParamValue::Int(rng.range_i64(1, 150)));
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let Ok(cfg) = Cfg::read(&params) else {
                continue;
            };
            let g = Growth::new(cfg);
            let loose = g.loose_bound(&f);
            let mut pen = Pen::new(Counter::default(), f.step, f.band());
            g.draw(&f, &mut pen);
            let n = pen.finish().notes;
            assert!(g.bound(&f) >= n, "draw {draw}: bound {} < {n}", g.bound(&f));
            assert!(loose >= n, "draw {draw}: loose bound {loose} < {n}");
        }
    }
}
