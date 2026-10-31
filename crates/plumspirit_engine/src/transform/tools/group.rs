//! Group Notes (v0.7): persistent note groups (`GroupId` per note, the
//! project's [`GroupTable`]) — Group / Ungroup the selection plus ten advanced
//! functions (nesting, temporary and locked groups, names, colours, group-local
//! transforms, instance clones and links). The new table travels with the run
//! (`Output::groups`) and is committed in the same undo step as the notes.
//!
//! v0.9 (V09-S36 step 7): eleven functions declared by id (n01 – n11,
//! docs/TRANSFORM_ENGINE.md "Group Notes") run in a separate kernel
//! ([`Group9`]); with every one off and no Control Sweep the v0.8 kernel runs
//! unchanged. Group evaluates its Control Sweeps itself, once per group
//! ([`super::super::definition::kernel_sweeps`]): its output does not come
//! note by note from the selection, so the platform must not split it.

use super::super::boundary::{self, PitchPolicy};
use super::super::drivers::Place;
use super::super::param::*;
use super::super::preflight::Estimate;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::metric_position::bar_place;
use super::super::shared::phrase_detect::Phrases;
use super::super::theory::{KEY_NAMES, Scale, scale_names};
use super::super::{Ctx, Growth, Kernel, Output, Source, TNote, TransformError};
use super::notes::{sub_at, sub_place};
use super::{Contextual, boxed};
use crate::model::group::MAX_GROUPS as MAX_PROJECT_GROUPS;
use crate::model::{GroupInfo, GroupTable};
use crate::view::grid::MeterMap;
use std::collections::{BTreeMap, HashMap};

pub const GROUP: ToolDef = ToolDef {
    id: "group",
    spec: group_spec,
    build: group_build,
};

