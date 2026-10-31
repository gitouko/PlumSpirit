//! Continuous geometry engine (contract §86–88, §135–139).
//!
//! Notes are rectangles in a continuous plane: **x = tick**, **y = pitch row**
//! (a note of pitch `p` covers rows `[p, p + 1)`, its centre line is
//! `y = p + 0.5`). Geometry tools work in **unit coordinates** of the selection's
//! box (`u = (x − t0) / (t1 − t0)`, `v = (y − p0) / (p1 + 1 − p0)`), transform
//! points continuously, and then **rasterize** the result back to MIDI notes with
//! a deterministic policy:
//!
//! * `SampleStart` — the start and end points of the note's centre line are
//!   mapped; the pitch is the row of the mapped start point;
//! * `SampleMid` — like `SampleStart`, the pitch is the row of the mapped middle;
//! * `Split` — the centre line is cut into segments, each mapped at its middle;
//!   consecutive segments on the same row merge (bent notes become staircases).
//!
//! **Duplicate / Fill** (§87, §139): when a transformation stretches a note's
//! row vertically, the rows its mapped top and bottom edges enclose are filled
//! with copies, so vertical stretching keeps the artwork solid instead of moving
//! sparse notes apart.
//!
//! [`BoxScale`] is the exact (integer, rational) box mapping behind the Piano
//! Roll's scale handles: no floating point touches ticks.

use super::output::Output;
use super::param::*;
use super::preflight::{Estimate, Growth};
use super::{Kernel, PointKernel, Source, SourceStats, SourceTrack, TNote, TransformError};

/// The selection's box: ticks `[t0, t1)`, rows `[p0, p1]` (inclusive pitches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeomBox {
    pub t0: u64,
    pub t1: u64,
    pub p0: u8,
    pub p1: u8,
}

impl GeomBox {
    pub fn of(s: &SourceStats) -> GeomBox {
        if s.count == 0 {
            return GeomBox {
                t0: 0,
                t1: 1,
                p0: 60,
                p1: 60,
            };
        }
        GeomBox {
            t0: s.min_start,
            t1: s.max_end.max(s.min_start + 1),
            p0: s.min_pitch,
            p1: s.max_pitch.max(s.min_pitch),
        }
    }

    #[inline]
    pub fn width(&self) -> f64 {
        (self.t1 - self.t0).max(1) as f64
    }

    /// Rows of the box (≥ 1).
    #[inline]
    pub fn rows(&self) -> f64 {
        f64::from(self.p1 - self.p0) + 1.0
    }

    #[inline]
    pub fn to_unit(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.t0 as f64) / self.width(),
            (y - f64::from(self.p0)) / self.rows(),
        )
    }

    #[inline]
    pub fn from_unit(&self, u: f64, v: f64) -> (f64, f64) {
        (
            self.t0 as f64 + u * self.width(),
            f64::from(self.p0) + v * self.rows(),
        )
    }
}

/// Rasterization policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Raster {
    SampleStart,
    SampleMid,
    Split,
}

/// Largest |time| (ticks) a mapped point keeps: 2⁹⁶, far past the last tick
/// (2⁶⁴), so ends and lengths stay inside i128.
pub const MAP_X_CAP: f64 = 79_228_162_514_264_337_593_543_950_336.0;
/// Largest |row| a mapped point keeps (2⁴⁰ rows; the keyboard has 128 / 256).
pub const MAP_Y_CAP: f64 = 1_099_511_627_776.0;

pub const RASTERS: &[&str] = &[
    "Sample the start",
    "Sample the middle",
    "Split along the path",
];

/// How Duplicate Notes builds the rows a vertical stretch adds (B1-0115).
/// Only [`BoxScale`] (Scale Y and the scale handles) reads it; the geometry
/// tools' rasterizer always fills rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FillMode {
    /// **Fill added rows**: every target row of the note's stretched row
    /// cell gets a copy (every `res`-th piano row) — cells are exact, so the
    /// stretched art is solid and never doubled.
    #[default]
    Rows,
    /// **Copy by pitch step**: the note on its scaled row plus `copies`
    /// copies, each `step` semitones above the last. `copies == 0` is Auto:
    /// ⌈rows after ÷ rows before⌉ − 1 copies, kept inside the scaled box, so
    /// every note gets the same thickness (fractional factors overlap the
    /// next row's copies — what Scale Y's overlap prevention removes).
    Step { step: u8, copies: u8 },
}

/// The largest pitch step / copy count of [`FillMode::Step`].
pub const MAX_FILL_STEP: u8 = 24;
pub const MAX_FILL_COPIES: u8 = 64;

/// Duplicate / Fill options (§139).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    /// Fill every `res`-th row (1 = solid).
    pub res: u8,
    /// Copies fade: velocity falls with the distance from the source row.
    pub fade: bool,
    /// Copies keep the source note's subchannel (otherwise they cycle through
    /// the track's subchannels, one step per row).
    pub keep_sub: bool,
    /// Copies landing on a row already covered by an overlapping copy of the
    /// same source note are merged (never two notes on one row there).
    pub merge: bool,
    /// B1-0115: added rows by cell (default) or by a pitch step.
    pub mode: FillMode,
}

impl Default for Fill {
    fn default() -> Self {
        Fill {
            res: 1,
            fade: false,
            keep_sub: true,
            merge: true,
            mode: FillMode::Rows,
        }
    }
}

/// What happens to points mapped outside the selection box.
pub const UNDEFINED: &[&str] = &[
    "Leave them (pitch range policy below)",
    "Wrap around the selection box",
    "Clamp to the selection box",
];

pub const FILL_POLICIES: &[&str] = &["Full copies", "Copies fade with distance"];
pub const OVERLAP_POLICIES: &[&str] = &["Merge overlapping copies", "Keep every copy"];

/// The shared geometry controls (§88) every geometry tool appends to its
/// parameters (keys `g.*`).
pub fn geom_specs() -> Vec<ParamSpec> {
    vec![
        float(
            "g.pivot_x",
            "Pivot X (of the selection box)",
            0.0,
            100.0,
            50.0,
            "%",
        )
        .help("Where the pivot sits in time: 0 % = the selection box's start, 100 % = its end. Scale X / Y and the tool's own turns center on it."),
        float(
            "g.pivot_y",
            "Pivot Y (of the selection box)",
            0.0,
            100.0,
            50.0,
            "%",
        )
        .help("Where the pivot sits in pitch: 0 % = the selection box's lowest row, 100 % = above its highest."),
        float("g.scale_x", "Scale X", -400.0, 400.0, 100.0, "%")
            .help("Stretches the result in time around the pivot after the tool's own transformation (100 % = unchanged, negative = mirrored)."),
        float("g.scale_y", "Scale Y", -400.0, 400.0, 100.0, "%")
            .help("Stretches the result in pitch around the pivot after the tool's own transformation (100 % = unchanged, negative = upside down)."),
        int(
            "g.pos_x",
            "Position X (ticks)",
            -1_000_000_000,
            1_000_000_000,
            0,
        )
        .help("Moves the whole result later (positive) or earlier (negative) by this many ticks."),
        int("g.pos_y", "Position Y (semitones)", -127, 127, 0)
            .keyed()
            .help("Moves the whole result up (positive) or down (negative) by this many semitones."),
        choice("g.undefined", "Undefined area", UNDEFINED, 0).help(
            "What happens to notes mapped outside the selection box: they stay where they land (the key range rule still applies), wrap around to the box's other side, or are clamped to its edges.",
        ),
        choice("g.raster", "Rasterization", RASTERS, 1).help(
            "How a mapped note becomes a MIDI note again: its pitch from the mapped start or the mapped middle of the note, or the note cut into segments that each find their own pitch (bent notes become staircases).",
        ),
        int(
            "g.segments",
            "Segments per note (split / quality)",
            1,
            256,
            8,
        )
        .help("With Split along the path: how many pieces each note is cut into before mapping; more pieces follow curves more closely and make more notes."),
        flag("g.fill", "Duplicate / fill stretched rows", false).help(
            "When the transformation stretches a note over several rows, those rows get copies of it, so stretched artwork stays solid instead of breaking apart.",
        ),
        int("g.fill_res", "Fill every n-th row", 1, 12, 1)
            .help("With Duplicate / fill: 1 fills every stretched row, 2 every second row, and so on."),
        choice("g.fill_policy", "Copies", FILL_POLICIES, 0)
            .help("Full copies keep the note's velocity; fading copies get softer the farther they are from the note's own row."),
        choice("g.overlap", "Overlaps", OVERLAP_POLICIES, 0).help(
            "Copies of one note that overlap on the same row merge into one note, or every copy is kept.",
        ),
        flag("g.keep_sub", "Copies keep the subchannel", true).help(
            "On: copies take the source note's subchannel. Off: they step through the track's subchannels, one per row.",
        ),
    ]
}

