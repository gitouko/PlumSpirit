//! Editing: every musical change goes through the document's undo history as one
//! [`Operation`]; selection changes do not (they are not document content).

use super::dialogs::{Dialog, ScaleAxis};
use super::{PlumSpiritApp, jobs};
use crate::diagnostics::format_duration;
use crate::piano_roll::RollAction;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::{self, EditControl, ExactFactor, PreparedEdit};
use plumspirit_engine::history::{HistoryError, Operation, Outcome};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::{Bpm, NoteRecord};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Edits touching more selected notes than this are prepared on a worker thread.
/// Measured on the reference machine (docs/PERFORMANCE.md): preparing Scale X for
/// 100k notes takes ~6 ms and committing ~2 ms, so smaller edits fit in one frame.
pub(super) const BACKGROUND_EDIT_THRESHOLD: usize = 100_000;
/// Factors with more than ~19 significant digits use exact 256-bit arithmetic
/// (~0.7 µs per note), so they move to the background much earlier.
const BACKGROUND_EDIT_THRESHOLD_SLOW: usize = 10_000;

#[derive(Clone, Copy, Debug)]
pub(super) enum Transform {
    FlipX,
    FlipY,
}

impl PlumSpiritApp {
    /// Applies an operation through the undo history. Too-large edits ask whether
    /// to continue without undo; other failures are reported. Returns the outcome
    /// on success.
    pub(super) fn apply_op(&mut self, op: Operation) -> Option<Outcome> {
        if self.job.is_some() {
            return None;
        }
        // B2-A01: the one editability gate — while the Piano Roll is locked
        // (no clip, the Conductor) no command, tool, paste or gesture reaches
        // the notes it would fall back to.
        if let Some(lock) = self.note_lock() {
            use plumspirit_engine::{editor::NoteTarget, model::ContentRef};
            let target = self.edit_target();
            let host = self.doc().and_then(|d| match target {
                NoteTarget::Track(i) => d.track(i).map(|t| ContentRef::Track(t.id())),
                NoteTarget::Content { content, .. } => Some(ContentRef::Owned(content)),
            });
            if touches_target(&op, target, host) {
                self.set_status(lock.hint(), StatusKind::Info);
                return None;
            }
        }

        // B2-F18: an edit of the selection that is not routed to every clip
        // of the Track Aggregate (tools, Quantize, Flip, Scale, …) is
        // refused while the selection spans several clips — never a partial
        // edit of one of them.
        if selection_dependent(&op) && self.selection_targets().len() > 1 {
            self.set_status(
                "This edit changes one clip at a time: select notes in one clip, or open the clip (Enter in the Track List).",
                StatusKind::Info,
            );
            return None;
        }
        // A Preview summary worker shares the document; stop it first.
        self.release_preview_worker();
        self.release_tool_worker();
        let label = op.label();
        // Prepared edits are timed by `commit_prepared` (including their
        // preparation, possibly on a worker).
        let timed_here = !matches!(op, Operation::Prepared { .. });
        let o = self.open.as_mut()?;
        let Some(doc) = Arc::get_mut(&mut o.doc) else {
            // Never fail silently: something still shares the document.
            self.set_status(
                format!("{label} could not be applied because the project is busy; try again."),
                StatusKind::Warning,
            );
            return None;
        };
        let started = Instant::now();
        match o.history.apply(doc, op) {
            Ok(out) => {
                // Batch and structural edits (and anything slow) are timed for the
                // status bar; single-note gestures are not.
                let elapsed = started.elapsed();
                if timed_here
                    && out != Outcome::Unchanged
                    && super::tasks::significant(label, elapsed)
                {
                    self.tasks
                        .record(label, elapsed, super::tasks::TaskOutcome::Done);
                }
                Some(out)
            }
            Err(HistoryError::TooLarge {
                needed,
                limit,
                operation,
            }) => {
                const MIB: usize = 1024 * 1024;
                let message = format!(
                    "Undo information for this edit needs about {} MB, more than the {} MB reserved for undo on this computer. Nothing has been changed yet.",
                    needed / MIB + 1,
                    limit / MIB
                );
                log::warn!("{label}: {message}");
                self.pending_operation = Some(operation);
                self.dialog = Some(Dialog::TooLargeForUndo {
                    label: label.to_owned(),
                    message,
                });
                None
            }
            Err(e) => {
                let msg = e.user_message();
                match e {
                    HistoryError::Inconsistent(_) => self.show_error("Undo history reset.", msg),
                    _ => self.set_status(msg, StatusKind::Error),
                }
                None
            }
        }
    }

