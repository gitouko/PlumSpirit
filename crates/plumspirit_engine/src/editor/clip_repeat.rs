//! Paste Repeat on the selected clips (v0.9 V09-S13 / S15, contract §088 –
//! §091, §094, §219, §257; `docs/PASTE_REPEAT.md`): the clip adapter of the
//! shared timing core (`transform/repeat.rs`). Every selected clip stays and
//! gets one copy per repeat *k* on its own row at `start + offset(k)` —
//! linked (the same content: no note is copied) or independent (a new owned
//! content whose chunks are shared until either side changes) — as one
//! `SetClips` undo step. The anchor is the first selected clip's start, the
//! span reaches the last one's end; musical and physical units are song
//! time.
//!
//! S15 adds the ten clip functions (§094), each off by default (all off:
//! exactly S13). Their parameters are a [`ParamSet`] of the schema
//! [`params`] (the dialog draws it with the tool windows' rows). The plan
//! ([`plan_clip_repeat`]) lists every new placement — what the dialog
//! previews — and the content keys: a generation whose notes differ from its
//! source's (a transpose, a subchannel remap) plays a **derived content**
//! made once per distinct (content, transpose, remap) key at Apply
//! ([`apply_clip_repeat`]); its linked copies share it, independent ones
//! share its chunks. The source is never rewritten.

use super::clips::{
    ClipEditError, Planned, clip_span, copy_content, new_clip_id, new_content_id, placement,
    set_clips,
};
use crate::model::project::KeyRange;
use crate::model::{ClipId, ClipPlacement, ContentRef, OwnedContent, PlumDocument, TempoMap};
use crate::transform::TEMPO_RATE;
use crate::transform::param::*;
use crate::transform::repeat::series::{
    Morph, copies_with, cumulative, fracture, picked, tree, tree_blocks,
};
use crate::transform::repeat::{RepeatClock, RepeatPlan, RepeatSource, RepeatSpec, plan};
use crate::transform::rng::{Rng, hash, salt};
use crate::view::grid::MeterMap;
use std::collections::{BTreeSet, HashMap};

/// Most clips one Paste Repeat may add (copies × selected clips).
pub const MAX_NEW_CLIPS: u64 = 1_000_000;

/// Most notes the derived contents of one Paste Repeat may hold (the
/// transform platform's cap).
pub const MAX_DERIVED_NOTES: u64 = 50_000_000;

/// The schema's id (the tool catalog's `paste_repeat_clips`).
pub const TOOL_ID: &str = "paste_repeat_clips";

/// The ten functions (§094), function 1 first — the catalog's names.
pub const FUNCTIONS: [&str; 10] = [
    "Track/Subtrack Lane Offset",
    "Temporal Acceleration",
    "Gap/Overlap Morph",
    "Linked-vs-Independent Repeat Mode",
    "Pitch Transpose per Clip Generation",
    "Subchannel Remap per Generation",
    "Stack Mosaic",
    "Ping-Pong Clip Order",
    "Seeded Clip Omission/Mutation",
    "Recursive Clip Pattern",
];

/// What function 1 does past the first or last track.
pub const LANE_EDGES: [&str; 3] = [
    "Refuse",
    "Wrap around the tracks",
    "Stop at the first / last track",
];

/// How a morph (function 3) runs over the series.
pub const MORPH_MODES: [&str; 2] = ["Start → end", "Graph"];

/// Function 4's per-generation patterns.
pub const LINK_PATTERNS: [&str; 3] = [
    "Alternate: odd generations independent",
    "Every n-th generation independent",
    "New master: the first copy independent, the rest linked to it",
];

/// Function 6's modes.
pub const REMAP_MODES: [&str; 2] = [
    "Rotate within the list",
    "Whole clip to the list's next entry",
];

/// Function 7's patterns.
pub const MOSAIC_PATTERNS: [&str; 2] = ["Full", "Checkerboard"];

/// What function 8 mirrors.
pub const PING_PONG_MODES: [&str; 3] = ["Time order", "Track order", "Both"];

/// What function 9 does with the generations its graph picks.
pub const OMIT_MODES: [&str; 3] = ["Omit", "Mutate", "Omit and mutate"];

