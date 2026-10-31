//! View → Note Color Preferences… (v0.9 contract §064–§068, §214): how
//! subchannel note colours are chosen — **Random by Seed** (a stored seed;
//! the same seed gives the same palette after a restart) or **Manual** (up to
//! 16 chosen colours, deterministic automatic colours after them). The
//! preferences are application settings (every project, every restart); the
//! resolved palette lives in the egui context (`theme::note_palette`) and
//! colours every note: Piano Roll, dense tiles, Onion, keyboard, swatches.

use super::PlumSpiritApp;
use crate::ui::theme::{self, MANUAL_NOTE_COLORS, NoteColorMode, NoteColorPrefs, NotePalette};
use egui::{Color32, RichText, Sense, Vec2};
use std::sync::Arc;

/// Subchannels shown in the preview strip (beyond 16 to show the expansion).
pub const PREVIEW_SUBCHANNELS: u32 = 24;

#[derive(Default)]
pub struct NoteColorWindow {
    pub open: bool,
    presets: super::color_presets::PresetUi,
    track_presets: super::color_presets::PresetUi,
}

/// What the window changed this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Edit {
    pub prefs: Option<NoteColorPrefs>,
}

/// A fresh seed for "New Seed" (stored once chosen, so the palette it gives
/// stays the same afterwards).
fn new_seed(previous: u64) -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let s = (t ^ previous.rotate_left(29)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 16;
    if s == 0 || s == previous {
        previous.wrapping_add(1).max(1)
    } else {
        s
    }
}

/// Swaps Manual colours `a` and `b` (reorder), filling the list with the
/// defaults first so positions are stable.
pub fn swap_manual(prefs: &mut NoteColorPrefs, a: usize, b: usize) {
    let mut m = prefs.manual_colors();
    if a < m.len() && b < m.len() {
        m.swap(a, b);
    }
    prefs.manual = m;
}

/// Sets Manual colour `k` (0-based), filling the list with the defaults
/// first.
pub fn set_manual(prefs: &mut NoteColorPrefs, k: usize, rgb: [u8; 3]) {
    let mut m = prefs.manual_colors();
    if let Some(c) = m.get_mut(k) {
        *c = rgb;
    }
    prefs.manual = m;
}

/// Resets Manual colour `k` to its default.
pub fn reset_manual(prefs: &mut NoteColorPrefs, k: usize) {
    let d = NoteColorPrefs::default_manual();
    if let Some(c) = d.get(k) {
        set_manual(prefs, k, *c);
    }
}

/// Draws the window's content; returns the new preferences when anything
/// changed (the caller applies and saves them).
pub fn ui(ui: &mut egui::Ui, prefs: &NoteColorPrefs, dark: bool) -> Edit {
    let mut p = prefs.clone();
    let mut changed = false;
    ui.label("How subchannels get their note colors. These preferences are kept for every project and survive a restart.");
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for m in NoteColorMode::ALL {
            let tip = match m {
                NoteColorMode::RandomBySeed => "Hues chosen from a seed. The same seed always gives the same colors; seed 0 is PlumSpirit's own palette.",
                NoteColorMode::Rainbow16 => "Sixteen rainbow colors in a repeating cycle; subchannel 17 repeats color 1.",
                NoteColorMode::Manual => "Pick the colors of subchannels 1–16 yourself. Later subchannels get automatic variants of them (subchannel 17 never repeats color 1).",
            };
            if ui.radio_value(&mut p.mode, m, m.label()).on_hover_text(tip).changed() {
                changed = true;
            }
        }
    });
    ui.add_space(6.0);
    match p.mode {
        NoteColorMode::Rainbow16 => {
            ui.label(
                "Subchannels 1–16 use the rainbow in order; 17 repeats 1, 18 repeats 2, and so on.",
            );
        }
        NoteColorMode::RandomBySeed => {
            ui.horizontal(|ui| {
                ui.label("Seed");
                changed |= ui
                    .add(crate::ui::stepper::Stepper::new(&mut p.seed).speed(1.0))
                    .on_hover_text("Any whole number. 0 is PlumSpirit's own palette.")
                    .changed();
                if ui
                    .button("New Seed")
                    .on_hover_text("Chooses another seed and keeps it.")
                    .clicked()
                {
                    p.seed = new_seed(p.seed);
                    changed = true;
                }
                if ui
                    .add_enabled(p.seed != 0, egui::Button::new("PlumSpirit Colors"))
                    .on_hover_text("Seed 0: the colors PlumSpirit has always used.")
                    .clicked()
                {
                    p.seed = 0;
                    changed = true;
                }
            });
        }
        NoteColorMode::Manual => {
            let colors = p.manual_colors();
            egui::Grid::new("manual-note-colors")
                .num_columns(4)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for (k, c) in colors.iter().enumerate() {
                        ui.label(RichText::new(format!("{:>2}", k + 1)).monospace());
                        let mut rgb = *c;
                        if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                            set_manual(&mut p, k, rgb);
                            changed = true;
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(k > 0, egui::Button::new("◀").small())
                                .on_hover_text("Move one place earlier")
                                .clicked()
                            {
                                swap_manual(&mut p, k, k - 1);
                                changed = true;
                            }
                            if ui
                                .add_enabled(
                                    k + 1 < MANUAL_NOTE_COLORS,
                                    egui::Button::new("▶").small(),
                                )
                                .on_hover_text("Move one place later")
                                .clicked()
                            {
                                swap_manual(&mut p, k, k + 1);
                                changed = true;
                            }
                        });
                        let default = NoteColorPrefs::default_manual().get(k).copied();
                        if ui
                            .add_enabled(default != Some(*c), egui::Button::new("Reset").small())
                            .on_hover_text("This color back to its default")
                            .clicked()
                        {
                            reset_manual(&mut p, k);
                            changed = true;
                        }
                        ui.end_row();
                    }
                });
            if ui
                .button("Reset All")
                .on_hover_text("All 16 colors back to PlumSpirit's own palette")
                .clicked()
            {
                p.manual = NoteColorPrefs::default_manual();
                changed = true;
            }
        }
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Preview").strong());
    preview(ui, &NotePalette::new(p.clone()), dark);
    Edit {
        prefs: changed.then_some(p),
    }
}

