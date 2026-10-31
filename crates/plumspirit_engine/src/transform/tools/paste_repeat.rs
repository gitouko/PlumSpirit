//! Paste Repeat on the selected notes (v0.9 V09-S13 / S14, contract §088 –
//! §093; `docs/PASTE_REPEAT.md`): the notes adapter of the shared timing
//! core (`transform/repeat.rs`). Every selected note stays and gets one copy
//! per repeat *k* at `anchor + offset(k)`, the anchor being the selection's
//! first start, with every attribute kept (length, pitch, velocity,
//! channel, subchannel, group, mute). The platform supplies the window,
//! Preview (the same kernel previews and commits), presets, preflight and
//! one undo step.
//!
//! S14 adds the per-copy functions (§092 – §093). The plan's offsets become a
//! **copy list** once per run (never per note): every entry carries its
//! generation *k*, its offset and its gap to the previous copy, and what the
//! functions do to a note of that copy (pitch shift, subchannel offset, gate
//! factor, velocity) is computed per entry. Every function is off by
//! default: with all of them off the output is exactly S13's.

use super::super::TransformError;
use super::super::drivers::{SUB_WRAPS, SubWrap, move_sub};
use super::super::geom::{Geom, GeomBox};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::repeat::series::{CopySlot, Morph, copies, cumulative, fracture, place, tree};
use super::super::repeat::{
    Decimal, RepeatClock, RepeatSource, RepeatSpec, RepeatUnit, Spacing, plan,
};
use super::super::{Growth, Kernel, Output, Source, SourceTrack, TNote, boundary};
use super::{Contextual, Pointwise, boxed};

pub const PASTE_REPEAT_NOTES: ToolDef = ToolDef {
    id: "paste_repeat_notes",
    spec,
    build,
};

/// The unit choices, in `RepeatUnit::ALL` order.
pub const UNIT_LABELS: [&str; 11] = [
    "Bars",
    "Beats",
    "1/4 notes",
    "1/8 notes",
    "1/16 notes",
    "1/32 notes",
    "Frames",
    "Ticks",
    "Seconds",
    "Milliseconds",
    "Microseconds",
];

/// The default unit (the owner's reference dialog: 1.000 Seconds).
pub const DEFAULT_UNIT: u32 = 8;

/// The spacing choices.
pub const SPACING_LABELS: [&str; 2] = ["End to end", "Even spacing"];

/// How a morph (Gate, Velocity) runs over the series.
pub const MORPH_MODES: [&str; 2] = ["Start → end", "Graph"];

/// The names of the per-copy functions (§092 – §093), function 1 first.
pub const FUNCTIONS: [&str; 10] = [
    "Subchannel Offset",
    "Cumulative Pitch Vector",
    "Temporal Acceleration Curve",
    "Gate Morph",
    "Velocity / Expression Morph",
    "Alternating Mirror / Reverse",
    "Seeded Fracture / Omission",
    "Voice Phase Stagger",
    "Geometric Drift per Repeat",
    "Recursive Repeat Tree",
];

/// What function 6 changes on its copies.
pub const MIRROR_MODES: [&str; 3] = ["Mirror pitch order", "Reverse time order", "Both"];

/// What function 7 does with the copies its graph picks.
pub const FRACTURE_MODES: [&str; 3] = ["Omit", "Rearrange", "Omit and rearrange"];

/// How function 8 tells voices apart.
pub const VOICE_MODES: [&str; 2] = ["Chord voices", "Subchannels"];

