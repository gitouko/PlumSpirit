//! Track management (v0.7, contract §129–131): multi-selection (Ctrl toggles,
//! Shift selects a range of visible rows), Add / Delete from any track,
//! drag-and-drop reorder and reparent, keyboard nudges and indent / outdent.
//!
//! Scope rule (shown in the menus): Delete, Move, Nudge and Indent act on whole
//! **subtrees** (a track always travels with its subtracks); Mute / Solo of the
//! selection act on the **selected tracks only**. Every structural change is one
//! undo step through the engine's `MoveTracks` / `DeleteTrack` operations, which
//! refuse cycles and orphaned subtracks.

use super::PlumSpiritApp;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::history::{Outcome, TrackPlace};
use plumspirit_engine::model::TrackId;
use std::collections::BTreeSet;

/// The tracks (indices, sorted) an action started on track `clicked` applies
/// to. The multi-selection is the Ctrl / Shift-clicked tracks **plus the
/// active track** (both are highlighted in the panel): an action started on
/// any of them applies to all of them; an action started on another track
/// applies to that track alone. `ids` are the document's track ids by index;
/// `active` is `None` while the Conductor is selected.
fn action_set(
    ids: &[TrackId],
    selected: &BTreeSet<TrackId>,
    clicked: usize,
    active: Option<usize>,
) -> Vec<usize> {
    let in_sel =
        ids.get(clicked).is_some_and(|id| selected.contains(id)) || active == Some(clicked);
    if !in_sel || selected.is_empty() {
        return vec![clicked];
    }
    let mut v: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| selected.contains(id))
        .map(|(i, _)| i)
        .collect();
    if let Some(a) = active
        && !v.contains(&a)
        && a < ids.len()
    {
        v.push(a);
    }
    v.sort_unstable();
    v
}

