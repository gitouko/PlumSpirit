//! Wave H note editors: Note Properties, Join / Glue and Note Repeat — each a
//! platform recipe with ten advanced functions (docs/v0_7/TOOL_CATALOG.md);
//! all previews are the kernel's own output — and the one-click Mute Notes
//! command kernel (v0.9: the rule-based Advanced Mute was removed).

use super::super::boundary::{self, PitchPolicy};
use super::super::output::{Lane, MAX_EVENTS};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, salt};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::density_window::budget_keep_if_by;
use super::super::shared::metric_position::{bar_bounds, bar_place, metric_position};
use super::super::shared::phrase_detect::Phrases;
use super::super::shared::tempo_time::TempoTime;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::{Ctx, FLAG_MUTED, Growth, Kernel, Output, Source, TNote, TransformError};
use super::{Contextual, NOTE_VALUES, boxed, note_value, per_track, pos, warp};
use crate::editor::expr::{Expr, MAX_EXPR_LEN, valid_name};
use crate::editor::ranges::{self, Across, RangeRow, Target};

fn sub_pos(src: &Source, n: &TNote) -> usize {
    src.track(n.track)
        .and_then(|t| t.sub_index(n.sub))
        .unwrap_or(0)
}

// ============================================================ Note Properties

pub const NOTE_PROPS: ToolDef = ToolDef {
    id: "note_props",
    spec: note_props_spec,
    build: note_props_build,
};

/// Properties addressed by gradient / histogram / random functions.
const PROPS: &[&str] = &["Velocity", "Release", "Length (ticks)", "Pitch offset"];
const COPY_FROM: &[&str] = &["First note", "Last note", "Loudest note", "Longest note"];
const COPY_WHAT: &[&str] = &[
    "Velocity",
    "Velocity and release",
    "Velocity and length",
    "Velocity, release, length, channel and subchannel",
];
const MACROS: &[&str] = &[
    "Staccato (50 % length, +10 velocity)",
    "Accent beats (+20 velocity on beats)",
    "Soften (−20 velocity, release 32)",
    "Legato feel (110 % length, release 80)",
    "Ghost off-beats (velocity 24 off the beat)",
];
const CTL: &[&str] = &[
    "None",
    "Pan (CC 10)",
    "Mod X (CC 1)",
    "Mod Y (CC 74)",
    "Fine pitch (pitch bend)",
];
/// n01: what a value is read from.
const NP_SOURCES: &[&str] = &[
    "Pitch",
    "Velocity",
    "Length",
    "Position in the bar",
    "Subchannel",
];
/// n01 / n03: what a value is written to.
const NP_TARGETS: &[&str] = &["Velocity", "Release", "Length", "Channel"];
/// n02: which notes are accented.
const NP_ACCENTS: &[&str] = &["Downbeats", "Phrase starts", "Leaps", "Chord tops"];
/// n03: when the next value of the list starts.
const NP_ADVANCE: &[&str] = &["Note", "Beat", "Bar", "Chord"];
/// n04 / n07: the properties shaped across the notes.
const NP_PROPS3: &[&str] = &["Velocity", "Release", "Length"];
/// n05: which end stays.
const NP_KEEP: &[&str] = &["Start", "End"];
/// n06: the neighbours averaged.
const NP_PER: &[&str] = &["Track", "Key", "Subchannel"];
const NP_OUTLIER_ACTIONS: &[&str] = &["Clamp to the bound", "Set to the median"];
const NP_ZONE_TARGETS: &[&str] = &["MIDI channel", "Subchannel"];
const NP_CONTROLLERS: &[&str] = &["CC 10 pan", "CC 1 modulation", "CC 74", "CC 11 expression"];
/// n10: the condition's property, its test and what is set.
const NP_IF: &[&str] = &[
    "Pitch",
    "Velocity",
    "Length",
    "Channel",
    "Subchannel",
    "Position in the bar",
];
const NP_TESTS: &[&str] = &["Less than", "At least", "Equal", "Between"];
const NP_THEN: &[&str] = &["Velocity", "Release", "Length", "Channel", "Subchannel"];
/// n11: which follows which, and how.
const NP_SYNC: &[&str] = &["Subchannel → channel", "Channel → subchannel"];
const NP_MAPPING: &[&str] = &["Same index", "Table"];
/// n12 (B1-0075 – B1-0077): what an out-of-range result does, and how a
/// result becomes a whole number.
const NP_X_POLICY: &[&str] = &[
    "Clamp into the range",
    "Wrap around the range",
    "Leave the note unchanged",
    "Stop with an error",
];
const NP_X_ROUND: &[&str] = &["Nearest", "Down", "Up", "Toward zero"];
/// n12: the variables every expression field may read, in this order (see
/// [`NpExpr::vars`]).
pub const NP_X_VARS: [&str; 12] = [
    "t", "g", "k", "v", "rel", "c", "sc", "index", "count", "ppqn", "pos", "rnd",
];
/// n12: the fields (keys after `n12.`) and their labels, in output order:
/// start, gate, key, velocity, channel, subchannel, controller value.
const NP_X_FIELDS: [(&str, &str); 7] = [
    ("n12.t", "Ticks (t)"),
    ("n12.g", "Gates (g)"),
    ("n12.k", "Keys (k)"),
    ("n12.v", "Velocities (v)"),
    ("n12.c", "Channels (c)"),
    ("n12.sc", "Subchannels (sc)"),
    ("n12.cc", "Controller value"),
];
const X_CLAMP: usize = 0;
const X_WRAP: usize = 1;
const X_KEEP: usize = 2;
/// n12: the most user constants.
const X_MAX_CONSTS: usize = 16;
/// n12: the largest tick or gate the expressions handle exactly (2^53 − 1:
/// every integer up to it is exact in an `f64`).
const X_MAX_TICK: i128 = (1 << 53) - 1;

const SRC_PITCH: usize = 0;
const SRC_VELOCITY: usize = 1;
const SRC_LENGTH: usize = 2;
const SRC_BAR: usize = 3;
const ACCENT_DOWNBEATS: usize = 0;
const ACCENT_PHRASES: usize = 1;
const ACCENT_LEAPS: usize = 2;
const ADVANCE_NOTE: usize = 0;
const ADVANCE_BEAT: usize = 1;
const ADVANCE_BAR: usize = 2;
const ADVANCE_CHORD: usize = 3;
const PER_KEY: usize = 1;
const PER_SUB: usize = 2;
const TEST_LESS: usize = 0;
const TEST_AT_LEAST: usize = 1;
const TEST_EQUAL: usize = 2;
/// n10 "then set": the subchannel (positions in the track's list).
const THEN_SUB: usize = 4;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const NOTE_PROPS_HELP: &[(&str, &str)] = &[
    (
        "vel.on",
        "Sets every selected note's velocity to the Velocity below; off keeps each note's own velocity.",
    ),
    (
        "vel",
        "The velocity every note gets while Set Velocity is on (1 = softest, 127 = loudest).",
    ),
    (
        "rel.on",
        "Sets every selected note's release (Note Off) velocity; off keeps each note's own.",
    ),
    (
        "rel",
        "The release velocity written in each note's Note Off (0 – 127; many synthesizers ignore it).",
    ),
    (
        "pitch.on",
        "Moves every selected note to the one key below; off keeps each note's key.",
    ),
    (
        "pitch",
        "The key every note is moved to while Set Pitch is on (60 = middle C).",
    ),
    (
        "ch.on",
        "Sets every selected note's MIDI channel; off keeps each note's channel. The subchannel is not changed.",
    ),
    (
        "ch",
        "The MIDI channel (1 – 16) every note gets while Set MIDI Channel is on.",
    ),
    (
        "sub.on",
        "Gives every selected note one subchannel of its track, chosen by its place in the track's subchannel list.",
    ),
    (
        "sub",
        "Which subchannel the notes get, by its place in their track's list (0 = the first; past the end = the last).",
    ),
    (
        "suboff.on",
        "Shifts each note's subchannel number by the offset below (after Set Subchannel when both are on); MIDI channels stay as they are.",
    ),
    (
        "suboff",
        "How many subchannel numbers each note moves (+ up, − down); a move below the first subchannel stops the edit with a message and changes nothing.",
    ),
    (
        "start.on",
        "Moves every selected note to start at the one tick below.",
    ),
    (
        "start",
        "The tick every note starts at while Set Start is on (0 = the start of the song).",
    ),
    ("dur.on", "Gives every selected note the one length below."),
    (
        "dur",
        "The length every note gets, in ticks (one quarter note = the project's PPQN).",
    ),
    (
        "grp.on",
        "Puts every selected note into the note group below; a group id that does not exist yet is created.",
    ),
    (
        "grp",
        "The note group the notes join (0 = no group: takes them out of their group).",
    ),
    (
        "ctl",
        "Writes one controller event at each note's start, on the note's channel: pan, modulation, CC 74 or fine pitch (pitch bend). None writes nothing.",
    ),
    (
        "ctl.val",
        "The controller value written at each note start: 0 – 127 for the CCs; for Fine Pitch, cents (−100 … +100) at the default ±2 semitone bend range.",
    ),
    (
        "a1",
        "Adds the amounts below to each note's own values instead of replacing them (after the Set fields above).",
    ),
    (
        "a1.vel",
        "Added to each note's velocity; the result stays within 1 – 127.",
    ),
    (
        "a1.rel",
        "Added to each note's release velocity; the result stays within 0 – 127.",
    ),
    (
        "a1.pitch",
        "Moves each note this many semitones (+ up, − down).",
    ),
    (
        "a1.start",
        "Moves each note this many ticks later (+) or earlier (−); Before Tick 0 decides what happens to notes pushed before the song's start.",
    ),
    (
        "a1.dur",
        "Lengthens (+) or shortens (−) each note by this many ticks; Length ≤ 0 decides what happens to notes shortened to nothing.",
    ),
    (
        "a2",
        "Multiplies each note's velocity, release and length by the percentages below (100 % = unchanged).",
    ),
    (
        "a2.vel",
        "Each velocity times this percentage (80 % turns 100 into 80); the result stays within 1 – 127.",
    ),
    ("a2.rel", "Each release velocity times this percentage."),
    (
        "a2.dur",
        "Each length times this percentage (50 % halves it, 200 % doubles it).",
    ),
    (
        "a3",
        "Sets one property along a ramp from the selection's earliest start to its latest.",
    ),
    (
        "a3.prop",
        "What the ramp sets: velocity, release, length in ticks, or a pitch offset added to each note's own key.",
    ),
    ("a3.from", "The value at the selection's earliest start."),
    ("a3.to", "The value at the selection's latest start."),
    (
        "a3.curve",
        "How the value travels from From to To across the selection's time span (a straight line by default).",
    ),
    (
        "a4",
        "Copies values of one model note of the selection onto every selected note.",
    ),
    (
        "a4.from",
        "The model note: the first or last note of the selection, the loudest or the longest (the earlier one on a tie).",
    ),
    (
        "a4.what",
        "Which of the model's values every note takes: velocity only, with release, with length, or also its channel and subchannel (the subchannel only for notes on the model's track).",
    ),
    (
        "a5",
        "Scales every velocity by one factor so the loudest note, or the average, lands on the target; the others keep their proportions.",
    ),
    (
        "a5.mode",
        "Loudest: the loudest note lands on the target; Average: the mean velocity does.",
    ),
    (
        "a5.target",
        "The velocity the loudest note or the average is scaled to (results stay within 1 – 127).",
    ),
    (
        "a6",
        "Spreads one property evenly by rank: the note with the smallest value gets Lowest, the largest gets Highest, the rest step evenly between in their order.",
    ),
    (
        "a6.prop",
        "The property ranked and spread: velocity, release or length in ticks.",
    ),
    ("a6.lo", "The value given to the lowest-ranked note."),
    ("a6.hi", "The value given to the highest-ranked note."),
    (
        "a7",
        "Protects the ticked properties: whatever the other settings do, locked values stay as they were.",
    ),
    ("a7.vel", "Keeps every note's original velocity."),
    ("a7.pitch", "Keeps every note's original key."),
    ("a7.start", "Keeps every note's original start."),
    ("a7.dur", "Keeps every note's original length."),
    (
        "a8",
        "Adds an offset chosen by each note's subchannel: the first number for its track's first subchannel, the second for the next …",
    ),
    (
        "a8.prop",
        "What the offsets change: velocity, pitch in semitones, or start in ticks.",
    ),
    (
        "a8.table",
        "The offset for each subchannel position (the first entry = the track's first subchannel); the list starts again when a track has more subchannels than entries.",
    ),
    (
        "a9",
        "Sets one property to a random value between From and To; the same seed always gives each note the same value.",
    ),
    (
        "a9.prop",
        "The property randomized: velocity, release, length in ticks, or a pitch offset added to each note's own key.",
    ),
    ("a9.lo", "One end of the random range (included)."),
    ("a9.hi", "The other end of the random range (included)."),
    (
        "a10",
        "Applies one ready-made combination of changes to every note.",
    ),
    (
        "a10.macro",
        "Staccato halves lengths and adds 10 velocity; Accent Beats adds 20 to notes on a beat; Soften takes 20 off and sets release 32; Legato Feel lengthens by 10 % with release 80; Ghost Off-Beats sets notes off the beat to velocity 24.",
    ),
    (
        "n01",
        "Sets one property from another: the source value, read on a 0 – 1 scale, goes through the Map curve onto Low … High.",
    ),
    (
        "n01.from",
        "What is read: the key within the selection's range, the velocity, the length (up to the longest note), the place inside the bar, or the subchannel's place in its track's list.",
    ),
    (
        "n01.to",
        "What is written: velocity, release, length in ticks, or MIDI channel (1 – 16).",
    ),
    (
        "n01.map",
        "The curve from the source's 0 – 1 to the output's Low – High (a straight line by default).",
    ),
    ("n01.lo", "The value written where the mapped source is 0."),
    ("n01.hi", "The value written where the mapped source is 1."),
    ("n02", "Adds velocity to the notes the rule picks out."),
    (
        "n02.rule",
        "Which notes are accented: notes on a bar line, the first notes of each phrase, notes that leap from the previous onset by more than Leap, or the top note of each chord.",
    ),
    (
        "n02.amount",
        "The velocity added to the accented notes (negative softens them); results stay within 1 – 127.",
    ),
    (
        "n02.leap",
        "For Leaps: how far, in semitones, a note must jump from the nearest key of the previous onset on its track to be accented.",
    ),
    (
        "n02.rest",
        "For Phrase Starts: a silence at least this long, in ticks, begins a new phrase.",
    ),
    (
        "n03",
        "Steps through a list of values: each note (or beat, bar, chord) takes the next value, and the list starts again when it runs out.",
    ),
    (
        "n03.prop",
        "The property the values set: velocity, release, length in ticks, or MIDI channel (1 – 16).",
    ),
    (
        "n03.values",
        "The values in order; the first goes to the first note, beat, bar or chord.",
    ),
    (
        "n03.per",
        "When the next value is used: each note of a track, each beat or bar since the selection's first note, or each chord (notes starting together share a value).",
    ),
    (
        "n04",
        "Makes one property travel from the first note's value to the last note's along the Shape, joining the two ends smoothly.",
    ),
    (
        "n04.prop",
        "The property bridged: velocity, release or length.",
    ),
    (
        "n04.shape",
        "How the value travels from the first note to the last across the selection's time span.",
    ),
    (
        "n04.strength",
        "How far each note moves toward the bridge (0 % = unchanged, 100 % = exactly on it).",
    ),
    (
        "n05",
        "Gives notes a length in milliseconds at the song's tempo, so they last the same time whatever the BPM.",
    ),
    (
        "n05.ms",
        "The length in milliseconds, turned into ticks through the tempo map at each note.",
    ),
    (
        "n05.keep",
        "Which end stays: Start keeps the start and moves the end; End keeps the end and moves the start.",
    ),
    (
        "n06",
        "Evens out velocities: each one moves toward the average of the notes around it in time.",
    ),
    (
        "n06.window",
        "How many neighboring notes are averaged, the note itself in the middle (1 = no change).",
    ),
    (
        "n06.strength",
        "How far each velocity moves toward that average (100 % = onto it).",
    ),
    (
        "n06.per",
        "Which notes count as neighbors: the whole track, the same key on the track, or the same subchannel.",
    ),
    (
        "n07",
        "Pulls values that stray far from the rest back in: anything more than the set number of standard deviations from the mean.",
    ),
    (
        "n07.prop",
        "The property checked: velocity, release or length.",
    ),
    (
        "n07.k",
        "How many standard deviations from the mean a value may lie before it counts as an outlier (smaller = stricter).",
    ),
    (
        "n07.action",
        "What an outlier becomes: the nearest allowed bound, or the median of the selection.",
    ),
    (
        "n08",
        "Splits the keyboard into zones at the split keys and gives each zone's notes a MIDI channel or a subchannel.",
    ),
    (
        "n08.splits",
        "The keys where a new zone starts (a note on a split key belongs to the zone above).",
    ),
    (
        "n08.values",
        "The value of each zone, lowest zone first: a MIDI channel (1 – 16) or a subchannel position; the list starts again when there are more zones.",
    ),
    (
        "n08.target",
        "What the zone values set: the MIDI channel, or the subchannel by its place in the track's list.",
    ),
    (
        "n09",
        "Writes a controller point at each note's start, its value following a ramp across the selection's time span.",
    ),
    (
        "n09.cc",
        "The controller written: CC 10 pan, CC 1 modulation, CC 74 or CC 11 expression.",
    ),
    (
        "n09.from",
        "The controller value at the selection's earliest start (0 – 127).",
    ),
    (
        "n09.to",
        "The controller value at the selection's latest start (0 – 127).",
    ),
    (
        "n09.shape",
        "How the controller value travels from From to To.",
    ),
    ("n10", "Sets a property only on the notes that pass a test."),
    (
        "n10.if",
        "The property tested: key, velocity, length in ticks, MIDI channel (1 – 16), subchannel position, or ticks after the bar line.",
    ),
    (
        "n10.test",
        "How the property is compared with Value: less than, at least, equal to, or between Value and Upper Value (both included).",
    ),
    (
        "n10.value",
        "The number the tested property is compared with.",
    ),
    ("n10.upper", "For Between: the other end of the range."),
    (
        "n10.then",
        "What is set on the notes that pass: velocity, release, length in ticks, MIDI channel, or subchannel (by its place in the track's list).",
    ),
    ("n10.to", "The value set on the notes that pass."),
    (
        "n11",
        "Makes MIDI channels and subchannels follow each other.",
    ),
    (
        "n11.dir",
        "Subchannel → Channel sets each note's channel from its subchannel's place; Channel → Subchannel picks the subchannel from the note's channel.",
    ),
    (
        "n11.map",
        "Same Index: subchannel position 0 ↔ channel 1, 1 ↔ 2 …; Table: the list below gives the value for each index.",
    ),
    (
        "n11.table",
        "For Table: the value for each index — channels 1 – 16 when setting channels, subchannel positions when setting subchannels; the list starts again when it runs out.",
    ),
    (
        "n12",
        "Sets ticks, gates, keys, velocities, channels, subchannels and a controller from your own formulas (Experimental); an empty field keeps each note's value.",
    ),
    (
        "n12.t",
        "The new start tick. Variables: t g k v rel c sc index count ppqn pos rnd and your constants; operators + - * / % // ^; functions such as sin, floor, round, mod (also as math.sin …). Example: t + index * 10 staggers the notes.",
    ),
    (
        "n12.g",
        "The new gate (length in ticks, at least 1). Examples: g * 2 doubles every length; ppqn / 4 makes sixteenth notes.",
    ),
    (
        "n12.k",
        "The new key (0 – 127; 0 – 255 in 256-Key Mode). Example: k + round(sin(index / 4) * 12) bends the keys up and down by up to an octave.",
    ),
    (
        "n12.v",
        "The new velocity (1 – 127). Examples: 20 + 100 * pos fades in across the selection; v * 0.5 halves every velocity.",
    ),
    (
        "n12.c",
        "The new MIDI channel (1 – 16; c reads 1 – 16 too). Examples: index % 16 + 1 cycles the channels; (c*4 + t/120 + k/8) % 16 + 1 scatters them.",
    ),
    (
        "n12.sc",
        "The new subchannel as its position in the track's list (sc reads 0 for the first). Example: index % 4 cycles the first four subchannels.",
    ),
    (
        "n12.ccn",
        "The controller (CC 0 – 119) the Controller Value formula writes at each note's start, on the note's channel. CC 120 – 127 are channel-mode commands (All Notes Off and the like) and are not offered.",
    ),
    (
        "n12.cc",
        "The controller value (0 – 127) written at each note's start; empty writes none. Example: round(pos * 127) ramps the controller across the selection.",
    ),
    (
        "n12.consts",
        "Your named constants as name = value, separated by semicolons — for example a = 12; b = a * 2. A value may use numbers, pi, e, the functions and earlier constants.",
    ),
    (
        "n12.policy",
        "A result outside its range: Clamp pins it to the nearest valid value; Wrap folds keys, velocities, channels, subchannels and controllers around their range (ticks and gates clamp); Leave the Note Unchanged skips the note; Stop with an Error applies nothing. A result that is no number (÷ 0, √ of a negative) skips its note unless you stop.",
    ),
    (
        "n12.round",
        "How a result becomes a whole number: Nearest (halves away from zero), Down, Up or Toward Zero; a result within rounding noise of a whole number counts as that number.",
    ),
    (
        "n13",
        "Shapes properties with any number of ranges: each covers a part of the selection and runs a property from one value to another through its own graph; rows apply top to bottom.",
    ),
    (
        "n13.rows",
        "The ranges, top to bottom: each has its switch, target (velocities, gates, ticks, transpose, keys, channels, subchannels or a controller), the part of the selection it covers along time, key, note order or velocity, From / To through its graph, and how it combines with the rows above (Set, Add, Multiply %, At Most, At Least).",
    ),
    (
        "n12.seed",
        "The seed of rnd, each note's repeatable random number from 0 up to 1; the same seed gives the same result every time.",
    ),
];

