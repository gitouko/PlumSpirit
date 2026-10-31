//! Merge Clips in Sequence / Merge Clips Stacked in the app (v0.9 V09-S12,
//! contract §047 – §049, §218; `docs/CLIP_MERGE.md`): the Track List's
//! selected clips → the merge Preview (a modeless window: the plan's
//! numbers, the destination for Stacked, the subchannel mapping, a dashed
//! outline of the result in the Track List) → Apply (one undo step) or
//! Cancel (nothing). The selection before the merge is remembered with the
//! history state, so Undo restores it and Redo selects the merged clip again.

use super::PlumSpiritApp;
use super::dialogs::Dialog;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::clip_merge::{
    MergeKind, MergePlan, check_merge, merge_clips, plan_merge,
};
use plumspirit_engine::history::StateId;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::{ClipId, PlumDocument};

/// How many remembered clip selections a project keeps (oldest dropped).
const MEMO: usize = 32;

/// The merge Preview window's state.
#[derive(Clone, Debug)]
pub struct MergeDialog {
    pub plan: MergePlan,
    /// The Track List selection when the Preview opened (what Undo restores).
    pub selection: Vec<ClipId>,
    /// Destination choices (Stacked): (row, name).
    pub dests: Vec<(usize, String)>,
    /// The destination picked in the window (the app re-plans when it
    /// differs from the plan's).
    pub want_dest: usize,
    /// Text of the plan, made when it was planned.
    pub span: String,
    pub dest_name: String,
    pub mapping: Vec<String>,
    pub error: Option<String>,
}

impl MergeDialog {
    fn new(
        doc: &PlumDocument,
        plan: MergePlan,
        selection: Vec<ClipId>,
        meter: Option<&plumspirit_engine::view::grid::MeterMap>,
    ) -> MergeDialog {
        let name = |i: usize| doc.track_label(i);
        let pos = |t: u64| {
            meter.map_or_else(
                || format!("tick {}", group_digits(t)),
                |m| m.format_position(t),
            )
        };
        let span = format!(
            "{} – {} ({} ticks)",
            pos(plan.start),
            pos(plan.end),
            group_digits(plan.end - plan.start)
        );
        let sub_name = |track: usize, id: plumspirit_engine::model::SubchannelId| {
            doc.track(track)
                .and_then(|t| t.subchannels().get(id).map(|s| s.display_name()))
                .unwrap_or_else(|| format!("Subchannel {}", id.number()))
        };
        let mapping = plan
            .mapping
            .iter()
            .filter(|m| m.table != plan.dest || m.from != m.to)
            .map(|m| {
                let to = format!("Subchannel {}", m.to.number());
                format!(
                    "{} · {} → {}{}",
                    name(m.table),
                    sub_name(m.table, m.from),
                    to,
                    if m.added { " (new)" } else { "" }
                )
            })
            .collect();
        MergeDialog {
            dests: plan.rows.iter().map(|&r| (r, name(r))).collect(),
            want_dest: plan.dest,
            dest_name: name(plan.dest),
            span,
            mapping,
            plan,
            selection,
            error: None,
        }
    }
}

impl PlumSpiritApp {
    /// Why Merge Clips in Sequence / Stacked cannot run on the Track List's
    /// selection (`None`: it can, or the command is off for another reason —
    /// no project, a job running). The menus show the sentence as the
    /// disabled entry's tooltip; no note is visited (`check_merge`).
    pub(super) fn merge_unavailable(&self, kind: MergeKind) -> Option<String> {
        let o = self.open.as_ref()?;
        if self.main_view != super::track_list::MainView::TrackList {
            return Some(
                "Merging joins clips selected in the Track List: show the Track List (View menu) and select the clips."
                    .into(),
            );
        }
        if self.track_list.selected.len() < 2 {
            return Some("Select two or more clips in the Track List to merge them.".into());
        }
        let ids: Vec<ClipId> = self.track_list.selected.iter().copied().collect();
        check_merge(&o.doc, &ids, kind).err().map(|e| e.0)
    }

