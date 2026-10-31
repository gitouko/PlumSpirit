//! Clipping Mask (contract §83–85) and Paint Color (§99–100).
//!
//! **Clipping Mask** treats notes as rectangles (x = time, y = pitch row). The
//! *mask* is the union of the base track's notes (`spatial::RowMask`); the *art*
//! is the selection. The result is the art clipped to the mask (notes split at
//! the mask's edges). Nothing outside the mask is deleted unless "Replace the
//! art" is chosen; Vice Versa swaps mask and art. Masking is
//! `O((n + m) log m)`.
//!
//! **Paint Color** is subchannel painting: notes change subchannel identity
//! (colour), never their MIDI channel unless asked.

use super::super::boundary::{self, PitchPolicy};
use super::super::output::NEW_SUBTRACK;
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, salt};
use super::super::spatial::RowMask;
use super::super::{
    Estimate, Growth, Kernel, Output, Source, SourceStats, SourceTrack, TNote, TrackScope,
    TransformError,
};
use super::{Contextual, NOTE_VALUES, boxed, note_value, pos};
use crate::model::PlumDocument;

// =============================================================== Clipping Mask

pub const CLIP_MASK: ToolDef = ToolDef {
    id: "clip_mask",
    spec: clip_spec,
    build: clip_build,
};

const BASES: &[&str] = &["The track below", "The track above", "Track number…"];
const RESULTS: &[&str] = &[
    "Add to the mask's track (art untouched)",
    "Replace the art with the clipped part",
    "Add to the art's track",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const CLIP_MASK_HELP: &[(&str, &str)] = &[
    (
        "base",
        "The track whose notes form the mask: the track below the selection's, the one above, or a track by number.",
    ),
    (
        "base.n",
        "For Track Number: the mask track's number in the track list.",
    ),
    (
        "vice",
        "Swaps the roles: the selected notes become the mask and the base track's notes are clipped by them.",
    ),
    (
        "result",
        "Where the clipped notes go: added to the mask's track (the art stays), replacing the art, or added to the art's track.",
    ),
    (
        "tracks",
        "Further mask tracks by number, combined by the union, intersection or subtract functions below.",
    ),
    (
        "a1",
        "Keeps the parts of the notes outside the mask instead of inside it.",
    ),
    (
        "a2",
        "Thins the clipped notes near the mask's edge at random (seeded), for a soft edge.",
    ),
    (
        "a2.width",
        "How far from the edge, in ticks, the thinning reaches.",
    ),
    ("a3", "Grows the mask before clipping."),
    ("a3.ticks", "How far the mask grows in time, in ticks."),
    ("a3.rows", "How many keys the mask grows up and down."),
    ("a4", "Shrinks the mask before clipping."),
    ("a4.ticks", "How far the mask shrinks in time, in ticks."),
    ("a4.rows", "How many keys the mask shrinks up and down."),
    (
        "a5",
        "Uses the union of the base track and the further mask tracks as the mask.",
    ),
    (
        "a6",
        "Uses only where the base track and every further mask track overlap as the mask.",
    ),
    (
        "a7",
        "Uses the base track minus the further mask tracks as the mask.",
    ),
    (
        "a8",
        "Clips each subchannel only by mask notes of the same subchannel.",
    ),
    (
        "a9",
        "Slides the mask in time across the selection: no offset at its start, the offset below at its end.",
    ),
    (
        "a9.ticks",
        "How far the mask has moved by the selection's end, in ticks.",
    ),
    (
        "a10",
        "Puts the result into a new subtrack instead of an existing track.",
    ),
    (
        "n01",
        "Uses a drawn shape as the mask instead of (or with) a track.",
    ),
    (
        "n01.shape",
        "The shape: an ellipse, a rectangle, a triangle, a star, or your polygon.",
    ),
    (
        "n01.cx",
        "The shape's center in time, as a share of the selection's span.",
    ),
    (
        "n01.cy",
        "The shape's center in pitch, as a share of the selection's range.",
    ),
    (
        "n01.size",
        "The shape's size, as a share of the selection's box.",
    ),
    (
        "n01.polygon",
        "For Polygon: the corners as x, y pairs in thousandths of the shape's square.",
    ),
    (
        "n02",
        "Uses only the base notes within a velocity range as the mask.",
    ),
    ("n02.lo", "The lowest velocity of a mask note."),
    ("n02.hi", "The highest velocity of a mask note."),
    (
        "n03",
        "Fades the clipped notes' velocity toward the mask's edge.",
    ),
    (
        "n03.width",
        "How far from the edge, in ticks, the fade reaches.",
    ),
    ("n03.rows", "How many keys from the edge the fade reaches."),
    (
        "n03.floor",
        "The velocity at the very edge, as a share of the note's.",
    ),
    ("n04", "Moves and scales the mask before clipping."),
    ("n04.dx", "Moves the mask in time, in ticks."),
    ("n04.dy", "Moves the mask in pitch, in keys."),
    ("n04.sx", "Stretches the mask in time."),
    ("n04.sy", "Stretches the mask in pitch."),
    (
        "n05",
        "Clips to a stroke along the mask's outline instead of its whole area.",
    ),
    ("n05.width", "The stroke's width in time, in ticks."),
    ("n05.rows", "The stroke's height in pitch, in keys."),
    (
        "n05.side",
        "Where the stroke lies: inside the outline, outside it, or centered on it.",
    ),
    ("n06", "Repeats the mask along time."),
    ("n06.period", "The distance between repeats, in ticks."),
    ("n06.count", "How many times the mask repeats."),
    ("n06.mirror", "Mirrors every other repeat in time."),
    (
        "n07",
        "Masks by density: only places where the base track has many notes count.",
    ),
    (
        "n07.window",
        "The time, in ticks, over which base notes are counted.",
    ),
    (
        "n07.threshold",
        "The fewest base notes that make a place part of the mask.",
    ),
    ("n07.rows", "How many keys above and below are counted too."),
    (
        "n08",
        "Keeps the notes outside the mask too, marked differently.",
    ),
    (
        "n08.mode",
        "Outside notes move to another subchannel, get a scaled velocity, or are removed.",
    ),
    (
        "n08.offset",
        "For Subchannel Offset: how many subchannels the outside notes move.",
    ),
    (
        "n08.vel",
        "For Velocity Scaled: the outside notes' velocity as a share of their own.",
    ),
    (
        "n09",
        "Uses only the mask's timing: any mask note sounding at a time opens the gate, at any key or near the note.",
    ),
    (
        "n09.rows",
        "Any Key opens the gate whatever the pitch; Within the Rows needs a mask note near the note's key.",
    ),
    (
        "n09.within",
        "For Within the Rows: how many keys away a mask note may be.",
    ),
    (
        "n10",
        "Masks by marker sections, chapters or the time selection.",
    ),
    (
        "n10.ranges",
        "The ranges used: marker sections, chapters, or the time selection.",
    ),
    (
        "n10.keep",
        "Keep the notes inside the ranges, or outside them.",
    ),
    (
        "n11",
        "Copies a property of the mask notes onto the clipped notes under them.",
    ),
    (
        "n11.prop",
        "The property copied: velocity, subchannel or channel.",
    ),
    (
        "n11.mode",
        "For Velocity: replace it, multiply by it, or average with it.",
    ),
    ("n11.amount", "How much of the copied value applies."),
];

fn clip_spec() -> ToolSpec {
    let mut p = vec![
        choice("base", "Base (mask) track", BASES, 0),
        int("base.n", "Base track number", 1, 65_535, 1),
        flag(
            "vice",
            "Vice Versa (the selection masks the base track's notes)",
            false,
        ),
        choice("result", "Result", RESULTS, 0),
        list(
            "tracks",
            "More mask tracks (numbers, for union / intersection / subtract)",
            1,
            65_535,
            64,
            &[3],
        ),
        seed("seed"),
        adv_switch(1, "Invert mask"),
        adv_switch(2, "Soft edge (thinning)"),
        ticks("a2.width", "Edge width (ticks)", 1, 1 << 30, 480).adv(2),
        adv_switch(3, "Expand mask"),
        ticks("a3.ticks", "Expand in time (ticks)", 0, 1 << 30, 120).adv(3),
        int("a3.rows", "Expand in pitch (rows)", 0, 24, 1).adv(3),
        adv_switch(4, "Contract mask"),
        ticks("a4.ticks", "Contract in time (ticks)", 0, 1 << 30, 60).adv(4),
        int("a4.rows", "Contract in pitch (rows)", 0, 24, 0).adv(4),
        adv_switch(5, "Multi-track union mask"),
        adv_switch(6, "Multi-track intersection mask"),
        adv_switch(7, "Subtract mask"),
        adv_switch(8, "Subchannel-sensitive masking"),
        adv_switch(9, "Animated / time-shifted mask"),
        int(
            "a9.ticks",
            "Mask offset at the end of the selection (ticks)",
            -1_000_000_000,
            1_000_000_000,
            480,
        )
        .adv(9),
        adv_switch(10, "Result to a new subtrack"),
        // V09-S38 step 3: functions declared by stable id (TOOL_CATALOG
        // n01 … n11).
        flag("n01", "Shape mask", false),
        choice("n01.shape", "Shape", SHAPES, 0),
        float("n01.cx", "Center X", 0.0, 100.0, 50.0, "%"),
        float("n01.cy", "Center Y", 0.0, 100.0, 50.0, "%"),
        float("n01.size", "Size", 1.0, 200.0, 100.0, "%"),
        list(
            "n01.polygon",
            "Polygon (x, y ‰ of the shape's square)",
            0,
            1000,
            64,
            &[500, 0, 1000, 500, 500, 1000, 0, 500],
        ),
        flag("n02", "Velocity-filtered base", false),
        int("n02.lo", "Lowest velocity", 1, 127, 64),
        int("n02.hi", "Highest velocity", 1, 127, 127),
        flag("n03", "Edge fade", false),
        ticks("n03.width", "Fade width", 0, 1 << 30, 240),
        int("n03.rows", "Fade rows", 0, 24, 1),
        float("n03.floor", "Floor", 0.0, 100.0, 20.0, "%"),
        flag("n04", "Mask transform", false),
        int("n04.dx", "Offset X (ticks)", -100_000_000, 100_000_000, 480),
        int("n04.dy", "Offset Y (rows)", -127, 127, 0),
        float("n04.sx", "Scale X", 1.0, 1000.0, 100.0, "%"),
        float("n04.sy", "Scale Y", 1.0, 1000.0, 100.0, "%"),
        flag("n05", "Outline stroke", false),
        ticks("n05.width", "Stroke width", 1, 1 << 30, 120),
        int("n05.rows", "Stroke rows", 1, 24, 1),
        choice("n05.side", "Side", SIDES, 0),
        flag("n06", "Mask tile", false),
        ticks("n06.period", "Period", 1, 1 << 30, 3840),
        int("n06.count", "Count", 1, 1024, 4),
        choice("n06.mirror", "Mirror alternate", NO_YES, 0),
        flag("n07", "Density mask", false),
        ticks("n07.window", "Window", 1, 1 << 30, 960),
        int("n07.threshold", "Threshold (notes)", 1, 100_000, 4),
        int("n07.rows", "Rows", 0, 24, 0),
        flag("n08", "Split result", false),
        choice("n08.mode", "Outside notes", OUTSIDE, 0),
        int("n08.offset", "Subchannel offset", -4095, 4095, 1),
        float("n08.vel", "Outside velocity", 0.0, 150.0, 50.0, "%"),
        flag("n09", "Time gate", false),
        choice("n09.rows", "Rows", GATE_ROWS, 0),
        int("n09.within", "Within (rows)", 0, 48, 2),
        flag("n10", "Marker mask", false),
        choice("n10.ranges", "Ranges", RANGES, 0),
        choice("n10.keep", "Keep", KEEP, 0),
        flag("n11", "Base property transfer", false),
        choice("n11.prop", "Property", TRANSFER, 0),
        choice("n11.mode", "Mode (velocity)", TRANSFER_MODES, 0),
        float("n11.amount", "Amount", 0.0, 100.0, 100.0, "%"),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, CLIP_MASK_HELP);
    ToolSpec {
        id: "clip_mask",
        name: "Clipping Mask",
        family: Family::Geometry,
        summary: "Keeps only the parts of the selected notes that lie inside the base track's notes (notes are rectangles in time × pitch). The result is added to the base track by default; the selection stays as it is.",
        params: p,
        advanced: vec![
            "Invert mask",
            "Soft edge",
            "Expand mask",
            "Contract mask",
            "Multi-track union",
            "Multi-track intersection",
            "Subtract mask",
            "Subchannel-sensitive masking",
            "Animated / time-shifted mask",
            "Result to a new subtrack",
        ],
        // The catalog's orders = the run order (`Clip9`).
        functions: [
            ("n01", "Shape mask", 110),
            ("n10", "Marker mask", 120),
            ("n02", "Velocity-filtered base", 130),
            ("n07", "Density mask", 140),
            ("n09", "Time gate", 150),
            ("n04", "Mask transform", 160),
            ("n05", "Outline stroke", 170),
            ("n06", "Mask tile", 180),
            ("n08", "Split result", 190),
            ("n03", "Edge fade", 510),
            ("n11", "Base property transfer", 520),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// Adds the base track (layer 0, also a destination) and the extra mask tracks
/// (layer 1) to the source. With a generated mask (v0.9 n01 Shape mask, n10
/// Marker mask) the base track is needed only as the art (Vice Versa) or as
/// the destination (Add to the mask's track); without one the source gets
/// the extra layer alone. B2-F16: every track is read as the song shows it
/// (its clips, at their places) on the edited notes' ticks, the base counted
/// from the track the edited clip sits on, the results going into the clip
/// the base track plays ([`placed_destination`](super::super::source::placed_destination)).
pub fn prepare(
    doc: &PlumDocument,
    scopes: &[TrackScope],
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    let Some((active, shift)) = super::super::source::edit_place(scopes, ctx) else {
        return Ok(());
    };
    let base = match p.choice("base") {
        0 => active + 1,
        1 => active.wrapping_sub(1),
        _ => (p.int("base.n").max(1) - 1) as usize,
    };
    let extras = |skip: usize| -> Vec<TNote> {
        p.list("tracks")
            .iter()
            .filter_map(|&n| usize::try_from(n - 1).ok())
            .filter(|&i| i != skip && doc.track(i).is_some())
            .flat_map(|i| Source::placed_layer(doc, i, u16::MAX, limit, shift))
            .collect()
    };
    if base == active || doc.track(base).is_none() {
        if !needs_base(p) {
            src.aux = std::sync::Arc::new(vec![Vec::new(), extras(base)]);
            src.aux_slot = None;
            return Ok(());
        }
        return Err(TransformError::Invalid(
            "Choose a base (mask) track: another existing track than the one being edited.".into(),
        ));
    }
    let dest = super::super::source::placed_destination(doc, base, shift)
        .ok_or_else(|| TransformError::Invalid("The base track does not exist.".into()))?;
    let layer = src.placed_layer_slots(doc, base, limit, shift)?;
    let slot = match src
        .tracks
        .iter()
        .position(|t| t.host() == dest.host() && t.frame == dest.frame)
    {
        Some(s) => s,
        None => {
            src.tracks.push(dest);
            src.tracks.len() - 1
        }
    } as u16;
    src.aux = std::sync::Arc::new(vec![layer, extras(base)]);
    src.aux_slot = Some(slot);
    Ok(())
}

/// Whether the run needs a base track: always, except with a generated mask
/// (n01, n10) whose result does not go to the base track and whose art is
/// the selection.
fn needs_base(p: &Params) -> bool {
    !(p.flag("n01") || p.flag("n10")) || p.flag("vice") || p.choice("result") == 0
}

/// Subchannel `id` of track `t`, or the track's first one (keeps the MIDI
/// channel of the note: only the colour identity is mapped).
fn sub_in(t: &SourceTrack, id: u32) -> u32 {
    if t.sub_index(id).is_some() {
        id
    } else {
        t.subs.first().map_or(0, |s| s.0)
    }
}

fn clip_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let vice = p.flag("vice");
    let result = p.choice("result");
    if vice && result == 1 {
        return Err(TransformError::Invalid(
            "In Vice Versa mode the clipped base notes can only be added (choose an “Add …” result).".into(),
        ));
    }
    // V09-0143: with Control Sweeps the v0.9 kernel evaluates them per note.
    if CLIP_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return clip9_build(p, b);
    }
    let seed = p.seed("seed");
    let invert = p.adv(1);
    let soft = p.adv(2).then(|| p.ticks("a2.width"));
    let expand = p
        .adv(3)
        .then(|| (p.ticks("a3.ticks"), p.int("a3.rows").clamp(0, 24) as u8));
    let contract = p
        .adv(4)
        .then(|| (p.ticks("a4.ticks"), p.int("a4.rows").clamp(0, 24) as u8));
    let combine = if p.adv(6) {
        2
    } else if p.adv(7) {
        3
    } else if p.adv(5) {
        1
    } else {
        0
    };
    let by_sub = p.adv(8);
    let animate = p.adv(9).then(|| p.int("a9.ticks"));
    let new_sub = p.adv(10);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let Some(base_slot) = src.aux_slot else {
                return Err(TransformError::Invalid(
                    "Choose a base (mask) track first.".into(),
                ));
            };
            let empty = Vec::new();
            let base_notes = src.aux.first().unwrap_or(&empty);
            let extra = src.aux.get(1).unwrap_or(&empty);
            let (art, mask_notes): (&[TNote], &[TNote]) = if vice {
                (base_notes, &src.notes)
            } else {
                (&src.notes, base_notes)
            };
            if art.is_empty() {
                return Ok(());
            }
            let t0 = art.iter().map(|n| n.start).min().unwrap_or(0);
            let t1 = art.iter().map(TNote::end).max().unwrap_or(1).max(t0 + 1);
            let sub_key = |n: &TNote| -> usize {
                if !by_sub {
                    return 0;
                }
                src.track(n.track)
                    .and_then(|t| t.sub_index(n.sub))
                    .unwrap_or(0)
                    + 1
            };
            // One mask per subchannel position (or a single one).
            let keys: Vec<usize> = if by_sub {
                let mut k: Vec<usize> = mask_notes.iter().map(sub_key).collect();
                k.sort_unstable();
                k.dedup();
                k
            } else {
                vec![0]
            };
            let extra_mask = RowMask::from_notes(src.keys, extra);
            let build = |key: usize| -> RowMask {
                let mut m =
                    RowMask::from_notes(src.keys, mask_notes.iter().filter(|n| sub_key(n) == key));
                m = match combine {
                    1 => m.union(&extra_mask),
                    2 => m.intersect(&extra_mask),
                    3 => m.subtract(&extra_mask, 0, u64::MAX),
                    _ => m,
                };
                if let Some((t, r)) = expand {
                    m = m.expand(t, r);
                }
                if let Some((t, r)) = contract {
                    m = m.contract(t, r, 0, t1.saturating_add(t));
                }
                if invert {
                    m = m.invert(t0, t1);
                }
                m
            };
            let masks: Vec<(usize, RowMask)> = keys.iter().map(|&k| (k, build(k))).collect();
            let empty_mask = RowMask::empty(src.keys);
            let mask_for = |n: &TNote| -> &RowMask {
                let k = sub_key(n);
                masks
                    .iter()
                    .find(|m| m.0 == k)
                    .map_or(&empty_mask, |m| &m.1)
            };
            // Keep the selection (except when it is the art being replaced).
            let replace = !vice && result == 1 && !new_sub;
            if !replace {
                for n in &src.notes {
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        i64::from(n.pitch),
                        i64::from(n.vel),
                        n,
                    );
                }
            }
            // Where the clipped notes go.
            let first_slot = src.notes.first().map_or(0, |n| n.track);
            let dest = |n: &TNote| -> u16 {
                match (vice, result) {
                    (false, 0) => base_slot,
                    (true, 0) => first_slot,
                    (false, _) => n.track,
                    (true, _) => base_slot,
                }
            };
            if new_sub {
                out.new_subtrack_parent = Some(match (vice, result) {
                    (false, 0) | (true, 2) => base_slot,
                    _ => first_slot,
                });
            }
            let mut kept = 0u64;
            for n in art {
                let m = mask_for(n);
                let off = animate.map_or(0i64, |a| {
                    let span = (t1 - t0).max(1) as f64;
                    (a as f64 * (n.start.saturating_sub(t0)) as f64 / span).round() as i64
                });
                let (qs, qe) = (
                    (i128::from(n.start) - i128::from(off)).max(0) as u64,
                    (i128::from(n.end()) - i128::from(off)).max(0) as u64,
                );
                let d = dest(n);
                let mut t = *n;
                t.track = if new_sub { NEW_SUBTRACK } else { d };
                if let Some(st) = src.track(d) {
                    t.sub = sub_in(st, n.sub);
                }
                let mut pieces: Vec<(u64, u64)> = Vec::new();
                m.clip(n.pitch, qs, qe, |a, b| pieces.push((a, b)));
                for (a, b) in pieces {
                    let (a, b) = (
                        (i128::from(a) + i128::from(off)).max(0),
                        (i128::from(b) + i128::from(off)).max(0),
                    );
                    if let Some(w) = soft {
                        let mid = ((a + b) / 2) as u64;
                        let dist = m.edge_distance(n.pitch, mid.saturating_sub(off.max(0) as u64));
                        if dist < w {
                            let keep = dist as f64 / w as f64;
                            if !Rng::for_index(
                                seed ^ super::super::rng::note_key(n),
                                salt::KEEP,
                                a as u64,
                            )
                            .chance(keep)
                            {
                                continue;
                            }
                        }
                    }
                    kept += 1;
                    if !out.emit(a, b - a, i64::from(n.pitch), i64::from(n.vel), &t) {
                        return out.check();
                    }
                }
                out.check()?;
            }
            out.message = Some(format!(
                "{} clipped note piece(s).",
                crate::midi::group_digits(kept)
            ));
            Ok(())
        },
        // The selection plus the clipped pieces (a piece per mask gap; the
        // output cap bounds the rest).
        growth: Growth::Expand(4),
        estimate: None,
    })
}

