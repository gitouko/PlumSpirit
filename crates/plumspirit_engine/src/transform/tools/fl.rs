//! FL Studio-inspired Piano Roll tools (original PlumSpirit implementations of
//! the documented behaviour — docs/references/v0_7/FL_TOOL_BEHAVIOR.md).
//! Wave D: Arpeggiator, Articulate, Chopper.

use super::super::boundary::{self, PitchPolicy};
use super::super::drivers::Place;
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, note_key, salt};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::density_window::{
    PolyOrder, TrailingWindow, budget_keep, budget_keep_if_by, holding_loads, poly_cap_keep_by,
    poly_ceiling_ends_by, trailing_counts,
};
use super::super::shared::euclid::Euclid;
use super::super::shared::metric_position::metric_position;
use super::super::shared::phrase_detect::Phrases;
use super::super::shared::scale_lattice::Lattice;
use super::super::shared::seeded_stream::{region_seed, salt_of, stream};
use super::super::shared::tempo_time::{MAX_HZ, MIN_HZ, TempoTime};
use super::super::sweep::{PerLevel, Rebuild, SweptParams};
use super::super::theory::{KEY_NAMES, Scale, scale_names};
use super::super::{
    FLAG_MUTED, Growth, Kernel, Output, PointKernel, Source, TNote, TransformError,
};
use super::{
    Contextual, NOTE_VALUES, Pointwise, TIME_MULS, art_grid_values, boxed, next_onsets, note_value,
    per_track, pos, smooth_noise, time_mul, warp,
};

/// Onset groups (chords) of a time-ordered index list: notes starting within
/// `tol` ticks of the group's first note (the shared `chord_groups` helper,
/// V09-S34).
fn chords(src: &Source, idx: &[usize], tol: u64) -> ChordGroups {
    ChordGroups::of(&src.notes, idx, tol)
}

// ================================================================ Arpeggiator

pub const ARPEGGIATE: ToolDef = ToolDef {
    id: "arpeggiate",
    spec: arp_spec,
    build: arp_build,
};

const ARP_PATTERNS: &[&str] = &[
    "Up",
    "Down",
    "Up-Down",
    "Down-Up",
    "Converge",
    "Diverge",
    "Thumb (low note between)",
    "Pinky (top note between)",
    "Random (seeded)",
    "Custom (list)",
];
const PATTERN_MODES: &[&str] = &["Normal", "Flip vertical", "Alternate"];
const RANGE_MODES: &[&str] = &[
    "Normal (low → high)",
    "Flip (high → low)",
    "Alternate (low → high → low)",
];
const SYNC_MODES: &[&str] = &[
    "Time (play the pattern once)",
    "Block (until every chord note ends)",
    "Chord (until any chord note ends)",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const ARPEGGIATE_HELP: &[(&str, &str)] = &[
    (
        "pattern",
        "The order the arpeggio walks the chord's notes: up, down, up and back, inward from both ends, outward from the middle, alternating with the lowest or the top note, shuffled (seeded) or your own list.",
    ),
    (
        "custom",
        "For Custom: the steps as places in the chord's ladder (0 = the lowest note, 1 = the next …; −1 = the top note, −2 the one below it); the list repeats.",
    ),
    (
        "pvel",
        "Velocities given to the steps in turn (the list repeats); Levels decides how much they replace the chord notes' own velocities.",
    ),
    (
        "mode",
        "Normal plays the pattern as it is; Flip Vertical turns it upside down (the top note where the lowest was); Alternate flips every other cycle.",
    ),
    ("step", "The length of one arpeggio step, as a note value."),
    (
        "step.ticks",
        "For Custom: the length of one arpeggio step, in ticks.",
    ),
    (
        "mul",
        "Makes the steps faster (×2 faster halves each step) or slower than the chosen step length.",
    ),
    (
        "range",
        "How many octaves the arpeggio spans: the chord's notes repeated in each octave above.",
    ),
    (
        "range.mode",
        "The order of the octaves: low to high, high to low, or low to high and back down.",
    ),
    (
        "sync",
        "How long the arpeggio runs: the pattern once, until every note of the chord has ended, or until the first of them ends.",
    ),
    (
        "gate",
        "Each step's note length as a share of the step (100 % = notes touch, lower = shorter, detached notes).",
    ),
    (
        "levels",
        "Blends each step's velocity from the chord note's own (0) to the pattern velocity (1).",
    ),
    (
        "tol",
        "Notes that start within this many ticks of a chord's first note belong to that chord (0 = only notes starting together).",
    ),
    (
        "group",
        "Puts each chord's arpeggio into a note group of its own, so it can be selected as one.",
    ),
    (
        "a1",
        "Splits every step into quicker sub-notes jumping between octaves, repeated at deeper levels.",
    ),
    (
        "a1.depth",
        "How many times the split repeats inside each sub-note.",
    ),
    (
        "a1.branch",
        "How many sub-notes each step splits into (the original pitch, an octave down, an octave up …).",
    ),
    (
        "a2",
        "Controls how far the arpeggio may jump between consecutive steps.",
    ),
    (
        "a2.mode",
        "Prevent moves a step by octaves until it is within Largest Jump of the previous step; Encourage jumps an octave away whenever the move would be smaller.",
    ),
    (
        "a2.max",
        "The jump size, in semitones, that Prevent never exceeds and Encourage always reaches.",
    ),
    (
        "a3",
        "Morphs the arpeggio from the main pattern into a second one across the selection.",
    ),
    ("a3.b", "The pattern the arpeggio morphs toward."),
    (
        "a3.curve",
        "How far the morph has gone along the selection's time span (0 = the main pattern, 1 = Contour B).",
    ),
    (
        "a4",
        "Changes how many steps play, from the selection's start to its end.",
    ),
    (
        "a4.from",
        "The step density at the selection's start (×2 = twice as many, twice as short steps).",
    ),
    ("a4.to", "The step density at the selection's end."),
    (
        "a5",
        "Adds layers that play the same pattern at other rates against the main one, for polyrhythms.",
    ),
    (
        "a5.layers",
        "Each layer's steps per pattern cycle (3 against a 4-step pattern gives 3 against 4).",
    ),
    (
        "a6",
        "Gives the arpeggio's steps the track's subchannels in turn.",
    ),
    (
        "a6.seq",
        "The subchannel of each step in turn, by its place in the track's list (0 = the first); the list repeats.",
    ),
    ("a7", "Mirrors every step's pitch around an axis key."),
    ("a7.axis", "The key the pitches are mirrored around."),
    (
        "a7.kind",
        "Chromatic mirrors by semitones; Diatonic mirrors by steps of the key and scale below, so the result stays in key.",
    ),
    ("a7.key", "For Diatonic: the key of the scale."),
    ("a7.scale", "For Diatonic: the scale the mirror follows."),
    (
        "a8",
        "Shapes velocity and length across every pattern cycle, for accents.",
    ),
    (
        "a8.vel",
        "The velocity multiplier from a cycle's first step (left) to its last (right).",
    ),
    (
        "a8.gate",
        "The length multiplier from a cycle's first step (left) to its last (right).",
    ),
    (
        "a9",
        "Adds copies of the arpeggio that start a few steps later and transposed, like a canon.",
    ),
    (
        "a9.copies",
        "How many voices play, the original included (1 = no copies).",
    ),
    (
        "a9.phase",
        "How many steps later each copy starts than the one before.",
    ),
    (
        "a9.semis",
        "How many semitones each copy is transposed from the one before.",
    ),
    (
        "a10",
        "Moves each chord's arpeggio by whole octaves relative to the previous chord's.",
    ),
    (
        "a10.goal",
        "Minimize keeps the arpeggio close to the previous chord's register; Maximize makes it jump as far as possible.",
    ),
    (
        "n01",
        "Plays only the steps of a Euclidean rhythm: Pulses hits spread as evenly as possible over Steps.",
    ),
    ("n01.steps", "The length of the rhythm, in steps."),
    ("n01.pulses", "How many of those steps play."),
    ("n01.rot", "Rotates the rhythm by this many steps."),
    (
        "n01.rest",
        "What the steps that do not play do: stay silent, lengthen the previous note over them, or tie into the next note (it starts earlier).",
    ),
    (
        "n01.phase",
        "Restart the rhythm with each chord, or let it run on from chord to chord.",
    ),
    ("n02", "Turns steps into quick bursts of repeated notes."),
    (
        "n02.repeats",
        "How many notes each step bursts into, step by step in turn (1 = no burst); the list repeats.",
    ),
    (
        "n02.amount",
        "How much of the burst plays along the selection's time span (0 = single notes, 1 = all the repeats).",
    ),
    (
        "n02.decay",
        "Each repeat of a burst is this much quieter than the one before.",
    ),
    (
        "n02.gate",
        "Each repeat's length as a share of its slot in the burst.",
    ),
    (
        "n03",
        "Sets the step rate in real time (steps per second or milliseconds per step), whatever the tempo.",
    ),
    (
        "n03.unit",
        "Whether the rates below are steps per second (Hz) or milliseconds per step.",
    ),
    ("n03.from", "The rate at the selection's start."),
    ("n03.to", "The rate at the selection's end."),
    (
        "n03.curve",
        "How the rate travels from the start rate to the end rate across the selection.",
    ),
    (
        "n04",
        "Walks through the pattern by a stride instead of step by step: each step moves this many places in the pattern.",
    ),
    (
        "n04.stride",
        "How many places in the pattern each step moves.",
    ),
    (
        "n04.change",
        "Added to the stride after every full cycle, so the walk speeds up or slows down.",
    ),
    (
        "n04.ends",
        "At the pattern's end, wrap around to the start or bounce back.",
    ),
    (
        "n04.start",
        "The place in the pattern the walk starts from (0 = the first).",
    ),
    (
        "n05",
        "Fills the gaps between the arpeggio's notes with passing notes.",
    ),
    (
        "n05.fill",
        "Which notes fill the gaps: the key and scale, its pentatonic, or every semitone.",
    ),
    (
        "n05.key",
        "The key of the scale the passing notes come from.",
    ),
    ("n05.scale", "The scale the passing notes come from."),
    (
        "n05.most",
        "At most this many passing notes between two arpeggio notes (spread evenly through the gap).",
    ),
    (
        "n05.vel",
        "The passing notes' velocity as a share of the note they follow.",
    ),
    (
        "n06",
        "Makes the last steps before a chord change glide toward the next chord's lowest note.",
    ),
    ("n06.steps", "How many of the last steps glide."),
    (
        "n06.path",
        "The notes the glide steps through: every semitone, the key and scale, or the next chord's own notes.",
    ),
    ("n06.key", "For Key and Scale: the key of the scale."),
    (
        "n06.scale",
        "For Key and Scale: the scale the glide steps through.",
    ),
    (
        "n06.shape",
        "How the glide moves from the current note to the target (a straight line by default).",
    ),
    (
        "n07",
        "Adds a pedal note under the arpeggio: the chord's lowest or top note, held or struck again.",
    ),
    (
        "n07.anchor",
        "Which chord note becomes the pedal: the lowest, the highest, or both.",
    ),
    (
        "n07.every",
        "Strike the pedal again every this many steps (0 = hold it through the chord).",
    ),
    (
        "n07.oct",
        "Moves the pedal this many octaves down (0 = at the chord note's own pitch).",
    ),
    (
        "n07.vel",
        "The pedal's velocity as a share of its chord note's.",
    ),
    (
        "n08",
        "Fits a whole number of pattern cycles into each chord, so the arpeggio ends with the chord.",
    ),
    ("n08.cycles", "How many pattern cycles fill each chord."),
    (
        "n08.round",
        "Exact Ticks fits the cycles exactly; Nearest Grid Step rounds the step to the snap; At Least One Cycle plays fewer cycles where steps would get shorter than Shortest Step.",
    ),
    ("n08.min", "No step is made shorter than this, in ticks."),
    (
        "n09",
        "Walks the pattern by seeded chance: each step stays, moves up, moves down or leaps.",
    ),
    (
        "n09.w",
        "The relative chances of staying, moving up, moving down and leaping, in that order.",
    ),
    ("n09.leap", "The largest leap, in places of the pattern."),
    (
        "n09.mem",
        "How many recent moves cannot be repeated straight away (0 = any move may repeat).",
    ),
    (
        "n10",
        "Keeps the arpeggio playing through short rests between chords.",
    ),
    (
        "n10.until",
        "How far the latched arpeggio plays: to the next chord, to the end of the bar, or for at most Longest Fill.",
    ),
    (
        "n10.fill",
        "For Longest Fill: the longest stretch, in ticks, the arpeggio plays on into a rest.",
    ),
    (
        "n10.below",
        "Only rests shorter than this (ticks) are filled.",
    ),
    (
        "n10.fade",
        "The velocity multiplier across the filled rest (1 = full, 0 = silent).",
    ),
    (
        "n11",
        "Keeps the arpeggio under a notes-per-second limit, measured in a sliding window.",
    ),
    (
        "n11.nps",
        "The most notes per second the arpeggio may play on one track.",
    ),
    (
        "n11.window",
        "The length of the sliding window the notes are counted in.",
    ),
    (
        "n11.how",
        "Lengthen Steps drops notes over the limit and lengthens the note before; Drop Off-Steps removes every other step first; Merge Octave Copies removes doubled octaves first.",
    ),
];

fn arp_spec() -> ToolSpec {
    let mut p = vec![
        choice("pattern", "Pattern", ARP_PATTERNS, 0),
        list(
            "custom",
            "Custom steps (chord index, −1 = top)",
            -64,
            64,
            256,
            &[0, 1, 2, 1],
        ),
        list("pvel", "Pattern velocities", 1, 127, 256, &[100]),
        choice("mode", "Pattern mode", PATTERN_MODES, 0),
        choice("step", "Step", NOTE_VALUES, 4),
        ticks("step.ticks", "Custom step", 1, 1 << 40, 240),
        choice("mul", "Time multiplication", TIME_MULS, 3),
        int("range", "Range (octaves)", 1, 8, 1),
        choice("range.mode", "Range pattern", RANGE_MODES, 0),
        choice("sync", "Sync", SYNC_MODES, 1),
        float("gate", "Gate", 1.0, 100.0, 90.0, "%"),
        float(
            "levels",
            "Levels (source → pattern velocity)",
            0.0,
            1.0,
            0.0,
            "",
        ),
        ticks("tol", "Chord tolerance", 0, 1 << 30, 0),
        flag("group", "Group notes", false),
        seed("seed"),
        adv_switch(1, "Fractal octave cascade"),
        int("a1.depth", "Depth", 1, 3, 1).adv(1),
        int("a1.branch", "Branches", 2, 4, 2).adv(1),
        adv_switch(2, "Voice-crossing control"),
        choice("a2.mode", "Crossings", &["Prevent", "Encourage"], 0).adv(2),
        int("a2.max", "Largest jump (semitones)", 1, 48, 7).adv(2),
        adv_switch(3, "Pitch-contour morph"),
        choice("a3.b", "Contour B", ARP_PATTERNS, 1).adv(3),
        curve(
            "a3.curve",
            "A → B across the selection",
            Curve::line(0.0, 1.0),
        )
        .adv(3),
        adv_switch(4, "Density envelope"),
        float("a4.from", "Density at start", 0.25, 8.0, 1.0, "×").adv(4),
        float("a4.to", "Density at end", 0.25, 8.0, 2.0, "×").adv(4),
        adv_switch(5, "Polyrhythmic layers"),
        list("a5.layers", "Steps per cycle, per layer", 1, 64, 4, &[3, 4]).adv(5),
        adv_switch(6, "Subchannel cycler"),
        list("a6.seq", "Subchannel positions", 0, 4095, 256, &[0, 1]).adv(6),
        adv_switch(7, "Harmonic reflection"),
        pitch("a7.axis", "Axis", 60).adv(7),
        choice(
            "a7.kind",
            "Reflection",
            &["Chromatic", "Diatonic (in key)"],
            0,
        )
        .adv(7),
        choice("a7.key", "Key", &KEY_NAMES, 0).adv(7),
        choice("a7.scale", "Scale", scale_names(), 0).adv(7),
        adv_switch(8, "Accent trajectory"),
        curve(
            "a8.vel",
            "Velocity across each cycle",
            Curve::line(1.0, 0.5),
        )
        .overshoot()
        .adv(8),
        curve("a8.gate", "Gate across each cycle", Curve::flat(1.0))
            .overshoot()
            .adv(8),
        adv_switch(9, "Phase-staggered copies"),
        int("a9.copies", "Copies", 1, 4, 1).adv(9),
        int("a9.phase", "Phase offset (steps)", 1, 16, 1).adv(9),
        int("a9.semis", "Transpose per copy", -24, 24, 12).adv(9),
        adv_switch(10, "Adaptive voice leading"),
        choice(
            "a10.goal",
            "Goal",
            &["Minimize movement", "Maximize movement"],
            0,
        )
        .adv(10),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Euclidean step gate", false),
        int("n01.steps", "Steps", 1, 64, 8),
        int("n01.pulses", "Pulses", 0, 64, 5),
        int("n01.rot", "Rotation", -64, 64, 0),
        choice("n01.rest", "Rest steps", ARP_RESTS, 0),
        choice("n01.phase", "Phase", ARP_EUCLID_PHASE, 0),
        flag("n02", "Ratchet bursts", false),
        list("n02.repeats", "Repeats per step", 1, 16, 64, &[1, 2, 1, 4]),
        curve(
            "n02.amount",
            "Burst amount across the selection",
            Curve::flat(1.0),
        ),
        float("n02.decay", "Decay per repeat", 0.0, 50.0, 15.0, "%"),
        float("n02.gate", "Burst gate", 5.0, 100.0, 60.0, "%"),
        flag("n03", "Real-time rate (Hz / ms)", false),
        choice("n03.unit", "Unit", ARP_RATE_UNITS, 0),
        float("n03.from", "Start rate", 0.1, 2000.0, 8.0, ""),
        float("n03.to", "End rate", 0.1, 2000.0, 8.0, ""),
        curve(
            "n03.curve",
            "Start → end across the selection",
            Curve::line(0.0, 1.0),
        )
        .overshoot(),
        flag("n04", "Modular stride walk", false),
        int("n04.stride", "Stride", 1, 24, 2),
        int("n04.change", "Stride change per cycle", -8, 8, 0),
        choice("n04.ends", "Ends", &["Wrap", "Bounce"], 0),
        int("n04.start", "Start index", 0, 63, 0),
        flag("n05", "Scale passing-tone fill", false),
        choice("n05.fill", "Fill", ARP_FILLS, 0),
        choice("n05.key", "Key", &KEY_NAMES, 0),
        choice("n05.scale", "Scale", scale_names(), 0),
        int("n05.most", "Most passing notes per gap", 0, 12, 2),
        float("n05.vel", "Fill velocity", 10.0, 100.0, 80.0, "%"),
        flag("n06", "Chord-change glide", false),
        int("n06.steps", "Glide steps", 1, 32, 4),
        choice("n06.path", "Path", ARP_GLIDE_PATHS, 0),
        choice("n06.key", "Key", &KEY_NAMES, 0),
        choice("n06.scale", "Scale", scale_names(), 0),
        curve("n06.shape", "Glide shape", Curve::line(0.0, 1.0)).overshoot(),
        flag("n07", "Pedal anchor voice", false),
        choice("n07.anchor", "Anchor", ARP_ANCHORS, 0),
        int("n07.every", "Re-strike every (steps; 0 = hold)", 0, 64, 0),
        int("n07.oct", "Octave", -3, 0, -1),
        float("n07.vel", "Anchor velocity", 10.0, 150.0, 90.0, "%"),
        flag("n08", "Fit cycles to the chord", false),
        float("n08.cycles", "Cycles per chord", 0.25, 64.0, 2.0, "×"),
        choice("n08.round", "Rounding", ARP_FIT_ROUNDING, 0),
        ticks("n08.min", "Shortest step", 1, 1 << 30, 30),
        flag("n09", "Markov ladder walk", false),
        list(
            "n09.w",
            "Weights: stay, up, down, leap",
            0,
            100,
            4,
            &[10, 40, 40, 10],
        ),
        int("n09.leap", "Largest leap", 2, 24, 4),
        int("n09.mem", "Memory (moves not repeated)", 0, 3, 1),
        flag("n10", "Latch across rests", false),
        choice("n10.until", "Latch until", ARP_LATCH_UNTIL, 0),
        ticks("n10.fill", "Longest fill", 1, 1 << 30, 1920),
        ticks("n10.below", "Only rests shorter than", 1, 1 << 30, 3840),
        curve("n10.fade", "Fade across the fill", Curve::line(1.0, 0.3)).overshoot(),
        flag("n11", "NPS ceiling", false),
        int("n11.nps", "Most notes per second", 1, 1_000_000, 2000),
        float("n11.window", "Window", 10.0, 2000.0, 1000.0, "ms"),
        choice("n11.how", "Strategy", ARP_CEILING, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, ARPEGGIATE_HELP);
    ToolSpec {
        id: "arpeggiate",
        name: "Arpeggiator",
        family: Family::Fl,
        summary: "Turns chords into arpeggios: a pattern walks the chord notes over an octave range at a step length, until the chord (or block) ends.",
        params: p,
        advanced: vec![
            "Fractal octave cascade",
            "Voice-crossing control",
            "Pitch-contour morph",
            "Density envelope",
            "Polyrhythmic layers",
            "Subchannel cycler",
            "Harmonic reflection",
            "Accent trajectory",
            "Phase-staggered copies",
            "Adaptive voice leading",
        ],
        functions: vec![
            FunctionSpec {
                id: "n01",
                name: "Euclidean step gate",
                order: 110,
            },
            FunctionSpec {
                id: "n02",
                name: "Ratchet bursts",
                order: 120,
            },
            FunctionSpec {
                id: "n03",
                name: "Real-time rate (Hz / ms)",
                order: 130,
            },
            FunctionSpec {
                id: "n04",
                name: "Modular stride walk",
                order: 140,
            },
            FunctionSpec {
                id: "n05",
                name: "Scale passing-tone fill",
                order: 150,
            },
            FunctionSpec {
                id: "n06",
                name: "Chord-change glide",
                order: 160,
            },
            FunctionSpec {
                id: "n07",
                name: "Pedal anchor voice",
                order: 170,
            },
            FunctionSpec {
                id: "n08",
                name: "Fit cycles to the chord",
                order: 180,
            },
            FunctionSpec {
                id: "n09",
                name: "Markov ladder walk",
                order: 190,
            },
            FunctionSpec {
                id: "n10",
                name: "Latch across rests",
                order: 200,
            },
            FunctionSpec {
                id: "n11",
                name: "NPS ceiling",
                order: 700,
            },
        ],
        needs_selection: true,
        context: false,
    }
}

const ARP_RESTS: &[&str] = &[
    "Silence",
    "Hold the previous step",
    "Tie into the next step",
];
const ARP_EUCLID_PHASE: &[&str] = &["Restart with each chord", "Run on across chords"];
const ARP_RATE_UNITS: &[&str] = &["Hz (steps per second)", "Milliseconds per step"];
const ARP_FILLS: &[&str] = &["Key and scale", "Pentatonic of the scale", "Chromatic"];
const ARP_GLIDE_PATHS: &[&str] = &["Chromatic", "Key and scale", "Nearest chord tone"];
const ARP_ANCHORS: &[&str] = &["Lowest note", "Highest note", "Both"];
const ARP_FIT_ROUNDING: &[&str] = &["Exact ticks", "Nearest grid step", "At least one cycle"];
const ARP_LATCH_UNTIL: &[&str] = &["Next chord", "Bar end", "Longest fill"];
const ARP_CEILING: &[&str] = &["Lengthen steps", "Drop off-steps", "Merge octave copies"];

/// Index sequence of one pattern cycle over a ladder of `l` notes.
fn pattern_seq(kind: usize, l: usize, custom: &[i64], rng: Option<&mut Rng>) -> Vec<usize> {
    if l == 0 {
        return Vec::new();
    }
    let up: Vec<usize> = (0..l).collect();
    match kind {
        0 => up,
        1 => up.into_iter().rev().collect(),
        2 => {
            let mut v = up.clone();
            v.extend((1..l.saturating_sub(1)).rev());
            v
        }
        3 => {
            let mut v: Vec<usize> = up.iter().rev().copied().collect();
            v.extend(1..l.saturating_sub(1));
            v
        }
        4 => {
            let (mut a, mut b) = (0usize, l - 1);
            let mut v = Vec::with_capacity(l);
            while a <= b {
                v.push(a);
                if a != b {
                    v.push(b);
                }
                a += 1;
                if b == 0 {
                    break;
                }
                b -= 1;
            }
            v
        }
        5 => {
            let m = l / 2;
            let mut v = vec![m];
            for d in 1..=l {
                if m + d < l {
                    v.push(m + d);
                }
                if d <= m {
                    v.push(m - d);
                }
            }
            v.truncate(l);
            v
        }
        6 => {
            let mut v = Vec::new();
            for i in 1..l.max(2) {
                v.push(0);
                v.push(i.min(l - 1));
            }
            v
        }
        7 => {
            let mut v = Vec::new();
            for i in 0..l.saturating_sub(1).max(1) {
                v.push(l - 1);
                v.push(i);
            }
            v
        }
        8 => {
            let mut v = up;
            if let Some(r) = rng {
                for i in (1..v.len()).rev() {
                    let j = r.range_i64(0, i as i64) as usize;
                    v.swap(i, j);
                }
            }
            v
        }
        _ => {
            let c: Vec<usize> = custom
                .iter()
                .map(|&x| x.rem_euclid(l as i64) as usize)
                .collect();
            if c.is_empty() { up } else { c }
        }
    }
}

/// The Arpeggiator's functions declared by stable id (V09-S34; TOOL_CATALOG
/// `arpeggiate` n01 … n11). `None` = switched off; with every one off the
/// kernel runs exactly as in v0.8.
#[derive(Default)]
struct ArpNew {
    /// n01: the pattern, rest steps (0 silence, 1 hold the previous step,
    /// 2 tie into the next step), run on across chords.
    euclid: Option<(Euclid, usize, bool)>,
    /// n02: repeats per step, amount across the selection, decay per repeat
    /// and burst gate (fractions).
    ratchet: Option<(Vec<i64>, Curve, f64, f64)>,
    /// n03: milliseconds per step (else Hz), start, end, curve across the
    /// selection.
    rate: Option<(bool, f64, f64, Curve)>,
    /// n04: stride, change per cycle, bounce (else wrap), start index.
    stride: Option<(i64, i64, bool, i64)>,
    /// n05: lattice, most passing notes per gap, velocity (fraction).
    fill: Option<(Lattice, usize, f64)>,
    /// n06: glide steps, path (0 chromatic, 1 key and scale, 2 the next
    /// chord's tones), the key-and-scale lattice, shape.
    glide: Option<(usize, usize, Lattice, Curve)>,
    /// n07: anchor (0 lowest, 1 highest, 2 both), re-strike every (0 =
    /// hold), octave, velocity (fraction).
    pedal: Option<(usize, u64, i64, f64)>,
    /// n08: cycles per chord, rounding (0 exact ticks, 1 nearest grid step,
    /// 2 at least one cycle), shortest step.
    fit: Option<(f64, usize, u64)>,
    /// n09: weights (stay, up, down, leap), largest leap, memory.
    markov: Option<([f64; 4], i64, usize)>,
    /// n10: until (0 next chord, 1 bar end, 2 longest fill), longest fill,
    /// only rests shorter than, fade.
    latch: Option<(usize, u64, u64, Curve)>,
    /// n11: most notes per second, window (µs), strategy (0 lengthen steps,
    /// 1 drop off-steps, 2 merge octave copies).
    ceiling: Option<(u64, u64, usize)>,
}

/// One arpeggio step: its start, length, and whether it lies in a latched
/// rest (n10).
#[derive(Clone, Copy, Debug)]
struct ArpStep {
    t: u64,
    len: u64,
    latched: bool,
}

/// n08: a chord's step clock.
#[derive(Clone, Copy, Debug)]
enum ArpFit {
    /// Every step this many ticks.
    Uniform(u64),
    /// Step `k` at `start + ⌊k · span / per⌋`.
    Exact { span: u64, per: f64 },
}

/// n08: the clock that fits `cycles` pattern cycles of `base_len` steps
/// into `span` ticks.
fn arp_fit(
    cycles: f64,
    rounding: usize,
    shortest: u64,
    span: u64,
    base_len: usize,
    snap: u64,
) -> ArpFit {
    let l = base_len.max(1) as f64;
    let span = span.max(1);
    let step = |c: f64| span as f64 / (c * l);
    match rounding {
        1 => {
            let g = snap.max(1);
            let s = ((step(cycles) / g as f64).round() as u64).max(1) * g;
            ArpFit::Uniform(s.max(shortest))
        }
        2 => {
            let fit = (span as f64 / (shortest as f64 * l)).floor();
            ArpFit::Exact {
                span,
                per: cycles.min(fit).max(1.0) * l,
            }
        }
        _ if step(cycles) < shortest as f64 => ArpFit::Uniform(shortest),
        _ => ArpFit::Exact {
            span,
            per: cycles * l,
        },
    }
}

/// n05: the ladder with up to `most` lattice pitches between neighbours,
/// evenly chosen, each a copy of the lower neighbour's note at `vel` × its
/// velocity.
fn arp_fill(full: &[(i64, TNote)], lat: Lattice, most: usize, vel: f64) -> Vec<(i64, TNote)> {
    if most == 0 || full.len() < 2 {
        return full.to_vec();
    }
    let mut out = Vec::with_capacity(full.len() * (most + 1));
    for w in full.windows(2) {
        let ((a, n), b) = (w[0], w[1].0);
        out.push(w[0]);
        let (lo, hi) = (a.min(b), a.max(b));
        let m = if hi - lo > 1 {
            lat.steps_between(lo, hi - 1).max(0) as usize
        } else {
            0
        };
        let take = most.min(m);
        let dir = if b > a { 1 } else { -1 };
        let mut tpl = n;
        tpl.vel = (f64::from(n.vel) * vel).round().clamp(1.0, 127.0) as u8;
        for j in 0..take {
            let q = ((j + 1) * (m + 1) / (take + 1)) as i64;
            out.push((lat.step(a, dir * q), tpl));
        }
    }
    out.push(full[full.len() - 1]);
    out
}

/// n09's state: its stream, weights (stay, up, down, leap), largest leap,
/// memory and the last moves (newest first).
struct ArpMarkov {
    rng: Rng,
    w: [f64; 4],
    leap: i64,
    mem: usize,
    recent: [u8; 3],
}

/// n04 / n09: which position of the pattern cycle each step plays.
struct ArpWalk {
    pos: i64,
    stride: Option<(i64, i64, bool)>,
    markov: Option<ArpMarkov>,
    base_len: u64,
}

impl ArpWalk {
    fn new(a: &Arp, seed_key: &[u64], base_len: usize) -> ArpWalk {
        ArpWalk {
            pos: a.new.stride.map_or(0, |s| s.3),
            stride: a.new.stride.map(|(s, c, b, _)| (s, c, b)),
            markov: a.new.markov.map(|(w, leap, mem)| ArpMarkov {
                rng: stream(a.seed, salt_of("n09"), seed_key),
                w,
                leap,
                mem,
                recent: [u8::MAX; 3],
            }),
            base_len: base_len.max(1) as u64,
        }
    }

    /// The pattern position step `k` plays in a cycle of `len` steps.
    fn phase(&self, k: u64, len: usize) -> usize {
        if self.stride.is_none() && self.markov.is_none() {
            return (k % len.max(1) as u64) as usize;
        }
        let len = len.max(1) as i64;
        // Markov alone bounces (a line inside the chord); the stride's ends
        // decide otherwise.
        let bounce = self.stride.is_none_or(|s| s.2);
        if !bounce || len == 1 {
            return self.pos.rem_euclid(len) as usize;
        }
        let per = 2 * (len - 1);
        let q = self.pos.rem_euclid(per);
        (if q < len { q } else { per - q }) as usize
    }

    /// Moves on after step `k`.
    fn advance(&mut self, k: u64) {
        let s = self
            .stride
            .map_or(1, |(s, c, _)| s + c * (k / self.base_len) as i64);
        let mv = match &mut self.markov {
            None => s,
            Some(ArpMarkov {
                rng,
                w,
                leap,
                mem,
                recent,
            }) => {
                let mut ws = *w;
                for (kind, x) in ws.iter_mut().enumerate() {
                    if recent[..(*mem).min(3)].contains(&(kind as u8)) {
                        *x = 0.0;
                    }
                }
                let kind = rng.weighted(&ws).or_else(|| rng.weighted(w)).unwrap_or(1);
                recent.rotate_right(1);
                recent[0] = kind as u8;
                match kind {
                    0 => 0,
                    1 => s,
                    2 => -s,
                    _ => {
                        let size = 2 + rng.below((*leap - 1).max(1) as u64) as i64;
                        if rng.below(2) == 0 { size } else { -size }
                    }
                }
            }
        };
        self.pos += mv;
    }
}

/// The step clock of one layer of one chord: steps from `start` while
/// before `end` (`limit`, or the end of a latched rest — steps at or after
/// `limit` are latched): uniform (`lstep`), exact (n08) or in real time
/// (n03, the layer's step scaled by `ratio`). Lazy: no buffer of steps.
#[derive(Clone, Copy)]
struct ArpClock<'a> {
    rate: Option<&'a (bool, f64, f64, Curve)>,
    fit: Option<(u64, f64)>,
    once: bool,
    start: u64,
    limit: u64,
    end: u64,
    lstep: u64,
    ratio: f64,
    /// n03: the fewest ticks between real-time steps (the preflight's bound
    /// holds even where microseconds lose precision far out).
    min_gap: u64,
    base_len: u64,
    min_start: u64,
    span: u64,
    at_us: Option<f64>,
    t: u64,
    k: u64,
    done: bool,
}

impl ArpClock<'_> {
    fn next(&mut self, tt: &mut TempoTime) -> Option<ArpStep> {
        if self.done || self.t >= self.end || (self.once && self.k >= self.base_len) {
            return None;
        }
        let t = self.t;
        let next: u128 = match (self.fit, self.rate) {
            (Some((span, per)), _) => {
                u128::from(self.start) + ((self.k + 1) as f64 * span as f64 / per).floor() as u128
            }
            (None, Some((ms, a, b, curve))) => {
                let at = *self.at_us.get_or_insert_with(|| tt.micros_at(t) as f64);
                let pos = if self.span == 0 {
                    0.0
                } else {
                    (t.saturating_sub(self.min_start) as f64 / self.span as f64).min(1.0)
                };
                let v = a + (b - a) * curve.eval(pos);
                let hz = if *ms { 1000.0 / v.max(0.1) } else { v };
                let at = at + 1e6 / hz.clamp(MIN_HZ, MAX_HZ) * self.ratio;
                self.at_us = Some(at);
                if at >= u64::MAX as f64 {
                    // Beyond the representable song time: no further steps.
                    self.done = true;
                    u128::from(self.end)
                } else {
                    u128::from(tt.tick_at_micros(at.round() as u64))
                        .max(u128::from(t) + u128::from(self.min_gap))
                }
            }
            _ => u128::from(t) + u128::from(self.lstep),
        };
        let next = next.max(u128::from(t) + 1);
        let step = ArpStep {
            t,
            len: u64::try_from(next - u128::from(t)).unwrap_or(u64::MAX),
            latched: t >= self.limit,
        };
        match u64::try_from(next) {
            Ok(n) => self.t = n,
            Err(_) => self.done = true,
        }
        self.k += 1;
        Some(step)
    }
}

struct Arp {
    pattern: usize,
    custom: Vec<i64>,
    pvel: Vec<i64>,
    mode: usize,
    step: u64,
    range: i64,
    range_mode: usize,
    sync: usize,
    gate: f64,
    levels: f64,
    tol: u64,
    group: bool,
    seed: u64,
    fractal: Option<(u32, i64)>,
    crossing: Option<(usize, i64)>,
    morph: Option<(usize, Curve)>,
    density: Option<(f64, f64)>,
    layers: Vec<i64>,
    sub_seq: Vec<i64>,
    reflect: Option<(i64, bool, Scale)>,
    accent: Option<(Curve, Curve)>,
    copies: Option<(i64, i64, i64)>,
    lead: Option<bool>,
    new: ArpNew,
    ctx: super::super::Ctx,
    /// With Control Sweeps (E0422): what each level's settings are built
    /// from (a chord takes its first note's).
    swept: Option<std::sync::Arc<Rebuild>>,
}

impl Arp {
    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        match &self.swept {
            Some(r) => {
                let lv: PerLevel<Arp> = r.levels(&self.ctx, src, |p, b| arp_of(p, b).0);
                self.run_at(src, out, |c| lv.at(c[0]))
            }
            // Without sweeps every chord reads `self` (the same code as
            // before E0422).
            None => self.run_at(src, out, |_| self),
        }
    }

    /// The run with each chord's settings `at` (its first note's level's
    /// under Control Sweeps, E0422).
    #[inline]
    fn run_at<'a>(
        &'a self,
        src: &Source,
        out: &mut Output,
        at: impl Fn(&[usize]) -> &'a Arp,
    ) -> Result<(), TransformError> {
        let mark = out.mark();
        let base = out.notes.len();
        // n11: whether each note emitted since `base` lies on an odd step.
        let mut off_steps: Vec<bool> = Vec::new();
        let mut tt = TempoTime::new(&self.ctx);
        for (slot, idx) in per_track(src).iter().enumerate() {
            let mut prev_mean: Option<f64> = None;
            let mut gen_k: u64 = 0;
            let groups = chords(src, idx, self.tol);
            // n01: steps counted across chords, per layer.
            let mut across: Vec<u64> = Vec::new();
            for ci in 0..groups.len() {
                let chord = groups.get(ci);
                // The chord's settings: its first note's level under
                // Control Sweeps (E0422).
                let me = at(chord);
                let mut ladder: Vec<TNote> = chord.iter().map(|&i| src.notes[i]).collect();
                ladder.sort_by_key(|n| (n.pitch, n.sub, n.start));
                let start = ladder.iter().map(|n| n.start).min().unwrap_or(0);
                let end_all = ladder.iter().map(|n| n.end()).max().unwrap_or(start);
                let end_any = ladder.iter().map(|n| n.end()).min().unwrap_or(start);
                // Octave range and range pattern.
                let mut full: Vec<(i64, TNote)> = Vec::new();
                for o in 0..me.range {
                    for n in &ladder {
                        full.push((i64::from(n.pitch) + 12 * o, *n));
                    }
                }
                match me.range_mode {
                    1 => full.reverse(),
                    2 => {
                        let back: Vec<(i64, TNote)> = full
                            .iter()
                            .rev()
                            .skip(1)
                            .take(full.len().saturating_sub(2))
                            .copied()
                            .collect();
                        full.extend(back);
                    }
                    _ => {}
                }
                // Adaptive voice leading: octave shift of the whole ladder.
                if let Some(minimize) = me.lead {
                    let mean = full.iter().map(|x| x.0 as f64).sum::<f64>() / full.len() as f64;
                    if let Some(pm) = prev_mean {
                        let best = (-2..=2i64)
                            .map(|o| (o, (mean + 12.0 * o as f64 - pm).abs()))
                            .min_by(|a, b| {
                                if minimize {
                                    a.1.total_cmp(&b.1)
                                } else {
                                    b.1.total_cmp(&a.1)
                                }
                            })
                            .map_or(0, |x| x.0);
                        for x in &mut full {
                            x.0 += 12 * best;
                        }
                    }
                    prev_mean =
                        Some(full.iter().map(|x| x.0 as f64).sum::<f64>() / full.len() as f64);
                }
                // n05: passing tones between the ladder's neighbours.
                if let Some((lat, most, vel)) = &me.new.fill {
                    full = arp_fill(&full, *lat, *most, *vel);
                }
                let l = full.len();
                let limit = match me.sync {
                    0 => u64::MAX,
                    2 => end_any,
                    _ => end_all,
                };
                let t_norm = pos(&ladder[0], &src.stats);
                let mut step = match me.density {
                    Some((a, b)) => {
                        let d = a + (b - a) * t_norm;
                        ((me.step as f64 / d).round() as u64).max(1)
                    }
                    None => me.step,
                };
                let group = if me.group {
                    (hash(&[slot as u64, start, 0xA7]) as u32) | 1
                } else {
                    0
                };
                // Layers: (steps per cycle) — the first is the pattern itself.
                let base_len = {
                    let mut r = Rng::for_index(me.seed, salt::ORDER, start ^ ((slot as u64) << 48));
                    pattern_seq(me.pattern, l, &me.custom, Some(&mut r))
                        .len()
                        .max(1)
                };
                // n08: whole cycles fill the chord.
                let span_end = if limit == u64::MAX { end_all } else { limit };
                let fit = me.new.fit.map(|(c, rounding, shortest)| {
                    arp_fit(
                        c,
                        rounding,
                        shortest,
                        span_end.saturating_sub(start),
                        base_len,
                        me.ctx.snap,
                    )
                });
                let exact = match fit {
                    Some(ArpFit::Uniform(s)) => {
                        step = s;
                        None
                    }
                    Some(ArpFit::Exact { span, per }) => {
                        step = ((span as f64 / per).round() as u64).max(1);
                        Some((span, per))
                    }
                    None => None,
                };
                let cycle = step.saturating_mul(base_len as u64);
                let layers: Vec<u64> = if me.layers.is_empty() {
                    vec![step]
                } else {
                    std::iter::once(step)
                        .chain(me.layers.iter().map(|&n| (cycle / n.max(1) as u64).max(1)))
                        .collect()
                };
                // The next chord of the track (n06, n10).
                let next = (ci + 1 < groups.len()).then(|| groups.get(ci + 1));
                let next_start = next.map(|g| src.notes[g[0]].start);
                // n10: how far a latched rest reaches (`limit`: no latch).
                let latch_end = match (&me.new.latch, next_start) {
                    (Some((until, longest, below, _)), Some(ns))
                        if limit != u64::MAX && ns > limit && ns - limit < *below =>
                    {
                        match until {
                            0 => ns,
                            1 => {
                                let m = metric_position(&me.ctx.meter, limit.saturating_sub(1));
                                (m.bar_start + m.bar_len).clamp(limit, ns)
                            }
                            _ => limit.saturating_add(*longest).min(ns),
                        }
                    }
                    _ => limit,
                };
                if across.len() < layers.len() {
                    across.resize(layers.len(), 0);
                }
                // n07: anchor notes, emitted after the layer.
                let mut anchors: Vec<(u64, u64)> = Vec::new();
                for (li, &lstep) in layers.iter().enumerate() {
                    let clock = ArpClock {
                        // n08 drives layer 0 in every rounding mode — its
                        // uniform step (nearest grid step, or the shortest
                        // step) too, never n03's real time, so a chord plays
                        // the cycles it asks for (and the preflight's
                        // ⌈1.5·C⌉ patterns hold).
                        rate: if li == 0 && fit.is_some() {
                            None
                        } else {
                            me.new.rate.as_ref()
                        },
                        fit: if li == 0 { exact } else { None },
                        once: me.sync == 0,
                        start,
                        limit,
                        end: latch_end,
                        lstep,
                        ratio: lstep as f64 / step.max(1) as f64,
                        min_gap: me.new.rate.as_ref().map_or(1, |r| {
                            let speed = step.max(1) as f64 / lstep.max(1) as f64;
                            tt.min_ticks_per_step(arp_rate_max_hz(r) * speed)
                        }),
                        base_len: base_len as u64,
                        min_start: src.stats.min_start,
                        span: src.stats.max_start.saturating_sub(src.stats.min_start),
                        at_us: None,
                        t: start,
                        k: 0,
                        done: false,
                    };
                    // n06 needs the number of main (unlatched) steps first.
                    let main = if me.new.glide.is_some() {
                        let mut c = clock;
                        let mut n = 0usize;
                        while let Some(s) = c.next(&mut tt) {
                            if s.latched {
                                break;
                            }
                            n += 1;
                        }
                        n
                    } else {
                        usize::MAX
                    };
                    let mut clock = clock;
                    let mut walk = ArpWalk::new(self, &[slot as u64, start, li as u64], base_len);
                    let glide_from = self
                        .new
                        .glide
                        .as_ref()
                        .map_or(usize::MAX, |g| main.saturating_sub(g.0));
                    let mut glide_p0: Option<i64> = None;
                    let mut tie: Option<u64> = None;
                    let mut last_played: Option<(usize, usize)> = None;
                    // n07: the current re-struck anchor's start; the end of
                    // the main steps so far.
                    let mut anchor_from: Option<u64> = None;
                    let mut main_end = start;
                    let mut next_k: u64 = 0;
                    while let Some(st) = clock.next(&mut tt) {
                        let k = next_k;
                        next_k += 1;
                        let t = st.t;
                        if !st.latched {
                            main_end = t.saturating_add(st.len).min(limit);
                        }
                        let cyc = k / base_len as u64;
                        let mut r = Rng::for_index(
                            me.seed,
                            salt::ORDER,
                            start ^ cyc ^ ((slot as u64) << 48),
                        );
                        let seq = pattern_seq(me.pattern, l, &me.custom, Some(&mut r));
                        let phase = walk.phase(k, seq.len());
                        let mut ix = seq[phase % seq.len()];
                        if let Some((b, c)) = &me.morph {
                            let mut r2 = Rng::for_index(me.seed, salt::SHAPE, start ^ cyc);
                            let sb = pattern_seq(*b, l, &me.custom, Some(&mut r2));
                            let w = c.eval(t_norm);
                            let ib = sb[phase % sb.len()];
                            ix = ((ix as f64) * (1.0 - w) + ib as f64 * w).round() as usize;
                        }
                        let flip = match me.mode {
                            1 => true,
                            2 => cyc % 2 == 1,
                            _ => false,
                        };
                        if flip {
                            ix = l - 1 - ix.min(l - 1);
                        }
                        let (mut p, tpl) = full[ix.min(l - 1)];
                        let mut tpl = tpl;
                        tpl.group = group;
                        if let Some((axis, diatonic, sc)) = &me.reflect {
                            p = if *diatonic {
                                let (da, _) = sc.degree_of(*axis);
                                let (dp, off) = sc.degree_of(p);
                                sc.pitch_of(2 * da - dp) - off
                            } else {
                                2 * axis - p
                            };
                        }
                        if let Some((mode, max)) = me.crossing
                            && let Some(prev) = out
                                .notes
                                .last()
                                .filter(|_| k > 0)
                                .map(|n| i64::from(n.pitch))
                        {
                            if mode == 0 {
                                while p - prev > max {
                                    p -= 12;
                                }
                                while prev - p > max {
                                    p += 12;
                                }
                            } else if (p - prev).abs() < max {
                                p += if k % 2 == 1 { 12 } else { -12 };
                            }
                        }
                        // n06: the last steps glide toward the next chord.
                        if let (Some((_, path, scale, shape)), Some(g)) = (&me.new.glide, next)
                            && !st.latched
                            && (k as usize) >= glide_from
                        {
                            let p0 = *glide_p0.get_or_insert(p);
                            let target = g
                                .iter()
                                .map(|&i| i64::from(src.notes[i].pitch))
                                .min()
                                .unwrap_or(p0);
                            let lat = match path {
                                0 => Lattice::chromatic(),
                                1 => *scale,
                                _ => Lattice::of_pitches(
                                    g.iter().map(|&i| i64::from(src.notes[i].pitch)),
                                ),
                            };
                            let gl = main - glide_from;
                            let j = k as usize - glide_from;
                            let f = shape.eval((j + 1) as f64 / (gl + 1) as f64);
                            let n = lat.steps_between(p0, target);
                            p = lat.step(p0, (n as f64 * f).round() as i64);
                        }
                        if !me.sub_seq.is_empty() {
                            let pos = me.sub_seq[(gen_k % me.sub_seq.len() as u64) as usize];
                            let tr = &src.tracks[slot];
                            if !tr.subs.is_empty() {
                                let (id, _) =
                                    tr.subs[(pos.rem_euclid(tr.subs.len() as i64)) as usize];
                                tpl.sub = id;
                            }
                        }
                        let pv = me.pvel
                            [(k % me.pvel.len().max(1) as u64) as usize % me.pvel.len().max(1)];
                        let mut vel =
                            f64::from(tpl.vel) * (1.0 - me.levels) + pv as f64 * me.levels;
                        let mut gate = me.gate / 100.0;
                        if let Some((cv, cg)) = &me.accent {
                            let ph = phase as f64 / seq.len().max(1) as f64;
                            vel *= cv.eval(ph).max(0.01) * 1.0;
                            gate *= cg.eval(ph).max(0.01);
                        }
                        // n10: latched steps fade.
                        if st.latched
                            && let Some((_, _, _, fade)) = &me.new.latch
                        {
                            let f = (t - limit) as f64 / (latch_end - limit).max(1) as f64;
                            vel *= fade.eval(f).max(0.0);
                        }
                        let end_cap = if st.latched { latch_end } else { limit };
                        let mut dur = ((st.len as f64) * gate).round() as u64;
                        if end_cap != u64::MAX {
                            dur = dur.min(end_cap - t);
                        }
                        let dur = dur.max(1);
                        // n07: re-struck anchors on layer 0's main steps
                        // (each lasts until the next re-strike × Gate).
                        if li == 0
                            && !st.latched
                            && let Some((_, every, _, _)) = me.new.pedal
                            && every > 0
                            && k.is_multiple_of(every)
                        {
                            let previous = anchor_from.replace(t);
                            if let Some(a) = previous {
                                anchors.push((a, me.anchor_len(t - a)));
                            }
                        }
                        // n01: the Euclidean gate.
                        let play = match &me.new.euclid {
                            Some((e, _, run)) => e.hit(if *run { across[li] } else { k }),
                            None => true,
                        };
                        across[li] += 1;
                        if !play {
                            match me.new.euclid.as_ref().map_or(0, |e| e.1) {
                                1 => {
                                    if let Some((a, b)) = last_played {
                                        let want = t.saturating_add(st.len).min(end_cap);
                                        for n in &mut out.notes[a..b] {
                                            if want > n.start {
                                                n.dur = n.dur.max(want - n.start);
                                            }
                                        }
                                    }
                                }
                                2 => {
                                    tie.get_or_insert(t);
                                }
                                _ => {}
                            }
                            gen_k += 1;
                            walk.advance(k);
                            continue;
                        }
                        let (t0, dur, span) = match tie.take() {
                            Some(t0) => (t0, dur + (t - t0), st.len + (t - t0)),
                            None => (t, dur, st.len),
                        };
                        let first = out.notes.len();
                        // V09-0143: the driver dimension is the arpeggio
                        // step (0 = the chord's first step).
                        out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
                        let copies = me.copies.map_or(1, |c| c.0);
                        for c in 0..copies {
                            let (tc, pc) = match me.copies {
                                Some((_, ph, semis)) => (
                                    t0 as i128
                                        + i128::from(c) * i128::from(ph) * i128::from(st.len),
                                    p + c * semis,
                                ),
                                None => (t0 as i128, p),
                            };
                            // n02: the step as a burst of repeats.
                            let reps = me.new.ratchet.as_ref().map_or(1, |(r, amount, _, _)| {
                                let n = r[(k as usize) % r.len()].clamp(1, 16);
                                1 + ((n - 1) as f64 * amount.eval(t_norm).clamp(0.0, 1.0)).round()
                                    as i64
                            });
                            match &me.new.ratchet {
                                Some((_, _, decay, bgate)) if reps > 1 => {
                                    let sub = (span / reps as u64).max(1);
                                    let d = ((sub as f64 * bgate).round() as u64).max(1);
                                    for j in 0..reps {
                                        let v = vel * (1.0 - decay).powi(j as i32);
                                        me.emit_cascade(
                                            out,
                                            tc + i128::from(j) * i128::from(sub),
                                            d,
                                            pc,
                                            v.round() as i64,
                                            &tpl,
                                            0,
                                        );
                                    }
                                }
                                _ => me.emit_cascade(out, tc, dur, pc, vel.round() as i64, &tpl, 0),
                            }
                        }
                        last_played = Some((first, out.notes.len()));
                        if me.new.ceiling.is_some() {
                            off_steps.resize(out.notes.len() - base, k % 2 == 1);
                        }
                        gen_k += 1;
                        walk.advance(k);
                        out.check()?;
                    }
                    // n07: the last re-struck anchor, or one held over the
                    // main steps of layer 0.
                    if li == 0
                        && let Some((_, every, _, _)) = me.new.pedal
                    {
                        if let Some(a) = anchor_from.take() {
                            anchors.push((a, me.anchor_len(main_end.saturating_sub(a))));
                        } else if every == 0 && main_end > start {
                            anchors.push((start, main_end - start));
                        }
                    }
                }
                if let Some((which, _, oct, velf)) = me.new.pedal {
                    let picks: &[usize] = match which {
                        0 => &[0],
                        1 => &[1],
                        _ => &[0, 1],
                    };
                    // The pedal's anchors: dimension 0.
                    out.set_dim(0);
                    for &(t, len) in &anchors {
                        for &w in picks {
                            let n = if w == 0 {
                                ladder[0]
                            } else {
                                ladder[ladder.len() - 1]
                            };
                            let mut tpl = n;
                            tpl.group = group;
                            out.emit(
                                i128::from(t),
                                i128::from(len),
                                i64::from(n.pitch) + 12 * oct,
                                (f64::from(n.vel) * velf).round() as i64,
                                &tpl,
                            );
                        }
                    }
                    if me.new.ceiling.is_some() {
                        off_steps.resize(out.notes.len() - base, false);
                    }
                    out.check()?;
                }
            }
        }
        if let Some((nps, window, how)) = self.new.ceiling {
            off_steps.resize(out.notes.len() - base, false);
            arp_ceiling(out, mark, base, &off_steps, &mut tt, nps, window, how);
        }
        Ok(())
    }

    /// n07: a re-struck anchor's length over `span` ticks (× Gate).
    fn anchor_len(&self, span: u64) -> u64 {
        ((span as f64 * self.gate / 100.0).round() as u64).max(1)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_cascade(
        &self,
        out: &mut Output,
        t: i128,
        dur: u64,
        p: i64,
        vel: i64,
        tpl: &TNote,
        depth: u32,
    ) {
        match self.fractal {
            Some((max_depth, branch)) if depth < max_depth && dur >= branch as u64 => {
                let sub = dur / branch as u64;
                for b in 0..branch {
                    let dir = if b % 2 == 0 { 1 } else { -1 };
                    let pb = p + dir * 12 * ((b + 1) / 2);
                    self.emit_cascade(
                        out,
                        t + i128::from(b) * i128::from(sub),
                        sub,
                        pb,
                        vel,
                        tpl,
                        depth + 1,
                    );
                }
            }
            _ => {
                out.emit(t, i128::from(dur), p, vel, tpl);
            }
        }
    }
}

/// n11: keeps the notes emitted since `mark` (`base` in `out.notes`) under
/// `nps` notes per second in every trailing window of `window_us` (the
/// canonical NPS window, through the tempo map), per track in time order.
/// Strategies 1 / 2 first drop their preferred notes — off-steps, or octave
/// copies of a lower note at the same start — wherever a window holding them
/// is over the ceiling; then any note still over it goes, and strategy 0
/// lengthens the previous kept note over each dropped one.
#[allow(clippy::too_many_arguments)]
fn arp_ceiling(
    out: &mut Output,
    mark: super::super::output::Mark,
    base: usize,
    off_steps: &[bool],
    tt: &mut TempoTime,
    nps: u64,
    window_us: u64,
    how: usize,
) {
    let n = out.notes.len() - base;
    if n == 0 {
        return;
    }
    let cap = ((nps as f64 * window_us as f64 / 1e6).floor() as usize).max(1);
    let notes = &out.notes[base..];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (notes[i].track, notes[i].start, notes[i].pitch, i));
    // Octave copies: a note whose pitch class a lower note at the same start
    // (same track) already has.
    let mut copy = vec![false; n];
    let mut seen = 0u16;
    for (j, &i) in order.iter().enumerate() {
        let same = j > 0 && {
            let p = notes[order[j - 1]];
            (p.track, p.start) == (notes[i].track, notes[i].start)
        };
        if !same {
            seen = 0;
        }
        let pc = 1u16 << (notes[i].pitch % 12);
        copy[i] = seen & pc != 0;
        seen |= pc;
    }
    let times: Vec<u64> = order
        .iter()
        .map(|&i| tt.micros_at(notes[i].start))
        .collect();
    let mut keep = vec![true; n];
    let mut new_end = vec![0u64; n];
    let mut lo = 0;
    while lo < n {
        let track = notes[order[lo]].track;
        let hi = lo
            + order[lo..]
                .iter()
                .take_while(|&&i| notes[i].track == track)
                .count();
        if how != 0 {
            // A note lies in an overloaded window when a window ending at
            // it or within `window_us` after it holds more than `cap`.
            let counts = trailing_counts(&times[lo..hi], window_us);
            let mut next_over = usize::MAX;
            for j in (lo..hi).rev() {
                if counts[j - lo] as usize > cap {
                    next_over = j;
                }
                let i = order[j];
                let preferred = if how == 1 { off_steps[i] } else { copy[i] };
                let loaded = next_over != usize::MAX
                    && times[next_over] < times[j].saturating_add(window_us);
                if preferred && loaded {
                    keep[i] = false;
                }
            }
        }
        let mut w = TrailingWindow::new(window_us);
        let mut last: Option<usize> = None;
        for j in lo..hi {
            let i = order[j];
            if !keep[i] {
                continue;
            }
            if w.count_at(times[j]) >= cap {
                keep[i] = false;
                if how == 0
                    && let Some(k) = last
                {
                    new_end[k] = new_end[k].max(notes[k].end()).max(notes[i].end());
                }
                continue;
            }
            w.push(times[j]);
            last = Some(i);
        }
        lo = hi;
    }
    out.adjust_since(mark, |i, note, _, _| {
        if i >= n {
            return true;
        }
        if !keep[i] {
            return false;
        }
        if new_end[i] > note.end() {
            note.dur = new_end[i] - note.start;
        }
        true
    });
}

/// n03: the fastest rate the settings reach, in Hz.
fn arp_rate_max_hz((ms, from, to, _): &(bool, f64, f64, Curve)) -> f64 {
    if *ms {
        1000.0 / from.min(*to).max(0.1)
    } else {
        from.max(*to)
    }
}

/// The Arpeggiator's settings from `p` and the bound of its output.
fn arp_of(p: &Params, b: &BuildCtx) -> (Arp, u64) {
    let (num, den) = time_mul(p.choice("mul"));
    let step = (note_value(p.choice("step"), p.ticks("step.ticks"), b.ctx) * num / den).max(1);
    let new = ArpNew {
        euclid: p.flag("n01").then(|| {
            (
                Euclid::new(
                    p.int("n01.steps") as u32,
                    p.int("n01.pulses").max(0) as u32,
                    p.int("n01.rot"),
                ),
                p.choice("n01.rest"),
                p.choice("n01.phase") == 1,
            )
        }),
        ratchet: p.flag("n02").then(|| {
            let mut r = p.list("n02.repeats");
            if r.is_empty() {
                r = vec![1];
            }
            (
                r,
                p.curve("n02.amount"),
                p.float("n02.decay") / 100.0,
                p.float("n02.gate") / 100.0,
            )
        }),
        rate: p.flag("n03").then(|| {
            (
                p.choice("n03.unit") == 1,
                p.float("n03.from"),
                p.float("n03.to"),
                p.curve("n03.curve"),
            )
        }),
        stride: p.flag("n04").then(|| {
            (
                p.int("n04.stride"),
                p.int("n04.change"),
                p.choice("n04.ends") == 1,
                p.int("n04.start"),
            )
        }),
        fill: p.flag("n05").then(|| {
            let sc = Scale::new(p.choice("n05.key"), p.choice("n05.scale"));
            let lat = match p.choice("n05.fill") {
                0 => Lattice::of_scale(&sc),
                1 => Lattice::pentatonic_of(&sc),
                _ => Lattice::chromatic(),
            };
            (lat, p.int("n05.most") as usize, p.float("n05.vel") / 100.0)
        }),
        glide: p.flag("n06").then(|| {
            (
                p.int("n06.steps") as usize,
                p.choice("n06.path"),
                Lattice::of_scale(&Scale::new(p.choice("n06.key"), p.choice("n06.scale"))),
                p.curve("n06.shape"),
            )
        }),
        pedal: p.flag("n07").then(|| {
            (
                p.choice("n07.anchor"),
                p.int("n07.every") as u64,
                p.int("n07.oct"),
                p.float("n07.vel") / 100.0,
            )
        }),
        fit: p.flag("n08").then(|| {
            (
                p.float("n08.cycles"),
                p.choice("n08.round"),
                p.ticks("n08.min").max(1),
            )
        }),
        markov: p.flag("n09").then(|| {
            let w = p.list("n09.w");
            let g = |i: usize| w.get(i).copied().unwrap_or(0) as f64;
            (
                [g(0), g(1), g(2), g(3)],
                p.int("n09.leap"),
                p.int("n09.mem") as usize,
            )
        }),
        latch: p.flag("n10").then(|| {
            (
                p.choice("n10.until"),
                p.ticks("n10.fill"),
                p.ticks("n10.below"),
                p.curve("n10.fade"),
            )
        }),
        ceiling: p.flag("n11").then(|| {
            (
                p.int("n11.nps").max(1) as u64,
                (p.float("n11.window") * 1000.0).round() as u64,
                p.choice("n11.how"),
            )
        }),
    };
    let a = Arp {
        pattern: p.choice("pattern"),
        custom: p.list("custom"),
        pvel: {
            let v = p.list("pvel");
            if v.is_empty() { vec![100] } else { v }
        },
        mode: p.choice("mode"),
        step,
        range: p.int("range"),
        range_mode: p.choice("range.mode"),
        sync: p.choice("sync"),
        gate: p.float("gate"),
        levels: p.float("levels"),
        tol: p.ticks("tol"),
        group: p.flag("group"),
        seed: p.seed("seed"),
        fractal: p
            .adv(1)
            .then(|| (p.int("a1.depth") as u32, p.int("a1.branch"))),
        crossing: p.adv(2).then(|| (p.choice("a2.mode"), p.int("a2.max"))),
        morph: p.adv(3).then(|| (p.choice("a3.b"), p.curve("a3.curve"))),
        density: p.adv(4).then(|| (p.float("a4.from"), p.float("a4.to"))),
        layers: if p.adv(5) {
            p.list("a5.layers")
        } else {
            Vec::new()
        },
        sub_seq: if p.adv(6) {
            p.list("a6.seq")
        } else {
            Vec::new()
        },
        reflect: p.adv(7).then(|| {
            (
                p.int("a7.axis"),
                p.choice("a7.kind") == 1,
                Scale::new(p.choice("a7.key"), p.choice("a7.scale")),
            )
        }),
        accent: p.adv(8).then(|| (p.curve("a8.vel"), p.curve("a8.gate"))),
        copies: p
            .adv(9)
            .then(|| (p.int("a9.copies"), p.int("a9.phase"), p.int("a9.semis"))),
        lead: p.adv(10).then(|| p.choice("a10.goal") == 0),
        new,
        ctx: b.ctx.clone(),
        swept: None,
    };
    // Bound: Σ steps over every note (a chord is no longer than its longest
    // note) × layers × copies × cascade.
    let mut fastest = match a.density {
        Some((x, y)) => (step as f64 / x.max(y)).max(1.0) as u64,
        None => step,
    };
    // n03: real-time steps are never closer than the fastest rate at the
    // slowest tempo allows; n08: never shorter than its shortest step
    // (whole cycles may be shorter with "at least one cycle": below).
    if let Some(r) = &a.new.rate {
        let hz = arp_rate_max_hz(r);
        let layer_speedup = a
            .layers
            .iter()
            .map(|&n| n.max(1) as f64)
            .fold(1.0, f64::max);
        let tt = TempoTime::new(b.ctx);
        fastest = fastest.min(tt.min_ticks_per_step(hz * layer_speedup));
    }
    if let Some((_, rounding, shortest)) = a.new.fit {
        fastest = fastest.min(if rounding == 1 {
            shortest.max(1).min(b.ctx.snap.max(1))
        } else {
            shortest.max(1)
        });
    }
    // n05: a ladder grows by up to `most` notes per gap.
    let fill = a.new.fill.as_ref().map_or(1, |f| f.1 as u64 + 1);
    // Patterns per chord (sync "Time", n08): a pattern walks the ladder
    // (chord notes × octaves × n05's fill, about twice with the up-and-down
    // range) once, or twice for the back-and-forth patterns (Up-Down,
    // Down-Up, Thumb, Pinky), or the custom list; summed over the chords.
    let back_and_forth = if matches!(a.pattern, 2 | 3 | 6 | 7) {
        2
    } else {
        1
    };
    let range_mul = if a.range_mode == 2 { 2 } else { 1 };
    let per_note = (a.range.max(1) as u64)
        .saturating_mul(fill)
        .saturating_mul(back_and_forth * range_mul);
    let patterns = (b.stats.count as u64)
        .saturating_mul(per_note + a.custom.len() as u64 + 1)
        .saturating_mul(1 + a.layers.len() as u64);
    // A chord's notes may start up to `tol` after its first note.
    let mut total = b
        .stats
        .steps_bound(fastest)
        .saturating_add((b.stats.count as u64).saturating_mul(a.tol / fastest.max(1) + 1));
    // n08: a chord plays at most ⌈1.5 · cycles⌉ patterns (the nearest grid
    // step may be up to a third shorter than the exact one), plus one step.
    if let Some((c, _, _)) = a.new.fit {
        let fit = patterns
            .saturating_mul((1.5 * c.max(1.0)).ceil() as u64)
            .saturating_add(b.stats.count as u64);
        total = total.min(fit);
    }
    for &n in &a.layers {
        total = total.saturating_add(
            b.stats
                .steps_bound((fastest.min(step).saturating_mul(64) / n.max(1) as u64).max(1))
                .saturating_mul(64),
        );
    }
    if a.sync == 0 {
        // No sync: every chord plays its pattern once per layer, whatever its
        // length — a pattern is at most 2 × the ladder (chord notes × octaves,
        // twice with the up-and-down range), or the custom pattern's length.
        total = patterns;
    }
    if matches!(a.new.fit, Some((_, 2, _))) {
        total = total.saturating_add(patterns);
    }
    // n10: a latched rest is shorter than its threshold and lies between
    // two chords of one track (rests of a track never overlap).
    if let Some((_, _, below, _)) = a.new.latch {
        let span = b.stats.max_start.saturating_sub(b.stats.min_start);
        let per_rest = (b.stats.count as u64).saturating_mul(below / fastest.max(1) + 1);
        let per_track = (b.tracks.len().max(1) as u64)
            .saturating_mul(span / fastest.max(1) + 1)
            .saturating_add(b.stats.count as u64);
        total = total.saturating_add(
            per_rest
                .min(per_track)
                .saturating_mul(1 + a.layers.len() as u64),
        );
    }
    // n07: one anchor (two with "Both") per re-strike, or per chord when
    // held.
    let anchors = a.new.pedal.map_or(0u64, |(w, every, _, _)| {
        let picks = if w == 2 { 2 } else { 1 };
        let count = b.stats.count as u64;
        let per = total
            .checked_div(every)
            .map_or(count, |q| q.saturating_add(count));
        per.saturating_mul(picks)
    });
    // n02: steps cycle through the repeat list, so a chord's steps burst
    // into at most the list's average per step plus one list's worth.
    if let Some((r, _, _, _)) = &a.new.ratchet {
        let sum: u64 = r.iter().map(|&x| x.clamp(1, 16) as u64).sum();
        let len = r.len().max(1) as u64;
        total = total
            .saturating_mul(sum)
            .div_ceil(len)
            .saturating_add((b.stats.count as u64).saturating_mul(sum));
    }
    if let Some((c, _, _)) = a.copies {
        total = total.saturating_mul(c as u64);
    }
    if let Some((d, br)) = a.fractal {
        total = total.saturating_mul((br as u64).pow(d));
    }
    let total = total.saturating_add(anchors);
    (a, total)
}

fn arp_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let (mut a, mut total) = arp_of(p, b);
    // With Control Sweeps (E0422) each chord takes its level's settings;
    // the bound is the largest over the swept ranges' corners.
    if let Some(rb) = Rebuild::of(p, b) {
        for (_, t) in rb.corners(b.ctx, arp_of) {
            total = total.max(t);
        }
        a.swept = Some(rb);
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| a.run(src, out),
        growth: Growth::Generate(total),
        estimate: Some(total),
    })
}

// ================================================================ Articulate

pub const ARTICULATE: ToolDef = ToolDef {
    id: "articulate",
    spec: art_spec,
    build: art_build,
};

const STYLES: &[&str] = &["Legato", "Portato", "Staccato", "Custom"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const ARTICULATE_HELP: &[(&str, &str)] = &[
    (
        "style",
        "How long each note lasts up to the next onset: Legato fills the whole distance, Portato 80 % with a small gap, Staccato 45 %, Custom the Multiply percentage.",
    ),
    (
        "multiply",
        "For Custom: each note's length as a share of the distance to the next onset (or of its own length with Use Lengths).",
    ),
    (
        "gap",
        "Leaves a small gap (1/32 of the distance, at least one tick) wherever a note would reach the next onset.",
    ),
    (
        "use_lengths",
        "Scales each note's own length instead of measuring to the next onset.",
    ),
    (
        "chop",
        "Also cuts any note that is still sounding where a later note starts.",
    ),
    (
        "just_chop",
        "Only cuts notes where a later note starts; every other length stays as it is.",
    ),
    (
        "only_sel",
        "Measures to the next selected note only; off, unselected notes of the track count as next onsets too.",
    ),
    (
        "variation",
        "Lengthens or shortens each note at random by up to this share (seeded).",
    ),
    (
        "a1",
        "Keeps a gap before each next onset, its size following a curve across the selection.",
    ),
    (
        "a1.curve",
        "The gap from the selection's start (left) to its end (right), as a share of Largest Gap.",
    ),
    (
        "a1.max",
        "The gap, in ticks, where the curve is at its top.",
    ),
    (
        "a2",
        "Makes the length percentage travel from a start value to an end value across the selection.",
    ),
    ("a2.from", "The length percentage at the selection's start."),
    ("a2.to", "The length percentage at the selection's end."),
    (
        "a2.shape",
        "How the percentage travels: 1 = evenly, above 1 = slowly at first, below 1 = quickly at first.",
    ),
    (
        "a3",
        "Makes louder notes longer and softer ones shorter, or the reverse.",
    ),
    (
        "a3.amount",
        "+ makes loud notes longer and soft ones shorter; − the reverse; 0 = no effect.",
    ),
    (
        "a4",
        "Changes the length by register: one percentage at a low key, another at a high key, blended between.",
    ),
    (
        "a4.lo",
        "The low key where Length at the Low Pivot applies (and below).",
    ),
    (
        "a4.hi",
        "The high key where Length at the High Pivot applies (and above).",
    ),
    ("a4.low", "The length percentage at the low pivot."),
    ("a4.high", "The length percentage at the high pivot."),
    (
        "a5",
        "Gives every note of a chord the length worked out for its lowest note, so chords stay together.",
    ),
    (
        "a6",
        "Steps through a list of styles, one onset after another.",
    ),
    (
        "a6.seq",
        "The style of each onset in turn: 0 legato, 1 portato, 2 staccato; the list repeats.",
    ),
    (
        "a7",
        "Varies lengths with smooth seeded noise, so neighboring notes vary alike.",
    ),
    (
        "a7.amount",
        "The largest lengthening or shortening, as a share of the length.",
    ),
    (
        "a7.len",
        "How many ticks the variation takes to change course (longer = slower, smoother changes).",
    ),
    (
        "a8",
        "Spreads the lengths by rank: the shortest note becomes Shortest, the longest becomes Longest, the rest between in their order.",
    ),
    (
        "a8.min",
        "The length, in ticks, given to the shortest note.",
    ),
    ("a8.max", "The length, in ticks, given to the longest note."),
    (
        "a8.shape",
        "How the lengths are spread between Shortest and Longest (1 = evenly by rank).",
    ),
    (
        "a9",
        "Copies a phrase's articulation: one length percentage per onset in turn.",
    ),
    (
        "a9.ratios",
        "The length of each onset in turn, as a percentage of the distance to the next onset; the list repeats.",
    ),
    (
        "a10",
        "Makes every note end a fixed number of ticks after (overlap) or before (gap) the next onset.",
    ),
    (
        "a10.ticks",
        "+ overlaps the next onset by this many ticks; − ends this many ticks before it.",
    ),
    (
        "n01",
        "Ends each note a fixed time in milliseconds before the next onset, whatever the tempo.",
    ),
    (
        "n01.gap",
        "The silence, in milliseconds, before the next onset.",
    ),
    (
        "n01.short",
        "No note is made shorter than this, in milliseconds.",
    ),
    (
        "n01.to",
        "Measure to the next onset of the whole track, or the next note on the same key.",
    ),
    (
        "n02",
        "Sets the length by the note's place in the bar, one gate per grid position.",
    ),
    ("n02.grid", "The grid the bar is divided into."),
    (
        "n02.gates",
        "The gate of each grid position from the bar line, in percent; the list repeats.",
    ),
    (
        "n02.between",
        "Notes between grid positions take the nearest position's gate, or a blend of the two.",
    ),
    (
        "n03",
        "Lengthens the last note of each phrase, and eases the notes leading up to it.",
    ),
    (
        "n03.rest",
        "A silence at least this long, in ticks, ends a phrase.",
    ),
    (
        "n03.split",
        "Phrases end at rests only, or at rests and bar lines.",
    ),
    (
        "n03.last",
        "The phrase's last note's length as a percentage of its own (never past the next phrase's start).",
    ),
    (
        "n03.approach",
        "How many onsets before the last one lengthen gradually toward it.",
    ),
    (
        "n03.shape",
        "How the approach notes grow toward the last note's length.",
    ),
    (
        "n04",
        "Shortens notes where many notes start close together, and lengthens them where few do.",
    ),
    (
        "n04.window",
        "The time, in ticks, over which recent note starts are counted.",
    ),
    (
        "n04.sparse",
        "The gate where the count is at or below Sparse At.",
    ),
    (
        "n04.dense",
        "The gate where the count is at or above Dense At.",
    ),
    (
        "n04.sparse_at",
        "How many starts in the window count as sparse.",
    ),
    (
        "n04.dense_at",
        "How many starts in the window count as dense.",
    ),
    (
        "n05",
        "Connects small steps legato and shortens notes before larger leaps.",
    ),
    (
        "n05.within",
        "Steps up to this many semitones to the next note are played legato (connected).",
    ),
    (
        "n05.curve",
        "The gate multiplier for larger intervals, from no leap (left) to Largest Interval (right).",
    ),
    (
        "n05.largest",
        "The interval, in semitones, at the right end of the curve; larger leaps count as this.",
    ),
    (
        "n05.next",
        "Which next note the interval is measured to: the lowest note of the next onset, or the nearest key among them.",
    ),
    (
        "n06",
        "Sets the length by the tempo at each note: one gate at a slow tempo, another at a fast one.",
    ),
    (
        "n06.slow",
        "The tempo at and below which Gate at Slow applies.",
    ),
    (
        "n06.fast",
        "The tempo at and above which Gate at Fast applies.",
    ),
    ("n06.at_slow", "The gate at the slow tempo."),
    ("n06.at_fast", "The gate at the fast tempo."),
    (
        "n06.shape",
        "How the gate moves from the slow value to the fast one as the tempo rises.",
    ),
    (
        "n07",
        "Snaps every gate to one of a few allowed ratios, for uniform articulation.",
    ),
    (
        "n07.ratios",
        "The allowed gates, in percent of the distance to the next onset.",
    ),
    (
        "n07.round",
        "Snap to the nearest allowed ratio, the next lower, or the next higher.",
    ),
    (
        "n07.strength",
        "How far each gate moves toward its snapped ratio (100 % = exactly onto it).",
    ),
    (
        "n08",
        "Sets the length by each note's voice in its chord: bass, inner voices and top voice apart.",
    ),
    (
        "n08.ranks",
        "The gate of each voice in turn, in percent, counted from the bottom or the top; the last entry covers further voices.",
    ),
    ("n08.top", "The gate of the chord's top voice."),
    (
        "n08.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n08.from",
        "Count the voices from the bottom (the lowest note first) or from the top (the note under the top voice first).",
    ),
    (
        "n09",
        "Changes the length each time the same key repeats, for a fading or growing repetition.",
    ),
    (
        "n09.change",
        "How much each repeat's length changes from the one before (− shortens, + lengthens).",
    ),
    (
        "n09.reset",
        "A rest at least this long, in ticks, on that key starts the count again.",
    ),
    (
        "n09.floor",
        "The shortest a repeat can become, as a share of its length (the longest is the inverse).",
    ),
    (
        "n09.key",
        "What counts as the same note: the key, the key on the same channel, or the key on the same subchannel.",
    ),
    (
        "n10",
        "Copies the articulation of a reference: each note takes the gate of the nearest reference onset.",
    ),
    (
        "n10.ref",
        "Where the reference notes come from: the track's unselected notes, a subchannel of the track, or another track.",
    ),
    (
        "n10.num",
        "The reference's number: a subchannel number for A Subchannel, a track number for A Track.",
    ),
    (
        "n10.window",
        "A reference onset at most this many ticks away from a note is matched to it.",
    ),
    (
        "n10.blend",
        "How far each note's gate moves toward the reference's (100 % = the same).",
    ),
    (
        "n11",
        "Shortens notes so that no more than a set number sound at once on a track.",
    ),
    (
        "n11.most",
        "The most notes that may sound at the same moment on one track.",
    ),
    (
        "n11.first",
        "Which notes are shortened first: the longest, the oldest, or the quietest.",
    ),
    (
        "n11.short",
        "No note is shortened below this length, in ticks.",
    ),
];

fn art_spec() -> ToolSpec {
    let mut p = vec![
        choice("style", "Options", STYLES, 0),
        float("multiply", "Multiply (Custom)", 10.0, 100.0, 100.0, "%"),
        flag("gap", "Ensure small gap between notes", false),
        flag("use_lengths", "Use lengths", false),
        flag("chop", "Chop chords", false),
        flag("just_chop", "Just chop chords", false),
        flag("only_sel", "Only with selection", true),
        float("variation", "Variation", 0.0, 100.0, 0.0, "%"),
        seed("seed"),
        adv_switch(1, "Gap envelope"),
        curve(
            "a1.curve",
            "Gap across the selection",
            Curve::line(0.0, 1.0),
        )
        .overshoot()
        .adv(1),
        ticks("a1.max", "Largest gap", 1, 1 << 40, 240).adv(1),
        adv_switch(2, "Gate curve (start % → end %)"),
        float("a2.from", "Start", 10.0, 200.0, 100.0, "%").adv(2),
        float("a2.to", "End", 10.0, 200.0, 40.0, "%").adv(2),
        float("a2.shape", "Curvature", 0.2, 5.0, 1.0, "").adv(2),
        adv_switch(3, "Velocity-aware response"),
        float(
            "a3.amount",
            "Louder → longer (+) / shorter (−)",
            -1.0,
            1.0,
            0.5,
            "",
        )
        .adv(3),
        adv_switch(4, "Register-dependent"),
        pitch("a4.lo", "Low pivot", 36).adv(4),
        pitch("a4.hi", "High pivot", 96).adv(4),
        float("a4.low", "Length at the low pivot", 10.0, 200.0, 100.0, "%").adv(4),
        float(
            "a4.high",
            "Length at the high pivot",
            10.0,
            200.0,
            50.0,
            "%",
        )
        .adv(4),
        adv_switch(5, "Chord-group articulation"),
        adv_switch(6, "Alternating pattern"),
        list(
            "a6.seq",
            "Sequence (0 legato, 1 portato, 2 staccato)",
            0,
            2,
            64,
            &[0, 2, 2],
        )
        .adv(6),
        adv_switch(7, "Correlated micro-gate variation"),
        float("a7.amount", "Amount", 0.0, 100.0, 20.0, "%").adv(7),
        ticks("a7.len", "Correlation length", 1, 1 << 40, 960).adv(7),
        adv_switch(8, "Duration quantile mapping"),
        ticks("a8.min", "Shortest", 1, 1 << 40, 60).adv(8),
        ticks("a8.max", "Longest", 1, 1 << 40, 960).adv(8),
        float("a8.shape", "Curvature", 0.2, 5.0, 1.0, "").adv(8),
        adv_switch(9, "Articulation transfer"),
        list(
            "a9.ratios",
            "Gate % per onset (from a phrase)",
            1,
            400,
            4096,
            &[100, 50, 50, 100],
        )
        .adv(9),
        adv_switch(10, "Overlap sculptor"),
        int(
            "a10.ticks",
            "Overlap (+) / gap (−) ticks",
            -1_000_000,
            1_000_000,
            30,
        )
        .adv(10),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Real-time gap", false),
        float("n01.gap", "Gap", 0.0, 1000.0, 30.0, "ms"),
        float("n01.short", "Shortest length", 0.0, 1000.0, 20.0, "ms"),
        choice("n01.to", "Measure to", ART_GAP_TO, 0),
        flag("n02", "Metric-position gates", false),
        choice("n02.grid", "Grid", art_grid_values(), 2),
        list(
            "n02.gates",
            "Gate per position",
            1,
            400,
            64,
            &[100, 60, 80, 60],
        ),
        choice("n02.between", "Between positions", ART_BETWEEN, 0),
        flag("n03", "Phrase-end lengthening", false),
        ticks("n03.rest", "Phrase split rest", 0, 1 << 40, 480),
        choice("n03.split", "Split at", ART_SPLIT, 0),
        float("n03.last", "Last-note length", 50.0, 800.0, 200.0, "%"),
        int("n03.approach", "Approach notes", 0, 16, 2),
        curve("n03.shape", "Approach shape", Curve::line(0.0, 1.0)).overshoot(),
        flag("n04", "Density-adaptive gate", false),
        ticks("n04.window", "Window", 1, 1 << 40, 960),
        float("n04.sparse", "Sparse gate", 10.0, 200.0, 120.0, "%"),
        float("n04.dense", "Dense gate", 10.0, 200.0, 50.0, "%"),
        int(
            "n04.sparse_at",
            "Sparse at (notes per window)",
            1,
            100_000,
            2,
        ),
        int(
            "n04.dense_at",
            "Dense at (notes per window)",
            1,
            100_000,
            16,
        ),
        flag("n05", "Interval-dependent legato", false),
        int("n05.within", "Legato within (semitones)", 0, 24, 2),
        curve("n05.curve", "Gate over interval", Curve::line(1.0, 0.3)).overshoot(),
        int("n05.largest", "Largest interval", 1, 127, 12),
        choice("n05.next", "Next note", ART_NEXT_NOTE, 0),
        flag("n06", "Tempo-following gate", false),
        float("n06.slow", "Slow pivot", 1.0, 10_000.0, 90.0, "BPM"),
        float("n06.fast", "Fast pivot", 1.0, 10_000.0, 240.0, "BPM"),
        float("n06.at_slow", "Gate at slow", 10.0, 200.0, 100.0, "%"),
        float("n06.at_fast", "Gate at fast", 10.0, 200.0, 50.0, "%"),
        curve("n06.shape", "Shape", Curve::line(0.0, 1.0)).overshoot(),
        flag("n07", "Gate ratio palette", false),
        list("n07.ratios", "Allowed ratios", 1, 400, 32, &[25, 50, 100]),
        choice("n07.round", "Rounding", ART_ROUNDING, 0),
        float("n07.strength", "Strength", 0.0, 100.0, 100.0, "%"),
        flag("n08", "Voice-rank articulation", false),
        list("n08.ranks", "Gate by rank", 1, 400, 32, &[150, 60]),
        float("n08.top", "Top voice gate", 1.0, 400.0, 100.0, "%"),
        ticks("n08.tol", "Chord tolerance", 0, 1 << 30, 0),
        choice("n08.from", "Ranks count from", ART_RANK_FROM, 0),
        flag("n09", "Repetition decay", false),
        float("n09.change", "Change per repeat", -50.0, 50.0, -10.0, "%"),
        ticks("n09.reset", "Reset after rest", 0, 1 << 40, 480),
        float("n09.floor", "Floor", 1.0, 100.0, 25.0, "%"),
        choice("n09.key", "Same key means", ART_SAME_KEY, 0),
        flag("n10", "Reference articulation match", false),
        choice("n10.ref", "Reference", ART_REFERENCE, 0),
        int(
            "n10.num",
            "Reference subchannel or track (number)",
            1,
            4096,
            1,
        ),
        ticks("n10.window", "Match window", 0, 1 << 40, 240),
        float("n10.blend", "Blend", 0.0, 100.0, 100.0, "%"),
        flag("n11", "Polyphony ceiling trim", false),
        int("n11.most", "Most sounding notes", 1, 1_000_000, 16),
        choice("n11.first", "Trim first", ART_TRIM_FIRST, 0),
        ticks("n11.short", "Shortest length", 1, 1 << 40, 30),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, ARTICULATE_HELP);
    ToolSpec {
        id: "articulate",
        name: "Articulate",
        family: Family::Fl,
        summary: "Sets note lengths relative to the next onset: legato, portato, staccato, or custom multiply, gaps and chopped chords.",
        params: p,
        advanced: vec![
            "Gap envelope",
            "Gate curve (start % → end %)",
            "Velocity-aware response",
            "Register-dependent",
            "Chord-group articulation",
            "Alternating pattern",
            "Correlated micro-gate variation",
            "Duration quantile mapping",
            "Articulation transfer",
            "Overlap sculptor",
        ],
        functions: [
            ("n01", "Real-time gap", 110),
            ("n02", "Metric-position gates", 120),
            ("n03", "Phrase-end lengthening", 130),
            ("n04", "Density-adaptive gate", 140),
            ("n05", "Interval-dependent legato", 150),
            ("n06", "Tempo-following gate", 160),
            ("n07", "Gate ratio palette", 170),
            ("n08", "Voice-rank articulation", 180),
            ("n09", "Repetition decay", 190),
            ("n10", "Reference articulation match", 200),
            ("n11", "Polyphony ceiling trim", 700),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: true,
    }
}

const ART_GAP_TO: &[&str] = &["Next onset of the track", "Next onset of the same key"];
const ART_BETWEEN: &[&str] = &["Nearest position", "Interpolate"];
const ART_SPLIT: &[&str] = &["Rests only", "Rests and bar lines"];
const ART_NEXT_NOTE: &[&str] = &["Next onset", "Nearest pitch among the next onsets"];
const ART_ROUNDING: &[&str] = &["Nearest", "Down", "Up"];
const ART_RANK_FROM: &[&str] = &["Bottom", "Top"];
const ART_SAME_KEY: &[&str] = &["Key", "Key and channel", "Key and subchannel"];
const ART_REFERENCE: &[&str] = &["Unselected notes of the track", "A subchannel", "A track"];
const ART_TRIM_FIRST: &[&str] = &["Longest", "Oldest", "Quietest"];

/// n02's grid: the musical note values (no custom ticks).
fn style_mult(style: usize, custom: f64) -> (f64, bool) {
    match style {
        0 => (1.0, false),
        1 => (0.8, true),
        2 => (0.45, false),
        _ => (custom / 100.0, false),
    }
}

/// Articulate's v0.9 functions declared by id (TOOL_CATALOG `articulate`
/// n01 … n11). `None` = switched off; with every one off the kernel is the
/// v0.8 one.
#[derive(Clone, Default)]
struct ArtNew {
    /// n01: gap and shortest length in µs; measured to the same key.
    gap: Option<(u64, u64, bool)>,
    /// n02: grid ticks, gate per position (fractions), interpolate.
    metric: Option<(u64, Vec<f64>, bool)>,
    /// n03: split rest, split at bar lines too, last-note factor, approach
    /// onsets, approach shape.
    phrase: Option<(u64, bool, f64, usize, Curve)>,
    /// n04: window ticks, sparse / dense gate (fractions), sparse / dense
    /// notes per window.
    density: Option<(u64, f64, f64, f64, f64)>,
    /// n05: legato within (semitones), gate over the interval, largest
    /// interval, next note = nearest pitch of the next onset.
    interval: Option<(u32, Curve, u32, bool)>,
    /// n06: slow / fast pivot (BPM), gate at slow / fast (fractions), shape.
    tempo: Option<(f64, f64, f64, f64, Curve)>,
    /// n07: allowed ratios (fractions, ascending, distinct), rounding,
    /// strength (fraction).
    palette: Option<(Vec<f64>, usize, f64)>,
    /// n08: gate by rank (fractions), top voice gate (fraction), chord
    /// tolerance, ranks counted from the top.
    rank: Option<(Vec<f64>, f64, u64, bool)>,
    /// n09: change per repeat (fraction), reset rest, floor (fraction),
    /// same-key mode (0 key, 1 + channel, 2 + subchannel).
    repeat: Option<(f64, u64, f64, usize)>,
    /// n10: reference (0 unselected notes, 1 subchannel, 2 track), its
    /// number (1-based), match window, blend (fraction).
    reference: Option<(usize, usize, u64, f64)>,
    /// n11: most sounding notes, trim first (0 longest, 1 oldest, 2
    /// quietest), shortest length.
    ceiling: Option<(usize, usize, u64)>,
}

impl ArtNew {
    fn from(p: &Params, ctx: &super::super::Ctx) -> ArtNew {
        let pct = |k: &str| p.float(k) / 100.0;
        let pcts = |k: &str| -> Vec<f64> { p.list(k).iter().map(|&v| v as f64 / 100.0).collect() };
        let us = |k: &str| (p.float(k).max(0.0) * 1000.0).round() as u64;
        ArtNew {
            gap: p
                .flag("n01")
                .then(|| (us("n01.gap"), us("n01.short"), p.choice("n01.to") == 1)),
            metric: p.flag("n02").then(|| {
                let grid = p.choice("n02.grid").min(art_grid_values().len() - 1);
                let mut gates = pcts("n02.gates");
                if gates.is_empty() {
                    gates.push(1.0);
                }
                (
                    note_value(grid, 1, ctx).max(1),
                    gates,
                    p.choice("n02.between") == 1,
                )
            }),
            phrase: p.flag("n03").then(|| {
                (
                    p.ticks("n03.rest"),
                    p.choice("n03.split") == 1,
                    pct("n03.last"),
                    p.int("n03.approach").max(0) as usize,
                    p.curve("n03.shape"),
                )
            }),
            density: p.flag("n04").then(|| {
                (
                    p.ticks("n04.window").max(1),
                    pct("n04.sparse"),
                    pct("n04.dense"),
                    p.int("n04.sparse_at") as f64,
                    p.int("n04.dense_at") as f64,
                )
            }),
            interval: p.flag("n05").then(|| {
                (
                    p.int("n05.within").max(0) as u32,
                    p.curve("n05.curve"),
                    p.int("n05.largest").max(1) as u32,
                    p.choice("n05.next") == 1,
                )
            }),
            tempo: p.flag("n06").then(|| {
                (
                    p.float("n06.slow"),
                    p.float("n06.fast"),
                    pct("n06.at_slow"),
                    pct("n06.at_fast"),
                    p.curve("n06.shape"),
                )
            }),
            palette: p.flag("n07").then(|| {
                let mut r = pcts("n07.ratios");
                r.sort_by(f64::total_cmp);
                r.dedup();
                if r.is_empty() {
                    r.push(1.0);
                }
                (r, p.choice("n07.round"), pct("n07.strength"))
            }),
            rank: p.flag("n08").then(|| {
                let mut r = pcts("n08.ranks");
                if r.is_empty() {
                    r.push(1.0);
                }
                (
                    r,
                    pct("n08.top"),
                    p.ticks("n08.tol"),
                    p.choice("n08.from") == 1,
                )
            }),
            repeat: p.flag("n09").then(|| {
                (
                    pct("n09.change"),
                    p.ticks("n09.reset"),
                    pct("n09.floor").clamp(0.01, 1.0),
                    p.choice("n09.key"),
                )
            }),
            reference: p.flag("n10").then(|| {
                (
                    p.choice("n10.ref"),
                    p.int("n10.num").max(1) as usize,
                    p.ticks("n10.window"),
                    pct("n10.blend"),
                )
            }),
            ceiling: p.flag("n11").then(|| {
                (
                    p.int("n11.most").max(1) as usize,
                    p.choice("n11.first"),
                    p.ticks("n11.short").max(1),
                )
            }),
        }
    }

    /// Whether any per-note function (n01 – n10) is on.
    fn per_note(&self) -> bool {
        self.gap.is_some()
            || self.metric.is_some()
            || self.phrase.is_some()
            || self.density.is_some()
            || self.interval.is_some()
            || self.tempo.is_some()
            || self.palette.is_some()
            || self.rank.is_some()
            || self.repeat.is_some()
            || self.reference.is_some()
    }

    /// Whether the run reads the tempo map or the meter.
    fn needs_ctx(&self) -> bool {
        self.gap.is_some()
            || self.metric.is_some()
            || self.tempo.is_some()
            || self.phrase.as_ref().is_some_and(|p| p.1)
    }
}

/// n05's verdict for one note.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ArtStep {
    /// No next onset: unchanged.
    Keep,
    /// Within the legato interval: connected to the next onset.
    Legato,
    /// A leap: the gate multiplied by the curve's value.
    Gate(f64),
}

/// The per-note values of the functions switched on, computed once per run
/// from the source (an empty table = that function off).
#[derive(Default)]
struct ArtTables {
    metric: Vec<f64>,
    density: Vec<f64>,
    interval: Vec<ArtStep>,
    tempo: Vec<f64>,
    rank: Vec<f64>,
    /// n03: (factor, longest length the lengthening may reach).
    phrase: Vec<(f64, u64)>,
    repeat: Vec<f64>,
    /// n10: the matched reference ratio (NaN = none within the window).
    reference: Vec<f64>,
    /// n01: the longest length the real-time gap allows (`u64::MAX` = no
    /// next onset).
    gap: Vec<u64>,
}

/// `(v − a) / (b − a)` clamped to 0 … 1 (a step at `b` when `a = b`).
fn art_ramp(v: f64, a: f64, b: f64) -> f64 {
    if a == b {
        if v >= b { 1.0 } else { 0.0 }
    } else {
        ((v - a) / (b - a)).clamp(0.0, 1.0)
    }
}

/// n07: the allowed ratio `r` snaps to (`ratios` ascending, not empty).
fn art_snap(ratios: &[f64], round: usize, r: f64) -> f64 {
    let k = ratios.partition_point(|&a| a < r);
    let up = ratios.get(k).copied();
    let down = if ratios.get(k) == Some(&r) {
        Some(r)
    } else {
        k.checked_sub(1).map(|j| ratios[j])
    };
    match (round, down, up) {
        (1, Some(d), _) | (2, Some(d), None) => d,
        (2, _, Some(u)) | (1, None, Some(u)) => u,
        (_, Some(d), Some(u)) => {
            if r - d <= u - r {
                d
            } else {
                u
            }
        }
        (_, Some(d), None) => d,
        (_, None, Some(u)) => u,
        (_, None, None) => r,
    }
}

/// n10: a reference's onsets (notes sorted by start) with their gate ratio —
/// the onset's longest note over the distance to the next reference onset
/// (the last onset has none and is left out).
fn art_ref_onsets(refn: &[&TNote]) -> Vec<(u64, f64)> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < refn.len() {
        let s = refn[k].start;
        let mut j = k;
        let mut longest = 0u64;
        while j < refn.len() && refn[j].start == s {
            longest = longest.max(refn[j].dur);
            j += 1;
        }
        if let Some(nx) = refn.get(j) {
            out.push((s, longest as f64 / (nx.start - s) as f64));
        }
        k = j;
    }
    out
}

/// n10: the ratio of the reference onset nearest to `t` within `window`
/// (the earlier one on a tie), NaN when none.
fn art_nearest(ons: &[(u64, f64)], t: u64, window: u64) -> f64 {
    let p = ons.partition_point(|o| o.0 < t);
    let mut best: Option<(u64, f64)> = None;
    for c in [p.checked_sub(1), Some(p)].into_iter().flatten() {
        if let Some(&(s, r)) = ons.get(c) {
            let d = s.abs_diff(t);
            if d <= window && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, r));
            }
        }
    }
    best.map_or(f64::NAN, |b| b.1)
}

impl ArtTables {
    #[allow(clippy::too_many_lines)]
    fn of<'n>(
        src: &Source,
        ctx: Option<&super::super::Ctx>,
        new: &ArtNew,
        ctxn: &[TNote],
        next_track: &[Option<u64>],
        tracks: &[Vec<usize>],
        nw: &impl Fn(usize) -> &'n ArtNew,
    ) -> ArtTables {
        let n = src.notes.len();
        let mut t = ArtTables::default();
        if let (Some((_, short, same_key)), Some(ctx)) = (new.gap, ctx) {
            let by_key;
            let next: &[Option<u64>] = if same_key {
                by_key = next_onsets(&src.notes, ctxn, |x| (x.track, x.pitch));
                &by_key
            } else {
                next_track
            };
            let mut tt = TempoTime::new(ctx);
            t.gap = src
                .notes
                .iter()
                .zip(next)
                .enumerate()
                .map(|(i, (x, nx))| match *nx {
                    Some(nx) => {
                        // The last tick sounding no later than `gap` before
                        // the next onset; never past it (a 0 ms gap).
                        let gap = nw(i).gap.map_or(0, |g| g.0);
                        let target = tt.micros_at(nx).saturating_sub(gap);
                        let end = tt
                            .tick_at_micros(target.saturating_add(1))
                            .saturating_sub(1)
                            .min(nx);
                        let shortest = tt.ticks_after(x.start, short);
                        end.saturating_sub(x.start).max(shortest)
                    }
                    None => u64::MAX,
                })
                .collect();
        }
        if let (Some((step, gates, interpolate)), Some(ctx)) = (&new.metric, ctx) {
            let at = |s: u64, slots: u64| {
                if s >= slots {
                    gates[0]
                } else {
                    gates[(s % gates.len() as u64) as usize]
                }
            };
            // Notes come in time order: the bar is looked up only when a note
            // leaves the previous one's.
            let mut bar: Option<(u64, u64)> = None;
            t.metric = src
                .notes
                .iter()
                .map(|x| {
                    let (bar_start, bar_len) = match bar {
                        Some((s0, l)) if x.start >= s0 && x.start - s0 < l => (s0, l),
                        _ => {
                            let m = metric_position(&ctx.meter, x.start);
                            bar = Some((m.bar_start, m.bar_len));
                            (m.bar_start, m.bar_len)
                        }
                    };
                    let off = x.start.saturating_sub(bar_start);
                    let slots = bar_len.div_ceil(*step).max(1);
                    let a = (off / step).min(slots - 1);
                    // Positions at a · step from the bar line; the next one
                    // (or the next bar line) at a1 — saturating at the far end.
                    let a0 = a * step;
                    let a1 = (a + 1)
                        .saturating_mul(*step)
                        .min(bar_len)
                        .max(a0.saturating_add(1));
                    if *interpolate {
                        let f = (off - a0) as f64 / (a1 - a0) as f64;
                        at(a, slots) + (at(a + 1, slots) - at(a, slots)) * f
                    } else if off - a0 <= a1.saturating_sub(off) {
                        at(a, slots)
                    } else {
                        at(a + 1, slots)
                    }
                })
                .collect();
        }
        if let Some((window, sparse, _, s_at, d_at)) = new.density {
            let mut all: Vec<(u16, u64, usize)> = src
                .notes
                .iter()
                .enumerate()
                .map(|(i, x)| (x.track, x.start, i))
                .chain(ctxn.iter().map(|x| (x.track, x.start, usize::MAX)))
                .collect();
            all.sort_unstable();
            let mut counts = vec![0u32; n];
            let mut lo = 0;
            while lo < all.len() {
                let tr = all[lo].0;
                let hi = lo + all[lo..].iter().take_while(|x| x.0 == tr).count();
                let times: Vec<u64> = all[lo..hi].iter().map(|x| x.1).collect();
                let c = trailing_counts(&times, window);
                for (k, x) in all[lo..hi].iter().enumerate() {
                    if x.2 != usize::MAX {
                        counts[x.2] = c[k];
                    }
                }
                lo = hi;
            }
            t.density = counts
                .iter()
                .enumerate()
                .map(|(i, &c)| {
                    let dense = nw(i).density.map_or(sparse, |d| d.2);
                    sparse + (dense - sparse) * art_ramp(f64::from(c), s_at, d_at)
                })
                .collect();
        }
        if let Some((_, curve, largest, nearest)) = &new.interval {
            let mut all: Vec<(u16, u64, u8)> = src
                .notes
                .iter()
                .chain(ctxn)
                .map(|x| (x.track, x.start, x.pitch))
                .collect();
            all.sort_unstable();
            t.interval = src
                .notes
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let within = nw(i).interval.as_ref().map_or(0, |v| v.0);
                    let p = all.partition_point(|a| (a.0, a.1) <= (x.track, x.start));
                    let Some(&(tr, at, lowest)) = all.get(p) else {
                        return ArtStep::Keep;
                    };
                    if tr != x.track {
                        return ArtStep::Keep;
                    }
                    let next = if *nearest {
                        let g = &all[p..p + all[p..].partition_point(|a| a.0 == tr && a.1 == at)];
                        let k = g.partition_point(|a| a.2 < x.pitch);
                        let mut best = (u8::MAX, lowest);
                        for c in [k.checked_sub(1), Some(k)].into_iter().flatten() {
                            if let Some(a) = g.get(c)
                                && a.2.abs_diff(x.pitch) < best.0
                            {
                                best = (a.2.abs_diff(x.pitch), a.2);
                            }
                        }
                        best.1
                    } else {
                        lowest
                    };
                    let iv = u32::from(next.abs_diff(x.pitch));
                    if iv <= within {
                        ArtStep::Legato
                    } else {
                        ArtStep::Gate(
                            curve
                                .eval((f64::from(iv) / f64::from(*largest)).min(1.0))
                                .max(0.0),
                        )
                    }
                })
                .collect();
        }
        if let (Some((slow, fast, g_slow, _, shape)), Some(ctx)) = (&new.tempo, ctx) {
            let tt = TempoTime::new(ctx);
            t.tempo = src
                .notes
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let g_fast = nw(i).tempo.as_ref().map_or(*g_slow, |v| v.3);
                    let y = shape.eval(art_ramp(tt.bpm_at(x.start), *slow, *fast));
                    g_slow + (g_fast - g_slow) * y
                })
                .collect();
        }
        if let Some((ranks, _, tol, from_top)) = &new.rank {
            let mut groups = ChordGroups::default();
            for idx in tracks {
                groups.append(&chords(src, idx, *tol));
            }
            let (r, sizes) = groups.voice_ranks(&src.notes);
            t.rank = (0..n)
                .map(|i| {
                    let size = sizes[i];
                    if size < 2 {
                        1.0
                    } else if r[i] + 1 == size {
                        nw(i).rank.as_ref().map_or(1.0, |v| v.1)
                    } else {
                        let k = if *from_top { size - 2 - r[i] } else { r[i] };
                        ranks[(k as usize).min(ranks.len() - 1)]
                    }
                })
                .collect();
        }
        if let Some((_, bars, _, approach, shape)) = &new.phrase {
            let meter = if *bars { ctx.map(|c| &c.meter) } else { None };
            let mut ph = vec![(1.0, u64::MAX); n];
            let rest_of = |i: usize| nw(i).phrase.as_ref().map_or(0, |v| v.0);
            for idx in tracks {
                let phrases = Phrases::of_by(&src.notes, idx, &rest_of, meter);
                let k = phrases.len();
                for q in 0..k {
                    let s = phrases.get(idx, q);
                    let cap = (q + 1 < k).then(|| src.notes[phrases.get(idx, q + 1)[0]].start);
                    // The last onset, then the approach onsets before it.
                    let mut j = s.len();
                    let mut onset = 0usize;
                    while j > 0 && onset <= *approach {
                        let st = src.notes[s[j - 1]].start;
                        let mut i0 = j - 1;
                        while i0 > 0 && src.notes[s[i0 - 1]].start == st {
                            i0 -= 1;
                        }
                        for &i in &s[i0..j] {
                            let f = nw(i).phrase.as_ref().map_or(1.0, |v| v.2);
                            let factor = if onset == 0 {
                                f
                            } else {
                                let x = (approach + 1 - onset) as f64 / (approach + 1) as f64;
                                1.0 + (f - 1.0) * shape.eval(x)
                            };
                            let most =
                                cap.map_or(u64::MAX, |c| c.saturating_sub(src.notes[i].start));
                            ph[i] = (factor, most);
                        }
                        j = i0;
                        onset += 1;
                    }
                }
            }
            t.phrase = ph;
        }
        if let Some((_, reset, floor, mode)) = new.repeat {
            // The notes are in time order, so each key's chain is walked in
            // one pass with its state (repeat count, latest end) in a table:
            // dense for key / key + channel, a map for subchannel ids.
            let slots = src.tracks.len().max(1) * 256 * if mode == 1 { 16 } else { 1 };
            let mut dense: Vec<(i32, u64, bool)> = if mode == 2 {
                Vec::new()
            } else {
                vec![(0, 0, false); slots]
            };
            let mut sparse: std::collections::HashMap<(u16, u8, u32), (i32, u64, bool)> =
                std::collections::HashMap::new();
            t.repeat = src
                .notes
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let change = nw(i).repeat.map_or(0.0, |v| v.0);
                    let st = if mode == 2 {
                        sparse.entry((x.track, x.pitch, x.sub)).or_default()
                    } else {
                        let k = usize::from(x.track) * 256 + usize::from(x.pitch);
                        let k = if mode == 1 {
                            k * 16 + usize::from(x.ch & 15)
                        } else {
                            k
                        };
                        &mut dense[k]
                    };
                    let (k, end, seen) = *st;
                    *st = if seen && x.start.saturating_sub(end) <= reset {
                        (k.saturating_add(1), end.max(x.end()), true)
                    } else {
                        (0, x.end(), true)
                    };
                    (1.0 + change).powi(st.0).clamp(floor, 1.0 / floor)
                })
                .collect();
        }
        if let Some((mode, num, window, _)) = new.reference {
            let mut rr = vec![f64::NAN; n];
            if mode == 2 {
                let mut refn: Vec<&TNote> = src
                    .aux
                    .first()
                    .map(|v| v.iter().collect())
                    .unwrap_or_default();
                refn.sort_by_key(|x| x.start);
                let ons = art_ref_onsets(&refn);
                for (i, x) in src.notes.iter().enumerate() {
                    rr[i] = art_nearest(&ons, x.start, window);
                }
            } else {
                for (slot, idx) in tracks.iter().enumerate() {
                    let sub = if mode == 1 {
                        match src.tracks.get(slot).and_then(|tr| tr.subs.get(num - 1)) {
                            Some(s) => Some(s.0),
                            None => continue,
                        }
                    } else {
                        None
                    };
                    let mut refn: Vec<&TNote> = src
                        .context
                        .iter()
                        .filter(|x| usize::from(x.track) == slot && sub.is_none_or(|s| x.sub == s))
                        .collect();
                    if let Some(s) = sub {
                        refn.extend(idx.iter().map(|&i| &src.notes[i]).filter(|x| x.sub == s));
                        refn.sort_by_key(|x| x.start);
                    }
                    let ons = art_ref_onsets(&refn);
                    for &i in idx {
                        if sub.is_none_or(|s| src.notes[i].sub != s) {
                            rr[i] = art_nearest(&ons, src.notes[i].start, window);
                        }
                    }
                }
            }
            t.reference = rr;
        }
        t
    }

    /// Stage 1 (the gate ratio, after the frozen functions): n02 → n04 →
    /// n05 → n06 → n08.
    fn gate(&self, i: usize, mut m: f64) -> f64 {
        if let Some(g) = self.metric.get(i) {
            m *= g;
        }
        if let Some(g) = self.density.get(i) {
            m *= g;
        }
        match self.interval.get(i) {
            Some(ArtStep::Legato) => m = 1.0,
            Some(ArtStep::Gate(g)) => m *= g,
            _ => {}
        }
        if let Some(g) = self.tempo.get(i) {
            m *= g;
        }
        if let Some(g) = self.rank.get(i) {
            m *= g;
        }
        m
    }

    /// Stage 2 (the length, after the frozen functions): n03 → n07 → n09 →
    /// n10. `base` = what the gate ratio is measured against (the distance
    /// to the next onset, or the note's own length).
    fn length(&self, i: usize, mut len: f64, base: f64, new: &ArtNew) -> f64 {
        if let Some(&(f, most)) = self.phrase.get(i) {
            len = if f >= 1.0 {
                len.max((len * f).min(most as f64))
            } else {
                len * f
            };
        }
        if let Some((ratios, round, strength)) = &new.palette
            && base > 0.0
        {
            let r = len / base;
            len = base * (r + strength * (art_snap(ratios, *round, r) - r));
        }
        if let Some(f) = self.repeat.get(i) {
            len *= f;
        }
        if let (Some(&rr), Some((.., blend))) = (self.reference.get(i), new.reference)
            && !rr.is_nan()
            && base > 0.0
        {
            let r = len / base;
            len = base * (r + blend * (rr - r));
        }
        len
    }

    /// Stage 3 (after the frozen gap rules): n01's real-time gap.
    fn gap(&self, i: usize, len: f64) -> f64 {
        match self.gap.get(i) {
            Some(&most) if most != u64::MAX => len.min(most as f64),
            _ => len,
        }
    }
}

/// n11 (output pass): per track, ends trimmed so no more than `most`
/// notes sound at any tick where the shortest length allows (the shared
/// `density_window::poly_ceiling_ends`).
fn art_ceiling(
    out: &mut Output,
    mark: super::super::output::Mark,
    base: usize,
    (most, first, shortest): (usize, usize, u64),
    most_of: &dyn Fn(usize) -> usize,
) {
    let notes = &out.notes[base..];
    let n = notes.len();
    if n <= most && (0..n).all(|i| n <= most_of(i)) {
        return;
    }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_unstable_by_key(|&i| (notes[i].track, notes[i].start, i));
    let mut end: Vec<u64> = notes.iter().map(TNote::end).collect();
    let mut lo = 0;
    while lo < n {
        let tr = notes[idx[lo]].track;
        let hi = lo
            + idx[lo..]
                .iter()
                .take_while(|&&i| notes[i].track == tr)
                .count();
        let ends = poly_ceiling_ends_by(notes, &idx[lo..hi], most_of, shortest, |i| {
            let x = &notes[i];
            match first {
                0 => (u64::MAX - x.dur, x.start),
                1 => (x.start, 0),
                _ => (u64::from(x.vel), x.start),
            }
        });
        for (k, &i) in idx[lo..hi].iter().enumerate() {
            end[i] = ends[k];
        }
        lo = hi;
    }
    out.adjust_since(mark, |i, note, _, _| {
        if let Some(&e) = end.get(i)
            && e < note.end()
        {
            note.dur = e - note.start;
        }
        true
    });
}

/// Reads the reference track of Articulate's n10 ("A track") into
/// `Source::aux` (its notes around the selection, capped).
pub fn articulate_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("n10") || p.choice("n10.ref") != 2 || src.stats.is_empty() {
        return Ok(());
    }
    let num = p.int("n10.num");
    let track = usize::try_from(num - 1)
        .ok()
        .filter(|&i| doc.track(i).is_some())
        .ok_or_else(|| {
            TransformError::Invalid(format!(
                "Track {num} does not exist. Choose the reference track by its number in the track list."
            ))
        })?;
    let w = p.ticks("n10.window");
    let a = src.stats.min_start.saturating_sub(w);
    // Far enough past the selection for the last matched onset's next one.
    let b = src
        .stats
        .max_start
        .saturating_add(w)
        .saturating_add(src.stats.span())
        .saturating_add(1);
    let mut layer = Vec::new();
    // B2-F16: the reference track as the song shows it, on these ticks.
    let shift = ctx.map_or(0, |c| c.shift);
    super::super::source::placed_query(doc, track, shift, a, b, &mut |r| {
        if r.start_tick >= a && r.start_tick <= b && layer.len() < limit {
            layer.push(TNote::from_record(&r, u16::MAX));
        }
    });
    src.aux = std::sync::Arc::new(vec![layer]);
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn art_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let style = p.choice("style");
    let custom = p.float("multiply");
    let gap = p.flag("gap");
    let use_lengths = p.flag("use_lengths");
    let chop = p.flag("chop") || p.flag("just_chop");
    let just_chop = p.flag("just_chop");
    let only_sel = p.flag("only_sel");
    let variation = p.float("variation") / 100.0;
    let seed = p.seed("seed");
    let a1 = p.adv(1).then(|| (p.curve("a1.curve"), p.ticks("a1.max")));
    let a2 = p.adv(2).then(|| {
        (
            p.float("a2.from") / 100.0,
            p.float("a2.to") / 100.0,
            p.float("a2.shape"),
        )
    });
    let a3 = p.adv(3).then(|| p.float("a3.amount"));
    let a4 = p.adv(4).then(|| {
        (
            p.int("a4.lo") as f64,
            p.int("a4.hi") as f64,
            p.float("a4.low") / 100.0,
            p.float("a4.high") / 100.0,
        )
    });
    let a5 = p.adv(5);
    let a6 = if p.adv(6) {
        p.list("a6.seq")
    } else {
        Vec::new()
    };
    let a7 = p
        .adv(7)
        .then(|| (p.float("a7.amount") / 100.0, p.ticks("a7.len")));
    let a8 = p
        .adv(8)
        .then(|| (p.ticks("a8.min"), p.ticks("a8.max"), p.float("a8.shape")));
    let a9 = if p.adv(9) {
        p.list("a9.ratios")
    } else {
        Vec::new()
    };
    let a10 = p.adv(10).then(|| p.int("a10.ticks"));
    let stats = b.stats.clone();
    let new = ArtNew::from(p, b.ctx);
    let song = new.needs_ctx().then(|| b.ctx.clone());
    // With Control Sweeps (E0422; every Articulate target is a function's
    // control): each note's functions from its level.
    let swept = Rebuild::of(p, b);
    let sctx = b.ctx.clone();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let lv: Option<PerLevel<ArtNew>> = swept
                .as_ref()
                .map(|r| r.levels(&sctx, src, |p, b| ArtNew::from(p, b.ctx)));
            let nw_fn = |i: usize| lv.as_ref().map_or(&new, |l| l.at(i));
            let nw = &nw_fn;
            let mark = out.mark();
            let base_len = out.notes.len();
            let ctx: &[TNote] = if only_sel { &[] } else { &src.context };
            let next = next_onsets(&src.notes, ctx, |n| n.track);
            // Onset index per note (alternating patterns, transfer).
            let mut onset_ix = vec![0usize; src.notes.len()];
            let chord_next: Vec<Option<u64>> = next.clone();
            let mut chord_groups = ChordGroups::default();
            let tracks = per_track(src);
            for idx in &tracks {
                let mut k = 0usize;
                let mut last: Option<u64> = None;
                for &i in idx {
                    let s = src.notes[i].start;
                    if last.is_some_and(|l| l != s) {
                        k += 1;
                    }
                    last = Some(s);
                    onset_ix[i] = k;
                }
                if a5 {
                    chord_groups.append(&chords(src, idx, 0));
                }
            }
            // v0.9 functions declared by id: per-note values once per run.
            let tables = new
                .per_note()
                .then(|| ArtTables::of(src, song.as_ref(), &new, ctx, &next, &tracks, nw));
            // Length ranks for quantile mapping.
            let ranks: Vec<f64> = if a8.is_some() {
                let mut order: Vec<usize> = (0..src.notes.len()).collect();
                order.sort_by_key(|&i| (src.notes[i].dur, i));
                let mut r = vec![0.0; src.notes.len()];
                let n = order.len().max(2) as f64 - 1.0;
                for (k, &i) in order.iter().enumerate() {
                    r[i] = k as f64 / n.max(1.0);
                }
                r
            } else {
                Vec::new()
            };
            let mut lens: Vec<f64> = vec![0.0; src.notes.len()];
            for (i, n) in src.notes.iter().enumerate() {
                let nx = chord_next[i];
                let dist = nx.map(|x| x - n.start);
                let mut len: f64;
                if just_chop {
                    len = n.dur as f64;
                } else {
                    let (mut mult, mut small_gap) = style_mult(style, custom);
                    small_gap |= gap;
                    if !a6.is_empty() {
                        let code = a6[onset_ix[i] % a6.len()] as usize;
                        let (m, g) = style_mult(code.min(2), custom);
                        mult = m;
                        small_gap |= g;
                    }
                    let base = match (use_lengths, dist) {
                        (false, Some(d)) => d as f64,
                        _ => n.dur as f64,
                    };
                    if let Some((f, t, sh)) = a2 {
                        mult = f + (t - f) * pos(n, &stats).powf(sh);
                    }
                    if let Some(am) = a3 {
                        mult *= (1.0 + am * (f64::from(n.vel) / 127.0 * 2.0 - 1.0)).max(0.05);
                    }
                    if let Some((lo, hi, ml, mh)) = a4 {
                        let x = if hi > lo {
                            ((f64::from(n.pitch) - lo) / (hi - lo)).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        mult *= ml + (mh - ml) * x;
                    }
                    if !a9.is_empty() {
                        mult = a9[onset_ix[i] % a9.len()] as f64 / 100.0;
                    }
                    if let Some(t) = &tables {
                        mult = t.gate(i, mult);
                    }
                    len = base * mult;
                    if let Some((lo, hi, sh)) = a8 {
                        len = lo as f64 + (hi as f64 - lo as f64) * ranks[i].powf(sh);
                    }
                    if variation > 0.0 {
                        let mut r = Rng::for_note(seed, salt::GATE, n);
                        len *= 1.0 + variation * (r.unit() * 2.0 - 1.0);
                    }
                    if let Some((amt, l)) = a7 {
                        len *= 1.0 + amt * smooth_noise(seed, salt::SHAPE, n.start, l);
                    }
                    if let Some(t) = &tables {
                        len = t.length(i, len, base, nw(i));
                    }
                    if let Some(d) = dist {
                        if small_gap && len >= d as f64 {
                            len = d as f64 - (d as f64 / 32.0).max(1.0);
                        }
                        if let Some((c, mx)) = &a1 {
                            len = len.min(d as f64 - c.eval(pos(n, &stats)) * *mx as f64);
                        }
                        if let Some(o) = a10 {
                            len = d as f64 + o as f64;
                        }
                    }
                    if let Some(t) = &tables {
                        len = t.gap(i, len);
                    }
                }
                // Chop chords: a held note is cut where a later onset begins.
                if chop
                    && let Some(d) = next[i]
                    && n.start as f64 + len > d as f64
                {
                    len = (d - n.start) as f64;
                }
                lens[i] = len;
            }
            // Chord groups are articulated as one unit: every note of a chord
            // takes the length computed for its lowest note.
            for g in chord_groups.iter() {
                if let Some(&root) = g.iter().min_by_key(|&&i| (src.notes[i].pitch, i)) {
                    let l = lens[root];
                    for &i in g {
                        lens[i] = l;
                    }
                }
            }
            for (i, n) in src.notes.iter().enumerate() {
                out.emit(
                    i128::from(n.start),
                    lens[i].round() as i128,
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                );
                out.check()?;
            }
            if let Some(c) = new.ceiling {
                let most_of = |i: usize| nw(i).ceiling.map_or(c.0, |x| x.0);
                art_ceiling(out, mark, base_len, c, &most_of);
            }
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}

// ================================================================ Chopper

pub const CHOP: ToolDef = ToolDef {
    id: "chop",
    spec: chop_spec,
    build: chop_build,
};

const CHOP_PATTERNS: &[&str] = &[
    "Even (unit)",
    "Pairs 2+2",
    "Gallop 1+1+2",
    "Reverse gallop 2+1+1",
    "Dotted 3+1",
    "Swing 2+1",
    "Stutter 1+1+1+5",
    "Custom (list)",
];

fn builtin_chop(i: usize, custom: &[i64]) -> Vec<i64> {
    match i {
        1 => vec![2, 2],
        2 => vec![1, 1, 2],
        3 => vec![2, 1, 1],
        4 => vec![3, 1],
        5 => vec![2, 1],
        6 => vec![1, 1, 1, 5],
        7 if !custom.is_empty() => custom.to_vec(),
        _ => vec![1],
    }
}

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CHOP_HELP: &[(&str, &str)] = &[
    (
        "pattern",
        "The rhythm each note is sliced into, as slice lengths in units: even slices, pairs, gallops, dotted, swing, stutter, or your own list.",
    ),
    (
        "custom",
        "For Custom: the slice lengths in units, in order (1 1 2 = two short slices and a long one); the pattern repeats through the note.",
    ),
    (
        "pvel",
        "The velocity of each slice in turn, as a percentage of the note's (Levels decides how much applies); the list repeats.",
    ),
    (
        "ptrans",
        "Transposes each slice in turn by this many semitones; the list repeats.",
    ),
    (
        "holds",
        "Marks slices that are not cut off: a 1 joins that slice to the next one; the list follows the pattern's slices and repeats.",
    ),
    (
        "unit",
        "The length of one unit of the pattern, as a note value.",
    ),
    (
        "unit.ticks",
        "For Custom: the length of one unit, in ticks.",
    ),
    (
        "mul",
        "Makes the slices faster (×2 faster halves every slice) or slower than the chosen unit.",
    ),
    (
        "levels",
        "How much the slice velocities apply: 0 keeps the note's velocity, 1 applies the pattern's percentages fully.",
    ),
    (
        "absolute",
        "Aligns the pattern to the song's grid, so slices fall at the same places whatever the note's start; off starts the pattern at each note's start.",
    ),
    (
        "group",
        "Puts each note's slices into a note group of their own, so they can be selected as one.",
    ),
    (
        "a1",
        "Morphs the slice pattern into a second pattern across the selection.",
    ),
    ("a1.b", "The pattern the slicing morphs toward."),
    (
        "a1.curve",
        "How far the morph has gone along the selection's time span (0 = the main pattern, 1 = Pattern B).",
    ),
    (
        "a2",
        "Cuts every slice again into smaller equal parts, level after level.",
    ),
    ("a2.depth", "How many levels of further cutting."),
    (
        "a2.parts",
        "How many equal parts each slice is cut into at every level.",
    ),
    (
        "a3",
        "Keeps each slice only with a chance that follows a curve across the selection (seeded); the rest become silence.",
    ),
    (
        "a3.curve",
        "The chance a slice is kept, from the selection's start (left) to its end (right).",
    ),
    (
        "a4",
        "Moves the slices through the track's subchannels, slice by slice.",
    ),
    (
        "a4.seq",
        "How many subchannels each slice moves from the note's own, slice by slice in turn; the list repeats.",
    ),
    (
        "a5",
        "Shortens every slice but the note's last, so the slices are detached while the note still ends where it did.",
    ),
    (
        "a5.gate",
        "Each inner slice's length as a share of its slot.",
    ),
    (
        "a6",
        "Makes high notes slice faster and low notes slower (or the reverse) around a pivot key.",
    ),
    ("a6.pivot", "The key at which the slice unit is unchanged."),
    (
        "a6.amount",
        "How much the slice rate doubles per octave above the pivot (and halves below); negative reverses it.",
    ),
    (
        "a7",
        "Bunches the slices toward the note's start or end instead of spacing them evenly.",
    ),
    (
        "a7.dist",
        "+ packs the slices ever closer toward the note's end (speeding up), − toward its start (slowing down); 0 = even spacing.",
    ),
    ("a8", "Shapes the slices' velocity along each note."),
    (
        "a8.curve",
        "The velocity multiplier from the note's start (left) to its end (right).",
    ),
    ("a9", "Plays each note's slice lengths in reverse order."),
    (
        "a9.mode",
        "Reversed flips every note; Ping-Pong per Note flips about half of them (seeded by the note), alternating the rhythm.",
    ),
    (
        "a10",
        "Groups the slices of each note into smaller note groups of a few slices each.",
    ),
    ("a10.size", "How many consecutive slices share one group."),
    (
        "n01",
        "Slices only on the hits of a Euclidean rhythm on the unit grid: Pulses cuts spread as evenly as possible over Steps.",
    ),
    ("n01.steps", "The length of the rhythm, in units."),
    ("n01.pulses", "How many of those units start a new slice."),
    ("n01.rot", "Rotates the rhythm by this many units."),
    (
        "n01.by",
        "Rotates the rhythm further for each note (its place on the track), each semitone, or each subchannel; None rotates every note alike.",
    ),
    (
        "n02",
        "Slices at a real-time rate (slices per second or milliseconds per slice), whatever the tempo.",
    ),
    (
        "n02.unit",
        "Whether the rates below are slices per second (Hz) or milliseconds per slice.",
    ),
    ("n02.from", "The rate at the start."),
    ("n02.to", "The rate at the end."),
    (
        "n02.curve",
        "How the rate travels from the start rate to the end rate.",
    ),
    (
        "n02.along",
        "Whether the rate's start and end refer to each note, or to the whole selection.",
    ),
    (
        "n03",
        "Makes the slices step in pitch from the note toward an interval, like a glissando.",
    ),
    (
        "n03.interval",
        "Where the last slice lands, in semitones from the note (+ up, − down).",
    ),
    (
        "n03.path",
        "Which pitches the slices step through: every semitone, the key and scale, or octaves only.",
    ),
    ("n03.key", "For Key and Scale: the key of the scale."),
    (
        "n03.scale",
        "For Key and Scale: the scale the glissando steps through.",
    ),
    (
        "n03.shape",
        "How the pitch travels from the first slice to the last (a straight line by default).",
    ),
    (
        "n04",
        "Hands the slices of a chord around its voices, a few voices per slice, like a hocket.",
    ),
    (
        "n04.voices",
        "How many voices of the chord sound in each slice.",
    ),
    (
        "n04.order",
        "The order the voices take turns: up from the bass, down from the top, outside-in, or your list.",
    ),
    (
        "n04.list",
        "For List: the voices' turns by rank from the bass (0 = the bass, 1 = the next …); unlisted voices follow.",
    ),
    (
        "n04.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n05",
        "Chooses each note's number of slices so the selection plays at a target number of notes per second.",
    ),
    (
        "n05.nps",
        "The notes per second the slicing aims for, counting the other notes sounding around it.",
    ),
    (
        "n05.curve",
        "The target as a share of Target NPS across the selection (1 = the full target).",
    ),
    (
        "n05.window",
        "The time around each note, in milliseconds, over which the sounding notes are counted.",
    ),
    (
        "n05.most",
        "No note is cut into more than this many slices.",
    ),
    (
        "n06",
        "Ramps each note's slice velocities toward the velocity of the note that follows.",
    ),
    (
        "n06.next",
        "Which following note: the next onset of the track (its note nearest in pitch), or the next note on the same key.",
    ),
    (
        "n06.shape",
        "How the velocity travels from the note's own to the next note's along the note.",
    ),
    (
        "n06.amount",
        "How far the ramp reaches toward the next note's velocity (100 % = all the way by the note's end).",
    ),
    (
        "n07",
        "Offsets each note's slice grid by its key, subchannel or chord rank, so the cuts spiral across the chord.",
    ),
    (
        "n07.phase",
        "How far the grid moves per step, as a share of one unit.",
    ),
    (
        "n07.by",
        "What counts the steps: semitones from the reference key, the subchannel's place, or the voice's rank in its chord.",
    ),
    ("n07.ref", "For Semitone: the key whose grid is not moved."),
    (
        "n07.mode",
        "Wrap in the Unit keeps the offset within one unit; Accumulate lets it run through the whole pattern.",
    ),
    (
        "n08",
        "Continues slicing into short rests after a note, fading out.",
    ),
    (
        "n08.longest",
        "The longest stretch, in ticks, the slices continue into a rest.",
    ),
    (
        "n08.below",
        "Only rests shorter than this, in ticks, are filled.",
    ),
    (
        "n08.fade",
        "The velocity multiplier across the filled rest (1 = full, 0 = silent).",
    ),
    (
        "n08.next",
        "The rest ends at the next onset of the track, or the next note on the same key.",
    ),
    (
        "n09",
        "Changes the slice unit by the note's place in the bar, one factor per grid position.",
    ),
    ("n09.grid", "The grid the bar is divided into."),
    (
        "n09.factors",
        "The factor of each grid position from the bar line; the list repeats.",
    ),
    (
        "n09.factor",
        "Divide makes the slices that many times shorter; Multiply that many times longer.",
    ),
    (
        "n10",
        "Cuts each note wherever a reference note starts inside it, instead of the pattern.",
    ),
    (
        "n10.ref",
        "Where the reference onsets come from: the other selected notes, the track's unselected notes, a subchannel of the track, or all tracks.",
    ),
    ("n10.num", "For A Subchannel: the subchannel's number."),
    (
        "n10.merge",
        "Reference onsets closer together than this, in ticks, make only one cut.",
    ),
    ("n10.short", "No slice is made shorter than this, in ticks."),
    (
        "n11",
        "Lets the pattern run on from one note to the next instead of starting again at every note.",
    ),
    (
        "n11.per",
        "Which notes share a running pattern: the whole track, each key, or each subchannel.",
    ),
    (
        "n11.reset",
        "When the pattern starts again: never, at every bar line, or after a rest longer than Reset Rest.",
    ),
    (
        "n11.rest",
        "For Rests Longer Than: the rest length, in ticks, that starts the pattern again.",
    ),
];

fn chop_spec() -> ToolSpec {
    let mut p = vec![
        choice("pattern", "Pattern", CHOP_PATTERNS, 0),
        list(
            "custom",
            "Custom slice lengths (units)",
            1,
            64,
            256,
            &[1, 1, 2],
        ),
        list("pvel", "Slice velocities %", 1, 200, 256, &[100]),
        list(
            "ptrans",
            "Slice transposition (semitones)",
            -48,
            48,
            256,
            &[0],
        ),
        list(
            "holds",
            "Hold (1 = not chopped, joins next)",
            0,
            1,
            256,
            &[0],
        ),
        choice("unit", "Unit", NOTE_VALUES, 4),
        ticks("unit.ticks", "Custom unit", 1, 1 << 40, 240),
        choice("mul", "Time multiplier", TIME_MULS, 3),
        float("levels", "Levels (original → pattern)", 0.0, 1.0, 1.0, ""),
        flag("absolute", "Absolute pattern (grid aligned)", false),
        flag("group", "Group notes", false),
        seed("seed"),
        adv_switch(1, "Multi-pattern morph"),
        choice("a1.b", "Pattern B", CHOP_PATTERNS, 4).adv(1),
        curve(
            "a1.curve",
            "A → B across the selection",
            Curve::line(0.0, 1.0),
        )
        .adv(1),
        adv_switch(2, "Chop depth"),
        int("a2.depth", "Depth", 1, 4, 1).adv(2),
        int("a2.parts", "Parts per level", 2, 4, 2).adv(2),
        adv_switch(3, "Probability map"),
        curve(
            "a3.curve",
            "Keep probability across the selection",
            Curve::flat(0.6),
        )
        .adv(3),
        adv_switch(4, "Subchannel slice cycling"),
        list(
            "a4.seq",
            "Subchannel steps per slice",
            -4096,
            4096,
            256,
            &[0, 1],
        )
        .adv(4),
        adv_switch(5, "Gate-preserve mode"),
        float("a5.gate", "Inner slice gate", 5.0, 100.0, 60.0, "%").adv(5),
        adv_switch(6, "Pitch-dependent density"),
        pitch("a6.pivot", "Pivot", 60).adv(6),
        float("a6.amount", "Octaves → density", -2.0, 2.0, 1.0, "").adv(6),
        adv_switch(7, "Time-warp spacing"),
        float("a7.dist", "Distortion", -100.0, 100.0, 60.0, "%").adv(7),
        adv_switch(8, "Velocity transfer curve"),
        curve(
            "a8.curve",
            "Velocity along each note",
            Curve::line(1.0, 0.3),
        )
        .overshoot()
        .adv(8),
        adv_switch(9, "Mirrored slice order"),
        choice("a9.mode", "Order", &["Reversed", "Ping-pong per note"], 0).adv(9),
        adv_switch(10, "Hierarchical chop groups"),
        int("a10.size", "Slices per child group", 1, 64, 2).adv(10),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Euclidean slice rhythm", false),
        int("n01.steps", "Steps", 1, 64, 8),
        int("n01.pulses", "Pulses", 1, 64, 3),
        int("n01.rot", "Rotation", -64, 64, 0),
        choice("n01.by", "Rotate by", CHOP_ROTATE_BY, 0),
        flag("n02", "Real-time slice rate", false),
        choice("n02.unit", "Unit", ARP_RATE_UNITS, 0),
        float("n02.from", "Start rate", 0.1, 2000.0, 16.0, ""),
        float("n02.to", "End rate", 0.1, 2000.0, 16.0, ""),
        curve("n02.curve", "Rate curve", Curve::line(0.0, 1.0)),
        choice("n02.along", "Rate runs along", CHOP_RATE_ALONG, 0),
        flag("n03", "Slice glissando", false),
        int("n03.interval", "Interval (semitones)", -48, 48, 12),
        choice("n03.path", "Path", CHOP_GLISS_PATH, 0),
        choice("n03.key", "Key", &KEY_NAMES, 0),
        choice("n03.scale", "Scale", scale_names(), 0),
        curve("n03.shape", "Shape", Curve::line(0.0, 1.0)).overshoot(),
        flag("n04", "Voice hocket", false),
        int("n04.voices", "Voices per slice", 1, 16, 1),
        choice("n04.order", "Order", CHOP_HOCKET_ORDER, 0),
        list(
            "n04.list",
            "Voice order (ranks from the bass)",
            0,
            127,
            128,
            &[0, 2, 1],
        ),
        ticks("n04.tol", "Chord tolerance", 0, 1 << 30, 0),
        flag("n05", "Target NPS chop", false),
        int("n05.nps", "Target NPS", 1, 10_000_000, 1000),
        curve("n05.curve", "Target over the selection", Curve::flat(1.0)).overshoot(),
        float("n05.window", "Window", 10.0, 5000.0, 1000.0, "ms"),
        int("n05.most", "Most slices per note", 1, 4096, 8),
        flag("n06", "Velocity bridge", false),
        choice("n06.next", "Next note", CHOP_NEXT_NOTE, 0),
        curve("n06.shape", "Ramp shape", Curve::line(0.0, 1.0)).overshoot(),
        float("n06.amount", "Amount", 0.0, 100.0, 100.0, "%"),
        flag("n07", "Pitch phase spiral", false),
        float(
            "n07.phase",
            "Phase per step (% of the unit)",
            -100.0,
            100.0,
            25.0,
            "%",
        ),
        choice("n07.by", "Step by", CHOP_PHASE_BY, 0),
        pitch("n07.ref", "Reference pitch", 60),
        choice("n07.mode", "Phase", CHOP_PHASE_MODE, 0),
        flag("n08", "Rest fill", false),
        ticks("n08.longest", "Longest fill", 1, 1 << 40, 960),
        ticks("n08.below", "Only rests shorter than", 1, 1 << 40, 1920),
        curve("n08.fade", "Fade", Curve::line(1.0, 0.3)).overshoot(),
        choice("n08.next", "Next onset of", CHOP_FILL_NEXT, 0),
        flag("n09", "Metric unit map", false),
        choice("n09.grid", "Grid", art_grid_values(), 2),
        list(
            "n09.factors",
            "Unit factor per position",
            1,
            64,
            64,
            &[1, 2, 2, 2],
        ),
        choice("n09.factor", "Factor", CHOP_FACTOR, 0),
        flag("n10", "Onset-synchronous chop", false),
        choice("n10.ref", "Reference", CHOP_REFERENCE, 0),
        int("n10.num", "Reference subchannel (number)", 1, 4096, 1),
        ticks("n10.merge", "Merge cuts within", 0, 1 << 40, 30),
        ticks("n10.short", "Shortest slice", 1, 1 << 40, 60),
        flag("n11", "Running pattern phase", false),
        choice("n11.per", "Carry per", CHOP_CARRY, 0),
        choice("n11.reset", "Reset at", CHOP_RESET, 0),
        ticks("n11.rest", "Reset rest", 0, 1 << 40, 1920),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, CHOP_HELP);
    ToolSpec {
        id: "chop",
        name: "Chopper",
        family: Family::Fl,
        summary: "Slices notes with a rhythmic pattern (relative to each note, or aligned to the grid).",
        params: p,
        advanced: vec![
            "Multi-pattern morph",
            "Chop depth",
            "Probability map",
            "Subchannel slice cycling",
            "Gate-preserve mode",
            "Pitch-dependent density",
            "Time-warp spacing",
            "Velocity transfer curve",
            "Mirrored slice order",
            "Hierarchical chop groups",
        ],
        functions: [
            ("n01", "Euclidean slice rhythm", 110),
            ("n02", "Real-time slice rate", 120),
            ("n03", "Slice glissando", 130),
            ("n04", "Voice hocket", 140),
            ("n05", "Target NPS chop", 150),
            ("n06", "Velocity bridge", 160),
            ("n07", "Pitch phase spiral", 170),
            ("n08", "Rest fill", 180),
            ("n09", "Metric unit map", 190),
            ("n10", "Onset-synchronous chop", 200),
            ("n11", "Running pattern phase", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

const CHOP_ROTATE_BY: &[&str] = &["None", "Per note", "Per semitone", "Per subchannel"];
const CHOP_RATE_ALONG: &[&str] = &["Each note", "The selection"];
const CHOP_GLISS_PATH: &[&str] = &["Chromatic", "Key and scale", "Octaves"];
const CHOP_HOCKET_ORDER: &[&str] = &["Up", "Down", "Outside-in", "List"];
const CHOP_NEXT_NOTE: &[&str] = &["Next onset of the track", "Next note of the same key"];
const CHOP_PHASE_BY: &[&str] = &["Semitone", "Subchannel", "Chord rank"];
const CHOP_PHASE_MODE: &[&str] = &["Wrap in the unit", "Accumulate through the pattern"];
const CHOP_FILL_NEXT: &[&str] = &["The track", "The same key"];
const CHOP_FACTOR: &[&str] = &["Divide", "Multiply"];
const CHOP_REFERENCE: &[&str] = &[
    "Other selected notes",
    "Unselected notes of the track",
    "A subchannel",
    "All tracks",
];
const CHOP_CARRY: &[&str] = &["Track", "Key", "Subchannel"];
const CHOP_RESET: &[&str] = &["Never", "Every bar", "Rests longer than"];

/// The v0.9 functions of the Chopper declared by id (TOOL_CATALOG `chop`
/// n01 … n11). `None` = switched off; with every one off the kernel is the
/// v0.8 one.
#[derive(Clone, Default)]
struct ChopNew {
    /// n01: the pattern, rotate by (0 none, 1 note, 2 semitone, 3
    /// subchannel).
    euclid: Option<(Euclid, usize)>,
    /// n02: milliseconds per slice, start / end rate, curve, along the
    /// selection.
    rate: Option<(bool, f64, f64, Curve, bool)>,
    /// n03: interval, path (0 chromatic, 1 scale, 2 octaves), lattice, shape.
    gliss: Option<(i64, usize, Lattice, Curve)>,
    /// n04: voices per slice, order (0 up, 1 down, 2 outside-in, 3 list),
    /// list, chord tolerance.
    hocket: Option<(u32, usize, Vec<i64>, u64)>,
    /// n05: target NPS, curve over the selection, window µs, most slices.
    target: Option<(f64, Curve, f64, u32)>,
    /// n06: next note of the same key, shape, amount (fraction).
    bridge: Option<(bool, Curve, f64)>,
    /// n07: phase per step (fraction of the unit), step by (0 semitone, 1
    /// subchannel, 2 chord rank), reference pitch, accumulate.
    spiral: Option<(f64, usize, i64, bool)>,
    /// n08: longest fill, only rests shorter than, fade, same key.
    fill: Option<(u64, u64, Curve, bool)>,
    /// n09: grid ticks, factor per position, multiply.
    metric: Option<(u64, Vec<f64>, bool)>,
    /// n10: reference (0 other selected, 1 unselected of the track, 2 a
    /// subchannel, 3 all tracks), subchannel place (1-based), merge,
    /// shortest slice.
    onset: Option<(usize, usize, u64, u64)>,
    /// n11: carry per (0 track, 1 key, 2 subchannel), reset (0 never, 1
    /// every bar, 2 rests longer than), reset rest.
    running: Option<(usize, usize, u64)>,
}

impl ChopNew {
    fn from(p: &Params) -> ChopNew {
        ChopNew {
            euclid: p.flag("n01").then(|| {
                (
                    Euclid::new(
                        p.int("n01.steps").max(1) as u32,
                        p.int("n01.pulses").max(1) as u32,
                        p.int("n01.rot"),
                    ),
                    p.choice("n01.by"),
                )
            }),
            rate: p.flag("n02").then(|| {
                (
                    p.choice("n02.unit") == 1,
                    p.float("n02.from"),
                    p.float("n02.to"),
                    p.curve("n02.curve"),
                    p.choice("n02.along") == 1,
                )
            }),
            gliss: p.flag("n03").then(|| {
                (
                    p.int("n03.interval"),
                    p.choice("n03.path"),
                    Lattice::of_scale(&Scale::new(p.choice("n03.key"), p.choice("n03.scale"))),
                    p.curve("n03.shape"),
                )
            }),
            hocket: p.flag("n04").then(|| {
                (
                    p.int("n04.voices").max(1) as u32,
                    p.choice("n04.order"),
                    p.list("n04.list"),
                    p.ticks("n04.tol"),
                )
            }),
            target: p.flag("n05").then(|| {
                (
                    p.int("n05.nps").max(1) as f64,
                    p.curve("n05.curve"),
                    p.float("n05.window").max(1.0) * 1000.0,
                    p.int("n05.most").max(1) as u32,
                )
            }),
            bridge: p.flag("n06").then(|| {
                (
                    p.choice("n06.next") == 1,
                    p.curve("n06.shape"),
                    p.float("n06.amount") / 100.0,
                )
            }),
            spiral: p.flag("n07").then(|| {
                (
                    p.float("n07.phase") / 100.0,
                    p.choice("n07.by"),
                    p.int("n07.ref"),
                    p.choice("n07.mode") == 1,
                )
            }),
            fill: p.flag("n08").then(|| {
                (
                    p.ticks("n08.longest").max(1),
                    p.ticks("n08.below").max(1),
                    p.curve("n08.fade"),
                    p.choice("n08.next") == 1,
                )
            }),
            metric: p.flag("n09").then(|| {
                let mut f: Vec<f64> = p
                    .list("n09.factors")
                    .iter()
                    .map(|&v| v.max(1) as f64)
                    .collect();
                if f.is_empty() {
                    f.push(1.0);
                }
                (0, f, p.choice("n09.factor") == 1)
            }),
            onset: p.flag("n10").then(|| {
                (
                    p.choice("n10.ref"),
                    p.int("n10.num").max(1) as usize,
                    p.ticks("n10.merge"),
                    p.ticks("n10.short").max(1),
                )
            }),
            running: p.flag("n11").then(|| {
                (
                    p.choice("n11.per"),
                    p.choice("n11.reset"),
                    p.ticks("n11.rest"),
                )
            }),
        }
    }

    fn any(&self) -> bool {
        self.euclid.is_some()
            || self.rate.is_some()
            || self.gliss.is_some()
            || self.hocket.is_some()
            || self.target.is_some()
            || self.bridge.is_some()
            || self.spiral.is_some()
            || self.fill.is_some()
            || self.metric.is_some()
            || self.onset.is_some()
            || self.running.is_some()
    }

    /// Whether a function on needs the other notes (the kernel then runs
    /// over the whole source; per-note functions keep the lazy preview).
    fn contextual(&self) -> bool {
        self.euclid.as_ref().is_some_and(|e| e.1 == 1)
            || self.hocket.is_some()
            || self.target.is_some()
            || self.bridge.is_some()
            || self.spiral.as_ref().is_some_and(|s| s.1 == 2)
            || self.fill.is_some()
            || self.onset.is_some()
            || self.running.is_some()
    }
}

/// What one note's slicing needs from the other notes (whole-source runs;
/// the defaults in a per-note run).
#[derive(Clone, Copy, Debug, Default)]
struct ChopInfo {
    /// n01 "per note": the note's place among its track's selected notes.
    index: u64,
    /// The note's voice rank and its chord's size (n04, n07 chord rank).
    rank: u32,
    size: u32,
    /// n04: the voice's place in the hocket order.
    voice: u32,
    /// n05: the note's unit in ticks (0 = the tool's).
    unit: f64,
    /// n06: the next note's velocity.
    next_vel: Option<u8>,
    /// n08: where the fill ends (0 = no fill).
    fill_end: u64,
    /// n10: the note's cuts in the shared cut list.
    cuts: (usize, usize),
    /// n11: the pattern step the note starts at.
    k0: u64,
}

/// n04: the place of voice `rank` (0 = the bass) of a chord of `size` in
/// the hocket order.
fn chop_voice(order: usize, list: &[i64], rank: u32, size: u32) -> u32 {
    let top = size.saturating_sub(1);
    match order {
        1 => top - rank.min(top),
        2 => {
            let from_top = top - rank.min(top);
            if rank <= from_top {
                2 * rank
            } else {
                2 * from_top + 1
            }
        }
        3 => match list.iter().position(|&r| r == i64::from(rank)) {
            Some(q) => q as u32,
            None => list.len() as u32 + rank,
        },
        _ => rank,
    }
}

#[derive(Clone)]
struct Chop {
    a: Vec<i64>,
    b: Option<(Vec<i64>, Curve)>,
    vels: Vec<i64>,
    trans: Vec<i64>,
    holds: Vec<i64>,
    unit: u64,
    levels: f64,
    absolute: bool,
    group: bool,
    seed: u64,
    depth: Option<(u32, u64)>,
    keep: Option<Curve>,
    sub_seq: Vec<i64>,
    gate_preserve: Option<f64>,
    density: Option<(f64, f64)>,
    warp: Option<f64>,
    vcurve: Option<Curve>,
    mirror: Option<usize>,
    child: Option<u64>,
    stats: super::super::SourceStats,
    subs: Vec<Vec<(u32, u8)>>,
    new: std::sync::Arc<ChopNew>,
    /// The song context (tempo map, meter) when a function reads it.
    ctx: Option<std::sync::Arc<super::super::Ctx>>,
    /// n02 along the selection: its first start and last end in µs.
    sel_us: (f64, f64),
    /// n02: the fewest ticks between two real-time slices (the preflight's
    /// bound holds where microseconds lose precision far out).
    min_gap: u64,
}

impl Chop {
    /// Note `i`'s functions under Control Sweeps (its level's, E0422).
    fn nw<'a>(&'a self, lv: Option<&'a PerLevel<Chop>>, i: usize) -> &'a ChopNew {
        lv.map_or(&*self.new, |l| &*l.at(i).new)
    }

    /// The place of the note's subchannel in its track's list (0 if absent).
    fn sub_place(&self, n: &TNote) -> i64 {
        self.subs
            .get(usize::from(n.track))
            .and_then(|t| t.iter().position(|s| s.0 == n.sub))
            .unwrap_or(0) as i64
    }

    /// n07: how far the slice grid moves back from its origin (ticks).
    fn phase_shift(&self, n: &TNote, info: &ChopInfo, unit: f64, period: f64) -> f64 {
        let Some((phi, by, reference, accumulate)) = &self.new.spiral else {
            return 0.0;
        };
        let steps = match by {
            0 => i64::from(n.pitch) - reference,
            1 => self.sub_place(n),
            _ => i64::from(info.rank),
        } as f64;
        let m = if *accumulate {
            period.max(1.0)
        } else {
            unit.max(1.0)
        };
        (phi * steps * unit).rem_euclid(m)
    }

    /// The unit of note `n` in ticks: the tool's → [a6 pitch density] → n09
    /// → n05 (a whole-source unit replaces it).
    fn unit_of(&self, n: &TNote, info: &ChopInfo) -> f64 {
        let mut unit = self.unit as f64;
        if let Some((pivot, amt)) = self.density {
            unit *= 2f64.powf(-(f64::from(n.pitch) - pivot) / 12.0 * amt);
        }
        if let (Some((step, factors, multiply)), Some(ctx)) = (&self.new.metric, &self.ctx) {
            let m = metric_position(&ctx.meter, n.start);
            let (slot, _) = m.slot(n.start, *step);
            let f = factors[(slot % factors.len() as u64) as usize];
            unit = if *multiply { unit * f } else { unit / f };
        }
        if info.unit > 0.0 {
            unit = info.unit;
        }
        unit.max(1.0)
    }

    /// Slice boundaries (absolute ticks) of note `n`, ascending, covering
    /// [s, e] (e = the fill's end with n08), each with its pattern slot; the
    /// last entry's slot is the pattern step after the note (n11 carries
    /// it).
    fn bounds(&self, n: &TNote, info: &ChopInfo, cuts: &[u64]) -> Vec<(u64, usize)> {
        let (s, e) = (n.start, n.end().max(info.fill_end));
        let k0 = info.k0 as usize;
        // n10: the reference onsets replace the pattern.
        if self.new.onset.is_some() {
            let c = &cuts[info.cuts.0..info.cuts.1];
            let mut out = vec![(s, k0)];
            out.extend(c.iter().enumerate().map(|(k, &t)| (t, k0 + k + 1)));
            out.push((e, k0 + c.len() + 1));
            out.dedup_by_key(|x| x.0);
            return out;
        }
        // n02: slices at a real-time rate replace the pattern.
        if let (Some((ms, from, to, curve, along)), Some(ctx)) = (&self.new.rate, &self.ctx) {
            let mut tt = TempoTime::new(ctx);
            let (lo, hi) = (tt.micros_at(s) as f64, tt.micros_at(e) as f64);
            let (sel_lo, sel_hi) = self.sel_us;
            let rate = |pos: f64| {
                let x = if *along {
                    let at = lo + pos * (hi - lo).max(1.0);
                    ((at - sel_lo) / (sel_hi - sel_lo).max(1.0)).clamp(0.0, 1.0)
                } else {
                    pos
                };
                let v = from + (to - from) * curve.eval(x);
                if *ms { 1000.0 / v.max(0.1) } else { v }
            };
            let mut out: Vec<(u64, usize)> = Vec::new();
            for t in tt.rate_steps(s, e, rate) {
                if out
                    .last()
                    .is_some_and(|l| t < l.0.saturating_add(self.min_gap))
                {
                    continue;
                }
                out.push((t, k0 + out.len()));
            }
            out.push((e, k0 + out.len()));
            out.dedup_by_key(|x| x.0);
            return out;
        }
        let unit = self.unit_of(n, info);
        // n01: onsets of the Euclidean pattern on the unit grid.
        if let Some((eu, by)) = &self.new.euclid {
            let steps = eu.steps() as i64;
            let rot = match by {
                1 => info.index as i64,
                2 => i64::from(n.pitch),
                3 => self.sub_place(n),
                _ => 0,
            } + k0 as i64;
            let period = unit * steps as f64;
            let origin = if self.absolute {
                let p = period.max(1.0);
                ((s as f64 / p).floor() * p) as u64
            } else {
                s
            } as f64
                - self.phase_shift(n, info, unit, period);
            let first = ((s as f64 - origin) / unit).floor().max(0.0) as u64;
            let mut out = vec![(s, k0 + first as usize)];
            let mut k = first;
            loop {
                let t = origin + k as f64 * unit;
                if t >= e as f64 || k - first >= 1_000_000 {
                    break;
                }
                let tb = t.round() as u64;
                if tb > s && tb < e && eu.hit((k as i64 + rot).rem_euclid(steps) as u64) {
                    out.push((tb, k0 + k as usize));
                }
                k += 1;
            }
            out.push((e, k0 + k as usize));
            out.dedup_by_key(|x| x.0);
            return out;
        }
        let t = pos(n, &self.stats);
        let lens: Vec<f64> = match &self.b {
            Some((b, c)) => {
                let w = c.eval(t);
                let m = self.a.len().max(b.len());
                (0..m)
                    .map(|i| {
                        let x = self.a[i % self.a.len()] as f64;
                        let y = b[i % b.len()] as f64;
                        x + (y - x) * w
                    })
                    .collect()
            }
            None => self.a.iter().map(|&x| x as f64).collect(),
        };
        let period: f64 = lens.iter().sum::<f64>() * unit;
        let origin = if self.absolute {
            let p = period.max(1.0);
            ((s as f64 / p).floor() * p) as u64
        } else {
            s
        };
        let mut out = vec![(s, k0)];
        let mut t = origin as f64 - self.phase_shift(n, info, unit, period);
        let mut k = 0usize;
        let mut guard = 0u32;
        while t < e as f64 && guard < 1_000_000 {
            let slot = (k + k0) % lens.len();
            t += lens[slot] * unit;
            k += 1;
            guard += 1;
            let tb = t.round() as u64;
            if tb > s && tb < e {
                out.push((tb, k0 + k));
            }
        }
        out.push((e, k0 + k));
        out.dedup_by_key(|x| x.0);
        out
    }

    fn sub_for(&self, n: &TNote, k: usize) -> u32 {
        if self.sub_seq.is_empty() {
            return n.sub;
        }
        let Some(t) = self.subs.get(usize::from(n.track)) else {
            return n.sub;
        };
        if t.is_empty() {
            return n.sub;
        }
        let base = t.iter().position(|s| s.0 == n.sub).unwrap_or(0) as i64;
        let step = self.sub_seq[k % self.sub_seq.len()];
        t[(base + step).rem_euclid(t.len() as i64) as usize].0
    }

    fn map(&self, n: &TNote, info: &ChopInfo, cuts: &[u64], out: &mut Output) {
        let mut b = self.bounds(n, info, cuts);
        let end = b.last().map_or(n.end(), |x| x.0);
        // Holds: a slice marked "hold" joins the next one.
        if self.holds.contains(&1) {
            let mut merged = vec![b[0]];
            for w in b.windows(2) {
                let slot = w[0].1 % self.holds.len().max(1);
                if self.holds.get(slot) == Some(&1) && w[1].0 != end {
                    continue;
                }
                merged.push(w[1]);
            }
            b = merged;
        }
        let mut pieces: Vec<(u64, u64, usize)> =
            b.windows(2).map(|w| (w[0].0, w[1].0, w[0].1)).collect();
        if let Some(a) = self.warp {
            let (s, e) = (n.start as f64, end as f64);
            let span = (e - s).max(1.0);
            let m = |x: u64| (s + warp((x as f64 - s) / span, a) * span).round() as u64;
            pieces = pieces
                .into_iter()
                .map(|(x, y, k)| (m(x), m(y), k))
                .collect();
        }
        if let Some(mode) = self.mirror
            && pieces.len() > 1
        {
            let reverse = mode == 0 || (hash(&[note_key(n)]) & 1 == 1);
            if reverse {
                let lens: Vec<u64> = pieces.iter().rev().map(|p| p.1 - p.0).collect();
                let mut t = n.start;
                for (i, p) in pieces.iter_mut().enumerate() {
                    p.0 = t;
                    t += lens[i];
                    p.1 = t;
                }
            }
        }
        let parent = if self.group || self.child.is_some() {
            (note_key(n) as u32) | 1
        } else {
            n.group
        };
        // n04: k voices of the chord sound per slice, in the hocket order.
        let hocket = self
            .new
            .hocket
            .as_ref()
            .filter(|_| info.size >= 2)
            .map(|h| (h.0.min(info.size), info.size, info.voice));
        let count = pieces.len();
        for (i, (x, y, k)) in pieces.into_iter().enumerate() {
            if y <= x {
                continue;
            }
            // V09-0143: the driver dimension is the piece (0 = the note's
            // first).
            out.set_dim(i as u32);
            if let Some((voices, size, voice)) = hocket {
                let first = (i as u64 * u64::from(voices)) % u64::from(size);
                if (u64::from(voice) + u64::from(size) - first) % u64::from(size)
                    >= u64::from(voices)
                {
                    continue;
                }
            }
            self.emit_piece(n, info, out, x, y, k, i, count, parent, 0);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_piece(
        &self,
        n: &TNote,
        info: &ChopInfo,
        out: &mut Output,
        x: u64,
        y: u64,
        k: usize,
        i: usize,
        count: usize,
        parent: u32,
        depth: u32,
    ) {
        if let Some((max, parts)) = self.depth
            && depth < max
            && y - x >= parts
        {
            let len = (y - x) / parts;
            for j in 0..parts {
                let a = x + j * len;
                let b = if j + 1 == parts { y } else { a + len };
                self.emit_piece(n, info, out, a, b, k, i, count, parent, depth + 1);
            }
            return;
        }
        if let Some(c) = &self.keep {
            let mut r = Rng::new(hash(&[self.seed, salt::KEEP, note_key(n), x]));
            if !r.chance(c.eval(pos(n, &self.stats))) {
                return;
            }
        }
        let slot = k % self.vels.len().max(1);
        let pv = self.vels.get(slot).copied().unwrap_or(100) as f64 / 100.0;
        let mut base = f64::from(n.vel);
        // n06: the note's velocity ramps toward the next note's.
        if let (Some((_, shape, amount)), Some(next)) = (&self.new.bridge, info.next_vel) {
            let u = (x.saturating_sub(n.start) as f64 / n.dur.max(1) as f64).clamp(0.0, 1.0);
            base += amount * shape.eval(u) * (f64::from(next) - base);
        }
        let mut vel = base * (1.0 - self.levels + self.levels * pv);
        if let Some(c) = &self.vcurve {
            // (a7's warp can round a far-end slice start below the note's)
            let u = x.saturating_sub(n.start) as f64 / n.dur.max(1) as f64;
            vel *= c.eval(u).max(0.01);
        }
        // n08: slices in the rest fade along the fill.
        if let Some((_, _, fade, _)) = &self.new.fill
            && info.fill_end > n.end()
            && x >= n.end()
        {
            let u = (x - n.end()) as f64 / (info.fill_end - n.end()) as f64;
            vel *= fade.eval(u).max(0.0);
        }
        let tr = self
            .trans
            .get(k % self.trans.len().max(1))
            .copied()
            .unwrap_or(0);
        // n03: the slices step toward the target interval.
        let mut pitch = i64::from(n.pitch);
        if let Some((interval, path, lattice, shape)) = &self.new.gliss {
            let f = shape.eval(if count > 1 {
                i as f64 / (count - 1) as f64
            } else {
                0.0
            });
            pitch = match path {
                1 => {
                    let steps = lattice.steps_between(pitch, pitch + interval);
                    let k = (steps as f64 * f).round() as i64;
                    if k == 0 {
                        pitch
                    } else {
                        lattice.step(pitch, k)
                    }
                }
                2 => pitch + 12 * (*interval as f64 / 12.0 * f).round() as i64,
                _ => pitch + (*interval as f64 * f).round() as i64,
            };
        }
        let mut tpl = *n;
        tpl.sub = self.sub_for(n, i);
        if let Some(sub) = self.subs.get(usize::from(n.track))
            && let Some(ch) = sub.iter().find(|s| s.0 == tpl.sub).map(|s| s.1)
            && tpl.sub != n.sub
        {
            tpl.ch = ch;
        }
        tpl.group = match self.child {
            Some(size) => parent ^ ((i as u64 / size.max(1)) as u32).wrapping_mul(0x9E37_79B9) | 1,
            None => parent,
        };
        let mut dur = y - x;
        if let Some(g) = self.gate_preserve
            && i + 1 < count
        {
            dur = ((dur as f64) * g / 100.0).round().max(1.0) as u64;
        }
        out.emit(
            i128::from(x),
            i128::from(dur),
            pitch + tr,
            vel.round() as i64,
            &tpl,
        );
    }

    /// The per-note values the whole-source functions need.
    #[allow(clippy::too_many_lines)]
    fn infos(&self, src: &Source, lv: Option<&PerLevel<Chop>>) -> (Vec<ChopInfo>, Vec<u64>) {
        let n = src.notes.len();
        let mut info = vec![ChopInfo::default(); n];
        let mut cuts: Vec<u64> = Vec::new();
        let tracks = per_track(src);
        let new = &*self.new;
        if new.euclid.as_ref().is_some_and(|e| e.1 == 1) {
            for idx in &tracks {
                for (k, &i) in idx.iter().enumerate() {
                    info[i].index = k as u64;
                }
            }
        }
        // Chords: n04 (its tolerance) and n07 by chord rank (the same
        // tolerance, else onsets that start together).
        let tol = match &new.hocket {
            Some(h) => Some(h.3),
            None => new.spiral.as_ref().filter(|s| s.1 == 2).map(|_| 0),
        };
        if let Some(tol) = tol {
            let mut groups = ChordGroups::default();
            for idx in &tracks {
                groups.append(&chords(src, idx, tol));
            }
            let (r, sz) = groups.voice_ranks(&src.notes);
            for i in 0..n {
                info[i].rank = r[i].min(sz[i].saturating_sub(1));
                info[i].size = sz[i];
                if let Some((_, order, list, _)) = &new.hocket {
                    info[i].voice = chop_voice(*order, list, info[i].rank, sz[i]);
                }
            }
        }
        // n06 / n08: the next onset of the track or the next note of the key.
        let by_track = (new.bridge.as_ref().is_some_and(|b| !b.0)
            || new.fill.as_ref().is_some_and(|f| !f.3))
        .then(|| {
            let mut v: Vec<(u16, u64, u8, u8)> = src
                .notes
                .iter()
                .map(|x| (x.track, x.start, x.pitch, x.vel))
                .collect();
            v.sort_unstable();
            v
        });
        let by_key = (new.bridge.as_ref().is_some_and(|b| b.0)
            || new.fill.as_ref().is_some_and(|f| f.3))
        .then(|| {
            let mut v: Vec<(u16, u8, u64, u8)> = src
                .notes
                .iter()
                .map(|x| (x.track, x.pitch, x.start, x.vel))
                .collect();
            v.sort_unstable();
            v
        });
        // (next onset tick, the velocity of its note nearest in pitch / of
        // the key's next note)
        let next_of = |x: &TNote, same_key: bool| -> Option<(u64, u8)> {
            if same_key {
                let v = by_key.as_ref()?;
                let p = v.partition_point(|a| (a.0, a.1, a.2) <= (x.track, x.pitch, x.start));
                v.get(p)
                    .filter(|a| (a.0, a.1) == (x.track, x.pitch))
                    .map(|a| (a.2, a.3))
            } else {
                let v = by_track.as_ref()?;
                let p = v.partition_point(|a| (a.0, a.1) <= (x.track, x.start));
                let &(tr, at, _, _) = v.get(p)?;
                if tr != x.track {
                    return None;
                }
                let g = &v[p..p + v[p..].partition_point(|a| a.0 == tr && a.1 == at)];
                let k = g.partition_point(|a| a.2 < x.pitch);
                let mut best: Option<(u8, u8)> = None;
                for c in [k.checked_sub(1), Some(k)].into_iter().flatten() {
                    if let Some(a) = g.get(c) {
                        let d = a.2.abs_diff(x.pitch);
                        if best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, a.3));
                        }
                    }
                }
                best.map(|b| (at, b.1))
            }
        };
        for (i, x) in src.notes.iter().enumerate() {
            if let Some((same_key, _, _)) = &new.bridge {
                info[i].next_vel = next_of(x, *same_key).map(|v| v.1);
            }
            if let Some((_, below, _, same_key)) = &new.fill
                && let Some((at, _)) = next_of(x, *same_key)
            {
                let longest = self.nw(lv, i).fill.as_ref().map_or(0, |f| f.0);
                let rest = at.saturating_sub(x.end());
                if rest > 0 && rest < *below {
                    info[i].fill_end = at.min(x.end().saturating_add(longest));
                }
            }
        }
        // n05: each note's unit from the target NPS over the polyphony
        // around it (the time-weighted mean of sounding selected notes over
        // the window centred on the note, or the note when longer).
        if let (Some((_, curve, window, most)), Some(ctx)) = (&new.target, &self.ctx) {
            let mut tt = TempoTime::new(ctx);
            let spans: Vec<(f64, f64)> = src
                .notes
                .iter()
                .map(|x| (tt.micros_at(x.start) as f64, tt.micros_at(x.end()) as f64))
                .collect();
            let mut ev: Vec<(f64, i64)> =
                spans.iter().flat_map(|&(a, b)| [(a, 1), (b, -1)]).collect();
            ev.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let mut times = Vec::with_capacity(ev.len());
            let mut area = Vec::with_capacity(ev.len());
            let mut after = Vec::with_capacity(ev.len());
            let (mut cur, mut acc) = (0i64, 0.0f64);
            let mut last = ev.first().map_or(0.0, |e| e.0);
            for (t, d) in ev {
                acc += cur as f64 * (t - last);
                last = t;
                cur += d;
                times.push(t);
                area.push(acc);
                after.push(cur);
            }
            let area_at = |t: f64| {
                let j = times.partition_point(|&x| x <= t);
                if j == 0 {
                    0.0
                } else {
                    area[j - 1] + after[j - 1] as f64 * (t - times[j - 1])
                }
            };
            let mean_len = match &new.euclid {
                Some((eu, _)) => eu.steps() as f64 / eu.pulses().max(1) as f64,
                None => self.a.iter().sum::<i64>() as f64 / self.a.len().max(1) as f64,
            };
            for (i, x) in src.notes.iter().enumerate() {
                let (a, b) = spans[i];
                let c = (a + b) / 2.0;
                let (lo, hi) = (a.min(c - window / 2.0), b.max(c + window / 2.0));
                let poly = ((area_at(hi) - area_at(lo)) / (hi - lo).max(1.0)).max(1e-9);
                let nps = self.nw(lv, i).target.as_ref().map_or(0.0, |t| t.0);
                let target = nps * curve.eval(pos(x, &self.stats)).max(0.0);
                let slices = (target * (b - a) / 1e6 / poly)
                    .round()
                    .clamp(1.0, f64::from(*most));
                info[i].unit = (x.dur as f64 / (slices * mean_len.max(1e-9))).max(1.0);
            }
        }
        // n10: each note's cuts at the reference onsets inside it.
        if let Some((mode, num, _, shortest)) = &new.onset {
            let starts = |it: &mut dyn Iterator<Item = u64>| -> Vec<u64> {
                let mut v: Vec<u64> = it.collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            let global = match mode {
                0 => starts(&mut src.notes.iter().map(|x| x.start)),
                3 => starts(&mut src.aux.first().into_iter().flatten().map(|x| x.start)),
                _ => Vec::new(),
            };
            for (slot, idx) in tracks.iter().enumerate() {
                let local = match mode {
                    1 => starts(
                        &mut src
                            .context
                            .iter()
                            .filter(|x| usize::from(x.track) == slot)
                            .map(|x| x.start),
                    ),
                    2 => match src.tracks.get(slot).and_then(|t| t.subs.get(num - 1)) {
                        Some(&(sub, _)) => starts(
                            &mut idx
                                .iter()
                                .map(|&i| &src.notes[i])
                                .chain(src.context.iter().filter(|x| usize::from(x.track) == slot))
                                .filter(|x| x.sub == sub)
                                .map(|x| x.start),
                        ),
                        None => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                let refs = if matches!(mode, 0 | 3) {
                    &global
                } else {
                    &local
                };
                for &i in idx {
                    let merge = self.nw(lv, i).onset.map_or(0, |o| o.2);
                    let merge = &merge;
                    let (s, e) = (src.notes[i].start, src.notes[i].end());
                    let from = cuts.len();
                    let (mut prev, mut cluster) = (s, None::<u64>);
                    for &t in &refs[refs.partition_point(|&t| t <= s)..] {
                        if t >= e {
                            break;
                        }
                        if cluster.is_some_and(|c| t - c < *merge) {
                            continue;
                        }
                        cluster = Some(t);
                        if t - prev >= *shortest && e - t >= *shortest {
                            cuts.push(t);
                            prev = t;
                        }
                    }
                    info[i].cuts = (from, cuts.len());
                }
            }
        }
        // n11: the pattern position runs on from note to note per track,
        // key or subchannel (a chord's notes start together), resetting at
        // bar lines or after long rests.
        if let Some((per, reset, rest)) = &new.running {
            // key → (k at the chain's last start, k after it, last start,
            // latest end, bar)
            type Chain = (u64, u64, u64, u64, u64);
            let mut state: std::collections::HashMap<(u16, u32), Chain> =
                std::collections::HashMap::new();
            for (i, x) in src.notes.iter().enumerate() {
                let key = (
                    x.track,
                    match per {
                        1 => u32::from(x.pitch),
                        2 => x.sub,
                        _ => 0,
                    },
                );
                let bar = match (&self.ctx, reset) {
                    (Some(ctx), 1) => metric_position(&ctx.meter, x.start).bar,
                    _ => 0,
                };
                let (k0, chord) = match state.get(&key) {
                    Some(&(k_last, _, at, _, _)) if at == x.start => (k_last, true),
                    Some(&(_, k_after, _, end, b)) => {
                        let fresh = match reset {
                            1 => bar != b,
                            2 => x.start.saturating_sub(end) > *rest,
                            _ => false,
                        };
                        (if fresh { 0 } else { k_after }, false)
                    }
                    None => (0, false),
                };
                info[i].k0 = k0;
                let after = self
                    .bounds(x, &info[i], &cuts)
                    .last()
                    .map_or(k0, |l| l.1 as u64);
                let st = state.entry(key).or_insert((0, 0, 0, 0, 0));
                *st = if chord {
                    (k0, st.1.max(after), x.start, st.3.max(x.end()), bar)
                } else {
                    (k0, after, x.start, x.end(), bar)
                };
            }
        }
        (info, cuts)
    }
}

/// Reads what the Chopper's n10 needs besides the selection: the track's
/// unselected notes (a subchannel or the track's notes) or every track's
/// notes around the selection (all tracks).
pub fn chop_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("n10") || src.stats.is_empty() {
        return Ok(());
    }
    match p.choice("n10.ref") {
        1 | 2 if src.context.is_empty() => {
            let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
            *src = s.with_context(doc);
        }
        3 => {
            let (a, b) = (src.stats.min_start, src.stats.max_end);
            let mut layer = Vec::new();
            // B2-F16: every track as the song shows it, on these ticks.
            let shift = ctx.map_or(0, |c| c.shift);
            for i in 0..doc.track_count() {
                super::super::source::placed_query(doc, i, shift, a, b, &mut |r| {
                    if r.start_tick >= a && r.start_tick < b && layer.len() < limit {
                        layer.push(TNote::from_record(&r, u16::MAX));
                    }
                });
            }
            src.aux = std::sync::Arc::new(vec![layer]);
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
/// The Chopper's settings from `p` and the bound of its output.
fn chop_of(p: &Params, b: &BuildCtx) -> (Chop, u64) {
    let (num, den) = time_mul(p.choice("mul"));
    let unit = (note_value(p.choice("unit"), p.ticks("unit.ticks"), b.ctx) * num / den).max(1);
    let custom = p.list("custom");
    let mut new = ChopNew::from(p);
    if let Some(m) = &mut new.metric {
        let grid = p.choice("n09.grid").min(art_grid_values().len() - 1);
        m.0 = note_value(grid, 1, b.ctx).max(1);
    }
    let tt = TempoTime::new(b.ctx);
    let max_hz = new.rate.as_ref().map(|(ms, from, to, _, _)| {
        if *ms {
            1000.0 / from.min(*to).max(0.1)
        } else {
            from.max(*to)
        }
    });
    let min_gap = max_hz.map_or(1, |hz| tt.min_ticks_per_step(hz));
    let sel_us = if new.rate.is_some() && !b.stats.is_empty() {
        let mut tt = TempoTime::new(b.ctx);
        (
            tt.micros_at(b.stats.min_start) as f64,
            tt.micros_at(b.stats.max_end) as f64,
        )
    } else {
        (0.0, 0.0)
    };
    let c = Chop {
        a: builtin_chop(p.choice("pattern"), &custom),
        b: p.adv(1)
            .then(|| (builtin_chop(p.choice("a1.b"), &custom), p.curve("a1.curve"))),
        vels: p.list("pvel"),
        trans: p.list("ptrans"),
        holds: p.list("holds"),
        unit,
        levels: p.float("levels"),
        absolute: p.flag("absolute"),
        group: p.flag("group"),
        seed: p.seed("seed"),
        depth: p
            .adv(2)
            .then(|| (p.int("a2.depth") as u32, p.int("a2.parts") as u64)),
        keep: p.adv(3).then(|| p.curve("a3.curve")),
        sub_seq: if p.adv(4) {
            p.list("a4.seq")
        } else {
            Vec::new()
        },
        gate_preserve: p.adv(5).then(|| p.float("a5.gate")),
        density: p
            .adv(6)
            .then(|| (p.int("a6.pivot") as f64, p.float("a6.amount"))),
        warp: p.adv(7).then(|| -3.93 * p.float("a7.dist") / 100.0),
        vcurve: p.adv(8).then(|| p.curve("a8.curve")),
        mirror: p.adv(9).then(|| p.choice("a9.mode")),
        child: p.adv(10).then(|| p.int("a10.size") as u64),
        stats: b.stats.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        ctx: new.any().then(|| std::sync::Arc::new(b.ctx.clone())),
        new: std::sync::Arc::new(new),
        sel_us,
        min_gap,
    };
    // Bound: Σ slices over every note (finest slice) × recursion.
    let finest = match c.density {
        Some((_, amt)) => (unit as f64 * 2f64.powf(-(127.0 / 12.0) * amt.abs())).max(1.0) as u64,
        None => unit,
    };
    let min_len = c.a.iter().copied().min().unwrap_or(1).max(1) as u64;
    let mut total = if !c.new.any() {
        b.stats
            .steps_bound(finest.saturating_mul(min_len))
            .saturating_add(b.stats.count as u64)
    } else {
        let count = b.stats.count as u64;
        // n09 divides the unit by at most its largest factor.
        let finest = match &c.new.metric {
            Some((_, f, false)) => {
                (finest as f64 / f.iter().copied().fold(1.0, f64::max)).max(1.0) as u64
            }
            _ => finest,
        };
        // The finest slice of the mechanism that places the boundaries.
        let step = match (&c.new.onset, &c.new.rate, &c.new.euclid) {
            (Some((_, _, _, shortest)), _, _) => (*shortest).max(1),
            (None, Some(_), _) => min_gap.max(1),
            (None, None, Some(_)) => finest,
            _ => finest.saturating_mul(min_len),
        };
        let mut t = if c.new.onset.is_none()
            && c.new.rate.is_none()
            && let Some((_, _, _, most)) = &c.new.target
        {
            // n05 sets each note's unit: its slice count ≤ most × the
            // pattern's longest / shortest slice + one pattern.
            let (cycle, ratio) = match &c.new.euclid {
                Some((eu, _)) => (eu.steps() as u64, eu.steps() as u64),
                None => {
                    let max_len = c.a.iter().copied().max().unwrap_or(1).max(1) as u64;
                    (c.a.len() as u64, max_len.div_ceil(min_len))
                }
            };
            count.saturating_mul(
                u64::from(*most)
                    .saturating_mul(ratio)
                    .saturating_add(cycle)
                    .saturating_add(2),
            )
        } else {
            b.stats.steps_bound(step)
        };
        // A moved grid (n07, n11) adds a partial slice per note.
        t = t.saturating_add(count.saturating_mul(2));
        if let Some((longest, below, _, _)) = &c.new.fill {
            let fill = (*longest).min(*below);
            // With n10 the fill is the last slice made longer (no cuts in
            // the rest); n05's unit is at least one tick.
            let per = if c.new.onset.is_some() {
                0
            } else if c.new.target.is_some() && c.new.rate.is_none() {
                fill
            } else {
                fill / step.max(1)
            };
            t = t.saturating_add(count.saturating_mul(per.saturating_add(2)));
        }
        t
    };
    if let Some((d, parts)) = c.depth {
        total = total.saturating_mul(parts.pow(d));
    }
    (c, total)
}

fn chop_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let (c, mut total) = chop_of(p, b);
    // With Control Sweeps (E0422) each note takes its level's settings
    // (the passes see the whole selection); the bound is the largest over
    // the swept ranges' corners.
    let swept = Rebuild::of(p, b);
    if let Some(rb) = &swept {
        for (_, t) in rb.corners(b.ctx, chop_of) {
            total = total.max(t);
        }
    }
    if c.new.contextual() || swept.is_some() {
        let c = std::sync::Arc::new(c);
        let sctx = b.ctx.clone();
        boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                let lv: Option<PerLevel<Chop>> = swept
                    .as_ref()
                    .map(|r| r.levels(&sctx, src, |p, b| chop_of(p, b).0));
                let (info, cuts) = c.infos(src, lv.as_ref());
                for (i, n) in src.notes.iter().enumerate() {
                    let me = lv.as_ref().map_or(&*c, |l| l.at(i));
                    me.map(n, &info[i], &cuts, out);
                    out.check()?;
                }
                Ok(())
            },
            growth: Growth::Generate(total),
            estimate: None,
        })
    } else {
        boxed(Pointwise {
            f: move |n: &TNote, out: &mut Output| c.map(n, &ChopInfo::default(), &[], out),
            ahead: 0,
            behind: 0,
            growth: Growth::Generate(total),
        })
    }
}

// ================================================================ Quantizer

pub const QUANTIZE: ToolDef = ToolDef {
    id: "quantize",
    spec: qnt_spec,
    build: qnt_build,
};

const QNT_MODES: &[&str] = &[
    "Quantize duration",
    "Quantize end time",
    "Leave duration",
    "Leave end time",
];
const QNT_FRAMES: &[&str] = &[
    "24 fps",
    "25 fps",
    "30 fps",
    "50 fps",
    "60 fps",
    "120 fps",
    "Milliseconds",
];
/// Frame rates of n02's fps choices (the last choice is milliseconds).
const QNT_FPS: [f64; 6] = [24.0, 25.0, 30.0, 50.0, 60.0, 120.0];
const QNT_FREE: &[&str] = &[
    "Nearest free step",
    "Next free step",
    "Previous free step",
    "Leave unquantized",
];
const QNT_CLUSTER: &[&str] = &["Earliest", "Loudest", "Centroid", "Latest"];
const QNT_BETWEEN: &[&str] = &["Octave steps", "Linear"];
const QNT_GROOVE_VEL: &[&str] = &["Keep", "Take from the groove"];
const QNT_ANCHOR: &[&str] = &["Song start", "Every bar", "Every beat", "Every marker"];
const QNT_REANCHOR: &[&str] = &["First note", "Every phrase"];
/// Most points n05's elastic grid may have across the selection.
pub const QNT_MAX_ELASTIC: usize = 4_194_304;

/// Ticks of `units` 1/48-beat units.
fn units48(units: i64, ppq: u64) -> i128 {
    i128::from(units) * i128::from(ppq) / 48
}

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const QUANTIZE_HELP: &[(&str, &str)] = &[
    (
        "grid",
        "The grid note starts are pulled toward, as a note value.",
    ),
    ("grid.ticks", "For Custom: the grid step, in ticks."),
    (
        "groove",
        "Pulls the notes toward the onsets of a groove pattern below instead of an even grid.",
    ),
    (
        "groove.pos",
        "The groove's onsets within one groove length, in 1/48 of a beat (0 = the beat, 24 = half a beat later).",
    ),
    (
        "groove.beats",
        "How many beats one cycle of the groove lasts.",
    ),
    (
        "groove.vel",
        "The velocity of each groove onset in turn; Levels decides how much the notes take them.",
    ),
    (
        "start",
        "How far each start moves toward its grid point (0 % = unchanged, 100 % = exactly on it).",
    ),
    (
        "sens",
        "Only notes within this share of half a grid step from a grid point are moved (100 % = every note).",
    ),
    (
        "dur",
        "For the quantizing modes: how far each length or end moves toward the grid (0 % = unchanged, 100 % = fully).",
    ),
    (
        "mode",
        "Quantize Duration snaps lengths to the grid; Quantize End Time snaps the ends; Leave Duration keeps each length; Leave End Time keeps each end where it was.",
    ),
    (
        "levels",
        "Blends each note's velocity from its own (0) to its groove onset's velocity (1).",
    ),
    (
        "a1",
        "Quantizes dense passages harder (or softer) than sparse ones.",
    ),
    (
        "a1.window",
        "The time, in ticks, around each note over which density is measured.",
    ),
    (
        "a1.amount",
        "+ quantizes denser passages more strongly and sparse ones less; − the reverse.",
    ),
    (
        "a2",
        "Uses one strength for notes ahead of the grid and another for notes behind it.",
    ),
    (
        "a2.early",
        "The strength for notes that start before their grid point.",
    ),
    (
        "a2.late",
        "The strength for notes that start after their grid point.",
    ),
    (
        "a3",
        "Sets the start strength and the length strength by curves across the selection.",
    ),
    (
        "a3.start",
        "The start strength from the selection's start (left) to its end (right).",
    ),
    (
        "a3.gate",
        "The length strength from the selection's start (left) to its end (right).",
    ),
    (
        "a4",
        "Pulls each note toward the nearest point of several grids at once, for polyrhythms.",
    ),
    (
        "a4.grids",
        "The grid steps, in 1/48 of a beat (12 = sixteenths, 16 = sixteenth triplets).",
    ),
    (
        "a5",
        "Moves notes of each pitch class (C … B) by its own small offset after quantizing.",
    ),
    (
        "a5.offsets",
        "The offset for C, C♯ … B in order, in 1/48 of a beat (+ later, − earlier).",
    ),
    ("a6", "Blends the groove with a second groove (B)."),
    ("a6.pos", "The second groove's onsets, in 1/48 of a beat."),
    (
        "a6.mix",
        "How far the targets move from groove A (0) to groove B (1).",
    ),
    (
        "a7",
        "Moves every note of a chord by the same amount as its first note, so chords stay intact.",
    ),
    (
        "a7.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "a8",
        "Shifts the grid for each subchannel, so layers sit at different offsets.",
    ),
    (
        "a8.phase",
        "The grid offset for each subchannel in turn, in 1/48 of a beat (the list repeats).",
    ),
    (
        "a9",
        "Keeps part of each note's own timing feel after quantizing (its deviation minus the average deviation).",
    ),
    (
        "a9.keep",
        "How much of each note's deviation from the grid is kept (0 % = fully quantized).",
    ),
    (
        "a10",
        "Makes the start strength travel from one value to another across the selection.",
    ),
    ("a10.from", "The strength at the selection's start."),
    ("a10.to", "The strength at the selection's end."),
    ("n01", "Swings the grid: every other step is moved later."),
    (
        "n01.swing",
        "Where the off-beat falls within a pair of steps (50 % = straight, 66 % = triplet swing).",
    ),
    ("n01.unit", "The note value whose pairs are swung."),
    (
        "n01.curve",
        "How much of the swing applies across the selection (1 = all of it).",
    ),
    (
        "n02",
        "Snaps to a grid of video frames or milliseconds, in real time.",
    ),
    (
        "n02.grid",
        "The frame rate, or Milliseconds for a custom step.",
    ),
    ("n02.ms", "For Milliseconds: the grid step."),
    (
        "n02.phase",
        "Moves the real-time grid later by this many milliseconds.",
    ),
    (
        "n03",
        "Makes sure no two notes of the same key land on the same grid step; later ones look for a free step.",
    ),
    (
        "n03.free",
        "Where a colliding note goes: the nearest free step, the next free step, the previous one, or it stays unquantized.",
    ),
    ("n03.radius", "How many grid steps away a free step may be."),
    (
        "n03.key",
        "What counts as the same note: the key, the key on the same channel, or the key on the same subchannel.",
    ),
    (
        "n04",
        "Snaps notes that start close together onto one shared start.",
    ),
    (
        "n04.window",
        "Notes starting within this many ticks of a cluster's first note join the cluster.",
    ),
    (
        "n04.target",
        "Where the cluster lands: on its earliest note, its loudest, its average time, or its latest.",
    ),
    (
        "n04.largest",
        "A cluster holds at most this many notes; more start a new cluster.",
    ),
    (
        "n05",
        "Uses a grid whose spacing changes across the selection (accelerating or slowing).",
    ),
    (
        "n05.from",
        "The grid spacing, in ticks, at the selection's start.",
    ),
    (
        "n05.to",
        "The grid spacing, in ticks, at the selection's end.",
    ),
    (
        "n05.curve",
        "How the spacing changes from the start to the end.",
    ),
    (
        "n06",
        "Gives low and high notes different grids: one at a low key, another at a high key.",
    ),
    (
        "n06.lo",
        "The low key where Grid at the Low Pivot applies (and below).",
    ),
    (
        "n06.hi",
        "The high key where Grid at the High Pivot applies (and above).",
    ),
    ("n06.low", "The grid step, in ticks, at the low pivot."),
    ("n06.high", "The grid step, in ticks, at the high pivot."),
    (
        "n06.between",
        "Octave Steps changes the grid by the same ratio every octave; Linear changes it evenly by semitone.",
    ),
    (
        "n07",
        "Learns a groove from the selection itself and pulls every note toward it.",
    ),
    ("n07.beats", "How many beats the learned groove spans."),
    ("n07.slots", "How many slots each beat is divided into."),
    (
        "n07.vel",
        "Keep each note's velocity, or take its slot's average velocity.",
    ),
    (
        "n07.strength",
        "How far each note moves toward its slot's average timing (and velocity).",
    ),
    (
        "n08",
        "Measures the grid from bar lines, beats or markers, with its own grid in odd meters.",
    ),
    (
        "n08.anchor",
        "Where the grid restarts: the song's start, every bar, every beat, or every marker.",
    ),
    (
        "n08.grid",
        "The grid used in bars of the odd meters listed below.",
    ),
    (
        "n08.odd",
        "The time-signature numerators that count as odd meters (5, 7 …).",
    ),
    (
        "n09",
        "Pulls notes toward the nearest onset of a reference instead of the grid.",
    ),
    (
        "n09.ref",
        "Where the reference notes come from: the track's unselected notes, a subchannel of the track, or another track.",
    ),
    (
        "n09.num",
        "The reference's number: a subchannel number for A Subchannel, a track number for A Track.",
    ),
    (
        "n09.dist",
        "Only notes at most this many ticks from a reference onset are pulled.",
    ),
    (
        "n09.strength",
        "How far a pulled note moves toward its reference onset.",
    ),
    (
        "n10",
        "Cleans up the gaps between onsets so each is a whole multiple of a unit.",
    ),
    (
        "n10.unit",
        "The unit the gaps between onsets are measured in, in ticks.",
    ),
    ("n10.mult", "The multiples of the unit a gap may be."),
    (
        "n10.anchor",
        "Where the cleaned rhythm is anchored: the selection's first note, or the start of every phrase.",
    ),
    ("n11", "Snaps velocities to a small set of levels."),
    (
        "n11.levels",
        "How many evenly spaced velocity levels there are (used when the list below is empty).",
    ),
    (
        "n11.allowed",
        "Your own velocity levels (empty = the evenly spaced levels).",
    ),
    (
        "n11.round",
        "Snap to the nearest level, the next lower, or the next higher.",
    ),
    (
        "n11.strength",
        "How far each velocity moves toward its level (100 % = onto it).",
    ),
];

#[allow(clippy::too_many_lines)]
fn qnt_spec() -> ToolSpec {
    let mut p = vec![
        choice("grid", "Grid", NOTE_VALUES, 4),
        ticks("grid.ticks", "Custom grid", 1, 1 << 40, 240),
        flag("groove", "Use groove template", false),
        list(
            "groove.pos",
            "Groove onsets (1/48 beat)",
            0,
            48 * 64,
            512,
            &[0, 14, 24, 38],
        ),
        int("groove.beats", "Groove length (beats)", 1, 64, 1),
        list(
            "groove.vel",
            "Groove velocities",
            1,
            127,
            512,
            &[110, 70, 100, 70],
        ),
        float(
            "start",
            "Start time (original → quantized)",
            0.0,
            100.0,
            100.0,
            "%",
        ),
        float("sens", "Sensitivity", 1.0, 100.0, 100.0, "%"),
        float(
            "dur",
            "Duration (original → quantized)",
            0.0,
            100.0,
            100.0,
            "%",
        ),
        choice("mode", "Mode", QNT_MODES, 2),
        float("levels", "Levels (original → groove)", 0.0, 1.0, 0.0, ""),
        adv_switch(1, "Adaptive strength by density"),
        ticks("a1.window", "Density window", 1, 1 << 40, 960).adv(1),
        float(
            "a1.amount",
            "Denser → stronger (+) / weaker (−)",
            -1.0,
            1.0,
            0.5,
            "",
        )
        .adv(1),
        adv_switch(2, "Separate early / late strengths"),
        float("a2.early", "Early notes", 0.0, 100.0, 100.0, "%").adv(2),
        float("a2.late", "Late notes", 0.0, 100.0, 40.0, "%").adv(2),
        adv_switch(3, "Onset vs gate curves"),
        curve(
            "a3.start",
            "Start strength across the selection",
            Curve::line(0.2, 1.0),
        )
        .adv(3),
        curve(
            "a3.gate",
            "Gate strength across the selection",
            Curve::line(1.0, 0.2),
        )
        .adv(3),
        adv_switch(4, "Multi-grid (polyrhythmic)"),
        list("a4.grids", "Grids (1/48 beat)", 1, 48 * 16, 8, &[12, 16]).adv(4),
        adv_switch(5, "Timing offsets per pitch class"),
        list(
            "a5.offsets",
            "C … B offsets (1/48 beat)",
            -48,
            48,
            12,
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .adv(5),
        adv_switch(6, "Groove interpolation (A ↔ B)"),
        list(
            "a6.pos",
            "Groove B onsets (1/48 beat)",
            0,
            48 * 64,
            512,
            &[0, 12, 24, 36],
        )
        .adv(6),
        float("a6.mix", "A → B", 0.0, 1.0, 0.5, "").adv(6),
        adv_switch(7, "Chord lock"),
        ticks("a7.tol", "Chord tolerance", 0, 1 << 30, 10).adv(7),
        adv_switch(8, "Subchannel phase"),
        list(
            "a8.phase",
            "Grid offset by subchannel (1/48 beat)",
            -48 * 4,
            48 * 4,
            4096,
            &[0, 6],
        )
        .adv(8),
        adv_switch(9, "Error-distribution preservation"),
        float("a9.keep", "Expression kept", 0.0, 100.0, 50.0, "%").adv(9),
        adv_switch(10, "Progressive strength envelope"),
        float("a10.from", "Strength at start", 0.0, 100.0, 0.0, "%").adv(10),
        float("a10.to", "Strength at end", 0.0, 100.0, 100.0, "%").adv(10),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Swing", false),
        float("n01.swing", "Swing amount", 50.0, 80.0, 66.0, "%"),
        choice("n01.unit", "Swing unit", art_grid_values(), 3),
        curve("n01.curve", "Swing over the selection", Curve::flat(1.0)),
        flag("n02", "Frame grid", false),
        choice("n02.grid", "Frame grid", QNT_FRAMES, 4),
        float("n02.ms", "Milliseconds", 0.1, 1000.0, 10.0, "ms"),
        float("n02.phase", "Phase", 0.0, 1000.0, 0.0, "ms"),
        flag("n03", "Collision-free slots", false),
        choice("n03.free", "Free step", QNT_FREE, 0),
        int("n03.radius", "Search radius (grid steps)", 1, 64, 4),
        choice("n03.key", "Same key means", ART_SAME_KEY, 0),
        flag("n04", "Cluster snap", false),
        ticks("n04.window", "Cluster window", 0, 1 << 30, 40),
        choice("n04.target", "Target", QNT_CLUSTER, 0),
        int("n04.largest", "Largest cluster", 2, 100_000, 128),
        flag("n05", "Elastic grid", false),
        ticks("n05.from", "Start spacing", 1, 1 << 40, 480),
        ticks("n05.to", "End spacing", 1, 1 << 40, 120),
        curve("n05.curve", "Spacing curve", Curve::line(0.0, 1.0)),
        flag("n06", "Register grid", false),
        pitch("n06.lo", "Low pivot", 48),
        pitch("n06.hi", "High pivot", 84),
        ticks("n06.low", "Grid at the low pivot", 1, 1 << 40, 960),
        ticks("n06.high", "Grid at the high pivot", 1, 1 << 40, 120),
        choice("n06.between", "Between the pivots", QNT_BETWEEN, 0),
        flag("n07", "Self-groove", false),
        int("n07.beats", "Groove length (beats)", 1, 16, 4),
        int("n07.slots", "Slots per beat", 1, 48, 4),
        choice("n07.vel", "Velocities", QNT_GROOVE_VEL, 0),
        float("n07.strength", "Groove strength", 0.0, 100.0, 100.0, "%"),
        flag("n08", "Meter-aware anchor", false),
        choice("n08.anchor", "Anchor", QNT_ANCHOR, 1),
        choice("n08.grid", "Grid in odd meters", art_grid_values(), 3),
        list(
            "n08.odd",
            "Odd meters (numerators)",
            1,
            64,
            64,
            &[5, 7, 9, 11, 13, 15],
        ),
        flag("n09", "Onset magnet", false),
        choice("n09.ref", "Reference", ART_REFERENCE, 0),
        int(
            "n09.num",
            "Reference subchannel or track (number)",
            1,
            4096,
            1,
        ),
        ticks("n09.dist", "Largest distance", 0, 1 << 40, 120),
        float("n09.strength", "Magnet strength", 0.0, 100.0, 100.0, "%"),
        flag("n10", "IOI ratio cleanup", false),
        ticks("n10.unit", "Unit", 1, 1 << 40, 240),
        list(
            "n10.mult",
            "Allowed multiples",
            1,
            64,
            64,
            &[1, 2, 3, 4, 6, 8],
        ),
        choice("n10.anchor", "Re-anchor", QNT_REANCHOR, 1),
        flag("n11", "Velocity level snap", false),
        int("n11.levels", "Levels", 2, 127, 8),
        list(
            "n11.allowed",
            "Allowed velocities (empty: the levels)",
            1,
            127,
            127,
            &[],
        ),
        choice("n11.round", "Rounding", ART_ROUNDING, 0),
        float("n11.strength", "Snap strength", 0.0, 100.0, 100.0, "%"),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, QUANTIZE_HELP);
    ToolSpec {
        id: "quantize",
        name: "Quantizer",
        family: Family::Fl,
        summary: "Moves note starts (and lengths or ends) toward a grid or groove, with strength, sensitivity and duration controls.",
        params: p,
        advanced: vec![
            "Adaptive strength by density",
            "Separate early / late strengths",
            "Onset vs gate curves",
            "Multi-grid (polyrhythmic)",
            "Timing offsets per pitch class",
            "Groove interpolation (A ↔ B)",
            "Chord lock",
            "Subchannel phase",
            "Error-distribution preservation",
            "Progressive strength envelope",
        ],
        functions: [
            ("n01", "Swing", 110),
            ("n02", "Frame grid", 120),
            ("n03", "Collision-free slots", 520),
            ("n04", "Cluster snap", 130),
            ("n05", "Elastic grid", 140),
            ("n06", "Register grid", 150),
            ("n07", "Self-groove", 160),
            ("n08", "Meter-aware anchor", 170),
            ("n09", "Onset magnet", 180),
            ("n10", "IOI ratio cleanup", 190),
            ("n11", "Velocity level snap", 530),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// The Quantizer's v0.9 functions declared by id (TOOL_CATALOG `quantize`
/// n01 … n11). `None` = switched off; with every one off the kernel is the
/// v0.8 one.
#[derive(Clone, Default)]
struct QntNew {
    /// n01: swing unit (ticks), swing (fraction 0.5 … 0.8), its amount over
    /// the selection.
    swing: Option<(u64, f64, Curve)>,
    /// n02: frame period and phase in µs.
    frame: Option<(f64, f64)>,
    /// n03: free step (0 nearest, 1 next, 2 previous, 3 leave unquantized),
    /// search radius in grid steps, same-key mode (0 key, 1 + channel, 2 +
    /// subchannel).
    slots: Option<(usize, i128, usize)>,
    /// n04: window, target (0 earliest, 1 loudest, 2 centroid, 3 latest),
    /// largest cluster.
    cluster: Option<(u64, usize, usize)>,
    /// n05: the elastic grid's points (ascending, distinct) and its largest
    /// spacing.
    elastic: Option<(std::sync::Arc<Vec<u64>>, u64)>,
    /// n06: low / high pivot, the grid at each, linear (else octave steps).
    register: Option<(i64, i64, u64, u64, bool)>,
    /// n07: groove length (beats), slots per beat, take velocities, strength.
    groove: Option<(u64, u64, bool, f64)>,
    /// n08: anchor (0 song start, 1 bar, 2 beat, 3 marker), the grid in odd
    /// meters, the odd numerators (ascending).
    anchor: Option<(usize, u64, Vec<u64>)>,
    /// n09: reference (0 unselected notes of the track, 1 a subchannel, 2 a
    /// track), its number (1-based), largest distance, strength.
    magnet: Option<(usize, usize, u64, f64)>,
    /// n10: unit, allowed interval lengths (multiples × unit, ascending, 0
    /// first), re-anchor every phrase.
    ioi: Option<(u64, Vec<i128>, bool)>,
    /// n11: levels (ascending, distinct), rounding, strength.
    levels: Option<(Vec<f64>, usize, f64)>,
}

impl QntNew {
    fn from(
        p: &Params,
        ctx: &super::super::Ctx,
        stats: &super::super::SourceStats,
    ) -> Result<QntNew, TransformError> {
        let pct = |k: &str| p.float(k) / 100.0;
        let musical = |k: &str| note_value(p.choice(k).min(art_grid_values().len() - 1), 1, ctx);
        let elastic = if p.flag("n05") {
            Some(qnt_elastic(
                p.ticks("n05.from").max(1),
                p.ticks("n05.to").max(1),
                &p.curve("n05.curve"),
                stats,
            )?)
        } else {
            None
        };
        Ok(QntNew {
            swing: p.flag("n01").then(|| {
                (
                    musical("n01.unit"),
                    pct("n01.swing").clamp(0.5, 0.8),
                    p.curve("n01.curve"),
                )
            }),
            frame: p.flag("n02").then(|| {
                let period = match QNT_FPS.get(p.choice("n02.grid")) {
                    Some(fps) => 1e6 / fps,
                    None => p.float("n02.ms").max(0.1) * 1000.0,
                };
                (period, p.float("n02.phase").max(0.0) * 1000.0)
            }),
            slots: p.flag("n03").then(|| {
                (
                    p.choice("n03.free"),
                    i128::from(p.int("n03.radius").max(1)),
                    p.choice("n03.key"),
                )
            }),
            cluster: p.flag("n04").then(|| {
                (
                    p.ticks("n04.window"),
                    p.choice("n04.target"),
                    p.int("n04.largest").max(2) as usize,
                )
            }),
            elastic,
            register: p.flag("n06").then(|| {
                (
                    p.int("n06.lo"),
                    p.int("n06.hi"),
                    p.ticks("n06.low").max(1),
                    p.ticks("n06.high").max(1),
                    p.choice("n06.between") == 1,
                )
            }),
            groove: p.flag("n07").then(|| {
                (
                    p.int("n07.beats").max(1) as u64,
                    p.int("n07.slots").max(1) as u64,
                    p.choice("n07.vel") == 1,
                    pct("n07.strength"),
                )
            }),
            anchor: p.flag("n08").then(|| {
                let mut odd: Vec<u64> =
                    p.list("n08.odd").iter().map(|&v| v.max(1) as u64).collect();
                odd.sort_unstable();
                odd.dedup();
                (p.choice("n08.anchor"), musical("n08.grid"), odd)
            }),
            magnet: p.flag("n09").then(|| {
                (
                    p.choice("n09.ref"),
                    p.int("n09.num").max(1) as usize,
                    p.ticks("n09.dist"),
                    pct("n09.strength"),
                )
            }),
            ioi: p.flag("n10").then(|| {
                let unit = p.ticks("n10.unit").max(1);
                let mut m: Vec<u64> = p
                    .list("n10.mult")
                    .iter()
                    .map(|&v| v.max(1) as u64)
                    .collect();
                if m.is_empty() {
                    m = (1..=64).collect();
                }
                m.push(0);
                m.sort_unstable();
                m.dedup();
                (
                    unit,
                    m.iter()
                        .map(|&k| i128::from(k) * i128::from(unit))
                        .collect(),
                    p.choice("n10.anchor") == 1,
                )
            }),
            levels: p.flag("n11").then(|| {
                let mut l: Vec<f64> = p
                    .list("n11.allowed")
                    .iter()
                    .map(|&v| v.clamp(1, 127) as f64)
                    .collect();
                if l.is_empty() {
                    let n = p.int("n11.levels").clamp(2, 127);
                    l = (0..n)
                        .map(|j| (1.0 + 126.0 * j as f64 / (n - 1) as f64).round())
                        .collect();
                }
                l.sort_by(f64::total_cmp);
                l.dedup();
                (l, p.choice("n11.round"), pct("n11.strength"))
            }),
        })
    }

    /// Whether a function reads other notes (the run is whole-source).
    fn whole(&self) -> bool {
        self.slots.is_some() || self.pulls()
    }

    /// Whether a function decides where starts are pulled (n04, n07, n09,
    /// n10).
    fn pulls(&self) -> bool {
        self.cluster.is_some()
            || self.groove.is_some()
            || self.magnet.is_some()
            || self.ioi.is_some()
    }
}

/// n05: the elastic grid across the selection — points from its first start
/// whose spacing runs from `from` to `to` along `curve` (the place along the
/// selection, as `pos`), integrated step by step, until one spacing past its
/// last end. Refused past [`QNT_MAX_ELASTIC`] points.
fn qnt_elastic(
    from: u64,
    to: u64,
    curve: &Curve,
    stats: &super::super::SourceStats,
) -> Result<(std::sync::Arc<Vec<u64>>, u64), TransformError> {
    let widest = from.max(to);
    if stats.is_empty() {
        return Ok((std::sync::Arc::new(Vec::new()), widest));
    }
    let lo = stats.min_start;
    let end = stats.max_end.saturating_add(widest);
    let refuse = || {
        TransformError::TooLarge(format!(
            "The elastic grid would need more than {} steps across the selection. Choose wider spacings or select a shorter stretch.",
            crate::midi::group_digits(QNT_MAX_ELASTIC as u64)
        ))
    };
    // Even the widest spacing everywhere needs this many points.
    if (end - lo) / widest > QNT_MAX_ELASTIC as u64 {
        return Err(refuse());
    }
    let span = stats.max_start.saturating_sub(lo).max(1) as f64;
    let (a, b) = (from as f64, to as f64);
    let mut pts = Vec::new();
    let mut p = lo as f64;
    for _ in 0..=QNT_MAX_ELASTIC {
        let t = p.round().min(u64::MAX as f64) as u64;
        if pts.last() != Some(&t) {
            pts.push(t);
        }
        if t >= end {
            return Ok((std::sync::Arc::new(pts), widest));
        }
        let x = ((p - lo as f64) / span).clamp(0.0, 1.0);
        let step = (a + (b - a) * curve.eval(x)).max(1.0);
        let next = p + step;
        if next <= p {
            break;
        }
        p = next;
    }
    Err(refuse())
}

/// The grid of one note's start (or end): where the grid restarts, the next
/// anchor (a grid point no target may pass), and its spacing.
#[derive(Clone, Copy, Debug)]
struct NoteGrid {
    origin: i128,
    next: Option<i128>,
    grid: u64,
}

/// Where a note's start is pulled (n04 / n07 / n09 / n10 decide it instead
/// of the grid).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pull {
    /// The grid (with the sensitivity window and the start strength).
    Grid,
    /// A target from the notes themselves, at the start strength.
    To(i128),
    /// A target at its own strength (fraction).
    Own(i128, f64),
}

/// n01's swing warp of `x` ticks into a pair of swing units `u` with the
/// off-step at `f` (0.5 … 0.8) of the pair, and its inverse.
fn qnt_swing(x: i128, u: u64, f: f64, inverse: bool) -> i128 {
    let u = i128::from(u.max(1));
    let pair = 2 * u;
    let base = x.div_euclid(pair) * pair;
    let r = (x - base) as f64;
    let (uf, sp) = (u as f64, f * 2.0 * u as f64);
    let y = if inverse {
        if r < sp {
            r / (2.0 * f)
        } else {
            uf + (r - sp) / (2.0 - 2.0 * f)
        }
    } else if r < uf {
        r * 2.0 * f
    } else {
        sp + (r - uf) * (2.0 - 2.0 * f)
    };
    base + y.round() as i128
}

/// The point of `pts` (ascending) nearest to `t` (the later one on a tie)
/// and the spacing around it.
fn qnt_nearest_point(pts: &[u64], t: i128) -> Option<(i128, u64)> {
    let k = pts.partition_point(|&p| i128::from(p) < t);
    let up = pts.get(k).map(|&p| i128::from(p));
    let down = k.checked_sub(1).map(|j| i128::from(pts[j]));
    let gap = |a: usize, b: usize| {
        pts.get(b)
            .zip(pts.get(a))
            .map_or(1, |(y, x)| (y - x).max(1))
    };
    match (down, up) {
        (Some(d), Some(u)) => Some(if t - d < u - t {
            (d, gap(k - 1, k))
        } else {
            (u, gap(k - 1, k))
        }),
        (None, Some(u)) => Some((u, gap(k, k + 1))),
        (Some(d), None) => Some((d, gap(k.saturating_sub(2), k - 1))),
        (None, None) => None,
    }
}

#[derive(Clone)]
struct Qnt {
    grid: u64,
    grids: Vec<u64>,
    groove: Option<(Vec<u64>, u64, Vec<i64>)>,
    groove_b: Option<(Vec<u64>, f64)>,
    start_s: f64,
    sens: f64,
    dur_s: f64,
    mode: usize,
    levels: f64,
    early_late: Option<(f64, f64)>,
    curves: Option<(Curve, Curve)>,
    pc_offsets: Vec<i128>,
    sub_phase: Vec<i128>,
    progressive: Option<(f64, f64)>,
    stats: super::super::SourceStats,
    subs: Vec<Vec<(u32, u8)>>,
    /// v0.9 functions declared by id.
    new: QntNew,
    ppq: u64,
    /// The context, when a function reads the tempo map or the meter (n02,
    /// n08).
    song: Option<super::super::Ctx>,
    /// n08: marker starts in the notes' ticks, ascending.
    markers: Vec<i128>,
    /// No function changes the grid itself (n01, n02, n05, n08 off): the
    /// v0.8 target, without n08's anchor clip.
    plain: bool,
    /// Some v0.9 function is on (else the kernel runs the v0.8 rule).
    any_new: bool,
}

impl Qnt {
    /// v0.8's nearest target of `t` (grid `grid`, multi-grid or groove), with
    /// its groove index.
    fn target(&self, t: i128, origin: i128, grid: u64) -> (i128, Option<usize>) {
        let near = |grid: u64| -> i128 {
            let g = i128::from(grid.max(1));
            let rel = t - origin;
            let down = rel.div_euclid(g) * g;
            let q = if (rel - down) * 2 >= g {
                down + g
            } else {
                down
            };
            origin + q
        };
        if let Some((pos, period, _)) = &self.groove {
            let per = i128::from(*period);
            let base = (t - origin).div_euclid(per) * per + origin;
            let mut best = (base + per, Some(0usize));
            for cand_base in [base - per, base, base + per] {
                for (i, p) in pos.iter().enumerate() {
                    let c = cand_base + i128::from(*p);
                    if (c - t).abs() < (best.0 - t).abs() {
                        best = (c, Some(i));
                    }
                }
            }
            if let Some((bpos, mix)) = &self.groove_b {
                let mut bb = base + per;
                for cand_base in [base - per, base, base + per] {
                    for p in bpos {
                        let c = cand_base + i128::from(*p);
                        if (c - t).abs() < (bb - t).abs() {
                            bb = c;
                        }
                    }
                }
                let m = (best.0 as f64 * (1.0 - mix) + bb as f64 * mix).round() as i128;
                return (m, best.1);
            }
            return best;
        }
        if !self.grids.is_empty() {
            let mut best = near(self.grids[0]);
            for &g in &self.grids[1..] {
                let c = near(g);
                if (c - t).abs() < (best - t).abs() {
                    best = c;
                }
            }
            return (best, None);
        }
        (near(grid), None)
    }

    /// n06: the grid of a note of `pitch` between the two pivots.
    fn register_grid(&self, pitch: u8) -> u64 {
        let Some((lo, hi, gl, gh, linear)) = self.new.register else {
            return self.grid;
        };
        let p = i64::from(pitch);
        let x = if lo == hi {
            if p >= hi { 1.0 } else { 0.0 }
        } else if linear {
            ((p - lo) as f64 / (hi - lo) as f64).clamp(0.0, 1.0)
        } else {
            // Whole octaves from the low pivot toward the high one; the
            // grid changes by the same ratio every octave.
            let d = (p - lo) * (hi - lo).signum();
            let full = (hi - lo).abs();
            let k = (full / 12).max(1);
            if d >= full {
                1.0
            } else {
                (d.div_euclid(12)).clamp(0, k) as f64 / k as f64
            }
        };
        let g = if linear {
            gl as f64 + (gh as f64 - gl as f64) * x
        } else {
            gl as f64 * (gh as f64 / gl as f64).powf(x)
        };
        (g.round() as u64).max(1)
    }

    /// The v0.8 grid origin of `n` (a8: its subchannel's phase).
    #[inline]
    fn phase_of(&self, n: &TNote) -> i128 {
        if self.sub_phase.is_empty() {
            0
        } else {
            let si = self
                .subs
                .get(usize::from(n.track))
                .and_then(|t| t.iter().position(|s| s.0 == n.sub))
                .unwrap_or(0);
            self.sub_phase[si % self.sub_phase.len()]
        }
    }

    /// The grid at `at` for note `n`: the v0.8 origin (subchannel phase),
    /// n06's register grid, n08's anchor and odd-meter grid.
    fn note_grid(&self, at: u64, n: &TNote) -> NoteGrid {
        let phase = self.phase_of(n);
        let mut g = NoteGrid {
            origin: phase,
            next: None,
            grid: if self.new.register.is_some() {
                self.register_grid(n.pitch)
            } else {
                self.grid
            },
        };
        let (Some((mode, odd_grid, odd)), Some(ctx)) = (&self.new.anchor, &self.song) else {
            return g;
        };
        if !odd.is_empty()
            && let Some((num, _)) = ctx.meter.signature_at(at)
            && odd.binary_search(&num).is_ok()
        {
            g.grid = *odd_grid;
        }
        let (anchor, next) = match *mode {
            1 | 2 => {
                let m = metric_position(&ctx.meter, at);
                if *mode == 1 {
                    (m.bar_start, m.bar_start.saturating_add(m.bar_len))
                } else {
                    (m.beat_start, m.beat_start.saturating_add(m.beat_len))
                }
            }
            3 => {
                let t = i128::from(at);
                let k = self.markers.partition_point(|&m| m <= t);
                g.origin += k.checked_sub(1).map_or(0, |j| self.markers[j]);
                g.next = self.markers.get(k).map(|&m| m + phase);
                return g;
            }
            _ => return g,
        };
        g.origin += i128::from(anchor);
        g.next = Some(i128::from(next) + phase);
        g
    }

    /// The grid target of `t`: n02's frame grid, n05's elastic grid or the
    /// v0.8 target (grid / multi-grid / groove, never past n08's next
    /// anchor), swung by n01 — with its groove index and the local spacing.
    fn target_grid(
        &self,
        t: i128,
        ng: &NoteGrid,
        tt: &mut Option<TempoTime<'_>>,
        swing: Option<(u64, f64)>,
    ) -> (i128, Option<usize>, u64) {
        let swing = swing.filter(|s| s.1 != 0.5);
        let at = match swing {
            Some((u, f)) => ng.origin + qnt_swing(t - ng.origin, u, f, true),
            None => t,
        };
        let (q, gi, spacing, anchored) =
            if let (Some((period, phase)), Some(tt)) = (self.new.frame, tt.as_mut()) {
                let a = at.clamp(0, i128::from(u64::MAX)) as u64;
                let q = tt.snap_to_grid(a, period, phase);
                let sp = tt.ticks_after(q, period.round() as u64).max(1);
                (i128::from(q), None, sp, false)
            } else if let Some((pts, widest)) = &self.new.elastic {
                match qnt_nearest_point(pts, at) {
                    Some((q, sp)) => (q, None, sp, false),
                    None => (at, None, *widest, false),
                }
            } else {
                let (mut q, mut gi) = self.target(at, ng.origin, ng.grid);
                let mut anchored = false;
                if let Some(nx) = ng.next
                    && (nx < q || (q <= at && nx - at <= at - q))
                {
                    q = nx;
                    gi = None;
                    anchored = true;
                }
                (q, gi, ng.grid, anchored)
            };
        let q = match swing {
            Some((u, f)) if !anchored => ng.origin + qnt_swing(q - ng.origin, u, f, false),
            _ => q,
        };
        (q, gi, spacing)
    }

    /// The quantized (start, dur, velocity, target − start) of `n` with
    /// strength scale `k`, its start pulled as `pull` says. `NEW` = some
    /// v0.9 function is on (false: the v0.8 rule, compiled without them).
    fn quantize<const NEW: bool>(
        &self,
        n: &TNote,
        k: f64,
        tt: &mut Option<TempoTime<'_>>,
        pull: Pull,
    ) -> (i128, i128, i64, i128) {
        let ng = if NEW {
            self.note_grid(n.start, n)
        } else {
            NoteGrid {
                origin: self.phase_of(n),
                next: None,
                grid: self.grid,
            }
        };
        let pull = if NEW { pull } else { Pull::Grid };
        let s = i128::from(n.start);
        let e = s + i128::from(n.dur);
        let t = pos(n, &self.stats);
        let swing = if NEW {
            self.new
                .swing
                .as_ref()
                .map(|(u, f, c)| (*u, 0.5 + (f - 0.5) * c.eval(t).clamp(0.0, 1.0)))
        } else {
            None
        };
        let (q, gi, spacing, own) = match pull {
            Pull::Grid if !NEW || self.plain => {
                let (q, gi) = self.target(s, ng.origin, ng.grid);
                (q, gi, ng.grid, None)
            }
            Pull::Grid => {
                let (q, gi, sp) = self.target_grid(s, &ng, tt, swing);
                (q, gi, sp, None)
            }
            Pull::To(q) => (q, None, ng.grid, None),
            Pull::Own(q, st) => (q, None, ng.grid, Some(st)),
        };
        let window = (spacing as f64 * self.sens / 100.0 / 2.0).max(0.5);
        let within = pull != Pull::Grid || ((q - s).abs() as f64) <= window || self.sens >= 100.0;
        let mut ss = self.start_s / 100.0;
        let mut ds = self.dur_s / 100.0;
        if let Some((a, b)) = self.progressive {
            ss = (a + (b - a) * t) / 100.0;
        }
        if let Some((cs, cg)) = &self.curves {
            ss = cs.eval(t);
            ds = cg.eval(t);
        }
        if let Some((early, late)) = self.early_late {
            ss = if s < q { early } else { late } / 100.0;
        }
        let ss = match own {
            Some(st) => st.clamp(0.0, 1.0),
            None => (ss * k).clamp(0.0, 1.0),
        };
        let start = if within {
            s + ((q - s) as f64 * ss).round() as i128
        } else {
            s
        };
        let pc_off = self
            .pc_offsets
            .get(usize::from(n.pitch % 12))
            .copied()
            .unwrap_or(0);
        let start = start + pc_off;
        let g = i128::from(ng.grid.max(1));
        let qdur = |d: i128| -> i128 {
            let down = d - d.rem_euclid(g);
            let q = if d.rem_euclid(g) * 2 >= g {
                down + g
            } else {
                down
            };
            q.max(g)
        };
        let dur = match self.mode {
            // Quantize duration: length snapped to the grid, mixed by Duration.
            0 => {
                let d = i128::from(n.dur);
                d + ((qdur(d) - d) as f64 * ds).round() as i128
            }
            // Quantize end time: end snapped to the grid, mixed.
            1 => {
                let qe = if !NEW || self.plain {
                    self.target(e, ng.origin, ng.grid).0
                } else {
                    let nge = if self.new.anchor.is_some() {
                        self.note_grid(n.end(), n)
                    } else {
                        ng
                    };
                    self.target_grid(e, &nge, tt, swing).0
                };
                let end = e + ((qe - e) as f64 * ds).round() as i128;
                end - start
            }
            // Leave duration.
            2 => i128::from(n.dur),
            // Leave end time.
            _ => e - start,
        };
        let vel = match (&self.groove, gi) {
            (Some((_, _, vels)), Some(i)) if !vels.is_empty() && self.levels > 0.0 => {
                let gv = vels[i % vels.len()] as f64;
                (f64::from(n.vel) * (1.0 - self.levels) + gv * self.levels).round() as i64
            }
            _ => i64::from(n.vel),
        };
        (start, dur, vel, q - s)
    }

    /// n11 (order 530, the last step of the note): the velocity snapped to
    /// the allowed levels at the strength.
    #[inline]
    fn snap_vel(&self, v: i64) -> i64 {
        match &self.new.levels {
            Some((levels, round, strength)) => {
                let x = v.clamp(1, 127) as f64;
                (x + strength * (art_snap(levels, *round, x) - x)).round() as i64
            }
            None => v,
        }
    }

    /// The real-time lookups of one run (n02 only).
    fn tempo_time(&self) -> Option<TempoTime<'_>> {
        self.new.frame.and(self.song.as_ref()).map(TempoTime::new)
    }

    /// n04 / n07 / n09 / n10: where each note's start is pulled (the grid
    /// unless one of them decides it — n09 within its distance, then n10,
    /// n07, n04) and, when n07 takes velocities, the mean velocity of each
    /// note's groove slot.
    #[allow(clippy::too_many_lines)]
    fn pulls<'n>(&self, src: &Source, nw: &impl Fn(usize) -> &'n QntNew) -> (Vec<Pull>, Vec<f64>) {
        let n = src.notes.len();
        let mut pull = vec![Pull::Grid; n];
        let mut vel = Vec::new();
        let tracks = (self.new.cluster.is_some() || self.new.ioi.is_some()).then(|| per_track(src));
        if let (Some((_, target, largest)), None, None) =
            (self.new.cluster, &self.new.groove, &self.new.ioi)
        {
            for idx in tracks.iter().flatten() {
                let mut k = 0;
                while k < idx.len() {
                    let first = src.notes[idx[k]].start;
                    // The cluster's window: its first note's.
                    let window = nw(idx[k]).cluster.map_or(0, |c| c.0);
                    let mut j = k + 1;
                    while j < idx.len()
                        && j - k < largest
                        && src.notes[idx[j]].start - first <= window
                    {
                        j += 1;
                    }
                    let c = &idx[k..j];
                    let at = match target {
                        0 => first,
                        1 => {
                            // The loudest note (the earliest of equals).
                            let mut best = c[0];
                            for &i in c {
                                if src.notes[i].vel > src.notes[best].vel {
                                    best = i;
                                }
                            }
                            src.notes[best].start
                        }
                        2 => {
                            let sum: u128 = c.iter().map(|&i| u128::from(src.notes[i].start)).sum();
                            let len = c.len() as u128;
                            ((sum + len / 2) / len) as u64
                        }
                        _ => src.notes[c[c.len() - 1]].start,
                    };
                    for &i in c {
                        pull[i] = Pull::To(i128::from(at));
                    }
                    k = j;
                }
            }
        }
        if let Some((beats, per_beat, take, _)) = self.new.groove {
            let ppq = i128::from(self.ppq.max(1));
            let spb = i128::from(per_beat);
            let total = i128::from(beats * per_beat);
            // Slot j from the grid's origin: origin + round(j · ppq / spb).
            let point = |origin: i128, j: i128| origin + (2 * j * ppq + spb).div_euclid(2 * spb);
            let mut sum = vec![0i128; total as usize];
            let mut vsum = vec![0u64; total as usize];
            let mut count = vec![0u64; total as usize];
            let place: Vec<(i128, usize)> = src
                .notes
                .iter()
                .map(|x| {
                    let o = self.note_grid(x.start, x).origin;
                    let s = i128::from(x.start);
                    let j = (2 * (s - o) * spb + ppq).div_euclid(2 * ppq);
                    let p = point(o, j);
                    let g = j.rem_euclid(total) as usize;
                    sum[g] += s - p;
                    vsum[g] += u64::from(x.vel);
                    count[g] += 1;
                    (p, g)
                })
                .collect();
            let avg = |g: usize| -> i128 {
                let c = i128::from(count[g].max(1));
                (2 * sum[g] + c).div_euclid(2 * c)
            };
            if self.new.ioi.is_none() {
                for (i, &(p, g)) in place.iter().enumerate() {
                    let strength = nw(i).groove.map_or(0.0, |x| x.3);
                    pull[i] = Pull::Own(p + avg(g), strength);
                }
            }
            if take {
                vel = place
                    .iter()
                    .map(|&(_, g)| vsum[g] as f64 / count[g].max(1) as f64)
                    .collect();
            }
        }
        if let Some((_, _, phrases)) = &self.new.ioi {
            // Each onset's unit and allowed lengths (its first note's).
            let lens_of = |i: usize| -> (&Vec<i128>, i128) {
                let (unit, lens, _) = nw(i).ioi.as_ref().unwrap_or(self.new.ioi.as_ref().unwrap());
                let longest = *lens.last().unwrap_or(&0);
                // Intervals up to half a unit past the longest allowed one
                // snap; longer ones are rests and stay as they are.
                (lens, longest + i128::from(*unit / 2))
            };
            for idx in tracks.iter().flatten() {
                let starts = if *phrases {
                    let rest_of = |i: usize| {
                        u64::try_from(*lens_of(i).0.last().unwrap_or(&0)).unwrap_or(u64::MAX)
                    };
                    let ph = Phrases::of_by(&src.notes, idx, &rest_of, None);
                    let mut first = vec![false; idx.len()];
                    let mut at = 0;
                    for k in 0..ph.len() {
                        first[at] = true;
                        at += ph.get(idx, k).len();
                    }
                    first
                } else {
                    Vec::new()
                };
                let mut prev: Option<(i128, i128)> = None;
                let mut k = 0;
                while k < idx.len() {
                    let orig = i128::from(src.notes[idx[k]].start);
                    let mut j = k + 1;
                    while j < idx.len() && i128::from(src.notes[idx[j]].start) == orig {
                        j += 1;
                    }
                    let (lens, limit) = lens_of(idx[k]);
                    let new = match prev {
                        Some((po, pn)) if !starts.get(k).copied().unwrap_or(false) => {
                            let ioi = orig - po;
                            if ioi <= limit {
                                // The nearest allowed length (the shorter on a tie).
                                let a = lens.partition_point(|&l| l < ioi);
                                let up = lens.get(a).copied();
                                let down = a.checked_sub(1).map(|b| lens[b]);
                                pn + match (down, up) {
                                    (Some(d), Some(u)) if ioi - d <= u - ioi => d,
                                    (_, Some(u)) => u,
                                    (Some(d), None) => d,
                                    (None, None) => ioi,
                                }
                            } else {
                                pn + ioi
                            }
                        }
                        _ => orig,
                    };
                    for &i in &idx[k..j] {
                        pull[i] = Pull::To(new);
                    }
                    prev = Some((orig, new));
                    k = j;
                }
            }
        }
        if let Some((reference, num, _, strength)) = self.new.magnet {
            let nearest = |ons: &[u64], t: u64, dist: u64| -> Option<u64> {
                let k = ons.partition_point(|&o| o < t);
                let mut best: Option<u64> = None;
                for c in [k.checked_sub(1), Some(k)].into_iter().flatten() {
                    if let Some(&o) = ons.get(c)
                        && o.abs_diff(t) <= dist
                        && best.is_none_or(|b| o.abs_diff(t) < b.abs_diff(t))
                    {
                        best = Some(o);
                    }
                }
                best
            };
            if reference == 2 {
                let mut ons: Vec<u64> = src
                    .aux
                    .first()
                    .map(|v| v.iter().map(|x| x.start).collect())
                    .unwrap_or_default();
                ons.sort_unstable();
                ons.dedup();
                for (i, x) in src.notes.iter().enumerate() {
                    let dist = nw(i).magnet.map_or(0, |m| m.2);
                    if let Some(o) = nearest(&ons, x.start, dist) {
                        pull[i] = Pull::Own(i128::from(o), strength);
                    }
                }
            } else {
                let slots = src.tracks.len();
                let mut ons: Vec<Vec<u64>> = vec![Vec::new(); slots];
                let sub = |slot: usize| -> Option<Option<u32>> {
                    if reference == 1 {
                        src.tracks
                            .get(slot)
                            .and_then(|t| t.subs.get(num - 1))
                            .map(|s| Some(s.0))
                    } else {
                        Some(None)
                    }
                };
                let subs: Vec<Option<Option<u32>>> = (0..slots).map(sub).collect();
                for x in src.context.iter() {
                    let slot = usize::from(x.track);
                    if let Some(Some(want)) = subs.get(slot)
                        && want.is_none_or(|s| x.sub == s)
                    {
                        ons[slot].push(x.start);
                    }
                }
                if reference == 1 {
                    for x in &src.notes {
                        if let Some(Some(Some(s))) = subs.get(usize::from(x.track))
                            && x.sub == *s
                        {
                            ons[usize::from(x.track)].push(x.start);
                        }
                    }
                }
                for o in &mut ons {
                    o.sort_unstable();
                    o.dedup();
                }
                for (i, x) in src.notes.iter().enumerate() {
                    let slot = usize::from(x.track);
                    let Some(Some(want)) = subs.get(slot) else {
                        continue;
                    };
                    // The reference subchannel's own notes lead: not pulled.
                    if want.is_some_and(|s| x.sub == s) {
                        continue;
                    }
                    let dist = nw(i).magnet.map_or(0, |m| m.2);
                    if let Some(o) = nearest(&ons[slot], x.start, dist) {
                        pull[i] = Pull::Own(i128::from(o), strength);
                    }
                }
            }
        }
        (pull, vel)
    }
}

/// n03 (order 520, after the start / length rules): per key, in quantized
/// order, a note that would share its start with an earlier one or overlap
/// it moves to the nearest / next / previous free grid step within the
/// radius, or — leave unquantized, or nothing free — back to its original
/// start and length.
fn qnt_slots(
    src: &Source,
    res: &mut [(i128, i128, i64, i128)],
    grid_of: impl Fn(usize) -> u64,
    (free, _, key_mode): (usize, i128, usize),
    radius_of: impl Fn(usize) -> i128,
) {
    // (key, quantized start, index): the source is in time order, so the
    // index breaks ties by the original start. The key packs track, pitch
    // and the channel or subchannel.
    let mut order: Vec<(u64, i128, u32)> = src
        .notes
        .iter()
        .zip(res.iter())
        .enumerate()
        .map(|(i, (x, r))| {
            let third = match key_mode {
                0 => 0,
                1 => u64::from(x.ch),
                _ => u64::from(x.sub),
            };
            let key = u64::from(x.track) << 40 | u64::from(x.pitch) << 32 | third;
            (key, r.0, i as u32)
        })
        .collect();
    order.sort_unstable();
    // Whether [c, c + d) is free of every placed note of the key (`placed`
    // ascending by start; a note starting `longest` or more before c ends by
    // c).
    let is_free = |placed: &[(i128, i128)], longest: i128, c: i128, d: i128| -> bool {
        // Mostly at the end: the notes come in quantized order.
        let hi = if placed.last().is_none_or(|p| p.0 < c + d) {
            placed.len()
        } else {
            placed.partition_point(|p| p.0 < c + d)
        };
        for p in placed[..hi].iter().rev() {
            if p.0 <= c - longest {
                break;
            }
            if p.1 > c {
                return false;
            }
        }
        true
    };
    let mut placed: Vec<(i128, i128)> = Vec::new();
    for group in order.chunk_by(|a, b| a.0 == b.0) {
        placed.clear();
        let mut longest: i128 = 1;
        for &(_, _, i) in group {
            let i = i as usize;
            let (st, du) = (res[i].0, res[i].1.max(1));
            let pick = if is_free(&placed, longest, st, du) {
                Some(st)
            } else if free == 3 {
                None
            } else {
                let g = i128::from(grid_of(i).max(1));
                let mut found = None;
                'search: for k in 1..=radius_of(i) {
                    let cands = match free {
                        0 => [Some(st + k * g), Some(st - k * g)],
                        1 => [Some(st + k * g), None],
                        _ => [Some(st - k * g), None],
                    };
                    for c in cands.into_iter().flatten() {
                        if c >= 0 && is_free(&placed, longest, c, du) {
                            found = Some(c);
                            break 'search;
                        }
                    }
                }
                found
            };
            match pick {
                Some(c) => res[i].0 = c,
                None => {
                    let x = &src.notes[i];
                    res[i].0 = i128::from(x.start);
                    res[i].1 = i128::from(x.dur);
                }
            }
            let (s2, d2) = (res[i].0, res[i].1.max(1));
            // Mostly appended: the notes come in quantized order.
            if placed.last().is_none_or(|p| p.0 <= s2) {
                placed.push((s2, s2 + d2));
            } else {
                let at = placed.partition_point(|p| p.0 <= s2);
                placed.insert(at, (s2, s2 + d2));
            }
            longest = longest.max(d2);
        }
    }
}

/// Reads the references of the Quantizer's n09 (Onset magnet): the scoped
/// tracks' unselected notes within the distance of the selection
/// (`Source::context`), or the reference track's notes (`Source::aux`).
pub fn quantize_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("n09") || src.stats.is_empty() {
        return Ok(());
    }
    let d = p.ticks("n09.dist");
    let a = src.stats.min_start.saturating_sub(d);
    let b = src.stats.max_start.saturating_add(d);
    if p.choice("n09.ref") == 2 {
        let num = p.int("n09.num");
        let track = usize::try_from(num - 1)
            .ok()
            .filter(|&i| doc.track(i).is_some())
            .ok_or_else(|| {
                TransformError::Invalid(format!(
                    "Track {num} does not exist. Choose the reference track by its number in the track list."
                ))
            })?;
        let mut layer = Vec::new();
        // B2-F16: the reference track as the song shows it, on these ticks.
        let shift = ctx.map_or(0, |c| c.shift);
        super::super::source::placed_query(doc, track, shift, a, b, &mut |r| {
            if r.start_tick >= a && r.start_tick <= b && layer.len() < limit {
                layer.push(TNote::from_record(&r, u16::MAX));
            }
        });
        src.aux = std::sync::Arc::new(vec![layer]);
    } else {
        let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
        *src = s.with_context_between(doc, a, b);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
/// The Quantizer's settings from `p`.
fn qnt_of(p: &Params, b: &BuildCtx) -> Result<Qnt, TransformError> {
    let ppq = b.ctx.ppq;
    let grid = note_value(p.choice("grid"), p.ticks("grid.ticks"), b.ctx);
    let to_ticks = |v: Vec<i64>| -> Vec<u64> {
        v.into_iter()
            .map(|u| units48(u, ppq).max(0) as u64)
            .collect()
    };
    let groove = p.flag("groove").then(|| {
        let mut pos = to_ticks(p.list("groove.pos"));
        pos.sort_unstable();
        pos.dedup();
        if pos.is_empty() {
            pos.push(0);
        }
        (
            pos,
            (p.int("groove.beats") as u64 * ppq).max(1),
            p.list("groove.vel"),
        )
    });
    let new = QntNew::from(p, b.ctx, b.stats)?;
    let song = (new.frame.is_some() || new.anchor.is_some()).then(|| b.ctx.clone());
    let markers = if new.anchor.as_ref().is_some_and(|a| a.0 == 3) {
        b.ctx
            .markers
            .iter()
            .map(|&m| i128::from(m) - b.ctx.shift)
            .collect()
    } else {
        Vec::new()
    };
    let q = Qnt {
        grid,
        grids: if p.adv(4) {
            to_ticks(p.list("a4.grids"))
                .into_iter()
                .map(|g| g.max(1))
                .collect()
        } else {
            Vec::new()
        },
        groove_b: (p.adv(6) && groove.is_some())
            .then(|| (to_ticks(p.list("a6.pos")), p.float("a6.mix"))),
        groove,
        start_s: p.float("start"),
        sens: p.float("sens"),
        dur_s: p.float("dur"),
        mode: p.choice("mode"),
        levels: p.float("levels"),
        early_late: p.adv(2).then(|| (p.float("a2.early"), p.float("a2.late"))),
        curves: p.adv(3).then(|| (p.curve("a3.start"), p.curve("a3.gate"))),
        pc_offsets: if p.adv(5) {
            p.list("a5.offsets")
                .into_iter()
                .map(|u| units48(u, ppq))
                .collect()
        } else {
            Vec::new()
        },
        sub_phase: if p.adv(8) {
            p.list("a8.phase")
                .into_iter()
                .map(|u| units48(u, ppq))
                .collect()
        } else {
            Vec::new()
        },
        progressive: p.adv(10).then(|| (p.float("a10.from"), p.float("a10.to"))),
        stats: b.stats.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        new,
        ppq,
        song,
        markers,
        plain: false,
        any_new: false,
    };
    let n = &q.new;
    let q = Qnt {
        plain: n.swing.is_none() && n.frame.is_none() && n.elastic.is_none() && n.anchor.is_none(),
        any_new: !(n.swing.is_none()
            && n.frame.is_none()
            && n.elastic.is_none()
            && n.anchor.is_none()
            && n.register.is_none()
            && n.levels.is_none()
            && !n.whole()),
        ..q
    };
    Ok(q)
}

fn qnt_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let q = qnt_of(p, b)?;
    // With Control Sweeps (E0422) always the whole-source run, each note
    // with its level's settings.
    let swept = Rebuild::of(p, b);
    let sctx = b.ctx.clone();
    let density = p
        .adv(1)
        .then(|| (p.ticks("a1.window"), p.float("a1.amount")));
    let chord_tol = p.adv(7).then(|| p.ticks("a7.tol"));
    let keep = p.adv(9).then(|| p.float("a9.keep") / 100.0);
    if density.is_some()
        || chord_tol.is_some()
        || keep.is_some()
        || q.new.whole()
        || swept.is_some()
    {
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                // Level settings are built where the base ones were; a
                // level that fails to build keeps the base settings.
                let lv: Option<PerLevel<Option<Qnt>>> = swept
                    .as_ref()
                    .map(|r| r.levels(&sctx, src, |p, b| qnt_of(p, b).ok()));
                let me =
                    |i: usize| -> &Qnt { lv.as_ref().and_then(|l| l.at(i).as_ref()).unwrap_or(&q) };
                let nw = |i: usize| -> &QntNew { &me(i).new };
                let dens: Vec<f64> = match density {
                    Some((w, amt)) => {
                        let d = super::editors::local_density(&src.notes, w);
                        let mean = d.iter().map(|&x| x as f64).sum::<f64>() / d.len().max(1) as f64;
                        d.iter()
                            .map(|&x| {
                                (1.0 + amt * (x as f64 / mean.max(1e-9) - 1.0)).clamp(0.0, 2.0)
                            })
                            .collect()
                    }
                    None => vec![1.0; src.notes.len()],
                };
                let mut tt = q.tempo_time();
                let (pulls, gvel) = if q.new.pulls() {
                    q.pulls(src, &nw)
                } else {
                    (Vec::new(), Vec::new())
                };
                let mut res: Vec<(i128, i128, i64, i128)> = src
                    .notes
                    .iter()
                    .zip(&dens)
                    .enumerate()
                    .map(|(i, (n, &k))| {
                        let pull = pulls.get(i).copied().unwrap_or(Pull::Grid);
                        let me = me(i);
                        if me.any_new {
                            me.quantize::<true>(n, k, &mut tt, pull)
                        } else {
                            me.quantize::<false>(n, k, &mut tt, pull)
                        }
                    })
                    .collect();
                // n07: velocities toward the mean of the groove's slots.
                if q.new.groove.is_some() {
                    for (i, (r, &gv)) in res.iter_mut().zip(&gvel).enumerate() {
                        let strength = nw(i).groove.map_or(0.0, |x| x.3);
                        let v = r.2 as f64;
                        r.2 = (v + (gv - v) * strength).round() as i64;
                    }
                }
                if let Some(tol) = chord_tol {
                    // Chord members move by the delta of the chord's first note.
                    for idx in per_track(src) {
                        for g in chords(src, &idx, tol).iter() {
                            let first = g[0];
                            let delta = res[first].0 - i128::from(src.notes[first].start);
                            for &i in g {
                                let n = &src.notes[i];
                                let new_start = i128::from(n.start) + delta;
                                let end_shift = res[i].0 + res[i].1;
                                res[i].1 = match q.mode {
                                    2 => i128::from(n.dur),
                                    _ => end_shift - new_start,
                                };
                                res[i].0 = new_start;
                            }
                        }
                    }
                }
                if let Some(keep) = keep {
                    // Keep each note's deviation minus the mean deviation.
                    let mean = src
                        .notes
                        .iter()
                        .zip(&res)
                        .map(|(n, r)| (i128::from(n.start) - (i128::from(n.start) + r.3)) as f64)
                        .sum::<f64>()
                        / src.notes.len().max(1) as f64;
                    for (n, r) in src.notes.iter().zip(res.iter_mut()) {
                        let grid_pt = i128::from(n.start) + r.3;
                        let dev = (i128::from(n.start) - grid_pt) as f64 - mean;
                        let new = grid_pt + (dev * keep).round() as i128;
                        r.1 += r.0 - new;
                        if q.mode == 2 {
                            r.1 = i128::from(n.dur);
                        }
                        r.0 = new;
                    }
                }
                if let Some(slots) = q.new.slots {
                    let grid_of =
                        |i: usize| me(i).note_grid(src.notes[i].start, &src.notes[i]).grid;
                    let radius_of = |i: usize| nw(i).slots.map_or(slots.1, |x| x.1);
                    qnt_slots(src, &mut res, grid_of, slots, radius_of);
                }
                for (i, (n, r)) in src.notes.iter().zip(&res).enumerate() {
                    out.emit(r.0, r.1, i64::from(n.pitch), me(i).snap_vel(r.2), n);
                    out.check()?;
                }
                Ok(())
            },
            growth: Growth::Map,
            estimate: None,
        });
    }
    let mut reach = q
        .grid
        .max(q.grids.iter().copied().max().unwrap_or(0))
        .saturating_mul(2)
        + q.groove.as_ref().map_or(0, |g| g.1.saturating_mul(2))
        + q.pc_offsets
            .iter()
            .map(|x| x.unsigned_abs() as u64)
            .max()
            .unwrap_or(0)
        + q.sub_phase
            .iter()
            .map(|x| x.unsigned_abs() as u64)
            .max()
            .unwrap_or(0);
    // v0.9: how far the new per-note functions can move a start.
    if let Some((_, _, gl, gh, _)) = q.new.register {
        reach = reach.saturating_add(gl.max(gh).saturating_mul(2));
    }
    if let Some((_, odd, _)) = &q.new.anchor {
        reach = reach.saturating_add(odd.saturating_mul(2));
    }
    if let (Some((period, _)), Some(ctx)) = (q.new.frame, &q.song) {
        reach = reach.saturating_add(TempoTime::new(ctx).max_ticks_in(period));
    }
    if let Some((_, widest)) = &q.new.elastic {
        reach = reach.saturating_add(widest.saturating_mul(2));
    }
    if let Some((unit, _, _)) = &q.new.swing {
        reach = reach.saturating_add(unit.saturating_mul(4));
    }
    let max_dur = b.stats.max_dur;
    let behind = reach.saturating_add(max_dur);
    if !q.any_new {
        // The v0.8 rule (no v0.9 function on).
        return boxed(Pointwise {
            f: move |n: &TNote, out: &mut Output| {
                let (s, d, v, _) = q.quantize::<false>(n, 1.0, &mut None, Pull::Grid);
                out.emit(s, d, i64::from(n.pitch), v, n);
            },
            ahead: reach,
            behind,
            growth: Growth::Map,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let (s, d, v, _) = if q.new.frame.is_some() {
                q.quantize::<true>(n, 1.0, &mut q.tempo_time(), Pull::Grid)
            } else {
                q.quantize::<true>(n, 1.0, &mut None, Pull::Grid)
            };
            out.emit(s, d, i64::from(n.pitch), q.snap_vel(v), n);
        },
        ahead: reach,
        behind,
        growth: Growth::Map,
    })
}

// ================================================================ Randomizer

pub const RANDOMIZE: ToolDef = ToolDef {
    id: "randomize",
    spec: rnd_spec,
    build: rnd_build,
};

const RND_MODES: &[&str] = &[
    "Randomize levels of the selected notes",
    "Generate notes in the selection's time range (keep the selection)",
    "Generate notes and replace the selection",
];
const RND_REGIONS: &[&str] = &["Whole range", "Bar", "N bars", "Marker section"];
const RND_CHORD_LEN: &[&str] = &["Bar", "2 bars", "4 bars", "1/2", "1/4"];
const RND_CHORD_TYPES: &[&str] = &["Triad", "Seventh", "Ninth", "Sus"];
const RND_MUTATE: &[&str] = &["Pitch", "Onset", "Presence", "All"];
const RND_MOVE: &[&str] = &["Each note", "Each chord as a block"];
const RND_CELLS: &[&str] = &["Beat", "Half bar", "Bar"];
const RND_SILENT: &[&str] = &["The selection", "The track", "All tracks"];
/// Most bars, cells, regions or beat columns a Randomizer function walks
/// across the selection (more is refused before anything is allocated).
pub const RND_MAX_CELLS: u64 = 4_194_304;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const RANDOMIZE_HELP: &[(&str, &str)] = &[
    (
        "mode",
        "Randomize Levels changes the selected notes' velocities (and releases); the Generate modes write random notes over the selection's time span, keeping or replacing the selection.",
    ),
    (
        "octave",
        "For generating: the octave the random notes start from (4 = middle C's).",
    ),
    (
        "range",
        "For generating: how many octaves above Octave the random notes may reach.",
    ),
    (
        "key",
        "The key of the scale the random notes are drawn from.",
    ),
    ("scale", "The scale the random notes are drawn from."),
    (
        "length",
        "For generating: the length of each random note, and the step between note slots, as a note value.",
    ),
    (
        "length.ticks",
        "For Custom: the note length and step, in ticks.",
    ),
    (
        "variation",
        "Makes random notes randomly shorter (−) or longer (+) than Length; 0 = all the same length.",
    ),
    (
        "population",
        "For generating: the chance that each step of each voice holds a note (100 % = every step).",
    ),
    (
        "stack",
        "For generating: how many voices are drawn at every step (chords).",
    ),
    (
        "merge",
        "Joins random notes of the same key that touch or overlap into one longer note.",
    ),
    (
        "vel",
        "How much velocities change: a random amount up to this share of 127 (− with Bipolar off: only downward).",
    ),
    (
        "rel",
        "How much release velocities change, as a share of 127.",
    ),
    (
        "bipolar",
        "Changes go up and down at random; off, they go only in the direction of the amount's sign.",
    ),
    (
        "reset",
        "Starts every note from velocity 100 before randomizing, instead of its own velocity.",
    ),
    (
        "a1",
        "Draws pitches as a random walk: each note moves a few scale degrees from the last.",
    ),
    (
        "a1.step",
        "The largest move between consecutive random notes, in scale degrees.",
    ),
    ("a2", "Draws scale degrees by weights instead of evenly."),
    (
        "a2.weights",
        "The weight of each scale degree, from the tonic up (8 2 5 … favors the tonic, then the third and fifth).",
    ),
    (
        "a3",
        "Keeps a random note only with its pitch class's probability.",
    ),
    (
        "a3.prob",
        "The keep probability for C, C♯ … B in order, in percent.",
    ),
    (
        "a4",
        "For Randomize Levels: randomizes a note only with its subchannel's probability.",
    ),
    (
        "a4.weights",
        "The probability for each subchannel in turn, in percent; the list repeats.",
    ),
    (
        "a5",
        "Changes the population across the selection's time span.",
    ),
    (
        "a5.curve",
        "The population multiplier from the start (left) to the end (right); 0.5 = as set, 1 = twice as dense.",
    ),
    (
        "a6",
        "Thins the random notes so no more than a set number sound at once (and no key overlaps itself).",
    ),
    (
        "a6.max",
        "The most random notes that may sound at the same moment.",
    ),
    (
        "a7",
        "Makes the random notes symmetrical in time (a palindrome) or in pitch (mirrored around the range's center).",
    ),
    (
        "a7.axis",
        "Time mirrors the first half into the second; Pitch mirrors every other note around the center of the range.",
    ),
    (
        "a8",
        "Blends two seeds: each step takes the main seed or the second one by chance.",
    ),
    ("a8.seed", "The second seed."),
    (
        "a8.mix",
        "The share of steps that use the second seed (0 = only the main seed, 1 = only the second).",
    ),
    ("a9", "Chooses the shape of the random draws."),
    (
        "a9.dist",
        "Uniform draws all values alike; Gaussian favors the middle; Triangular leans to the middle; Exponential favors small changes.",
    ),
    (
        "a10",
        "Learns the selection's intervals and note spacing and generates random notes with the same feel.",
    ),
    (
        "n01",
        "Gives each region (bar, group of bars or marker section) its own seed, so regions vary independently.",
    ),
    (
        "n01.region",
        "The size of each region: the whole range, one bar, N bars, or one marker section.",
    ),
    ("n01.bars", "For N Bars: how many bars one region spans."),
    ("n01.step", "How far apart the regions' seeds are."),
    (
        "n02",
        "Generates the rhythm as a Markov chain: whether a step sounds depends on whether the step before did.",
    ),
    ("n02.nn", "The chance that a note follows a note."),
    ("n02.rn", "The chance that a note follows a rest."),
    (
        "n02.grid",
        "The step of the Markov rhythm, as a note value.",
    ),
    (
        "n03",
        "Draws the random notes from chord tones of a progression.",
    ),
    (
        "n03.chords",
        "The progression as scale degrees (1 = the tonic chord); the list repeats.",
    ),
    ("n03.len", "How long each chord lasts."),
    (
        "n03.type",
        "The chord built on each degree: triad, seventh, ninth or suspended.",
    ),
    (
        "n03.tension",
        "The share of notes taken from outside the chord (tension notes).",
    ),
    (
        "n04",
        "Scatters bursts of clustered notes over the selection.",
    ),
    ("n04.rate", "How many bursts per bar on average."),
    ("n04.notes", "How many notes each burst holds."),
    (
        "n04.time",
        "How far the notes of a burst spread in time, in ticks.",
    ),
    (
        "n04.pitch",
        "How far the notes of a burst spread in pitch, in semitones.",
    ),
    (
        "n05",
        "Fills a shape drawn by an upper and a lower edge with random notes.",
    ),
    (
        "n05.upper",
        "The shape's upper edge across the selection, as a share of the generating range (Octave … Octave + Range).",
    ),
    (
        "n05.lower",
        "The shape's lower edge across the selection, as a share of the generating range (Octave … Octave + Range).",
    ),
    (
        "n05.density",
        "How many notes per beat and per semitone fill the shape.",
    ),
    (
        "n05.soft",
        "How many semitones the edges fade over, so the shape's border is ragged rather than sharp.",
    ),
    (
        "n06",
        "Repeats a short random motif, changing it a little on every repeat.",
    ),
    ("n06.beats", "How many beats the motif lasts."),
    (
        "n06.mutation",
        "The share of the motif's notes changed on each repeat.",
    ),
    (
        "n06.mutate",
        "What a change alters: the pitch, the onset, whether the note plays, or all of these.",
    ),
    (
        "n06.transpose",
        "The transposition of each repeat in turn, in semitones; the list repeats.",
    ),
    (
        "n07",
        "For Randomize Levels: moves some selected notes a few scale degrees up or down (seeded).",
    ),
    ("n07.prob", "The chance that a note (or chord) is moved."),
    ("n07.step", "The largest move, in scale degrees."),
    (
        "n07.move",
        "Each Note moves on its own; Each Chord as a Block moves a chord's notes together.",
    ),
    ("n08", "Adds quieter echoes after some of the notes."),
    ("n08.prob", "The chance that a note gets echoes."),
    ("n08.most", "The most echoes a note can get."),
    (
        "n08.delays",
        "The possible gaps, in ticks, before each echo (one is picked at random for each).",
    ),
    (
        "n08.decay",
        "How much quieter each echo is than the one before.",
    ),
    (
        "n09",
        "Divides each cell into a random tuplet (3, 4, 5 …) and places the random notes on it.",
    ),
    ("n09.subs", "The subdivisions a cell may take."),
    (
        "n09.weights",
        "The relative chance of each subdivision, in the same order.",
    ),
    (
        "n09.cell",
        "The span divided into a tuplet: a beat, half a bar, or a bar.",
    ),
    (
        "n10",
        "Shapes the random notes by their place in the bar: each grid position has its own population and velocity.",
    ),
    ("n10.grid", "The grid the bar is divided into."),
    (
        "n10.weights",
        "The population weight of each grid position in turn, in percent; the list repeats.",
    ),
    (
        "n10.vel",
        "The velocity of each grid position in turn; the list repeats.",
    ),
    (
        "n11",
        "Generates random notes only where other notes are silent.",
    ),
    (
        "n11.in",
        "Whose silence counts: the selection's, the whole track's, or all tracks'.",
    ),
    (
        "n11.rest",
        "Only rests at least this long, in ticks, are filled.",
    ),
    (
        "n11.margin",
        "Keeps this many ticks free at both ends of every rest.",
    ),
];

#[allow(clippy::too_many_lines)]
fn rnd_spec() -> ToolSpec {
    let mut p = vec![
        choice("mode", "Mode", RND_MODES, 1),
        int("octave", "Octave", 0, 9, 4).keyed_octaves(),
        int("range", "Range (octaves)", 1, 5, 1),
        choice("key", "Key", &KEY_NAMES, 0),
        choice("scale", "Scale", scale_names(), 0),
        choice("length", "Length", NOTE_VALUES, 4),
        ticks("length.ticks", "Custom length", 1, 1 << 40, 240),
        float(
            "variation",
            "Length variation (shorter ↔ longer)",
            -1.0,
            1.0,
            0.0,
            "",
        ),
        float("population", "Population", 0.0, 100.0, 50.0, "%"),
        int("stack", "Stack (polyphony)", 1, 16, 1),
        flag("merge", "Merge same notes", false),
        float("vel", "Velocity randomization", -100.0, 100.0, 30.0, "%"),
        float("rel", "Release randomization", -100.0, 100.0, 0.0, "%"),
        flag("bipolar", "Bipolar", true),
        flag("reset", "Reset levels before processing", false),
        seed("seed"),
        adv_switch(1, "Random walk"),
        int("a1.step", "Largest step (scale degrees)", 1, 12, 2).adv(1),
        adv_switch(2, "Weighted scale degrees"),
        list(
            "a2.weights",
            "Weight per degree",
            0,
            100,
            16,
            &[8, 2, 5, 2, 6, 3, 1],
        )
        .adv(2),
        adv_switch(3, "Per-pitch probability"),
        list(
            "a3.prob",
            "C … B keep probability %",
            0,
            100,
            12,
            &[100, 50, 100, 50, 100, 100, 50, 100, 50, 100, 50, 100],
        )
        .adv(3),
        adv_switch(4, "Per-subchannel probability"),
        list("a4.weights", "Weight by subchannel", 0, 100, 4096, &[1, 1]).adv(4),
        adv_switch(5, "Density envelope"),
        curve(
            "a5.curve",
            "Population across the range",
            Curve::line(0.2, 1.0),
        )
        .adv(5),
        adv_switch(6, "Overlap constraint solver"),
        int("a6.max", "Most simultaneous notes", 1, 16, 2).adv(6),
        adv_switch(7, "Symmetry-preserving"),
        choice(
            "a7.axis",
            "Mirror",
            &["Time (palindrome)", "Pitch (around the center)"],
            0,
        )
        .adv(7),
        adv_switch(8, "Seed morphing"),
        seed("a8.seed").adv(8),
        float("a8.mix", "Seed A → seed B", 0.0, 1.0, 0.5, "").adv(8),
        adv_switch(9, "Distribution choice"),
        choice(
            "a9.dist",
            "Distribution",
            super::super::rng::Distribution::NAMES,
            1,
        )
        .adv(9),
        adv_switch(10, "Pattern-conditioned randomness"),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Seed lattice", false),
        choice("n01.region", "Region", RND_REGIONS, 1),
        int("n01.bars", "Bars per region", 1, 64, 1),
        int("n01.step", "Region seed step", 1, 1_000_000, 1),
        flag("n02", "Markov rhythm", false),
        float("n02.nn", "Note → note", 0.0, 100.0, 70.0, "%"),
        float("n02.rn", "Rest → note", 0.0, 100.0, 25.0, "%"),
        choice("n02.grid", "Markov grid", art_grid_values(), 4),
        flag("n03", "Chord-tone generation", false),
        list(
            "n03.chords",
            "Chords (scale degrees)",
            1,
            7,
            64,
            &[1, 6, 4, 5],
        ),
        choice("n03.len", "Chord length", RND_CHORD_LEN, 0),
        choice("n03.type", "Chord type", RND_CHORD_TYPES, 0),
        float("n03.tension", "Tension notes", 0.0, 100.0, 10.0, "%"),
        flag("n04", "Cluster bursts", false),
        float("n04.rate", "Bursts per bar", 0.0, 64.0, 2.0, ""),
        int("n04.notes", "Notes per burst", 1, 4096, 12),
        ticks("n04.time", "Time spread", 0, 1 << 40, 120),
        int("n04.pitch", "Pitch spread (semitones)", 0, 48, 5),
        flag("n05", "Shape fill", false),
        curve(
            "n05.upper",
            "Upper edge",
            Curve::smooth(vec![(0.0, 0.5), (0.5, 1.0), (1.0, 0.5)]),
        ),
        curve(
            "n05.lower",
            "Lower edge",
            Curve::smooth(vec![(0.0, 0.5), (0.5, 0.0), (1.0, 0.5)]),
        ),
        float(
            "n05.density",
            "Density (notes per beat per semitone)",
            0.01,
            64.0,
            0.5,
            "",
        ),
        int("n05.soft", "Edge softness (semitones)", 0, 24, 2),
        flag("n06", "Motif mutation", false),
        int("n06.beats", "Motif length (beats)", 1, 64, 4),
        float("n06.mutation", "Mutation per repeat", 0.0, 100.0, 20.0, "%"),
        choice("n06.mutate", "Mutate", RND_MUTATE, 3),
        list(
            "n06.transpose",
            "Transposition per repeat (semitones)",
            -24,
            24,
            64,
            &[0],
        ),
        flag("n07", "Pitch scatter", false),
        float("n07.prob", "Scatter probability", 0.0, 100.0, 50.0, "%"),
        int("n07.step", "Largest step (scale degrees)", 1, 14, 2),
        choice("n07.move", "Move", RND_MOVE, 0),
        flag("n08", "Stochastic echoes", false),
        float("n08.prob", "Echo probability", 0.0, 100.0, 50.0, "%"),
        int("n08.most", "Most echoes", 1, 16, 3),
        list(
            "n08.delays",
            "Delays (ticks)",
            1,
            100_000,
            32,
            &[120, 240, 360],
        ),
        float("n08.decay", "Decay per echo", 0.0, 90.0, 30.0, "%"),
        flag("n09", "Tuplet dice", false),
        list("n09.subs", "Subdivisions", 1, 32, 32, &[3, 4, 5, 6, 7]),
        list("n09.weights", "Weights", 0, 100, 32, &[30, 40, 10, 15, 5]),
        choice("n09.cell", "Cell", RND_CELLS, 0),
        flag("n10", "Metric profile", false),
        choice("n10.grid", "Profile grid", art_grid_values(), 3),
        list(
            "n10.weights",
            "Population weight per position",
            0,
            100,
            64,
            &[100, 30, 60, 30, 80, 30, 60, 30],
        ),
        list(
            "n10.vel",
            "Velocity per position",
            1,
            127,
            64,
            &[120, 70, 95, 70, 110, 70, 95, 70],
        ),
        flag("n11", "Rest-only generation", false),
        choice("n11.in", "Silent in", RND_SILENT, 0),
        ticks("n11.rest", "Shortest rest", 0, 1 << 40, 240),
        ticks("n11.margin", "Margin", 0, 1 << 40, 30),
    ];
    p.extend(boundary::specs(PitchPolicy::Clamp));
    with_help(&mut p, RANDOMIZE_HELP);
    ToolSpec {
        id: "randomize",
        name: "Randomizer",
        family: Family::Fl,
        summary: "Generates random notes in a key and range over the selection's time span, or randomizes the selected notes' levels (repeatable with the seed).",
        params: p,
        advanced: vec![
            "Random walk",
            "Weighted scale degrees",
            "Per-pitch probability",
            "Per-subchannel probability",
            "Density envelope",
            "Overlap constraint solver",
            "Symmetry-preserving",
            "Seed morphing",
            "Distribution choice",
            "Pattern-conditioned randomness",
        ],
        functions: [
            ("n01", "Seed lattice", 110),
            ("n02", "Markov rhythm", 120),
            ("n03", "Chord-tone generation", 130),
            ("n04", "Cluster bursts", 140),
            ("n05", "Shape fill", 150),
            ("n06", "Motif mutation", 160),
            ("n07", "Pitch scatter", 170),
            ("n08", "Stochastic echoes", 600),
            ("n09", "Tuplet dice", 180),
            ("n10", "Metric profile", 190),
            ("n11", "Rest-only generation", 200),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// The Randomizer's v0.9 functions declared by id (TOOL_CATALOG
/// `randomize` n01 … n11). `None` = switched off; with every one off the
/// kernel is the v0.8 one.
#[derive(Clone, Default)]
struct RndNew {
    /// n01: region (0 whole range, 1 bar, 2 N bars, 3 marker section), bars
    /// per region, region seed step.
    lattice: Option<(usize, u64, u64)>,
    /// n02: note → note and rest → note probabilities, grid ticks.
    markov: Option<(f64, f64, u64)>,
    /// n03: chord degrees (0-based), chord length (0 bar, 1 two bars, 2 four
    /// bars, 3 half note, 4 quarter), chord degrees above the root, tension
    /// share.
    chords: Option<(Vec<i64>, usize, &'static [i64], f64)>,
    /// n04: bursts per bar, notes per burst, time spread, pitch spread.
    bursts: Option<(f64, u64, u64, i64)>,
    /// n05: upper / lower edge, density, edge softness.
    shape: Option<(Curve, Curve, f64, i64)>,
    /// n06: motif length (ticks), mutation share, mutate (0 pitch, 1 onset,
    /// 2 presence, 3 all), transposition per repeat.
    motif: Option<(u64, f64, usize, Vec<i64>)>,
    /// n07: probability, largest step, each chord as a block.
    scatter: Option<(f64, i64, bool)>,
    /// n08: probability, most echoes, delays, decay per echo.
    echoes: Option<(f64, u64, Vec<u64>, f64)>,
    /// n09: subdivisions, their weights, cell (0 beat, 1 half bar, 2 bar).
    tuplets: Option<(Vec<u64>, Vec<f64>, usize)>,
    /// n10: grid ticks, weight per position (fractions), velocity per
    /// position.
    profile: Option<(u64, Vec<f64>, Vec<i64>)>,
    /// n11: silent in (0 the selection, 1 the track, 2 all tracks), shortest
    /// rest, margin.
    rests: Option<(usize, u64, u64)>,
}

impl RndNew {
    fn from(p: &Params, ctx: &super::super::Ctx) -> RndNew {
        let pct = |k: &str| p.float(k) / 100.0;
        let musical = |k: &str| note_value(p.choice(k).min(art_grid_values().len() - 1), 1, ctx);
        RndNew {
            lattice: p.flag("n01").then(|| {
                (
                    p.choice("n01.region"),
                    p.int("n01.bars").max(1) as u64,
                    p.int("n01.step").max(1) as u64,
                )
            }),
            markov: p
                .flag("n02")
                .then(|| (pct("n02.nn"), pct("n02.rn"), musical("n02.grid").max(1))),
            chords: p.flag("n03").then(|| {
                let mut degs: Vec<i64> = p
                    .list("n03.chords")
                    .iter()
                    .map(|&d| d.clamp(1, 7) - 1)
                    .collect();
                if degs.is_empty() {
                    degs.push(0);
                }
                let shape: &'static [i64] = match p.choice("n03.type") {
                    0 => &[0, 2, 4],
                    1 => &[0, 2, 4, 6],
                    2 => &[0, 2, 4, 6, 8],
                    _ => &[0, 3, 4],
                };
                (degs, p.choice("n03.len"), shape, pct("n03.tension"))
            }),
            bursts: p.flag("n04").then(|| {
                (
                    p.float("n04.rate").max(0.0),
                    p.int("n04.notes").max(1) as u64,
                    p.ticks("n04.time"),
                    p.int("n04.pitch").max(0),
                )
            }),
            shape: p.flag("n05").then(|| {
                (
                    p.curve("n05.upper"),
                    p.curve("n05.lower"),
                    p.float("n05.density").max(0.0),
                    p.int("n05.soft").max(0),
                )
            }),
            motif: p.flag("n06").then(|| {
                let mut tr = p.list("n06.transpose");
                if tr.is_empty() {
                    tr.push(0);
                }
                (
                    (p.int("n06.beats").max(1) as u64).saturating_mul(ctx.ppq.max(1)),
                    pct("n06.mutation"),
                    p.choice("n06.mutate"),
                    tr,
                )
            }),
            scatter: p.flag("n07").then(|| {
                (
                    pct("n07.prob"),
                    p.int("n07.step").max(1),
                    p.choice("n07.move") == 1,
                )
            }),
            echoes: p.flag("n08").then(|| {
                let mut d: Vec<u64> = p
                    .list("n08.delays")
                    .iter()
                    .map(|&x| x.max(1) as u64)
                    .collect();
                if d.is_empty() {
                    d.push(ctx.ppq.max(1));
                }
                (
                    pct("n08.prob"),
                    p.int("n08.most").max(1) as u64,
                    d,
                    pct("n08.decay").clamp(0.0, 0.9),
                )
            }),
            tuplets: p.flag("n09").then(|| {
                let subs: Vec<u64> = p
                    .list("n09.subs")
                    .iter()
                    .map(|&s| s.clamp(1, 32) as u64)
                    .collect();
                let subs = if subs.is_empty() { vec![4] } else { subs };
                let w = p.list("n09.weights");
                let weights = (0..subs.len())
                    .map(|i| w.get(i).map_or(1.0, |&x| x.max(0) as f64))
                    .collect();
                (subs, weights, p.choice("n09.cell"))
            }),
            profile: p.flag("n10").then(|| {
                let w: Vec<f64> = p
                    .list("n10.weights")
                    .iter()
                    .map(|&x| x as f64 / 100.0)
                    .collect();
                let v = p.list("n10.vel");
                (
                    musical("n10.grid").max(1),
                    if w.is_empty() { vec![1.0] } else { w },
                    if v.is_empty() { vec![100] } else { v },
                )
            }),
            rests: p.flag("n11").then(|| {
                (
                    p.choice("n11.in"),
                    p.ticks("n11.rest"),
                    p.ticks("n11.margin"),
                )
            }),
        }
    }

    /// Whether the generation reads the meter or the markers.
    fn needs_ctx(&self) -> bool {
        self.lattice.is_some()
            || self.chords.is_some()
            || self.shape.is_some()
            || self.bursts.is_some()
            || self.tuplets.is_some()
            || self.profile.is_some()
    }

    /// Whether a generator other than the slot clock fills the range.
    fn other_generator(&self) -> bool {
        self.bursts.is_some() || self.shape.is_some()
    }

    /// Whether anything may put a generated note outside the selection's
    /// range (aligned regions, bursts, motifs) — the run then clips.
    fn may_leave_range(&self) -> bool {
        self.lattice.is_some()
            || self.bursts.is_some()
            || self.shape.is_some()
            || self.motif.is_some()
            || self.tuplets.is_some()
    }
}

/// Cells covering `[a, b)`: beats (`kind` 0), half bars (1) or bars (2) of
/// the meter — `(start, length)`, the first holding `a`. `None` past `cap`
/// cells (refused at once when even the longest cells would need more).
fn rnd_cells(
    meter: &crate::view::grid::MeterMap,
    ppq: u64,
    a: u64,
    b: u64,
    kind: usize,
    cap: u64,
) -> Option<Vec<(u64, u64)>> {
    // The longest bar a signature allows: 255 whole notes.
    let longest = ppq.max(1).saturating_mul(4 * 255);
    if b.saturating_sub(a) / longest > cap {
        return None;
    }
    let mut v = Vec::new();
    let mut t = a;
    while t < b {
        let m = metric_position(meter, t);
        let (s, len) = match kind {
            0 => (m.beat_start, m.beat_len.max(1)),
            1 => {
                let h = (m.bar_len / 2).max(1);
                if t - m.bar_start < h {
                    (m.bar_start, h)
                } else {
                    (m.bar_start + h, (m.bar_len - h).max(1))
                }
            }
            _ => (m.bar_start, m.bar_len.max(1)),
        };
        v.push((s, len));
        if v.len() as u64 > cap {
            return None;
        }
        let next = s.saturating_add(len);
        if next <= t {
            break;
        }
        t = next;
    }
    Some(v)
}

/// One generated note: start, length, pitch, base velocity.
type Gen = (u64, u64, i64, u8);

/// n03: a chord's tones and tension tones in the range.
type ChordPitches = (Vec<i64>, Vec<i64>);

/// A region of the seed lattice: `[start, end)`, its seed and seed B (a8).
#[derive(Clone, Copy, Debug)]
struct Region {
    start: u64,
    end: u64,
    seed: u64,
    seed_b: u64,
}

/// The Randomizer's generation settings (shared by the slot clock and the
/// v0.9 generators).
struct RndGen<'a> {
    new: &'a RndNew,
    /// Control Sweeps evaluated at each generated unit (E0422).
    sp: Option<&'a SweptParams>,
    ctx: Option<&'a super::super::Ctx>,
    scale: &'a Scale,
    lo_deg: i64,
    n_deg: i64,
    len: u64,
    variation: f64,
    t0: u64,
    t1: u64,
}

impl RndGen<'_> {
    /// Float parameter `key` (the value `base` without a sweep of it) at
    /// a unit starting at `t` (E0422: Time-measured).
    fn f_at(&self, key: &str, base: f64, t: u64) -> f64 {
        match self.sp {
            Some(sp) if sp.drives(key) => sp.float(key, &rnd_unit(t), Place::unit(0, 1)),
            _ => base,
        }
    }

    /// [`RndGen::f_at`] of a share in % (`base` already a fraction).
    fn pct_at(&self, key: &str, base: f64, t: u64) -> f64 {
        match self.sp {
            Some(sp) if sp.drives(key) => sp.float(key, &rnd_unit(t), Place::unit(0, 1)) / 100.0,
            _ => base,
        }
    }

    /// Int parameter `key` at a unit starting at `t`.
    fn i_at(&self, key: &str, base: i64, t: u64) -> i64 {
        match self.sp {
            Some(sp) if sp.drives(key) => sp.int(key, &rnd_unit(t), Place::unit(0, 1)),
            _ => base,
        }
    }

    /// A length with the variation.
    fn length(&self, r: &mut Rng) -> u64 {
        let mut d = self.len as f64;
        if self.variation != 0.0 {
            d *= 1.0 + self.variation * r.unit() * if self.variation > 0.0 { 3.0 } else { 0.9 };
        }
        (d.round() as u64).max(1)
    }

    /// The lowest and highest pitch of the range.
    fn bounds(&self) -> (i64, i64) {
        (
            self.scale.pitch_of(self.lo_deg),
            self.scale.pitch_of(self.lo_deg + self.n_deg),
        )
    }

    /// The selection's place of `t` (0 … 1).
    fn place(&self, t: u64) -> f64 {
        (t.saturating_sub(self.t0) as f64 / (self.t1 - self.t0) as f64).clamp(0.0, 1.0)
    }

    /// n10: the population weight and velocity of the grid position of `t`
    /// in its bar.
    fn profile_at(&self, t: u64) -> Option<(f64, u8)> {
        let ((g, w, v), Some(ctx)) = (self.new.profile.as_ref()?, self.ctx) else {
            return None;
        };
        let m = metric_position(&ctx.meter, t);
        let i = ((t - m.bar_start + g / 2) / g) as usize;
        Some((w[i % w.len()], v[i % v.len()].clamp(1, 127) as u8))
    }

    /// n03: the chord-tone and tension pitches of the range at `t` (chord
    /// index counted from `rs`).
    fn chord_index(&self, t: u64, rs: u64) -> u64 {
        let Some((_, len, _, _)) = &self.new.chords else {
            return 0;
        };
        match (len, self.ctx) {
            (0..=2, Some(ctx)) => {
                let bars = [1, 2, 4][*len];
                let a = metric_position(&ctx.meter, rs).bar;
                metric_position(&ctx.meter, t).bar.saturating_sub(a) / bars
            }
            (0..=2, None) => t.saturating_sub(rs) / (self.len.max(1) * 16),
            (3, _) => {
                t.saturating_sub(rs) / self.ctx.map_or(960, |c| c.ppq).saturating_mul(2).max(1)
            }
            _ => t.saturating_sub(rs) / self.ctx.map_or(960, |c| c.ppq).max(1),
        }
    }

    /// n03: chord tones and tension tones (scale tones off the chord) in the
    /// range for chord `k`.
    fn chord_pitches(&self, k: u64) -> ChordPitches {
        let Some((degs, _, shape, _)) = &self.new.chords else {
            return (Vec::new(), Vec::new());
        };
        let root = degs[(k % degs.len() as u64) as usize];
        let mask = shape.iter().fold(0u16, |m, &d| {
            m | 1 << self.scale.pitch_of(root + d).rem_euclid(12)
        });
        let (lo, hi) = self.bounds();
        let mut tones = Vec::new();
        let mut tension = Vec::new();
        for p in lo..=hi {
            if mask & (1 << p.rem_euclid(12)) != 0 {
                tones.push(p);
            } else if self.scale.contains(p) {
                tension.push(p);
            }
        }
        (tones, tension)
    }

    /// n03: a pitch from chord `pitches` (tension tones at the tension
    /// share).
    fn chord_pitch(&self, r: &mut Rng, pitches: &ChordPitches, t: u64) -> i64 {
        let tension = self.new.chords.as_ref().map_or(0.0, |c| c.3);
        let tension = self.pct_at("n03.tension", tension, t);
        let pool = if !pitches.1.is_empty() && r.chance(tension) || pitches.0.is_empty() {
            &pitches.1
        } else {
            &pitches.0
        };
        if pool.is_empty() {
            return self.bounds().0;
        }
        pool[r.below(pool.len() as u64) as usize]
    }

    /// A pitch for a generator (bursts, shape fill centres): n03's chords, or
    /// any scale pitch in the range.
    fn any_pitch(&self, r: &mut Rng, t: u64, rs: u64) -> i64 {
        if self.new.chords.is_some() {
            let pitches = self.chord_pitches(self.chord_index(t, rs));
            return self.chord_pitch(r, &pitches, t);
        }
        self.scale
            .pitch_of(self.lo_deg + r.below(self.n_deg.max(0) as u64 + 1) as i64)
    }

    /// n04 over `[a, b)`: per bar, `rate` burst centres (the fraction as a
    /// chance), each spawning notes spread in time and pitch around it.
    fn bursts(&self, region: &Region, a: u64, b: u64, out: &mut Vec<Gen>) {
        let (Some((rate, notes, spread, pspread)), Some(ctx)) = (&self.new.bursts, self.ctx) else {
            return;
        };
        let Some(bars) = rnd_cells(&ctx.meter, ctx.ppq, a, b, 2, RND_MAX_CELLS) else {
            return;
        };
        let lat = Lattice::of_scale(self.scale);
        let salt = salt_of("n04");
        for (bs, bl) in bars {
            // Each bar's rate and burst size (its own under a sweep).
            let rate = self.f_at("n04.rate", *rate, bs).max(0.0);
            let notes = &(self.i_at("n04.notes", *notes as i64, bs).max(1) as u64);
            let mut r = stream(region.seed, salt, &[bs]);
            let count = rate.floor() as u64 + u64::from(r.chance(rate.fract()));
            for c in 0..count {
                let mut rc = stream(region.seed, salt, &[bs, c + 1]);
                let centre = bs + rc.below(bl);
                let cp = self.any_pitch(&mut rc, centre, region.start);
                let vel = self.profile_at(centre).map_or(100, |x| x.1);
                for _ in 0..*notes {
                    let dt = rc.range_i64(
                        -(*spread as i64).min(i64::MAX / 2),
                        (*spread as i64).min(i64::MAX / 2),
                    );
                    let s = i128::from(centre) + i128::from(dt);
                    let p = lat.nearest(cp + rc.range_i64(-pspread, *pspread));
                    let d = self.length(&mut rc);
                    if s >= i128::from(a) && s < i128::from(b) {
                        out.push((s as u64, d, p, vel));
                    }
                }
            }
        }
    }

    /// n05 over `[a, b)`: per beat column and scale pitch between the two
    /// edges, `density` notes (the fraction as a chance), tapering over the
    /// soft edge.
    fn shape_fill(&self, region: &Region, a: u64, b: u64, out: &mut Vec<Gen>) {
        let Some((upper, lower, density, soft)) = &self.new.shape else {
            return;
        };
        let beat = self.ctx.map_or(960, |c| c.ppq).max(1);
        let (lo, hi) = self.bounds();
        let salt = salt_of("n05");
        let mut col = a;
        while col < b {
            let x = self.place(col.saturating_add(beat / 2));
            let mut top = lo as f64 + (hi - lo) as f64 * upper.eval(x).clamp(0.0, 1.0);
            let mut bottom = lo as f64 + (hi - lo) as f64 * lower.eval(x).clamp(0.0, 1.0);
            if top < bottom {
                std::mem::swap(&mut top, &mut bottom);
            }
            let from = (bottom.ceil() as i64 - soft).max(0);
            let to = (top.floor() as i64 + soft).min(i64::from(crate::model::note::MAX_PITCH_256));
            let width = beat.min(b - col);
            // The column's density (its own under a sweep).
            let density_at = self.f_at("n05.density", *density, col).max(0.0);
            for p in from..=to {
                if !self.scale.contains(p) {
                    continue;
                }
                let outside = (bottom - p as f64).max(p as f64 - top).max(0.0);
                let w = if outside <= 0.0 {
                    1.0
                } else {
                    1.0 - outside / (*soft as f64 + 1.0)
                };
                if w <= 0.0 {
                    continue;
                }
                let e = density_at * w;
                let mut r = stream(region.seed, salt, &[col, p as u64]);
                let count = e.floor() as u64 + u64::from(r.chance(e.fract()));
                for _ in 0..count {
                    let s = col + r.below(width);
                    let vel = self.profile_at(s).map_or(100, |x| x.1);
                    out.push((s, self.length(&mut r), p, vel));
                }
            }
            col = col.saturating_add(beat);
            if col == u64::MAX {
                break;
            }
        }
    }

    /// n06: the motif `motif` (notes of `[region.start, region.start + len)`)
    /// repeated across the region, each repeat mutating a share of its notes
    /// independently and transposed by the cycled list.
    fn repeat_motif(&self, region: &Region, motif: &[Gen], out: &mut Vec<Gen>) {
        let Some((m, share, kind, transpose)) = &self.new.motif else {
            return;
        };
        let lat = Lattice::of_scale(self.scale);
        let salt = salt_of("n06");
        let mut k = 1u64;
        while let Some(off) = m
            .checked_mul(k)
            .filter(|o| region.start.saturating_add(*o) < region.end)
        {
            let tr = transpose[((k - 1) % transpose.len() as u64) as usize];
            // The repeat's mutation share (its own under a sweep).
            let share = self.pct_at("n06.mutation", *share, region.start.saturating_add(off));
            for (i, &(s, d, p, v)) in motif.iter().enumerate() {
                let mut r = stream(region.seed, salt, &[k, i as u64]);
                let (mut s, mut p) = (s.saturating_add(off), p + tr);
                if r.chance(share) {
                    match if *kind == 3 {
                        r.below(3) as usize
                    } else {
                        *kind
                    } {
                        0 => {
                            let step = [-2, -1, 1, 2][r.below(4) as usize];
                            p = lat.step(p, step);
                        }
                        1 => {
                            let shift = (self.len as i128) * if r.chance(0.5) { 1 } else { -1 };
                            s = (i128::from(s) + shift).clamp(0, i128::from(u64::MAX)) as u64;
                        }
                        _ => continue,
                    }
                }
                if s < region.end {
                    out.push((s, d, p, v));
                }
            }
            k += 1;
        }
    }
}

/// A generated unit starting at `t` (what a Time-measured sweep reads).
fn rnd_unit(t: u64) -> TNote {
    TNote {
        start: t,
        ..TNote::default()
    }
}

/// n11: the stretches of `[t0, t1)` where the reference notes are silent
/// for at least `shortest`, each shrunk by `margin` at both ends.
fn rnd_rests(
    busy: &mut [(u64, u64)],
    t0: u64,
    t1: u64,
    shortest: u64,
    margin: u64,
) -> Vec<(u64, u64)> {
    busy.sort_unstable();
    let mut free = Vec::new();
    let mut at = t0;
    let mut push = |a: u64, b: u64| {
        if b > a && b - a >= shortest.max(1) {
            let (x, y) = (a.saturating_add(margin), b.saturating_sub(margin));
            if y > x {
                free.push((x, y));
            }
        }
    };
    for &(s, e) in busy.iter() {
        if e <= at {
            continue;
        }
        if s > at {
            push(at, s.min(t1));
        }
        at = at.max(e);
        if at >= t1 {
            break;
        }
    }
    if at < t1 {
        push(at, t1);
    }
    free
}

/// Reads the reference notes of the Randomizer's n11 (Rest-only
/// generation): the scoped tracks' unselected notes sounding in the
/// selection's range (`Source::context`), or every track's (`Source::aux`).
pub fn randomize_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("n11") || src.stats.is_empty() || p.choice("mode") == 0 {
        return Ok(());
    }
    let (a, b) = (src.stats.min_start, src.stats.max_end);
    match p.choice("n11.in") {
        1 => {
            let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
            *src = s.with_context_sounding(doc, a, b);
        }
        2 => {
            let mut layer = Vec::new();
            // B2-F16: every track as the song shows it, on these ticks.
            let shift = ctx.map_or(0, |c| c.shift);
            for i in 0..doc.track_count() {
                super::super::source::placed_query(doc, i, shift, a, b, &mut |r| {
                    if layer.len() < limit {
                        layer.push(TNote::from_record(&r, u16::MAX));
                    }
                });
            }
            src.aux = std::sync::Arc::new(vec![layer]);
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn rnd_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let mode = p.choice("mode");
    let scale = Scale::new(p.choice("key"), p.choice("scale"));
    let octave = p.int("octave");
    let range = p.int("range");
    let len = note_value(p.choice("length"), p.ticks("length.ticks"), b.ctx);
    let variation = p.float("variation");
    let population = p.float("population") / 100.0;
    let stack = p.int("stack").max(1) as usize;
    let merge = p.flag("merge");
    let vel_amt = p.float("vel") / 100.0;
    let rel_amt = p.float("rel") / 100.0;
    let bipolar = p.flag("bipolar");
    let reset = p.flag("reset");
    let seed = p.seed("seed");
    let walk = p.adv(1).then(|| p.int("a1.step"));
    let weights = if p.adv(2) {
        p.list("a2.weights")
    } else {
        Vec::new()
    };
    let pc_prob = if p.adv(3) {
        p.list("a3.prob")
    } else {
        Vec::new()
    };
    let sub_w = if p.adv(4) {
        p.list("a4.weights")
    } else {
        Vec::new()
    };
    let dens = p.adv(5).then(|| p.curve("a5.curve"));
    let max_sim = p.adv(6).then(|| p.int("a6.max") as usize);
    let sym = p.adv(7).then(|| p.choice("a7.axis"));
    let morph = p.adv(8).then(|| (p.seed("a8.seed"), p.float("a8.mix")));
    let dist = if p.adv(9) {
        super::super::rng::Distribution::from_index(p.choice("a9.dist"))
    } else {
        super::super::rng::Distribution::Uniform
    };
    let learned = p.adv(10);
    let new = RndNew::from(p, b.ctx);
    // Control Sweeps evaluated at each unit (E0422: the generated slots,
    // bars, columns and repeats at their time, n07's groups by their lead
    // note, n08's echoes by the echoed note) — every pass over the whole
    // selection.
    let sp = SweptParams::of(p, b);
    let song = new.needs_ctx().then(|| b.ctx.clone());
    let markers: Vec<u64> = if new.lattice.as_ref().is_some_and(|l| l.0 == 3) {
        b.ctx
            .markers
            .iter()
            .filter_map(|&m| u64::try_from(i128::from(m) - b.ctx.shift).ok())
            .collect()
    } else {
        Vec::new()
    };
    let span = b.stats.span();
    // Learned rhythms step by the notes' own inter-onset intervals.
    let step_min = if learned {
        b.stats.min_gap.min(len).max(1)
    } else {
        len.max(1)
    };
    let slots = (span / step_min).saturating_add(1);
    let mut est = if mode == 0 {
        b.stats.count as u64
    } else {
        slots
            .saturating_mul(stack as u64)
            .saturating_add(if mode == 1 { b.stats.count as u64 } else { 0 })
    };
    // v0.9: the generators' bounds (the cells they walk are counted here, so
    // an impossible range is refused before anything runs).
    if mode != 0 && !b.stats.is_empty() {
        let refuse = |what: &str| {
            TransformError::TooLarge(format!(
                "The Randomizer would need more than {} {what} across the selection. Select a shorter stretch.",
                crate::midi::group_digits(RND_MAX_CELLS)
            ))
        };
        let ctx = b.ctx;
        let t0 = b.stats.min_start;
        let t1 = b.stats.max_end.max(t0 + 1);
        // Aligned regions may start before the selection (one region).
        let reach = match new.lattice {
            Some((kind @ (1 | 2), bars, _)) => {
                let n = if kind == 2 { bars } else { 1 };
                let (_, s) = super::super::shared::metric_position::bars_region(&ctx.meter, t0, n);
                t0 - s.min(t0)
            }
            Some((3, _, _)) => markers
                .iter()
                .rev()
                .find(|&&m| m <= t0)
                .map_or(0, |&m| t0 - m),
            _ => 0,
        };
        let a = t0 - reach;
        let regions = match new.lattice {
            Some((1, n, _)) | Some((2, n, _)) => {
                let bars = rnd_cells(&ctx.meter, ctx.ppq, a, t1, 2, RND_MAX_CELLS)
                    .ok_or_else(|| refuse("bars"))?
                    .len() as u64;
                bars / if new.lattice.is_some_and(|l| l.0 == 2) {
                    n
                } else {
                    1
                } + 1
            }
            Some((3, _, _)) => markers.len() as u64 + 1,
            _ => 1,
        };
        let mut bound = 0u64;
        if new.other_generator() {
            if let Some((rate, notes, _, _)) = new.bursts {
                // The largest rate and burst the sweeps allow.
                let (rate, notes) = match &sp {
                    Some(sp) => (
                        rate.max(sp.float_bounds("n04.rate").1.max(0.0)),
                        notes.max(sp.int_bounds("n04.notes").1.max(1) as u64),
                    ),
                    None => (rate, notes),
                };
                let bars = rnd_cells(&ctx.meter, ctx.ppq, a, t1, 2, RND_MAX_CELLS)
                    .ok_or_else(|| refuse("bars"))?
                    .len() as u64;
                bound = bound.saturating_add(
                    bars.saturating_mul(rate.ceil() as u64)
                        .saturating_mul(notes),
                );
            }
            if let Some((_, _, density, soft)) = &new.shape {
                let density = &match &sp {
                    Some(sp) => density.max(sp.float_bounds("n05.density").1.max(0.0)),
                    None => *density,
                };
                let cols = (t1 - a) / ctx.ppq.max(1) + 2;
                if cols > RND_MAX_CELLS {
                    return Err(refuse("beats"));
                }
                let lo = scale.pitch_of(scale.degree_of(octave * 12).0);
                let hi = scale
                    .pitch_of(scale.degree_of(octave * 12).0 + scale.steps.len() as i64 * range);
                let rows = (hi - lo + 2 * soft + 1).max(1) as u64;
                bound = bound.saturating_add(
                    cols.saturating_mul(rows)
                        .saturating_mul(density.ceil() as u64),
                );
            }
        } else {
            let slots = if let Some((subs, _, cell)) = &new.tuplets {
                let cells = rnd_cells(&ctx.meter, ctx.ppq, a, t1, *cell, RND_MAX_CELLS)
                    .ok_or_else(|| refuse("cells"))?
                    .len() as u64;
                cells.saturating_mul(subs.iter().copied().max().unwrap_or(1))
            } else {
                let step = new.markov.map_or(step_min, |m| m.2);
                ((t1 - a) / step.max(1)).saturating_add(regions)
            };
            bound = slots.saturating_mul(stack as u64);
        }
        if new.motif.is_some() {
            bound = bound.saturating_mul(2);
        }
        est = bound.saturating_add(if mode == 1 { b.stats.count as u64 } else { 0 });
    }
    if let Some((_, most, _, _)) = &new.echoes {
        est = est.saturating_mul(most.saturating_add(1));
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let base = out.notes.len();
            // Levels of an existing note.
            let level = |v: u8, amt: f64, r: &mut Rng| -> i64 {
                let base = if reset { 100.0 } else { f64::from(v) };
                let u = if bipolar {
                    r.bipolar(dist)
                } else {
                    r.bipolar(dist).abs() * amt.signum()
                };
                (base + u * amt.abs() * 127.0).round() as i64
            };
            // Per-subchannel probability of being randomized (levels mode).
            let sub_index = |n: &TNote| -> usize {
                src.track(n.track)
                    .and_then(|t| t.sub_index(n.sub))
                    .unwrap_or(0)
            };
            // n08 (order 600): echoes of everything emitted since `base`.
            let echo = |out: &mut Output| -> Result<(), TransformError> {
                let Some((prob, most, delays, decay)) = &new.echoes else {
                    return Ok(());
                };
                let salt = salt_of("n08");
                let end = out.notes.len();
                for i in base..end {
                    let n = out.notes[i];
                    // The echoed note's probability and decay (its own under
                    // a sweep).
                    let (prob, decay) = match &sp {
                        Some(sp) => {
                            let pl = Place::unit(i - base, end - base);
                            (
                                sp.float("n08.prob", &n, pl) / 100.0,
                                (sp.float("n08.decay", &n, pl) / 100.0).clamp(0.0, 0.9),
                            )
                        }
                        None => (*prob, *decay),
                    };
                    let mut r = stream(seed, salt, &[note_key(&n), u64::from(n.track)]);
                    if !r.chance(prob) {
                        continue;
                    }
                    let m = 1 + r.below(*most);
                    let mut t = i128::from(n.start);
                    for j in 1..=m {
                        t += i128::from(delays[r.below(delays.len() as u64) as usize]);
                        let v = f64::from(n.vel) * (1.0 - decay).powi(j as i32);
                        out.emit(
                            t,
                            i128::from(n.dur),
                            i64::from(n.pitch),
                            v.round().max(1.0) as i64,
                            &n,
                        );
                    }
                    out.check()?;
                }
                Ok(())
            };
            if mode == 0 || mode == 1 {
                // n07 (levels mode): pitches moved by scale degrees, per note
                // or per chord block.
                let scattered: Vec<i64> = match (&new.scatter, mode) {
                    (Some((prob, step, blocks)), 0) => {
                        let lat = Lattice::of_scale(&scale);
                        let salt = salt_of("n07");
                        let mut pv: Vec<i64> =
                            src.notes.iter().map(|n| i64::from(n.pitch)).collect();
                        let groups: Vec<Vec<usize>> = if *blocks {
                            let mut g = Vec::new();
                            for idx in per_track(src) {
                                for c in chords(src, &idx, 0).iter() {
                                    g.push(c.to_vec());
                                }
                            }
                            g
                        } else {
                            (0..src.notes.len()).map(|i| vec![i]).collect()
                        };
                        // Each group's probability: its lead note's under a
                        // sweep.
                        let places = sp
                            .as_ref()
                            .filter(|s| s.drives("n07.prob"))
                            .map(|_| super::super::sweep::note_places(src));
                        for g in groups {
                            let lead = &src.notes[g[0]];
                            let prob = match (&sp, &places) {
                                (Some(sp), Some(pl)) => {
                                    sp.float("n07.prob", lead, pl[g[0]]) / 100.0
                                }
                                _ => *prob,
                            };
                            let mut r =
                                stream(seed, salt, &[note_key(lead), u64::from(lead.track)]);
                            if !r.chance(prob) {
                                continue;
                            }
                            let v = r.below(2 * *step as u64) as i64 - step;
                            let k = if v >= 0 { v + 1 } else { v };
                            for i in g {
                                pv[i] = lat.step(pv[i], k);
                            }
                        }
                        pv
                    }
                    _ => Vec::new(),
                };
                for (ni, n) in src.notes.iter().enumerate() {
                    let mut v = i64::from(n.vel);
                    let mut tpl = *n;
                    let randomized = match (mode, sub_w.is_empty()) {
                        (0, true) => true,
                        (0, false) => {
                            let w = sub_w[sub_index(n) % sub_w.len()] as f64 / 100.0;
                            Rng::for_note(seed, salt::SUB, n).chance(w.clamp(0.0, 1.0))
                        }
                        _ => false,
                    };
                    if randomized {
                        let mut r = Rng::for_note(seed, salt::VELOCITY, n);
                        v = level(n.vel, vel_amt, &mut r);
                        if rel_amt != 0.0 {
                            let mut r = Rng::for_note(seed, salt::GATE, n);
                            tpl.rel = level(n.rel, rel_amt, &mut r).clamp(0, 127) as u8;
                        }
                    }
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        scattered.get(ni).copied().unwrap_or(i64::from(n.pitch)),
                        v,
                        &tpl,
                    );
                }
                if mode == 0 {
                    echo(out)?;
                    return out.check();
                }
            }
            // Generation over the selection's span.
            let Some(first) = src.notes.first() else {
                return Ok(());
            };
            let t0 = src.stats.min_start;
            let t1 = src.stats.max_end.max(t0 + 1);
            let lo_deg = scale.degree_of(octave * 12).0;
            let n_deg = (scale.steps.len() as i64) * range;
            // Learned interval / IOI histograms (pattern-conditioned).
            let (intervals, iois): (Vec<i64>, Vec<u64>) = if learned {
                let mut iv = Vec::new();
                let mut io = Vec::new();
                for w in src.notes.windows(2) {
                    iv.push(i64::from(w[1].pitch) - i64::from(w[0].pitch));
                    if w[1].start > w[0].start {
                        io.push(w[1].start - w[0].start);
                    }
                }
                (iv, io)
            } else {
                (Vec::new(), Vec::new())
            };
            let g = RndGen {
                new: &new,
                sp: sp.as_ref(),
                ctx: song.as_ref(),
                scale: &scale,
                lo_deg,
                n_deg,
                len,
                variation,
                t0,
                t1,
            };
            // n01: the regions with their own seeds (one region = v0.8).
            let lat_salt = salt_of("n01");
            let region_of = |index: u64, start: u64, end: u64| -> Region {
                let step = new.lattice.map_or(1, |l| l.2);
                let k = index.wrapping_mul(step);
                Region {
                    start,
                    end,
                    seed: region_seed(seed, lat_salt, k),
                    seed_b: morph.map_or(0, |(sb, _)| region_seed(sb, lat_salt, k)),
                }
            };
            let regions: Vec<Region> = match (new.lattice, song.as_ref()) {
                (None, _) => vec![Region {
                    start: t0,
                    end: t1,
                    seed,
                    seed_b: morph.map_or(0, |m| m.0),
                }],
                (Some((1, _, _)), Some(ctx)) | (Some((2, _, _)), Some(ctx)) => {
                    let n = if new.lattice.is_some_and(|l| l.0 == 2) {
                        new.lattice.map_or(1, |l| l.1)
                    } else {
                        1
                    };
                    let (mut index, mut s) =
                        super::super::shared::metric_position::bars_region(&ctx.meter, t0, n);
                    let mut v = Vec::new();
                    while s < t1 && (v.len() as u64) < RND_MAX_CELLS {
                        let mut e = s;
                        for _ in 0..n {
                            let m = metric_position(&ctx.meter, e);
                            e = m.bar_start.saturating_add(m.bar_len.max(1));
                        }
                        v.push(region_of(index, s, e.max(s + 1)));
                        index += 1;
                        if e <= s {
                            break;
                        }
                        s = e;
                    }
                    v
                }
                (Some((3, _, _)), _) => {
                    let k = markers.partition_point(|&m| m <= t0);
                    let mut v = Vec::new();
                    let mut s = k.checked_sub(1).map_or(t0, |j| markers[j]);
                    for (j, &m) in markers.iter().enumerate().skip(k) {
                        if m >= t1 {
                            break;
                        }
                        v.push(region_of(j as u64, s, m));
                        s = m;
                    }
                    let idx = markers.partition_point(|&m| m <= s) as u64;
                    v.push(region_of(idx, s, t1));
                    v
                }
                _ => vec![region_of(0, t0, t1)],
            };
            let mut tpl = *first;
            tpl.group = 0;
            let mut generated: Vec<Gen> = Vec::new();
            let markov_salt = salt_of("n02");
            let tuplet_salt = salt_of("n09");
            for region in &regions {
                let wend = match &new.motif {
                    Some((m, ..)) => region.start.saturating_add(*m).min(region.end),
                    None => region.end,
                };
                let from = generated.len();
                if new.other_generator() {
                    g.bursts(region, region.start, wend, &mut generated);
                    g.shape_fill(region, region.start, wend, &mut generated);
                } else {
                    // n09: the cells' slots, each cell its dice roll.
                    let tuplet_slots: Option<Vec<u64>> = match (&new.tuplets, song.as_ref()) {
                        (Some((subs, w, cell)), Some(ctx)) => {
                            let mut v = Vec::new();
                            for (cs, cl) in rnd_cells(
                                &ctx.meter,
                                ctx.ppq,
                                region.start,
                                wend,
                                *cell,
                                RND_MAX_CELLS,
                            )
                            .unwrap_or_default()
                            {
                                let mut r = stream(region.seed, tuplet_salt, &[cs]);
                                let s = subs[r.weighted(w).unwrap_or(0)];
                                for i in 0..s {
                                    let t = cs
                                        + (u128::from(cl) * u128::from(i) / u128::from(s)) as u64;
                                    if t >= region.start && t < wend {
                                        v.push(t);
                                    }
                                }
                            }
                            v.dedup();
                            Some(v)
                        }
                        _ => None,
                    };
                    let mut walk_deg = lo_deg + n_deg / 2;
                    let mut last_pitch = i64::from(first.pitch);
                    let mut markov_on = vec![false; stack];
                    let mut chord: Option<(u64, ChordPitches)> = None;
                    let mut t = region.start;
                    let mut k: u64 = 0;
                    loop {
                        if let Some(v) = &tuplet_slots {
                            match v.get(k as usize) {
                                Some(&x) => t = x,
                                None => break,
                            }
                        } else if t >= wend {
                            break;
                        }
                        let pos_t = (t.saturating_sub(t0)) as f64 / (t1 - t0) as f64;
                        let mut pop = population;
                        if let Some(c) = &dens {
                            pop *= c.eval(pos_t) * 2.0;
                        }
                        let prof = g.profile_at(t);
                        if let Some((w, _)) = prof {
                            pop *= w;
                        }
                        let slot_seed = match morph {
                            Some((_, mix)) => {
                                let pick =
                                    Rng::for_index(region.seed ^ 0x5EED, salt::ORDER, k).unit();
                                if pick < mix {
                                    region.seed_b
                                } else {
                                    region.seed
                                }
                            }
                            None => region.seed,
                        };
                        let mut r = Rng::for_index(slot_seed, salt::KEEP, k);
                        if new.chords.is_some() {
                            let ci = g.chord_index(t, region.start);
                            if chord.as_ref().is_none_or(|c| c.0 != ci) {
                                chord = Some((ci, g.chord_pitches(ci)));
                            }
                        }
                        // n02's probabilities at this slot (its own under a
                        // sweep).
                        let markov = new.markov.map(|(nn, rn, gr)| {
                            (g.pct_at("n02.nn", nn, t), g.pct_at("n02.rn", rn, t), gr)
                        });
                        for (voice, was_on) in markov_on.iter_mut().enumerate() {
                            let sounds = match markov {
                                // n02: a two-state chain per voice.
                                Some((nn, rn, _)) => {
                                    let p0 = if *was_on { nn } else { rn };
                                    let mut mr = stream(slot_seed, markov_salt, &[k, voice as u64]);
                                    let on =
                                        mr.chance((p0 * prof.map_or(1.0, |x| x.0)).clamp(0.0, 1.0));
                                    *was_on = on;
                                    on
                                }
                                None => r.chance(pop.clamp(0.0, 1.0)),
                            };
                            if !sounds {
                                continue;
                            }
                            let pitch = if let Some((_, pitches)) = &chord {
                                g.chord_pitch(&mut r, pitches, t)
                            } else if learned && !intervals.is_empty() {
                                let iv =
                                    intervals[r.range_i64(0, intervals.len() as i64 - 1) as usize];
                                last_pitch = (last_pitch + iv)
                                    .clamp(scale.pitch_of(lo_deg), scale.pitch_of(lo_deg + n_deg));
                                last_pitch
                            } else if let Some(step) = walk {
                                walk_deg = (walk_deg + r.range_i64(-step, step))
                                    .clamp(lo_deg, lo_deg + n_deg);
                                scale.pitch_of(walk_deg)
                            } else if !weights.is_empty() {
                                let total: i64 = weights.iter().sum::<i64>().max(1);
                                let mut x = r.range_i64(0, total - 1);
                                let mut d = 0usize;
                                for (i, w) in weights.iter().enumerate() {
                                    if x < *w {
                                        d = i;
                                        break;
                                    }
                                    x -= w;
                                }
                                let oct = r.range_i64(0, range - 1);
                                scale.pitch_of(lo_deg + oct * scale.steps.len() as i64 + d as i64)
                            } else {
                                let u = (r.bipolar(dist) + 1.0) / 2.0;
                                scale.pitch_of(lo_deg + (u * n_deg as f64).floor() as i64)
                            };
                            if !pc_prob.is_empty() {
                                let pr = pc_prob
                                    .get(pitch.rem_euclid(12) as usize)
                                    .copied()
                                    .unwrap_or(100) as f64
                                    / 100.0;
                                if !r.chance(pr) {
                                    continue;
                                }
                            }
                            let mut d = len as f64;
                            if variation != 0.0 {
                                d *= 1.0
                                    + variation
                                        * r.unit()
                                        * if variation > 0.0 { 3.0 } else { 0.9 };
                            }
                            let d = (d.round() as u64).max(1);
                            generated.push((t, d, pitch, prof.map_or(100, |x| x.1)));
                        }
                        let step = if tuplet_slots.is_some() {
                            0
                        } else if let Some((_, _, grid)) = new.markov {
                            grid
                        } else if learned && !iois.is_empty() {
                            iois[r.range_i64(0, iois.len() as i64 - 1) as usize].max(1)
                        } else {
                            len
                        };
                        t = t.saturating_add(step);
                        k += 1;
                        if generated.len() as u64 > est.saturating_mul(2).max(1 << 20) {
                            break;
                        }
                        if tuplet_slots.is_none() && step == 0 {
                            break;
                        }
                    }
                }
                if new.motif.is_some() {
                    let motif: Vec<Gen> = generated[from..].to_vec();
                    g.repeat_motif(region, &motif, &mut generated);
                }
                if generated.len() as u64 > est.saturating_mul(2).max(1 << 20) {
                    break;
                }
            }
            if new.may_leave_range() {
                generated.retain(|x| x.0 >= t0 && x.0 < t1);
            }
            if let Some(axis) = sym {
                match axis {
                    0 => {
                        // Palindrome: the second half mirrors the first.
                        let mid = t0 + (t1 - t0) / 2;
                        generated.retain(|g| g.0 < mid);
                        let mirrored: Vec<Gen> = generated
                            .iter()
                            .map(|&(s, d, p, v)| {
                                // In u128: t0 + t1 and s + d may exceed u64.
                                let m = (u128::from(t1) + u128::from(t0))
                                    .saturating_sub(u128::from(s) + u128::from(d));
                                (u64::try_from(m).unwrap_or(u64::MAX), d, p, v)
                            })
                            .collect();
                        generated.extend(mirrored);
                    }
                    _ => {
                        let centre = scale.pitch_of(lo_deg) + scale.pitch_of(lo_deg + n_deg);
                        for g in generated.iter_mut().skip(1).step_by(2) {
                            g.2 = centre - g.2;
                        }
                    }
                }
            }
            // n11: only where the reference notes are silent.
            if let Some((within, shortest, margin)) = new.rests {
                let slot = first.track;
                let mut busy: Vec<(u64, u64)> = match within {
                    0 => src.notes.iter().map(|n| (n.start, n.end())).collect(),
                    1 => src
                        .notes
                        .iter()
                        .chain(src.context.iter())
                        .filter(|n| n.track == slot)
                        .map(|n| (n.start, n.end()))
                        .collect(),
                    _ => src
                        .aux
                        .first()
                        .map(|v| v.iter().map(|n| (n.start, n.end())).collect())
                        .unwrap_or_default(),
                };
                let free = rnd_rests(&mut busy, t0, t1, shortest, margin);
                generated.retain_mut(|x| {
                    let k = free.partition_point(|f| f.1 <= x.0);
                    match free.get(k) {
                        Some(&(a, b)) if x.0 >= a => {
                            x.1 = x.1.min(b - x.0).max(1);
                            true
                        }
                        _ => false,
                    }
                });
            }
            generated.sort_unstable();
            if merge {
                let mut merged: Vec<Gen> = Vec::new();
                for g in generated {
                    match merged.iter_mut().rev().find(|m| m.2 == g.2) {
                        Some(m) if m.0.saturating_add(m.1) >= g.0 => {
                            m.1 = g.0.saturating_add(g.1).max(m.0.saturating_add(m.1)) - m.0
                        }
                        _ => merged.push(g),
                    }
                }
                generated = merged;
            }
            if let Some(max) = max_sim {
                // Greedy: no same-pitch overlap, at most `max` sounding notes.
                let mut kept: Vec<Gen> = Vec::new();
                for g in generated {
                    let sounding = kept
                        .iter()
                        .filter(|k| k.0.saturating_add(k.1) > g.0)
                        .count();
                    let clash = kept
                        .iter()
                        .any(|k| k.2 == g.2 && k.0.saturating_add(k.1) > g.0);
                    if sounding < max && !clash {
                        kept.push(g);
                    }
                }
                generated = kept;
            }
            let tr = src.track(first.track);
            // n01: each note's own dice come from its region's seed (and its
            // place), never from its index among all the generated notes.
            let note_seed = |s: u64| -> u64 {
                let k = regions.partition_point(|r| r.start <= s);
                regions[k.saturating_sub(1)].seed
            };
            for (i, (s, d, p, bv)) in generated.into_iter().enumerate() {
                let mut n = tpl;
                if !sub_w.is_empty()
                    && let Some(tr) = tr
                    && !tr.subs.is_empty()
                {
                    let total: i64 = sub_w.iter().sum::<i64>().max(1);
                    let mut rs = if new.lattice.is_some() {
                        stream(note_seed(s), salt::SUB, &[s, p as u64, d])
                    } else {
                        Rng::for_index(seed, salt::SUB, i as u64)
                    };
                    let mut x = rs.range_i64(0, total - 1);
                    let mut si = 0usize;
                    for (j, w) in sub_w.iter().enumerate() {
                        if x < *w {
                            si = j;
                            break;
                        }
                        x -= w;
                    }
                    let (id, chn) = tr.subs[si % tr.subs.len()];
                    n.sub = id;
                    n.ch = chn;
                }
                let mut r = if new.lattice.is_some() {
                    stream(note_seed(s), salt::VELOCITY, &[s, p as u64, d])
                } else {
                    Rng::for_index(seed, salt::VELOCITY, i as u64)
                };
                let v = level(bv, vel_amt, &mut r);
                out.emit(i128::from(s), i128::from(d), p, v, &n);
                out.check()?;
            }
            echo(out)?;
            Ok(())
        },
        growth: Growth::Generate(est),
        estimate: Some(est),
    })
}

// ================================================================ Strum

pub const STRUM: ToolDef = ToolDef {
    id: "strum",
    spec: strum_spec,
    build: strum_build,
};

const STRUM_TIME_IS: &[&str] = &["Whole chord", "Per note step"];
const STRUM_DIRECTIONS: &[&str] = &["Low → high", "High → low", "Keep"];
const STRUM_FIT_TO: &[&str] = &["Chord end", "Next chord"];
const STRUM_FILL: &[&str] = &["Chord tones", "Scale", "Chromatic"];
const STRUM_GROUPS_FROM: &[&str] = &["Bottom", "Top"];
const STRUM_IN_GROUP: &[&str] = &["Up", "Down", "Together"];
const STRUM_ORDER_BY: &[&str] = &["Velocity", "Subchannel", "Track", "Channel", "Length"];
const STRUM_ASC: &[&str] = &["Ascending", "Descending"];
const STRUM_INDEX_BY: &[&str] = &["Chord count", "Chord size", "Bar position"];
const STRUM_WRAP: &[&str] = &["Saw", "Triangle", "Fan"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const STRUM_HELP: &[(&str, &str)] = &[
    (
        "start.on",
        "Spreads the chord's note starts; off leaves the starts where they are (to shape only ends or velocities).",
    ),
    (
        "time",
        "How many ticks the strum spreads from the first note struck to the last (+ strums low to high, − high to low).",
    ),
    (
        "tension",
        "Speeds the strum up (+) or slows it down (−) as it goes: 0 spaces the notes evenly.",
    ),
    (
        "vel",
        "How much quieter the last note struck is than the first (+), or louder (−), in percent.",
    ),
    (
        "vel.tension",
        "How the velocity change is spread across the strum: 0 evenly, + late, − early.",
    ),
    (
        "preserve_end",
        "Keeps every note's end where it was (later notes become shorter); off moves each end with its start.",
    ),
    (
        "ahead",
        "Starts the strum half its spread early, so it straddles the chord's original time.",
    ),
    (
        "end.on",
        "Also spreads the note ends, by the end time below.",
    ),
    (
        "end.time",
        "How many ticks the last note's end moves relative to the first's (+ later, − earlier).",
    ),
    (
        "end.tension",
        "How the end shift is spread across the notes: 0 evenly, + late, − early.",
    ),
    (
        "chop",
        "Cuts each note where the track's next chord starts.",
    ),
    (
        "alternate",
        "Strums every other chord in the opposite direction.",
    ),
    (
        "tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "a1",
        "Places the strum's notes by a drawn curve instead of evenly.",
    ),
    (
        "a1.curve",
        "Each note's share of the strum time by its place in the chord (left = the first note struck, right = the last).",
    ),
    (
        "a2",
        "Shapes the start spread, the end shift and the velocity change with their own curvatures.",
    ),
    (
        "a2.start",
        "The curvature of the start spread (1 = even, above 1 = slow then fast, below 1 = fast then slow).",
    ),
    ("a2.end", "The curvature of the end shift."),
    (
        "a2.endshift",
        "How many ticks the last note's end moves relative to the first's.",
    ),
    ("a2.vel", "The curvature of the velocity change."),
    (
        "a3",
        "Gives each chord in turn its own strum direction from a list.",
    ),
    (
        "a3.seq",
        "The direction of each chord in turn: 0 up, 1 down; the list repeats.",
    ),
    (
        "a4",
        "Strums high chords faster and low chords slower (or the reverse) around a pivot key.",
    ),
    ("a4.pivot", "The key at which the strum time is unchanged."),
    (
        "a4.amount",
        "How much the strum speeds up per octave above the pivot (and slows below); negative reverses it.",
    ),
    (
        "a5",
        "Strikes each chord's notes in a seeded random order instead of by pitch.",
    ),
    (
        "a6",
        "Delays each subchannel's notes by its own extra time.",
    ),
    (
        "a6.delays",
        "The extra ticks for each subchannel in turn (the list repeats).",
    ),
    (
        "a7",
        "Strums each chord several times, back and forth, within its length.",
    ),
    ("a7.passes", "How many strokes each chord gets."),
    (
        "a8",
        "Makes the strum time grow with the chord's size: a fixed number of ticks per note.",
    ),
    (
        "a8.per",
        "How many ticks the strum adds for each note of the chord.",
    ),
    (
        "a9",
        "Keeps one voice of the chord on the beat; the others are strummed around it.",
    ),
    (
        "a9.voice",
        "The voice that stays on the beat: the lowest, the highest, or the middle one.",
    ),
    (
        "a10",
        "Orders the strum by pitch and subchannel together, spiraling through the layers.",
    ),
    (
        "a10.step",
        "How many semitones one subchannel step counts for in the order.",
    ),
    (
        "n01",
        "Sets the strum time in milliseconds, so it sounds the same at any tempo.",
    ),
    (
        "n01.ms",
        "The strum time in milliseconds (+ low to high, − high to low).",
    ),
    (
        "n01.is",
        "Whole Chord: the time from the first note to the last; Per Note Step: the time between neighboring notes.",
    ),
    (
        "n02",
        "Chooses each chord's strum direction by its place in the bar, like down- and up-strokes.",
    ),
    ("n02.grid", "The grid the bar is divided into."),
    (
        "n02.dirs",
        "The direction at each grid position in turn: 0 down, 1 up, 2 no strum; the list repeats.",
    ),
    (
        "n02.up",
        "How long an up-stroke takes compared with a down-stroke.",
    ),
    ("n03", "Scales each strum to a share of the chord's length."),
    (
        "n03.share",
        "The strum time as a share of the chord's length (or of the time to the next chord).",
    ),
    (
        "n03.to",
        "Measure the chord to its own end, or to the start of the next chord.",
    ),
    (
        "n03.most",
        "The strum never spreads further than this many ticks.",
    ),
    (
        "n04",
        "Leads into each chord with a quick run, like a glissando.",
    ),
    (
        "n04.octaves",
        "How many octaves the run spans before the chord.",
    ),
    (
        "n04.fill",
        "The notes the run steps through: the chord's own tones, the scale, or every semitone.",
    ),
    ("n04.key", "For Scale: the key of the scale."),
    ("n04.scale", "For Scale: the scale the run steps through."),
    (
        "n04.vel",
        "The run's velocity as a share of the chord's first note.",
    ),
    (
        "n05",
        "Picks each chord's direction from the melody's motion: after a leap the strum goes the set way.",
    ),
    (
        "n05.leap",
        "How far, in semitones, a chord must move from the previous one to count as a leap.",
    ),
    (
        "n05.after",
        "The direction after a leap: low to high, high to low, or keep the previous direction.",
    ),
    (
        "n06",
        "Splits each chord into small groups that strum one after another.",
    ),
    ("n06.voices", "How many voices each group holds."),
    (
        "n06.gap",
        "The pause, in ticks, between one group and the next.",
    ),
    (
        "n06.from",
        "Groups are counted from the bottom or the top of the chord.",
    ),
    (
        "n06.order",
        "The order inside a group: up, down, or all together.",
    ),
    (
        "n07",
        "Orders the strum by a note property instead of by pitch.",
    ),
    (
        "n07.by",
        "The property that orders the strum: velocity, subchannel, track, channel or length.",
    ),
    ("n07.dir", "Ascending or descending order of that property."),
    (
        "n08",
        "Gives successive chords their own strum times from a list.",
    ),
    (
        "n08.times",
        "The strum times in turn, in ticks (− = high to low); the list repeats.",
    ),
    (
        "n08.by",
        "What picks the entry: the chord's number, its size, or its place in the bar.",
    ),
    (
        "n08.grid",
        "For Bar Position: the grid the bar is divided into.",
    ),
    ("n08.scale", "Scales every time in the list."),
    (
        "n09",
        "Wraps very large chords into repeated strum windows, so walls of notes ripple instead of one long smear.",
    ),
    ("n09.voices", "How many voices one window holds."),
    ("n09.window", "How long one window lasts, in ticks."),
    (
        "n09.shape",
        "How each window runs: a saw (up, then jump back), a triangle (up and down), or a fan (outward from the middle).",
    ),
    (
        "n10",
        "Spaces the strum by the intervals between the notes: wider gaps take longer.",
    ),
    (
        "n10.per",
        "How many ticks each semitone between neighboring notes adds.",
    ),
    (
        "n10.base",
        "A fixed number of ticks added per voice, whatever the interval.",
    ),
    ("n11", "Places the strummed notes on a grid of steps."),
    ("n11.grid", "The step grid."),
    (
        "n11.steps",
        "How many grid steps separate one voice from the next.",
    ),
    (
        "n11.offset",
        "Moves the whole strum this many grid steps later.",
    ),
];

#[allow(clippy::too_many_lines)]
fn strum_spec() -> ToolSpec {
    let mut p = vec![
        flag("start.on", "Start: enabled", true),
        int(
            "time",
            "Time (ticks; + low → high, − high → low)",
            -100_000,
            100_000,
            120,
        ),
        float("tension", "Tension (+ accelerates)", -100.0, 100.0, 0.0, ""),
        float("vel", "Velocity (+ decreasing)", -100.0, 100.0, 0.0, "%"),
        float("vel.tension", "Velocity tension", -100.0, 100.0, 0.0, ""),
        flag("preserve_end", "Preserve end", false),
        flag("ahead", "Trigger ahead (straddle)", false),
        flag("end.on", "End: enabled", false),
        int("end.time", "End time shift (ticks)", -100_000, 100_000, 120),
        float("end.tension", "End tension", -100.0, 100.0, 0.0, ""),
        flag("chop", "Chop chords", false),
        flag("alternate", "Alternate direction", false),
        ticks("tol", "Chord tolerance", 0, 1 << 30, 0),
        seed("seed"),
        adv_switch(1, "User-drawn strum-time curve"),
        curve(
            "a1.curve",
            "Offset by position in the chord",
            Curve::line(0.0, 1.0),
        )
        .adv(1),
        adv_switch(2, "Independent start / end / velocity curvature"),
        float("a2.start", "Start exponent", 0.2, 5.0, 2.0, "").adv(2),
        float("a2.end", "End exponent", 0.2, 5.0, 0.5, "").adv(2),
        int("a2.endshift", "End shift (ticks)", -100_000, 100_000, 60).adv(2),
        float("a2.vel", "Velocity exponent", 0.2, 5.0, 1.0, "").adv(2),
        adv_switch(3, "Ping-pong direction pattern"),
        list(
            "a3.seq",
            "Directions per chord (0 up, 1 down)",
            0,
            1,
            64,
            &[0, 0, 1],
        )
        .adv(3),
        adv_switch(4, "Register-dependent strum speed"),
        pitch("a4.pivot", "Pivot", 60).adv(4),
        float("a4.amount", "Octaves → speed", -2.0, 2.0, 0.5, "").adv(4),
        adv_switch(5, "Seeded ordering"),
        adv_switch(6, "Subchannel lane delays"),
        list(
            "a6.delays",
            "Extra ticks by subchannel",
            -100_000,
            100_000,
            4096,
            &[0, 30],
        )
        .adv(6),
        adv_switch(7, "Multi-pass strum"),
        int("a7.passes", "Passes", 2, 8, 2).adv(7),
        adv_switch(8, "Chord-size-adaptive spread"),
        int("a8.per", "Ticks per note", 1, 100_000, 30).adv(8),
        adv_switch(9, "Anchor voice"),
        choice("a9.voice", "Anchor", &["Lowest", "Highest", "Middle"], 0).adv(9),
        adv_switch(10, "Spiral ordering (pitch + subchannel)"),
        int("a10.step", "Semitones per subchannel", 1, 24, 7).adv(10),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Real-time strum", false),
        float(
            "n01.ms",
            "Real time (+ low → high, − high → low)",
            -2000.0,
            2000.0,
            60.0,
            "ms",
        ),
        choice("n01.is", "Time is", STRUM_TIME_IS, 0),
        flag("n02", "Metric strum direction", false),
        choice("n02.grid", "Direction grid", art_grid_values(), 3),
        list(
            "n02.dirs",
            "Direction per position (0 down, 1 up, 2 none)",
            0,
            2,
            64,
            &[0, 1],
        ),
        float("n02.up", "Up-stroke time", 10.0, 200.0, 70.0, "%"),
        flag("n03", "Fit to the chord", false),
        float("n03.share", "Share of the chord", 0.0, 100.0, 25.0, "%"),
        choice("n03.to", "Measure to", STRUM_FIT_TO, 0),
        ticks("n03.most", "Largest spread", 0, 1 << 40, 960),
        flag("n04", "Glissando extension", false),
        int("n04.octaves", "Run octaves", 0, 8, 1),
        choice("n04.fill", "Run fill", STRUM_FILL, 0),
        choice("n04.key", "Run key", &KEY_NAMES, 0),
        choice("n04.scale", "Run scale", scale_names(), 0),
        float("n04.vel", "Run velocity", 10.0, 150.0, 70.0, "%"),
        flag("n05", "Continuity direction", false),
        int("n05.leap", "Leap threshold (semitones)", 1, 48, 12),
        choice("n05.after", "After a leap", STRUM_DIRECTIONS, 0),
        flag("n06", "Sub-strums", false),
        int("n06.voices", "Voices per group", 1, 64, 2),
        ticks("n06.gap", "Group gap", 0, 1 << 40, 60),
        choice("n06.from", "Groups from", STRUM_GROUPS_FROM, 0),
        choice("n06.order", "Order inside a group", STRUM_IN_GROUP, 0),
        flag("n07", "Order by property", false),
        choice("n07.by", "Order by", STRUM_ORDER_BY, 0),
        choice("n07.dir", "Order direction", STRUM_ASC, 0),
        flag("n08", "Strum time sequence", false),
        list(
            "n08.times",
            "Times (ticks; − high → low)",
            -100_000,
            100_000,
            64,
            &[120, 60, -90],
        ),
        choice("n08.by", "Index by", STRUM_INDEX_BY, 0),
        choice("n08.grid", "Sequence grid", art_grid_values(), 3),
        float("n08.scale", "Time scale", 0.0, 400.0, 100.0, "%"),
        flag("n09", "Wall wrap", false),
        int("n09.voices", "Voices per window", 2, 4096, 16),
        ticks("n09.window", "Window", 0, 1 << 40, 240),
        choice("n09.shape", "Wrap shape", STRUM_WRAP, 0),
        flag("n10", "Interval-proportional spacing", false),
        float("n10.per", "Ticks per semitone", 0.0, 10_000.0, 10.0, ""),
        ticks("n10.base", "Base time per voice", 0, 1 << 40, 0),
        flag("n11", "Grid-step strum", false),
        choice("n11.grid", "Step grid", art_grid_values(), 6),
        int("n11.steps", "Steps per voice", 1, 64, 1),
        int("n11.offset", "Offset (grid steps)", 0, 63, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, STRUM_HELP);
    ToolSpec {
        id: "strum",
        name: "Strum",
        family: Family::Fl,
        summary: "Spreads the notes of each chord in time (low → high or high → low), with acceleration, velocity change and end shifts.",
        params: p,
        advanced: vec![
            "User-drawn strum-time curve",
            "Independent start / end / velocity curvature",
            "Ping-pong direction pattern",
            "Register-dependent strum speed",
            "Seeded ordering",
            "Subchannel lane delays",
            "Multi-pass strum",
            "Chord-size-adaptive spread",
            "Anchor voice",
            "Spiral ordering (pitch + subchannel)",
        ],
        functions: [
            ("n01", "Real-time strum", 110),
            ("n02", "Metric strum direction", 120),
            ("n03", "Fit to the chord", 130),
            ("n04", "Glissando extension", 140),
            ("n05", "Continuity direction", 150),
            ("n06", "Sub-strums", 160),
            ("n07", "Order by property", 170),
            ("n08", "Strum time sequence", 180),
            ("n09", "Wall wrap", 190),
            ("n10", "Interval-proportional spacing", 200),
            ("n11", "Grid-step strum", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// Strum's v0.9 functions declared by id (TOOL_CATALOG `strum` n01 … n11).
/// `None` = switched off; with every one off the kernel is the v0.8 one.
#[derive(Clone, Default)]
struct StrumNew {
    /// n01: real time in µs (signed), per note step.
    real: Option<(f64, bool)>,
    /// n02: grid, direction per position (0 down, 1 up, 2 none), up-stroke
    /// factor.
    metric: Option<(u64, Vec<i64>, f64)>,
    /// n03: share, measure to the next chord, largest spread.
    fit: Option<(f64, bool, u64)>,
    /// n04: run octaves, fill (0 chord tones, 1 scale, 2 chromatic), the
    /// scale, run velocity factor.
    run: Option<(i64, usize, Scale, f64)>,
    /// n05: leap threshold, after a leap (0 low → high, 1 high → low, 2
    /// keep).
    continuity: Option<(i64, usize)>,
    /// n06: voices per group, gap, groups from the top, order in a group
    /// (0 up, 1 down, 2 together).
    groups: Option<(usize, u64, bool, usize)>,
    /// n07: property (0 velocity, 1 subchannel, 2 track, 3 channel, 4
    /// length), descending.
    order: Option<(usize, bool)>,
    /// n08: times, index by (0 chord count, 1 chord size, 2 bar position),
    /// grid, scale factor.
    sequence: Option<(Vec<i64>, usize, u64, f64)>,
    /// n09: voices per window, window, shape (0 saw, 1 triangle, 2 fan).
    wrap: Option<(usize, u64, usize)>,
    /// n10: ticks per semitone, base per voice.
    intervals: Option<(f64, u64)>,
    /// n11: grid, steps per voice, offset steps.
    steps: Option<(u64, u64, u64)>,
}

impl StrumNew {
    fn from(p: &Params, ctx: &super::super::Ctx) -> StrumNew {
        let pct = |k: &str| p.float(k) / 100.0;
        let musical = |k: &str| note_value(p.choice(k).min(art_grid_values().len() - 1), 1, ctx);
        StrumNew {
            real: p
                .flag("n01")
                .then(|| (p.float("n01.ms") * 1000.0, p.choice("n01.is") == 1)),
            metric: p.flag("n02").then(|| {
                let mut d = p.list("n02.dirs");
                if d.is_empty() {
                    d.push(0);
                }
                (musical("n02.grid").max(1), d, pct("n02.up"))
            }),
            fit: p.flag("n03").then(|| {
                (
                    pct("n03.share"),
                    p.choice("n03.to") == 1,
                    p.ticks("n03.most"),
                )
            }),
            run: p.flag("n04").then(|| {
                (
                    p.int("n04.octaves").max(0),
                    p.choice("n04.fill"),
                    Scale::new(p.choice("n04.key"), p.choice("n04.scale")),
                    pct("n04.vel"),
                )
            }),
            continuity: p
                .flag("n05")
                .then(|| (p.int("n05.leap").max(1), p.choice("n05.after"))),
            groups: p.flag("n06").then(|| {
                (
                    p.int("n06.voices").max(1) as usize,
                    p.ticks("n06.gap"),
                    p.choice("n06.from") == 1,
                    p.choice("n06.order"),
                )
            }),
            order: p
                .flag("n07")
                .then(|| (p.choice("n07.by"), p.choice("n07.dir") == 1)),
            sequence: p.flag("n08").then(|| {
                let mut t = p.list("n08.times");
                if t.is_empty() {
                    t.push(0);
                }
                (
                    t,
                    p.choice("n08.by"),
                    musical("n08.grid").max(1),
                    pct("n08.scale"),
                )
            }),
            wrap: p.flag("n09").then(|| {
                (
                    p.int("n09.voices").max(2) as usize,
                    p.ticks("n09.window"),
                    p.choice("n09.shape"),
                )
            }),
            intervals: p
                .flag("n10")
                .then(|| (p.float("n10.per").max(0.0), p.ticks("n10.base"))),
            steps: p.flag("n11").then(|| {
                (
                    musical("n11.grid").max(1),
                    p.int("n11.steps").max(1) as u64,
                    p.int("n11.offset").max(0) as u64,
                )
            }),
        }
    }
}

/// The strum curve of a place `u` in the chord (v0.8: the drawn curve, the
/// start exponent or the tension warp).
struct StrumShape<'a> {
    a1: Option<&'a Curve>,
    a2: Option<(f64, f64, i64, f64)>,
    tension: f64,
}

impl StrumShape<'_> {
    #[inline(always)]
    fn at(&self, u: f64) -> f64 {
        match (self.a1, self.a2) {
            (Some(c), _) => c.eval(u),
            (None, Some((e, _, _, _))) => u.powf(e),
            _ => warp(u, -self.tension / 25.0),
        }
    }
}

/// n02 / n08: the grid position of `t` in its bar.
fn strum_position(ctx: &super::super::Ctx, t: u64, grid: u64) -> usize {
    let m = metric_position(&ctx.meter, t);
    ((t - m.bar_start + grid / 2) / grid.max(1)) as usize
}

/// n04: the run pitches beyond the strum's first voice `first` (ascending
/// toward it from `octaves` below, or descending from above when `down`),
/// nearest the chord last.
fn strum_run(first: i64, octaves: i64, down: bool, lattice: Lattice) -> Vec<i64> {
    let far = if down {
        first + 12 * octaves
    } else {
        first - 12 * octaves
    };
    let mut v: Vec<i64> = if down {
        (first + 1..=far).filter(|&p| lattice.contains(p)).collect()
    } else {
        (far..first).filter(|&p| lattice.contains(p)).collect()
    };
    if down {
        v.reverse();
    }
    // Farthest first: the run moves toward the chord.
    v
}

/// Strum's settings for one run (the closure's state as a value, so the
/// v0.8 rule can be compiled without the v0.9 stages).
struct StrumEnv {
    start_on: bool,
    time: i64,
    tension: f64,
    vel: f64,
    vt: f64,
    preserve_end: bool,
    ahead: bool,
    end_on: bool,
    end_time: i64,
    end_t: f64,
    chop: bool,
    alternate: bool,
    tol: u64,
    seed: u64,
    a1: Option<Curve>,
    a2: Option<(f64, f64, i64, f64)>,
    a3: Vec<i64>,
    a4: Option<(f64, f64)>,
    a5: bool,
    a6: Vec<i64>,
    a8: Option<i64>,
    a9: Option<usize>,
    a10: Option<i64>,
    passes: u64,
    new: StrumNew,
    song: Option<super::super::Ctx>,
}

/// One Strum run. `NEW` = some v0.9 function is on (false: the v0.8 rule,
/// compiled without them).
#[allow(clippy::too_many_lines)]
fn strum_chords<const NEW: bool>(
    e: &StrumEnv,
    src: &Source,
    out: &mut Output,
    lv: Option<&PerLevel<StrumNew>>,
) -> Result<(), TransformError> {
    let &StrumEnv {
        start_on,
        time,
        tension,
        vel,
        vt,
        preserve_end,
        ahead,
        end_on,
        end_time,
        end_t,
        chop,
        alternate,
        tol,
        seed,
        ref a1,
        a2,
        ref a3,
        a4,
        a5,
        ref a6,
        a8,
        a9,
        a10,
        passes,
        ref new,
        ref song,
    } = e;
    let mut tt = if NEW {
        new.real.and(song.as_ref()).map(TempoTime::new)
    } else {
        None
    };
    let sh = StrumShape {
        a1: a1.as_ref(),
        a2,
        tension,
    };
    let shape = |u: f64| sh.at(u);
    // A function with its own offset law (else the strum curve).
    let own_law = NEW
        && (new.groups.is_some()
            || new.wrap.is_some()
            || new.intervals.is_some()
            || new.steps.is_some());
    for (slot, idx) in per_track(src).iter().enumerate() {
        let chord_list = chords(src, idx, tol);
        // n05: the pitch where the previous strum ended.
        let mut last_end: Option<i64> = None;
        for (ci, g) in chord_list.iter().enumerate() {
            // The chord's functions: its first note's level under Control
            // Sweeps (E0422).
            let new: &StrumNew = lv.map_or(new, |l| l.at(g[0]));
            let tr = &src.tracks[slot];
            let sub_ix = |n: &TNote| tr.sub_index(n.sub).unwrap_or(0) as i64;
            let mut notes: Vec<TNote> = g.iter().map(|&i| src.notes[i]).collect();
            match (if NEW { new.order } else { None }, a10) {
                // n07: by a property, pitch breaking ties.
                (Some((by, desc)), _) => {
                    let key = |n: &TNote| -> u64 {
                        match by {
                            0 => u64::from(n.vel),
                            1 => sub_ix(n) as u64,
                            2 => u64::from(n.track),
                            3 => u64::from(n.ch),
                            _ => n.dur,
                        }
                    };
                    notes.sort_by(|a, b| {
                        let o = key(a).cmp(&key(b));
                        (if desc { o.reverse() } else { o })
                            .then((a.pitch, a.sub).cmp(&(b.pitch, b.sub)))
                    });
                }
                (None, Some(step)) => {
                    notes.sort_by_key(|n| (i64::from(n.pitch) + step * sub_ix(n), n.sub))
                }
                (None, None) => notes.sort_by_key(|n| (n.pitch, n.sub)),
            }
            let chord_start = notes.iter().map(|n| n.start).min().unwrap_or(0);
            // The signed time source: n08 > n01 > Time.
            let seq_time = new
                .sequence
                .as_ref()
                .filter(|_| NEW)
                .map(|(times, by, grid, scale)| {
                    let i = match (by, song.as_ref()) {
                        (0, _) => ci,
                        (1, _) => notes.len().saturating_sub(1),
                        (_, Some(ctx)) => strum_position(ctx, chord_start, *grid),
                        _ => 0,
                    };
                    times[i % times.len()] as f64 * scale
                });
            let mut down = match (seq_time, if NEW { new.real } else { None }) {
                (Some(t), _) => t < 0.0,
                (None, Some((us, _))) => us < 0.0,
                _ => time < 0,
            };
            if alternate && ci % 2 == 1 {
                down = !down;
            }
            if !a3.is_empty() && a3[ci % a3.len()] == 1 {
                down = !down;
            }
            // n02: the direction of the chord's grid position.
            let mut still = false;
            if NEW && let (Some((grid, dirs, _)), Some(ctx)) = (&new.metric, song.as_ref()) {
                match dirs[strum_position(ctx, chord_start, *grid) % dirs.len()] {
                    0 => down = false,
                    1 => down = true,
                    _ => still = true,
                }
            }
            // n05: start at the voice nearest the previous strum's end.
            if NEW && let (Some((leap, after)), Some(prev)) = (new.continuity, last_end) {
                let (a, z) = (
                    i64::from(notes[0].pitch),
                    i64::from(notes[notes.len() - 1].pitch),
                );
                let (da, dz) = ((a - prev).abs(), (z - prev).abs());
                if da.min(dz) > leap {
                    match after {
                        0 => down = false,
                        1 => down = true,
                        _ => {}
                    }
                } else {
                    down = dz < da;
                }
            }
            if down {
                notes.reverse();
            }
            if a5 {
                let mut r =
                    Rng::for_index(seed, salt::ORDER, notes[0].start ^ ((slot as u64) << 50));
                for i in (1..notes.len()).rev() {
                    let j = r.range_i64(0, i as i64) as usize;
                    notes.swap(i, j);
                }
            }
            if NEW && new.continuity.is_some() {
                last_end = Some(i64::from(notes[notes.len() - 1].pitch));
            }
            let k = notes.len();
            let mean = notes.iter().map(|n| f64::from(n.pitch)).sum::<f64>() / k as f64;
            let mut total = time.unsigned_abs() as f64;
            if let Some(per) = a8 {
                total = (per as f64) * (k.saturating_sub(1)) as f64;
            }
            let register = a4.map_or(1.0, |(pivot, amt)| 2f64.powf((pivot - mean) / 12.0 * amt));
            if a4.is_some() {
                total *= register;
            }
            // n01: the strum in real time from the chord's start.
            let mut real_us: Option<f64> = None;
            if NEW && let (Some((us, per_step)), Some(tt)) = (new.real, tt.as_mut()) {
                let mut t_us = us.abs()
                    * if per_step {
                        k.saturating_sub(1) as f64
                    } else {
                        1.0
                    };
                t_us *= register;
                real_us = Some(t_us);
                total = tt.ticks_after(chord_start, t_us.round() as u64) as f64;
            }
            let chord_end = notes.iter().map(|n| n.end()).max().unwrap_or(0);
            // n03: a share of the chord (or of the time to the next).
            if NEW && let Some((share, to_next, most)) = new.fit {
                let measure = if to_next {
                    if ci + 1 < chord_list.len() {
                        src.notes[chord_list.get(ci + 1)[0]]
                            .start
                            .saturating_sub(chord_start)
                    } else {
                        chord_end - chord_start
                    }
                } else {
                    chord_end - chord_start
                };
                total = (share * measure as f64).min(most as f64);
                real_us = None;
            }
            if let Some(t) = seq_time {
                total = t.abs();
                real_us = None;
            }
            if NEW
                && let Some((_, _, up)) = &new.metric
                && down
            {
                total *= up;
                real_us = real_us.map(|u| u * up);
            }
            if still {
                total = 0.0;
                real_us = None;
            }
            // Pitch rank of each voice (n06 groups by pitch).
            let ranks: Vec<usize> = if NEW && new.groups.is_some() {
                let mut ix: Vec<usize> = (0..k).collect();
                ix.sort_by_key(|&i| (notes[i].pitch, notes[i].sub, i));
                let mut r = vec![0; k];
                for (rank, &i) in ix.iter().enumerate() {
                    r[i] = rank;
                }
                r
            } else {
                Vec::new()
            };
            // n10: cumulative interval offsets in strum order.
            let cum: Vec<f64> = match if NEW { new.intervals } else { None } {
                Some((per, base)) => {
                    let mut c = vec![0.0; k];
                    for j in 1..k {
                        let d = (i64::from(notes[j].pitch) - i64::from(notes[j - 1].pitch)).abs();
                        c[j] = c[j - 1] + base as f64 + per * d as f64;
                    }
                    c
                }
                None => Vec::new(),
            };
            // The offset of voice `j` at place `u` (the strum law).
            let mut law = |j: usize, u: f64| -> f64 {
                if still {
                    return 0.0;
                }
                if let Some((grid, per, off)) = new.steps {
                    return ((off + j as u64 * per) * grid) as f64;
                }
                if new.intervals.is_some() {
                    return cum[j];
                }
                if let Some((n, window, kind)) = new.wrap {
                    let (c, w) = (j / n, j % n);
                    let span = (n - 1) as f64;
                    let x = match kind {
                        0 => w as f64 / span,
                        1 => (if c % 2 == 0 { w } else { n - 1 - w }) as f64 / span,
                        _ => (w as f64 - span / 2.0).abs() / (span / 2.0),
                    };
                    return window as f64 * shape(x);
                }
                if let Some((per, gap, from_top, inside)) = new.groups {
                    let r = if from_top { k - 1 - ranks[j] } else { ranks[j] };
                    let (gi, w) = (r / per, r % per);
                    let size = per.min(k - gi * per);
                    // Ascending pitch inside the group.
                    let asc = if from_top { size - 1 - w } else { w };
                    let pos = match inside {
                        0 => asc,
                        1 => size - 1 - asc,
                        _ => 0,
                    };
                    let x = if size > 1 {
                        pos as f64 / (size - 1) as f64
                    } else {
                        0.0
                    };
                    let spread = if inside == 2 { 0.0 } else { total };
                    return gi as f64 * (spread + gap as f64)
                        + if inside == 2 { 0.0 } else { total * shape(x) };
                }
                match (real_us, tt.as_mut()) {
                    (Some(us), Some(tt)) => {
                        tt.ticks_after(chord_start, (us * shape(u)).round() as u64) as f64
                    }
                    _ => total * shape(u),
                }
            };
            let plain = !NEW || (!own_law && !still && real_us.is_none());
            let anchor = a9.map(|v| match v {
                0 => notes
                    .iter()
                    .enumerate()
                    .min_by_key(|x| x.1.pitch)
                    .map_or(0, |x| x.0),
                1 => notes
                    .iter()
                    .enumerate()
                    .max_by_key(|x| x.1.pitch)
                    .map_or(0, |x| x.0),
                _ => k / 2,
            });
            let anchor_off = anchor.map(|a| {
                let ua = if k > 1 {
                    a as f64 / (k - 1) as f64
                } else {
                    0.0
                };
                law(a, ua)
            });
            let pass_len = (chord_end - chord_start) / passes.max(1);
            // n04: the run into the chord, one strum step per note
            // before the first voice (pass 0).
            if NEW
                && let Some((octaves, fill, rscale, rvel)) = &new.run
                && *octaves > 0
                && !still
            {
                let first = notes[0];
                let lattice = match fill {
                    0 => Lattice::of_pitches(notes.iter().map(|n| i64::from(n.pitch))),
                    1 => Lattice::of_scale(rscale),
                    _ => Lattice::chromatic(),
                };
                let low_high = i64::from(notes[k - 1].pitch) >= i64::from(first.pitch);
                let run = strum_run(i64::from(first.pitch), *octaves, !low_high, lattice);
                let u1 = if k > 1 { 1.0 / (k - 1) as f64 } else { 1.0 };
                let off0 = law(0, 0.0);
                let step = if k > 1 { law(1, u1) - off0 } else { total };
                let step = step.abs().max(1.0);
                let shift = anchor_off.unwrap_or(0.0) + if ahead { total / 2.0 } else { 0.0 };
                let r = run.len();
                // V09-0143: the lead-in run's notes have dimension 0.
                out.set_dim(0);
                for (i, &pitch) in run.iter().enumerate() {
                    let off = off0 - (r - i) as f64 * step - shift;
                    let s = i128::from(first.start) + off.round() as i128;
                    let v = (f64::from(first.vel) * rvel).round() as i64;
                    out.emit(s, i128::from(first.dur), pitch, v, &first);
                }
            }
            for pass in 0..passes {
                for (j, n) in notes.iter().enumerate() {
                    // V09-0143: the driver dimension is the place in the
                    // strum order (0 = the first note struck).
                    out.set_dim(j as u32);
                    let u = if k > 1 {
                        j as f64 / (k - 1) as f64
                    } else {
                        0.0
                    };
                    let u = if passes > 1 && pass % 2 == 1 {
                        1.0 - u
                    } else {
                        u
                    };
                    // The v0.8 curve inline unless a function sets the law.
                    let mut off = if !start_on {
                        0.0
                    } else if plain {
                        total * sh.at(u)
                    } else {
                        // A pass going back walks the voices backwards.
                        let jl = if passes > 1 && pass % 2 == 1 {
                            k - 1 - j
                        } else {
                            j
                        };
                        law(jl, u)
                    };
                    if let Some(a) = anchor_off {
                        off -= a;
                    }
                    if ahead {
                        off -= total / 2.0;
                    }
                    if !a6.is_empty() {
                        off += a6[(sub_ix(n) as usize) % a6.len()] as f64;
                    }
                    let base = i128::from(n.start) + i128::from(pass * pass_len);
                    let s = base + off.round() as i128;
                    let mut e = if passes > 1 {
                        base + i128::from(pass_len.max(1))
                    } else {
                        i128::from(n.end())
                    };
                    if !preserve_end && passes == 1 {
                        e += off.round() as i128;
                    }
                    if let Some((_, ee, shift, _)) = a2 {
                        e += (shift as f64 * u.powf(ee)).round() as i128;
                    } else if end_on {
                        e += (end_time as f64 * warp(u, -end_t / 25.0)).round() as i128;
                    }
                    if chop
                        && let Some(&nx) = idx
                            .iter()
                            .map(|&i| &src.notes[i].start)
                            .find(|&&st| st > n.start)
                    {
                        e = e.min(i128::from(nx));
                    }
                    let uv = match a2 {
                        Some((_, _, _, ve)) => u.powf(ve),
                        None => warp(u, -vt / 25.0),
                    };
                    let v = f64::from(n.vel) * (1.0 - vel * uv);
                    out.emit(s, e - s, i64::from(n.pitch), v.round() as i64, n);
                }
                out.check()?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn strum_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let start_on = p.flag("start.on");
    let time = p.int("time");
    let tension = p.float("tension");
    let vel = p.float("vel") / 100.0;
    let vt = p.float("vel.tension");
    let preserve_end = p.flag("preserve_end");
    let ahead = p.flag("ahead");
    let end_on = p.flag("end.on");
    let end_time = p.int("end.time");
    let end_t = p.float("end.tension");
    let chop = p.flag("chop");
    let alternate = p.flag("alternate");
    let tol = p.ticks("tol");
    let seed = p.seed("seed");
    let a1 = p.adv(1).then(|| p.curve("a1.curve"));
    let a2 = p.adv(2).then(|| {
        (
            p.float("a2.start"),
            p.float("a2.end"),
            p.int("a2.endshift"),
            p.float("a2.vel"),
        )
    });
    let a3 = if p.adv(3) {
        p.list("a3.seq")
    } else {
        Vec::new()
    };
    let a4 = p
        .adv(4)
        .then(|| (p.int("a4.pivot") as f64, p.float("a4.amount")));
    let a5 = p.adv(5);
    let a6 = if p.adv(6) {
        p.list("a6.delays")
    } else {
        Vec::new()
    };
    let a7 = p.adv(7).then(|| p.int("a7.passes") as usize);
    let a8 = p.adv(8).then(|| p.int("a8.per"));
    let a9 = p.adv(9).then(|| p.choice("a9.voice"));
    let a10 = p.adv(10).then(|| p.int("a10.step"));
    let passes = a7.unwrap_or(1) as u64;
    let new = StrumNew::from(p, b.ctx);
    // With Control Sweeps (E0422) each chord takes its level's functions;
    // n04's notes per octave bound by the swept range's largest.
    let swept = Rebuild::of(p, b);
    let sctx = b.ctx.clone();
    let run_octaves = match (&swept, b.ctx.sweeps.as_deref()) {
        (Some(_), Some(sw)) => new
            .run
            .as_ref()
            .map(|r| sw.int_bounds("n04.octaves", r.0).1.max(0) as u64),
        _ => None,
    };
    let song = (new.real.is_some()
        || new.metric.is_some()
        || new.sequence.as_ref().is_some_and(|s| s.1 == 2))
    .then(|| b.ctx.clone());
    // n04 adds at most 12 notes per octave per chord (each chord has a note).
    let run_most = run_octaves.map_or_else(
        || new.run.as_ref().map_or(0, |r| 12 * r.0 as u64),
        |o| 12 * o,
    );
    let any_new = new.real.is_some()
        || new.metric.is_some()
        || new.fit.is_some()
        || new.run.is_some()
        || new.continuity.is_some()
        || new.groups.is_some()
        || new.order.is_some()
        || new.sequence.is_some()
        || new.wrap.is_some()
        || new.intervals.is_some()
        || new.steps.is_some();
    let env = StrumEnv {
        start_on,
        time,
        tension,
        vel,
        vt,
        preserve_end,
        ahead,
        end_on,
        end_time,
        end_t,
        chop,
        alternate,
        tol,
        seed,
        a1,
        a2,
        a3,
        a4,
        a5,
        a6,
        a8,
        a9,
        a10,
        passes,
        new,
        song,
    };
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let lv: Option<PerLevel<StrumNew>> = swept
                .as_ref()
                .map(|r| r.levels(&sctx, src, |p, b| StrumNew::from(p, b.ctx)));
            if any_new {
                strum_chords::<true>(&env, src, out, lv.as_ref())
            } else {
                strum_chords::<false>(&env, src, out, lv.as_ref())
            }
        },
        growth: if passes > 1 || run_most > 0 {
            Growth::Expand(passes.max(1).saturating_add(run_most))
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

// ================================================================ Flam

pub const FLAM: ToolDef = ToolDef {
    id: "flam",
    spec: flam_spec,
    build: flam_build,
};

const FLAM_PRESETS: &[&str] = &[
    "Custom",
    "Tight",
    "Loose",
    "Drag (2 strokes)",
    "Ruff (3 strokes)",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const FLAM_HELP: &[(&str, &str)] = &[
    (
        "preset",
        "Ready-made flams: Tight (15 ms), Loose (45 ms), Drag (2 strokes, 25 ms apart), Ruff (3 strokes, 20 ms apart); Custom uses the settings below.",
    ),
    (
        "time",
        "For Custom: the distance between the grace stroke and the note — in milliseconds with Absolute on, in ticks with it off.",
    ),
    (
        "absolute",
        "Measures Time in milliseconds (the same at any tempo); off, in ticks.",
    ),
    (
        "before",
        "Places the grace strokes before the note; off places them after it.",
    ),
    (
        "vel",
        "The grace strokes' velocity as a percentage of the note's.",
    ),
    (
        "group",
        "Puts each note and its grace strokes into a note group of their own.",
    ),
    ("a1", "Sets how many grace strokes each note gets."),
    ("a1.count", "The number of grace strokes per note."),
    (
        "a2",
        "Spaces the strokes unevenly, speeding up or slowing down toward the note.",
    ),
    (
        "a2.dist",
        "+ bunches the strokes furthest from the note and widens the gaps toward it; − bunches them next to the note; 0 = even.",
    ),
    (
        "a3",
        "Ramps the strokes' velocity from the stroke nearest the note to the furthest one.",
    ),
    (
        "a3.from",
        "The velocity next to the note, as a percentage of the note's (the ramp starts here at the nearest stroke's place).",
    ),
    (
        "a3.to",
        "The velocity of the stroke furthest from the note, as a percentage of the note's.",
    ),
    (
        "a4",
        "Ramps the strokes' lengths from the stroke nearest the note to the furthest one.",
    ),
    (
        "a4.from",
        "The length next to the note, as a share of each stroke's spacing (the ramp starts here at the nearest stroke's place).",
    ),
    (
        "a4.to",
        "The length of the stroke furthest from the note, as a share of its spacing.",
    ),
    (
        "a5",
        "Moves each stroke in pitch, so the graces slide into the note.",
    ),
    (
        "a5.step",
        "How many semitones each stroke lies from the next one toward the note (−1 = from a half step below).",
    ),
    (
        "a6",
        "Puts the strokes on other subchannels of the track, in turn.",
    ),
    (
        "a6.seq",
        "How many subchannels each stroke moves from the note's own, stroke by stroke in turn; the list repeats.",
    ),
    (
        "a7",
        "Shifts each stroke a little earlier or later at random (seeded).",
    ),
    (
        "a7.amount",
        "The largest shift, as a share of the spacing between strokes.",
    ),
    (
        "a8",
        "Places the strokes on both sides of the note, mirrored.",
    ),
    (
        "a9",
        "Flams chords as a whole: the strokes come from one voice of each chord, or every voice shares one timing.",
    ),
    (
        "a9.from",
        "The voice the strokes come from: each chord's top note, its bottom note, or every note with shared timing.",
    ),
    (
        "a9.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "a10",
        "Gives every grace stroke its own smaller flam, level after level.",
    ),
    ("a10.depth", "How many levels of flams within flams."),
    (
        "n01",
        "Makes the grace strokes walk toward the note from the previous note or from a fixed interval away.",
    ),
    (
        "n01.from",
        "Where the approach starts: the track's previous note, or a fixed interval from the note.",
    ),
    (
        "n01.interval",
        "For Fixed Interval: how many semitones from the note the approach starts (− = below).",
    ),
    (
        "n01.path",
        "The pitches the strokes step through: every semitone, the scale, or repeats of the start note.",
    ),
    ("n01.key", "For Scale: the key of the scale."),
    (
        "n01.scale",
        "For Scale: the scale the approach steps through.",
    ),
    ("n02", "Flams only the notes at chosen places in the bar."),
    ("n02.grid", "The grid the bar is divided into."),
    (
        "n02.mask",
        "Whether notes at each grid position get a flam: 1 = flam, 0 = none; the list repeats.",
    ),
    (
        "n02.strokes",
        "The number of strokes at each position in turn (empty = the stroke count).",
    ),
    (
        "n03",
        "Flams only accented notes, with more strokes the louder the note.",
    ),
    ("n03.threshold", "Notes at least this loud get a flam."),
    (
        "n03.strokes",
        "The number of strokes a note at velocity 127 gets (quieter accents get fewer).",
    ),
    (
        "n03.spacing",
        "The stroke spacing at velocity 127, as a share of the normal spacing.",
    ),
    (
        "n04",
        "Plays the strokes on other MIDI channels, in turn (channel 10 for drums, say).",
    ),
    (
        "n04.channels",
        "The channels used for the strokes, in turn.",
    ),
    (
        "n04.per",
        "Change the channel at every stroke, or at every note.",
    ),
    (
        "n05",
        "Makes every stroke a small cluster of notes instead of one.",
    ),
    ("n05.size", "How many notes each cluster holds."),
    (
        "n05.kind",
        "The cluster's notes: neighboring semitones, neighboring scale notes, or octaves.",
    ),
    (
        "n05.place",
        "Where the cluster lies: above the stroke's pitch, below it, or centered on it.",
    ),
    ("n05.key", "For Scale: the key of the scale."),
    (
        "n05.scale",
        "For Scale: the scale the cluster is built from.",
    ),
    (
        "n05.curve",
        "The cluster size from the first stroke (left) to the last (right), as a share of Cluster Size.",
    ),
    (
        "n06",
        "Places the strokes at distances from a list instead of evenly.",
    ),
    (
        "n06.offsets",
        "The distance of each stroke from the note, nearest first.",
    ),
    (
        "n06.unit",
        "Whether the offsets are ticks, milliseconds, or percent of Time.",
    ),
    (
        "n07",
        "Adds a mirrored twin to every stroke, the same distance on the other side of the note's pitch.",
    ),
    (
        "n07.spread",
        "How far apart, in steps, each stroke and its twin move per stroke.",
    ),
    (
        "n07.in",
        "Mirror by semitones, or by steps of the scale below.",
    ),
    ("n07.key", "For Scale: the key of the scale."),
    ("n07.scale", "For Scale: the scale the mirror follows."),
    (
        "n07.vel",
        "The twins' velocity as a share of their stroke's.",
    ),
    (
        "n08",
        "Leads into each note with a roll whose rate speeds up (or slows down) in real time.",
    ),
    (
        "n08.lead",
        "How long the roll lasts before the note, in milliseconds.",
    ),
    (
        "n08.from",
        "The roll's rate at its start, in strokes per second.",
    ),
    (
        "n08.to",
        "The roll's rate as it reaches the note, in strokes per second.",
    ),
    ("n08.most", "The most strokes a roll may have."),
    ("n09", "Pulls the strokes toward a grid."),
    ("n09.grid", "The grid the strokes are pulled toward."),
    (
        "n09.strength",
        "How far each stroke moves toward the grid (100 % = onto it).",
    ),
    (
        "n10",
        "Makes the grace strokes chord tones: of the note's own chord, the previous chord, or the note's triad in the key.",
    ),
    (
        "n10.source",
        "Where the tones come from: this chord, the previous chord, or the triad of the note in the key.",
    ),
    (
        "n10.order",
        "The order of the tones toward the note: up, down, or the nearest tone last.",
    ),
    (
        "n10.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    ("n10.key", "For Triad: the key."),
    ("n10.scale", "For Triad: the scale the triad is built in."),
    (
        "n11",
        "Keeps the strokes under a notes-per-second limit, measured in a sliding window.",
    ),
    (
        "n11.nps",
        "The most notes per second a track may play, strokes included.",
    ),
    (
        "n11.window",
        "The length of the sliding window, in milliseconds.",
    ),
    (
        "n11.drop",
        "Which strokes are dropped first: those on the busiest notes, the quietest, or the earliest.",
    ),
];

fn flam_spec() -> ToolSpec {
    let mut p = vec![
        choice("preset", "Options", FLAM_PRESETS, 0),
        float("time", "Time", 1.0, 2000.0, 30.0, ""),
        flag("absolute", "Absolute (time in milliseconds)", true),
        flag("before", "Before", true),
        float("vel", "Velocity", 1.0, 200.0, 60.0, "%"),
        flag("group", "Group notes", false),
        seed("seed"),
        adv_switch(1, "Variable retrigger count"),
        int("a1.count", "Strokes", 1, 16, 3).adv(1),
        adv_switch(2, "Accelerating / decelerating spacing"),
        float("a2.dist", "Acceleration", -100.0, 100.0, 50.0, "%").adv(2),
        adv_switch(3, "Velocity crescendo / decrescendo"),
        float("a3.from", "First stroke", 1.0, 200.0, 30.0, "%").adv(3),
        float("a3.to", "Last stroke", 1.0, 200.0, 90.0, "%").adv(3),
        adv_switch(4, "Gate taper"),
        float("a4.from", "First stroke gate", 5.0, 100.0, 90.0, "%").adv(4),
        float("a4.to", "Last stroke gate", 5.0, 100.0, 30.0, "%").adv(4),
        adv_switch(5, "Pitch-spread flam"),
        int("a5.step", "Semitones per stroke", -24, 24, -1).adv(5),
        adv_switch(6, "Subchannel cycling"),
        list(
            "a6.seq",
            "Subchannel steps per stroke",
            -4096,
            4096,
            64,
            &[1, 2],
        )
        .adv(6),
        adv_switch(7, "Seeded timing variance"),
        float(
            "a7.amount",
            "Variance (of the spacing)",
            0.0,
            100.0,
            30.0,
            "%",
        )
        .adv(7),
        adv_switch(8, "Symmetric before / after"),
        adv_switch(9, "Chord-aware flam"),
        choice(
            "a9.from",
            "Strokes from",
            &[
                "Top note of each chord",
                "Bottom note",
                "Every note (shared timing)",
            ],
            0,
        )
        .adv(9),
        ticks("a9.tol", "Chord tolerance", 0, 1 << 30, 0).adv(9),
        adv_switch(10, "Recursive flam layers"),
        int("a10.depth", "Depth", 1, 3, 1).adv(10),
        // v0.9 (V09-S35): functions declared by id.
        flag("n01", "Approach path", false),
        choice("n01.from", "Approach from", FLAM_FROM, 0),
        int("n01.interval", "Interval", -24, 24, -5),
        choice("n01.path", "Path", FLAM_PATHS, 0),
        choice("n01.key", "Path key", &KEY_NAMES, 0),
        choice("n01.scale", "Path scale", scale_names(), 0),
        flag("n02", "Metric flam mask", false),
        choice("n02.grid", "Mask grid", art_grid_values(), 2),
        list("n02.mask", "Flam at position (1 = flam)", 0, 1, 64, &[0, 1]),
        list(
            "n02.strokes",
            "Strokes per position (empty = the stroke count)",
            0,
            16,
            64,
            &[],
        ),
        flag("n03", "Accent-gated flam", false),
        int("n03.threshold", "Threshold", 1, 127, 90),
        int("n03.strokes", "Strokes at 127", 1, 16, 3),
        float("n03.spacing", "Spacing at 127", 10.0, 200.0, 100.0, "%"),
        flag("n04", "Stroke channel cycle", false),
        list("n04.channels", "Stroke channels", 1, 16, 16, &[10]),
        choice("n04.per", "Cycle per", FLAM_CYCLE_PER, 0),
        flag("n05", "Cluster strokes", false),
        int("n05.size", "Cluster size", 1, 24, 3),
        choice("n05.kind", "Cluster kind", FLAM_CLUSTER_KINDS, 0),
        choice("n05.place", "Cluster place", FLAM_PLACES, 0),
        choice("n05.key", "Cluster key", &KEY_NAMES, 0),
        choice("n05.scale", "Cluster scale", scale_names(), 0),
        curve("n05.curve", "Size over the strokes", Curve::flat(1.0)),
        flag("n06", "Stroke offset list", false),
        list("n06.offsets", "Offsets", 0, 100_000, 64, &[30, 50, 120]),
        choice("n06.unit", "Offset unit", FLAM_OFFSET_UNITS, 0),
        flag("n07", "Mirrored strokes", false),
        int("n07.spread", "Spread per stroke", 1, 24, 2),
        choice("n07.in", "Mirror in", FLAM_MIRROR_IN, 0),
        choice("n07.key", "Mirror key", &KEY_NAMES, 0),
        choice("n07.scale", "Mirror scale", scale_names(), 0),
        float("n07.vel", "Twin velocity", 10.0, 150.0, 80.0, "%"),
        flag("n08", "Roll rate (Hz)", false),
        float("n08.lead", "Lead-in", 1.0, 10_000.0, 250.0, "ms"),
        float("n08.from", "Start rate", 1.0, 200.0, 12.0, "Hz"),
        float("n08.to", "End rate", 1.0, 200.0, 32.0, "Hz"),
        int("n08.most", "Most strokes", 1, 1024, 64),
        flag("n09", "Stroke grid", false),
        choice("n09.grid", "Stroke grid", art_grid_values(), 6),
        float("n09.strength", "Grid strength", 0.0, 100.0, 100.0, "%"),
        flag("n10", "Chord-tone graces", false),
        choice("n10.source", "Tones from", FLAM_TONES_FROM, 0),
        choice("n10.order", "Tone order", FLAM_TONE_ORDER, 0),
        ticks("n10.tol", "Chord tolerance", 0, 1 << 30, 30),
        choice("n10.key", "Triad key", &KEY_NAMES, 0),
        choice("n10.scale", "Triad scale", scale_names(), 0),
        flag("n11", "NPS budget", false),
        int("n11.nps", "Most notes per second", 1, 1_000_000, 2000),
        float("n11.window", "Budget window", 10.0, 2000.0, 1000.0, "ms"),
        choice("n11.drop", "Drop first", FLAM_DROP, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, FLAM_HELP);
    ToolSpec {
        id: "flam",
        name: "Flam",
        family: Family::Fl,
        summary: "Adds quick grace strokes before (or after) each note, at a time and velocity you choose.",
        params: p,
        advanced: vec![
            "Variable retrigger count",
            "Accelerating / decelerating spacing",
            "Velocity crescendo / decrescendo",
            "Gate taper",
            "Pitch-spread flam",
            "Subchannel cycling",
            "Seeded timing variance",
            "Symmetric before / after",
            "Chord-aware flam",
            "Recursive flam layers",
        ],
        functions: [
            ("n01", "Approach path", 110),
            ("n02", "Metric flam mask", 120),
            ("n03", "Accent-gated flam", 130),
            ("n04", "Stroke channel cycle", 140),
            ("n05", "Cluster strokes", 150),
            ("n06", "Stroke offset list", 160),
            ("n07", "Mirrored strokes", 170),
            ("n08", "Roll rate (Hz)", 180),
            ("n09", "Stroke grid", 190),
            ("n10", "Chord-tone graces", 200),
            ("n11", "NPS budget", 700),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

const FLAM_FROM: &[&str] = &["Previous note of the track", "Fixed interval"];
const FLAM_PATHS: &[&str] = &["Chromatic", "Scale", "Repeat the start"];
const FLAM_CYCLE_PER: &[&str] = &["Stroke", "Note"];
const FLAM_CLUSTER_KINDS: &[&str] = &["Chromatic", "Scale", "Octaves"];
const FLAM_PLACES: &[&str] = &["Above", "Below", "Centered"];
const FLAM_OFFSET_UNITS: &[&str] = &["Ticks", "Milliseconds", "% of the time"];
const FLAM_MIRROR_IN: &[&str] = &["Chromatic", "Scale"];
const FLAM_TONES_FROM: &[&str] = &[
    "This chord",
    "Previous chord",
    "Triad of the note in the key",
];
const FLAM_TONE_ORDER: &[&str] = &["Up", "Down", "Nearest last"];
const FLAM_DROP: &[&str] = &[
    "On the busiest notes",
    "The quietest strokes",
    "The earliest strokes",
];

/// The Flam's v0.9 functions declared by id (TOOL_CATALOG `flam` n01 …
/// n11). `None` = switched off; with every one off the kernel runs exactly
/// as in v0.8.
#[derive(Clone, Default)]
struct FlamNew {
    /// n01: from the previous note of the track (else the fixed interval),
    /// interval, path (0 chromatic, 1 scale, 2 repeat the start), lattice.
    approach: Option<(bool, i64, usize, Lattice)>,
    /// n02: grid ticks, flam per position (cycled), strokes per position
    /// (cycled; empty = the stroke count).
    metric: Option<(u64, Vec<i64>, Vec<i64>)>,
    /// n03: threshold, strokes at 127, spacing factor at 127.
    accent: Option<(u8, u64, f64)>,
    /// n04: channels (0-based), cycled per note (else per stroke).
    channels: Option<(Vec<u8>, bool)>,
    /// n05: size, lattice (`None` = octaves), place (0 above, 1 below, 2
    /// centred), size along the strokes.
    cluster: Option<(i64, Option<Lattice>, usize, Curve)>,
    /// n06: offsets, unit (0 ticks, 1 ms, 2 % of the time).
    offsets: Option<(Vec<i64>, usize)>,
    /// n07: spread per stroke, lattice (`None` = chromatic), twin velocity.
    mirror: Option<(i64, Option<Lattice>, f64)>,
    /// n08: lead-in µs, start / end rate (Hz), most strokes.
    roll: Option<(u64, f64, f64, u64)>,
    /// n09: grid ticks, strength.
    grid: Option<(u64, f64)>,
    /// n10: source (0 this chord, 1 previous chord, 2 the note's triad),
    /// order (0 up, 1 down, 2 nearest last), chord tolerance, scale.
    tones: Option<(usize, usize, u64, Scale)>,
    /// n11: notes per second, window µs, drop first (0 the busiest notes,
    /// 1 the quietest strokes, 2 the earliest strokes).
    budget: Option<(u64, u64, usize)>,
}

impl FlamNew {
    fn any(&self) -> bool {
        self.approach.is_some()
            || self.metric.is_some()
            || self.accent.is_some()
            || self.channels.is_some()
            || self.cluster.is_some()
            || self.offsets.is_some()
            || self.mirror.is_some()
            || self.roll.is_some()
            || self.grid.is_some()
            || self.tones.is_some()
            || self.budget.is_some()
    }

    /// Whether a function reads other notes or the tempo map (whole-source).
    fn contextual(&self) -> bool {
        self.approach.as_ref().is_some_and(|a| a.0)
            || self.channels.as_ref().is_some_and(|c| c.1)
            || self.tones.as_ref().is_some_and(|t| t.0 < 2)
            || self.roll.is_some()
            || self.budget.is_some()
    }
}

/// What one note's strokes follow (decided per source note: the n02 / n03
/// count and spacing, the n08 roll, the n01 / n04 / n10 context).
#[derive(Clone, Copy)]
struct FlamPlan<'a> {
    /// Strokes per side.
    count: u64,
    /// Spacing factor (n03).
    spacing: f64,
    /// n08: stroke distances before / after the note, nearest first (the
    /// source note only; recursive layers use `count`).
    roll: Option<(&'a [u64], &'a [u64])>,
    /// n01: the path's start pitch.
    from: i64,
    /// n04 (per note): the note's place in its track.
    index: u64,
    /// n10: the grace tones in time order (end-aligned to the strokes).
    tones: &'a [i64],
}

/// Where Flam's notes go: straight to the output, or — for the n11 budget —
/// also recorded with their stroke index.
trait FlamSink {
    fn stroke(&mut self, s: i128, d: i128, p: i64, v: i64, tpl: &TNote, j: u64);
    fn note(&mut self, n: TNote);
    fn check(&self) -> Result<(), TransformError>;
}

impl FlamSink for Output {
    #[inline]
    fn stroke(&mut self, s: i128, d: i128, p: i64, v: i64, tpl: &TNote, j: u64) {
        // V09-0143: the driver dimension is the stroke's distance index.
        self.set_dim(u32::try_from(j).unwrap_or(u32::MAX));
        self.emit(s, d, p, v, tpl);
    }

    #[inline]
    fn note(&mut self, n: TNote) {
        self.set_dim(0);
        self.push(n);
    }

    fn check(&self) -> Result<(), TransformError> {
        Output::check(self)
    }
}

/// n11: the output plus, for every note it holds from `base` on, the
/// stroke's distance index (0 = a source note). Strokes held back for the
/// Shift policy (negative starts) are not in `out.notes` and not recorded.
struct FlamTracked<'o, const SRC: bool> {
    out: &'o mut Output,
    base: usize,
    rank: Vec<u32>,
    /// The source note of each output note (n11's budget per note under a
    /// Control Sweep, E0422) and the note being written.
    /// (Kept only with `SRC`: under Control Sweeps.)
    src: Vec<u32>,
    cur: u32,
}

impl<const SRC: bool> FlamTracked<'_, SRC> {
    fn sync(&mut self, j: u64) {
        let n = self.out.notes.len() - self.base;
        if self.rank.len() < n {
            self.rank.resize(n, u32::try_from(j).unwrap_or(u32::MAX));
            if SRC {
                self.src.resize(n, self.cur);
            }
        }
    }
}

impl<const SRC: bool> FlamSink for FlamTracked<'_, SRC> {
    fn stroke(&mut self, s: i128, d: i128, p: i64, v: i64, tpl: &TNote, j: u64) {
        self.out.set_dim(u32::try_from(j).unwrap_or(u32::MAX));
        self.out.emit(s, d, p, v, tpl);
        self.sync(j);
    }

    fn note(&mut self, n: TNote) {
        self.out.set_dim(0);
        self.out.push(n);
        self.sync(0);
    }

    fn check(&self) -> Result<(), TransformError> {
        self.out.check()
    }
}

#[derive(Clone)]
struct Flam {
    spacing_ms: Option<f64>,
    spacing_ticks: f64,
    before: bool,
    vel: f64,
    group: bool,
    seed: u64,
    count: u64,
    accel: Option<f64>,
    vramp: Option<(f64, f64)>,
    gramp: Option<(f64, f64)>,
    step: Option<i64>,
    sub_seq: Vec<i64>,
    jitter: Option<f64>,
    both: bool,
    depth: u32,
    ctx: super::super::Ctx,
    subs: Vec<Vec<(u32, u8)>>,
    new: FlamNew,
}

impl Flam {
    fn spacing(&self, at: u64) -> f64 {
        match self.spacing_ms {
            Some(ms) => {
                let s = self.ctx.seconds_at(at);
                let t = self.ctx.tick_at_seconds((s - ms / 1000.0).max(0.0));
                ((at - t.min(at)) as f64).max(1.0)
            }
            None => self.spacing_ticks.max(1.0),
        }
    }

    /// n06: the distance of stroke `j` (1 = nearest) from a note at `start`:
    /// the list cycled (each cycle further out by the last offset), in
    /// ticks, real milliseconds or % of the spacing `sp`.
    #[allow(clippy::too_many_arguments)]
    fn list_offset<'c>(
        &'c self,
        list: &[i64],
        unit: usize,
        j: u64,
        sp: f64,
        scale: f64,
        start: u64,
        side: i128,
        tt: &mut Option<TempoTime<'c>>,
    ) -> f64 {
        let l = list.len() as u64;
        let last = list[list.len() - 1] as f64;
        let raw = list[((j - 1) % l) as usize] as f64 + ((j - 1) / l) as f64 * last;
        match unit {
            0 => raw * scale,
            1 => {
                let us = (raw * 1000.0 * scale).round() as u64;
                let tt = tt.get_or_insert_with(|| TempoTime::new(&self.ctx));
                if side < 0 {
                    start.saturating_sub(tt.tick_before(start, us)) as f64
                } else {
                    tt.ticks_after(start, us) as f64
                }
            }
            _ => raw / 100.0 * sp,
        }
    }

    /// n09: `s` moved toward the nearest point of a `g`-tick grid that
    /// restarts at every bar line, by `strength`.
    fn grid_snap(&self, s: i128, g: u64, strength: f64) -> i128 {
        let g = i128::from(g.max(1));
        let target = match u64::try_from(s) {
            Ok(t) => {
                let base = i128::from(metric_position(&self.ctx.meter, t).bar_start);
                base + ((s - base) as f64 / g as f64).round() as i128 * g
            }
            Err(_) => (s as f64 / g as f64).round() as i128 * g,
        };
        s + ((target - s) as f64 * strength).round() as i128
    }

    /// n08: the roll's stroke distances from the note (nearest first) on
    /// one side: strokes where the phase of a rate gliding linearly in real
    /// time from the start rate (the far end) to the end rate (at the note)
    /// crosses a whole number, the last `most` of them, never at the note
    /// itself or before the song's start; equal ticks merged.
    fn roll_into(
        &self,
        n: &TNote,
        (lead_us, r0, r1, most): (u64, f64, f64, u64),
        after: bool,
        tt: &mut TempoTime<'_>,
        buf: &mut Vec<u64>,
    ) {
        buf.clear();
        let l = lead_us as f64 / 1e6;
        let total = r0 * l + (r1 - r0) * l / 2.0;
        let strokes = (total + 1e-9).floor().max(0.0) as u64;
        let first = strokes.saturating_sub(most);
        let note_us = tt.micros_at(n.start);
        let a = (r1 - r0) / (2.0 * l);
        // From the stroke nearest the note outward.
        for k in (first..strokes).rev() {
            let k = k as f64;
            // τ (seconds from the roll's start) where the phase reaches k.
            let tau = if a.abs() < 1e-12 {
                k / r0
            } else {
                (-r0 + (r0 * r0 + 4.0 * a * k).max(0.0).sqrt()) / (2.0 * a)
            };
            let before_note_us = ((l - tau) * 1e6).round();
            if before_note_us < 1.0 {
                continue;
            }
            let d = before_note_us as u64;
            let tick = if after {
                tt.tick_at_micros(note_us.saturating_add(d))
            } else {
                if d > note_us {
                    continue;
                }
                tt.tick_at_micros(note_us - d)
            };
            let dist = if after {
                tick.saturating_sub(n.start)
            } else {
                n.start.saturating_sub(tick)
            };
            if dist > 0 && buf.last().is_none_or(|&b| dist > b) {
                buf.push(dist);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn strokes<'c, const NEW: bool, S: FlamSink>(
        &'c self,
        n: &TNote,
        key: u64,
        out: &mut S,
        depth: u32,
        scale: f64,
        plan: &FlamPlan<'_>,
        tt: &mut Option<TempoTime<'c>>,
    ) {
        let sp = if NEW {
            self.spacing(n.start) * scale * plan.spacing
        } else {
            self.spacing(n.start) * scale
        };
        let sides: &[i128] = if self.both {
            &[-1, 1]
        } else if self.before {
            &[-1]
        } else {
            &[1]
        };
        let group = if self.group {
            (note_key(n) as u32) | 1
        } else {
            n.group
        };
        let count = if NEW { plan.count } else { self.count };
        for &side in sides {
            let roll = if NEW {
                plan.roll.map(|(b, a)| if side < 0 { b } else { a })
            } else {
                None
            };
            let c = roll.map_or(count, |r| r.len() as u64);
            for j in 1..=c {
                let u = j as f64 / c as f64;
                let mut off = match self.accel {
                    Some(a) => sp * c as f64 * warp(u, -3.93 * a / 100.0),
                    None => sp * j as f64,
                };
                if let Some(v) = self.jitter {
                    let mut r = Rng::new(hash(&[self.seed, salt::TICK, key, j, depth as u64]));
                    off += sp * v * (r.unit() * 2.0 - 1.0);
                }
                // The spacing a stroke's length follows.
                let mut local = sp;
                if NEW {
                    if let Some(r) = roll {
                        let k = j as usize - 1;
                        off = r[k] as f64;
                        local = (r[k] - if k > 0 { r[k - 1] } else { 0 }) as f64;
                    } else if let Some((list, unit)) = &self.new.offsets
                        && !list.is_empty()
                    {
                        off = self.list_offset(list, *unit, j, sp, scale, n.start, side, tt);
                        let prev = if j > 1 {
                            self.list_offset(list, *unit, j - 1, sp, scale, n.start, side, tt)
                        } else {
                            0.0
                        };
                        local = (off - prev).abs();
                        if local < 1.0 {
                            local = sp;
                        }
                    }
                }
                let mut s = i128::from(n.start) + side * off.round() as i128;
                if NEW && let Some((g, strength)) = self.new.grid {
                    s = self.grid_snap(s, g, strength);
                }
                let gate = match self.gramp {
                    Some((a, b)) => a + (b - a) * u,
                    None => 0.9,
                };
                let dur = ((local * gate).round() as i128).max(1);
                let vf = match self.vramp {
                    Some((a, b)) => a + (b - a) * u,
                    None => self.vel,
                };
                let mut tpl = *n;
                tpl.group = group;
                if !self.sub_seq.is_empty()
                    && let Some(t) = self.subs.get(usize::from(n.track))
                    && !t.is_empty()
                {
                    let base = t.iter().position(|s| s.0 == n.sub).unwrap_or(0) as i64;
                    let st = self.sub_seq[(j as usize - 1) % self.sub_seq.len()];
                    let (id, ch) = t[(base + st).rem_euclid(t.len() as i64) as usize];
                    tpl.sub = id;
                    tpl.ch = ch;
                }
                let mut pitch = i64::from(n.pitch) + self.step.map_or(0, |st| st * j as i64);
                // The stroke's place in time order (0 = the farthest).
                let i = c - j;
                if NEW {
                    if let Some((_, _, path, lat)) = &self.new.approach {
                        pitch = match path {
                            2 => plan.from,
                            _ => {
                                let k = lat.steps_between(plan.from, i64::from(n.pitch));
                                let at = (k as f64 * i as f64 / c as f64).round() as i64;
                                if at == 0 {
                                    plan.from
                                } else {
                                    lat.step(plan.from, at)
                                }
                            }
                        };
                    }
                    if self.new.tones.is_some() && !plan.tones.is_empty() {
                        let len = plan.tones.len() as i64;
                        pitch = plan.tones[(i as i64 - c as i64).rem_euclid(len) as usize];
                    }
                    if let Some((ch, per_note)) = &self.new.channels {
                        let k = if *per_note { plan.index } else { j - 1 };
                        tpl.ch = ch[(k % ch.len() as u64) as usize];
                    }
                }
                let v = (f64::from(n.vel) * vf / 100.0).round() as i64;
                if depth > 0 {
                    let mut g = tpl;
                    g.start = s.max(0) as u64;
                    g.dur = dur as u64;
                    g.pitch = pitch.clamp(0, i64::from(self.ctx.keys.max_pitch())) as u8;
                    g.vel = v.clamp(1, 127) as u8;
                    if NEW {
                        let inner = FlamPlan {
                            roll: None,
                            ..*plan
                        };
                        self.strokes::<NEW, S>(
                            &g,
                            hash(&[key, j]),
                            out,
                            depth - 1,
                            scale * 0.5,
                            &inner,
                            tt,
                        );
                    } else {
                        self.strokes::<NEW, S>(
                            &g,
                            hash(&[key, j]),
                            out,
                            depth - 1,
                            scale * 0.5,
                            plan,
                            tt,
                        );
                    }
                }
                if NEW && (self.new.cluster.is_some() || self.new.mirror.is_some()) {
                    self.expand(pitch, n.pitch, j, i, c, (s, dur, v), &tpl, out);
                } else {
                    out.stroke(s, dur, pitch, v, &tpl, j);
                }
            }
        }
    }

    /// n05 / n07: stroke `j` (time place `i` of `c`) at `pitch` as a cluster
    /// of lattice neighbours, moved `j × spread` steps up with a twin for
    /// every note mirrored around the source note's pitch `centre`.
    #[allow(clippy::too_many_arguments)]
    fn expand<S: FlamSink>(
        &self,
        pitch: i64,
        centre: u8,
        j: u64,
        i: u64,
        c: u64,
        (s, dur, v): (i128, i128, i64),
        tpl: &TNote,
        out: &mut S,
    ) {
        let centre = i64::from(centre);
        let p0 = match &self.new.mirror {
            Some((spread, Some(lat), _)) => lat.step(pitch, spread * j as i64),
            Some((spread, None, _)) => pitch + spread * j as i64,
            None => pitch,
        };
        let (lo, hi, lat) = match &self.new.cluster {
            Some((size, lat, place, curve)) => {
                let t = if c > 1 {
                    i as f64 / (c - 1) as f64
                } else {
                    0.0
                };
                let k =
                    ((*size as f64 * curve.eval(t).clamp(0.0, 1.0)).round() as i64).clamp(1, *size);
                let lat = lat.unwrap_or_else(|| Lattice::octaves_of(p0));
                match place {
                    0 => (0, k - 1, lat),
                    1 => (-(k - 1), 0, lat),
                    _ => (-((k - 1) / 2), k / 2, lat),
                }
            }
            None => (0, 0, Lattice::chromatic()),
        };
        for k in lo..=hi {
            let m = if k == 0 { p0 } else { lat.step(p0, k) };
            out.stroke(s, dur, m, v, tpl, j);
            if let Some((_, mlat, twin)) = &self.new.mirror {
                let t = match mlat {
                    Some(l) => l.step(centre, -l.steps_between(centre, m)),
                    None => 2 * centre - m,
                };
                if t != m {
                    let tv = (v as f64 * twin).round() as i64;
                    out.stroke(s, dur, t, tv, tpl, j);
                }
            }
        }
    }

    /// The strokes of source note `n` (`with_strokes`: it gets strokes)
    /// and the note itself; `from` / `index` / `tones` the n01 / n04 / n10
    /// context (contextual runs), `bufs` the n08 roll's buffers.
    #[allow(clippy::too_many_arguments)]
    fn map<'c, const NEW: bool, S: FlamSink>(
        &'c self,
        n: &TNote,
        out: &mut S,
        with_strokes: bool,
        key: u64,
        (from, index, tones): (Option<i64>, u64, &[i64]),
        tt: &mut Option<TempoTime<'c>>,
        bufs: &mut (Vec<u64>, Vec<u64>),
    ) {
        if with_strokes {
            if NEW {
                let mut count = self.count;
                let mut spacing = 1.0;
                if let Some((g, mask, counts)) = &self.new.metric {
                    let at = n.start.saturating_add(g / 2);
                    let (slot, _) = metric_position(&self.ctx.meter, at).slot(at, *g);
                    let flam = mask.is_empty() || mask[(slot % mask.len() as u64) as usize] != 0;
                    count = if !flam {
                        0
                    } else if counts.is_empty() {
                        count
                    } else {
                        counts[(slot % counts.len() as u64) as usize].max(0) as u64
                    };
                }
                if let Some((thr, most, at127)) = self.new.accent {
                    if n.vel < thr {
                        count = 0;
                    } else if count > 0 {
                        let t = if thr >= 127 {
                            1.0
                        } else {
                            f64::from(n.vel - thr) / f64::from(127 - thr)
                        };
                        count = (count as f64 + (most as f64 - count as f64) * t)
                            .round()
                            .max(1.0) as u64;
                        spacing = 1.0 + (at127 - 1.0) * t;
                    }
                }
                let mut triad: [i64; 3];
                let tones: &[i64] = match &self.new.tones {
                    Some((2, order, _, scale)) => {
                        let p = i64::from(n.pitch);
                        let (d, _) = scale.degree_of(p);
                        triad = [
                            scale.pitch_of(d),
                            scale.pitch_of(d + 2),
                            scale.pitch_of(d + 4),
                        ];
                        flam_order_tones(&mut triad, *order, p);
                        &triad
                    }
                    _ => tones,
                };
                let from = match &self.new.approach {
                    Some((prev, interval, _, _)) => {
                        let fixed = i64::from(n.pitch) + interval;
                        if *prev { from.unwrap_or(fixed) } else { fixed }
                    }
                    None => 0,
                };
                let roll = match self.new.roll {
                    Some(r) if count > 0 => {
                        let tt = tt.get_or_insert_with(|| TempoTime::new(&self.ctx));
                        let (b, a) = bufs;
                        let sides = (self.both || self.before, self.both || !self.before);
                        if sides.0 {
                            self.roll_into(n, r, false, tt, b);
                        } else {
                            b.clear();
                        }
                        if sides.1 {
                            self.roll_into(n, r, true, tt, a);
                        } else {
                            a.clear();
                        }
                        Some((&b[..], &a[..]))
                    }
                    _ => None,
                };
                let plan = FlamPlan {
                    count,
                    spacing,
                    roll,
                    from,
                    index,
                    tones,
                };
                self.strokes::<true, S>(n, key, out, self.depth, 1.0, &plan, tt);
            } else {
                let plan = FlamPlan {
                    count: self.count,
                    spacing: 1.0,
                    roll: None,
                    from: 0,
                    index: 0,
                    tones: &[],
                };
                self.strokes::<false, S>(n, key, out, self.depth, 1.0, &plan, tt);
            }
        }
        out.note(*n);
    }

    /// A contextual run with the v0.9 functions: the n01 / n04 / n10 context
    /// per note, the v0.8 iteration (per chord with [a9]), then n11.
    fn run_new(
        &self,
        src: &Source,
        out: &mut Output,
        chord_aware: Option<(usize, u64)>,
        lv: Option<&PerLevel<Flam>>,
    ) -> Result<(), TransformError> {
        match lv {
            // Each note's settings under Control Sweeps (E0422).
            Some(l) if self.new.budget.is_some() => {
                self.run_new_at::<true>(src, out, chord_aware, |i| l.at(i))
            }
            Some(l) => self.run_new_at::<false>(src, out, chord_aware, |i| l.at(i)),
            None => self.run_new_at::<false>(src, out, chord_aware, |_| self),
        }
    }

    /// [`Flam::run_new`] with note `i`'s settings `me(i)`.
    /// `SRC`: each stroke's source note is kept (n11's budget per note
    /// under a sweep).
    fn run_new_at<'a, const SRC: bool>(
        &'a self,
        src: &Source,
        out: &mut Output,
        chord_aware: Option<(usize, u64)>,
        me: impl Fn(usize) -> &'a Flam,
    ) -> Result<(), TransformError> {
        let tracks = per_track(src);
        let tables = FlamTables::of(self, src, &tracks);
        let mut tt: Option<TempoTime<'_>> = None;
        let mut bufs = (Vec::new(), Vec::new());
        let mut tone_buf: Vec<i64> = Vec::new();
        let mark = out.mark();
        let base = out.notes.len();
        let mut sink = FlamTracked::<SRC> {
            out,
            base,
            rank: Vec::new(),
            src: Vec::new(),
            cur: 0,
        };
        // The closure borrows the context and the buffers until the walk ends.
        {
            let mut visit = |i: usize, with: bool, key: u64, sink: &mut FlamTracked<'_, SRC>| {
                let tones = tables.tones(i, src, &mut tone_buf);
                let cx = (
                    tables.from.get(i).copied().flatten(),
                    tables.index.get(i).copied().unwrap_or(0),
                    tones,
                );
                if SRC {
                    sink.cur = i as u32;
                }
                me(i).map::<true, _>(&src.notes[i], sink, with, key, cx, &mut tt, &mut bufs);
            };
            match chord_aware {
                None => {
                    for (i, n) in src.notes.iter().enumerate() {
                        visit(i, true, note_key(n), &mut sink);
                        sink.check()?;
                    }
                }
                Some((pick_from, tol)) => {
                    for idx in &tracks {
                        for g in chords(src, idx, tol).iter() {
                            let chord_key =
                                hash(&[src.notes[g[0]].start, u64::from(src.notes[g[0]].track)]);
                            let pick = match pick_from {
                                0 => g.iter().copied().max_by_key(|&i| src.notes[i].pitch),
                                1 => g.iter().copied().min_by_key(|&i| src.notes[i].pitch),
                                _ => None,
                            };
                            for &i in g {
                                visit(i, pick.is_none_or(|p| p == i), chord_key, &mut sink);
                            }
                            sink.check()?;
                        }
                    }
                }
            }
        }
        if let Some(b) = self.new.budget {
            let rank = std::mem::take(&mut sink.rank);
            let srcs = std::mem::take(&mut sink.src);
            let tt = tt.get_or_insert_with(|| TempoTime::new(&self.ctx));
            // Each stroke's budget: its note's (its level's n11 under
            // Control Sweeps).
            let cap = |(nps, w, _): (u64, u64, usize)| {
                ((nps as f64 * w as f64 / 1e6).floor() as usize).max(1)
            };
            if SRC {
                let cap_of = |k: usize| {
                    cap(srcs
                        .get(k)
                        .and_then(|&i| me(i as usize).new.budget)
                        .unwrap_or(b))
                };
                flam_budget(sink.out, mark, base, &rank, tt, b, Some(&cap_of));
            } else {
                flam_budget(sink.out, mark, base, &rank, tt, b, None);
            }
        }
        Ok(())
    }
}

/// The per-note context of a contextual Flam run (only the tables the
/// switched-on functions read; empty otherwise).
struct FlamTables {
    /// n01: the nearest pitch of the track's previous onset.
    from: Vec<Option<i64>>,
    /// n04 per note: the note's place in its track.
    index: Vec<u64>,
    /// n10 (this / previous chord): chords' tones, ascending, de-duplicated
    /// (`spans` into `table`), the chord each note reads and the order.
    table: Vec<i64>,
    spans: Vec<(u32, u32)>,
    chord_of: Vec<u32>,
    order: Option<usize>,
}

impl FlamTables {
    fn of(f: &Flam, src: &Source, tracks: &[Vec<usize>]) -> FlamTables {
        let n = src.notes.len();
        let mut t = FlamTables {
            from: Vec::new(),
            index: Vec::new(),
            table: Vec::new(),
            spans: Vec::new(),
            chord_of: Vec::new(),
            order: None,
        };
        if f.new.approach.as_ref().is_some_and(|a| a.0) {
            t.from = vec![None; n];
            for idx in tracks {
                let g = chords(src, idx, 0);
                for k in 1..g.len() {
                    let prev = g.get(k - 1);
                    for &i in g.get(k) {
                        let p = i64::from(src.notes[i].pitch);
                        t.from[i] = prev
                            .iter()
                            .map(|&q| i64::from(src.notes[q].pitch))
                            .min_by_key(|&q| ((q - p).abs(), q));
                    }
                }
            }
        }
        if f.new.channels.as_ref().is_some_and(|c| c.1) {
            t.index = vec![0; n];
            for idx in tracks {
                for (k, &i) in idx.iter().enumerate() {
                    t.index[i] = k as u64;
                }
            }
        }
        if let Some((source, order, tol, _)) = &f.new.tones
            && *source < 2
        {
            t.chord_of = vec![0; n];
            t.order = Some(*order);
            for idx in tracks {
                let g = chords(src, idx, *tol);
                for k in 0..g.len() {
                    let start = t.table.len();
                    t.table
                        .extend(g.get(k).iter().map(|&i| i64::from(src.notes[i].pitch)));
                    t.table[start..].sort_unstable();
                    let mut w = start;
                    for r in start..t.table.len() {
                        if r == start || t.table[r] != t.table[w - 1] {
                            t.table[w] = t.table[r];
                            w += 1;
                        }
                    }
                    t.table.truncate(w);
                    t.spans.push((start as u32, (w - start) as u32));
                    let me = t.spans.len() as u32 - 1;
                    let read = if *source == 1 && k > 0 { me - 1 } else { me };
                    for &i in g.get(k) {
                        t.chord_of[i] = read;
                    }
                }
            }
        }
        t
    }

    /// n10: note `i`'s grace tones in their order (in `buf`), or none.
    fn tones<'b>(&self, i: usize, src: &Source, buf: &'b mut Vec<i64>) -> &'b [i64] {
        let Some(order) = self.order else {
            return &[];
        };
        let (a, len) = self.spans[self.chord_of[i] as usize];
        buf.clear();
        buf.extend_from_slice(&self.table[a as usize..(a + len) as usize]);
        flam_order_tones(buf, order, i64::from(src.notes[i].pitch));
        buf
    }
}

/// n10: orders `tones` (ascending) up, down or with the tone nearest
/// `pitch` last (farthest first; ties low first).
fn flam_order_tones(tones: &mut [i64], order: usize, pitch: i64) {
    match order {
        0 => tones.sort_unstable(),
        1 => tones.sort_unstable_by(|a, b| b.cmp(a)),
        _ => tones.sort_unstable_by_key(|&t| (std::cmp::Reverse((t - pitch).abs()), t)),
    }
}

/// n11: removes strokes emitted since `mark` (`base` in `out.notes`; `rank`
/// = each note's stroke index, 0 = a source note) until every trailing
/// window of `window_us` (real time, the canonical NPS window) holds at most
/// `nps` notes per second — the source notes always stay — dropping first
/// the strokes in the busiest windows (farther strokes first), the quietest
/// or the earliest, each only while a window holding it is over the budget
/// (`density_window::budget_keep`).
fn flam_budget(
    out: &mut Output,
    mark: super::super::output::Mark,
    base: usize,
    rank: &[u32],
    tt: &mut TempoTime<'_>,
    (nps, window_us, how): (u64, u64, usize),
    cap_of: Option<&dyn Fn(usize) -> usize>,
) {
    let n = rank.len().min(out.notes.len() - base);
    if n == 0 {
        return;
    }
    let notes = &out.notes[base..base + n];
    // Time order (packed keys, no indirection while sorting).
    let mut by_start: Vec<(u64, u32)> = notes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.start, i as u32))
        .collect();
    by_start.sort_unstable();
    let order: Vec<usize> = by_start.iter().map(|x| x.1 as usize).collect();
    drop(by_start);
    let times: Vec<u64> = order
        .iter()
        .map(|&i| tt.micros_at(notes[i].start))
        .collect();
    // Priority keys packed high to low: (busiest window | velocity), the
    // stroke farther out first, then time order (alone for the earliest).
    let mut keyed: Vec<(u64, u32)> = Vec::new();
    let loads = (how == 0).then(|| holding_loads(&times, window_us));
    for (q, &i) in order.iter().enumerate() {
        let r = rank[i];
        if r == 0 {
            continue;
        }
        let first = match &loads {
            Some(l) => u64::from(u32::MAX - l[q]),
            None if how == 1 => u64::from(notes[i].vel),
            None => 0,
        };
        // The earliest first: time order alone.
        let key = if how >= 2 {
            0
        } else {
            (first << 32) | u64::from(u32::MAX - r)
        };
        keyed.push((key, q as u32));
    }
    keyed.sort_unstable();
    let removable: Vec<usize> = keyed.iter().map(|x| x.1 as usize).collect();
    drop(keyed);
    let keep_sorted = match cap_of {
        // Each stroke's own budget (E0422).
        Some(cap_of) => budget_keep_if_by(
            &times,
            window_us,
            |q| cap_of(order[q]),
            &removable,
            |_| true,
        ),
        None => {
            let cap = ((nps as f64 * window_us as f64 / 1e6).floor() as usize).max(1);
            budget_keep(&times, window_us, cap, &removable)
        }
    };
    let mut keep = vec![true; n];
    for (q, &i) in order.iter().enumerate() {
        keep[i] = keep_sorted[q];
    }
    out.adjust_since(mark, |i, _, _, _| i >= n || keep[i]);
}

/// Flam's settings from `p`.
fn flam_of(p: &Params, b: &BuildCtx) -> Flam {
    let (mut time, mut count) = (p.float("time"), 1u64);
    let mut absolute = p.flag("absolute");
    match p.choice("preset") {
        1 => (time, absolute) = (15.0, true),
        2 => (time, absolute) = (45.0, true),
        3 => (time, count, absolute) = (25.0, 2, true),
        4 => (time, count, absolute) = (20.0, 3, true),
        _ => {}
    }
    if p.adv(1) {
        count = p.int("a1.count") as u64;
    }
    let lattice = |on: bool, key: &str, scale: &str| {
        on.then(|| Lattice::of_scale(&Scale::new(p.choice(key), p.choice(scale))))
    };
    let new = FlamNew {
        approach: p.flag("n01").then(|| {
            let path = p.choice("n01.path");
            (
                p.choice("n01.from") == 0,
                p.int("n01.interval"),
                path,
                lattice(path == 1, "n01.key", "n01.scale").unwrap_or_else(Lattice::chromatic),
            )
        }),
        metric: p.flag("n02").then(|| {
            let grid = p.choice("n02.grid").min(art_grid_values().len() - 1);
            (
                note_value(grid, 1, b.ctx),
                p.list("n02.mask"),
                p.list("n02.strokes"),
            )
        }),
        accent: p.flag("n03").then(|| {
            (
                p.int("n03.threshold").clamp(1, 127) as u8,
                p.int("n03.strokes").max(1) as u64,
                p.float("n03.spacing") / 100.0,
            )
        }),
        channels: p
            .flag("n04")
            .then(|| {
                let ch: Vec<u8> = p
                    .list("n04.channels")
                    .iter()
                    .map(|&c| (c.clamp(1, 16) - 1) as u8)
                    .collect();
                (ch, p.choice("n04.per") == 1)
            })
            .filter(|c| !c.0.is_empty()),
        cluster: p.flag("n05").then(|| {
            let kind = p.choice("n05.kind");
            (
                p.int("n05.size").max(1),
                match kind {
                    0 => Some(Lattice::chromatic()),
                    1 => lattice(true, "n05.key", "n05.scale"),
                    _ => None,
                },
                p.choice("n05.place"),
                p.curve("n05.curve"),
            )
        }),
        offsets: p
            .flag("n06")
            .then(|| (p.list("n06.offsets"), p.choice("n06.unit")))
            .filter(|o| !o.0.is_empty()),
        mirror: p.flag("n07").then(|| {
            (
                p.int("n07.spread").max(1),
                lattice(p.choice("n07.in") == 1, "n07.key", "n07.scale"),
                p.float("n07.vel") / 100.0,
            )
        }),
        roll: p.flag("n08").then(|| {
            (
                (p.float("n08.lead") * 1000.0).round() as u64,
                p.float("n08.from"),
                p.float("n08.to"),
                p.int("n08.most").max(1) as u64,
            )
        }),
        grid: p.flag("n09").then(|| {
            let grid = p.choice("n09.grid").min(art_grid_values().len() - 1);
            (note_value(grid, 1, b.ctx), p.float("n09.strength") / 100.0)
        }),
        tones: p.flag("n10").then(|| {
            (
                p.choice("n10.source"),
                p.choice("n10.order"),
                p.ticks("n10.tol"),
                Scale::new(p.choice("n10.key"), p.choice("n10.scale")),
            )
        }),
        budget: p.flag("n11").then(|| {
            (
                p.int("n11.nps").max(1) as u64,
                (p.float("n11.window") * 1000.0).round() as u64,
                p.choice("n11.drop"),
            )
        }),
    };
    Flam {
        spacing_ms: absolute.then_some(time),
        spacing_ticks: time,
        before: p.flag("before"),
        vel: p.float("vel"),
        group: p.flag("group"),
        seed: p.seed("seed"),
        count,
        accel: p.adv(2).then(|| p.float("a2.dist")),
        vramp: p.adv(3).then(|| (p.float("a3.from"), p.float("a3.to"))),
        gramp: p
            .adv(4)
            .then(|| (p.float("a4.from") / 100.0, p.float("a4.to") / 100.0)),
        step: p.adv(5).then(|| p.int("a5.step")),
        sub_seq: if p.adv(6) {
            p.list("a6.seq")
        } else {
            Vec::new()
        },
        jitter: p.adv(7).then(|| p.float("a7.amount") / 100.0),
        both: p.adv(8),
        depth: if p.adv(10) {
            p.int("a10.depth") as u32
        } else {
            0
        },
        ctx: b.ctx.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        new,
    }
}

/// Flam's output per note and its stroke reach in ticks (the preflight
/// and the lazy preview's window).
fn flam_bounds(f: &Flam, b: &BuildCtx) -> (u64, u64) {
    let (time, count, any_new) = (f.spacing_ticks, f.count, f.new.any());
    let side = if f.both { 2 } else { 1 };
    let per = if any_new {
        // Strokes per side at the top level (n02 / n03 counts, the n08
        // roll) and in recursive layers; n05 / n07 notes per stroke.
        let n02_most = f
            .new
            .metric
            .as_ref()
            .map_or(0, |m| m.2.iter().copied().max().unwrap_or(0).max(0) as u64);
        let n03_most = f.new.accent.map_or(0, |a| a.1);
        let inner = count.max(n02_most).max(n03_most);
        // n08: a roll has ⌊(start + end rate) / 2 × lead-in⌋ strokes (its
        // phase at the note), at most `most`.
        let roll = f.new.roll.map_or(0, |(lead, r0, r1, most)| {
            let phase = (r0 + r1) / 2.0 * lead as f64 / 1e6;
            most.min(phase.floor().max(0.0) as u64 + 1)
        });
        let top = inner.max(roll);
        let mult = f.new.cluster.as_ref().map_or(1, |c| c.0 as u64)
            * if f.new.mirror.is_some() { 2 } else { 1 };
        let mut x = 0u64;
        let mut level = side * top;
        for _ in 0..=f.depth {
            x = x.saturating_add(level);
            level = level.saturating_mul(side * inner);
        }
        1u64.saturating_add(x.saturating_mul(mult))
    } else {
        let mut x = 1u64;
        let mut level = side * count;
        for _ in 0..=f.depth {
            x = x.saturating_add(level);
            level = level.saturating_mul(side * count);
        }
        x
    };
    // Reach of a stroke in ticks (tick-based time only; millisecond time
    // depends on the tempo at each note, so it is evaluated over the source).
    let reach = if any_new {
        let n02_most = f
            .new
            .metric
            .as_ref()
            .map_or(0, |m| m.2.iter().copied().max().unwrap_or(0).max(0) as u64);
        let most = count.max(n02_most).max(f.new.accent.map_or(0, |a| a.1)) as f64;
        let sp = time * f.new.accent.map_or(1.0, |a| a.2.max(1.0));
        let spaced = sp * (most * 4.0 + 4.0);
        let listed = f.new.offsets.as_ref().map_or(0.0, |(l, unit)| {
            let top = l.iter().copied().max().unwrap_or(0) as f64 * 2.0 * (most + 1.0);
            match unit {
                0 => top,
                // Milliseconds: the most ticks that real time spans at the
                // song's fastest tempo.
                1 => TempoTime::new(b.ctx).max_ticks_in(top * 1000.0) as f64,
                _ => top / 100.0 * sp,
            }
        });
        let grid = f.new.grid.map_or(0, |g| g.0);
        (spaced.max(listed).ceil() as u64).saturating_add(grid)
    } else {
        (time as u64).saturating_mul(count * 4 + 4)
    };
    (per, reach)
}

fn flam_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let f = flam_of(p, b);
    let absolute = f.spacing_ms.is_some();
    let any_new = f.new.any();
    let contextual_new = f.new.contextual();
    let (mut per, mut reach) = flam_bounds(&f, b);
    // With Control Sweeps (E0422; every Flam target is a function's
    // control) the whole-source run, each note with its level's settings;
    // the bounds the largest over the swept ranges' corners.
    let swept = Rebuild::of(p, b);
    if let Some(rb) = &swept {
        for c in rb.corners(b.ctx, flam_of) {
            let (pc, rc) = flam_bounds(&c, b);
            per = per.max(pc);
            reach = reach.max(rc);
        }
    }
    let sctx = b.ctx.clone();
    if p.adv(9) || absolute || contextual_new || swept.is_some() {
        let chord_aware = p.adv(9);
        let from = p.choice("a9.from");
        let tol = p.ticks("a9.tol");
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                let lv: Option<PerLevel<Flam>> =
                    swept.as_ref().map(|r| r.levels(&sctx, src, flam_of));
                if any_new {
                    return f.run_new(src, out, chord_aware.then_some((from, tol)), lv.as_ref());
                }
                match &lv {
                    Some(l) => flam_v08_run(src, out, chord_aware, from, tol, |i| l.at(i)),
                    None => flam_v08_run(src, out, chord_aware, from, tol, |_| &f),
                }
            },
            growth: Growth::Expand(per),
            estimate: None,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let mut tt: Option<TempoTime<'_>> = None;
            let mut bufs = (Vec::new(), Vec::new());
            let none = (None, 0, &[][..]);
            if any_new {
                f.map::<true, _>(n, out, true, note_key(n), none, &mut tt, &mut bufs);
            } else {
                f.map::<false, _>(n, out, true, note_key(n), none, &mut tt, &mut bufs);
            }
        },
        ahead: reach,
        behind: reach,
        growth: Growth::Expand(per),
    })
}

/// Flam's contextual v0.8 run (every v0.9 function off: [a9]'s chords or
/// absolute spacing) with note `i`'s settings `me(i)` (its level's under
/// Control Sweeps, E0422).
fn flam_v08_run<'a>(
    src: &Source,
    out: &mut Output,
    chord_aware: bool,
    from: usize,
    tol: u64,
    me: impl Fn(usize) -> &'a Flam,
) -> Result<(), TransformError> {
    let mut tt: Option<TempoTime<'_>> = None;
    let mut bufs = (Vec::new(), Vec::new());
    let none = (None, 0, &[][..]);
    if !chord_aware {
        for (i, n) in src.notes.iter().enumerate() {
            me(i).map::<false, _>(n, out, true, note_key(n), none, &mut tt, &mut bufs);
            out.check()?;
        }
        return Ok(());
    }
    for idx in per_track(src) {
        for g in chords(src, &idx, tol).iter() {
            let chord_key = hash(&[src.notes[g[0]].start, u64::from(src.notes[g[0]].track)]);
            let pick = match from {
                0 => g.iter().copied().max_by_key(|&i| src.notes[i].pitch),
                1 => g.iter().copied().min_by_key(|&i| src.notes[i].pitch),
                _ => None,
            };
            for &i in g {
                let with = pick.is_none_or(|p| p == i);
                me(i).map::<false, _>(
                    &src.notes[i],
                    out,
                    with,
                    chord_key,
                    none,
                    &mut tt,
                    &mut bufs,
                );
            }
            out.check()?;
        }
    }
    Ok(())
}

// ================================================================ Limit

pub const LIMIT: ToolDef = ToolDef {
    id: "limit",
    spec: limit_spec,
    build: limit_build,
};

const SNAP_MODES: &[&str] = &["Above", "Under", "Alternate", "Nearest"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const LIMIT_HELP: &[(&str, &str)] = &[
    ("key", "The root of the scale the notes are kept in."),
    (
        "scale",
        "The scale the notes are kept in; notes outside it are snapped.",
    ),
    (
        "snap",
        "Where an out-of-scale note goes: the scale note above, the one under, alternately above and under, or the nearest.",
    ),
    (
        "lo",
        "The lowest key a note may keep; lower notes move up by octaves.",
    ),
    (
        "hi",
        "The highest key a note may keep; higher notes move down by octaves.",
    ),
    (
        "wrap",
        "Notes above the range wrap to its lowest octave and notes below to its highest, instead of moving the fewest octaves.",
    ),
    (
        "a1",
        "Keeps the notes in your own set of pitch classes instead of the scale.",
    ),
    ("a1.set", "The allowed pitch classes, 0 = C … 11 = B."),
    ("a2", "Allows only some degrees of the scale."),
    ("a2.from", "The lowest scale degree allowed (1 = the root)."),
    ("a2.to", "The highest scale degree allowed."),
    ("a3", "Gives each subchannel its own key range."),
    (
        "a3.ranges",
        "Low and high keys in pairs, one pair per subchannel in turn (the first pair = the track's first subchannel); the list repeats.",
    ),
    (
        "a4",
        "Moves out-of-range notes toward the middle of the range, by octaves.",
    ),
    (
        "a5",
        "Reflects out-of-range notes back from the range's edge, like a mirror.",
    ),
    (
        "a6",
        "Moves out-of-range notes to the octave nearest the previous note of their line, for smooth voice leading.",
    ),
    (
        "a7",
        "Spreads crowded octaves out: octaves holding too many notes give some to the least crowded octave in range.",
    ),
    (
        "a7.limit",
        "The most notes an octave may hold, as a multiple of the average per octave.",
    ),
    (
        "a8",
        "Moves the range across the selection: from Range Low / High at the start to the values below at the end.",
    ),
    ("a8.lo", "The range's lowest key at the selection's end."),
    ("a8.hi", "The range's highest key at the selection's end."),
    (
        "a9",
        "Removes some pitch classes from the allowed set; notes on them are snapped.",
    ),
    ("a9.exclude", "The excluded pitch classes, 0 = C … 11 = B."),
    (
        "a10",
        "Follows the sounding harmony: only the pitch classes most heard around each note are allowed.",
    ),
    (
        "a10.window",
        "The time, in ticks, around each note whose notes decide the harmony.",
    ),
    (
        "a10.top",
        "How many of the most heard pitch classes are allowed.",
    ),
    ("n01", "Keeps velocities within a range."),
    ("n01.lo", "The lowest velocity allowed."),
    ("n01.hi", "The highest velocity allowed."),
    (
        "n01.mode",
        "What happens to velocities outside: clamped to the edge, compressed toward it by the ratio, or folded back inside.",
    ),
    (
        "n01.ratio",
        "For Compress: how strongly velocities beyond the edge are squeezed (4 : 1 keeps a quarter of the overshoot).",
    ),
    ("n02", "Keeps note lengths within limits."),
    ("n02.shortest", "The shortest length allowed, in ticks."),
    ("n02.longest", "The longest length allowed, in ticks."),
    (
        "n02.short",
        "Notes that are too short are lengthened to the shortest length, or removed.",
    ),
    (
        "n02.long",
        "Notes that are too long are cut to the longest length, or split into repeats of it.",
    ),
    (
        "n03",
        "Removes notes so no more than a set number sound at once.",
    ),
    (
        "n03.most",
        "The most notes that may sound at the same moment.",
    ),
    (
        "n03.first",
        "Which notes are removed first: the quietest, inner voices, the shortest, or the newest.",
    ),
    (
        "n03.scope",
        "Count the sounding notes per track, or over all selected notes together.",
    ),
    (
        "n04",
        "Removes notes so no more than a set number start at once or within a time window.",
    ),
    (
        "n04.most",
        "The most notes that may start per tick or per window.",
    ),
    (
        "n04.per",
        "Count starts at each tick, or within the sliding window below.",
    ),
    (
        "n04.window",
        "For Window: the window's length, in milliseconds.",
    ),
    (
        "n04.first",
        "Which notes are removed first: the quietest, the shortest, the highest, or the lowest.",
    ),
    (
        "n05",
        "Limits how far a line may jump from one note to the next.",
    ),
    ("n05.leap", "The largest jump allowed, in semitones."),
    (
        "n05.fix",
        "What a larger jump becomes: moved by octaves toward the previous note, folded back, or removed.",
    ),
    (
        "n05.line",
        "What one line is: a track, a subchannel, or a MIDI channel.",
    ),
    (
        "n06",
        "Limits how wide a chord may spread from its lowest note to its highest.",
    ),
    (
        "n06.span",
        "The widest chord allowed, in semitones; outer notes beyond it move inward by octaves.",
    ),
    (
        "n06.keep",
        "Which part of the chord stays in place: the bass, the top, or the center.",
    ),
    (
        "n06.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n07",
        "Keeps repeated notes of the same key at least a minimum distance apart.",
    ),
    (
        "n07.dist",
        "The shortest distance allowed between two starts of the same key, in ticks.",
    ),
    (
        "n07.action",
        "Notes closer than that are removed, or merged into the earlier note.",
    ),
    (
        "n07.same",
        "What counts as the same key: the key, the key on the same MIDI channel, or the key on the same subchannel.",
    ),
    (
        "n08",
        "Keeps notes inside a time window, wrapping or folding the ones outside it back in.",
    ),
    ("n08.start", "The tick where the window starts."),
    ("n08.len", "How long the window is, in ticks."),
    (
        "n08.outside",
        "What happens to notes outside the window: wrapped around, folded back, clamped to its edge, or removed.",
    ),
    (
        "n09",
        "Allows only some MIDI channels or subchannels; notes on others move to an allowed one.",
    ),
    (
        "n09.target",
        "Whether the lists below limit MIDI channels or subchannels.",
    ),
    ("n09.channels", "The MIDI channels allowed (1 – 16)."),
    (
        "n09.subs",
        "The subchannels allowed, by number (1 = the first).",
    ),
    (
        "n09.remap",
        "Where a note on a forbidden one goes: the nearest allowed, the next allowed in turn, or the first allowed.",
    ),
    (
        "n10",
        "Compresses the pitch range toward a center key, like a dynamics compressor for pitch.",
    ),
    ("n10.centre", "The key the range is compressed toward."),
    (
        "n10.knee",
        "Notes within this many semitones of the center are left alone.",
    ),
    (
        "n10.ratio",
        "How strongly the distance beyond the knee is squeezed (2 : 1 halves it).",
    ),
    (
        "n10.snap",
        "Where the compressed pitches land: on any semitone, or on the key and scale.",
    ),
    (
        "n11",
        "Limits how many octave copies of a pitch class a chord may hold.",
    ),
    (
        "n11.most",
        "The most notes of one pitch class a chord may keep.",
    ),
    (
        "n11.keep",
        "Which copies stay: the lowest, the highest, or the outer ones.",
    ),
    (
        "n11.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
];

fn limit_spec() -> ToolSpec {
    let mut p = vec![
        choice("key", "Key (root)", &KEY_NAMES, 0),
        choice("scale", "Scale", scale_names(), 0),
        choice("snap", "Snap", SNAP_MODES, 0),
        pitch("lo", "Range low", 48),
        pitch("hi", "Range high", 72),
        flag("wrap", "Wrap to top / bottom", false),
        adv_switch(1, "Arbitrary allowed-pitch set"),
        list(
            "a1.set",
            "Allowed pitch classes (0 = C … 11 = B)",
            0,
            11,
            12,
            &[0, 3, 5, 7, 10],
        )
        .adv(1),
        adv_switch(2, "Scale-degree range"),
        int("a2.from", "Lowest degree (1-based)", 1, 12, 1).adv(2),
        int("a2.to", "Highest degree", 1, 12, 5).adv(2),
        adv_switch(3, "Per-subchannel ranges"),
        list(
            "a3.ranges",
            "(low, high) pairs by subchannel",
            0,
            127,
            8192,
            &[48, 72, 60, 84],
        )
        .keyed()
        .adv(3),
        adv_switch(4, "Fold toward the center"),
        adv_switch(5, "Mirror at the boundaries"),
        adv_switch(6, "Nearest voice-leading correction"),
        adv_switch(7, "Register-density balancing"),
        float(
            "a7.limit",
            "Most notes per octave (× average)",
            1.0,
            4.0,
            1.5,
            "",
        )
        .adv(7),
        adv_switch(8, "Dynamic range envelope"),
        pitch("a8.lo", "Low at the end", 60).adv(8),
        pitch("a8.hi", "High at the end", 96).adv(8),
        adv_switch(9, "Pitch-class exclusion mask"),
        list("a9.exclude", "Excluded pitch classes", 0, 11, 12, &[6]).adv(9),
        adv_switch(10, "Harmonic window (follow the sounding chord)"),
        ticks("a10.window", "Window", 1, 1 << 40, 1920).adv(10),
        int("a10.top", "Pitch classes kept", 1, 12, 4).adv(10),
        // v0.9 (V09-S35): functions declared by id.
        flag("n01", "Velocity range", false),
        int("n01.lo", "Lowest velocity", 1, 127, 20),
        int("n01.hi", "Highest velocity", 1, 127, 110),
        choice("n01.mode", "Velocities outside", LIMIT_VEL_MODES, 0),
        float("n01.ratio", "Compression ratio", 1.0, 20.0, 4.0, ": 1"),
        flag("n02", "Length limits", false),
        ticks("n02.shortest", "Shortest length", 1, 1 << 40, 60),
        ticks("n02.longest", "Longest length", 1, 1 << 40, 1920),
        choice("n02.short", "Too short", LIMIT_SHORT, 0),
        choice("n02.long", "Too long", LIMIT_LONG, 0),
        flag("n03", "Polyphony cap", false),
        int("n03.most", "Most sounding notes", 1, 1_000_000, 8),
        choice("n03.first", "Remove first", LIMIT_POLY_FIRST, 0),
        choice("n03.scope", "Polyphony", LIMIT_SCOPE, 0),
        flag("n04", "Onset rate limit", false),
        int("n04.most", "Most onsets", 1, 10_000_000, 1000),
        choice("n04.per", "Per", LIMIT_PER, 1),
        float("n04.window", "Window", 1.0, 5000.0, 1000.0, "ms"),
        choice("n04.first", "Remove first", LIMIT_ONSET_FIRST, 0),
        flag("n05", "Leap limit", false),
        int("n05.leap", "Largest leap", 1, 48, 12),
        choice("n05.fix", "Larger leaps", LIMIT_LEAP_FIX, 0),
        choice("n05.line", "Line per", LIMIT_LINE, 0),
        flag("n06", "Chord span limit", false),
        int("n06.span", "Largest span", 1, 127, 24),
        choice("n06.keep", "Keep in place", LIMIT_SPAN_KEEP, 0),
        ticks("n06.tol", "Chord tolerance", 0, 1 << 30, 30),
        flag("n07", "Retrigger spacing", false),
        ticks("n07.dist", "Shortest distance", 1, 1 << 40, 30),
        choice("n07.action", "Closer notes", LIMIT_RETRIGGER, 0),
        choice("n07.same", "Same key means", LIMIT_SAME_KEY, 0),
        flag("n08", "Time window wrap", false),
        ticks("n08.start", "Window start", 0, 1 << 40, 0),
        ticks("n08.len", "Window length", 1, 1 << 40, 7680),
        choice("n08.outside", "Notes outside", LIMIT_OUTSIDE, 0),
        flag("n09", "Allowed channels or subchannels", false),
        choice("n09.target", "Allowed", LIMIT_TARGET, 1),
        list("n09.channels", "Allowed MIDI channels", 1, 16, 16, &[1, 2]),
        list("n09.subs", "Allowed subchannels", 0, 4095, 64, &[1, 2]),
        choice("n09.remap", "Others move to", LIMIT_REMAP, 0),
        flag("n10", "Range compression", false),
        pitch("n10.centre", "Center", 60),
        int("n10.knee", "Knee", 0, 64, 6),
        float("n10.ratio", "Ratio", 1.0, 20.0, 2.0, ": 1"),
        choice("n10.snap", "Snap", LIMIT_SNAP, 0),
        flag("n11", "Octave-doubling limit", false),
        int("n11.most", "Most copies per pitch class", 1, 11, 1),
        choice("n11.keep", "Keep", LIMIT_KEEP, 0),
        ticks("n11.tol", "Chord tolerance", 0, 1 << 30, 30),
    ];
    p.extend(boundary::specs(PitchPolicy::Clamp));
    with_help(&mut p, LIMIT_HELP);
    ToolSpec {
        id: "limit",
        name: "Limit",
        family: Family::Fl,
        summary: "Keeps notes in a key / scale and a key range: out-of-scale notes snap above or under, out-of-range notes move by octaves (or wrap).",
        params: p,
        advanced: vec![
            "Arbitrary allowed-pitch set",
            "Scale-degree range",
            "Per-subchannel ranges",
            "Fold toward the center",
            "Mirror at the boundaries",
            "Nearest voice-leading correction",
            "Register-density balancing",
            "Dynamic range envelope",
            "Pitch-class exclusion mask",
            "Harmonic window (follow the sounding chord)",
        ],
        functions: [
            ("n01", "Velocity range", 110),
            ("n02", "Length limits", 120),
            ("n03", "Polyphony cap", 710),
            ("n04", "Onset rate limit", 720),
            ("n05", "Leap limit", 150),
            ("n06", "Chord span limit", 160),
            ("n07", "Retrigger spacing", 700),
            ("n08", "Time window wrap", 180),
            ("n09", "Allowed channels or subchannels", 190),
            ("n10", "Range compression", 200),
            ("n11", "Octave-doubling limit", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

const LIMIT_VEL_MODES: &[&str] = &["Clamp", "Compress", "Fold"];
const LIMIT_SHORT: &[&str] = &["Lengthen", "Remove"];
const LIMIT_LONG: &[&str] = &["Cut", "Split into repeats"];
const LIMIT_POLY_FIRST: &[&str] = &["Quietest", "Inner voices", "Shortest", "Newest"];
const LIMIT_SCOPE: &[&str] = &["Per track", "Over all selected"];
const LIMIT_PER: &[&str] = &["Tick", "Window"];
const LIMIT_ONSET_FIRST: &[&str] = &["Quietest", "Shortest", "Highest", "Lowest"];
const LIMIT_LEAP_FIX: &[&str] = &["Octaves toward it", "Fold", "Remove"];
const LIMIT_LINE: &[&str] = &["Track", "Subchannel", "MIDI channel"];
const LIMIT_SPAN_KEEP: &[&str] = &["Bass", "Top", "Center"];
const LIMIT_RETRIGGER: &[&str] = &["Remove the later", "Merge into the earlier"];
const LIMIT_SAME_KEY: &[&str] = &["Key", "Key and MIDI channel", "Key and subchannel"];
const LIMIT_OUTSIDE: &[&str] = &["Wrap", "Fold", "Clamp", "Remove"];
const LIMIT_TARGET: &[&str] = &["MIDI channels", "Subchannels"];
const LIMIT_REMAP: &[&str] = &["Nearest", "Next in turn", "First allowed"];
const LIMIT_SNAP: &[&str] = &["Chromatic", "Key and scale"];
const LIMIT_KEEP: &[&str] = &["Lowest", "Highest", "Outer"];

#[derive(Clone)]
struct Limit {
    mask: u16,
    root: u8,
    snap: usize,
    lo: i64,
    hi: i64,
    wrap: bool,
    degrees: Option<(usize, usize)>,
    sub_ranges: Vec<(i64, i64)>,
    fold_centre: bool,
    mirror: bool,
    envelope: Option<(i64, i64)>,
    stats: super::super::SourceStats,
    subs: Vec<Vec<(u32, u8)>>,
}

impl Limit {
    fn range_for(&self, n: &TNote) -> (i64, i64) {
        let (mut lo, mut hi) = (self.lo, self.hi);
        if let Some((l1, h1)) = self.envelope {
            let t = pos(n, &self.stats);
            lo = (lo as f64 + (l1 - lo) as f64 * t).round() as i64;
            hi = (hi as f64 + (h1 - hi) as f64 * t).round() as i64;
        }
        if !self.sub_ranges.is_empty() {
            let si = self
                .subs
                .get(usize::from(n.track))
                .and_then(|t| t.iter().position(|s| s.0 == n.sub))
                .unwrap_or(0);
            let r = self.sub_ranges[si % self.sub_ranges.len()];
            lo = r.0;
            hi = r.1;
        }
        (lo.min(hi), lo.max(hi))
    }

    /// Snap to the allowed set (by mode), then fit into the range.
    fn pitch(&self, n: &TNote, mask: u16, prev: Option<i64>) -> i64 {
        let ok = |p: i64| mask & (1 << p.rem_euclid(12)) != 0;
        let mut p = i64::from(n.pitch);
        if mask != 0 && !ok(p) {
            let mut up = p;
            while !ok(up) {
                up += 1;
            }
            let mut down = p;
            while !ok(down) {
                down -= 1;
            }
            p = match self.snap {
                0 => up,
                1 => down,
                2 => {
                    if n.start.wrapping_add(u64::from(n.pitch)).is_multiple_of(2) {
                        up
                    } else {
                        down
                    }
                }
                _ => {
                    if up - p <= p - down {
                        up
                    } else {
                        down
                    }
                }
            };
        }
        if let Some((a, b)) = self.degrees {
            // Degrees outside [a, b] (within the octave) move to the nearest
            // allowed degree.
            let steps: Vec<i64> = (0..12)
                .filter(|i| mask & (1 << ((i + i64::from(self.root)) % 12)) != 0)
                .collect();
            if !steps.is_empty() {
                let rel = (p - i64::from(self.root)).rem_euclid(12);
                let oct_base = p - rel;
                let d = steps.iter().position(|&s| s == rel).unwrap_or(0);
                let (a, b) = (
                    a.min(steps.len() - 1),
                    b.min(steps.len() - 1).max(a.min(steps.len() - 1)),
                );
                if d < a {
                    p = oct_base + steps[a];
                } else if d > b {
                    p = oct_base + steps[b];
                }
            }
        }
        let (lo, hi) = self.range_for(n);
        if p < lo || p > hi {
            if self.mirror {
                let span = (hi - lo).max(1);
                let period = 2 * span;
                let m = (p - lo).rem_euclid(period);
                p = lo + if m <= span { m } else { period - m };
                if mask != 0 && !ok(p) {
                    p = if p > lo + span / 2 { p - 1 } else { p + 1 };
                }
            } else if let Some(prev) = prev {
                // Octave of this pitch class nearest to the previous note, in range.
                let pc = p.rem_euclid(12);
                let mut best = p;
                let mut best_d = i64::MAX;
                let mut c = lo - (lo - pc).rem_euclid(12);
                while c <= hi + 12 {
                    if c >= lo && c <= hi && (c - prev).abs() < best_d {
                        best = c;
                        best_d = (c - prev).abs();
                    }
                    c += 12;
                }
                p = best;
            } else if self.fold_centre {
                let centre = (lo + hi) / 2;
                let k = ((centre - p) as f64 / 12.0).round() as i64;
                p += 12 * k;
            } else if self.wrap {
                // Above → the lowest octave of the limit; below → the highest.
                let pc = p.rem_euclid(12);
                if p > hi {
                    p = lo + (pc - lo).rem_euclid(12);
                } else {
                    p = hi - (hi - pc).rem_euclid(12);
                }
            } else {
                while p > hi {
                    p -= 12;
                }
                while p < lo {
                    p += 12;
                }
            }
        }
        p
    }
}

/// The v0.8 functions that read the other notes: a6 voice leading, a7
/// register balancing (most per octave × average), a10 harmonic window
/// (window, pitch classes kept).
type LimitFrozen = (bool, Option<f64>, Option<(u64, usize)>);

/// The Limit's pitch rule over the whole source (the v0.8 contextual path).
/// `input`: the pitches that enter the rule (v0.9 n05 / n10; else each
/// note's own); `keep`: the notes still in the result (empty = all) — the
/// others neither lead voices nor count for the balance. `NEW` false is the
/// v0.8 computation, compiled without the v0.9 checks (`input` and `keep`
/// are ignored).
fn limit_pitches<const NEW: bool>(
    l: &Limit,
    src: &Source,
    (voice, balance, window): LimitFrozen,
    input: Option<&[i64]>,
    keep: &[bool],
) -> Vec<i64> {
    let kept = |i: usize| !NEW || keep.is_empty() || keep[i];
    // Harmonic window: allowed set = most frequent pitch classes of
    // the notes starting within ±window/2 (weighted by length).
    let masks: Vec<u16> = match window {
        Some((w, top)) => {
            let half = w / 2;
            let mut lo_i = 0usize;
            let mut hi_i = 0usize;
            let mut weights = [0u128; 12];
            let notes = &src.notes;
            notes
                .iter()
                .map(|n| {
                    let a = n.start.saturating_sub(half);
                    let bnd = n.start.saturating_add(half);
                    while hi_i < notes.len() && notes[hi_i].start <= bnd {
                        weights[usize::from(notes[hi_i].pitch % 12)] += u128::from(notes[hi_i].dur);
                        hi_i += 1;
                    }
                    while lo_i < notes.len() && notes[lo_i].start < a {
                        weights[usize::from(notes[lo_i].pitch % 12)] -= u128::from(notes[lo_i].dur);
                        lo_i += 1;
                    }
                    let mut order: Vec<usize> = (0..12).collect();
                    order.sort_by_key(|&c| (std::cmp::Reverse(weights[c]), c));
                    order.iter().take(top).fold(0u16, |m, &c| m | 1 << c)
                })
                .collect()
        }
        None => vec![l.mask; src.notes.len()],
    };
    // Previous output pitch per (track, subchannel) for voice leading.
    let mut prev: std::collections::HashMap<(u16, u32), i64> = Default::default();
    let mut pitches: Vec<i64> = Vec::with_capacity(src.notes.len());
    for (i, n) in src.notes.iter().enumerate() {
        let moved;
        let n = match input.filter(|_| NEW) {
            Some(inp) => {
                moved = TNote {
                    pitch: inp[i].clamp(0, 255) as u8,
                    ..*n
                };
                &moved
            }
            None => n,
        };
        if !kept(i) {
            pitches.push(i64::from(n.pitch));
            continue;
        }
        let pv = if voice {
            prev.get(&(n.track, n.sub)).copied()
        } else {
            None
        };
        let p = l.pitch(n, masks[i], pv);
        prev.insert((n.track, n.sub), p);
        pitches.push(p);
    }
    if let Some(limit) = balance {
        // Move notes from crowded octaves to the least crowded one
        // in range (same pitch class).
        let mut counts: std::collections::BTreeMap<i64, usize> = Default::default();
        let mut total = 0usize;
        for (i, &p) in pitches.iter().enumerate() {
            if kept(i) {
                *counts.entry(p.div_euclid(12)).or_default() += 1;
                total += 1;
            }
        }
        let (lo, hi) = (l.lo.min(l.hi), l.lo.max(l.hi));
        let (lo_o, hi_o) = (lo.div_euclid(12), hi.div_euclid(12));
        // Average over every octave of the range (empty ones too).
        let avg = total as f64 / (hi_o - lo_o + 1).max(1) as f64;
        let cap = (avg * limit).ceil() as usize;
        for (i, p) in pitches.iter_mut().enumerate() {
            if !kept(i) {
                continue;
            }
            let o = p.div_euclid(12);
            if counts.get(&o).copied().unwrap_or(0) > cap {
                let target = (lo_o..=hi_o)
                    .filter(|c| {
                        let q = *p + 12 * (c - o);
                        q >= lo && q <= hi
                    })
                    .min_by_key(|c| counts.get(c).copied().unwrap_or(0));
                if let Some(t) = target
                    && t != o
                {
                    *counts.entry(o).or_default() -= 1;
                    *counts.entry(t).or_default() += 1;
                    *p += 12 * (t - o);
                }
            }
        }
    }
    pitches
}

/// n09: the allowed MIDI channels (1-based) or subchannel positions.
#[derive(Clone)]
struct LimitAllowed {
    subs: bool,
    /// Sorted, distinct.
    set: Vec<i64>,
    /// In the list's order (the first allowed is the list's first).
    order: Vec<i64>,
    remap: usize,
}

impl LimitAllowed {
    /// The allowed value a value outside the set moves to, among those `ok`
    /// accepts: the nearest (the lower on a tie), the next above in turn
    /// (round to the lowest) or the list's first.
    fn pick(&self, from: i64, ok: impl Fn(i64) -> bool) -> Option<i64> {
        match self.remap {
            0 => self
                .set
                .iter()
                .copied()
                .filter(|&a| ok(a))
                .min_by_key(|&a| ((a - from).abs(), a)),
            1 => self
                .set
                .iter()
                .copied()
                .filter(|&a| ok(a))
                .find(|&a| a > from)
                .or_else(|| self.set.iter().copied().find(|&a| ok(a))),
            _ => self.order.iter().copied().find(|&a| ok(a)),
        }
    }

    /// Moves the note to an allowed MIDI channel (its subchannel stays) or
    /// an allowed subchannel of its track (its MIDI channel stays); a track
    /// without any allowed subchannel leaves the note where it is.
    fn remap(&self, t: &mut TNote, subs: &[Vec<(u32, u8)>]) {
        if !self.subs {
            let c = i64::from(t.ch) + 1;
            if self.set.binary_search(&c).is_err()
                && let Some(a) = self.pick(c, |_| true)
            {
                t.ch = (a - 1) as u8;
            }
            return;
        }
        let Some(table) = subs.get(usize::from(t.track)) else {
            return;
        };
        let at = table.iter().position(|s| s.0 == t.sub).unwrap_or(0) as i64;
        let exists = |a: i64| a >= 0 && (a as usize) < table.len();
        if self.set.binary_search(&at).is_ok() && exists(at) {
            return;
        }
        if let Some(a) = self.pick(at, exists) {
            t.sub = table[a as usize].0;
        }
    }
}

/// The Limit's v0.9 functions declared by id (TOOL_CATALOG `limit` n01 …
/// n11). `None` = switched off; with every one off the kernel is the v0.8
/// one.
#[derive(Clone, Default)]
struct LimitNew {
    /// n01: low, high, mode (0 clamp, 1 compress, 2 fold), ratio.
    vel: Option<(i64, i64, usize, f64)>,
    /// n02: shortest, longest, remove the too short (else lengthen), split
    /// the too long into repeats (else cut).
    lengths: Option<(u64, u64, bool, bool)>,
    /// n03: most sounding, remove first (0 quietest, 1 inner voices, 2
    /// shortest, 3 newest), over all selected (else per track).
    poly: Option<(usize, usize, bool)>,
    /// n04: most onsets, window µs (`None` = per tick), remove first (0
    /// quietest, 1 shortest, 2 highest, 3 lowest).
    onsets: Option<(usize, Option<u64>, usize)>,
    /// n05: largest leap, fix (0 octaves toward, 1 fold, 2 remove), line
    /// per (0 track, 1 subchannel, 2 MIDI channel).
    leap: Option<(i64, usize, usize)>,
    /// n06: largest span, keep (0 bass, 1 top, 2 centre), chord tolerance.
    span: Option<(i64, usize, u64)>,
    /// n07: shortest distance, merge (else remove), same key (0 key, 1 +
    /// channel, 2 + subchannel).
    retrigger: Option<(u64, bool, usize)>,
    /// n08: window start, length, outside (0 wrap, 1 fold, 2 clamp, 3
    /// remove).
    window: Option<(u64, u64, usize)>,
    /// n09.
    allowed: Option<LimitAllowed>,
    /// n10: centre, knee, ratio, lattice (`None` = chromatic).
    compress: Option<(i64, i64, f64, Option<Lattice>)>,
    /// n11: most copies, keep (0 lowest, 1 highest, 2 outer), chord
    /// tolerance.
    doubling: Option<(usize, usize, u64)>,
}

impl LimitNew {
    fn any(&self) -> bool {
        self.vel.is_some()
            || self.lengths.is_some()
            || self.contextual()
            || self.window.is_some()
            || self.allowed.is_some()
            || self.compress.is_some()
    }

    /// Functions that read the other notes (whole-source run).
    fn contextual(&self) -> bool {
        self.poly.is_some()
            || self.onsets.is_some()
            || self.leap.is_some()
            || self.span.is_some()
            || self.retrigger.is_some()
            || self.doubling.is_some()
    }

    /// Some function can remove a note.
    fn thins(&self) -> bool {
        self.poly.is_some()
            || self.onsets.is_some()
            || self.retrigger.is_some()
            || self.doubling.is_some()
            || self.leap.is_some_and(|l| l.1 == 2)
            || self.window.is_some_and(|w| w.2 == 3)
            || self.lengths.is_some_and(|l| l.2)
    }

    /// n02 splits: the longest length.
    fn split(&self) -> Option<u64> {
        self.lengths.filter(|l| l.3).map(|l| l.1)
    }

    /// n10: beyond the knee the distance to the centre is divided by the
    /// ratio, then snapped to the nearest semitone or key-and-scale pitch
    /// (a tie toward the centre).
    fn compressed(&self, p: i64) -> i64 {
        let Some((centre, knee, ratio, lattice)) = self.compress else {
            return p;
        };
        let d = p - centre;
        if d.abs() <= knee {
            return p;
        }
        let x = centre as f64 + d.signum() as f64 * (knee as f64 + (d.abs() - knee) as f64 / ratio);
        let (below, above) = match lattice {
            None => (x.floor() as i64, x.ceil() as i64),
            Some(l) => {
                let f = x.floor() as i64;
                let c = x.ceil() as i64;
                (
                    if l.contains(f) { f } else { l.below(f) },
                    if l.contains(c) { c } else { l.above(c) },
                )
            }
        };
        let (db, da) = (x - below as f64, above as f64 - x);
        if (db - da).abs() < 1e-9 {
            // A tie: toward the centre.
            if d > 0 { below } else { above }
        } else if db < da {
            below
        } else {
            above
        }
    }

    /// n05: the pitch of a note leaping more than the limit from `r` (the
    /// previous onset's nearest original pitch on its line), or `None` to
    /// remove it.
    fn leap_fix(&self, p: i64, r: i64) -> Option<i64> {
        let (leap, fix, _) = self.leap?;
        if (p - r).abs() <= leap {
            return Some(p);
        }
        match fix {
            0 => {
                // Octave by octave toward it while that brings it nearer.
                let mut p = p;
                while (p - r).abs() > leap {
                    let q = p - 12 * (p - r).signum();
                    if (q - r).abs() >= (p - r).abs() {
                        break;
                    }
                    p = q;
                }
                Some(p)
            }
            1 => {
                let (lo, span) = (r - leap, 2 * leap);
                let m = (p - lo).rem_euclid(2 * span);
                Some(lo + if m <= span { m } else { 2 * span - m })
            }
            _ => None,
        }
    }

    /// n01.
    fn velocity(&self, v: i64) -> i64 {
        let Some((lo, hi, mode, ratio)) = self.vel else {
            return v;
        };
        match mode {
            0 => v.clamp(lo, hi),
            1 => {
                if v > hi {
                    hi + ((v - hi) as f64 / ratio).round() as i64
                } else if v < lo {
                    lo - ((lo - v) as f64 / ratio).round() as i64
                } else {
                    v
                }
            }
            _ => {
                let span = hi - lo;
                if span == 0 {
                    return lo;
                }
                let m = (v - lo).rem_euclid(2 * span);
                lo + if m <= span { m } else { 2 * span - m }
            }
        }
    }

    /// n08: where a start lands, or `None` (removed).
    fn place(&self, s: u64) -> Option<u64> {
        let Some((a, len, how)) = self.window else {
            return Some(s);
        };
        if s >= a && s - a < len {
            return Some(s);
        }
        let (a, len, s) = (i128::from(a), i128::from(len), i128::from(s));
        let at = match how {
            0 => a + (s - a).rem_euclid(len),
            1 => {
                let m = (s - a).rem_euclid(2 * len);
                a + if m < len { m } else { 2 * len - 1 - m }
            }
            2 => {
                if s < a {
                    a
                } else {
                    a + len - 1
                }
            }
            _ => return None,
        };
        Some(at as u64)
    }

    /// How many notes n02 makes of a note `dur` long (0: removed).
    fn pieces(&self, dur: u64) -> u64 {
        let Some((lo, hi, remove, split)) = self.lengths else {
            return 1;
        };
        if dur > hi && split {
            let whole = dur / hi;
            let rest = dur % hi;
            whole + u64::from(rest > 0 && !(rest < lo && remove))
        } else {
            u64::from(!(dur < lo && remove))
        }
    }

    /// Emits one source note with its pitch through n01, n09, n02 and n08
    /// (each piece of a split note placed in the window on its own).
    fn emit(&self, n: &TNote, pitch: i64, subs: &[Vec<(u32, u8)>], out: &mut Output) {
        let vel = self.velocity(i64::from(n.vel));
        let mut tpl = *n;
        if let Some(a) = &self.allowed {
            a.remap(&mut tpl, subs);
        }
        let mut put = |off: u64, d: u64| match self.place(n.start.saturating_add(off)) {
            Some(s) => out.emit(i128::from(s), i128::from(d), pitch, vel, &tpl),
            None => true,
        };
        let Some((lo, hi, remove, split)) = self.lengths else {
            put(0, n.dur);
            return;
        };
        // A piece shorter than the shortest is lengthened or removed.
        let fit = |d: u64| {
            if d < lo {
                (!remove).then_some(lo)
            } else {
                Some(d)
            }
        };
        if n.dur <= hi || !split {
            if let Some(d) = fit(n.dur.min(hi)) {
                put(0, d);
            }
            return;
        }
        let mut at = 0u64;
        while at < n.dur {
            let d = (n.dur - at).min(hi);
            if let Some(d) = fit(d)
                && !put(at, d)
            {
                return;
            }
            at += d;
        }
    }
}

/// n03: which notes stay so that at most `cap` sound at any tick, per track
/// or over all, removing the quietest, the inner voices (the middle of the
/// sounding pitches), the shortest or the newest first
/// (`density_window::poly_cap_keep`; ties: the later start, then the later
/// note). Muted and zero-length notes never sound and always stay.
fn limit_poly_keep(
    notes: &[TNote],
    cap: &dyn Fn(usize) -> usize,
    first: usize,
    all: bool,
) -> Vec<bool> {
    let keys: Vec<u64> = match first {
        0 => notes.iter().map(|x| 255 - u64::from(x.vel)).collect(),
        2 => notes.iter().map(|x| u64::MAX - x.dur).collect(),
        3 => notes.iter().map(|x| x.start).collect(),
        _ => Vec::new(),
    };
    let order = if first == 1 {
        PolyOrder::MiddlePitch
    } else {
        PolyOrder::Static(&keys)
    };
    poly_cap_keep_by(notes, cap, !all, order)
}

/// n04: which notes stay so that at most `cap` notes start per tick
/// (`window` `None`) or in every trailing window of real time `(t − W, t]`
/// (the canonical NPS window, through the tempo map) — the notes go in the
/// chosen order (the quietest, the shortest, the highest, the lowest; ties
/// spread evenly over time), each only while a window holding it is over
/// (`density_window::budget_keep`). Muted notes neither count nor go.
fn limit_onset_keep(
    notes: &[TNote],
    cap: &dyn Fn(usize) -> usize,
    window: Option<u64>,
    first: usize,
    tt: &mut TempoTime<'_>,
) -> Vec<bool> {
    let mut keep = vec![true; notes.len()];
    let mut by: Vec<(u64, u32)> = notes
        .iter()
        .enumerate()
        .filter(|(_, x)| x.flags & FLAG_MUTED == 0)
        .map(|(i, x)| (x.start, i as u32))
        .collect();
    by.sort_unstable();
    let times: Vec<u64> = match window {
        Some(_) => by.iter().map(|&(s, _)| tt.micros_at(s)).collect(),
        // Per tick: the window (t − 1, t] holds the tick alone.
        None => by.iter().map(|&(s, _)| s).collect(),
    };
    // Ties go in bit-reversed time order, so equal notes (one velocity
    // everywhere) are removed evenly over time rather than from one end.
    let bits = usize::BITS - by.len().leading_zeros();
    let spread = |q: usize| {
        if bits == 0 {
            0
        } else {
            (q as u32).reverse_bits() >> (32 - bits)
        }
    };
    let mut keyed: Vec<(u64, u32, u32)> = by
        .iter()
        .enumerate()
        .map(|(q, &(_, i))| {
            let x = &notes[i as usize];
            let primary = match first {
                0 => u64::from(x.vel),
                1 => x.dur,
                2 => 255 - u64::from(x.pitch),
                _ => u64::from(x.pitch),
            };
            (primary, spread(q), q as u32)
        })
        .collect();
    keyed.sort_unstable();
    let removable: Vec<usize> = keyed.iter().map(|x| x.2 as usize).collect();
    drop(keyed);
    let kept = budget_keep_if_by(
        &times,
        window.unwrap_or(1),
        |q| cap(by[q].1 as usize),
        &removable,
        |_| true,
    );
    for (q, &(_, i)) in by.iter().enumerate() {
        keep[i as usize] = kept[q];
    }
    keep
}

/// n07: which notes stay — a note starting less than `dist` after the
/// previous kept note of its key (the pitch over all selected notes, the
/// pitch and MIDI channel, or the pitch and subchannel of a track) goes,
/// or is merged into that note (which then lasts to the later end). Muted
/// notes are left alone. Returns the keep flags and the merged lengths.
fn limit_retrigger(
    notes: &[TNote],
    dist: &dyn Fn(usize) -> u64,
    same: usize,
) -> (Vec<bool>, Vec<Option<u64>>) {
    let n = notes.len();
    let mut keep = vec![true; n];
    let mut merged: Vec<Option<u64>> = vec![None; n];
    let mut order: Vec<(u64, u64, u32)> = notes
        .iter()
        .enumerate()
        .filter(|(_, x)| x.flags & FLAG_MUTED == 0)
        .map(|(i, x)| {
            let k = match same {
                0 => u64::from(x.pitch),
                1 => u64::from(x.ch) << 8 | u64::from(x.pitch),
                _ => u64::from(x.track) << 40 | u64::from(x.sub) << 8 | u64::from(x.pitch),
            };
            (k, x.start, i as u32)
        })
        .collect();
    order.sort_unstable();
    let mut last: Option<(u64, usize)> = None;
    for &(k, s, i) in &order {
        let i = i as usize;
        match last {
            Some((lk, j)) if lk == k && s - notes[j].start < dist(i) => {
                keep[i] = false;
                let end = notes[i].end();
                let cur = merged[j].map_or(notes[j].end(), |d| notes[j].start + d);
                if end > cur {
                    merged[j] = Some(end - notes[j].start);
                }
            }
            _ => last = Some((k, i)),
        }
    }
    (keep, merged)
}

/// n06: moves the notes of a chord (`pitches` of its members) outside the
/// largest span by octaves to the octave nearest the window kept in place
/// (the bass, the top, or around the centre); a tie moves fewer octaves.
fn limit_span(pitches: &mut [i64], span: i64, keep: usize) {
    if pitches.len() < 2 {
        return;
    }
    let (lo0, hi0) = (
        *pitches.iter().min().unwrap(),
        *pitches.iter().max().unwrap(),
    );
    if hi0 - lo0 <= span {
        return;
    }
    let lo = match keep {
        0 => lo0,
        1 => hi0 - span,
        _ => (lo0 + hi0).div_euclid(2) - span / 2,
    };
    let hi = lo + span;
    for p in pitches.iter_mut() {
        if *p > hi {
            let a = *p - 12 * (*p - hi + 11).div_euclid(12);
            let b = a + 12;
            *p = if a >= lo || lo - a < b - hi { a } else { b };
        } else if *p < lo {
            let a = *p + 12 * (lo - *p + 11).div_euclid(12);
            let b = a - 12;
            *p = if a <= hi || a - hi < lo - b { a } else { b };
        }
    }
}

/// n11: in a chord (`members`: (pitch, note index)), at most `most` notes
/// per pitch class stay — the lowest, the highest or the outer ones
/// (lowest, highest, second lowest, …; unisons by note order); the others'
/// `keep` is cleared.
fn limit_doubling(members: &mut [(i64, usize)], most: usize, how: usize, keep: &mut [bool]) {
    members.sort_unstable_by_key(|&(p, i)| (p.rem_euclid(12), p, i));
    let mut a = 0;
    while a < members.len() {
        let pc = members[a].0.rem_euclid(12);
        let mut b = a;
        while b < members.len() && members[b].0.rem_euclid(12) == pc {
            b += 1;
        }
        let m = b - a;
        if m > most {
            for (r, &(_, i)) in members[a..b].iter().enumerate() {
                let stays = match how {
                    0 => r < most,
                    1 => r >= m - most,
                    // Outer: rank from either end, the low one first.
                    _ => {
                        let from_end = m - 1 - r;
                        let pick = if r <= from_end {
                            2 * r
                        } else {
                            2 * from_end + 1
                        };
                        pick < most
                    }
                };
                if !stays {
                    keep[i] = false;
                }
            }
        }
        a = b;
    }
}

/// The Limit's kernel with a v0.9 function on (the v0.8 kernels are built
/// unchanged otherwise).
struct LimitKernel {
    l: Limit,
    new: LimitNew,
    /// B2-F01: the output-shaping stages n07, n03, n04 in run order
    /// (`order::LIMIT_OUTPUT`).
    order: Vec<&'static str>,
    frozen: LimitFrozen,
    ctx: super::super::Ctx,
    growth: Growth,
    /// With Control Sweeps (E0422; every Limit target is a function's
    /// control): what each level's functions are built from.
    swept: Option<std::sync::Arc<Rebuild>>,
}

impl LimitKernel {
    fn contextual(&self) -> bool {
        self.new.contextual()
            || self.frozen.0
            || self.frozen.1.is_some()
            || self.frozen.2.is_some()
            || self.swept.is_some()
    }

    /// The pitch entering the v0.8 rule after n10, as a note.
    fn input(&self, n: &TNote, p: i64) -> TNote {
        TNote {
            pitch: self.new.compressed(p).clamp(0, 255) as u8,
            ..*n
        }
    }

    fn run_whole(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let new = &self.new;
        // Each note's functions under Control Sweeps (E0422).
        let lv: Option<PerLevel<Option<LimitNew>>> = self
            .swept
            .as_ref()
            .map(|r| r.levels(&self.ctx, src, |p, _| limit_new(p).ok()));
        let nw =
            |i: usize| -> &LimitNew { lv.as_ref().and_then(|l| l.at(i).as_ref()).unwrap_or(new) };
        let n = src.notes.len();
        let base = out.notes.len();
        let mark = out.mark();
        let mut keep = vec![true; n];
        // 1. n05 against the previous onset of the note's line (original
        // pitches), then n10.
        let mut input: Vec<i64> = src.notes.iter().map(|x| i64::from(x.pitch)).collect();
        if let Some((_, _, line)) = new.leap {
            struct Line {
                start: u64,
                prev: Vec<i64>,
                cur: Vec<i64>,
            }
            let mut lines: std::collections::HashMap<(u16, u32), Line> = Default::default();
            for (i, x) in src.notes.iter().enumerate() {
                let k = (
                    x.track,
                    match line {
                        0 => 0,
                        1 => x.sub,
                        _ => u32::from(x.ch),
                    },
                );
                let ln = lines.entry(k).or_insert_with(|| Line {
                    start: x.start,
                    prev: Vec::new(),
                    cur: Vec::new(),
                });
                if x.start > ln.start {
                    ln.prev = std::mem::take(&mut ln.cur);
                    ln.start = x.start;
                }
                let p = i64::from(x.pitch);
                // The previous onset's nearest pitch (the lower on a tie).
                if let Some(&r) = ln.prev.iter().min_by_key(|&&r| ((r - p).abs(), r)) {
                    match nw(i).leap_fix(p, r) {
                        Some(q) => input[i] = q,
                        None => keep[i] = false,
                    }
                }
                ln.cur.push(p);
            }
        }
        for (i, p) in input.iter_mut().enumerate() {
            *p = nw(i).compressed(*p);
        }
        // 2. The v0.8 rule.
        let frozen = self.frozen;
        let mut pitches = if frozen.0 || frozen.1.is_some() || frozen.2.is_some() {
            limit_pitches::<true>(&self.l, src, frozen, Some(&input), &keep)
        } else {
            src.notes
                .iter()
                .zip(&input)
                .map(|(x, &p)| {
                    let t = TNote {
                        pitch: p.clamp(0, 255) as u8,
                        ..*x
                    };
                    self.l.pitch(&t, self.l.mask, None)
                })
                .collect()
        };
        out.check()?;
        // 3. Chords (onsets within a tolerance per track, the original
        // starts): n06 span, then n11 doublings.
        if new.span.is_some() || new.doubling.is_some() {
            let mut buf: Vec<i64> = Vec::new();
            let mut members: Vec<(i64, usize)> = Vec::new();
            for idx in per_track(src) {
                if let Some((_, how, tol)) = new.span {
                    let kept: Vec<usize> = idx.iter().copied().filter(|&i| keep[i]).collect();
                    for g in chords(src, &kept, tol).iter() {
                        // The chord's span: its first note's.
                        let span = nw(g[0]).span.map_or(1, |x| x.0);
                        buf.clear();
                        buf.extend(g.iter().map(|&i| pitches[i]));
                        limit_span(&mut buf, span, how);
                        for (&i, &p) in g.iter().zip(&buf) {
                            pitches[i] = p;
                        }
                    }
                }
                if let Some((_, how, tol)) = new.doubling {
                    let kept: Vec<usize> = idx.iter().copied().filter(|&i| keep[i]).collect();
                    for g in chords(src, &kept, tol).iter() {
                        let most = nw(g[0]).doubling.map_or(1, |x| x.0);
                        members.clear();
                        members.extend(g.iter().map(|&i| (pitches[i], i)));
                        limit_doubling(&mut members, most, how, &mut keep);
                    }
                }
            }
            out.check()?;
        }
        // 4. Velocity, channels, time, length per note (and, under Control
        // Sweeps, each output note's source note for the output passes).
        let mut src_of: Vec<u32> = Vec::new();
        for (i, x) in src.notes.iter().enumerate() {
            if keep[i] {
                nw(i).emit(x, pitches[i], &self.l.subs, out);
                if lv.is_some() {
                    src_of.resize(out.notes.len() - base, i as u32);
                }
            }
            out.check()?;
        }
        // 5. Output shaping in run order (B2-F01; declared: n07 retriggers,
        // n03 polyphony, n04 onsets). Under Control Sweeps each output note
        // keeps its source note's settings: `src_of` follows the removals.
        for id in &self.order {
            let of = |k: usize| -> &LimitNew { src_of.get(k).map_or(new, |&i| nw(i as usize)) };
            let kept: Vec<bool> = match *id {
                "n07" => {
                    let Some((_, merge, same)) = new.retrigger else {
                        continue;
                    };
                    let dist_of = |k: usize| of(k).retrigger.map_or(0, |r| r.0);
                    let (kept, merged) = limit_retrigger(&out.notes[base..], &dist_of, same);
                    let m = kept.len();
                    out.adjust_since(mark, |i, t, _, _| {
                        if i >= m {
                            return true;
                        }
                        if merge && let Some(d) = merged[i] {
                            t.dur = d;
                        }
                        kept[i]
                    });
                    kept
                }
                "n03" => {
                    let Some((_, first, all)) = new.poly else {
                        continue;
                    };
                    let cap_of = |k: usize| of(k).poly.map_or(1, |x| x.0);
                    let kept = limit_poly_keep(&out.notes[base..], &cap_of, first, all);
                    let m = kept.len();
                    out.adjust_since(mark, |i, _, _, _| i >= m || kept[i]);
                    kept
                }
                "n04" => {
                    let Some((_, window, first)) = new.onsets else {
                        continue;
                    };
                    let mut tt = TempoTime::new(&self.ctx);
                    let cap_of = |k: usize| of(k).onsets.map_or(1, |x| x.0);
                    let kept =
                        limit_onset_keep(&out.notes[base..], &cap_of, window, first, &mut tt);
                    let m = kept.len();
                    out.adjust_since(mark, |i, _, _, _| i >= m || kept[i]);
                    kept
                }
                _ => continue,
            };
            if !src_of.is_empty() {
                let mut k = kept.iter();
                src_of.retain(|_| k.next().copied().unwrap_or(true));
            }
            out.check()?;
        }
        Ok(())
    }
}

impl Kernel for LimitKernel {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate_source(&self, src: &Source) -> super::super::Estimate {
        // Under Control Sweeps each note's longest length is its level's:
        // the stats bound (the swept range's shortest) holds.
        if self.new.split().is_none() || self.swept.is_some() {
            return self.estimate(&src.stats);
        }
        // n02 splits only the notes longer than the longest: count exactly.
        let total = src
            .notes
            .iter()
            .fold(0u64, |a, x| a.saturating_add(self.new.pieces(x.dur)));
        super::super::Estimate::for_notes(src.notes.len() as u64, total)
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        (!self.contextual()).then_some(self as &dyn PointKernel)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        if self.contextual() {
            return self.run_whole(src, out);
        }
        for x in &src.notes {
            self.map(x, out);
            out.check()?;
        }
        Ok(())
    }
}

impl PointKernel for LimitKernel {
    fn map(&self, n: &TNote, out: &mut Output) {
        let t = self.input(n, i64::from(n.pitch));
        let p = self.l.pitch(&t, self.l.mask, None);
        self.new.emit(n, p, &self.l.subs, out);
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        if self.new.window.is_some() {
            // n08 brings notes from anywhere into its window.
            return (0, u64::MAX);
        }
        // n02 lengthens up to the shortest length past the note's end.
        let behind = self.new.lengths.map_or(0, |l| l.0);
        (t0.saturating_sub(behind), t1)
    }
}

fn limit_new(p: &Params) -> Result<LimitNew, TransformError> {
    let allowed = if p.flag("n09") {
        let subs = p.choice("n09.target") == 1;
        let order: Vec<i64> = if subs {
            p.list("n09.subs").into_iter().filter(|&a| a >= 0).collect()
        } else {
            p.list("n09.channels")
                .into_iter()
                .filter(|a| (1..=16).contains(a))
                .collect()
        };
        if order.is_empty() {
            return Err(TransformError::Invalid(if subs {
                "Allow at least one subchannel.".into()
            } else {
                "Allow at least one MIDI channel (1 – 16).".into()
            }));
        }
        let mut set = order.clone();
        set.sort_unstable();
        set.dedup();
        Some(LimitAllowed {
            subs,
            set,
            order,
            remap: p.choice("n09.remap"),
        })
    } else {
        None
    };
    Ok(LimitNew {
        vel: p.flag("n01").then(|| {
            let (a, b) = (p.int("n01.lo"), p.int("n01.hi"));
            (
                a.min(b),
                a.max(b),
                p.choice("n01.mode"),
                p.float("n01.ratio").max(1.0),
            )
        }),
        lengths: p.flag("n02").then(|| {
            let (a, b) = (p.ticks("n02.shortest"), p.ticks("n02.longest"));
            (
                a.min(b).max(1),
                a.max(b).max(1),
                p.choice("n02.short") == 1,
                p.choice("n02.long") == 1,
            )
        }),
        poly: p.flag("n03").then(|| {
            (
                p.int("n03.most").max(1) as usize,
                p.choice("n03.first"),
                p.choice("n03.scope") == 1,
            )
        }),
        onsets: p.flag("n04").then(|| {
            (
                p.int("n04.most").max(1) as usize,
                (p.choice("n04.per") == 1)
                    .then(|| ((p.float("n04.window") * 1000.0).round() as u64).max(1)),
                p.choice("n04.first"),
            )
        }),
        leap: p.flag("n05").then(|| {
            (
                p.int("n05.leap").max(1),
                p.choice("n05.fix"),
                p.choice("n05.line"),
            )
        }),
        span: p.flag("n06").then(|| {
            (
                p.int("n06.span").max(1),
                p.choice("n06.keep"),
                p.ticks("n06.tol"),
            )
        }),
        retrigger: p.flag("n07").then(|| {
            (
                p.ticks("n07.dist").max(1),
                p.choice("n07.action") == 1,
                p.choice("n07.same"),
            )
        }),
        window: p.flag("n08").then(|| {
            (
                p.ticks("n08.start"),
                p.ticks("n08.len").max(1),
                p.choice("n08.outside"),
            )
        }),
        allowed,
        compress: p.flag("n10").then(|| {
            (
                p.int("n10.centre"),
                p.int("n10.knee").max(0),
                p.float("n10.ratio").max(1.0),
                (p.choice("n10.snap") == 1)
                    .then(|| Lattice::of_scale(&Scale::new(p.choice("key"), p.choice("scale")))),
            )
        }),
        doubling: p.flag("n11").then(|| {
            (
                p.int("n11.most").max(1) as usize,
                p.choice("n11.keep"),
                p.ticks("n11.tol"),
            )
        }),
    })
}

fn limit_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let scale = Scale::new(p.choice("key"), p.choice("scale"));
    let mut mask = scale.mask();
    if p.adv(1) {
        mask = p
            .list("a1.set")
            .iter()
            .fold(0u16, |m, &c| m | 1 << c.rem_euclid(12));
    }
    if p.adv(9) {
        for c in p.list("a9.exclude") {
            mask &= !(1 << c.rem_euclid(12));
        }
        if mask == 0 {
            return Err(TransformError::Invalid(
                "Every pitch class is excluded; allow at least one.".into(),
            ));
        }
    }
    let ranges = p.list("a3.ranges");
    let l = Limit {
        mask,
        root: scale.root,
        snap: p.choice("snap"),
        lo: p.int("lo"),
        hi: p.int("hi"),
        wrap: p.flag("wrap"),
        degrees: p.adv(2).then(|| {
            let a = (p.int("a2.from") - 1).max(0) as usize;
            let b2 = (p.int("a2.to") - 1).max(0) as usize;
            (a.min(b2), a.max(b2))
        }),
        sub_ranges: if p.adv(3) {
            ranges.chunks_exact(2).map(|c| (c[0], c[1])).collect()
        } else {
            Vec::new()
        },
        fold_centre: p.adv(4),
        mirror: p.adv(5),
        envelope: p.adv(8).then(|| (p.int("a8.lo"), p.int("a8.hi"))),
        stats: b.stats.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
    };
    let voice = p.adv(6);
    let balance = p.adv(7).then(|| p.float("a7.limit"));
    let window = p
        .adv(10)
        .then(|| (p.ticks("a10.window"), p.int("a10.top") as usize));
    let new = limit_new(p)?;
    let swept = Rebuild::of(p, b);
    if new.any() || swept.is_some() {
        // With Control Sweeps (E0422) the whole-source run, each note with
        // its level's functions; n02's pieces bounded by the swept range's
        // shortest longest-length.
        let mut split = new.split();
        if let (Some(hi), Some(sw)) = (split, b.ctx.sweeps.as_deref())
            && swept.is_some()
        {
            split = Some(hi.min(sw.int_bounds("n02.longest", hi as i64).0.max(1) as u64));
        }
        let growth = match split {
            Some(hi) => Growth::Expand(b.stats.max_dur.div_ceil(hi).max(1)),
            None if new.thins() => Growth::Thin,
            None => Growth::Map,
        };
        return Ok(Box::new(LimitKernel {
            l,
            new,
            order: p.sequence(crate::transform::order::LIMIT_OUTPUT.members),
            frozen: (voice, balance, window),
            ctx: b.ctx.clone(),
            growth,
            swept,
        }));
    }
    if voice || balance.is_some() || window.is_some() {
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                let pitches = limit_pitches::<false>(&l, src, (voice, balance, window), None, &[]);
                for (n, p) in src.notes.iter().zip(pitches) {
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        p,
                        i64::from(n.vel),
                        n,
                    );
                    out.check()?;
                }
                Ok(())
            },
            growth: Growth::Map,
            estimate: None,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let p = l.pitch(n, l.mask, None);
            out.emit(
                i128::from(n.start),
                i128::from(n.dur),
                p,
                i64::from(n.vel),
                n,
            );
        },
        ahead: 0,
        behind: 0,
        growth: Growth::Map,
    })
}
