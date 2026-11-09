//! A headless `PlumSpiritApp` for tests (v0.9 V09-S10, the context matrix).
//!
//! The app is built through its real constructor with eframe's test
//! creation context and a document installed the way New / Open install
//! one. Actions go through the same entry points as the UI: the
//! track-action dispatcher (`apply_track_action`, used by every track,
//! folder, Conductor and empty-space menu), menu commands (`enabled` +
//! `execute`), keyboard chords (`run_chord`) and the Track List's clip
//! intents (`apply_clip_intent`). The `set_*` helpers put the app into each
//! context of `docs/v0_9/CONTEXT_MATRIX.md`.

use super::commands::Command;
use super::dialogs::Dialog;
use super::track_list::{ClipIntent, MainView};
use super::{Boot, DocSource, PlumSpiritApp, TreeRowView};
use crate::ui::track_panel::TrackAction;
use plumspirit_engine::model::{ClipId, PlumDocument, TrackId};

pub(super) struct Harness {
    pub app: PlumSpiritApp,
    pub ctx: egui::Context,
    frame: eframe::Frame,
    /// The modifier keys held during [`Self::app_frame_events`] frames.
    pub modifiers: egui::Modifiers,
}

impl Harness {
    /// The app with `doc` open (saved, generation 1), Track List shown.
    pub fn new(doc: PlumDocument) -> Harness {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let boot = Boot {
            config_dir: None,
            settings: crate::settings::Settings::default(),
            font: crate::ui::fonts::UiFont {
                kind: crate::ui::fonts::UiFontKind::BuiltIn,
                family: "egui default".into(),
                style: String::new(),
                path: None,
                bytes: None,
                face_index: 0,
            },
            startup_file: None,
            dev_overlay: false,
            renderer: crate::renderer::RunInfo::default(),
            bench_frames: None,
        };
        let mut app = PlumSpiritApp::new(&cc, boot);
        app.install_document(doc, DocSource::New, true);
        app.main_view = MainView::TrackList;
        Harness {
            app,
            ctx,
            frame: eframe::Frame::_new_kittest(),
            modifiers: egui::Modifiers::NONE,
        }
    }

    pub fn doc(&self) -> &PlumDocument {
        self.app.doc().expect("a document")
    }

    pub fn id(&self, i: usize) -> TrackId {
        self.doc().track(i).expect("track").id()
    }

    /// The visible rows of the track tree (what the panel shows).
    pub fn rows(&mut self) -> Vec<TreeRowView> {
        let generation = self.app.doc_generation();
        let o = self.app.open.as_ref().expect("open");
        self.app
            .tree_rows
            .rows(&o.doc, generation, &self.app.collapsed)
            .to_vec()
    }

    // ----------------------------------------------------------- entries

    /// A menu item of a track / folder / Conductor / empty-space menu.
    pub fn track_action(&mut self, a: TrackAction) {
        let rows = self.rows();
        if let Some(c) = self.app.apply_track_action(a, &rows) {
            self.command(c);
        }
    }

    /// Poll the real native job completion path (save/export/edit workers).
    pub fn poll_background_jobs(&mut self) {
        self.app.poll_job(&self.ctx, &self.frame);
    }

    /// A main-menu command (only when it is enabled, as the menu bar does).
    /// Returns whether it ran.
    pub fn command(&mut self, c: Command) -> bool {
        if !self.app.enabled(c) {
            return false;
        }
        self.app.execute(c, &self.ctx, &self.frame);
        true
    }

    /// Settings window: Reset section / Restore all defaults (B1-0058).
    pub fn settings_reset(&mut self, s: Option<super::settings_window::Section>) -> usize {
        self.app.reset_settings(s, &self.ctx, &self.frame)
    }

    /// One Settings window entry's action (B1-0058).
    pub fn settings_action(&mut self, a: super::settings_window::Action) {
        self.app.settings_action(a, &self.ctx, &self.frame);
    }

    /// One whole application frame (menus, toolbar, panels, the main view,
    /// open windows) on a window of `physical` pixels at the monitor scale
    /// `native` (B1-0062: the area in points shrinks with the UI scale the
    /// way egui-winit computes it). Returns the frame's shapes.
    pub fn app_frame(
        &mut self,
        physical: egui::Vec2,
        native: f32,
    ) -> Vec<egui::epaint::ClippedShape> {
        self.app_frame_events(physical, native, Vec::new())
    }

