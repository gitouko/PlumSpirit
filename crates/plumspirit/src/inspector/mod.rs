//! The MIDI Inspector (v0.9 V09-S18, `docs/INSPECTOR.md`): what the project
//! contains — the notes the exported file holds, the exact maximum NPS and
//! polyphony, the duration through the tempo map, the timing and tempo — and
//! NPS / polyphony charts over the whole song.
//!
//! Nothing here counts notes itself: totals and peaks come from the shared
//! analysis (`app/analysis.rs`, `Scope::Project`, the MIDI Player's scope, I9)
//! and the export snapshot the Player keeps (I1); the charts are the peak
//! profile's exact per-column maxima (I5), recomputed only where an edit can
//! change them (I6).

use crate::player::Prepared;
use plumspirit_engine::analysis::profile::PeakProfile;
use plumspirit_engine::analysis::{AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::conductor::format_tempo;
use plumspirit_engine::model::timing::TimingKind;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// Most chart columns (I5): at most one per pixel, never more than this.
pub const MAX_COLUMNS: usize = 2_048;

/// The window of the Inspector's NPS (the canonical 1 s, I3).
pub const WINDOW_NS: u64 = DEFAULT_WINDOW_NS;

/// Changed starts and ends examined at once to update the chart in the
/// frame; more, or more than [`SYNC_COLUMNS`] touched columns: a worker.
const SYNC_DELTA_EVENTS: u64 = 1 << 17;
/// Chart columns recomputed at once after an edit (each a few partial block
/// sweeps).
const SYNC_COLUMNS: usize = 128;

/// The Inspector's values (I3), all exact; `None` peaks: the peak profile is
/// still being built.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectorValues {
    /// Notes the exported file holds (I1).
    pub notes: u64,
    /// Notes stored in the project — what a save writes (I2).
    pub stored: u64,
    /// Notes on keys above 127 in the file (256-Key Mode; counted, never
    /// truncated).
    pub wide_keys: u64,
    /// Highest NPS (1 s window) anywhere in the song.
    pub max_nps: Option<u64>,
    /// Highest polyphony anywhere in the song.
    pub max_poly: Option<u64>,
    /// The file's length in document ticks (`ExportSnapshot::end_tick`).
    pub end_tick: u64,
    /// The same through the tempo map.
    pub duration_ns: u64,
    /// The file's timing ("960 PPQN"; the project's too when they differ).
    pub timing: String,
    pub bpm_start: String,
    pub bpm_low: String,
    pub bpm_high: String,
    /// Tempo changes the file holds (ramps as the steps written).
    pub tempo_changes: usize,
}

/// The values of `doc` from the Player's snapshot `prep`, the analysis
/// index `idx` and its peak profile (when ready). `tempo_changes` comes
/// from [`Inspector::tempo_changes`] (cached per snapshot). O(tempo points).
pub fn values(
    doc: &PlumDocument,
    prep: &Prepared,
    idx: &AnalysisIndex,
    timing: &Timing,
    profile: Option<&PeakProfile>,
    tempo_changes: usize,
) -> InspectorValues {
    let scope = Scope::Project;
    let s = &prep.snapshot;
    let last = idx.last_tick(&scope);
    let end_tick = s.end_tick();
    let c = doc.conductor();
    let points = c.tempo_points();
    let low = points
        .iter()
        .min_by(|a, b| a.bpm_f64().total_cmp(&b.bpm_f64()));
    let high = points
        .iter()
        .max_by(|a, b| a.bpm_f64().total_cmp(&b.bpm_f64()));
    let start = format_tempo(c.start_tempo());
    InspectorValues {
        notes: idx.notes_in(&scope),
        stored: doc.stored_note_count() as u64,
        wide_keys: s.plan().wide.notes,
        max_nps: profile.map(|p| p.peak_nps_so_far(idx, &scope, timing, last)),
        max_poly: profile.map(|p| p.peak_poly_so_far(idx, &scope, last)),
        end_tick,
        duration_ns: prep.timing.nanos_at(end_tick),
        timing: timing_text(doc, prep),
        bpm_low: low.map_or_else(|| start.clone(), |p| format_tempo(p.value)),
        bpm_high: high.map_or_else(|| start.clone(), |p| format_tempo(p.value)),
        bpm_start: start,
        tempo_changes,
    }
}