/// Shared geometry settings read from [`geom_specs`].
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    pub pivot: (f64, f64),
    pub scale: (f64, f64),
    /// Offset in ticks and semitones.
    pub pos: (f64, f64),
    pub raster: Raster,
    pub segments: u32,
    pub fill: Option<Fill>,
    pub bbox: GeomBox,
    /// 0 leave, 1 wrap around the box, 2 clamp to the box.
    pub undefined: u8,
    /// The keyboard's highest row (127, or 255 in 256-Key Mode): Fill never
    /// adds rows above it.
    pub top: i64,
}

impl Geom {
    pub fn from_params(p: &Params, stats: &SourceStats) -> Geom {
        Geom {
            pivot: (p.float("g.pivot_x") / 100.0, p.float("g.pivot_y") / 100.0),
            scale: (p.float("g.scale_x") / 100.0, p.float("g.scale_y") / 100.0),
            pos: (p.int("g.pos_x") as f64, p.int("g.pos_y") as f64),
            raster: match p.choice("g.raster") {
                0 => Raster::SampleStart,
                2 => Raster::Split,
                _ => Raster::SampleMid,
            },
            segments: p.int("g.segments").clamp(1, 256) as u32,
            fill: p.flag("g.fill").then(|| Fill {
                res: p.int("g.fill_res").clamp(1, 12) as u8,
                fade: p.choice("g.fill_policy") == 1,
                keep_sub: p.flag("g.keep_sub"),
                merge: p.choice("g.overlap") == 0,
                mode: FillMode::Rows,
            }),
            bbox: GeomBox::of(stats),
            undefined: p.choice("g.undefined").min(2) as u8,
            top: i64::from(p.keys.max_pitch()),
        }
    }

    /// Identity settings for a box (tests, internal use).
    pub fn identity(bbox: GeomBox) -> Geom {
        Geom {
            pivot: (0.5, 0.5),
            scale: (1.0, 1.0),
            pos: (0.0, 0.0),
            raster: Raster::SampleMid,
            segments: 8,
            fill: None,
            bbox,
            undefined: 0,
            top: i64::from(crate::model::note::MAX_PITCH),
        }
    }

    /// The shared affine part (scale around the pivot, then position), in
    /// absolute coordinates.
    #[inline]
    pub fn affine(&self, x: f64, y: f64) -> (f64, f64) {
        let (u, v) = self.bbox.to_unit(x, y);
        let u = self.pivot.0 + (u - self.pivot.0) * self.scale.0;
        let v = self.pivot.1 + (v - self.pivot.1) * self.scale.1;
        let (x, y) = self.bbox.from_unit(u, v);
        self.undefined_area(x + self.pos.0, y + self.pos.1)
    }

    /// Applies the undefined-area policy to an absolute point.
    #[inline]
    pub fn undefined_area(&self, x: f64, y: f64) -> (f64, f64) {
        let b = &self.bbox;
        let (t0, w) = (b.t0 as f64, b.width());
        let (p0, rows) = (f64::from(b.p0), b.rows());
        match self.undefined {
            1 => (
                t0 + (x - t0).rem_euclid(w.max(1.0)),
                p0 + (y - p0).rem_euclid(rows.max(1.0)),
            ),
            2 => (x.clamp(t0, t0 + w), y.clamp(p0, p0 + rows - 1e-9)),
            _ => (x, y),
        }
    }

    /// Output notes growth bound per source note.
    pub fn per_note(&self) -> u64 {
        let split = if self.raster == Raster::Split {
            u64::from(self.segments)
        } else {
            1
        };
        let fill = if self.fill.is_some() {
            // Worst case: a note stretched over the whole pitch range (128
            // rows) plus its own row when that lies outside it.
            129
        } else {
            1
        };
        split * fill
    }

    /// An absolute point through the unit-space map `f`, then the shared
    /// affine part. Always inlined into [`Geom::rasterize`] (V09-S40 step 6:
    /// left to the optimizer, it was outlined in one build and inlined in
    /// another — ±4 % instructions on every geometry tool's path between
    /// builds of unrelated changes).
    #[inline(always)]
    fn map_point(&self, f: &dyn Fn(f64, f64) -> (f64, f64), x: f64, y: f64) -> (f64, f64) {
        let b = &self.bbox;
        let (u, v) = b.to_unit(x, y);
        let (u, v) = f(u, v);
        let (x, y) = b.from_unit(u, v);
        let (x, y) = self.affine(x, y);
        // A map may send a point arbitrarily far (V09-S45: Trails' runaway
        // feedback); far past every tick (2⁶⁴) and row, the integer work
        // below (lengths, ends, row offsets in i128 / i64) must not overflow.
        // The Output drops or clamps such notes by its policies either way.
        (
            x.clamp(-MAP_X_CAP, MAP_X_CAP),
            y.clamp(-MAP_Y_CAP, MAP_Y_CAP),
        )
    }

    /// Maps note `n` with the unit-space map `f` (then the shared affine part)
    /// and rasterizes the result into `out`. Returns false once `out` is full.
    pub fn rasterize(
        &self,
        n: &TNote,
        f: &dyn Fn(f64, f64) -> (f64, f64),
        tracks: &[SourceTrack],
        out: &mut Output,
    ) -> bool {
        let y = f64::from(n.pitch) + 0.5;
        let (xs, xe) = (n.start as f64, n.end() as f64);
        // Segments along the centre line: (x0, x1, row) after mapping.
        let mut segs: Vec<(f64, f64, f64)> = Vec::new();
        match self.raster {
            Raster::SampleStart | Raster::SampleMid => {
                let (a, ya) = self.map_point(f, xs, y);
                let (e, _) = self.map_point(f, xe, y);
                let row = if self.raster == Raster::SampleStart {
                    ya
                } else {
                    self.map_point(f, (xs + xe) / 2.0, y).1
                };
                segs.push((a.min(e), a.max(e), row));
            }
            Raster::Split => {
                let k = self.segments.max(1);
                for i in 0..k {
                    let x0 = xs + (xe - xs) * f64::from(i) / f64::from(k);
                    let x1 = xs + (xe - xs) * f64::from(i + 1) / f64::from(k);
                    let (a, _) = self.map_point(f, x0, y);
                    let (e, _) = self.map_point(f, x1, y);
                    let (_, row) = self.map_point(f, (x0 + x1) / 2.0, y);
                    segs.push((a.min(e), a.max(e), row));
                }
            }
        }
        // Vertical extent of the note's row after mapping (for Fill): the
        // mapped bottom and top edges at the note's middle.
        let fill_rows = self.fill.map(|fl| {
            let xm = (xs + xe) / 2.0;
            let lo = self.map_point(f, xm, f64::from(n.pitch)).1;
            let hi = self.map_point(f, xm, f64::from(n.pitch) + 1.0).1;
            (fl, lo.min(hi), lo.max(hi))
        });
        let mut last: Option<(i128, i128, i64)> = None;
        let flush = |seg: (i128, i128, i64), out: &mut Output| -> bool {
            let (s, e, row) = seg;
            let d = (e - s).max(1);
            match fill_rows {
                None => out.emit(s, d, row, i64::from(n.vel), n),
                Some((fl, lo, hi)) => {
                    // Rows whose centre lies inside [lo, hi) — only rows that
                    // exist on the keyboard (0–127, or 0–255 in 256-Key Mode):
                    // filling rows beyond it would only pile wrapped copies
                    // onto the same rows — and always the own row (subject to
                    // the boundary policy).
                    let first = ((lo - 0.5).ceil() as i64).max(0);
                    let last_row = ((hi - 0.5).ceil() as i64)
                        .saturating_sub(1)
                        .max(first)
                        .min(self.top);
                    let mut ok = true;
                    let mut prev_row: Option<i64> = None;
                    for q in first..=last_row {
                        // abs_diff: a mapped row far off the keyboard
                        // (saturated at i64's ends) must not overflow.
                        let k = q.abs_diff(row);
                        // Every res-th row of the piano (a common lattice for all
                        // notes, so stripes line up), and always the own row.
                        if q != row && q.rem_euclid(i64::from(fl.res)) != 0 {
                            continue;
                        }
                        if fl.merge && prev_row == Some(q) {
                            continue;
                        }
                        prev_row = Some(q);
                        let mut t = *n;
                        if !fl.keep_sub
                            && q != row
                            && let Some(tr) = tracks.get(usize::from(n.track))
                        {
                            let (id, ch) = tr.sub_offset(n.sub, q - row);
                            t.sub = id;
                            t.ch = ch;
                        }
                        let vel = if fl.fade && q != row {
                            let span = (hi - lo).max(1.0);
                            let fall = 1.0 - (k as f64 / span).min(0.9);
                            (f64::from(n.vel) * fall).round() as i64
                        } else {
                            i64::from(n.vel)
                        };
                        ok &= out.emit(s, d, q, vel.max(1), &t);
                    }
                    if !(first..=last_row).contains(&row) {
                        ok &= out.emit(s, d, row, i64::from(n.vel), n);
                    }
                    ok
                }
            }
        };
        for (a, e, r) in segs {
            let seg = (a.round() as i128, e.round() as i128, r.floor() as i64);
            match &mut last {
                Some(l) if l.2 == seg.2 && seg.0 <= l.1 => l.1 = l.1.max(seg.1),
                _ => {
                    if let Some(l) = last.take()
                        && !flush(l, out)
                    {
                        return false;
                    }
                    last = Some(seg);
                }
            }
        }
        match last {
            Some(l) => flush(l, out),
            None => true,
        }
    }
}

