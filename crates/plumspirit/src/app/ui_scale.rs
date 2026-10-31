//! B1-0062: the global UI scale (30 – 175 %).
//!
//! The scale is egui's zoom factor: `pixels_per_point = zoom × the
//! monitor's own scale`, so every point-based size — menus, icons, text,
//! panels, dialogs, the keyboard, graphs — scales together, and a move to a
//! monitor with another DPI keeps working (egui takes the new native scale
//! and multiplies it). The Piano Roll's musical zoom (`RollView`: pixels per
//! tick, row height) is stored in points and is never rewritten by a scale
//! change, and nothing in the document (note ticks, lengths, pitches,
//! selection) depends on it; the dense tiles pick their level from the
//! physical pixels per tick, as they do for a high-DPI monitor.

use super::PlumSpiritApp;
use crate::settings::UI_SCALE_RANGE;

/// The egui zoom factor of a UI scale percent (clamped to 30 – 175 %).
pub fn zoom_of(percent: u16) -> f32 {
    f32::from(percent.clamp(UI_SCALE_RANGE.0, UI_SCALE_RANGE.1)) / 100.0
}

/// A window size in the current points as points at 100 % UI scale (what
/// the next start creates the window with, before the scale is applied).
pub fn unscaled_size(points: egui::Vec2, zoom: f32) -> [f32; 2] {
    let z = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };
    [points.x * z, points.y * z]
}

impl PlumSpiritApp {
    /// Keeps egui's zoom factor on the setting (cheap; called every frame,
    /// changes only when the setting moved).
    pub(super) fn apply_ui_scale(&self, ctx: &egui::Context) {
        let want = zoom_of(self.settings.ui_scale_percent);
        if (ctx.zoom_factor() - want).abs() > 1e-4 {
            ctx.set_zoom_factor(want);
        }
    }

