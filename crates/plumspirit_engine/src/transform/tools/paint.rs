//! v0.8 **Paint** (contract §164–§181): subchannel / colour-pattern painting
//! of the selected notes by deterministic, unpredictable-looking subchannel
//! offsets. The notes keep their time, pitch, velocity and MIDI channel
//! (unless asked); only their subchannel (colour) changes.
//!
//! Feature 1 (always on): **triggers** by Time (every interval, from the
//! selection start or bar 1), Pitch (every n semitones) or Notes (every n
//! notes), forward or backward, optionally jittered (seeded). Each trigger
//! moves the subchannel by the signed offset.
//!
//! The fourteen advanced functions compose in this fixed order (each one is
//! optional):
//!
//! 1. Step generators per trigger `j` — a1 random walk, a12 mutation bursts
//!    (and a2 weighted palette in "step" mode) replace the plain offset step;
//!    several generators add their steps; a13 momentum then shapes the step
//!    stream (`v = inertia·v + step + kick`). Position `P[k]` = sum of the
//!    steps of triggers `1 … k`.
//! 2. Per-note terms added to `P[k]`: a2 weighted palette (offset mode,
//!    chosen per trigger), a3 time graph (or an absolute subchannel), a4 pitch
//!    graph, a5 velocity graph, a6 gate graph, a7 density (indexed: binary
//!    search over sorted starts), a8 chord fan-out, a9 connected regions
//!    (`mask::connected_regions`, O(n log n)), a14 2D field.
//! 3. a7 density strength scales everything above except its own term.
//! 4. a11 symmetry re-evaluates the positional terms at the mirrored /
//!    translated position for notes past the axis (time / pitch / both).
//! 5. The wrap policy turns `note's subchannel + total` into a subchannel.
//! 6. a10 neighbour contrast tries `total ± d` (`d ≤ search`) and keeps the
//!    one shared by the fewest already-painted neighbours (bounded window per
//!    pitch row; ties: smallest |d|, then +d).
//!
//! Same source + parameters + seed → same subchannels (Preview = Apply).

use super::super::boundary::{self, PitchPolicy};
use super::super::editors::{Editor, Labels};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::Rng;
use super::super::{Growth, Output, Source, SourceTrack, TNote, TransformError};
use super::mask::connected_regions;
use super::{Contextual, NOTE_VALUES, boxed, note_value};

pub const ID: &str = "subpaint";

pub const PAINT: ToolDef = ToolDef {
    id: ID,
    spec,
    build,
};

/// Semantic editors of Paint's list parameters.
pub fn editor(key: &str) -> Option<Editor> {
    Some(match key {
        "a2.offsets" | "a2.weights" => Editor::Sequence(Labels::Index),
        "a14.field" => Editor::Lattice { angle: false },
        "n02.palette" | "n02.trans" | "n07.palette" => Editor::Sequence(Labels::Index),
        _ => return None,
    })
}

const TRIGGERS: &[&str] = &["Time", "Pitch", "Notes"];
const DIRECTIONS: &[&str] = &[
    "Forward (from the start / lowest / first note)",
    "Backward (from the end / highest / last note)",
];
/// The wrap policies are the shared driver service's (`transform/drivers.rs`).
const WRAPS: &[&str] = super::super::drivers::SUB_WRAPS;
const GRAPH_MODES: &[&str] = &["Offset", "Absolute subchannel"];
const PALETTE_MODES: &[&str] = &[
    "Offset from the note's subchannel (per trigger)",
    "Step added at each trigger",
];
const ORDERS: &[&str] = &[
    "Ascending (lowest voice first)",
    "Descending (highest first)",
];
const AXES: &[&str] = &["Time", "Pitch", "Both"];
const RELATIONS: &[&str] = &["Mirror", "Complement", "Same (repeat)", "Mirror + offset"];
const RETURNS: &[&str] = &[
    "Return to the held subchannel",
    "Stay at the new subchannel",
];
const FIELD_MODES: &[&str] = &["Smooth (between lattice points)", "Cells (nearest point)"];
// V09-S38 step 5: the functions declared by id.
const CA_RULES: &[&str] = &["Majority", "Life-like", "Cyclic"];
const HOODS: &[&str] = &["4 (sides)", "8 (sides and corners)"];
const MARKOV_ORDERS: &[&str] = &["Time", "Pitch"];
const FLASH_REFS: &[&str] = &[
    "A track (its number; 0 = the selection's onsets)",
    "A subchannel of the note's track (its number; 0 = all its notes)",
];
const PHRASE_BY: &[&str] = &[
    "Phrase index",
    "Contour direction (up / down / flat)",
    "Phrase length",
];

/// Salts of Paint's independent random decisions.
mod salt {
    pub const JITTER: u64 = 0x5041_0001;
    pub const WALK: u64 = 0x5041_0002;
    pub const PALETTE: u64 = 0x5041_0003;
    pub const REGION: u64 = 0x5041_0004;
    pub const BURST: u64 = 0x5041_0005;
    pub const KICK: u64 = 0x5041_0006;
}

/// Most triggers a sequential function walks through (a1, a2 steps, a12, a13).
pub const MAX_TRIGGERS: u64 = 64_000_000;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SUBPAINT_HELP: &[(&str, &str)] = &[
    (
        "trigger",
        "What moves the color on: a span of time, a number of semitones up the keyboard, or a count of notes. Each time it passes, every later note moves on by the offset.",
    ),
    (
        "every",
        "For Time: how much time passes between color steps, as a note value.",
    ),
    (
        "every.ticks",
        "For Time with Custom: the time between color steps, in ticks.",
    ),
    (
        "every.pitch",
        "For Pitch: how many semitones up from the lowest note make one color step.",
    ),
    (
        "every.notes",
        "For Notes: how many notes (in time order) make one color step.",
    ),
    (
        "offset",
        "How many subchannels the color moves at each step (+ forward through the track's subchannels, − backward).",
    ),
    (
        "dir",
        "Forward counts the steps from the start, the lowest key or the first note; Backward from the end, the highest key or the last note.",
    ),
    (
        "grid",
        "For Time: counts the steps from bar 1 of the song, so the colors line up with the song's grid instead of the selection's first note.",
    ),
    (
        "jitter",
        "Moves each step point earlier or later by up to this share of the interval (seeded), so the color changes fall unevenly.",
    ),
    (
        "wrap",
        "What happens when a color passes the end: cycle through the track's subchannels, wrap, bounce back or stop within the range below, or create new subchannels as needed.",
    ),
    (
        "range.lo",
        "For the range policies: the first subchannel of the range (1 = the track's first).",
    ),
    (
        "range.n",
        "For the range policies: how many subchannels the range holds.",
    ),
    (
        "set_channel",
        "Also gives each note the MIDI channel of its new subchannel; off keeps each note's MIDI channel.",
    ),
    (
        "a1",
        "Replaces the fixed offset with a seeded random walk: each step goes up or down by a random amount.",
    ),
    (
        "a1.max",
        "The largest step the walk takes at once, in subchannels.",
    ),
    (
        "a1.bias",
        "Leans the walk: + more steps up, − more steps down, 0 = even.",
    ),
    (
        "a1.return",
        "How strongly the walk is pulled back toward the starting color the further it wanders.",
    ),
    (
        "a2",
        "Picks colors from a list of candidate offsets, each with its own chance (seeded).",
    ),
    ("a2.offsets", "The candidate subchannel offsets."),
    (
        "a2.weights",
        "The chance of each candidate, in the same order (relative weights; 0 = never).",
    ),
    (
        "a2.mode",
        "Offset: each step picks one candidate as an offset from the note's color; Step: the picked candidate is added at each step, so the colors travel.",
    ),
    (
        "a3",
        "Adds an offset that follows a graph across the selection's time span, or sets the color outright.",
    ),
    (
        "a3.curve",
        "The graph from the selection's first start (left) to its last (right).",
    ),
    ("a3.lo", "The value where the graph is at its bottom."),
    ("a3.hi", "The value where the graph is at its top."),
    (
        "a3.mode",
        "Offset adds the graph's value to the color; Absolute Subchannel makes the value the subchannel number itself (1 = the first).",
    ),
    (
        "a4",
        "Adds an offset that follows a graph from the lowest key of the selection to its highest.",
    ),
    (
        "a4.curve",
        "The graph from the lowest key (left) to the highest (right).",
    ),
    ("a4.lo", "The offset where the graph is at its bottom."),
    ("a4.hi", "The offset where the graph is at its top."),
    (
        "a5",
        "Adds an offset that follows a graph of each note's velocity.",
    ),
    (
        "a5.curve",
        "The graph from velocity 1 (left) to 127 (right).",
    ),
    ("a5.lo", "The offset where the graph is at its bottom."),
    ("a5.hi", "The offset where the graph is at its top."),
    (
        "a6",
        "Adds an offset that follows a graph of each note's length.",
    ),
    (
        "a6.curve",
        "The graph from the shortest note (left) to the longest (right).",
    ),
    ("a6.lo", "The offset where the graph is at its bottom."),
    ("a6.hi", "The offset where the graph is at its top."),
    (
        "a6.min",
        "The length that counts as shortest, in ticks (0 = the selection's shortest note); shorter notes count as this.",
    ),
    (
        "a6.max",
        "The length that counts as longest, in ticks (0 = the selection's longest note); longer notes count as this.",
    ),
    (
        "a7",
        "Changes the colors by how crowded each note's surroundings are: its own offset, and how strongly the other offsets apply.",
    ),
    (
        "a7.window",
        "How far before and after each note (in ticks) the note starts are counted.",
    ),
    (
        "a7.sparse",
        "The offset given where the notes are sparsest.",
    ),
    ("a7.dense", "The offset given where the notes are densest."),
    (
        "a7.str_sparse",
        "How strongly the other functions' offsets apply where the notes are sparse (100 % = fully, 0 % = not at all).",
    ),
    (
        "a7.str_dense",
        "How strongly the other functions' offsets apply where the notes are dense.",
    ),
    (
        "a8",
        "Gives each voice of a chord its own color, fanned out around the chord's center.",
    ),
    (
        "a8.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "a8.order",
        "Which voice comes first in the fan: the lowest or the highest.",
    ),
    (
        "a8.spacing",
        "How many subchannels apart neighboring voices are.",
    ),
    ("a8.center", "An offset added to the whole fan."),
    (
        "a8.wrap",
        "After this many voices the fan starts again from the first offset (0 = never).",
    ),
    (
        "a9",
        "Gives each connected shape of notes one color of its own (seeded).",
    ),
    (
        "a9.gap",
        "Notes this close in time (ticks) still count as touching, so small gaps do not split a shape.",
    ),
    ("a9.lo", "The smallest offset a shape can get."),
    ("a9.hi", "The largest offset a shape can get."),
    (
        "a10",
        "Nudges each color so it differs from the notes around it, for clearer contrast.",
    ),
    (
        "a10.radius",
        "How many semitones above and below count as neighbors.",
    ),
    (
        "a10.search",
        "How far, in subchannels, a color may be nudged to find one its neighbors do not use.",
    ),
    (
        "a10.window",
        "Notes that ended up to this many ticks earlier still count as neighbors (0 = only overlapping notes).",
    ),
    (
        "a11",
        "Makes the colors symmetrical: the second half of the selection repeats or mirrors the first.",
    ),
    (
        "a11.axis",
        "Where the selection is split in half: in time, in pitch, or both.",
    ),
    (
        "a11.rel",
        "How the second half relates to the first: mirrored, mirrored with the offsets negated, repeated as it is, or mirrored plus an offset.",
    ),
    (
        "a11.k",
        "For Mirror + Offset: the extra offset added to the second half.",
    ),
    (
        "a12",
        "Holds the color steady for a while, then jumps in short random bursts (seeded).",
    ),
    (
        "a12.hold",
        "How many steps the color holds still between bursts.",
    ),
    ("a12.width", "How many steps a burst lasts."),
    (
        "a12.mag",
        "How many subchannels each jump of a burst moves (up or down at random).",
    ),
    (
        "a12.return",
        "After a burst: go back to the color held before it, or carry on from where the burst ended.",
    ),
    (
        "a13",
        "Gives the color steps momentum: each step keeps part of the last one's motion, with random kicks.",
    ),
    (
        "a13.inertia",
        "How much of the previous motion each step keeps (0 % = none, 99 % = drifts on and on).",
    ),
    (
        "a13.kick",
        "The chance at each step of an extra push of one subchannel up or down (seeded).",
    ),
    (
        "a14",
        "Adds an offset read from a square grid of values laid over the selection: time across, pitch up.",
    ),
    (
        "a14.field",
        "The grid's values, bottom row first, left to right (4, 9, 16 … values make a 2 × 2, 3 × 3, 4 × 4 … grid).",
    ),
    (
        "a14.mode",
        "Smooth blends between the grid points; Cells takes the nearest point's value.",
    ),
    (
        "n01",
        "Lets colors spread between neighboring notes like a cellular automaton, on a grid of snap steps × keys.",
    ),
    (
        "n01.rule",
        "Majority: each cell takes its neighbors' most common color; Life-like: colors live, die and are born by neighbor counts; Cyclic: a cell advances to the next color when a neighbor already has it.",
    ),
    ("n01.gens", "How many rounds the automaton runs."),
    (
        "n01.hood",
        "Which cells count as neighbors: the 4 sides, or the sides and the 4 corners.",
    ),
    (
        "n02",
        "Colors the notes as a Markov chain: each note's color is drawn from the palette by chances that depend on the previous note's color (seeded).",
    ),
    (
        "n02.palette",
        "The colors used, as subchannel numbers (1 = the first; 0 = keep the note's own color).",
    ),
    (
        "n02.trans",
        "The chances of moving from each palette entry to each other, one row per entry (row 1 = from the first entry), in percent.",
    ),
    (
        "n02.order",
        "Which note counts as previous: the one before in time, or the one below in pitch.",
    ),
    (
        "n03",
        "Colors the notes from an L-system: a string of letters grown by the rules, read one letter per note in time order.",
    ),
    (
        "n03.axiom",
        "The starting string; letters A – Z stand for subchannels 1 – 26.",
    ),
    (
        "n03.rules",
        "How each letter is rewritten every iteration (A=AB means each A becomes AB); letters without a rule stay.",
    ),
    (
        "n03.iters",
        "How many times the rules are applied to the string.",
    ),
    (
        "n04",
        "Paints a reaction-diffusion pattern (spots and stripes) over the notes, in two colors: subchannel 1 and 2.",
    ),
    (
        "n04.feed",
        "How fast the pattern's substance is fed in; with Kill it decides whether spots, stripes or mazes form.",
    ),
    (
        "n04.kill",
        "How fast the pattern's substance is removed; with Feed it decides whether spots, stripes or mazes form.",
    ),
    (
        "n04.steps",
        "How many simulation steps run before the colors are read (more = a more developed pattern).",
    ),
    (
        "n05",
        "Flashes the color at each onset of a reference, fading back over the decay.",
    ),
    (
        "n05.ref",
        "What gives the onsets: a track (by number), or a subchannel of the note's own track.",
    ),
    (
        "n05.num",
        "The reference's number: a track number or a subchannel number (0 = the selection's own onsets, or all the track's notes).",
    ),
    (
        "n05.flash",
        "How many subchannels the color jumps at an onset.",
    ),
    (
        "n05.decay",
        "How long, in ticks, the flash takes to fade back to no offset (0 = only notes exactly at an onset flash).",
    ),
    (
        "n06",
        "Gives every key its own color walk over time; neighboring keys can share steps.",
    ),
    (
        "n06.max",
        "The largest step a row's walk takes at once, in subchannels.",
    ),
    (
        "n06.corr",
        "The chance that a key repeats the step of the key below, so neighboring rows move together.",
    ),
    (
        "n07",
        "Gives each phrase (notes separated by rests) one color from the palette.",
    ),
    (
        "n07.rest",
        "A silence at least this long, in ticks, begins a new phrase.",
    ),
    (
        "n07.by",
        "Phrase Index: palette entries in turn; Contour Direction: rising, falling and flat phrases get entries 1, 2 and 3; Phrase Length: shorter phrases take earlier entries.",
    ),
    (
        "n07.palette",
        "The colors used, as subchannel numbers (1 = the first; 0 = keep the note's own color).",
    ),
];

