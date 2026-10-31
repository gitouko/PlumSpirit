//! The MIDI Inspector window (v0.9 V09-S18, `docs/INSPECTOR.md` I8): View →
//! MIDI Inspector (F11) beside the editor — the values, one per line with
//! their Exact / Updating… tag, then NPS and polyphony charts of the whole
//! song. Everything comes from the shared analysis and the export snapshot
//! the MIDI Player keeps (`crate::inspector`); nothing is counted here.
//!
//! B1-0108 / B1-0109: the charts zoom and pan — one visible window for all
//! three (Ctrl+wheel or X −/+ zoom at the pointer, wheel / Shift+wheel or a
//! right / middle drag pans, the scrollbar under them, Fit, NPS / Poly Peak
//! jump), each chart its own vertical zoom (Alt+wheel or Y −/+, a scrollbar
//! when zoomed, Fit Y); a double-click fits both. Every NPS / Polyphony
//! column is the exact maximum over its own span at any zoom (computed for
//! the visible window, `Chart::sync_in`), so two peaks are told apart by
//! zooming, never lost to sampling. The Tempo chart draws the Conductor's
//! tempo (steps and ramps, its points marked) on the time or the musical
//! axis, linear or logarithmic.

use super::PlumSpiritApp;
use super::analysis::NotesKey;
use super::analytics_charts::{self, followed_window};
use crate::inspector::{self, InspectorValues, Row, SaveCheck, WINDOW_NS, Window};
use plumspirit_engine::analysis::{Scope, Timing};
use std::time::Duration;

/// Repaint interval while something is being computed.
const POLL: Duration = Duration::from_millis(30);

impl PlumSpiritApp {
    pub(super) fn set_inspector_follow(&mut self, on: bool) {
        self.settings.inspector_follow_playhead = on;
        self.inspector.follow_suspended = false;
        self.save_settings();
    }

    pub(super) fn inspector_shown(&self) -> bool {
        self.settings.inspector && self.open.is_some()
    }

    /// Shows or hides the Inspector (the setting is saved). Hiding it keeps
    /// nothing (the snapshot stays while the Player is shown).
    pub(super) fn set_inspector(&mut self, on: bool) {
        if self.settings.inspector == on {
            return;
        }
        self.settings.inspector = on;
        if !on {
            self.inspector.clear();
            if !self.player_shown() {
                self.player.clear();
            }
        }
        self.save_settings();
    }