    pub(super) fn apply_without_undo(&mut self, op: Operation) {
        let label = op.label();
        let Some(o) = self.open.as_mut() else {
            return;
        };
        let Some(doc) = Arc::get_mut(&mut o.doc) else {
            return;
        };
        // B1-0103: a Change PPQN (Preserve beats) over the undo budget still
        // moves the views with the ticks.
        let retime = match &op {
            Operation::SetTimebase { timebase, .. } if timebase.retimes_ticks() => doc
                .division()
                .musical_ppqn()
                .zip(timebase.division().musical_ppqn()),
            _ => None,
        };
        match o.history.apply_without_undo(doc, op) {
            Ok(_) => {
                if let Some((from, to)) = retime {
                    self.halt_for_retime();
                    self.retime_views(from as u32, to as u32);
                    self.dialog = None;
                }
                self.set_status(
                    format!("{label} applied without undo; the undo history was cleared."),
                    StatusKind::Warning,
                )
            }
            Err(e) => self.show_error(&format!("{label} was not applied."), e.user_message()),
        }
    }

    pub(super) fn undo(&mut self) {
        self.roll.cancel_gesture();
        self.graphs.cancel();
        let Some(o) = self.open.as_mut() else {
            return;
        };
        let previous_revision = o.history.current_state();
        let Some(doc) = Arc::get_mut(&mut o.doc) else {
            return;
        };
        let current = doc.track(o.active_track).map(|t| t.id());
        match o.history.undo(doc) {
            Ok(Some(a)) => {
                if let Some(i) = a.shown_track(current).and_then(|id| o.doc.index_of(id)) {
                    o.active_track = i;
                }
                o.active_track = o.active_track.min(o.doc.track_count() - 1);
                if a.structure_changed {
                    self.meter = None;
                }
                if let Some((from, to)) = a.retimed {
                    self.halt_for_retime();
                    self.retime_views(from, to);
                }
                self.set_status(format!("Undid {}.", a.label), StatusKind::Info);
                self.restore_clip_selection();
                self.lua_automation.block(self.lua_observation_stamp());
                self.publish_lua_observation(
                    crate::lua::reactive::Kind::Undo,
                    crate::lua::reactive::Data::History { previous_revision },
                );
            }
            Ok(None) => {}
            Err(e) => self.show_error("Undo failed.", e.user_message()),
        }
    }

    pub(super) fn redo(&mut self) {
        self.roll.cancel_gesture();
        self.graphs.cancel();
        let Some(o) = self.open.as_mut() else {
            return;
        };
        let previous_revision = o.history.current_state();
        let Some(doc) = Arc::get_mut(&mut o.doc) else {
            return;
        };
        let current = doc.track(o.active_track).map(|t| t.id());
        match o.history.redo(doc) {
            Ok(Some(a)) => {
                if let Some(i) = a.shown_track(current).and_then(|id| o.doc.index_of(id)) {
                    o.active_track = i;
                }
                o.active_track = o.active_track.min(o.doc.track_count() - 1);
                if a.structure_changed {
                    self.meter = None;
                }
                if let Some((from, to)) = a.retimed {
                    self.halt_for_retime();
                    self.retime_views(from, to);
                }
                self.set_status(format!("Redid {}.", a.label), StatusKind::Info);
                self.restore_clip_selection();
                self.lua_automation.block(self.lua_observation_stamp());
                self.publish_lua_observation(
                    crate::lua::reactive::Kind::Redo,
                    crate::lua::reactive::Data::History { previous_revision },
                );
            }
            Ok(None) => {}
            Err(e) => self.show_error("Redo failed.", e.user_message()),
        }
    }

    /// Runs `work` synchronously for small selections, or on a worker thread for
    /// large ones (the UI stays responsive and shows progress).
    pub(super) fn run_edit(
        &mut self,
        label: &'static str,
        heavy: bool,
        work: jobs::EditWork,
        ctx: &egui::Context,
    ) -> Result<(), editor::EditError> {
        let Some(o) = &self.open else {
            return Ok(());
        };
        if self.job.is_some() {
            return Ok(());
        }
        let total = self.selected_count() as u64;
        if heavy {
            let doc = o.doc.clone();
            let id = self.next_id();
            match jobs::spawn_edit(id, doc, label, total, work, ctx) {
                Ok(job) => self.job = Some(job),
                Err(e) => self.show_error("Could not start the operation.", e.to_string()),
            }
            return Ok(());
        }
        let started = Instant::now();
        let prepared = work(&o.doc, EditControl::default())?;
        self.commit_prepared(label, prepared, started);
        Ok(())
    }