fn spec() -> ToolSpec {
    let mut p = vec![
        choice("trigger", "Offset after every", TRIGGERS, 0),
        choice("every", "Time interval", NOTE_VALUES, 2),
        ticks("every.ticks", "Custom interval (ticks)", 1, 1 << 40, 480),
        int("every.pitch", "Pitch interval (semitones)", 1, 127, 2).keyed(),
        int("every.notes", "Note interval (notes)", 1, 1_000_000, 4),
        int(
            "offset",
            "Subchannel offset per trigger (+/−)",
            -4096,
            4096,
            1,
        ),
        choice("dir", "Direction", DIRECTIONS, 0),
        flag(
            "grid",
            "Time: count from bar 1 (song grid) instead of the selection start",
            false,
        ),
        float("jitter", "Interval jitter (seeded)", 0.0, 100.0, 0.0, "%"),
        choice("wrap", "Subchannels", WRAPS, 0),
        int("range.lo", "Range: first subchannel", 1, 1 << 24, 1),
        int("range.n", "Range: subchannels", 1, 4096, 4),
        flag(
            "set_channel",
            "Also set the subchannel's MIDI channel",
            false,
        ),
        seed("seed"),
        // 1 · random walk (function 2)
        adv_switch(1, "Seeded random walk"),
        int("a1.max", "Max step", 1, 64, 2).adv(1),
        float("a1.bias", "Direction bias", -100.0, 100.0, 0.0, "%").adv(1),
        float(
            "a1.return",
            "Return-to-origin tendency",
            0.0,
            100.0,
            20.0,
            "%",
        )
        .adv(1),
        // 2 · weighted palette (function 3)
        adv_switch(2, "Weighted palette field"),
        list(
            "a2.offsets",
            "Candidate offsets",
            -4096,
            4096,
            64,
            &[0, 1, 4],
        )
        .adv(2),
        list("a2.weights", "Weights (%)", 0, 100, 64, &[60, 25, 15]).adv(2),
        choice("a2.mode", "Use", PALETTE_MODES, 0).adv(2),
        // 3 · time graph (function 4)
        adv_switch(3, "Time → subchannel graph"),
        curve("a3.curve", "Selection time → value", Curve::identity())
            .overshoot()
            .adv(3),
        int("a3.lo", "Value at the bottom", -4096, 4096, 0).adv(3),
        int("a3.hi", "Value at the top", -4096, 4096, 7).adv(3),
        choice("a3.mode", "Value is", GRAPH_MODES, 0).adv(3),
        // 4 · pitch graph (function 5)
        adv_switch(4, "Pitch → subchannel graph"),
        curve("a4.curve", "Low → high pitch → offset", Curve::identity())
            .overshoot()
            .adv(4),
        int("a4.lo", "Offset at the bottom", -4096, 4096, 0).adv(4),
        int("a4.hi", "Offset at the top", -4096, 4096, 7).adv(4),
        // 5 · velocity graph (function 6)
        adv_switch(5, "Velocity → subchannel graph"),
        curve("a5.curve", "Velocity 1 … 127 → offset", Curve::identity())
            .overshoot()
            .adv(5),
        int("a5.lo", "Offset at the bottom", -4096, 4096, 0).adv(5),
        int("a5.hi", "Offset at the top", -4096, 4096, 7).adv(5),
        // 6 · gate graph (function 7)
        adv_switch(6, "Gate → subchannel graph"),
        curve("a6.curve", "Short → long → offset", Curve::identity())
            .overshoot()
            .adv(6),
        int("a6.lo", "Offset at the bottom", -4096, 4096, 0).adv(6),
        int("a6.hi", "Offset at the top", -4096, 4096, 7).adv(6),
        ticks(
            "a6.min",
            "Shortest (ticks, 0 = the selection's)",
            0,
            1 << 40,
            0,
        )
        .adv(6),
        ticks(
            "a6.max",
            "Longest (ticks, 0 = the selection's)",
            0,
            1 << 40,
            0,
        )
        .adv(6),
        // 7 · density (function 8)
        adv_switch(7, "Density-reactive paint"),
        ticks("a7.window", "Window (± ticks)", 1, 1 << 40, 480).adv(7),
        int("a7.sparse", "Offset where sparse", -4096, 4096, 0).adv(7),
        int("a7.dense", "Offset where dense", -4096, 4096, 4).adv(7),
        float(
            "a7.str_sparse",
            "Other offsets where sparse",
            0.0,
            400.0,
            100.0,
            "%",
        )
        .adv(7),
        float(
            "a7.str_dense",
            "Other offsets where dense",
            0.0,
            400.0,
            100.0,
            "%",
        )
        .adv(7),
        // 8 · chord fan-out (function 9)
        adv_switch(8, "Chord fan-out"),
        ticks("a8.tol", "Same onset within (ticks)", 0, 1 << 32, 0).adv(8),
        choice("a8.order", "Voices", ORDERS, 0).adv(8),
        int("a8.spacing", "Spacing (subchannels per voice)", -64, 64, 1).adv(8),
        int("a8.center", "Center offset", -4096, 4096, 0).adv(8),
        int("a8.wrap", "Wrap after voices (0 = never)", 0, 64, 0).adv(8),
        // 9 · connected regions (function 10)
        adv_switch(9, "Connected art regions"),
        ticks("a9.gap", "Join across gaps up to (ticks)", 0, 1 << 32, 0).adv(9),
        int("a9.lo", "Lowest region offset", -4096, 4096, 0).adv(9),
        int("a9.hi", "Highest region offset", -4096, 4096, 7).adv(9),
        // 10 · neighbour contrast (function 11)
        adv_switch(10, "Neighbor contrast maximizer"),
        int("a10.radius", "Neighbor rows (± semitones)", 0, 12, 2).adv(10),
        int("a10.search", "Try offsets up to ±", 1, 16, 3).adv(10),
        ticks("a10.window", "Neighbors within (ticks)", 0, 1 << 32, 0).adv(10),
        // 11 · symmetry (function 12)
        adv_switch(11, "Symmetry paint"),
        choice("a11.axis", "Around the", AXES, 0).adv(11),
        choice("a11.rel", "Second half", RELATIONS, 0).adv(11),
        int("a11.k", "Offset (Mirror + offset)", -4096, 4096, 2).adv(11),
        // 12 · mutation bursts (function 13)
        adv_switch(12, "Mutation bursts"),
        int("a12.hold", "Hold for (triggers)", 1, 100_000, 8).adv(12),
        int("a12.width", "Burst width (triggers)", 1, 1000, 2).adv(12),
        int("a12.mag", "Jump size", 1, 4096, 3).adv(12),
        choice("a12.return", "After a burst", RETURNS, 0).adv(12),
        // 13 · momentum (function 14)
        adv_switch(13, "Momentum / inertia"),
        float("a13.inertia", "Inertia", 0.0, 99.0, 70.0, "%").adv(13),
        float("a13.kick", "Kick chance (±1)", 0.0, 100.0, 25.0, "%").adv(13),
        // 14 · 2D field (function 15)
        adv_switch(14, "2D subchannel field"),
        list(
            "a14.field",
            "Field (time × pitch lattice, bottom row first)",
            -4096,
            4096,
            256,
            &[0, 1, 2, 1, 3, 5, 2, 5, 8],
        )
        .adv(14),
        choice("a14.mode", "Between points", FIELD_MODES, 0).adv(14),
        // V09-S38 step 5: the functions declared by id (`docs/TRANSFORM_ENGINE.md`
        // "Paint V2"). Palettes hold subchannel numbers (1 = the first; 0 =
        // the note's own colour) through the wrap policy above.
        flag("n01", "Color automaton", false),
        choice("n01.rule", "Rule", CA_RULES, 0),
        int("n01.gens", "Generations", 1, 64, 4),
        choice("n01.hood", "Neighborhood", HOODS, 1),
        flag("n02", "Markov colors", false),
        list(
            "n02.palette",
            "Palette (subchannel numbers; 0 = the note's own)",
            0,
            4095,
            16,
            &[1, 2, 3],
        ),
        list(
            "n02.trans",
            "Transitions (row by row, % from each palette entry)",
            0,
            100,
            256,
            &[10, 80, 10, 10, 10, 80, 80, 10, 10],
        ),
        choice("n02.order", "Order", MARKOV_ORDERS, 0),
        flag("n03", "L-system colors", false),
        text(
            "n03.axiom",
            "Axiom (letters A – Z = subchannels 1 – 26)",
            256,
            "A",
        ),
        text("n03.rules", "Rules (e.g. A=AB B=A)", 1024, "A=AB B=A"),
        int("n03.iters", "Iterations", 1, 12, 6),
        flag("n04", "Reaction-diffusion", false),
        float("n04.feed", "Feed", 0.0, 10.0, 2.9, "%"),
        float("n04.kill", "Kill", 0.0, 10.0, 5.7, "%"),
        int("n04.steps", "Steps", 1, 1000, 400),
        flag("n05", "Onset flash", false),
        choice("n05.ref", "Reference", FLASH_REFS, 0),
        int("n05.num", "Reference number", 0, 65535, 0),
        int("n05.flash", "Flash offset", -4096, 4096, 2),
        ticks("n05.decay", "Decay (ticks)", 0, 1 << 40, 480),
        flag("n06", "Row walks", false),
        int("n06.max", "Largest step", 1, 64, 1),
        float("n06.corr", "Row correlation", 0.0, 100.0, 50.0, "%"),
        flag("n07", "Phrase colors", false),
        ticks("n07.rest", "Phrase split rest (ticks)", 0, 1 << 40, 480),
        choice("n07.by", "Color by", PHRASE_BY, 0),
        list(
            "n07.palette",
            "Palette (subchannel numbers; 0 = the note's own)",
            0,
            4095,
            64,
            &[1, 2, 3, 4],
        ),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, SUBPAINT_HELP);
    ToolSpec {
        id: ID,
        // v0.9 §087 / §212: shown as "Paint V2"; the id `subpaint` (presets,
        // recipes, journal entries) never changes. §213: the owner's "Paint
        // Extra" is this tool; it lives in the Iterations section.
        name: "Paint V2",
        family: Family::Iterations,
        summary: "Paints the selected notes' subchannels (colors) with deterministic offsets: after every interval of time, pitch or notes the subchannel moves by the offset; fourteen functions add walks, palettes, graphs, density, chords, regions, contrast, symmetry, bursts, momentum and a 2D field. Same seed → same colors.",
        params: p,
        advanced: vec![
            "Seeded random walk",
            "Weighted palette field",
            "Time → subchannel graph",
            "Pitch → subchannel graph",
            "Velocity → subchannel graph",
            "Gate → subchannel graph",
            "Density-reactive paint",
            "Chord fan-out",
            "Connected art regions",
            "Neighbor contrast maximizer",
            "Symmetry paint",
            "Mutation bursts",
            "Momentum / inertia",
            "2D subchannel field",
        ],
        // Orders = where each runs in `Sub9` (§235): the colour assignments
        // first, then the evolution, then the offsets on top.
        functions: [
            ("n01", "Color automaton", 150),
            ("n02", "Markov colors", 120),
            ("n03", "L-system colors", 110),
            ("n04", "Reaction-diffusion", 140),
            ("n05", "Onset flash", 170),
            ("n06", "Row walks", 160),
            ("n07", "Phrase colors", 130),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

#[derive(Clone, Debug)]
struct Graph {
    curve: Curve,
    lo: i64,
    hi: i64,
}

impl Graph {
    fn read(p: &Params, n: u8) -> Graph {
        let k = |s: &str| format!("a{n}.{s}");
        Graph {
            curve: p.curve(&k("curve")),
            lo: p.int(&k("lo")),
            hi: p.int(&k("hi")),
        }
    }

    /// Value at `x ∈ [0, 1]` (rounded half away from zero).
    fn at(&self, x: f64) -> i64 {
        let y = self.curve.eval(x.clamp(0.0, 1.0));
        (self.lo as f64 + (self.hi - self.lo) as f64 * y).round() as i64
    }
}

/// Everything the kernel needs, read once from the parameters.
#[derive(Clone, Debug)]
struct Cfg {
    trigger: usize,
    every_ticks: u64,
    every_pitch: u64,
    every_notes: u64,
    offset: i64,
    backward: bool,
    grid: bool,
    jitter: f64,
    wrap: usize,
    lo: i64,
    n: i64,
    set_channel: bool,
    seed: u64,
    walk: Option<(i64, f64, f64)>,
    palette: Option<(Vec<i64>, Vec<u64>, bool)>,
    time_g: Option<(Graph, bool)>,
    pitch_g: Option<Graph>,
    vel_g: Option<Graph>,
    gate_g: Option<(Graph, u64, u64)>,
    density: Option<(u64, i64, i64, f64, f64)>,
    fan: Option<(u64, bool, i64, i64, i64)>,
    regions: Option<(u64, i64, i64)>,
    contrast: Option<(i64, i64, u64)>,
    symmetry: Option<(usize, usize, i64)>,
    bursts: Option<(u64, u64, i64, bool)>,
    momentum: Option<(f64, f64)>,
    field: Option<(Vec<i64>, usize, bool)>,
}

impl Cfg {
    fn sequential(&self) -> bool {
        self.walk.is_some()
            || self.palette.as_ref().is_some_and(|p| p.2)
            || self.bursts.is_some()
            || self.momentum.is_some()
    }
}

fn build(p: &Params, b: &BuildCtx) -> Result<Box<dyn super::super::Kernel>, TransformError> {
    let palette = p.adv(2).then(|| {
        let offs = p.list("a2.offsets");
        let mut w: Vec<u64> = p
            .list("a2.weights")
            .iter()
            .map(|&x| x.max(0) as u64)
            .collect();
        w.resize(offs.len(), 0);
        (offs, w, p.choice("a2.mode") == 1)
    });
    if let Some((offs, w, _)) = &palette
        && (offs.is_empty() || w.iter().all(|&x| x == 0))
    {
        return Err(TransformError::Invalid(
            "The weighted palette needs at least one candidate offset with a weight above 0."
                .into(),
        ));
    }
    let field = p.adv(14).then(|| {
        let v = p.list("a14.field");
        let s = (v.len() as f64).sqrt() as usize;
        (v, s, p.choice("a14.mode") == 0)
    });
    if let Some((v, s, _)) = &field
        && (*s < 1 || s * s != v.len())
    {
        return Err(TransformError::Invalid(
            "The 2D field must be a square lattice (4, 9, 16, … values).".into(),
        ));
    }
    let cfg = Cfg {
        trigger: p.choice("trigger"),
        every_ticks: note_value(p.choice("every"), p.ticks("every.ticks"), b.ctx),
        every_pitch: p.int("every.pitch").max(1) as u64,
        every_notes: p.int("every.notes").max(1) as u64,
        offset: p.int("offset"),
        backward: p.choice("dir") == 1,
        grid: p.flag("grid"),
        jitter: p.float("jitter").clamp(0.0, 100.0) / 100.0,
        wrap: p.choice("wrap"),
        lo: p.int("range.lo").max(1) - 1,
        n: p.int("range.n").max(1),
        set_channel: p.flag("set_channel"),
        seed: p.seed("seed"),
        walk: p.adv(1).then(|| {
            (
                p.int("a1.max").max(1),
                p.float("a1.bias") / 100.0,
                p.float("a1.return") / 100.0,
            )
        }),
        palette,
        time_g: p
            .adv(3)
            .then(|| (Graph::read(p, 3), p.choice("a3.mode") == 1)),
        pitch_g: p.adv(4).then(|| Graph::read(p, 4)),
        vel_g: p.adv(5).then(|| Graph::read(p, 5)),
        gate_g: p
            .adv(6)
            .then(|| (Graph::read(p, 6), p.ticks("a6.min"), p.ticks("a6.max"))),
        density: p.adv(7).then(|| {
            (
                p.ticks("a7.window").max(1),
                p.int("a7.sparse"),
                p.int("a7.dense"),
                p.float("a7.str_sparse") / 100.0,
                p.float("a7.str_dense") / 100.0,
            )
        }),
        fan: p.adv(8).then(|| {
            (
                p.ticks("a8.tol"),
                p.choice("a8.order") == 1,
                p.int("a8.spacing"),
                p.int("a8.center"),
                p.int("a8.wrap"),
            )
        }),
        regions: p
            .adv(9)
            .then(|| (p.ticks("a9.gap"), p.int("a9.lo"), p.int("a9.hi"))),
        contrast: p.adv(10).then(|| {
            (
                p.int("a10.radius").max(0),
                p.int("a10.search").max(1),
                p.ticks("a10.window"),
            )
        }),
        symmetry: p
            .adv(11)
            .then(|| (p.choice("a11.axis"), p.choice("a11.rel"), p.int("a11.k"))),
        bursts: p.adv(12).then(|| {
            (
                p.int("a12.hold").max(1) as u64,
                p.int("a12.width").max(1) as u64,
                p.int("a12.mag").max(1),
                p.choice("a12.return") == 0,
            )
        }),
        momentum: p
            .adv(13)
            .then(|| (p.float("a13.inertia") / 100.0, p.float("a13.kick") / 100.0)),
        field,
    };
    // V09-S38 step 5: any function declared by id, or a Control Sweep,
    // runs the v0.9 kernel (the v0.8 colours, then the new stages).
    let new = New::read(p)?;
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        let k = Sub9 {
            cfg,
            new,
            order: p.sequence(crate::transform::order::SUBPAINT_PASSES.members),
            ctx: b.ctx.clone(),
        };
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| k.run(src, out),
            growth: Growth::Map,
            estimate: None,
        });
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| paint(&cfg, src, out),
        growth: Growth::Map,
        estimate: None,
    })
}