// ------------------------------------------------- v0.9 (V09-S38 step 3)

/// The functions declared by id (TOOL_CATALOG n01 … n11).
const CLIP_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const SHAPES: &[&str] = &["Ellipse", "Rectangle", "Triangle", "Star", "Polygon"];
const SIDES: &[&str] = &["Inside", "Outside", "Centered"];
const NO_YES: &[&str] = &["No", "Yes"];
const OUTSIDE: &[&str] = &["Subchannel offset", "Velocity scaled", "Removed"];
const GATE_ROWS: &[&str] = &["Any key", "Within the rows"];
const RANGES: &[&str] = &["Marker sections", "Chapters", "Time selection"];
const KEEP: &[&str] = &["Inside", "Outside"];
const TRANSFER: &[&str] = &["Velocity", "Subchannel", "Channel"];
const TRANSFER_MODES: &[&str] = &["Replace", "Multiply", "Average"];

/// Mask parameters a Control Sweep drives by levels: each value builds its
/// own mask (the platform's quantization, [`UnitSweeps::levels`]); the
/// other swept parameters are read at each note.
const MASK_SWEPT: [&str; 7] = [
    "n01.size",
    "n02.lo",
    "n04.dx",
    "n04.dy",
    "n05.width",
    "n06.period",
    "n07.threshold",
];

/// The inner radius of n01's star relative to the outer one (a regular
/// pentagram: sin 18° / sin 126°).
const STAR_INNER: f64 = 0.381_966_011_250_105_1;

/// n01's shape, in units of the art's box (time × rows, y up).
#[derive(Clone, Debug)]
struct Shape9 {
    kind: usize,
    cx: f64,
    cy: f64,
    /// The side of the shape's square (1 = the box).
    size: f64,
    /// n01.polygon in ‰ of the square (x, y; y up).
    poly: Vec<(f64, f64)>,
}

/// Everything that shapes one mask (one level's values when swept).
#[derive(Clone, Debug)]
struct MaskSet {
    shape: Option<Shape9>,
    /// n02: the mask notes' velocity range (either order).
    vel: Option<(u8, u8)>,
    /// n07: window, threshold, rows.
    density: Option<(u64, usize, u8)>,
    /// n09: `None` = any key, `Some(r)` = within ± r rows.
    gate: Option<Option<u8>>,
    /// n04: offset X, offset Y, scale X, scale Y (ratios).
    xform: Option<(i64, i64, f64, f64)>,
    /// n05: width, rows, side.
    outline: Option<(u64, u8, usize)>,
    /// n06: period, count, mirror.
    tile: Option<(u64, u64, bool)>,
    /// B2-F01: n05's outline before n04's transform (`order::CLIP_MASK_EDITS`;
    /// declared: the transform first).
    outline_first: bool,
}

/// The art's box: its span `[t0, t1)` and pitch range.
#[derive(Clone, Copy, Debug)]
struct ArtBox {
    t0: u64,
    t1: u64,
    plo: u8,
    phi: u8,
}

/// A mask's region: rows of time intervals, or one interval list for every
/// pitch (`uniform`: n09's any-key gate, n10's ranges — unbounded in pitch,
/// so row operations leave it alone).
#[derive(Clone, Debug)]
struct Region {
    m: RowMask,
    uniform: bool,
}

impl Region {
    fn rows(m: RowMask) -> Region {
        Region { m, uniform: false }
    }

    fn new(m: RowMask, uniform: bool) -> Region {
        Region { m, uniform }
    }

    /// The region on `rows` rows (a uniform one copied to each).
    fn materialized(self, rows: usize) -> RowMask {
        if self.uniform {
            RowMask::from_rows(vec![self.m.row(0).to_vec(); rows])
        } else {
            self.m
        }
    }

    /// a5 / a6 / a7 with the extra layer's mask.
    fn combine(self, how: u8, o: &RowMask) -> Region {
        let m = self.materialized(o.row_count());
        Region::rows(match how {
            1 => m.union(o),
            2 => m.intersect(o),
            3 => m.subtract(o, 0, u64::MAX),
            _ => m,
        })
    }

    fn expand(self, t: u64, r: u8) -> Region {
        let r = if self.uniform { 0 } else { r };
        Region::new(self.m.expand(t, r), self.uniform)
    }

    fn contract(self, t: u64, r: u8, t0: u64, t1: u64) -> Region {
        let r = if self.uniform { 0 } else { r };
        Region::new(self.m.contract(t, r, t0, t1), self.uniform)
    }

    fn invert(self, t0: u64, t1: u64) -> Region {
        Region::new(self.m.invert(t0, t1), self.uniform)
    }

    /// n09 Any key: the union of every row, for every pitch.
    fn project(self) -> Region {
        if self.uniform {
            return self;
        }
        let all: Vec<(u64, u64)> = (0..self.m.row_count())
            .flat_map(|p| self.m.row(p as u8).iter().copied())
            .collect();
        Region::new(RowMask::from_rows(vec![all]), true)
    }

    /// n04: scaled around the region's box centre (rows by nearest
    /// neighbour), then moved; times rounded, clipped at tick 0.
    fn transform(self, dx: i64, dy: i64, sx: f64, sy: f64) -> Region {
        let Some((e0, e1)) = self.m.extent() else {
            return self;
        };
        let tc = (e0 as f64 + e1 as f64) / 2.0;
        let map =
            |x: u64| -> u64 { ((tc + (x as f64 - tc) * sx).round() + dx as f64).max(0.0) as u64 };
        let moved = |r: &[(u64, u64)]| -> Vec<(u64, u64)> {
            r.iter().map(|&(a, b)| (map(a), map(b))).collect()
        };
        if self.uniform {
            return Region::new(RowMask::from_rows(vec![moved(self.m.row(0))]), true);
        }
        let rows = self.m.row_count();
        let filled = |p: &usize| !self.m.row(*p as u8).is_empty();
        let lo = (0..rows).find(filled).unwrap_or(0);
        let hi = (0..rows).rev().find(filled).unwrap_or(0);
        let pc = (lo as f64 + hi as f64 + 1.0) / 2.0;
        let out = (0..rows)
            .map(|r2| {
                let r1 = r2 as f64 - dy as f64;
                let src = (pc + (r1 + 0.5 - pc) / sy).floor();
                if src < 0.0 || src >= rows as f64 {
                    Vec::new()
                } else {
                    moved(self.m.row(src as u8))
                }
            })
            .collect();
        Region::rows(RowMask::from_rows(out))
    }

    /// n05: the band of `w` ticks and `r` rows along the region's outline
    /// (inside: the region minus its erosion; outside: the dilation minus
    /// the region; centred: half of each).
    fn outline(self, w: u64, r: u8, side: usize) -> Region {
        let Some((_, end)) = self.m.extent() else {
            return self;
        };
        let r = if self.uniform { 0 } else { r };
        let m = &self.m;
        let erode = |t: u64, rr: u8| m.contract(t, rr, 0, end.saturating_add(t));
        let band = match side {
            0 => m.subtract(&erode(w, r), 0, u64::MAX),
            1 => m.expand(w, r).subtract(m, 0, u64::MAX),
            _ => {
                let (w1, r1) = (w / 2, r / 2);
                m.expand(w1, r1)
                    .subtract(&erode(w - w1, r - r1), 0, u64::MAX)
            }
        };
        Region::new(band, self.uniform)
    }
}

/// n06: copies of the mask every `period` ticks from its extent `[e0, e1)`,
/// odd copies mirrored in time when asked — evaluated per query, never
/// stored.
#[derive(Clone, Copy, Debug)]
struct Tile {
    period: u64,
    count: u64,
    mirror: bool,
    e0: u64,
    e1: u64,
}

/// A finished mask: the region and its tiling.
#[derive(Clone, Debug)]
struct View {
    r: Region,
    tile: Option<Tile>,
}

/// The first index of `r` (partitioned by `pred`: true … true, false …
/// false) where `pred` is false, searched outward from `at` (exponential
/// steps, then a binary search inside the last step) — O(log distance).
fn gallop(r: &[(u64, u64)], at: usize, pred: impl Fn(&(u64, u64)) -> bool) -> usize {
    let n = r.len();
    let at = at.min(n);
    let mut step = 1;
    if at < n && pred(&r[at]) {
        // In (at, n].
        let mut lo = at + 1;
        loop {
            let probe = at + step;
            if probe >= n || !pred(&r[probe]) {
                let hi = probe.min(n);
                return lo + r[lo..hi].partition_point(&pred);
            }
            lo = probe + 1;
            step *= 2;
        }
    }
    // In [0, at]; `r[hi]` is false (or past the end).
    let mut hi = at;
    loop {
        if step > hi {
            return r[..hi].partition_point(&pred);
        }
        let probe = hi - step;
        if pred(&r[probe]) {
            return probe + 1 + r[probe + 1..hi].partition_point(&pred);
        }
        hi = probe;
        step *= 2;
    }
}

/// Merges the sorted, possibly overlapping intervals of `v[from..]`
/// (touching ones too).
fn merge_from(v: &mut Vec<(u64, u64)>, from: usize) {
    v[from..].sort_unstable();
    let mut w = from;
    for i in from..v.len() {
        let x = v[i];
        if w > from && x.0 <= v[w - 1].1 {
            v[w - 1].1 = v[w - 1].1.max(x.1);
        } else {
            v[w] = x;
            w += 1;
        }
    }
    v.truncate(w);
}

impl View {
    fn rp(&self, p: u8) -> u8 {
        if self.r.uniform { 0 } else { p }
    }