/// The functions' parameters (switches `a1` … `a10`; the dialog's
/// Advanced section). Every function is off by default.
pub fn params() -> Vec<ParamSpec> {
    vec![
        // 1 — Track / Subtrack Lane Offset.
        adv_switch(1, FUNCTIONS[0]),
        int("a1.lanes", "Tracks per copy", -64, 64, 1)
            .adv(1)
            .help("Copy k of a clip lands k × this many tracks down (negative: up), in the track list's order (subtracks included)."),
        choice("a1.edge", "Past the first / last track", &LANE_EDGES, 0).adv(1),
        // 2 — Temporal Acceleration.
        adv_switch(2, FUNCTIONS[1]),
        curve(
            "a2.curve",
            "Spacing over the series (middle: as planned)",
            Curve::new(vec![(0.0, 0.5), (1.0, 0.5)]),
        )
        .adv(2)
        .help("x: the copy's place in the series; y: its gap to the previous copy — 0 none, the middle 100 %, the top the maximum."),
        float("a2.max", "Spacing at the top of the graph", 100.0, 1000.0, 200.0, "%").adv(2),
        // 3 — Gap / Overlap Morph.
        adv_switch(3, FUNCTIONS[2]),
        choice("a3.mode", "Gap over the series", &MORPH_MODES, 0).adv(3),
        float("a3.start", "Extra gap before copy 1", -95.0, 400.0, 0.0, "% of the block")
            .adv(3),
        float("a3.end", "Extra gap before the last copy", -95.0, 400.0, -50.0, "% of the block")
            .adv(3)
            .help("Negative: the copies overlap (−50 %: by half a block)."),
        curve(
            "a3.curve",
            "Graph: start (bottom) → end (top)",
            Curve::new(vec![(0.0, 0.0), (1.0, 1.0)]),
        )
        .adv(3),
        // 4 — Linked vs Independent repeat mode.
        adv_switch(4, FUNCTIONS[3]),
        choice("a4.pattern", "Per generation", &LINK_PATTERNS, 0)
            .adv(4)
            .help("Overrides Linked / Independent for each generation."),
        int("a4.every", "Every n-th (from copy 1)", 2, 16, 2).adv(4),
        // 5 — Pitch Transpose per Clip Generation.
        adv_switch(5, FUNCTIONS[4]),
        int("a5.step", "Semitones per generation", -127, 127, 12)
            .keyed()
            .adv(5),
        list(
            "a5.seq",
            "Interval sequence (cycled; empty: the step)",
            -127,
            127,
            64,
            &[],
        )
        .adv(5)
        .help("Generation k moves by the first k intervals added up. The source clip is never changed; each transposed generation gets its own content."),
        // 6 — Subchannel Remap per Generation.
        adv_switch(6, FUNCTIONS[5]),
        choice("a6.mode", "Remap", &REMAP_MODES, 0).adv(6),
        list("a6.list", "Subchannels (of the clip's track)", 0, 4095, 64, &[0, 1, 2, 3])
            .adv(6)
            .help("Rotate: a note on the list's i-th subchannel goes to entry i + k × step of generation k (others keep theirs). Whole clip: every note of generation k goes to entry k. The MIDI channel never changes."),
        int("a6.step", "Entries per generation (Rotate)", -64, 64, 1).adv(6),
        // 7 — Stack Mosaic.
        adv_switch(7, FUNCTIONS[6]),
        int("a7.rows", "Rows of tracks", 2, 64, 2).adv(7),
        int("a7.step", "Tracks between rows (0: the block's height)", -64, 64, 0).adv(7),
        choice("a7.pattern", "Cells", &MOSAIC_PATTERNS, 0)
            .adv(7)
            .help("Checkerboard keeps the cells whose copy and row numbers add up to an even number."),
        float("a7.stagger", "Each row starts later by", 0.0, 100.0, 0.0, "% of the spacing").adv(7),
        // 8 — Ping-Pong Clip Order.
        adv_switch(8, FUNCTIONS[7]),
        choice("a8.mode", "Reverse", &PING_PONG_MODES, 0)
            .adv(8)
            .help("On the generations it reverses, the selected clips change places inside the block (their notes are not reversed)."),
        int("a8.every", "On every n-th generation (from copy 1)", 2, 16, 2).adv(8),
        // 9 — Seeded Clip Omission / Mutation.
        adv_switch(9, FUNCTIONS[8]),
        choice("a9.mode", "Picked generations are", &OMIT_MODES, 0).adv(9),
        curve(
            "a9.curve",
            "Chance over the series",
            Curve::new(vec![(0.0, 0.25), (1.0, 0.25)]),
        )
        .adv(9)
        .help("x: the generation's place in the series; y: the chance it is picked (whole generations)."),
        seed("a9.seed").adv(9),
        flag("a9.mute", "Mutation: mute", true).adv(9),
        flag("a9.transpose", "Mutation: transpose", false).adv(9),
        int("a9.range", "Transpose within ± semitones", 1, 48, 12).adv(9),
        flag("a9.reverse", "Mutation: reverse the clip order", false).adv(9),
        // 10 — Recursive Clip Pattern.
        adv_switch(10, FUNCTIONS[9]),
        int("a10.depth", "Depth", 1, 4, 2).adv(10),
        int("a10.children", "Copies per block", 1, 16, 2).adv(10),
        float("a10.ratio", "Child spacing", 5.0, 100.0, 50.0, "% of the parent's").adv(10),
    ]
}

/// A selected clip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceClip {
    pub placement: ClipPlacement,
    /// Its track's index (row).
    pub row: usize,
    /// Song ticks it occupies.
    pub span: (u64, u64),
}

/// How a new clip gets its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// The source's content, or its generation's derived content.
    Shared,
    /// An independent copy of it.
    Own,
    /// The content new clip *j* (earlier in the list) got.
    Same(u32),
}

/// A subchannel remap (function 6): logical subchannel ids, resolved from
/// the list's positions in the source clip's track.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Remap {
    /// Every note goes to this subchannel (Whole clip).
    pub all: Option<u32>,
    /// From → to, sorted by `from` (Rotate; other ids keep theirs).
    pub pairs: Vec<(u32, u32)>,
}

