//! Note Outline (V09-S46, contract §136 – §138): notes tracing the boundary
//! of the selected note art, computed **exactly** on row intervals
//! ([`RowMask`]: the art's sorted tick intervals per pitch row) — never note
//! against note and never on a capped cell grid (Contour Trace's v0.8 grid is
//! the coarse, cell-based counterpart).
//!
//! Per track slot, with a cell of Resolution ticks × one row:
//!
//! * the art A is the union of the notes' rectangles (optionally quantized
//!   outward to the cell grid), simplified when asked (a closing by a box of
//!   the Simplification: gaps, holes and notches up to about twice its size
//!   are bridged, so a mosaic of short notes is traced as one silhouette —
//!   art is never removed);
//! * **Outer** = D(A, offset + thickness) − D(A, offset), **Inner** =
//!   E(A, offset) − E(A, offset + thickness), **Both** their union — D / E
//!   dilation and erosion by a box (square corners) or a diamond (cut
//!   corners) of Thickness along time (cells) and across pitch (rows),
//!   `spatial::Shape`; the keyboard's ends and the time before the art are
//!   outside;
//! * Cut before the first note / after the last clips the outline to the
//!   art's span; the art is kept (carved where an inner outline lies, so no
//!   two notes overlap) or dropped;
//! * each outline interval becomes a note (or dashes on an absolute
//!   dash + gap grid) with the attributes of the **nearest art note**
//!   (rows × cell + the time gap; a search widening row by row), its
//!   velocity scaled or fixed, a subchannel step, a delay and a gate.
//!
//! Control Sweeps act per art note: thickness and offset by the note the
//! outline grows from (the outline is the union of each level's band minus
//! the whole art's inner part, so no edges appear between levels), the
//! attributes by each piece's nearest note.
//!
//! **Preflight (§138)**: the masks cost O(intervals · rows reached); the
//! loose bound (intervals × the rows a band reaches, dashes over the span) is
//! used while under the warning size, else the output is counted exactly
//! from the masks before anything is emitted.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::super::param::*;
use super::super::preflight::{Estimate, Growth};
use super::super::registry::{BuildCtx, Family, ToolDef, ToolSpec};
use super::super::spatial::{RowMask, Shape};
use super::super::sweep::note_places;
use super::super::{Ctx, Kernel, Limits, Output, Source, SourceTrack, TNote, TransformError};
use super::xgeom::{sub_stepped, vel_scaled};
use super::xgrid::{by_slot, span_of};
use super::{NOTE_VALUES, boxed, note_value};

pub const NOTE_OUTLINE: ToolDef = ToolDef {
    id: "note_outline",
    spec: outline_spec,
    build: outline_build,
};

/// The outline modes (§137).
pub const MODES: &[&str] = &["Outer Outline", "Inner Outline", "Both"];
const CORNERS: &[&str] = &["Square corners (box)", "Cut corners (diamond)"];
const VELS: &[&str] = &["From the nearest art note (scaled)", "Fixed"];
/// Most different (thickness, offset) levels Control Sweeps may give one
/// slot (each costs a dilation / erosion of the art).
pub const MAX_LEVELS: usize = 64;
const MAX_THICK_X: i64 = 64;
const MAX_THICK_Y: i64 = 16;
const MAX_OFFSET: i64 = 64;
const MAX_SIMPLIFY: i64 = 32;

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const NOTE_OUTLINE_HELP: &[(&str, &str)] = &[
    (
        "mode",
        "Where the outline goes: around the outside of the note art, along the inside of its edge, or both.",
    ),
    (
        "thick_x",
        "How thick the outline is along time, in cells (0 = no thickness that way).",
    ),
    (
        "thick_y",
        "How thick the outline is across pitch, in rows (0 = no thickness that way).",
    ),
    (
        "offset",
        "How far the outline stays away from the art, in cells and rows.",
    ),
    (
        "res",
        "The length of one cell in time; the outline is built on this grid (Snap uses the Piano Roll's snap).",
    ),
    (
        "res.ticks",
        "The cell length in ticks, used when Resolution is set to Custom Ticks.",
    ),
    (
        "quantize",
        "Snaps the art to the cell grid before tracing it.",
    ),
    (
        "corners",
        "Square Corners keeps the outline's corners full; Cut Corners rounds them off like a diamond.",
    ),
    (
        "simplify",
        "Smooths away gaps and notches up to this size, in cells, before tracing.",
    ),
    (
        "cut_start",
        "Leaves out the part of the outline before the first note.",
    ),
    (
        "cut_end",
        "Leaves out the part of the outline after the last note.",
    ),
    (
        "keep",
        "Keeps the note art itself (with an inner outline carved out of it); off leaves only the outline.",
    ),
    (
        "vel.mode",
        "The outline's velocity: taken from the nearest art note and scaled, or one fixed value.",
    ),
    (
        "vel.scale",
        "The outline's velocity as a share of the nearest art note's.",
    ),
    (
        "vel",
        "The velocity of every outline note, used with Fixed.",
    ),
    (
        "sub_step",
        "How many subchannel positions the outline moves from the art (0 = the same color).",
    ),
    ("delay", "Moves the outline later in time, in ticks."),
    (
        "gate",
        "The outline notes' length as a share of their cell.",
    ),
    (
        "dash",
        "Breaks the outline into dashes this long, in ticks (0 = a solid outline).",
    ),
    ("gap", "The length of the gaps between dashes, in ticks."),
];

