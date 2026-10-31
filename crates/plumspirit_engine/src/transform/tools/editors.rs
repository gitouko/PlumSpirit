//! PlumSpirit note editors on the platform: Humanize, Transpose (Wave B) and the
//! other editors (Note Properties, Legato, Join, Group, Note Repeat — Wave H).

use super::super::boundary::{self, Boundary, PitchPolicy};
use super::super::output::{Lane, MAX_EVENTS, Mark};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Distribution, Rng, hash, note_key, salt};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::metric_position::{bar_bounds, bar_place};
use super::super::shared::phrase_detect::Phrases;
use super::super::shared::scale_lattice::Lattice;
use super::super::shared::seeded_stream::{salt_of, stream};
use super::super::shared::tempo_time::TempoTime;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::{KEY_NAMES, Scale, SnapMode, scale_names};
use super::super::{Ctx, FLAG_MUTED, Growth, Kernel, Output, Source, TNote, TransformError};
use super::{
    Contextual, Pointwise, art_grid_values, boxed, note_value, per_track, pos, smooth_noise,
};
use crate::view::grid::MeterMap;

// ================================================================ Humanize

pub const HUMANIZE: ToolDef = ToolDef {
    id: "humanize",
    spec: humanize_spec,
    build: humanize_build,
};

const GATE_UNITS: &[&str] = &["Ticks", "Percent of length"];
/// n01: where the rubato warp returns to the written time.
const HUM_ANCHORS: &[&str] = &["Bar lines", "Phrase ends (rests of a beat or more)", "None"];
const HUM_SHAPES: &[&str] = &["Smooth noise", "Drawn curve"];
const HUM_DIRECTIONS: &[&str] = &["Late", "Early or late"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const HUMANIZE_HELP: &[(&str, &str)] = &[
    (
        "tick.min",
        "The earliest a note may move, in ticks (negative = earlier); each note's offset is drawn between Min and Max.",
    ),
    ("tick.max", "The latest a note may move, in ticks."),
    (
        "gate.unit",
        "Whether the length corrections below are in ticks or in percent of each note's length.",
    ),
    (
        "gate.min",
        "The most a note may be shortened (negative) or the least it is lengthened.",
    ),
    ("gate.max", "The most a note may be lengthened."),
    ("vel.min", "The lowest velocity change (negative = softer)."),
    ("vel.max", "The highest velocity change."),
    (
        "a1",
        "Draws the random offsets in steps, so they land on multiples of the steps below.",
    ),
    (
        "a1.step",
        "Timing offsets are rounded to multiples of this many ticks.",
    ),
    (
        "a1.vstep",
        "Velocity changes are rounded to multiples of this.",
    ),
    (
        "a2",
        "Draws the offsets from a bell curve: most notes change a little, few a lot.",
    ),
    (
        "a2.sigma",
        "How wide the bell is, as a share of half the range (small = most notes stay close to the middle).",
    ),
    (
        "a3",
        "Draws the offsets from a triangle, peaking at a chosen place in the range.",
    ),
    (
        "a3.mode",
        "Where the triangle peaks: −1 = at Min, 0 = in the middle, 1 = at Max.",
    ),
    (
        "a4",
        "Makes the offsets drift smoothly over time, so neighboring notes move alike (like a player's tempo wander).",
    ),
    (
        "a4.len",
        "How many ticks the drift takes to change course (longer = slower, smoother wander).",
    ),
    (
        "a4.mix",
        "Blends independent random offsets (0) with the smooth drift (1).",
    ),
    (
        "a5",
        "Gives every note of a chord the same random offsets, so chords stay together.",
    ),
    (
        "a5.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "a6",
        "Varies dense passages more (or less) than sparse ones.",
    ),
    (
        "a6.window",
        "The time, in ticks, around each note over which density is measured.",
    ),
    (
        "a6.amount",
        "+ gives denser passages more variation; − less.",
    ),
    ("a7", "Makes louder notes looser in time (or tighter)."),
    (
        "a7.amount",
        "+ makes loud notes vary more in time and soft ones less; − the reverse.",
    ),
    (
        "a8",
        "Changes the amount of variation across the selection.",
    ),
    (
        "a8.curve",
        "The amount from the selection's start (left) to its end (right): 0 = none, 1 = the full ranges.",
    ),
    (
        "a9",
        "Gives each subchannel its own timing bias and amount of variation.",
    ),
    (
        "a9.bias",
        "A fixed timing offset for each subchannel in turn, in ticks (+ later, − earlier).",
    ),
    (
        "a9.scale",
        "The amount of variation for each subchannel in turn, in percent (100 = as set).",
    ),
    (
        "a10",
        "Repeats the same random feel every period: notes at the same place in the period (and the same pitch class) get the same offsets.",
    ),
    (
        "a10.period",
        "The length, in ticks, after which the random feel repeats.",
    ),
    (
        "n01",
        "Speeds up and slows down the music smoothly in real time (rubato), returning to the written time at anchors.",
    ),
    (
        "n01.depth",
        "The most a note is pushed early or late by the rubato, in milliseconds.",
    ),
    ("n01.period", "How long one rubato swell lasts, in ticks."),
    (
        "n01.anchor",
        "Where the rubato is back in time: at bar lines, at phrase ends (rests of a beat or more), or nowhere.",
    ),
    (
        "n01.shape",
        "Smooth Noise wanders at random (seeded); Drawn Curve follows the curve below.",
    ),
    (
        "n01.curve",
        "For Drawn Curve: the push over one period (½ = in time, 1 = latest, 0 = earliest).",
    ),
    (
        "n02",
        "Plays the low and high hands with their own timing, like a pianist.",
    ),
    (
        "n02.split",
        "The lowest key of the high hand; lower notes belong to the low hand.",
    ),
    (
        "n02.low",
        "The low hand's timing offset, in ticks (+ later).",
    ),
    (
        "n02.high",
        "The high hand's timing offset, in ticks (+ later).",
    ),
    (
        "n02.spread",
        "A random timing spread for each hand, in ticks.",
    ),
    (
        "n03",
        "Shapes each phrase's velocities in an arc, like a phrase rising and falling.",
    ),
    (
        "n03.depth",
        "How many velocity steps the arc adds at its peak (and takes away where the shape is low).",
    ),
    (
        "n03.shape",
        "The arc across each phrase (½ = unchanged, 1 = the full depth added, 0 = the full depth taken away).",
    ),
    (
        "n03.rest",
        "A silence at least this long, in ticks, begins a new phrase.",
    ),
    (
        "n03.vary",
        "How much the arc's depth varies from phrase to phrase (seeded).",
    ),
    (
        "n04",
        "Accents notes by their place in the bar: each grid position adds or removes velocity.",
    ),
    ("n04.grid", "The grid the bar is divided into."),
    (
        "n04.accents",
        "The velocity change of each grid position in turn, from the bar line; the list repeats.",
    ),
    (
        "n04.spread",
        "A random velocity spread added to every accent.",
    ),
    (
        "n05",
        "Tires on fast repeated notes of a key: each repeat gets softer and looser until a rest.",
    ),
    (
        "n05.rate",
        "Repeats faster than this many per second count as tiring.",
    ),
    ("n05.loss", "The velocity lost with each tiring repeat."),
    (
        "n05.loose",
        "How much the timing variation grows with each tiring repeat.",
    ),
    (
        "n05.rest",
        "A rest at least this long, in ticks, on that key restores full strength.",
    ),
    (
        "n06",
        "Adds copies of the notes as extra players, each with its own timing and velocity.",
    ),
    (
        "n06.players",
        "How many extra players copy the part (the original notes stay as they are).",
    ),
    (
        "n06.spread",
        "The most a copy's timing differs from the original, in ticks.",
    ),
    (
        "n06.vel",
        "The most a copy's velocity differs from the original.",
    ),
    (
        "n06.step",
        "Puts each player this many subchannels further along, so players can be colored apart.",
    ),
    (
        "n07",
        "Makes notes after a big leap land a little off, by the size of the leap.",
    ),
    ("n07.per", "The timing error per octave of leap, in ticks."),
    ("n07.dir", "Late only, or early or late at random."),
    ("n07.most", "The largest timing error, in ticks."),
    (
        "n08",
        "Varies notes on the grid less than notes off it (or the reverse).",
    ),
    ("n08.grid", "The grid that decides which notes are on it."),
    (
        "n08.on",
        "The amount of variation for notes on the grid (100 % = as set).",
    ),
    ("n08.off", "The amount of variation for notes off the grid."),
    (
        "n09",
        "Lets the swing drift slowly between two amounts over time.",
    ),
    ("n09.unit", "The note value whose pairs are swung."),
    (
        "n09.lo",
        "The straightest the swing gets (50 % = straight).",
    ),
    (
        "n09.hi",
        "The heaviest the swing gets (66 % = triplet swing).",
    ),
    (
        "n09.period",
        "How long, in ticks, the swing takes to drift from one end toward the other.",
    ),
    (
        "n10",
        "Overlaps small steps between consecutive notes a little, as fingers do in legato playing.",
    ),
    ("n10.short", "The shortest overlap, in ticks."),
    ("n10.long", "The longest overlap, in ticks."),
    ("n10.steps", "Only steps up to this many semitones overlap."),
    (
        "n11",
        "Rolls chords slightly, so their notes start a few ticks apart.",
    ),
    (
        "n11.spread",
        "The longest roll, in ticks, from the chord's first note to its last.",
    ),
    (
        "n11.up",
        "The chance a chord rolls upward (from its lowest note) rather than downward.",
    ),
    (
        "n11.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
];

fn humanize_spec() -> ToolSpec {
    let mut p = vec![
        int("tick.min", "Tick correction min", -1_000_000, 1_000_000, 0),
        int("tick.max", "Tick correction max", -1_000_000, 1_000_000, 0),
        choice("gate.unit", "Gate correction in", GATE_UNITS, 0),
        int("gate.min", "Gate correction min", -1_000_000, 1_000_000, 0),
        int("gate.max", "Gate correction max", -1_000_000, 1_000_000, 0),
        int("vel.min", "Velocity correction min", -127, 127, 0),
        int("vel.max", "Velocity correction max", -127, 127, 0),
        seed("seed"),
        adv_switch(1, "Uniform, stepped"),
        int("a1.step", "Tick step", 1, 1_000_000, 10).adv(1),
        int("a1.vstep", "Velocity step", 1, 127, 4).adv(1),
        adv_switch(2, "Gaussian"),
        float("a2.sigma", "Spread (σ of half range)", 0.05, 1.0, 0.33, "").adv(2),
        adv_switch(3, "Triangular"),
        float("a3.mode", "Peak position", -1.0, 1.0, 0.0, "").adv(3),
        adv_switch(4, "Correlated random walk"),
        ticks("a4.len", "Correlation length", 1, u64::MAX >> 2, 960).adv(4),
        float("a4.mix", "Walk vs. independent", 0.0, 1.0, 1.0, "").adv(4),
        adv_switch(5, "Chord lock"),
        ticks("a5.tol", "Chord tolerance", 0, 1_000_000, 0).adv(5),
        adv_switch(6, "Density-aware variance"),
        ticks("a6.window", "Density window", 1, u64::MAX >> 2, 960).adv(6),
        float(
            "a6.amount",
            "Denser → more (+) / less (−)",
            -1.0,
            1.0,
            0.5,
            "",
        )
        .adv(6),
        adv_switch(7, "Velocity-linked timing"),
        float(
            "a7.amount",
            "Louder → looser (+) / tighter (−)",
            -1.0,
            1.0,
            0.5,
            "",
        )
        .adv(7),
        adv_switch(8, "Envelope over time"),
        curve(
            "a8.curve",
            "Amount across the selection",
            Curve::line(0.0, 1.0),
        )
        .overshoot()
        .adv(8),
        adv_switch(9, "Per-subchannel profiles"),
        list(
            "a9.bias",
            "Tick bias by subchannel",
            -1_000_000,
            1_000_000,
            4096,
            &[],
        )
        .adv(9),
        list("a9.scale", "Amount % by subchannel", 0, 1000, 4096, &[]).adv(9),
        adv_switch(10, "Transferable fingerprint"),
        ticks("a10.period", "Fingerprint period", 1, u64::MAX >> 2, 3840).adv(10),
        // V09-S36: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Rubato warp", false),
        float("n01.depth", "Rubato depth", 0.0, 500.0, 30.0, "ms"),
        ticks("n01.period", "Rubato period", 1, 1 << 40, 3840),
        choice("n01.anchor", "Anchor", HUM_ANCHORS, 0),
        choice("n01.shape", "Rubato shape", HUM_SHAPES, 0),
        curve(
            "n01.curve",
            "Drawn rubato (½ = in time, 1 = latest)",
            Curve::smooth(vec![(0.0, 0.5), (0.3, 0.9), (0.7, 0.15), (1.0, 0.5)]),
        ),
        flag("n02", "Hand split", false),
        pitch("n02.split", "Split (lowest key of the high hand)", 60),
        int("n02.low", "Low hand offset (ticks)", -2000, 2000, 15),
        int("n02.high", "High hand offset (ticks)", -2000, 2000, 0),
        ticks("n02.spread", "Hand spread", 0, 1 << 30, 10),
        flag("n03", "Phrase dynamics", false),
        int("n03.depth", "Arc depth", 0, 64, 16),
        curve(
            "n03.shape",
            "Arc shape (½ = unchanged)",
            Curve::smooth(vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]),
        )
        .overshoot(),
        ticks("n03.rest", "Phrase split rest", 0, 1 << 40, 480),
        float(
            "n03.vary",
            "Variation between phrases",
            0.0,
            100.0,
            25.0,
            "%",
        ),
        flag("n04", "Metric accents", false),
        choice("n04.grid", "Accent grid", art_grid_values(), 2),
        list(
            "n04.accents",
            "Accent per position",
            -64,
            64,
            64,
            &[10, -4, 4, -4],
        ),
        int("n04.spread", "Accent spread", 0, 32, 4),
        flag("n05", "Repetition fatigue", false),
        float("n05.rate", "Rate threshold", 1.0, 100.0, 8.0, "/s"),
        float("n05.loss", "Velocity loss per repeat", 0.0, 20.0, 2.0, ""),
        float(
            "n05.loose",
            "Looseness gain per repeat",
            0.0,
            50.0,
            10.0,
            "%",
        ),
        ticks("n05.rest", "Recovery rest", 0, 1 << 40, 480),
        flag("n06", "Ensemble copies", false),
        int("n06.players", "Players", 1, 8, 2),
        ticks("n06.spread", "Timing spread", 0, 1 << 30, 20),
        int("n06.vel", "Velocity spread", 0, 64, 8),
        int("n06.step", "Subchannel step per player", -64, 64, 1),
        flag("n07", "Leap error", false),
        ticks("n07.per", "Error per octave", 0, 1 << 30, 20),
        choice("n07.dir", "Direction", HUM_DIRECTIONS, 0),
        ticks("n07.most", "Largest error", 0, 1 << 30, 120),
        flag("n08", "Grid-relative variance", false),
        choice("n08.grid", "Variance grid", art_grid_values(), 4),
        float("n08.on", "On the grid", 0.0, 200.0, 50.0, "%"),
        float("n08.off", "Off the grid", 0.0, 200.0, 150.0, "%"),
        flag("n09", "Swing drift", false),
        choice("n09.unit", "Swing unit", art_grid_values(), 3),
        float("n09.lo", "Lowest swing", 50.0, 80.0, 50.0, "%"),
        float("n09.hi", "Highest swing", 50.0, 80.0, 62.0, "%"),
        ticks("n09.period", "Drift period", 1, 1 << 40, 7680),
        flag("n10", "Finger overlap", false),
        ticks("n10.short", "Shortest overlap", 0, 1 << 30, 5),
        ticks("n10.long", "Longest overlap", 0, 1 << 30, 30),
        int("n10.steps", "Only steps up to (semitones)", 0, 12, 2),
        flag("n11", "Chord micro-roll", false),
        ticks("n11.spread", "Largest roll", 0, 1 << 30, 30),
        float("n11.up", "Up tendency", 0.0, 100.0, 80.0, "%"),
        ticks("n11.tol", "Roll chord tolerance", 0, 1 << 30, 10),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, HUMANIZE_HELP);
    ToolSpec {
        id: "humanize",
        name: "Humanize",
        family: Family::Editor,
        summary: "Randomly varies start times, lengths and velocities within the given ranges (repeatable with the seed).",
        params: p,
        advanced: vec![
            "Uniform, stepped",
            "Gaussian",
            "Triangular",
            "Correlated random walk",
            "Chord lock",
            "Density-aware variance",
            "Velocity-linked timing",
            "Envelope over time",
            "Per-subchannel profiles",
            "Transferable fingerprint",
        ],
        // Orders = the run order (docs/TRANSFORM_ENGINE.md "Humanize"):
        // amounts of the frozen jitter, time maps, offsets, velocity
        // shapes, ends, copies.
        functions: [
            ("n01", "Rubato warp", 140),
            ("n02", "Hand split", 150),
            ("n03", "Phrase dynamics", 180),
            ("n04", "Metric accents", 190),
            ("n05", "Repetition fatigue", 120),
            ("n06", "Ensemble copies", 210),
            ("n07", "Leap error", 160),
            ("n08", "Grid-relative variance", 110),
            ("n09", "Swing drift", 130),
            ("n10", "Finger overlap", 200),
            ("n11", "Chord micro-roll", 170),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// Humanize's v0.9 functions declared by id (TOOL_CATALOG `humanize` n01 …
/// n11). `None` = switched off; with every one off the kernel is the v0.8
/// one.
#[derive(Clone, Default)]
struct HumNew {
    /// n01: the monotone real-time warp.
    rubato: Option<Rubato>,
    /// n02: split key, low / high hand offsets, hand spread.
    hands: Option<(u8, i64, i64, u64)>,
    /// n03: arc depth, arc shape, phrase split rest, variation (fraction).
    arc: Option<(f64, Curve, u64, f64)>,
    /// n04: grid ticks, accent per position, spread.
    accents: Option<(u64, Vec<i64>, f64)>,
    /// n05: the longest interval (µs) that still tires, velocity loss and
    /// looseness gain per repeat (fraction), recovery rest.
    fatigue: Option<(f64, f64, f64, u64)>,
    /// n06: players, timing spread, velocity spread, subchannel step.
    ensemble: Option<(u64, u64, f64, i64)>,
    /// n07: ticks per octave, early or late, largest error.
    leap: Option<(u64, bool, u64)>,
    /// n08: grid ticks, amount on / off the grid (fractions).
    grid: Option<(u64, f64, f64)>,
    /// n09: unit ticks, lowest / highest swing (fractions), drift period.
    swing: Option<(u64, f64, f64, u64)>,
    /// n10: shortest / longest overlap, largest step (semitones).
    overlap: Option<(u64, u64, u8)>,
    /// n11: largest spread, up tendency (fraction), chord tolerance.
    roll: Option<(u64, f64, u64)>,
    /// B2-F01: the rubato (n01) warps before the swing (n09) moves the
    /// start (`order::HUMANIZE_TIME`; declared: the swing first).
    rubato_first: bool,
}

impl HumNew {
    fn from(p: &Params, ctx: &Ctx) -> HumNew {
        let pct = |k: &str| p.float(k) / 100.0;
        let grid = |k: &str| note_value(p.choice(k).min(art_grid_values().len() - 1), 1, ctx);
        HumNew {
            rubato: p.flag("n01").then(|| Rubato::new(p, ctx)),
            hands: p.flag("n02").then(|| {
                (
                    p.int("n02.split").clamp(0, 255) as u8,
                    p.int("n02.low"),
                    p.int("n02.high"),
                    p.ticks("n02.spread"),
                )
            }),
            arc: p.flag("n03").then(|| {
                (
                    p.int("n03.depth") as f64,
                    p.curve("n03.shape"),
                    p.ticks("n03.rest"),
                    pct("n03.vary"),
                )
            }),
            accents: p.flag("n04").then(|| {
                (
                    grid("n04.grid"),
                    p.list("n04.accents"),
                    p.int("n04.spread") as f64,
                )
            }),
            fatigue: p.flag("n05").then(|| {
                (
                    1e6 / p.float("n05.rate").max(1.0),
                    p.float("n05.loss"),
                    pct("n05.loose"),
                    p.ticks("n05.rest"),
                )
            }),
            ensemble: p.flag("n06").then(|| {
                (
                    p.int("n06.players").clamp(1, 8) as u64,
                    p.ticks("n06.spread"),
                    p.int("n06.vel") as f64,
                    p.int("n06.step"),
                )
            }),
            leap: p.flag("n07").then(|| {
                (
                    p.ticks("n07.per"),
                    p.choice("n07.dir") == 1,
                    p.ticks("n07.most"),
                )
            }),
            grid: p
                .flag("n08")
                .then(|| (grid("n08.grid"), pct("n08.on"), pct("n08.off"))),
            swing: p.flag("n09").then(|| {
                let (a, b) = (pct("n09.lo"), pct("n09.hi"));
                (grid("n09.unit"), a.min(b), a.max(b), p.ticks("n09.period"))
            }),
            overlap: p.flag("n10").then(|| {
                let (a, b) = (p.ticks("n10.short"), p.ticks("n10.long"));
                (a.min(b), a.max(b), p.int("n10.steps").clamp(0, 12) as u8)
            }),
            roll: p
                .flag("n11")
                .then(|| (p.ticks("n11.spread"), pct("n11.up"), p.ticks("n11.tol"))),
            rubato_first: p.sequence(crate::transform::order::HUMANIZE_TIME.members)[0] == "n01",
        }
    }

    fn any(&self) -> bool {
        self.rubato.is_some()
            || self.hands.is_some()
            || self.arc.is_some()
            || self.accents.is_some()
            || self.fatigue.is_some()
            || self.ensemble.is_some()
            || self.leap.is_some()
            || self.grid.is_some()
            || self.swing.is_some()
            || self.overlap.is_some()
            || self.roll.is_some()
    }

    /// Whether every switched-on function is a per-note rule (the lazy
    /// viewport preview): the others read the notes around each note.
    fn pointwise(&self) -> bool {
        self.rubato
            .as_ref()
            .is_none_or(|r| r.anchor != ANCHOR_PHRASES)
            && self.arc.is_none()
            && self.fatigue.is_none()
            && self.leap.is_none()
            && self.overlap.is_none()
            && self.roll.is_none()
    }

    /// How far (ticks) the per-note functions can move a note beyond the
    /// frozen jitter (the lazy preview's source window).
    fn reach(&self, ctx: &Ctx) -> u64 {
        let mut r: u64 = 0;
        if let Some(w) = &self.rubato {
            r = r.saturating_add(TempoTime::new(ctx).max_ticks_in(w.depth_us));
        }
        if let Some((unit, ..)) = self.swing {
            r = r.saturating_add(unit.saturating_mul(3) / 5 + 1);
        }
        if let Some((_, lo, hi, spread)) = self.hands {
            r = r
                .saturating_add(lo.unsigned_abs().max(hi.unsigned_abs()))
                .saturating_add(spread);
        }
        if let Some((_, spread, ..)) = self.ensemble {
            r = r.saturating_add(spread);
        }
        r
    }
}

/// n01 anchors (`HUM_ANCHORS`).
const ANCHOR_BARS: usize = 0;
const ANCHOR_PHRASES: usize = 1;

/// n01: a warp of performed time that never changes the order of notes —
/// a displacement of up to the depth (real time) that returns to zero at
/// every anchor, its amplitude capped so that time plus displacement stays
/// strictly increasing at the song's fastest tempo.
#[derive(Clone)]
struct Rubato {
    depth_us: f64,
    period: u64,
    anchor: usize,
    /// The drawn shape (`None` = smooth seeded noise).
    curve: Option<Curve>,
    /// The drawn shape's slope bound per unit of its segment (pinned ends
    /// included) and its jump where a Repeat curve wraps.
    slope_u: f64,
    jump: f64,
    /// Ticks per microsecond at the song's fastest tempo.
    tpus: f64,
}

impl Rubato {
    fn new(p: &Params, ctx: &Ctx) -> Rubato {
        let curve = (p.choice("n01.shape") == 1).then(|| p.curve("n01.curve"));
        let (slope_u, jump) = curve.as_ref().map_or((0.0, 0.0), rubato_slope);
        let tt = TempoTime::new(ctx);
        Rubato {
            depth_us: p.float("n01.depth") * 1000.0,
            period: p.ticks("n01.period").max(1),
            anchor: p.choice("n01.anchor"),
            curve,
            slope_u,
            jump,
            tpus: tt.max_bpm() * ctx.ppq.max(1) as f64 / 60e6,
        }
    }

    /// The drawn displacement at `u` of its segment: the curve (½ = in
    /// time) less the line through its ends, so it is 0 at both anchors.
    fn drawn(c: &Curve, u: f64) -> f64 {
        let at = |x: f64| 2.0 * c.eval(x) - 1.0;
        at(u) - ((1.0 - u) * at(0.0) + u * at(1.0))
    }

    /// The time of `t` after the warp (`phrase` = the phrase holding `t`
    /// with phrase anchors: its first start and latest end; `bar` = the bar
    /// of the last call, reused while `t` lies in it — a note's end mostly
    /// shares its start's bar).
    fn warp(
        &self,
        seed: u64,
        meter: &MeterMap,
        tt: &mut TempoTime,
        t: u64,
        phrase: Option<(u64, u64)>,
        bar: &mut Option<(u64, u64)>,
    ) -> u64 {
        use std::f64::consts::PI;
        let seg = match self.anchor {
            ANCHOR_BARS => Some(match *bar {
                Some((a, len)) if t >= a && t - a < len => (a, len),
                _ => {
                    let b = bar_bounds(meter, t);
                    *bar = Some(b);
                    b
                }
            }),
            ANCHOR_PHRASES => match phrase {
                Some((a, b)) if b > a && t >= a && t < b => Some((a, b - a)),
                _ => return t,
            },
            _ => self
                .curve
                .as_ref()
                .map(|_| (t - t % self.period, self.period)),
        };
        let salt = salt_of("n01");
        // The displacement shape in [-1, 1] and its slope bound per tick.
        let (g, slope) = match (&self.curve, seg) {
            (Some(c), Some((a, len))) => {
                let u = (t - a) as f64 / len as f64;
                (
                    Rubato::drawn(c, u).clamp(-1.0, 1.0),
                    self.slope_u / len as f64 + 2.0 * self.jump,
                )
            }
            (None, Some((a, len))) => {
                let u = (t - a) as f64 / len as f64;
                (
                    smooth_noise(seed, salt, t, self.period) * (PI * u).sin(),
                    PI / self.period as f64 + PI / len as f64,
                )
            }
            _ => (
                smooth_noise(seed, salt, t, self.period),
                PI / self.period as f64,
            ),
        };
        // Neighbouring ticks are at least 1 / tpus µs apart: a displacement
        // changing by less than that per tick keeps time increasing.
        let cap = if slope > 0.0 {
            0.9 / (slope * self.tpus.max(f64::MIN_POSITIVE))
        } else {
            f64::INFINITY
        };
        let d = self.depth_us.min(cap) * g;
        if d == 0.0 || !d.is_finite() {
            return t;
        }
        let us = tt.micros_f(t) + d;
        tt.tick_at_micros_f(us.max(0.0))
    }
}

/// The slope bound of a drawn rubato per unit of its segment: the curve's
/// steepest path (a straight segment's slope, the monotone cubic at most
/// three times its neighbouring secants) doubled (2y − 1) plus the line
/// through the ends (≤ 2), and the jump where a Repeat curve wraps.
fn rubato_slope(c: &Curve) -> (f64, f64) {
    let p = &c.points;
    let mut secant: f64 = 0.0;
    for w in p.windows(2) {
        let dx = w[1].0 - w[0].0;
        let dy = (w[1].1 - w[0].1).abs();
        secant = secant.max(if dx > 0.0 { dy / dx } else { f64::INFINITY });
    }
    let jump = match (c.outside, p.first(), p.last()) {
        (Outside::Repeat, Some(a), Some(b)) if a.0 > 0.0 || b.0 < 1.0 => 2.0 * (b.1 - a.1).abs(),
        _ => 0.0,
    };
    (6.0 * secant + 2.0, jump)
}

/// n09: `t` on a swung grid — cells of two units from the bar line, the
/// second unit's start moved to `2 · unit · swing`, the swing wandering
/// between its bounds by smooth seeded noise over the drift period (one
/// value per cell). Partial cells at a bar's end stay straight; the map
/// never changes the order of notes.
fn hum_swing(seed: u64, meter: &MeterMap, t: u64, sw: (u64, f64, f64, u64)) -> u64 {
    let (unit, lo, hi, period) = sw;
    let unit = unit.max(1);
    let cell = unit.saturating_mul(2);
    let (bar_start, bar_len) = bar_bounds(meter, t);
    let cs = bar_start + (t - bar_start) / cell * cell;
    if cs.saturating_add(cell) > bar_start.saturating_add(bar_len) {
        return t;
    }
    let a = lo + (hi - lo) * (smooth_noise(seed, salt_of("n09"), cs, period) + 1.0) / 2.0;
    let (o, u) = ((t - cs) as f64, unit as f64);
    let o2 = if o < u {
        o * 2.0 * a
    } else {
        2.0 * u * a + (o - u) * (2.0 - 2.0 * a)
    };
    cs.saturating_add((o2.round().max(0.0) as u64).min(cell))
}

/// n08: the jitter amount at `t` — `on` on a line of the grid (restarting
/// at bar lines), `off` half a grid step away, in between linearly.
fn hum_grid_amount(meter: &MeterMap, t: u64, g: (u64, f64, f64)) -> f64 {
    let (step, on, off) = g;
    let step = step.max(1);
    let (bar_start, bar_len) = bar_bounds(meter, t);
    let r = (t - bar_start) % step;
    let to_bar = bar_start.saturating_add(bar_len).saturating_sub(t);
    let dist = r.min(step - r).min(to_bar);
    let d = (dist as f64 / (step as f64 / 2.0)).min(1.0);
    on + (off - on) * d
}

/// The entry of `sorted` (ascending by pitch) nearest to `p` (ties: the
/// lower pitch).
fn nearest_pitch(sorted: &[(u8, u32)], p: u8) -> Option<(u8, u32)> {
    let k = sorted.partition_point(|e| e.0 < p);
    match (k.checked_sub(1).map(|j| sorted[j]), sorted.get(k)) {
        (Some(b), Some(&a)) => Some(if p - b.0 <= a.0 - p { b } else { a }),
        (b, a) => b.or(a.copied()),
    }
}

/// Bounds of the onsets (notes with one start) of a time-ordered index
/// list: each onset's first position, then `idx.len()`.
fn onset_bounds(notes: &[TNote], idx: &[usize]) -> Vec<usize> {
    let mut b = Vec::new();
    for (k, &i) in idx.iter().enumerate() {
        if k == 0 || notes[idx[k - 1]].start != notes[i].start {
            b.push(k);
        }
    }
    b.push(idx.len());
    b
}

/// The unmuted notes of an onset as (pitch, index), sorted.
fn onset_pitches(notes: &[TNote], g: &[usize], into: &mut Vec<(u8, u32)>) {
    into.clear();
    into.extend(
        g.iter()
            .filter(|&&i| notes[i].flags & FLAG_MUTED == 0)
            .map(|&i| (notes[i].pitch, i as u32)),
    );
    into.sort_unstable();
}

/// What the whole-source functions found around each note (vectors only
/// for the functions switched on).
#[derive(Default)]
struct HumCx {
    /// n01 phrase anchors: the first start and latest end of the phrase.
    phrase: Vec<(u64, u64)>,
    /// n03: the phrase arc's velocity change.
    vel: Vec<i32>,
    /// n05: repeats in a row on the key (the fatigue level).
    tired: Vec<u32>,
    /// n07 + n11: tick offsets (leap error, chord roll).
    dt: Vec<i64>,
    /// n10: the note of the next onset nearest in pitch (`u32::MAX` none).
    next: Vec<u32>,
}

/// One note's view of [`HumCx`].
#[derive(Clone, Copy, Default)]
struct NoteCx {
    phrase: Option<(u64, u64)>,
    vel: i64,
    tired: u32,
    dt: i64,
}

impl HumCx {
    fn at(&self, i: usize) -> NoteCx {
        NoteCx {
            phrase: self.phrase.get(i).copied(),
            vel: self.vel.get(i).map_or(0, |&v| i64::from(v)),
            tired: self.tired.get(i).copied().unwrap_or(0),
            dt: self.dt.get(i).copied().unwrap_or(0),
        }
    }
}

/// A note after the pipeline (before n10's end and n06's copies).
#[derive(Clone, Copy)]
struct HumPlan {
    start: i128,
    end: i128,
    vel: i64,
}

#[derive(Clone)]
struct Humanize {
    tick: (i64, i64),
    gate: (i64, i64),
    gate_pct: bool,
    vel: (i64, i64),
    seed: u64,
    step: Option<(i64, i64)>,
    dist: Distribution,
    sigma: f64,
    tri_mode: Option<f64>,
    walk: Option<(u64, f64)>,
    chord_tol: Option<u64>,
    vel_link: Option<f64>,
    envelope: Option<Curve>,
    sub_bias: Vec<i64>,
    sub_scale: Vec<i64>,
    fingerprint: Option<u64>,
    stats: super::super::SourceStats,
    subs: Vec<Vec<(u32, u8)>>,
    /// V09-S36: the functions declared by id.
    new: HumNew,
    ctx: Ctx,
    /// With Control Sweeps (E0422): what each level's functions are
    /// built from.
    swept: Option<std::sync::Arc<Rebuild>>,
}

impl Humanize {
    /// Each note's settings under Control Sweeps (E0422; every Humanize
    /// target is a function's control): `None` without sweeps.
    fn levels(&self, src: &Source) -> Option<PerLevel<Humanize>> {
        let r = self.swept.as_ref()?;
        Some(r.levels(&self.ctx, src, |p, b| Humanize {
            new: HumNew::from(p, b.ctx),
            ..self.clone()
        }))
    }

    /// A value in [-1, 1] for property `salt` of note `n`.
    fn draw(&self, n: &TNote, s: u64) -> f64 {
        let key = match (self.fingerprint, self.chord_tol) {
            (Some(period), _) => hash(&[n.start % period, u64::from(n.pitch % 12), 0xF1]),
            (None, Some(tol)) if s == salt::TICK => {
                hash(&[n.start / (tol + 1), u64::from(n.track), 0xC4])
            }
            _ => note_key(n),
        };
        let mut r = Rng::new(hash(&[self.seed, s, key]));
        let white = if let Some(m) = self.tri_mode {
            // Triangular on [-1, 1] with its peak at m.
            let c = (m + 1.0) / 2.0;
            let u = r.unit();
            let x = if u < c {
                (u * c).sqrt()
            } else {
                1.0 - ((1.0 - u) * (1.0 - c)).sqrt()
            };
            2.0 * x - 1.0
        } else if self.dist == Distribution::Gaussian {
            (r.normal() * self.sigma).clamp(-1.0, 1.0)
        } else {
            r.bipolar(Distribution::Uniform)
        };
        match self.walk {
            Some((len, mix)) => {
                let w = smooth_noise(self.seed, s, n.start, len);
                mix * w + (1.0 - mix) * white
            }
            None => white,
        }
    }

    fn sub_index(&self, n: &TNote) -> usize {
        self.subs
            .get(usize::from(n.track))
            .and_then(|t| t.iter().position(|s| s.0 == n.sub))
            .unwrap_or(0)
    }

    /// The frozen (v0.8) jitter of `n`: the tick offset (with the
    /// subchannel bias), the gate change (ticks or %) and the velocity
    /// change. `density_scale` scales every amount, `tick_mult` (V09-S36
    /// n05) the timing amount; at 1 the arithmetic is v0.8's.
    #[inline(always)]
    fn draws(&self, n: &TNote, density_scale: f64, tick_mult: f64) -> (i64, f64, i64) {
        let mut scale = density_scale;
        if let Some(c) = &self.envelope {
            scale *= c.eval(pos(n, &self.stats));
        }
        let si = self.sub_index(n);
        if !self.sub_scale.is_empty() {
            scale *= self.sub_scale[si % self.sub_scale.len()] as f64 / 100.0;
        }
        let bias = if self.sub_bias.is_empty() {
            0
        } else {
            self.sub_bias[si % self.sub_bias.len()]
        };
        let value = |range: (i64, i64), u: f64, extra: f64| -> f64 {
            let (lo, hi) = (range.0.min(range.1) as f64, range.0.max(range.1) as f64);
            let c = (lo + hi) / 2.0;
            let h = (hi - lo) / 2.0;
            c + u * h * scale * extra
        };
        let quant = |v: f64, step: i64| -> i64 {
            if step <= 1 {
                v.round() as i64
            } else {
                ((v / step as f64).round() as i64) * step
            }
        };
        let tick_extra = match self.vel_link {
            Some(a) => (1.0 + a * (f64::from(n.vel) / 127.0 * 2.0 - 1.0)).max(0.0),
            None => 1.0,
        } * tick_mult;
        let (tstep, vstep) = self.step.unwrap_or((1, 1));
        let dt = quant(
            value(self.tick, self.draw(n, salt::TICK), tick_extra),
            tstep,
        ) + bias;
        let dg = value(self.gate, self.draw(n, salt::GATE), 1.0);
        let dv = quant(value(self.vel, self.draw(n, salt::VELOCITY), 1.0), vstep);
        (dt, dg, dv)
    }

    /// The length after the frozen gate change `dg`.
    #[inline(always)]
    fn gated(&self, dur: u64, dg: f64) -> i128 {
        if self.gate_pct {
            (dur as f64 * (1.0 + dg / 100.0)).round() as i128
        } else {
            i128::from(dur) + dg.round() as i128
        }
    }

    /// The v0.8 rule.
    fn apply(&self, n: &TNote, density_scale: f64, out: &mut Output) {
        let (dt, dg, dv) = self.draws(n, density_scale, 1.0);
        out.emit(
            i128::from(n.start) + i128::from(dt),
            self.gated(n.dur, dg),
            i64::from(n.pitch),
            i64::from(n.vel) + dv,
            n,
        );
    }

    /// The v0.9 pipeline of one note (docs/TRANSFORM_ENGINE.md
    /// "Humanize"): the frozen jitter with n08 / n05 setting its amounts,
    /// then the time maps n09 and n01 on the performed note, the offsets
    /// (n02, and n07 / n11 from `cx`), the velocity shapes (n03 from `cx`,
    /// n04, n05).
    fn plan(&self, n: &TNote, density: f64, cx: &NoteCx, tt: &mut TempoTime) -> HumPlan {
        let nw = &self.new;
        let meter = &self.ctx.meter;
        let amount = nw.grid.map_or(1.0, |g| hum_grid_amount(meter, n.start, g));
        let tired = f64::from(cx.tired);
        let (loss, tick_mult) = match nw.fatigue {
            Some((_, loss, gain, _)) => (loss * tired, 1.0 + gain * tired),
            None => (0.0, 1.0),
        };
        let (dt, dg, dv) = self.draws(n, density * amount, tick_mult);
        let mut start = i128::from(n.start) + i128::from(dt);
        let dur = self.gated(n.dur, dg);
        let mut end = start + dur;
        if nw.rubato_first
            && (nw.swing.is_some() || nw.rubato.is_some())
            && let Ok(s) = u64::try_from(start)
        {
            // B2-F01, the user's order: the rubato warps both ends, then the
            // swing moves the warped start (the warped length kept).
            let (s2, e2) = match &nw.rubato {
                Some(r) => {
                    let mut bar = None;
                    let s2 = r.warp(self.seed, meter, tt, s, cx.phrase, &mut bar);
                    let e2 = match u64::try_from(end) {
                        Ok(e) if dur > 0 => {
                            i128::from(r.warp(self.seed, meter, tt, e, cx.phrase, &mut bar))
                        }
                        _ => i128::from(s2) + dur,
                    };
                    (s2, e2)
                }
                None => (s, end),
            };
            let s3 = nw
                .swing
                .map_or(s2, |sw| hum_swing(self.seed, meter, s2, sw));
            start = i128::from(s3);
            end = e2 + i128::from(s3) - i128::from(s2);
        } else if (nw.swing.is_some() || nw.rubato.is_some())
            && let Ok(s) = u64::try_from(start)
        {
            // Swing moves the start (the length kept), the rubato warps
            // both ends; both maps keep the order of notes.
            let s2 = nw.swing.map_or(s, |sw| hum_swing(self.seed, meter, s, sw));
            let e2 = end + i128::from(s2) - i128::from(s);
            let (s3, e3) = match &nw.rubato {
                Some(r) => {
                    let mut bar = None;
                    let s3 = r.warp(self.seed, meter, tt, s2, cx.phrase, &mut bar);
                    let e3 = match u64::try_from(e2) {
                        Ok(e) if dur > 0 => {
                            i128::from(r.warp(self.seed, meter, tt, e, cx.phrase, &mut bar))
                        }
                        _ => i128::from(s3) + dur,
                    };
                    (s3, e3)
                }
                None => (s2, e2),
            };
            start = i128::from(s3);
            end = e3;
        }
        let mut off = cx.dt;
        if let Some((split, lo, hi, spread)) = nw.hands {
            let high = n.pitch >= split;
            let u = stream(
                self.seed,
                salt_of("n02"),
                &[u64::from(n.track), u64::from(high), n.start],
            )
            .unit()
                * 2.0
                - 1.0;
            off += if high { hi } else { lo } + (spread as f64 * u).round() as i64;
        }
        start += i128::from(off);
        end += i128::from(off);
        let mut vel = i64::from(n.vel) + dv + cx.vel - loss.round() as i64;
        if let Some((g, acc, spread)) = &nw.accents {
            if !acc.is_empty() {
                // The grid position in the bar (the last may be partial).
                let (bar_start, bar_len) = bar_bounds(meter, n.start);
                let g = (*g).max(1);
                let slot = ((n.start - bar_start) / g).min(bar_len.div_ceil(g).max(1) - 1);
                vel += acc[(slot % acc.len() as u64) as usize];
            }
            let u = stream(self.seed, salt_of("n04"), &[note_key(n)]).unit() * 2.0 - 1.0;
            vel += (spread * u).round() as i64;
        }
        HumPlan { start, end, vel }
    }

    /// n06: the subchannel `steps` table positions after the note's
    /// (wrapping) and its MIDI channel.
    fn sub_step(&self, n: &TNote, steps: i64) -> (u32, u8) {
        match self.subs.get(usize::from(n.track)) {
            Some(t) if !t.is_empty() && steps != 0 => {
                let i = t.iter().position(|s| s.0 == n.sub).unwrap_or(0) as i64;
                t[(i + steps).rem_euclid(t.len() as i64) as usize]
            }
            _ => (n.sub, n.ch),
        }
    }

    /// Emits note `n` through the v0.9 pipeline: [`Humanize::plan`], n10's
    /// end (`next` = the planned start and pitch of the next onset's
    /// nearest note), n06's copies.
    fn emit9(
        &self,
        n: &TNote,
        density: f64,
        cx: &NoteCx,
        next: Option<(i128, u8)>,
        tt: &mut TempoTime,
        out: &mut Output,
    ) {
        let mut p = self.plan(n, density, cx, tt);
        if let (Some((short, long, steps)), Some((ns, np))) = (self.new.overlap, next) {
            let step = n.pitch.abs_diff(np);
            if (1..=steps).contains(&step) {
                let u = stream(self.seed, salt_of("n10"), &[note_key(n)]).unit();
                let o = short as f64 + (long - short) as f64 * u;
                let end = ns + o.round() as i128;
                if end > p.start {
                    p.end = end;
                }
            }
        }
        let dur = p.end - p.start;
        // V09-0143: the player (0 = the note itself, n06's players 1 …).
        out.set_dim(0);
        out.emit(p.start, dur, i64::from(n.pitch), p.vel, n);
        if let Some((players, spread, vspread, step)) = self.new.ensemble {
            let key = note_key(n);
            for j in 1..=players {
                out.set_dim(u32::try_from(j).unwrap_or(u32::MAX));
                let mut r = stream(self.seed, salt_of("n06"), &[key, j]);
                let dt = (spread as f64 * (r.unit() * 2.0 - 1.0)).round() as i128;
                let dv = (vspread * (r.unit() * 2.0 - 1.0)).round() as i64;
                let mut tpl = *n;
                (tpl.sub, tpl.ch) = self.sub_step(n, step.saturating_mul(j as i64));
                if !out.emit(p.start + dt, dur, i64::from(n.pitch), p.vel + dv, &tpl) {
                    return;
                }
            }
        }
    }

    /// What the whole-source functions read around each note (`lv`: each
    /// note's settings under Control Sweeps, E0422).
    fn context(&self, src: &Source, lv: Option<&PerLevel<Humanize>>) -> HumCx {
        let notes = &src.notes;
        let count = notes.len();
        let nw = &self.new;
        let nw_at = |i: usize| lv.map_or(nw, |l| &l.at(i).new);
        let tracks = per_track(src);
        let mut cx = HumCx::default();
        if nw
            .rubato
            .as_ref()
            .is_some_and(|r| r.anchor == ANCHOR_PHRASES)
        {
            // Phrase ends: rests of a beat or more (phrase_detect).
            cx.phrase = vec![(0, 0); count];
            for idx in &tracks {
                let ph = Phrases::of(notes, idx, self.ctx.ppq.max(1) - 1, None);
                for g in ph.iter(idx) {
                    let a = notes[g[0]].start;
                    let b = g.iter().map(|&i| notes[i].end()).max().unwrap_or(a);
                    for &i in g {
                        cx.phrase[i] = (a, b);
                    }
                }
            }
        }
        if let Some((_, shape, rest, vary)) = &nw.arc {
            cx.vel = vec![0; count];
            for (t, idx) in tracks.iter().enumerate() {
                for g in Phrases::of(notes, idx, *rest, None).iter(idx) {
                    let a = notes[g[0]].start;
                    let z = notes[g[g.len() - 1]].start;
                    let r = stream(self.seed, salt_of("n03"), &[t as u64, a]).unit() * 2.0 - 1.0;
                    for &i in g {
                        let depth = nw_at(i).arc.as_ref().map_or(0.0, |x| x.0);
                        let d = (depth * (1.0 + vary * r)).max(0.0);
                        let u = if z > a {
                            (notes[i].start - a) as f64 / (z - a) as f64
                        } else {
                            0.5
                        };
                        cx.vel[i] = (d * (2.0 * shape.eval(u) - 1.0)).round() as i32;
                    }
                }
            }
        }
        if let Some((calm_us, _, _, rest)) = nw.fatigue {
            cx.tired = vec![0; count];
            let mut tt = TempoTime::new(&self.ctx);
            // Per key: the last sounding start (µs), the latest end so far,
            // the level, seen.
            let mut keys = vec![(0.0f64, 0u64, 0u32, false); 256];
            for idx in &tracks {
                keys.fill((0.0, 0, 0, false));
                for &i in idx {
                    let n = &notes[i];
                    let us = tt.micros_f(n.start);
                    let k = &mut keys[usize::from(n.pitch)];
                    let recovered = !k.3 || n.start.saturating_sub(k.1) > rest;
                    let level = if recovered {
                        0
                    } else if us - k.0 < calm_us {
                        k.2.saturating_add(1)
                    } else {
                        k.2
                    };
                    cx.tired[i] = level;
                    if n.flags & FLAG_MUTED == 0 {
                        let end = if recovered { n.end() } else { k.1.max(n.end()) };
                        *k = (us, end, level, true);
                    }
                }
            }
        }
        if nw.leap.is_some() || nw.roll.is_some() {
            cx.dt = vec![0; count];
        }
        if let Some((_, both, most)) = nw.leap {
            let (mut prev, mut cur) = (Vec::new(), Vec::new());
            for idx in &tracks {
                prev.clear();
                let b = onset_bounds(notes, idx);
                for w in b.windows(2) {
                    let g = &idx[w[0]..w[1]];
                    for &i in g {
                        let n = &notes[i];
                        if let Some((q, _)) = nearest_pitch(&prev, n.pitch) {
                            let per = nw_at(i).leap.map_or(0, |x| x.0);
                            let leap = u128::from(n.pitch.abs_diff(q));
                            let err = ((u128::from(per) * leap + 6) / 12).min(u128::from(most));
                            let late = !both
                                || stream(self.seed, salt_of("n07"), &[note_key(n)]).unit() < 0.5;
                            let err = err as i64;
                            cx.dt[i] += if late { err } else { -err };
                        }
                    }
                    onset_pitches(notes, g, &mut cur);
                    if !cur.is_empty() {
                        std::mem::swap(&mut prev, &mut cur);
                    }
                }
            }
        }
        if let Some((_, up, tol)) = nw.roll {
            let mut order: Vec<usize> = Vec::new();
            for (t, idx) in tracks.iter().enumerate() {
                for g in ChordGroups::of(notes, idx, tol).iter() {
                    if g.len() < 2 {
                        continue;
                    }
                    order.clear();
                    order.extend_from_slice(g);
                    order.sort_by_key(|&i| (notes[i].pitch, notes[i].sub, notes[i].start, i));
                    let mut r = stream(self.seed, salt_of("n11"), &[t as u64, notes[g[0]].start]);
                    // The chord's share of the spread; each voice's own
                    // largest spread (one value without a sweep).
                    let share = r.unit();
                    let upward = r.unit() < up;
                    // Uneven gaps (½ … 1½ of the mean) from the first voice
                    // to the last.
                    let gaps = r.clone();
                    let total: f64 = (1..g.len()).map(|_| 0.5 + r.unit()).sum();
                    let mut r = gaps;
                    let mut at = 0.0;
                    let c = order.len();
                    for k in 0..c {
                        if k > 0 {
                            at += 0.5 + r.unit();
                        }
                        let voice = if upward { order[k] } else { order[c - 1 - k] };
                        let s = nw_at(voice).roll.map_or(0, |x| x.0) as f64 * share;
                        cx.dt[voice] += (s * at / total).round() as i64;
                    }
                }
            }
        }
        if nw.overlap.is_some() {
            cx.next = vec![u32::MAX; count];
            let (mut next, mut cur) = (Vec::new(), Vec::new());
            for idx in &tracks {
                next.clear();
                let b = onset_bounds(notes, idx);
                for w in b.windows(2).rev() {
                    let g = &idx[w[0]..w[1]];
                    for &i in g {
                        if let Some((_, j)) = nearest_pitch(&next, notes[i].pitch) {
                            cx.next[i] = j;
                        }
                    }
                    onset_pitches(notes, g, &mut cur);
                    if !cur.is_empty() {
                        std::mem::swap(&mut next, &mut cur);
                    }
                }
            }
        }
        cx
    }

    /// The whole-source v0.9 run.
    fn run9(
        &self,
        src: &Source,
        density: Option<(u64, f64)>,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let notes = &src.notes;
        let lv = self.levels(src);
        // Note i's settings (its level's with sweeps).
        let h = |i: usize| lv.as_ref().map_or(self, |l| l.at(i));
        let cx = self.context(src, lv.as_ref());
        let scales: Vec<f64> = match density {
            Some((window, amount)) => {
                let d = local_density(notes, window);
                let mean = d.iter().map(|&c| c as f64).sum::<f64>() / d.len().max(1) as f64;
                d.iter()
                    .map(|&c| {
                        let rel = if mean > 0.0 { c as f64 / mean } else { 1.0 };
                        (1.0 + amount * (rel - 1.0)).clamp(0.0, 4.0)
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        let scale = |i: usize| scales.get(i).copied().unwrap_or(1.0);
        let mut tt = TempoTime::new(&self.ctx);
        // n10 reads where the next note was planned to start.
        let starts: Vec<i128> = if self.new.overlap.is_some() {
            (0..notes.len())
                .map(|i| h(i).plan(&notes[i], scale(i), &cx.at(i), &mut tt).start)
                .collect()
        } else {
            Vec::new()
        };
        for (i, n) in notes.iter().enumerate() {
            let next = cx
                .next
                .get(i)
                .filter(|&&j| j != u32::MAX)
                .map(|&j| (starts[j as usize], notes[j as usize].pitch));
            h(i).emit9(n, scale(i), &cx.at(i), next, &mut tt, out);
            out.check()?;
        }
        Ok(())
    }
}

fn humanize_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let h = Humanize {
        tick: (p.int("tick.min"), p.int("tick.max")),
        gate: (p.int("gate.min"), p.int("gate.max")),
        gate_pct: p.choice("gate.unit") == 1,
        vel: (p.int("vel.min"), p.int("vel.max")),
        seed: p.seed("seed"),
        step: p.adv(1).then(|| (p.int("a1.step"), p.int("a1.vstep"))),
        dist: if p.adv(2) {
            Distribution::Gaussian
        } else {
            Distribution::Uniform
        },
        sigma: p.float("a2.sigma"),
        tri_mode: p.adv(3).then(|| p.float("a3.mode")),
        walk: p.adv(4).then(|| (p.ticks("a4.len"), p.float("a4.mix"))),
        chord_tol: p.adv(5).then(|| p.ticks("a5.tol")),
        vel_link: p.adv(7).then(|| p.float("a7.amount")),
        envelope: p.adv(8).then(|| p.curve("a8.curve")),
        sub_bias: if p.adv(9) {
            p.list("a9.bias")
        } else {
            Vec::new()
        },
        sub_scale: if p.adv(9) {
            p.list("a9.scale")
        } else {
            Vec::new()
        },
        fingerprint: p.adv(10).then(|| p.ticks("a10.period").max(1)),
        stats: b.stats.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        new: HumNew::from(p, b.ctx),
        ctx: b.ctx.clone(),
        swept: None,
    };
    let mut h = h;
    h.swept = Rebuild::of(p, b);
    let swept = h.swept.is_some();
    let reach = |r: (i64, i64)| r.0.unsigned_abs().max(r.1.unsigned_abs());
    let bias = h
        .sub_bias
        .iter()
        .map(|v| v.unsigned_abs())
        .max()
        .unwrap_or(0);
    let scale_max = 4 * h.sub_scale.iter().copied().max().unwrap_or(100).max(100) as u64 / 100;
    let ahead = reach(h.tick).saturating_mul(scale_max).saturating_add(bias);
    let gate = if h.gate_pct {
        b.stats.max_dur.saturating_mul(reach(h.gate)) / 100
    } else {
        reach(h.gate)
    };
    let behind = ahead.saturating_add(gate.saturating_mul(scale_max));
    let density = p
        .adv(6)
        .then(|| (p.ticks("a6.window").max(1), p.float("a6.amount")));
    if h.new.any() || swept {
        // V09-S36: the v0.9 pipeline (n06 adds a copy per player); with
        // Control Sweeps always the whole-source run (E0422).
        let growth = match h.new.ensemble {
            Some((players, ..)) => Growth::Expand(players + 1),
            None => Growth::Map,
        };
        if h.new.pointwise() && density.is_none() && !swept {
            // The jitter grows by n05 / n08's largest amount (n08 ≤ 2×), the
            // rubato, swing, hands and copies move notes further.
            let more = h
                .new
                .grid
                .map_or(1, |g| g.1.max(g.2).ceil().max(1.0) as u64);
            let extra = h.new.reach(b.ctx);
            let ahead = ahead.saturating_mul(more).saturating_add(extra);
            let behind = behind
                .saturating_mul(more)
                .saturating_add(extra.saturating_mul(2));
            return boxed(Pointwise {
                f: move |n: &TNote, out: &mut Output| {
                    let mut tt = TempoTime::new(&h.ctx);
                    h.emit9(n, 1.0, &NoteCx::default(), None, &mut tt, out);
                },
                ahead,
                behind,
                growth,
            });
        }
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| h.run9(src, density, out),
            growth,
            estimate: None,
        });
    }
    if let Some((window, amount)) = density {
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                let d = local_density(&src.notes, window);
                let mean = d.iter().map(|&c| c as f64).sum::<f64>() / d.len().max(1) as f64;
                for (n, &c) in src.notes.iter().zip(&d) {
                    let rel = if mean > 0.0 { c as f64 / mean } else { 1.0 };
                    let s = (1.0 + amount * (rel - 1.0)).clamp(0.0, 4.0);
                    h.apply(n, s, out);
                    out.check()?;
                }
                Ok(())
            },
            growth: Growth::Map,
            estimate: None,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| h.apply(n, 1.0, out),
        ahead,
        behind,
        growth: Growth::Map,
    })
}

/// For each note (time-sorted), the number of notes starting within ±window/2.
pub fn local_density(notes: &[TNote], window: u64) -> Vec<u32> {
    let half = window / 2;
    let mut out = Vec::with_capacity(notes.len());
    let (mut lo, mut hi) = (0usize, 0usize);
    for n in notes {
        let a = n.start.saturating_sub(half);
        let b = n.start.saturating_add(half);
        while lo < notes.len() && notes[lo].start < a {
            lo += 1;
        }
        while hi < notes.len() && notes[hi].start <= b {
            hi += 1;
        }
        out.push((hi - lo) as u32);
    }
    out
}

// ================================================================ Transpose

pub const TRANSPOSE: ToolDef = ToolDef {
    id: "transpose",
    spec: transpose_spec,
    build: transpose_build,
};

pub const TRANSPOSE_SCOPES: &[&str] = &[
    "Selected notes",
    "Active track",
    "Active track and its subtracks",
    "Whole project",
];

/// n02: what the intervals are measured from.
const TR_REFERENCES: &[&str] = &["First note", "Lowest note", "Pitch"];
/// n02: where the scaled pitches land.
const TR_SNAPS: &[&str] = &["Chromatic", "Key and scale"];
/// n03: the channel each bent note plays on.
const TR_CHANNELS: &[&str] = &["Keep", "Allocate a free channel per note"];
/// n04: the reorderings.
const TR_PERMUTATIONS: &[&str] = &["Reverse", "Rotate", "Sort up", "Sort down", "Outside in"];
/// n04: what one permutation spans.
const TR_PERM_SCOPES: &[&str] = &["Selection", "Each bar", "Each phrase"];
/// n05: when the next root of the list starts.
const TR_ADVANCE: &[&str] = &["Chord", "Bar", "Beat"];
/// n05 / n07: what a shift counts.
const TR_UNITS: &[&str] = &["Semitones", "Scale degrees"];
/// n06: how each note picks its octave.
const TR_SPREADS: &[&str] = &["By onset", "Alternating", "By pitch class"];

const REF_FIRST: usize = 0;
const REF_LOWEST: usize = 1;
const PERM_REVERSE: usize = 0;
const PERM_ROTATE: usize = 1;
const PERM_SORT_UP: usize = 2;
const PERM_SORT_DOWN: usize = 3;
const SCOPE_BAR: usize = 1;
const SCOPE_PHRASE: usize = 2;
const ADV_CHORD: usize = 0;
const ADV_BEAT: usize = 2;
const SPREAD_ONSET: usize = 0;
const SPREAD_ALTERNATING: usize = 1;
const SPREAD_PITCH_CLASS: usize = 2;
/// n08: the pitch of a shifted frequency of 0 Hz or below (outside every
/// key range: the boundary policy decides).
const NO_FREQUENCY: i64 = -1024;
/// n03: MIDI channel 10 (index 9), which General MIDI keeps for drums.
const DRUM_CHANNEL: u8 = 9;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const TRANSPOSE_HELP: &[(&str, &str)] = &[
    (
        "scope",
        "Which notes move: the selected notes, the active track, the active track with its subtracks, or the whole project.",
    ),
    ("semis", "How many semitones the notes move (+ up, − down)."),
    (
        "octaves",
        "How many octaves the notes move, on top of the semitones.",
    ),
    ("key", "The key used by the scale-degree functions."),
    ("scale", "The scale used by the scale-degree functions."),
    (
        "a1",
        "Moves the notes by scale degrees of the key and scale, so they stay in key.",
    ),
    (
        "a1.deg",
        "How many scale degrees the notes move (+ up, − down).",
    ),
    (
        "a2",
        "Moves the notes by scale degrees but keeps their accidentals: notes outside the scale stay the same distance from it.",
    ),
    ("a2.deg", "How many scale degrees the notes move."),
    ("a3", "Mirrors the notes upside down around an axis key."),
    ("a3.axis", "The key the notes are mirrored around."),
    (
        "a4",
        "Folds notes into a range by octaves, so none goes below the lowest or above the highest key.",
    ),
    ("a4.lo", "The lowest key; lower notes move up by octaves."),
    (
        "a4.hi",
        "The highest key; higher notes move down by octaves.",
    ),
    ("a5", "Moves each subchannel's notes by its own interval."),
    (
        "a5.table",
        "The semitones for each subchannel in turn (the first entry = the track's first subchannel); the list repeats.",
    ),
    (
        "a6",
        "Moves the notes by an amount that follows a curve across the selection.",
    ),
    (
        "a6.curve",
        "The share of Depth from the selection's start (left) to its end (right).",
    ),
    (
        "a6.depth",
        "The move, in semitones, where the curve is at 1.",
    ),
    (
        "a7",
        "Moves each chord's root by scale degrees and keeps the chord's own shape (major stays major).",
    ),
    ("a7.deg", "How many scale degrees each chord's root moves."),
    (
        "a8",
        "Moves the notes by whole octaves so they fit inside a range.",
    ),
    ("a8.lo", "The lowest key of the range."),
    ("a8.hi", "The highest key of the range."),
    (
        "a9",
        "Moves each onset by the next interval of a list, in turn.",
    ),
    (
        "a9.seq",
        "The semitones for each onset in turn; the list repeats.",
    ),
    (
        "a10",
        "Replaces pitch classes by others, in pairs (from, to).",
    ),
    (
        "a10.map",
        "Pairs of pitch classes, 0 = C … 11 = B (1 2 6 7 turns C♯ into D and F♯ into G).",
    ),
    (
        "n01",
        "Maps the notes from one key and scale onto another, degree by degree (major to minor, say).",
    ),
    ("n01.from_key", "The key the notes are in."),
    ("n01.from_scale", "The scale the notes are in."),
    ("n01.to_key", "The key the notes are mapped to."),
    ("n01.to_scale", "The scale the notes are mapped to."),
    (
        "n01.offset",
        "Moves the mapped notes by this many degrees of the target scale.",
    ),
    (
        "n02",
        "Scales the intervals from a reference: ×2 doubles every distance, −1 turns the melody upside down.",
    ),
    (
        "n02.factor",
        "The factor every interval from the reference is multiplied by.",
    ),
    (
        "n02.ref",
        "What the intervals are measured from: the first note, the lowest note, or the pitch below.",
    ),
    ("n02.pitch", "For Pitch: the reference key."),
    (
        "n02.snap",
        "Where the scaled pitches land: on any semitone, or on the key and scale below.",
    ),
    ("n02.key", "For Key and Scale: the key."),
    ("n02.scale", "For Key and Scale: the scale."),
    ("n03", "Tunes the notes by cents with pitch bend events."),
    (
        "n03.cents",
        "How many cents the notes are bent (+ sharp, − flat).",
    ),
    (
        "n03.range",
        "The synthesizer's pitch bend range, in semitones, used to compute the bend value (2 for most General MIDI players).",
    ),
    (
        "n03.channels",
        "Keep leaves each note on its channel (the bend then affects every note sounding there); Allocate moves each bent note to a free channel of its own.",
    ),
    (
        "n04",
        "Reorders the pitches of the notes while their rhythm stays.",
    ),
    (
        "n04.perm",
        "How the pitches are reordered: reversed, rotated, sorted up, sorted down, or outside in.",
    ),
    (
        "n04.rotate",
        "For Rotate: how many places the pitches shift.",
    ),
    (
        "n04.scope",
        "What one reordering spans: the whole selection, each bar, or each phrase.",
    ),
    (
        "n05",
        "Transposes successive parts by a list of roots, like a chord progression.",
    ),
    (
        "n05.roots",
        "The transposition of each part in turn; the list repeats.",
    ),
    (
        "n05.per",
        "When the next root is used: each chord, each bar, or each beat.",
    ),
    (
        "n05.unit",
        "Whether the roots count semitones or scale degrees.",
    ),
    ("n05.key", "For Scale Degrees: the key."),
    ("n05.scale", "For Scale Degrees: the scale."),
    (
        "n06",
        "Spreads the notes over several octaves of a range, keeping their pitch classes.",
    ),
    ("n06.lo", "The lowest key of the spread."),
    ("n06.hi", "The highest key of the spread."),
    (
        "n06.mode",
        "By Onset steps up an octave per onset; Alternating jumps between low and high octaves; By Pitch Class places each pitch class in its own octave.",
    ),
    (
        "n07",
        "Moves chords to a new root with the smoothest voice leading, each voice moving as little as it can.",
    ),
    ("n07.shift", "How far the root moves."),
    (
        "n07.unit",
        "Whether the shift counts semitones or scale degrees.",
    ),
    (
        "n07.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    ("n07.key", "For Scale Degrees: the key."),
    ("n07.scale", "For Scale Degrees: the scale."),
    (
        "n08",
        "Shifts every note's frequency by a fixed number of hertz, which bends the harmony (a frequency shifter).",
    ),
    (
        "n08.hz",
        "The frequency shift (+ higher, − lower); the result is rounded to the nearest key.",
    ),
    (
        "n08.a4",
        "The tuning of A4 used to turn keys into frequencies.",
    ),
    (
        "n09",
        "Pulls every note toward the nearest overtone of a fundamental.",
    ),
    (
        "n09.fund",
        "The fundamental whose overtones the notes snap to.",
    ),
    ("n09.partials", "The highest overtone used."),
    (
        "n09.strength",
        "How far each note moves toward its overtone (100 % = onto it).",
    ),
    (
        "n10",
        "Makes the notes rise (or fall) endlessly inside a band, as octave layers fading at the edges (a Shepard tone).",
    ),
    (
        "n10.rise",
        "How many semitones the notes rise per bar (negative = fall).",
    ),
    ("n10.lo", "The band's lowest key."),
    (
        "n10.hi",
        "The band's highest key (the band is whole octaves from Band Low).",
    ),
    ("n10.layers", "How many octave layers each note plays."),
    (
        "n10.fade",
        "How many semitones from the band's edges the layers fade out.",
    ),
    (
        "n11",
        "Makes notes moved up softer and notes moved down louder (or the reverse), to keep the loudness even.",
    ),
    (
        "n11.per",
        "The velocity taken away per octave a note moves up (and added per octave down); negative reverses it.",
    ),
    ("n11.floor", "No velocity falls below this."),
    ("n11.ceil", "No velocity rises above this."),
];

fn transpose_spec() -> ToolSpec {
    let mut p = vec![
        choice("scope", "Apply to", TRANSPOSE_SCOPES, 0),
        int("semis", "Semitones", -127, 127, 0).keyed(),
        int("octaves", "Octaves", -10, 10, 0).keyed_octaves(),
        choice("key", "Key", &KEY_NAMES, 0).adv(1),
        choice("scale", "Scale", scale_names(), 0).adv(1),
        adv_switch(1, "Scale-degree transpose"),
        int("a1.deg", "Scale degrees", -70, 70, 2).adv(1),
        adv_switch(2, "Diatonic transpose (keep accidentals)"),
        int("a2.deg", "Scale degrees", -70, 70, 2).adv(2),
        adv_switch(3, "Pitch-axis inversion"),
        pitch("a3.axis", "Axis", 60).adv(3),
        adv_switch(4, "Octave fold"),
        pitch("a4.lo", "Lowest", 48).adv(4),
        pitch("a4.hi", "Highest", 72).adv(4),
        adv_switch(5, "Per-subchannel intervals"),
        list(
            "a5.table",
            "Semitones by subchannel",
            -127,
            127,
            4096,
            &[0, 12],
        )
        .keyed()
        .adv(5),
        adv_switch(6, "Time envelope"),
        curve(
            "a6.curve",
            "Shape across the selection",
            Curve::line(0.0, 1.0),
        )
        .overshoot()
        .adv(6),
        int("a6.depth", "Depth (semitones)", -127, 127, 12)
            .keyed()
            .adv(6),
        adv_switch(7, "Chord-preserving harmonic transpose"),
        int("a7.deg", "Chord roots by scale degrees", -70, 70, 1).adv(7),
        adv_switch(8, "Register fit"),
        pitch("a8.lo", "Range low", 36).adv(8),
        pitch("a8.hi", "Range high", 84).adv(8),
        adv_switch(9, "Alternating interval sequence"),
        list(
            "a9.seq",
            "Semitones per onset",
            -127,
            127,
            256,
            &[0, 7, 0, -5],
        )
        .keyed()
        .adv(9),
        adv_switch(10, "Pitch substitution table"),
        list(
            "a10.map",
            "Pitch-class pairs (from, to, …)",
            0,
            11,
            24,
            &[1, 2, 6, 7],
        )
        .adv(10),
        // V09-S36: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Key-to-key map", false),
        choice("n01.from_key", "Source key", &KEY_NAMES, 0),
        choice("n01.from_scale", "Source scale", scale_names(), 0),
        choice("n01.to_key", "Target key", &KEY_NAMES, 9),
        choice("n01.to_scale", "Target scale", scale_names(), 2),
        int("n01.offset", "Degree offset", -14, 14, 0),
        flag("n02", "Interval scaling", false),
        float("n02.factor", "Factor", -4.0, 4.0, 2.0, "×"),
        choice("n02.ref", "Reference", TR_REFERENCES, 0),
        pitch("n02.pitch", "Reference pitch", 60),
        choice("n02.snap", "Snap", TR_SNAPS, 0),
        choice("n02.key", "Snap key", &KEY_NAMES, 0),
        choice("n02.scale", "Snap scale", scale_names(), 0),
        flag("n03", "Cents by pitch bend", false),
        int("n03.cents", "Cents", -100, 100, 50),
        int("n03.range", "Bend range (semitones)", 1, 48, 2),
        choice("n03.channels", "Channels", TR_CHANNELS, 0),
        flag("n04", "Pitch order permutation", false),
        choice("n04.perm", "Permutation", TR_PERMUTATIONS, 0),
        int("n04.rotate", "Rotate by", -64, 64, 1),
        choice("n04.scope", "Permute within", TR_PERM_SCOPES, 0),
        flag("n05", "Root list", false),
        list("n05.roots", "Roots", -48, 48, 256, &[0, 5, 7, 5]),
        choice("n05.per", "Advance per", TR_ADVANCE, 1),
        choice("n05.unit", "Root unit", TR_UNITS, 0),
        choice("n05.key", "Root key", &KEY_NAMES, 0),
        choice("n05.scale", "Root scale", scale_names(), 0),
        flag("n06", "Octave spread", false),
        pitch("n06.lo", "Spread low", 36),
        pitch("n06.hi", "Spread high", 84),
        choice("n06.mode", "Spread", TR_SPREADS, 0),
        flag("n07", "Voice-leading transpose", false),
        int("n07.shift", "Root shift", -24, 24, 5),
        choice("n07.unit", "Shift unit", TR_UNITS, 0),
        ticks("n07.tol", "Chord tolerance", 0, 1 << 30, 0),
        choice("n07.key", "Shift key", &KEY_NAMES, 0),
        choice("n07.scale", "Shift scale", scale_names(), 0),
        flag("n08", "Frequency shift", false),
        float("n08.hz", "Shift", -4000.0, 4000.0, 100.0, "Hz"),
        float("n08.a4", "Reference A4", 400.0, 480.0, 440.0, "Hz"),
        flag("n09", "Overtone snap", false),
        pitch("n09.fund", "Fundamental", 36),
        int("n09.partials", "Highest partial", 2, 64, 16),
        float("n09.strength", "Strength", 0.0, 100.0, 100.0, "%"),
        flag("n10", "Shepard band", false),
        int("n10.rise", "Rise per bar (semitones)", -12, 12, 1),
        pitch("n10.lo", "Band low", 36),
        pitch("n10.hi", "Band high", 83),
        int("n10.layers", "Layers", 1, 8, 3),
        int("n10.fade", "Fade width (semitones)", 0, 36, 12),
        flag("n11", "Loudness compensation", false),
        int("n11.per", "Velocity per octave", -32, 32, 8),
        int("n11.floor", "Floor", 1, 127, 1),
        int("n11.ceil", "Ceiling", 1, 127, 127),
    ];
    p.extend(boundary::specs(PitchPolicy::Refuse));
    with_help(&mut p, TRANSPOSE_HELP);
    ToolSpec {
        id: "transpose",
        name: "Transpose",
        family: Family::Editor,
        summary: "Moves notes by semitones and octaves; notes that would leave the project's keys (0–127, or 0–255 in 256-Key Mode) follow the boundary policy (refused by default).",
        params: p,
        advanced: vec![
            "Scale-degree transpose",
            "Diatonic transpose (keep accidentals)",
            "Pitch-axis inversion",
            "Octave fold",
            "Per-subchannel intervals",
            "Time envelope",
            "Chord-preserving harmonic transpose",
            "Register fit",
            "Alternating interval sequence",
            "Pitch substitution table",
        ],
        // Orders = the run order (docs/TRANSFORM_ENGINE.md "Transpose"):
        // after the v0.8 pitch, the notes' order and roots (n04, n05, n07),
        // the per-note pitch maps (n01, n02, n08, n09, n06), the band (n10),
        // velocity (n11) as each note is emitted, then the emitted notes'
        // channels and bends (n03).
        functions: [
            ("n01", "Key-to-key map", 140),
            ("n02", "Interval scaling", 150),
            ("n03", "Cents by pitch bend", 520),
            ("n04", "Pitch order permutation", 110),
            ("n05", "Root list", 120),
            ("n06", "Octave spread", 180),
            ("n07", "Voice-leading transpose", 130),
            ("n08", "Frequency shift", 160),
            ("n09", "Overtone snap", 170),
            ("n10", "Shepard band", 190),
            ("n11", "Loudness compensation", 510),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// Collects the unselected notes sounding while the selection does when n03
/// allocates channels (a channel is free for a note while none of them
/// sounds on it).
pub fn transpose_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
) -> Result<(), TransformError> {
    if src.stats.is_empty() || !p.flag("n03") || p.choice("n03.channels") != 1 {
        return Ok(());
    }
    let (a, b) = (src.stats.min_start, src.stats.max_end);
    let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
    *src = s.with_context_sounding(doc, a, b);
    Ok(())
}

/// n01: degree `k` of the source scale → degree `round(k·m / n)` of the
/// target scale (the same degree when both have as many notes), octaves and
/// accidentals kept, plus the degree offset; the target's tonic lies within
/// a tritone of the source's (C major → A minor maps C4 to A3).
#[derive(Clone)]
struct KeyMap {
    from: Scale,
    to: Scale,
    offset: i64,
    /// Whole octaves bringing the target's tonic within a tritone.
    octave: i64,
}

impl KeyMap {
    fn new(from: Scale, to: Scale, offset: i64) -> KeyMap {
        let raw = i64::from(to.root) - i64::from(from.root);
        let near = (raw + 6).rem_euclid(12) - 6;
        KeyMap {
            from,
            to,
            offset,
            octave: near - raw,
        }
    }

    fn map(&self, p: i64) -> i64 {
        let (deg, acc) = self.from.degree_of(p);
        let (nf, nt) = (self.from.steps.len() as i64, self.to.steps.len() as i64);
        let (oct, k) = (deg.div_euclid(nf), deg.rem_euclid(nf));
        let mapped = (2 * k * nt + nf) / (2 * nf);
        self.to.pitch_of(oct * nt + mapped + self.offset) + acc + self.octave
    }
}

/// n10: an endless rise inside a band of whole octaves.
#[derive(Clone)]
struct Shepard {
    /// Semitones per bar (negative = falling).
    rise: i64,
    lo: i64,
    /// The band's whole octaves (≥ 1): keys `lo … lo + 12·octaves − 1`.
    octaves: i64,
    /// Octave layers (1 … octaves).
    layers: i64,
    /// Semitones from either edge over which velocity fades.
    fade: i64,
    /// The bar of the selection's first note (song bar index).
    first_bar: u64,
}

impl Shepard {
    /// Semitones risen at `tick`: the rate times the bars (and part of a
    /// bar) since the first note's bar line, toward zero.
    fn rise_at(&self, meter: &MeterMap, tick: u64) -> i64 {
        if self.rise == 0 {
            return 0;
        }
        let (bar, start, len) = bar_place(meter, tick);
        let bars = bar.saturating_sub(self.first_bar) as f64
            + tick.saturating_sub(start) as f64 / len.max(1) as f64;
        (self.rise as f64 * bars).trunc() as i64
    }

    /// `x` wrapped by octaves into the band.
    fn wrap(&self, x: i64) -> i64 {
        self.lo + (x - self.lo).rem_euclid(12 * self.octaves)
    }

    /// Velocity `vel` of a note at band key `q`: scaled by (d + 1) / (fade
    /// + 1) within `fade` semitones (d) of either edge, at least 1.
    fn velocity(&self, q: i64, vel: i64) -> i64 {
        let top = self.lo + 12 * self.octaves - 1;
        let d = (q - self.lo).min(top - q);
        if self.fade == 0 || d >= self.fade {
            return vel;
        }
        ((vel as f64 * (d + 1) as f64 / (self.fade + 1) as f64).round() as i64).max(1)
    }
}

/// Transpose's v0.9 functions declared by id (TOOL_CATALOG `transpose` n01 …
/// n11). `None` = switched off; with every one off the kernel is the v0.8
/// one.
#[derive(Clone, Default)]
struct TrNew {
    /// n01: the key-to-key map.
    keymap: Option<KeyMap>,
    /// n02: factor, reference (`TR_REFERENCES`), reference pitch, snap
    /// lattice (`None` = chromatic).
    scaling: Option<(f64, usize, i64, Option<Lattice>)>,
    /// n03: cents, bend range (semitones), allocate free channels.
    cents: Option<(i64, i64, bool)>,
    /// n04: permutation, rotation, scope.
    perm: Option<(usize, i64, usize)>,
    /// n05: roots, advance per (`TR_ADVANCE`), scale of degree roots
    /// (`None` = semitones).
    roots: Option<(Vec<i64>, usize, Option<Scale>)>,
    /// n06: lowest key, highest key, mode.
    spread: Option<(i64, i64, usize)>,
    /// n07: root shift, scale of degree shifts (`None` = semitones), chord
    /// tolerance.
    lead: Option<(i64, Option<Scale>, u64)>,
    /// n08: shift (Hz), reference A4 (Hz).
    freq: Option<(f64, f64)>,
    /// n09: the partials' keys (ascending, distinct), strength (fraction).
    overtone: Option<(Vec<i64>, f64)>,
    /// n10: the band.
    shepard: Option<Shepard>,
    /// n11: velocity per octave, floor, ceiling.
    loud: Option<(i64, i64, i64)>,
    /// n05: the bar and beat of the selection's first note (song indices).
    first_bar: u64,
    first_beat: Option<u64>,
    /// B2-F01: the switched-on pitch maps (n01, n02, n08, n09, n06) in run
    /// order (`order::TRANSPOSE_PITCH`; declared order unless the user chose
    /// one).
    order: Vec<PitchMap>,
}

/// One of Transpose's reorderable pitch maps (B2-F01).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PitchMap {
    /// n01 key-to-key map.
    KeyMap,
    /// n02 interval scaling.
    Scaling,
    /// n08 frequency shift.
    Freq,
    /// n09 overtone snap.
    Overtone,
    /// n06 octave spread.
    Spread,
}

impl TrNew {
    fn from(p: &Params, b: &BuildCtx) -> TrNew {
        let sc = |k: &str, s: &str| Scale::new(p.choice(k), p.choice(s));
        let empty = b.stats.is_empty();
        let meter = &b.ctx.meter;
        let first = if empty { 0 } else { b.stats.min_start };
        let first_bar = bar_place(meter, first).0;
        let pair = |a: &str, z: &str| {
            let (a, z) = (p.int(a), p.int(z));
            (a.min(z), a.max(z))
        };
        TrNew {
            keymap: p.flag("n01").then(|| {
                KeyMap::new(
                    sc("n01.from_key", "n01.from_scale"),
                    sc("n01.to_key", "n01.to_scale"),
                    p.int("n01.offset"),
                )
            }),
            scaling: p.flag("n02").then(|| {
                (
                    p.float("n02.factor"),
                    p.choice("n02.ref"),
                    p.int("n02.pitch"),
                    (p.choice("n02.snap") == 1)
                        .then(|| Lattice::of_scale(&sc("n02.key", "n02.scale"))),
                )
            }),
            cents: p.flag("n03").then(|| {
                (
                    p.int("n03.cents"),
                    p.int("n03.range").max(1),
                    p.choice("n03.channels") == 1,
                )
            }),
            perm: p.flag("n04").then(|| {
                (
                    p.choice("n04.perm"),
                    p.int("n04.rotate"),
                    p.choice("n04.scope"),
                )
            }),
            roots: (p.flag("n05") && !p.list("n05.roots").is_empty()).then(|| {
                (
                    p.list("n05.roots"),
                    p.choice("n05.per"),
                    (p.choice("n05.unit") == 1).then(|| sc("n05.key", "n05.scale")),
                )
            }),
            spread: p.flag("n06").then(|| {
                let (lo, hi) = pair("n06.lo", "n06.hi");
                (lo, hi, p.choice("n06.mode"))
            }),
            lead: p.flag("n07").then(|| {
                (
                    p.int("n07.shift"),
                    (p.choice("n07.unit") == 1).then(|| sc("n07.key", "n07.scale")),
                    p.ticks("n07.tol"),
                )
            }),
            freq: p
                .flag("n08")
                .then(|| (p.float("n08.hz"), p.float("n08.a4").max(1.0))),
            overtone: p.flag("n09").then(|| {
                let f = p.int("n09.fund") as f64;
                let mut keys: Vec<i64> = (1..=p.int("n09.partials").clamp(1, 64))
                    .map(|k| (f + 12.0 * (k as f64).log2()).round() as i64)
                    .collect();
                keys.dedup();
                (keys, p.float("n09.strength") / 100.0)
            }),
            shepard: p.flag("n10").then(|| {
                let (lo, hi) = pair("n10.lo", "n10.hi");
                let octaves = ((hi - lo + 1) / 12).max(1);
                Shepard {
                    rise: p.int("n10.rise"),
                    lo,
                    octaves,
                    layers: p.int("n10.layers").clamp(1, 8).min(octaves),
                    fade: p.int("n10.fade").max(0),
                    first_bar,
                }
            }),
            loud: p.flag("n11").then(|| {
                let (lo, hi) = pair("n11.floor", "n11.ceil");
                (p.int("n11.per"), lo, hi)
            }),
            first_bar,
            first_beat: meter.beat_at(first).map(|(i, _)| i),
            order: Vec::new(),
        }
        .with_order(p)
    }

    /// The switched-on pitch maps in the order `p` asks for (B2-F01).
    fn with_order(mut self, p: &Params) -> TrNew {
        self.order = p
            .sequence(crate::transform::order::TRANSPOSE_PITCH.members)
            .into_iter()
            .filter_map(|id| match id {
                "n01" if self.keymap.is_some() => Some(PitchMap::KeyMap),
                "n02" if self.scaling.is_some() => Some(PitchMap::Scaling),
                "n08" if self.freq.is_some() => Some(PitchMap::Freq),
                "n09" if self.overtone.as_ref().is_some_and(|o| !o.0.is_empty()) => {
                    Some(PitchMap::Overtone)
                }
                "n06" if self.spread.is_some() => Some(PitchMap::Spread),
                _ => None,
            })
            .collect();
        self
    }

    fn any(&self) -> bool {
        self.keymap.is_some()
            || self.scaling.is_some()
            || self.cents.is_some()
            || self.perm.is_some()
            || self.roots.is_some()
            || self.spread.is_some()
            || self.lead.is_some()
            || self.freq.is_some()
            || self.overtone.is_some()
            || self.shepard.is_some()
            || self.loud.is_some()
    }

    /// Whether every switched-on function is a per-note rule (the lazy
    /// viewport preview): the others read the notes around each note or
    /// write pitch bends.
    fn pointwise(&self) -> bool {
        self.scaling
            .as_ref()
            .is_none_or(|s| s.1 != REF_FIRST && s.1 != REF_LOWEST)
            && self.cents.is_none_or(|c| c.0 == 0)
            && self.perm.is_none()
            && self.roots.as_ref().is_none_or(|r| r.1 != ADV_CHORD)
            && self.spread.is_none_or(|s| s.2 == SPREAD_PITCH_CLASS)
            && self.lead.is_none()
    }

    /// Output notes per source note (n10's layers).
    fn copies(&self) -> u64 {
        self.shepard.as_ref().map_or(1, |s| s.layers as u64)
    }
}

/// n02: the pitch of `lat` nearest to `x` (the lower one on a tie).
fn nearest_real(lat: Lattice, x: f64) -> i64 {
    let fl = x.floor() as i64;
    let down = if lat.contains(fl) { fl } else { lat.below(fl) };
    let up = lat.above(fl);
    if x - down as f64 <= up as f64 - x {
        down
    } else {
        up
    }
}

/// n04: `ps` reordered.
fn permuted(ps: &[i64], perm: usize, k: i64) -> Vec<i64> {
    let m = ps.len();
    let mut sorted = ps.to_vec();
    sorted.sort_unstable();
    match perm {
        PERM_REVERSE => ps.iter().rev().copied().collect(),
        PERM_ROTATE => (0..m)
            .map(|j| ps[(j as i64 - k).rem_euclid(m as i64) as usize])
            .collect(),
        PERM_SORT_UP => sorted,
        PERM_SORT_DOWN => sorted.into_iter().rev().collect(),
        // Outside in: lowest, highest, second lowest, second highest, …
        _ => (0..m)
            .map(|j| {
                if j % 2 == 0 {
                    sorted[j / 2]
                } else {
                    sorted[m - 1 - j / 2]
                }
            })
            .collect(),
    }
}

/// n07's working memory, reused from chord to chord (no allocation per
/// chord or per voice).
#[derive(Default)]
struct LeadScratch {
    /// Every voice's candidate keys, flat; voice `i`'s are
    /// `cand[at[i]..at[i + 1]]`.
    cand: Vec<i64>,
    at: Vec<usize>,
    /// The least total movement ending at each candidate, and the previous
    /// voice's candidate it came from.
    cost: Vec<u64>,
    back: Vec<u32>,
    out: Vec<i64>,
}

/// n07: targets for `voices` (ascending pitches) on the pitch classes of
/// `tones` — each within an octave of its voice, in the voices' order
/// (distinct voices stay distinct), moving the voices least in total (ties:
/// the lower pitches) — in `s.out`; false when no such assignment exists.
/// A single voice takes the nearest key of its tone directly; a chord runs
/// a dynamic programme over the candidates. O(voices × candidates).
fn voice_lead(voices: &[i64], tones: u16, s: &mut LeadScratch) -> bool {
    s.out.clear();
    if let [v] = voices {
        // The nearest key of the one tone (the lower on a tritone).
        let t = i64::from(tones.trailing_zeros());
        let d = (t - v).rem_euclid(12);
        s.out.push(v + if d <= 5 { d } else { d - 12 });
        return true;
    }
    let lat = Lattice::from_mask(tones);
    s.cand.clear();
    s.at.clear();
    for &v in voices {
        s.at.push(s.cand.len());
        s.cand
            .extend((v - 12..=v + 12).filter(|&q| lat.contains(q)));
    }
    s.at.push(s.cand.len());
    const NONE: u64 = u64::MAX;
    s.cost.clear();
    s.cost.resize(s.cand.len(), NONE);
    s.back.clear();
    s.back.resize(s.cand.len(), 0);
    for c in s.at[0]..s.at[1] {
        s.cost[c] = s.cand[c].abs_diff(voices[0]);
    }
    for i in 1..voices.len() {
        let (p0, p1) = (s.at[i - 1], s.at[i]);
        let strict = voices[i] > voices[i - 1];
        // Running minimum over the previous candidates below q (or at q
        // for a doubled voice); the first (lowest) on a tie.
        let (mut j, mut best, mut arg) = (p0, NONE, p0);
        for c in s.at[i]..s.at[i + 1] {
            let q = s.cand[c];
            while j < p1 && (s.cand[j] < q || (!strict && s.cand[j] == q)) {
                if s.cost[j] < best {
                    best = s.cost[j];
                    arg = j;
                }
                j += 1;
            }
            if best != NONE {
                s.cost[c] = best + q.abs_diff(voices[i]);
                s.back[c] = arg as u32;
            }
        }
    }
    let last = voices.len() - 1;
    let (mut c, mut best) = (0usize, NONE);
    for k in s.at[last]..s.at[last + 1] {
        if s.cost[k] < best {
            best = s.cost[k];
            c = k;
        }
    }
    if best == NONE {
        return false;
    }
    s.out.resize(voices.len(), 0);
    for i in (0..voices.len()).rev() {
        s.out[i] = s.cand[c];
        c = s.back[c] as usize;
    }
    true
}

/// The bend value of `cents` at a bend range of `range` semitones.
fn cents_bend(cents: i64, range: i64) -> i64 {
    (8192.0 + cents as f64 / (100.0 * range as f64) * 8192.0)
        .round()
        .clamp(0.0, 16383.0) as i64
}

struct Transpose {
    offset: i64,
    scale: Scale,
    degree: Option<i64>,
    diatonic: Option<i64>,
    axis: Option<i64>,
    fold: Option<(i64, i64)>,
    table: Vec<i64>,
    envelope: Option<(Curve, i64)>,
    fit_shift: i64,
    pc_map: [i64; 12],
    stats: super::super::SourceStats,
    subs: Vec<Vec<(u32, u8)>>,
    /// V09-S36: the functions declared by id.
    new: TrNew,
    ctx: Ctx,
    /// With Control Sweeps (E0422): what each level's settings are built
    /// from.
    swept: Option<std::sync::Arc<Rebuild>>,
}

impl Transpose {
    /// The pitch before context-dependent steps (sequence, chords).
    fn pitch(&self, n: &TNote, extra: i64) -> i64 {
        let mut p = i64::from(n.pitch);
        if let Some(a) = self.axis {
            p = 2 * a - p;
        }
        p += self.pc_map[p.rem_euclid(12) as usize];
        if let Some(d) = self.degree {
            let (deg, _) = self.scale.degree_of(self.scale.snap(p, SnapMode::Nearest));
            p = self.scale.pitch_of(deg + d);
        } else if let Some(d) = self.diatonic {
            let (deg, off) = self.scale.degree_of(p);
            p = self.scale.pitch_of(deg + d) + off;
        }
        p += self.offset + extra;
        if !self.table.is_empty() {
            let si = self
                .subs
                .get(usize::from(n.track))
                .and_then(|t| t.iter().position(|s| s.0 == n.sub))
                .unwrap_or(0);
            p += self.table[si % self.table.len()];
        }
        if let Some((c, depth)) = &self.envelope {
            p += (c.eval(pos(n, &self.stats)) * *depth as f64).round() as i64;
        }
        if let Some((lo, hi)) = self.fold {
            let (lo, hi) = (lo.min(hi), lo.max(hi));
            if hi - lo >= 11 {
                while p > hi {
                    p -= 12;
                }
                while p < lo {
                    p += 12;
                }
            }
        }
        p + self.fit_shift
    }

    fn emit(&self, n: &TNote, p: i64, out: &mut Output) {
        out.emit(
            i128::from(n.start),
            i128::from(n.dur),
            p,
            i64::from(n.vel),
            n,
        );
    }

    /// n05: the root list's entry for a note at `start` advancing per bar
    /// or beat (counted from the selection's first note).
    fn root_index(&self, start: u64) -> u64 {
        let Some((_, per, _)) = &self.new.roots else {
            return 0;
        };
        let meter = &self.ctx.meter;
        if *per == ADV_BEAT
            && let (Some(first), Some((b, _))) = (self.new.first_beat, meter.beat_at(start))
        {
            return b.saturating_sub(first);
        }
        bar_place(meter, start).0.saturating_sub(self.new.first_bar)
    }

    /// n05: `p` moved by entry `k` of the root list.
    fn root_step(&self, p: i64, k: u64) -> i64 {
        let Some((list, _, sc)) = &self.new.roots else {
            return p;
        };
        let e = list[(k % list.len() as u64) as usize];
        match sc {
            None => p + e,
            Some(sc) => {
                let (d, off) = sc.degree_of(p);
                sc.pitch_of(d + e) + off
            }
        }
    }

    /// n07: `p` moved by the root shift (semitones, or degrees keeping the
    /// accidental).
    fn lead_step(&self, p: i64) -> i64 {
        match &self.new.lead {
            Some((d, Some(sc), _)) => {
                let (deg, off) = sc.degree_of(p);
                sc.pitch_of(deg + d) + off
            }
            Some((d, None, _)) => p + d,
            None => p,
        }
    }

    /// One pitch map of the group (B2-F01: n01, n02, n08, n09, n06 run in
    /// `TrNew::order`) on pitch `p`; a switched-off map leaves it.
    /// `reference` = n02's reference, `onset` = the note's onset number on
    /// its track (n06's onset spreads).
    fn map_stage(&self, m: PitchMap, mut p: i64, reference: i64, onset: u64) -> i64 {
        let w = &self.new;
        match m {
            PitchMap::KeyMap => {
                if let Some(km) = &w.keymap {
                    p = km.map(p);
                }
            }
            PitchMap::Scaling => {
                if let Some((f, _, _, lat)) = w.scaling {
                    let x = reference as f64 + f * (p - reference) as f64;
                    p = match lat {
                        Some(lat) => nearest_real(lat, x),
                        None => x.round() as i64,
                    };
                }
            }
            PitchMap::Freq => {
                if let Some((hz, a4)) = w.freq {
                    let f = a4 * ((p - 69) as f64 / 12.0).exp2() + hz;
                    p = if f > 0.0 {
                        (69.0 + 12.0 * (f / a4).log2()).round() as i64
                    } else {
                        NO_FREQUENCY
                    };
                }
            }
            PitchMap::Overtone => {
                if let Some((keys, s)) = &w.overtone
                    && !keys.is_empty()
                {
                    let k = keys.partition_point(|&x| x < p);
                    let t = match (k.checked_sub(1).map(|j| keys[j]), keys.get(k)) {
                        (Some(a), Some(&b)) => {
                            if p - a <= b - p {
                                a
                            } else {
                                b
                            }
                        }
                        (Some(a), None) => a,
                        (None, Some(&b)) => b,
                        (None, None) => p,
                    };
                    p += (s * (t - p) as f64).round() as i64;
                }
            }
            PitchMap::Spread => {
                if let Some((lo, hi, mode)) = w.spread {
                    let first = lo + (p - lo).rem_euclid(12);
                    if first <= hi {
                        let count = (hi - first) / 12 + 1;
                        let slot = match mode {
                            SPREAD_ONSET => onset as i64 % count,
                            SPREAD_ALTERNATING => {
                                let r = onset as i64 % count;
                                if r % 2 == 0 { r / 2 } else { count - 1 - r / 2 }
                            }
                            _ => (p - lo).rem_euclid(12) * count / 12,
                        };
                        p = first + 12 * slot;
                    }
                }
            }
        }
        p
    }

    /// n10 → n11 of one note at its mapped pitch `p` (after the v0.8 pitch,
    /// n04, n05, n07 and the pitch maps): emits it (n10: one note per layer).
    fn layers(&self, n: &TNote, p: i64, out: &mut Output) {
        let w = &self.new;
        let vel = i64::from(n.vel);
        match &w.shepard {
            Some(sh) => {
                let x = p + sh.rise_at(&self.ctx.meter, n.start);
                for j in 0..sh.layers {
                    let q = sh.wrap(x + 12 * j);
                    self.emit9(n, q, sh.velocity(q, vel), out);
                }
            }
            None => self.emit9(n, p, vel, out),
        }
    }

    /// Emits `n` at pitch `p` and velocity `vel`, n11 compensating the
    /// distance from its written pitch (to the key the boundary policy
    /// gives it).
    #[inline]
    fn emit9(&self, n: &TNote, p: i64, vel: i64, out: &mut Output) {
        let vel = match self.new.loud {
            Some((per, lo, hi)) => {
                let landed = out.boundary.pitch(p).0.map_or(p, i64::from);
                let d = landed - i64::from(n.pitch);
                if d == 0 {
                    vel
                } else {
                    (vel - (per as f64 * d as f64 / 12.0).round() as i64).clamp(lo, hi)
                }
            }
            None => vel,
        };
        out.emit(i128::from(n.start), i128::from(n.dur), p, vel, n);
    }

    /// The v0.9 pipeline over the whole source: the v0.8 pitch (with the a7
    /// chord roots and the a9 sequence per onset), n04, n05, n07 and n01 for
    /// every note, n02's reference, then each note's tail, then n03's
    /// channels and bends.
    fn run9(
        &self,
        src: &Source,
        chord_deg: Option<i64>,
        seq: &[i64],
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let w = &self.new;
        // Each note's frozen settings and a7 degree under Control Sweeps
        // (E0422; every Transpose target is a frozen or base control).
        let lv = self.swept.as_ref().map(|r| {
            r.levels(&self.ctx, src, |p, b| {
                let (t, d, _) = transpose_of(p, b);
                (t, d)
            })
        });
        let at = |i: usize| -> (&Transpose, Option<i64>) {
            match &lv {
                Some(l) => {
                    let c = l.at(i);
                    (&c.0, c.1)
                }
                None => (self, chord_deg),
            }
        };
        let tracks = per_track(src);
        let n = src.notes.len();
        let onsets = w.roots.as_ref().is_some_and(|r| r.1 == ADV_CHORD)
            || w.spread.is_some_and(|s| s.2 != SPREAD_PITCH_CLASS);
        let mut pitch = vec![0i64; n];
        let mut onset: Vec<u32> = if onsets { vec![0; n] } else { Vec::new() };
        for idx in &tracks {
            // Onset groups of the track (as v0.8): chords share the a7
            // root shift; the a9 sequence advances once per onset.
            let (mut a, mut gi) = (0usize, 0usize);
            while a < idx.len() {
                let s = src.notes[idx[a]].start;
                let mut z = a + 1;
                while z < idx.len() && src.notes[idx[z]].start == s {
                    z += 1;
                }
                let group = &idx[a..z];
                let seq_off = if seq.is_empty() {
                    0
                } else {
                    seq[gi % seq.len()]
                };
                let root = || {
                    let root = group
                        .iter()
                        .map(|&i| i64::from(src.notes[i].pitch))
                        .min()
                        .unwrap_or(0);
                    let snapped = self.scale.snap(root, SnapMode::Nearest);
                    (snapped, self.scale.degree_of(snapped).0)
                };
                let chord_root = chord_deg.map(|_| root());
                for &i in group {
                    let (ti, di) = at(i);
                    let chord_off = match (di, chord_root) {
                        (Some(d), Some((snapped, deg))) => self.scale.pitch_of(deg + d) - snapped,
                        _ => 0,
                    };
                    pitch[i] = ti.pitch(&src.notes[i], seq_off + chord_off);
                    if onsets {
                        onset[i] = gi as u32;
                    }
                }
                gi += 1;
                a = z;
            }
        }
        if let Some((perm, k, scope)) = w.perm {
            for idx in &tracks {
                self.permute_track(src, idx, scope, perm, k, &mut pitch);
            }
        }
        if let Some((_, per, _)) = &w.roots {
            for (i, nt) in src.notes.iter().enumerate() {
                let k = if *per == ADV_CHORD {
                    u64::from(onset[i])
                } else {
                    self.root_index(nt.start)
                };
                pitch[i] = self.root_step(pitch[i], k);
            }
        }
        if let Some((_, _, tol)) = &w.lead {
            let tol = *tol;
            let mut voices: Vec<(i64, usize)> = Vec::new();
            let mut ps: Vec<i64> = Vec::new();
            let mut scratch = LeadScratch::default();
            for idx in &tracks {
                for chord in ChordGroups::of(&src.notes, idx, tol).iter() {
                    voices.clear();
                    voices.extend(chord.iter().map(|&i| (pitch[i], i)));
                    voices.sort_unstable();
                    let tones = voices
                        .iter()
                        .fold(0u16, |m, v| m | 1 << self.lead_step(v.0).rem_euclid(12));
                    ps.clear();
                    ps.extend(voices.iter().map(|v| v.0));
                    if voice_lead(&ps, tones, &mut scratch) {
                        for (v, &q) in voices.iter().zip(&scratch.out) {
                            pitch[v.1] = q;
                        }
                    } else {
                        for v in &voices {
                            pitch[v.1] = self.lead_step(v.0);
                        }
                    }
                }
            }
        }
        // The pitch maps in run order (B2-F01), each over every note; n02's
        // reference is read from the pitches as the maps before it left them.
        for &m in &w.order {
            let reference = match w.scaling {
                Some((_, REF_FIRST, ..)) if m == PitchMap::Scaling => (0..n)
                    .min_by_key(|&i| (src.notes[i].start, pitch[i]))
                    .map_or(0, |i| pitch[i]),
                Some((_, REF_LOWEST, ..)) if m == PitchMap::Scaling => {
                    pitch.iter().copied().min().unwrap_or(0)
                }
                Some((_, _, r, _)) => r,
                None => 0,
            };
            for (i, p) in pitch.iter_mut().enumerate() {
                let k = onset.get(i).map_or(0, |&o| u64::from(o));
                *p = self.map_stage(m, *p, reference, k);
            }
        }
        let mark = out.mark();
        let from = out.notes.len();
        for (i, nt) in src.notes.iter().enumerate() {
            self.layers(nt, pitch[i], out);
            if i & 0xFFF == 0xFFF {
                out.check()?;
            }
        }
        out.check()?;
        if let Some((cents, range, allocate)) = w.cents
            && cents != 0
        {
            self.bend_cents(src, out, mark, from, cents_bend(cents, range), allocate);
        }
        Ok(())
    }

    /// n04: reorders the pitches of track `idx` (time order) within each
    /// scope — the selection, each bar or each phrase (split by rests of a
    /// beat or more) — keeping the times: the notes in time order (a
    /// chord's notes by pitch) take the reordered pitches.
    fn permute_track(
        &self,
        src: &Source,
        idx: &[usize],
        scope: usize,
        perm: usize,
        k: i64,
        pitch: &mut [i64],
    ) {
        let mut apply = |part: &[usize]| {
            let mut order = part.to_vec();
            order.sort_unstable_by_key(|&i| (src.notes[i].start, pitch[i], i));
            let ps: Vec<i64> = order.iter().map(|&i| pitch[i]).collect();
            for (&i, q) in order.iter().zip(permuted(&ps, perm, k)) {
                pitch[i] = q;
            }
        };
        match scope {
            SCOPE_BAR => {
                let meter = &self.ctx.meter;
                let mut a = 0;
                while a < idx.len() {
                    let (bar, len) = bar_bounds(meter, src.notes[idx[a]].start);
                    let end = bar.saturating_add(len);
                    let mut z = a + 1;
                    while z < idx.len() && src.notes[idx[z]].start < end {
                        z += 1;
                    }
                    apply(&idx[a..z]);
                    a = z;
                }
            }
            SCOPE_PHRASE => {
                let ph = Phrases::of(&src.notes, idx, self.ctx.ppq.max(1), None);
                for part in ph.iter(idx) {
                    apply(part);
                }
            }
            _ => apply(idx),
        }
    }

    /// n03: bends the notes emitted since `mark` (`from` in `out.notes`) to
    /// `value` — on their own channels, or (allocating) each on the first
    /// channel from its own (round the sixteen, skipping the drum channel)
    /// on which no unselected note sounds while it does; a note finding none
    /// keeps its channel and is not bent. Then per track and channel a bend
    /// at each bent note's start and the centre after each run of
    /// overlapping bent notes.
    fn bend_cents(
        &self,
        src: &Source,
        out: &mut Output,
        mark: Mark,
        from: usize,
        value: i64,
        allocate: bool,
    ) {
        let notes = &out.notes[from..];
        let mut chans: Vec<u8> = notes.iter().map(|n| n.ch).collect();
        let mut bent = vec![true; notes.len()];
        let mut lost = 0u64;
        if allocate {
            // Unselected notes per (track, channel): starts ascending with
            // the running latest end.
            let mut busy: std::collections::HashMap<(u16, u8), (Vec<u64>, Vec<u64>)> =
                Default::default();
            for c in src.context.iter() {
                let e = busy.entry((c.track, c.ch)).or_default();
                e.0.push(c.start);
                e.1.push(c.end());
            }
            for (starts, ends) in busy.values_mut() {
                let mut v: Vec<(u64, u64)> =
                    starts.iter().copied().zip(ends.iter().copied()).collect();
                v.sort_unstable();
                let mut run = 0;
                *starts = v.iter().map(|x| x.0).collect();
                *ends = v
                    .iter()
                    .map(|x| {
                        run = run.max(x.1);
                        run
                    })
                    .collect();
            }
            let free = |track: u16, ch: u8, s: u64, e: u64| {
                busy.get(&(track, ch)).is_none_or(|(starts, ends)| {
                    let k = starts.partition_point(|&x| x < e);
                    k == 0 || ends[k - 1] <= s
                })
            };
            for (k, nt) in notes.iter().enumerate() {
                let (s, e) = (nt.start, nt.end().max(nt.start.saturating_add(1)));
                let pick = (0..16u8)
                    .map(|d| (nt.ch + d) % 16)
                    .filter(|&c| c == nt.ch || c != DRUM_CHANNEL)
                    .find(|&c| free(nt.track, c, s, e));
                match pick {
                    Some(c) => chans[k] = c,
                    None => {
                        bent[k] = false;
                        lost += 1;
                    }
                }
            }
            out.adjust_since(mark, |i, n, _, _| {
                if let Some(&c) = chans.get(i) {
                    n.ch = c;
                }
                true
            });
        }
        let mut runs: Vec<(u16, u8, u64, u64)> = out.notes[from..]
            .iter()
            .zip(&bent)
            .filter(|(_, b)| **b)
            .map(|(n, _)| (n.track, n.ch, n.start, n.end()))
            .collect();
        runs.sort_unstable();
        let mut k = 0;
        while k < runs.len() {
            let (track, ch) = (runs[k].0, runs[k].1);
            let mut until = 0u64;
            let mut open = false;
            while k < runs.len() && runs[k].0 == track && runs[k].1 == ch {
                let (_, _, s, e) = runs[k];
                if open && s > until {
                    out.emit_event(track, Lane::PitchBend, ch, i128::from(until), 8192);
                    open = false;
                }
                out.emit_event(track, Lane::PitchBend, ch, i128::from(s), value);
                until = if open { until.max(e) } else { e };
                open = true;
                k += 1;
            }
            out.emit_event(track, Lane::PitchBend, ch, i128::from(until), 8192);
        }
        if lost > 0 && out.message.is_none() {
            out.message = Some(format!(
                "{} note(s) found no free channel and were not bent.",
                crate::midi::group_digits(lost)
            ));
        }
    }
}

/// Transpose's settings from `p` (the kernel, a7's degree, a9's sequence).
fn transpose_of(p: &Params, b: &BuildCtx) -> (Transpose, Option<i64>, Vec<i64>) {
    let scale = Scale::new(p.choice("key"), p.choice("scale"));
    let mut pc_map = [0i64; 12];
    if p.adv(10) {
        let m = p.list("a10.map");
        for pair in m.chunks_exact(2) {
            let (from, to) = (pair[0].rem_euclid(12), pair[1].rem_euclid(12));
            pc_map[from as usize] = to - from;
        }
    }
    let offset = p.int("semis") + 12 * p.int("octaves");
    let mut t = Transpose {
        offset,
        scale: scale.clone(),
        degree: p.adv(1).then(|| p.int("a1.deg")),
        diatonic: (p.adv(2) && !p.adv(1)).then(|| p.int("a2.deg")),
        axis: p.adv(3).then(|| p.int("a3.axis")),
        fold: p.adv(4).then(|| (p.int("a4.lo"), p.int("a4.hi"))),
        table: if p.adv(5) {
            p.list("a5.table")
        } else {
            Vec::new()
        },
        envelope: p.adv(6).then(|| (p.curve("a6.curve"), p.int("a6.depth"))),
        fit_shift: 0,
        pc_map,
        stats: b.stats.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        new: TrNew::from(p, b),
        ctx: b.ctx.clone(),
        swept: None,
    };
    if p.adv(8) && !b.stats.is_empty() {
        // Octave shift that best centres the transposed range in [lo, hi].
        let (lo, hi) = (p.int("a8.lo"), p.int("a8.hi"));
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        let a = i64::from(b.stats.min_pitch) + offset;
        let z = i64::from(b.stats.max_pitch) + offset;
        let centre = (lo + hi) as f64 / 2.0;
        let mid = (a + z) as f64 / 2.0;
        t.fit_shift = (((centre - mid) / 12.0).round() as i64) * 12;
    }
    let chord_deg = p.adv(7).then(|| p.int("a7.deg"));
    let seq = if p.adv(9) {
        p.list("a9.seq")
    } else {
        Vec::new()
    };
    (t, chord_deg, seq)
}

fn transpose_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let (mut t, chord_deg, seq) = transpose_of(p, b);
    t.swept = Rebuild::of(p, b);
    if t.new.any() || t.swept.is_some() {
        // V09-S36: the v0.9 pipeline (n10 adds a note per layer); with
        // Control Sweeps always the whole-source run (E0422).
        let copies = t.new.copies();
        let growth = if copies > 1 {
            Growth::Expand(copies)
        } else {
            Growth::Map
        };
        if let Some((cents, ..)) = t.new.cents
            && cents != 0
        {
            // A bend at each note's start and at most one centre per note.
            let events = (b.stats.count as u64)
                .saturating_mul(copies)
                .saturating_mul(2);
            if events > MAX_EVENTS as u64 {
                return Err(TransformError::TooLarge(format!(
                    "The cents would write about {} pitch-bend events; select fewer notes or use fewer Shepard layers.",
                    crate::midi::group_digits(events)
                )));
            }
        }
        if t.new.pointwise() && chord_deg.is_none() && seq.is_empty() && t.swept.is_none() {
            let reference = t.new.scaling.as_ref().map_or(0, |s| s.2);
            return boxed(Pointwise {
                f: move |n: &TNote, out: &mut Output| {
                    let mut p = t.pitch(n, 0);
                    if t.new.roots.is_some() {
                        p = t.root_step(p, t.root_index(n.start));
                    }
                    for &m in &t.new.order {
                        p = t.map_stage(m, p, reference, 0);
                    }
                    t.layers(n, p, out);
                },
                ahead: 0,
                behind: 0,
                growth,
            });
        }
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| t.run9(src, chord_deg, &seq, out),
            growth,
            estimate: None,
        });
    }
    if chord_deg.is_some() || !seq.is_empty() {
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                // Onset groups per track: chords share the root shift; the
                // sequence advances once per onset.
                for slot in 0..src.tracks.len() as u16 {
                    let idx = src.indices_of_track(slot);
                    let notes: Vec<TNote> = idx.iter().map(|&i| src.notes[i]).collect();
                    for (gi, (a, len)) in Source::onset_groups(&notes, 0).into_iter().enumerate() {
                        let group = &notes[a..a + len];
                        let seq_off = if seq.is_empty() {
                            0
                        } else {
                            seq[gi % seq.len()]
                        };
                        let chord_off = match chord_deg {
                            Some(d) => {
                                let root =
                                    group.iter().map(|n| i64::from(n.pitch)).min().unwrap_or(0);
                                let snapped = t.scale.snap(root, SnapMode::Nearest);
                                let (deg, _) = t.scale.degree_of(snapped);
                                t.scale.pitch_of(deg + d) - snapped
                            }
                            None => 0,
                        };
                        for n in group {
                            t.emit(n, t.pitch(n, seq_off + chord_off), out);
                        }
                        out.check()?;
                    }
                }
                Ok(())
            },
            growth: Growth::Map,
            estimate: None,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| t.emit(n, t.pitch(n, 0), out),
        ahead: 0,
        behind: 0,
        growth: Growth::Map,
    })
}