// ================================================================ Scale Y cleanup

/// B2-F28 (contract §022): the destructive cleanup a Scale Y with "Clean up
/// Scale Y overlaps" applies to its own results — on a shrink (handles: any
/// shrink, or any Fill) — in Preview and Apply alike (one kernel). Partners
/// are only notes of the same selection on the same Track, Subchannel, MIDI
/// channel and key; other keys, layers and unselected notes are never touched.
/// Notes already stacked or overlapping in the selection are cleaned like the
/// ones the scale makes collide (the owner asked for a destructive cleanup;
/// Undo restores every original). Ties keep the earlier note in source order
/// (stable sort). No mode ever makes a zero-length note, and a selected
/// zero-length note goes only as a duplicate or overlap of another note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScaleYCleanup {
    /// Same start and length on one key and layer (velocity may differ): one
    /// note stays, the first in source order. Partial overlaps stay. Default.
    #[default]
    ExactDuplicates,
    /// Equal starts keep the longest note (ties: source order); a note still
    /// sounding at the next attack on its key is cut there.
    TrimEarlier,
    /// The earliest note stays (ties: the longest, then source order) and
    /// every note that starts before it ends is removed whole — the beta1
    /// (B1-0116) result, now applied to the whole selection.
    RemoveOverlapping,
}

impl ScaleYCleanup {
    pub const ALL: [ScaleYCleanup; 3] = [
        ScaleYCleanup::ExactDuplicates,
        ScaleYCleanup::TrimEarlier,
        ScaleYCleanup::RemoveOverlapping,
    ];

    /// The settings key (`scale_y.cleanup`).
    pub fn key(self) -> &'static str {
        match self {
            ScaleYCleanup::ExactDuplicates => "exact",
            ScaleYCleanup::TrimEarlier => "trim",
            ScaleYCleanup::RemoveOverlapping => "remove",
        }
    }

    pub fn from_key(key: &str) -> Option<ScaleYCleanup> {
        ScaleYCleanup::ALL.into_iter().find(|m| m.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleYCleanup::ExactDuplicates => "Exact duplicates only (default)",
            ScaleYCleanup::TrimEarlier => "Trim earlier overlapping gates",
            ScaleYCleanup::RemoveOverlapping => "Remove later overlapping notes",
        }
    }
}

/// Applies `mode` to `notes` in place (sorted by [`super::output::sort_notes`]
/// afterwards). O(N log N): one stable sort and one pass, also for a
/// million-note stack on one key.
pub fn clean_scale_y(notes: &mut Vec<TNote>, mode: ScaleYCleanup) -> super::overlap::Report {
    use std::cmp::Reverse;
    let mut report = super::overlap::Report::default();
    let layer = |n: &TNote| (n.track, n.sub, n.ch, n.pitch);
    match mode {
        ScaleYCleanup::ExactDuplicates => notes.sort_by_key(|n| (layer(n), n.start, n.dur)),
        ScaleYCleanup::TrimEarlier | ScaleYCleanup::RemoveOverlapping => {
            notes.sort_by_key(|n| (layer(n), n.start, Reverse(n.dur)))
        }
    }
    let mut write = 0;
    // End of the last kept note on the current layer (RemoveOverlapping).
    let mut kept_end = 0u64;
    for read in 0..notes.len() {
        let n = notes[read];
        let same_layer = write > 0 && layer(&notes[write - 1]) == layer(&n);
        if same_layer {
            let prev = &mut notes[write - 1];
            match mode {
                ScaleYCleanup::ExactDuplicates => {
                    if (prev.start, prev.dur) == (n.start, n.dur) {
                        report.removed += 1;
                        continue;
                    }
                }
                ScaleYCleanup::TrimEarlier => {
                    if prev.start == n.start {
                        report.removed += 1;
                        continue;
                    }
                    if prev.start.saturating_add(prev.dur) > n.start {
                        // prev.start < n.start: never zero-length.
                        prev.dur = n.start - prev.start;
                        report.trimmed += 1;
                    }
                }
                ScaleYCleanup::RemoveOverlapping => {
                    if n.start < kept_end || n.start == prev.start {
                        report.removed += 1;
                        continue;
                    }
                }
            }
        }
        if !same_layer {
            kept_end = 0;
        }
        kept_end = kept_end.max(n.start.saturating_add(n.dur));
        notes[write] = n;
        write += 1;
    }
    notes.truncate(write);
    super::output::sort_notes(notes);
    report
}

// ================================================================ Box scale

/// Exact box scaling (Piano Roll handles): the selection box `[t0, t1) ×
/// [p0, p1]` maps onto `[nt0, nt1) × [np0, np1]` with integer rational
/// arithmetic (round half up): `x' = nt0 + (x − t0)·W'/W`. Rows map by their
/// lower edge (`row' = np0 + ⌊(p − p0)·R'/R⌋`), so the source box's rows land
/// exactly on the target box; with [`Fill`] each row fills every target row of
/// its cell `[⌊(p − p0)·R'/R⌋, ⌊(p + 1 − p0)·R'/R⌋)`; with
/// [`FillMode::Step`] it gets copies a pitch step apart instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxScale {
    pub from: GeomBox,
    pub to: GeomBox,
    pub fill: Option<Fill>,
    /// B2-F28 "Clean up Scale Y overlaps": on a shrink, or with [`Fill`],
    /// the results are cleaned by [`clean_scale_y`] with [`Self::cleanup`]
    /// (beta1's B1-0116 spared overlaps the source already had; beta2 cleans
    /// the whole selection). Off: only the v0.9 §086 shrink cleanup of
    /// identical results ([`CollapseFilter`]).
    pub prevent: bool,
    pub cleanup: ScaleYCleanup,
}

