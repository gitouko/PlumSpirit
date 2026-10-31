//! Piano Roll edit tools as platform recipes: Delete (filters), Select
//! (filters → selection), Slice, Snap to Scale, Flip, Duplicate, Score
//! Stretch (v0.9 removed Interpolate). Interactive parts (brushes, lasso drawing, zoom, scrub) live in
//! the app; every batch behaviour is here so it previews and undoes like any
//! other tool.

use super::super::boundary::{self, PitchPolicy};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, note_key, salt};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::metric_position::bar_bounds;
use super::super::shared::scale_lattice::Lattice;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::{KEY_NAMES, Scale, SnapMode, scale_names};
use super::super::{Growth, Kernel, Output, Source, SourceStats, TNote, TransformError};
use super::duplicate::{
    DUP_AT, DUP_CANON, DUP_FIT, DUP_INCLUDE, DUP_MOTION, DUP_TARGETS, DUP_THIN, DUP_WITHIN, Dup9,
    DupNew,
};
use super::flip::{FLIP_ANCHOR, FLIP_HALF, FLIP_PLACE, FLIP_RECURSE, FLIP_REGION, Flip9, FlipNew};
use super::snap::{
    SNAP_CANDIDATES, SNAP_CHORD, SNAP_PER, SNAP_RULES, SNAP_TUNING, SNAP_WINDOW, Snap9, SnapNew,
};
use super::stretch::{STRETCH_CAP, STRETCH_KEEP, STRETCH_NPS, Stretch9, StretchNew};
use super::{Contextual, NOTE_VALUES, Pointwise, art_grid_values, boxed, note_value, pos};

/// Normalized position of a note in the selection's bounding box: x = start
/// time, y = pitch (0 = lowest selected pitch, 1 = highest).
fn norm_xy(n: &TNote, s: &SourceStats) -> (f64, f64) {
    let span = s.max_end.saturating_sub(s.min_start).max(1) as f64;
    let x = n.start.saturating_sub(s.min_start) as f64 / span;
    let ph = f64::from(s.max_pitch.saturating_sub(s.min_pitch)).max(1.0);
    let y = (f64::from(n.pitch) - f64::from(s.min_pitch)) / ph;
    (x, y)
}