fn note_props_spec() -> ToolSpec {
    let mut p = vec![
        flag("vel.on", "Set velocity", false),
        int("vel", "Velocity", 1, 127, 100),
        flag("rel.on", "Set release velocity", false),
        int("rel", "Release velocity", 0, 127, 64),
        flag("pitch.on", "Set pitch", false),
        pitch("pitch", "Pitch", 60),
        flag("ch.on", "Set MIDI channel", false),
        int("ch", "MIDI channel", 1, 16, 1),
        flag("sub.on", "Set subchannel", false),
        int(
            "sub",
            "Subchannel (position in the track's list)",
            0,
            4095,
            0,
        ),
        // v0.8 §160–§161: relative subchannel (logical numbers, unbounded; the
        // notes keep their MIDI channels — no wrap through channels).
        flag("suboff.on", "Offset subchannels", false),
        int(
            "suboff",
            "Subchannel offset (+/−; 1, 3, 5 with +2 → 3, 5, 7)",
            -1_000_000,
            1_000_000,
            1,
        ),
        flag("start.on", "Set start", false),
        ticks("start", "Start (ticks)", 0, 1 << 50, 0),
        flag("dur.on", "Set length", false),
        ticks("dur", "Length (ticks)", 1, 1 << 40, 240),
        flag("grp.on", "Set group", false),
        int("grp", "Group id (0 = none)", 0, i64::from(u32::MAX), 0),
        choice("ctl", "Controller at each note start", CTL, 0),
        int(
            "ctl.val",
            "Controller value (fine pitch: cents)",
            -100,
            127,
            64,
        ),
        adv_switch(1, "Relative (+/−) mode"),
        int("a1.vel", "Velocity +/−", -127, 127, 10).adv(1),
        int("a1.rel", "Release +/−", -127, 127, 0).adv(1),
        int("a1.pitch", "Pitch +/− (semitones)", -127, 127, 0)
            .keyed()
            .adv(1),
        int(
            "a1.start",
            "Start +/− (ticks)",
            -1_000_000_000,
            1_000_000_000,
            0,
        )
        .adv(1),
        int(
            "a1.dur",
            "Length +/− (ticks)",
            -1_000_000_000,
            1_000_000_000,
            0,
        )
        .adv(1),
        adv_switch(2, "Proportional scaling"),
        float("a2.vel", "Velocity", 0.0, 400.0, 80.0, "%").adv(2),
        float("a2.rel", "Release", 0.0, 400.0, 100.0, "%").adv(2),
        float("a2.dur", "Length", 1.0, 1000.0, 100.0, "%").adv(2),
        adv_switch(3, "Gradient across the selection"),
        choice("a3.prop", "Property", PROPS, 0).adv(3),
        // −127: the lowest pitch offset (the "Pitch offset" property).
        int("a3.from", "From", -127, 1_000_000, 40).keyed().adv(3),
        int("a3.to", "To", -127, 1_000_000, 120).keyed().adv(3),
        curve("a3.curve", "Shape", Curve::identity())
            .overshoot()
            .adv(3),
        adv_switch(4, "Property copy / paste"),
        choice("a4.from", "Copy from", COPY_FROM, 0).adv(4),
        choice("a4.what", "Copy", COPY_WHAT, 0).adv(4),
        adv_switch(5, "Normalization"),
        choice(
            "a5.mode",
            "Normalize",
            &["Loudest note to the target", "Average to the target"],
            0,
        )
        .adv(5),
        int("a5.target", "Target velocity", 1, 127, 110).adv(5),
        adv_switch(6, "Histogram distribution (by rank)"),
        choice("a6.prop", "Property", &PROPS[..3], 0).adv(6),
        int("a6.lo", "Lowest", 0, 1_000_000, 30).adv(6),
        int("a6.hi", "Highest", 0, 1_000_000, 120).adv(6),
        adv_switch(7, "Property lock mask"),
        flag("a7.vel", "Lock velocity", true).adv(7),
        flag("a7.pitch", "Lock pitch", false).adv(7),
        flag("a7.start", "Lock start", false).adv(7),
        flag("a7.dur", "Lock length", false).adv(7),
        adv_switch(8, "Per-subchannel offset table"),
        choice(
            "a8.prop",
            "Offsets apply to",
            &["Velocity", "Pitch", "Start (ticks)"],
            0,
        )
        .adv(8),
        list(
            "a8.table",
            "Offset by subchannel position",
            -100_000,
            100_000,
            4096,
            &[0, -10, 10, -20],
        )
        .adv(8),
        adv_switch(9, "Deterministic random range"),
        choice("a9.prop", "Property", PROPS, 0).adv(9),
        int("a9.lo", "From", -127, 1_000_000, 60).keyed().adv(9),
        int("a9.hi", "To", -127, 1_000_000, 110).keyed().adv(9),
        seed("a9.seed").adv(9),
        adv_switch(10, "Multi-property macro preset"),
        choice("a10.macro", "Macro", MACROS, 0).adv(10),
        // V09-S36: functions declared by stable id (TOOL_CATALOG n01 … n11).
        flag("n01", "Cross-property mapping", false),
        choice("n01.from", "Source", NP_SOURCES, 0),
        choice("n01.to", "Target", NP_TARGETS, 0),
        curve("n01.map", "Map", Curve::identity()).overshoot(),
        int("n01.lo", "Low", 0, 1_000_000, 40),
        int("n01.hi", "High", 0, 1_000_000, 120),
        flag("n02", "Accent rules", false),
        choice("n02.rule", "Rule", NP_ACCENTS, 0),
        int("n02.amount", "Amount", -64, 64, 20),
        int("n02.leap", "Leap (semitones)", 1, 24, 7),
        ticks("n02.rest", "Phrase split rest", 0, 1 << 40, 480),
        flag("n03", "Value cycle", false),
        choice("n03.prop", "Property", NP_TARGETS, 0),
        list(
            "n03.values",
            "Values",
            0,
            1_000_000,
            256,
            &[100, 60, 80, 60],
        ),
        choice("n03.per", "Advance per", NP_ADVANCE, 0),
        flag("n04", "Ends bridge", false),
        choice("n04.prop", "Property", NP_PROPS3, 0),
        curve("n04.shape", "Shape", Curve::identity()).overshoot(),
        float("n04.strength", "Strength", 0.0, 100.0, 100.0, "%"),
        flag("n05", "Real-time length", false),
        float("n05.ms", "Length", 1.0, 60_000.0, 250.0, "ms"),
        choice("n05.keep", "Keep", NP_KEEP, 0),
        flag("n06", "Velocity smoothing", false),
        int("n06.window", "Window (notes)", 1, 256, 5),
        float("n06.strength", "Strength", 0.0, 100.0, 100.0, "%"),
        choice("n06.per", "Per", NP_PER, 0),
        flag("n07", "Outlier clamp", false),
        choice("n07.prop", "Property", NP_PROPS3, 0),
        float("n07.k", "Deviations", 0.5, 6.0, 2.0, "σ"),
        choice("n07.action", "Action", NP_OUTLIER_ACTIONS, 0),
        flag("n08", "Pitch zones", false),
        list("n08.splits", "Split keys", 0, 255, 255, &[48, 72]),
        list("n08.values", "Value per zone", 0, 4095, 256, &[1, 2, 3]),
        choice("n08.target", "Zone target", NP_ZONE_TARGETS, 0),
        flag("n09", "Controller gradient", false),
        choice("n09.cc", "Controller", NP_CONTROLLERS, 0),
        int("n09.from", "From", 0, 127, 0),
        int("n09.to", "To", 0, 127, 127),
        curve("n09.shape", "Shape", Curve::identity()).overshoot(),
        flag("n10", "Conditional set", false),
        choice("n10.if", "If", NP_IF, 2),
        choice("n10.test", "Test", NP_TESTS, 0),
        int("n10.value", "Value", 0, 1_000_000, 240),
        int("n10.upper", "Upper value", 0, 1_000_000, 480),
        choice("n10.then", "Then set", NP_THEN, 0),
        int("n10.to", "To", 0, 1_000_000, 30),
        flag("n11", "Channel and subchannel sync", false),
        choice("n11.dir", "Direction", NP_SYNC, 0),
        choice("n11.map", "Mapping", NP_MAPPING, 0),
        list("n11.table", "Table", 0, 4095, 4096, &[1, 2, 3, 4]),
        // B1-0075: expression fields (empty = the note's own value).
        flag("n12", "Math expressions (Experimental)", false),
        text("n12.t", NP_X_FIELDS[0].1, MAX_EXPR_LEN, ""),
        text("n12.g", NP_X_FIELDS[1].1, MAX_EXPR_LEN, ""),
        text("n12.k", NP_X_FIELDS[2].1, MAX_EXPR_LEN, ""),
        text("n12.v", NP_X_FIELDS[3].1, MAX_EXPR_LEN, ""),
        text("n12.c", NP_X_FIELDS[4].1, MAX_EXPR_LEN, ""),
        text("n12.sc", NP_X_FIELDS[5].1, MAX_EXPR_LEN, ""),
        int(
            "n12.ccn",
            "Controller number",
            0,
            i64::from(ranges::MAX_CONTROLLER),
            1,
        ),
        text("n12.cc", NP_X_FIELDS[6].1, MAX_EXPR_LEN, ""),
        text("n12.consts", "Constants", MAX_EXPR_LEN, ""),
        choice("n12.policy", "Out of range", NP_X_POLICY, 0),
        choice("n12.round", "Rounding", NP_X_ROUND, 0),
        seed("n12.seed"),
        // B1-0078: any number of range rows (`editor::ranges` text).
        flag("n13", "Ranges", false),
        text("n13.rows", "Ranges", ranges::MAX_TEXT, ""),
    ];
    p.extend(boundary::specs(PitchPolicy::Refuse));
    with_help(&mut p, NOTE_PROPS_HELP);
    ToolSpec {
        id: "note_props",
        name: "Note Properties",
        family: Family::Editor,
        summary: "Sets velocity, release, pitch, channel, subchannel, start, length or group of the selected notes (unset fields keep each note's value — \"Mixed\"); optional controller at each note start.",
        params: p,
        advanced: vec![
            "Relative (+/−) mode",
            "Proportional scaling",
            "Gradient across the selection",
            "Property copy / paste",
            "Normalization",
            "Histogram distribution",
            "Property lock mask",
            "Per-subchannel offset table",
            "Deterministic random range",
            "Multi-property macro preset",
        ],
        // Orders = the run order (docs/TRANSFORM_ENGINE.md "Note
        // Properties"), after the v0.8 fields and functions and before the
        // lock mask: values (n03, n01, n10), channels (n08, n11), length
        // (n05), shapes across the notes (n04, n06, n07), accents (n02), the
        // math expressions (n12, B1-0075), then the controller points (n09).
        functions: [
            ("n01", "Cross-property mapping", 120),
            ("n02", "Accent rules", 200),
            ("n03", "Value cycle", 110),
            ("n04", "Ends bridge", 170),
            ("n05", "Real-time length", 160),
            ("n06", "Velocity smoothing", 180),
            ("n07", "Outlier clamp", 190),
            ("n08", "Pitch zones", 140),
            ("n09", "Controller gradient", 510),
            ("n10", "Conditional set", 130),
            ("n11", "Channel and subchannel sync", 150),
            ("n12", "Math expressions (Experimental)", 210),
            ("n13", "Ranges", 205),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// A note being edited in wide integers.
#[derive(Clone, Copy, Debug)]
struct W {
    start: i128,
    dur: i128,
    pitch: i64,
    vel: i64,
    rel: i64,
    ch: u8,
    sub: u32,
    group: u32,
}

impl W {
    fn of(n: &TNote) -> W {
        W {
            start: i128::from(n.start),
            dur: i128::from(n.dur),
            pitch: i64::from(n.pitch),
            vel: i64::from(n.vel),
            rel: i64::from(n.rel),
            ch: n.ch,
            sub: n.sub,
            group: n.group,
        }
    }

    fn prop(&self, k: usize) -> i64 {
        match k {
            0 => self.vel,
            1 => self.rel,
            2 => self.dur.min(i128::from(i64::MAX)) as i64,
            _ => self.pitch,
        }
    }

    fn set_prop(&mut self, k: usize, v: i64, orig_pitch: i64) {
        match k {
            0 => self.vel = v.clamp(1, 127),
            1 => self.rel = v.clamp(0, 127),
            2 => self.dur = i128::from(v.max(1)),
            _ => self.pitch = orig_pitch + v,
        }
    }

    /// v0.9: sets velocity, release, length or channel (`NP_TARGETS`; a
    /// channel is 1 – 16).
    fn set_target(&mut self, k: usize, v: i64) {
        match k {
            0 => self.vel = v.clamp(1, 127),
            1 => self.rel = v.clamp(0, 127),
            2 => self.dur = i128::from(v.max(1)),
            _ => self.ch = (v.clamp(1, 16) - 1) as u8,
        }
    }

    fn emit(&self, n: &TNote, out: &mut Output) {
        let mut t = *n;
        t.ch = self.ch.min(15);
        t.sub = self.sub;
        t.group = self.group;
        t.rel = self.rel.clamp(0, 127) as u8;
        out.emit(self.start, self.dur, self.pitch, self.vel, &t);
    }
}

/// Note Properties' v0.9 functions declared by id (TOOL_CATALOG
/// `note_props` n01 … n11). `None` = switched off; with every one off the
/// kernel is the v0.8 one.
#[derive(Clone, Default)]
struct NpNew {
    /// n01: source (`NP_SOURCES`), target (`NP_TARGETS`), map, low, high.
    cross: Option<(usize, usize, Curve, i64, i64)>,
    /// n02: rule (`NP_ACCENTS`), amount, leap, phrase split rest.
    accent: Option<(usize, i64, i64, u64)>,
    /// n03: property (`NP_TARGETS`), values, advance per (`NP_ADVANCE`).
    cycle: Option<(usize, Vec<i64>, usize)>,
    /// n04: property (`NP_PROPS3`), shape, strength (fraction).
    bridge: Option<(usize, Curve, f64)>,
    /// n05: length (µs), keep the end (else the start).
    realtime: Option<(u64, bool)>,
    /// n06: window (notes), strength (fraction), per (`NP_PER`).
    smooth: Option<(usize, f64, usize)>,
    /// n07: property (`NP_PROPS3`), deviations, set to the median (else
    /// clamp).
    outlier: Option<(usize, f64, bool)>,
    /// n08: split keys (ascending, distinct), values, subchannel (else
    /// channel).
    zones: Option<(Vec<i64>, Vec<i64>, bool)>,
    /// n09: lane, from, to, shape.
    ctl: Option<(Lane, i64, i64, Curve)>,
    /// n10: if (`NP_IF`), test (`NP_TESTS`), value, upper value, then
    /// (`NP_THEN`), to.
    cond: Option<(usize, usize, i64, i64, usize, i64)>,
    /// n11: channel → subchannel (else subchannel → channel), table (empty
    /// = the same index).
    sync: Option<(bool, Vec<i64>)>,
}

impl NpNew {
    fn from(p: &Params) -> NpNew {
        let pct = |k: &str| (p.float(k) / 100.0).clamp(0.0, 1.0);
        NpNew {
            cross: p.flag("n01").then(|| {
                (
                    p.choice("n01.from"),
                    p.choice("n01.to"),
                    p.curve("n01.map"),
                    p.int("n01.lo"),
                    p.int("n01.hi"),
                )
            }),
            accent: p.flag("n02").then(|| {
                (
                    p.choice("n02.rule"),
                    p.int("n02.amount"),
                    p.int("n02.leap"),
                    p.ticks("n02.rest"),
                )
            }),
            cycle: (p.flag("n03") && !p.list("n03.values").is_empty()).then(|| {
                (
                    p.choice("n03.prop"),
                    p.list("n03.values"),
                    p.choice("n03.per"),
                )
            }),
            bridge: p.flag("n04").then(|| {
                (
                    p.choice("n04.prop"),
                    p.curve("n04.shape"),
                    pct("n04.strength"),
                )
            }),
            realtime: p.flag("n05").then(|| {
                (
                    (p.float("n05.ms") * 1000.0).round().max(1.0) as u64,
                    p.choice("n05.keep") == 1,
                )
            }),
            smooth: p.flag("n06").then(|| {
                (
                    p.int("n06.window").clamp(1, 256) as usize,
                    pct("n06.strength"),
                    p.choice("n06.per"),
                )
            }),
            outlier: p.flag("n07").then(|| {
                (
                    p.choice("n07.prop"),
                    p.float("n07.k").max(0.0),
                    p.choice("n07.action") == 1,
                )
            }),
            zones: (p.flag("n08") && !p.list("n08.values").is_empty()).then(|| {
                let mut s = p.list("n08.splits");
                s.sort_unstable();
                s.dedup();
                (s, p.list("n08.values"), p.choice("n08.target") == 1)
            }),
            ctl: p.flag("n09").then(|| {
                let lane = match p.choice("n09.cc") {
                    0 => Lane::Cc(10),
                    1 => Lane::Cc(1),
                    2 => Lane::Cc(74),
                    _ => Lane::Cc(11),
                };
                (
                    lane,
                    p.int("n09.from"),
                    p.int("n09.to"),
                    p.curve("n09.shape"),
                )
            }),
            cond: p.flag("n10").then(|| {
                (
                    p.choice("n10.if"),
                    p.choice("n10.test"),
                    p.int("n10.value"),
                    p.int("n10.upper"),
                    p.choice("n10.then"),
                    p.int("n10.to"),
                )
            }),
            sync: p.flag("n11").then(|| {
                (
                    p.choice("n11.dir") == 1,
                    if p.choice("n11.map") == 1 {
                        p.list("n11.table")
                    } else {
                        Vec::new()
                    },
                )
            }),
        }
    }

    fn any(&self) -> bool {
        self.cross.is_some()
            || self.accent.is_some()
            || self.cycle.is_some()
            || self.bridge.is_some()
            || self.realtime.is_some()
            || self.smooth.is_some()
            || self.outlier.is_some()
            || self.zones.is_some()
            || self.ctl.is_some()
            || self.cond.is_some()
            || self.sync.is_some()
    }
}

/// Every setting of one Note Properties run.
#[derive(Clone)]
struct Np {
    vel: Option<i64>,
    rel: Option<i64>,
    pit: Option<i64>,
    ch: Option<u8>,
    sub: Option<usize>,
    suboff: Option<i64>,
    start: Option<u64>,
    dur: Option<u64>,
    grp: Option<u32>,
    new_table: Option<crate::model::GroupTable>,
    ctl: usize,
    ctl_val: i64,
    rel_mode: Option<(i64, i64, i64, i128, i128)>,
    scale: Option<(f64, f64, f64)>,
    grad: Option<(usize, i64, i64, Curve)>,
    copy: Option<(usize, usize)>,
    norm: Option<(usize, i64)>,
    hist: Option<(usize, i64, i64)>,
    lock: Option<(bool, bool, bool, bool)>,
    table: Option<(usize, Vec<i64>)>,
    rnd: Option<(usize, i64, i64, u64)>,
    mac: Option<usize>,
    ppq: u64,
    new: NpNew,
    /// n12: the math expressions (parsed once at build).
    expr: Option<std::sync::Arc<NpExpr>>,
    /// n13: the range rows (B1-0078), parsed once at build.
    ranges: Option<std::sync::Arc<Vec<RangeRow>>>,
    /// B2-F01: the value passes (n03, n01, n10, n08, n11, n05, n04, n06,
    /// n07, n02, n13, n12) in run order (`order::NOTE_PROPS_PASSES`).
    order: Vec<&'static str>,
    ctx: Ctx,
    /// With Control Sweeps (E0422): what each level's functions are built
    /// from (every Note Properties target is a function's control).
    swept: Option<std::sync::Arc<Rebuild>>,
}

/// What the v0.8 functions read from the whole selection.
struct NpPre {
    /// [a4]'s model note.
    model: Option<TNote>,
    /// [a5]'s factor.
    norm_k: Option<f64>,
    /// [a6]'s ranks.
    ranks: Option<Vec<usize>>,
}

/// The position of subchannel `id` in its track's list (0 when unlisted)
/// and the list's length.
/// Each note's place for a cycled list (Note Properties n03, Note Repeat
/// n09): its place on its track (`ADVANCE_NOTE`), the beats / bars since the
/// selection's first note (`ADVANCE_BEAT` / `ADVANCE_BAR`), or its onset's
/// place on its track (`ADVANCE_CHORD`).
fn place_index(src: &Source, meter: &crate::view::grid::MeterMap, per: usize) -> Vec<u64> {
    let ns = &src.notes;
    let first = src.stats.min_start;
    match per {
        ADVANCE_NOTE | ADVANCE_CHORD => {
            let mut k = vec![0u64; ns.len()];
            for idx in per_track(src) {
                let mut g = 0u64;
                for (j, &i) in idx.iter().enumerate() {
                    if per == ADVANCE_NOTE {
                        k[i] = j as u64;
                    } else {
                        if j > 0 && ns[i].start != ns[idx[j - 1]].start {
                            g += 1;
                        }
                        k[i] = g;
                    }
                }
            }
            k
        }
        ADVANCE_BEAT => match meter.beat_at(first) {
            Some((b0, _)) => ns
                .iter()
                .map(|n| {
                    meter
                        .beat_at(n.start)
                        .map_or(0, |(b, _)| b.saturating_sub(b0))
                })
                .collect(),
            None => place_index(src, meter, ADVANCE_BAR),
        },
        _ => {
            let b0 = bar_place(meter, first).0;
            ns.iter()
                .map(|n| bar_place(meter, n.start).0.saturating_sub(b0))
                .collect()
        }
    }
}

pub(super) fn sub_place(src: &Source, track: u16, id: u32) -> (usize, usize) {
    src.track(track)
        .map_or((0, 0), |t| (t.sub_index(id).unwrap_or(0), t.subs.len()))
}

/// The subchannel at position `pos` of the note's track (the last when
/// beyond the list; unchanged without a list).
pub(super) fn sub_at(src: &Source, track: u16, pos: i64, keep: u32) -> u32 {
    src.track(track)
        .and_then(|t| {
            let i = usize::try_from(pos.max(0)).unwrap_or(usize::MAX);
            t.subs
                .get(i.min(t.subs.len().saturating_sub(1)))
                .map(|s| s.0)
        })
        .unwrap_or(keep)
}

impl Np {
    fn pre(&self, ns: &[TNote]) -> NpPre {
        // Copy source (function 4).
        let model = self.copy.map(|(from, _)| {
            let i = match from {
                0 => 0,
                1 => ns.len() - 1,
                2 => (0..ns.len())
                    .max_by_key(|&i| (ns[i].vel, usize::MAX - i))
                    .unwrap_or(0),
                _ => (0..ns.len())
                    .max_by_key(|&i| (ns[i].dur, usize::MAX - i))
                    .unwrap_or(0),
            };
            ns[i]
        });
        // Normalization factor (function 5).
        let norm_k = self.norm.map(|(mode, target)| {
            let v = if mode == 0 {
                f64::from(ns.iter().map(|n| n.vel).max().unwrap_or(1))
            } else {
                ns.iter().map(|n| f64::from(n.vel)).sum::<f64>() / ns.len() as f64
            };
            target as f64 / v.max(1.0)
        });
        // Ranks (function 6).
        let ranks: Option<Vec<usize>> = self.hist.map(|(k, _, _)| {
            let mut order: Vec<usize> = (0..ns.len()).collect();
            order.sort_by_key(|&i| (W::of(&ns[i]).prop(k), i));
            let mut r = vec![0; ns.len()];
            for (rank, &i) in order.iter().enumerate() {
                r[i] = rank;
            }
            r
        });
        NpPre {
            model,
            norm_k,
            ranks,
        }
    }

    /// The v0.8 fields and functions [a1] – [a6], [a8] – [a10] on note `i`
    /// (the lock mask [a7] and the velocity clamp come in [`Np::finish`]).
    #[inline(always)]
    fn stages(&self, src: &Source, pre: &NpPre, i: usize, n: &TNote) -> Result<W, TransformError> {
        let ns = &src.notes;
        let o = W::of(n);
        let mut w = o;
        let tr = src.track(n.track);
        if let Some(v) = self.vel {
            w.vel = v;
        }
        if let Some(v) = self.rel {
            w.rel = v;
        }
        if let Some(v) = self.pit {
            w.pitch = v;
        }
        if let Some(c) = self.ch {
            w.ch = c;
        }
        if let Some(s) = self.sub
            && let Some(t) = tr
            && let Some(&(id, _)) = t.subs.get(s.min(t.subs.len().saturating_sub(1)))
        {
            w.sub = id;
        }
        // Offset after Set (both may be on): logical subchannel + offset.
        if let Some(off) = self.suboff {
            let v = i64::from(w.sub) + off;
            w.sub = u32::try_from(v).map_err(|_| {
                TransformError::Invalid(format!(
                    "Subchannel offset {off:+} would move subchannel {} {} the valid range; nothing was changed.",
                    u64::from(w.sub) + 1,
                    if v < 0 { "below" } else { "beyond" }
                ))
            })?;
        }
        if let Some(s) = self.start {
            w.start = i128::from(s);
        }
        if let Some(d) = self.dur {
            w.dur = i128::from(d);
        }
        if let Some(g) = self.grp {
            w.group = g;
        }
        if let Some((dv, dr, dp, ds, dd)) = self.rel_mode {
            w.vel += dv;
            w.rel += dr;
            w.pitch += dp;
            w.start += ds;
            w.dur += dd;
        }
        if let Some((sv, sr, sd)) = self.scale {
            w.vel = (w.vel as f64 * sv / 100.0).round() as i64;
            w.rel = (w.rel as f64 * sr / 100.0).round() as i64;
            w.dur = (w.dur as f64 * sd / 100.0).round() as i128;
        }
        if let Some((k, from, to, c)) = &self.grad {
            let x = c.eval(pos(n, &src.stats));
            let v = (*from as f64 + (*to - *from) as f64 * x).round() as i64;
            w.set_prop(*k, v, o.pitch);
        }
        if let (Some(m), Some((_, what))) = (pre.model, self.copy) {
            w.vel = i64::from(m.vel);
            if what == 1 || what == 3 {
                w.rel = i64::from(m.rel);
            }
            if what >= 2 {
                w.dur = i128::from(m.dur);
            }
            if what == 3 {
                w.ch = m.ch;
                // Subchannel only within the same track (ids are per track).
                if m.track == n.track {
                    w.sub = m.sub;
                }
            }
        }
        if let Some(k) = pre.norm_k {
            w.vel = (w.vel as f64 * k).round() as i64;
        }
        if let (Some(r), Some((k, lo, hi))) = (&pre.ranks, self.hist) {
            let t = if ns.len() > 1 {
                r[i] as f64 / (ns.len() - 1) as f64
            } else {
                0.5
            };
            let v = (lo as f64 + (hi - lo) as f64 * t).round() as i64;
            w.set_prop(k, v, o.pitch);
        }
        if let Some((k, tab)) = &self.table
            && !tab.is_empty()
        {
            let off = tab[sub_pos(src, n) % tab.len()];
            match k {
                0 => w.vel += off,
                1 => w.pitch += off,
                _ => w.start += i128::from(off),
            }
        }
        if let Some((k, lo, hi, seed)) = self.rnd {
            let v = Rng::for_note(seed, salt::SHAPE, n).range_i64(lo.min(hi), lo.max(hi));
            w.set_prop(k, v, o.pitch);
        }
        if let Some(m) = self.mac {
            let on_beat = n.start.is_multiple_of(self.ppq.max(1));
            match m {
                0 => {
                    w.dur = (w.dur / 2).max(1);
                    w.vel += 10;
                }
                1 => {
                    if on_beat {
                        w.vel += 20;
                    }
                }
                2 => {
                    w.vel -= 20;
                    w.rel = 32;
                }
                3 => {
                    w.dur = w.dur.saturating_add(w.dur / 10);
                    w.rel = 80;
                }
                _ => {
                    if !on_beat {
                        w.vel = 24;
                    }
                }
            }
        }
        Ok(w)
    }

    /// The lock mask [a7], the velocity clamp, the note and the base
    /// controller point (left out when n09 writes the same controller).
    #[inline(always)]
    fn finish(&self, n: &TNote, mut w: W, out: &mut Output, base_ctl: bool) {
        let o = W::of(n);
        if let Some((lv, lp, ls, ld)) = self.lock {
            if lv {
                w.vel = o.vel;
            }
            if lp {
                w.pitch = o.pitch;
            }
            if ls {
                w.start = o.start;
            }
            if ld {
                w.dur = o.dur;
            }
        }
        w.vel = w.vel.clamp(1, 127);
        w.emit(n, out);
        if self.ctl > 0 && base_ctl && w.start >= 0 {
            let (lane, v) = self.base_lane();
            out.emit_event(n.track, lane, w.ch, w.start, v);
        }
    }

    /// The base controller's lane and value.
    fn base_lane(&self) -> (Lane, i64) {
        match self.ctl {
            1 => (Lane::Cc(10), self.ctl_val.clamp(0, 127)),
            2 => (Lane::Cc(1), self.ctl_val.clamp(0, 127)),
            3 => (Lane::Cc(74), self.ctl_val.clamp(0, 127)),
            // Cents at the default ±2 semitone range: 8192 / 200 per cent.
            _ => (
                Lane::PitchBend,
                8192 + self.ctl_val.clamp(-100, 100) * 8192 / 200,
            ),
        }
    }

    fn end_run(&self, out: &mut Output) {
        if let Some(t) = &self.new_table {
            out.groups = Some(t.clone());
        }
        if self.ctl > 0
            || self.new.ctl.is_some()
            || self.expr.as_ref().is_some_and(|x| x.writes_cc())
            || self.ranges.as_ref().is_some_and(|r| {
                r.iter()
                    .any(|r| r.on && matches!(r.target, Target::Controller(_)))
            })
        {
            out.message = Some(
                "Controller events are written on each note's MIDI channel and affect every note sounding on that channel."
                    .into(),
            );
        }
    }

    /// The v0.8 kernel: each note on its own.
    fn run8(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let ns = &src.notes;
        if ns.is_empty() {
            return Ok(());
        }
        let pre = self.pre(ns);
        for (i, n) in ns.iter().enumerate() {
            let w = self.stages(src, &pre, i, n)?;
            self.finish(n, w, out, true);
            out.check()?;
        }
        self.end_run(out);
        Ok(())
    }

    /// The v0.9 pipeline: the v0.8 stages for every note, the functions
    /// declared by id as passes over them, then the lock, the notes and the
    /// controller points.
    fn run9(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        // Each note's functions under Control Sweeps (E0422).
        match &self.swept {
            Some(r) => {
                let lv: PerLevel<NpNew> = r.levels(&self.ctx, src, |p, _| NpNew::from(p));
                self.run9_at(src, out, |i| lv.at(i))
            }
            None => self.run9_at(src, out, |_| &self.new),
        }
    }

    /// [`Np::run9`] with note `i`'s functions `at(i)`.
    fn run9_at<'a>(
        &'a self,
        src: &Source,
        out: &mut Output,
        at: impl Fn(usize) -> &'a NpNew,
    ) -> Result<(), TransformError> {
        let ns = &src.notes;
        if ns.is_empty() {
            return Ok(());
        }
        let pre = self.pre(ns);
        let mut ws: Vec<W> = Vec::with_capacity(ns.len());
        for (i, n) in ns.iter().enumerate() {
            ws.push(self.stages(src, &pre, i, n)?);
        }
        let w = &self.new;
        // B2-F01: the value passes in run order (declared: n03, n01, n10,
        // n08, n11, n05, n04, n06, n07, n02, n13, n12 — each reads what the
        // passes before it wrote). The controller lanes keep their
        // precedence whatever the order (n12's lane wins, n13's next).
        let mut rcount = 0u64;
        let mut rcc: Vec<Vec<(u8, i64)>> = Vec::new();
        let mut xcount = XCount::default();
        let mut xcc = Vec::new();
        for pass in &self.order {
            match *pass {
                "n03" => {
                    if let Some((prop, values, per)) = &w.cycle {
                        let k = self.cycle_index(src, *per);
                        for (i, x) in ws.iter_mut().enumerate() {
                            x.set_target(*prop, values[(k[i] % values.len() as u64) as usize]);
                        }
                    }
                }
                "n01" => {
                    if let Some((from, to, map, _, _)) = &w.cross {
                        for (i, (x, n)) in ws.iter_mut().zip(ns.iter()).enumerate() {
                            let (lo, hi) = at(i).cross.as_ref().map_or((0, 0), |c| (c.3, c.4));
                            let u = self.source_value(src, n, x, *from);
                            let v = (lo as f64 + (hi - lo) as f64 * map.eval(u)).round() as i64;
                            x.set_target(*to, v);
                        }
                    }
                }
                "n10" => {
                    if let Some((what, test, value, upper, then, _)) = w.cond {
                        for (i, (x, n)) in ws.iter_mut().zip(ns.iter()).enumerate() {
                            let to = at(i).cond.map_or(0, |c| c.5);
                            let v = self.if_value(src, n, x, what);
                            let hit = match test {
                                TEST_LESS => v < value,
                                TEST_AT_LEAST => v >= value,
                                TEST_EQUAL => v == value,
                                _ => v >= value.min(upper) && v <= value.max(upper),
                            };
                            if hit {
                                if then == THEN_SUB {
                                    x.sub = sub_at(src, n.track, to, x.sub);
                                } else {
                                    x.set_target(then, to);
                                }
                            }
                        }
                    }
                }
                "n08" => {
                    if let Some((splits, values, to_sub)) = &w.zones {
                        for (x, n) in ws.iter_mut().zip(ns.iter()) {
                            let z = splits.partition_point(|&s| s <= x.pitch);
                            let v = values[z % values.len()];
                            if *to_sub {
                                x.sub = sub_at(src, n.track, v, x.sub);
                            } else {
                                x.ch = (v.clamp(1, 16) - 1) as u8;
                            }
                        }
                    }
                }
                "n11" => {
                    if let Some((to_sub, table)) = &w.sync {
                        for (x, n) in ws.iter_mut().zip(ns.iter()) {
                            if *to_sub {
                                let c = i64::from(x.ch);
                                let pos = if table.is_empty() {
                                    c
                                } else {
                                    table[(c as usize) % table.len()]
                                };
                                x.sub = sub_at(src, n.track, pos, x.sub);
                            } else {
                                let (pos, _) = sub_place(src, n.track, x.sub);
                                x.ch = if table.is_empty() {
                                    (pos % 16) as u8
                                } else {
                                    (table[pos % table.len()].clamp(1, 16) - 1) as u8
                                };
                            }
                        }
                    }
                }
                "n05" => {
                    if let Some((_, keep_end)) = w.realtime {
                        let mut tt = TempoTime::new(&self.ctx);
                        for (i, x) in ws.iter_mut().enumerate() {
                            let us = at(i).realtime.map_or(0, |r| r.0);
                            let Ok(s) = u64::try_from(x.start) else {
                                continue;
                            };
                            if keep_end {
                                let e = s.saturating_add(
                                    u64::try_from(x.dur.max(1)).unwrap_or(u64::MAX),
                                );
                                let s2 = tt.tick_before(e, us).min(e.saturating_sub(1));
                                x.start = i128::from(s2);
                                x.dur = i128::from(e - s2);
                            } else {
                                x.dur = i128::from(tt.ticks_after(s, us).max(1));
                            }
                        }
                    }
                }
                "n04" => {
                    if let Some((prop, shape, _)) = &w.bridge {
                        // The first / last note: earliest start (the lowest key) / latest
                        // start (the highest key).
                        let first = (0..ns.len()).min_by_key(|&i| (ns[i].start, ns[i].pitch, i));
                        let last = (0..ns.len())
                            .max_by_key(|&i| (ns[i].start, ns[i].pitch, std::cmp::Reverse(i)));
                        if let (Some(a), Some(b)) = (first, last) {
                            let (va, vb) = (ws[a].prop(*prop) as f64, ws[b].prop(*prop) as f64);
                            for (i, (x, n)) in ws.iter_mut().zip(ns.iter()).enumerate() {
                                let strength = at(i).bridge.as_ref().map_or(0.0, |b| b.2);
                                let t = va + (vb - va) * shape.eval(pos(n, &src.stats));
                                let cur = x.prop(*prop) as f64;
                                x.set_target(*prop, (cur + strength * (t - cur)).round() as i64);
                            }
                        }
                    }
                }
                "n06" => {
                    if let Some((window, _, per)) = w.smooth {
                        self.smooth(
                            src,
                            &mut ws,
                            window,
                            |i| at(i).smooth.map_or(0.0, |m| m.1),
                            per,
                        );
                    }
                }
                "n07" => {
                    if let Some((prop, _, median)) = w.outlier {
                        outliers(
                            &mut ws,
                            prop,
                            |i| at(i).outlier.map_or(0.0, |o| o.1),
                            median,
                        );
                    }
                }
                "n02" => {
                    if let Some((rule, _, _, rest)) = w.accent {
                        let hit =
                            self.accents(src, rule, |i| at(i).accent.map_or(0, |a| a.2), rest);
                        for (i, (x, h)) in ws.iter_mut().zip(hit).enumerate() {
                            if h {
                                let amount = at(i).accent.map_or(0, |a| a.1);
                                x.vel = (x.vel + amount).clamp(1, 127);
                            }
                        }
                    }
                }
                "n13" => {
                    // n13 (order 205, B1-0078): the range rows, top to bottom.
                    if let Some(rows) = &self.ranges {
                        rcc = apply_ranges(src, &self.ctx, rows, &mut ws, &mut rcount)?;
                    }
                }
                "n12" => {
                    // n12 (order 210, B1-0075): the math expressions, last of the value
                    // passes — every field reads what the passes above wrote.
                    if let Some(x) = &self.expr {
                        xcc = x.run(src, &self.ctx, &mut ws, out, &mut xcount)?;
                    }
                }
                _ => {}
            }
        }
        let r_lanes: Vec<u8> = self.ranges.as_ref().map_or_else(Vec::new, |rows| {
            let mut l: Vec<u8> = rows
                .iter()
                .filter(|r| r.on)
                .filter_map(|r| match r.target {
                    Target::Controller(n) => Some(n),
                    _ => None,
                })
                .collect();
            l.sort_unstable();
            l.dedup();
            l
        });
        let x_lane = self
            .expr
            .as_ref()
            .filter(|x| x.writes_cc())
            .map(|x| Lane::Cc(x.cc));
        // n12's controller gives no way: the base controller and n09 leave
        // out its lane (one point per tick).
        let in_ranges = |l: Lane| matches!(l, Lane::Cc(n) if r_lanes.binary_search(&n).is_ok());
        let n09 = w
            .ctl
            .as_ref()
            .filter(|c| x_lane != Some(c.0) && !in_ranges(c.0));
        let base_ctl = n09.is_none_or(|c| self.ctl == 0 || self.base_lane().0 != c.0)
            && (self.ctl == 0
                || (x_lane != Some(self.base_lane().0) && !in_ranges(self.base_lane().0)));
        for (i, n) in ns.iter().enumerate() {
            self.finish(n, ws[i], out, base_ctl);
            if let Some(vals) = rcc.get(i) {
                let x = &ws[i];
                let start = if self.lock.is_some_and(|l| l.2) {
                    i128::from(n.start)
                } else {
                    x.start
                };
                for &(cc, v) in vals {
                    if start >= 0 && x_lane != Some(Lane::Cc(cc)) {
                        out.emit_event(n.track, Lane::Cc(cc), x.ch, start, v);
                    }
                }
            }
            if let (Some(lane), Some(Some(v))) = (x_lane, xcc.get(i)) {
                let x = &ws[i];
                let start = if self.lock.is_some_and(|l| l.2) {
                    i128::from(n.start)
                } else {
                    x.start
                };
                if start >= 0 {
                    out.emit_event(n.track, lane, x.ch, start, *v);
                }
            }
            if let Some((lane, from, _, shape)) = n09 {
                let to = at(i).ctl.as_ref().map_or(0, |c| c.2);
                let x = &ws[i];
                let start = if self.lock.is_some_and(|l| l.2) {
                    i128::from(n.start)
                } else {
                    x.start
                };
                if start >= 0 {
                    let v = (*from as f64 + (to - *from) as f64 * shape.eval(pos(n, &src.stats)))
                        .round() as i64;
                    out.emit_event(n.track, *lane, x.ch, start, v);
                }
            }
            if i & 0xFFF == 0xFFF {
                out.check()?;
            }
        }
        out.check()?;
        self.end_run(out);
        if let Some(rows) = &self.ranges {
            let on = rows.iter().filter(|r| r.on).count();
            let m = format!(
                "Ranges: {} note(s) changed by {on} range(s).",
                crate::midi::group_digits(rcount)
            );
            out.message = Some(match out.message.take() {
                Some(c) => format!("{m} {c}"),
                None => m,
            });
        }
        if self.expr.is_some() {
            let m = xcount.message();
            out.message = Some(match out.message.take() {
                Some(c) => format!("{m} {c}"),
                None => m,
            });
        }
        Ok(())
    }

    /// n03: each note's entry of the list ([`place_index`]).
    fn cycle_index(&self, src: &Source, per: usize) -> Vec<u64> {
        place_index(src, &self.ctx.meter, per)
    }

    /// n01: the source property of a note on 0 … 1 — its key in the
    /// selection's range, velocity 1 … 127, length up to the longest
    /// written, its place in the bar, its subchannel's place in the list.
    fn source_value(&self, src: &Source, n: &TNote, x: &W, from: usize) -> f64 {
        let s = &src.stats;
        let frac = |v: f64, lo: f64, hi: f64| {
            if hi > lo {
                ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        match from {
            SRC_PITCH => frac(
                x.pitch as f64,
                f64::from(s.min_pitch),
                f64::from(s.max_pitch),
            ),
            SRC_VELOCITY => frac(x.vel as f64, 1.0, 127.0),
            SRC_LENGTH => frac(x.dur as f64, 0.0, s.max_dur as f64),
            SRC_BAR => {
                let t = u64::try_from(x.start.max(0)).unwrap_or(u64::MAX);
                let (bs, bl) = bar_bounds(&self.ctx.meter, t);
                frac(t.saturating_sub(bs) as f64, 0.0, bl as f64)
            }
            _ => {
                let (pos, len) = sub_place(src, n.track, x.sub);
                frac(pos as f64, 0.0, len.saturating_sub(1) as f64)
            }
        }
    }

    /// n10: the tested property — key, velocity, length (ticks), channel (1
    /// – 16), subchannel (place in the list), ticks after the bar line.
    fn if_value(&self, src: &Source, n: &TNote, x: &W, what: usize) -> i64 {
        match what {
            0 => x.pitch,
            1 => x.vel,
            2 => x.dur.min(i128::from(i64::MAX)) as i64,
            3 => i64::from(x.ch) + 1,
            4 => sub_place(src, n.track, x.sub).0 as i64,
            _ => {
                let t = u64::try_from(x.start.max(0)).unwrap_or(u64::MAX);
                let (bs, _) = bar_bounds(&self.ctx.meter, t);
                t.saturating_sub(bs).min(i64::MAX as u64) as i64
            }
        }
    }

    /// n06: per track (key, subchannel) in time order, each velocity moved
    /// toward the mean of the `window` notes around it (as many before as
    /// after, one more after for an even window) — prefix sums, O(n log n)
    /// for the grouping.
    fn smooth(
        &self,
        src: &Source,
        ws: &mut [W],
        window: usize,
        strength: impl Fn(usize) -> f64,
        per: usize,
    ) {
        let ns = &src.notes;
        let key = |i: usize| {
            let n = &ns[i];
            (
                n.track,
                match per {
                    PER_KEY => u32::from(n.pitch),
                    PER_SUB => n.sub,
                    _ => 0,
                },
            )
        };
        let mut order: Vec<usize> = (0..ns.len()).collect();
        order.sort_by_key(|&i| (key(i), ns[i].start, ns[i].pitch, i));
        let before = (window - 1) / 2;
        let after = window / 2;
        let mut prefix: Vec<i64> = Vec::new();
        let mut a = 0;
        while a < order.len() {
            let k = key(order[a]);
            let mut z = a + 1;
            while z < order.len() && key(order[z]) == k {
                z += 1;
            }
            let g = &order[a..z];
            prefix.clear();
            prefix.push(0);
            for &i in g {
                let last = *prefix.last().unwrap_or(&0);
                prefix.push(last + ws[i].vel);
            }
            for (j, &i) in g.iter().enumerate() {
                let lo = j.saturating_sub(before);
                let hi = (j + after + 1).min(g.len());
                let mean = (prefix[hi] - prefix[lo]) as f64 / (hi - lo) as f64;
                let v = ws[i].vel as f64;
                ws[i].vel = ((v + strength(i) * (mean - v)).round() as i64).clamp(1, 127);
            }
            a = z;
        }
    }

    /// n02: which notes start something (read from the written notes).
    fn accents(
        &self,
        src: &Source,
        rule: usize,
        leap: impl Fn(usize) -> i64,
        rest: u64,
    ) -> Vec<bool> {
        let ns = &src.notes;
        let mut hit = vec![false; ns.len()];
        match rule {
            ACCENT_DOWNBEATS => {
                for (i, n) in ns.iter().enumerate() {
                    hit[i] = bar_bounds(&self.ctx.meter, n.start).0 == n.start;
                }
            }
            ACCENT_PHRASES => {
                for idx in per_track(src) {
                    let ph = Phrases::of(ns, &idx, rest, None);
                    for part in ph.iter(&idx) {
                        let s = ns[part[0]].start;
                        for &i in part.iter().take_while(|&&i| ns[i].start == s) {
                            hit[i] = true;
                        }
                    }
                }
            }
            ACCENT_LEAPS => {
                for idx in per_track(src) {
                    let mut prev: Vec<i64> = Vec::new();
                    let mut cur: Vec<i64> = Vec::new();
                    let mut j = 0;
                    while j < idx.len() {
                        let s = ns[idx[j]].start;
                        let mut z = j;
                        cur.clear();
                        while z < idx.len() && ns[idx[z]].start == s {
                            let p = i64::from(ns[idx[z]].pitch);
                            cur.push(p);
                            if let Some(q) =
                                prev.iter().copied().min_by_key(|&q| (q.abs_diff(p), q))
                                && (p - q).abs() > leap(idx[z])
                            {
                                hit[idx[z]] = true;
                            }
                            z += 1;
                        }
                        std::mem::swap(&mut prev, &mut cur);
                        j = z;
                    }
                }
            }
            _ => {
                for idx in per_track(src) {
                    for chord in ChordGroups::of(ns, &idx, 0).iter() {
                        if chord.len() >= 2
                            && let Some(&top) = chord
                                .iter()
                                .max_by_key(|&&i| (ns[i].pitch, std::cmp::Reverse(i)))
                        {
                            hit[top] = true;
                        }
                    }
                }
            }
        }
        hit
    }
}

/// n07: values of `prop` (`NP_PROPS3`) beyond `k` standard deviations of
/// the mean (population) clamped to the bound or set to the median (the
/// lower middle).
fn outliers(ws: &mut [W], prop: usize, k: impl Fn(usize) -> f64, median: bool) {
    let n = ws.len();
    if n < 2 {
        return;
    }
    let vals: Vec<f64> = ws.iter().map(|x| x.prop(prop) as f64).collect();
    let mean = vals.iter().sum::<f64>() / n as f64;
    let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
    let sd = var.sqrt();
    if sd == 0.0 {
        return;
    }
    let mid = median.then(|| {
        let mut v: Vec<i64> = ws.iter().map(|x| x.prop(prop)).collect();
        let m = (n - 1) / 2;
        *v.select_nth_unstable(m).1
    });
    for (i, x) in ws.iter_mut().enumerate() {
        let k = k(i);
        let (lo, hi) = (mean - k * sd, mean + k * sd);
        let v = x.prop(prop) as f64;
        if v < lo || v > hi {
            let to = match mid {
                Some(m) => m,
                None if v < lo => lo.ceil() as i64,
                None => hi.floor() as i64,
            };
            x.set_target(prop, to);
        }
    }
}

/// n13 (B1-0078): applies the range rows to the written notes, top to
/// bottom — each row where its span covers the note (positions along time,
/// key, note order or velocity read from the selected notes themselves),
/// combined by its blend with what the rows above left. Only the properties
/// a row touches are rewritten (rounded half away from zero, then held in
/// their limits: ticks ≥ 0, gates ≥ 1, keys in the project's range,
/// velocities 1 – 127, channels 1 – 16, subchannels in the track's list).
/// Returns each note's controller values; counts the changed notes.
fn apply_ranges(
    src: &Source,
    ctx: &Ctx,
    rows: &[RangeRow],
    ws: &mut [W],
    changed: &mut u64,
) -> Result<Vec<Vec<(u8, i64)>>, TransformError> {
    let ns = &src.notes;
    let live: Vec<&RangeRow> = rows.iter().filter(|r| r.on).collect();
    if live.is_empty() || ns.is_empty() {
        return Ok(Vec::new());
    }
    let need = |a: Across| live.iter().any(|r| r.across == a);
    let st = &src.stats;
    let order: Vec<f64> = if need(Across::Order) {
        let mut o: Vec<usize> = (0..ns.len()).collect();
        o.sort_by_key(|&i| (ns[i].start, ns[i].pitch, ns[i].track, i));
        let mut r = vec![0.0; ns.len()];
        let last = (ns.len() - 1).max(1) as f64;
        for (k, &i) in o.iter().enumerate() {
            r[i] = k as f64 / last;
        }
        r
    } else {
        Vec::new()
    };
    let (vlo, vhi) = if need(Across::Velocity) {
        ns.iter()
            .fold((u8::MAX, 0u8), |(a, b), n| (a.min(n.vel), b.max(n.vel)))
    } else {
        (0, 0)
    };
    let frac = |v: f64, lo: f64, hi: f64| {
        if hi > lo {
            ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let ticks = live
        .iter()
        .any(|r| matches!(r.target, Target::Ticks | Target::Gates));
    let top = i128::from(ctx.keys.max_pitch());
    let any_cc = live
        .iter()
        .any(|r| matches!(r.target, Target::Controller(_)));
    let mut ccs: Vec<Vec<(u8, i64)>> = if any_cc {
        vec![Vec::new(); ns.len()]
    } else {
        Vec::new()
    };
    let mut cc_now: Vec<(u8, f64)> = Vec::new();
    for (i, (x, n)) in ws.iter_mut().zip(ns.iter()).enumerate() {
        if ticks && (x.start.abs() > X_MAX_TICK || x.dur > X_MAX_TICK) {
            return Err(TransformError::Invalid(format!(
                "Ranges compute ticks exactly only up to {} (2^53 − 1); the note at tick {} is beyond. Nothing was changed.",
                crate::midi::group_digits(X_MAX_TICK as u64),
                crate::midi::group_digits(n.start)
            )));
        }
        let at = |a: Across| match a {
            Across::Time => pos(n, st),
            Across::Key => frac(
                f64::from(n.pitch),
                f64::from(st.min_pitch),
                f64::from(st.max_pitch),
            ),
            Across::Order => order[i],
            Across::Velocity => frac(f64::from(n.vel), f64::from(vlo), f64::from(vhi)),
        };
        let base = *x;
        let (sc, subs) = sub_place(src, n.track, x.sub);
        // start, dur, pitch, vel, ch (1 – 16), sub (place)
        let mut v = [
            x.start as f64,
            x.dur as f64,
            x.pitch as f64,
            x.vel as f64,
            f64::from(x.ch) + 1.0,
            sc as f64,
        ];
        let b = v;
        let mut touched = [false; 6];
        cc_now.clear();
        for r in &live {
            let Some(y) = r.value_at(at(r.across)) else {
                continue;
            };
            let (k, offset) = match r.target {
                Target::Ticks => (0, true),
                Target::Gates => (1, false),
                Target::Transpose => (2, true),
                Target::Keys => (2, false),
                Target::Velocities => (3, false),
                Target::Channels => (4, false),
                Target::Subchannels => (5, false),
                Target::Controller(c) => {
                    let cur = cc_now.iter().find(|e| e.0 == c).map_or(0.0, |e| e.1);
                    let nv = r.blend.apply(cur, y, 0.0, false);
                    match cc_now.iter_mut().find(|e| e.0 == c) {
                        Some(e) => e.1 = nv,
                        None => cc_now.push((c, nv)),
                    }
                    continue;
                }
            };
            v[k] = r.blend.apply(v[k], y, b[k], offset);
            touched[k] = true;
        }
        let r = |f: f64| -> i128 {
            if f.is_finite() {
                f.round().clamp(-(X_MAX_TICK as f64), X_MAX_TICK as f64) as i128
            } else {
                0
            }
        };
        if touched[0] {
            x.start = r(v[0]).max(0);
        }
        if touched[1] {
            x.dur = r(v[1]).max(1);
        }
        if touched[2] {
            x.pitch = r(v[2]).clamp(0, top) as i64;
        }
        if touched[3] {
            x.vel = r(v[3]).clamp(1, 127) as i64;
        }
        if touched[4] {
            x.ch = (r(v[4]).clamp(1, 16) - 1) as u8;
        }
        if touched[5] && subs > 0 {
            x.sub = sub_at(
                src,
                n.track,
                r(v[5]).clamp(0, subs as i128 - 1) as i64,
                x.sub,
            );
        }
        if any_cc {
            ccs[i] = cc_now
                .iter()
                .map(|&(c, f)| (c, r(f).clamp(0, 127) as i64))
                .collect();
        }
        if (
            base.start, base.dur, base.pitch, base.vel, base.ch, base.sub,
        ) != (x.start, x.dur, x.pitch, x.vel, x.ch, x.sub)
            || ccs.get(i).is_some_and(|c| !c.is_empty())
        {
            *changed += 1;
        }
    }
    Ok(ccs)
}

/// n12 (B1-0075 – B1-0077): Note Properties' math expressions — one
/// formula per property in the pure expression engine (`editor::expr`: no
/// names but the variables, the constants and the listed functions; bounded
/// size; a result that is no finite number is an error, never a value).
#[derive(Clone, Debug)]
struct NpExpr {
    /// The fields of [`NP_X_FIELDS`]; `None` = empty (unchanged / no
    /// controller).
    f: [Option<Expr>; 7],
    /// The controller the controller value is written to.
    cc: u8,
    /// `NP_X_POLICY`.
    policy: usize,
    /// `NP_X_ROUND`.
    round: usize,
    seed: u64,
}

/// What n12 did, for the preview line (B1-0077).
#[derive(Default)]
struct XCount {
    changed: u64,
    clamped: u64,
    wrapped: u64,
    /// Notes left unchanged because a result was out of range.
    kept_range: u64,
    /// Notes left unchanged because a formula gave no number.
    kept_nan: u64,
    /// The first reason a note was left unchanged.
    first: Option<String>,
}

impl NpExpr {
    /// Parses the switched-on function's fields; a formula or constant
    /// that does not parse is an error naming its field (shown at once in
    /// the tool window).
    fn from(p: &Params) -> Result<Option<NpExpr>, TransformError> {
        if !p.flag("n12") {
            return Ok(None);
        }
        let consts = constants(&p.text("n12.consts"))?;
        let cref: Vec<(&str, f64)> = consts.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let mut f: [Option<Expr>; 7] = Default::default();
        for (slot, (key, label)) in f.iter_mut().zip(NP_X_FIELDS) {
            let src = p.text(key);
            if src.trim().is_empty() {
                continue;
            }
            *slot = Some(
                Expr::parse_with(&src, &NP_X_VARS, &cref)
                    .map_err(|e| TransformError::Invalid(format!("{label}: {e}.")))?,
            );
        }
        // Channel-mode messages (CC 120 – 127) are commands the controller
        // editor refuses to draw: refuse them here, before any preview, so
        // Preview = Apply (B1-S72 sweep, set 229).
        // The stored value, not the one clamped to the parameter's range: a
        // preset or project from before beta1 may hold CC 120 – 127.
        let ccn = match p.set.get_raw("n12.ccn") {
            Some(ParamValue::Int(v)) => *v,
            _ => p.int("n12.ccn"),
        };
        // Only when the Controller Value formula writes something.
        if f[6].is_some() && ccn > i64::from(ranges::MAX_CONTROLLER) {
            return Err(TransformError::Invalid(format!(
                "Controller number {ccn} is a channel-mode message (CC 120 – 127: All Notes Off and the like); the Controller Value formula writes CC 0 – {}.",
                ranges::MAX_CONTROLLER
            )));
        }
        Ok(Some(NpExpr {
            f,
            cc: p.int("n12.ccn").clamp(0, i64::from(ranges::MAX_CONTROLLER)) as u8,
            policy: p.choice("n12.policy"),
            round: p.choice("n12.round"),
            seed: p.seed("n12.seed"),
        }))
    }

    /// Whether any field reads variable `name`.
    fn uses(&self, name: &str) -> bool {
        self.f.iter().flatten().any(|e| e.uses(name))
    }

    /// Whether a controller value is written.
    fn writes_cc(&self) -> bool {
        self.f[6].is_some()
    }

    /// Runs the formulas over the written notes `ws` (all fields read the
    /// note's values before n12, as one simultaneous assignment), returns
    /// each note's controller value and fills `count`.
    fn run(
        &self,
        src: &Source,
        ctx: &Ctx,
        ws: &mut [W],
        out: &mut Output,
        count: &mut XCount,
    ) -> Result<Vec<Option<i64>>, TransformError> {
        let ns = &src.notes;
        let mut ccs = vec![None; if self.writes_cc() { ns.len() } else { 0 }];
        if self.f.iter().all(Option::is_none) {
            return Ok(ccs);
        }
        // index: the place in time order (start, key, track, then the
        // selection's order).
        let index: Vec<u64> = if self.uses("index") {
            let mut order: Vec<usize> = (0..ns.len()).collect();
            order.sort_by_key(|&i| (ns[i].start, ns[i].pitch, ns[i].track, i));
            let mut r = vec![0u64; ns.len()];
            for (k, &i) in order.iter().enumerate() {
                r[i] = k as u64;
            }
            r
        } else {
            Vec::new()
        };
        let exact_ticks = self.uses("t") || self.f[0].is_some();
        let exact_gates = self.uses("g") || self.f[1].is_some();
        let rnd = self.uses("rnd");
        let top = i128::from(ctx.keys.max_pitch());
        for (i, (x, n)) in ws.iter_mut().zip(ns.iter()).enumerate() {
            if i & 0xFFF == 0xFFF {
                out.check()?;
            }
            if (exact_ticks && x.start.abs() > X_MAX_TICK) || (exact_gates && x.dur > X_MAX_TICK) {
                return Err(TransformError::Invalid(format!(
                    "Math expressions compute ticks exactly only up to {} (2^53 − 1); the note at tick {} is beyond. Nothing was changed.",
                    crate::midi::group_digits(X_MAX_TICK as u64),
                    crate::midi::group_digits(u64::try_from(x.start.max(0)).unwrap_or(u64::MAX))
                )));
            }
            let (sc, subs) = sub_place(src, n.track, x.sub);
            let vals = [
                x.start as f64,
                x.dur as f64,
                x.pitch as f64,
                x.vel as f64,
                x.rel as f64,
                f64::from(x.ch) + 1.0,
                sc as f64,
                index.get(i).map_or(0.0, |&k| k as f64),
                ns.len() as f64,
                ctx.ppq as f64,
                pos(n, &src.stats),
                if rnd {
                    Rng::for_note(self.seed, salt::EXPR, n).unit()
                } else {
                    0.0
                },
            ];
            // Each field's range: start, gate, key, velocity, channel,
            // subchannel position (unchanged without a list), controller.
            let ranges: [(i128, i128); 7] = [
                (0, X_MAX_TICK),
                (1, X_MAX_TICK),
                (0, top),
                (1, 127),
                (1, 16),
                (0, subs.saturating_sub(1) as i128),
                (0, 127),
            ];
            let mut got = [0i128; 7];
            let mut keep: Option<String> = None;
            let (mut clamped, mut wrapped) = (0u64, 0u64);
            for (j, e) in self.f.iter().enumerate() {
                let Some(e) = e else { continue };
                if j == 5 && subs == 0 {
                    continue;
                }
                let r = match e.eval_vars(&vals) {
                    Ok(r) => r,
                    Err(err) => {
                        let why = format!("{}: {err}", NP_X_FIELDS[j].1);
                        if self.policy == NP_X_POLICY.len() - 1 {
                            return Err(TransformError::Invalid(format!(
                                "{why} (note at tick {}, key {}). Nothing was changed.",
                                crate::midi::group_digits(n.start),
                                n.pitch
                            )));
                        }
                        count.kept_nan += 1;
                        keep = Some(why);
                        break;
                    }
                };
                let v = whole(r, self.round);
                let (lo, hi) = ranges[j];
                if v >= lo as f64 && v <= hi as f64 {
                    got[j] = v as i128;
                    continue;
                }
                let wraps = matches!(j, 2..=6);
                match self.policy {
                    X_CLAMP => {
                        got[j] = if v < lo as f64 { lo } else { hi };
                        clamped += 1;
                    }
                    X_WRAP if wraps => {
                        // `v` may be huge: wrap in floating point (exact
                        // for the small ranges), then pin.
                        let size = (hi - lo + 1) as f64;
                        let w = (v - lo as f64).rem_euclid(size).floor() as i128;
                        got[j] = (lo + w).clamp(lo, hi);
                        wrapped += 1;
                    }
                    X_WRAP => {
                        got[j] = if v < lo as f64 { lo } else { hi };
                        clamped += 1;
                    }
                    X_KEEP => {
                        count.kept_range += 1;
                        keep = Some(format!(
                            "{} gives {} outside {lo} – {hi}",
                            NP_X_FIELDS[j].1,
                            fmt_value(v)
                        ));
                        break;
                    }
                    _ => {
                        return Err(TransformError::Invalid(format!(
                            "{} gives {} for the note at tick {}, key {} — outside {lo} – {hi}. Nothing was changed.",
                            NP_X_FIELDS[j].1,
                            fmt_value(v),
                            crate::midi::group_digits(n.start),
                            n.pitch
                        )));
                    }
                }
            }
            if let Some(why) = keep {
                if count.first.is_none() {
                    count.first = Some(format!(
                        "{why} (note at tick {}, key {})",
                        crate::midi::group_digits(n.start),
                        n.pitch
                    ));
                }
                continue;
            }
            count.clamped += clamped;
            count.wrapped += wrapped;
            let before = *x;
            for (j, e) in self.f.iter().enumerate() {
                if e.is_none() || (j == 5 && subs == 0) {
                    continue;
                }
                let v = got[j];
                match j {
                    0 => x.start = v,
                    1 => x.dur = v,
                    2 => x.pitch = v as i64,
                    3 => x.vel = v as i64,
                    4 => x.ch = (v - 1) as u8,
                    5 => x.sub = sub_at(src, n.track, v as i64, x.sub),
                    _ => ccs[i] = Some(v as i64),
                }
            }
            if (
                before.start,
                before.dur,
                before.pitch,
                before.vel,
                before.ch,
                before.sub,
            ) != (x.start, x.dur, x.pitch, x.vel, x.ch, x.sub)
            {
                count.changed += 1;
            }
        }
        Ok(ccs)
    }
}

impl XCount {
    /// The preview line: changed notes, adjusted values, skipped notes.
    fn message(&self) -> String {
        let g = crate::midi::group_digits;
        let mut s = format!("Math expressions: {} note(s) changed", g(self.changed));
        if self.clamped > 0 {
            s.push_str(&format!(", {} value(s) clamped", g(self.clamped)));
        }
        if self.wrapped > 0 {
            s.push_str(&format!(", {} value(s) wrapped", g(self.wrapped)));
        }
        if self.kept_range > 0 {
            s.push_str(&format!(
                ", {} left unchanged (out of range)",
                g(self.kept_range)
            ));
        }
        if self.kept_nan > 0 {
            s.push_str(&format!(
                ", {} left unchanged (no number)",
                g(self.kept_nan)
            ));
        }
        s.push('.');
        if let Some(f) = &self.first {
            s.push_str(&format!(" First: {f}."));
        }
        s
    }
}

/// n12: a result as a whole number by the rounding mode (`NP_X_ROUND`); a
/// value within rounding noise of a whole number is that number (0.1 * 30
/// is 3 in every mode).
fn whole(x: f64, mode: usize) -> f64 {
    let n = x.round();
    if (x - n).abs() <= (x.abs() * 4.0 * f64::EPSILON).max(1e-9) {
        return n;
    }
    match mode {
        0 => n,
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}

fn fmt_value(v: f64) -> String {
    if v.abs() < 1e15 {
        format!("{}", v as i128)
    } else {
        format!("{v:e}")
    }
}

/// n12: the user constants `name = value; …` (values are constant
/// expressions over numbers, pi, e and earlier constants).
fn constants(text: &str) -> Result<Vec<(String, f64)>, TransformError> {
    let mut out: Vec<(String, f64)> = Vec::new();
    let bad = |m: String| TransformError::Invalid(format!("Constants: {m}."));
    for part in text.split([';', '\n']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, value)) = part.split_once('=') else {
            return Err(bad(format!("\"{part}\" needs the form name = value")));
        };
        let name = name.trim().to_ascii_lowercase();
        if !valid_name(&name) || NP_X_VARS.contains(&name.as_str()) {
            return Err(bad(format!(
                "\"{name}\" cannot be a constant's name (letters, digits and _, not a variable, function, pi or e)"
            )));
        }
        if out.iter().any(|(n, _)| *n == name) {
            return Err(bad(format!("\"{name}\" is defined twice")));
        }
        if out.len() == X_MAX_CONSTS {
            return Err(bad(format!("at most {X_MAX_CONSTS} constants")));
        }
        let cref: Vec<(&str, f64)> = out.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let v = Expr::parse_with(value, &[], &cref)
            .and_then(|e| e.eval_vars(&[]))
            .map_err(|e| bad(format!("{name}: {e}")))?;
        out.push((name, v));
    }
    Ok(out)
}

fn note_props_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let set = |k: &str| p.flag(k);
    let grp = set("grp.on").then(|| p.int("grp") as u32);
    // A group id that does not exist yet is created (named after its id).
    let new_table = grp
        .filter(|&g| g != 0 && b.ctx.groups.get(g).is_none())
        .map(|g| {
            let mut t = (*b.ctx.groups).clone();
            t.insert(crate::model::GroupInfo {
                id: g,
                ..Default::default()
            });
            t
        });
    let np = Np {
        vel: set("vel.on").then(|| p.int("vel")),
        rel: set("rel.on").then(|| p.int("rel")),
        pit: set("pitch.on").then(|| p.int("pitch")),
        ch: set("ch.on").then(|| (p.int("ch") - 1) as u8),
        sub: set("sub.on").then(|| p.int("sub") as usize),
        suboff: set("suboff.on").then(|| p.int("suboff")),
        start: set("start.on").then(|| p.ticks("start")),
        dur: set("dur.on").then(|| p.ticks("dur")),
        grp,
        new_table,
        ctl: p.choice("ctl"),
        ctl_val: p.int("ctl.val"),
        rel_mode: p.adv(1).then(|| {
            (
                p.int("a1.vel"),
                p.int("a1.rel"),
                p.int("a1.pitch"),
                i128::from(p.int("a1.start")),
                i128::from(p.int("a1.dur")),
            )
        }),
        scale: p
            .adv(2)
            .then(|| (p.float("a2.vel"), p.float("a2.rel"), p.float("a2.dur"))),
        grad: p.adv(3).then(|| {
            (
                p.choice("a3.prop"),
                p.int("a3.from"),
                p.int("a3.to"),
                p.curve("a3.curve"),
            )
        }),
        copy: p.adv(4).then(|| (p.choice("a4.from"), p.choice("a4.what"))),
        norm: p.adv(5).then(|| (p.choice("a5.mode"), p.int("a5.target"))),
        hist: p
            .adv(6)
            .then(|| (p.choice("a6.prop"), p.int("a6.lo"), p.int("a6.hi"))),
        lock: p.adv(7).then(|| {
            (
                p.flag("a7.vel"),
                p.flag("a7.pitch"),
                p.flag("a7.start"),
                p.flag("a7.dur"),
            )
        }),
        table: p.adv(8).then(|| (p.choice("a8.prop"), p.list("a8.table"))),
        rnd: p.adv(9).then(|| {
            (
                p.choice("a9.prop"),
                p.int("a9.lo"),
                p.int("a9.hi"),
                p.seed("a9.seed"),
            )
        }),
        mac: p.adv(10).then(|| p.choice("a10.macro")),
        ppq: b.ctx.ppq,
        new: NpNew::from(p),
        order: p.sequence(crate::transform::order::NOTE_PROPS_PASSES.members),
        expr: NpExpr::from(p)?.map(std::sync::Arc::new),
        ranges: if p.flag("n13") {
            Some(std::sync::Arc::new(
                ranges::parse(&p.text("n13.rows")).map_err(TransformError::Invalid)?,
            ))
        } else {
            None
        },
        ctx: b.ctx.clone(),
        swept: Rebuild::of(p, b),
    };
    if np.new.any() || np.swept.is_some() || np.expr.is_some() || np.ranges.is_some() {
        // With Control Sweeps always the v0.9 pipeline (E0422; with every
        // function off it is the v0.8 result).
        // V09-S36: at most one base controller point and one n09 point per
        // note (B1-0075: and one n12 point).
        let events = (b.stats.count as u64).saturating_mul(
            u64::from(np.ctl > 0)
                + u64::from(np.new.ctl.is_some())
                + u64::from(np.expr.as_ref().is_some_and(|x| x.writes_cc()))
                + np.ranges.as_ref().map_or(0, |r| {
                    let mut l: Vec<u8> = r
                        .iter()
                        .filter(|r| r.on)
                        .filter_map(|r| match r.target {
                            Target::Controller(n) => Some(n),
                            _ => None,
                        })
                        .collect();
                    l.sort_unstable();
                    l.dedup();
                    l.len() as u64
                }),
        );
        if events > MAX_EVENTS as u64 {
            return Err(TransformError::TooLarge(format!(
                "The controller points would be about {}; select fewer notes.",
                crate::midi::group_digits(events)
            )));
        }
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| np.run9(src, out),
            growth: Growth::Map,
            estimate: None,
        });
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| np.run8(src, out),
        growth: Growth::Map,
        estimate: None,
    })
}

// ======================================================================= Mute

/// The one-click Mute Notes / Unmute Notes / Mute / Unmute toggle commands
/// (v0.8 §199, v0.9 §117) as a platform kernel (Preview, Undo, background
/// runs come from the platform). Not a tool: no window, no advanced
/// functions, not in any tool menu — v0.9 removed the rule-based Advanced
/// Mute (`mute`, see `transform::removed_tool`).
pub const MUTE_NOTES: ToolDef = ToolDef {
    id: "mute_notes",
    spec: mute_spec,
    build: mute_build,
};

fn mute_spec() -> ToolSpec {
    let mut p = vec![choice("action", "Action", &["Mute", "Unmute", "Toggle"], 0)
        .help("Mute silences the selected notes, Unmute makes them play again, Toggle flips each note's state.")];
    p.extend(boundary::specs(PitchPolicy::Discard));
    ToolSpec {
        id: "mute_notes",
        name: "Mute Notes",
        family: Family::Interactive,
        summary: "Mutes, unmutes or toggles the selected notes. Muted notes stay in the project, draw hollow and do not play, export or count. This is not Track Mute.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

fn mute_build(p: &Params, _b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let action = p.choice("action");
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let mut changed = 0u64;
            for n in &src.notes {
                let mut t = *n;
                let muted = match action {
                    0 => true,
                    1 => false,
                    _ => !n.muted(),
                };
                if muted != n.muted() {
                    changed += 1;
                }
                t.flags = (t.flags & !FLAG_MUTED) | if muted { FLAG_MUTED } else { 0 };
                out.emit(
                    i128::from(t.start),
                    i128::from(t.dur),
                    i64::from(t.pitch),
                    i64::from(t.vel),
                    &t,
                );
                out.check()?;
            }
            out.message = Some(format!(
                "{} note(s) change mute state.",
                crate::midi::group_digits(changed)
            ));
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}

// ================================================================ Join / Glue

pub const JOIN: ToolDef = ToolDef {
    id: "join",
    spec: join_spec,
    build: join_build,
};

const JOIN_ALIGN: &[&str] = &[
    "The grid (from each bar line)",
    "The chain's first fragment",
];
const JOIN_PIECES: &[&str] = &["Equal", "Greedy"];
const JOIN_VEL_REF: &[&str] = &["Previous fragment", "Chain start"];
const JOIN_BOUNDS: &[&str] = &["Bar lines", "Beats", "Grid lines"];
const JOIN_CC: &[&str] = &["CC 11 expression", "CC 7 volume", "CC 1 modulation"];
const JOIN_KEEP: &[&str] = &["First track", "Active track"];
const JOIN_VEL_FROM: &[&str] = &["Loudest", "First track's"];
const JOIN_INTO: &[&str] = &["Previous", "Next", "Longer neighbor"];
const JOIN_MATCH: &[&str] = &["Same pitches", "Same pitch classes", "Same bass and top"];
const JOIN_INTERRUPT: &[&str] = &["Of the track", "Of any track"];
const JOIN_FIRST: &[&str] = &["Shortest gaps", "Earliest"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const JOIN_HELP: &[(&str, &str)] = &[
    (
        "a1",
        "Also joins fragments separated by a small gap instead of only touching ones.",
    ),
    (
        "a1.gap",
        "The largest gap between fragments that is still joined, in ticks.",
    ),
    ("a2", "Also joins fragments that overlap each other."),
    (
        "a3",
        "Gives the joined note the average velocity and release of its fragments.",
    ),
    (
        "a4",
        "Takes the joined note's properties from another fragment than the first.",
    ),
    (
        "a4.which",
        "Which fragment gives the velocity, release and channel: the last one or the loudest one.",
    ),
    (
        "a5",
        "Gives the joined note the velocity of its fragments averaged by their lengths.",
    ),
    (
        "a6",
        "Lets fragments on different subchannels join, putting the joined note on one subchannel.",
    ),
    (
        "a6.target",
        "The subchannel the joined notes go to, by its position in the track's list (0 = the first).",
    ),
    ("a7", "Lets fragments in different note groups join."),
    (
        "a8",
        "Joins the fragments in groups of a set size instead of the whole chain.",
    ),
    ("a8.n", "How many fragments make up each joined note."),
    (
        "a9",
        "Also joins fragments across a longer gap, as long as no other pitch plays in between on the same line.",
    ),
    ("a9.gap", "The largest gap joined this way, in ticks."),
    (
        "a10",
        "Gives the joined note the properties of its longest fragment.",
    ),
    (
        "n01",
        "Joins the fragments that fall in the same time window into one note per window.",
    ),
    ("n01.window", "The length of each window."),
    (
        "n01.ticks",
        "The window length in ticks, used when Window is set to Custom Ticks.",
    ),
    (
        "n01.align",
        "Where the windows are counted from: the grid from each bar line, or each chain's first fragment.",
    ),
    (
        "n02",
        "Limits how long a joined note may get, splitting longer runs into pieces.",
    ),
    ("n02.max", "The longest a joined note may be, in ticks."),
    (
        "n02.pieces",
        "Equal splits a long run into pieces of equal length; Greedy makes each piece as long as allowed.",
    ),
    ("n03", "Joins only fragments whose velocities are close."),
    (
        "n03.diff",
        "The largest velocity difference that is still joined.",
    ),
    (
        "n03.ref",
        "Compares each fragment with the previous fragment or with the first fragment of the run.",
    ),
    (
        "n04",
        "Joins fragments only as much as needed to bring the notes per second down to a target.",
    ),
    ("n04.nps", "The notes per second the joining aims for."),
    (
        "n04.window",
        "The time over which the notes per second are counted, in milliseconds.",
    ),
    (
        "n04.gap",
        "The largest gap that may be joined to reach the target, in ticks.",
    ),
    (
        "n05",
        "Joins fragments that were split at bar lines, beats or grid lines, like tied notes.",
    ),
    (
        "n05.at",
        "The lines the ties are found at: bar lines, beats or grid lines.",
    ),
    ("n05.grid", "The grid used when Boundary is Grid Lines."),
    (
        "n05.tol",
        "How far from the line the fragments may meet and still be tied, in ticks.",
    ),
    (
        "n06",
        "Writes the fragments' velocities as a controller curve, so the joined note keeps the loudness changes.",
    ),
    (
        "n06.cc",
        "The controller the velocities go to: expression (CC 11), volume (CC 7) or modulation (CC 1).",
    ),
    (
        "n06.scale",
        "How strongly the velocities are written to the controller.",
    ),
    (
        "n07",
        "Joins identical notes that sit on several tracks at the same time into one.",
    ),
    (
        "n07.keep",
        "Which track keeps the joined note: the first track or the active track.",
    ),
    (
        "n07.vel",
        "The joined note's velocity: the loudest of the copies, or the first track's.",
    ),
    (
        "n07.tol",
        "How far apart the copies may start and still be joined, in ticks.",
    ),
    (
        "n08",
        "Merges very short fragments into a neighboring note.",
    ),
    (
        "n08.short",
        "Fragments shorter than this are merged, in ticks.",
    ),
    (
        "n08.into",
        "Which neighbor takes a short fragment: the previous note, the next note, or the longer of the two.",
    ),
    (
        "n08.gap",
        "The largest gap to the neighbor that is still merged, in ticks.",
    ),
    (
        "n09",
        "Joins a chord that repeats right away into one long chord.",
    ),
    (
        "n09.gap",
        "The largest gap between the repeated chords, in ticks.",
    ),
    (
        "n09.match",
        "When two chords count as the same: the same pitches, the same pitch classes, or the same bass and top notes.",
    ),
    (
        "n09.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
    (
        "n10",
        "Joins fragments across a rest as long as no other note plays in between.",
    ),
    (
        "n10.of",
        "Which notes interrupt a join: those of the same track or of any track.",
    ),
    (
        "n10.rest",
        "The longest rest that is still joined, in ticks.",
    ),
    (
        "n11",
        "Joins fragments until each bar has at most a set number of them.",
    ),
    ("n11.most", "The most fragments a bar may keep."),
    (
        "n11.first",
        "Which joins happen first: across the shortest gaps or the earliest fragments.",
    ),
];

fn join_spec() -> ToolSpec {
    let mut p = vec![
        adv_switch(1, "Gap tolerance"),
        ticks("a1.gap", "Largest gap joined (ticks)", 1, 1 << 40, 60).adv(1),
        adv_switch(2, "Overlap union"),
        adv_switch(3, "Property blend (average)"),
        adv_switch(4, "First / last policy"),
        choice(
            "a4.which",
            "Properties from",
            &["Last fragment", "Loudest fragment"],
            0,
        )
        .adv(4),
        adv_switch(5, "Velocity-weighted merge"),
        adv_switch(6, "Cross-subchannel join with explicit target"),
        int("a6.target", "Target subchannel (position)", 0, 4095, 0).adv(6),
        adv_switch(7, "Join across note groups"),
        adv_switch(8, "Periodic chain join"),
        int("a8.n", "Join every n fragments", 2, 64, 2).adv(8),
        adv_switch(9, "Contour-aware join"),
        ticks(
            "a9.gap",
            "Largest gap inside the line (ticks)",
            1,
            1 << 40,
            480,
        )
        .adv(9),
        adv_switch(10, "Join to longest envelope"),
        // V09-S36 step 5: the functions declared by id (docs/TRANSFORM_ENGINE.md
        // "Join").
        flag("n01", "Window join", false),
        choice("n01.window", "Window", NOTE_VALUES, 2),
        ticks("n01.ticks", "Window ticks (Custom ticks)", 1, 1 << 40, 480),
        choice("n01.align", "Align windows to", JOIN_ALIGN, 0),
        flag("n02", "Longest result", false),
        ticks("n02.max", "Longest result (ticks)", 1, 1 << 40, 1920),
        choice("n02.pieces", "Pieces", JOIN_PIECES, 0),
        flag("n03", "Velocity similarity", false),
        int("n03.diff", "Largest velocity difference", 0, 126, 12),
        choice("n03.ref", "Compare to", JOIN_VEL_REF, 0),
        flag("n04", "NPS target join", false),
        int("n04.nps", "Target notes per second", 1, 10_000_000, 1000),
        float("n04.window", "Window", 10.0, 5000.0, 1000.0, "ms"),
        ticks("n04.gap", "Largest gap joined (ticks)", 0, 1 << 40, 240),
        flag("n05", "Boundary ties", false),
        choice("n05.at", "Boundary", JOIN_BOUNDS, 0),
        choice("n05.grid", "Grid", super::art_grid_values(), 4),
        ticks("n05.tol", "Tolerance (ticks)", 0, 1 << 40, 10),
        flag("n06", "Velocity to expression", false),
        choice("n06.cc", "Controller", JOIN_CC, 0),
        float("n06.scale", "Scale", 0.0, 200.0, 100.0, "%"),
        flag("n07", "Cross-track join", false),
        choice("n07.keep", "Keep on", JOIN_KEEP, 0),
        choice("n07.vel", "Velocity", JOIN_VEL_FROM, 0),
        ticks("n07.tol", "Start tolerance (ticks)", 0, 1 << 40, 0),
        flag("n08", "Absorb short fragments", false),
        ticks("n08.short", "Shorter than (ticks)", 1, 1 << 40, 60),
        choice("n08.into", "Into", JOIN_INTO, 2),
        ticks(
            "n08.gap",
            "Largest gap to the neighbor (ticks)",
            0,
            1 << 40,
            120,
        ),
        flag("n09", "Repeated chord join", false),
        ticks("n09.gap", "Largest gap (ticks)", 0, 1 << 40, 240),
        choice("n09.match", "Match", JOIN_MATCH, 0),
        ticks("n09.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n10", "Uninterrupted join", false),
        choice("n10.of", "Interrupting notes", JOIN_INTERRUPT, 0),
        ticks("n10.rest", "Largest rest (ticks)", 0, 1 << 40, 960),
        flag("n11", "Per-bar fragment limit", false),
        int("n11.most", "Most fragments per bar", 1, 1024, 4),
        choice("n11.first", "Join first", JOIN_FIRST, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, JOIN_HELP);
    ToolSpec {
        id: "join",
        name: "Join / Glue",
        family: Family::Editor,
        summary: "Merges fragments of one note chain — same track, pitch, subchannel, MIDI channel, note group and mute state — that touch (end = next start; Gap tolerance: within the gap) into one note from the earliest start to the latest end, with the first fragment's velocity and properties. Different pitches never merge (C, D, E stay three notes); Overlap union also merges overlapping fragments.",
        params: p,
        advanced: vec![
            "Gap tolerance",
            "Overlap union",
            "Property blend (average)",
            "First / last policy",
            "Velocity-weighted merge",
            "Cross-subchannel join with explicit target",
            "Join across note groups",
            "Periodic chain join",
            "Contour-aware join",
            "Join to longest envelope",
        ],
        // Orders = where each runs in `Join::run9` (§235).
        functions: [
            ("n01", "Window join", 110),
            ("n02", "Longest result", 170),
            ("n03", "Velocity similarity", 160),
            ("n04", "NPS target join", 190),
            ("n05", "Boundary ties", 120),
            ("n06", "Velocity to expression", 510),
            ("n07", "Cross-track join", 200),
            ("n08", "Absorb short fragments", 130),
            ("n09", "Repeated chord join", 140),
            ("n10", "Uninterrupted join", 150),
            ("n11", "Per-bar fragment limit", 180),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

fn join_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let v8 = JoinV08::from(p);
    let new = JoinNew::from(p, b.ctx);
    // With Control Sweeps always the v0.9 pipeline (E0422).
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        if new.expr.is_some() {
            // At most a point per fragment and a resting point per joined
            // note (two fragments or more).
            let events = (b.stats.count as u64).saturating_mul(3) / 2;
            if events > MAX_EVENTS as u64 {
                return Err(TransformError::TooLarge(format!(
                    "The controller points could be about {}; select fewer notes.",
                    crate::midi::group_digits(events)
                )));
            }
        }
        let j = Join {
            v8,
            new,
            ctx: b.ctx.clone(),
            swept: Rebuild::of(p, b),
        };
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| j.run9(src, out),
            growth: Growth::Thin,
            estimate: None,
        });
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| v8.run8(src, out),
        growth: Growth::Thin,
        estimate: None,
    })
}

/// The v0.8 Join rule — the chains, the run of fragments joined from each
/// fragment, the joined note — shared by the v0.8 kernel ([`JoinV08::run8`])
/// and the v0.9 pipeline ([`Join::run9`]); V09-S36 step 5 moved it here
/// unchanged (inlined, same arithmetic).
#[derive(Clone, Copy, Debug)]
struct JoinV08 {
    gap: u64,
    union: bool,
    blend: bool,
    which: Option<usize>,
    weighted: bool,
    cross: Option<usize>,
    across_groups: bool,
    every: Option<usize>,
    contour: Option<u64>,
    longest: bool,
}

/// A fragment chain: track, subchannel, MIDI channel, note group, mute flag,
/// pitch.
type JoinKey = (u16, u32, u8, u32, u8, u8);

/// Onsets for the contour rule: track, subchannel, start, pitch (sorted).
type JoinLine = Vec<(u16, u32, u64, u8)>;

impl JoinV08 {
    fn from(p: &Params) -> JoinV08 {
        JoinV08 {
            gap: if p.adv(1) { p.ticks("a1.gap") } else { 0 },
            union: p.adv(2),
            blend: p.adv(3),
            which: p.adv(4).then(|| p.choice("a4.which")),
            weighted: p.adv(5),
            cross: p.adv(6).then(|| p.int("a6.target") as usize),
            // v0.8 §193: fragments of different note groups are not one
            // chain unless explicitly joined across groups.
            across_groups: p.adv(7),
            every: p.adv(8).then(|| p.int("a8.n") as usize),
            contour: p.adv(9).then(|| p.ticks("a9.gap")),
            longest: p.adv(10),
        }
    }

    /// The chain of a fragment.
    #[inline(always)]
    fn key(&self, n: &TNote) -> JoinKey {
        (
            n.track,
            if self.cross.is_some() { 0 } else { n.sub },
            if self.cross.is_some() { 0 } else { n.ch },
            if self.across_groups { 0 } else { n.group },
            n.flags & FLAG_MUTED,
            n.pitch,
        )
    }

    /// The chain key packed high to low in [`JoinV08::key`]'s tuple order
    /// (104 bits).
    #[inline(always)]
    fn packed(&self, n: &TNote) -> u128 {
        let (t, s, c, g, m, p) = self.key(n);
        u128::from(t) << 88
            | u128::from(s) << 56
            | u128::from(c) << 48
            | u128::from(g) << 16
            | u128::from(m) << 8
            | u128::from(p)
    }

    /// The fragments in chain order: by chain, start, index — a total
    /// order, so sorting packed values (no reads of the notes while
    /// sorting; V09-S36 step 5) gives v0.8's stable sort by key exactly.
    #[inline(always)]
    fn order(&self, ns: &[TNote]) -> Vec<usize> {
        let mut keyed: Vec<(u128, u64, usize)> = ns
            .iter()
            .enumerate()
            .map(|(i, n)| (self.packed(n), n.start, i))
            .collect();
        keyed.sort_unstable();
        keyed.into_iter().map(|k| k.2).collect()
    }

    /// Contour: per (track, sub) the onsets of other pitches, to refuse
    /// joins across a melodic move (empty without [a9]).
    #[inline(always)]
    fn line(&self, ns: &[TNote]) -> JoinLine {
        let mut line: JoinLine = Vec::new();
        if self.contour.is_some() {
            line = ns
                .iter()
                .map(|n| {
                    (
                        n.track,
                        if self.cross.is_some() { 0 } else { n.sub },
                        n.start,
                        n.pitch,
                    )
                })
                .collect();
            line.sort_unstable();
        }
        line
    }

    #[inline(always)]
    fn moved_between(line: &JoinLine, t: u16, s: u32, a: u64, b: u64, pitch: u8) -> bool {
        let lo = line.partition_point(|x| (x.0, x.1, x.2) <= (t, s, a));
        let hi = line.partition_point(|x| (x.0, x.1, x.2) < (t, s, b));
        line[lo..hi.max(lo)].iter().any(|x| x.3 != pitch)
    }

    /// The run of fragments joined from position `i` of the chain order
    /// (`at(p)`: the fragment at position `p`, `len` of them): its end `j`
    /// (exclusive), its latest end and its last fragment's end.
    #[inline(always)]
    fn extend<'a>(
        &self,
        at: impl Fn(usize) -> &'a TNote,
        len: usize,
        line: &JoinLine,
        i: usize,
    ) -> (usize, u64, u64) {
        let first = *at(i);
        let k = self.key(&first);
        let mut j = i + 1;
        let mut end = first.end();
        let mut last_end = first.end();
        while j < len && self.key(at(j)) == k {
            let n = at(j);
            if self.every.is_some_and(|e| (j - i) >= e) {
                break;
            }
            let reach = end.saturating_add(self.gap);
            // Overlapping fragments join only with Overlap union.
            if n.start < end && !self.union {
                break;
            }
            let ok = n.start <= reach
                || self.contour.is_some_and(|g| {
                    n.start <= end.saturating_add(g)
                        && !Self::moved_between(line, k.0, k.1, end, n.start, k.5)
                });
            if !ok {
                break;
            }
            end = end.max(n.end());
            last_end = n.end();
            j += 1;
        }
        (j, end, last_end)
    }

    /// The note joined from `frags` (chain order; the first gives the
    /// start, `frags[lead]` the properties unless a function picks another
    /// fragment) and its end.
    #[inline(always)]
    fn joined(
        &self,
        src: &Source,
        frags: &[TNote],
        lead: usize,
        end: u64,
        last_end: u64,
    ) -> (TNote, u64) {
        let first = frags[0];
        let stop = last_end.max(end).max(first.start + 1);
        let mut model = frags[lead];
        if let Some(w) = self.which {
            model = if w == 0 {
                *frags.last().expect("non-empty")
            } else {
                *frags.iter().max_by_key(|f| f.vel).expect("non-empty")
            };
        }
        if self.longest {
            model = *frags.iter().max_by_key(|f| f.dur).expect("non-empty");
        }
        let mut t = frags[lead];
        t.vel = model.vel;
        t.rel = model.rel;
        t.ch = model.ch;
        if self.blend {
            let c = frags.len() as u64;
            t.vel = (frags.iter().map(|f| u64::from(f.vel)).sum::<u64>() / c) as u8;
            t.rel = (frags.iter().map(|f| u64::from(f.rel)).sum::<u64>() / c) as u8;
        }
        if self.weighted {
            let tot: u128 = frags.iter().map(|f| u128::from(f.dur)).sum();
            let s: u128 = frags
                .iter()
                .map(|f| u128::from(f.dur) * u128::from(f.vel))
                .sum();
            t.vel = (s / tot.max(1)).clamp(1, 127) as u8;
        }
        if let Some(target) = self.cross
            && frags.len() > 1
            && let Some(tr) = src.track(first.track)
            && !tr.subs.is_empty()
        {
            let (id, ch) = tr.subs[target.min(tr.subs.len() - 1)];
            t.sub = id;
            t.ch = ch;
        }
        (t, stop)
    }

    /// The v0.8 kernel: each chain's runs in turn.
    fn run8(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let ns = &src.notes;
        let order = self.order(ns);
        let line = self.line(ns);
        let mut i = 0;
        let mut joined = 0u64;
        let mut frags: Vec<TNote> = Vec::new();
        while i < order.len() {
            let (j, end, last_end) = self.extend(|p| &ns[order[p]], order.len(), &line, i);
            frags.clear();
            frags.extend(order[i..j].iter().map(|&x| ns[x]));
            let (t, stop) = self.joined(src, &frags, 0, end, last_end);
            let first = frags[0];
            joined += (frags.len() - 1) as u64;
            out.emit(
                i128::from(first.start),
                i128::from(stop.saturating_sub(first.start)),
                i64::from(t.pitch),
                i64::from(t.vel),
                &t,
            );
            out.check()?;
            i = j;
        }
        out.message = Some(format!(
            "{} fragment(s) joined.",
            crate::midi::group_digits(joined)
        ));
        Ok(())
    }
}

/// Join's v0.9 functions (V09-S36 step 5), declared by id.
#[derive(Clone, Debug)]
struct JoinNew {
    /// n01: window ticks; aligned to the chain's first fragment (else to
    /// the bar lines).
    window: Option<(u64, bool)>,
    /// n02: longest result; greedy pieces (else equal ones).
    longest: Option<(u64, bool)>,
    /// n03: largest velocity difference; against the run's first fragment
    /// (else the previous fragment).
    similar: Option<(u8, bool)>,
    /// n04: starts per window, the window (µs), the largest gap joined.
    nps: Option<(usize, u64, u64)>,
    /// n05: boundary (`JOIN_BOUNDS`), grid ticks, tolerance.
    ties: Option<(usize, u64, u64)>,
    /// n06: lane, its resting value, scale (fraction).
    expr: Option<(Lane, i64, f64)>,
    /// n07: keep on the active track (else the first); the first track's
    /// velocity (else the loudest); start tolerance.
    across: Option<(bool, bool, u64)>,
    /// n08: shorter than, into (`JOIN_INTO`), largest gap.
    absorb: Option<(u64, usize, u64)>,
    /// n09: largest gap, match (`JOIN_MATCH`), chord tolerance.
    chords: Option<(u64, usize, u64)>,
    /// n10: notes of any track interrupt (else of the track), largest rest.
    rests: Option<(bool, u64)>,
    /// n11: most fragments per bar; earliest first (else shortest gaps).
    per_bar: Option<(usize, bool)>,
}

impl JoinNew {
    fn from(p: &Params, ctx: &Ctx) -> JoinNew {
        JoinNew {
            window: p.flag("n01").then(|| {
                (
                    note_value(p.choice("n01.window"), p.ticks("n01.ticks"), ctx),
                    p.choice("n01.align") == 1,
                )
            }),
            longest: p
                .flag("n02")
                .then(|| (p.ticks("n02.max").max(1), p.choice("n02.pieces") == 1)),
            similar: p.flag("n03").then(|| {
                (
                    p.int("n03.diff").clamp(0, 126) as u8,
                    p.choice("n03.ref") == 1,
                )
            }),
            nps: p.flag("n04").then(|| {
                let nps = p.int("n04.nps").max(1) as f64;
                let w = (p.float("n04.window") * 1000.0).round().max(1.0) as u64;
                (
                    ((nps * w as f64 / 1e6).floor() as usize).max(1),
                    w,
                    p.ticks("n04.gap"),
                )
            }),
            ties: p.flag("n05").then(|| {
                (
                    p.choice("n05.at"),
                    note_value(p.choice("n05.grid"), 1, ctx),
                    p.ticks("n05.tol"),
                )
            }),
            expr: p.flag("n06").then(|| {
                let (lane, rest) = match p.choice("n06.cc") {
                    1 => (Lane::Cc(7), 100),
                    2 => (Lane::Cc(1), 0),
                    _ => (Lane::Cc(11), 127),
                };
                (lane, rest, p.float("n06.scale").max(0.0) / 100.0)
            }),
            across: p.flag("n07").then(|| {
                (
                    p.choice("n07.keep") == 1,
                    p.choice("n07.vel") == 1,
                    p.ticks("n07.tol"),
                )
            }),
            absorb: p.flag("n08").then(|| {
                (
                    p.ticks("n08.short"),
                    p.choice("n08.into"),
                    p.ticks("n08.gap"),
                )
            }),
            chords: p.flag("n09").then(|| {
                (
                    p.ticks("n09.gap"),
                    p.choice("n09.match"),
                    p.ticks("n09.tol"),
                )
            }),
            rests: p
                .flag("n10")
                .then(|| (p.choice("n10.of") == 1, p.ticks("n10.rest"))),
            per_bar: p.flag("n11").then(|| {
                (
                    p.int("n11.most").clamp(1, 1024) as usize,
                    p.choice("n11.first") == 1,
                )
            }),
        }
    }

    fn any(&self) -> bool {
        self.window.is_some()
            || self.longest.is_some()
            || self.similar.is_some()
            || self.nps.is_some()
            || self.ties.is_some()
            || self.expr.is_some()
            || self.across.is_some()
            || self.absorb.is_some()
            || self.chords.is_some()
            || self.rests.is_some()
            || self.per_bar.is_some()
    }
}

/// A run of joined fragments: positions `[a, b)` in chain order, its start
/// and its end (the latest end, at least start + 1).
type JoinRun = (usize, usize, u64, u64);

/// The runs of `link` (`link[p]`: fragment `p` of `c`, the fragments in
/// chain order, joins the run before it).
fn join_runs(c: &[TNote], link: &[bool]) -> Vec<JoinRun> {
    let mut v = Vec::new();
    let mut p = 0;
    while p < link.len() {
        let a = p;
        let s = c[a].start;
        let mut e = c[a].end();
        p += 1;
        while p < link.len() && link[p] {
            e = e.max(c[p].end());
            p += 1;
        }
        v.push((a, p, s, e.max(s.saturating_add(1))));
    }
    v
}

/// The pieces of the run `[a, b)` no longer than `limit` (greedy: a piece
/// takes the next fragment while it stays within the limit; one fragment
/// longer than the limit is a piece of its own): calls `cut` at each piece
/// start after the first and returns the number of pieces.
fn join_pieces(
    c: &[TNote],
    (a, b): (usize, usize),
    limit: u64,
    mut cut: impl FnMut(usize),
) -> usize {
    let mut count = 1;
    let mut s = c[a].start;
    let mut e = c[a].end().max(s.saturating_add(1));
    for (p, x) in c.iter().enumerate().take(b).skip(a + 1) {
        let ne = e.max(x.end());
        if ne - s <= limit {
            e = ne;
        } else {
            cut(p);
            count += 1;
            s = x.start;
            e = x.end().max(s.saturating_add(1));
        }
    }
    count
}

/// Runs being joined under the gates (n03 similarity, n02 longest result)
/// by n11 and n04: a union-find over consecutive runs, each group's start,
/// end and velocity range kept at its first run.
struct JoinRuns {
    first: Vec<usize>,
    last: Vec<usize>,
    chain_first: Vec<bool>,
    up: Vec<usize>,
    start: Vec<u64>,
    stop: Vec<u64>,
    vmin: Vec<u8>,
    vmax: Vec<u8>,
}

impl JoinRuns {
    fn new(c: &[TNote], head: &[bool], runs: &[JoinRun]) -> JoinRuns {
        let mut r = JoinRuns {
            first: Vec::with_capacity(runs.len()),
            last: Vec::with_capacity(runs.len()),
            chain_first: Vec::with_capacity(runs.len()),
            up: (0..runs.len()).collect(),
            start: Vec::with_capacity(runs.len()),
            stop: Vec::with_capacity(runs.len()),
            vmin: Vec::with_capacity(runs.len()),
            vmax: Vec::with_capacity(runs.len()),
        };
        for &(a, b, s, e) in runs {
            let v = c[a..b].iter().map(|x| x.vel);
            r.first.push(a);
            r.last.push(b - 1);
            r.chain_first.push(head[a]);
            r.start.push(s);
            r.stop.push(e);
            r.vmin.push(v.clone().min().unwrap_or(0));
            r.vmax.push(v.max().unwrap_or(0));
        }
        r
    }

    fn find(&mut self, mut r: usize) -> usize {
        let mut root = r;
        while self.up[root] != root {
            root = self.up[root];
        }
        while self.up[r] != root {
            let next = self.up[r];
            self.up[r] = root;
            r = next;
        }
        root
    }

    /// Joins run `r` (the first run of its group) into the group before it
    /// in its chain when the gates allow it; true when joined.
    fn try_join(&mut self, r: usize, c: &[TNote], gates: &JoinNew, link: &mut [bool]) -> bool {
        if self.chain_first[r] || self.find(r) != r {
            return false;
        }
        let g = self.find(r - 1);
        if let Some((d, chain_start)) = gates.similar {
            if chain_start {
                let refv = c[self.first[g]].vel;
                if self.vmin[r].abs_diff(refv) > d || self.vmax[r].abs_diff(refv) > d {
                    return false;
                }
            } else if c[self.first[r]].vel.abs_diff(c[self.last[r - 1]].vel) > d {
                return false;
            }
        }
        let stop = self.stop[g].max(self.stop[r]);
        if let Some((limit, _)) = gates.longest
            && stop - self.start[g] > limit
        {
            return false;
        }
        self.up[r] = g;
        self.stop[g] = stop;
        self.vmin[g] = self.vmin[g].min(self.vmin[r]);
        self.vmax[g] = self.vmax[g].max(self.vmax[r]);
        link[self.first[r]] = true;
        true
    }
}

/// The v0.9 Join pipeline (any new function on, or Control Sweeps).
struct Join {
    v8: JoinV08,
    new: JoinNew,
    ctx: Ctx,
    /// With Control Sweeps (E0422): what each level's functions are built
    /// from (every Join target is a function's control).
    swept: Option<std::sync::Arc<Rebuild>>,
}

impl Join {
    /// The window holding tick `t` (n01): from each bar line, or from the
    /// chain's first fragment at `chain0`.
    fn window_of(&self, t: u64, chain0: u64, w: u64, from_first: bool) -> (u64, u64) {
        if from_first {
            (0, t.saturating_sub(chain0) / w)
        } else {
            let (bs, _) = bar_bounds(&self.ctx.meter, t);
            (bs, t.saturating_sub(bs) / w)
        }
    }

    /// n05: the fragment ending at `prev_end` and the one starting at
    /// `next` meet (within the tolerance) at a bar line, a beat or a grid
    /// line (the grid restarting at bar lines), both within the tolerance
    /// of it.
    fn tie(&self, prev_end: u64, next: u64, (at, grid, tol): (usize, u64, u64)) -> bool {
        if prev_end.abs_diff(next) > tol {
            return false;
        }
        let meter = &self.ctx.meter;
        let (lo, hi) = match at {
            0 => {
                let (bs, bl) = bar_bounds(meter, next);
                (bs, bs.saturating_add(bl))
            }
            1 => {
                let m = metric_position(meter, next);
                (m.beat_start, m.beat_start.saturating_add(m.beat_len))
            }
            _ => {
                let (bs, bl) = bar_bounds(meter, next);
                let g = grid.max(1);
                let lo = bs + (next - bs) / g * g;
                (lo, lo.saturating_add(g).min(bs.saturating_add(bl)))
            }
        };
        [lo, hi]
            .into_iter()
            .any(|l| l.abs_diff(next) <= tol && l.abs_diff(prev_end) <= tol)
    }

    /// n08: runs shorter than `short` join the previous, the next or the
    /// longer neighbouring run of their chain within `gap` (the other one
    /// when the chosen one is missing or too far); a run joined into the
    /// next one is marked `crumb` (it gives the joined note no properties).
    fn absorb<'c>(
        c: &[TNote],
        head: &[bool],
        link: &mut [bool],
        crumb: &mut [bool],
        (_, into, gap): (u64, usize, u64),
        cf: &impl Fn(usize) -> &'c JoinNew,
    ) {
        let runs = join_runs(c, link);
        let mut moves: Vec<(usize, bool)> = Vec::new();
        for (r, &(a, _, s, e)) in runs.iter().enumerate() {
            let short = cf(a).absorb.map_or(0, |x| x.0);
            if e - s >= short {
                continue;
            }
            let prev = (!head[a])
                .then(|| r - 1)
                .filter(|&q| s.saturating_sub(runs[q].3) <= gap);
            let next = runs
                .get(r + 1)
                .filter(|n| !head[n.0] && n.2.saturating_sub(e) <= gap)
                .map(|_| r + 1);
            let span = |q: usize| runs[q].3 - runs[q].2;
            let into_next = match (into, prev, next) {
                (_, None, None) => continue,
                (_, Some(_), None) => false,
                (_, None, Some(_)) => true,
                (0, _, _) => false,
                (1, _, _) => true,
                (_, Some(p), Some(q)) => span(q) > span(p),
            };
            moves.push((r, into_next));
        }
        for (r, into_next) in moves {
            if into_next {
                link[runs[r + 1].0] = true;
                crumb[runs[r].0..runs[r].1].fill(true);
            } else {
                link[runs[r].0] = true;
            }
        }
    }

    /// n09: per track, a chord (two pitches or more, unmuted notes, onsets
    /// within the tolerance) followed by a matching chord within `gap`: each
    /// note of the second joins the same key of the first.
    fn chord_join(
        src: &Source,
        order: &[usize],
        head: &[bool],
        link: &mut [bool],
        (_, how, tol): (u64, usize, u64),
        gap_of: &dyn Fn(usize) -> u64,
    ) {
        let ns = &src.notes;
        let mut pos = vec![0usize; ns.len()];
        for (p, &i) in order.iter().enumerate() {
            pos[i] = p;
        }
        let mut chord = vec![usize::MAX; ns.len()];
        let mut id = 0usize;
        let classes = |ps: &[u8]| ps.iter().fold(0u16, |m, &p| m | 1 << (p % 12));
        for idx in per_track(src) {
            let idx: Vec<usize> = idx.into_iter().filter(|&i| !ns[i].muted()).collect();
            let groups = ChordGroups::of(ns, &idx, tol);
            let mut prev: Option<(usize, Vec<u8>, u64)> = None;
            for c in groups.iter() {
                let mut ps: Vec<u8> = c.iter().map(|&i| ns[i].pitch).collect();
                ps.sort_unstable();
                ps.dedup();
                let s = c.iter().map(|&i| ns[i].start).min().unwrap_or(0);
                let e = c.iter().map(|&i| ns[i].end()).max().unwrap_or(0);
                for &i in c {
                    chord[i] = id;
                }
                if let Some((pid, pps, pe)) = &prev
                    && ps.len() >= 2
                    && pps.len() >= 2
                    && s.saturating_sub(*pe) <= gap_of(c[0])
                    && match how {
                        0 => *pps == ps,
                        1 => classes(pps) == classes(&ps),
                        _ => pps[0] == ps[0] && pps[pps.len() - 1] == ps[ps.len() - 1],
                    }
                {
                    for &i in c {
                        let p = pos[i];
                        if !head[p] && chord[order[p - 1]] == *pid {
                            link[p] = true;
                        }
                    }
                }
                prev = Some((id, ps, e));
                id += 1;
            }
        }
    }

    /// n10: a fragment after a rest of at most `rest` joins the run before
    /// it when no other sounding note of the track (or of any track) starts
    /// in the rest `[run end, start)`.
    fn rest_join<'c>(
        src: &Source,
        c: &[TNote],
        head: &[bool],
        link: &mut [bool],
        (any, _): (bool, u64),
        cf: &impl Fn(usize) -> &'c JoinNew,
    ) {
        let ns = &src.notes;
        let lanes = if any {
            1
        } else {
            ns.iter()
                .map(|x| usize::from(x.track) + 1)
                .max()
                .unwrap_or(1)
        };
        let lane = |x: &TNote| if any { 0 } else { usize::from(x.track) };
        let mut starts: Vec<Vec<u64>> = vec![Vec::new(); lanes];
        for x in ns.iter().filter(|x| !x.muted()) {
            starts[lane(x)].push(x.start);
        }
        for s in &mut starts {
            s.sort_unstable();
        }
        let mut run_end = 0u64;
        for (p, x) in c.iter().enumerate() {
            if head[p] {
                run_end = x.end();
                continue;
            }
            let rest = cf(p).rests.map_or(0, |r| r.1);
            if !link[p] && x.start > run_end && x.start - run_end <= rest {
                let s = &starts[lane(x)];
                let lo = s.partition_point(|&t| t < run_end);
                let hi = s.partition_point(|&t| t < x.start);
                link[p] = lo == hi;
            }
            run_end = if link[p] {
                run_end.max(x.end())
            } else {
                x.end()
            };
        }
    }

    /// n03: a join holds only when the fragment's velocity is within `d` of
    /// the previous fragment's (or of its run's first fragment's).
    fn similar_gate<'c>(
        c: &[TNote],
        head: &[bool],
        link: &mut [bool],
        (_, chain_start): (u8, bool),
        cf: &impl Fn(usize) -> &'c JoinNew,
    ) {
        let mut refv = 0u8;
        for p in 0..c.len() {
            let v = c[p].vel;
            let d = cf(p).similar.map_or(0, |x| x.0);
            if head[p] || !link[p] {
                refv = v;
                continue;
            }
            let r = if chain_start { refv } else { c[p - 1].vel };
            if v.abs_diff(r) > d {
                link[p] = false;
                refv = v;
            }
        }
    }

    /// n02: runs longer than `limit` split into pieces within it: greedy
    /// ones, or as many (the fewest) cut at the fragment starts nearest the
    /// run's equal divisions — greedy again when such a cut would leave a
    /// piece over the limit.
    fn longest_cut<'c>(
        c: &[TNote],
        link: &mut [bool],
        (_, greedy): (u64, bool),
        cf: &impl Fn(usize) -> &'c JoinNew,
    ) {
        for (a, b, s, e) in join_runs(c, link) {
            let limit = cf(a).longest.map_or(1, |x| x.0);
            if e - s <= limit || b - a < 2 {
                continue;
            }
            if !greedy {
                let k = join_pieces(c, (a, b), limit, |_| {});
                if let Some(cuts) = Self::equal_cuts(c, (a, b, s, e), k, limit) {
                    for p in cuts {
                        link[p] = false;
                    }
                    continue;
                }
            }
            join_pieces(c, (a, b), limit, |p| link[p] = false);
        }
    }

    /// The `k − 1` cuts of run `[a, b)` (start `s`, end `e`) at the fragment
    /// starts nearest `s + j·(e − s)/k` (ties: the earlier start), each after
    /// the one before and leaving a fragment for every later piece; `None`
    /// when a piece would be longer than `limit`.
    fn equal_cuts(c: &[TNote], (a, b, s, e): JoinRun, k: usize, limit: u64) -> Option<Vec<usize>> {
        let start = |p: usize| c[p].start;
        let mut cuts = Vec::with_capacity(k.saturating_sub(1));
        let mut prev = a;
        for j in 1..k {
            let cut = (u128::from(s) + u128::from(e - s) * j as u128 / k as u128) as u64;
            let (lo, hi) = (prev + 1, b - (k - j));
            // The first start at or after c, and the one before it.
            let q = lo + c[lo..=hi].partition_point(|x| x.start < cut);
            let p = if q > hi {
                hi
            } else if q > lo && cut - start(q - 1) <= start(q) - cut {
                q - 1
            } else {
                q
            };
            cuts.push(p);
            prev = p;
        }
        let mut from = a;
        for &to in cuts.iter().chain(std::iter::once(&b)) {
            let s0 = start(from);
            let end = c[from..to].iter().map(TNote::end).max().unwrap_or(s0);
            if end.max(s0.saturating_add(1)) - s0 > limit {
                return None;
            }
            from = to;
        }
        Some(cuts)
    }

    /// n11: per chain and bar, while more than `most` runs start in the
    /// bar, a run joins the one before it in the bar — shortest gaps or the
    /// earliest first — when the gates allow it.
    fn per_bar<'c>(
        &self,
        c: &[TNote],
        head: &[bool],
        link: &mut [bool],
        (_, earliest): (usize, bool),
        cf: &impl Fn(usize) -> &'c JoinNew,
    ) {
        let runs = join_runs(c, link);
        let mut rs = JoinRuns::new(c, head, &runs);
        let meter = &self.ctx.meter;
        let bars: Vec<u64> = runs.iter().map(|r| bar_bounds(meter, r.2).0).collect();
        let mut r0 = 0;
        while r0 < runs.len() {
            let mut r1 = r0 + 1;
            while r1 < runs.len() && !head[runs[r1].0] && bars[r1] == bars[r0] {
                r1 += 1;
            }
            let most = cf(runs[r0].0).per_bar.map_or(1, |x| x.0);
            let mut count = r1 - r0;
            if count > most {
                let mut cand: Vec<(u64, u64, usize)> = (r0 + 1..r1)
                    .map(|r| {
                        let gap = if earliest {
                            0
                        } else {
                            runs[r].2.saturating_sub(runs[r - 1].3)
                        };
                        (gap, runs[r].2, r)
                    })
                    .collect();
                cand.sort_unstable();
                for (_, _, r) in cand {
                    if count <= most {
                        break;
                    }
                    if rs.try_join(r, c, cf(runs[r].0), link) {
                        count -= 1;
                    }
                }
            }
            r0 = r1;
        }
    }

    /// n04: runs within `gap` of the run before them join it — shortest
    /// gaps first, equal gaps spread evenly over time — while a window of real time holding their start has
    /// more than `cap` sounding starts (`density_window::budget_keep_if`,
    /// the canonical NPS window through the tempo map), when the gates allow
    /// it.
    fn nps_join<'c>(
        &self,
        c: &[TNote],
        head: &[bool],
        link: &mut [bool],
        (_, window, gap): (usize, u64, u64),
        cf: &impl Fn(usize) -> &'c JoinNew,
    ) {
        let runs = join_runs(c, link);
        let mut rs = JoinRuns::new(c, head, &runs);
        let mut by: Vec<(u64, u32)> = runs
            .iter()
            .enumerate()
            .filter(|(_, r)| !c[r.0].muted())
            .map(|(k, r)| (r.2, k as u32))
            .collect();
        by.sort_unstable();
        let mut tt = TempoTime::new(&self.ctx);
        let times: Vec<u64> = by.iter().map(|&(s, _)| tt.micros_at(s)).collect();
        let mut place = vec![u32::MAX; runs.len()];
        for (q, &(_, k)) in by.iter().enumerate() {
            place[k as usize] = q as u32;
        }
        // Equal gaps go in bit-reversed time order, so a uniform stream is
        // joined evenly over time rather than from one end.
        let bits = usize::BITS - by.len().leading_zeros();
        let spread = |q: u32| {
            if bits == 0 {
                0
            } else {
                q.reverse_bits() >> (32 - bits)
            }
        };
        let mut cand: Vec<(u64, u32, u32)> = (0..runs.len())
            .filter(|&r| !head[runs[r].0] && place[r] != u32::MAX)
            .filter_map(|r| {
                let g = runs[r].2.saturating_sub(runs[r - 1].3);
                (g <= gap).then(|| (g, spread(place[r]), place[r]))
            })
            .collect();
        cand.sort_unstable();
        let removable: Vec<usize> = cand.iter().map(|c| c.2 as usize).collect();
        drop(cand);
        let cap = |q: usize| cf(runs[by[q].1 as usize].0).nps.map_or(1, |x| x.0);
        budget_keep_if_by(&times, window, cap, &removable, |q| {
            let r = by[q].1 as usize;
            rs.try_join(r, c, cf(runs[r].0), link)
        });
    }

    /// n07: joined notes of the same key, length and mute state starting
    /// within the tolerance of the cluster's first one on two tracks or
    /// more are one note, kept on the first track of the source (or the
    /// active track when it holds one of them) at the loudest velocity of
    /// the cluster (or the first track's); the others go. Returns how many
    /// went.
    fn cross_track(
        &self,
        src: &Source,
        res: &mut [(TNote, usize, usize)],
        kept: &mut [bool],
        (active, first_vel, tol): (bool, bool, u64),
    ) -> u64 {
        let active_slot = if active {
            self.ctx
                .active
                .and_then(|ix| src.tracks.iter().position(|t| t.index == ix))
                .and_then(|s| u16::try_from(s).ok())
        } else {
            None
        };
        // By key, length, mute state, start, track (packed values: no reads
        // of the notes while sorting).
        let mut keyed: Vec<(u8, u64, u8, u64, u16, usize)> = res
            .iter()
            .enumerate()
            .map(|(k, (t, _, _))| (t.pitch, t.dur, t.flags & FLAG_MUTED, t.start, t.track, k))
            .collect();
        keyed.sort_unstable();
        let idx: Vec<usize> = keyed.iter().map(|x| x.5).collect();
        let mut removed = 0u64;
        let mut c0 = 0;
        while c0 < keyed.len() {
            let f = keyed[c0];
            let mut c1 = c0 + 1;
            while c1 < keyed.len()
                && (keyed[c1].0, keyed[c1].1, keyed[c1].2) == (f.0, f.1, f.2)
                && keyed[c1].3 - f.3 <= tol
            {
                c1 += 1;
            }
            let cl = &idx[c0..c1];
            c0 = c1;
            let first_track = cl.iter().map(|&k| res[k].0.track).min().unwrap_or(0);
            if cl.iter().all(|&k| res[k].0.track == first_track) {
                continue;
            }
            let keep_on = active_slot
                .filter(|&s| cl.iter().any(|&k| res[k].0.track == s))
                .unwrap_or(first_track);
            let vel = if first_vel {
                cl.iter()
                    .map(|&k| res[k].0)
                    .find(|t| t.track == first_track)
                    .map_or(0, |t| t.vel)
            } else {
                cl.iter().map(|&k| res[k].0.vel).max().unwrap_or(0)
            };
            for &k in cl {
                if res[k].0.track == keep_on {
                    res[k].0.vel = vel;
                } else {
                    kept[k] = false;
                    removed += 1;
                }
            }
        }
        removed
    }

    /// n06: over each joined note of two fragments or more (kept, unmuted)
    /// its fragments' velocities × scale as a stepped controller curve on
    /// its channel; notes of one track and channel share the lane — its
    /// value is the loudest sounding curve's, the resting value once none
    /// sounds. One point per tick, only where the value changes. Returns the
    /// points written.
    fn expression<'c>(
        c: &[TNote],
        res: &[(TNote, usize, usize)],
        kept: &[bool],
        (lane, rest, _): (Lane, i64, f64),
        cf: &impl Fn(usize) -> &'c JoinNew,
        out: &mut Output,
    ) -> usize {
        // (track, channel, tick, end, note, value)
        let mut ev: Vec<(u16, u8, u64, bool, u32, i16)> = Vec::new();
        for (k, &(t, a, b)) in res.iter().enumerate() {
            if !kept[k] || b - a < 2 || t.flags & FLAG_MUTED != 0 {
                continue;
            }
            for (q, f) in c[a..b].iter().enumerate() {
                let scale = cf(a + q).expr.map_or(0.0, |x| x.2);
                let v = (f64::from(f.vel) * scale).round().clamp(0.0, 127.0) as i16;
                ev.push((t.track, t.ch, f.start, false, k as u32, v));
            }
            ev.push((t.track, t.ch, t.end(), true, k as u32, 0));
        }
        ev.sort_unstable();
        let mut cur = vec![-1i16; res.len()];
        let mut sounding: std::collections::BTreeMap<i16, u32> = Default::default();
        let mut written = 0usize;
        let mut q = 0;
        while q < ev.len() {
            let (tr, ch) = (ev[q].0, ev[q].1);
            let mut last: Option<i64> = None;
            sounding.clear();
            while q < ev.len() && (ev[q].0, ev[q].1) == (tr, ch) {
                let tick = ev[q].2;
                while q < ev.len() && (ev[q].0, ev[q].1, ev[q].2) == (tr, ch, tick) {
                    let (_, _, _, end, k, v) = ev[q];
                    let k = k as usize;
                    if cur[k] >= 0
                        && let Some(c) = sounding.get_mut(&cur[k])
                    {
                        *c -= 1;
                        if *c == 0 {
                            sounding.remove(&cur[k]);
                        }
                    }
                    cur[k] = if end { -1 } else { v };
                    if !end {
                        *sounding.entry(v).or_insert(0) += 1;
                    }
                    q += 1;
                }
                let value = sounding
                    .last_key_value()
                    .map_or(rest, |(&v, _)| i64::from(v));
                if last != Some(value) {
                    out.emit_event(tr, lane, ch, i128::from(tick), value);
                    written += 1;
                    last = Some(value);
                }
            }
        }
        written
    }

    fn run9(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        // Each note's functions under Control Sweeps (E0422).
        match &self.swept {
            Some(r) => {
                let lv: PerLevel<JoinNew> =
                    r.levels(&self.ctx, src, |p, b| JoinNew::from(p, b.ctx));
                self.run9_at(src, out, |i| lv.at(i))
            }
            None => self.run9_at(src, out, |_| &self.new),
        }
    }

    /// [`Join::run9`] with note `i`'s functions `at(i)`.
    fn run9_at<'a>(
        &'a self,
        src: &Source,
        out: &mut Output,
        at: impl Fn(usize) -> &'a JoinNew,
    ) -> Result<(), TransformError> {
        let ns = &src.notes;
        let (v8, nw) = (&self.v8, &self.new);
        let order = v8.order(ns);
        let cf_fn = |p: usize| at(order[p]);
        let cf = &cf_fn;
        // The fragments in chain order (one gather; every stage below reads
        // them in sequence).
        let c: Vec<TNote> = order.iter().map(|&i| ns[i]).collect();
        let n = c.len();
        let head: Vec<bool> = (0..n)
            .map(|p| p == 0 || v8.key(&c[p]) != v8.key(&c[p - 1]))
            .collect();
        let mut link = vec![false; n];
        let mut crumb = vec![false; n];
        // 1. Which neighbours join: the v0.8 rule, or n01 (110) windows and
        //    n05 (120) boundary ties in its place.
        if nw.window.is_none() && nw.ties.is_none() {
            let line = v8.line(ns);
            let mut i = 0;
            while i < n {
                let (j, _, _) = v8.extend(|p| &c[p], n, &line, i);
                link[i + 1..j].fill(true);
                i = j;
            }
        } else {
            let mut chain0 = 0u64;
            for p in 0..n {
                let x = &c[p];
                if head[p] {
                    chain0 = x.start;
                    continue;
                }
                let y = &c[p - 1];
                let mut l = false;
                if let Some((_, from_first)) = nw.window {
                    let w = cf(p).window.map_or(1, |x| x.0);
                    l = self.window_of(y.start, chain0, w, from_first)
                        == self.window_of(x.start, chain0, w, from_first);
                }
                if let Some(t) = cf(p).ties {
                    l = l || self.tie(y.end(), x.start, t);
                }
                link[p] = l;
            }
        }
        // 2. Joins added: n08 (130), n09 (140), n10 (150).
        if let Some(a) = nw.absorb {
            Self::absorb(&c, &head, &mut link, &mut crumb, a, cf);
        }
        if let Some(ch) = nw.chords {
            let gap_of = |i: usize| at(i).chords.map_or(0, |x| x.0);
            Self::chord_join(src, &order, &head, &mut link, ch, &gap_of);
        }
        if let Some(r) = nw.rests {
            Self::rest_join(src, &c, &head, &mut link, r, cf);
        }
        // 3. The gates: n03 (160), n02 (170).
        if let Some(s) = nw.similar {
            Self::similar_gate(&c, &head, &mut link, s, cf);
        }
        if let Some(l) = nw.longest {
            Self::longest_cut(&c, &mut link, l, cf);
        }
        // 4. Joins until a target, within the gates: n11 (180), n04 (190).
        if let Some(b) = nw.per_bar {
            self.per_bar(&c, &head, &mut link, b, cf);
        }
        if let Some(t) = nw.nps {
            self.nps_join(&c, &head, &mut link, t, cf);
        }
        out.check()?;
        // 5. The joined notes (the v0.8 note of each run).
        let mut res: Vec<(TNote, usize, usize)> = Vec::new();
        let mut joined = 0u64;
        for (a, b, _, _) in join_runs(&c, &link) {
            let frags = &c[a..b];
            let end = frags.iter().map(TNote::end).max().unwrap_or(0);
            let last_end = frags[frags.len() - 1].end();
            let lead = (a..b).position(|p| !crumb[p]).unwrap_or(0);
            let (mut t, stop) = v8.joined(src, frags, lead, end, last_end);
            t.start = frags[0].start;
            t.dur = stop.saturating_sub(t.start);
            joined += (b - a - 1) as u64;
            res.push((t, a, b));
        }
        // 6. n07 (200) cross-track duplicates.
        let mut kept = vec![true; res.len()];
        let dups = match nw.across {
            Some(c) => self.cross_track(src, &mut res, &mut kept, c),
            None => 0,
        };
        for (k, (t, _, _)) in res.iter().enumerate() {
            if kept[k] {
                out.emit(
                    i128::from(t.start),
                    i128::from(t.dur),
                    i64::from(t.pitch),
                    i64::from(t.vel),
                    t,
                );
                out.check()?;
            }
        }
        // 7. n06 (510) the fragments' dynamics as a controller curve.
        let points = match nw.expr {
            Some(e) => Self::expression(&c, &res, &kept, e, cf, out),
            None => 0,
        };
        out.check()?;
        let mut msg = format!("{} fragment(s) joined.", crate::midi::group_digits(joined));
        if dups > 0 {
            msg.push_str(&format!(
                " {} duplicate(s) on other tracks joined.",
                crate::midi::group_digits(dups)
            ));
        }
        if points > 0 {
            msg.push_str(" Controller events are written on each joined note's MIDI channel and affect every note sounding on that channel.");
        }
        out.message = Some(msg);
        Ok(())
    }
}

