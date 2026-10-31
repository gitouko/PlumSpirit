//! Chord Progression and Chord Stamp.
//!
//! The Chord Progression generator is an **original, rule-based** PlumSpirit
//! algorithm (weighted functional-harmony transitions between scale degrees,
//! seeded). It reproduces the documented workflow of FL Studio's tool (count,
//! key, octave, length, generate / typed / analyze, voicing, inversion, bass,
//! extensions, names, presets) but not Image-Line's proprietary model, which is
//! not public (FL_TOOL_BEHAVIOR.md §Chord Progression).

use super::super::boundary::{self, PitchPolicy};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, salt};
use super::super::shared::density_window::{PolyOrder, poly_cap_keep};
use super::super::shared::scale_lattice::Lattice;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::{
    self, CHORDS, ChordShape, KEY_NAMES, Scale, parse_chord, parse_roman, roman_name, scale_names,
};
use super::super::{Ctx, Growth, Kernel, Output, PointKernel, Source, TNote, TransformError};
use super::{Contextual, NOTE_VALUES, Pointwise, boxed, note_value, pos};

// ================================================================ Chord Progression

pub const CHORD_PROGRESSION: ToolDef = ToolDef {
    id: "chord_progression",
    spec: cp_spec,
    build: cp_build,
};

const CP_MODES: &[&str] = &[
    "Generate (seeded)",
    "Typed progression",
    "Analyze the selection",
];
const VOICINGS: &[&str] = &["Block", "Open", "Octave", "Stacked"];
const EXISTING: &[&str] = &[
    "Keep the selected notes",
    "Replace the selected notes",
    "Snap the selected notes to chord tones",
];
const VL_GOALS: &[&str] = &["Smooth", "Compact", "Contrary motion", "Maximal movement"];
const BASS_MOTION: &[&str] = &["Ascending", "Descending", "Pedal (tonic)", "Step", "Leap"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CHORD_PROGRESSION_HELP: &[(&str, &str)] = &[
    (
        "mode",
        "Generate invents a progression (seeded); Typed Progression plays the chords you write; Analyze the Selection finds the chord under each chord length of the selected notes.",
    ),
    (
        "text",
        "For Typed Progression: the chords, as Roman numerals in the key (I vi IV V) or names (C Am F G); ? generates that chord, = repeats the previous one; the list repeats.",
    ),
    ("key", "The key the progression is in."),
    (
        "scale",
        "The scale of the key; generated and Roman-numeral chords use its notes.",
    ),
    ("count", "How many chords the progression has."),
    ("length", "How long each chord lasts, as a note value."),
    (
        "length.ticks",
        "For Custom: how long each chord lasts, in ticks.",
    ),
    (
        "octave",
        "The octave the chords are voiced in (4 = around middle C).",
    ),
    (
        "adventure",
        "0 follows conventional chord moves; toward 1 the generator picks any degree more freely, adds more extensions and borrows chords more often.",
    ),
    (
        "ext",
        "Up to this many extra chord tones (sevenths, ninths …) the generator may stack on a chord.",
    ),
    (
        "voicing",
        "Block keeps the chord close; Open lifts every other voice an octave; Octave doubles the lowest voice an octave up; Stacked doubles the whole chord an octave up.",
    ),
    (
        "inversion",
        "Moves the lowest voices up an octave (+) or the highest down (−), this many times.",
    ),
    ("bass", "Adds the chord's root two octaves below the chord."),
    ("transpose", "Moves every chord this many semitones."),
    (
        "existing",
        "What happens to the selected notes: kept as they are, removed, or moved to the nearest tone of the chord sounding at their start.",
    ),
    (
        "swap",
        "Pairs of chord positions to swap (2 4 swaps the second and fourth chords); 1 = the first chord.",
    ),
    (
        "regen",
        "Chord positions (1 = the first) that the generator picks again, as the Alternative below.",
    ),
    (
        "alt",
        "Which alternative the regenerated positions take (another number gives another choice).",
    ),
    (
        "a1",
        "Pulls generated chords toward the tonic and the dominant.",
    ),
    (
        "a1.amount",
        "How strong the pull is (0 = none, 1 = the tonic five times and the dominant three times as likely).",
    ),
    (
        "a2",
        "Re-voices each chord (inversion and octave) for the smoothest or most striking move from the chord before.",
    ),
    (
        "a2.goal",
        "Smooth moves the voices least; Compact keeps the chord narrow; Contrary Motion moves top and bottom opposite ways; Maximal Movement moves the voices most.",
    ),
    (
        "a3",
        "Stacks a second triad (an upper structure) over every chord, as a polychord.",
    ),
    (
        "a3.interval",
        "How many semitones above the chord's root the upper triad's root lies (7 = a fifth above).",
    ),
    (
        "a3.octave",
        "Moves the upper triad this many octaves from the chord's octave.",
    ),
    (
        "a3.sub",
        "Puts the upper triad's notes this many subchannels away from the chord's, so it can be colored apart.",
    ),
    (
        "a4",
        "Moves the progression into another key halfway through.",
    ),
    ("a4.key", "The key of the progression's second half."),
    ("a4.scale", "The scale of the progression's second half."),
    (
        "a5",
        "Mirrors every chord's pitches around an axis key (negative harmony).",
    ),
    ("a5.axis", "The key the chords are mirrored around."),
    (
        "a6",
        "Chooses generated chords to follow a tension curve, from restful (0) to tense (1).",
    ),
    (
        "a6.curve",
        "The target tension from the first chord (left) to the last (right).",
    ),
    (
        "a7",
        "Controls how the bass moves from chord to chord (turns the bass note on).",
    ),
    (
        "a7.motion",
        "Ascending always moves up; Descending always down; Pedal holds the tonic; Step moves to the nearest root; Leap jumps at least a fourth.",
    ),
    (
        "a8",
        "Gives each voice of the chord its own subchannel, low to high, so voices can be colored apart.",
    ),
    (
        "a8.seq",
        "The subchannel of each voice from the lowest up, by place in the track's list (0 = the first); the list repeats.",
    ),
    (
        "a9",
        "Breaks each chord into several seeded hits instead of one long block.",
    ),
    (
        "a9.hits",
        "How many hits each chord is broken into (cut on a grid of 1/16 of the chord).",
    ),
    (
        "a10",
        "Fits the progression to the selected melody: chords follow its harmony and stay below its lowest note (the selected notes are kept).",
    ),
    (
        "n01",
        "Gives the chords their own lengths in beats, with a rate that can speed up or slow down across the progression.",
    ),
    (
        "n01.beats",
        "The length of each chord in beats, in turn; the list repeats.",
    ),
    (
        "n01.rate",
        "How fast the chords change across the progression: ½ = as listed, 1 = twice as fast, 0 = half as fast.",
    ),
    (
        "n01.snap",
        "Rounds every chord length to whole beats, bars or eighth notes.",
    ),
    (
        "n02",
        "Replaces some chords with the same degree borrowed from another mode.",
    ),
    (
        "n02.share",
        "The chance that a chord in a chosen place is borrowed (seeded).",
    ),
    (
        "n02.from",
        "The mode chords are borrowed from: the parallel minor or major, Dorian, Mixolydian, Phrygian or Lydian.",
    ),
    (
        "n02.where",
        "Which chords may be borrowed: any, only pre-dominants (ii and IV), or those at cadences.",
    ),
    (
        "n03",
        "Inserts a secondary dominant just before chosen chords, leading into them.",
    ),
    (
        "n03.before",
        "Which chords get a dominant before them: every chord, chords on a bar line, or the listed positions.",
    ),
    (
        "n03.positions",
        "For Listed Positions: the chords (1 = the first) that get a dominant before them.",
    ),
    (
        "n03.kind",
        "The dominant inserted: V7 of the chord, vii°7 of it, or the tritone substitute.",
    ),
    (
        "n03.share",
        "How much of the previous chord's length the dominant takes over.",
    ),
    ("n04", "Ends each phrase with a chosen cadence."),
    ("n04.phrase", "How many chords make one phrase."),
    (
        "n04.cadence",
        "The cadence at phrase ends: authentic (V – I), plagal (IV – I), half (IV – V) or deceptive (V – vi).",
    ),
    (
        "n04.strength",
        "The chance that each phrase end gets the cadence (seeded).",
    ),
    (
        "n05",
        "Spreads each chord's tones over a wide key range, like a wall of sound.",
    ),
    ("n05.lo", "The lowest key of the wall."),
    ("n05.hi", "The highest key of the wall."),
    (
        "n05.density",
        "Which tones fill the wall: every chord tone, only root and fifth, or fewer tones the higher the octave.",
    ),
    (
        "n05.vel",
        "The velocity multiplier from the wall's lowest key (left) to its highest (right).",
    ),
    ("n06", "Inserts a short passing chord before chord changes."),
    (
        "n06.len",
        "How long each passing chord lasts, in ticks (taken from the end of the chord before).",
    ),
    (
        "n06.kind",
        "The passing chord: a half step above the next chord, a half step below it, or a scale step away.",
    ),
    (
        "n06.where",
        "Where passing chords go: before every chord change, or only at phrase ends.",
    ),
    (
        "n07",
        "Suspends one voice of chosen chords and resolves it partway through.",
    ),
    ("n07.kind", "The suspension: 4 – 3, 9 – 8, 7 – 6 or 2 – 3."),
    (
        "n07.where",
        "Which chords are suspended: every chord, chords at cadences, or the listed positions.",
    ),
    (
        "n07.positions",
        "For Listed Positions: the chords (1 = the first) that are suspended.",
    ),
    (
        "n07.resolve",
        "When the suspended voice resolves, as a share of the chord's length.",
    ),
    (
        "n08",
        "Writes chords that fit the selected melody, one chord per bar, beat or melody note.",
    ),
    (
        "n08.per",
        "How often the chord may change: every bar, every beat, or every melody note.",
    ),
    (
        "n08.degrees",
        "The scale degrees the chords may be built on (1 = the tonic … 7).",
    ),
    (
        "n08.fit",
        "Chord Tones Only makes every melody note a chord tone; Passing Notes Allowed lets some melody notes pass between chord tones.",
    ),
    (
        "n08.adventure",
        "How freely the harmonizer picks less obvious chords (seeded).",
    ),
    (
        "n09",
        "Generates chords by root movement: each root moves by a chosen interval with the weights below.",
    ),
    (
        "n09.weights",
        "The relative chances of moving the root a fourth up, a fifth up, a third, a step, a tritone, or to a chromatic mediant.",
    ),
    (
        "n09.start",
        "The scale degree the walk starts on (1 = the tonic).",
    ),
    (
        "n09.qualities",
        "The chords on the roots: as the scale gives them, all major, all minor, or mixed at random.",
    ),
    (
        "n10",
        "Sets the number of voices in every chord, adding or removing voices as needed.",
    ),
    ("n10.voices", "How many voices each chord has."),
    (
        "n10.by",
        "How voices are added: extensions (sevenths, ninths …), octave doublings, or both.",
    ),
    (
        "n10.order",
        "Which voices are doubled first: from the bass, from the top, or alternating.",
    ),
    (
        "n11",
        "Builds the chords as a neo-Riemannian chain: each triad turns into the next by P, L or R.",
    ),
    (
        "n11.chain",
        "The transformations in order: 0 = P (major ↔ minor), 1 = L (leading-tone exchange), 2 = R (relative).",
    ),
    (
        "n11.start",
        "The chain starts from the key's tonic triad, or from the first typed chord.",
    ),
    (
        "n11.repeat",
        "How many times the list of transformations is applied in a row.",
    ),
];

fn cp_spec() -> ToolSpec {
    let mut p = vec![
        choice("mode", "Generate", CP_MODES, 0),
        text(
            "text",
            "Progression (e.g. I vi IV V, or C Am F G; ? = generate)",
            400,
            "I vi IV V",
        ),
        choice("key", "Key", &KEY_NAMES, 0),
        choice("scale", "Scale", scale_names(), 0),
        int("count", "Count", 1, 64, 4),
        choice("length", "Chord length", NOTE_VALUES, 0),
        ticks("length.ticks", "Custom length", 1, 1 << 40, 3840),
        int("octave", "Octave", 1, 7, 4).keyed_octaves(),
        float("adventure", "Conventional ↔ adventurous", 0.0, 1.0, 0.2, ""),
        int("ext", "Extra notes (extensions)", 0, 3, 0),
        choice("voicing", "Voicing", VOICINGS, 0),
        int("inversion", "Inversion", -3, 3, 0),
        flag("bass", "Bass note", true),
        int("transpose", "Transpose (semitones)", -24, 24, 0),
        choice("existing", "Selected notes", EXISTING, 0),
        list("swap", "Swap positions (pairs, 1-based)", 1, 64, 32, &[]),
        list("regen", "Regenerate positions (1-based)", 1, 64, 64, &[]),
        int("alt", "Alternative", 0, 16, 1),
        seed("seed"),
        adv_switch(1, "Harmonic gravity"),
        float(
            "a1.amount",
            "Pull toward tonic / dominant",
            0.0,
            1.0,
            0.7,
            "",
        )
        .adv(1),
        adv_switch(2, "Voice-leading optimizer"),
        choice("a2.goal", "Goal", VL_GOALS, 0).adv(2),
        adv_switch(3, "Polychord builder"),
        int(
            "a3.interval",
            "Upper structure root (semitones above)",
            1,
            23,
            7,
        )
        .adv(3),
        int("a3.octave", "Upper structure octave offset", -2, 3, 1).adv(3),
        int("a3.sub", "Upper structure subchannel step", -64, 64, 1).adv(3),
        adv_switch(4, "Modulation path"),
        choice("a4.key", "Target key", &KEY_NAMES, 7).adv(4),
        choice("a4.scale", "Target scale", scale_names(), 0).adv(4),
        adv_switch(5, "Symmetric-harmony mode"),
        pitch("a5.axis", "Axis", 62).adv(5),
        adv_switch(6, "Tension envelope"),
        curve(
            "a6.curve",
            "Target tension across the progression",
            Curve::line(0.1, 0.8),
        )
        .adv(6),
        adv_switch(7, "Bass motion constraint"),
        choice("a7.motion", "Bass moves", BASS_MOTION, 3).adv(7),
        adv_switch(8, "Subchannel voicing layers"),
        list(
            "a8.seq",
            "Subchannel position per voice (low → high)",
            0,
            4095,
            16,
            &[0, 1, 2, 3],
        )
        .adv(8),
        adv_switch(9, "Chord rhythm fracture"),
        int("a9.hits", "Hits per chord", 2, 16, 4).adv(9),
        adv_switch(10, "Melody-contour preservation"),
        // v0.9 (V09-S35): functions declared by id.
        flag("n01", "Harmonic rhythm", false),
        list("n01.beats", "Beats per chord", 1, 64, 64, &[4, 4, 2, 2]),
        curve(
            "n01.rate",
            "Rate of change (½ = as listed, 1 = twice as fast)",
            Curve::flat(0.5),
        ),
        choice("n01.snap", "Snap lengths to", CP_SNAPS, 0),
        flag("n02", "Modal interchange", false),
        float("n02.share", "Borrow share", 0.0, 100.0, 30.0, "%"),
        choice("n02.from", "Borrow from", CP_BORROW, 0),
        choice("n02.where", "Borrow where", CP_BORROW_WHERE, 0),
        flag("n03", "Secondary dominants", false),
        choice("n03.before", "Before", CP_BEFORE, 0),
        list(
            "n03.positions",
            "Before positions (1-based)",
            1,
            64,
            64,
            &[2, 4],
        ),
        choice("n03.kind", "Dominant", CP_SECONDARY, 0),
        float(
            "n03.share",
            "Share of the previous chord",
            10.0,
            90.0,
            50.0,
            "%",
        ),
        flag("n04", "Cadence control", false),
        int("n04.phrase", "Phrase length (chords)", 2, 16, 4),
        choice("n04.cadence", "Cadence", CP_CADENCES, 0),
        float("n04.strength", "Cadence strength", 0.0, 100.0, 100.0, "%"),
        flag("n05", "Wall voicing", false),
        pitch("n05.lo", "Wall low", 36),
        pitch("n05.hi", "Wall high", 84),
        choice("n05.density", "Wall density", CP_WALL, 0),
        curve("n05.vel", "Velocity by register", Curve::flat(1.0)).overshoot(),
        flag("n06", "Passing chords", false),
        ticks("n06.len", "Passing length", 1, 1 << 40, 240),
        choice("n06.kind", "Passing chord", CP_PASSING, 0),
        choice("n06.where", "Passing where", CP_PASS_WHERE, 0),
        flag("n07", "Suspensions", false),
        choice("n07.kind", "Suspension", CP_SUS, 0),
        choice("n07.where", "Suspend", CP_SUS_WHERE, 0),
        list(
            "n07.positions",
            "Suspend positions (1-based)",
            1,
            64,
            64,
            &[1, 3],
        ),
        float("n07.resolve", "Resolve at", 10.0, 90.0, 50.0, "%"),
        flag("n08", "Harmonize the melody", false),
        choice("n08.per", "Chord per", CP_HARM_PER, 0),
        list(
            "n08.degrees",
            "Allowed degrees",
            1,
            7,
            7,
            &[1, 2, 3, 4, 5, 6],
        ),
        choice("n08.fit", "Fit", CP_FIT, 0),
        float(
            "n08.adventure",
            "Harmonizing adventure",
            0.0,
            100.0,
            20.0,
            "%",
        ),
        flag("n09", "Root-motion walk", false),
        list(
            "n09.weights",
            "Root motion weights (4th up, 5th up, 3rd, step, tritone, chromatic mediant)",
            0,
            100,
            6,
            &[40, 20, 15, 15, 5, 5],
        ),
        int("n09.start", "Start degree", 1, 7, 1),
        choice("n09.qualities", "Qualities", CP_QUALITIES, 0),
        flag("n10", "Chord size", false),
        int("n10.voices", "Voices", 1, 32, 6),
        choice("n10.by", "Add voices by", CP_ADD_BY, 2),
        choice("n10.order", "Doubling order", CP_DOUBLE_ORDER, 0),
        flag("n11", "Neo-Riemannian chain", false),
        list(
            "n11.chain",
            "Transformations (0 = P, 1 = L, 2 = R)",
            0,
            2,
            64,
            &[1, 2],
        ),
        choice("n11.start", "Chain starts from", CP_NR_START, 0),
        int("n11.repeat", "Repeat", 1, 64, 4),
    ];
    p.extend(boundary::specs(PitchPolicy::WrapOctave));
    with_help(&mut p, CHORD_PROGRESSION_HELP);
    ToolSpec {
        id: "chord_progression",
        name: "Chord Progression",
        family: Family::Fl,
        summary: "Generates, types or analyzes a chord progression in a key (original seeded rule-based generator), with voicings, inversions, bass and extensions.",
        params: p,
        advanced: vec![
            "Harmonic gravity",
            "Voice-leading optimizer",
            "Polychord builder",
            "Modulation path",
            "Symmetric-harmony mode",
            "Tension envelope",
            "Bass motion constraint",
            "Subchannel voicing layers",
            "Chord rhythm fracture",
            "Melody-contour preservation",
        ],
        functions: [
            ("n01", "Harmonic rhythm", 110),
            ("n02", "Modal interchange", 120),
            ("n03", "Secondary dominants", 130),
            ("n04", "Cadence control", 140),
            ("n05", "Wall voicing", 150),
            ("n06", "Passing chords", 160),
            ("n07", "Suspensions", 170),
            ("n08", "Harmonize the melody", 180),
            ("n09", "Root-motion walk", 190),
            ("n10", "Chord size", 200),
            ("n11", "Neo-Riemannian chain", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: false,
        context: false,
    }
}

/// Transition weights between scale degrees (rows: from I … vii°): an original
/// functional-harmony table (T → S → D → T tendencies).
const TRANSITIONS: [[f64; 7]; 7] = [
    [0.05, 0.15, 0.05, 0.30, 0.30, 0.15, 0.00],
    [0.05, 0.00, 0.05, 0.10, 0.60, 0.10, 0.10],
    [0.05, 0.10, 0.00, 0.30, 0.10, 0.45, 0.00],
    [0.30, 0.15, 0.05, 0.00, 0.35, 0.10, 0.05],
    [0.55, 0.05, 0.05, 0.10, 0.00, 0.20, 0.05],
    [0.10, 0.25, 0.10, 0.30, 0.20, 0.00, 0.05],
    [0.70, 0.00, 0.20, 0.00, 0.05, 0.05, 0.00],
];

/// Tension of a chord (0 consonant … 1 dissonant), used by the envelope.
fn tension(c: &ChordShape, scale: &Scale) -> f64 {
    let (deg, off) = scale.degree_of(i64::from(c.root));
    let base = match deg.rem_euclid(7) {
        0 => 0.0,
        5 => 0.2,
        3 => 0.25,
        1 => 0.35,
        2 => 0.4,
        4 => 0.6,
        _ => 0.9,
    };
    let borrowed = if off != 0 { 0.2 } else { 0.0 };
    (base + borrowed + 0.1 * c.intervals.len().saturating_sub(3) as f64).min(1.0)
}

/// The diatonic chord on degree `d` with `size` tones (stacked thirds).
fn diatonic(scale: &Scale, d: i64, size: usize) -> ChordShape {
    let root = scale.pitch_of(d);
    let intervals: Vec<u8> = (0..size as i64)
        .map(|k| (scale.pitch_of(d + 2 * k) - root) as u8)
        .collect();
    ChordShape {
        root: root.rem_euclid(12) as u8,
        intervals,
        bass: None,
    }
}

struct Plan {
    chords: Vec<ChordShape>,
    names: String,
    /// Which chords the generator chose (not typed or analysed).
    generated: Vec<bool>,
}

#[allow(clippy::too_many_arguments)]
fn plan(
    p: &Params,
    b: &BuildCtx,
    src: Option<&Source>,
    start: u64,
    len: u64,
) -> Result<Plan, TransformError> {
    let scale = Scale::new(p.choice("key"), p.choice("scale"));
    let mode = p.choice("mode");
    let adventure = p.float("adventure");
    let ext = p.int("ext") as usize;
    let seed = p.seed("seed");
    let gravity = p.adv(1).then(|| p.float("a1.amount"));
    let envelope = p.adv(6).then(|| p.curve("a6.curve"));
    let modulate = p
        .adv(4)
        .then(|| Scale::new(p.choice("a4.key"), p.choice("a4.scale")));
    let mut count = p.int("count") as usize;
    if (mode == 2 || p.adv(10))
        && let Some(src) = src.filter(|s| !s.notes.is_empty())
    {
        count = (src.stats.span().div_ceil(len.max(1)) as usize).clamp(1, 256);
    }
    let tokens: Vec<String> = p
        .text("text")
        .split([' ', ',', '-', '|'])
        .filter(|t| !t.trim().is_empty())
        .map(|t| t.trim().to_string())
        .collect();
    let regen = p.list("regen");
    let alt = p.int("alt") as u64;
    let mut chords: Vec<ChordShape> = Vec::with_capacity(count);
    let mut generated: Vec<bool> = Vec::with_capacity(count);
    let mut prev_deg: usize = 0;
    for i in 0..count {
        let sc = match &modulate {
            Some(b2) if i >= count / 2 => b2,
            _ => &scale,
        };
        let variant = if regen.contains(&((i + 1) as i64)) {
            alt
        } else {
            0
        };
        let mut r = Rng::new(hash(&[seed, salt::SHAPE, i as u64, variant]));
        // Typed / analysed chords.
        let typed = if mode == 1 {
            tokens
                .get(i % tokens.len().max(1))
                .and_then(|t| match t.as_str() {
                    "?" => None,
                    "=" | "." => chords.last().cloned(),
                    t => parse_roman(t, sc).or_else(|| parse_chord(t)),
                })
        } else {
            None
        };
        let analysed = if mode == 2 || p.adv(10) {
            src.and_then(|s| {
                analyse_segment(
                    s,
                    start.saturating_add((i as u64).saturating_mul(len)),
                    len,
                    sc,
                )
            })
        } else {
            None
        };
        generated.push(typed.is_none() && analysed.is_none());
        let chord = if let Some(c) = typed.or(analysed) {
            c
        } else {
            // Weighted next degree.
            let mut w: Vec<f64> = (0..7)
                .map(|j| {
                    let base = if i == 0 {
                        if j == 0 { 1.0 } else { 0.0 }
                    } else {
                        TRANSITIONS[prev_deg][j]
                    };
                    base * (1.0 - adventure) + adventure / 7.0
                })
                .collect();
            if let Some(g) = gravity {
                w[0] *= 1.0 + 4.0 * g;
                w[4] *= 1.0 + 2.0 * g;
            }
            if let Some(c) = &envelope {
                let target = c.eval(if count > 1 {
                    i as f64 / (count - 1) as f64
                } else {
                    0.0
                });
                for (j, x) in w.iter_mut().enumerate() {
                    let t = tension(&diatonic(sc, j as i64, 3), sc);
                    *x *= (1.0 - (t - target).abs()).max(0.02).powi(3);
                }
            }
            let total: f64 = w.iter().sum::<f64>().max(1e-9);
            let mut x = r.unit() * total;
            let mut d = 0usize;
            for (j, wj) in w.iter().enumerate() {
                if x < *wj {
                    d = j;
                    break;
                }
                x -= wj;
            }
            prev_deg = d;
            let mut size = 3;
            for _ in 0..ext {
                if r.chance(0.3 + 0.5 * adventure) {
                    size += 1;
                }
            }
            let mut c = diatonic(sc, d as i64, size);
            // Adventurous: occasionally borrow from the parallel mode.
            if r.chance(adventure * 0.35) {
                c.root = (c.root + 11) % 12;
            }
            c
        };
        chords.push(chord);
    }
    // Swaps (pairs of 1-based positions).
    for pair in p.list("swap").chunks_exact(2) {
        let (a, b2) = (pair[0] as usize - 1, pair[1] as usize - 1);
        if a < chords.len() && b2 < chords.len() {
            chords.swap(a, b2);
            generated.swap(a, b2);
        }
    }
    let _ = b;
    let names = chord_names(chords.iter(), &scale);
    Ok(Plan {
        chords,
        names,
        generated,
    })
}

/// "Chords: C – Am – F – G (I – vi – IV – V)."
fn chord_names<'a>(chords: impl Iterator<Item = &'a ChordShape> + Clone, scale: &Scale) -> String {
    let names = chords
        .clone()
        .map(|c| c.name())
        .collect::<Vec<_>>()
        .join(" – ");
    let romans = chords
        .map(|c| roman_name(c, scale))
        .collect::<Vec<_>>()
        .join(" – ");
    format!("Chords: {names} ({romans}).")
}

/// The triad (major / minor / diminished on any root) whose tones carry the
/// most note-time in `[t0, t0 + len)`, preferring chords of the scale.
fn analyse_segment(src: &Source, t0: u64, len: u64, scale: &Scale) -> Option<ChordShape> {
    let t1 = t0.saturating_add(len);
    let mut w = [0f64; 12];
    for n in &src.notes {
        let a = n.start.max(t0);
        let b = n.end().min(t1);
        if b > a {
            w[usize::from(n.pitch % 12)] += (b - a) as f64;
        }
    }
    if w.iter().all(|&x| x == 0.0) {
        return None;
    }
    let mut best: Option<(f64, ChordShape)> = None;
    for root in 0..12u8 {
        for shape in [&[0u8, 4, 7][..], &[0, 3, 7], &[0, 3, 6]] {
            let c = ChordShape {
                root,
                intervals: shape.to_vec(),
                bass: None,
            };
            let mask = c.pitch_classes();
            let inside: f64 = (0..12).filter(|i| mask & (1 << i) != 0).map(|i| w[i]).sum();
            let outside: f64 = (0..12).filter(|i| mask & (1 << i) == 0).map(|i| w[i]).sum();
            let diatonic = shape.iter().all(|s| scale.contains(i64::from(root + s)));
            let score = inside - 0.5 * outside
                + if diatonic { 0.01 * inside } else { 0.0 }
                + if w[usize::from(root)] > 0.0 {
                    0.05 * inside
                } else {
                    0.0
                };
            if best.as_ref().is_none_or(|b| score > b.0) {
                best = Some((score, c));
            }
        }
    }
    best.map(|b| b.1)
}

/// Pitches of a voiced chord (ascending) at `base` (root octave pitch).
fn voice(c: &ChordShape, base: i64, voicing: usize, inversion: i64) -> Vec<i64> {
    let root = base + i64::from(c.root) - base.rem_euclid(12);
    let mut v: Vec<i64> = c.intervals.iter().map(|&i| root + i64::from(i)).collect();
    v.sort_unstable();
    for _ in 0..inversion.max(0) {
        let x = v.remove(0);
        v.push(x + 12);
    }
    for _ in 0..(-inversion).max(0) {
        if let Some(x) = v.pop() {
            v.insert(0, x - 12);
        }
    }
    match voicing {
        1 if v.len() >= 3 => {
            // Open: every other voice up an octave.
            for (i, x) in v.iter_mut().enumerate() {
                if i % 2 == 1 {
                    *x += 12;
                }
            }
        }
        2 => {
            if let Some(&lo) = v.first() {
                v.push(lo + 12);
            }
        }
        3 => {
            let up: Vec<i64> = v.iter().map(|x| x + 12).collect();
            v.extend(up);
        }
        _ => {}
    }
    v.sort_unstable();
    v.dedup();
    v
}

const CP_SNAPS: &[&str] = &["Beat", "Bar", "1/8"];
const CP_BORROW: &[&str] = &[
    "Parallel minor / major",
    "Dorian",
    "Mixolydian",
    "Phrygian",
    "Lydian",
];
const CP_BORROW_WHERE: &[&str] = &["Anywhere", "Pre-dominants", "Cadences"];
const CP_BEFORE: &[&str] = &["Every chord", "Strong beats", "Listed positions"];
const CP_SECONDARY: &[&str] = &["V7", "vii°7", "Tritone substitute"];
const CP_CADENCES: &[&str] = &[
    "Authentic (V – I)",
    "Plagal (IV – I)",
    "Half (IV – V)",
    "Deceptive (V – vi)",
];
const CP_WALL: &[&str] = &["All chord tones", "Root and fifth", "Thinner per octave"];
const CP_PASSING: &[&str] = &[
    "Chromatic from above",
    "Chromatic from below",
    "Diatonic step",
];
const CP_PASS_WHERE: &[&str] = &["Every change", "Phrase ends"];
const CP_SUS: &[&str] = &["4 – 3", "9 – 8", "7 – 6", "2 – 3"];
const CP_SUS_WHERE: &[&str] = &["Every chord", "Cadences", "Listed positions"];
const CP_HARM_PER: &[&str] = &["Bar", "Beat", "Melody note"];
const CP_FIT: &[&str] = &["Chord tones only", "Passing notes allowed"];
const CP_QUALITIES: &[&str] = &["Diatonic", "All major", "All minor", "Mixed"];
const CP_ADD_BY: &[&str] = &["Extensions", "Octave doublings", "Both"];
const CP_DOUBLE_ORDER: &[&str] = &["Bass first", "Top first", "Alternate"];
const CP_NR_START: &[&str] = &["The key's tonic", "The first typed chord"];

/// The Chord Progression's v0.9 functions declared by id (TOOL_CATALOG
/// `chord_progression` n01 … n11). `None` = switched off; with every one off
/// the run is the v0.8 one.
#[derive(Clone, Default)]
struct CpNew {
    /// n01: beats per chord (cycled), rate curve (½ = as listed), snap (0
    /// beat, 1 bar, 2 eighth).
    rhythm: Option<(Vec<i64>, Curve, usize)>,
    /// n02: share 0–1, from (0 parallel, 1 Dorian, 2 Mixolydian, 3
    /// Phrygian, 4 Lydian), where (0 anywhere, 1 pre-dominants, 2 cadences).
    borrow: Option<(f64, usize, usize)>,
    /// n03: before (0 every chord, 1 strong beats, 2 listed), positions
    /// (1-based), kind (0 V7, 1 vii°7, 2 tritone substitute), share 0–1.
    secondary: Option<(usize, Vec<i64>, usize, f64)>,
    /// n04: phrase length, cadence (0 authentic, 1 plagal, 2 half, 3
    /// deceptive), strength 0–1.
    cadence: Option<(usize, usize, f64)>,
    /// n05: low, high, density (0 all, 1 root and fifth, 2 thinner per
    /// octave), velocity by register.
    wall: Option<(i64, i64, usize, Curve)>,
    /// n06: length, kind (0 from above, 1 from below, 2 diatonic step),
    /// where (0 every change, 1 phrase ends).
    passing: Option<(u64, usize, usize)>,
    /// n07: kind (0 4–3, 1 9–8, 2 7–6, 3 2–3), where (0 every chord, 1
    /// cadences, 2 listed), positions, resolve at 0–1.
    suspension: Option<(usize, usize, Vec<i64>, f64)>,
    /// n08: per (0 bar, 1 beat, 2 melody note), degrees (1-based), passing
    /// notes allowed, adventure 0–1.
    harmonize: Option<(usize, Vec<i64>, bool, f64)>,
    /// n09: weights (fourth up, fifth up, third, step, tritone, chromatic
    /// mediant), start degree (0-based), qualities (0 diatonic, 1 major, 2
    /// minor, 3 mixed).
    walk: Option<(Vec<f64>, i64, usize)>,
    /// n10: voices, add by (0 extensions, 1 doublings, 2 both), order (0
    /// bass first, 1 top first, 2 alternate).
    size: Option<(usize, usize, usize)>,
    /// n11: transformations (0 P, 1 L, 2 R), from the first typed chord
    /// (else the key's tonic), repeat.
    chain: Option<(Vec<i64>, bool, usize)>,
}

impl CpNew {
    fn any(&self) -> bool {
        self.rhythm.is_some()
            || self.borrow.is_some()
            || self.secondary.is_some()
            || self.cadence.is_some()
            || self.wall.is_some()
            || self.passing.is_some()
            || self.suspension.is_some()
            || self.harmonize.is_some()
            || self.walk.is_some()
            || self.size.is_some()
            || self.chain.is_some()
    }

    /// Chords per phrase: n04's phrase length, else 4.
    fn phrase(&self) -> usize {
        self.cadence.map_or(4, |c| c.0.max(2))
    }
}

fn cp_new(p: &Params) -> CpNew {
    let pct = |k: &str| (p.float(k) / 100.0).clamp(0.0, 1.0);
    CpNew {
        rhythm: p
            .flag("n01")
            .then(|| {
                (
                    p.list("n01.beats"),
                    p.curve("n01.rate"),
                    p.choice("n01.snap"),
                )
            })
            .filter(|r| !r.0.is_empty()),
        borrow: p.flag("n02").then(|| {
            (
                pct("n02.share"),
                p.choice("n02.from"),
                p.choice("n02.where"),
            )
        }),
        secondary: p.flag("n03").then(|| {
            (
                p.choice("n03.before"),
                p.list("n03.positions"),
                p.choice("n03.kind"),
                pct("n03.share"),
            )
        }),
        cadence: p.flag("n04").then(|| {
            (
                p.int("n04.phrase").max(2) as usize,
                p.choice("n04.cadence"),
                pct("n04.strength"),
            )
        }),
        wall: p.flag("n05").then(|| {
            let (a, b) = (p.int("n05.lo"), p.int("n05.hi"));
            (
                a.min(b),
                a.max(b),
                p.choice("n05.density"),
                p.curve("n05.vel"),
            )
        }),
        passing: p.flag("n06").then(|| {
            (
                p.ticks("n06.len").max(1),
                p.choice("n06.kind"),
                p.choice("n06.where"),
            )
        }),
        suspension: p.flag("n07").then(|| {
            (
                p.choice("n07.kind"),
                p.choice("n07.where"),
                p.list("n07.positions"),
                pct("n07.resolve"),
            )
        }),
        harmonize: p.flag("n08").then(|| {
            (
                p.choice("n08.per"),
                p.list("n08.degrees"),
                p.choice("n08.fit") == 1,
                pct("n08.adventure"),
            )
        }),
        walk: p.flag("n09").then(|| {
            (
                p.list("n09.weights")
                    .iter()
                    .map(|&w| w.max(0) as f64)
                    .collect(),
                (p.int("n09.start") - 1).clamp(0, 6),
                p.choice("n09.qualities"),
            )
        }),
        size: p.flag("n10").then(|| {
            (
                p.int("n10.voices").clamp(1, 32) as usize,
                p.choice("n10.by"),
                p.choice("n10.order"),
            )
        }),
        chain: p
            .flag("n11")
            .then(|| {
                (
                    p.list("n11.chain"),
                    p.choice("n11.start") == 1,
                    p.int("n11.repeat").max(1) as usize,
                )
            })
            .filter(|c| !c.0.is_empty()),
    }
}

/// A major or minor triad.
fn triad(root: i64, minor: bool) -> ChordShape {
    ChordShape {
        root: root.rem_euclid(12) as u8,
        intervals: if minor { vec![0, 3, 7] } else { vec![0, 4, 7] },
        bass: None,
    }
}

/// Minor when the chord has a minor third and no major one.
fn is_minor(c: &ChordShape) -> bool {
    c.intervals.contains(&3) && !c.intervals.contains(&4)
}

/// n11: a neo-Riemannian transformation of the chord's triad — P (the
/// parallel), L (leading-tone exchange) or R (the relative).
fn neo_riemannian(c: &ChordShape, t: i64) -> ChordShape {
    let r = i64::from(c.root);
    match (is_minor(c), t) {
        (false, 0) => triad(r, true),
        (false, 1) => triad(r + 4, true),
        (false, _) => triad(r + 9, true),
        (true, 0) => triad(r, false),
        (true, 1) => triad(r + 8, false),
        (true, _) => triad(r + 3, false),
    }
}

/// The key's scale degree of a chord root (0-based within the octave), or
/// `None` when the root lies outside the scale.
fn degree_in(scale: &Scale, root: u8) -> Option<i64> {
    let (d, off) = scale.degree_of(i64::from(root));
    (off == 0).then(|| d.rem_euclid(scale.steps.len() as i64))
}

/// One chord of the timeline: where it sounds, its shape and its place in
/// the progression (`None`: an inserted secondary dominant or passing
/// chord); `key` seeds its rhythm (the place for the progression's chords,
/// as in v0.8).
#[derive(Clone)]
struct Slot {
    t0: u64,
    len: u64,
    chord: ChordShape,
    pos: Option<usize>,
    key: u64,
    /// The chord whose settings the slot takes under Control Sweeps
    /// (E0422): its own, an inserted chord the one it leads to.
    unit: usize,
}

/// n08's segments of the selection: its bars or beats (the meter's), or
/// onset to onset of the melody (the last to the latest end).
fn cp_segments(src: &Source, ctx: &Ctx, per: usize) -> Vec<(u64, u64)> {
    use super::super::shared::metric_position::metric_position;
    let (a, z) = (
        src.stats.min_start,
        src.stats.max_end.max(src.stats.min_start.saturating_add(1)),
    );
    let mut segs: Vec<(u64, u64)> = Vec::new();
    match per {
        0 | 1 => {
            let mut t = a;
            while t < z {
                let m = metric_position(&ctx.meter, t);
                let (s, l) = if per == 0 {
                    (m.bar_start, m.bar_len)
                } else {
                    (m.beat_start, m.beat_len)
                };
                let e = s.saturating_add(l.max(1)).max(t.saturating_add(1));
                if e <= t {
                    break;
                }
                segs.push((s, e - s));
                t = e;
            }
        }
        _ => {
            let mut starts: Vec<u64> = src.notes.iter().map(|n| n.start).collect();
            starts.dedup();
            for (k, &s) in starts.iter().enumerate() {
                let e = starts
                    .get(k + 1)
                    .copied()
                    .unwrap_or(z)
                    .max(s.saturating_add(1));
                segs.push((s, e - s));
            }
        }
    }
    segs
}

/// At most how many segments [`cp_segments`] gives (without making them).
fn cp_segment_bound(src: &Source, ctx: &Ctx, per: usize) -> u64 {
    match per {
        // Distinct onsets.
        2 => {
            let mut n = 0u64;
            let mut last = None;
            for x in &src.notes {
                if last != Some(x.start) {
                    n += 1;
                    last = Some(x.start);
                }
            }
            n
        }
        // A beat or bar is at least a 32nd note long.
        _ => {
            let bound = src.stats.span() / (ctx.ppq / 8).max(1) + 2;
            if bound <= 1 << 22 {
                cp_segments(src, ctx, per).len() as u64
            } else {
                bound
            }
        }
    }
}

/// n08: segments of the melody (bars, beats or melody notes) and the chord
/// that fits each best — note-time in the segment on chord tones scores, off
/// them costs (×2, or ×¼ for passing notes that do not sound at the
/// segment's start), a small functional-flow bonus between diatonic
/// degrees, chromatic triads only with adventure (penalised by its
/// complement, seeded jitter by it). A rest keeps the previous chord.
fn cp_harmonize(
    src: &Source,
    scale: &Scale,
    seed: u64,
    (_, degrees, passing, adventure): &(usize, Vec<i64>, bool, f64),
    segs: &[(u64, u64)],
    adventure_at: &dyn Fn(usize) -> f64,
) -> Vec<ChordShape> {
    use super::super::shared::seeded_stream::{salt_of, stream};
    // Note-time per pitch class in each segment and the classes sounding at
    // its start: one sweep over note starts and ends (ends first on a tie).
    let mut ev: Vec<(u64, bool, u8)> = Vec::with_capacity(src.notes.len() * 2);
    for n in &src.notes {
        if n.dur > 0 {
            ev.push((n.start, true, n.pitch % 12));
            ev.push((n.end(), false, n.pitch % 12));
        }
    }
    ev.sort_unstable_by_key(|e| (e.0, e.1, e.2));
    let mut cnt = [0i64; 12];
    let mut k = 0;
    let mut cands: Vec<(ChordShape, Option<usize>)> = degrees
        .iter()
        .map(|&d| {
            let d = (d - 1).rem_euclid(scale.steps.len() as i64);
            (diatonic(scale, d, 3), Some(d as usize))
        })
        .collect();
    // Chromatic triads are candidates where the adventure is above 0 (each
    // segment's own under Control Sweeps, E0422).
    let chromatic = (0..segs.len()).any(|si| adventure_at(si) > 0.0) || *adventure > 0.0;
    if chromatic {
        for root in 0..12 {
            for minor in [false, true] {
                let c = triad(root, minor);
                if !cands.iter().any(|x| x.0 == c) {
                    cands.push((c, None));
                }
            }
        }
    }
    let seven = scale.steps.len() == 7;
    let mut chords: Vec<ChordShape> = Vec::with_capacity(segs.len());
    let mut prev: Option<(ChordShape, Option<usize>)> = None;
    for (si, &(s, l)) in segs.iter().enumerate() {
        while k < ev.len() && ev[k].0 <= s {
            cnt[usize::from(ev[k].2)] += if ev[k].1 { 1 } else { -1 };
            k += 1;
        }
        let strong = cnt.map(|c| c > 0);
        let mut w = [0f64; 12];
        let mut last = s;
        let e = s.saturating_add(l);
        while k < ev.len() && ev[k].0 < e {
            let dt = (ev[k].0 - last) as f64;
            for pc in 0..12 {
                w[pc] += cnt[pc] as f64 * dt;
            }
            last = ev[k].0;
            cnt[usize::from(ev[k].2)] += if ev[k].1 { 1 } else { -1 };
            k += 1;
        }
        let dt = (e - last) as f64;
        for pc in 0..12 {
            w[pc] += cnt[pc] as f64 * dt;
        }
        if w.iter().all(|&x| x == 0.0) {
            let c = prev
                .clone()
                .unwrap_or_else(|| (diatonic(scale, 0, 3), Some(0)));
            chords.push(c.0.clone());
            prev = Some(c);
            continue;
        }
        let mut r = stream(seed, salt_of("n08"), &[si as u64]);
        let adventure = adventure_at(si);
        let mut best: Option<(f64, usize)> = None;
        for (ci, (c, deg)) in cands.iter().enumerate() {
            if deg.is_none() && adventure <= 0.0 {
                continue;
            }
            let mask = c.pitch_classes();
            let mut fit = 0.0;
            for pc in 0..12 {
                if w[pc] == 0.0 {
                    continue;
                }
                fit += w[pc] / l as f64
                    * if mask & (1 << pc) != 0 {
                        1.0
                    } else if *passing && !strong[pc] {
                        -0.25
                    } else {
                        -2.0
                    };
            }
            let flow = match (deg, prev.as_ref().and_then(|p| p.1)) {
                (Some(d), Some(pd)) if seven => 0.1 * TRANSITIONS[pd][*d],
                (Some(_), _) => 0.0,
                (None, _) => -(1.0 - adventure),
            };
            let score = fit + flow + adventure * 0.2 * r.unit();
            if best.is_none_or(|b| score > b.0) {
                best = Some((score, ci));
            }
        }
        let c = cands[best.expect("candidates").1].clone();
        chords.push(c.0.clone());
        prev = Some(c);
    }
    chords
}

/// n09: a chord reached from `prev` by a root motion drawn from the weights
/// (fourth up, fifth up, a third, a step, the tritone, a chromatic mediant
/// that keeps the quality); thirds and steps stay in the key when one does
/// (diatonic qualities). `None` when every weight is 0.
fn cp_walk_step(
    prev: &ChordShape,
    scale: &Scale,
    (weights, _, qualities): &(Vec<f64>, i64, usize),
    r: &mut Rng,
) -> Option<ChordShape> {
    let mut w = weights.clone();
    w.resize(6, 0.0);
    let motion = r.weighted(&w)?;
    let from = i64::from(prev.root);
    let options: &[i64] = match motion {
        0 => &[5],
        1 => &[7],
        2 | 5 => &[3, 4, -3, -4],
        3 => &[1, 2, -1, -2],
        _ => &[6],
    };
    let mut roots: Vec<i64> = options.iter().map(|o| from + o).collect();
    if *qualities == 0 && matches!(motion, 2 | 3) {
        let inside: Vec<i64> = roots
            .iter()
            .copied()
            .filter(|&x| scale.contains(x))
            .collect();
        if !inside.is_empty() {
            roots = inside;
        }
    }
    let root = roots[r.below(roots.len() as u64) as usize];
    Some(if motion == 5 {
        triad(root, is_minor(prev))
    } else {
        cp_quality(root, scale, *qualities, r)
    })
}

/// A chord on `root` of the walk's qualities: the key's triad on that
/// degree (major off the key), all major, all minor, or seeded.
fn cp_quality(root: i64, scale: &Scale, qualities: usize, r: &mut Rng) -> ChordShape {
    match qualities {
        0 => match degree_in(scale, root.rem_euclid(12) as u8) {
            Some(d) => diatonic(scale, d, 3),
            None => triad(root, false),
        },
        1 => triad(root, false),
        2 => triad(root, true),
        _ => triad(root, r.chance(0.5)),
    }
}

/// n10: the voicing grown or cut to `n` voices — cut from the top; grown by
/// stacked thirds of the key above the top voice (extensions; with "both"
/// until seven pitch classes), then octave doublings of the voicing: below
/// from the bass, above from the top, or alternately from the outside in.
fn cp_resize(mut v: Vec<i64>, scale: &Scale, (n, by, order): (usize, usize, usize)) -> Vec<i64> {
    v.sort_unstable();
    if v.len() >= n || v.is_empty() {
        v.truncate(n);
        return v;
    }
    let pcs = |v: &[i64]| {
        v.iter()
            .fold(0u16, |m, &p| m | 1 << p.rem_euclid(12))
            .count_ones()
    };
    if by != 1 {
        let mut top = *v.last().unwrap();
        let mut guard = 0;
        while v.len() < n && (by == 0 || pcs(&v) < 7) && guard < 64 {
            let (d, _) = scale.degree_of(top);
            let mut next = scale.pitch_of(d + 2);
            if next <= top {
                next += 12;
            }
            v.push(next);
            top = next;
            guard += 1;
        }
    }
    if by != 0 {
        let base = v.clone();
        let k = base.len();
        let mut oct = 1i64;
        'outer: while v.len() < n && oct <= 10 {
            for j in 0..k {
                let cand = match order {
                    0 => base[j] - 12 * oct,
                    1 => base[k - 1 - j] + 12 * oct,
                    _ => {
                        if j % 2 == 0 {
                            base[j / 2] - 12 * oct
                        } else {
                            base[k - 1 - j / 2] + 12 * oct
                        }
                    }
                };
                if !v.contains(&cand) {
                    v.push(cand);
                    if v.len() >= n {
                        break 'outer;
                    }
                }
            }
            oct += 1;
        }
    }
    v.sort_unstable();
    v
}

/// n05: the chord's pitch classes (of the voicing `v`) in every octave of
/// `[lo, hi]` — all of them, the root and fifth, or thinner toward the
/// bottom (the lowest octave of the range the root, the next root and
/// fifth) — with each pitch's velocity factor by register.
fn cp_wall(
    v: &[i64],
    c: &ChordShape,
    (lo, hi, density, curve): &(i64, i64, usize, Curve),
) -> Vec<(i64, f64)> {
    let all = v.iter().fold(0u16, |m, &p| m | 1 << p.rem_euclid(12));
    let root = i64::from(c.root);
    let fifth = root
        + c.intervals
            .iter()
            .map(|&i| i64::from(i))
            .find(|i| (6..=8).contains(i))
            .unwrap_or(7);
    let rf = 1u16 << root.rem_euclid(12) | 1 << fifth.rem_euclid(12);
    let span = (hi - lo).max(1) as f64;
    (*lo..=*hi)
        .filter(|&p| {
            let mask = match density {
                0 => all,
                1 => rf,
                _ => match (p - lo) / 12 {
                    0 => 1 << root.rem_euclid(12),
                    1 => rf,
                    _ => all,
                },
            };
            mask & (1 << p.rem_euclid(12)) != 0
        })
        .map(|p| (p, curve.eval((p - lo) as f64 / span)))
        .collect()
}

/// A voice of a chord: pitch, subchannel step (the polychord's), velocity
/// factor (the wall's), and whether it is a chord / bass voice (a
/// suspension's candidate).
type CpVoice = (i64, i64, f64, bool);

/// Everything a Chord Progression run needs (the v0.8 settings and n01 …
/// n11).
struct Cp {
    specs: Vec<ParamSpec>,
    set: ParamSet,
    ctx: Ctx,
    tracks: Vec<super::super::source::SourceTrack>,
    start: u64,
    len: u64,
    octave: i64,
    voicing: usize,
    inversion: i64,
    bass: bool,
    transpose: i64,
    existing: usize,
    vl: Option<usize>,
    poly: Option<(i64, i64, i64)>,
    mirror: Option<i64>,
    motion: Option<usize>,
    layers: Vec<i64>,
    fracture: Option<u64>,
    melody: bool,
    seed: u64,
    scale: Scale,
    new: CpNew,
    /// The preflight's bound (from the statistics), notes per chord slot,
    /// the progression's chord count bound.
    est: u64,
    per_slot: u64,
    count_bound: u64,
    /// n11's chords at the most repeats (0 off).
    chain_max: u64,
}

impl Cp {
    /// The progression as slots: the v0.8 chords one length each, or with
    /// v0.9 functions — chords from n08 / n11 / n09, then n04 cadences and
    /// n02 borrowing by place, timed by n08's segments or n01's rhythm, then
    /// n03 secondary dominants and n06 passing chords inserted at the end of
    /// the chord before.
    fn slots(&self, plan: Plan, src: &Source, units: &mut Vec<CpNew>) -> Vec<Slot> {
        use super::super::shared::metric_position::metric_position;
        use super::super::shared::seeded_stream::{salt_of, stream};
        let new = &self.new;
        let (start, len, scale) = (self.start, self.len, &self.scale);
        if !new.any() {
            return plan
                .chords
                .into_iter()
                .enumerate()
                .map(|(i, chord)| Slot {
                    t0: start.saturating_add((i as u64).saturating_mul(len)),
                    len,
                    chord,
                    pos: Some(i),
                    key: i as u64,
                    unit: i,
                })
                .collect();
        }
        // 1. How many chords and when: n08's segments, n01's rhythm or one
        // length each (times depend on the count alone).
        let segs: Option<Vec<(u64, u64)>> = match &new.harmonize {
            Some(h) if !src.notes.is_empty() => Some(cp_segments(src, &self.ctx, h.0)),
            _ => None,
        };
        // n11's repeats (the chain is one unit: under Control Sweeps its
        // value at the progression's start, E0422).
        let repeat = new.chain.as_ref().map_or(1, |c| self.chain_repeat(c.2));
        let count = match (&segs, &new.chain) {
            (Some(s), _) => s.len(),
            (None, Some((list, _, _))) => 1 + list.len() * repeat,
            _ => plan.chords.len(),
        };
        let times: Vec<(u64, u64)> = match (&segs, &new.rhythm) {
            (Some(t), _) => t.clone(),
            (None, Some((beats, rate, snap))) => {
                let ppq = self.ctx.ppq.max(1);
                let mut t = start;
                (0..count)
                    .map(|i| {
                        let x = if count > 1 {
                            i as f64 / (count - 1) as f64
                        } else {
                            0.0
                        };
                        let b = beats[i % beats.len()].max(1) as f64
                            * 2f64.powf(1.0 - 2.0 * rate.eval(x));
                        let unit = match snap {
                            0 => ppq,
                            1 => metric_position(&self.ctx.meter, t).bar_len.max(1),
                            _ => (ppq / 2).max(1),
                        };
                        let l = ((b * ppq as f64 / unit as f64).round().max(1.0) as u64)
                            .saturating_mul(unit);
                        let s = t;
                        t = t.saturating_add(l);
                        (s, l)
                    })
                    .collect()
            }
            (None, None) => (0..count)
                .map(|i| (start.saturating_add((i as u64).saturating_mul(len)), len))
                .collect(),
        };
        // Each chord's functions (its own place's under Control Sweeps).
        *units = self.unit_news(src, &times);
        let nw = |i: usize| units.get(i).unwrap_or(new);
        // 2. Chords.
        let mut chords = match (&new.harmonize, &segs, &new.chain) {
            (Some(h), Some(segs), _) => cp_harmonize(src, scale, self.seed, h, segs, &|si| {
                nw(si).harmonize.as_ref().map_or(h.3, |x| x.3)
            }),
            (_, _, Some((list, typed, _))) => {
                let tonic = || {
                    let c = diatonic(scale, 0, 3);
                    triad(i64::from(c.root), is_minor(&c))
                };
                let first = if *typed {
                    let pp = Params::new(&self.specs, &self.set).with_keys(self.ctx.keys);
                    pp.text("text")
                        .split([' ', ',', '-', '|'])
                        .find(|t| !t.trim().is_empty())
                        .and_then(|t| {
                            parse_roman(t.trim(), scale).or_else(|| parse_chord(t.trim()))
                        })
                        .map(|c| triad(i64::from(c.root), is_minor(&c)))
                        .unwrap_or_else(tonic)
                } else {
                    tonic()
                };
                let mut v = vec![first];
                for _ in 0..repeat {
                    for &t in list {
                        let next = neo_riemannian(v.last().unwrap(), t);
                        v.push(next);
                    }
                }
                v
            }
            _ => {
                let mut c = plan.chords;
                if let Some(walk) = &new.walk {
                    for i in 0..c.len() {
                        if !plan.generated.get(i).copied().unwrap_or(false) {
                            continue;
                        }
                        let mut r = stream(self.seed, salt_of("n09"), &[i as u64]);
                        c[i] = match i.checked_sub(1).map(|j| c[j].clone()) {
                            Some(prev) => match cp_walk_step(&prev, scale, walk, &mut r) {
                                Some(x) => x,
                                None => continue,
                            },
                            None => {
                                let first = nw(i).walk.as_ref().map_or(walk.1, |w| w.1);
                                cp_quality(scale.pitch_of(first), scale, walk.2, &mut r)
                            }
                        };
                    }
                }
                c
            }
        };
        debug_assert_eq!(chords.len(), count);
        let count = chords.len();
        // Each chord's phrase length (n04's).
        let phrase = |i: usize| nw(i).phrase();
        let ends_phrase = |i: usize| (i + 1).is_multiple_of(phrase(i)) || i + 1 == count;
        // 3. n04 cadences, then n02 borrowing.
        if let Some((_, cadence, strength)) = new.cadence {
            let (a, b) = [(4, 0), (3, 0), (3, 4), (4, 5)][cadence.min(3)];
            for e in 1..count {
                let strength = nw(e).cadence.map_or(strength, |c| c.2);
                if ends_phrase(e)
                    && !e.is_multiple_of(phrase(e))
                    && stream(self.seed, salt_of("n04"), &[e as u64]).chance(strength)
                {
                    chords[e - 1] = diatonic(scale, a, 3);
                    chords[e] = diatonic(scale, b, 3);
                }
            }
        }
        if let Some((share, from, wh)) = new.borrow {
            let borrowed = Scale::new(
                usize::from(scale.root),
                match from {
                    0 if scale.steps.contains(&4) => 1,
                    0 => 0,
                    1 => 4,
                    2 => 7,
                    3 => 5,
                    _ => 6,
                },
            );
            for (i, c) in chords.iter_mut().enumerate() {
                let Some(d) = degree_in(scale, c.root) else {
                    continue;
                };
                let here = match wh {
                    0 => true,
                    1 => d == 1 || d == 3,
                    _ => ends_phrase(i) || ends_phrase(i + 1),
                };
                let share = nw(i).borrow.map_or(share, |b| b.0);
                if here && stream(self.seed, salt_of("n02"), &[i as u64]).chance(share) {
                    *c = diatonic(&borrowed, d, c.intervals.len().clamp(3, 7));
                }
            }
        }
        let mut slots: Vec<Slot> = chords
            .into_iter()
            .zip(times)
            .enumerate()
            .map(|(i, (chord, (t0, len)))| Slot {
                t0,
                len,
                chord,
                pos: Some(i),
                key: i as u64,
                unit: i,
            })
            .collect();
        // 4. Insertions at the end of the slot before a chosen chord.
        let insert = |slots: &mut Vec<Slot>, at: usize, d: u64, chord: ChordShape, key: u64| {
            let p = &mut slots[at - 1];
            if p.len < 2 {
                return;
            }
            let d = d.clamp(1, p.len - 1);
            p.len -= d;
            let t0 = p.t0.saturating_add(p.len);
            slots.insert(
                at,
                Slot {
                    t0,
                    len: d,
                    chord,
                    pos: None,
                    key,
                    unit: slots[at].unit,
                },
            );
        };
        if let Some((before, positions, kind, share)) = &new.secondary {
            let mut k = 1;
            while k < slots.len() {
                let i = slots[k].pos.unwrap_or(0);
                let here = match before {
                    0 => true,
                    1 => metric_position(&self.ctx.meter, slots[k].t0).bar_start == slots[k].t0,
                    _ => positions.contains(&(i as i64 + 1)),
                };
                if here && slots[k].pos.is_some() {
                    let r = i64::from(slots[k].chord.root);
                    let dom = match kind {
                        0 => ChordShape {
                            root: (r + 7).rem_euclid(12) as u8,
                            intervals: vec![0, 4, 7, 10],
                            bass: None,
                        },
                        1 => ChordShape {
                            root: (r + 11).rem_euclid(12) as u8,
                            intervals: vec![0, 3, 6, 9],
                            bass: None,
                        },
                        _ => ChordShape {
                            root: (r + 1).rem_euclid(12) as u8,
                            intervals: vec![0, 4, 7, 10],
                            bass: None,
                        },
                    };
                    let share = nw(i).secondary.as_ref().map_or(*share, |x| x.3);
                    let d = (slots[k - 1].len as f64 * share).round() as u64;
                    let n = slots.len();
                    insert(&mut slots, k, d, dom, 1 << 40 | (i as u64) << 2);
                    k += slots.len() - n;
                }
                k += 1;
            }
        }
        if let Some((plen, kind, wh)) = new.passing {
            let mut k = 1;
            let mut prev_main: Option<ChordShape> = slots.first().map(|s| s.chord.clone());
            while k < slots.len() {
                let Some(i) = slots[k].pos else {
                    k += 1;
                    continue;
                };
                let target = slots[k].chord.clone();
                let changes = prev_main.as_ref().is_none_or(|p| *p != target);
                prev_main = Some(target.clone());
                let here = match wh {
                    0 => changes,
                    _ => i.is_multiple_of(phrase(i)),
                };
                if here {
                    let plen = nw(i).passing.map_or(plen, |x| x.0);
                    let r = i64::from(target.root);
                    let chord = match kind {
                        0 => ChordShape {
                            root: (r + 1).rem_euclid(12) as u8,
                            ..target.clone()
                        },
                        1 => ChordShape {
                            root: (r + 11).rem_euclid(12) as u8,
                            ..target.clone()
                        },
                        _ => match degree_in(scale, target.root) {
                            Some(d) => diatonic(scale, d + 1, 3),
                            None => ChordShape {
                                root: (r + 1).rem_euclid(12) as u8,
                                ..target.clone()
                            },
                        },
                    };
                    let n = slots.len();
                    insert(&mut slots, k, plen, chord, 2 << 40 | (i as u64) << 2);
                    k += slots.len() - n;
                }
                k += 1;
            }
        }
        slots
    }

    /// n11's repeats: under Control Sweeps (E0422) the chain — one unit —
    /// at the progression's start.
    fn chain_repeat(&self, base: usize) -> usize {
        match self.ctx.sweeps.as_deref() {
            Some(sw) if sw.drives("n11.repeat") => {
                let at = TNote {
                    start: self.start,
                    dur: self.len,
                    vel: 100,
                    ..TNote::default()
                };
                let place = super::super::drivers::Place::unit(0, 1);
                let sw = sw.in_frame(super::super::drivers::Frame::of_notes(&[at]));
                sw.int("n11.repeat", base as i64, &at, place).max(1) as usize
            }
            _ => base,
        }
    }

    /// Under Control Sweeps (E0422) each chord's functions, its swept
    /// values at its place among the chords ([`UnitPlaces`]: its index, its
    /// start in the progression, the melody note at its start — pitch and
    /// velocity); empty without sweeps.
    fn unit_news(&self, src: &Source, times: &[(u64, u64)]) -> Vec<CpNew> {
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return Vec::new();
        };
        let ns = &src.notes;
        let notes: Vec<TNote> = times
            .iter()
            .map(|&(t0, len)| {
                // The first melody note starting in the chord, else the one
                // sounding at its start.
                let k = ns.partition_point(|n| n.start < t0);
                let m = ns
                    .get(k)
                    .filter(|n| n.start < t0.saturating_add(len))
                    .or_else(|| ns[..k].iter().rev().find(|n| n.end() > t0));
                TNote {
                    start: t0,
                    dur: len,
                    pitch: m.map_or(0, |n| n.pitch),
                    vel: m.map_or(100, |n| n.vel),
                    ..TNote::default()
                }
            })
            .collect();
        let up = sw.over(notes);
        (0..up.len())
            .map(|i| {
                let set = up.set_at(&self.set, i);
                cp_new(&Params::new(&self.specs, &set).with_keys(self.ctx.keys))
            })
            .collect()
    }

    /// n07: the suspended voice of a chord (its index in `voices`) and its
    /// suspension pitch, or `None`.
    fn suspend(&self, c: &ChordShape, voices: &[CpVoice], kind: usize) -> Option<(usize, i64)> {
        let chordal = || voices.iter().enumerate().filter(|(_, v)| v.3);
        let root = i64::from(c.root);
        let lowest = chordal().min_by_key(|(_, v)| v.0)?;
        let above =
            |p: i64| super::super::shared::scale_lattice::Lattice::of_scale(&self.scale).above(p);
        let highest = |f: &dyn Fn(i64) -> bool| {
            chordal()
                .filter(|(_, v)| f(v.0))
                .max_by_key(|(_, v)| v.0)
                .map(|(i, v)| (i, v.0))
        };
        match kind {
            0 => {
                let third = [4i64, 3]
                    .into_iter()
                    .find(|t| c.intervals.contains(&(*t as u8)))?;
                let (i, p) = highest(&|p| (p - root).rem_euclid(12) == third)?;
                Some((i, p - third + 5))
            }
            1 => {
                let (i, p) = highest(&|p| (p - root).rem_euclid(12) == 0 && p != lowest.1.0)?;
                Some((i, p + 2))
            }
            2 => {
                let b = lowest.1.0;
                let (i, p) = highest(&|p| p > b && matches!((p - b).rem_euclid(12), 8 | 9))?;
                Some((i, above(p)))
            }
            _ => Some((lowest.0, above(lowest.1.0))),
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let pp = Params::new(&self.specs, &self.set).with_keys(self.ctx.keys);
        let bc = BuildCtx {
            ctx: &self.ctx,
            stats: &src.stats,
            tracks: &self.tracks,
        };
        let plan = plan(&pp, &bc, Some(src), self.start, self.len)?;
        let any_new = self.new.any();
        let message = (!any_new).then(|| plan.names.clone());
        let v08_chords = (!any_new).then(|| plan.chords.clone());
        let mut units: Vec<CpNew> = Vec::new();
        let slots = self.slots(plan, src, &mut units);
        let nw = |i: usize| units.get(i).unwrap_or(&self.new);
        out.message = Some(
            message.unwrap_or_else(|| chord_names(slots.iter().map(|s| &s.chord), &self.scale)),
        );
        let (start, len, scale) = (self.start, self.len, &self.scale);
        // Template: the first selected note, or the track's first subchannel.
        let tpl = src.notes.first().copied().unwrap_or_else(|| {
            let (sub, ch) = src
                .tracks
                .first()
                .and_then(|t| t.subs.first().copied())
                .unwrap_or((0, 0));
            TNote {
                sub,
                ch,
                vel: 100,
                ..TNote::default()
            }
        });
        let (voicing, inversion) = (self.voicing, self.inversion);
        let base = (self.octave + 1) * 12 + i64::from(scale.root) + self.transpose;
        let mut prev: Option<Vec<i64>> = None;
        let mut prev_bass: Option<i64> = None;
        let tr = src.tracks.first();
        let count = slots.iter().filter(|s| s.pos.is_some()).count();
        for (si, slot) in slots.iter().enumerate() {
            // V09-0143: the driver dimension is the chord (0 = the
            // progression's first).
            out.set_dim(si as u32);
            let (c, t0, len) = (&slot.chord, slot.t0, slot.len);
            // The chord's functions (its place's under Control Sweeps).
            let new = nw(slot.unit);
            let phrase = new.phrase();
            let mut v = voice(c, base, voicing, inversion);
            if let (Some(goal), Some(pv)) = (self.vl, &prev) {
                let mut best = v.clone();
                let mut best_cost = f64::INFINITY;
                for inv in -2..=2i64 {
                    for oct in -1..=1i64 {
                        let cand: Vec<i64> = voice(c, base + 12 * oct, voicing, inversion + inv);
                        let n = cand.len().min(pv.len());
                        let moves: f64 = (0..n).map(|k| (cand[k] - pv[k]).abs() as f64).sum();
                        let span = (cand.last().unwrap_or(&0) - cand.first().unwrap_or(&0)) as f64;
                        let top =
                            cand.last().copied().unwrap_or(0) - pv.last().copied().unwrap_or(0);
                        let low =
                            cand.first().copied().unwrap_or(0) - pv.first().copied().unwrap_or(0);
                        let cost = match goal {
                            0 => moves,
                            1 => span + moves * 0.1,
                            2 => {
                                moves * 0.2
                                    + if top.signum() * low.signum() < 0 {
                                        0.0
                                    } else {
                                        24.0
                                    }
                            }
                            _ => -moves,
                        };
                        if cost < best_cost {
                            best_cost = cost;
                            best = cand;
                        }
                    }
                }
                v = best;
            }
            if let Some(axis) = self.mirror {
                v = v.iter().map(|p| 2 * axis - p).collect();
                v.sort_unstable();
            }
            // n10: the voice-leading skeleton is the chord before resizing.
            if let Some(size) = new.size {
                prev = Some(v.clone());
                v = cp_resize(v, scale, size);
            }
            // Melody preservation: chords stay below the melody's lowest note.
            let melody_low = if self.melody {
                src.notes
                    .iter()
                    .filter(|n| n.start < t0.saturating_add(len) && n.end() > t0)
                    .map(|n| i64::from(n.pitch))
                    .min()
            } else {
                None
            };
            if let Some(m) = melody_low {
                while v.last().is_some_and(|&top| top >= m) {
                    for x in &mut v {
                        *x -= 12;
                    }
                }
            }
            if self.new.size.is_none() {
                prev = Some(v.clone());
            }
            let mut voices: Vec<CpVoice> = match &self.new.wall {
                Some(w) => cp_wall(&v, c, w)
                    .into_iter()
                    .filter(|&(p, _)| melody_low.is_none_or(|m| p < m))
                    .map(|(p, f)| (p, 0, f, true))
                    .collect(),
                None => v.iter().map(|&p| (p, 0, 1.0, true)).collect(),
            };
            if self.bass {
                let root = base - 24 + (i64::from(c.root) - base.rem_euclid(12)).rem_euclid(12);
                let mut bp = root;
                if let (Some(m), Some(pb)) = (self.motion, prev_bass) {
                    let cands: Vec<i64> = (-2..=2).map(|o| root + 12 * o).collect();
                    bp = match m {
                        0 => *cands.iter().filter(|&&x| x >= pb).min().unwrap_or(&root),
                        1 => *cands.iter().filter(|&&x| x <= pb).max().unwrap_or(&root),
                        2 => base - 24,
                        3 => *cands
                            .iter()
                            .min_by_key(|&&x| (x - pb).abs())
                            .unwrap_or(&root),
                        _ => *cands
                            .iter()
                            .filter(|&&x| (x - pb).abs() >= 5)
                            .min_by_key(|&&x| (x - pb).abs())
                            .unwrap_or(&root),
                    };
                } else if self.motion == Some(2) {
                    bp = base - 24;
                }
                prev_bass = Some(bp);
                voices.insert(0, (bp, 0, 1.0, true));
            }
            if let Some((iv, oct, sub_step)) = self.poly {
                let up = ChordShape {
                    root: ((i64::from(c.root) + iv).rem_euclid(12)) as u8,
                    intervals: vec![0, 4, 7],
                    bass: None,
                };
                for x in voice(&up, base + 12 * oct, 0, 0) {
                    voices.push((x, sub_step, 1.0, false));
                }
            }
            // n07: one voice suspended, resolving at a share of the chord.
            let sus = match (&new.suspension, slot.pos) {
                (Some((kind, wh, positions, at)), Some(i)) if len >= 2 => {
                    let here = match wh {
                        0 => true,
                        1 => (i + 1).is_multiple_of(phrase) || i + 1 == count,
                        _ => positions.contains(&(i as i64 + 1)),
                    };
                    here.then(|| self.suspend(c, &voices, *kind))
                        .flatten()
                        .map(|(vi, sp)| {
                            let r = ((len as f64 * at).round() as u64).clamp(1, len - 1);
                            (vi, sp, t0.saturating_add(r))
                        })
                }
                _ => None,
            };
            // Rhythm: one block, or seeded hits on a 1/16-of-chord grid.
            let hits: Vec<(u64, u64)> = match self.fracture {
                Some(k) => {
                    let grid = (len / 16).max(1);
                    let mut r = Rng::new(hash(&[self.seed, salt::ORDER, slot.key]));
                    let mut cuts: Vec<u64> = (1..16).collect();
                    for j in (1..cuts.len()).rev() {
                        let x = r.range_i64(0, j as i64) as usize;
                        cuts.swap(j, x);
                    }
                    let mut c: Vec<u64> = cuts
                        .into_iter()
                        .take((k - 1) as usize)
                        .map(|x| x * grid)
                        // Cuts inside the chord only (a chord shorter than
                        // 16 ticks has fewer).
                        .filter(|&a| a < len)
                        .collect();
                    c.push(0);
                    c.sort_unstable();
                    c.iter()
                        .enumerate()
                        .map(|(j, &a)| {
                            let b2 = c.get(j + 1).copied().unwrap_or(len);
                            (t0.saturating_add(a), (b2 - a).max(1))
                        })
                        .collect()
                }
                None => vec![(t0, len)],
            };
            for (vi, &(pitch, sub_step, factor, _)) in voices.iter().enumerate() {
                let mut n = tpl;
                n.group = 0;
                if let Some(t) = tr
                    && !t.subs.is_empty()
                {
                    if !self.layers.is_empty() {
                        let (id, ch) = t.subs[(self.layers[vi % self.layers.len()]
                            .rem_euclid(t.subs.len() as i64))
                            as usize];
                        n.sub = id;
                        n.ch = ch;
                    } else if sub_step != 0 {
                        let (id, ch) = t.sub_offset(tpl.sub, sub_step);
                        n.sub = id;
                        n.ch = ch;
                    }
                }
                let vel = if factor == 1.0 {
                    i64::from(tpl.vel)
                } else {
                    (f64::from(tpl.vel) * factor).round() as i64
                };
                for &(s, d) in &hits {
                    match sus {
                        Some((si, sp, at)) if si == vi => {
                            let e = s.saturating_add(d);
                            if s < at {
                                out.emit(i128::from(s), i128::from(e.min(at) - s), sp, vel, &n);
                            }
                            if e > at {
                                let a = s.max(at);
                                out.emit(i128::from(a), i128::from(e - a), pitch, vel, &n);
                            }
                        }
                        _ => {
                            out.emit(i128::from(s), i128::from(d), pitch, vel, &n);
                        }
                    }
                }
            }
            out.check()?;
        }
        // The selected notes: kept, replaced, or snapped to chord tones.
        let chord_at = |t: u64| -> Option<&ChordShape> {
            match &v08_chords {
                Some(chords) => chords.get(((t.saturating_sub(start)) / len.max(1)) as usize),
                None => {
                    let k = slots.partition_point(|s| s.t0 <= t);
                    let s = slots.get(k.checked_sub(1)?)?;
                    (t < s.t0.saturating_add(s.len)).then_some(&s.chord)
                }
            }
        };
        out.set_dim(0);
        for n in &src.notes {
            match self.existing {
                1 => {}
                2 => {
                    let p = match chord_at(n.start) {
                        Some(c) => {
                            let mask = c.pitch_classes();
                            let ok = |p: i64| mask & (1 << p.rem_euclid(12)) != 0;
                            let mut up = i64::from(n.pitch);
                            let mut dn = up;
                            while !ok(up) {
                                up += 1;
                            }
                            while !ok(dn) {
                                dn -= 1;
                            }
                            if up - i64::from(n.pitch) <= i64::from(n.pitch) - dn {
                                up
                            } else {
                                dn
                            }
                        }
                        None => i64::from(n.pitch),
                    };
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        p,
                        i64::from(n.vel),
                        n,
                    );
                }
                _ => {
                    out.push(*n);
                }
            }
        }
        Ok(())
    }
}