/// Subchannels 1–24 as notes at velocity 1, 64 and 127 (the expansion after
/// 16 is visible).
pub(super) fn preview(ui: &mut egui::Ui, palette: &NotePalette, dark: bool) {
    let cell = Vec2::new(
        (ui.available_width() / PREVIEW_SUBCHANNELS as f32).clamp(4.0, 18.0),
        30.0,
    );
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(cell.x * PREVIEW_SUBCHANNELS as f32, cell.y + 14.0),
        Sense::hover(),
    );
    let p = ui.painter_at(rect);
    let bg = theme::palette(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    })
    .roll_bg;
    p.rect_filled(rect, 0.0, bg);
    for id in 0..PREVIEW_SUBCHANNELS {
        let x = rect.left() + id as f32 * cell.x;
        for (row, vel) in [127u8, 64, 1].into_iter().enumerate() {
            let e = palette.entry(id, vel, dark);
            let r = egui::Rect::from_min_size(
                egui::pos2(x + 2.0, rect.top() + 2.0 + row as f32 * 9.0),
                Vec2::new(cell.x - 4.0, 8.0),
            );
            p.rect_filled(r, 0.0, e.border);
            p.rect_filled(r.shrink(1.0), 0.0, e.fill);
        }
        if id % 4 == 0 || id == 16 {
            p.text(
                egui::pos2(x + cell.x / 2.0, rect.bottom() - 7.0),
                egui::Align2::CENTER_CENTER,
                (id + 1).to_string(),
                egui::FontId::proportional(9.0),
                if id == 16 {
                    Color32::from_rgb(0xE0, 0xB0, 0x5E)
                } else {
                    ui.visuals().weak_text_color()
                },
            );
        }
    }
}

/// The note palette of the application settings: the note colour
/// preferences with the Selection Opacity (v0.9 §119).
pub(super) fn palette_of(settings: &crate::settings::Settings) -> NotePalette {
    NotePalette::new(settings.note_colors.clone()).with_selection(settings.selection_opacity)
}

#[derive(Clone)]
struct CachedTrackPalette {
    stamp: (u64, u64, theme::TrackColorMode, u64),
    palette: Arc<theme::TrackPalette>,
}

