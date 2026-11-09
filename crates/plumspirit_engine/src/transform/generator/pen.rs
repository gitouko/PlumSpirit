//! The generators' pen (V09-S51, `docs/GENERATORS.md` "The pen"): shapes in
//! note space — x = ticks, y = pitch rows (row r covers [r, r + 1), its
//! centre is r + ½) — rasterized onto the resolution grid, straight into a
//! [`Sink`]: a [`Counter`] (the exact preflight, nothing allocated) or an
//! [`Emitter`] (the notes, into the run's `Output`). Both passes make the
//! same calls in the same order, so the count is exact.
//!
//! Rules (deterministic):
//! * a **stroke** (polyline) becomes one note per row each segment stays on:
//!   the segment's x interval on that row, start and end rounded to the
//!   grid, at least one step; consecutive pieces on one row with the same
//!   brush merge (a horizontal line is one note, a diagonal a staircase, a
//!   steep line one step per row);
//! * a **fill** (polygon, even–odd) becomes one note per row interval of its
//!   scanline at the row's centre;
//! * a **note** is one piece `[x0, x1)` on a row.
//!
//! Rows are clipped to one keyboard below and above the project's
//! ([`Band`]): what lands outside the keyboard inside that band goes through
//! the run's boundary policy; beyond it nothing is drawn, so a segment costs
//! at most three keyboards of rows whatever its coordinates.
//!
//! **Maps** (V09-S52, shared by every generator — [`Maps`]): a [`RowMap`]
//! moves each piece's row before it is merged (a fold back into a row range
//! — wrap or reflect — then a snap onto a scale), a [`TimeMap`] pulls each
//! finished note's start towards a musical grid by a strength, its length
//! kept, and an [`Accent`] raises the velocity of notes near given points
//! or beat lines. Maps never add a note (they move pieces; moved pieces may
//! merge), so a shape's bound holds with any maps.

use super::super::shared::fast_hash::FastMap;
use super::super::shared::scale_lattice::Lattice;
use super::super::{Output, TNote};
use std::sync::Arc;

/// How a piece of a shape plays: velocity, the part of each note kept
/// (gate, 0 < g ≤ 1) and the subchannel as a position in the target track's
/// subchannel table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush {
    pub vel: u8,
    pub gate: f64,
    pub sub: u32,
}

/// Where finished notes go.
pub trait Sink {
    /// One note `[start, start + dur)` on row `row` (= pitch); false stops
    /// drawing (the output is full or cancelled).
    fn note(&mut self, start: i128, dur: i128, row: i64, brush: &Brush) -> bool;
}

/// Counts notes (the exact preflight; allocates nothing). Counting stops
/// once `cap` is passed: such a run is refused anyway, so a huge request
/// costs at most `cap` steps to refuse.
#[derive(Clone, Copy, Debug)]
pub struct Counter {
    pub notes: u64,
    pub cap: u64,
}

impl Counter {
    /// A counter that stops after `cap` notes (its count is then `cap + 1`).
    pub fn capped(cap: u64) -> Counter {
        Counter { notes: 0, cap }
    }
}

impl Default for Counter {
    fn default() -> Counter {
        Counter::capped(u64::MAX - 1)
    }
}

impl Sink for Counter {
    #[inline]
    fn note(&mut self, _: i128, _: i128, _: i64, _: &Brush) -> bool {
        self.notes += 1;
        self.notes <= self.cap
    }
}

/// Emits notes into a run's `Output`: the template's track slot, release
/// velocity and group; the brush's velocity and subchannel (its position in
/// `subs`, the target track's table, modulo its length).
pub struct Emitter<'a> {
    pub out: &'a mut Output,
    pub tpl: TNote,
    pub subs: &'a [(u32, u8)],
}

impl Sink for Emitter<'_> {
    #[inline]
    fn note(&mut self, start: i128, dur: i128, row: i64, brush: &Brush) -> bool {
        let mut t = self.tpl;
        if !self.subs.is_empty() {
            let (id, ch) = self.subs[brush.sub as usize % self.subs.len()];
            t.sub = id;
            t.ch = ch;
        }
        t.vel = brush.vel;
        self.out.emit(start, dur, row, i64::from(brush.vel), &t)
    }
}

/// The rows a pen draws on: `[lo, hi)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub lo: i64,
    pub hi: i64,
}

impl Band {
    /// One keyboard of `keys` rows below and above it.
    pub fn around(keys: usize) -> Band {
        let k = keys as i64;
        Band { lo: -k, hi: 2 * k }
    }
}

/// How rows outside a range come back into it ([`RowMap`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fold {
    /// Rows stay where they are.
    #[default]
    None,
    /// Modulo the range: the row above the top is the bottom row (a line
    /// leaving the top re-enters at the bottom — split at the seam).
    Wrap,
    /// A triangle fold: the row above the top is the top row again, then
    /// downwards (a line bounces off the edges; the continuous reflection
    /// of row space at the range's edges).
    Reflect,
}