    pub(super) fn commit_prepared(
        &mut self,
        label: &'static str,
        mut prepared: PreparedEdit,
        started: Instant,
    ) {
        if prepared == PreparedEdit::Identity {
            self.set_status(format!("{label}: nothing to change."), StatusKind::Info);
            return;
        }
        // v0.9 §086: a shrinking Scale X / Y removes the duplicates it creates.
        let removed = match &prepared {
            PreparedEdit::ReplaceSelected { host, .. } if label.starts_with("Scale") => {
                let n = prepared.affected_notes();
                self.doc()
                    .and_then(|d| d.notes_of(*host))
                    .map_or(0, |notes| notes.selected_count().saturating_sub(n))
            }
            _ => 0,
        };
        // B1-0117: the General Note Overlap Prevention on the edits it covers.
        let overlap = match (OVERLAP_EDITS.contains(&label), self.general_overlap()) {
            (true, Some(g)) => self.doc().map(|d| {
                plumspirit_engine::transform::overlap::prevent_in_edit(d, &mut prepared, g)
            }),
            _ => None,
        };
        let n = prepared.affected_notes();
        self.last_overlap = overlap;
        if overlap.is_some()
            && matches!(&prepared, PreparedEdit::InsertSelected { records, .. } if records.is_empty())
        {
            // Everything it would add overlaps: no undo step.
            self.set_status(
                format!("{label}: nothing added.{}", overlap_feedback(overlap)),
                StatusKind::Info,
            );
            return;
        }
        if self
            .apply_op(Operation::Prepared {
                edit: prepared,
                label,
            })
            .is_none()
        {
            return;
        }
        let elapsed = started.elapsed();
        self.tasks.record(
            format!("{label} ({} notes)", group_digits(n as u64)),
            elapsed,
            super::tasks::TaskOutcome::Done,
        );
        let removed = if removed > 0 {
            format!(
                " {} duplicate(s) the shrink created removed.",
                group_digits(removed as u64)
            )
        } else {
            String::new()
        };
        self.set_status(
            format!(
                "{label}: {} note(s) changed in {}.{removed}{}",
                group_digits(n as u64),
                format_duration(elapsed),
                overlap_feedback(overlap)
            ),
            StatusKind::Info,
        );
        if elapsed > Duration::from_millis(50) || n > 10_000 {
            log::info!(
                "{label} committed for {n} note(s) in {}",
                format_duration(elapsed)
            );
        }
    }

    pub(super) fn transform(&mut self, t: Transform, ctx: &egui::Context) {
        let ti = self.edit_target();
        let heavy = self.selected_count() >= BACKGROUND_EDIT_THRESHOLD;
        let (label, work): (&'static str, jobs::EditWork) = match t {
            Transform::FlipX => (
                "Flip X",
                Box::new(move |d, c| editor::prepare_flip_x(d, ti, c)),
            ),
            Transform::FlipY => (
                "Flip Y",
                Box::new(move |d, _| editor::prepare_flip_y(d, ti)),
            ),
        };
        if let Err(e) = self.run_edit(label, heavy, work, ctx) {
            self.show_error(&format!("{label} was not applied."), e.user_message());
        }
    }

    /// Scale dialog "Apply". Returns an error message to show inside the dialog.
    pub(super) fn apply_scale(
        &mut self,
        axis: ScaleAxis,
        text: &str,
        opts: editor::ScaleY,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let factor = ExactFactor::parse(text).map_err(|e| e.to_string())?;
        // v0.8 §196: Repeat Last Tool repeats this scale (a recipe, not output).
        self.last_recipe = Some(super::tools::Recipe::Scale {
            axis,
            text: text.to_owned(),
            opts,
        });
        let ti = self.edit_target();
        if axis == ScaleAxis::Y {
            // B1-0115 / B1-0116: the options Scale Y starts from next time
            // (and the scale handles use); Duplicate Notes itself is chosen
            // each time (§191).
            let p = &mut self.settings.scale_y;
            let before = *p;
            if let Some(fill) = opts.fill {
                p.fill = fill;
            }
            p.prevent = opts.prevent_overlap;
            p.cleanup = opts.cleanup;
            if *p != before {
                self.save_settings();
            }
            let limits = plumspirit_engine::transform::Limits::for_system_memory(
                crate::platform::total_memory_bytes(),
            );
            let selected = self.selected_count();
            let heavy = selected >= BACKGROUND_EDIT_THRESHOLD
                || (opts.fill.is_none()
                    && factor.text().chars().filter(char::is_ascii_digit).count() > 19
                    && selected >= BACKGROUND_EDIT_THRESHOLD_SLOW);
            let label = match opts.fill.map(|f| f.mode) {
                None => axis.title(),
                Some(plumspirit_engine::transform::geom::FillMode::Rows) => "Scale Y (Fill)",
                Some(plumspirit_engine::transform::geom::FillMode::Step { .. }) => {
                    "Scale Y (Copy by Pitch Step)"
                }
            };
            let work: jobs::EditWork =
                Box::new(move |d, _| editor::prepare_scale_y_with(d, ti, &factor, opts, &limits));
            return self
                .run_edit(label, heavy, work, ctx)
                .map_err(|e| e.user_message());
        }
        let selected = self.selected_count();
        let heavy = selected >= BACKGROUND_EDIT_THRESHOLD
            || (factor.text().chars().filter(char::is_ascii_digit).count() > 19
                && selected >= BACKGROUND_EDIT_THRESHOLD_SLOW);
        let work: jobs::EditWork = Box::new(move |d, c| editor::prepare_scale_x(d, ti, &factor, c));
        self.run_edit(axis.title(), heavy, work, ctx)
            .map_err(|e| e.user_message())
    }