/// Boundary from params, for tools that use the standard controls.
pub fn boundary_of(p: &Params) -> Boundary {
    Boundary::from_params(p)
}

// ================================================================ Legato

pub const LEGATO: ToolDef = ToolDef {
    id: "legato",
    spec: legato_spec,
    build: legato_build,
};

/// n01: what happens to rests longer than the threshold.
const LEG_LONG_RESTS: &[&str] = &["Leave them", "Shrink to the gap"];
/// n02: the metric line a note may not cross.
const LEG_BOUNDARIES: &[&str] = &["Beat", "Bar", "Grid"];
/// n03: where the pedal lifts.
const LEG_RELEASES: &[&str] = &["Every bar", "Every N beats", "Chord changes"];
const LEG_PEDAL_MODES: &[&str] = &["Extend the notes", "Write CC 64"];
/// n04: an extension that would pass the limit.
const LEG_OVER: &[&str] = &["Stop the extension", "Keep the written length"];
/// n04: which notes are extended first.
const LEG_FIRST: &[&str] = &["Lowest voices", "Highest voices", "Earliest notes"];
/// n06: what counts as the same key.
const LEG_SAME_KEY: &[&str] = &["Key", "Key and channel", "Key and subchannel"];
/// n07: how a common tone is tied.
const LEG_TIES: &[&str] = &["Merge into one note", "Hold the first and drop the repeat"];
/// n09: the voice kept as the line, and the other voices.
const LEG_VOICES: &[&str] = &["Top", "Bottom"];
const LEG_OTHERS: &[&str] = &["Keep", "Shorten to the gap", "Remove"];
/// n10: which onset a note connects to.
const LEG_DIRECTIONS: &[&str] = &["Next higher", "Next lower", "Same direction"];
/// n11: whose onsets are counted.
const LEG_COUNT_OF: &[&str] = &["The track", "The subchannel", "All selected"];