const GRP_BY: &[&str] = &[
    "Phrases (split at rests)",
    "Chords",
    "Bars",
    "Subchannels",
    "Connected shapes",
];
const GRP_PER: &[&str] = &["Each track", "All selected notes together"];
const GRP_PERM: &[&str] = &[
    "Reverse",
    "Rotate",
    "By pitch (lowest first)",
    "By length (shortest first)",
    "List",
];
const GRP_ORDER: &[&str] = &["By time", "By pitch"];
const GRP_FILL: &[&str] = &["Repeat", "Stretch"];
const GRP_NEXT: &[&str] = &["The next group", "The next group on the same track"];
const GRP_FIT: &[&str] = &["Length", "Pitch span", "Length and pitch span"];
const GRP_ANCHOR: &[&str] = &["Start", "Center", "End"];
const GRP_SPLIT: &[&str] = &["Largest rests", "Bar lines", "Equal note counts"];
const GRP_SOURCE: &[&str] = &["First group", "Largest group", "Group number"];
const GRP_SHAPE_FIT: &[&str] = &["Keep the size", "Scale to each group's length"];
const GRP_THIN: &[&str] = &[
    "Outline (lowest and highest of each onset)",
    "Loudest",
    "Evenly spaced",
];
const GRP_TARGET: &[&str] = &[
    "Velocity",
    "Gate (% of the length)",
    "MIDI channel",
    "Subchannel (position, 0 = the first)",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const GROUP_HELP: &[(&str, &str)] = &[
    (
        "action",
        "Group puts the selected notes into one group; Ungroup removes one level of grouping; Keep the Groups changes nothing but runs the functions below.",
    ),
    (
        "a1",
        "Groups the selection inside its existing groups, as a nested level, instead of replacing them.",
    ),
    (
        "a2",
        "Makes the new group temporary: it is not saved with the project.",
    ),
    (
        "a3",
        "Locks or unlocks the selection's groups; tools never change the notes of a locked group.",
    ),
    ("a3.mode", "Lock or unlock."),
    ("a4", "Gives the group a name."),
    ("a4.name", "The group's name."),
    (
        "a5",
        "Draws the group with a colored outline in the Piano Roll.",
    ),
    (
        "a5.rgb",
        "The outline color as red, green and blue (0 – 255 each).",
    ),
    ("a6", "Scales each group in time around its own pivot."),
    (
        "a6.pivot",
        "The point of each group that stays in place: its start, center or end.",
    ),
    (
        "a6.factor",
        "The time scale (50 % = half as long, 200 % = twice as long).",
    ),
    ("a7", "Mirrors each group inside its own box."),
    (
        "a7.axis",
        "Pitch turns each group upside down; Time plays it backwards.",
    ),
    (
        "a8",
        "Transposes each group by its place in time: the first group by the step, the second by twice the step …",
    ),
    ("a8.step", "The semitones added per group."),
    ("a9", "Adds copies of each group after it."),
    ("a9.count", "How many copies follow each group."),
    (
        "a9.gap",
        "The silence after each instance before the next copy, in ticks.",
    ),
    (
        "a10",
        "Links the selection's groups so editing one edits the others, or unlinks them.",
    ),
    ("a10.mode", "Link the selection's groups, or unlink them."),
    (
        "n01",
        "Groups the notes automatically: by phrases, chords, bars, subchannels or connected shapes.",
    ),
    (
        "n01.by",
        "What makes one group: a phrase (split at rests), a chord, a bar, a subchannel, or a connected shape.",
    ),
    (
        "n01.rest",
        "For Phrases: a rest at least this long, in ticks, ends a group.",
    ),
    (
        "n01.tol",
        "For Chords: notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n01.per",
        "Group each track on its own, or all selected notes together.",
    ),
    (
        "n02",
        "Reorders the groups in time: each group moves to another group's slot.",
    ),
    (
        "n02.order",
        "The new order: reversed, rotated, by pitch (lowest first), by length (shortest first), or your list.",
    ),
    ("n02.rotate", "For Rotate: how many slots each group moves."),
    (
        "n02.list",
        "For List: the slot each group takes, in time order (0 = the first slot).",
    ),
    (
        "n03",
        "Moves each group a little further than the one before.",
    ),
    (
        "n03.shift",
        "How many ticks each group moves more than the previous one (+ later, − earlier).",
    ),
    ("n03.order", "Count the groups by time or by pitch."),
    (
        "n04",
        "Fills the gap after each group by repeating or stretching it up to the next group.",
    ),
    (
        "n04.fill",
        "Repeat the group until the next one, or stretch it to reach it.",
    ),
    ("n04.gap", "A gap, in ticks, left before the next group."),
    (
        "n04.next",
        "Fill up to the next group of any track, or the next group on the same track.",
    ),
    (
        "n05",
        "Fits every group to a target length and / or pitch span.",
    ),
    (
        "n05.fit",
        "What is fitted: the length, the pitch span, or both.",
    ),
    ("n05.len", "The target length, in ticks."),
    ("n05.span", "The target pitch span, in semitones."),
    (
        "n05.anchor",
        "The point that stays in place: the group's start, center or end (its lowest key, middle or highest for pitch).",
    ),
    (
        "n06",
        "Splits groups that are too large and merges groups that are very close.",
    ),
    ("n06.largest", "Groups with more notes than this are split."),
    (
        "n06.merge",
        "Groups closer together than this, in ticks, are merged (0 = never).",
    ),
    (
        "n06.at",
        "Where large groups are split: at their largest rests, at bar lines, or into equal note counts.",
    ),
    ("n07", "Copies the shape of one group onto all the others."),
    (
        "n07.source",
        "The group whose shape is copied: the first, the largest, or a group by number.",
    ),
    (
        "n07.number",
        "For Group Number: the group, counted in time order (1 = the first).",
    ),
    (
        "n07.fit",
        "Keep the copied shape's size, or scale it to each group's length.",
    ),
    ("n08", "Transposes each group by scale degrees from a list."),
    (
        "n08.degrees",
        "The scale degrees each group moves, in turn; the list repeats.",
    ),
    ("n08.key", "The key of the scale."),
    ("n08.scale", "The scale the degrees count in."),
    ("n08.order", "Count the groups by time or by pitch."),
    ("n08.offset", "Added to every degree in the list."),
    ("n09", "Thins every group down to a few notes."),
    ("n09.count", "How many notes each group keeps."),
    (
        "n09.keep",
        "Which notes stay: the outline (lowest and highest of each onset), the loudest, or notes evenly spaced through the group.",
    ),
    (
        "n10",
        "Sets a property of each group's notes from the group's index, through a curve.",
    ),
    (
        "n10.target",
        "The property set: velocity, gate (percent of the length), MIDI channel, or subchannel (position, 0 = the first).",
    ),
    (
        "n10.map",
        "The curve from the first group (left) to the last (right), between Low and High.",
    ),
    ("n10.lo", "The value where the curve is at 0."),
    ("n10.hi", "The value where the curve is at 1."),
    ("n10.order", "Count the groups by time or by pitch."),
    (
        "n11",
        "Scales the groups in a series: each one by the factor again (shrinking or growing groups).",
    ),
    (
        "n11.factor",
        "Each group's size as a share of the previous group's.",
    ),
    (
        "n11.pivot",
        "The point of each group that stays in place: its start, center or end.",
    ),
    ("n11.order", "Count the groups by time or by pitch."),
];

fn group_spec() -> ToolSpec {
    let mut p = vec![
        choice(
            "action",
            "Action",
            &[
                "Group the selection",
                "Ungroup (one level)",
                "Keep the groups (advanced functions only)",
            ],
            0,
        ),
        adv_switch(1, "Nested groups"),
        adv_switch(2, "Temporary group (not saved)"),
        adv_switch(3, "Lock group"),
        choice("a3.mode", "Lock state", &["Lock", "Unlock"], 0).adv(3),
        adv_switch(4, "Named group"),
        text("a4.name", "Name", 64, "Group").adv(4),
        adv_switch(5, "Group color outline"),
        list("a5.rgb", "Color (red, green, blue)", 0, 255, 3, &[255, 170, 60]).adv(5),
        adv_switch(6, "Local pivot (time scale per group)"),
        choice("a6.pivot", "Pivot", &["Group start", "Group center", "Group end"], 0).adv(6),
        float("a6.factor", "Time scale", 10.0, 400.0, 50.0, "%").adv(6),
        adv_switch(7, "Local coordinate system"),
        choice(
            "a7.axis",
            "Mirror inside each group's box",
            &["Pitch (upside down)", "Time (backwards)"],
            0,
        )
        .adv(7),
        adv_switch(8, "Transform isolation (per-group transpose)"),
        int(
            "a8.step",
            "Semitones per group (the first group in time moves this far, the second twice as far, …)",
            -24,
            24,
            2,
        ).adv(8),
        adv_switch(9, "Instance cloning"),
        int("a9.count", "Clones", 1, 16, 1).adv(9),
        ticks("a9.gap", "Gap after each instance (ticks)", 0, 1 << 40, 0).adv(9),
        adv_switch(10, "Linked groups"),
        choice("a10.mode", "Links", &["Link the selection's groups", "Unlink"], 0).adv(10),
        // V09-S36 step 7: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Group Notes").
        flag("n01", "Auto-group", false),
        choice("n01.by", "By", GRP_BY, 0),
        ticks("n01.rest", "Split rest (ticks)", 0, 1 << 40, 480),
        ticks("n01.tol", "Chord tolerance (ticks)", 0, 1 << 40, 10),
        choice("n01.per", "Per", GRP_PER, 0),
        flag("n02", "Group permutation", false),
        choice("n02.order", "Order", GRP_PERM, 0),
        int("n02.rotate", "Rotate by (slots)", -64, 64, 1),
        list(
            "n02.list",
            "Slot of each group in time order (0 = the first slot)",
            0,
            4095,
            256,
            &[1, 0],
        ),
        flag("n03", "Group stagger", false),
        int("n03.shift", "Shift per group (ticks)", -100_000, 100_000, 240),
        choice("n03.order", "Order", GRP_ORDER, 0),
        flag("n04", "Group fill", false),
        choice("n04.fill", "Fill", GRP_FILL, 0),
        ticks("n04.gap", "Gap (ticks)", 0, 1 << 40, 0),
        choice("n04.next", "Until", GRP_NEXT, 0),
        flag("n05", "Group fit", false),
        choice("n05.fit", "Fit", GRP_FIT, 0),
        ticks("n05.len", "Target length (ticks)", 1, 1 << 40, 3840),
        int("n05.span", "Target pitch span (semitones)", 0, 255, 12),
        choice("n05.anchor", "Anchor", GRP_ANCHOR, 0),
        flag("n06", "Split and merge groups", false),
        int("n06.largest", "Largest group (notes)", 2, 100_000, 16),
        ticks("n06.merge", "Merge groups closer than (ticks)", 0, 1 << 40, 0),
        choice("n06.at", "Split at", GRP_SPLIT, 0),
        flag("n07", "Group shape copy", false),
        choice("n07.source", "Source", GRP_SOURCE, 0),
        int("n07.number", "Group number (in time order)", 1, 100_000, 1),
        choice("n07.fit", "Fit", GRP_SHAPE_FIT, 0),
        flag("n08", "Group degree sequence", false),
        list(
            "n08.degrees",
            "Scale degrees per group (cycled)",
            -14,
            14,
            64,
            &[0, 2, 4, 1],
        ),
        choice("n08.key", "Key", &KEY_NAMES, 0),
        choice("n08.scale", "Scale", scale_names(), 0),
        choice("n08.order", "Order", GRP_ORDER, 0),
        int("n08.offset", "Degree offset", -14, 14, 0),
        flag("n09", "Group thinning", false),
        int("n09.count", "Notes per group", 1, 100_000, 4),
        choice("n09.keep", "Keep", GRP_THIN, 0),
        flag("n10", "Group index mapping", false),
        choice("n10.target", "Target", GRP_TARGET, 0),
        curve("n10.map", "Map (group index → value)", Curve::line(0.0, 1.0)).overshoot(),
        int("n10.lo", "Low", 0, 4095, 40),
        int("n10.hi", "High", 0, 4095, 120),
        choice("n10.order", "Order", GRP_ORDER, 0),
        flag("n11", "Group scale series", false),
        float("n11.factor", "Factor per group", 50.0, 200.0, 90.0, "%"),
        choice("n11.pivot", "Pivot", GRP_ANCHOR, 0),
        choice("n11.order", "Order", GRP_ORDER, 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, GROUP_HELP);
    ToolSpec {
        id: "group",
        name: "Group Notes",
        family: Family::Editor,
        summary: "Puts the selected notes into one group (saved with the project, Ctrl+G) or ungroups them; clicking a grouped note selects its whole group. Locked groups are never changed by tools.",
        params: p,
        advanced: vec![
            "Nested groups",
            "Temporary group",
            "Lock group",
            "Named group",
            "Group color outline",
            "Local pivot",
            "Local coordinate system",
            "Transform isolation",
            "Instance cloning",
            "Linked groups with unlink",
        ],
        // Orders = where each runs in `Group9` (§235).
        functions: [
            ("n01", "Auto-group", 110),
            ("n02", "Group permutation", 170),
            ("n03", "Group stagger", 180),
            ("n04", "Group fill", 200),
            ("n05", "Group fit", 150),
            ("n06", "Split and merge groups", 120),
            ("n07", "Group shape copy", 130),
            ("n08", "Group degree sequence", 190),
            ("n09", "Group thinning", 140),
            ("n10", "Group index mapping", 510),
            ("n11", "Group scale series", 160),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: true,
        context: false,
    }
}

/// Time and pitch box of a set of notes.
#[derive(Clone, Copy, Debug)]
struct GBox {
    t0: u64,
    t1: u64,
    p0: u8,
    p1: u8,
}

/// The v0.8 settings: the action and the frozen functions a1 – a10.
#[derive(Clone, Debug)]
struct GrpV08 {
    action: usize,
    nested: bool,
    temporary: bool,
    lock: Option<bool>,
    name: Option<String>,
    color: Option<[u8; 3]>,
    pivot: Option<(usize, f64)>,
    /// a6's time scale in % (what a Control Sweep of it combines with).
    pivot_pct: f64,
    mirror: Option<usize>,
    iso: Option<i64>,
    clones: Option<(u64, u64)>,
    link: Option<bool>,
}

impl GrpV08 {
    fn from(p: &Params) -> GrpV08 {
        GrpV08 {
            action: p.choice("action"),
            nested: p.adv(1),
            temporary: p.adv(2),
            lock: p.adv(3).then(|| p.choice("a3.mode") == 0),
            name: p.adv(4).then(|| {
                let n = p.text("a4.name");
                let n = n.trim().to_string();
                if n.is_empty() { "Group".to_string() } else { n }
            }),
            color: p.adv(5).then(|| {
                let v = p.list("a5.rgb");
                let c = |i: usize| v.get(i).copied().unwrap_or(0).clamp(0, 255) as u8;
                [c(0), c(1), c(2)]
            }),
            pivot: p
                .adv(6)
                .then(|| (p.choice("a6.pivot"), p.float("a6.factor") / 100.0)),
            pivot_pct: p.float("a6.factor"),
            mirror: p.adv(7).then(|| p.choice("a7.axis")),
            iso: p.adv(8).then(|| p.int("a8.step")),
            clones: p
                .adv(9)
                .then(|| (p.int("a9.count") as u64, p.ticks("a9.gap"))),
            link: p.adv(10).then(|| p.choice("a10.mode") == 0),
        }
    }

    /// The attributes of a group the action creates.
    fn info(&self) -> GroupInfo {
        GroupInfo {
            temporary: self.temporary,
            locked: self.lock == Some(true),
            name: self.name.clone(),
            color: self.color,
            ..Default::default()
        }
    }

    /// Instances of every note (the note and its clones).
    fn copies(&self) -> u64 {
        self.clones.map_or(1, |c| c.0 + 1)
    }
}

fn group_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let v8 = GrpV08::from(p);
    let new = GrpNew::from(p);
    let swept = b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty());
    let base = (*b.ctx.groups).clone();
    if new.any() || swept {
        return Ok(Box::new(Group9 {
            v8,
            new,
            base,
            ctx: b.ctx.clone(),
            count: b.stats.count as u64,
        }));
    }
    let GrpV08 {
        action,
        nested,
        temporary,
        lock,
        name,
        color,
        pivot,
        mirror,
        iso,
        clones,
        link,
        ..
    } = v8;
    let copies = clones.map_or(1, |c| c.0 + 1);
    let total = (b.stats.count as u64).saturating_mul(copies);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            let mut table: GroupTable = base.clone();
            let mut notes: Vec<TNote> = src.notes.clone();
            // 1. The action.
            let new_id = (action == 0).then(|| {
                table.add(GroupInfo {
                    temporary,
                    locked: lock == Some(true),
                    name: name.clone(),
                    color,
                    ..Default::default()
                })
            });
            match (action, new_id) {
                (0, Some(id)) => {
                    for n in &mut notes {
                        if nested && n.group != 0 && table.get(n.group).is_some() {
                            let r = table.root_of(n.group);
                            if r != id
                                && let Some(g) = table.get_mut(r)
                            {
                                g.parent = id;
                            }
                        } else {
                            n.group = id;
                        }
                    }
                }
                (1, _) => {
                    for n in &mut notes {
                        n.group = table.get(n.group).map_or(0, |g| g.parent);
                    }
                }
                _ => {}
            }
            // Top-level groups the selection belongs to (locked notes too, so
            // they can be unlocked).
            let mut roots: Vec<u32> = notes
                .iter()
                .chain(src.locked.iter())
                .filter(|n| n.group != 0)
                .map(|n| table.root_of(n.group))
                .collect();
            roots.sort_unstable();
            roots.dedup();
            if action != 0 {
                for &r in &roots {
                    if let Some(g) = table.get_mut(r) {
                        if let Some(l) = lock {
                            g.locked = l;
                        }
                        if let Some(n) = &name {
                            g.name = Some(n.clone());
                        }
                        if let Some(c) = color {
                            g.color = Some(c);
                        }
                        if temporary {
                            g.temporary = true;
                        }
                    }
                }
            }
            if let Some(l) = link {
                let set = roots.first().copied().unwrap_or(0);
                for &r in &roots {
                    if let Some(g) = table.get_mut(r) {
                        g.link = if l && roots.len() > 1 { set } else { 0 };
                    }
                }
            }
            // Group boxes (by top-level group; ungrouped notes are left alone).
            let root =
                |t: &GroupTable, n: &TNote| if n.group == 0 { 0 } else { t.root_of(n.group) };
            let mut boxes: BTreeMap<u32, GBox> = BTreeMap::new();
            for n in &notes {
                let r = root(&table, n);
                if r == 0 {
                    continue;
                }
                let e = boxes.entry(r).or_insert(GBox {
                    t0: u64::MAX,
                    t1: 0,
                    // Above every pitch until a note widens the box.
                    p0: u8::MAX,
                    p1: 0,
                });
                e.t0 = e.t0.min(n.start);
                e.t1 = e.t1.max(n.end());
                e.p0 = e.p0.min(n.pitch);
                e.p1 = e.p1.max(n.pitch);
            }
            // Group order in time (transform isolation).
            let mut by_time: Vec<(u64, u32)> = boxes.iter().map(|(&id, b)| (b.t0, id)).collect();
            by_time.sort_unstable();
            let order: BTreeMap<u32, i64> = by_time
                .iter()
                .enumerate()
                .map(|(k, &(_, id))| (id, k as i64 + 1))
                .collect();
            // Clone groups: one new group per (instance, top-level group).
            let mut clone_ids: BTreeMap<(u64, u32), u32> = BTreeMap::new();
            if let Some((count, _)) = clones {
                for &r in boxes.keys() {
                    let set = match table.get(r) {
                        Some(g) if g.link != 0 => g.link,
                        _ => r,
                    };
                    if let Some(g) = table.get_mut(r) {
                        g.link = set;
                    }
                    let info = table.get(r).cloned().unwrap_or_default();
                    for k in 1..=count {
                        let id = table.add(GroupInfo {
                            parent: 0,
                            link: set,
                            locked: false,
                            ..info.clone()
                        });
                        clone_ids.insert((k, r), id);
                    }
                }
            }
            for n in &notes {
                let r = root(&table, n);
                // V09-0143: the driver dimension is the group's place in
                // time (1 = the earliest; ungrouped notes 0).
                out.set_dim(order.get(&r).copied().unwrap_or(0) as u32);
                let mut s = i128::from(n.start);
                let mut d = i128::from(n.dur);
                let mut pitch = i64::from(n.pitch);
                if let Some(bx) = boxes.get(&r) {
                    if let Some((pv, f)) = pivot {
                        let c = match pv {
                            0 => bx.t0 as f64,
                            1 => (bx.t0 as f64 + bx.t1 as f64) / 2.0,
                            _ => bx.t1 as f64,
                        };
                        let a = c + (n.start as f64 - c) * f;
                        let e = c + (n.end() as f64 - c) * f;
                        s = a.round() as i128;
                        d = ((e - a).round() as i128).max(1);
                    }
                    match mirror {
                        Some(0) => pitch = i64::from(bx.p0) + i64::from(bx.p1) - pitch,
                        Some(_) => {
                            let e = s + d;
                            s = i128::from(bx.t0) + i128::from(bx.t1) - e;
                        }
                        None => {}
                    }
                    if let Some(step) = iso {
                        pitch += step * order.get(&r).copied().unwrap_or(0);
                    }
                }
                out.emit(s, d, pitch, i64::from(n.vel), n);
                if let (Some((count, gap)), Some(bx)) = (clones, boxes.get(&r)) {
                    let span = i128::from(bx.t1 - bx.t0) + i128::from(gap);
                    for k in 1..=count {
                        let mut t = *n;
                        t.group = clone_ids.get(&(k, r)).copied().unwrap_or(0);
                        out.emit(s + span * k as i128, d, pitch, i64::from(n.vel), &t);
                    }
                }
                out.check()?;
            }
            // Ungrouped notes become unreferenced groups only by ungrouping;
            // the table keeps the entries (other notes may still use them).
            out.message = Some(match (action, new_id) {
                (0, Some(id)) => format!("Group {id} created."),
                (1, _) => "The selected notes leave their innermost group.".into(),
                _ => format!("{} group(s) in the selection.", roots.len()),
            });
            out.groups = Some(table);
            Ok(())
        },
        growth: Growth::Generate(total),
        estimate: Some(total),
    })
}

