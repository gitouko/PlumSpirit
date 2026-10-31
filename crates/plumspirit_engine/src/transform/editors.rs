//! Semantic parameter editors (v0.8 §185–§188): what a list parameter
//! *means*, so the tool window can show an editor made for it (bars for an
//! ordered sequence, cells for a rhythm, colour swatches for subchannel
//! cycles, a draggable quad for projective corners, …) instead of a comma
//! string. The comma text stays available as "Edit Raw Values".
//!
//! Every `List` parameter of every registered tool is listed here; the test
//! `every_list_parameter_has_a_semantic_editor` fails for a new list that is
//! not, and checks that each editor fits the parameter's range and length.

use super::param::{Keyed, ParamKind, ParamSpec, ParamValue};
use crate::model::note::MAX_PITCH;

/// What the entries of a sequence stand for (their axis labels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Labels {
    /// 1, 2, 3, …
    Index,
    /// Subchannel positions of the track (shown with their colours).
    Subchannel,
    /// C, C♯, … B (exactly twelve entries).
    PitchClass,
    Voice,
    Stage,
    Bar,
    Onset,
    Degree,
    Slice,
    Step,
    Layer,
    Stroke,
    Well,
    /// Stay, Up, Down, Leap (a walk's move weights; V09-S34).
    Move,
    /// Grid positions in the bar (metric profiles; V09-S34).
    Position,
    /// Root motions: fourth up, fifth up, third, step, tritone, chromatic
    /// mediant (Chord Progression's walk weights; V09-S35).
    Motion,
}

impl Labels {
    /// Axis label of entry `i` (0-based).
    pub fn label(self, i: usize) -> String {
        const PC: [&str; 12] = [
            "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
        ];
        match self {
            Labels::Index => format!("{}", i + 1),
            Labels::Subchannel => format!("Sub {}", i + 1),
            Labels::PitchClass => PC[i % 12].to_owned(),
            Labels::Voice => format!("Voice {}", i + 1),
            Labels::Stage => format!("Stage {}", i + 1),
            Labels::Bar => format!("Bar {}", i + 1),
            Labels::Onset => format!("Onset {}", i + 1),
            Labels::Degree => format!("Degree {}", i + 1),
            Labels::Slice => format!("Slice {}", i + 1),
            Labels::Step => format!("Step {}", i + 1),
            Labels::Layer => format!("Layer {}", i + 1),
            Labels::Stroke => format!("Stroke {}", i + 1),
            Labels::Well => format!("Well {}", i + 1),
            Labels::Position => format!("Position {}", i + 1),
            Labels::Move => ["Stay", "Up", "Down", "Leap"]
                .get(i)
                .map_or_else(|| format!("{}", i + 1), |s| (*s).to_owned()),
            Labels::Motion => ["4th", "5th", "3rd", "Step", "TT", "Med."]
                .get(i)
                .map_or_else(|| format!("{}", i + 1), |s| (*s).to_owned()),
        }
    }
}

/// How a set of points is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Independent points (wells, pins, centres).
    Free,
    /// An open path through the points in order.
    Path,
    /// A closed polygon.
    Polygon,
}

/// Units of a one-dimensional position list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Ticks,
    /// 1/48 of a beat.
    Beat48,
    /// Percent of the span.
    Percent,
    /// MIDI pitch (a keyboard).
    Pitch,
}