// ================================================================ Note Repeat

pub const REPEAT: ToolDef = ToolDef {
    id: "repeat",
    spec: repeat_spec,
    build: repeat_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const REPEAT_HELP: &[(&str, &str)] = &[
    (
        "len",
        "The length of each repeat, as a note value: every note is cut into repeats of this length within its own span.",
    ),
    (
        "len.ticks",
        "For Custom: the length of each repeat, in ticks.",
    ),
    (
        "gate",
        "Each repeat's sounding length as a share of its slot (100 % = repeats touch, lower = detached).",
    ),
    (
        "pitch.step",
        "Moves each repeat this many semitones further than the one before (60, +2 → 60, 62, 64 …).",
    ),
    (
        "overflow",
        "Lets the last repeat keep its full length past the note's end; off cuts it at the note's end.",
    ),
    (
        "a1",
        "Moves the repeats through the track's subchannels, a step every few repeats.",
    ),
    ("a1.n", "The repeats between subchannel steps."),
    (
        "a1.step",
        "How many subchannels each step moves (+ forward, − backward; wraps around the track's list).",
    ),
    (
        "a2",
        "Leaves a gap after each repeat instead of a gate percentage.",
    ),
    ("a2.gap", "The silence after each repeat, in ticks."),
    ("a3", "Shapes the repeats' velocity across each note."),
    (
        "a3.curve",
        "The velocity multiplier from the note's first repeat (left) to its last (right).",
    ),
    ("a4", "Shapes the repeats' gate across each note."),
    (
        "a4.curve",
        "The gate multiplier from the note's first repeat (left) to its last (right).",
    ),
    (
        "a5",
        "Spaces the repeats unevenly within the note, speeding up or slowing down, while the note's span stays the same.",
    ),
    (
        "a5.amount",
        "+ starts wide and speeds up toward the note's end; − starts fast and slows down; 0 = even.",
    ),
    (
        "a6",
        "Adds a repeating cycle of intervals to the repeats' pitches.",
    ),
    (
        "a6.cycle",
        "The interval added to each repeat in turn, in semitones; the list repeats.",
    ),
    (
        "a7",
        "Keeps each repeat (after the first) only with a chance (seeded).",
    ),
    ("a7.prob", "The chance that a repeat is kept."),
    (
        "a8",
        "Hands the repeats of a chord from voice to voice, bouncing up and down through the chord.",
    ),
    (
        "a9",
        "Moves each chord voice's repeats through a cycle of subchannels, each voice a few steps out of phase.",
    ),
    ("a9.cycle", "How many subchannels the cycle walks through."),
    (
        "a9.phase",
        "How many steps each voice of a chord is ahead of the voice below it.",
    ),
    (
        "a10",
        "Splits every repeat into smaller repeats again, level after level.",
    ),
    ("a10.depth", "How many levels of splitting."),
    (
        "a10.div",
        "How many parts each repeat splits into at every level.",
    ),
    (
        "n01",
        "Gives every note an exact number of repeats, whatever its length.",
    ),
    ("n01.n", "How many repeats each note gets."),
    (
        "n01.rest",
        "Where the leftover time goes when the note does not divide evenly: spread over the repeats, or at the end.",
    ),
    (
        "n02",
        "Adds echoes after each note, each one quieter (a delay with feedback).",
    ),
    ("n02.delay", "The time between echoes, in ticks."),
    (
        "n02.feedback",
        "Each echo's velocity as a share of the one before.",
    ),
    ("n02.echoes", "How many echoes each note gets."),
    (
        "n02.pitch",
        "Moves each echo this many semitones further than the one before.",
    ),
    (
        "n03",
        "Repeats like a bouncing ball: each gap shorter than the last.",
    ),
    (
        "n03.restitution",
        "Each gap as a share of the gap before (closer to 1 = longer bouncing).",
    ),
    ("n03.first", "The first gap, in ticks."),
    (
        "n03.shortest",
        "The bouncing stops once a gap would be shorter than this, in ticks.",
    ),
    (
        "n12",
        "Writes a controller value at each repeat, stepping through a list.",
    ),
    (
        "n12.cc",
        "The controller written: CC 10 pan, CC 1 modulation, CC 11 expression or CC 74.",
    ),
    (
        "n12.values",
        "The controller value of each repeat in turn (0 – 127); the list repeats.",
    ),
    (
        "n05",
        "Turns each note into a cloud of short grains scattered in time and pitch (seeded).",
    ),
    ("n05.len", "The length of each grain, in ticks."),
    ("n05.density", "How many grains per beat."),
    (
        "n05.spread",
        "How far grains may stray from the note's pitch, in semitones.",
    ),
    (
        "n05.overlap",
        "How far each grain's start may scatter, as a share of the spacing.",
    ),
    (
        "n13",
        "Speeds the repeats up toward a peak and slows them down after it, like a passing siren.",
    ),
    (
        "n13.peak",
        "Where in the note the repeats are fastest (0 % = at its start, 100 % = at its end).",
    ),
    (
        "n13.depth",
        "How much faster the repeats are at the peak than at the ends (100 % = twice as fast).",
    ),
    ("n07", "Repeats only some voices of each chord."),
    (
        "n07.voices",
        "The voices that repeat: the lowest, the highest, the inner voices, or all but the lowest.",
    ),
    (
        "n07.others",
        "The other voices hold their notes, or are left as written.",
    ),
    (
        "n07.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n08",
        "Repeats on a ladder of subdivisions that gets finer as the note goes on.",
    ),
    ("n08.start", "The repeats per beat at the note's start."),
    (
        "n08.factor",
        "How many times finer the subdivision gets at each step.",
    ),
    (
        "n08.every",
        "How often the subdivision steps up: every beat, every half note, or every bar.",
    ),
    (
        "n08.most",
        "The subdivision never goes finer than this many repeats per beat.",
    ),
    ("n09", "Steps through a list of repeat lengths."),
    (
        "n09.lengths",
        "The repeat lengths in turn, in ticks; the list repeats.",
    ),
    (
        "n09.per",
        "When the next length is used: each note, each beat, or each chord.",
    ),
    (
        "n10",
        "Holds each note first, then starts the repeats partway through (a delayed tremolo).",
    ),
    (
        "n10.share",
        "How much of the note is held before the repeats start.",
    ),
    ("n10.ticks", "The hold lasts at least this many ticks."),
    (
        "n10.entry",
        "The repeats start suddenly, or accelerate into full speed.",
    ),
    ("n14", "Lets each repeat ring into the next one."),
    (
        "n14.overlap",
        "Each repeat's length as a share of its slot (150 % = it overlaps the next by half a slot).",
    ),
];

fn repeat_spec() -> ToolSpec {
    let mut p = vec![
        choice("len", "Repeat length", NOTE_VALUES, 4),
        ticks("len.ticks", "Custom length (ticks)", 1, 1 << 40, 120),
        float("gate", "Gate", 1.0, 100.0, 100.0, "%"),
        // v0.8 §162–§163: cumulative pitch per repeat (60, +2 → 60, 62, 64 …);
        // the Pitch cycle (advanced) is added after it: base + r × offset + cycle[r].
        int(
            "pitch.step",
            "Pitch offset per repeat (semitones, cumulative)",
            -48,
            48,
            0,
        ),
        flag(
            "overflow",
            "Overflow: the last repeat keeps its full length past the note's end",
            false,
        ),
        adv_switch(1, "Subchannel offset every N repeats"),
        int("a1.n", "Every N repeats", 1, 64, 1).adv(1),
        int("a1.step", "Subchannel steps (signed, wraps)", -64, 64, 1).adv(1),
        adv_switch(2, "Cut / gap length"),
        ticks("a2.gap", "Gap after each repeat (ticks)", 1, 1 << 40, 30).adv(2),
        adv_switch(3, "Velocity envelope"),
        curve(
            "a3.curve",
            "Velocity across the repeats",
            Curve::line(1.0, 0.4),
        )
        .overshoot()
        .adv(3),
        adv_switch(4, "Gate envelope"),
        curve("a4.curve", "Gate across the repeats", Curve::line(1.0, 0.3))
            .overshoot()
            .adv(4),
        adv_switch(5, "Acceleration curve (span preserved)"),
        float("a5.amount", "Acceleration", -100.0, 100.0, 60.0, "%").adv(5),
        adv_switch(6, "Pitch cycle"),
        list(
            "a6.cycle",
            "Intervals (semitones)",
            -48,
            48,
            64,
            &[0, 12, 7],
        )
        .adv(6),
        adv_switch(7, "Seeded probability"),
        float("a7.prob", "Keep each repeat", 0.0, 100.0, 70.0, "%").adv(7),
        seed("a7.seed").adv(7),
        adv_switch(8, "Ping-pong chord order"),
        adv_switch(9, "Subchannel phase"),
        int("a9.cycle", "Subchannels in the cycle", 1, 64, 4).adv(9),
        int("a9.phase", "Phase per chord note", 0, 64, 1).adv(9),
        adv_switch(10, "Recursive repeat"),
        int("a10.depth", "Depth", 1, 4, 2).adv(10),
        int("a10.div", "Parts per repeat", 2, 8, 2).adv(10),
        // V09-S36 step 6: the functions declared by id (ids n04 / n06 / n11
        // are not used; docs/TRANSFORM_ENGINE.md "Note Repeat").
        flag("n01", "Exact count", false),
        int("n01.n", "Repeats per note", 1, 4096, 4),
        choice("n01.rest", "Remainder", REP_REMAINDER, 0),
        flag("n02", "Delay echo", false),
        ticks("n02.delay", "Delay (ticks)", 1, 1 << 40, 480),
        float("n02.feedback", "Feedback", 0.0, 100.0, 60.0, "%"),
        int("n02.echoes", "Echoes", 1, 64, 3),
        int("n02.pitch", "Pitch per echo (semitones)", -24, 24, 0),
        flag("n03", "Bounce", false),
        float("n03.restitution", "Restitution", 0.1, 0.95, 0.7, ""),
        ticks("n03.first", "First interval (ticks)", 1, 1 << 40, 480),
        ticks("n03.shortest", "Shortest interval (ticks)", 1, 1 << 40, 30),
        flag("n12", "Controller steps", false),
        choice("n12.cc", "Controller", REP_CC, 0),
        list("n12.values", "Values", 0, 127, 64, &[32, 96]),
        flag("n05", "Grain cloud", false),
        ticks("n05.len", "Grain length (ticks)", 1, 1 << 40, 60),
        float("n05.density", "Grains per beat", 0.25, 256.0, 16.0, ""),
        int("n05.spread", "Pitch spread (semitones)", 0, 24, 2),
        float(
            "n05.overlap",
            "Overlap (start scatter)",
            0.0,
            90.0,
            30.0,
            "%",
        ),
        seed("n05.seed"),
        flag("n13", "Doppler rate", false),
        float("n13.peak", "Peak at", 0.0, 100.0, 50.0, "%"),
        float("n13.depth", "Depth", 0.0, 400.0, 100.0, "%"),
        flag("n07", "Repeated voices", false),
        choice("n07.voices", "Voices", REP_VOICES, 1),
        choice("n07.others", "Others", REP_OTHERS, 0),
        ticks("n07.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n08", "Subdivision ladder", false),
        int("n08.start", "Start subdivision (per beat)", 1, 64, 1),
        int("n08.factor", "Factor", 2, 4, 2),
        choice("n08.every", "Step every", REP_STEP, 0),
        int("n08.most", "Most per beat", 1, 256, 16),
        flag("n09", "Length cycle", false),
        list(
            "n09.lengths",
            "Lengths (ticks)",
            1,
            100_000,
            64,
            &[240, 320, 160],
        ),
        choice("n09.per", "Advance per", REP_ADVANCE, 0),
        flag("n10", "Delayed tremolo", false),
        float("n10.share", "Hold before", 0.0, 95.0, 50.0, "%"),
        ticks("n10.ticks", "Hold at least (ticks)", 0, 1 << 40, 0),
        choice("n10.entry", "Entry", REP_ENTRY, 0),
        flag("n14", "Overlapping repeats", false),
        float("n14.overlap", "Overlap", 100.0, 400.0, 150.0, "%"),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, REPEAT_HELP);
    ToolSpec {
        id: "repeat",
        name: "Chopper V2",
        family: Family::Iterations,
        summary: "Subdivides each note into repeats of the chosen length (and gate) inside its own span.",
        params: p,
        advanced: vec![
            "Subchannel offset every N repeats",
            "Cut / gap length",
            "Velocity envelope",
            "Gate envelope",
            "Acceleration curve",
            "Pitch cycle",
            "Seeded probability",
            "Ping-pong chord order",
            "Subchannel phase",
            "Recursive repeat",
        ],
        // Orders = where each runs in `Rep9::run9` (§235).
        functions: [
            ("n01", "Exact count", 140),
            ("n02", "Delay echo", 510),
            ("n03", "Bounce", 160),
            ("n12", "Controller steps", 520),
            ("n05", "Grain cloud", 170),
            ("n13", "Doppler rate", 180),
            ("n07", "Repeated voices", 110),
            ("n08", "Subdivision ladder", 150),
            ("n09", "Length cycle", 130),
            ("n10", "Delayed tremolo", 120),
            ("n14", "Overlapping repeats", 190),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

const REP_REMAINDER: &[&str] = &["Spread evenly", "At the end"];
const REP_CC: &[&str] = &["CC 10 pan", "CC 1 modulation", "CC 11 expression", "CC 74"];
const REP_VOICES: &[&str] = &["Lowest", "Highest", "Inner", "All but the lowest"];
const REP_OTHERS: &[&str] = &["Hold", "Leave as written"];
const REP_STEP: &[&str] = &["Beat", "Half note", "Bar (at least a beat)"];
const REP_ADVANCE: &[&str] = &["Note", "Beat", "Chord"];
const REP_ENTRY: &[&str] = &["Sudden", "Accelerate into"];

/// Largest repeat count per note (a note longer than this × length is cut
/// into this many repeats … per level).
const MAX_REPEATS: u64 = 1 << 20;

fn repeat_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let len = note_value(p.choice("len"), p.ticks("len.ticks"), b.ctx);
    let gate = p.float("gate") / 100.0;
    let overflow = p.flag("overflow");
    let pitch_step = p.int("pitch.step");
    let sub_every = p.adv(1).then(|| (p.int("a1.n").max(1), p.int("a1.step")));
    let gap = p.adv(2).then(|| p.ticks("a2.gap"));
    let venv = p.adv(3).then(|| p.curve("a3.curve"));
    let genv = p.adv(4).then(|| p.curve("a4.curve"));
    let accel = p.adv(5).then(|| -3.93 * p.float("a5.amount") / 100.0);
    let cycle = if p.adv(6) {
        p.list("a6.cycle")
    } else {
        Vec::new()
    };
    let prob = p
        .adv(7)
        .then(|| (p.float("a7.prob") / 100.0, p.seed("a7.seed")));
    let pingpong = p.adv(8);
    let phase = p
        .adv(9)
        .then(|| (p.int("a9.cycle").max(1), p.int("a9.phase")));
    let rec = p
        .adv(10)
        .then(|| (p.int("a10.depth") as u32, p.int("a10.div") as u64));
    // Bound: Σ ⌈dur / len⌉ (+1 per note), × (2·parts − 1)^depth: a block of
    // d ticks split into parts of ⌊d / parts⌋ ticks gives ⌈d / ⌊d / parts⌋⌉ ≤
    // 2·parts − 1 pieces (rounding).
    let mut total = b
        .stats
        .steps_bound(len)
        .saturating_add(b.stats.count as u64);
    if let Some((d, parts)) = rec {
        total = total.saturating_mul((2 * parts).saturating_sub(1).max(1).saturating_pow(d));
    }
    let new = RepNew::from(p, b.ctx);
    let swept = Rebuild::of(p, b);
    if new.any() || swept.is_some() {
        // With Control Sweeps always the v0.9 pipeline (E0422), its bound
        // the largest over the swept ranges' corners.
        let mut r = rep9_of(p, b);
        let total = match &swept {
            Some(rb) => {
                let mut t = r.bound(b.stats)?;
                for c in rb.corners(b.ctx, rep9_of) {
                    t = t.max(c.bound(b.stats)?);
                }
                t
            }
            None => r.bound(b.stats)?,
        };
        r.swept = swept;
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| r.run9(src, out),
            growth: Growth::Generate(total),
            estimate: Some(total),
        });
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let ns = &src.notes;
            // Chord membership (same track and start), for ping-pong / phase.
            let mut chord_of = vec![(0usize, 1usize); ns.len()];
            let mut i = 0;
            while i < ns.len() {
                let mut j = i;
                while j < ns.len() && ns[j].start == ns[i].start {
                    j += 1;
                }
                // Same start; split per track and order by pitch.
                let mut idx: Vec<usize> = (i..j).collect();
                idx.sort_by_key(|&k| (ns[k].track, ns[k].pitch, k));
                let mut a = 0;
                while a < idx.len() {
                    let mut b2 = a;
                    while b2 < idx.len() && ns[idx[b2]].track == ns[idx[a]].track {
                        b2 += 1;
                    }
                    for (r, &k) in idx[a..b2].iter().enumerate() {
                        chord_of[k] = (r, b2 - a);
                    }
                    a = b2;
                }
                i = j;
            }
            for (ni, n) in ns.iter().enumerate() {
                let (rank, size) = chord_of[ni];
                let tr = src.track(n.track);
                // Level 0: the note's own repeats.
                let mut stack: Vec<(u64, u64, u32)> = vec![(n.start, n.dur, 0)];
                while let Some((s0, d0, depth)) = stack.pop() {
                    let l = if depth == 0 {
                        len
                    } else {
                        let parts = rec.map_or(2, |r| r.1);
                        (d0 / parts).max(1)
                    };
                    let k = d0.div_ceil(l).clamp(1, MAX_REPEATS);
                    for r in 0..k {
                        // V09-0143: the repeat (0 = the first).
                        out.set_dim(r as u32);
                        let u0 = r as f64 / k as f64;
                        let u1 = (r + 1) as f64 / k as f64;
                        let (a, b2) = match accel {
                            Some(acc) => (
                                s0.saturating_add((warp(u0, acc) * d0 as f64).round() as u64),
                                s0.saturating_add((warp(u1, acc) * d0 as f64).round() as u64),
                            ),
                            // Saturating: nested repeats of an overflowing slot may
                            // reach past the tick range (dropped by the output).
                            None => (
                                s0.saturating_add(r.saturating_mul(l)),
                                s0.saturating_add((r + 1).saturating_mul(l).min(d0)),
                            ),
                        };
                        let full = if accel.is_some() {
                            b2.saturating_sub(a)
                        } else {
                            l
                        };
                        let slot = if overflow {
                            full
                        } else {
                            b2.saturating_sub(a).max(1)
                        };
                        let mut g = gate;
                        if let Some(c) = &genv {
                            // B1-0113: an overshooting gate graph may lengthen past the slot.
                            g *= c.eval(u0).max(0.0);
                        }
                        let mut d = ((slot as f64 * g).round() as u64).max(1);
                        if let Some(gp) = gap {
                            d = d.saturating_sub(gp).max(1);
                        }
                        if let Some((d_max, _)) = rec
                            && depth < d_max
                        {
                            stack.push((a, d, depth + 1));
                            continue;
                        }
                        if pingpong && size > 1 {
                            let period = 2 * (size - 1);
                            let ph = (r as usize) % period;
                            let pick = if ph < size { ph } else { period - ph };
                            if pick != rank {
                                continue;
                            }
                        }
                        if let Some((pr, seed)) = prob
                            && r > 0
                            && !Rng::for_index(seed ^ super::super::rng::note_key(n), salt::KEEP, r)
                                .chance(pr)
                        {
                            continue;
                        }
                        let mut t = *n;
                        if let (Some((every, step)), Some(tr)) = (sub_every, tr) {
                            let (id, ch) = tr.sub_offset(n.sub, (r as i64 / every) * step);
                            t.sub = id;
                            t.ch = ch;
                        }
                        if let (Some((cyc, ph)), Some(tr)) = (phase, tr) {
                            let off = (r as i64 + ph * rank as i64).rem_euclid(cyc);
                            let (id, ch) = tr.sub_offset(n.sub, off);
                            t.sub = id;
                            t.ch = ch;
                        }
                        let pitch = i64::from(n.pitch)
                            + pitch_step * r as i64
                            + if cycle.is_empty() {
                                0
                            } else {
                                cycle[(r as usize) % cycle.len()]
                            };
                        let vel = match &venv {
                            Some(c) => (f64::from(n.vel) * c.eval(u0)).round() as i64,
                            None => i64::from(n.vel),
                        };
                        if !out.emit(i128::from(a), i128::from(d), pitch, vel.max(1), &t) {
                            return out.check();
                        }
                    }
                    out.check()?;
                }
            }
            Ok(())
        },
        growth: Growth::Generate(total),
        estimate: Some(total),
    })
}