    /// For each copy of `t` that meets `[s, e)` on row `rp`: the base
    /// intervals overlapping its query `[qs, qe)` (a slice of the row, found
    /// by galloping from the previous copy's — consecutive copies' queries
    /// lie a period apart), the query, and the copy's placement: `(shift,
    /// false)` maps base `[a, b)` to `[a + shift, b + shift)`, `(m, true)`
    /// (a mirrored copy) to `[m − b, m − a)`.
    fn copies(
        &self,
        t: &Tile,
        rp: u8,
        s: u64,
        e: u64,
        mut f: impl FnMut(&[(u64, u64)], u64, u64, i128, bool),
    ) {
        let (s, e) = (i128::from(s), i128::from(e));
        let (p, e0, e1) = (
            i128::from(t.period.max(1)),
            i128::from(t.e0),
            i128::from(t.e1),
        );
        if s >= e || e <= e0 {
            return;
        }
        let row = self.r.m.row(rp);
        let k_lo = if s < e1 { 0 } else { (s - e1) / p + 1 };
        let k_hi = ((e - e0 - 1) / p).min(i128::from(t.count) - 1);
        // Cursors (first overlapping, first past) of the plain and the
        // mirrored copies.
        let mut cur = [(0usize, 0usize); 2];
        for k in k_lo..=k_hi {
            let sh = k * p;
            let mirrored = t.mirror && k % 2 == 1;
            // Base tick y of a mirrored copy lands on e0 + e1 + sh − 1 − y.
            let (qs, qe, place) = if mirrored {
                let m = e0 + e1 + sh;
                ((m - e).max(0), (m - s).max(0), m)
            } else {
                ((s - sh).max(0), e - sh, sh)
            };
            if qs >= qe {
                continue;
            }
            let (qs, qe) = (qs as u64, qe as u64);
            let c = &mut cur[usize::from(mirrored)];
            c.0 = gallop(row, c.0, |x| x.1 <= qs);
            c.1 = gallop(row, c.1.max(c.0), |x| x.0 < qe);
            f(&row[c.0..c.1], qs, qe, place, mirrored);
        }
    }

    /// Appends the pieces of `[s, e)` on row `p` inside the mask to `out`
    /// (sorted, disjoint, never touching).
    fn clip(&self, p: u8, s: u64, e: u64, out: &mut Vec<(u64, u64)>) {
        let rp = self.rp(p);
        match &self.tile {
            None => self.r.m.clip(rp, s, e, |a, b| out.push((a, b))),
            Some(t) => {
                let from = out.len();
                self.copies(t, rp, s, e, |over, qs, qe, place, mirrored| {
                    for &(a, b) in over {
                        let (a, b) = (i128::from(a.max(qs)), i128::from(b.min(qe)));
                        out.push(if mirrored {
                            ((place - b) as u64, (place - a) as u64)
                        } else {
                            ((a + place) as u64, (b + place) as u64)
                        });
                    }
                });
                merge_from(out, from);
            }
        }
    }

    /// An upper bound of [`View::clip`]'s pieces (exact without tiling).
    fn count(&self, p: u8, s: u64, e: u64) -> u64 {
        let rp = self.rp(p);
        match &self.tile {
            None => self.r.m.overlapping(rp, s, e) as u64,
            Some(t) => {
                let mut c = 0u64;
                self.copies(t, rp, s, e, |over, _, _, _, _| c += over.len() as u64);
                c
            }
        }
    }

    /// Distance from tick `t` on row `p` to the nearest mask edge — the
    /// v0.8 rule without tiling; with it, at most `w + 1` (edges looked
    /// for within `w` ticks).
    fn edge(&self, p: u8, t: u64, w: u64) -> u64 {
        if self.tile.is_none() {
            return self.r.m.edge_distance(self.rp(p), t);
        }
        let (lo, hi) = (t.saturating_sub(w), t.saturating_add(w).saturating_add(1));
        let mut v = Vec::new();
        self.clip(p, lo, hi, &mut v);
        let mut d = w.saturating_add(1);
        for (a, b) in v {
            if a > lo || a == 0 {
                d = d.min(t.abs_diff(a));
            }
            if b < hi {
                d = d.min(t.abs_diff(b));
            }
        }
        d
    }

    /// Whether the mask covers tick `t` on row `p` (rows off the keyboard
    /// are outside; a uniform mask has every row).
    fn covers(&self, p: i64, t: u64) -> bool {
        let rp = if self.r.uniform {
            0
        } else if p < 0 || p >= self.r.m.row_count() as i64 {
            return false;
        } else {
            p as u8
        };
        match &self.tile {
            None => {
                let r = self.r.m.row(rp);
                let i = r.partition_point(|x| x.1 <= t);
                r.get(i).is_some_and(|x| x.0 <= t)
            }
            Some(_) => {
                let mut v = Vec::new();
                self.clip(rp, t, t.saturating_add(1), &mut v);
                !v.is_empty()
            }
        }
    }

    fn bytes(&self) -> u64 {
        self.r.m.intervals() as u64 * 16
    }
}

/// n01: the shape rasterized at each row's centre (ellipse and rectangle in
/// closed form, the others as polygons by the even-odd scanline rule of
/// Select's lasso).
fn shape_mask(sh: &Shape9, b: &ArtBox, rows: usize) -> RowMask {
    let (t0, w) = (b.t0 as f64, b.t1.saturating_sub(b.t0).max(1) as f64);
    let (p0, h) = (
        f64::from(b.plo),
        (f64::from(b.phi) + 1.0 - f64::from(b.plo)).max(1.0),
    );
    let half = sh.size / 2.0;
    let (cx, cy) = (sh.cx, sh.cy);
    // ‰-square coordinates (0 … 1) → box units.
    let pt = |x: f64, y: f64| (cx + half * (2.0 * x - 1.0), cy + half * (2.0 * y - 1.0));
    let poly: Vec<(f64, f64)> = match sh.kind {
        2 => vec![pt(0.5, 1.0), pt(0.0, 0.0), pt(1.0, 0.0)],
        3 => (0..10)
            .map(|k| {
                let a = std::f64::consts::FRAC_PI_2 + f64::from(k) * std::f64::consts::PI / 5.0;
                let r = if k % 2 == 0 { 1.0 } else { STAR_INNER };
                pt(0.5 + 0.5 * r * a.cos(), 0.5 + 0.5 * r * a.sin())
            })
            .collect(),
        4 => sh.poly.iter().map(|&(x, y)| pt(x, y)).collect(),
        _ => Vec::new(),
    };
    let tick = |u: f64| (t0 + u * w).round().max(0.0) as u64;
    let mut xs: Vec<f64> = Vec::new();
    let out = (0..rows)
        .map(|r| {
            let v = (r as f64 + 0.5 - p0) / h;
            xs.clear();
            match sh.kind {
                0 => {
                    let dv = (v - cy) / half;
                    if half > 0.0 && dv.abs() <= 1.0 {
                        let k = half * (1.0 - dv * dv).sqrt();
                        xs.extend([cx - k, cx + k]);
                    }
                }
                1 => {
                    if (v - cy).abs() <= half {
                        xs.extend([cx - half, cx + half]);
                    }
                }
                _ => {
                    let n = poly.len();
                    if n >= 3 {
                        let mut j = n - 1;
                        for i in 0..n {
                            let ((xi, yi), (xj, yj)) = (poly[i], poly[j]);
                            if (yi > v) != (yj > v) {
                                xs.push(xi + (v - yi) * (xj - xi) / (yj - yi));
                            }
                            j = i;
                        }
                        xs.sort_by(f64::total_cmp);
                    }
                }
            }
            xs.chunks_exact(2)
                .map(|c| (tick(c[0]), tick(c[1])))
                .filter(|(a, b)| a < b)
                .collect()
        })
        .collect();
    RowMask::from_rows(out)
}

/// n07: where at least `n` starts of the notes on rows within ± `rr` lie in
/// the window `(t − ⌊W/2⌋, t + ⌈W/2⌉]` (`density_window`'s trailing NPS
/// window, centred on the point).
fn density_mask<'a>(
    notes: impl Iterator<Item = &'a TNote>,
    w: u64,
    n: usize,
    rr: u8,
    rows: usize,
) -> RowMask {
    let mut by: Vec<Vec<u64>> = vec![Vec::new(); rows];
    for x in notes {
        if let Some(v) = by.get_mut(usize::from(x.pitch)) {
            v.push(x.start);
        }
    }
    for v in &mut by {
        v.sort_unstable();
    }
    let back = w.div_ceil(2);
    let rr = usize::from(rr);
    let mut buf: Vec<u64> = Vec::new();
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let times: &[u64] = if rr == 0 {
            &by[r]
        } else {
            buf.clear();
            for v in &by[r.saturating_sub(rr)..=(r + rr).min(rows - 1)] {
                buf.extend_from_slice(v);
            }
            buf.sort_unstable();
            &buf
        };
        out.push(
            super::super::shared::density_window::dense_ranges(times, w, n)
                .into_iter()
                .map(|(a, b)| (a.saturating_sub(back), b.saturating_sub(back)))
                .filter(|(a, b)| a < b)
                .collect(),
        );
    }
    RowMask::from_rows(out)
}

/// n11: the mask notes by row, start-ordered, with each prefix's note that
/// ends last — the note sounding at a tick that lasts longest (ties: the
/// earliest in time order).
struct Lookup<'a> {
    rows: Vec<Vec<&'a TNote>>,
    best: Vec<Vec<u32>>,
}

impl<'a> Lookup<'a> {
    fn new(notes: &'a [TNote], rows: usize) -> Lookup<'a> {
        let mut by: Vec<Vec<&TNote>> = vec![Vec::new(); rows];
        for n in notes {
            if let Some(v) = by.get_mut(usize::from(n.pitch)) {
                v.push(n);
            }
        }
        let best = by
            .iter_mut()
            .map(|v| {
                v.sort_by_key(|n| n.start);
                let mut cur = 0usize;
                (0..v.len())
                    .map(|i| {
                        if v[i].end() > v[cur].end() {
                            cur = i;
                        }
                        cur as u32
                    })
                    .collect()
            })
            .collect();
        Lookup { rows: by, best }
    }

    fn at(&self, pitch: u8, t: u64) -> Option<&'a TNote> {
        let v = self.rows.get(usize::from(pitch))?;
        let k = v.partition_point(|n| n.start <= t);
        let j = *self.best[usize::from(pitch)].get(k.checked_sub(1)?)? as usize;
        (v[j].end() > t).then_some(v[j])
    }
}

/// The v0.9 Clipping Mask: the v0.8 rule (`clip_build`'s closure, the same
/// arithmetic) with the functions declared by id; see
/// `docs/TRANSFORM_ENGINE.md` "Clipping Mask".
struct Clip9 {
    // ----- v0.8, as `clip_build` reads them
    vice: bool,
    result: usize,
    seed: u64,
    invert: bool,
    soft: Option<u64>,
    expand: Option<(u64, u8)>,
    contract: Option<(u64, u8)>,
    combine: u8,
    by_sub: bool,
    animate: Option<i64>,
    new_sub: bool,
    // ----- v0.9
    mask: MaskSet,
    /// n10: ranges, keep inside.
    ranges: Option<(usize, bool)>,
    /// n08: mode (0 subchannel offset, 1 velocity scaled), offset, ratio.
    split: Option<(usize, i64, f64)>,
    /// n03: width, rows, floor (ratio).
    fade: Option<(u64, u8, f64)>,
    /// n11: property, mode, amount (ratio).
    transfer: Option<(usize, usize, f64)>,
    ctx: super::super::Ctx,
    /// The preflight's plan, taken by the run on the same source.
    cache: std::sync::Mutex<Option<(PlanKey, Plan)>>,
}

/// What a run of [`Clip9`] works with: the art's masks and per-note values
/// (the art itself is [`Clip9::art`]).
struct Plan {
    views: Vec<View>,
    /// Each art note's view (`u32::MAX`: an empty mask — a8 with no mask
    /// notes in its subchannel); `None` = every note uses `views[0]`.
    which: Option<Vec<u32>>,
    t0: u64,
    t1: u64,
    base_slot: Option<u16>,
    /// n10 found no ranges.
    no_ranges: bool,
    /// The art's places and the sweeps measured in its frame.
    places: Vec<super::super::drivers::Place>,
    sweeps: Option<super::super::sweep::UnitSweeps>,
}

impl Plan {
    fn view(&self, i: usize) -> Option<&View> {
        match &self.which {
            None => self.views.first(),
            Some(w) => self.views.get(w[i] as usize),
        }
    }

    fn bytes(&self) -> u64 {
        self.views.iter().map(View::bytes).sum::<u64>()
            + self.which.as_ref().map_or(0, |w| w.len() as u64 * 4)
    }
}

/// The source a [`Plan`] was built for: its note and layer storage and its
/// statistics (the preflight's plan is handed to the run that follows on
/// the same source; any other source builds its own).
#[derive(Debug, PartialEq)]
struct PlanKey {
    notes: (usize, usize),
    aux: usize,
    aux_lens: Vec<usize>,
    aux_slot: Option<u16>,
    stats: SourceStats,
}

impl PlanKey {
    fn of(src: &Source) -> PlanKey {
        PlanKey {
            notes: (src.notes.as_ptr() as usize, src.notes.len()),
            aux: std::sync::Arc::as_ptr(&src.aux) as usize,
            aux_lens: src.aux.iter().map(Vec::len).collect(),
            aux_slot: src.aux_slot,
            stats: src.stats.clone(),
        }
    }
}

fn clip9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let combine = if p.adv(6) {
        2
    } else if p.adv(7) {
        3
    } else if p.adv(5) {
        1
    } else {
        0
    };
    let mask = MaskSet {
        shape: p.flag("n01").then(|| Shape9 {
            kind: p.choice("n01.shape"),
            cx: p.float("n01.cx") / 100.0,
            cy: p.float("n01.cy") / 100.0,
            size: p.float("n01.size") / 100.0,
            poly: p
                .list("n01.polygon")
                .chunks_exact(2)
                .map(|c| (c[0] as f64 / 1000.0, c[1] as f64 / 1000.0))
                .collect(),
        }),
        vel: p.flag("n02").then(|| {
            (
                p.int("n02.lo").clamp(1, 127) as u8,
                p.int("n02.hi").clamp(1, 127) as u8,
            )
        }),
        density: p.flag("n07").then(|| {
            (
                p.ticks("n07.window").max(1),
                p.int("n07.threshold").max(1) as usize,
                p.int("n07.rows").clamp(0, 24) as u8,
            )
        }),
        gate: p
            .flag("n09")
            .then(|| (p.choice("n09.rows") == 1).then(|| p.int("n09.within").clamp(0, 48) as u8)),
        xform: p.flag("n04").then(|| {
            (
                p.int("n04.dx"),
                p.int("n04.dy"),
                p.float("n04.sx").max(1.0) / 100.0,
                p.float("n04.sy").max(1.0) / 100.0,
            )
        }),
        outline: p.flag("n05").then(|| {
            (
                p.ticks("n05.width").max(1),
                p.int("n05.rows").clamp(1, 24) as u8,
                p.choice("n05.side"),
            )
        }),
        tile: p.flag("n06").then(|| {
            (
                p.ticks("n06.period").max(1),
                p.int("n06.count").clamp(1, 1024) as u64,
                p.choice("n06.mirror") == 1,
            )
        }),
        outline_first: p.sequence(crate::transform::order::CLIP_MASK_EDITS.members)[0] == "n05",
    };
    let k = Clip9 {
        vice: p.flag("vice"),
        result: p.choice("result"),
        seed: p.seed("seed"),
        invert: p.adv(1),
        soft: p.adv(2).then(|| p.ticks("a2.width")),
        expand: p
            .adv(3)
            .then(|| (p.ticks("a3.ticks"), p.int("a3.rows").clamp(0, 24) as u8)),
        contract: p
            .adv(4)
            .then(|| (p.ticks("a4.ticks"), p.int("a4.rows").clamp(0, 24) as u8)),
        combine,
        by_sub: p.adv(8),
        animate: p.adv(9).then(|| p.int("a9.ticks")),
        new_sub: p.adv(10),
        mask,
        ranges: p
            .flag("n10")
            .then(|| (p.choice("n10.ranges"), p.choice("n10.keep") == 0)),
        split: p
            .flag("n08")
            .then(|| {
                (
                    p.choice("n08.mode"),
                    p.int("n08.offset"),
                    p.float("n08.vel") / 100.0,
                )
            })
            .filter(|s| s.0 < 2),
        fade: p.flag("n03").then(|| {
            (
                p.ticks("n03.width"),
                p.int("n03.rows").clamp(0, 24) as u8,
                p.float("n03.floor").clamp(0.0, 100.0) / 100.0,
            )
        }),
        transfer: p.flag("n11").then(|| {
            (
                p.choice("n11.prop"),
                p.choice("n11.mode"),
                p.float("n11.amount").clamp(0.0, 100.0) / 100.0,
            )
        }),
        ctx: b.ctx.clone(),
        cache: Default::default(),
    };
    Ok(Box::new(k))
}

