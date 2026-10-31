//! Fractal / L-System (§159, `gen.lsystem`; `docs/GENERATORS.md` "Fractal /
//! L-System"): the third Advanced generator — a string of symbols rewritten
//! by rules a number of times, then read by a turtle, its path drawn by the
//! shared path pen (`generator/path.rs`, as Lines, Arcs and Parametric).
//! The base: the fractal plant X → F−[[X]+X]+F[+FX]−X, F → FF from the
//! axiom X, 5 rewrites, turns of 22.5°, the turtle heading along time,
//! stretched over the box.
//!
//! **Bounded expansion**: the string is never stored. Before anything is
//! rewritten, upper bounds of its length, of the segments drawn and of the
//! branches closed are counted per symbol (the largest successor of every
//! rule a symbol may take, rewrite by rewrite), and a request beyond the
//! caps is refused with a sentence; the symbols are then rewritten depth
//! first (recursion as deep as the rewrites, ≤ 12) and read by the turtle
//! as they come. Every random choice is keyed by the symbol's place in the
//! rewriting (its ancestors' choices), so it does not depend on order.
//!
//! The alphabet: `F` `G` draw a step, `f` `g` move a step without drawing,
//! `+` / `−` turn left / right (left: towards higher pitches when heading
//! along time), `|` turns back, `[` / `]` push / pop the turtle; other
//! symbols only rewrite (c07 lets letters draw too). The pipeline:
//!
//! 1. **Strings** — c01 axiom, c02 rules (`P=successor`, `;` between), c03
//!    rewrites, c08 more rules for the same symbols chosen by the seed, c09
//!    weights (a symbol's rules share 100 %: under it, the symbol may stay
//!    as it is), c25 a rule set B taking over generation by generation (or
//!    the evolution: every generation drawn in turn).
//! 2. **Turtle** — c04 step, c05 angle, c06 branches (length per level,
//!    extra turn), c07 drawing letters, c10 / c11 the steps of newer growth
//!    scaled in time / pitch (a symbol's generation: when its line began —
//!    a symbol rewritten into itself keeps its parent's), c17 pruning, c18
//!    tropism, c19 seeded jitter of turns and steps, c23 joins between
//!    branch tips, c24 a forest of several axioms side by side.
//! 3. **Placement** — c21 mirror / radial copies about the start, the
//!    plants and stages laid out along time, then stretched over the box
//!    (c20 keeps the proportions instead; c04 draws at its own size from an
//!    anchor, cut at the box); c22 wraps / reflects into the box.
//! 4. **Drawing** — strokes along each piece (a piece: one branch depth, one
//!    symbol class, one plant); by branch depth c12 thickness, c13 density,
//!    c14 velocity, c15 gate, c16 subchannel.

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::hash;
use super::super::{Curve, Kernel, TransformError};
use super::draw::{Aff, BoxG, Gb, Pt, clip_seg, positions};
use super::path::{PathStyle, Scratch, VISIT_CAP, measure, too_many_visits};
use super::pen::{Brush, Draw};
use super::{Frame, Shapes};
use crate::midi::group_digits;
use std::f64::consts::{PI, TAU};
use std::sync::OnceLock;

pub const LSYSTEM: ToolDef = ToolDef {
    id: "gen.lsystem",
    spec: lsystem_spec,
    build: lsystem_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_axiom", "Axiom", 10),
    ("c02_rewrite_rules", "Rewrite Rules", 20),
    ("c03_iteration_depth", "Iteration Depth", 30),
    ("c04_turtle_step_length", "Turtle Step Length", 40),
    ("c05_turn_angle", "Turn Angle", 50),
    ("c06_branch_push_pop", "Branch Push/Pop", 60),
    ("c07_multiple_symbols", "Multiple Symbols", 70),
    (
        "c08_stochastic_rules_with_seed",
        "Stochastic Rules with Seed",
        80,
    ),
    ("c09_rule_weights", "Rule Weights", 90),
    (
        "c10_time_scale_per_generation",
        "Time Scale Per Generation",
        100,
    ),
    (
        "c11_pitch_scale_per_generation",
        "Pitch Scale Per Generation",
        110,
    ),
    ("c12_thickness_per_depth", "Thickness Per Depth", 120),
    ("c13_density_per_depth", "Density Per Depth", 130),
    ("c14_velocity_per_depth", "Velocity Per Depth", 140),
    ("c15_gate_per_depth", "Gate Per Depth", 150),
    ("c16_subchannel_per_depth", "Subchannel Per Depth", 160),
    ("c17_branch_pruning", "Branch Pruning", 170),
    (
        "c18_tropism_direction_bias",
        "Tropism / Direction Bias",
        180,
    ),
    ("c19_growth_noise", "Growth Noise", 190),
    ("c20_bounding_box_fit", "Bounding-Box Fit", 200),
    ("c21_mirror_symmetry", "Mirror / Symmetry", 210),
    ("c22_wrap_reflect", "Wrap / Reflect", 220),
    ("c23_branch_join", "Branch Join", 230),
    ("c24_multi_axiom_forest", "Multi-Axiom Forest", 240),
    (
        "c25_rule_set_morph_evolution",
        "Rule-Set Morph / Evolution",
        250,
    ),
];

/// The base: the axiom, the rules, the rewrites and the angle (degrees).
pub const BASE_AXIOM: &str = "X";
pub const BASE_RULES: &str = "X=F-[[X]+X]+F[+FX]-X; F=FF";
pub const BASE_DEPTH: u32 = 5;
pub const BASE_ANGLE: f64 = 22.5;
/// Most rewrites.
pub const MAX_DEPTH: u32 = 12;
/// Longest axiom or successor (symbols).
pub const RULE_LEN: usize = 256;
/// Most rules of one set.
pub const MAX_RULES: usize = 32;
/// Most symbols the rewriting may make (all plants and stages).
pub const SYMBOL_CAP: u128 = 1 << 22;
/// Most segments drawn (copies and joins included).
pub const SEGMENT_CAP: u128 = 1 << 20;
/// Most points after wrapping / cutting.
pub const POINT_CAP: f64 = (1u64 << 21) as f64;

const ANCHORS: &[&str] = &[
    "The box's start, middle row",
    "The box's center",
    "The box's start, lowest row",
];
const FITS: &[&str] = &[
    "Keep the proportions (whole shape inside)",
    "Fit the box's width (proportions kept)",
    "Fit the box's height (proportions kept)",
];
const AXES: &[&str] = &[
    "None",
    "Pitch mirror (about the start's row)",
    "Time mirror (about the start)",
    "Both",
];
const WRAPS: &[&str] = &["Wrap round", "Reflect"];
const JOINS: &[&str] = &[
    "Join the tips of sibling branches",
    "Join each tip back to its branch point",
];
const MORPHS: &[&str] = &[
    "Morph to rule set B over the generations",
    "Evolution: each generation in turn along time",
];