/// Note Repeat's v0.9 functions (V09-S36 step 6), declared by id.
#[derive(Clone, Debug)]
struct RepNew {
    /// n07: voices (`REP_VOICES`), the others hold (else as written), chord
    /// tolerance.
    voices: Option<(usize, bool, u64)>,
    /// n10: hold share (fraction), hold at least (ticks), accelerate into
    /// the repeats.
    delayed: Option<(f64, u64, bool)>,
    /// n09: lengths (ticks), advance per (`ADVANCE_*`).
    lengths: Option<(Vec<u64>, usize)>,
    /// n01: repeats per note, remainder at the end (else spread evenly).
    count: Option<(u64, bool)>,
    /// n08: start subdivision, factor, step (`REP_STEP`), most per beat.
    ladder: Option<(u64, u64, usize, u64)>,
    /// n03: restitution, first interval, shortest interval.
    bounce: Option<(f64, u64, u64)>,
    /// n05: grain length, spacing (ticks), pitch spread, scatter
    /// (fraction), seed.
    grains: Option<(u64, f64, i64, f64, u64)>,
    /// n13: peak (fraction), depth (fraction).
    doppler: Option<(f64, f64)>,
    /// n14: overlap (a factor ≥ 1 on the gate).
    overlap: Option<f64>,
    /// n02: delay, feedback (fraction), echoes, pitch per echo.
    echo: Option<(u64, f64, u64, i64)>,
    /// n12: lane, values.
    cc: Option<(Lane, Vec<i64>)>,
}

