//! Geometry tools on the shared engine (`transform/geom.rs`): Skew, Distort,
//! Perspective, Warp and Shear. Each is a continuous map of the selection's
//! unit box (u = time, v = pitch rows), followed by the shared controls (pivot,
//! scale, position, undefined area) and deterministic rasterization (with
//! optional Duplicate / Fill). Maps are pure functions of the note and the
//! parameters, so Preview and Apply agree exactly.

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Geom, Raster, geom_specs};
use super::super::output::Mark;
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::metric_position::{bar_bounds, metric_position};
use super::super::shared::phrase_detect::Phrases;
use super::super::shared::seeded_stream::{note_key, salt_of, stream};
use super::super::shared::tempo_time::TempoTime;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::{Contextual, art_grid_values, boxed, note_value, per_track};
use std::f64::consts::PI;

/// Builds the kernel of a geometry tool from its unit-space map.
pub(super) fn geom_kernel<M>(
    g: Geom,
    tracks: Vec<SourceTrack>,
    map: M,
) -> Result<Box<dyn Kernel>, TransformError>
where
    M: Fn(&TNote, f64, f64) -> (f64, f64) + Send + Sync + 'static,
{
    let per = g.per_note();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                if !g.rasterize(n, &|u, v| map(n, u, v), &tracks, out) {
                    break;
                }
                out.check()?;
            }
            Ok(())
        },
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

pub(super) fn spec(
    id: &'static str,
    name: &'static str,
    summary: &'static str,
    mut p: Vec<ParamSpec>,
    advanced: [&'static str; 10],
) -> ToolSpec {
    p.extend(geom_specs());
    p.extend(boundary::specs(PitchPolicy::Discard));
    ToolSpec {
        id,
        name,
        family: Family::Geometry,
        summary,
        params: p,
        advanced: advanced.to_vec(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// Subchannel position of a note in its track's table.
pub(super) fn sub_pos(tracks: &[SourceTrack], n: &TNote) -> usize {
    tracks
        .get(usize::from(n.track))
        .and_then(|t| t.sub_index(n.sub))
        .unwrap_or(0)
}

#[inline]
pub(super) fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Catmull-Rom interpolation of `pts` (values at equal spacing over 0..1).
pub(super) fn catmull(pts: &[f64], x: f64) -> f64 {
    match pts.len() {
        0 => 0.0,
        1 => pts[0],
        n => {
            let s = x.clamp(0.0, 1.0) * (n - 1) as f64;
            let i = (s.floor() as usize).min(n - 2);
            let t = s - i as f64;
            let p = |k: isize| pts[(i as isize + k).clamp(0, n as isize - 1) as usize];
            let (p0, p1, p2, p3) = (p(-1), p(0), p(1), p(2));
            0.5 * ((2.0 * p1)
                + (-p0 + p2) * t
                + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t)
        }
    }
}

/// Piecewise-linear interpolation of `pts` at equal spacing.
pub(super) fn linear(pts: &[f64], x: f64) -> f64 {
    match pts.len() {
        0 => 0.0,
        1 => pts[0],
        n => {
            let s = x.clamp(0.0, 1.0) * (n - 1) as f64;
            let i = (s.floor() as usize).min(n - 2);
            let t = s - i as f64;
            pts[i] + (pts[i + 1] - pts[i]) * t
        }
    }
}

pub(super) fn permille(v: &[i64]) -> Vec<f64> {
    v.iter().map(|&x| x as f64 / 1000.0).collect()
}

// ======================================================================= Skew

pub const SKEW: ToolDef = ToolDef {
    id: "skew",
    spec: skew_spec,
    build: skew_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SKEW_HELP: &[(&str, &str)] = &[
    (
        "amount",
        "How far the top of the selection moves in time compared with the pivot, as a share of the box's width per box height (negative slants the other way).",
    ),
    (
        "a1",
        "Sets the slant by an angle instead of the X skew amount.",
    ),
    (
        "a1.angle",
        "The angle of the slant, in degrees (0 = no slant).",
    ),
    (
        "a2",
        "Also slants in pitch: notes move up or down in proportion to their distance in time from the pivot.",
    ),
    (
        "a2.amount",
        "How far the right edge moves in pitch, as a share of the box's height per box width (negative moves it down).",
    ),
    (
        "a3",
        "Links the two directions, so each one also follows the other's movement.",
    ),
    (
        "a3.amount",
        "How strongly the second direction follows the first one's movement.",
    ),
    (
        "a4",
        "Shifts notes by a curve of their height instead of a straight slant.",
    ),
    (
        "a4.curve",
        "How far each row moves in time, by its height in the box.",
    ),
    (
        "a5",
        "Divides the selection into pitch bands and slants each band from its own bottom, like a sawtooth.",
    ),
    (
        "a5.zones",
        "How many pitch bands the selection is divided into.",
    ),
    (
        "a6",
        "Changes the slant amount from the bottom of the selection to its top.",
    ),
    ("a6.lo", "The slant amount at the lowest note."),
    ("a6.hi", "The slant amount at the highest note."),
    (
        "a7",
        "Changes the slant amount over the selection's time with a curve.",
    ),
    (
        "a7.curve",
        "The slant's strength over the selection's time (1 = the full amount).",
    ),
    (
        "a8",
        "Slants both halves away from the pivot row, making a chevron shape.",
    ),
    ("a9", "Slants every other subchannel the opposite way."),
    (
        "a10",
        "Also places copies between the original and the slanted position, filling the area it sweeps.",
    ),
    (
        "a10.copies",
        "How many copies lie between the start and the end position.",
    ),
    (
        "n01",
        "Turns the slant into a staircase: rows move in steps and starts snap to a grid.",
    ),
    ("n01.grid", "The grid the shifted starts snap to."),
    ("n01.rows", "How many rows share one step of the staircase."),
    (
        "n01.strength",
        "How far the starts are pulled onto the grid (100 % = exactly on it).",
    ),
    (
        "n02",
        "Makes the slant go back and forth over time, in a zigzag.",
    ),
    ("n02.period", "How long one back-and-forth is, in ticks."),
    ("n02.phase", "Where the zigzag starts within its period."),
    (
        "n02.shape",
        "The zigzag's shape: straight up and down (Triangle), a ramp that drops back (Saw), or smooth (Sine).",
    ),
    (
        "n03",
        "Also slants the note ends by their own amount, so notes get longer or shorter with their height.",
    ),
    (
        "n03.amount",
        "How far the note ends move, as a share of the box's width per box height.",
    ),
    (
        "n03.anchor",
        "Which part of each note stays in place: its start, its end or its center.",
    ),
    (
        "n04",
        "Slants each chord, bar, group or phrase around its own pivot instead of the whole selection's.",
    ),
    ("n04.unit", "The unit that is slanted on its own."),
    (
        "n04.pivot",
        "The row each unit is slanted around: its bottom, center or top.",
    ),
    (
        "n04.tol",
        "Notes starting within this many ticks of each other count as one chord.",
    ),
    (
        "n05",
        "Wraps notes that are moved out of a span back in at its other side.",
    ),
    ("n05.axis", "Whether notes wrap in time or in pitch."),
    (
        "n05.span",
        "The span notes wrap within: the selection, each bar, a number of beats, or a number of octaves.",
    ),
    (
        "n05.n",
        "How many beats (in time) or octaves (in pitch) the span is.",
    ),
    (
        "n06",
        "Sets the slant in real time: each semitone moves notes by a number of milliseconds, through the tempo.",
    ),
    (
        "n06.ms",
        "How many milliseconds each semitone above the reference moves a note (negative = earlier).",
    ),
    (
        "n06.ref",
        "The row with no movement: the lowest note or the pivot.",
    ),
    (
        "n07",
        "Changes the velocity by how far each note was moved.",
    ),
    (
        "n07.lo",
        "The velocity of notes that did not move, as a share of their own.",
    ),
    (
        "n07.hi",
        "The velocity of the notes that moved farthest, as a share of their own.",
    ),
    (
        "n07.curve",
        "How the velocity changes from no movement (left) to the largest movement (right).",
    ),
    (
        "n08",
        "Scales the slant so the selection's last note ends exactly at a target.",
    ),
    (
        "n08.target",
        "The target: the end of the time selection, a tick, or the next bar line.",
    ),
    ("n08.tick", "The target tick, used with Tick."),
    (
        "n09",
        "Adds a springy wobble to the slant that swings and dies down along the pitch.",
    ),
    (
        "n09.overshoot",
        "How far the wobble swings past the straight slant.",
    ),
    (
        "n09.damping",
        "How quickly the wobble dies down toward the top.",
    ),
    (
        "n09.swings",
        "How many swings the wobble makes over the selection's height.",
    ),
    (
        "n10",
        "Moves each track by a set amount more than the previous one, like a stagger.",
    ),
    (
        "n10.shift",
        "How many ticks each track moves more than the previous one (negative = earlier).",
    ),
    (
        "n10.order",
        "The order the tracks are counted in: their order in the track list, or by their lowest note.",
    ),
    ("n11", "Adds a copy slanted the opposite way."),
    ("n11.vel", "The copy's velocity as a share of the note's."),
    (
        "n11.sub",
        "How many subchannel positions the copy moves from the note's (0 = the same subchannel).",
    ),
];

fn skew_spec() -> ToolSpec {
    let mut s = spec(
        "skew",
        "Skew",
        "Slants the selection: notes move in time in proportion to their height (X skew) around the pivot.",
        vec![
            float(
                "amount",
                "X skew (time shift over the box height)",
                -400.0,
                400.0,
                50.0,
                "%",
            ),
            adv_switch(1, "X shear by angle"),
            float("a1.angle", "Angle", -80.0, 80.0, 30.0, "°").adv(1),
            adv_switch(2, "Y shear"),
            float(
                "a2.amount",
                "Y skew (pitch shift over the box width)",
                -400.0,
                400.0,
                50.0,
                "%",
            )
            .adv(2),
            adv_switch(3, "Dual-axis (coupled)"),
            float(
                "a3.amount",
                "Second-axis coupling",
                -200.0,
                200.0,
                40.0,
                "%",
            )
            .adv(3),
            adv_switch(4, "Nonlinear curve"),
            curve("a4.curve", "Shift by height", Curve::line(0.0, 1.0))
                .overshoot()
                .adv(4),
            adv_switch(5, "Segmented zones"),
            int("a5.zones", "Zones (pitch bands)", 2, 32, 3).adv(5),
            adv_switch(6, "Pitch-dependent amount"),
            float("a6.lo", "Amount at the bottom", -400.0, 400.0, 0.0, "%").adv(6),
            float("a6.hi", "Amount at the top", -400.0, 400.0, 100.0, "%").adv(6),
            adv_switch(7, "Time-dependent amount"),
            curve("a7.curve", "Amount over time", Curve::line(0.0, 1.0))
                .overshoot()
                .adv(7),
            adv_switch(8, "Mirrored (chevron)"),
            adv_switch(9, "Alternating subchannels"),
            adv_switch(10, "Fill the swept area (trail copies)"),
            int("a10.copies", "Copies between start and end", 1, 16, 3).adv(10),
            // V09-S37: functions declared by stable id (TOOL_CATALOG n01 … n11).
            flag("n01", "Grid-snapped staircase", false),
            choice("n01.grid", "Grid", art_grid_values(), 4),
            int("n01.rows", "Rows per step", 1, 24, 4),
            float("n01.strength", "Strength", 0.0, 100.0, 100.0, "%"),
            flag("n02", "Zigzag skew", false),
            ticks("n02.period", "Period", 1, 1 << 40, 1920),
            float("n02.phase", "Phase", 0.0, 100.0, 0.0, "%"),
            choice("n02.shape", "Shape", ZIGZAG, 0),
            flag("n03", "End skew", false),
            float("n03.amount", "End amount", -400.0, 400.0, 50.0, "%"),
            choice("n03.anchor", "Anchor", END_ANCHORS, 0),
            flag("n04", "Per-unit skew", false),
            choice("n04.unit", "Unit", SKEW_UNITS, 0),
            choice("n04.pivot", "Pivot", UNIT_PIVOTS, 0),
            ticks("n04.tol", "Chord tolerance", 0, 1 << 40, 30),
            flag("n05", "Wrap skew", false),
            choice("n05.axis", "Axis", WRAP_AXES, 0),
            choice("n05.span", "Span", WRAP_SPANS, 1),
            int("n05.n", "Beats or octaves", 1, 64, 4),
            flag("n06", "Real-time slope", false),
            float(
                "n06.ms",
                "Milliseconds per semitone",
                -1000.0,
                1000.0,
                10.0,
                "ms",
            ),
            choice("n06.ref", "Reference", RT_REFS, 0),
            flag("n07", "Shift depth", false),
            float("n07.lo", "Velocity at no shift", 0.0, 200.0, 100.0, "%"),
            float(
                "n07.hi",
                "Velocity at the largest shift",
                0.0,
                200.0,
                50.0,
                "%",
            ),
            curve("n07.curve", "Shape", Curve::line(0.0, 1.0)).overshoot(),
            flag("n08", "Fit to end", false),
            choice("n08.target", "Target", FIT_TARGETS, 2),
            ticks("n08.tick", "Tick", 0, 1 << 40, 7680),
            flag("n09", "Elastic skew", false),
            float("n09.overshoot", "Overshoot", 0.0, 200.0, 50.0, "%"),
            float("n09.damping", "Damping", 0.0, 100.0, 40.0, "%"),
            int("n09.swings", "Swings", 1, 16, 3),
            flag("n10", "Track stagger", false),
            int("n10.shift", "Shift per track", -100_000, 100_000, 240),
            choice("n10.order", "Order", STAGGER_ORDER, 0),
            flag("n11", "Mirror copy", false),
            float("n11.vel", "Copy velocity", 0.0, 150.0, 80.0, "%"),
            int("n11.sub", "Copy subchannel offset", -4095, 4095, 1),
        ],
        [
            "X shear by angle",
            "Y shear",
            "Dual-axis",
            "Nonlinear curve",
            "Segmented zones",
            "Pitch-dependent amount",
            "Time-dependent amount",
            "Mirrored",
            "Alternating subchannels",
            "Fill the swept area",
        ],
    );
    s.functions = [
        ("n01", "Grid-snapped staircase", 180),
        ("n02", "Zigzag skew", 130),
        ("n03", "End skew", 150),
        ("n04", "Per-unit skew", 110),
        ("n05", "Wrap skew", 190),
        ("n06", "Real-time slope", 120),
        ("n07", "Shift depth", 510),
        ("n08", "Fit to end", 170),
        ("n09", "Elastic skew", 140),
        ("n10", "Track stagger", 160),
        ("n11", "Mirror copy", 600),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, SKEW_HELP);
    s
}

/// X skew: time moves by `k` box widths per box height `h` above the pivot
/// (unit coordinates). Shared by the Skew tool and the Piano Roll's Ctrl+corner
/// gesture ([`CornerWarp`]).
#[inline]
pub(crate) fn skew_x(u: f64, h: f64, k: f64) -> f64 {
    u + k * h
}

/// Y skew: pitch moves by `a` box heights per box width `w` right of the pivot.
#[inline]
pub(crate) fn skew_y(v: f64, w: f64, a: f64) -> f64 {
    v + a * w
}

fn skew_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S38 gate (E0409): Control Sweeps run inside the v0.9 kernel, per
    // note (`definition::kernel_sweeps`).
    if SKEW_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return skew9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let mut ax = p.float("amount") / 100.0;
    if p.adv(1) {
        ax = (p.float("a1.angle") * PI / 180.0).tan();
    }
    let ay = p.adv(2).then(|| p.float("a2.amount") / 100.0);
    let dual = p.adv(3).then(|| p.float("a3.amount") / 100.0);
    let shape = p.adv(4).then(|| p.curve("a4.curve"));
    let zones = p.adv(5).then(|| p.int("a5.zones").max(2) as f64);
    let ramp = p
        .adv(6)
        .then(|| (p.float("a6.lo") / 100.0, p.float("a6.hi") / 100.0));
    let tcurve = p.adv(7).then(|| p.curve("a7.curve"));
    let mirrored = p.adv(8);
    let alt = p.adv(9);
    let trail = p.adv(10).then(|| p.int("a10.copies").max(1) as u32);
    let tracks = b.tracks.to_vec();
    let (pu, pv) = g.pivot;
    let tr2 = tracks.clone();
    let map = move |n: &TNote, u: f64, v: f64| -> (f64, f64) {
        let mut h = v - pv;
        if mirrored {
            h = h.abs();
        }
        if let Some(c) = &shape {
            h = c.eval(v.clamp(0.0, 1.0)) - pv;
        }
        if let Some(z) = zones {
            // Each band skews from its own bottom.
            h = (v * z).fract() / z;
        }
        let mut k = ax;
        if let Some((lo, hi)) = ramp {
            k = lo + (hi - lo) * v.clamp(0.0, 1.0);
        }
        if let Some(c) = &tcurve {
            k *= c.eval(u.clamp(0.0, 1.0));
        }
        if alt && sub_pos(&tr2, n) % 2 == 1 {
            k = -k;
        }
        let mut u2 = skew_x(u, h, k);
        let mut v2 = v;
        if let Some(a) = ay {
            v2 = skew_y(v, u - pu, a);
        }
        if let Some(c) = dual {
            // Coupled: the second axis follows the first one's displacement.
            v2 += c * (u2 - u);
            u2 += c * (v2 - v) * 0.5;
        }
        (u2, v2)
    };
    match trail {
        None => geom_kernel(g, tracks, map),
        Some(copies) => {
            let per = g.per_note() * (u64::from(copies) + 1);
            boxed(Contextual {
                f: move |src: &Source, out: &mut Output| {
                    for n in &src.notes {
                        for c in 0..=copies {
                            // V09-0143: the trail copy (0 = the note where it
                            // was, `copies` = fully skewed).
                            out.set_dim(c);
                            let w = f64::from(c) / f64::from(copies);
                            let ok = g.rasterize(
                                n,
                                &|u, v| {
                                    let (u2, v2) = map(n, u, v);
                                    (u + (u2 - u) * w, v + (v2 - v) * w)
                                },
                                &tracks,
                                out,
                            );
                            if !ok {
                                return out.check();
                            }
                        }
                        out.check()?;
                    }
                    Ok(())
                },
                growth: Growth::Expand(per),
                estimate: None,
            })
        }
    }
}

// ------------------------------------------------------------ Skew (v0.9)

const SKEW_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const ZIGZAG: &[&str] = &["Triangle", "Saw", "Sine"];
const END_ANCHORS: &[&str] = &["Start", "End", "Center"];
const SKEW_UNITS: &[&str] = &["Chord", "Bar", "Group", "Phrase"];
const UNIT_PIVOTS: &[&str] = &["Bottom", "Center", "Top"];
const WRAP_AXES: &[&str] = &["Time", "Pitch"];
const WRAP_SPANS: &[&str] = &["Selection", "Bar", "Beats", "Octaves"];
const RT_REFS: &[&str] = &["Lowest note", "Pivot"];
const FIT_TARGETS: &[&str] = &["Time selection end", "Tick", "Next bar line"];
const STAGGER_ORDER: &[&str] = &["Track order", "Lowest pitch"];

/// "No unit" in [`SkewRun::unit_of`] (the selection's pivot).
const NO_UNIT: u32 = u32::MAX;

/// The v0.9 Skew (V09-S37 step 1): the v0.8 rule re-read from the same
/// parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions, run in their catalog order:
///
/// * 110 n04 Per-unit skew — the pivot of each chord / bar / group / phrase;
/// * 120 n06 Real-time slope — the X shift in seconds through the tempo map;
/// * 130 n02 Zigzag skew — the amount times a profile of the note's start;
/// * 140 n09 Elastic skew — the height plus a damped wobble along pitch;
/// * 150 n03 End skew — note ends shift by their own amount (lengths change);
/// * 160 n10 Track stagger — a time offset per track rank;
/// * 170 n08 Fit to end — scales the X skew so the last end lands on a target;
/// * 180 n01 Grid-snapped staircase — stepped rows, starts on a grid;
/// * 190 n05 Wrap skew — shifted notes wrap around a time or pitch span;
/// * 510 n07 Shift depth — velocity by the distance each note moved;
/// * 600 n11 Mirror copy — a copy skewed the opposite way.
///
/// Everything is a pure function of the note, its unit and the selection
/// (the shared box), so Preview and Apply agree; the shared Scale /
/// Position / Undefined area / rasterization run after it as for v0.8.
#[derive(Clone)]
struct Skew9 {
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    ax: f64,
    ay: Option<f64>,
    dual: Option<f64>,
    shape: Option<Curve>,
    zones: Option<f64>,
    ramp: Option<(f64, f64)>,
    tcurve: Option<Curve>,
    mirrored: bool,
    alt: bool,
    trail: Option<u32>,
    /// What the X amount comes from (per note with sweeps): the amount
    /// (%), a1's angle, n06's milliseconds or n08's direction only.
    amount_pct: f64,
    angle: Option<f64>,
    ms: f64,
    // ----- v0.9 functions
    /// n01: grid (ticks), rows per step, strength 0 … 1.
    stair: Option<(u64, i64, f64)>,
    /// n02: period (ticks), phase 0 … 1, shape.
    zig: Option<(f64, f64, usize)>,
    /// n03: end amount (box widths per box height), anchor 0 / 1 / ½.
    end: Option<(f64, f64)>,
    /// n04: unit, pivot, chord tolerance.
    unit: Option<(usize, usize, u64)>,
    /// n05: pitch axis, span, beats or octaves.
    wrap: Option<(bool, usize, i64)>,
    /// n06: reference = the lowest note (else the pivot).
    rt_lowest: Option<bool>,
    /// n07: velocity factor at no shift and at the largest shift, shape.
    depth: Option<(f64, f64, Curve)>,
    /// n08: target, tick.
    fit: Option<(usize, u64)>,
    /// n09: overshoot, decay rate, swings.
    elastic: Option<(f64, f64, f64)>,
    /// n10: ticks per track rank, rank by the lowest pitch.
    stagger: Option<(f64, bool)>,
    /// n11: copy velocity factor, subchannel offset.
    mirror: Option<(f64, i64)>,
    /// Seconds at tick 0 and per tick there (real time before the start).
    s0: f64,
    spt0: f64,
    /// One tempo throughout (no map, or a constant one): seconds are
    /// `s0 + tick · spt0` everywhere (n06 without lookups).
    linear: bool,
}

/// A unit's box for n04.
#[derive(Clone, Copy, Debug)]
struct UnitBox {
    t0: u64,
    t1: u64,
    lo: u8,
    hi: u8,
}

/// One note's values of the parameters Skew's Control Sweeps drive
/// (V09-S38 gate, E0409: evaluated per note at its exact place, inside the
/// kernel, so n04's units, n07's largest move, n08's fit and n10's ranks see
/// the whole selection).
#[derive(Clone, Copy, Debug)]
struct SVals {
    /// The X amount (box widths per box height; seconds per semitone with
    /// n06; ±1 with n08).
    ax: f64,
    rows: i64,
    strength: f64,
    phase: f64,
    end: f64,
    over: f64,
    shift: f64,
    hi: f64,
    mvel: f64,
}

/// What one run derives from its source before mapping notes.
struct SkewRun {
    unit_of: Vec<u32>,
    units: Vec<UnitBox>,
    /// n10 rank per track slot.
    stagger: Vec<f64>,
    /// Every note's swept values (`base` for all without sweeps).
    base: SVals,
    vals: Vec<SVals>,
    /// n08 scale of the X skew (1 without Fit).
    f: f64,
    /// n07 largest distance moved.
    dmax: f64,
}

/// One note's mapping settings (unit coordinates of the selection box).
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// The note's X amount and n09 overshoot ([`SVals`]).
    ax: f64,
    over: f64,
    pu: f64,
    pv: f64,
    /// σ · zigzag · fit: the factor on the X amount.
    kf: f64,
    sigma: f64,
    /// The note's start, end and centre row.
    us: f64,
    ue: f64,
    v: f64,
    /// n03: length change (box widths) and anchor.
    dl: f64,
    anchor: f64,
    /// Per-note offsets (n10, n01, n05).
    du: f64,
    dv: f64,
    /// The height and the amount on the note's centre row (every point of
    /// its centre line; the amount NaN when it varies along time: [a7]).
    h: f64,
    k: f64,
}

/// How much of the per-note pipeline [`Skew9::frame`] builds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Upto {
    /// Through n10 (Fit's intercepts).
    Stagger,
    /// Everything.
    All,
}

fn skew9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let rt = p.flag("n06");
    let fit = p.flag("n08");
    let mut ax = p.float("amount") / 100.0;
    if p.adv(1) {
        ax = (p.float("a1.angle") * PI / 180.0).tan();
    }
    if rt {
        // The slope in seconds per semitone replaces the amount.
        ax = p.float("n06.ms") / 1000.0;
    }
    if fit {
        // Fit chooses the magnitude; the amount keeps the direction.
        ax = if ax < 0.0 { -1.0 } else { 1.0 };
    }
    let ctx = b.ctx.clone();
    let s0 = ctx.seconds_at(0);
    let q = ctx.ppq.max(1);
    let spt0 = ((ctx.seconds_at(q) - s0) / q as f64).max(1e-12);
    let linear = ctx
        .tempo
        .as_ref()
        .is_none_or(|t| t.min_bpm(q) == t.max_bpm(q));
    let k = Skew9 {
        g,
        tracks: b.tracks.to_vec(),
        ax,
        amount_pct: p.float("amount"),
        angle: p.adv(1).then(|| p.float("a1.angle")),
        ms: p.float("n06.ms"),
        ay: p.adv(2).then(|| p.float("a2.amount") / 100.0),
        dual: p.adv(3).then(|| p.float("a3.amount") / 100.0),
        shape: p.adv(4).then(|| p.curve("a4.curve")),
        zones: p.adv(5).then(|| p.int("a5.zones").max(2) as f64),
        ramp: p
            .adv(6)
            .then(|| (p.float("a6.lo") / 100.0, p.float("a6.hi") / 100.0)),
        tcurve: p.adv(7).then(|| p.curve("a7.curve")),
        mirrored: p.adv(8),
        alt: p.adv(9),
        trail: p.adv(10).then(|| p.int("a10.copies").max(1) as u32),
        stair: p.flag("n01").then(|| {
            (
                note_value(p.choice("n01.grid").min(10), 0, &ctx),
                p.int("n01.rows").clamp(1, 24),
                (p.float("n01.strength") / 100.0).clamp(0.0, 1.0),
            )
        }),
        zig: p.flag("n02").then(|| {
            (
                p.ticks("n02.period").max(1) as f64,
                (p.float("n02.phase") / 100.0).clamp(0.0, 1.0),
                p.choice("n02.shape").min(2),
            )
        }),
        end: p.flag("n03").then(|| {
            (
                p.float("n03.amount") / 100.0,
                [0.0, 1.0, 0.5][p.choice("n03.anchor").min(2)],
            )
        }),
        unit: p.flag("n04").then(|| {
            (
                p.choice("n04.unit").min(3),
                p.choice("n04.pivot").min(2),
                p.ticks("n04.tol"),
            )
        }),
        wrap: p.flag("n05").then(|| {
            (
                p.choice("n05.axis") == 1,
                p.choice("n05.span").min(3),
                p.int("n05.n").clamp(1, 64),
            )
        }),
        rt_lowest: rt.then(|| p.choice("n06.ref") == 0),
        depth: p.flag("n07").then(|| {
            (
                p.float("n07.lo") / 100.0,
                p.float("n07.hi") / 100.0,
                p.curve("n07.curve"),
            )
        }),
        fit: fit.then(|| (p.choice("n08.target").min(2), p.ticks("n08.tick"))),
        elastic: p.flag("n09").then(|| {
            (
                p.float("n09.overshoot") / 100.0,
                5.0 * (p.float("n09.damping") / 100.0).clamp(0.0, 1.0),
                p.int("n09.swings").clamp(1, 16) as f64,
            )
        }),
        stagger: p
            .flag("n10")
            .then(|| (p.int("n10.shift") as f64, p.choice("n10.order") == 1)),
        mirror: p.flag("n11").then(|| {
            (
                (p.float("n11.vel") / 100.0).clamp(0.0, 1.5),
                p.int("n11.sub"),
            )
        }),
        ctx,
        s0,
        spt0,
        linear,
    };
    if let Some((0, _)) = k.fit
        && k.ctx.time_range.is_none()
    {
        return Err(TransformError::Invalid(
            "Fit to end needs a time selection: select a time range on the ruler, or choose a tick or the next bar line as the target.".into(),
        ));
    }
    if k.fit.is_some() && k.g.scale.0 <= 0.0 {
        return Err(TransformError::Invalid(
            "Fit to end needs a positive Scale X, so the last note's end can be placed on the target.".into(),
        ));
    }
    let mut per = k.g.per_note();
    if let Some(c) = k.trail {
        per *= u64::from(c) + 1;
    }
    if k.mirror.is_some() {
        per *= 2;
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

impl Skew9 {
    /// The height of point row `vh` above the pivot (box heights): the v0.8
    /// rule (mirror, curve, zones) plus n09's damped wobble.
    #[inline(always)]
    fn height(&self, fr: &Frame, vh: f64) -> f64 {
        let mut h = vh - fr.pv;
        if self.mirrored {
            h = h.abs();
        }
        if let Some(c) = &self.shape {
            h = c.eval(vh.clamp(0.0, 1.0)) - fr.pv;
        }
        if let Some(z) = self.zones {
            // Each band skews from its own bottom.
            h = (vh * z).fract() / z;
        }
        if let Some((_, decay, swings)) = self.elastic {
            let y = vh.clamp(0.0, 1.0);
            h += fr.over * (-decay * y).exp() * (PI * swings * y).sin();
        }
        h
    }

    /// The X amount at point `u` (box widths per box height; seconds per
    /// semitone with n06).
    #[inline(always)]
    fn amount(&self, fr: &Frame, n: &TNote, u: f64, vh: f64) -> f64 {
        let mut k = fr.ax;
        if let Some((lo, hi)) = self.ramp {
            k = lo + (hi - lo) * vh.clamp(0.0, 1.0);
        }
        if let Some(c) = &self.tcurve {
            k *= c.eval(u.clamp(0.0, 1.0));
        }
        if self.alt && sub_pos(&self.tracks, n) % 2 == 1 {
            k = -k;
        }
        k * fr.kf
    }

    /// [`Skew9::height`] and [`Skew9::amount`] on the centre row, kept in
    /// the frame (after its pivot and factor are final).
    #[inline(always)]
    fn cache(&self, fr: &mut Frame, n: &TNote) {
        fr.h = self.height(fr, fr.v);
        fr.k = if self.tcurve.is_none() {
            self.amount(fr, n, 0.0, fr.v)
        } else {
            f64::NAN
        };
    }

    #[inline(always)]
    fn h_at(&self, fr: &Frame, vh: f64) -> f64 {
        if vh == fr.v {
            fr.h
        } else {
            self.height(fr, vh)
        }
    }

    #[inline(always)]
    fn k_at(&self, fr: &Frame, n: &TNote, u: f64, vh: f64) -> f64 {
        if vh == fr.v && !fr.k.is_nan() {
            fr.k
        } else {
            self.amount(fr, n, u, vh)
        }
    }

    /// Song seconds of a (real) tick; linear before tick 0.
    fn secs(&self, x: f64) -> f64 {
        if x <= 0.0 || self.linear {
            return self.s0 + x * self.spt0;
        }
        let f = x.floor();
        let a = self.ctx.seconds_at(f as u64);
        let r = x - f;
        if r == 0.0 {
            a
        } else {
            a + (self.ctx.seconds_at((f as u64).saturating_add(1)) - a) * r
        }
    }

    /// The (real) tick at song seconds `s` (the inverse of [`Skew9::secs`]).
    fn ticks_at(&self, s: f64) -> f64 {
        if !s.is_finite() {
            return 0.0;
        }
        if s <= self.s0 || self.linear {
            return (s - self.s0) / self.spt0;
        }
        // The tempo map's tick (frame-rounded), corrected to the tick whose
        // span holds `s` (a step or two at most), then the fraction.
        let mut t = self.ctx.tick_at_seconds(s);
        for _ in 0..8 {
            if t == 0 || self.ctx.seconds_at(t) <= s {
                break;
            }
            t -= 1;
        }
        let mut a = self.ctx.seconds_at(t);
        for _ in 0..8 {
            let b = self.ctx.seconds_at(t.saturating_add(1));
            if b > s || t == u64::MAX {
                let w = b - a;
                return t as f64 + if w > 0.0 { ((s - a) / w).min(1.0) } else { 0.0 };
            }
            t += 1;
            a = b;
        }
        t as f64
    }

    /// The X shift of point (u, row vh) before the conversion: box widths,
    /// or seconds with n06.
    #[inline(always)]
    fn shift(&self, fr: &Frame, n: &TNote, u: f64, vh: f64) -> f64 {
        let h = self.h_at(fr, vh);
        let k = self.k_at(fr, n, u, vh);
        if self.rt_lowest.is_some() {
            k * h * self.g.bbox.rows()
        } else {
            k * h
        }
    }

    /// The X image of point (u, row vh), before the dual coupling.
    #[inline(always)]
    fn x_of(&self, fr: &Frame, n: &TNote, u: f64, vh: f64) -> f64 {
        if self.rt_lowest.is_none() {
            let h = self.h_at(fr, vh);
            let k = self.k_at(fr, n, u, vh);
            return skew_x(u, h, k);
        }
        let ds = self.shift(fr, n, u, vh);
        if ds == 0.0 {
            return u;
        }
        let b = &self.g.bbox;
        let x = b.t0 as f64 + u * b.width();
        (self.ticks_at(self.secs(x) + ds) - b.t0 as f64) / b.width()
    }

    /// The full image of point (u, v) (unit coordinates, before the shared
    /// affine part); `vh` is the row the shift is computed from (n01 steps).
    #[inline(always)]
    fn point(&self, fr: &Frame, n: &TNote, u: f64, v: f64, vh: f64) -> (f64, f64) {
        let mut u2 = self.x_of(fr, n, u, vh);
        let mut v2 = v;
        if let Some(a) = self.ay {
            v2 = skew_y(v, u - fr.pu, a * fr.sigma);
        }
        if let Some(c) = self.dual {
            // Coupled: the second axis follows the first one's displacement.
            v2 += c * (u2 - u);
            u2 += c * (v2 - v) * 0.5;
        }
        if fr.dl != 0.0 {
            let span = fr.ue - fr.us;
            let lam = if span > 0.0 { (u - fr.us) / span } else { 0.0 };
            u2 += (lam - fr.anchor) * fr.dl;
        }
        (u2 + fr.du, v2 + fr.dv)
    }

    /// Absolute tick of unit `u`.
    #[inline(always)]
    fn abs_x(&self, u: f64) -> f64 {
        self.g.bbox.t0 as f64 + u * self.g.bbox.width()
    }

    /// n02's profile at the note's start (−1 … 1).
    fn zigzag(&self, n: &TNote, phase: f64) -> f64 {
        let Some((period, _, shape)) = self.zig else {
            return 1.0;
        };
        let x = (n.start as f64 - self.g.bbox.t0 as f64) / period + phase;
        let fr = x - x.floor();
        match shape {
            0 => 1.0 - 4.0 * (fr - 0.5).abs(),
            1 => 2.0 * fr - 1.0,
            _ => (2.0 * PI * x).sin(),
        }
    }

    /// The grid tick nearest `x` (n01): steps of the grid restarting at each
    /// bar line (round half up).
    fn snap(&self, x: f64, grid: u64) -> f64 {
        let gr = grid.max(1) as f64;
        if x < 0.0 {
            return (x / gr + 0.5).floor() * gr;
        }
        let (bs, _) = bar_bounds(&self.ctx.meter, x as u64);
        let bs = bs as f64;
        bs + ((x - bs) / gr + 0.5).floor() * gr
    }

    /// Note `i`'s frame for sign `sigma` (−1: the n11 copy) and fit `f`.
    fn frame(&self, run: &SkewRun, i: usize, n: &TNote, sigma: f64, f: f64, upto: Upto) -> Frame {
        let b = &self.g.bbox;
        let w = run.vals.get(i).unwrap_or(&run.base);
        let (us, v) = b.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
        let ue = (n.end() as f64 - b.t0 as f64) / b.width();
        let (mut pu, mut pv) = self.g.pivot;
        let mut lowest = b.p0;
        if self.unit.is_some() {
            let k = run.unit_of[i];
            if k != NO_UNIT {
                let ub = run.units[k as usize];
                let (_, pivot, _) = self.unit.unwrap_or_default();
                pu = (ub.t0 as f64 + self.g.pivot.0 * (ub.t1 - ub.t0) as f64 - b.t0 as f64)
                    / b.width();
                let row = match pivot {
                    0 => f64::from(ub.lo),
                    1 => (f64::from(ub.lo) + f64::from(ub.hi) + 1.0) / 2.0,
                    _ => f64::from(ub.hi) + 1.0,
                };
                pv = (row - f64::from(b.p0)) / b.rows();
                lowest = ub.lo;
            }
        }
        if self.rt_lowest == Some(true) {
            pv = (f64::from(lowest) + 0.5 - f64::from(b.p0)) / b.rows();
        }
        let mut fr = Frame {
            ax: w.ax,
            over: w.over,
            pu,
            pv,
            kf: sigma * self.zigzag(n, w.phase) * f,
            sigma,
            us,
            ue,
            v,
            dl: 0.0,
            anchor: 0.0,
            du: 0.0,
            dv: 0.0,
            h: 0.0,
            k: 0.0,
        };
        self.cache(&mut fr, n);
        if let Some((_, anchor)) = self.end {
            let bw = b.width();
            let dl = (sigma * w.end * fr.h * bw).max(-(n.dur.saturating_sub(1) as f64));
            fr.dl = dl / bw;
            fr.anchor = anchor;
        }
        if self.stagger.is_some() {
            fr.du = run
                .stagger
                .get(usize::from(n.track))
                .copied()
                .unwrap_or(0.0)
                * w.shift
                / b.width();
        }
        if upto == Upto::Stagger {
            return fr;
        }
        if let Some((grid, _, _)) = self.stair {
            let (rows, strength) = (w.rows, w.strength);
            let x_plain = self.abs_x(self.point(&fr, n, us, v, v).0);
            let r = i64::from(n.pitch) - i64::from(b.p0);
            let stepped = (r.div_euclid(rows) * rows) as f64 + 0.5;
            let x_stair = self.abs_x(self.point(&fr, n, us, v, stepped / b.rows()).0);
            let d = strength * (self.snap(x_stair, grid) - x_plain);
            fr.du += d / b.width();
        }
        if let Some((pitch_axis, span, count)) = self.wrap {
            if pitch_axis {
                // The sampled row (the raster's) wraps into the octaves
                // holding the note (from the selection's lowest row), or
                // into the selection's rows (time spans on this axis).
                let su = match self.g.raster {
                    Raster::SampleStart => us,
                    _ => (us + ue) / 2.0,
                };
                let (_, v2) = self.point(&fr, n, su, v, v);
                let row = (f64::from(b.p0) + v2 * b.rows()).floor() as i64;
                let p0 = i64::from(b.p0);
                let (base, len) = if span == 3 {
                    let oct = 12 * count;
                    (p0 + (i64::from(n.pitch) - p0).div_euclid(oct) * oct, oct)
                } else {
                    (p0, i64::from(b.p1) - p0 + 1)
                };
                let wrapped = base + (row - base).rem_euclid(len.max(1));
                fr.dv += (wrapped - row) as f64 / b.rows();
            } else {
                // The shifted start wraps into the span holding the original
                // start (an octave span on this axis means the selection).
                let x = self.abs_x(self.point(&fr, n, us, v, v).0);
                let (a, len) = match span {
                    1 => {
                        let (bs, bl) = bar_bounds(&self.ctx.meter, n.start);
                        (bs as f64, bl as f64)
                    }
                    2 => {
                        let m = metric_position(&self.ctx.meter, n.start);
                        let l = m.beat_len.max(1).saturating_mul(count as u64) as f64;
                        let k = ((n.start - m.bar_start) as f64 / l).floor();
                        (m.bar_start as f64 + k * l, l)
                    }
                    _ => (b.t0 as f64, b.width()),
                };
                let wrapped = a + (x - a).rem_euclid(len.max(1.0));
                fr.du += (wrapped - x) / b.width();
            }
        }
        fr
    }

    /// Track slots' n10 offsets: rank (track order, or the lowest selected
    /// pitch, then track order) × the shift.
    fn stagger_table(&self, src: &Source) -> Vec<f64> {
        let Some((_, by_pitch)) = self.stagger else {
            return Vec::new();
        };
        let slots = src.tracks.len().max(1);
        let mut lowest = vec![u8::MAX; slots];
        let mut used = vec![false; slots];
        for n in &src.notes {
            let t = usize::from(n.track).min(slots - 1);
            used[t] = true;
            lowest[t] = lowest[t].min(n.pitch);
        }
        let mut keys: Vec<(u8, usize, usize)> = (0..slots)
            .filter(|&t| used[t])
            .map(|t| {
                let index = src.tracks.get(t).map_or(t, |x| x.index);
                (if by_pitch { lowest[t] } else { 0 }, index, t)
            })
            .collect();
        keys.sort_unstable();
        let mut off = vec![0.0; slots];
        for (rank, &(_, _, t)) in keys.iter().enumerate() {
            off[t] = rank as f64;
        }
        off
    }

    /// n04's units: chords / bars / phrases per track, groups across tracks.
    fn units(&self, src: &Source) -> (Vec<u32>, Vec<UnitBox>) {
        let Some((kind, _, tol)) = self.unit else {
            return (Vec::new(), Vec::new());
        };
        let notes = &src.notes;
        let mut unit_of = vec![NO_UNIT; notes.len()];
        let mut units: Vec<UnitBox> = Vec::new();
        let mut add = |idx: &[usize], unit_of: &mut Vec<u32>| {
            let k = units.len() as u32;
            let mut ub = UnitBox {
                t0: u64::MAX,
                t1: 0,
                lo: u8::MAX,
                hi: 0,
            };
            for &i in idx {
                let n = &notes[i];
                ub.t0 = ub.t0.min(n.start);
                ub.t1 = ub.t1.max(n.end());
                ub.lo = ub.lo.min(n.pitch);
                ub.hi = ub.hi.max(n.pitch);
                unit_of[i] = k;
            }
            units.push(ub);
        };
        if kind == 2 {
            let mut by_group: std::collections::BTreeMap<u32, Vec<usize>> = Default::default();
            for (i, n) in notes.iter().enumerate() {
                if n.group != 0 {
                    by_group.entry(n.group).or_default().push(i);
                }
            }
            for idx in by_group.values() {
                add(idx, &mut unit_of);
            }
            return (unit_of, units);
        }
        for idx in per_track(src) {
            if idx.is_empty() {
                continue;
            }
            match kind {
                0 => {
                    for c in ChordGroups::of(notes, &idx, tol).iter() {
                        add(c, &mut unit_of);
                    }
                }
                3 => {
                    let ph = Phrases::of(notes, &idx, self.ctx.ppq.max(1), None);
                    for p in ph.iter(&idx) {
                        add(p, &mut unit_of);
                    }
                }
                _ => {
                    let mut a = 0;
                    while a < idx.len() {
                        let bar = bar_bounds(&self.ctx.meter, notes[idx[a]].start).0;
                        let mut e = a + 1;
                        while e < idx.len()
                            && bar_bounds(&self.ctx.meter, notes[idx[e]].start).0 == bar
                        {
                            e += 1;
                        }
                        add(&idx[a..e], &mut unit_of);
                        a = e;
                    }
                }
            }
        }
        (unit_of, units)
    }

    /// n08: the factor on the X skew that puts the latest end of the result
    /// on the target (in the final coordinates, through the shared affine
    /// part). Every note's end moves monotonically with the factor, so the
    /// factor is the first crossing of a note's own target factor.
    fn fit_factor(&self, src: &Source, run: &SkewRun) -> Result<f64, TransformError> {
        let Some((target, tick)) = self.fit else {
            return Ok(1.0);
        };
        let b = &self.g.bbox;
        let target = match target {
            0 => self.ctx.time_range.map_or(0, |r| r.1) as f64,
            1 => tick as f64,
            _ => {
                let last = src.notes.iter().map(TNote::end).max().unwrap_or(0);
                let (bs, bl) = bar_bounds(&self.ctx.meter, last);
                bs.saturating_add(bl) as f64
            }
        };
        // The target before the shared scale / position (Scale X > 0).
        let (w, pug, sx) = (b.width(), self.g.pivot.0, self.g.scale.0);
        let xt = b.t0 as f64 + (pug + ((target - self.g.pos.0 - b.t0 as f64) / w - pug) / sx) * w;
        let beta = self.dual.map_or(1.0, |c| 1.0 + 0.5 * c * c);
        let mut lo = 0.0f64; // f ≥ every note that must come back down
        let mut hi = f64::INFINITY; // f ≤ every note that must not pass
        let mut reach = f64::INFINITY; // the first note reaching xt
        let mut top = f64::NEG_INFINITY; // the latest end at f = 0
        let mut any_up = false;
        for (i, n) in src.notes.iter().enumerate() {
            let fr0 = self.frame(run, i, n, 1.0, 0.0, Upto::Stagger);
            let g0 = self.abs_x(self.point(&fr0, n, fr0.ue, fr0.v, fr0.v).0);
            top = top.max(g0);
            let phase = run.vals.get(i).unwrap_or(&run.base).phase;
            let mut fr1 = Frame {
                kf: self.zigzag(n, phase),
                ..fr0
            };
            self.cache(&mut fr1, n);
            // The note's own factor: where its end reaches xt.
            let (slope, fi) = if self.rt_lowest.is_some() {
                let ds = self.shift(&fr1, n, fr1.ue, fr1.v);
                let xe = n.end() as f64;
                let y = xe + (xt - g0) / beta;
                (ds, (self.secs(y) - self.secs(xe)) / ds)
            } else {
                let g1 = self.abs_x(self.point(&fr1, n, fr1.ue, fr1.v, fr1.v).0);
                (g1 - g0, (xt - g0) / (g1 - g0))
            };
            if slope > 0.0 {
                any_up = true;
                if g0 <= xt {
                    reach = reach.min(fi);
                    hi = hi.min(fi);
                } else {
                    return Err(TransformError::Invalid(
                        "Fit to end cannot reach the target: a note ending after it moves later with this skew direction. Reverse the X skew or choose a later target.".into(),
                    ));
                }
            } else if g0 > xt {
                if slope < 0.0 {
                    lo = lo.max(fi);
                } else {
                    return Err(TransformError::Invalid(
                        "Fit to end cannot reach the target: a note ending after it does not move with the skew (it lies on the pivot row). Move the pivot or choose a later target.".into(),
                    ));
                }
            }
        }
        let f = if top <= xt {
            if !any_up {
                return Err(TransformError::Invalid(
                    "Fit to end cannot reach the target: no note moves later with this skew direction. Reverse the X skew or move the pivot.".into(),
                ));
            }
            reach
        } else {
            if lo > hi {
                return Err(TransformError::Invalid(
                    "Fit to end cannot reach the target: moving the last notes back to it would push others past it. Choose a later target.".into(),
                ));
            }
            lo
        };
        Ok(if f.is_finite() { f } else { 0.0 })
    }

    /// Every note's swept values: the settings', with the Control Sweeps
    /// applied at each note's place.
    fn vals(&self, src: &Source) -> (SVals, Vec<SVals>) {
        let ax_of = |amount: f64, ms: f64| -> f64 {
            let mut ax = amount / 100.0;
            if let Some(a) = self.angle {
                ax = (a * PI / 180.0).tan();
            }
            if self.rt_lowest.is_some() {
                ax = ms / 1000.0;
            }
            if self.fit.is_some() {
                ax = if ax < 0.0 { -1.0 } else { 1.0 };
            }
            ax
        };
        let base = SVals {
            ax: self.ax,
            rows: self.stair.map_or(1, |s| s.1),
            strength: self.stair.map_or(0.0, |s| s.2),
            phase: self.zig.map_or(0.0, |z| z.1),
            end: self.end.map_or(0.0, |e| e.0),
            over: self.elastic.map_or(0.0, |e| e.0),
            shift: self.stagger.map_or(0.0, |s| s.0),
            hi: self.depth.as_ref().map_or(1.0, |d| d.1),
            mvel: self.mirror.map_or(1.0, |m| m.0),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let f = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v, n, pl)
                    } else {
                        v
                    }
                };
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                SVals {
                    ax: if sw.drives("amount") || sw.drives("n06.ms") {
                        ax_of(f("amount", self.amount_pct), f("n06.ms", self.ms))
                    } else {
                        base.ax
                    },
                    rows: if sw.drives("n01.rows") {
                        sw.int("n01.rows", base.rows, n, pl).clamp(1, 24)
                    } else {
                        base.rows
                    },
                    strength: pct("n01.strength", base.strength).clamp(0.0, 1.0),
                    phase: pct("n02.phase", base.phase).clamp(0.0, 1.0),
                    end: pct("n03.amount", base.end),
                    over: pct("n09.overshoot", base.over),
                    shift: if sw.drives("n10.shift") {
                        sw.int("n10.shift", base.shift as i64, n, pl) as f64
                    } else {
                        base.shift
                    },
                    hi: pct("n07.hi", base.hi),
                    mvel: pct("n11.vel", base.mvel).clamp(0.0, 1.5),
                }
            })
            .collect();
        (base, per)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let (unit_of, units) = self.units(src);
        let (base, vals) = self.vals(src);
        let mut run = SkewRun {
            unit_of,
            units,
            stagger: self.stagger_table(src),
            base,
            vals,
            f: 1.0,
            dmax: 0.0,
        };
        run.f = self.fit_factor(src, &run)?;
        if self.depth.is_some() {
            for (i, n) in src.notes.iter().enumerate() {
                let fr = self.frame(&run, i, n, 1.0, run.f, Upto::All);
                let (u2, v2) = self.point(&fr, n, fr.us, fr.v, fr.v);
                run.dmax = run.dmax.max((u2 - fr.us).hypot(v2 - fr.v));
            }
        }
        let signs: &[f64] = if self.mirror.is_some() {
            &[1.0, -1.0]
        } else {
            &[1.0]
        };
        for (i, n) in src.notes.iter().enumerate() {
            for &sigma in signs {
                let fr = self.frame(&run, i, n, sigma, run.f, Upto::All);
                let mut tpl = *n;
                if sigma > 0.0 {
                    if let Some((lo, _, c)) = &self.depth {
                        let hi = run.vals.get(i).unwrap_or(&run.base).hi;
                        let (u2, v2) = self.point(&fr, n, fr.us, fr.v, fr.v);
                        let d = (u2 - fr.us).hypot(v2 - fr.v);
                        let x = if run.dmax > 0.0 { d / run.dmax } else { 0.0 };
                        let k = lo + (hi - lo) * c.eval(x.clamp(0.0, 1.0));
                        tpl.vel = (f64::from(n.vel) * k).round().clamp(1.0, 127.0) as u8;
                    }
                } else if let Some((_, sub)) = self.mirror {
                    let vel = run.vals.get(i).unwrap_or(&run.base).mvel;
                    tpl.vel = (f64::from(n.vel) * vel).round().clamp(1.0, 127.0) as u8;
                    if sub != 0
                        && let Some(t) = self.tracks.get(usize::from(n.track))
                    {
                        let (id, ch) = t.sub_offset(n.sub, sub);
                        tpl.sub = id;
                        tpl.ch = ch;
                    }
                }
                out.set_dim(0);
                let ok = match self.trail {
                    None => self.g.rasterize(
                        &tpl,
                        &|u, v| self.point(&fr, n, u, v, v),
                        &self.tracks,
                        out,
                    ),
                    Some(copies) => {
                        let mut ok = true;
                        for c in 0..=copies {
                            out.set_dim(c);
                            let w = f64::from(c) / f64::from(copies);
                            ok = self.g.rasterize(
                                &tpl,
                                &|u, v| {
                                    let (u2, v2) = self.point(&fr, n, u, v, v);
                                    (u + (u2 - u) * w, v + (v2 - v) * w)
                                },
                                &self.tracks,
                                out,
                            );
                            if !ok {
                                break;
                            }
                        }
                        ok
                    }
                };
                if !ok {
                    return out.check();
                }
            }
            out.check()?;
        }
        Ok(())
    }
}

// ==================================================================== Distort

pub const DISTORT: ToolDef = ToolDef {
    id: "distort",
    spec: distort_spec,
    build: distort_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const DISTORT_HELP: &[(&str, &str)] = &[
    (
        "amount",
        "Bulges the selection outward around the pivot (+) or pinches it inward (−); the effect fades toward the edges.",
    ),
    (
        "a1",
        "Moves the four corners of the selection's box freely; the inside follows.",
    ),
    (
        "a1.corners",
        "Each corner's offset in thousandths of the box, as x, y pairs: bottom left, bottom right, top right, top left.",
    ),
    (
        "a2",
        "Bends the top and bottom edges of the box through several points.",
    ),
    (
        "a2.top",
        "The top edge's vertical offsets from left to right, in thousandths of the box.",
    ),
    (
        "a2.bottom",
        "The bottom edge's vertical offsets from left to right, in thousandths of the box.",
    ),
    ("a3", "Pinches the selection toward the pivot."),
    ("a3.amount", "How strongly the selection is pinched."),
    (
        "a4",
        "Magnifies a round area around the pivot, like a lens.",
    ),
    ("a4.amount", "How strongly the lens magnifies."),
    ("a4.radius", "The lens's size, as a share of the box."),
    (
        "a5",
        "Bows the selection outward, more the further from the pivot (barrel distortion).",
    ),
    ("a5.k", "How strong the barrel bowing is."),
    (
        "a6",
        "Bows the selection inward, more the further from the pivot (pincushion distortion).",
    ),
    ("a6.k", "How strong the pincushion is."),
    (
        "a7",
        "Distorts only a band along the box's edges; the middle stays as it is.",
    ),
    (
        "a7.width",
        "How wide the edge band is, as a share of the box.",
    ),
    (
        "a8",
        "Keeps the area around the pivot still; the distortion grows away from it.",
    ),
    (
        "a9",
        "Narrows the top of the box (or the bottom with −), like a keystone perspective.",
    ),
    (
        "a9.amount",
        "How much narrower the top becomes (− narrows the bottom).",
    ),
    (
        "a10",
        "Makes notes in squeezed areas louder and notes in stretched areas softer.",
    ),
    (
        "n01",
        "Magnifies a band along time or pitch, compressing what lies outside it.",
    ),
    (
        "n01.axis",
        "Whether the lens band runs along time or along pitch.",
    ),
    (
        "n01.focus",
        "Where the band's center lies, as a share of the box.",
    ),
    (
        "n01.width",
        "How wide the magnified band is, as a share of the box.",
    ),
    (
        "n01.mag",
        "How much the band is magnified (200 % = twice as large).",
    ),
    (
        "n02",
        "Breaks the selection into blocks that each take one block's content (a mosaic).",
    ),
    ("n02.width", "The width of each block, as a note value."),
    ("n02.rows", "The height of each block, in keys."),
    (
        "n03",
        "Distorts only inside a region; the effect fades out at its edge.",
    ),
    (
        "n03.shape",
        "The region's shape: an ellipse or a rectangle.",
    ),
    (
        "n03.x",
        "The region's center in time, as a share of the box.",
    ),
    (
        "n03.y",
        "The region's center in pitch, as a share of the box.",
    ),
    ("n03.size", "The region's size, as a share of the box."),
    (
        "n03.feather",
        "How softly the effect fades out at the region's edge.",
    ),
    (
        "n04",
        "Makes the selection sag between supports, like a hanging rope.",
    ),
    ("n04.sag", "How many keys the middle sags (− rises)."),
    (
        "n04.supports",
        "Where the selection is held up: at its ends, at its center, or at listed points.",
    ),
    (
        "n04.points",
        "For Listed: the support points, in thousandths of the box's width.",
    ),
    (
        "n04.shape",
        "The sag's shape: a parabola or a catenary (a hanging chain).",
    ),
    (
        "n05",
        "Applies the distortion separately to each bar, group of beats or chord, instead of the whole box.",
    ),
    (
        "n05.tile",
        "What each tile is: a bar, a number of beats, or a chord.",
    ),
    ("n05.beats", "For Beats: how many beats one tile spans."),
    (
        "n05.tol",
        "For Chord: notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n06",
        "Pulls the notes toward a drawn line (or pushes them away).",
    ),
    ("n06.line", "The line's pitch across the box, as a curve."),
    (
        "n06.strength",
        "How strongly the notes are pulled (+) or pushed (−).",
    ),
    (
        "n06.falloff",
        "How far from the line the pull reaches, as a share of the box.",
    ),
    (
        "n07",
        "Tapers the selection: it narrows in pitch along time.",
    ),
    (
        "n07.start",
        "How much narrower the selection is at its start.",
    ),
    ("n07.end", "How much narrower the selection is at its end."),
    (
        "n07.shape",
        "How the taper moves from the start value to the end value.",
    ),
    (
        "n08",
        "Shatters the selection into shards flying apart from the center.",
    ),
    ("n08.shards", "How many shards."),
    (
        "n08.explosion",
        "How far the shards fly apart, as a share of the box.",
    ),
    (
        "n08.sub",
        "Moves each shard this many subchannels further, so shards can be colored apart.",
    ),
    ("n09", "Bends the selection along an arc."),
    ("n09.angle", "How far the arc bends (± degrees)."),
    (
        "n09.keep",
        "Keep the notes' lengths, or keep the shape's proportions.",
    ),
    (
        "n10",
        "Sorts the notes in each row by a property, like pixel sorting.",
    ),
    (
        "n10.by",
        "The property sorted by: velocity, length or subchannel.",
    ),
    ("n10.dir", "Ascending or descending."),
    (
        "n10.within",
        "Sort each whole row, or only runs of notes closer together than the gap.",
    ),
    (
        "n10.gap",
        "For Runs: notes farther apart than this, in ticks, start a new run.",
    ),
    (
        "n11",
        "Splits the box into four quadrants and rearranges them.",
    ),
    (
        "n11.x",
        "Where the box is split in time, as a share of its width.",
    ),
    (
        "n11.y",
        "Where the box is split in pitch, as a share of its height.",
    ),
    (
        "n11.mode",
        "Swap diagonally, rotate the quadrants clockwise, or mirror them in pairs.",
    ),
];

fn distort_spec() -> ToolSpec {
    let mut s = spec(
        "distort",
        "Distort",
        "Bends the selection's box: a radial bulge (positive) or pinch (negative) around the pivot; advanced modes move corners, edges and lenses.",
        vec![
            float("amount", "Bulge (+) / pinch (−)", -100.0, 100.0, 40.0, "%"),
            adv_switch(1, "Four-corner free distort"),
            list(
                "a1.corners",
                "Corner offsets ‰ (BL x,y, BR x,y, TR x,y, TL x,y)",
                -1000,
                1000,
                8,
                &[0, 0, 0, 0, 200, 0, -200, 0],
            )
            .adv(1),
            adv_switch(2, "Multi-point boundary"),
            list(
                "a2.top",
                "Top edge offsets ‰ (left → right)",
                -1000,
                1000,
                64,
                &[0, 150, 0],
            )
            .adv(2),
            list(
                "a2.bottom",
                "Bottom edge offsets ‰",
                -1000,
                1000,
                64,
                &[0, -150, 0],
            )
            .adv(2),
            adv_switch(3, "Pinch"),
            float("a3.amount", "Pinch", 0.0, 100.0, 40.0, "%").adv(3),
            adv_switch(4, "Bulge (lens)"),
            float("a4.amount", "Bulge", 0.0, 100.0, 40.0, "%").adv(4),
            float("a4.radius", "Lens radius", 5.0, 100.0, 50.0, "%").adv(4),
            adv_switch(5, "Barrel"),
            float("a5.k", "Barrel strength", 0.0, 200.0, 50.0, "%").adv(5),
            adv_switch(6, "Pincushion"),
            float("a6.k", "Pincushion strength", 0.0, 200.0, 50.0, "%").adv(6),
            adv_switch(7, "Edge-only"),
            float("a7.width", "Edge band", 1.0, 50.0, 20.0, "%").adv(7),
            adv_switch(8, "Center-lock"),
            adv_switch(9, "Perspective blend"),
            float(
                "a9.amount",
                "Keystone (top narrower)",
                -90.0,
                90.0,
                40.0,
                "%",
            )
            .adv(9),
            adv_switch(10, "Density-compensated velocity"),
            // V09-S37: functions declared by stable id (TOOL_CATALOG n01 … n11).
            flag("n01", "Axis lens", false),
            choice("n01.axis", "Axis", LENS_AXES, 0),
            float("n01.focus", "Focus", 0.0, 100.0, 50.0, "%"),
            float("n01.width", "Width", 1.0, 100.0, 30.0, "%"),
            float("n01.mag", "Magnification", 25.0, 800.0, 200.0, "%"),
            flag("n02", "Block mosaic", false),
            choice("n02.width", "Block width", art_grid_values(), 2),
            int("n02.rows", "Block height", 1, 48, 6),
            flag("n03", "Region mask", false),
            choice("n03.shape", "Region", REGIONS, 0),
            float("n03.x", "Center X", 0.0, 100.0, 50.0, "%"),
            float("n03.y", "Center Y", 0.0, 100.0, 50.0, "%"),
            float("n03.size", "Size", 1.0, 200.0, 60.0, "%"),
            float("n03.feather", "Feather", 0.0, 100.0, 30.0, "%"),
            flag("n04", "Gravity sag", false),
            int("n04.sag", "Sag", -48, 48, 6),
            choice("n04.supports", "Supports", SAG_SUPPORTS, 0),
            list(
                "n04.points",
                "Support points (‰ of the box width)",
                0,
                1000,
                64,
                &[0, 500, 1000],
            ),
            choice("n04.shape", "Shape", SAG_SHAPES, 0),
            flag("n05", "Tiled distortion", false),
            choice("n05.tile", "Tile", TILES, 0),
            int("n05.beats", "Beats", 1, 64, 4),
            ticks("n05.tol", "Chord tolerance", 0, 1 << 40, 30),
            flag("n06", "Line attractor", false),
            curve("n06.line", "Line", Curve::line(0.2, 0.8)),
            float("n06.strength", "Strength", -100.0, 100.0, 50.0, "%"),
            float("n06.falloff", "Falloff", 1.0, 100.0, 25.0, "%"),
            flag("n07", "Taper", false),
            float("n07.start", "Taper at start", 0.0, 100.0, 0.0, "%"),
            float("n07.end", "Taper at end", 0.0, 100.0, 80.0, "%"),
            curve("n07.shape", "Taper shape", Curve::line(0.0, 1.0)),
            flag("n08", "Shatter", false),
            int("n08.shards", "Shards", 2, 256, 12),
            float("n08.explosion", "Explosion", 0.0, 200.0, 30.0, "%"),
            int("n08.sub", "Subchannel step per shard", -64, 64, 0),
            flag("n09", "Arc bend", false),
            float("n09.angle", "Bend angle", -180.0, 180.0, 90.0, "°"),
            choice("n09.keep", "Keep", ARC_KEEP, 0),
            flag("n10", "Pixel sort", false),
            choice("n10.by", "Sort by", SORT_BY, 0),
            choice("n10.dir", "Direction", SORT_DIR, 0),
            choice("n10.within", "Within", SORT_WITHIN, 0),
            ticks("n10.gap", "Gap", 0, 1 << 40, 240),
            flag("n11", "Quadrant shuffle", false),
            float("n11.x", "Split X", 1.0, 99.0, 50.0, "%"),
            float("n11.y", "Split Y", 1.0, 99.0, 50.0, "%"),
            choice("n11.mode", "Mode", QUAD_MODES, 0),
        ],
        [
            "Four-corner free",
            "Multi-point boundary",
            "Pinch",
            "Bulge",
            "Barrel",
            "Pincushion",
            "Edge-only",
            "Center-lock",
            "Perspective blend",
            "Density-compensated",
        ],
    );
    s.functions = [
        ("n01", "Axis lens", 120),
        ("n02", "Block mosaic", 170),
        ("n03", "Region mask", 200),
        ("n04", "Gravity sag", 130),
        ("n05", "Tiled distortion", 110),
        ("n06", "Line attractor", 140),
        ("n07", "Taper", 150),
        ("n08", "Shatter", 190),
        ("n09", "Arc bend", 160),
        ("n10", "Pixel sort", 510),
        ("n11", "Quadrant shuffle", 180),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, DISTORT_HELP);
    s
}

fn distort_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S38 gate (E0409): Control Sweeps run inside the v0.9 kernel, per
    // note (`definition::kernel_sweeps`).
    if DISTORT_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return distort9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let amount = p.float("amount") / 100.0;
    let corners = p.adv(1).then(|| {
        let v = permille(&p.list("a1.corners"));
        let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
        [(c(0), c(1)), (c(2), c(3)), (c(4), c(5)), (c(6), c(7))]
    });
    let edges = p
        .adv(2)
        .then(|| (permille(&p.list("a2.top")), permille(&p.list("a2.bottom"))));
    let pinch = p.adv(3).then(|| p.float("a3.amount") / 100.0);
    let lens = p
        .adv(4)
        .then(|| (p.float("a4.amount") / 100.0, p.float("a4.radius") / 100.0));
    let barrel = p.adv(5).then(|| p.float("a5.k") / 100.0);
    let cushion = p.adv(6).then(|| p.float("a6.k") / 100.0);
    let edge_only = p.adv(7).then(|| p.float("a7.width") / 100.0);
    let centre_lock = p.adv(8);
    let keystone = p.adv(9).then(|| p.float("a9.amount") / 100.0);
    let density = p.adv(10);
    let (cu, cv) = g.pivot;
    let map = move |u: f64, v: f64| -> (f64, f64) {
        let (dx, dy) = (u - cu, v - cv);
        let r = (dx * dx + dy * dy).sqrt();
        let mut d = (0.0, 0.0);
        // Base radial bulge / pinch inside the unit circle around the pivot.
        let f = amount * (1.0 - (r / 0.75).min(1.0)).powi(2);
        d.0 += dx * f;
        d.1 += dy * f;
        if let Some(c) = corners {
            // Bilinear interpolation of the corner offsets.
            let (bl, br, tr, tl) = (c[0], c[1], c[2], c[3]);
            let bx = bl.0 + (br.0 - bl.0) * u;
            let by = bl.1 + (br.1 - bl.1) * u;
            let tx = tl.0 + (tr.0 - tl.0) * u;
            let ty = tl.1 + (tr.1 - tl.1) * u;
            d.0 += bx + (tx - bx) * v;
            d.1 += by + (ty - by) * v;
        }
        if let Some((top, bottom)) = &edges {
            let t = catmull(top, u);
            let bo = catmull(bottom, u);
            d.1 += bo + (t - bo) * v;
        }
        if let Some(k) = pinch {
            let s = -k * (1.0 - r.min(1.0));
            d.0 += dx * s;
            d.1 += dy * s;
        }
        if let Some((k, rad)) = lens
            && r < rad
        {
            let s = k * (1.0 - r / rad);
            d.0 += dx * s;
            d.1 += dy * s;
        }
        if let Some(k) = barrel {
            d.0 += dx * k * r * r;
            d.1 += dy * k * r * r;
        }
        if let Some(k) = cushion {
            d.0 -= dx * k * r * r;
            d.1 -= dy * k * r * r;
        }
        if let Some(w) = edge_only {
            let e = u.min(1.0 - u).min(v).min(1.0 - v).max(0.0);
            let s = 1.0 - smoothstep(e / w.max(1e-6));
            d.0 *= s;
            d.1 *= s;
        }
        if centre_lock {
            let s = smoothstep(r / 0.5);
            d.0 *= s;
            d.1 *= s;
        }
        let (mut u2, v2) = (u + d.0, v + d.1);
        if let Some(k) = keystone {
            u2 = cu + (u2 - cu) * (1.0 - k * v2.clamp(0.0, 1.0));
        }
        (u2, v2)
    };
    let tracks = b.tracks.to_vec();
    let per = g.per_note();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                let mut t = *n;
                if density {
                    // Local area change (Jacobian) of the map at the note's
                    // centre: compressed areas get louder, stretched softer.
                    let (u, v) = g.bbox.to_unit(
                        (n.start as f64 + n.end() as f64) / 2.0,
                        f64::from(n.pitch) + 0.5,
                    );
                    let e = 1e-3;
                    let (a, b2) = map(u + e, v);
                    let (c, d) = map(u - e, v);
                    let (e1, f1) = map(u, v + e);
                    let (g1, h1) = map(u, v - e);
                    let jac = ((a - c) * (f1 - h1) - (b2 - d) * (e1 - g1)) / (4.0 * e * e);
                    let k = (1.0 / jac.abs().max(0.05)).sqrt().clamp(0.25, 4.0);
                    t.vel = (f64::from(n.vel) * k).round().clamp(1.0, 127.0) as u8;
                }
                if !g.rasterize(&t, &|u, v| map(u, v), &tracks, out) {
                    break;
                }
                out.check()?;
            }
            Ok(())
        },
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

// --------------------------------------------------------- Distort (v0.9)

const DISTORT_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const LENS_AXES: &[&str] = &["Time", "Pitch"];
const REGIONS: &[&str] = &["Ellipse", "Rectangle"];
const SAG_SUPPORTS: &[&str] = &["Ends", "Center", "Listed"];
const SAG_SHAPES: &[&str] = &["Parabola", "Catenary"];
const TILES: &[&str] = &["Bar", "Beats", "Chord"];
const ARC_KEEP: &[&str] = &["Lengths", "Aspect"];
const SORT_BY: &[&str] = &["Velocity", "Length", "Subchannel"];
const SORT_DIR: &[&str] = &["Ascending", "Descending"];
const SORT_WITHIN: &[&str] = &["Whole row", "Runs closer than the gap"];
const QUAD_MODES: &[&str] = &["Swap diagonally", "Rotate clockwise", "Mirror pairs"];
/// The catenary's sharpness (cosh(κ x), normalised).
const CATENARY_K: f64 = 2.0;
/// The farthest a v0.9 Distort image may lie from the box (box units):
/// with the widest box (2⁶⁴ ticks) and Scale X 400 % still inside the
/// raster's i128 ticks.
const DISTORT_REACH: f64 = 1e12;

/// The v0.9 Distort (V09-S37 step 2): the v0.8 map re-read from the same
/// parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions, in their catalog order:
///
/// * 110 n05 Tiled distortion — each bar / N beats / chord is its own box;
/// * 120 n01 Axis lens — a one-axis fisheye after the v0.8 map;
/// * 130 n04 Gravity sag — rows sag between supports;
/// * 140 n06 Line attractor — rows pulled toward a curve;
/// * 150 n07 Taper — the pitch extent squeezed along time;
/// * 160 n09 Arc bend — the box bent along a circular arc;
/// * 170 n02 Block mosaic — each note moved by its block centre's motion;
/// * 180 n11 Quadrant shuffle — whole notes moved between quadrants;
/// * 190 n08 Shatter — notes moved with their seeded shard;
/// * 200 n03 Region mask — all of it only inside a feathered region;
/// * 510 n10 Pixel sort — attributes sorted along each row.
#[derive(Clone)]
struct Distort9 {
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    amount: f64,
    corners: Option<[(f64, f64); 4]>,
    edges: Option<(Vec<f64>, Vec<f64>)>,
    pinch: Option<f64>,
    lens: Option<(f64, f64)>,
    barrel: Option<f64>,
    cushion: Option<f64>,
    edge_only: Option<f64>,
    centre_lock: bool,
    keystone: Option<f64>,
    density: bool,
    // ----- v0.9 functions
    /// n01: pitch axis, band [a, b] → [a2, b2].
    axis_lens: Option<(bool, f64, f64, f64, f64)>,
    /// n02: block width (ticks), rows.
    mosaic: Option<(u64, i64)>,
    /// n03: rectangle, centre, half size, feather (share of it).
    region: Option<(bool, f64, f64, f64, f64)>,
    /// n04: sag (rows), supports (0 … 1, ascending), catenary.
    sag: Option<(f64, Vec<f64>, bool)>,
    /// n05: tile, beats, chord tolerance.
    tile: Option<(usize, u64, u64)>,
    /// n06: line, strength, falloff.
    attract: Option<(Curve, f64, f64)>,
    /// n07: taper at the start and end, shape.
    taper: Option<(f64, f64, Curve)>,
    /// n08: shard seeds (unit box), explosion, subchannel step per shard.
    shatter: Option<Shatter>,
    /// n09: signed angle (radians), radius of the neutral line.
    arc: Option<(f64, f64)>,
    /// n10: sort by, descending, run gap.
    sort: Option<(usize, bool, Option<u64>)>,
    /// n11: split point, mode.
    quad: Option<(f64, f64, usize)>,
    /// n01's settings (axis, focus %, width %, magnification %) and n09's
    /// "keep the length", for the per-note values with sweeps.
    lens_in: Option<(bool, f64, f64, f64)>,
    arc_keep: bool,
}

/// n01: the band [a, b] (unit coordinates) and its image [a2, b2] for
/// focus, width and magnification in %.
fn lens_band(axis: bool, focus: f64, width: f64, mag: f64) -> (bool, f64, f64, f64, f64) {
    let f = (focus / 100.0).clamp(0.0, 1.0);
    let w = (width / 100.0).clamp(0.01, 1.0);
    let m = (mag / 100.0).clamp(0.25, 8.0);
    let (a, bb) = ((f - w / 2.0).max(0.0), (f + w / 2.0).min(1.0));
    let (a2, b2) = if m == 1.0 {
        (a, bb)
    } else {
        ((f - m * (f - a)).max(0.0), (f + m * (bb - f)).min(1.0))
    };
    (axis, a, bb, a2, b2)
}

/// n09: the signed angle (radians) and the neutral line's radius for an
/// angle in degrees (`keep`: the chord keeps the width).
fn arc_of(degrees: f64, keep: bool) -> (f64, f64) {
    let th = degrees.clamp(-180.0, 180.0) * PI / 180.0;
    let r = if th == 0.0 {
        0.0
    } else if keep {
        1.0 / (2.0 * (th.abs() / 2.0).sin())
    } else {
        1.0 / th.abs()
    };
    (th, r)
}

/// One note's values of the parameters Distort's Control Sweeps drive
/// (V09-S38 gate, E0409: per note inside the kernel, so n05's chords and
/// n10's sort see the whole selection).
#[derive(Clone, Copy, Debug)]
struct DVals {
    amount: f64,
    lens: Option<(bool, f64, f64, f64, f64)>,
    half: f64,
    sag: f64,
    strength: f64,
    te: f64,
    explosion: f64,
    arc: Option<(f64, f64)>,
    qx: f64,
}

/// n08's settings: shard seeds (unit box), explosion, subchannel step per
/// shard.
type Shatter = (Vec<(f64, f64)>, f64, i64);

/// One note's settings for [`Distort9::point`].
#[derive(Clone, Copy, Debug)]
struct DFrame {
    /// The box the distortion works in (absolute): start, width, lowest
    /// row, rows (the selection's unless n05 tiles).
    t0: f64,
    w: f64,
    p0: f64,
    rows: f64,
    tiled: bool,
    /// n02: the note's motion (frame units).
    mosaic: Option<(f64, f64)>,
    /// n11: source and target quadrant.
    quad: Option<(usize, usize)>,
    /// n08: the shard's motion (frame units).
    shard: (f64, f64),
    /// n08: the shard's number (V09-0143: the driver dimension).
    shard_ix: u32,
    /// The note's swept values.
    d: DVals,
}

fn distort9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let ctx = b.ctx.clone();
    let lens_in = p.flag("n01").then(|| {
        (
            p.choice("n01.axis") == 1,
            p.float("n01.focus"),
            p.float("n01.width"),
            p.float("n01.mag"),
        )
    });
    let lens_band = lens_in.map(|(axis, f, w, m)| lens_band(axis, f, w, m));
    let sag = p.flag("n04").then(|| {
        let supports = match p.choice("n04.supports") {
            0 => vec![0.0, 1.0],
            1 => vec![0.5],
            _ => {
                let mut v: Vec<f64> = permille(&p.list("n04.points"))
                    .into_iter()
                    .map(|x| x.clamp(0.0, 1.0))
                    .collect();
                v.sort_by(f64::total_cmp);
                v.dedup();
                if v.is_empty() {
                    v.push(0.5);
                }
                v
            }
        };
        (
            p.int("n04.sag") as f64,
            supports,
            p.choice("n04.shape") == 1,
        )
    });
    let shatter = p.flag("n08").then(|| {
        let n = p.int("n08.shards").clamp(2, 256) as u64;
        let salt = salt_of("distort.n08");
        // Sorted by x for the nearest-seed scan; a shard's number (its
        // subchannel step) is its place in this order.
        let mut seeds: Vec<(f64, f64)> = (0..n)
            .map(|k| {
                let mut r = stream(0, salt, &[k]);
                (r.unit(), r.unit())
            })
            .collect();
        seeds.sort_by(|a, b| a.0.total_cmp(&b.0));
        (
            seeds,
            (p.float("n08.explosion") / 100.0).clamp(0.0, 2.0),
            p.int("n08.sub").clamp(-64, 64),
        )
    });
    let arc_keep = p.choice("n09.keep") == 1;
    let arc = p
        .flag("n09")
        .then(|| arc_of(p.float("n09.angle"), arc_keep));
    let k = Distort9 {
        amount: p.float("amount") / 100.0,
        lens_in,
        arc_keep,
        corners: p.adv(1).then(|| {
            let v = permille(&p.list("a1.corners"));
            let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
            [(c(0), c(1)), (c(2), c(3)), (c(4), c(5)), (c(6), c(7))]
        }),
        edges: p
            .adv(2)
            .then(|| (permille(&p.list("a2.top")), permille(&p.list("a2.bottom")))),
        pinch: p.adv(3).then(|| p.float("a3.amount") / 100.0),
        lens: p
            .adv(4)
            .then(|| (p.float("a4.amount") / 100.0, p.float("a4.radius") / 100.0)),
        barrel: p.adv(5).then(|| p.float("a5.k") / 100.0),
        cushion: p.adv(6).then(|| p.float("a6.k") / 100.0),
        edge_only: p.adv(7).then(|| p.float("a7.width") / 100.0),
        centre_lock: p.adv(8),
        keystone: p.adv(9).then(|| p.float("a9.amount") / 100.0),
        density: p.adv(10),
        axis_lens: lens_band,
        mosaic: p.flag("n02").then(|| {
            (
                note_value(p.choice("n02.width").min(10), 0, &ctx),
                p.int("n02.rows").clamp(1, 48),
            )
        }),
        region: p.flag("n03").then(|| {
            (
                p.choice("n03.shape") == 1,
                p.float("n03.x") / 100.0,
                p.float("n03.y") / 100.0,
                (p.float("n03.size") / 200.0).max(1e-6),
                (p.float("n03.feather") / 100.0).clamp(0.0, 1.0),
            )
        }),
        sag,
        tile: p.flag("n05").then(|| {
            (
                p.choice("n05.tile").min(2),
                p.int("n05.beats").clamp(1, 64) as u64,
                p.ticks("n05.tol"),
            )
        }),
        attract: p.flag("n06").then(|| {
            (
                p.curve("n06.line"),
                (p.float("n06.strength") / 100.0).clamp(-1.0, 1.0),
                (p.float("n06.falloff") / 100.0).max(0.01),
            )
        }),
        taper: p.flag("n07").then(|| {
            (
                (p.float("n07.start") / 100.0).clamp(0.0, 1.0),
                (p.float("n07.end") / 100.0).clamp(0.0, 1.0),
                p.curve("n07.shape"),
            )
        }),
        shatter,
        arc,
        sort: p.flag("n10").then(|| {
            (
                p.choice("n10.by").min(2),
                p.choice("n10.dir") == 1,
                (p.choice("n10.within") == 1).then(|| p.ticks("n10.gap")),
            )
        }),
        quad: p.flag("n11").then(|| {
            (
                (p.float("n11.x") / 100.0).clamp(0.01, 0.99),
                (p.float("n11.y") / 100.0).clamp(0.01, 0.99),
                p.choice("n11.mode").min(2),
            )
        }),
        g,
        ctx,
        tracks: b.tracks.to_vec(),
    };
    let per = k.g.per_note();
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

/// n08: the seed nearest to `p` (ties: the lowest index). `seeds` are
/// sorted by x (the build sorts them, keeping the drawing order as the
/// index tie-break through a stable sort); the scan walks outward from
/// `p`'s x and stops once the x distance alone exceeds the best.
fn nearest_seed(seeds: &[(f64, f64)], p: (f64, f64)) -> usize {
    let start = seeds.partition_point(|s| s.0 < p.0);
    let mut best = (f64::INFINITY, usize::MAX);
    let consider = |j: usize, best: &mut (f64, usize)| {
        let s = seeds[j];
        let d = (s.0 - p.0).powi(2) + (s.1 - p.1).powi(2);
        if d < best.0 || (d == best.0 && j < best.1) {
            *best = (d, j);
        }
    };
    let mut r = start;
    while r < seeds.len() && (seeds[r].0 - p.0).powi(2) <= best.0 {
        consider(r, &mut best);
        r += 1;
    }
    let mut l = start;
    while l > 0 && (seeds[l - 1].0 - p.0).powi(2) <= best.0 {
        l -= 1;
        consider(l, &mut best);
    }
    best.1.min(seeds.len().saturating_sub(1))
}

/// The shape between supports `t` ∈ 0 … 1 (1 at the middle) or of an
/// overhang `t` ∈ 0 … 1 from its support (1 at the end).
fn sag_shape(t: f64, span: bool, catenary: bool) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match (span, catenary) {
        (true, false) => 4.0 * t * (1.0 - t),
        (true, true) => {
            let c = CATENARY_K.cosh();
            (c - (CATENARY_K * (2.0 * t - 1.0)).cosh()) / (c - 1.0)
        }
        (false, false) => t * t,
        (false, true) => ((CATENARY_K * t).cosh() - 1.0) / (CATENARY_K.cosh() - 1.0),
    }
}

impl Distort9 {
    /// The v0.8 map (`distort_build`'s, unchanged) in frame units.
    #[inline(always)]
    fn v08(&self, dv: &DVals, u: f64, v: f64) -> (f64, f64) {
        let (cu, cv) = self.g.pivot;
        let (dx, dy) = (u - cu, v - cv);
        let r = (dx * dx + dy * dy).sqrt();
        let mut d = (0.0, 0.0);
        let f = dv.amount * (1.0 - (r / 0.75).min(1.0)).powi(2);
        d.0 += dx * f;
        d.1 += dy * f;
        if let Some(c) = self.corners {
            let (bl, br, tr, tl) = (c[0], c[1], c[2], c[3]);
            let bx = bl.0 + (br.0 - bl.0) * u;
            let by = bl.1 + (br.1 - bl.1) * u;
            let tx = tl.0 + (tr.0 - tl.0) * u;
            let ty = tl.1 + (tr.1 - tl.1) * u;
            d.0 += bx + (tx - bx) * v;
            d.1 += by + (ty - by) * v;
        }
        if let Some((top, bottom)) = &self.edges {
            let t = catmull(top, u);
            let bo = catmull(bottom, u);
            d.1 += bo + (t - bo) * v;
        }
        if let Some(k) = self.pinch {
            let s = -k * (1.0 - r.min(1.0));
            d.0 += dx * s;
            d.1 += dy * s;
        }
        if let Some((k, rad)) = self.lens
            && r < rad
        {
            let s = k * (1.0 - r / rad);
            d.0 += dx * s;
            d.1 += dy * s;
        }
        if let Some(k) = self.barrel {
            d.0 += dx * k * r * r;
            d.1 += dy * k * r * r;
        }
        if let Some(k) = self.cushion {
            d.0 -= dx * k * r * r;
            d.1 -= dy * k * r * r;
        }
        if let Some(w) = self.edge_only {
            let e = u.min(1.0 - u).min(v).min(1.0 - v).max(0.0);
            let s = 1.0 - smoothstep(e / w.max(1e-6));
            d.0 *= s;
            d.1 *= s;
        }
        if self.centre_lock {
            let s = smoothstep(r / 0.5);
            d.0 *= s;
            d.1 *= s;
        }
        let (mut u2, v2) = (u + d.0, v + d.1);
        if let Some(k) = self.keystone {
            u2 = cu + (u2 - cu) * (1.0 - k * v2.clamp(0.0, 1.0));
        }
        (u2, v2)
    }

    /// The continuous part (v0.8, n01, n04, n06, n07, n09) in frame units;
    /// `rows` the frame's rows (n04's semitones).
    #[inline(always)]
    fn chain(&self, dv: &DVals, u: f64, v: f64, rows: f64) -> (f64, f64) {
        let (mut u, mut v) = self.v08(dv, u, v);
        if let Some((pitch, a, b, a2, b2)) = dv.lens
            && (a2 != a || b2 != b)
        {
            let x = if pitch { v } else { u };
            let y = if !(0.0..=1.0).contains(&x) {
                x
            } else if x < a {
                if a > 0.0 { x * a2 / a } else { a2 }
            } else if x <= b {
                if b > a {
                    a2 + (x - a) * (b2 - a2) / (b - a)
                } else {
                    a2
                }
            } else if b < 1.0 {
                b2 + (x - b) * (1.0 - b2) / (1.0 - b)
            } else {
                b2
            };
            if pitch {
                v = y;
            } else {
                u = y;
            }
        }
        if let Some((_, supports, catenary)) = &self.sag
            && dv.sag != 0.0
        {
            let s = dv.sag;
            let first = supports[0];
            let last = supports[supports.len() - 1];
            let shape = if u < first {
                sag_shape((first - u) / first.max(1e-9), false, *catenary)
            } else if u > last {
                sag_shape((u - last) / (1.0 - last).max(1e-9), false, *catenary)
            } else {
                let j = supports
                    .partition_point(|&x| x <= u)
                    .clamp(1, supports.len().max(2) - 1);
                let (l, r) = (supports[j - 1], supports[j.min(supports.len() - 1)]);
                if r > l {
                    sag_shape((u - l) / (r - l), true, *catenary)
                } else {
                    0.0
                }
            };
            v -= s * shape / rows;
        }
        if let Some((line, _, falloff)) = &self.attract
            && dv.strength != 0.0
        {
            let s = dv.strength;
            let y = line.eval(u.clamp(0.0, 1.0));
            let d = (v - y) / falloff;
            v += s * (-d * d).exp() * (y - v);
        }
        if let Some((ts, _, c)) = &self.taper
            && (*ts != 0.0 || dv.te != 0.0)
        {
            let te = dv.te;
            let t = (ts + (te - ts) * c.eval(u.clamp(0.0, 1.0))).clamp(0.0, 1.0);
            let pv = self.g.pivot.1;
            v = pv + (v - pv) * (1.0 - t);
        }
        if let Some((th, rad)) = dv.arc
            && th != 0.0
        {
            let s = th.signum();
            let pv = self.g.pivot.1;
            let phi = th.abs() * (u - 0.5);
            let rho = rad + s * (v - pv);
            u = 0.5 + rho * phi.sin();
            v = pv - s * rad + s * rho * phi.cos();
        }
        (u, v)
    }

    /// n11: quadrant `k` (0 bottom-left, 1 bottom-right, 2 top-left, 3
    /// top-right) of the split box.
    fn quadrant(&self, k: usize, sx: f64) -> (f64, f64, f64, f64) {
        let (_, sy, _) = self.quad.unwrap_or((0.5, 0.5, 0));
        let (x0, x1) = if k & 1 == 0 { (0.0, sx) } else { (sx, 1.0) };
        let (y0, y1) = if k & 2 == 0 { (0.0, sy) } else { (sy, 1.0) };
        (x0, x1, y0, y1)
    }

    /// The full image of point (u, v) (selection units) for a note's frame.
    #[inline(always)]
    fn point(&self, fr: &DFrame, u: f64, v: f64) -> (f64, f64) {
        let b = &self.g.bbox;
        let (fu, fv) = if fr.tiled {
            let x = b.t0 as f64 + u * b.width();
            let y = f64::from(b.p0) + v * b.rows();
            ((x - fr.t0) / fr.w, (y - fr.p0) / fr.rows)
        } else {
            (u, v)
        };
        let (mut qu, mut qv) = match fr.mosaic {
            Some((du, dv)) => (fu + du, fv + dv),
            None => self.chain(&fr.d, fu, fv, fr.rows),
        };
        if let Some((from, to)) = fr.quad {
            let (sx0, sx1, sy0, sy1) = self.quadrant(from, fr.d.qx);
            let (tx0, tx1, ty0, ty1) = self.quadrant(to, fr.d.qx);
            let mirror = self.quad.is_some_and(|q| q.2 == 2);
            qu = if mirror {
                tx0 + (sx1 - qu) * (tx1 - tx0) / (sx1 - sx0)
            } else {
                tx0 + (qu - sx0) * (tx1 - tx0) / (sx1 - sx0)
            };
            qv = ty0 + (qv - sy0) * (ty1 - ty0) / (sy1 - sy0);
        }
        qu += fr.shard.0;
        qv += fr.shard.1;
        let (u2, v2) = if fr.tiled {
            let x = fr.t0 + qu * fr.w;
            let y = fr.p0 + qv * fr.rows;
            b.to_unit(x, y)
        } else {
            (qu, qv)
        };
        // Far beyond the box (a tile's frame around a note spanning the
        // whole tick range): keep the image finite and within what the
        // raster's wide integers hold.
        let bound = |x: f64, or: f64| {
            if x.is_nan() {
                or
            } else {
                x.clamp(-DISTORT_REACH, DISTORT_REACH)
            }
        };
        let (u2, v2) = (bound(u2, u), bound(v2, v));
        if let Some((rect, cx, cy, _, feather)) = self.region {
            let half = fr.d.half;
            let (dx, dy) = ((u - cx) / half, (v - cy) / half);
            let rho = if rect {
                dx.abs().max(dy.abs())
            } else {
                (dx * dx + dy * dy).sqrt()
            };
            let inner = 1.0 - feather;
            let m = if rho <= inner {
                1.0
            } else if rho >= 1.0 {
                0.0
            } else {
                1.0 - smoothstep((rho - inner) / feather.max(1e-9))
            };
            if m < 1.0 {
                return (u + m * (u2 - u), v + m * (v2 - v));
            }
        }
        (u2, v2)
    }

    /// n05's tiles: per note its box (absolute start, width, lowest row,
    /// rows); chords per track.
    fn tiles(&self, src: &Source) -> Vec<(f64, f64, f64, f64)> {
        let Some((kind, beats, tol)) = self.tile else {
            return Vec::new();
        };
        let b = &self.g.bbox;
        let (p0, rows) = (f64::from(b.p0), b.rows());
        let notes = &src.notes;
        let mut out = vec![(b.t0 as f64, b.width(), p0, rows); notes.len()];
        if kind < 2 {
            for (i, n) in notes.iter().enumerate() {
                let (a, len) = if kind == 0 {
                    bar_bounds(&self.ctx.meter, n.start)
                } else {
                    let m = metric_position(&self.ctx.meter, n.start);
                    let len = m.beat_len.max(1).saturating_mul(beats);
                    let k = (n.start - m.bar_start) / len;
                    (m.bar_start + k * len, len)
                };
                out[i] = (a as f64, len.max(1) as f64, p0, rows);
            }
            return out;
        }
        for idx in per_track(src) {
            if idx.is_empty() {
                continue;
            }
            for c in ChordGroups::of(notes, &idx, tol).iter() {
                let (mut t0, mut t1, mut lo, mut hi) = (u64::MAX, 0u64, u8::MAX, 0u8);
                for &i in c {
                    let n = &notes[i];
                    t0 = t0.min(n.start);
                    t1 = t1.max(n.end());
                    lo = lo.min(n.pitch);
                    hi = hi.max(n.pitch);
                }
                let bx = (
                    t0 as f64,
                    t1.saturating_sub(t0).max(1) as f64,
                    f64::from(lo),
                    f64::from(hi) - f64::from(lo) + 1.0,
                );
                for &i in c {
                    out[i] = bx;
                }
            }
        }
        out
    }

    /// Note `i`'s frame and template (n08's subchannel step).
    fn frame(
        &self,
        tiles: &[(f64, f64, f64, f64)],
        d: DVals,
        i: usize,
        n: &TNote,
    ) -> (DFrame, TNote) {
        let b = &self.g.bbox;
        let (t0, w, p0, rows) =
            tiles
                .get(i)
                .copied()
                .unwrap_or((b.t0 as f64, b.width(), f64::from(b.p0), b.rows()));
        let mut fr = DFrame {
            t0,
            w,
            p0,
            rows,
            tiled: self.tile.is_some(),
            mosaic: None,
            quad: None,
            shard: (0.0, 0.0),
            shard_ix: 0,
            d,
        };
        let mut tpl = *n;
        let to_frame = |x: f64, y: f64| ((x - t0) / w, (y - p0) / rows);
        if let Some((bw, rows_per)) = self.mosaic {
            let (bs, _) = bar_bounds(&self.ctx.meter, n.start);
            let cell_t = bs + (n.start - bs) / bw.max(1) * bw.max(1);
            let r = i64::from(n.pitch) - i64::from(b.p0);
            let cell_p = i64::from(b.p0) + r.div_euclid(rows_per) * rows_per;
            let (cu, cv) = to_frame(
                cell_t as f64 + bw as f64 / 2.0,
                cell_p as f64 + rows_per as f64 / 2.0,
            );
            let (mu, mv) = self.chain(&d, cu, cv, rows);
            fr.mosaic = Some((mu - cu, mv - cv));
        }
        let centre = to_frame(
            (n.start as f64 + n.end() as f64) / 2.0,
            f64::from(n.pitch) + 0.5,
        );
        if let Some((_, sy, mode)) = self.quad {
            let sx = d.qx;
            let k = usize::from(centre.0 >= sx) | usize::from(centre.1 >= sy) << 1;
            let to = match mode {
                0 => 3 - k,
                1 => [2, 0, 3, 1][k],
                _ => k ^ 1,
            };
            fr.quad = Some((k, to));
        }
        if let Some((seeds, _, step)) = &self.shatter {
            let e = d.explosion;
            let j = nearest_seed(seeds, centre);
            let s = seeds[j];
            let (cu, cv) = self.g.pivot;
            fr.shard = (e * (s.0 - cu), e * (s.1 - cv));
            fr.shard_ix = u32::try_from(j).unwrap_or(u32::MAX);
            if *step != 0
                && let Some(t) = self.tracks.get(usize::from(n.track))
            {
                let (id, ch) = t.sub_offset(n.sub, j as i64 * step);
                tpl.sub = id;
                tpl.ch = ch;
            }
        }
        (fr, tpl)
    }

    /// Every note's swept values: the settings', with the Control Sweeps
    /// applied at each note's place.
    fn vals(&self, src: &Source) -> (DVals, Vec<DVals>) {
        let base = DVals {
            amount: self.amount,
            lens: self.axis_lens,
            half: self.region.map_or(1.0, |r| r.3),
            sag: self.sag.as_ref().map_or(0.0, |s| s.0),
            strength: self.attract.as_ref().map_or(0.0, |a| a.1),
            te: self.taper.as_ref().map_or(0.0, |t| t.1),
            explosion: self.shatter.as_ref().map_or(0.0, |s| s.1),
            arc: self.arc,
            qx: self.quad.map_or(0.5, |q| q.0),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let f = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v, n, pl)
                    } else {
                        v
                    }
                };
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let mut d = base;
                d.amount = pct("amount", base.amount);
                if let Some((axis, fo, w, m)) = self.lens_in
                    && (sw.drives("n01.focus") || sw.drives("n01.mag"))
                {
                    d.lens = Some(lens_band(axis, f("n01.focus", fo), w, f("n01.mag", m)));
                }
                if sw.drives("n03.size") && self.region.is_some() {
                    d.half = (sw.float("n03.size", base.half * 200.0, n, pl) / 200.0).max(1e-6);
                }
                if sw.drives("n04.sag") {
                    d.sag = sw.int("n04.sag", base.sag as i64, n, pl) as f64;
                }
                d.strength = pct("n06.strength", base.strength).clamp(-1.0, 1.0);
                d.te = pct("n07.end", base.te).clamp(0.0, 1.0);
                d.explosion = pct("n08.explosion", base.explosion).clamp(0.0, 2.0);
                if sw.drives("n09.angle") && self.arc.is_some() {
                    let deg = self.arc.map_or(0.0, |a| a.0 * 180.0 / PI);
                    d.arc = Some(arc_of(sw.float("n09.angle", deg, n, pl), self.arc_keep));
                }
                d.qx = pct("n11.x", base.qx).clamp(0.01, 0.99);
                d
            })
            .collect();
        (base, per)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let tiles = self.tiles(src);
        let (base, per) = self.vals(src);
        let mark = out.mark();
        for (i, n) in src.notes.iter().enumerate() {
            let (fr, mut t) = self.frame(&tiles, per.get(i).copied().unwrap_or(base), i, n);
            if self.density {
                // Local area change (Jacobian) of the whole map at the
                // note's centre, as v0.8.
                let (u, v) = self.g.bbox.to_unit(
                    (n.start as f64 + n.end() as f64) / 2.0,
                    f64::from(n.pitch) + 0.5,
                );
                let e = 1e-3;
                let (a, b2) = self.point(&fr, u + e, v);
                let (c, d) = self.point(&fr, u - e, v);
                let (e1, f1) = self.point(&fr, u, v + e);
                let (g1, h1) = self.point(&fr, u, v - e);
                let jac = ((a - c) * (f1 - h1) - (b2 - d) * (e1 - g1)) / (4.0 * e * e);
                let k = (1.0 / jac.abs().max(0.05)).sqrt().clamp(0.25, 4.0);
                t.vel = (f64::from(n.vel) * k).round().clamp(1.0, 127.0) as u8;
            }
            out.set_dim(fr.shard_ix);
            if !self
                .g
                .rasterize(&t, &|u, v| self.point(&fr, u, v), &self.tracks, out)
            {
                break;
            }
            out.check()?;
        }
        if let Some((by, desc, gap)) = self.sort {
            pixel_sort(out, mark, &self.tracks, by, desc, gap);
        }
        Ok(())
    }
}

/// n10: within each (track, row) — or each run of notes closer than `gap`
/// in it (from the latest end so far) — the values of `by` (velocity,
/// length, subchannel with its channel; subchannels by their place in the
/// track's table) sorted along time, ties in time order; starts and
/// pitches stay. Two passes over the emitted notes (notes waiting for the
/// Shift policy count at tick 0).
fn pixel_sort(
    out: &mut Output,
    mark: Mark,
    tracks: &[SourceTrack],
    by: usize,
    desc: bool,
    gap: Option<u64>,
) {
    // (track, pitch, start, end, emission number) and each note's value.
    let mut keys: Vec<(u16, u8, u64, u64, usize)> = Vec::new();
    let mut vals: Vec<(u64, u32, u8)> = Vec::new();
    out.adjust_since(mark, |i, n, _, _| {
        keys.push((n.track, n.pitch, n.start, n.end(), i));
        vals.push(match by {
            0 => (u64::from(n.vel), 0, 0),
            1 => (n.dur, 0, 0),
            _ => {
                let place = tracks
                    .get(usize::from(n.track))
                    .and_then(|t| t.sub_index(n.sub))
                    .map_or(u64::from(n.sub), |x| x as u64);
                (place, n.sub, n.ch)
            }
        });
        true
    });
    keys.sort_unstable();
    let mut assign: Vec<Option<(u64, u32, u8)>> = vec![None; vals.len()];
    let mut buf: Vec<(u64, u32, u8)> = Vec::new();
    let mut a = 0;
    while a < keys.len() {
        let mut e = a + 1;
        while e < keys.len() && (keys[e].0, keys[e].1) == (keys[a].0, keys[a].1) {
            e += 1;
        }
        let mut r = a;
        while r < e {
            let mut s = r + 1;
            match gap {
                Some(gap) => {
                    let mut reach = keys[r].3;
                    while s < e && keys[s].2.saturating_sub(reach) <= gap {
                        reach = reach.max(keys[s].3);
                        s += 1;
                    }
                }
                None => s = e,
            }
            buf.clear();
            buf.extend(keys[r..s].iter().map(|k| vals[k.4]));
            buf.sort_by(|x, y| if desc { y.0.cmp(&x.0) } else { x.0.cmp(&y.0) });
            for (k, val) in keys[r..s].iter().zip(&buf) {
                assign[k.4] = Some(*val);
            }
            r = s;
        }
        a = e;
    }
    out.adjust_since(mark, |i, n, _, _| {
        if let Some((x, sub, ch)) = assign[i] {
            match by {
                0 => n.vel = x as u8,
                // Never past the last tick.
                1 => n.dur = x.min(u64::MAX - n.start).max(1),
                _ => {
                    n.sub = sub;
                    n.ch = ch;
                }
            }
        }
        true
    });
}

// ================================================================ Perspective

pub const PERSPECTIVE: ToolDef = ToolDef {
    id: "perspective",
    spec: perspective_spec,
    build: perspective_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PERSPECTIVE_HELP: &[(&str, &str)] = &[
    (
        "top",
        "How wide the top of the selection becomes compared with its bottom (below 100 % it narrows away into the distance; above 100 % it widens).",
    ),
    (
        "a1",
        "Places the selection's four corners freely instead of using the top edge width.",
    ),
    (
        "a1.corners",
        "Where the bottom-left, bottom-right, top-right and top-left corners go, as x, y pairs in thousandths of the selection's box.",
    ),
    ("a2", "Also turns the projected selection around its pivot."),
    (
        "a2.angle",
        "How far the projection is turned, in degrees (positive = counterclockwise).",
    ),
    (
        "a3",
        "Makes the notes recede toward a vanishing point to the left or right, more strongly the higher they are.",
    ),
    (
        "a3.x",
        "Where the vanishing point lies across time, as a share of the box's width (0 % = its left edge).",
    ),
    (
        "a3.depth",
        "How strongly the notes are drawn toward the vanishing point.",
    ),
    (
        "a4",
        "Makes the notes recede toward a vanishing point above or below, more strongly the later they are.",
    ),
    (
        "a4.y",
        "Where the vanishing point lies across pitch, as a share of the box's height (0 % = its bottom).",
    ),
    (
        "a4.depth",
        "How strongly the notes are drawn toward the vanishing point.",
    ),
    (
        "a5",
        "Makes the notes recede toward a point beyond the top-right corner, more strongly toward that corner.",
    ),
    (
        "a5.depth",
        "How strongly the notes are drawn toward that point.",
    ),
    (
        "a6",
        "Flattens the pitch spread over time by a curve, as if the plane tilts away.",
    ),
    (
        "a6.curve",
        "How much the rows are squeezed toward the pivot over the selection's time (1 = squeezed most).",
    ),
    (
        "a7",
        "Moves only the note starts and keeps every note's written length.",
    ),
    (
        "a8",
        "Squeezes the rows together as they near a horizon line in pitch.",
    ),
    (
        "a8.row",
        "The horizon's height, as a share of the box's height (100 % = its top edge).",
    ),
    (
        "a9",
        "Squeezes time together as it nears a horizon line in time.",
    ),
    (
        "a9.x",
        "The horizon's place in time, as a share of the box's width (100 % = its right edge).",
    ),
    (
        "a10",
        "Adds smaller copies of the projection, each further into the distance.",
    ),
    ("a10.count", "How many shrinking copies are added."),
    (
        "n01",
        "Makes notes quieter the farther away they are, like fog.",
    ),
    (
        "n01.near",
        "The velocity of the nearest notes, as a share of their own.",
    ),
    (
        "n01.far",
        "The velocity of the farthest notes, as a share of their own.",
    ),
    (
        "n01.shape",
        "How the velocity changes from near (left) to far (right).",
    ),
    ("n02", "Colors notes in bands by how far away they are."),
    (
        "n02.bands",
        "How many depth bands the projection is divided into.",
    ),
    (
        "n02.palette",
        "The subchannel position of each band from near to far, repeated (0 = the track's first subchannel).",
    ),
    ("n03", "Thins out notes the farther away they are (seeded)."),
    ("n03.keep", "The share of notes kept at the far end."),
    (
        "n03.shape",
        "How the share kept falls from near (left) to far (right).",
    ),
    (
        "n04",
        "Adds a mirrored copy of the projection, like a reflection on a floor or a ceiling.",
    ),
    (
        "n04.side",
        "Where the reflection appears: below the projection or above it.",
    ),
    (
        "n04.gap",
        "How many keys separate the reflection from the projection.",
    ),
    (
        "n04.fade",
        "How much quieter the reflection gets the farther it is from the mirror line.",
    ),
    (
        "n05",
        "Draws guide notes for the horizon and the lines leading to the vanishing point.",
    ),
    (
        "n05.lines",
        "How many vanishing lines are drawn (0 = only the horizon).",
    ),
    (
        "n05.len",
        "The length of each guide note along the lines, in ticks.",
    ),
    ("n05.vel", "The velocity of the guide notes."),
    (
        "n06",
        "Straightens a selection that is already in perspective back to a rectangle before projecting it.",
    ),
    (
        "n06.from",
        "Where the slanted shape comes from: found from the selection's left and right edges, or the corners listed below.",
    ),
    (
        "n06.corners",
        "The slanted shape's bottom-left, bottom-right, top-right and top-left corners, as x, y pairs in thousandths of the box.",
    ),
    ("n07", "Bends the projection's horizon into a curve."),
    (
        "n07.curvature",
        "How strongly the horizon bends (negative bends it the other way).",
    ),
    (
        "n08",
        "Hides notes behind others: where two notes land on the same key at the same time, only one stays.",
    ),
    (
        "n08.wins",
        "Which note stays where they overlap: the nearer one or the farther one.",
    ),
    (
        "n08.key",
        "What counts as the same key: only the pitch, or the pitch and the subchannel.",
    ),
    (
        "n09",
        "Adds a second copy for the other eye, shifted in time by depth, for a stereo pair.",
    ),
    (
        "n09.sep",
        "How far the farthest notes of the second copy are shifted, in ticks (nearer notes move less).",
    ),
    (
        "n09.sub",
        "The subchannel of the second copy, by its position in the track's list.",
    ),
    (
        "n10",
        "Adds a shadow copy of the projection, offset and quieter.",
    ),
    (
        "n10.x",
        "How far the shadow is shifted in time, in ticks (negative = earlier).",
    ),
    (
        "n10.y",
        "How many keys the shadow is shifted (negative = down).",
    ),
    ("n10.vel", "The shadow's velocity as a share of the note's."),
    (
        "n10.sub",
        "The shadow's subchannel, by its position in the track's list.",
    ),
    (
        "n11",
        "Blurs notes away from a focal depth by moving them at random (seeded), like a camera's depth of field.",
    ),
    (
        "n11.focus",
        "The depth that stays sharp (0 % = the nearest notes, 100 % = the farthest).",
    ),
    (
        "n11.time",
        "The largest random move in time at the most blurred depth, in ticks.",
    ),
    (
        "n11.rows",
        "The largest random move in pitch at the most blurred depth, in keys.",
    ),
];

fn perspective_spec() -> ToolSpec {
    let mut s = spec(
        "perspective",
        "Perspective",
        "Projects the selection as a plane seen in perspective: the top edge narrows (or widens) toward a vanishing point.",
        vec![
            float("top", "Top edge width", 5.0, 300.0, 50.0, "%"),
            adv_switch(1, "Four-corner projective"),
            list(
                "a1.corners",
                "Corners ‰ (BL x,y, BR x,y, TR x,y, TL x,y)",
                -2000,
                3000,
                8,
                &[0, 0, 1000, 0, 800, 1000, 200, 1000],
            )
            .adv(1),
            adv_switch(2, "Free rotation"),
            float("a2.angle", "Angle", -180.0, 180.0, 20.0, "°").adv(2),
            adv_switch(3, "Horizontal vanishing point"),
            float("a3.x", "Vanishing point X", -200.0, 300.0, 150.0, "%").adv(3),
            float("a3.depth", "Depth", 0.0, 95.0, 50.0, "%").adv(3),
            adv_switch(4, "Vertical vanishing point"),
            float("a4.y", "Vanishing point Y", -200.0, 300.0, 150.0, "%").adv(4),
            float("a4.depth", "Depth", 0.0, 95.0, 50.0, "%").adv(4),
            adv_switch(5, "Dual vanishing points"),
            float("a5.depth", "Depth of both", 0.0, 95.0, 40.0, "%").adv(5),
            adv_switch(6, "Depth envelope"),
            curve("a6.curve", "Depth over time", Curve::line(0.0, 1.0))
                .overshoot()
                .adv(6),
            adv_switch(7, "Preserve gate aspect"),
            adv_switch(8, "Pitch horizon"),
            float(
                "a8.row",
                "Horizon (of the box height)",
                -100.0,
                200.0,
                120.0,
                "%",
            )
            .adv(8),
            adv_switch(9, "Time horizon"),
            float(
                "a9.x",
                "Horizon (of the box width)",
                -100.0,
                200.0,
                120.0,
                "%",
            )
            .adv(9),
            adv_switch(10, "Tiled perspective echoes"),
            int("a10.count", "Echoes", 1, 8, 3).adv(10),
            // V09-S37: functions declared by stable id (TOOL_CATALOG n01 … n11).
            flag("n01", "Depth fog", false),
            float("n01.near", "Near velocity", 0.0, 200.0, 100.0, "%"),
            float("n01.far", "Far velocity", 0.0, 200.0, 30.0, "%"),
            curve("n01.shape", "Fog shape", Curve::line(0.0, 1.0)).overshoot(),
            flag("n02", "Depth color bands", false),
            int("n02.bands", "Bands", 2, 64, 4),
            list("n02.palette", "Palette", 0, 4095, 64, &[0, 1, 2, 3]),
            flag("n03", "Depth thinning", false),
            float("n03.keep", "Keep at the far end", 0.0, 100.0, 25.0, "%"),
            curve("n03.shape", "Thinning shape", Curve::line(0.0, 1.0)),
            flag("n04", "Floor reflection", false),
            choice("n04.side", "Side", REFLECT_SIDES, 0),
            int("n04.gap", "Gap", 0, 48, 2),
            float("n04.fade", "Fade", 0.0, 100.0, 60.0, "%"),
            flag("n05", "Horizon guides", false),
            int("n05.lines", "Vanishing lines", 0, 32, 8),
            ticks("n05.len", "Line note length", 1, 1 << 40, 240),
            int("n05.vel", "Line velocity", 1, 127, 40),
            flag("n06", "Rectify", false),
            choice("n06.from", "Corners from", RECT_FROM, 0),
            list(
                "n06.corners",
                "Corners ‰ (BL x,y, BR x,y, TR x,y, TL x,y)",
                0,
                1000,
                8,
                &[0, 0, 1000, 0, 750, 1000, 250, 1000],
            ),
            flag("n07", "Curved horizon", false),
            float("n07.curvature", "Curvature", -100.0, 100.0, 30.0, "%"),
            flag("n08", "Occlusion", false),
            choice("n08.wins", "Wins", OCC_WINS, 0),
            choice("n08.key", "Same key means", OCC_KEY, 0),
            flag("n09", "Stereo pair", false),
            ticks("n09.sep", "Eye separation", 0, 1 << 40, 120),
            int("n09.sub", "Second-eye subchannel", 0, 4095, 1),
            flag("n10", "Drop shadow", false),
            int("n10.x", "Offset X", -100_000, 100_000, 60),
            int("n10.y", "Offset Y", -48, 48, -2),
            float("n10.vel", "Shadow velocity", 0.0, 150.0, 40.0, "%"),
            int("n10.sub", "Shadow subchannel", 0, 4095, 2),
            flag("n11", "Depth of field", false),
            float("n11.focus", "Focal depth", 0.0, 100.0, 0.0, "%"),
            ticks("n11.time", "Blur in time", 0, 1 << 40, 240),
            int("n11.rows", "Blur in rows", 0, 24, 2),
        ],
        [
            "Four-corner projective",
            "Free rotation",
            "Horizontal vanishing point",
            "Vertical vanishing point",
            "Dual vanishing points",
            "Depth envelope",
            "Preserve gate aspect",
            "Pitch horizon",
            "Time horizon",
            "Tiled perspective echoes",
        ],
    );
    s.functions = [
        ("n01", "Depth fog", 510),
        ("n02", "Depth color bands", 520),
        ("n03", "Depth thinning", 700),
        ("n04", "Floor reflection", 600),
        ("n05", "Horizon guides", 110),
        ("n06", "Rectify", 120),
        ("n07", "Curved horizon", 130),
        ("n08", "Occlusion", 710),
        ("n09", "Stereo pair", 610),
        ("n10", "Drop shadow", 620),
        ("n11", "Depth of field", 140),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, PERSPECTIVE_HELP);
    s
}

/// Homography mapping the unit square onto quad `q` (BL, BR, TR, TL).
pub(crate) fn square_to_quad(q: [(f64, f64); 4]) -> [f64; 9] {
    let (x0, y0) = q[0];
    let (x1, y1) = q[1];
    let (x2, y2) = q[2];
    let (x3, y3) = q[3];
    let (dx1, dy1) = (x1 - x2, y1 - y2);
    let (dx2, dy2) = (x3 - x2, y3 - y2);
    let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
    let det = dx1 * dy2 - dx2 * dy1;
    let (g, h) = if det.abs() < 1e-12 || (sx.abs() < 1e-12 && sy.abs() < 1e-12) {
        (0.0, 0.0)
    } else {
        ((sx * dy2 - dx2 * sy) / det, (dx1 * sy - sx * dy1) / det)
    };
    [
        x1 - x0 + g * x1,
        x3 - x0 + h * x3,
        x0,
        y1 - y0 + g * y1,
        y3 - y0 + h * y3,
        y0,
        g,
        h,
        1.0,
    ]
}

#[inline]
pub(crate) fn apply_h(m: &[f64; 9], u: f64, v: f64) -> (f64, f64) {
    let w = m[6] * u + m[7] * v + m[8];
    let w = if w.abs() < 1e-9 { 1e-9 } else { w };
    (
        (m[0] * u + m[1] * v + m[2]) / w,
        (m[3] * u + m[4] * v + m[5]) / w,
    )
}

fn perspective_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S38 gate (E0409): Control Sweeps run inside the v0.9 kernel, per
    // note (`definition::kernel_sweeps`).
    if PERSPECTIVE_NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return persp9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let top = p.float("top") / 100.0;
    let quad = if p.adv(1) {
        let v = permille(&p.list("a1.corners"));
        let c = |i: usize, d: f64| v.get(i).copied().unwrap_or(d);
        [
            (c(0, 0.0), c(1, 0.0)),
            (c(2, 1.0), c(3, 0.0)),
            (c(4, 1.0), c(5, 1.0)),
            (c(6, 0.0), c(7, 1.0)),
        ]
    } else {
        let m = (1.0 - top) / 2.0;
        [(0.0, 0.0), (1.0, 0.0), (1.0 - m, 1.0), (m, 1.0)]
    };
    let h = square_to_quad(quad);
    let rot = p.adv(2).then(|| p.float("a2.angle") * PI / 180.0);
    let hvp = p
        .adv(3)
        .then(|| (p.float("a3.x") / 100.0, p.float("a3.depth") / 100.0));
    let vvp = p
        .adv(4)
        .then(|| (p.float("a4.y") / 100.0, p.float("a4.depth") / 100.0));
    let dual = p.adv(5).then(|| p.float("a5.depth") / 100.0);
    let env = p.adv(6).then(|| p.curve("a6.curve"));
    let keep_gate = p.adv(7);
    let ph = p.adv(8).then(|| p.float("a8.row") / 100.0);
    let th = p.adv(9).then(|| p.float("a9.x") / 100.0);
    let echoes = p.adv(10).then(|| p.int("a10.count").max(1) as u32);
    let (cu, cv) = g.pivot;
    let map = move |u: f64, v: f64| -> (f64, f64) {
        let (mut u2, mut v2) = apply_h(&h, u, v);
        if let Some(a) = rot {
            let (s, c) = a.sin_cos();
            let (x, y) = (u2 - cu, v2 - cv);
            u2 = cu + x * c - y * s;
            v2 = cv + x * s + y * c;
        }
        // Points recede toward a vanishing point as they go "deeper":
        // horizontal VP — depth grows with v; vertical VP — with u.
        if let Some((x, d)) = hvp {
            let k = d * v2.clamp(0.0, 1.0);
            u2 += (x - u2) * k;
            v2 *= 1.0 - d * 0.5;
        }
        if let Some((y, d)) = vvp {
            let k = d * u2.clamp(0.0, 1.0);
            v2 += (y - v2) * k;
        }
        if let Some(d) = dual {
            let k = d * u2.clamp(0.0, 1.0) * v2.clamp(0.0, 1.0);
            u2 += (1.5 - u2) * k;
            v2 += (1.5 - v2) * k;
        }
        if let Some(c) = &env {
            let k = c.eval(u.clamp(0.0, 1.0));
            v2 = cv + (v2 - cv) * (1.0 - 0.8 * k);
        }
        if let Some(hz) = ph {
            // Rows compress toward the horizon (1 / distance falloff).
            let dist = hz - v2;
            v2 = hz - dist / (1.0 + dist.abs());
        }
        if let Some(hz) = th {
            let dist = hz - u2;
            u2 = hz - dist / (1.0 + dist.abs());
        }
        (u2, v2)
    };
    let tracks = b.tracks.to_vec();
    let copies = echoes.unwrap_or(0);
    let per = g.per_note() * (u64::from(copies) + 1);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                for e in 0..=copies {
                    // V09-0143: the echo (0 = the note itself).
                    out.set_dim(e);
                    // Echo e: shrunk by 0.6^e toward the top of the plane.
                    let s = 0.6f64.powi(e as i32);
                    let ok = if keep_gate {
                        // Only the start moves; the length stays.
                        let out_one = |u: f64, v: f64| {
                            let (u2, v2) = map(u, v);
                            (u2 * s + (1.0 - s) * 0.5, v2 * s + (1.0 - s))
                        };
                        let (u0, v0) = g.bbox.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
                        let (u1, v1) = out_one(u0, v0);
                        let (x, y) = g.bbox.from_unit(u1, v1);
                        let (x, y) = g.affine(x, y);
                        out.emit(
                            x.round() as i128,
                            i128::from(n.dur),
                            y.floor() as i64,
                            i64::from(n.vel),
                            n,
                        )
                    } else {
                        g.rasterize(
                            n,
                            &|u, v| {
                                let (u2, v2) = map(u, v);
                                (u2 * s + (1.0 - s) * 0.5, v2 * s + (1.0 - s))
                            },
                            &tracks,
                            out,
                        )
                    };
                    if !ok {
                        return out.check();
                    }
                }
                out.check()?;
            }
            Ok(())
        },
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

// ----------------------------------------------------- Perspective (v0.9)

const PERSPECTIVE_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const REFLECT_SIDES: &[&str] = &["Below", "Above"];
const RECT_FROM: &[&str] = &["The selection's box", "Listed"];
const OCC_WINS: &[&str] = &["Near", "Far"];
const OCC_KEY: &[&str] = &["Key", "Key and subchannel"];
/// Most guide notes n05 may draw (refused with a sentence beyond).
const MAX_GUIDES: u64 = 1 << 22;

/// The v0.9 Perspective (V09-S37 step 3): the v0.8 map re-read from the
/// same parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions, in their catalog order (110 n05 guides, 120 n06
/// rectify, 130 n07 curved horizon, 140 n11 depth of field, 510 n01 fog,
/// 520 n02 bands, 600 n04 reflection, 610 n09 stereo, 620 n10 shadow, 700
/// n03 thinning, 710 n08 occlusion). A note's *projected depth* is the
/// v0.8 homography's projective scale w at its (rectified) centre,
/// normalised over the unit box's corners (0 near … 1 far); without
/// perspective (w constant) its row in the box.
#[derive(Clone)]
struct Persp9 {
    g: Geom,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    h: [f64; 9],
    rot: Option<f64>,
    hvp: Option<(f64, f64)>,
    vvp: Option<(f64, f64)>,
    dual: Option<f64>,
    env: Option<Curve>,
    keep_gate: bool,
    ph: Option<f64>,
    th: Option<f64>,
    echoes: u32,
    /// The projective scale's range over the unit box (depth).
    w_lo: f64,
    w_hi: f64,
    // ----- v0.9 functions
    /// n01: velocity factor near and far, shape.
    fog: Option<(f64, f64, Curve)>,
    /// n02: bands, palette (subchannel positions).
    bands: Option<(u32, Vec<i64>)>,
    /// n03: share kept at the far end, shape.
    thin: Option<(f64, Curve)>,
    /// n04: above, gap (rows), fade; the mirror row (box units).
    reflect: Option<(bool, f64, f64, f64)>,
    /// n05: guide notes, drawn once per run.
    guides: Option<Guides>,
    /// n06: the listed corners' inverse (else the selection's box per run).
    rectify: Option<Option<[f64; 9]>>,
    /// n07: curvature.
    curve: Option<f64>,
    /// n08: far wins, the key includes the subchannel.
    occl: Option<(bool, bool)>,
    /// n09: eye separation (ticks), second-eye subchannel position.
    stereo: Option<(f64, i64)>,
    /// n10: offset (ticks, rows), velocity factor, subchannel position.
    shadow: Option<(f64, f64, f64, i64)>,
    /// n11: focal depth, blur in time (ticks) and rows.
    dof: Option<(f64, f64, f64)>,
    /// The top edge (%) when [a1]'s corners are off (the per-note map with
    /// a sweep of it), and the sweeps' context.
    top_pct: Option<f64>,
    ctx: Ctx,
}

/// The v0.8 homography for a top edge of `top` (ratio of the width) and
/// its depth range over the unit box.
fn persp_h(top: f64) -> ([f64; 9], f64, f64) {
    let m = (1.0 - top) / 2.0;
    let h = square_to_quad([(0.0, 0.0), (1.0, 0.0), (1.0 - m, 1.0), (m, 1.0)]);
    let ws =
        [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| h[6] * u + h[7] * v + h[8]);
    let lo = ws.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = ws.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (h, lo, hi)
}

/// n04's mirror row: the projected box's bottom (or top) edge.
fn mirror_axis(h: &[f64; 9], above: bool) -> f64 {
    let vs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| apply_h(h, u, v).1);
    if above {
        vs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    } else {
        vs.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

/// One note's values of the parameters Perspective's Control Sweeps drive
/// (V09-S38 gate, E0409: per note inside the kernel, so n05's guides, n06's
/// box, n08's occlusion and n10's shadows see the whole selection).
#[derive(Clone, Copy, Debug)]
struct PVals {
    h: [f64; 9],
    w_lo: f64,
    w_hi: f64,
    axis: f64,
    far: f64,
    bands: u32,
    keep: f64,
    fade: f64,
    curve: f64,
    sep: f64,
    sx: f64,
    svel: f64,
    focus: f64,
}

/// n05's guide notes (absolute start, row), their length and velocity.
type Guides = (Vec<(i128, i64)>, u64, u8);

/// A 3 × 3 matrix's inverse (None when singular).
fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let co = [
        e * i - f * h,
        -(d * i - f * g),
        d * h - e * g,
        -(b * i - c * h),
        a * i - c * g,
        -(a * h - b * g),
        b * f - c * e,
        -(a * f - c * d),
        a * e - b * d,
    ];
    let det = a * co[0] + b * co[1] + c * co[2];
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    // Transposed cofactors / det.
    Some([
        co[0] / det,
        co[3] / det,
        co[6] / det,
        co[1] / det,
        co[4] / det,
        co[7] / det,
        co[2] / det,
        co[5] / det,
        co[8] / det,
    ])
}

/// Subchannel `pos` (a place in the track's table, wrapping) of `t`.
pub(super) fn sub_at(tracks: &[SourceTrack], n: &TNote, pos: i64) -> (u32, u8) {
    match tracks.get(usize::from(n.track)) {
        Some(t) if !t.subs.is_empty() => t.subs[pos.rem_euclid(t.subs.len() as i64) as usize],
        _ => (n.sub, n.ch),
    }
}

/// n05: the guide notes of homography `h` in box `g` — the horizon across
/// the box and `lines` vanishing lines (the images of evenly spaced
/// verticals u = (k + ½) / lines from the bottom row's centre to the top
/// row's), sampled
/// every `len` ticks or row, through the shared scale / position. None
/// without a vanishing point (no perspective).
fn guide_points(g: &Geom, h: &[f64; 9], lines: u32, len: u64) -> Option<Vec<(i128, i64)>> {
    if h[7].abs() < 1e-12 {
        return None;
    }
    let vp = (h[1] / h[7], h[4] / h[7]);
    let dir = if h[6].abs() < 1e-12 {
        (h[0], h[3])
    } else {
        (h[0] / h[6] - vp.0, h[3] / h[6] - vp.1)
    };
    let abs = |u: f64, v: f64| {
        let (x, y) = g.bbox.from_unit(u, v);
        g.affine(x, y)
    };
    let len = len.max(1) as f64;
    let mut pts: Vec<(i128, i64)> = Vec::new();
    let push = |x: f64, y: f64, pts: &mut Vec<(i128, i64)>| {
        let p = (x.round() as i128, y.floor() as i64);
        if pts.last() != Some(&p) {
            pts.push(p);
        }
    };
    // The horizon at every step across the box (where it is defined).
    if dir.0.abs() > 1e-12 {
        let steps = (g.bbox.width() / len).ceil().max(1.0) as u64;
        for k in 0..=steps {
            let u = (k as f64 * len / g.bbox.width()).min(1.0);
            let v = vp.1 + (u - vp.0) * dir.1 / dir.0;
            let (x, y) = abs(u, v);
            push(x, y, &mut pts);
        }
    }
    // From the bottom row's centre to the top row's.
    let (vb, vt) = (0.5 / g.bbox.rows(), 1.0 - 0.5 / g.bbox.rows());
    for k in 0..lines {
        let u = (f64::from(k) + 0.5) / f64::from(lines);
        let (b0, b1) = apply_h(h, u, vb);
        let (t0, t1) = apply_h(h, u, vt);
        let (bx, by) = abs(b0, b1);
        let (tx, ty) = abs(t0, t1);
        let n = (((tx - bx).abs() / len).ceil() + (ty - by).abs().ceil()).max(1.0) as u64;
        for i in 0..=n {
            let f = i as f64 / n as f64;
            let (x, y) = abs(b0 + (t0 - b0) * f, b1 + (t1 - b1) * f);
            push(x, y, &mut pts);
        }
    }
    Some(pts)
}

/// The number of guide points [`guide_points`] would make at most.
fn guide_bound(g: &Geom, h: &[f64; 9], lines: u32, len: u64) -> u64 {
    if h[7].abs() < 1e-12 {
        return 0;
    }
    let abs = |u: f64, v: f64| {
        let (x, y) = g.bbox.from_unit(u, v);
        g.affine(x, y)
    };
    let len = len.max(1) as f64;
    let mut total = (g.bbox.width() / len).ceil().max(1.0) + 1.0;
    let (vb, vt) = (0.5 / g.bbox.rows(), 1.0 - 0.5 / g.bbox.rows());
    for k in 0..lines {
        let u = (f64::from(k) + 0.5) / f64::from(lines);
        let (b0, b1) = apply_h(h, u, vb);
        let (t0, t1) = apply_h(h, u, vt);
        let (bx, by) = abs(b0, b1);
        let (tx, ty) = abs(t0, t1);
        total += (((tx - bx).abs() / len).ceil() + (ty - by).abs().ceil()).max(1.0) + 1.0;
    }
    if total.is_finite() {
        total.min(u64::MAX as f64) as u64
    } else {
        u64::MAX
    }
}

fn persp9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let top = p.float("top") / 100.0;
    let quad = if p.adv(1) {
        let v = permille(&p.list("a1.corners"));
        let c = |i: usize, d: f64| v.get(i).copied().unwrap_or(d);
        [
            (c(0, 0.0), c(1, 0.0)),
            (c(2, 1.0), c(3, 0.0)),
            (c(4, 1.0), c(5, 1.0)),
            (c(6, 0.0), c(7, 1.0)),
        ]
    } else {
        let m = (1.0 - top) / 2.0;
        [(0.0, 0.0), (1.0, 0.0), (1.0 - m, 1.0), (m, 1.0)]
    };
    let h = square_to_quad(quad);
    let ws =
        [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| h[6] * u + h[7] * v + h[8]);
    let w_lo = ws.iter().copied().fold(f64::INFINITY, f64::min);
    let w_hi = ws.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let guides = if p.flag("n05") {
        let lines = p.int("n05.lines").clamp(0, 32) as u32;
        let len = p.ticks("n05.len").max(1);
        let bound = guide_bound(&g, &h, lines, len);
        if bound > MAX_GUIDES {
            return Err(TransformError::TooLarge(format!(
                "Horizon guides would draw up to {} notes (at most {}). Lengthen the line notes or draw fewer vanishing lines.",
                crate::midi::group_digits(bound),
                crate::midi::group_digits(MAX_GUIDES)
            )));
        }
        guide_points(&g, &h, lines, len).map(|pts| (pts, len, p.int("n05.vel").clamp(1, 127) as u8))
    } else {
        None
    };
    let reflect = p.flag("n04").then(|| {
        let above = p.choice("n04.side") == 1;
        // The mirror row: the projected box's bottom (or top) edge.
        let vs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| apply_h(&h, u, v).1);
        let axis = if above {
            vs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        } else {
            vs.iter().copied().fold(f64::INFINITY, f64::min)
        };
        (
            above,
            p.int("n04.gap").clamp(0, 48) as f64,
            (p.float("n04.fade") / 100.0).clamp(0.0, 1.0),
            axis,
        )
    });
    let rectify = p.flag("n06").then(|| {
        (p.choice("n06.from") == 1).then(|| {
            let v = permille(&p.list("n06.corners"));
            let c = |i: usize, d: f64| v.get(i).copied().unwrap_or(d);
            let q = [
                (c(0, 0.0), c(1, 0.0)),
                (c(2, 1.0), c(3, 0.0)),
                (c(4, 1.0), c(5, 1.0)),
                (c(6, 0.0), c(7, 1.0)),
            ];
            invert3(&square_to_quad(q)).unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])
        })
    });
    let k = Persp9 {
        h,
        rot: p.adv(2).then(|| p.float("a2.angle") * PI / 180.0),
        hvp: p
            .adv(3)
            .then(|| (p.float("a3.x") / 100.0, p.float("a3.depth") / 100.0)),
        vvp: p
            .adv(4)
            .then(|| (p.float("a4.y") / 100.0, p.float("a4.depth") / 100.0)),
        dual: p.adv(5).then(|| p.float("a5.depth") / 100.0),
        env: p.adv(6).then(|| p.curve("a6.curve")),
        keep_gate: p.adv(7),
        ph: p.adv(8).then(|| p.float("a8.row") / 100.0),
        th: p.adv(9).then(|| p.float("a9.x") / 100.0),
        echoes: if p.adv(10) {
            p.int("a10.count").max(1) as u32
        } else {
            0
        },
        w_lo,
        w_hi,
        fog: p.flag("n01").then(|| {
            (
                p.float("n01.near") / 100.0,
                p.float("n01.far") / 100.0,
                p.curve("n01.shape"),
            )
        }),
        bands: p.flag("n02").then(|| {
            let mut pal = p.list("n02.palette");
            if pal.is_empty() {
                pal.push(0);
            }
            (p.int("n02.bands").clamp(2, 64) as u32, pal)
        }),
        thin: p.flag("n03").then(|| {
            (
                (p.float("n03.keep") / 100.0).clamp(0.0, 1.0),
                p.curve("n03.shape"),
            )
        }),
        reflect,
        guides,
        rectify,
        curve: p
            .flag("n07")
            .then(|| (p.float("n07.curvature") / 100.0).clamp(-1.0, 1.0)),
        occl: p
            .flag("n08")
            .then(|| (p.choice("n08.wins") == 1, p.choice("n08.key") == 1)),
        stereo: p
            .flag("n09")
            .then(|| (p.ticks("n09.sep") as f64, p.int("n09.sub"))),
        shadow: p.flag("n10").then(|| {
            (
                p.int("n10.x") as f64,
                p.int("n10.y") as f64,
                (p.float("n10.vel") / 100.0).clamp(0.0, 1.5),
                p.int("n10.sub"),
            )
        }),
        dof: p.flag("n11").then(|| {
            (
                (p.float("n11.focus") / 100.0).clamp(0.0, 1.0),
                p.ticks("n11.time") as f64,
                p.int("n11.rows").clamp(0, 24) as f64,
            )
        }),
        top_pct: (!p.adv(1)).then(|| p.float("top")),
        ctx: b.ctx.clone(),
        g,
        tracks: b.tracks.to_vec(),
    };
    let mut per = k.g.per_note() * (u64::from(k.echoes) + 1);
    per = per.saturating_mul(
        1 + u64::from(k.reflect.is_some())
            + u64::from(k.stereo.is_some())
            + u64::from(k.shadow.is_some()),
    );
    let extra = k.guides.as_ref().map_or(0, |gd| gd.0.len() as u64);
    let estimate = (per > 1 || extra > 0).then(|| {
        (b.stats.count as u64)
            .saturating_mul(per)
            .saturating_add(extra)
    });
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth: if per > 1 || extra > 0 {
            Growth::Expand(per.max(2))
        } else {
            Growth::Map
        },
        estimate,
    })
}

/// One note copy's settings for [`Persp9::point`].
#[derive(Clone, Copy, Debug)]
struct PFrame {
    /// The note's homography and n07 curvature ([`PVals`]).
    h: [f64; 9],
    curve: f64,
    /// Echo scale (a10).
    s: f64,
    /// n11's offset and the copy's offset (box units).
    du: f64,
    dv: f64,
    /// n04: mirror about this row (box units) with the gap.
    mirror: Option<(f64, f64)>,
}

impl Persp9 {
    /// The v0.8 map (`perspective_build`'s, unchanged).
    #[inline(always)]
    fn v08(&self, h: &[f64; 9], u: f64, v: f64) -> (f64, f64) {
        let (cu, cv) = self.g.pivot;
        let (mut u2, mut v2) = apply_h(h, u, v);
        if let Some(a) = self.rot {
            let (s, c) = a.sin_cos();
            let (x, y) = (u2 - cu, v2 - cv);
            u2 = cu + x * c - y * s;
            v2 = cv + x * s + y * c;
        }
        if let Some((x, d)) = self.hvp {
            let k = d * v2.clamp(0.0, 1.0);
            u2 += (x - u2) * k;
            v2 *= 1.0 - d * 0.5;
        }
        if let Some((y, d)) = self.vvp {
            let k = d * u2.clamp(0.0, 1.0);
            v2 += (y - v2) * k;
        }
        if let Some(d) = self.dual {
            let k = d * u2.clamp(0.0, 1.0) * v2.clamp(0.0, 1.0);
            u2 += (1.5 - u2) * k;
            v2 += (1.5 - v2) * k;
        }
        if let Some(c) = &self.env {
            let k = c.eval(u.clamp(0.0, 1.0));
            v2 = cv + (v2 - cv) * (1.0 - 0.8 * k);
        }
        if let Some(hz) = self.ph {
            let dist = hz - v2;
            v2 = hz - dist / (1.0 + dist.abs());
        }
        if let Some(hz) = self.th {
            let dist = hz - u2;
            u2 = hz - dist / (1.0 + dist.abs());
        }
        (u2, v2)
    }

    /// The image of point (u, v) for a copy (`rect`: n06's inverse).
    #[inline(always)]
    fn point(&self, rect: Option<&[f64; 9]>, fr: &PFrame, u: f64, v: f64) -> (f64, f64) {
        let (u, v) = match rect {
            Some(m) => apply_h(m, u, v),
            None => (u, v),
        };
        let (u2, v2) = self.v08(&fr.h, u, v);
        let s = fr.s;
        let (mut u2, mut v2) = (u2 * s + (1.0 - s) * 0.5, v2 * s + (1.0 - s));
        if self.curve.is_some() && fr.curve != 0.0 {
            let c = fr.curve;
            let x = 2.0 * u2 - 1.0;
            v2 -= c * v2 * x * x;
        }
        u2 += fr.du;
        v2 += fr.dv;
        if let Some((axis, gap)) = fr.mirror {
            v2 = 2.0 * axis - v2 + gap;
        }
        (u2, v2)
    }

    /// Projected depth of a (rectified) point, 0 near … 1 far.
    fn depth(&self, pv: &PVals, u: f64, v: f64) -> f64 {
        if pv.w_hi - pv.w_lo < 1e-9 {
            return v.clamp(0.0, 1.0);
        }
        let w = pv.h[6] * u + pv.h[7] * v + pv.h[8];
        ((w - pv.w_lo) / (pv.w_hi - pv.w_lo)).clamp(0.0, 1.0)
    }

    /// Every note's swept values: the settings', with the Control Sweeps
    /// applied at each note's place.
    fn vals(&self, src: &Source) -> (PVals, Vec<PVals>) {
        let base = PVals {
            h: self.h,
            w_lo: self.w_lo,
            w_hi: self.w_hi,
            axis: self.reflect.map_or(0.0, |r| r.3),
            far: self.fog.as_ref().map_or(1.0, |f| f.1),
            bands: self.bands.as_ref().map_or(2, |b| b.0),
            keep: self.thin.as_ref().map_or(1.0, |t| t.0),
            fade: self.reflect.map_or(0.0, |r| r.2),
            curve: self.curve.unwrap_or(0.0),
            sep: self.stereo.map_or(0.0, |s| s.0),
            sx: self.shadow.map_or(0.0, |s| s.0),
            svel: self.shadow.map_or(1.0, |s| s.2),
            focus: self.dof.map_or(0.0, |d| d.0),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let mut d = base;
                if let Some(top) = self.top_pct
                    && sw.drives("top")
                {
                    let (h, lo, hi) = persp_h(sw.float("top", top, n, pl) / 100.0);
                    (d.h, d.w_lo, d.w_hi) = (h, lo, hi);
                    if let Some((above, ..)) = self.reflect {
                        d.axis = mirror_axis(&h, above);
                    }
                }
                d.far = pct("n01.far", base.far);
                if sw.drives("n02.bands") {
                    d.bands = sw
                        .int("n02.bands", i64::from(base.bands), n, pl)
                        .clamp(2, 64) as u32;
                }
                d.keep = pct("n03.keep", base.keep).clamp(0.0, 1.0);
                d.fade = pct("n04.fade", base.fade).clamp(0.0, 1.0);
                d.curve = pct("n07.curvature", base.curve).clamp(-1.0, 1.0);
                if sw.drives("n09.sep") {
                    d.sep = sw.int("n09.sep", base.sep as i64, n, pl) as f64;
                }
                if sw.drives("n10.x") {
                    d.sx = sw.int("n10.x", base.sx as i64, n, pl) as f64;
                }
                d.svel = pct("n10.vel", base.svel).clamp(0.0, 1.5);
                d.focus = pct("n11.focus", base.focus).clamp(0.0, 1.0);
                d
            })
            .collect();
        (base, per)
    }

    /// n06 "the selection's box": the trapezoid of the selection's left and
    /// right edges (the rows of the notes starting in the first quarter of
    /// the box, ending in the last) taken back to the box.
    fn rect_of(&self, src: &Source) -> Option<[f64; 9]> {
        let b = &self.g.bbox;
        let (w, t0, t1) = (b.width(), b.t0 as f64, b.t1 as f64);
        let (mut l, mut r) = ((u8::MAX, 0u8), (u8::MAX, 0u8));
        for n in &src.notes {
            if (n.start as f64) < t0 + w / 4.0 {
                l = (l.0.min(n.pitch), l.1.max(n.pitch));
            }
            if (n.end() as f64) > t1 - w / 4.0 {
                r = (r.0.min(n.pitch), r.1.max(n.pitch));
            }
        }
        if l.0 > l.1 || r.0 > r.1 {
            return None;
        }
        let row = |p: f64| (p - f64::from(b.p0)) / b.rows();
        let q = [
            (0.0, row(f64::from(l.0))),
            (1.0, row(f64::from(r.0))),
            (1.0, row(f64::from(r.1) + 1.0)),
            (0.0, row(f64::from(l.1) + 1.0)),
        ];
        invert3(&square_to_quad(q))
    }

    /// Emits note `t` (template) mapped by `fr`; `rect` n06. Returns false
    /// once the output is full.
    fn emit(
        &self,
        rect: Option<&[f64; 9]>,
        fr: &PFrame,
        n: &TNote,
        t: &TNote,
        out: &mut Output,
    ) -> bool {
        if self.keep_gate {
            // Only the start moves; the length stays (v0.8 [a7]).
            let (u0, v0) = self
                .g
                .bbox
                .to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
            let (u1, v1) = self.point(rect, fr, u0, v0);
            let (x, y) = self.g.bbox.from_unit(u1, v1);
            let (x, y) = self.g.affine(x, y);
            out.emit(
                x.round() as i128,
                i128::from(n.dur),
                y.floor() as i64,
                i64::from(t.vel),
                t,
            )
        } else {
            self.g
                .rasterize(t, &|u, v| self.point(rect, fr, u, v), &self.tracks, out)
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let base = out.notes.len();
        let first_run = out.is_empty();
        // Per emitted note (out.notes from `base`): depth, shadow.
        let mut info: Vec<(f32, bool)> = Vec::new();
        let rect_box = match self.rectify {
            Some(None) => self.rect_of(src),
            _ => None,
        };
        let rect: Option<&[f64; 9]> = match &self.rectify {
            Some(Some(m)) => Some(m),
            Some(None) => rect_box.as_ref(),
            None => None,
        };
        let track = |out: &Output, from: usize, d: f64, sh: bool, info: &mut Vec<(f32, bool)>| {
            for _ in from..out.notes.len() {
                info.push((d as f32, sh));
            }
        };
        if first_run
            && let Some((pts, len, vel)) = &self.guides
            && let Some(n0) = src.notes.first()
        {
            let mut t = *n0;
            t.vel = *vel;
            t.flags &= !super::super::FLAG_MUTED;
            out.set_dim(0);
            for &(x, row) in pts {
                let from = out.notes.len();
                if !out.emit(x, i128::from(*len), row, i64::from(*vel), &t) {
                    return out.check();
                }
                track(out, from, 0.0, false, &mut info);
            }
        }
        let (bw, rows) = (self.g.bbox.width(), self.g.bbox.rows());
        let (pbase, per) = self.vals(src);
        for (i, n) in src.notes.iter().enumerate() {
            let pv = per.get(i).unwrap_or(&pbase);
            let key = note_key(n);
            let (cu, cv) = self.g.bbox.to_unit(
                (n.start as f64 + n.end() as f64) / 2.0,
                f64::from(n.pitch) + 0.5,
            );
            let (ru, rv) = match rect {
                Some(m) => apply_h(m, cu, cv),
                None => (cu, cv),
            };
            let d0 = self.depth(pv, ru, rv);
            for e in 0..=self.echoes {
                out.set_dim(e);
                let s = 0.6f64.powi(e as i32);
                // An echo lies deeper: toward the far edge by its shrink.
                let d = 1.0 - s * (1.0 - d0);
                if let Some((_, c)) = &self.thin {
                    let p = 1.0 - (1.0 - pv.keep) * c.eval(d);
                    let mut r = stream(0, salt_of("perspective.n03"), &[key, u64::from(e)]);
                    if r.unit() >= p {
                        continue;
                    }
                }
                let mut fr = PFrame {
                    h: pv.h,
                    curve: pv.curve,
                    s,
                    du: 0.0,
                    dv: 0.0,
                    mirror: None,
                };
                if let Some((_, bt, br)) = self.dof {
                    let blur = (d - pv.focus).abs();
                    let mut r = stream(0, salt_of("perspective.n11"), &[key, u64::from(e)]);
                    let (a, b2) = (r.unit(), r.unit());
                    fr.du = (bt * blur * (2.0 * a - 1.0)).round() / bw;
                    fr.dv = (br * blur * (2.0 * b2 - 1.0)).round() / rows;
                }
                let mut t = *n;
                if let Some((near, _, c)) = &self.fog {
                    let far = pv.far;
                    let k = near + (far - near) * c.eval(d);
                    t.vel = (f64::from(n.vel) * k).round().clamp(1.0, 127.0) as u8;
                }
                if let Some((_, pal)) = &self.bands {
                    let bands = pv.bands;
                    let k = ((d * f64::from(bands)) as u32).min(bands - 1) as usize;
                    let (id, ch) = sub_at(&self.tracks, n, pal[k % pal.len()]);
                    t.sub = id;
                    t.ch = ch;
                }
                let from = out.notes.len();
                if !self.emit(rect, &fr, n, &t, out) {
                    return out.check();
                }
                track(out, from, d, false, &mut info);
                if let Some((above, gap, _, _)) = self.reflect {
                    let (fade, axis) = (pv.fade, pv.axis);
                    let mut c = t;
                    let (_, pv) = self.point(rect, &fr, cu, cv);
                    let dist = (pv - axis).abs().min(1.0);
                    c.vel = (f64::from(t.vel) * (1.0 - fade * dist))
                        .round()
                        .clamp(1.0, 127.0) as u8;
                    let gap = gap / rows * if above { 1.0 } else { -1.0 };
                    let cf = PFrame {
                        mirror: Some((axis, gap)),
                        ..fr
                    };
                    let from = out.notes.len();
                    if !self.emit(rect, &cf, n, &c, out) {
                        return out.check();
                    }
                    track(out, from, d, false, &mut info);
                }
                if let Some((_, pos)) = self.stereo {
                    let sep = pv.sep;
                    let mut c = t;
                    let (id, ch) = sub_at(&self.tracks, n, pos);
                    c.sub = id;
                    c.ch = ch;
                    let cf = PFrame {
                        du: fr.du + (sep * d).round() / bw,
                        ..fr
                    };
                    let from = out.notes.len();
                    if !self.emit(rect, &cf, n, &c, out) {
                        return out.check();
                    }
                    track(out, from, d, false, &mut info);
                }
                if let Some((_, dy, _, pos)) = self.shadow {
                    let (dx, vel) = (pv.sx, pv.svel);
                    let mut c = t;
                    c.vel = (f64::from(t.vel) * vel).round().clamp(1.0, 127.0) as u8;
                    let (id, ch) = sub_at(&self.tracks, n, pos);
                    c.sub = id;
                    c.ch = ch;
                    let cf = PFrame {
                        du: fr.du + dx / bw,
                        dv: fr.dv + dy / rows,
                        ..fr
                    };
                    let from = out.notes.len();
                    if !self.emit(rect, &cf, n, &c, out) {
                        return out.check();
                    }
                    track(out, from, d, true, &mut info);
                }
            }
            out.check()?;
        }
        if self.shadow.is_some() || self.occl.is_some() {
            hide(&mut out.notes, base, &info, self.occl);
        }
        Ok(())
    }
}

/// The longest part of `[s, e)` that `cover` — disjoint intervals sorted by
/// start — leaves free (the first of equal ones), or None.
fn longest_free(cover: &[(u64, u64)], s: u64, e: u64) -> Option<(u64, u64)> {
    let mut best: Option<(u64, u64)> = None;
    let take = |a: u64, b: u64, best: &mut Option<(u64, u64)>| {
        if b > a && best.is_none_or(|(x, y)| b - a > y - x) {
            *best = Some((a, b));
        }
    };
    let mut at = s;
    let from = cover.partition_point(|c| c.1 <= s);
    for &(a, b) in &cover[from..] {
        if a >= e {
            break;
        }
        take(at, a, &mut best);
        at = at.max(b);
    }
    take(at, e, &mut best);
    best
}

/// n10 / n08: shadows never cover art on their key (each keeps its longest
/// uncovered part, or goes); with `occl`, per key (track, row, and the
/// subchannel when asked) nearer notes (farther with `far wins`) hide what
/// they cover of the others (ties: emission order) — every note keeps its
/// longest uncovered part, the whole spans of the notes before it staying
/// solid. `info` holds each note's depth and shadow flag from `base` on.
/// Sorts and flat buffers only: O(n log n), no allocation per note.
pub(super) fn hide(
    notes: &mut Vec<TNote>,
    base: usize,
    info: &[(f32, bool)],
    occl: Option<(bool, bool)>,
) {
    let n = info.len().min(notes.len().saturating_sub(base));
    let mut keep = vec![true; n];
    if info[..n].iter().any(|x| x.1) {
        // The art's merged spans per (track, row).
        let mut art: Vec<(u16, u8, u64, u64)> = (0..n)
            .filter(|&i| !info[i].1)
            .map(|i| {
                let t = &notes[base + i];
                (t.track, t.pitch, t.start, t.end())
            })
            .collect();
        art.sort_unstable();
        let mut merged: Vec<((u16, u8), (u64, u64))> = Vec::with_capacity(art.len());
        for (tr, p, a, b) in art {
            match merged.last_mut() {
                Some((k, iv)) if *k == (tr, p) && a <= iv.1 => iv.1 = iv.1.max(b),
                _ => merged.push(((tr, p), (a, b))),
            }
        }
        let spans: Vec<(u64, u64)> = merged.iter().map(|m| m.1).collect();
        // The shadows in (track, row, start) order walk the spans once.
        let mut shadows: Vec<(u16, u8, u64, usize)> = (0..n)
            .filter(|&i| info[i].1)
            .map(|i| {
                let t = &notes[base + i];
                (t.track, t.pitch, t.start, i)
            })
            .collect();
        shadows.sort_unstable();
        let (mut at, mut hi) = (0, 0);
        let mut current: Option<(u16, u8)> = None;
        for (tr, p, _, i) in shadows {
            if current != Some((tr, p)) {
                // The key's spans: [at, hi).
                current = Some((tr, p));
                at = hi.max(at);
                while at < merged.len() && merged[at].0 < (tr, p) {
                    at += 1;
                }
                hi = at;
                while hi < merged.len() && merged[hi].0 == (tr, p) {
                    hi += 1;
                }
            }
            if at == hi {
                continue;
            }
            let t = &mut notes[base + i];
            match longest_free(&spans[at..hi], t.start, t.end()) {
                Some((a, b)) => {
                    t.start = a;
                    t.dur = b - a;
                }
                None => keep[i] = false,
            }
        }
    }
    if let Some((far_wins, with_sub)) = occl {
        // Notes grouped by key, each group in winning order.
        let rank = |d: f32| {
            let b = d.to_bits();
            if far_wins { u32::MAX - b } else { b }
        };
        let mut order: Vec<(u16, u8, u32, u32, usize)> = (0..n)
            .filter(|&i| !info[i].1 && keep[i])
            .map(|i| {
                let t = &notes[base + i];
                let sub = if with_sub { t.sub } else { 0 };
                (t.track, t.pitch, sub, rank(info[i].0), i)
            })
            .collect();
        order.sort_unstable();
        let (mut coords, mut next): (Vec<u64>, Vec<usize>) = (Vec::new(), Vec::new());
        let mut a = 0;
        while a < order.len() {
            let mut e = a + 1;
            let key = |o: &(u16, u8, u32, u32, usize)| (o.0, o.1, o.2);
            while e < order.len() && key(&order[e]) == key(&order[a]) {
                e += 1;
            }
            let group = &order[a..e];
            // Elementary segments between the group's ends; `next[x]` is
            // the first segment ≥ x not yet covered (union-find).
            coords.clear();
            for g in group {
                let t = &notes[base + g.4];
                coords.push(t.start);
                coords.push(t.end());
            }
            coords.sort_unstable();
            coords.dedup();
            let m = coords.len().saturating_sub(1);
            next.clear();
            next.extend(0..=m);
            fn find(next: &mut [usize], x: usize) -> usize {
                let mut r = x;
                while next[r] != r {
                    r = next[r];
                }
                let mut y = x;
                while next[y] != r {
                    let z = next[y];
                    next[y] = r;
                    y = z;
                }
                r
            }
            for g in group {
                let t = &mut notes[base + g.4];
                let i = coords.partition_point(|&c| c < t.start);
                let j = coords.partition_point(|&c| c < t.end());
                // The free segments in [i, j), in order; the longest run of
                // touching ones; then all of [i, j) becomes solid.
                let mut best: Option<(u64, u64)> = None;
                let mut run: Option<(u64, u64)> = None;
                let mut x = find(&mut next, i);
                while x < j {
                    let (s0, s1) = (coords[x], coords[x + 1]);
                    run = match run {
                        Some((rs, re)) if re == s0 => Some((rs, s1)),
                        Some(r) => {
                            if best.is_none_or(|(p, q)| r.1 - r.0 > q - p) {
                                best = Some(r);
                            }
                            Some((s0, s1))
                        }
                        None => Some((s0, s1)),
                    };
                    next[x] = x + 1;
                    x = find(&mut next, x + 1);
                }
                if let Some(r) = run
                    && best.is_none_or(|(p, q)| r.1 - r.0 > q - p)
                {
                    best = Some(r);
                }
                match best {
                    Some((s0, s1)) => {
                        t.start = s0;
                        t.dur = s1 - s0;
                    }
                    None => keep[g.4] = false,
                }
            }
            a = e;
        }
    }
    let mut i = 0;
    notes.retain(|_| {
        let k = i < base || keep.get(i - base).copied().unwrap_or(true);
        i += 1;
        k
    });
}

// ======================================================================= Warp

pub const WARP: ToolDef = ToolDef {
    id: "warp",
    spec: warp_spec,
    build: warp_build,
};

const MESH_SHAPES: &[&str] = &["Wave", "Twist", "Ripple", "S-curve"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const WARP_HELP: &[(&str, &str)] = &[
    (
        "x",
        "Where the warp point sits in time, as a share of the selection's box (0 % = its start, 100 % = its end).",
    ),
    (
        "y",
        "Where the warp point sits in pitch, as a share of the selection's box (0 % = its lowest key, 100 % = its highest).",
    ),
    (
        "dx",
        "How far the warp point pushes along time, as a share of the box's width (− = earlier).",
    ),
    (
        "dy",
        "How far the warp point pushes along pitch, as a share of the box's height (− = lower).",
    ),
    (
        "radius",
        "How far the push reaches around the warp point, as a share of the box (fading smoothly to nothing at the edge).",
    ),
    (
        "a1",
        "Warps with a grid of mesh nodes instead of one point: each node moves its surroundings.",
    ),
    ("a1.rows", "How many rows of nodes the mesh has."),
    ("a1.cols", "How many columns of nodes the mesh has."),
    (
        "a1.nodes",
        "Each node's offset in thousandths of the box, as x, y pairs, row by row from the bottom.",
    ),
    ("a2", "Places free nodes anywhere, each with its own push."),
    (
        "a2.nodes",
        "Each node as x, y, push x, push y, in thousandths of the box.",
    ),
    (
        "a3",
        "Interpolates between the mesh nodes with smooth curves instead of straight lines.",
    ),
    (
        "a4",
        "Pins points that never move; the warp fades out around them.",
    ),
    (
        "a4.pins",
        "The pins' positions as x, y pairs, in thousandths of the box.",
    ),
    (
        "a4.radius",
        "How far around each pin the notes stay put, as a share of the box.",
    ),
    (
        "a5",
        "Keeps the edges of the selection's box in place; only the inside warps.",
    ),
    (
        "a6",
        "Wraps notes pushed past the box's edge around to the opposite side.",
    ),
    (
        "a7",
        "Mirrors the mesh: nodes on one side move like their mirror images on the other.",
    ),
    ("a8", "Adds a local push with a brush in one area."),
    (
        "a8.x",
        "The brush's position in time, as a share of the box.",
    ),
    (
        "a8.y",
        "The brush's position in pitch, as a share of the box.",
    ),
    ("a8.radius", "The brush's size, as a share of the box."),
    (
        "a8.dx",
        "How far the brush pushes along time, as a share of the box (− = earlier).",
    ),
    ("a9", "Warps by a ready-made mesh shape."),
    (
        "a9.shape",
        "The shape: a wave, a twist, a ripple or an S-curve.",
    ),
    ("a9.amount", "How strong the shape is."),
    (
        "a10",
        "Morphs the mesh from its own offsets (A) to a second set (B) across the selection.",
    ),
    (
        "a10.b",
        "Mesh B's node offsets for a 3 × 3 mesh, as x, y pairs in thousandths of the box.",
    ),
    (
        "n01",
        "Smooths the mesh: each node moves toward the average of its neighbors.",
    ),
    ("n01.iter", "How many smoothing passes run."),
    (
        "n01.amount",
        "How far each node moves toward its neighbors' average per pass.",
    ),
    (
        "n02",
        "Bends the selection so its outline fits the outline of a reference.",
    ),
    (
        "n02.ref",
        "The reference: a track, a subchannel of the track, or the track's unselected notes.",
    ),
    (
        "n02.num",
        "The reference's number (a track or subchannel number).",
    ),
    (
        "n02.fit",
        "Outline per Row fits each pitch row's extent; Bounding Rows fits the overall top and bottom.",
    ),
    ("n03", "Cuts the box into cells and rearranges them."),
    ("n03.rows", "How many rows of cells."),
    ("n03.cols", "How many columns of cells."),
    (
        "n03.arrange",
        "How the cells move: shuffled (seeded), rows rotated, columns rotated, or by your list.",
    ),
    (
        "n03.order",
        "For List: the cell each cell moves to, counted row by row from the bottom left (0 = the first).",
    ),
    (
        "n04",
        "Bends the selection through a complex-number function (a conformal map, keeping small shapes' angles).",
    ),
    ("n04.fn", "The function: z², √z, 1/z, exp, log or sin."),
    (
        "n04.strength",
        "How far the notes move toward the mapped position (100 % = fully).",
    ),
    (
        "n05",
        "Moves the mesh nodes by seeded noise, for an organic wobble.",
    ),
    (
        "n05.amount",
        "How far the nodes may move, as a share of the box.",
    ),
    (
        "n05.scale",
        "How many noise nodes span the box (more = finer wobble).",
    ),
    (
        "n06",
        "Moves every chord as one block, so warped chords stay intact.",
    ),
    (
        "n06.tol",
        "Notes that start within this many ticks of each other count as one chord.",
    ),
    (
        "n06.rigid",
        "Rigid in time only (each voice may still move in pitch), or in time and pitch.",
    ),
    (
        "n07",
        "Warps with a round mesh: rings and spokes around a center.",
    ),
    ("n07.rings", "How many rings of nodes."),
    ("n07.spokes", "How many spokes (nodes per ring)."),
    (
        "n07.nodes",
        "Each node's offset in thousandths of the box, as x, y pairs, ring by ring from the center.",
    ),
    (
        "n07.x",
        "The center's position in time, as a share of the box.",
    ),
    (
        "n07.y",
        "The center's position in pitch, as a share of the box.",
    ),
    (
        "n08",
        "Folds the selection over a line, like folding paper.",
    ),
    (
        "n08.angle",
        "The fold line's angle (90° = an upright line).",
    ),
    (
        "n08.offset",
        "Moves the fold line from the box's center, as a share of the box.",
    ),
    (
        "n08.fold",
        "Which side is folded over: the left side, the right side, or both meet at the line.",
    ),
    (
        "n09",
        "Pushes the notes along a drawn path, like dragging a brush.",
    ),
    (
        "n09.path",
        "The path's points as x, y pairs, in thousandths of the box.",
    ),
    (
        "n09.radius",
        "How far around the path the notes are pushed, as a share of the box.",
    ),
    (
        "n09.push",
        "How far the notes are pushed along the path's direction (− = backward).",
    ),
    ("n10", "Applies the warp several times, each time smaller."),
    ("n10.iter", "How many times the warp is applied."),
    (
        "n10.scale",
        "Each repetition's strength as a share of the previous one.",
    ),
    (
        "n11",
        "Smears one column of the selection across a width, stretching it like wet paint.",
    ),
    (
        "n11.col",
        "The source column's position in time, as a share of the box.",
    ),
    ("n11.width", "How wide the smear is, as a share of the box."),
    (
        "n11.dir",
        "The smear runs to the right, to the left, or both ways.",
    ),
];

fn warp_spec() -> ToolSpec {
    let mut s = spec(
        "warp",
        "Warp",
        "Pushes the selection with a warp point: everything near it moves along (smooth falloff); advanced modes use a mesh of nodes.",
        vec![
            float("x", "Warp point X", 0.0, 100.0, 50.0, "%"),
            float("y", "Warp point Y", 0.0, 100.0, 50.0, "%"),
            float("dx", "Push X", -100.0, 100.0, 20.0, "%"),
            float("dy", "Push Y", -100.0, 100.0, 20.0, "%"),
            float("radius", "Radius", 5.0, 150.0, 50.0, "%"),
            adv_switch(1, "Mesh rows × columns"),
            int("a1.rows", "Rows", 2, 9, 3).adv(1),
            int("a1.cols", "Columns", 2, 9, 3).adv(1),
            list(
                "a1.nodes",
                "Node offsets ‰ (x, y per node, row by row from the bottom)",
                -1000,
                1000,
                162,
                &[0, 0, 0, 0, 0, 0, 0, 0, 150, 100, 0, 0, 0, 0, 0, 0, 0, 0],
            )
            .adv(1),
            adv_switch(2, "Draggable nodes"),
            list(
                "a2.nodes",
                "Free nodes ‰ (x, y, push x, push y, …)",
                -1000,
                1000,
                64,
                &[250, 250, 100, 0, 750, 750, -100, 0],
            )
            .adv(2),
            adv_switch(3, "Spline interpolation"),
            adv_switch(4, "Rigid pins"),
            list("a4.pins", "Pins ‰ (x, y, …)", 0, 1000, 32, &[500, 500]).adv(4),
            float("a4.radius", "Pin radius", 1.0, 100.0, 20.0, "%").adv(4),
            adv_switch(5, "Edge lock"),
            adv_switch(6, "Wrap boundaries"),
            adv_switch(7, "Symmetry-linked nodes"),
            adv_switch(8, "Brush local warp"),
            float("a8.x", "Brush X", 0.0, 100.0, 30.0, "%").adv(8),
            float("a8.y", "Brush Y", 0.0, 100.0, 30.0, "%").adv(8),
            float("a8.radius", "Brush radius", 1.0, 100.0, 20.0, "%").adv(8),
            float("a8.dx", "Brush push X", -100.0, 100.0, 15.0, "%").adv(8),
            adv_switch(9, "Mesh shape preset"),
            choice("a9.shape", "Shape", MESH_SHAPES, 0).adv(9),
            float("a9.amount", "Amount", 0.0, 100.0, 15.0, "%").adv(9),
            adv_switch(10, "Morph mesh A → B across the selection"),
            list(
                "a10.b",
                "Mesh B node offsets ‰ (3 × 3: x, y per node)",
                -1000,
                1000,
                18,
                &[0, 0, 0, 0, 0, 0, 0, 0, -150, -100, 0, 0, 0, 0, 0, 0, 0, 0],
            )
            .adv(10),
            // V09-S37: functions declared by stable id (TOOL_CATALOG n01 … n11).
            flag("n01", "Mesh smoothing", false),
            int("n01.iter", "Iterations", 1, 64, 4),
            float("n01.amount", "Amount", 0.0, 100.0, 50.0, "%"),
            flag("n02", "Shape fit", false),
            choice("n02.ref", "Reference", FIT_REFS, 0),
            int("n02.num", "Reference number", 0, 65535, 1),
            choice("n02.fit", "Fit", FIT_MODES, 0),
            flag("n03", "Cell permutation", false),
            int("n03.rows", "Rows", 1, 32, 2),
            int("n03.cols", "Columns", 1, 32, 4),
            choice("n03.arrange", "Arrange", CELL_ARRANGE, 0),
            list("n03.order", "Order", 0, 1023, 1024, &[1, 0, 3, 2]),
            flag("n04", "Conformal map", false),
            choice("n04.fn", "Function", CONFORMAL, 0),
            float("n04.strength", "Strength", 0.0, 100.0, 50.0, "%"),
            flag("n05", "Mesh noise", false),
            float("n05.amount", "Amount", 0.0, 100.0, 10.0, "%"),
            int("n05.scale", "Scale", 1, 32, 4),
            flag("n06", "Chord-rigid warp", false),
            ticks("n06.tol", "Chord tolerance", 0, 1 << 40, 30),
            choice("n06.rigid", "Rigid in", RIGID_IN, 1),
            flag("n07", "Polar mesh", false),
            int("n07.rings", "Rings", 1, 16, 2),
            int("n07.spokes", "Spokes", 3, 64, 8),
            list(
                "n07.nodes",
                "Node offsets ‰ (x, y per node, ring by ring from the center)",
                -1000,
                1000,
                2048,
                &POLAR_DEFAULT,
            ),
            float("n07.x", "Center X", 0.0, 100.0, 50.0, "%"),
            float("n07.y", "Center Y", 0.0, 100.0, 50.0, "%"),
            flag("n08", "Line fold", false),
            float("n08.angle", "Angle", -180.0, 180.0, 90.0, "°"),
            float("n08.offset", "Offset", -100.0, 100.0, 0.0, "%"),
            choice("n08.fold", "Fold", FOLDS, 0),
            flag("n09", "Path brush", false),
            list(
                "n09.path",
                "Path ‰ (x, y per point)",
                0,
                1000,
                64,
                &[100, 200, 400, 700, 700, 300, 900, 800],
            ),
            float("n09.radius", "Radius", 1.0, 100.0, 15.0, "%"),
            float("n09.push", "Push", -100.0, 100.0, 20.0, "%"),
            flag("n10", "Iterated warp", false),
            int("n10.iter", "Iterations", 1, 16, 3),
            float("n10.scale", "Scale per iteration", 10.0, 150.0, 70.0, "%"),
            flag("n11", "Smear", false),
            float("n11.col", "Source column", 0.0, 100.0, 50.0, "%"),
            float("n11.width", "Smear width", 0.0, 100.0, 25.0, "%"),
            choice("n11.dir", "Direction", SMEAR_DIRS, 0),
        ],
        [
            "Mesh rows × columns",
            "Draggable nodes",
            "Spline interpolation",
            "Rigid pins",
            "Edge lock",
            "Wrap boundaries",
            "Symmetry-linked nodes",
            "Brush local warp",
            "Mesh shape preset",
            "Morph mesh A → B",
        ],
    );
    s.functions = [
        ("n01", "Mesh smoothing", 110),
        ("n02", "Shape fit", 120),
        ("n03", "Cell permutation", 190),
        ("n04", "Conformal map", 170),
        ("n05", "Mesh noise", 130),
        ("n06", "Chord-rigid warp", 200),
        ("n07", "Polar mesh", 140),
        ("n08", "Line fold", 180),
        ("n09", "Path brush", 150),
        ("n10", "Iterated warp", 160),
        ("n11", "Smear", 210),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, WARP_HELP);
    s
}

/// A displacement mesh: `rows × cols` nodes over the unit box (shared with
/// Custom Shape's Mesh Warp, V09-S57).
#[derive(Clone, Debug)]
pub(crate) struct Mesh {
    rows: usize,
    cols: usize,
    /// (dx, dy) per node, row-major from the bottom.
    d: Vec<(f64, f64)>,
}

impl Mesh {
    pub(crate) fn from_list(rows: usize, cols: usize, v: &[i64]) -> Mesh {
        let p = permille(v);
        let d = (0..rows * cols)
            .map(|i| {
                (
                    p.get(2 * i).copied().unwrap_or(0.0),
                    p.get(2 * i + 1).copied().unwrap_or(0.0),
                )
            })
            .collect();
        Mesh { rows, cols, d }
    }

    fn shape(kind: usize, amount: f64, rows: usize, cols: usize) -> Mesh {
        let mut d = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                let (x, y) = (c as f64 / (cols - 1) as f64, r as f64 / (rows - 1) as f64);
                d.push(match kind {
                    0 => (0.0, amount * (2.0 * PI * x).sin()),
                    1 => {
                        let (dx, dy) = (x - 0.5, y - 0.5);
                        (-dy * amount * 2.0, dx * amount * 2.0)
                    }
                    2 => {
                        let r2 = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
                        let k = amount * (r2 * 4.0 * PI).sin();
                        ((x - 0.5) * k, (y - 0.5) * k)
                    }
                    _ => (amount * (PI * y).sin() * (1.0 - 2.0 * y).signum(), 0.0),
                });
            }
        }
        Mesh { rows, cols, d }
    }

    /// [`Mesh::at`] with the spline, reading only the 4 × 4 nodes around
    /// the point (the same values: Catmull-Rom only reads its neighbours) —
    /// no allocation, O(1) per point (the v0.9 noise mesh, up to 33 × 33).
    fn at_local(&self, u: f64, v: f64) -> (f64, f64) {
        let (rows, cols) = (self.rows, self.cols);
        if rows < 2 || cols < 2 {
            return (0.0, 0.0);
        }
        let around = |x: f64, n: usize| {
            let s = x.clamp(0.0, 1.0) * (n - 1) as f64;
            let i = (s.floor() as usize).min(n - 2);
            let at = |k: isize| (i as isize + k).clamp(0, n as isize - 1) as usize;
            ([at(-1), at(0), at(1), at(2)], s - i as f64)
        };
        let (cs, tu) = around(u, cols);
        let (rs, tv) = around(v, rows);
        #[inline(always)]
        fn c4(p: [f64; 4], t: f64) -> f64 {
            let (p0, p1, p2, p3) = (p[0], p[1], p[2], p[3]);
            0.5 * ((2.0 * p1)
                + (-p0 + p2) * t
                + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t)
        }
        // Both components at once: each of the 4 rows across its 4
        // columns at u, then the 4 row values at v.
        let (mut rx, mut ry) = ([0.0; 4], [0.0; 4]);
        for (ri, &r) in rs.iter().enumerate() {
            let (mut cx, mut cy) = ([0.0; 4], [0.0; 4]);
            for (ci, &c) in cs.iter().enumerate() {
                let p = self.d[r * cols + c];
                cx[ci] = p.0;
                cy[ci] = p.1;
            }
            rx[ri] = c4(cx, tu);
            ry[ri] = c4(cy, tu);
        }
        let out = [c4(rx, tv), c4(ry, tv)];
        (out[0], out[1])
    }

    pub(crate) fn at(&self, u: f64, v: f64, spline: bool) -> (f64, f64) {
        let (rows, cols) = (self.rows, self.cols);
        if rows < 2 || cols < 2 {
            return (0.0, 0.0);
        }
        if spline {
            // Catmull-Rom over columns of each row, then over the rows.
            let col_vals = |r: usize, k: usize| -> Vec<f64> {
                (0..cols)
                    .map(|c| {
                        let p = self.d[r * cols + c];
                        if k == 0 { p.0 } else { p.1 }
                    })
                    .collect()
            };
            let mut out = [0.0; 2];
            for (k, o) in out.iter_mut().enumerate() {
                let per_row: Vec<f64> = (0..rows).map(|r| catmull(&col_vals(r, k), u)).collect();
                *o = catmull(&per_row, v);
            }
            return (out[0], out[1]);
        }
        let x = u.clamp(0.0, 1.0) * (cols - 1) as f64;
        let y = v.clamp(0.0, 1.0) * (rows - 1) as f64;
        let (c0, r0) = (
            (x.floor() as usize).min(cols - 2),
            (y.floor() as usize).min(rows - 2),
        );
        let (fx, fy) = (x - c0 as f64, y - r0 as f64);
        let p = |r: usize, c: usize| self.d[r * cols + c];
        let lerp =
            |a: (f64, f64), b: (f64, f64), t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let bottom = lerp(p(r0, c0), p(r0, c0 + 1), fx);
        let top = lerp(p(r0 + 1, c0), p(r0 + 1, c0 + 1), fx);
        lerp(bottom, top, fy)
    }
}

fn warp_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S38 gate (E0409): Control Sweeps run inside the v0.9 kernel, per
    // note (`definition::kernel_sweeps`).
    if WARP_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return warp9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let (px, py) = (p.float("x") / 100.0, p.float("y") / 100.0);
    let (pdx, pdy) = (p.float("dx") / 100.0, p.float("dy") / 100.0);
    let radius = (p.float("radius") / 100.0).max(1e-3);
    let mesh = p.adv(1).then(|| {
        Mesh::from_list(
            p.int("a1.rows").clamp(2, 9) as usize,
            p.int("a1.cols").clamp(2, 9) as usize,
            &p.list("a1.nodes"),
        )
    });
    let free: Vec<f64> = if p.adv(2) {
        permille(&p.list("a2.nodes"))
    } else {
        Vec::new()
    };
    let spline = p.adv(3);
    let pins = p
        .adv(4)
        .then(|| (permille(&p.list("a4.pins")), p.float("a4.radius") / 100.0));
    let edge_lock = p.adv(5);
    let wrap = p.adv(6);
    let symmetric = p.adv(7);
    let brush = p.adv(8).then(|| {
        (
            p.float("a8.x") / 100.0,
            p.float("a8.y") / 100.0,
            (p.float("a8.radius") / 100.0).max(1e-3),
            p.float("a8.dx") / 100.0,
        )
    });
    let shape = p
        .adv(9)
        .then(|| Mesh::shape(p.choice("a9.shape"), p.float("a9.amount") / 100.0, 5, 5));
    let morph = p.adv(10).then(|| Mesh::from_list(3, 3, &p.list("a10.b")));
    let (cu, _) = g.pivot;
    let field = move |u: f64, v: f64| -> (f64, f64) {
        let mut d = (0.0, 0.0);
        let r = ((u - px).powi(2) + (v - py).powi(2)).sqrt();
        // Spline: a Catmull-Rom radial profile instead of the smoothstep.
        let w = if spline {
            if r >= radius {
                0.0
            } else {
                catmull(&[1.0, 0.95, 0.6, 0.15, 0.0], r / radius).clamp(0.0, 1.0)
            }
        } else {
            1.0 - smoothstep(r / radius)
        };
        d.0 += pdx * w;
        d.1 += pdy * w;
        if let Some(m) = &mesh {
            let (a, b2) = m.at(u, v, spline);
            d.0 += a;
            d.1 += b2;
        }
        for c in free.chunks_exact(4) {
            let rr = ((u - c[0]).powi(2) + (v - c[1]).powi(2)).sqrt();
            let w = 1.0 - smoothstep(rr / 0.35);
            d.0 += c[2] * w;
            d.1 += c[3] * w;
        }
        if let Some((bx, by, br, bdx)) = brush {
            let rr = ((u - bx).powi(2) + (v - by).powi(2)).sqrt();
            d.0 += bdx * (1.0 - smoothstep(rr / br));
        }
        if let Some(m) = &shape {
            let (a, b2) = m.at(u, v, true);
            d.0 += a;
            d.1 += b2;
        }
        if let Some(mb) = &morph {
            // Mesh A = the base push (as a 3 × 3 field) blending into mesh B.
            let (a, b2) = mb.at(u, v, spline);
            let t = u.clamp(0.0, 1.0);
            d.0 = d.0 * (1.0 - t) + a * t;
            d.1 = d.1 * (1.0 - t) + b2 * t;
        }
        d
    };
    let map = move |u: f64, v: f64| -> (f64, f64) {
        let mut d = field(u, v);
        if symmetric {
            // Mirror-linked: the field on the left mirrors the right.
            let m = field(2.0 * cu - u, v);
            d = ((d.0 - m.0) / 2.0, (d.1 + m.1) / 2.0);
        }
        if let Some((pins, pr)) = &pins {
            for c in pins.chunks_exact(2) {
                let rr = ((u - c[0]).powi(2) + (v - c[1]).powi(2)).sqrt();
                let s = smoothstep(rr / pr.max(1e-3));
                d.0 *= s;
                d.1 *= s;
            }
        }
        if edge_lock {
            let e = smoothstep(u.min(1.0 - u).max(0.0) * 4.0)
                * smoothstep(v.min(1.0 - v).max(0.0) * 4.0);
            d.0 *= e;
            d.1 *= e;
        }
        let (mut u2, mut v2) = (u + d.0, v + d.1);
        if wrap {
            u2 = crate::transform::geom::rem1(u2);
            v2 = crate::transform::geom::rem1(v2);
        }
        (u2, v2)
    };
    geom_kernel(g, b.tracks.to_vec(), move |_, u, v| map(u, v))
}

// ------------------------------------------------------------ Warp (v0.9)

const WARP_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const FIT_REFS: &[&str] = &["A track", "A subchannel", "Unselected notes of the track"];
const FIT_MODES: &[&str] = &["Outline per row", "Bounding rows"];
const CELL_ARRANGE: &[&str] = &["Seeded", "Rotate rows", "Rotate columns", "List"];
const CONFORMAL: &[&str] = &["z²", "√z", "1/z", "exp", "log", "sin"];
const RIGID_IN: &[&str] = &["Time", "Time and pitch"];
const FOLDS: &[&str] = &["Left side", "Right side", "Both ways"];
const SMEAR_DIRS: &[&str] = &["Right", "Left", "Both"];
/// n07's default: 2 rings × 8 spokes, the outer ring pushed outward on
/// every second spoke (‰; x, y per node, ring by ring from the centre).
const POLAR_DEFAULT: [i64; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0, 0, 0, 100, 0, 0, -100, 0, 0, 0, 0,
    -100, 0, 0,
];

/// n02 Shape fit's reference notes (in `Source::aux` layer 0): another
/// track (its number in the track list, refused with a sentence when it
/// does not exist), a subchannel of the selected tracks (all their notes on
/// it) or the unselected notes of the selected tracks.
pub fn warp_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !p.flag("n02") || src.stats.is_empty() {
        return Ok(());
    }
    let mut layer: Vec<TNote> = Vec::new();
    let num = p.int("n02.num").max(0);
    let shift = ctx.map_or(0, |c| c.shift);
    match p.choice("n02.ref") {
        0 => {
            let t = usize::try_from(num - 1)
                .ok()
                .filter(|&i| doc.track(i).is_some())
                .ok_or_else(|| {
                    TransformError::Invalid(format!(
                        "Track {num} does not exist. Choose the reference track by its number in the track list."
                    ))
                })?;
            // B2-F16: the reference track as the song shows it.
            super::super::source::placed_query(doc, t, shift, 0, u64::MAX, &mut |r| {
                if layer.len() < limit {
                    layer.push(TNote::from_record(&r, u16::MAX));
                }
            });
        }
        1 => {
            let sub = num as u32;
            for st in &src.tracks {
                scoped_query(doc, st, 0, u64::MAX, &mut |r| {
                    if layer.len() < limit && r.sub.0 == sub {
                        layer.push(TNote::from_record(&r, u16::MAX));
                    }
                });
            }
        }
        _ => {
            let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
            *src = s.with_context(doc);
            layer.extend(src.context.iter().copied());
        }
    }
    src.aux = std::sync::Arc::new(vec![layer]);
    Ok(())
}

/// The v0.9 Warp (V09-S37 step 4): the v0.8 field and map re-read from the
/// same parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions in their catalog order: 110 n01 mesh smoothing, 120
/// n02 shape fit (before the field), 130 n05 mesh noise, 140 n07 polar
/// mesh, 150 n09 path brush (added to the warp), 160 n10 iterated warp,
/// 170 n04 conformal map, 180 n08 line fold, 190 n03 cell permutation,
/// 200 n06 chord-rigid warp, 210 n11 smear.
#[derive(Clone)]
struct Warp9 {
    g: Geom,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    px: f64,
    py: f64,
    pdx: f64,
    pdy: f64,
    radius: f64,
    mesh: Option<Mesh>,
    free: Vec<f64>,
    spline: bool,
    pins: Option<(Vec<f64>, f64)>,
    edge_lock: bool,
    wrap: bool,
    symmetric: bool,
    brush: Option<(f64, f64, f64, f64)>,
    shape: Option<Mesh>,
    morph: Option<Mesh>,
    // ----- v0.9 functions
    /// n02: reference choice is in the source; outline per row (else the
    /// bounding box).
    fit: Option<bool>,
    /// n03: rows, columns, arrangement, list.
    cells: Option<(usize, usize, usize, Vec<i64>)>,
    /// n04: function, strength.
    conformal: Option<(usize, f64)>,
    /// n05: the noise mesh.
    noise: Option<Mesh>,
    /// n06: chord tolerance, rigid in pitch too.
    rigid: Option<(u64, bool)>,
    /// n07: rings, spokes, node offsets (ring-major), centre.
    polar: Option<Polar>,
    /// n08: the fold line's normal and point, fold mode.
    fold: Option<Fold>,
    /// n09: path points, radius, push.
    path: Option<PathBrush>,
    /// n10: iterations, scale per iteration.
    iter: Option<(u32, f64)>,
    /// n11: column (box units), width (box units), direction.
    smear: Option<(f64, f64, usize)>,
    /// n01's iterations and amount and the nodes before smoothing (mesh,
    /// morph, polar), for notes whose smoothing a sweep changes.
    raw: Option<RawNodes>,
    /// n05: the amount the noise mesh was drawn at, and the setting.
    noise_at: f64,
    noise_set: f64,
    /// n08's offset and mode (a swept angle turns the line per note).
    fold_def: Option<(f64, usize)>,
    ctx: Ctx,
}

/// n01's iterations, amount and unsmoothed nodes (mesh, morph, polar).
type RawNodes = (
    u32,
    f64,
    Option<Mesh>,
    Option<Mesh>,
    Option<Vec<(f64, f64)>>,
);

/// n08: the fold line's normal, a point on it, the fold mode.
type Fold = ((f64, f64), (f64, f64), usize);

/// n07: the distance from `centre` to the farthest box corner.
fn polar_far(centre: (f64, f64)) -> f64 {
    [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
        .iter()
        .map(|&(x, y): &(f64, f64)| (x - centre.0).hypot(y - centre.1))
        .fold(1e-9, f64::max)
}

/// n08's line at `degrees`, `off` from the box's centre along its normal.
fn fold_of(degrees: f64, off: f64, mode: usize) -> Fold {
    let a = degrees * PI / 180.0;
    let n = (-a.sin(), a.cos());
    ((n.0, n.1), (0.5 + off * n.0, 0.5 + off * n.1), mode)
}

/// One note's values of the parameters Warp's Control Sweeps drive (V09-S38
/// gate, E0409: per note inside the kernel, so n06's chords and n11's smear
/// see the whole selection).
#[derive(Clone, Copy, Debug)]
struct WV {
    pdx: f64,
    pdy: f64,
    /// n01's amount (its smoothed nodes: [`WCfg`]).
    smooth: f64,
    strength: f64,
    /// n05: the noise mesh's factor (1 at the setting).
    noise: f64,
    /// n07: the polar centre's X.
    pcx: f64,
    fold: Option<Fold>,
    push: f64,
    scale: f64,
    width: f64,
}

/// One note's warp settings: its values and the nodes they select.
struct WCfg<'a> {
    v: WV,
    mesh: Option<&'a Mesh>,
    morph: Option<&'a Mesh>,
    polar: Option<&'a [(f64, f64)]>,
}
/// n09: path points (box units), radius, push.
type PathBrush = (Vec<(f64, f64)>, f64, f64);

/// n07's ring × spoke mesh around a centre (box units).
#[derive(Clone, Debug)]
struct Polar {
    rings: usize,
    spokes: usize,
    d: Vec<(f64, f64)>,
    centre: (f64, f64),
    /// The distance to the farthest box corner.
    far: f64,
}

impl Polar {
    /// The node offset at point (u, v): bilinear in (ring, spoke), zero at
    /// the centre, rings at (i + 1) / rings of the farthest box corner,
    /// spokes wrapping.
    fn at(&self, u: f64, v: f64) -> (f64, f64) {
        self.at_from(u, v, self.centre, self.far)
    }

    /// [`Polar::at`] around another centre (a swept n07 X).
    fn at_from(&self, u: f64, v: f64, centre: (f64, f64), far: f64) -> (f64, f64) {
        self.at_with(&self.d, u, v, centre, far)
    }

    /// [`Polar::at_from`] with other nodes (a swept n01 smoothing).
    fn at_with(
        &self,
        d: &[(f64, f64)],
        u: f64,
        v: f64,
        (cx, cy): (f64, f64),
        far: f64,
    ) -> (f64, f64) {
        let (dx, dy) = (u - cx, v - cy);
        let rho = (dx.hypot(dy) / far * self.rings as f64).clamp(0.0, self.rings as f64);
        let mut th = dy.atan2(dx) / (2.0 * PI);
        if th < 0.0 {
            th += 1.0;
        }
        let s = th * self.spokes as f64;
        let j0 = (s.floor() as usize) % self.spokes;
        let j1 = (j0 + 1) % self.spokes;
        let fs = s - s.floor();
        let node = |ring: usize, j: usize| d[ring * self.spokes + j];
        let ring_at = |ring: usize| {
            let (a, b) = (node(ring, j0), node(ring, j1));
            (a.0 + (b.0 - a.0) * fs, a.1 + (b.1 - a.1) * fs)
        };
        // ρ in 0 … 1: from the centre (0) to ring 0; then between rings.
        if rho <= 1.0 {
            let r0 = ring_at(0);
            (r0.0 * rho, r0.1 * rho)
        } else {
            let i = ((rho.floor() as usize).max(1) - 1).min(self.rings - 1);
            let i1 = (i + 1).min(self.rings - 1);
            let f = (rho - 1.0 - i as f64).clamp(0.0, 1.0);
            let (a, b) = (ring_at(i), ring_at(i1));
            (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
        }
    }
}

/// n01: `iters` passes of `d ← d + amount · (mean of the neighbours − d)`
/// over a node grid (`wrap_cols`: the columns wrap, as a polar mesh's
/// spokes).
fn smooth_nodes(
    d: &mut [(f64, f64)],
    rows: usize,
    cols: usize,
    wrap_cols: bool,
    iters: u32,
    amount: f64,
) {
    if amount == 0.0 || rows * cols == 0 {
        return;
    }
    let mut next = d.to_vec();
    for _ in 0..iters {
        for r in 0..rows {
            for c in 0..cols {
                let mut sum = (0.0, 0.0);
                let mut k = 0.0;
                let mut add = |rr: usize, cc: usize| {
                    let p = d[rr * cols + cc];
                    sum.0 += p.0;
                    sum.1 += p.1;
                    k += 1.0;
                };
                if r > 0 {
                    add(r - 1, c);
                }
                if r + 1 < rows {
                    add(r + 1, c);
                }
                if c > 0 {
                    add(r, c - 1);
                } else if wrap_cols && cols > 2 {
                    add(r, cols - 1);
                }
                if c + 1 < cols {
                    add(r, c + 1);
                } else if wrap_cols && cols > 2 {
                    add(r, 0);
                }
                let p = d[r * cols + c];
                next[r * cols + c] = if k > 0.0 {
                    (
                        p.0 + amount * (sum.0 / k - p.0),
                        p.1 + amount * (sum.1 / k - p.1),
                    )
                } else {
                    p
                };
            }
        }
        d.copy_from_slice(&next);
    }
}

fn warp9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let smooth = p.flag("n01").then(|| {
        (
            p.int("n01.iter").clamp(1, 64) as u32,
            (p.float("n01.amount") / 100.0).clamp(0.0, 1.0),
        )
    });
    let mut mesh = p.adv(1).then(|| {
        Mesh::from_list(
            p.int("a1.rows").clamp(2, 9) as usize,
            p.int("a1.cols").clamp(2, 9) as usize,
            &p.list("a1.nodes"),
        )
    });
    let mut morph = p.adv(10).then(|| Mesh::from_list(3, 3, &p.list("a10.b")));
    let mut polar = p.flag("n07").then(|| {
        let rings = p.int("n07.rings").clamp(1, 16) as usize;
        let spokes = p.int("n07.spokes").clamp(3, 64) as usize;
        let v = permille(&p.list("n07.nodes"));
        let d = (0..rings * spokes)
            .map(|i| {
                (
                    v.get(2 * i).copied().unwrap_or(0.0),
                    v.get(2 * i + 1).copied().unwrap_or(0.0),
                )
            })
            .collect();
        let centre = (p.float("n07.x") / 100.0, p.float("n07.y") / 100.0);
        let far = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
            .iter()
            .map(|&(x, y): &(f64, f64)| (x - centre.0).hypot(y - centre.1))
            .fold(1e-9, f64::max);
        Polar {
            rings,
            spokes,
            d,
            centre,
            far,
        }
    });
    // The unsmoothed nodes, for notes whose smoothing a sweep changes.
    let raw = smooth.map(|(iters, amount)| {
        (
            iters,
            amount,
            mesh.clone(),
            morph.clone(),
            polar.as_ref().map(|m| m.d.clone()),
        )
    });
    if let Some((iters, amount)) = smooth {
        if let Some(m) = &mut mesh {
            smooth_nodes(&mut m.d, m.rows, m.cols, false, iters, amount);
        }
        if let Some(m) = &mut morph {
            smooth_nodes(&mut m.d, m.rows, m.cols, false, iters, amount);
        }
        if let Some(m) = &mut polar {
            smooth_nodes(&mut m.d, m.rings, m.spokes, true, iters, amount);
        }
    }
    let noise_set = (p.float("n05.amount") / 100.0).clamp(0.0, 1.0);
    // Drawn at the setting's amount (1 when only a sweep sets it).
    let noise_at = if noise_set == 0.0
        && b.ctx
            .sweeps
            .as_ref()
            .is_some_and(|s| s.drives("n05.amount"))
    {
        1.0
    } else {
        noise_set
    };
    let noise = p.flag("n05").then(|| {
        let n = p.int("n05.scale").clamp(1, 32) as usize + 1;
        let amount = noise_at;
        let salt = salt_of("warp.n05");
        let d = (0..n * n)
            .map(|k| {
                let mut r = stream(0, salt, &[k as u64]);
                (
                    amount * (2.0 * r.unit() - 1.0),
                    amount * (2.0 * r.unit() - 1.0),
                )
            })
            .collect();
        Mesh {
            rows: n,
            cols: n,
            d,
        }
    });
    let fold_def = p
        .flag("n08")
        .then(|| (p.float("n08.offset") / 100.0, p.choice("n08.fold").min(2)));
    let fold = fold_def.map(|(off, mode)| fold_of(p.float("n08.angle"), off, mode));
    let path = p.flag("n09").then(|| {
        let v = permille(&p.list("n09.path"));
        (
            v.chunks_exact(2).map(|c| (c[0], c[1])).collect(),
            (p.float("n09.radius") / 100.0).max(1e-3),
            p.float("n09.push") / 100.0,
        )
    });
    let k = Warp9 {
        px: p.float("x") / 100.0,
        py: p.float("y") / 100.0,
        pdx: p.float("dx") / 100.0,
        pdy: p.float("dy") / 100.0,
        radius: (p.float("radius") / 100.0).max(1e-3),
        mesh,
        free: if p.adv(2) {
            permille(&p.list("a2.nodes"))
        } else {
            Vec::new()
        },
        spline: p.adv(3),
        pins: p
            .adv(4)
            .then(|| (permille(&p.list("a4.pins")), p.float("a4.radius") / 100.0)),
        edge_lock: p.adv(5),
        wrap: p.adv(6),
        symmetric: p.adv(7),
        brush: p.adv(8).then(|| {
            (
                p.float("a8.x") / 100.0,
                p.float("a8.y") / 100.0,
                (p.float("a8.radius") / 100.0).max(1e-3),
                p.float("a8.dx") / 100.0,
            )
        }),
        shape: p
            .adv(9)
            .then(|| Mesh::shape(p.choice("a9.shape"), p.float("a9.amount") / 100.0, 5, 5)),
        morph,
        fit: p.flag("n02").then(|| p.choice("n02.fit") == 0),
        cells: p.flag("n03").then(|| {
            (
                p.int("n03.rows").clamp(1, 32) as usize,
                p.int("n03.cols").clamp(1, 32) as usize,
                p.choice("n03.arrange").min(3),
                p.list("n03.order"),
            )
        }),
        conformal: p.flag("n04").then(|| {
            (
                p.choice("n04.fn").min(5),
                (p.float("n04.strength") / 100.0).clamp(0.0, 1.0),
            )
        }),
        noise,
        rigid: p
            .flag("n06")
            .then(|| (p.ticks("n06.tol"), p.choice("n06.rigid") == 1)),
        polar,
        fold,
        path,
        iter: p.flag("n10").then(|| {
            (
                p.int("n10.iter").clamp(1, 16) as u32,
                (p.float("n10.scale") / 100.0).clamp(0.1, 1.5),
            )
        }),
        smear: p.flag("n11").then(|| {
            (
                (p.float("n11.col") / 100.0).clamp(0.0, 1.0),
                (p.float("n11.width") / 100.0).clamp(0.0, 1.0),
                p.choice("n11.dir").min(2),
            )
        }),
        raw,
        noise_at,
        noise_set,
        fold_def,
        ctx: b.ctx.clone(),
        g,
        tracks: b.tracks.to_vec(),
    };
    let mut per = k.g.per_note();
    if k.smear.is_some() {
        // At most one smear copy per emitted note.
        per = per.saturating_mul(2);
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

/// The cell-to-cell moves n03 makes, per note: (source cell, target cell).
type CellMap = Vec<usize>;

/// One note's settings for [`Warp9::point`].
#[derive(Clone, Copy, Debug)]
struct WFrame {
    /// n02: the row's time fit (selection span → reference span, box
    /// units), or the box fit (u0, u-scale, v0, v-scale … as an affine).
    fit: Option<[f64; 4]>,
    /// n03: the note's translation (box units).
    cell: (f64, f64),
    /// n06: the chord's rigid motion (box units): replaces the map.
    rigid: Option<(f64, f64)>,
}

impl Warp9 {
    /// The v0.8 field (`warp_build`'s, unchanged).
    #[inline(always)]
    fn field(&self, c: &WCfg, u: f64, v: f64) -> (f64, f64) {
        let mut d = (0.0, 0.0);
        let r = ((u - self.px).powi(2) + (v - self.py).powi(2)).sqrt();
        let w = if self.spline {
            if r >= self.radius {
                0.0
            } else {
                catmull(&[1.0, 0.95, 0.6, 0.15, 0.0], r / self.radius).clamp(0.0, 1.0)
            }
        } else {
            1.0 - smoothstep(r / self.radius)
        };
        d.0 += c.v.pdx * w;
        d.1 += c.v.pdy * w;
        if let Some(m) = c.mesh {
            let (a, b2) = m.at(u, v, self.spline);
            d.0 += a;
            d.1 += b2;
        }
        for c in self.free.chunks_exact(4) {
            let rr = ((u - c[0]).powi(2) + (v - c[1]).powi(2)).sqrt();
            let w = 1.0 - smoothstep(rr / 0.35);
            d.0 += c[2] * w;
            d.1 += c[3] * w;
        }
        if let Some((bx, by, br, bdx)) = self.brush {
            let rr = ((u - bx).powi(2) + (v - by).powi(2)).sqrt();
            d.0 += bdx * (1.0 - smoothstep(rr / br));
        }
        if let Some(m) = &self.shape {
            let (a, b2) = m.at(u, v, true);
            d.0 += a;
            d.1 += b2;
        }
        if let Some(mb) = c.morph {
            let (a, b2) = mb.at(u, v, self.spline);
            let t = u.clamp(0.0, 1.0);
            d.0 = d.0 * (1.0 - t) + a * t;
            d.1 = d.1 * (1.0 - t) + b2 * t;
        }
        d
    }

    /// The v0.8 map (`warp_build`'s, unchanged).
    #[inline(always)]
    fn v08(&self, c: &WCfg, u: f64, v: f64) -> (f64, f64) {
        let mut d = self.field(c, u, v);
        if self.symmetric {
            let m = self.field(c, 2.0 * self.g.pivot.0 - u, v);
            d = ((d.0 - m.0) / 2.0, (d.1 + m.1) / 2.0);
        }
        if let Some((pins, pr)) = &self.pins {
            for c in pins.chunks_exact(2) {
                let rr = ((u - c[0]).powi(2) + (v - c[1]).powi(2)).sqrt();
                let s = smoothstep(rr / pr.max(1e-3));
                d.0 *= s;
                d.1 *= s;
            }
        }
        if self.edge_lock {
            let e = smoothstep(u.min(1.0 - u).max(0.0) * 4.0)
                * smoothstep(v.min(1.0 - v).max(0.0) * 4.0);
            d.0 *= e;
            d.1 *= e;
        }
        let (mut u2, mut v2) = (u + d.0, v + d.1);
        if self.wrap {
            u2 = crate::transform::geom::rem1(u2);
            v2 = crate::transform::geom::rem1(v2);
        }
        (u2, v2)
    }

    /// The warp: v0.8 plus n05 / n07 / n09's displacements at the point.
    #[inline(always)]
    fn warp(&self, c: &WCfg, u: f64, v: f64) -> (f64, f64) {
        let (mut u2, mut v2) = self.v08(c, u, v);
        if let Some(m) = &self.noise {
            let (a, b) = m.at_local(u, v);
            if c.v.noise == 1.0 {
                u2 += a;
                v2 += b;
            } else {
                u2 += a * c.v.noise;
                v2 += b * c.v.noise;
            }
        }
        if let Some(pm) = &self.polar {
            let (a, b) = if c.polar.is_none() && c.v.pcx == pm.centre.0 {
                pm.at(u, v)
            } else {
                // Another centre or other nodes: the same rings and spokes.
                let centre = (c.v.pcx, pm.centre.1);
                let far = polar_far(centre);
                pm.at_with(c.polar.unwrap_or(&pm.d), u, v, centre, far)
            };
            u2 += a;
            v2 += b;
        }
        if let Some((pts, rad, _)) = &self.path
            && c.v.push != 0.0
            && pts.len() >= 2
        {
            let push = &c.v.push;
            // The nearest point on the path; its segment's direction.
            let mut best = (f64::INFINITY, (0.0, 0.0));
            for s in pts.windows(2) {
                let (a, b) = (s[0], s[1]);
                let (ex, ey) = (b.0 - a.0, b.1 - a.1);
                let len2 = ex * ex + ey * ey;
                if len2 <= 0.0 {
                    continue;
                }
                let t = (((u - a.0) * ex + (v - a.1) * ey) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (a.0 + ex * t, a.1 + ey * t);
                let dist = (u - qx).hypot(v - qy);
                if dist < best.0 {
                    let l = len2.sqrt();
                    best = (dist, (ex / l, ey / l));
                }
            }
            if best.0 < *rad {
                let w = push * (1.0 - smoothstep(best.0 / rad));
                u2 += best.1.0 * w;
                v2 += best.1.1 * w;
            }
        }
        (u2, v2)
    }

    /// The continuous chain (n10 iterations of the warp, n04, n08).
    #[inline(always)]
    fn chain(&self, c: &WCfg, u: f64, v: f64) -> (f64, f64) {
        let (mut u2, mut v2) = self.warp(c, u, v);
        if let Some((n, _)) = self.iter {
            let s = c.v.scale;
            let mut f = 1.0;
            for _ in 1..n {
                f *= s;
                let (a, b) = self.warp(c, u2, v2);
                u2 += f * (a - u2);
                v2 += f * (b - v2);
            }
        }
        if let Some((func, _)) = self.conformal
            && c.v.strength != 0.0
        {
            let s = c.v.strength;
            let (a, b) = conformal_map(func, u2, v2);
            u2 += s * (a - u2);
            v2 += s * (b - v2);
        }
        if let Some((n, c, mode)) = c.v.fold {
            let side = (u2 - c.0) * n.0 + (v2 - c.1) * n.1;
            let flip = match mode {
                0 => side > 0.0,
                1 => side < 0.0,
                _ => true,
            };
            if flip {
                u2 -= 2.0 * side * n.0;
                v2 -= 2.0 * side * n.1;
            }
        }
        (u2, v2)
    }

    /// The image of point (u, v) for a note's frame.
    #[inline(always)]
    fn point(&self, cf: &WCfg, fr: &WFrame, u: f64, v: f64) -> (f64, f64) {
        if let Some((du, dv)) = fr.rigid {
            return (u + du, v + dv);
        }
        let (u, v) = match fr.fit {
            Some([a, b, c, d]) => (a + b * u, c + d * v),
            None => (u, v),
        };
        let (u2, v2) = self.chain(cf, u, v);
        (u2 + fr.cell.0, v2 + fr.cell.1)
    }

    /// n03: each cell's target cell.
    fn cell_map(&self) -> Option<CellMap> {
        let (rows, cols, arrange, order) = self.cells.as_ref()?;
        let n = rows * cols;
        Some(match arrange {
            0 => {
                let mut t: Vec<usize> = (0..n).collect();
                let mut r = stream(0, salt_of("warp.n03"), &[n as u64]);
                for i in (1..n).rev() {
                    let j = r.below(i as u64 + 1) as usize;
                    t.swap(i, j);
                }
                t
            }
            1 => (0..n)
                .map(|k| {
                    let (r, c) = (k / cols, k % cols);
                    r * cols + (c + r) % cols
                })
                .collect(),
            2 => (0..n)
                .map(|k| {
                    let (r, c) = (k / cols, k % cols);
                    ((r + c) % rows) * cols + c
                })
                .collect(),
            _ => (0..n)
                .map(|k| {
                    if order.is_empty() {
                        k
                    } else {
                        order[k % order.len()].rem_euclid(n as i64) as usize
                    }
                })
                .collect(),
        })
    }

    /// Every note's swept values: the settings', with the Control Sweeps
    /// applied at each note's place (n01's amount to 1‰: one set of
    /// smoothed nodes per value).
    fn vals(&self, src: &Source) -> (WV, Vec<WV>) {
        let base = WV {
            pdx: self.pdx,
            pdy: self.pdy,
            smooth: self.raw.as_ref().map_or(0.0, |r| r.1),
            strength: self.conformal.map_or(0.0, |c| c.1),
            noise: 1.0,
            pcx: self.polar.as_ref().map_or(0.0, |p| p.centre.0),
            fold: self.fold,
            push: self.path.as_ref().map_or(0.0, |p| p.2),
            scale: self.iter.map_or(1.0, |i| i.1),
            width: self.smear.map_or(0.0, |s| s.1),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let mut d = base;
                d.pdx = pct("dx", base.pdx);
                d.pdy = pct("dy", base.pdy);
                if sw.drives("n01.amount") && self.raw.is_some() {
                    let a = pct("n01.amount", base.smooth).clamp(0.0, 1.0);
                    d.smooth = (a * 1000.0).round() / 1000.0;
                }
                d.strength = pct("n04.strength", base.strength).clamp(0.0, 1.0);
                if sw.drives("n05.amount") && self.noise_at > 0.0 {
                    let a = (sw.float("n05.amount", self.noise_set * 100.0, n, pl) / 100.0)
                        .clamp(0.0, 1.0);
                    d.noise = a / self.noise_at;
                }
                d.pcx = pct("n07.x", base.pcx);
                if let Some((off, mode)) = self.fold_def
                    && sw.drives("n08.angle")
                {
                    let deg = base
                        .fold
                        .map_or(0.0, |f| (-f.0.0).atan2(f.0.1) * 180.0 / PI);
                    d.fold = Some(fold_of(sw.float("n08.angle", deg, n, pl), off, mode));
                }
                d.push = pct("n09.push", base.push);
                d.scale = pct("n10.scale", base.scale).clamp(0.1, 1.5);
                d.width = pct("n11.width", base.width).clamp(0.0, 1.0);
                d
            })
            .collect();
        (base, per)
    }

    /// n01: the smoothed nodes (mesh, morph, polar) of every amount the
    /// notes ask for other than the setting's, sorted by the amount's bits.
    #[allow(clippy::type_complexity)]
    fn variants(
        &self,
        per: &[WV],
    ) -> Vec<(u64, Option<Mesh>, Option<Mesh>, Option<Vec<(f64, f64)>>)> {
        let Some((iters, amount, mesh, morph, polar)) = &self.raw else {
            return Vec::new();
        };
        let mut keys: Vec<u64> = per
            .iter()
            .map(|v| v.smooth.to_bits())
            .filter(|&k| k != amount.to_bits())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys.into_iter()
            .map(|k| {
                let a = f64::from_bits(k);
                let sm = |m: &Option<Mesh>| {
                    m.clone().map(|mut m| {
                        smooth_nodes(&mut m.d, m.rows, m.cols, false, *iters, a);
                        m
                    })
                };
                let po = polar.clone().map(|mut d| {
                    if let Some(pm) = &self.polar {
                        smooth_nodes(&mut d, pm.rings, pm.spokes, true, *iters, a);
                    }
                    d
                });
                (k, sm(mesh), sm(morph), po)
            })
            .collect()
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let b = &self.g.bbox;
        let (w, rows) = (b.width(), b.rows());
        // n02: per row (selection span, reference span) or the boxes.
        let mut row_fit: Vec<Option<[f64; 4]>> = Vec::new();
        let mut box_fit: Option<[f64; 4]> = None;
        if let Some(per_row) = self.fit {
            let refs: &[TNote] = src.aux.first().map_or(&[], |l| l.as_slice());
            let refs = if refs.is_empty() {
                src.context.as_slice()
            } else {
                refs
            };
            if !refs.is_empty() {
                let to_u = |x: u64| (x as f64 - b.t0 as f64) / w;
                if per_row {
                    let mut sel = vec![(u64::MAX, 0u64); 256];
                    let mut rf = vec![(u64::MAX, 0u64); 256];
                    for n in &src.notes {
                        let s = &mut sel[usize::from(n.pitch)];
                        *s = (s.0.min(n.start), s.1.max(n.end()));
                    }
                    for n in refs {
                        let s = &mut rf[usize::from(n.pitch)];
                        *s = (s.0.min(n.start), s.1.max(n.end()));
                    }
                    row_fit = (0..256)
                        .map(|r| {
                            let (s, q) = (sel[r], rf[r]);
                            (s.0 < s.1 && q.0 < q.1).then(|| {
                                let (s0, s1, r0, r1) = (to_u(s.0), to_u(s.1), to_u(q.0), to_u(q.1));
                                let k = (r1 - r0) / (s1 - s0);
                                [r0 - s0 * k, k, 0.0, 1.0]
                            })
                        })
                        .collect();
                } else {
                    let (mut t0, mut t1, mut lo, mut hi) = (u64::MAX, 0u64, u8::MAX, 0u8);
                    for n in refs {
                        t0 = t0.min(n.start);
                        t1 = t1.max(n.end());
                        lo = lo.min(n.pitch);
                        hi = hi.max(n.pitch);
                    }
                    let (u0, u1) = (to_u(t0), to_u(t1.max(t0 + 1)));
                    let v0 = (f64::from(lo) - f64::from(b.p0)) / rows;
                    let v1 = (f64::from(hi) + 1.0 - f64::from(b.p0)) / rows;
                    box_fit = Some([u0, u1 - u0, v0, v1 - v0]);
                }
            }
        }
        let cells = self.cell_map();
        let (wbase, wper) = self.vals(src);
        let variants = self.variants(&wper);
        let cfg_of = |i: usize| -> WCfg {
            let v = wper.get(i).copied().unwrap_or(wbase);
            match variants
                .binary_search_by_key(&v.smooth.to_bits(), |x| x.0)
                .ok()
                .map(|k| &variants[k])
            {
                Some((_, m, mo, po)) => WCfg {
                    v,
                    mesh: m.as_ref(),
                    morph: mo.as_ref(),
                    polar: po.as_deref(),
                },
                None => WCfg {
                    v,
                    mesh: self.mesh.as_ref(),
                    morph: self.morph.as_ref(),
                    polar: None,
                },
            }
        };
        let frame = |i: usize, n: &TNote| -> WFrame {
            let fit = if self.fit == Some(true) {
                row_fit.get(usize::from(n.pitch)).copied().flatten()
            } else {
                box_fit
            };
            let mut fr = WFrame {
                fit,
                cell: (0.0, 0.0),
                rigid: None,
            };
            if let (Some((cr, cc, _, _)), Some(map)) = (&self.cells, &cells) {
                // The note's cell: where its centre lands.
                let (u, v) = b.to_unit(
                    (n.start as f64 + n.end() as f64) / 2.0,
                    f64::from(n.pitch) + 0.5,
                );
                let (u, v) = self.point(&cfg_of(i), &fr, u, v);
                let c = ((u.clamp(0.0, 1.0 - 1e-12) * *cc as f64) as usize).min(cc - 1);
                let r = ((v.clamp(0.0, 1.0 - 1e-12) * *cr as f64) as usize).min(cr - 1);
                let to = map[r * cc + c];
                fr.cell = (
                    ((to % cc) as f64 - c as f64) / *cc as f64,
                    ((to / cc) as f64 - r as f64) / *cr as f64,
                );
            }
            fr
        };
        // n06: every chord's rigid motion (its box centre's).
        let mut rigid: Vec<Option<(f64, f64)>> = Vec::new();
        if let Some((tol, pitch_too)) = self.rigid {
            rigid = vec![None; src.notes.len()];
            for idx in per_track(src) {
                if idx.is_empty() {
                    continue;
                }
                for chord in ChordGroups::of(&src.notes, &idx, tol).iter() {
                    let (mut t0, mut t1, mut lo, mut hi) = (u64::MAX, 0u64, u8::MAX, 0u8);
                    for &i in chord {
                        let n = &src.notes[i];
                        t0 = t0.min(n.start);
                        t1 = t1.max(n.end());
                        lo = lo.min(n.pitch);
                        hi = hi.max(n.pitch);
                    }
                    let (cu, cv) = b.to_unit(
                        (t0 as f64 + t1 as f64) / 2.0,
                        (f64::from(lo) + f64::from(hi) + 1.0) / 2.0,
                    );
                    let first = &src.notes[chord[0]];
                    let (mu, mv) = self.point(&cfg_of(chord[0]), &frame(chord[0], first), cu, cv);
                    for &i in chord {
                        let n = &src.notes[i];
                        let dv = if pitch_too {
                            mv - cv
                        } else {
                            // Time rigid: the note's own pitch motion.
                            let (nu, nv) = b.to_unit(
                                (n.start as f64 + n.end() as f64) / 2.0,
                                f64::from(n.pitch) + 0.5,
                            );
                            self.point(&cfg_of(i), &frame(i, n), nu, nv).1 - nv
                        };
                        rigid[i] = Some((mu - cu, dv));
                    }
                }
            }
        }
        let base = out.notes.len();
        // n11 with a swept width: each result note's source.
        let mut from: Vec<u32> = Vec::new();
        let swept_width = !wper.is_empty() && self.smear.is_some();
        for (i, n) in src.notes.iter().enumerate() {
            let mut fr = frame(i, n);
            if let Some(r) = rigid.get(i).copied().flatten() {
                fr.rigid = Some(r);
            }
            let cf = cfg_of(i);
            let ok = self
                .g
                .rasterize(n, &|u, v| self.point(&cf, &fr, u, v), &self.tracks, out);
            if swept_width {
                from.resize(out.notes.len() - base, i as u32);
            }
            if !ok {
                return out.check();
            }
            out.check()?;
        }
        if let Some((col, width, dir)) = self.smear {
            // n11: every result note sounding at the column gets a copy
            // held across the width from it.
            let x = b.t0 as f64 + col * w;
            let len0 = (width * w).round().max(1.0) as i128;
            let xc = x.round() as i128;
            let end = out.notes.len();
            for k in base..end {
                let len = match from.get(k - base) {
                    Some(&i) => (wper[i as usize].width * w).round().max(1.0) as i128,
                    None => len0,
                };
                let t = out.notes[k];
                let (s, e) = (i128::from(t.start), i128::from(t.end()));
                if !(s <= xc && xc < e) {
                    continue;
                }
                let (a, d) = match dir {
                    0 => (xc, len),
                    1 => (xc - len, len),
                    _ => (xc - len, 2 * len),
                };
                if !out.emit(a, d, i64::from(t.pitch), i64::from(t.vel), &t) {
                    break;
                }
            }
        }
        Ok(())
    }
}

/// n04: point (u, v) of the box through a complex function — z = 2(u − ½) +
/// 2i(v − ½), w = f(z) scaled to stay near the box, back as ½ + w / 2 —
/// or, for log, the log-polar unwrapping (angle across, log radius up).
fn conformal_map(f: usize, u: f64, v: f64) -> (f64, f64) {
    let (x, y) = (2.0 * (u - 0.5), 2.0 * (v - 0.5));
    let (wr, wi) = match f {
        0 => (0.5 * (x * x - y * y), 0.5 * (2.0 * x * y)),
        1 => {
            let r = x.hypot(y).sqrt();
            let a = y.atan2(x) / 2.0;
            (r * a.cos(), r * a.sin())
        }
        2 => {
            let m = (x * x + y * y).max(1e-6);
            (0.25 * x / m, -0.25 * y / m)
        }
        3 => {
            let e = x.exp();
            let k = 1.0 / (std::f64::consts::E - 1.0);
            ((e * y.cos() - 1.0) * k, e * y.sin() * k)
        }
        4 => {
            let r = x.hypot(y).max(1.0 / 64.0);
            let a = y.atan2(x);
            let lo = (1.0f64 / 64.0).ln();
            let hi = 2f64.sqrt().ln();
            return ((a + PI) / (2.0 * PI), (r.ln() - lo) / (hi - lo));
        }
        _ => {
            let (a, b) = (PI / 2.0 * x, PI / 2.0 * y);
            let k = 1.0 / (PI / 2.0).cosh();
            (a.sin() * b.cosh() * k, a.cos() * b.sinh() * k)
        }
    };
    (0.5 + wr / 2.0, 0.5 + wi / 2.0)
}

// ====================================================================== Shear

pub const SHEAR: ToolDef = ToolDef {
    id: "shear",
    spec: shear_spec,
    build: shear_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const SHEAR_HELP: &[(&str, &str)] = &[
    (
        "curve",
        "How far each row shifts in time, by its height in the selection (bottom left, top right): 0.5 = no shift, above later, below earlier.",
    ),
    (
        "amount",
        "The largest shift, as a share of the box's width (where the curve reaches 0 or 1).",
    ),
    (
        "a1",
        "Shifts rows in time by height with a smooth spline through the curve's points.",
    ),
    (
        "a2",
        "Also shifts each column in pitch by its place in time.",
    ),
    (
        "a2.curve",
        "How far each column shifts in pitch, by its time in the selection (0.5 = none).",
    ),
    (
        "a2.amount",
        "The largest pitch shift, as a share of the box's height.",
    ),
    (
        "a3",
        "Shifts rows by a list of values instead of the curve.",
    ),
    (
        "a3.points",
        "The shifts from the bottom row to the top, in thousandths of the box's width (in between: interpolated).",
    ),
    ("a4", "Shapes the shift with a Bézier curve."),
    (
        "a4.handles",
        "The Bézier's start, two handles and end, in thousandths of the box's width.",
    ),
    ("a5", "Mirrors the shift profile around the middle row."),
    (
        "a6",
        "Repeats the shift profile several times from bottom to top.",
    ),
    ("a6.periods", "How many times the profile repeats."),
    ("a7", "Bends the profile with an exponent (tension)."),
    (
        "a7.tension",
        "The exponent: above 1 the shift stays small longer, below 1 it grows quickly.",
    ),
    ("a8", "Gives each subchannel its own shift amount."),
    (
        "a8.table",
        "The amount for each subchannel in turn, in thousandths (1000 = the full amount, −1000 = reversed).",
    ),
    (
        "a9",
        "Morphs from the main curve (A) to a second curve (B) across time.",
    ),
    ("a9.curve", "Curve B, reached by the selection's end."),
    ("a10", "Shifts in steps instead of smoothly."),
    ("a10.steps", "How many steps the profile is divided into."),
    (
        "n01",
        "Shifts the rows to follow a contour: the selection's top or bottom edge, or a track's.",
    ),
    (
        "n01.of",
        "The contour followed: the selection's top, its bottom, or a track's notes.",
    ),
    ("n01.track", "For A Track: the track's number."),
    (
        "n01.smooth",
        "How many columns the contour is smoothed over.",
    ),
    (
        "n01.scale",
        "How strongly the shift follows the contour (− reverses it).",
    ),
    ("n02", "Changes note lengths with the shear's slope."),
    (
        "n02.mode",
        "Keep the lengths, stretch them, or shrink them where the shear slopes.",
    ),
    ("n02.amount", "How strongly the slope changes the lengths."),
    ("n03", "Adds a seeded noise profile to the shift."),
    (
        "n03.rough",
        "How rough the noise is (higher = finer, jaggier variation).",
    ),
    (
        "n03.amp",
        "How large the noise shifts are, as a share of the amount.",
    ),
    (
        "n04",
        "Shifts rows below an axis key with their own profile.",
    ),
    (
        "n04.axis",
        "The key below which the second profile applies.",
    ),
    ("n04.curve", "The profile below the axis (0.5 = no shift)."),
    (
        "n04.amount",
        "The largest shift below the axis, as a share of the box's width.",
    ),
    (
        "n05",
        "Shapes the shear by its slope instead of its position: the curve says how steeply rows shift.",
    ),
    ("n05.curve", "The slope by height (0.5 = no slope)."),
    ("n05.scale", "How strong the slope is."),
    (
        "n06",
        "Shifts the rows by the song's tempo changes across the selection.",
    ),
    ("n06.scale", "How strongly the tempo shifts the rows."),
    ("n06.invert", "Reverses the direction of the tempo shift."),
    ("n07", "Shears only a band of keys; the rest stays put."),
    ("n07.lo", "The band's lowest key."),
    ("n07.hi", "The band's highest key."),
    (
        "n07.feather",
        "How many keys the shear fades over at the band's edges.",
    ),
    (
        "n08",
        "Pulses the shift at each onset of a reference, decaying after it.",
    ),
    (
        "n08.ref",
        "Where the onsets come from: the track's unselected notes, a subchannel, or a track.",
    ),
    (
        "n08.num",
        "The reference's number (a subchannel or track number).",
    ),
    (
        "n08.pulse",
        "The pulse's shape over its decay (1 = full, 0 = none).",
    ),
    ("n08.amp", "How many keys a pulse shifts the notes."),
    ("n08.decay", "How long, in ticks, a pulse lasts."),
    (
        "n09",
        "Adds copies at stepped strengths of the shear, back toward the original.",
    ),
    ("n09.copies", "How many copies."),
    (
        "n09.vel",
        "Each copy's velocity change from the one before.",
    ),
    ("n10", "Applies the shear separately to each phrase."),
    (
        "n10.rest",
        "A rest at least this long, in ticks, starts a new phrase.",
    ),
    (
        "n10.stretch",
        "Stretch the profile over each phrase, or repeat it with a fixed period.",
    ),
    (
        "n10.period",
        "For A Fixed Period: the profile's length, in ticks.",
    ),
    ("n11", "Colors the notes by how far they moved."),
    ("n11.bands", "How many distance bands."),
    (
        "n11.palette",
        "The color of each band, from least moved to most (places in the track's list).",
    ),
];

fn shear_spec() -> ToolSpec {
    let mut s = spec(
        "shear",
        "Shear",
        "Shifts each row of the selection in time by a multi-point curve of the row's height (not a single angle): the curve's points can be edited freely.",
        vec![
            curve(
                "curve",
                "Shift by height (0.5 = none)",
                Curve::new(vec![(0.0, 0.5), (0.3, 0.8), (0.7, 0.2), (1.0, 0.5)]),
            )
            .overshoot(),
            float(
                "amount",
                "Amount (of the box width)",
                -200.0,
                200.0,
                40.0,
                "%",
            ),
            adv_switch(1, "X-by-Y spline"),
            adv_switch(2, "Y-by-X spline"),
            curve(
                "a2.curve",
                "Pitch shift by time (0.5 = none)",
                Curve::line(0.3, 0.7),
            )
            .overshoot()
            .adv(2),
            float(
                "a2.amount",
                "Amount (of the box height)",
                -200.0,
                200.0,
                40.0,
                "%",
            )
            .adv(2),
            adv_switch(3, "Multi-point list"),
            list(
                "a3.points",
                "Shifts ‰ from bottom to top",
                -1000,
                1000,
                64,
                &[0, 200, -200, 100, 0],
            )
            .adv(3),
            adv_switch(4, "Bezier handles"),
            list(
                "a4.handles",
                "Bezier ‰ (start, handle 1, handle 2, end)",
                -1000,
                1000,
                4,
                &[0, 400, -400, 0],
            )
            .adv(4),
            adv_switch(5, "Mirrored"),
            adv_switch(6, "Repeat periods"),
            int("a6.periods", "Periods", 2, 16, 3).adv(6),
            adv_switch(7, "Tension"),
            float("a7.tension", "Tension (exponent)", 0.2, 5.0, 2.0, "").adv(7),
            adv_switch(8, "Subchannel-specific amounts"),
            list(
                "a8.table",
                "Amount ‰ by subchannel position",
                -2000,
                2000,
                4096,
                &[1000, -1000],
            )
            .adv(8),
            adv_switch(9, "Morph A → B over time"),
            curve("a9.curve", "Curve B", Curve::line(0.8, 0.2))
                .overshoot()
                .adv(9),
            adv_switch(10, "Step nodes"),
            int("a10.steps", "Steps", 2, 64, 4).adv(10),
            // V09-S37: functions declared by stable id (TOOL_CATALOG n01 … n11).
            flag("n01", "Contour follow", false),
            choice("n01.of", "Contour of", CONTOUR_OF, 0),
            int("n01.track", "Track", 1, 65535, 1),
            int("n01.smooth", "Smoothing (columns)", 0, 64, 8),
            float("n01.scale", "Scale", -400.0, 400.0, 100.0, "%"),
            flag("n02", "Slope lengths", false),
            choice("n02.mode", "Lengths", SLOPE_LENGTHS, 1),
            float("n02.amount", "Amount", 0.0, 200.0, 100.0, "%"),
            flag("n03", "Noise profile", false),
            float("n03.rough", "Roughness", 0.0, 100.0, 30.0, "%"),
            float("n03.amp", "Amplitude", 0.0, 400.0, 50.0, "%"),
            flag("n04", "Split axis", false),
            pitch("n04.axis", "Axis", 60),
            curve(
                "n04.curve",
                "Profile below (0.5 = none)",
                Curve::new(vec![(0.0, 0.5), (0.3, 0.2), (0.7, 0.8), (1.0, 0.5)]),
            )
            .overshoot(),
            float("n04.amount", "Amount below", -400.0, 400.0, -40.0, "%"),
            flag("n05", "Slope profile", false),
            curve("n05.curve", "Slope (0.5 = none)", Curve::line(0.2, 0.8)).overshoot(),
            float("n05.scale", "Scale", -400.0, 400.0, 100.0, "%"),
            flag("n06", "Tempo profile", false),
            float("n06.scale", "Scale", -400.0, 400.0, 100.0, "%"),
            choice("n06.invert", "Invert", NO_YES, 0),
            flag("n07", "Band limit", false),
            pitch("n07.lo", "Low", 48),
            pitch("n07.hi", "High", 72),
            int("n07.feather", "Feather (rows)", 0, 48, 4),
            flag("n08", "Onset pulse", false),
            choice("n08.ref", "Reference", PULSE_REFS, 0),
            int("n08.num", "Reference number", 0, 65535, 1),
            curve("n08.pulse", "Pulse", Curve::line(1.0, 0.0)).overshoot(),
            int("n08.amp", "Amplitude (rows)", -48, 48, 4),
            ticks("n08.decay", "Decay", 1, 1 << 40, 480),
            flag("n09", "Strength copies", false),
            int("n09.copies", "Copies", 1, 16, 3),
            float("n09.vel", "Velocity step", -50.0, 50.0, -15.0, "%"),
            flag("n10", "Phrase-local profile", false),
            ticks("n10.rest", "Phrase split rest", 0, 1 << 40, 480),
            choice("n10.stretch", "Stretch", PHRASE_STRETCH, 0),
            ticks("n10.period", "Period", 1, 1 << 40, 3840),
            flag("n11", "Displacement colors", false),
            int("n11.bands", "Bands", 2, 64, 4),
            list("n11.palette", "Palette", 0, 4095, 64, &[0, 1, 2, 3]),
        ],
        [
            "X-by-Y spline",
            "Y-by-X spline",
            "Multi-point list",
            "Bezier handles",
            "Mirrored",
            "Repeat periods",
            "Tension",
            "Subchannel-specific curves",
            "Morph A → B",
            "Step nodes",
        ],
    );
    s.functions = [
        ("n01", "Contour follow", 150),
        ("n02", "Slope lengths", 190),
        ("n03", "Noise profile", 130),
        ("n04", "Split axis", 120),
        ("n05", "Slope profile", 140),
        ("n06", "Tempo profile", 160),
        ("n07", "Band limit", 180),
        ("n08", "Onset pulse", 170),
        ("n09", "Strength copies", 600),
        ("n10", "Phrase-local profile", 110),
        ("n11", "Displacement colors", 520),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, SHEAR_HELP);
    s
}

fn shear_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S38 gate (E0409): Control Sweeps run inside the v0.9 kernel, per
    // note (`definition::kernel_sweeps`).
    if SHEAR_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return shear9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let mut c = p.curve("curve");
    if p.adv(1) {
        c.smoothness = 1.0;
    }
    let amount = p.float("amount") / 100.0;
    let yx = p
        .adv(2)
        .then(|| (p.curve("a2.curve"), p.float("a2.amount") / 100.0));
    let pts = p.adv(3).then(|| permille(&p.list("a3.points")));
    let bez = p.adv(4).then(|| {
        let v = permille(&p.list("a4.handles"));
        let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
        [c(0), c(1), c(2), c(3)]
    });
    let mirrored = p.adv(5);
    let periods = p.adv(6).then(|| p.int("a6.periods").max(2) as f64);
    let tension = p.adv(7).then(|| p.float("a7.tension"));
    let table = if p.adv(8) {
        permille(&p.list("a8.table"))
    } else {
        Vec::new()
    };
    let morph = p.adv(9).then(|| p.curve("a9.curve"));
    let steps = p.adv(10).then(|| p.int("a10.steps").max(2) as f64);
    let tracks = b.tracks.to_vec();
    let (_, pv) = g.pivot;
    let tr2 = tracks.clone();
    let map = move |n: &TNote, u: f64, v: f64| -> (f64, f64) {
        let mut x = v.clamp(0.0, 1.0);
        if mirrored {
            x = (2.0 * (x - pv)).abs().min(1.0);
        }
        if let Some(k) = periods {
            x = (x * k).fract();
        }
        // Shift in [-0.5, 0.5] × amount.
        let mut s = match (&pts, bez) {
            (Some(pts), _) => linear(pts, x),
            (None, Some(b)) => {
                let t = x;
                let mt = 1.0 - t;
                mt * mt * mt * b[0]
                    + 3.0 * mt * mt * t * b[1]
                    + 3.0 * mt * t * t * b[2]
                    + t * t * t * b[3]
            }
            _ => c.eval(x) - 0.5,
        };
        if let Some(cb) = &morph {
            let t = u.clamp(0.0, 1.0);
            s = s * (1.0 - t) + (cb.eval(x) - 0.5) * t;
        }
        if let Some(t) = tension {
            s = s.signum() * (s.abs() * 2.0).powf(t) / 2.0;
        }
        if let Some(k) = steps {
            s = (s * k).round() / k;
        }
        let mut a = amount;
        if !table.is_empty() {
            a *= table[sub_pos(&tr2, n) % table.len()];
        }
        let mut v2 = v;
        if let Some((cy, ay)) = &yx {
            v2 += (cy.eval(u.clamp(0.0, 1.0)) - 0.5) * ay;
        }
        (u + s * a, v2)
    };
    geom_kernel(g, tracks, map)
}

// ----------------------------------------------------------- Shear (v0.9)

const SHEAR_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const CONTOUR_OF: &[&str] = &["The selection's top", "The selection's bottom", "A track"];
const SLOPE_LENGTHS: &[&str] = &["Keep", "Stretch with the slope", "Shrink with the slope"];
const NO_YES: &[&str] = &["No", "Yes"];
const PULSE_REFS: &[&str] = &["Unselected notes of the track", "A subchannel", "A track"];
const PHRASE_STRETCH: &[&str] = &["To the phrase", "A fixed period"];
/// Columns of the box n01's contour and n06's tempo are sampled at (each
/// column's value belongs to its centre; linear in between).
const SHEAR_COLS: usize = 1024;
/// Steps of n05's slope integral (trapezoids; linear in between).
const SLOPE_STEPS: usize = 1024;

/// n01's contour track (`Source::aux` layer 0) and n08's reference notes
/// (layer 1): another track by its number in the track list (refused with
/// a sentence when it does not exist), every note of the selected tracks on
/// a subchannel, or the selected tracks' unselected notes (into
/// `Source::context`, from the decay before the selection on).
pub fn shear_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    let contour = p.flag("n01") && p.choice("n01.of") == 2;
    let pulse = p.flag("n08");
    if !(contour || pulse) || src.stats.is_empty() {
        return Ok(());
    }
    let (a, b) = (src.stats.min_start, src.stats.max_end);
    let track = |num: i64| {
        usize::try_from(num - 1)
            .ok()
            .filter(|&i| doc.track(i).is_some())
            .ok_or_else(|| {
                TransformError::Invalid(format!(
                    "Track {num} does not exist. Choose the reference track by its number in the track list."
                ))
            })
    };
    // B2-F16: a reference track as the song shows it, on these ticks.
    let shift = ctx.map_or(0, |c| c.shift);
    let take = |t: usize, from: u64, layer: &mut Vec<TNote>| {
        super::super::source::placed_query(doc, t, shift, from, b, &mut |r| {
            if layer.len() < limit {
                layer.push(TNote::from_record(&r, u16::MAX));
            }
        });
    };
    let mut layers = vec![Vec::new(), Vec::new()];
    if contour {
        take(track(p.int("n01.track"))?, a, &mut layers[0]);
    }
    if pulse {
        let from = a.saturating_sub(p.ticks("n08.decay"));
        let num = p.int("n08.num").max(0);
        match p.choice("n08.ref") {
            0 => {
                let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
                *src = s.with_context_between(doc, from, b);
            }
            1 => {
                let sub = num as u32;
                let layer = &mut layers[1];
                for st in &src.tracks {
                    scoped_query(doc, st, from, b, &mut |r| {
                        if layer.len() < limit && r.sub.0 == sub {
                            layer.push(TNote::from_record(&r, u16::MAX));
                        }
                    });
                }
            }
            _ => take(track(num)?, from, &mut layers[1]),
        }
    }
    src.aux = std::sync::Arc::new(layers);
    Ok(())
}

/// The v0.9 Shear (V09-S37 step 5): the v0.8 profile re-read from the same
/// parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions in their catalog order: 110 n10 phrase-local profile
/// (each unit's own box), 120 n04 split axis, 130 n03 noise profile, 140
/// n05 slope profile (the profile), 150 n01 contour follow, 160 n06 tempo
/// profile, 170 n08 onset pulse (pitch shifts by time), 180 n07 band limit,
/// 190 n02 slope lengths, 520 n11 displacement colours, 600 n09 strength
/// copies.
#[derive(Clone)]
struct Shear9 {
    g: Geom,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    curve: Curve,
    amount: f64,
    yx: Option<(Curve, f64)>,
    pts: Option<Vec<f64>>,
    bez: Option<[f64; 4]>,
    mirrored: bool,
    periods: Option<f64>,
    tension: Option<f64>,
    table: Vec<f64>,
    morph: Option<Curve>,
    steps: Option<f64>,
    // ----- v0.9 functions
    /// n01: contour of (0 top, 1 bottom, 2 a track), smoothing radius
    /// (columns), scale.
    contour: Option<(usize, usize, f64)>,
    /// n02: +1 stretch / −1 shrink, amount.
    slope_len: Option<(f64, f64)>,
    /// n03: noise nodes (−½ … ½ over the profile's input), amplitude.
    noise: Option<(Vec<f64>, f64)>,
    /// n04: axis row, profile below, amount below.
    split: Option<(f64, Curve, f64)>,
    /// n05: ∫ 2 (slope − ½) at SLOPE_STEPS + 1 points, scale.
    slope: Option<(Vec<f64>, f64)>,
    /// n06: the shift per column (box heights): ± scale · (BPM / mean − 1),
    /// and the scale it was built at.
    tempo: Option<(Vec<f64>, f64, f64)>,
    /// n07: low row, high row, feather rows.
    band: Option<(f64, f64, f64)>,
    /// n07's rows as set (a sweep moves the first).
    band_raw: (i64, i64),
    ctx: Ctx,
    /// n08: reference choice, pulse, amplitude (rows), decay (ticks).
    pulse: Option<(usize, Curve, f64, f64)>,
    /// n09: copies, velocity step.
    copies: Option<(u32, f64)>,
    /// n10: split rest, fixed period (`None`: to the phrase).
    phrase: Option<(u64, Option<u64>)>,
    /// n11: bands, palette.
    colours: Option<(u32, Vec<i64>)>,
}

/// What one run derives from its source before mapping notes.
#[derive(Default)]
struct ShearRun {
    /// n01: contour − its mean per column (rows).
    contour: Vec<f64>,
    /// n08: the reference onsets, ascending.
    onsets: Vec<u64>,
}

/// One note's values of the parameters Shear's Control Sweeps drive (V09-S38
/// gate, E0409: per note inside the kernel, so n01's contour, n10's phrases
/// and n11's largest move see the whole selection).
#[derive(Clone, Copy, Debug)]
struct ShVals {
    amount: f64,
    cscale: f64,
    slen: f64,
    namp: f64,
    axis: f64,
    kscale: f64,
    tscale: f64,
    lo: f64,
    hi: f64,
    pamp: f64,
    copies: u32,
}

/// One note's settings for [`Shear9::point`].
#[derive(Clone, Copy, Debug)]
struct SFrame {
    /// The note's swept values.
    w: ShVals,
    /// n10: the unit's start tick, width, lowest row, rows.
    local: Option<(f64, f64, f64, f64)>,
    /// The note's start and end (box units).
    us: f64,
    ue: f64,
    /// n02: the end's move (box widths).
    dl: f64,
    /// n08: the onsets up to the note's start (where the search for its
    /// points' latest onset starts).
    onset: usize,
}

/// n10's unit boxes: each note's unit, each unit's (start tick, width,
/// lowest row, rows).
type Units = (Vec<u32>, Vec<(f64, f64, f64, f64)>);
/// n09: the full map's values by their exact point (bits of u, v).
type Memo = std::cell::RefCell<Vec<(u64, u64, (f64, f64))>>;

/// How far [`Shear9::latest`] walks from a note's onset index before it
/// searches.
const ONSET_WALK: usize = 8;

/// Per-column values `c` (column i's at its centre, (i + ½) / len of the
/// box) at `u`: linear in between, the end columns' values beyond.
#[inline(always)]
fn col_at(c: &[f64], u: f64) -> f64 {
    match c.len() {
        0 => 0.0,
        1 => c[0],
        n => {
            let x = (u * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
            let i = (x.floor() as usize).min(n - 2);
            let t = x - i as f64;
            c[i] + (c[i + 1] - c[i]) * t
        }
    }
}

fn shear9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    // A function whose setting does nothing is skipped — unless a Control
    // Sweep drives that setting (its value then varies per note).
    let swept = |k: &str| b.ctx.sweeps.as_ref().is_some_and(|s| s.drives(k));
    let mut c = p.curve("curve");
    if p.adv(1) {
        c.smoothness = 1.0;
    }
    let bx = g.bbox;
    let tempo = p
        .flag("n06")
        .then(|| {
            let scale = p.float("n06.scale") / 100.0;
            let sign = if p.choice("n06.invert") == 1 {
                -1.0
            } else {
                1.0
            };
            let tt = super::super::shared::tempo_time::TempoTime::new(b.ctx);
            let w = u128::from(bx.t1 - bx.t0).max(1);
            let k = SHEAR_COLS as u128;
            let bpm: Vec<f64> = (0..k)
                .map(|i| tt.bpm_at(bx.t0.saturating_add(((2 * i + 1) * w / (2 * k)) as u64)))
                .collect();
            let (lo, hi) = bpm
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, z), &x| {
                    (a.min(x), z.max(x))
                });
            if (scale == 0.0 && !swept("n06.scale")) || lo == hi || !lo.is_finite() || lo <= 0.0 {
                // One tempo over the box (or no scale): nothing moves.
                return None;
            }
            // The table at the setting's scale (1 when only a sweep scales).
            let set = scale;
            let scale = if scale == 0.0 { 1.0 } else { scale };
            let mean = bpm.iter().sum::<f64>() / SHEAR_COLS as f64;
            Some((
                bpm.iter()
                    .map(|&x| sign * scale * (x / mean - 1.0))
                    .collect(),
                scale,
                set,
            ))
        })
        .flatten();
    let noise = p
        .flag("n03")
        .then(|| {
            let amp = p.float("n03.amp") / 100.0;
            let count =
                2 + (p.float("n03.rough").clamp(0.0, 100.0) / 100.0 * 62.0).round() as usize;
            let salt = salt_of("shear.n03");
            let nodes = (0..count)
                .map(|k| stream(0, salt, &[k as u64]).unit() - 0.5)
                .collect();
            (amp != 0.0 || swept("n03.amp")).then_some((nodes, amp))
        })
        .flatten();
    let slope = p
        .flag("n05")
        .then(|| {
            let scale = p.float("n05.scale") / 100.0;
            let cs = p.curve("n05.curve");
            let f = |j: usize| 2.0 * (cs.eval(j as f64 / SLOPE_STEPS as f64) - 0.5);
            let h = 1.0 / SLOPE_STEPS as f64;
            let mut tab = Vec::with_capacity(SLOPE_STEPS + 1);
            let mut acc = 0.0;
            tab.push(acc);
            for j in 1..=SLOPE_STEPS {
                acc += (f(j - 1) + f(j)) * 0.5 * h;
                tab.push(acc);
            }
            (scale != 0.0 || swept("n05.scale")).then_some((tab, scale))
        })
        .flatten();
    let k = Shear9 {
        curve: c,
        amount: p.float("amount") / 100.0,
        yx: p
            .adv(2)
            .then(|| (p.curve("a2.curve"), p.float("a2.amount") / 100.0)),
        pts: p.adv(3).then(|| permille(&p.list("a3.points"))),
        bez: p.adv(4).then(|| {
            let v = permille(&p.list("a4.handles"));
            let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
            [c(0), c(1), c(2), c(3)]
        }),
        mirrored: p.adv(5),
        periods: p.adv(6).then(|| p.int("a6.periods").max(2) as f64),
        tension: p.adv(7).then(|| p.float("a7.tension")),
        table: if p.adv(8) {
            permille(&p.list("a8.table"))
        } else {
            Vec::new()
        },
        morph: p.adv(9).then(|| p.curve("a9.curve")),
        steps: p.adv(10).then(|| p.int("a10.steps").max(2) as f64),
        contour: p
            .flag("n01")
            .then(|| {
                let scale = p.float("n01.scale") / 100.0;
                (scale != 0.0 || swept("n01.scale")).then(|| {
                    (
                        p.choice("n01.of").min(2),
                        p.int("n01.smooth").clamp(0, 64) as usize,
                        scale,
                    )
                })
            })
            .flatten(),
        slope_len: p
            .flag("n02")
            .then(|| {
                let amount = p.float("n02.amount") / 100.0;
                let sign = match p.choice("n02.mode") {
                    1 => 1.0,
                    2 => -1.0,
                    _ => 0.0,
                };
                (sign != 0.0 && (amount != 0.0 || swept("n02.amount"))).then_some((sign, amount))
            })
            .flatten(),
        noise,
        split: p.flag("n04").then(|| {
            (
                p.int("n04.axis") as f64,
                p.curve("n04.curve"),
                p.float("n04.amount") / 100.0,
            )
        }),
        slope,
        tempo,
        band: p.flag("n07").then(|| {
            let (a, z) = (p.int("n07.lo"), p.int("n07.hi"));
            (
                a.min(z) as f64,
                a.max(z) as f64,
                p.int("n07.feather").clamp(0, 48) as f64,
            )
        }),
        band_raw: (p.int("n07.lo"), p.int("n07.hi")),
        ctx: b.ctx.clone(),
        pulse: p
            .flag("n08")
            .then(|| {
                let amp = p.int("n08.amp").clamp(-48, 48) as f64;
                (amp != 0.0 || swept("n08.amp")).then(|| {
                    (
                        p.choice("n08.ref").min(2),
                        p.curve("n08.pulse"),
                        amp,
                        p.ticks("n08.decay").max(1) as f64,
                    )
                })
            })
            .flatten(),
        copies: p.flag("n09").then(|| {
            (
                p.int("n09.copies").clamp(1, 16) as u32,
                p.float("n09.vel").clamp(-50.0, 50.0) / 100.0,
            )
        }),
        phrase: p.flag("n10").then(|| {
            (
                p.ticks("n10.rest"),
                (p.choice("n10.stretch") == 1).then(|| p.ticks("n10.period").max(1)),
            )
        }),
        colours: p.flag("n11").then(|| {
            let mut pal = p.list("n11.palette");
            if pal.is_empty() {
                pal.push(0);
            }
            (p.int("n11.bands").clamp(2, 64) as u32, pal)
        }),
        g,
        tracks: b.tracks.to_vec(),
    };
    let mut per = k.g.per_note();
    if let Some((n, _)) = k.copies {
        // A swept count reaches the parameter's largest.
        let n = if swept("n09.copies") { 16 } else { n };
        per = per.saturating_mul(u64::from(n) + 1);
    }
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

impl Shear9 {
    /// The v0.8 map (`shear_build`'s, unchanged) with the profile's v0.9
    /// parts: n04 (rows below the axis: the second profile and amount), n03
    /// (seeded noise added), n05 (the slope integral added). `row` is the
    /// point's absolute row (the axis test).
    #[inline(always)]
    fn v08(&self, w: &ShVals, n: &TNote, u: f64, v: f64, row: f64) -> (f64, f64) {
        let (_, pv) = self.g.pivot;
        let mut x = v.clamp(0.0, 1.0);
        if self.mirrored {
            x = (2.0 * (x - pv)).abs().min(1.0);
        }
        if let Some(k) = self.periods {
            x = (x * k).fract();
        }
        let below = self.split.as_ref().filter(|_| row < w.axis);
        // Shift in [-0.5, 0.5] × amount.
        let mut s = match below {
            Some((_, cb, _)) => cb.eval(x) - 0.5,
            None => match (&self.pts, self.bez) {
                (Some(pts), _) => linear(pts, x),
                (None, Some(b)) => {
                    let t = x;
                    let mt = 1.0 - t;
                    mt * mt * mt * b[0]
                        + 3.0 * mt * mt * t * b[1]
                        + 3.0 * mt * t * t * b[2]
                        + t * t * t * b[3]
                }
                _ => self.curve.eval(x) - 0.5,
            },
        };
        if let Some((nodes, _)) = &self.noise {
            s += w.namp * catmull(nodes, x);
        }
        if let Some((tab, _)) = &self.slope {
            s += w.kscale * linear(tab, x);
        }
        if let Some(cb) = &self.morph {
            let t = u.clamp(0.0, 1.0);
            s = s * (1.0 - t) + (cb.eval(x) - 0.5) * t;
        }
        if let Some(t) = self.tension {
            s = s.signum() * (s.abs() * 2.0).powf(t) / 2.0;
        }
        if let Some(k) = self.steps {
            s = (s * k).round() / k;
        }
        let mut a = below.map_or(w.amount, |b| b.2);
        if !self.table.is_empty() {
            a *= self.table[sub_pos(&self.tracks, n) % self.table.len()];
        }
        let mut v2 = v;
        if let Some((cy, ay)) = &self.yx {
            v2 += (cy.eval(u.clamp(0.0, 1.0)) - 0.5) * ay;
        }
        (u + s * a, v2)
    }

    /// The image of point (u, v) (box units) for a note's frame.
    #[inline(always)]
    fn point(&self, fr: &SFrame, run: &ShearRun, n: &TNote, u: f64, v: f64) -> (f64, f64) {
        let b = &self.g.bbox;
        let rows = b.rows();
        let row = f64::from(b.p0) + v * rows;
        let (mut u2, mut v2) = match fr.local {
            None => self.v08(&fr.w, n, u, v, row),
            Some((ut0, uw, up0, ur)) => {
                // n10: the profile over the unit's own box.
                let (t0, w) = (b.t0 as f64, b.width());
                let (a, c) = self.v08(&fr.w, n, (t0 + u * w - ut0) / uw, (row - up0) / ur, row);
                (
                    (ut0 + a * uw - t0) / w,
                    (up0 + c * ur - f64::from(b.p0)) / rows,
                )
            }
        };
        // Pitch shifts by time: n01, n06, n08.
        if self.contour.is_some() && !run.contour.is_empty() {
            v2 += fr.w.cscale * col_at(&run.contour, u) / rows;
        }
        if let Some((t, at, _)) = &self.tempo {
            if fr.w.tscale == *at {
                v2 += col_at(t, u);
            } else {
                v2 += col_at(t, u) * (fr.w.tscale / at);
            }
        }
        if let Some((_, pulse, _, decay)) = &self.pulse
            && !run.onsets.is_empty()
        {
            let amp = &fr.w.pamp;
            let x = b.t0 as f64 + u * b.width();
            let i = Self::latest(&run.onsets, fr.onset, x);
            if i > 0 {
                let dt = x - run.onsets[i - 1] as f64;
                if dt < *decay {
                    v2 += amp * pulse.eval(dt / decay) / rows;
                }
            }
        }
        // n07: the displacement fades outside the band.
        if let Some((_, _, feather)) = self.band {
            let (lo, hi) = (fr.w.lo, fr.w.hi);
            let r = row.floor();
            let d = if r < lo {
                lo - r
            } else if r > hi {
                r - hi
            } else {
                0.0
            };
            if d > 0.0 {
                let k = (1.0 - d / (feather + 1.0)).max(0.0);
                u2 = u + (u2 - u) * k;
                v2 = v + (v2 - v) * k;
            }
        }
        if fr.dl != 0.0 {
            let span = fr.ue - fr.us;
            let lam = if span > 0.0 { (u - fr.us) / span } else { 0.0 };
            u2 += lam * fr.dl;
        }
        (u2, v2)
    }

    /// n08: the number of onsets at or before tick `x` — walked from `from`
    /// (the count at the note's start: its points lie close by), searched
    /// when farther.
    #[inline(always)]
    fn latest(onsets: &[u64], from: usize, x: f64) -> usize {
        let mut i = from.min(onsets.len());
        for _ in 0..ONSET_WALK {
            if i < onsets.len() && onsets[i] as f64 <= x {
                i += 1;
            } else if i > 0 && onsets[i - 1] as f64 > x {
                i -= 1;
            } else {
                return i;
            }
        }
        onsets.partition_point(|&o| o as f64 <= x)
    }

    /// n01: the contour of `notes` per column of the box (the highest or
    /// lowest row sounding in it; held through gaps), moving-averaged over
    /// ±`r` columns, minus its mean; empty without a note in the box.
    fn contour_cols(&self, notes: &[TNote], top: bool, r: usize) -> Vec<f64> {
        let b = &self.g.bbox;
        let k = SHEAR_COLS as i128;
        let w = i128::from(b.t1 - b.t0).max(1);
        let t0 = i128::from(b.t0);
        // Per column boundary: each pitch's count change.
        let mut diff = vec![0i32; (SHEAR_COLS + 1) * 256];
        let mut any = false;
        for n in notes {
            let i0 = ((i128::from(n.start) - t0) * k).div_euclid(w).clamp(0, k) as usize;
            let i1 = (-(-(i128::from(n.end()) - t0) * k).div_euclid(w)).clamp(0, k) as usize;
            if i0 < i1 {
                diff[i0 * 256 + usize::from(n.pitch)] += 1;
                diff[i1 * 256 + usize::from(n.pitch)] -= 1;
                any = true;
            }
        }
        if !any {
            return Vec::new();
        }
        let mut cnt = [0i32; 256];
        let mut raw = vec![f64::NAN; SHEAR_COLS];
        for (i, x) in raw.iter_mut().enumerate() {
            for (c, d) in cnt.iter_mut().zip(&diff[i * 256..(i + 1) * 256]) {
                *c += d;
            }
            let pick = if top {
                (0..256).rev().find(|&p| cnt[p] > 0)
            } else {
                (0..256).find(|&p| cnt[p] > 0)
            };
            if let Some(p) = pick {
                *x = p as f64;
            }
        }
        let mut last = raw.iter().copied().find(|x| !x.is_nan()).unwrap_or(0.0);
        for x in &mut raw {
            if x.is_nan() {
                *x = last;
            } else {
                last = *x;
            }
        }
        let mut pre = vec![0.0; SHEAR_COLS + 1];
        for i in 0..SHEAR_COLS {
            pre[i + 1] = pre[i] + raw[i];
        }
        let sm: Vec<f64> = (0..SHEAR_COLS)
            .map(|i| {
                let (a, e) = (i.saturating_sub(r), (i + r + 1).min(SHEAR_COLS));
                (pre[e] - pre[a]) / (e - a) as f64
            })
            .collect();
        let mean = sm.iter().sum::<f64>() / SHEAR_COLS as f64;
        sm.into_iter().map(|x| x - mean).collect()
    }

    /// n10: each note's unit box (start tick, width, lowest row, rows): its
    /// phrase (per track, split at rests longer than the split rest), or its
    /// window of the fixed period from the box's start (per track; the
    /// window's own rows).
    fn units(&self, src: &Source) -> Units {
        let Some((rest, period)) = self.phrase else {
            return (Vec::new(), Vec::new());
        };
        let notes = &src.notes;
        let t0 = self.g.bbox.t0;
        let mut unit_of = vec![0u32; notes.len()];
        let mut boxes = Vec::new();
        let mut add = |idx: &[usize], win: Option<(u64, u64)>| {
            let (mut a, mut e, mut lo, mut hi) = (u64::MAX, 0u64, u8::MAX, 0u8);
            for &i in idx {
                let n = &notes[i];
                a = a.min(n.start);
                e = e.max(n.end());
                lo = lo.min(n.pitch);
                hi = hi.max(n.pitch);
            }
            let (ut0, uw) = match win {
                Some((s, len)) => (s as f64, len as f64),
                None => (a as f64, e.saturating_sub(a).max(1) as f64),
            };
            let k = boxes.len() as u32;
            boxes.push((ut0, uw, f64::from(lo), f64::from(hi - lo) + 1.0));
            for &i in idx {
                unit_of[i] = k;
            }
        };
        for idx in per_track(src) {
            if idx.is_empty() {
                continue;
            }
            match period {
                None => {
                    for ph in Phrases::of(notes, &idx, rest, None).iter(&idx) {
                        add(ph, None);
                    }
                }
                Some(len) => {
                    let win = |i: usize| notes[i].start.saturating_sub(t0) / len;
                    let mut a = 0;
                    while a < idx.len() {
                        let k = win(idx[a]);
                        let mut e = a + 1;
                        while e < idx.len() && win(idx[e]) == k {
                            e += 1;
                        }
                        add(&idx[a..e], Some((t0.saturating_add(k * len), len)));
                        a = e;
                    }
                }
            }
        }
        (unit_of, boxes)
    }

    /// Every note's swept values: the settings', with the Control Sweeps
    /// applied at each note's place.
    fn vals(&self, src: &Source) -> (ShVals, Vec<ShVals>) {
        let base = ShVals {
            amount: self.amount,
            cscale: self.contour.map_or(0.0, |c| c.2),
            slen: self.slope_len.map_or(0.0, |x| x.1),
            namp: self.noise.as_ref().map_or(0.0, |x| x.1),
            axis: self.split.as_ref().map_or(0.0, |x| x.0),
            kscale: self.slope.as_ref().map_or(0.0, |x| x.1),
            tscale: self.tempo.as_ref().map_or(0.0, |x| x.1),
            lo: self.band.map_or(0.0, |x| x.0),
            hi: self.band.map_or(0.0, |x| x.1),
            pamp: self.pulse.as_ref().map_or(0.0, |x| x.2),
            copies: self.copies.map_or(0, |x| x.0),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let int = |k: &str, v: i64| if sw.drives(k) { sw.int(k, v, n, pl) } else { v };
                let mut d = base;
                d.amount = pct("amount", base.amount);
                d.cscale = pct("n01.scale", base.cscale);
                d.slen = pct("n02.amount", base.slen);
                d.namp = pct("n03.amp", base.namp);
                d.axis = int("n04.axis", base.axis as i64) as f64;
                d.kscale = pct("n05.scale", base.kscale);
                if let Some((_, _, set)) = &self.tempo
                    && sw.drives("n06.scale")
                {
                    d.tscale = sw.float("n06.scale", set * 100.0, n, pl) / 100.0;
                }
                if sw.drives("n07.lo") {
                    let a = sw.int("n07.lo", self.band_raw.0, n, pl);
                    let z = self.band_raw.1;
                    (d.lo, d.hi) = (a.min(z) as f64, a.max(z) as f64);
                }
                if sw.drives("n08.amp") {
                    d.pamp = sw.int("n08.amp", base.pamp as i64, n, pl).clamp(-48, 48) as f64;
                }
                if sw.drives("n09.copies") {
                    d.copies = sw
                        .int("n09.copies", i64::from(base.copies), n, pl)
                        .clamp(1, 16) as u32;
                }
                d
            })
            .collect();
        (base, per)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let b = &self.g.bbox;
        let mut run = ShearRun::default();
        if let Some((of, r, _)) = self.contour {
            let refs: &[TNote] = if of == 2 {
                src.aux.first().map_or(&[], |l| l.as_slice())
            } else {
                &src.notes
            };
            run.contour = self.contour_cols(refs, of != 1, r);
        }
        if let Some((choice, ..)) = &self.pulse {
            let refs: &[TNote] = if *choice == 0 {
                src.context.as_slice()
            } else {
                src.aux.get(1).map_or(&[], |l| l.as_slice())
            };
            let mut o: Vec<u64> = refs.iter().map(|n| n.start).collect();
            o.sort_unstable();
            o.dedup();
            run.onsets = o;
        }
        let (unit_of, boxes) = self.units(src);
        let (w, rows) = (b.width(), b.rows());
        let (vbase, vper) = self.vals(src);
        let frame = |i: usize, n: &TNote| -> SFrame {
            let (us, v) = b.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
            let mut fr = SFrame {
                w: vper.get(i).copied().unwrap_or(vbase),
                local: unit_of.get(i).map(|&k| boxes[k as usize]),
                us,
                ue: (n.end() as f64 - b.t0 as f64) / w,
                dl: 0.0,
                onset: if run.onsets.is_empty() {
                    0
                } else {
                    run.onsets.partition_point(|&o| o <= n.start)
                },
            };
            if let Some((sign, _)) = self.slope_len {
                let amount = fr.w.slen;
                // n02: the end moves by the shift between the neighbouring
                // rows (half the difference of the rows above and below).
                let up = self.point(&fr, &run, n, us, v + 1.0 / rows).0;
                let dn = self.point(&fr, &run, n, us, v - 1.0 / rows).0;
                let d = sign * amount * ((up - dn) / 2.0).abs();
                fr.dl = d.max(-(n.dur.saturating_sub(1) as f64) / w);
            }
            fr
        };
        // n11: the largest distance a note's start moves.
        let moved = |fr: &SFrame, n: &TNote| -> f64 {
            let v = (f64::from(n.pitch) + 0.5 - f64::from(b.p0)) / rows;
            let (u2, v2) = self.point(fr, &run, n, fr.us, v);
            (u2 - fr.us).hypot(v2 - v)
        };
        // Each note's distance (kept for the run below).
        let dist: Vec<f64> = if self.colours.is_some() {
            src.notes
                .iter()
                .enumerate()
                .map(|(i, n)| moved(&frame(i, n), n))
                .collect()
        } else {
            Vec::new()
        };
        let dmax = dist.iter().copied().fold(0.0f64, f64::max);
        let colour = |t: &mut TNote, n: &TNote, d: f64| {
            if let Some((bands, pal)) = &self.colours {
                let k = if dmax > 0.0 {
                    ((d / dmax * f64::from(*bands)) as u32).min(bands - 1)
                } else {
                    0
                };
                let (id, ch) = sub_at(&self.tracks, n, pal[k as usize % pal.len()]);
                t.sub = id;
                t.ch = ch;
            }
        };
        // n09: the full map's values at the original's raster points, reused
        // by its copies (the raster asks for the same points again).
        let memo: Memo = Default::default();
        for (i, n) in src.notes.iter().enumerate() {
            let fr = frame(i, n);
            let d = dist.get(i).copied().unwrap_or(0.0);
            let mut t = *n;
            colour(&mut t, n, d);
            memo.borrow_mut().clear();
            let full = |u: f64, v: f64| {
                if self.copies.is_some() {
                    let key = (u.to_bits(), v.to_bits());
                    if let Some(&(_, _, r)) = memo.borrow().iter().find(|m| (m.0, m.1) == key) {
                        return r;
                    }
                    let r = self.point(&fr, &run, n, u, v);
                    let mut m = memo.borrow_mut();
                    if m.len() < 64 {
                        m.push((key.0, key.1, r));
                    }
                    return r;
                }
                self.point(&fr, &run, n, u, v)
            };
            // V09-0143: the copy (0 = the sheared note, n09's copies 1 …).
            out.set_dim(0);
            if !self.g.rasterize(&t, &full, &self.tracks, out) {
                return out.check();
            }
            if let Some((_, step)) = self.copies {
                let copies = fr.w.copies;
                // n09: copies at stepped strengths back toward the original.
                for j in 1..=copies {
                    out.set_dim(j);
                    let f = 1.0 - f64::from(j) / f64::from(copies + 1);
                    let mut c = *n;
                    c.vel = (f64::from(n.vel) * (1.0 + f64::from(j) * step))
                        .round()
                        .clamp(1.0, 127.0) as u8;
                    colour(&mut c, n, d * f);
                    let part = |u: f64, v: f64| {
                        let (u2, v2) = full(u, v);
                        (u + (u2 - u) * f, v + (v2 - v) * f)
                    };
                    if !self.g.rasterize(&c, &part, &self.tracks, out) {
                        return out.check();
                    }
                }
            }
            out.check()?;
        }
        Ok(())
    }
}

// ====================================================================== Twirl

pub const TWIRL: ToolDef = ToolDef {
    id: "twirl",
    spec: twirl_spec,
    build: twirl_build,
};

const FALLOFFS: &[&str] = &["Smooth", "Sharp", "Constant"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const TWIRL_HELP: &[(&str, &str)] = &[
    (
        "angle",
        "How far the center is turned (degrees); the turn fades to nothing at the ellipse's edge (− turns the other way).",
    ),
    (
        "cx",
        "The twirl's center in time, as a share of the selection's box.",
    ),
    (
        "cy",
        "The twirl's center in pitch, as a share of the selection's box.",
    ),
    (
        "rx",
        "The ellipse's radius along time, as a share of the box: outside it nothing turns.",
    ),
    (
        "ry",
        "The ellipse's radius along pitch, as a share of the box.",
    ),
    (
        "a1",
        "Chooses how the turn fades from the center to the edge.",
    ),
    (
        "a1.shape",
        "Smooth fades gently, Sharp falls off quickly away from the center, Constant turns the whole ellipse by the full angle.",
    ),
    (
        "a2",
        "Turns everything inside an inner radius fully, fading only between it and the edge.",
    ),
    (
        "a2.inner",
        "The inner radius, as a share of the ellipse, inside which the turn is full.",
    ),
    ("a3", "Turns an outer ring the opposite way."),
    (
        "a3.ring",
        "Where the reversed ring starts, as a share of the ellipse's radius.",
    ),
    ("a4", "Adds more twirl centers."),
    (
        "a4.centres",
        "The extra centers as x, y pairs, in thousandths of the box.",
    ),
    ("a5", "Tilts the ellipse."),
    ("a5.tilt", "The ellipse's tilt angle."),
    (
        "a6",
        "Turns along a logarithmic spiral: the radius grows or shrinks as it turns.",
    ),
    (
        "a6.k",
        "How fast the spiral grows (− shrinks toward the center).",
    ),
    ("a7", "Turns in steps of a fixed angle instead of smoothly."),
    ("a7.step", "The size of each angle step."),
    ("a8", "Turns each subchannel by an extra angle."),
    (
        "a8.offset",
        "The extra angle per subchannel position (the first subchannel gets none).",
    ),
    (
        "a9",
        "Leaves copies behind at fractions of the turn, like a motion trail.",
    ),
    ("a9.copies", "How many trail copies."),
    (
        "a10",
        "Sets the turn's strength by distance from the center with a curve.",
    ),
    (
        "a10.curve",
        "The strength from the center (left) to the ellipse's edge (right).",
    ),
    ("n01", "Moves the twirl's center along a path across time."),
    (
        "n01.x",
        "The center's position in time along the selection.",
    ),
    (
        "n01.y",
        "The center's position in pitch along the selection.",
    ),
    (
        "n01.speed",
        "How fast the center travels its path (100 % = once across the selection).",
    ),
    (
        "n02",
        "Spins each chord, group or bar around its own center.",
    ),
    (
        "n02.unit",
        "What spins on its own: each chord, each note group, or each bar.",
    ),
    ("n02.angle", "How far each unit spins."),
    ("n02.alt", "Spin every other unit the opposite way."),
    ("n03", "Thins the result into spiral arms, like a galaxy."),
    ("n03.arms", "How many arms."),
    ("n03.width", "How wide each arm is, in degrees."),
    (
        "n03.winding",
        "How tightly the arms wind around the center (− the other way).",
    ),
    ("n04", "Clears a hole in the middle of the twirl."),
    (
        "n04.radius",
        "The hole's radius, as a share of the ellipse.",
    ),
    (
        "n04.inside",
        "Notes in the hole are pushed out to its rim, or removed.",
    ),
    ("n05", "Turns only a sector (a slice of the circle)."),
    ("n05.start", "Where the sector starts, in degrees."),
    ("n05.width", "How wide the sector is, in degrees."),
    (
        "n05.feather",
        "How softly the turn fades at the sector's edges, in degrees.",
    ),
    (
        "n06",
        "Adds a row of vortices spinning alternately, like a vortex street.",
    ),
    ("n06.count", "How many vortices."),
    (
        "n06.spacing",
        "The distance between vortices, as a share of the box.",
    ),
    (
        "n06.offset",
        "How far every other vortex is moved up or down, as a share of the box.",
    ),
    (
        "n07",
        "Turns the notes' bodies with the twirl: notes turned upright become columns of short pieces.",
    ),
    ("n07.bodies", "Bodies follow the twirl, or stay horizontal."),
    (
        "n07.res",
        "How many keys each piece of a turned note covers.",
    ),
    ("n07.shortest", "No piece is shorter than this, in ticks."),
    (
        "n08",
        "Snaps the twirled notes onto rings around the center.",
    ),
    ("n08.rings", "How many rings."),
    (
        "n08.strength",
        "How far notes move onto their ring (100 % = exactly).",
    ),
    (
        "n09",
        "Re-lays the notes on a sunflower spiral (phyllotaxis).",
    ),
    (
        "n09.order",
        "The order the notes are laid out in: by time, pitch or velocity.",
    ),
    ("n09.spacing", "The spiral's size, as a share of the disc."),
    (
        "n09.divergence",
        "The angle between consecutive notes (137.5° = the golden angle).",
    ),
    (
        "n10",
        "Adds random wobble to the turn, ring by ring (seeded).",
    ),
    ("n10.noise", "The largest extra turn, in degrees."),
    ("n10.rings", "How many rings of independent wobble."),
    (
        "n11",
        "Lengthens (or shortens) notes along the direction of the turn.",
    ),
    (
        "n11.amount",
        "How much the turn's motion stretches each note (− shortens).",
    ),
];

fn twirl_spec() -> ToolSpec {
    let mut s = spec(
        "twirl",
        "Twirl",
        "Rotates the selection around a center, most at the center and not at all at the edge of the ellipse (Photopea-style Twirl), then rasterizes the result to notes.",
        vec![
            float("angle", "Angle", -999.0, 999.0, 180.0, "°"),
            float("cx", "Center X", 0.0, 100.0, 50.0, "%"),
            float("cy", "Center Y", 0.0, 100.0, 50.0, "%"),
            float("rx", "X scale (ellipse radius)", 5.0, 200.0, 50.0, "%"),
            float("ry", "Y scale (ellipse radius)", 5.0, 200.0, 50.0, "%"),
            adv_switch(1, "Falloff shape"),
            choice("a1.shape", "Falloff", FALLOFFS, 0).adv(1),
            adv_switch(2, "Inner / outer radius"),
            float("a2.inner", "Full rotation inside", 0.0, 95.0, 30.0, "%").adv(2),
            adv_switch(3, "Reverse outer ring"),
            float("a3.ring", "Ring starts at", 5.0, 95.0, 60.0, "%").adv(3),
            adv_switch(4, "Multiple centers"),
            list(
                "a4.centres",
                "More centers ‰ (x, y, …)",
                0,
                1000,
                16,
                &[250, 250, 750, 750],
            )
            .adv(4),
            adv_switch(5, "Tilted ellipse"),
            float("a5.tilt", "Ellipse tilt", -90.0, 90.0, 30.0, "°").adv(5),
            adv_switch(6, "Log spiral"),
            float("a6.k", "Spiral growth", -100.0, 100.0, 30.0, "%").adv(6),
            adv_switch(7, "Stepped angle"),
            float("a7.step", "Step", 1.0, 180.0, 30.0, "°").adv(7),
            adv_switch(8, "Subchannel angular offset"),
            float(
                "a8.offset",
                "Extra angle per subchannel position",
                -360.0,
                360.0,
                45.0,
                "°",
            )
            .adv(8),
            adv_switch(9, "Trail duplication"),
            int("a9.copies", "Trail copies", 1, 8, 3).adv(9),
            adv_switch(10, "Strength envelope by radius"),
            curve(
                "a10.curve",
                "Strength by radius (0 = center)",
                Curve::line(1.0, 0.2),
            )
            .overshoot()
            .adv(10),
            // V09-S38: functions declared by stable id (TOOL_CATALOG n01 … n11).
            flag("n01", "Moving center", false),
            curve("n01.x", "Path X (center by time)", Curve::line(0.2, 0.8)),
            curve("n01.y", "Path Y (center by time)", Curve::flat(0.5)),
            float("n01.speed", "Path speed", 10.0, 400.0, 100.0, "%"),
            flag("n02", "Local vortices", false),
            choice("n02.unit", "Unit", VORTEX_UNITS, 0),
            float("n02.angle", "Angle", -720.0, 720.0, 90.0, "°"),
            choice("n02.alt", "Alternate direction", NO_YES, 0),
            flag("n03", "Galaxy arms", false),
            int("n03.arms", "Arms", 1, 12, 3),
            float("n03.width", "Arm width", 1.0, 180.0, 30.0, "°"),
            float("n03.winding", "Winding", -100.0, 100.0, 50.0, "%"),
            flag("n04", "Void", false),
            float("n04.radius", "Void radius", 0.0, 100.0, 20.0, "%"),
            choice("n04.inside", "Inside notes", VOID_INSIDE, 0),
            flag("n05", "Sector mask", false),
            float("n05.start", "Start angle", -180.0, 180.0, 0.0, "°"),
            float("n05.width", "Sector width", 1.0, 360.0, 180.0, "°"),
            float("n05.feather", "Feather", 0.0, 90.0, 15.0, "°"),
            flag("n06", "Vortex street", false),
            int("n06.count", "Vortices", 2, 32, 4),
            float("n06.spacing", "Spacing", 1.0, 100.0, 25.0, "%"),
            float("n06.offset", "Row offset", 0.0, 50.0, 15.0, "%"),
            flag("n07", "Body rotation", false),
            choice("n07.bodies", "Bodies", BODIES, 0),
            int("n07.res", "Row resolution", 1, 24, 1),
            ticks("n07.shortest", "Shortest piece", 1, 1 << 40, 30),
            flag("n08", "Ring snap", false),
            int("n08.rings", "Rings", 1, 64, 6),
            float("n08.strength", "Strength", 0.0, 100.0, 100.0, "%"),
            flag("n09", "Phyllotaxis", false),
            choice("n09.order", "Order", PHYLLO_ORDER, 0),
            float(
                "n09.spacing",
                "Spacing (disc radius)",
                1.0,
                100.0,
                90.0,
                "%",
            ),
            float("n09.divergence", "Divergence", 1.0, 359.0, 137.508, "°"),
            flag("n10", "Turbulent twirl", false),
            float("n10.noise", "Noise", 0.0, 180.0, 30.0, "°"),
            int("n10.rings", "Rings", 1, 64, 8),
            flag("n11", "Tangential lengths", false),
            float("n11.amount", "Amount", -100.0, 400.0, 100.0, "%"),
        ],
        [
            "Falloff shape",
            "Inner / outer radius",
            "Reverse outer ring",
            "Multiple centers",
            "Tilted ellipse",
            "Log spiral",
            "Stepped angle",
            "Subchannel angular offset",
            "Trail duplication",
            "Strength envelope by radius",
        ],
    );
    s.functions = [
        ("n01", "Moving center", 130),
        ("n02", "Local vortices", 120),
        ("n03", "Galaxy arms", 500),
        ("n04", "Void", 170),
        ("n05", "Sector mask", 150),
        ("n06", "Vortex street", 140),
        ("n07", "Body rotation", 600),
        ("n08", "Ring snap", 180),
        ("n09", "Phyllotaxis", 110),
        ("n10", "Turbulent twirl", 160),
        ("n11", "Tangential lengths", 190),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, TWIRL_HELP);
    s
}

fn twirl_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S38 gate (E0409): Control Sweeps run inside the v0.9 kernel, per
    // note (`definition::kernel_sweeps`).
    if TWIRL_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return twirl9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let angle = p.float("angle") * PI / 180.0;
    let mut centres = vec![(p.float("cx") / 100.0, p.float("cy") / 100.0)];
    if p.adv(4) {
        let v = permille(&p.list("a4.centres"));
        centres.extend(v.chunks_exact(2).map(|c| (c[0], c[1])));
    }
    let (rx, ry) = (
        (p.float("rx") / 100.0).max(1e-3),
        (p.float("ry") / 100.0).max(1e-3),
    );
    let shape = p.adv(1).then(|| p.choice("a1.shape"));
    let inner = p.adv(2).then(|| p.float("a2.inner") / 100.0);
    let ring = p.adv(3).then(|| p.float("a3.ring") / 100.0);
    let tilt = p.adv(5).then(|| p.float("a5.tilt") * PI / 180.0);
    let spiral = p.adv(6).then(|| p.float("a6.k") / 100.0);
    let step = p.adv(7).then(|| p.float("a7.step") * PI / 180.0);
    let sub_off = p.adv(8).then(|| p.float("a8.offset") * PI / 180.0);
    let copies = p.adv(9).then(|| p.int("a9.copies").max(1) as u32);
    let env = p.adv(10).then(|| p.curve("a10.curve"));
    let tracks = b.tracks.to_vec();
    let tr2 = tracks.clone();
    // One rotation around (cx, cy) scaled by `frac` of the angle.
    let twirl = move |n: &TNote, u: f64, v: f64, frac: f64| -> (f64, f64) {
        let (mut u, mut v) = (u, v);
        for &(cx, cy) in &centres {
            // Tilt the ellipse: rotate into its frame before normalizing.
            let (mut dx, mut dy) = (u - cx, v - cy);
            if let Some(t) = tilt {
                let (s, c) = (-t).sin_cos();
                (dx, dy) = (dx * c - dy * s, dx * s + dy * c);
            }
            let (x, y) = (dx / rx, dy / ry);
            let r = (x * x + y * y).sqrt();
            if r >= 1.0 {
                continue;
            }
            let t = 1.0 - r;
            let mut w = match shape {
                Some(0) => smoothstep(t),
                Some(1) => t * t * t,
                Some(_) => 1.0,
                None => t,
            };
            if let Some(i) = inner {
                w = if r <= i {
                    1.0
                } else {
                    (1.0 - r) / (1.0 - i).max(1e-6)
                };
            }
            if let Some(c) = &env {
                w *= c.eval(r);
            }
            let mut a = angle * w * frac;
            if let Some(rg) = ring
                && r > rg
            {
                a = -a;
            }
            if let Some(o) = sub_off {
                a += o * sub_pos(&tr2, n) as f64 * w * frac;
            }
            if let Some(s) = step {
                a = (a / s).round() * s;
            }
            let (s, c) = a.sin_cos();
            let (mut x2, mut y2) = (x * c - y * s, x * s + y * c);
            if let Some(k) = spiral {
                let f = (k * a).exp();
                x2 *= f;
                y2 *= f;
            }
            let (mut ex, mut ey) = (x2 * rx, y2 * ry);
            if let Some(t) = tilt {
                let (s, c) = t.sin_cos();
                (ex, ey) = (ex * c - ey * s, ex * s + ey * c);
            }
            u = cx + ex;
            v = cy + ey;
        }
        (u, v)
    };
    match copies {
        None => geom_kernel(g, tracks, move |n, u, v| twirl(n, u, v, 1.0)),
        Some(k) => {
            let per = g.per_note() * (u64::from(k) + 1);
            boxed(Contextual {
                f: move |src: &Source, out: &mut Output| {
                    for n in &src.notes {
                        for c in 0..=k {
                            // V09-0143: the trail copy (0 = the note where
                            // it was, `k` = fully twirled).
                            out.set_dim(c);
                            let frac = f64::from(c) / f64::from(k);
                            if !g.rasterize(n, &|u, v| twirl(n, u, v, frac), &tracks, out) {
                                return out.check();
                            }
                        }
                        out.check()?;
                    }
                    Ok(())
                },
                growth: Growth::Expand(per),
                estimate: None,
            })
        }
    }
}

// ----------------------------------------------------------- Twirl (v0.9)

const TWIRL_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];
const VORTEX_UNITS: &[&str] = &["Chord", "Group", "Bar"];
const VOID_INSIDE: &[&str] = &["Pushed to the rim", "Removed"];
const BODIES: &[&str] = &["Follow the twirl", "Keep horizontal"];
const PHYLLO_ORDER: &[&str] = &["Time", "Pitch", "Velocity"];

/// The v0.9 Twirl (V09-S38 step 1): the v0.8 rotation re-read from the same
/// parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions in their catalog order: 110 n09 phyllotaxis (notes
/// re-laid on a golden-angle spiral), 120 n02 local vortices (each unit
/// spun around its own centre), 130 n01 moving centre, 140 n06 vortex
/// street (more centres, alternating spin), 150 n05 sector mask, 160 n10
/// turbulent twirl (angle noise by radius), 170 n04 void, 180 n08 ring snap,
/// 190 n11 tangential lengths, 500 n03 galaxy arms (thinning), 600 n07 body
/// rotation (notes turned toward vertical become columns of pieces).
#[derive(Clone)]
struct Twirl9 {
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    angle: f64,
    /// [0] the main centre, then a4's.
    centres: Vec<(f64, f64)>,
    rx: f64,
    ry: f64,
    shape: Option<usize>,
    inner: Option<f64>,
    ring: Option<f64>,
    tilt: Option<f64>,
    spiral: Option<f64>,
    step: Option<f64>,
    sub_off: Option<f64>,
    copies: Option<u32>,
    env: Option<Curve>,
    // ----- v0.9 functions
    /// n01: path X, path Y (box units by time), speed.
    path: Option<(Curve, Curve, f64)>,
    /// n02: unit (chord / group / bar), angle (radians), alternate.
    local: Option<(usize, f64, bool)>,
    /// n03: arms, half the arm width (radians), winding (turns over the
    /// ellipse radius).
    arms: Option<(f64, f64, f64)>,
    /// n04: void radius (of the ellipse), inside notes removed.
    void: Option<(f64, bool)>,
    /// n05: start angle, width, feather (degrees).
    sector: Option<(f64, f64, f64)>,
    /// n06: the street's centres (box units) and spins (±1).
    street: Vec<(f64, f64, f64)>,
    /// n07: row resolution, shortest piece (ticks).
    bodies: Option<(i64, u64)>,
    /// n08: rings, strength.
    snap: Option<(f64, f64)>,
    /// n09: order (time / pitch / velocity), disc radius (of the ellipse
    /// radius), divergence (radians).
    phyllo: Option<(usize, f64, f64)>,
    /// n10: the angle noise's nodes over the radius (radians) and the
    /// amount they were drawn at.
    noise: Option<(Vec<f64>, f64, f64)>,
    /// n11: amount.
    tangential: Option<f64>,
    /// n06's count, spacing and offset (a swept spacing rebuilds the street
    /// per note).
    street_def: Option<(i64, f64, f64)>,
}

/// n06: the street's centres (box units) and spins (±1) around the main
/// centre.
fn street_of(cx: f64, cy: f64, n: i64, spacing: f64, off: f64) -> Vec<(f64, f64, f64)> {
    (0..n)
        .map(|k| {
            let even = k % 2 == 0;
            (
                cx + (k as f64 - (n - 1) as f64 / 2.0) * spacing,
                cy + if even { off } else { -off },
                if even { 1.0 } else { -1.0 },
            )
        })
        .collect()
}

/// One note's values of the parameters Twirl's Control Sweeps drive (V09-S38
/// gate, E0409: per note inside the kernel, so n02's units and n09's spiral
/// see the whole selection).
#[derive(Clone, Copy, Debug)]
struct TVals {
    angle: f64,
    speed: f64,
    /// n02: the units' angle (before the alternating sign).
    la: f64,
    half: f64,
    rv: f64,
    sstart: f64,
    spacing: f64,
    snap: f64,
    div: f64,
    noise: f64,
}

/// n02: a unit's centre, radii (box units) and signed angle.
type Vortex = (f64, f64, f64, f64, f64);

/// One note's settings for [`Twirl9::point`].
#[derive(Clone, Copy, Debug)]
struct TFrame {
    /// The note's swept values.
    w: TVals,
    /// n09: the note's move to its spiral place (box units).
    shift: (f64, f64),
    /// n02: the note's unit.
    local: Option<Vortex>,
    /// The note's start and end (box units, before the shift).
    us: f64,
    ue: f64,
    /// n11: the end's move (box widths) at full strength.
    dl: f64,
}

/// n05: the sector's weight at angle `theta` (radians, the ellipse frame):
/// 1 inside [start, start + width], falling linearly to 0 over the feather
/// outside it.
fn sector_weight((start, width, feather): (f64, f64, f64), theta: f64) -> f64 {
    let rel = (theta.to_degrees() - start).rem_euclid(360.0);
    if rel <= width {
        return 1.0;
    }
    let d = (rel - width).min(360.0 - rel);
    if feather > 0.0 && d < feather {
        1.0 - d / feather
    } else {
        0.0
    }
}

fn twirl9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    // A function whose setting does nothing is skipped — unless a Control
    // Sweep drives that setting (its value then varies per note).
    let swept = |k: &str| b.ctx.sweeps.as_ref().is_some_and(|s| s.drives(k));
    let mut centres = vec![(p.float("cx") / 100.0, p.float("cy") / 100.0)];
    if p.adv(4) {
        let v = permille(&p.list("a4.centres"));
        centres.extend(v.chunks_exact(2).map(|c| (c[0], c[1])));
    }
    let (cx, cy) = centres[0];
    let street_def = p.flag("n06").then(|| {
        (
            p.int("n06.count").clamp(2, 32),
            p.float("n06.spacing") / 100.0,
            p.float("n06.offset") / 100.0,
        )
    });
    let street = street_def.map_or(Vec::new(), |(n, spacing, off)| {
        street_of(cx, cy, n, spacing, off)
    });
    let noise = p
        .flag("n10")
        .then(|| {
            let amount = p.float("n10.noise").clamp(0.0, 180.0) * PI / 180.0;
            let rings = p.int("n10.rings").clamp(1, 64) as u64;
            let salt = salt_of("twirl.n10");
            // Built at the setting's amount (1 when only a sweep sets it).
            (amount != 0.0 || swept("n10.noise")).then(|| {
                let at = if amount == 0.0 { 1.0 } else { amount };
                let nodes = (0..=rings)
                    .map(|k| at * (2.0 * stream(0, salt, &[k]).unit() - 1.0))
                    .collect();
                (nodes, at, amount)
            })
        })
        .flatten();
    let k = Twirl9 {
        angle: p.float("angle") * PI / 180.0,
        rx: (p.float("rx") / 100.0).max(1e-3),
        ry: (p.float("ry") / 100.0).max(1e-3),
        centres,
        shape: p.adv(1).then(|| p.choice("a1.shape")),
        inner: p.adv(2).then(|| p.float("a2.inner") / 100.0),
        ring: p.adv(3).then(|| p.float("a3.ring") / 100.0),
        tilt: p.adv(5).then(|| p.float("a5.tilt") * PI / 180.0),
        spiral: p.adv(6).then(|| p.float("a6.k") / 100.0),
        step: p.adv(7).then(|| p.float("a7.step") * PI / 180.0),
        sub_off: p.adv(8).then(|| p.float("a8.offset") * PI / 180.0),
        copies: p.adv(9).then(|| p.int("a9.copies").max(1) as u32),
        env: p.adv(10).then(|| p.curve("a10.curve")),
        path: p.flag("n01").then(|| {
            (
                p.curve("n01.x"),
                p.curve("n01.y"),
                p.float("n01.speed").clamp(10.0, 400.0) / 100.0,
            )
        }),
        local: p
            .flag("n02")
            .then(|| {
                let a = p.float("n02.angle") * PI / 180.0;
                (a != 0.0 || swept("n02.angle"))
                    .then(|| (p.choice("n02.unit").min(2), a, p.choice("n02.alt") == 1))
            })
            .flatten(),
        arms: p.flag("n03").then(|| {
            (
                p.int("n03.arms").clamp(1, 12) as f64,
                p.float("n03.width").clamp(1.0, 180.0) * PI / 360.0,
                p.float("n03.winding") / 100.0,
            )
        }),
        void: p
            .flag("n04")
            .then(|| {
                let rv = (p.float("n04.radius") / 100.0).clamp(0.0, 1.0);
                (rv > 0.0 || swept("n04.radius")).then(|| (rv, p.choice("n04.inside") == 1))
            })
            .flatten(),
        sector: p
            .flag("n05")
            .then(|| {
                let width = p.float("n05.width").clamp(1.0, 360.0);
                (width < 360.0).then(|| {
                    (
                        p.float("n05.start"),
                        width,
                        p.float("n05.feather").clamp(0.0, 90.0),
                    )
                })
            })
            .flatten(),
        street,
        bodies: (p.flag("n07") && p.choice("n07.bodies") == 0).then(|| {
            (
                p.int("n07.res").clamp(1, 24),
                p.ticks("n07.shortest").max(1),
            )
        }),
        snap: p
            .flag("n08")
            .then(|| {
                let k = (p.float("n08.strength") / 100.0).clamp(0.0, 1.0);
                (k > 0.0 || swept("n08.strength"))
                    .then(|| (p.int("n08.rings").clamp(1, 64) as f64, k))
            })
            .flatten(),
        phyllo: p.flag("n09").then(|| {
            (
                p.choice("n09.order").min(2),
                p.float("n09.spacing").clamp(1.0, 100.0) / 100.0,
                p.float("n09.divergence").clamp(1.0, 359.0) * PI / 180.0,
            )
        }),
        noise,
        tangential: p
            .flag("n11")
            .then(|| {
                let a = p.float("n11.amount") / 100.0;
                (a != 0.0).then_some(a)
            })
            .flatten(),
        street_def,
        ctx: b.ctx.clone(),
        g,
        tracks: b.tracks.to_vec(),
    };
    let copies = u64::from(k.copies.map_or(0, |c| c)) + 1;
    let per = k.g.per_note().saturating_mul(copies);
    let (growth, estimate) = match k.bodies {
        None => (
            if per > 1 {
                Growth::Expand(per)
            } else {
                Growth::Map
            },
            None,
        ),
        Some((res, shortest)) => {
            // A note becomes at most one piece per `shortest` ticks of its
            // length (or its usual raster): count · per + Σ length ÷
            // shortest, per copy.
            let pieces = u64::try_from(b.stats.sum_dur / u128::from(shortest)).unwrap_or(u64::MAX);
            let total = (b.stats.count as u64)
                .saturating_mul(k.g.per_note())
                .saturating_add(pieces)
                .saturating_mul(copies);
            let rows = (k.g.top as u64 + 1).div_ceil(res as u64) + 1;
            (Growth::Expand(per.saturating_mul(rows)), Some(total))
        }
    };
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth,
        estimate,
    })
}

impl Twirl9 {
    /// The main centre at `u`: n01's path, else Centre X / Y.
    #[inline(always)]
    fn main_centre(&self, u: f64, speed: f64) -> (f64, f64) {
        match &self.path {
            Some((px, py, _)) => {
                let t = u * speed;
                (px.eval(t), py.eval(t))
            }
            None => self.centres[0],
        }
    }

    /// Point (u, v) in centre `c`'s ellipse frame: (x, y, r).
    #[inline(always)]
    fn frame_of(&self, (cx, cy): (f64, f64), u: f64, v: f64) -> (f64, f64, f64) {
        let (mut dx, mut dy) = (u - cx, v - cy);
        if let Some(t) = self.tilt {
            let (s, c) = (-t).sin_cos();
            (dx, dy) = (dx * c - dy * s, dx * s + dy * c);
        }
        let (x, y) = (dx / self.rx, dy / self.ry);
        (x, y, (x * x + y * y).sqrt())
    }

    /// The v0.8 falloff at radius `r` (< 1): a1 shape, a2 inner radius,
    /// a10 envelope.
    #[inline(always)]
    fn falloff(&self, r: f64) -> f64 {
        let t = 1.0 - r;
        let mut w = match self.shape {
            Some(0) => smoothstep(t),
            Some(1) => t * t * t,
            Some(_) => 1.0,
            None => t,
        };
        if let Some(i) = self.inner {
            w = if r <= i {
                1.0
            } else {
                (1.0 - r) / (1.0 - i).max(1e-6)
            };
        }
        if let Some(c) = &self.env {
            w *= c.eval(r);
        }
        w
    }

    /// One centre's rotation (`twirl_build`'s loop body, unchanged) with
    /// n05's sector weight, n10's angle noise, n06's spin and — around the
    /// main centre — n04's void and n08's ring snap.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn spin(
        &self,
        tv: &TVals,
        n: &TNote,
        (u, v): (f64, f64),
        frac: f64,
        c: (f64, f64),
        sign: f64,
        main: bool,
    ) -> (f64, f64) {
        let (x, y, r) = self.frame_of(c, u, v);
        if r >= 1.0 {
            return (u, v);
        }
        let mut w = self.falloff(r);
        if let Some((_, width, feather)) = self.sector {
            let fs = sector_weight((tv.sstart, width, feather), y.atan2(x));
            if fs == 0.0 {
                return (u, v);
            }
            if fs != 1.0 {
                w *= fs;
            }
        }
        let mut a = tv.angle * w * frac;
        if let Some(rg) = self.ring
            && r > rg
        {
            a = -a;
        }
        if let Some(o) = self.sub_off {
            a += o * sub_pos(&self.tracks, n) as f64 * w * frac;
        }
        if let Some((nodes, at, _)) = &self.noise {
            if tv.noise == *at {
                a += catmull(nodes, r) * w * frac;
            } else {
                a += catmull(nodes, r) * (tv.noise / at) * w * frac;
            }
        }
        if sign != 1.0 {
            a = -a;
        }
        if let Some(s) = self.step {
            a = (a / s).round() * s;
        }
        let (s, cs) = a.sin_cos();
        let (mut x2, mut y2) = (x * cs - y * s, x * s + y * cs);
        if let Some(k) = self.spiral {
            let f = (k * a).exp();
            x2 *= f;
            y2 *= f;
        }
        if main {
            (x2, y2) = self.ring_parts(tv, x2, y2, frac);
        }
        let (mut ex, mut ey) = (x2 * self.rx, y2 * self.ry);
        if let Some(t) = self.tilt {
            let (s, cs) = t.sin_cos();
            (ex, ey) = (ex * cs - ey * s, ex * s + ey * cs);
        }
        (c.0 + ex, c.1 + ey)
    }

    /// n04 (pushed to the rim) and n08 on a rotated point of the main
    /// ellipse (its frame), at `frac` of their effect (trail copies).
    #[inline(always)]
    fn ring_parts(&self, tv: &TVals, mut x2: f64, mut y2: f64, frac: f64) -> (f64, f64) {
        let to = |r_new: f64, x2: &mut f64, y2: &mut f64| {
            let r2 = x2.hypot(*y2);
            let (tx, ty) = if r2 > 0.0 {
                (*x2 * r_new / r2, *y2 * r_new / r2)
            } else {
                (r_new, 0.0)
            };
            if frac == 1.0 {
                (*x2, *y2) = (tx, ty);
            } else {
                *x2 += (tx - *x2) * frac;
                *y2 += (ty - *y2) * frac;
            }
        };
        if let Some((_, false)) = self.void
            && x2.hypot(y2) < tv.rv
        {
            to(tv.rv, &mut x2, &mut y2);
        }
        if let Some((rings, _)) = self.snap {
            let k = tv.snap;
            let r2 = x2.hypot(y2);
            let snapped = ((r2 * rings).floor().min(rings - 1.0) + 0.5) / rings;
            to(r2 + (snapped - r2) * k, &mut x2, &mut y2);
        }
        (x2, y2)
    }

    /// The image of point (u, v) (box units) for a note's frame at `frac`
    /// of the twirl (a9's trail copies).
    #[inline(always)]
    fn point(&self, fr: &TFrame, n: &TNote, u0: f64, v0: f64, frac: f64) -> (f64, f64) {
        let (mut u, mut v) = (u0, v0);
        if self.phyllo.is_some() {
            u += fr.shift.0;
            v += fr.shift.1;
        }
        if let Some((lu, lv, lrx, lry, la)) = fr.local {
            let (x, y) = ((u - lu) / lrx, (v - lv) / lry);
            let r = (x * x + y * y).sqrt();
            if r < 1.0 {
                let (s, c) = (la * (1.0 - r) * frac).sin_cos();
                u = lu + (x * c - y * s) * lrx;
                v = lv + (x * s + y * c) * lry;
            }
        }
        let tv = &fr.w;
        for (i, &c) in self.centres.iter().enumerate() {
            let c = if i == 0 {
                self.main_centre(u, tv.speed)
            } else {
                c
            };
            (u, v) = self.spin(tv, n, (u, v), frac, c, 1.0, i == 0);
        }
        match self.street_def {
            Some((count, spacing, off)) if tv.spacing != spacing => {
                let (cx, cy) = self.centres[0];
                for (sx, sy, sg) in street_of(cx, cy, count, tv.spacing, off) {
                    (u, v) = self.spin(tv, n, (u, v), frac, (sx, sy), sg, false);
                }
            }
            _ => {
                for &(sx, sy, sg) in &self.street {
                    (u, v) = self.spin(tv, n, (u, v), frac, (sx, sy), sg, false);
                }
            }
        }
        if fr.dl != 0.0 {
            let span = fr.ue - fr.us;
            let lam = if span > 0.0 { (u0 - fr.us) / span } else { 0.0 };
            u += lam * fr.dl * frac;
        }
        (u, v)
    }

    /// n09: each note's move to its place on the spiral (k-th of N notes in
    /// the order at radius spacing · √((k + ½) / N) of the ellipse — Vogel's
    /// sunflower, evenly dense whatever N — angle k · divergence).
    fn spiral_shifts(&self, src: &Source, vals: &[TVals]) -> Vec<(f64, f64)> {
        let Some((order, spacing, div)) = self.phyllo else {
            return Vec::new();
        };
        let notes = &src.notes;
        let mut idx: Vec<usize> = (0..notes.len()).collect();
        // The source is in time order already (start, pitch, …): pitch and
        // velocity orders are stable bucket passes over it.
        if order != 0 {
            let key = |i: usize| {
                if order == 1 {
                    usize::from(notes[i].pitch)
                } else {
                    usize::from(notes[i].vel)
                }
            };
            let mut at = vec![0usize; 257];
            for i in 0..notes.len() {
                at[key(i) + 1] += 1;
            }
            for k in 1..at.len() {
                at[k] += at[k - 1];
            }
            for i in 0..notes.len() {
                let k = key(i);
                idx[at[k]] = i;
                at[k] += 1;
            }
        }
        let b = &self.g.bbox;
        let (cx, cy) = self.centres[0];
        let mut out = vec![(0.0, 0.0); notes.len()];
        let count = notes.len().max(1) as f64;
        for (k, &i) in idx.iter().enumerate() {
            let n = &notes[i];
            let rho = spacing * ((k as f64 + 0.5) / count).sqrt();
            let div = vals.get(i).map_or(div, |w| w.div);
            let (s, c) = (k as f64 * div).sin_cos();
            let (mu, mv) = b.to_unit(
                (n.start as f64 + n.end() as f64) / 2.0,
                f64::from(n.pitch) + 0.5,
            );
            out[i] = (cx + self.rx * rho * c - mu, cy + self.ry * rho * s - mv);
        }
        out
    }

    /// n02: each note's unit (centre, radii — the unit's box, √2 wider so
    /// its corners turn too — and signed angle, alternating by the units'
    /// order in time when asked); notes without a unit (group 0) none.
    fn vortices(&self, src: &Source, shifts: &[(f64, f64)]) -> Vec<Option<Vortex>> {
        let Some((unit, _, alt)) = self.local else {
            return Vec::new();
        };
        let notes = &src.notes;
        let b = &self.g.bbox;
        // The units as ranges of one index list (no list per unit).
        let mut flat: Vec<usize> = Vec::with_capacity(notes.len());
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut push = |part: &[usize], flat: &mut Vec<usize>| {
            ranges.push((flat.len(), flat.len() + part.len()));
            flat.extend_from_slice(part);
        };
        match unit {
            1 => {
                let mut by: Vec<usize> =
                    (0..notes.len()).filter(|&i| notes[i].group != 0).collect();
                by.sort_by_key(|&i| (notes[i].group, i));
                let mut a = 0;
                while a < by.len() {
                    let g = notes[by[a]].group;
                    let mut e = a + 1;
                    while e < by.len() && notes[by[e]].group == g {
                        e += 1;
                    }
                    push(&by[a..e], &mut flat);
                    a = e;
                }
            }
            _ => {
                for idx in per_track(src) {
                    if idx.is_empty() {
                        continue;
                    }
                    if unit == 0 {
                        let tol = (self.ctx.ppq / 32).max(1);
                        for c in ChordGroups::of(notes, &idx, tol).iter() {
                            push(c, &mut flat);
                        }
                    } else {
                        let mut a = 0;
                        while a < idx.len() {
                            let bar = bar_bounds(&self.ctx.meter, notes[idx[a]].start).0;
                            let mut e = a + 1;
                            while e < idx.len()
                                && bar_bounds(&self.ctx.meter, notes[idx[e]].start).0 == bar
                            {
                                e += 1;
                            }
                            push(&idx[a..e], &mut flat);
                            a = e;
                        }
                    }
                }
            }
        }
        let units: Vec<&[usize]> = ranges.iter().map(|&(a, e)| &flat[a..e]).collect();
        // Boxes (box units, after n09's moves) and their order in time.
        let shift = |i: usize| shifts.get(i).copied().unwrap_or((0.0, 0.0));
        let boxes: Vec<(f64, f64, f64, f64)> = units
            .iter()
            .map(|u| {
                let (mut ua, mut ub, mut va, mut vb) = (
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                );
                for &i in *u {
                    let n = &notes[i];
                    let (d0, d1) = shift(i);
                    let (s, lo) = b.to_unit(n.start as f64, f64::from(n.pitch));
                    let (e, hi) = b.to_unit(n.end() as f64, f64::from(n.pitch) + 1.0);
                    ua = ua.min(s + d0);
                    ub = ub.max(e + d0);
                    va = va.min(lo + d1);
                    vb = vb.max(hi + d1);
                }
                (ua, ub, va, vb)
            })
            .collect();
        let mut order: Vec<usize> = (0..units.len()).collect();
        order.sort_by(|&x, &y| {
            (boxes[x].0, boxes[x].2)
                .partial_cmp(&(boxes[y].0, boxes[y].2))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(x.cmp(&y))
        });
        let mut out = vec![None; notes.len()];
        for (rank, &k) in order.iter().enumerate() {
            let (ua, ub, va, vb) = boxes[k];
            let sign = if alt && rank % 2 == 1 { -1.0 } else { 1.0 };
            let f = Some((
                (ua + ub) / 2.0,
                (va + vb) / 2.0,
                ((ub - ua) / 2.0 * std::f64::consts::SQRT_2).max(1e-9),
                ((vb - va) / 2.0 * std::f64::consts::SQRT_2).max(1e-9),
                // The sign; each note's angle multiplies it (`run`).
                sign,
            ));
            for &i in units[k] {
                out[i] = f;
            }
        }
        out
    }

    /// The point before the main twirl (n09's move, n02's local spin at
    /// full strength).
    #[inline(always)]
    fn pre(&self, fr: &TFrame, u: f64, v: f64) -> (f64, f64) {
        let (mut u, mut v) = (u + fr.shift.0, v + fr.shift.1);
        if let Some((lu, lv, lrx, lry, la)) = fr.local {
            let (x, y) = ((u - lu) / lrx, (v - lv) / lry);
            let r = (x * x + y * y).sqrt();
            if r < 1.0 {
                let (s, c) = (la * (1.0 - r)).sin_cos();
                u = lu + (x * c - y * s) * lrx;
                v = lv + (x * s + y * c) * lry;
            }
        }
        (u, v)
    }

    /// Whether note `n` is removed: n04 (its middle inside the void before
    /// the twirl) or n03 (its twirled middle off every arm inside the main
    /// ellipse).
    fn removed(&self, fr: &TFrame, n: &TNote) -> bool {
        if !matches!(self.void, Some((_, true))) && self.arms.is_none() {
            return false;
        }
        let b = &self.g.bbox;
        let (mu, mv) = b.to_unit(
            (n.start as f64 + n.end() as f64) / 2.0,
            f64::from(n.pitch) + 0.5,
        );
        if let Some((_, true)) = self.void {
            let (pu, pv) = self.pre(fr, mu, mv);
            let (_, _, r) = self.frame_of(self.main_centre(pu, fr.w.speed), pu, pv);
            if r < fr.w.rv {
                return true;
            }
        }
        if let Some((arms, _, winding)) = self.arms {
            let half = fr.w.half;
            let (pu, _) = self.pre(fr, mu, mv);
            let c = self.main_centre(pu, fr.w.speed);
            let (tu, tv) = self.point(fr, n, mu, mv, 1.0);
            let (x, y, r) = self.frame_of(c, tu, tv);
            if r < 1.0 {
                let period = 2.0 * PI / arms;
                let psi = (y.atan2(x) - winding * 2.0 * PI * r).rem_euclid(period);
                let d = psi.min(period - psi);
                if d > half {
                    return true;
                }
            }
        }
        false
    }

    /// Every note's swept values: the settings', with the Control Sweeps
    /// applied at each note's place.
    fn vals(&self, src: &Source) -> (TVals, Vec<TVals>) {
        let base = TVals {
            angle: self.angle,
            speed: self.path.as_ref().map_or(1.0, |p| p.2),
            la: self.local.map_or(0.0, |l| l.1),
            half: self.arms.map_or(0.0, |a| a.1),
            rv: self.void.map_or(0.0, |v| v.0),
            sstart: self.sector.map_or(0.0, |s| s.0),
            spacing: self.street_def.map_or(0.0, |s| s.1),
            snap: self.snap.map_or(0.0, |s| s.1),
            div: self.phyllo.map_or(0.0, |p| p.2),
            noise: self.noise.as_ref().map_or(0.0, |n| n.1),
        };
        // The setting itself (0 when the nodes were drawn at 1 for a sweep).
        let noise_set = self.noise.as_ref().map_or(0.0, |n| n.2);
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| {
                let f = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v, n, pl)
                    } else {
                        v
                    }
                };
                let deg = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 180.0 / PI, n, pl) * PI / 180.0
                    } else {
                        v
                    }
                };
                let pct = |k: &str, v: f64| {
                    if sw.drives(k) {
                        sw.float(k, v * 100.0, n, pl) / 100.0
                    } else {
                        v
                    }
                };
                let mut d = base;
                d.angle = deg("angle", base.angle);
                d.speed = pct("n01.speed", base.speed).clamp(0.1, 4.0);
                d.la = deg("n02.angle", base.la);
                if sw.drives("n03.width") {
                    // The arm's half width: degrees / 2.
                    d.half = (sw
                        .float("n03.width", base.half * 360.0 / PI, n, pl)
                        .clamp(1.0, 180.0))
                        * PI
                        / 360.0;
                }
                d.rv = pct("n04.radius", base.rv).clamp(0.0, 1.0);
                d.sstart = f("n05.start", base.sstart);
                d.spacing = pct("n06.spacing", base.spacing);
                d.snap = pct("n08.strength", base.snap).clamp(0.0, 1.0);
                if sw.drives("n09.divergence") {
                    d.div = sw
                        .float("n09.divergence", base.div * 180.0 / PI, n, pl)
                        .clamp(1.0, 359.0)
                        * PI
                        / 180.0;
                }
                if sw.drives("n10.noise") {
                    d.noise = sw
                        .float("n10.noise", noise_set * 180.0 / PI, n, pl)
                        .clamp(0.0, 180.0)
                        * PI
                        / 180.0;
                }
                d
            })
            .collect();
        (base, per)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let b = &self.g.bbox;
        let w = b.width();
        let (tbase, tper) = self.vals(src);
        let shifts = self.spiral_shifts(src, &tper);
        let locals = self.vortices(src, &shifts);
        let copies = self.copies.unwrap_or(0);
        for (i, n) in src.notes.iter().enumerate() {
            let (us, v) = b.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
            let tv = tper.get(i).copied().unwrap_or(tbase);
            let mut fr = TFrame {
                w: tv,
                shift: shifts.get(i).copied().unwrap_or((0.0, 0.0)),
                local: locals
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|(a, b2, c, d, sign)| (a, b2, c, d, sign * tv.la)),
                us,
                ue: (n.end() as f64 - b.t0 as f64) / w,
                dl: 0.0,
            };
            if let Some(amount) = self.tangential {
                // n11: the arc the start travels around the main centre.
                let (pu, pv) = self.pre(&fr, us, v);
                let (_, _, r) = self.frame_of(self.main_centre(pu, tv.speed), pu, pv);
                if r < 1.0 {
                    let arc = (tv.angle * self.falloff(r)).abs() * r * self.rx;
                    fr.dl = (amount * arc).max(-(n.dur.saturating_sub(1) as f64) / w);
                }
            }
            if self.removed(&fr, n) {
                continue;
            }
            for c in 0..=copies {
                out.set_dim(c);
                let frac = if self.copies.is_some() {
                    f64::from(c) / f64::from(copies)
                } else {
                    1.0
                };
                let f = |u: f64, v: f64| self.point(&fr, n, u, v, frac);
                let ok = match self.bodies {
                    Some((res, shortest)) => match self.pieces(n, &f, res, shortest, out) {
                        Some(ok) => ok,
                        None => self.g.rasterize(n, &f, &self.tracks, out),
                    },
                    None => self.g.rasterize(n, &f, &self.tracks, out),
                };
                if !ok {
                    return out.check();
                }
            }
            out.check()?;
        }
        Ok(())
    }

    /// n07: a note whose mapped ends lie `res` rows or more apart becomes
    /// pieces — one per `res` rows, at most one per `shortest` ticks of its
    /// length — each the image of its part of the note (time span between
    /// the part's mapped ends, the row of its mapped middle). None when the
    /// note stays one note (the usual raster); Some(false) once `out` is
    /// full.
    fn pieces(
        &self,
        n: &TNote,
        f: &dyn Fn(f64, f64) -> (f64, f64),
        res: i64,
        shortest: u64,
        out: &mut Output,
    ) -> Option<bool> {
        let b = &self.g.bbox;
        let y = f64::from(n.pitch) + 0.5;
        let map = |x: f64| -> (f64, f64) {
            let (u, v) = b.to_unit(x, y);
            let (u, v) = f(u, v);
            let (x, y) = b.from_unit(u, v);
            self.g.affine(x, y)
        };
        let (xs, d) = (n.start as f64, n.dur as f64);
        let (r0, r1) = (map(xs).1.floor(), map(xs + d).1.floor());
        let span = (r1 - r0).abs();
        if span.is_nan() || span < res as f64 {
            return None;
        }
        let by_rows = (span / res as f64).ceil() as u64;
        let m = by_rows.min((n.dur / shortest).max(1)).max(1);
        for j in 0..m {
            let (ta, tb) = (j as f64 / m as f64, (j + 1) as f64 / m as f64);
            let xa = map(xs + ta * d).0;
            let xb = map(xs + tb * d).0;
            let row = map(xs + (ta + tb) / 2.0 * d).1.floor();
            let (s, e) = (xa.min(xb).round(), xa.max(xb).round());
            if !out.emit(
                s as i128,
                ((e - s) as i128).max(1),
                row as i64,
                i64::from(n.vel),
                n,
            ) {
                return Some(false);
            }
        }
        Some(true)
    }
}

// ======================================================================= Wave

pub const WAVE: ToolDef = ToolDef {
    id: "wave",
    spec: wave_spec,
    build: wave_build,
};

const WAVE_TYPES: &[&str] = &["Sine", "Triangle", "Square"];
const INTERFERENCE: &[&str] = &["Sum", "Strongest", "Multiply"];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const WAVE_HELP: &[(&str, &str)] = &[
    (
        "gens",
        "How many wave generators are added together; each gets a random wavelength, amplitude and phase from the seed.",
    ),
    (
        "len_min",
        "The shortest wavelength a generator may get, as a share of the selection's box.",
    ),
    (
        "len_max",
        "The longest wavelength a generator may get, as a share of the selection's box.",
    ),
    (
        "amp_min",
        "The smallest amplitude a generator may get, as a share of the selection's box.",
    ),
    (
        "amp_max",
        "The largest amplitude a generator may get, as a share of the selection's box.",
    ),
    (
        "sx",
        "How strongly the wave moves notes in time (0 % = no time movement).",
    ),
    (
        "sy",
        "How strongly the wave moves notes in pitch (0 % = no pitch movement).",
    ),
    (
        "type",
        "The waveform of every generator: smooth sine, straight-edged triangle, or square jumps.",
    ),
    (
        "a1",
        "Uses a sawtooth waveform instead of the chosen type: a straight ramp that drops back each period.",
    ),
    ("a2", "Uses a waveform you draw instead of the chosen type."),
    (
        "a2.curve",
        "One period of the waveform; 0.5 is the middle line, higher moves notes one way and lower the other.",
    ),
    (
        "a3",
        "Gives the generators set starting phases instead of random ones.",
    ),
    (
        "a3.phases",
        "The starting phase of each generator in turn, in degrees (repeated when there are more generators).",
    ),
    (
        "a4",
        "Stretches or shrinks the wavelength across the selection, so the wave speeds up or slows down.",
    ),
    (
        "a4.sweep",
        "How much longer the wavelength is at the selection's end than at its start (negative = shorter).",
    ),
    ("a5", "Shapes how strong the wave is across the selection."),
    (
        "a5.curve",
        "The wave's strength over the selection's time (0 = no movement, 1 = full).",
    ),
    ("a6", "Changes how the generators are combined."),
    (
        "a6.mode",
        "Sum adds the generators; Strongest uses whichever is largest at each point; Multiply multiplies them for a ring-modulated wave.",
    ),
    (
        "a7",
        "Makes the time movement follow time instead of pitch (by default each row of notes moves in time by its pitch).",
    ),
    (
        "a8",
        "Makes the pitch movement follow pitch instead of time (by default each moment moves in pitch by its time).",
    ),
    (
        "a9",
        "Puts the pitch wave a quarter period behind the time wave, so notes move in circles.",
    ),
    (
        "a10",
        "Gives each subchannel its own phase, so the subchannels wave against each other.",
    ),
    (
        "a10.phases",
        "The phase of each subchannel by its position in the track's list, in degrees (repeated).",
    ),
    (
        "n01",
        "Ties the wavelength to the song's tempo: beats, bars or milliseconds instead of a share of the box.",
    ),
    ("n01.unit", "The unit of the synced wavelength."),
    (
        "n01.len",
        "The longest generator's wavelength in the chosen unit; the others run at whole multiples of it.",
    ),
    (
        "n02",
        "Cuts each note into short pieces that follow a wave, so every note itself becomes wavy.",
    ),
    ("n02.seg", "The length of each piece, in ticks."),
    ("n02.amp", "How far the pieces move up and down, in keys."),
    ("n02.len", "The length of one wave along a note, in ticks."),
    (
        "n03",
        "Starts the wave over at musical points, so each section begins at the same place in the wave.",
    ),
    (
        "n03.at",
        "Where the wave restarts: each bar, phrase, chord, marker, or each note start of a reference.",
    ),
    (
        "n03.ref",
        "The track whose note starts restart the wave, by its number in the track list (0 = the selection's own note starts).",
    ),
    ("n03.phase", "The phase the wave restarts from, in degrees."),
    (
        "n04",
        "Uses the wave to change a note property as well as moving the notes.",
    ),
    (
        "n04.prop",
        "The property the wave changes: velocity, gate (length) or release velocity.",
    ),
    (
        "n04.depth",
        "How strongly the property follows the wave (100 % = from nothing up to double).",
    ),
    (
        "n04.phase",
        "How far the property's wave is shifted from the movement wave, in degrees.",
    ),
    (
        "n05",
        "Keeps only the notes where the wave is above (or below) a level, deleting the rest.",
    ),
    (
        "n05.threshold",
        "The level, as a share of the wave's height (0 % = the middle line).",
    ),
    (
        "n05.keep",
        "Which notes stay: those where the wave is above the level, or below it.",
    ),
    (
        "n06",
        "Moves notes along a Lissajous figure: time and pitch wave at different speeds.",
    ),
    ("n06.a", "How many times faster the time movement waves."),
    ("n06.b", "How many times faster the pitch movement waves."),
    (
        "n06.phase",
        "How far the pitch wave is shifted from the time wave, in degrees.",
    ),
    (
        "n07",
        "Folds, rectifies or clips the wave's peaks for harder shapes.",
    ),
    (
        "n07.mode",
        "Fold bends the peaks back; Full Rectify turns the lows into highs; Half Rectify removes the lows; Clip flattens the peaks.",
    ),
    (
        "n07.threshold",
        "The level where folding or clipping starts, as a share of the wave's height.",
    ),
    (
        "n07.folds",
        "How many times a peak may fold back before it is clipped.",
    ),
    (
        "n08",
        "Holds the wave's value steady for a while before jumping to the next, for a stepped wave.",
    ),
    ("n08.hold", "How long each value is held, in ticks."),
    (
        "n08.phase",
        "Shifts where the hold steps begin, as a share of the hold length.",
    ),
    (
        "n09",
        "Makes the wave pattern travel across pitch as time goes on.",
    ),
    (
        "n09.speed",
        "How many keys the pattern moves per beat (negative = downward).",
    ),
    (
        "n10",
        "Gives each track its own phase, so the tracks wave one after another.",
    ),
    (
        "n10.phase",
        "How much later each track's wave is than the previous track's, in degrees.",
    ),
    (
        "n10.order",
        "The order the tracks are counted in: their order in the track list, or by their lowest note.",
    ),
    (
        "n11",
        "Bends the waveform inside each period so its middle comes earlier or later (phase distortion).",
    ),
    (
        "n11.amount",
        "How strongly the waveform is bent (0 % = unchanged).",
    ),
    (
        "n11.pivot",
        "Where in the period the waveform's middle moves to.",
    ),
];

fn wave_spec() -> ToolSpec {
    let mut s = spec(
        "wave",
        "Wave",
        "Displaces the selection by the sum of wave generators (Photopea-style Wave): rows shift in time and columns in pitch; Randomize draws new generators.",
        vec![
            int("gens", "Number of generators", 1, 999, 5),
            float(
                "len_min",
                "Min. wavelength (of the box)",
                1.0,
                400.0,
                10.0,
                "%",
            ),
            float(
                "len_max",
                "Max. wavelength (of the box)",
                1.0,
                400.0,
                60.0,
                "%",
            ),
            float(
                "amp_min",
                "Min. amplitude (of the box)",
                0.0,
                100.0,
                2.0,
                "%",
            ),
            float(
                "amp_max",
                "Max. amplitude (of the box)",
                0.0,
                100.0,
                8.0,
                "%",
            ),
            float("sx", "Scale X (time displacement)", 0.0, 400.0, 100.0, "%"),
            float("sy", "Scale Y (pitch displacement)", 0.0, 400.0, 100.0, "%"),
            choice("type", "Type", WAVE_TYPES, 0),
            seed("seed"),
            adv_switch(1, "Sawtooth"),
            adv_switch(2, "Custom drawn waveform"),
            curve(
                "a2.curve",
                "One period (0.5 = center)",
                Curve::smooth(vec![
                    (0.0, 0.5),
                    (0.25, 1.0),
                    (0.5, 0.5),
                    (0.6, 0.0),
                    (1.0, 0.5),
                ]),
            )
            .overshoot()
            .adv(2),
            adv_switch(3, "Phase per generator"),
            list(
                "a3.phases",
                "Phases (degrees)",
                -360,
                360,
                64,
                &[0, 90, 180],
            )
            .adv(3),
            adv_switch(4, "Frequency sweep"),
            float(
                "a4.sweep",
                "Wavelength change across the selection",
                -90.0,
                400.0,
                100.0,
                "%",
            )
            .adv(4),
            adv_switch(5, "Amplitude sweep"),
            curve(
                "a5.curve",
                "Amplitude across the selection",
                Curve::line(0.0, 1.0),
            )
            .overshoot()
            .adv(5),
            adv_switch(6, "Interference"),
            choice("a6.mode", "Combine generators", INTERFERENCE, 1).adv(6),
            adv_switch(7, "X displacement along time"),
            adv_switch(8, "Y displacement along pitch"),
            adv_switch(9, "Vector X + Y (circular)"),
            adv_switch(10, "Subchannel phase lattice"),
            list(
                "a10.phases",
                "Phase by subchannel position (degrees)",
                -360,
                360,
                4096,
                &[0, 90, 180, 270],
            )
            .adv(10),
            // V09-S38 step 2: the functions declared by id
            // (docs/TRANSFORM_ENGINE.md "Wave").
            flag("n01", "Synced wavelength", false),
            choice("n01.unit", "Unit", WAVE_SYNC_UNITS, 0),
            float("n01.len", "Wavelength", 0.01, 10_000.0, 4.0, ""),
            flag("n02", "Wavy notes", false),
            ticks("n02.seg", "Segment length (ticks)", 1, 1 << 40, 60),
            int("n02.amp", "Amplitude (rows)", 1, 24, 2),
            ticks("n02.len", "Wavelength (ticks)", 1, 1 << 40, 480),
            flag("n03", "Phase reset", false),
            choice("n03.at", "Reset at", WAVE_RESETS, 0),
            int(
                "n03.ref",
                "Reference track (0 = the selection's onsets)",
                0,
                65_535,
                0,
            ),
            float("n03.phase", "Phase after reset", 0.0, 360.0, 0.0, "°"),
            flag("n04", "Property modulation", false),
            choice("n04.prop", "Property", WAVE_PROPS, 0),
            float("n04.depth", "Depth", 0.0, 100.0, 50.0, "%"),
            float("n04.phase", "Phase offset", 0.0, 360.0, 0.0, "°"),
            flag("n05", "Wave gate", false),
            float("n05.threshold", "Threshold", -100.0, 100.0, 0.0, "%"),
            choice("n05.keep", "Keep", WAVE_KEEP, 0),
            flag("n06", "Lissajous", false),
            int("n06.a", "X frequency", 1, 16, 1),
            int("n06.b", "Y frequency", 1, 16, 2),
            float("n06.phase", "Phase", 0.0, 360.0, 90.0, "°"),
            flag("n07", "Wavefold", false),
            choice("n07.mode", "Mode", WAVE_FOLDS, 0),
            float("n07.threshold", "Threshold", 1.0, 100.0, 50.0, "%"),
            int("n07.folds", "Folds", 1, 8, 2),
            flag("n08", "Sample and hold", false),
            ticks("n08.hold", "Hold length (ticks)", 1, 1 << 40, 240),
            float("n08.phase", "Phase", 0.0, 100.0, 0.0, "%"),
            flag("n09", "Traveling wave", false),
            float("n09.speed", "Speed (rows per beat)", -64.0, 64.0, 2.0, ""),
            flag("n10", "Track phase", false),
            float("n10.phase", "Phase per track", -360.0, 360.0, 90.0, "°"),
            choice("n10.order", "Order", STAGGER_ORDER, 0),
            flag("n11", "Phase distortion", false),
            float("n11.amount", "Distortion", 0.0, 100.0, 50.0, "%"),
            float("n11.pivot", "Pivot", 1.0, 99.0, 20.0, "%"),
        ],
        [
            "Sawtooth",
            "Custom drawn waveform",
            "Phase per generator",
            "Frequency sweep",
            "Amplitude sweep",
            "Interference",
            "X displacement along time",
            "Y displacement along pitch",
            "Vector X + Y",
            "Subchannel phase lattice",
        ],
    );
    // Orders = where each runs in `Wave9` (§235).
    s.functions = [
        ("n01", "Synced wavelength", 130),
        ("n02", "Wavy notes", 210),
        ("n03", "Phase reset", 120),
        ("n04", "Property modulation", 200),
        ("n05", "Wave gate", 190),
        ("n06", "Lissajous", 170),
        ("n07", "Wavefold", 180),
        ("n08", "Sample and hold", 150),
        ("n09", "Traveling wave", 140),
        ("n10", "Track phase", 110),
        ("n11", "Phase distortion", 160),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, WAVE_HELP);
    s
}

const WAVE_SYNC_UNITS: &[&str] = &["Beats", "Bars", "Milliseconds"];
const WAVE_RESETS: &[&str] = &["Bar", "Phrase", "Chord", "Marker", "Reference onsets"];
const WAVE_PROPS: &[&str] = &["Velocity", "Gate", "Release"];
const WAVE_KEEP: &[&str] = &["Above", "Below"];
const WAVE_FOLDS: &[&str] = &["Fold", "Full rectify", "Half rectify", "Clip"];
const WAVE_NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];

fn wave_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-0143: with Control Sweeps the v0.9 kernel evaluates them per note.
    if WAVE_NEW.iter().any(|k| p.flag(k)) || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return wave9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let n_gen = p.int("gens").clamp(1, 999) as usize;
    let (l0, l1) = {
        let a = p.float("len_min") / 100.0;
        let c = p.float("len_max") / 100.0;
        (a.min(c).max(1e-3), a.max(c).max(1e-3))
    };
    let (a0, a1) = {
        let a = p.float("amp_min") / 100.0;
        let c = p.float("amp_max") / 100.0;
        (a.min(c), a.max(c))
    };
    let (sx, sy) = (p.float("sx") / 100.0, p.float("sy") / 100.0);
    let kind = if p.adv(1) { 3 } else { p.choice("type") };
    let drawn = p.adv(2).then(|| p.curve("a2.curve"));
    let phases: Vec<f64> = if p.adv(3) {
        p.list("a3.phases")
            .iter()
            .map(|&d| d as f64 * PI / 180.0)
            .collect()
    } else {
        Vec::new()
    };
    let sweep = p.adv(4).then(|| p.float("a4.sweep") / 100.0);
    let amp_env = p.adv(5).then(|| p.curve("a5.curve"));
    let mode = if p.adv(6) { p.choice("a6.mode") } else { 0 };
    let along_x = p.adv(7);
    let along_y = p.adv(8);
    let vector = p.adv(9);
    let lattice: Vec<f64> = if p.adv(10) {
        p.list("a10.phases")
            .iter()
            .map(|&d| d as f64 * PI / 180.0)
            .collect()
    } else {
        Vec::new()
    };
    let seed = p.seed("seed");
    // Deterministic generators: (wavelength, amplitude, phase).
    let gens: Vec<(f64, f64, f64)> = (0..n_gen)
        .map(|i| {
            let mut r = super::super::rng::Rng::for_index(seed, 0x3A7E, i as u64);
            let len = l0 + (l1 - l0) * r.unit();
            let amp = a0 + (a1 - a0) * r.unit();
            let ph = phases
                .get(i % phases.len().max(1))
                .copied()
                .filter(|_| !phases.is_empty())
                .unwrap_or_else(|| r.unit() * 2.0 * PI);
            (len, amp, ph)
        })
        .collect();
    let shape = move |x: f64| -> f64 {
        // x in radians → [-1, 1].
        let t = crate::transform::geom::rem1(x / (2.0 * PI));
        if let Some(c) = &drawn {
            return (c.eval(t) - 0.5) * 2.0;
        }
        match kind {
            0 => x.sin(),
            1 => {
                if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                }
            }
            2 => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            _ => 2.0 * t - 1.0,
        }
    };
    let tracks = b.tracks.to_vec();
    let tr2 = tracks.clone();
    let combine = move |pos: f64, base_phase: f64, u: f64| -> f64 {
        let mut acc: f64 = match mode {
            2 => 1.0,
            _ => 0.0,
        };
        for &(len, amp, ph) in &gens {
            let len = match sweep {
                Some(k) => len * (1.0 + k * u.clamp(0.0, 1.0)).max(0.05),
                None => len,
            };
            let w = amp * shape(2.0 * PI * pos / len + ph + base_phase);
            acc = match mode {
                1 => {
                    if w.abs() > acc.abs() {
                        w
                    } else {
                        acc
                    }
                }
                2 => acc * (1.0 + w),
                _ => acc + w,
            };
        }
        if mode == 2 {
            acc -= 1.0;
        }
        if let Some(c) = &amp_env {
            acc *= c.eval(u.clamp(0.0, 1.0));
        }
        acc
    };
    geom_kernel(g, tracks, move |n, u, v| {
        let lp = if lattice.is_empty() {
            0.0
        } else {
            lattice[sub_pos(&tr2, n) % lattice.len()]
        };
        // Time displacement depends on the row (or on time), pitch on time (or row).
        let du = combine(if along_x { u } else { v }, lp, u);
        let dv = if vector {
            combine(if along_y { v } else { u }, lp + PI / 2.0, u)
        } else {
            combine(if along_y { v } else { u }, lp + 1.0, u)
        };
        (u + sx * du, v + sy * dv)
    })
}

// ------------------------------------------------------------ Wave (v0.9)

/// Rank of each track slot among the selection's tracks (track order, or
/// the lowest selected pitch, then track order); slots without notes get 0.
fn track_ranks(src: &Source, by_pitch: bool) -> Vec<usize> {
    let slots = src.tracks.len().max(1);
    let mut lowest = vec![u8::MAX; slots];
    let mut used = vec![false; slots];
    for n in &src.notes {
        let t = usize::from(n.track).min(slots - 1);
        used[t] = true;
        lowest[t] = lowest[t].min(n.pitch);
    }
    let mut keys: Vec<(u8, usize, usize)> = (0..slots)
        .filter(|&t| used[t])
        .map(|t| {
            let index = src.tracks.get(t).map_or(t, |x| x.index);
            (if by_pitch { lowest[t] } else { 0 }, index, t)
        })
        .collect();
    keys.sort_unstable();
    let mut rank = vec![0; slots];
    for (r, &(_, _, t)) in keys.iter().enumerate() {
        rank[t] = r;
    }
    rank
}

/// Track `ref` (≥ 1, its number in the track list) for Wave's n03
/// reference onsets: its notes in the selection's span as `Source::aux`
/// layer 0 (0 = the selection's own onsets, nothing to read).
pub fn wave_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if !(p.flag("n03") && p.choice("n03.at") == 4) || src.stats.is_empty() {
        return Ok(());
    }
    let num = p.int("n03.ref");
    if num < 1 {
        return Ok(());
    }
    let t = usize::try_from(num - 1)
        .ok()
        .filter(|&i| doc.track(i).is_some())
        .ok_or_else(|| {
            TransformError::Invalid(format!(
                "Track {num} does not exist. Choose the reference track by its number in the track list (0 = the selection's own onsets)."
            ))
        })?;
    let (a, b) = (src.stats.min_start, src.stats.max_end);
    let mut layer = Vec::new();
    // B2-F16: the reference track as the song shows it, on these ticks.
    let shift = ctx.map_or(0, |c| c.shift);
    super::super::source::placed_query(doc, t, shift, a, b, &mut |r| {
        if layer.len() < limit {
            layer.push(TNote::from_record(&r, u16::MAX));
        }
    });
    src.aux = std::sync::Arc::new(vec![layer]);
    Ok(())
}

/// The notes of a track in scope (its edited notes — an opened clip's
/// content — on the edited ticks) sounding in `a ..= b` (B2-F16: never the
/// track's raw store when a clip's content is edited).
fn scoped_query(
    doc: &crate::model::PlumDocument,
    st: &super::super::SourceTrack,
    a: u64,
    b: u64,
    f: &mut dyn FnMut(crate::model::NoteRecord),
) {
    if st.insert_only {
        return;
    }
    if let Ok(t) = crate::editor::note_view(doc, st.target()) {
        t.notes.query(a, b, |_, v| f(v.record()));
    }
}

/// One note's values of the parameters Wave's Control Sweeps drive
/// (V09-0143: evaluated per note at its exact place, inside the kernel, so
/// n03's phrases / chords / onsets and n10's track ranks see the whole
/// selection).
#[derive(Clone, Copy, Debug)]
struct WVals {
    sx: f64,
    sy: f64,
    /// n01: the wavelength in its unit.
    sync: f64,
    /// n02: rows.
    amp: f64,
    /// n04: ratio.
    depth: f64,
    /// n05: ratio of the wave's largest value.
    gate: f64,
    /// n07: ratio of the wave's largest value.
    fold: f64,
    /// n08: ticks.
    hold: f64,
    /// n09: rows per beat.
    speed: f64,
    /// n11: the knee's place (0.5 = no distortion).
    knee: f64,
}

/// What a wave read is measured along.
#[derive(Clone, Copy)]
enum WPos {
    /// Box units (the v0.8 position).
    Box(f64),
    /// n01: periods of the synced wavelength.
    Sync(f64),
}

/// The v0.9 Wave (V09-S38 step 2): the v0.8 displacement re-read from the
/// same parameters (bit-identical with every new function neutral) plus the
/// id-keyed functions in their run order: 110 n10 track phase, 120 n03
/// phase reset, 130 n01 synced wavelength, 140 n09 travelling wave, 150
/// n08 sample and hold, 160 n11 phase distortion, 170 n06 Lissajous, 180
/// n07 wavefold, 190 n05 wave gate (removes notes), 200 n04 property
/// modulation, 210 n02 wavy notes (pieces).
#[derive(Clone)]
struct Wave9 {
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    // ----- the v0.8 rule
    /// (wavelength, amplitude, phase) per generator.
    gens: Vec<(f64, f64, f64)>,
    sx: f64,
    sy: f64,
    kind: usize,
    drawn: Option<Curve>,
    sweep: Option<f64>,
    amp_env: Option<Curve>,
    mode: usize,
    along_x: bool,
    along_y: bool,
    vector: bool,
    lattice: Vec<f64>,
    // ----- v0.9 functions
    /// n01: unit (beats / bars / ms), wavelength; each generator's harmonic
    /// of it.
    sync: Option<(usize, f64)>,
    harmonics: Vec<f64>,
    /// n02: segment length (ticks), amplitude (rows), wavelength (ticks).
    wavy: Option<(u64, f64, f64)>,
    /// n03: reset at (bar / phrase / chord / marker / reference onsets),
    /// phase after a reset (radians).
    reset: Option<(usize, f64)>,
    /// n04: property (velocity / gate / release), depth, phase offset.
    modulate: Option<(usize, f64, f64)>,
    /// n05: threshold, keep below.
    gate: Option<(f64, bool)>,
    /// n06: X and Y frequency, Y phase (radians).
    lissajous: Option<(f64, f64, f64)>,
    /// n07: mode, threshold, folds.
    fold: Option<(usize, f64, u32)>,
    /// n08: hold length (ticks), phase (of the hold).
    hold: Option<(f64, f64)>,
    /// n09: rows per beat.
    travel: Option<f64>,
    /// n10: phase per track rank (radians), by lowest pitch.
    track_phase: Option<(f64, bool)>,
    /// n11: distortion, pivot (0 – 1).
    distort: Option<(f64, f64)>,
    /// The wave's largest |value| (n04, n05, n07 measure against it).
    norm: f64,
}

/// One note's context for its reads.
struct WNote<'a> {
    /// The base phase (a10's lattice + n10's track phase).
    base: f64,
    vals: &'a WVals,
}

fn wave9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    // ----- the v0.8 reading (as `wave_build`)
    let n_gen = p.int("gens").clamp(1, 999) as usize;
    let (l0, l1) = {
        let a = p.float("len_min") / 100.0;
        let c = p.float("len_max") / 100.0;
        (a.min(c).max(1e-3), a.max(c).max(1e-3))
    };
    let (a0, a1) = {
        let a = p.float("amp_min") / 100.0;
        let c = p.float("amp_max") / 100.0;
        (a.min(c), a.max(c))
    };
    let phases: Vec<f64> = if p.adv(3) {
        p.list("a3.phases")
            .iter()
            .map(|&d| d as f64 * PI / 180.0)
            .collect()
    } else {
        Vec::new()
    };
    let lattice: Vec<f64> = if p.adv(10) {
        p.list("a10.phases")
            .iter()
            .map(|&d| d as f64 * PI / 180.0)
            .collect()
    } else {
        Vec::new()
    };
    let seed = p.seed("seed");
    let gens: Vec<(f64, f64, f64)> = (0..n_gen)
        .map(|i| {
            let mut r = super::super::rng::Rng::for_index(seed, 0x3A7E, i as u64);
            let len = l0 + (l1 - l0) * r.unit();
            let amp = a0 + (a1 - a0) * r.unit();
            let ph = phases
                .get(i % phases.len().max(1))
                .copied()
                .filter(|_| !phases.is_empty())
                .unwrap_or_else(|| r.unit() * 2.0 * PI);
            (len, amp, ph)
        })
        .collect();
    let mode = if p.adv(6) { p.choice("a6.mode") } else { 0 };
    // The largest |value| the generators can reach (before a5's curve).
    let norm = match mode {
        1 => gens.iter().map(|g| g.1.abs()).fold(0.0, f64::max),
        2 => gens.iter().map(|g| 1.0 + g.1.abs()).product::<f64>() - 1.0,
        _ => gens.iter().map(|g| g.1.abs()).sum(),
    };
    // n01: generator i runs at harmonic round(longest / own) of the synced
    // wavelength (the longest at the wavelength itself), so their sum
    // repeats exactly every wavelength.
    let longest = gens.iter().map(|g| g.0).fold(0.0, f64::max);
    let harmonics = gens
        .iter()
        .map(|g| (longest / g.0).round().max(1.0))
        .collect();
    let deg = |k: &str| p.float(k) * PI / 180.0;
    let k = Wave9 {
        sx: p.float("sx") / 100.0,
        sy: p.float("sy") / 100.0,
        kind: if p.adv(1) { 3 } else { p.choice("type") },
        drawn: p.adv(2).then(|| p.curve("a2.curve")),
        sweep: p.adv(4).then(|| p.float("a4.sweep") / 100.0),
        amp_env: p.adv(5).then(|| p.curve("a5.curve")),
        mode,
        along_x: p.adv(7),
        along_y: p.adv(8),
        vector: p.adv(9),
        lattice,
        gens,
        harmonics,
        sync: p
            .flag("n01")
            .then(|| (p.choice("n01.unit").min(2), p.float("n01.len").max(0.01))),
        wavy: p.flag("n02").then(|| {
            (
                p.ticks("n02.seg").max(1),
                p.int("n02.amp").clamp(1, 24) as f64,
                p.ticks("n02.len").max(1) as f64,
            )
        }),
        reset: p
            .flag("n03")
            .then(|| (p.choice("n03.at").min(4), deg("n03.phase"))),
        modulate: p
            .flag("n04")
            .then(|| {
                let depth = p.float("n04.depth").clamp(0.0, 100.0) / 100.0;
                (depth > 0.0 || b.ctx.sweeps.is_some())
                    .then(|| (p.choice("n04.prop").min(2), depth, deg("n04.phase")))
            })
            .flatten(),
        gate: p.flag("n05").then(|| {
            (
                p.float("n05.threshold").clamp(-100.0, 100.0) / 100.0,
                p.choice("n05.keep") == 1,
            )
        }),
        lissajous: p.flag("n06").then(|| {
            (
                p.int("n06.a").clamp(1, 16) as f64,
                p.int("n06.b").clamp(1, 16) as f64,
                deg("n06.phase"),
            )
        }),
        fold: p.flag("n07").then(|| {
            (
                p.choice("n07.mode").min(3),
                p.float("n07.threshold").clamp(1.0, 100.0) / 100.0,
                p.int("n07.folds").clamp(1, 8) as u32,
            )
        }),
        hold: p.flag("n08").then(|| {
            (
                p.ticks("n08.hold").max(1) as f64,
                p.float("n08.phase").clamp(0.0, 100.0) / 100.0,
            )
        }),
        travel: p
            .flag("n09")
            .then(|| {
                let s = p.float("n09.speed");
                (s != 0.0 || b.ctx.sweeps.is_some()).then_some(s)
            })
            .flatten(),
        track_phase: p
            .flag("n10")
            .then(|| {
                let a = deg("n10.phase");
                (a != 0.0).then(|| (a, p.choice("n10.order") == 1))
            })
            .flatten(),
        distort: p
            .flag("n11")
            .then(|| {
                let d = p.float("n11.amount").clamp(0.0, 100.0) / 100.0;
                (d > 0.0 || b.ctx.sweeps.is_some())
                    .then(|| (d, p.float("n11.pivot").clamp(1.0, 99.0) / 100.0))
            })
            .flatten(),
        norm,
        ctx: b.ctx.clone(),
        g,
        tracks: b.tracks.to_vec(),
    };
    let per = k.g.per_note();
    let (growth, estimate) = match k.wavy {
        None => (
            if per > 1 {
                Growth::Expand(per)
            } else {
                Growth::Map
            },
            None,
        ),
        Some((seg, _, _)) => {
            // A note of length d becomes ⌈d / seg⌉ ≤ d / seg + 1 pieces:
            // per · (count + Σ length ÷ seg) — Σ length doubled when n04
            // scales gates (by at most 1 + 100 %).
            let gates = if matches!(k.modulate, Some((1, _, _))) {
                2
            } else {
                1
            };
            let pieces = u64::try_from(b.stats.sum_dur.saturating_mul(gates) / u128::from(seg))
                .unwrap_or(u64::MAX);
            let total = (b.stats.count as u64)
                .saturating_add(pieces)
                .saturating_mul(per);
            (Growth::Expand(per.saturating_mul(2)), Some(total))
        }
    };
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| k.run(src, out),
        growth,
        estimate,
    })
}

/// n07: `w` folded back at ±`t` (up to `folds` times, then clipped),
/// rectified or clipped.
#[inline]
fn wavefold(w: f64, mode: usize, t: f64, folds: u32) -> f64 {
    match mode {
        1 => w.abs(),
        2 => w.max(0.0),
        3 => w.clamp(-t, t),
        _ => {
            let mut w = w;
            for _ in 0..folds {
                if w > t {
                    w = 2.0 * t - w;
                } else if w < -t {
                    w = -2.0 * t - w;
                } else {
                    break;
                }
            }
            w.clamp(-t, t)
        }
    }
}

/// n11: the phase `t` (0 – 1) bent so that it reaches ½ at `knee` instead
/// of at ½ (Casio-style phase distortion).
#[inline]
fn phase_bend(t: f64, knee: f64) -> f64 {
    if t < knee {
        0.5 * t / knee
    } else {
        0.5 + 0.5 * (t - knee) / (1.0 - knee)
    }
}

impl Wave9 {
    /// The waveform at `x` radians (−1 … 1), n11's bend inside the period.
    #[inline]
    fn shape(&self, x: f64, knee: Option<f64>) -> f64 {
        let mut t = crate::transform::geom::rem1(x / (2.0 * PI));
        let mut x = x;
        if let Some(k) = knee {
            t = phase_bend(t, k);
            x = 2.0 * PI * t;
        }
        if let Some(c) = &self.drawn {
            return (c.eval(t) - 0.5) * 2.0;
        }
        match self.kind {
            0 => x.sin(),
            1 => {
                if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                }
            }
            2 => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            _ => 2.0 * t - 1.0,
        }
    }

    /// The generators combined at `pos` with `base` phase (the v0.8
    /// `combine`, plus n01's synced positions, n06's frequency factor,
    /// n11's bend and n07's fold).
    fn combine(&self, pos: WPos, base: f64, u: f64, mul: Option<f64>, w: &WVals) -> f64 {
        let knee = self.distort.map(|_| w.knee);
        let mut acc: f64 = match self.mode {
            2 => 1.0,
            _ => 0.0,
        };
        for (i, &(len, amp, ph)) in self.gens.iter().enumerate() {
            let arg = match pos {
                WPos::Box(pos) => {
                    let len = match self.sweep {
                        Some(k) => len * (1.0 + k * u.clamp(0.0, 1.0)).max(0.05),
                        None => len,
                    };
                    2.0 * PI * pos / len
                }
                WPos::Sync(periods) => {
                    let f = match self.sweep {
                        Some(k) => (1.0 + k * u.clamp(0.0, 1.0)).max(0.05),
                        None => 1.0,
                    };
                    2.0 * PI * periods * self.harmonics[i] / f
                }
            };
            let arg = match mul {
                Some(m) => arg * m,
                None => arg,
            };
            let wv = amp * self.shape(arg + ph + base, knee);
            acc = match self.mode {
                1 => {
                    if wv.abs() > acc.abs() {
                        wv
                    } else {
                        acc
                    }
                }
                2 => acc * (1.0 + wv),
                _ => acc + wv,
            };
        }
        if self.mode == 2 {
            acc -= 1.0;
        }
        if let Some(c) = &self.amp_env {
            acc *= c.eval(u.clamp(0.0, 1.0));
        }
        if let Some((mode, _, folds)) = self.fold {
            acc = wavefold(acc, mode, w.fold * self.norm, folds);
        }
        acc
    }

    /// The song position of tick `x` in n01's unit (beats, bars or
    /// milliseconds through the tempo map), real-valued.
    fn sync_units(&self, unit: usize, x: f64, tt: &std::cell::RefCell<TempoTime<'_>>) -> f64 {
        let x = x.max(0.0);
        match unit {
            0 => x / self.ctx.ppq.max(1) as f64,
            1 => {
                let tick = (x + 1e-9).floor();
                let (bar, start, len) = super::super::shared::metric_position::bar_place(
                    &self.ctx.meter,
                    tick.min(u64::MAX as f64) as u64,
                );
                bar as f64 + (x - start as f64) / len.max(1) as f64
            }
            _ => {
                // (`as` saturates; keep room for the next tick.)
                let tick = (x.floor() as u64).min(u64::MAX - 1);
                let mut tt = tt.borrow_mut();
                let (m0, m1) = (tt.micros_f(tick), tt.micros_f(tick + 1));
                (m0 + (x - tick as f64) * (m1 - m0)) / 1000.0
            }
        }
    }

    /// The last reset point at or before tick `x` (n03), if any.
    fn reset_at(&self, x: f64, points: &[u64]) -> Option<f64> {
        let (at, _) = self.reset?;
        if at == 0 {
            return Some(bar_bounds(&self.ctx.meter, (x + 1e-9).max(0.0).floor() as u64).0 as f64);
        }
        let k = points.partition_point(|&p| p as f64 <= x + 1e-9);
        (k > 0).then(|| points[k - 1] as f64)
    }

    /// One displacement read: along time (`time`) or pitch at the point
    /// `(u, v)` (`ut` = the held time), with `base` phase and n06's factor.
    #[allow(clippy::too_many_arguments)]
    fn read(
        &self,
        time: bool,
        u: f64,
        v: f64,
        ut: f64,
        base: f64,
        mul: Option<f64>,
        nw: &WNote,
        ctx: &WRun<'_>,
    ) -> f64 {
        let b = &self.g.bbox;
        let w = nw.vals;
        let env_u = if self.hold.is_some() { ut } else { u };
        if !time {
            let mut pos = v;
            if self.travel.is_some() {
                // n09: the pattern along pitch moves `speed` rows a beat.
                let beats = ut * b.width() / self.ctx.ppq.max(1) as f64;
                pos = v - w.speed * beats / b.rows();
            }
            return self.combine(WPos::Box(pos), base, env_u, mul, w);
        }
        if self.hold.is_none() && self.reset.is_none() && self.sync.is_none() {
            return self.combine(WPos::Box(u), base, env_u, mul, w);
        }
        let x = b.t0 as f64 + ut * b.width();
        let r = self.reset_at(x, &ctx.resets);
        let base = match (r, self.reset) {
            (Some(_), Some((_, ph))) => base + ph,
            _ => base,
        };
        let pos = match self.sync {
            Some((unit, _)) => {
                let q = self.sync_units(unit, x, &ctx.tt)
                    - r.map_or(0.0, |r| self.sync_units(unit, r, &ctx.tt));
                WPos::Sync(q / w.sync)
            }
            None => match r {
                Some(r) => WPos::Box((x - r) / b.width()),
                None => WPos::Box(ut),
            },
        };
        self.combine(pos, base, env_u, mul, w)
    }

    /// n08: the time `u` held at the start of its hold cell.
    #[inline]
    fn held(&self, u: f64, w: &WVals) -> f64 {
        let Some((_, phase)) = self.hold else {
            return u;
        };
        let b = &self.g.bbox;
        let (t0, wd) = (b.t0 as f64, b.width());
        let x = t0 + u * wd;
        let h = w.hold.max(1.0);
        let off = phase * h;
        let xq = t0 + off + ((x - t0 - off) / h + 1e-9).floor() * h;
        (xq - t0) / wd
    }

    /// The time read's (dv) phase offset: v0.8's 1 rad (π/2 with a9), or
    /// n06's Y phase.
    #[inline]
    fn dv_phase(&self) -> f64 {
        match self.lissajous {
            Some((_, _, ph)) => ph,
            None if self.vector => PI / 2.0,
            None => 1.0,
        }
    }

    /// The displaced point `(u, v)` of a note (the v0.8 map with the v0.9
    /// terms).
    #[inline]
    fn point(&self, u: f64, v: f64, nw: &WNote, ctx: &WRun<'_>) -> (f64, f64) {
        let ut = self.held(u, nw.vals);
        let (mx, my) = match self.lissajous {
            Some((a, b, _)) => (Some(a), Some(b)),
            None => (None, None),
        };
        let du = self.read(self.along_x, u, v, ut, nw.base, mx, nw, ctx);
        let dv = self.read(
            !self.along_y,
            u,
            v,
            ut,
            nw.base + self.dv_phase(),
            my,
            nw,
            ctx,
        );
        (u + nw.vals.sx * du, v + nw.vals.sy * dv)
    }

    /// The wave's value at note `n`'s start (the pitch displacement's read,
    /// −1 … 1 of its largest value) with `extra` phase — n04 and n05.
    fn value_at(&self, n: &TNote, nw: &WNote, ctx: &WRun<'_>, extra: f64) -> f64 {
        if self.norm <= 0.0 {
            return 0.0;
        }
        let b = &self.g.bbox;
        let (u, v) = b.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
        let ut = self.held(u, nw.vals);
        let my = self.lissajous.map(|l| l.1);
        let w = self.read(
            !self.along_y,
            u,
            v,
            ut,
            nw.base + self.dv_phase() + extra,
            my,
            nw,
            ctx,
        );
        (w / self.norm).clamp(-1.0, 1.0)
    }

    /// Every note's swept values (one shared set without sweeps).
    fn vals(&self, src: &Source) -> (WVals, Vec<WVals>) {
        let knee = |d: f64, pivot: f64| 0.5 + d.clamp(0.0, 1.0) * (pivot - 0.5);
        let pivot = self.distort.map_or(0.5, |d| d.1);
        let base = WVals {
            sx: self.sx,
            sy: self.sy,
            sync: self.sync.map_or(1.0, |s| s.1),
            amp: self.wavy.map_or(0.0, |w| w.1),
            depth: self.modulate.map_or(0.0, |m| m.1),
            gate: self.gate.map_or(-1.0, |g| g.0),
            fold: self.fold.map_or(1.0, |f| f.1),
            hold: self.hold.map_or(1.0, |h| h.0),
            speed: self.travel.unwrap_or(0.0),
            knee: knee(self.distort.map_or(0.0, |d| d.0), pivot),
        };
        let Some(sw) = self.ctx.sweeps.as_deref().filter(|s| !s.is_empty()) else {
            return (base, Vec::new());
        };
        let places = super::super::sweep::note_places(src);
        let per = src
            .notes
            .iter()
            .zip(places)
            .map(|(n, place)| {
                // Percent parameters are swept in %, kept as ratios.
                let pct = |key: &str, v: f64| {
                    if sw.drives(key) {
                        sw.float(key, v * 100.0, n, place) / 100.0
                    } else {
                        v
                    }
                };
                let fl = |key: &str, v: f64| {
                    if sw.drives(key) {
                        sw.float(key, v, n, place)
                    } else {
                        v
                    }
                };
                let d = self.distort.map_or(0.0, |d| d.0);
                WVals {
                    sx: pct("sx", base.sx),
                    sy: pct("sy", base.sy),
                    sync: fl("n01.len", base.sync).max(0.01),
                    amp: if sw.drives("n02.amp") {
                        sw.int("n02.amp", base.amp as i64, n, place).clamp(1, 24) as f64
                    } else {
                        base.amp
                    },
                    depth: pct("n04.depth", base.depth),
                    gate: pct("n05.threshold", base.gate),
                    fold: pct("n07.threshold", base.fold).max(0.01),
                    hold: if sw.drives("n08.hold") {
                        sw.int("n08.hold", base.hold as i64, n, place).max(1) as f64
                    } else {
                        base.hold
                    },
                    speed: fl("n09.speed", base.speed),
                    knee: knee(pct("n11.amount", d), pivot),
                }
            })
            .collect();
        (base, per)
    }

    /// n03's reset points (sorted ticks): phrase starts, chord onsets,
    /// markers or reference onsets (bars are looked up per point).
    fn resets(&self, src: &Source) -> Vec<u64> {
        let Some((at, _)) = self.reset else {
            return Vec::new();
        };
        let ns = &src.notes;
        let mut idx: Vec<usize> = (0..ns.len()).collect();
        idx.sort_by_key(|&i| (ns[i].start, i));
        let mut v: Vec<u64> = match at {
            1 => Phrases::of(ns, &idx, self.ctx.ppq.max(1), None)
                .iter(&idx)
                .map(|ph| ns[ph[0]].start)
                .collect(),
            2 => ChordGroups::of(ns, &idx, (self.ctx.ppq / 96).max(1))
                .iter()
                .map(|c| c.iter().map(|&i| ns[i].start).min().unwrap_or(0))
                .collect(),
            3 => self.ctx.markers.to_vec(),
            4 => match src.aux.first() {
                Some(layer) => layer.iter().map(|n| n.start).collect(),
                None => ns.iter().map(|n| n.start).collect(),
            },
            _ => Vec::new(),
        };
        v.sort_unstable();
        v.dedup();
        v
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let b = self.g.bbox;
        let (base_vals, per_vals) = self.vals(src);
        let ranks = match self.track_phase {
            Some((_, by_pitch)) => track_ranks(src, by_pitch),
            None => Vec::new(),
        };
        let ctx = WRun {
            resets: self.resets(src),
            tt: std::cell::RefCell::new(TempoTime::new(&self.ctx)),
        };
        for (i, n) in src.notes.iter().enumerate() {
            let vals = per_vals.get(i).unwrap_or(&base_vals);
            let mut base = if self.lattice.is_empty() {
                0.0
            } else {
                self.lattice[sub_pos(&self.tracks, n) % self.lattice.len()]
            };
            if let Some((ph, _)) = self.track_phase {
                let r = ranks.get(usize::from(n.track)).copied().unwrap_or(0);
                base += ph * r as f64;
            }
            let nw = WNote { base, vals };
            if let Some((_, below)) = self.gate {
                let w = self.value_at(n, &nw, &ctx, 0.0);
                let keep = if below {
                    w <= vals.gate
                } else {
                    w >= vals.gate
                };
                if !keep {
                    continue;
                }
            }
            let mut t = *n;
            if let Some((prop, _, ph)) = self.modulate {
                let f = 1.0 + vals.depth * self.value_at(n, &nw, &ctx, ph);
                match prop {
                    0 => t.vel = (f64::from(n.vel) * f).round().clamp(1.0, 127.0) as u8,
                    1 => t.dur = ((n.dur as f64 * f).round() as u64).max(1),
                    _ => t.rel = (f64::from(n.rel) * f).round().clamp(0.0, 127.0) as u8,
                }
            }
            let ok = match self.wavy {
                None => {
                    self.g
                        .rasterize(&t, &|u, v| self.point(u, v, &nw, &ctx), &self.tracks, out)
                }
                Some((seg, _, len)) => {
                    // n02: pieces of `seg` ticks, each `amp` · wave rows off
                    // (the wave along the note from its start), then waved.
                    let mut ok = true;
                    let mut at = 0u64;
                    while at < t.dur && ok {
                        let d = seg.min(t.dur - at);
                        let off = vals.amp
                            * self
                                .shape(2.0 * PI * at as f64 / len, self.distort.map(|_| vals.knee));
                        let dv = off.round() / b.rows();
                        let piece = TNote {
                            start: t.start.saturating_add(at),
                            dur: d,
                            ..t
                        };
                        ok = self.g.rasterize(
                            &piece,
                            &|u, v| self.point(u, v + dv, &nw, &ctx),
                            &self.tracks,
                            out,
                        );
                        at += d;
                    }
                    ok
                }
            };
            if !ok {
                return out.check();
            }
            out.check()?;
        }
        Ok(())
    }
}

/// Per-run lookups of a Wave run (n03's reset points, the tempo map).
struct WRun<'c> {
    resets: Vec<u64>,
    tt: std::cell::RefCell<TempoTime<'c>>,
}
