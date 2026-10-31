//! Parametric Equation (§157, §240, `gen.parametric`; `docs/GENERATORS.md`
//! "Parametric Equation"): the second Advanced generator — curves given by
//! formulas in the math-only sandbox (`editor::expr`, named variables),
//! drawn by the shared path pen (`generator/path.rs`, as Lines and Arcs).
//! The base: a sawtooth of `a` (3) curved teeth rising across the box,
//! X(t) = t, Y(t) = 1 − (1 − frac(a · t))² for t from 0 to 1, sampled at
//! 256 even steps of t and joined by strokes — its drops drawn as the
//! samples join them (c20 detects them).
//!
//! Formulas give points as **box fractions** (x from the box's start, y
//! from its bottom, 0 – 1) and may use the variables `t`, `x`, `y`, `s`,
//! `a` … `d` (c10's constants), `noise` and `rnd` (c24). The pipeline, in
//! this order:
//!
//! 1. **Curve** — c01 / c02 X(t), Y(t) (Y may use x = X(t)), or c07 polar
//!    R(t) (θ = 2π t about the box's centre, R = 1 the box's ellipse), or
//!    c08 cartesian y = F(x) (x = t); c09 up to two more layers (X, Y
//!    pairs); c23 a second pair B blended in by a share (several shapes);
//!    c24 seeded noise added to y.
//! 2. **Samples** — t from c03 to c04, c05 even steps, or c06 adaptive
//!    (intervals halved where the curve leaves its chord by more than a
//!    tolerance on the grid).
//! 3. **Maps** (box fractions) — c11 time scale, c12 pitch scale, c13
//!    offset, c22 wrap / reflect into the box, c21 domain clipping; then on
//!    the grid c14 rotation about the box's centre.
//! 4. **Breaks** — c20: where consecutive samples jump more than a share of
//!    the box or a formula has no value (without c20 such a value refuses
//!    the run with a sentence); wraps and the domain's edges break too.
//! 5. **Drawing** — strokes along each piece; c15 / c16 a note every grid
//!    unit kept by a share from the curve's speed (dense where t moves it
//!    slowly) or its curvature; c17 – c19 velocity, gate and subchannel by
//!    formulas at each sample.
//! 6. **c25** — a library of named equations kept in the tool's presets;
//!    every formula is validated (sentences name the formula, the reason and
//!    the character).

use super::super::param::*;
use super::super::registry::{BuildCtx, ToolDef, ToolSpec};
use super::super::rng::hash;
use super::super::{Kernel, TransformError};
use super::draw::{Aff, BoxG, Gb, Pt, clip_seg, inside, positions};
use super::path::{
    PIECE_CAP, PathStyle, Scratch, VISIT_CAP, curvature, curve_along, measure, too_many_pieces,
    too_many_visits,
};
use super::pen::Draw;
use super::{Frame, Shapes};
use crate::editor::expr::{Expr, ExprError};
use crate::midi::group_digits;
use std::f64::consts::TAU;
use std::sync::OnceLock;

pub const PARAMETRIC: ToolDef = ToolDef {
    id: "gen.parametric",
    spec: parametric_spec,
    build: parametric_build,
};

/// The contract's 25 functions (catalog id, name, order).
pub const FUNCTIONS: &[(&str, &str, u16)] = &[
    ("c01_x_t_formula", "X(t) Formula", 10),
    ("c02_y_t_formula", "Y(t) Formula", 20),
    ("c03_t_start", "t Start", 30),
    ("c04_t_end", "t End", 40),
    ("c05_sample_count", "Sample Count", 50),
    ("c06_adaptive_sampling", "Adaptive Sampling", 60),
    ("c07_polar_equation_mode", "Polar Equation Mode", 70),
    ("c08_cartesian_mode", "Cartesian Mode", 80),
    (
        "c09_multiple_equation_layers",
        "Multiple Equation Layers",
        90,
    ),
    (
        "c10_user_constants_variables",
        "User Constants/Variables",
        100,
    ),
    ("c11_time_scale", "Time Scale", 110),
    ("c12_pitch_scale", "Pitch Scale", 120),
    ("c13_x_y_offset", "X/Y Offset", 130),
    ("c14_rotation", "Rotation", 140),
    (
        "c15_derivative_based_density",
        "Derivative-Based Density",
        150,
    ),
    (
        "c16_curvature_based_density",
        "Curvature-Based Density",
        160,
    ),
    ("c17_velocity_formula", "Velocity Formula", 170),
    ("c18_gate_formula", "Gate Formula", 180),
    ("c19_subchannel_formula", "Subchannel Formula", 190),
    (
        "c20_discontinuity_detection",
        "Discontinuity Detection",
        200,
    ),
    ("c21_domain_clipping", "Domain Clipping", 210),
    (
        "c22_wrap_reflect_boundaries",
        "Wrap/Reflect Boundaries",
        220,
    ),
    ("c23_formula_a_b_morph", "Formula A→B Morph", 230),
    (
        "c24_deterministic_noise_functions",
        "Deterministic Noise Functions",
        240,
    ),
    (
        "c25_saved_equation_presets_expression_validation",
        "Saved Equation Presets / Expression Validation",
        250,
    ),
];

/// The formulas' variables, in this order.
pub const VARS: [&str; 10] = ["t", "x", "y", "s", "a", "b", "c", "d", "noise", "rnd"];
/// The base's formulas.
pub const BASE_X: &str = "t";
pub const BASE_Y: &str = "1 - pow(1 - (a*t - floor(a*t)), 2)";
/// The constants a – d without c10.
pub const BASE_CONSTS: [f64; 4] = [3.0, 1.0, 0.0, 0.0];
/// Samples of the base.
pub const BASE_SAMPLES: usize = 256;
/// The even steps adaptive sampling starts from.
pub const ADAPTIVE_START: usize = 16;
/// Most samples of one curve (even or adaptive).
pub const SAMPLE_CAP: usize = 16_384;
/// Most formula steps (samples × formula sizes) of one request.
pub const EVAL_CAP: u64 = 1 << 26;
/// The library c25 starts with.
pub const LIBRARY: &str = "Lissajous: 0.5 + 0.45*sin(6*pi*t) | 0.5 + 0.45*sin(8*pi*t); \
Rose: 0.5 + 0.45*cos(10*pi*t)*cos(2*pi*t) | 0.5 + 0.45*cos(10*pi*t)*sin(2*pi*t); \
Spiral: 0.5 + 0.45*t*cos(12*pi*t) | 0.5 + 0.45*t*sin(12*pi*t); \
Heart: 0.5 + 0.48*sin(2*pi*t)*sin(2*pi*t)*sin(2*pi*t) | 0.47 + 0.03*(13*cos(2*pi*t) - 5*cos(4*pi*t) - 2*cos(6*pi*t) - cos(8*pi*t)); \
Sine: t | 0.5 + 0.4*sin(2*pi*t)";