/// The editor of one parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Editor {
    /// Chosen from the kind (numbers, switches, menus, curves, …).
    Auto,
    /// Ordered values as draggable bars.
    Sequence(Labels),
    /// Pitch intervals in semitones: bars around 0 on a semitone grid.
    Intervals(Labels),
    /// On / off cells (rhythms, keep / remove patterns).
    Steps(Labels),
    /// One menu choice per entry.
    Choices(&'static [&'static str], Labels),
    /// Subchannel positions shown as colour swatches: an ordered cycle /
    /// palette (`set` false) or a set of subchannels (`set` true).
    Subchannels { set: bool },
    /// Points in ‰ of a box, `per` values per point (x, y, then extra
    /// values: a push vector, a direction), drawn as `shape`.
    Points { per: u8, shape: Shape },
    /// One point: x in ‰, y a MIDI pitch.
    PitchPoint,
    /// Four corners BL, BR, TR, TL (x, y each): offsets from the box corners
    /// (`offsets`) or positions in ‰ of the box.
    Quad { offsets: bool },
    /// One box x0, y0, x1, y1 in ‰.
    Rect,
    /// Values on a square grid, bottom row first (`angle`: degrees shown as
    /// arrows).
    Lattice { angle: bool },
    /// Node offsets (x, y per node) of a square mesh, row by row from the
    /// bottom; the side follows the length.
    Mesh,
    /// A set of pitch classes (an octave of keys).
    PitchClasses,
    /// Pitch-class pairs: from → to.
    PitchClassMap,
    /// (low, high) pitch pairs, one per entry.
    Ranges(Labels),
    /// Track numbers (1-based) chosen from the project's tracks.
    Tracks,
    /// Positions on a line, ascending.
    Positions(Unit),
    /// A set of whole numbers within the range (toggle cells).
    NumberSet,
    /// Pairs of 1-based positions (a, b, …).
    IndexPairs,
    /// A colour (red, green, blue).
    Rgb,
    /// The four values of a cubic Bézier (start, handle 1, handle 2, end),
    /// shown as bars with the curve.
    Bezier,
}

/// Choices for scale lists (index = entry of `theory::SCALES`).
pub fn scale_names() -> &'static [&'static str] {
    super::theory::scale_names()
}

const ARTIC: &[&str] = &["Legato", "Portato", "Staccato"];
const UP_DOWN: &[&str] = &["Up", "Down"];
const STROKES: &[&str] = &["Down", "Up", "None"];

/// The editor of parameter `p` of tool `tool`.
/// Chord Stamp's chord types (theory `CHORDS` order) and "(unlisted)" (its
/// n01 map; V09-S35).
const CHORD_TYPES_UNLISTED: &[&str] = &[
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
    "(unlisted)",
];
const CHORD_TYPES: &[&str] = CHORD_TYPES_UNLISTED.split_at(21).0;
/// Riff Machine section letters (its n03 form; V09-S35).
const SECTIONS: &[&str] = &["A", "B", "C", "D", "E", "F", "G", "H"];