impl Clip9 {
    fn generated(&self) -> bool {
        self.mask.shape.is_some() || self.ranges.is_some()
    }

    fn replace(&self) -> bool {
        !self.vice && self.result == 1 && !self.new_sub
    }

    /// Whether a mask parameter's function is on (its sweep then builds a
    /// mask per level).
    fn mask_drives(&self, key: &str) -> bool {
        let m = &self.mask;
        match key {
            "n01.size" => m.shape.is_some(),
            "n02.lo" => m.vel.is_some(),
            "n04.dx" | "n04.dy" => m.xform.is_some(),
            "n05.width" => m.outline.is_some(),
            "n06.period" => m.tile.is_some(),
            "n07.threshold" => m.density.is_some(),
            _ => false,
        }
    }

    /// The mask settings at level key `g` of the swept mask parameters.
    fn mask_at(
        &self,
        sw: &super::super::sweep::UnitSweeps,
        lv: &super::super::sweep::SweepLevels,
        g: u64,
    ) -> MaskSet {
        let mut m = self.mask.clone();
        if let Some(s) = &mut m.shape {
            s.size = sw.level_float(lv, g, "n01.size", s.size * 100.0).max(1.0) / 100.0;
        }
        if let Some(v) = &mut m.vel {
            v.0 = sw.level_int(lv, g, "n02.lo", i64::from(v.0)).clamp(1, 127) as u8;
        }
        if let Some(x) = &mut m.xform {
            x.0 = sw.level_int(lv, g, "n04.dx", x.0);
            x.1 = sw.level_int(lv, g, "n04.dy", x.1);
        }
        if let Some(o) = &mut m.outline {
            o.0 = sw.level_int(lv, g, "n05.width", o.0 as i64).max(1) as u64;
        }
        if let Some(t) = &mut m.tile {
            t.0 = sw.level_int(lv, g, "n06.period", t.0 as i64).max(1) as u64;
        }
        if let Some(d) = &mut m.density {
            d.1 = sw.level_int(lv, g, "n07.threshold", d.1 as i64).max(1) as usize;
        }
        m
    }

    /// n10's ranges in the tools' ticks (kept inside, or their complement),
    /// and whether there were none.
    fn ranges(&self) -> Option<(Vec<(u64, u64)>, bool)> {
        let (kind, inside) = self.ranges?;
        let shift = self.ctx.shift;
        let tool =
            |t: u64| -> u64 { (i128::from(t) - shift).clamp(0, i128::from(u64::MAX)) as u64 };
        let v: Vec<(u64, u64)> = match kind {
            // Point markers (every marker start but the chapters'), paired:
            // [m0, m1), [m2, m3), …; an odd last one opens a range to the end.
            0 => {
                let mut all: Vec<u64> = self.ctx.markers.to_vec();
                all.sort_unstable();
                let mut ch: Vec<u64> = self.ctx.chapters.iter().map(|c| c.0).collect();
                ch.sort_unstable();
                let mut j = 0;
                let mut pts: Vec<u64> = Vec::new();
                for m in all {
                    while j < ch.len() && ch[j] < m {
                        j += 1;
                    }
                    if j < ch.len() && ch[j] == m {
                        j += 1;
                        continue;
                    }
                    pts.push(tool(m));
                }
                pts.chunks(2)
                    .map(|c| (c[0], c.get(1).copied().unwrap_or(u64::MAX)))
                    .collect()
            }
            1 => self
                .ctx
                .chapters
                .iter()
                .map(|&(a, b)| (tool(a), tool(b)))
                .collect(),
            _ => self.ctx.time_range.into_iter().collect(),
        };
        let m = RowMask::from_rows(vec![v]);
        let none = m.is_empty();
        let m = if inside { m } else { m.invert(0, u64::MAX) };
        Some((m.row(0).to_vec(), none))
    }

    /// A mask's region before its level-swept geometry: the source (n01
    /// shape, n10 ranges, or the mask notes — n02's velocities, n07's
    /// density), the frozen a5 – a7, a3, a4, a1, then n09.
    fn source<'a>(
        &self,
        ms: &MaskSet,
        notes: impl Iterator<Item = &'a TNote>,
        extra: Option<&RowMask>,
        ranges: Option<&Vec<(u64, u64)>>,
        b: &ArtBox,
        keys: crate::model::project::KeyRange,
    ) -> Region {
        let rows = keys.keys();
        let mut region = if let Some(sh) = &ms.shape {
            let mut m = shape_mask(sh, b, rows);
            if let Some(rg) = ranges {
                m = m.intersect(&RowMask::from_rows(vec![rg.clone(); rows]));
            }
            Region::rows(m)
        } else if let Some(rg) = ranges {
            Region::new(RowMask::from_rows(vec![rg.clone()]), true)
        } else {
            let vel = ms.vel.map(|(a, c)| (a.min(c), a.max(c)));
            let it = notes.filter(move |n| vel.is_none_or(|(lo, hi)| (lo..=hi).contains(&n.vel)));
            Region::rows(match ms.density {
                Some((w, n, rr)) => density_mask(it, w, n, rr, rows),
                None => RowMask::from_notes(keys, it),
            })
        };
        if let Some(x) = extra {
            region = region.combine(self.combine, x);
        }
        if let Some((t, r)) = self.expand {
            region = region.expand(t, r);
        }
        if let Some((t, r)) = self.contract {
            region = region.contract(t, r, 0, b.t1.saturating_add(t));
        }
        if self.invert {
            region = region.invert(b.t0, b.t1);
        }
        if let Some(g) = ms.gate {
            region = match g {
                None => region.project(),
                Some(r) => region.expand(0, r),
            };
        }
        region
    }

    /// The finished mask: [`Clip9::source`]'s region through n04, n05 and
    /// n06.
    fn finish(ms: &MaskSet, mut region: Region) -> View {
        // B2-F01: n04 and n05 in the user's order (declared: n04 first).
        let transform = |region: Region| match ms.xform {
            Some((dx, dy, sx, sy)) if !(dx == 0 && dy == 0 && sx == 1.0 && sy == 1.0) => {
                region.transform(dx, dy, sx, sy)
            }
            _ => region,
        };
        let outline = |region: Region| match ms.outline {
            Some((w, r, side)) => region.outline(w, r, side),
            None => region,
        };
        region = if ms.outline_first {
            transform(outline(region))
        } else {
            outline(transform(region))
        };
        let tile = ms
            .tile
            .filter(|t| t.1 > 1)
            .and_then(|(period, count, mirror)| {
                region.m.extent().map(|(e0, e1)| Tile {
                    period,
                    count,
                    mirror,
                    e0,
                    e1,
                })
            });
        View { r: region, tile }
    }

    /// The notes clipped: the selection, or the base track's in Vice Versa.
    fn art<'a>(&self, src: &'a Source) -> &'a [TNote] {
        if self.vice {
            src.aux.first().map_or(&[], Vec::as_slice)
        } else {
            &src.notes
        }
    }

    fn plan(&self, src: &Source) -> Result<Plan, TransformError> {
        let base_slot = src.aux_slot;
        if base_slot.is_none() && (!self.generated() || self.vice || self.result == 0) {
            return Err(TransformError::Invalid(
                "Choose a base (mask) track first.".into(),
            ));
        }
        let base_notes: &[TNote] = src.aux.first().map_or(&[], Vec::as_slice);
        let extra_notes: &[TNote] = src.aux.get(1).map_or(&[], Vec::as_slice);
        let (art, mask_notes): (&[TNote], &[TNote]) = if self.vice {
            (base_notes, &src.notes)
        } else {
            (&src.notes, base_notes)
        };
        let mut plan = Plan {
            views: Vec::new(),
            which: None,
            t0: 0,
            t1: 1,
            base_slot,
            no_ranges: false,
            places: Vec::new(),
            sweeps: None,
        };
        if art.is_empty() {
            return Ok(plan);
        }
        let t0 = art.iter().map(|n| n.start).min().unwrap_or(0);
        let t1 = art.iter().map(TNote::end).max().unwrap_or(1).max(t0 + 1);
        let bx = ArtBox {
            t0,
            t1,
            plo: art.iter().map(|n| n.pitch).min().unwrap_or(0),
            phi: art.iter().map(|n| n.pitch).max().unwrap_or(0),
        };
        plan.t0 = t0;
        plan.t1 = t1;
        // Per-note sweeps in the art's frame (the base track's in Vice Versa).
        if let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) {
            let (sw, places) = if self.vice {
                (
                    sw.in_frame(super::super::drivers::Frame::of_notes(art)),
                    super::super::sweep::places_of(src, art),
                )
            } else {
                (sw.clone(), super::super::sweep::note_places(src))
            };
            plan.sweeps = Some(sw);
            plan.places = places;
        }
        let ranges = self.ranges();
        plan.no_ranges = ranges.as_ref().is_some_and(|r| r.1);
        let ranges = ranges.map(|r| r.0);
        let extra = (self.combine != 0).then(|| RowMask::from_notes(src.keys, extra_notes));
        // The level groups of the swept mask parameters (one group without).
        let swept: Vec<&str> = MASK_SWEPT
            .iter()
            .copied()
            .filter(|k| self.mask_drives(k))
            .collect();
        let levels = plan
            .sweeps
            .as_ref()
            .filter(|_| !swept.is_empty())
            .and_then(|sw| sw.levels(&swept, art, &plan.places).map(|lv| (sw, lv)));
        let groups: Vec<(MaskSet, Option<Vec<usize>>)> = match &levels {
            None => vec![(self.mask.clone(), None)],
            Some((sw, lv)) => lv
                .groups
                .iter()
                .map(|(g, idx)| (self.mask_at(sw, lv, *g), Some(idx.clone())))
                .collect(),
        };
        // a8: one mask per subchannel position of the mask notes (not for a
        // generated mask, which has no notes to split).
        let by_sub = self.by_sub && !self.generated();
        let sub_key = |n: &TNote| -> usize {
            if !by_sub {
                return 0;
            }
            src.track(n.track)
                .and_then(|t| t.sub_index(n.sub))
                .unwrap_or(0)
                + 1
        };
        if !by_sub && groups.len() == 1 {
            let (ms, _) = &groups[0];
            let region = self.source(
                ms,
                mask_notes.iter(),
                extra.as_ref(),
                ranges.as_ref(),
                &bx,
                src.keys,
            );
            plan.views.push(Self::finish(ms, region));
            return Ok(plan);
        }
        let mut which = vec![u32::MAX; art.len()];
        let mut mask_keys: Vec<usize> = mask_notes.iter().map(sub_key).collect();
        mask_keys.sort_unstable();
        mask_keys.dedup();
        // Source regions by what shapes them (the levels of n01's size,
        // n02's velocity and n07's threshold, and the subchannel), each
        // built once: levels of n04 / n05 / n06 alone share one.
        type SourceKey = (u64, Option<(u8, u8)>, Option<usize>, usize);
        let mut sources: Vec<(SourceKey, Region)> = Vec::new();
        for (ms, idx) in &groups {
            let idx: Vec<usize> = idx.clone().unwrap_or_else(|| (0..art.len()).collect());
            let mut by_key: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
            for i in idx {
                by_key.entry(sub_key(&art[i])).or_default().push(i);
            }
            for (key, members) in by_key {
                if by_sub && mask_keys.binary_search(&key).is_err() {
                    continue;
                }
                let sk: SourceKey = (
                    ms.shape.as_ref().map_or(0, |s| s.size.to_bits()),
                    ms.vel,
                    ms.density.map(|d| d.1),
                    key,
                );
                let region = match sources.iter().find(|x| x.0 == sk) {
                    Some(x) => x.1.clone(),
                    None => {
                        let r = self.source(
                            ms,
                            mask_notes.iter().filter(|n| sub_key(n) == key),
                            extra.as_ref(),
                            ranges.as_ref(),
                            &bx,
                            src.keys,
                        );
                        sources.push((sk, r.clone()));
                        r
                    }
                };
                let at = plan.views.len() as u32;
                plan.views.push(Self::finish(ms, region));
                for i in members {
                    which[i] = at;
                }
            }
        }
        plan.which = Some(which);
        Ok(plan)
    }

    /// v0.8's time offset of a9 for an art note.
    fn offset(&self, n: &TNote, t0: u64, t1: u64) -> i64 {
        self.animate.map_or(0i64, |a| {
            let span = (t1 - t0).max(1) as f64;
            (a as f64 * (n.start.saturating_sub(t0)) as f64 / span).round() as i64
        })
    }
}

impl Kernel for Clip9 {
    fn growth(&self) -> Growth {
        Growth::Expand(4)
    }

    /// From the statistics alone: the kept selection plus, per art note of
    /// length d, at most ⌊(d + 1) / 2⌋ pieces (a mask's intervals are
    /// separated by at least a tick) and, with n08, at most d outside
    /// parts. In Vice Versa the art is the base track, which the
    /// selection's statistics do not bound (the run's preflight,
    /// [`Kernel::estimate_source`], counts exactly).
    fn estimate(&self, stats: &SourceStats) -> Estimate {
        let n = stats.count as u64;
        if self.vice {
            return Estimate::for_notes(n, u64::MAX);
        }
        let d = u64::try_from(stats.sum_dur).unwrap_or(u64::MAX);
        let kept = if self.replace() { 0 } else { n };
        let pieces = d.saturating_add(n) / 2;
        let parts = if self.split.is_some() { d } else { 0 };
        Estimate::for_notes(n, kept.saturating_add(pieces).saturating_add(parts))
    }