/// How far (in ranges) beyond a folded range a shape may reach: the clip
/// band of a folding pen ([`RowMap::clip`]) — generators keep their travel
/// inside it.
pub const FOLD_REACH: i64 = 9;

/// The row map: every piece's row is folded into `lo ..= hi` (when asked),
/// then snapped onto `scale` (when set; the nearest scale pitch, the lower
/// one on a tie — `Lattice::nearest`). Identity by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowMap {
    pub fold: Fold,
    pub lo: i64,
    /// The range's top row (inclusive).
    pub hi: i64,
    pub scale: Option<Lattice>,
}

impl RowMap {
    /// Whether the map moves nothing.
    pub fn is_identity(&self) -> bool {
        self.fold == Fold::None && self.scale.is_none()
    }

    /// Where row `r` goes.
    #[inline]
    pub fn map(&self, r: i64) -> i64 {
        let n = (self.hi - self.lo + 1).max(1);
        let r = match self.fold {
            Fold::None => r,
            Fold::Wrap => self.lo + (r - self.lo).rem_euclid(n),
            Fold::Reflect => {
                let k = (r - self.lo).rem_euclid(2 * n);
                self.lo + if k < n { k } else { 2 * n - 1 - k }
            }
        };
        match self.scale {
            Some(l) => l.nearest(r),
            None => r,
        }
    }

    /// The rows a segment is clipped to before mapping: `band` itself, or
    /// for a fold [`FOLD_REACH`] ranges below and above the range (every
    /// row there folds back into it), so a folded segment still costs a
    /// bounded number of rows.
    pub fn clip(&self, band: Band) -> Band {
        if self.fold == Fold::None {
            return band;
        }
        let n = (self.hi - self.lo + 1).max(1);
        Band {
            lo: self.lo - FOLD_REACH * n,
            hi: self.hi + 1 + FOLD_REACH * n,
        }
    }
}

/// The time map: each finished note's start pulled towards the nearest
/// line of a `grid` (ticks; 0 or 1 = off) by `strength` (0 – 1), its length
/// kept. The pull is rounded to the resolution grid; at full strength the
/// start is the grid line itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeMap {
    pub grid: u64,
    pub strength: f64,
}

impl TimeMap {
    /// Whether the map moves nothing.
    pub fn is_identity(&self) -> bool {
        self.grid <= 1 || self.strength.is_nan() || self.strength <= 0.0
    }

    /// Where a note starting at `start` starts (resolution `step` ticks).
    #[inline]
    pub fn map(&self, start: i128, step: i128) -> i128 {
        if self.is_identity() {
            return start;
        }
        let g = i128::from(self.grid);
        let q = start.div_euclid(g);
        let rem = start - q * g;
        // The nearest line (half way rounds forward).
        let target = if 2 * rem >= g { (q + 1) * g } else { q * g };
        let delta = target - start;
        if self.strength >= 1.0 {
            return target;
        }
        let pulled = delta as f64 * self.strength.clamp(0.0, 1.0);
        let step = step.max(1);
        start + ((pulled / step as f64).round() as i128) * step
    }
}

/// Accents: a finished note (its whole extent, before the gate and the
/// time map, on its mapped row) within `radius` grid units (a step across,
/// a row up) of one of the points, or of a beat line (every `beat` ticks
/// from tick 0; 0 = none), gets velocity `vel`.
#[derive(Clone, Debug, Default)]
pub struct Accent {
    pub vel: u8,
    pub radius: f64,
    pub beat: u64,
    /// The points by row (`floor(y)`, after the row map), x ascending.
    rows: FastMap<i64, Vec<(f64, f64)>>,
    count: usize,
}

impl Accent {
    /// Accents at `points` (note space, before the row map — moved through
    /// `map` like the notes) and at beat lines every `beat` ticks.
    pub fn new(
        vel: u8,
        radius: f64,
        beat: u64,
        points: impl IntoIterator<Item = (f64, f64)>,
        map: &RowMap,
    ) -> Accent {
        let mut rows: FastMap<i64, Vec<(f64, f64)>> = FastMap::default();
        let mut count = 0;
        for (x, y) in points {
            if !(x.is_finite() && y.is_finite()) {
                continue;
            }
            let q = y.floor();
            let r = map.map(q as i64);
            rows.entry(r).or_default().push((x, r as f64 + (y - q)));
            count += 1;
        }
        for v in rows.values_mut() {
            v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        }
        Accent {
            vel: vel.clamp(1, 127),
            radius: if radius.is_finite() {
                radius.max(0.0)
            } else {
                0.0
            },
            beat,
            rows,
            count,
        }
    }

    /// How many points accent.
    pub fn points(&self) -> usize {
        self.count
    }