impl BoxScale {
    /// A scale without Duplicate Notes or overlap prevention.
    pub fn plain(from: GeomBox, to: GeomBox) -> BoxScale {
        BoxScale {
            from,
            to,
            fill: None,
            prevent: false,
            cleanup: ScaleYCleanup::default(),
        }
    }

    #[inline]
    fn map_x(&self, x: u64) -> i128 {
        let w = i128::from(self.from.t1 - self.from.t0).max(1);
        let w2 = i128::from(self.to.t1) - i128::from(self.to.t0);
        let num = (i128::from(x) - i128::from(self.from.t0)) * w2;
        // round half up of num / w (w > 0)
        i128::from(self.to.t0) + (2 * num + w).div_euclid(2 * w)
    }

    /// Rows of the source box and of the target box.
    #[inline]
    fn row_counts(&self) -> (i64, i64) {
        (
            i64::from(self.from.p1.saturating_sub(self.from.p0)) + 1,
            i64::from(self.to.p1.saturating_sub(self.to.p0)) + 1,
        )
    }

    /// Target rows `[a, b)` of source row `p` (relative scaling of row edges).
    #[inline]
    fn rows_of(&self, p: u8) -> (i64, i64) {
        let (r, r2) = self.row_counts();
        let k = i64::from(p) - i64::from(self.from.p0);
        let a = i64::from(self.to.p0) + (k * r2).div_euclid(r);
        let b = i64::from(self.to.p0) + ((k + 1) * r2).div_euclid(r);
        (a, b.max(a + 1))
    }

    /// [`FillMode::Step`]: (step, copies per note, kept inside the box).
    fn step_copies(&self, step: u8, copies: u8) -> (i64, i64, bool) {
        let step = i64::from(step.clamp(1, MAX_FILL_STEP));
        if copies == 0 {
            // Auto: ⌈R'/R⌉ − 1 copies one step apart.
            let (r, r2) = self.row_counts();
            (step, ((r2 - 1) / r).max(0), true)
        } else {
            (step, i64::from(copies.min(MAX_FILL_COPIES)), false)
        }
    }

    /// Most notes one source note can become.
    pub fn per_note(&self) -> u64 {
        match self.fill {
            None => 1,
            Some(Fill {
                mode: FillMode::Step { step, copies },
                ..
            }) => self.step_copies(step, copies).1 as u64 + 1,
            Some(f) => {
                let (r, r2) = self.row_counts();
                ((r2 as u64).div_ceil(r as u64) / u64::from(f.res.max(1))).max(1) + 1
            }
        }
    }

    /// Changes the number of rows (the Y extent): what makes overlap
    /// prevention "Scale Y-specific" on the handles.
    pub fn scales_y(&self) -> bool {
        self.row_counts().0 != self.row_counts().1
    }
}

impl Kernel for BoxScale {
    fn growth(&self) -> Growth {
        match self.fill {
            None => Growth::Map,
            Some(_) => Growth::Expand(self.per_note()),
        }
    }

    fn estimate(&self, stats: &SourceStats) -> Estimate {
        Estimate::from_growth(self.growth(), stats)
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        Some(self)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        // v0.9 §086: a shrinking axis can collapse different notes onto the
        // same spot; those new duplicates are removed (sources are sorted by
        // start and the time map is monotone, so equal results are adjacent).
        // B1-0116: with overlap prevention, every overlap the scale creates.
        let mut filter = (!self.prevent && self.shrinks()).then(CollapseFilter::default);
        let cleanup = self.prevent && (self.shrinks() || self.fill.is_some());
        let first = out.notes.len();
        for n in &src.notes {
            let before = out.notes.len();
            self.map_with(n, Some(src), out);
            let from = NoteSpot::of(n);
            if let Some(f) = filter.as_mut() {
                for q in &out.notes[before..] {
                    f.push(NoteSpot::of(q), from);
                }
            }
            out.check()?;
        }
        if let Some(f) = filter {
            out.stats.collapsed += f.apply_from(&mut out.notes, first) as u64;
        }
        if cleanup {
            let mut result = out.notes.split_off(first);
            let report = clean_scale_y(&mut result, self.cleanup);
            out.stats.overlaps_removed += report.removed;
            out.stats.overlaps_trimmed += report.trimmed;
            out.notes.extend(result);
        }
        Ok(())
    }
}

impl BoxScale {
    /// The target box is narrower or has fewer rows than the source box.
    pub fn shrinks(&self) -> bool {
        self.to.t1.saturating_sub(self.to.t0) < self.from.t1 - self.from.t0
            || self.to.p1.saturating_sub(self.to.p0) < self.from.p1 - self.from.p0
    }

    /// One copy (`k` = its distance in rows from the note's own row, 0 the
    /// note itself): gate inherited, velocity full or fading over `span`
    /// rows, subchannel kept or stepped once per row.
    #[allow(clippy::too_many_arguments)]
    fn emit_copy(
        n: &TNote,
        fl: &Fill,
        tracks: &[SourceTrack],
        s: i128,
        d: i128,
        q: i64,
        k: u64,
        span: f64,
        out: &mut Output,
    ) -> bool {
        let mut t = *n;
        if !fl.keep_sub
            && k > 0
            && let Some(tr) = tracks.get(usize::from(n.track))
        {
            let (id, ch) = tr.sub_offset(n.sub, k as i64);
            t.sub = id;
            t.ch = ch;
        }
        let vel = if fl.fade && k > 0 {
            let fall = 1.0 - (k as f64 / span.max(1.0)).min(0.9);
            (f64::from(n.vel) * fall).round() as i64
        } else {
            i64::from(n.vel)
        };
        out.emit(s, d, q, vel.max(1), &t)
    }

    fn map_with(&self, n: &TNote, src: Option<&Source>, out: &mut Output) {
        let tracks: &[SourceTrack] = src.map_or(&[], |s| s.tracks.as_slice());
        let s = self.map_x(n.start);
        let e = self.map_x(n.end());
        let (s, e) = (s.min(e), s.max(e));
        let d = (e - s).max(1);
        let (a, b) = self.rows_of(n.pitch);
        match self.fill {
            None => {
                out.emit(s, d, a, i64::from(n.vel), n);
            }
            Some(
                fl @ Fill {
                    mode: FillMode::Step { step, copies },
                    ..
                },
            ) => {
                let (step, count, inside) = self.step_copies(step, copies);
                let top = i64::from(self.to.p1);
                for i in 0..=count {
                    let q = a + i * step;
                    if i > 0 && inside && q > top {
                        break;
                    }
                    let k = (q - a) as u64;
                    let span = (count * step + 1) as f64;
                    if !Self::emit_copy(n, &fl, tracks, s, d, q, k, span, out) {
                        return;
                    }
                }
            }
            Some(fl) => {
                for q in a..b {
                    let k = (q - a) as u64;
                    // Same lattice as `Geom::rasterize`: every res-th piano row,
                    // and always the note's own (first) row.
                    if k > 0 && q.rem_euclid(i64::from(fl.res.max(1))) != 0 {
                        continue;
                    }
                    if !Self::emit_copy(n, &fl, tracks, s, d, q, k, (b - a) as f64, out) {
                        return;
                    }
                }
            }
        }
    }
}

impl PointKernel for BoxScale {
    fn map(&self, n: &TNote, out: &mut Output) {
        self.map_with(n, None, out);
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        // Inverse of the time map (conservative: the whole source box when
        // the target is degenerate).
        let w = i128::from(self.from.t1 - self.from.t0).max(1);
        let w2 = i128::from(self.to.t1) - i128::from(self.to.t0);
        if w2 <= 0 {
            return (0, u64::MAX);
        }
        let inv = |x: u64| -> i128 {
            i128::from(self.from.t0) + (i128::from(x) - i128::from(self.to.t0)) * w / w2
        };
        let a = (inv(t0) - 2).max(0);
        let b = inv(t1) + 2;
        (
            u64::try_from(a).unwrap_or(0).min(self.from.t0.min(t0)),
            u64::try_from(b.max(0)).unwrap_or(u64::MAX),
        )
    }
}

// ======================================================= Shrink cleanup