impl RepNew {
    fn from(p: &Params, ctx: &Ctx) -> RepNew {
        let pct = |k: &str| p.float(k) / 100.0;
        RepNew {
            voices: p.flag("n07").then(|| {
                (
                    p.choice("n07.voices"),
                    p.choice("n07.others") == 0,
                    p.ticks("n07.tol"),
                )
            }),
            delayed: p.flag("n10").then(|| {
                (
                    pct("n10.share").clamp(0.0, 0.95),
                    p.ticks("n10.ticks"),
                    p.choice("n10.entry") == 1,
                )
            }),
            lengths: p.flag("n09").then(|| {
                let v: Vec<u64> = p
                    .list("n09.lengths")
                    .into_iter()
                    .map(|x| x.max(1) as u64)
                    .collect();
                let per = match p.choice("n09.per") {
                    1 => ADVANCE_BEAT,
                    2 => ADVANCE_CHORD,
                    _ => ADVANCE_NOTE,
                };
                (if v.is_empty() { vec![240] } else { v }, per)
            }),
            count: p.flag("n01").then(|| {
                (
                    p.int("n01.n").clamp(1, 4096) as u64,
                    p.choice("n01.rest") == 1,
                )
            }),
            ladder: p.flag("n08").then(|| {
                let most = p.int("n08.most").clamp(1, 256) as u64;
                (
                    (p.int("n08.start").clamp(1, 64) as u64).min(most),
                    p.int("n08.factor").clamp(2, 4) as u64,
                    p.choice("n08.every"),
                    most,
                )
            }),
            bounce: p.flag("n03").then(|| {
                (
                    p.float("n03.restitution").clamp(0.1, 0.95),
                    p.ticks("n03.first").max(1),
                    p.ticks("n03.shortest").max(1),
                )
            }),
            grains: p.flag("n05").then(|| {
                (
                    p.ticks("n05.len").max(1),
                    ctx.ppq.max(1) as f64 / p.float("n05.density").clamp(0.25, 256.0),
                    p.int("n05.spread").clamp(0, 24),
                    pct("n05.overlap").clamp(0.0, 0.9),
                    p.seed("n05.seed"),
                )
            }),
            doppler: p
                .flag("n13")
                .then(|| (pct("n13.peak").clamp(0.0, 1.0), pct("n13.depth").max(0.0))),
            overlap: p.flag("n14").then(|| pct("n14.overlap").max(1.0)),
            echo: p.flag("n02").then(|| {
                (
                    p.ticks("n02.delay").max(1),
                    pct("n02.feedback").clamp(0.0, 1.0),
                    p.int("n02.echoes").clamp(1, 64) as u64,
                    p.int("n02.pitch"),
                )
            }),
            cc: p.flag("n12").then(|| {
                let lane = match p.choice("n12.cc") {
                    1 => Lane::Cc(1),
                    2 => Lane::Cc(11),
                    3 => Lane::Cc(74),
                    _ => Lane::Cc(10),
                };
                let v = p.list("n12.values");
                (lane, if v.is_empty() { vec![64] } else { v })
            }),
        }
    }