pub fn editor(tool: &str, p: &ParamSpec) -> Editor {
    if !matches!(p.kind, ParamKind::List { .. }) {
        return Editor::Auto;
    }
    use Editor as E;
    use Labels as L;
    // v0.9 mandatory functions (every eligible tool): signed offsets in turn.
    if matches!(p.key, "m.sub.cycle" | "m.pitch.cycle") {
        return E::Intervals(L::Step);
    }
    match (tool, p.key) {
        // ----- ordered sequences
        ("arpeggiate", "custom") | ("arpeggiate", "pvel") | ("arpeggiate", "n02.repeats") => {
            E::Sequence(L::Step)
        }
        ("arpeggiate", "n09.w") => E::Sequence(L::Move),
        ("arpeggiate", "a5.layers") | ("pulse_field", "a7.cycles") => E::Sequence(L::Layer),
        ("articulate", "a9.ratios") | ("quantize", "groove.vel") => E::Sequence(L::Onset),
        ("articulate", "n02.gates") => E::Sequence(L::Position),
        ("randomize", "n10.weights") | ("randomize", "n10.vel") => E::Sequence(L::Position),
        ("randomize", "n03.chords")
        | ("randomize", "n08.delays")
        | ("randomize", "n09.subs")
        | ("randomize", "n09.weights") => E::Sequence(L::Index),
        ("randomize", "n06.transpose") => E::Intervals(L::Index),
        ("kaleidoscope", "n05.semis") => E::Intervals(L::Index),
        ("kaleidoscope", "n06.sectors") => E::Sequence(L::Index),
        ("kaleidoscope", "n10.pattern") => E::Steps(L::Index),
        ("orbit", "n05.points") => E::NumberSet,
        ("strum", "n08.times") => E::Sequence(L::Index),
        ("articulate", "n07.ratios") => E::Sequence(L::Index),
        ("articulate", "n08.ranks") => E::Sequence(L::Voice),
        ("chop", "n04.list") => E::Sequence(L::Voice),
        ("chop", "n09.factors") => E::Sequence(L::Position),
        ("claw", "n03.periods") => E::Sequence(L::Index),
        ("claw", "n05.order") => E::Sequence(L::Slice),
        ("chop", "custom") | ("chop", "pvel") | ("chop", "a4.seq") | ("claw", "a10.map") => {
            E::Sequence(L::Slice)
        }
        ("claw", "a1.stages") | ("riff", "a1.order") => E::Sequence(L::Stage),
        ("quantize", "a4.grids")
        | ("distort", "a2.top")
        | ("distort", "a2.bottom")
        | ("distort", "n04.points")
        | ("warp", "n03.order")
        | ("warp", "n07.nodes")
        | ("shear", "a3.points")
        | ("wave", "a3.phases") => E::Sequence(L::Index),
        ("quantize", "a5.offsets") | ("randomize", "a3.prob") => E::Sequence(L::PitchClass),
        ("randomize", "a2.weights") | ("snap_scale", "a2.weights") => E::Sequence(L::Degree),
        ("chord_stamp", "a7.vel")
        | ("chord_stamp", "n09.hits")
        | ("chord_stamp", "n10.octaves")
        | ("paste_repeat_notes", "a8.phases") => E::Sequence(L::Voice),
        ("riff", "s1.degrees") => E::Sequence(L::Bar),
        ("riff", "n02.octaves") | ("riff", "n02.stages") => E::Sequence(L::Layer),
        ("riff", "n09.accents") | ("humanize", "n04.accents") => E::Sequence(L::Position),
        ("draw", "n09.rhythm") | ("draw", "n09.accents") => E::Sequence(L::Step),
        ("flam", "a6.seq") | ("flam", "n06.offsets") => E::Sequence(L::Stroke),
        ("flam", "n02.strokes") => E::Sequence(L::Position),
        ("flam", "n04.channels") | ("chord_progression", "n01.beats") => E::Sequence(L::Index),
        ("chord_progression", "n09.weights") => E::Sequence(L::Motion),
        ("gravity_well", "a2.signs") => E::Sequence(L::Well),
        ("claw", "a6.phase")
        | ("quantize", "a8.phase")
        | ("randomize", "a4.weights")
        | ("strum", "a6.delays")
        | ("slice", "a7.cuts")
        | ("stretch", "a6.factors")
        | ("humanize", "a9.bias")
        | ("humanize", "a9.scale")
        | ("note_props", "a8.table")
        | ("slide", "a8.ranges")
        | ("shear", "a8.table")
        | ("wave", "a10.phases")
        | ("gravity_well", "a8.mass")
        | ("pulse_field", "a8.phases")
        | ("flow_field", "a9.angles") => E::Sequence(L::Subchannel),
        ("shear", "a4.handles") => E::Bezier,
        // ----- intervals
        ("chord_stamp", "a1.intervals")
        | ("repeat", "a6.cycle")
        | ("paint", "a3.chord")
        | ("draw", "n04.chord")
        | ("slide", "a4.nodes") => E::Intervals(L::Index),
        ("draw", "n05.lines") => E::Intervals(L::Index),
        ("draw", "n10.pattern") => E::Intervals(L::Step),
        ("transpose", "a9.seq") => E::Intervals(L::Onset),
        ("transpose", "n05.roots") => E::Intervals(L::Index),
        ("repeat", "n12.values") | ("repeat", "n09.lengths") | ("group", "n02.list") => {
            E::Sequence(L::Index)
        }
        ("group", "n08.degrees") => E::Intervals(L::Index),
        ("note_props", "n03.values")
        | ("note_props", "n08.splits")
        | ("note_props", "n08.values")
        | ("note_props", "n11.table") => E::Sequence(L::Index),
        ("paste_repeat_notes", "a2.seq") | ("paste_repeat_clips", "a5.seq") => {
            E::Intervals(L::Step)
        }
        ("transpose", "a5.table") => E::Intervals(L::Subchannel),
        ("chop", "ptrans") => E::Intervals(L::Slice),
        // ----- on / off cells
        ("chop", "holds") | ("claw", "a5.mask") => E::Steps(L::Slice),
        ("delete", "a5.pattern") => E::Steps(L::Index),
        ("delete", "n04.mask") | ("select", "n03.mask") => E::Steps(L::Position),
        ("particle_burst", "n01.pattern") => E::Steps(L::Position),
        ("flam", "n02.mask") => E::Steps(L::Position),
        ("paint", "a9.pattern") => E::Steps(L::Step),
        ("gen.lines", "c12_dot_pattern.pattern") => E::Steps(L::Step),
        ("paint", "n04.pattern") => E::Steps(L::Index),
        ("snap_scale", "n03.cents") => E::Sequence(L::Index),
        // ----- one choice per entry
        ("chord_progression", "n11.chain") => E::Choices(&["P", "L", "R"], L::Index),
        ("chord_stamp", "n01.map") => E::Choices(CHORD_TYPES_UNLISTED, L::PitchClass),
        ("chord_stamp", "n07.types") => E::Choices(CHORD_TYPES, L::Index),
        ("riff", "n03.form") => E::Choices(SECTIONS, L::Index),
        ("articulate", "a6.seq") => E::Choices(ARTIC, L::Onset),
        ("strum", "a3.seq") => E::Choices(UP_DOWN, L::Index),
        ("strum", "n02.dirs") => E::Choices(STROKES, L::Position),
        ("snap_scale", "a6.scales") => E::Choices(scale_names(), L::Bar),
        ("snap_scale", "a7.scales") => E::Choices(scale_names(), L::Subchannel),
        // ----- subchannels
        ("delete", "a3.subs") | ("select", "a4.subs") | ("limit", "n09.subs") => {
            E::Subchannels { set: true }
        }
        ("arpeggiate", "a6.seq")
        | ("draw", "a6.subs")
        | ("draw", "n11.palette")
        | ("snap_scale", "n07.palette")
        | ("flip", "n07.pairs")
        | ("paint", "a5.subs")
        | ("chord_progression", "a8.seq")
        | ("chord_stamp", "a6.seq")
        | ("paint_color", _)
        | ("paste_repeat_clips", "a6.list")
        | ("perspective", "n02.palette")
        | ("shear", "n11.palette")
        | ("pulse_field", "n11.palette")
        | ("particle_burst", "n07.palette")
        | ("flow_field", "n03.palette")
        | ("gen.polygons", "c18_per_edge_subchannels.subs")
        | ("gen.custom", "c13_per_edge_subchannel.subs") => E::Subchannels { set: false },
        // ----- geometry
        ("select", "a1.path")
        | ("warp", "n09.path")
        | ("stretch", "n03.anchors")
        | ("flow_field", "n10.path") => E::Points {
            per: 2,
            shape: Shape::Path,
        },
        ("select", "a2.poly")
        | ("clip_mask", "n01.polygon")
        | ("gen.polygons", "c03_irregular_polygon.points")
        | ("gen.custom", "c01_arbitrary_vertex_placement.points")
        | ("gen.custom", "c19_shape_union.points")
        | ("gen.custom", "c20_shape_subtract.points")
        | ("gen.custom", "c21_shape_intersection.points")
        | ("gen.custom", "c24_shape_a_b_morph.points") => E::Points {
            per: 2,
            shape: Shape::Polygon,
        },
        ("gen.custom", "c05_multiple_contours.points")
        | ("gen.custom", "c06_interior_holes.points") => E::Points {
            per: 2,
            shape: Shape::Free,
        },
        ("warp", "a2.nodes") => E::Points {
            per: 4,
            shape: Shape::Free,
        },
        ("warp", "a4.pins")
        | ("twirl", "a4.centres")
        | ("gravity_well", "a1.wells")
        | ("slice", "a3.centre") => E::Points {
            per: 2,
            shape: Shape::Free,
        },
        // V09-S50: Tumours' attractors (time ‰, pitch ‰, strength %).
        ("tumours", "c23_attractor_repulsor_field.points") => E::Points {
            per: 3,
            shape: Shape::Free,
        },
        // V09-S50: Tumours' anchor seeds.
        ("tumours", "c14_growth_seed_mode.anchors") => E::Points {
            per: 2,
            shape: Shape::Free,
        },
        ("flip", "a1.pivot") => E::PitchPoint,
        ("distort", "a1.corners") => E::Quad { offsets: true },
        ("perspective", "a1.corners") | ("perspective", "n06.corners") => {
            E::Quad { offsets: false }
        }
        ("delete", "a9.box") => E::Rect,
        ("flow_field", "a4.angles") => E::Lattice { angle: true },
        ("warp", "a1.nodes") | ("warp", "a10.b") | ("gen.custom", "c23_mesh_warp.nodes") => E::Mesh,
        // ----- pitch
        ("limit", "a1.set") | ("limit", "a9.exclude") | ("snap_scale", "a1.pcs") => E::PitchClasses,
        ("transpose", "a10.map") => E::PitchClassMap,
        ("limit", "a3.ranges") => E::Ranges(L::Subchannel),
        ("riff", "a7.splits") => E::Positions(Unit::Pitch),
        // ----- positions, sets, misc
        ("slice", "a1.ticks") | ("slice_notes", "at") | ("duplicate", "n03.ticks") => {
            E::Positions(Unit::Ticks)
        }
        ("quantize", "groove.pos") | ("quantize", "a6.pos") => E::Positions(Unit::Beat48),
        ("chord_stamp", "a8.onsets") => E::Positions(Unit::Percent),
        ("chord_progression", "regen")
        | ("quantize", "n08.odd")
        | ("quantize", "n10.mult")
        | ("quantize", "n11.allowed")
        | ("limit", "n09.channels")
        | ("chord_progression", "n03.positions")
        | ("chord_progression", "n07.positions")
        | ("chord_progression", "n08.degrees")
        | ("chord_stamp", "n12.avoid")
        | ("chord_stamp", "n05.skip")
        | ("chord_stamp", "n06.voices")
        | ("riff", "n06.allowed")
        | ("slide", "n10.pool")
        | ("delete", "n10.intervals")
        | ("duplicate", "n08.ccs") => E::NumberSet,
        ("chord_progression", "swap") => E::IndexPairs,
        ("clip_mask", "tracks")
        | ("duplicate", "n01.tracks")
        | ("gen.growth", "c21_external_track_field.tracks")
        | ("tumours", "c24_mask_boundary_constraint.tracks") => {
            E::Tracks
        }
        // Copy k at the k-th offset: the order is the copies' (no sorting).
        ("duplicate", "n11.offsets") => E::Sequence(L::Index),
        // V09-S52: Lines' polyline heights (% of the box, at even times).
        ("gen.lines", "c03_polyline_segments.heights") => E::Sequence(L::Index),
        // V09-S56: Polygons' cells per note by edge (cycled).
        ("gen.polygons", "c20_per_edge_density.every")
        | ("gen.custom", "c02_bezier_handles_per_vertex.handles")
        | ("gen.custom", "c05_multiple_contours.sizes")
        | ("gen.custom", "c06_interior_holes.sizes")
        | ("gen.custom", "c11_per_edge_density.every")
        | ("gen.custom", "c12_per_vertex_density.share")
        | ("gen.custom", "c14_per_vertex_velocity.vel")
        // V09-S59: the L-system's weight k for rule k.
        | ("gen.lsystem", "c09_rule_weights.weights")
        // V09-S60: the spirograph's circles and harmonics, in order.
        | ("gen.spirograph", "c14_fourier_harmonic_sum.freqs")
        | ("gen.spirograph", "c15_harmonic_amplitudes.amps")
        | ("gen.spirograph", "c16_harmonic_phases.phases")
        // V09-S61: the growth's points (x, y pairs).
        | ("gen.growth", "c01_seed_points.points")
        | ("gen.growth", "c06_attraction_field.points")
        | ("gen.growth", "c07_repulsion_field.points") => E::Sequence(L::Index),
        ("stretch", "n09.factors") => E::Sequence(L::Bar),
        ("group", "a5.rgb") => E::Rgb,
        // v0.8 Paint (`paint.rs`) declares its own.
        _ => super::tools::paint::editor(p.key)
            .filter(|_| tool == super::tools::paint::ID)
            .unwrap_or(E::Auto),
    }
}

