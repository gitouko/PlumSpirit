//! Track Folders in the app (v0.8 §077–§080): the track list's folder
//! actions, each one undo step through the history (collapse is view state).

use super::{PlumSpiritApp, StatusKind};
use crate::ui::track_panel::TrackAction;
use plumspirit_engine::editor::folders as fold;
use plumspirit_engine::history::{Operation, Outcome, TrackPlace};
use plumspirit_engine::model::TrackId;

impl PlumSpiritApp {
    /// Applies a planned folder / track-layout operation keeping the same
    /// track active; returns true when something changed.
    fn apply_layout(&mut self, planned: Result<Operation, String>, done: &str) -> bool {
        let op = match planned {
            Ok(op) => op,
            Err(msg) => {
                self.set_status(msg, StatusKind::Warning);
                return false;
            }
        };
        let active_id = self
            .doc()
            .and_then(|d| d.track(self.active_track()))
            .map(|t| t.id());
        match self.apply_op(op) {
            Some(out) if out != Outcome::Unchanged => {
                self.meter = None;
                if let Some(i) = active_id.and_then(|id| self.doc()?.index_of(id))
                    && let Some(o) = self.open.as_mut()
                {
                    o.active_track = i;
                }
                if !done.is_empty() {
                    self.set_status(done.to_owned(), StatusKind::Info);
                }
                true
            }
            _ => false,
        }
    }

    fn folder_name(&self, id: u32) -> String {
        self.doc()
            .and_then(|d| d.project().folders.get(id).map(|f| f.name.clone()))
            .unwrap_or_default()
    }

    /// The track list's folder actions.
    pub(super) fn folder_action(&mut self, a: TrackAction) {
        let Some(doc) = self.doc() else {
            return;
        };
        match a {
            TrackAction::FolderToggle(id) => {
                let Some(d) = self.doc_mut() else {
                    return;
                };
                let c = &mut d.project_mut().folders_collapsed;
                if !c.remove(&id) {
                    c.insert(id);
                }
                self.tree_rows.collapse_revision += 1;
            }
            TrackAction::FolderNew(at) => {
                let planned = match at {
                    None => fold::plan_create(doc, &[], "", None),
                    Some((i, true)) => {
                        let tracks = self.action_tracks(i);
                        fold::plan_create(doc, &tracks, "", None)
                    }
                    Some((i, false)) => {
                        let root = fold::top_root(doc, i);
                        let anchor = doc.track(root).map(|t| t.id());
                        fold::plan_create(doc, &[], "", anchor)
                    }
                };
                match planned {
                    Ok((op, id)) => {
                        if self.apply_layout(Ok(op), "") {
                            let name = self.folder_name(id);
                            self.set_status(
                                format!("Created folder “{name}”. Type a name, Enter to keep it."),
                                StatusKind::Info,
                            );
                            self.folder_rename = Some((id, name));
                        }
                    }
                    Err(msg) => self.set_status(msg, StatusKind::Warning),
                }
            }
            TrackAction::FolderRenameStart(id) => {
                let name = self.folder_name(id);
                self.folder_rename = Some((id, name));
            }
            TrackAction::FolderRename(id, text) => {
                if text.trim().is_empty() || text == self.folder_name(id) {
                    return;
                }
                let planned = fold::plan_rename(doc, id, &text);
                self.apply_layout(planned, "Folder renamed.");
            }
            TrackAction::FolderDelete(id) => {
                let name = self.folder_name(id);
                let n = fold::folder_roots(doc, id).len();
                let planned = fold::plan_delete(doc, id);
                if self.apply_layout(planned, "")
                    && let Some(d) = self.doc_mut()
                {
                    d.project_mut().folders_collapsed.remove(&id);
                    self.tree_rows.collapse_revision += 1;
                    self.set_status(
                        format!(
                            "Deleted folder “{name}”; its {n} track(s) stay in the list. Undo brings the folder back."
                        ),
                        StatusKind::Info,
                    );
                }
            }
            TrackAction::FolderAddTrack(id) => {
                let name = self.folder_name(id);
                let new_id = plumspirit_engine::model::TrackId(doc.next_track_id());
                let planned = fold::plan_add_track_to_folder(doc, id);
                if self.apply_layout(planned, "") {
                    self.expand_folder(id);
                    self.selected_tracks.clear();
                    if let Some(i) = self.doc().and_then(|d| d.index_of(new_id)) {
                        self.select_track(i);
                    }
                    self.set_status(
                        format!("Added a track to folder “{name}”. Undo removes it."),
                        StatusKind::Info,
                    );
                }
            }
            TrackAction::FolderDeleteWithTracks(id) => {
                let roots = fold::folder_roots(doc, id);
                if roots.is_empty() {
                    return;
                }
                let empty = roots.iter().all(|&r| {
                    (r..doc.subtree_end(r)).all(|k| {
                        doc.tracks()[k].notes.is_empty() && doc.tracks()[k].events.is_empty()
                    })
                });
                if empty {
                    self.delete_folder_with_tracks(id);
                } else if let Some(super::Dialog::DeleteTrack {
                    roots,
                    notes,
                    events,
                    subtracks,
                    ..
                }) = self.delete_tracks_dialog(&roots)
                {
                    self.dialog = Some(super::Dialog::DeleteTrack {
                        roots,
                        name: format!("folder “{}”", self.folder_name(id)),
                        notes,
                        events,
                        subtracks,
                        folder: id,
                    });
                }
            }
            TrackAction::MoveToFolder(i, id) => {
                let tracks = self.action_tracks(i);
                let planned = fold::plan_into(doc, &tracks, id);
                let name = self.folder_name(id);
                self.expand_folder(id);
                self.apply_layout(planned, &format!("Moved into folder “{name}”."));
            }
            TrackAction::RemoveFromFolder(i) => {
                let tracks = self.action_tracks(i);
                let planned = fold::plan_out(doc, &tracks);
                self.apply_layout(planned, "Removed from the folder.");
            }
            TrackAction::DropOnFolder {
                dragged,
                folder,
                before,
            } => {
                let tracks = self.action_tracks(dragged);
                if before {
                    let planned = fold::plan_next_to_folder(doc, &tracks, folder, true);
                    self.apply_layout(planned, "Moved before the folder.");
                } else {
                    let planned = fold::plan_into(doc, &tracks, folder);
                    let name = self.folder_name(folder);
                    self.expand_folder(folder);
                    self.apply_layout(planned, &format!("Moved into folder “{name}”."));
                }
            }
            TrackAction::MoveFolder {
                folder,
                target,
                place,
            } => {
                let (target, place) = if target >= doc.track_count() {
                    (doc.track_count().saturating_sub(1), TrackPlace::After)
                } else {
                    (target, place)
                };
                let planned = fold::plan_move_folder(doc, folder, target, place);
                self.apply_layout(planned, "Folder moved.");
            }
            _ => {}
        }
    }