/// Most copies a repeat tree (function 10) may make, all levels together.
pub const MAX_TREE_COPIES: u64 = 1_000_000;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PASTE_REPEAT_NOTES_HELP: &[(&str, &str)] = &[
    ("count", "How many copies are pasted after the selection."),
    (
        "spacing",
        "End to End pastes each copy right after the previous one; Even Spacing starts a copy every so many units.",
    ),
    (
        "unit",
        "The unit of the spacing: bars, beats, note values, frames, ticks or real time (seconds stay seconds under tempo changes).",
    ),
    (
        "a1",
        "Moves each copy to another subchannel (color); the MIDI channel never changes.",
    ),
    (
        "a1.wrap",
        "What happens past the last subchannel: cycle the track's list, wrap or reflect within a range, stop at the range's edge, or make new subchannels.",
    ),
    (
        "a1.lo",
        "The first subchannel of the range, by its number (1 = the first).",
    ),
    ("a1.n", "How many subchannels the range holds."),
    ("a2", "Transposes each copy further than the previous one."),
    (
        "a2.step",
        "How many semitones each copy moves from the previous one (12 = an octave).",
    ),
    (
        "a3",
        "Speeds the copies up or slows them down over the series with a curve.",
    ),
    (
        "a3.max",
        "The gap between copies at the top of the graph, as a share of the planned gap.",
    ),
    (
        "a4",
        "Changes the note lengths gradually from the first copy to the last.",
    ),
    (
        "a4.mode",
        "Start → End changes the length evenly; Graph follows the curve below.",
    ),
    (
        "a4.start",
        "The note length of the first copy, as a share of the original.",
    ),
    (
        "a4.end",
        "The note length of the last copy, as a share of the original.",
    ),
    (
        "a4.curve",
        "How the length moves from the first copy's (bottom) to the last copy's (top) over the series.",
    ),
    (
        "a5",
        "Changes the velocity gradually from the first copy to the last.",
    ),
    (
        "a5.mode",
        "Start → End changes the velocity evenly; Graph follows the curve below.",
    ),
    ("a5.start", "The velocity of the first copy."),
    ("a5.end", "The velocity of the last copy."),
    (
        "a5.curve",
        "How the velocity moves from the first copy's (bottom) to the last copy's (top) over the series.",
    ),
    ("a6", "Mirrors or reverses every n-th copy."),
    (
        "a6.mode",
        "What changes on those copies: the pitch order is mirrored, the time order is reversed, or both.",
    ),
    (
        "a6.every",
        "Which copies change: every n-th one, counting from copy 1.",
    ),
    (
        "a7",
        "Picks some copies at random (seeded) and leaves them out or swaps them around.",
    ),
    (
        "a7.mode",
        "What happens to the picked copies: they are left out, they swap their places, or both.",
    ),
    (
        "a8",
        "Shifts each voice of a copy by its own amount, so the voices are out of phase.",
    ),
    (
        "a8.by",
        "How voices are told apart: by their place in each chord (from the lowest note), or by subchannel.",
    ),
    (
        "a9",
        "Scales, slants and turns each copy a little more than the previous one.",
    ),
    (
        "a9.scale_x",
        "Each copy's length in time as a share of the previous copy's.",
    ),
    (
        "a9.scale_y",
        "Each copy's pitch range as a share of the previous copy's.",
    ),
    (
        "a9.skew_x",
        "How much more each copy slants in time than the previous one.",
    ),
    (
        "a9.skew_y",
        "How much more each copy slants in pitch than the previous one.",
    ),
    (
        "a10",
        "Repeats each copy again in smaller groups inside its gap, like a tree.",
    ),
    (
        "a10.depth",
        "How many levels of copies inside copies are made.",
    ),
    (
        "a10.children",
        "How many copies each block spawns on the next level.",
    ),
    (
        "a10.ratio",
        "How far apart a block's copies are, as a share of the gap of the level above.",
    ),
];