/// One track's selected notes and its derived data.
struct TrackRun<'a> {
    notes: &'a [TNote],
    tr: Option<&'a SourceTrack>,
    min_start: u64,
    max_start: u64,
    min_pitch: u8,
    max_pitch: u8,
    min_dur: u64,
    max_dur: u64,
    /// Rank of each note in (start, pitch) order (Notes triggers).
    rank: Vec<u64>,
    /// Starts in ascending order (density).
    starts: Vec<u64>,
    max_density: u64,
    /// Chord fan-out offset per note.
    fan: Vec<i64>,
    /// Region offset per note.
    region: Vec<i64>,
    /// Positions `P[k]` for sequential functions (index = trigger).
    seq: Vec<i64>,
    /// Number of triggers (largest index + 1).
    triggers: u64,
}

fn paint(cfg: &Cfg, src: &Source, out: &mut Output) -> Result<(), TransformError> {
    let ns = &src.notes;
    out.reserve(ns.len())?;
    // The source is in time order across tracks: paint each track's notes
    // (subchannels are per track), then emit everything in source order.
    let mut by_track: Vec<Vec<u32>> = vec![Vec::new(); src.tracks.len()];
    for (i, n) in ns.iter().enumerate() {
        if let Some(v) = by_track.get_mut(usize::from(n.track)) {
            v.push(i as u32);
        }
    }
    let mut result: Vec<(u32, u8)> = ns.iter().map(|n| (n.sub, n.ch)).collect();
    for (slot, idx) in by_track.iter().enumerate() {
        if idx.is_empty() {
            continue;
        }
        let notes: Vec<TNote> = idx.iter().map(|&i| ns[i as usize]).collect();
        let run = prepare(cfg, &notes, src.track(slot as u16))?;
        let painted = paint_track(cfg, &run, out)?;
        for (k, &i) in idx.iter().enumerate() {
            result[i as usize] = painted[k];
        }
        out.check()?;
    }
    for (i, x) in ns.iter().enumerate() {
        let mut t = *x;
        (t.sub, t.ch) = result[i];
        if !out.push(t) {
            return out.check();
        }
    }
    Ok(())
}