// ------------------------------------------------------------------ v0.9

/// n01: how the selection is grouped automatically.
#[derive(Clone, Copy, Debug)]
struct Auto {
    by: usize,
    rest: u64,
    tol: u64,
    all: bool,
}

/// n05: what a group is fitted to.
#[derive(Clone, Copy, Debug)]
struct Fit {
    len: Option<u64>,
    span: Option<i64>,
    anchor: usize,
}

/// The settings of the functions declared by id (`None` = off).
#[derive(Clone, Debug)]
struct GrpNew {
    auto: Option<Auto>,
    /// n02: order, rotation, slot list.
    perm: Option<(usize, i64, Vec<i64>)>,
    /// n03: shift per group, by pitch.
    stagger: Option<(i64, bool)>,
    /// n04: stretch (else repeat), gap, per track.
    fill: Option<(bool, u64, bool)>,
    fit: Option<Fit>,
    /// n06: largest group, merge closer than, split at.
    split: Option<(i64, u64, usize)>,
    /// n07: source, group number, scale to each group's length.
    shape: Option<(usize, i64, bool)>,
    /// n08: degrees, key and scale, by pitch, offset.
    degrees: Option<(Vec<i64>, Scale, bool, i64)>,
    /// n09: notes per group, keep.
    thin: Option<(i64, usize)>,
    /// n10: target, map, low, high, by pitch.
    map: Option<(usize, Curve, i64, i64, bool)>,
    /// n11: factor per group (%), pivot, by pitch.
    series: Option<(f64, usize, bool)>,
}

impl GrpNew {
    fn from(p: &Params) -> GrpNew {
        let by_pitch = |k: &str| p.choice(k) == 1;
        GrpNew {
            auto: p.flag("n01").then(|| Auto {
                by: p.choice("n01.by"),
                rest: p.ticks("n01.rest"),
                tol: p.ticks("n01.tol"),
                all: p.choice("n01.per") == 1,
            }),
            perm: p.flag("n02").then(|| {
                (
                    p.choice("n02.order"),
                    p.int("n02.rotate"),
                    p.list("n02.list"),
                )
            }),
            stagger: p
                .flag("n03")
                .then(|| (p.int("n03.shift"), by_pitch("n03.order"))),
            fill: p.flag("n04").then(|| {
                (
                    p.choice("n04.fill") == 1,
                    p.ticks("n04.gap"),
                    p.choice("n04.next") == 1,
                )
            }),
            fit: p.flag("n05").then(|| {
                let f = p.choice("n05.fit");
                Fit {
                    len: (f != 1).then(|| p.ticks("n05.len").max(1)),
                    span: (f != 0).then(|| p.int("n05.span").max(0)),
                    anchor: p.choice("n05.anchor"),
                }
            }),
            split: p.flag("n06").then(|| {
                (
                    p.int("n06.largest").max(2),
                    p.ticks("n06.merge"),
                    p.choice("n06.at"),
                )
            }),
            shape: p.flag("n07").then(|| {
                (
                    p.choice("n07.source"),
                    p.int("n07.number").max(1),
                    p.choice("n07.fit") == 1,
                )
            }),
            degrees: p.flag("n08").then(|| {
                (
                    p.list("n08.degrees"),
                    Scale::new(p.choice("n08.key"), p.choice("n08.scale")),
                    by_pitch("n08.order"),
                    p.int("n08.offset"),
                )
            }),
            thin: p
                .flag("n09")
                .then(|| (p.int("n09.count").max(1), p.choice("n09.keep"))),
            map: p.flag("n10").then(|| {
                (
                    p.choice("n10.target"),
                    p.curve("n10.map"),
                    p.int("n10.lo"),
                    p.int("n10.hi"),
                    by_pitch("n10.order"),
                )
            }),
            series: p.flag("n11").then(|| {
                (
                    p.float("n11.factor"),
                    p.choice("n11.pivot"),
                    by_pitch("n11.order"),
                )
            }),
        }
    }

    fn any(&self) -> bool {
        self.auto.is_some()
            || self.perm.is_some()
            || self.stagger.is_some()
            || self.fill.is_some()
            || self.fit.is_some()
            || self.split.is_some()
            || self.shape.is_some()
            || self.degrees.is_some()
            || self.thin.is_some()
            || self.map.is_some()
            || self.series.is_some()
    }

    /// Whether the result can hold more notes than the selection × clones
    /// (n07 copies a shape onto every group, n04 repeats groups).
    fn grows(&self) -> bool {
        self.shape.is_some() || self.fill.is_some_and(|f| !f.0)
    }
}

/// The groups of the selection: top-level group ids (ascending) and each
/// group's selected notes (indices, time-major), plus the notes without a
/// group. Flat index lists with offsets — no allocation per group.
struct Groups {
    ids: Vec<u32>,
    off: Vec<usize>,
    members: Vec<u32>,
    free: Vec<u32>,
}

