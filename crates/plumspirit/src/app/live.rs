//! v0.6 live features in the frame loop: the analysis index, live keyboard
//! colours, the Progressor overlay and note audition.
//!
//! Everything here reads the committed document (never a Preview) and the
//! transport position; nothing walks all notes per frame.

use super::analysis::{NotesKey, Peaks};
use super::keys::{self, KeysKey};
use super::progressor::{self, Readout, ScopeSetting, Template, Token, WindowOutcome};
use super::tasks::TaskOutcome;
use super::{PlumSpiritApp, format_clock, live_bpm};
use crate::audio::TransportState;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::analysis::Scope;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::NoteAttrs;
use std::time::Duration;

/// Repaint interval while a background analysis build runs.
const BUILD_POLL: Duration = Duration::from_millis(100);

impl PlumSpiritApp {
    /// Live keyboard colours are shown: the feature is on and the transport
    /// has a position to describe (playing, starting or paused).
    pub(super) fn key_colors_on(&self) -> bool {
        self.settings.live_keys
            && self.open.is_some()
            && matches!(
                self.transport.state(),
                TransportState::Playing | TransportState::Seeking | TransportState::Paused
            )
    }

    /// L-G02: the tracks whose sounding notes color the keys — what the
    /// Piano Roll shows (the active track and its subtracks, plus the Onion
    /// ghosts while the Onion is on), or the chosen scope.
    pub(super) fn key_tracks(
        &self,
        doc: &plumspirit_engine::model::PlumDocument,
    ) -> Option<std::collections::BTreeSet<plumspirit_engine::model::TrackId>> {
        if self.settings.key_follow_roll {
            let active = (!self.conductor_selected()).then(|| self.active_track());
            Some(keys::shown_tracks(doc, active, &self.onion_ghosts()))
        } else {
            keys::scope_tracks(doc, self.settings.key_scope, self.active_track())
        }
    }

    /// The analysis scope of a scope setting for the open document.
    fn live_scope(&self, s: ScopeSetting) -> Scope {
        self.doc()
            .and_then(|d| keys::scope_tracks(d, s, self.active_track()))
            .map_or(Scope::Project, Scope::Tracks)
    }