/// The trigger index of coordinate `x` (≥ 0, units of the trigger) with
/// interval `iv`: the number of trigger points `t_j = j·iv + jitter_j`
/// (`j ≥ 1`) at or below `x`.
fn trigger_index(cfg: &Cfg, x: f64, iv: f64) -> u64 {
    if cfg.jitter == 0.0 {
        return (x / iv).floor().max(0.0) as u64;
    }
    let point = |j: u64| -> f64 {
        let r = Rng::for_index(cfg.seed, salt::JITTER, j).unit() - 0.5;
        j as f64 * iv + r * cfg.jitter * iv
    };
    // Points are nondecreasing (each within ±iv/2 of j·iv), so only
    // `base` and `base + 1` can straddle x.
    let base = (x / iv).floor().max(0.0) as u64;
    if point(base + 1) <= x {
        base + 1
    } else if base >= 1 && point(base) <= x {
        base
    } else {
        base.saturating_sub(1)
    }
}

impl TrackRun<'_> {
    /// Trigger index of a note at time `t` / pitch `p` / rank `r`.
    fn k(&self, cfg: &Cfg, t: u64, p: u8, r: u64) -> u64 {
        match cfg.trigger {
            0 => {
                let origin = if cfg.grid { 0 } else { self.min_start };
                let x = if cfg.backward {
                    self.max_start.saturating_sub(t)
                } else {
                    t.saturating_sub(origin)
                };
                trigger_index(cfg, x as f64, cfg.every_ticks as f64)
            }
            1 => {
                let x = if cfg.backward {
                    self.max_pitch.saturating_sub(p)
                } else {
                    p.saturating_sub(self.min_pitch)
                };
                trigger_index(cfg, f64::from(x), cfg.every_pitch as f64)
            }
            _ => {
                let n = self.notes.len() as u64;
                let x = if cfg.backward { n - 1 - r } else { r };
                trigger_index(cfg, x as f64, cfg.every_notes as f64)
            }
        }
    }

    fn time_pos(&self, t: u64) -> f64 {
        let span = self.max_start.saturating_sub(self.min_start);
        if span == 0 {
            0.0
        } else {
            t.saturating_sub(self.min_start) as f64 / span as f64
        }
    }

    fn pitch_pos(&self, p: u8) -> f64 {
        let span = self.max_pitch.saturating_sub(self.min_pitch);
        if span == 0 {
            0.0
        } else {
            f64::from(p.saturating_sub(self.min_pitch)) / f64::from(span)
        }
    }

    /// Starts within `[t − w, t + w]` (density).
    fn density_at(&self, t: u64, w: u64) -> u64 {
        let a = self.starts.partition_point(|&s| s < t.saturating_sub(w));
        let b = self.starts.partition_point(|&s| s <= t.saturating_add(w));
        (b - a) as u64
    }
}

fn prepare<'a>(
    cfg: &Cfg,
    notes: &'a [TNote],
    tr: Option<&'a SourceTrack>,
) -> Result<TrackRun<'a>, TransformError> {
    let n = notes.len();
    let mut r = TrackRun {
        notes,
        tr,
        min_start: notes.iter().map(|x| x.start).min().unwrap_or(0),
        max_start: notes.iter().map(|x| x.start).max().unwrap_or(0),
        min_pitch: notes.iter().map(|x| x.pitch).min().unwrap_or(0),
        max_pitch: notes.iter().map(|x| x.pitch).max().unwrap_or(0),
        min_dur: notes.iter().map(|x| x.dur).min().unwrap_or(1),
        max_dur: notes.iter().map(|x| x.dur).max().unwrap_or(1),
        rank: Vec::new(),
        starts: Vec::new(),
        max_density: 1,
        fan: Vec::new(),
        region: Vec::new(),
        seq: Vec::new(),
        triggers: 0,
    };
    // Ranks in (start, pitch, content) order — deterministic for ties.
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by_key(|&i| {
        let x = &notes[i as usize];
        (x.start, x.pitch, x.dur, x.sub, x.vel, x.ch, i)
    });
    r.rank = vec![0; n];
    for (k, &i) in order.iter().enumerate() {
        r.rank[i as usize] = k as u64;
    }
    if let Some((w, ..)) = cfg.density {
        r.starts = notes.iter().map(|x| x.start).collect();
        r.starts.sort_unstable();
        r.max_density = notes
            .iter()
            .map(|x| r.density_at(x.start, w))
            .max()
            .unwrap_or(1)
            .max(1);
    }
    if let Some((tol, desc, spacing, center, wrap)) = cfg.fan {
        r.fan = vec![0; n];
        let mut g = 0;
        while g < n {
            let s0 = notes[order[g] as usize].start;
            let mut h = g + 1;
            while h < n && notes[order[h] as usize].start <= s0.saturating_add(tol) {
                h += 1;
            }
            let mut voices: Vec<u32> = order[g..h].to_vec();
            voices.sort_by_key(|&i| (notes[i as usize].pitch, i));
            if desc {
                voices.reverse();
            }
            let m = voices.len() as i64;
            let width = if wrap > 0 { m.min(wrap) } else { m };
            for (v, &i) in voices.iter().enumerate() {
                let v = if wrap > 0 { v as i64 % wrap } else { v as i64 };
                r.fan[i as usize] = center + spacing * v - (spacing * (width - 1)).div_euclid(2);
            }
            g = h;
        }
    }
    if let Some((gap, lo, hi)) = cfg.regions {
        let comp = connected_regions(notes, gap);
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        r.region = comp
            .iter()
            .map(|&c| Rng::for_note(cfg.seed, salt::REGION, &notes[c as usize]).range_i64(lo, hi))
            .collect();
    }
    // Triggers: the largest index any note (or mirrored position) can reach.
    r.triggers = notes
        .iter()
        .enumerate()
        .map(|(i, x)| r.k(cfg, x.start, x.pitch, r.rank[i]))
        .max()
        .map_or(0, |k| k + 1);
    if cfg.symmetry.is_some() {
        // Mirrored / translated coordinates stay inside the span: indices
        // of the span's ends bound them.
        let ends = [
            r.k(cfg, r.min_start, r.min_pitch, 0),
            r.k(cfg, r.max_start, r.max_pitch, n.saturating_sub(1) as u64),
        ];
        r.triggers = r.triggers.max(ends[0] + 1).max(ends[1] + 1);
    }
    if cfg.sequential() {
        if r.triggers > MAX_TRIGGERS {
            return Err(TransformError::TooLarge(format!(
                "The interval makes {} triggers over this selection (at most {} for walks, bursts, momentum or palette steps); use a longer interval.",
                r.triggers, MAX_TRIGGERS
            )));
        }
        r.seq = sequence(cfg, r.triggers);
    }
    Ok(r)
}

/// Positions `P[0 … k−1]` of the sequential functions (step generators and
/// momentum).
fn sequence(cfg: &Cfg, k: u64) -> Vec<i64> {
    let mut out = Vec::with_capacity(k as usize);
    let mut pos: i64 = 0;
    let mut v: f64 = 0.0;
    let mut held: i64 = 0;
    out.push(0);
    let generators =
        cfg.walk.is_some() || cfg.bursts.is_some() || cfg.palette.as_ref().is_some_and(|p| p.2);
    for j in 1..k {
        let mut step: i64 = if generators { 0 } else { cfg.offset };
        if let Some((max, bias, ret)) = cfg.walk {
            let mut r = Rng::for_index(cfg.seed, salt::WALK, j);
            let mag = r.range_i64(1, max);
            // Probability of a positive step: bias, pulled back toward 0.
            let pull = ret * (pos as f64 / max as f64).tanh();
            let p_up = (0.5 + bias / 2.0 - pull / 2.0).clamp(0.0, 1.0);
            step = step.saturating_add(if r.unit() < p_up { mag } else { -mag });
        }
        if let Some((offs, w, true)) = &cfg.palette {
            step = step.saturating_add(weighted(
                offs,
                w,
                Rng::for_index(cfg.seed, salt::PALETTE, j).next_u64(),
            ));
        }
        if let Some((hold, width, mag, back)) = cfg.bursts {
            // `hold` quiet triggers, then `width` seeded jumps of ±mag; with
            // "return", the next quiet period starts at the held subchannel.
            let phase = (j - 1) % (hold + width);
            if phase == hold {
                held = pos;
            }
            if phase >= hold {
                let mut r = Rng::for_index(cfg.seed, salt::BURST, j);
                step = step.saturating_add(if r.unit() < 0.5 { mag } else { -mag });
            } else if phase == 0 && j > 1 && back {
                step = step.saturating_add(held.saturating_sub(pos));
            }
        }
        if let Some((inertia, kick)) = cfg.momentum {
            let mut r = Rng::for_index(cfg.seed, salt::KICK, j);
            let k = if r.unit() < kick {
                if r.unit() < 0.5 { 1.0 } else { -1.0 }
            } else {
                0.0
            };
            v = inertia * v + step as f64 + k;
            step = v.round() as i64;
        }
        pos = pos.saturating_add(step);
        out.push(pos);
    }
    out
}

/// Offset `offs[i]` chosen with probability `w[i] / Σw` by `draw`.
fn weighted(offs: &[i64], w: &[u64], draw: u64) -> i64 {
    let total: u64 = w.iter().sum();
    if total == 0 {
        return 0;
    }
    let mut x = draw % total;
    for (o, &wi) in offs.iter().zip(w) {
        if x < wi {
            return *o;
        }
        x -= wi;
    }
    *offs.last().unwrap_or(&0)
}

/// Bilinear (or nearest) sample of the square lattice at (u, v) ∈ [0, 1]².
fn sample_field(field: &[i64], s: usize, smooth: bool, u: f64, v: f64) -> i64 {
    if s == 1 {
        return field[0];
    }
    let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    if !smooth {
        let i = ((u * s as f64) as usize).min(s - 1);
        let j = ((v * s as f64) as usize).min(s - 1);
        return field[j * s + i];
    }
    let x = u * (s - 1) as f64;
    let y = v * (s - 1) as f64;
    let (i0, j0) = ((x as usize).min(s - 2), (y as usize).min(s - 2));
    let (fx, fy) = (x - i0 as f64, y - j0 as f64);
    let at = |i: usize, j: usize| field[j * s + i] as f64;
    let a = at(i0, j0) * (1.0 - fx) + at(i0 + 1, j0) * fx;
    let b = at(i0, j0 + 1) * (1.0 - fx) + at(i0 + 1, j0 + 1) * fx;
    (a * (1.0 - fy) + b * fy).round() as i64
}