fn cp_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let len = note_value(p.choice("length"), p.ticks("length.ticks"), b.ctx);
    let start = if b.stats.is_empty() {
        b.ctx.cursor
    } else {
        b.stats.min_start
    };
    let count_bound = (p.int("count") as u64)
        .max(b.stats.span() / len.max(1) + 1)
        .min(256);
    let fracture = p.adv(9).then(|| p.int("a9.hits") as u64);
    let new = cp_new(p);
    // Under Control Sweeps (E0422) the largest chord size and chain the
    // swept ranges allow.
    let swept_max = |key: &str, base: i64| match b.ctx.sweeps.as_deref() {
        Some(sw) => sw.int_bounds(key, base).1.max(base),
        None => base,
    };
    let voices_max = new
        .size
        .map(|s| swept_max("n10.voices", s.0 as i64).clamp(1, 32) as u64);
    let chain_max = new.chain.as_ref().map_or(0, |c| {
        1 + c.0.len() as u64 * swept_max("n11.repeat", c.2 as i64).max(1) as u64
    });
    // v0.9: notes per chord slot — the wall's keys, or a voicing (at most
    // 14: seven tones stacked) / n10's voices, plus the bass and a
    // polychord, one more per hit for a suspension; ×3 slots with inserted
    // secondary dominants and passing chords.
    let per_slot = (1 + u64::from(new.secondary.is_some()) + u64::from(new.passing.is_some()))
        * (match (&new.wall, voices_max) {
            (Some(w), _) => (w.1 - w.0 + 1).max(1) as u64,
            (None, Some(v)) => v.max(14),
            (None, None) => 14,
        } + 5)
        * fracture.unwrap_or(1);
    let est = if new.any() {
        // Chords: the progression's, n11's chain or n08's segments (at most
        // one per 32nd note or per note; counted exactly on the source).
        let ppq = b.ctx.ppq.max(1);
        let chords = count_bound
            .max(chain_max)
            .max(new.harmonize.as_ref().map_or(0, |h| match h.0 {
                2 => b.stats.count as u64 + 1,
                _ => b.stats.span() / (ppq / 8).max(1) + 2,
            }));
        chords
            .saturating_mul(per_slot)
            .saturating_add(b.stats.count as u64)
    } else {
        count_bound * 24 * fracture.unwrap_or(1) + b.stats.count as u64
    };
    let cp = Cp {
        // Parameters are re-read inside the kernel through an owned copy.
        specs: p.specs.to_vec(),
        set: p.set.clone(),
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        start,
        len,
        octave: p.int("octave"),
        voicing: p.choice("voicing"),
        inversion: p.int("inversion"),
        bass: p.flag("bass") || p.adv(7),
        transpose: p.int("transpose"),
        existing: if p.adv(10) { 0 } else { p.choice("existing") },
        vl: p.adv(2).then(|| p.choice("a2.goal")),
        poly: p
            .adv(3)
            .then(|| (p.int("a3.interval"), p.int("a3.octave"), p.int("a3.sub"))),
        mirror: p.adv(5).then(|| p.int("a5.axis")),
        motion: p.adv(7).then(|| p.choice("a7.motion")),
        layers: if p.adv(8) {
            p.list("a8.seq")
        } else {
            Vec::new()
        },
        fracture,
        melody: p.adv(10),
        seed: p.seed("seed"),
        scale: Scale::new(p.choice("key"), p.choice("scale")),
        new,
        est,
        per_slot,
        count_bound,
        chain_max,
    };
    Ok(Box::new(cp))
}