    /// Per frame, before the panels: audition safety, the analysis index, the
    /// keyboard tracker and the Progressor Settings window.
    pub(super) fn update_live(&mut self, ctx: &egui::Context, dark: bool) {
        self.poll_project_releases(ctx);
        self.remember_onion();
        // Audition safety: a held key ends with the pointer, focus or a dialog.
        let focused = ctx.input(|i| i.focused);
        let lost_focus = self.audition_focus && !focused;
        self.audition_focus = focused;
        let held_lost = self.audition_held.is_some()
            && (!ctx.input(|i| i.pointer.primary_down()) || !self.roll.audition_key_held());
        if lost_focus || held_lost || self.dialog.is_some() || !self.settings.audition {
            self.audition_release_all();
        }
        if self.audition_pending.is_some() {
            self.audition_pending();
            ctx.request_repaint_after(BUILD_POLL);
        }

        let show_keys = self.key_colors_on();
        if (self.settings.progressor.enabled
            || show_keys
            || self.player_shown()
            || self.inspector_shown())
            && let Some(o) = &self.open
        {
            let doc = o.doc.clone();
            let generation = o.generation;
            if let Some(f) = self.analysis.sync(&doc, generation) {
                self.tasks.record(
                    format!("Analyzed {} notes", group_digits(f.notes)),
                    f.elapsed,
                    TaskOutcome::Done,
                );
            }
            if self.analysis.busy() || self.analysis.profile_pending() {
                ctx.request_repaint_after(BUILD_POLL);
            }
            if show_keys {
                let key = KeysKey {
                    notes: NotesKey::of(&doc, generation),
                    dark,
                    tracks: self.key_tracks(&doc),
                    // The roll draws the active track in subchannel colours.
                    active: (!self.conductor_selected())
                        .then(|| doc.track(self.active_track()).map(|t| t.id()))
                        .flatten(),
                    palette: crate::ui::theme::note_palette(ctx),
                    played: false,
                };
                let index = self.analysis.index().cloned();
                self.keys.update(
                    &doc,
                    self.analysis.corrections(),
                    index.as_deref(),
                    self.transport.position(),
                    key,
                );
            }
            // v0.9 (V09-S17): the snapshot the Player draws (taken on a
            // worker, P5). Its keyboard is read from the snapshot at the hit
            // line when drawn (v1.0-beta1 B1-0111, `player_lit`).
            if self.player_shown() && self.player.sync(&doc) {
                ctx.request_repaint_after(super::player_view::SNAPSHOT_POLL);
            }
        }
        if !show_keys {
            self.keys.clear();
        }
        // v0.9 (V09-S18): the MIDI Inspector (`docs/INSPECTOR.md`).
        self.update_inspector_window(ctx);
        // v1.0-Launch L-E01: the selection charts.
        self.update_selection_charts_window(ctx);

        self.update_player_window(ctx);
        if self.progressor_window.is_some() {
            match progressor::settings_window(ctx, &mut self.settings.progressor) {
                WindowOutcome::Open => {}
                WindowOutcome::Ok => {
                    self.progressor_window = None;
                    self.save_settings();
                }
                WindowOutcome::Cancel => {
                    if let Some(w) = self.progressor_window.take() {
                        self.settings.progressor = w.original;
                    }
                }
            }
        }
        if self.selection_window.is_some() {
            let ps = &mut self.settings.progressor;
            match super::selection_stats::settings_window(
                ctx,
                &mut ps.selection_template,
                &mut self.settings.selection_analytics,
                &mut ps.selection_collapsed,
            ) {
                WindowOutcome::Open => {}
                WindowOutcome::Ok => {
                    self.selection_window = None;
                    self.save_settings();
                }
                WindowOutcome::Cancel => {
                    if let Some((t, on, compact)) = self.selection_window.take() {
                        self.settings.progressor.selection_template = t;
                        self.settings.selection_analytics = on;
                        self.settings.progressor.selection_collapsed = compact;
                    }
                }
            }
        }
    }

    /// The Progressor text for the current position and whether it describes
    /// an earlier version of the notes (an update is running). `None`: off, no
    /// document, or nothing analysed yet.
    pub(super) fn progressor_text(&mut self) -> Option<(String, bool)> {
        if !self.settings.progressor.enabled || self.open.is_none() {
            return None;
        }
        let src = &self.settings.progressor.template;
        if self
            .progressor_template
            .as_ref()
            .is_none_or(|(s, _)| s != src)
        {
            self.progressor_template = Some((src.clone(), Template::parse(src)));
        }
        let (window, so_far, needs_measure, needs_bpm) = {
            let t = &self.progressor_template.as_ref()?.1;
            (
                t.uses(|k| k == Token::PeakPoly1s),
                t.uses(Token::needs_peaks),
                t.uses(|k| k == Token::Measure),
                t.uses(|k| k == Token::Bpm),
            )
        };
        let scope = self.live_scope(self.settings.progressor.scope);
        // Stopped: the transport cursor; playing / paused: the playhead.
        let tick = self.transport.position();
        let window_ns = self.settings.progressor.window_ns();
        let snap = self
            .analysis
            .query(tick, &scope, window_ns, Peaks { window, so_far })?;
        let m = &snap.metrics;
        let measure = if needs_measure {
            self.meter_map()
                .map_or_else(|| group_digits(m.tick), |mm| mm.format_position(m.tick))
        } else {
            String::new()
        };
        let bpm = match (needs_bpm, self.doc()) {
            (true, Some(d)) => live_bpm(d, m.tick, self.settings.speed).0,
            _ => String::new(),
        };
        let readout = Readout {
            notes: m.notes_passed,
            total: m.total_notes,
            nps: m.nps,
            poly: m.poly,
            pps: m.pps,
            peak_nps: snap.peak_nps,
            peak_poly: snap.peak_poly,
            peak_poly_1s: snap.peak_poly_1s,
            time: format_clock(m.nanos as f64 / 1e9),
            tick: m.tick,
            measure,
            bpm,
            scope: self.settings.progressor.scope.short(),
        };
        let t = &self.progressor_template.as_ref()?.1;
        Some((
            progressor::render_text(t, &readout, &self.settings.progressor),
            snap.stale,
        ))
    }