impl PlumSpiritApp {
    /// Shift+click: selects every visible row between the active track and
    /// `i` (inclusive).
    pub(super) fn select_track_range(&mut self, i: usize, rows: &[super::TreeRowView]) {
        let active = self.active_track();
        let pos = |k: usize| rows.iter().position(|r| !r.header && r.index == k);
        let (Some(a), Some(b)) = (pos(active), pos(i)) else {
            self.select_track(i);
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        let ids: Vec<_> = rows[lo..=hi]
            .iter()
            .filter(|r| !r.header)
            .filter_map(|r| self.doc().and_then(|d| d.track(r.index)).map(|t| t.id()))
            .collect();
        self.selected_tracks.clear();
        self.selected_tracks.extend(ids);
        self.select_track(i);
        if let Some(id) = self.doc().and_then(|d| d.track(i)).map(|t| t.id()) {
            self.selected_tracks.insert(id);
        }
    }

    /// The tracks an action applies to: the multi-selection plus the active
    /// track, or `clicked` alone when it is not part of the selection.
    pub(super) fn action_tracks(&self, clicked: usize) -> Vec<usize> {
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let ids: Vec<_> = doc.tracks().iter().map(|t| t.id()).collect();
        let active = (!self.conductor_selected()).then(|| self.active_track());
        action_set(&ids, &self.selected_tracks, clicked, active)
    }

    /// Top-most roots of `tracks` (a track inside another listed subtree goes
    /// with it), in document order.
    fn subtree_roots(&self, tracks: &[usize]) -> Vec<usize> {
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let mut v = tracks.to_vec();
        v.sort_unstable();
        v.dedup();
        let mut out = Vec::new();
        let mut covered = 0usize;
        for i in v {
            if i < covered {
                continue;
            }
            out.push(i);
            covered = doc.subtree_end(i);
        }
        out
    }

    /// Adds an empty track right after `i`'s subtree, as `i`'s sibling (a
    /// subtrack beside a subtrack), and makes it active. One undo step.
    pub(super) fn add_track_below(&mut self, i: usize) {
        let Some(doc) = self.doc() else {
            return;
        };
        // v0.8 §203: folder-aware (beside a folder member the new track joins
        // the folder instead of splitting it).
        let op = match plumspirit_engine::editor::folders::plan_add_track_below(doc, i) {
            Ok(op) => op,
            Err(msg) => {
                self.set_status(msg, StatusKind::Warning);
                return;
            }
        };
        let to = doc.subtree_end(i);
        if self.apply_op(op).is_some() {
            self.selected_tracks.clear();
            self.select_track(to);
            self.meter = None;
            self.set_status(
                format!("Added {}.", self.track_display_name(to)),
                StatusKind::Info,
            );
        }
    }

    /// Delete from the track list: the selection (when `clicked` is part of
    /// it) or `clicked`, each with its subtracks, after a confirmation that
    /// counts what goes.
    pub(super) fn request_delete_tracks(&mut self, clicked: usize) {
        let roots = self.subtree_roots(&self.action_tracks(clicked));
        let Some(doc) = self.doc() else {
            return;
        };
        let covered: usize = roots.iter().map(|&r| doc.subtree_end(r) - r).sum();
        if roots.is_empty() {
            return;
        }
        if covered >= doc.track_count() {
            self.set_status(
                "A MIDI file needs at least one track: these are all the tracks of the project.",
                StatusKind::Warning,
            );
            return;
        }
        let empty = roots.iter().all(|&r| {
            (r..doc.subtree_end(r))
                .all(|k| doc.tracks()[k].notes.is_empty() && doc.tracks()[k].events.is_empty())
        });
        if empty && covered == roots.len() {
            self.delete_tracks(&roots);
        } else {
            self.dialog = self.delete_tracks_dialog(&roots);
        }
    }

    /// Drag-and-drop: moves the dragged track (or the selection it belongs to)
    /// before / after / into `target`. One undo step.
    pub(super) fn move_tracks_to(&mut self, dragged: usize, target: usize, place: TrackPlace) {
        let tracks = self.action_tracks(dragged);
        let Some(doc) = self.doc() else {
            return;
        };
        let ids: Vec<_> = tracks
            .iter()
            .filter_map(|&i| doc.track(i).map(|t| t.id()))
            .collect();
        let active_id = doc.track(self.active_track()).map(|t| t.id());
        // v0.8: folder-aware (moved top-level tracks take the target's folder).
        match plumspirit_engine::editor::folders::plan_move_next_to(doc, &tracks, target, place) {
            Ok(op) => {
                if let Some(out) = self.apply_op(op)
                    && out != Outcome::Unchanged
                {
                    self.meter = None;
                    // Keep the same track active.
                    if let Some(i) = active_id.and_then(|id| self.doc()?.index_of(id))
                        && let Some(o) = self.open.as_mut()
                    {
                        o.active_track = i;
                    }
                    self.set_status(
                        format!("Moved {} track(s). Undo puts them back.", ids.len().max(1)),
                        StatusKind::Info,
                    );
                }
            }
            Err(msg) => self.set_status(msg, StatusKind::Warning),
        }
    }

    /// Moves the active track's subtree (or the selection) one sibling up or
    /// down (Alt+↑ / Alt+↓).
    pub(super) fn nudge_tracks(&mut self, i: usize, down: bool) {
        let Some(doc) = self.doc() else {
            return;
        };
        let roots = self.subtree_roots(&self.action_tracks(i));
        let (Some(&first), Some(&last)) = (roots.first(), roots.last()) else {
            return;
        };
        let parent = doc.track(first).and_then(|t| t.parent());
        // Previous / next sibling (same parent) outside the moved block.
        let target = if down {
            let after = doc.subtree_end(last);
            (after < doc.track_count() && doc.track(after).and_then(|t| t.parent()) == parent)
                .then_some(after)
        } else {
            (0..first)
                .rev()
                .find(|&k| doc.track(k).and_then(|t| t.parent()) == parent)
        };
        let Some(t) = target else {
            self.set_status(
                if down {
                    "The track is already the last of its level."
                } else {
                    "The track is already the first of its level."
                },
                StatusKind::Info,
            );
            return;
        };
        let place = if down {
            TrackPlace::After
        } else {
            TrackPlace::Before
        };
        self.move_tracks_to(i, t, place);
    }

    /// Indent: the track becomes the last subtrack of its previous sibling.
    /// Outdent: it becomes its parent's next sibling.
    pub(super) fn indent_tracks(&mut self, i: usize, indent: bool) {
        let Some(doc) = self.doc() else {
            return;
        };
        let parent = doc.track(i).and_then(|t| t.parent());
        if indent {
            let prev = (0..i)
                .rev()
                .find(|&k| doc.track(k).and_then(|t| t.parent()) == parent);
            match prev {
                Some(p) => self.move_tracks_to(i, p, TrackPlace::Into),
                None => self.set_status(
                    "There is no track above at this level to become the parent.",
                    StatusKind::Info,
                ),
            }
        } else {
            match parent.and_then(|p| doc.index_of(p)) {
                Some(pi) => self.move_tracks_to(i, pi, TrackPlace::After),
                None => self.set_status("The track is already at the top level.", StatusKind::Info),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_on_the_active_track_include_the_ctrl_clicked_tracks() {
        let ids: Vec<TrackId> = (0..5).map(TrackId).collect();
        // Click track 2 (active), Ctrl+click track 3: both are the selection.
        let sel: BTreeSet<TrackId> = [TrackId(3)].into();
        assert_eq!(action_set(&ids, &sel, 2, Some(2)), vec![2, 3]);
        assert_eq!(action_set(&ids, &sel, 3, Some(2)), vec![2, 3]);
        // Another track: that one alone.
        assert_eq!(action_set(&ids, &sel, 4, Some(2)), vec![4]);
        // No multi-selection: the clicked track.
        assert_eq!(action_set(&ids, &BTreeSet::new(), 2, Some(2)), vec![2]);
        // Conductor selected (no active track): only Ctrl-clicked tracks.
        let sel: BTreeSet<TrackId> = [TrackId(1), TrackId(4)].into();
        assert_eq!(action_set(&ids, &sel, 4, None), vec![1, 4]);
        assert_eq!(action_set(&ids, &sel, 2, None), vec![2]);
    }
}