    pub(super) fn copy(&mut self) -> bool {
        let hosts = self.aggregate_hosts();
        let Some(doc) = self.doc() else {
            return false;
        };
        match super::musical_clipboard::snapshot_notes(doc, &hosts) {
            Ok(Some((_, clip))) => {
                let n = clip.len();
                self.clipboard = Some(clip);
                self.set_status(
                    format!("Copied {} note(s).", group_digits(n as u64)),
                    StatusKind::Info,
                );
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.show_error("Copy failed.", e.user_message());
                false
            }
        }
    }

    pub(super) fn paste(&mut self, ctx: &egui::Context) {
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        // v0.8 §204: the Conductor holds no notes; pasting notes while it is
        // selected used to write them into the hidden last active track.
        if self.conductor_selected() {
            self.set_status(
                "The Conductor holds no notes: select a track to paste the copied notes into.",
                StatusKind::Info,
            );
            return;
        }
        // The edit cursor on the roll's axis (content ticks of an opened
        // clip); messages name the song position. B2-F18: in Track
        // Aggregate, into the clip playing at the cursor.
        let at_song = self.roll.cursor_tick;
        let (ti, at) = self.destination_at(at_song);
        let n = clip.len();
        if clip.has_groups() {
            // Grouped notes paste into fresh groups: the group table and the
            // notes change in one undo step (prepared here, not in a job).
            let Some(d) = self.doc() else {
                return;
            };
            match clip.prepare_paste_grouped(d, ti, at) {
                Ok((edit, table)) => self.apply_grouped_paste(edit, table, at_song),
                Err(e) => self.show_error("Paste was not applied.", e.user_message()),
            }
            return;
        }
        let heavy = clip.len() >= BACKGROUND_EDIT_THRESHOLD;
        let work: jobs::EditWork = Box::new(move |d, _| clip.prepare_paste(d, ti, at));
        self.last_overlap = None;
        match self.run_edit("Paste", heavy, work, ctx) {
            Ok(()) => {
                if !heavy && self.dialog.is_none() {
                    // B1-0117: what the Note Overlap Prevention removed.
                    let r = self.last_overlap.take();
                    let gone = r.map_or(0, |r| r.removed as usize);
                    self.set_status(
                        format!(
                            "Pasted {} note(s) at tick {}.{}",
                            group_digits(n.saturating_sub(gone) as u64),
                            group_digits(at_song),
                            overlap_feedback(r)
                        ),
                        StatusKind::Info,
                    );
                }
            }
            Err(e) => self.show_error("Paste was not applied.", e.user_message()),
        }
    }

    pub(super) fn apply_grouped_paste(
        &mut self,
        edit: PreparedEdit,
        table: plumspirit_engine::model::GroupTable,
        at_song: u64,
    ) {
        let n = edit.affected_notes();
        let op = Operation::Group {
            ops: vec![
                Operation::SetGroups {
                    table: Box::new(table),
                    label: "Paste",
                },
                Operation::Prepared {
                    edit,
                    label: "Paste",
                },
            ],
            label: "Paste",
        };
        if self.apply_op(op).is_some() {
            self.set_status(
                format!(
                    "Pasted {} note(s) at tick {} into new groups.",
                    group_digits(n as u64),
                    group_digits(at_song)
                ),
                StatusKind::Info,
            );
        }
    }

    pub(super) fn delete_selected(&mut self) {
        // B2-F18: every clip of the Track Aggregate with a selection.
        let total = self.selected_count();
        let op = self.routed("Delete Notes", |t| Operation::DeleteSelected { track: t });
        let grouped = matches!(op, Operation::Group { .. });
        if let Some(Outcome::Notes(n)) = self.apply_op(op) {
            let n = if grouped { total } else { n };
            self.set_status(
                format!("Deleted {} note(s).", group_digits(n as u64)),
                StatusKind::Info,
            );
        }
    }

    pub(super) fn add_track(&mut self) {
        if let Some(Outcome::Track(i)) = self.apply_op(Operation::AddTrack) {
            // The new track is the active one — also when the Conductor was
            // (V09-S10 context matrix: it stayed selected and the new track
            // was not shown active).
            self.selected_tracks.clear();
            self.select_track(i);
            self.meter = None;
            self.set_status(format!("Added Track {}.", i + 1), StatusKind::Info);
        }
    }