impl Remap {
    /// The subchannel a note on `sub` goes to.
    pub fn of(&self, sub: u32) -> u32 {
        if let Some(a) = self.all {
            return a;
        }
        match self.pairs.binary_search_by_key(&sub, |p| p.0) {
            Ok(i) => self.pairs[i].1,
            Err(_) => sub,
        }
    }
}

/// A content a generation plays instead of its source's (built at Apply).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentKey {
    pub content: ContentRef,
    /// Semitones.
    pub transpose: i32,
    /// Index into [`ClipRepeatPlan::remaps`] (`None`: subchannels kept).
    pub remap: Option<u32>,
}

/// One clip the repeat adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewClip {
    /// Index into [`ClipRepeatPlan::sources`].
    pub source: u32,
    /// The track index it lands on.
    pub row: u32,
    pub start: u64,
    /// The generation it plays (1 … N; 0: the source's own column of a
    /// mosaic).
    pub generation: u32,
    /// Index into [`ClipRepeatPlan::keys`] (`None`: the source's content).
    pub key: Option<u32>,
    pub link: Link,
    pub muted: bool,
}

/// Everything Apply will add, planned without touching notes.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipRepeatPlan {
    /// The core's plan (the first-level offsets).
    pub base: RepeatPlan,
    pub sources: Vec<SourceClip>,
    /// In creation order (the new ids follow it).
    pub clips: Vec<NewClip>,
    pub keys: Vec<ContentKey>,
    pub remaps: Vec<Remap>,
    /// Notes the derived contents will hold.
    pub derived_notes: u64,
    /// Notes left out of derived contents (outside the keys).
    pub left_out_notes: u64,
    /// Copies left out because every note of theirs fell outside the keys.
    pub empty_copies: u64,
    /// The generations (first level, before omission).
    pub generations: u32,
    /// Generations omitted (function 9).
    pub omitted: u32,
    /// Generations mutated (function 9).
    pub mutated: u32,
    /// The base choice (the undo label).
    pub linked: bool,
    /// The clip table's revision and the track count it was planned for.
    pub clip_revision: u64,
    pub track_count: usize,
}

impl ClipRepeatPlan {
    /// The song tick where the last new clip ends.
    pub fn end(&self) -> u64 {
        self.clips
            .iter()
            .map(|c| {
                let s = &self.sources[c.source as usize];
                c.start.saturating_add(s.span.1 - s.span.0)
            })
            .max()
            .unwrap_or(0)
    }
}

/// The selected clips (each once) with their song spans, the repeat source
/// (anchor = first start, end = last end, `snap`).
fn sources(
    doc: &PlumDocument,
    ids: &[ClipId],
    snap: Option<u64>,
) -> Result<(Vec<SourceClip>, RepeatSource), ClipEditError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let (mut lo, mut hi) = (u64::MAX, 0u64);
    for &id in ids {
        if !seen.insert(id) {
            continue;
        }
        let p = placement(doc, id)?;
        let row = doc
            .index_of(p.track)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        let span = clip_span(doc, &p);
        lo = lo.min(span.0);
        hi = hi.max(span.1);
        out.push(SourceClip {
            placement: p,
            row,
            span,
        });
    }
    if out.is_empty() {
        return Err(ClipEditError("Select the clips to paste again.".into()));
    }
    Ok((
        out,
        RepeatSource {
            start: lo,
            end: hi,
            snap,
        },
    ))
}

fn too_many(n: u64) -> ClipEditError {
    ClipEditError(format!(
        "This would add {} clips; at most {} can be added at once.",
        crate::midi::group_digits(n),
        crate::midi::group_digits(MAX_NEW_CLIPS)
    ))
}

/// Where the copies of clips `ids` go for `spec` (`snap`: the Track List's
/// snap unit). Pure; `Err` is the dialog's sentence.
pub fn plan_repeat_clips(
    doc: &PlumDocument,
    ids: &[ClipId],
    spec: &RepeatSpec,
    snap: Option<u64>,
) -> Result<RepeatPlan, ClipEditError> {
    let (srcs, src) = sources(doc, ids, snap)?;
    let n = u64::from(spec.count).saturating_mul(srcs.len() as u64);
    if n > MAX_NEW_CLIPS {
        return Err(ClipEditError(format!(
            "{} copies of {} clips would add {} clips; at most {} can be added at once.",
            crate::midi::group_digits(u64::from(spec.count)),
            crate::midi::group_digits(srcs.len() as u64),
            crate::midi::group_digits(n),
            crate::midi::group_digits(MAX_NEW_CLIPS)
        )));
    }
    let meter = MeterMap::new(doc.division(), &doc.meter_events(srcs[0].row));
    let tempo = TempoMap::for_document(doc, TEMPO_RATE);
    let clock = RepeatClock {
        meter: &meter,
        tempo: Some(&tempo),
        ppq: doc.division().musical_ppqn().unwrap_or(960).max(1),
        shift: 0,
    };
    plan(spec, src, &clock).map_err(ClipEditError)
}

/// Function 9's settings.
struct Chance {
    curve: Curve,
    seed: u64,
    omit: bool,
    mutate: bool,
    mute: bool,
    /// Transposing mutation: ± this many semitones at most.
    transpose: Option<i64>,
    reverse: bool,
}