const ANCHORS: &[&str] = &["The box's start", "The box's center"];
const WRAPS: &[&str] = &["Wrap round", "Reflect"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PARAMETRIC_HELP: &[(&str, &str)] = &[
    (
        "c01_x_t_formula",
        "Sets the curve's position in time with a formula of t instead of the base sawtooth.",
    ),
    (
        "c02_y_t_formula",
        "Sets the curve's height in pitch with a formula of t instead of the base sawtooth.",
    ),
    (
        "c03_t_start",
        "Starts the curve at another value of t than 0.",
    ),
    ("c03_t_start.t", "The value of t where the curve starts."),
    ("c04_t_end", "Ends the curve at another value of t than 1."),
    ("c04_t_end.t", "The value of t where the curve ends."),
    (
        "c05_sample_count",
        "Sets how many points of the curve are computed and joined.",
    ),
    (
        "c06_adaptive_sampling",
        "Computes more points where the curve bends and fewer where it is straight.",
    ),
    (
        "c07_polar_equation_mode",
        "Draws the curve as a radius around the box's center that changes with the angle (a polar formula).",
    ),
    (
        "c08_cartesian_mode",
        "Draws the curve as a height formula of the position in time: y = F(x).",
    ),
    (
        "c09_multiple_equation_layers",
        "Draws up to two more curves from formulas of their own over the same t.",
    ),
    (
        "c09_multiple_equation_layers.count",
        "How many extra curves are drawn (1 or 2).",
    ),
    (
        "c09_multiple_equation_layers.x2",
        "The second curve's position in time as a formula of t (a share of the box's width).",
    ),
    (
        "c09_multiple_equation_layers.y2",
        "The second curve's height as a formula of t (a share of the box's height).",
    ),
    (
        "c09_multiple_equation_layers.x3",
        "The third curve's position in time as a formula of t (a share of the box's width).",
    ),
    (
        "c09_multiple_equation_layers.y3",
        "The third curve's height as a formula of t (a share of the box's height).",
    ),
    (
        "c10_user_constants_variables",
        "Sets your own values of a, b, c and d for the formulas to use.",
    ),
    (
        "c10_user_constants_variables.a",
        "The value of a in every formula.",
    ),
    (
        "c10_user_constants_variables.b",
        "The value of b in every formula.",
    ),
    (
        "c10_user_constants_variables.c",
        "The value of c in every formula.",
    ),
    ("c11_time_scale", "Squeezes or stretches the curve in time."),
    (
        "c11_time_scale.k",
        "How wide the curve is, as a share of its own width.",
    ),
    (
        "c11_time_scale.anchor",
        "The point the curve is scaled about: the box's start or its center.",
    ),
    (
        "c12_pitch_scale",
        "Squeezes or stretches the curve in pitch.",
    ),
    ("c13_x_y_offset", "Moves the whole curve in time and pitch."),
    (
        "c13_x_y_offset.x",
        "How far the curve moves in time, as a share of the box's width.",
    ),
    (
        "c13_x_y_offset.y",
        "How far the curve moves in pitch, as a share of the box's height.",
    ),
    ("c14_rotation", "Turns the curve about the box's center."),
    (
        "c15_derivative_based_density",
        "Places separate notes along the curve, closer together where the curve moves slowly.",
    ),
    (
        "c16_curvature_based_density",
        "Places separate notes along the curve, closer together where it bends.",
    ),
    (
        "c16_curvature_based_density.strength",
        "How strongly the bends decide where the notes go (0 % = evenly).",
    ),
    (
        "c17_velocity_formula",
        "Sets the velocity at each point with a formula.",
    ),
    (
        "c18_gate_formula",
        "Sets the note length at each point with a formula.",
    ),
    (
        "c18_gate_formula.g",
        "The gate as a percentage, as a formula of t, x, y, s (0 – 1 along the piece), a – d, noise and rnd.",
    ),
    (
        "c19_subchannel_formula",
        "Sets the subchannel (color) at each point with a formula.",
    ),
    (
        "c20_discontinuity_detection",
        "Leaves gaps where the curve jumps or a formula has no value, instead of joining them with a line.",
    ),
    (
        "c21_domain_clipping",
        "Draws only the part of the curve inside a window of the box.",
    ),
    (
        "c21_domain_clipping.x0",
        "The window's start in time, as a share of the box's width.",
    ),
    (
        "c21_domain_clipping.x1",
        "The window's end in time, as a share of the box's width.",
    ),
    (
        "c21_domain_clipping.y0",
        "The window's bottom, as a share of the box's height.",
    ),
    (
        "c22_wrap_reflect_boundaries",
        "Brings parts of the curve that leave the box back inside it.",
    ),
    (
        "c22_wrap_reflect_boundaries.mode",
        "Wrap Round brings them back in at the other side; Reflect bounces them off the edges.",
    ),
    (
        "c23_formula_a_b_morph",
        "Blends the curve toward a second curve given by its own formulas.",
    ),
    (
        "c23_formula_a_b_morph.x",
        "The second curve's position in time as a formula of t.",
    ),
    (
        "c23_formula_a_b_morph.y",
        "The second curve's height as a formula of t.",
    ),
    (
        "c23_formula_a_b_morph.amount",
        "How far the curve moves toward the second one.",
    ),
    (
        "c24_deterministic_noise_functions",
        "Adds smooth seeded noise to the curve and lets the formulas use noise and rnd.",
    ),
    (
        "c24_deterministic_noise_functions.freq",
        "How fast the noise changes, in wiggles per unit of t.",
    ),
    (
        "c25_saved_equation_presets_expression_validation",
        "Loads a named equation from a library you can edit and save with the tool's presets.",
    ),
];

fn parametric_spec() -> ToolSpec {
    let mut own = vec![
        super::seed_spec(),
        flag("c01_x_t_formula", "X(t) Formula", false),
        text("c01_x_t_formula.x", "X(t)", 1000, "t + 0.08*sin(6*pi*t)").help(
            "The point's x as a share of the box's width (0 its start, 1 its end), in t and the variables a – d, noise, rnd.",
        ),
        flag("c02_y_t_formula", "Y(t) Formula", false),
        text("c02_y_t_formula.y", "Y(t)", 1000, "0.5 + 0.4*sin(2*pi*t)*cos(6*pi*t)").help(
            "The point's y as a share of the box's height (0 its lowest row, 1 its highest); x is X(t).",
        ),
        flag("c03_t_start", "t Start", false),
        float("c03_t_start.t", "t from", -1000.0, 1000.0, 0.25, ""),
        flag("c04_t_end", "t End", false),
        float("c04_t_end.t", "t to", -1000.0, 1000.0, 0.75, ""),
        flag("c05_sample_count", "Sample Count", false),
        int("c05_sample_count.n", "Samples", 2, SAMPLE_CAP as i64, 24)
            .help("Even steps of t from its start to its end, joined by strokes."),
        flag("c06_adaptive_sampling", "Adaptive Sampling", false),
        float("c06_adaptive_sampling.tolerance", "Tolerance (grid units)", 0.01, 8.0, 1.0, "")
            .help("From 16 even steps of t (not the sample count), each step halved (12 times at most) while the curve's middle lies farther than this from the chord on the grid."),
        flag("c07_polar_equation_mode", "Polar Equation Mode", false),
        text("c07_polar_equation_mode.r", "R(t)", 1000, "0.6 + 0.35*cos(5*2*pi*t)").help(
            "The radius at angle θ = 2π t about the box's center: 1 reaches the box's ellipse.",
        ),
        flag("c08_cartesian_mode", "Cartesian Mode", false),
        text("c08_cartesian_mode.f", "y = F(x)", 1000, "0.5 + 0.45*sin(4*pi*x)*exp(-2*x)")
            .help("x runs over t's range (box shares); polar mode wins over it."),
        flag("c09_multiple_equation_layers", "Multiple Equation Layers", false),
        int("c09_multiple_equation_layers.count", "More layers", 1, 2, 1),
        text("c09_multiple_equation_layers.x2", "Layer 2 X(t)", 1000, "t"),
        text("c09_multiple_equation_layers.y2", "Layer 2 Y(t)", 1000, "pow(1 - (a*t - floor(a*t)), 2)"),
        text("c09_multiple_equation_layers.x3", "Layer 3 X(t)", 1000, "t"),
        text("c09_multiple_equation_layers.y3", "Layer 3 Y(t)", 1000, "0.5"),
        int("c09_multiple_equation_layers.sub_step", "Subchannel step per layer", 0, 16, 1)
            .help("Each layer its own curve over the same t, drawn with the subchannel position moved this much per layer."),
        flag("c10_user_constants_variables", "User Constants/Variables", false),
        float("c10_user_constants_variables.a", "a", -1000.0, 1000.0, 5.0, ""),
        float("c10_user_constants_variables.b", "b", -1000.0, 1000.0, 2.0, ""),
        float("c10_user_constants_variables.c", "c", -1000.0, 1000.0, 0.5, ""),
        float("c10_user_constants_variables.d", "d", -1000.0, 1000.0, 0.25, "")
            .help("Named values every formula may use (without this function a = 3, b = 1, c = d = 0)."),
        flag("c11_time_scale", "Time Scale", false),
        float("c11_time_scale.k", "Time scale", 1.0, 1000.0, 50.0, "%"),
        choice("c11_time_scale.anchor", "About", ANCHORS, 0),
        flag("c12_pitch_scale", "Pitch Scale", false),
        float("c12_pitch_scale.k", "Pitch scale", 1.0, 1000.0, 50.0, "%")
            .help("About the box's middle row."),
        flag("c13_x_y_offset", "X/Y Offset", false),
        float("c13_x_y_offset.x", "Move x (% of the box)", -100.0, 100.0, 10.0, "%"),
        float("c13_x_y_offset.y", "Move y (% of the box)", -100.0, 100.0, 10.0, "%"),
        flag("c14_rotation", "Rotation", false),
        float("c14_rotation.angle", "Angle", -180.0, 180.0, 30.0, "°")
            .help("About the box's center on the grid (a step across is a row up)."),
        flag("c15_derivative_based_density", "Derivative-Based Density", false),
        float("c15_derivative_based_density.strength", "Strength", 0.0, 200.0, 100.0, "%")
            .help("A note every grid unit along the curve, kept by (the slowest speed / the speed there) ^ strength: dense where t moves the curve slowly, sparse where it races."),
        flag("c16_curvature_based_density", "Curvature-Based Density", false),
        float("c16_curvature_based_density.strength", "Strength", 0.0, 200.0, 100.0, "%"),
        float("c16_curvature_based_density.floor", "Least share", 0.0, 100.0, 10.0, "%")
            .help("A note every grid unit, kept by least + (1 − least) · (curvature / the most) ^ strength: dense in the bends."),
        flag("c17_velocity_formula", "Velocity Formula", false),
        text("c17_velocity_formula.v", "Velocity", 1000, "40 + 87*y")
            .help("1 – 127 at each sample, in t, x, y (the drawn point's box shares), s (0 – 1 along the piece), a – d, noise, rnd."),
        flag("c18_gate_formula", "Gate Formula", false),
        text("c18_gate_formula.g", "Gate (%)", 1000, "100 - 80*s"),
        flag("c19_subchannel_formula", "Subchannel Formula", false),
        text("c19_subchannel_formula.sub", "Subchannel position", 1000, "floor(4*s)")
            .help("0 – 4095, rounded, at each sample."),
        flag("c20_discontinuity_detection", "Discontinuity Detection", false),
        float("c20_discontinuity_detection.jump", "A jump of more than (% of the box)", 1.0, 100.0, 25.0, "%")
            .help("Consecutive samples farther apart than this (box shares) are not joined; samples where a formula has no value are left out (without this function such a value refuses the run)."),
        flag("c21_domain_clipping", "Domain Clipping", false),
        float("c21_domain_clipping.x0", "x from", 0.0, 100.0, 20.0, "%"),
        float("c21_domain_clipping.x1", "x to", 0.0, 100.0, 80.0, "%"),
        float("c21_domain_clipping.y0", "y from", 0.0, 100.0, 20.0, "%"),
        float("c21_domain_clipping.y1", "y to", 0.0, 100.0, 80.0, "%")
            .help("Only the curve inside this window of the box (cut exactly at its edges)."),
        flag("c22_wrap_reflect_boundaries", "Wrap/Reflect Boundaries", false),
        choice("c22_wrap_reflect_boundaries.mode", "Outside the box", WRAPS, 0),
        float("c22_wrap_reflect_boundaries.extent", "Extent", 1.0, 16.0, 2.0, "×")
            .help("The curve scaled this much about the box's center, then brought back: wrapped round (broken at each wrap) or reflected at the edges."),
        flag("c23_formula_a_b_morph", "Formula A→B Morph", false),
        text("c23_formula_a_b_morph.x", "B: X(t)", 1000, "t"),
        text("c23_formula_a_b_morph.y", "B: Y(t)", 1000, "0.5 + 0.4*sin(2*pi*t)"),
        float("c23_formula_a_b_morph.amount", "Morph", 0.0, 100.0, 50.0, "%"),
        int("c23_formula_a_b_morph.steps", "Curves drawn", 1, 16, 1)
            .help("Each sample moved this share of the way from curve A (the formulas above) to curve B at the same t. More curves: evenly from A (0 %) to the share."),
        flag("c24_deterministic_noise_functions", "Deterministic Noise Functions", false),
        float("c24_deterministic_noise_functions.freq", "Noise frequency (per unit of t)", 0.5, 256.0, 12.0, ""),
        float("c24_deterministic_noise_functions.amount", "Noise added to y (% of the box)", 0.0, 50.0, 6.0, "%")
            .help("noise: smooth seeded noise of t (−1 – 1); rnd: a seeded random number per sample (0 – 1) — both usable in every formula (0 without this function); the noise also moves y."),
        flag(
            "c25_saved_equation_presets_expression_validation",
            "Saved Equation Presets / Expression Validation",
            false,
        ),
        text(
            "c25_saved_equation_presets_expression_validation.library",
            "Equation library",
            4096,
            LIBRARY,
        )
        .help("Named equations: `Name: X(t) | Y(t)`, `;` between them; saved and loaded with the tool's presets. Every formula is checked before anything is drawn."),
        text(
            "c25_saved_equation_presets_expression_validation.name",
            "Equation to load",
            64,
            "Lissajous",
        )
        .help("The library's equation used for X(t) and Y(t) (over X / Y Formula's)."),
    ];
    with_help(&mut own, PARAMETRIC_HELP);
    super::spec(
        "gen.parametric",
        "Parametric Equation",
        "Generates notes along a curve given by formulas: a sawtooth across the generation box to start from (where, length and pitch range above), sampled in t and joined on the resolution grid.",
        own,
        FUNCTIONS,
    )
}

/// A parsed formula and its name (for sentences).
#[derive(Clone, Debug)]
struct F {
    e: Expr,
    name: &'static str,
}

fn parse(name: &'static str, src: &str) -> Result<F, TransformError> {
    Expr::parse_vars(src, &VARS)
        .map(|e| F { e, name })
        .map_err(|e| TransformError::Invalid(say(name, &e, None)))
}

/// A sentence for formula error `e` (at sample `t` when evaluating).
fn say(name: &str, e: &ExprError, t: Option<f64>) -> String {
    let msg = e.message.trim_end_matches('.');
    match (t, e.at) {
        (Some(t), _) => format!("{name}: {msg} (t = {t})."),
        (None, Some(at)) => format!("{name}: {msg} (at character {}).", at + 1),
        (None, None) => format!("{name}: {msg}."),
    }
}

/// The main curve.
#[derive(Clone, Debug)]
enum Main {
    Param(F, F),
    Polar(F),
    Cartesian(F),
}

/// The functions' settings.
#[derive(Clone, Debug)]
struct Cfg {
    main: Main,
    layers: Vec<(F, F)>,
    layer_sub: u32,
    t0: f64,
    t1: f64,
    samples: usize,
    adaptive: Option<f64>,
    consts: [f64; 4],
    /// c11: scale, anchor (0 start, 0.5 centre).
    tscale: Option<(f64, f64)>,
    pscale: Option<f64>,
    offset: (f64, f64),
    rotate: f64,
    speed: Option<f64>,
    curv: Option<(f64, f64)>,
    vel: Option<F>,
    gate: Option<F>,
    sub: Option<F>,
    jump: Option<f64>,
    clip: Option<[f64; 4]>,
    /// c22: reflect?, extent.
    wrap: Option<(bool, f64)>,
    /// c23: B, amount, steps.
    morph: Option<(F, F, f64, u32)>,
    /// c24: frequency, amount.
    noise: Option<(f64, f64)>,
}

/// The library's equations: names and their X(t), Y(t) sources.
pub type Library = Vec<(String, String, String)>;

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
                "Equation {} of the library has no name — write it as Name: X(t) | Y(t).",
                k + 1
            ));
        };
        let Some((x, y)) = body.split_once('|') else {
            return Err(format!(
                "The library's equation “{}” needs X(t) and Y(t) separated by |.",
                name.trim()
            ));
        };
        let name = name.trim().to_string();
        for (part, src) in [("X(t)", x), ("Y(t)", y)] {
            if let Err(e) = Expr::parse_vars(src, &VARS) {
                return Err(say(&format!("The library's “{name}” {part}"), &e, None));
            }
        }
        out.push((name, x.trim().to_string(), y.trim().to_string()));
    }
    Ok(out)
}