const RELEASE_BEATS: usize = 1;
const RELEASE_CHORDS: usize = 2;
const BOUNDARY_BEAT: usize = 0;
const BOUNDARY_BAR: usize = 1;
const FIRST_LOWEST: usize = 0;
const FIRST_HIGHEST: usize = 1;
const DIRECTION_UP: usize = 0;
const DIRECTION_DOWN: usize = 1;

/// A note's part in the v0.9 pipeline.
const ROLE_LEGATO: u8 = 0;
/// Kept as written (n08 outside the lengths, n09's other voices kept).
const ROLE_WRITTEN: u8 = 1;
/// n09's other voices shortened to the gap.
const ROLE_SHORTEN: u8 = 2;
/// Removed (n07's tied repeats, n09's other voices removed).
const ROLE_REMOVED: u8 = 3;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const LEGATO_HELP: &[(&str, &str)] = &[
    (
        "context",
        "Lets the track's unselected notes count as the next note a selected note is extended to.",
    ),
    (
        "a1",
        "Ends every note exactly where the next one starts, shortening notes that overlap it.",
    ),
    (
        "a2",
        "Ends every note a fixed amount after the next one starts, so neighbors overlap.",
    ),
    (
        "a2.ticks",
        "How far each note runs past the next note's start, in ticks.",
    ),
    (
        "a3",
        "Ends every note a fixed amount before the next one starts, leaving a small gap.",
    ),
    (
        "a3.ticks",
        "How long the gap before the next note is, in ticks.",
    ),
    (
        "a4",
        "Sets the overlap as a share of the distance to the next note (negative leaves a gap).",
    ),
    (
        "a4.pct",
        "How far past the next note's start each note ends, as a share of the distance to it.",
    ),
    (
        "a5",
        "Extends notes only to the next chord, skipping single notes in between.",
    ),
    (
        "a5.min",
        "How many notes must start together to count as a chord.",
    ),
    (
        "a6",
        "Extends each note only to the next note on the same key.",
    ),
    (
        "a7",
        "Extends each note to the next note close to it in pitch, so each voice joins its own line.",
    ),
    (
        "a7.max",
        "How many semitones away the next note of the voice may be.",
    ),
    (
        "a8",
        "Extends each note only to the next note on the same subchannel.",
    ),
    (
        "a9",
        "Changes the overlap gradually across the selection, following a curve.",
    ),
    (
        "a9.curve",
        "How the overlap changes over the selection's time, from Overlap at 0 to Overlap at 1.",
    ),
    (
        "a9.min",
        "The overlap in ticks where the curve is at 0 (negative leaves a gap).",
    ),
    (
        "a9.max",
        "The overlap in ticks where the curve is at 1 (negative leaves a gap).",
    ),
    (
        "a10",
        "Keeps notes from getting shorter (or longer) than they were written.",
    ),
    (
        "a10.mode",
        "Never Shorter only lengthens notes; Never Longer only shortens them.",
    ),
    (
        "n01",
        "Fills only short rests; longer rests are left alone or shrunk.",
    ),
    ("n01.max", "The longest rest that is filled, in ticks."),
    (
        "n01.long",
        "What happens to longer rests: they are left as they are, or shrunk to the gap below.",
    ),
    (
        "n01.gap",
        "The rest left before the next note when a longer rest is shrunk, in ticks.",
    ),
    (
        "n02",
        "Stops notes from being extended across a beat, a bar line or a grid line.",
    ),
    (
        "n02.at",
        "The line a note may not cross: the next beat, the next bar line, or the next grid line.",
    ),
    (
        "n02.grid",
        "The grid used when the boundary is Grid (it restarts at each bar line).",
    ),
    (
        "n02.offset",
        "Moves the boundary lines earlier (negative) or later, in ticks.",
    ),
    (
        "n03",
        "Holds notes like a sustain pedal until the pedal lifts.",
    ),
    (
        "n03.at",
        "Where the pedal lifts: at every bar line, every few beats, or at each chord change.",
    ),
    (
        "n03.beats",
        "How many beats the pedal is held, used with Every N Beats.",
    ),
    (
        "n03.mode",
        "Extend the Notes makes the notes themselves longer; Write CC 64 adds sustain pedal events and leaves the notes as written.",
    ),
    (
        "n03.gap",
        "How long after each lift the pedal goes down again, in ticks (Write CC 64).",
    ),
    (
        "n04",
        "Limits how many notes may sound at once, so extensions never make the music too thick.",
    ),
    (
        "n04.most",
        "The most notes that may sound at the same time.",
    ),
    (
        "n04.over",
        "When a note would pass the limit: its extension stops there, or it keeps its written length.",
    ),
    (
        "n04.first",
        "Which notes get their extension first: the lowest voices, the highest, or the earliest notes.",
    ),
    (
        "n05",
        "Fills a rest from both sides: the note before is extended and the note after starts earlier, meeting in the middle.",
    ),
    (
        "n05.split",
        "Where in the rest the two notes meet (0 % = at the earlier note's end, 100 % = at the later note's start).",
    ),
    (
        "n05.most",
        "The farthest a note's start may move earlier, in ticks.",
    ),
    (
        "n06",
        "Leaves a short gap before a note when the same key plays again, so it is heard again.",
    ),
    (
        "n06.gap",
        "How long the gap before the repeated key is, in ticks.",
    ),
    (
        "n06.key",
        "What counts as the same key: only the pitch, the pitch and the MIDI channel, or the pitch and the subchannel.",
    ),
    (
        "n07",
        "Ties a chord tone that the next chord repeats, so it holds instead of playing again.",
    ),
    (
        "n07.tie",
        "Merge Into One Note joins the two (with the louder velocity); the other choice holds the first note and removes the repeat.",
    ),
    (
        "n07.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
    (
        "n08",
        "Extends only notes of certain lengths; the others keep their written length.",
    ),
    (
        "n08.short",
        "Only notes shorter than this are extended, in ticks.",
    ),
    (
        "n08.long",
        "Only notes longer than this are extended, in ticks (0 = no lower limit).",
    ),
    (
        "n09",
        "Makes a single legato line from the top or bottom voice of the music.",
    ),
    (
        "n09.voice",
        "The voice that becomes the legato line: the highest notes or the lowest.",
    ),
    (
        "n09.others",
        "What happens to the other voices: kept as they are, shortened to the gap before the next note, or removed.",
    ),
    (
        "n09.gap",
        "The gap left before the next note when the other voices are shortened, in ticks.",
    ),
    (
        "n10",
        "Connects each note to the next note above or below it, instead of simply the next note.",
    ),
    (
        "n10.to",
        "Next Higher connects to the next higher note; Next Lower to the next lower; Same Direction keeps going the way the line moved.",
    ),
    ("n10.range", "How many semitones away the next note may be."),
    (
        "n10.window",
        "How far ahead to look for the next note, in ticks.",
    ),
    (
        "n11",
        "Holds each note over several following note starts instead of just to the next one.",
    ),
    ("n11.n", "How many note starts each note holds over."),
    (
        "n11.of",
        "Whose note starts are counted: the note's track, its subchannel, or all selected notes.",
    ),
];

fn legato_spec() -> ToolSpec {
    let mut p = vec![
        flag("context", "Unselected notes count as next onsets", false),
        adv_switch(1, "Exact touch"),
        adv_switch(2, "Fixed overlap"),
        ticks("a2.ticks", "Overlap", 1, 1 << 40, 30).adv(2),
        adv_switch(3, "Fixed gap"),
        ticks("a3.ticks", "Gap", 1, 1 << 40, 30).adv(3),
        adv_switch(4, "Percentage overlap"),
        float(
            "a4.pct",
            "Overlap (of the distance to the next onset)",
            -90.0,
            200.0,
            25.0,
            "%",
        )
        .adv(4),
        adv_switch(5, "Chord-aware legato"),
        int("a5.min", "Smallest chord", 2, 16, 2).adv(5),
        adv_switch(6, "Same-pitch-only legato"),
        adv_switch(7, "Nearest-voice legato"),
        int("a7.max", "Largest voice step (semitones)", 0, 24, 4).adv(7),
        adv_switch(8, "Subchannel-isolated legato"),
        adv_switch(9, "Overlap envelope"),
        curve(
            "a9.curve",
            "Overlap across the selection",
            Curve::line(0.0, 1.0),
        )
        .overshoot()
        .adv(9),
        int("a9.min", "Overlap at 0", -1_000_000, 1_000_000, -60).adv(9),
        int("a9.max", "Overlap at 1", -1_000_000, 1_000_000, 120).adv(9),
        adv_switch(10, "Preserve-original-end constraint"),
        choice(
            "a10.mode",
            "Constraint",
            &["Never shorter than written", "Never longer than written"],
            0,
        )
        .adv(10),
        // V09-S36: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Gap threshold", false),
        ticks("n01.max", "Largest rest filled", 0, 1 << 40, 480),
        choice("n01.long", "Longer rests", LEG_LONG_RESTS, 0),
        ticks("n01.gap", "Gap", 0, 1 << 40, 120),
        flag("n02", "Metric boundary", false),
        choice("n02.at", "Boundary", LEG_BOUNDARIES, 0),
        choice("n02.grid", "Grid", art_grid_values(), 3),
        int("n02.offset", "Offset (ticks)", -960, 960, 0),
        flag("n03", "Pedal emulation", false),
        choice("n03.at", "Release at", LEG_RELEASES, 0),
        int("n03.beats", "Beats", 1, 32, 4),
        choice("n03.mode", "Pedal mode", LEG_PEDAL_MODES, 0),
        ticks("n03.gap", "Re-pedal gap", 0, 1 << 30, 30),
        flag("n04", "Polyphony-bounded legato", false),
        int("n04.most", "Most sounding notes", 1, 1_000_000, 16),
        choice("n04.over", "Over the limit", LEG_OVER, 0),
        choice("n04.first", "Extend first", LEG_FIRST, 0),
        flag("n05", "Meet in the middle", false),
        float("n05.split", "Split at", 0.0, 100.0, 50.0, "%"),
        ticks("n05.most", "Largest start move", 0, 1 << 30, 240),
        flag("n06", "Retrigger gap", false),
        ticks("n06.gap", "Retrigger gap", 0, 1 << 30, 20),
        choice("n06.key", "Same key means", LEG_SAME_KEY, 0),
        flag("n07", "Common-tone ties", false),
        choice("n07.tie", "Tie", LEG_TIES, 0),
        ticks("n07.tol", "Chord tolerance", 0, 1 << 30, 0),
        flag("n08", "Length-selective legato", false),
        ticks("n08.short", "Only notes shorter than", 1, 1 << 40, 480),
        ticks("n08.long", "Only notes longer than", 0, 1 << 40, 0),
        flag("n09", "Mono line", false),
        choice("n09.voice", "Voice", LEG_VOICES, 0),
        choice("n09.others", "Others", LEG_OTHERS, 1),
        ticks("n09.gap", "Gap", 0, 1 << 30, 60),
        flag("n10", "Directional legato", false),
        choice("n10.to", "Connect to", LEG_DIRECTIONS, 0),
        int("n10.range", "Range (semitones)", 1, 48, 7),
        ticks("n10.window", "Search window", 1, 1 << 40, 3840),
        flag("n11", "Hold for N onsets", false),
        int("n11.n", "Hold over (onsets)", 1, 64, 2),
        choice("n11.of", "Count onsets of", LEG_COUNT_OF, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, LEGATO_HELP);
    ToolSpec {
        id: "legato",
        name: "Legato",
        family: Family::Editor,
        summary: "Extends notes to the next onset (filling gaps); advanced modes set exact touch, overlaps, gaps, chords, voices and subchannels.",
        params: p,
        advanced: vec![
            "Exact touch",
            "Fixed overlap",
            "Fixed gap",
            "Percentage overlap",
            "Chord-aware legato",
            "Same-pitch-only legato",
            "Nearest-voice legato",
            "Subchannel-isolated legato",
            "Overlap envelope",
            "Preserve-original-end constraint",
        ],
        // Orders = the run order (docs/TRANSFORM_ENGINE.md "Legato"): the
        // onset each note connects to (n11, n10, n02, n03), which notes
        // connect (n08, n09), the rests (n01, n05), then ties (n07), the
        // polyphony bound (n04) and the retrigger gap (n06).
        functions: [
            ("n01", "Gap threshold", 170),
            ("n02", "Metric boundary", 130),
            ("n03", "Pedal emulation", 140),
            ("n04", "Polyphony-bounded legato", 200),
            ("n05", "Meet in the middle", 180),
            ("n06", "Retrigger gap", 510),
            ("n07", "Common-tone ties", 190),
            ("n08", "Length-selective legato", 150),
            ("n09", "Mono line", 160),
            ("n10", "Directional legato", 120),
            ("n11", "Hold for N onsets", 110),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: true,
    }
}

/// A next-onset key ([`LegV08::key`]) and the onset's tick.
type LegOnset = ((u16, u32, u8), u64);

/// Legato's v0.8 rules (the base switch and the frozen functions), shared
/// by the v0.8 kernel and the v0.9 pipeline.
#[derive(Clone)]
struct LegV08 {
    use_ctx: bool,
    exact: bool,
    overlap: Option<i128>,
    gap: Option<i128>,
    pct: Option<f64>,
    chord_min: Option<usize>,
    same_pitch: bool,
    voice: Option<i64>,
    by_sub: bool,
    env: Option<(Curve, i64, i64)>,
    constraint: Option<usize>,
    stats: super::super::SourceStats,
}

impl LegV08 {
    fn from(p: &Params, stats: &super::super::SourceStats) -> LegV08 {
        LegV08 {
            use_ctx: p.flag("context"),
            exact: p.adv(1),
            overlap: p.adv(2).then(|| p.ticks("a2.ticks") as i128),
            gap: p.adv(3).then(|| p.ticks("a3.ticks") as i128),
            pct: p.adv(4).then(|| p.float("a4.pct") / 100.0),
            chord_min: p.adv(5).then(|| p.int("a5.min") as usize),
            same_pitch: p.adv(6),
            voice: p.adv(7).then(|| p.int("a7.max")),
            by_sub: p.adv(8),
            env: p
                .adv(9)
                .then(|| (p.curve("a9.curve"), p.int("a9.min"), p.int("a9.max"))),
            constraint: p.adv(10).then(|| p.choice("a10.mode")),
            stats: stats.clone(),
        }
    }

    /// The next-onset key: the track, its subchannel ([a8]), its pitch
    /// ([a6]).
    fn key(&self, n: &TNote) -> (u16, u32, u8) {
        (
            n.track,
            if self.by_sub { n.sub } else { 0 },
            if self.same_pitch { n.pitch } else { 0 },
        )
    }

    /// Each note's next onset (the v0.8 rule: by key, then [a5]'s chords or
    /// [a7]'s voices).
    fn next(&self, notes: &[TNote], ctx: &[TNote]) -> Vec<Option<u64>> {
        let mut next = super::next_onsets(notes, ctx, |n| self.key(n));
        if let Some(min) = self.chord_min {
            // Next onset = the next onset group with at least `min` notes.
            let all: Vec<&TNote> = notes.iter().chain(ctx.iter()).collect();
            let mut starts: Vec<(u16, u64)> = all.iter().map(|n| (n.track, n.start)).collect();
            starts.sort_unstable();
            let mut chords: Vec<(u16, u64)> = Vec::new();
            let mut i = 0;
            while i < starts.len() {
                let mut j = i;
                while j < starts.len() && starts[j] == starts[i] {
                    j += 1;
                }
                if j - i >= min {
                    chords.push(starts[i]);
                }
                i = j;
            }
            for (k, n) in notes.iter().enumerate() {
                let at = chords.partition_point(|c| *c <= (n.track, n.start));
                next[k] = chords.get(at).filter(|c| c.0 == n.track).map(|c| c.1);
            }
        }
        if let Some(max) = self.voice {
            // The next onset group (same track) holding a note within `max`
            // semitones; look ahead at most 64 groups.
            let all: Vec<TNote> = {
                let mut v: Vec<TNote> = notes.iter().chain(ctx.iter()).copied().collect();
                v.sort_by_key(|n| (n.track, n.start, n.pitch));
                v
            };
            for (k, n) in notes.iter().enumerate() {
                let mut at = all.partition_point(|x| (x.track, x.start) <= (n.track, n.start));
                let mut groups = 0;
                next[k] = None;
                while at < all.len() && all[at].track == n.track && groups < 64 {
                    let s = all[at].start;
                    let mut hit = false;
                    while at < all.len() && all[at].track == n.track && all[at].start == s {
                        if (i64::from(all[at].pitch) - i64::from(n.pitch)).abs() <= max {
                            hit = true;
                        }
                        at += 1;
                    }
                    if hit {
                        next[k] = Some(s);
                        break;
                    }
                    groups += 1;
                }
            }
        }
        next
    }

    /// The end of note `n` connecting to `next` (v0.8; before the [a10]
    /// constraint): to the onset (positioned by [a1] – [a4], [a9]) or, with
    /// none of those, never shorter than written.
    #[inline]
    fn end(&self, n: &TNote, next: Option<u64>) -> i128 {
        let start = i128::from(n.start);
        let orig_end = start + i128::from(n.dur);
        match next {
            None => orig_end,
            Some(nx) => {
                let nx = i128::from(nx);
                let positioned = self.exact
                    || self.overlap.is_some()
                    || self.gap.is_some()
                    || self.pct.is_some()
                    || self.env.is_some()
                    || self.chord_min.is_some()
                    || self.voice.is_some();
                let mut e = if positioned { nx } else { orig_end.max(nx) };
                if let Some(o) = self.overlap {
                    e = nx + o;
                }
                if let Some(g) = self.gap {
                    e = nx - g;
                }
                if let Some(pc) = self.pct {
                    e = nx + ((nx - start) as f64 * pc).round() as i128;
                }
                if let Some((c, lo, hi)) = &self.env {
                    let w = c.eval(pos(n, &self.stats));
                    e = nx + (*lo as f64 + (*hi - *lo) as f64 * w).round() as i128;
                }
                e
            }
        }
    }

    /// [a10]: never shorter / never longer than written.
    #[inline]
    fn constrain(&self, end: i128, orig_end: i128) -> i128 {
        match self.constraint {
            Some(0) => end.max(orig_end),
            Some(_) => end.min(orig_end),
            None => end,
        }
    }
}

/// Legato's v0.9 functions declared by id (TOOL_CATALOG `legato` n01 …
/// n11). `None` = switched off; with every one off the kernel is the v0.8
/// one.
#[derive(Clone, Default)]
struct LegNew {
    /// n01: largest rest filled, shrink longer rests (else leave them), gap.
    rests: Option<(u64, bool, u64)>,
    /// n02: boundary (`LEG_BOUNDARIES`), grid ticks, offset.
    metric: Option<(usize, u64, i64)>,
    /// n03: release at (`LEG_RELEASES`), beats, write CC 64 (else extend),
    /// re-pedal gap.
    pedal: Option<(usize, u64, bool, u64)>,
    /// n04: most sounding notes, stop the extension (else keep the written
    /// length), extend first (`LEG_FIRST`).
    poly: Option<(u32, bool, usize)>,
    /// n05: split (fraction of the rest), largest start move.
    meet: Option<(f64, u64)>,
    /// n06: retrigger gap, same key (`LEG_SAME_KEY`).
    retrigger: Option<(u64, usize)>,
    /// n07: hold the first (else merge), chord tolerance.
    ties: Option<(bool, u64)>,
    /// n08: only notes shorter than, only notes longer than.
    lengths: Option<(u64, u64)>,
    /// n09: bottom voice (else top), others (`LEG_OTHERS`), gap.
    mono: Option<(bool, usize, u64)>,
    /// n10: connect to (`LEG_DIRECTIONS`), range, search window.
    direction: Option<(usize, u8, u64)>,
    /// n11: onsets, count onsets of (`LEG_COUNT_OF`).
    hold: Option<(u64, usize)>,
}

impl LegNew {
    fn from(p: &Params, ctx: &Ctx) -> LegNew {
        LegNew {
            rests: p.flag("n01").then(|| {
                (
                    p.ticks("n01.max"),
                    p.choice("n01.long") == 1,
                    p.ticks("n01.gap"),
                )
            }),
            metric: p.flag("n02").then(|| {
                (
                    p.choice("n02.at"),
                    note_value(
                        p.choice("n02.grid").min(art_grid_values().len() - 1),
                        1,
                        ctx,
                    ),
                    p.int("n02.offset"),
                )
            }),
            pedal: p.flag("n03").then(|| {
                (
                    p.choice("n03.at"),
                    p.int("n03.beats").clamp(1, 32) as u64,
                    p.choice("n03.mode") == 1,
                    p.ticks("n03.gap"),
                )
            }),
            poly: p.flag("n04").then(|| {
                (
                    p.int("n04.most").clamp(1, 1_000_000) as u32,
                    p.choice("n04.over") == 0,
                    p.choice("n04.first"),
                )
            }),
            meet: p.flag("n05").then(|| {
                (
                    (p.float("n05.split") / 100.0).clamp(0.0, 1.0),
                    p.ticks("n05.most"),
                )
            }),
            retrigger: p
                .flag("n06")
                .then(|| (p.ticks("n06.gap"), p.choice("n06.key"))),
            ties: p
                .flag("n07")
                .then(|| (p.choice("n07.tie") == 1, p.ticks("n07.tol"))),
            lengths: p
                .flag("n08")
                .then(|| (p.ticks("n08.short"), p.ticks("n08.long"))),
            mono: p.flag("n09").then(|| {
                (
                    p.choice("n09.voice") == 1,
                    p.choice("n09.others"),
                    p.ticks("n09.gap"),
                )
            }),
            direction: p.flag("n10").then(|| {
                (
                    p.choice("n10.to"),
                    p.int("n10.range").clamp(1, 48) as u8,
                    p.ticks("n10.window"),
                )
            }),
            hold: p
                .flag("n11")
                .then(|| (p.int("n11.n").clamp(1, 64) as u64, p.choice("n11.of"))),
        }
    }

    fn any(&self) -> bool {
        self.rests.is_some()
            || self.metric.is_some()
            || self.pedal.is_some()
            || self.poly.is_some()
            || self.meet.is_some()
            || self.retrigger.is_some()
            || self.ties.is_some()
            || self.lengths.is_some()
            || self.mono.is_some()
            || self.direction.is_some()
            || self.hold.is_some()
    }

    /// Whether notes can be removed (n07's repeats, n09's other voices).
    fn thins(&self) -> bool {
        self.ties.is_some() || self.mono.is_some_and(|m| m.1 == 2)
    }
}

/// n03: where the pedal lifts.
enum Pedal {
    /// Every bar line.
    Bars,
    /// Every `n` beats from the selection's first beat (`first`; bars
    /// without musical timing).
    Beats { n: u64, first: Option<u64> },
    /// Per track, the starts of chords whose pitch classes differ from the
    /// chord before (ascending).
    Chords(Vec<Vec<u64>>),
}

struct Legato {
    v08: LegV08,
    new: LegNew,
    ctx: Ctx,
    /// With Control Sweeps (E0422): what each level's settings are built
    /// from.
    swept: Option<std::sync::Arc<Rebuild>>,
}

/// n11: each note's N-th following onset (distinct start) of its track, its
/// subchannel or all the notes.
fn hold_next(
    notes: &[TNote],
    ctx: &[TNote],
    count: impl Fn(usize) -> u64,
    of: usize,
) -> Vec<Option<u64>> {
    let key = |n: &TNote| match of {
        0 => (n.track, 0u32),
        1 => (n.track, n.sub),
        _ => (0, 0),
    };
    let mut all: Vec<((u16, u32), u64)> = notes
        .iter()
        .chain(ctx.iter())
        .map(|n| (key(n), n.start))
        .collect();
    all.sort_unstable();
    all.dedup();
    notes
        .iter()
        .enumerate()
        .map(|(q, n)| {
            let k = key(n);
            let i = all.partition_point(|x| *x <= (k, n.start));
            let j = i.saturating_add(count(q).max(1) as usize - 1);
            all.get(j).filter(|x| x.0 == k).map(|x| x.1)
        })
        .collect()
}

/// n10: each note's next onset of its track holding a key above (below, or
/// continuing the step from the previous onset's nearest note — either way
/// for the first onset and repeats) within `range`, at most `window` ticks
/// after its start. A backward sweep over the onsets with each key's
/// earliest later start: O(n × range).
fn directional_next(
    notes: &[TNote],
    ctx: &[TNote],
    to: usize,
    range: impl Fn(usize) -> u8,
    window: u64,
) -> Vec<Option<u64>> {
    let mut out = vec![None; notes.len()];
    // (track, start, pitch, note index or MAX for context).
    let mut all: Vec<(u16, u64, u8, usize)> = notes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.track, n.start, n.pitch, i))
        .chain(ctx.iter().map(|n| (n.track, n.start, n.pitch, usize::MAX)))
        .collect();
    all.sort_unstable();
    let mut later = [u64::MAX; 256];
    let mut a = 0;
    while a < all.len() {
        // One track: its onset groups, then the backward sweep.
        let track = all[a].0;
        let mut z = a;
        while z < all.len() && all[z].0 == track {
            z += 1;
        }
        let mut bounds = vec![a];
        for i in a + 1..z {
            if all[i].1 != all[i - 1].1 {
                bounds.push(i);
            }
        }
        bounds.push(z);
        later.fill(u64::MAX);
        for g in (0..bounds.len() - 1).rev() {
            let (g0, g1) = (bounds[g], bounds[g + 1]);
            let prev = (g > 0).then(|| &all[bounds[g - 1]..g0]);
            for &(_, s, p, i) in &all[g0..g1] {
                if i == usize::MAX {
                    continue;
                }
                let p = i64::from(p);
                let range = i64::from(range(i));
                let dir = match to {
                    DIRECTION_UP => 1,
                    DIRECTION_DOWN => -1,
                    _ => prev.map_or(0, |pg| {
                        // The previous onset's nearest note (the lower on a tie).
                        let q = pg
                            .iter()
                            .map(|x| i64::from(x.2))
                            .min_by_key(|&q| (q.abs_diff(p), q))
                            .unwrap_or(p);
                        (p - q).signum()
                    }),
                };
                let (lo, hi) = match dir {
                    1 => (p + 1, p + range),
                    -1 => (p - range, p - 1),
                    _ => (p - range, p + range),
                };
                let best = (lo.max(0)..=hi.min(255))
                    .filter(|&q| q != p || dir != 0)
                    .map(|q| later[q as usize])
                    .min()
                    .unwrap_or(u64::MAX);
                if best != u64::MAX && best - s <= window {
                    out[i] = Some(best);
                }
            }
            for &(_, s, p, _) in &all[g0..g1] {
                later[usize::from(p)] = s;
            }
        }
        a = z;
    }
    out
}

impl Legato {
    /// n02: the first beat line, bar line or grid line (restarting at bar
    /// lines) after `s`.
    fn boundary(&self, s: u64, at: usize, grid: u64) -> u64 {
        let meter = &self.ctx.meter;
        let (bs, bl) = bar_bounds(meter, s);
        let bar_end = bs.saturating_add(bl);
        match at {
            BOUNDARY_BEAT => meter
                .beat_at(s)
                .and_then(|(i, _)| meter.beat_start(i + 1))
                .filter(|&t| t > s)
                .unwrap_or(bar_end),
            BOUNDARY_BAR => bar_end,
            _ => {
                let g = grid.max(1);
                bs.saturating_add(((s - bs) / g + 1).saturating_mul(g))
                    .min(bar_end)
            }
        }
    }

    /// n03: where the pedal lifts.
    fn pedal(&self, src: &Source, at: usize, beats: u64) -> Pedal {
        match at {
            RELEASE_BEATS => Pedal::Beats {
                n: beats.max(1),
                first: self.ctx.meter.beat_at(src.stats.min_start).map(|(i, _)| i),
            },
            RELEASE_CHORDS => {
                let mut lists = Vec::with_capacity(src.tracks.len());
                for idx in per_track(src) {
                    let mut out = Vec::new();
                    let mut prev: Option<u16> = None;
                    for chord in ChordGroups::of(&src.notes, &idx, 0).iter() {
                        let mask = chord
                            .iter()
                            .fold(0u16, |m, &i| m | 1 << (src.notes[i].pitch % 12));
                        if prev.is_some_and(|p| p != mask) {
                            out.push(src.notes[chord[0]].start);
                        }
                        prev = Some(mask);
                    }
                    lists.push(out);
                }
                Pedal::Chords(lists)
            }
            _ => Pedal::Bars,
        }
    }

    /// n03: the first release after `s` on `track` (the selection's latest
    /// end when none comes before it).
    fn release_after(&self, pedal: &Pedal, track: u16, s: u64, last: u64) -> u64 {
        let meter = &self.ctx.meter;
        let bar = || {
            let (bs, bl) = bar_bounds(meter, s);
            bs.saturating_add(bl)
        };
        let r = match pedal {
            Pedal::Bars => bar(),
            Pedal::Beats { n, first } => match (first, meter.beat_at(s)) {
                (Some(f), Some((i, _))) => {
                    let k = i.saturating_sub(*f) / n + 1;
                    meter
                        .beat_start(f.saturating_add(k.saturating_mul(*n)))
                        .unwrap_or_else(bar)
                }
                _ => bar(),
            },
            Pedal::Chords(lists) => lists
                .get(usize::from(track))
                .and_then(|l| l.get(l.partition_point(|&x| x <= s)).copied())
                .unwrap_or(last),
        };
        r.min(last).max(s)
    }

    /// Each note's settings under Control Sweeps (E0422): the v0.8 and
    /// v0.9 settings of its level; `None` without sweeps.
    fn levels(&self, src: &Source) -> Option<PerLevel<(LegV08, LegNew)>> {
        let r = self.swept.as_ref()?;
        Some(r.levels(&self.ctx, src, |p, b| {
            (LegV08::from(p, b.stats), LegNew::from(p, b.ctx))
        }))
    }

    /// The v0.9 pipeline over the whole source.
    fn run9(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        match self.levels(src) {
            // Note k's settings: its level's (E0422).
            Some(l) => self.run9_at(src, out, |k| {
                let c = l.at(k);
                (&c.0, &c.1)
            }),
            // Without sweeps every note reads the kernel's own settings.
            None => self.run9_at(src, out, |_| (&self.v08, &self.new)),
        }
    }

    /// [`Legato::run9`] with note `k`'s settings `at(k)`.
    #[inline]
    fn run9_at<'a>(
        &'a self,
        src: &Source,
        out: &mut Output,
        at: impl Fn(usize) -> (&'a LegV08, &'a LegNew),
    ) -> Result<(), TransformError> {
        let (v, w) = (&self.v08, &self.new);
        let notes = &src.notes[..];
        let n = notes.len();
        let ctx: &[TNote] = if v.use_ctx { &src.context[..] } else { &[] };
        let last = src.stats.max_end;
        // 1. The onset each note connects to.
        let base = v.next(notes, ctx);
        let mut next = base.clone();
        if let Some((_, of)) = w.hold {
            next = hold_next(notes, ctx, |k| at(k).1.hold.map_or(1, |h| h.0), of);
        }
        if let Some((to, _, window)) = w.direction {
            next = directional_next(
                notes,
                ctx,
                to,
                |k| at(k).1.direction.map_or(1, |d| d.1),
                window,
            );
        }
        if let Some((at_b, grid, _)) = w.metric {
            for (k, nt) in notes.iter().enumerate() {
                let offset = at(k).1.metric.map_or(0, |m| m.2);
                let b = self.boundary(nt.start, at_b, grid);
                let t = next[k].map_or(b, |x| x.min(b));
                next[k] = Some(
                    (i128::from(t) + i128::from(offset)).clamp(0, i128::from(u64::MAX)) as u64,
                );
            }
        }
        let pedal = w
            .pedal
            .map(|(at_r, beats, cc, gap)| (self.pedal(src, at_r, beats), cc, gap));
        if let Some((pd, false, _)) = &pedal {
            for (k, nt) in notes.iter().enumerate() {
                let gap = at(k).1.pedal.map_or(0, |p| p.3);
                let r = self.release_after(pd, nt.track, nt.start, last);
                next[k] = Some(r.saturating_sub(gap).max(nt.start));
            }
        }
        // 2. Which notes connect.
        let mut role = vec![ROLE_LEGATO; n];
        if w.lengths.is_some() {
            for (k, nt) in notes.iter().enumerate() {
                let (short, long) = at(k).1.lengths.unwrap_or((0, 0));
                if !(nt.dur < short && nt.dur > long) {
                    role[k] = ROLE_WRITTEN;
                }
            }
        }
        if let Some((bottom, others, _)) = w.mono {
            for idx in per_track(src) {
                for chord in ChordGroups::of(notes, &idx, 0).iter() {
                    let line = if bottom {
                        chord.iter().min_by_key(|&&i| (notes[i].pitch, i))
                    } else {
                        chord
                            .iter()
                            .max_by_key(|&&i| (notes[i].pitch, std::cmp::Reverse(i)))
                    };
                    for &i in chord {
                        if Some(&i) != line {
                            role[i] = match others {
                                0 => ROLE_WRITTEN,
                                1 => ROLE_SHORTEN,
                                _ => ROLE_REMOVED,
                            };
                        }
                    }
                }
            }
        }
        // 3. The rests: n01 decides long ones, n05 meets in the middle.
        let mut start: Vec<u64> = notes.iter().map(|x| x.start).collect();
        let mut fixed: Vec<Option<i128>> = vec![None; n];
        if let Some((_, shrink, gap)) = w.rests {
            for (k, nt) in notes.iter().enumerate() {
                if role[k] != ROLE_LEGATO {
                    continue;
                }
                let max = at(k).1.rests.map_or(0, |r| r.0);
                if let Some(t) = next[k] {
                    let (e0, t) = (i128::from(nt.end()), i128::from(t));
                    if t - e0 > i128::from(max) {
                        fixed[k] = Some(if shrink {
                            e0.max(t - i128::from(gap))
                        } else {
                            e0
                        });
                    }
                }
            }
        }
        let mut target = next.clone();
        if let Some((_, most)) = w.meet {
            self.meet(
                notes,
                &base,
                &next,
                &role,
                &fixed,
                |k| at(k).1.meet.map_or(0.5, |m| m.0),
                most,
                &mut start,
                &mut target,
            );
        }
        let mut end: Vec<i128> = Vec::with_capacity(n);
        for (k, nt) in notes.iter().enumerate() {
            let e0 = i128::from(nt.end());
            end.push(match role[k] {
                ROLE_LEGATO => {
                    let vk = at(k).0;
                    let e = fixed[k].unwrap_or_else(|| vk.end(nt, target[k]));
                    vk.constrain(e, e0)
                }
                ROLE_SHORTEN => {
                    let gap = i128::from(w.mono.map_or(0, |m| m.2));
                    match next[k] {
                        Some(t) => e0.min(i128::from(t) - gap).max(i128::from(nt.start) + 1),
                        None => e0,
                    }
                }
                _ => e0,
            });
        }
        // 4. Ties, the polyphony bound, the retrigger gap.
        let mut vel: Vec<u8> = notes.iter().map(|x| x.vel).collect();
        if let Some((hold_first, tol)) = w.ties {
            self.tie(src, &mut role, &start, &mut end, &mut vel, hold_first, tol);
        }
        if let Some((_, partial, first)) = w.poly {
            self.bound(
                src,
                &role,
                &start,
                &mut end,
                |k| at(k).1.poly.map_or(1, |p| p.0),
                partial,
                first,
            );
        }
        if let Some((_, same)) = w.retrigger {
            self.retrigger(
                src,
                &role,
                &start,
                &mut end,
                |k| at(k).1.retrigger.map_or(0, |r| r.0),
                same,
            );
        }
        for (k, nt) in notes.iter().enumerate() {
            if role[k] == ROLE_REMOVED {
                continue;
            }
            let s = i128::from(start[k]);
            out.emit(s, end[k] - s, i64::from(nt.pitch), i64::from(vel[k]), nt);
            if k & 0xFFF == 0xFFF {
                out.check()?;
            }
        }
        out.check()?;
        if let Some((pd, true, gap)) = &pedal {
            self.pedal_events(src, &role, pd, *gap, out)?;
        }
        Ok(())
    }

    /// n05: per key and next onset S, the notes connecting to S (still the
    /// v0.8 onset, not decided by n01) whose latest written end E leaves a
    /// rest: they meet the notes starting at S (selected, connecting) at
    /// `max(E + split·rest, S − most)` — those start there, these end there.
    #[allow(clippy::too_many_arguments)]
    fn meet(
        &self,
        notes: &[TNote],
        base: &[Option<u64>],
        next: &[Option<u64>],
        role: &[u8],
        fixed: &[Option<i128>],
        split: impl Fn(usize) -> f64,
        most: u64,
        start: &mut [u64],
        target: &mut [Option<u64>],
    ) {
        use std::collections::HashMap;
        let v = &self.v08;
        let eligible =
            |k: usize| role[k] == ROLE_LEGATO && fixed[k].is_none() && next[k] == base[k];
        let movers: std::collections::HashSet<((u16, u32, u8), u64)> = notes
            .iter()
            .enumerate()
            .filter(|(k, _)| role[*k] == ROLE_LEGATO)
            .map(|(_, nt)| (v.key(nt), nt.start))
            .collect();
        // Per key and onset: the latest written end and (with a sweep) the
        // note holding it, whose split is used.
        let mut latest: HashMap<LegOnset, (u64, usize)> = HashMap::new();
        for (k, nt) in notes.iter().enumerate() {
            if let Some(s) = next[k]
                && eligible(k)
                && movers.contains(&(v.key(nt), s))
            {
                let e = latest.entry((v.key(nt), s)).or_insert((nt.end(), k));
                if nt.end() > e.0 {
                    *e = (nt.end(), k);
                }
            }
        }
        let mut meet_at: HashMap<((u16, u32, u8), u64), u64> = HashMap::new();
        for (&(key, s), &(e, by)) in &latest {
            if e < s {
                let m = e + ((s - e) as f64 * split(by)).round() as u64;
                meet_at.insert((key, s), m.max(s.saturating_sub(most)).min(s));
            }
        }
        for (k, nt) in notes.iter().enumerate() {
            if let Some(s) = next[k]
                && eligible(k)
                && let Some(&m) = meet_at.get(&(v.key(nt), s))
            {
                target[k] = Some(m);
            }
        }
        for (k, nt) in notes.iter().enumerate() {
            if role[k] == ROLE_LEGATO
                && let Some(&m) = meet_at.get(&(v.key(nt), nt.start))
            {
                start[k] = m;
            }
        }
    }

    /// n07: per track, a tone of a chord (`chord_groups`, the tolerance)
    /// repeated by the next chord while it still sounds is tied: the first
    /// note lasts to the repeat's end and the repeat goes — merged with the
    /// loudest velocity of the tie, or held at the first's.
    #[allow(clippy::too_many_arguments)]
    fn tie(
        &self,
        src: &Source,
        role: &mut [u8],
        start: &[u64],
        end: &mut [i128],
        vel: &mut [u8],
        hold_first: bool,
        tol: u64,
    ) {
        let notes = &src.notes;
        let mut prev: Vec<(u8, usize)> = Vec::new();
        let mut cur: Vec<(u8, usize)> = Vec::new();
        for idx in per_track(src) {
            let idx: Vec<usize> = idx
                .into_iter()
                .filter(|&k| role[k] == ROLE_LEGATO)
                .collect();
            prev.clear();
            for chord in ChordGroups::of(notes, &idx, tol).iter() {
                cur.clear();
                cur.extend(chord.iter().map(|&k| (notes[k].pitch, k)));
                cur.sort_unstable();
                let mut j = 0;
                for c in cur.iter_mut() {
                    while j < prev.len() && prev[j].0 < c.0 {
                        j += 1;
                    }
                    if j < prev.len() && prev[j].0 == c.0 {
                        let head = prev[j].1;
                        let k = c.1;
                        if end[head] >= i128::from(start[k]) {
                            end[head] = end[head].max(end[k]);
                            if !hold_first {
                                vel[head] = vel[head].max(vel[k]);
                            }
                            role[k] = ROLE_REMOVED;
                            c.1 = head;
                        }
                        // One tie per tone: a doubled key of the chord stays.
                        j += 1;
                    }
                }
                std::mem::swap(&mut prev, &mut cur);
            }
        }
    }

    /// n04: the extensions beyond the written ends (`density_window::
    /// capped_extensions`) granted in the chosen order while at most `cap`
    /// notes — written parts and the unselected notes included — sound.
    #[allow(clippy::too_many_arguments)]
    fn bound(
        &self,
        src: &Source,
        role: &[u8],
        start: &[u64],
        end: &mut [i128],
        cap: impl Fn(usize) -> u32,
        partial: bool,
        first: usize,
    ) {
        let notes = &src.notes;
        let clamp = |e: i128| e.clamp(0, i128::from(u64::MAX)) as u64;
        let mut base: Vec<(u64, u64)> = Vec::with_capacity(notes.len() + src.context.len());
        let mut ext: Vec<(u64, u64)> = Vec::new();
        let mut who: Vec<usize> = Vec::new();
        for (k, nt) in notes.iter().enumerate() {
            if role[k] == ROLE_REMOVED {
                continue;
            }
            let e = clamp(end[k]);
            base.push((start[k], e.min(nt.end())));
            if e > nt.end() {
                ext.push((nt.end(), e));
                who.push(k);
            }
        }
        base.extend(src.context.iter().map(|c| (c.start, c.end())));
        let mut order: Vec<usize> = (0..who.len()).collect();
        match first {
            FIRST_LOWEST => {
                order.sort_unstable_by_key(|&j| (notes[who[j]].pitch, notes[who[j]].start, j))
            }
            FIRST_HIGHEST => order.sort_unstable_by_key(|&j| {
                (
                    std::cmp::Reverse(notes[who[j]].pitch),
                    notes[who[j]].start,
                    j,
                )
            }),
            _ => order.sort_unstable_by_key(|&j| (notes[who[j]].start, notes[who[j]].pitch, j)),
        }
        let granted = super::super::shared::density_window::capped_extensions_by(
            &base,
            &ext,
            &order,
            |j| cap(who[j]),
            partial,
        );
        for (j, &k) in who.iter().enumerate() {
            end[k] = i128::from(granted[j]);
        }
    }

    /// n06: a connecting note reaching the next start of the same key (its
    /// track and key, plus its channel or subchannel) ends the gap before
    /// it (at least one tick long); unselected notes count. One sort by
    /// (packed key, start) and a backward pass giving every note the next
    /// later start of its key.
    fn retrigger(
        &self,
        src: &Source,
        role: &[u8],
        start: &[u64],
        end: &mut [i128],
        gap: impl Fn(usize) -> u64,
        same: usize,
    ) {
        let key = |n: &TNote| {
            u64::from(n.track) << 40
                | u64::from(n.pitch) << 32
                | match same {
                    0 => 0,
                    1 => u64::from(n.ch),
                    _ => u64::from(n.sub),
                }
        };
        let notes = &src.notes;
        const CONTEXT: u32 = u32::MAX;
        let mut all: Vec<(u64, u64, u32)> = notes
            .iter()
            .enumerate()
            .filter(|(k, _)| role[*k] != ROLE_REMOVED)
            .map(|(k, nt)| (key(nt), start[k], k as u32))
            .chain(src.context.iter().map(|c| (key(c), c.start, CONTEXT)))
            .collect();
        all.sort_unstable();
        // Backward: the earliest start of the key later than the current
        // run of equal starts.
        let mut i = all.len();
        let (mut key_at, mut later): (Option<u64>, Option<u64>) = (None, None);
        while i > 0 {
            let (k, s, _) = all[i - 1];
            if key_at != Some(k) {
                key_at = Some(k);
                later = None;
            }
            let mut j = i;
            while j > 0 && all[j - 1].0 == k && all[j - 1].1 == s {
                j -= 1;
            }
            if let Some(ns) = later {
                for &(_, _, idx) in &all[j..i] {
                    if idx == CONTEXT || role[idx as usize] != ROLE_LEGATO {
                        continue;
                    }
                    let idx = idx as usize;
                    let limit = i128::from(ns) - i128::from(gap(idx));
                    if end[idx] > limit {
                        end[idx] = limit.max(i128::from(start[idx]) + 1);
                    }
                }
            }
            later = Some(s);
            i = j;
        }
    }

    /// n03 (CC 64): per track and channel of the notes, the pedal down at
    /// the selection's first start, up at each release and down again the
    /// re-pedal gap after it, up at the latest end. Refused (a sentence)
    /// before writing more than `MAX_EVENTS` events.
    fn pedal_events(
        &self,
        src: &Source,
        role: &[u8],
        pedal: &Pedal,
        gap: u64,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let (t0, t1) = (src.stats.min_start, src.stats.max_end);
        let mut chans: Vec<(u16, u8)> = src
            .notes
            .iter()
            .enumerate()
            .filter(|(k, _)| role[*k] != ROLE_REMOVED)
            .map(|(_, n)| (n.track, n.ch))
            .collect();
        chans.sort_unstable();
        chans.dedup();
        let budget = (MAX_EVENTS / 2 / chans.len().max(1)).max(1);
        let too_many = || {
            TransformError::TooLarge(
                "The pedal would write more than 8,000,000 CC 64 events; release less often or select fewer notes.".into(),
            )
        };
        let lists: Vec<Vec<u64>> = match pedal {
            Pedal::Chords(lists) => lists.clone(),
            _ => {
                // The release lines between the first start and the latest
                // end, counted against the budget as they are walked.
                let mut v = Vec::new();
                let mut t = t0;
                loop {
                    let r = self.release_after(pedal, 0, t, t1);
                    if r <= t || r >= t1 {
                        break;
                    }
                    if v.len() >= budget {
                        return Err(too_many());
                    }
                    v.push(r);
                    t = r;
                }
                vec![v]
            }
        };
        let total: usize = chans
            .iter()
            .map(|&(track, _)| {
                let l = if matches!(pedal, Pedal::Chords(_)) {
                    lists.get(usize::from(track)).map_or(0, Vec::len)
                } else {
                    lists[0].len()
                };
                2 * l + 2
            })
            .sum();
        if total > MAX_EVENTS {
            return Err(too_many());
        }
        let lane = Lane::Cc(64);
        let down = gap.max(1);
        for &(track, ch) in &chans {
            let list = if matches!(pedal, Pedal::Chords(_)) {
                lists.get(usize::from(track)).map_or(&[][..], |l| &l[..])
            } else {
                &lists[0][..]
            };
            out.emit_event(track, lane, ch, i128::from(t0), 127);
            for &r in list.iter().filter(|&&r| r > t0 && r < t1) {
                out.emit_event(track, lane, ch, i128::from(r), 0);
                if r.saturating_add(down) < t1 {
                    out.emit_event(track, lane, ch, i128::from(r + down), 127);
                }
            }
            out.emit_event(track, lane, ch, i128::from(t1), 0);
        }
        out.check()
    }
}

fn legato_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let v = LegV08::from(p, b.stats);
    let new = LegNew::from(p, b.ctx);
    let swept = Rebuild::of(p, b);
    if new.any() || swept.is_some() {
        // V09-S36: the v0.9 pipeline (n07 / n09 may remove notes); with
        // Control Sweeps (E0422) each note's settings come from its level,
        // the passes see the whole selection.
        let growth = if new.thins() {
            Growth::Thin
        } else {
            Growth::Map
        };
        let l = Legato {
            v08: v,
            new,
            ctx: b.ctx.clone(),
            swept,
        };
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| l.run9(src, out),
            growth,
            estimate: None,
        });
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let ctx: Vec<TNote> = if v.use_ctx {
                src.context.to_vec()
            } else {
                Vec::new()
            };
            let next = v.next(&src.notes, &ctx);
            for (k, n) in src.notes.iter().enumerate() {
                let start = i128::from(n.start);
                let orig_end = start + i128::from(n.dur);
                let end = v.constrain(v.end(n, next[k]), orig_end);
                out.emit(start, end - start, i64::from(n.pitch), i64::from(n.vel), n);
                out.check()?;
            }
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}
