//! Slide / Portamento (EXPERIMENTAL, MIDI-compatible). Standard MIDI has no
//! slide notes, so a glide is rendered as **pitch-bend events** on the note's
//! MIDI channel: the bend starts at the interval from the previous pitch and
//! returns to the centre over the glide time while the note sounds at its own
//! pitch. The bend range is assumed (default ±2 semitones) or written with RPN 0.
//! Pitch bend acts on the whole channel: other notes sounding on it during a
//! glide bend too (function 9 finds those conflicts).
//!
//! v0.9 (V09-S35 step 6): eleven functions declared by id (TOOL_CATALOG
//! `slide` n01 … n11). With any of them on, the kernel plans every bend
//! first — glide sources (n06, n09), the interval window (n02), real-time
//! glide times (n01), bend chains (n07), release bends (n03), bend trills
//! (n04) and controller envelopes (n05) as *bend units* in semitones — then
//! moves units to free channels (n10), chooses bend ranges (n08), writes
//! the events and thins them (n11). With every one off it is the v0.8
//! kernel (docs/TRANSFORM_ENGINE.md "Slide / Portamento").

use super::super::boundary::{self, PitchPolicy};
use super::super::output::Lane;
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::density_window::sweep_cap_keep;
use super::super::shared::tempo_time::TempoTime;
use super::super::sweep::{PerLevel, Rebuild, swept_ticks_max};
use super::super::{Ctx, Growth, Kernel, Output, Source, TNote, TransformError};
use super::{Contextual, NOTE_VALUES, boxed, note_value, warp};

pub const SLIDE: ToolDef = ToolDef {
    id: "slide",
    spec: slide_spec,
    build: slide_build,
};

