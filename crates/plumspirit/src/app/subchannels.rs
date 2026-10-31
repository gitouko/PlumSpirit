//! Subchannels (v0.5): the active subchannel for new notes, creating, editing,
//! deleting subchannels and moving notes between them.
//!
//! A subchannel is a PlumSpirit note group with its own colour; its "MIDI
//! channel" (1–16) is only the physical channel new notes receive. Every change
//! goes through the undo history; choosing the active subchannel is view state.

use super::PlumSpiritApp;
use super::dialogs::Dialog;
use super::tasks::TaskOutcome;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::{Subchannel, SubchannelId};
use std::collections::HashMap;
use std::time::Instant;

/// Largest number of subchannels added by one "Add Subchannels…" (a practical
/// bound per operation; more can be added in further steps).
pub const MAX_SUBCHANNEL_BATCH: usize = 1_000_000;

/// The subchannel receiving new notes on `track`: `chosen` if it still
/// exists, otherwise the track's first.
fn current_sub(
    track: &plumspirit_engine::model::Track,
    chosen: Option<SubchannelId>,
) -> Subchannel {
    let table = track.subchannels();
    chosen
        .and_then(|id| table.get(id))
        .unwrap_or_else(|| table.first())
        .clone()
}

/// v0.9 §083 / §260: what Ctrl+P does — the selected notes of `track` move
/// to its current subchannel and take that subchannel's MIDI channel (as
/// Move Selected Notes to Subchannel does by default), in one undo step.
fn paint_current_sub_op(
    track: impl Into<plumspirit_engine::editor::NoteTarget>,
    sub: SubchannelId,
) -> Operation {
    Operation::SetSubchannel {
        track: track.into(),
        sub,
        set_channel: true,
    }
}

impl PlumSpiritApp {
    /// The subchannel receiving new notes on track `index` (the one chosen by
    /// the user if it still exists, otherwise the track's first).
    pub(super) fn active_sub_of(&self, index: usize) -> Option<Subchannel> {
        let t = self.doc()?.track(index)?;
        Some(current_sub(t, self.active_subs.get(&t.id()).copied()))
    }

    /// v0.9 §083 (Ctrl+P): paints the active track's selected notes with its
    /// current subchannel (the one new notes get), one undo step.
    pub(super) fn paint_selection_with_current_sub(&mut self) {
        let ti = self.active_track();
        let Some(sc) = self.active_sub_of(ti) else {
            return;
        };
        let n = self.selected_count();
        if n == 0 {
            self.set_status("Select one or more notes first.", StatusKind::Info);
            return;
        }
        // The roll's notes (an opened clip's content; V09-S11).
        if self
            .apply_op(paint_current_sub_op(self.edit_target(), sc.id))
            .is_some()
        {
            self.set_status(
                format!(
                    "Painted {} note(s) with Subchannel {} (MIDI channel {}).",
                    group_digits(n as u64),
                    sc.id.number(),
                    sc.midi_channel + 1
                ),
                StatusKind::Info,
            );
        }
    }

    /// Selects exactly the notes of subchannel `id` in the active track.
    pub(super) fn select_sub_notes(&mut self, id: SubchannelId) {
        let Some(notes) = self.edit_notes_mut() else {
            return;
        };
        let started = std::time::Instant::now();
        notes.clear_selection();
        let n = notes.select_sub(id);
        self.set_status(
            format!(
                "Selected {} note(s) of subchannel {}.",
                group_digits(n as u64),
                id.number()
            ),
            StatusKind::Info,
        );
        self.tasks.finish(
            format!("Selected {} note(s)", group_digits(n as u64)),
            started,
            TaskOutcome::Done,
        );
    }

    pub(super) fn set_active_sub(&mut self, id: SubchannelId) {
        let Some(t) = self.doc().and_then(|d| d.track(self.active_track())) else {
            return;
        };
        if t.subchannels().contains(id) {
            let tid = t.id();
            self.active_subs.insert(tid, id);
        }
    }