    pub(super) fn request_delete_track(&mut self) {
        let Some(o) = &self.open else {
            return;
        };
        if o.conductor_selected {
            self.dialog = Some(Dialog::Message {
                title: "The Conductor cannot be deleted.".into(),
                body: "Every project has exactly one Conductor: it holds the song's tempo, time signatures and markers for all tracks. Select a track to delete that track instead.".into(),
                is_error: false,
            });
            return;
        }
        let i = o.active_track;
        // v0.8 §204: with several tracks selected (the active one among them)
        // Delete Track deletes the selection, like the track list's menu.
        if self.action_tracks(i).len() > 1 {
            self.request_delete_tracks(i);
            return;
        }
        let Some(o) = &self.open else {
            return;
        };
        let Some(t) = o.doc.track(i) else {
            return;
        };
        if o.doc.track_count() == 1 {
            self.dialog = Some(Dialog::Message {
                title: "The last track cannot be deleted.".into(),
                body: "A MIDI file needs at least one track.".into(),
                is_error: false,
            });
            return;
        }
        let subtree = o.doc.subtree_end(i) - i;
        if subtree >= o.doc.track_count() {
            self.dialog = Some(Dialog::Message {
                title: "The last track cannot be deleted.".into(),
                body: "A MIDI file needs at least one track; this track and its subtracks are all the tracks of the project.".into(),
                is_error: false,
            });
            return;
        }
        if subtree == 1 && t.notes.is_empty() && t.events.is_empty() {
            self.delete_tracks(&[i]);
        } else {
            self.dialog = self.delete_tracks_dialog(&[i]);
        }
    }

    /// Deletes the subtrees rooted at `roots` (one undo step).
    pub(super) fn delete_tracks(&mut self, roots: &[usize]) {
        self.roll.cancel_gesture();
        self.graphs.cancel();
        let mut sorted = roots.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let op = if sorted.len() == 1 {
            Operation::DeleteTrack { index: sorted[0] }
        } else {
            // From the last to the first: earlier indices stay valid.
            Operation::Group {
                ops: sorted
                    .iter()
                    .rev()
                    .map(|&index| Operation::DeleteTrack { index })
                    .collect(),
                label: "Delete Tracks",
            }
        };
        if self.apply_op(op).is_some() {
            let count = self.doc().map_or(1, |d| d.track_count());
            if let Some(o) = self.open.as_mut() {
                o.active_track = sorted[0].min(count.saturating_sub(1));
            }
            self.selected_tracks.clear();
            self.meter = None;
            let what = if sorted.len() == 1 {
                format!("track {}", sorted[0] + 1)
            } else {
                format!("{} tracks", sorted.len())
            };
            let it = if sorted.len() == 1 { "it" } else { "them" };
            self.set_status(
                format!("Deleted {what}. Undo restores {it} (with subtracks)."),
                StatusKind::Info,
            );
        }
    }

    /// Commits the tempo typed in the transport row.
    pub(super) fn commit_tempo_text(&mut self) {
        let text = self.tempo_text.trim().to_owned();
        if text == self.tempo_display() {
            return;
        }
        // B1-0101: a tempo a MIDI file can store; its rounding is said.
        match Bpm::parse_midi(&text) {
            Ok(b) => {
                if self.set_start_tempo(b) {
                    let note = b.midi_note().map(|n| format!(" {n}")).unwrap_or_default();
                    self.set_status(format!("Tempo set to {b} BPM.{note}"), StatusKind::Info);
                }
            }
            Err(e) => {
                self.set_status(e, StatusKind::Error);
            }
        }
        self.tempo_text = self.tempo_display();
    }

    /// Sets the tempo at tick 0 (one undoable Conductor edit). Returns true when
    /// the tempo changed.
    pub(super) fn set_start_tempo(&mut self, b: Bpm) -> bool {
        let Some(d) = self.doc() else {
            return false;
        };
        match d
            .conductor()
            .prepare_set_start_tempo(plumspirit_engine::model::TempoValue::Bpm(b))
        {
            Ok(edit) => self
                .apply_op(Operation::Conductor {
                    edit,
                    label: "Change Tempo",
                })
                .is_some_and(|o| o != Outcome::Unchanged),
            Err(e) => {
                self.set_status(e.to_string(), StatusKind::Error);
                false
            }
        }
    }

