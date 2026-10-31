//! v1.0-Launch L-E01 … L-E08 (contract §007): the advanced section of
//! Selection Analytics — charts over the **selected notes only**.
//!
//! The readout's caret (or View → Selection Analytics Charts) shows / hides
//! it; the compact counters stay as they are. It reads the selection's own
//! `SelectionAnalysis` (the same canonical definitions as the readout, the
//! Progressor and the MIDI Inspector — notes as they play, active over
//! `[start, end)`, NPS / PPS over the Progressor's window) through the
//! Inspector's chart component (`analytics_charts`):
//!
//! * **NPS** and **Polyphony** — each column the exact maximum over its own
//!   span at any zoom, from the selection's peak profile; the headline maxima
//!   and the Peak buttons are the analysis's exact peaks, never the grid.
//! * **Notes** — the cumulative count by each column's end: the selected
//!   notes from the selection's start (disjoint groups stay flat between
//!   them), or every note of the song (Settings: `selection_counter_song`).
//! * **Tempo** — the Conductor's tempo over the same window (steps and linear
//!   ramps); every time on every chart goes through this tempo map.
//!
//! Live: the analysis is rebuilt / updated after every edit, Undo / Redo,
//! Clip move or tempo change (it is keyed by the tracks' revisions,
//! corrections and the tempo map); the chart columns follow it through
//! `Chart::sync_in` — only the columns an edit can change are recomputed, on
//! a worker when many, while the last columns stay drawn ("in view …"). A
//! newer analysis is never replaced by an older result (each result carries
//! the analysis it was computed for).
//!
//! The playhead line moves with the transport and stays where it is when
//! paused; hovering shows the exact NPS / PPS / polyphony / notes at that
//! time; a click seeks (if enabled) and never changes the note selection.
//! Follow Playhead pages the charts while the playhead is inside the
//! selection; outside it the view stays on the selection and the header
//! says where the playhead is.

use super::PlumSpiritApp;
use super::analytics_charts::{self, ChartsData, Counter, followed_window};
use crate::inspector::{self, Axis, Chart, ChartView, Window};
use plumspirit_engine::analysis::Scope;
use plumspirit_engine::analysis::selection::SelectionAnalysis;
use plumspirit_engine::midi::group_digits;
use std::sync::Arc;
use std::time::Duration;

/// Repaint interval while something is being computed.
const POLL: Duration = Duration::from_millis(30);

/// The cumulative counts of the drawn columns, for the chart basis they
/// were computed for.
struct CounterCache {
    /// The index read (selection or song; a `Weak`, so its address is never
    /// reused by a newer index while cached), the bounds and the mode.
    index: std::sync::Weak<plumspirit_engine::analysis::AnalysisIndex>,
    bounds: Vec<u64>,
    song: bool,
    values: Vec<u64>,
}

/// The state of the selection charts (app field `sel_charts`).
#[derive(Default)]
pub struct SelectionCharts {
    pub chart: Chart,
    pub view: ChartView,
    /// Manual navigation pauses following until Resume or a transport
    /// navigation (as the editor and the Inspector).
    pub follow_suspended: bool,
    /// Where the charts were painted (tests click them).
    pub rects: Vec<egui::Rect>,
    /// The selection span the view was fitted to.
    span: Option<(u64, u64)>,
    counter: Option<CounterCache>,
}

impl SelectionCharts {
    pub fn clear(&mut self) {
        *self = SelectionCharts::default();
    }

    /// Close Project, on the reaper: a chart worker still running finishes
    /// there.
    pub fn release(self) {
        self.chart.release();
    }
}

/// `[first, last]` of the selection as a window of `[0, last]` on `axis`.
fn selection_window(
    timing: &plumspirit_engine::analysis::Timing,
    a: &SelectionAnalysis,
    axis: Axis,
) -> Window {
    let end = a.last_tick.max(1);
    let lo = inspector::fraction_of(timing, axis, end, a.first_tick).min(1.0 - inspector::MIN_SPAN);
    Window { axis, lo, hi: 1.0 }
}

/// Notes started by the end of each column of `bounds` (exact): the
/// selected notes, or every note of the song.
fn counter_values(index: &plumspirit_engine::analysis::AnalysisIndex, bounds: &[u64]) -> Vec<u64> {
    let scope = Scope::Project;
    let n = bounds.len().saturating_sub(1);
    (0..n)
        .map(|k| {
            let end = if k + 1 == n {
                bounds[k + 1]
            } else {
                bounds[k + 1].saturating_sub(1).max(bounds[k])
            };
            index.notes_passed(&scope, end)
        })
        .collect()
}

impl PlumSpiritApp {
    /// Shows or hides the selection charts (the setting is saved).
    pub(super) fn set_selection_charts(&mut self, on: bool) {
        self.settings.selection_advanced = on;
        if !on {
            self.sel_charts.clear();
        }
        self.save_settings();
    }