const SLIDE_DIRS: &[&str] = &["Up", "Down", "Both"];
const SLIDE_RELEASES: &[&str] = &["Fall", "Doit", "Scoop"];
const SLIDE_WAVES: &[&str] = &["Square", "Triangle", "Sine"];
const SLIDE_CCS: &[&str] = &["CC 1 modulation", "CC 7 volume", "CC 11 expression"];
const SLIDE_SOURCES: &[&str] = &[
    "The nearest previous pitch",
    "The previous note on the subchannel",
    "The previous note on the track",
    "The highest previous voice",
];
const SLIDE_RECENTRE: &[&str] = &["When the range runs out", "At rests", "Both"];
const SLIDE_RPN: &[&str] = &["Once per channel", "Before each change"];
const SLIDE_MATCH: &[&str] = &["By rank", "By nearest pitch"];
const SLIDE_POLICIES: &[&str] = &["Round robin", "Least recently used", "Per subchannel"];
const SLIDE_CH10: &[&str] = &["Skip it", "Use it"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SLIDE_HELP: &[(&str, &str)] = &[
    (
        "from",
        "Where each glide starts: the previous selected note on the same channel, or a fixed interval from the note.",
    ),
    (
        "interval",
        "For Fixed Interval: how many semitones from the note the glide starts (− = from below).",
    ),
    (
        "time",
        "How long each glide takes, as a note value, ending at the note's start.",
    ),
    ("time.ticks", "For Custom: the glide time, in ticks."),
    (
        "steps",
        "How many pitch-bend points each glide writes (more = smoother, more events).",
    ),
    (
        "range",
        "The synthesizer's pitch-bend range in semitones, used to compute the bend values (2 for most General MIDI players).",
    ),
    (
        "rpn",
        "Writes the bend range (RPN 0) on each channel before its first glide, so the synthesizer uses the same range.",
    ),
    (
        "a1",
        "Glides at an even speed from the start pitch to the note.",
    ),
    (
        "a2",
        "Glides along a curve that moves fast first and slows into the note (or the reverse).",
    ),
    (
        "a2.amount",
        "+ moves quickly at first and eases into the note; − starts slowly and speeds up; 0 = even.",
    ),
    ("a3", "Glides along your own drawn curve."),
    (
        "a3.curve",
        "The glide's pitch over its time (0 = the start pitch, 1 = the note).",
    ),
    (
        "a4",
        "Glides through a series of pitches on the way to the note.",
    ),
    (
        "a4.nodes",
        "The pitches passed through, in semitones from the target note, in order.",
    ),
    (
        "a5",
        "Glides in steps of whole semitones (or larger) instead of smoothly.",
    ),
    ("a5.res", "The size of each step, in semitones."),
    (
        "a6",
        "Overshoots the note slightly and settles back onto it.",
    ),
    (
        "a6.cents",
        "How far past the note the glide goes, in cents.",
    ),
    ("a7", "Adds vibrato while gliding."),
    ("a7.cents", "The vibrato's depth, in cents."),
    ("a7.rate", "The vibrato's speed, in cycles per beat."),
    ("a8", "Uses a different bend range for each subchannel."),
    (
        "a8.ranges",
        "The bend range for each subchannel in turn, in semitones; the list repeats.",
    ),
    (
        "a9",
        "Detects glides that would also bend other notes sounding on the same channel.",
    ),
    (
        "a9.mode",
        "Skip those glides, or shorten them so they end before the other note starts.",
    ),
    (
        "a10",
        "Writes the glide as short chromatic notes instead of pitch bends.",
    ),
    ("a10.len", "No step note is shorter than this, in ticks."),
    (
        "n01",
        "Times each glide in milliseconds, longer for bigger intervals, whatever the tempo.",
    ),
    ("n01.ms", "The base glide time, in milliseconds."),
    (
        "n01.per",
        "Added to the glide time for every semitone of the interval, in milliseconds.",
    ),
    ("n01.lo", "No glide is shorter than this, in milliseconds."),
    ("n01.hi", "No glide is longer than this, in milliseconds."),
    (
        "n02",
        "Glides only between notes whose interval lies in a range.",
    ),
    ("n02.lo", "The smallest interval glided, in semitones."),
    ("n02.hi", "The largest interval glided, in semitones."),
    ("n02.dir", "Glide only upward, only downward, or both ways."),
    (
        "n03",
        "Adds a bend at the end of each note: a fall, a doit or a scoop.",
    ),
    (
        "n03.kind",
        "Fall bends down at the end; Doit bends up at the end; Scoop bends up into the note's start.",
    ),
    ("n03.depth", "How far the release bend goes, in semitones."),
    ("n03.time", "How much of the note the release bend takes."),
    (
        "n03.shape",
        "The bend's shape over its time (0 = none, 1 = the full depth).",
    ),
    (
        "n04",
        "Trills by bending quickly between the note and another pitch.",
    ),
    (
        "n04.interval",
        "The other pitch of the trill, in semitones from the note.",
    ),
    ("n04.rate", "How many trill alternations per beat."),
    (
        "n04.wave",
        "The trill's shape: a square (jumps), a triangle (straight slides) or a sine (smooth).",
    ),
    (
        "n04.delay",
        "How long, in ticks, the note is held before the trill starts.",
    ),
    (
        "n05",
        "Writes a controller envelope along each glide (modulation, volume or expression).",
    ),
    ("n05.cc", "The controller written."),
    (
        "n05.curve",
        "The controller's value over the glide (0 = Envelope Low, 1 = Envelope High).",
    ),
    ("n05.lo", "The controller value at the curve's bottom."),
    ("n05.hi", "The controller value at the curve's top."),
    ("n06", "Chooses which earlier note each glide starts from."),
    (
        "n06.from",
        "The nearest previous pitch, the previous note on the same subchannel, the previous note on the track, or the highest previous voice.",
    ),
    (
        "n06.window",
        "How far back, in ticks, the source note is searched for.",
    ),
    (
        "n07",
        "Chains glides on one bend so successive notes slide from one to the next, re-centering when needed.",
    ),
    (
        "n07.recentre",
        "When the bend resets to the center: when the bend range runs out, at rests, or both.",
    ),
    (
        "n07.rest",
        "A rest at least this long, in ticks, counts for re-centering.",
    ),
    (
        "n08",
        "Chooses the bend range each glide needs and writes it (RPN 0), up to a limit.",
    ),
    ("n08.max", "The largest bend range allowed, in semitones."),
    (
        "n08.write",
        "Write the range once per channel, or before each change.",
    ),
    ("n09", "Glides chords voice by voice into the next chord."),
    (
        "n09.by",
        "Match each voice to the next chord's voice of the same rank, or to its nearest pitch.",
    ),
    (
        "n09.max",
        "Voices farther apart than this, in semitones, do not glide.",
    ),
    (
        "n09.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n10",
        "Moves gliding notes to channels of their own, so their bends never affect other notes.",
    ),
    ("n10.pool", "The MIDI channels that may be used."),
    (
        "n10.policy",
        "How channels are handed out: in turn, least recently used, or one per subchannel.",
    ),
    (
        "n10.ch10",
        "Skip channel 10 (drums in General MIDI) or use it like the others.",
    ),
    (
        "n10.tail",
        "How long, in ticks, a channel stays reserved after its note ends (for release tails).",
    ),
    (
        "n11",
        "Removes bend events that change the pitch too little to matter, to keep the file light.",
    ),
    (
        "n11.cents",
        "A bend point is kept only if dropping it would put the pitch off by more than this, in cents.",
    ),
    ("n11.rate", "The most bend events per second on a channel."),
];

fn slide_spec() -> ToolSpec {
    let mut p = vec![
        choice(
            "from",
            "Glide from",
            &[
                "The previous selected note on the channel",
                "A fixed interval below / above",
            ],
            0,
        ),
        int("interval", "Fixed interval (semitones)", -48, 48, -2),
        choice("time", "Glide time", NOTE_VALUES, 4),
        ticks("time.ticks", "Custom glide time (ticks)", 1, 1 << 40, 120),
        int("steps", "Bend points per glide", 2, 256, 16),
        int("range", "Bend range (± semitones)", 1, 48, 2),
        flag(
            "rpn",
            "Write the bend range (RPN 0) on each channel before the first glide",
            false,
        ),
        adv_switch(1, "Linear glide"),
        adv_switch(2, "Exponential glide"),
        float("a2.amount", "Curvature", -100.0, 100.0, 70.0, "%").adv(2),
        adv_switch(3, "Custom curve"),
        curve(
            "a3.curve",
            "Glide (0 = start pitch, 1 = target)",
            Curve::line(0.0, 1.0),
        )
        .adv(3),
        adv_switch(4, "Multi-node path"),
        list(
            "a4.nodes",
            "Path nodes (semitones from the target)",
            -48,
            48,
            16,
            &[3, -1],
        )
        .adv(4),
        adv_switch(5, "Quantized stepped glide"),
        int("a5.res", "Step (semitones)", 1, 12, 1).adv(5),
        adv_switch(6, "Overshoot and return"),
        int("a6.cents", "Overshoot (cents)", 1, 400, 50).adv(6),
        adv_switch(7, "Vibrato on glide"),
        int("a7.cents", "Depth (cents)", 1, 200, 30).adv(7),
        float("a7.rate", "Rate (cycles per beat)", 0.25, 32.0, 4.0, "").adv(7),
        adv_switch(8, "Per-subchannel bend range"),
        list(
            "a8.ranges",
            "Bend range by subchannel position",
            1,
            48,
            4096,
            &[2, 12],
        )
        .adv(8),
        adv_switch(9, "Polyphonic conflict detection"),
        choice(
            "a9.mode",
            "Glides that bend other sounding notes",
            &["Skip them", "Shorten them to end before the conflict"],
            0,
        )
        .adv(9),
        adv_switch(10, "Render as note steps"),
        int("a10.len", "Shortest step (ticks)", 1, 100_000, 20).adv(10),
        // v0.9 (V09-S35): functions declared by id.
        flag("n01", "Real-time glide timing", false),
        float("n01.ms", "Glide time", 0.0, 5000.0, 60.0, "ms"),
        float("n01.per", "Glide time per semitone", 0.0, 500.0, 10.0, "ms"),
        float("n01.lo", "Shortest glide", 0.0, 5000.0, 20.0, "ms"),
        float("n01.hi", "Longest glide", 0.0, 5000.0, 400.0, "ms"),
        flag("n02", "Interval window", false),
        int("n02.lo", "Smallest glided interval (semitones)", 0, 48, 1),
        int("n02.hi", "Largest glided interval (semitones)", 0, 48, 5),
        choice("n02.dir", "Glide direction", SLIDE_DIRS, 2),
        flag("n03", "Release bends", false),
        choice("n03.kind", "Release bend", SLIDE_RELEASES, 0),
        int("n03.depth", "Release depth (semitones)", 1, 48, 2),
        float(
            "n03.time",
            "Release time",
            1.0,
            100.0,
            25.0,
            "% of the note",
        ),
        curve(
            "n03.shape",
            "Release shape (0 = none, 1 = the depth)",
            Curve::line(0.0, 1.0),
        ),
        flag("n04", "Bend trill", false),
        int("n04.interval", "Trill interval (semitones)", -12, 12, 2),
        float("n04.rate", "Trill rate (per beat)", 0.25, 64.0, 4.0, ""),
        choice("n04.wave", "Trill shape", SLIDE_WAVES, 0),
        ticks("n04.delay", "Trill delay (ticks)", 0, 1 << 40, 0),
        flag("n05", "Expression envelope", false),
        choice("n05.cc", "Envelope controller", SLIDE_CCS, 2),
        curve(
            "n05.curve",
            "Envelope over the glide (0 = low, 1 = high)",
            Curve::line(0.0, 1.0),
        )
        .overshoot(),
        int("n05.lo", "Envelope low", 0, 127, 64),
        int("n05.hi", "Envelope high", 0, 127, 127),
        flag("n06", "Glide source", false),
        choice("n06.from", "Glide starts from", SLIDE_SOURCES, 0),
        ticks(
            "n06.window",
            "Source search window (ticks)",
            0,
            1 << 40,
            960,
        ),
        flag("n07", "Bend chain", false),
        choice("n07.recentre", "Re-center", SLIDE_RECENTRE, 0),
        ticks("n07.rest", "Rest (ticks)", 1, 1 << 40, 240),
        flag("n08", "Auto bend range", false),
        int("n08.max", "Largest bend range (semitones)", 1, 48, 12),
        choice("n08.write", "Write the range", SLIDE_RPN, 0),
        flag("n09", "Voice-matched chord glides", false),
        choice("n09.by", "Match voices", SLIDE_MATCH, 0),
        int("n09.max", "Largest voice glide (semitones)", 1, 48, 7),
        ticks("n09.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n10", "Channel allocation", false),
        list(
            "n10.pool",
            "Channel pool (1 – 16)",
            1,
            16,
            16,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        ),
        choice("n10.policy", "Allocation", SLIDE_POLICIES, 0),
        choice("n10.ch10", "Channel 10", SLIDE_CH10, 0),
        ticks("n10.tail", "Release tail (ticks)", 0, 1 << 40, 120),
        flag("n11", "Bend thinning", false),
        int("n11.cents", "Largest pitch error (cents)", 1, 100, 5),
        int("n11.rate", "Most bend events per second", 10, 10_000, 500),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, SLIDE_HELP);
    ToolSpec {
        id: "slide",
        name: "Slide / Portamento",
        family: Family::Fl,
        summary: "EXPERIMENTAL. Glides into each selected note from the previous one (or a fixed interval), written as pitch-bend events on the note's MIDI channel; the notes stay as they are.",
        params: p,
        advanced: vec![
            "Linear glide",
            "Exponential glide",
            "Custom curve",
            "Multi-node path",
            "Quantized stepped glide",
            "Overshoot and return",
            "Vibrato on glide",
            "Per-subchannel bend range",
            "Polyphonic conflict detection",
            "Render as note steps",
        ],
        functions: [
            ("n06", "Glide source", 110),
            ("n09", "Voice-matched chord glides", 120),
            ("n02", "Interval window", 130),
            ("n01", "Real-time glide timing", 140),
            ("n07", "Bend chain", 150),
            ("n03", "Release bends", 160),
            ("n04", "Bend trill", 170),
            ("n05", "Expression envelope", 180),
            ("n10", "Channel allocation", 520),
            ("n08", "Auto bend range", 530),
            ("n11", "Bend thinning", 700),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: true,
    }
}

/// Bend value for `semis` semitones at bend range `range`.
fn bend(semis: f64, range: f64) -> i64 {
    (8192.0 + semis / range * 8192.0)
        .round()
        .clamp(0.0, 16_383.0) as i64
}

/// The glide's path and shape ([a1] – [a6]).
#[derive(Clone)]
struct GlideShape {
    linear: bool,
    expo: Option<f64>,
    custom: Option<Curve>,
    nodes: Vec<i64>,
    quant: Option<i64>,
    over: Option<f64>,
}

impl GlideShape {
    fn shape(&self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        if self.linear {
            u
        } else if let Some(a) = self.expo {
            warp(u, a)
        } else if let Some(c) = &self.custom {
            c.eval(u).clamp(0.0, 1.0)
        } else {
            // Default: smooth ease in-out.
            u * u * (3.0 - 2.0 * u)
        }
    }

    /// Offset from the target (semitones) at glide position `u` ∈ [0, 1] of
    /// a glide from `interval` semitones away: the path `interval`, the
    /// nodes, 0, along the shape.
    fn offset(&self, interval: f64, u: f64) -> f64 {
        let len = self.nodes.len() + 2;
        let at = |k: usize| -> f64 {
            if k == 0 {
                interval
            } else if k == len - 1 {
                0.0
            } else {
                self.nodes[k - 1] as f64
            }
        };
        let s = self.shape(u) * (len - 1) as f64;
        let k = (s.floor() as usize).min(len - 2);
        let f = s - k as f64;
        let mut v = at(k) + (at(k + 1) - at(k)) * f;
        if let Some(o) = self.over {
            // Past the target by `o` in the last third, then back.
            let sign = if interval > 0.0 { -1.0 } else { 1.0 };
            v += sign * o * (std::f64::consts::PI * u).sin().powi(2) * u;
        }
        if let Some(q) = self.quant {
            v = (v / q as f64).round() * q as f64;
        }
        v
    }

    /// The largest node distance ([a4]).
    fn nodes_max(&self) -> f64 {
        self.nodes
            .iter()
            .map(|&x| (x as f64).abs())
            .fold(0.0, f64::max)
    }

    /// The farthest the path goes beyond its interval (overshoot, nodes).
    fn extra(&self) -> f64 {
        self.over.unwrap_or(0.0) + self.nodes_max()
    }
}

/// The Slide's v0.9 functions declared by id. `None` = off.
#[derive(Clone, Default)]
struct SlideNew {
    /// n01: time ms, ms per semitone, shortest ms, longest ms.
    rt: Option<(f64, f64, f64, f64)>,
    /// n02: smallest, largest interval, direction (0 up, 1 down, 2 both).
    window: Option<(i64, i64, usize)>,
    /// n03: kind (0 fall, 1 doit, 2 scoop), depth, share 0 – 1, shape.
    release: Option<(usize, i64, f64, Curve)>,
    /// n04: interval, rate per beat, wave (0 square, 1 triangle, 2 sine),
    /// delay ticks.
    trill: Option<(i64, f64, usize, u64)>,
    /// n05: controller number, curve, low, high.
    env: Option<(u8, Curve, i64, i64)>,
    /// n06: source (0 nearest previous pitch, 1 previous on the
    /// subchannel, 2 previous on the track, 3 highest previous voice),
    /// window ticks.
    source: Option<(usize, u64)>,
    /// n07: re-centre (0 range, 1 rests, 2 both), rest ticks.
    chain: Option<(usize, u64)>,
    /// n08: largest range, before each change (else once per channel).
    auto_range: Option<(i64, bool)>,
    /// n09: by nearest pitch (else rank), largest interval, tolerance.
    voices: Option<(bool, i64, u64)>,
    /// n10 on (its settings also serve n09).
    alloc_on: bool,
    /// n10 settings: pool (channels 0 – 15), policy (0 round robin, 1
    /// least recently used, 2 per subchannel), tail ticks.
    alloc: (Vec<u8>, usize, u64),
    /// n11: largest error (semitones), most events per second.
    thin: Option<(f64, u64)>,
}

impl SlideNew {
    fn any(&self) -> bool {
        self.rt.is_some()
            || self.window.is_some()
            || self.release.is_some()
            || self.trill.is_some()
            || self.env.is_some()
            || self.source.is_some()
            || self.chain.is_some()
            || self.auto_range.is_some()
            || self.voices.is_some()
            || self.alloc_on
            || self.thin.is_some()
    }
}

fn slide_new(p: &Params) -> SlideNew {
    let ms = |k: &str| p.float(k).max(0.0);
    let mut pool: Vec<u8> = p
        .list("n10.pool")
        .iter()
        .filter(|&&c| (1..=16).contains(&c))
        .map(|&c| (c - 1) as u8)
        .collect();
    if p.choice("n10.ch10") == 0 {
        pool.retain(|&c| c != 9);
    }
    pool.sort_unstable();
    pool.dedup();
    SlideNew {
        rt: p
            .flag("n01")
            .then(|| (ms("n01.ms"), ms("n01.per"), ms("n01.lo"), ms("n01.hi"))),
        window: p.flag("n02").then(|| {
            let (a, b) = (p.int("n02.lo"), p.int("n02.hi"));
            (a.min(b), a.max(b), p.choice("n02.dir"))
        }),
        release: p.flag("n03").then(|| {
            (
                p.choice("n03.kind"),
                p.int("n03.depth").max(1),
                (p.float("n03.time") / 100.0).clamp(0.01, 1.0),
                p.curve("n03.shape"),
            )
        }),
        trill: p.flag("n04").then(|| {
            (
                p.int("n04.interval"),
                p.float("n04.rate").clamp(0.25, 64.0),
                p.choice("n04.wave"),
                p.ticks("n04.delay"),
            )
        }),
        env: p.flag("n05").then(|| {
            (
                [1u8, 7, 11][p.choice("n05.cc").min(2)],
                p.curve("n05.curve"),
                p.int("n05.lo").clamp(0, 127),
                p.int("n05.hi").clamp(0, 127),
            )
        }),
        source: p
            .flag("n06")
            .then(|| (p.choice("n06.from"), p.ticks("n06.window"))),
        chain: p
            .flag("n07")
            .then(|| (p.choice("n07.recentre"), p.ticks("n07.rest").max(1))),
        auto_range: p
            .flag("n08")
            .then(|| (p.int("n08.max").max(1), p.choice("n08.write") == 1)),
        voices: p.flag("n09").then(|| {
            (
                p.choice("n09.by") == 1,
                p.int("n09.max").max(1),
                p.ticks("n09.tol"),
            )
        }),
        alloc_on: p.flag("n10"),
        alloc: (pool, p.choice("n10.policy"), p.ticks("n10.tail")),
        thin: p.flag("n11").then(|| {
            (
                p.int("n11.cents").max(1) as f64 / 100.0,
                p.int("n11.rate").max(1) as u64,
            )
        }),
    }
}

/// The context the v0.9 functions read (V09-S35): with n06, n09 or n10 on,
/// the unselected notes sounding from a search window / release tail before
/// the selection (the v0.8 context starts at the selection).
pub fn slide_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
) -> Result<(), TransformError> {
    if src.stats.is_empty() || !(p.flag("n06") || p.flag("n09") || p.flag("n10")) {
        return Ok(());
    }
    let mut back = 0u64;
    if p.flag("n06") {
        // The largest window its Control Sweep can give (E0422).
        back = back.max(swept_ticks_max(p, "slide", "n06.window"));
    }
    if p.flag("n10") {
        back = back.max(p.ticks("n10.tail"));
    }
    let a = src.stats.min_start.saturating_sub(back);
    let b = src.stats.max_end.saturating_add(src.stats.span());
    let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
    *src = s.with_context_sounding(doc, a, b);
    Ok(())
}

/// Every setting of one Slide run.
#[derive(Clone)]
struct SlideCfg {
    fixed: Option<i64>,
    time: u64,
    steps: u64,
    range: i64,
    rpn: bool,
    glide: GlideShape,
    vib: Option<(f64, f64)>,
    sub_ranges: Vec<i64>,
    conflict: Option<usize>,
    as_notes: Option<u64>,
    ppq: u64,
    ctx: Ctx,
    new: SlideNew,
    /// With Control Sweeps and a v0.9 function on (E0422; every target is
    /// a function's control): what each level's settings are built from —
    /// each note's glide, release, trill, envelope and source window and
    /// each bend point's thinning error are its level's, in one run.
    swept: Option<std::sync::Arc<Rebuild>>,
}

/// The Slide's settings from `p`.
fn slide_cfg(p: &Params, b: &BuildCtx) -> SlideCfg {
    SlideCfg {
        fixed: (p.choice("from") == 1).then(|| p.int("interval")),
        time: note_value(p.choice("time"), p.ticks("time.ticks"), b.ctx),
        steps: p.int("steps").max(2) as u64,
        range: p.int("range").max(1),
        rpn: p.flag("rpn"),
        glide: GlideShape {
            linear: p.adv(1),
            expo: p.adv(2).then(|| -3.93 * p.float("a2.amount") / 100.0),
            custom: p.adv(3).then(|| p.curve("a3.curve")),
            nodes: if p.adv(4) {
                p.list("a4.nodes")
            } else {
                Vec::new()
            },
            quant: p.adv(5).then(|| p.int("a5.res").max(1)),
            over: p.adv(6).then(|| p.int("a6.cents") as f64 / 100.0),
        },
        vib: p
            .adv(7)
            .then(|| (p.int("a7.cents") as f64 / 100.0, p.float("a7.rate"))),
        sub_ranges: if p.adv(8) {
            p.list("a8.ranges")
        } else {
            Vec::new()
        },
        conflict: p.adv(9).then(|| p.choice("a9.mode")),
        as_notes: p.adv(10).then(|| p.int("a10.len").max(1) as u64),
        ppq: b.ctx.ppq.max(1),
        ctx: b.ctx.clone(),
        new: slide_new(p),
        swept: None,
    }
}

/// The v0.9 functions' own points (written even with [a10]).
fn slide_new_events(cfg: &SlideCfg, b: &BuildCtx) -> u64 {
    let count = b.stats.count as u64;
    let new = &cfg.new;
    let mut new_events = 0u64;
    if new.any() {
        // Release bends, envelopes, range writes and chain re-centres per
        // note; trill points over the notes' lengths.
        let mut per_note = 0u64;
        if new.release.is_some() {
            per_note += cfg.steps + 2;
        }
        if new.env.is_some() {
            per_note += cfg.steps;
        }
        if new.auto_range.is_some() || new.chain.is_some() {
            per_note += 5;
        }
        if let Some((_, rate, wave, _)) = new.trill {
            let pts = if wave == 0 { 2.0 } else { TRILL_POINTS as f64 };
            let cycles = b.stats.sum_dur as f64 * rate / cfg.ppq as f64;
            new_events =
                new_events.saturating_add(((cycles * pts) as u64).saturating_add(3 * count));
        }
        new_events = new_events.saturating_add(count.saturating_mul(per_note));
    }
    new_events
}

fn slide_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let mut cfg = slide_cfg(p, b);
    let count = b.stats.count as u64;
    let per_glide = cfg.steps + 8 + cfg.vib.map_or(0, |_| 256);
    let total_events = count.saturating_mul(per_glide);
    let mut new_events = slide_new_events(&cfg, b);
    if cfg.new.any() {
        // With Control Sweeps (E0422) each note takes its level's settings;
        // the bound is the largest over the swept ranges' corners (the
        // trill rate). With every function off they drive nothing: v0.8.
        cfg.swept = Rebuild::of(p, b);
        if let Some(rb) = &cfg.swept {
            for c in rb.corners(b.ctx, slide_cfg) {
                new_events = new_events.max(slide_new_events(&c, b));
            }
        }
    }
    let new = &cfg.new;
    let notes_bound = match cfg.as_notes {
        Some(_) => count.saturating_mul(98),
        None => count,
    };
    let total_events = if cfg.as_notes.is_none() {
        total_events.saturating_add(new_events)
    } else {
        new_events
    };
    if total_events > super::super::output::MAX_EVENTS as u64 {
        return Err(TransformError::TooLarge(format!(
            "The glides would write about {} pitch-bend events; select fewer notes or use fewer bend points.",
            crate::midi::group_digits(total_events)
        )));
    }
    let v09 = new.any();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            if v09 {
                cfg.run_v09(src, out)
            } else {
                cfg.run_v08(src, out)
            }
        },
        growth: Growth::Generate(notes_bound),
        estimate: Some(notes_bound),
    })
}