impl Kernel for Cp {
    fn growth(&self) -> Growth {
        Growth::Add(self.est)
    }

    fn estimate(&self, stats: &super::super::SourceStats) -> super::super::Estimate {
        super::super::Estimate::for_notes(stats.count as u64, self.est)
    }

    /// n08 counts its segments on the source (one chord per distinct onset,
    /// bar or beat), so a large melody harmonized per note is not refused by
    /// the bound of one chord per note.
    fn estimate_source(&self, src: &Source) -> super::super::Estimate {
        match &self.new.harmonize {
            Some(h) if !src.notes.is_empty() => {
                let chords = cp_segment_bound(src, &self.ctx, h.0)
                    .max(self.count_bound)
                    .max(self.chain_max);
                let est = chords
                    .saturating_mul(self.per_slot)
                    .saturating_add(src.notes.len() as u64);
                super::super::Estimate::for_notes(src.notes.len() as u64, est)
            }
            _ => self.estimate(&src.stats),
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        Cp::run(self, src, out)?;
        out.check()
    }
}

/// Chord names of the progression the parameters describe (for the window).
pub fn describe(p: &Params, b: &BuildCtx, src: Option<&Source>) -> Option<String> {
    let len = note_value(p.choice("length"), p.ticks("length.ticks"), b.ctx);
    let start = if b.stats.is_empty() {
        b.ctx.cursor
    } else {
        b.stats.min_start
    };
    plan(p, b, src, start, len).ok().map(|p| p.names)
}

// ================================================================ Chord Stamp

pub const CHORD_STAMP: ToolDef = ToolDef {
    id: "chord_stamp",
    spec: cs_spec,
    build: cs_build,
};

fn chord_type_names() -> &'static [&'static str] {
    const N: [&str; 21] = [
        "Major",
        "Minor",
        "Diminished",
        "Augmented",
        "Sus2",
        "Sus4",
        "Power",
        "Dominant 7",
        "Major 7",
        "Minor 7",
        "Minor major 7",
        "Half-diminished",
        "Diminished 7",
        "Add 9",
        "Major 6",
        "Minor 6",
        "Dominant 9",
        "Major 9",
        "Minor 9",
        "Dominant 11",
        "Dominant 13",
    ];
    &N
}

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CHORD_STAMP_HELP: &[(&str, &str)] = &[
    ("chord", "The chord every selected note becomes."),
    (
        "inversion",
        "Moves the chord's lowest voices up an octave (+) or its highest down (−), this many times.",
    ),
    (
        "placement",
        "Root builds the chord upward from the note; Top builds it downward so the note is the chord's top voice.",
    ),
    (
        "a1",
        "Builds the chord from your own intervals instead of a chord type.",
    ),
    (
        "a1.intervals",
        "The chord's intervals from the root, in semitones (0 5 10 15 = stacked fourths).",
    ),
    (
        "a2",
        "Builds the chord from the key and scale, stacking scale thirds on the note, so every chord stays in key.",
    ),
    ("a2.key", "The key the chords follow."),
    ("a2.scale", "The scale whose thirds are stacked."),
    (
        "a3",
        "Re-voices each chord to move as little as possible from the chord before it.",
    ),
    (
        "a4",
        "Opens the chord up: from closed (0) to widely spread voices over several octaves (1).",
    ),
    (
        "a4.spread",
        "How far the voices are spread: 0 = closed, 1 = each voice about an octave further than the last.",
    ),
    (
        "a5",
        "Adds the note's pitch class as a bass note in a fixed octave.",
    ),
    (
        "a5.octave",
        "The bass note's octave (2 = two octaves below middle C).",
    ),
    (
        "a6",
        "Gives each voice of the chord its own subchannel, low to high.",
    ),
    (
        "a6.seq",
        "The subchannel of each voice from the lowest up, by place in the track's list (0 = the first); the list repeats.",
    ),
    (
        "a7",
        "Gives each voice of the chord its own velocity, low to high.",
    ),
    (
        "a7.vel",
        "The velocity of each voice from the lowest up, as a percentage of the note's; the list repeats.",
    ),
    (
        "a8",
        "Strikes the chord several times within the note instead of once.",
    ),
    (
        "a8.onsets",
        "Where each hit starts, as a percentage of the note's length (0 = its start).",
    ),
    (
        "a9",
        "Morphs the chord shape into another across the selection.",
    ),
    (
        "a9.to",
        "The chord type the shapes morph toward by the selection's end.",
    ),
    (
        "a10",
        "Adds copies of the chord in higher octaves, each quieter.",
    ),
    (
        "a10.copies",
        "How many octave copies are added above the chord.",
    ),
    (
        "a10.decay",
        "Each copy's velocity as a percentage of the copy below it.",
    ),
    (
        "n01",
        "Chooses the chord type by the note's pitch class: a C gets one chord, a D another …",
    ),
    (
        "n01.map",
        "The chord type for C, C♯ … B in order, by its place in the Chord list (0 = Major); 21 = no chord.",
    ),
    (
        "n01.unlisted",
        "Pitch classes marked 21 get the main chord, or are left alone.",
    ),
    ("n02", "Makes louder notes stamp chords with more voices."),
    ("n02.lo", "How many voices a note at velocity 1 gets."),
    ("n02.hi", "How many voices a note at velocity 127 gets."),
    (
        "n02.by",
        "How voices are added: extensions (sevenths, ninths …), or octave doublings.",
    ),
    (
        "n03",
        "Stamps a cluster of neighboring notes instead of a chord.",
    ),
    ("n03.width", "How many notes the cluster holds."),
    (
        "n03.kind",
        "The cluster's notes: every semitone, the scale's notes, or whole tones.",
    ),
    (
        "n03.place",
        "Where the cluster lies: above the note, below it, or centered on it.",
    ),
    ("n03.key", "For Scale: the key."),
    (
        "n03.scale",
        "For Scale: the scale the cluster is built from.",
    ),
    (
        "n12",
        "Avoids harsh intervals inside the chord (half steps, tritones …).",
    ),
    (
        "n12.avoid",
        "The intervals to avoid between any two voices, in semitones (1 = a half step, 6 = a tritone).",
    ),
    (
        "n12.fix",
        "A clashing voice moves up an octave, or is dropped.",
    ),
    (
        "n12.keep",
        "Which voices never move: the bass, the top, or both.",
    ),
    ("n05", "Stamps the note's overtone series as the chord."),
    (
        "n05.partials",
        "How many partials (overtones) are stamped, the note itself being the first.",
    ),
    (
        "n05.skip",
        "Partials left out (7 = the out-of-tune seventh partial).",
    ),
    (
        "n05.rolloff",
        "Each partial is this much quieter than the one below.",
    ),
    (
        "n06",
        "Drops chosen voices an octave or more, for jazz-style drop voicings.",
    ),
    (
        "n06.drop",
        "Drop 2 lowers the second voice from the top; Drop 3 the third; Drop 2 and 4 both; Listed Voices the ones below.",
    ),
    (
        "n06.voices",
        "For Listed Voices: the voices dropped, counted from the top (1 = the top voice).",
    ),
    ("n06.by", "How many octaves the dropped voices move down."),
    (
        "n07",
        "Steps through a list of chord types, one after another.",
    ),
    (
        "n07.types",
        "The chord types in turn, by their place in the Chord list (0 = Major); the list repeats.",
    ),
    (
        "n07.per",
        "When the next type is used: each note, each beat, or each bar.",
    ),
    (
        "n08",
        "Chooses each chord's inversion so it fits inside a key range.",
    ),
    ("n08.lo", "The lowest key the chord may reach."),
    ("n08.hi", "The highest key the chord may reach."),
    (
        "n08.prefer",
        "Among inversions that fit, prefer the one closest to the note, the lowest, or the highest.",
    ),
    (
        "n09",
        "Gives each voice of the chord its own number of hits over the note, for a polyrhythm.",
    ),
    (
        "n09.hits",
        "The number of evenly spaced hits of each voice in turn.",
    ),
    ("n09.gate", "Each hit's length as a share of its slot."),
    (
        "n09.from",
        "Count the voices from the bottom or from the top.",
    ),
    ("n10", "Moves each voice by its own number of octaves."),
    (
        "n10.octaves",
        "The octave shift of each voice from the lowest up; the list repeats.",
    ),
    (
        "n10.rotate",
        "Fixed applies the same pattern to every chord; Rotate shifts it by one voice per note.",
    ),
    (
        "n11",
        "Keeps the stamped chords under a limit of voices sounding at once.",
    ),
    (
        "n11.most",
        "The most voices that may sound at the same moment.",
    ),
    (
        "n11.drop",
        "Which voices are dropped first: inner voices, the quietest, or the newest.",
    ),
];