    /// Brings the snapshot, the values and the chart (`columns` wide) up to
    /// date. Returns the values to show (`None` before anything was
    /// analysed), whether they describe an earlier version (shown as
    /// "Updating…") and whether work is still running (repaint).
    pub(super) fn inspector_values(
        &mut self,
        columns: usize,
    ) -> (Option<InspectorValues>, bool, bool) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return (None, false, false);
        };
        let mut busy = self.player.sync(&doc);
        let idx = self.analysis.index().cloned();
        let timing = self.analysis.timing().cloned();
        let profile = self.analysis.profile(&Scope::Project, WINDOW_NS);
        busy |= self.analysis.busy() || self.analysis.profile_pending();
        let (Some(prep), Some(idx), Some(timing)) = (self.player.current(), idx, timing) else {
            return (self.inspector.last.clone(), true, true);
        };
        let n = self.inspector.tempo_changes(prep);
        let v = inspector::values(&doc, prep, &idx, &timing, profile.as_deref(), n);
        let fresh = prep.snapshot.key() == doc.export_key();
        if let Some(p) = &profile {
            if self.settings.inspector_follow_playhead && !self.inspector.follow_suspended {
                self.inspector.view.window = followed_window(
                    self.inspector.view.window,
                    &timing,
                    v.end_tick,
                    self.transport.position(),
                    self.settings.follow_position,
                );
            }
            let window = self.inspector.view.window;
            busy |= self
                .inspector
                .chart
                .sync_in(&idx, p, &timing, v.end_tick, columns, window);
        }
        self.inspector.last = Some(v.clone());
        (Some(v), !fresh, busy || !fresh || profile.is_none())
    }

    /// The lines the window shows, and whether work is still running.
    fn inspector_rows(&mut self, columns: usize) -> Option<(Vec<Row>, bool)> {
        let (v, stale, busy) = self.inspector_values(columns);
        let checking = self.drive_save_check();
        let now = crate::recent::now_secs();
        let out = v.map(|v| {
            let mut rows = inspector::rows(&v, stale);
            rows.extend(inspector::save_row(&self.inspector.save_check, now));
            (rows, busy || checking)
        });
        self.inspector.drawn = out.clone();
        out
    }

    /// After a successful save (from `finish_save`): the Inspector's live
    /// values are checked against a fresh analysis of the saved version
    /// once that version's live analysis is ready (I7).
    pub(super) fn inspector_saved(&mut self) {
        if !self.inspector_shown() {
            return;
        }
        if let Some(o) = self.open.as_ref() {
            self.inspector.save_check = SaveCheck::Waiting(NotesKey::of(&o.doc, o.generation));
        }
    }

    /// Advances the on-save check; true while it is waiting or running.
    fn drive_save_check(&mut self) -> bool {
        match &self.inspector.save_check {
            SaveCheck::Waiting(key) => {
                let Some(o) = self.open.as_ref() else {
                    self.inspector.save_check = SaveCheck::None;
                    return false;
                };
                if NotesKey::of(&o.doc, o.generation) != *key {
                    // Edited before the live analysis of the saved version
                    // was ready: that version is gone, nothing to check.
                    self.inspector.save_check = SaveCheck::None;
                    return false;
                }
                let doc = o.doc.clone();
                let (Some(index), Some(timing)) = (
                    self.analysis.index().cloned(),
                    self.analysis.timing().cloned(),
                ) else {
                    return true;
                };
                let Some(profile) = self.analysis.profile(&Scope::Project, WINDOW_NS) else {
                    return true;
                };
                let live = inspector::Live {
                    index,
                    profile,
                    timing,
                };
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new()
                    .name("plumspirit-inspector-check".into())
                    .spawn(move || {
                        let _ = tx.send(inspector::check(&doc, &live));
                    });
                self.inspector.save_check = match spawned {
                    Ok(_) => SaveCheck::Running(rx),
                    Err(e) => {
                        log::warn!("the Inspector's check after saving could not start: {e}");
                        SaveCheck::None
                    }
                };
                true
            }
            SaveCheck::Running(rx) => match rx.try_recv() {
                Ok(r) => {
                    if let Err(e) = &r {
                        log::error!(
                            "internal error: after saving, a fresh analysis differs from the live one ({e}); analyzing the project again"
                        );
                        self.analysis.reset();
                        self.inspector.chart.clear();
                    }
                    self.inspector.save_check = SaveCheck::Done(crate::recent::now_secs(), r);
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.inspector.save_check = SaveCheck::None;
                    false
                }
            },
            SaveCheck::None | SaveCheck::Done(..) => false,
        }
    }

    /// Per frame (from `update_live`): the window while it is shown.
    pub(super) fn update_inspector_window(&mut self, ctx: &egui::Context) {
        if !self.inspector_shown() {
            return;
        }
        let mut open = true;
        let mut busy = false;
        // Beside the editor on the right, clear of the track panel.
        let screen = ctx.content_rect();
        let mut want_help = false;
        egui::Window::new("MIDI Inspector")
            .open(&mut open)
            .default_pos(egui::pos2(
                (screen.right() - 440.0).max(0.0),
                screen.top() + 96.0,
            ))
            .default_width(400.0)
            .default_height(760.0)
            .resizable(true)
            .max_height((screen.height() - 16.0).max(80.0))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("What the exported MIDI file holds").weak());
                    want_help |= super::help::help_button(ui, "player-inspector");
                });
                let width = ui.available_width().max(80.0);
                let ppp = ctx.pixels_per_point();
                let columns = ((width * ppp).round() as usize).clamp(1, inspector::MAX_COLUMNS);
                let Some((rows, b)) = self.inspector_rows(columns) else {
                    ui.label("Analyzing the project…");
                    busy = true;
                    return;
                };
                busy = b;
                let weak = ui.visuals().weak_text_color();
                egui::Grid::new("inspector_rows")
                    .num_columns(3)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for r in &rows {
                            ui.label(r.label);
                            ui.label(&r.value);
                            ui.label(egui::RichText::new(r.tag).color(weak));
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                if let Some(v) = self.inspector.last.clone() {
                    busy |= self.inspector_charts(ui, &v);
                }
            });
        if busy {
            ctx.request_repaint_after(POLL);
        }
        if !open {
            self.set_inspector(false);
        }
        if want_help {
            self.open_help("player-inspector");
        }
    }
}