fn point_in_polygon(x: f64, y: f64, poly: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn polygon(list: &[i64]) -> Vec<(f64, f64)> {
    list.chunks_exact(2)
        .map(|c| (c[0] as f64 / 1000.0, c[1] as f64 / 1000.0))
        .collect()
}

pub(super) fn sub_index(src: &Source, n: &TNote) -> usize {
    src.track(n.track)
        .and_then(|t| t.sub_index(n.sub))
        .unwrap_or(0)
}

/// Notes overlapped by an earlier note of the same pitch (and track) still
/// sounding at their start.
fn overlapped(src: &Source) -> Vec<bool> {
    let mut order: Vec<usize> = (0..src.notes.len()).collect();
    order.sort_by_key(|&i| {
        let n = &src.notes[i];
        (n.track, n.pitch, n.start, i)
    });
    let mut out = vec![false; src.notes.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        let (t, p) = (src.notes[order[i]].track, src.notes[order[i]].pitch);
        let mut reach = 0u64;
        while j < order.len() && src.notes[order[j]].track == t && src.notes[order[j]].pitch == p {
            let n = &src.notes[order[j]];
            if j > i && n.start < reach {
                out[order[j]] = true;
            }
            reach = reach.max(n.end());
            j += 1;
        }
        i = j;
    }
    out
}

/// Notes identical in (track, start, pitch[, subchannel]) to an earlier one.
fn duplicates(src: &Source, by_sub: bool) -> Vec<bool> {
    let mut seen = std::collections::HashSet::new();
    src.notes
        .iter()
        .map(|n| !seen.insert((n.track, n.start, n.pitch, if by_sub { n.sub } else { 0 })))
        .collect()
}

/// Per note: the number of notes in its (time cell × semitone band) cell, and
/// its rank inside the cell (time order).
fn cells(src: &Source, cell_ticks: u64, band: u8) -> (Vec<usize>, Vec<usize>) {
    let mut map: std::collections::HashMap<(u16, u64, u8), Vec<usize>> = Default::default();
    for (i, n) in src.notes.iter().enumerate() {
        map.entry((n.track, n.start / cell_ticks.max(1), n.pitch / band.max(1)))
            .or_default()
            .push(i);
    }
    let mut count = vec![0; src.notes.len()];
    let mut rank = vec![0; src.notes.len()];
    for v in map.values() {
        for (r, &i) in v.iter().enumerate() {
            count[i] = v.len();
            rank[i] = r;
        }
    }
    (count, rank)
}

// ================================================================ Delete

pub const DELETE: ToolDef = ToolDef {
    id: "delete",
    spec: delete_spec,
    build: delete_build,
};

const SHAPES: &[&str] = &["Rectangle", "Ellipse"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const DELETE_HELP: &[(&str, &str)] = &[
    (
        "invert",
        "Turns the filters around: the notes that match stay and every other selected note is deleted.",
    ),
    (
        "a1",
        "Deletes only notes whose pitch lies in a range of keys.",
    ),
    ("a1.lo", "The lowest key of the range that is deleted."),
    ("a1.hi", "The highest key of the range that is deleted."),
    ("a2", "Deletes only notes whose velocity lies in a range."),
    ("a2.lo", "The lowest velocity that is deleted."),
    ("a2.hi", "The highest velocity that is deleted."),
    ("a3", "Deletes only notes on the listed subchannels."),
    (
        "a3.subs",
        "The subchannel positions whose notes are deleted (0 = the track's first subchannel).",
    ),
    (
        "a4",
        "Thins crowded spots: in each time cell of each key, only the first few notes stay.",
    ),
    ("a4.cell", "The length of one time cell."),
    (
        "a4.keep",
        "How many notes of each key stay in one cell (in time order); the rest are deleted.",
    ),
    (
        "a5",
        "Deletes notes by their place in the selection's order, following a repeating pattern.",
    ),
    (
        "a5.pattern",
        "One digit per note in turn, repeated: 1 deletes the note, 0 keeps it (0, 1 deletes every second note).",
    ),
    (
        "a6",
        "Deletes each matching note by chance (seeded, so the same seed gives the same result).",
    ),
    ("a6.prob", "The chance that each note is deleted."),
    (
        "a7",
        "Deletes only notes that start while an earlier note on the same key is still sounding.",
    ),
    (
        "a8",
        "Deletes exact duplicates: notes with the same start and key as an earlier note.",
    ),
    (
        "a8.by_sub",
        "Counts notes as duplicates only when they are also on the same subchannel.",
    ),
    (
        "a9",
        "Deletes only notes inside a rectangle or an ellipse drawn over the selection.",
    ),
    ("a9.shape", "The shape of the area that is deleted."),
    (
        "a9.box",
        "The area's corners: left, bottom, right and top, in thousandths of the selection's width and height (250, 250, 750, 750 is the middle half).",
    ),
    (
        "a10",
        "Deletes only notes above or below a contour drawn across the selection.",
    ),
    (
        "a10.curve",
        "The contour: its height over the selection's time, in thousandths of the selection's pitch range.",
    ),
    ("a10.side", "Which side of the contour is deleted."),
    (
        "n01",
        "Deletes notes hidden inside a longer note on the same key that starts with them and lasts at least as long.",
    ),
    (
        "n01.same",
        "What counts as the same key: only the pitch, the pitch and the MIDI channel, or the pitch and the subchannel.",
    ),
    (
        "n01.tol",
        "How much later than the hidden note the longer note may start and still cover it, in ticks.",
    ),
    (
        "n02",
        "Deletes only notes whose length lies in a range (good for removing very short notes).",
    ),
    ("n02.lo", "The shortest length that is deleted, in ticks."),
    ("n02.hi", "The longest length that is deleted, in ticks."),
    ("n03", "Deletes only notes that are not in a scale."),
    ("n03.key", "The key of the scale."),
    ("n03.scale", "The scale whose notes stay."),
    (
        "n03.passing",
        "Delete Them Too removes every out-of-scale note; the other choice keeps out-of-scale notes shorter than 1/16 as passing notes.",
    ),
    (
        "n04",
        "Deletes notes by where they start in the bar, following a repeating pattern on a grid.",
    ),
    ("n04.grid", "The grid that divides each bar into positions."),
    (
        "n04.mask",
        "One digit per grid position from the bar line, repeated: 1 deletes the notes starting there, 0 keeps them.",
    ),
    (
        "n05",
        "Thins chords: in each chord only a set number of notes stay.",
    ),
    ("n05.keep", "How many notes of each chord stay."),
    (
        "n05.which",
        "Which notes stay: the highest, the lowest, or the outer ones (half from the top, half from the bottom).",
    ),
    (
        "n05.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
    (
        "n06",
        "Hollows out a block of notes: the notes on its edge stay and the inside is deleted.",
    ),
    (
        "n06.keep",
        "Outline Only deletes the whole inside; Every Nth Interior Row keeps every Nth row inside; Checkerboard keeps a checkered pattern inside.",
    ),
    (
        "n06.n",
        "The spacing used by Every Nth Interior Row and the square size of the checkerboard.",
    ),
    (
        "n07",
        "Deletes notes that start at the same time as a note of a reference (for example to remove doubled notes).",
    ),
    (
        "n07.ref",
        "The reference: the track's unselected notes, a subchannel of the track, or another track.",
    ),
    (
        "n07.num",
        "The subchannel position (1 = the first) or the track number in the track list, by the reference chosen.",
    ),
    (
        "n07.tol",
        "How far apart the two starts may be, in ticks, for the notes to collide.",
    ),
    (
        "n07.match",
        "Same Key collides only on the same pitch; Any Key collides on any pitch.",
    ),
    (
        "n08",
        "Crops the selection to a region: everything outside it is deleted.",
    ),
    (
        "n08.region",
        "The region to keep: the time selection, the loop, or the marker section that holds the selection's first note.",
    ),
    (
        "n08.crossing",
        "What happens to notes that cross the region's edge: cut at the edge, kept whole, or deleted.",
    ),
    (
        "n09",
        "Deletes notes that stand alone, with no other selected note nearby.",
    ),
    (
        "n09.radius",
        "How close in time another note must be for a note not to be alone, in ticks.",
    ),
    (
        "n09.rows",
        "How many keys above and below count as nearby (0 = the same key only).",
    ),
    (
        "n10",
        "Removes clashing intervals from chords, deleting as few notes as possible.",
    ),
    (
        "n10.intervals",
        "The intervals that count as a clash, in semitones within an octave (1 = a minor second, 6 = a tritone).",
    ),
    (
        "n10.which",
        "Which note of a clashing pair is deleted: the higher, the lower, or the quieter one.",
    ),
    (
        "n10.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
    (
        "n11",
        "Deletes matching notes only until a set number of notes is left (a note budget).",
    ),
    (
        "n11.most",
        "How many notes the selection is thinned down to; deleting stops when only this many are left.",
    ),
    (
        "n11.first",
        "Which matching notes go first: spread evenly in time, the quietest, or the shortest.",
    ),
];

fn delete_spec() -> ToolSpec {
    let mut p = vec![
        flag("invert", "Delete the notes that do NOT match", false),
        seed("seed"),
        adv_switch(1, "Pitch-range eraser"),
        pitch("a1.lo", "From", 0).adv(1),
        pitch("a1.hi", "To", 59).adv(1),
        adv_switch(2, "Velocity-range eraser"),
        int("a2.lo", "From", 1, 127, 1).adv(2),
        int("a2.hi", "To", 1, 127, 63).adv(2),
        adv_switch(3, "Subchannel eraser"),
        list("a3.subs", "Subchannel positions", 0, 4095, 4096, &[1]).adv(3),
        adv_switch(4, "Density-aware thinning"),
        choice("a4.cell", "Cell", NOTE_VALUES, 4).adv(4),
        int("a4.keep", "Keep at most per cell", 1, 1000, 2).adv(4),
        adv_switch(5, "Alternating-pattern erase"),
        list(
            "a5.pattern",
            "Delete (1) / keep (0) by order",
            0,
            1,
            64,
            &[0, 1],
        )
        .adv(5),
        adv_switch(6, "Probability erase"),
        float("a6.prob", "Probability", 0.0, 100.0, 50.0, "%").adv(6),
        adv_switch(7, "Overlap-only erase"),
        adv_switch(8, "Duplicate-note cleanup"),
        flag("a8.by_sub", "Only within the same subchannel", true).adv(8),
        adv_switch(9, "Geometric mask erase"),
        choice("a9.shape", "Shape", SHAPES, 1).adv(9),
        list(
            "a9.box",
            "Box (x0, y0, x1, y1 in ‰ of the selection)",
            0,
            1000,
            4,
            &[250, 250, 750, 750],
        )
        .adv(9),
        adv_switch(10, "Time / pitch contour erase"),
        curve(
            "a10.curve",
            "Contour (pitch ‰ over time)",
            Curve::line(0.3, 0.7),
        )
        .adv(10),
        choice(
            "a10.side",
            "Erase",
            &["Above the contour", "Below the contour"],
            0,
        )
        .adv(10),
        // V09-S39 step 1: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Delete").
        flag("n01", "Covered notes", false),
        choice("n01.same", "Same key means", DEL_SAME_KEY, 0),
        ticks("n01.tol", "Start tolerance (ticks)", 0, 1 << 40, 0),
        flag("n02", "Length range", false),
        ticks("n02.lo", "Shortest (ticks)", 0, 1 << 40, 0),
        ticks("n02.hi", "Longest (ticks)", 0, 1 << 40, 30),
        flag("n03", "Out of scale", false),
        choice("n03.key", "Key", &KEY_NAMES, 0),
        choice("n03.scale", "Scale", scale_names(), 0),
        choice("n03.passing", "Passing notes", DEL_PASSING, 0),
        flag("n04", "Metric mask", false),
        choice("n04.grid", "Grid", super::art_grid_values(), 3),
        list(
            "n04.mask",
            "Delete (1) / keep (0) at each grid position",
            0,
            1,
            64,
            &[0, 1],
        ),
        flag("n05", "Stack keep", false),
        int("n05.keep", "Keep", 1, 128, 2),
        choice("n05.which", "Keep the", DEL_STACK, 0),
        ticks("n05.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n06", "Hollow interior", false),
        choice("n06.keep", "Keep", DEL_HOLLOW, 0),
        int("n06.n", "N", 2, 32, 2),
        flag("n07", "Reference collision", false),
        choice("n07.ref", "Reference", DEL_REFERENCE, 0),
        int("n07.num", "Reference number", 0, 65535, 1),
        ticks("n07.tol", "Tolerance (ticks)", 0, 1 << 40, 10),
        choice("n07.match", "Match", DEL_MATCH, 0),
        flag("n08", "Crop", false),
        choice("n08.region", "Region", DEL_REGION, 0),
        choice("n08.crossing", "Crossing notes", DEL_CROSSING, 0),
        flag("n09", "Isolated notes", false),
        ticks("n09.radius", "Radius (ticks)", 0, 1 << 40, 480),
        int("n09.rows", "Rows", 0, 24, 2),
        flag("n10", "Clash removal", false),
        list("n10.intervals", "Intervals (semitones)", 1, 11, 11, &[1, 6]),
        choice("n10.which", "Delete the", DEL_CLASH, 0),
        ticks("n10.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n11", "Budget thinning", false),
        int("n11.most", "Most notes", 1, 1_000_000_000, 100_000),
        choice("n11.first", "Delete first", DEL_BUDGET, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, DELETE_HELP);
    ToolSpec {
        id: "delete",
        name: "Delete",
        family: Family::Interactive,
        summary: "Deletes the selected notes — or, with the filters, only those that match (all enabled filters must match). Preview shows what stays.",
        params: p,
        advanced: vec![
            "Pitch-range eraser",
            "Velocity-range eraser",
            "Subchannel eraser",
            "Density-aware thinning",
            "Alternating-pattern erase",
            "Probability erase",
            "Overlap-only erase",
            "Duplicate-note cleanup",
            "Geometric mask erase",
            "Time / pitch contour erase",
        ],
        // Orders = the catalog's (§235): every criterion is one more filter
        // of the same AND; n11 then chooses among the matching notes.
        functions: [
            ("n01", "Covered notes", 110),
            ("n02", "Length range", 120),
            ("n03", "Out of scale", 130),
            ("n04", "Metric mask", 140),
            ("n05", "Stack keep", 150),
            ("n06", "Hollow interior", 160),
            ("n07", "Reference collision", 170),
            ("n08", "Crop", 180),
            ("n09", "Isolated notes", 190),
            ("n10", "Clash removal", 200),
            ("n11", "Budget thinning", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

const DEL_SAME_KEY: &[&str] = &["Key", "Key and channel", "Key and subchannel"];
const DEL_PASSING: &[&str] = &["Delete them too", "Keep when shorter than 1/16"];
const DEL_STACK: &[&str] = &["Highest", "Lowest", "Outer"];
const DEL_HOLLOW: &[&str] = &[
    "Outline only",
    "Every Nth interior row",
    "Checkerboard interior",
];
const DEL_REFERENCE: &[&str] = &["Unselected notes of the track", "A subchannel", "A track"];
const DEL_MATCH: &[&str] = &["Same key", "Any key"];
const DEL_REGION: &[&str] = &["Time selection", "Loop", "Marker section"];
const DEL_CROSSING: &[&str] = &["Cut at the edge", "Keep", "Delete"];
const DEL_CLASH: &[&str] = &["Higher", "Lower", "Quieter"];
const DEL_BUDGET: &[&str] = &["Evenly", "Quietest", "Shortest"];

/// The delete / select / mute filters: which source notes match every enabled
/// criterion of `p` (keys `a1`… as in the Delete spec).
fn delete_matches(p: &Params, b: &BuildCtx, src: &Source) -> Vec<bool> {
    let n = src.notes.len();
    let mut m = vec![true; n];
    let seed = p.seed("seed");
    if p.adv(1) {
        let (lo, hi) = (p.int("a1.lo"), p.int("a1.hi"));
        for (i, x) in src.notes.iter().enumerate() {
            m[i] &= (lo.min(hi)..=lo.max(hi)).contains(&i64::from(x.pitch));
        }
    }
    if p.adv(2) {
        let (lo, hi) = (p.int("a2.lo"), p.int("a2.hi"));
        for (i, x) in src.notes.iter().enumerate() {
            m[i] &= (lo.min(hi)..=lo.max(hi)).contains(&i64::from(x.vel));
        }
    }
    if p.adv(3) {
        let subs = p.list("a3.subs");
        for (i, x) in src.notes.iter().enumerate() {
            m[i] &= subs.contains(&(sub_index(src, x) as i64));
        }
    }
    if p.adv(4) {
        let cell = note_value(p.choice("a4.cell"), 240, b.ctx);
        let keep = p.int("a4.keep") as usize;
        let (_, rank) = cells(src, cell, 1);
        for i in 0..n {
            m[i] &= rank[i] >= keep;
        }
    }
    if p.adv(5) {
        let pat = p.list("a5.pattern");
        if !pat.is_empty() {
            for (i, x) in m.iter_mut().enumerate() {
                *x &= pat[i % pat.len()] == 1;
            }
        }
    }
    if p.adv(6) {
        let prob = p.float("a6.prob") / 100.0;
        for (i, x) in src.notes.iter().enumerate() {
            m[i] &= Rng::for_note(seed, salt::KEEP, x).chance(prob);
        }
    }
    if p.adv(7) {
        let o = overlapped(src);
        for i in 0..n {
            m[i] &= o[i];
        }
    }
    if p.adv(8) {
        let d = duplicates(src, p.flag("a8.by_sub"));
        for i in 0..n {
            m[i] &= d[i];
        }
    }
    if p.adv(9) {
        let bx = p.list("a9.box");
        if bx.len() == 4 {
            let (x0, y0, x1, y1) = (
                bx[0].min(bx[2]) as f64 / 1000.0,
                bx[1].min(bx[3]) as f64 / 1000.0,
                bx[0].max(bx[2]) as f64 / 1000.0,
                bx[1].max(bx[3]) as f64 / 1000.0,
            );
            let ellipse = p.choice("a9.shape") == 1;
            for (i, x) in src.notes.iter().enumerate() {
                let (u, v) = norm_xy(x, &src.stats);
                let inside = if ellipse {
                    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                    let (rx, ry) = (((x1 - x0) / 2.0).max(1e-9), ((y1 - y0) / 2.0).max(1e-9));
                    ((u - cx) / rx).powi(2) + ((v - cy) / ry).powi(2) <= 1.0
                } else {
                    u >= x0 && u <= x1 && v >= y0 && v <= y1
                };
                m[i] &= inside;
            }
        }
    }
    if p.adv(10) {
        let c = p.curve("a10.curve");
        let above = p.choice("a10.side") == 0;
        for (i, x) in src.notes.iter().enumerate() {
            let (u, v) = norm_xy(x, &src.stats);
            let cv = c.eval(u);
            m[i] &= if above { v > cv } else { v < cv };
        }
    }
    m
}

fn delete_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let new = DelNew::from(p, b.ctx);
    // V09-S39: any new function, or Control Sweeps (evaluated per note
    // inside the kernel) — the v0.9 kernel; else the v0.8 one, untouched.
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        let k = Del9 {
            set: p.set.clone(),
            specs: p.specs.to_vec(),
            ctx: b.ctx.clone(),
            tracks: b.tracks.to_vec(),
            invert: p.flag("invert"),
            a6: p.adv(6),
            seed: p.seed("seed"),
            new,
            swept: Rebuild::of(p, b),
        };
        return boxed(Contextual {
            f: move |src: &Source, out: &mut Output| k.run(src, out),
            growth: Growth::Thin,
            estimate: None,
        });
    }
    let set = p.set.clone();
    let specs = p.specs.to_vec();
    let ctx = b.ctx.clone();
    let tracks = b.tracks.to_vec();
    let invert = p.flag("invert");
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let pp = Params::new(&specs, &set).with_keys(ctx.keys);
            let bc = BuildCtx {
                ctx: &ctx,
                stats: &src.stats,
                tracks: &tracks,
            };
            let m = delete_matches(&pp, &bc, src);
            for (n, del) in src.notes.iter().zip(m) {
                if del == invert {
                    out.push(*n);
                }
            }
            out.check()
        },
        growth: Growth::Thin,
        estimate: None,
    })
}

/// Reads Delete n07's references (V09-S39): the scoped tracks' unselected
/// notes starting within the tolerance of the selection's starts
/// (`Source::context` — also for a subchannel, whose selected notes are in
/// the source), or the reference track's notes there (`Source::aux`; its
/// selected notes are the selection's own when it is in scope). The
/// tolerance is the largest a Control Sweep can give it.
pub fn delete_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
) -> Result<(), TransformError> {
    if !p.flag("n07") || src.stats.is_empty() {
        return Ok(());
    }
    let tol = super::super::sweep::swept_ticks_max(p, "delete", "n07.tol");
    let a = src.stats.min_start.saturating_sub(tol);
    let b = src.stats.max_start.saturating_add(tol);
    if p.choice("n07.ref") == 2 {
        let num = p.int("n07.num");
        let index = usize::try_from(num - 1)
            .ok()
            .filter(|&i| doc.track(i).is_some());
        let Some(index) = index else {
            return Err(TransformError::Invalid(format!(
                "Track {num} does not exist. Choose the reference track by its number in the track list."
            )));
        };
        let track = doc.track(index).expect("checked");
        let scoped = src.tracks.iter().any(|t| t.index == index);
        let mut layer = Vec::new();
        track.notes.query(a, b, |_, v| {
            if v.start_tick >= a
                && v.start_tick <= b
                && !(scoped && v.selected)
                && layer.len() < limit
            {
                layer.push(TNote::from_record(&v.record(), u16::MAX));
            }
        });
        src.aux = std::sync::Arc::new(vec![layer]);
    } else if src.context.is_empty() {
        let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
        *src = s.with_context_between(doc, a, b);
    }
    Ok(())
}

/// Delete's settings of the functions declared by id (V09-S39 step 1) and
/// of a6's probability — per note under Control Sweeps ([`PerLevel`]).
#[derive(Clone, Debug)]
struct DelNew {
    /// n01: same key means (0 key, 1 + channel, 2 + subchannel), start
    /// tolerance.
    covered: Option<(usize, u64)>,
    /// n02: shortest, longest (either order).
    length: Option<(u64, u64)>,
    /// n03: the key's scale, and the length under which an out-of-scale
    /// note is kept as a passing note.
    scale: Option<(Lattice, Option<u64>)>,
    /// n04: grid ticks, delete (1) / keep (0) per grid position (cycled).
    metric: Option<(u64, Vec<i64>)>,
    /// n05: keep N, which (0 highest, 1 lowest, 2 outer), chord tolerance.
    stack: Option<(usize, usize, u64)>,
    /// n06: keep (0 outline only, 1 every Nth interior row, 2 checkerboard
    /// interior), N.
    hollow: Option<(usize, u64)>,
    /// n07: reference (0 unselected notes of the track, 1 a subchannel, 2 a
    /// track), its number, tolerance, any key.
    reference: Option<(usize, i64, u64, bool)>,
    /// n08: region (0 time selection, 1 loop, 2 marker section), crossing
    /// notes (0 cut, 1 keep, 2 delete).
    crop: Option<(usize, usize)>,
    /// n09: radius (ticks), rows.
    isolated: Option<(u64, u8)>,
    /// n10: the listed interval classes (bit d = d semitones mod 12), which
    /// note goes (0 higher, 1 lower, 2 quieter), chord tolerance.
    clash: Option<(u16, usize, u64)>,
    /// n11: most notes, delete first (0 evenly, 1 quietest, 2 shortest).
    budget: Option<(u64, usize)>,
    /// a6: the probability (0 – 1).
    prob: f64,
}

impl DelNew {
    fn from(p: &Params, ctx: &super::super::Ctx) -> DelNew {
        DelNew {
            covered: p
                .flag("n01")
                .then(|| (p.choice("n01.same"), p.ticks("n01.tol"))),
            length: p.flag("n02").then(|| {
                let (a, b) = (p.ticks("n02.lo"), p.ticks("n02.hi"));
                (a.min(b), a.max(b))
            }),
            scale: p.flag("n03").then(|| {
                (
                    Lattice::of_scale(&Scale::new(p.choice("n03.key"), p.choice("n03.scale"))),
                    (p.choice("n03.passing") == 1).then(|| (ctx.ppq / 4).max(1)),
                )
            }),
            metric: p.flag("n04").then(|| {
                let grid = p.choice("n04.grid").min(super::art_grid_values().len() - 1);
                (note_value(grid, 1, ctx).max(1), p.list("n04.mask"))
            }),
            stack: p.flag("n05").then(|| {
                (
                    p.int("n05.keep").max(1) as usize,
                    p.choice("n05.which"),
                    p.ticks("n05.tol"),
                )
            }),
            hollow: p
                .flag("n06")
                .then(|| (p.choice("n06.keep"), p.int("n06.n").max(2) as u64)),
            reference: p.flag("n07").then(|| {
                (
                    p.choice("n07.ref"),
                    p.int("n07.num"),
                    p.ticks("n07.tol"),
                    p.choice("n07.match") == 1,
                )
            }),
            crop: p
                .flag("n08")
                .then(|| (p.choice("n08.region"), p.choice("n08.crossing"))),
            isolated: p
                .flag("n09")
                .then(|| (p.ticks("n09.radius"), p.int("n09.rows").clamp(0, 24) as u8)),
            clash: p.flag("n10").then(|| {
                let mut mask = 0u16;
                for d in p.list("n10.intervals") {
                    if (1..=11).contains(&d) {
                        mask |= 1 << d;
                    }
                }
                (mask, p.choice("n10.which"), p.ticks("n10.tol"))
            }),
            budget: p
                .flag("n11")
                .then(|| (p.int("n11.most").max(1) as u64, p.choice("n11.first"))),
            prob: p.float("a6.prob") / 100.0,
        }
    }

    /// Whether any function declared by id is on.
    fn any(&self) -> bool {
        self.covered.is_some()
            || self.length.is_some()
            || self.scale.is_some()
            || self.metric.is_some()
            || self.stack.is_some()
            || self.hollow.is_some()
            || self.reference.is_some()
            || self.crop.is_some()
            || self.isolated.is_some()
            || self.clash.is_some()
            || self.budget.is_some()
    }
}

/// The v0.9 Delete (V09-S39 step 1): the v0.8 filters re-read from the same
/// parameters (`delete_matches`, untouched — shared with Select / Mute;
/// a6's probability per note), then each function declared by id is one
/// more criterion of the same AND (n01 … n10, each over the whole
/// selection's original notes), Invert flips the result, n11 chooses which
/// of the matching notes go, and n08's crossing notes are cut instead of
/// deleted.
#[derive(Clone)]
struct Del9 {
    set: ParamSet,
    specs: Vec<ParamSpec>,
    ctx: super::super::Ctx,
    tracks: Vec<super::super::SourceTrack>,
    invert: bool,
    /// a6 is on (its probability is per note, [`DelNew::prob`]).
    a6: bool,
    seed: u64,
    new: DelNew,
    swept: Option<std::sync::Arc<Rebuild>>,
}

/// Where a note lies against n08's region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Inside,
    Outside,
    Crossing,
}

fn side_of(n: &TNote, (a, b): (u64, u64)) -> Side {
    let e = n.end();
    if n.start >= a && n.start < b && e <= b {
        Side::Inside
    } else if e <= a || n.start >= b {
        Side::Outside
    } else {
        Side::Crossing
    }
}

/// A 256-bit row set (n10's kept pitches).
#[derive(Clone, Copy, Default)]
struct Rows256([u64; 4]);

impl Rows256 {
    fn set(&mut self, p: usize) {
        self.0[p >> 6] |= 1 << (p & 63);
    }

    /// `self >> k` (bit `i` of the result = bit `i + k`).
    fn shr(&self, k: usize) -> Rows256 {
        let mut o = [0u64; 4];
        let (w, b) = (k >> 6, k & 63);
        for (i, x) in o.iter_mut().enumerate() {
            let lo = self.0.get(i + w).copied().unwrap_or(0);
            let hi = self.0.get(i + w + 1).copied().unwrap_or(0);
            *x = if b == 0 {
                lo
            } else {
                (lo >> b) | (hi << (64 - b))
            };
        }
        Rows256(o)
    }

    fn meets(&self, o: &Rows256) -> bool {
        self.0.iter().zip(&o.0).any(|(a, b)| a & b != 0)
    }
}

/// Bottom-up range-maximum tree (n01's longer notes within the tolerance).
struct MaxTree {
    n: usize,
    t: Vec<u64>,
}

impl MaxTree {
    fn new(v: impl ExactSizeIterator<Item = u64>) -> MaxTree {
        let n = v.len().max(1);
        let mut t = vec![0u64; 2 * n];
        for (i, x) in v.enumerate() {
            t[n + i] = x;
        }
        for i in (1..n).rev() {
            t[i] = t[2 * i].max(t[2 * i + 1]);
        }
        MaxTree { n, t }
    }

    /// Largest value in `[a, b)` (0 when empty).
    fn max(&self, a: usize, b: usize) -> u64 {
        let (mut l, mut r, mut m) = (a + self.n, b.min(self.n) + self.n, 0);
        while l < r {
            if l & 1 == 1 {
                m = m.max(self.t[l]);
                l += 1;
            }
            if r & 1 == 1 {
                r -= 1;
                m = m.max(self.t[r]);
            }
            l >>= 1;
            r >>= 1;
        }
        m
    }
}

impl Del9 {
    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        // Each note's settings under Control Sweeps (E0422 / V09-0143).
        match &self.swept {
            Some(r) => {
                let lv: PerLevel<DelNew> = r.levels(&self.ctx, src, |p, b| DelNew::from(p, b.ctx));
                self.run_at(src, out, |i| lv.at(i), lv.swept())
            }
            None => self.run_at(src, out, |_| &self.new, false),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run_at<'a>(
        &'a self,
        src: &Source,
        out: &mut Output,
        me: impl Fn(usize) -> &'a DelNew,
        swept: bool,
    ) -> Result<(), TransformError> {
        let ns = &src.notes;
        let n = ns.len();
        let nw = &self.new;
        // The v0.8 filters (a6 per note below).
        let mut set = self.set.clone();
        set.set("a6", ParamValue::Bool(false));
        let pp = Params::new(&self.specs, &set).with_keys(self.ctx.keys);
        let bc = BuildCtx {
            ctx: &self.ctx,
            stats: &src.stats,
            tracks: &self.tracks,
        };
        let mut m = delete_matches(&pp, &bc, src);
        if self.a6 {
            for (i, x) in ns.iter().enumerate() {
                m[i] &= Rng::for_note(self.seed, salt::KEEP, x).chance(me(i).prob);
            }
        }
        // n01 Covered notes.
        if let Some((same, _)) = nw.covered {
            let c = covered(ns, same, |i| me(i).covered.map_or(0, |c| c.1));
            for i in 0..n {
                m[i] &= c[i];
            }
        }
        // n02 Length range.
        if nw.length.is_some() {
            for (i, x) in ns.iter().enumerate() {
                let (lo, hi) = me(i).length.unwrap_or((0, u64::MAX));
                m[i] &= (lo..=hi).contains(&x.dur);
            }
        }
        // n03 Out of scale.
        if let Some((lat, passing)) = nw.scale {
            for (i, x) in ns.iter().enumerate() {
                m[i] &= !lat.contains(i64::from(x.pitch)) && passing.is_none_or(|l| x.dur >= l);
            }
        }
        // n04 Metric mask: the grid slot holding the start, from its bar line.
        if let Some((grid, mask)) = &nw.metric {
            for (i, x) in ns.iter().enumerate() {
                m[i] &= !mask.is_empty() && {
                    let (bs, bl) = bar_bounds(&self.ctx.meter, x.start);
                    let slots = bl.div_ceil(*grid).max(1);
                    let slot = ((x.start - bs) / grid).min(slots - 1);
                    mask[(slot % mask.len() as u64) as usize] == 1
                };
            }
        }
        // n05 Stack keep: per track, chords by onset; the chord's first
        // note's settings.
        if let Some((_, _, tol)) = nw.stack {
            let mut go = vec![false; n];
            for idx in super::per_track(src) {
                let cg = ChordGroups::of(ns, &idx, tol);
                let (rank, size) = cg.voice_ranks(ns);
                for g in cg.iter() {
                    let (keep, which, _) = me(g[0]).stack.unwrap_or((usize::MAX, 0, 0));
                    for &i in g {
                        let (r, k) = (rank[i] as usize, size[i] as usize);
                        let kept = match which {
                            0 => r + keep >= k,
                            1 => r < keep,
                            _ => r < keep.div_ceil(2) || r + keep / 2 >= k,
                        };
                        go[i] = !kept;
                    }
                }
            }
            for i in 0..n {
                m[i] &= go[i];
            }
        }
        // n06 Hollow interior (cells of the snap step from the first start).
        if nw.hollow.is_some() && n > 0 {
            let t0 = src.stats.min_start;
            let cell = self.ctx.snap.max(1);
            let edge = super::super::spatial::cell_edges(ns, src.keys, t0, cell, false);
            let p0 = src.stats.min_pitch;
            for (i, x) in ns.iter().enumerate() {
                let (keep, k) = me(i).hollow.unwrap_or((0, 2));
                let row = u64::from(x.pitch.saturating_sub(p0));
                let col = (x.start - t0) / cell;
                let stays = edge[i]
                    || match keep {
                        0 => false,
                        1 => row.is_multiple_of(k),
                        _ => {
                            let q = (k / 2).max(1);
                            (col / q + row / q).is_multiple_of(2)
                        }
                    };
                m[i] &= !stays;
            }
        }
        // n07 Reference collision.
        if let Some((mode, num, _, any_key)) = nw.reference {
            let c = self.collisions(src, mode, num, any_key, |i| {
                me(i).reference.map_or(0, |r| r.2)
            });
            for i in 0..n {
                m[i] &= c[i];
            }
        }
        // n08 Crop.
        let mut cut: Option<(u64, u64)> = None;
        if let Some((region, crossing)) = nw.crop {
            match self.region(src, region) {
                Some(r) => {
                    let cuts = crossing == 0 && !self.invert;
                    for (i, x) in ns.iter().enumerate() {
                        m[i] &= match side_of(x, r) {
                            Side::Inside => false,
                            Side::Outside => true,
                            Side::Crossing => crossing != 1,
                        };
                    }
                    if cuts {
                        cut = Some(r);
                    }
                }
                None => {
                    m.iter_mut().for_each(|x| *x = false);
                    out.message = Some(
                        match region {
                            0 => "Crop: there is no time selection — select a time range on the ruler.",
                            1 => "Crop: there is no loop — set a loop range first.",
                            _ => "Crop: there are no markers — add markers to mark the sections.",
                        }
                        .into(),
                    );
                }
            }
        }
        // n09 Isolated notes.
        if nw.isolated.is_some() {
            let c = isolated(ns, src.keys.keys(), |i| me(i).isolated.unwrap_or((0, 0)));
            for i in 0..n {
                m[i] &= c[i];
            }
        }
        // n10 Clash removal.
        if let Some((_, which, tol)) = nw.clash {
            let mut go = vec![false; n];
            let mut order: Vec<usize> = Vec::new();
            for idx in super::per_track(src) {
                let audible: Vec<usize> = idx.into_iter().filter(|&i| !ns[i].muted()).collect();
                let cg = ChordGroups::of(ns, &audible, tol);
                for g in cg.iter() {
                    let mask = me(g[0]).clash.map_or(0, |c| c.0);
                    clash_chord(ns, g, mask, which, &mut order, &mut go);
                }
            }
            for i in 0..n {
                m[i] &= go[i];
            }
        }
        // Invert, then n11 among the matching notes.
        let mut del: Vec<bool> = m.iter().map(|&x| x != self.invert).collect();
        if nw.budget.is_some() {
            self.budget(
                ns,
                &mut del,
                cut,
                |i| me(i).budget.unwrap_or((u64::MAX, 0)),
                swept,
            );
        }
        for (i, x) in ns.iter().enumerate() {
            if !del[i] {
                out.push(*x);
            } else if let Some(r) = cut
                && side_of(x, r) == Side::Crossing
            {
                let s = x.start.max(r.0);
                let e = x.end().min(r.1);
                out.push(TNote {
                    start: s,
                    dur: e - s,
                    ..*x
                });
            }
        }
        out.check()
    }

    /// n08's region in the tools' ticks: the time selection, the loop, or
    /// the marker section holding the selection's first start (from the
    /// last marker at or before it — the song start without one — to the
    /// next marker).
    fn region(&self, src: &Source, region: usize) -> Option<(u64, u64)> {
        match region {
            0 => self.ctx.time_range,
            1 => self.ctx.loop_range,
            _ => {
                if self.ctx.markers.is_empty() {
                    return None;
                }
                let local = |t: u64| (i128::from(t) - self.ctx.shift).max(0) as u64;
                let s0 = src.stats.min_start;
                let mut lo = 0u64;
                let mut hi = u64::MAX;
                for &mk in self.ctx.markers.iter() {
                    let t = local(mk);
                    if t <= s0 {
                        lo = lo.max(t);
                    } else {
                        hi = hi.min(t);
                    }
                }
                Some((lo, hi))
            }
        }
    }

    /// n07: whether each note starts within its tolerance of a reference
    /// note (same key, or any key) — the unselected notes of its track
    /// (`Source::context`), the notes of the reference subchannel of its
    /// track (selected or not; the subchannel's own notes stay), or the
    /// reference track's notes (`Source::aux`, any track of the selection).
    fn collisions(
        &self,
        src: &Source,
        mode: usize,
        num: i64,
        any_key: bool,
        tol: impl Fn(usize) -> u64,
    ) -> Vec<bool> {
        let ns = &src.notes;
        // The reference subchannel of each slot (mode 1).
        let ref_sub = |slot: u16| -> Option<u32> {
            let k = usize::try_from(num - 1).ok()?;
            src.tracks.get(usize::from(slot))?.subs.get(k).map(|s| s.0)
        };
        let key = |slot: u16, x: &TNote| (slot, if any_key { 0 } else { x.pitch }, x.start);
        let mut refs: Vec<(u16, u8, u64)> = Vec::new();
        match mode {
            0 => refs.extend(src.context.iter().map(|x| key(x.track, x))),
            1 => {
                for x in src.context.iter().chain(ns.iter()) {
                    if ref_sub(x.track) == Some(x.sub) {
                        refs.push(key(x.track, x));
                    }
                }
            }
            _ => {
                if let Some(layer) = src.aux.first() {
                    refs.extend(layer.iter().map(|x| key(0, x)));
                }
            }
        }
        refs.sort_unstable();
        ns.iter()
            .enumerate()
            .map(|(i, x)| {
                if mode == 1 && ref_sub(x.track) == Some(x.sub) {
                    return false;
                }
                let (slot, p, t) = key(if mode == 2 { 0 } else { x.track }, x);
                let d = tol(i);
                let k = refs.partition_point(|r| *r < (slot, p, t.saturating_sub(d)));
                refs.get(k)
                    .is_some_and(|r| r.0 == slot && r.1 == p && r.2 <= t.saturating_add(d))
            })
            .collect()
    }

    /// n11: of the notes about to go, only as many go as leave at most N in
    /// the selection, chosen evenly in time, quietest or shortest first
    /// (ties in bit-reversed time order, so equal notes thin evenly). Under
    /// a sweep each distinct N keeps at most ⌊N × its notes / all notes⌋ of
    /// its notes. Notes n08 cuts are not counted as going.
    fn budget(
        &self,
        ns: &[TNote],
        del: &mut [bool],
        cut: Option<(u64, u64)>,
        cfg: impl Fn(usize) -> (u64, usize),
        swept: bool,
    ) {
        let total = ns.len() as u128;
        let mut classes: Vec<(u64, usize)> = (0..ns.len()).map(|i| (cfg(i).0, i)).collect();
        if swept {
            classes.sort_unstable();
        }
        let cuts = |i: usize| cut.is_some_and(|r| side_of(&ns[i], r) == Side::Crossing);
        let mut a = 0;
        while a < classes.len() {
            let v = classes[a].0;
            let mut b = a;
            while b < classes.len() && (!swept || classes[b].0 == v) {
                b += 1;
            }
            let members = (b - a) as u128;
            let keep = (u128::from(v) * members / total.max(1)).min(members);
            let cands: Vec<usize> = classes[a..b]
                .iter()
                .map(|c| c.1)
                .filter(|&i| del[i] && !cuts(i))
                .collect();
            let k = ((members - keep) as usize).min(cands.len());
            for &i in &cands {
                del[i] = false;
            }
            let first = cfg(cands.first().copied().unwrap_or(0)).1;
            let m = cands.len();
            match first {
                0 => {
                    for (j, &i) in cands.iter().enumerate() {
                        del[i] = (j + 1) * k / m.max(1) > j * k / m.max(1);
                    }
                }
                _ => {
                    let mut order: Vec<(u64, u64, usize)> = cands
                        .iter()
                        .enumerate()
                        .map(|(j, &i)| {
                            let x = &ns[i];
                            let w = if first == 1 { u64::from(x.vel) } else { x.dur };
                            (w, (j as u64).reverse_bits(), i)
                        })
                        .collect();
                    order.sort_unstable();
                    for o in &order[..k] {
                        del[o.2] = true;
                    }
                }
            }
            a = b;
        }
    }
}

/// n01: whether each note lies completely inside a longer **unmuted** note
/// of its key (with its MIDI channel / its subchannel) on its track — one
/// starting no later than `tol(i)` after it and ending at or after its end.
/// Per key, in start order: the latest end before its start, the latest end
/// among notes starting with it (strictly after its end) and the longest
/// note starting within the tolerance after it (a range-maximum tree).
fn covered(ns: &[TNote], same: usize, tol: impl Fn(usize) -> u64) -> Vec<bool> {
    let n = ns.len();
    let mut out = vec![false; n];
    // The key packed (track, pitch, channel or subchannel), sorted with the
    // start and the index — no per-comparison lookups.
    let key = |x: &TNote| -> u64 {
        let extra = match same {
            1 => u64::from(x.ch),
            2 => u64::from(x.sub),
            _ => 0,
        };
        u64::from(x.track) << 48 | u64::from(x.pitch) << 40 | extra
    };
    let mut keyed: Vec<(u64, u64, usize)> = ns
        .iter()
        .enumerate()
        .map(|(i, x)| (key(x), x.start, i))
        .collect();
    keyed.sort_unstable();
    let order: Vec<usize> = keyed.iter().map(|k| k.2).collect();
    let mut a = 0;
    while a < n {
        let k = keyed[a].0;
        let mut b = a;
        while b < n && keyed[b].0 == k {
            b += 1;
        }
        let g = &order[a..b];
        // The unmuted notes of the key: starts and lengths (start order).
        let cover: Vec<usize> = g.iter().copied().filter(|&i| !ns[i].muted()).collect();
        let starts: Vec<u64> = cover.iter().map(|&i| ns[i].start).collect();
        let tree = g
            .iter()
            .any(|&i| tol(i) > 0)
            .then(|| MaxTree::new(cover.iter().map(|&i| ns[i].dur)));
        // Latest end of the unmuted notes starting before the current
        // start, and among those starting with it.
        let mut before = None::<u64>;
        let mut c = 0;
        let mut s = 0;
        while s < g.len() {
            let t = ns[g[s]].start;
            let mut e = s;
            while e < g.len() && ns[g[e]].start == t {
                e += 1;
            }
            while c < cover.len() && starts[c] < t {
                let x = &ns[cover[c]];
                before = Some(before.map_or(x.end(), |b| b.max(x.end())));
                c += 1;
            }
            let mut c2 = c;
            let mut same_start = None::<u64>;
            while c2 < cover.len() && starts[c2] == t {
                let x = &ns[cover[c2]];
                same_start = Some(same_start.map_or(x.end(), |b| b.max(x.end())));
                c2 += 1;
            }
            for &i in &g[s..e] {
                let x = &ns[i];
                let d = tol(i);
                out[i] = before.is_some_and(|b| b >= x.end() && b > x.start)
                    || same_start.is_some_and(|b| b > x.end())
                    || (d > 0
                        && tree.as_ref().is_some_and(|tr| {
                            let hi = starts.partition_point(|&st| st <= t.saturating_add(d));
                            hi > c2 && tr.max(c2, hi) > x.dur
                        }));
            }
            s = e;
        }
        a = b;
    }
    out
}

/// n09: whether each note has **no** other selected note within its radius
/// (the gap between the spans, 0 when they meet) on the rows within its
/// rows. Per row in start order with the running latest end: on another
/// row, the notes starting by its end + radius reach it when the latest of
/// their ends is within the radius before its start; on its own row, the
/// notes before it the same way and the next one by its start —
/// O(n · (2 rows + 1) · log n).
fn isolated(ns: &[TNote], rows: usize, cfg: impl Fn(usize) -> (u64, u8)) -> Vec<bool> {
    let mut by_row: Vec<Vec<usize>> = vec![Vec::new(); rows.max(256)];
    for (i, x) in ns.iter().enumerate() {
        by_row[usize::from(x.pitch)].push(i);
    }
    let starts: Vec<Vec<u64>> = by_row
        .iter()
        .map(|r| r.iter().map(|&i| ns[i].start).collect())
        .collect();
    let latest: Vec<Vec<u64>> = by_row
        .iter()
        .map(|r| {
            let mut m = 0u64;
            r.iter()
                .map(|&i| {
                    m = m.max(ns[i].end());
                    m
                })
                .collect()
        })
        .collect();
    let mut place = vec![0usize; ns.len()];
    for r in &by_row {
        for (k, &i) in r.iter().enumerate() {
            place[i] = k;
        }
    }
    ns.iter()
        .enumerate()
        .map(|(i, x)| {
            let (rad, rr) = cfg(i);
            let reach = x.end().saturating_add(rad);
            let p = usize::from(x.pitch);
            let own = {
                let k = place[i];
                (k > 0 && latest[p][k - 1].saturating_add(rad) >= x.start)
                    || starts[p].get(k + 1).is_some_and(|&s| s <= reach)
            };
            if own {
                return false;
            }
            let lo = p.saturating_sub(usize::from(rr));
            let hi = (p + usize::from(rr)).min(by_row.len() - 1);
            !(lo..=hi).filter(|&q| q != p).any(|q| {
                let k = starts[q].partition_point(|&s| s <= reach);
                k > 0 && latest[q][k - 1].saturating_add(rad) >= x.start
            })
        })
        .collect()
}

/// n10 in one chord (`g`, audible notes of one track): the notes are kept
/// in turn — from the lowest (the higher of a pair goes), from the highest
/// (the lower goes) or from the loudest (the quieter goes; equal
/// velocities: the lower pitch first) — and a note goes when it forms a
/// listed interval class (semitones mod 12; never a unison or an octave)
/// with a note already kept, so no listed interval is left and the fewest
/// notes go.
fn clash_chord(
    ns: &[TNote],
    g: &[usize],
    mask: u16,
    which: usize,
    order: &mut Vec<usize>,
    go: &mut [bool],
) {
    if mask == 0 || g.len() < 2 {
        return;
    }
    // Distances d (1 … 255) whose class is listed, upward and as seen
    // from the top of the 256-row reversed set.
    let mut dist = Rows256::default();
    for d in 1..256usize {
        if mask & (1 << (d % 12)) != 0 {
            dist.set(d);
        }
    }
    order.clear();
    order.extend_from_slice(g);
    match which {
        0 => order.sort_by_key(|&i| (ns[i].pitch, i)),
        1 => order.sort_by_key(|&i| (std::cmp::Reverse(ns[i].pitch), i)),
        _ => order.sort_by_key(|&i| (std::cmp::Reverse(ns[i].vel), ns[i].pitch, i)),
    }
    // Kept pitches, and the same set mirrored (bit 255 − p) for the
    // intervals below a note.
    let (mut kept, mut mirror) = (Rows256::default(), Rows256::default());
    for &i in order.iter() {
        let p = usize::from(ns[i].pitch);
        let above = kept.shr(p).meets(&dist);
        let below = mirror.shr(255 - p).meets(&dist);
        if above || below {
            go[i] = true;
        } else {
            kept.set(p);
            mirror.set(255 - p);
        }
    }
}

// ================================================================ Select

pub const SELECT: ToolDef = ToolDef {
    id: "select",
    spec: select_spec,
    build: select_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SELECT_HELP: &[(&str, &str)] = &[
    (
        "among",
        "Which notes are searched: only the selected ones (narrowing the selection), or every note of the track.",
    ),
    ("a1", "Selects the notes inside a freehand lasso path."),
    (
        "a1.path",
        "The lasso's points as x, y pairs in thousandths of the notes' extent (x = time, y = pitch; 0 = start / lowest, 1000 = end / highest).",
    ),
    ("a2", "Selects the notes inside a polygon."),
    (
        "a2.poly",
        "The polygon's corners as x, y pairs in thousandths of the notes' extent.",
    ),
    (
        "a3",
        "Keeps only notes within a pitch range and a velocity range.",
    ),
    ("a3.lo", "The lowest key selected."),
    ("a3.hi", "The highest key selected."),
    ("a3.vlo", "The lowest velocity selected."),
    ("a3.vhi", "The highest velocity selected."),
    ("a4", "Keeps only notes on chosen subchannels."),
    (
        "a4.subs",
        "The subchannels selected, by place in the track's list (0 = the first).",
    ),
    ("a5", "Keeps only notes within a length range."),
    ("a5.min", "The shortest length selected, in ticks."),
    ("a5.max", "The longest length selected, in ticks."),
    (
        "a6",
        "Keeps only notes in crowded areas: cells holding at least a set number of notes.",
    ),
    (
        "a6.cell",
        "The size of one cell along time (a cell is a time step × 12 keys).",
    ),
    (
        "a6.min",
        "The fewest notes a cell must hold for its notes to be selected.",
    ),
    (
        "a7",
        "Selects every run of notes that repeats the opening motif (the same intervals and spacing as the first notes).",
    ),
    ("a7.len", "How many of the first notes make the motif."),
    (
        "a8",
        "Selects the connected shape that starts at the earliest, lowest note: notes touching in time and at most a semitone apart.",
    ),
    (
        "a8.gap",
        "Notes this close in time (ticks) still count as touching.",
    ),
    ("a9", "Selects every Nth note in time order."),
    ("a9.n", "Selects one note out of this many."),
    ("a9.offset", "The first note selected, counted from 0."),
    (
        "a10",
        "Selects the notes that do not match instead of those that do.",
    ),
    (
        "n01",
        "Grows the selection to nearby notes, or shrinks it away from its edges.",
    ),
    (
        "n01.mode",
        "Grow adds notes near the selection; Shrink removes selected notes near unselected ones.",
    ),
    ("n01.radius", "How far in time, in ticks, counts as near."),
    ("n01.rows", "How many keys above and below count as near."),
    ("n02", "Selects one voice of every chord."),
    (
        "n02.voice",
        "The voice selected: the top, the bottom, the inner voices, or the voice at a rank.",
    ),
    (
        "n02.rank",
        "For Rank: which voice from the bottom (1 = the lowest).",
    ),
    (
        "n02.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    ("n03", "Selects notes by their place in the bar."),
    ("n03.grid", "The grid the bar is divided into."),
    (
        "n03.mask",
        "Whether notes at each grid position are selected: 1 = select, 0 = not; the list repeats.",
    ),
    (
        "n03.tol",
        "A note within this many ticks of a grid position counts as on it.",
    ),
    (
        "n04",
        "Selects notes that move from the previous note by an interval within a range.",
    ),
    ("n04.lo", "The smallest interval selected, in semitones."),
    ("n04.hi", "The largest interval selected, in semitones."),
    (
        "n04.dir",
        "Count only upward moves, only downward moves, or both.",
    ),
    ("n05", "Selects overlapping notes."),
    (
        "n05.kind",
        "Same-key overlaps; notes completely covered by another of the same key; any overlap; or same-key notes overlapping across subchannels.",
    ),
    (
        "n05.same",
        "What counts as the same key: the key, or the key on the same channel.",
    ),
    (
        "n06",
        "Selects notes that stand out from their surroundings by pitch, velocity or length.",
    ),
    ("n06.prop", "The property compared."),
    (
        "n06.k",
        "How many standard deviations from the local mean a note must lie to be selected (smaller = more notes).",
    ),
    (
        "n06.window",
        "The time, in ticks, around each note that its surroundings are measured over.",
    ),
    (
        "n07",
        "Traces a line through the notes: from a start note, each next note within a step and a gap.",
    ),
    (
        "n07.start",
        "Where the line starts: the first selected note, the highest note at the start, or the lowest.",
    ),
    (
        "n07.step",
        "The largest pitch step the line may take, in semitones.",
    ),
    (
        "n07.gap",
        "The largest gap in time the line may jump, in ticks.",
    ),
    (
        "n08",
        "Selects the notes in a region of the song: a marker section, a chapter, or where the tempo is above or below a value.",
    ),
    ("n08.region", "The kind of region."),
    ("n08.bpm", "For the tempo regions: the tempo compared with."),
    ("n09", "Selects the chords of a chosen size."),
    ("n09.lo", "The smallest chord selected, in notes."),
    ("n09.hi", "The largest chord selected, in notes."),
    (
        "n09.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n10",
        "Combines the result with a stored selection, or stores it.",
    ),
    (
        "n10.op",
        "Union adds the stored notes; Subtract removes them; Intersect keeps the common notes; Exclusive Or keeps notes in one but not both; Store saves the result in the slot.",
    ),
    ("n10.slot", "The storage slot used (1 – 9)."),
    (
        "n11",
        "Selects the outline of a note shape, or its interior.",
    ),
    (
        "n11.select",
        "Outline keeps the notes on the shape's edge; Interior keeps the notes inside it.",
    ),
    (
        "n11.hood",
        "Which neighbors decide the edge: the 4 sides, or the 8 sides and corners.",
    ),
];

fn select_spec() -> ToolSpec {
    let mut p = vec![
        choice(
            "among",
            "Select among",
            &["The selected notes", "Every note of the track"],
            1,
        ),
        adv_switch(1, "Freehand (magic) lasso"),
        list(
            "a1.path",
            "Path (x, y in ‰ of the notes' extent)",
            0,
            1000,
            2048,
            &[100, 100, 500, 50, 900, 300, 800, 900, 300, 800],
        )
        .adv(1),
        adv_switch(2, "Polygon lasso"),
        list(
            "a2.poly",
            "Corners (x, y ‰)",
            0,
            1000,
            64,
            &[0, 0, 500, 0, 500, 1000, 0, 1000],
        )
        .adv(2),
        adv_switch(3, "Pitch / velocity filter"),
        pitch("a3.lo", "Pitch from", 60).adv(3),
        pitch("a3.hi", "Pitch to", 127).adv(3),
        int("a3.vlo", "Velocity from", 1, 127, 1).adv(3),
        int("a3.vhi", "Velocity to", 1, 127, 127).adv(3),
        adv_switch(4, "Subchannel filter"),
        list("a4.subs", "Subchannel positions", 0, 4095, 4096, &[0]).adv(4),
        adv_switch(5, "Duration filter"),
        ticks("a5.min", "Shortest", 0, 1 << 40, 0).adv(5),
        ticks("a5.max", "Longest", 1, 1 << 40, 480).adv(5),
        adv_switch(6, "Density region"),
        choice("a6.cell", "Cell", NOTE_VALUES, 3).adv(6),
        int("a6.min", "At least this many notes per cell", 1, 1000, 4).adv(6),
        adv_switch(7, "Pattern similarity"),
        int("a7.len", "Motif length (first notes)", 2, 16, 3).adv(7),
        adv_switch(8, "Connected note-art component"),
        ticks("a8.gap", "Touching within", 0, 1 << 30, 0).adv(8),
        adv_switch(9, "Every Nth"),
        int("a9.n", "Every", 2, 1000, 2).adv(9),
        int("a9.offset", "Starting at", 0, 999, 0).adv(9),
        adv_switch(10, "Invert the result"),
        // V09-S39 step 2: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Select").
        flag("n01", "Grow or shrink", false),
        choice("n01.mode", "Mode", &["Grow", "Shrink"], 0),
        ticks("n01.radius", "Radius (ticks)", 0, 1 << 40, 240),
        int("n01.rows", "Rows", 0, 24, 1),
        flag("n02", "Voice select", false),
        choice("n02.voice", "Voice", &["Top", "Bottom", "Inner", "Rank"], 0),
        int("n02.rank", "Rank (1 = the lowest)", 1, 128, 1),
        ticks("n02.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n03", "Metric select", false),
        choice("n03.grid", "Grid", super::art_grid_values(), 2),
        list(
            "n03.mask",
            "Select (1) / not (0) at each grid position",
            0,
            1,
            64,
            &[0, 1],
        ),
        ticks("n03.tol", "Tolerance (ticks)", 0, 1 << 40, 10),
        flag("n04", "Interval select", false),
        int("n04.lo", "Smallest interval", 0, 127, 5),
        int("n04.hi", "Largest interval", 0, 127, 127),
        choice("n04.dir", "Direction", &["Up", "Down", "Both"], 2),
        flag("n05", "Overlap select", false),
        choice("n05.kind", "Kind", SEL_OVERLAP, 0),
        choice("n05.same", "Same key means", &["Key", "Key and channel"], 0),
        flag("n06", "Outlier select", false),
        choice("n06.prop", "Property", &["Pitch", "Velocity", "Length"], 0),
        float("n06.k", "Deviations", 0.5, 6.0, 3.0, ""),
        ticks("n06.window", "Window (ticks)", 1, 1 << 40, 7680),
        flag("n07", "Line trace", false),
        choice("n07.start", "Start", SEL_LINE_START, 1),
        int("n07.step", "Largest step (semitones)", 0, 24, 4),
        ticks("n07.gap", "Largest gap (ticks)", 0, 1 << 40, 480),
        flag("n08", "Region select", false),
        choice("n08.region", "Region", SEL_REGION, 0),
        float("n08.bpm", "BPM", 1.0, 10000.0, 120.0, "BPM"),
        flag("n09", "Chord size select", false),
        int("n09.lo", "Smallest chord", 1, 1000, 5),
        int("n09.hi", "Largest chord", 1, 1000, 1000),
        ticks("n09.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        flag("n10", "Set operations", false),
        choice("n10.op", "Operation", SEL_SET_OPS, 4),
        int("n10.slot", "Slot", 1, 9, 1),
        flag("n11", "Outline select", false),
        choice("n11.select", "Select", &["Outline", "Interior"], 0),
        choice(
            "n11.hood",
            "Neighborhood",
            &["4 neighbors", "8 neighbors"],
            0,
        ),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, SELECT_HELP);
    ToolSpec {
        id: "select",
        name: "Select",
        family: Family::Interactive,
        summary: "Selects notes by lasso, property, subchannel, density, pattern, connected art or every Nth. Apply changes only the selection (no undo step).",
        params: p,
        advanced: vec![
            "Freehand (magic) lasso",
            "Polygon lasso",
            "Pitch / velocity filter",
            "Subchannel filter",
            "Duration filter",
            "Density region",
            "Pattern similarity",
            "Connected note-art component",
            "Every Nth",
            "Invert the result",
        ],
        // Orders = the catalog's (§235) = where each runs in `Sel9::run_at`.
        functions: [
            ("n01", "Grow or shrink", 110),
            ("n02", "Voice select", 120),
            ("n03", "Metric select", 130),
            ("n04", "Interval select", 140),
            ("n05", "Overlap select", 150),
            ("n06", "Outlier select", 160),
            ("n07", "Line trace", 170),
            ("n08", "Region select", 180),
            ("n09", "Chord size select", 190),
            ("n10", "Set operations", 200),
            ("n11", "Outline select", 210),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: false,
        context: false,
    }
}

const SEL_OVERLAP: &[&str] = &[
    "Same-key overlaps",
    "Covered",
    "Any overlap",
    "Across subchannels",
];
const SEL_LINE_START: &[&str] = &[
    "First selected note",
    "Highest at the start",
    "Lowest at the start",
];
const SEL_REGION: &[&str] = &["Marker section", "Chapter", "Tempo above", "Tempo below"];
const SEL_SET_OPS: &[&str] = &["Union", "Subtract", "Intersect", "Exclusive or", "Store"];

fn select_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let v8 = Sel8::from(p, b);
    let new = SelNew::from(p, b.ctx);
    // V09-S39: any new function, or Control Sweeps (evaluated per note
    // inside the kernel) — the v0.9 kernel; else the v0.8 one.
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Ok(Box::new(Sel9 {
            v8,
            new,
            order: p.sequence(crate::transform::order::SELECT_STEPS.members),
            ctx: b.ctx.clone(),
            swept: Rebuild::of(p, b),
        }));
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let m = v8.matches(src);
            for (x, keep) in src.notes.iter().zip(m) {
                if keep != v8.invert {
                    out.push(*x);
                }
            }
            out.check()
        },
        growth: Growth::Thin,
        estimate: None,
    })
}

/// The v0.8 Select rule (V09-S39 step 2 moved it here from the kernel's
/// closure unchanged — same fields, same arithmetic) shared by the v0.8
/// kernel and the v0.9 one ([`Sel9`]).
#[derive(Clone)]
struct Sel8 {
    path: Vec<(f64, f64)>,
    poly: Vec<(f64, f64)>,
    drawn: Option<Vec<(f64, f64)>>,
    pv: Option<(i64, i64, i64, i64)>,
    subs: Option<Vec<i64>>,
    dur: Option<(u64, u64)>,
    dens: Option<(u64, usize)>,
    motif: Option<usize>,
    comp: Option<u64>,
    nth: Option<(usize, usize)>,
    invert: bool,
}

impl Sel8 {
    fn from(p: &Params, b: &BuildCtx) -> Sel8 {
        // A lasso drawn in the Piano Roll (absolute ticks / rows) replaces the
        // typed ‰ path or corners.
        let drawn: Option<Vec<(f64, f64)>> = b.ctx.stroke.as_ref().and_then(|st| {
            (st.points.len() >= 3)
                .then(|| st.points.iter().map(|q| (q.tick as f64, q.pitch)).collect())
        });
        let path = if p.adv(1) && drawn.is_none() {
            polygon(&p.list("a1.path"))
        } else {
            Vec::new()
        };
        let poly = if p.adv(2) && drawn.is_none() {
            polygon(&p.list("a2.poly"))
        } else {
            Vec::new()
        };
        let drawn = drawn.filter(|_| p.adv(1) || p.adv(2));
        let pv = p.adv(3).then(|| {
            (
                p.int("a3.lo"),
                p.int("a3.hi"),
                p.int("a3.vlo"),
                p.int("a3.vhi"),
            )
        });
        let subs = if p.adv(4) {
            Some(p.list("a4.subs"))
        } else {
            None
        };
        let dur = p.adv(5).then(|| (p.ticks("a5.min"), p.ticks("a5.max")));
        let dens = p.adv(6).then(|| {
            (
                note_value(p.choice("a6.cell"), 240, b.ctx),
                p.int("a6.min") as usize,
            )
        });
        let motif = p.adv(7).then(|| p.int("a7.len") as usize);
        let comp = p.adv(8).then(|| p.ticks("a8.gap"));
        let nth = p
            .adv(9)
            .then(|| (p.int("a9.n") as usize, p.int("a9.offset") as usize));
        Sel8 {
            path,
            poly,
            drawn,
            pv,
            subs,
            dur,
            dens,
            motif,
            comp,
            nth,
            invert: p.adv(10),
        }
    }

    /// Whether each source note matches every enabled filter (before a10's
    /// inversion).
    #[allow(clippy::too_many_lines)]
    fn matches(&self, src: &Source) -> Vec<bool> {
        let n = src.notes.len();
        let mut m = vec![true; n];
        for (i, x) in src.notes.iter().enumerate() {
            let (u, v) = norm_xy(x, &src.stats);
            if !self.path.is_empty() {
                m[i] &= point_in_polygon(u, v, &self.path);
            }
            if !self.poly.is_empty() {
                m[i] &= point_in_polygon(u, v, &self.poly);
            }
            if let Some(d) = &self.drawn {
                // The note's middle against the drawn lasso.
                let mx = (x.start as f64 + x.end() as f64) / 2.0;
                m[i] &= point_in_polygon(mx, f64::from(x.pitch) + 0.5, d);
            }
            if let Some((lo, hi, vlo, vhi)) = self.pv {
                m[i] &= (lo.min(hi)..=lo.max(hi)).contains(&i64::from(x.pitch))
                    && (vlo.min(vhi)..=vlo.max(vhi)).contains(&i64::from(x.vel));
            }
            if let Some(s) = &self.subs {
                m[i] &= s.contains(&(sub_index(src, x) as i64));
            }
            if let Some((a, bb)) = self.dur {
                m[i] &= x.dur >= a.min(bb) && x.dur <= a.max(bb);
            }
        }
        if let Some((cell, min)) = self.dens {
            let (count, _) = cells(src, cell, 12);
            for i in 0..n {
                m[i] &= count[i] >= min;
            }
        }
        if let Some(k) = self.motif
            && n > k
        {
            // The first k notes' (interval, inter-onset) pattern; select
            // every run of k notes with the same pattern.
            let sig = |j: usize| -> Vec<(i64, u64)> {
                (j..j + k - 1)
                    .map(|q| {
                        (
                            i64::from(src.notes[q + 1].pitch) - i64::from(src.notes[q].pitch),
                            src.notes[q + 1].start - src.notes[q].start,
                        )
                    })
                    .collect()
            };
            let target = sig(0);
            let mut hit = vec![false; n];
            for j in 0..=n - k {
                if sig(j) == target {
                    for h in hit.iter_mut().skip(j).take(k) {
                        *h = true;
                    }
                }
            }
            for i in 0..n {
                m[i] &= hit[i];
            }
        }
        if let Some(gap) = self.comp
            && n > 0
        {
            // Flood fill from the earliest-lowest note over notes that touch
            // in time (within `gap`) and are at most a semitone apart.
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by_key(|&i| (src.notes[i].start, src.notes[i].pitch));
            let mut seen = vec![false; n];
            let mut stack = vec![order[0]];
            seen[order[0]] = true;
            // Bucket by pitch for neighbour search (one per key of the
            // project's keyboard).
            let mut by_pitch: Vec<Vec<usize>> = vec![Vec::new(); src.keys.keys()];
            for &i in &order {
                by_pitch[usize::from(src.notes[i].pitch)].push(i);
            }
            while let Some(i) = stack.pop() {
                let a = &src.notes[i];
                for dp in -1i32..=1 {
                    let p2 = i32::from(a.pitch) + dp;
                    let Some(bucket) = usize::try_from(p2).ok().and_then(|p| by_pitch.get(p))
                    else {
                        continue;
                    };
                    let lo = bucket
                        .partition_point(|&j| src.notes[j].end().saturating_add(gap) < a.start);
                    for &j in &bucket[lo..] {
                        let c = &src.notes[j];
                        if c.start > a.end().saturating_add(gap) {
                            break;
                        }
                        if !seen[j] && c.track == a.track {
                            seen[j] = true;
                            stack.push(j);
                        }
                    }
                }
            }
            for i in 0..n {
                m[i] &= seen[i];
            }
        }
        if let Some((every, off)) = self.nth {
            for (i, x) in m.iter_mut().enumerate() {
                *x &= i >= off && (i - off) % every == 0;
            }
        }
        m
    }
}

/// Select's settings of the functions declared by id (V09-S39 step 2) —
/// per note under Control Sweeps ([`PerLevel`]).
#[derive(Clone, Debug)]
struct SelNew {
    /// n01: mode (0 grow, 1 shrink), radius, rows.
    grow: Option<(usize, u64, u8)>,
    /// n02: voice (0 top, 1 bottom, 2 inner, 3 rank), rank (1 = lowest),
    /// chord tolerance.
    voice: Option<(usize, usize, u64)>,
    /// n03: grid ticks, positions (1 select / 0 not, cycled), tolerance.
    metric: Option<(u64, Vec<i64>, u64)>,
    /// n04: smallest, largest interval, direction (0 up, 1 down, 2 both).
    interval: Option<(u64, u64, usize)>,
    /// n05: kind (0 same-key overlaps, 1 covered, 2 any overlap, 3 across
    /// subchannels), same key with the MIDI channel.
    overlap: Option<(usize, bool)>,
    /// n06: property (0 pitch, 1 velocity, 2 length), deviations, window.
    outlier: Option<(usize, f64, u64)>,
    /// n07: start (0 first selected note, 1 highest at the start, 2 lowest
    /// at the start), largest step, largest gap.
    line: Option<(usize, u8, u64)>,
    /// n08: region (0 marker section, 1 chapter, 2 tempo above, 3 tempo
    /// below), BPM.
    region: Option<(usize, f64)>,
    /// n09: smallest, largest chord, chord tolerance.
    chord: Option<(usize, usize, u64)>,
    /// n10: operation (0 union, 1 subtract, 2 intersect, 3 exclusive or, 4
    /// store), slot (1 – 9).
    setop: Option<(usize, usize)>,
    /// n11: interior (else the outline), eight neighbours.
    outline: Option<(bool, bool)>,
}

impl SelNew {
    fn from(p: &Params, ctx: &super::super::Ctx) -> SelNew {
        SelNew {
            grow: p.flag("n01").then(|| {
                (
                    p.choice("n01.mode"),
                    p.ticks("n01.radius"),
                    p.int("n01.rows").clamp(0, 24) as u8,
                )
            }),
            voice: p.flag("n02").then(|| {
                (
                    p.choice("n02.voice"),
                    p.int("n02.rank").max(1) as usize,
                    p.ticks("n02.tol"),
                )
            }),
            metric: p.flag("n03").then(|| {
                let grid = p.choice("n03.grid").min(super::art_grid_values().len() - 1);
                (
                    note_value(grid, 1, ctx).max(1),
                    p.list("n03.mask"),
                    p.ticks("n03.tol"),
                )
            }),
            interval: p.flag("n04").then(|| {
                let (a, b) = (p.int("n04.lo").max(0) as u64, p.int("n04.hi").max(0) as u64);
                (a.min(b), a.max(b), p.choice("n04.dir"))
            }),
            overlap: p
                .flag("n05")
                .then(|| (p.choice("n05.kind"), p.choice("n05.same") == 1)),
            outlier: p.flag("n06").then(|| {
                (
                    p.choice("n06.prop"),
                    p.float("n06.k").max(0.0),
                    p.ticks("n06.window"),
                )
            }),
            line: p.flag("n07").then(|| {
                (
                    p.choice("n07.start"),
                    p.int("n07.step").clamp(0, 24) as u8,
                    p.ticks("n07.gap"),
                )
            }),
            region: p
                .flag("n08")
                .then(|| (p.choice("n08.region"), p.float("n08.bpm"))),
            chord: p.flag("n09").then(|| {
                let (a, b) = (
                    p.int("n09.lo").max(1) as usize,
                    p.int("n09.hi").max(1) as usize,
                );
                (a.min(b), a.max(b), p.ticks("n09.tol"))
            }),
            setop: p
                .flag("n10")
                .then(|| (p.choice("n10.op"), p.int("n10.slot").clamp(1, 9) as usize)),
            outline: p
                .flag("n11")
                .then(|| (p.choice("n11.select") == 1, p.choice("n11.hood") == 1)),
        }
    }

    fn any(&self) -> bool {
        self.grow.is_some()
            || self.voice.is_some()
            || self.metric.is_some()
            || self.interval.is_some()
            || self.overlap.is_some()
            || self.outlier.is_some()
            || self.line.is_some()
            || self.region.is_some()
            || self.chord.is_some()
            || self.setop.is_some()
            || self.outline.is_some()
    }

    /// n01 grows from the unselected notes around the selection.
    fn grows(&self) -> bool {
        self.grow.is_some_and(|g| g.0 == 0)
    }

    /// n10 adds the slot's notes (union / exclusive or).
    fn adds_slot(&self) -> bool {
        self.setop.is_some_and(|s| s.0 == 0 || s.0 == 3)
    }
}

/// Reads Select n01's candidates (V09-S39): growing a selection that is the
/// source (Select among the selected notes), the scoped tracks' unselected
/// notes sounding within the largest radius a sweep can give.
pub fn select_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
) -> Result<(), TransformError> {
    if !p.flag("n01") || p.choice("n01.mode") != 0 || src.stats.is_empty() {
        return Ok(());
    }
    if src.context.is_empty() {
        let r = super::super::sweep::swept_ticks_max(p, "select", "n01.radius");
        let a = src.stats.min_start.saturating_sub(r);
        let b = src.stats.max_end.saturating_add(r);
        let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
        *src = s.with_context_sounding(doc, a, b);
    }
    Ok(())
}

/// The v0.9 Select (V09-S39 step 2): the v0.8 result (`Sel8`, a10's
/// inversion included), then the functions declared by id in the catalog's
/// order over a pool of notes — the source, plus (n01 growing) the
/// unselected notes around it, plus (n10 adding) the slot's notes:
/// n01 grows / shrinks the result, n02 – n06, n08, n09 and n11 keep the
/// result's notes their predicate holds for (computed over the pool), n07
/// keeps the line traced through the result, n10 combines it with a stored
/// slot. The output is the notes to select.
struct Sel9 {
    v8: Sel8,
    new: SelNew,
    /// B2-F01: n01 – n09 in run order (`order::SELECT_STEPS`).
    order: Vec<&'static str>,
    ctx: super::super::Ctx,
    swept: Option<std::sync::Arc<Rebuild>>,
}

/// A slot note's identity: track id, start, length, pitch, velocity,
/// subchannel, channel.
type SlotKey = (u32, u64, u64, u8, u8, u32, u8);

impl Kernel for Sel9 {
    fn growth(&self) -> Growth {
        Growth::Thin
    }

    /// The selection can grow by the unselected notes read for n01 and the
    /// slot's notes for n10.
    fn estimate_source(&self, src: &Source) -> super::super::Estimate {
        let mut n = src.stats.count as u64;
        if self.new.grows() {
            n += src.context.len() as u64;
        }
        if let Some((_, slot)) = self.new.setop.filter(|_| self.new.adds_slot()) {
            n += self
                .ctx
                .selection_slots
                .get(slot - 1)
                .map_or(0, |v| v.len() as u64);
        }
        super::super::Estimate::for_notes(src.stats.count as u64, n)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let m8 = self.v8.matches(src);
        // The pool: the source, and (n01 growing) the unselected notes.
        let grow_ctx = self.new.grows() && !src.context.is_empty();
        let (mut pool, mut r): (Vec<TNote>, Vec<bool>) = if grow_ctx {
            let mut v: Vec<(TNote, bool)> = src
                .notes
                .iter()
                .zip(&m8)
                .map(|(x, &k)| (*x, k != self.v8.invert))
                .chain(src.context.iter().map(|x| (*x, false)))
                .collect();
            v.sort_unstable_by(|(a, _), (b, _)| {
                (
                    a.start, a.pitch, a.track, a.sub, a.dur, a.vel, a.ch, a.rel, a.group, a.flags,
                )
                    .cmp(&(
                        b.start, b.pitch, b.track, b.sub, b.dur, b.vel, b.ch, b.rel, b.group,
                        b.flags,
                    ))
            });
            v.into_iter().unzip()
        } else {
            (
                src.notes.clone(),
                m8.iter().map(|&k| k != self.v8.invert).collect(),
            )
        };
        let base = pool.len();
        // Each pool note's settings under Control Sweeps (E0422 / V09-0143).
        match &self.swept {
            Some(rb) if base > 0 => {
                let lv: PerLevel<SelNew> =
                    PerLevel::of_notes(&self.ctx, src, &pool, &rb.set, &rb.specs, |p| {
                        SelNew::from(p, &self.ctx)
                    });
                self.run_at(src, &mut pool, &mut r, base, out, |i| {
                    lv.at(i.min(base - 1))
                })
            }
            _ => self.run_at(src, &mut pool, &mut r, base, out, |_| &self.new),
        }
    }
}

/// Indices of `ns` per track slot (each in the pool's time order).
fn slots_of(ns: &[TNote]) -> Vec<Vec<usize>> {
    let mut v: Vec<Vec<usize>> = Vec::new();
    for (i, x) in ns.iter().enumerate() {
        let t = usize::from(x.track);
        if v.len() <= t {
            v.resize(t + 1, Vec::new());
        }
        v[t].push(i);
    }
    v
}

impl Sel9 {
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn run_at<'a>(
        &'a self,
        src: &Source,
        pool: &mut Vec<TNote>,
        r: &mut Vec<bool>,
        base: usize,
        out: &mut Output,
        me: impl Fn(usize) -> &'a SelNew,
    ) -> Result<(), TransformError> {
        let nw = &self.new;
        let ns: &[TNote] = &pool[..base];
        // B2-F01: n01 – n09 in run order (declared: by number) — grow /
        // shrink and the line trace read the result so far; the criteria
        // keep the notes they match.
        for id in &self.order {
            match *id {
                "n01" => {
                    // n01 Grow or shrink.
                    if let Some((mode, ..)) = nw.grow {
                        let cfg = |i: usize| me(i).grow.map_or((0, 0), |g| (g.1, g.2));
                        if mode == 0 {
                            sel_grow(&pool[..base], &mut r[..base], cfg);
                        } else {
                            sel_shrink(&pool[..base], &mut r[..base], src.keys, cfg);
                        }
                    }
                }
                "n02" => {
                    // n02 Voice select: per track, chords of the pool by onset.
                    if let Some((_, _, tol)) = nw.voice {
                        let mut keep = vec![false; base];
                        for idx in slots_of(ns) {
                            let cg = ChordGroups::of(ns, &idx, tol);
                            let (rank, size) = cg.voice_ranks(ns);
                            for g in cg.iter() {
                                let (voice, k, _) = me(g[0]).voice.unwrap_or((0, 1, 0));
                                for &i in g {
                                    let (rk, sz) = (rank[i] as usize, size[i] as usize);
                                    keep[i] = match voice {
                                        0 => rk + 1 == sz,
                                        1 => rk == 0,
                                        2 => rk > 0 && rk + 1 < sz,
                                        _ => rk + 1 == k,
                                    };
                                }
                            }
                        }
                        for i in 0..base {
                            r[i] &= keep[i];
                        }
                    }
                }
                "n03" => {
                    // n03 Metric select: the nearest grid line (restarting at each bar
                    // line; the next bar line is its slot 0) within the tolerance.
                    if let Some((grid, mask, _)) = &nw.metric {
                        for (i, x) in ns.iter().enumerate() {
                            let tol = me(i).metric.as_ref().map_or(0, |m| m.2);
                            r[i] &= !mask.is_empty() && {
                                let (bs, bl) = bar_bounds(&self.ctx.meter, x.start);
                                let k = (x.start - bs + grid / 2) / grid;
                                let (line, slot) = match bs.checked_add(k * grid) {
                                    Some(l) if l < bs.saturating_add(bl) => (l, k),
                                    _ => (bs.saturating_add(bl), 0),
                                };
                                x.start.abs_diff(line) <= tol
                                    && mask[(slot % mask.len() as u64) as usize] == 1
                            };
                        }
                    }
                }
                "n04" => {
                    // n04 Interval select: from the nearest pitch of the previous onset
                    // of the note's track (the lower on a tie).
                    if nw.interval.is_some() {
                        let mut keep = vec![false; base];
                        for idx in slots_of(ns) {
                            let mut prev: Vec<u8> = Vec::new();
                            let mut cur: Vec<u8> = Vec::new();
                            let mut a = 0;
                            while a < idx.len() {
                                let t = ns[idx[a]].start;
                                let mut b = a;
                                cur.clear();
                                while b < idx.len() && ns[idx[b]].start == t {
                                    cur.push(ns[idx[b]].pitch);
                                    b += 1;
                                }
                                if !prev.is_empty() {
                                    for &i in &idx[a..b] {
                                        let p = ns[i].pitch;
                                        let k = prev.partition_point(|&q| q < p);
                                        let q = match (
                                            k.checked_sub(1).map(|j| prev[j]),
                                            prev.get(k),
                                        ) {
                                            (Some(lo), Some(&hi)) => {
                                                if p - lo <= hi - p {
                                                    lo
                                                } else {
                                                    hi
                                                }
                                            }
                                            (Some(lo), None) => lo,
                                            (None, Some(&hi)) => hi,
                                            (None, None) => p,
                                        };
                                        let (lo, hi, dir) = me(i).interval.unwrap_or((0, 0, 2));
                                        let d = u64::from(p.abs_diff(q));
                                        keep[i] = (lo..=hi).contains(&d)
                                            && match dir {
                                                0 => p >= q,
                                                1 => p <= q,
                                                _ => true,
                                            };
                                    }
                                }
                                std::mem::swap(&mut prev, &mut cur);
                                prev.sort_unstable();
                                a = b;
                            }
                        }
                        for i in 0..base {
                            r[i] &= keep[i];
                        }
                    }
                }
                "n05" => {
                    // n05 Overlap select.
                    if let Some((kind, with_ch)) = nw.overlap {
                        let o = overlaps(ns, kind, with_ch);
                        for i in 0..base {
                            r[i] &= o[i];
                        }
                    }
                }
                "n06" => {
                    // n06 Outlier select.
                    if let Some((prop, ..)) = nw.outlier {
                        let o =
                            outliers(ns, prop, |i| me(i).outlier.map_or((0.0, 0), |o| (o.1, o.2)));
                        for i in 0..base {
                            r[i] &= o[i];
                        }
                    }
                }
                "n07" => {
                    // n07 Line trace through the result.
                    if let Some((start, ..)) = nw.line {
                        let line = trace_line(ns, &r[..base], start, |i| {
                            me(i).line.map_or((0, 0), |l| (l.1, l.2))
                        });
                        for i in 0..base {
                            r[i] &= line[i];
                        }
                    }
                }
                "n08" => {
                    // n08 Region select.
                    if let Some((region, _)) = nw.region {
                        match self.sel_region(region) {
                            Some(Some((a, b))) => {
                                for (i, x) in ns.iter().enumerate() {
                                    r[i] &= x.start >= a && x.start < b;
                                }
                            }
                            Some(None) => {
                                let tt =
                                    super::super::shared::tempo_time::TempoTime::new(&self.ctx);
                                for (i, x) in ns.iter().enumerate() {
                                    let bpm = me(i).region.map_or(0.0, |g| g.1);
                                    let at = tt.bpm_at(x.start);
                                    r[i] &= if region == 2 { at > bpm } else { at < bpm };
                                }
                            }
                            None => {
                                r[..base].iter_mut().for_each(|x| *x = false);
                                out.message = Some(
                                if region == 0 {
                                    "Region select: there are no markers — add markers to mark the sections."
                                } else {
                                    "Region select: the cursor is in no chapter."
                                }
                                .into(),
                            );
                            }
                        }
                    }
                }
                "n09" => {
                    // n09 Chord size select.
                    if let Some((_, _, tol)) = nw.chord {
                        let mut keep = vec![false; base];
                        for idx in slots_of(ns) {
                            let cg = ChordGroups::of(ns, &idx, tol);
                            for g in cg.iter() {
                                let (lo, hi, _) = me(g[0]).chord.unwrap_or((1, usize::MAX, 0));
                                let ok = (lo..=hi).contains(&g.len());
                                for &i in g {
                                    keep[i] = ok;
                                }
                            }
                        }
                        for i in 0..base {
                            r[i] &= keep[i];
                        }
                    }
                }
                _ => {}
            }
        }
        // n10 Set operations with a stored slot.
        if let Some((op, slot)) = nw.setop
            && op < 4
        {
            let stored: &[super::super::SlotNote] = self
                .ctx
                .selection_slots
                .get(slot - 1)
                .map_or(&[], |v| v.as_slice());
            let key_of = |x: &TNote| -> Option<SlotKey> {
                let t = src.tracks.get(usize::from(x.track))?;
                Some((t.id.0, x.start, x.dur, x.pitch, x.vel, x.sub, x.ch))
            };
            let mut keys: Vec<(SlotKey, usize)> = pool[..base]
                .iter()
                .enumerate()
                .filter_map(|(i, x)| key_of(x).map(|k| (k, i)))
                .collect();
            keys.sort_unstable();
            let mut in_slot = vec![false; base];
            for s in stored {
                let k: SlotKey = (s.track.0, s.start, s.dur, s.pitch, s.vel, s.sub, s.ch);
                let a = keys.partition_point(|e| e.0 < k);
                let mut hit = false;
                for e in keys[a..].iter().take_while(|e| e.0 == k) {
                    in_slot[e.1] = true;
                    hit = true;
                }
                if !hit
                    && (op == 0 || op == 3)
                    && let Some(t) = src.tracks.iter().position(|t| t.id == s.track)
                {
                    pool.push(TNote {
                        start: s.start,
                        dur: s.dur,
                        pitch: s.pitch,
                        vel: s.vel,
                        sub: s.sub,
                        ch: s.ch,
                        rel: 64,
                        track: t as u16,
                        ..TNote::default()
                    });
                    r.push(true);
                }
            }
            for i in 0..base {
                r[i] = match op {
                    0 => r[i] || in_slot[i],
                    1 => r[i] && !in_slot[i],
                    2 => r[i] && in_slot[i],
                    _ => r[i] != in_slot[i],
                };
            }
            if stored.is_empty() {
                out.message = Some(format!(
                    "Set operations: slot {slot} is empty — store a selection in it first."
                ));
            }
        }
        // n11 Outline select (over the pool and the slot's notes).
        if let Some((interior, eight)) = nw.outline
            && !pool.is_empty()
        {
            let t0 = pool.iter().map(|x| x.start).min().unwrap_or(0);
            let edge =
                super::super::spatial::cell_edges(pool, src.keys, t0, self.ctx.snap.max(1), eight);
            for (x, e) in r.iter_mut().zip(edge) {
                *x &= e != interior;
            }
        }
        for (x, &keep) in pool.iter().zip(r.iter()) {
            if keep {
                out.push(*x);
            }
        }
        out.check()
    }

    /// n08's region in the tools' ticks: the marker section holding the
    /// cursor (from the last marker at or before it — the song start without
    /// one — to the next), or the chapter holding it; `Some(None)` for the
    /// tempo modes; `None` when there is no such region.
    fn sel_region(&self, region: usize) -> Option<Option<(u64, u64)>> {
        let local = |t: u64| (i128::from(t) - self.ctx.shift).max(0) as u64;
        let c = self.ctx.cursor;
        match region {
            0 => {
                if self.ctx.markers.is_empty() {
                    return None;
                }
                let (mut lo, mut hi) = (0u64, u64::MAX);
                for &mk in self.ctx.markers.iter() {
                    let t = local(mk);
                    if t <= c {
                        lo = lo.max(t);
                    } else {
                        hi = hi.min(t);
                    }
                }
                Some(Some((lo, hi)))
            }
            1 => self
                .ctx
                .chapters
                .iter()
                .map(|&(a, b)| (local(a), local(b)))
                .find(|&(a, b)| a <= c && c < b)
                .map(Some),
            _ => Some(None),
        }
    }
}