    /// Adds `count` subchannels after the highest number (one undo step). Their
    /// MIDI channels continue from the last subchannel's (+ 1 each, cycling
    /// 1–16, optionally skipping channel 10). The first new one becomes active.
    pub(super) fn add_subchannels(&mut self, count: usize) {
        let ti = self.active_track();
        let Some(t) = self.doc().and_then(|d| d.track(ti)) else {
            return;
        };
        if count == 0 || count > MAX_SUBCHANNEL_BATCH {
            self.set_status(
                format!(
                    "Add 1 to {} subchannels at a time.",
                    group_digits(MAX_SUBCHANNEL_BATCH as u64)
                ),
                StatusKind::Warning,
            );
            return;
        }
        let channels = t
            .subchannels()
            .auto_channels(count, self.settings.skip_channel_10);
        let Some(ids) = t.subchannels().free_ids(count) else {
            self.set_status(
                "This track cannot hold that many more subchannels.",
                StatusKind::Error,
            );
            return;
        };
        let first = ids[0];
        let subs: Vec<Subchannel> = ids
            .into_iter()
            .zip(channels)
            .map(|(id, ch)| Subchannel::new(id, ch))
            .collect();
        let started = Instant::now();
        if self
            .apply_op(Operation::AddSubchannels { track: ti, subs })
            .is_some()
        {
            self.set_active_sub(first);
            let label = if count == 1 {
                format!("Added Subchannel {}", first.number())
            } else {
                format!("Added {} subchannels", group_digits(count as u64))
            };
            self.tasks.finish(label.clone(), started, TaskOutcome::Done);
            self.set_status(format!("{label}."), StatusKind::Success);
        }
    }

    /// "Go to" a subchannel by its number: selects it, or asks whether to create
    /// it. Never silently picks another one.
    pub(super) fn go_to_subchannel(&mut self, text: &str) {
        let t = text.trim().trim_start_matches('#');
        let Some(id) = t.parse::<u64>().ok().and_then(SubchannelId::from_number) else {
            self.set_status(
                format!(
                    "Enter a subchannel number from 1 to {}.",
                    group_digits(u64::from(u32::MAX) + 1)
                ),
                StatusKind::Warning,
            );
            return;
        };
        let ti = self.active_track();
        let Some(track) = self.doc().and_then(|d| d.track(ti)) else {
            return;
        };
        if track.subchannels().contains(id) {
            self.set_active_sub(id);
            self.sub_goto.clear();
            self.set_status(
                format!("Subchannel {} is active.", id.number()),
                StatusKind::Info,
            );
        } else {
            let channel = track
                .subchannels()
                .auto_channels(1, self.settings.skip_channel_10)[0];
            self.dialog = Some(Dialog::CreateSubchannel {
                track: ti,
                id,
                channel,
            });
        }
    }

    /// Creates subchannel `id` (the user confirmed) and makes it active.
    pub(super) fn create_subchannel(&mut self, track: usize, id: SubchannelId, channel: u8) {
        if self
            .apply_op(Operation::AddSubchannels {
                track,
                subs: vec![Subchannel::new(id, channel)],
            })
            .is_some()
        {
            self.set_active_sub(id);
            self.sub_goto.clear();
            self.set_status(
                format!("Added Subchannel {}.", id.number()),
                StatusKind::Success,
            );
        }
    }

    /// Opens the edit dialog for subchannel `id` of the active track.
    pub(super) fn edit_subchannel_dialog(&mut self, id: SubchannelId) {
        let ti = self.active_track();
        let Some(sc) = self
            .doc()
            .and_then(|d| d.track(ti))
            .and_then(|t| t.subchannels().get(id).cloned())
        else {
            return;
        };
        self.dialog = Some(Dialog::EditSubchannel {
            track: ti,
            id,
            name: sc.name.clone().unwrap_or_default(),
            channel: sc.midi_channel,
            move_notes: true,
            error: None,
        });
    }

