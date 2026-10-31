//! Song-level editing: the Conductor (tempo, time signatures, markers), the
//! Pitch Bend / Pan / BPM lanes, the Erase tool, Quantize and the numeric Gate /
//! Velocity / Zoom fields. Every change goes through the undo history as one
//! operation; dialogs validate before anything changes.

use super::PlumSpiritApp;
use super::dialogs::{Dialog, parse_tick};
use crate::piano_roll::RollAction;
use crate::piano_roll::graph::GraphAction;
use crate::piano_roll::viewport;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::controllers;
use plumspirit_engine::editor::{self, parse_gate, parse_velocity};
use plumspirit_engine::history::{Operation, Outcome};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::conductor::{
    ConductorEdit, ConductorError, Interp, format_tempo, parse_denominator, parse_numerator,
};
use plumspirit_engine::model::{Bpm, NoteLoc, RankSetBuilder, TempoValue};

/// Selected notes of the active track: how many, and their gate / velocity when
/// all are the same (for the toolbar fields).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionSummary {
    pub count: usize,
    /// v0.8: selected notes that are muted (Mute / Unmute Notes availability).
    pub muted: usize,
    pub gate: Option<u64>,
    pub velocity: Option<u8>,
}

impl PlumSpiritApp {
    // ------------------------------------------------------------ helpers

    /// Applies a prepared Conductor edit as one undo step. Returns true when the
    /// song changed.
    pub(super) fn apply_conductor(
        &mut self,
        edit: Result<ConductorEdit, ConductorError>,
        label: &'static str,
    ) -> bool {
        match edit {
            Ok(edit) => {
                let changed = self
                    .apply_op(Operation::Conductor { edit, label })
                    .is_some_and(|o| o != Outcome::Unchanged);
                if changed {
                    self.meter = None;
                    self.set_status(format!("{label}."), StatusKind::Info);
                }
                changed
            }
            Err(e) => {
                self.set_status(e.to_string(), StatusKind::Error);
                false
            }
        }
    }

    /// Readable position for dialogs: "Bar 3 · Beat 2 (tick 1,920)".
    fn position_hint(&mut self, tick: u64) -> String {
        let bar = self
            .meter_map()
            .map(|m| m.describe(tick))
            .unwrap_or_default();
        format!("{bar} (tick {})", group_digits(tick))
    }

    /// The edit / playback position (where Song → Add … puts new entries).
    fn insert_position(&self) -> u64 {
        self.transport.position()
    }

    pub(super) fn selection_summary(&mut self) -> SelectionSummary {
        let Some(o) = &self.open else {
            return SelectionSummary::default();
        };
        if o.conductor_selected {
            return SelectionSummary::default();
        }
        let Ok(t) = plumspirit_engine::editor::note_view(&o.doc, self.edit_target()) else {
            return SelectionSummary::default();
        };
        let key = (t.host(), t.notes.revision());
        if let Some((k, s)) = self.summary
            && k == key
        {
            return s;
        }
        // L-S38: one columnar pass in the engine (fully selected chunks by
        // column), not a closure per selected note.
        let u = t.notes.selection_uniformity();
        let s = SelectionSummary {
            count: t.notes.selected_count(),
            muted: u.muted,
            gate: u.gate,
            velocity: u.velocity,
        };
        self.summary = Some((key, s));
        s
    }

    // ------------------------------------------------------ numeric fields

    pub(super) fn commit_gate_text(&mut self, text: &str) {
        match parse_gate(text) {
            Ok(g) => {
                let ti = self.edit_target();
                if let Some(Outcome::Notes(n)) = self.apply_op(Operation::SetDuration {
                    track: ti,
                    duration: g,
                }) {
                    self.set_status(
                        format!(
                            "Gate {} ticks set on {} note(s).",
                            group_digits(g),
                            group_digits(n as u64)
                        ),
                        StatusKind::Info,
                    );
                }
            }
            Err(e) => self.set_status(e.user_message(), StatusKind::Error),
        }
    }

