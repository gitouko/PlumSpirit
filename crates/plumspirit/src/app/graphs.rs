//! The two Event Graphs: which target each slot shows, their edits, curves,
//! the graph selection (Delete / Copy / Paste) and the compact event list.
//!
//! Graph slots (target, channel, visibility) are project settings saved in the
//! `.lup` (outside undo, like Onion Skin); everything a graph *edits* goes
//! through the undo history as one operation per gesture.

use super::PlumSpiritApp;
use super::curves::{CurveContext, curve_label};
use super::dialogs::Dialog;
use crate::piano_roll::graph::{GraphAction, GraphTool};
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::controllers::{
    self, ControllerKind, EventClip, WritePolicy, collect_points, graph_controller,
};
use plumspirit_engine::editor::curves::event_budget;
use plumspirit_engine::editor::expr::Expr;
use plumspirit_engine::history::Outcome;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::conductor::{Interp, format_master_volume, format_tempo};
use plumspirit_engine::model::{GraphSlot, GraphTarget, TempoValue};

/// Copied graph events (Ctrl+C in a graph): values relative to the first tick.
#[derive(Clone, Debug)]
pub enum EventClipboard {
    Controller(EventClip),
    Master(Vec<(u64, u16)>),
}

impl PlumSpiritApp {
    /// What the graph tool row asked for.
    pub(super) fn apply_graph_bar(&mut self, out: crate::ui::graph_bar::GraphBarOutput) {
        if let Some(t) = out.tool {
            self.settings.graph_tool = t;
            self.graphs.cancel();
            self.save_settings();
        }
        if out.settings_changed {
            // Saved a moment later, so dragging a value does not write the file
            // every frame.
            self.settings_save_at =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(800));
            self.custom_expr = None;
        }
        if out.open_functions {
            self.dialog = Some(Dialog::CurveFunctions(Box::new(
                super::functions::FunctionsDraft::new(
                    &self.settings.curve_presets,
                    &self.settings.curve.custom,
                ),
            )));
        }
        if out.toggle_list {
            self.event_list_open = !self.event_list_open;
        }
        if out.toggle_graph2 {
            let v = !self.graph_slot(1).visible;
            self.update_graph_slot(1, |s| s.visible = v);
            self.activate_graph(if v { 1 } else { 0 });
        }
        if out.apply_anchors {
            let slot = self.active_graph;
            if let Some(a) = self.graphs.views[slot].pending_anchors() {
                self.graphs.views[slot].clear_anchors();
                self.apply_curve(slot, Some(a), None);
            }
        }
        if out.cancel_anchors {
            self.graphs.views[self.active_graph].clear_anchors();
        }
    }

    /// A graph slot as saved in the project (defaults without a project).
    pub(super) fn graph_slot(&self, slot: usize) -> GraphSlot {
        self.doc()
            .map(|d| d.project().graphs.slots[slot.min(1)])
            .unwrap_or_else(|| plumspirit_engine::model::GraphConfig::default().slots[slot.min(1)])
    }

    /// What slot `slot` shows right now: Graph 1 shows the BPM while the
    /// Conductor is selected and its own target belongs to a track (v0.3
    /// behaviour).
    pub(super) fn shown_target(&self, slot: usize) -> GraphTarget {
        let s = self.graph_slot(slot);
        if slot == 0 && self.conductor_selected() && !controllers::graph_is_global(s.target) {
            GraphTarget::Bpm
        } else {
            s.target
        }
    }

    pub(super) fn update_graph_slot(&mut self, slot: usize, f: impl FnOnce(&mut GraphSlot)) {
        if let Some(d) = self.doc_mut() {
            f(&mut d.project_mut().graphs.slots[slot.min(1)]);
        }
        self.graphs.views[slot.min(1)].cancel();
        self.graphs.views[slot.min(1)].clear_point_selection();
    }

    /// Makes `slot` the graph that receives edits.
    pub(super) fn activate_graph(&mut self, slot: usize) {
        if self.active_graph != slot {
            self.graphs.views[self.active_graph].clear_anchors();
            self.active_graph = slot;
        }
    }

    /// The parsed custom function chosen in the curve settings (cached by text).
    pub(super) fn refresh_custom_expr(&mut self) {
        let name = self.settings.curve.custom.clone();
        let text = self
            .settings
            .curve_presets
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.expr.clone());
        let Some(text) = text else {
            self.custom_expr = None;
            return;
        };
        if self.custom_expr.as_ref().is_some_and(|(t, _)| *t == text) {
            return;
        }
        let parsed = Expr::parse(&text).map_err(|e| e.to_string());
        self.custom_expr = Some((text, parsed));
    }

    fn event_budget(&self) -> usize {
        event_budget(
            self.open
                .as_ref()
                .map_or(64 << 20, |o| o.history.limits().max_bytes),
        )
    }

    /// Applies a graph edit from slot `slot`.
    pub(super) fn apply_graph_action(&mut self, slot: usize, a: GraphAction) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        // The notes and events edited (an opened clip's content; V09-S11).
        let ti = self.edit_target();
        let view = &mut self.graphs.views[slot.min(1)];
        match a {
            GraphAction::SetController {
                kind,
                channel,
                target,
                tick,
                value,
            } => {
                let Ok(t) = plumspirit_engine::editor::note_view(&doc, ti) else {
                    return;
                };
                let edit = controllers::prepare_set(t, kind, channel, target, tick, value);
                drop(doc);
                self.apply_events(edit, kind.label());
            }
            GraphAction::MoveControllers {
                kind,
                channel,
                indices,
                dtick,
                dvalue,
            } => {
                let Ok(t) = plumspirit_engine::editor::note_view(&doc, ti) else {
                    return;
                };
                let edit = controllers::prepare_move(t, kind, channel, &indices, dtick, dvalue);
                drop(doc);
                view.clear_point_selection();
                self.apply_events(edit, "Move Events");
            }
            GraphAction::DeleteControllers(indices) => {
                let Ok(t) = plumspirit_engine::editor::note_view(&doc, ti) else {
                    return;
                };
                let edit = controllers::prepare_delete(t, &indices);
                drop(doc);
                view.clear_point_selection();
                self.apply_events(
                    edit,
                    if indices.len() > 1 {
                        "Delete Events"
                    } else {
                        "Delete Event"
                    },
                );
            }
            GraphAction::EditController {
                kind,
                channel,
                index,
                tick,
            } => {
                let value = index
                    .and_then(|i| {
                        plumspirit_engine::editor::note_view(&doc, ti)
                            .ok()?
                            .events()
                            .events()
                            .get(i)
                            .copied()
                    })
                    .and_then(|e| kind.value_of(&e, channel))
                    .unwrap_or(if kind.is_channel_mode() {
                        kind.command_value()
                    } else {
                        kind.center()
                    });
                drop(doc);
                self.dialog = Some(Dialog::Controller {
                    kind,
                    channel,
                    index,
                    tick: tick.to_string(),
                    value: match kind {
                        ControllerKind::PitchBend => {
                            (i32::from(value) - i32::from(kind.center())).to_string()
                        }
                        ControllerKind::Cc(_) => value.to_string(),
                    },
                    error: None,
                    focused: false,
                });
            }
            GraphAction::SetTempo {
                target,
                tick,
                bpm,
                interp,
            } => {
                let edit =
                    doc.conductor()
                        .prepare_set_tempo(target, tick, TempoValue::Bpm(bpm), interp);
                drop(doc);
                self.apply_conductor(
                    edit,
                    if target.is_some() {
                        "Change Tempo"
                    } else {
                        "Add Tempo Point"
                    },
                );
            }
            GraphAction::SetInterp { id, interp } => {
                let edit = doc.conductor().prepare_set_interp(id, interp);
                drop(doc);
                self.apply_conductor(
                    edit,
                    match interp {
                        Interp::Step => "Tempo Step",
                        Interp::Linear => "Tempo Ramp",
                    },
                );
            }
            GraphAction::DeleteTempo(ids) => {
                let edit = doc.conductor().prepare_remove_tempo(&ids);
                drop(doc);
                if self.apply_conductor(
                    edit,
                    if ids.len() > 1 {
                        "Delete Tempo Points"
                    } else {
                        "Delete Tempo Point"
                    },
                ) {
                    self.graphs.clear_point_selection();
                }
            }
            GraphAction::EditTempo { id, tick } => {
                drop(doc);
                self.open_tempo_dialog(id, tick);
            }
            GraphAction::SetMaster {
                target,
                tick,
                value,
            } => {
                let edit = doc.conductor().prepare_set_master(target, tick, value);
                drop(doc);
                self.apply_conductor(
                    edit,
                    if target.is_some() {
                        "Change Master Volume"
                    } else {
                        "Add Master Volume"
                    },
                );
            }
            GraphAction::DeleteMaster(ids) => {
                let edit = doc.conductor().prepare_remove_master(&ids);
                drop(doc);
                if self.apply_conductor(edit, "Delete Master Volume") {
                    self.graphs.clear_point_selection();
                }
            }
            GraphAction::EditMaster { id, tick } => {
                let c = doc.conductor();
                let (tick, value) = match id.and_then(|i| c.master_by_id(i)) {
                    Some((_, p)) => (p.tick, p.value),
                    None => (tick, c.master_at(tick)),
                };
                drop(doc);
                self.dialog = Some(Dialog::MasterVolume {
                    id,
                    tick: tick.to_string(),
                    value: format_master_volume(value)
                        .split(' ')
                        .next()
                        .unwrap_or("100%")
                        .to_owned(),
                    error: None,
                    focused: false,
                });
            }
            GraphAction::Curve { from, to } => {
                drop(doc);
                self.apply_curve(slot, Some((from, to)), None);
            }
            GraphAction::FreeHand { samples } => {
                drop(doc);
                self.apply_curve(slot, None, Some(samples));
            }
        }
    }

    /// Generates and applies a curve (or free-hand stroke) on slot `slot` as
    /// one undo step.
    pub(super) fn apply_curve(
        &mut self,
        slot: usize,
        anchors: Option<((u64, f64), (u64, f64))>,
        stroke: Option<Vec<(u64, f64)>>,
    ) {
        self.refresh_custom_expr();
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let target = self.shown_target(slot);
        let channel = self.graph_slot(slot).channel;
        let ti = self.edit_target();
        let Some(meter) = self.meter_map().cloned() else {
            return;
        };
        let snap = self.roll.snap.unit_ticks(doc.division());
        let budget = self.event_budget();
        let tool = self.settings.graph_tool;
        let conductor = self.conductor_selected();
        let result = {
            let ctx = CurveContext {
                doc: &doc,
                track: (!conductor)
                    .then(|| plumspirit_engine::editor::note_view(&doc, ti).ok())
                    .flatten(),
                track_index: ti,
                channel,
                meter: &meter,
                settings: &self.settings.curve,
                custom: self.custom_expr.as_ref().map(|c| &c.1),
                snap,
            };
            let label = curve_label(tool.shape().filter(|_| stroke.is_none()));
            let generated = match (&anchors, &stroke) {
                (Some((from, to)), _) => ctx.generate(
                    target,
                    tool.shape()
                        .unwrap_or(plumspirit_engine::editor::curves::Shape::Line),
                    *from,
                    *to,
                    budget,
                ),
                (None, Some(samples)) => ctx.generate_freehand(target, samples, budget),
                (None, None) => return,
            };
            generated.and_then(|g| {
                let n = g.len();
                ctx.operation(g, label, budget).map(|op| (op, n, label))
            })
        };
        drop(doc);
        match result {
            Ok((op, n, label)) => {
                if self.apply_op(op).is_some_and(|o| o != Outcome::Unchanged) {
                    let policy = match self.settings.curve.policy {
                        WritePolicy::Replace => "replacing the events in its range",
                        WritePolicy::Merge => "merged with the existing events",
                    };
                    self.set_status(
                        format!(
                            "{label}: {} {} on {}{}.",
                            group_digits(n as u64),
                            if target == GraphTarget::Velocity {
                                "note velocities set"
                            } else {
                                "events written"
                            },
                            controllers::graph_label(target),
                            if target == GraphTarget::Velocity {
                                String::new()
                            } else {
                                format!(", {policy}")
                            }
                        ),
                        StatusKind::Info,
                    );
                    if tool == GraphTool::Curve(plumspirit_engine::editor::curves::Shape::Random)
                        && self.settings.curve.advance_seed
                    {
                        self.settings.curve.seed = self.settings.curve.seed.wrapping_add(1);
                        self.save_settings();
                    }
                }
            }
            Err(e) => self.set_status(e.message, StatusKind::Error),
        }
    }

    /// Delete in the active graph: its selected events. Returns true when it
    /// acted.
    pub(super) fn delete_graph_selection(&mut self) -> bool {
        let slot = self.active_graph;
        let target = self.shown_target(slot);
        let view = &self.graphs.views[slot];
        match target {
            GraphTarget::Bpm => {
                let first = self
                    .doc()
                    .and_then(|d| d.conductor().tempo_points().first().map(|p| p.id));
                let ids: Vec<u32> = view
                    .selected_tempo()
                    .into_iter()
                    .filter(|i| Some(*i) != first)
                    .collect();
                if ids.is_empty() {
                    return false;
                }
                self.apply_graph_action(slot, GraphAction::DeleteTempo(ids));
            }
            GraphTarget::MasterVolume => {
                let ids = view.selected_master();
                if ids.is_empty() {
                    return false;
                }
                self.apply_graph_action(slot, GraphAction::DeleteMaster(ids));
            }
            GraphTarget::Velocity => return false,
            _ => {
                let ids = view.selected_controllers();
                if ids.is_empty() {
                    return false;
                }
                self.apply_graph_action(slot, GraphAction::DeleteControllers(ids));
            }
        }
        true
    }

    /// Ctrl+C in a graph: copies its selected events. Returns true when it acted.
    pub(super) fn copy_graph_selection(&mut self) -> bool {
        let slot = self.active_graph;
        let target = self.shown_target(slot);
        let channel = self.graph_slot(slot).channel;
        let view = &self.graphs.views[slot];
        match target {
            GraphTarget::MasterVolume => {
                let ids = view.selected_master();
                let Some(d) = self.doc() else {
                    return false;
                };
                let mut pts: Vec<(u64, u16)> = d
                    .conductor()
                    .master_volume()
                    .iter()
                    .filter(|p| ids.contains(&p.id))
                    .map(|p| (p.tick, p.value))
                    .collect();
                if pts.is_empty() {
                    return false;
                }
                let t0 = pts[0].0;
                for p in &mut pts {
                    p.0 -= t0;
                }
                let n = pts.len();
                self.event_clipboard = Some(EventClipboard::Master(pts));
                self.set_status(
                    format!("Copied {n} Master Volume point(s)."),
                    StatusKind::Info,
                );
                true
            }
            t => {
                let Some(kind) = graph_controller(t) else {
                    return false;
                };
                let ids = view.selected_controllers();
                if ids.is_empty() {
                    return false;
                }
                let ti = self.edit_target();
                let Some(track) = self
                    .doc()
                    .and_then(|d| plumspirit_engine::editor::note_view(d, ti).ok())
                else {
                    return false;
                };
                let clip = EventClip::copy(track, kind, channel, &ids);
                if clip.is_empty() {
                    return false;
                }
                let n = clip.points.len();
                self.event_clipboard = Some(EventClipboard::Controller(clip));
                self.set_status(
                    format!("Copied {n} {} event(s).", kind.label()),
                    StatusKind::Info,
                );
                true
            }
        }
    }

    /// Ctrl+V in a graph: pastes copied events at the edit position onto the
    /// active graph's target (same kind of events only). Returns true when it
    /// acted.
    pub(super) fn paste_graph_events(&mut self) -> bool {
        let Some(clip) = self.event_clipboard.clone() else {
            return false;
        };
        let slot = self.active_graph;
        let target = self.shown_target(slot);
        let channel = self.graph_slot(slot).channel;
        // Master Volume is song time; a track's (or an opened clip's)
        // controllers are on the roll's axis (V09-S11).
        let at = match target {
            GraphTarget::MasterVolume => self.roll.cursor_tick,
            _ => self.edit_cursor(),
        };
        let budget = self.event_budget();
        match (clip, target) {
            (EventClipboard::Master(pts), GraphTarget::MasterVolume) => {
                let mut placed = Vec::with_capacity(pts.len());
                for (t, v) in pts {
                    match at.checked_add(t) {
                        Some(t) => placed.push((t, v)),
                        None => {
                            self.set_status(
                                "The paste would go beyond the largest tick.",
                                StatusKind::Error,
                            );
                            return true;
                        }
                    }
                }
                let edit = self
                    .doc()
                    .map(|d| d.conductor().prepare_master_points(&placed, false));
                if let Some(edit) = edit {
                    self.apply_conductor(edit, "Paste Master Volume");
                }
                true
            }
            (EventClipboard::Controller(clip), t)
                if clip.kind.is_some() && graph_controller(t) == clip.kind =>
            {
                let kind = clip.kind.unwrap_or(ControllerKind::PAN);
                let points = match clip.placed(at) {
                    Ok(p) => p,
                    Err(e) => {
                        self.set_status(e.user_message(), StatusKind::Error);
                        return true;
                    }
                };
                let ti = self.edit_target();
                let edit = self
                    .doc()
                    .and_then(|d| plumspirit_engine::editor::note_view(d, ti).ok())
                    .map(|t| {
                        controllers::prepare_points(
                            t,
                            kind,
                            channel,
                            &points,
                            WritePolicy::Merge,
                            budget,
                        )
                    });
                if let Some(edit) = edit {
                    self.apply_events(edit, "Paste Events");
                }
                true
            }
            (EventClipboard::Controller(clip), _) => {
                self.set_status(
                    format!(
                        "The copied events are {}; choose that in the graph to paste them.",
                        clip.kind
                            .map_or_else(String::new, ControllerKind::full_label)
                    ),
                    StatusKind::Warning,
                );
                true
            }
            (EventClipboard::Master(_), _) => {
                self.set_status(
                    "The copied points are Master Volume; show Master Volume in the graph to paste them.",
                    StatusKind::Warning,
                );
                true
            }
        }
    }

    /// The compact event list of the active graph (a small window).
    pub(super) fn event_list_ui(&mut self, ctx: &egui::Context) {
        if !self.event_list_open {
            return;
        }
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let slot = self.active_graph;
        let target = self.shown_target(slot);
        let channel = self.graph_slot(slot).channel;
        let meter = self.meter_map().cloned();
        let ti = self.edit_target();
        let conductor = self.conductor_selected();
        let mut open = true;
        let mut select: Option<(Vec<u64>, bool)> = None;
        let mut seek: Option<u64> = None;
        let mut edit: Option<GraphAction> = None;
        let title = format!(
            "Event List — Graph {}: {}{}",
            slot + 1,
            controllers::graph_label(target),
            if graph_controller(target).is_some() {
                format!(" · Ch {}", channel + 1)
            } else {
                String::new()
            }
        );
        // (key, tick, type, channel, value, selected)
        let view = &self.graphs.views[slot];
        let rows: Vec<(u64, u64, String, String, String, bool)> = match target {
            GraphTarget::Bpm => doc
                .conductor()
                .tempo_points()
                .iter()
                .map(|p| {
                    (
                        u64::from(p.id),
                        p.tick,
                        match p.interp {
                            Interp::Step => "Tempo".into(),
                            Interp::Linear => "Tempo (ramp)".into(),
                        },
                        "—".into(),
                        format!("{} BPM", format_tempo(p.value)),
                        view.selected_tempo().contains(&p.id),
                    )
                })
                .collect(),
            GraphTarget::MasterVolume => doc
                .conductor()
                .master_volume()
                .iter()
                .map(|p| {
                    (
                        u64::from(p.id),
                        p.tick,
                        "Master Volume".into(),
                        "all".into(),
                        format_master_volume(p.value),
                        view.selected_master().contains(&p.id),
                    )
                })
                .collect(),
            GraphTarget::Velocity => {
                // Notes starting in the visible range (bounded).
                let mut out = Vec::new();
                if let Some(t) = (!conductor)
                    .then(|| plumspirit_engine::editor::note_view(&doc, ti).ok())
                    .flatten()
                {
                    let (t0, t1) = self.roll.view.time.visible_range(self.roll.view_width());
                    t.notes.query(t0, t1, |_, n| {
                        if n.start_tick >= t0 && out.len() < 10_000 {
                            out.push((
                                0,
                                n.start_tick,
                                format!(
                                    "Note {}",
                                    plumspirit_engine::model::note::pitch_name(n.attrs.pitch())
                                ),
                                (n.attrs.channel() + 1).to_string(),
                                n.attrs.velocity().to_string(),
                                n.selected,
                            ));
                        }
                    });
                }
                out
            }
            t => match (
                graph_controller(t),
                (!conductor)
                    .then(|| plumspirit_engine::editor::note_view(&doc, ti).ok())
                    .flatten(),
            ) {
                (Some(kind), Some(track)) => {
                    let sel = view.selected_controllers();
                    collect_points(track.events(), kind, channel)
                        .into_iter()
                        .map(|p| {
                            (
                                p.index as u64,
                                p.tick,
                                kind.full_label(),
                                (channel + 1).to_string(),
                                kind.display(p.value),
                                sel.binary_search(&p.index).is_ok(),
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            },
        };
        let pal = &crate::ui::theme::ui_palette(ctx);
        egui::Window::new(title)
            .id(egui::Id::new("plumspirit-event-list"))
            .open(&mut open)
            .default_size([400.0, 280.0])
            .default_pos({
                let r = ctx.content_rect();
                egui::pos2(r.right() - 420.0, r.bottom() - 330.0)
            })
            .min_width(300.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                let small = |s: &str| egui::RichText::new(s).size(crate::ui::theme::size::SMALL);
                ui.label(
                    small(if target == GraphTarget::Velocity {
                        "Notes starting in the visible range (velocity belongs to each note). Click a row to go there."
                    } else {
                        "Click a row to select it (Ctrl+click adds), double-click to edit its value. Delete removes the selected events."
                    })
                    .color(pal.text_weak),
                );
                ui.separator();
                let row_h = 18.0;
                ui.label(
                    small(&format!("{:<14}  {:<22}  {:>3}  {}", "Position", "Type", "Ch", "Value"))
                        .monospace()
                        .strong(),
                );
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, rows.len(), |ui, range| {
                        for (key, tick, kind, ch, value, sel) in &rows[range] {
                            let pos = meter
                                .as_ref()
                                .map_or_else(|| tick.to_string(), |m| m.format_position(*tick));
                            let text = format!("{pos:<14}  {kind:<22}  {ch:>3}  {value}");
                            let resp = ui
                                .add(
                                    egui::Button::selectable(*sel, small(&text).monospace())
                                        .min_size(egui::vec2(ui.available_width(), row_h - 2.0)),
                                )
                                .on_hover_text(format!("tick {}", group_digits(*tick)));
                            if resp.clicked() {
                                seek = Some(*tick);
                                if target != GraphTarget::Velocity {
                                    select = Some((vec![*key], ui.input(|i| i.modifiers.command)));
                                }
                            }
                            if resp.double_clicked() && target != GraphTarget::Velocity {
                                edit = Some(match target {
                                    GraphTarget::Bpm => GraphAction::EditTempo {
                                        id: Some(*key as u32),
                                        tick: *tick,
                                    },
                                    GraphTarget::MasterVolume => GraphAction::EditMaster {
                                        id: Some(*key as u32),
                                        tick: *tick,
                                    },
                                    t => GraphAction::EditController {
                                        kind: graph_controller(t).unwrap_or(ControllerKind::PAN),
                                        channel,
                                        index: Some(*key as usize),
                                        tick: *tick,
                                    },
                                });
                            }
                        }
                    });
                ui.label(small(&format!("{} row(s)", group_digits(rows.len() as u64))).color(pal.text_weak));
            });
        drop(doc);
        self.event_list_open = open;
        if let Some((keys, add)) = select {
            let view = &mut self.graphs.views[slot];
            match target {
                GraphTarget::Bpm => view.select_tempo(keys.iter().map(|&k| k as u32), add),
                GraphTarget::MasterVolume => {
                    view.select_master(keys.iter().map(|&k| k as u32), add)
                }
                _ => view.select_controllers(keys.iter().map(|&k| k as usize), add),
            }
            self.graph_focus = true;
        }
        if let Some(t) = seek {
            self.seek_to(t);
        }
        if let Some(a) = edit {
            self.apply_graph_action(slot, a);
        }
    }
}