/// Where a note sits, for the Scale shrink cleanup (v0.9 §086): track,
/// subchannel, MIDI channel, pitch, start and end. Velocity, release, group
/// and mute do not make two notes at one spot different.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteSpot {
    pub track: u16,
    pub sub: u32,
    pub ch: u8,
    pub pitch: u8,
    pub end: u64,
    pub start: u64,
}

impl NoteSpot {
    pub fn of(n: &TNote) -> NoteSpot {
        NoteSpot {
            track: n.track,
            sub: n.sub,
            ch: n.ch,
            pitch: n.pitch,
            end: n.start.saturating_add(n.dur),
            start: n.start,
        }
    }

    /// A note of one track (the editor's Scale X / Y).
    pub fn of_record(r: &crate::model::NoteRecord) -> NoteSpot {
        NoteSpot {
            track: 0,
            sub: r.sub.0,
            ch: r.attrs.channel(),
            pitch: r.attrs.pitch(),
            end: r.start_tick.saturating_add(r.duration_ticks),
            start: r.start_tick,
        }
    }
}

/// The Scale shrink cleanup (v0.9 §086, §259), shared by the scale handles,
/// Scale X / Y and Scale Y Fill: when a shrinking scale puts results of
/// **different** source notes on the same [`NoteSpot`], only one of them
/// stays. Results of source notes that were already identical (an intentional
/// stacked note) are never removed: in each group of equal results the source
/// spot with the most results stays (ties: the earliest), the others go.
/// Overlaps that are not identical are untouched; broader overlap removal is
/// an explicit tool.
///
/// Results are pushed in order with their source; results with equal starts
/// must arrive together (true for a monotone time map over notes sorted by
/// start — every Scale). Only one group of equal starts is held at a time.
#[derive(Debug, Default)]
pub struct CollapseFilter {
    group: Vec<(NoteSpot, NoteSpot, usize)>,
    next: usize,
    drop: Vec<usize>,
    /// Scratch for the quick test of a group (one hash per result).
    hashes: Vec<u64>,
}

impl CollapseFilter {
    /// Quick test: can two results of this group (same start) be equal?
    /// Hashes of the rest of the spot, sorted; only a repeated hash needs the
    /// exact comparison (most groups are chords on different keys).
    fn may_repeat(g: &[(NoteSpot, NoteSpot, usize)], hashes: &mut Vec<u64>) -> bool {
        hashes.clear();
        hashes.extend(g.iter().map(|e| {
            let r = &e.0;
            let a = u64::from(r.pitch) | u64::from(r.ch) << 8 | u64::from(r.track) << 16;
            let b = u64::from(r.sub) ^ r.end.rotate_left(29);
            (a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b).wrapping_mul(0xD6E8_FEB8_6659_FD93)
        }));
        hashes.sort_unstable();
        hashes.windows(2).any(|w| w[0] == w[1])
    }

    /// Result number `n` (counting from 0 in push order) from `source`.
    pub fn push(&mut self, result: NoteSpot, source: NoteSpot) {
        if self
            .group
            .first()
            .is_some_and(|g| g.0.start != result.start)
        {
            self.flush();
        }
        self.group.push((result, source, self.next));
        self.next += 1;
    }

    fn flush(&mut self) {
        let g = &mut self.group;
        if g.len() >= 2 && Self::may_repeat(g, &mut self.hashes) {
            g.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
            let mut i = 0;
            while i < g.len() {
                let mut j = i + 1;
                while j < g.len() && g[j].0 == g[i].0 {
                    j += 1;
                }
                if j - i >= 2 && g[i..j].iter().any(|e| e.1 != g[i].1) {
                    // The source spot with the most results (ties: earliest).
                    let mut srcs: Vec<(NoteSpot, usize)> =
                        g[i..j].iter().map(|e| (e.1, e.2)).collect();
                    srcs.sort_unstable();
                    let (mut best, mut best_n, mut best_first) = (srcs[0].0, 0usize, usize::MAX);
                    let mut a = 0;
                    while a < srcs.len() {
                        let mut b = a + 1;
                        while b < srcs.len() && srcs[b].0 == srcs[a].0 {
                            b += 1;
                        }
                        let (n, first) = (b - a, srcs[a].1);
                        if n > best_n || (n == best_n && first < best_first) {
                            (best, best_n, best_first) = (srcs[a].0, n, first);
                        }
                        a = b;
                    }
                    self.drop
                        .extend(g[i..j].iter().filter(|e| e.1 != best).map(|e| e.2));
                }
                i = j;
            }
        }
        g.clear();
    }

    /// The results to remove (push numbers, ascending).
    pub fn finish(mut self) -> Vec<usize> {
        self.flush();
        self.drop.sort_unstable();
        self.drop
    }

    /// Removes the collapsed results from `results` (pushed in this order);
    /// returns how many were removed.
    pub fn apply<T>(self, results: &mut Vec<T>) -> usize {
        self.apply_from(results, 0)
    }

    /// [`Self::apply`] when the results were pushed from `results[first..]`.
    pub fn apply_from<T>(self, results: &mut Vec<T>, first: usize) -> usize {
        drop_indices(results, first, &self.finish())
    }
}

/// Removes `results[first + i]` for every `i` of `drop` (ascending); returns
/// how many were removed.
fn drop_indices<T>(results: &mut Vec<T>, first: usize, drop: &[usize]) -> usize {
    if !drop.is_empty() {
        let (mut k, mut i) = (0, 0usize);
        results.retain(|_| {
            let keep = i < first || drop.get(k) != Some(&(i - first));
            k += usize::from(!keep);
            i += 1;
            keep
        });
    }
    drop.len()
}

// ================================================= Scale Y overlap prevention

/// Scale Y overlap prevention (B1-0116), shared by the scale handles, Scale Y
/// and its Duplicate Notes modes.
///
/// Two results **conflict** when they sit on the same track, subchannel,
/// MIDI channel and pitch and their intervals `[start, end)` overlap, unless
/// the source already had that overlap: both are the same kind of result
/// (the same copy distance `rank`, 0 = the scaled note itself) of sources on
/// one key (same track, subchannel, channel and pitch) — a scale maps time
/// monotonically, so such an overlap existed before it ran (same-pitch
/// polyphony, an intentional stack of identical notes; a shrink that makes
/// two such notes identical is the §086 collapse and still conflicts). Chords
/// on different keys never conflict. Of two conflicting results the lower
/// rank stays (scaled notes before generated copies; ties: the one pushed
/// first); broader overlap removal is the General Note Overlap Prevention.
///
/// Results are pushed in order. While their starts never decrease (every
/// scale over notes sorted by start) the filter only holds results that can
/// still overlap a later one; otherwise it keeps every result (still exact).
#[derive(Debug, Default)]
pub struct OverlapFilter {
    active: std::collections::HashMap<(u16, u32, u8, u8), Vec<OverlapEntry>>,
    drop: Vec<usize>,
    next: usize,
    last_start: u64,
    unsorted: bool,
    since_sweep: usize,
}

#[derive(Clone, Copy, Debug)]
struct OverlapEntry {
    spot: NoteSpot,
    source: NoteSpot,
    rank: u64,
    index: usize,
}

impl OverlapEntry {
    fn overlaps(&self, r: &NoteSpot) -> bool {
        self.spot.start < r.end.max(r.start + 1) && r.start < self.spot.end.max(self.spot.start + 1)
    }

    /// The overlap of `self` and a result (`spot`, `rank`, `source`) was in
    /// the source already.
    fn intentional(&self, spot: &NoteSpot, rank: u64, source: &NoteSpot) -> bool {
        let key = |s: &NoteSpot| (s.track, s.sub, s.ch, s.pitch);
        self.rank == rank
            && key(&self.source) == key(source)
            && (self.spot != *spot || self.source == *source)
    }
}