    /// Link With Default Gate, no note selected: the typed length becomes the
    /// length of new notes (a user setting, not an edit).
    pub(super) fn commit_default_gate_text(&mut self, text: &str) {
        match parse_gate(text) {
            Ok(g) => {
                self.settings.default_gate = plumspirit_engine::editor::gate::DefaultGate::Ticks(g);
                self.save_settings();
                self.set_status(
                    format!(
                        "New notes: {} ticks (Custom). Existing notes are unchanged.",
                        group_digits(g)
                    ),
                    StatusKind::Info,
                );
            }
            Err(e) => self.set_status(e.user_message(), StatusKind::Error),
        }
    }

    pub(super) fn commit_velocity_text(&mut self, text: &str) {
        match parse_velocity(text) {
            Ok(v) => {
                let ti = self.edit_target();
                if let Some(Outcome::Notes(n)) = self.apply_op(Operation::SetVelocity {
                    track: ti,
                    velocity: v,
                }) {
                    self.set_status(
                        format!("Velocity {v} set on {} note(s).", group_digits(n as u64)),
                        StatusKind::Info,
                    );
                }
            }
            Err(e) => self.set_status(e.user_message(), StatusKind::Error),
        }
    }

    pub(super) fn commit_zoom_text(&mut self, text: &str, x_axis: bool) {
        let in_track_list =
            self.main_view == super::track_list::MainView::TrackList && self.open.is_some();
        let result = viewport::parse_percent(text).and_then(|p| {
            if in_track_list {
                match (x_axis, self.open.as_ref().map(|o| o.doc.clone())) {
                    (true, Some(doc)) => self.track_list.set_zoom_x_percent(p, &doc),
                    (false, _) => self.track_list.set_row_percent(p),
                    (true, None) => Ok(()),
                }
            } else if x_axis {
                let pos = self.transport.position();
                match self.open.as_ref().map(|o| o.doc.clone()) {
                    Some(doc) => self.roll.set_zoom_x_percent(p, &doc, pos),
                    None => Ok(()),
                }
            } else {
                self.roll.set_zoom_y_percent(p)
            }
        });
        if let Err(e) = result {
            self.set_status(e, StatusKind::Error);
        }
    }

    // ------------------------------------------------------------ erase

    pub(super) fn erase_notes(&mut self, locs: &[NoteLoc]) {
        let ti = self.edit_target();
        let Some(notes) = self.edit_notes() else {
            return;
        };
        let mut ranks: Vec<usize> = locs.iter().filter_map(|&l| notes.rank_of(l)).collect();
        ranks.sort_unstable();
        ranks.dedup();
        if ranks.is_empty() {
            return;
        }
        let mut b = RankSetBuilder::default();
        for r in &ranks {
            b.push(*r);
        }
        if let Some(Outcome::Notes(n)) = self.apply_op(Operation::EraseNotes {
            track: ti,
            ranks: b.finish(),
        }) {
            self.set_status(
                format!("Erased {} note(s).", group_digits(n as u64)),
                StatusKind::Info,
            );
        }
    }

    // ---------------------------------------------------- marker band / ruler