    pub(super) fn apply_roll_actions(&mut self, actions: Vec<RollAction>, ctx: &egui::Context) {
        // B2-F15 / F19: a locked roll edits and selects no notes (the roll
        // sends none; this guards every other sender).
        let mut actions = actions;
        if let Some(lock) = self.note_lock() {
            let n = actions.len();
            actions.retain(|a| !a.touches_notes());
            if actions.len() < n {
                self.set_status(lock.hint(), StatusKind::Info);
            }
        }
        // What the roll edits (the opened clip's content, else the active
        // track's own notes; `app/clip_session.rs`).
        let ti = self.edit_target();
        for a in actions {
            match a {
                RollAction::FocusClip { clip, loc, add } => {
                    self.focus_aggregate_clip(clip, loc, add);
                }
                RollAction::InsertNote(record) => {
                    // B2-F18: into the clip playing where it is drawn.
                    let (ti, start) = self.destination_at(self.to_song(record.start_tick));
                    let record = NoteRecord {
                        start_tick: start,
                        ..record
                    };
                    // B1-0117: a drawn note that would overlap is not placed
                    // (or is shortened, with Shorten the Earlier Note).
                    let placed = match (self.general_overlap(), self.doc()) {
                        (Some(g), Some(d)) => editor::note_view(d, ti).ok().map(|v| {
                            plumspirit_engine::transform::overlap::prevent_note(v.notes, &record, g)
                        }),
                        _ => None,
                    };
                    match placed {
                        Some(None) => self.set_status(
                            "Not drawn: the note would overlap a note on the same key (Edit → Note Overlap Prevention).",
                            StatusKind::Info,
                        ),
                        Some(Some(r)) => {
                            let shortened = r.duration_ticks != record.duration_ticks;
                            if self
                                .apply_op(Operation::InsertNote { track: ti, record: r })
                                .is_some()
                                && shortened
                            {
                                self.set_status(
                                    "Drawn shorter: it ends where the next note on the same key starts (Note Overlap Prevention).",
                                    StatusKind::Info,
                                );
                            }
                        }
                        None => {
                            self.apply_op(Operation::InsertNote { track: ti, record });
                        }
                    }
                }
                RollAction::MoveSelected { dt, dp } => {
                    // B2-F18: several clips of the aggregate move together,
                    // prepared here as one undo step.
                    let targets = self.selection_targets();
                    if targets.len() > 1 {
                        let Some(d) = self.doc() else {
                            continue;
                        };
                        let ops: Result<Vec<Operation>, _> = targets
                            .iter()
                            .map(|&t| {
                                editor::prepare_move(d, t, dt, dp, EditControl::default()).map(
                                    |edit| Operation::Prepared {
                                        edit,
                                        label: "Move Notes",
                                    },
                                )
                            })
                            .collect();
                        match ops {
                            Ok(ops) => {
                                self.apply_op(Operation::Group {
                                    ops,
                                    label: "Move Notes",
                                });
                            }
                            Err(e) => self.set_status(e.user_message(), StatusKind::Error),
                        }
                        continue;
                    }
                    let heavy = self.selected_count() >= BACKGROUND_EDIT_THRESHOLD;
                    let work: jobs::EditWork =
                        Box::new(move |d, c| editor::prepare_move(d, ti, dt, dp, c));
                    if let Err(e) = self.run_edit("Move Notes", heavy, work, ctx) {
                        self.set_status(e.user_message(), StatusKind::Error);
                    }
                }
                RollAction::ResizeSelected { delta } => {
                    let Ok(delta) = i64::try_from(delta) else {
                        self.set_status("That resize is too large.", StatusKind::Error);
                        continue;
                    };
                    let op = self.routed("Resize Notes", |t| Operation::Resize { track: t, delta });
                    self.apply_op(op);
                }
                RollAction::SetVelocity(v) => {
                    let total = self.selected_count();
                    let op = self.routed("Set Velocity", |t| Operation::SetVelocity {
                        track: t,
                        velocity: v,
                    });
                    let grouped = matches!(op, Operation::Group { .. });
                    if let Some(Outcome::Notes(n)) = self.apply_op(op) {
                        let n = if grouped { total } else { n };
                        self.set_status(
                            format!("Velocity {v} set on {} note(s).", group_digits(n as u64)),
                            StatusKind::Info,
                        );
                    }
                }
                RollAction::Seek(tick) => self.seek_from_roll(tick),
                RollAction::SetTimeSelection(r) => self.set_time_selection(r),
                RollAction::LoopTimeSelection => self.loop_to_selection(),
                RollAction::SelectInTimeSelection => {
                    if !self.conductor_selected() {
                        self.select_in_time_selection(&[ti.host_track()]);
                    }
                }
                RollAction::Erase(locs) => self.erase_notes(&locs),
                RollAction::AuditionKey(pitch) => self.audition_key(pitch),
                // The roll's tick is on its axis; the transport is song time.
                RollAction::AuditionNote { pitch, tick } => {
                    self.audition_note(pitch, self.to_song(tick))
                }
                RollAction::ScaleBox { from, to, fill } => self.commit_scale_box(from, to, fill),
                RollAction::WarpBox { from, warp, fill } => self.commit_warp_box(from, warp, fill),
                RollAction::Stroke { points, done } => self.tool_stroke(&points, done, ctx),
                RollAction::StrokeCanceled => self.cancel_custom_stroke(),
                RollAction::StrokeVertex(t, p) => self.tool_vertex(t, p),
                a @ (RollAction::MoveMarker { .. }
                | RollAction::EditMarker(_)
                | RollAction::DeleteMarker(_)
                | RollAction::AddMarkerAt(_)
                | RollAction::EditSignature(_)
                | RollAction::DeleteSignature(_)
                | RollAction::AddSignatureAt(_)) => self.apply_song_action(a),
                selection => {
                    // B2-F18: a replacing selection replaces it in every
                    // clip of the aggregate.
                    if matches!(
                        selection,
                        RollAction::SelectOnly(_)
                            | RollAction::ClearSelection
                            | RollAction::SelectRegion { .. }
                    ) {
                        self.clear_other_selections();
                    }
                    self.graphs.clear_point_selection();
                    self.roll.selected_marker = None;
                    self.roll.selected_signature = None;
                    let Some(d) = self.doc_mut() else {
                        return;
                    };
                    let groups = std::mem::take(&mut d.project_mut().groups);
                    let Ok(n) = editor::notes_mut(d, ti) else {
                        d.project_mut().groups = groups;
                        return;
                    };
                    // v0.7: a grouped note brings its whole (top-level) group.
                    let whole_group =
                        |n: &mut plumspirit_engine::model::NoteStore,
                         loc: plumspirit_engine::model::NoteLoc| {
                            let g = n.get(loc).map_or(0, |v| v.group);
                            if g != 0 {
                                let root = groups.root_of(g);
                                n.select_groups(|x| x != 0 && groups.root_of(x) == root);
                            }
                        };
                    match selection {
                        RollAction::SelectOnly(loc) => {
                            n.clear_selection();
                            n.set_selected(loc, true);
                            whole_group(n, loc);
                        }
                        RollAction::ToggleSelected(loc) => {
                            n.toggle_selected(loc);
                        }
                        RollAction::AddSelected(loc) => {
                            n.set_selected(loc, true);
                            whole_group(n, loc);
                        }
                        RollAction::ClearSelection => n.clear_selection(),
                        RollAction::SelectRegion {
                            t0,
                            t1,
                            pitch_lo,
                            pitch_hi,
                        } => {
                            n.select_in_region(t0, t1, pitch_lo, pitch_hi);
                        }
                        _ => {}
                    }
                    d.project_mut().groups = groups;
                }
            }
        }
    }
}