/// n01 grow: every pool note outside the result within the radius (the gap
/// between the spans) and the rows of a result note — each result note with
/// its own radius and rows. Per rows value, per row: the result notes by
/// start − radius with the running latest end + radius, one binary search
/// per candidate and row.
fn sel_grow(ns: &[TNote], r: &mut [bool], cfg: impl Fn(usize) -> (u64, u8)) {
    let mut classes: std::collections::BTreeMap<u8, Vec<usize>> = Default::default();
    for (i, &x) in r.iter().enumerate() {
        if x {
            classes.entry(cfg(i).1).or_default().push(i);
        }
    }
    let mut add = vec![false; ns.len()];
    for (rows, members) in classes {
        // Per row: (start − radius, end + radius), by the first.
        let mut by_row: Vec<Vec<(u64, u64)>> = vec![Vec::new(); 256];
        for &i in &members {
            let (rad, _) = cfg(i);
            let x = &ns[i];
            by_row[usize::from(x.pitch)]
                .push((x.start.saturating_sub(rad), x.end().saturating_add(rad)));
        }
        let mut latest: Vec<Vec<u64>> = Vec::with_capacity(256);
        for row in &mut by_row {
            row.sort_unstable();
            let mut m = 0u64;
            latest.push(
                row.iter()
                    .map(|e| {
                        m = m.max(e.1);
                        m
                    })
                    .collect(),
            );
        }
        for (i, p) in ns.iter().enumerate() {
            if r[i] || add[i] {
                continue;
            }
            let lo = usize::from(p.pitch).saturating_sub(usize::from(rows));
            let hi = (usize::from(p.pitch) + usize::from(rows)).min(255);
            add[i] = (lo..=hi).any(|q| {
                let k = by_row[q].partition_point(|e| e.0 <= p.end());
                k > 0 && latest[q][k - 1] >= p.start
            });
        }
    }
    for (x, a) in r.iter_mut().zip(add) {
        *x |= a;
    }
}