/// What function 9 does to one generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mutation {
    mute: bool,
    transpose: i64,
    reverse: bool,
}

/// The settings the functions read, once per plan.
struct Functions {
    lanes: Option<(i64, usize)>,
    accel: Option<(Curve, f64)>,
    morph: Option<Morph>,
    /// Function 4: (pattern, every n).
    pattern: Option<(usize, u32)>,
    transpose: Option<(i64, Vec<i64>)>,
    /// Function 6: (whole clip, positions, step).
    remap: Option<(bool, Vec<i64>, i64)>,
    /// Function 7: (rows, track step, checkerboard, stagger as a fraction).
    mosaic: Option<(u32, i64, bool, f64)>,
    /// Function 8: (every n, time order, track order).
    ping_pong: Option<(u32, bool, bool)>,
    chance: Option<Chance>,
    /// Function 10: (depth, children, ratio).
    tree: Option<(u32, u32, f64)>,
}

impl Functions {
    fn read(p: &Params<'_>) -> Functions {
        Functions {
            lanes: p.adv(1).then(|| (p.int("a1.lanes"), p.choice("a1.edge"))),
            accel: p
                .adv(2)
                .then(|| (p.curve("a2.curve"), p.float("a2.max") / 100.0)),
            morph: p.adv(3).then(|| Morph {
                start: p.float("a3.start") / 100.0,
                end: p.float("a3.end") / 100.0,
                curve: (p.choice("a3.mode") == 1).then(|| p.curve("a3.curve")),
            }),
            pattern: p.adv(4).then(|| {
                (
                    p.choice("a4.pattern"),
                    p.int("a4.every").clamp(2, 16) as u32,
                )
            }),
            transpose: p.adv(5).then(|| (p.int("a5.step"), p.list("a5.seq"))),
            remap: p.adv(6).then(|| {
                (
                    p.choice("a6.mode") == 1,
                    p.list("a6.list"),
                    p.int("a6.step"),
                )
            }),
            mosaic: p.adv(7).then(|| {
                (
                    p.int("a7.rows").clamp(2, 64) as u32,
                    p.int("a7.step"),
                    p.choice("a7.pattern") == 1,
                    p.float("a7.stagger").clamp(0.0, 100.0) / 100.0,
                )
            }),
            ping_pong: p.adv(8).then(|| {
                let m = p.choice("a8.mode");
                (p.int("a8.every").clamp(2, 16) as u32, m != 1, m != 0)
            }),
            chance: p.adv(9).then(|| {
                let m = p.choice("a9.mode");
                Chance {
                    curve: p.curve("a9.curve"),
                    seed: p.seed("a9.seed"),
                    omit: m != 1,
                    mutate: m != 0,
                    mute: p.flag("a9.mute"),
                    transpose: p
                        .flag("a9.transpose")
                        .then(|| p.int("a9.range").clamp(1, 48)),
                    reverse: p.flag("a9.reverse"),
                }
            }),
            tree: p.adv(10).then(|| {
                (
                    p.int("a10.depth").clamp(1, 4) as u32,
                    p.int("a10.children").clamp(1, 16) as u32,
                    p.float("a10.ratio") / 100.0,
                )
            }),
        }
    }
}

/// Function 9's mutation of generations 1 … `n` (index k − 1): a picked
/// generation gets one of the enabled kinds, drawn from (seed, k).
fn mutations(c: &Chance, n: usize) -> Vec<Mutation> {
    let mut kinds = Vec::new();
    if c.mute {
        kinds.push(0u8);
    }
    if c.transpose.is_some() {
        kinds.push(1);
    }
    if c.reverse {
        kinds.push(2);
    }
    (1..=n as u32)
        .map(|k| {
            if !c.mutate || kinds.is_empty() || !picked(c.seed, salt::MUTATE, k, n, &c.curve) {
                return Mutation::default();
            }
            let mut r = Rng::new(hash(&[c.seed, salt::MUTATE, u64::from(k), 1]));
            let kind = kinds[(r.next_u64() % kinds.len() as u64) as usize];
            match kind {
                0 => Mutation {
                    mute: true,
                    ..Mutation::default()
                },
                1 => {
                    let a = r.range_i64(1, c.transpose.unwrap_or(1));
                    let sign = if r.next_u64() & 1 == 0 { 1 } else { -1 };
                    Mutation {
                        transpose: a * sign,
                        ..Mutation::default()
                    }
                }
                _ => Mutation {
                    reverse: true,
                    ..Mutation::default()
                },
            }
        })
        .collect()
}