    /// Per frame (from `update_live`): the window while it is shown.
    pub(super) fn update_selection_charts_window(&mut self, ctx: &egui::Context) {
        if !self.settings.selection_analytics
            || !self.settings.selection_advanced
            || self.open.is_none()
        {
            return;
        }
        let mut open = true;
        let mut busy = false;
        let screen = ctx.content_rect();
        egui::Window::new("Selection Analytics")
            .id(egui::Id::new("selection-charts"))
            .open(&mut open)
            // Centred: clear of the readout (a corner) and of the MIDI
            // Inspector (the right side).
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .default_width(420.0)
            .default_height(620.0)
            .resizable(true)
            .max_height((screen.height() - 16.0).max(80.0))
            .vscroll(true)
            .show(ctx, |ui| {
                busy = self.selection_charts_ui(ui);
            });
        if busy {
            ctx.request_repaint_after(POLL);
        }
        if !open {
            self.set_selection_charts(false);
        }
    }

    /// The window's content; true while something is computed.
    fn selection_charts_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let pending = self.sel_stats.pending;
        let Some((a, timing)) = self.sel_stats.current() else {
            ui.label(if pending {
                "Analyzing the selection…"
            } else {
                "Select notes (or clips in the Track List) to analyze them."
            });
            self.sel_charts.chart.clear();
            return pending;
        };
        let (a, timing) = (Arc::clone(a), Arc::clone(timing));
        if a.counted == 0 {
            ui.label(if a.selected == 0 {
                "Select notes (or clips in the Track List) to analyze them."
            } else {
                "The selected notes do not play (muted, or removed by a Track Property correction): there is nothing to chart."
            });
            self.sel_charts.chart.clear();
            return pending;
        }
        let end = a.last_tick.max(1);
        let playhead = self.transport.position();
        // ------------------------------------------------ the header
        let clock = |t: u64| super::format_clock(timing.nanos_at(t) as f64 / 1e9);
        ui.label(
            egui::RichText::new(format!(
                "{} notes · {} – {} ({})",
                group_digits(a.counted),
                clock(a.first_tick),
                clock(a.last_tick),
                super::format_clock(a.span_ns as f64 / 1e9),
            ))
            .strong(),
        )
        .on_hover_text("The selected notes as they play (muted notes and notes a Track Property correction removes are not counted), from the first start to the last end.");
        let m = a.at(&timing, playhead, a.window_ns);
        let where_ = if playhead < a.first_tick {
            " (before the selection)"
        } else if playhead > a.last_tick {
            " (after the selection)"
        } else {
            ""
        };
        ui.label(format!(
            "At the playhead {}{where_}: NPS {}, PPS {:.1}, poly {}, {} of {} notes passed",
            clock(playhead),
            group_digits(m.nps),
            m.pps,
            group_digits(m.poly),
            group_digits(m.notes_passed),
            group_digits(a.counted),
        ))
        .on_hover_text("Exact at the playhead's position, playing or not (a seek updates it; pausing keeps it).");
        if pending {
            ui.label(egui::RichText::new("Updating analysis…").weak());
        }
        // ------------------------------------------- the counter mode
        ui.horizontal(|ui| {
            ui.label("Note counter");
            let mut song = self.settings.selection_counter_song;
            ui.selectable_value(&mut song, false, "Selection")
                .on_hover_text("The selected notes, counted from the selection's start; between separate groups of selected notes the count stays flat.");
            ui.selectable_value(&mut song, true, "Song")
                .on_hover_text("Every note of the song (as it plays) started by that time.");
            if song != self.settings.selection_counter_song {
                self.settings.selection_counter_song = song;
                self.sel_charts.counter = None;
                self.save_settings();
            }
            ui.separator();
            let mut seek = self.settings.selection_chart_seek;
            if ui
                .checkbox(&mut seek, "Click to seek")
                .on_hover_text("A click or drag on a chart moves the playhead there. The note selection is never changed.")
                .changed()
            {
                self.settings.selection_chart_seek = seek;
                self.save_settings();
            }
        });
        // --------------------------------------------------- the view
        let fit = selection_window(&timing, &a, self.sel_charts.view.window.axis);
        if self.sel_charts.span != Some((a.first_tick, a.last_tick)) {
            // A new selection (or an edit that moved its ends): fit it.
            self.sel_charts.span = Some((a.first_tick, a.last_tick));
            self.sel_charts.view.window = fit;
        }
        let inside = (a.first_tick..=a.last_tick).contains(&playhead);
        if self.settings.selection_follow_playhead && !self.sel_charts.follow_suspended && inside {
            let w = followed_window(
                self.sel_charts.view.window,
                &timing,
                end,
                playhead,
                self.settings.follow_position,
            );
            // Never page before the selection's start.
            self.sel_charts.view.window = if w.lo < fit.lo {
                w.around(fit.lo + w.span() / 2.0, w.span())
            } else {
                w
            };
        }
        let width = ui.available_width().max(80.0);
        let columns = ((width * ui.ctx().pixels_per_point()).round() as usize)
            .clamp(1, inspector::MAX_COLUMNS);
        let window = self.sel_charts.view.window;
        let mut busy =
            self.sel_charts
                .chart
                .sync_in(a.index(), a.profile(), &timing, end, columns, window);
        // The counter for the columns drawn (cached per basis).
        let song_index = self.analysis.index().cloned();
        let song = self.settings.selection_counter_song;
        let bounds = self.sel_charts.chart.bounds().to_vec();
        let index: Option<&Arc<plumspirit_engine::analysis::AnalysisIndex>> = if song {
            song_index.as_ref()
        } else {
            Some(a.index())
        };
        let counter = match index {
            Some(ix) => {
                let fresh = self.sel_charts.counter.as_ref().is_some_and(|c| {
                    std::ptr::eq(c.index.as_ptr(), Arc::as_ptr(ix))
                        && c.song == song
                        && c.bounds == bounds
                });
                if !fresh {
                    self.sel_charts.counter = Some(CounterCache {
                        index: Arc::downgrade(ix),
                        bounds: bounds.clone(),
                        song,
                        values: counter_values(ix, &bounds),
                    });
                }
                self.sel_charts.counter.as_ref().map(|c| Counter {
                    title: if song {
                        "Notes (song total)".into()
                    } else {
                        "Notes (selection, cumulative)".into()
                    },
                    hint: if song {
                        "Every note of the song started by the end of each bar's span (exact)."
                    } else {
                        "Selected notes started by the end of each bar's span, from the selection's start (exact)."
                    },
                    values: c.values.clone(),
                })
            }
            None => {
                busy = true;
                None
            }
        };
        let meter = self.meter_map().cloned();
        let ppqn = self.doc().and_then(|d| d.division().musical_ppqn());
        let points: Vec<u64> = self
            .doc()
            .map(|d| {
                d.conductor()
                    .tempo_points()
                    .iter()
                    .map(|p| p.tick)
                    .collect()
            })
            .unwrap_or_default();
        let at_line = |tick: u64| {
            let m = a.at(&timing, tick, a.window_ns);
            format!(
                "Exact at this time: NPS {}, PPS {:.1}, poly {}, {} notes passed",
                group_digits(m.nps),
                m.pps,
                group_digits(m.poly),
                group_digits(m.notes_passed)
            )
        };
        let peaks = (
            (a.peak_nps > 0).then_some(a.peak_nps_tick),
            (a.peak_poly > 0).then_some(a.peak_poly_tick),
        );
        let SelectionCharts {
            chart,
            view,
            follow_suspended,
            rects,
            ..
        } = &mut self.sel_charts;
        let chart = &*chart;
        let data = ChartsData {
            id: "sel",
            timing: &timing,
            end,
            chart,
            index: a.index(),
            maxima: (Some(a.peak_nps), Some(a.peak_poly)),
            max_hint: "The selection's exact maximum (from its analysis, never read from the bars). Polyphony counts notes active over [start, end): a note ending where another starts is not counted with it.",
            peak_ticks: peaks,
            playhead,
            meter,
            tempo_points: &points,
            ppqn,
            fit,
            fit_hint: "The selection (double-click a chart: also its height)",
            seek: self.settings.selection_chart_seek,
            follow: Some(self.settings.selection_follow_playhead),
            follow_hint: "Page the charts with the playhead while it is inside the selection; outside it they stay on the selection. Manual navigation pauses following; Resume, Play, Stop or Seek resumes it.",
            counter,
            exact_at: Some(&at_line),
        };
        let out = analytics_charts::charts_ui(ui, view, follow_suspended, rects, &data);
        if let Some(on) = out.follow {
            self.settings.selection_follow_playhead = on;
            self.sel_charts.follow_suspended = false;
            self.save_settings();
        }
        if out.resume {
            self.sel_charts.follow_suspended = false;
        }
        if let Some(tick) = out.seek {
            self.seek_to(tick);
        }
        busy || out.busy || pending
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use super::super::track_list::MainView;
    use super::*;
    use plumspirit_engine::analysis::profile::{PeakProfile, Peaks};
    use plumspirit_engine::analysis::{AnalysisIndex, Timing};
    use plumspirit_engine::history::Operation;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};

    const SIZE: egui::Vec2 = egui::vec2(1600.0, 1000.0);
    const SEL_FROM: u64 = 20_000;

    fn note(start: u64, len: u64, pitch: u8) -> NoteRecord {
        NoteRecord::new(
            start,
            len,
            NoteAttrs::new(pitch, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
        )
    }

    /// The selectable notes: `n` overlapping notes from [`SEL_FROM`].
    fn run(n: u64, len: u64) -> Vec<NoteRecord> {
        (0..n)
            .map(|i| note(SEL_FROM + i * 120, len, 40 + (i % 40) as u8))
            .collect()
    }

    /// Track 0: a 100-note chord at tick 0 (never selected) and `n`
    /// overlapping notes from [`SEL_FROM`]; track 1: other notes.
    fn doc(n: u64) -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
        let mut v: Vec<NoteRecord> = (0..100).map(|p| note(0, 960, p as u8)).collect();
        v.extend(run(n, 300));
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
        d.add_track().unwrap();
        let w: Vec<NoteRecord> = (0..50).map(|i| note(21_000 + i * 240, 100, 70)).collect();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&w, false)
            .unwrap();
        d
    }

    /// The Piano Roll on track 0 with the run selected, charts shown.
    fn harness(n: u64) -> Harness {
        let mut h = Harness::new(doc(n));
        // Every project opens in the Track List (its first frame).
        h.app_frame(SIZE, 1.0);
        h.set_track(0);
        h.app.main_view = MainView::PianoRoll;
        h.app
            .doc_mut()
            .unwrap()
            .track_mut(0)
            .unwrap()
            .notes
            .select_range(SEL_FROM, u64::MAX, false);
        h.app.settings.selection_advanced = true;
        h
    }

    /// Frames until the analysis and the charts of the current window are
    /// ready.
    fn settle(h: &mut Harness) {
        for _ in 0..5_000 {
            h.app_frame(SIZE, 1.0);
            let c = &h.app.sel_charts;
            let fresh = h
                .app
                .sel_stats
                .current()
                .is_some_and(|(a, _)| c.chart.describes(a.index()));
            if !h.app.sel_stats.pending
                && fresh
                && !c.chart.columns.is_empty()
                && c.chart.window() == Some(c.view.window)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let c = &h.app.sel_charts;
        panic!(
            "the selection charts did not settle: pending {} current {} describes {:?} columns {} window {:?} view {:?}",
            h.app.sel_stats.pending,
            h.app.sel_stats.current().is_some(),
            h.app
                .sel_stats
                .current()
                .map(|(a, _)| c.chart.describes(a.index())),
            c.chart.columns.len(),
            c.chart.window(),
            c.view.window
        );
    }

    /// The chart columns of a document holding only `notes` (the same
    /// tempo), over `bounds`: an independent path through the song-scope
    /// analysis.
    fn reference(
        notes: &[NoteRecord],
        bpm: Bpm,
        bounds: &[u64],
        window_ns: u64,
    ) -> Vec<(u64, u64)> {
        let mut d = PlumDocument::new_project("ref".into(), bpm, 960);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(notes, false)
            .unwrap();
        let idx = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        let p = PeakProfile::build_with(
            &idx,
            &Scope::Project,
            &timing,
            window_ns,
            Peaks::WithPps,
            None,
        )
        .unwrap();
        p.chart_columns(&idx, &Scope::Project, &timing, bounds)
    }

    fn columns_and_bounds(h: &Harness) -> (Vec<(u64, u64)>, Vec<u64>) {
        let c = &h.app.sel_charts.chart;
        (c.columns.clone(), c.bounds().to_vec())
    }

    fn analysis(h: &Harness) -> Arc<SelectionAnalysis> {
        Arc::clone(h.app.sel_stats.current().unwrap().0)
    }

    /// A primary click at `pos` (hover first, as a pointer would).
    fn click(h: &mut Harness, pos: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.app_frame_events(SIZE, 1.0, vec![egui::Event::PointerMoved(pos)]);
        h.app_frame_events(SIZE, 1.0, vec![button(true), button(false)]);
    }

    #[test]
    fn the_caret_and_the_view_command_show_and_hide_the_selection_charts() {
        let mut h = harness(200);
        h.app.settings.selection_advanced = false;
        h.app_frame(SIZE, 1.0);
        h.app_frame(SIZE, 1.0);
        let v = h.app.overlay_views[1].expect("the readout is drawn");
        let caret = v.advanced.expect("with its caret");
        assert!(v.rect.contains(caret.center()) && !caret.intersects(v.chip));
        click(&mut h, caret.center());
        assert!(
            h.app.settings.selection_advanced,
            "the caret shows the charts"
        );
        assert!(
            h.app.settings.progressor.selection_collapsed,
            "the compact counters stay as they are"
        );
        assert_eq!(
            h.doc().track(0).unwrap().notes.selected_count(),
            200,
            "the click on the caret is not a click on the notes"
        );
        settle(&mut h);
        assert_eq!(
            h.app.sel_charts.rects.len(),
            4,
            "NPS, polyphony, notes, tempo"
        );
        assert!(h.command(Command::ToggleSelectionCharts));
        assert!(!h.app.settings.selection_advanced);
        assert!(
            h.app.sel_charts.chart.columns.is_empty(),
            "nothing kept when hidden"
        );
        h.app_frame(SIZE, 1.0);
        assert!(
            h.app.overlay_views[1].is_some(),
            "the compact readout stays"
        );
        assert!(h.command(Command::ToggleSelectionCharts));
        assert!(h.app.settings.selection_advanced);
        // The chip over the notes expands the (compact) readout and, like
        // the caret, leaves the selection alone.
        h.app_frame(SIZE, 1.0);
        let chip = h.app.overlay_views[1].unwrap().chip;
        assert!(
            h.app.sel_charts.rects.iter().all(|r| !r.intersects(chip)),
            "the window opens clear of the readout"
        );
        click(&mut h, chip.center());
        assert!(!h.app.settings.progressor.selection_collapsed);
        assert_eq!(h.doc().track(0).unwrap().notes.selected_count(), 200);
    }

    #[test]
    fn the_charts_are_exact_for_the_selected_notes_only() {
        let mut h = harness(200);
        settle(&mut h);
        let a = analysis(&h);
        assert_eq!(a.counted, 200);
        let (cols, bounds) = columns_and_bounds(&h);
        // Fitted to the selection: the first column starts at its first note.
        assert_eq!(bounds.first().copied(), Some(SEL_FROM));
        assert_eq!(bounds.last().copied(), Some(a.last_tick));
        assert_eq!(
            cols,
            reference(&run(200, 300), Bpm::DEFAULT, &bounds, a.window_ns)
        );
        // The headline maxima are the analysis's exact peaks, and the
        // unselected 100-note chord and track 1 are not in them.
        let max = |k: usize| {
            cols.iter()
                .map(|c| if k == 0 { c.0 } else { c.1 })
                .max()
                .unwrap()
        };
        assert_eq!((max(0), max(1)), (a.peak_nps, a.peak_poly));
        assert_eq!(a.peak_poly, 3);
        // The note counter: 200 by the end; the song mode counts everything
        // started by then (the chord, the run, track 1).
        let c = h.app.sel_charts.counter.as_ref().unwrap();
        assert_eq!(c.values.last().copied(), Some(200));
        assert!(c.values.windows(2).all(|w| w[0] <= w[1]));
        h.app.settings.selection_counter_song = true;
        h.app.sel_charts.counter = None;
        settle(&mut h);
        let c = h.app.sel_charts.counter.as_ref().unwrap();
        assert_eq!(c.values.last().copied(), Some(100 + 200 + 50));
        assert_eq!(
            c.values.first().copied(),
            Some(100 + 1),
            "the chord and the first note"
        );
    }

    #[test]
    fn the_charts_follow_edits_undo_redo_and_tempo_changes() {
        let mut h = harness(200);
        settle(&mut h);
        let (first, _) = columns_and_bounds(&h);
        // Lengthen the selected notes: more overlap.
        h.app.apply_op(Operation::Resize {
            track: 0.into(),
            delta: 300,
        });
        settle(&mut h);
        let (cols, bounds) = columns_and_bounds(&h);
        let a = analysis(&h);
        assert_eq!(a.peak_poly, 5);
        assert_eq!(
            cols,
            reference(&run(200, 600), Bpm::DEFAULT, &bounds, a.window_ns)
        );
        // Undo / Redo: exactly before / after.
        assert!(h.command(Command::Undo));
        settle(&mut h);
        assert_eq!(columns_and_bounds(&h).0, first);
        assert!(h.command(Command::Redo));
        settle(&mut h);
        assert_eq!(columns_and_bounds(&h).0, cols);
        // A tempo change moves every time: the columns are recomputed on the
        // new tempo map (and still exact).
        let fast = Bpm::parse("240").unwrap();
        assert!(h.app.set_start_tempo(fast));
        settle(&mut h);
        let (cols2, bounds2) = columns_and_bounds(&h);
        let a = analysis(&h);
        assert_eq!(
            cols2,
            reference(&run(200, 600), fast, &bounds2, a.window_ns)
        );
        assert!(a.peak_nps > 0);
        // Deleting the selection leaves nothing to chart (and no panic).
        assert!(h.command(Command::Delete));
        for _ in 0..5 {
            h.app_frame(SIZE, 1.0);
        }
        assert!(h.app.sel_charts.chart.columns.is_empty());
    }

    /// What a selection analysis says (every exact value it shows).
    fn fingerprint(a: &SelectionAnalysis) -> Vec<u128> {
        vec![
            a.selected.into(),
            a.counted.into(),
            a.first_tick.into(),
            a.last_tick.into(),
            a.span_ns.into(),
            a.peak_poly.into(),
            a.peak_poly_tick.into(),
            a.peak_nps.into(),
            a.peak_nps_tick.into(),
            a.peak_pps.to_bits().into(),
            a.peak_pps_tick.into(),
            a.area_ticks,
            a.area_ns,
        ]
    }

    /// A fresh analysis of track 0's selected notes in the document now.
    fn fresh(d: &PlumDocument, window_ns: u64) -> Vec<u128> {
        use plumspirit_engine::analysis::document_corrections;
        use plumspirit_engine::analysis::selection::SelectionInput;
        let inputs = vec![SelectionInput {
            id: d.track(0).unwrap().id(),
            notes: d.track(0).unwrap().notes.clone(),
            correction: document_corrections(d)[0],
            shift: 0,
        }];
        let a = SelectionAnalysis::build_for(&inputs, &Timing::for_document(d), window_ns, None)
            .unwrap();
        fingerprint(&a)
    }

    /// L-F01 (contract §009): the readout over the notes is compact by
    /// default (its first line); Alt held over it shows every line, unfaded,
    /// while held (Detailed Hover) — from the analysis already made.
    #[test]
    fn the_readout_is_compact_by_default_and_detailed_while_alt_is_held_over_it() {
        let mut h = harness(200);
        h.app.settings.selection_advanced = false;
        assert!(h.app.settings.progressor.selection_collapsed, "compact");
        settle_readout(&mut h);
        let compact = h.app.overlay_views[1].expect("the readout").rect;
        let a = analysis(&h);
        // Hovering alone: still compact (and faded so the notes show).
        let over = compact.center();
        h.app_frame_events(SIZE, 1.0, vec![egui::Event::PointerMoved(over)]);
        h.app_frame(SIZE, 1.0);
        assert_eq!(
            h.app.overlay_views[1].unwrap().rect.height(),
            compact.height()
        );
        // Alt held over it: every line.
        h.modifiers = egui::Modifiers::ALT;
        h.app_frame(SIZE, 1.0);
        let detailed = h.app.overlay_views[1].unwrap().rect;
        assert!(
            detailed.height() > compact.height() * 3.0,
            "{detailed:?} vs {compact:?}"
        );
        h.app_frame(SIZE, 1.0);
        assert_eq!(
            h.app.overlay_views[1].unwrap().rect,
            detailed,
            "stable while held"
        );
        assert!(Arc::ptr_eq(&analysis(&h), &a), "hovering analyzes nothing");
        // Released: compact again.
        h.modifiers = egui::Modifiers::NONE;
        h.app_frame(SIZE, 1.0);
        assert_eq!(
            h.app.overlay_views[1].unwrap().rect.height(),
            compact.height()
        );
        // Alt away from the readout: nothing changes.
        h.modifiers = egui::Modifiers::ALT;
        let away = egui::pos2(compact.left() - 200.0, compact.top() - 200.0);
        h.app_frame_events(SIZE, 1.0, vec![egui::Event::PointerMoved(away)]);
        h.app_frame(SIZE, 1.0);
        assert_eq!(
            h.app.overlay_views[1].unwrap().rect.height(),
            compact.height()
        );
        h.modifiers = egui::Modifiers::NONE;
        // The setting: every line all the time.
        h.app.settings.progressor.selection_collapsed = false;
        h.app_frame(SIZE, 1.0);
        assert_eq!(
            h.app.overlay_views[1].unwrap().rect.height(),
            detailed.height()
        );
    }

    /// Frames until the readout's analysis is ready (charts hidden).
    fn settle_readout(h: &mut Harness) {
        for _ in 0..2_000 {
            h.app_frame(SIZE, 1.0);
            if !h.app.sel_stats.pending && h.app.sel_stats.current().is_some() {
                h.app_frame(SIZE, 1.0);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the selection was not analyzed");
    }

    /// L-E02: after every kind of edit, its undo and its redo, the shown
    /// analysis (and the charts drawn from it) equal a fresh analysis of the
    /// selection in the edited document.
    #[test]
    fn every_edit_kind_its_undo_and_redo_reanalyze_the_selection_exactly() {
        use crate::piano_roll::RollAction;
        let mut h = harness(200);
        settle(&mut h);
        let roll = |h: &mut Harness, a: RollAction| {
            let ctx = h.ctx.clone();
            h.app.apply_roll_actions(vec![a], &ctx);
        };
        let check = |h: &mut Harness, what: &str| {
            settle(h);
            let a = analysis(h);
            assert_eq!(fingerprint(&a), fresh(h.doc(), a.window_ns), "{what}");
            assert!(
                h.app.sel_charts.chart.describes(a.index()),
                "{what}: the charts"
            );
            fingerprint(&a)
        };
        type Edit = Box<dyn Fn(&mut Harness)>;
        let edits: Vec<(&str, Edit)> = vec![
            (
                "move",
                Box::new(move |h| roll(h, RollAction::MoveSelected { dt: 240, dp: 2 })),
            ),
            (
                "resize",
                Box::new(move |h| roll(h, RollAction::ResizeSelected { delta: 170 })),
            ),
            (
                "velocity",
                Box::new(move |h| roll(h, RollAction::SetVelocity(30))),
            ),
            (
                "transform (Flip X)",
                Box::new(|h| assert!(h.command(Command::FlipX))),
            ),
            (
                "duplicate",
                Box::new(|h| assert!(h.command(Command::DuplicateOctaveUp))),
            ),
            (
                "copy and paste",
                Box::new(|h| {
                    assert!(h.command(Command::Copy));
                    h.app.seek_to(400_000);
                    assert!(h.command(Command::Paste));
                }),
            ),
            (
                "tempo",
                Box::new(|h| assert!(h.app.set_start_tempo(Bpm::parse("187.5").unwrap()))),
            ),
        ];
        for (what, edit) in &edits {
            let before = check(&mut h, what);
            let rev = h.doc().track(0).unwrap().notes.revision();
            edit(&mut h);
            let after = check(&mut h, what);
            // A velocity change, and Flip X of this evenly spaced run, keep
            // every value; the others change some.
            if !matches!(*what, "velocity" | "transform (Flip X)") {
                assert_ne!(after, before, "{what} changes the analysis");
            }
            assert!(
                *what == "tempo" || h.doc().track(0).unwrap().notes.revision() != rev,
                "{what} edited the notes"
            );
            assert!(h.command(Command::Undo), "{what}: undo");
            if *what == "copy and paste" {
                // Undo removes the pasted (selected) notes; the earlier
                // selection is not restored (as after undoing a drawn note).
                for _ in 0..5 {
                    h.app_frame(SIZE, 1.0);
                }
                assert_eq!(h.doc().track(0).unwrap().notes.selected_count(), 0);
                assert!(h.app.sel_stats.current().is_none(), "{what}: undo");
                assert!(h.app.sel_charts.chart.columns.is_empty(), "{what}: undo");
            } else {
                assert_eq!(check(&mut h, what), before, "{what}: undo");
            }
            assert!(h.command(Command::Redo), "{what}: redo");
            assert_eq!(check(&mut h, what), after, "{what}: redo");
        }
        // Delete: nothing is selected, nothing charted; undo brings both
        // back.
        let before = check(&mut h, "delete");
        assert!(h.command(Command::Delete));
        for _ in 0..5 {
            h.app_frame(SIZE, 1.0);
        }
        assert!(h.app.sel_stats.current().is_none());
        assert!(h.app.sel_charts.chart.columns.is_empty());
        assert!(h.command(Command::Undo));
        assert_eq!(check(&mut h, "delete: undo"), before);
    }

    /// L-E02: moving the selected clips in the Track List moves their
    /// analysis with them (same counts and peaks, later ticks).
    #[test]
    fn moving_selected_clips_moves_their_analysis() {
        let mut h = harness(200);
        h.app.main_view = MainView::TrackList;
        h.app_frame(SIZE, 1.0);
        assert!(h.command(Command::SelectAll));
        settle(&mut h);
        let a = analysis(&h);
        assert_eq!(a.counted, 100 + 200 + 50, "every clip's notes");
        assert!(h.command(Command::NudgeClipsRight));
        settle(&mut h);
        let b = analysis(&h);
        let step = b.first_tick - a.first_tick;
        assert!(step > 0, "the clips moved");
        assert_eq!(b.last_tick, a.last_tick + step);
        assert_eq!(
            (b.counted, b.peak_poly, b.peak_nps, b.peak_poly_tick),
            (a.counted, a.peak_poly, a.peak_nps, a.peak_poly_tick + step)
        );
        assert!(h.app.sel_charts.chart.describes(b.index()));
        assert!(h.command(Command::Undo));
        settle(&mut h);
        assert_eq!(fingerprint(&analysis(&h)), fingerprint(&a));
    }

    /// L-E02 / L-E07 bench (release): every note of a generated dense song
    /// selected (1 M and 10 M), the Piano Roll drawn, the charts window on
    /// and off in the same process while the playhead sweeps the selection
    /// (fitted and zoomed 16x: page turns). The analysis is never rebuilt by
    /// the transport.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit selection_charts_frame_cost -- --ignored --nocapture --test-threads=1"]
    fn selection_charts_frame_cost() {
        use plumspirit_engine::fixtures::{FixtureShape, generate};
        use std::time::{Duration, Instant};
        let sizes: Vec<usize> = std::env::var("PLUMSPIRIT_BENCH_NOTES").map_or_else(
            |_| vec![1_000_000, 10_000_000],
            |v| v.split(',').filter_map(|x| x.parse().ok()).collect(),
        );
        for n in sizes {
            let mut h = Harness::new(generate(FixtureShape::Dense, n, 960, 5));
            h.app_frame(SIZE, 1.0);
            // The fixture's notes are on track 1 (track 0: the Conductor's).
            h.set_track(1);
            h.app.main_view = MainView::PianoRoll;
            // The roll's first frames (its view, tiles) before selecting.
            for _ in 0..30 {
                h.app_frame(SIZE, 1.0);
                std::thread::sleep(Duration::from_millis(5));
            }
            h.app
                .doc_mut()
                .unwrap()
                .track_mut(1)
                .unwrap()
                .notes
                .select_range(0, u64::MAX, false);
            // Ablation: PLUMSPIRIT_BENCH_CHARTS=0 measures the readout alone.
            let charts = std::env::var("PLUMSPIRIT_BENCH_CHARTS").map_or(true, |v| v != "0");
            h.app.settings.selection_advanced = charts;
            if std::env::var("PLUMSPIRIT_BENCH_READOUT").is_ok_and(|v| v == "0") {
                h.app.settings.selection_analytics = false;
                let f = Instant::now();
                h.app_frame(SIZE, 1.0);
                println!(
                    "selection readout off, {n} selected notes: first frame {:.1} ms",
                    f.elapsed().as_secs_f64() * 1e3
                );
                continue;
            }
            let t = Instant::now();
            let mut slowest = (Duration::ZERO, 0usize);
            for k in 0.. {
                let f = Instant::now();
                h.app_frame(SIZE, 1.0);
                if f.elapsed() > slowest.0 {
                    slowest = (f.elapsed(), k);
                }
                let c = &h.app.sel_charts;
                if !h.app.sel_stats.pending
                    && h.app.sel_stats.current().is_some_and(|(a, _)| {
                        !charts
                            || (c.chart.describes(a.index())
                                && c.chart.window() == Some(c.view.window))
                    })
                {
                    break;
                }
                assert!(t.elapsed() < Duration::from_secs(600), "never ready");
                std::thread::sleep(Duration::from_millis(1));
            }
            let ready = t.elapsed();
            if !charts {
                println!(
                    "selection readout only, {n} selected notes: ready {:.0} ms, slowest frame {:.1} ms (frame {})",
                    ready.as_secs_f64() * 1e3,
                    slowest.0.as_secs_f64() * 1e3,
                    slowest.1
                );
                continue;
            }
            let (slowest, slowest_at) = slowest;
            let a = analysis(&h);
            let d = h.doc().clone();
            let generation = h.app.doc_generation();
            let sweep = |h: &mut Harness, zoom: f64| -> (f64, f64, f64) {
                let fit = h.app.sel_charts.view.window;
                if zoom > 1.0 {
                    h.app.sel_charts.view.window = Window {
                        hi: fit.lo + fit.span() / zoom,
                        ..fit
                    };
                }
                let mut v: Vec<f64> = (0..180u64)
                    .map(|k| {
                        let tick = a.first_tick + (a.last_tick - a.first_tick) * k / 180;
                        h.app.transport.seek(tick, &d, generation);
                        let f = Instant::now();
                        let shapes = h.app_frame(SIZE, 1.0);
                        std::hint::black_box(h.ctx.tessellate(shapes, 1.0));
                        f.elapsed().as_secs_f64() * 1e3
                    })
                    .collect();
                h.app.sel_charts.view.window = fit;
                v.sort_by(f64::total_cmp);
                (v[90], v[178], v[179])
            };
            h.app.settings.selection_advanced = false;
            h.app.sel_charts.clear();
            let off = sweep(&mut h, 1.0);
            h.app.settings.selection_advanced = true;
            let mut ready_again = 0;
            while !h
                .app
                .sel_stats
                .current()
                .is_some_and(|(x, _)| h.app.sel_charts.chart.describes(x.index()))
            {
                h.app_frame(SIZE, 1.0);
                ready_again += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            let fitted = sweep(&mut h, 1.0);
            let zoomed = sweep(&mut h, 16.0);
            assert!(
                Arc::ptr_eq(h.app.sel_stats.current().unwrap().0, &a),
                "the transport never re-analyzes"
            );
            println!(
                "selection charts, {} selected notes: analysis + charts ready {:.0} ms (slowest frame {:.1} ms, frame {}; charts again after hiding: {ready_again} frames); frame p50 / p99 / max: charts off {:.2} / {:.2} / {:.2} ms, on (fitted) {:.2} / {:.2} / {:.2} ms, on (16x, page turns) {:.2} / {:.2} / {:.2} ms",
                a.counted,
                ready.as_secs_f64() * 1e3,
                slowest.as_secs_f64() * 1e3,
                slowest_at,
                off.0,
                off.1,
                off.2,
                fitted.0,
                fitted.1,
                fitted.2,
                zoomed.0,
                zoomed.1,
                zoomed.2,
            );
        }
    }

    /// L-E07: a click on a chart seeks there (if enabled) and never touches
    /// the note selection; with Click to seek off it does nothing.
    #[test]
    fn a_click_on_a_chart_seeks_without_changing_the_selection() {
        let mut h = harness(200);
        settle(&mut h);
        let a = analysis(&h);
        h.app.settings.selection_follow_playhead = false;
        h.app.seek_to(0);
        settle(&mut h);
        let nps = h.app.sel_charts.rects[0];
        let at = egui::pos2(nps.center().x, nps.center().y);
        // The time under the pointer: the window's middle.
        let w = h.app.sel_charts.view.window;
        let timing = Timing::for_document(h.doc());
        let want = inspector::tick_of(&timing, w.axis, a.last_tick.max(1), (w.lo + w.hi) / 2.0);
        click(&mut h, at);
        let got = h.app.transport.position();
        let tol = (a.last_tick - a.first_tick) / 50;
        assert!(got.abs_diff(want) <= tol, "seek to {got}, want ~{want}");
        assert!((a.first_tick..=a.last_tick).contains(&got));
        assert_eq!(h.doc().track(0).unwrap().notes.selected_count(), 200);
        assert!(Arc::ptr_eq(&analysis(&h), &a), "a seek re-analyzes nothing");
        // Click to seek off: a click elsewhere (another chart, so it is not
        // a double-click) leaves the playhead.
        h.app.settings.selection_chart_seek = false;
        let poly = h.app.sel_charts.rects[1];
        click(
            &mut h,
            egui::pos2(poly.left() + poly.width() / 5.0, poly.center().y),
        );
        assert_eq!(h.app.transport.position(), got, "Click to seek off");
    }

    #[test]
    fn follow_pages_inside_the_selection_and_stays_on_it_outside() {
        let mut h = harness(400);
        settle(&mut h);
        let a = analysis(&h);
        // Zoom to the selection's first eighth.
        let fit = h.app.sel_charts.view.window;
        h.app.sel_charts.view.window = Window {
            hi: fit.lo + fit.span() / 8.0,
            ..fit
        };
        h.app.sel_charts.follow_suspended = false;
        // A seek near the selection's end pages the charts there.
        let late = a.last_tick - 600;
        h.app.seek_to(late);
        settle(&mut h);
        let w = h.app.sel_charts.view.window;
        let end = a.last_tick.max(1);
        let timing = Timing::for_document(h.doc());
        let f = inspector::fraction_of(&timing, w.axis, end, late);
        assert!(w.lo <= f && f <= w.hi, "{w:?} shows {f}");
        // Before the selection: the view stays where it is.
        h.app.seek_to(100);
        h.app_frame(SIZE, 1.0);
        assert_eq!(h.app.sel_charts.view.window, w);
        // The playhead value at a paused position is exact without playing.
        let m = a.at(&timing, late, a.window_ns);
        assert!(m.notes_passed > 0 && m.poly > 0);
    }

    #[test]
    fn an_empty_selection_shows_nothing_and_a_large_one_is_analyzed_in_the_background() {
        let mut h = harness(200);
        h.app
            .doc_mut()
            .unwrap()
            .track_mut(0)
            .unwrap()
            .notes
            .clear_selection();
        for _ in 0..5 {
            h.app_frame(SIZE, 1.0);
        }
        assert!(h.app.sel_charts.chart.columns.is_empty());
        // 120,000 selected notes: analyzed on a worker; frames go on.
        let mut h = harness(120_000);
        let t = std::time::Instant::now();
        h.app_frame(SIZE, 1.0);
        assert!(h.app.sel_stats.pending, "a worker analyzes it");
        assert!(t.elapsed().as_secs_f64() < 5.0);
        settle(&mut h);
        let a = analysis(&h);
        assert_eq!(a.counted, 120_000);
        let (cols, _) = columns_and_bounds(&h);
        assert_eq!(cols.iter().map(|c| c.1).max(), Some(a.peak_poly));
        assert_eq!(cols.iter().map(|c| c.0).max(), Some(a.peak_nps));
    }
}