/// n01 shrink: a result note stays when the result's notes (merged row
/// intervals) cover its span widened by its radius on every row within its
/// rows (off the keyboard and before the song start: never) —
/// morphological erosion; its boundary notes go.
fn sel_shrink(
    ns: &[TNote],
    r: &mut [bool],
    keys: crate::model::project::KeyRange,
    cfg: impl Fn(usize) -> (u64, u8),
) {
    let m = super::super::spatial::RowMask::from_notes(
        keys,
        ns.iter().zip(r.iter()).filter(|e| *e.1).map(|e| e.0),
    );
    let rows = keys.keys() as i64;
    let go: Vec<bool> = ns
        .iter()
        .enumerate()
        .map(|(i, x)| {
            if !r[i] {
                return false;
            }
            let (rad, rr) = cfg(i);
            let (a, b) = (x.start.saturating_sub(rad), x.end().saturating_add(rad));
            if a >= b {
                return false;
            }
            // Before the song start nothing is selected.
            if x.start < rad {
                return true;
            }
            let p = i64::from(x.pitch);
            !(p - i64::from(rr)..=p + i64::from(rr)).all(|q| {
                (0..rows).contains(&q) && {
                    let run = m.row(q as u8);
                    let k = run.partition_point(|e| e.1 <= a);
                    run.get(k).is_some_and(|e| e.0 <= a && e.1 >= b)
                }
            })
        })
        .collect();
    for (x, g) in r.iter_mut().zip(go) {
        *x &= !g;
    }
}