/// Points per cycle of a triangle or sine trill (n04).
const TRILL_POINTS: u64 = 8;

/// One bend point of a unit: tick, semitones from the centre, and whether
/// thinning must keep it (a gesture's first or last point, a re-centre).
#[derive(Clone, Copy, Debug)]
struct Pt {
    tick: u64,
    semis: f64,
    keep: bool,
}

/// The notes one pitch-bend gesture belongs to — a glided note (with its
/// release bend and trill), or a bend chain (n07) — on one channel.
#[derive(Clone, Debug)]
struct Unit {
    track: u16,
    /// The channel (n10 may move it).
    ch: u8,
    /// Selected-note indices (time order).
    notes: Vec<usize>,
    start: u64,
    end: u64,
    sub_pos: usize,
    pb: Vec<Pt>,
    /// n05 points (tick, controller, value).
    cc: Vec<(u64, u8, i64)>,
    /// v0.8 range of the unit (bend range, [a8]).
    range: f64,
    /// Glides in the unit (for the summary).
    glides: u64,
    /// Moved by the allocation (n10, or n09's chord voices).
    alloc: bool,
    /// The anchor pitch notes of a chain are played at (n07).
    anchor: Option<u8>,
    /// The farthest its bends may go (semitones): v0.8's rule — the
    /// interval + overshoot + the largest node + vibrato depth — and the
    /// release depth and trill interval.
    bound: f64,
}