/// B1-0117: the prepared edits (by undo label) the General Note Overlap
/// Prevention covers; tool runs, the scale / corner handles and Draw are
/// covered at their own commit. Moves, nudges, Undo / Redo and imports are not.
pub(super) const OVERLAP_EDITS: &[&str] = &[
    "Paste",
    "Scale X",
    "Scale Y",
    "Scale Y (Fill)",
    "Scale Y (Copy by Pitch Step)",
    "Flip X",
    "Flip Y",
    "Quantize",
    "Quantize Ticks",
    "Quantize Gates",
];

/// The status-line words for what the General Note Overlap Prevention did
/// (nothing when it did not run).
pub(super) fn overlap_feedback(r: Option<plumspirit_engine::transform::overlap::Report>) -> String {
    match r {
        None => String::new(),
        Some(r) if r.removed + r.trimmed == 0 => " Note Overlap Prevention: no overlap.".into(),
        Some(r) => format!(
            " Note Overlap Prevention: {} removed, {} shortened.",
            group_digits(r.removed),
            group_digits(r.trimmed)
        ),
    }
}

#[cfg(test)]
mod overlap_tests {
    //! B1-S39: the General Note Overlap Prevention through the app's own
    //! entry points — off by default, Paste, Draw and a one-click tool.
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use crate::piano_roll::RollAction;
    use plumspirit_engine::model::{
        Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
    };

    fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
        NoteRecord {
            start_tick: start,
            duration_ticks: dur,
            attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        }
    }

    /// 60 at 0..480 selected, 72 at 0..480 unselected.
    fn harness() -> Harness {
        let mut d = PlumDocument::new_project("b1s39".into(), Bpm::DEFAULT, 960);
        let n = &mut d.track_mut(0).unwrap().notes;
        n.insert(rec(0, 480, 72), false).unwrap();
        n.insert(rec(0, 480, 60), true).unwrap();
        let mut h = Harness::new(d);
        h.app.main_view = super::super::track_list::MainView::PianoRoll;
        h
    }

    fn count(h: &Harness) -> usize {
        h.doc().track(0).unwrap().notes.len()
    }

    fn status(h: &Harness) -> String {
        h.app
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn off_by_default_then_paste_and_draw_keep_clear_of_existing_notes() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        assert!(!h.app.settings.overlap.on, "B1-0117: off by default");
        assert!(h.app.general_overlap().is_none());
        // Off: pasting onto the copied note doubles it.
        h.app.copy();
        h.app.paste(&ctx);
        assert_eq!(count(&h), 3);
        assert!(!status(&h).contains("Overlap"), "{}", status(&h));
        h.app.undo();
        assert_eq!(count(&h), 2);
        // On: the pasted note would overlap the one already there: it goes.
        assert!(h.command(Command::ToggleOverlapPrevention));
        assert!(h.app.settings.overlap.on);
        h.app.paste(&ctx);
        assert_eq!(count(&h), 2);
        assert!(
            status(&h).contains("Note Overlap Prevention: 1 removed, 0 shortened."),
            "{}",
            status(&h)
        );
        assert!(
            !h.app.enabled(Command::Undo),
            "nothing was added: no empty undo step"
        );
        // Draw on top of a note: not placed.
        h.app
            .apply_roll_actions(vec![RollAction::InsertNote(rec(240, 480, 60))], &ctx);
        assert_eq!(count(&h), 2);
        assert!(status(&h).starts_with("Not drawn"), "{}", status(&h));
        // Shorten the Earlier Note: a note drawn before the 72 that reaches
        // into it is drawn shorter.
        assert!(h.command(Command::OverlapMode(2)));
        h.app
            .apply_roll_actions(vec![RollAction::InsertNote(rec(960, 480, 61))], &ctx);
        assert_eq!(count(&h), 3, "a free key: drawn as it is");
        // Off again: everything as before.
        assert!(h.command(Command::ToggleOverlapPrevention));
        h.app.paste(&ctx);
        assert_eq!(count(&h), 4);
    }

    #[test]
    fn a_one_click_tool_and_repeat_last_tool_follow_the_setting() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        // Duplicate Up One Octave copies 60 onto the unselected 72.
        h.command(Command::ToggleOverlapPrevention);
        assert!(h.command(Command::DuplicateOctaveUp));
        assert_eq!(count(&h), 2, "the copy would overlap 72: removed");
        assert!(status(&h).contains("Overlaps: 1 removed"), "{}", status(&h));
        // Only among results: the copy stays.
        assert!(h.command(Command::ToggleOverlapExisting));
        assert!(h.command(Command::DuplicateOctaveUp));
        assert_eq!(count(&h), 3);
        // Repeat Last Tool runs it again with the setting of now.
        assert!(h.command(Command::ToggleOverlapExisting));
        assert!(h.command(Command::RepeatLastTool));
        // The selection (60 + its 72 copy) goes up an octave: the new 84
        // stays, the new 72 would lie on the 72s already there and goes.
        assert_eq!(count(&h), 4);
        let on_72 = h
            .doc()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .filter(|r| r.attrs.pitch() == 72)
            .count();
        assert_eq!(on_72, 2);
        let _ = ctx;
    }
}