/// n05: whether each note overlaps another — of its key on its track (with
/// the MIDI channel), is covered by a longer one of its key (Delete's n01
/// rule), overlaps any note of its track, or overlaps a note of its key on
/// another subchannel. Zero-length notes overlap nothing. Per key in start
/// order: the latest end before it (and, across subchannels, the latest end
/// of another subchannel than that one's), the next note (of another
/// subchannel) — O(n log n).
fn overlaps(ns: &[TNote], kind: usize, with_ch: bool) -> Vec<bool> {
    let n = ns.len();
    if kind == 1 {
        return covered(ns, usize::from(with_ch), |_| 0);
    }
    let key = |x: &TNote| -> u64 {
        let p = if kind == 2 { 0 } else { u64::from(x.pitch) + 1 };
        let c = if with_ch && kind != 2 {
            u64::from(x.ch) + 1
        } else {
            0
        };
        u64::from(x.track) << 24 | p << 8 | c
    };
    let mut keyed: Vec<(u64, u64, usize)> = ns
        .iter()
        .enumerate()
        .filter(|(_, x)| x.dur > 0)
        .map(|(i, x)| (key(x), x.start, i))
        .collect();
    keyed.sort_unstable();
    let mut out = vec![false; n];
    let mut a = 0;
    while a < keyed.len() {
        let k = keyed[a].0;
        let mut b = a;
        while b < keyed.len() && keyed[b].0 == k {
            b += 1;
        }
        let g: Vec<usize> = keyed[a..b].iter().map(|e| e.2).collect();
        // The next note of another subchannel after each (in group order).
        let mut next_diff = vec![usize::MAX; g.len()];
        for j in (0..g.len().saturating_sub(1)).rev() {
            next_diff[j] = if ns[g[j + 1]].sub != ns[g[j]].sub {
                j + 1
            } else {
                next_diff[j + 1]
            };
        }
        // The latest end so far, with its subchannel, and the latest end of
        // any other subchannel than that one.
        let mut best: Option<(u64, u32)> = None;
        let mut other: Option<u64> = None;
        for (j, &i) in g.iter().enumerate() {
            let x = &ns[i];
            let before = if kind == 3 {
                match best {
                    Some((e, s)) if s != x.sub => Some(e),
                    _ => other,
                }
            } else {
                best.map(|b| b.0)
            };
            let after = if kind == 3 {
                next_diff.get(j).copied().filter(|&d| d != usize::MAX)
            } else {
                (j + 1 < g.len()).then_some(j + 1)
            };
            out[i] = before.is_some_and(|e| e > x.start)
                || after.is_some_and(|d| ns[g[d]].start < x.end());
            // Update (latest end, its subchannel) and the other subchannels'
            // latest.
            let e = x.end();
            match best {
                Some((be, bs)) if be >= e => {
                    if bs != x.sub {
                        other = Some(other.map_or(e, |o| o.max(e)));
                    }
                }
                Some((be, bs)) => {
                    if bs != x.sub {
                        other = Some(other.map_or(be, |o| o.max(be)));
                    }
                    best = Some((e, x.sub));
                }
                None => best = Some((e, x.sub)),
            }
        }
        a = b;
    }
    out
}

/// A wavelet matrix over small integers (ranks of the values): the k-th
/// smallest value in any index range in O(log σ) (n06's windows).
struct Wavelet {
    /// Per level (high bit first): the bits, rank-1 counts per 64-bit word,
    /// and the number of zeros.
    levels: Vec<(Vec<u64>, Vec<u32>, usize)>,
}

impl Wavelet {
    fn new(mut v: Vec<u32>) -> Wavelet {
        let max = v.iter().copied().max().unwrap_or(0);
        let bits = (32 - max.leading_zeros()).max(1) as usize;
        let n = v.len();
        let mut levels = Vec::with_capacity(bits);
        for lv in (0..bits).rev() {
            let mut words = vec![0u64; n / 64 + 1];
            for (i, &x) in v.iter().enumerate() {
                if (x >> lv) & 1 == 1 {
                    words[i / 64] |= 1 << (i % 64);
                }
            }
            let mut cum = Vec::with_capacity(words.len() + 1);
            let mut c = 0u32;
            for w in &words {
                cum.push(c);
                c += w.count_ones();
            }
            let zeros = n - c as usize;
            let (z, o): (Vec<u32>, Vec<u32>) = v.iter().partition(|&&x| (x >> lv) & 1 == 0);
            v = z;
            v.extend(o);
            levels.push((words, cum, zeros));
        }
        Wavelet { levels }
    }

    /// Ones in `[0, i)` of a level.
    fn rank1((words, cum, _): &(Vec<u64>, Vec<u32>, usize), i: usize) -> usize {
        let w = i / 64;
        let b = i % 64;
        cum[w] as usize
            + if b == 0 {
                0
            } else {
                (words[w] & ((1u64 << b) - 1)).count_ones() as usize
            }
    }

    /// The `k`-th smallest (0-based) value in `[l, r)`.
    fn kth(&self, mut l: usize, mut r: usize, mut k: usize) -> u32 {
        let mut v = 0u32;
        for lv in &self.levels {
            let (l1, r1) = (Self::rank1(lv, l), Self::rank1(lv, r));
            let zeros_in = (r - l) - (r1 - l1);
            v <<= 1;
            if k < zeros_in {
                l -= l1;
                r -= r1;
            } else {
                k -= zeros_in;
                v |= 1;
                l = lv.2 + l1;
                r = lv.2 + r1;
            }
        }
        v
    }
}

/// n06: whether each note's pitch, velocity or length lies beyond k robust
/// deviations from the median of its track's notes starting within half
/// the window before or after it (itself included; at least four notes):
/// |x − median| > k · IQR / 1.349 and > 0 — quartiles by nearest rank, from
/// a wavelet matrix over each track's values in start order (any window in
/// O(log n)).
fn outliers(ns: &[TNote], prop: usize, cfg: impl Fn(usize) -> (f64, u64)) -> Vec<bool> {
    let mut out = vec![false; ns.len()];
    let value = |x: &TNote| -> u64 {
        match prop {
            0 => u64::from(x.pitch),
            1 => u64::from(x.vel),
            _ => x.dur,
        }
    };
    for idx in slots_of(ns) {
        if idx.len() < 4 {
            continue;
        }
        let mut sorted: Vec<u64> = idx.iter().map(|&i| value(&ns[i])).collect();
        sorted.sort_unstable();
        sorted.dedup();
        let ranks: Vec<u32> = idx
            .iter()
            .map(|&i| sorted.partition_point(|&s| s < value(&ns[i])) as u32)
            .collect();
        let wm = Wavelet::new(ranks.clone());
        let starts: Vec<u64> = idx.iter().map(|&i| ns[i].start).collect();
        for (j, &i) in idx.iter().enumerate() {
            let (k, w) = cfg(i);
            let half = w / 2;
            let t = starts[j];
            let l = starts.partition_point(|&s| s < t.saturating_sub(half));
            let r = starts.partition_point(|&s| s <= t.saturating_add(half));
            let m = r - l;
            if m < 4 {
                continue;
            }
            let q = |num: usize| sorted[wm.kth(l, r, num * (m - 1) / 4) as usize] as f64;
            let (q1, med, q3) = (q(1), q(2), q(3));
            let dev = (sorted[ranks[j] as usize] as f64 - med).abs();
            out[i] = dev > 0.0 && dev > k * (q3 - q1) / 1.349;
        }
    }
    out
}

/// n07: the melodic line from the start note through the result's notes of
/// its track: from each note, the earliest later onset within the largest
/// gap after its end and the largest step of its pitch (the nearest pitch
/// there; ties the lower) — per pitch the result's starts, one binary
/// search per pitch of the step's range and step of the line.
fn trace_line(
    ns: &[TNote],
    r: &[bool],
    start: usize,
    cfg: impl Fn(usize) -> (u8, u64),
) -> Vec<bool> {
    let mut on = vec![false; ns.len()];
    let first = r.iter().position(|&x| x);
    let Some(f) = first else {
        return on;
    };
    let t0 = ns[f].start;
    let at_start = (f..ns.len())
        .take_while(|&i| ns[i].start == t0)
        .filter(|&i| r[i]);
    let s = match start {
        // The loudest note of the first onset (ties: the lower).
        0 => at_start.max_by_key(|&i| {
            (
                ns[i].vel,
                std::cmp::Reverse(ns[i].pitch),
                std::cmp::Reverse(i),
            )
        }),
        1 => at_start.max_by_key(|&i| (ns[i].pitch, std::cmp::Reverse(i))),
        _ => at_start.min_by_key(|&i| (ns[i].pitch, i)),
    }
    .unwrap_or(f);
    let track = ns[s].track;
    // The result's notes of the track per pitch, by start.
    let mut by_pitch: Vec<Vec<usize>> = vec![Vec::new(); 256];
    for (i, x) in ns.iter().enumerate() {
        if r[i] && x.track == track {
            by_pitch[usize::from(x.pitch)].push(i);
        }
    }
    let mut c = s;
    loop {
        on[c] = true;
        let x = &ns[c];
        let (step, gap) = cfg(c);
        let reach = x.end().saturating_add(gap);
        let lo = usize::from(x.pitch).saturating_sub(usize::from(step));
        let hi = (usize::from(x.pitch) + usize::from(step)).min(255);
        let mut best: Option<(u64, u8, u8, usize)> = None;
        for (q, b) in by_pitch.iter().enumerate().take(hi + 1).skip(lo) {
            let k = b.partition_point(|&j| ns[j].start <= x.start);
            if let Some(&j) = b.get(k)
                && ns[j].start <= reach
            {
                let cand = (ns[j].start, (q as u8).abs_diff(x.pitch), q as u8, j);
                if best.is_none_or(|bb| cand < bb) {
                    best = Some(cand);
                }
            }
        }
        match best {
            Some((.., j)) => c = j,
            None => break,
        }
    }
    on
}

// ================================================================ Slice

pub const SLICE: ToolDef = ToolDef {
    id: "slice",
    spec: slice_spec,
    build: slice_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SLICE_HELP: &[(&str, &str)] = &[
    (
        "grid",
        "The grid the notes are cut at: every note crossing a grid line is cut there (Snap uses the Piano Roll's snap).",
    ),
    (
        "grid.ticks",
        "The grid length in ticks, used when Slice at Every is set to Custom Ticks.",
    ),
    ("a1", "Also cuts at a list of exact ticks."),
    (
        "a1.ticks",
        "The ticks where every note crossing them is cut.",
    ),
    (
        "a2",
        "Also cuts at a second, evenly spaced grid with its own spacing and offset.",
    ),
    (
        "a2.every",
        "How far apart the extra cut lines are, in ticks.",
    ),
    ("a2.offset", "Where the extra cut lines start, in ticks."),
    (
        "a3",
        "Also cuts along rays fanning out from a point, like slicing a pie.",
    ),
    (
        "a3.centre",
        "The rays' center: x and y in thousandths of the selection's box.",
    ),
    ("a3.rays", "How many rays cut through the selection."),
    (
        "a4",
        "Also cuts each note once along a wave that moves with its pitch.",
    ),
    (
        "a4.amp",
        "How far the wave swings from each note's middle, in thousandths of the selection's span.",
    ),
    ("a4.period", "How many semitones one wave spans."),
    ("a5", "Also cuts each note at random places (seeded)."),
    ("a5.cuts", "How many random cuts each note gets."),
    (
        "a6",
        "Also cuts notes into equal pieces, more pieces the higher the note.",
    ),
    ("a6.low", "How many equal cuts a note at pitch 0 gets."),
    ("a6.high", "How many equal cuts a note on the top key gets."),
    (
        "a7",
        "Also cuts notes into equal pieces, a number set per subchannel.",
    ),
    (
        "a7.cuts",
        "How many equal cuts the notes of each subchannel position get (repeated).",
    ),
    (
        "a8",
        "Puts the pieces of each note in a note group of their own, so they select together.",
    ),
    (
        "a9",
        "Moves every other piece of a note to another subchannel (color).",
    ),
    (
        "a9.step",
        "How many subchannel positions the alternate pieces move.",
    ),
    ("a10", "Halves every piece again, once per level."),
    ("a10.depth", "How many times every piece is halved."),
];

fn slice_spec() -> ToolSpec {
    let mut p = vec![
        choice("grid", "Slice at every", NOTE_VALUES, 3),
        ticks("grid.ticks", "Custom", 1, 1 << 40, 480),
        seed("seed"),
        adv_switch(1, "Multi-line slice"),
        list(
            "a1.ticks",
            "Slice at ticks",
            0,
            i64::MAX >> 2,
            256,
            &[240, 720],
        )
        .adv(1),
        adv_switch(2, "Parallel slice grid"),
        ticks("a2.every", "Every", 1, 1 << 40, 160).adv(2),
        ticks("a2.offset", "Offset", 0, 1 << 40, 80).adv(2),
        adv_switch(3, "Radial slice"),
        list("a3.centre", "Center (x ‰, y ‰)", 0, 1000, 2, &[500, 500]).adv(3),
        int("a3.rays", "Rays", 1, 64, 6).adv(3),
        adv_switch(4, "Wave slice"),
        float("a4.amp", "Amplitude (‰ of the span)", 0.0, 500.0, 100.0, "").adv(4),
        float("a4.period", "Period (semitones)", 1.0, 48.0, 12.0, "").adv(4),
        adv_switch(5, "Random slice field"),
        int("a5.cuts", "Cuts per note", 1, 32, 2).adv(5),
        adv_switch(6, "Pitch-dependent slice"),
        int("a6.low", "Cuts at pitch 0", 0, 32, 0).adv(6),
        int("a6.high", "Cuts at the top key", 0, 32, 8).adv(6),
        adv_switch(7, "Subchannel-dependent slice"),
        list("a7.cuts", "Equal cuts by subchannel", 0, 32, 4096, &[1, 3]).adv(7),
        adv_switch(8, "Slice and group"),
        adv_switch(9, "Slice and alternate subchannel"),
        int("a9.step", "Subchannel step", -64, 64, 1).adv(9),
        adv_switch(10, "Recursive slicing"),
        int("a10.depth", "Halve every piece this many times", 1, 6, 1).adv(10),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, SLICE_HELP);
    ToolSpec {
        id: "slice",
        name: "Slice",
        family: Family::Interactive,
        summary: "Cuts notes into pieces at grid lines, chosen ticks, rays, waves or seeded positions.",
        params: p,
        advanced: vec![
            "Multi-line slice",
            "Parallel slice grid",
            "Radial slice",
            "Wave slice",
            "Random slice field",
            "Pitch-dependent slice",
            "Subchannel-dependent slice",
            "Slice and group",
            "Slice and alternate subchannel",
            "Recursive slicing",
        ],
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

fn slice_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let grid = note_value(p.choice("grid"), p.ticks("grid.ticks"), b.ctx);
    let seed = p.seed("seed");
    let lines: Vec<u64> = if p.adv(1) {
        p.list("a1.ticks")
            .iter()
            .map(|&x| x.max(0) as u64)
            .collect()
    } else {
        Vec::new()
    };
    let parallel = p
        .adv(2)
        .then(|| (p.ticks("a2.every").max(1), p.ticks("a2.offset")));
    let radial = p.adv(3).then(|| {
        let c = p.list("a3.centre");
        (
            c.first().copied().unwrap_or(500) as f64 / 1000.0,
            c.get(1).copied().unwrap_or(500) as f64 / 1000.0,
            p.int("a3.rays").max(1),
        )
    });
    let wave = p
        .adv(4)
        .then(|| (p.float("a4.amp") / 1000.0, p.float("a4.period")));
    let random = p.adv(5).then(|| p.int("a5.cuts") as u64);
    let pitch_cuts = p
        .adv(6)
        .then(|| (p.int("a6.low") as f64, p.int("a6.high") as f64));
    // Pitch-dependent cuts go from the lowest to the highest key of the
    // project (127, or 255 in 256-Key Mode).
    let top = f64::from(b.ctx.keys.max_pitch());
    let sub_cuts = if p.adv(7) {
        p.list("a7.cuts")
    } else {
        Vec::new()
    };
    let group = p.adv(8);
    let alt = p.adv(9).then(|| p.int("a9.step"));
    let depth = if p.adv(10) {
        p.int("a10.depth") as u32
    } else {
        0
    };
    let stats = b.stats.clone();
    let subs: Vec<Vec<(u32, u8)>> = b.tracks.iter().map(|t| t.subs.clone()).collect();
    let max_cuts = (b.stats.max_dur / grid).saturating_add(lines.len() as u64)
        + parallel.map_or(0, |(e, _)| b.stats.max_dur / e)
        + radial.map_or(0, |r| r.2 as u64)
        + random.unwrap_or(0)
        + 40;
    let per = max_cuts.saturating_add(1).saturating_mul(1 << depth);
    let total = b
        .stats
        .steps_bound(grid.min(parallel.map_or(u64::MAX, |x| x.0)))
        .saturating_add(b.stats.count as u64 * (per.min(1 << 16)));
    let span = stats.max_end.saturating_sub(stats.min_start).max(1) as f64;
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let (s, e) = (n.start, n.end());
            let mut cuts: Vec<u64> = Vec::new();
            // Checked: grid lines past the end of the tick range do not exist.
            let mut g = (s / grid).checked_add(1).and_then(|k| k.checked_mul(grid));
            while let Some(x) = g
                && x < e
                && cuts.len() < 1 << 16
            {
                cuts.push(x);
                g = x.checked_add(grid);
            }
            cuts.extend(lines.iter().copied().filter(|&t| t > s && t < e));
            if let Some((every, off)) = parallel {
                let mut t = if s >= off {
                    ((s - off) / every + 1)
                        .checked_mul(every)
                        .and_then(|x| x.checked_add(off))
                } else {
                    Some(off)
                };
                while let Some(x) = t
                    && x < e
                    && cuts.len() < 1 << 17
                {
                    cuts.push(x);
                    t = x.checked_add(every);
                }
            }
            if let Some((cx, cy, rays)) = radial {
                let (_, y) = norm_xy(n, &stats);
                for k in 0..rays {
                    let th = std::f64::consts::PI * (k as f64 + 0.5) / rays as f64;
                    let x = cx + (y - cy) / th.tan();
                    let t = stats.min_start as f64 + x * span;
                    if t > s as f64 && t < e as f64 {
                        cuts.push(t as u64);
                    }
                }
            }
            if let Some((amp, period)) = wave {
                let mid = s as f64 + (e - s) as f64 / 2.0;
                let t =
                    mid + amp * span * (std::f64::consts::TAU * f64::from(n.pitch) / period).sin();
                if t > s as f64 && t < e as f64 {
                    cuts.push(t as u64);
                }
            }
            if let Some(k) = random {
                let mut r = Rng::for_note(seed, salt::TICK, n);
                for _ in 0..k {
                    let t = s + (r.unit() * (e - s) as f64) as u64;
                    if t > s && t < e {
                        cuts.push(t);
                    }
                }
            }
            let equal = |k: u64, cuts: &mut Vec<u64>| {
                for j in 1..=k {
                    let t = s + (e - s) * j / (k + 1);
                    if t > s && t < e {
                        cuts.push(t);
                    }
                }
            };
            if let Some((lo, hi)) = pitch_cuts {
                equal(
                    (lo + (hi - lo) * f64::from(n.pitch) / top).round() as u64,
                    &mut cuts,
                );
            }
            let si = subs
                .get(usize::from(n.track))
                .and_then(|t| t.iter().position(|x| x.0 == n.sub))
                .unwrap_or(0);
            if !sub_cuts.is_empty() {
                equal(sub_cuts[si % sub_cuts.len()].max(0) as u64, &mut cuts);
            }
            cuts.sort_unstable();
            cuts.dedup();
            let mut pieces: Vec<(u64, u64)> = Vec::with_capacity(cuts.len() + 1);
            let mut a = s;
            for c in cuts {
                pieces.push((a, c));
                a = c;
            }
            pieces.push((a, e));
            for _ in 0..depth {
                pieces = pieces
                    .into_iter()
                    .flat_map(|(x, y)| {
                        if y - x >= 2 {
                            let m = x + (y - x) / 2;
                            vec![(x, m), (m, y)]
                        } else {
                            vec![(x, y)]
                        }
                    })
                    .collect();
            }
            let gid = if group {
                (note_key(n) as u32) | 1
            } else {
                n.group
            };
            for (k, (x, y)) in pieces.into_iter().enumerate() {
                let mut tpl = *n;
                tpl.group = gid;
                if let Some(step) = alt
                    && k % 2 == 1
                    && let Some(t) = subs.get(usize::from(n.track))
                    && !t.is_empty()
                {
                    let (id, ch) = t[((si as i64) + step).rem_euclid(t.len() as i64) as usize];
                    tpl.sub = id;
                    tpl.ch = ch;
                }
                out.emit(
                    i128::from(x),
                    i128::from(y - x),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    &tpl,
                );
            }
        },
        ahead: 0,
        behind: 0,
        growth: Growth::Generate(total),
    })
}

