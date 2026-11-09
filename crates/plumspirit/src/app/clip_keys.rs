//! v1.0-beta2 (B2-F29, contract §023): the Track List's keyboard. Clip keys
//! act on the selected clip placements — never on the Piano Roll's notes —
//! through the same `ClipIntent`s as the clip menu (one Undo step each, the
//! clips keep their ids). Added here: the nudges (← / →), Select All
//! (Ctrl+A) over the clips, Zoom to Selection and Jump to Playhead (both
//! views), and the sentence a clip key says when it has nothing to act on.

use super::commands::Command;
use super::track_list::{ClipIntent, MainView};
use super::{PlumSpiritApp, StatusKind};

impl PlumSpiritApp {
    /// The Track List is the view shown (a project open, no MIDI Player).
    pub(super) fn track_list_shown(&self) -> bool {
        self.open.is_some() && self.main_view == MainView::TrackList && !self.player_shown()
    }

    /// The nudge step: the Track List's Snap unit, one beat with Snap off.
    pub(super) fn nudge_step(&self) -> u64 {
        self.track_list.snap.filter(|&u| u > 0).unwrap_or_else(|| {
            self.doc()
                .and_then(|d| d.division().musical_ppqn())
                .map_or(480, u64::from)
        })
    }

    /// Why Nudge Clips Left / Right cannot run (`None`: it can, or the
    /// command is off for another reason). The menus show it as the
    /// disabled entry's tooltip; the key says it in the status bar.
    pub(super) fn nudge_unavailable(&self, right: bool) -> Option<String> {
        self.open.as_ref()?;
        if !self.track_list_shown() {
            return Some(
                "Nudging moves clips selected in the Track List: show the Track List (View menu) and select the clips."
                    .into(),
            );
        }
        if self.track_list.selected.is_empty() {
            return Some(
                "Select one or more clips in the Track List to nudge them (click a clip; Ctrl+click adds more)."
                    .into(),
            );
        }
        if !right && self.track_list.selection().iter().any(|r| r.start == 0) {
            return Some(
                "A selected clip already starts at the beginning of the song: it cannot move earlier."
                    .into(),
            );
        }
        None
    }

    /// Nudge Clips Left / Right (← / →, the clip menu): the selected clips
    /// one step earlier / later as one Undo step; earlier stops at the
    /// song's start.
    pub(super) fn nudge_clips(&mut self, right: bool) {
        if let Some(why) = self.nudge_unavailable(right) {
            self.set_status(why, StatusKind::Info);
            return;
        }
        let step = self.nudge_step();
        if let Some(moves) = self.track_list.nudge_moves(step, right) {
            self.apply_clip_intent(ClipIntent::Move(moves));
        }
    }

    /// Select All while the Track List is shown: every clip it draws.
    pub(super) fn select_all_clips(&mut self) {
        let generation = self.doc_generation();
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        if self.track_list.select_all(&doc, generation) == 0 {
            self.set_status(
                "The Track List shows no clips to select (tracks without notes have none).",
                StatusKind::Info,
            );
        }
    }

    /// Why Zoom to Selection cannot run (`None`: it can). Cheap: counts
    /// only (the menus ask every frame they are open).
    pub(super) fn zoom_selection_unavailable(&self) -> Option<String> {
        self.open.as_ref()?;
        if self.track_list_shown() {
            return self
                .track_list
                .selected
                .is_empty()
                .then(|| "Select clips in the Track List to zoom to them.".to_owned());
        }
        (self.selected_count() == 0)
            .then(|| "Select notes in the Piano Roll to zoom to them.".to_owned())
    }

    /// The selected notes' box on the roll's axis (each clip of a Track
    /// Aggregate at its own place).
    fn note_selection_box(&self) -> Option<(u64, u64, u8, u8)> {
        let doc = self.doc()?;
        let base = self.edit_shift();
        let mut acc: Option<(u64, u64, u8, u8)> = None;
        for h in self.aggregate_hosts() {
            let Ok(v) = plumspirit_engine::editor::note_view(doc, h.target) else {
                continue;
            };
            let Some(b) = v.notes.selection_bounds() else {
                continue;
            };
            let at = |t: u64| (i128::from(t) + h.shift - base).max(0) as u64;
            let (t0, t1) = (at(b.min_start), at(b.max_end));
            acc = Some(match acc {
                None => (t0, t1, b.min_pitch, b.max_pitch),
                Some((a0, a1, lo, hi)) => (
                    a0.min(t0),
                    a1.max(t1),
                    lo.min(b.min_pitch),
                    hi.max(b.max_pitch),
                ),
            });
        }
        acc
    }

    /// Zoom to Selection: the selected clips across the Track List, or the
    /// selected notes across the Piano Roll (Zoom Back returns).
    pub(super) fn zoom_to_selection(&mut self) {
        if let Some(why) = self.zoom_selection_unavailable() {
            self.set_status(why, StatusKind::Info);
            return;
        }
        if self.track_list_shown() {
            let sel = self.track_list.selection();
            let (Some(t0), Some(t1)) = (
                sel.iter().map(|r| r.start).min(),
                sel.iter().map(|r| r.end).max(),
            ) else {
                return;
            };
            self.track_list.zoom_to(t0, t1);
        } else if let Some((t0, t1, lo, hi)) = self.note_selection_box() {
            self.zoom_fit(t0, t1.max(t0 + 1), lo, hi);
        }
    }

    /// Jump to Playhead: the view shown scrolls to the playhead (when it is
    /// off screen) and a paused Follow Playhead resumes.
    pub(super) fn jump_to_playhead(&mut self) {
        let Some(end) = self.doc().map(|d| d.duration_ticks()) else {
            return;
        };
        self.follow_suspended = false;
        if self.track_list_shown() {
            let pos = self.transport.position();
            self.track_list.reveal(pos, end);
        } else {
            self.reveal_playhead();
        }
    }

    /// What a clip key says in the Track List when it has nothing to act
    /// on (`None`: it acted, or it is not a clip key). A Rename / Clip
    /// Color window has the keyboard: nothing to say.
    pub(super) fn clip_key_refusal(&self, c: Command) -> Option<String> {
        if self.track_list.has_window() {
            return None;
        }
        let what = match c {
            Command::Delete => "Delete",
            Command::Cut => "Cut",
            Command::Copy => "Copy",
            Command::SliceNotes => "Slice",
            Command::OpenTool("duplicate") => "Duplicate",
            Command::Paste => {
                return self.clip_clipboard.is_none().then(|| {
                    "Paste in the Track List pastes copied clips: select clips and copy them first."
                        .to_owned()
                });
            }
            _ => return None,
        };
        self.track_list.selected.is_empty().then(|| {
            format!(
                "{what} acts on the clips selected in the Track List: click a clip first (Ctrl+click or Shift+click adds more; {} selects them all).",
                super::keybinds::key_or_menu(Command::SelectAll, "Edit → Select All")
            )
        })
    }
}