impl Groups {
    fn of(gid: &[u32], table: &GroupTable) -> Groups {
        // Top-level group of each note (consecutive notes mostly share one).
        let mut slot = vec![0u32; gid.len()];
        let (mut lg, mut lr) = (0u32, 0u32);
        for (r, &g) in slot.iter_mut().zip(gid) {
            if g != 0 {
                if g != lg {
                    lg = g;
                    lr = table.root_of(g);
                }
                *r = lr;
            }
        }
        // The distinct ids: a table over the id range when it is compact
        // (ids are handed out in order), else a sort.
        let (lo, hi) = slot
            .iter()
            .filter(|&&r| r != 0)
            .fold((u32::MAX, 0u32), |(a, b), &r| (a.min(r), b.max(r)));
        let dense = lo <= hi && ((hi - lo) as usize) < 4 * slot.len() + 1024;
        let mut table_of: Vec<u32> = Vec::new();
        let ids: Vec<u32> = if dense {
            table_of = vec![0u32; (hi - lo) as usize + 1];
            for &r in &slot {
                if r != 0 {
                    table_of[(r - lo) as usize] = 1;
                }
            }
            let mut ids = Vec::new();
            for (k, t) in table_of.iter_mut().enumerate() {
                if *t != 0 {
                    ids.push(lo + k as u32);
                    *t = ids.len() as u32;
                }
            }
            ids
        } else {
            let mut ids: Vec<u32> = slot.iter().copied().filter(|&r| r != 0).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let mut off = vec![0usize; ids.len() + 1];
        let (mut lr, mut ls) = (0u32, 0u32);
        for r in slot.iter_mut() {
            if *r != 0 {
                if *r != lr {
                    lr = *r;
                    ls = if dense {
                        table_of[(*r - lo) as usize]
                    } else {
                        ids.binary_search(r).map_or(0, |k| k as u32 + 1)
                    };
                }
                *r = ls;
                off[ls as usize] += 1;
            }
        }
        drop(table_of);
        for k in 1..off.len() {
            off[k] += off[k - 1];
        }
        let mut fill = off.clone();
        let mut members = vec![0u32; *off.last().unwrap_or(&0)];
        let mut free = Vec::new();
        for (i, &s) in slot.iter().enumerate() {
            if s == 0 {
                free.push(i as u32);
            } else {
                let f = &mut fill[s as usize - 1];
                members[*f] = i as u32;
                *f += 1;
            }
        }
        Groups {
            ids,
            off,
            members,
            free,
        }
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn get(&self, k: usize) -> &[u32] {
        &self.members[self.off[k]..self.off[k + 1]]
    }
}

/// The distinct non-zero ids of `v`, ascending: a table over the id range
/// when it is compact (ids are handed out in order), else a sort.
fn distinct_ids(mut v: Vec<u32>) -> Vec<u32> {
    let (lo, hi) = v
        .iter()
        .filter(|&&r| r != 0)
        .fold((u32::MAX, 0u32), |(a, b), &r| (a.min(r), b.max(r)));
    if lo > hi {
        return Vec::new();
    }
    if ((hi - lo) as usize) < 4 * v.len() + 1024 {
        let mut seen = vec![false; (hi - lo) as usize + 1];
        for &r in &v {
            if r != 0 {
                seen[(r - lo) as usize] = true;
            }
        }
        return seen
            .iter()
            .enumerate()
            .filter(|x| *x.1)
            .map(|(k, _)| lo + k as u32)
            .collect();
    }
    v.retain(|&r| r != 0);
    v.sort_unstable();
    v.dedup();
    v
}

/// A group's time span and pitch range.
#[derive(Clone, Copy, Debug, Default)]
struct B {
    t0: i128,
    t1: i128,
    p0: i64,
    p1: i64,
}

impl B {
    const EMPTY: B = B {
        t0: i128::MAX,
        t1: i128::MIN,
        p0: i64::MAX,
        p1: i64::MIN,
    };

    #[inline]
    fn add(&mut self, s: i128, e: i128, p: i64) {
        self.t0 = self.t0.min(s);
        self.t1 = self.t1.max(e);
        self.p0 = self.p0.min(p);
        self.p1 = self.p1.max(p);
    }

    fn span(&self) -> i128 {
        (self.t1 - self.t0).max(0)
    }

    /// The time at anchor `a` (start, centre, end).
    fn time_at(&self, a: usize) -> f64 {
        match a {
            0 => self.t0 as f64,
            1 => (self.t0 as f64 + self.t1 as f64) / 2.0,
            _ => self.t1 as f64,
        }
    }

    /// The pitch at anchor `a` (lowest, middle, highest).
    fn pitch_at(&self, a: usize) -> f64 {
        match a {
            0 => self.p0 as f64,
            1 => (self.p0 as f64 + self.p1 as f64) / 2.0,
            _ => self.p1 as f64,
        }
    }
}

/// One note of a group being transformed: its time, length and pitch so
/// far, the selected note it comes from and whether it is a copy of
/// another group's note (n07).
#[derive(Clone, Copy, Debug)]
struct W {
    s: i128,
    d: i128,
    p: i32,
    n: u32,
    /// The group (slot) it belongs to.
    g: u32,
    copy: bool,
}

/// Size of a work note (preflight memory).
const W_BYTES: u64 = std::mem::size_of::<W>() as u64;

/// Most work notes one run builds (the preflight's note ceiling).
const MAX_WORK: u64 = 50_000_000;

/// Scales `w`'s time by `f` around tick `a` (ends rounded; at least one
/// tick long). `f = 1` leaves it exactly.
#[inline]
fn scale_time(w: &mut W, a: f64, f: f64) {
    if f == 1.0 {
        return;
    }
    let s = (a + (w.s as f64 - a) * f).round() as i128;
    let e = (a + ((w.s + w.d) as f64 - a) * f).round() as i128;
    w.s = s;
    w.d = (e - s).max(1);
}

/// The boxes of the groups' work notes (ranges `wr`).
fn wboxes(w: &[W], wr: &[(usize, usize)]) -> Vec<B> {
    wr.iter()
        .map(|&(a, b)| {
            let mut x = B::EMPTY;
            for v in &w[a..b] {
                x.add(v.s, v.s + v.d, i64::from(v.p));
            }
            if a == b { B::default() } else { x }
        })
        .collect()
}

/// Groups in time order (start, id) or pitch order (lowest, highest,
/// start, id).
fn order_of(bx: &[B], ids: &[u32], by_pitch: bool) -> Vec<usize> {
    let mut o: Vec<usize> = (0..bx.len()).collect();
    if by_pitch {
        o.sort_unstable_by_key(|&k| (bx[k].p0, bx[k].p1, bx[k].t0, ids[k]));
    } else {
        o.sort_unstable_by_key(|&k| (bx[k].t0, ids[k]));
    }
    o
}

fn too_many_groups(table: &GroupTable, more: usize) -> Result<(), TransformError> {
    if table.len().saturating_add(more) > MAX_PROJECT_GROUPS {
        return Err(TransformError::TooLarge(format!(
            "This would make {} new groups; a project holds at most {} — group by a larger unit or select fewer notes.",
            crate::midi::group_digits(more as u64),
            crate::midi::group_digits(MAX_PROJECT_GROUPS as u64)
        )));
    }
    Ok(())
}

/// Union-find root with path halving.
fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[parent[x as usize] as usize];
        parent[x as usize] = p;
        x = p;
    }
    x
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let (a, b) = (find(parent, a), find(parent, b));
    if a != b {
        // The smaller index stays the root (deterministic).
        let (lo, hi) = (a.min(b), a.max(b));
        parent[hi as usize] = lo;
    }
}

/// n01 "Connected shapes": the component of each note of `pi` (positions,
/// one representative each) — two notes connect when their pitches differ
/// by at most a semitone and at most `rest` ticks lie between their spans
/// (overlapping or touching spans connect). One sort by (pitch, start), a
/// sweep per row and per pair of neighbouring rows keeping the latest end
/// so far: O(n log n).
fn shape_components(ns: &[TNote], pi: &[usize], rest: u64) -> Vec<u32> {
    let m = pi.len();
    let mut parent: Vec<u32> = (0..m as u32).collect();
    // (pitch, start, position) packed into one value: a total order sorted
    // without reading the notes per comparison.
    let mut packed: Vec<u128> = pi
        .iter()
        .enumerate()
        .map(|(j, &i)| u128::from(ns[i].pitch) << 96 | u128::from(ns[i].start) << 32 | j as u128)
        .collect();
    packed.sort_unstable();
    let ord: Vec<u32> = packed.iter().map(|&k| k as u32).collect();
    drop(packed);
    let note = |j: u32| &ns[pi[j as usize]];
    // Rows of equal pitch in `ord`.
    let mut rows: Vec<(u8, usize, usize)> = Vec::new();
    let mut a = 0;
    while a < m {
        let p = note(ord[a]).pitch;
        let mut b = a + 1;
        while b < m && note(ord[b]).pitch == p {
            b += 1;
        }
        rows.push((p, a, b));
        a = b;
    }
    let near = |start: u64, end: u64| start.saturating_sub(end) <= rest;
    for &(_, a, b) in &rows {
        let mut hold: Option<(u64, u32)> = None;
        for &j in &ord[a..b] {
            let n = note(j);
            if let Some((e, h)) = hold
                && near(n.start, e)
            {
                union(&mut parent, j, h);
            }
            if hold.is_none_or(|(e, _)| n.end() > e) {
                hold = Some((n.end(), j));
            }
        }
    }
    for w in rows.windows(2) {
        let ((p, a0, b0), (q, a1, b1)) = (w[0], w[1]);
        if q != p.wrapping_add(1) {
            continue;
        }
        let (mut i, mut k) = (a0, a1);
        let mut hold: [Option<(u64, u32)>; 2] = [None, None];
        while i < b0 || k < b1 {
            let take_low = k >= b1 || (i < b0 && note(ord[i]).start <= note(ord[k]).start);
            let (row, j) = if take_low {
                i += 1;
                (0, ord[i - 1])
            } else {
                k += 1;
                (1, ord[k - 1])
            };
            let n = note(j);
            if let Some((e, h)) = hold[1 - row]
                && near(n.start, e)
            {
                union(&mut parent, j, h);
            }
            if hold[row].is_none_or(|(e, _)| n.end() > e) {
                hold[row] = Some((n.end(), j));
            }
        }
    }
    (0..m as u32).map(|j| find(&mut parent, j)).collect()
}

/// n01: the unit of each note of `idx` (time-major indices into `ns`),
/// numbered by first appearance, and the number of units.
fn auto_units(
    ns: &[TNote],
    idx: &[usize],
    a: &Auto,
    src: &Source,
    meter: &MeterMap,
) -> (Vec<u32>, usize) {
    // Partitions: positions into `idx`, per track or all together.
    let parts: Vec<Vec<usize>> = if a.all {
        vec![(0..idx.len()).collect()]
    } else {
        let mut by: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
        for (k, &i) in idx.iter().enumerate() {
            by.entry(ns[i].track).or_default().push(k);
        }
        by.into_values().collect()
    };
    let mut label = vec![0u64; idx.len()];
    let mut base = 0u64;
    for part in &parts {
        let pi: Vec<usize> = part.iter().map(|&k| idx[k]).collect();
        let mut put = |runs: &mut dyn Iterator<Item = usize>| {
            // Contiguous runs of `pi`, one label each.
            let mut j = 0;
            for (q, len) in runs.enumerate() {
                for _ in 0..len {
                    label[part[j]] = base + q as u64;
                    j += 1;
                }
            }
        };
        match a.by {
            0 => {
                let ph = Phrases::of(ns, &pi, a.rest, None);
                put(&mut ph.iter(&pi).map(<[usize]>::len));
                base += ph.len() as u64;
            }
            1 => {
                let cg = ChordGroups::of(ns, &pi, a.tol);
                put(&mut cg.iter().map(<[usize]>::len));
                base += cg.len() as u64;
            }
            2 => {
                let mut last = None;
                let mut q = 0u64;
                for (j, &i) in pi.iter().enumerate() {
                    let bar = bar_place(meter, ns[i].start).0;
                    if last.is_some_and(|l| l != bar) {
                        q += 1;
                    }
                    last = Some(bar);
                    label[part[j]] = base + q;
                }
                base += q + 1;
            }
            3 => {
                let mut keys: HashMap<u64, u64> = HashMap::new();
                for (j, &i) in pi.iter().enumerate() {
                    let n = &ns[i];
                    let key = if a.all {
                        sub_place(src, n.track, n.sub).0 as u64
                    } else {
                        u64::from(n.sub)
                    };
                    let next = keys.len() as u64;
                    label[part[j]] = base + *keys.entry(key).or_insert(next);
                }
                base += keys.len() as u64;
            }
            _ => {
                let comp = shape_components(ns, &pi, a.rest);
                for (j, c) in comp.into_iter().enumerate() {
                    label[part[j]] = base + u64::from(c);
                }
                base += pi.len() as u64;
            }
        }
    }
    // Units numbered by first appearance in time-major order (labels are
    // below `base` ≤ the note count: a dense table, no hashing per note).
    let mut first = vec![u32::MAX; base as usize];
    let mut count = 0u32;
    let unit: Vec<u32> = label
        .iter()
        .map(|&l| {
            let f = &mut first[l as usize];
            if *f == u32::MAX {
                *f = count;
                count += 1;
            }
            *f
        })
        .collect();
    (unit, count as usize)
}