    /// Whether the note `[start, end)` on `row` is accented (grid of `step`
    /// ticks).
    pub fn hits(&self, start: i128, end: i128, row: i64, step: i128) -> bool {
        let step = step.max(1) as f64;
        let reach = self.radius * step;
        if self.beat > 0 {
            let b = i128::from(self.beat);
            let k0 = start.div_euclid(b) * b;
            let k1 = k0 + b;
            let d = if k0 == start || k1 < end {
                0
            } else {
                (start - k0).min(k1 - end)
            };
            if d as f64 <= reach {
                return true;
            }
        }
        if self.count == 0 {
            return false;
        }
        let (s, e) = (start as f64, end as f64);
        let span = self.radius.ceil() as i64 + 1;
        for q in row.saturating_sub(span)..=row.saturating_add(span) {
            let Some(v) = self.rows.get(&q) else {
                continue;
            };
            let from = v.partition_point(|p| p.0 < s - reach);
            for &(x, y) in &v[from..] {
                if x > e + reach {
                    break;
                }
                let dx = (s - x).max(x - e).max(0.0) / step;
                let dy = (row as f64 - y).max(y - (row + 1) as f64).max(0.0);
                if dx * dx + dy * dy <= self.radius * self.radius {
                    return true;
                }
            }
        }
        false
    }
}

/// The maps a pen applies ([`RowMap`], [`TimeMap`], [`Accent`]); identity
/// by default.
#[derive(Clone, Debug, Default)]
pub struct Maps {
    pub rows: RowMap,
    pub time: TimeMap,
    pub accent: Option<Arc<Accent>>,
}

/// What a generator draws with (object-safe: the counting and the emitting
/// pen are the same calls).
pub trait Draw {
    /// A polyline through `pts` (ticks, rows) with one brush. The stroke
    /// ends with it (a later shape never merges into it).
    fn stroke(&mut self, pts: &[(f64, f64)], brush: Brush) -> bool;
    /// A polyline whose segment `i` (from `pts[i]` to `pts[i + 1]`) is drawn
    /// with `brushes[i]` (the last brush for the rest when shorter).
    fn stroke_with(&mut self, pts: &[(f64, f64)], brushes: &[Brush]) -> bool;
    /// A part of a polyline drawn like [`Draw::stroke_with`] whose stroke
    /// goes on: nothing ends with it (the piece open at its last point
    /// merges with the next part's), so a polyline sent as parts sharing
    /// their joints — the last with `stroke_with` — makes exactly the notes
    /// of one call (V09-S60: a long stroke's pieces stay bounded in memory
    /// and a capped count stops as soon as it passes the cap).
    fn stroke_part(&mut self, pts: &[(f64, f64)], brushes: &[Brush]) -> bool;
    /// A filled polygon (even–odd), scanned at each row's centre.
    fn fill(&mut self, poly: &[(f64, f64)], brush: Brush) -> bool;
    /// One note `[x0, x1)` on `row`.
    fn note(&mut self, x0: f64, x1: f64, row: i64, brush: Brush) -> bool;
}

/// One piece waiting to be merged with the next.
#[derive(Clone, Copy, Debug)]
struct Piece {
    start: i128,
    end: i128,
    row: i64,
    brush: Brush,
    /// The merged pieces' extent before rounding (ticks): the note is
    /// rounded once from it, not piece by piece.
    lo: f64,
    hi: f64,
}

/// The rasterizer over a sink, on a grid of `step` ticks (from tick 0).
pub struct Pen<S: Sink> {
    sink: S,
    step: i128,
    /// The rows notes are written on.
    band: Band,
    /// The rows segments are clipped to before the row map (`band` unless
    /// the map folds, [`RowMap::clip`]).
    clip: Band,
    maps: Maps,
    open: Option<Piece>,
    xs: Vec<f64>,
}

impl<S: Sink> Pen<S> {
    pub fn new(sink: S, step: u64, band: Band) -> Pen<S> {
        Pen {
            sink,
            step: i128::from(step.max(1)),
            band,
            clip: band,
            maps: Maps::default(),
            open: None,
            xs: Vec::new(),
        }
    }

    /// The same pen applying `maps` to every piece / note.
    pub fn with_maps(mut self, maps: Maps) -> Pen<S> {
        self.clip = maps.rows.clip(self.band);
        self.maps = maps;
        self
    }

    /// The sink, after the last piece was written.
    pub fn finish(mut self) -> S {
        self.flush();
        self.sink
    }

    /// The grid cells piece `[lo, hi]` covers: those whose centres it spans
    /// (its ends rounded to the grid), or — spanning none — the cell
    /// containing its middle (on a grid line, within 10⁻⁷ of a step: the
    /// cell after it).
    #[inline]
    fn span(step: i128, lo: f64, hi: f64) -> (i128, i128) {
        let s = Self::snap(step, lo);
        let e = Self::snap(step, hi);
        if e > s {
            return (s, e);
        }
        let m = (lo + hi) / 2.0 / step as f64;
        let r = m.round();
        let cell = if (m - r).abs() < 1e-7 { r } else { m.floor() };
        let c = Self::snap(step, cell * step as f64);
        (c, c.saturating_add(step))
    }