    fn any(&self) -> bool {
        self.voices.is_some()
            || self.delayed.is_some()
            || self.lengths.is_some()
            || self.count.is_some()
            || self.ladder.is_some()
            || self.bounce.is_some()
            || self.grains.is_some()
            || self.doppler.is_some()
            || self.overlap.is_some()
            || self.echo.is_some()
            || self.cc.is_some()
    }
}

/// One repeat slot: start, end, nominal length (what Overflow keeps),
/// velocity factor, pitch added, a grain (its own length, no gate).
type RepSlot = (u64, u64, u64, f64, i64, bool);

/// The v0.9 Note Repeat pipeline (any new function on): the v0.8 fields,
/// then the new functions.
struct Rep9 {
    len: u64,
    gate: f64,
    overflow: bool,
    pitch_step: i64,
    sub_every: Option<(i64, i64)>,
    gap: Option<u64>,
    venv: Option<Curve>,
    genv: Option<Curve>,
    accel: Option<f64>,
    cycle: Vec<i64>,
    prob: Option<(f64, u64)>,
    pingpong: bool,
    phase: Option<(i64, i64)>,
    rec: Option<(u32, u64)>,
    new: RepNew,
    ctx: Ctx,
    /// With Control Sweeps (E0422): what each level's settings are built
    /// from.
    swept: Option<std::sync::Arc<Rebuild>>,
}