    /// The masks built and every art note's pieces counted (two binary
    /// searches each), without writing a note.
    fn estimate_source(&self, src: &Source) -> Estimate {
        let Ok(plan) = self.plan(src) else {
            // The run reports the sentence.
            return Estimate::for_notes(src.notes.len() as u64, src.notes.len() as u64);
        };
        let n = src.notes.len() as u64;
        let art = self.art(src);
        if art.is_empty() {
            return Estimate::for_notes(n, 0);
        }
        let mut total = if self.replace() { 0 } else { n };
        for (i, a) in art.iter().enumerate() {
            let off = self.offset(a, plan.t0, plan.t1);
            let (qs, qe) = (
                (i128::from(a.start) - i128::from(off)).max(0) as u64,
                (i128::from(a.end()) - i128::from(off)).max(0) as u64,
            );
            let c = plan.view(i).map_or(0, |v| v.count(a.pitch, qs, qe));
            total = total.saturating_add(c.min(qe.saturating_sub(qs).saturating_add(1) / 2));
            if self.split.is_some() {
                total = total.saturating_add(c.saturating_add(1).min(a.dur));
            }
        }
        let mut e = Estimate::for_notes(n, total);
        e.temp_bytes = e.temp_bytes.saturating_add(plan.bytes());
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((PlanKey::of(src), plan));
        e
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let cached = self.cache.lock().unwrap_or_else(|e| e.into_inner()).take();
        let plan = match cached {
            Some((k, p)) if k == PlanKey::of(src) => p,
            _ => self.plan(src)?,
        };
        let art = self.art(src);
        if art.is_empty() {
            return Ok(());
        }
        let (t0, t1) = (plan.t0, plan.t1);
        // Keep the selection (except when it is the art being replaced).
        if !self.replace() {
            for n in &src.notes {
                out.emit(
                    i128::from(n.start),
                    i128::from(n.dur),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                );
            }
        }
        let first_slot = src.notes.first().map_or(0, |n| n.track);
        let base = plan.base_slot.unwrap_or(first_slot);
        let dest = |n: &TNote| -> u16 {
            match (self.vice, self.result) {
                (false, 0) => base,
                (true, 0) => first_slot,
                (false, _) => n.track,
                (true, _) => base,
            }
        };
        if self.new_sub {
            out.new_subtrack_parent = Some(match (self.vice, self.result) {
                (false, 0) | (true, 2) => base,
                _ => first_slot,
            });
        }
        let lookup = self.transfer.map(|_| {
            let mask_notes: &[TNote] = if self.vice {
                &src.notes
            } else {
                src.aux.first().map_or(&[], Vec::as_slice)
            };
            Lookup::new(mask_notes, src.keys.keys())
        });
        let transfer_salt = super::super::shared::seeded_stream::salt_of("n11");
        let sw = plan.sweeps.as_ref();
        let (mut kept, mut outside) = (0u64, 0u64);
        let mut pieces: Vec<(u64, u64)> = Vec::new();
        let mut inside: Vec<(i128, i128)> = Vec::new();
        for (i, n) in art.iter().enumerate() {
            let view = plan.view(i);
            let off = self.offset(n, t0, t1);
            let (qs, qe) = (
                (i128::from(n.start) - i128::from(off)).max(0) as u64,
                (i128::from(n.end()) - i128::from(off)).max(0) as u64,
            );
            let d = dest(n);
            let st = src.track(d);
            let mut t = *n;
            t.track = if self.new_sub { NEW_SUBTRACK } else { d };
            if let Some(st) = st {
                t.sub = sub_in(st, n.sub);
            }
            // The per-note swept values.
            let place = plan.places.get(i).copied().unwrap_or_default();
            let pct = |key: &str, v: f64| match sw {
                Some(s) if s.drives(key) => s.float(key, v * 100.0, n, place) / 100.0,
                _ => v,
            };
            let fade = self.fade.map(|(w, r, floor)| {
                let w = match sw {
                    Some(s) if s.drives("n03.width") => {
                        s.int("n03.width", w as i64, n, place).max(0) as u64
                    }
                    _ => w,
                };
                (w, r, floor)
            });
            let split = self
                .split
                .map(|(m, o, v)| (m, o, pct("n08.vel", v).max(0.0)));
            let transfer = self
                .transfer
                .map(|(p, m, a)| (p, m, pct("n11.amount", a).clamp(0.0, 1.0)));
            pieces.clear();
            inside.clear();
            if let Some(v) = view {
                v.clip(n.pitch, qs, qe, &mut pieces);
            }
            for &(a, b) in &pieces {
                let (a, b) = (
                    (i128::from(a) + i128::from(off)).max(0),
                    (i128::from(b) + i128::from(off)).max(0),
                );
                if split.is_some() {
                    inside.push((a, b));
                }
                let v = view.expect("pieces come from a mask");
                if let Some(w) = self.soft {
                    let mid = ((a + b) / 2) as u64;
                    let dist = v.edge(n.pitch, mid.saturating_sub(off.max(0) as u64), w);
                    if dist < w {
                        let keep = dist as f64 / w as f64;
                        if !Rng::for_index(
                            self.seed ^ super::super::rng::note_key(n),
                            salt::KEEP,
                            a as u64,
                        )
                        .chance(keep)
                        {
                            continue;
                        }
                    }
                }
                kept += 1;
                let mut vel = f64::from(n.vel);
                let mut tt = t;
                // n03: the velocity fades toward the mask's edge.
                if let Some((w, rows, floor)) = fade {
                    let tm = ((a + b) / 2 - i128::from(off)).max(0) as u64;
                    let ft = if w == 0 {
                        1.0
                    } else {
                        (v.edge(n.pitch, tm, w) as f64 / w as f64).min(1.0)
                    };
                    let mut fr = 1.0;
                    for k in 1..=i64::from(rows) {
                        let p = i64::from(n.pitch);
                        if !v.covers(p - k, tm) || !v.covers(p + k, tm) {
                            fr = (k - 1) as f64 / f64::from(rows);
                            break;
                        }
                    }
                    vel *= floor + (1.0 - floor) * ft.min(fr);
                }
                // n11: the base note sounding at the piece's middle.
                if let (Some((prop, mode, amount)), Some(lk)) = (transfer, &lookup)
                    && amount > 0.0
                    && let Some(bn) = lk.at(n.pitch, ((a + b) / 2) as u64)
                {
                    match prop {
                        0 => {
                            let vb = f64::from(bn.vel);
                            vel = match mode {
                                1 => vel * (1.0 + amount * (vb / 127.0 - 1.0)),
                                2 => vel + amount * ((vel + vb) / 2.0 - vel),
                                _ => vel + amount * (vb - vel),
                            };
                        }
                        _ => {
                            let take = amount >= 1.0
                                || super::super::shared::seeded_stream::stream(
                                    self.seed,
                                    transfer_salt,
                                    &[super::super::rng::note_key(n), a as u64],
                                )
                                .chance(amount);
                            if take {
                                if prop == 1 {
                                    if let Some(st) = st {
                                        tt.sub = sub_in(st, bn.sub);
                                    }
                                } else {
                                    tt.ch = bn.ch;
                                }
                            }
                        }
                    }
                }
                let vel = if fade.is_some() || transfer.is_some() {
                    vel.round().clamp(1.0, 127.0) as i64
                } else {
                    i64::from(n.vel)
                };
                if !out.emit(a, b - a, i64::from(n.pitch), vel, &tt) {
                    return out.check();
                }
            }
            // n08: the parts of the note outside the mask, kept.
            if let Some((mode, step, ratio)) = split {
                let (s, e) = (i128::from(n.start), i128::from(n.end()));
                let mut at = s;
                let mut parts: Vec<(i128, i128)> = Vec::new();
                for &(a, b) in &inside {
                    if a > at {
                        parts.push((at, a.min(e)));
                    }
                    at = at.max(b);
                }
                if at < e {
                    parts.push((at, e));
                }
                let mut tt = t;
                let mut vel = i64::from(n.vel);
                if mode == 0 {
                    if let Some(st) = st {
                        tt.sub = st.sub_offset(t.sub, step).0;
                    }
                } else {
                    vel = (f64::from(n.vel) * ratio).round().clamp(1.0, 127.0) as i64;
                }
                for (a, b) in parts.into_iter().filter(|p| p.1 > p.0) {
                    outside += 1;
                    if !out.emit(a, b - a, i64::from(n.pitch), vel, &tt) {
                        return out.check();
                    }
                }
            }
            out.check()?;
        }
        let mut msg = format!("{} clipped note piece(s).", crate::midi::group_digits(kept));
        if self.split.is_some() {
            msg.push_str(&format!(
                " {} outside part(s) kept.",
                crate::midi::group_digits(outside)
            ));
        }
        if plan.no_ranges {
            msg.push_str(match self.ranges.map_or(0, |r| r.0) {
                0 => " There are no marker sections to mask with.",
                1 => " There are no chapters to mask with.",
                _ => " There is no time selection to mask with.",
            });
        }
        out.message = Some(msg);
        Ok(())
    }
}

// ================================================================ Paint Color

pub const PAINT_COLOR: ToolDef = ToolDef {
    id: "paint_color",
    spec: paint_spec,
    build: paint_build,
};

const PROPS: &[&str] = &["Velocity", "Length", "Pitch", "Release"];
/// Notes per (track, cell).
type CellCounts = std::collections::HashMap<(u16, u64), u32>;

const REGION_SEEDS: &[&str] = &[
    "The region of the first selected note",
    "The largest region",
    "Every region (palette cycles per region)",
];

/// Connected regions of note art: two notes of the same track touch when
/// their pitches differ by at most one row and their time spans (closed,
/// widened by `gap` on the right) intersect. Returns each note's region
/// representative (the smallest note index of the region).
///
/// `O(n log n)`: notes of one row are merged into disjoint runs by a sweep;
/// runs of adjacent rows are joined by a two-pointer merge (union–find), so
/// dense art never compares every note with every note.
pub fn connected_regions(ns: &[TNote], gap: u64) -> Vec<u32> {
    let n = ns.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            let g = p[p[x as usize] as usize];
            p[x as usize] = g;
            x = g;
        }
        x
    }
    fn union(p: &mut [u32], a: u32, b: u32) {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            // Smallest index wins (deterministic representative).
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            p[hi as usize] = lo;
        }
    }
    // Rows: (track, pitch) → note indices in start order.
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by_key(|&i| {
        let x = &ns[i as usize];
        (x.track, x.pitch, x.start, i)
    });
    // Runs per row: (track, pitch, start, end + gap, first note).
    let mut runs: Vec<(u16, u8, u64, u64, u32)> = Vec::new();
    for &i in &order {
        let x = &ns[i as usize];
        let e = x.end().saturating_add(gap);
        if let Some(r) = runs.last_mut()
            && r.0 == x.track
            && r.1 == x.pitch
            && x.start <= r.3
        {
            r.3 = r.3.max(e);
            union(&mut parent, r.4, i);
            continue;
        }
        runs.push((x.track, x.pitch, x.start, e, i));
    }
    // Adjacent rows: runs of (t, p) against runs of (t, p + 1).
    let mut a = 0usize;
    while a < runs.len() {
        let (t, p) = (runs[a].0, runs[a].1);
        let a_end = a + runs[a..].partition_point(|r| (r.0, r.1) == (t, p));
        let b = a_end;
        if p < u8::MAX && b < runs.len() && (runs[b].0, runs[b].1) == (t, p + 1) {
            let b_end = b + runs[b..].partition_point(|r| (r.0, r.1) == (t, p + 1));
            let (mut i, mut j) = (a, b);
            while i < a_end && j < b_end {
                let (ra, rb) = (runs[i], runs[j]);
                if ra.2 <= rb.3 && rb.2 <= ra.3 {
                    union(&mut parent, ra.4, rb.4);
                }
                if ra.3 < rb.3 {
                    i += 1;
                } else {
                    j += 1;
                }
            }
        }
        a = a_end;
    }
    (0..n as u32).map(|i| find(&mut parent, i)).collect()
}

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PAINT_COLOR_HELP: &[(&str, &str)] = &[
    (
        "sub",
        "The subchannel (color) the notes are painted, by its place in the track's list (0 = the first).",
    ),
    (
        "set_channel",
        "Also gives each painted note the MIDI channel of its new subchannel; off keeps each note's MIDI channel.",
    ),
    (
        "a1",
        "Repaints only the notes that already have one color, leaving the others alone.",
    ),
    (
        "a1.from",
        "The subchannel (by place in the list) whose notes are repainted.",
    ),
    (
        "a2",
        "Paints the notes with a palette in turn, note after note.",
    ),
    (
        "a2.palette",
        "The subchannels used in turn, by place in the track's list; the list repeats.",
    ),
    (
        "a3",
        "Paints a gradient across time: the palette spread from the selection's start to its end.",
    ),
    (
        "a3.palette",
        "The subchannels from start to end, by place in the track's list.",
    ),
    (
        "a4",
        "Paints a gradient across pitch: the palette spread from the lowest note to the highest.",
    ),
    (
        "a4.palette",
        "The subchannels from bottom to top, by place in the track's list.",
    ),
    (
        "a5",
        "Paints a checkerboard of the main color and a second color.",
    ),
    ("a5.cell", "The width of one checker cell, as a note value."),
    ("a5.rows", "The height of one checker cell, in keys."),
    (
        "a5.other",
        "The second color of the checkerboard (place in the track's list).",
    ),
    ("a6", "Paints stripes across pitch or time."),
    (
        "a6.dir",
        "Horizontal stripes change with pitch; vertical stripes change with time.",
    ),
    (
        "a6.width",
        "The width of each stripe: keys for horizontal stripes, time cells for vertical ones.",
    ),
    (
        "a6.palette",
        "The stripes' colors in turn (places in the track's list); the list repeats.",
    ),
    (
        "a7",
        "Gives every note a random color from the palette (seeded).",
    ),
    (
        "a7.palette",
        "The colors drawn from (places in the track's list).",
    ),
    ("a8", "Colors notes by how crowded their surroundings are."),
    (
        "a8.cell",
        "The size of the cells in which notes are counted.",
    ),
    (
        "a8.palette",
        "The colors from sparsest (first) to densest (last).",
    ),
    (
        "a9",
        "Colors notes by a property: velocity, length, pitch or release.",
    ),
    ("a9.prop", "The property that picks the color."),
    (
        "a9.palette",
        "The colors from the lowest value (first) to the highest (last).",
    ),
    (
        "a10",
        "Paints only one connected shape of notes (notes touching in time and a key apart), or every shape its own color.",
    ),
    (
        "a10.gap",
        "Notes up to this many ticks apart still count as touching.",
    ),
    (
        "a10.seed",
        "Which shape is painted: the one holding the first selected note, the largest, or every shape (each with the next palette color).",
    ),
    (
        "a10.palette",
        "For Every Region: the colors given to the shapes in turn.",
    ),
    (
        "n01",
        "Paints rings, a pinwheel, a spiral or box frames around a center point.",
    ),
    (
        "n01.by",
        "The pattern: rings by distance, a pinwheel by angle, a spiral, or rectangular frames.",
    ),
    (
        "n01.cx",
        "The center's place in time, as a share of the selection's span.",
    ),
    (
        "n01.cy",
        "The center's place in pitch, as a share of the selection's range.",
    ),
    (
        "n01.band",
        "How wide each ring or band is, as a share of the selection's size.",
    ),
    (
        "n01.palette",
        "The bands' colors in turn, outward from the center.",
    ),
    (
        "n02",
        "Paints the edge of each note shape one color and, optionally, its inside another.",
    ),
    ("n02.edge", "The edge color (place in the track's list)."),
    (
        "n02.interior",
        "Keep the inside's colors, or paint the inside too.",
    ),
    ("n02.inner", "For Paint: the inside color."),
    (
        "n02.hood",
        "Which neighbors decide the edge: the 4 sides, or the sides and corners.",
    ),
    (
        "n03",
        "Blends two colors in a fine ordered pattern along a gradient (dithering).",
    ),
    ("n03.a", "The color at the gradient's start."),
    ("n03.b", "The color at the gradient's end."),
    (
        "n03.dir",
        "The gradient runs across time, across pitch, or outward from the center.",
    ),
    (
        "n03.matrix",
        "The size of the dither pattern (larger = smoother blend).",
    ),
    (
        "n03.curve",
        "How the blend moves from color A to color B along the direction.",
    ),
    ("n04", "Gives each pitch class its own color."),
    (
        "n04.classes",
        "The color of C, C♯ … B in order (places in the track's list).",
    ),
    ("n05", "Paints diagonal hatch lines across the notes."),
    ("n05.angle", "The angle of the hatch lines."),
    ("n05.spacing", "The distance between lines, in keys."),
    ("n05.width", "The thickness of each line, in keys."),
    ("n05.colour", "The hatch color (place in the track's list)."),
    (
        "n05.cross",
        "Single lines, or crossed lines in both directions.",
    ),
    ("n06", "Paints text onto the notes in a pixel font."),
    ("n06.text", "The text written."),
    ("n06.size", "The letter height, in keys."),
    (
        "n06.x",
        "Where the text starts in time, as a share of the selection's span.",
    ),
    (
        "n06.y",
        "Where the text sits in pitch, as a share of the selection's range.",
    ),
    ("n06.colour", "The text color (place in the track's list)."),
    ("n07", "Colors notes by their place in the bar."),
    ("n07.grid", "The grid the bar is divided into."),
    (
        "n07.palette",
        "The color of each grid position in turn; the list repeats.",
    ),
    ("n08", "Cycles colors in waves through the notes."),
    (
        "n08.period",
        "How long, in ticks, one full cycle through the palette takes.",
    ),
    ("n08.palette", "The colors cycled through."),
    (
        "n08.dir",
        "The waves run along time, along pitch, or diagonally.",
    ),
    (
        "n09",
        "Colors notes by how many other notes are stacked under them.",
    ),
    (
        "n09.palette",
        "The colors by stack depth (the first = no notes under it).",
    ),
    (
        "n09.count",
        "Count only notes on the same key, or any note at that time.",
    ),
    (
        "n10",
        "Blurs the colors: each note takes the most common color around it.",
    ),
    (
        "n10.radius",
        "How far in time, in ticks, the neighbors reach.",
    ),
    (
        "n10.rows",
        "How many keys above and below the neighbors reach.",
    ),
    ("n10.iter", "How many times the blur is repeated."),
    ("n11", "Replaces colors through a table of pairs."),
    (
        "n11.pairs",
        "Pairs of colors, from then to (places in the track's list): 0 1 1 0 swaps the first two colors.",
    ),
];