impl OverlapFilter {
    /// The next result (counting from 0 in push order), `rank` rows from its
    /// source's scaled row (0 = the scaled note itself), from `source`.
    pub fn push(&mut self, result: NoteSpot, rank: u64, source: NoteSpot) {
        let index = self.next;
        self.next += 1;
        if result.start < self.last_start {
            self.unsorted = true;
        }
        self.last_start = self.last_start.max(result.start);
        self.since_sweep += 1;
        if self.since_sweep >= 1 << 16 && !self.unsorted {
            self.since_sweep = 0;
            let now = self.last_start;
            self.active.retain(|_, v| {
                v.retain(|e| e.spot.end.max(e.spot.start + 1) > now);
                !v.is_empty()
            });
        }
        let list = self
            .active
            .entry((result.track, result.sub, result.ch, result.pitch))
            .or_default();
        if !self.unsorted {
            let now = result.start;
            list.retain(|e| e.spot.end.max(e.spot.start + 1) > now);
        }
        let conflicts =
            |e: &OverlapEntry| e.overlaps(&result) && !e.intentional(&result, rank, &source);
        if list.iter().any(|e| conflicts(e) && e.rank <= rank) {
            // An earlier result of the same or a lower rank stays.
            self.drop.push(index);
            return;
        }
        // This result outranks every result it conflicts with.
        let drop = &mut self.drop;
        list.retain(|e| {
            let lose = conflicts(e);
            if lose {
                drop.push(e.index);
            }
            !lose
        });
        list.push(OverlapEntry {
            spot: result,
            source,
            rank,
            index,
        });
    }

    /// The results to remove (push numbers, ascending).
    pub fn finish(mut self) -> Vec<usize> {
        self.drop.sort_unstable();
        self.drop
    }

    /// Removes the conflicting results from `results` (pushed in this order);
    /// returns how many were removed.
    pub fn apply<T>(self, results: &mut Vec<T>) -> usize {
        self.apply_from(results, 0)
    }

    /// [`Self::apply`] when the results were pushed from `results[first..]`.
    pub fn apply_from<T>(self, results: &mut Vec<T>, first: usize) -> usize {
        drop_indices(results, first, &self.finish())
    }
}

// ============================================================ Corner gestures

/// A corner of the selection box, in the order of a perspective quad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Corner {
    BottomLeft,
    BottomRight,
    TopRight,
    TopLeft,
}

impl Corner {
    /// Quad order: bottom left, bottom right, top right, top left.
    pub const ALL: [Corner; 4] = [
        Corner::BottomLeft,
        Corner::BottomRight,
        Corner::TopRight,
        Corner::TopLeft,
    ];

    pub fn is_right(self) -> bool {
        matches!(self, Corner::BottomRight | Corner::TopRight)
    }

    pub fn is_top(self) -> bool {
        matches!(self, Corner::TopRight | Corner::TopLeft)
    }

    /// The corner in unit coordinates of the box (u = time, v = pitch rows).
    pub fn unit(self) -> (f64, f64) {
        (
            if self.is_right() { 1.0 } else { 0.0 },
            if self.is_top() { 1.0 } else { 0.0 },
        )
    }
}

/// What a direct corner drag does (§084, §085).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WarpKind {
    /// Ctrl + drag along time: the dragged corner's horizontal edge (the top
    /// or the bottom of the box) slides in time; the opposite edge's two
    /// corners stay.
    SkewX,
    /// Ctrl + drag along pitch: the dragged corner's vertical edge (the left
    /// or the right of the box) slides in pitch; the opposite edge's two
    /// corners stay.
    SkewY,
    /// Ctrl+Shift: only the dragged corner moves (free Perspective); the
    /// other three stay.
    Perspective,
    /// B1-0091 Alt + corner drag: a rotation about the selection box's
    /// center, as it looks on screen. `dt` = the angle in millidegrees
    /// (counter-clockwise on screen), `dr` = the ticks one row is tall on
    /// screen when the drag began, ×1000 (the view's aspect).
    RotateCenter,
    /// B1-0091: the same about the corner opposite the dragged one.
    RotateCorner,
}

impl WarpKind {
    /// A rotation (Alt + corner).
    pub fn is_rotation(self) -> bool {
        matches!(self, WarpKind::RotateCenter | WarpKind::RotateCorner)
    }
}

/// A direct corner gesture on the selection box: `corner` dragged by `dt`
/// ticks and `dr` pitch rows. The continuous maps are the canonical ones —
/// Skew's X / Y shear ([`super::tools::geometry`] `skew_x` / `skew_y`, the
/// pivot on the anchored edge) and Perspective's four-corner homography
/// (`square_to_quad`) — rasterized by the shared [`Geom::rasterize`], so a
/// gesture gives exactly the notes of the Skew / Perspective tool with the
/// equivalent parameters, without their parameter ranges or ‰ grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CornerWarp {
    pub kind: WarpKind,
    pub corner: Corner,
    pub dt: i64,
    pub dr: i64,
}

/// The unit-space map of a [`CornerWarp`].
#[derive(Clone, Copy, Debug)]
enum CornerMap {
    SkewX { k: f64, pivot_v: f64 },
    SkewY { a: f64, pivot_u: f64 },
    Quad { h: [f64; 9] },
}

impl CornerMap {
    #[inline]
    fn apply(&self, u: f64, v: f64) -> (f64, f64) {
        use super::tools::geometry::{apply_h, skew_x, skew_y};
        match *self {
            CornerMap::SkewX { k, pivot_v } => (skew_x(u, v - pivot_v, k), v),
            CornerMap::SkewY { a, pivot_u } => (u, skew_y(v, u - pivot_u, a)),
            CornerMap::Quad { h } => apply_h(&h, u, v),
        }
    }
}

/// Largest |angle| a rotation keeps (millidegrees; any whole number of turns
/// is the same picture).
const MAX_MDEG: i64 = 360_000;

impl CornerWarp {
    /// B1-0091: a rotation by `degrees` (counter-clockwise on screen) about
    /// the box's center or the corner opposite `corner`, for a view where one
    /// row is `ticks_per_row` ticks tall on screen.
    pub fn rotation(
        corner: Corner,
        about_center: bool,
        degrees: f64,
        ticks_per_row: f64,
    ) -> CornerWarp {
        let mdeg = (degrees * 1000.0).round();
        let mdeg = if mdeg.is_finite() {
            (mdeg as i64).rem_euclid(MAX_MDEG)
        } else {
            0
        };
        let k = (ticks_per_row * 1000.0).round();
        CornerWarp {
            kind: if about_center {
                WarpKind::RotateCenter
            } else {
                WarpKind::RotateCorner
            },
            corner,
            dt: mdeg,
            dr: if k.is_finite() {
                (k as i64).max(1)
            } else {
                1000
            },
        }
    }

    /// A rotation's angle in degrees, in [0, 360).
    pub fn degrees(&self) -> f64 {
        self.dt.rem_euclid(MAX_MDEG) as f64 / 1000.0
    }

    /// The gesture changes nothing (the moving edge / corner is where it was).
    pub fn is_identity(&self) -> bool {
        match self.kind {
            WarpKind::SkewX => self.dt == 0,
            WarpKind::SkewY => self.dr == 0,
            WarpKind::Perspective => self.dt == 0 && self.dr == 0,
            WarpKind::RotateCenter | WarpKind::RotateCorner => self.dt.rem_euclid(MAX_MDEG) == 0,
        }
    }

    /// Whether corner `k` moves with the dragged corner.
    fn moves(&self, k: Corner) -> bool {
        match self.kind {
            WarpKind::SkewX => k.is_top() == self.corner.is_top(),
            WarpKind::SkewY => k.is_right() == self.corner.is_right(),
            WarpKind::Perspective => k == self.corner,
            WarpKind::RotateCenter | WarpKind::RotateCorner => true,
        }
    }

    /// A rotation's pivot in the box's unit coordinates.
    pub fn pivot_unit(&self) -> (f64, f64) {
        match self.kind {
            WarpKind::RotateCorner => {
                let (u, v) = self.corner.unit();
                (1.0 - u, 1.0 - v)
            }
            _ => (0.5, 0.5),
        }
    }