/// The track a clip on `row` lands on after moving `delta` tracks (`n`
/// tracks), by function 1's edge rule (Refuse when function 1 is off).
fn lane(row: usize, delta: i64, n: usize, edge: usize, k: u32) -> Result<usize, ClipEditError> {
    let to = (row as i64).saturating_add(delta);
    let last = n as i64 - 1;
    if (0..=last).contains(&to) {
        return Ok(to as usize);
    }
    match edge {
        1 => Ok(to.rem_euclid(n as i64) as usize),
        2 => Ok(to.clamp(0, last) as usize),
        // A mosaic row (k = 0) names its own settings first: with Lane
        // Offset off its edge rule is not on screen (GUI journey M2, S15).
        _ if k == 0 => Err(ClipEditError(format!(
            "A mosaic row would move a clip from track {} past the {} track; use fewer rows or a {} row step (Stack Mosaic), or turn on Track/Subtrack Lane Offset and let the copies wrap around or stop there.",
            row + 1,
            if to < 0 { "first" } else { "last" },
            if to < 0 { "positive" } else { "negative" }
        ))),
        _ => Err(ClipEditError(format!(
            "Copy {k} would move a clip from track {} past the {} track; lower the track offsets, or let the copies wrap around or stop there (Track/Subtrack Lane Offset).",
            row + 1,
            if to < 0 { "first" } else { "last" }
        ))),
    }
}

/// Pitch histogram of a content's notes (for the derived contents' counts).
fn histogram(doc: &PlumDocument, c: ContentRef) -> [u64; 256] {
    let mut h = [0u64; 256];
    let notes = match c {
        ContentRef::Track(t) => doc.track_by_id(t).map(|t| &t.notes),
        ContentRef::Owned(id) => doc.clips().content(id).map(|c| &c.notes),
    };
    if let Some(n) = notes {
        for v in n.iter() {
            h[usize::from(v.attrs.pitch())] += 1;
        }
    }
    h
}

/// The notes of histogram `h` that stay inside `keys` moved by `t`.
fn inside(h: &[u64; 256], t: i64, keys: KeyRange) -> u64 {
    let max = i64::from(keys.max_pitch());
    (0..256i64)
        .filter(|&p| (0..=max).contains(&(p + t)))
        .map(|p| h[p as usize])
        .sum()
}

/// Function 6's subchannel ids of `list` (positions) in track `row`'s
/// table; positions the track lacks are left out (each track cycles
/// through the listed subchannels it has).
fn remap_ids(doc: &PlumDocument, row: usize, list: &[i64]) -> Vec<u32> {
    let table = doc
        .track(row)
        .map(|t| t.subchannels().entries())
        .unwrap_or(&[]);
    list.iter()
        .filter_map(|&p| usize::try_from(p).ok().and_then(|i| table.get(i)))
        .map(|e| e.id.0)
        .collect()
}

/// Generation `k`'s remap of subchannel ids `ids` (`None`: unchanged).
fn remap_of(ids: &[u32], whole: bool, step: i64, k: u32) -> Option<Remap> {
    if ids.is_empty() || k == 0 {
        return None;
    }
    let len = ids.len();
    if whole {
        return Some(Remap {
            all: Some(ids[(k as usize - 1) % len]),
            pairs: Vec::new(),
        });
    }
    let rot = (i64::from(k).saturating_mul(step)).rem_euclid(len as i64) as usize;
    let mut pairs: Vec<(u32, u32)> = Vec::new();
    for i in 0..len {
        let (from, to) = (ids[i], ids[(i + rot) % len]);
        if !pairs.iter().any(|p| p.0 == from) {
            pairs.push((from, to));
        }
    }
    pairs.retain(|p| p.0 != p.1);
    pairs.sort_unstable();
    (!pairs.is_empty()).then_some(Remap { all: None, pairs })
}

/// The plan of Paste Repeat for clips `ids`: the core's plan for `spec`,
/// then the functions of `fx` (schema [`params`]) on top. Pure; `Err` is
/// the dialog's sentence.
pub fn plan_clip_repeat(
    doc: &PlumDocument,
    ids: &[ClipId],
    spec: &RepeatSpec,
    snap: Option<u64>,
    linked: bool,
    fx: &ParamSet,
) -> Result<ClipRepeatPlan, ClipEditError> {
    let base = plan_repeat_clips(doc, ids, spec, snap)?;
    build_plan(doc, ids, base, linked, fx)
}

/// One column of the arrangement: a block of the selected clips at `off`.
#[derive(Clone, Copy, Debug)]
struct Column {
    /// The generation (0: the source's own column of a mosaic).
    k: u32,
    off: u64,
    gap: u64,
}