impl PlumSpiritApp {
    fn track_palette(&self, ctx: &egui::Context) -> Arc<theme::TrackPalette> {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        if self.settings.track_color_mode == theme::TrackColorMode::Manual {
            self.settings.track_color_manual.hash(&mut hash);
        }
        let stamp =
            self.open
                .as_ref()
                .map_or((0, 0, self.settings.track_color_mode, hash.finish()), |o| {
                    (
                        o.generation,
                        o.doc.structure_revision(),
                        self.settings.track_color_mode,
                        hash.finish(),
                    )
                });
        let id = egui::Id::new("plumspirit-track-palette");
        if let Some(c) = ctx.data(|d| d.get_temp::<CachedTrackPalette>(id))
            && c.stamp == stamp
        {
            return c.palette;
        }
        let ids = self
            .doc()
            .into_iter()
            .flat_map(|d| d.tracks())
            .map(|t| t.id().0);
        let palette = Arc::new(
            theme::TrackPalette::new(self.settings.track_color_mode, ids)
                .with_manual(&self.settings.track_color_manual),
        );
        ctx.data_mut(|d| {
            d.insert_temp(
                id,
                CachedTrackPalette {
                    stamp,
                    palette: palette.clone(),
                },
            )
        });
        palette
    }
    pub(super) fn sync_track_colors(&self, ctx: &egui::Context) {
        let tracks = self.track_palette(ctx);
        let current = theme::note_palette(ctx);
        if current.tracks_match(&tracks) {
            return;
        }
        let next = current.as_ref().clone().with_tracks(tracks);
        if next != *current {
            theme::set_note_palette(ctx, Arc::new(next));
        }
    }
    /// Puts the current preferences' palette into the context (start-up and
    /// every change).
    pub(super) fn apply_note_colors(&self, ctx: &egui::Context) {
        theme::set_note_palette(
            ctx,
            Arc::new(palette_of(&self.settings).with_tracks(self.track_palette(ctx))),
        );
    }