/// One line of the Inspector: label, value and its truthfulness tag (I4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub value: String,
    /// "Exact", "Updating…" or empty for plain facts (timing, counts of
    /// events).
    pub tag: &'static str,
}

pub const EXACT: &str = "Exact";
pub const UPDATING: &str = "Updating…";

/// The Inspector's lines for `v` (I2, I3, I4). `updating`: the values
/// describe an earlier version of the project (shown, marked).
pub fn rows(v: &InspectorValues, updating: bool) -> Vec<Row> {
    let g = plumspirit_engine::midi::group_digits;
    let exact = |known: bool| {
        if updating || !known { UPDATING } else { EXACT }
    };
    let mut out = vec![Row {
        label: "Notes",
        value: g(v.notes),
        tag: exact(true),
    }];
    if v.stored != v.notes {
        let why = if v.stored > v.notes {
            "muted notes are kept but not exported"
        } else {
            "linked clips play their notes again"
        };
        out.push(Row {
            label: "In the project",
            value: format!("{} stored ({why})", g(v.stored)),
            tag: "",
        });
    }
    if v.wide_keys > 0 {
        out.push(Row {
            label: "Keys above 127",
            value: format!(
                "{} notes (a Standard MIDI export asks what to do with them)",
                g(v.wide_keys)
            ),
            tag: exact(true),
        });
    }
    let peak = |p: Option<u64>| p.map_or_else(|| "…".to_string(), g);
    out.push(Row {
        label: "Max NPS",
        value: peak(v.max_nps),
        tag: exact(v.max_nps.is_some()),
    });
    out.push(Row {
        label: "Max polyphony",
        value: peak(v.max_poly),
        tag: exact(v.max_poly.is_some()),
    });
    out.push(Row {
        label: "Duration",
        value: crate::app::format_clock(v.duration_ns as f64 / 1e9),
        tag: exact(true),
    });
    out.push(Row {
        label: "Timing",
        value: v.timing.clone(),
        tag: "",
    });
    out.push(Row {
        label: "BPM",
        value: if v.bpm_low == v.bpm_high {
            v.bpm_start.clone()
        } else {
            format!(
                "{} at the start (lowest {}, highest {})",
                v.bpm_start, v.bpm_low, v.bpm_high
            )
        },
        tag: exact(true),
    });
    out.push(Row {
        label: "Tempo changes",
        value: g(v.tempo_changes as u64),
        tag: "",
    });
    out
}

/// The line the on-save check adds (I7), `now` in seconds since the epoch.
pub fn save_row(check: &SaveCheck, now: u64) -> Option<Row> {
    let value = match check {
        SaveCheck::None => return None,
        SaveCheck::Waiting(_) | SaveCheck::Running(_) => {
            "checking the values against a fresh analysis…".to_string()
        }
        SaveCheck::Done(when, Ok(())) => format!(
            "values checked against a fresh analysis ({})",
            crate::recent::age_text(now, *when)
        ),
        SaveCheck::Done(_, Err(_)) => {
            "a fresh analysis differed; the project was analyzed again (details in the log)"
                .to_string()
        }
    };
    Some(Row {
        label: "Saved",
        value,
        tag: "",
    })
}

/// "960 PPQN" (or SMPTE), and the project's resolution when the file is
/// written at another one.
fn timing_text(doc: &PlumDocument, prep: &Prepared) -> String {
    let describe = |k: TimingKind| match k {
        TimingKind::Ppqn(p) => format!("{p} PPQN"),
        TimingKind::Smpte {
            frames_per_second,
            ticks_per_frame,
        } => {
            let fps = if frames_per_second == 29 {
                "29.97".to_string()
            } else {
                frames_per_second.to_string()
            };
            format!("SMPTE {fps} fps, {ticks_per_frame} ticks per frame")
        }
    };
    let file = plumspirit_engine::model::timing::Division::from_raw(prep.snapshot.division_word());
    let project = doc.division();
    if file.kind() == project.kind() {
        describe(file.kind())
    } else {
        format!(
            "{} (the project: {})",
            describe(file.kind()),
            describe(project.kind())
        )
    }
}