    /// `x` rounded to a grid of `step` ticks (saturating far outside the
    /// tick range).
    #[inline]
    fn snap(step: i128, x: f64) -> i128 {
        let q = (x / step as f64).round();
        let q = if q.abs() < 4e18 {
            // The same value through the hardware's 64-bit conversion (the
            // 128-bit one is a slow library call).
            i128::from(q as i64)
        } else if q.is_finite() {
            q.clamp(-1e30, 1e30) as i128
        } else {
            0
        };
        q.saturating_mul(step)
    }

    /// Writes the open piece (gate applied: the part kept from its start, at
    /// least one tick).
    fn flush(&mut self) -> bool {
        let Some(o) = self.open.take() else {
            return true;
        };
        let len = (o.end - o.start).max(1);
        let gate = if o.brush.gate.is_finite() {
            o.brush.gate.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let dur = if gate >= 1.0 {
            // round(len × 1) = len (a full gate needs no float round trip).
            len
        } else {
            let l = match i64::try_from(len) {
                Ok(v) => v as f64,
                Err(_) => len as f64,
            };
            ((l * gate).round() as i128).clamp(1, len)
        };
        let mut brush = o.brush;
        if let Some(a) = &self.maps.accent
            && a.hits(o.start, o.end, o.row, self.step)
        {
            brush.vel = a.vel;
        }
        let start = self.maps.time.map(o.start, self.step);
        self.sink.note(start, dur, o.row, &brush)
    }

    /// A piece `[xa, xb)` (either order) on `row` (before the row map).
    fn piece(&mut self, xa: f64, xb: f64, row: i64, brush: Brush) -> bool {
        let row = if self.maps.rows.is_identity() {
            row
        } else {
            self.maps.rows.map(row)
        };
        if row < self.band.lo || row >= self.band.hi || !xa.is_finite() || !xb.is_finite() {
            return true;
        }
        let (a, b) = if xa <= xb { (xa, xb) } else { (xb, xa) };
        let step = self.step;
        let (mut s, mut e) = Self::span(step, a, b);
        if let Some(o) = &mut self.open
            && o.row == row
        {
            if o.brush == brush && s <= o.end && e >= o.start {
                // Rounded once from the merged extent: a path's vertices
                // inside a row never widen its note.
                o.lo = o.lo.min(a);
                o.hi = o.hi.max(b);
                (o.start, o.end) = Self::span(step, o.lo, o.hi);
                return true;
            }
            // A neighbour of another brush on the row (a stroke whose brush
            // changes): the new piece keeps only what lies beyond it, so a
            // stroke's consecutive pieces never overlap.
            if s < o.end && e > o.start {
                if s >= o.start {
                    s = o.end;
                } else {
                    e = o.start;
                }
                if e <= s {
                    return true;
                }
            }
        }
        if !self.flush() {
            return false;
        }
        self.open = Some(Piece {
            start: s,
            end: e,
            row,
            brush,
            lo: a,
            hi: b,
        });
        true
    }

    /// One segment: a piece per row it stays on (rows outside the band are
    /// clipped away first).
    fn segment_controlled(
        &mut self,
        p0: (f64, f64),
        p1: (f64, f64),
        brush: Brush,
        check: &mut impl FnMut() -> bool,
    ) -> bool {
        if !check() {
            return false;
        }
        let ((x0, y0), (x1, y1)) = (p0, p1);
        if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
            return true;
        }
        // Clip y to the clip band [lo, hi) (parametrically).
        let (lo, hi) = (self.clip.lo as f64, self.clip.hi as f64 - 1e-9);
        let (mut ta, mut tb) = (0.0f64, 1.0f64);
        let dy = y1 - y0;
        if dy == 0.0 {
            if y0 < lo || y0 > hi {
                return true;
            }
        } else {
            let (t_lo, t_hi) = ((lo - y0) / dy, (hi - y0) / dy);
            let (u, v) = if t_lo < t_hi {
                (t_lo, t_hi)
            } else {
                (t_hi, t_lo)
            };
            ta = ta.max(u);
            tb = tb.min(v);
            if ta > tb {
                return true;
            }
        }
        let at = |t: f64| (x0 + (x1 - x0) * t, y0 + dy * t);
        let (xa, ya) = at(ta);
        let (xb, yb) = at(tb);
        let (ra, rb) = (ya.floor() as i64, yb.floor() as i64);
        if ra == rb {
            return self.piece(xa, xb, ra, brush);
        }
        let dir = if rb > ra { 1 } else { -1 };
        let mut r = ra;
        let mut s0 = 0.0f64;
        let mut visited = 0usize;
        loop {
            if visited.is_multiple_of(64) && !check() {
                return false;
            }
            visited += 1;
            let s1 = if r == rb {
                1.0
            } else {
                let yb_row = if dir > 0 { (r + 1) as f64 } else { r as f64 };
                ((yb_row - ya) / (yb - ya)).clamp(0.0, 1.0)
            };
            if s1 > s0 {
                let xs = xa + (xb - xa) * s0;
                let xe = xa + (xb - xa) * s1;
                if !self.piece(xs, xe, r, brush) {
                    return false;
                }
            }
            if r == rb {
                break;
            }
            r += dir;
            s0 = s1;
        }
        true
    }
    /// Preflight the fill crossing workspace before an untrusted bounded call.
    pub fn reserve_fill(
        &mut self,
        vertices: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        self.xs.try_reserve_exact(vertices)
    }