    /// Sets the UI scale (presets, the typed value, Reset): applied from
    /// the next frame on, saved, reported in the status bar.
    pub(super) fn set_ui_scale(&mut self, percent: u16, ctx: &egui::Context) {
        let p = percent.clamp(UI_SCALE_RANGE.0, UI_SCALE_RANGE.1);
        self.settings_window
            .forget_draft(super::settings_window::UI_SCALE_LABEL);
        if p == self.settings.ui_scale_percent {
            return;
        }
        self.settings.ui_scale_percent = p;
        self.apply_ui_scale(ctx);
        self.save_settings();
        self.set_status(
            format!("UI scale: {p} %."),
            crate::ui::status_bar::StatusKind::Info,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::Command;
    use super::super::settings_window::{Action, Section};
    use super::super::test_support::Harness;
    use super::super::track_list::MainView;
    use crate::ui::testkit::texts;
    use egui::Vec2;

    const SCALES: [u16; 5] = [30, 100, 125, 150, 175];

    fn harness() -> Harness {
        Harness::new(super::super::context_matrix::fixture())
    }

    /// The scale reaches egui (pixels per point = scale × monitor scale) on
    /// the next frame, from Settings, the View menu's command and Reset
    /// section; it is saved like any setting.
    #[test]
    fn the_ui_scale_sets_egui_zoom_and_resets_to_100() {
        let mut h = harness();
        let screen = Vec2::new(1366.0, 768.0);
        h.app_frame(screen, 1.0);
        assert_eq!(h.ctx.pixels_per_point(), 1.0);
        for (p, native) in [(150, 1.0), (175, 1.25), (30, 2.0), (125, 1.5)] {
            h.settings_action(Action::UiScale(p));
            assert_eq!(h.app.settings.ui_scale_percent, p);
            h.app_frame(screen, native);
            h.app_frame(screen, native);
            let want = f32::from(p) / 100.0;
            assert!((h.ctx.zoom_factor() - want).abs() < 1e-6, "{p}");
            assert!(
                (h.ctx.pixels_per_point() - want * native).abs() < 1e-5,
                "{p} % on a {native}× monitor"
            );
            assert!(h.status().contains(&format!("UI scale: {p} %.")));
        }
        // A monitor change (DPI 150 % → 100 %) keeps the user's scale.
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        assert!((h.ctx.pixels_per_point() - 1.25).abs() < 1e-5);
        assert!(h.command(Command::ResetUiScale));
        assert_eq!(h.app.settings.ui_scale_percent, 100);
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        assert_eq!(h.ctx.pixels_per_point(), 1.0);
        h.settings_action(Action::UiScale(60));
        assert_eq!(h.settings_reset(Some(Section::Interface)), 1);
        assert_eq!(h.app.settings.ui_scale_percent, 100);
        // Out-of-range requests are clamped, never stored as they are.
        h.settings_action(Action::UiScale(500));
        assert_eq!(h.app.settings.ui_scale_percent, 175);
        h.settings_action(Action::UiScale(1));
        assert_eq!(h.app.settings.ui_scale_percent, 30);
        assert_eq!(
            super::super::keybinds::shortcuts_of(Command::ResetUiScale),
            ["Ctrl+Alt+0"]
        );
    }

    /// B1-0063: in the toolbar, Zoom X and Zoom Y each read "label, box,
    /// −, +" (the shared stepper pair after the box, the same for both).
    #[test]
    fn the_zoom_boxes_have_minus_then_plus_after_each_box() {
        let mut h = harness();
        let screen = Vec2::new(1366.0, 768.0);
        h.app_frame(screen, 1.0);
        let shapes = h.app_frame(screen, 1.0);
        let shown = texts(&shapes);
        // The toolbar row: the row of the "X" label.
        let x = shown.iter().find(|t| t.0 == "X").expect("Zoom X label").1;
        let mut row: Vec<(String, f32)> = shown
            .iter()
            .filter(|t| (t.1.y - x.y).abs() < 4.0 && t.1.x >= x.x)
            .map(|t| (t.0.clone(), t.1.x))
            .collect();
        row.sort_by(|a, b| a.1.total_cmp(&b.1));
        let seq: Vec<&str> = row.iter().map(|t| t.0.as_str()).take(8).collect();
        assert_eq!(seq[0], "X");
        assert_eq!(&seq[2..4], ["−", "+"], "{seq:?}");
        assert_eq!(seq[4], "Y", "{seq:?}");
        assert_eq!(&seq[6..8], ["−", "+"], "{seq:?}");
    }

    /// B1-0057 acceptance: at 1280 × 800 and 100 % the primary toolbar is
    /// one row in both views, Onion on or off — view switch, Select / Draw /
    /// Erase, Settings / Undo / Redo / Mute, Snap, Onion, Zoom, and in the
    /// Piano Roll Gate / Velocity; the Track List leaves the note fields out.
    #[test]
    fn the_primary_toolbar_is_one_row_at_1280_points() {
        let mut h = harness();
        h.set_notes(0);
        let screen = Vec2::new(1280.0, 800.0);
        // B1-0068 (layout jitter): switching Onion on moves nothing after it.
        let mut zoom_x_at = Vec::new();
        for onion in [false, true] {
            if onion {
                assert!(h.command(Command::ToggleOnion));
            }
            for view in [MainView::TrackList, MainView::PianoRoll] {
                h.app.main_view = view;
                h.app_frame(screen, 1.0);
                h.app_frame(screen, 1.0);
                let shapes = h.app_frame(screen, 1.0);
                let shown = texts(&shapes);
                let y = shown.iter().find(|t| t.0 == "Snap").expect("Snap").1.y;
                let row: Vec<&str> = shown
                    .iter()
                    .filter(|t| (t.1.y - y).abs() < 4.0)
                    .map(|t| t.0.trim())
                    .collect();
                let mut want = vec![
                    "Track List",
                    "Piano Roll",
                    "Select",
                    "Draw",
                    "Erase",
                    "Onion",
                    "X",
                    "Y",
                    "1:1",
                    "Fit",
                ];
                if view == MainView::PianoRoll {
                    want.extend(["Gate", "Vel"]);
                }
                for w in &want {
                    assert!(
                        row.contains(w),
                        "{view:?} onion {onion}: {w} not on the row {row:?}"
                    );
                }
                if view == MainView::PianoRoll {
                    zoom_x_at.push(shown.iter().find(|t| t.0 == "X").expect("X").1.x);
                }
                let gate = shown.iter().any(|t| t.0 == "Gate");
                assert_eq!(gate, view == MainView::PianoRoll, "{view:?}");
                // Nothing of the toolbar on a second row: the next texts
                // below are the transport bar's.
                let below: Vec<&str> = shown
                    .iter()
                    .filter(|t| t.1.y > y + 4.0 && t.1.y < y + 26.0)
                    .map(|t| t.0.as_str())
                    .collect();
                assert!(
                    below
                        .iter()
                        .all(|t| !["Gate", "Vel", "ticks", "X", "Y", "Fit", "Onion"].contains(t)),
                    "{view:?}: wrapped {below:?}"
                );
            }
        }
        assert_eq!(zoom_x_at.len(), 2);
        assert_eq!(zoom_x_at[0], zoom_x_at[1], "Onion on shifted the toolbar");
    }

    /// The saved window size is the size at 100 % (so the next start, which
    /// creates the window before the scale applies, gets the same window).
    #[test]
    fn the_saved_window_size_does_not_follow_the_ui_scale() {
        assert_eq!(
            super::unscaled_size(Vec2::new(780.0, 438.0), 1.75),
            [1365.0, 766.5]
        );
        assert_eq!(
            super::unscaled_size(Vec2::new(800.0, 600.0), 0.0),
            [800.0, 600.0]
        );
        assert_eq!(super::zoom_of(10), 0.3);
        assert_eq!(super::zoom_of(999), 1.75);
    }

    /// B1-0062 acceptance: the whole window (menus, toolbar, panels, Track
    /// List, Piano Roll with the Event Graphs, Settings window) draws at 30 /
    /// 100 / 125 / 150 / 175 % on a 1366 × 768 screen and on a 150 % DPI
    /// monitor; scaling changes neither the document, the selection nor the
    /// Piano Roll's musical zoom; under 60 % the Settings window warns.
    #[test]
    fn every_view_draws_at_every_scale_and_the_notes_do_not_move() {
        let mut h = harness();
        let fp = h.doc().fingerprint();
        h.set_notes(0);
        let selected = h.app.selected_count();
        assert!(h.command(Command::OpenSettings));
        let screen = Vec2::new(1366.0, 768.0);
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        let view0 = h.app.roll.view;
        assert_ne!(view0.time.pixels_per_tick, 0.1, "the roll has drawn");
        for view in [MainView::PianoRoll, MainView::TrackList] {
            h.app.main_view = view;
            for native in [1.0, 1.5] {
                for p in SCALES {
                    h.settings_action(Action::UiScale(p));
                    for section in [Section::Interface, Section::ImportExport] {
                        h.app.settings_window.section = Some(section);
                        h.app_frame(screen, native);
                        let shapes = h.app_frame(screen, native);
                        let shown: Vec<String> = texts(&shapes).into_iter().map(|t| t.0).collect();
                        assert!(shown.iter().any(|t| t == "View"), "{p} %: menu bar");
                        let warns = shown
                            .iter()
                            .any(|t| t.contains("controls may become too small"));
                        assert_eq!(
                            warns,
                            p < 60 && section == Section::Interface,
                            "{p} % {section:?}"
                        );
                    }
                    assert_eq!(h.doc().fingerprint(), fp, "{p} %");
                    assert_eq!(h.app.selected_count(), selected, "{p} %");
                    let v = h.app.roll.view;
                    assert_eq!(v.time.pixels_per_tick, view0.time.pixels_per_tick, "{p} %");
                    assert_eq!(v.time.origin_tick, view0.time.origin_tick, "{p} %");
                    assert_eq!(v.pitch.row_height, view0.pitch.row_height, "{p} %");
                }
            }
        }
    }

    /// B2-A03: inspect actual owned window rectangles, not just whether
    /// shapes were emitted. Controls must stay inside the usable screen.
    #[test]
    fn representative_windows_fit_scaled_small_screens() {
        let mut failures = Vec::new();
        for screen in [Vec2::new(1366.0, 768.0), Vec2::new(960.0, 600.0)] {
            for scale in [30, 100, 175] {
                for kind in 0..9 {
                    let mut h = harness();
                    h.set_notes(0);
                    h.settings_action(Action::UiScale(scale));
                    let (label, id) = match kind {
                        0 => {
                            h.app.set_inspector(true);
                            ("Inspector", egui::Id::new(Some("MIDI Inspector")))
                        }
                        1 => {
                            h.app.open_audio_settings();
                            ("Audio Settings", egui::Id::new(Some("Audio Settings")))
                        }
                        2 => {
                            h.app.open_note_colors();
                            (
                                "Color Preferences",
                                egui::Id::new(Some("General Color Preferences")),
                            )
                        }
                        3 => {
                            h.command(Command::OpenSettings);
                            ("Settings", egui::Id::new(Some("Settings")))
                        }
                        4 => {
                            h.app.open_diagnostics_window();
                            (
                                "Diagnostics",
                                egui::Id::new("plumspirit-diagnostics-report"),
                            )
                        }
                        5 => {
                            h.app.open_tool("transpose");
                            (
                                "Tool / presets / functions",
                                egui::Id::new(("plumspirit-tool", "transpose")),
                            )
                        }
                        6 => {
                            h.app.open_help("glossary");
                            ("Help", egui::Id::new("plumspirit-help"))
                        }
                        7 => {
                            h.command(Command::AddTracks);
                            ("Add Tracks", egui::Id::new("plumspirit-dialog"))
                        }
                        _ => {
                            h.app.open_track_properties(0);
                            (
                                "Track Properties",
                                egui::Id::new("plumspirit-preview-dialog"),
                            )
                        }
                    };
                    for _ in 0..5 {
                        h.app_frame(screen, 1.0);
                    }
                    let usable = h.ctx.content_rect();
                    let rect = h
                        .ctx
                        .memory(|m| m.area_rect(id))
                        .unwrap_or_else(|| panic!("{label} did not open"));
                    if rect.left() < usable.left() - 2.0
                        || rect.top() < usable.top() - 2.0
                        || rect.right() > usable.right() + 2.0
                        || rect.bottom() > usable.bottom() + 2.0
                    {
                        failures.push(format!(
                            "{label}, {scale}%, {screen:?}: {rect:?} outside {usable:?}"
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn short_scaled_windows_scroll_to_their_lower_controls() {
        let screen = Vec2::new(960.0, 600.0);
        for inspector in [true, false] {
            let mut h = harness();
            h.set_notes(0);
            h.settings_action(Action::UiScale(175));
            let id = if inspector {
                h.app.set_inspector(true);
                egui::Id::new(Some("MIDI Inspector"))
            } else {
                h.app.open_tool("transpose");
                egui::Id::new(("plumspirit-tool", "transpose"))
            };
            for _ in 0..6 {
                h.app_frame(screen, 1.0);
            }
            let rect = h.ctx.memory(|m| m.area_rect(id)).unwrap();
            let pos = rect.left_top() + egui::vec2(30.0, 90.0);
            h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(pos)]);
            h.app_frame_events(
                screen,
                1.0,
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -1500.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                }],
            );
            for _ in 0..20 {
                h.app_frame(screen, 1.0);
            }
            let shown = texts(&h.app_frame(screen, 1.0));
            assert!(
                shown.iter().any(|(s, _)| if inspector {
                    s.starts_with("Tempo —")
                } else {
                    s == "Apply"
                }),
                "lower controls reachable: inspector={inspector}, {shown:?}"
            );
        }
    }
}