/// The plan for the core's `base` plan and the functions of `fx` — in the
/// order of `docs/PASTE_REPEAT.md`: time (2, 3), omission / mutation (9),
/// recursion (10), mosaic (7), then per clip ping-pong (8), tracks (1), the
/// content key (5, 6, 9) and linked / independent (4).
pub fn build_plan(
    doc: &PlumDocument,
    ids: &[ClipId],
    base: RepeatPlan,
    linked: bool,
    fx: &ParamSet,
) -> Result<ClipRepeatPlan, ClipEditError> {
    let (srcs, src) = sources(doc, ids, None)?;
    let specs = params();
    let keys_range = doc.project().keys;
    let p = Params::new(&specs, fx).with_keys(keys_range);
    let f = Functions::read(&p);
    let (lo, hi) = (src.start, src.end);
    let span = hi.saturating_sub(lo);
    let n_src = srcs.len() as u64;
    let too_many_sources = |blocks: u64| -> Result<(), ClipEditError> {
        let t = blocks.saturating_mul(n_src);
        if t > MAX_NEW_CLIPS {
            Err(too_many(t))
        } else {
            Ok(())
        }
    };
    // 1. Time: the core's offsets, (2) acceleration, (3) morph.
    let mut slots = copies_with(
        &base.offsets,
        f.accel.as_ref().map(|(c, m)| (c, *m)),
        f.morph.as_ref().map(|m| (m, span)),
    )
    .map_err(ClipEditError)?;
    let generations = slots.len();
    too_many_sources(generations as u64)?;
    // 2. (9) Omission and mutation of whole generations.
    let mut muts = vec![Mutation::default(); generations];
    if let Some(c) = &f.chance {
        if c.omit {
            fracture(&mut slots, &c.curve, c.seed, true, false);
        }
        muts = mutations(c, generations);
    }
    let omitted = (generations - slots.len()) as u32;
    let mutated = slots
        .iter()
        .filter(|s| muts[s.k as usize - 1] != Mutation::default())
        .count() as u32;
    // 3. (10) The recursion: counted before it is built.
    if let Some((depth, children, ratio)) = f.tree {
        too_many_sources((slots.len() as u64).saturating_mul(tree_blocks(depth, children)))?;
        slots = tree(&slots, depth, children, ratio, MAX_NEW_CLIPS).map_err(ClipEditError)?;
    }
    // 4. (7) The mosaic's columns (the source's own column first) and rows.
    let mut columns: Vec<Column> = Vec::with_capacity(slots.len() + 1);
    if f.mosaic.is_some() {
        columns.push(Column {
            k: 0,
            off: 0,
            gap: slots.first().map_or(span.max(1), |s| s.gap),
        });
    }
    columns.extend(slots.iter().map(|s| Column {
        k: s.k,
        off: s.off,
        gap: s.gap,
    }));
    let (rows, row_step, checker, stagger) = match f.mosaic {
        None => (1u32, 0i64, false, 0.0),
        Some((r, st, ch, sg)) => {
            let (rmin, rmax) = srcs
                .iter()
                .fold((usize::MAX, 0), |(a, b), s| (a.min(s.row), b.max(s.row)));
            let height = (rmax - rmin + 1) as i64;
            (r, if st == 0 { height } else { st }, ch, sg)
        }
    };
    let cell = |k: u32, r: u32| !(k == 0 && r == 0 || checker && (k + r) % 2 == 1);
    let cells: u64 = columns
        .iter()
        .map(|c| (0..rows).filter(|&r| cell(c.k, r)).count() as u64)
        .sum();
    too_many_sources(cells)?;
    let total = cells.saturating_mul(n_src);
    let tracks = doc.track_count();
    let (row_min, row_max) = srcs
        .iter()
        .fold((usize::MAX, 0), |(a, b), s| (a.min(s.row), b.max(s.row)));
    let edge = f.lanes.map_or(0, |l| l.1);
    // Function 6's ids per source row, checked once.
    let mut remap_rows: HashMap<usize, Vec<u32>> = HashMap::new();
    if let Some((whole, list, _)) = &f.remap {
        for s in &srcs {
            remap_rows
                .entry(s.row)
                .or_insert_with(|| remap_ids(doc, s.row, list));
        }
        let need = if *whole { 1 } else { 2 };
        if remap_rows.values().all(|ids| ids.len() < need) {
            return Err(ClipEditError(format!(
                "The selected clips' tracks have {} of the listed subchannels, so Subchannel Remap per Generation would change nothing; list subchannels those tracks have.",
                if *whole { "none" } else { "fewer than two" }
            )));
        }
    }
    let mut out = ClipRepeatPlan {
        base,
        sources: Vec::new(),
        clips: Vec::with_capacity(total as usize),
        keys: Vec::new(),
        remaps: Vec::new(),
        derived_notes: 0,
        left_out_notes: 0,
        empty_copies: 0,
        generations: generations as u32,
        omitted,
        mutated,
        linked,
        clip_revision: doc.clips().revision(),
        track_count: tracks,
    };
    let mut key_of: HashMap<ContentKey, u32> = HashMap::new();
    let mut remap_index: HashMap<Remap, u32> = HashMap::new();
    let mut remap_cache: HashMap<(usize, u32), Option<u32>> = HashMap::new();
    let mut hist: HashMap<ContentRef, [u64; 256]> = HashMap::new();
    // New master (function 4): the first copy of each (source, key).
    let mut master: HashMap<(u32, Option<u32>), u32> = HashMap::new();
    for col in &columns {
        let k = col.k;
        let m = if k == 0 {
            Mutation::default()
        } else {
            muts[k as usize - 1]
        };
        let shift = f
            .transpose
            .as_ref()
            .map_or(0, |(step, seq)| cumulative(*step, seq, k))
            .saturating_add(m.transpose);
        let (rev_time, rev_rows) = match f.ping_pong {
            Some((every, t, r)) if k >= 1 && (k - 1).is_multiple_of(every) => (t, r),
            _ => (false, false),
        };
        let (rev_time, rev_rows) = if m.reverse {
            // The mutation reverses what ping-pong would (time order when
            // ping-pong is off); twice is no reversal.
            let (pt, pr) = f.ping_pong.map_or((true, false), |p| (p.1, p.2));
            (rev_time ^ pt, rev_rows ^ pr)
        } else {
            (rev_time, rev_rows)
        };
        let own = match f.pattern {
            Some((0, _)) if k >= 1 => k % 2 == 1,
            Some((1, n)) if k >= 1 => (k - 1).is_multiple_of(n),
            Some((0 | 1, _)) | None => !linked,
            Some(_) => false,
        };
        for r in 0..rows {
            if !cell(k, r) {
                continue;
            }
            let lag = (f64::from(r) * stagger * col.gap as f64).round() as u64;
            for (si, s) in srcs.iter().enumerate() {
                let rel = if rev_time {
                    lo + (hi - s.span.1)
                } else {
                    s.span.0
                };
                let start = rel
                    .checked_add(col.off)
                    .and_then(|x| x.checked_add(lag))
                    .filter(|&st| st.checked_add(s.span.1 - s.span.0).is_some())
                    .ok_or_else(|| ClipEditError("A clip cannot be placed that far.".into()))?;
                let base_row = if rev_rows {
                    row_min + row_max - s.row
                } else {
                    s.row
                };
                let delta = f
                    .lanes
                    .map_or(0, |l| l.0.saturating_mul(i64::from(k)))
                    .saturating_add(row_step.saturating_mul(i64::from(r)));
                let row = if delta == 0 {
                    base_row
                } else {
                    lane(base_row, delta, tracks, edge, k)?
                };
                let remap = match &f.remap {
                    None => None,
                    Some((whole, _, step)) => {
                        let ids = &remap_rows[&s.row];
                        let len = ids.len().max(1) as u32;
                        let phase = if *whole {
                            k
                        } else {
                            (i64::from(k).saturating_mul(*step)).rem_euclid(i64::from(len)) as u32
                        };
                        *remap_cache.entry((s.row, phase)).or_insert_with(|| {
                            remap_of(ids, *whole, *step, k).map(|m| {
                                let next = out.remaps.len() as u32;
                                *remap_index.entry(m.clone()).or_insert_with(|| {
                                    out.remaps.push(m);
                                    next
                                })
                            })
                        })
                    }
                };
                let content = s.placement.content;
                let h = if shift == 0 && remap.is_none() {
                    None
                } else {
                    Some(
                        hist.entry(content)
                            .or_insert_with(|| histogram(doc, content)),
                    )
                };
                // An empty content needs no derived copy: it plays as it is.
                let key = match h {
                    None => None,
                    Some(h) if h.iter().all(|&c| c == 0) => None,
                    Some(h) => {
                        let all: u64 = h.iter().sum();
                        let stay = inside(h, shift, keys_range);
                        if stay == 0 && all > 0 {
                            out.empty_copies += 1;
                            continue;
                        }
                        let ck = ContentKey {
                            content,
                            transpose: shift.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                            remap,
                        };
                        Some(*key_of.entry(ck).or_insert_with(|| {
                            out.keys.push(ck);
                            out.derived_notes = out.derived_notes.saturating_add(stay);
                            out.left_out_notes = out.left_out_notes.saturating_add(all - stay);
                            (out.keys.len() - 1) as u32
                        }))
                    }
                };
                let index = out.clips.len() as u32;
                let link = match f.pattern {
                    Some((2, _)) => match master.get(&(si as u32, key)) {
                        Some(&j) => Link::Same(j),
                        None => {
                            master.insert((si as u32, key), index);
                            Link::Own
                        }
                    },
                    _ if own => Link::Own,
                    _ => Link::Shared,
                };
                out.clips.push(NewClip {
                    source: si as u32,
                    row: row as u32,
                    start,
                    generation: k,
                    key,
                    link,
                    muted: m.mute,
                });
            }
        }
    }
    if out.derived_notes > MAX_DERIVED_NOTES {
        return Err(ClipEditError(format!(
            "The transposed or remapped generations would hold {} notes; at most {} can be made at once. Repeat fewer times or transpose less.",
            crate::midi::group_digits(out.derived_notes),
            crate::midi::group_digits(MAX_DERIVED_NOTES)
        )));
    }
    out.sources = srcs;
    Ok(out)
}