/// What a glided note's own glide is (before chains).
#[derive(Clone, Copy, Debug)]
struct GlidePlan {
    /// Semitones from the target at the glide's start.
    interval: f64,
    len: u64,
}

impl SlideCfg {
    /// The v0.8 range of note `n` (the bend range, or [a8] by its
    /// subchannel position).
    fn range_of(&self, src: &Source, n: &TNote) -> f64 {
        let r = if self.sub_ranges.is_empty() {
            self.range
        } else {
            let k = src
                .track(n.track)
                .and_then(|t| t.sub_index(n.sub))
                .unwrap_or(0);
            self.sub_ranges[k % self.sub_ranges.len()].max(1)
        };
        r as f64
    }

    /// The v0.8 kernel (every v0.9 function off).
    fn run_v08(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let (fixed, time, steps, range, rpn) =
            (self.fixed, self.time, self.steps, self.range, self.rpn);
        let (vib, conflict, as_notes, ppq) = (self.vib, self.conflict, self.as_notes, self.ppq);
        let ns = &src.notes;
        // Previous selected note per (track, channel).
        let mut order: Vec<usize> = (0..ns.len()).collect();
        order.sort_by_key(|&i| (ns[i].track, ns[i].ch, ns[i].start, ns[i].pitch, i));
        let mut prev: Vec<Option<u8>> = vec![None; ns.len()];
        for w in order.windows(2) {
            let (a, b2) = (&ns[w[0]], &ns[w[1]]);
            if (a.track, a.ch) == (b2.track, b2.ch) && a.start < b2.start {
                prev[w[1]] = Some(a.pitch);
            } else if (a.track, a.ch) == (b2.track, b2.ch) {
                prev[w[1]] = prev[w[0]];
            }
        }
        // Every note sounding on a channel (selection + context), for
        // conflicts.
        let mut sounding: Vec<(u16, u8, u64, u64)> = ns
            .iter()
            .chain(src.context.iter())
            .map(|n| (n.track, n.ch, n.start, n.end()))
            .collect();
        sounding.sort_unstable();
        let others_during = |n: &TNote, a: u64, b2: u64| -> Option<u64> {
            // Earliest start (≥ a) of a conflicting window, or a when a note
            // other than `n` sounds at a.
            let lo = sounding.partition_point(|x| (x.0, x.1) < (n.track, n.ch));
            let hi = sounding.partition_point(|x| (x.0, x.1) <= (n.track, n.ch));
            let mut first: Option<u64> = None;
            for x in &sounding[lo..hi] {
                if x.2 >= b2 {
                    break;
                }
                if (x.2, x.3) == (n.start, n.end()) {
                    continue;
                }
                if x.3 > a && x.2 < b2 {
                    let at = x.2.max(a);
                    first = Some(first.map_or(at, |f: u64| f.min(at)));
                }
            }
            first
        };
        let mut rpn_done: Vec<(u16, u8)> = Vec::new();
        let (mut glides, mut skipped_range, mut skipped_conflict) = (0u64, 0u64, 0u64);
        for (i, n) in ns.iter().enumerate() {
            out.set_dim(0);
            if as_notes.is_none() {
                out.emit(
                    i128::from(n.start),
                    i128::from(n.dur),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                );
            }
            let from = match fixed {
                Some(iv) => Some(i64::from(n.pitch) + iv),
                None => prev[i].map(i64::from),
            };
            let Some(from) = from else {
                if as_notes.is_some() {
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        i64::from(n.pitch),
                        i64::from(n.vel),
                        n,
                    );
                }
                continue;
            };
            let interval = (from - i64::from(n.pitch)) as f64;
            let r = self.range_of(src, n);
            let mut len = time.min(n.dur).max(1);
            if let Some(mode) = conflict
                && let Some(at) = others_during(n, n.start, n.start + len)
            {
                if mode == 0 || at <= n.start + 1 {
                    skipped_conflict += 1;
                    if as_notes.is_some() {
                        out.emit(
                            i128::from(n.start),
                            i128::from(n.dur),
                            i64::from(n.pitch),
                            i64::from(n.vel),
                            n,
                        );
                    }
                    continue;
                }
                len = (at - n.start).max(1);
            }
            let peak = interval.abs()
                + self.glide.over.unwrap_or(0.0)
                + self.glide.nodes_max()
                + vib.map_or(0.0, |v| v.0);
            if as_notes.is_none() && peak > r + 1e-9 {
                skipped_range += 1;
                continue;
            }
            glides += 1;
            if let Some(min_len) = as_notes {
                self.emit_steps(n, interval, len, min_len, out);
                out.check()?;
                continue;
            }
            if rpn && !rpn_done.contains(&(n.track, n.ch)) {
                rpn_done.push((n.track, n.ch));
                emit_rpn(out, n.track, n.ch, n.start, r as i64);
            }
            for j in 0..steps {
                let u = j as f64 / (steps - 1) as f64;
                let t = n.start + (len as f64 * u).round() as u64;
                let v = if j + 1 == steps {
                    0.0
                } else {
                    self.glide.offset(interval, u)
                };
                out.emit_event(n.track, Lane::PitchBend, n.ch, i128::from(t), bend(v, r));
            }
            if let Some((depth, rate)) = vib {
                // Vibrato after the glide until the note ends (16 points per
                // cycle, at most 256), then back to the centre.
                let cycle = (ppq as f64 / rate).max(2.0);
                let glide_end = n.start.saturating_add(len);
                let span = n.end().saturating_sub(glide_end) as f64;
                let pts = ((span / cycle) * 16.0).min(256.0) as u64;
                for j in 1..=pts {
                    let off = (span * j as f64 / pts as f64) as u64;
                    let t = glide_end.saturating_add(off);
                    let ph = off as f64 / cycle;
                    let v = depth * (ph * std::f64::consts::TAU).sin();
                    out.emit_event(n.track, Lane::PitchBend, n.ch, i128::from(t), bend(v, r));
                }
                out.emit_event(n.track, Lane::PitchBend, n.ch, i128::from(n.end()), 8192);
            }
            out.check()?;
        }
        out.message = Some(summary(
            glides,
            skipped_range,
            skipped_conflict,
            range,
            as_notes.is_none(),
            &[],
        ));
        Ok(())
    }

    /// [a10]: the glide as chromatic note steps — one note per distinct
    /// semitone on the path — then the note itself.
    fn emit_steps(&self, n: &TNote, interval: f64, len: u64, min_len: u64, out: &mut Output) {
        let k = self.steps.min(len / min_len).max(1);
        let mut last: Option<i64> = None;
        let mut seg_start = n.start;
        // V09-0143: the driver dimension is the step along the glide (0 =
        // the first; the note itself comes last).
        let mut seg = 0u32;
        for j in 0..k {
            let t = n.start + len * j / k;
            let v = self.glide.offset(interval, j as f64 / k as f64).round() as i64;
            if last != Some(v) {
                if let Some(lv) = last {
                    out.set_dim(seg);
                    seg += 1;
                    out.emit(
                        i128::from(seg_start),
                        i128::from(t - seg_start),
                        i64::from(n.pitch) + lv,
                        i64::from(n.vel),
                        n,
                    );
                }
                last = Some(v);
                seg_start = t;
            }
        }
        if let Some(lv) = last
            && lv != 0
        {
            let t = n.start + len;
            out.set_dim(seg);
            seg += 1;
            out.emit(
                i128::from(seg_start),
                i128::from(t - seg_start),
                i64::from(n.pitch) + lv,
                i64::from(n.vel),
                n,
            );
            seg_start = t;
        }
        out.set_dim(seg);
        out.emit(
            i128::from(seg_start),
            i128::from(n.end().saturating_sub(seg_start).max(1)),
            i64::from(n.pitch),
            i64::from(n.vel),
            n,
        );
    }

    /// The glide's own points (and [a7]'s vibrato after it) for a glide of
    /// `g` into note `n`, every point moved by `base` semitones (a chain's
    /// offset); the vibrato stops at `stop`.
    fn glide_points(&self, n: &TNote, g: GlidePlan, base: f64, stop: u64, pb: &mut Vec<Pt>) {
        let steps = self.steps;
        for j in 0..steps {
            let u = j as f64 / (steps - 1) as f64;
            let t = n.start.saturating_add((g.len as f64 * u).round() as u64);
            let last = j + 1 == steps;
            let v = if last {
                0.0
            } else {
                self.glide.offset(g.interval, u)
            };
            pb.push(Pt {
                tick: t,
                semis: base + v,
                keep: j == 0 || last,
            });
        }
        if let Some((depth, rate)) = self.vib {
            let cycle = (self.ppq as f64 / rate).max(2.0);
            let glide_end = n.start.saturating_add(g.len);
            let span = stop.saturating_sub(glide_end) as f64;
            let pts = ((span / cycle) * 16.0).min(256.0) as u64;
            for j in 1..=pts {
                let off = (span * j as f64 / pts as f64) as u64;
                let ph = off as f64 / cycle;
                pb.push(Pt {
                    tick: glide_end.saturating_add(off),
                    semis: base + depth * (ph * std::f64::consts::TAU).sin(),
                    keep: false,
                });
            }
            pb.push(Pt {
                tick: stop,
                semis: base,
                keep: true,
            });
        }
    }

    /// The glide source of every selected note: the fixed interval, the
    /// previous selected note on the channel (v0.8), n06's rule, and n09's
    /// matching voice for chords (n09 wins, then n06).
    fn sources<'a>(&self, src: &Source, at: &impl Fn(usize) -> &'a SlideCfg) -> Vec<Option<i64>> {
        let ns = &src.notes;
        let mut from: Vec<Option<i64>> = match self.fixed {
            Some(iv) => ns.iter().map(|n| Some(i64::from(n.pitch) + iv)).collect(),
            None => {
                let mut order: Vec<usize> = (0..ns.len()).collect();
                order.sort_by_key(|&i| (ns[i].track, ns[i].ch, ns[i].start, ns[i].pitch, i));
                let mut prev: Vec<Option<i64>> = vec![None; ns.len()];
                for w in order.windows(2) {
                    let (a, b) = (&ns[w[0]], &ns[w[1]]);
                    if (a.track, a.ch) == (b.track, b.ch) && a.start < b.start {
                        prev[w[1]] = Some(i64::from(a.pitch));
                    } else if (a.track, a.ch) == (b.track, b.ch) {
                        prev[w[1]] = prev[w[0]];
                    }
                }
                prev
            }
        };
        if let Some((rule, window)) = self.new.source {
            // Each note's window (its level's under Control Sweeps).
            from = if self.swept.is_some() {
                source_by_rule(src, rule, |i| at(i).new.source.map_or(window, |x| x.1))
            } else {
                source_by_rule(src, rule, |_| window)
            };
        }
        if let Some((nearest, max, tol)) = self.new.voices {
            // Chords per track (selected notes, onsets within tolerance).
            let mut idx: Vec<usize> = (0..ns.len()).collect();
            idx.sort_by_key(|&i| (ns[i].track, ns[i].start, ns[i].pitch, i));
            let mut k = 0;
            while k < idx.len() {
                let t = ns[idx[k]].track;
                let e = k + idx[k..].partition_point(|&i| ns[i].track == t);
                let groups = ChordGroups::of(ns, &idx[k..e], tol);
                for g in 1..groups.len() {
                    let (cur, prev) = (groups.get(g), groups.get(g - 1));
                    if cur.len() < 2 {
                        continue;
                    }
                    let mut pv: Vec<i64> = prev.iter().map(|&i| i64::from(ns[i].pitch)).collect();
                    pv.sort_unstable();
                    let mut cv: Vec<usize> = cur.to_vec();
                    cv.sort_by_key(|&i| (ns[i].pitch, i));
                    let (m, c) = (pv.len(), cv.len());
                    for (r, &i) in cv.iter().enumerate() {
                        let p = i64::from(ns[i].pitch);
                        let s = if nearest {
                            *pv.iter().min_by_key(|&&q| ((q - p).abs(), q)).unwrap()
                        } else {
                            // Rank r of c ↔ rank round(r·(m − 1)/(c − 1)).
                            pv[((r * (m - 1)) as f64 / (c - 1) as f64).round() as usize]
                        };
                        from[i] = ((s - p).abs() <= max).then_some(s);
                    }
                }
                k = e;
            }
        }
        from
    }

    /// The v0.9 kernel: plan bend units, allocate channels, choose ranges,
    /// write, thin.
    fn run_v09(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let ns = &src.notes;
        let new = &self.new;
        // Each note's settings (its level's under Control Sweeps, E0422).
        let lv: Option<PerLevel<SlideCfg>> = self
            .swept
            .as_ref()
            .map(|r| r.levels(&self.ctx, src, slide_cfg));
        let at_fn = |i: usize| lv.as_ref().map_or(self, |l| l.at(i));
        let at = &at_fn;
        let from = self.sources(src, at);
        let mut tt = TempoTime::new(&self.ctx);
        // Sounding notes per (track, channel) for [a9]'s conflicts.
        let mut sounding: Vec<(u16, u8, u64, u64)> = Vec::new();
        if self.conflict.is_some() {
            sounding = ns
                .iter()
                .chain(src.context.iter())
                .map(|n| (n.track, n.ch, n.start, n.end()))
                .collect();
            sounding.sort_unstable();
        }
        let conflict_at = |n: &TNote, a: u64, b: u64| -> Option<u64> {
            let lo = sounding.partition_point(|x| (x.0, x.1) < (n.track, n.ch));
            let hi = sounding.partition_point(|x| (x.0, x.1) <= (n.track, n.ch));
            let mut first: Option<u64> = None;
            for x in &sounding[lo..hi] {
                if x.2 >= b {
                    break;
                }
                if (x.2, x.3) == (n.start, n.end()) {
                    continue;
                }
                if x.3 > a && x.2 < b {
                    let at = x.2.max(a);
                    first = Some(first.map_or(at, |f: u64| f.min(at)));
                }
            }
            first
        };
        let mut c = Counts::default();
        // 1. Each note's glide (source, n02 window, n01 / v0.8 time, [a9]).
        let mut plan: Vec<Option<GlidePlan>> = vec![None; ns.len()];
        let mut stepped: Vec<bool> = vec![false; ns.len()];
        for (i, n) in ns.iter().enumerate() {
            let Some(f) = from[i] else { continue };
            let semis = f - i64::from(n.pitch);
            let new = &at(i).new;
            if let Some((lo, hi, dir)) = new.window {
                let ok_dir = match dir {
                    0 => semis < 0,
                    1 => semis > 0,
                    _ => true,
                };
                if !ok_dir || semis.abs() < lo || semis.abs() > hi {
                    c.window += 1;
                    continue;
                }
            }
            let time = match new.rt {
                Some((ms, per, lo, hi)) => {
                    let t = (ms + per * semis.abs() as f64).clamp(lo.min(hi), lo.max(hi));
                    tt.ticks_after(n.start, (t * 1000.0).round() as u64)
                }
                None => self.time,
            };
            let mut len = time.min(n.dur).max(1);
            if let Some(mode) = self.conflict
                && let Some(at) = conflict_at(n, n.start, n.start.saturating_add(len))
            {
                if mode == 0 || at <= n.start.saturating_add(1) {
                    c.conflict += 1;
                    continue;
                }
                len = (at - n.start).max(1);
            }
            plan[i] = Some(GlidePlan {
                interval: semis as f64,
                len,
            });
            stepped[i] = self.as_notes.is_some();
        }
        // 2. Units: chains (n07) or one per bending note.
        let mut units = self.units(src, &plan, &stepped, &mut c, at);
        // 3. Channels (n10 for every unit, n09's voices always).
        if new.alloc_on || new.voices.is_some() {
            allocate(src, &mut units, &new.alloc, &mut c);
        }
        // 4. Ranges: n08's (per channel or per change) or the v0.8 range.
        let mut rpns: Vec<(u16, u8, u64, i64)> = Vec::new();
        let ranges = self.ranges(&units, &mut rpns, &mut c);
        // 5. Notes (chains re-pitched, moved channels), the steps of [a10].
        let mut note_of: Vec<Option<(u8, Option<u8>)>> = vec![None; ns.len()];
        for (u, r) in units.iter().zip(&ranges) {
            if r.is_some() {
                for &i in &u.notes {
                    note_of[i] = Some((u.ch, u.anchor));
                }
            }
        }
        for (i, n) in ns.iter().enumerate() {
            out.set_dim(0);
            if stepped[i]
                && let Some(g) = plan[i]
            {
                self.emit_steps(n, g.interval, g.len, self.as_notes.unwrap_or(1), out);
                c.glides += 1;
                continue;
            }
            let mut m = *n;
            if let Some((ch, anchor)) = note_of[i] {
                m.ch = ch;
                if let Some(a) = anchor {
                    m.pitch = a;
                }
            }
            out.emit(
                i128::from(m.start),
                i128::from(m.dur),
                i64::from(m.pitch),
                i64::from(m.vel),
                &m,
            );
        }
        out.check()?;
        // 6. Events: range writes, then each unit's points (unit order =
        // its first note's order, as v0.8 wrote them).
        for &(track, ch, at, r) in &rpns {
            emit_rpn(out, track, ch, at, r);
        }
        let mut pbs: Vec<(u16, u8, u64, i64, bool)> = Vec::new();
        // Under Control Sweeps each point's thinning error is its unit's
        // (the unit's first note's level).
        let mut pb_unit: Vec<u32> = Vec::new();
        for (k, (u, r)) in units.iter().zip(&ranges).enumerate() {
            let Some(r) = *r else { continue };
            c.glides += u.glides;
            for p in &u.pb {
                pbs.push((u.track, u.ch, p.tick, bend(p.semis, r), p.keep));
            }
            if lv.is_some() {
                pb_unit.resize(pbs.len(), k as u32);
            }
            for &(t, cc, v) in &u.cc {
                out.emit_event(u.track, Lane::Cc(cc), u.ch, i128::from(t), v);
            }
        }
        if let Some((cents, rate)) = new.thin {
            let before = pbs.len();
            let cents_of = |k: usize| match pb_unit.get(k) {
                Some(&u) => at(units[u as usize].notes[0])
                    .new
                    .thin
                    .map_or(cents, |x| x.0),
                None => cents,
            };
            pbs = self.thin(pbs, &units, &ranges, &cents_of, rate, &mut tt);
            c.thinned = (before - pbs.len()) as u64;
        }
        for (track, ch, t, v, _) in pbs {
            out.emit_event(track, Lane::PitchBend, ch, i128::from(t), v);
        }
        out.check()?;
        let mut extra: Vec<String> = Vec::new();
        let g = crate::midi::group_digits;
        if c.window > 0 {
            extra.push(format!(
                "{} glide(s) lie outside the interval window.",
                g(c.window)
            ));
        }
        if c.no_channel > 0 {
            extra.push(format!(
                "{} glide(s) found no free channel in the pool and were skipped.",
                g(c.no_channel)
            ));
        }
        if c.thinned > 0 {
            extra.push(format!("{} bend point(s) thinned.", g(c.thinned)));
        }
        out.message = Some(summary(
            c.glides, c.range, c.conflict, self.range, true, &extra,
        ));
        Ok(())
    }

    /// The bend units: per (track, channel) in time order, each glided note
    /// with its release bend (n03) and trill (n04) — or, with n07, chains:
    /// from the note before a glide, consecutive glides played on that
    /// note's pitch with the bend carrying the line. Notes without a glide
    /// get a unit when they carry a release bend or a trill.
    fn units<'a>(
        &self,
        src: &Source,
        plan: &[Option<GlidePlan>],
        stepped: &[bool],
        c: &mut Counts,
        at: &impl Fn(usize) -> &'a SlideCfg,
    ) -> Vec<Unit> {
        let ns = &src.notes;
        let new = &self.new;
        let mut order: Vec<usize> = (0..ns.len()).collect();
        order.sort_by_key(|&i| (ns[i].track, ns[i].ch, ns[i].start, ns[i].pitch, i));
        let bends = |i: usize| {
            (plan[i].is_some() && !stepped[i]) || new.release.is_some() || new.trill.is_some()
        };
        let mut units: Vec<Unit> = Vec::new();
        // The last note per (track, channel) and its unit.
        let mut last: std::collections::HashMap<(u16, u8), (usize, Option<usize>)> =
            Default::default();
        for &i in &order {
            let n = &ns[i];
            let key = (n.track, n.ch);
            let g = plan[i].filter(|_| !stepped[i]);
            let range = self.chain_range(src, n);
            if let (Some((mode, rest)), Some(g), Some(&(li, lu))) = (new.chain, g, last.get(&key)) {
                let prev = &ns[li];
                let anchor = lu.and_then(|u| units[u].anchor).unwrap_or(prev.pitch);
                let base = f64::from(n.pitch) - f64::from(anchor);
                let from_off = f64::from(prev.pitch) - f64::from(anchor);
                let legato = n.start >= prev.end();
                let rest_break = mode != 0 && n.start.saturating_sub(prev.end()) >= rest;
                let me = at(i);
                let fits = base.abs() + me.extra_of() <= range + 1e-9;
                if legato && !rest_break && fits {
                    // Chain on: the note before the glide anchors it.
                    let ui = match lu {
                        Some(u) => u,
                        None => {
                            let mut u = at(li).unit(src, li, None, self.chain_range(src, prev));
                            u.pb.clear();
                            units.push(u);
                            units.len() - 1
                        }
                    };
                    let gl = GlidePlan {
                        interval: from_off - base,
                        len: g.len,
                    };
                    let stop = me.release_start(n, Some(g));
                    let u = &mut units[ui];
                    // A held offset is not re-centred inside the chain.
                    if u.anchor.is_none() {
                        if u.pb
                            .last()
                            .is_some_and(|p| p.semis == 0.0 && p.tick == prev.end())
                        {
                            u.pb.pop();
                        }
                        u.anchor = Some(anchor);
                    }
                    let mut pb = std::mem::take(&mut u.pb);
                    me.glide_points(n, gl, base, stop, &mut pb);
                    me.extras(n, Some(g), base, &mut pb);
                    let mut cc = std::mem::take(&mut units[ui].cc);
                    me.envelope(n, Some(g), &mut cc);
                    let u = &mut units[ui];
                    u.pb = pb;
                    u.cc = cc;
                    u.bound = u.bound.max(
                        from_off.abs().max(base.abs())
                            + self.glide.over.unwrap_or(0.0)
                            + self.glide.nodes_max()
                            + self.vib.map_or(0.0, |v| v.0),
                    );
                    u.bound = u.bound.max(base.abs() + me.extra_of());
                    u.notes.push(i);
                    u.end = u.end.max(n.end());
                    u.glides += 1;
                    last.insert(key, (i, Some(ui)));
                    continue;
                }
                if let Some(u) = lu {
                    close_chain(&mut units[u], prev);
                }
                if mode == 1 && legato && !rest_break && !fits {
                    // "At rests": a glide out of the range is not chained —
                    // the note is played without it.
                    c.range += 1;
                    let u = at(i).unit(src, i, None, range);
                    let lu = if u.pb.is_empty() {
                        None
                    } else {
                        units.push(u);
                        Some(units.len() - 1)
                    };
                    last.insert(key, (i, lu));
                    continue;
                }
            } else if let Some(&(li, Some(u))) = last.get(&key) {
                close_chain(&mut units[u], &ns[li]);
            }
            // A glide beyond the range (v0.8's bound) goes before its points
            // are made.
            let g = match g {
                Some(gl) if self.glide_bound(gl) > range + 1e-9 => {
                    c.range += 1;
                    None
                }
                g => g,
            };
            let lu = if (g.is_some() || new.release.is_some() || new.trill.is_some()) && bends(i) {
                let u = at(i).unit(src, i, g, range);
                if u.pb.is_empty() && u.cc.is_empty() {
                    None
                } else {
                    units.push(u);
                    Some(units.len() - 1)
                }
            } else {
                None
            };
            last.insert(key, (i, lu));
        }
        for (_, (li, lu)) in last {
            if let Some(u) = lu {
                close_chain(&mut units[u], &ns[li]);
            }
        }
        // The allocation moves every unit with n10 on, chord voices with n09.
        let voiced = new.voices.map(|(_, _, tol)| chord_members(ns, tol));
        for u in &mut units {
            u.alloc = new.alloc_on
                || voiced
                    .as_ref()
                    .is_some_and(|v| u.notes.iter().any(|&i| v[i]) && u.glides > 0);
        }
        // Units in the order v0.8 wrote them (their first note's order).
        units.sort_by_key(|u| u.notes[0]);
        units
    }

    /// The range a chain may use (n08's largest, else the note's v0.8
    /// range).
    fn chain_range(&self, src: &Source, n: &TNote) -> f64 {
        match self.new.auto_range {
            Some((max, _)) => max as f64,
            None => self.range_of(src, n),
        }
    }

    /// How far beyond a glide's interval its points go ([a4], [a6], [a7],
    /// n03, n04).
    fn extra_of(&self) -> f64 {
        let mut e = self.glide.extra() + self.vib.map_or(0.0, |v| v.0);
        if let Some((kind, depth, _, _)) = &self.new.release
            && *kind != 2
        {
            e = e.max(*depth as f64);
        }
        if let Some((iv, _, _, _)) = self.new.trill {
            e = e.max(iv.unsigned_abs() as f64);
        }
        e
    }

    /// v0.8's bound of a glide: interval + overshoot + largest node +
    /// vibrato depth.
    fn glide_bound(&self, g: GlidePlan) -> f64 {
        g.interval.abs()
            + self.glide.over.unwrap_or(0.0)
            + self.glide.nodes_max()
            + self.vib.map_or(0.0, |v| v.0)
    }

    /// One note's unit: its glide (if any), release bend, trill, envelope.
    fn unit(&self, src: &Source, i: usize, g: Option<GlidePlan>, range: f64) -> Unit {
        let n = &src.notes[i];
        let mut pb = Vec::with_capacity(self.steps as usize + 2);
        let mut cc = Vec::new();
        let stop = self.release_start(n, g);
        if let Some(g) = g {
            self.glide_points(n, g, 0.0, stop, &mut pb);
            self.envelope(n, Some(g), &mut cc);
        }
        self.extras(n, g, 0.0, &mut pb);
        if pb.last().is_some_and(|p| p.semis != 0.0) {
            // Back to the centre when the note ends.
            pb.push(Pt {
                tick: n.end(),
                semis: 0.0,
                keep: true,
            });
        }
        let mut bound = g.map_or(0.0, |g| self.glide_bound(g));
        if let Some((kind, depth, _, _)) = &self.new.release
            && (*kind != 2 || g.is_none())
        {
            bound = bound.max(*depth as f64);
        }
        if let Some((iv, _, _, _)) = self.new.trill {
            bound = bound.max(iv.unsigned_abs() as f64);
        }
        Unit {
            track: n.track,
            ch: n.ch,
            notes: vec![i],
            start: n.start,
            end: n.end(),
            sub_pos: src
                .track(n.track)
                .and_then(|t| t.sub_index(n.sub))
                .unwrap_or(0),
            pb,
            cc,
            range,
            glides: u64::from(g.is_some()),
            alloc: false,
            anchor: None,
            bound,
        }
    }

    /// Where n03's fall / doit begins (the note's end without it).
    fn release_start(&self, n: &TNote, g: Option<GlidePlan>) -> u64 {
        match &self.new.release {
            Some((kind, _, share, _)) if *kind != 2 => {
                let glide_end = n.start.saturating_add(g.map_or(0, |g| g.len));
                let rs = n.end() - ((n.dur as f64 * share).round() as u64).min(n.dur);
                rs.max(glide_end).min(n.end())
            }
            _ => n.end(),
        }
    }

    /// n03's scoop (notes without a glide), n04's trill, then n03's fall /
    /// doit on note `n` (after its glide), moved by `base` semitones.
    fn extras(&self, n: &TNote, g: Option<GlidePlan>, base: f64, pb: &mut Vec<Pt>) {
        let steps = self.steps;
        let mut bend_in_end = n.start.saturating_add(g.map_or(0, |g| g.len));
        let rs = self.release_start(n, g);
        if let Some((2, depth, share, shape)) = &self.new.release
            && g.is_none()
        {
            // A scoop up into the note from `depth` below.
            let depth = *depth as f64;
            let len = ((n.dur as f64 * share).round() as u64).clamp(1, n.dur.max(1));
            for j in 0..steps {
                let u = j as f64 / (steps - 1) as f64;
                pb.push(Pt {
                    tick: n.start.saturating_add((len as f64 * u).round() as u64),
                    semis: base - depth * (1.0 - shape.eval(u).clamp(0.0, 1.0)),
                    keep: j == 0 || j + 1 == steps,
                });
            }
            bend_in_end = n.start.saturating_add(len);
        }
        if let Some((iv, rate, wave, delay)) = self.new.trill {
            let t0 = bend_in_end.max(n.start.saturating_add(delay));
            if t0 < rs {
                let cycle = (self.ppq as f64 / rate).max(2.0);
                let iv = iv as f64;
                if wave == 0 {
                    // Square: the note, then the interval, each half a cycle.
                    let halves = ((rs - t0) as f64 / (cycle / 2.0)).ceil() as u64;
                    for k in 0..halves {
                        let t = t0.saturating_add((k as f64 * cycle / 2.0).round() as u64);
                        if t >= rs {
                            break;
                        }
                        pb.push(Pt {
                            tick: t,
                            semis: base + if k % 2 == 1 { iv } else { 0.0 },
                            keep: true,
                        });
                    }
                } else {
                    let span = (rs - t0) as f64;
                    let pts = (span / cycle * TRILL_POINTS as f64).ceil() as u64;
                    for j in 0..pts {
                        let off = cycle * j as f64 / TRILL_POINTS as f64;
                        let ph = (off / cycle).fract();
                        let w = if wave == 1 {
                            1.0 - (2.0 * ph - 1.0).abs()
                        } else {
                            (1.0 - (std::f64::consts::TAU * ph).cos()) / 2.0
                        };
                        pb.push(Pt {
                            tick: t0.saturating_add(off.round() as u64),
                            semis: base + iv * w,
                            keep: j == 0,
                        });
                    }
                }
                pb.push(Pt {
                    tick: rs,
                    semis: base,
                    keep: true,
                });
            }
        }
        if let Some((kind, depth, _, shape)) = &self.new.release
            && *kind != 2
            && rs < n.end()
        {
            // A fall (down) or doit (up) to the depth at the note's end.
            let sign = if *kind == 0 { -1.0 } else { 1.0 };
            let last = n.end() - 1;
            for j in 0..steps {
                let u = j as f64 / (steps - 1) as f64;
                pb.push(Pt {
                    tick: rs.saturating_add(((last.max(rs) - rs) as f64 * u).round() as u64),
                    semis: base + sign * *depth as f64 * shape.eval(u).clamp(0.0, 1.0),
                    keep: j == 0 || j + 1 == steps,
                });
            }
        }
    }

    /// n05: the controller over the glide (`steps` points).
    fn envelope(&self, n: &TNote, g: Option<GlidePlan>, cc: &mut Vec<(u64, u8, i64)>) {
        let (Some((num, curve, lo, hi)), Some(g)) = (&self.new.env, g) else {
            return;
        };
        for j in 0..self.steps {
            let u = j as f64 / (self.steps - 1) as f64;
            // B1-0113: an overshooting envelope goes past Low / High (the
            // controller value is still 0 – 127).
            let v = *lo as f64 + (*hi - *lo) as f64 * curve.eval(u);
            cc.push((
                n.start.saturating_add((g.len as f64 * u).round() as u64),
                *num,
                v.round() as i64,
            ));
        }
    }

    /// Each unit's bend range — `None` when it is skipped (out of range) —
    /// and the RPN writes: n08 per channel (the smallest covering all its
    /// units up to the largest) or before each change; else the v0.8 range,
    /// written once per channel when asked.
    fn ranges(
        &self,
        units: &[Unit],
        rpns: &mut Vec<(u16, u8, u64, i64)>,
        c: &mut Counts,
    ) -> Vec<Option<f64>> {
        let need = |u: &Unit| (u.bound - 1e-9).ceil().max(1.0) as i64;
        match self.new.auto_range {
            None => {
                let mut done: Vec<(u16, u8)> = Vec::new();
                units
                    .iter()
                    .map(|u| {
                        if u.bound > u.range + 1e-9 {
                            c.range += u.glides.max(1);
                            return None;
                        }
                        if self.rpn && u.glides > 0 && !done.contains(&(u.track, u.ch)) {
                            done.push((u.track, u.ch));
                            rpns.push((u.track, u.ch, u.start, u.range as i64));
                        }
                        Some(u.range)
                    })
                    .collect()
            }
            Some((max, each)) => {
                let mut per: std::collections::BTreeMap<(u16, u8), i64> = Default::default();
                if !each {
                    for u in units {
                        let r = need(u);
                        if r <= max {
                            let e = per.entry((u.track, u.ch)).or_insert(1);
                            *e = (*e).max(r);
                        }
                    }
                }
                let mut current: std::collections::BTreeMap<(u16, u8), i64> = Default::default();
                let mut by_time: Vec<usize> = (0..units.len()).collect();
                by_time.sort_by_key(|&k| (units[k].start, k));
                let mut out = vec![None; units.len()];
                for k in by_time {
                    let u = &units[k];
                    let r = need(u);
                    if r > max {
                        c.range += u.glides.max(1);
                        continue;
                    }
                    let r = if each { r } else { per[&(u.track, u.ch)] };
                    if current.get(&(u.track, u.ch)) != Some(&r) {
                        current.insert((u.track, u.ch), r);
                        rpns.push((u.track, u.ch, u.start, r));
                    }
                    out[k] = Some(r as f64);
                }
                out
            }
        }
    }

    /// n11: per (track, channel), points whose value stays within the error
    /// of the last kept one are dropped (a bend holds its value until the
    /// next point), then at most `rate` points in any second (through the
    /// tempo map; `density_window::sweep_cap_keep`), the smallest changes
    /// first; gesture ends and re-centres always stay.
    #[allow(clippy::type_complexity)]
    fn thin(
        &self,
        pbs: Vec<(u16, u8, u64, i64, bool)>,
        units: &[Unit],
        ranges: &[Option<f64>],
        cents: &dyn Fn(usize) -> f64,
        rate: u64,
        tt: &mut TempoTime,
    ) -> Vec<(u16, u8, u64, i64, bool)> {
        // The smallest range on each channel turns the error into bend steps.
        let mut range_of: std::collections::BTreeMap<(u16, u8), f64> = Default::default();
        for (u, r) in units.iter().zip(ranges) {
            if let Some(r) = r {
                let e = range_of.entry((u.track, u.ch)).or_insert(*r);
                *e = e.min(*r);
            }
        }
        // One point per tick (the last written wins, as the run keeps it).
        let mut v: Vec<(usize, (u16, u8, u64, i64, bool))> = pbs.into_iter().enumerate().collect();
        v.sort_by_key(|(k, e)| (e.0, e.1, e.2, std::cmp::Reverse(*k)));
        v.dedup_by_key(|(_, e)| (e.0, e.1, e.2));
        let mut out = Vec::with_capacity(v.len());
        let mut a = 0;
        while a < v.len() {
            let key = (v[a].1.0, v[a].1.1);
            let b = a + v[a..].partition_point(|(_, e)| (e.0, e.1) == key);
            let r = range_of.get(&key).copied().unwrap_or(2.0);
            let mut kept: Vec<(u16, u8, u64, i64, bool)> = Vec::new();
            for (k, e) in &v[a..b] {
                // The point's own error (its unit's level's, E0422).
                let tol = cents(*k) / r * 8192.0;
                let last = kept.last().map(|x| x.3);
                if e.4 || last.is_none_or(|l| ((e.3 - l) as f64).abs() > tol) {
                    kept.push(*e);
                }
            }
            // At most `rate` points per second, the smallest changes going
            // first where a second is over.
            let times: Vec<u64> = kept.iter().map(|e| tt.micros_at(e.2)).collect();
            let prio: Vec<u64> = (0..kept.len())
                .map(|k| {
                    if k == 0 {
                        0
                    } else {
                        (kept[k].3 - kept[k - 1].3).unsigned_abs()
                    }
                })
                .collect();
            let removable: Vec<bool> = kept.iter().map(|e| !e.4).collect();
            let keep = sweep_cap_keep(&times, 1_000_000, rate as usize, &prio, &removable);
            out.extend(kept.into_iter().zip(keep).filter(|x| x.1).map(|x| x.0));
            a = b;
        }
        out
    }
}