/// Why `e` cannot edit `p` (range / length / default), or `None`.
pub fn misfit(e: Editor, p: &ParamSpec) -> Option<String> {
    let ParamKind::List { min, max, max_len } = p.kind else {
        return (e != Editor::Auto).then(|| "editor on a non-list parameter".into());
    };
    let def = match &p.default {
        ParamValue::List(v) => v.clone(),
        _ => return Some("default is not a list".into()),
    };
    let need = |ok: bool, why: &str| (!ok).then(|| why.to_owned());
    match e {
        Editor::Auto => Some("no semantic editor".into()),
        Editor::Sequence(Labels::PitchClass) => need(max_len == 12, "C … B needs 12 entries"),
        Editor::Sequence(_) | Editor::Intervals(_) | Editor::Bezier => {
            need(min < max, "empty range")
        }
        Editor::Steps(_) => need(min == 0 && max == 1, "cells are 0 / 1"),
        Editor::Choices(c, _) => need(
            min == 0 && (max as usize) < c.len(),
            "choice index outside the menu",
        ),
        Editor::Subchannels { .. } => need(min >= 0, "negative subchannel position"),
        Editor::Points { per, .. } => need(
            max_len % usize::from(per) == 0 && def.len() % usize::from(per) == 0,
            "length not a multiple of the values per point",
        ),
        Editor::PitchPoint => need(max_len == 2, "one point"),
        Editor::Quad { .. } => need(max_len == 8 && def.len() == 8, "four corners"),
        Editor::Rect => need(max_len == 4 && def.len() == 4, "one box"),
        Editor::Lattice { .. } => need(
            {
                let s = (def.len() as f64).sqrt() as usize;
                s * s == def.len()
            },
            "not a square grid",
        ),
        Editor::Mesh => need(
            {
                let s = ((def.len() / 2) as f64).sqrt() as usize;
                def.len() % 2 == 0 && s * s * 2 == def.len()
            },
            "not a square mesh",
        ),
        Editor::PitchClasses => need(min == 0 && max == 11, "pitch classes are 0 … 11"),
        Editor::PitchClassMap => need(
            min == 0 && max == 11 && max_len % 2 == 0,
            "pitch-class pairs",
        ),
        // Pitch lists are declared for the standard keyboard and keyed (their
        // top reaches 255 in a 256-Key Mode project, `ParamSpec::range_in`).
        Editor::Ranges(_) => need(
            max_len % 2 == 0 && min >= 0 && max <= i64::from(MAX_PITCH) && p.keyed == Keyed::Keys,
            "keyed pitch pairs",
        ),
        Editor::Tracks => need(min >= 1, "track numbers are 1-based"),
        Editor::Positions(Unit::Pitch) => need(
            min >= 0 && max <= i64::from(MAX_PITCH) && p.keyed == Keyed::Keys,
            "keyed pitches",
        ),
        Editor::Positions(Unit::Percent) => need(min >= 0 && max <= 100, "percent"),
        Editor::Positions(_) => need(min >= 0, "negative position"),
        Editor::NumberSet => need(max - min <= 128, "too many cells"),
        Editor::IndexPairs => need(max_len % 2 == 0 && min >= 1, "1-based pairs"),
        Editor::Rgb => need(max_len == 3 && min == 0 && max == 255, "red, green, blue"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_list_parameter_has_a_semantic_editor() {
        let mut lists = 0;
        for t in super::super::registry() {
            for p in &(t.spec)().params {
                let e = editor(t.id, p);
                if matches!(p.kind, ParamKind::List { .. }) {
                    lists += 1;
                    if let Some(why) = misfit(e, p) {
                        panic!("{} {} ({}): {why} — {e:?}", t.id, p.key, p.label);
                    }
                } else {
                    assert_eq!(e, Editor::Auto, "{} {}", t.id, p.key);
                }
            }
        }
        // v0.9 removed Interpolate, LFO, Level Scaling and Advanced Mute
        // (their list parameters went with them).
        assert!(lists >= 112, "{lists}");
    }

    #[test]
    fn labels_name_their_entries() {
        assert_eq!(Labels::PitchClass.label(1), "C♯");
        assert_eq!(Labels::Subchannel.label(0), "Sub 1");
        assert_eq!(Labels::Index.label(4), "5");
    }
}