    /// A folder that receives tracks opens (so the user sees where they went).
    /// Deletes folder `id` with all of its tracks (one undo step).
    pub(super) fn delete_folder_with_tracks(&mut self, id: u32) {
        let Some(doc) = self.doc() else {
            return;
        };
        let name = self.folder_name(id);
        let n = fold::folder_roots(doc, id).len();
        let first = fold::folder_roots(doc, id).first().copied().unwrap_or(0);
        let planned = fold::plan_delete_with_tracks(doc, id);
        self.roll.cancel_gesture();
        self.graphs.cancel();
        let op = match planned {
            Ok(op) => op,
            Err(msg) => {
                self.set_status(msg, StatusKind::Warning);
                return;
            }
        };
        if self.apply_op(op).is_some() {
            let count = self.doc().map_or(1, |d| d.track_count());
            if let Some(o) = self.open.as_mut() {
                o.active_track = first.min(count.saturating_sub(1));
            }
            if let Some(d) = self.doc_mut() {
                d.project_mut().folders_collapsed.remove(&id);
            }
            self.tree_rows.collapse_revision += 1;
            self.selected_tracks.clear();
            self.meter = None;
            self.set_status(
                format!("Deleted folder “{name}” and its {n} track(s). Undo brings them back."),
                StatusKind::Info,
            );
        }
    }

