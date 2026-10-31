//! Claw Machine (docs/references/v0_7/CLAW_REFERENCE.md).
//!
//! Within each period `[p0, p0 + P)` every note start and end is remapped by
//! `g_a(u) = (e^{a·u} − 1)/(e^a − 1)`, `u = (t − p0)/P`, `a = −3.93 · d/100`
//! (fitted to the owner's FL Studio video at the knob extremes; intermediate
//! values are PlumSpirit's interpolation). Trash every, stretch to compensate
//! and remove short notes follow the documented purpose with PlumSpirit rules.
//!
//! v0.9 (V09-S34, TOOL_CATALOG `claw` n01 … n11): the periods come from one
//! [`Grid`] — uniform (v0.8), a cycled sequence (n03), morphed across the
//! selection (n01), drifting (n08); the map of a period is the frozen one
//! after the onset equalizer (n10), blended toward map B (n04); n09 rescales
//! every period and closes up the timeline, n05 moves slices; n06 trashes
//! slices by what they hold, n07 refills trashed slices, n02 shapes velocity
//! and length by the map's slope, n11 keeps the dry notes beside the wet
//! ones. With every one off the kernel computes what v0.8 computed. The
//! pipeline: docs/TRANSFORM_ENGINE.md "Claw Machine".

use super::super::boundary::{self, PitchPolicy};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, note_key, salt};
use super::super::shared::seeded_stream::{salt_of, stream};
use super::super::sweep::{PerLevel, Rebuild};
use super::super::{Growth, Kernel, Output, Source, TNote, TransformError};
use super::xgeom::{sub_stepped, vel_scaled};
use super::{Contextual, boxed, warp};

pub const CLAW: ToolDef = ToolDef {
    id: "claw",
    spec: claw_spec,
    build: claw_build,
};

/// |a| at ±100 % (fitted, CLAW_REFERENCE.md).
pub const CLAW_A_MAX: f64 = 3.93;

/// Most periods one run lays out across the selection (n01 / n08 tables:
/// 16 bytes each, so at most 64 MB).
pub const MAX_PERIODS: usize = 1 << 22;