// Random streams (tags).
const T_PICK: u64 = 0x5049_434B;
const T_MORPH: u64 = 0x4D4F_5250;
const T_PICKB: u64 = 0x5049_4B42;
const T_PRUNE: u64 = 0x5052_554E;
const T_TURN: u64 = 0x5455_524E;
const T_LEN: u64 = 0x4C45_4E47;
const T_KEEP: u64 = 0x4B45_4550;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const LSYSTEM_HELP: &[(&str, &str)] = &[
    (
        "c01_axiom",
        "Starts the rewriting from a string of your own instead of the plant's X.",
    ),
    (
        "c02_rewrite_rules",
        "Uses rules of your own to rewrite the symbols instead of the fractal plant's.",
    ),
    (
        "c03_iteration_depth",
        "Sets how many times the rules rewrite the string instead of 5.",
    ),
    (
        "c03_iteration_depth.depth",
        "How many times every symbol is rewritten; each rewrite makes the drawing more detailed.",
    ),
    (
        "c04_turtle_step_length",
        "Draws the turtle at a fixed step size from a starting point instead of stretching it over the box.",
    ),
    (
        "c04_turtle_step_length.anchor",
        "Where the turtle starts: at the box's start on its middle row, at its center, or at its start on its lowest row.",
    ),
    (
        "c05_turn_angle",
        "Sets how far + and - turn the turtle instead of 22.5 degrees.",
    ),
    (
        "c05_turn_angle.angle",
        "How many degrees each + or - turns the turtle.",
    ),
    (
        "c06_branch_push_pop",
        "Shortens the steps inside branches and can turn each new branch further.",
    ),
    (
        "c06_branch_push_pop.turn",
        "An extra turn the turtle makes at the start of every branch, in degrees.",
    ),
    (
        "c07_multiple_symbols",
        "Lets other letters draw a step too, each with its own length and subchannel.",
    ),
    (
        "c07_multiple_symbols.length",
        "How long the listed letters' steps are, as a share of F's step.",
    ),
    (
        "c07_multiple_symbols.sub_step",
        "How many subchannel positions each listed letter moves from F's.",
    ),
    (
        "c08_stochastic_rules_with_seed",
        "Adds more rules for the same symbols, so each place picks one at random (seeded).",
    ),
    (
        "c09_rule_weights",
        "Sets how likely each rule is to be picked.",
    ),
    (
        "c10_time_scale_per_generation",
        "Makes newer growth shorter (or longer) in time than older growth.",
    ),
    (
        "c11_pitch_scale_per_generation",
        "Makes newer growth smaller (or larger) in pitch than older growth.",
    ),
    (
        "c11_pitch_scale_per_generation.k",
        "A step's pitch extent is multiplied by this once per generation of growth.",
    ),
    (
        "c12_thickness_per_depth",
        "Draws the trunk thick and each deeper branch thinner.",
    ),
    (
        "c12_thickness_per_depth.rows",
        "How many rows thick the trunk is.",
    ),
    (
        "c12_thickness_per_depth.factor",
        "Each branch's thickness as a share of the branch it grows from.",
    ),
    (
        "c13_density_per_depth",
        "Places separate notes along the path, fewer on deeper branches.",
    ),
    (
        "c14_velocity_per_depth",
        "Changes the velocity with each branch depth.",
    ),
    ("c14_velocity_per_depth.start", "The velocity of the trunk."),
    (
        "c14_velocity_per_depth.step",
        "How much the velocity changes per branch depth (negative = softer).",
    ),
    (
        "c15_gate_per_depth",
        "Changes the note lengths with each branch depth.",
    ),
    (
        "c15_gate_per_depth.start",
        "The gate (share of each note's length) of the trunk.",
    ),
    (
        "c15_gate_per_depth.factor",
        "Each branch depth's gate as a share of the one before.",
    ),
    (
        "c16_subchannel_per_depth",
        "Moves each branch depth to the next subchannel (color).",
    ),
    (
        "c16_subchannel_per_depth.step",
        "How many subchannel positions each branch depth moves.",
    ),
    (
        "c17_branch_pruning",
        "Removes branches that are too deep, and others at random (seeded).",
    ),
    (
        "c17_branch_pruning.depth",
        "The deepest branch level that is kept.",
    ),
    (
        "c17_branch_pruning.share",
        "The chance that any branch is dropped.",
    ),
    (
        "c18_tropism_direction_bias",
        "Bends the growth toward a direction, like a plant leaning toward light.",
    ),
    (
        "c18_tropism_direction_bias.strength",
        "How far each step bends toward the direction.",
    ),
    (
        "c19_growth_noise",
        "Varies each turn and step a little at random (seeded), for a natural look.",
    ),
    (
        "c19_growth_noise.angle",
        "The largest random change of a turn, in degrees.",
    ),
    (
        "c19_growth_noise.length",
        "The largest random change of a step, as a share of the step.",
    ),
    (
        "c20_bounding_box_fit",
        "Fits the drawing into the box while keeping its proportions instead of stretching it.",
    ),
    (
        "c20_bounding_box_fit.mode",
        "Keep the whole shape inside, fill the box's width, or fill its height.",
    ),
    (
        "c20_bounding_box_fit.margin",
        "The space kept free around the drawing, as a share of the box.",
    ),
    (
        "c21_mirror_symmetry",
        "Adds mirrored and turned copies of the drawing around its start.",
    ),
    (
        "c21_mirror_symmetry.axis",
        "The mirror: none, in pitch about the start's row, in time about the start, or both.",
    ),
    (
        "c22_wrap_reflect",
        "Brings parts that leave the box back inside it.",
    ),
    (
        "c22_wrap_reflect.mode",
        "Wrap Round brings them back in at the other side; Reflect bounces them off the edges.",
    ),
    (
        "c23_branch_join",
        "Draws lines between branch tips, for a web or a net.",
    ),
    (
        "c23_branch_join.mode",
        "Join the tips of branches that grow from the same point, or join each tip back to where its branch began.",
    ),
    (
        "c24_multi_axiom_forest",
        "Grows several plants side by side, each from its own axiom.",
    ),
    ("c24_multi_axiom_forest.count", "How many plants grow."),
    (
        "c24_multi_axiom_forest.gap",
        "The space between plants, as a share of the widest plant.",
    ),
    (
        "c24_multi_axiom_forest.sub_step",
        "How many subchannel positions each plant moves from the previous one.",
    ),
    (
        "c25_rule_set_morph_evolution",
        "Changes the rules gradually to a second set, or draws every generation in turn.",
    ),
    (
        "c25_rule_set_morph_evolution.mode",
        "Morph switches to rule set B over the generations; Evolution draws each generation one after another along time.",
    ),
    (
        "c25_rule_set_morph_evolution.rules",
        "The second set of rules, written like the rules above.",
    ),
    (
        "c25_rule_set_morph_evolution.gap",
        "The space between the generations drawn by Evolution, as a share of a generation's width.",
    ),
];