/// A chain's end: back to the centre after its last note.
fn close_chain(u: &mut Unit, last: &TNote) {
    if u.anchor.is_some() && u.pb.last().is_some_and(|p| p.semis != 0.0) {
        u.pb.push(Pt {
            tick: last.end(),
            semis: 0.0,
            keep: true,
        });
    }
}

/// Which selected notes are voices of a chord of two or more (n09).
fn chord_members(ns: &[TNote], tol: u64) -> Vec<bool> {
    let mut idx: Vec<usize> = (0..ns.len()).collect();
    idx.sort_by_key(|&i| (ns[i].track, ns[i].start, ns[i].pitch, i));
    let mut out = vec![false; ns.len()];
    let mut k = 0;
    while k < idx.len() {
        let t = ns[idx[k]].track;
        let e = k + idx[k..].partition_point(|&i| ns[i].track == t);
        for g in ChordGroups::of(ns, &idx[k..e], tol).iter() {
            if g.len() > 1 {
                for &i in g {
                    out[i] = true;
                }
            }
        }
        k = e;
    }
    out
}

/// n06: the glide source of every selected note from the notes sounding
/// before it (selected and context, same track): those that started before
/// it and still sound or ended at most `window` ticks before — the pitch
/// nearest it (ties: the lower), or the highest; or the previous note (the
/// latest start before it, its highest pitch) on its subchannel or track,
/// within the window. One sweep in time order.
fn source_by_rule(src: &Source, rule: usize, window: impl Fn(usize) -> u64) -> Vec<Option<i64>> {
    use std::collections::HashMap;
    let ns = &src.notes;
    // Pool: (start, end, pitch, track, sub, selected index).
    let mut pool: Vec<(u64, u64, u8, u16, u32, usize)> = ns
        .iter()
        .enumerate()
        .map(|(i, n)| (n.start, n.end(), n.pitch, n.track, n.sub, i))
        .chain(
            src.context
                .iter()
                .map(|n| (n.start, n.end(), n.pitch, n.track, n.sub, usize::MAX)),
        )
        .collect();
    pool.sort_unstable_by_key(|x| (x.0, x.3, x.2, x.5));
    let mut out = vec![None; ns.len()];
    // Candidates per track: per pitch the latest end of the notes started
    // before — a candidate of a note starting at `s` when it ended at most
    // the note's window before (E0422: each note's own window; the set the
    // S35 expiry kept).
    let mut ends: HashMap<u16, [Option<u64>; 256]> = HashMap::new();
    // The previous note per key: (start, end, highest pitch at that start).
    let mut last: HashMap<(u16, u32), (u64, u64, u8)> = HashMap::new();
    let mut a = 0;
    while a < pool.len() {
        let s = pool[a].0;
        let b = a + pool[a..].partition_point(|x| x.0 == s);
        for x in &pool[a..b] {
            let (_, _, p, tr, sub, i) = *x;
            if i == usize::MAX {
                continue;
            }
            let window = window(i);
            out[i] = match rule {
                0 | 3 => ends.get(&tr).and_then(|c| {
                    let have = |q: i64| {
                        (0..256).contains(&q)
                            && c[q as usize].is_some_and(|e| e.saturating_add(window) >= s)
                    };
                    if rule == 3 {
                        (0..256i64).rev().find(|&q| have(q))
                    } else {
                        let p = i64::from(p);
                        (0..256i64).find_map(|d| {
                            if have(p - d) {
                                Some(p - d)
                            } else if have(p + d) {
                                Some(p + d)
                            } else {
                                None
                            }
                        })
                    }
                }),
                _ => {
                    let key = if rule == 1 { (tr, sub) } else { (tr, u32::MAX) };
                    last.get(&key)
                        .filter(|l| l.1.saturating_add(window) >= s)
                        .map(|l| i64::from(l.2))
                }
            };
        }
        // Then this onset's notes become candidates for later ones.
        for x in &pool[a..b] {
            let (st, e, p, tr, sub, _) = *x;
            let c = &mut ends.entry(tr).or_insert([None; 256])[usize::from(p)];
            *c = Some(c.map_or(e, |x| x.max(e)));
            for key in [(tr, sub), (tr, u32::MAX)] {
                let l = last.entry(key).or_insert((st, e, p));
                if l.0 < st {
                    *l = (st, e, p);
                } else if l.0 == st && p >= l.2 {
                    *l = (st, l.1.max(e), p);
                }
            }
        }
        a = b;
    }
    out
}