/// n06: a part of a group being split — a note directly in the group, or
/// a nested group (moved whole, at its first note).
#[derive(Clone, Copy, Debug)]
struct Blk {
    start: u64,
    end: u64,
    size: usize,
    /// The nested group (0 = a note directly in the group).
    child: u32,
}

/// n06: where a group of parts `blk` (time order) is cut into pieces of at
/// most `most` notes — at the largest rests (`at` 0; ties: the latest), at
/// bar lines (1; else after `most` notes) or into equal note counts (2).
/// Returns the first part of every piece after the first. A part larger
/// than `most` is a piece of its own.
fn split_cuts(blk: &[Blk], most: usize, at: usize, meter: &MeterMap) -> Vec<usize> {
    let m = blk.len();
    let mut sum = vec![0usize; m + 1];
    for (j, b) in blk.iter().enumerate() {
        sum[j + 1] = sum[j] + b.size;
    }
    let mut cuts = Vec::new();
    if at == 2 {
        let total = sum[m];
        let pieces = total.div_ceil(most.max(1)).max(1);
        let mut c = 1;
        for q in 1..pieces {
            // The first part reaching q / pieces of the notes.
            while c < m && sum[c] * pieces < q * total {
                c += 1;
            }
            if c < m && cuts.last().is_none_or(|&l| l < c) {
                cuts.push(c);
            }
        }
        return cuts;
    }
    // Rests before each part (time from the latest end so far).
    let mut rest = vec![i128::MIN; m];
    let mut latest = 0u64;
    for (j, b) in blk.iter().enumerate() {
        if j > 0 {
            rest[j] = i128::from(b.start) - i128::from(latest);
        }
        latest = latest.max(b.end);
    }
    // Bar line cuts: a part in another bar than the one before it.
    let mut allowed_upto = vec![0usize; m];
    if at == 1 {
        let mut last = 0;
        let mut prev_bar = None;
        for (j, b) in blk.iter().enumerate() {
            let bar = bar_place(meter, b.start).0;
            if j > 0 && prev_bar != Some(bar) {
                last = j;
            }
            prev_bar = Some(bar);
            allowed_upto[j] = last;
        }
    }
    // Greedy pieces; the window of cut candidates moves forward only, so a
    // monotone deque keeps its largest rest (O(n)).
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let (mut i, mut jp, mut pushed) = (0usize, 0usize, 1usize);
    while i < m {
        while jp < m && sum[jp + 1] - sum[i] <= most {
            jp += 1;
        }
        if jp >= m {
            break;
        }
        let reach = jp.max(i + 1);
        if reach >= m {
            // The last part alone is larger than the limit: its own piece.
            break;
        }
        let c = if at == 1 {
            let a = allowed_upto[reach];
            if a > i { a } else { reach }
        } else {
            while pushed <= reach {
                while dq.back().is_some_and(|&b| rest[b] <= rest[pushed]) {
                    dq.pop_back();
                }
                dq.push_back(pushed);
                pushed += 1;
            }
            while dq.front().is_some_and(|&f| f <= i) {
                dq.pop_front();
            }
            dq.front().copied().unwrap_or(reach)
        };
        cuts.push(c);
        i = c;
        jp = jp.max(i);
    }
    cuts
}

/// The child of `r` on the way up from group `g` (`g` itself when its
/// parent is `r`).
fn child_of(table: &GroupTable, g: u32, r: u32) -> u32 {
    let mut cur = g;
    for _ in 0..=crate::model::group::MAX_GROUP_DEPTH {
        match table.get(cur) {
            Some(e) if e.parent != r && e.parent != 0 => cur = e.parent,
            _ => break,
        }
    }
    cur
}

/// What a v0.9 run made, before emission.
struct Plan {
    table: GroupTable,
    /// Group id of every selected note after the structure stage.
    gid: Vec<u32>,
    groups: Groups,
    w: Vec<W>,
    /// Work notes of each group.
    wr: Vec<(usize, usize)>,
    /// n04: repeats and their period per group.
    fills: Vec<(u64, i128)>,
    /// a9: clone spacing per group and the clones' group ids (group-major).
    span0: Vec<i128>,
    clone_ids: Vec<u32>,
    /// n10: the value per group.
    map: Vec<i64>,
    message: String,
}

impl Plan {
    /// Notes the emission writes.
    fn emitted(&self, clones: u64) -> u64 {
        let mut total = self.groups.free.len() as u64;
        for (k, &(a, b)) in self.wr.iter().enumerate() {
            let fills = self.fills.get(k).map_or(0, |f| f.0);
            total = total.saturating_add(
                ((b - a) as u64)
                    .saturating_mul(fills.saturating_add(1))
                    .saturating_mul(clones.saturating_add(1)),
            );
        }
        total
    }
}

enum Planned {
    Full(Box<Plan>),
    /// More work notes than [`MAX_WORK`] (n07): their count, nothing built.
    Over(u64),
}

/// The v0.9 Group kernel (any function declared by id, or a Control Sweep).
struct Group9 {
    v8: GrpV08,
    new: GrpNew,
    base: GroupTable,
    ctx: Ctx,
    count: u64,
}

type Unit = (TNote, Place);

impl Group9 {
    /// An upper bound of the result for `n` selected notes from the counts
    /// alone: n × clones; n07 at most n groups × n source notes; n04's
    /// repeats depend on where the groups land (no bound without the notes).
    fn bound(&self, n: u64) -> u64 {
        if self.new.fill.is_some_and(|f| !f.0) {
            return u64::MAX;
        }
        let work = if self.new.shape.is_some() {
            n.saturating_mul(n)
        } else {
            n
        };
        work.saturating_mul(self.v8.copies())
    }

    /// Each group's first note and place (Note index = its place in time
    /// order) for the Control Sweeps — empty without sweeps.
    fn units(&self, ns: &[TNote], gs: &Groups, tord: &[usize], src: &Source) -> Vec<Unit> {
        if self.ctx.sweeps.as_ref().is_none_or(|s| s.is_empty()) {
            return Vec::new();
        }
        let mut rank = vec![0usize; gs.len()];
        for (k, &s) in tord.iter().enumerate() {
            rank[s] = k;
        }
        (0..gs.len())
            .map(|k| {
                let n = ns[gs.get(k)[0] as usize];
                let (sub_pos, subs) = sub_place(src, n.track, n.sub);
                (
                    n,
                    Place {
                        index: rank[k],
                        count: gs.len(),
                        sub_pos,
                        subs: subs.max(1),
                        ..Place::default()
                    },
                )
            })
            .collect()
    }

    /// Integer parameter `key` for group `k` (swept or `base`).
    fn sv_int(&self, key: &str, base: i64, units: &[Unit], k: usize) -> i64 {
        match (&self.ctx.sweeps, units.get(k)) {
            (Some(s), Some(u)) => s.int(key, base, &u.0, u.1),
            _ => base,
        }
    }

    fn sv_float(&self, key: &str, base: f64, units: &[Unit], k: usize) -> f64 {
        match (&self.ctx.sweeps, units.get(k)) {
            (Some(s), Some(u)) => s.float(key, base, &u.0, u.1),
            _ => base,
        }
    }

    /// The groups' boxes of their own (selected) notes and the time order.
    fn own(ns: &[TNote], gs: &Groups) -> (Vec<B>, Vec<usize>) {
        let bx: Vec<B> = (0..gs.len())
            .map(|k| {
                let mut b = B::EMPTY;
                for &i in gs.get(k) {
                    let n = &ns[i as usize];
                    b.add(
                        i128::from(n.start),
                        i128::from(n.start) + i128::from(n.dur),
                        i64::from(n.pitch),
                    );
                }
                b
            })
            .collect();
        let o = order_of(&bx, &gs.ids, false);
        (bx, o)
    }