fn outline_spec() -> ToolSpec {
    let mut p = vec![
        choice("mode", "Outline", MODES, 0),
        int("thick_x", "Thickness along time (cells)", 0, MAX_THICK_X, 1),
        int(
            "thick_y",
            "Thickness across pitch (rows)",
            0,
            MAX_THICK_Y,
            1,
        ),
        int(
            "offset",
            "Offset from the art (cells and rows)",
            0,
            MAX_OFFSET,
            0,
        ),
        choice("res", "Resolution (one cell)", NOTE_VALUES, 10),
        ticks("res.ticks", "Custom cell (ticks)", 1, 1 << 24, 120),
        flag(
            "quantize",
            "Quantize the art to the resolution first",
            false,
        ),
        choice("corners", "Corners", CORNERS, 0),
        int("simplify", "Simplification (cells)", 0, MAX_SIMPLIFY, 0),
        flag("cut_start", "Cut the outline before the first note", false),
        flag("cut_end", "Cut the outline after the last note", false),
        flag(
            "keep",
            "Keep the art (carved where an inner outline lies)",
            true,
        ),
        choice("vel.mode", "Outline velocity", VELS, 0),
        float(
            "vel.scale",
            "Velocity (of the nearest note)",
            1.0,
            200.0,
            100.0,
            "%",
        ),
        int("vel", "Fixed velocity", 1, 127, 100),
        int("sub_step", "Subchannel steps (outline)", -64, 64, 0),
        ticks("delay", "Outline delay", 0, 1 << 40, 0),
        float("gate", "Outline gate", 1.0, 100.0, 100.0, "%"),
        ticks("dash", "Dash (0: solid)", 0, 1 << 24, 0),
        ticks("gap", "Dash gap", 0, 1 << 24, 120),
    ];
    with_help(&mut p, NOTE_OUTLINE_HELP);
    ToolSpec {
        id: "note_outline",
        name: "Note Outline",
        family: Family::Geometry,
        summary: "Adds notes tracing the outline of the selected note art — outside it, inside its edge or both — with a thickness, an offset, simplification and a resolution, exactly on the notes' edges.",
        params: p,
        advanced: Vec::new(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// One art note's band geometry (cells along time, rows, cells and rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Geo {
    tx: u32,
    ty: u32,
    off: u32,
}

/// One piece's attributes.
#[derive(Clone, Copy, Debug)]
struct Attr {
    vel_scale: f64,
    vel: i64,
    sub: i64,
    delay: u64,
    gate: f64,
    dash: u64,
    gap: u64,
}

/// What [`Outline::visit`] hands its sink: a kept piece of an art note, or
/// an outline piece (row, start, end, its nearest note, its attributes).
enum Ev<'a> {
    Keep(&'a TNote, u64, u64),
    Piece(usize, u64, u64, &'a TNote, Attr),
}

/// A slot's outline (and the inner part to carve from the kept art), in
/// absolute ticks.
#[derive(Debug)]
struct SlotPlan {
    idx: Vec<usize>,
    outline: RowMask,
    inner: RowMask,
}

#[derive(Debug)]
struct Outline {
    outer: bool,
    inner: bool,
    geo: Geo,
    cell: u64,
    diamond: bool,
    simplify: u32,
    quantize: bool,
    cut_start: bool,
    cut_end: bool,
    keep: bool,
    fixed_vel: bool,
    attr: Attr,
    tracks: Vec<SourceTrack>,
    ctx: Ctx,
    growth: Growth,
    /// The plan an exact count made, for the run that follows it on the
    /// same notes (by their fingerprint); the run takes it, so its memory
    /// is freed when the run ends.
    cache: Mutex<Option<(u64, Vec<SlotPlan>)>>,
}

/// A fingerprint of the source notes (FNV-1a over each note's place and
/// slot): the cached plan is reused only for the very same notes.
fn fingerprint(src: &Source) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    eat(src.notes.len() as u64);
    for n in &src.notes {
        eat(n.start);
        eat(n.dur);
        eat(u64::from(n.pitch) | u64::from(n.track) << 16);
    }
    h
}

/// Dashes of `[a, b)` on the absolute grid of `dash` ticks every
/// `dash + gap`: (first period, last period), none when empty.
fn dash_periods(a: u64, b: u64, dash: u64, gap: u64) -> Option<(u64, u64)> {
    let period = dash.saturating_add(gap).max(1);
    if b <= a || dash == 0 {
        return None;
    }
    let first = a / period + u64::from(a % period >= dash);
    let last = (b - 1) / period;
    (first <= last).then_some((first, last))
}

/// Pieces of `[s, e)` outside the sorted intervals `row`.
fn outside(row: &[(u64, u64)], s: u64, e: u64, mut f: impl FnMut(u64, u64) -> bool) -> bool {
    let mut at = s;
    let i = row.partition_point(|x| x.1 <= s);
    for &(a, b) in &row[i..] {
        if a >= e {
            break;
        }
        if a > at && !f(at, a) {
            return false;
        }
        at = at.max(b);
        if at >= e {
            return true;
        }
    }
    if at < e { f(at, e) } else { true }
}

impl Outline {
    fn shape(&self, x: u32, y: u32) -> Shape {
        Shape {
            kx: u64::from(x).saturating_mul(self.cell),
            ky: y,
            diamond: self.diamond,
        }
    }

    /// Every note's band geometry (the base without Control Sweeps on it).
    fn geos(&self, src: &Source) -> Vec<Geo> {
        let sw = self
            .ctx
            .sweeps
            .as_deref()
            .filter(|s| s.drives("thick_x") || s.drives("thick_y") || s.drives("offset"));
        let Some(sw) = sw else {
            return vec![self.geo; src.notes.len()];
        };
        let places = note_places(src);
        let g = self.geo;
        src.notes
            .iter()
            .zip(&places)
            .map(|(n, &pl)| Geo {
                tx: sw
                    .int("thick_x", i64::from(g.tx), n, pl)
                    .clamp(0, MAX_THICK_X) as u32,
                ty: sw
                    .int("thick_y", i64::from(g.ty), n, pl)
                    .clamp(0, MAX_THICK_Y) as u32,
                off: sw
                    .int("offset", i64::from(g.off), n, pl)
                    .clamp(0, MAX_OFFSET) as u32,
            })
            .collect()
    }

    /// The outline of every slot.
    fn plan(&self, src: &Source) -> Result<Vec<SlotPlan>, TransformError> {
        let geos = self.geos(src);
        let mut plans = Vec::new();
        for (_, idx) in by_slot(src) {
            let (outline, inner) = self.slot(src, &idx, &geos)?;
            plans.push(SlotPlan {
                idx,
                outline,
                inner,
            });
        }
        Ok(plans)
    }

    /// One slot's outline and inner part (absolute ticks).
    fn slot(
        &self,
        src: &Source,
        idx: &[usize],
        geos: &[Geo],
    ) -> Result<(RowMask, RowMask), TransformError> {
        let keys = src.keys;
        let cell = self.cell;
        let mut levels: BTreeMap<Geo, Vec<usize>> = BTreeMap::new();
        for &i in idx {
            levels.entry(geos[i]).or_default().push(i);
        }
        if levels.len() > MAX_LEVELS {
            return Err(TransformError::Invalid(format!(
                "The Control Sweeps give the outline {} different thickness / offset levels (at most {MAX_LEVELS}). Use fewer sweep levels.",
                levels.len()
            )));
        }
        let reach = levels
            .keys()
            .map(|g| g.off + g.tx.max(g.ty))
            .max()
            .unwrap_or(0)
            + self.simplify;
        // Work shifted right of tick 0, so the outside before the art exists.
        let pad = u64::from(reach)
            .saturating_add(1)
            .saturating_mul(cell)
            .saturating_add(1);
        let rect = |n: &TNote| {
            let (s, e) = if self.quantize {
                (
                    n.start / cell * cell,
                    n.end().div_ceil(cell).saturating_mul(cell),
                )
            } else {
                (n.start, n.end())
            };
            (s.saturating_add(pad), e.saturating_add(pad))
        };
        let mask_of = |ids: &[usize]| {
            let mut rows = vec![Vec::new(); keys.keys()];
            for &i in ids {
                let n = &src.notes[i];
                if let Some(r) = rows.get_mut(usize::from(n.pitch)) {
                    r.push(rect(n));
                }
            }
            RowMask::from_rows(rows)
        };
        let mut art = mask_of(idx);
        let Some((_, end)) = art.extent() else {
            return Ok((RowMask::empty(keys), RowMask::empty(keys)));
        };
        let hi = end.saturating_add(pad);
        let simple = Shape {
            kx: u64::from(self.simplify).saturating_mul(cell),
            ky: self.simplify,
            diamond: false,
        };
        if self.simplify > 0 {
            // ∪ the art: at the keyboard's ends the erosion sees the outside
            // rows, and a closing never removes art.
            art = art.dilate(simple).erode(simple, 0, hi).union(&art);
        }
        let single = levels.len() == 1;
        let mut outline = RowMask::empty(keys);
        let mut inner = RowMask::empty(keys);
        for (g, ids) in &levels {
            // The art this level grows from (all of it for one level).
            let own = if single {
                art.clone()
            } else if self.simplify > 0 {
                mask_of(ids).dilate(simple).intersect(&art)
            } else {
                mask_of(ids)
            };
            let full = self.shape(g.off + g.tx, g.off + g.ty);
            let gap = self.shape(g.off, g.off);
            if self.outer {
                let near = if g.off == 0 {
                    art.clone()
                } else {
                    art.dilate(gap)
                };
                let band = own.dilate(full).subtract(&near, 0, hi);
                outline = outline.union(&band);
            }
            if self.inner {
                let e0 = if g.off == 0 {
                    art.clone()
                } else {
                    art.erode(gap, 0, hi)
                };
                let band = own.intersect(&e0).subtract(&art.erode(full, 0, hi), 0, hi);
                inner = inner.union(&band);
            }
        }
        if self.cut_start || self.cut_end {
            let (t0, t1) = span_of(&src.notes, idx);
            let lo = if self.cut_start {
                t0.saturating_add(pad)
            } else {
                0
            };
            let up = if self.cut_end {
                t1.saturating_add(pad)
            } else {
                u64::MAX
            };
            outline = outline.clip_time(lo, up);
            inner = inner.clip_time(lo, up);
        }
        let outline = outline.union(&inner).shift_back(pad);
        Ok((outline, inner.shift_back(pad)))
    }

    /// The art note nearest to (row `p`, tick `t`): rows away × the cell plus
    /// the time gap (0 inside a note); ties by fewer rows, then the earlier
    /// note. `rows` holds each row's notes in start order.
    fn nearest(&self, notes: &[TNote], rows: &[Vec<usize>], p: usize, t: u64) -> usize {
        let mut best: Option<(u64, usize, usize)> = None;
        for r in 0..rows.len() {
            let rd = (r as u64).saturating_mul(self.cell);
            if best.is_some_and(|b| rd > b.0) {
                break;
            }
            let up = (r > 0).then(|| p + r).filter(|&q| q < rows.len());
            for q in [p.checked_sub(r), up].into_iter().flatten() {
                let row = &rows[q];
                let i = row.partition_point(|&k| notes[k].start <= t);
                for &k in &row[i.saturating_sub(4)..(i + 1).min(row.len())] {
                    let n = &notes[k];
                    let gap = n
                        .start
                        .saturating_sub(t)
                        .saturating_add(t.saturating_sub(n.end().saturating_sub(1)));
                    let c = (rd.saturating_add(gap), r, k);
                    if best.is_none_or(|b| c < b) {
                        best = Some(c);
                    }
                }
            }
        }
        best.map_or(0, |b| b.2)
    }

    /// A piece's attributes from its nearest note (the swept ones there).
    fn attr(&self, n: &TNote, place: Option<super::super::drivers::Place>) -> Attr {
        let mut a = self.attr;
        let (Some(sw), Some(pl)) = (self.ctx.sweeps.as_deref(), place) else {
            return a;
        };
        if sw.drives("vel.scale") {
            a.vel_scale = sw
                .float("vel.scale", a.vel_scale * 100.0, n, pl)
                .clamp(1.0, 200.0)
                / 100.0;
        }
        if sw.drives("vel") {
            a.vel = sw.int("vel", a.vel, n, pl).clamp(1, 127);
        }
        if sw.drives("sub_step") {
            a.sub = sw.int("sub_step", a.sub, n, pl).clamp(-64, 64);
        }
        if sw.drives("delay") {
            a.delay = sw
                .int("delay", a.delay.min(i64::MAX as u64) as i64, n, pl)
                .max(0) as u64;
        }
        if sw.drives("gate") {
            a.gate = sw.float("gate", a.gate * 100.0, n, pl).clamp(1.0, 100.0) / 100.0;
        }
        if sw.drives("dash") {
            a.dash = sw
                .int("dash", a.dash.min(i64::MAX as u64) as i64, n, pl)
                .max(0) as u64;
        }
        if sw.drives("gap") {
            a.gap = sw
                .int("gap", a.gap.min(i64::MAX as u64) as i64, n, pl)
                .max(0) as u64;
        }
        a
    }

    fn rows_of(&self, src: &Source, idx: &[usize]) -> Vec<Vec<usize>> {
        let mut rows = vec![Vec::new(); src.keys.keys()];
        for &i in idx {
            if let Some(r) = rows.get_mut(usize::from(src.notes[i].pitch)) {
                r.push(i);
            }
        }
        rows
    }

    /// Visits every note the plan makes: the kept art's pieces, then each
    /// outline piece (row, start, length, the nearest note, its attributes).
    /// Stops when `f` returns false.
    fn visit<'a>(&self, src: &'a Source, plans: &[SlotPlan], mut f: impl FnMut(Ev<'a>) -> bool) {
        let places = self
            .ctx
            .sweeps
            .as_deref()
            .is_some_and(|s| !s.is_empty())
            .then(|| note_places(src));
        for plan in plans {
            if self.keep {
                for &i in &plan.idx {
                    let n = &src.notes[i];
                    if !outside(plan.inner.row(n.pitch), n.start, n.end(), |a, b| {
                        f(Ev::Keep(n, a, b))
                    }) {
                        return;
                    }
                }
            }
            if plan.outline.is_empty() {
                continue;
            }
            let rows = self.rows_of(src, &plan.idx);
            for (p, row) in plan.outline.rows() {
                for &(a, b) in row {
                    let k = self.nearest(&src.notes, &rows, p, a);
                    let n = &src.notes[k];
                    let at = self.attr(n, places.as_ref().map(|v| v[k]));
                    if at.dash == 0 {
                        if !f(Ev::Piece(p, a, b, n, at)) {
                            return;
                        }
                    } else if let Some((first, last)) = dash_periods(a, b, at.dash, at.gap) {
                        let period = at.dash.saturating_add(at.gap).max(1);
                        for k in first..=last {
                            let s = k.saturating_mul(period).max(a);
                            let e = k.saturating_mul(period).saturating_add(at.dash).min(b);
                            if s < e && !f(Ev::Piece(p, s, e, n, at)) {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    /// The notes the plan makes (counted, nothing emitted).
    fn count(&self, src: &Source, plans: &[SlotPlan]) -> u64 {
        let mut total = 0u64;
        let places = self
            .ctx
            .sweeps
            .as_deref()
            .is_some_and(|s| !s.is_empty())
            .then(|| note_places(src));
        for plan in plans {
            if self.keep {
                for &i in &plan.idx {
                    let n = &src.notes[i];
                    outside(plan.inner.row(n.pitch), n.start, n.end(), |_, _| {
                        total += 1;
                        true
                    });
                }
            }
            if plan.outline.is_empty() {
                continue;
            }
            let rows = self.rows_of(src, &plan.idx);
            for (p, row) in plan.outline.rows() {
                for &(a, b) in row {
                    let at = if places.is_some() {
                        let k = self.nearest(&src.notes, &rows, p, a);
                        self.attr(&src.notes[k], places.as_ref().map(|v| v[k]))
                    } else {
                        self.attr
                    };
                    total = total.saturating_add(if at.dash == 0 {
                        1
                    } else {
                        dash_periods(a, b, at.dash, at.gap).map_or(0, |(f, l)| l - f + 1)
                    });
                }
            }
        }
        total
    }

    /// A loose bound of the output from the note count and span alone.
    fn loose(
        &self,
        n: u64,
        span: u64,
        keys: u64,
        reach: (u64, u64),
        levels: u64,
        dash: Option<u64>,
    ) -> u64 {
        let (ry, s) = reach;
        // Band intervals: each row's intervals come from the art intervals
        // (and the complement's) within the rows reached.
        let rows_reached = 2 * ry + 1;
        // Each of the simplification's two passes (the closing) can split
        // intervals by the rows it reaches.
        let simp = (2 * s + 1).saturating_pow(2);
        let bands = n
            .saturating_add(4 * keys)
            .saturating_mul(rows_reached)
            .saturating_mul(simp)
            .saturating_mul(4)
            .saturating_mul(levels)
            .saturating_add(4 * keys);
        let kept = if self.keep {
            n.saturating_add(if self.inner { bands } else { 0 })
        } else {
            0
        };
        let dashes = dash.map_or(0, |period| {
            let reach_x = (ry + s + 1).saturating_mul(self.cell).saturating_mul(2);
            keys.saturating_mul(span.saturating_add(reach_x) / period.max(1) + 1)
        });
        kept.saturating_add(bands).saturating_add(dashes)
    }
}

impl Kernel for Outline {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        let n = src.notes.len() as u64;
        let loose = match self.growth {
            Growth::Generate(m) => m,
            _ => u64::MAX,
        };
        if loose <= Limits::default().warn_notes {
            return Estimate::for_notes(n, loose);
        }
        // Counted exactly from the masks (refusals come back from `run`);
        // the run that follows reuses them.
        match self.plan(src) {
            Ok(plans) => {
                let count = self.count(src, &plans).min(loose);
                if let Ok(mut c) = self.cache.lock() {
                    *c = Some((fingerprint(src), plans));
                }
                Estimate::for_notes(n, count)
            }
            Err(_) => Estimate::for_notes(n, n),
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let cached = self.cache.lock().ok().and_then(|mut c| c.take());
        let plans = match cached {
            Some((key, plans)) if key == fingerprint(src) => plans,
            _ => self.plan(src)?,
        };
        let tracks = &self.tracks;
        // V09-0143: the driver dimension is 0 for the kept art, 1 for the
        // outline.
        self.visit(src, &plans, |ev| match ev {
            Ev::Keep(n, a, b) if (a, b) == (n.start, n.end()) => {
                out.set_dim(0);
                out.push(*n)
            }
            Ev::Keep(n, a, b) => {
                out.set_dim(0);
                out.emit(
                    i128::from(a),
                    i128::from(b - a),
                    i64::from(n.pitch),
                    i64::from(n.vel),
                    n,
                )
            }
            Ev::Piece(p, a, b, n, at) => {
                out.set_dim(1);
                let mut t = *n;
                if at.sub != 0 {
                    let s = sub_stepped(tracks, n, at.sub);
                    (t.sub, t.ch) = (s.sub, s.ch);
                }
                let vel = if self.fixed_vel {
                    at.vel
                } else {
                    i64::from(vel_scaled(n, at.vel_scale))
                };
                let len = (((b - a) as f64) * at.gate).round().max(1.0);
                out.emit(
                    i128::from(a) + i128::from(at.delay),
                    len as i128,
                    p as i64,
                    vel,
                    &t,
                )
            }
        });
        out.check()
    }
}

fn outline_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let mode = p.choice("mode").min(MODES.len() - 1);
    let geo = Geo {
        tx: p.int("thick_x").clamp(0, MAX_THICK_X) as u32,
        ty: p.int("thick_y").clamp(0, MAX_THICK_Y) as u32,
        off: p.int("offset").clamp(0, MAX_OFFSET) as u32,
    };
    let cell = note_value(p.choice("res"), p.ticks("res.ticks"), b.ctx).max(1);
    let k = Outline {
        outer: mode != 1,
        inner: mode != 0,
        geo,
        cell,
        diamond: p.choice("corners") == 1,
        simplify: p.int("simplify").clamp(0, MAX_SIMPLIFY) as u32,
        quantize: p.flag("quantize"),
        cut_start: p.flag("cut_start"),
        cut_end: p.flag("cut_end"),
        keep: p.flag("keep"),
        fixed_vel: p.choice("vel.mode") == 1,
        attr: Attr {
            vel_scale: p.float("vel.scale").clamp(1.0, 200.0) / 100.0,
            vel: p.int("vel").clamp(1, 127),
            sub: p.int("sub_step").clamp(-64, 64),
            delay: p.ticks("delay"),
            gate: p.float("gate").clamp(1.0, 100.0) / 100.0,
            dash: p.ticks("dash"),
            gap: p.ticks("gap"),
        },
        tracks: b.tracks.to_vec(),
        ctx: b.ctx.clone(),
        growth: Growth::Map,
        cache: Mutex::new(None),
    };
    // The loose bound over every level the Control Sweeps can reach.
    let swept_geo = b
        .ctx
        .sweeps
        .as_deref()
        .is_some_and(|sw| sw.drives("thick_x") || sw.drives("thick_y") || sw.drives("offset"));
    let (ty_hi, off_hi, tx_hi, dash) = match b.ctx.sweeps.as_deref() {
        Some(sw) if !sw.is_empty() => {
            let hi = |key: &str, base: u32, max: i64| {
                sw.int_bounds(key, i64::from(base)).1.clamp(0, max) as u64
            };
            let lo = |key: &str, base: u64| sw.int_bounds(key, base.min(i64::MAX as u64) as i64).0;
            let dash_on = k.attr.dash > 0 || sw.drives("dash");
            // The shortest dash period a swept dash / gap can reach.
            let period = (lo("dash", k.attr.dash).max(1) as u64)
                .saturating_add(lo("gap", k.attr.gap).max(0) as u64);
            (
                hi("thick_y", geo.ty, MAX_THICK_Y),
                hi("offset", geo.off, MAX_OFFSET),
                hi("thick_x", geo.tx, MAX_THICK_X),
                dash_on.then_some(period),
            )
        }
        _ => (
            u64::from(geo.ty),
            u64::from(geo.off),
            u64::from(geo.tx),
            (k.attr.dash > 0).then(|| k.attr.dash.saturating_add(k.attr.gap)),
        ),
    };
    let s = b.stats;
    let span = s.max_end.saturating_sub(s.min_start);
    let ry = off_hi + ty_hi.max(tx_hi);
    let bound = k.loose(
        s.count as u64,
        span,
        b.ctx.keys.keys() as u64,
        (ry, u64::from(k.simplify)),
        if swept_geo { MAX_LEVELS as u64 } else { 1 },
        dash,
    );
    let mut k = k;
    k.growth = Growth::Generate(bound);
    boxed(k)
}