#[cfg(test)]
mod tempo_tests {
    //! B1-S41 (B1-0101): typed tempos are checked against what a MIDI file can
    //! store, and a tempo MIDI rounds is said.
    use super::super::test_support::Harness;
    use plumspirit_engine::model::{Bpm, PlumDocument};

    fn status(h: &Harness) -> String {
        h.app
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn typed_tempos_stay_in_the_midi_range_and_rounding_is_said() {
        let mut h = Harness::new(PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960));
        h.app.tempo_text = "1e9".into();
        h.app.commit_tempo_text();
        assert!(status(&h).contains("60,000,000"), "{}", status(&h));
        assert_eq!(h.app.tempo_display(), "120");
        h.app.tempo_text = "123.456".into();
        h.app.commit_tempo_text();
        assert!(
            status(&h).contains("A MIDI file stores 486,003 µs per quarter note"),
            "{}",
            status(&h)
        );
        h.app.tempo_text = "60000000".into();
        h.app.commit_tempo_text();
        assert_eq!(h.app.tempo_display(), "60000000");
        // New Project refuses a tempo outside the MIDI range.
        let e = h.app.create_project("x", "2", "960", false).unwrap_err();
        assert!(e.contains("3.5762788"), "{e}");
        assert!(h.app.create_project("x", "140", "65535", false).is_ok());
    }
}

/// B2-A01: whether `op` changes the notes of `target` (`host`: the same
/// target as a content reference, for prepared edits). Track-level
/// operations (properties, names, subchannel tables, clips, the Conductor,
/// track events) never do.
fn touches_target(
    op: &Operation,
    target: plumspirit_engine::editor::NoteTarget,
    host: Option<plumspirit_engine::model::ContentRef>,
) -> bool {
    match op {
        Operation::InsertNote { track, .. }
        | Operation::DeleteSelected { track }
        | Operation::Resize { track, .. }
        | Operation::SetVelocity { track, .. }
        | Operation::SetDuration { track, .. }
        | Operation::EraseNotes { track, .. }
        | Operation::EraseRecords { track, .. }
        | Operation::SetSubchannel { track, .. }
        | Operation::SetVelocities { track, .. }
        | Operation::SetChannel { track, .. } => *track == target,
        Operation::Prepared { edit, .. } => match edit {
            PreparedEdit::Identity => false,
            PreparedEdit::ReplaceSelected { host: h, .. }
            | PreparedEdit::ReplaceSelectedErasing { host: h, .. }
            | PreparedEdit::RemapSelectedPitch { host: h, .. }
            | PreparedEdit::InsertSelected { host: h, .. } => Some(*h) == host,
        },
        Operation::Group { ops, .. } => ops.iter().any(|o| touches_target(o, target, host)),
        _ => false,
    }
}

/// B2-F18: whether `op` changes the selected notes of one host (a single,
/// not routed, selection-wide edit).
fn selection_dependent(op: &Operation) -> bool {
    match op {
        Operation::DeleteSelected { .. }
        | Operation::Resize { .. }
        | Operation::SetVelocity { .. }
        | Operation::SetDuration { .. }
        | Operation::SetSubchannel { .. }
        | Operation::SetVelocities { .. }
        | Operation::SetChannel { .. } => true,
        Operation::Prepared { edit, .. } => matches!(
            edit,
            PreparedEdit::ReplaceSelected { .. }
                | PreparedEdit::ReplaceSelectedErasing { .. }
                | PreparedEdit::RemapSelectedPitch { .. }
        ),
        _ => false,
    }
}