    /// [`Self::app_frame`] with input events (positions in points).
    pub fn app_frame_events(
        &mut self,
        physical: egui::Vec2,
        native: f32,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let ppp = native * self.ctx.zoom_factor();
        let mut events = events;
        events.insert(0, egui::Event::ModifiersChanged(self.modifiers));
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, physical / ppp)),
            events,
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(native);
        let (app, frame) = (&mut self.app, &mut self.frame);
        let mut out = self.ctx.run_ui(input, |ui| eframe::App::ui(app, ui, frame));
        out.textures_delta.clear();
        out.shapes
    }

    /// One frame of the Settings window (B1-0058).
    pub fn settings_frame(&mut self) {
        self.ctx.begin_pass(egui::RawInput::default());
        self.app.settings_window_ui(&self.ctx, &self.frame);
        let mut out = self.ctx.end_pass();
        out.textures_delta.clear();
    }

    /// A keyboard chord's command (the Track List's clip model is current,
    /// as every frame leaves it).
    pub fn chord(&mut self, c: Command) {
        self.refresh_track_list();
        self.app.run_chord(c, &self.ctx, &self.frame);
    }

    /// A key pressed (no modifiers) and handled by the shortcut dispatcher
    /// as one frame's input.
    pub fn press(&mut self, key: egui::Key) {
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        self.ctx.begin_pass(raw);
        self.app.shortcuts(&self.ctx, &self.frame);
        let mut out = self.ctx.end_pass();
        out.textures_delta.clear();
    }

    /// A key pressed (no modifiers) and handled by the transport dispatcher
    /// (rows read before any widget: Space, Tab, …) as one frame's input.
    pub fn transport_press(&mut self, key: egui::Key) {
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        self.ctx.begin_pass(raw);
        self.app.transport_shortcuts(&self.ctx, &self.frame);
        let mut out = self.ctx.end_pass();
        out.textures_delta.clear();
    }

    /// One frame with a key press handled by the shortcut dispatcher and
    /// then the Keyboard Shortcuts window (B1-0060).
    pub fn keybinds_frame(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        self.ctx.begin_pass(raw);
        self.app.shortcuts(&self.ctx, &self.frame);
        self.app.keybinds_window_ui(&self.ctx, &self.frame);
        let mut out = self.ctx.end_pass();
        out.textures_delta.clear();
    }

    /// B2-F29: input events handled by the shortcut dispatcher as one frame
    /// (the Track List's clip model current, as every frame leaves it); the
    /// frame's modifiers are the first key's.
    pub fn shortcut_events(&mut self, mut events: Vec<egui::Event>) {
        self.refresh_track_list();
        let mods = events
            .iter()
            .find_map(|e| match e {
                egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, egui::Event::ModifiersChanged(mods));
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        self.ctx.begin_pass(raw);
        self.app.shortcuts(&self.ctx, &self.frame);
        let mut out = self.ctx.end_pass();
        out.textures_delta.clear();
    }

    /// B2-F29: one key press (with `modifiers`) through the shortcut
    /// dispatcher.
    pub fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.shortcut_events(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    /// Builds the Track List's clip model for the document as a frame does.
    pub fn refresh_track_list(&mut self) {
        let generation = self.app.doc_generation();
        if let Some(doc) = self.app.open.as_ref().map(|o| o.doc.clone()) {
            self.app.track_list.model(&doc, generation);
        }
    }

    /// Runs the background job (and any job its result starts) to the end,
    /// as the frame loop polls it; the archive session is released the way
    /// each frame releases it (L-D02).
    pub fn wait_jobs(&mut self) {
        let start = std::time::Instant::now();
        loop {
            self.app.poll_job(&self.ctx, &self.frame);
            self.app.release_unused_archive();
            if self.app.job.is_none() {
                return;
            }
            assert!(start.elapsed().as_secs() < 120, "the job did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// A dialog outcome, as the dialog's buttons give it.
    pub fn dialog_outcome(&mut self, o: super::dialogs::DialogOutcome) {
        self.app.apply_dialog_outcome(o, &self.ctx, &self.frame);
    }

    /// A clip edit from the Track List.
    pub fn clip(&mut self, i: ClipIntent) {
        self.app.apply_clip_intent(i);
    }

    /// Confirms an open Delete Track / Delete Folder dialog as its button
    /// does; returns the roots it deleted (None: no such dialog).
    pub fn confirm_delete(&mut self) -> Option<Vec<usize>> {
        let Some(Dialog::DeleteTrack { roots, folder, .. }) = self.app.dialog.take() else {
            return None;
        };
        if folder == 0 {
            self.app.delete_tracks(&roots);
        } else {
            self.app.delete_folder_with_tracks(folder);
        }
        Some(roots)
    }

    /// The status bar's sentence.
    pub fn status(&self) -> String {
        self.app
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    }

    // ----------------------------------------------------------- contexts

    /// Clears every selection (tracks, clips, notes); the active track stays
    /// (PlumSpirit always has an active track or the Conductor).
    pub fn set_nothing(&mut self) {
        self.app.selected_tracks.clear();
        self.app.track_list.selected.clear();
        let n = self.doc().track_count();
        for i in 0..n {
            if let Some(t) = self.app.doc_mut().and_then(|d| d.track_mut(i)) {
                t.notes.clear_selection();
            }
        }
    }

    pub fn set_conductor(&mut self) {
        self.set_nothing();
        self.command(Command::SelectConductor);
        assert!(self.app.conductor_selected());
    }

    /// Track `i` clicked (a top-level track or a subtrack).
    pub fn set_track(&mut self, i: usize) {
        self.set_nothing();
        self.app.select_track(i);
    }

    /// Tracks `all` selected (Ctrl+click), `all[0]` active.
    pub fn set_tracks(&mut self, all: &[usize]) {
        self.set_track(all[0]);
        let ids: Vec<TrackId> = all.iter().map(|&i| self.id(i)).collect();
        self.app.selected_tracks.extend(ids);
    }

    /// Clips selected as clicks select them: the first one's track active.
    pub fn set_clips(&mut self, ids: &[ClipId]) {
        self.set_nothing();
        let track = self.doc().clips().placement(ids[0]).map(|p| p.track);
        if let Some(i) = track.and_then(|t| self.doc().tracks().iter().position(|x| x.id() == t)) {
            self.app.select_track(i);
        }
        self.app.track_list.selected.extend(ids.iter().copied());
    }

    /// Every note of track `i` selected (Select All on the active track).
    pub fn set_notes(&mut self, i: usize) {
        self.set_track(i);
        self.app.main_view = MainView::PianoRoll;
        assert!(self.command(Command::SelectAll), "Select All");
        assert!(self.app.selected_count() > 0);
    }
}