/// n10: each unit to a free channel of the pool — no other note on it
/// overlapping the unit's notes, and none ending less than the release tail
/// before them — round robin, least recently used, or by subchannel
/// position; a unit that finds none keeps its channel and loses its bends.
fn allocate(src: &Source, units: &mut [Unit], alloc: &(Vec<u8>, usize, u64), c: &mut Counts) {
    let (pool, policy, tail) = (&alloc.0, alloc.1, alloc.2);
    if pool.is_empty() {
        for u in units.iter_mut().filter(|u| u.alloc) {
            u.pb.clear();
            u.cc.clear();
            u.glides = 0;
            c.no_channel += 1;
        }
        return;
    }
    let moved: std::collections::HashSet<usize> = units
        .iter()
        .filter(|u| u.alloc)
        .flat_map(|u| u.notes.iter().copied())
        .collect();
    // Fixed notes per (track, channel): (start, end + tail), sorted by
    // start, with the running largest end.
    let mut fixed: std::collections::HashMap<(u16, u8), Vec<(u64, u64)>> = Default::default();
    for (i, n) in src.notes.iter().enumerate() {
        if !moved.contains(&i) {
            fixed
                .entry((n.track, n.ch))
                .or_default()
                .push((n.start, n.end().saturating_add(tail)));
        }
    }
    for n in src.context.iter() {
        fixed
            .entry((n.track, n.ch))
            .or_default()
            .push((n.start, n.end().saturating_add(tail)));
    }
    let mut prefix: std::collections::HashMap<(u16, u8), (Vec<u64>, Vec<u64>)> = Default::default();
    for (k, mut v) in fixed {
        v.sort_unstable();
        let mut run = 0;
        let ends: Vec<u64> = v
            .iter()
            .map(|x| {
                run = run.max(x.1);
                run
            })
            .collect();
        prefix.insert(k, (v.into_iter().map(|x| x.0).collect(), ends));
    }
    let busy_fixed = |track: u16, ch: u8, s: u64, e: u64| -> bool {
        prefix.get(&(track, ch)).is_some_and(|(starts, ends)| {
            let k = starts.partition_point(|&x| x < e);
            k > 0 && ends[k - 1] > s
        })
    };
    // Allocated units: per (track, channel) the latest end + tail.
    let mut until: std::collections::HashMap<(u16, u8), u64> = Default::default();
    let mut rr: std::collections::HashMap<u16, usize> = Default::default();
    let mut order: Vec<usize> = (0..units.len()).filter(|&k| units[k].alloc).collect();
    order.sort_by_key(|&k| (units[k].start, k));
    for k in order {
        let (track, s, e, sub_pos) = (
            units[k].track,
            units[k].start,
            units[k].end,
            units[k].sub_pos,
        );
        let free = |ch: u8| {
            !busy_fixed(track, ch, s, e) && until.get(&(track, ch)).is_none_or(|&t| t <= s)
        };
        let pick = match policy {
            2 => Some(pool[sub_pos % pool.len()]).filter(|&ch| free(ch)),
            1 => pool
                .iter()
                .copied()
                .filter(|&ch| free(ch))
                .min_by_key(|&ch| (until.get(&(track, ch)).copied().unwrap_or(0), ch)),
            _ => {
                let at = rr.get(&track).copied().unwrap_or(0);
                (0..pool.len())
                    .map(|d| (at + d) % pool.len())
                    .find(|&j| free(pool[j]))
                    .map(|j| {
                        rr.insert(track, j + 1);
                        pool[j]
                    })
            }
        };
        match pick {
            Some(ch) => {
                units[k].ch = ch;
                until.insert((track, ch), e.saturating_add(tail));
            }
            None => {
                let u = &mut units[k];
                u.pb.clear();
                u.cc.clear();
                c.no_channel += u.glides.max(1);
                u.glides = 0;
                u.anchor = None;
            }
        }
    }
}