fn paint_spec() -> ToolSpec {
    let mut p = vec![
        int(
            "sub",
            "Subchannel (position in the track's list)",
            0,
            4095,
            1,
        ),
        flag(
            "set_channel",
            "Also set the subchannel's MIDI channel",
            false,
        ),
        seed("seed"),
        adv_switch(1, "Single subchannel paint (only notes of one color)"),
        int("a1.from", "Repaint only subchannel position", 0, 4095, 0).adv(1),
        adv_switch(2, "Cyclic palette brush"),
        list(
            "a2.palette",
            "Palette (subchannel positions)",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(2),
        adv_switch(3, "Gradient by X (time)"),
        list(
            "a3.palette",
            "Palette from start to end",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(3),
        adv_switch(4, "Gradient by Y (pitch)"),
        list(
            "a4.palette",
            "Palette from bottom to top",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(4),
        adv_switch(5, "Checker pattern"),
        choice("a5.cell", "Cell width", NOTE_VALUES, 2).adv(5),
        int("a5.rows", "Cell height (rows)", 1, 64, 2).adv(5),
        int("a5.other", "Second subchannel position", 0, 4095, 0).adv(5),
        adv_switch(6, "Stripe pattern"),
        choice(
            "a6.dir",
            "Stripes",
            &["Horizontal (by pitch)", "Vertical (by time)"],
            0,
        )
        .adv(6),
        int("a6.width", "Stripe width (rows / cells)", 1, 64, 2).adv(6),
        list("a6.palette", "Palette", 0, 4095, 64, &[0, 1]).adv(6),
        adv_switch(7, "Seeded scatter colors"),
        list("a7.palette", "Palette", 0, 4095, 64, &[0, 1, 2, 3]).adv(7),
        adv_switch(8, "Density-based coloring"),
        choice("a8.cell", "Density cell", NOTE_VALUES, 2).adv(8),
        list(
            "a8.palette",
            "Palette from sparse to dense",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(8),
        adv_switch(9, "Property-based coloring"),
        choice("a9.prop", "Property", PROPS, 0).adv(9),
        list(
            "a9.palette",
            "Palette from low to high",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(9),
        adv_switch(10, "Flood fill a connected region"),
        int(
            "a10.gap",
            "Join notes up to this many ticks apart",
            0,
            1_000_000,
            0,
        )
        .adv(10),
        choice("a10.seed", "Region", REGION_SEEDS, 0).adv(10),
        list(
            "a10.palette",
            "Palette for “every region”",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(10),
        // V09-S38 step 4: functions declared by stable id (TOOL_CATALOG
        // n01 … n11).
        flag("n01", "Polar gradient", false),
        choice("n01.by", "By", POLAR, 0),
        float("n01.cx", "Center X", 0.0, 100.0, 50.0, "%"),
        float("n01.cy", "Center Y", 0.0, 100.0, 50.0, "%"),
        float("n01.band", "Band width", 1.0, 100.0, 10.0, "%"),
        list("n01.palette", "Palette", 0, 4095, 64, &[0, 1, 2, 3]),
        flag("n02", "Edge paint", false),
        int("n02.edge", "Edge color", 0, 4095, 1),
        choice("n02.interior", "Interior", INTERIOR, 0),
        int("n02.inner", "Interior color", 0, 4095, 0),
        choice("n02.hood", "Neighborhood", HOODS, 0),
        flag("n03", "Ordered dither", false),
        int("n03.a", "Color A", 0, 4095, 0),
        int("n03.b", "Color B", 0, 4095, 1),
        choice("n03.dir", "Direction", DITHER_DIRS, 0),
        choice("n03.matrix", "Matrix", MATRICES, 1),
        curve("n03.curve", "Gradient", Curve::line(0.0, 1.0)),
        flag("n04", "Pitch-class colors", false),
        list(
            "n04.classes",
            "Color per pitch class (C, C♯ … B)",
            0,
            4095,
            12,
            &[0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3],
        ),
        flag("n05", "Hatch", false),
        float("n05.angle", "Angle", -90.0, 90.0, 45.0, "°"),
        int("n05.spacing", "Spacing (rows)", 2, 64, 6),
        int("n05.width", "Width (rows)", 1, 32, 1),
        int("n05.colour", "Color", 0, 4095, 1),
        choice("n05.cross", "Cross", CROSS, 0),
        flag("n06", "Glyph paint", false),
        text("n06.text", "Text", 64, "PLUM"),
        int("n06.size", "Size (rows)", 5, 128, 14),
        float("n06.x", "Position X", 0.0, 100.0, 10.0, "%"),
        float("n06.y", "Position Y", 0.0, 100.0, 30.0, "%"),
        int("n06.colour", "Color", 0, 4095, 1),
        flag("n07", "Metric paint", false),
        choice("n07.grid", "Grid", NOTE_VALUES, 2),
        list(
            "n07.palette",
            "Palette per position",
            0,
            4095,
            64,
            &[1, 0, 0, 0],
        ),
        flag("n08", "Palette cycle", false),
        ticks("n08.period", "Period", 1, 1 << 40, 3840),
        list("n08.palette", "Palette", 0, 4095, 64, &[0, 1, 2, 3]),
        choice("n08.dir", "Direction", CYCLE_DIRS, 0),
        flag("n09", "Stack depth", false),
        list(
            "n09.palette",
            "Palette by depth",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        ),
        choice("n09.count", "Count", DEPTH_COUNT, 0),
        flag("n10", "Color blur", false),
        ticks("n10.radius", "Radius", 0, 1 << 30, 480),
        int("n10.rows", "Rows", 1, 24, 1),
        int("n10.iter", "Iterations", 1, 16, 2),
        flag("n11", "Replace table", false),
        list(
            "n11.pairs",
            "Pairs (from, to, …)",
            0,
            4095,
            128,
            &[0, 1, 1, 0],
        ),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, PAINT_COLOR_HELP);
    ToolSpec {
        id: "paint_color",
        name: "Paint Color",
        family: Family::Geometry,
        summary: "Paints the selected notes into a subchannel (its color) — subchannel identity only; the notes keep their MIDI channel unless asked.",
        params: p,
        advanced: vec![
            "Single subchannel paint",
            "Cyclic palette brush",
            "Gradient by X",
            "Gradient by Y",
            "Checker pattern",
            "Stripe pattern",
            "Seeded scatter",
            "Density-based coloring",
            "Property-based coloring",
            "Flood fill connected region",
        ],
        // The catalog's orders = the run order (`Paint9`).
        functions: [
            ("n01", "Polar gradient", 110),
            ("n02", "Edge paint", 120),
            ("n03", "Ordered dither", 130),
            ("n04", "Pitch-class colors", 140),
            ("n05", "Hatch", 150),
            ("n06", "Glyph paint", 160),
            ("n07", "Metric paint", 170),
            ("n08", "Palette cycle", 180),
            ("n09", "Stack depth", 190),
            ("n10", "Color blur", 200),
            ("n11", "Replace table", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// Palette entry `t ∈ [0, 1]` of `pal` (or `fallback`).
fn pick(pal: &[i64], t: f64, fallback: usize) -> usize {
    if pal.is_empty() {
        return fallback;
    }
    let i = ((t.clamp(0.0, 1.0) * pal.len() as f64) as usize).min(pal.len() - 1);
    pal[i].max(0) as usize
}

fn paint_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-0143: with Control Sweeps the v0.9 kernel evaluates them per note.
    if CLIP_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return paint9_build(p, b);
    }
    let target = p.int("sub").max(0) as usize;
    let set_channel = p.flag("set_channel");
    let seed = p.seed("seed");
    let only = p.adv(1).then(|| p.int("a1.from").max(0) as usize);
    let cyclic = p.adv(2).then(|| p.list("a2.palette"));
    let grad_x = p.adv(3).then(|| p.list("a3.palette"));
    let grad_y = p.adv(4).then(|| p.list("a4.palette"));
    let checker = p.adv(5).then(|| {
        (
            note_value(p.choice("a5.cell"), 240, b.ctx),
            p.int("a5.rows").max(1) as u64,
            p.int("a5.other").max(0) as usize,
        )
    });
    let stripes = p.adv(6).then(|| {
        (
            p.choice("a6.dir"),
            p.int("a6.width").max(1) as u64,
            p.list("a6.palette"),
        )
    });
    let scatter = p.adv(7).then(|| p.list("a7.palette"));
    let density = p.adv(8).then(|| {
        (
            note_value(p.choice("a8.cell"), 240, b.ctx),
            p.list("a8.palette"),
        )
    });
    let prop = p
        .adv(9)
        .then(|| (p.choice("a9.prop"), p.list("a9.palette")));
    let flood = p
        .adv(10)
        .then(|| (p.int("a10.gap").max(0) as u64, p.choice("a10.seed")));
    let flood_pal = p.adv(10).then(|| p.list("a10.palette"));
    let cell_x = b.ctx.ppq.max(1);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let ns = &src.notes;
            let st = &src.stats;
            // Density per (track, cell) (function 8).
            let dens: Option<(u64, CellCounts, u32)> = density.as_ref().map(|(cell, _)| {
                let mut m = std::collections::HashMap::new();
                for n in ns {
                    *m.entry((n.track, n.start / cell.max(&1))).or_insert(0u32) += 1;
                }
                let max = m.values().copied().max().unwrap_or(1);
                (*cell, m, max)
            });
            // Flood fill (function 10): connected regions (notes touching in time
            // within `gap` on the same or an adjacent row of the same track).
            let regions: Option<(Vec<u32>, Vec<bool>)> = flood.map(|(gap, mode)| {
                let comp = connected_regions(ns, gap);
                let paint: Vec<bool> = match mode {
                    // The region of the first selected note.
                    0 => comp
                        .iter()
                        .map(|&c| Some(c) == comp.first().copied())
                        .collect(),
                    // The largest region (ties → the earliest).
                    1 => {
                        let mut size = vec![0u32; ns.len()];
                        for &c in &comp {
                            size[c as usize] += 1;
                        }
                        let best = comp
                            .iter()
                            .copied()
                            .max_by_key(|&c| (size[c as usize], std::cmp::Reverse(c)))
                            .unwrap_or(0);
                        comp.iter().map(|&c| c == best).collect()
                    }
                    _ => vec![true; ns.len()],
                };
                (comp, paint)
            });
            // Region number in order of first appearance (mode 2 palette).
            let region_rank: Vec<u32> = match &regions {
                Some((comp, _)) => {
                    let mut rank = vec![u32::MAX; ns.len()];
                    let mut next = 0u32;
                    comp.iter()
                        .map(|&c| {
                            if rank[c as usize] == u32::MAX {
                                rank[c as usize] = next;
                                next += 1;
                            }
                            rank[c as usize]
                        })
                        .collect()
                }
                None => Vec::new(),
            };
            for (i, n) in ns.iter().enumerate() {
                let Some(tr) = src.track(n.track) else {
                    continue;
                };
                let cur = tr.sub_index(n.sub).unwrap_or(0);
                let mut paint = true;
                if let Some(f) = only {
                    paint &= cur == f;
                }
                if let Some((_, r)) = &regions {
                    paint &= r[i];
                }
                let mut want = target;
                if let Some(pal) = &cyclic
                    && !pal.is_empty()
                {
                    want = pal[i % pal.len()].max(0) as usize;
                }
                if let Some(pal) = &grad_x {
                    want = pick(pal, pos(n, st), want);
                }
                if let Some(pal) = &grad_y {
                    let span = f64::from(st.max_pitch.saturating_sub(st.min_pitch)).max(1.0);
                    want = pick(
                        pal,
                        f64::from(n.pitch.saturating_sub(st.min_pitch)) / span,
                        want,
                    );
                }
                if let Some((cell, rows, other)) = checker {
                    let cx = n.start / cell.max(1);
                    let cy = u64::from(n.pitch) / rows;
                    if (cx + cy) % 2 == 1 {
                        want = other;
                    }
                }
                if let Some((dir, w, pal)) = &stripes
                    && !pal.is_empty()
                {
                    let k = if *dir == 0 {
                        u64::from(n.pitch) / w
                    } else {
                        n.start / (cell_x * w)
                    };
                    want = pal[(k % pal.len() as u64) as usize].max(0) as usize;
                }
                if let Some(pal) = &scatter
                    && !pal.is_empty()
                {
                    let k = Rng::for_note(seed, salt::SUB, n).range_i64(0, pal.len() as i64 - 1);
                    want = pal[k as usize].max(0) as usize;
                }
                if let (Some((cell, m, max)), Some((_, pal))) = (&dens, &density) {
                    let c = m
                        .get(&(n.track, n.start / (*cell).max(1)))
                        .copied()
                        .unwrap_or(0);
                    want = pick(
                        pal,
                        f64::from(c.saturating_sub(1)) / f64::from((*max).max(2) - 1),
                        want,
                    );
                }
                if let Some((k, pal)) = &prop {
                    let t = match k {
                        0 => f64::from(n.vel.saturating_sub(1)) / 126.0,
                        1 => (n.dur as f64 / st.max_dur.max(1) as f64).min(1.0),
                        // Pitch from the lowest to the highest key of the project.
                        2 => f64::from(n.pitch) / f64::from(src.keys.max_pitch()),
                        _ => f64::from(n.rel) / 127.0,
                    };
                    want = pick(pal, t, want);
                }
                if let (Some((_, 2)), Some(pal)) = (flood, &flood_pal)
                    && !pal.is_empty()
                {
                    want = pal[region_rank[i] as usize % pal.len()].max(0) as usize;
                }
                let mut t = *n;
                if paint && !tr.subs.is_empty() {
                    let (id, ch) = tr.subs[want.min(tr.subs.len() - 1)];
                    t.sub = id;
                    if set_channel {
                        t.ch = ch;
                    }
                }
                out.emit(
                    i128::from(t.start),
                    i128::from(t.dur),
                    i64::from(t.pitch),
                    i64::from(t.vel),
                    &t,
                );
                out.check()?;
            }
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}

// ------------------------------------------------- v0.9 (V09-S38 step 4)

const POLAR: &[&str] = &["Radius (rings)", "Angle (pinwheel)", "Spiral", "Box frames"];
const INTERIOR: &[&str] = &["Keep", "Paint"];
const HOODS: &[&str] = &["4 (sides)", "8 (sides and corners)"];
const DITHER_DIRS: &[&str] = &["X (time)", "Y (pitch)", "Radial"];
const MATRICES: &[&str] = &["2 × 2", "4 × 4", "8 × 8"];
const CROSS: &[&str] = &["Single", "Crossed"];
const CYCLE_DIRS: &[&str] = &["Time", "Pitch", "Diagonal"];
const DEPTH_COUNT: &[&str] = &["Same key", "Any key in the column"];

/// PlumSpirit's own 5 × 7 bitmap font (n06 Glyph paint): rows top to
/// bottom, bit 4 = the leftmost column. Lower-case letters are drawn as
/// capitals; any other character is a hollow box.
const FONT: &[(char, [u8; 7])] = &[
    (' ', [0, 0, 0, 0, 0, 0, 0]),
    (
        'A',
        [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        'B',
        [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
    ),
    (
        'C',
        [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
    ),
    (
        'D',
        [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
    ),
    (
        'E',
        [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
    ),
    (
        'F',
        [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
    ),
    (
        'G',
        [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
    ),
    (
        'H',
        [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        'I',
        [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
    ),
    (
        'J',
        [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
    ),
    (
        'K',
        [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
    ),
    (
        'L',
        [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
    ),
    (
        'M',
        [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        'N',
        [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        'O',
        [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        'P',
        [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
    ),
    (
        'Q',
        [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
    ),
    (
        'R',
        [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
    ),
    (
        'S',
        [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
    ),
    (
        'T',
        [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
    ),
    (
        'U',
        [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        'V',
        [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
    ),
    (
        'W',
        [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
    ),
    (
        'X',
        [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
    ),
    (
        'Y',
        [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
    ),
    (
        'Z',
        [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
    ),
    (
        '0',
        [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
    ),
    (
        '1',
        [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
    ),
    (
        '2',
        [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
    ),
    (
        '3',
        [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
    ),
    (
        '4',
        [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
    ),
    (
        '5',
        [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
    ),
    (
        '6',
        [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        '7',
        [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
    ),
    (
        '8',
        [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        '9',
        [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
    ),
    (
        '!',
        [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
    ),
    (
        '?',
        [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100,
        ],
    ),
    (
        '.',
        [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
    ),
    (
        ',',
        [
            0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000,
        ],
    ),
    (
        '-',
        [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
    ),
    (
        '+',
        [
            0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
        ],
    ),
    (
        ':',
        [
            0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000,
        ],
    ),
    (
        '\'',
        [
            0b00100, 0b00100, 0b01000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
    ),
    (
        '"',
        [
            0b01010, 0b01010, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
    ),
    (
        '/',
        [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
    ),
    (
        '(',
        [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
    ),
    (
        ')',
        [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
    ),
    (
        '=',
        [
            0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000,
        ],
    ),
    (
        '#',
        [
            0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010,
        ],
    ),
    (
        '*',
        [
            0b00000, 0b10101, 0b01110, 0b11111, 0b01110, 0b10101, 0b00000,
        ],
    ),
    (
        '&',
        [
            0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101,
        ],
    ),
    (
        '_',
        [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
    ),
    (
        '<',
        [
            0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010,
        ],
    ),
    (
        '>',
        [
            0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000,
        ],
    ),
];

/// A 5 × 7 glyph's rows, top to bottom (bit 4 = the leftmost column).
type Glyph = [u8; 7];

/// The hollow box drawn for characters the font lacks.
const NO_GLYPH: [u8; 7] = [
    0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111,
];

/// The 5 × 7 bitmap of `c` (rows top to bottom).
fn glyph(c: char) -> [u8; 7] {
    let c = c.to_ascii_uppercase();
    FONT.iter().find(|g| g.0 == c).map_or(NO_GLYPH, |g| g.1)
}

/// The Bayer threshold matrix of side `m` (a power of two): entries 0 …
/// m² − 1, `B(2n) = [[4B, 4B + 2], [4B + 3, 4B + 1]]`.
fn bayer(m: usize) -> Vec<u32> {
    let mut b = vec![0u32];
    let mut n = 1;
    while n < m {
        let mut nb = vec![0u32; 4 * n * n];
        for y in 0..n {
            for x in 0..n {
                let v = 4 * b[y * n + x];
                nb[y * 2 * n + x] = v;
                nb[y * 2 * n + x + n] = v + 2;
                nb[(y + n) * 2 * n + x] = v + 3;
                nb[(y + n) * 2 * n + x + n] = v + 1;
            }
        }
        b = nb;
        n *= 2;
    }
    b
}

/// The v0.9 Paint Color: the v0.8 colour choice (`paint_build`'s closure,
/// the same order of overrides) with the functions declared by id; see
/// `docs/TRANSFORM_ENGINE.md` "Paint Color".
struct Paint9 {
    // ----- v0.8, as `paint_build` reads them
    target: usize,
    set_channel: bool,
    seed: u64,
    only: Option<usize>,
    cyclic: Option<Vec<i64>>,
    grad_x: Option<Vec<i64>>,
    grad_y: Option<Vec<i64>>,
    checker: Option<(u64, u64, usize)>,
    stripes: Option<(usize, u64, Vec<i64>)>,
    scatter: Option<Vec<i64>>,
    density: Option<(u64, Vec<i64>)>,
    prop: Option<(usize, Vec<i64>)>,
    flood: Option<(u64, usize)>,
    flood_pal: Option<Vec<i64>>,
    cell_x: u64,
    // ----- v0.9
    /// n01: by, centre X, centre Y, band (ratios), palette.
    polar: Option<(usize, f64, f64, f64, Vec<i64>)>,
    /// n02: edge colour, paint the interior, interior colour, 8-neighbourhood.
    edge: Option<(usize, bool, usize, bool)>,
    /// n03: colour A, colour B, direction, matrix side, gradient.
    dither: Option<(usize, usize, usize, usize, Curve)>,
    /// n04: a colour per pitch class.
    classes: Option<Vec<i64>>,
    /// n05: angle (°), spacing, width, colour, crossed.
    hatch: Option<(f64, i64, i64, usize, bool)>,
    /// n06: text, size, position X / Y (ratios), colour.
    glyphs: Option<(Vec<Glyph>, i64, f64, f64, usize)>,
    /// n07: grid, palette.
    metric: Option<(u64, Vec<i64>)>,
    /// n08: period, palette, direction.
    cycle: Option<(u64, Vec<i64>, usize)>,
    /// n09: palette, any key.
    depth: Option<(Vec<i64>, bool)>,
    /// n10: radius, rows, iterations.
    blur: Option<(u64, u8, u32)>,
    /// n11: (from, to) pairs.
    table: Option<Vec<(usize, usize)>>,
    /// B2-F01: the colour functions n01 – n11 in run order
    /// (`order::PAINT_COLOR_PASSES`).
    order: Vec<&'static str>,
    ctx: super::super::Ctx,
}

fn paint9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let list_on = |k: &str, l: &str| p.flag(k).then(|| p.list(l));
    let k = Paint9 {
        target: p.int("sub").max(0) as usize,
        set_channel: p.flag("set_channel"),
        seed: p.seed("seed"),
        only: p.adv(1).then(|| p.int("a1.from").max(0) as usize),
        cyclic: p.adv(2).then(|| p.list("a2.palette")),
        grad_x: p.adv(3).then(|| p.list("a3.palette")),
        grad_y: p.adv(4).then(|| p.list("a4.palette")),
        checker: p.adv(5).then(|| {
            (
                note_value(p.choice("a5.cell"), 240, b.ctx),
                p.int("a5.rows").max(1) as u64,
                p.int("a5.other").max(0) as usize,
            )
        }),
        stripes: p.adv(6).then(|| {
            (
                p.choice("a6.dir"),
                p.int("a6.width").max(1) as u64,
                p.list("a6.palette"),
            )
        }),
        scatter: p.adv(7).then(|| p.list("a7.palette")),
        density: p.adv(8).then(|| {
            (
                note_value(p.choice("a8.cell"), 240, b.ctx),
                p.list("a8.palette"),
            )
        }),
        prop: p
            .adv(9)
            .then(|| (p.choice("a9.prop"), p.list("a9.palette"))),
        flood: p
            .adv(10)
            .then(|| (p.int("a10.gap").max(0) as u64, p.choice("a10.seed"))),
        flood_pal: p.adv(10).then(|| p.list("a10.palette")),
        cell_x: b.ctx.ppq.max(1),
        polar: p.flag("n01").then(|| {
            (
                p.choice("n01.by"),
                p.float("n01.cx") / 100.0,
                p.float("n01.cy") / 100.0,
                p.float("n01.band").max(1.0) / 100.0,
                p.list("n01.palette"),
            )
        }),
        edge: p.flag("n02").then(|| {
            (
                p.int("n02.edge").max(0) as usize,
                p.choice("n02.interior") == 1,
                p.int("n02.inner").max(0) as usize,
                p.choice("n02.hood") == 1,
            )
        }),
        dither: p.flag("n03").then(|| {
            (
                p.int("n03.a").max(0) as usize,
                p.int("n03.b").max(0) as usize,
                p.choice("n03.dir"),
                2usize << p.choice("n03.matrix").min(2),
                p.curve("n03.curve"),
            )
        }),
        classes: list_on("n04", "n04.classes"),
        hatch: p.flag("n05").then(|| {
            (
                p.float("n05.angle"),
                p.int("n05.spacing").max(2),
                p.int("n05.width").max(1),
                p.int("n05.colour").max(0) as usize,
                p.choice("n05.cross") == 1,
            )
        }),
        glyphs: p.flag("n06").then(|| {
            (
                p.text("n06.text").chars().map(glyph).collect(),
                p.int("n06.size").max(5),
                p.float("n06.x") / 100.0,
                p.float("n06.y") / 100.0,
                p.int("n06.colour").max(0) as usize,
            )
        }),
        metric: p.flag("n07").then(|| {
            (
                note_value(p.choice("n07.grid"), 240, b.ctx).max(1),
                p.list("n07.palette"),
            )
        }),
        cycle: p.flag("n08").then(|| {
            (
                p.ticks("n08.period").max(1),
                p.list("n08.palette"),
                p.choice("n08.dir"),
            )
        }),
        depth: p
            .flag("n09")
            .then(|| (p.list("n09.palette"), p.choice("n09.count") == 1)),
        blur: p.flag("n10").then(|| {
            (
                p.ticks("n10.radius"),
                p.int("n10.rows").clamp(1, 24) as u8,
                p.int("n10.iter").clamp(1, 16) as u32,
            )
        }),
        table: p.flag("n11").then(|| {
            p.list("n11.pairs")
                .chunks_exact(2)
                .map(|c| (c[0].max(0) as usize, c[1].max(0) as usize))
                .collect()
        }),
        order: p.sequence(crate::transform::order::PAINT_COLOR_PASSES.members),
        ctx: b.ctx.clone(),
    };
    Ok(Box::new(k))
}

/// `pal[k mod len]` (or `fallback` for an empty palette).
fn cycle_pick(pal: &[i64], k: i64, fallback: usize) -> usize {
    if pal.is_empty() {
        return fallback;
    }
    pal[k.rem_euclid(pal.len() as i64) as usize].max(0) as usize
}

impl Paint9 {
    /// n02: whether each note touches an empty cell (cells of the snap step
    /// from the first start × one row; rows off the keyboard empty) on a
    /// side, or also on a corner. The occupied cells of each row are merged
    /// into runs (`RowMask`), so a note is checked by binary searches: its
    /// run starts or ends with it (left / right), or the rows above and
    /// below do not cover its cells (one more cell each way for corners) —
    /// O(n log n), whatever the span.
    fn edges(&self, src: &Source, eight: bool) -> Vec<bool> {
        super::super::spatial::cell_edges(
            &src.notes,
            src.keys,
            src.stats.min_start,
            self.ctx.snap.max(1),
            eight,
        )
    }

    /// n09: how many other selected notes overlap each note's span — on its
    /// key, or on any key — O(n log n).
    fn depths(ns: &[TNote], any_key: bool) -> Vec<u32> {
        let mut out = vec![0u32; ns.len()];
        let mut groups: std::collections::BTreeMap<u8, Vec<usize>> = Default::default();
        for (i, n) in ns.iter().enumerate() {
            groups
                .entry(if any_key { 0 } else { n.pitch })
                .or_default()
                .push(i);
        }
        for idx in groups.values() {
            let mut starts: Vec<u64> = idx.iter().map(|&i| ns[i].start).collect();
            let mut ends: Vec<u64> = idx.iter().map(|&i| ns[i].end()).collect();
            starts.sort_unstable();
            ends.sort_unstable();
            for &i in idx {
                let n = &ns[i];
                // Spans [s, e) meeting [n.s, n.e): starts before its end
                // minus ends at or before its start; itself when not empty.
                let a = starts.partition_point(|&s| s < n.end());
                let b = ends.partition_point(|&e| e <= n.start);
                out[i] = (a - b - usize::from(n.dur > 0)) as u32;
            }
        }
        out
    }

    /// n10: each painted note takes the colour most common among the notes
    /// starting within ± radius on the rows within ± rows (itself included;
    /// ties: its own colour, else the lowest), `iter` times — per row, one
    /// counting window advanced on each row of its band (no merging; the
    /// rows' starts and colours contiguous), O(iter · n · (2 rows + 1 +
    /// colours)).
    fn blur(
        ns: &[TNote],
        colour: &mut [usize],
        painted: &[bool],
        radius: u64,
        rows: u8,
        iter: u32,
    ) {
        let mut ids: Vec<usize> = colour.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let k = ids.len();
        let mut by_row: Vec<Vec<usize>> = vec![Vec::new(); 256];
        for (i, n) in ns.iter().enumerate() {
            by_row[usize::from(n.pitch)].push(i);
        }
        for v in &mut by_row {
            v.sort_by_key(|&i| (ns[i].start, i));
        }
        let starts: Vec<Vec<u64>> = by_row
            .iter()
            .map(|v| v.iter().map(|&i| ns[i].start).collect())
            .collect();
        let rr = usize::from(rows);
        let mut count = vec![0u32; k];
        let mut lo = vec![0usize; 2 * rr + 1];
        let mut hi = vec![0usize; 2 * rr + 1];
        for _ in 0..iter {
            // Each row's colours (dense ids) in its start order.
            let cols: Vec<Vec<u32>> = by_row
                .iter()
                .map(|v| {
                    v.iter()
                        .map(|&i| ids.binary_search(&colour[i]).unwrap_or(0) as u32)
                        .collect()
                })
                .collect();
            let mut next: Vec<usize> = colour.to_vec();
            for r in 0..256usize {
                if by_row[r].is_empty() {
                    continue;
                }
                let (b0, b1) = (r.saturating_sub(rr), (r + rr).min(255));
                count.iter_mut().for_each(|c| *c = 0);
                lo.iter_mut().for_each(|x| *x = 0);
                hi.iter_mut().for_each(|x| *x = 0);
                for (j, &i) in by_row[r].iter().enumerate() {
                    let s = starts[r][j];
                    let (from, to) = (s.saturating_sub(radius), s.saturating_add(radius));
                    for q in b0..=b1 {
                        let (st, co, x) = (&starts[q], &cols[q], q - b0);
                        while hi[x] < st.len() && st[hi[x]] <= to {
                            count[co[hi[x]] as usize] += 1;
                            hi[x] += 1;
                        }
                        while lo[x] < hi[x] && st[lo[x]] < from {
                            count[co[lo[x]] as usize] -= 1;
                            lo[x] += 1;
                        }
                    }
                    if !painted[i] {
                        continue;
                    }
                    let own = cols[r][j] as usize;
                    let best = count.iter().copied().max().unwrap_or(0);
                    let pick = if count[own] == best {
                        own
                    } else {
                        count.iter().position(|&c| c == best).unwrap_or(own)
                    };
                    next[i] = ids[pick];
                }
            }
            colour.copy_from_slice(&next);
        }
    }
}

impl Kernel for Paint9 {
    fn growth(&self) -> Growth {
        Growth::Map
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let ns = &src.notes;
        let st = &src.stats;
        let sw = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty());
        let places = if sw.is_some() {
            super::super::sweep::note_places(src)
        } else {
            Vec::new()
        };
        let place = |i: usize| places.get(i).copied().unwrap_or_default();
        let fl = |key: &str, v: f64, i: usize| match sw {
            Some(s) if s.drives(key) => s.float(key, v, &ns[i], place(i)),
            _ => v,
        };
        let int = |key: &str, v: i64, i: usize| match sw {
            Some(s) if s.drives(key) => s.int(key, v, &ns[i], place(i)),
            _ => v,
        };
        // ----- the v0.8 choice (`paint_build`'s closure)
        let dens: Option<(u64, CellCounts, u32)> = self.density.as_ref().map(|(cell, _)| {
            let mut m = std::collections::HashMap::new();
            for n in ns {
                *m.entry((n.track, n.start / cell.max(&1))).or_insert(0u32) += 1;
            }
            let max = m.values().copied().max().unwrap_or(1);
            (*cell, m, max)
        });
        let regions: Option<(Vec<u32>, Vec<bool>)> = self.flood.map(|(gap, mode)| {
            let comp = connected_regions(ns, gap);
            let paint: Vec<bool> = match mode {
                0 => comp
                    .iter()
                    .map(|&c| Some(c) == comp.first().copied())
                    .collect(),
                1 => {
                    let mut size = vec![0u32; ns.len()];
                    for &c in &comp {
                        size[c as usize] += 1;
                    }
                    let best = comp
                        .iter()
                        .copied()
                        .max_by_key(|&c| (size[c as usize], std::cmp::Reverse(c)))
                        .unwrap_or(0);
                    comp.iter().map(|&c| c == best).collect()
                }
                _ => vec![true; ns.len()],
            };
            (comp, paint)
        });
        let region_rank: Vec<u32> = match &regions {
            Some((comp, _)) => {
                let mut rank = vec![u32::MAX; ns.len()];
                let mut next = 0u32;
                comp.iter()
                    .map(|&c| {
                        if rank[c as usize] == u32::MAX {
                            rank[c as usize] = next;
                            next += 1;
                        }
                        rank[c as usize]
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        // ----- the v0.9 passes over the whole selection
        let edges = self.edge.map(|e| self.edges(src, e.3));
        let depths = self.depth.as_ref().map(|d| Self::depths(ns, d.1));
        let bayer_m = self.dither.as_ref().map(|d| (d.3, bayer(d.3)));
        let span_t = st.max_start.saturating_sub(st.min_start) as f64;
        let span_p = f64::from(st.max_pitch.saturating_sub(st.min_pitch)).max(1.0);
        let snap = self.ctx.snap.max(1);
        let n = ns.len();
        let mut colour = vec![0usize; n];
        let mut painted = vec![false; n];
        let mut own = vec![0usize; n];
        for (i, nt) in ns.iter().enumerate() {
            let Some(tr) = src.track(nt.track) else {
                continue;
            };
            let cur = tr.sub_index(nt.sub).unwrap_or(0);
            own[i] = cur;
            let mut paint = true;
            if let Some(f) = self.only {
                paint &= cur == f;
            }
            if let Some((_, r)) = &regions {
                paint &= r[i];
            }
            // n11 starts from the note's own colour.
            let mut want = if self.table.is_some() {
                cur
            } else {
                int("sub", self.target as i64, i).max(0) as usize
            };
            if let Some(pal) = &self.cyclic
                && !pal.is_empty()
            {
                want = pal[i % pal.len()].max(0) as usize;
            }
            if let Some(pal) = &self.grad_x {
                want = pick(pal, pos(nt, st), want);
            }
            if let Some(pal) = &self.grad_y {
                let span = f64::from(st.max_pitch.saturating_sub(st.min_pitch)).max(1.0);
                want = pick(
                    pal,
                    f64::from(nt.pitch.saturating_sub(st.min_pitch)) / span,
                    want,
                );
            }
            if let Some((cell, rows, other)) = self.checker {
                let cx = nt.start / cell.max(1);
                let cy = u64::from(nt.pitch) / rows;
                if (cx + cy) % 2 == 1 {
                    want = other;
                }
            }
            if let Some((dir, w, pal)) = &self.stripes
                && !pal.is_empty()
            {
                let k = if *dir == 0 {
                    u64::from(nt.pitch) / w
                } else {
                    nt.start / (self.cell_x * w)
                };
                want = pal[(k % pal.len() as u64) as usize].max(0) as usize;
            }
            if let Some(pal) = &self.scatter
                && !pal.is_empty()
            {
                let k = Rng::for_note(self.seed, salt::SUB, nt).range_i64(0, pal.len() as i64 - 1);
                want = pal[k as usize].max(0) as usize;
            }
            if let (Some((cell, m, max)), Some((_, pal))) = (&dens, &self.density) {
                let c = m
                    .get(&(nt.track, nt.start / (*cell).max(1)))
                    .copied()
                    .unwrap_or(0);
                want = pick(
                    pal,
                    f64::from(c.saturating_sub(1)) / f64::from((*max).max(2) - 1),
                    want,
                );
            }
            if let Some((k, pal)) = &self.prop {
                let t = match k {
                    0 => f64::from(nt.vel.saturating_sub(1)) / 126.0,
                    1 => (nt.dur as f64 / st.max_dur.max(1) as f64).min(1.0),
                    2 => f64::from(nt.pitch) / f64::from(src.keys.max_pitch()),
                    _ => f64::from(nt.rel) / 127.0,
                };
                want = pick(pal, t, want);
            }
            if let (Some((_, 2)), Some(pal)) = (self.flood, &self.flood_pal)
                && !pal.is_empty()
            {
                want = pal[region_rank[i] as usize % pal.len()].max(0) as usize;
            }
            colour[i] = if paint { want } else { cur };
            painted[i] = paint;
        }
        // B2-F01: the colour functions in run order (declared: n01 … n09,
        // n10, n11 — each overrides or repaints the colour so far) over the
        // painted notes.
        let stage = |id: &str, i: usize, mut want: usize| -> usize {
            let nt = &ns[i];
            let u = pos(nt, st);
            let v = f64::from(nt.pitch.saturating_sub(st.min_pitch)) / span_p;
            match id {
                "n01" => {
                    if let Some((by, cx, cy, band, pal)) = &self.polar
                        && !pal.is_empty()
                    {
                        let cx = fl("n01.cx", cx * 100.0, i) / 100.0;
                        let band = fl("n01.band", band * 100.0, i).max(1.0) / 100.0;
                        let (du, dv) = (u - cx, v - cy);
                        let turn = (dv.atan2(du) + std::f64::consts::PI) / std::f64::consts::TAU;
                        let d = du.hypot(dv);
                        let k = match by {
                            0 => d / band,
                            1 => turn / band,
                            2 => d / band + turn,
                            _ => du.abs().max(dv.abs()) / band,
                        };
                        want = cycle_pick(pal, k.floor() as i64, want);
                    }
                }
                "n02" => {
                    if let (Some((ec, interior, inner, _)), Some(e)) = (self.edge, &edges) {
                        if e[i] {
                            want = int("n02.edge", ec as i64, i).max(0) as usize;
                        } else if interior {
                            want = inner;
                        }
                    }
                }
                "n03" => {
                    if let (Some((a, b, dir, _, curve)), Some((m, mat))) = (&self.dither, &bayer_m)
                    {
                        let a = int("n03.a", *a as i64, i).max(0) as usize;
                        let t = match dir {
                            0 => u,
                            1 => v,
                            _ => ((u - 0.5).hypot(v - 0.5) / std::f64::consts::FRAC_1_SQRT_2)
                                .min(1.0),
                        };
                        let (cx, cy) = ((nt.start / snap) as usize % m, usize::from(nt.pitch) % m);
                        let threshold = (f64::from(mat[cy * m + cx]) + 0.5) / (m * m) as f64;
                        want = if curve.eval(t) > threshold { *b } else { a };
                    }
                }
                "n04" => {
                    if let Some(pal) = &self.classes {
                        want = cycle_pick(pal, i64::from(nt.pitch % 12), want);
                    }
                }
                "n05" => {
                    if let Some((angle, spacing, width, c, cross)) = self.hatch {
                        let angle = fl("n05.angle", angle, i).to_radians();
                        let spacing = int("n05.spacing", spacing, i).max(2) as f64;
                        let x = (nt.start.saturating_sub(st.min_start) / snap) as f64;
                        let y = f64::from(nt.pitch);
                        let on =
                            |a: f64| (y * a.cos() - x * a.sin()).rem_euclid(spacing) < width as f64;
                        if on(angle) || (cross && on(angle + std::f64::consts::FRAC_PI_2)) {
                            want = c;
                        }
                    }
                }
                "n06" => {
                    if let Some((text, size, gx, gy, c)) = &self.glyphs {
                        let size = int("n06.size", *size, i).max(5) as f64;
                        let gx = fl("n06.x", gx * 100.0, i) / 100.0;
                        let ph = size / 7.0;
                        let pw = ph * snap as f64;
                        let x0 = st.min_start as f64 + gx * span_t;
                        let y0 = f64::from(st.min_pitch) + gy * span_p;
                        let px = ((nt.start as f64 - x0) / pw).floor();
                        let py = ((f64::from(nt.pitch) + 0.5 - y0) / ph).floor();
                        if px >= 0.0 && (0.0..7.0).contains(&py) {
                            let (ch, col) = ((px as usize) / 6, (px as usize) % 6);
                            if col < 5
                                && text
                                    .get(ch)
                                    .is_some_and(|g| g[6 - py as usize] >> (4 - col) & 1 == 1)
                            {
                                want = *c;
                            }
                        }
                    }
                }
                "n07" => {
                    if let Some((grid, pal)) = &self.metric {
                        let (bar, _) = super::super::shared::metric_position::bar_bounds(
                            &self.ctx.meter,
                            nt.start,
                        );
                        want = cycle_pick(pal, ((nt.start - bar) / grid) as i64, want);
                    }
                }
                "n08" => {
                    if let Some((period, pal, dir)) = &self.cycle {
                        let period = int("n08.period", *period as i64, i).max(1) as f64;
                        let dt = nt.start.saturating_sub(st.min_start) as f64;
                        let dp = f64::from(nt.pitch.saturating_sub(st.min_pitch)) * snap as f64;
                        let k = match dir {
                            0 => dt / period,
                            1 => (dp / period).floor(),
                            _ => (dt + dp) / period,
                        };
                        want = cycle_pick(pal, k.floor() as i64, want);
                    }
                }
                "n09" => {
                    if let (Some((pal, _)), Some(d)) = (&self.depth, &depths)
                        && !pal.is_empty()
                    {
                        want = pal[(d[i] as usize).min(pal.len() - 1)].max(0) as usize;
                    }
                }
                _ => {}
            }
            want
        };
        for id in &self.order {
            match *id {
                "n10" => {
                    if let Some((radius, rows, iter)) = self.blur {
                        Self::blur(ns, &mut colour, &painted, radius, rows, iter);
                    }
                }
                "n11" => {
                    if let Some(t) = &self.table {
                        for (c, p) in colour.iter_mut().zip(&painted) {
                            if let Some(&(_, to)) = t.iter().find(|x| x.0 == *c)
                                && *p
                            {
                                *c = to;
                            }
                        }
                    }
                }
                id => {
                    for i in 0..n {
                        if painted[i] {
                            colour[i] = stage(id, i, colour[i]);
                        }
                    }
                }
            }
        }
        for (i, nt) in ns.iter().enumerate() {
            let Some(tr) = src.track(nt.track) else {
                continue;
            };
            let mut t = *nt;
            if painted[i] && !tr.subs.is_empty() {
                let (id, ch) = tr.subs[colour[i].min(tr.subs.len() - 1)];
                t.sub = id;
                if self.set_channel {
                    t.ch = ch;
                }
            }
            out.emit(
                i128::from(t.start),
                i128::from(t.dur),
                i64::from(t.pitch),
                i64::from(t.vel),
                &t,
            );
            out.check()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(ns: &[TNote], gap: u64) -> Vec<u32> {
        let n = ns.len();
        let mut comp: Vec<u32> = (0..n as u32).collect();
        // Repeated relaxation to the smallest reachable index.
        loop {
            let mut changed = false;
            for i in 0..n {
                for j in 0..n {
                    let (a, b) = (&ns[i], &ns[j]);
                    let touch = a.track == b.track
                        && a.pitch.abs_diff(b.pitch) <= 1
                        && a.start <= b.end() + gap
                        && b.start <= a.end() + gap;
                    if touch && comp[j] < comp[i] {
                        comp[i] = comp[j];
                        changed = true;
                    }
                }
            }
            if !changed {
                return comp;
            }
        }
    }

    #[test]
    fn the_bitmap_font_and_the_bayer_matrices_are_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for (c, rows) in super::FONT {
            assert!(seen.insert(*c), "{c} twice");
            assert!(rows.iter().all(|r| *r < 32), "{c}");
        }
        assert_eq!(super::glyph('h'), super::glyph('H'));
        assert_eq!(super::glyph('€'), super::NO_GLYPH);
        assert_eq!(super::bayer(2), vec![0, 2, 3, 1]);
        assert_eq!(
            super::bayer(4),
            vec![0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5]
        );
        let mut b8 = super::bayer(8);
        b8.sort_unstable();
        assert_eq!(b8, (0..64).collect::<Vec<u32>>());
    }

    #[test]
    fn gallop_finds_the_partition_point_from_any_start() {
        for n in 0..12usize {
            let r: Vec<(u64, u64)> = (0..n as u64).map(|i| (i * 10, i * 10 + 5)).collect();
            for at in 0..=n + 2 {
                for t in 0..=(n as u64 * 10 + 10) {
                    assert_eq!(
                        super::gallop(&r, at, |x| x.1 <= t),
                        r.partition_point(|x| x.1 <= t),
                        "n {n} at {at} t {t}"
                    );
                }
            }
        }
    }

    /// A tiled view's pieces and count against the tiled mask built
    /// explicitly (every copy shifted or mirrored, then merged), on random
    /// rows, periods, counts and queries.
    #[test]
    fn tiled_views_clip_like_the_explicit_union_of_their_copies() {
        let mut x: u64 = 0x711E_0038;
        let mut rnd = |m: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m.max(1)
        };
        for case in 0..300 {
            let mut row: Vec<(u64, u64)> = (0..rnd(8))
                .map(|_| {
                    let a = 100 + rnd(400);
                    (a, a + 1 + rnd(60))
                })
                .collect();
            let m = RowMask::from_rows(vec![std::mem::take(&mut row)]);
            let Some((e0, e1)) = m.extent() else {
                continue;
            };
            let t = super::Tile {
                period: 1 + rnd(500),
                count: 2 + rnd(6),
                mirror: rnd(2) == 1,
                e0,
                e1,
            };
            let v = super::View {
                r: super::Region::new(m.clone(), true),
                tile: Some(t),
            };
            let mut all: Vec<(u64, u64)> = Vec::new();
            for k in 0..t.count {
                for &(a, b) in m.row(0) {
                    let sh = k * t.period;
                    all.push(if t.mirror && k % 2 == 1 {
                        (e0 + e1 + sh - b, e0 + e1 + sh - a)
                    } else {
                        (a + sh, b + sh)
                    });
                }
            }
            let tiled = RowMask::from_rows(vec![all]);
            for _ in 0..20 {
                let s = rnd(4000);
                let e = s + rnd(1500);
                let mut got = Vec::new();
                v.clip(7, s, e, &mut got);
                let mut want = Vec::new();
                tiled.clip(0, s, e, |a, b| want.push((a, b)));
                assert_eq!(got, want, "case {case} [{s}, {e}) {t:?}");
                assert!(v.count(7, s, e) >= got.len() as u64);
            }
        }
    }

    #[test]
    fn connected_regions_equal_a_brute_force_flood_fill() {
        let mut r = Rng::for_index(7, 1, 0);
        for round in 0..40u64 {
            let mut ns: Vec<TNote> = (0..60)
                .map(|_| TNote {
                    start: r.range_i64(0, 3000) as u64,
                    dur: r.range_i64(1, 300) as u64,
                    pitch: r.range_i64(58, 66) as u8,
                    vel: 64,
                    track: r.range_i64(0, 1) as u16,
                    ..TNote::default()
                })
                .collect();
            ns.sort_by_key(|n| n.start);
            let gap = round % 3 * 50;
            assert_eq!(
                connected_regions(&ns, gap),
                brute(&ns, gap),
                "round {round}"
            );
        }
    }
}