    /// The structure stage: the action with n01, the v0.8 attributes and
    /// links, then n06. Returns the table, each note's group id, the roots
    /// the v0.8 message counts and the message parts.
    #[allow(clippy::type_complexity)]
    fn structure(
        &self,
        src: &Source,
    ) -> Result<(GroupTable, Vec<u32>, usize, Option<u32>, Vec<String>), TransformError> {
        let ns = &src.notes;
        let (v, w) = (&self.v8, &self.new);
        let mut table = self.base.clone();
        let mut gid: Vec<u32> = ns.iter().map(|n| n.group).collect();
        let mut notes_msg = Vec::new();
        let mut new_id = None;
        // 1. The action (n01: one group per unit instead of one group).
        match v.action {
            0 => match &w.auto {
                Some(a) => {
                    let idx: Vec<usize> = (0..ns.len()).collect();
                    let (unit, count) = auto_units(ns, &idx, a, src, &self.ctx.meter);
                    too_many_groups(&table, count)?;
                    let first = table.next_id();
                    let ids: Vec<u32> = (0..count).map(|_| table.add(v.info())).collect();
                    for (k, &i) in idx.iter().enumerate() {
                        let id = ids[unit[k] as usize];
                        let g = gid[i];
                        if v.nested && g != 0 && table.get(g).is_some() {
                            // An existing group nests in the unit of its
                            // first note.
                            let r = table.root_of(g);
                            if r < first
                                && let Some(e) = table.get_mut(r)
                            {
                                e.parent = id;
                            }
                        } else {
                            gid[i] = id;
                        }
                    }
                    notes_msg.push(format!("Auto-group made {count} group(s)."));
                }
                None => {
                    let id = table.add(v.info());
                    new_id = Some(id);
                    for g in gid.iter_mut() {
                        if v.nested && *g != 0 && table.get(*g).is_some() {
                            let r = table.root_of(*g);
                            if r != id
                                && let Some(e) = table.get_mut(r)
                            {
                                e.parent = id;
                            }
                        } else {
                            *g = id;
                        }
                    }
                }
            },
            1 => {
                for g in gid.iter_mut() {
                    *g = table.get(*g).map_or(0, |e| e.parent);
                }
            }
            _ => {}
        }
        // n01 with the other actions: the selected notes left without a
        // group.
        if v.action != 0
            && let Some(a) = &w.auto
        {
            let idx: Vec<usize> = (0..ns.len()).filter(|&i| gid[i] == 0).collect();
            if !idx.is_empty() {
                let (unit, count) = auto_units(ns, &idx, a, src, &self.ctx.meter);
                too_many_groups(&table, count)?;
                let ids: Vec<u32> = (0..count)
                    .map(|_| table.add(GroupInfo::default()))
                    .collect();
                for (k, &i) in idx.iter().enumerate() {
                    gid[i] = ids[unit[k] as usize];
                }
                notes_msg.push(format!("Auto-group made {count} group(s)."));
            }
        }
        // 2. The v0.8 attributes and links of the selection's top-level
        // groups (locked notes too, so they can be unlocked).
        let (mut lg, mut lr) = (0u32, 0u32);
        let roots = distinct_ids(
            gid.iter()
                .copied()
                .chain(src.locked.iter().map(|n| n.group))
                .map(|g| {
                    if g != 0 && g != lg {
                        lg = g;
                        lr = table.root_of(g);
                    }
                    if g == 0 { 0 } else { lr }
                })
                .collect(),
        );
        if v.action != 0 {
            for &r in &roots {
                if let Some(g) = table.get_mut(r) {
                    if let Some(l) = v.lock {
                        g.locked = l;
                    }
                    if let Some(n) = &v.name {
                        g.name = Some(n.clone());
                    }
                    if let Some(c) = v.color {
                        g.color = Some(c);
                    }
                    if v.temporary {
                        g.temporary = true;
                    }
                }
            }
        }
        if let Some(l) = v.link {
            let set = roots.first().copied().unwrap_or(0);
            for &r in &roots {
                if let Some(g) = table.get_mut(r) {
                    g.link = if l && roots.len() > 1 { set } else { 0 };
                }
            }
        }
        // 3. n06: merge groups closer than the gap (the later group's notes
        // join the earlier one, its nested groups too), then split groups
        // larger than N notes.
        if let Some((largest, merge, at)) = w.split {
            let gs = Groups::of(&gid, &table);
            let (bx, tord) = Self::own(ns, &gs);
            let units = self.units(ns, &gs, &tord, src);
            let mut merged: HashMap<u32, u32> = HashMap::new();
            if let Some(&first) = tord.first() {
                let (mut cur, mut cur_t1) = (gs.ids[first], bx[first].t1);
                for &k in &tord[1..] {
                    let m = i128::from(self.sv_int("n06.merge", merge as i64, &units, k).max(0));
                    let gap = (bx[k].t0 - cur_t1).max(0);
                    if gap < m {
                        merged.insert(gs.ids[k], cur);
                        cur_t1 = cur_t1.max(bx[k].t1);
                    } else {
                        cur = gs.ids[k];
                        cur_t1 = bx[k].t1;
                    }
                }
            }
            if !merged.is_empty() {
                for g in gid.iter_mut() {
                    if let Some(&a) = merged.get(g) {
                        *g = a;
                    }
                }
                let kids: Vec<(u32, u32)> = table
                    .entries()
                    .iter()
                    .filter_map(|e| merged.get(&e.parent).map(|&a| (e.id, a)))
                    .collect();
                for (id, a) in kids {
                    if let Some(e) = table.get_mut(id) {
                        e.parent = a;
                    }
                }
            }
            // Split.
            let gs = Groups::of(&gid, &table);
            let most = largest as usize;
            let mut plans: Vec<(usize, Vec<Blk>, Vec<u32>, Vec<usize>)> = Vec::new();
            let mut pieces = 0usize;
            for k in 0..gs.len() {
                let m = gs.get(k);
                if m.len() <= most {
                    continue;
                }
                let r = gs.ids[k];
                let mut blk: Vec<Blk> = Vec::new();
                let mut part_of: Vec<u32> = Vec::with_capacity(m.len());
                let mut child_part: HashMap<u32, u32> = HashMap::new();
                let mut top: HashMap<u32, u32> = HashMap::new();
                for &i in m {
                    let n = &ns[i as usize];
                    let g = gid[i as usize];
                    if g == r {
                        blk.push(Blk {
                            start: n.start,
                            end: n.end(),
                            size: 1,
                            child: 0,
                        });
                        part_of.push(blk.len() as u32 - 1);
                        continue;
                    }
                    let c = *top.entry(g).or_insert_with(|| child_of(&table, g, r));
                    match child_part.get(&c) {
                        Some(&u) => {
                            let b = &mut blk[u as usize];
                            b.end = b.end.max(n.end());
                            b.size += 1;
                            part_of.push(u);
                        }
                        None => {
                            blk.push(Blk {
                                start: n.start,
                                end: n.end(),
                                size: 1,
                                child: c,
                            });
                            child_part.insert(c, blk.len() as u32 - 1);
                            part_of.push(blk.len() as u32 - 1);
                        }
                    }
                }
                let cuts = split_cuts(&blk, most, at, &self.ctx.meter);
                if !cuts.is_empty() {
                    pieces += cuts.len();
                    plans.push((k, blk, part_of, cuts));
                }
            }
            too_many_groups(&table, pieces)?;
            for (k, blk, part_of, cuts) in plans {
                let r = gs.ids[k];
                let info = table.get(r).cloned().unwrap_or_default();
                let ids: Vec<u32> = cuts
                    .iter()
                    .map(|_| {
                        table.add(GroupInfo {
                            parent: 0,
                            link: 0,
                            locked: false,
                            ..info.clone()
                        })
                    })
                    .collect();
                // The piece of each part: the number of cuts at or before it.
                let mut piece = vec![0usize; blk.len()];
                let mut q = 0;
                for (j, p) in piece.iter_mut().enumerate() {
                    while q < cuts.len() && cuts[q] <= j {
                        q += 1;
                    }
                    *p = q;
                }
                for (j, b) in blk.iter().enumerate() {
                    if piece[j] > 0
                        && b.child != 0
                        && let Some(e) = table.get_mut(b.child)
                    {
                        e.parent = ids[piece[j] - 1];
                    }
                }
                for (&i, &u) in gs.get(k).iter().zip(&part_of) {
                    let (b, q) = (&blk[u as usize], piece[u as usize]);
                    if q > 0 && b.child == 0 {
                        gid[i as usize] = ids[q - 1];
                    }
                }
            }
            if !merged.is_empty() || pieces > 0 {
                notes_msg.push(format!(
                    "{} group(s) merged, {} new from splits.",
                    merged.len(),
                    pieces
                ));
            }
        }
        Ok((table, gid, roots.len(), new_id, notes_msg))
    }