/// The range write (RPN 0) on channel `ch` just before `at`.
fn emit_rpn(out: &mut Output, track: u16, ch: u8, at: u64, r: i64) {
    let t0 = i128::from(at.saturating_sub(4));
    out.emit_event(track, Lane::Cc(101), ch, t0, 0);
    out.emit_event(track, Lane::Cc(100), ch, t0 + 1, 0);
    out.emit_event(track, Lane::Cc(6), ch, t0 + 2, r);
    out.emit_event(track, Lane::Cc(38), ch, t0 + 3, 0);
}

/// What the v0.9 run skipped or changed (for its summary).
#[derive(Default)]
struct Counts {
    glides: u64,
    range: u64,
    conflict: u64,
    window: u64,
    no_channel: u64,
    thinned: u64,
}

/// The summary sentence (the v0.8 wording, then the v0.9 counts).
fn summary(
    glides: u64,
    skipped_range: u64,
    skipped_conflict: u64,
    range: i64,
    bends: bool,
    extra: &[String],
) -> String {
    let g = crate::midi::group_digits;
    let mut m = format!("{} glide(s).", g(glides));
    if skipped_range > 0 {
        m.push_str(&format!(
            " {} glide(s) exceed the bend range of ±{range} semitones and were skipped; raise the bend range (and write it with RPN 0).",
            g(skipped_range)
        ));
    }
    if skipped_conflict > 0 {
        m.push_str(&format!(
            " {} glide(s) would bend other notes on the same channel and were skipped.",
            g(skipped_conflict)
        ));
    }
    for e in extra {
        m.push(' ');
        m.push_str(e);
    }
    if bends {
        m.push_str(" Pitch bend acts on the whole MIDI channel.");
    }
    m
}