    /// Rotates unit point `(u, v)` of box `b` about the pivot as on screen:
    /// time and pitch are first brought to the same screen scale.
    fn rotate_unit(&self, b: &GeomBox, u: f64, v: f64) -> (f64, f64) {
        let k = (self.dr.max(1) as f64) / 1000.0;
        // The box's width over its height, as drawn.
        let a = (b.width() / (b.rows() * k)).max(1e-12);
        let (pu, pv) = self.pivot_unit();
        let (sin, cos) = self.degrees().to_radians().sin_cos();
        let (x, y) = ((u - pu) * a, v - pv);
        (pu + (x * cos - y * sin) / a, pv + x * sin + y * cos)
    }

    /// The box's corners after the gesture, in its unit coordinates
    /// (bottom left, bottom right, top right, top left).
    pub fn quad(&self, b: &GeomBox) -> [(f64, f64); 4] {
        let du = self.dt as f64 / b.width();
        let dv = self.dr as f64 / b.rows();
        Corner::ALL.map(|k| {
            let (u, v) = k.unit();
            if !self.moves(k) {
                return (u, v);
            }
            match self.kind {
                WarpKind::SkewX => (u + du, v),
                WarpKind::SkewY => (u, v + dv),
                WarpKind::Perspective => (u + du, v + dv),
                WarpKind::RotateCenter | WarpKind::RotateCorner => self.rotate_unit(b, u, v),
            }
        })
    }

    /// The corners after the gesture in absolute coordinates (ticks, pitch
    /// rows; the box's top edge is `p1 + 1`) — the outline the Piano Roll
    /// draws while dragging.
    pub fn corners(&self, b: &GeomBox) -> [(f64, f64); 4] {
        self.quad(b).map(|(u, v)| b.from_unit(u, v))
    }

    /// A usable gesture: the quad stays strictly convex and keeps its
    /// orientation (a Perspective corner dragged across the diagonal would
    /// fold the plane). Skews always are.
    pub fn valid(&self, b: &GeomBox) -> bool {
        let q = self.quad(b);
        (0..4).all(|i| {
            let (a, o, c) = (q[i], q[(i + 1) % 4], q[(i + 2) % 4]);
            let (e1, e2) = ((o.0 - a.0, o.1 - a.1), (c.0 - o.0, c.1 - o.1));
            e1.0 * e2.1 - e1.1 * e2.0 > 1e-9
        })
    }

    fn map(&self, b: &GeomBox) -> CornerMap {
        let c = self.corner;
        match self.kind {
            // The moving edge sits at height 1 above the pivot (the anchored
            // edge): the top edge moves by +k, the bottom edge by −k.
            WarpKind::SkewX => {
                let k = self.dt as f64 / b.width();
                if c.is_top() {
                    CornerMap::SkewX { k, pivot_v: 0.0 }
                } else {
                    CornerMap::SkewX {
                        k: -k,
                        pivot_v: 1.0,
                    }
                }
            }
            WarpKind::SkewY => {
                let a = self.dr as f64 / b.rows();
                if c.is_right() {
                    CornerMap::SkewY { a, pivot_u: 0.0 }
                } else {
                    CornerMap::SkewY {
                        a: -a,
                        pivot_u: 1.0,
                    }
                }
            }
            // A rotation is affine, so the homography of its rotated box is
            // exactly it (and gives the Lazy preview's source window).
            WarpKind::Perspective | WarpKind::RotateCenter | WarpKind::RotateCorner => {
                CornerMap::Quad {
                    h: super::tools::geometry::square_to_quad(self.quad(b)),
                }
            }
        }
    }

    /// Where the gesture sends unit point `(u, v)` of box `b` (tests).
    pub fn map_unit(&self, b: &GeomBox, u: f64, v: f64) -> (f64, f64) {
        self.map(b).apply(u, v)
    }

    /// The kernel previewed while dragging and committed on release (one
    /// undo step): the selection box `b` (the handles' box), Duplicate / Fill
    /// when Alt is held, rows up to the project's highest key.
    pub fn kernel(
        &self,
        b: GeomBox,
        fill: Option<Fill>,
        keys: crate::model::project::KeyRange,
    ) -> CornerKernel {
        let mut geom = Geom::identity(b);
        geom.fill = fill;
        geom.top = i64::from(keys.max_pitch());
        CornerKernel {
            warp: *self,
            geom,
            map: self.map(&b),
        }
    }
}

/// The exact, pointwise kernel of a [`CornerWarp`].
#[derive(Clone, Debug)]
pub struct CornerKernel {
    warp: CornerWarp,
    geom: Geom,
    map: CornerMap,
}

impl CornerKernel {
    pub fn warp(&self) -> CornerWarp {
        self.warp
    }
}

impl Kernel for CornerKernel {
    fn growth(&self) -> Growth {
        match self.geom.per_note() {
            1 => Growth::Map,
            n => Growth::Expand(n),
        }
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        Some(self)
    }
}

impl PointKernel for CornerKernel {
    fn map(&self, n: &TNote, out: &mut Output) {
        let m = self.map;
        if self.warp.kind.is_rotation() {
            // B1-0091: a rotated note is cut along its path into as many
            // pieces as rows it crosses (a staircase keeping the cells it
            // covers), never filled.
            let mut g = self.geom;
            g.fill = None;
            g.raster = Raster::Split;
            let b = &g.bbox;
            let y = f64::from(n.pitch) + 0.5;
            let row = |x: f64| {
                let (u, v) = b.to_unit(x, y);
                b.from_unit(m.apply(u, v).0, m.apply(u, v).1).1
            };
            let rows = (row(n.start as f64) - row(n.end() as f64)).abs();
            g.segments = (rows.ceil() as u32)
                .saturating_mul(2)
                .saturating_add(1)
                .clamp(1, 256);
            g.rasterize(n, &|u, v| m.apply(u, v), &[], out);
            return;
        }
        self.geom.rasterize(n, &|u, v| m.apply(u, v), &[], out);
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        let b = &self.geom.bbox;
        // Results round to whole ticks: one tick of slack on each side.
        let pad = |d: u64| (t0.saturating_sub(d), t1.saturating_add(d));
        match self.map {
            // Points of the box move in time by k·(v − pivot)·W, v in [0, 1]:
            // between 0 (the anchored edge) and dt. A result starting before
            // tick 0 is moved to 0 whole, which needs no wider window (its
            // source starts before −shift ≤ t1 − shift).
            CornerMap::SkewX { k, pivot_v } => {
                let w = b.width();
                let (a, e) = (k * (0.0 - pivot_v) * w, k * (1.0 - pivot_v) * w);
                let (lo, hi) = (a.min(e).floor() - 1.0, a.max(e).ceil() + 1.0);
                let start = (t0 as f64 - hi).max(0.0);
                let end = (t1 as f64 - lo).min(u64::MAX as f64);
                (start as u64, end as u64)
            }
            // Time is unchanged.
            CornerMap::SkewY { .. } => pad(1),
            CornerMap::Quad { h } => {
                // The source of the strip [t0, t1]: the part of the quad inside
                // the strip, mapped back (a homography maps convex polygons onto
                // convex polygons, vertices onto vertices).
                // Results starting before tick 0 are moved to 0 whole (the
                // preview's boundary): such a result can end up to the quad's
                // reach left of 0 later than its image.
                let w = b.width();
                let quad = self.warp.quad(b);
                let left = quad
                    .iter()
                    .map(|q| b.t0 as f64 + q.0 * w)
                    .fold(0.0f64, f64::min);
                let ua = (t0 as f64 - 1.0 + left - b.t0 as f64) / w;
                let ub = (t1 as f64 + 1.0 - b.t0 as f64) / w;
                let poly = clip_x(&clip_x(&quad, ua, true), ub, false);
                let Some(inv) = invert3(&h) else {
                    return (0, u64::MAX);
                };
                if poly.is_empty() {
                    return (1, 0);
                }
                let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                for &(x, y) in &poly {
                    let (u, _) = super::tools::geometry::apply_h(&inv, x, y);
                    lo = lo.min(u);
                    hi = hi.max(u);
                }
                let (lo, hi) = (lo.clamp(0.0, 1.0), hi.clamp(0.0, 1.0));
                let a = (b.t0 as f64 + lo * w).floor() - 1.0;
                let e = (b.t0 as f64 + hi * w).ceil() + 1.0;
                (a.max(0.0) as u64, e.min(u64::MAX as f64) as u64)
            }
        }
    }
}