fn lsystem_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        flag("c01_axiom", "Axiom", false),
        text("c01_axiom.axiom", "Axiom", RULE_LEN, "[-X]X[+X]")
            .help("The string the rewriting starts from."),
        flag("c02_rewrite_rules", "Rewrite Rules", false),
        text("c02_rewrite_rules.rules", "Rules", 2048, "X=F[+X]F[-X]+X; F=FF").help(
            "Each rule P=successor replaces symbol P every rewrite (`;` between rules); symbols without a rule stay. F G draw, f g move, + - turn, | turns back, [ ] branch.",
        ),
        flag("c03_iteration_depth", "Iteration Depth", false),
        int("c03_iteration_depth.depth", "Rewrites", 0, i64::from(MAX_DEPTH), 6),
        flag("c04_turtle_step_length", "Turtle Step Length", false),
        float("c04_turtle_step_length.step", "Step (grid units)", 0.01, 64.0, 0.25, "")
            .help("The turtle's step on the grid (a step across is a row up), drawn from the anchor at that size and cut at the box (Bounding-Box Fit wins over it)."),
        choice("c04_turtle_step_length.anchor", "Start at", ANCHORS, 0),
        flag("c05_turn_angle", "Turn Angle", false),
        float("c05_turn_angle.angle", "Angle", 0.0, 180.0, 30.0, "°"),
        flag("c06_branch_push_pop", "Branch Push/Pop", false),
        float("c06_branch_push_pop.length", "Step per branch level", 10.0, 100.0, 70.0, "%")
            .help("Each [ multiplies the step by this (] restores it)."),
        float("c06_branch_push_pop.turn", "Extra turn per branch", -90.0, 90.0, 0.0, "°"),
        flag("c07_multiple_symbols", "Multiple Symbols", false),
        text("c07_multiple_symbols.letters", "Letters that also draw", 8, "X")
            .help("Each listed letter draws a step too (besides F and G), with its own length and subchannel."),
        float("c07_multiple_symbols.length", "Their step (% of F's)", 1.0, 200.0, 50.0, "%"),
        int("c07_multiple_symbols.sub_step", "Subchannel step per letter", 0, 16, 1),
        flag("c08_stochastic_rules_with_seed", "Stochastic Rules with Seed", false),
        text(
            "c08_stochastic_rules_with_seed.rules",
            "More rules",
            2048,
            "X=F+[[X]-X]-F[-FX]+X",
        )
        .help("Added to the rules: a symbol with several rules takes one at random (by the seed) at each place, by their weights."),
        flag("c09_rule_weights", "Rule Weights", false),
        list("c09_rule_weights.weights", "Weights (%)", 0, 1000, 64, &[100, 50]).help(
            "Weight k for rule k (the rules, then the more rules; missing: 100). A symbol's rules share 100 %: below it, the symbol may stay as it is; above, they are scaled to 100 %.",
        ),
        flag("c10_time_scale_per_generation", "Time Scale Per Generation", false),
        float("c10_time_scale_per_generation.k", "Time scale per generation", 10.0, 200.0, 75.0, "%")
            .help("A step's time extent times this to the power of its generation (when its line of growth began)."),
        flag("c11_pitch_scale_per_generation", "Pitch Scale Per Generation", false),
        float("c11_pitch_scale_per_generation.k", "Pitch scale per generation", 10.0, 200.0, 75.0, "%"),
        flag("c12_thickness_per_depth", "Thickness Per Depth", false),
        float("c12_thickness_per_depth.rows", "Trunk thickness (rows)", 1.0, 16.0, 3.0, ""),
        float("c12_thickness_per_depth.factor", "Per branch depth", 10.0, 100.0, 60.0, "%"),
        flag("c13_density_per_depth", "Density Per Depth", false),
        float("c13_density_per_depth.factor", "Notes kept per depth", 0.0, 100.0, 70.0, "%")
            .help("A note every grid unit along the path, a share of them kept: this to the power of the branch depth."),
        flag("c14_velocity_per_depth", "Velocity Per Depth", false),
        int("c14_velocity_per_depth.start", "Trunk velocity", 1, 127, 120),
        int("c14_velocity_per_depth.step", "Change per depth", -64, 64, -20),
        flag("c15_gate_per_depth", "Gate Per Depth", false),
        float("c15_gate_per_depth.start", "Trunk gate", 1.0, 100.0, 100.0, "%"),
        float("c15_gate_per_depth.factor", "Per depth", 10.0, 100.0, 60.0, "%"),
        flag("c16_subchannel_per_depth", "Subchannel Per Depth", false),
        int("c16_subchannel_per_depth.step", "Subchannel step per depth", 0, 64, 1),
        flag("c17_branch_pruning", "Branch Pruning", false),
        int("c17_branch_pruning.depth", "Deepest branch kept", 0, 64, 3),
        float("c17_branch_pruning.share", "Branches dropped at random", 0.0, 100.0, 20.0, "%"),
        flag("c18_tropism_direction_bias", "Tropism / Direction Bias", false),
        float("c18_tropism_direction_bias.angle", "Towards", -180.0, 180.0, -90.0, "°")
            .help("0° along time, 90° up in pitch."),
        float("c18_tropism_direction_bias.strength", "Strength per step", 0.0, 100.0, 15.0, "%"),
        flag("c19_growth_noise", "Growth Noise", false),
        float("c19_growth_noise.angle", "Turn jitter (±°)", 0.0, 90.0, 8.0, ""),
        float("c19_growth_noise.length", "Step jitter (±%)", 0.0, 90.0, 20.0, ""),
        flag("c20_bounding_box_fit", "Bounding-Box Fit", false),
        choice("c20_bounding_box_fit.mode", "Fit", FITS, 0),
        float("c20_bounding_box_fit.margin", "Margin", 0.0, 45.0, 5.0, "%"),
        flag("c21_mirror_symmetry", "Mirror / Symmetry", false),
        choice("c21_mirror_symmetry.axis", "Mirror", AXES, 1),
        int("c21_mirror_symmetry.copies", "Radial copies", 1, 12, 1)
            .help("Copies turned evenly about the start (1: none)."),
        flag("c22_wrap_reflect", "Wrap / Reflect", false),
        choice("c22_wrap_reflect.mode", "Outside the box", WRAPS, 0),
        float("c22_wrap_reflect.extent", "Extent", 1.0, 16.0, 2.0, "×")
            .help("The drawing scaled this much about the box's center, then brought back: wrapped round or reflected at the edges (cut exactly where it crosses them)."),
        flag("c23_branch_join", "Branch Join", false),
        choice("c23_branch_join.mode", "Join", JOINS, 0),
        flag("c24_multi_axiom_forest", "Multi-Axiom Forest", false),
        int("c24_multi_axiom_forest.count", "Plants", 2, 8, 3),
        text("c24_multi_axiom_forest.axioms", "Axioms (; between)", 1024, "X; FX; X")
            .help("Plant k grows from axiom k (cycled; an empty one: the axiom above), each with its own random stream."),
        float("c24_multi_axiom_forest.gap", "Gap (% of the widest plant)", 0.0, 200.0, 15.0, "%"),
        int("c24_multi_axiom_forest.sub_step", "Subchannel step per plant", 0, 16, 0),
        flag("c25_rule_set_morph_evolution", "Rule-Set Morph / Evolution", false),
        choice("c25_rule_set_morph_evolution.mode", "Mode", MORPHS, 0),
        text(
            "c25_rule_set_morph_evolution.rules",
            "Rule set B",
            2048,
            "X=F[+X][-X]FX; F=FF",
        ),
        float("c25_rule_set_morph_evolution.amount", "Morph", 0.0, 100.0, 100.0, "%")
            .help("At rewrite g of n, a symbol takes rule set B's rule with this share × g / n (seeded)."),
        float("c25_rule_set_morph_evolution.gap", "Gap between generations", 0.0, 200.0, 10.0, "%"),
    ];
    with_help(&mut own, LSYSTEM_HELP);
    super::spec(
        "gen.lsystem",
        "Fractal / L-System",
        "Generates notes along an L-system's turtle path: the fractal plant to start from, rewritten 5 times and stretched over the generation box (where, length and pitch range above).",
        own,
        FUNCTIONS,
    )
}