impl Cfg {
    fn read(p: &Params) -> Result<Cfg, TransformError> {
        let on = |id: &str| p.flag(id);
        let pct = |k: &str| p.float(k) / 100.0;
        let mut x = parse("X(t)", BASE_X)?;
        let mut y = parse("Y(t)", BASE_Y)?;
        if on("c01_x_t_formula") {
            x = parse("X(t)", &p.text("c01_x_t_formula.x"))?;
        }
        if on("c02_y_t_formula") {
            y = parse("Y(t)", &p.text("c02_y_t_formula.y"))?;
        }
        let k25 = "c25_saved_equation_presets_expression_validation";
        if on(k25) {
            let lib = parse_library(&p.text(&format!("{k25}.library")))
                .map_err(TransformError::Invalid)?;
            let want = p.text(&format!("{k25}.name"));
            let want = want.trim();
            let Some((_, sx, sy)) = lib.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(want))
            else {
                let names: Vec<&str> = lib.iter().map(|(n, _, _)| n.as_str()).collect();
                return Err(TransformError::Invalid(if names.is_empty() {
                    "The equation library is empty — add one as Name: X(t) | Y(t).".into()
                } else {
                    format!(
                        "The equation library has no equation named “{want}” — it has {}.",
                        names.join(", ")
                    )
                }));
            };
            x = parse("X(t)", sx)?;
            y = parse("Y(t)", sy)?;
        }
        let main = if on("c07_polar_equation_mode") {
            Main::Polar(parse("R(t)", &p.text("c07_polar_equation_mode.r"))?)
        } else if on("c08_cartesian_mode") {
            Main::Cartesian(parse("F(x)", &p.text("c08_cartesian_mode.f"))?)
        } else {
            Main::Param(x, y)
        };
        let mut layers = Vec::new();
        let mut layer_sub = 0;
        if on("c09_multiple_equation_layers") {
            let k = "c09_multiple_equation_layers";
            let n = p.int(&format!("{k}.count")).clamp(1, 2);
            layers.push((
                parse("Layer 2 X(t)", &p.text(&format!("{k}.x2")))?,
                parse("Layer 2 Y(t)", &p.text(&format!("{k}.y2")))?,
            ));
            if n >= 2 {
                layers.push((
                    parse("Layer 3 X(t)", &p.text(&format!("{k}.x3")))?,
                    parse("Layer 3 Y(t)", &p.text(&format!("{k}.y3")))?,
                ));
            }
            layer_sub = p.int(&format!("{k}.sub_step")).clamp(0, 16) as u32;
        }
        let mut c = Cfg {
            main,
            layers,
            layer_sub,
            t0: 0.0,
            t1: 1.0,
            samples: BASE_SAMPLES,
            adaptive: None,
            consts: BASE_CONSTS,
            tscale: None,
            pscale: None,
            offset: (0.0, 0.0),
            rotate: 0.0,
            speed: None,
            curv: None,
            vel: None,
            gate: None,
            sub: None,
            jump: None,
            clip: None,
            wrap: None,
            morph: None,
            noise: None,
        };
        if on("c03_t_start") {
            c.t0 = p.float("c03_t_start.t").clamp(-1000.0, 1000.0);
        }
        if on("c04_t_end") {
            c.t1 = p.float("c04_t_end.t").clamp(-1000.0, 1000.0);
        }
        if on("c05_sample_count") {
            c.samples = p.int("c05_sample_count.n").clamp(2, SAMPLE_CAP as i64) as usize;
        }
        if on("c06_adaptive_sampling") {
            c.adaptive = Some(p.float("c06_adaptive_sampling.tolerance").clamp(0.01, 8.0));
        }
        if on("c10_user_constants_variables") {
            let k = "c10_user_constants_variables";
            for (i, n) in ["a", "b", "c", "d"].iter().enumerate() {
                c.consts[i] = p.float(&format!("{k}.{n}")).clamp(-1000.0, 1000.0);
            }
        }
        if on("c11_time_scale") {
            c.tscale = Some((
                pct("c11_time_scale.k").clamp(0.01, 10.0),
                if p.choice("c11_time_scale.anchor") == 1 {
                    0.5
                } else {
                    0.0
                },
            ));
        }
        if on("c12_pitch_scale") {
            c.pscale = Some(pct("c12_pitch_scale.k").clamp(0.01, 10.0));
        }
        if on("c13_x_y_offset") {
            c.offset = (
                pct("c13_x_y_offset.x").clamp(-1.0, 1.0),
                pct("c13_x_y_offset.y").clamp(-1.0, 1.0),
            );
        }
        if on("c14_rotation") {
            c.rotate = p
                .float("c14_rotation.angle")
                .clamp(-180.0, 180.0)
                .to_radians();
        }
        if on("c15_derivative_based_density") {
            c.speed = Some(pct("c15_derivative_based_density.strength").clamp(0.0, 2.0));
        }
        if on("c16_curvature_based_density") {
            c.curv = Some((
                pct("c16_curvature_based_density.strength").clamp(0.0, 2.0),
                pct("c16_curvature_based_density.floor").clamp(0.0, 1.0),
            ));
        }
        if on("c17_velocity_formula") {
            c.vel = Some(parse("Velocity", &p.text("c17_velocity_formula.v"))?);
        }
        if on("c18_gate_formula") {
            c.gate = Some(parse("Gate", &p.text("c18_gate_formula.g"))?);
        }
        if on("c19_subchannel_formula") {
            c.sub = Some(parse("Subchannel", &p.text("c19_subchannel_formula.sub"))?);
        }
        if on("c20_discontinuity_detection") {
            c.jump = Some(pct("c20_discontinuity_detection.jump").clamp(0.01, 1.0));
        }
        if on("c21_domain_clipping") {
            let k = "c21_domain_clipping";
            let g = |s: &str| pct(&format!("{k}.{s}")).clamp(0.0, 1.0);
            let (x0, x1, y0, y1) = (g("x0"), g("x1"), g("y0"), g("y1"));
            c.clip = Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]);
        }
        if on("c22_wrap_reflect_boundaries") {
            c.wrap = Some((
                p.choice("c22_wrap_reflect_boundaries.mode") == 1,
                p.float("c22_wrap_reflect_boundaries.extent")
                    .clamp(1.0, 16.0),
            ));
        }
        if on("c23_formula_a_b_morph") {
            let k = "c23_formula_a_b_morph";
            c.morph = Some((
                parse("B: X(t)", &p.text(&format!("{k}.x")))?,
                parse("B: Y(t)", &p.text(&format!("{k}.y")))?,
                pct(&format!("{k}.amount")).clamp(0.0, 1.0),
                p.int(&format!("{k}.steps")).clamp(1, 16) as u32,
            ));
        }
        if on("c24_deterministic_noise_functions") {
            let k = "c24_deterministic_noise_functions";
            c.noise = Some((
                p.float(&format!("{k}.freq")).clamp(0.5, 256.0),
                pct(&format!("{k}.amount")).clamp(0.0, 0.5),
            ));
        }
        Ok(c)
    }

    /// The curves drawn: (layer, morph share).
    fn curves(&self) -> Vec<(usize, Option<f64>)> {
        let shares: Vec<Option<f64>> = match &self.morph {
            None => vec![None],
            Some((_, _, amt, steps)) if *steps <= 1 => vec![Some(*amt)],
            Some((_, _, amt, steps)) => (0..*steps)
                .map(|k| Some(amt * f64::from(k) / f64::from(steps - 1)))
                .collect(),
        };
        let mut out = Vec::new();
        for l in 0..=self.layers.len() {
            for &s in &shares {
                out.push((l, s));
            }
        }
        out
    }

    /// The formula steps one sample of layer `l` costs.
    fn cost(&self, l: usize) -> u64 {
        let c = |f: &F| f.e.cost() as u64;
        let mut n = match (l, &self.main) {
            (0, Main::Param(x, y)) => c(x) + c(y),
            (0, Main::Polar(r)) => c(r),
            (0, Main::Cartesian(f)) => c(f),
            (k, _) => self.layers.get(k - 1).map_or(0, |(x, y)| c(x) + c(y)),
        };
        if let Some((x, y, _, _)) = &self.morph {
            n += c(x) + c(y);
        }
        for f in [&self.vel, &self.gate, &self.sub].into_iter().flatten() {
            n += c(f);
        }
        n + 1
    }
}