    pub(super) fn apply_subchannel_edit(
        &mut self,
        track: usize,
        id: SubchannelId,
        name: &str,
        channel: u8,
        move_notes: bool,
    ) -> Result<(), String> {
        let name = name.trim();
        let mut sc = Subchannel::new(id, channel);
        sc.name = (!name.is_empty()).then(|| name.to_owned());
        sc.validate()?;
        let started = Instant::now();
        self.apply_op(Operation::EditSubchannel {
            track,
            sub: sc,
            move_notes,
        });
        self.tasks.finish(
            format!("Changed Subchannel {}", id.number()),
            started,
            TaskOutcome::Done,
        );
        Ok(())
    }

    /// Asks before deleting a subchannel (with its note count).
    pub(super) fn request_delete_subchannel(&mut self, id: SubchannelId) {
        let ti = self.active_track();
        let Some(t) = self.doc().and_then(|d| d.track(ti)) else {
            return;
        };
        if t.subchannels().len() < 2 {
            self.set_status(
                "A track keeps at least one subchannel, so its last subchannel cannot be deleted.",
                StatusKind::Warning,
            );
            return;
        }
        let notes = t.notes.count_in_sub(id);
        let name = t
            .subchannels()
            .get(id)
            .map(Subchannel::display_name)
            .unwrap_or_default();
        if notes == 0 {
            self.delete_subchannel(ti, id);
        } else {
            self.dialog = Some(Dialog::DeleteSubchannel {
                track: ti,
                id,
                name,
                notes,
            });
        }
    }

    pub(super) fn delete_subchannel(&mut self, track: usize, id: SubchannelId) {
        let started = Instant::now();
        if self
            .apply_op(Operation::DeleteSubchannel { track, id })
            .is_some()
        {
            if let Some(tid) = self.doc().and_then(|d| d.track(track)).map(|t| t.id())
                && self.active_subs.get(&tid) == Some(&id)
            {
                self.active_subs.remove(&tid);
            }
            self.tasks.finish(
                format!("Deleted Subchannel {}", id.number()),
                started,
                TaskOutcome::Done,
            );
            self.set_status(
                format!(
                    "Deleted Subchannel {} ({} restores it and its notes).",
                    id.number(),
                    super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                ),
                StatusKind::Info,
            );
        }
    }

    /// Moves the selected notes of the active track into subchannel `id`.
    pub(super) fn move_selection_to_sub(&mut self, id: SubchannelId, set_channel: bool) {
        let ti = self.edit_target();
        let n = self.selected_count();
        if n == 0 {
            self.set_status("Select one or more notes first.", StatusKind::Warning);
            return;
        }
        let started = Instant::now();
        if self
            .apply_op(Operation::SetSubchannel {
                track: ti,
                sub: id,
                set_channel,
            })
            .is_some()
        {
            self.tasks.finish(
                format!(
                    "Moved {} note(s) to Subchannel {}",
                    group_digits(n as u64),
                    id.number()
                ),
                started,
                TaskOutcome::Done,
            );
        }
    }

    /// Note counts per subchannel of the active track (for the Subchannels
    /// window), recomputed only when the notes change. O(notes) once.
    pub(super) fn subchannel_counts(&mut self) -> HashMap<u32, usize> {
        let ti = self.edit_target();
        let Some(t) = self
            .doc()
            .and_then(|d| plumspirit_engine::editor::note_view(d, ti).ok())
        else {
            return HashMap::new();
        };
        let key = (t.host(), t.notes.content_revision());
        if let Some((k, c)) = &self.sub_counts
            && *k == key
        {
            return c.clone();
        }
        let mut c: HashMap<u32, usize> = HashMap::new();
        for v in t.notes.iter() {
            *c.entry(v.sub.0).or_default() += 1;
        }
        self.sub_counts = Some((key, c.clone()));
        c
    }
}

/// What the Subchannels window asked for.
enum WindowAction {
    Select(SubchannelId),
    SelectNotes(SubchannelId),
    Edit(SubchannelId),
    Delete(SubchannelId),
    Add,
    AddMany,
    MoveHere(SubchannelId),
}