/// `what` with its first letter in capitals.
fn cap(what: &str) -> String {
    let mut c = what.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Checks a string of symbols (`what` names it in sentences): ASCII signs,
/// no `=` or `;`, at most [`RULE_LEN`], brackets balanced.
pub fn symbols(s: &str, what: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(c) = s
        .chars()
        .find(|c| !c.is_ascii_graphic() || *c == '=' || *c == ';')
    {
        return Err(format!(
            "The symbol “{c}” in {what} is not allowed — use letters, digits and signs such as + - | [ ]."
        ));
    }
    if s.len() > RULE_LEN {
        return Err(format!("{} is longer than {RULE_LEN} symbols.", cap(what)));
    }
    let mut open = 0usize;
    for b in s.bytes() {
        match b {
            b'[' => open += 1,
            b']' => {
                if open == 0 {
                    return Err(format!("{} has a ] without its [.", cap(what)));
                }
                open -= 1;
            }
            _ => {}
        }
    }
    if open > 0 {
        return Err(format!("{} leaves {open} [ open.", cap(what)));
    }
    Ok(s.into_bytes())
}

/// Parses rules `P=successor` separated by `;` or new lines; `what` names
/// the set in sentences.
pub fn parse_rules(text: &str, what: &str) -> Result<Vec<(u8, Vec<u8>)>, String> {
    let mut out = Vec::new();
    for entry in text.split([';', '\n']) {
        let e: String = entry.chars().filter(|c| !c.is_whitespace()).collect();
        if e.is_empty() {
            continue;
        }
        let n = out.len() + 1;
        if n > MAX_RULES {
            return Err(format!("{} has more than {MAX_RULES} rules.", cap(what)));
        }
        let Some((p, s)) = e.split_once('=') else {
            return Err(format!(
                "Rule {n} of {what} has no = — write it as X=F[+X]."
            ));
        };
        let pb = p.as_bytes();
        if pb.len() != 1 || !pb[0].is_ascii_graphic() || matches!(pb[0], b'[' | b']' | b';') {
            return Err(format!(
                "Rule {n} of {what} needs one symbol before = (it has “{p}”)."
            ));
        }
        out.push((pb[0], symbols(s, &format!("rule {n} ({p}) of {what}"))?));
    }
    Ok(out)
}

/// Rules by predecessor: successors with their weights (shares of 1).
#[derive(Clone, Debug)]
struct Set {
    by: Vec<Vec<(Vec<u8>, f64)>>,
}

impl Set {
    fn new() -> Set {
        Set {
            by: vec![Vec::new(); 128],
        }
    }

    fn add(&mut self, rules: Vec<(u8, Vec<u8>)>, weights: &mut dyn Iterator<Item = f64>) {
        for (p, s) in rules {
            let w = weights.next().unwrap_or(1.0);
            self.by[usize::from(p)].push((s, w));
        }
    }

    fn has(&self, s: u8) -> bool {
        !self.by[usize::from(s)].is_empty()
    }
}

/// The functions' settings.
#[derive(Clone, Debug)]
struct Cfg {
    axiom: Vec<u8>,
    a: Set,
    /// c25 morph: rule set B and its share.
    b: Option<(Set, f64)>,
    /// c25 evolution: the gap between generations (share of the widest).
    evolution: Option<f64>,
    depth: u32,
    angle: f64,
    /// c04: grid units, anchor.
    step: Option<(f64, usize)>,
    /// c06: step factor per level, extra turn (radians).
    branch: Option<(f64, f64)>,
    /// c07: drawing letters, their step, subchannel step.
    letters: Vec<u8>,
    letter_len: f64,
    letter_sub: u32,
    tscale: Option<f64>,
    pscale: Option<f64>,
    /// c12: trunk rows, factor.
    thick: Option<(f64, f64)>,
    density: Option<f64>,
    /// c14: trunk velocity, change per depth.
    vel: Option<(f64, f64)>,
    /// c15: trunk gate (share), factor.
    gate: Option<(f64, f64)>,
    depth_sub: u32,
    /// c17: deepest kept, random share.
    prune: Option<(u32, f64)>,
    /// c18: direction (radians), strength.
    tropism: Option<(f64, f64)>,
    /// c19: turn jitter (radians), step jitter (share).
    noise: Option<(f64, f64)>,
    /// c20: mode, margin.
    fit: Option<(usize, f64)>,
    /// c21: axis, radial copies.
    mirror: (usize, u32),
    /// c22: reflect?, extent.
    wrap: Option<(bool, f64)>,
    join: Option<usize>,
    /// c24: the plants' axioms, gap, subchannel step.
    forest: Option<(Vec<Vec<u8>>, f64, u32)>,
}

impl Cfg {
    fn read(p: &Params) -> Result<Cfg, TransformError> {
        let on = |id: &str| p.flag(id);
        let pct = |k: &str| p.float(k) / 100.0;
        let bad = TransformError::Invalid;
        let mut axiom = symbols(BASE_AXIOM, "the axiom").map_err(bad)?;
        if on("c01_axiom") {
            axiom = symbols(&p.text("c01_axiom.axiom"), "the axiom").map_err(bad)?;
        }
        let mut rules = parse_rules(BASE_RULES, "the rules").map_err(bad)?;
        if on("c02_rewrite_rules") {
            rules = parse_rules(&p.text("c02_rewrite_rules.rules"), "the rewrite rules")
                .map_err(bad)?;
        }
        let more = if on("c08_stochastic_rules_with_seed") {
            parse_rules(
                &p.text("c08_stochastic_rules_with_seed.rules"),
                "the more rules",
            )
            .map_err(bad)?
        } else {
            Vec::new()
        };
        let weights: Vec<f64> = if on("c09_rule_weights") {
            p.list("c09_rule_weights.weights")
                .iter()
                .map(|&w| w.clamp(0, 1000) as f64 / 100.0)
                .collect()
        } else {
            Vec::new()
        };
        let mut a = Set::new();
        let mut wi = weights.into_iter();
        a.add(rules, &mut wi);
        a.add(more, &mut wi);
        let mut c = Cfg {
            axiom,
            a,
            b: None,
            evolution: None,
            depth: BASE_DEPTH,
            angle: BASE_ANGLE.to_radians(),
            step: None,
            branch: None,
            letters: Vec::new(),
            letter_len: 1.0,
            letter_sub: 0,
            tscale: None,
            pscale: None,
            thick: None,
            density: None,
            vel: None,
            gate: None,
            depth_sub: 0,
            prune: None,
            tropism: None,
            noise: None,
            fit: None,
            mirror: (0, 1),
            wrap: None,
            join: None,
            forest: None,
        };
        if on("c03_iteration_depth") {
            c.depth = p
                .int("c03_iteration_depth.depth")
                .clamp(0, i64::from(MAX_DEPTH)) as u32;
        }
        if on("c04_turtle_step_length") {
            c.step = Some((
                p.float("c04_turtle_step_length.step").clamp(0.01, 64.0),
                p.choice("c04_turtle_step_length.anchor")
                    .min(ANCHORS.len() - 1),
            ));
        }
        if on("c05_turn_angle") {
            c.angle = p
                .float("c05_turn_angle.angle")
                .clamp(0.0, 180.0)
                .to_radians();
        }
        if on("c06_branch_push_pop") {
            c.branch = Some((
                pct("c06_branch_push_pop.length").clamp(0.1, 1.0),
                p.float("c06_branch_push_pop.turn")
                    .clamp(-90.0, 90.0)
                    .to_radians(),
            ));
        }
        if on("c07_multiple_symbols") {
            let t = p.text("c07_multiple_symbols.letters");
            for ch in t.chars().filter(|c| !c.is_whitespace()) {
                if !ch.is_ascii_alphabetic() || matches!(ch, 'F' | 'G' | 'f' | 'g') {
                    return Err(bad(format!(
                        "“{ch}” cannot be a drawing letter — list letters other than F, G, f and g."
                    )));
                }
                let b = ch as u8;
                if !c.letters.contains(&b) {
                    c.letters.push(b);
                }
            }
            c.letter_len = pct("c07_multiple_symbols.length").clamp(0.01, 2.0);
            c.letter_sub = p.int("c07_multiple_symbols.sub_step").clamp(0, 16) as u32;
        }
        if on("c10_time_scale_per_generation") {
            c.tscale = Some(pct("c10_time_scale_per_generation.k").clamp(0.1, 2.0));
        }
        if on("c11_pitch_scale_per_generation") {
            c.pscale = Some(pct("c11_pitch_scale_per_generation.k").clamp(0.1, 2.0));
        }
        if on("c12_thickness_per_depth") {
            c.thick = Some((
                p.float("c12_thickness_per_depth.rows").clamp(1.0, 16.0),
                pct("c12_thickness_per_depth.factor").clamp(0.1, 1.0),
            ));
        }
        if on("c13_density_per_depth") {
            c.density = Some(pct("c13_density_per_depth.factor").clamp(0.0, 1.0));
        }
        if on("c14_velocity_per_depth") {
            c.vel = Some((
                p.int("c14_velocity_per_depth.start").clamp(1, 127) as f64,
                p.int("c14_velocity_per_depth.step").clamp(-64, 64) as f64,
            ));
        }
        if on("c15_gate_per_depth") {
            c.gate = Some((
                pct("c15_gate_per_depth.start").clamp(0.01, 1.0),
                pct("c15_gate_per_depth.factor").clamp(0.1, 1.0),
            ));
        }
        if on("c16_subchannel_per_depth") {
            c.depth_sub = p.int("c16_subchannel_per_depth.step").clamp(0, 64) as u32;
        }
        if on("c17_branch_pruning") {
            c.prune = Some((
                p.int("c17_branch_pruning.depth").clamp(0, 64) as u32,
                pct("c17_branch_pruning.share").clamp(0.0, 1.0),
            ));
        }
        if on("c18_tropism_direction_bias") {
            c.tropism = Some((
                p.float("c18_tropism_direction_bias.angle")
                    .clamp(-180.0, 180.0)
                    .to_radians(),
                pct("c18_tropism_direction_bias.strength").clamp(0.0, 1.0),
            ));
        }
        if on("c19_growth_noise") {
            c.noise = Some((
                p.float("c19_growth_noise.angle")
                    .clamp(0.0, 90.0)
                    .to_radians(),
                pct("c19_growth_noise.length").clamp(0.0, 0.9),
            ));
        }
        if on("c20_bounding_box_fit") {
            c.fit = Some((
                p.choice("c20_bounding_box_fit.mode").min(FITS.len() - 1),
                pct("c20_bounding_box_fit.margin").clamp(0.0, 0.45),
            ));
        }
        if on("c21_mirror_symmetry") {
            c.mirror = (
                p.choice("c21_mirror_symmetry.axis").min(AXES.len() - 1),
                p.int("c21_mirror_symmetry.copies").clamp(1, 12) as u32,
            );
        }
        if on("c22_wrap_reflect") {
            c.wrap = Some((
                p.choice("c22_wrap_reflect.mode") == 1,
                p.float("c22_wrap_reflect.extent").clamp(1.0, 16.0),
            ));
        }
        if on("c23_branch_join") {
            c.join = Some(p.choice("c23_branch_join.mode").min(JOINS.len() - 1));
        }
        if on("c24_multi_axiom_forest") {
            let k = "c24_multi_axiom_forest";
            let n = p.int(&format!("{k}.count")).clamp(2, 8) as usize;
            let mut list = Vec::new();
            for (i, e) in p.text(&format!("{k}.axioms")).split(';').enumerate() {
                if i >= 8 {
                    break;
                }
                let e = e.trim();
                list.push(if e.is_empty() {
                    c.axiom.clone()
                } else {
                    symbols(e, &format!("plant {}'s axiom", i + 1)).map_err(bad)?
                });
            }
            if list.is_empty() {
                list.push(c.axiom.clone());
            }
            let axioms = (0..n).map(|i| list[i % list.len()].clone()).collect();
            c.forest = Some((
                axioms,
                pct(&format!("{k}.gap")).clamp(0.0, 2.0),
                p.int(&format!("{k}.sub_step")).clamp(0, 16) as u32,
            ));
        }
        if on("c25_rule_set_morph_evolution") {
            let k = "c25_rule_set_morph_evolution";
            if p.choice(&format!("{k}.mode")) == 1 {
                c.evolution = Some(pct(&format!("{k}.gap")).clamp(0.0, 2.0));
            } else {
                let mut b = Set::new();
                b.add(
                    parse_rules(&p.text(&format!("{k}.rules")), "rule set B").map_err(bad)?,
                    &mut std::iter::empty(),
                );
                c.b = Some((b, pct(&format!("{k}.amount")).clamp(0.0, 1.0)));
            }
        }
        Ok(c)
    }

    /// Whether symbol `s` can be rewritten.
    fn rewrites(&self, s: u8) -> bool {
        self.a.has(s)
            || self
                .b
                .as_ref()
                .is_some_and(|(b, amt)| *amt > 0.0 && b.has(s))
    }

    /// The class of a drawing symbol: 0 for F / G, k + 1 for c07's letter k.
    fn class(&self, s: u8) -> Option<u32> {
        if s == b'F' || s == b'G' {
            return Some(0);
        }
        self.letters
            .iter()
            .position(|&l| l == s)
            .map(|k| k as u32 + 1)
    }

    /// The successor symbol `s` takes at rewrite `g` (1 – depth) at the
    /// place keyed `key`, or `None` to stay as it is.
    fn choose(&self, seed: u64, s: u8, g: u32, key: u64) -> Option<&[u8]> {
        let unit = |tag: u64| (hash(&[seed, tag, key]) >> 11) as f64 / (1u64 << 53) as f64;
        if let Some((b, amt)) = &self.b {
            let alts = &b.by[usize::from(s)];
            if !alts.is_empty() && unit(T_MORPH) < amt * f64::from(g) / f64::from(self.depth.max(1))
            {
                let k = ((unit(T_PICKB) * alts.len() as f64) as usize).min(alts.len() - 1);
                return Some(&alts[k].0);
            }
        }
        let alts = &self.a.by[usize::from(s)];
        if alts.is_empty() {
            return None;
        }
        let total: f64 = alts.iter().map(|a| a.1).sum();
        if alts.len() == 1 && total >= 1.0 {
            return Some(&alts[0].0);
        }
        if total <= 0.0 {
            return None;
        }
        let u = unit(T_PICK);
        let scale = total.max(1.0);
        let mut acc = 0.0;
        for (succ, w) in alts {
            acc += w / scale;
            if u < acc {
                return Some(succ);
            }
        }
        // Rounding: the rules share all of it.
        if total >= 1.0 {
            alts.iter()
                .rev()
                .find(|a| a.1 > 0.0)
                .map(|a| a.0.as_slice())
        } else {
            None
        }
    }

    /// Upper bounds per symbol after 0, 1, … `depth` rewrites: the symbols,
    /// the segments drawn and the branches closed.
    fn bounds(&self, depth: u32) -> Vec<Vec<[u128; 3]>> {
        let draws = |s: u8| u128::from(self.class(s).is_some());
        let mut levels = vec![
            (0..128u8)
                .map(|s| [1, draws(s), u128::from(s == b']')])
                .collect::<Vec<_>>(),
        ];
        for _ in 0..depth {
            let prev = levels.last().expect("a level");
            let mut cur = prev.clone();
            for s in 0..128u8 {
                if !self.rewrites(s) {
                    continue;
                }
                let sum = |succ: &[u8]| {
                    let mut t = [0u128; 3];
                    for &c in succ {
                        for (j, v) in t.iter_mut().enumerate() {
                            *v = v.saturating_add(prev[usize::from(c)][j]);
                        }
                    }
                    t
                };
                let mut best = [0u128; 3];
                let mut take = |t: [u128; 3]| {
                    for j in 0..3 {
                        best[j] = best[j].max(t[j]);
                    }
                };
                let alts = &self.a.by[usize::from(s)];
                let total: f64 = alts.iter().map(|a| a.1).sum();
                for (succ, w) in alts {
                    if *w > 0.0 {
                        take(sum(succ));
                    }
                }
                let mut keep = alts.is_empty() || total < 1.0;
                if let Some((b, amt)) = &self.b
                    && *amt > 0.0
                {
                    for (succ, _) in &b.by[usize::from(s)] {
                        take(sum(succ));
                    }
                    keep |= *amt < 1.0;
                }
                if keep {
                    take(prev[usize::from(s)]);
                }
                cur[usize::from(s)] = best;
            }
            levels.push(cur);
        }
        levels
    }

    /// The plants and stages drawn: axiom, rewrites, random stream, plant.
    fn items(&self, seed: u64) -> Vec<(&[u8], u32, u64, usize)> {
        let plants: Vec<&[u8]> = match &self.forest {
            Some((ax, _, _)) => ax.iter().map(|a| a.as_slice()).collect(),
            None => vec![&self.axiom],
        };
        let stages: Vec<u32> = if self.evolution.is_some() && self.depth > 0 {
            (1..=self.depth).collect()
        } else {
            vec![self.depth]
        };
        let mut out = Vec::new();
        for (k, ax) in plants.into_iter().enumerate() {
            let s = if k == 0 {
                seed
            } else {
                hash(&[seed, 0x504C_414E, k as u64])
            };
            for &d in &stages {
                out.push((ax, d, s, k));
            }
        }
        out
    }

    /// Copies per drawing (mirror × radial).
    fn copies(&self) -> u128 {
        let m = match self.mirror.0 {
            0 => 1,
            3 => 4,
            _ => 2,
        };
        m * u128::from(self.mirror.1)
    }
}

/// One piece of the turtle's path: its points, branch depth, symbol class
/// and plant.
#[derive(Clone, Debug)]
struct Raw {
    pts: Vec<(f64, f64)>,
    depth: u32,
    class: u32,
    plant: usize,
}

#[derive(Clone, Copy, Debug)]
struct St {
    x: f64,
    y: f64,
    h: f64,
    depth: u32,
    len: f64,
}

/// The turtle reading the symbols as the rewriting makes them.
struct Turtle<'a> {
    cfg: &'a Cfg,
    seed: u64,
    plant: usize,
    st: St,
    stack: Vec<St>,
    /// c23: the last closed branch's tip at each open level.
    tips: Vec<Option<(f64, f64)>>,
    /// Inside a pruned branch: its open brackets.
    skip: u32,
    cur: Option<usize>,
    out: Vec<Raw>,
}