    /// Same native stroke/merging rules with bounded segment and row stop checks.
    pub fn stroke_controlled(
        &mut self,
        pts: &[(f64, f64)],
        brush: Brush,
        mut check: impl FnMut() -> bool,
    ) -> bool {
        self.stroke_part_controlled(pts, std::slice::from_ref(&brush), &mut check)
            && check()
            && self.flush()
    }

    fn stroke_part_controlled(
        &mut self,
        pts: &[(f64, f64)],
        brushes: &[Brush],
        check: &mut impl FnMut() -> bool,
    ) -> bool {
        if !check() {
            return false;
        }
        let Some(&last) = brushes.last() else {
            return true;
        };
        if pts.len() == 1 {
            let (x, y) = pts[0];
            if !self.piece(x, x, y.floor() as i64, brushes[0]) {
                return false;
            }
        }
        for (i, w) in pts.windows(2).enumerate() {
            let b = brushes.get(i).copied().unwrap_or(last);
            if !self.segment_controlled(w[0], w[1], b, check) {
                return false;
            }
        }
        true
    }

    /// Same native even-odd rasterizer with cooperative stop checks, including empty rows.
    pub fn fill_controlled(
        &mut self,
        poly: &[(f64, f64)],
        brush: Brush,
        mut check: impl FnMut() -> bool,
    ) -> bool {
        if !check() {
            return false;
        }
        if !self.flush() {
            return false;
        }
        if poly.len() < 3 || poly.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
            return true;
        }
        let (ymin, ymax) = poly
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
                (a.min(p.1), b.max(p.1))
            });
        // Rows whose centre r + ½ lies in [ymin, ymax), inside the clip band.
        let r0 = ((ymin - 0.5).ceil() as i64).max(self.clip.lo);
        let r1 = (((ymax - 0.5).ceil() as i64) - 1).min(self.clip.hi - 1);
        let mut xs = std::mem::take(&mut self.xs);
        let mut ok = true;
        'rows: for r in r0..=r1 {
            if !check() {
                ok = false;
                break;
            }
            let yc = r as f64 + 0.5;
            xs.clear();
            for k in 0..poly.len() {
                if k.is_multiple_of(64) && !check() {
                    ok = false;
                    break 'rows;
                }
                let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                if (a.1 <= yc && yc < b.1) || (b.1 <= yc && yc < a.1) {
                    xs.push(a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1));
                }
            }
            xs.sort_by(f64::total_cmp);
            for (i, pair) in xs.chunks_exact(2).enumerate() {
                if i.is_multiple_of(64) && !check() {
                    ok = false;
                    break 'rows;
                }
                if pair[1] > pair[0] && !(self.piece(pair[0], pair[1], r, brush) && self.flush()) {
                    ok = false;
                    break 'rows;
                }
            }
        }
        self.xs = xs;
        ok
    }
}

impl<S: Sink> Draw for Pen<S> {
    fn stroke(&mut self, pts: &[(f64, f64)], brush: Brush) -> bool {
        self.stroke_with(pts, std::slice::from_ref(&brush))
    }

    fn stroke_with(&mut self, pts: &[(f64, f64)], brushes: &[Brush]) -> bool {
        if brushes.is_empty() {
            return true;
        }
        self.stroke_part(pts, brushes) && self.flush()
    }

    fn stroke_part(&mut self, pts: &[(f64, f64)], brushes: &[Brush]) -> bool {
        self.stroke_part_controlled(pts, brushes, &mut || true)
    }

    fn fill(&mut self, poly: &[(f64, f64)], brush: Brush) -> bool {
        self.fill_controlled(poly, brush, || true)
    }