/// Column boundaries over the song's duration in seconds (I5): `cols`
/// equal time spans from tick 0 to `end_tick`, as ticks (`cols + 1` of
/// them, the last one `end_tick`).
pub fn bounds(timing: &Timing, end_tick: u64, cols: usize) -> Vec<u64> {
    let cols = cols.max(1) as u128;
    let dur = u128::from(timing.nanos_at(end_tick));
    let mut v: Vec<u64> = (0..=cols)
        .map(|k| timing.tick_at_nanos((dur * k / cols) as u64))
        .collect();
    if let Some(l) = v.last_mut() {
        *l = end_tick;
    }
    // Rounding never makes them decrease, but keep them ordered for safety.
    for i in 1..v.len() {
        v[i] = v[i].max(v[i - 1]);
    }
    v
}

/// B1-0108: the charts' horizontal axis — song time (seconds through the
/// tempo map) or musical time (ticks, so bars are evenly spaced).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    #[default]
    Time,
    Musical,
}

/// B1-0108: the visible part of the song as fractions `[lo, hi]` of its
/// span on `axis` (0 = the start, 1 = the end).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub axis: Axis,
    pub lo: f64,
    pub hi: f64,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            axis: Axis::Time,
            lo: 0.0,
            hi: 1.0,
        }
    }
}

/// The narrowest window: a millionth of the song (a 40M-note song of ten
/// minutes still zooms to 0.6 ms).
pub const MIN_SPAN: f64 = 1e-6;

impl Window {
    pub fn span(&self) -> f64 {
        self.hi - self.lo
    }

    /// The whole song.
    pub fn is_full(&self) -> bool {
        self.lo <= 0.0 && self.hi >= 1.0
    }

    /// Kept inside the song and at least [`MIN_SPAN`] wide.
    fn clamped(mut self) -> Window {
        let span = self.span().clamp(MIN_SPAN, 1.0);
        if !span.is_finite() {
            return Window {
                axis: self.axis,
                ..Window::default()
            };
        }
        self.lo = self.lo.clamp(0.0, 1.0 - span);
        self.hi = self.lo + span;
        self
    }

    /// Zoomed by `factor` (> 1: in) keeping the point at `at` (fraction of
    /// the visible width) where it is.
    pub fn zoomed(self, factor: f64, at: f64) -> Window {
        let at = at.clamp(0.0, 1.0);
        let pivot = self.lo + at * self.span();
        let span = (self.span() / factor.max(1e-9)).clamp(MIN_SPAN, 1.0);
        Window {
            lo: pivot - at * span,
            hi: pivot - at * span + span,
            ..self
        }
        .clamped()
    }

    /// Moved by `delta` visible widths (positive: later).
    pub fn panned(self, delta: f64) -> Window {
        let d = delta * self.span();
        Window {
            lo: self.lo + d,
            hi: self.hi + d,
            ..self
        }
        .clamped()
    }

    /// Centred on song fraction `at`, `span` wide.
    pub fn around(self, at: f64, span: f64) -> Window {
        let span = span.clamp(MIN_SPAN, 1.0);
        Window {
            lo: at - span / 2.0,
            hi: at + span / 2.0,
            ..self
        }
        .clamped()
    }
}

/// B1-0108: where song tick `tick` falls on `axis`, as a fraction of the
/// song span (`end_tick`; its time through `timing`).
pub fn fraction_of(timing: &Timing, axis: Axis, end_tick: u64, tick: u64) -> f64 {
    match axis {
        Axis::Time => {
            let dur = timing.nanos_at(end_tick).max(1) as f64;
            timing.nanos_at(tick) as f64 / dur
        }
        Axis::Musical => tick as f64 / end_tick.max(1) as f64,
    }
}

/// B1-0108: the song tick at fraction `f` of the song span on `axis`.
pub fn tick_of(timing: &Timing, axis: Axis, end_tick: u64, f: f64) -> u64 {
    let f = f.clamp(0.0, 1.0);
    match axis {
        Axis::Time => {
            let dur = timing.nanos_at(end_tick) as f64;
            timing.tick_at_nanos((dur * f).round() as u64).min(end_tick)
        }
        Axis::Musical => ((end_tick as f64) * f).round() as u64,
    }
}