impl Turtle<'_> {
    fn unit(&self, tag: u64, key: u64) -> f64 {
        (hash(&[self.seed, tag, key]) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn seg(&mut self, a: (f64, f64), b: (f64, f64), depth: u32, class: u32) {
        match self.cur {
            Some(i) if self.out[i].depth == depth && self.out[i].class == class => {
                self.out[i].pts.push(b);
            }
            _ => {
                self.out.push(Raw {
                    pts: vec![a, b],
                    depth,
                    class,
                    plant: self.plant,
                });
                self.cur = Some(self.out.len() - 1);
            }
        }
    }

    fn join(&mut self, a: (f64, f64), b: (f64, f64), depth: u32) {
        self.out.push(Raw {
            pts: vec![a, b],
            depth,
            class: 0,
            plant: self.plant,
        });
    }

    fn leaf(&mut self, s: u8, origin: u32, key: u64) {
        let c = self.cfg;
        if self.skip > 0 {
            match s {
                b'[' => self.skip += 1,
                b']' => self.skip -= 1,
                _ => {}
            }
            return;
        }
        match s {
            b'+' | b'-' => {
                let mut a = c.angle;
                if let Some((j, _)) = c.noise {
                    a += j * (2.0 * self.unit(T_TURN, key) - 1.0);
                }
                self.st.h += if s == b'+' { a } else { -a };
            }
            b'|' => self.st.h += PI,
            b'[' => {
                if let Some((deepest, share)) = c.prune
                    && (self.st.depth + 1 > deepest
                        || (share > 0.0 && self.unit(T_PRUNE, key) < share))
                {
                    self.skip = 1;
                    return;
                }
                self.stack.push(self.st);
                self.tips.push(None);
                self.st.depth += 1;
                if let Some((k, turn)) = c.branch {
                    self.st.len *= k;
                    self.st.h += turn;
                }
                self.cur = None;
            }
            b']' => {
                let Some(prev) = self.stack.pop() else {
                    return;
                };
                let tip = (self.st.x, self.st.y);
                let d = self.st.depth;
                self.tips.pop();
                self.st = prev;
                self.cur = None;
                match c.join {
                    Some(0) => {
                        if let Some(Some(t0)) = self.tips.last().copied() {
                            self.join(t0, tip, d);
                        }
                        if let Some(t) = self.tips.last_mut() {
                            *t = Some(tip);
                        }
                    }
                    Some(_) => {
                        let at = (self.st.x, self.st.y);
                        self.join(tip, at, d);
                    }
                    None => {}
                }
            }
            _ => {
                let class = c.class(s);
                let moves = s == b'f' || s == b'g';
                if class.is_none() && !moves {
                    return;
                }
                let mut l = self.st.len;
                if class.is_some_and(|k| k > 0) {
                    l *= c.letter_len;
                }
                if let Some((_, j)) = c.noise {
                    l *= 1.0 + j * (2.0 * self.unit(T_LEN, key) - 1.0);
                }
                let (sn, cs) = self.st.h.sin_cos();
                let (mut dx, mut dy) = (l * cs, l * sn);
                let e = origin.min(64) as i32;
                if let Some(k) = c.tscale {
                    dx *= k.powi(e);
                }
                if let Some(k) = c.pscale {
                    dy *= k.powi(e);
                }
                let a = (self.st.x, self.st.y);
                self.st.x += dx;
                self.st.y += dy;
                match class {
                    Some(k) => self.seg(a, (self.st.x, self.st.y), self.st.depth, k),
                    None => self.cur = None,
                }
                if let Some((dir, k)) = c.tropism {
                    self.st.h += k * (dir - self.st.h).sin();
                }
            }
        }
    }
}

/// The rewriting, depth first.
struct Walk<'a, 'b> {
    cfg: &'a Cfg,
    seed: u64,
    depth: u32,
    turtle: &'b mut Turtle<'a>,
}

impl Walk<'_, '_> {
    /// Symbol `s` of generation `origin` at the place keyed `key`, after
    /// `done` rewrites.
    fn walk(&mut self, s: u8, origin: u32, key: u64, done: u32) {
        let cfg = self.cfg;
        if done >= self.depth || !cfg.rewrites(s) {
            self.turtle.leaf(s, origin, key);
            return;
        }
        let g = done + 1;
        match cfg.choose(self.seed, s, g, key) {
            None => self.walk(s, origin, hash(&[key, T_KEEP]), g),
            Some(succ) => {
                for (i, &c) in succ.iter().enumerate() {
                    let o = if c == s { origin } else { g };
                    self.walk(c, o, hash(&[key, i as u64 + 1]), g);
                }
            }
        }
    }
}

/// Turns `p` about the origin by (sin, cos) `r`.
fn turn(p: (f64, f64), r: (f64, f64)) -> (f64, f64) {
    (p.0 * r.1 - p.1 * r.0, p.0 * r.0 + p.1 * r.1)
}

/// The box of `pieces` (min x, min y, max x, max y), if any.
fn extent(pieces: &[Raw]) -> Option<[f64; 4]> {
    let mut e: Option<[f64; 4]> = None;
    for p in pieces.iter().flat_map(|r| r.pts.iter()) {
        let b = e.get_or_insert([p.0, p.1, p.0, p.1]);
        b[0] = b[0].min(p.0);
        b[1] = b[1].min(p.1);
        b[2] = b[2].max(p.0);
        b[3] = b[3].max(p.1);
    }
    e
}

/// What is drawn: the pieces (grid units) with their styles and brushes.
#[derive(Debug, Default)]
struct Built {
    paths: Vec<(Vec<Pt>, PathStyle, Brush)>,
}

struct LSystem {
    cfg: Cfg,
    built: OnceLock<Result<Built, TransformError>>,
}

impl LSystem {
    fn new(cfg: Cfg) -> LSystem {
        LSystem {
            cfg,
            built: OnceLock::new(),
        }
    }

    /// The caps' sentences, counted before anything is rewritten.
    fn check(&self, seed: u64) -> Result<(), TransformError> {
        let c = &self.cfg;
        let levels = c.bounds(c.depth);
        let (mut syms, mut segs) = (0u128, 0u128);
        for (ax, d, _, _) in c.items(seed) {
            let lv = &levels[d as usize];
            for &s in ax {
                let b = lv[usize::from(s)];
                syms = syms.saturating_add(b[0]);
                let j = if c.join.is_some() { b[2] } else { 0 };
                segs = segs.saturating_add(b[1].saturating_add(j));
            }
        }
        segs = segs.saturating_mul(c.copies());
        let say = |n: u128| {
            if n > u128::from(u64::MAX) {
                "more than 18 quintillion".to_string()
            } else {
                group_digits(n as u64)
            }
        };
        if syms > SYMBOL_CAP {
            return Err(TransformError::TooLarge(format!(
                "The L-system could grow to {} symbols; at most {} are allowed — use fewer rewrites, shorter rules, or fewer plants or generations.",
                say(syms),
                say(SYMBOL_CAP)
            )));
        }
        if segs > SEGMENT_CAP {
            return Err(TransformError::TooLarge(format!(
                "The L-system could draw {} segments (copies and joins included); at most {} are allowed — use fewer rewrites, copies or plants.",
                say(segs),
                say(SEGMENT_CAP)
            )));
        }
        Ok(())
    }

    /// The turtle's pieces of every plant and stage, copied, laid out.
    fn grow(&self, seed: u64) -> Vec<Raw> {
        let c = &self.cfg;
        let mut items: Vec<Vec<Raw>> = Vec::new();
        for (ax, d, s, plant) in c.items(seed) {
            let mut t = Turtle {
                cfg: c,
                seed: s,
                plant,
                st: St {
                    x: 0.0,
                    y: 0.0,
                    h: 0.0,
                    depth: 0,
                    len: 1.0,
                },
                stack: Vec::new(),
                tips: vec![None],
                skip: 0,
                cur: None,
                out: Vec::new(),
            };
            {
                let mut w = Walk {
                    cfg: c,
                    seed: s,
                    depth: d,
                    turtle: &mut t,
                };
                for (i, &sym) in ax.iter().enumerate() {
                    w.walk(sym, 0, hash(&[s, 0x4158_494F, i as u64]), 0);
                }
            }
            let mut pieces = t.out;
            // c21: mirror, then radial copies, about the start.
            let (axis, n) = c.mirror;
            let flips: &[(f64, f64)] = match axis {
                1 => &[(1.0, -1.0)],
                2 => &[(-1.0, 1.0)],
                3 => &[(1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)],
                _ => &[],
            };
            let one = pieces.clone();
            for &(fx, fy) in flips {
                for r in &one {
                    let mut m = r.clone();
                    for p in &mut m.pts {
                        *p = (p.0 * fx, p.1 * fy);
                    }
                    pieces.push(m);
                }
            }
            if n > 1 {
                let one = pieces.clone();
                for k in 1..n {
                    let r = (TAU * f64::from(k) / f64::from(n)).sin_cos();
                    for p in &one {
                        let mut m = p.clone();
                        for q in &mut m.pts {
                            *q = turn(*q, r);
                        }
                        pieces.push(m);
                    }
                }
            }
            items.push(pieces);
        }
        if items.len() == 1 {
            return items.pop().unwrap_or_default();
        }
        // Side by side along time, starts on one row.
        let gap = match (&c.forest, c.evolution) {
            (Some((_, g, _)), _) => *g,
            (None, Some(g)) => g,
            _ => 0.0,
        };
        let boxes: Vec<Option<[f64; 4]>> = items.iter().map(|p| extent(p)).collect();
        let widest = boxes
            .iter()
            .flatten()
            .map(|b| b[2] - b[0])
            .fold(0.0f64, f64::max);
        let gap = gap * if widest > 0.0 { widest } else { 1.0 };
        let mut at = 0.0;
        let mut out = Vec::new();
        for (pieces, b) in items.into_iter().zip(boxes) {
            let Some(b) = b else {
                continue;
            };
            let dx = at - b[0];
            for mut r in pieces {
                for p in &mut r.pts {
                    p.0 += dx;
                }
                out.push(r);
            }
            at += b[2] - b[0] + gap;
        }
        out
    }

    /// The pieces as box shares (0 – 1 inside), after the fit, the wrap or
    /// the cut; a sentence when the wrap would make too many points.
    fn place(&self, raw: Vec<Raw>, gb: &Gb) -> Result<Vec<Raw>, TransformError> {
        let c = &self.cfg;
        let Some(e) = extent(&raw) else {
            return Ok(Vec::new());
        };
        let (w, h) = (e[2] - e[0], e[3] - e[1]);
        let (gw, gh) = (gb.u1 - gb.u0, gb.v1 - gb.v0);
        let share = |v: f64, size: f64| if size > 0.0 { v / size } else { 0.0 };
        let (cx, cy) = ((e[0] + e[2]) / 2.0, (e[1] + e[3]) / 2.0);
        type Map = Box<dyn Fn((f64, f64)) -> (f64, f64)>;
        let map: Map = match (c.fit, c.step) {
            (Some((mode, m)), _) => {
                let rx = if w > 0.0 {
                    (1.0 - 2.0 * m) * gw / w
                } else {
                    f64::INFINITY
                };
                let ry = if h > 0.0 {
                    (1.0 - 2.0 * m) * gh / h
                } else {
                    f64::INFINITY
                };
                let mut s = match mode {
                    1 => rx,
                    2 => ry,
                    _ => rx.min(ry),
                };
                if !s.is_finite() {
                    s = rx.min(ry);
                }
                if !s.is_finite() {
                    s = 0.0;
                }
                Box::new(move |p| {
                    (
                        0.5 + share(s * (p.0 - cx), gw),
                        0.5 + share(s * (p.1 - cy), gh),
                    )
                })
            }
            (None, Some((step, anchor))) => {
                let (ax, ay) = match anchor {
                    1 => (0.5, 0.5),
                    2 => (0.0, 0.0),
                    _ => (0.0, 0.5),
                };
                Box::new(move |p| (ax + share(step * p.0, gw), ay + share(step * p.1, gh)))
            }
            (None, None) => Box::new(move |p| {
                (
                    if w > 0.0 { (p.0 - e[0]) / w } else { 0.5 },
                    if h > 0.0 { (p.1 - e[1]) / h } else { 0.5 },
                )
            }),
        };
        let mut placed: Vec<Raw> = raw
            .into_iter()
            .map(|mut r| {
                for p in &mut r.pts {
                    *p = map(*p);
                }
                r
            })
            .collect();
        if let Some((reflect, k)) = c.wrap {
            // Count the points the cuts make before making them.
            let sc = |v: f64| 0.5 + (v - 0.5) * k;
            let mut n = 0.0f64;
            for r in &placed {
                n += r.pts.len() as f64;
                for q in r.pts.windows(2) {
                    for j in 0..2 {
                        let (a, b) = if j == 0 {
                            (q[0].0, q[1].0)
                        } else {
                            (q[0].1, q[1].1)
                        };
                        let (a, b) = (sc(a), sc(b));
                        n += (a.max(b).ceil() - a.min(b).floor()).max(0.0);
                    }
                }
                if !n.is_finite() || n > POINT_CAP {
                    return Err(TransformError::TooLarge(format!(
                        "Wrapping the L-system into the box would make more than {} points — use a smaller extent or step.",
                        group_digits(POINT_CAP as u64)
                    )));
                }
            }
            placed = placed.iter().flat_map(|r| fold(r, k, reflect)).collect();
        } else if c.step.is_some() && c.fit.is_none() || c.fit.is_some_and(|(m, _)| m > 0) {
            placed = placed.iter().flat_map(cut).collect();
        } else {
            for r in &mut placed {
                for p in &mut r.pts {
                    *p = (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0));
                }
            }
        }
        Ok(placed)
    }

    /// The pieces and styles, or a cap's sentence.
    fn build(&self, f: &Frame) -> Result<Built, TransformError> {
        let c = &self.cfg;
        self.check(f.seed)?;
        let g = BoxG::of(f);
        let gb = Gb::of(&g, 0.0);
        let placed = self.place(self.grow(f.seed), &gb)?;
        let m = Aff {
            a: g.step,
            ..Aff::ID
        };
        let mut visits = 0.0;
        let mut b = Built::default();
        for r in placed {
            if r.pts.len() < 2 {
                continue;
            }
            let mut pts: Vec<Pt> = r
                .pts
                .iter()
                .map(|&q| {
                    let (u, v) = gb.at(q);
                    Pt { x: u, y: v, s: 0.0 }
                })
                .collect();
            positions(&mut pts);
            let d = r.depth as i32;
            let mut st = PathStyle::default();
            if let Some((rows, k)) = c.thick {
                st.thick = (rows * k.powi(d)).max(1.0);
            }
            if let Some(k) = c.density {
                let share = k.powi(d).clamp(0.0, 1.0);
                if share <= 0.0 {
                    // Nothing kept: not visited at all.
                    continue;
                }
                st.every = Some((1.0, 1.0));
                st.keep = Some(Curve::new(vec![(0.0, share), (1.0, share)]));
                let q: Vec<Pt> = pts.iter().map(|&x| m.ap(x)).collect();
                visits += st.visits(&measure(&q, &g));
                if visits > VISIT_CAP {
                    return Err(too_many_visits(visits));
                }
            }
            let mut br = f.brush;
            if let Some((v0, dv)) = c.vel {
                br.vel = (v0 + dv * f64::from(r.depth)).round().clamp(1.0, 127.0) as u8;
            }
            if let Some((g0, k)) = c.gate {
                br.gate = ((g0 * k.powi(d)) * 100.0).round().clamp(1.0, 100.0) / 100.0;
            }
            let plant_sub = u64::from(c.forest.as_ref().map_or(0, |fo| fo.2)) * r.plant as u64;
            let sub = u64::from(br.sub)
                + u64::from(c.depth_sub) * u64::from(r.depth)
                + u64::from(c.letter_sub) * u64::from(r.class)
                + plant_sub;
            br.sub = sub.min(4095) as u32;
            b.paths.push((pts, st, br));
        }
        Ok(b)
    }

    fn built(&self, f: &Frame) -> Option<&Built> {
        self.built.get_or_init(|| self.build(f)).as_ref().ok()
    }
}