/// The derived content of `key` with id `id`: a chunk-sharing copy of the
/// key's content, every note moved by the transpose (notes that would leave
/// the keys are left out) and its subchannel remapped (`remap`).
fn derive(
    doc: &PlumDocument,
    key: &ContentKey,
    remap: Option<&Remap>,
    id: crate::model::ContentId,
    keys: KeyRange,
) -> Result<OwnedContent, ClipEditError> {
    let mut c = copy_content(doc, key.content, id)?;
    let t = i64::from(key.transpose);
    if t != 0 {
        let max = i64::from(keys.max_pitch());
        let (lo, hi) = if t > 0 {
            ((max - t + 1).max(0), max)
        } else {
            (0, (-t - 1).min(max))
        };
        if lo <= hi {
            c.notes.select_in_region(0, u64::MAX, lo as u8, hi as u8);
            c.notes.remove_selected();
        }
        c.notes.select_all();
        c.notes.map_selected_attrs(|a| {
            a.with_pitch_wide((i64::from(a.pitch()) + t) as u8)
                .unwrap_or(a)
        });
        c.notes.clear_selection();
    }
    if let Some(m) = remap {
        c.notes.select_all();
        let subs = c.notes.selected_subs().map_err(|_| {
            ClipEditError("There is not enough memory to remap the subchannels.".into())
        })?;
        c.notes.map_selected_subs(|i| m.of(subs[i]));
        c.notes.clear_selection();
    }
    Ok(c)
}