impl PlumSpiritApp {
    /// B1-0108 / B1-0109: the toolbar, the NPS / Polyphony / Tempo charts
    /// and the scrollbar (`analytics_charts::charts_ui`, shared with
    /// Selection Analytics). Returns true while something is computed.
    fn inspector_charts(&mut self, ui: &mut egui::Ui, v: &InspectorValues) -> bool {
        let (Some(timing), Some(index)) = (
            self.analysis.timing().cloned(),
            self.analysis.index().cloned(),
        ) else {
            return true;
        };
        let peaks = self.inspector_peak_ticks();
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
        let data = analytics_charts::ChartsData {
            id: "insp",
            timing: &timing,
            end: v.end_tick.max(1),
            chart: &self.inspector.chart,
            index: &index,
            maxima: (Some(v.max_nps.unwrap_or(0)), Some(v.max_poly.unwrap_or(0))),
            max_hint: "The song's exact maximum (never read from the bars).",
            peak_ticks: peaks,
            playhead: self.transport.position(),
            meter,
            tempo_points: &points,
            ppqn,
            fit: Window::default(),
            fit_hint: "The whole song (double-click a chart: also its height)",
            seek: true,
            follow: Some(self.settings.inspector_follow_playhead),
            follow_hint: "Follow all three charts with page turns at the Playback menu's Follow Position. Manual chart navigation pauses following; Resume, Play, Stop or Seek resumes it.",
            counter: None,
            exact_at: None,
        };
        let out = analytics_charts::charts_ui(
            ui,
            &mut self.inspector.view,
            &mut self.inspector.follow_suspended,
            &mut self.inspector.chart_rects,
            &data,
        );
        if let Some(on) = out.follow {
            self.set_inspector_follow(on);
        }
        if out.resume {
            self.inspector.follow_suspended = false;
        }
        if let Some(tick) = out.seek {
            self.seek_to(tick);
        }
        out.busy
    }