/// The positional part of a note's offset at time `t` / pitch `p` / rank `r`
/// (re-evaluated at mirrored positions by the symmetry), and an absolute
/// base when the time graph sets one.
fn positional(cfg: &Cfg, run: &TrackRun, t: u64, p: u8, r: u64) -> (i64, Option<i64>) {
    let k = run.k(cfg, t, p, r);
    let mut total = if cfg.sequential() {
        run.seq.get(k as usize).copied().unwrap_or(0)
    } else {
        (k as i64).saturating_mul(cfg.offset)
    };
    if let Some((offs, w, false)) = &cfg.palette {
        total = total.saturating_add(weighted(
            offs,
            w,
            Rng::for_index(cfg.seed, salt::PALETTE, k).next_u64(),
        ));
    }
    let mut absolute = None;
    if let Some((g, abs)) = &cfg.time_g {
        let v = g.at(run.time_pos(t));
        if *abs {
            absolute = Some((v - 1).max(0));
        } else {
            total = total.saturating_add(v);
        }
    }
    if let Some(g) = &cfg.pitch_g {
        total = total.saturating_add(g.at(run.pitch_pos(p)));
    }
    if let Some((field, s, smooth)) = &cfg.field {
        total = total.saturating_add(sample_field(
            field,
            *s,
            *smooth,
            run.time_pos(t),
            run.pitch_pos(p),
        ));
    }
    (total, absolute)
}

/// Offset of note `i` before the wrap policy (and the absolute base).
fn offset_of(cfg: &Cfg, run: &TrackRun, i: usize) -> (i64, Option<i64>) {
    let n = &run.notes[i];
    let r = run.rank[i];
    let (mut pos, mut absolute) = positional(cfg, run, n.start, n.pitch, r);
    if let Some((axis, rel, k)) = cfg.symmetry {
        let tc2 = u128::from(run.min_start) + u128::from(run.max_start); // 2 × centre
        let pc2 = u16::from(run.min_pitch) + u16::from(run.max_pitch);
        let past_t = (axis == 0 || axis == 2) && 2 * u128::from(n.start) > tc2;
        let past_p = (axis == 1 || axis == 2) && 2 * u16::from(n.pitch) > pc2;
        if past_t || past_p {
            let half_t = (run.max_start - run.min_start).div_ceil(2);
            let half_p = (run.max_pitch - run.min_pitch).div_ceil(2);
            let nn = run.notes.len() as u64;
            let (t2, p2, r2) = if rel == 2 {
                // Same: translate back by half the span (repeat).
                (
                    if past_t {
                        n.start.saturating_sub(half_t)
                    } else {
                        n.start
                    },
                    if past_p {
                        n.pitch.saturating_sub(half_p)
                    } else {
                        n.pitch
                    },
                    if past_t {
                        r.saturating_sub(nn.div_ceil(2))
                    } else {
                        r
                    },
                )
            } else {
                (
                    if past_t {
                        (tc2 - u128::from(n.start)) as u64
                    } else {
                        n.start
                    },
                    if past_p {
                        (pc2 - u16::from(n.pitch)) as u8
                    } else {
                        n.pitch
                    },
                    if past_t { nn - 1 - r } else { r },
                )
            };
            let (m, mabs) = positional(cfg, run, t2, p2, r2);
            let flips = u32::from(past_t) + u32::from(past_p);
            pos = match rel {
                1 if flips % 2 == 1 => m.saturating_neg(),
                3 => m.saturating_add(k),
                _ => m,
            };
            absolute = mabs;
        }
    }
    // Per-note terms that do not depend on the position.
    if let Some(g) = &cfg.vel_g {
        pos = pos.saturating_add(g.at((f64::from(n.vel) - 1.0) / 126.0));
    }
    if let Some((g, lo, hi)) = &cfg.gate_g {
        let lo = if *lo == 0 { run.min_dur } else { *lo };
        let hi = if *hi == 0 { run.max_dur } else { *hi };
        let x = if hi > lo {
            (n.dur.clamp(lo, hi) - lo) as f64 / (hi - lo) as f64
        } else {
            0.0
        };
        pos = pos.saturating_add(g.at(x));
    }
    if !run.fan.is_empty() {
        pos = pos.saturating_add(run.fan[i]);
    }
    if !run.region.is_empty() {
        pos = pos.saturating_add(run.region[i]);
    }
    if let Some((w, sparse, dense, s_sparse, s_dense)) = cfg.density {
        // Local density at the note itself: its own offset, and the strength
        // of everything above.
        let d = run.density_at(n.start, w) as f64 / run.max_density as f64;
        let own = (sparse as f64 + (dense - sparse) as f64 * d).round() as i64;
        let s = s_sparse + (s_dense - s_sparse) * d;
        pos = own.saturating_add((pos as f64 * s).round() as i64);
    }
    (pos, absolute)
}

/// The subchannel (id, MIDI channel) of note `n` moved by `off` under the
/// wrap policy (`base`: an absolute starting point instead of the note's) —
/// the shared [`super::super::drivers::move_sub`].
fn apply_policy(
    cfg: &Cfg,
    tr: Option<&SourceTrack>,
    n: &TNote,
    off: i64,
    base: Option<i64>,
) -> (u32, u8) {
    use super::super::drivers::{SubWrap, move_sub};
    move_sub(
        SubWrap::from_choice(cfg.wrap, cfg.lo, cfg.n),
        cfg.set_channel,
        tr,
        n,
        off,
        base,
    )
}

/// Each note's (subchannel, MIDI channel) on one track.
fn paint_track(
    cfg: &Cfg,
    run: &TrackRun,
    out: &mut Output,
) -> Result<Vec<(u32, u8)>, TransformError> {
    let n = run.notes.len();
    let mut result: Vec<(u32, u8)> = Vec::with_capacity(n);
    for i in 0..n {
        let (off, base) = offset_of(cfg, run, i);
        result.push(apply_policy(cfg, run.tr, &run.notes[i], off, base));
        if i % 65_536 == 65_535 {
            out.check()?;
        }
    }
    if let Some((radius, search, window)) = cfg.contrast {
        contrast(cfg, run, radius, search, window, &mut result);
    }
    Ok(result)
}

/// Neighbour contrast: notes in start order; each keeps the candidate shared
/// by the fewest already-painted neighbours (overlapping within `window`,
/// pitch within `radius`; at most `PER_ROW` recent notes per row).
fn contrast(
    cfg: &Cfg,
    run: &TrackRun,
    radius: i64,
    search: i64,
    window: u64,
    result: &mut [(u32, u8)],
) {
    const PER_ROW: usize = 8;
    let n = run.notes.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (run.rank[i], i));
    // Per pitch row up to the track's highest pitch (any key range): recent
    // painted notes (end, subchannel), newest last.
    let top = i64::from(run.max_pitch);
    let mut rows: Vec<std::collections::VecDeque<(u64, u32)>> =
        vec![Default::default(); run.max_pitch as usize + 1];
    for &i in &order {
        let x = &run.notes[i];
        let lo_t = x.start.saturating_sub(window);
        let mut neigh: Vec<u32> = Vec::new();
        let p0 = (i64::from(x.pitch) - radius).clamp(0, top) as usize;
        let p1 = (i64::from(x.pitch) + radius).clamp(0, top) as usize;
        for row in &rows[p0..=p1] {
            for &(end, s) in row.iter().rev() {
                if end >= lo_t {
                    neigh.push(s);
                }
            }
        }
        if !neigh.is_empty() {
            let (off, base) = offset_of(cfg, run, i);
            let mut best = (usize::MAX, 0i64, result[i]);
            for d in std::iter::once(0).chain((1..=search).flat_map(|d| [d, -d])) {
                let c = apply_policy(cfg, run.tr, x, off.saturating_add(d), base);
                let cost = neigh.iter().filter(|&&s| s == c.0).count();
                if cost < best.0 {
                    best = (cost, d, c);
                }
                if cost == 0 {
                    break;
                }
            }
            result[i] = best.2;
        }
        let row = &mut rows[usize::from(x.pitch)];
        row.push_back((x.end().saturating_add(window), result[i].0));
        if row.len() > PER_ROW {
            row.pop_front();
        }
    }
}

// ================================================================== v0.9

/// Most cell updates the colour automaton and the reaction-diffusion may
/// make in one run (cells × generations or steps): beyond it the run is
/// refused with a sentence before anything is allocated.
pub const MAX_CELL_UPDATES: u64 = 1 << 29;

/// Most symbols an L-system iteration keeps (exact for rules that never
/// erase a symbol: the first symbols of an iteration come from the first
/// symbols of the one before).
const MAX_LSYSTEM: usize = 1 << 24;

/// The settings of Paint V2's functions declared by id (`None` = off).
#[derive(Clone, Debug)]
struct New {
    /// n01: rule (majority / life-like / cyclic), generations, 8-neighbourhood.
    automaton: Option<(usize, i64, bool)>,
    /// n02: palette, transition weights (K × K, row by row), pitch order.
    markov: Option<(Vec<i64>, Vec<u64>, bool)>,
    /// n03: axiom, rules (by symbol), iterations.
    lsystem: Option<(Vec<u8>, Rules, i64)>,
    /// n04: feed, kill (ratios), steps.
    rd: Option<(f64, f64, i64)>,
    /// n05: reference (track / subchannel), number, flash offset, decay.
    flash: Option<(usize, i64, i64, u64)>,
    /// n06: largest step, row correlation (ratio).
    walks: Option<(i64, f64)>,
    /// n07: split rest, colour by, palette.
    phrases: Option<(u64, usize, Vec<i64>)>,
}

/// n03's productions by symbol (ASCII; `None` = the symbol stays).
type Rules = Vec<Option<Vec<u8>>>;

/// n03's rules: `X=YZ` or `X->YZ` separated by spaces, commas or
/// semicolons; letters are case-insensitive (A – Z are colours, any other
/// symbol is rewritten but paints nothing).
fn parse_rules(text: &str) -> Result<Rules, TransformError> {
    let mut rules: Rules = vec![None; 128];
    let bad = || {
        TransformError::Invalid(
            "L-system rules are written like A=AB B=A: one symbol, '=', its replacement.".into(),
        )
    };
    for tok in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if tok.is_empty() {
            continue;
        }
        let (lhs, rhs) = tok
            .split_once("->")
            .or_else(|| tok.split_once('='))
            .ok_or_else(bad)?;
        let mut l = lhs.bytes();
        let (Some(sym), None) = (l.next(), l.next()) else {
            return Err(bad());
        };
        if !sym.is_ascii_graphic() || !rhs.is_ascii() {
            return Err(bad());
        }
        rules[usize::from(sym.to_ascii_uppercase())] =
            Some(rhs.bytes().map(|b| b.to_ascii_uppercase()).collect());
    }
    Ok(rules)
}

/// The colour letters (A – Z as 1 – 26) of iterations 0 … `iters` of the
/// L-system, each at most `need` long (a note `k` reads entry `k mod len`,
/// so longer sequences are never read past `need`).
fn lsystem(axiom: &[u8], rules: &[Option<Vec<u8>>], iters: usize, need: usize) -> Vec<Vec<u8>> {
    let cap = need.saturating_mul(4).clamp(4096, MAX_LSYSTEM);
    let letters = |s: &[u8]| -> Vec<u8> {
        s.iter()
            .filter(|b| b.is_ascii_uppercase())
            .map(|b| b - b'A' + 1)
            .take(need.max(1))
            .collect()
    };
    let mut cur: Vec<u8> = axiom.iter().map(|b| b.to_ascii_uppercase()).collect();
    cur.truncate(cap);
    let mut out = vec![letters(&cur)];
    for _ in 0..iters {
        let mut next = Vec::with_capacity(cur.len().saturating_mul(2).min(cap));
        for &c in &cur {
            match rules.get(usize::from(c)).and_then(Option::as_ref) {
                Some(r) => next.extend_from_slice(&r[..r.len().min(cap - next.len())]),
                None => next.push(c),
            }
            if next.len() >= cap {
                break;
            }
        }
        out.push(letters(&next));
        cur = next;
    }
    out
}