    /// Merge Clips in Sequence / Stacked (Ctrl+J / Ctrl+Alt+J, menus): plans
    /// the merge of the Track List's selected clips and opens the Preview,
    /// or says in the status bar why it cannot.
    pub(super) fn start_clip_merge(&mut self, kind: MergeKind) {
        if let Some(why) = self.merge_unavailable(kind) {
            self.set_status(why, StatusKind::Warning);
            return;
        }
        let ids: Vec<ClipId> = self.track_list.selected.iter().copied().collect();
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        match plan_merge(&doc, &ids, kind, None) {
            Ok(plan) => {
                let meter = self.meter_map().cloned();
                self.dialog = Some(Dialog::MergeClips(Box::new(MergeDialog::new(
                    &doc,
                    plan,
                    ids,
                    meter.as_ref(),
                ))));
            }
            Err(e) => self.set_status(e.to_string(), StatusKind::Warning),
        }
    }

    /// The destination picked in the Preview changed: plan again.
    pub(super) fn replan_clip_merge(&mut self) {
        let Some(Dialog::MergeClips(d)) = self.dialog.as_ref() else {
            return;
        };
        if d.want_dest == d.plan.dest {
            return;
        }
        let (ids, kind, dest, sel): (Vec<ClipId>, MergeKind, usize, Vec<ClipId>) = (
            d.plan.sources.iter().map(|s| s.id).collect(),
            d.plan.kind,
            d.want_dest,
            d.selection.clone(),
        );
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let meter = self.meter_map().cloned();
        match plan_merge(&doc, &ids, kind, Some(dest)) {
            Ok(plan) => {
                self.dialog = Some(Dialog::MergeClips(Box::new(MergeDialog::new(
                    &doc,
                    plan,
                    sel,
                    meter.as_ref(),
                ))));
            }
            Err(e) => {
                if let Some(Dialog::MergeClips(d)) = self.dialog.as_mut() {
                    d.want_dest = d.plan.dest;
                    d.error = Some(e.to_string());
                }
            }
        }
    }

    /// Apply in the Preview: one undo step; the merged clip becomes the
    /// selection; the selection before is remembered for Undo (§218).
    /// `Err`: the sentence for the Preview (the clips changed meanwhile).
    pub(super) fn apply_clip_merge(&mut self) -> Result<(), String> {
        let Some(Dialog::MergeClips(d)) = self.dialog.as_ref() else {
            return Ok(());
        };
        let plan = d.plan.clone();
        let before_sel = d.selection.clone();
        let Some(o) = self.open.as_ref() else {
            return Ok(());
        };
        let doc = o.doc.clone();
        let before = o.history.current_state();
        let new_id = ClipId::Placed(doc.clips().next_ids().0);
        let op = merge_clips(&doc, &plan).map_err(|e| e.to_string())?;
        drop(doc);
        self.apply_op(op)
            .ok_or_else(|| "The clips were not merged.".to_string())?;
        self.track_list.select_only(new_id);
        if let Some(o) = self.open.as_mut() {
            let after = o.history.current_state();
            remember(&mut o.clip_selections, before, before_sel);
            remember(&mut o.clip_selections, after, vec![new_id]);
        }
        self.select_track(plan.dest);
        self.set_status(
            format!(
                "Merged {} clips into one on {} ({} notes).",
                plan.sources.len(),
                self.track_display_name(plan.dest),
                group_digits(plan.notes as u64)
            ),
            StatusKind::Info,
        );
        Ok(())
    }

    /// After Undo / Redo: the Track List selection remembered for the state
    /// the history is now in, if any (§218: Undo of a merge restores the
    /// original selection exactly).
    pub(super) fn restore_clip_selection(&mut self) {
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let state = o.history.current_state();
        if let Some((_, sel)) = o.clip_selections.iter().rev().find(|(s, _)| *s == state) {
            self.track_list.selected = sel.iter().copied().collect();
        }
    }
}

/// Remembers `sel` for history state `state` (the newest entry wins).
pub(super) fn remember(memo: &mut Vec<(StateId, Vec<ClipId>)>, state: StateId, sel: Vec<ClipId>) {
    memo.retain(|(s, _)| *s != state);
    memo.push((state, sel));
    if memo.len() > MEMO {
        memo.remove(0);
    }
}