/// Note Repeat's v0.9 settings from `p` (the v0.8 fields and the functions).
fn rep9_of(p: &Params, b: &BuildCtx) -> Rep9 {
    Rep9 {
        len: note_value(p.choice("len"), p.ticks("len.ticks"), b.ctx),
        gate: p.float("gate") / 100.0,
        overflow: p.flag("overflow"),
        pitch_step: p.int("pitch.step"),
        sub_every: p.adv(1).then(|| (p.int("a1.n").max(1), p.int("a1.step"))),
        gap: p.adv(2).then(|| p.ticks("a2.gap")),
        venv: p.adv(3).then(|| p.curve("a3.curve")),
        genv: p.adv(4).then(|| p.curve("a4.curve")),
        accel: p.adv(5).then(|| -3.93 * p.float("a5.amount") / 100.0),
        cycle: if p.adv(6) {
            p.list("a6.cycle")
        } else {
            Vec::new()
        },
        prob: p
            .adv(7)
            .then(|| (p.float("a7.prob") / 100.0, p.seed("a7.seed"))),
        pingpong: p.adv(8),
        phase: p
            .adv(9)
            .then(|| (p.int("a9.cycle").max(1), p.int("a9.phase"))),
        rec: p
            .adv(10)
            .then(|| (p.int("a10.depth") as u32, p.int("a10.div") as u64)),
        new: RepNew::from(p, b.ctx),
        ctx: b.ctx.clone(),
        swept: None,
    }
}