fn spec() -> ToolSpec {
    let mut params = vec![
        int(
            "count",
            "Number of times to paste",
            1,
            i64::from(super::super::repeat::MAX_COPIES),
            3,
        ),
        choice("spacing", "Paste spacing", &SPACING_LABELS, 0),
        text("every", "Paste every (Even spacing)", 24, "1").help(
            "A number: up to 6 decimals of a second, 3 of a millisecond; bars and beats take whole numbers.",
        ),
        choice("unit", "Unit", &UNIT_LABELS, DEFAULT_UNIT),
        // 1 — Subchannel Offset (§092).
        adv_switch(1, FUNCTIONS[0]),
        int("a1.offset", "Subchannels per copy", -256, 256, 1)
            .adv(1)
            .help("Copy k moves each note's logical subchannel by k × this. The MIDI channel never changes."),
        choice("a1.wrap", "Where subchannels go", SUB_WRAPS, 0).adv(1),
        int("a1.lo", "Range from subchannel", 1, 4096, 1).adv(1),
        int("a1.n", "Range size", 1, 4096, 4).adv(1),
        // 2 — Cumulative Pitch Vector.
        adv_switch(2, FUNCTIONS[1]),
        int("a2.step", "Semitones per copy", -127, 127, 12)
            .keyed()
            .adv(2),
        list(
            "a2.seq",
            "Interval sequence (cycled; empty: the step)",
            -127,
            127,
            64,
            &[],
        )
        .adv(2)
        .help("Copy k moves by the first k intervals added up (2, 2, 1, … climbs a scale)."),
        // 3 — Temporal Acceleration Curve.
        adv_switch(3, FUNCTIONS[2]),
        curve(
            "a3.curve",
            "Spacing over the series (middle: as planned)",
            Curve::new(vec![(0.0, 0.5), (1.0, 0.5)]),
        )
        .adv(3)
        .help("x: the copy's place in the series; y: its gap to the previous copy — 0 none, the middle 100 %, the top the maximum."),
        float("a3.max", "Spacing at the top of the graph", 100.0, 1000.0, 200.0, "%").adv(3),
        // 4 — Gate Morph.
        adv_switch(4, FUNCTIONS[3]),
        choice("a4.mode", "Gate over the series", &MORPH_MODES, 0).adv(4),
        float("a4.start", "Length of copy 1", 1.0, 400.0, 100.0, "%").adv(4),
        float("a4.end", "Length of the last copy", 1.0, 400.0, 50.0, "%").adv(4),
        curve(
            "a4.curve",
            "Graph: start (bottom) → end (top)",
            Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
        )
        .adv(4),
        // 5 — Velocity / Expression Morph.
        adv_switch(5, FUNCTIONS[4]),
        choice("a5.mode", "Velocity over the series", &MORPH_MODES, 0).adv(5),
        int("a5.start", "Velocity of copy 1", 1, 127, 100).adv(5),
        int("a5.end", "Velocity of the last copy", 1, 127, 40).adv(5),
        curve(
            "a5.curve",
            "Graph: start (bottom) → end (top)",
            Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
        )
        .adv(5),
        flag("a5.keep", "Keep each note's variation", false)
            .adv(5)
            .help("On: every note moves by the same amount (louder and softer notes stay apart). Off: every note of a copy gets its velocity."),
        // 6 — Alternating Mirror / Reverse.
        adv_switch(6, FUNCTIONS[5]),
        choice("a6.mode", "Change", &MIRROR_MODES, 0).adv(6),
        int("a6.every", "On every n-th copy (from copy 1)", 2, 16, 2).adv(6),
        // 7 — Seeded Fracture / Omission.
        adv_switch(7, FUNCTIONS[6]),
        choice("a7.mode", "Picked copies are", &FRACTURE_MODES, 0).adv(7),
        curve(
            "a7.curve",
            "Chance over the series",
            Curve::new(vec![(0.0, 0.25), (1.0, 0.25)]),
        )
        .adv(7)
        .help("x: the copy's place in the series; y: the chance it is picked (whole copies only)."),
        seed("a7.seed").adv(7),
        // 8 — Voice Phase Stagger.
        adv_switch(8, FUNCTIONS[7]),
        choice("a8.by", "Voices are", &VOICE_MODES, 0).adv(8),
        list(
            "a8.phases",
            "Phase per voice (% of the copy's spacing)",
            0,
            100,
            64,
            &[0, 25, 50, 75],
        )
        .adv(8)
        .help("Chord voices count from the lowest note of a chord (notes that start together); the list cycles over the voices."),
        // 9 — Geometric Drift per Repeat.
        adv_switch(9, FUNCTIONS[8]),
        float("a9.scale_x", "Time scale per copy", 50.0, 200.0, 100.0, "%").adv(9),
        float("a9.scale_y", "Pitch scale per copy", 50.0, 200.0, 100.0, "%").adv(9),
        float("a9.skew_x", "Time skew per copy", -100.0, 100.0, 0.0, "%").adv(9),
        float("a9.skew_y", "Pitch skew per copy", -100.0, 100.0, 0.0, "%").adv(9),
        float("a9.rotate", "Rotation per copy", -180.0, 180.0, 10.0, "°")
            .adv(9)
            .help("Around the selection's center, in its box (as the geometry tools rotate); copy k turns k times."),
        // 10 — Recursive Repeat Tree.
        adv_switch(10, FUNCTIONS[9]),
        int("a10.depth", "Depth", 1, 4, 2).adv(10),
        int("a10.children", "Copies per block", 1, 16, 2).adv(10),
        float("a10.ratio", "Child spacing", 5.0, 100.0, 50.0, "% of the parent's").adv(10),
    ];
    params.extend(boundary::specs(boundary::PitchPolicy::Discard));
    with_help(&mut params, PASTE_REPEAT_NOTES_HELP);
    // B1-0099 (§013-E): "Paste Repeat (Notes)" is Duplicate V2 in the
    // Iterations section. The id `paste_repeat_notes` stays — presets,
    // keybinds, projects and the Edit → Paste Repeat… command find it by id —
    // and its catalog category stays Paste Repeat (`definition.rs`), so its
    // parameters and functions are exactly what they were.
    ToolSpec {
        id: "paste_repeat_notes",
        name: "Duplicate V2",
        family: Family::Iterations,
        summary: "Pastes the selected notes again a number of times: end to end, or one copy every so many bars, beats, note values, ticks or (real) seconds — under tempo changes a second stays a second.",
        params,
        advanced: FUNCTIONS.to_vec(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// The repeat the parameters ask for.
pub fn repeat_spec(p: &Params<'_>) -> Result<RepeatSpec, String> {
    let spacing = if p.choice("spacing") == 1 {
        Spacing::Even
    } else {
        Spacing::EndToEnd
    };
    // End to end does not use Paste every (greyed in the reference dialog).
    let every = match spacing {
        Spacing::Even => Decimal::parse(&p.text("every"))?,
        Spacing::EndToEnd => Decimal::whole(1),
    };
    Ok(RepeatSpec {
        count: u32::try_from(p.int("count")).unwrap_or(0),
        spacing,
        every,
        unit: RepeatUnit::ALL
            .get(p.choice("unit"))
            .copied()
            .unwrap_or(RepeatUnit::Seconds),
    })
}

/// Function 9's map of copy *k* in the source box's unit space, around its
/// centre: `(u, v) ↦ c + M·((u, v) − c)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drift {
    m: [f64; 4],
}

impl Drift {
    /// The k-th power of the per-copy drift: scale `s^k`, skew `k·skew`,
    /// rotation `k·angle` (factors held to ±10⁶ so nothing turns infinite).
    fn of(k: u32, scale: (f64, f64), skew: (f64, f64), angle: f64) -> Drift {
        let k = f64::from(k);
        let pow = |s: f64| (s.ln() * k).exp().clamp(1e-6, 1e6);
        let (sx, sy) = (pow(scale.0), pow(scale.1));
        let (kx, ky) = ((skew.0 * k).clamp(-1e6, 1e6), (skew.1 * k).clamp(-1e6, 1e6));
        let (sin, cos) = (angle * k).sin_cos();
        // R · K · S with K = [[1, kx], [ky, 1]], S = diag(sx, sy).
        let ks = [sx, kx * sy, ky * sx, sy];
        Drift {
            m: [
                cos * ks[0] - sin * ks[2],
                cos * ks[1] - sin * ks[3],
                sin * ks[0] + cos * ks[2],
                sin * ks[1] + cos * ks[3],
            ],
        }
    }

    #[inline]
    fn apply(&self, u: f64, v: f64) -> (f64, f64) {
        let (x, y) = (u - 0.5, v - 0.5);
        (
            0.5 + self.m[0] * x + self.m[1] * y,
            0.5 + self.m[2] * x + self.m[3] * y,
        )
    }
}

/// What copy *k* does to every note of it (computed once per generation).
#[derive(Clone, Copy, Debug)]
struct Look {
    pitch: i64,
    sub: i64,
    /// Length factor (1 = unchanged).
    gate: f64,
    /// Velocity: the target of the copy, or the shift of every note.
    vel: Option<f64>,
    /// Function 6 on this copy: (mirror the pitches, reverse the time).
    mirror: (bool, bool),
    /// Function 9.
    drift: Option<Drift>,
}

/// Function 9's step per copy: scale (x, y), skew (x, y), angle (radians).
type DriftStep = ((f64, f64), (f64, f64), f64);

/// The kernel's per-copy settings.
struct Functions {
    sub: Option<(i64, SubWrap)>,
    pitch: Option<(i64, Vec<i64>)>,
    gate: Option<Morph>,
    vel: Option<(Morph, bool)>,
    /// Function 6: (every n-th, mirror pitch, reverse time).
    mirror: Option<(u32, bool, bool)>,
    /// Function 9: per-copy scale, skew and angle.
    drift: Option<DriftStep>,
}

impl Functions {
    fn read(p: &Params<'_>) -> Functions {
        let morph = |n: u8, scale: f64| -> Morph {
            let (start, end) = if n == 4 {
                (p.float("a4.start"), p.float("a4.end"))
            } else {
                (p.int("a5.start") as f64, p.int("a5.end") as f64)
            };
            Morph {
                start: start / scale,
                end: end / scale,
                curve: (p.choice(if n == 4 { "a4.mode" } else { "a5.mode" }) == 1)
                    .then(|| p.curve(if n == 4 { "a4.curve" } else { "a5.curve" })),
            }
        };
        Functions {
            sub: p.adv(1).then(|| {
                (
                    p.int("a1.offset"),
                    SubWrap::from_choice(p.choice("a1.wrap"), p.int("a1.lo") - 1, p.int("a1.n")),
                )
            }),
            pitch: p.adv(2).then(|| (p.int("a2.step"), p.list("a2.seq"))),
            gate: p.adv(4).then(|| morph(4, 100.0)),
            vel: p.adv(5).then(|| (morph(5, 1.0), p.flag("a5.keep"))),
            mirror: p.adv(6).then(|| {
                let m = p.choice("a6.mode");
                (p.int("a6.every").clamp(2, 16) as u32, m != 1, m != 0)
            }),
            drift: p.adv(9).then(|| {
                (
                    (p.float("a9.scale_x") / 100.0, p.float("a9.scale_y") / 100.0),
                    (p.float("a9.skew_x") / 100.0, p.float("a9.skew_y") / 100.0),
                    p.float("a9.rotate").to_radians(),
                )
            }),
        }
    }

    /// The largest length factor of any copy (for the preview window).
    fn max_gate(&self, n: usize) -> f64 {
        match &self.gate {
            None => 1.0,
            Some(m) => (1..=n as u32)
                .map(|k| m.at(place(k, n)))
                .fold(1.0, f64::max),
        }
    }

    /// Whether every note's copies are plain shifted copies (S13's loop).
    fn plain(&self) -> bool {
        self.sub.is_none()
            && self.pitch.is_none()
            && self.gate.is_none()
            && self.vel.is_none()
            && self.mirror.is_none()
            && self.drift.is_none()
    }

    fn look(&self, k: u32, n: usize, mean_vel: f64) -> Look {
        let u = place(k, n);
        let kk = i64::from(k);
        Look {
            pitch: self
                .pitch
                .as_ref()
                .map_or(0, |(step, seq)| cumulative(*step, seq, k)),
            sub: self.sub.map_or(0, |(o, _)| o.saturating_mul(kk)),
            gate: self.gate.as_ref().map_or(1.0, |m| m.at(u)),
            vel: self.vel.as_ref().map(|(m, keep)| {
                let t = m.at(u);
                if *keep { t - mean_vel } else { t }
            }),
            mirror: match self.mirror {
                Some((every, pitch, time)) if (k - 1).is_multiple_of(every) => (pitch, time),
                _ => (false, false),
            },
            drift: self.drift.map(|(s, sk, a)| Drift::of(k, s, sk, a)),
        }
    }
}

/// Everything a run needs per note: the copy slots, the looks per
/// generation and the source box.
struct Repeater {
    slots: Vec<CopySlot>,
    /// By generation − 1.
    looks: Vec<Look>,
    plain: bool,
    keep_vel: bool,
    wrap: Option<SubWrap>,
    tracks: Vec<SourceTrack>,
    /// Function 8: (by subchannels, phases in %).
    phases: Option<(bool, Vec<i64>)>,
    /// Source box: first start, last end, lowest and highest pitch.
    span: (u64, u64, i64, i64),
    geom: Geom,
}

impl Repeater {
    /// A note's voice for function 8 when voices are subchannels.
    fn sub_voice(&self, n: &TNote) -> usize {
        self.tracks
            .get(usize::from(n.track))
            .and_then(|t| t.sub_index(n.sub))
            .unwrap_or(0)
    }

    fn emit(&self, n: &TNote, voice: usize, out: &mut Output) {
        out.push(*n);
        if self.plain && self.phases.is_none() {
            for c in &self.slots {
                if !out.emit(
                    i128::from(n.start) + i128::from(c.off),
                    i128::from(n.dur),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                ) {
                    return;
                }
            }
            return;
        }
        let phase = self
            .phases
            .as_ref()
            .and_then(|(_, ph)| (!ph.is_empty()).then(|| ph[voice % ph.len()]));
        let (s0, e1, p0, p1) = self.span;
        for c in &self.slots {
            let l = &self.looks[(c.k - 1) as usize];
            let mut tpl = *n;
            if let Some(w) = self.wrap {
                // The logical subchannel only: the MIDI channel stays (§092).
                tpl.sub = move_sub(
                    w,
                    false,
                    self.tracks.get(usize::from(n.track)),
                    n,
                    l.sub,
                    None,
                )
                .0;
            }
            // Function 6 inside the source box (time order, pitch order).
            let start = if l.mirror.1 {
                (s0 + e1).saturating_sub(n.end())
            } else {
                n.start
            };
            let pitch = if l.mirror.0 {
                p0 + p1 - i64::from(n.pitch)
            } else {
                i64::from(n.pitch)
            };
            let dur = if l.gate == 1.0 {
                n.dur
            } else {
                ((n.dur as f64 * l.gate).round() as u64).max(1)
            };
            let vel = match l.vel {
                None => i64::from(n.vel),
                Some(v) if self.keep_vel => (f64::from(n.vel) + v).round() as i64,
                Some(v) => v.round() as i64,
            }
            .clamp(1, 127);
            let off = i128::from(c.off)
                + phase.map_or(0, |ph| (c.gap as f64 * ph as f64 / 100.0).round() as i128);
            let ok = match &l.drift {
                None => out.emit(
                    i128::from(start) + off,
                    i128::from(dur),
                    pitch + l.pitch,
                    vel,
                    &tpl,
                ),
                Some(d) => {
                    // The changed note in the box, mapped by the drift and
                    // rasterized by the canonical engine, then moved to its
                    // copy (time) and pitch shift.
                    tpl.start = start;
                    tpl.dur = dur;
                    tpl.pitch = pitch.clamp(0, 255) as u8;
                    tpl.vel = vel as u8;
                    let g = Geom {
                        pos: (off as f64, l.pitch as f64),
                        ..self.geom
                    };
                    g.rasterize(&tpl, &|u, v| d.apply(u, v), &self.tracks, out)
                }
            };
            if !ok {
                return;
            }
        }
    }
}

/// Every chord voice of the source: a note's rank by pitch among the notes
/// of its track that start on the same tick (0 = the lowest).
fn chord_voices(notes: &[TNote]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_unstable_by_key(|&i| (notes[i].track, notes[i].start, notes[i].pitch, i));
    let mut voice = vec![0; notes.len()];
    let mut rank = 0;
    for w in 0..order.len() {
        let i = order[w];
        if w > 0 {
            let j = order[w - 1];
            rank = if (notes[j].track, notes[j].start) == (notes[i].track, notes[i].start) {
                rank + 1
            } else {
                0
            };
        }
        voice[i] = rank;
    }
    voice
}

fn build(p: &Params<'_>, b: &BuildCtx<'_>) -> Result<Box<dyn Kernel>, TransformError> {
    let spec = repeat_spec(p).map_err(TransformError::Invalid)?;
    let src = RepeatSource {
        start: b.stats.min_start,
        end: b.stats.max_end,
        snap: Some(b.ctx.snap),
    };
    let rp = plan(&spec, src, &RepeatClock::of(b.ctx)).map_err(TransformError::Invalid)?;
    let accel = p
        .adv(3)
        .then(|| (p.curve("a3.curve"), p.float("a3.max") / 100.0));
    let mut list = copies(&rp.offsets, accel.as_ref().map(|(c, m)| (c, *m)))
        .map_err(TransformError::Invalid)?;
    let generations = list.len();
    if p.adv(7) {
        let m = p.choice("a7.mode");
        fracture(
            &mut list,
            &p.curve("a7.curve"),
            p.seed("a7.seed"),
            m != 1,
            m != 0,
        );
    }
    if p.adv(10) {
        list = tree(
            &list,
            p.int("a10.depth").clamp(1, 4) as u32,
            p.int("a10.children").clamp(1, 16) as u32,
            p.float("a10.ratio") / 100.0,
            MAX_TREE_COPIES,
        )
        .map_err(TransformError::Invalid)?;
    }
    let f = Functions::read(p);
    let mean_vel = b.stats.mean_velocity();
    let looks: Vec<Look> = (1..=generations as u32)
        .map(|k| f.look(k, generations, mean_vel))
        .collect();
    let phases = p
        .adv(8)
        .then(|| (p.choice("a8.by") == 1, p.list("a8.phases")));
    let chords = phases.as_ref().is_some_and(|(subs, _)| !subs);
    let wrap = f.sub.map(|s| s.1);
    let span = if b.stats.count == 0 {
        (0, 0, 60, 60)
    } else {
        (
            b.stats.min_start,
            b.stats.max_end,
            i64::from(b.stats.min_pitch),
            i64::from(b.stats.max_pitch),
        )
    };
    let mut geom = Geom::identity(GeomBox::of(b.stats));
    geom.top = i64::from(p.keys.max_pitch());
    let growth = Growth::Expand(list.len() as u64 + 1);
    // How far a copy can land from its source note (the lazy preview's
    // window): the last slot plus its phase, the longest extra length, the
    // span for reversed time; anywhere once the drift moves notes.
    let reach = list
        .iter()
        .map(|c| c.off.saturating_add(c.gap))
        .max()
        .unwrap_or(0);
    let extra = (b.stats.max_dur as f64 * (f.max_gate(generations) - 1.0).max(0.0)).ceil() as u64;
    let swing = if f.mirror.is_some_and(|m| m.2) {
        span.1.saturating_sub(span.0)
    } else {
        0
    };
    let (ahead, behind) = if f.drift.is_some() {
        (u64::MAX, u64::MAX)
    } else {
        (swing, reach.saturating_add(extra).saturating_add(swing))
    };
    let rep = Repeater {
        plain: f.plain(),
        keep_vel: f.vel.as_ref().is_some_and(|v| v.1),
        tracks: if wrap.is_some() || phases.is_some() || f.drift.is_some() {
            b.tracks.to_vec()
        } else {
            Vec::new()
        },
        wrap,
        slots: list,
        looks,
        phases,
        span,
        geom,
    };
    if chords {
        // Chord voices need the notes that start together: a whole-source run.
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                let voices = chord_voices(&src.notes);
                for (n, v) in src.notes.iter().zip(voices) {
                    rep.emit(n, v, out);
                    out.check()?;
                }
                Ok(())
            },
            growth,
            estimate: None,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let v = if rep.phases.is_some() {
                rep.sub_voice(n)
            } else {
                0
            };
            rep.emit(n, v, out);
        },
        ahead,
        behind,
        growth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pitch_sequence_adds_up_its_intervals_over_whole_cycles() {
        let f = Functions {
            sub: None,
            pitch: Some((12, vec![2, 2, -1])),
            gate: None,
            vel: None,
            mirror: None,
            drift: None,
        };
        let pitches: Vec<i64> = (1..=7u32).map(|k| f.look(k, 7, 0.0).pitch).collect();
        // Brute force: running sums of 2, 2, −1, 2, 2, −1, 2.
        let mut want = Vec::new();
        let mut s = 0;
        for i in 0..7 {
            s += [2, 2, -1][i % 3];
            want.push(s);
        }
        assert_eq!(pitches, want);
        let f = Functions {
            pitch: Some((-5, Vec::new())),
            ..f
        };
        assert_eq!(f.look(4, 7, 0.0).pitch, -20);
    }

    #[test]
    fn the_drift_of_copy_k_is_the_kth_power_of_one_step() {
        let one = Drift::of(1, (1.1, 0.9), (0.05, -0.02), 0.2);
        let three = Drift::of(3, (1.1, 0.9), (0.05, -0.02), 0.2);
        // Scale only, or rotation only: powers compose exactly.
        let s1 = Drift::of(1, (1.25, 0.8), (0.0, 0.0), 0.0);
        let s3 = Drift::of(3, (1.25, 0.8), (0.0, 0.0), 0.0);
        let (u, v) = (0.9, 0.2);
        let mut p = (u, v);
        for _ in 0..3 {
            p = s1.apply(p.0, p.1);
        }
        let q = s3.apply(u, v);
        assert!(
            (p.0 - q.0).abs() < 1e-12 && (p.1 - q.1).abs() < 1e-12,
            "{p:?} {q:?}"
        );
        let r1 = Drift::of(1, (1.0, 1.0), (0.0, 0.0), 0.3);
        let r2 = Drift::of(2, (1.0, 1.0), (0.0, 0.0), 0.3);
        let a = r1.apply(r1.apply(u, v).0, r1.apply(u, v).1);
        let b = r2.apply(u, v);
        assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
        // The centre stays; a huge power stays finite.
        assert_eq!(one.apply(0.5, 0.5), (0.5, 0.5));
        assert_eq!(three.apply(0.5, 0.5), (0.5, 0.5));
        let huge = Drift::of(100_000, (2.0, 0.5), (1.0, 1.0), 1.0);
        let (x, y) = huge.apply(1.0, 0.0);
        assert!(x.is_finite() && y.is_finite());
    }

    #[test]
    fn chord_voices_rank_notes_that_start_together_by_pitch() {
        let n = |track: u16, start: u64, pitch: u8| TNote {
            start,
            dur: 10,
            sub: 0,
            group: 0,
            track,
            pitch,
            vel: 64,
            rel: 64,
            ch: 0,
            flags: 0,
        };
        let notes = [
            n(0, 0, 67),
            n(0, 0, 60),
            n(0, 0, 64),
            n(0, 10, 50),
            n(1, 0, 40),
            n(0, 10, 55),
        ];
        assert_eq!(chord_voices(&notes), vec![2, 0, 1, 0, 0, 1]);
    }
}