    /// B2-F03: Add Tracks… at `at` (None: below the active track, at the
    /// end from the Conductor) — opens the count dialog with the place said
    /// in words; nothing changes until it is confirmed.
    pub(super) fn request_add_tracks(&mut self, at: Option<fold::AddTracksAt>) {
        let Some(o) = &self.open else {
            return;
        };
        let at = at.unwrap_or(if o.conductor_selected {
            fold::AddTracksAt::End
        } else {
            fold::AddTracksAt::Below(o.active_track)
        });
        let Some(doc) = self.doc() else {
            return;
        };
        let place = match at {
            fold::AddTracksAt::End => "at the end of the track list".to_owned(),
            fold::AddTracksAt::Below(i) => {
                let Some(t) = doc.track(i) else {
                    return;
                };
                let folder = match t.parent() {
                    Some(_) => 0,
                    None => doc.project().folders.folder_of(t.id()),
                };
                let mut p = format!(
                    "below “{}” and its subtracks, at its level",
                    self.track_display_name(i)
                );
                if folder != 0 {
                    p.push_str(&format!(" (in folder “{}”)", self.folder_name(folder)));
                }
                p
            }
            fold::AddTracksAt::Folder(id) => {
                format!("at the end of folder “{}”", self.folder_name(id))
            }
        };
        let existing = doc.track_count();
        self.dialog = Some(super::Dialog::AddTracks(Box::new(
            super::dialogs::AddTracksDraft {
                at,
                place,
                count: "16".into(),
                existing,
                available: crate::platform::available_memory_bytes(),
                error: None,
                focused: false,
                confirm: None,
            },
        )));
    }

    /// B2-F03: adds `count` empty tracks at `at` as one undo step. The new
    /// tracks are selected with the first one active, their parent / folder
    /// is expanded and the list scrolls to them when they are out of view.
    pub(super) fn add_tracks(&mut self, at: fold::AddTracksAt, count: usize) {
        let Some(doc) = self.doc() else {
            return;
        };
        let op = match fold::plan_add_tracks(doc, at, count) {
            Ok(op) => op,
            Err(msg) => {
                self.show_error("The tracks were not added.", msg);
                return;
            }
        };
        let first = doc.next_track_id();
        let (parent, folder) = match at {
            fold::AddTracksAt::End => (None, 0),
            fold::AddTracksAt::Below(i) => match doc.track(i) {
                Some(t) if t.parent().is_some() => (t.parent(), 0),
                Some(t) => (None, doc.project().folders.folder_of(t.id())),
                None => (None, 0),
            },
            fold::AddTracksAt::Folder(id) => (None, id),
        };
        let started = std::time::Instant::now();
        if self.apply_op(op).is_none() {
            return;
        }
        if let Some(p) = parent
            && self.collapsed.remove(&p)
        {
            self.tree_rows.collapse_revision += 1;
        }
        if folder != 0 {
            self.expand_folder(folder);
        }
        let Some(i) = self.doc().and_then(|d| d.index_of(TrackId(first))) else {
            return;
        };
        self.selected_tracks.clear();
        if count > 1 {
            self.selected_tracks
                .extend((0..count as u32).map(|k| TrackId(first + k)));
        }
        self.select_track(i);
        self.meter = None;
        self.reveal_track(i);
        let label = if count == 1 {
            "Added a track".to_owned()
        } else {
            format!(
                "Added {} tracks",
                plumspirit_engine::midi::group_digits(count as u64)
            )
        };
        self.tasks
            .finish(label.clone(), started, super::tasks::TaskOutcome::Done);
        self.set_status(format!("{label}. Undo removes them."), StatusKind::Success);
    }

    /// Scrolls the track list (and the Track List beside it) so track `i`'s
    /// row is in view, when the last frame showed it outside the list.
    fn reveal_track(&mut self, i: usize) {
        let generation = self.doc_generation();
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let rows = self.tree_rows.rows(&o.doc, generation, &self.collapsed);
        let Some(r) = rows.iter().position(|row| !row.header && row.index == i) else {
            return;
        };
        let g = &self.row_geometry;
        let (top, h) = (r as f32 * g.row_h, g.list.height());
        if g.row_h > 0.0 && h > 0.0 && (top < g.scroll || top + g.row_h > g.scroll + h) {
            self.track_list_scroll = Some((top - h / 3.0).max(0.0));
        }
    }

    fn expand_folder(&mut self, id: u32) {
        if let Some(d) = self.doc_mut()
            && d.project_mut().folders_collapsed.remove(&id)
        {
            self.tree_rows.collapse_revision += 1;
        }
    }
}