    fn plan(&self, src: &Source) -> Result<Planned, TransformError> {
        let ns = &src.notes;
        let (v, w) = (&self.v8, &self.new);
        let (mut table, gid, roots, new_id, mut extra) = self.structure(src)?;
        let gs = Groups::of(&gid, &table);
        let g_n = gs.len();
        let (own, tord) = Self::own(ns, &gs);
        let units = self.units(ns, &gs, &tord, src);
        // ---- content: n07 shape copy, n09 thinning.
        let source = match w.shape {
            Some((s, number, _)) if g_n > 1 => Some(match s {
                0 => tord[0],
                1 => {
                    let mut best = tord[0];
                    for &k in &tord {
                        if gs.get(k).len() > gs.get(best).len() {
                            best = k;
                        }
                    }
                    best
                }
                _ => tord[((number - 1) as usize).min(g_n - 1)],
            }),
            _ => None,
        };
        let work: u64 = (0..g_n)
            .map(|k| match source {
                Some(s) => gs.get(s).len() as u64,
                None => gs.get(k).len() as u64,
            })
            .sum();
        if work > MAX_WORK {
            return Ok(Planned::Over(
                work.saturating_mul(v.copies()) + gs.free.len() as u64,
            ));
        }
        let mut wv: Vec<W> = Vec::with_capacity(work as usize);
        let mut wr = vec![(0usize, 0usize); g_n];
        for &k in &tord {
            let a = wv.len();
            match source {
                Some(s) if s != k => {
                    let (sa, ta) = (gs.get(s)[0] as usize, gs.get(k)[0] as usize);
                    let (sa, ta) = (&ns[sa], &ns[ta]);
                    let f = match w.shape {
                        Some((_, _, true)) => own[k].span() as f64 / own[s].span().max(1) as f64,
                        _ => 1.0,
                    };
                    let t0 = i128::from(ta.start);
                    for &i in gs.get(s) {
                        let n = &ns[i as usize];
                        let rs = i128::from(n.start) - i128::from(sa.start);
                        let (s1, d1) = if f == 1.0 {
                            (t0 + rs, i128::from(n.dur))
                        } else {
                            let re = rs + i128::from(n.dur);
                            let s1 = t0 + (rs as f64 * f).round() as i128;
                            let e1 = t0 + (re as f64 * f).round() as i128;
                            (s1, (e1 - s1).max(1))
                        };
                        wv.push(W {
                            s: s1,
                            d: d1,
                            p: i32::from(ta.pitch) + i32::from(n.pitch) - i32::from(sa.pitch),
                            n: i,
                            g: k as u32,
                            copy: true,
                        });
                    }
                }
                _ => {
                    for &i in gs.get(k) {
                        let n = &ns[i as usize];
                        wv.push(W {
                            s: i128::from(n.start),
                            d: i128::from(n.dur),
                            p: i32::from(n.pitch),
                            n: i,
                            g: k as u32,
                            copy: false,
                        });
                    }
                }
            }
            wr[k] = (a, wv.len());
        }
        if let Some(s) = source {
            extra.push(format!(
                "The shape of group {} copied onto {} group(s).",
                tord.iter().position(|&k| k == s).unwrap_or(0) + 1,
                g_n - 1
            ));
        }
        if let Some((count, keep)) = w.thin {
            let mut kept = vec![false; wv.len()];
            let mut removed = 0usize;
            for (k, &(a, b)) in wr.iter().enumerate() {
                let c = b - a;
                let most = self.sv_int("n09.count", count, &units, k).max(1) as usize;
                if c <= most {
                    kept[a..b].iter_mut().for_each(|x| *x = true);
                    continue;
                }
                removed += c - most;
                thin_group(&wv[a..b], ns, most, keep, &mut kept[a..b]);
            }
            if removed > 0 {
                // Compact in the build order (groups in time order).
                let mut at = 0;
                for &k in &tord {
                    let (a, b) = wr[k];
                    let a2 = at;
                    for i in a..b {
                        if kept[i] {
                            wv[at] = wv[i];
                            at += 1;
                        }
                    }
                    wr[k] = (a2, at);
                }
                wv.truncate(at);
                extra.push(format!("Thinning removed {removed} note(s)."));
            }
        }
        // ---- geometry.
        let box0 = wboxes(&wv, &wr);
        // v0.8 a6 / a7 on the content's boxes.
        if v.pivot.is_some() || v.mirror.is_some() {
            for k in 0..g_n {
                let (a, b) = wr[k];
                let bx = box0[k];
                // The v0.8 ratio unless a sweep drives it (then the swept
                // percentage).
                let f = v.pivot.map(|(_, f)| match &self.ctx.sweeps {
                    Some(s) if s.drives("a6.factor") => {
                        self.sv_float("a6.factor", v.pivot_pct, &units, k) / 100.0
                    }
                    _ => f,
                });
                for x in &mut wv[a..b] {
                    if let (Some((pv, _)), Some(f)) = (v.pivot, f) {
                        let c = bx.time_at(pv);
                        let a1 = c + (x.s as f64 - c) * f;
                        let e = c + ((x.s + x.d) as f64 - c) * f;
                        x.s = a1.round() as i128;
                        x.d = ((e - a1).round() as i128).max(1);
                    }
                    match v.mirror {
                        Some(0) => x.p = (bx.p0 + bx.p1 - i64::from(x.p)) as i32,
                        Some(_) => {
                            let e = x.s + x.d;
                            x.s = bx.t0 + bx.t1 - e;
                        }
                        None => {}
                    }
                }
            }
        }
        // n05 fit.
        if let Some(fit) = w.fit {
            let bx = wboxes(&wv, &wr);
            for k in 0..g_n {
                let (a, b) = wr[k];
                let x = bx[k];
                let tf = fit.len.map(|l| {
                    let l = self.sv_int("n05.len", l as i64, &units, k).max(1);
                    (x.time_at(fit.anchor), l as f64 / x.span().max(1) as f64)
                });
                let pf = fit.span.and_then(|sp| {
                    let sp = self.sv_int("n05.span", sp, &units, k).max(0);
                    (x.p1 > x.p0)
                        .then(|| (x.pitch_at(fit.anchor), sp as f64 / (x.p1 - x.p0) as f64))
                });
                for y in &mut wv[a..b] {
                    if let Some((c, f)) = tf {
                        scale_time(y, c, f);
                    }
                    if let Some((c, g)) = pf {
                        y.p = (c + (f64::from(y.p) - c) * g).round() as i32;
                    }
                }
            }
        }
        // n11 scale series.
        if let Some((f, pivot, by_pitch)) = w.series {
            let bx = wboxes(&wv, &wr);
            let mut acc = 1.0f64;
            for (j, &k) in order_of(&bx, &gs.ids, by_pitch).iter().enumerate() {
                if j > 0 {
                    acc *= self.sv_float("n11.factor", f, &units, k) / 100.0;
                }
                let c = bx[k].time_at(pivot);
                let (a, b) = wr[k];
                for y in &mut wv[a..b] {
                    scale_time(y, c, acc);
                }
            }
        }
        // n02 permutation, n03 stagger: whole groups moved in time.
        let mut bx = wboxes(&wv, &wr);
        let mut moved = vec![0i128; g_n];
        if let Some((order, rotate, list)) = &w.perm
            && g_n > 1
        {
            let to = order_of(&bx, &gs.ids, false);
            let slots: Vec<i128> = to.iter().map(|&k| bx[k].t0).collect();
            let n = g_n as i64;
            let ranked = |by_pitch: bool| {
                let mut o: Vec<usize> = (0..g_n).collect();
                if by_pitch {
                    o.sort_unstable_by_key(|&k| (bx[k].p0, bx[k].p1, bx[k].t0, gs.ids[k]));
                } else {
                    o.sort_unstable_by_key(|&k| (bx[k].span(), bx[k].t0, gs.ids[k]));
                }
                let mut slot = vec![0usize; g_n];
                for (j, &k) in o.iter().enumerate() {
                    slot[k] = j;
                }
                slot
            };
            let slot: Vec<usize> = match order {
                0 => {
                    let mut s = vec![0; g_n];
                    for (j, &k) in to.iter().enumerate() {
                        s[k] = g_n - 1 - j;
                    }
                    s
                }
                1 => {
                    let mut s = vec![0; g_n];
                    for (j, &k) in to.iter().enumerate() {
                        s[k] = (j as i64 + rotate).rem_euclid(n) as usize;
                    }
                    s
                }
                2 => ranked(true),
                3 => ranked(false),
                _ => {
                    let mut s = vec![0; g_n];
                    for (j, &k) in to.iter().enumerate() {
                        s[k] = if list.is_empty() {
                            j
                        } else {
                            list[j % list.len()].rem_euclid(n) as usize
                        };
                    }
                    s
                }
            };
            for k in 0..g_n {
                moved[k] += slots[slot[k]] - bx[k].t0;
            }
        }
        if let Some((shift, by_pitch)) = w.stagger {
            let cur: Vec<B> = bx
                .iter()
                .zip(&moved)
                .map(|(b, &m)| B {
                    t0: b.t0 + m,
                    t1: b.t1 + m,
                    ..*b
                })
                .collect();
            let mut off = 0i128;
            for (j, &k) in order_of(&cur, &gs.ids, by_pitch).iter().enumerate() {
                if j > 0 {
                    off += i128::from(self.sv_int("n03.shift", shift, &units, k));
                }
                moved[k] += off;
            }
        }
        for k in 0..g_n {
            if moved[k] != 0 {
                let (a, b) = wr[k];
                for y in &mut wv[a..b] {
                    y.s += moved[k];
                }
                bx[k].t0 += moved[k];
                bx[k].t1 += moved[k];
            }
        }
        // v0.8 a8 (each group in the content's time order moves k × step).
        if let Some(step) = v.iso {
            let mut acc = 0i64;
            for &k in &order_of(&box0, &gs.ids, false) {
                acc = acc.saturating_add(self.sv_int("a8.step", step, &units, k));
                let (a, b) = wr[k];
                for y in &mut wv[a..b] {
                    y.p = (i64::from(y.p) + acc).clamp(-(1 << 30), 1 << 30) as i32;
                }
                bx[k].p0 += acc;
                bx[k].p1 += acc;
            }
        }
        // n08 degree sequence.
        if let Some((degrees, scale, by_pitch, offset)) = &w.degrees {
            for (j, &k) in order_of(&bx, &gs.ids, *by_pitch).iter().enumerate() {
                let base = if degrees.is_empty() {
                    0
                } else {
                    degrees[j % degrees.len()]
                };
                let deg = base + self.sv_int("n08.offset", *offset, &units, k);
                let (a, b) = wr[k];
                for y in &mut wv[a..b] {
                    let (dd, off) = scale.degree_of(i64::from(y.p));
                    y.p = (scale.pitch_of(dd + deg) + off).clamp(-(1 << 30), 1 << 30) as i32;
                }
            }
        }
        // n04 fill: repeat or stretch each group until the next one starts.
        let mut fills = vec![(0u64, 0i128); g_n];
        if let Some((stretch, gap, per_track)) = w.fill {
            let o = order_of(&bx, &gs.ids, false);
            let track_of = |k: usize| ns[gs.get(k)[0] as usize].track;
            let mut next: HashMap<u16, i128> = HashMap::new();
            let mut any: Option<i128> = None;
            let mut repeats = 0u64;
            for &k in o.iter().rev() {
                let key = if per_track { track_of(k) } else { 0 };
                let nt = if per_track {
                    next.get(&key).copied()
                } else {
                    any
                };
                let x = bx[k];
                if let Some(nt) = nt {
                    let gap = i128::from(self.sv_int("n04.gap", gap as i64, &units, k).max(0));
                    let span = x.span().max(1);
                    if stretch {
                        let end = nt - gap;
                        if end > x.t0 {
                            let f = (end - x.t0) as f64 / span as f64;
                            let (a, b) = wr[k];
                            for y in &mut wv[a..b] {
                                scale_time(y, x.t0 as f64, f);
                            }
                        }
                    } else {
                        let period = span + gap;
                        let room = nt - x.t0;
                        let r = if room >= 2 * period {
                            (room / period - 1) as u64
                        } else {
                            0
                        };
                        repeats =
                            repeats.saturating_add(r.saturating_mul((wr[k].1 - wr[k].0) as u64));
                        fills[k] = (r, period);
                    }
                }
                if per_track {
                    next.insert(key, x.t0);
                } else {
                    any = Some(x.t0);
                }
            }
            if repeats > 0 {
                extra.push(format!("Group fill added {repeats} note(s)."));
            }
        }
        // n10 index mapping (applied as the notes are written).
        let mut map = Vec::new();
        if let Some((_, curve, lo, hi, by_pitch)) = &w.map {
            let bx = if *by_pitch {
                wboxes(&wv, &wr)
            } else {
                bx.clone()
            };
            map = vec![0i64; g_n];
            for (j, &k) in order_of(&bx, &gs.ids, *by_pitch).iter().enumerate() {
                let u = if g_n > 1 {
                    j as f64 / (g_n - 1) as f64
                } else {
                    0.0
                };
                map[k] = (*lo as f64 + curve.eval(u) * (*hi - *lo) as f64).round() as i64;
            }
        }
        // v0.8 a9: clone groups (ids by top-level group id, as v0.8).
        let mut span0 = Vec::new();
        let mut clone_ids = Vec::new();
        if let Some((count, gap)) = v.clones {
            span0 = box0.iter().map(|b| b.span() + i128::from(gap)).collect();
            clone_ids.reserve(g_n * count as usize);
            for &r in &gs.ids {
                let set = match table.get(r) {
                    Some(g) if g.link != 0 => g.link,
                    _ => r,
                };
                if let Some(g) = table.get_mut(r) {
                    g.link = set;
                }
                let info = table.get(r).cloned().unwrap_or_default();
                for _ in 1..=count {
                    clone_ids.push(table.add(GroupInfo {
                        parent: 0,
                        link: set,
                        locked: false,
                        ..info.clone()
                    }));
                }
            }
        }
        let mut message = match (v.action, new_id) {
            (0, Some(id)) => format!("Group {id} created."),
            (0, None) => "The selection was grouped automatically.".into(),
            (1, _) => "The selected notes leave their innermost group.".into(),
            _ => format!("{roots} group(s) in the selection."),
        };
        for e in extra {
            message.push(' ');
            message.push_str(&e);
        }
        Ok(Planned::Full(Box::new(Plan {
            table,
            gid,
            groups: gs,
            w: wv,
            wr,
            fills,
            span0,
            clone_ids,
            map,
            message,
        })))
    }