/// B1-0108: column boundaries over the visible window `w` (`cols` equal
/// spans on its axis), as ticks; `cols + 1` of them, never decreasing. The
/// whole song on the time axis is exactly [`bounds`].
pub fn bounds_in(timing: &Timing, end_tick: u64, w: Window, cols: usize) -> Vec<u64> {
    if w.is_full() && w.axis == Axis::Time {
        return bounds(timing, end_tick, cols);
    }
    let cols = cols.max(1);
    let mut v: Vec<u64> = (0..=cols)
        .map(|k| {
            tick_of(
                timing,
                w.axis,
                end_tick,
                w.lo + w.span() * k as f64 / cols as f64,
            )
        })
        .collect();
    for i in 1..v.len() {
        v[i] = v[i].max(v[i - 1]);
    }
    v
}

/// The tick a click at `fraction` (0 = left edge, 1 = right edge) of a
/// chart with column boundaries `bounds` seeks to: the start of the column
/// under it (V09-0058).
pub fn seek_tick(bounds: &[u64], fraction: f32) -> Option<u64> {
    let n = bounds.len().checked_sub(1).filter(|&n| n > 0)?;
    let k = ((fraction.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
    Some(bounds[k])
}

/// What a chart describes: kept as `Arc`s so an identity can never be
/// reused while the chart names it.
#[derive(Clone)]
struct ChartBasis {
    index: Arc<AnalysisIndex>,
    profile: Arc<PeakProfile>,
    timing: Arc<Timing>,
    end_tick: u64,
    cols: usize,
    /// B1-0108: the visible part of the song.
    window: Window,
    /// Derived from `timing`, `end_tick`, `window` and `cols` ([`bounds_in`]).
    bounds: Vec<u64>,
}

impl ChartBasis {
    /// Same inputs (the bounds follow from them, so a frame never
    /// recomputes them just to compare).
    fn same(
        &self,
        index: &Arc<AnalysisIndex>,
        profile: &Arc<PeakProfile>,
        timing: &Arc<Timing>,
        end_tick: u64,
        cols: usize,
        window: Window,
    ) -> bool {
        Arc::ptr_eq(&self.index, index)
            && Arc::ptr_eq(&self.profile, profile)
            && Arc::ptr_eq(&self.timing, timing)
            && self.end_tick == end_tick
            && self.cols == cols
            && self.window == window
    }
}

struct ChartJob {
    basis: ChartBasis,
    rx: Receiver<Vec<(u64, u64)>>,
}

/// The NPS / polyphony chart columns (I5): `(highest NPS, highest poly)` per
/// column, exact. Kept current by [`Chart::sync`]; after an edit only the
/// columns it can change are recomputed (I6), at once when few, else on a
/// worker while the previous columns stay shown.
#[derive(Default)]
pub struct Chart {
    basis: Option<ChartBasis>,
    pub columns: Vec<(u64, u64)>,
    job: Option<ChartJob>,
    /// Compute everything inline (tests).
    pub inline: bool,
    /// Whole charts computed / columns updated in place (tests, diagnostics).
    pub full_builds: u64,
    pub updates: u64,
}

impl Chart {
    /// The tick boundaries of [`Chart::columns`].
    pub fn bounds(&self) -> &[u64] {
        self.basis.as_ref().map_or(&[], |b| &b.bounds)
    }

    /// Keeps the columns current for `idx` / `profile` / `timing`, `cols`
    /// columns up to `end_tick`. Returns true while a worker computes them
    /// (poll again).
    #[cfg(test)]
    pub fn sync(
        &mut self,
        idx: &Arc<AnalysisIndex>,
        profile: &Arc<PeakProfile>,
        timing: &Arc<Timing>,
        end_tick: u64,
        cols: usize,
    ) -> bool {
        self.sync_in(idx, profile, timing, end_tick, cols, Window::default())
    }

    /// The shown columns were computed for `idx` (not an earlier version
    /// still drawn while the columns of an edit are computed).
    pub fn describes(&self, idx: &Arc<AnalysisIndex>) -> bool {
        self.basis
            .as_ref()
            .is_some_and(|b| Arc::ptr_eq(&b.index, idx))
    }

    /// The window the shown columns describe (they stay drawn, mapped to
    /// the current window, while new ones are computed).
    pub fn window(&self) -> Option<Window> {
        self.basis.as_ref().map(|b| b.window)
    }

    /// B1-0108: [`Chart::sync`] for the visible window `window`: every
    /// column is the exact maximum over its own span, whatever the zoom.
    /// While a worker runs for an older window no second one starts; the
    /// newest window is computed when it is done.
    pub fn sync_in(
        &mut self,
        idx: &Arc<AnalysisIndex>,
        profile: &Arc<PeakProfile>,
        timing: &Arc<Timing>,
        end_tick: u64,
        cols: usize,
        window: Window,
    ) -> bool {
        let cols = cols.clamp(1, MAX_COLUMNS);
        if let Some(j) = &self.job {
            match j.rx.try_recv() {
                Ok(c) => {
                    let j = self.job.take().expect("checked");
                    self.columns = c;
                    self.basis = Some(j.basis);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.job = None,
            }
        }
        if self
            .basis
            .as_ref()
            .is_some_and(|b| b.same(idx, profile, timing, end_tick, cols, window))
        {
            return false;
        }
        if self.job.is_some() {
            // Its result comes first; the newest window follows.
            return true;
        }
        let want = ChartBasis {
            index: Arc::clone(idx),
            profile: Arc::clone(profile),
            timing: Arc::clone(timing),
            end_tick,
            cols,
            window,
            bounds: bounds_in(timing, end_tick, window, cols),
        };
        let scope = Scope::Project;
        // An edit under the same tempo map and layout: only the columns a
        // dirty range touches change.
        let mut dirty = None;
        if let Some(b) = &self.basis
            && Arc::ptr_eq(&b.timing, timing)
            && b.bounds == want.bounds
            && b.columns_match(self.columns.len())
            && let Some(delta) = idx.delta_since_within(&b.index, &scope, SYNC_DELTA_EVENTS)
        {
            let ranges = profile.dirty_ranges(&delta, timing);
            let touched = touched_columns(&want.bounds, &ranges);
            if touched <= SYNC_COLUMNS || self.inline {
                self.columns = profile.chart_columns_update(
                    idx,
                    &scope,
                    timing,
                    &want.bounds,
                    &self.columns,
                    &ranges,
                );
                self.basis = Some(want);
                self.job = None;
                self.updates += 1;
                return false;
            }
            dirty = Some((ranges, self.columns.clone()));
        }
        self.full_builds += u64::from(dirty.is_none());
        if self.inline {
            self.columns = profile.chart_columns(idx, &scope, timing, &want.bounds);
            self.basis = Some(want);
            return false;
        }
        let (i2, p2, t2, b2, s2) = (
            Arc::clone(idx),
            Arc::clone(profile),
            Arc::clone(timing),
            want.bounds.clone(),
            scope.clone(),
        );
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("plumspirit-inspector-chart".into())
            .spawn(move || {
                let c = match dirty {
                    Some((ranges, prev)) => {
                        p2.chart_columns_update(&i2, &s2, &t2, &b2, &prev, &ranges)
                    }
                    None => p2.chart_columns(&i2, &s2, &t2, &b2),
                };
                let _ = tx.send(c);
            });
        match spawned {
            Ok(_) => {
                self.job = Some(ChartJob { basis: want, rx });
                true
            }
            Err(e) => {
                log::warn!("the Inspector chart thread could not start ({e}); computing it inline");
                self.columns = profile.chart_columns(idx, &scope, timing, &want.bounds);
                self.basis = Some(want);
                false
            }
        }
    }

    /// Close Project, on the reaper: a column worker still running
    /// finishes here (its result is dropped).
    pub fn release(mut self) {
        if let Some(j) = self.job.take() {
            drop(j.rx.into_iter().last());
        }
    }

    /// Nothing is kept (the Inspector was closed or the project changed).
    pub fn clear(&mut self) {
        *self = Chart {
            inline: self.inline,
            ..Chart::default()
        };
    }
}

impl ChartBasis {
    fn columns_match(&self, n: usize) -> bool {
        self.bounds.len() == n + 1
    }
}

/// Columns of `bounds` that a range of `ranges` (sorted, merged) touches.
fn touched_columns(bounds: &[u64], ranges: &[(u64, u64)]) -> usize {
    let n = bounds.len().saturating_sub(1);
    (0..n)
        .filter(|&k| {
            let lo = bounds[k];
            let hi = if k + 1 == n {
                bounds[k + 1].max(lo)
            } else {
                bounds[k + 1].saturating_sub(1).max(lo)
            };
            let i = ranges.partition_point(|r| r.1 < lo);
            ranges.get(i).is_some_and(|r| r.0 <= hi)
        })
        .count()
}

/// What an analysis answers for the on-save check (I7): totals, peaks and
/// chart columns over a fixed layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answers {
    pub notes: u64,
    pub max_nps: u64,
    pub max_poly: u64,
    pub columns: Vec<(u64, u64)>,
}

/// Columns of the on-save check's layout.
pub const CHECK_COLUMNS: usize = 512;

/// `idx` / `prof`'s answers over [`CHECK_COLUMNS`] columns of the song.
pub fn answers_of(idx: &AnalysisIndex, timing: &Timing, prof: &PeakProfile) -> Answers {
    let scope = Scope::Project;
    let last = idx.last_tick(&scope);
    let b = bounds(timing, last, CHECK_COLUMNS);
    Answers {
        notes: idx.notes_in(&scope),
        max_nps: prof.peak_nps_so_far(idx, &scope, timing, last),
        max_poly: prof.peak_poly_so_far(idx, &scope, last),
        columns: prof.chart_columns(idx, &scope, timing, &b),
    }
}

/// The live analysis the Inspector showed for the saved document.
pub struct Live {
    pub index: Arc<AnalysisIndex>,
    pub profile: Arc<PeakProfile>,
    pub timing: Arc<Timing>,
}

/// The on-save check (I7): the live (incrementally updated) analysis of the
/// saved document against a fresh one built from scratch. `Ok(())` when
/// they agree, else a sentence naming the first difference. O(notes) — run
/// on a worker.
pub fn check(doc: &PlumDocument, live: &Live) -> Result<(), String> {
    let idx = AnalysisIndex::for_document(doc);
    let timing = Timing::for_document(doc);
    let prof =
        PeakProfile::build(&idx, &Scope::Project, &timing, WINDOW_NS, None).expect("not canceled");
    let fresh = answers_of(&idx, &timing, &prof);
    let was = answers_of(&live.index, &live.timing, &live.profile);
    let g = plumspirit_engine::midi::group_digits;
    if was.notes != fresh.notes {
        return Err(format!(
            "notes {} (live) / {} (fresh)",
            g(was.notes),
            g(fresh.notes)
        ));
    }
    if was.max_nps != fresh.max_nps {
        return Err(format!(
            "maximum NPS {} (live) / {} (fresh)",
            g(was.max_nps),
            g(fresh.max_nps)
        ));
    }
    if was.max_poly != fresh.max_poly {
        return Err(format!(
            "maximum polyphony {} (live) / {} (fresh)",
            g(was.max_poly),
            g(fresh.max_poly)
        ));
    }
    if let Some(k) = (0..fresh.columns.len()).find(|&k| was.columns.get(k) != fresh.columns.get(k))
    {
        return Err(format!("chart column {} of {CHECK_COLUMNS}", k + 1));
    }
    Ok(())
}

/// The state of the on-save check (I7).
#[derive(Default)]
pub enum SaveCheck {
    #[default]
    None,
    /// Saved; waiting for the live analysis of that version (the notes key
    /// of the saved document) to be ready.
    Waiting(crate::app::analysis::NotesKey),
    Running(Receiver<Result<(), String>>),
    /// Done at this time (seconds since the epoch): agreed, or the sentence.
    Done(u64, Result<(), String>),
}

/// The Inspector's state while it is shown.
#[derive(Default)]
pub struct Inspector {
    /// Tempo changes of the snapshot with this export key (O(events) once
    /// per snapshot).
    tempo_changes: Option<(u64, usize)>,
    pub chart: Chart,
    /// The last values shown (kept, marked "Updating…", while the analysis
    /// catches up with an edit).
    pub last: Option<InspectorValues>,
    /// The lines last drawn and whether work was still running (tests,
    /// diagnostics).
    pub drawn: Option<(Vec<Row>, bool)>,
    /// The check after a save (I7).
    pub save_check: SaveCheck,
    /// Where the charts were last painted (tests click them).
    pub chart_rects: Vec<egui::Rect>,
    /// B1-0108: the charts' visible window and per-chart vertical zoom.
    pub view: ChartView,
    /// B2-F30: manual chart navigation pauses following until Resume or a
    /// transport navigation (Play, Stop, Seek), as in the editor.
    pub follow_suspended: bool,
}

/// B1-0108: one chart's vertical view — `zoom` × (1 = the visible exact
/// maximum fills the height) and, when zoomed in, which part is shown
/// (`pos`: 0 = the bottom, 1 = the top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YView {
    pub zoom: f64,
    pub pos: f64,
}

impl Default for YView {
    fn default() -> Self {
        YView {
            zoom: 1.0,
            pos: 0.0,
        }
    }
}

/// Most vertical zoom.
pub const MAX_Y_ZOOM: f64 = 1_000_000.0;

impl YView {
    /// The value range shown when the full range is `[lo, hi]`.
    pub fn range(&self, lo: f64, hi: f64) -> (f64, f64) {
        let full = (hi - lo).max(f64::MIN_POSITIVE);
        let zoom = self.zoom.clamp(1.0, MAX_Y_ZOOM);
        let span = full / zoom;
        let start = lo + self.pos.clamp(0.0, 1.0) * (full - span);
        (start, start + span)
    }

    /// Zoomed by `factor` keeping the value at `at` (fraction of the height
    /// from the bottom) where it is.
    pub fn zoomed(self, factor: f64, at: f64) -> YView {
        let zoom = (self.zoom * factor).clamp(1.0, MAX_Y_ZOOM);
        if zoom <= 1.0 {
            return YView::default();
        }
        // In units of the full range: the shown start and the pivot.
        let (span0, span1) = (1.0 / self.zoom, 1.0 / zoom);
        let start0 = self.pos * (1.0 - span0);
        let pivot = start0 + at.clamp(0.0, 1.0) * span0;
        let start1 = (pivot - at.clamp(0.0, 1.0) * span1).clamp(0.0, 1.0 - span1);
        YView {
            zoom,
            pos: if span1 >= 1.0 {
                0.0
            } else {
                start1 / (1.0 - span1)
            },
        }
    }
}

/// B1-0108: what the charts show.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChartView {
    pub window: Window,
    /// NPS, Polyphony, Tempo, and the note counter (Selection Analytics).
    pub y: [YView; 4],
    /// The Tempo chart on a logarithmic BPM scale (extreme tempos).
    pub tempo_log: bool,
}

impl Inspector {
    /// Close Project, on the reaper. A chart or save check already running
    /// finishes here; its result cannot reach a subsequently opened project.
    pub fn release(mut self) {
        if let Some(j) = self.chart.job.take() {
            drop(j.rx.into_iter().last());
        }
        if let SaveCheck::Running(rx) = std::mem::replace(&mut self.save_check, SaveCheck::None) {
            drop(rx.into_iter().last());
        }
    }

    /// Tempo changes the file of `prep` holds (cached per snapshot).
    pub fn tempo_changes(&mut self, prep: &Prepared) -> usize {
        let key = prep.snapshot.key();
        match self.tempo_changes {
            Some((k, n)) if k == key => n,
            _ => {
                let n = prep.snapshot.tempo_changes().len();
                self.tempo_changes = Some((key, n));
                n
            }
        }
    }

    /// Nothing is kept.
    pub fn clear(&mut self) {
        self.tempo_changes = None;
        self.chart.clear();
        self.view = ChartView::default();
        self.follow_suspended = false;
        self.last = None;
        self.drawn = None;
        self.save_check = SaveCheck::None;
    }
}

#[cfg(test)]
mod tests;