const PERIODS: &[&str] = &[
    "Beat",
    "2 beats",
    "4 beats",
    "8 beats",
    "16 beats",
    "Custom ticks",
];
const TRASH: &[&str] = &["(none)", "1/2", "1/3", "1/4", "1/6", "1/8", "1/16"];
const MORPH_SNAP: &[&str] = &["Ticks", "Grid", "Beats"];
const SEQ_UNIT: &[&str] = &["Beats", "1/16 notes", "Ticks"];
const SHUFFLE: &[&str] = &["List", "Seeded per period", "Seeded once"];
const KEEP_BY: &[&str] = &["Densest", "Sparsest", "Loudest", "Highest"];
const FILL_FROM: &[&str] = &["Previous slice", "First slice of the period", "Next slice"];
const DRIFT_MODE: &[&str] = &["Wrap in the period", "Accumulate"];
const ANCHOR: &[&str] = &["Start", "Center", "End"];
const LENGTHS: &[&str] = &["Scale", "Keep"];
const EQ_PER: &[&str] = &["Period", "Period and key", "Period and subchannel"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CLAW_HELP: &[(&str, &str)] = &[
    (
        "period",
        "The span time is warped within, repeating through the selection.",
    ),
    (
        "period.ticks",
        "For Custom Ticks: the period's length, in ticks.",
    ),
    (
        "trash",
        "Cuts each period into this many slices and removes the notes starting in the last slice of each group ((none) removes nothing).",
    ),
    (
        "dist",
        "Warps time inside each period: + pushes the notes toward the period's end (rushing), − toward its start; 0 = unchanged.",
    ),
    (
        "stretch",
        "After trashing, lengthens the remaining notes to the next remaining onset (or the period's end), so no gap is left.",
    ),
    (
        "short",
        "Removes notes made shorter than the length below by the warp.",
    ),
    (
        "short.ticks",
        "Notes shorter than this, in ticks, are removed.",
    ),
    (
        "origin",
        "Whether periods count from bar 1 of the song or from the selection's first note.",
    ),
    (
        "a1",
        "Warps each period in several stages, each part with its own distortion.",
    ),
    (
        "a1.stages",
        "The distortion of each stage in turn, in percent (+ toward the stage's end, − toward its start).",
    ),
    (
        "a2",
        "Warps time by your own drawn map instead of the distortion knob.",
    ),
    (
        "a2.curve",
        "Where each position in the period moves to (left = position, up = new position).",
    ),
    ("a3", "Warps every other period in the opposite direction."),
    ("a4", "Applies the warp again inside smaller sub-periods."),
    ("a4.parts", "How many sub-periods each period is cut into."),
    ("a4.depth", "How many levels of sub-periods."),
    ("a5", "Removes or keeps slices of each period by a pattern."),
    (
        "a5.mask",
        "Keep (1) or remove (0) for each slice in turn; the list's length is the number of slices.",
    ),
    (
        "a6",
        "Shifts the period for each subchannel, so layers warp out of step.",
    ),
    (
        "a6.phase",
        "The period shift for each subchannel in turn, as a percentage of the period.",
    ),
    (
        "a7",
        "Warps high notes more (or less) than low ones around a pivot key.",
    ),
    ("a7.pivot", "The key where the distortion is as set."),
    (
        "a7.amount",
        "How much the distortion changes per octave above the pivot (and the opposite below).",
    ),
    (
        "a8",
        "Thins notes where the warp packs them together (seeded), so density stays even.",
    ),
    (
        "a9",
        "Warps symmetrically: the first half toward the period's start, the second half mirrored toward its end.",
    ),
    (
        "a10",
        "Places the slice starts at listed positions in the period instead of a curve.",
    ),
    (
        "a10.map",
        "Where each slice start lands, in thousandths of the period (the list's length is the number of slices).",
    ),
    (
        "n01",
        "Changes the period across the selection, from the main period to an end period.",
    ),
    ("n01.end", "The period at the selection's end, in ticks."),
    (
        "n01.curve",
        "How the period moves from the main one to the end period.",
    ),
    (
        "n01.snap",
        "Round each period to whole ticks, to the snap grid, or to whole beats.",
    ),
    (
        "n02",
        "Makes the warp change velocity and length where it compresses or stretches time.",
    ),
    (
        "n02.vel",
        "How much compressed (rushed) notes get louder (+) or softer (−).",
    ),
    (
        "n02.len",
        "How much compressed notes get shorter (+) or longer (−).",
    ),
    (
        "n02.curve",
        "How strongly the effect follows the compression (left = none, right = 4× rushed).",
    ),
    ("n03", "Uses a sequence of different periods in turn."),
    (
        "n03.periods",
        "The period lengths in turn; the list repeats.",
    ),
    (
        "n03.unit",
        "Whether the periods are in beats, 1/16 notes or ticks.",
    ),
    (
        "n04",
        "Morphs the warp from the main map (A) to a second map (B) across the selection.",
    ),
    (
        "n04.map",
        "Map B: where each position in the period moves to.",
    ),
    (
        "n04.curve",
        "How far the morph has gone along the selection (0 = map A, 1 = map B).",
    ),
    ("n05", "Reorders the slices inside each period."),
    ("n05.slices", "How many slices each period is cut into."),
    (
        "n05.order",
        "The slot each slice moves to (0 = the first slot); the list's order is the slices' order.",
    ),
    (
        "n05.shuffle",
        "Use the list, shuffle each period differently (seeded), or shuffle once for all periods.",
    ),
    (
        "n06",
        "Trashes slices by what they hold: keeps the densest, sparsest, loudest or highest slices.",
    ),
    ("n06.slices", "How many slices each period is cut into."),
    ("n06.keep", "How many slices of each period are kept."),
    (
        "n06.by",
        "Which slices are kept: the densest, the sparsest, the loudest or the highest.",
    ),
    ("n07", "Refills trashed slices with copies of other slices."),
    (
        "n07.from",
        "The copy comes from the previous slice, the period's first slice, or the next slice.",
    ),
    (
        "n07.vel",
        "The refill's velocity as a share of the copied notes'.",
    ),
    ("n07.semis", "Transposes the refill by this many semitones."),
    (
        "n08",
        "Shifts the period grid a little more every period, so the warp drifts against the music.",
    ),
    ("n08.drift", "How many ticks the grid moves per period."),
    (
        "n08.mode",
        "Wrap the drift within the period, or let it accumulate.",
    ),
    (
        "n08.curve",
        "How much of the drift applies across the selection.",
    ),
    (
        "n09",
        "Rescales every period to a new length, closing up the timeline.",
    ),
    ("n09.period", "The new period length, in ticks."),
    (
        "n09.anchor",
        "The point of each period that stays in place: its start, center or end.",
    ),
    (
        "n09.lengths",
        "Scale the notes' lengths with the period, or keep them.",
    ),
    (
        "n10",
        "Evens out the note onsets inside each period, so they spread uniformly.",
    ),
    (
        "n10.strength",
        "How far the onsets move toward even spacing (100 % = fully).",
    ),
    (
        "n10.bins",
        "How finely each period is divided when measuring the onsets.",
    ),
    (
        "n10.per",
        "Equalize per period, per period and key, or per period and subchannel.",
    ),
    (
        "n11",
        "Keeps the original notes (dry) beside the warped ones (wet).",
    ),
    (
        "n11.dry",
        "The original notes' velocity, as a share of their own.",
    ),
    (
        "n11.wet",
        "The warped notes' velocity, as a share of their own.",
    ),
    (
        "n11.sub",
        "Moves the dry notes this many subchannels, so the layers can be colored apart.",
    ),
];

fn claw_spec() -> ToolSpec {
    let mut p = vec![
        choice("period", "Period", PERIODS, 2),
        ticks("period.ticks", "Custom period", 1, 1 << 40, 3840),
        choice("trash", "Trash every", TRASH, 0),
        float("dist", "Time distortion", -100.0, 100.0, 0.0, "%"),
        flag("stretch", "Stretch to compensate", false),
        flag("short", "Remove short notes", false),
        ticks("short.ticks", "Shorter than", 1, 1 << 30, 60),
        choice(
            "origin",
            "Periods start at",
            &["Bar 1 (song grid)", "The selection's start"],
            0,
        ),
        seed("seed"),
        adv_switch(1, "Multi-stage distortion curve"),
        list(
            "a1.stages",
            "Distortion % per stage",
            -100,
            100,
            4,
            &[100, -100],
        )
        .adv(1),
        adv_switch(2, "Editable distortion spline"),
        curve(
            "a2.curve",
            "Position in the period → new position",
            Curve::smooth(vec![(0.0, 0.0), (0.5, 0.15), (1.0, 1.0)]),
        )
        .adv(2),
        adv_switch(3, "Alternating direction per period"),
        adv_switch(4, "Nested period subdivision"),
        int("a4.parts", "Sub-periods", 2, 8, 2).adv(4),
        int("a4.depth", "Depth", 1, 3, 1).adv(4),
        adv_switch(5, "Selective removal mask"),
        list(
            "a5.mask",
            "Keep (1) / remove (0) per slice",
            0,
            1,
            64,
            &[1, 0, 1, 1],
        )
        .adv(5),
        adv_switch(6, "Subchannel-specific claw phase"),
        list(
            "a6.phase",
            "Phase % of the period by subchannel",
            0,
            100,
            4096,
            &[0, 50],
        )
        .adv(6),
        adv_switch(7, "Pitch-controlled distortion"),
        pitch("a7.pivot", "Pivot", 60).adv(7),
        float("a7.amount", "Change per octave", -100.0, 100.0, 40.0, "%").adv(7),
        adv_switch(8, "Density compensation"),
        adv_switch(9, "Symmetric center claw"),
        adv_switch(10, "Distortion map (numeric)"),
        list(
            "a10.map",
            "Target of each slice start (‰ of the period)",
            0,
            1000,
            64,
            &[0, 50, 150, 300, 500, 700],
        )
        .adv(10),
        // V09-S34: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Period morph", false),
        ticks("n01.end", "End period", 1, 1 << 40, 1920),
        curve(
            "n01.curve",
            "Period → end period across the selection",
            Curve::line(0.0, 1.0),
        ),
        choice("n01.snap", "Snap periods to", MORPH_SNAP, 0),
        flag("n02", "Warp dynamics", false),
        float("n02.vel", "Velocity amount", -100.0, 100.0, 50.0, "%"),
        float("n02.len", "Length amount", -100.0, 100.0, 50.0, "%"),
        curve(
            "n02.curve",
            "Response (compression → effect)",
            Curve::line(0.0, 1.0),
        ),
        flag("n03", "Period sequence", false),
        list("n03.periods", "Periods", 1, 4096, 64, &[3, 3, 2]),
        choice("n03.unit", "Unit", SEQ_UNIT, 1),
        flag("n04", "Map morph A → B", false),
        curve(
            "n04.map",
            "Map B: position in the period → new position",
            Curve::smooth(vec![(0.0, 0.0), (0.5, 0.85), (1.0, 1.0)]),
        ),
        curve(
            "n04.curve",
            "A → B across the selection",
            Curve::line(0.0, 1.0),
        ),
        flag("n05", "Slice permutation", false),
        int("n05.slices", "Slices per period", 2, 64, 4),
        list(
            "n05.order",
            "Order (slot of each slice)",
            0,
            63,
            64,
            &[0, 2, 1, 3],
        ),
        choice("n05.shuffle", "Shuffle", SHUFFLE, 0),
        flag("n06", "Content-aware trash", false),
        int("n06.slices", "Slices per period", 2, 64, 8),
        int("n06.keep", "Keep", 1, 64, 4),
        choice("n06.by", "Keep the", KEEP_BY, 0),
        flag("n07", "Trash refill", false),
        choice("n07.from", "Fill with", FILL_FROM, 0),
        float("n07.vel", "Fill velocity", 1.0, 150.0, 70.0, "%"),
        int("n07.semis", "Fill transposition", -48, 48, 0),
        flag("n08", "Phase drift", false),
        int(
            "n08.drift",
            "Drift per period (ticks)",
            -100_000,
            100_000,
            120,
        ),
        choice("n08.mode", "Drift", DRIFT_MODE, 0),
        curve("n08.curve", "Drift curve", Curve::flat(1.0)),
        flag("n09", "Period rescale", false),
        ticks("n09.period", "New period", 1, 1 << 40, 2880),
        choice("n09.anchor", "Anchor", ANCHOR, 0),
        choice("n09.lengths", "Lengths", LENGTHS, 0),
        flag("n10", "Onset equalizer", false),
        float("n10.strength", "Strength", 0.0, 100.0, 100.0, "%"),
        int("n10.bins", "Resolution (bins)", 4, 256, 32),
        choice("n10.per", "Equalize per", EQ_PER, 0),
        flag("n11", "Dry / wet layers", false),
        float("n11.dry", "Dry velocity", 0.0, 150.0, 100.0, "%"),
        float("n11.wet", "Wet velocity", 0.0, 150.0, 100.0, "%"),
        int("n11.sub", "Dry subchannel offset", -4095, 4095, 1),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, CLAW_HELP);
    ToolSpec {
        id: "claw",
        name: "Claw Machine",
        family: Family::Fl,
        summary: "Warps time inside each period toward its start or end (bouncing-ball, rush), trashes slices, stretches or removes short notes.",
        params: p,
        advanced: vec![
            "Multi-stage distortion curve",
            "Editable distortion spline",
            "Alternating direction per period",
            "Nested period subdivision",
            "Selective removal mask",
            "Subchannel-specific claw phase",
            "Pitch-controlled distortion",
            "Density compensation",
            "Symmetric center claw",
            "Distortion map (numeric)",
        ],
        functions: [
            ("n01", "Period morph", 110),
            ("n02", "Warp dynamics", 510),
            ("n03", "Period sequence", 120),
            ("n04", "Map morph A → B", 130),
            ("n05", "Slice permutation", 140),
            ("n06", "Content-aware trash", 150),
            ("n07", "Trash refill", 160),
            ("n08", "Phase drift", 170),
            ("n09", "Period rescale", 180),
            ("n10", "Onset equalizer", 190),
            ("n11", "Dry / wet layers", 600),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

// ------------------------------------------------------------------ periods

/// One period of the grid: its index, start and length (ticks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Period {
    k: i128,
    p0: i128,
    len: i128,
}

impl Period {
    /// The same period in coordinates shifted by `sh` (a6 phase).
    #[inline]
    fn shifted(self, sh: i128) -> Period {
        Period {
            p0: self.p0 + sh,
            ..self
        }
    }
}

/// Periods laid out by a cycle of lengths: period `k0` starts at `anchor`,
/// period `k0 + i` has length `lens[i mod m]` (one length = the v0.8 grid).
#[derive(Clone, Debug)]
struct Cycle {
    anchor: i128,
    k0: i128,
    lens: Vec<i128>,
    /// `prefix[i]` = sum of the first `i` lengths (`m + 1` entries).
    prefix: Vec<i128>,
    total: i128,
}

impl Cycle {
    fn new(anchor: i128, k0: i128, lens: Vec<i128>) -> Cycle {
        let mut prefix = Vec::with_capacity(lens.len() + 1);
        let mut s: i128 = 0;
        prefix.push(0);
        for &l in &lens {
            s += l;
            prefix.push(s);
        }
        Cycle {
            anchor,
            k0,
            lens,
            prefix,
            total: s,
        }
    }

    /// The period holding tick `t`.
    #[inline(always)]
    fn locate(&self, t: i128) -> Period {
        let d = t - self.anchor;
        if self.lens.len() == 1 {
            let len = self.total;
            let c = floor_div(d, len);
            return Period {
                k: self.k0 + c,
                p0: self.anchor + c * len,
                len,
            };
        }
        self.locate_in_cycle(d)
    }

    /// [`Cycle::locate`] for a cycle of several lengths (`d` = `t − anchor`).
    #[inline(never)]
    fn locate_in_cycle(&self, d: i128) -> Period {
        let c = floor_div(d, self.total);
        let r = d - c * self.total;
        let j = self.prefix.partition_point(|&x| x <= r) - 1;
        Period {
            k: self.k0 + c * self.lens.len() as i128 + j as i128,
            p0: self.anchor + c * self.total + self.prefix[j],
            len: self.lens[j],
        }
    }

    /// Period number `k`.
    fn period(&self, k: i128) -> Period {
        let m = self.lens.len() as i128;
        let d = k - self.k0;
        let c = d.div_euclid(m);
        let j = d.rem_euclid(m) as usize;
        Period {
            k,
            p0: self.anchor + c * self.total + self.prefix[j],
            len: self.lens[j],
        }
    }
}

/// The run's periods (unshifted): a cycle before the selection, a table of
/// period starts across it when the lengths change there (n01 morph, n08
/// drift), and a cycle after it (the lengths at the selection's end, the
/// drift offset reached there).
#[derive(Clone, Debug)]
struct Grid {
    head: Cycle,
    /// Starts of periods `kt0 …`; the last entry is the tail's anchor.
    table: Vec<i128>,
    kt0: i128,
    tail: Option<Cycle>,
}

impl Grid {
    #[inline(always)]
    fn locate(&self, t: i128) -> Period {
        match &self.tail {
            None => self.head.locate(t),
            Some(tail) => self.locate_in_table(t, tail),
        }
    }

    #[inline(never)]
    fn locate_in_table(&self, t: i128, tail: &Cycle) -> Period {
        if t < self.table[0] {
            self.head.locate(t)
        } else if t >= self.table[self.table.len() - 1] {
            tail.locate(t)
        } else {
            let i = self.table.partition_point(|&x| x <= t) - 1;
            Period {
                k: self.kt0 + i as i128,
                p0: self.table[i],
                len: self.table[i + 1] - self.table[i],
            }
        }
    }

    fn period(&self, k: i128) -> Period {
        let Some(tail) = &self.tail else {
            return self.head.period(k);
        };
        let last = self.kt0 + self.table.len() as i128 - 1;
        if k < self.kt0 {
            self.head.period(k)
        } else if k >= last {
            tail.period(k)
        } else {
            let i = (k - self.kt0) as usize;
            Period {
                k,
                p0: self.table[i],
                len: self.table[i + 1] - self.table[i],
            }
        }
    }
}

// ------------------------------------------------------------ the functions

/// n01: the period moves from the tool's period to `end` along `curve`
/// across the selection; lengths snapped to ticks / the grid / beats.
#[derive(Clone)]
struct Morph {
    end: f64,
    curve: Curve,
    snap: usize,
}

/// n02: velocity and length scaled by how much the map compresses time.
#[derive(Clone)]
struct Dynamics {
    vel: f64,
    len: f64,
    curve: Curve,
}

/// n05: slices per period and the slot of each slice.
#[derive(Clone)]
struct Permute {
    slices: usize,
    /// The list's permutation (List) or the one seeded shuffle (Seeded
    /// once); `None` = a seeded shuffle per period.
    fixed: Option<Vec<u8>>,
}

/// n06: keep the `keep` slices of `slices` that rank first `by`.
#[derive(Clone, Copy)]
struct Content {
    slices: usize,
    keep: usize,
    by: usize,
}

/// n07: where a trashed slice's copy comes from, its velocity and
/// transposition.
#[derive(Clone, Copy)]
struct Refill {
    from: usize,
    vel: f64,
    semis: i64,
}

/// n08: ticks added to each period's start, accumulated.
#[derive(Clone)]
struct Drift {
    per: f64,
    wrap: bool,
    curve: Curve,
}

/// n09: every period's contents squeezed / stretched to `period` ticks.
#[derive(Clone, Copy)]
struct Rescale {
    period: i128,
    anchor: usize,
    keep_lengths: bool,
}

/// n10: the onset histogram of each period (and key / subchannel).
#[derive(Clone, Copy)]
struct Equalize {
    strength: f64,
    bins: usize,
    per: usize,
}

/// n11: the dry layer beside the wet one.
#[derive(Clone, Copy)]
struct Layers {
    dry: f64,
    wet: f64,
    sub: i64,
}

/// The v0.9 functions declared by id (TOOL_CATALOG `claw` n01 … n11).
/// `None` = switched off; with every one off the kernel is the v0.8 one.
#[derive(Clone, Default)]
struct ClawNew {
    morph: Option<Morph>,
    dynamics: Option<Dynamics>,
    /// n03: the cycled period lengths in ticks.
    sequence: Option<Vec<i128>>,
    /// n04: map B (monotone) and the A → B weight across the selection.
    map_b: Option<(Curve, Curve)>,
    permute: Option<Permute>,
    content: Option<Content>,
    refill: Option<Refill>,
    drift: Option<Drift>,
    rescale: Option<Rescale>,
    equalize: Option<Equalize>,
    layers: Option<Layers>,
    seed: u64,
}

/// Internal mark of an n07 copy among the kept notes (cleared before it is
/// emitted; TNote flags use bits 0 and 1 only).
const COPY: u8 = 0x80;

/// Trashing schemes (n07 refills a slice of the scheme that trashed it).
const BY_TRASH: u8 = 0;
const BY_MASK: u8 = 1;
const BY_CONTENT: u8 = 2;

/// A trashed slice: (slot — a shift of a grid, [`Levels::slot`] —, scheme,
/// period, slice).
type SliceKey = (u32, u8, i128, u16);

#[derive(Clone)]
struct Claw {
    period: u64,
    origin: u64,
    a: f64,
    trash: u64,
    stretch: bool,
    short: Option<u64>,
    seed: u64,
    stages: Vec<f64>,
    spline: Option<Curve>,
    alternate: bool,
    nested: Option<(u64, u32)>,
    mask: Vec<i64>,
    sub_phase: Vec<i64>,
    pitch_ctl: Option<(f64, f64)>,
    density: bool,
    symmetric: bool,
    map: Vec<f64>,
    subs: Vec<Vec<(u32, u8)>>,
    /// The selection's first and last start (n01 / n04 / n08 positions, the
    /// n09 reference period, the extent of the period table).
    sel: (u64, u64),
    ppq: u64,
    snap: u64,
    /// n10's strength (each level's under Control Sweeps).
    eq_strength: f64,
    new: ClawNew,
}

/// One grid shift of the run (a6) and its n09 reference period.
#[derive(Clone, Copy, Debug)]
struct Shift {
    sh: i128,
    /// The period holding the selection's start (n09: it keeps its place).
    k_ref: i128,
    /// That period's start (shifted).
    o_ref: i128,
    /// n09: the anchor's offset, `a × (len − new period)`.
    aoff: i128,
}

/// n10: per (shift, period, key) the sorted onset bins.
struct EqTable {
    /// (group key, offset into `bins`, onsets), sorted by key.
    dir: Vec<(u128, u32, u32)>,
    bins: Vec<u16>,
    b: usize,
    per: usize,
    k_base: i128,
}

impl EqTable {
    fn key(&self, sidx: usize, k: i128, n: &TNote) -> u128 {
        let key: u32 = match self.per {
            0 => 0,
            1 => u32::from(n.pitch),
            _ => n.sub,
        };
        (sidx as u128) << 112 | u128::from((k - self.k_base) as u64) << 48 | u128::from(key) << 16
    }

    fn group(&self, sidx: usize, k: i128, n: &TNote) -> Option<&[u16]> {
        let key = self.key(sidx, k, n);
        let i = self.dir.binary_search_by_key(&key, |x| x.0).ok()?;
        let (_, off, len) = self.dir[i];
        Some(&self.bins[off as usize..(off + len) as usize])
    }

    /// The equalized position of `u`: `(1 − s)·u + s·F(u)`, `F` the onsets'
    /// cumulative histogram interpolated inside each bin.
    fn h(&self, g: &[u16], u: f64, strength: f64) -> f64 {
        let bf = self.b as f64;
        let x = (u * bf).clamp(0.0, bf);
        let b = (x.floor() as usize).min(self.b);
        let c0 = g.partition_point(|&v| usize::from(v) < b);
        let c1 = g.partition_point(|&v| usize::from(v) <= b);
        let f = (c0 as f64 + (x - b as f64) * (c1 - c0) as f64) / g.len() as f64;
        (1.0 - strength) * u + strength * f
    }
}

/// What a run lays out once: the grid, the shifts, n10's histograms.
struct RunCx {
    grid: Grid,
    /// The first period length (a6 phases are % of it).
    l0: i128,
    shifts: Vec<Shift>,
    /// n10's histograms (set once the survivors are known).
    eq: std::cell::OnceCell<EqTable>,
}

/// n05: the permutation of the period last asked for (seeded per period).
#[derive(Default)]
struct PermCache {
    k: Option<i128>,
    perm: Vec<u8>,
}

/// `⌊d / len⌋` (`len > 0`): one hardware division while both fit in 64
/// bits (`i128::div_euclid` costs two library calls), the same value.
#[inline]
fn floor_div(d: i128, len: i128) -> i128 {
    match (u64::try_from(d), u64::try_from(len)) {
        (Ok(a), Ok(b)) => i128::from(a / b),
        _ => d.div_euclid(len),
    }
}

/// `x as f64` without the 128-bit conversion call while `x` fits in 64 bits
/// (the same correctly rounded value).
#[inline]
fn f64_of(x: i128) -> f64 {
    match i64::try_from(x) {
        Ok(v) => v as f64,
        Err(_) => x as f64,
    }
}

/// `x.round() as i128`, through a 64-bit conversion while in range (the same
/// value).
#[inline]
fn round_i128(x: f64) -> i128 {
    let r = x.round();
    if r.abs() < 9.0e18 {
        i128::from(r as i64)
    } else {
        r as i128
    }
}

/// Rounded `a × b / c` (`c > 0`).
#[inline]
fn mul_div_round(a: i128, b: i128, c: i128) -> i128 {
    (2 * a * b + c).div_euclid(2 * c)
}

/// A slot per slice from a list cycled in blocks of its own length (so
/// `1 0` swaps pairs), each slot used once; slices left over take the free
/// slots in order.
fn perm_from_list(order: &[i64], m: usize) -> Vec<u8> {
    let mut used = vec![false; m];
    let mut perm = vec![u8::MAX; m];
    if !order.is_empty() {
        let l = order.len();
        for (j, slot) in perm.iter_mut().enumerate() {
            let want = ((j / l * l) as i64 + order[j % l]).rem_euclid(m as i64) as usize;
            if !used[want] {
                used[want] = true;
                *slot = want as u8;
            }
        }
    }
    let mut free = (0..m).filter(|&s| !used[s]);
    for slot in perm.iter_mut() {
        if *slot == u8::MAX {
            *slot = free.next().unwrap_or(0) as u8;
        }
    }
    perm
}

/// A seeded shuffle of `m` slots for `key`.
fn perm_seeded(seed: u64, key: &[u64], m: usize) -> Vec<u8> {
    let mut v: Vec<u8> = (0..m as u8).collect();
    stream(seed, salt_of("n05"), key).shuffle(&mut v);
    v
}

impl Claw {
    /// The unit map g(u) on [0, 1] for period index `k` and note `n`.
    fn g(&self, u: f64, k: i128, n: &TNote) -> f64 {
        let mut a = self.a;
        if let Some((pivot, amt)) = self.pitch_ctl {
            a *= (1.0 + amt / 100.0 * (f64::from(n.pitch) - pivot) / 12.0).clamp(-1.0, 3.0);
        }
        if self.alternate && k.rem_euclid(2) == 1 {
            a = -a;
        }
        if let Some(c) = &self.spline {
            return c.eval(u);
        }
        if !self.map.is_empty() {
            // Piecewise linear through slice starts (monotone), ending at 1.
            let m = self.map.len();
            let x = u * m as f64;
            let i = (x.floor() as usize).min(m - 1);
            let y0 = self.map[i];
            let y1 = if i + 1 < m { self.map[i + 1] } else { 1.0 };
            return y0 + (y1.max(y0) - y0) * (x - i as f64);
        }
        if !self.stages.is_empty() {
            let s = self.stages.len();
            let x = u * s as f64;
            let i = (x.floor() as usize).min(s - 1);
            let local = x - i as f64;
            let aa = -CLAW_A_MAX * self.stages[i] / 100.0;
            return (i as f64 + warp(local, aa)) / s as f64;
        }
        if self.symmetric {
            return if u < 0.5 {
                warp(2.0 * u, a) / 2.0
            } else {
                1.0 - warp(2.0 * (1.0 - u), a) / 2.0
            };
        }
        warp(u, a)
    }

    /// The whole unit map of a period: n10's equalizer, the frozen map,
    /// blended toward map B by `w` (n04). With n04 / n10 off this is `g`.
    #[inline(always)]
    fn unit(&self, u: f64, k: i128, n: &TNote, w: f64, eq: Option<(&EqTable, &[u16])>) -> f64 {
        let u = match eq {
            Some((t, g)) => t.h(g, u, self.eq_strength),
            None => u,
        };
        let v = self.g(u, k, n);
        match &self.new.map_b {
            Some((b, _)) if w > 0.0 => (1.0 - w) * v + w * b.eval(u),
            _ => v,
        }
    }

    /// The unit map's slope at `u` (central difference, as v0.8's density
    /// compensation measures it).
    fn slope(&self, u: f64, k: i128, n: &TNote, w: f64, eq: Option<(&EqTable, &[u16])>) -> f64 {
        let h = 1e-3;
        let (lo, hi) = ((u - h).max(0.0), (u + h).min(1.0));
        (self.unit(hi, k, n, w, eq) - self.unit(lo, k, n, w, eq)) / (hi - lo)
    }

    /// Maps tick `t` (which belongs to period `k` unless `t` is that period's
    /// end boundary) — the frozen nested subdivision below the top level.
    fn map_tick(&self, t: i128, n: &TNote, period: i128, origin: i128, depth: u32) -> i128 {
        let k = floor_div(t - origin, period);
        let p0 = origin + k * period;
        let len = f64_of(period);
        let u = f64_of(t - p0) / len;
        let v = self.g(u, k, n);
        let mut r = p0 + round_i128(v * len);
        if let Some((parts, max)) = self.nested
            && depth < max
        {
            let sub = period / i128::from(parts);
            if sub >= 2 {
                r = self.map_tick(r, n, sub, p0, depth + 1);
            }
        }
        r
    }

    /// Maps tick `t` inside period `per` (shifted): the unit map, then the
    /// nested subdivision (a4).
    #[inline]
    fn map_top(
        &self,
        t: i128,
        n: &TNote,
        per: &Period,
        w: f64,
        eq: Option<(&EqTable, &[u16])>,
    ) -> i128 {
        let len = f64_of(per.len);
        let u = f64_of(t - per.p0) / len;
        let v = self.unit(u, per.k, n, w, eq);
        let mut r = per.p0 + round_i128(v * len);
        if let Some((parts, max)) = self.nested
            && max > 0
        {
            let sub = per.len / i128::from(parts);
            if sub >= 2 {
                r = self.map_tick(r, n, sub, per.p0, 1);
            }
        }
        r
    }

    /// n04's weight of map B in a period (by the period's place in the
    /// selection).
    #[inline]
    fn weight(&self, per: &Period) -> f64 {
        match &self.new.map_b {
            Some((_, c)) => c.eval(self.sel_u(per.p0)).clamp(0.0, 1.0),
            None => 0.0,
        }
    }

    /// Position of tick `t` across the selection's starts, 0–1.
    #[inline]
    fn sel_u(&self, t: i128) -> f64 {
        let (a, b) = (i128::from(self.sel.0), i128::from(self.sel.1));
        if b <= a {
            0.0
        } else {
            ((t - a) as f64 / (b - a) as f64).clamp(0.0, 1.0)
        }
    }

    // -------------------------------------------------------- the grid

    /// The base lengths of the cycle: n03's sequence or the period.
    fn base_lens(&self) -> Vec<i128> {
        self.new
            .sequence
            .clone()
            .unwrap_or_else(|| vec![i128::from(self.period.max(1))])
    }

    /// n01: base length `b` at selection position `u` (snapped).
    fn morphed(&self, m: &Morph, b: i128, u: f64) -> i128 {
        let p = self.period.max(1) as f64;
        let ratio = ((p + (m.end - p) * m.curve.eval(u)) / p).max(0.0);
        let x = b as f64 * ratio;
        let unit = match m.snap {
            0 => 1.0,
            1 => self.snap.max(1) as f64,
            _ => self.ppq.max(1) as f64,
        };
        let steps = (x / unit).round().max(1.0);
        if steps >= 1e30 {
            return 1 << 100;
        }
        steps as i128 * unit as i128
    }

    fn lens_at(&self, base: &[i128], u: f64, from: i128) -> Vec<i128> {
        let m = base.len() as i128;
        (0..m)
            .map(|i| {
                let b = base[(from + i).rem_euclid(m) as usize];
                match &self.new.morph {
                    Some(mo) => self.morphed(mo, b, u),
                    None => b,
                }
            })
            .collect()
    }

    /// Lays the periods out (unshifted), covering the selection's starts
    /// from `max_shift` before the first one.
    fn grid(&self, max_shift: i128) -> Result<Grid, TransformError> {
        let base = self.base_lens();
        let origin = i128::from(self.origin);
        let head = Cycle::new(origin, 0, self.lens_at(&base, 0.0, 0));
        if self.new.morph.is_none() && self.new.drift.is_none() {
            return Ok(Grid {
                head,
                table: Vec::new(),
                kt0: 0,
                tail: None,
            });
        }
        let m = base.len() as i128;
        let len_of = |k: i128, u: f64| -> i128 {
            let b = base[k.rem_euclid(m) as usize];
            match &self.new.morph {
                Some(mo) => self.morphed(mo, b, u),
                None => b,
            }
        };
        let lo = i128::from(self.sel.0) - max_shift;
        let hi = i128::from(self.sel.1);
        let too_many = || {
            TransformError::TooLarge(format!(
                "Laying out the periods across the selection would take more than {} periods, so nothing was changed. Choose a longer period, a longer end period or a smaller drift.",
                crate::midi::group_digits(MAX_PERIODS as u64)
            ))
        };
        // Refuse at once when even the longest possible steps need too many
        // periods (no loop of millions of steps before the refusal).
        let reach = |c: &Curve| {
            c.points
                .iter()
                .map(|q| q.1)
                .chain([c.eval(0.0), c.eval(1.0)])
                .fold(1.0f64, |a, y| a.max(y.abs()))
        };
        let longest = *base.iter().max().unwrap_or(&1) as f64
            * match &self.new.morph {
                Some(mo) => {
                    let p = self.period.max(1) as f64;
                    (1.0 + (mo.end - p).abs() / p * reach(&mo.curve)) * 2.0
                }
                None => 1.0,
            }
            + self.ppq.max(self.snap) as f64;
        let step = 2.0 * longest
            + self
                .new
                .drift
                .as_ref()
                .map_or(0.0, |d| d.per.abs() * reach(&d.curve));
        if (hi - lo) as f64 / step > MAX_PERIODS as f64 {
            return Err(too_many());
        }
        let first = head.locate(lo);
        let (mut k, mut b) = (first.k, first.p0);
        let mut acc: i128 = 0;
        let mut table = Vec::new();
        loop {
            let len = len_of(k, self.sel_u(b));
            let off = match &self.new.drift {
                Some(d) if d.wrap => acc.rem_euclid(len),
                _ => acc,
            };
            let s = b + off;
            table.push(s);
            if s > hi {
                break;
            }
            if table.len() > MAX_PERIODS {
                return Err(too_many());
            }
            b += len;
            k += 1;
            if let Some(d) = &self.new.drift {
                let mut step = (d.per * d.curve.eval(self.sel_u(b))).round() as i128;
                if !d.wrap {
                    // Every period keeps at least one tick.
                    step = step.max(1 - len);
                }
                acc += step;
            }
        }
        let kt0 = first.k;
        let kk = kt0 + table.len() as i128 - 1;
        let tail = Cycle::new(table[table.len() - 1], kk, self.lens_at(&base, 1.0, kk));
        Ok(Grid {
            head,
            table,
            kt0,
            tail: Some(tail),
        })
    }

    /// The a6 shift of note `n`: its subchannel's phase % of the first
    /// period length.
    #[inline]
    fn shift_of(&self, n: &TNote, l0: i128) -> i128 {
        if self.sub_phase.is_empty() {
            return 0;
        }
        let si = self
            .subs
            .get(usize::from(n.track))
            .and_then(|t| t.iter().position(|s| s.0 == n.sub))
            .unwrap_or(0);
        l0 * i128::from(self.sub_phase[si % self.sub_phase.len()]) / 100
    }

    fn prepare(&self) -> Result<RunCx, TransformError> {
        let l0 = self.base_lens()[0];
        let mut shs: Vec<i128> = if self.sub_phase.is_empty() {
            vec![0]
        } else {
            self.sub_phase
                .iter()
                .map(|&p| l0 * i128::from(p) / 100)
                .collect()
        };
        shs.sort_unstable();
        shs.dedup();
        let max_shift = *shs.last().unwrap_or(&0);
        let grid = self.grid(max_shift)?;
        let shifts = shs
            .into_iter()
            .map(|sh| {
                let r = grid.locate(i128::from(self.sel.0) - sh);
                let aoff = match self.new.rescale {
                    Some(rs) => match rs.anchor {
                        0 => 0,
                        1 => (r.len - rs.period).div_euclid(2),
                        _ => r.len - rs.period,
                    },
                    None => 0,
                };
                Shift {
                    sh,
                    k_ref: r.k,
                    o_ref: r.p0 + sh,
                    aoff,
                }
            })
            .collect();
        Ok(RunCx {
            grid,
            l0,
            shifts,
            eq: std::cell::OnceCell::new(),
        })
    }

    #[inline]
    fn shift_index(&self, n: &TNote, cx: &RunCx) -> usize {
        if cx.shifts.len() == 1 {
            return 0;
        }
        let sh = self.shift_of(n, cx.l0);
        cx.shifts.partition_point(|s| s.sh < sh)
    }

    // -------------------------------------------------------- per note

    /// The scheme and slice that trash note `n` (Trash every, then a5).
    fn trashed(&self, n: &TNote, sidx: usize, cx: &RunCx) -> Option<(u8, i128, u16)> {
        if self.trash <= 1 && self.mask.is_empty() {
            return None;
        }
        let sh = cx.shifts[sidx].sh;
        let t = i128::from(n.start) - sh;
        let per = cx.grid.locate(t);
        let off = t - per.p0;
        if self.trash > 1 {
            let j = off * i128::from(self.trash) / per.len;
            if j == i128::from(self.trash) - 1 {
                return Some((BY_TRASH, per.k, j as u16));
            }
        }
        if !self.mask.is_empty() {
            let j = (off * self.mask.len() as i128 / per.len) as usize;
            if self.mask[j] == 0 {
                return Some((BY_MASK, per.k, j as u16));
            }
        }
        None
    }

    /// n09: where tick `r` of period `per` lands in the closed-up timeline.
    #[inline]
    fn rescaled(&self, rs: &Rescale, r: i128, per: &Period, sh: &Shift) -> i128 {
        sh.o_ref
            + (per.k - sh.k_ref) * rs.period
            + sh.aoff
            + mul_div_round(r - per.p0, rs.period, per.len)
    }

    /// n05: how far the slice holding mapped start `r` moves.
    fn slot_shift(&self, pm: &Permute, r: i128, per: &Period, cache: &mut PermCache) -> i128 {
        let m = pm.slices as i128;
        let j = ((r - per.p0) * m / per.len).clamp(0, m - 1);
        let slot = match &pm.fixed {
            Some(p) => i128::from(p[j as usize]),
            None => {
                if cache.k != Some(per.k) || cache.perm.len() != pm.slices {
                    cache.perm = perm_seeded(
                        self.new.seed,
                        &[per.k as u64, (per.k >> 64) as u64],
                        pm.slices,
                    );
                    cache.k = Some(per.k);
                }
                i128::from(cache.perm[j as usize])
            }
        };
        let lout = match self.new.rescale {
            Some(rs) => rs.period,
            None => per.len,
        };
        slot * lout / m - j * lout / m
    }

    /// Note `n` starting at `s` mapped: (start, end, note with its new
    /// velocity), or `None` when density compensation (a8) thins it.
    fn place(
        &self,
        n: &TNote,
        s: i128,
        sidx: usize,
        cx: &RunCx,
        cache: &mut PermCache,
    ) -> Option<(i128, i128, TNote)> {
        let eq = cx.eq.get();
        let shift = &cx.shifts[sidx];
        let sh = shift.sh;
        let per = cx.grid.locate(s - sh).shifted(sh);
        let w = self.weight(&per);
        let eqs = eq.and_then(|t| t.group(sidx, per.k, n).map(|g| (t, g)));
        let ns0 = self.map_top(s, n, &per, w, eqs);
        let e = s + i128::from(n.dur);
        let pe = cx.grid.locate(e - sh).shifted(sh);
        // An end on a boundary belongs to the period it closes.
        let ne0 = if pe.p0 == e {
            e
        } else {
            let eqe = eq.and_then(|t| t.group(sidx, pe.k, n).map(|g| (t, g)));
            self.map_top(e, n, &pe, self.weight(&pe), eqe)
        };
        let u = if self.density || self.new.dynamics.is_some() {
            f64_of(s - per.p0) / f64_of(per.len)
        } else {
            0.0
        };
        if self.density {
            // Thin where the map compresses time (keep ∝ local slope).
            let slope = self.slope(u, per.k, n, w, eqs);
            let mut r = Rng::new(hash(&[self.seed, salt::KEEP, note_key(n)]));
            if !r.chance(slope.clamp(0.0, 1.0)) {
                return None;
            }
        }
        let (mut ns, mut ne) = match &self.new.rescale {
            Some(rs) => {
                let ns = self.rescaled(rs, ns0, &per, shift);
                let ne = if rs.keep_lengths {
                    ns + (ne0 - ns0)
                } else {
                    self.rescaled(rs, ne0, &pe, shift)
                };
                (ns, ne)
            }
            None => (ns0, ne0),
        };
        if let Some(pm) = &self.new.permute {
            let d = self.slot_shift(pm, ns0, &per, cache);
            ns += d;
            ne += d;
        }
        let mut t = *n;
        if let Some(dy) = &self.new.dynamics {
            let mut slope = self.slope(u, per.k, n, w, eqs);
            if let Some(rs) = &self.new.rescale {
                slope *= rs.period as f64 / per.len as f64;
            }
            // Compression: +1 at 4× rushed, −1 at 4× dragged.
            let c = (-slope.max(1e-12).log2() / 2.0).clamp(-1.0, 1.0);
            let cr = c.signum() * dy.curve.eval(c.abs()).clamp(0.0, 1.0);
            t.vel = vel_scaled(n, (dy.vel * cr).exp2());
            let dur = ne - ns;
            if dur > 0 {
                let f = (-dy.len * cr).exp2();
                ne = ns + ((dur as f64 * f).round() as i128).max(1);
            }
        }
        Some((ns, ne, t))
    }

    /// The end of the (output) period holding mapped start `s` of `n`.
    fn period_end_out(&self, s: i128, n: &TNote, cx: &RunCx) -> i128 {
        let shift = &cx.shifts[self.shift_index(n, cx)];
        match &self.new.rescale {
            Some(rs) => {
                let base = shift.o_ref + shift.aoff;
                base + ((s - base).div_euclid(rs.period) + 1) * rs.period
            }
            None => {
                let p = cx.grid.locate(s - shift.sh);
                p.p0 + shift.sh + p.len
            }
        }
    }

    // -------------------------------------------------- whole-source passes

    /// n06: per (slot, period) the slices kept (bit `j`; empty slices
    /// count as kept) among `survivors` ((note, slot): a slot is a shift of
    /// a level's grid, [`Levels::slot`]), sorted by (slot, period). Under
    /// Control Sweeps a period keeps its first note's count (E0422).
    fn content_masks<L: Levels>(
        &self,
        c: &Content,
        src: &Source,
        l: &L,
        survivors: &[(u32, u32)],
    ) -> Vec<(u32, i128, u64)> {
        let m = c.slices;
        #[derive(Clone)]
        struct Acc {
            k: i128,
            live: bool,
            keep: usize,
            count: Vec<u64>,
            vel: Vec<u64>,
            pitch: Vec<u64>,
        }
        let fresh = Acc {
            k: 0,
            live: false,
            keep: c.keep,
            count: vec![0; m],
            vel: vec![0; m],
            pitch: vec![0; m],
        };
        let mut accs = vec![fresh; l.slots()];
        let mut out = Vec::new();
        let finish = |slot: usize, a: &mut Acc, out: &mut Vec<(u32, i128, u64)>| {
            if !a.live {
                return;
            }
            let mut filled: Vec<usize> = (0..m).filter(|&j| a.count[j] > 0).collect();
            let cmp_mean = |x: &[u64], i: usize, j: usize| {
                // x_i / count_i vs x_j / count_j without division.
                (u128::from(x[j]) * u128::from(a.count[i]))
                    .cmp(&(u128::from(x[i]) * u128::from(a.count[j])))
            };
            filled.sort_by(|&i, &j| {
                let o = match c.by {
                    0 => a.count[j].cmp(&a.count[i]),
                    1 => a.count[i].cmp(&a.count[j]),
                    2 => cmp_mean(&a.vel, i, j),
                    _ => cmp_mean(&a.pitch, i, j),
                };
                o.then(i.cmp(&j))
            });
            let mut mask: u64 = (0..m)
                .filter(|&j| a.count[j] == 0)
                .fold(0, |acc, j| acc | 1 << j);
            for &j in filled.iter().take(a.keep) {
                mask |= 1 << j;
            }
            out.push((slot as u32, a.k, mask));
            a.live = false;
            a.count.iter_mut().for_each(|x| *x = 0);
            a.vel.iter_mut().for_each(|x| *x = 0);
            a.pitch.iter_mut().for_each(|x| *x = 0);
        };
        for &(i, slot) in survivors {
            let n = &src.notes[i as usize];
            let (g, si) = l.slot(slot);
            let cx = l.grid(g);
            let t = i128::from(n.start) - cx.shifts[si].sh;
            let per = cx.grid.locate(t);
            let j = ((t - per.p0) * m as i128 / per.len) as usize;
            let a = &mut accs[slot as usize];
            if a.live && a.k != per.k {
                finish(slot as usize, a, &mut out);
            }
            if !a.live {
                // The period's first note's count (its level's, E0422).
                a.keep = l
                    .claw(l.tag(i as usize))
                    .new
                    .content
                    .map_or(c.keep, |x| x.keep);
            }
            a.k = per.k;
            a.live = true;
            a.count[j] += 1;
            a.vel[j] += u64::from(n.vel);
            a.pitch[j] += u64::from(n.pitch);
        }
        for (slot, a) in accs.iter_mut().enumerate() {
            finish(slot, a, &mut out);
        }
        out.sort_unstable_by_key(|x| (x.0, x.1));
        out
    }

    /// n07: one copy of a neighbouring kept slice per trashed slice (each
    /// copy with its source note's level, E0422).
    fn refill_copies<L: Levels>(
        &self,
        r: &Refill,
        src: &Source,
        l: &L,
        survivors: &[(u32, u32)],
        mut keys: Vec<SliceKey>,
        masks: &[(u32, i128, u64)],
    ) -> Vec<(TNote, i128, u32, L::Tag)> {
        keys.sort_unstable();
        keys.dedup();
        let content = self.new.content;
        let slices = |scheme: u8| -> i128 {
            match scheme {
                BY_TRASH => i128::from(self.trash),
                BY_MASK => self.mask.len() as i128,
                _ => content.map_or(1, |c| c.slices as i128),
            }
        };
        let kept = |slot: u32, scheme: u8, k: i128, j: i128| -> bool {
            match scheme {
                BY_TRASH => j != i128::from(self.trash) - 1,
                BY_MASK => self.mask[j as usize] != 0,
                _ => masks
                    .binary_search_by_key(&(slot, k), |x| (x.0, x.1))
                    .map_or(true, |i| masks[i].2 & 1 << j != 0),
            }
        };
        let starts: Vec<u64> = survivors
            .iter()
            .map(|&(i, _)| src.notes[i as usize].start)
            .collect();
        let mut copies = Vec::new();
        for (slot, scheme, k, j) in keys {
            let m = slices(scheme);
            let j = i128::from(j);
            let source = match r.from {
                1 => (j != 0 && kept(slot, scheme, k, 0)).then_some((k, 0)),
                from => {
                    let step: i128 = if from == 0 { -1 } else { 1 };
                    let (mut kk, mut jj) = (k, j);
                    let mut found = None;
                    for _ in 0..m {
                        jj += step;
                        if jj < 0 {
                            kk -= 1;
                            jj = m - 1;
                        } else if jj >= m {
                            kk += 1;
                            jj = 0;
                        }
                        if kept(slot, scheme, kk, jj) {
                            found = Some((kk, jj));
                            break;
                        }
                    }
                    found
                }
            };
            let Some((sk, sj)) = source else {
                continue;
            };
            let (g, si) = l.slot(slot);
            let cx = l.grid(g);
            let sh = cx.shifts[si].sh;
            let sp = cx.grid.period(sk);
            let a = sp.p0 + sj * sp.len / m + sh;
            let b = sp.p0 + (sj + 1) * sp.len / m + sh;
            let dp = cx.grid.period(k);
            let delta = dp.p0 + j * dp.len / m + sh - a;
            let lo = starts.partition_point(|&x| i128::from(x) < a);
            let hi = starts.partition_point(|&x| i128::from(x) < b);
            for &(i, s2) in &survivors[lo..hi] {
                if s2 != slot {
                    continue;
                }
                let n = &src.notes[i as usize];
                let tg = l.tag(i as usize);
                let vel = l.claw(tg).new.refill.map_or(r.vel, |x| x.vel);
                let s = i128::from(n.start) + delta;
                let mut c = *n;
                c.start = s.clamp(0, i128::from(u64::MAX)) as u64;
                c.vel = vel_scaled(n, vel);
                c.flags |= COPY;
                copies.push((c, s, slot, tg));
            }
        }
        copies
    }

    /// n10: the onset bins of the `survivors` of grid `g` per (shift,
    /// period, key).
    fn eq_table<L: Levels>(
        &self,
        e: &Equalize,
        src: &Source,
        l: &L,
        g: usize,
        survivors: &[(u32, u32)],
    ) -> EqTable {
        let cx = l.grid(g);
        let max_shift = cx.shifts.last().map_or(0, |s| s.sh);
        let k_base = cx.grid.locate(i128::from(self.sel.0) - max_shift).k;
        let mut t = EqTable {
            dir: Vec::new(),
            bins: Vec::with_capacity(survivors.len()),
            b: e.bins,
            per: e.per,
            k_base,
        };
        let mut keys: Vec<u128> = survivors
            .iter()
            .filter_map(|&(i, slot)| {
                let (gg, si) = l.slot(slot);
                if gg != g {
                    return None;
                }
                let n = &src.notes[i as usize];
                let x = i128::from(n.start) - cx.shifts[si].sh;
                let per = cx.grid.locate(x);
                let bin = ((x - per.p0) * e.bins as i128 / per.len).min(e.bins as i128 - 1);
                Some(t.key(si, per.k, n) | bin as u128)
            })
            .collect();
        keys.sort_unstable();
        let mut i = 0;
        while i < keys.len() {
            let g = keys[i] & !0xFFFF;
            let off = t.bins.len() as u32;
            while i < keys.len() && keys[i] & !0xFFFF == g {
                t.bins.push((keys[i] & 0xFFFF) as u16);
                i += 1;
            }
            t.dir.push((g, off, t.bins.len() as u32 - off));
        }
        t
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let cx = self.prepare()?;
        self.run_in(&One { c: self, cx }, src, out)
    }

    /// The run under Control Sweeps (E0422): each note with its level's
    /// settings (`lv`) and grid — levels whose grid settings agree (n01,
    /// n08, n09: the swept ones) share one — while n06's ranks, n07's
    /// copies and n10's histograms see every note of a grid.
    fn run_swept(
        &self,
        lv: &PerLevel<Claw>,
        src: &Source,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let mut keys: Vec<GridKey> = Vec::new();
        let mut grids: Vec<RunCx> = Vec::new();
        let mut grid_of = Vec::with_capacity(lv.all().len());
        let mut periods = 0usize;
        for c in lv.all() {
            let k = c.grid_key();
            let g = match keys.iter().position(|x| *x == k) {
                Some(g) => g,
                None => {
                    let cx = c.prepare()?;
                    periods = periods.saturating_add(cx.grid.table.len());
                    if periods > MAX_PERIODS {
                        return Err(TransformError::TooLarge(format!(
                            "Laying out the periods of every Control Sweep level would take more than {} periods in all, so nothing was changed. Sweep the end period, the drift or the new period over a narrower range, or choose a longer period.",
                            crate::midi::group_digits(MAX_PERIODS as u64)
                        )));
                    }
                    keys.push(k);
                    grids.push(cx);
                    grids.len() - 1
                }
            };
            grid_of.push(g as u32);
            out.check()?;
        }
        let mut base = Vec::with_capacity(grids.len() + 1);
        let mut at = 0u32;
        for cx in &grids {
            base.push(at);
            at += cx.shifts.len() as u32;
        }
        base.push(at);
        self.run_in(
            &Swept {
                lv,
                grid_of,
                grids,
                base,
            },
            src,
            out,
        )
    }

    fn run_in<L: Levels>(
        &self,
        l: &L,
        src: &Source,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let mut cache = PermCache::default();
        let nw = &self.new;
        let mut kept: Vec<(i128, i128, TNote, L::Tag)> = Vec::with_capacity(src.notes.len());
        if nw.content.is_none() && nw.refill.is_none() && nw.equalize.is_none() {
            for (i, n) in src.notes.iter().enumerate() {
                let tg = l.tag(i);
                let (c, cx) = (l.claw(tg), l.cx(tg));
                let sidx = c.shift_index(n, cx);
                if c.trashed(n, sidx, cx).is_some() {
                    continue;
                }
                if let Some((s, e, t)) = c.place(n, i128::from(n.start), sidx, cx, &mut cache) {
                    kept.push((s, e, t, tg));
                }
            }
        } else {
            // Trash every and a5, then n06 by content.
            let mut keys: Vec<SliceKey> = Vec::new();
            let mut survivors: Vec<(u32, u32)> = Vec::with_capacity(src.notes.len());
            for (i, n) in src.notes.iter().enumerate() {
                let tg = l.tag(i);
                let (c, cx) = (l.claw(tg), l.cx(tg));
                let sidx = c.shift_index(n, cx);
                let slot = l.base(tg) + sidx as u32;
                match c.trashed(n, sidx, cx) {
                    Some((scheme, k, j)) => {
                        if nw.refill.is_some() {
                            keys.push((slot, scheme, k, j));
                        }
                    }
                    None => survivors.push((i as u32, slot)),
                }
            }
            let masks = match &nw.content {
                Some(c) => {
                    let masks = self.content_masks(c, src, l, &survivors);
                    let m = c.slices as i128;
                    survivors.retain(|&(i, slot)| {
                        let n = &src.notes[i as usize];
                        let (g, si) = l.slot(slot);
                        let cx = l.grid(g);
                        let t = i128::from(n.start) - cx.shifts[si].sh;
                        let per = cx.grid.locate(t);
                        let j = (t - per.p0) * m / per.len;
                        let keep = masks
                            .binary_search_by_key(&(slot, per.k), |x| (x.0, x.1))
                            .map_or(true, |x| masks[x].2 & 1 << j != 0);
                        if !keep && nw.refill.is_some() {
                            keys.push((slot, BY_CONTENT, per.k, j as u16));
                        }
                        keep
                    });
                    masks
                }
                None => Vec::new(),
            };
            let copies = match &nw.refill {
                Some(r) => self.refill_copies(r, src, l, &survivors, keys, &masks),
                None => Vec::new(),
            };
            if let Some(e) = &nw.equalize {
                for g in 0..l.grids() {
                    let t = self.eq_table(e, src, l, g, &survivors);
                    let _ = l.grid(g).eq.set(t);
                }
            }
            for &(i, slot) in &survivors {
                let n = &src.notes[i as usize];
                let tg = l.tag(i as usize);
                let (g, si) = l.slot(slot);
                if let Some((s, e, t)) =
                    l.claw(tg)
                        .place(n, i128::from(n.start), si, l.grid(g), &mut cache)
                {
                    kept.push((s, e, t, tg));
                }
            }
            for (c, s, slot, tg) in &copies {
                let (g, si) = l.slot(*slot);
                if let Some((s, e, t)) = l.claw(*tg).place(c, *s, si, l.grid(g), &mut cache) {
                    kept.push((s, e, t, *tg));
                }
            }
        }
        if self.stretch {
            // Fill the space trashed notes left: each note reaches the next kept
            // onset of its track (or its period's end).
            kept.sort_by_key(|x| (x.2.track, x.0, x.2.pitch));
            let n = kept.len();
            for i in 0..n {
                let (s, e, note, tg) = kept[i];
                let next = kept[i + 1..]
                    .iter()
                    .find(|x| x.2.track == note.track && x.0 > s)
                    .map(|x| x.0);
                let pend = l.claw(tg).period_end_out(s, &note, l.cx(tg));
                let target = next.unwrap_or(pend).min(pend.max(e));
                if target > e {
                    kept[i].1 = target;
                }
            }
        }
        let semis = nw.refill.map_or(0, |r| r.semis);
        let wet = nw.layers.map(|l| l.wet);
        // Under Control Sweeps each note's wet level is its level's.
        if L::SWEPT || wet.is_none_or(|w| w > 0.0) {
            for (s, e, mut n, tg) in kept {
                if let Some(min) = self.short
                    && e - s < i128::from(min)
                {
                    continue;
                }
                let wet = if L::SWEPT {
                    let w = l.claw(tg).new.layers.map(|x| x.wet);
                    if w.is_some_and(|w| w <= 0.0) {
                        continue;
                    }
                    w
                } else {
                    wet
                };
                let mut pitch = i64::from(n.pitch);
                if n.flags & COPY != 0 {
                    n.flags &= !COPY;
                    pitch += semis;
                }
                let vel = match wet {
                    Some(w) => i64::from(vel_scaled(&n, w)),
                    None => i64::from(n.vel),
                };
                out.emit(s, e - s, pitch, vel, &n);
                out.check()?;
            }
        }
        if let Some(l) = nw.layers
            && l.dry > 0.0
        {
            for n in &src.notes {
                let t = sub_stepped(&src.tracks, n, l.sub);
                out.emit(
                    i128::from(n.start),
                    i128::from(n.dur),
                    i64::from(n.pitch),
                    i64::from(vel_scaled(n, l.dry)),
                    &t,
                );
                out.check()?;
            }
        }
        Ok(())
    }

    /// What decides the grid among the swept parameters (n01's end
    /// period, n08's drift, n09's new period): levels that agree share one.
    fn grid_key(&self) -> GridKey {
        (
            self.new.morph.as_ref().map(|m| m.end.to_bits()),
            self.new.drift.as_ref().map(|d| d.per.to_bits()),
            self.new.rescale.map(|r| r.period),
        )
    }
}

/// [`Claw::grid_key`].
type GridKey = (Option<u64>, Option<u64>, Option<i128>);

/// Each note's settings and grid in a run: one of each without Control
/// Sweeps ([`One`]), its level's under them ([`Swept`], E0422). A slot is
/// one shift (a6) of one grid; the whole-source passes group by slot.
trait Levels {
    /// What a placed note carries to find its level again (nothing
    /// without sweeps).
    type Tag: Copy;
    const SWEPT: bool;
    fn tag(&self, i: usize) -> Self::Tag;
    fn claw(&self, t: Self::Tag) -> &Claw;
    fn cx(&self, t: Self::Tag) -> &RunCx;
    /// The first slot of the note's grid.
    fn base(&self, t: Self::Tag) -> u32;
    /// The grid and its shift index of `slot`.
    fn slot(&self, slot: u32) -> (usize, usize);
    fn grid(&self, g: usize) -> &RunCx;
    fn grids(&self) -> usize;
    fn slots(&self) -> usize;
}

struct One<'a> {
    c: &'a Claw,
    cx: RunCx,
}

impl Levels for One<'_> {
    type Tag = ();
    const SWEPT: bool = false;
    #[inline(always)]
    fn tag(&self, _: usize) {}
    #[inline(always)]
    fn claw(&self, _: ()) -> &Claw {
        self.c
    }
    #[inline(always)]
    fn cx(&self, _: ()) -> &RunCx {
        &self.cx
    }
    #[inline(always)]
    fn base(&self, _: ()) -> u32 {
        0
    }
    #[inline(always)]
    fn slot(&self, slot: u32) -> (usize, usize) {
        (0, slot as usize)
    }
    #[inline(always)]
    fn grid(&self, _: usize) -> &RunCx {
        &self.cx
    }
    fn grids(&self) -> usize {
        1
    }
    fn slots(&self) -> usize {
        self.cx.shifts.len()
    }
}

struct Swept<'a> {
    lv: &'a PerLevel<Claw>,
    /// Per level its grid.
    grid_of: Vec<u32>,
    grids: Vec<RunCx>,
    /// Per grid its first slot (one more entry: the slot count).
    base: Vec<u32>,
}