    fn emit(&self, src: &Source, pl: Plan, out: &mut Output) -> Result<(), TransformError> {
        let ns = &src.notes;
        let clones = self.v8.clones.map_or(0, |c| c.0);
        let target = self.new.map.as_ref().map(|m| m.0);
        // V09-0143: each group's place in time (1 = the earliest by its
        // first note; ungrouped notes 0), the driver dimension.
        let place: Vec<u32> = {
            let n = pl.groups.ids.len();
            let mut by: Vec<(u64, usize)> = (0..n)
                .map(|k| {
                    let t0 = pl.groups.get(k).iter().map(|&i| ns[i as usize].start).min();
                    (t0.unwrap_or(u64::MAX), k)
                })
                .collect();
            by.sort_unstable();
            let mut place = vec![0u32; n];
            for (r, &(_, k)) in by.iter().enumerate() {
                place[k] = u32::try_from(r + 1).unwrap_or(u32::MAX);
            }
            place
        };
        // One work note with its repeats and clones.
        let write = |x: &W, out: &mut Output| -> Result<(), TransformError> {
            let k = x.g as usize;
            out.set_dim(place.get(k).copied().unwrap_or(0));
            let mut t = ns[x.n as usize];
            if x.copy {
                let anchor = &ns[pl.groups.get(k)[0] as usize];
                t.track = anchor.track;
                t.ch = anchor.ch;
                t.sub = anchor.sub;
                t.group = pl.groups.ids[k];
            } else {
                t.group = pl.gid[x.n as usize];
            }
            let (mut d, mut vel) = (x.d, i64::from(t.vel));
            if let (Some(tg), Some(&val)) = (target, pl.map.get(k)) {
                match tg {
                    0 => vel = val,
                    1 => d = ((d * i128::from(val.max(0)) + 50) / 100).max(1),
                    2 => t.ch = (val.clamp(1, 16) - 1) as u8,
                    _ => t.sub = sub_at(src, t.track, val, t.sub),
                }
            }
            let (fills, period) = pl.fills[k];
            for f in 0..=fills {
                let s = x.s + period * i128::from(f);
                out.emit(s, d, i64::from(x.p), vel, &t);
                for c in 1..=clones {
                    let mut t2 = t;
                    t2.group = pl.clone_ids[k * clones as usize + c as usize - 1];
                    out.emit(s + pl.span0[k] * i128::from(c), d, i64::from(x.p), vel, &t2);
                }
                out.check()?;
            }
            Ok(())
        };
        // A note without a group, as it is.
        let free = |i: usize, out: &mut Output| -> Result<(), TransformError> {
            out.set_dim(0);
            let n = &ns[i];
            let mut t = *n;
            t.group = 0;
            out.emit(
                i128::from(n.start),
                i128::from(n.dur),
                i64::from(n.pitch),
                i64::from(n.vel),
                &t,
            );
            out.check()
        };
        if pl.w.iter().any(|x| x.copy) {
            // n07: group by group.
            for &i in &pl.groups.free {
                free(i as usize, out)?;
            }
            for x in &pl.w {
                write(x, out)?;
            }
        } else {
            // In the selection's order: groups interleaved in time stay
            // nearly sorted for the run's final sort.
            let mut at = vec![u32::MAX; ns.len()];
            for (j, x) in pl.w.iter().enumerate() {
                at[x.n as usize] = j as u32;
            }
            for (i, &j) in at.iter().enumerate() {
                if j != u32::MAX {
                    write(&pl.w[j as usize], out)?;
                } else if pl.gid[i] == 0 {
                    free(i, out)?;
                }
            }
        }
        out.message = Some(pl.message);
        out.groups = Some(pl.table);
        Ok(())
    }
}

/// n09: marks the `most` notes of one group's work notes `w` to keep.
fn thin_group(w: &[W], ns: &[TNote], most: usize, keep: usize, kept: &mut [bool]) {
    let c = w.len();
    match keep {
        0 => {
            // Outline: the lowest and highest note of every onset.
            let mut outline = Vec::new();
            let mut inner = Vec::new();
            let mut a = 0;
            while a < c {
                let mut b = a + 1;
                while b < c && w[b].s == w[a].s {
                    b += 1;
                }
                let lo = (a..b).min_by_key(|&j| (w[j].p, j)).unwrap_or(a);
                let hi = (a..b)
                    .max_by_key(|&j| (w[j].p, usize::MAX - j))
                    .unwrap_or(a);
                for j in a..b {
                    if j == lo || j == hi {
                        outline.push(j);
                    } else {
                        inner.push(j);
                    }
                }
                a = b;
            }
            if outline.len() >= most {
                for i in 0..most {
                    kept[outline[i * outline.len() / most]] = true;
                }
            } else {
                for &j in &outline {
                    kept[j] = true;
                }
                let rest = most - outline.len();
                for i in 0..rest {
                    kept[inner[i * inner.len() / rest]] = true;
                }
            }
        }
        1 => {
            let mut o: Vec<usize> = (0..c).collect();
            o.sort_unstable_by_key(|&j| {
                (
                    std::cmp::Reverse(ns[w[j].n as usize].vel),
                    w[j].s,
                    w[j].p,
                    j,
                )
            });
            for &j in &o[..most] {
                kept[j] = true;
            }
        }
        _ => {
            for i in 0..most {
                kept[i * c / most] = true;
            }
        }
    }
}

impl Kernel for Group9 {
    fn growth(&self) -> Growth {
        Growth::Generate(self.bound(self.count))
    }

    /// From the statistics alone (the exact count is
    /// [`Kernel::estimate_source`]'s, which the run's preflight uses).
    fn estimate(&self, stats: &super::super::SourceStats) -> Estimate {
        let n = stats.count as u64;
        Estimate::for_notes(n, self.bound(n))
    }

    /// Exact when n07 or n04 (repeat) can add notes: the run's plan counted
    /// without writing a note.
    fn estimate_source(&self, src: &Source) -> Estimate {
        let n = src.notes.len() as u64;
        if !self.new.grows() {
            let mut e = Estimate::for_notes(n, n.saturating_mul(self.v8.copies()));
            e.temp_bytes = e.temp_bytes.saturating_add(n.saturating_mul(W_BYTES));
            return e;
        }
        match self.plan(src) {
            Ok(Planned::Full(p)) => {
                let mut e = Estimate::for_notes(n, p.emitted(self.v8.clones.map_or(0, |c| c.0)));
                e.temp_bytes = e
                    .temp_bytes
                    .saturating_add((p.w.len() as u64).saturating_mul(W_BYTES));
                e
            }
            Ok(Planned::Over(c)) => Estimate::for_notes(n, c),
            // The run reports the sentence.
            Err(_) => Estimate::for_notes(n, n.saturating_mul(self.v8.copies())),
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        match self.plan(src)? {
            Planned::Full(p) => self.emit(src, *p, out)?,
            Planned::Over(c) => {
                return Err(TransformError::TooLarge(format!(
                    "Group shape copy would write about {} notes; select fewer groups or a smaller source group.",
                    crate::midi::group_digits(c)
                )));
            }
        }
        out.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_ids_are_sorted_and_unique_on_both_paths() {
        // Compact range (table) and a sparse one (sort) give the same.
        let dense = vec![5, 0, 3, 5, 9, 3, 0, 7];
        assert_eq!(distinct_ids(dense), vec![3, 5, 7, 9]);
        let sparse = vec![4_000_000_000, 7, 0, 7, 3_000_000_000];
        assert_eq!(distinct_ids(sparse), vec![7, 3_000_000_000, 4_000_000_000]);
        assert!(distinct_ids(vec![0, 0]).is_empty());
    }

    fn blk(start: u64, end: u64, size: usize) -> Blk {
        Blk {
            start,
            end,
            size,
            child: 0,
        }
    }

    #[test]
    fn split_cuts_keep_pieces_within_the_limit() {
        let m = MeterMap::new(crate::model::timing::Division::ppqn(960), &[(0, 4, 2)]);
        // A part larger than the limit is a piece of its own, also last.
        let v = vec![blk(0, 10, 1), blk(20, 30, 1), blk(40, 50, 9)];
        assert_eq!(split_cuts(&v, 4, 0, &m), vec![2]);
        assert_eq!(split_cuts(&v, 4, 1, &m), vec![2]);
        // Every piece of single notes holds at most the limit, in every mode.
        let notes: Vec<Blk> = (0..37u64)
            .map(|i| blk(i * 700 + (i % 5) * 300, i * 700 + 650, 1))
            .collect();
        for at in 0..3 {
            let cuts = split_cuts(&notes, 5, at, &m);
            let mut edges = vec![0];
            edges.extend(&cuts);
            edges.push(notes.len());
            assert!(
                edges.windows(2).all(|w| w[0] < w[1] && w[1] - w[0] <= 5),
                "{at}"
            );
        }
    }
}