/// Sutherland–Hodgman: the part of polygon `p` with `x ≥ c` (`keep_right`)
/// or `x ≤ c`.
fn clip_x(p: &[(f64, f64)], c: f64, keep_right: bool) -> Vec<(f64, f64)> {
    let inside = |q: (f64, f64)| if keep_right { q.0 >= c } else { q.0 <= c };
    let mut out = Vec::with_capacity(p.len() + 2);
    for i in 0..p.len() {
        let (a, e) = (p[i], p[(i + 1) % p.len()]);
        if inside(a) {
            out.push(a);
        }
        if inside(a) != inside(e) {
            let t = (c - a.0) / (e.0 - a.0);
            out.push((c, a.1 + (e.1 - a.1) * t));
        }
    }
    out
}

/// Inverse of a 3 × 3 matrix (row major), `None` when singular.
fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let c = [
        m[4] * m[8] - m[5] * m[7],
        m[5] * m[6] - m[3] * m[8],
        m[3] * m[7] - m[4] * m[6],
    ];
    let det = m[0] * c[0] + m[1] * c[1] + m[2] * c[2];
    if det.abs() < 1e-15 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        c[0] * d,
        (m[2] * m[7] - m[1] * m[8]) * d,
        (m[1] * m[5] - m[2] * m[4]) * d,
        c[1] * d,
        (m[0] * m[8] - m[2] * m[6]) * d,
        (m[2] * m[3] - m[0] * m[5]) * d,
        c[2] * d,
        (m[1] * m[6] - m[0] * m[7]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ])
}

/// Snaps a dragged box edge to the snap grid (Shift while scaling, §137):
/// `edge` is rounded to the nearest multiple of `snap` (ticks).
/// `x.rem_euclid(1.0)`, bit for bit (signed zeros, the rounding to 1.0 of a
/// tiny negative `x`, NaN / ±inf), without its call to the software `fmod`:
/// `x − ⌊x⌋` is the same single rounding. v0.8 §256: the Wave tool spent
/// about half of its full run in `fmod`, and that loop's speed depended on
/// code placement (+50 % after unrelated code was added). Used by the tools'
/// per-note periodic shapes.
#[inline]
pub fn rem1(x: f64) -> f64 {
    let r = x - x.floor();
    if r == 0.0 { 0.0f64.copysign(x) } else { r }
}

pub fn snap_edge(edge: i128, snap: u64) -> i128 {
    let s = i128::from(snap.max(1));
    (edge + s / 2).div_euclid(s) * s
}

#[cfg(test)]
mod tests {

    #[test]
    fn rem1_equals_rem_euclid_bit_for_bit() {
        let mut vals = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            2.0,
            -2.0,
            0.5,
            -0.5,
            1e-20,
            -1e-20,
            -1e-300,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            1e15 + 0.25,
            -1e15 - 0.25,
            4.5e15,
            -4.5e15,
            1e300,
            -1e300,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            0.999_999_999_999_999_9,
            -0.999_999_999_999_999_9,
        ];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let v = f64::from_bits(x);
            if !v.is_nan() {
                vals.push(v);
            }
            vals.push((x >> 11) as f64 / (1u64 << 40) as f64 - 2048.0);
        }
        for v in vals {
            let (a, b) = (rem1(v), v.rem_euclid(1.0));
            assert!(
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
                "{v:e}: {a:e} vs {b:e}"
            );
        }
    }

    use super::*;
    use crate::transform::Boundary;

    fn note(start: u64, dur: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur,
            pitch,
            vel: 100,
            ..TNote::default()
        }
    }

    fn run_box(k: &BoxScale, notes: &[TNote]) -> Vec<TNote> {
        let mut out = Output::new(Boundary::default(), 1 << 20);
        for n in notes {
            k.map(n, &mut out);
        }
        out.finish().unwrap().0
    }

    #[test]
    fn box_scale_is_exact_and_identity_preserves_notes() {
        let from = GeomBox {
            t0: 0,
            t1: 960,
            p0: 60,
            p1: 63,
        };
        let id = BoxScale {
            from,
            to: from,
            fill: None,
            prevent: false,
            cleanup: Default::default(),
        };
        let ns = [note(0, 480, 60), note(480, 480, 63), note(240, 1, 61)];
        let mut want = ns.to_vec();
        want.sort_by_key(|n| (n.start, n.pitch));
        let got = run_box(&id, &ns);
        assert_eq!(
            got.iter()
                .map(|n| (n.start, n.dur, n.pitch))
                .collect::<Vec<_>>(),
            want.iter()
                .map(|n| (n.start, n.dur, n.pitch))
                .collect::<Vec<_>>()
        );
        // Double width and double height, anchored at the box's start / bottom.
        let k = BoxScale {
            from,
            to: GeomBox {
                t0: 0,
                t1: 1920,
                p0: 60,
                p1: 67,
            },
            fill: None,
            prevent: false,
            cleanup: Default::default(),
        };
        let g = run_box(&k, &[note(480, 480, 61)]);
        assert_eq!((g[0].start, g[0].dur, g[0].pitch), (960, 960, 62));
        // Fill: the row of 61 covers target rows 62 and 63.
        let f = BoxScale {
            fill: Some(Fill::default()),
            ..k
        };
        let g = run_box(&f, &[note(480, 480, 61)]);
        assert_eq!(g.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![62, 63]);
        // A 4-row box filled to 12 rows: every target row is covered once.
        let f = BoxScale {
            from,
            to: GeomBox {
                t0: 0,
                t1: 960,
                p0: 48,
                p1: 59,
            },
            fill: Some(Fill::default()),
            prevent: false,
            cleanup: Default::default(),
        };
        let g = run_box(
            &f,
            &[
                note(0, 10, 60),
                note(0, 10, 61),
                note(0, 10, 62),
                note(0, 10, 63),
            ],
        );
        let rows: Vec<u8> = g.iter().map(|n| n.pitch).collect();
        assert_eq!(rows, (48..=59).collect::<Vec<u8>>());
    }

    #[test]
    fn rasterization_policies_and_fill_are_deterministic() {
        let bbox = GeomBox {
            t0: 0,
            t1: 1000,
            p0: 60,
            p1: 69,
        };
        let mut g = Geom::identity(bbox);
        let n = note(0, 1000, 60);
        let id = |u: f64, v: f64| (u, v);
        let mut out = Output::new(Boundary::default(), 1000);
        assert!(g.rasterize(&n, &id, &[], &mut out));
        let (r, _) = out.finish().unwrap();
        assert_eq!((r[0].start, r[0].dur, r[0].pitch), (0, 1000, 60));
        // A ramp: v rises with u by half the box → split gives a staircase.
        let ramp = |u: f64, v: f64| (u, v + 0.5 * u);
        g.raster = Raster::Split;
        g.segments = 10;
        let mut out = Output::new(Boundary::default(), 1000);
        g.rasterize(&n, &ramp, &[], &mut out);
        let (r, _) = out.finish().unwrap();
        assert!(r.len() > 3);
        assert!(
            r.windows(2)
                .all(|w| w[0].pitch <= w[1].pitch && w[0].end() <= w[1].start + 1)
        );
        // Vertical stretch ×3 with fill: three rows per note.
        g.raster = Raster::SampleMid;
        g.scale = (1.0, 3.0);
        g.pivot = (0.0, 0.0);
        g.fill = Some(Fill::default());
        let mut out = Output::new(Boundary::default(), 1000);
        g.rasterize(&note(0, 10, 61), &id, &[], &mut out);
        let (r, _) = out.finish().unwrap();
        assert_eq!(
            r.iter().map(|n| n.pitch).collect::<Vec<_>>(),
            vec![63, 64, 65]
        );
        assert_eq!(snap_edge(250, 240), 240);
        assert_eq!(snap_edge(-130, 240), -240);
    }
}