    /// v0.9 §119: sets the Selection Opacity (an application setting; the
    /// palette is replaced, so the exact renderer, the dense tiles, the
    /// velocity lane and previews all follow it at once). `now`: save at
    /// once (the dialog), else a moment later (a dragged slider).
    pub(super) fn set_selection_opacity(&mut self, percent: u8, ctx: &egui::Context, now: bool) {
        let v = percent.min(100);
        if v == self.settings.selection_opacity {
            return;
        }
        self.settings.selection_opacity = v;
        self.apply_note_colors(ctx);
        if now {
            self.save_settings();
            self.set_status(
                format!("Selection opacity: {v} %."),
                crate::app::StatusKind::Info,
            );
        } else {
            self.settings_save_at =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(800));
        }
        ctx.request_repaint();
    }

    pub(super) fn open_note_colors(&mut self) {
        self.note_color_window.open = true;
    }

    pub(super) fn note_colors_ui(&mut self, ctx: &egui::Context) {
        if !self.note_color_window.open {
            return;
        }
        let dark = ctx.theme() == egui::Theme::Dark;
        let mut open = true;
        let mut edit = Edit::default();
        let can_edit_track = self.enabled(super::commands::Command::TrackProperties);
        let mut edit_track = false;
        let mut track_mode = self.settings.track_color_mode;
        let mut track_manual = self.settings.track_color_manual.clone();
        egui::Window::new("General Color Preferences")
            .open(&mut open)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height((ctx.content_rect().height() - 150.0).max(100.0))
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new("Note Color Preferences")
                            .default_open(true).show(ui, |ui| {
                                edit = self::ui(ui, &self.settings.note_colors, dark);
                                if let Some(p)=self.note_color_window.presets.draw(ui,&self.settings.note_colors,dark,super::color_presets::Kind::Note) { edit.prefs=Some(p); }
                            });
                        ui.separator();
                        egui::CollapsingHeader::new("Track Color Preferences")
                            .default_open(true).show(ui, |ui| {
                                ui.radio_value(&mut track_mode,theme::TrackColorMode::Curated,"PlumSpirit Curated Palette");
                                ui.radio_value(&mut track_mode,theme::TrackColorMode::Rainbow16,"Rainbow Through 16 Tracks");
                                ui.radio_value(&mut track_mode,theme::TrackColorMode::Manual,"Manual Track Palette");
                                if track_mode==theme::TrackColorMode::Manual {
                                    let palette=theme::TrackPalette::new(track_mode,0..24).with_manual(&track_manual);
                                    egui::Grid::new("manual-track-colors").num_columns(4).show(ui,|ui| {
                                        for i in 0..16 {
                                            ui.label(format!("{}",i+1));let c=palette.color(i,None,dark,255);let mut rgb=[c.r(),c.g(),c.b()];
                                            if egui::color_picker::color_edit_button_srgb(ui,&mut rgb).changed() {
                                                track_manual=(0..16).map(|k|{let c=palette.color(k,None,dark,255);[c.r(),c.g(),c.b()]}).collect();track_manual[i as usize]=rgb;
                                            }
                                            if i%2==1 {ui.end_row();}
                                        }
                                    });
                                }
                                ui.label("Curated colors keep stable Track identities. Rainbow follows musical Track order, including Subtracks; Conductor and Folder headers do not count. Track Properties overrides take priority.");
                                ui.horizontal_wrapped(|ui| {
                                    let preview=theme::TrackPalette::new(track_mode,0..24).with_manual(&track_manual);
                                    for (id, (name, _, _)) in theme::TRACK_PALETTE.iter().enumerate() {
                                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                                        ui.painter().rect_filled(r, 0.0, preview.color(id as u32, None, dark, 255));
                                        resp.on_hover_text(*name);
                                    }
                                });
                                let current=super::color_presets::track_adapter(track_mode,&track_manual);
                                if let Some(p)=self.note_color_window.track_presets.draw(ui,&current,dark,super::color_presets::Kind::Track) {
                                    track_mode=super::color_presets::track_mode(p.mode);track_manual=p.manual;
                                }
                                edit_track = ui.add_enabled(can_edit_track, egui::Button::new("Track Properties…"))
                                    .on_hover_text("Edit the active Track's individual color override.").clicked();
                            });
                    });
            });
        if track_mode != self.settings.track_color_mode
            || track_manual != self.settings.track_color_manual
        {
            self.settings.track_color_mode = track_mode;
            self.settings.track_color_manual = track_manual;
            self.apply_note_colors(ctx);
            self.save_settings();
            ctx.request_repaint();
        }
        if edit_track && let Some(o) = self.open.as_ref() {
            self.open_track_properties(o.active_track);
        }
        if let Some(p) = edit.prefs {
            log::info!(
                "note colors: {} (seed {}, {} manual color(s))",
                p.mode.label(),
                p.seed,
                p.manual.len()
            );
            self.settings.note_colors = p;
            self.apply_note_colors(ctx);
            // Saved a moment later, so dragging the seed or a colour does not
            // write the file every frame.
            self.settings_save_at =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(800));
            ctx.request_repaint();
        }
        if !open {
            self.note_color_window.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_edits_reorder_and_reset_keep_sixteen_colours() {
        let mut p = NoteColorPrefs {
            mode: NoteColorMode::Manual,
            ..NoteColorPrefs::default()
        };
        let d = NoteColorPrefs::default_manual();
        set_manual(&mut p, 3, [1, 2, 3]);
        assert_eq!(p.manual.len(), MANUAL_NOTE_COLORS);
        assert_eq!(p.manual[3], [1, 2, 3]);
        assert_eq!(p.manual[0], d[0]);
        swap_manual(&mut p, 3, 4);
        assert_eq!((p.manual[3], p.manual[4]), (d[4], [1, 2, 3]));
        reset_manual(&mut p, 4);
        assert_eq!(p.manual[4], d[4]);
        reset_manual(&mut p, 3);
        assert_eq!(p.manual, d);
        // Out of range: nothing changes.
        set_manual(&mut p, 16, [9, 9, 9]);
        swap_manual(&mut p, 15, 16);
        assert_eq!(p.manual, d);
        // The palette follows: colour 4 (index 3) exactly, at full velocity.
        set_manual(&mut p, 3, [200, 10, 90]);
        assert_eq!(NotePalette::new(p.clone()).base_rgb(3, true), [200, 10, 90]);
    }

    #[test]
    fn general_color_preferences_keeps_note_settings_and_exposes_both_panels() {
        use super::super::test_support::Harness;
        let mut h = Harness::new(plumspirit_engine::model::PlumDocument::new_project(
            "colors".into(),
            Default::default(),
            480,
        ));
        h.app.settings.note_colors.seed = 72;
        h.app.settings.note_colors.manual = vec![[14, 80, 230]; 16];
        let before = h.app.settings.clone();
        h.app.open_note_colors();
        let mut labels = Vec::new();
        for _ in 0..3 {
            h.ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(680.0, 520.0),
                )),
                ..Default::default()
            });
            h.app.note_colors_ui(&h.ctx);
            let mut out = h.ctx.end_pass();
            out.textures_delta.clear();
            labels = crate::ui::testkit::texts(&out.shapes)
                .into_iter()
                .map(|(t, _)| t)
                .collect();
        }
        for label in [
            "General Color Preferences",
            "Note Color Preferences",
            "Track Color Preferences",
        ] {
            assert!(labels.iter().any(|t| t == label), "{label}: {labels:?}");
        }
        assert_eq!(h.app.settings, before);
        assert!(!h.app.open.as_ref().unwrap().dirty());
    }

    #[test]
    fn track_palette_tracks_structure_undo_and_nested_tracks_without_touching_midi() {
        use super::super::test_support::Harness;
        use plumspirit_engine::history::{Operation, TrackPlace, plan_track_moves};
        let mut doc = plumspirit_engine::model::PlumDocument::new_project(
            "tracks".into(),
            Default::default(),
            480,
        );
        for _ in 1..33 {
            doc.add_track().unwrap();
        }
        let mut h = Harness::new(doc);
        h.app.settings.track_color_mode = theme::TrackColorMode::Rainbow16;
        h.app.apply_note_colors(&h.ctx);
        let key = theme::note_palette(&h.ctx).key();
        let before = plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap();
        h.app.sync_track_colors(&h.ctx);
        assert_eq!(theme::note_palette(&h.ctx).key(), key);
        assert_eq!(
            plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap(),
            before
        );
        let op = plan_track_moves(h.doc(), &[2], 0, TrackPlace::Into).unwrap();
        h.app.apply_op(op).unwrap();
        h.app.sync_track_colors(&h.ctx);
        assert!(h.doc().tracks().iter().any(|t| t.parent().is_some()));
        for (i, t) in h.doc().tracks().iter().enumerate() {
            let c = theme::note_palette(&h.ctx).track_color(t.id().0, None, true, 255);
            assert_eq!([c.r(), c.g(), c.b()], theme::RAINBOW_16[i % 16]);
        }
        h.command(super::super::commands::Command::Undo);
        h.app.sync_track_colors(&h.ctx);
        assert_eq!(theme::note_palette(&h.ctx).key(), key);
        let id = h.doc().track(0).unwrap().id();
        let mut props = h.doc().track(0).unwrap().props().clone();
        props.onion_color = Some([1, 2, 3]);
        h.app
            .apply_op(Operation::TrackProps {
                track: id,
                props,
                end: None,
                label: "Color",
            })
            .unwrap();
        h.app.sync_track_colors(&h.ctx);
        assert_eq!(
            theme::note_palette(&h.ctx).track_color(
                id.0,
                h.doc().track(0).unwrap().props().onion_color,
                true,
                255
            ),
            Color32::from_rgb(1, 2, 3)
        );
    }

    #[test]
    fn applying_a_manual_track_palette_preserves_notes_midi_and_explicit_overrides() {
        use super::super::test_support::Harness;
        let mut h = Harness::new(plumspirit_engine::model::PlumDocument::new_project(
            "manual".into(),
            Default::default(),
            480,
        ));
        let before = plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap();
        let note = h.app.settings.note_colors.clone();
        h.app.settings.track_color_mode = theme::TrackColorMode::Manual;
        h.app.settings.track_color_manual = vec![[19, 37, 101]; 16];
        h.app.apply_note_colors(&h.ctx);
        let p = theme::note_palette(&h.ctx);
        let c = p.track_color(h.doc().track(0).unwrap().id().0, None, true, 255);
        assert_eq!([c.r(), c.g(), c.b()], [19, 37, 101]);
        assert_eq!(
            p.track_color(0, Some([9, 8, 7]), true, 255),
            Color32::from_rgb(9, 8, 7)
        );
        assert_eq!(h.app.settings.note_colors, note);
        assert_eq!(
            plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap(),
            before
        );
        assert!(!h.app.open.as_ref().unwrap().dirty());
        h.app.open_note_colors();
        h.app.note_color_window.track_presets.pending = Some(
            super::super::color_presets::track_adapter(theme::TrackColorMode::Rainbow16, &[]),
        );
        let before = h.app.settings.clone();
        for _ in 0..2 {
            h.ctx.begin_pass(Default::default());
            h.app.note_colors_ui(&h.ctx);
            let mut o = h.ctx.end_pass();
            o.textures_delta.clear();
        }
        assert_eq!(h.app.settings, before, "preview waits for Apply Import");
    }

    #[test]
    fn a_new_seed_differs_and_is_never_zero() {
        for prev in [0u64, 1, 42, u64::MAX] {
            let s = new_seed(prev);
            assert_ne!(s, prev);
            assert_ne!(s, 0);
        }
    }

    fn frame(prefs: &NoteColorPrefs, dark: bool) -> Edit {
        let ctx = egui::Context::default();
        let mut out = Edit::default();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 900.0),
                )),
                ..Default::default()
            };
            let mut full = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| out = self::ui(ui, prefs, dark));
            });
            full.textures_delta.clear();
        }
        out
    }

    /// The window draws in both modes and both themes and changes nothing by
    /// itself.
    #[test]
    fn the_window_draws_and_waits_for_a_change() {
        for mode in NoteColorMode::ALL {
            for dark in [true, false] {
                let p = NoteColorPrefs {
                    mode,
                    seed: 7,
                    manual: Vec::new(),
                };
                assert_eq!(frame(&p, dark), Edit::default());
            }
        }
    }
}
