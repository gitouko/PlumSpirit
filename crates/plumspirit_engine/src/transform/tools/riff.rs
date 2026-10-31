//! Riff Machine: a pipeline of the platform's own kernels (FL_TOOL_BEHAVIOR.md
//! §Riff Machine): note progression → chords → arpeggiation → mirroring →
//! levels → articulation → groove → fit. Each stage builds the registered
//! tool's kernel from a few riff parameters and runs it on the previous stage's
//! output — one implementation per behaviour, no monolithic black box.
//!
//! v0.9 (V09-S35 step 5): eleven functions declared by id (TOOL_CATALOG
//! `riff` n01 … n11) around that pipeline — variants (n08) → sections
//! (n03) → register layers (n02) with a bass stage (n05) inside the
//! pipeline → call and response (n01) → span fill (n07) → bar displacement
//! (n10) → counter-melody (n06) → accent map (n09) → build-up (n04) →
//! ending (n11), then the macro stage [a10]. With every one off the run is
//! the v0.8 one (docs/TRANSFORM_ENGINE.md "Riff Machine").

use super::super::boundary::{self, PitchPolicy};
use super::super::drivers::{Frame, Place};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, note_key, salt};
use super::super::shared::metric_position::metric_position;
use super::super::shared::scale_lattice::Lattice;
use super::super::shared::seeded_stream::{region_seed, salt_of, stream};
use super::super::sweep::{Rebuild, SweptParams};
use super::super::theory::{KEY_NAMES, Scale, SnapMode, parse_chord, parse_roman, scale_names};
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::{Contextual, NOTE_VALUES, boxed, note_value};