impl Levels for Swept<'_> {
    type Tag = u32;
    const SWEPT: bool = true;
    fn tag(&self, i: usize) -> u32 {
        self.lv.group(i) as u32
    }
    fn claw(&self, t: u32) -> &Claw {
        &self.lv.all()[t as usize]
    }
    fn cx(&self, t: u32) -> &RunCx {
        &self.grids[self.grid_of[t as usize] as usize]
    }
    fn base(&self, t: u32) -> u32 {
        self.base[self.grid_of[t as usize] as usize]
    }
    fn slot(&self, slot: u32) -> (usize, usize) {
        let g = self.base.partition_point(|&b| b <= slot) - 1;
        (g, (slot - self.base[g]) as usize)
    }
    fn grid(&self, g: usize) -> &RunCx {
        &self.grids[g]
    }
    fn grids(&self) -> usize {
        self.grids.len()
    }
    fn slots(&self) -> usize {
        *self.base.last().unwrap_or(&0) as usize
    }
}

/// Claw's settings from `p` and its growth bound.
fn claw_of(p: &Params, b: &BuildCtx) -> (Claw, Growth) {
    let q = b.ctx.ppq;
    let period = match p.choice("period") {
        0 => q,
        1 => 2 * q,
        2 => 4 * q,
        3 => 8 * q,
        4 => 16 * q,
        _ => p.ticks("period.ticks"),
    }
    .max(1);
    let trash = match p.choice("trash") {
        1 => 2,
        2 => 3,
        3 => 4,
        4 => 6,
        5 => 8,
        6 => 16,
        _ => 0,
    };
    let mut map: Vec<f64> = if p.adv(10) {
        p.list("a10.map")
            .iter()
            .map(|&x| x as f64 / 1000.0)
            .collect()
    } else {
        Vec::new()
    };
    for i in 1..map.len() {
        map[i] = map[i].max(map[i - 1]);
    }
    // Monotone: running maximum of the drawn points.
    let monotone = |mut c: Curve| {
        let mut m: f64 = 0.0;
        for pt in &mut c.points {
            m = m.max(pt.1);
            pt.1 = m;
        }
        c
    };
    let seed = p.seed("seed");
    let mask = if p.adv(5) {
        p.list("a5.mask")
    } else {
        Vec::new()
    };
    let new = ClawNew {
        morph: p.flag("n01").then(|| Morph {
            end: p.ticks("n01.end").max(1) as f64,
            curve: p.curve("n01.curve"),
            snap: p.choice("n01.snap"),
        }),
        dynamics: p.flag("n02").then(|| Dynamics {
            vel: p.float("n02.vel") / 100.0,
            len: p.float("n02.len") / 100.0,
            curve: p.curve("n02.curve"),
        }),
        sequence: p
            .flag("n03")
            .then(|| {
                let unit = match p.choice("n03.unit") {
                    0 => q,
                    1 => q / 4,
                    _ => 1,
                }
                .max(1);
                p.list("n03.periods")
                    .iter()
                    .map(|&x| i128::from(x.max(1)) * i128::from(unit))
                    .collect::<Vec<i128>>()
            })
            .filter(|v| !v.is_empty()),
        map_b: p
            .flag("n04")
            .then(|| (monotone(p.curve("n04.map")), p.curve("n04.curve"))),
        permute: p.flag("n05").then(|| {
            let m = p.int("n05.slices").clamp(2, 64) as usize;
            Permute {
                slices: m,
                fixed: match p.choice("n05.shuffle") {
                    0 => Some(perm_from_list(&p.list("n05.order"), m)),
                    1 => None,
                    _ => Some(perm_seeded(seed, &[], m)),
                },
            }
        }),
        content: p.flag("n06").then(|| {
            let m = p.int("n06.slices").clamp(2, 64) as usize;
            Content {
                slices: m,
                keep: (p.int("n06.keep").max(1) as usize).min(m),
                by: p.choice("n06.by"),
            }
        }),
        refill: p.flag("n07").then(|| Refill {
            from: p.choice("n07.from"),
            vel: p.float("n07.vel") / 100.0,
            semis: p.int("n07.semis"),
        }),
        drift: p.flag("n08").then(|| Drift {
            per: p.int("n08.drift") as f64,
            wrap: p.choice("n08.mode") == 0,
            curve: p.curve("n08.curve"),
        }),
        rescale: p.flag("n09").then(|| Rescale {
            period: i128::from(p.ticks("n09.period").max(1)),
            anchor: p.choice("n09.anchor"),
            keep_lengths: p.choice("n09.lengths") == 1,
        }),
        equalize: p.flag("n10").then(|| Equalize {
            strength: p.float("n10.strength").clamp(0.0, 100.0) / 100.0,
            bins: p.int("n10.bins").clamp(4, 256) as usize,
            per: p.choice("n10.per"),
        }),
        layers: p.flag("n11").then(|| Layers {
            dry: p.float("n11.dry") / 100.0,
            wet: p.float("n11.wet") / 100.0,
            sub: p.int("n11.sub"),
        }),
        seed,
    };
    // Preflight bound: n07 copies a kept slice into at most the trashed
    // slices that walk back (or forward) to it — Trash every 1 per period,
    // a5 its removed slices, n06 twice its trashed slices (two periods'
    // runs meet) —; n11 adds the dry notes.
    let growth = if new.refill.is_some() || new.layers.is_some() {
        let mut copies = 0u64;
        if new.refill.is_some() {
            if trash > 1 {
                copies += 1;
            }
            if mask.contains(&1) {
                copies += mask.iter().filter(|&&x| x == 0).count() as u64;
            }
            if let Some(c) = new.content {
                copies += 2 * (c.slices - c.keep) as u64;
            }
        }
        Growth::Expand(1 + copies + u64::from(new.layers.is_some()))
    } else {
        Growth::Thin
    };
    let c = Claw {
        period,
        origin: if p.choice("origin") == 1 && !b.stats.is_empty() {
            b.stats.min_start
        } else {
            0
        },
        a: -CLAW_A_MAX * p.float("dist") / 100.0,
        trash,
        stretch: p.flag("stretch"),
        short: p.flag("short").then(|| p.ticks("short.ticks")),
        seed,
        stages: if p.adv(1) {
            p.list("a1.stages").iter().map(|&x| x as f64).collect()
        } else {
            Vec::new()
        },
        spline: p.adv(2).then(|| monotone(p.curve("a2.curve"))),
        alternate: p.adv(3),
        nested: p
            .adv(4)
            .then(|| (p.int("a4.parts") as u64, p.int("a4.depth") as u32)),
        mask,
        sub_phase: if p.adv(6) {
            p.list("a6.phase")
        } else {
            Vec::new()
        },
        pitch_ctl: p
            .adv(7)
            .then(|| (p.int("a7.pivot") as f64, p.float("a7.amount"))),
        density: p.adv(8),
        symmetric: p.adv(9),
        map,
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        sel: (b.stats.min_start, b.stats.max_start.max(b.stats.min_start)),
        ppq: q,
        snap: b.ctx.snap,
        eq_strength: new.equalize.map_or(0.0, |e| e.strength),
        new,
    };
    (c, growth)
}