#[cfg(test)]
mod tests {
    //! The merge commands on the app harness: each command from the Track
    //! List selection, its refusal sentence, the Preview (numbers,
    //! destination choice), Apply as one undo step with the merged clip
    //! selected, Undo restoring the original selection exactly, Redo the
    //! merged clip, and Cancel changing nothing.
    use super::*;
    use crate::app::commands::Command;
    use crate::app::dialogs::DialogOutcome;
    use crate::app::test_support::Harness;
    use crate::app::track_list::{ClipIntent, MainView};
    use plumspirit_engine::model::{Bpm, ContentRef, NoteAttrs, NoteOffKind, NoteRecord};

    fn note(t: u64, p: u8) -> NoteRecord {
        NoteRecord::new(
            t,
            200,
            NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        )
    }

    /// Three tracks with four notes each.
    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("merge".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        d.add_track().unwrap();
        for i in 0..3 {
            let recs: Vec<NoteRecord> = (0..4u64).map(|k| note(k * 960, 60 + i as u8)).collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    fn outcome(h: &mut Harness, o: DialogOutcome) {
        h.dialog_outcome(o);
    }

    #[test]
    fn merge_in_sequence_previews_applies_and_undo_restores_the_selection() {
        let mut h = Harness::new(song());
        let t0 = ClipId::Primary(h.id(0));
        // A second clip on row 0 (an independent copy at bar 6).
        h.clip(ClipIntent::Duplicate {
            copies: vec![(t0, 0, 9_600)],
            linked: false,
        });
        let copy = ClipId::Placed(0);
        h.set_clips(&[t0, copy]);
        let fp = h.doc().state_fingerprint();
        assert!(h.app.enabled(Command::MergeClips(MergeKind::Sequence)));
        h.chord(Command::MergeClips(MergeKind::Sequence));
        let Some(Dialog::MergeClips(d)) = h.app.dialog.as_ref() else {
            panic!("the Preview opens: {}", h.status());
        };
        assert_eq!(d.plan.notes, 8);
        assert_eq!(d.dests.len(), 1);
        assert!(d.mapping.is_empty(), "one table: no mapping rows");
        // Cancel: nothing changes.
        outcome(&mut h, DialogOutcome::Closed);
        assert!(h.app.dialog.is_none());
        assert_eq!(h.doc().state_fingerprint(), fp);
        // Again, Apply.
        h.chord(Command::MergeClips(MergeKind::Sequence));
        outcome(&mut h, DialogOutcome::ClipMergeApply);
        assert!(h.app.dialog.is_none(), "{}", h.status());
        let merged = ClipId::Placed(1);
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![merged]
        );
        assert!(h.doc().clips().placement(t0).is_none());
        assert!(h.doc().clips().placement(copy).is_none());
        assert_eq!(
            h.app.open.as_ref().unwrap().history.undo_label(),
            Some("Merge Clips in Sequence")
        );
        // Undo: the two clips and the selection, exactly.
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().state_fingerprint(), fp);
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![t0, copy]
        );
        assert!(h.command(Command::Redo));
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![merged]
        );
    }

    #[test]
    fn merge_stacked_offers_each_row_and_maps_into_the_one_picked() {
        let mut h = Harness::new(song());
        let ids: Vec<ClipId> = (0..3).map(|i| ClipId::Primary(h.id(i))).collect();
        h.set_clips(&ids);
        h.chord(Command::MergeClips(MergeKind::Stack));
        let Some(Dialog::MergeClips(d)) = h.app.dialog.as_mut() else {
            panic!("the Preview opens: {}", h.status());
        };
        assert_eq!(
            d.dests.iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(d.plan.dest, 0);
        assert_eq!(d.mapping.len(), 2, "tracks 2 and 3: main subchannel → new");
        assert!(d.mapping[0].contains("(new)"), "{:?}", d.mapping);
        d.want_dest = 2;
        h.app.replan_clip_merge();
        let Some(Dialog::MergeClips(d)) = h.app.dialog.as_ref() else {
            panic!("still open");
        };
        assert_eq!(
            (d.plan.dest, d.dest_name.as_str()),
            (2, h.app.track_display_name(2).as_str())
        );
        outcome(&mut h, DialogOutcome::ClipMergeApply);
        let merged = h.doc().clips().explicit_placements()[0].clone();
        assert_eq!(merged.track, h.id(2));
        assert!(matches!(merged.content, ContentRef::Owned(_)));
        assert_eq!(h.app.active_track(), 2);
        assert_eq!(h.app.main_view, MainView::TrackList);
    }

    #[test]
    fn the_merge_commands_say_why_they_cannot_run() {
        let mut h = Harness::new(song());
        let ids: Vec<ClipId> = (0..2).map(|i| ClipId::Primary(h.id(i))).collect();
        // One clip: Ctrl+J says why, nothing opens.
        h.set_clips(&ids[..1]);
        h.chord(Command::MergeClips(MergeKind::Sequence));
        assert!(h.app.dialog.is_none());
        assert!(h.status().contains("two or more clips"), "{}", h.status());
        // Two rows in Sequence: a sentence, no Preview, nothing changed.
        h.set_clips(&ids);
        let fp = h.doc().state_fingerprint();
        h.chord(Command::MergeClips(MergeKind::Sequence));
        assert!(h.app.dialog.is_none());
        assert!(h.status().contains("single row"), "{}", h.status());
        assert_eq!(h.doc().state_fingerprint(), fp);
        // In the Piano Roll the Track List selection is not acted on.
        h.app.main_view = MainView::PianoRoll;
        assert!(!h.app.enabled(Command::MergeClips(MergeKind::Stack)));
    }

    /// The Preview window itself (headless): it shows the plan's numbers —
    /// timing, the destination choice, notes, events, memory, the mapping
    /// with its new subchannels — and its buttons, and stays open by itself.
    #[test]
    fn the_preview_window_shows_the_plans_numbers() {
        let mut h = Harness::new(song());
        let ids: Vec<ClipId> = (0..3).map(|i| ClipId::Primary(h.id(i))).collect();
        h.set_clips(&ids);
        h.chord(Command::MergeClips(MergeKind::Stack));
        let mut dialog = h.app.dialog.take().expect("the Preview opens");
        let ctx = egui::Context::default();
        let mut texts: Vec<String> = Vec::new();
        for _ in 0..2 {
            let mut outcome = DialogOutcome::Closed;
            let mut o = ctx.run_ui(egui::RawInput::default(), |ui| {
                outcome = crate::app::dialogs::show(&mut dialog, ui.ctx(), false);
            });
            o.textures_delta.clear();
            assert!(matches!(outcome, DialogOutcome::Open));
            fn collect(s: &egui::Shape, out: &mut Vec<String>) {
                match s {
                    egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                    _ => {}
                }
            }
            texts.clear();
            o.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        }
        for want in [
            "Merge Clips Stacked",
            "Timing",
            "Destination",
            "Notes",
            "12",
            "Events",
            "Memory",
            "Merge",
            "Cancel",
        ] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }
        assert!(
            texts.iter().any(|t| t.starts_with("Subchannels: 2 mapped")),
            "{texts:?}"
        );
        assert_eq!(
            texts.iter().filter(|t| t.ends_with("(new)")).count(),
            2,
            "{texts:?}"
        );
    }

    /// The document changed under the Preview: Apply refuses with a sentence
    /// in the window, changing nothing.
    #[test]
    fn apply_refuses_a_stale_preview() {
        let mut h = Harness::new(song());
        let ids: Vec<ClipId> = (0..2).map(|i| ClipId::Primary(h.id(i))).collect();
        h.set_clips(&ids);
        h.chord(Command::MergeClips(MergeKind::Stack));
        assert!(matches!(h.app.dialog, Some(Dialog::MergeClips(_))));
        h.app
            .doc_mut()
            .unwrap()
            .track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(10, 90)], false)
            .unwrap();
        let fp = h.doc().state_fingerprint();
        outcome(&mut h, DialogOutcome::ClipMergeApply);
        let Some(Dialog::MergeClips(d)) = h.app.dialog.as_ref() else {
            panic!("stays open");
        };
        assert!(d.error.as_deref().unwrap_or("").contains("changed"));
        assert_eq!(h.doc().state_fingerprint(), fp);
    }

    /// v1.0-beta1 B1-0096 (owner report, image10 / image14): after a merge
    /// the track's primary clip is gone and its old notes stay only for
    /// Undo; picking the track must show the merged content — not the stale
    /// pre-merge notes — and the panel must count the merged clip.
    #[test]
    fn after_a_merge_the_track_shows_and_counts_the_merged_content() {
        use plumspirit_engine::editor::NoteTarget;
        let mut h = Harness::new(song());
        let t0 = ClipId::Primary(h.id(0));
        h.clip(ClipIntent::Duplicate {
            copies: vec![(t0, 0, 9_600)],
            linked: false,
        });
        let copy = ClipId::Placed(0);
        h.set_clips(&[t0, copy]);
        h.chord(Command::MergeClips(MergeKind::Sequence));
        outcome(&mut h, DialogOutcome::ClipMergeApply);
        assert!(h.app.dialog.is_none(), "{}", h.status());
        let merged = ClipId::Placed(1);
        let ContentRef::Owned(c) = h.doc().clips().placement(merged).unwrap().content else {
            panic!("a merge makes an owned content");
        };
        // Pick another track, then the merged track again (as the owner did).
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Track(1),
            "track 2 is the identity"
        );
        h.set_track(0);
        h.app.sync_clip_session();
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Content {
                host: 0,
                content: c
            },
            "the roll shows the merged content"
        );
        assert_eq!(h.app.edit_notes().unwrap().len(), 8);
        assert_eq!(h.app.edit_shift(), 0);
        assert!(h.app.session_title().is_some(), "the roll names the clip");
        // The stale own notes still exist (for Undo) but are not shown.
        assert_eq!(h.doc().track(0).unwrap().notes.len(), 4);
        assert_eq!(h.doc().placed_note_count(0), 8);
        // Undo: the track's own notes again, at the identity.
        assert!(h.command(Command::Undo));
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        assert_eq!(h.doc().placed_note_count(0), 8, "own 4 + the copy's 4");
        assert!(h.command(Command::Redo));
        h.app.sync_clip_session();
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Content {
                host: 0,
                content: c
            }
        );
    }

    /// The same for a merge on a subtrack whose own content is empty: the
    /// subtrack shows the merged clip it hosts.
    #[test]
    fn a_subtrack_shows_the_merged_clip_it_hosts() {
        use plumspirit_engine::editor::NoteTarget;
        use plumspirit_engine::history::Operation;
        let mut d = song();
        let mut hist = plumspirit_engine::history::History::default();
        hist.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 1,
            },
        )
        .unwrap();
        let mut h = Harness::new(d);
        // Track 1's clip copied twice onto its (empty) subtrack, then merged.
        let t0 = ClipId::Primary(h.id(0));
        h.clip(ClipIntent::Duplicate {
            copies: vec![(t0, 1, 0), (t0, 1, 9_600)],
            linked: false,
        });
        h.set_clips(&[ClipId::Placed(0), ClipId::Placed(1)]);
        h.chord(Command::MergeClips(MergeKind::Sequence));
        outcome(&mut h, DialogOutcome::ClipMergeApply);
        assert!(h.app.dialog.is_none(), "{}", h.status());
        let p = h.doc().clips().placement(ClipId::Placed(2)).unwrap();
        let ContentRef::Owned(c) = p.content else {
            panic!("owned");
        };
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Content {
                host: 1,
                content: c
            }
        );
        assert_eq!(h.app.edit_notes().unwrap().len(), 8);
        assert_eq!(h.doc().placed_note_count(1), 8);
        assert_eq!(h.doc().track(1).unwrap().notes.len(), 0);
    }
}