impl New {
    fn read(p: &Params) -> Result<New, TransformError> {
        let lsystem = if p.flag("n03") {
            Some((
                p.text("n03.axiom").into_bytes(),
                parse_rules(&p.text("n03.rules"))?,
                p.int("n03.iters").clamp(1, 12),
            ))
        } else {
            None
        };
        Ok(New {
            automaton: p.flag("n01").then(|| {
                (
                    p.choice("n01.rule").min(2),
                    p.int("n01.gens").clamp(1, 64),
                    p.choice("n01.hood") == 1,
                )
            }),
            markov: p.flag("n02").then(|| {
                (
                    p.list("n02.palette"),
                    p.list("n02.trans")
                        .iter()
                        .map(|&w| w.max(0) as u64)
                        .collect(),
                    p.choice("n02.order") == 1,
                )
            }),
            lsystem,
            rd: p.flag("n04").then(|| {
                (
                    p.float("n04.feed") / 100.0,
                    p.float("n04.kill") / 100.0,
                    p.int("n04.steps").clamp(1, 1000),
                )
            }),
            flash: p.flag("n05").then(|| {
                (
                    p.choice("n05.ref"),
                    p.int("n05.num").max(0),
                    p.int("n05.flash"),
                    p.ticks("n05.decay"),
                )
            }),
            walks: p
                .flag("n06")
                .then(|| (p.int("n06.max").max(1), p.float("n06.corr") / 100.0)),
            phrases: p.flag("n07").then(|| {
                (
                    p.ticks("n07.rest"),
                    p.choice("n07.by"),
                    p.list("n07.palette"),
                )
            }),
        })
    }

    fn any(&self) -> bool {
        self.automaton.is_some()
            || self.markov.is_some()
            || self.lsystem.is_some()
            || self.rd.is_some()
            || self.flash.is_some()
            || self.walks.is_some()
            || self.phrases.is_some()
    }
}

/// One note's values of the parameters Paint V2's Control Sweeps drive
/// (V09-0143: evaluated per note at its exact place, inside the kernel, so
/// the automaton, the reaction-diffusion, the chains and the phrases see the
/// whole selection).
#[derive(Clone, Copy, Debug, Default)]
struct Vals {
    gens: i64,
    iters: i64,
    feed: f64,
    kill: f64,
    steps: i64,
    flash: i64,
    decay: u64,
    step: i64,
    corr: f64,
    rest: u64,
}

/// The v0.9 Paint V2: the v0.8 colours (`prepare` / `paint_track`, as
/// `paint` computes them), then the functions declared by id in the run
/// order n03, n02, n07, n04, n01, n06, n05 — see `docs/TRANSFORM_ENGINE.md`
/// "Paint V2".
struct Sub9 {
    cfg: Cfg,
    new: New,
    /// B2-F01: the colour stages n03, n02, n07, n04, n01, n06, n05 in run
    /// order (`order::SUBPAINT_PASSES`).
    order: Vec<&'static str>,
    ctx: super::super::Ctx,
}

/// Track `num` (≥ 1, its number in the track list) for n05's reference
/// onsets: its notes from the decay (at least two beats) before the
/// selection to its end as `Source::aux` layer 0 (0 = the selection's own
/// onsets, nothing to read).
pub fn subpaint_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !(p.flag("n05") && p.choice("n05.ref") == 0) || src.stats.is_empty() {
        return Ok(());
    }
    let num = p.int("n05.num");
    if num < 1 {
        return Ok(());
    }
    let t = usize::try_from(num - 1)
        .ok()
        .filter(|&i| doc.track(i).is_some())
        .ok_or_else(|| {
            TransformError::Invalid(format!(
                "Track {num} does not exist. Choose the reference track by its number in the track list (0 = the selection's own onsets)."
            ))
        })?;
    let beat = doc.division().musical_ppqn().unwrap_or(960);
    let back = p.ticks("n05.decay").max(2 * beat);
    let (a, b) = (src.stats.min_start.saturating_sub(back), src.stats.max_end);
    let mut layer = Vec::new();
    // B2-F16: the reference track as the song shows it, on these ticks.
    let shift = ctx.map_or(0, |c| c.shift);
    super::super::source::placed_query(doc, t, shift, a, b, &mut |r| {
        if layer.len() < limit {
            layer.push(TNote::from_record(&r, u16::MAX));
        }
    });
    src.aux = std::sync::Arc::new(vec![layer]);
    Ok(())
}

/// The note grid of one track for n01 / n04: a note sits in the cell of its
/// start (snap-step columns from the selection's first start × rows);
/// neighbours of each occupied cell in a flat list, the sides (above, left,
/// right, below) before the corners.
struct Cells {
    /// The cell of each note (track order).
    of: Vec<u32>,
    /// (row, column) of each cell, sorted.
    key: Vec<(u8, u64)>,
    /// Neighbours of cell `c`: sides `nb[start[c] .. mid[c]]`, corners
    /// `nb[mid[c] .. start[c + 1]]`.
    start: Vec<u32>,
    mid: Vec<u32>,
    nb: Vec<u32>,
}

impl Cells {
    fn of(notes: &[TNote], t0: u64, snap: u64) -> Cells {
        // (row, column, note) packed: one sort without indirection.
        let snap = snap.max(1);
        let mut order: Vec<u128> = notes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                u128::from(n.pitch) << 96
                    | u128::from(n.start.saturating_sub(t0) / snap) << 32
                    | i as u128
            })
            .collect();
        order.sort_unstable();
        let mut key: Vec<(u8, u64)> = Vec::new();
        let mut of = vec![0u32; notes.len()];
        for &x in &order {
            let k = ((x >> 96) as u8, (x >> 32) as u64);
            if key.last() != Some(&k) {
                key.push(k);
            }
            of[x as u32 as usize] = key.len() as u32 - 1;
        }
        drop(order);
        // Rows: the cells of row `r` are key[row[r] .. row[r + 1]].
        let mut row = vec![0usize; 257];
        for &(r, _) in &key {
            row[usize::from(r) + 1] += 1;
        }
        for r in 0..256 {
            row[r + 1] += row[r];
        }
        // Neighbours by a merge walk: the rows above and below are scanned
        // with pointers that only move forward as the column grows.
        let near = |sl: &[(u8, u64)], base: usize, p: &mut usize, x: u64| {
            let lo = x.saturating_sub(1);
            while *p < sl.len() && sl[*p].1 < lo {
                *p += 1;
            }
            let (mut l, mut m, mut r) = (None, None, None);
            let mut q = *p;
            while q < sl.len() && sl[q].1 <= x.saturating_add(1) {
                let c = sl[q].1;
                let at = Some((base + q) as u32);
                if x.checked_sub(1) == Some(c) {
                    l = at;
                } else if c == x {
                    m = at;
                } else if x.checked_add(1) == Some(c) {
                    r = at;
                }
                q += 1;
            }
            (l, m, r)
        };
        let mut start = Vec::with_capacity(key.len() + 1);
        let mut mid = Vec::with_capacity(key.len());
        let mut nb = Vec::new();
        start.push(0u32);
        for r in 0..256usize {
            let (a, b) = (row[r], row[r + 1]);
            if a == b {
                continue;
            }
            let (ua, ub) = if r > 0 { (row[r - 1], row[r]) } else { (a, a) };
            let (da, db) = if r < 255 {
                (row[r + 1], row[r + 2])
            } else {
                (b, b)
            };
            let (mut pu, mut pd) = (0usize, 0usize);
            for g in a..b {
                let x = key[g].1;
                let up = near(&key[ua..ub], ua, &mut pu, x);
                let dn = near(&key[da..db], da, &mut pd, x);
                let left = (g > a && x.checked_sub(1) == Some(key[g - 1].1)).then(|| g as u32 - 1);
                let right =
                    (g + 1 < b && x.checked_add(1) == Some(key[g + 1].1)).then(|| g as u32 + 1);
                // Sides (above, left, right, below), then the corners.
                nb.extend([up.1, left, right, dn.1].into_iter().flatten());
                mid.push(nb.len() as u32);
                nb.extend([up.0, up.2, dn.0, dn.2].into_iter().flatten());
                start.push(nb.len() as u32);
            }
        }
        Cells {
            of,
            key,
            start,
            mid,
            nb,
        }
    }

    fn len(&self) -> usize {
        self.key.len()
    }

    /// The neighbours of cell `c`: the sides, with the corners when `eight`.
    #[inline]
    fn around(&self, c: usize, eight: bool) -> &[u32] {
        let end = if eight {
            self.start[c + 1]
        } else {
            self.mid[c]
        };
        &self.nb[self.start[c] as usize..end as usize]
    }

    /// The sides and the corners of cell `c`.
    #[inline]
    fn split(&self, c: usize) -> (&[u32], &[u32]) {
        let (a, m, b) = (
            self.start[c] as usize,
            self.mid[c] as usize,
            self.start[c + 1] as usize,
        );
        (&self.nb[a..m], &self.nb[m..b])
    }
}

/// The most frequent of `ids` (ties: `own` when it is among them, else the
/// smallest id); sorts `ids`.
fn majority(ids: &mut [u32], own: Option<u32>) -> Option<u32> {
    ids.sort_unstable();
    let mut best: Option<(usize, u32)> = None;
    let mut i = 0;
    while i < ids.len() {
        let id = ids[i];
        let mut e = i + 1;
        while e < ids.len() && ids[e] == id {
            e += 1;
        }
        let c = e - i;
        // Ascending ids: an equal count replaces only for the own colour.
        if best.is_none_or(|(bc, bid)| c > bc || (c == bc && Some(id) == own && Some(bid) != own)) {
            best = Some((c, id));
        }
        i = e;
    }
    best.map(|b| b.1)
}

/// `x` within 0 … 1 by `max` / `min` (2 instructions; `f32::clamp`'s NaN
/// handling made the reaction-diffusion step a third slower).
#[inline(always)]
#[allow(clippy::manual_clamp)]
fn unit(x: f32) -> f32 {
    x.max(0.0).min(1.0)
}

/// Each note's last value in `record`: notes waiting for update `t` (their
/// own generation / step count) take their cell's value after it.
fn by_generation(counts: &[i64]) -> Vec<Vec<u32>> {
    let max = counts.iter().copied().max().unwrap_or(0).max(0) as usize;
    let mut at = vec![Vec::new(); max + 1];
    for (k, &g) in counts.iter().enumerate() {
        at[g.max(0) as usize].push(k as u32);
    }
    at
}

impl Sub9 {
    /// The note's colour (`cur`) with subchannel `id` (its MIDI channel too
    /// with "set the channel").
    fn with_sub(&self, tr: Option<&SourceTrack>, cur: (u32, u8), id: u32) -> (u32, u8) {
        let ch = if self.cfg.set_channel {
            tr.and_then(|t| t.sub_channel(id)).unwrap_or(cur.1)
        } else {
            cur.1
        };
        (id, ch)
    }

    /// The colour `cur` moved by `off` under the wrap policy.
    fn shift(&self, tr: Option<&SourceTrack>, n: &TNote, cur: (u32, u8), off: i64) -> (u32, u8) {
        if off == 0 {
            return cur;
        }
        let mut t = *n;
        (t.sub, t.ch) = cur;
        apply_policy(&self.cfg, tr, &t, off, None)
    }