// ================================================================ Slice Notes

/// Most cut positions one Slice Notes run takes (the app passes the edit
/// cursor, or a time selection's start and end).
pub const SLICE_NOTES_MAX_CUTS: usize = 16;

/// The one-click Slice Notes command (v0.9 §118) as a platform kernel
/// (Undo, background runs, journal from the platform): cuts each selected
/// note that strictly contains a cut position. Not a tool: no window, no
/// advanced functions, not in any tool menu — the v0.8 Slice tool with its
/// advanced functions stays loadable for old presets (LEGACY_MAP `slice`).
pub const SLICE_NOTES: ToolDef = ToolDef {
    id: "slice_notes",
    spec: slice_notes_spec,
    build: slice_notes_build,
};

fn slice_notes_spec() -> ToolSpec {
    let mut p = vec![list(
        "at",
        "Cut at (ticks)",
        0,
        i64::MAX >> 2,
        SLICE_NOTES_MAX_CUTS,
        &[],
    )
    .help("The ticks where every selected note crossing them is cut; empty: the edit cursor, or the time selection's start and end.")];
    p.extend(boundary::specs(PitchPolicy::Discard));
    ToolSpec {
        id: "slice_notes",
        name: "Slice Notes",
        family: Family::Interactive,
        summary: "Cuts every selected note that crosses the edit cursor (or a time selection's start or end) into pieces there. Each piece keeps the note's pitch, velocity, MIDI channel, subchannel, group and mute state; notes the cut only touches stay whole.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

fn slice_notes_build(p: &Params, _b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let mut cuts: Vec<u64> = p.list("at").iter().map(|&x| x.max(0) as u64).collect();
    cuts.sort_unstable();
    cuts.dedup();
    Ok(Box::new(SliceNotes { cuts }))
}

/// The Slice Notes kernel: its preflight counts the pieces exactly (only
/// the notes a cut crosses grow), so a big selection is never refused by
/// the worst case of every note crossing every cut.
struct SliceNotes {
    /// Sorted, distinct cut ticks.
    cuts: Vec<u64>,
}

impl SliceNotes {
    /// The cuts strictly inside `[s, e)`.
    fn inside(&self, s: u64, e: u64) -> &[u64] {
        let lo = self.cuts.partition_point(|&c| c <= s);
        let hi = lo + self.cuts[lo..].partition_point(|&c| c < e);
        &self.cuts[lo..hi]
    }
}

impl Kernel for SliceNotes {
    fn growth(&self) -> Growth {
        Growth::Expand(self.cuts.len() as u64 + 1)
    }

    fn estimate_source(&self, src: &Source) -> super::super::Estimate {
        let extra: u64 = src
            .notes
            .iter()
            .map(|n| self.inside(n.start, n.end()).len() as u64)
            .sum();
        let count = src.notes.len() as u64;
        super::super::Estimate::for_notes(count, count.saturating_add(extra))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let (mut cut, mut pieces) = (0u64, 0u64);
        for n in &src.notes {
            let (s, e) = (n.start, n.end());
            let inside = self.inside(s, e);
            let mut a = s;
            for &c in inside {
                out.emit(
                    i128::from(a),
                    i128::from(c - a),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                );
                a = c;
            }
            if !inside.is_empty() {
                cut += 1;
                pieces += inside.len() as u64 + 1;
            }
            out.emit(
                i128::from(a),
                i128::from(e - a),
                i64::from(n.pitch),
                i64::from(n.vel),
                n,
            );
            out.check()?;
        }
        let g = crate::midi::group_digits;
        out.message = Some(if cut == 0 {
            "No selected note crosses the cut position.".into()
        } else {
            format!("{} note(s) cut into {} pieces.", g(cut), g(pieces))
        });
        out.check()
    }
}

// ================================================================ Snap to Scale

pub const SNAP_SCALE: ToolDef = ToolDef {
    id: "snap_scale",
    spec: snap_spec,
    build: snap_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SNAP_SCALE_HELP: &[(&str, &str)] = &[
    ("key", "The root note of the scale the notes snap to."),
    (
        "scale",
        "The scale the notes snap to; notes already in it stay where they are.",
    ),
    (
        "a1",
        "Uses your own set of notes as the scale instead of a named one.",
    ),
    (
        "a1.pcs",
        "The scale's notes as semitones above the root (0 = the root, 7 = a fifth).",
    ),
    (
        "a2",
        "Weights the scale's notes, so an out-of-scale note goes to the heavier of its two neighbors in the scale.",
    ),
    (
        "a2.weights",
        "The weight of each scale degree from the root upward (higher = preferred).",
    ),
    (
        "a3",
        "Always snaps out-of-scale notes in one direction instead of to the nearest scale note.",
    ),
    ("a3.dir", "Whether out-of-scale notes move up or down."),
    (
        "a4",
        "Keeps the melody's direction: a note that rose from the previous one still rises after snapping, and one that fell still falls.",
    ),
    (
        "a5",
        "Snaps each chord's notes to the scale triad built on its lowest note instead of to the whole scale.",
    ),
    ("a6", "Uses a different scale for each bar, in turn."),
    (
        "a6.scales",
        "The scale of each bar by its place in the Scale menu (0 = the first), repeated.",
    ),
    ("a7", "Uses a different scale for each subchannel."),
    (
        "a7.scales",
        "The scale of each subchannel position by its place in the Scale menu (0 = the first), repeated.",
    ),
    (
        "a8",
        "Decides where a note goes when the scale notes above and below are equally far.",
    ),
    (
        "a8.tie",
        "On a tie the note goes up, down, alternately up and down, or down so the lower voice stays lower.",
    ),
    (
        "a9",
        "Changes to a second scale halfway through the selection.",
    ),
    ("a9.to", "The scale used in the selection's second half."),
    (
        "a10",
        "Marks the notes that would move with a subchannel instead of moving them, so you can see them first.",
    ),
    (
        "a10.sub",
        "The subchannel position the marked notes get (0 = the track's first subchannel).",
    ),
    (
        "n01",
        "Finds the key and scale that fit the notes best and snaps to it.",
    ),
    (
        "n01.cands",
        "Which scales are tried: major and minor, all seven modes, or every scale.",
    ),
    (
        "n01.window",
        "Finds one scale for the whole selection, a scale for each bar, or one for every few bars.",
    ),
    (
        "n01.bars",
        "How many bars share one detected scale, used with Every N Bars.",
    ),
    (
        "n02",
        "Leaves short notes (and, if chosen, notes off the beat) as written, as passing notes.",
    ),
    (
        "n02.short",
        "Notes shorter than this are left as written, in ticks.",
    ),
    (
        "n02.weak",
        "Snap snaps notes off the beat as usual; Leave keeps them as written.",
    ),
    (
        "n03",
        "Tunes the scale with pitch bends, for just intonation and other tunings.",
    ),
    (
        "n03.tuning",
        "The tuning: just intonation, quarter tones, Pythagorean, or the cents listed below.",
    ),
    (
        "n03.cents",
        "How many cents each semitone above the root is tuned up or down (used by Listed Cents and rounded by 24-TET).",
    ),
    (
        "n03.range",
        "The pitch-bend range of the instrument, in semitones, so the bends come out right.",
    ),
    (
        "n03.channels",
        "Keep leaves notes on their channels; Allocate Free Channels moves notes that need different bends to channels of their own.",
    ),
    (
        "n04",
        "Snaps chords as a whole, moving every voice as little as possible without making voices cross.",
    ),
    (
        "n04.unisons",
        "Whether two notes of a chord may snap onto the same key.",
    ),
    (
        "n04.prefer",
        "Smallest Movement moves the notes as little as possible; Keep the Spacing keeps the gaps between the voices.",
    ),
    (
        "n04.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
    (
        "n05",
        "Uses the notes that occur in a reference track or subchannel as the scale.",
    ),
    (
        "n05.ref",
        "Takes the notes from a track or from a subchannel of the active track.",
    ),
    (
        "n05.num",
        "The track number (0 = the active track) or the subchannel position whose notes make the scale.",
    ),
    (
        "n05.window",
        "Builds one scale from the whole reference, or a scale for each bar.",
    ),
    (
        "n05.least",
        "How many times a note must occur in the reference to belong to the scale.",
    ),
    (
        "n06",
        "Snaps notes on strong grid positions to a chord instead of the whole scale.",
    ),
    (
        "n06.grid",
        "The grid that marks the strong positions (counted from each bar line).",
    ),
    (
        "n06.chord",
        "The chord used: the scale's tonic triad, each bar's triad on its lowest note, or the chord list below.",
    ),
    (
        "n06.chords",
        "Chord names such as C Am F G, one per bar, repeated.",
    ),
    ("n07", "Colors the moved notes by how far they moved."),
    (
        "n07.palette",
        "The subchannel position for notes moved 1, 2, 3 … semitones (0 = the track's first subchannel).",
    ),
    (
        "n08",
        "Leaves an out-of-scale note as written when it leads by a semitone into a scale note right after it.",
    ),
    (
        "n08.window",
        "How soon after the note the scale note must start, in ticks.",
    ),
    (
        "n09",
        "Snaps notes to the chords of a progression instead of the scale.",
    ),
    (
        "n09.chords",
        "Chord names such as Am F C G, used in turn and repeated.",
    ),
    (
        "n09.per",
        "How long each chord lasts: a bar, a beat, or the chord length below.",
    ),
    (
        "n09.len",
        "The length of each chord, in ticks, used with Chord Length.",
    ),
    (
        "n10",
        "Uses a different scale for rising and falling lines, as in classical melodic minor.",
    ),
    (
        "n10.rule",
        "Melodic Minor uses its rising form up and natural minor down; Bebop Major and Bebop Dominant add their passing notes on the way down.",
    ),
    ("n10.key", "The key the directional scales are built on."),
    (
        "n11",
        "Moves each chord as a block by the smallest shift that puts all its notes in the scale.",
    ),
    ("n11.shift", "The largest shift tried, in semitones."),
    (
        "n11.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
];

fn snap_spec() -> ToolSpec {
    let mut p = vec![
        choice("key", "Root", &KEY_NAMES, 0),
        choice("scale", "Scale", scale_names(), 0),
        adv_switch(1, "User-defined scale"),
        list("a1.pcs", "Pitch classes", 0, 11, 12, &[0, 2, 3, 7, 8]).adv(1),
        adv_switch(2, "Weighted allowed notes"),
        list(
            "a2.weights",
            "Weight per scale degree",
            0,
            100,
            12,
            &[10, 1, 5, 1, 8, 1, 1],
        )
        .adv(2),
        adv_switch(3, "Nearest-up / down preference"),
        choice("a3.dir", "Prefer", &["Up", "Down"], 1).adv(3),
        adv_switch(4, "Preserve melodic direction"),
        adv_switch(5, "Chord-aware target"),
        adv_switch(6, "Time-varying scale zones"),
        list(
            "a6.scales",
            "Scale per bar (menu index)",
            0,
            19,
            64,
            &[0, 1],
        )
        .adv(6),
        adv_switch(7, "Subchannel-specific scales"),
        list(
            "a7.scales",
            "Scale by subchannel (menu index)",
            0,
            19,
            4096,
            &[0, 10],
        )
        .adv(7),
        adv_switch(8, "Tie-resolution policy"),
        choice(
            "a8.tie",
            "On a tie",
            &["Up", "Down", "Alternate", "Keep the lower voice lower"],
            2,
        )
        .adv(8),
        adv_switch(9, "Scale morph across the selection"),
        choice("a9.to", "Morph to", scale_names(), 4).adv(9),
        adv_switch(10, "Preview excluded notes (mark, do not correct)"),
        int("a10.sub", "Mark with subchannel position", 0, 4095, 1).adv(10),
        // V09-S39 step 5: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Snap to Scale").
        flag("n01", "Scale detection", false),
        choice("n01.cands", "Candidates", SNAP_CANDIDATES, 0),
        choice("n01.window", "Window", SNAP_WINDOW, 0),
        int("n01.bars", "Bars", 1, 64, 4),
        flag("n02", "Passing tolerance", false),
        ticks(
            "n02.short",
            "Leave notes shorter than (ticks)",
            0,
            1 << 24,
            120,
        ),
        choice("n02.weak", "Weak beats", &["Snap", "Leave"], 0),
        flag("n03", "Tuning by bend", false),
        choice("n03.tuning", "Tuning", SNAP_TUNING, 0),
        list(
            "n03.cents",
            "Cents (per semitone above the root, cycled)",
            -100,
            100,
            12,
            &[0, 0, 0, 0, -14, 0, 0, 2, 0, 0, 0, 0],
        ),
        int("n03.range", "Bend range (semitones)", 1, 48, 2),
        choice(
            "n03.channels",
            "Channels",
            &["Keep", "Allocate free channels"],
            0,
        ),
        flag("n04", "Voice-aware snap", false),
        choice("n04.unisons", "Avoid unisons", &["Yes", "No"], 0),
        choice(
            "n04.prefer",
            "Prefer",
            &["Smallest movement", "Keep the spacing"],
            0,
        ),
        ticks("n04.tol", "Chord tolerance (ticks)", 0, 1 << 20, 20),
        flag("n05", "Reference scale", false),
        choice("n05.ref", "Reference", &["A track", "A subchannel"], 0),
        int(
            "n05.num",
            "Reference number (track number, 0 = the active track; or subchannel position)",
            0,
            65535,
            0,
        ),
        choice("n05.window", "Window", &["The whole range", "Every bar"], 0),
        int("n05.least", "Least occurrences", 1, 1000, 1),
        flag("n06", "Metric hierarchy", false),
        choice("n06.grid", "Grid", art_grid_values(), 2),
        choice("n06.chord", "Chord", SNAP_CHORD, 0),
        text(
            "n06.chords",
            "Chords (one per bar, cycled)",
            512,
            "C Am F G",
        ),
        flag("n07", "Move-distance colors", false),
        list(
            "n07.palette",
            "Palette by distance (subchannel positions: 1, 2, … semitones)",
            0,
            4095,
            64,
            &[1, 2, 3],
        ),
        flag("n08", "Approach keep", false),
        ticks("n08.window", "Approach window (ticks)", 0, 1 << 24, 240),
        flag("n09", "Chord progression snap", false),
        text("n09.chords", "Chords (cycled)", 512, "Am F C G"),
        choice("n09.per", "Per", SNAP_PER, 0),
        ticks("n09.len", "Chord length (ticks)", 1, 1 << 24, 1920),
        flag("n10", "Directional scales", false),
        choice("n10.rule", "Rule", SNAP_RULES, 0),
        choice("n10.key", "Key", &KEY_NAMES, 9),
        flag("n11", "Chord shift snap", false),
        int("n11.shift", "Largest shift (semitones)", 1, 12, 2),
        ticks("n11.tol", "Chord tolerance (ticks)", 0, 1 << 20, 20),
    ];
    p.extend(boundary::specs(PitchPolicy::Clamp));
    with_help(&mut p, SNAP_SCALE_HELP);
    ToolSpec {
        id: "snap_scale",
        name: "Snap to Scale",
        family: Family::Interactive,
        summary: "Moves out-of-scale notes to the nearest note of the chosen scale.",
        params: p,
        advanced: vec![
            "User-defined scale",
            "Weighted allowed notes",
            "Nearest-up / down preference",
            "Preserve melodic direction",
            "Chord-aware target",
            "Time-varying scale zones",
            "Subchannel-specific scales",
            "Tie-resolution policy",
            "Scale morph across the selection",
            "Preview excluded notes (mark, do not correct)",
        ],
        // Orders = the catalog's (§235) = where each runs in `Snap9`: the
        // target keys (n01, n05, n06, n09, n10 — the latest that applies
        // wins), the exceptions (n02, n08), the choice of target (n04, n11)
        // and the output (n07 colours, n03 bends).
        functions: [
            ("n01", "Scale detection", 110),
            ("n02", "Passing tolerance", 160),
            ("n03", "Tuning by bend", 210),
            ("n04", "Voice-aware snap", 180),
            ("n05", "Reference scale", 120),
            ("n06", "Metric hierarchy", 130),
            ("n07", "Move-distance colors", 200),
            ("n08", "Approach keep", 170),
            ("n09", "Chord progression snap", 140),
            ("n10", "Directional scales", 150),
            ("n11", "Chord shift snap", 190),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

fn snap_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S39 step 5: any new function, or Control Sweeps (evaluated per
    // note inside the kernel) — the v0.9 kernel; else the v0.8 one.
    let new = SnapNew::from(p, b.ctx);
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Snap9::build(p, b, new);
    }
    let root = p.choice("key");
    let base = if p.adv(1) {
        let mask = p
            .list("a1.pcs")
            .iter()
            .fold(0u16, |m, &c| m | 1 << c.rem_euclid(12));
        Scale::from_mask(root as u8, mask.max(1))
    } else {
        Scale::new(root, p.choice("scale"))
    };
    let weights = if p.adv(2) {
        p.list("a2.weights")
    } else {
        Vec::new()
    };
    let dir = p.adv(3).then(|| p.choice("a3.dir"));
    let melodic = p.adv(4);
    let chord = p.adv(5);
    let zones = if p.adv(6) {
        p.list("a6.scales")
    } else {
        Vec::new()
    };
    let sub_scales = if p.adv(7) {
        p.list("a7.scales")
    } else {
        Vec::new()
    };
    let tie = p.adv(8).then(|| p.choice("a8.tie"));
    let morph = p.adv(9).then(|| Scale::new(root, p.choice("a9.to")));
    let mark = p.adv(10).then(|| p.int("a10.sub"));
    let bar = b.ctx.bar();
    let stats = b.stats.clone();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let mut prev: std::collections::HashMap<(u16, u32), (i64, i64)> = Default::default();
            // Chord roots per onset group (lowest note) for the chord-aware mode.
            let chord_mask: Vec<u16> = if chord {
                let mut v = vec![0u16; src.notes.len()];
                let mut i = 0;
                while i < src.notes.len() {
                    let mut j = i;
                    while j < src.notes.len() && src.notes[j].start == src.notes[i].start {
                        j += 1;
                    }
                    let lo = src.notes[i..j].iter().map(|n| n.pitch).min().unwrap_or(0) as i64;
                    let sc = &base;
                    let (d, _) = sc.degree_of(sc.snap(lo, SnapMode::Nearest));
                    let m =
                        (0..3).fold(0u16, |m, k| m | 1 << sc.pitch_of(d + 2 * k).rem_euclid(12));
                    for x in v.iter_mut().take(j).skip(i) {
                        *x = m;
                    }
                    i = j;
                }
                v
            } else {
                Vec::new()
            };
            for (i, n) in src.notes.iter().enumerate() {
                let mut sc = base.clone();
                if !zones.is_empty() {
                    let z = (n.start.saturating_sub(stats.min_start) / bar.max(1)) as usize;
                    sc = Scale::new(root, zones[z % zones.len()] as usize);
                }
                if !sub_scales.is_empty() {
                    let si = sub_index(src, n);
                    sc = Scale::new(root, sub_scales[si % sub_scales.len()] as usize);
                }
                if let Some(to) = &morph
                    && pos(n, &stats) >= 0.5
                {
                    sc = to.clone();
                }
                let mask = if chord { chord_mask[i] } else { sc.mask() };
                let ok = |q: i64| mask & (1 << q.rem_euclid(12)) != 0;
                let p0 = i64::from(n.pitch);
                let key = (n.track, n.sub);
                let pv = prev.get(&key).copied();
                let target = if ok(p0) {
                    p0
                } else {
                    let mut up = p0;
                    while !ok(up) {
                        up += 1;
                    }
                    let mut dn = p0;
                    while !ok(dn) {
                        dn -= 1;
                    }
                    let du = up - p0;
                    let dd = p0 - dn;
                    let weight = |q: i64| -> i64 {
                        if weights.is_empty() {
                            return 0;
                        }
                        let (d, _) = sc.degree_of(q);
                        weights[(d.rem_euclid(sc.steps.len() as i64) as usize) % weights.len()]
                    };
                    if let Some((orig_prev, out_prev)) = pv.filter(|_| melodic) {
                        // Keep the written direction from the previous note.
                        if p0 > orig_prev && dn <= out_prev {
                            up
                        } else if p0 < orig_prev && up >= out_prev {
                            dn
                        } else if du <= dd {
                            up
                        } else {
                            dn
                        }
                    } else if let Some(d) = dir {
                        if d == 0 { up } else { dn }
                    } else if !weights.is_empty() && weight(up) != weight(dn) {
                        if weight(up) > weight(dn) { up } else { dn }
                    } else if du < dd {
                        up
                    } else if dd < du {
                        dn
                    } else {
                        match tie {
                            Some(0) => up,
                            Some(1) => dn,
                            Some(3) => dn,
                            Some(_) => {
                                if n.start.wrapping_add(i as u64).is_multiple_of(2) {
                                    up
                                } else {
                                    dn
                                }
                            }
                            None => dn,
                        }
                    }
                };
                prev.insert(key, (p0, target));
                let mut tpl = *n;
                if let Some(pos_sub) = mark {
                    if target != p0
                        && let Some(t) = src.track(n.track)
                        && !t.subs.is_empty()
                    {
                        let (id, ch) = t.subs[(pos_sub as usize) % t.subs.len()];
                        tpl.sub = id;
                        tpl.ch = ch;
                    }
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        p0,
                        i64::from(n.vel),
                        &tpl,
                    );
                } else {
                    out.emit(
                        i128::from(n.start),
                        i128::from(n.dur),
                        target,
                        i64::from(n.vel),
                        &tpl,
                    );
                }
                out.check()?;
            }
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}

// ================================================================ Flip

pub const FLIP: ToolDef = ToolDef {
    id: "flip",
    spec: flip_spec,
    build: flip_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const FLIP_HELP: &[(&str, &str)] = &[
    (
        "axis",
        "Which way the selection is mirrored: backward in time, upside down in pitch, or both.",
    ),
    (
        "a1",
        "Mirrors around a point you choose instead of the selection's center.",
    ),
    (
        "a1.pivot",
        "The point mirrored around: its time in thousandths of the selection's span, then its pitch.",
    ),
    (
        "a2",
        "Swaps time and pitch, as if the selection is flipped across its diagonal.",
    ),
    (
        "a3",
        "Flips each chord on its own instead of the whole selection.",
    ),
    (
        "a4",
        "Flips each bar on its own instead of the whole selection.",
    ),
    ("a5", "Flips each subchannel's notes on their own."),
    (
        "a6",
        "Flips pitches by scale steps, so the result stays in a key.",
    ),
    ("a6.key", "The key of the scale the flip stays in."),
    ("a6.scale", "The scale the flip stays in."),
    (
        "a7",
        "Reverses only the order of the note lengths, keeping the starts and pitches.",
    ),
    (
        "a8",
        "Mirrors the velocities: the loudest notes become the softest and the other way around.",
    ),
    ("a9", "Flips only every other bar."),
    (
        "a10",
        "Moves the notes only part of the way to their mirrored place.",
    ),
    (
        "a10.amount",
        "How far the notes move toward their mirrored place (0 = not at all, 1 = fully mirrored).",
    ),
    (
        "n01",
        "Keeps the original and adds the mirrored copy next to it or on top of it.",
    ),
    (
        "n01.place",
        "Where the mirrored copy goes: after the original, before it, or on top of it.",
    ),
    (
        "n01.vel",
        "The mirrored copy's velocity as a share of the original's.",
    ),
    (
        "n02",
        "Keeps one half of the selection and replaces the other half with its mirror image.",
    ),
    (
        "n02.keep",
        "Which half is kept: the upper, lower, left or right one.",
    ),
    (
        "n02.axis",
        "The line between the halves: the selection's center, or the custom pivot.",
    ),
    (
        "n03",
        "Chooses which point of each note is mirrored in time.",
    ),
    (
        "n03.anchor",
        "The point that is mirrored: each note's start, its end, or its center (as in v0.8).",
    ),
    ("n03.snap", "Whether the mirrored notes snap to a grid."),
    ("n03.grid", "The grid the mirrored notes snap to."),
    (
        "n04",
        "Repeats the selection with every copy mirrored and moved along, like footprints (a glide reflection).",
    ),
    (
        "n04.period",
        "How far apart the copies are in time, in ticks.",
    ),
    ("n04.offset", "How many rows each copy moves in pitch."),
    ("n04.copies", "How many copies are made."),
    (
        "n05",
        "Mirrors across a slanted line instead of a straight one.",
    ),
    ("n05.angle", "The mirror line's angle, in degrees."),
    (
        "n05.offset",
        "Moves the mirror line away from the center, as a share of half the box.",
    ),
    (
        "n06",
        "Mirrors time in seconds or in ticks, which differs where the tempo changes.",
    ),
    (
        "n06.in",
        "Whether the time mirror is measured in seconds (as heard) or in ticks.",
    ),
    (
        "n07",
        "Swaps the subchannels (colors) of the mirrored notes in pairs.",
    ),
    (
        "n07.pairs",
        "The subchannel positions in pairs: each a, b pair swaps a and b in the mirror image.",
    ),
    ("n08", "Flips only the notes inside a region."),
    (
        "n08.region",
        "The region's shape: an ellipse or a rectangle.",
    ),
    (
        "n08.cx",
        "The region's center in time, as a share of the selection's box.",
    ),
    (
        "n08.cy",
        "The region's center in pitch, as a share of the selection's box.",
    ),
    ("n08.size", "The region's size, as a share of the box."),
    ("n09", "Mirrors halves inside halves again and again."),
    ("n09.depth", "How many levels of mirrors are applied."),
    (
        "n09.axis",
        "Which way each level mirrors: in time, in pitch, or alternating starting with time.",
    ),
    (
        "n10",
        "Mirrors the timing inside every pair of grid steps, turning a swing feel around while the beats stay.",
    ),
    ("n10.grid", "The grid whose pairs of steps are mirrored."),
    (
        "n11",
        "Mirrors around a line that moves over time, following a curve.",
    ),
    (
        "n11.path",
        "The mirror line's height over the selection's time.",
    ),
    (
        "n11.range",
        "How many rows the mirror line's full height covers.",
    ),
];

fn flip_spec() -> ToolSpec {
    let mut p = vec![
        choice("axis", "Flip", &["Time (X)", "Pitch (Y)", "Both"], 0),
        adv_switch(1, "Custom pivot"),
        list(
            "a1.pivot",
            "Pivot (x ‰ of the span, pitch)",
            0,
            1000,
            2,
            &[500, 60],
        )
        .adv(1),
        adv_switch(2, "Diagonal flip (swap time and pitch)"),
        adv_switch(3, "Per-chord flip"),
        adv_switch(4, "Per-bar flip"),
        adv_switch(5, "Per-subchannel flip"),
        adv_switch(6, "Scale-aware pitch flip"),
        choice("a6.key", "Key", &KEY_NAMES, 0).adv(6),
        choice("a6.scale", "Scale", scale_names(), 0).adv(6),
        adv_switch(7, "Flip lengths only (retrograde gates)"),
        adv_switch(8, "Flip velocities (mirror the contour)"),
        adv_switch(9, "Alternating flip (every other bar)"),
        adv_switch(10, "Partial flip blend"),
        float("a10.amount", "Amount", 0.0, 1.0, 0.5, "").adv(10),
        // V09-S39 step 6: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Flip").
        flag("n01", "Mirror append", false),
        choice("n01.place", "Placement", FLIP_PLACE, 0),
        float("n01.vel", "Copy velocity", 0.0, 150.0, 100.0, "%"),
        flag("n02", "Half mirror", false),
        choice("n02.keep", "Keep", FLIP_HALF, 0),
        choice("n02.axis", "Axis", &["Center", "Pivot (Custom pivot)"], 0),
        flag("n03", "Mirror anchor", false),
        choice("n03.anchor", "Mirror the", FLIP_ANCHOR, 0),
        choice("n03.snap", "Snap", &["No", "To the grid"], 0),
        choice("n03.grid", "Grid", art_grid_values(), 4),
        flag("n04", "Glide reflection", false),
        ticks("n04.period", "Period (ticks)", 1, 1 << 30, 1920),
        int("n04.offset", "Pitch offset (rows)", -48, 48, 0),
        int("n04.copies", "Copies", 1, 64, 4),
        flag("n05", "Angled mirror", false),
        float("n05.angle", "Angle", -180.0, 180.0, 45.0, "°"),
        float("n05.offset", "Offset", -100.0, 100.0, 0.0, "%"),
        flag("n06", "Real-time mirror", false),
        choice("n06.in", "Mirror in", &["Seconds", "Ticks"], 0),
        flag("n07", "Mirror color pairs", false),
        list(
            "n07.pairs",
            "Pairs (subchannel positions: a, b, a, b, …)",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        ),
        flag("n08", "Region flip", false),
        choice("n08.region", "Region", FLIP_REGION, 0),
        float("n08.cx", "Center X", 0.0, 100.0, 50.0, "%"),
        float("n08.cy", "Center Y", 0.0, 100.0, 50.0, "%"),
        float("n08.size", "Size", 1.0, 200.0, 50.0, "%"),
        flag("n09", "Recursive mirror", false),
        int("n09.depth", "Depth", 1, 6, 2),
        choice("n09.axis", "Axis", FLIP_RECURSE, 0),
        flag("n10", "Swing flip", false),
        choice("n10.grid", "Grid", art_grid_values(), 3),
        flag("n11", "Moving axis", false),
        curve(
            "n11.path",
            "Axis path",
            Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
        ),
        int("n11.range", "Range (rows)", 1, 127, 12),
    ];
    p.extend(boundary::specs(PitchPolicy::Clamp));
    with_help(&mut p, FLIP_HELP);
    ToolSpec {
        id: "flip",
        name: "Advanced Flip",
        family: Family::Interactive,
        summary: "Mirrors the selection in time and / or pitch around its bounds or a pivot.",
        params: p,
        advanced: vec![
            "Custom pivot",
            "Diagonal flip (swap time and pitch)",
            "Per-chord flip",
            "Per-bar flip",
            "Per-subchannel flip",
            "Scale-aware pitch flip",
            "Flip lengths only (retrograde gates)",
            "Flip velocities (mirror the contour)",
            "Alternating flip (every other bar)",
            "Partial flip blend",
        ],
        // Orders = the catalog's (§235) = where each runs in `Flip9`:
        // which notes flip (n08), the mirror (n06 seconds, n03 anchor, n05
        // angled line, n11 moving axis, or n09 recursive), the timing (n10),
        // the content (n02 half, n07 colours) and the copies (n01, n04).
        functions: [
            ("n01", "Mirror append", 200),
            ("n02", "Half mirror", 180),
            ("n03", "Mirror anchor", 130),
            ("n04", "Glide reflection", 210),
            ("n05", "Angled mirror", 140),
            ("n06", "Real-time mirror", 120),
            ("n07", "Mirror color pairs", 190),
            ("n08", "Region flip", 110),
            ("n09", "Recursive mirror", 160),
            ("n10", "Swing flip", 170),
            ("n11", "Moving axis", 150),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

fn flip_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S39 step 6: any new function, or Control Sweeps (evaluated per
    // note inside the kernel) — the v0.9 kernel (`tools/flip.rs`); else the
    // v0.8 one.
    let new = FlipNew::from(p, b.ctx);
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Flip9::build(p, b, new);
    }
    let axis = p.choice("axis");
    let pivot = p.adv(1).then(|| {
        let l = p.list("a1.pivot");
        (
            l.first().copied().unwrap_or(500) as f64 / 1000.0,
            l.get(1).copied().unwrap_or(60),
        )
    });
    let diagonal = p.adv(2);
    let per_chord = p.adv(3);
    let per_bar = p.adv(4);
    let per_sub = p.adv(5);
    let diatonic = p
        .adv(6)
        .then(|| Scale::new(p.choice("a6.key"), p.choice("a6.scale")));
    let gates = p.adv(7);
    let vels = p.adv(8);
    let alternate = p.adv(9);
    let blend = p.adv(10).then(|| p.float("a10.amount"));
    let bar = b.ctx.bar();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let n = src.notes.len();
            if n == 0 {
                return Ok(());
            }
            // Groups that flip independently.
            let key = |x: &TNote| -> (u64, u32) {
                let b0 = if per_bar || alternate {
                    x.start / bar.max(1)
                } else {
                    0
                };
                (b0, if per_sub { x.sub } else { u32::MAX })
            };
            let mut groups: std::collections::BTreeMap<(u64, u32), Vec<usize>> = Default::default();
            for (i, x) in src.notes.iter().enumerate() {
                groups.entry(key(x)).or_default().push(i);
            }
            let mut res: Vec<(i128, i128, i64, i64)> = src
                .notes
                .iter()
                .map(|x| {
                    (
                        i128::from(x.start),
                        i128::from(x.dur),
                        i64::from(x.pitch),
                        i64::from(x.vel),
                    )
                })
                .collect();
            for ((barno, _), idx) in &groups {
                if alternate && barno % 2 == 0 {
                    continue;
                }
                let s = idx.iter().map(|&i| src.notes[i].start).min().unwrap_or(0) as i128;
                let e = idx.iter().map(|&i| src.notes[i].end()).max().unwrap_or(0) as i128;
                let lo = idx.iter().map(|&i| src.notes[i].pitch).min().unwrap_or(0) as i64;
                let hi = idx.iter().map(|&i| src.notes[i].pitch).max().unwrap_or(0) as i64;
                let (px, pp) = match pivot {
                    Some((fx, pitch)) => (s as f64 + (e - s) as f64 * fx, pitch as f64),
                    None => ((s + e) as f64 / 2.0, (lo + hi) as f64 / 2.0),
                };
                if gates {
                    let mut order = idx.clone();
                    order.sort_by_key(|&i| (src.notes[i].start, src.notes[i].pitch));
                    let durs: Vec<u64> = order.iter().rev().map(|&i| src.notes[i].dur).collect();
                    for (k, &i) in order.iter().enumerate() {
                        res[i].1 = i128::from(durs[k]);
                    }
                }
                if vels {
                    let mut order = idx.clone();
                    order.sort_by_key(|&i| (src.notes[i].start, src.notes[i].pitch));
                    let v: Vec<u8> = order.iter().rev().map(|&i| src.notes[i].vel).collect();
                    for (k, &i) in order.iter().enumerate() {
                        res[i].3 = i64::from(v[k]);
                    }
                }
                if gates || vels {
                    continue;
                }
                for &i in idx {
                    let x = &src.notes[i];
                    let (mut ns, mut np) = (i128::from(x.start), i64::from(x.pitch));
                    if diagonal {
                        // Swap normalized time and pitch inside the group's box.
                        let u = if e > s {
                            (x.start as f64 - s as f64) / (e - s) as f64
                        } else {
                            0.0
                        };
                        let v = if hi > lo {
                            (np - lo) as f64 / (hi - lo) as f64
                        } else {
                            0.0
                        };
                        ns = s + (v * (e - s) as f64).round() as i128;
                        np = lo + (u * (hi - lo) as f64).round() as i64;
                    } else {
                        if axis == 0 || axis == 2 {
                            ns = (2.0 * px - x.end() as f64).round() as i128;
                        }
                        if axis == 1 || axis == 2 {
                            np = match &diatonic {
                                Some(sc) => {
                                    let (da, _) = sc.degree_of(pp.round() as i64);
                                    let (dp, off) = sc.degree_of(np);
                                    sc.pitch_of(2 * da - dp) - off
                                }
                                None => (2.0 * pp - np as f64).round() as i64,
                            };
                        }
                    }
                    if let Some(a) = blend {
                        ns = (x.start as f64 + (ns as f64 - x.start as f64) * a).round() as i128;
                        np = (f64::from(x.pitch) + (np as f64 - f64::from(x.pitch)) * a).round()
                            as i64;
                    }
                    res[i].0 = ns;
                    res[i].2 = np;
                }
            }
            if per_chord {
                // Reverse the pitch order inside each onset group.
                let mut i = 0;
                while i < n {
                    let mut j = i;
                    while j < n
                        && src.notes[j].start == src.notes[i].start
                        && src.notes[j].track == src.notes[i].track
                    {
                        j += 1;
                    }
                    let mut ps: Vec<i64> = (i..j).map(|k| res[k].2).collect();
                    ps.sort_unstable();
                    let mut order: Vec<usize> = (i..j).collect();
                    order.sort_by_key(|&k| res[k].2);
                    for (r, &k) in order.iter().enumerate() {
                        res[k].2 = ps[ps.len() - 1 - r];
                    }
                    i = j;
                }
            }
            for (x, r) in src.notes.iter().zip(res) {
                out.emit(r.0, r.1, r.2, r.3, x);
                out.check()?;
            }
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}

// ================================================================ Duplicate

pub const DUPLICATE: ToolDef = ToolDef {
    id: "duplicate",
    spec: dup_spec,
    build: dup_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const DUPLICATE_HELP: &[(&str, &str)] = &[
    (
        "grid",
        "Rounds the distance to the next copy up to a whole snap step, so copies land on the grid.",
    ),
    ("a1", "Makes several copies in a row instead of one."),
    ("a1.count", "How many copies follow the selection."),
    (
        "a2",
        "Adds a gap (or, negative, an overlap) between one copy and the next.",
    ),
    (
        "a2.ticks",
        "The extra distance between copies, in ticks (negative copies overlap).",
    ),
    (
        "a3",
        "Moves each copy further up or down in pitch than the previous one.",
    ),
    (
        "a3.step",
        "How many semitones each copy moves from the previous one (12 = an octave).",
    ),
    (
        "a4",
        "Makes each copy quieter than the previous one, like an echo.",
    ),
    (
        "a4.decay",
        "Each copy's velocity as a share of the previous copy's.",
    ),
    (
        "a5",
        "Moves each copy to the next subchannel of its track (wrapping around the list).",
    ),
    (
        "a5.step",
        "How many subchannel positions each copy moves (negative = backward).",
    ),
    ("a6", "Rounds the distance between copies up to whole bars."),
    ("a7", "Plays every other copy backward (mirrored in time)."),
    (
        "a8",
        "Transposes each copy by scale steps, so it stays in a key.",
    ),
    ("a8.key", "The key of the scale the copies move in."),
    ("a8.scale", "The scale the copies move in."),
    (
        "a8.degrees",
        "How many scale steps each copy moves from the previous one.",
    ),
    ("a9", "Leaves out notes of the copies at random (seeded)."),
    ("a9.prob", "The chance that a note of a copy is left out."),
    (
        "a10",
        "Repeats the selection until a set tick, as many copies as fit.",
    ),
    ("a10.until", "The tick the copies fill up to."),
    (
        "n01",
        "Puts the copies on other tracks instead of after the selection.",
    ),
    (
        "n01.targets",
        "Where the copies go: tracks after the selection's track, tracks listed by number, or the track's subtracks.",
    ),
    (
        "n01.tracks",
        "With Next Tracks, how many tracks after the selected one (1 = the next); with Listed Tracks, the tracks' numbers.",
    ),
    (
        "n01.count",
        "How many copies each target track gets, stacked at the same time.",
    ),
    (
        "n02",
        "Copies the selection as a canon: each entry comes in later and changed.",
    ),
    (
        "n02.kind",
        "Augmentation makes each entry slower; Diminution faster; Inversion turns it upside down; Retrograde plays it backward.",
    ),
    (
        "n02.factor",
        "How much slower (Augmentation) or faster (Diminution) each entry is than the previous one.",
    ),
    (
        "n02.delay",
        "How long after the previous entry each entry comes in, in ticks.",
    ),
    ("n02.entries", "How many entries follow the original."),
    (
        "n03",
        "Pastes a copy at chosen positions in the song instead of right after the selection.",
    ),
    (
        "n03.at",
        "The positions: every marker, every bar of the time selection, each chord change of a reference track, or a list of ticks.",
    ),
    (
        "n03.ticks",
        "The ticks the copies start at, used with Listed Ticks.",
    ),
    (
        "n03.ref",
        "The track whose chord changes give the positions, by its number in the track list (0 = this track).",
    ),
    (
        "n04",
        "Shifts each copy's notes along the selection's own pitch sequence, so the melody rotates from copy to copy.",
    ),
    (
        "n04.rotate",
        "How many places in the sequence each copy shifts (negative = the other way).",
    ),
    (
        "n05",
        "Places copies along a drawn path: each copy is moved in pitch by the path's height.",
    ),
    (
        "n05.path",
        "The path's shape over the copies, from the first to the last.",
    ),
    (
        "n05.range",
        "How many semitones the path's full height moves a copy.",
    ),
    (
        "n05.spacing",
        "The distance from one copy to the next, in ticks.",
    ),
    ("n05.copies", "How many copies are placed along the path."),
    (
        "n12",
        "Makes the copies a harmonic sequence: each one moves by a fixed interval.",
    ),
    (
        "n12.motion",
        "The interval each copy moves: a fifth up, a fourth up, a third down, or a semitone.",
    ),
    (
        "n12.within",
        "Chromatic moves by exact semitones; The Key moves by scale steps in a key.",
    ),
    (
        "n12.key",
        "The key the sequence moves in, used with The Key.",
    ),
    (
        "n12.scale",
        "The scale the sequence moves in, used with The Key.",
    ),
    (
        "n07",
        "Fits each copy to a chord progression, one chord per bar.",
    ),
    (
        "n07.chords",
        "Chord names such as C Am F G, one per bar, repeated when the list runs out.",
    ),
    (
        "n07.fit",
        "Transpose by the Root moves the whole copy to the bar's chord root; Snap to Chord Tones moves each note to the nearest chord tone.",
    ),
    (
        "n08",
        "Also copies the controller events under the selection with each copy.",
    ),
    (
        "n08.include",
        "What is copied: only notes, notes and controllers, or notes, controllers and pitch bend.",
    ),
    (
        "n08.ccs",
        "The controller numbers that are copied; leave it empty to copy all of them.",
    ),
    (
        "n09",
        "Thins each copy more than the previous one, so the copies fade out note by note.",
    ),
    (
        "n09.keep",
        "The share of notes each copy keeps from the previous one.",
    ),
    (
        "n09.by",
        "Which notes stay longest: the outline (top and bottom voices), the loudest, or notes spread evenly in time.",
    ),
    (
        "n10",
        "Stutters the start of the selection: its first notes repeat before each copy.",
    ),
    (
        "n10.notes",
        "How many of the selection's first notes make up the stutter.",
    ),
    ("n10.times", "How many times the stutter repeats."),
    (
        "n11",
        "Places copies at a list of distances from the selection.",
    ),
    (
        "n11.offsets",
        "How far each copy is from the original, in ticks, one copy per entry.",
    ),
];

fn dup_spec() -> ToolSpec {
    let mut p = vec![
        flag("grid", "Round the offset up to the snap", true),
        seed("seed"),
        adv_switch(1, "Count"),
        int("a1.count", "Copies", 1, 256, 3).adv(1),
        adv_switch(2, "Gap / overlap offset"),
        int(
            "a2.ticks",
            "Extra ticks between copies",
            -1_000_000,
            1_000_000,
            240,
        )
        .adv(2),
        adv_switch(3, "Pitch step per copy"),
        int("a3.step", "Semitones", -48, 48, 12).adv(3),
        adv_switch(4, "Velocity decay"),
        float("a4.decay", "Velocity per copy", 5.0, 100.0, 75.0, "%").adv(4),
        adv_switch(5, "Subchannel advance per copy"),
        int("a5.step", "Subchannel steps", -64, 64, 1).adv(5),
        adv_switch(6, "Bar-aligned placement"),
        adv_switch(7, "Mirror alternate copies"),
        adv_switch(8, "Scale-aware transpose per copy"),
        choice("a8.key", "Key", &KEY_NAMES, 0).adv(8),
        choice("a8.scale", "Scale", scale_names(), 0).adv(8),
        int("a8.degrees", "Scale degrees per copy", -14, 14, 2).adv(8),
        adv_switch(9, "Seeded omission"),
        float("a9.prob", "Omit probability", 0.0, 100.0, 25.0, "%").adv(9),
        adv_switch(10, "Fill to a target tick"),
        ticks("a10.until", "Until tick", 0, u64::MAX >> 2, 38_400).adv(10),
        // V09-S39 step 7: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Duplicate").
        flag("n01", "To tracks", false),
        choice("n01.targets", "Targets", DUP_TARGETS, 0),
        list(
            "n01.tracks",
            "Tracks (next: offsets after the track; listed: track numbers)",
            1,
            65_535,
            64,
            &[1],
        ),
        int("n01.count", "Copies on each track", 1, 64, 1),
        flag("n02", "Canon", false),
        choice("n02.kind", "Kind", DUP_CANON, 0),
        float("n02.factor", "Factor", 25.0, 400.0, 200.0, "%"),
        ticks("n02.delay", "Entry delay (ticks)", 0, 1 << 30, 1920),
        int("n02.entries", "Entries", 1, 16, 1),
        flag("n03", "At positions", false),
        choice("n03.at", "At", DUP_AT, 3),
        list("n03.ticks", "Ticks", 0, 1_000_000_000, 256, &[3840, 7680]),
        int("n03.ref", "Reference track (0 = this track)", 0, 65_535, 0),
        flag("n04", "Pitch rotation per copy", false),
        int("n04.rotate", "Rotate per copy", -64, 64, 1),
        flag("n05", "Along a path", false),
        curve(
            "n05.path",
            "Path",
            Curve::new(vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]),
        ),
        int("n05.range", "Pitch range", 1, 127, 12),
        ticks("n05.spacing", "Spacing (ticks)", 1, 1 << 30, 1920),
        int("n05.copies", "Copies", 1, 256, 8),
        flag("n12", "Harmonic sequence", false),
        choice("n12.motion", "Motion", DUP_MOTION, 0),
        choice("n12.within", "Within", DUP_WITHIN, 0),
        choice("n12.key", "Key", &KEY_NAMES, 0),
        choice("n12.scale", "Scale", scale_names(), 0),
        flag("n07", "Progression fit", false),
        text(
            "n07.chords",
            "Chords (one per bar, cycled)",
            512,
            "C Am F G",
        ),
        choice("n07.fit", "Fit", DUP_FIT, 0),
        flag("n08", "Copy controllers", false),
        choice("n08.include", "Include", DUP_INCLUDE, 1),
        list("n08.ccs", "Controllers (empty = all)", 0, 119, 120, &[]),
        flag("n09", "Thinning cascade", false),
        float("n09.keep", "Keep per copy", 10.0, 100.0, 50.0, "%"),
        choice("n09.by", "Keep the", DUP_THIN, 0),
        flag("n10", "Stutter copy", false),
        int("n10.notes", "Stutter notes", 1, 64, 4),
        int("n10.times", "Times", 1, 16, 2),
        flag("n11", "Offset list", false),
        list(
            "n11.offsets",
            "Offsets (ticks)",
            0,
            1_000_000_000,
            256,
            &[1920, 2880, 4800],
        ),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, DUPLICATE_HELP);
    ToolSpec {
        id: "duplicate",
        name: "Duplicate",
        family: Family::Interactive,
        summary: "Copies the selection right after itself (FL's Ctrl+B), with counts, offsets and per-copy changes.",
        params: p,
        advanced: vec![
            "Count",
            "Gap / overlap offset",
            "Pitch step per copy",
            "Velocity decay",
            "Subchannel advance per copy",
            "Bar-aligned placement",
            "Mirror alternate copies",
            "Scale-aware transpose per copy",
            "Seeded omission",
            "Fill to a target tick",
        ],
        // Orders = the catalog's (§235) = where each runs in `Dup9`: the
        // copies (n03 positions, n11 offsets, n05 path or n02 canon — the
        // first on; else n01's layers or v0.8's step), their tracks (n01),
        // the stutter before each (n10), the pitch (n04 rotation, n12
        // sequence, n07 fit), the share kept (n09) and the controllers
        // (n08).
        functions: [
            ("n01", "To tracks", 150),
            ("n02", "Canon", 140),
            ("n03", "At positions", 110),
            ("n04", "Pitch rotation per copy", 170),
            ("n05", "Along a path", 130),
            ("n12", "Harmonic sequence", 180),
            ("n07", "Progression fit", 190),
            ("n08", "Copy controllers", 210),
            ("n09", "Thinning cascade", 200),
            ("n10", "Stutter copy", 160),
            ("n11", "Offset list", 120),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

fn dup_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S39 step 7: any new function, or Control Sweeps (evaluated per
    // note inside the kernel) — the v0.9 kernel (`tools/duplicate.rs`);
    // else the v0.8 one.
    let new = DupNew::from(p, b.ctx);
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Dup9::build(p, b, new);
    }
    let snap = b.ctx.snap.max(1);
    let bar = b.ctx.bar().max(1);
    let span = b.stats.max_end.saturating_sub(b.stats.min_start).max(1);
    let mut offset = if p.flag("grid") {
        span.div_ceil(snap).saturating_mul(snap)
    } else {
        span
    };
    if p.adv(6) {
        offset = offset.div_ceil(bar).saturating_mul(bar);
    }
    let extra = if p.adv(2) { p.int("a2.ticks") } else { 0 };
    let step = (i128::from(offset) + i128::from(extra)).max(1);
    let mut count = if p.adv(1) {
        p.int("a1.count") as u64
    } else {
        1
    };
    if p.adv(10) {
        let until = p.ticks("a10.until");
        let room = i128::from(until) - i128::from(b.stats.max_end);
        count = if room > 0 {
            (room / step + 1).min(1 << 16) as u64
        } else {
            0
        };
    }
    let pitch_step = if p.adv(3) { p.int("a3.step") } else { 0 };
    let decay = p.adv(4).then(|| p.float("a4.decay") / 100.0);
    let sub_step = if p.adv(5) { p.int("a5.step") } else { 0 };
    let mirror = p.adv(7);
    let scale = p.adv(8).then(|| {
        (
            Scale::new(p.choice("a8.key"), p.choice("a8.scale")),
            p.int("a8.degrees"),
        )
    });
    let omit = p.adv(9).then(|| p.float("a9.prob") / 100.0);
    let seed = p.seed("seed");
    let (s0, e0) = (b.stats.min_start, b.stats.max_end);
    let subs: Vec<Vec<(u32, u8)>> = b.tracks.iter().map(|t| t.subs.clone()).collect();
    let reach = (step.unsigned_abs() as u64).saturating_mul(count + 1);
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            out.set_dim(0);
            out.push(*n);
            for c in 1..=count {
                out.set_dim(c as u32);
                if let Some(pr) = omit
                    && Rng::new(hash(&[seed, salt::KEEP, note_key(n), c])).chance(pr)
                {
                    continue;
                }
                let base = if mirror && c % 2 == 1 {
                    i128::from(s0) + i128::from(e0) - i128::from(n.end())
                } else {
                    i128::from(n.start)
                };
                let start = base + step * i128::from(c);
                let mut pitch = i64::from(n.pitch) + pitch_step * c as i64;
                if let Some((sc, deg)) = &scale {
                    let (d, off) = sc.degree_of(pitch);
                    pitch = sc.pitch_of(d + deg * c as i64) + off;
                }
                let vel = match decay {
                    Some(dk) => (f64::from(n.vel) * dk.powi(c as i32)).round() as i64,
                    None => i64::from(n.vel),
                };
                let mut tpl = *n;
                if sub_step != 0
                    && let Some(t) = subs.get(usize::from(n.track))
                    && !t.is_empty()
                {
                    let i = t.iter().position(|x| x.0 == n.sub).unwrap_or(0) as i64;
                    let (id, ch) = t[(i + sub_step * c as i64).rem_euclid(t.len() as i64) as usize];
                    tpl.sub = id;
                    tpl.ch = ch;
                }
                out.emit(start, i128::from(n.dur), pitch, vel, &tpl);
            }
        },
        ahead: 0,
        behind: reach,
        growth: Growth::Expand(count + 1),
    })
}

// ================================================================ Duplicate One Octave

/// The one-click Duplicate Up / Down One Octave commands (v0.9 §082) as a
/// platform kernel (Preview, Undo, background runs from the platform): every
/// selected note stays and gets a copy `semis` semitones away (±12 from the
/// menus) at the same time, with all its attributes. Copies outside the
/// project's key range are dropped and reported (boundary policy). Not a
/// tool: no window, no advanced functions — the Duplicate tool's
/// "Pitch step per copy" covers placed copies.
pub const DUPLICATE_OCTAVE: ToolDef = ToolDef {
    id: "duplicate_octave",
    spec: dup_octave_spec,
    build: dup_octave_build,
};

fn dup_octave_spec() -> ToolSpec {
    let mut p = vec![int("semis", "Semitones", -24, 24, 12)
        .help("How far the copy of every selected note lies from it, in semitones (12 = an octave up, -12 = down).")];
    p.extend(boundary::specs(PitchPolicy::Discard));
    ToolSpec {
        id: "duplicate_octave",
        name: "Duplicate One Octave",
        family: Family::Interactive,
        summary: "Adds a copy of every selected note one octave up (or down) at the same time, keeping its length, velocity, channel, subchannel, group and mute state. Copies beyond the project's keys are left out.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

fn dup_octave_build(p: &Params, _b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let semis = p.int("semis");
    if semis == 0 {
        return Err(TransformError::Invalid(
            "A copy at the same pitch would duplicate every note in place; choose a step other than 0 semitones.".into(),
        ));
    }
    boxed(Pointwise {
        f: move |n: &TNote, out: &mut Output| {
            let (s, d) = (i128::from(n.start), i128::from(n.dur));
            out.emit(s, d, i64::from(n.pitch), i64::from(n.vel), n);
            out.emit(s, d, i64::from(n.pitch) + semis, i64::from(n.vel), n);
        },
        ahead: 0,
        behind: 0,
        growth: Growth::Expand(2),
    })
}

// ================================================================ Score stretch

pub const STRETCH: ToolDef = ToolDef {
    id: "stretch",
    spec: stretch_spec,
    build: stretch_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const STRETCH_HELP: &[(&str, &str)] = &[
    (
        "factor",
        "How long the selection becomes, as a share of its length (200 % = twice as long, 50 % = half).",
    ),
    (
        "a1",
        "Stretches the selection to an exact length instead of by a factor.",
    ),
    (
        "a1.len",
        "The length the selection is stretched to, in ticks.",
    ),
    ("a2", "Chooses the point the selection is stretched around."),
    (
        "a2.pivot",
        "The point that stays in place: the selection's start, end, center, or the playhead.",
    ),
    (
        "a3",
        "Stretches only the note lengths, leaving the starts in place.",
    ),
    (
        "a4",
        "Stretches only the note starts, leaving the lengths as they are.",
    ),
    (
        "a5",
        "Stretches by a curve, so some parts stretch more than others.",
    ),
    (
        "a5.curve",
        "Where each position of the selection moves to (a straight line = an even stretch).",
    ),
    ("a6", "Stretches each subchannel by its own factor."),
    (
        "a6.factors",
        "The stretch factor of each subchannel position, in percent.",
    ),
    ("a7", "Snaps the stretched note starts to the grid."),
    (
        "a8",
        "Stretches in seconds instead of ticks, so the result keeps its timing through tempo changes.",
    ),
    (
        "a9",
        "Stretches only one segment of time, moving the notes after it along.",
    ),
    ("a9.from", "Where the stretched segment starts, in ticks."),
    ("a9.to", "Where the stretched segment ends, in ticks."),
    (
        "a10",
        "Also spreads the pitches apart (or together) around their middle.",
    ),
    (
        "a10.factor",
        "How wide the pitch range becomes, as a share of its own.",
    ),
    (
        "n01",
        "Converts the meter, so bars of one length become bars of another.",
    ),
    ("n01.from", "How many beats a bar has now."),
    ("n01.to", "How many beats a bar should have."),
    (
        "n02",
        "Keeps short notes at their written length while the rest stretch.",
    ),
    (
        "n02.short",
        "Notes shorter than this keep their length, in ticks.",
    ),
    (
        "n03",
        "Stretches by moving chosen points of the selection to new places.",
    ),
    (
        "n03.anchors",
        "Pairs of positions in thousandths of the selection: where a point is, then where it moves to.",
    ),
    (
        "n04",
        "Compresses without letting the notes per second get too high, thinning notes where needed.",
    ),
    (
        "n04.at",
        "The notes per second kept: those of the original, or a limit you set.",
    ),
    (
        "n04.limit",
        "The most notes per second allowed, used with A Limit.",
    ),
    (
        "n04.keep",
        "Which notes stay when thinning: the loudest, or the outline (top and bottom voices).",
    ),
    (
        "n05",
        "Stretches only the rests between phrases, leaving the phrases themselves alone.",
    ),
    (
        "n05.rest",
        "A rest at least this long splits two phrases, in ticks.",
    ),
    (
        "n05.factor",
        "How long the rests between phrases become, as a share of their length.",
    ),
    (
        "n06",
        "Keeps the groove: notes keep their offset from the grid after the stretch.",
    ),
    ("n06.grid", "The grid the groove is measured against."),
    (
        "n06.keep",
        "How much of each note's offset from the grid is kept.",
    ),
    (
        "n07",
        "Changes the pitch with the stretch, like a tape played slower or faster.",
    ),
    (
        "n07.semis",
        "How many semitones the pitch drops each time the length doubles.",
    ),
    (
        "n08",
        "Slows the end of the selection down, like a tape machine stopping.",
    ),
    ("n08.len", "How long the slow-down at the end is, in ticks."),
    (
        "n08.drop",
        "How many semitones the pitch drops by the very end.",
    ),
    (
        "n08.curve",
        "How the slow-down progresses over the stopping time.",
    ),
    ("n09", "Stretches each bar by its own factor."),
    (
        "n09.factors",
        "The stretch factor of each bar in turn, in percent (repeated).",
    ),
    (
        "n10",
        "Stretches before the pivot by one factor and after it by another.",
    ),
    (
        "n10.before",
        "How much the part before the pivot is stretched.",
    ),
    (
        "n10.after",
        "How much the part after the pivot is stretched.",
    ),
    (
        "n11",
        "Keeps stretched notes from running into the next note.",
    ),
    (
        "n11.at",
        "Where a note is cut: at the next note on the same key, or at the next note of any key.",
    ),
];

fn stretch_spec() -> ToolSpec {
    let mut p = vec![
        float("factor", "Stretch", 1.0, 1000.0, 200.0, "%"),
        adv_switch(1, "Fit to a target length"),
        ticks("a1.len", "Target length", 1, u64::MAX >> 2, 7680).adv(1),
        adv_switch(2, "Pivot"),
        choice(
            "a2.pivot",
            "Around",
            &["Start", "End", "Center", "Cursor"],
            2,
        )
        .adv(2),
        adv_switch(3, "Stretch lengths only"),
        adv_switch(4, "Stretch onsets only"),
        adv_switch(5, "Nonlinear stretch curve"),
        curve(
            "a5.curve",
            "Position → new position",
            Curve::smooth(vec![(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]),
        )
        .adv(5),
        adv_switch(6, "Per-subchannel factors"),
        list(
            "a6.factors",
            "Factor % by subchannel",
            1,
            1000,
            4096,
            &[100, 200],
        )
        .adv(6),
        adv_switch(7, "Snap stretched onsets to the grid"),
        adv_switch(8, "Tempo-preserving (stretch in seconds)"),
        adv_switch(9, "Segment stretch"),
        ticks("a9.from", "Segment from", 0, u64::MAX >> 2, 0).adv(9),
        ticks("a9.to", "Segment to", 0, u64::MAX >> 2, 3840).adv(9),
        adv_switch(10, "Proportional pitch stretch"),
        float("a10.factor", "Pitch range", 10.0, 400.0, 150.0, "%").adv(10),
        // V09-S39 step 8: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Score Stretch").
        flag("n01", "Meter conversion", false),
        int("n01.from", "From beats per bar", 1, 32, 4),
        int("n01.to", "To beats per bar", 1, 32, 3),
        flag("n02", "Short-note keep", false),
        ticks(
            "n02.short",
            "Keep notes shorter than (ticks)",
            1,
            1 << 30,
            240,
        ),
        flag("n03", "Anchor map", false),
        list(
            "n03.anchors",
            "Anchors (from ‰, to ‰ pairs)",
            0,
            1000,
            64,
            &[500, 250],
        ),
        flag("n04", "NPS-safe compression", false),
        choice("n04.at", "Keep NPS at", STRETCH_NPS, 0),
        int("n04.limit", "Limit (notes per second)", 1, 10_000_000, 1000),
        choice("n04.keep", "Keep the", STRETCH_KEEP, 0),
        flag("n05", "Rest stretch", false),
        ticks("n05.rest", "Phrase split rest (ticks)", 1, 1 << 30, 480),
        float("n05.factor", "Rest factor", 0.0, 1000.0, 200.0, "%"),
        flag("n06", "Groove preserve", false),
        choice("n06.grid", "Grid", art_grid_values(), 4),
        float("n06.keep", "Keep", 0.0, 100.0, 100.0, "%"),
        flag("n07", "Tape coupling", false),
        float("n07.semis", "Semitones per doubling", -24.0, 24.0, 12.0, ""),
        flag("n08", "Tape stop", false),
        ticks("n08.len", "Stop length (ticks)", 1, 1 << 30, 1920),
        int("n08.drop", "Pitch drop (semitones)", 0, 48, 12),
        curve(
            "n08.curve",
            "Slow-down",
            Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
        ),
        flag("n09", "Per-bar factors", false),
        list(
            "n09.factors",
            "Factors per bar (%)",
            10,
            1000,
            256,
            &[100, 50],
        ),
        flag("n10", "Asymmetric stretch", false),
        float("n10.before", "Factor before", 1.0, 1000.0, 150.0, "%"),
        float("n10.after", "Factor after", 1.0, 1000.0, 75.0, "%"),
        flag("n11", "Collision cap", false),
        choice("n11.at", "Cap at", STRETCH_CAP, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Clamp));
    with_help(&mut p, STRETCH_HELP);
    ToolSpec {
        id: "stretch",
        name: "Score Stretch",
        family: Family::Interactive,
        summary: "Stretches or compresses the selection in time around a pivot (starts and lengths together).",
        params: p,
        advanced: vec![
            "Fit to a target length",
            "Pivot",
            "Stretch lengths only",
            "Stretch onsets only",
            "Nonlinear stretch curve",
            "Per-subchannel factors",
            "Snap stretched onsets to the grid",
            "Tempo-preserving (stretch in seconds)",
            "Segment stretch",
            "Proportional pitch stretch",
        ],
        // Orders = the catalog's (§235) = where each runs in `Stretch9`: the
        // factor (n01), the time map (n05 rests, n03 anchors, n09 bars or
        // n10 two factors — the first on; else v0.8's), the starts (n06),
        // the lengths (n02, after v0.8's a3 / a4), the keys (n07), the end
        // (n08), the caps (n11) and the thinning (n04).
        functions: [
            ("n01", "Meter conversion", 110),
            ("n02", "Short-note keep", 170),
            ("n03", "Anchor map", 130),
            ("n04", "NPS-safe compression", 210),
            ("n05", "Rest stretch", 120),
            ("n06", "Groove preserve", 160),
            ("n07", "Tape coupling", 180),
            ("n08", "Tape stop", 190),
            ("n09", "Per-bar factors", 140),
            ("n10", "Asymmetric stretch", 150),
            ("n11", "Collision cap", 200),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

fn stretch_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S39 step 8: any new function, or Control Sweeps (evaluated per
    // note inside the kernel) — the v0.9 kernel (`tools/stretch.rs`); else
    // the v0.8 one.
    let new = StretchNew::from(p, b.ctx);
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Stretch9::build(p, b, new);
    }
    let s0 = if b.stats.is_empty() {
        0
    } else {
        b.stats.min_start
    };
    let e0 = b.stats.max_end.max(s0.saturating_add(1));
    let mut f = p.float("factor") / 100.0;
    if p.adv(1) {
        f = p.ticks("a1.len") as f64 / (e0 - s0) as f64;
    }
    let pivot = if p.adv(2) {
        match p.choice("a2.pivot") {
            0 => s0 as f64,
            1 => e0 as f64,
            3 => b.ctx.cursor as f64,
            _ => (s0 as f64 + e0 as f64) / 2.0,
        }
    } else {
        s0 as f64
    };
    let gates_only = p.adv(3);
    let onsets_only = p.adv(4);
    let curve = p.adv(5).then(|| p.curve("a5.curve"));
    let factors = if p.adv(6) {
        p.list("a6.factors")
    } else {
        Vec::new()
    };
    let snap = p.adv(7).then_some(b.ctx.snap.max(1));
    let seconds = p.adv(8);
    let segment = p.adv(9).then(|| {
        let (a, c) = (p.ticks("a9.from"), p.ticks("a9.to"));
        (a.min(c), a.max(c).max(a.min(c) + 1))
    });
    let pitch_f = p.adv(10).then(|| p.float("a10.factor") / 100.0);
    let ctx = b.ctx.clone();
    let (lo, hi) = (b.stats.min_pitch, b.stats.max_pitch);
    let subs: Vec<Vec<(u32, u8)>> = b.tracks.iter().map(|t| t.subs.clone()).collect();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let map = |t: f64, f: f64| -> f64 {
                if let Some((a, c)) = segment {
                    let (a, c) = (a as f64, c as f64);
                    return if t <= a {
                        t
                    } else if t <= c {
                        a + (t - a) * f
                    } else {
                        a + (c - a) * f + (t - c)
                    };
                }
                if let Some(cv) = &curve {
                    let u = ((t - s0 as f64) / (e0 - s0) as f64).clamp(0.0, 1.0);
                    return s0 as f64 + cv.eval(u) * (e0 - s0) as f64 * f;
                }
                if seconds {
                    let ps = ctx.seconds_at(pivot.max(0.0) as u64);
                    let ts = ctx.seconds_at(t.max(0.0) as u64);
                    return ctx.tick_at_seconds(ps + (ts - ps) * f) as f64;
                }
                pivot + (t - pivot) * f
            };
            for n in &src.notes {
                let mut fx = f;
                if !factors.is_empty() {
                    let si = subs
                        .get(usize::from(n.track))
                        .and_then(|t| t.iter().position(|x| x.0 == n.sub))
                        .unwrap_or(0);
                    fx = factors[si % factors.len()] as f64 / 100.0;
                }
                let (s, e) = (n.start as f64, n.end() as f64);
                let (mut ns, mut ne) = (map(s, fx), map(e, fx));
                if gates_only {
                    ns = s;
                    ne = s + (e - s) * fx;
                } else if onsets_only {
                    ne = ns + (e - s);
                }
                if let Some(g) = snap {
                    let q = (ns / g as f64).round() * g as f64;
                    ne += q - ns;
                    ns = q;
                }
                let pitch = match pitch_f {
                    Some(pf) => {
                        let c = (f64::from(lo) + f64::from(hi)) / 2.0;
                        (c + (f64::from(n.pitch) - c) * pf).round() as i64
                    }
                    None => i64::from(n.pitch),
                };
                out.emit(
                    ns.round() as i128,
                    (ne - ns).round() as i128,
                    pitch,
                    i64::from(n.vel),
                    n,
                );
                out.check()?;
            }
            Ok(())
        },
        growth: Growth::Map,
        estimate: None,
    })
}