    /// The ticks where the highest NPS and polyphony are first reached
    /// (from the exact peak profile), when it is ready.
    fn inspector_peak_ticks(&mut self) -> (Option<u64>, Option<u64>) {
        let (Some(idx), Some(timing)) = (
            self.analysis.index().cloned(),
            self.analysis.timing().cloned(),
        ) else {
            return (None, None);
        };
        let Some(p) = self.analysis.profile(&Scope::Project, WINDOW_NS) else {
            return (None, None);
        };
        let scope = Scope::Project;
        let (nps, nps_t) = p.peak_nps_first(&idx, &scope, &timing);
        let (poly, poly_t) = p.peak_poly_first(&idx, &scope);
        let _: &Timing = &timing;
        ((nps > 0).then_some(nps_t), (poly > 0).then_some(poly_t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::analytics_charts::bpm_label;
    use crate::app::commands::Command;
    use crate::app::test_support::Harness;
    use crate::inspector::{Axis, YView};
    use plumspirit_engine::midi::group_digits;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};

    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("i".into(), Bpm::parse("140").unwrap(), 480);
        d.add_track().unwrap();
        for t in 0..2usize {
            let mut recs: Vec<NoteRecord> = (0..3_000u64)
                .map(|k| {
                    NoteRecord::new(
                        k * 30 + t as u64 * 5,
                        20 + (k % 9) * 25,
                        NoteAttrs::new(
                            (30 + (k * 7) % 60) as u8,
                            90,
                            t as u8,
                            0,
                            NoteOffKind::NoteOff,
                        )
                        .unwrap(),
                    )
                })
                .collect();
            recs.sort_by_key(|n| n.start_tick);
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    /// The rows drawn once everything is current (frames simulated; the
    /// window draws inside `update_live`).
    fn settled(h: &mut Harness) -> Vec<Row> {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let ctx = h.ctx.clone();
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 1800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    h.app.update_live(ui.ctx(), true);
                },
            );
            out.textures_delta.clear();
            if let Some((rows, busy)) = h.app.inspector.drawn.clone()
                && !busy
                && rows.iter().all(|r| r.tag != inspector::UPDATING)
            {
                return rows;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the Inspector never settled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn value(rows: &[Row], label: &str) -> String {
        rows.iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("no {label} in {rows:?}"))
            .value
            .clone()
    }

    fn chart_frame(h: &mut Harness, events: Vec<egui::Event>) -> Vec<egui::epaint::ClippedShape> {
        let ctx = h.ctx.clone();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| h.app.update_live(ui.ctx(), true),
        );
        out.textures_delta.clear();
        out.shapes
    }

    fn click_chart_control(h: &mut Harness, label: &str) {
        let texts = crate::ui::testkit::texts(&chart_frame(h, vec![]));
        let pos = texts
            .iter()
            .find(|(t, _)| t == label)
            .unwrap_or_else(|| panic!("{label} absent: {texts:?}"))
            .1;
        chart_frame(h, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            chart_frame(
                h,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    }

    #[test]
    fn inspector_follow_tracks_seeks_wraps_and_tempo_edits_without_reanalysis() {
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::conductor::Interp;
        use plumspirit_engine::model::tempo::TempoValue;
        let mut d = doc();
        for (tick, bpm, interp) in [(0, "60", Interp::Linear), (30_000, "240", Interp::Step)] {
            let edit = d
                .conductor()
                .prepare_set_tempo(
                    None,
                    tick,
                    TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                    interp,
                )
                .unwrap();
            History::default()
                .apply(
                    &mut d,
                    Operation::Conductor {
                        edit,
                        label: "Tempo",
                    },
                )
                .unwrap();
        }
        let mut h = Harness::new(d);
        h.command(Command::ToggleMidiInspector);
        settled(&mut h);
        let index = h.app.analysis.index().unwrap().clone();
        let profile = h.app.analysis.profile(&Scope::Project, WINDOW_NS).unwrap();
        let builds = h.app.analysis.builds_started;
        let end = h.app.inspector.last.as_ref().unwrap().end_tick;
        for axis in [Axis::Time, Axis::Musical] {
            h.app.inspector.view.window = Window {
                axis,
                lo: 0.0,
                hi: 0.1,
            };
            h.app.inspector.view.y[2].zoom = 4.0;
            // Forward playback, pause, a loop wrap, restart, and the end.
            for tick in [end * 3 / 4, end * 3 / 4 + 10, 2_000, 0, end] {
                h.app.seek_to(tick);
                settled(&mut h);
                let w = h.app.inspector.view.window;
                let timing = h.app.analysis.timing().unwrap();
                let f = inspector::fraction_of(timing, axis, end, tick);
                assert!(f >= w.lo - 1e-8 && f <= w.hi + 1e-8, "{tick}: {w:?}, {f}");
                assert!((w.span() - 0.1).abs() < 1e-8);
                assert_eq!(h.app.inspector.view.y[2].zoom, 4.0);
                assert!(h.app.inspector.chart.columns.len() <= inspector::MAX_COLUMNS);
                assert_eq!(
                    h.app.inspector.chart.columns,
                    profile.chart_columns(
                        &index,
                        &Scope::Project,
                        timing,
                        h.app.inspector.chart.bounds()
                    )
                );
                let count = h.app.inspector.chart.full_builds;
                for _ in 0..3 {
                    settled(&mut h);
                }
                assert_eq!(
                    h.app.inspector.chart.full_builds, count,
                    "stationary playhead reuses columns"
                );
            }
        }
        assert!(std::sync::Arc::ptr_eq(
            h.app.analysis.index().unwrap(),
            &index
        ));
        assert!(std::sync::Arc::ptr_eq(
            &h.app.analysis.profile(&Scope::Project, WINDOW_NS).unwrap(),
            &profile
        ));
        assert_eq!(h.app.analysis.builds_started, builds);
        h.app.inspector.view.window.axis = Axis::Time;
        h.app.seek_to(end / 2);
        let edit = h
            .doc()
            .conductor()
            .prepare_set_tempo(
                None,
                end / 3,
                TempoValue::Bpm(Bpm::parse("20").unwrap()),
                Interp::Step,
            )
            .unwrap();
        h.app.apply_op(Operation::Conductor {
            edit,
            label: "Tempo",
        });
        settled(&mut h);
        let w = h.app.inspector.view.window;
        let f = inspector::fraction_of(
            h.app.analysis.timing().unwrap(),
            Axis::Time,
            end,
            h.app.transport.position(),
        );
        assert!(
            f >= w.lo - 1e-8 && f <= w.hi + 1e-8,
            "edited tempo: {w:?}, {f}"
        );
        h.app.inspector.follow_suspended = true;
        h.app.stop_playback();
        assert!(!h.app.inspector.follow_suspended);
    }

    #[test]
    fn inspector_manual_navigation_pauses_follow_and_resume_and_toggle_work() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiInspector);
        settled(&mut h);
        assert!(h.app.settings.inspector_follow_playhead);
        for k in 0..3 {
            for modifiers in [
                egui::Modifiers::COMMAND,
                egui::Modifiers::ALT,
                egui::Modifiers::NONE,
            ] {
                chart_frame(&mut h, vec![]);
                let pos = h.app.inspector.chart_rects[k].center();
                chart_frame(&mut h, vec![egui::Event::PointerMoved(pos)]);
                chart_frame(
                    &mut h,
                    vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -100.0),
                        modifiers,
                        phase: egui::TouchPhase::Move,
                    }],
                );
                assert!(h.app.inspector.follow_suspended, "chart {k}, {modifiers:?}");
                let w = h.app.inspector.view.window;
                settled(&mut h);
                assert_eq!(
                    h.app.inspector.view.window, w,
                    "paused following leaves manual navigation alone"
                );
                click_chart_control(&mut h, "Resume");
                assert!(!h.app.inspector.follow_suspended);
            }
        }
        click_chart_control(&mut h, "Follow Playhead");
        assert!(!h.app.settings.inspector_follow_playhead);
        h.app.inspector.view.window = Window {
            axis: Axis::Musical,
            lo: 0.1,
            hi: 0.2,
        };
        h.app.seek_to(80_000);
        settled(&mut h);
        assert_eq!(
            h.app.inspector.view.window.lo, 0.1,
            "off stays off after a seek"
        );
        click_chart_control(&mut h, "Follow Playhead");
        settled(&mut h);
        assert!(h.app.inspector.view.window.lo > 0.2);
        h.app.set_inspector(false);
        assert!(h.app.inspector.chart.columns.is_empty());
        assert!(h.app.inspector.last.is_none());
        assert!(h.app.settings.inspector_follow_playhead);
        h.app.set_inspector(true);
        settled(&mut h);
        assert!(h.app.inspector.view.window.is_full());
        assert!(!h.app.inspector.follow_suspended);
    }

    #[test]
    fn the_midi_inspector_toggles_from_the_menu_command_and_f11() {
        let mut h = Harness::new(doc());
        assert!(!h.app.inspector_shown());
        assert!(h.command(Command::ToggleMidiInspector));
        assert!(h.app.inspector_shown() && h.app.settings.inspector);
        assert!(h.command(Command::ToggleMidiInspector));
        assert!(!h.app.inspector_shown());
        h.press(egui::Key::F11);
        assert!(h.app.inspector_shown(), "F11");
        h.press(egui::Key::F11);
        assert!(!h.app.inspector_shown());
        assert!(h.app.inspector.last.is_none(), "closing keeps nothing");
    }

    #[test]
    fn the_inspector_shows_the_exported_files_exact_values_and_follows_edits() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiInspector);
        let rows = settled(&mut h);
        let snap = crate::player::snapshot(h.app.open.as_ref().unwrap().doc.as_ref());
        assert_eq!(value(&rows, "Notes"), group_digits(snap.note_count()));
        assert_eq!(value(&rows, "Timing"), "480 PPQN");
        assert_eq!(value(&rows, "BPM"), "140");
        assert!(
            rows.iter().all(|r| r.tag != inspector::UPDATING),
            "{rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.label == "In the project"),
            "nothing hidden"
        );
        // Bounded chart, its highest column the exact peaks.
        let cols = h.app.inspector.chart.columns.clone();
        assert!(
            (100..=inspector::MAX_COLUMNS).contains(&cols.len()),
            "one column per pixel of the window: {}",
            cols.len()
        );
        let v = h.app.inspector.last.clone().unwrap();
        let top = cols
            .iter()
            .fold((0, 0), |m, c| (m.0.max(c.0), m.1.max(c.1)));
        assert_eq!((Some(top.0), Some(top.1)), (v.max_nps, v.max_poly));
        assert_eq!(value(&rows, "Max NPS"), group_digits(top.0));
        // An edit: a chord of 25 notes in the middle — the values and the
        // chart follow without reopening (V09-0054).
        if let Some(o) = h.app.open.as_mut() {
            let recs: Vec<NoteRecord> = (0..25u8)
                .map(|k| {
                    NoteRecord::new(
                        30_000,
                        400,
                        NoteAttrs::new(40 + k, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            std::sync::Arc::make_mut(&mut o.doc)
                .track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let rows = settled(&mut h);
        assert_eq!(value(&rows, "Notes"), group_digits(snap.note_count() + 25));
        let v2 = h.app.inspector.last.clone().unwrap();
        assert!(v2.max_poly > v.max_poly, "{v2:?}");
        let cols = h.app.inspector.chart.columns.clone();
        let top = cols
            .iter()
            .fold((0, 0), |m, c| (m.0.max(c.0), m.1.max(c.1)));
        assert_eq!((Some(top.0), Some(top.1)), (v2.max_nps, v2.max_poly));
    }

    #[test]
    fn saving_checks_the_inspectors_live_values_against_a_fresh_analysis() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiInspector);
        settled(&mut h);
        // What `finish_save` calls after a verified save.
        h.app.inspector_saved();
        let rows = settled(&mut h);
        assert!(
            value(&rows, "Saved").starts_with("values checked against a fresh analysis ("),
            "{rows:?}"
        );
        assert!(matches!(
            h.app.inspector.save_check,
            SaveCheck::Done(_, Ok(()))
        ));
        // Saved, then edited before the check could start: nothing checked.
        h.app.inspector_saved();
        if let Some(o) = h.app.open.as_mut() {
            std::sync::Arc::make_mut(&mut o.doc)
                .track_mut(1)
                .unwrap()
                .notes
                .select_all();
            std::sync::Arc::make_mut(&mut o.doc)
                .track_mut(1)
                .unwrap()
                .notes
                .set_selected_durations(7)
                .unwrap();
        }
        let rows = settled(&mut h);
        assert!(!rows.iter().any(|r| r.label == "Saved"), "{rows:?}");
    }

    #[test]
    fn clicking_a_chart_moves_the_playhead_to_that_column() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiInspector);
        settled(&mut h);
        let frame = |h: &mut Harness, events: Vec<egui::Event>| {
            let ctx = h.ctx.clone();
            let raw = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| h.app.update_live(ui.ctx(), true));
            out.textures_delta.clear();
        };
        frame(&mut h, vec![]);
        let rect = h.app.inspector.chart_rects[1];
        let pos = egui::pos2(rect.left() + rect.width() * 0.75, rect.center().y);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut h, vec![egui::Event::PointerMoved(pos)]);
        frame(&mut h, vec![button(true)]);
        frame(&mut h, vec![button(false)]);
        let bounds = h.app.inspector.chart.bounds().to_vec();
        let want = inspector::seek_tick(&bounds, 0.75).unwrap();
        assert!(want > 0);
        // The column under the pointer (within one column of 75 %).
        let got = h.app.transport.position();
        let k = bounds.partition_point(|&b| b <= got).saturating_sub(1);
        let kw = bounds.partition_point(|&b| b <= want).saturating_sub(1);
        assert!(
            k.abs_diff(kw) <= 1,
            "playhead {got} (column {k}), expected column {kw}"
        );
        assert_eq!(got, bounds[k], "a column's start");
    }

    /// B1-0108 / B1-0109: three charts (NPS, Polyphony, Tempo); Ctrl+wheel
    /// over a chart zooms the shared window at the pointer and the columns
    /// follow it exactly, Alt+wheel zooms that chart's height only, a wheel
    /// pans, a double-click fits; the Tempo chart's labels show the BPM.
    #[test]
    fn the_inspector_charts_zoom_pan_and_fit_with_exact_columns() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiInspector);
        settled(&mut h);
        let frame = |h: &mut Harness, events: Vec<egui::Event>| {
            let ctx = h.ctx.clone();
            let raw = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| h.app.update_live(ui.ctx(), true));
            out.textures_delta.clear();
        };
        frame(&mut h, vec![]);
        assert_eq!(
            h.app.inspector.chart_rects.len(),
            3,
            "NPS, Polyphony, Tempo"
        );
        let nps = h.app.inspector.chart_rects[0];
        let at = egui::pos2(nps.left() + nps.width() * 0.25, nps.center().y);
        let wheel = |m: egui::Modifiers, dy: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            modifiers: m,
            phase: egui::TouchPhase::Move,
        };
        frame(&mut h, vec![egui::Event::PointerMoved(at)]);
        frame(&mut h, vec![wheel(egui::Modifiers::COMMAND, 400.0)]);
        let w = h.app.inspector.view.window;
        assert!(w.span() < 0.9, "Ctrl+wheel zooms in: {w:?}");
        assert!(
            (w.lo + 0.25 * w.span() - 0.25).abs() < 0.01,
            "about the pointer: {w:?}"
        );
        // The columns are recomputed for the window (inline small song).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while h.app.inspector.chart.window() != Some(w) {
            assert!(std::time::Instant::now() < deadline);
            frame(&mut h, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
        let b = h.app.inspector.chart.bounds().to_vec();
        assert!(b[0] > 0, "the window starts after the song start");
        // Alt+wheel: this chart's height only.
        frame(&mut h, vec![wheel(egui::Modifiers::ALT, 400.0)]);
        assert!(h.app.inspector.view.y[0].zoom > 1.0);
        assert_eq!(h.app.inspector.view.y[1], YView::default());
        assert_eq!(h.app.inspector.view.window, w, "the window is unchanged");
        // A plain wheel pans later.
        frame(&mut h, vec![wheel(egui::Modifiers::NONE, -200.0)]);
        let p = h.app.inspector.view.window;
        assert!(p.lo > w.lo && (p.span() - w.span()).abs() < 1e-9, "{p:?}");
        // A double-click fits the window and the chart's height.
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut h, vec![click(true), click(false)]);
        frame(&mut h, vec![click(true), click(false)]);
        assert!(h.app.inspector.view.window.is_full());
        assert_eq!(h.app.inspector.view.y[0], YView::default());
        assert_eq!(bpm_label(120.0), "120");
        assert_eq!(bpm_label(60_000_000.0), "60,000,000");
        assert_eq!(bpm_label(3.5762788), "3.5762788");
        assert_eq!(bpm_label(128.12345), "128.123");
    }

    #[test]
    fn the_inspector_window_paints_its_rows_and_two_bounded_charts() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiInspector);
        settled(&mut h);
        let ctx = h.ctx.clone();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            h.app.update_inspector_window(ui.ctx());
        });
        out.textures_delta.clear();
        let mut texts = Vec::new();
        let mut meshes = 0;
        let mut quads = 0;
        for c in &out.shapes {
            match &c.shape {
                egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
                egui::Shape::Mesh(m) => {
                    meshes += 1;
                    quads += m.vertices.len() / 4;
                }
                _ => {}
            }
        }
        assert!(texts.iter().any(|t| t == "Notes"), "{texts:?}");
        assert!(
            texts.iter().any(|t| t.starts_with("NPS — max ")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with("Polyphony — max ")),
            "{texts:?}"
        );
        assert!(meshes >= 2, "one mesh per chart");
        assert!(
            quads <= 2 * inspector::MAX_COLUMNS,
            "bounded: {quads} quads"
        );
    }

    /// Resident memory of this process (Linux; 0 elsewhere), bytes.
    fn rss() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .map_or(0, |kb| kb * 1024)
    }

    /// B2-F30: same-process whole-song / following frame comparison. No
    /// analysis rebuild is allowed as the canonical transport advances.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit inspector_follow_frame_cost -- --ignored --nocapture"]
    fn inspector_follow_frame_cost() {
        use plumspirit_engine::fixtures::{FixtureShape, generate};
        use std::time::Instant;
        for n in [1_000_000, 10_000_000] {
            let mut h = Harness::new(generate(FixtureShape::Dense, n, 960, 5));
            h.command(Command::ToggleMidiInspector);
            chart_frame(&mut h, vec![]);
            settled(&mut h);
            let d = h.doc().clone();
            let generation = h.app.doc_generation();
            let index = h.app.analysis.index().unwrap().clone();
            let profile = h.app.analysis.profile(&Scope::Project, WINDOW_NS).unwrap();
            let builds = h.app.analysis.builds_started;
            let end = h.app.inspector.last.as_ref().unwrap().end_tick;
            for zoom in [1.0, 16.0] {
                h.app.inspector.view.window = Window::default().zoomed(zoom, 0.0);
                h.app.seek_to(0);
                settled(&mut h);
                let queries = h.app.inspector.chart.full_builds;
                let mut frames = Vec::new();
                for k in 0..180 {
                    h.app.transport.seek(end * k / 180, &d, generation);
                    let t = Instant::now();
                    let shapes = chart_frame(&mut h, vec![]);
                    std::hint::black_box(h.ctx.tessellate(shapes, h.ctx.pixels_per_point()));
                    frames.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                settled(&mut h);
                frames.sort_by(f64::total_cmp);
                println!(
                    "inspector follow {n} notes, {zoom}x: frame p50 {:.3} ms / p99 {:.3} ms / max {:.3} ms, {} bounded chart queries, 0 analysis rebuilds",
                    frames[90],
                    frames[178],
                    frames[179],
                    h.app.inspector.chart.full_builds - queries
                );
                assert!(std::sync::Arc::ptr_eq(
                    h.app.analysis.index().unwrap(),
                    &index
                ));
                assert!(std::sync::Arc::ptr_eq(
                    &h.app.analysis.profile(&Scope::Project, WINDOW_NS).unwrap(),
                    &profile
                ));
                assert_eq!(h.app.analysis.builds_started, builds);
                assert!(h.app.inspector.chart.columns.len() <= inspector::MAX_COLUMNS);
                assert_eq!(
                    h.app.inspector.chart.columns,
                    profile.chart_columns(
                        &index,
                        &Scope::Project,
                        h.app.analysis.timing().unwrap(),
                        h.app.inspector.chart.bounds()
                    )
                );
            }
        }
    }

    /// V09-0053 / V09-0054 bench (release, `docs/INSPECTOR.md`): the
    /// Inspector shown at 1 M and 10 M notes (16 tracks of chords, a tempo
    /// change every 32 bars): until the values and chart are ready, settled
    /// frames (window painted and tessellated), a one-note edit until the
    /// values and chart are current again (in the frame or on a worker), the
    /// on-save check on its worker, and the process RSS.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit inspector_frame_cost -- --ignored --nocapture"]
    fn inspector_frame_cost() {
        use std::time::Instant;
        for total in [1_000_000u64, 10_000_000] {
            let rss0 = rss();
            let per_track = total / 16;
            let mut d = PlumDocument::new_project("bench".into(), Bpm::parse("140").unwrap(), 960);
            for _ in 1..16 {
                d.add_track().unwrap();
            }
            for ti in 0..16usize {
                let mut recs: Vec<NoteRecord> = (0..per_track)
                    .map(|k| {
                        let chord = k / 16;
                        NoteRecord::new(
                            chord * 120 + ti as u64 * 3,
                            90 + (k % 5) * 30,
                            NoteAttrs::new(
                                (20 + (chord * 7 + k % 16 * 5 + ti as u64) % 90) as u8,
                                1 + (k % 126) as u8,
                                (ti % 16) as u8,
                                0,
                                NoteOffKind::NoteOff,
                            )
                            .unwrap(),
                        )
                    })
                    .collect();
                recs.sort_by_key(|r| r.start_tick);
                d.track_mut(ti)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&recs, false)
                    .unwrap();
            }
            let rss_doc = rss();
            let mut h = Harness::new(d);
            h.command(Command::ToggleMidiInspector);
            let frame = |h: &mut Harness| -> Duration {
                let ctx = h.ctx.clone();
                let t = Instant::now();
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    h.app.update_live(ui.ctx(), true);
                });
                let shapes = std::mem::take(&mut out.shapes);
                let prims = ctx.tessellate(shapes, out.pixels_per_point);
                std::hint::black_box(prims);
                let e = t.elapsed();
                out.textures_delta.clear();
                e
            };
            let current = |h: &Harness| {
                h.app.inspector.drawn.as_ref().is_some_and(|(rows, busy)| {
                    !busy && rows.iter().all(|r| r.tag != inspector::UPDATING)
                })
            };
            let until_current = |h: &mut Harness| -> (Duration, usize, Duration) {
                let t = Instant::now();
                let mut frames = 0;
                let mut slowest = Duration::ZERO;
                loop {
                    slowest = slowest.max(frame(h));
                    frames += 1;
                    if current(h) {
                        return (t.elapsed(), frames, slowest);
                    }
                    assert!(t.elapsed() < Duration::from_secs(300), "never current");
                    std::thread::sleep(Duration::from_millis(1));
                }
            };
            let (ready, ready_frames, ready_slowest) = until_current(&mut h);
            let rss_ready = rss();
            let stat = |mut v: Vec<Duration>| {
                v.sort();
                (
                    v[v.len() / 2].as_secs_f64() * 1e3,
                    v[v.len() - 1].as_secs_f64() * 1e3,
                )
            };
            let settled = stat((0..120).map(|_| frame(&mut h)).collect());
            // A one-note edit in the middle of the song.
            let (full0, upd0) = (
                h.app.inspector.chart.full_builds,
                h.app.inspector.chart.updates,
            );
            let prof0 = h.app.analysis.profiles_updated;
            let mid = h.doc().tracks()[0].notes.max_end().unwrap_or(0) / 2;
            if let Some(o) = h.app.open.as_mut() {
                let rec = NoteRecord::new(
                    mid,
                    480,
                    NoteAttrs::new(64, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                );
                std::sync::Arc::make_mut(&mut o.doc)
                    .track_mut(0)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&[rec], false)
                    .unwrap();
            }
            let (edit, edit_frames, edit_slowest) = until_current(&mut h);
            let chart_how = if h.app.inspector.chart.updates > upd0 {
                "columns updated in the frame"
            } else if h.app.inspector.chart.full_builds > full0 {
                "whole chart on the worker"
            } else {
                "columns updated on the worker"
            };
            let profile_how = if h.app.analysis.profiles_updated > prof0 {
                "profile updated in the frame"
            } else {
                "profile on the worker"
            };
            // The on-save check (fresh analysis on its worker).
            h.app.inspector_saved();
            let t = Instant::now();
            loop {
                frame(&mut h);
                if matches!(h.app.inspector.save_check, SaveCheck::Done(..)) {
                    break;
                }
                assert!(
                    t.elapsed() < Duration::from_secs(300),
                    "the check never finished"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            let check = t.elapsed();
            let ok = matches!(h.app.inspector.save_check, SaveCheck::Done(_, Ok(())));
            let mb = |b: u64| b as f64 / 1_048_576.0;
            println!(
                "inspector bench {} notes (16 tracks): ready (snapshot, index, profile, values, {} chart columns) after {:.0} ms ({} frames, slowest {:.1} ms); settled frame median {:.2} / slowest {:.2} ms; one-note edit current after {:.1} ms ({} frames, slowest {:.2} ms; {}, {}); on-save check {:.0} ms ({}); RSS doc +{:.1} MB, Inspector ready +{:.1} MB over the doc (incl. the shared analysis index)",
                total,
                h.app.inspector.chart.columns.len(),
                ready.as_secs_f64() * 1e3,
                ready_frames,
                ready_slowest.as_secs_f64() * 1e3,
                settled.0,
                settled.1,
                edit.as_secs_f64() * 1e3,
                edit_frames,
                edit_slowest.as_secs_f64() * 1e3,
                profile_how,
                chart_how,
                check.as_secs_f64() * 1e3,
                if ok { "agreed" } else { "DIFFERED" },
                mb(rss_doc.saturating_sub(rss0)),
                mb(rss_ready.saturating_sub(rss_doc)),
            );
            assert!(ok);
        }
    }
}