/// Piece `r` cut to the box (shares 0 – 1); a piece ends where it leaves.
fn cut(r: &Raw) -> Vec<Raw> {
    let w = [0.0, 0.0, 1.0, 1.0];
    let mut out = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    let flush = |cur: &mut Vec<(f64, f64)>, out: &mut Vec<Raw>| {
        if cur.len() >= 2 {
            out.push(Raw {
                pts: std::mem::take(cur),
                depth: r.depth,
                class: r.class,
                plant: r.plant,
            });
        } else {
            cur.clear();
        }
    };
    for q in r.pts.windows(2) {
        let (p, s) = (q[0], q[1]);
        match clip_seg(&w, p, s) {
            Some((a, b)) => {
                let at = |k: f64| (p.0 + (s.0 - p.0) * k, p.1 + (s.1 - p.1) * k);
                if a > 0.0 || cur.is_empty() {
                    flush(&mut cur, &mut out);
                    cur.push(at(a));
                }
                cur.push(at(b));
                if b < 1.0 {
                    flush(&mut cur, &mut out);
                }
            }
            None => flush(&mut cur, &mut out),
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Piece `r` scaled by `k` about the box's centre and brought back into it:
/// wrapped (a new piece in each cell) or reflected, cut exactly where it
/// crosses the cells' edges.
fn fold(r: &Raw, k: f64, reflect: bool) -> Vec<Raw> {
    let sc = |p: (f64, f64)| (0.5 + (p.0 - 0.5) * k, 0.5 + (p.1 - 0.5) * k);
    let back = |q: (f64, f64), cell: (i64, i64)| {
        let f = |v: f64, c: i64| {
            let r = v - c as f64;
            if reflect && c.rem_euclid(2) == 1 {
                1.0 - r
            } else {
                r
            }
        };
        (
            f(q.0, cell.0).clamp(0.0, 1.0),
            f(q.1, cell.1).clamp(0.0, 1.0),
        )
    };
    let mut out = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    let mut cell_now: Option<(i64, i64)> = None;
    let push = |cur: &mut Vec<(f64, f64)>, out: &mut Vec<Raw>| {
        if cur.len() >= 2 {
            out.push(Raw {
                pts: std::mem::take(cur),
                depth: r.depth,
                class: r.class,
                plant: r.plant,
            });
        } else {
            cur.clear();
        }
    };
    for q in r.pts.windows(2) {
        let (a, b) = (sc(q[0]), sc(q[1]));
        let mut ts = vec![0.0, 1.0];
        for (pa, pb) in [(a.0, b.0), (a.1, b.1)] {
            if pa == pb {
                continue;
            }
            let (lo, hi) = (pa.min(pb), pa.max(pb));
            let mut n = lo.floor() + 1.0;
            while n < hi {
                ts.push((n - pa) / (pb - pa));
                n += 1.0;
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.dedup();
        let at = |t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        for w in ts.windows(2) {
            let (t0, t1) = (w[0], w[1]);
            if t1 <= t0 {
                continue;
            }
            let m = at((t0 + t1) / 2.0);
            let cell = (m.0.floor() as i64, m.1.floor() as i64);
            let (p0, p1) = (back(at(t0), cell), back(at(t1), cell));
            if !(reflect || cell_now == Some(cell)) {
                push(&mut cur, &mut out);
            }
            if cur.is_empty() {
                cur.push(p0);
            }
            cur.push(p1);
            cell_now = Some(cell);
        }
    }
    push(&mut cur, &mut out);
    out
}

impl Shapes for LSystem {
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
        // Branches, copies and joins overlap (their notes merge): the most
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

fn lsystem_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let g = LSystem::new(Cfg::read(p)?);
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
    fn rules_parse_and_bad_ones_are_named_in_sentences() {
        let r = parse_rules(BASE_RULES, "the rules").unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].0, r[1].0), (b'X', b'F'));
        assert_eq!(r[1].1, b"FF");
        for (bad, has) in [
            ("XF", "no ="),
            ("XY=F", "one symbol"),
            ("X=F]", "without its ["),
            ("X=[F", "open"),
            ("X=F=G", "not allowed"),
            ("X=Fé", "not allowed"),
        ] {
            let e = parse_rules(bad, "the rules").unwrap_err();
            assert!(e.contains(has) && e.ends_with('.'), "{bad}: {e}");
        }
        assert!(symbols(&"F".repeat(RULE_LEN + 1), "the axiom").is_err());
    }

    fn cfg(kv: &[(&str, ParamValue)]) -> Cfg {
        let spec = lsystem_spec();
        let mut p = ParamSet::defaults(&spec.params);
        for (k, v) in kv {
            p.set(k, v.clone());
        }
        Cfg::read(&Params::new(&spec.params, &p)).unwrap()
    }

    /// The deterministic rules' bounds are the exact lengths of the strings.
    #[test]
    fn bounds_of_deterministic_rules_are_the_exact_string_lengths() {
        let c = cfg(&[]);
        let lv = c.bounds(5);
        // X → 4 X and 3 F; F → 2 F: after n rewrites of X: 4ⁿ X, F as below.
        let (mut x, mut f) = (1u128, 0u128);
        for _ in 0..5 {
            (x, f) = (4 * x, 2 * f + 3 * x);
        }
        assert_eq!(lv[5][usize::from(b'X')][1], f);
        // Every symbol of the successor counted: brackets and turns too.
        let mut t = Turtle {
            cfg: &c,
            seed: 1,
            plant: 0,
            st: St {
                x: 0.0,
                y: 0.0,
                h: 0.0,
                depth: 0,
                len: 1.0,
            },
            stack: Vec::new(),
            tips: vec![None],
            skip: 0,
            cur: None,
            out: Vec::new(),
        };
        let mut w = Walk {
            cfg: &c,
            seed: 1,
            depth: 5,
            turtle: &mut t,
        };
        w.walk(b'X', 0, 7, 0);
        let segs: usize = t.out.iter().map(|r| r.pts.len() - 1).sum();
        assert_eq!(segs as u128, f);
    }

    /// The lower bound never passes the count and the cheap bound never
    /// falls below it, over seeded random settings in small boxes.
    #[test]
    fn the_lower_bound_never_passes_the_count_and_the_bound_never_falls_below_it() {
        let spec = lsystem_spec();
        let mut rng = Rng::new(0x5345_3539);
        for draw in 0..200 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 200)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            p.set("gen.seed", ParamValue::Int(rng.range_i64(1, 1000)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            p.set(
                "c03_iteration_depth.depth",
                ParamValue::Int(rng.range_i64(0, 6)),
            );
            for (k, lo, hi) in [
                ("c05_turn_angle.angle", 0.0, 180.0),
                ("c04_turtle_step_length.step", 0.05, 8.0),
                ("c22_wrap_reflect.extent", 1.0, 4.0),
                ("c18_tropism_direction_bias.angle", -180.0, 180.0),
            ] {
                p.set(k, ParamValue::Float(rng.range_f64(lo, hi)));
            }
            for (k, n) in [
                ("c20_bounding_box_fit.mode", 3),
                ("c21_mirror_symmetry.axis", 4),
                ("c22_wrap_reflect.mode", 2),
                ("c23_branch_join.mode", 2),
                ("c25_rule_set_morph_evolution.mode", 2),
                ("c04_turtle_step_length.anchor", 3),
            ] {
                p.set(k, ParamValue::Choice(rng.below(n) as u32));
            }
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let Ok(cfg) = Cfg::read(&params) else {
                continue;
            };
            let g = LSystem::new(cfg);
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

    #[test]
    fn folding_cuts_exactly_at_the_cells_edges() {
        let r = Raw {
            pts: vec![(0.0, 0.5), (1.0, 0.5)],
            depth: 0,
            class: 0,
            plant: 0,
        };
        // Extent 2: from -0.5 to 1.5 — three cells, wrapped: three pieces.
        let w = fold(&r, 2.0, false);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].pts, vec![(0.5, 0.5), (1.0, 0.5)]);
        assert_eq!(w[1].pts, vec![(0.0, 0.5), (1.0, 0.5)]);
        // Reflected: one piece going back and forth.
        let f = fold(&r, 2.0, true);
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].pts,
            vec![(0.5, 0.5), (0.0, 0.5), (1.0, 0.5), (0.5, 0.5)]
        );
    }
}