/// Smooth seeded value noise at `u` (−1 – 1).
fn noise(seed: u64, u: f64) -> f64 {
    if !u.is_finite() {
        return 0.0;
    }
    let k = u.floor();
    let f = u - k;
    let r = |i: f64| {
        let h = hash(&[seed, 0x4E4F_4953, i as i64 as u64]);
        (h >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let w = f * f * (3.0 - 2.0 * f);
    r(k) + (r(k + 1.0) - r(k)) * w
}

/// One sample: t, its point (box shares) or why it has none, and whether it
/// breaks from the previous one.
#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f64,
    p: Option<(f64, f64)>,
    brk: bool,
}

/// One piece of a curve: its points on the grid with t and their box
/// shares.
#[derive(Clone, Debug, Default)]
struct Piece {
    pts: Vec<Pt>,
    ts: Vec<f64>,
    fr: Vec<(f64, f64)>,
}

/// What is drawn: the pieces with their styles and layer.
#[derive(Debug, Default)]
struct Built {
    paths: Vec<(Vec<Pt>, PathStyle, usize)>,
}

struct Parametric {
    cfg: Cfg,
    built: OnceLock<Result<Built, TransformError>>,
}

impl Parametric {
    fn new(cfg: Cfg) -> Parametric {
        Parametric {
            cfg,
            built: OnceLock::new(),
        }
    }

    /// The variables' values at `t` for sample `i` of layer `l`.
    fn vals(&self, seed: u64, l: usize, i: usize, t: f64) -> [f64; 10] {
        let c = &self.cfg;
        let (nz, rnd) = match c.noise {
            Some((freq, _)) => (
                noise(seed ^ l as u64, t * freq),
                (hash(&[seed, 0x0052_4E44, l as u64, i as u64]) >> 11) as f64 / (1u64 << 53) as f64,
            ),
            None => (0.0, 0.0),
        };
        [
            t,
            0.0,
            0.0,
            0.0,
            c.consts[0],
            c.consts[1],
            c.consts[2],
            c.consts[3],
            nz,
            rnd,
        ]
    }

    /// The point of layer `l` (morph share `m`) at `t` as box shares, or
    /// the sentence why it has none.
    fn point(
        &self,
        seed: u64,
        l: usize,
        m: Option<f64>,
        i: usize,
        t: f64,
    ) -> Result<(f64, f64), String> {
        let c = &self.cfg;
        let mut v = self.vals(seed, l, i, t);
        let ev = |f: &F, v: &[f64; 10]| f.e.eval_vars(v).map_err(|e| say(f.name, &e, Some(t)));
        let pair = |x: &F, y: &F, v: &mut [f64; 10]| -> Result<(f64, f64), String> {
            let px = ev(x, v)?;
            v[1] = px;
            let py = ev(y, v)?;
            Ok((px, py))
        };
        let mut p = match (l, &c.main) {
            (0, Main::Param(x, y)) => pair(x, y, &mut v)?,
            (0, Main::Polar(r)) => {
                let rr = ev(r, &v)?;
                let th = TAU * t;
                (0.5 + 0.5 * rr * th.cos(), 0.5 + 0.5 * rr * th.sin())
            }
            (0, Main::Cartesian(f)) => {
                v[1] = t;
                (t, ev(f, &v)?)
            }
            (k, _) => {
                let (x, y) = &c.layers[k - 1];
                pair(x, y, &mut v)?
            }
        };
        if let (Some(share), Some((bx, by, _, _))) = (m, &c.morph) {
            let mut w = self.vals(seed, l, i, t);
            let q = pair(bx, by, &mut w)?;
            p = (p.0 + (q.0 - p.0) * share, p.1 + (q.1 - p.1) * share);
        }
        if let Some((_, amt)) = c.noise {
            p.1 += amt * v[8];
        }
        // Maps in box shares.
        if let Some((k, a)) = c.tscale {
            p.0 = a + (p.0 - a) * k;
        }
        if let Some(k) = c.pscale {
            p.1 = 0.5 + (p.1 - 0.5) * k;
        }
        p = (p.0 + c.offset.0, p.1 + c.offset.1);
        if p.0.is_finite() && p.1.is_finite() {
            Ok(p)
        } else {
            Err(format!("The curve has no finite point at t = {t}."))
        }
    }

    /// The samples of curve `(l, m)` on grid box `gb` (unit-sized steps of
    /// t, or adaptive), or a sentence.
    fn samples(&self, seed: u64, l: usize, m: Option<f64>, gb: &Gb) -> Result<Vec<Sample>, String> {
        let c = &self.cfg;
        let n = c.samples.max(2);
        let at = |i: usize, t: f64| -> Result<Sample, String> {
            match self.point(seed, l, m, i, t) {
                Ok(p) => Ok(Sample {
                    t,
                    p: Some(p),
                    brk: false,
                }),
                Err(_) if c.jump.is_some() => Ok(Sample {
                    t,
                    p: None,
                    brk: true,
                }),
                Err(e) => Err(e),
            }
        };
        // Adaptive sampling starts from 16 even steps.
        let n = if c.adaptive.is_some() {
            ADAPTIVE_START + 1
        } else {
            n
        };
        let ts: Vec<f64> = (0..n)
            .map(|k| c.t0 + (c.t1 - c.t0) * k as f64 / (n - 1) as f64)
            .collect();
        let mut out = Vec::with_capacity(n);
        for (k, &t) in ts.iter().enumerate() {
            out.push(at(k, t)?);
        }
        let Some(tol) = c.adaptive else {
            return Ok(out);
        };
        // Halve each step while its middle leaves the chord (on the grid).
        let mut idx = n;
        let mut res = Vec::with_capacity(out.len() * 2);
        let mut stack: Vec<(Sample, Sample, u32)> = Vec::new();
        res.push(out[0]);
        for w in out.windows(2) {
            stack.push((w[0], w[1], 0));
            while let Some((a, b, depth)) = stack.pop() {
                let split = match (a.p, b.p) {
                    (Some(pa), Some(pb)) if depth < 12 && idx < SAMPLE_CAP => {
                        let mid = at(idx, (a.t + b.t) / 2.0)?;
                        idx += 1;
                        match mid.p {
                            Some(pm) => {
                                let (ga, gm, gbb) = (gb.at(pa), gb.at(pm), gb.at(pb));
                                let ch = ((ga.0 + gbb.0) / 2.0, (ga.1 + gbb.1) / 2.0);
                                ((gm.0 - ch.0).hypot(gm.1 - ch.1) > tol).then_some(mid)
                            }
                            None => Some(mid),
                        }
                    }
                    _ => None,
                };
                match split {
                    Some(mid) => {
                        // Right half first on the stack: the left is done
                        // first, so points come out in t order.
                        stack.push((mid, b, depth + 1));
                        stack.push((a, mid, depth + 1));
                    }
                    None => res.push(b),
                }
            }
        }
        Ok(res)
    }

    /// The pieces of the samples after wrapping, clipping and breaks.
    fn pieces(&self, s: &[Sample], gb: &Gb) -> Vec<Piece> {
        let c = &self.cfg;
        let mut out: Vec<Piece> = Vec::new();
        let mut cur = Piece::default();
        let push = |cur: &mut Piece, out: &mut Vec<Piece>| {
            if cur.pts.len() >= 2 {
                out.push(std::mem::take(cur));
            } else {
                *cur = Piece::default();
            }
        };
        let add = |cur: &mut Piece, t: f64, q: (f64, f64)| {
            let (u, v) = gb.rot(gb.at(q));
            cur.pts.push(Pt { x: u, y: v, s: 0.0 });
            cur.ts.push(t);
            cur.fr.push(q);
        };
        // Wrap / reflect: the cell of the scaled point, its folded value.
        let fold = |p: (f64, f64)| -> ((f64, f64), (i64, i64)) {
            match c.wrap {
                None => (p, (0, 0)),
                Some((reflect, k)) => {
                    let sc = |x: f64| 0.5 + (x - 0.5) * k;
                    let (x, y) = (sc(p.0), sc(p.1));
                    let cell = (x.floor() as i64, y.floor() as i64);
                    let f = |v: f64| {
                        if reflect {
                            let r = v.rem_euclid(2.0);
                            if r > 1.0 { 2.0 - r } else { r }
                        } else {
                            v.rem_euclid(1.0)
                        }
                    };
                    ((f(x), f(y)), if reflect { (0, 0) } else { cell })
                }
            }
        };
        // (t, the folded point, its cell) of the previous sample.
        type Prev = (f64, (f64, f64), (i64, i64));
        let mut prev: Option<Prev> = None;
        for smp in s {
            let Some(p0) = smp.p else {
                push(&mut cur, &mut out);
                prev = None;
                continue;
            };
            let (p, cell) = fold(p0);
            let joined = match prev {
                Some((_, q, qc)) if !smp.brk => {
                    qc == cell && c.jump.is_none_or(|j| (p.0 - q.0).hypot(p.1 - q.1) <= j)
                }
                _ => false,
            };
            if !joined {
                push(&mut cur, &mut out);
            }
            match c.clip {
                None => add(&mut cur, smp.t, p),
                Some(w) => {
                    // The segment from the previous point, clipped to the
                    // window; a piece ends where it leaves.
                    let from = if joined {
                        prev.map(|q| (q.0, q.1))
                    } else {
                        None
                    };
                    match from {
                        None => {
                            if inside(&w, p) {
                                add(&mut cur, smp.t, p);
                            }
                        }
                        Some((tq, q)) => match clip_seg(&w, q, p) {
                            Some((a, b)) => {
                                let lerp = |k: f64| {
                                    (
                                        (q.0 + (p.0 - q.0) * k, q.1 + (p.1 - q.1) * k),
                                        tq + (smp.t - tq) * k,
                                    )
                                };
                                if a > 0.0 || cur.pts.is_empty() {
                                    push(&mut cur, &mut out);
                                    let (pa, ta) = lerp(a);
                                    add(&mut cur, ta, pa);
                                }
                                let (pb, tb) = lerp(b);
                                add(&mut cur, tb, pb);
                                if b < 1.0 {
                                    push(&mut cur, &mut out);
                                }
                            }
                            None => push(&mut cur, &mut out),
                        },
                    }
                }
            }
            prev = Some((smp.t, p, cell));
        }
        push(&mut cur, &mut out);
        // Positions along each piece by length.
        for pc in &mut out {
            positions(&mut pc.pts);
        }
        out
    }

    /// The style of piece `pc` of layer `l`.
    fn style(&self, seed: u64, l: usize, pc: &Piece) -> Result<PathStyle, String> {
        let c = &self.cfg;
        let mut st = PathStyle::default();
        let n = pc.pts.len();
        // Densities: a note every grid unit, kept by a share along s.
        let mut share: Option<Vec<f64>> = None;
        if let Some(k) = c.speed {
            let sp: Vec<f64> = (0..n)
                .map(|i| {
                    let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
                    let (p, q) = (pc.pts[a], pc.pts[b]);
                    let dt = (pc.ts[b] - pc.ts[a]).abs();
                    if dt > 0.0 {
                        (q.x - p.x).hypot(q.y - p.y) / dt
                    } else {
                        0.0
                    }
                })
                .collect();
            let slow = sp
                .iter()
                .copied()
                .filter(|&v| v > 0.0 && v.is_finite())
                .fold(f64::INFINITY, f64::min);
            share = Some(
                sp.iter()
                    .map(|&v| {
                        if v > 0.0 && slow.is_finite() {
                            (slow / v).powf(k).clamp(0.0, 1.0)
                        } else {
                            1.0
                        }
                    })
                    .collect(),
            );
        }
        if let Some((k, floor)) = c.curv {
            let kv = curvature(&pc.pts);
            let most = kv.iter().copied().fold(0.0f64, f64::max);
            let cs: Vec<f64> = kv
                .iter()
                .map(|&x| {
                    let r = if most > 0.0 { (x / most).powf(k) } else { 1.0 };
                    (floor + (1.0 - floor) * r).clamp(0.0, 1.0)
                })
                .collect();
            share = Some(match share {
                Some(s) => s.iter().zip(&cs).map(|(a, b)| a * b).collect(),
                None => cs,
            });
        }
        if let Some(sh) = share {
            st.every = Some((1.0, 1.0));
            st.keep = Some(curve_along(&pc.pts, |i| sh[i]));
        }
        // Brushes by formulas at each sample.
        if c.vel.is_some() || c.gate.is_some() || c.sub.is_some() {
            let mut vals: Vec<[f64; 3]> = Vec::with_capacity(n);
            for i in 0..n {
                let t = pc.ts[i];
                let mut v = self.vals(seed, l, i, t);
                v[1] = pc.fr[i].0;
                v[2] = pc.fr[i].1;
                v[3] = pc.pts[i].s;
                let ev = |f: &Option<F>, lo: f64, hi: f64| -> Result<f64, String> {
                    match f {
                        Some(f) => {
                            f.e.eval_vars(&v)
                                .map(|x| x.clamp(lo, hi))
                                .map_err(|e| say(f.name, &e, Some(t)))
                        }
                        None => Ok(0.0),
                    }
                };
                vals.push([
                    ev(&c.gate, 1.0, 100.0)? / 100.0,
                    ev(&c.vel, 1.0, 127.0)? / 127.0,
                    ev(&c.sub, 0.0, 4095.0)?.round() / 4095.0,
                ]);
            }
            if c.gate.is_some() {
                st.grad.gate = Some((0.0, 1.0));
            }
            if c.vel.is_some() {
                st.grad.vel = Some((0.0, 127.0));
            }
            if c.sub.is_some() {
                st.grad.sub = Some((0.0, 4095.0));
            }
            st.shape = Some(Box::new([
                curve_along(&pc.pts, |i| vals[i][0]),
                curve_along(&pc.pts, |i| vals[i][1]),
                curve_along(&pc.pts, |i| vals[i][2]),
            ]));
        }
        Ok(st)
    }

    /// The pieces and styles, or a cap's or a formula's sentence.
    fn build(&self, f: &Frame) -> Result<Built, TransformError> {
        let c = &self.cfg;
        let curves = c.curves();
        let per = if c.adaptive.is_some() {
            SAMPLE_CAP
        } else {
            c.samples
        } as u64;
        let work: u64 = curves
            .iter()
            .map(|&(l, _)| per.saturating_mul(c.cost(l)))
            .fold(0u64, u64::saturating_add);
        if work > EVAL_CAP {
            return Err(TransformError::TooLarge(format!(
                "The formulas would take {} steps to evaluate; at most {} are allowed — use fewer samples, layers or morph curves, or shorter formulas.",
                group_digits(work),
                group_digits(EVAL_CAP)
            )));
        }
        let g = BoxG::of(f);
        let gb = Gb::of(&g, c.rotate);
        let m = Aff {
            a: g.step,
            ..Aff::ID
        };
        let mut b = Built::default();
        let (mut visits, mut cut) = (0.0, 0.0);
        for (l, mm) in curves {
            let s = self
                .samples(f.seed, l, mm, &gb)
                .map_err(TransformError::Invalid)?;
            for pc in self.pieces(&s, &gb) {
                let st = self
                    .style(f.seed, l, &pc)
                    .map_err(TransformError::Invalid)?;
                if st.keep.is_some() || st.varies() {
                    let q: Vec<Pt> = pc.pts.iter().map(|&x| m.ap(x)).collect();
                    let size = measure(&q, &g);
                    visits += st.visits(&size);
                    if visits > VISIT_CAP {
                        return Err(too_many_visits(visits));
                    }
                    // V09-S60: brush formulas cut strokes into pieces.
                    cut += st.pieces(&size, false, f, &f.band());
                    if cut > PIECE_CAP {
                        return Err(too_many_pieces(cut));
                    }
                }
                b.paths.push((pc.pts, st, l));
            }
        }
        Ok(b)
    }

    fn built(&self, f: &Frame) -> Option<&Built> {
        self.built.get_or_init(|| self.build(f)).as_ref().ok()
    }

    /// The frame with layer `l`'s subchannel position.
    fn frame_of(&self, f: &Frame, l: usize) -> Frame {
        let mut g = f.clone();
        g.brush.sub = (g.brush.sub + self.cfg.layer_sub * l as u32).min(4095);
        g
    }
}

impl Shapes for Parametric {
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
        for (p, st, l) in &b.paths {
            let fl = self.frame_of(f, *l);
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
        let mut total = 0.0;
        for (p, st, l) in &b.paths {
            let fl = self.frame_of(f, *l);
            total += st.lower(std::slice::from_ref(p), &m, false, &g, &rows, &fl);
        }
        if total.is_finite() && total < u64::MAX as f64 {
            total as u64
        } else {
            0
        }
    }
}

fn parametric_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = Frame::of(p, b.ctx, b.stats);
    let g = Parametric::new(Cfg::read(p)?);
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
        let names: Vec<&str> = lib.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, ["Lissajous", "Rose", "Spiral", "Heart", "Sine"]);
        assert!(parse_library("A t | t").is_err());
        assert!(parse_library("A: t").is_err());
        let e = parse_library("A: t | sin(").unwrap_err();
        assert!(e.contains("“A”") && e.ends_with('.'), "{e}");
    }

    #[test]
    fn noise_is_smooth_seeded_and_bounded() {
        let a: Vec<f64> = (0..1000).map(|i| noise(7, i as f64 / 100.0)).collect();
        assert!(a.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(a.windows(2).all(|w| (w[1] - w[0]).abs() < 0.1));
        assert_ne!(noise(7, 3.3), noise(8, 3.3));
        assert_eq!(noise(7, 3.3), noise(7, 3.3));
    }

    /// The lower bound never passes the count and the cheap bound never
    /// falls below it, over seeded random settings (functions on with
    /// probability ⅓) in small boxes.
    #[test]
    fn the_lower_bound_never_passes_the_count_and_the_bound_never_falls_below_it() {
        let spec = parametric_spec();
        let mut rng = Rng::new(0x5345_3538);
        for draw in 0..200 {
            let mut p = ParamSet::defaults(&spec.params);
            p.set("gen.length", ParamValue::Int(rng.range_i64(240, 240 * 200)));
            p.set("gen.low", ParamValue::Int(rng.range_i64(30, 60)));
            p.set("gen.high", ParamValue::Int(rng.range_i64(60, 100)));
            p.set("gen.seed", ParamValue::Int(rng.range_i64(1, 1000)));
            for (id, _, _) in FUNCTIONS {
                p.set(id, ParamValue::Bool(rng.below(3) == 0));
            }
            for (k, lo, hi) in [
                ("c05_sample_count.n", 2, 400),
                ("c09_multiple_equation_layers.count", 1, 2),
                ("c23_formula_a_b_morph.steps", 1, 4),
            ] {
                p.set(k, ParamValue::Int(rng.range_i64(lo, hi)));
            }
            for (k, lo, hi) in [
                ("c03_t_start.t", -1.0, 0.5),
                ("c04_t_end.t", 0.5, 2.0),
                ("c06_adaptive_sampling.tolerance", 0.05, 2.0),
                ("c14_rotation.angle", -180.0, 180.0),
                ("c22_wrap_reflect_boundaries.extent", 1.0, 4.0),
                ("c20_discontinuity_detection.jump", 1.0, 60.0),
            ] {
                p.set(k, ParamValue::Float(rng.range_f64(lo, hi)));
            }
            p.set(
                "c22_wrap_reflect_boundaries.mode",
                ParamValue::Choice(rng.below(2) as u32),
            );
            let params = Params::new(&spec.params, &p);
            let mut c = Ctx::simple(960);
            c.cursor = rng.below(10_000);
            let f = Frame::of(&params, &c, &SourceStats::default());
            let Ok(cfg) = Cfg::read(&params) else {
                continue;
            };
            let g = Parametric::new(cfg);
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