    fn note(&mut self, x0: f64, x1: f64, row: i64, brush: Brush) -> bool {
        self.flush() && self.piece(x0, x1, row, brush) && self.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the pieces (start, end, row).
    #[derive(Default)]
    struct Rec(Vec<(i128, i128, i64)>);

    impl Sink for Rec {
        fn note(&mut self, s: i128, d: i128, r: i64, _: &Brush) -> bool {
            self.0.push((s, s + d, r));
            true
        }
    }

    const B: Brush = Brush {
        vel: 100,
        gate: 1.0,
        sub: 0,
    };

    #[test]
    fn controlled_empty_fill_stops_and_preserves_preflight_workspace_for_reuse() {
        let mut p = Pen::new(Rec::default(), 1, Band { lo: 0, hi: 4096 });
        p.reserve_fill(4).unwrap();
        let mut checks = 0;
        assert!(
            !p.fill_controlled(&[(1., 0.), (1., 4096.), (1., 4096.)], B, || {
                checks += 1;
                checks < 10
            })
        );
        assert_eq!(checks, 10);
        assert!(p.xs.capacity() >= 4);
        assert!(p.fill(&[(0., 0.), (4., 0.), (4., 2.), (0., 2.)], B));
        assert_eq!(p.finish().0, vec![(0, 4, 0), (0, 4, 1)]);
    }

    #[test]
    fn controlled_stroke_stops_inside_a_long_segment() {
        let mut p = Pen::new(Rec::default(), 1, Band { lo: 0, hi: 4096 });
        let mut checks = 0;
        assert!(!p.stroke_controlled(&[(1., 0.), (1., 4096.)], B, || {
            checks += 1;
            checks < 5
        }));
        assert_eq!(checks, 5);
        let notes = p.finish().0;
        assert!(!notes.is_empty() && notes.len() <= 129);
    }

    fn pen() -> Pen<Rec> {
        Pen::new(Rec::default(), 120, Band::around(128))
    }

    #[test]
    fn a_horizontal_stroke_is_one_note_on_the_grid() {
        let mut p = pen();
        assert!(p.stroke(&[(10.0, 60.5), (500.0, 60.5), (965.0, 60.5)], B));
        assert_eq!(p.finish().0, vec![(0, 960, 60)]);
    }

    #[test]
    fn a_diagonal_is_a_staircase_and_a_steep_line_one_step_per_row() {
        // Four rows over 480 ticks: 120 each.
        let mut p = pen();
        p.stroke(&[(0.0, 60.0), (480.0, 64.0)], B);
        assert_eq!(
            p.finish().0,
            vec![(0, 120, 60), (120, 240, 61), (240, 360, 62), (360, 480, 63)]
        );
        // Vertical: one step on every row, both directions.
        let mut p = pen();
        p.stroke(&[(240.0, 70.5), (240.0, 66.5)], B);
        let v = p.finish().0;
        assert_eq!(v.len(), 5);
        assert!(v.iter().all(|&(s, e, _)| (s, e) == (240, 360)));
        assert_eq!(
            v.iter().map(|x| x.2).collect::<Vec<_>>(),
            vec![70, 69, 68, 67, 66]
        );
    }

    #[test]
    fn fills_scan_each_rows_centre_and_gates_shorten_from_the_start() {
        // A triangle 0..960 at row 60 narrowing to a point at row 64.
        let mut p = pen();
        p.fill(&[(0.0, 60.0), (960.0, 60.0), (480.0, 64.0)], B);
        let v = p.finish().0;
        assert_eq!(v.len(), 4);
        // Row 60's centre (60.5): x from 60 to 900 → grid 120 … 960
        // (half a step rounds away from zero).
        assert_eq!(v[0], (120, 960, 60));
        assert_eq!(v[3].2, 63);
        let half = Brush { gate: 0.5, ..B };
        let mut p = pen();
        p.note(0.0, 960.0, 60, half);
        assert_eq!(p.finish().0, vec![(0, 480, 60)]);
    }

    #[test]
    fn counting_and_emitting_make_the_same_pieces_and_the_band_clips() {
        let shapes = |d: &mut dyn Draw| {
            d.stroke(&[(0.0, -1e9), (100_000.0, 1e9)], B);
            d.fill(&[(0.0, 10.0), (5000.0, 300.0), (9000.0, -40.0)], B);
            d.note(5.0, 5.0, 900, B);
        };
        let mut c = Pen::new(Counter::default(), 60, Band::around(128));
        shapes(&mut c);
        let n = c.finish().notes;
        let mut r = Pen::new(Rec::default(), 60, Band::around(128));
        shapes(&mut r);
        let v = r.finish().0;
        assert_eq!(n, v.len() as u64);
        // Only rows −128 … 255 were drawn: the stroke crosses all 384 (one
        // step each, near its middle), the note on row 900 is not drawn.
        assert!(v.iter().all(|x| (-128..256).contains(&x.2)));
        let stroke: Vec<_> = v
            .iter()
            .filter(|x| (49_000..51_000).contains(&x.0))
            .collect();
        assert_eq!(stroke.len(), 384);
    }

    #[test]
    fn row_maps_wrap_reflect_and_snap_onto_a_scale() {
        let wrap = RowMap {
            fold: Fold::Wrap,
            lo: 60,
            hi: 63,
            scale: None,
        };
        // Rows 60 … 63 repeat every four rows, both ways.
        let w: Vec<i64> = (56..70).map(|r| wrap.map(r)).collect();
        assert_eq!(
            w,
            vec![60, 61, 62, 63, 60, 61, 62, 63, 60, 61, 62, 63, 60, 61]
        );
        let refl = RowMap {
            fold: Fold::Reflect,
            ..wrap
        };
        // The edge rows repeat: 60 61 62 63 63 62 61 60 60 61 …
        let r: Vec<i64> = (60..72).map(|r| refl.map(r)).collect();
        assert_eq!(r, vec![60, 61, 62, 63, 63, 62, 61, 60, 60, 61, 62, 63]);
        assert_eq!(refl.map(59), 60);
        assert_eq!(refl.map(55), 63);
        // C major: C♯ (61) → C (tie: the lower), D♯ (63) → D, F♯ → F.
        let major = RowMap {
            scale: Some(Lattice::from_mask(0b1010_1011_0101)),
            ..RowMap::default()
        };
        assert_eq!(
            [60, 61, 62, 63, 64, 66, 71].map(|r| major.map(r)),
            [60, 60, 62, 62, 64, 65, 71]
        );
        assert!(RowMap::default().is_identity() && !major.is_identity());
        // A fold clips segments to its reach, not the band.
        assert_eq!(wrap.clip(Band::around(128)), Band { lo: 24, hi: 100 });
        assert_eq!(major.clip(Band::around(128)), Band::around(128));
    }

    #[test]
    fn a_folded_stroke_stays_in_its_range_and_a_scale_merges_rows() {
        // A line over 8 rows (60 → 68) folded into 60 … 63.
        let maps = |fold| Maps {
            rows: RowMap {
                fold,
                lo: 60,
                hi: 63,
                scale: None,
            },
            ..Maps::default()
        };
        let draw = |m: Maps| {
            let mut p = pen().with_maps(m);
            p.stroke(&[(0.0, 60.0), (960.0, 68.0)], B);
            p.finish().0
        };
        let rows = |v: &[(i128, i128, i64)]| v.iter().map(|x| x.2).collect::<Vec<_>>();
        assert_eq!(
            rows(&draw(maps(Fold::Wrap))),
            vec![60, 61, 62, 63, 60, 61, 62, 63]
        );
        // Reflect: the two pieces on row 63 touch and merge (one note).
        let r = draw(maps(Fold::Reflect));
        assert_eq!(rows(&r), vec![60, 61, 62, 63, 62, 61, 60]);
        assert_eq!(r[3], (360, 600, 63));
        // A segment beyond the band but within the fold's reach folds back
        // (the clip band is the reach); beyond the reach nothing is drawn.
        let mut p = pen().with_maps(maps(Fold::Wrap));
        p.stroke(&[(0.0, 30.0), (960.0, 99.5)], B);
        let v = p.finish().0;
        assert_eq!(v.len(), 70);
        assert!(v.iter().all(|x| (60..=63).contains(&x.2)));
        let mut p = pen().with_maps(maps(Fold::Wrap));
        p.stroke(&[(0.0, 300.0), (960.0, 330.5)], B);
        assert!(p.finish().0.is_empty());
        // Scale snap: the staircase over C … E in C major is C C D D E —
        // pieces snapped onto one row merge.
        let mut p = pen().with_maps(Maps {
            rows: RowMap {
                scale: Some(Lattice::from_mask(0b1010_1011_0101)),
                ..RowMap::default()
            },
            ..Maps::default()
        });
        p.stroke(&[(0.0, 60.0), (600.0, 65.0)], B);
        assert_eq!(
            p.finish().0,
            vec![(0, 240, 60), (240, 480, 62), (480, 600, 64)]
        );
    }

    #[test]
    fn a_time_map_pulls_starts_towards_the_grid_and_keeps_lengths() {
        let t = TimeMap {
            grid: 960,
            strength: 0.5,
        };
        // 360 → the line at 0: pull −360 × ½ = −180 = −1.5 steps, rounded
        // away from zero to −2 steps → 120; 600 → 960: +180 → +240 = 840.
        assert_eq!(t.map(360, 120), 120);
        assert_eq!(t.map(600, 120), 840);
        assert_eq!(t.map(480, 120), 720); // half way: forwards
        let full = TimeMap { strength: 1.0, ..t };
        assert_eq!(
            [0, 120, 470, 481, -100].map(|s| full.map(s, 120)),
            [0, 0, 0, 960, 0]
        );
        assert!(
            TimeMap::default().is_identity()
                && TimeMap {
                    grid: 1,
                    strength: 1.0
                }
                .is_identity()
        );
        // Through a pen: starts move, lengths stay; the count is the same.
        let mut p = pen().with_maps(Maps {
            time: full,
            ..Maps::default()
        });
        p.stroke(&[(0.0, 60.0), (480.0, 64.0)], B);
        assert_eq!(
            p.finish().0,
            vec![(0, 120, 60), (0, 120, 61), (0, 120, 62), (0, 120, 63)]
        );
        let mut c = Pen::new(Counter::default(), 120, Band::around(128)).with_maps(Maps {
            time: full,
            rows: RowMap {
                fold: Fold::Reflect,
                lo: 60,
                hi: 61,
                scale: None,
            },
            accent: None,
        });
        c.stroke(&[(0.0, 60.0), (480.0, 64.0)], B);
        // Rows 60 61 61 60: the two pieces on 61 touch and merge.
        assert_eq!(c.finish().notes, 3);
    }

    /// Records the pieces with their velocities.
    #[derive(Default)]
    struct Vel(Vec<(i128, i64, u8)>);

    impl Sink for Vel {
        fn note(&mut self, s: i128, _: i128, r: i64, b: &Brush) -> bool {
            self.0.push((s, r, b.vel));
            true
        }
    }

    #[test]
    fn accents_raise_notes_near_points_and_beat_lines() {
        let rows = RowMap::default();
        // A point at tick 600, row 62.5; radius one step.
        let a = Accent::new(127, 1.0, 0, [(600.0, 62.5)], &rows);
        assert_eq!(a.points(), 1);
        assert!(a.hits(480, 600, 62, 120)); // touches it
        assert!(a.hits(720, 840, 62, 120)); // one step after
        assert!(!a.hits(840, 960, 62, 120)); // two steps after
        assert!(a.hits(600, 720, 63, 120)); // half a row above
        assert!(!a.hits(600, 720, 64, 120)); // 1.5 rows above
        // Beat lines every 960 ticks, radius 0: a note containing or
        // touching one.
        let b = Accent::new(110, 0.0, 960, [], &rows);
        assert!(b.hits(960, 1080, 60, 120) && b.hits(900, 1000, 60, 120));
        assert!(b.hits(840, 960, 60, 120));
        assert!(!b.hits(1080, 1200, 60, 120) && !b.hits(840, 900, 60, 120));
        // Through a pen: the staircase's pieces near the point are accented.
        let mut p = Pen::new(Vel::default(), 120, Band::around(128)).with_maps(Maps {
            accent: Some(Arc::new(Accent::new(127, 0.0, 0, [(250.0, 62.4)], &rows))),
            ..Maps::default()
        });
        p.stroke(&[(0.0, 60.0), (480.0, 64.0)], B);
        assert_eq!(
            p.finish().0,
            vec![(0, 60, 100), (120, 61, 100), (240, 62, 127), (360, 63, 100)]
        );
        // Points are moved through the row map like the notes.
        let fold = RowMap {
            fold: Fold::Wrap,
            lo: 60,
            hi: 63,
            scale: None,
        };
        let c = Accent::new(127, 0.0, 0, [(0.0, 64.5)], &fold);
        assert!(c.hits(0, 120, 60, 120) && !c.hits(0, 120, 64, 120));
    }

    /// Records every note with its brush.
    #[derive(Default)]
    struct All(Vec<(i128, i128, i64, u8, u64)>);

    impl Sink for All {
        fn note(&mut self, s: i128, d: i128, r: i64, b: &Brush) -> bool {
            self.0.push((s, d, r, b.vel, b.gate.to_bits()));
            true
        }
    }

    #[test]
    fn a_stroke_sent_in_parts_sharing_their_joints_makes_the_notes_of_one_call() {
        // A wandering polyline with a brush per segment: runs of equal
        // brushes (pieces merge across a joint) and changes (neighbours
        // trimmed), flat and steep segments.
        let mut pts = Vec::new();
        let mut brushes = Vec::new();
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let (mut x, mut y) = (0.0f64, 60.0f64);
        for i in 0..2_000 {
            pts.push((x, y));
            x += (next() % 400) as f64;
            y = (y + (next() % 7) as f64 - 3.0).clamp(0.0, 127.9);
            let v = (next() % 3) as u8;
            brushes.push(Brush {
                vel: 90 + v,
                gate: if i % 5 == 0 { 0.5 } else { 1.0 },
                sub: 0,
            });
        }
        pts.push((x, y));
        let whole = {
            let mut p = Pen::new(All::default(), 120, Band::around(128));
            assert!(p.stroke_with(&pts, &brushes));
            p.finish().0
        };
        for parts in [2usize, 3, 7, 64, 1_999] {
            let mut p = Pen::new(All::default(), 120, Band::around(128));
            let cut = brushes.len().div_ceil(parts);
            let mut i = 0;
            while i + cut < brushes.len() {
                assert!(p.stroke_part(&pts[i..=i + cut], &brushes[i..i + cut]));
                i += cut;
            }
            assert!(p.stroke_with(&pts[i..], &brushes[i..]));
            assert_eq!(p.finish().0, whole, "{parts} parts");
        }
        // A later stroke never merges into a finished one.
        let mut p = Pen::new(All::default(), 120, Band::around(128));
        assert!(p.stroke_with(&[(0.0, 60.5), (120.0, 60.5)], &[B]));
        assert!(p.stroke_with(&[(120.0, 60.5), (240.0, 60.5)], &[B]));
        assert_eq!(p.finish().0.len(), 2);
    }
}