    // ------------------------------------------------------------- audition

    /// Ends every audition note (focus lost, dialog, project change, off).
    pub(super) fn audition_release_all(&mut self) {
        self.audition_held = None;
        self.audition_pending = None;
        self.transport.audition_all_off();
    }

    /// An audition asked for while the SoundFont was loading: played once it
    /// is ready (a key only while it is still held).
    fn audition_pending(&mut self) {
        let Some((pitch, tick, hold)) = self.audition_pending else {
            return;
        };
        if self.transport.loading_soundfont().is_some() {
            return;
        }
        self.audition_pending = None;
        match hold {
            Some(h) => {
                self.audition_start(pitch, tick, Some(h));
            }
            None if self.roll.audition_key_held() => {
                let p = self.roll.audition_pitch().unwrap_or(pitch);
                if let Some(ch) = self.audition_start(p, self.transport.position(), None) {
                    self.audition_held = Some((ch, p));
                }
            }
            None => {}
        }
    }

    /// A piano key was pressed (`Some`), glided to another key, or released.
    pub(super) fn audition_key(&mut self, pitch: Option<u8>) {
        if let Some((ch, key)) = self.audition_held {
            if Some(key) == pitch {
                return;
            }
            self.audition_held = None;
            self.transport.audition_off(ch, key);
        }
        let Some(p) = pitch else {
            return;
        };
        let tick = self.transport.position();
        if let Some(ch) = self.audition_start(p, tick, None) {
            self.audition_held = Some((ch, p));
        }
    }

    /// A note is being placed: a short sound of its pitch.
    pub(super) fn audition_note(&mut self, pitch: u8, tick: u64) {
        let hold = Duration::from_millis(u64::from(self.settings.audition_ms));
        self.audition_start(pitch, tick, Some(hold));
    }

    /// Starts an audition note on the channel new notes of the active track /
    /// subchannel get, with the instrument at `tick`. Returns that channel.
    /// The SoundFont is loaded on demand (like the first Play); the note then
    /// sounds when it is ready (see [`PlumSpiritApp::audition_pending`]).
    fn audition_start(&mut self, pitch: u8, tick: u64, hold: Option<Duration>) -> Option<u8> {
        if !self.settings.audition {
            return None;
        }
        self.audition_pending = None;
        if let Some(p) = self.effective_soundfont()
            && !self.transport.has_soundfont_for(&p)
        {
            self.transport.load_soundfont(p);
        }
        if self.transport.loading_soundfont().is_some() {
            // Sounds when the SoundFont is ready (a held key only if still held).
            self.audition_pending = Some((pitch, tick, hold));
            return None;
        }
        let ti = self.active_track();
        let doc = self.open.as_ref()?.doc.clone();
        let generation = self.doc_generation();
        let channel = match self.active_sub_of(ti) {
            Some(sc) => sc.midi_channel,
            None => doc.track(ti).map_or(0, |t| t.props().default_channel),
        };
        let velocity = NoteAttrs::default_for_pitch(pitch).map_or(100, NoteAttrs::velocity);
        match self
            .transport
            .audition_on(&doc, generation, ti, channel, pitch, velocity, tick, hold)
        {
            Ok(()) => Some(channel),
            Err(e) => {
                if !self.audition_warned {
                    self.audition_warned = true;
                    self.set_status(e, StatusKind::Warning);
                }
                None
            }
        }
    }
}