    /// Palette entry `v` (a subchannel number; 0 = keep `cur`) under the
    /// wrap policy.
    fn number(&self, tr: Option<&SourceTrack>, n: &TNote, cur: (u32, u8), v: i64) -> (u32, u8) {
        if v <= 0 {
            return cur;
        }
        let mut t = *n;
        (t.sub, t.ch) = cur;
        apply_policy(&self.cfg, tr, &t, 0, Some(v - 1))
    }

    /// Every note's swept values (one `Vals` for all without sweeps).
    fn vals(&self, src: &Source) -> (Vals, Vec<Vals>) {
        let nw = &self.new;
        let base = Vals {
            gens: nw.automaton.map_or(0, |a| a.1),
            iters: nw.lsystem.as_ref().map_or(0, |l| l.2),
            feed: nw.rd.map_or(0.0, |r| r.0),
            kill: nw.rd.map_or(0.0, |r| r.1),
            steps: nw.rd.map_or(0, |r| r.2),
            flash: nw.flash.map_or(0, |f| f.2),
            decay: nw.flash.map_or(0, |f| f.3),
            step: nw.walks.map_or(1, |w| w.0),
            corr: nw.walks.map_or(0.0, |w| w.1),
            rest: nw.phrases.as_ref().map_or(0, |p| p.0),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let i = |k: &str, v: i64| if sw.drives(k) { sw.int(k, v, n, pl) } else { v };
                let f = |k: &str, v: f64, scale: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * scale, n, pl) / scale
                    } else {
                        v
                    }
                };
                Vals {
                    gens: i("n01.gens", base.gens).clamp(1, 64),
                    iters: i("n03.iters", base.iters).clamp(1, 12),
                    feed: f("n04.feed", base.feed, 100.0),
                    kill: f("n04.kill", base.kill, 100.0),
                    steps: i("n04.steps", base.steps).clamp(1, 1000),
                    flash: i("n05.flash", base.flash),
                    decay: i("n05.decay", base.decay as i64).max(0) as u64,
                    step: i("n06.max", base.step).max(1),
                    corr: f("n06.corr", base.corr, 100.0),
                    rest: i("n07.rest", base.rest as i64).max(0) as u64,
                }
            })
            .collect();
        (base, per)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let cfg = &self.cfg;
        let ns = &src.notes;
        out.reserve(ns.len())?;
        let (base, per) = self.vals(src);
        let val = |i: usize| per.get(i).copied().unwrap_or(base);
        let mut by_track: Vec<Vec<u32>> = vec![Vec::new(); src.tracks.len()];
        for (i, n) in ns.iter().enumerate() {
            if let Some(v) = by_track.get_mut(usize::from(n.track)) {
                v.push(i as u32);
            }
        }
        // The cell-update budget over every track, before any work: notes
        // bound the cells; only past the budget are the cells counted.
        let snap = self.ctx.snap.max(1);
        if self.new.automaton.is_some() || self.new.rd.is_some() {
            let work = |exact: bool| -> u64 {
                let mut work = 0u64;
                for idx in &by_track {
                    let cells = if exact {
                        let mut k: Vec<u128> = idx
                            .iter()
                            .map(|&i| {
                                let n = &ns[i as usize];
                                u128::from(n.pitch) << 64
                                    | u128::from(n.start.saturating_sub(src.stats.min_start) / snap)
                            })
                            .collect();
                        k.sort_unstable();
                        k.dedup();
                        k.len() as u64
                    } else {
                        idx.len() as u64
                    };
                    let most = |f: &dyn Fn(&Vals) -> i64| {
                        idx.iter()
                            .map(|&i| f(&val(i as usize)))
                            .max()
                            .unwrap_or(0)
                            .max(0) as u64
                    };
                    if self.new.automaton.is_some() {
                        work = work.saturating_add(cells.saturating_mul(most(&|v| v.gens)));
                    }
                    if self.new.rd.is_some() {
                        work = work.saturating_add(cells.saturating_mul(most(&|v| v.steps)));
                    }
                }
                work
            };
            if work(false) > MAX_CELL_UPDATES {
                let w = work(true);
                if w > MAX_CELL_UPDATES {
                    return Err(TransformError::TooLarge(format!(
                        "The color automaton and the reaction-diffusion would make {w} cell updates (at most {MAX_CELL_UPDATES}); use fewer generations or steps, or a smaller selection."
                    )));
                }
            }
        }
        // n03: the letters of every iteration a note can ask for.
        let need = by_track.iter().map(Vec::len).max().unwrap_or(0);
        let letters = self.new.lsystem.as_ref().map(|(axiom, rules, _)| {
            let most = (0..ns.len())
                .map(|i| val(i).iters)
                .max()
                .unwrap_or(base.iters);
            lsystem(axiom, rules, most.clamp(0, 12) as usize, need)
        });
        let mut result: Vec<(u32, u8)> = ns.iter().map(|n| (n.sub, n.ch)).collect();
        for (slot, idx) in by_track.iter().enumerate() {
            if idx.is_empty() {
                continue;
            }
            let notes: Vec<TNote> = idx.iter().map(|&i| ns[i as usize]).collect();
            let tr = src.track(slot as u16);
            let run = prepare(cfg, &notes, tr)?;
            let mut col = paint_track(cfg, &run, out)?;
            let v: Vec<Vals> = idx.iter().map(|&i| val(i as usize)).collect();
            self.stages(src, tr, &notes, &v, letters.as_deref(), &mut col, out)?;
            for (k, &i) in idx.iter().enumerate() {
                result[i as usize] = col[k];
            }
            out.check()?;
        }
        for (i, x) in ns.iter().enumerate() {
            let mut t = *x;
            (t.sub, t.ch) = result[i];
            if !out.push(t) {
                return out.check();
            }
        }
        Ok(())
    }

    /// The new functions on one track's notes (`col`: their colours so far).
    #[allow(clippy::too_many_arguments)]
    fn stages(
        &self,
        src: &Source,
        tr: Option<&SourceTrack>,
        notes: &[TNote],
        v: &[Vals],
        letters: Option<&[Vec<u8>]>,
        col: &mut [(u32, u8)],
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let n = notes.len();
        let seed = self.cfg.seed;
        // Time order (start, pitch) on the track (packed keys; usually
        // sorted already — the source is in time order).
        let nw = &self.new;
        let time: Vec<u32> = if nw.lsystem.is_some()
            || nw.markov.is_some()
            || nw.phrases.is_some()
            || nw.walks.is_some()
        {
            let mut keys: Vec<u128> = notes
                .iter()
                .enumerate()
                .map(|(k, x)| u128::from(x.start) << 64 | u128::from(x.pitch) << 32 | k as u128)
                .collect();
            if !keys.is_sorted() {
                keys.sort_unstable();
            }
            keys.iter().map(|&x| x as u32).collect()
        } else {
            Vec::new()
        };
        // B2-F01: the colour stages in run order (declared: n03, n02, n07,
        // n04, n01, n06, n05 — each on the colours the ones before it left).
        let mut cells: Option<Cells> = None;
        for id in &self.order {
            match *id {
                "n03" => {
                    // ----- n03 (110): the L-system's letters in time order.
                    if let Some(seqs) = letters {
                        for (j, &k) in time.iter().enumerate() {
                            let k = k as usize;
                            let seq = &seqs[(v[k].iters.max(0) as usize).min(seqs.len() - 1)];
                            if !seq.is_empty() {
                                col[k] = self.number(
                                    tr,
                                    &notes[k],
                                    col[k],
                                    i64::from(seq[j % seq.len()]),
                                );
                            }
                        }
                    }
                }
                "n02" => {
                    // ----- n02 (120): a Markov chain over the palette.
                    if let Some((pal, w, by_pitch)) = &self.new.markov
                        && !pal.is_empty()
                    {
                        let m = pal.len();
                        let mut order = time.clone();
                        if *by_pitch {
                            order.sort_by_key(|&k| {
                                (notes[k as usize].pitch, notes[k as usize].start, k)
                            });
                        }
                        let salt = super::super::shared::seeded_stream::salt_of("subpaint.n02");
                        let mut state = 0usize;
                        for (j, &k) in order.iter().enumerate() {
                            let k = k as usize;
                            if j == 0 {
                                // The entry that already gives the note its colour.
                                state = (0..m)
                                    .find(|&s| self.number(tr, &notes[k], col[k], pal[s]) == col[k])
                                    .unwrap_or(0);
                            } else {
                                let row: Vec<f64> = (0..m)
                                    .map(|t| w.get(state * m + t).copied().unwrap_or(0) as f64)
                                    .collect();
                                let key = super::super::shared::seeded_stream::note_key(&notes[k]);
                                if let Some(t) =
                                    super::super::shared::seeded_stream::stream(seed, salt, &[key])
                                        .weighted(&row)
                                {
                                    state = t;
                                }
                            }
                            col[k] = self.number(tr, &notes[k], col[k], pal[state]);
                        }
                    }
                }
                "n07" => {
                    // ----- n07 (130): one colour per phrase.
                    if let Some((_, by, pal)) = &self.new.phrases
                        && !pal.is_empty()
                    {
                        let idx: Vec<usize> = time.iter().map(|&k| k as usize).collect();
                        let ph = super::super::shared::phrase_detect::Phrases::of_with(
                            notes,
                            &idx,
                            &|k| v[k].rest,
                        );
                        let count = ph.len();
                        let mut rank = vec![0usize; count];
                        if *by == 2 {
                            let mut o: Vec<(u64, usize)> = ph
                                .iter(&idx)
                                .enumerate()
                                .map(|(q, p)| {
                                    let end = p.iter().map(|&k| notes[k].end()).max().unwrap_or(0);
                                    (end - notes[p[0]].start, q)
                                })
                                .collect();
                            o.sort_unstable();
                            for (r, &(_, q)) in o.iter().enumerate() {
                                rank[q] = r;
                            }
                        }
                        for (q, p) in ph.iter(&idx).enumerate() {
                            let e = match by {
                                0 => q % pal.len(),
                                1 => {
                                    let s = super::super::shared::phrase_detect::contour_slope(
                                        notes, p,
                                    );
                                    (if s > 0.0 {
                                        0
                                    } else if s < 0.0 {
                                        1
                                    } else {
                                        2
                                    }) % pal.len()
                                }
                                _ => (rank[q] * pal.len() / count.max(1)).min(pal.len() - 1),
                            };
                            for &k in p {
                                col[k] = self.number(tr, &notes[k], col[k], pal[e]);
                            }
                        }
                    }
                }
                "n04" => {
                    // ----- n04 (140): reaction-diffusion on the note grid.
                    if self.new.rd.is_some() {
                        let cells = cells.get_or_insert_with(|| {
                            Cells::of(notes, src.stats.min_start, self.ctx.snap.max(1))
                        });
                        self.reaction(tr, notes, v, cells, col, out)?;
                    }
                }
                "n01" => {
                    // ----- n01 (150): the colour automaton on the note grid.
                    if let Some((rule, _, eight)) = self.new.automaton {
                        let cells = cells.get_or_insert_with(|| {
                            Cells::of(notes, src.stats.min_start, self.ctx.snap.max(1))
                        });
                        self.automaton(tr, notes, v, cells, (rule, eight), col, out)?;
                    }
                }
                "n06" => {
                    // ----- n06 (160): a walk on every row.
                    if self.new.walks.is_some() {
                        let t0 = src.stats.min_start;
                        let snap = self.ctx.snap.max(1);
                        let salt_s = super::super::shared::seeded_stream::salt_of("subpaint.n06");
                        let salt_c =
                            super::super::shared::seeded_stream::salt_of("subpaint.n06.corr");
                        let mut pos = [0i64; 256];
                        let mut seen = [false; 256];
                        for &k in &time {
                            let k = k as usize;
                            let nt = &notes[k];
                            let r = usize::from(nt.pitch);
                            if !seen[r] {
                                seen[r] = true;
                                continue;
                            }
                            let c = nt.start.saturating_sub(t0) / snap;
                            // The step of the row below (the same column) with the
                            // correlation's chance, else a fresh one.
                            let mut row = r as u64;
                            while row > 0
                                && super::super::shared::seeded_stream::stream(
                                    seed,
                                    salt_c,
                                    &[row, c],
                                )
                                .chance(v[k].corr)
                            {
                                row -= 1;
                            }
                            let m = v[k].step;
                            let step = super::super::shared::seeded_stream::stream(
                                seed,
                                salt_s,
                                &[row, c],
                            )
                            .range_i64(-m, m);
                            pos[r] = pos[r].saturating_add(step);
                            col[k] = self.shift(tr, nt, col[k], pos[r]);
                        }
                    }
                }
                "n05" => {
                    // ----- n05 (170): flashes after the reference's onsets.
                    if let Some((kind, num, _, _)) = self.new.flash {
                        let mut on: Vec<u64> = match (kind, num) {
                            (0, 0) => src.notes.iter().map(|x| x.start).collect(),
                            (1, 0) => notes.iter().map(|x| x.start).collect(),
                            (0, _) => src
                                .aux
                                .first()
                                .map(|l| l.iter().map(|x| x.start).collect())
                                .unwrap_or_default(),
                            _ => notes
                                .iter()
                                .filter(|x| i64::from(x.sub) == num - 1)
                                .map(|x| x.start)
                                .collect(),
                        };
                        on.sort_unstable();
                        on.dedup();
                        for k in 0..n {
                            let nt = &notes[k];
                            let p = on.partition_point(|&o| o <= nt.start);
                            if p == 0 {
                                continue;
                            }
                            let dt = nt.start - on[p - 1];
                            let (f, d) = (v[k].flash, v[k].decay);
                            let off = if dt == 0 {
                                f
                            } else if dt < d {
                                (f as f64 * (1.0 - dt as f64 / d as f64)).round() as i64
                            } else {
                                0
                            };
                            col[k] = self.shift(tr, nt, col[k], off);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// n04: Gray–Scott reaction-diffusion over the occupied cells (sides
    /// 0.2, corners 0.05, no flux across empty cells; D_u = 1, D_v = 0.5,
    /// dt = 1; v below 10⁻¹² is 0), seeded in 15 % of the 4 × 4-cell blocks
    /// (u = ½, v = ¼: single seeded cells diffuse away before they react);
    /// a note takes
    /// subchannel 2 where v > 0.2 after its steps, else subchannel 1.
    fn reaction(
        &self,
        tr: Option<&SourceTrack>,
        notes: &[TNote],
        v: &[Vals],
        cells: &Cells,
        col: &mut [(u32, u8)],
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let m = cells.len();
        let salt = super::super::shared::seeded_stream::salt_of("subpaint.n04");
        let mut u = vec![1.0f32; m];
        let mut w = vec![0.0f32; m];
        for (c, &(r, x)) in cells.key.iter().enumerate() {
            if super::super::shared::seeded_stream::stream(
                self.cfg.seed,
                salt,
                &[u64::from(r) / 4, x / 4],
            )
            .chance(0.15)
            {
                u[c] = 0.5;
                w[c] = 0.25;
            }
        }
        // Feed and feed + kill of each cell: its first note's.
        let mut fk = vec![(0.0f32, 0.0f32); m];
        let mut set = vec![false; m];
        for (k, &c) in cells.of.iter().enumerate() {
            if !set[c as usize] {
                set[c as usize] = true;
                let f = v[k].feed as f32;
                fk[c as usize] = (f, f + v[k].kill as f32);
            }
        }
        // u and v side by side (one load per neighbour). Every cell has eight
        // neighbour slots, sides then corners, a missing one pointing at the
        // cell itself: its difference is 0, so nothing flows across empty
        // cells, and the loop has no branches.
        let mut uv: Vec<[f32; 2]> = u.iter().zip(&w).map(|(&a, &b)| [a, b]).collect();
        drop((u, w));
        let nb8: Vec<[u32; 8]> = (0..m)
            .map(|c| {
                let (sd, cn) = cells.split(c);
                let mut x = [c as u32; 8];
                x[..sd.len()].copy_from_slice(sd);
                x[4..4 + cn.len()].copy_from_slice(cn);
                x
            })
            .collect();
        let wait = by_generation(&v.iter().map(|x| x.steps).collect::<Vec<i64>>());
        let mut uv2 = uv.clone();
        let mut done = vec![0.0f32; notes.len()];
        for (t, waiting) in wait.iter().enumerate() {
            if t > 0 {
                for (c, (n, (&(f, fk2), next))) in
                    nb8.iter().zip(fk.iter().zip(uv2.iter_mut())).enumerate()
                {
                    let g = |k: usize| uv[n[k] as usize];
                    let (s0, s1, s2, s3) = (g(0), g(1), g(2), g(3));
                    let (k0, k1, k2, k3) = (g(4), g(5), g(6), g(7));
                    let [uc, vc] = uv[c];
                    let su = s0[0] + s1[0] + s2[0] + s3[0];
                    let sv = s0[1] + s1[1] + s2[1] + s3[1];
                    let cu = k0[0] + k1[0] + k2[0] + k3[0];
                    let cv = k0[1] + k1[1] + k2[1] + k3[1];
                    let lu = 0.2 * (su - 4.0 * uc) + 0.05 * (cu - 4.0 * uc);
                    let lv = 0.2 * (sv - 4.0 * vc) + 0.05 * (cv - 4.0 * vc);
                    let uvv = uc * vc * vc;
                    let nv = unit(vc + 0.5 * lv + uvv - fk2 * vc);
                    *next = [
                        unit(uc + lu - uvv + f * (1.0 - uc)),
                        // Traces below 10⁻¹² are 0: they never react (u·v
                        // must pass feed + kill), and their squares would
                        // be subnormal floats, slow on every CPU.
                        if nv < 1e-12 { 0.0 } else { nv },
                    ];
                }
                std::mem::swap(&mut uv, &mut uv2);
                out.poll()?;
            }
            for &k in waiting {
                done[k as usize] = uv[cells.of[k as usize] as usize][1];
            }
        }
        for (k, nt) in notes.iter().enumerate() {
            col[k] = self.number(tr, nt, col[k], if done[k] > 0.2 { 2 } else { 1 });
        }
        Ok(())
    }

    /// n01: the colour automaton over the occupied cells (a cell starts with
    /// its notes' majority colour; each note takes its cell's colour after
    /// its generations).
    #[allow(clippy::too_many_arguments)]
    fn automaton(
        &self,
        tr: Option<&SourceTrack>,
        notes: &[TNote],
        v: &[Vals],
        cells: &Cells,
        (rule, eight): (usize, bool),
        col: &mut [(u32, u8)],
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let m = cells.len();
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); m];
        for (k, &c) in cells.of.iter().enumerate() {
            members[c as usize].push(col[k].0);
        }
        let start: Vec<u32> = members
            .iter_mut()
            .map(|ids| majority(ids, None).unwrap_or(0))
            .collect();
        drop(members);
        // The policy's first colour (life-like background) and each
        // colour's successor (cyclic), from a note of the track.
        let probe = notes[0];
        let bg = apply_policy(&self.cfg, tr, &probe, 0, Some(0)).0;
        // Dense colour indices in ascending id order (majority ties take the
        // smallest id); the background and successors are appended.
        let mut ids: Vec<u32> = start.clone();
        ids.sort_unstable();
        ids.dedup();
        let mut index: std::collections::HashMap<u32, u32> = ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32))
            .collect();
        let mut cur: Vec<u32> = start.iter().map(|id| index[id]).collect();
        let mut intern = |id: u32, ids: &mut Vec<u32>| -> u32 {
            *index.entry(id).or_insert_with(|| {
                ids.push(id);
                ids.len() as u32 - 1
            })
        };
        let bgi = intern(bg, &mut ids);
        let mut succ: Vec<Option<u32>> = Vec::new();
        let wait = by_generation(&v.iter().map(|x| x.gens).collect::<Vec<i64>>());
        let mut new = cur.clone();
        let mut done = vec![0u32; notes.len()];
        let mut near: Vec<u32> = Vec::with_capacity(9);
        let mut stable = false;
        for (t, waiting) in wait.iter().enumerate() {
            if t > 0 && !stable {
                let mut changed = false;
                for c in 0..m {
                    let own = cur[c];
                    let nb = cells.around(c, eight);
                    new[c] = match rule {
                        0 => {
                            near.clear();
                            near.extend(nb.iter().map(|&j| cur[j as usize]));
                            if near.iter().all(|&x| x == own) {
                                own
                            } else {
                                near.push(own);
                                majority(&mut near, Some(own)).unwrap_or(own)
                            }
                        }
                        1 => {
                            near.clear();
                            near.extend(nb.iter().map(|&j| cur[j as usize]).filter(|&x| x != bgi));
                            let live = near.len();
                            if own != bgi {
                                if live == 2 || live == 3 { own } else { bgi }
                            } else if live == 3 {
                                majority(&mut near, None).unwrap_or(own)
                            } else {
                                own
                            }
                        }
                        _ => {
                            if succ.len() < ids.len() {
                                succ.resize(ids.len(), None);
                            }
                            let nx = match succ[own as usize] {
                                Some(x) => x,
                                None => {
                                    let mut t = probe;
                                    t.sub = ids[own as usize];
                                    let id = apply_policy(&self.cfg, tr, &t, 1, None).0;
                                    let x = intern(id, &mut ids);
                                    succ[own as usize] = Some(x);
                                    x
                                }
                            };
                            if nb.iter().any(|&j| cur[j as usize] == nx) {
                                nx
                            } else {
                                own
                            }
                        }
                    };
                    changed |= new[c] != own;
                }
                std::mem::swap(&mut cur, &mut new);
                // Nothing changed: every later generation is this one.
                stable = !changed;
                out.poll()?;
            }
            for &k in waiting {
                done[k as usize] = ids[cur[cells.of[k as usize] as usize] as usize];
            }
        }
        for (k, &id) in done.iter().enumerate() {
            col[k] = self.with_sub(tr, col[k], id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::rng::hash;
    use super::*;

    #[test]
    fn weighted_choice_follows_the_weights() {
        let offs = [0, 1, 4];
        let w = [60, 25, 15];
        let mut c = [0usize; 3];
        for d in 0..10_000u64 {
            let o = weighted(&offs, &w, hash(&[d]));
            c[offs.iter().position(|&x| x == o).unwrap()] += 1;
        }
        assert!((5_700..6_300).contains(&c[0]), "{c:?}");
        assert!((2_200..2_800).contains(&c[1]), "{c:?}");
        assert!((1_200..1_800).contains(&c[2]), "{c:?}");
    }

    #[test]
    fn the_field_is_sampled_bottom_row_first() {
        let f = [0, 10, 20, 30];
        assert_eq!(sample_field(&f, 2, true, 0.0, 0.0), 0);
        assert_eq!(sample_field(&f, 2, true, 1.0, 0.0), 10);
        assert_eq!(sample_field(&f, 2, true, 0.0, 1.0), 20);
        assert_eq!(sample_field(&f, 2, true, 0.5, 0.5), 15);
        assert_eq!(sample_field(&f, 2, false, 0.9, 0.9), 30);
    }
}