    /// Marker band and ruler requests from the piano roll.
    pub(super) fn apply_song_action(&mut self, a: RollAction) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let c = doc.conductor();
        match a {
            RollAction::MoveMarker { id, start, end } => {
                if let Some((_, m)) = c.marker_by_id(id) {
                    let title = m.title.clone();
                    let edit = c.prepare_set_marker(Some(id), start, end, &title);
                    drop(doc);
                    self.apply_conductor(edit, "Move Marker");
                }
            }
            RollAction::DeleteMarker(id) => {
                let edit = c.prepare_remove_markers(&[id]);
                drop(doc);
                self.apply_conductor(edit, "Delete Marker");
            }
            RollAction::EditMarker(id) => {
                if let Some((_, m)) = c.marker_by_id(id) {
                    let (title, start, end) = (m.title.clone(), m.start, m.end);
                    drop(doc);
                    let hint = self.position_hint(start);
                    self.dialog = Some(Dialog::Marker {
                        id: Some(id),
                        title,
                        chapter: end.is_some(),
                        start: start.to_string(),
                        end: end.map(|e| e.to_string()).unwrap_or_default(),
                        by_duration: false,
                        hint,
                        error: None,
                        focused: false,
                    });
                }
            }
            RollAction::AddMarkerAt(tick) => {
                drop(doc);
                self.open_marker_dialog(tick);
            }
            RollAction::EditSignature(id) => {
                if let Some((i, s)) = c.signature_by_id(id) {
                    let (tick, n, d) = (s.tick, s.numerator, s.denominator());
                    drop(doc);
                    let hint = self.position_hint(tick);
                    self.dialog = Some(Dialog::TimeSignature {
                        id: Some(id),
                        tick: tick.to_string(),
                        numerator: n.to_string(),
                        denominator: d.to_string(),
                        first: i == 0,
                        hint,
                        error: None,
                        focused: false,
                    });
                }
            }
            RollAction::DeleteSignature(id) => {
                let edit = c.prepare_remove_signatures(&[id]);
                drop(doc);
                self.apply_conductor(edit, "Delete Time Signature");
            }
            RollAction::AddSignatureAt(tick) => {
                drop(doc);
                self.open_signature_dialog(tick);
            }
            _ => {}
        }
    }

    pub(super) fn open_marker_dialog(&mut self, tick: u64) {
        let hint = self.position_hint(tick);
        self.dialog = Some(Dialog::Marker {
            id: None,
            title: String::new(),
            chapter: false,
            start: tick.to_string(),
            end: String::new(),
            by_duration: true,
            hint,
            error: None,
            focused: false,
        });
    }

    pub(super) fn open_signature_dialog(&mut self, tick: u64) {
        let Some(doc) = self.doc() else {
            return;
        };
        // Start from the signature in effect there; snap the position to the bar.
        let s = *doc.conductor().signature_at(tick);
        let at = self
            .meter_map()
            .map_or(tick, |m| m.bar_start_at_or_before(tick));
        let hint = self.position_hint(at);
        self.dialog = Some(Dialog::TimeSignature {
            id: None,
            tick: at.to_string(),
            numerator: s.numerator.to_string(),
            denominator: s.denominator().to_string(),
            first: false,
            hint,
            error: None,
            focused: false,
        });
    }

    pub(super) fn open_tempo_dialog(&mut self, id: Option<u32>, tick: u64) {
        let Some(doc) = self.doc() else {
            return;
        };
        let c = doc.conductor();
        let (tick, bpm, linear, first) = match id.and_then(|id| c.tempo_by_id(id)) {
            Some((i, p)) => (
                p.tick,
                format_tempo(p.value),
                p.interp == Interp::Linear,
                i == 0,
            ),
            None => {
                let p = c.tempo_points()[c.tempo_index_at(tick)];
                (tick, format_tempo(p.value), false, false)
            }
        };
        let hint = self.position_hint(tick);
        self.dialog = Some(Dialog::Tempo {
            id,
            tick: tick.to_string(),
            bpm,
            linear,
            first,
            hint,
            error: None,
            focused: false,
        });
    }

    pub(super) fn apply_events(
        &mut self,
        edit: Result<controllers::EventsEdit, editor::EditError>,
        label: &'static str,
    ) {
        match edit {
            Ok(edit) => {
                let changed = self
                    .apply_op(Operation::Events { edit, label })
                    .is_some_and(|o| o != Outcome::Unchanged);
                if changed {
                    self.set_status(format!("{label}."), StatusKind::Info);
                }
            }
            Err(e) => self.set_status(e.user_message(), StatusKind::Error),
        }
    }

    /// Delete key: the active graph's selected events (when a graph was used
    /// last), a selected marker or time signature; otherwise the selected notes.
    pub(super) fn delete_focused(&mut self) -> bool {
        let conductor = self.open.as_ref().is_some_and(|o| o.conductor_selected);
        if self.graph_focus && self.delete_graph_selection() {
            return true;
        }
        if let Some(id) = self.roll.selected_marker {
            self.apply_song_action(RollAction::DeleteMarker(id));
            self.roll.selected_marker = None;
            return true;
        }
        if let Some(id) = self.roll.selected_signature {
            let first = self
                .doc()
                .and_then(|d| d.conductor().time_signatures().first().map(|s| s.id));
            if first != Some(id) {
                self.apply_song_action(RollAction::DeleteSignature(id));
                self.roll.selected_signature = None;
                return true;
            }
        }
        if self.selected_count() == 0 && self.delete_graph_selection() {
            return true;
        }
        conductor
    }

    // ---------------------------------------------------------- quantize

    /// The stable Quantize command/shortcut now opens the one Quantizer tool.
    pub(super) fn open_quantize(&mut self) {
        self.open_tool("quantize");
    }

    // ------------------------------------------------------------ dialogs

    /// OK / Apply of the song dialogs. Returns an error to show in the dialog.
    pub(super) fn apply_song_dialog(&mut self, _ctx: &egui::Context) -> Result<(), String> {
        let Some(dialog) = self.dialog.clone() else {
            return Ok(());
        };
        match dialog {
            Dialog::Marker {
                id,
                title,
                chapter,
                start,
                end,
                by_duration,
                ..
            } => {
                let s = parse_tick(&start, "start")?;
                let e = if chapter {
                    let v = parse_tick(&end, if by_duration { "duration" } else { "end" })?;
                    Some(if by_duration {
                        s.checked_add(v)
                            .ok_or("The chapter would end beyond the largest possible tick.")?
                    } else {
                        v
                    })
                } else {
                    None
                };
                let edit = self
                    .doc()
                    .map(|d| d.conductor().prepare_set_marker(id, s, e, &title))
                    .ok_or_else(String::new)?;
                let edit = edit.map_err(|e| e.to_string())?;
                self.apply_conductor(
                    Ok(edit),
                    if id.is_some() {
                        "Edit Marker"
                    } else {
                        "Add Marker"
                    },
                );
                Ok(())
            }
            Dialog::TimeSignature {
                id,
                tick,
                numerator,
                denominator,
                first,
                ..
            } => {
                let n = parse_numerator(&numerator).map_err(|e| e.to_string())?;
                let d = parse_denominator(&denominator).map_err(|e| e.to_string())?;
                let t = if first {
                    0
                } else {
                    parse_tick(&tick, "position")?
                };
                let edit = self
                    .doc()
                    .map(|doc| doc.conductor().prepare_set_signature(id, t, n, d))
                    .ok_or_else(String::new)?
                    .map_err(|e| e.to_string())?;
                self.apply_conductor(
                    Ok(edit),
                    if id.is_some() {
                        "Edit Time Signature"
                    } else {
                        "Add Time Signature"
                    },
                );
                Ok(())
            }
            Dialog::Tempo {
                id,
                tick,
                bpm,
                linear,
                first,
                ..
            } => {
                // B1-0101: a tempo a MIDI file can store.
                let b = Bpm::parse_midi(&bpm)?;
                let t = if first {
                    0
                } else {
                    parse_tick(&tick, "position")?
                };
                let interp = if linear { Interp::Linear } else { Interp::Step };
                let edit = self
                    .doc()
                    .map(|doc| {
                        doc.conductor()
                            .prepare_set_tempo(id, t, TempoValue::Bpm(b), interp)
                    })
                    .ok_or_else(String::new)?
                    .map_err(|e| e.to_string())?;
                self.apply_conductor(
                    Ok(edit),
                    if id.is_some() {
                        "Change Tempo"
                    } else {
                        "Add Tempo Point"
                    },
                );
                Ok(())
            }
            Dialog::Controller {
                kind,
                channel,
                index,
                tick,
                value,
                ..
            } => {
                let v = kind.parse(&value).map_err(|e| e.user_message())?;
                let t = parse_tick(&tick, "position")?;
                let ti = self.edit_target();
                let edit = self
                    .doc()
                    .and_then(|d| editor::note_view(d, ti).ok())
                    .map(|tr| controllers::prepare_set(tr, kind, channel, index, t, v))
                    .ok_or_else(String::new)?
                    .map_err(|e| e.user_message())?;
                self.apply_op(Operation::Events {
                    edit,
                    label: kind.label(),
                });
                Ok(())
            }
            Dialog::MasterVolume {
                id, tick, value, ..
            } => {
                let v = plumspirit_engine::model::conductor::parse_master_volume(&value)
                    .map_err(|e| e.to_string())?;
                let t = parse_tick(&tick, "position")?;
                let edit = self
                    .doc()
                    .map(|d| d.conductor().prepare_set_master(id, t, v))
                    .ok_or_else(String::new)?
                    .map_err(|e| e.to_string())?;
                self.apply_conductor(
                    Ok(edit),
                    if id.is_some() {
                        "Change Master Volume"
                    } else {
                        "Add Master Volume"
                    },
                );
                Ok(())
            }
            Dialog::TrackProperties(draft, _) => self.apply_track_properties(&draft),
            Dialog::SelectRange(draft, _) => self.apply_select_range(&draft),
            Dialog::CurveFunctions(draft) => {
                draft.validate()?;
                let chosen = draft.presets.get(draft.selected).map(|p| p.name.clone());
                self.settings.curve_presets = draft.presets.clone();
                if let Some(n) = chosen {
                    self.settings.curve.custom = n;
                }
                self.settings.graph_tool = crate::piano_roll::graph::GraphTool::Curve(
                    plumspirit_engine::editor::curves::Shape::Custom,
                );
                self.custom_expr = None;
                self.save_settings();
                self.set_status(
                    format!(
                        "Custom function “{}” selected for the f(t) tool.",
                        self.settings.curve.custom
                    ),
                    StatusKind::Info,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Delete in the song dialogs.
    pub(super) fn delete_from_dialog(&mut self) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        match dialog {
            Dialog::Marker { id: Some(id), .. } => {
                self.apply_song_action(RollAction::DeleteMarker(id));
            }
            Dialog::TimeSignature { id: Some(id), .. } => {
                self.apply_song_action(RollAction::DeleteSignature(id));
            }
            Dialog::Tempo { id: Some(id), .. } => {
                let slot = self.active_graph;
                self.apply_graph_action(slot, GraphAction::DeleteTempo(vec![id]));
            }
            Dialog::Controller {
                index: Some(index), ..
            } => {
                let slot = self.active_graph;
                self.apply_graph_action(slot, GraphAction::DeleteControllers(vec![index]));
            }
            Dialog::MasterVolume { id: Some(id), .. } => {
                let slot = self.active_graph;
                self.apply_graph_action(slot, GraphAction::DeleteMaster(vec![id]));
            }
            _ => {}
        }
    }

    /// Song → Add … at the edit / playback position.
    /// The Conductor menu's Add Tempo Change / Time Signature / Marker
    /// (V09-S10): at `at` (the Track List's Conductor lane) or at the edit
    /// position; nothing when the Conductor's timing / markers are fixed.
    pub(super) fn conductor_add(
        &mut self,
        what: crate::ui::track_panel::ConductorAdd,
        at: Option<u64>,
    ) {
        use crate::ui::track_panel::ConductorAdd;
        let Some(c) = self.doc().map(|d| d.conductor().clone()) else {
            return;
        };
        let (what, allowed) = match what {
            ConductorAdd::Tempo => (SongAdd::Tempo, c.timing_editable()),
            ConductorAdd::TimeSignature => (SongAdd::TimeSignature, c.timing_editable()),
            ConductorAdd::Marker => (SongAdd::Marker, c.markers_editable()),
        };
        if !allowed {
            return;
        }
        match (at, what) {
            (None, w) => self.add_at_position(w),
            (Some(t), SongAdd::Marker) => self.open_marker_dialog(t),
            (Some(t), SongAdd::TimeSignature) => self.open_signature_dialog(t),
            (Some(t), SongAdd::Tempo) => self.open_tempo_dialog(None, t),
        }
    }

    pub(super) fn add_at_position(&mut self, what: SongAdd) {
        let tick = self.insert_position();
        match what {
            SongAdd::Marker => self.open_marker_dialog(tick),
            SongAdd::TimeSignature => self.open_signature_dialog(tick),
            SongAdd::Tempo => self.open_tempo_dialog(None, tick),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongAdd {
    Marker,
    TimeSignature,
    Tempo,
}