/// n13: where normalized time `u` of equal rate lands under a rate profile
/// rising from 1 at the ends to 1 + depth at the peak (a triangle): the
/// inverse of the normalized cumulative rate `F(x) = x + depth·T(x)` (`T`
/// the triangle's integral), quadratic on each side of the peak — solved in
/// closed form (the stable root).
fn doppler_inverse(u: f64, peak: f64, depth: f64) -> f64 {
    if depth <= 0.0 {
        return u;
    }
    let m = peak.max(1.0 - peak).max(1e-9);
    let a = depth / (2.0 * m);
    // F(x) = b·x + a·x² up to the peak.
    let b = 1.0 + depth * (1.0 - peak / m);
    let f_peak = b * peak + a * peak * peak;
    // After the peak: F = F(p) + c·z − a·z², z = x − p.
    let c = 1.0 + depth;
    let total = f_peak + c * (1.0 - peak) - a * (1.0 - peak) * (1.0 - peak);
    let y = u.clamp(0.0, 1.0) * total;
    let x = if y <= f_peak {
        2.0 * y / (b + (b * b + 4.0 * a * y).sqrt())
    } else {
        let w = y - f_peak;
        peak + 2.0 * w / (c + (c * c - 4.0 * a * w).max(0.0).sqrt())
    };
    x.clamp(0.0, 1.0)
}

/// n08's step (`REP_STEP`), 0 when off.
fn step_of(nw: &RepNew) -> usize {
    nw.ladder.map_or(0, |l| l.2)
}

/// n08: the most repeats per beat a note `d` ticks long can reach — the
/// start × the factor per step period it enters (≤ ⌈d / period⌉ + 1
/// periods, a period at least `ppq · (step + 1)` for beats and half notes,
/// a beat for bars), at most the most per beat.
fn ladder_reach((start, factor, step, most): (u64, u64, usize, u64), d: u64, ppq: u64) -> u64 {
    let period = if step == 2 {
        ppq
    } else {
        ppq * (step as u64 + 1)
    };
    let steps = d.div_ceil(period.max(1));
    let mut r = start;
    for _ in 0..steps.min(64) {
        if r >= most {
            break;
        }
        r = r.saturating_mul(factor);
    }
    r.min(most)
}

impl Rep9 {
    /// The output bound (preflight): the level-0 slots of the schedule in
    /// use, the held parts, the recursion and the echoes.
    fn bound(&self, st: &super::super::SourceStats) -> Result<u64, TransformError> {
        let count = st.count as u64;
        let nw = &self.new;
        let ppq = self.ctx.ppq.max(1);
        let mut total = if let Some((_, spacing, ..)) = nw.grains {
            st.steps_bound((spacing.floor() as u64).max(1))
        } else if let Some((rest, first, shortest)) = nw.bounce {
            let j = if first >= shortest {
                ((shortest as f64 / first as f64).ln() / rest.ln()).floor() as u64 + 1
            } else {
                0
            };
            count
                .saturating_mul(j.saturating_add(2))
                .min(st.steps_bound(shortest).saturating_add(count))
        } else if let Some(ladder) = nw.ladder {
            // `Rep9::schedule` caps each note at ⌈d · r / beat⌉ + 5 slots, r
            // the most per beat it can reach (≤ the longest note's), + ⌈d /
            // beat⌉ + 2 stepping by bars (their grids need not fit a beat);
            // Σ ≤ Σ d / ⌊beat / r⌋ + 6 per note.
            let reach = ladder_reach(ladder, st.max_dur, ppq);
            let bars = if step_of(nw) == 2 {
                st.steps_bound(ppq).saturating_add(count.saturating_mul(2))
            } else {
                0
            };
            st.steps_bound((ppq / reach).max(1))
                .saturating_add(count.saturating_mul(6))
                .saturating_add(bars)
        } else if let Some((n, _)) = nw.count {
            count.saturating_mul(n)
        } else if let Some((lens, _)) = &nw.lengths {
            st.steps_bound(lens.iter().copied().min().unwrap_or(1))
                .saturating_add(count)
        } else {
            st.steps_bound(self.len).saturating_add(count)
        };
        if nw.delayed.is_some() {
            total = total.saturating_add(count);
        }
        if let Some((d, parts)) = self.rec {
            total = total.saturating_mul((2 * parts).saturating_sub(1).max(1).saturating_pow(d));
        }
        if nw.cc.is_some() && total > MAX_EVENTS as u64 {
            return Err(TransformError::TooLarge(format!(
                "The controller points could be about {}; select fewer notes.",
                crate::midi::group_digits(total)
            )));
        }
        if let Some((_, _, echoes, _)) = nw.echo {
            total = total.saturating_mul(echoes + 1);
        }
        Ok(total)
    }

    /// n07: per note, whether it repeats and its chord's latest end.
    fn voice_plan(src: &Source, (which, _, tol): (usize, bool, u64)) -> Vec<(bool, u64)> {
        let ns = &src.notes;
        let mut plan = vec![(true, 0u64); ns.len()];
        for idx in per_track(src) {
            for c in ChordGroups::of(ns, &idx, tol).iter() {
                let lo = c.iter().map(|&i| ns[i].pitch).min().unwrap_or(0);
                let hi = c.iter().map(|&i| ns[i].pitch).max().unwrap_or(0);
                let end = c.iter().map(|&i| ns[i].end()).max().unwrap_or(0);
                for &i in c {
                    let p = ns[i].pitch;
                    let rep = match which {
                        0 => p == lo,
                        1 => p == hi,
                        2 => p > lo && p < hi,
                        _ => p > lo,
                    };
                    plan[i] = (rep, end);
                }
            }
        }
        plan
    }

    /// The level-0 slots of a note's repeats over `[s0, s0 + d0)`: grains
    /// (n05), bounces (n03), a subdivision ladder (n08), an exact count
    /// (n01), a length from the cycle (n09) or the repeat length — the
    /// first on in that order; the count-based ones under the time warps
    /// (the frozen acceleration, n10's entry, n13).
    fn schedule(
        &self,
        n: &TNote,
        len: u64,
        (s0, d0): (u64, u64),
        warp_in: bool,
        v: &mut Vec<RepSlot>,
    ) {
        v.clear();
        let nw = &self.new;
        let end = s0.saturating_add(d0);
        if let Some((glen, spacing, spread, scatter, seed)) = nw.grains {
            use super::super::shared::seeded_stream::{salt_of, stream};
            let k = ((d0 as f64 / spacing).ceil() as u64)
                .clamp(1, d0.max(1))
                .min(MAX_REPEATS);
            let key = super::super::rng::note_key(n);
            let salt = salt_of("n05");
            for j in 0..k {
                let mut rng = stream(seed, salt, &[key, j]);
                let off = (rng.unit() - 0.5) * scatter * spacing;
                let x = (s0 as f64 + j as f64 * spacing + off)
                    .round()
                    .max(s0 as f64);
                let a = (x as u64).min(end.saturating_sub(1)).max(s0);
                let p = if spread > 0 {
                    rng.below(2 * spread as u64 + 1) as i64 - spread
                } else {
                    0
                };
                v.push((a, a.saturating_add(glen), glen, 1.0, p, true));
            }
            return;
        }
        if let Some((rest, first, shortest)) = nw.bounce {
            let (mut t, mut f, mut iv) = (s0, 1.0f64, first as f64);
            while iv >= shortest as f64 && (v.len() as u64) < MAX_REPEATS {
                let next = t.saturating_add(iv.round() as u64);
                if next >= end {
                    break;
                }
                v.push((t, next, next - t, f, 0, false));
                t = next;
                f *= rest;
                iv *= rest;
            }
            v.push((t, end, end - t, f, 0, false));
            return;
        }
        if let Some((start, factor, step, most)) = nw.ladder {
            let ppq = self.ctx.ppq.max(1);
            let meter = &self.ctx.meter;
            // The preflight's allowance per note (`Rep9::bound`): the most
            // per beat this note can reach, × its beats, + 5 (two partial
            // periods); the last slot allowed runs to the note's end.
            let reach = ladder_reach((start, factor, step, most), d0, ppq);
            let mut cap = (u128::from(d0) * u128::from(reach)).div_ceil(u128::from(ppq)) as u64 + 5;
            if step == 2 {
                cap += d0.div_ceil(ppq) + 2;
            }
            let cap = cap.min(MAX_REPEATS);
            let (mut t, mut rate) = (s0, start);
            while t < end && (v.len() as u64) < cap {
                // The step period holding t (beats and half notes on the
                // grid from the bar line; a bar, at least a beat long).
                let (bs, bl) = bar_bounds(meter, t);
                let (p0, len) = match step {
                    0 | 1 => {
                        let p = ppq * (step as u64 + 1);
                        (bs + (t - bs) / p * p, p)
                    }
                    _ => (bs, bl.max(ppq)),
                };
                let stop = p0.saturating_add(len).min(end).max(t + 1);
                // `rate` per beat: m equal slots on the period's own grid.
                let m = ((u128::from(rate) * u128::from(len)).div_ceil(u128::from(ppq)) as u64)
                    .clamp(1, len.max(1));
                let edge = |j: u64| {
                    p0.saturating_add((u128::from(j) * u128::from(len) / u128::from(m)) as u64)
                };
                let mut j = (u128::from(t - p0) * u128::from(m) / u128::from(len)) as u64;
                while t < stop && (v.len() as u64) < cap {
                    let mut b = edge(j + 1).min(stop).max(t + 1);
                    if v.len() as u64 + 1 == cap {
                        b = end;
                    }
                    v.push((t, b, len / m, 1.0, 0, false));
                    t = b;
                    j += 1;
                }
                rate = rate.saturating_mul(factor).min(most);
            }
            if v.is_empty() {
                v.push((s0, s0, len, 1.0, 0, false));
            }
            return;
        }
        // Count-based: k slots, linear or warped.
        let (k, linear_len) = match nw.count {
            Some((count, _)) => (count.min(d0.max(1)), None),
            None => (d0.div_ceil(len).clamp(1, MAX_REPEATS), Some(len)),
        };
        let acc = self.accel.or(warp_in.then_some(-3.93 * 0.6));
        let dop = nw.doppler.filter(|&(_, depth)| depth > 0.0);
        let at = |u: f64| {
            let mut x = u;
            if let Some((peak, depth)) = dop {
                x = doppler_inverse(x, peak, depth);
            }
            if let Some(a) = acc {
                x = warp(x, a);
            }
            s0.saturating_add((x * d0 as f64).round() as u64)
        };
        let warped = acc.is_some() || dop.is_some();
        let mut prev = if warped { at(0.0) } else { s0 };
        for r in 0..k {
            let (a, b2, full) = if warped {
                // Each edge once (the same value the next slot starts at).
                let a = prev;
                let b2 = at((r + 1) as f64 / k as f64);
                prev = b2;
                (a, b2, b2.saturating_sub(a))
            } else if let Some(l) = linear_len {
                // The v0.8 slots.
                (
                    s0.saturating_add(r.saturating_mul(l)),
                    s0.saturating_add((r + 1).saturating_mul(l).min(d0)),
                    l,
                )
            } else {
                // n01: the remainder spread evenly, or left at the end.
                let at_end = nw.count.is_some_and(|c| c.1);
                let edge = |j: u64| {
                    if at_end {
                        if j >= k { d0 } else { j * (d0 / k) }
                    } else {
                        (u128::from(j) * u128::from(d0) / u128::from(k)) as u64
                    }
                };
                let (a, b2) = (s0.saturating_add(edge(r)), s0.saturating_add(edge(r + 1)));
                (a, b2, b2 - a)
            };
            v.push((a, b2, full, 1.0, 0, false));
        }
    }

    fn run9(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        // Each note's settings under Control Sweeps (E0422).
        match &self.swept {
            Some(r) => {
                let lv: PerLevel<Rep9> = r.levels(&self.ctx, src, rep9_of);
                self.run9_at(src, out, |i| lv.at(i))
            }
            None => self.run9_at(src, out, |_| self),
        }
    }

    /// [`Rep9::run9`] with note `i`'s settings `me(i)`.
    fn run9_at<'a>(
        &'a self,
        src: &Source,
        out: &mut Output,
        me: impl Fn(usize) -> &'a Rep9,
    ) -> Result<(), TransformError> {
        let ns = &src.notes;
        let nw = &self.new;
        // Chord membership (same track and start) for ping-pong / phase, as
        // v0.8.
        let mut chord_of = vec![(0usize, 1usize); ns.len()];
        let mut i = 0;
        while i < ns.len() {
            let mut j = i;
            while j < ns.len() && ns[j].start == ns[i].start {
                j += 1;
            }
            let mut idx: Vec<usize> = (i..j).collect();
            idx.sort_by_key(|&k| (ns[k].track, ns[k].pitch, k));
            let mut a = 0;
            while a < idx.len() {
                let mut b2 = a;
                while b2 < idx.len() && ns[idx[b2]].track == ns[idx[a]].track {
                    b2 += 1;
                }
                for (r, &k) in idx[a..b2].iter().enumerate() {
                    chord_of[k] = (r, b2 - a);
                }
                a = b2;
            }
            i = j;
        }
        let plan = nw.voices.map(|v| Self::voice_plan(src, v));
        let places = nw
            .lengths
            .as_ref()
            .map(|(_, per)| place_index(src, &self.ctx.meter, *per));
        let mut slots: Vec<RepSlot> = Vec::new();
        let mut stack: Vec<(u64, u64, u32)> = Vec::new();
        for (ni, n) in ns.iter().enumerate() {
            out.set_dim(0);
            // n07: the voices that do not repeat hold or stay as written.
            if let (Some(plan), Some((_, hold, _))) = (&plan, nw.voices)
                && !plan[ni].0
            {
                let end = if hold {
                    plan[ni].1.max(n.end())
                } else {
                    n.end()
                };
                if !out.emit(
                    i128::from(n.start),
                    i128::from(end - n.start),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                ) {
                    return out.check();
                }
                continue;
            }
            let (rank, size) = chord_of[ni];
            let tr = src.track(n.track);
            let me = me(ni);
            let (mut s0, mut d0) = (n.start, n.dur);
            let mut warp_in = false;
            // n10: the note held first, the repeats after it.
            if let Some((share, ticks, accel_in)) = me.new.delayed {
                let h = ((share * d0 as f64).round() as u64)
                    .max(ticks)
                    .min(d0.saturating_sub(1));
                if h > 0 {
                    if !me.emit(
                        n,
                        (n.start, h, i64::from(n.pitch), i64::from(n.vel)),
                        None,
                        out,
                    ) {
                        return out.check();
                    }
                    s0 = s0.saturating_add(h);
                    d0 -= h;
                }
                warp_in = accel_in;
            }
            let len = match (&nw.lengths, &places) {
                (Some((lens, _)), Some(k)) => lens[(k[ni] % lens.len() as u64) as usize],
                _ => me.len,
            };
            me.schedule(n, len, (s0, d0), warp_in, &mut slots);
            let k = slots.len() as u64;
            for (r, &slot) in slots.iter().enumerate() {
                if !me.slot(n, tr, (rank, size), (r as u64, k), slot, 0, &mut stack, out) {
                    return out.check();
                }
            }
            // The frozen recursion's deeper levels (v0.8).
            while let Some((s0, d0, depth)) = stack.pop() {
                let parts = me.rec.map_or(2, |r| r.1);
                let l = (d0 / parts).max(1);
                let k = d0.div_ceil(l).clamp(1, MAX_REPEATS);
                for r in 0..k {
                    let u0 = r as f64 / k as f64;
                    let u1 = (r + 1) as f64 / k as f64;
                    let (a, b2) = match me.accel {
                        Some(acc) => (
                            s0.saturating_add((warp(u0, acc) * d0 as f64).round() as u64),
                            s0.saturating_add((warp(u1, acc) * d0 as f64).round() as u64),
                        ),
                        None => (
                            s0.saturating_add(r.saturating_mul(l)),
                            s0.saturating_add((r + 1).saturating_mul(l).min(d0)),
                        ),
                    };
                    let full = if me.accel.is_some() {
                        b2.saturating_sub(a)
                    } else {
                        l
                    };
                    let slot = (a, b2, full, 1.0, 0, false);
                    if !me.slot(n, tr, (rank, size), (r, k), slot, depth, &mut stack, out) {
                        return out.check();
                    }
                }
                out.check()?;
            }
            out.check()?;
        }
        Ok(())
    }

    /// One repeat (the v0.8 per-repeat rules, then n14's overlap on the
    /// gate, n03's velocity, n05's pitch, n12's controller point and n02's
    /// echoes); false once the output is full.
    #[allow(clippy::too_many_arguments)]
    fn slot(
        &self,
        n: &TNote,
        tr: Option<&super::super::SourceTrack>,
        (rank, size): (usize, usize),
        (r, k): (u64, u64),
        (a, b2, full, velf, padd, grain): RepSlot,
        depth: u32,
        stack: &mut Vec<(u64, u64, u32)>,
        out: &mut Output,
    ) -> bool {
        // V09-0143: the driver dimension is the repeat (0 = the first).
        out.set_dim(r.min(u64::from(u32::MAX)) as u32);
        let u0 = r as f64 / k as f64;
        let mut d = if grain {
            full
        } else {
            let slot = if self.overflow {
                full
            } else {
                b2.saturating_sub(a).max(1)
            };
            let mut g = self.gate;
            if let Some(c) = &self.genv {
                // B1-0113: an overshooting gate graph may lengthen past the slot.
                g *= c.eval(u0).max(0.0);
            }
            if let Some(o) = self.new.overlap {
                g *= o;
            }
            ((slot as f64 * g).round() as u64).max(1)
        };
        if !grain && let Some(gp) = self.gap {
            d = d.saturating_sub(gp).max(1);
        }
        if let Some((d_max, _)) = self.rec
            && depth < d_max
        {
            stack.push((a, d, depth + 1));
            return true;
        }
        if self.pingpong && size > 1 {
            let period = 2 * (size - 1);
            let ph = (r as usize) % period;
            let pick = if ph < size { ph } else { period - ph };
            if pick != rank {
                return true;
            }
        }
        if let Some((pr, seed)) = self.prob
            && r > 0
            && !Rng::for_index(seed ^ super::super::rng::note_key(n), salt::KEEP, r).chance(pr)
        {
            return true;
        }
        let mut t = *n;
        if let (Some((every, step)), Some(tr)) = (self.sub_every, tr) {
            let (id, ch) = tr.sub_offset(n.sub, (r as i64 / every) * step);
            t.sub = id;
            t.ch = ch;
        }
        if let (Some((cyc, ph)), Some(tr)) = (self.phase, tr) {
            let off = (r as i64 + ph * rank as i64).rem_euclid(cyc);
            let (id, ch) = tr.sub_offset(n.sub, off);
            t.sub = id;
            t.ch = ch;
        }
        let pitch = i64::from(n.pitch)
            + self.pitch_step * r as i64
            + if self.cycle.is_empty() {
                0
            } else {
                self.cycle[(r as usize) % self.cycle.len()]
            }
            + padd;
        let base = match &self.venv {
            Some(c) => f64::from(n.vel) * c.eval(u0),
            None => f64::from(n.vel),
        };
        let vel = (base * velf).round() as i64;
        let cc = self
            .new
            .cc
            .as_ref()
            .map(|(lane, values)| (*lane, values[(r as usize) % values.len()]));
        self.emit(&t, (a, d, pitch, vel.max(1)), cc, out)
    }

    /// Emits one note (start, length, pitch, velocity) with `t`'s fields,
    /// its controller point and n02's echoes; false once the output is
    /// full.
    fn emit(
        &self,
        t: &TNote,
        (a, d, pitch, vel): (u64, u64, i64, i64),
        cc: Option<(Lane, i64)>,
        out: &mut Output,
    ) -> bool {
        if !out.emit(i128::from(a), i128::from(d), pitch, vel, t) {
            return false;
        }
        if let Some((lane, value)) = cc {
            out.emit_event(t.track, lane, t.ch, i128::from(a), value);
        }
        if let Some((delay, fb, echoes, shift)) = self.new.echo {
            let mut v = vel as f64;
            for e in 1..=echoes {
                v *= fb;
                let ve = v.round() as i64;
                if ve < 1 {
                    break;
                }
                let at = i128::from(a) + i128::from(delay) * i128::from(e);
                if !out.emit(at, i128::from(d), pitch + shift * e as i64, ve, t) {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::doppler_inverse;

    /// Note Repeat n13: the closed-form inverse lands where the rate's
    /// numeric integral (10⁵ midpoint steps) reaches the share `u`, for
    /// peaks at the ends and inside and depths up to 400 %.
    #[test]
    fn doppler_inverse_matches_the_integral_of_the_rate() {
        for peak in [0.0, 0.2, 0.5, 0.85, 1.0] {
            for depth in [0.0, 0.5, 2.0, 4.0] {
                let m = f64::max(peak, 1.0 - peak);
                let rate = |x: f64| 1.0 + depth * (1.0 - (x - peak).abs() / m);
                let steps = 100_000;
                let h = 1.0 / steps as f64;
                let mut cum = vec![0.0; steps + 1];
                for i in 0..steps {
                    cum[i + 1] = cum[i] + rate((i as f64 + 0.5) * h) * h;
                }
                let total = cum[steps];
                for k in 0..=20 {
                    let u = k as f64 / 20.0;
                    let x = doppler_inverse(u, peak, depth);
                    let i = ((x / h) as usize).min(steps);
                    let reached = cum[i] / total;
                    assert!(
                        (reached - u).abs() < 1e-4,
                        "peak {peak} depth {depth} u {u}: x {x} reaches {reached}"
                    );
                }
                assert_eq!(doppler_inverse(0.0, peak, depth), 0.0);
                assert!((doppler_inverse(1.0, peak, depth) - 1.0).abs() < 1e-12);
            }
        }
    }
}