fn cs_spec() -> ToolSpec {
    let mut p = vec![
        choice("chord", "Chord", chord_type_names(), 0),
        int("inversion", "Inversion", -4, 4, 0),
        choice(
            "placement",
            "The note is the",
            &["Root (bottom-up)", "Top (top-down)"],
            0,
        ),
        adv_switch(1, "User-defined template"),
        list(
            "a1.intervals",
            "Intervals from the root",
            0,
            48,
            12,
            &[0, 5, 10, 15],
        )
        .adv(1),
        adv_switch(2, "Scale-aware chords"),
        choice("a2.key", "Key", &KEY_NAMES, 0).adv(2),
        choice("a2.scale", "Scale", scale_names(), 0).adv(2),
        adv_switch(3, "Smart voice leading from the prior chord"),
        adv_switch(4, "Spread ↔ closed continuum"),
        float("a4.spread", "Spread", 0.0, 1.0, 0.5, "").adv(4),
        adv_switch(5, "Bass-note lock"),
        int("a5.octave", "Bass octave", 0, 5, 2)
            .keyed_octaves()
            .adv(5),
        adv_switch(6, "Subchannel per voice"),
        list(
            "a6.seq",
            "Subchannel position per voice",
            0,
            4095,
            16,
            &[0, 1, 2],
        )
        .adv(6),
        adv_switch(7, "Velocity profile per voice"),
        list(
            "a7.vel",
            "Velocity % per voice (low → high)",
            1,
            200,
            16,
            &[100, 70, 80, 110],
        )
        .adv(7),
        adv_switch(8, "Rhythmized stamp"),
        list(
            "a8.onsets",
            "Hits at % of the note",
            0,
            99,
            32,
            &[0, 25, 50, 75],
        )
        .adv(8),
        adv_switch(9, "Chord-shape morph"),
        choice("a9.to", "Morph to", chord_type_names(), 9).adv(9),
        adv_switch(10, "Multi-octave fractal stamp"),
        int("a10.copies", "Octave copies", 1, 4, 1).adv(10),
        float("a10.decay", "Velocity per copy", 10.0, 100.0, 70.0, "%").adv(10),
        // v0.9 (V09-S35): functions declared by id (n04 does not exist).
        flag("n01", "Pitch-class chord map", false),
        list(
            "n01.map",
            "Chord type per pitch class (C … B)",
            0,
            21,
            12,
            &[8, 21, 18, 21, 9, 0, 21, 7, 21, 9, 21, 11],
        ),
        choice("n01.unlisted", "Unlisted pitch classes", CS_UNLISTED, 0),
        flag("n02", "Velocity to voices", false),
        int("n02.lo", "Voices at velocity 1", 1, 16, 2),
        int("n02.hi", "Voices at velocity 127", 1, 16, 6),
        choice("n02.by", "Add voices by", CS_ADD_BY, 0),
        flag("n03", "Cluster stamp", false),
        int("n03.width", "Cluster width", 2, 48, 5),
        choice("n03.kind", "Cluster kind", CS_CLUSTER_KINDS, 0),
        choice("n03.place", "Cluster place", CS_PLACES, 0),
        choice("n03.key", "Cluster key", &KEY_NAMES, 0),
        choice("n03.scale", "Cluster scale", scale_names(), 0),
        flag("n12", "Interval avoidance", false),
        list(
            "n12.avoid",
            "Avoid intervals (semitones)",
            1,
            11,
            11,
            &[1, 6],
        ),
        choice("n12.fix", "Fix by", CS_FIX, 0),
        choice("n12.keep", "Keep in place", CS_KEEP, 2),
        flag("n05", "Overtone stamp", false),
        int("n05.partials", "Partials", 2, 64, 8),
        list("n05.skip", "Leave out partials", 2, 64, 63, &[7]),
        float("n05.rolloff", "Rolloff per partial", 0.0, 50.0, 10.0, "%"),
        flag("n06", "Drop voicings", false),
        choice("n06.drop", "Drop", CS_DROPS, 0),
        list(
            "n06.voices",
            "Voices to drop (from the top)",
            1,
            16,
            16,
            &[2],
        ),
        int("n06.by", "Drop by (octaves)", 1, 3, 1),
        flag("n07", "Chord type sequence", false),
        list("n07.types", "Chord types", 0, 20, 64, &[0, 9, 7, 8]),
        choice("n07.per", "Advance per", CS_ADVANCE, 0),
        flag("n08", "Range-fit inversions", false),
        pitch("n08.lo", "Range low", 48),
        pitch("n08.hi", "Range high", 72),
        choice("n08.prefer", "Prefer", CS_PREFER, 0),
        flag("n09", "Voice polyrhythm", false),
        list("n09.hits", "Hits per voice", 1, 64, 16, &[3, 4, 5]),
        float("n09.gate", "Polyrhythm gate", 5.0, 100.0, 50.0, "%"),
        choice("n09.from", "Voices count from", CS_FROM, 0),
        flag("n10", "Voice octave pattern", false),
        list(
            "n10.octaves",
            "Octaves per voice (low → high)",
            -3,
            3,
            16,
            &[0, 1, -1],
        ),
        choice("n10.rotate", "Pattern", CS_ROTATE, 0),
        flag("n11", "Voice budget", false),
        int("n11.most", "Most voices sounding", 1, 1_000_000, 64),
        choice("n11.drop", "Drop first", CS_BUDGET_DROP, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, CHORD_STAMP_HELP);
    ToolSpec {
        id: "chord_stamp",
        name: "Chord Stamp",
        family: Family::Fl,
        summary: "Turns each selected note into a chord of the chosen type (the note as root or top).",
        params: p,
        advanced: vec![
            "User-defined template",
            "Scale-aware chords",
            "Smart voice leading from the prior chord",
            "Spread ↔ closed continuum",
            "Bass-note lock",
            "Subchannel per voice",
            "Velocity profile per voice",
            "Rhythmized stamp",
            "Chord-shape morph",
            "Multi-octave fractal stamp",
        ],
        functions: [
            ("n01", "Pitch-class chord map", 110),
            ("n02", "Velocity to voices", 120),
            ("n03", "Cluster stamp", 130),
            ("n12", "Interval avoidance", 140),
            ("n05", "Overtone stamp", 150),
            ("n06", "Drop voicings", 160),
            ("n07", "Chord type sequence", 170),
            ("n08", "Range-fit inversions", 180),
            ("n09", "Voice polyrhythm", 190),
            ("n10", "Voice octave pattern", 200),
            ("n11", "Voice budget", 700),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

const CS_UNLISTED: &[&str] = &["Use the main chord", "Leave the note alone"];
const CS_ADD_BY: &[&str] = &["Extensions", "Octave doublings"];
const CS_CLUSTER_KINDS: &[&str] = &["Chromatic", "Scale", "Whole-tone"];
const CS_PLACES: &[&str] = &["Above", "Below", "Centered"];
const CS_FIX: &[&str] = &["Move the voice up an octave", "Drop the voice"];
const CS_KEEP: &[&str] = &["The bass", "The top", "Both"];
const CS_DROPS: &[&str] = &["Drop 2", "Drop 3", "Drop 2 and 4", "Listed voices"];
const CS_ADVANCE: &[&str] = &["Note", "Beat", "Bar"];
const CS_PREFER: &[&str] = &["Closest to the note", "Lowest", "Highest"];
const CS_FROM: &[&str] = &["The bottom", "The top"];
const CS_ROTATE: &[&str] = &["Fixed", "Rotate by one voice per note"];
const CS_BUDGET_DROP: &[&str] = &["Inner voices", "Quietest", "Newest"];

/// The Chord Stamp's v0.9 functions declared by id (TOOL_CATALOG
/// `chord_stamp` n01 – n03, n12, n05 – n11). `None` = switched off; with
/// every one off the kernel is the v0.8 one.
#[derive(Clone, Default)]
struct CsNew {
    /// n01: chord type per pitch class (21+ = unlisted), skip the unlisted.
    map: Option<(Vec<i64>, bool)>,
    /// n02: voices at velocity 1 and 127, by doublings (else extensions).
    by_velocity: Option<(i64, i64, bool)>,
    /// n03: width, kind (0 chromatic, 1 scale, 2 whole-tone), place (0
    /// above, 1 below, 2 centred), lattice.
    cluster: Option<(i64, usize, usize, Lattice)>,
    /// n12: avoided distances, drop (else move up), keep (0 bass, 1 top, 2
    /// both).
    avoid: Option<(Vec<i64>, bool, usize)>,
    /// n05: partials, left out, rolloff 0–0.5.
    overtones: Option<(i64, Vec<i64>, f64)>,
    /// n06: voices from the top (1-based), octaves.
    drop: Option<(Vec<i64>, i64)>,
    /// n07: chord types, advance per (0 note, 1 beat, 2 bar).
    sequence: Option<(Vec<i64>, usize)>,
    /// n08: low, high, prefer (0 closest, 1 lowest, 2 highest).
    range: Option<(i64, i64, usize)>,
    /// n09: hits per voice, gate 0–1, from the top.
    poly: Option<(Vec<i64>, f64, bool)>,
    /// n10: octaves per voice, rotating.
    octaves: Option<(Vec<i64>, bool)>,
    /// n11: most voices, drop first (0 inner, 1 quietest, 2 newest).
    budget: Option<(usize, usize)>,
}

impl CsNew {
    fn any(&self) -> bool {
        self.map.is_some()
            || self.by_velocity.is_some()
            || self.cluster.is_some()
            || self.avoid.is_some()
            || self.overtones.is_some()
            || self.drop.is_some()
            || self.sequence.is_some()
            || self.range.is_some()
            || self.poly.is_some()
            || self.octaves.is_some()
            || self.budget.is_some()
    }

    /// Functions that need the note's place in the source or the whole
    /// result.
    fn contextual(&self) -> bool {
        self.sequence.as_ref().is_some_and(|s| s.1 == 0)
            || self.octaves.as_ref().is_some_and(|o| o.1)
            || self.budget.is_some()
    }
}

fn cs_new(p: &Params) -> CsNew {
    let types = |l: Vec<i64>| -> Vec<i64> { l.into_iter().map(|t| t.max(0)).collect() };
    CsNew {
        map: p
            .flag("n01")
            .then(|| (types(p.list("n01.map")), p.choice("n01.unlisted") == 1)),
        by_velocity: p.flag("n02").then(|| {
            (
                p.int("n02.lo").clamp(1, 16),
                p.int("n02.hi").clamp(1, 16),
                p.choice("n02.by") == 1,
            )
        }),
        cluster: p.flag("n03").then(|| {
            (
                p.int("n03.width").max(1),
                p.choice("n03.kind"),
                p.choice("n03.place"),
                Lattice::of_scale(&Scale::new(p.choice("n03.key"), p.choice("n03.scale"))),
            )
        }),
        avoid: p.flag("n12").then(|| {
            (
                p.list("n12.avoid"),
                p.choice("n12.fix") == 1,
                p.choice("n12.keep"),
            )
        }),
        overtones: p.flag("n05").then(|| {
            (
                p.int("n05.partials").clamp(2, 64),
                p.list("n05.skip"),
                (p.float("n05.rolloff") / 100.0).clamp(0.0, 0.5),
            )
        }),
        drop: p.flag("n06").then(|| {
            let voices = match p.choice("n06.drop") {
                0 => vec![2],
                1 => vec![3],
                2 => vec![2, 4],
                _ => p.list("n06.voices"),
            };
            (voices, p.int("n06.by").clamp(1, 3))
        }),
        sequence: p
            .flag("n07")
            .then(|| (types(p.list("n07.types")), p.choice("n07.per")))
            .filter(|s| !s.0.is_empty()),
        range: p.flag("n08").then(|| {
            let (a, b) = (p.int("n08.lo"), p.int("n08.hi"));
            (a.min(b), a.max(b), p.choice("n08.prefer"))
        }),
        poly: p
            .flag("n09")
            .then(|| {
                (
                    p.list("n09.hits"),
                    (p.float("n09.gate") / 100.0).clamp(0.05, 1.0),
                    p.choice("n09.from") == 1,
                )
            })
            .filter(|x| !x.0.is_empty()),
        octaves: p
            .flag("n10")
            .then(|| (p.list("n10.octaves"), p.choice("n10.rotate") == 1))
            .filter(|x| !x.0.is_empty()),
        budget: p
            .flag("n11")
            .then(|| (p.int("n11.most").max(1) as usize, p.choice("n11.drop"))),
    }
}

/// The intervals of chord type `t` (the table's last past its end).
fn cs_type(t: i64) -> Vec<i64> {
    CHORDS[(t.max(0) as usize).min(CHORDS.len() - 1)]
        .2
        .iter()
        .map(|&x| i64::from(x))
        .collect()
}

#[derive(Clone)]
struct Stamp {
    intervals: Vec<i64>,
    morph_to: Option<Vec<i64>>,
    inversion: i64,
    top_down: bool,
    scale: Option<Scale>,
    spread: Option<f64>,
    bass: Option<i64>,
    sub_seq: Vec<i64>,
    vel: Vec<i64>,
    onsets: Vec<i64>,
    copies: Option<(i64, f64)>,
    stats: super::super::SourceStats,
    subs: Vec<Vec<(u32, u8)>>,
    new: CsNew,
    ctx: Ctx,
}

impl Stamp {
    /// The v0.8 voicing of `intervals` on the note (scale-aware [a2], morph
    /// [a9], inversion, placement, spread [a4]) — without the bass lock.
    fn chord(&self, n: &TNote, intervals: &[i64]) -> Vec<i64> {
        let p = i64::from(n.pitch);
        let mut iv: Vec<i64> = match (&self.morph_to, &self.scale) {
            (_, Some(sc)) => {
                let (d, _) = sc.degree_of(sc.snap(p, theory::SnapMode::Nearest));
                (0..intervals.len() as i64)
                    .map(|k| sc.pitch_of(d + 2 * k) - sc.pitch_of(d))
                    .collect()
            }
            (Some(to), None) => {
                let t = pos(n, &self.stats);
                let m = intervals.len().max(to.len());
                (0..m)
                    .map(|i| {
                        let a = intervals.get(i).or(intervals.last()).copied().unwrap_or(0) as f64;
                        let b = to.get(i).or(to.last()).copied().unwrap_or(0) as f64;
                        (a + (b - a) * t).round() as i64
                    })
                    .collect()
            }
            _ => intervals.to_vec(),
        };
        iv.sort_unstable();
        iv.dedup();
        let mut v: Vec<i64> = iv.iter().map(|i| p + i).collect();
        for _ in 0..self.inversion.max(0) {
            let x = v.remove(0);
            v.push(x + 12);
        }
        for _ in 0..(-self.inversion).max(0) {
            if let Some(x) = v.pop() {
                v.insert(0, x - 12);
            }
        }
        if self.top_down {
            let top = *v.iter().max().unwrap_or(&p);
            for x in &mut v {
                *x -= top - p;
            }
        }
        if let Some(s) = self.spread {
            for (i, x) in v.iter_mut().enumerate() {
                *x += 12 * ((s * i as f64) / 2.0).round() as i64;
            }
        }
        v
    }

    /// The v0.8 voices: the chord, then the bass lock [a5].
    fn pitches(&self, n: &TNote) -> Vec<i64> {
        let mut v = self.chord(n, &self.intervals);
        if let Some(oct) = self.bass {
            v.insert(0, (oct + 1) * 12 + i64::from(n.pitch).rem_euclid(12));
        }
        v
    }

    /// The hits of a note ([a8] onsets, or the whole note).
    fn hits(&self, n: &TNote) -> Vec<(u64, u64)> {
        if self.onsets.is_empty() {
            vec![(n.start, n.dur)]
        } else {
            let mut o: Vec<u64> = self.onsets.iter().map(|&x| x.clamp(0, 99) as u64).collect();
            o.sort_unstable();
            o.dedup();
            o.iter()
                .enumerate()
                .map(|(i, &x)| {
                    // x ≤ 100, so start + dur·x/100 ≤ end (no overflow); the
                    // product is taken in u128.
                    let at = |x: u64| n.start + (u128::from(n.dur) * u128::from(x) / 100) as u64;
                    let a = at(x);
                    let b = o.get(i + 1).map_or(n.end(), |&y| at(y));
                    (a, (b - a).max(1))
                })
                .collect()
        }
    }

    fn emit(&self, n: &TNote, voices: &[i64], out: &mut Output) {
        let hits = self.hits(n);
        let copies = self.copies.map_or(1, |c| c.0 + 1);
        for (vi, &p) in voices.iter().enumerate() {
            // V09-0143: the driver dimension is the chord tone (0 = the
            // stamp's lowest voice).
            out.set_dim(vi as u32);
            let mut tpl = *n;
            if !self.sub_seq.is_empty()
                && let Some(t) = self.subs.get(usize::from(n.track))
                && !t.is_empty()
            {
                let (id, ch) =
                    t[(self.sub_seq[vi % self.sub_seq.len()].rem_euclid(t.len() as i64)) as usize];
                tpl.sub = id;
                tpl.ch = ch;
            }
            let vf = if self.vel.is_empty() {
                1.0
            } else {
                self.vel[vi % self.vel.len()] as f64 / 100.0
            };
            for c in 0..copies {
                let decay = self.copies.map_or(1.0, |x| (x.1 / 100.0).powi(c as i32));
                for &(s, d) in &hits {
                    out.emit(
                        i128::from(s),
                        i128::from(d),
                        p + 12 * c,
                        (f64::from(n.vel) * vf * decay).round() as i64,
                        &tpl,
                    );
                }
            }
        }
    }

    /// v0.9: the voices (pitch, velocity factor) of note `n`, the `i`-th of
    /// the source, or `None` to leave it alone (n01's unlisted classes):
    /// the chord type (n07 sequence, else n01 map, else the main chord), the
    /// v0.8 voicing, n03 cluster / n05 overtones in its place, n02 voice
    /// count, n06 drops, n10 octave pattern, n08 range fit, the bass lock
    /// [a5], n12 interval avoidance.
    fn voices(&self, n: &TNote, i: usize) -> Option<Vec<(i64, f64)>> {
        use super::super::shared::metric_position::metric_position;
        let new = &self.new;
        let p = i64::from(n.pitch);
        // 1. The chord type.
        let intervals: Vec<i64> = match (&new.sequence, &new.map) {
            (Some((types, per)), _) => {
                let k = match per {
                    0 => i as u64,
                    1 => n.start / self.ctx.ppq.max(1),
                    _ => metric_position(&self.ctx.meter, n.start).bar,
                };
                cs_type(types[(k % types.len() as u64) as usize])
            }
            (None, Some((map, skip))) => match map.get(p.rem_euclid(12) as usize) {
                Some(&t) if (t as usize) < CHORDS.len() => cs_type(t),
                _ if *skip => return None,
                _ => self.intervals.clone(),
            },
            (None, None) => self.intervals.clone(),
        };
        let mut v: Vec<(i64, f64)> = self
            .chord(n, &intervals)
            .into_iter()
            .map(|x| (x, 1.0))
            .collect();
        // 2. A cluster or the overtone series in its place (n05 wins).
        if let Some((width, kind, place, lattice)) = &new.cluster {
            let a = match place {
                0 => p,
                1 => p - width,
                _ => p - width / 2,
            };
            v = (a..=a + width)
                .filter(|&x| {
                    x == p
                        || match kind {
                            0 => true,
                            1 => lattice.contains(x),
                            _ => (x - p).rem_euclid(2) == 0,
                        }
                })
                .map(|x| (x, 1.0))
                .collect();
        }
        if let Some((partials, skip, rolloff)) = &new.overtones {
            v.clear();
            for k in 1..=*partials {
                if k > 1 && skip.contains(&k) {
                    continue;
                }
                let x = p + (12.0 * (k as f64).log2()).round() as i64;
                if v.iter().all(|y| y.0 != x) {
                    v.push((x, (1.0 - rolloff).powi(k as i32 - 1)));
                }
            }
        }
        v.sort_by_key(|x| x.0);
        // 3. Voices by velocity: cut from the top, or grown by stacked thirds
        // (alternating) above the top, or octave doublings from the bottom.
        if let Some((lo, hi, doublings)) = new.by_velocity {
            let want = (lo as f64 + (hi - lo) as f64 * f64::from(n.vel.saturating_sub(1)) / 126.0)
                .round()
                .max(1.0) as usize;
            if v.len() > want {
                v.truncate(want);
            } else if !v.is_empty() {
                let base: Vec<(i64, f64)> = v.clone();
                let mut k = 0usize;
                while v.len() < want && k < 64 {
                    let next = if doublings {
                        let b = base[k % base.len()];
                        (b.0 + 12 * (k / base.len() + 1) as i64, b.1)
                    } else {
                        let top = v[v.len() - 1].0;
                        let last = if v.len() >= 2 {
                            top - v[v.len() - 2].0
                        } else {
                            4
                        };
                        (top + if last == 4 { 3 } else { 4 }, 1.0)
                    };
                    if v.iter().all(|y| y.0 != next.0) {
                        v.push(next);
                    }
                    k += 1;
                }
                v.sort_by_key(|x| x.0);
            }
        }
        // 4. Drops: voices counted from the top an octave (or more) down.
        if let Some((drops, by)) = &new.drop {
            let m = v.len() as i64;
            for &d in drops {
                if d >= 1 && d <= m && m > 1 {
                    v[(m - d) as usize].0 -= 12 * by;
                }
            }
            v.sort_by_key(|x| x.0);
        }
        // 5. Octaves per voice (low → high), fixed or rotating per note.
        if let Some((pattern, rotate)) = &new.octaves {
            let r = if *rotate { i } else { 0 };
            for (k, x) in v.iter_mut().enumerate() {
                x.0 += 12 * pattern[(k + r) % pattern.len()];
            }
            v.sort_by_key(|x| x.0);
        }
        // 6. Range fit: the inversion and octave keeping every voice in
        // [low, high], by preference; else the least outside.
        if let Some(range) = new.range
            && !v.is_empty()
        {
            cs_range_fit(&mut v, p, range, true);
        }
        // 7. The bass lock.
        if let Some(oct) = self.bass {
            v.insert(0, ((oct + 1) * 12 + p.rem_euclid(12), 1.0));
        }
        // 8. Interval avoidance: from the bottom, a voice less than an
        // octave from a kept one by an avoided distance moves up an octave
        // (up to three) or is dropped; the bass / top may stay as they are.
        if let Some((avoid, drop, keep)) = &new.avoid
            && v.len() > 1
        {
            v.sort_by_key(|x| x.0);
            let (low, top) = (0, v.len() - 1);
            let mut kept: Vec<(i64, f64)> = Vec::with_capacity(v.len());
            let clash =
                |x: i64, kept: &[(i64, f64)]| kept.iter().any(|y| avoid.contains(&(x - y.0).abs()));
            for (k, &x) in v.iter().enumerate() {
                let fixed = (k == low && *keep != 1) || (k == top && *keep != 0);
                if fixed || !clash(x.0, &kept) {
                    kept.push(x);
                } else if !drop
                    && let Some(up) = (1..=3).map(|o| x.0 + 12 * o).find(|&u| !clash(u, &kept))
                {
                    kept.push((up, x.1));
                }
            }
            v = kept;
            v.sort_by_key(|x| x.0);
        }
        Some(v)
    }

    /// v0.9: emits note `n`'s voices through [a6] subchannels, [a7] / n05
    /// velocities, n09 polyrhythm (else [a8] hits) and [a10] copies; each
    /// written note's n11 key goes to `keys` (its inner rank in the stamp,
    /// its quietness or its start).
    fn emit_new(&self, n: &TNote, voices: &[(i64, f64)], out: &mut Output, keys: &mut Vec<u64>) {
        let common = self.hits(n);
        let copies = self.copies.map_or(1, |c| c.0 + 1);
        let m = voices.len();
        for (vi, &(p, factor)) in voices.iter().enumerate() {
            out.set_dim(vi as u32);
            let mut tpl = *n;
            if !self.sub_seq.is_empty()
                && let Some(t) = self.subs.get(usize::from(n.track))
                && !t.is_empty()
            {
                let (id, ch) =
                    t[(self.sub_seq[vi % self.sub_seq.len()].rem_euclid(t.len() as i64)) as usize];
                tpl.sub = id;
                tpl.ch = ch;
            }
            let vf = if self.vel.is_empty() {
                1.0
            } else {
                self.vel[vi % self.vel.len()] as f64 / 100.0
            };
            let hits: Vec<(u64, u64)> = match &self.new.poly {
                Some((list, gate, from_top)) => {
                    let k = if *from_top { m - 1 - vi } else { vi };
                    let h = list[k % list.len()].max(1) as u64;
                    (0..h)
                        .map(|j| {
                            let at = |j: u64| {
                                n.start + (u128::from(n.dur) * u128::from(j) / u128::from(h)) as u64
                            };
                            let (a, b) = (at(j), at(j + 1));
                            (a, (((b - a) as f64 * gate).round() as u64).max(1))
                        })
                        .collect()
                }
                None => common.clone(),
            };
            let inner = vi.min(m - 1 - vi) as u64;
            for c in 0..copies {
                let decay = self.copies.map_or(1.0, |x| (x.1 / 100.0).powi(c as i32));
                let vel = (f64::from(n.vel) * vf * factor * decay).round() as i64;
                for &(s, d) in &hits {
                    let before = out.notes.len();
                    out.emit(i128::from(s), i128::from(d), p + 12 * c, vel, &tpl);
                    if self.new.budget.is_some() && out.notes.len() > before {
                        let w = &out.notes[before];
                        keys.push(match self.new.budget.map_or(0, |b| b.1) {
                            0 => inner,
                            1 => 255 - u64::from(w.vel),
                            _ => w.start,
                        });
                    }
                }
            }
        }
    }
}

/// n08: the inversion (the lowest r voices up an octave) and octave (±11)
/// of the voicing `v` (sorted) that keep every voice in `[lo, hi]`, nearest
/// the note `p` (the centre), the lowest or the highest; else the least
/// outside. `restrict` scores only fitting candidates when some fit (the
/// same winner as scoring every candidate, tested).
fn cs_range_fit(v: &mut [(i64, f64)], p: i64, (lo, hi, prefer): (i64, i64, usize), restrict: bool) {
    let m = v.len();
    // Candidates evaluated in place (no voicing per candidate): the
    // lowest r voices up an octave, all shifted by k octaves. When
    // some candidate fits, only fitting ones can win: each rotation's
    // fitting octaves are a range (min + 12k ≥ low, max + 12k ≤
    // high); else every octave −11 … 11 is scored by what lies
    // outside. Same order, same winner as scoring them all.
    let mut c: Vec<i64> = Vec::with_capacity(m);
    let span = |r: usize| {
        let mut lo_c = i64::MAX;
        let mut hi_c = i64::MIN;
        for (i, x) in v.iter().enumerate() {
            let y = x.0 + if i < r { 12 } else { 0 };
            lo_c = lo_c.min(y);
            hi_c = hi_c.max(y);
        }
        (lo_c, hi_c)
    };
    let fits = |(a, b): (i64, i64)| {
        let k0 = (lo - a).div_euclid(12) + i64::from((lo - a).rem_euclid(12) != 0);
        let k1 = (hi - b).div_euclid(12);
        (k0.max(-11), k1.min(11))
    };
    let any_fit = (0..m).any(|r| {
        let (k0, k1) = fits(span(r));
        k0 <= k1
    });
    let mut best: Option<((i64, i64), usize, i64)> = None;
    for r in 0..m {
        c.clear();
        c.extend(
            v.iter()
                .enumerate()
                .map(|(i, x)| x.0 + if i < r { 12 } else { 0 }),
        );
        let (cmin, cmax) = span(r);
        let (k0, k1) = if any_fit && restrict {
            fits((cmin, cmax))
        } else {
            (-11, 11)
        };
        for k in k0..=k1 {
            let s = 12 * k;
            let (a, b) = (cmin + s, cmax + s);
            let outside: i64 = c
                .iter()
                .map(|&y| (lo - y - s).max(0) + (y + s - hi).max(0))
                .sum();
            let score = match prefer {
                0 => (a + b - 2 * p).abs(),
                1 => a,
                _ => -b,
            };
            let key = (outside, score);
            if best.is_none_or(|bb| key < bb.0) {
                best = Some((key, r, k));
            }
        }
    }
    let (_, r, k) = best.expect("a candidate");
    for (i, x) in v.iter_mut().enumerate() {
        x.0 += if i < r { 12 } else { 0 } + 12 * k;
    }
    v.sort_by_key(|x| x.0);
}

/// The Chord Stamp's kernel with a v0.9 function on.
struct CsKernel {
    st: Stamp,
    /// [a3]: each voice's octave nearest the previous chord's.
    voice_leading: bool,
    growth: Growth,
    /// With Control Sweeps (E0422; every target is a function's control):
    /// what each level's settings are built from — every note with its
    /// level's, in one run (a3's previous chord, n07's note index and n11's
    /// budget see the whole selection).
    swept: Option<std::sync::Arc<Rebuild>>,
}

impl CsKernel {
    fn whole(&self) -> bool {
        self.voice_leading || self.st.new.contextual() || self.swept.is_some()
    }
}

impl Kernel for CsKernel {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        (!self.whole()).then_some(self as &dyn PointKernel)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let st = &self.st;
        let lv: Option<PerLevel<Stamp>> = self
            .swept
            .as_ref()
            .map(|r| r.levels(&st.ctx, src, cs_stamp));
        let at = |i: usize| lv.as_ref().map_or(st, |l| l.at(i));
        let base = out.notes.len();
        let mark = out.mark();
        let mut keys: Vec<u64> = Vec::new();
        let mut prev: Option<Vec<i64>> = None;
        for (i, n) in src.notes.iter().enumerate() {
            let st = at(i);
            match st.voices(n, i) {
                Some(mut v) => {
                    if self.voice_leading {
                        if let Some(pv) = &prev {
                            for (k, x) in v.iter_mut().enumerate() {
                                if let Some(&q) = pv.get(k) {
                                    while x.0 - q > 6 {
                                        x.0 -= 12;
                                    }
                                    while q - x.0 > 6 {
                                        x.0 += 12;
                                    }
                                }
                            }
                            v.sort_by_key(|x| x.0);
                        }
                        prev = Some(v.iter().map(|x| x.0).collect());
                    }
                    st.emit_new(n, &v, out, &mut keys);
                }
                None => {
                    out.set_dim(0);
                    let before = out.notes.len();
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        i64::from(n.pitch),
                        i64::from(n.vel),
                        n,
                    );
                    if out.notes.len() > before && st.new.budget.is_some() {
                        keys.push(match st.new.budget.map_or(0, |b| b.1) {
                            0 => 0,
                            1 => 255 - u64::from(n.vel),
                            _ => n.start,
                        });
                    }
                }
            }
            out.check()?;
        }
        // n11: the voices sounding at once under the budget.
        if let Some((most, _)) = st.new.budget {
            let notes = &out.notes[base..];
            let m = notes.len().min(keys.len());
            let keep = poly_cap_keep(&notes[..m], most, false, PolyOrder::Static(&keys[..m]));
            out.adjust_since(mark, |i, _, _, _| i >= m || keep[i]);
        }
        Ok(())
    }
}

impl PointKernel for CsKernel {
    fn map(&self, n: &TNote, out: &mut Output) {
        let mut keys = Vec::new();
        match self.st.voices(n, 0) {
            Some(v) => self.st.emit_new(n, &v, out, &mut keys),
            None => {
                out.emit(
                    i128::from(n.start),
                    i128::from(n.dur),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                );
            }
        }
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        (t0, t1)
    }
}

/// The Chord Stamp's settings from `p`.
fn cs_stamp(p: &Params, b: &BuildCtx) -> Stamp {
    let base: Vec<i64> = if p.adv(1) {
        p.list("a1.intervals")
    } else {
        CHORDS[p.choice("chord").min(CHORDS.len() - 1)]
            .2
            .iter()
            .map(|&x| i64::from(x))
            .collect()
    };
    let new = cs_new(p);
    Stamp {
        intervals: if base.is_empty() { vec![0] } else { base },
        morph_to: p.adv(9).then(|| {
            CHORDS[p.choice("a9.to").min(CHORDS.len() - 1)]
                .2
                .iter()
                .map(|&x| i64::from(x))
                .collect()
        }),
        inversion: p.int("inversion"),
        top_down: p.choice("placement") == 1,
        scale: p
            .adv(2)
            .then(|| Scale::new(p.choice("a2.key"), p.choice("a2.scale"))),
        spread: p.adv(4).then(|| p.float("a4.spread")),
        bass: p.adv(5).then(|| p.int("a5.octave")),
        sub_seq: if p.adv(6) {
            p.list("a6.seq")
        } else {
            Vec::new()
        },
        vel: if p.adv(7) {
            p.list("a7.vel")
        } else {
            Vec::new()
        },
        onsets: if p.adv(8) {
            p.list("a8.onsets")
        } else {
            Vec::new()
        },
        copies: p
            .adv(10)
            .then(|| (p.int("a10.copies"), p.float("a10.decay"))),
        stats: b.stats.clone(),
        subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
        new,
        ctx: b.ctx.clone(),
    }
}

/// The notes per source note with a v0.9 function on: voices per note —
/// the largest chord type, cluster, overtone series or n02 count (+ the
/// bass) — times the hits per voice (n09's most, else [a8]'s) and the
/// copies.
fn cs_per(st: &Stamp) -> u64 {
    {
        let new = &st.new;
        let types_most = |l: &[i64]| l.iter().map(|&t| cs_type(t).len()).max().unwrap_or(0);
        // The keys the overtone series reaches (partials landing on one key
        // count once).
        let overtone_keys = |o: &(i64, Vec<i64>, f64)| {
            let mut keys: Vec<i64> = (1..=o.0)
                .filter(|k| *k == 1 || !o.1.contains(k))
                .map(|k| (12.0 * (k as f64).log2()).round() as i64)
                .collect();
            keys.dedup();
            keys.len()
        };
        let voices = [
            st.intervals.len(),
            st.morph_to.as_ref().map_or(0, Vec::len),
            new.sequence.as_ref().map_or(0, |s| types_most(&s.0)),
            new.map.as_ref().map_or(0, |m| types_most(&m.0)),
            new.cluster.as_ref().map_or(0, |c| c.0 as usize + 1),
            new.overtones.as_ref().map_or(0, overtone_keys),
            new.by_velocity.map_or(0, |b| b.0.max(b.1) as usize),
        ]
        .into_iter()
        .max()
        .unwrap_or(1)
            + usize::from(st.bass.is_some());
        // n09: the hits of the voices in turn (any m ≤ voices take the
        // first m of the cycled list, from the bottom or the top).
        let hits: u64 = match &new.poly {
            Some(x) => (0..voices).map(|k| x.0[k % x.0.len()].max(1) as u64).sum(),
            None => (voices * st.onsets.len().max(1)) as u64,
        };
        hits.max(1) * st.copies.map_or(1, |c| c.0 as u64 + 1)
    }
}

fn cs_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let st = cs_stamp(p, b);
    if st.new.any() {
        // With Control Sweeps (E0422) each note takes its level's settings;
        // the bound is the largest over the swept ranges' corners. (With
        // every function off the sweeps drive nothing: v0.8 below.)
        let mut per = cs_per(&st);
        let swept = Rebuild::of(p, b);
        if let Some(rb) = &swept {
            for c in rb.corners(b.ctx, cs_stamp) {
                per = per.max(cs_per(&c));
            }
        }
        return Ok(Box::new(CsKernel {
            st,
            voice_leading: p.adv(3),
            growth: Growth::Expand(per),
            swept,
        }));
    }
    let per = (st
        .intervals
        .len()
        .max(st.morph_to.as_ref().map_or(0, Vec::len)) as u64
        + 1)
        * (st.onsets.len().max(1) as u64)
        * (st.copies.map_or(1, |c| c.0 as u64 + 1));
    if p.adv(3) {
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| {
                let mut prev: Option<Vec<i64>> = None;
                for n in &src.notes {
                    let mut v = st.pitches(n);
                    if let Some(pv) = &prev {
                        // Octave of each voice nearest the previous chord's voice.
                        for (i, x) in v.iter_mut().enumerate() {
                            if let Some(&q) = pv.get(i) {
                                while *x - q > 6 {
                                    *x -= 12;
                                }
                                while q - *x > 6 {
                                    *x += 12;
                                }
                            }
                        }
                        v.sort_unstable();
                    }
                    st.emit(n, &v, out);
                    prev = Some(v);
                    out.check()?;
                }
                Ok(())
            },
            growth: Growth::Expand(per),
            estimate: None,
        });
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let v = st.pitches(n);
            st.emit(n, &v, out);
        },
        ahead: 0,
        behind: 0,
        growth: Growth::Expand(per),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The restricted range fit picks what scoring every candidate picks.
    #[test]
    fn range_fit_restricted_equals_the_full_scan() {
        let mut x: u64 = 0xF17_0035;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for _ in 0..5000 {
            let n = 1 + next(6) as usize;
            let mut v: Vec<(i64, f64)> = (0..n)
                .map(|_| (next(100) as i64 + 10, next(4) as f64))
                .collect();
            v.sort_by_key(|x| x.0);
            let lo = next(120) as i64;
            let hi = lo + next(40) as i64;
            let p = next(128) as i64;
            let prefer = next(3) as usize;
            let mut a = v.clone();
            let mut b = v.clone();
            cs_range_fit(&mut a, p, (lo, hi, prefer), true);
            cs_range_fit(&mut b, p, (lo, hi, prefer), false);
            assert_eq!(a, b, "{v:?} {lo} {hi} {p} {prefer}");
        }
    }
}