impl PlumSpiritApp {
    /// The Subchannels window (non-modal): every subchannel of the active track in
    /// a virtualized list (only the visible rows are laid out), with search,
    /// note counts, editing and deleting.
    pub(super) fn subchannel_window_ui(&mut self, ctx: &egui::Context, dark: bool) {
        if !self.subchannel_window {
            return;
        }
        let ti = self.active_track();
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            self.subchannel_window = false;
            return;
        };
        let Some(t) = doc.track(ti) else {
            return;
        };
        let counts = self.subchannel_counts();
        let active = self.active_sub_of(ti).map(|s| s.id);
        let selected = t.notes.selected_count();
        let editable = self.job.is_none() && !self.conductor_selected();
        let table = t.subchannels();
        let filter = self.sub_filter.trim().to_lowercase();
        // Filtered rows (indices into the table), cached per filter and table.
        let key = (t.id(), t.subchannels_revision(), filter.clone());
        if self.sub_filtered.as_ref().map(|(k, _)| k) != Some(&key) {
            let rows: Vec<usize> = if filter.is_empty() {
                Vec::new()
            } else {
                table
                    .entries()
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        e.id.number().to_string().contains(&filter)
                            || e.display_name().to_lowercase().contains(&filter)
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            self.sub_filtered = Some((key, rows));
        }
        let filtered = &self.sub_filtered.as_ref().expect("set above").1;
        let n_rows = if filter.is_empty() {
            table.len()
        } else {
            filtered.len()
        };
        let mut open = true;
        let mut action: Option<WindowAction> = None;
        let mut filter_text = std::mem::take(&mut self.sub_filter);
        let mut set_channel = self.sub_move_sets_channel;
        let pal = &crate::ui::theme::ui_palette(ctx);
        egui::Window::new(format!("Subchannels — {}", doc.track_label(ti)))
            .id(egui::Id::new("plumspirit-subchannels"))
            .open(&mut open)
            .default_size([340.0, 380.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} subchannel(s)",
                        group_digits(table.len() as u64)
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(editable, egui::Button::new("Add Several…"))
                            .clicked()
                        {
                            action = Some(WindowAction::AddMany);
                        }
                        if ui
                            .add_enabled(editable, egui::Button::new("+ Add"))
                            .clicked()
                        {
                            action = Some(WindowAction::Add);
                        }
                    });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut filter_text)
                        .hint_text("Find by number or name")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                let row_h = 22.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(ui.available_height() - 64.0)
                    .show_rows(ui, row_h, n_rows, |ui, range| {
                        for r in range {
                            let idx = if filter.is_empty() { r } else { filtered[r] };
                            let Some(e) = table.entries().get(idx) else {
                                continue;
                            };
                            let is_active = active == Some(e.id);
                            ui.horizontal(|ui| {
                                ui.set_height(row_h);
                                let (sw, _) = ui.allocate_exact_size(
                                    egui::vec2(12.0, 12.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    sw,
                                    0.0,
                                    crate::ui::theme::subchannel_color(
                                        &crate::ui::theme::note_palette(ui.ctx()),
                                        e.id.0,
                                        dark,
                                    ),
                                );
                                let text = format!(
                                    "{:>4}  {}",
                                    e.id.number(),
                                    e.name.as_deref().unwrap_or("")
                                );
                                let resp = ui
                                    .selectable_label(
                                        is_active,
                                        egui::RichText::new(text)
                                            .size(crate::ui::theme::size::SMALL),
                                    )
                                    .on_hover_text("Click: new notes go to this subchannel");
                                if resp.clicked() {
                                    action = Some(WindowAction::Select(e.id));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(editable, egui::Button::new("×").small())
                                            .on_hover_text("Delete this subchannel (and its notes)")
                                            .clicked()
                                        {
                                            action = Some(WindowAction::Delete(e.id));
                                        }
                                        if ui
                                            .add_enabled(
                                                editable,
                                                egui::Button::new("Edit").small(),
                                            )
                                            .clicked()
                                        {
                                            action = Some(WindowAction::Edit(e.id));
                                        }
                                        if ui
                                            .add_enabled(
                                                editable,
                                                egui::Button::new("Select").small(),
                                            )
                                            .on_hover_text("Select this subchannel's notes (replaces the selection)")
                                            .clicked()
                                        {
                                            action = Some(WindowAction::SelectNotes(e.id));
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Ch {} · {} notes",
                                                u16::from(e.midi_channel) + 1,
                                                group_digits(
                                                    counts.get(&e.id.0).copied().unwrap_or(0)
                                                        as u64
                                                )
                                            ))
                                            .size(crate::ui::theme::size::SMALL)
                                            .color(pal.text_weak),
                                        );
                                    },
                                );
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    let label = format!(
                        "Move {} selected note(s) here",
                        group_digits(selected as u64)
                    );
                    if ui
                        .add_enabled(
                            editable && selected > 0 && active.is_some(),
                            egui::Button::new(label),
                        )
                        .on_hover_text(
                            "Moves the selected notes into the active subchannel (one Undo step).",
                        )
                        .clicked()
                        && let Some(a) = active
                    {
                        action = Some(WindowAction::MoveHere(a));
                    }
                    ui.checkbox(&mut set_channel, "use its MIDI channel");
                });
            });
        self.sub_filter = filter_text;
        self.sub_move_sets_channel = set_channel;
        if !open {
            self.subchannel_window = false;
        }
        // The window read a shared handle of the document; release it before
        // any action edits (an edit needs the only handle: `Arc::get_mut`).
        // Holding it here made "+ Add", "Move here" and "Select" do nothing.
        drop(doc);
        match action {
            Some(WindowAction::Select(id)) => self.set_active_sub(id),
            Some(WindowAction::SelectNotes(id)) => self.select_sub_notes(id),
            Some(WindowAction::Edit(id)) => self.edit_subchannel_dialog(id),
            Some(WindowAction::Delete(id)) => self.request_delete_subchannel(id),
            Some(WindowAction::Add) => self.add_subchannels(1),
            Some(WindowAction::AddMany) => {
                self.dialog = Some(Dialog::AddMany {
                    target: self.track_display_name(self.active_track()),
                    what: super::dialogs::AddManyKind::Subchannels,
                    count: "16".into(),
                    error: None,
                    focused: false,
                });
            }
            Some(WindowAction::MoveHere(id)) => self.move_selection_to_sub(id, set_channel),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::history::History;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};

    /// v0.9 §260: current Subchannel N, Ctrl+P → every selected note is in N
    /// (on N's MIDI channel), unselected notes keep theirs, Undo restores
    /// exactly; a chosen subchannel that was deleted falls back to the first.
    #[test]
    fn ctrl_p_paints_the_selection_with_the_current_subchannel() {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        let sub = |id: u32, ch: u8| Subchannel {
            id: SubchannelId(id),
            midi_channel: ch,
            ..d.track(0).unwrap().subchannels().first().clone()
        };
        let subs = vec![sub(3, 4), sub(9, 7)];
        h.apply(&mut d, Operation::AddSubchannels { track: 0, subs })
            .unwrap();
        let rec = |i: u64, sub: u32| NoteRecord {
            start_tick: i * 10,
            duration_ticks: 5,
            attrs: NoteAttrs::new(60 + i as u8, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(sub),
            group: 0,
        };
        let notes: Vec<NoteRecord> = (0..6)
            .map(|i| rec(i, if i % 2 == 0 { 0 } else { 3 }))
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes[..4], true)
            .unwrap();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes[4..], false)
            .unwrap();
        let before = d.state_fingerprint();
        let t = d.track(0).unwrap();
        // The chosen current subchannel; a vanished choice → the first.
        let current = current_sub(t, Some(SubchannelId(9)));
        assert_eq!(current.id, SubchannelId(9));
        assert_eq!(
            current_sub(t, Some(SubchannelId(77))).id,
            t.subchannels().first().id
        );
        assert_eq!(current_sub(t, None).id, t.subchannels().first().id);
        h.apply(&mut d, paint_current_sub_op(0, current.id))
            .unwrap();
        let got: Vec<(u64, u32, u8, bool)> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.sub.0, n.attrs.channel(), n.selected))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 9, 7, true),
                (10, 9, 7, true),
                (20, 9, 7, true),
                (30, 9, 7, true),
                (40, 0, 0, false),
                (50, 3, 0, false),
            ]
        );
        h.undo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), before);
    }
}