/// One undo step adding every clip of `plan` (made by [`build_plan`] on this
/// document's clips): derived contents first built where a generation needs
/// one, then the placements in the plan's order (the new ids follow
/// `doc.clips().layout().next_clip`).
pub fn apply_clip_repeat(doc: &PlumDocument, plan: &ClipRepeatPlan) -> Planned {
    if doc.clips().revision() != plan.clip_revision || doc.track_count() != plan.track_count {
        return Err(ClipEditError(
            "The clips changed since this was planned; plan it again.".into(),
        ));
    }
    for s in &plan.sources {
        if doc.clips().placement(s.placement.id).as_ref() != Some(&s.placement) {
            return Err(ClipEditError(
                "The clips changed since this was planned; plan it again.".into(),
            ));
        }
    }
    let keys_range = doc.project().keys;
    let mut layout = doc.clips().layout();
    let mut contents: Vec<OwnedContent> = Vec::new();
    layout.placements.reserve(plan.clips.len());
    // Keys a shared placement plays: their derived content is placed as is
    // (independent copies clone it); other keys' first copy takes it.
    let shared: Vec<bool> = {
        let mut v = vec![false; plan.keys.len()];
        for c in &plan.clips {
            if let (Some(k), Link::Shared) = (c.key, c.link) {
                v[k as usize] = true;
            }
        }
        v
    };
    // Built derived contents: index into `contents`, and whether a copy
    // already owns it.
    let mut built: Vec<Option<(usize, bool)>> = vec![None; plan.keys.len()];
    let mut given: Vec<ContentRef> = Vec::with_capacity(plan.clips.len());
    for c in &plan.clips {
        let s = &plan.sources[c.source as usize];
        let content = match (c.key, c.link) {
            (_, Link::Same(j)) => given[j as usize],
            (None, Link::Shared) => s.placement.content,
            (None, Link::Own) => {
                let cid = new_content_id(&mut layout)?;
                contents.push(copy_content(doc, s.placement.content, cid)?);
                ContentRef::Owned(cid)
            }
            (Some(k), link) => {
                let k = k as usize;
                let base = match built[k] {
                    Some(b) => b,
                    None => {
                        let cid = new_content_id(&mut layout)?;
                        let key = &plan.keys[k];
                        let remap = key.remap.map(|r| &plan.remaps[r as usize]);
                        contents.push(derive(doc, key, remap, cid, keys_range)?);
                        let b = (contents.len() - 1, false);
                        built[k] = Some(b);
                        b
                    }
                };
                if link == Link::Shared || (!shared[k] && !base.1) {
                    if link == Link::Own {
                        built[k] = Some((base.0, true));
                    }
                    ContentRef::Owned(contents[base.0].id)
                } else {
                    let cid = new_content_id(&mut layout)?;
                    let mut copy = contents[base.0].clone();
                    copy.id = cid;
                    copy.notes.clear_selection();
                    contents.push(copy);
                    ContentRef::Owned(cid)
                }
            }
        };
        given.push(content);
        let track = doc
            .track(c.row as usize)
            .map(|t| t.id())
            .ok_or_else(|| ClipEditError("That track does not exist.".into()))?;
        let new_id = new_clip_id(&mut layout)?;
        // New ids are above every placed id: appended in order (no per-copy
        // search; a repeat of many clips stays linear).
        debug_assert!(layout.placements.last().is_none_or(|q| q.id < new_id));
        let src = s.placement.clone();
        // A derived content may end earlier (notes left out): its window is
        // the source's span, so the arrangement keeps its shape.
        let length = match (c.key, src.length) {
            (Some(_), None) if s.span.1 > s.span.0 => Some(s.span.1 - s.span.0),
            (_, l) => l,
        };
        layout.placements.push(ClipPlacement {
            id: new_id,
            track,
            content,
            start: c.start,
            length,
            muted: c.muted || src.muted,
            revision: 0,
            // A copy of a primary is a placement of that track's content:
            // an explicit window, as `duplicate_clips` makes.
            ..src
        });
    }
    Ok(set_clips(
        layout,
        contents,
        if plan.linked {
            "Paste Repeat Clips (Linked)"
        } else {
            "Paste Repeat Clips"
        },
    ))
}

/// One undo step adding, for every clip `ids` and every offset of `plan`, a
/// copy at `start + offset` on the clip's row (linked or independent) — S13's
/// repeat, every function off. The new clips' ids follow
/// `doc.clips().layout().next_clip` in (copy, clip) order.
pub fn repeat_clips(
    doc: &PlumDocument,
    ids: &[ClipId],
    plan: &RepeatPlan,
    linked: bool,
) -> Planned {
    let p = build_plan(doc, ids, plan.clone(), linked, &ParamSet::new())?;
    apply_clip_repeat(doc, &p)
}