fn claw_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let (c, mut growth) = claw_of(p, b);
    // With Control Sweeps (E0422) each note takes its level's settings;
    // n07's copies are bounded at the swept ranges' corners (n06's fewest
    // kept slices).
    let swept = Rebuild::of(p, b);
    if let Some(rb) = &swept {
        for (_, g) in rb.corners(b.ctx, claw_of) {
            if let (Growth::Expand(a), Growth::Expand(x)) = (growth, g) {
                growth = Growth::Expand(a.max(x));
            }
        }
    }
    let sctx = b.ctx.clone();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| match &swept {
            Some(r) => {
                let lv = r.levels(&sctx, src, |p, b| claw_of(p, b).0);
                c.run_swept(&lv, src, out)
            }
            None => c.run(src, out),
        },
        growth,
        estimate: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_locate_and_number_periods_both_ways() {
        let c = Cycle::new(100, 0, vec![30, 30, 20]);
        assert_eq!(
            c.locate(100),
            Period {
                k: 0,
                p0: 100,
                len: 30
            }
        );
        assert_eq!(c.locate(179).k, 2);
        assert_eq!(c.locate(180).p0, 180);
        assert_eq!(c.locate(180).k, 3);
        assert_eq!(c.locate(99), c.period(-1));
        assert_eq!(c.locate(99).p0, 80);
        for t in -500..500 {
            let p = c.locate(t);
            assert!(p.p0 <= t && t < p.p0 + p.len, "{t} {p:?}");
            assert_eq!(c.period(p.k), p);
        }
    }

    #[test]
    fn permutations_from_lists_use_every_slot_once() {
        assert_eq!(perm_from_list(&[0, 2, 1, 3], 4), vec![0, 2, 1, 3]);
        assert_eq!(perm_from_list(&[1, 0], 4), vec![1, 0, 3, 2]);
        assert_eq!(perm_from_list(&[2, 2, 2], 3), vec![2, 0, 1]);
        assert_eq!(perm_from_list(&[], 3), vec![0, 1, 2]);
        for m in 2..20 {
            let mut p = perm_seeded(7, &[m as u64], m);
            p.sort_unstable();
            assert_eq!(p, (0..m as u8).collect::<Vec<_>>());
        }
    }
}