pub const RIFF: ToolDef = ToolDef {
    id: "riff",
    spec: riff_spec,
    build: riff_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const RIFF_HELP: &[(&str, &str)] = &[
    (
        "source",
        "The Selected Notes runs your notes through the stages; A Generated Note Progression starts from one long note per bar on the degrees below.",
    ),
    ("bars", "How many bars the generated progression lasts."),
    ("key", "The key of the progression, the chords and the fit."),
    (
        "scale",
        "The scale of the progression, the chords and the fit.",
    ),
    (
        "s1.degrees",
        "Step 1: the scale degree of each bar's note (1 = the key's tonic … 7); the list repeats.",
    ),
    (
        "s2.on",
        "Step 2: turns every progression note into a chord.",
    ),
    ("s2.chord", "The chord built on each progression note."),
    (
        "s3.on",
        "Step 3: arpeggiates the chords (the Arpeggiator with the settings below).",
    ),
    (
        "s3.pattern",
        "The order the arpeggio walks the chord's notes.",
    ),
    (
        "s3.step",
        "The length of one arpeggio step, as a note value.",
    ),
    ("s3.range", "How many octaves the arpeggio spans."),
    (
        "s4.mirror",
        "Step 4: flips the riff in time (horizontally) or upside down in pitch (vertically).",
    ),
    (
        "s5.vel",
        "Step 5: randomizes velocities by up to this share (seeded); 0 skips the step.",
    ),
    (
        "s6.style",
        "Step 6: sets the note lengths legato, portato or staccato (Articulate); Off skips the step.",
    ),
    (
        "s7.on",
        "Step 7: pulls the notes toward a groove grid (Quantize).",
    ),
    ("s7.grid", "The grid the notes are pulled toward."),
    (
        "s7.strength",
        "How far each note moves toward the grid (100 % = onto it).",
    ),
    (
        "s8.on",
        "Step 8: fits the riff into the key range below (Limit).",
    ),
    (
        "s8.lo",
        "The lowest key the riff may use; notes below are moved up by octaves.",
    ),
    (
        "s8.hi",
        "The highest key the riff may use; notes above are moved down by octaves.",
    ),
    (
        "upto",
        "Runs the steps only up to this one (1 = the progression alone, 8 = every step), to hear how the riff builds up.",
    ),
    ("a1", "Lets you run steps 2 – 8 in another order."),
    (
        "a1.order",
        "The order of the stages, by their step numbers (2 chords … 8 fit).",
    ),
    (
        "a2",
        "Builds a second variant with the next seed and merges it with the first.",
    ),
    (
        "a2.merge",
        "Union plays both variants together; Alternate Bars takes even bars from the first and odd bars from the second.",
    ),
    (
        "a3",
        "Morphs the arpeggio step from the main step to another across the riff (seeded crossfade between the two versions).",
    ),
    (
        "a3.step",
        "The arpeggio step the riff morphs toward by its end.",
    ),
    (
        "a4",
        "Mutates the riff over a few generations: notes step up or down the scale by chance (seeded).",
    ),
    ("a4.gens", "How many generations of mutation run."),
    (
        "a4.rate",
        "The chance per generation that a note moves one scale step.",
    ),
    (
        "a5",
        "Repeats the riff's opening bars through the whole riff, so its motif returns unchanged.",
    ),
    ("a5.bars", "How many opening bars make the motif."),
    (
        "a6",
        "Thins the riff to at most a number of notes per bar (seeded choice of the notes kept).",
    ),
    ("a6.per_bar", "The most notes a bar may keep."),
    (
        "a7",
        "Puts the riff's registers on different subchannels, split at the keys below.",
    ),
    (
        "a7.splits",
        "The keys where the next subchannel starts (notes below the first split take the track's first subchannel).",
    ),
    (
        "a8",
        "Moves every note to the nearest tone of the chord written for its bar.",
    ),
    (
        "a8.chords",
        "The chord of each bar in turn, by name (Am F C G); the list repeats.",
    ),
    (
        "a9",
        "Moves some notes one snap step earlier or later (seeded), for rhythmic variation.",
    ),
    ("a9.rate", "The share of notes moved."),
    (
        "a10",
        "Runs one more tool, with your settings, on the finished riff.",
    ),
    (
        "a10.macro",
        "The tool's id, then its settings as key=value pairs (humanize tick.min=-15 tick.max=15).",
    ),
    (
        "n01",
        "Answers every phrase with a changed copy of itself, as call and response.",
    ),
    (
        "n01.bars",
        "How many bars make one phrase (call, then answer).",
    ),
    (
        "n01.answer",
        "How the answer changes the call: transposed, turned upside down, played backward, or moved later.",
    ),
    (
        "n01.interval",
        "For Transposed: how many semitones the answer moves.",
    ),
    (
        "n01.shift",
        "For Displaced: how many riff steps later the answer starts.",
    ),
    (
        "n02",
        "Builds the riff in several register layers, each with its own octave and stages.",
    ),
    ("n02.layers", "How many layers are stacked."),
    (
        "n02.octaves",
        "Each layer's octave shift, in turn; the list repeats.",
    ),
    (
        "n02.stages",
        "Each layer's stages as a sum: 1 chords, 2 arpeggio, 4 mirror, 8 levels, 16 articulation, 32 groove, 64 fit, 128 bass (127 = all but the bass).",
    ),
    (
        "n03",
        "Arranges the riff into sections (A, B …) in a form, varying each repeat.",
    ),
    (
        "n03.form",
        "The sections in order, as 0 = A, 1 = B … 7 = H (0 0 1 0 = A A B A).",
    ),
    ("n03.bars", "How many bars each section lasts."),
    (
        "n03.vary",
        "The share of notes moved one scale step each time a section repeats (seeded).",
    ),
    (
        "n04",
        "Builds up into the riff's end: the notes repeat ever faster and louder, then drop.",
    ),
    ("n04.bars", "How many bars before the drop build up."),
    (
        "n04.every",
        "Every this many bars of the build, each note is struck twice as often.",
    ),
    (
        "n04.rise",
        "How far the velocity rises toward 127 along the build.",
    ),
    (
        "n04.drop",
        "What the riff's last bar becomes: silence, one full chord at the build's peak, or nothing changes.",
    ),
    (
        "n05",
        "Adds a bass line under the riff, from its progression.",
    ),
    (
        "n05.pattern",
        "The bass line: roots, root and fifth in turn, a walking line, or an octave pump.",
    ),
    ("n05.rhythm", "The bass notes' length, as a note value."),
    (
        "n05.octave",
        "The bass's octave (2 = two octaves below middle C).",
    ),
    (
        "n05.at",
        "Where the bass stage runs among the steps: 2 = before the chords … 9 = after the fit.",
    ),
    (
        "n06",
        "Adds a counter-melody under the top voice, moving against it.",
    ),
    (
        "n06.motion",
        "How the counter-melody moves: contrary to the top voice, holding still, or in parallel thirds or sixths.",
    ),
    (
        "n06.allowed",
        "The intervals below the top voice, in semitones, the counter-melody may use.",
    ),
    (
        "n06.offset",
        "Moves the counter-melody this many semitones up or down.",
    ),
    (
        "n07",
        "Fits the riff to a longer span: looped, stretched, or looped with a little change each time.",
    ),
    (
        "n07.fill",
        "Loop repeats the riff; Stretch slows it to fill the span; Loop with Evolution mutates it slightly on every repeat.",
    ),
    (
        "n07.target",
        "The span to fill: the selection's span, the time selection, or a number of bars.",
    ),
    ("n07.bars", "For Bars: how many bars to fill."),
    (
        "n08",
        "Makes several variants with different seeds, side by side.",
    ),
    ("n08.count", "How many variants are made."),
    ("n08.step", "How far apart the variants' seeds are."),
    (
        "n08.layout",
        "Stacked in Subchannels plays them together, one subchannel each; One After Another places them in a row in time.",
    ),
    (
        "n09",
        "Accents the riff by its place in the bar: each grid step has its own velocity.",
    ),
    (
        "n09.accents",
        "The velocity of each step in turn, from the bar line; the list repeats.",
    ),
    ("n09.steps", "How many steps each bar is divided into."),
    (
        "n10",
        "Shifts each bar a little later than the one before, displacing the riff against the beat.",
    ),
    (
        "n10.shift",
        "How many riff steps each bar moves on from the previous bar.",
    ),
    (
        "n10.wrap",
        "Inside the Bar wraps shifted notes back to the bar's start; Free lets them move into the next bar.",
    ),
    ("n11", "Ends the riff with a final gesture."),
    (
        "n11.kind",
        "Cadence Chord ends on the tonic triad; Fill Run runs onto the tonic; Ritardando slows the last beats down.",
    ),
    ("n11.beats", "How many beats the ending lasts."),
    (
        "n11.run",
        "For Fill Run: the run goes down onto the tonic from above, or up from below.",
    ),
];

fn riff_spec() -> ToolSpec {
    let mut p = vec![
        choice(
            "source",
            "Start from",
            &["The selected notes", "A generated note progression"],
            1,
        ),
        int("bars", "Length (bars)", 1, 64, 4),
        choice("key", "Key", &KEY_NAMES, 0),
        choice("scale", "Scale", scale_names(), 1),
        list(
            "s1.degrees",
            "1 · Progression degrees (1-based, one per bar)",
            1,
            7,
            64,
            &[1, 6, 4, 5],
        ),
        flag("s2.on", "2 · Chords", true),
        choice(
            "s2.chord",
            "Chord type",
            &[
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
            ],
            0,
        ),
        flag("s3.on", "3 · Arpeggiation", true),
        choice(
            "s3.pattern",
            "Arpeggio pattern",
            &[
                "Up", "Down", "Up-Down", "Down-Up", "Converge", "Diverge", "Thumb", "Pinky",
                "Random",
            ],
            2,
        ),
        choice("s3.step", "Arpeggio step", NOTE_VALUES, 4),
        int("s3.range", "Arpeggio range (octaves)", 1, 4, 2),
        choice(
            "s4.mirror",
            "4 · Mirroring",
            &["Off", "Flip horizontally", "Flip vertically"],
            0,
        ),
        float(
            "s5.vel",
            "5 · Levels: velocity randomization",
            0.0,
            100.0,
            20.0,
            "%",
        ),
        choice(
            "s6.style",
            "6 · Articulation",
            &["Off", "Legato", "Portato", "Staccato"],
            2,
        ),
        flag("s7.on", "7 · Groove", false),
        choice("s7.grid", "Groove grid", NOTE_VALUES, 4),
        float("s7.strength", "Groove strength", 0.0, 100.0, 60.0, "%"),
        flag("s8.on", "8 · Fit", true),
        pitch("s8.lo", "Fit range low", 48),
        pitch("s8.hi", "Fit range high", 84),
        int("upto", "Preview up to step", 1, 8, 8),
        seed("seed"),
        adv_switch(1, "User-reorderable pipeline"),
        list(
            "a1.order",
            "Stage order (2–8)",
            2,
            8,
            7,
            &[2, 4, 3, 5, 6, 7, 8],
        )
        .adv(1),
        adv_switch(2, "Branching variant merged"),
        choice("a2.merge", "Merge", &["Union", "Alternate bars"], 1).adv(2),
        adv_switch(3, "Parameter morph across the riff"),
        choice("a3.step", "Arpeggio step at the end", NOTE_VALUES, 5).adv(3),
        adv_switch(4, "Evolution generations"),
        int("a4.gens", "Generations", 1, 16, 3).adv(4),
        float("a4.rate", "Mutation rate", 0.0, 100.0, 15.0, "%").adv(4),
        adv_switch(5, "Motif preservation lock"),
        int("a5.bars", "Motif length (bars)", 1, 8, 1).adv(5),
        adv_switch(6, "Density target"),
        int("a6.per_bar", "Notes per bar (at most)", 1, 256, 8).adv(6),
        adv_switch(7, "Subchannel orchestration"),
        list(
            "a7.splits",
            "Pitch splits (ascending)",
            0,
            127,
            16,
            &[60, 72],
        )
        .keyed()
        .adv(7),
        adv_switch(8, "Harmonic constraint"),
        text("a8.chords", "Chord per bar", 200, "Am F C G").adv(8),
        adv_switch(9, "Rhythm-only mutation"),
        float("a9.rate", "Notes moved", 0.0, 100.0, 30.0, "%").adv(9),
        adv_switch(10, "Macro stage (a tool preset as the last stage)"),
        text(
            "a10.macro",
            "tool id, then key=value …",
            400,
            "humanize tick.min=-15 tick.max=15",
        )
        .adv(10),
        // v0.9 (V09-S35): functions declared by id.
        flag("n01", "Call and response", false),
        int("n01.bars", "Phrase length (bars)", 1, 16, 1),
        choice("n01.answer", "Answer", RIFF_ANSWERS, 0),
        int("n01.interval", "Answer interval (semitones)", -24, 24, 5),
        int("n01.shift", "Answer displacement (steps)", -16, 16, 2),
        flag("n02", "Register layers", false),
        int("n02.layers", "Layers", 1, 8, 2),
        list("n02.octaves", "Octave per layer", -4, 4, 8, &[-1, 0]),
        list(
            "n02.stages",
            "Stages per layer (sum of 1 chords, 2 arpeggio, 4 mirror, 8 levels, 16 articulation, 32 groove, 64 fit, 128 bass)",
            0,
            255,
            8,
            &[1, 127],
        ),
        flag("n03", "Section form", false),
        list("n03.form", "Form (sections A – H)", 0, 7, 32, &[0, 0, 1, 0]),
        int("n03.bars", "Section length (bars)", 1, 16, 4),
        float("n03.vary", "Variation per repeat", 0.0, 100.0, 25.0, "%"),
        flag("n04", "Build-up", false),
        int("n04.bars", "Build bars", 1, 64, 4),
        int("n04.every", "Step halves every (bars)", 1, 16, 1),
        float("n04.rise", "Velocity rise", 0.0, 100.0, 50.0, "%"),
        choice("n04.drop", "Drop", RIFF_DROPS, 0),
        flag("n05", "Bass stage", false),
        choice("n05.pattern", "Bass pattern", RIFF_BASS, 1),
        choice("n05.rhythm", "Bass rhythm", NOTE_VALUES, 3),
        int("n05.octave", "Bass octave", 0, 4, 2),
        int("n05.at", "Bass stage position (2 first – 9 last)", 2, 9, 9),
        flag("n06", "Counter-melody", false),
        choice("n06.motion", "Counter motion", RIFF_MOTIONS, 0),
        list(
            "n06.allowed",
            "Allowed intervals below the top voice (semitones)",
            0,
            24,
            25,
            &[3, 4, 7, 8, 9, 12],
        ),
        int(
            "n06.offset",
            "Counter register offset (semitones)",
            -24,
            24,
            0,
        ),
        flag("n07", "Span fill", false),
        choice("n07.fill", "Fill", RIFF_FILLS, 0),
        choice("n07.target", "Fill target", RIFF_TARGETS, 2),
        int("n07.bars", "Fill length (bars)", 1, 1024, 16),
        flag("n08", "Variant sheet", false),
        int("n08.count", "Variants", 2, 8, 4),
        int("n08.step", "Seed step", 1, 1000, 1),
        choice("n08.layout", "Variant layout", RIFF_LAYOUTS, 0),
        flag("n09", "Accent map", false),
        list(
            "n09.accents",
            "Accent per step (velocity)",
            1,
            127,
            64,
            &[120, 70, 90, 70],
        ),
        int("n09.steps", "Accent steps per bar", 1, 64, 4),
        flag("n10", "Bar displacement", false),
        int("n10.shift", "Shift per bar (steps)", -16, 16, 1),
        choice("n10.wrap", "Displacement wrap", RIFF_WRAP, 0),
        flag("n11", "Ending", false),
        choice("n11.kind", "Ending kind", RIFF_ENDINGS, 0),
        int("n11.beats", "Ending length (beats)", 1, 8, 4),
        choice("n11.run", "Run direction", RIFF_RUN, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::WrapOctave));
    with_help(&mut p, RIFF_HELP);
    ToolSpec {
        id: "riff",
        name: "Riff Machine",
        family: Family::Fl,
        summary: "Builds a riff through a pipeline of the platform's tools: progression, chords, arpeggiation, mirroring, levels, articulation, groove and fit.",
        params: p,
        advanced: vec![
            "User-reorderable pipeline",
            "Branching variant merged",
            "Parameter morph across the riff",
            "Evolution generations",
            "Motif preservation lock",
            "Density target",
            "Subchannel orchestration",
            "Harmonic constraint",
            "Rhythm-only mutation",
            "Macro stage (a tool preset as the last stage)",
        ],
        functions: [
            ("n08", "Variant sheet", 110),
            ("n03", "Section form", 120),
            ("n02", "Register layers", 130),
            ("n05", "Bass stage", 140),
            ("n01", "Call and response", 150),
            ("n07", "Span fill", 160),
            ("n10", "Bar displacement", 170),
            ("n06", "Counter-melody", 180),
            ("n09", "Accent map", 190),
            ("n04", "Build-up", 200),
            ("n11", "Ending", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: false,
        context: false,
    }
}

/// Runs tool `id` with `set` on `notes`.
fn stage(
    id: &str,
    set: &ParamSet,
    notes: Vec<TNote>,
    tracks: &[SourceTrack],
    ctx: &Ctx,
    cap: usize,
) -> Result<Vec<TNote>, TransformError> {
    let def = super::super::tool(id)
        .ok_or_else(|| TransformError::Invalid(format!("Unknown tool \"{id}\".")))?;
    let src = Source::from_notes(notes, tracks.to_vec());
    let b = BuildCtx {
        ctx,
        stats: &src.stats,
        tracks,
    };
    let k = def.kernel(set, &b)?;
    let mut out = Output::new(def.boundary(set), cap);
    k.run(&src, &mut out)?;
    out.finish().map(|x| x.0)
}

fn set_of(id: &str, pairs: &[(&str, ParamValue)]) -> ParamSet {
    let def = super::super::tool(id).expect("registered");
    let mut s = ParamSet::defaults(&(def.spec)().params);
    for (k, v) in pairs {
        s.set(k, v.clone());
    }
    s
}

const RIFF_ANSWERS: &[&str] = &["Transposed", "Inverted", "Reversed", "Displaced"];
const RIFF_BASS: &[&str] = &["Roots", "Root and fifth", "Walking", "Octave pump"];
const RIFF_DROPS: &[&str] = &["A silent bar", "A full chord", "None"];
const RIFF_MOTIONS: &[&str] = &["Contrary", "Oblique", "Parallel thirds", "Parallel sixths"];
const RIFF_FILLS: &[&str] = &["Loop", "Stretch", "Loop with evolution"];
const RIFF_TARGETS: &[&str] = &["The selection's span", "The time selection", "Bars"];
const RIFF_LAYOUTS: &[&str] = &["Stacked in subchannels", "One after another"];
const RIFF_WRAP: &[&str] = &["Inside the bar", "Free"];
const RIFF_ENDINGS: &[&str] = &["Cadence chord", "Fill run", "Ritardando"];
const RIFF_RUN: &[&str] = &["Down", "Up"];

/// The bass stage's number in the stage order (after the v0.8 stages 2 – 8).
const BASS: usize = 9;

/// The Riff Machine's v0.9 functions declared by id (TOOL_CATALOG `riff`
/// n01 … n11). `None` = switched off; with every one off the run is the
/// v0.8 one.
#[derive(Clone, Default)]
struct RiffNew {
    /// n01: phrase bars, answer (0 transposed, 1 inverted, 2 reversed, 3
    /// displaced), interval (every answer), displacement steps.
    call: Option<(u64, usize, i64, i64)>,
    /// n02: layers, octave per layer, stage mask per layer (bit s − 2 =
    /// stage s, bit 7 the bass stage).
    layers: Option<(usize, Vec<i64>, Vec<i64>)>,
    /// n03: form (section indices 0 – 7), section bars, variation 0 – 1.
    form: Option<(Vec<i64>, u64, f64)>,
    /// n04: build bars, halves every (bars), velocity rise 0 – 1, drop (0
    /// silent bar, 1 full chord, 2 none).
    build: Option<(u64, u64, f64, usize)>,
    /// n05: pattern (0 roots, 1 root and fifth, 2 walking, 3 octave pump),
    /// rhythm (note value), octave, stage position (2 – 9).
    bass: Option<(usize, usize, i64, usize)>,
    /// n06: motion (0 contrary, 1 oblique, 2 thirds, 3 sixths), allowed
    /// intervals below the top voice, register offset.
    counter: Option<(usize, Vec<i64>, i64)>,
    /// n07: fill (0 loop, 1 stretch, 2 loop with evolution), target (0
    /// selection span, 1 time selection, 2 bars), bars.
    fill: Option<(usize, usize, u64)>,
    /// n08: variants, seed step, one after another (else stacked in
    /// subchannels).
    variants: Option<(u64, u64, bool)>,
    /// n09: accent velocity per step, steps per bar.
    accents: Option<(Vec<i64>, u64)>,
    /// n10: shift per bar (steps), free (else wrap inside the bar).
    displace: Option<(i64, bool)>,
    /// n11: ending (0 cadence chord, 1 fill run, 2 ritardando), beats, up.
    ending: Option<(usize, u64, bool)>,
}

impl RiffNew {
    fn any(&self) -> bool {
        self.call.is_some()
            || self.layers.is_some()
            || self.form.is_some()
            || self.build.is_some()
            || self.bass.is_some()
            || self.counter.is_some()
            || self.fill.is_some()
            || self.variants.is_some()
            || self.accents.is_some()
            || self.displace.is_some()
            || self.ending.is_some()
    }
}

fn riff_new(p: &Params) -> RiffNew {
    let pct = |k: &str| (p.float(k) / 100.0).clamp(0.0, 1.0);
    RiffNew {
        call: p.flag("n01").then(|| {
            (
                p.int("n01.bars").max(1) as u64,
                p.choice("n01.answer"),
                p.int("n01.interval"),
                p.int("n01.shift"),
            )
        }),
        layers: p
            .flag("n02")
            .then(|| {
                (
                    p.int("n02.layers").clamp(1, 8) as usize,
                    p.list("n02.octaves"),
                    p.list("n02.stages"),
                )
            })
            .filter(|l| !l.1.is_empty() && !l.2.is_empty()),
        form: p
            .flag("n03")
            .then(|| {
                (
                    p.list("n03.form"),
                    p.int("n03.bars").max(1) as u64,
                    pct("n03.vary"),
                )
            })
            .filter(|f| !f.0.is_empty()),
        build: p.flag("n04").then(|| {
            (
                p.int("n04.bars").max(1) as u64,
                p.int("n04.every").max(1) as u64,
                pct("n04.rise"),
                p.choice("n04.drop"),
            )
        }),
        bass: p.flag("n05").then(|| {
            (
                p.choice("n05.pattern"),
                p.choice("n05.rhythm"),
                p.int("n05.octave"),
                p.int("n05.at").clamp(2, 9) as usize,
            )
        }),
        counter: p
            .flag("n06")
            .then(|| {
                (
                    p.choice("n06.motion"),
                    p.list("n06.allowed"),
                    p.int("n06.offset"),
                )
            })
            .filter(|c| !c.1.is_empty()),
        fill: p.flag("n07").then(|| {
            (
                p.choice("n07.fill"),
                p.choice("n07.target"),
                p.int("n07.bars").max(1) as u64,
            )
        }),
        variants: p.flag("n08").then(|| {
            (
                p.int("n08.count").clamp(2, 8) as u64,
                p.int("n08.step").max(1) as u64,
                p.choice("n08.layout") == 1,
            )
        }),
        accents: p
            .flag("n09")
            .then(|| (p.list("n09.accents"), p.int("n09.steps").max(1) as u64))
            .filter(|a| !a.0.is_empty()),
        displace: p
            .flag("n10")
            .then(|| (p.int("n10.shift"), p.choice("n10.wrap") == 1)),
        ending: p.flag("n11").then(|| {
            (
                p.choice("n11.kind"),
                p.int("n11.beats").max(1) as u64,
                p.choice("n11.run") == 1,
            )
        }),
    }
}

/// The result cap (the sink's sentence): a pass that grows the riff past
/// `cap` notes stops before allocating further.
fn guard(len: usize, cap: usize) -> Result<(), TransformError> {
    if len > cap {
        return Err(TransformError::TooLarge(format!(
            "The result would have more than {} notes. Reduce the settings or select fewer notes.",
            crate::midi::group_digits(cap as u64)
        )));
    }
    Ok(())
}

/// `notes` moved by `d` ticks (never before tick 0).
fn shifted(mut notes: Vec<TNote>, d: i128) -> Vec<TNote> {
    if d != 0 {
        for n in &mut notes {
            n.start = (i128::from(n.start) + d).clamp(0, i128::from(u64::MAX)) as u64;
        }
    }
    notes
}

/// Notes sounding past `at` (and starting before it) end there.
fn cut_at(notes: &mut [TNote], at: u64) {
    for n in notes {
        if n.start < at && n.end() > at {
            n.dur = at - n.start;
        }
    }
}

#[derive(Clone)]
struct Riff {
    generate: bool,
    bars: u64,
    scale: Scale,
    degrees: Vec<i64>,
    chords: Option<u32>,
    arp: Option<(u32, u32, i64)>,
    mirror: usize,
    vel: f64,
    style: usize,
    groove: Option<(u32, f64)>,
    fit: Option<(i64, i64)>,
    upto: usize,
    seed: u64,
    order: Vec<usize>,
    branch: Option<usize>,
    morph_step: Option<u32>,
    evolve: Option<(u32, f64)>,
    motif: Option<u64>,
    density: Option<usize>,
    splits: Vec<i64>,
    harmony: Vec<Option<super::super::theory::ChordShape>>,
    rhythm: Option<f64>,
    macro_stage: Option<(String, ParamSet)>,
    ctx: Ctx,
    new: RiffNew,
    /// [a4]'s mutation rate (0 – 1, read even with [a4] off): n07's one
    /// evolution step per repeat.
    evolve_rate: f64,
    /// Control Sweeps evaluated over the riff it makes (E0422): each
    /// answer phrase, section, bar, note or top voice at its own place in
    /// the riff's span (set per run, [`Riff::run`]), the build and the
    /// ending where they sit (the riff's end).
    sp: Option<SweptParams>,
}

/// A unit of the riff starting at `t` (what a Time-measured sweep reads).
fn riff_unit(t: u64) -> TNote {
    TNote {
        start: t,
        ..TNote::default()
    }
}

impl Riff {
    /// Int parameter `key` (the value `base` without a sweep of it) at
    /// unit `n`.
    fn sw_int(&self, key: &str, base: i64, n: &TNote) -> i64 {
        match &self.sp {
            Some(sp) if sp.drives(key) => sp.int(key, n, Place::unit(0, 1)),
            _ => base,
        }
    }

    /// A share in % (`base` a fraction 0 – 1) at unit `n`.
    fn sw_pct(&self, key: &str, base: f64, n: &TNote) -> f64 {
        match &self.sp {
            Some(sp) if sp.drives(key) => {
                (sp.float(key, n, Place::unit(0, 1)) / 100.0).clamp(0.0, 1.0)
            }
            _ => base,
        }
    }

    /// The project's highest key (127, or 255 in 256-Key Mode).
    fn top(&self) -> i64 {
        i64::from(self.ctx.keys.max_pitch())
    }

    /// The stage order: the v0.8 one, with n05's bass stage at its position
    /// (2 = first … 9 = last).
    fn stages(&self) -> Vec<usize> {
        let mut order = self.order.clone();
        if let Some((_, _, _, at)) = self.new.bass {
            order.insert((at - 2).min(order.len()), BASS);
        }
        order
    }

    /// The stages on `start`: those of `mask` (bit s − 2 = stage s, bit 7
    /// the bass stage) in the stage order. [`Riff::upto`] counts the v0.8
    /// stages' places in the order, masked or not; the bass stage runs
    /// where it stands unless the order stopped before it.
    #[allow(clippy::too_many_arguments)]
    fn pipeline(
        &self,
        mask: i64,
        start: Vec<TNote>,
        tracks: &[SourceTrack],
        seed: u64,
        arp_step: u32,
        cap: usize,
    ) -> Result<Vec<TNote>, TransformError> {
        let bass = self.new.bass.is_some() && mask & 1 << (BASS - 2) != 0;
        // The progression the bass stage reads (before any stage).
        let progression = if bass { start.clone() } else { Vec::new() };
        let mut cur = start;
        let mut place = 0;
        for st in self.stages() {
            if st == BASS {
                if bass {
                    let line = self.bass_line(&progression, cap.saturating_sub(cur.len()))?;
                    cur.extend(line);
                }
                continue;
            }
            if place + 2 > self.upto {
                break;
            }
            place += 1;
            if mask & 1 << (st - 2) == 0 {
                continue;
            }
            cur = match st {
                2 => match self.chords {
                    Some(c) => stage(
                        "chord_stamp",
                        &set_of("chord_stamp", &[("chord", ParamValue::Choice(c))]),
                        cur,
                        tracks,
                        &self.ctx,
                        cap,
                    )?,
                    None => cur,
                },
                3 => match self.arp {
                    Some((pat, _, range)) => stage(
                        "arpeggiate",
                        &set_of(
                            "arpeggiate",
                            &[
                                ("pattern", ParamValue::Choice(pat)),
                                ("step", ParamValue::Choice(arp_step)),
                                ("range", ParamValue::Int(range)),
                                ("seed", ParamValue::Int(seed as i64)),
                            ],
                        ),
                        cur,
                        tracks,
                        &self.ctx,
                        cap,
                    )?,
                    None => cur,
                },
                4 => mirror(cur, self.mirror),
                5 if self.vel > 0.0 => stage(
                    "randomize",
                    &set_of(
                        "randomize",
                        &[
                            ("mode", ParamValue::Choice(0)),
                            ("vel", ParamValue::Float(self.vel)),
                            ("seed", ParamValue::Int(seed as i64 ^ 0x55)),
                        ],
                    ),
                    cur,
                    tracks,
                    &self.ctx,
                    cap,
                )?,
                6 if self.style > 0 => stage(
                    "articulate",
                    &set_of(
                        "articulate",
                        &[("style", ParamValue::Choice(self.style as u32 - 1))],
                    ),
                    cur,
                    tracks,
                    &self.ctx,
                    cap,
                )?,
                7 => match self.groove {
                    Some((grid, strength)) => stage(
                        "quantize",
                        &set_of(
                            "quantize",
                            &[
                                ("grid", ParamValue::Choice(grid)),
                                ("start", ParamValue::Float(strength)),
                            ],
                        ),
                        cur,
                        tracks,
                        &self.ctx,
                        cap,
                    )?,
                    None => cur,
                },
                8 => match self.fit {
                    Some((lo, hi)) => stage(
                        "limit",
                        &set_of(
                            "limit",
                            &[
                                ("key", ParamValue::Choice(u32::from(self.scale.root))),
                                ("lo", ParamValue::Int(lo)),
                                ("hi", ParamValue::Int(hi)),
                            ],
                        ),
                        cur,
                        tracks,
                        &self.ctx,
                        cap,
                    )?,
                    None => cur,
                },
                _ => cur,
            };
        }
        Ok(cur)
    }

    /// n05: a bass line from the progression's notes: on the rhythm's grid
    /// inside each, the root; root and fifth in turn; walking (root, third,
    /// fifth of the scale in turn, the last step a scale step toward the
    /// next note's root); or root and its octave in turn — in its own octave
    /// (octave 2 = C2 … B2), at the note's velocity.
    fn bass_line(&self, progression: &[TNote], cap: usize) -> Result<Vec<TNote>, TransformError> {
        let Some((pattern, rhythm, octave, _)) = self.new.bass else {
            return Ok(Vec::new());
        };
        let step = note_value(rhythm, 240, &self.ctx).max(1);
        let lattice = Lattice::of_scale(&self.scale);
        let root_of = |n: &TNote| (octave + 1) * 12 + i64::from(n.pitch).rem_euclid(12);
        let mut out = Vec::new();
        for (i, n) in progression.iter().enumerate() {
            let root = root_of(n);
            let next = progression.get(i + 1).map(root_of).unwrap_or(root);
            let k_max = n.dur.max(1).div_ceil(step);
            guard(
                (out.len() as u64)
                    .saturating_add(k_max)
                    .min(usize::MAX as u64) as usize,
                cap,
            )?;
            for k in 0..k_max {
                let p = match pattern {
                    0 => root,
                    1 if k % 2 == 1 => root + 7,
                    1 => root,
                    2 if k + 1 == k_max && k > 0 => {
                        if next >= root {
                            lattice.step(next, -1)
                        } else {
                            lattice.step(next, 1)
                        }
                    }
                    2 => lattice.step(root, 2 * (k as i64 % 3)),
                    _ if k % 2 == 1 => root + 12,
                    _ => root,
                };
                let s = n.start.saturating_add(k.saturating_mul(step));
                let mut b = *n;
                b.start = s;
                b.dur = step.min(n.end().saturating_sub(s)).max(1);
                b.pitch = p.clamp(0, self.top()) as u8;
                out.push(b);
            }
        }
        Ok(out)
    }

    /// The stages' result: one pipeline, or n02's layers — each its own
    /// stages, then moved by its octaves — together.
    fn build(
        &self,
        start: Vec<TNote>,
        tracks: &[SourceTrack],
        seed: u64,
        arp_step: u32,
        cap: usize,
    ) -> Result<Vec<TNote>, TransformError> {
        let Some((count, octaves, masks)) = &self.new.layers else {
            return self.pipeline(0xFF, start, tracks, seed, arp_step, cap);
        };
        // Under a sweep each note keeps the layers its place allows (the
        // most the range allows are built).
        let swept = self.sp.as_ref().filter(|s| s.drives("n02.layers"));
        let count = match swept {
            Some(sp) => sp.int_bounds("n02.layers").1.clamp(1, 8) as usize,
            None => *count,
        };
        let mut all = Vec::new();
        for l in 0..count {
            let mask = masks[l % masks.len()];
            let oct = octaves[l % octaves.len()];
            let mut v = self.pipeline(mask, start.clone(), tracks, seed, arp_step, cap)?;
            if let Some(sp) = swept {
                v.retain(|n| (l as i64) < sp.int("n02.layers", n, Place::unit(0, 1)).clamp(1, 8));
            }
            for n in &mut v {
                n.pitch = (i64::from(n.pitch) + 12 * oct).clamp(0, self.top()) as u8;
            }
            all.extend(v);
            guard(all.len(), cap)?;
        }
        Ok(all)
    }

    /// The riff's notes from `t0` over `bars` bars (generated degrees from
    /// entry `deg_off`; the selection moved to start at `t0`): the
    /// progression, the stages, then [a3] … [a7] — the v0.8 run.
    #[allow(clippy::too_many_arguments)]
    fn body(
        &self,
        src: &Source,
        tracks: &[SourceTrack],
        tpl: TNote,
        t0: u64,
        bars: u64,
        deg_off: usize,
        seed: u64,
        cap: usize,
    ) -> Result<Vec<TNote>, TransformError> {
        let bar = self.ctx.bar();
        // Step 1: the progression of long notes (or the selection).
        let start: Vec<TNote> = if self.generate || src.notes.is_empty() {
            (0..bars)
                .map(|i| {
                    let d = self.degrees[(i as usize + deg_off) % self.degrees.len().max(1)] - 1;
                    let mut n = tpl;
                    n.start = t0.saturating_add(i.saturating_mul(bar));
                    n.dur = bar;
                    n.pitch = (self.scale.pitch_of(d) + 60 - i64::from(self.scale.root % 12))
                        .clamp(0, self.top()) as u8;
                    n.group = 0;
                    n
                })
                .collect()
        } else {
            shifted(
                src.notes.clone(),
                i128::from(t0) - i128::from(src.stats.min_start),
            )
        };
        let arp_step = self.arp.map_or(4, |a| a.1);
        let mut notes = self.build(start.clone(), tracks, seed, arp_step, cap)?;
        if let Some(step_b) = self.morph_step {
            // Crossfade (per note, deterministic) from step A to step B over the riff.
            let b = self.build(start.clone(), tracks, seed, step_b, cap)?;
            let span = (bars * bar).max(1) as f64;
            let w = |n: &TNote| ((n.start.saturating_sub(t0)) as f64 / span).clamp(0.0, 1.0);
            let mut merged: Vec<TNote> = notes
                .into_iter()
                .filter(|n| Rng::new(hash(&[seed, 1, note_key(n)])).unit() >= w(n))
                .collect();
            merged.extend(
                b.into_iter()
                    .filter(|n| Rng::new(hash(&[seed, 2, note_key(n)])).unit() < w(n)),
            );
            notes = merged;
        }
        if let Some(merge) = self.branch {
            let alt = self.build(start, tracks, seed.wrapping_add(1), arp_step, cap)?;
            notes = match merge {
                0 => {
                    notes.extend(alt);
                    notes
                }
                _ => {
                    let bar_of = |n: &TNote| n.start.saturating_sub(t0) / bar;
                    let mut v: Vec<TNote> =
                        notes.into_iter().filter(|n| bar_of(n) % 2 == 0).collect();
                    v.extend(alt.into_iter().filter(|n| bar_of(n) % 2 == 1));
                    v
                }
            };
        }
        if let Some((gens, rate)) = self.evolve {
            for g in 0..gens {
                for n in &mut notes {
                    let mut r = Rng::new(hash(&[seed, u64::from(g), note_key(n), salt::PITCH]));
                    if r.chance(rate / 100.0) {
                        let (d, _) = self
                            .scale
                            .degree_of(self.scale.snap(i64::from(n.pitch), SnapMode::Nearest));
                        let step = if r.chance(0.5) { 1 } else { -1 };
                        n.pitch = self.scale.pitch_of(d + step).clamp(0, self.top()) as u8;
                    }
                }
            }
        }
        if let Some(rate) = self.rhythm {
            let grid = self.ctx.snap.max(1);
            for n in &mut notes {
                let mut r = Rng::new(hash(&[seed, note_key(n), salt::TICK]));
                if r.chance(rate / 100.0) {
                    let delta = if r.chance(0.5) {
                        grid as i128
                    } else {
                        -(grid as i128)
                    };
                    n.start = (i128::from(n.start) + delta).max(0) as u64;
                }
            }
        }
        if !self.harmony.is_empty() {
            for n in &mut notes {
                let b = ((n.start.saturating_sub(t0)) / bar) as usize;
                if let Some(Some(c)) = self.harmony.get(b % self.harmony.len()) {
                    let mask = c.pitch_classes();
                    let ok = |p: i64| mask & (1 << p.rem_euclid(12)) != 0;
                    let mut p = i64::from(n.pitch);
                    let mut k = 0;
                    while !ok(p) && k < 12 {
                        p += if k % 2 == 0 { k + 1 } else { -(k + 1) };
                        k += 1;
                    }
                    n.pitch = p.clamp(0, self.top()) as u8;
                }
            }
        }
        if let Some(len) = self.motif {
            // Bars k·len … repeat bars 0 … len−1 of the riff.
            let motif: Vec<TNote> = notes
                .iter()
                .copied()
                .filter(|n| n.start < t0.saturating_add(len.saturating_mul(bar)))
                .collect();
            let mut v: Vec<TNote> = motif.clone();
            let mut rep = 1u64;
            while rep * len < bars {
                for n in &motif {
                    let mut c = *n;
                    c.start = c
                        .start
                        .saturating_add(rep.saturating_mul(len).saturating_mul(bar));
                    v.push(c);
                }
                rep += 1;
            }
            notes = v;
        }
        if let Some(max) = self.density {
            notes.sort_by_key(|n| (n.start, n.pitch));
            let mut kept = Vec::new();
            let mut per_bar: std::collections::HashMap<u64, Vec<TNote>> = Default::default();
            for n in notes {
                per_bar
                    .entry(n.start.saturating_sub(t0) / bar)
                    .or_default()
                    .push(n);
            }
            let mut keys: Vec<u64> = per_bar.keys().copied().collect();
            keys.sort_unstable();
            for k in keys {
                let mut v = per_bar.remove(&k).unwrap_or_default();
                if v.len() > max {
                    v.sort_by_key(|n| hash(&[seed, note_key(n)]));
                    v.truncate(max);
                }
                kept.extend(v);
            }
            notes = kept;
        }
        if !self.splits.is_empty() {
            for n in &mut notes {
                if let Some(t) = tracks.get(usize::from(n.track))
                    && !t.subs.is_empty()
                {
                    let band = self
                        .splits
                        .iter()
                        .filter(|&&s| i64::from(n.pitch) >= s)
                        .count();
                    let (id, ch) = t.subs[band % t.subs.len()];
                    n.sub = id;
                    n.ch = ch;
                }
            }
        }
        Ok(notes)
    }

    /// The riff step (the arpeggio's note value, a sixteenth without it).
    fn step(&self) -> u64 {
        note_value(self.arp.map_or(4, |a| a.1 as usize), 240, &self.ctx).max(1)
    }

    /// One scale step up or down (seeded) — the evolution mutation of
    /// [a4], n03's variation and n07's evolving loop.
    fn mutate(&self, n: &mut TNote, r: &mut Rng) {
        let (d, _) = self
            .scale
            .degree_of(self.scale.snap(i64::from(n.pitch), SnapMode::Nearest));
        let k = if r.chance(0.5) { 1 } else { -1 };
        n.pitch = self.scale.pitch_of(d + k).clamp(0, self.top()) as u8;
    }

    /// n03: the sections of the form, section `j` at `t0 + j·N` bars.
    /// Section `s` (A = 0 … H = 7) reads the degree list from entry `s·N`
    /// when the list is longer than a section, else from entry `s`, with
    /// its own seed; the selection is moved to the section and cut to it.
    /// Every repeat of a section after its first moves a share of its notes
    /// by a scale step (seeded by the section, the repeat and the note).
    #[allow(clippy::too_many_arguments)]
    fn sections(
        &self,
        src: &Source,
        tracks: &[SourceTrack],
        tpl: TNote,
        t0: u64,
        seed: u64,
        cap: usize,
        (form, n, vary): (&[i64], u64, f64),
    ) -> Result<Vec<TNote>, TransformError> {
        let bar = self.ctx.bar().max(1);
        let generated = self.generate || src.notes.is_empty();
        let len = self.degrees.len().max(1);
        let salt = salt_of("n03");
        let mut all = Vec::new();
        let mut seen = [0u64; 8];
        for (j, &s) in form.iter().enumerate() {
            let s = s.clamp(0, 7) as u64;
            let at = t0.saturating_add((j as u64).saturating_mul(n).saturating_mul(bar));
            let deg_off = if len as u64 > n {
                (s * n) as usize % len
            } else {
                s as usize % len
            };
            let mut v = self.body(
                src,
                tracks,
                tpl,
                at,
                n,
                deg_off,
                region_seed(seed, salt, s),
                cap,
            )?;
            if !generated {
                let end = at.saturating_add(n.saturating_mul(bar));
                v.retain(|x| x.start < end);
            }
            let rep = &mut seen[s as usize];
            // The section's variation (its own at its start under a sweep).
            let vary = self.sw_pct("n03.vary", vary, &riff_unit(at));
            if *rep > 0 {
                for x in &mut v {
                    let mut r = stream(seed, salt, &[s, *rep, note_key(x)]);
                    if r.chance(vary) {
                        self.mutate(x, &mut r);
                    }
                }
            }
            *rep += 1;
            all.extend(v);
            guard(all.len(), cap)?;
        }
        Ok(all)
    }

    /// One riff (variant seed `seed`) from `t0`: the body (or n03's
    /// sections), then n01, n07, n10, n06, n09, n04, n11 and [a10]. Returns
    /// the notes and where the riff ends.
    #[allow(clippy::too_many_arguments)]
    fn riff(
        &self,
        src: &Source,
        tracks: &[SourceTrack],
        tpl: TNote,
        t0: u64,
        seed: u64,
        cap: usize,
    ) -> Result<(Vec<TNote>, u64), TransformError> {
        let bar = self.ctx.bar().max(1);
        let generated = self.generate || src.notes.is_empty();
        let new = &self.new;
        let (mut notes, bars) = match &new.form {
            Some((form, n, vary)) => (
                self.sections(src, tracks, tpl, t0, seed, cap, (form, *n, *vary))?,
                n.saturating_mul(form.len() as u64),
            ),
            None => {
                let bars = if generated {
                    self.bars
                } else {
                    src.stats.span().div_ceil(bar).max(1)
                };
                (
                    self.body(src, tracks, tpl, t0, self.bars, 0, seed, cap)?,
                    bars,
                )
            }
        };
        let mut end = t0.saturating_add(bars.saturating_mul(bar));
        let step = self.step();
        let top = self.top();
        // n01: phrases of N bars from t0 — the even ones play (the calls),
        // each odd one is the answer to the phrase before (when it starts
        // before the riff's end): transposed, inverted around the call's
        // first note, reversed in time or displaced by steps inside the
        // phrase — every answer also moved by the interval.
        if let Some((pb, answer, interval, shift)) = new.call {
            let plen = pb.saturating_mul(bar).max(1);
            let phrase = |n: &TNote| n.start.saturating_sub(t0) / plen;
            let calls: Vec<TNote> = notes
                .iter()
                .copied()
                .filter(|n| phrase(n) % 2 == 0)
                .collect();
            let mut axis: std::collections::HashMap<u64, (u64, u8)> = Default::default();
            for c in &calls {
                let e = axis.entry(phrase(c)).or_insert((c.start, c.pitch));
                *e = (*e).min((c.start, c.pitch));
            }
            let mut v = calls.clone();
            for c in &calls {
                let p = phrase(c);
                let ps = t0.saturating_add(p.saturating_mul(plen));
                if ps.saturating_add(plen) >= end {
                    continue;
                }
                // The answer's interval and displacement (at its phrase
                // under a sweep).
                let at = riff_unit(ps.saturating_add(plen));
                let interval = self.sw_int("n01.interval", interval, &at);
                let shift = self.sw_int("n01.shift", shift, &at);
                let mut a = *c;
                let mut pitch = i64::from(c.pitch);
                match answer {
                    1 => pitch = 2 * i64::from(axis[&p].1) - pitch,
                    2 => {
                        let off = c.end().saturating_sub(ps).min(plen);
                        a.start = ps + (plen - off);
                    }
                    3 => {
                        let off = c.start.saturating_sub(ps) as i128;
                        let d = i128::from(shift) * i128::from(step);
                        a.start = ps + (off + d).rem_euclid(i128::from(plen)) as u64;
                    }
                    _ => {}
                }
                a.pitch = (pitch + interval).clamp(0, top) as u8;
                a.start = a.start.saturating_add(plen);
                v.push(a);
            }
            notes = v;
        }
        // n07: the riff fitted to the target: looped (notes cut at the
        // target's end), stretched, or looped with one evolution step per
        // repeat; the time selection also moves it to its start.
        if let Some((fill, target, nbars)) = new.fill {
            let len = end.saturating_sub(t0).max(1);
            let (at, want) = match target {
                0 if !src.notes.is_empty() => (t0, src.stats.span().max(1)),
                1 => self
                    .ctx
                    .time_range
                    .map_or((t0, len), |(a, b)| (a, b.saturating_sub(a).max(1))),
                2 => (t0, nbars.saturating_mul(bar).max(1)),
                _ => (t0, len),
            };
            let limit = t0.saturating_add(want);
            notes = match fill {
                1 => notes
                    .into_iter()
                    .map(|mut n| {
                        let scale =
                            |x: u64| (u128::from(x) * u128::from(want) / u128::from(len)) as u64;
                        n.start = t0.saturating_add(scale(n.start.saturating_sub(t0)));
                        n.dur = scale(n.dur).max(1);
                        n
                    })
                    .collect(),
                _ => {
                    let salt = salt_of("n07");
                    let mut v = Vec::new();
                    let mut cur = notes;
                    let mut k = 0u64;
                    while k.saturating_mul(len) < want && !cur.is_empty() {
                        let off = k * len;
                        for n in &cur {
                            let mut c = *n;
                            c.start = c.start.saturating_add(off);
                            if c.start >= limit {
                                continue;
                            }
                            c.dur = c.dur.min(limit - c.start).max(1);
                            v.push(c);
                        }
                        guard(v.len(), cap)?;
                        k += 1;
                        if fill == 2 {
                            for n in &mut cur {
                                let mut r = stream(seed, salt, &[k, note_key(n)]);
                                if r.chance(self.evolve_rate) {
                                    self.mutate(n, &mut r);
                                }
                            }
                        }
                    }
                    v
                }
            };
            end = limit;
            if at != t0 {
                let d = i128::from(at) - i128::from(t0);
                notes = shifted(notes, d);
                end = (i128::from(end) + d).max(0) as u64;
            }
        }
        // n10: bar k of the riff (from t0) k·shift steps later, wrapped
        // inside its bar or freely.
        if let Some((shift, free)) = new.displace {
            for n in &mut notes {
                let b = n.start.saturating_sub(t0) / bar;
                if b == 0 {
                    continue;
                }
                // The bar's shift (at its start under a sweep).
                let shift = self.sw_int("n10.shift", shift, &riff_unit(t0 + b * bar));
                let d = i128::from(shift) * i128::from(b) * i128::from(step);
                n.start = if free {
                    (i128::from(n.start) + d).max(0) as u64
                } else {
                    let bs = t0 + b * bar;
                    let off = i128::from(n.start - bs);
                    bs + (off + d).rem_euclid(i128::from(bar)) as u64
                };
            }
        }
        // n06: one note against the top voice of every onset, below it at
        // an allowed interval — contrary (moving the other way by the same
        // distance, on the scale), oblique (holding), parallel thirds or
        // sixths (scale steps) — the nearest allowed interval when the
        // motion's pitch is not one; then the register offset.
        if let Some((motion, allowed, offset)) = &new.counter {
            let lattice = Lattice::of_scale(&self.scale);
            let mut tops: Vec<TNote> = notes.clone();
            tops.sort_by_key(|n| (n.start, std::cmp::Reverse(n.pitch)));
            tops.dedup_by_key(|n| n.start);
            let fix = |top: i64, near: i64| {
                allowed
                    .iter()
                    .map(|a| top - a)
                    .min_by_key(|&c| ((c - near).abs(), c))
                    .unwrap_or(top)
            };
            let mut prev: Option<(i64, i64)> = None;
            // The register offset by the top voice (its pitch in the riff's
            // range, its place in its span) under a sweep.
            let osp = self
                .sp
                .as_ref()
                .filter(|s| s.drives("n06.offset"))
                .map(|s| {
                    let f = Frame::of_notes(&notes);
                    s.in_frame(Frame {
                        min_start: t0,
                        max_start: end.saturating_sub(1).max(t0),
                        ..f
                    })
                });
            for t in &tops {
                let offset = &match &osp {
                    Some(sp) => sp.int("n06.offset", t, Place::unit(0, 1)),
                    None => *offset,
                };
                let tp = i64::from(t.pitch);
                let c = match (motion, prev) {
                    (2, _) => lattice.step(tp, -2),
                    (3, _) => lattice.step(tp, -5),
                    (_, None) => tp - allowed[0],
                    (0, Some((pt, pc))) => lattice.nearest(pc - (tp - pt)),
                    (_, Some((_, pc))) => pc,
                };
                let c = if allowed.contains(&(tp - c)) {
                    c
                } else {
                    fix(tp, c)
                };
                prev = Some((tp, c));
                let mut x = *t;
                x.pitch = (c + offset).clamp(0, top) as u8;
                notes.push(x);
            }
            guard(notes.len(), cap)?;
        }
        // n09: velocity accents per step of the meter's bar (steps per
        // bar, the list cycled): a note at the riff's base velocity (the
        // first selected note's, else 100) gets the step's accent, others
        // in proportion (the levels stage's variation kept).
        if let Some((accents, steps)) = &new.accents {
            let base = i64::from(tpl.vel.max(1));
            for n in &mut notes {
                let m = metric_position(&self.ctx.meter, n.start);
                let steps = self.sw_int("n09.steps", *steps as i64, n).max(1) as u64;
                let k = (u128::from(m.offset(n.start)) * u128::from(steps)
                    / u128::from(m.bar_len.max(1))) as usize;
                let a = accents[k % accents.len()];
                n.vel = ((a * i64::from(n.vel) + base / 2) / base).clamp(1, 127) as u8;
            }
        }
        // n04: the last N bars before the drop build up — every note
        // struck 2^g times evenly over its length in the g-th group of M
        // bars (at least a tick each), velocity raised toward 127 by the
        // rise along the build — into the drop: the riff's last bar silent
        // or one chord of its pitches at the build's peak velocity (what
        // sounds into it is cut there), or no drop.
        if let Some((nb, every, rise, drop)) = new.build {
            // The build's length where it sits (the riff's end) under a
            // sweep.
            let nb = self
                .sw_int("n04.bars", nb as i64, &riff_unit(end.saturating_sub(1)))
                .max(1) as u64;
            let has_drop = drop < 2;
            let be = if has_drop {
                end.saturating_sub(bar).max(t0)
            } else {
                end
            };
            let bs = be.saturating_sub(nb.saturating_mul(bar)).max(t0);
            let blen = be.saturating_sub(bs).max(1);
            let glen = every.saturating_mul(bar).max(1);
            let mut v = Vec::with_capacity(notes.len());
            let mut peak = 0u8;
            for n in notes {
                if n.start < bs || n.start >= be {
                    v.push(n);
                    continue;
                }
                let g = (n.start - bs) / glen;
                let dur = n.dur.max(1);
                let pieces = if g >= 63 { dur } else { (1u64 << g).min(dur) };
                guard(
                    (v.len() as u64)
                        .saturating_add(pieces)
                        .min(usize::MAX as u64) as usize,
                    cap,
                )?;
                let x = (n.start - bs) as f64 / blen as f64;
                // The note's rise (by its velocity under a sweep).
                let rise = self.sw_pct("n04.rise", rise, &n);
                let vel = (f64::from(n.vel) + (127.0 - f64::from(n.vel)) * rise * x)
                    .round()
                    .clamp(1.0, 127.0) as u8;
                peak = peak.max(vel);
                for k in 0..pieces {
                    let a = n.start + (u128::from(dur) * u128::from(k) / u128::from(pieces)) as u64;
                    let b =
                        n.start + (u128::from(dur) * u128::from(k + 1) / u128::from(pieces)) as u64;
                    let mut c = n;
                    c.start = a;
                    c.dur = (b - a).max(1);
                    c.vel = vel;
                    v.push(c);
                }
                guard(v.len(), cap)?;
            }
            notes = v;
            if has_drop && be < end {
                let in_drop: Vec<TNote> = notes
                    .iter()
                    .copied()
                    .filter(|n| n.start >= be && n.start < end)
                    .collect();
                notes.retain(|n| n.start < be || n.start >= end);
                cut_at(&mut notes, be);
                if drop == 1
                    && let Some(first) = in_drop.first()
                {
                    let mut pitches: Vec<u8> = in_drop.iter().map(|n| n.pitch).collect();
                    pitches.sort_unstable();
                    pitches.dedup();
                    let vel = in_drop.iter().map(|n| n.vel).fold(peak, u8::max);
                    for p in pitches {
                        let mut c = *first;
                        c.start = be;
                        c.dur = end - be;
                        c.pitch = p;
                        c.vel = vel;
                        notes.push(c);
                    }
                }
            }
        }
        // n11: the last beats end the riff — the scale's tonic triad
        // nearest the last note before them, a run of riff steps onto that
        // tonic (from above going down, from below going up), or a
        // ritardando (offsets u in the last beats move to u + u²/2, lengths
        // × (1 + u): half as fast at the end, the riff half the ending
        // longer).
        if let Some((kind, beats, up)) = new.ending {
            // The ending's length where it sits (the riff's end) under a
            // sweep.
            let beats = self
                .sw_int("n11.beats", beats as i64, &riff_unit(end.saturating_sub(1)))
                .max(1) as u64;
            let rs = end
                .saturating_sub(beats.saturating_mul(self.ctx.ppq))
                .max(t0);
            let rlen = end.saturating_sub(rs).max(1);
            let last = notes
                .iter()
                .filter(|n| n.start < rs)
                .max_by_key(|n| (n.start, n.pitch))
                .copied()
                .unwrap_or(tpl);
            let lattice = Lattice::of_scale(&self.scale);
            let near = i64::from(last.pitch);
            let r = i64::from(self.scale.root % 12);
            let tonic = r + 12 * (near - r + 6).div_euclid(12);
            let note = |p: i64, s: u64, d: u64| {
                let mut c = last;
                c.start = s;
                c.dur = d.max(1);
                c.pitch = p.clamp(0, top) as u8;
                c
            };
            match kind {
                0 | 1 => {
                    notes.retain(|n| n.start < rs || n.start >= end);
                    cut_at(&mut notes, rs);
                    if kind == 0 {
                        for k in [0, 2, 4] {
                            notes.push(note(lattice.step(tonic, k), rs, rlen));
                        }
                    } else {
                        let count = (rlen / step).max(1);
                        for k in 0..count {
                            let away = (count - 1 - k) as i64;
                            let p = lattice.step(tonic, if up { -away } else { away });
                            let s = rs + k * step;
                            let d = if k + 1 == count { end - s } else { step };
                            notes.push(note(p, s, d));
                        }
                    }
                }
                _ => {
                    for n in &mut notes {
                        if n.start >= rs && n.start < end {
                            let u = (n.start - rs) as f64 / rlen as f64;
                            n.start = rs + ((u + u * u / 2.0) * rlen as f64).round() as u64;
                            n.dur = ((n.dur as f64) * (1.0 + u)).round().max(1.0) as u64;
                        }
                    }
                    end = end.saturating_add(rlen / 2);
                }
            }
        }
        if let Some((id, set)) = &self.macro_stage {
            notes = stage(id, set, notes, tracks, &self.ctx, cap)?;
        }
        Ok((notes, end))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let tracks = src.tracks.clone();
        let t0 = if src.stats.is_empty() {
            self.ctx.cursor
        } else {
            src.stats.min_start
        };
        let tpl = src.notes.first().copied().unwrap_or_else(|| {
            let (sub, ch) = tracks
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
        let cap = out.limit();
        // Under Control Sweeps (E0422) the sweeps are measured in the
        // riff's span.
        let framed;
        let this = match &self.sp {
            Some(sp) => {
                let bar = self.ctx.bar().max(1);
                let generated = self.generate || src.notes.is_empty();
                let bars = match &self.new.form {
                    Some((form, n, _)) => n.saturating_mul(form.len() as u64),
                    None if generated => self.bars,
                    None => src.stats.span().div_ceil(bar).max(1),
                };
                let end = t0.saturating_add(bars.saturating_mul(bar));
                framed = Riff {
                    sp: Some(sp.in_frame(Frame {
                        min_start: t0,
                        max_start: end.saturating_sub(1).max(t0),
                        min_pitch: 0,
                        max_pitch: self.top().clamp(0, 255) as u8,
                    })),
                    ..self.clone()
                };
                &framed
            }
            None => self,
        };
        // n08: variants with consecutive seeds — each the riff its seed
        // makes from t0 (so a kept seed gives it again) — stacked in the
        // track's subchannels (variant j in subchannel j, cycled) or moved
        // one after another (each where the one before ends).
        let (count, seed_step, after) = self.new.variants.unwrap_or((1, 1, false));
        let mut at = t0;
        let mut notes: Vec<TNote> = Vec::new();
        // V09-0143: each note's variant, the driver dimension.
        let mut variant: Vec<u32> = Vec::new();
        for j in 0..count {
            let seed = self.seed.wrapping_add(j.wrapping_mul(seed_step));
            let (mut v, end) = this.riff(src, &tracks, tpl, t0, seed, cap)?;
            if count > 1 {
                if after {
                    v = shifted(v, i128::from(at) - i128::from(t0));
                    at = at.saturating_add(end.saturating_sub(t0).max(1));
                } else {
                    for n in &mut v {
                        if let Some(t) = tracks.get(usize::from(n.track))
                            && !t.subs.is_empty()
                        {
                            let (id, ch) = t.subs[j as usize % t.subs.len()];
                            n.sub = id;
                            n.ch = ch;
                        }
                    }
                }
            }
            variant.resize(
                variant.len() + v.len(),
                u32::try_from(j).unwrap_or(u32::MAX),
            );
            notes.extend(v);
            guard(notes.len(), cap)?;
        }
        for (n, &j) in notes.iter().zip(&variant) {
            out.set_dim(j);
            out.emit(
                i128::from(n.start),
                i128::from(n.dur),
                i64::from(n.pitch),
                i64::from(n.vel),
                n,
            );
            out.check()?;
        }
        // A generated riff never deletes the selection: it is kept.
        if self.generate {
            out.set_dim(0);
            for n in &src.notes {
                out.push(*n);
            }
        }
        Ok(())
    }
}

fn mirror(mut v: Vec<TNote>, mode: usize) -> Vec<TNote> {
    if v.is_empty() || mode == 0 {
        return v;
    }
    let s = v.iter().map(|n| n.start).min().unwrap_or(0);
    let e = v.iter().map(|n| n.end()).max().unwrap_or(0);
    let lo = v.iter().map(|n| n.pitch).min().unwrap_or(0);
    let hi = v.iter().map(|n| n.pitch).max().unwrap_or(0);
    for n in &mut v {
        if mode == 1 {
            n.start = s + (e - n.end());
        } else {
            n.pitch = lo + (hi - n.pitch);
        }
    }
    v
}

/// "tool key=value key=value" → (tool id, values) validated against the tool.
pub fn parse_macro(text: &str) -> Result<(String, ParamSet), TransformError> {
    let mut parts = text.split_whitespace();
    let id = parts.next().unwrap_or("").to_string();
    let def = super::super::tool(&id).ok_or_else(|| {
        TransformError::Invalid(format!("The macro names an unknown tool \"{id}\"."))
    })?;
    if id == "riff" {
        return Err(TransformError::Invalid(
            "A riff macro cannot run the Riff Machine itself.".into(),
        ));
    }
    let spec = (def.spec)();
    let mut set = ParamSet::defaults(&spec.params);
    for kv in parts {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| TransformError::Invalid(format!("\"{kv}\" is not key=value.")))?;
        let ps = spec.params.iter().find(|p| p.key == k).ok_or_else(|| {
            TransformError::Invalid(format!("The tool \"{id}\" has no setting \"{k}\"."))
        })?;
        let val = ParamValue::parse(&ps.kind, v)
            .ok_or_else(|| TransformError::Invalid(format!("\"{v}\" is not valid for \"{k}\".")))?;
        set.set(k, val);
    }
    Ok((id, set))
}

/// The Riff Machine's settings from `p`.
fn riff_of(p: &Params, b: &BuildCtx) -> Result<Riff, TransformError> {
    let scale = Scale::new(p.choice("key"), p.choice("scale"));
    let mut order: Vec<usize> = if p.adv(1) {
        p.list("a1.order").iter().map(|&x| x as usize).collect()
    } else {
        vec![2, 3, 4, 5, 6, 7, 8]
    };
    order.retain(|s| (2..=8).contains(s));
    let harmony = if p.adv(8) {
        p.text("a8.chords")
            .split_whitespace()
            .map(|t| parse_roman(t, &scale).or_else(|| parse_chord(t)))
            .collect()
    } else {
        Vec::new()
    };
    let r = Riff {
        generate: p.choice("source") == 1,
        bars: p.int("bars") as u64,
        scale,
        degrees: {
            let d = p.list("s1.degrees");
            if d.is_empty() { vec![1] } else { d }
        },
        chords: p.flag("s2.on").then(|| p.choice("s2.chord") as u32),
        arp: p.flag("s3.on").then(|| {
            (
                p.choice("s3.pattern") as u32,
                p.choice("s3.step") as u32,
                p.int("s3.range"),
            )
        }),
        mirror: p.choice("s4.mirror"),
        vel: p.float("s5.vel"),
        style: p.choice("s6.style"),
        groove: p
            .flag("s7.on")
            .then(|| (p.choice("s7.grid") as u32, p.float("s7.strength"))),
        fit: p.flag("s8.on").then(|| (p.int("s8.lo"), p.int("s8.hi"))),
        upto: p.int("upto") as usize,
        seed: p.seed("seed"),
        order,
        branch: p.adv(2).then(|| p.choice("a2.merge")),
        morph_step: p.adv(3).then(|| p.choice("a3.step") as u32),
        evolve: p
            .adv(4)
            .then(|| (p.int("a4.gens") as u32, p.float("a4.rate"))),
        motif: p.adv(5).then(|| p.int("a5.bars") as u64),
        density: p.adv(6).then(|| p.int("a6.per_bar") as usize),
        splits: if p.adv(7) {
            p.list("a7.splits")
        } else {
            Vec::new()
        },
        harmony,
        rhythm: p.adv(9).then(|| p.float("a9.rate")),
        macro_stage: if p.adv(10) {
            Some(parse_macro(&p.text("a10.macro"))?)
        } else {
            None
        },
        ctx: b.ctx.clone(),
        new: riff_new(p),
        evolve_rate: (p.float("a4.rate") / 100.0).clamp(0.0, 1.0),
        sp: None,
    };
    Ok(r)
}

fn riff_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let mut r = riff_of(p, b)?;
    let mut est = riff_estimate(&r, b);
    if r.new.any() {
        // Under Control Sweeps (E0422) the riff evaluates them over its
        // own units; the bound is the largest over the swept ranges'
        // corners. With every function off they drive nothing: v0.8.
        r.sp = SweptParams::of(p, b);
        if let Some(rb) = Rebuild::of(p, b) {
            for c in rb.corners(b.ctx, riff_of) {
                est = est.max(riff_estimate(&c?, b));
            }
        }
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| r.run(src, out),
        growth: Growth::Generate(est),
        estimate: Some(est),
    })
}

/// The preflight bound of the riff's notes. With every v0.9 function off
/// it is the v0.8 bound (notes per bar after chords × arpeggio density ×
/// branches); each function on multiplies or adds its own share:
/// n02 × layers, n05 the bass notes per bar (× the expansion of the chord
/// and arpeggio stages after it), n03 the form's bars (the selection once
/// per section), n01 × 2, n07 × the loops, n06 × 2, n04 the pieces of
/// every group of the build (at most a tick each), n11 the run, n08 ×
/// variants.
fn riff_estimate(r: &Riff, b: &BuildCtx) -> u64 {
    let bar = b.ctx.bar().max(1);
    let generated = r.generate || b.stats.is_empty();
    let bars = if generated {
        r.bars
    } else {
        (b.stats.span() / bar).max(1)
    };
    let step = note_value(r.arp.map_or(4, |a| a.1 as usize), 240, b.ctx).min(note_value(
        r.morph_step.map_or(4, |s| s as usize),
        240,
        b.ctx,
    ));
    let per_bar = (bar / step.max(1) + 1) * 8 + b.stats.count as u64;
    let branch = if r.branch.is_some() { 2 } else { 1 };
    let v08 = bars
        .saturating_mul(per_bar)
        .saturating_mul(branch)
        .saturating_mul(2);
    let new = &r.new;
    if !new.any() {
        return v08;
    }
    // One pipeline's notes per bar: the v0.8 share (a generated riff's
    // without the kept selection, added once at the end) plus the bass
    // stage's (per progression note on its grid; expanded by the chord
    // stamp and the arpeggio when they come after it).
    let mut pipe = if generated {
        per_bar - b.stats.count as u64
    } else {
        per_bar
    };
    if let Some((_, rhythm, _, _)) = new.bass {
        let rstep = note_value(rhythm, 240, b.ctx).max(1);
        let order = r.stages();
        let at = order.iter().position(|&s| s == BASS).unwrap_or(order.len());
        let after = |s: usize| order.iter().skip(at).any(|&x| x == s);
        let mut per_note = 1u64;
        if after(2) && r.chords.is_some() {
            per_note = per_note.saturating_mul(8);
        }
        if after(3) && r.arp.is_some() {
            per_note = per_note.saturating_mul(rstep / step.max(1) + 2);
        }
        let notes = if generated {
            bar / rstep + 1
        } else {
            // Over the whole selection, spread over its bars.
            ((b.stats.sum_dur / u128::from(rstep)) as u64)
                .saturating_add(b.stats.count as u64)
                .div_ceil(bars)
                .saturating_add(1)
        };
        pipe = pipe.saturating_add(notes.saturating_mul(per_note));
    }
    if let Some((layers, _, _)) = &new.layers {
        pipe = pipe.saturating_mul(*layers as u64);
    }
    // The riff's bars and its notes before the passes.
    let (riff_bars, mut est) = match &new.form {
        Some((form, n, _)) if generated => {
            let rb = n.saturating_mul(form.len() as u64);
            (rb, rb.saturating_mul(pipe))
        }
        // From a selection each section is the selection's riff cut to
        // its N bars.
        Some((form, n, _)) => (
            n.saturating_mul(form.len() as u64),
            (form.len() as u64).saturating_mul(bars.min(*n).saturating_mul(pipe)),
        ),
        None => (bars, bars.saturating_mul(pipe)),
    };
    est = est.saturating_mul(branch).saturating_mul(2);
    if new.call.is_some() {
        est = est.saturating_mul(2);
    }
    let mut total_bars = riff_bars.max(1);
    let mut stretch = 1u64;
    if let Some((fill, target, nbars)) = new.fill {
        let len = riff_bars.max(1).saturating_mul(bar);
        let want = match target {
            0 if !b.stats.is_empty() => b.stats.span().max(1),
            1 => b
                .ctx
                .time_range
                .map_or(len, |(a, z)| z.saturating_sub(a).max(1)),
            2 => nbars.saturating_mul(bar).max(1),
            _ => len,
        };
        if fill == 1 {
            stretch = want.div_ceil(len).max(1);
        } else {
            est = est.saturating_mul(want.div_ceil(len).max(1));
        }
        total_bars = want.div_ceil(bar).max(1);
    }
    if new.counter.is_some() {
        est = est.saturating_mul(2);
    }
    if let Some((nb, every, _, drop)) = new.build {
        // Notes per bar of the riff so far; group g's notes become at most
        // min(2^g, longest note) pieces each.
        let dens = est.div_ceil(total_bars).saturating_add(1);
        let longest = bar
            .max(if generated { 0 } else { b.stats.max_dur })
            .saturating_mul(stretch);
        let groups = nb.min(total_bars).div_ceil(every.max(1));
        let mut extra = 0u64;
        for g in 0..groups {
            let pieces = if g >= 63 {
                longest
            } else {
                (1u64 << g).min(longest)
            };
            extra = extra.saturating_add(dens.saturating_mul(every).saturating_mul(pieces));
        }
        est = est.saturating_add(extra);
        if drop == 1 {
            est = est.saturating_add(256);
        }
    }
    if let Some((_, beats, _)) = new.ending {
        est = est.saturating_add(beats.saturating_mul(b.ctx.ppq) / step.max(1) + 3);
    }
    if let Some((count, _, _)) = new.variants {
        est = est.saturating_mul(count);
    }
    if r.generate {
        est = est.saturating_add(b.stats.count as u64);
    }
    est.max(v08)
}
