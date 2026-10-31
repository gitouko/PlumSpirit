//! The context matrix (v0.9 §043 / §281, V09-S10, `docs/v0_9/CONTEXT_MATRIX.md`):
//! every context-sensitive action, run from each context where it applies,
//! through the entry point its menu, button or key uses (test_support).
//!
//! The fixture: track 0 (with subtrack 1), track 2 (with a linked copy of
//! its clip), track 3 (in folder "Drums"), track 4 (empty).

use super::commands::Command;
use super::dialogs::Dialog;
use super::test_support::Harness;
use super::track_list::{ClipIntent, MainView};
use crate::ui::track_panel::{ConductorAdd, TrackAction};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::project::Bpm;
use plumspirit_engine::model::{
    ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TrackId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    Conductor,
    Track,
    Subtrack,
    /// A track in a folder (the folder header's own menu is tested apart).
    FolderMember,
    Clip,
    Tracks,
    Clips,
    Notes,
    Nothing,
}

const ALL: [Ctx; 9] = [
    Ctx::Conductor,
    Ctx::Track,
    Ctx::Subtrack,
    Ctx::FolderMember,
    Ctx::Clip,
    Ctx::Tracks,
    Ctx::Clips,
    Ctx::Notes,
    Ctx::Nothing,
];

fn note(s: u64, p: u8) -> NoteRecord {
    NoteRecord {
        start_tick: s,
        duration_ticks: 200,
        attrs: NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

const FOLDER: &str = "Drums";

pub(super) fn fixture() -> PlumDocument {
    let mut d = PlumDocument::new_project("matrix".into(), Bpm::DEFAULT, 480);
    for _ in 0..3 {
        d.add_track().unwrap();
    }
    let mut h = History::new(HistoryLimits::default());
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    assert_eq!(d.track_count(), 5);
    for i in 0..4 {
        let recs: Vec<NoteRecord> = (0..8u64).map(|k| note(k * 240, 60 + i as u8)).collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let t3 = d.track(3).unwrap().id();
    let f = d.project_mut().folders.create(FOLDER, Some(t3));
    d.project_mut().folders.set_member(t3, f);
    let t2 = d.track(2).unwrap().id();
    let op = plumspirit_engine::editor::clips::duplicate_clips(
        &d,
        &[(ClipId::Primary(t2), 2, 3840)],
        true,
    )
    .unwrap();
    h.apply(&mut d, op).unwrap();
    d
}

/// The linked copy on track 2 and track 2's own clip.
fn clips(h: &Harness) -> (ClipId, ClipId) {
    let t2 = h.id(2);
    let placed = h
        .doc()
        .clips()
        .layout()
        .placements
        .iter()
        .find(|p| p.track == t2 && p.start == 3840)
        .map(|p| p.id)
        .expect("the linked copy");
    (ClipId::Primary(t2), placed)
}

fn enter(h: &mut Harness, c: Ctx) {
    let (primary, placed) = clips(h);
    match c {
        Ctx::Conductor => h.set_conductor(),
        Ctx::Track => h.set_track(2),
        Ctx::Subtrack => h.set_track(1),
        Ctx::FolderMember => h.set_track(3),
        Ctx::Clip => h.set_clips(&[placed]),
        Ctx::Tracks => h.set_tracks(&[2, 4]),
        Ctx::Clips => h.set_clips(&[primary, placed]),
        Ctx::Notes => h.set_notes(2),
        Ctx::Nothing => {
            h.set_track(2);
            h.set_nothing();
        }
    }
    h.app.main_view = if c == Ctx::Notes {
        MainView::PianoRoll
    } else {
        MainView::TrackList
    };
}

/// The track a context acts on with the active-track buttons and keys
/// (`None`: the Conductor).
fn active_of(c: Ctx) -> Option<usize> {
    match c {
        Ctx::Conductor => None,
        Ctx::Subtrack => Some(1),
        Ctx::FolderMember => Some(3),
        _ => Some(2),
    }
}

/// Track `i`'s parent index.
fn parent(h: &Harness, i: usize) -> Option<usize> {
    let p = h.doc().track(i)?.parent()?;
    h.doc().tracks().iter().position(|t| t.id() == p)
}

/// Track `i`'s level (0: top-level).
fn depth(h: &Harness, i: usize) -> usize {
    let mut d = 0;
    let mut k = i;
    while let Some(p) = parent(h, k) {
        d += 1;
        k = p;
    }
    d
}

fn ids(h: &Harness) -> Vec<TrackId> {
    h.doc().tracks().iter().map(|t| t.id()).collect()
}

fn undo(h: &mut Harness) {
    assert!(h.command(Command::Undo), "undo enabled");
}

/// §043's example: Add Track works from every context (the panel's button,
/// Ctrl+Shift+N, the Song / Conductor / empty-space menus all run
/// `Command::AddTrack`); the new track is the active one — also from the
/// Conductor, which it leaves.
#[test]
fn add_track_works_from_every_context() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let before = ids(&h);
        h.track_action(TrackAction::Add);
        let after = ids(&h);
        assert_eq!(after.len(), before.len() + 1, "{c:?}");
        assert_eq!(&after[..before.len()], &before[..], "{c:?}: appended");
        assert!(
            !h.app.conductor_selected(),
            "{c:?}: the new track is active"
        );
        assert_eq!(h.app.active_track(), before.len(), "{c:?}");
        undo(&mut h);
        assert_eq!(ids(&h), before, "{c:?}: one undo step");
    }
}

/// §043's example: Delete Track (the panel's button / Song menu) works from
/// every track context — the active track, or the selection it belongs to —
/// and refuses the Conductor with a message.
#[test]
fn delete_track_works_from_every_context_and_refuses_the_conductor() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let before = ids(&h);
        h.track_action(TrackAction::Delete);
        match c {
            Ctx::Conductor => {
                assert!(
                    matches!(h.app.dialog, Some(Dialog::Message { .. })),
                    "{c:?}"
                );
                assert_eq!(ids(&h), before, "{c:?}");
                continue;
            }
            _ => {
                assert!(h.confirm_delete().is_some(), "{c:?}: confirmation");
            }
        }
        let gone: Vec<TrackId> = match c {
            Ctx::Tracks => vec![before[2], before[4]],
            Ctx::Subtrack => vec![before[1]],
            Ctx::FolderMember => vec![before[3]],
            _ => vec![before[2]],
        };
        let want: Vec<TrackId> = before
            .iter()
            .copied()
            .filter(|t| !gone.contains(t))
            .collect();
        assert_eq!(ids(&h), want, "{c:?}");
        undo(&mut h);
        assert_eq!(ids(&h), before, "{c:?}: one undo step");
    }
}

/// The track menu's Delete Track(s) (panel row, Track List row, clip menu
/// Track ▸ — one builder, one action) acts on the clicked track or the
/// selection it belongs to; a parent takes its subtracks.
#[test]
fn the_track_menus_delete_acts_on_the_clicked_track_or_its_selection() {
    for (c, clicked, gone) in [
        (Ctx::Track, 0, vec![0, 1]),
        (Ctx::Subtrack, 1, vec![1]),
        (Ctx::FolderMember, 3, vec![3]),
        (Ctx::Tracks, 4, vec![2, 4]),
        (Ctx::Clip, 2, vec![2]),
        (Ctx::Clips, 4, vec![4]),
        (Ctx::Conductor, 2, vec![2]),
        (Ctx::Nothing, 4, vec![4]),
    ] {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let before = ids(&h);
        h.track_action(TrackAction::DeleteTracks(clicked));
        let _ = h.confirm_delete();
        let want: Vec<TrackId> = (0..before.len())
            .filter(|k| !gone.contains(k))
            .map(|k| before[k])
            .collect();
        assert_eq!(ids(&h), want, "{c:?} clicked {clicked}");
    }
}

/// Add Track Below from every track context (the clicked row's level and
/// folder), Add Subtrack under the clicked track.
#[test]
fn add_track_below_and_add_subtrack_work_from_every_track_context() {
    for (c, clicked) in [
        (Ctx::Track, 0),
        (Ctx::Subtrack, 1),
        (Ctx::FolderMember, 3),
        (Ctx::Tracks, 4),
        (Ctx::Clip, 2),
        (Ctx::Notes, 2),
        (Ctx::Conductor, 2),
    ] {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let n = h.doc().track_count();
        let end = h.doc().subtree_end(clicked);
        let level = depth(&h, clicked);
        let folder = h.doc().project().folders.folder_of(h.id(clicked));
        h.track_action(TrackAction::AddBelow(clicked));
        assert_eq!(h.doc().track_count(), n + 1, "{c:?}");
        assert_eq!(h.app.active_track(), end, "{c:?}: the new track is active");
        assert_eq!(depth(&h, end), level, "{c:?}: same level");
        assert_eq!(
            h.doc().project().folders.folder_of(h.id(end)),
            folder,
            "{c:?}: same folder"
        );
        undo(&mut h);
        h.track_action(TrackAction::AddSubtrack(clicked));
        assert_eq!(h.doc().track_count(), n + 1, "{c:?}");
        let sub = h.doc().subtree_end(clicked) - 1;
        assert_eq!(parent(&h, sub), Some(clicked), "{c:?}: a subtrack");
    }
}

/// §041: Add Piano Roll Clip from the track menu (on the clicked row) and
/// Insert / Song menu (on the active track) — one owned clip, one bar at
/// the edit cursor, selected, one undo step; from the Conductor a sentence
/// and no edit. Insert shows the Track List (where the clip is).
#[test]
fn add_piano_roll_clip_works_from_every_track_context() {
    let check = |h: &mut Harness, track: usize, c: Ctx| {
        let layout = h.doc().clips().layout();
        let added: Vec<_> = layout
            .placements
            .iter()
            .filter(|p| matches!(p.content, ContentRef::Owned(_)))
            .collect();
        assert_eq!(added.len(), 1, "{c:?}");
        let p = added[0];
        assert_eq!(p.track, h.id(track), "{c:?}");
        assert_eq!((p.start, p.length), (0, Some(1920)), "{c:?}");
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![p.id],
            "{c:?}: the new clip is selected"
        );
        assert_eq!(h.app.active_track(), track, "{c:?}");
        undo(h);
        assert!(
            !h.doc()
                .clips()
                .layout()
                .placements
                .iter()
                .any(|p| matches!(p.content, ContentRef::Owned(_))),
            "{c:?}: one undo step"
        );
    };
    for c in ALL {
        // Insert / Song menu: the active track.
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        h.chord(Command::AddClip);
        match active_of(c) {
            None => {
                assert!(h.status().contains("Conductor holds no notes"), "{c:?}");
                assert!(
                    !h.doc()
                        .clips()
                        .layout()
                        .placements
                        .iter()
                        .any(|p| matches!(p.content, ContentRef::Owned(_))),
                    "{c:?}"
                );
            }
            Some(t) => {
                assert_eq!(h.app.main_view, MainView::TrackList, "{c:?}");
                check(&mut h, t, c);
            }
        }
        // The track menu: the clicked row.
        let clicked = match c {
            Ctx::Tracks => 4,
            Ctx::Subtrack => 1,
            Ctx::FolderMember => 3,
            _ => 2,
        };
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        h.track_action(TrackAction::AddClip(clicked));
        check(&mut h, clicked, c);
    }
}

/// Mute / Solo from the track menu: the clicked track, or the selection it
/// belongs to (TT).
#[test]
fn mute_and_solo_act_on_the_clicked_track_or_its_selection() {
    for (c, act, muted) in [
        (Ctx::Track, TrackAction::ToggleMute(2), vec![2]),
        (Ctx::Subtrack, TrackAction::ToggleMute(1), vec![1]),
        (Ctx::FolderMember, TrackAction::ToggleMute(3), vec![3]),
        (Ctx::Tracks, TrackAction::MuteSelection(4, true), vec![2, 4]),
        (Ctx::Clip, TrackAction::ToggleMute(2), vec![2]),
    ] {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        h.track_action(act);
        let now: Vec<usize> = (0..h.doc().track_count())
            .filter(|&i| h.doc().tracks()[i].mute)
            .collect();
        assert_eq!(now, muted, "{c:?}");
        h.track_action(TrackAction::PlayAll);
        assert!(h.doc().tracks().iter().all(|t| !t.mute), "{c:?}");
    }
    let mut h = Harness::new(fixture());
    enter(&mut h, Ctx::Tracks);
    h.track_action(TrackAction::SoloSelection(2));
    let soloed: Vec<usize> = (0..5).filter(|&i| h.doc().tracks()[i].solo).collect();
    assert_eq!(soloed, vec![2, 4]);
}

/// Clip contexts (K, KK): Delete and Ctrl+D act on the selected clips while
/// the Track List is shown; with nothing selected they do not touch clips.
#[test]
fn clip_keys_act_on_the_selected_clips() {
    for c in [Ctx::Clip, Ctx::Clips, Ctx::Nothing] {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let n = h.doc().clips().layout().placements.len();
        let sel = h.app.track_list.selected.len();
        h.chord(Command::OpenTool("duplicate"));
        let after = h.doc().clips().layout().placements.len();
        assert_eq!(after, n + sel, "{c:?}: one copy per selected clip");
        assert_eq!(
            h.app.track_list.selected.len(),
            sel,
            "{c:?}: copies selected"
        );
        undo_if(&mut h, sel > 0);
        let primaries_shown = |h: &Harness| {
            plumspirit_engine::view::track_list::TrackListModel::build(h.doc())
                .row(2)
                .len()
        };
        let shown = primaries_shown(&h);
        enter(&mut h, c);
        // Mute (the clip menu's intent) takes the selection.
        let sel_ids: Vec<ClipId> = h.app.track_list.selected.iter().copied().collect();
        h.clip(ClipIntent::Mute(sel_ids.clone(), true));
        let muted = plumspirit_engine::view::track_list::TrackListModel::build(h.doc())
            .row(2)
            .iter()
            .filter(|r| r.muted)
            .count();
        assert_eq!(muted, sel_ids.len(), "{c:?}");
        undo_if(&mut h, !sel_ids.is_empty());
        enter(&mut h, c);
        h.chord(Command::Delete);
        assert_eq!(primaries_shown(&h), shown - sel, "{c:?}");
        assert!(h.app.dialog.is_none(), "{c:?}: no track deleted");
    }
}

fn undo_if(h: &mut Harness, yes: bool) {
    if yes {
        undo(h);
    }
}

/// The Conductor's adds: from the Conductor menu (row: edit position; lane:
/// the clicked tick) and the Song menu in every context — each opens its
/// dialog.
#[test]
fn conductor_adds_open_their_dialogs_from_every_context() {
    for c in ALL {
        for (what, at) in [
            (ConductorAdd::Marker, Some(960)),
            (ConductorAdd::TimeSignature, None),
            (ConductorAdd::Tempo, Some(1920)),
        ] {
            let mut h = Harness::new(fixture());
            enter(&mut h, c);
            h.track_action(TrackAction::ConductorAdd(what, at));
            let ok = matches!(
                (&h.app.dialog, what),
                (Some(Dialog::Marker { .. }), ConductorAdd::Marker)
                    | (
                        Some(Dialog::TimeSignature { .. }),
                        ConductorAdd::TimeSignature
                    )
                    | (Some(Dialog::Tempo { .. }), ConductorAdd::Tempo)
            );
            assert!(ok, "{c:?} {what:?}: {:?}", h.app.dialog.is_some());
        }
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        assert!(h.command(Command::AddMarker), "{c:?}: Song menu");
        assert!(matches!(h.app.dialog, Some(Dialog::Marker { .. })), "{c:?}");
    }
}

/// The folder header's menu and the empty-space menu.
#[test]
fn folder_and_empty_space_actions_work() {
    let mut h = Harness::new(fixture());
    enter(&mut h, Ctx::Nothing);
    let f = h.doc().project().folders.folder_of(h.id(3));
    assert_ne!(f, 0);
    let n = h.doc().track_count();
    h.track_action(TrackAction::FolderAddTrack(f));
    assert_eq!(h.doc().track_count(), n + 1);
    let new = h.app.active_track();
    assert_eq!(h.doc().project().folders.folder_of(h.id(new)), f);
    let folders = h.doc().project().folders.folders().len();
    h.track_action(TrackAction::FolderNew(None));
    assert_eq!(h.doc().project().folders.folders().len(), folders + 1);
    h.app.folder_rename = None;
    // Delete Folder and Its Tracks: confirmed, one step.
    let before = ids(&h);
    h.track_action(TrackAction::FolderDeleteWithTracks(f));
    assert!(h.confirm_delete().is_some());
    assert_eq!(h.doc().track_count(), before.len() - 2);
}

/// Note commands from the note menu / Edit menu need a selection (N); with
/// nothing selected Copy / Cut / Delete are not offered.
#[test]
fn note_commands_need_a_note_selection() {
    let mut h = Harness::new(fixture());
    enter(&mut h, Ctx::Notes);
    assert!(h.app.enabled(Command::Copy));
    assert!(h.app.enabled(Command::Cut));
    enter(&mut h, Ctx::Nothing);
    h.app.main_view = MainView::PianoRoll;
    assert!(!h.app.enabled(Command::Copy));
    assert!(!h.app.enabled(Command::Cut));
}

/// V09-S12 (§047, §218): the two merges act on several selected clips (KK)
/// in the Track List. From KK, Merge Clips in Sequence (the fixture's two
/// clips share track 2's row) opens the Preview, Apply is one undo step and
/// Undo restores the clips and the selection exactly; Stacked says why not.
/// In every other context both are disabled with a sentence (the menus'
/// tooltip) and Ctrl+J / Ctrl+Alt+J change nothing.
#[test]
fn the_clip_merges_need_several_clips_and_say_why_everywhere_else() {
    use plumspirit_engine::editor::clip_merge::MergeKind;
    let (seq, stack) = (
        Command::MergeClips(MergeKind::Sequence),
        Command::MergeClips(MergeKind::Stack),
    );
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let fp = h.doc().state_fingerprint();
        let sel: Vec<ClipId> = h.app.track_list.selected.iter().copied().collect();
        let why_stack = h.app.why_disabled(stack).unwrap_or_default();
        if c == Ctx::Clips {
            assert_eq!(h.app.why_disabled(seq), None);
            assert!(why_stack.contains("Merge Clips in Sequence"), "{why_stack}");
            h.chord(stack);
            assert!(h.app.dialog.is_none(), "Stacked refused");
            h.chord(seq);
            assert!(
                matches!(h.app.dialog, Some(Dialog::MergeClips(_))),
                "{c:?}: {}",
                h.status()
            );
            h.dialog_outcome(super::dialogs::DialogOutcome::ClipMergeApply);
            assert!(h.app.dialog.is_none(), "{}", h.status());
            assert_eq!(h.app.track_list.selected.len(), 1, "the merged clip");
            assert_ne!(h.doc().state_fingerprint(), fp);
            undo(&mut h);
            assert_eq!(h.doc().state_fingerprint(), fp);
            let back: Vec<ClipId> = h.app.track_list.selected.iter().copied().collect();
            assert_eq!(back, sel, "Undo restores the selection");
            continue;
        }
        let why_seq = h.app.why_disabled(seq).unwrap_or_default();
        let want = if c == Ctx::Notes {
            "show the Track List"
        } else {
            "two or more clips"
        };
        for why in [&why_seq, &why_stack] {
            assert!(why.contains(want), "{c:?}: {why}");
        }
        h.chord(seq);
        h.chord(stack);
        assert!(h.app.dialog.is_none(), "{c:?}");
        assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
        // In the Track List the chord says why (the Piano Roll leaves
        // Track List commands alone).
        if c != Ctx::Notes {
            assert!(h.status().contains(want), "{c:?}: {}", h.status());
        }
    }
}

/// V09-S13 (§088, §198): Paste Repeat (Ctrl+B) runs on several or one
/// selected clip (the dialog) and on a note selection (the notes tool); in
/// every other context it says why — in the menus' tooltip and in the
/// status bar for the chord — and changes nothing.
#[test]
fn paste_repeat_runs_on_clips_or_notes_and_says_why_everywhere_else() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let fp = h.doc().state_fingerprint();
        let why = h.app.why_disabled(Command::PasteRepeat);
        h.chord(Command::PasteRepeat);
        match c {
            Ctx::Clip | Ctx::Clips => {
                assert_eq!(why, None, "{c:?}");
                assert!(
                    matches!(h.app.dialog, Some(Dialog::RepeatClips(_))),
                    "{c:?}: {}",
                    h.status()
                );
                h.dialog_outcome(super::dialogs::DialogOutcome::ClipRepeatApply);
                assert!(h.app.dialog.is_none(), "{c:?}: {}", h.status());
                assert_ne!(h.doc().state_fingerprint(), fp);
                undo(&mut h);
                assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
            }
            Ctx::Notes => {
                assert_eq!(why, None);
                assert_eq!(
                    h.app.tool.as_ref().map(|t| t.def.id),
                    Some("paste_repeat_notes")
                );
            }
            _ => {
                let w = why.unwrap_or_default();
                assert!(w.contains("Select the clips to paste again"), "{c:?}: {w}");
                assert!(
                    h.status().contains("Select the clips"),
                    "{c:?}: {}",
                    h.status()
                );
                assert!(h.app.dialog.is_none() && h.app.tool.is_none(), "{c:?}");
                assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
            }
        }
    }
}

/// B1-0064: Onion Skin from the colour boxes, the toolbar button and the
/// keyboard: one project setting outside undo that marks the project unsaved
/// and is saved in the .lup; Track, Subtrack and Folder-member tracks alike.
#[test]
fn onion_tracks_are_set_from_boxes_toolbar_and_keys_and_saved() {
    use plumspirit_engine::model::project::OnionMode;
    let mut h = Harness::new(fixture());
    let id = |h: &Harness, i: usize| h.id(i);
    h.set_track(2);
    assert_eq!(h.doc().project().onion.mode, OnionMode::Off);
    // Colour boxes: the subtrack (1) and the folder member (3).
    h.track_action(TrackAction::OnionSet(vec![1, 3], true));
    let o = h.doc().project().onion.clone();
    assert_eq!(o.mode, OnionMode::Specific);
    assert_eq!(o.specific, vec![id(&h, 1), id(&h, 3)]);
    assert!(
        h.app.open.as_ref().unwrap().dirty(),
        "unsaved, outside undo"
    );
    assert!(!h.app.open.as_ref().unwrap().history.can_undo());
    // The toolbar button / Ctrl+Shift+O: off, then back to the list.
    assert!(h.command(Command::ToggleOnion));
    assert_eq!(h.doc().project().onion.mode, OnionMode::Off);
    assert_eq!(h.doc().project().onion.specific.len(), 2, "list kept");
    assert!(h.command(Command::ToggleOnion));
    assert_eq!(h.doc().project().onion.mode, OnionMode::Specific);
    // Previous + Next is remembered the same way.
    assert!(h.command(Command::Onion(OnionMode::PreviousAndNext)));
    assert!(h.command(Command::ToggleOnion));
    assert!(h.command(Command::ToggleOnion));
    assert_eq!(h.doc().project().onion.mode, OnionMode::PreviousAndNext);
    // Ctrl+Alt+O: the selected tracks (2 active, 0 and 4 selected) are
    // added to what is drawn (tracks 1 and 3, Previous + Next of 2), then
    // all hidden again.
    h.set_tracks(&[2, 0, 4]);
    assert!(h.command(Command::OnionSelectedTracks));
    let ids: Vec<_> = (0..5).map(|i| id(&h, i)).collect();
    assert_eq!(h.doc().project().onion.sources(&ids, 2), vec![0, 1, 3, 4]);
    assert!(h.command(Command::OnionSelectedTracks));
    assert_eq!(h.doc().project().onion.sources(&ids, 2), vec![1, 3]);
    // Nothing selected but the active track: a hint, no change.
    h.set_track(2);
    let before = h.doc().project().onion.clone();
    assert!(h.command(Command::OnionSelectedTracks));
    assert_eq!(h.doc().project().onion, before);
    assert!(h.status().contains("Select the tracks first"));
    // Saved and read back with the project.
    let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
    let (back, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert_eq!(back.project().onion, h.doc().project().onion);
    assert_eq!(
        super::keybinds::shortcuts_of(Command::ToggleOnion),
        ["Ctrl+Shift+O"]
    );
    assert_eq!(
        super::keybinds::shortcuts_of(Command::OnionSelectedTracks),
        ["Ctrl+Alt+O"]
    );
}

/// B2-D01 (owner screenshot 060457): with the Conductor selected the Onion
/// Skin's sources are the musical tracks themselves — the track that was
/// active before is neither excluded nor stands in for the Conductor; the
/// relative modes show the whole song, Specific its list. Back on a track
/// nothing changes. Ctrl+Alt+O on the Conductor adds every selected track.
#[test]
fn the_conductor_onion_shows_the_musical_tracks_with_none_excluded() {
    use plumspirit_engine::model::project::OnionMode;
    let mut h = Harness::new(fixture());
    let n = h.doc().track_count();
    h.set_track(0);
    h.set_conductor();
    assert!(h.app.conductor_selected());
    assert!(
        h.app.onion_ghosts().is_empty(),
        "Onion off: no notes at all"
    );
    // The toolbar button turns on Previous + Next: on the Conductor (no
    // neighbours) the whole song, track 0 included although it was active.
    let all: Vec<usize> = (0..n).collect();
    assert!(h.command(Command::ToggleOnion));
    assert_eq!(h.doc().project().onion.mode, OnionMode::PreviousAndNext);
    assert_eq!(h.app.onion_ghosts(), all, "track 0 was active: still shown");
    for mode in [OnionMode::Next, OnionMode::Previous, OnionMode::AllOthers] {
        assert!(h.command(Command::Onion(mode)));
        assert_eq!(h.app.onion_ghosts(), all, "{mode:?}");
    }
    // Whichever track was active before, the Conductor's sources are the same.
    for i in 0..n {
        h.set_track(i);
        let as_track = h.app.onion_ghosts();
        assert!(!as_track.contains(&i), "a track is never its own ghost");
        h.set_conductor();
        assert_eq!(
            h.app.onion_ghosts(),
            (0..n).collect::<Vec<_>>(),
            "after track {i}"
        );
    }
    // Specific: the chosen tracks, the first one included.
    assert!(h.command(Command::Onion(OnionMode::Specific)));
    h.track_action(TrackAction::OnionSet(vec![0, 3], true));
    h.set_conductor();
    assert_eq!(h.app.onion_ghosts(), vec![0, 3]);
    // Ctrl+Alt+O on the Conductor: every selected track is added (none is
    // "the active track" here).
    h.set_conductor();
    h.app.selected_tracks = [h.id(1), h.id(2)].into_iter().collect();
    assert!(h.command(Command::OnionSelectedTracks));
    assert_eq!(h.app.onion_ghosts(), vec![0, 1, 2, 3]);
}

/// B1-0064 Per-Track Settings: each track keeps its own Onion mode and list;
/// changing the active track by any path brings them back; off, nothing is
/// swapped; the entries are saved with the project.
#[test]
fn per_track_onion_settings_follow_the_active_track_through_the_app() {
    use plumspirit_engine::model::project::OnionMode;
    let mut h = Harness::new(fixture());
    let ids: Vec<_> = (0..5).map(|i| h.id(i)).collect();
    h.set_track(2);
    h.app.sync_onion_track();
    assert!(h.command(Command::ToggleOnionPerTrack));
    assert!(h.doc().project().onion.per_track);
    // Track 2 shows track 4.
    h.track_action(TrackAction::OnionSet(vec![4], true));
    // Track 0 chooses track 3 instead.
    h.set_track(0);
    h.app.sync_onion_track();
    assert_eq!(
        h.doc().project().onion.sources(&ids, 0),
        vec![4],
        "nothing own yet: in force"
    );
    h.track_action(TrackAction::OnionSet(vec![4], false));
    h.track_action(TrackAction::OnionSet(vec![3], true));
    assert_eq!(h.doc().project().onion.sources(&ids, 0), vec![3]);
    // Back to 2: its own [4]; to 0: its own [3].
    h.set_track(2);
    h.app.sync_onion_track();
    assert_eq!(h.doc().project().onion.sources(&ids, 2), vec![4]);
    h.set_track(0);
    h.app.sync_onion_track();
    assert_eq!(h.doc().project().onion.sources(&ids, 0), vec![3]);
    // Saved and reloaded with the project (the entries of tracks 0 and 2).
    let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
    let (back, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert_eq!(back.project().onion, h.doc().project().onion);
    assert_eq!(back.project().onion.track_entries.len(), 2);
    // Off: switching tracks keeps what is in force.
    assert!(h.command(Command::ToggleOnionPerTrack));
    h.set_track(2);
    h.app.sync_onion_track();
    assert_eq!(h.doc().project().onion.sources(&ids, 2), vec![3]);
    assert_eq!(h.doc().project().onion.mode, OnionMode::Specific);
}

/// L-G04 (contract §014; "pressing my recording keybind for obs
/// (alt+shift+7) causes note outlines to mysteriously disappear"): audit
/// result — no binding uses 7, but egui fires a shortcut without Alt while
/// Alt is held, so another program's Alt+Shift+… hotkey reached PlumSpirit
/// as the plain key (Alt+Shift+F7 toggled Ghost Outlines, Alt+Shift+1 – 3
/// switched the tool). Now Alt matches exactly. A paused Piano Roll frame
/// with Onion outlines is identical before and after Alt+Shift+7 (and the
/// keys other layouts make of it: Slash on German, the AltGr variant),
/// Alt+Shift+F7 / 1 / 2 / 3, a focus loss and return; the plain keys still
/// work. NOT VERIFIED: OBS itself on Windows.
#[test]
fn another_programs_alt_shift_hotkey_changes_nothing_in_the_frame() {
    use plumspirit_engine::model::project::OnionMode;
    use std::hash::{Hash, Hasher};
    let size = egui::vec2(1600.0, 900.0);
    let mut h = Harness::new(fixture());
    h.set_track(2);
    assert!(h.command(Command::Onion(OnionMode::AllOthers)));
    for _ in 0..2 {
        h.app_frame(size, 1.0);
    }
    h.app.main_view = MainView::PianoRoll;
    let picture = |h: &mut Harness| {
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        let mut n = 0;
        for c in h.app_frame(size, 1.0) {
            if let egui::Shape::Mesh(m) = &c.shape {
                for v in &m.vertices {
                    (v.pos.x.to_bits(), v.pos.y.to_bits(), v.color.to_array()).hash(&mut hs);
                    n += 1;
                }
            }
        }
        (n, hs.finish())
    };
    for _ in 0..3 {
        picture(&mut h);
    }
    let before = picture(&mut h);
    assert!(before.0 > 0);
    let state = |h: &Harness| {
        (
            h.app.settings.onion_outlines,
            h.app.roll.tool,
            h.doc().state_fingerprint(),
        )
    };
    let was = state(&h);
    let alt_shift = egui::Modifiers {
        alt: true,
        shift: true,
        ..Default::default()
    };
    let altgr_shift = egui::Modifiers {
        alt: true,
        shift: true,
        ctrl: true,
        command: true,
        mac_cmd: false,
    };
    let press = |key, modifiers, pressed| egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    for (mods, key) in [
        (alt_shift, egui::Key::Num7),
        (alt_shift, egui::Key::Slash),
        (altgr_shift, egui::Key::Num7),
        (alt_shift, egui::Key::F7),
        (alt_shift, egui::Key::Num1),
        (alt_shift, egui::Key::Num2),
        (alt_shift, egui::Key::Num3),
    ] {
        h.modifiers = mods;
        h.app_frame_events(size, 1.0, vec![press(key, mods, true)]);
        h.app_frame_events(size, 1.0, vec![press(key, mods, false)]);
        h.modifiers = egui::Modifiers::NONE;
        h.app_frame(size, 1.0);
        assert_eq!(state(&h), was, "{mods:?} {key:?}");
    }
    // The window loses the focus and gets it back (Alt alone, a recorder).
    h.app_frame_events(size, 1.0, vec![egui::Event::WindowFocused(false)]);
    h.app_frame_events(size, 1.0, vec![egui::Event::WindowFocused(true)]);
    assert_eq!(state(&h), was);
    assert_eq!(picture(&mut h), before, "the same frame");
    // The plain keys still work.
    h.app_frame_events(
        size,
        1.0,
        vec![press(egui::Key::F7, egui::Modifiers::NONE, true)],
    );
    assert_eq!(h.app.settings.onion_outlines, !was.0, "F7");
    h.app_frame_events(
        size,
        1.0,
        vec![press(egui::Key::Num3, egui::Modifiers::NONE, true)],
    );
    assert_eq!(h.app.roll.tool, crate::piano_roll::Tool::Erase, "3");
    // Shift is still matched loosely (the Shift rows come first): Alt+Up
    // moves the track even with Shift held.
    let rows = |h: &Harness| h.doc().tracks().iter().map(|t| t.id()).collect::<Vec<_>>();
    let order = rows(&h);
    let mods = egui::Modifiers {
        alt: true,
        shift: true,
        ..Default::default()
    };
    h.set_track(2);
    h.modifiers = mods;
    h.app_frame_events(size, 1.0, vec![press(egui::Key::ArrowUp, mods, true)]);
    h.modifiers = egui::Modifiers::NONE;
    assert_ne!(rows(&h), order, "Alt+Shift+Up: Move Track Up");
}

/// L-G06 (contract §013 / §024: "Piano Roll clean thin note outlines ... and
/// no shadow gradients"): a Piano Roll frame — exact notes, exact Onion
/// ghosts with outlines, dense tiles — holds no blurred (shadow) rectangle,
/// and every triangle the roll builds itself is one flat colour.
#[test]
fn the_piano_roll_draws_no_shadow_gradients() {
    use plumspirit_engine::model::project::OnionMode;
    let size = egui::vec2(1600.0, 900.0);
    let mut h = Harness::new(fixture());
    h.set_track(2);
    assert!(h.command(Command::Onion(OnionMode::AllOthers)));
    for _ in 0..2 {
        h.app_frame(size, 1.0);
    }
    h.app.main_view = MainView::PianoRoll;
    for zoomed_out in [false, true] {
        if zoomed_out {
            h.app.roll.view.time.pixels_per_tick /= 64.0;
        }
        let mut triangles = 0;
        for _ in 0..4 {
            for c in h.app_frame(size, 1.0) {
                match &c.shape {
                    egui::Shape::Rect(r) => {
                        assert_eq!(r.blur_width, 0.0, "a shadow: {r:?}");
                    }
                    egui::Shape::Mesh(m) => {
                        for t in m.indices.chunks(3) {
                            let c0 = m.vertices[t[0] as usize].color;
                            for &i in &t[1..] {
                                assert_eq!(m.vertices[i as usize].color, c0, "a gradient");
                            }
                            triangles += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(triangles > 0, "{zoomed_out}");
        let st = h.app.roll.stats;
        assert!(
            st.onion_primitives > 0 && st.primitives > 0,
            "{zoomed_out}: {st:?}"
        );
    }
}

/// L-G02: what colours the keys through the app — by default the tracks the
/// Piano Roll shows (the active track and its subtracks, plus the ghosts
/// while the Onion is on), the chosen scope otherwise; the choice is a
/// setting, reset brings the default back.
#[test]
fn the_keys_follow_the_roll_with_onion_on_and_off() {
    use plumspirit_engine::model::project::OnionMode;
    use std::collections::BTreeSet;
    let mut h = Harness::new(fixture());
    let ids: Vec<_> = (0..5).map(|i| h.id(i)).collect();
    let set = |v: &[usize]| -> BTreeSet<_> { v.iter().map(|&i| ids[i]).collect() };
    assert!(h.app.settings.key_follow_roll);
    // Track 0 (with its subtrack 1), Onion off.
    // (The document is shared only for the call: an edit needs it alone.)
    let keys = |h: &Harness| {
        let doc = h.app.open.as_ref().unwrap().doc.clone();
        h.app.key_tracks(&doc)
    };
    h.set_track(0);
    assert_eq!(keys(&h), Some(set(&[0, 1])));
    // Onion on: the ghosts join.
    assert!(h.command(Command::Onion(OnionMode::Next)));
    h.set_track(2);
    assert_eq!(keys(&h), Some(set(&[2, 3])));
    assert!(h.command(Command::ToggleOnion));
    assert_eq!(keys(&h), Some(set(&[2])), "off: the track only");
    // The Conductor: nothing without the Onion.
    h.set_conductor();
    assert_eq!(keys(&h), Some(BTreeSet::new()));
    // Another choice: that scope, mute / solo and all.
    assert!(h.command(Command::KeyScope(super::progressor::ScopeSetting::Project)));
    assert!(!h.app.settings.key_follow_roll);
    assert_eq!(keys(&h), None, "every track");
    assert!(h.command(Command::KeyFollowRoll));
    assert!(h.app.settings.key_follow_roll);
    assert!(h.command(Command::KeyScope(super::progressor::ScopeSetting::Audible)));
    h.settings_reset(Some(super::settings_window::Section::PianoRoll));
    assert!(h.app.settings.key_follow_roll, "reset: follow the roll");
}

/// L-G03 (contract §013; Flopp: "apply per track onion settings after
/// selecting only one should probably reset all the other tracks' onions
/// too"): Show Only Selected Tracks replaces the ghosts (an earlier
/// multi-track list does not persist), Add and Remove keep the others, an
/// Alt+click on a colour box shows only that track, Ctrl+Alt+Shift+O is
/// Replace; with Per-Track Settings, "Use This Setting for Every Track"
/// resets the tracks' own. Outside undo, saved with the project.
#[test]
fn onion_replace_add_and_remove_are_distinct_and_per_track_resets_on_request() {
    use plumspirit_engine::model::project::OnionMode;
    let mut h = Harness::new(fixture());
    let ids: Vec<_> = (0..5).map(|i| h.id(i)).collect();
    let shown = |h: &Harness, a: usize| h.doc().project().onion.sources(&ids, a);
    h.set_track(2);
    h.app.sync_onion_track();
    assert!(h.command(Command::Onion(OnionMode::PreviousAndNext)));
    assert_eq!(shown(&h, 2), vec![1, 3]);
    // Add: 0 and 4 join what is shown.
    h.set_tracks(&[2, 0, 4]);
    assert!(h.command(Command::OnionAddSelected));
    assert_eq!(shown(&h, 2), vec![0, 1, 3, 4]);
    // Replace: only 4 — the earlier multi-track list is gone.
    h.set_tracks(&[2, 4]);
    assert!(h.command(Command::OnionShowOnlySelected));
    assert_eq!(shown(&h, 2), vec![4]);
    assert_eq!(h.doc().project().onion.specific, vec![ids[4]]);
    // Add 0, then Remove 4 (twice: nothing more changes).
    h.set_tracks(&[2, 0]);
    assert!(h.command(Command::OnionAddSelected));
    assert_eq!(shown(&h, 2), vec![0, 4]);
    h.set_tracks(&[2, 4]);
    assert!(h.command(Command::OnionRemoveSelected));
    assert_eq!(shown(&h, 2), vec![0]);
    assert!(h.command(Command::OnionRemoveSelected));
    assert_eq!(shown(&h, 2), vec![0]);
    // Alt+click on a colour box: only that track.
    h.track_action(TrackAction::OnionOnly(vec![3]));
    assert_eq!(shown(&h, 2), vec![3]);
    // Only the active track selected: a hint, nothing changes.
    h.set_track(2);
    let before = h.doc().project().onion.clone();
    assert!(h.command(Command::OnionShowOnlySelected));
    assert_eq!(h.doc().project().onion, before);
    assert!(h.status().contains("Select the tracks first"));
    // The keyboard: Ctrl+Alt+Shift+O is Replace (not Ctrl+Shift+O, Toggle).
    h.set_tracks(&[2, 1, 4]);
    h.app.main_view = MainView::PianoRoll;
    h.app_frame(egui::vec2(1600.0, 900.0), 1.0);
    let mods = egui::Modifiers {
        alt: true,
        shift: true,
        command: true,
        ctrl: true,
        mac_cmd: false,
    };
    h.app_frame_events(
        egui::vec2(1600.0, 900.0),
        1.0,
        vec![egui::Event::Key {
            key: egui::Key::O,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }],
    );
    assert_eq!(h.doc().project().onion.mode, OnionMode::Specific);
    assert_eq!(shown(&h, 2), vec![1, 4]);
    assert_eq!(
        super::keybinds::shortcuts_of(Command::OnionShowOnlySelected),
        ["Ctrl+Alt+Shift+O"]
    );
    // Outside undo, saved with the project.
    assert!(!h.app.open.as_ref().unwrap().history.can_undo());
    let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
    let (back, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert_eq!(back.project().onion, h.doc().project().onion);
    // Per-Track Settings: each track keeps its own until reset.
    assert!(
        !h.command(Command::OnionUseForEveryTrack),
        "only with Per-Track Settings"
    );
    h.set_track(2);
    h.app.sync_onion_track();
    assert!(h.command(Command::ToggleOnionPerTrack));
    h.set_track(0);
    h.app.sync_onion_track();
    h.set_tracks(&[0, 3]);
    assert!(h.command(Command::OnionShowOnlySelected));
    assert_eq!(shown(&h, 0), vec![3]);
    h.set_track(2);
    h.app.sync_onion_track();
    assert_eq!(shown(&h, 2), vec![1, 4], "track 2's own");
    // Track 2's setting for every track: track 0's own is reset.
    assert!(h.command(Command::OnionUseForEveryTrack));
    assert!(h.status().contains("Every track now uses"));
    assert!(h.doc().project().onion.track_entries.is_empty());
    h.set_track(0);
    h.app.sync_onion_track();
    assert_eq!(shown(&h, 0), vec![1, 4], "no longer its own [3]");
}

/// B1-0086: Onion ghosts are drawn the same way whether the active edit
/// target is a track, a subtrack or a clip opened in the Piano Roll (the
/// linked copy on track 2 at tick 3840: ghosts in song time, shifted into
/// the clip's view).
#[test]
fn onion_ghosts_draw_for_a_track_a_subtrack_and_an_open_clip() {
    let mut d = fixture();
    // Track 3 also plays where track 2's linked copy is placed.
    let recs: Vec<NoteRecord> = (0..8u64).map(|k| note(3840 + k * 240, 62)).collect();
    d.track_mut(3)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = Harness::new(d);
    let screen = egui::Vec2::new(1366.0, 768.0);
    let ghosts_drawn = |h: &mut Harness| {
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        assert_eq!(h.app.main_view, MainView::PianoRoll);
        h.app.roll.stats.onion_primitives
    };
    h.set_track(2);
    h.track_action(TrackAction::OnionSet(vec![3], true));
    assert!(ghosts_drawn(&mut h) > 0, "track");
    h.set_track(1);
    assert!(ghosts_drawn(&mut h) > 0, "subtrack");
    let (_, placed) = clips(&h);
    h.set_track(2);
    h.clip(ClipIntent::Open(placed));
    assert!(ghosts_drawn(&mut h) > 0, "open clip");
    // Off: nothing.
    assert!(h.command(Command::ToggleOnion));
    assert_eq!(ghosts_drawn(&mut h), 0, "off");
}

/// B1-0065 (proposal image12: "text overlay shouldn't block me from seeing
/// the notes"): the roll's banners — Conductor, "Editing <clip>" — are drawn
/// in the ruler strip, on the row of its time-signature label, not over the
/// notes; at 100 % and 175 % UI scale.
#[test]
fn roll_banners_sit_in_the_ruler_not_over_the_notes() {
    use crate::ui::testkit::texts;
    let mut h = Harness::new(fixture());
    let screen = egui::Vec2::new(1366.0, 768.0);
    let frame = |h: &mut Harness| {
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app_frame(screen, 1.0);
        texts(&h.app_frame(screen, 1.0))
    };
    for scale in [100u16, 175] {
        h.settings_action(super::settings_window::Action::UiScale(scale));
        h.set_conductor();
        let shown = frame(&mut h);
        let y_of = |t: &str| shown.iter().find(|s| s.0.starts_with(t)).map(|s| s.1);
        let ruler_y = y_of("4/4").expect("time signature label in the ruler").y;
        let banner = y_of("Conductor — tempo").expect("Conductor banner");
        assert!(
            (banner.y - ruler_y).abs() < 4.0,
            "{scale} %: {banner:?} vs ruler {ruler_y}"
        );
        let (_, placed) = clips(&h);
        h.set_track(2);
        h.clip(ClipIntent::Open(placed));
        let shown = frame(&mut h);
        let y_of = |t: &str| shown.iter().find(|s| s.0.starts_with(t)).map(|s| s.1);
        let ruler_y = y_of("4/4").map(|p| p.y);
        // The roll's banner (the status bar may also say "Editing …").
        let editing = shown
            .iter()
            .filter(|s| s.0.starts_with("Editing "))
            .map(|s| s.1)
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .expect("clip banner");
        let ry = ruler_y.expect("time signature label in the ruler");
        assert!(
            (editing.y - ry).abs() < 4.0,
            "{scale} %: {editing:?} vs ruler {ry}"
        );
    }
}

/// B1-0065: the Progressor readout over the Piano Roll — a click on its chip
/// collapses / expands it, a drag moves it — at 30 / 100 / 175 % UI scale;
/// the notes under the rest of it stay reachable (only the chip reacts).
/// B1-0107: the Track List shows the Progressor and, with clips selected,
/// Selection Analytics of their notes, over its clip area; the readout's
/// chip collapses it without touching the clip selection.
#[test]
fn the_track_list_shows_the_progressor_and_selection_analytics_of_its_clips() {
    let mut h = Harness::new(fixture());
    let screen = egui::Vec2::new(1366.0, 768.0);
    h.app.main_view = MainView::TrackList;
    h.app.settings.progressor.template = "Notes: {notes}/{total_notes} · {scope}".into();
    h.app.settings.progressor.selection_template = "Sel {selected} · {gate_range}".into();
    for _ in 0..3 {
        h.app_frame(screen, 1.0);
    }
    let list = h.app.track_list.areas.list;
    let p = h.app.overlay_views[0].expect("Progressor over the Track List");
    assert!(list.contains_rect(p.rect), "{:?} in {list:?}", p.rect);
    assert!(
        h.app.overlay_views[1].is_none(),
        "no clips selected: no Selection Analytics"
    );
    // Select the clips of tracks 0 and 2: 16 notes of 120 ticks (the fixture).
    let ids: Vec<_> = [0usize, 2]
        .iter()
        .map(|&i| plumspirit_engine::model::ClipId::Primary(h.doc().track(i).unwrap().id()))
        .collect();
    h.app.track_list.select_only(ids[0]);
    h.app.track_list.selected.insert(ids[1]);
    for _ in 0..3 {
        h.app_frame(screen, 1.0);
    }
    let v = h.app.overlay_views[1].expect("Selection Analytics over the Track List");
    assert!(list.contains_rect(v.rect));
    let text = h
        .app
        .sel_stats
        .text(0, &h.app.settings.progressor)
        .expect("text");
    assert!(text.starts_with("Sel 16 · "), "{text}");
    // The chip collapses the readout; the clips stay selected.
    let press = |p: egui::Pos2, down: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: down,
        modifiers: egui::Modifiers::NONE,
    };
    let c = p.chip.center();
    h.app.settings.progressor.collapsed = false;
    h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(c)]);
    h.app_frame_events(screen, 1.0, vec![press(c, true), press(c, false)]);
    assert!(h.app.settings.progressor.collapsed, "the chip collapses it");
    assert_eq!(
        h.app.track_list.selection().len(),
        2,
        "clips still selected"
    );
    // Hide Text Overlays hides both.
    h.app.settings.hide_text_overlays = true;
    h.app_frame(screen, 1.0);
    assert!(h.app.overlay_views.iter().all(Option::is_none));
}

#[test]
fn the_progressor_chip_collapses_and_moves_the_readout_at_every_scale() {
    let mut h = Harness::new(fixture());
    h.set_track(2);
    let screen = egui::Vec2::new(1366.0, 768.0);
    let press = |p: egui::Pos2, down: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: down,
        modifiers: egui::Modifiers::NONE,
    };
    for scale in [30u16, 100, 175] {
        h.settings_action(super::settings_window::Action::UiScale(scale));
        h.app.settings.progressor.collapsed = false;
        h.app.settings.progressor.offset = [0.0, 0.0];
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        let v = h.app.overlay_views[0].expect("Progressor drawn");
        let c = v.chip.center();
        h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(c)]);
        h.app_frame_events(screen, 1.0, vec![press(c, true), press(c, false)]);
        assert!(
            h.app.settings.progressor.collapsed,
            "{scale} %: click collapses"
        );
        h.app_frame(screen, 1.0);
        let small = h.app.overlay_views[0].unwrap();
        assert!(small.rect.height() < v.rect.height(), "{scale} %");
        // Drag the chip 40 points left (the readout is in the top-right
        // corner): the offset grows inwards by about that much.
        let c = small.chip.center();
        h.app_frame_events(
            screen,
            1.0,
            vec![egui::Event::PointerMoved(c), press(c, true)],
        );
        for k in 1..=4 {
            let p = c - egui::vec2(10.0 * k as f32, 0.0);
            h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(p)]);
        }
        h.app_frame_events(screen, 1.0, vec![press(c - egui::vec2(40.0, 0.0), false)]);
        let off = h.app.settings.progressor.offset;
        assert!(off[0] > 20.0 && off[0] <= 40.5, "{scale} %: {off:?}");
        assert!(
            h.app.settings.progressor.collapsed,
            "{scale} %: a drag is not a click"
        );
    }
}

/// One key press (with its text for printable keys) as whole-frame input.
fn key_events(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    let mut v = vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }];
    if modifiers.is_none() && key.symbol_or_name().chars().count() == 1 {
        v.push(egui::Event::Text(key.symbol_or_name().to_lowercase()));
    }
    v.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers,
    });
    v
}

/// B1-0068: while a text field has keyboard focus no shortcut fires — plain
/// keys (tool 1 / 2 / 3, Delete, Home), Ctrl / Alt chords (Ctrl+A, Ctrl+Z,
/// Alt+Up), Space and Tab all go to the field; after Escape the same key
/// works again.
#[test]
fn shortcuts_never_fire_while_a_text_field_has_focus() {
    use crate::ui::testkit::texts;
    use egui::{Key, Modifiers};
    let mut h = Harness::new(fixture());
    h.set_notes(0);
    let screen = egui::Vec2::new(1366.0, 768.0);
    h.app_frame(screen, 1.0);
    h.app.main_view = MainView::PianoRoll;
    h.app_frame(screen, 1.0);
    let shapes = h.app_frame(screen, 1.0);
    let shown = texts(&shapes);
    let gate = shown.iter().find(|t| t.0 == "Gate").expect("Gate label").1;
    // The Gate box: the first text right of its label on the same row.
    let field = shown
        .iter()
        .filter(|t| (t.1.y - gate.y).abs() < 4.0 && t.1.x > gate.x + 4.0)
        .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .expect("Gate box")
        .1;
    let press = |p: egui::Pos2, down: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: down,
        modifiers: Modifiers::NONE,
    };
    h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(field)]);
    h.app_frame_events(screen, 1.0, vec![press(field, true), press(field, false)]);
    h.app_frame(screen, 1.0);
    assert!(h.ctx.egui_wants_keyboard_input(), "the Gate box has focus");
    let (fp, sel, tool, view) = (
        h.doc().fingerprint(),
        h.app.selected_count(),
        h.app.roll.tool,
        h.app.main_view,
    );
    assert!(sel > 0);
    for (key, mods) in [
        (Key::Num2, Modifiers::NONE),
        (Key::Num3, Modifiers::NONE),
        (Key::Delete, Modifiers::NONE),
        (Key::Home, Modifiers::NONE),
        (Key::A, Modifiers::COMMAND),
        (Key::Z, Modifiers::COMMAND),
        (Key::ArrowUp, Modifiers::ALT),
        (Key::Space, Modifiers::NONE),
        (Key::Tab, Modifiers::NONE),
    ] {
        h.app_frame_events(screen, 1.0, key_events(key, mods));
        assert_eq!(h.doc().fingerprint(), fp, "{key:?} {mods:?} edited");
        assert_eq!(h.app.selected_count(), sel, "{key:?} {mods:?}");
        assert_eq!(h.app.roll.tool, tool, "{key:?} {mods:?}");
        assert_eq!(h.app.main_view, view, "{key:?} {mods:?}");
    }
    // Escape leaves the box; then "2" is the Draw tool again.
    h.app_frame_events(screen, 1.0, key_events(Key::Escape, Modifiers::NONE));
    h.app_frame(screen, 1.0);
    assert!(!h.ctx.egui_wants_keyboard_input());
    h.app_frame_events(screen, 1.0, key_events(Key::Num2, Modifiers::NONE));
    assert_ne!(h.app.roll.tool, tool, "the shortcut works outside the box");
}

/// B1-0068: opening a dialog or window (Track Properties, a tool window,
/// Settings, Keyboard Shortcuts) keeps the note selection and the Piano
/// Roll / Track List scroll.
#[test]
fn opening_dialogs_keeps_the_selection_and_the_scroll() {
    let mut h = Harness::new(fixture());
    let screen = egui::Vec2::new(1366.0, 768.0);
    h.set_notes(0);
    h.app_frame(screen, 1.0);
    h.app.main_view = MainView::PianoRoll;
    h.app_frame(screen, 1.0);
    h.app_frame(screen, 1.0);
    assert_eq!(h.app.main_view, MainView::PianoRoll);
    h.app.roll.view.time.origin_tick += 480;
    h.app.roll.view.pitch.scroll_rows += 7.0;
    h.app_frame(screen, 1.0);
    let sel = h.app.selected_count();
    assert!(sel > 0);
    let roll = h.app.roll.view;
    let list = h.app.track_list.view;
    for c in [
        Command::TrackProperties,
        Command::OpenTool("duplicate"),
        Command::OpenSettings,
        Command::ShowKeybinds,
        Command::ShowHelp,
    ] {
        assert!(h.command(c), "{c:?}");
        for _ in 0..3 {
            h.app_frame(screen, 1.0);
        }
        assert_eq!(h.app.selected_count(), sel, "{c:?}");
        let v = h.app.roll.view;
        assert_eq!(v.time.origin_tick, roll.time.origin_tick, "{c:?}");
        assert_eq!(v.pitch.scroll_rows, roll.pitch.scroll_rows, "{c:?}");
        assert_eq!(
            h.app.track_list.view.time.origin_tick, list.time.origin_tick,
            "{c:?}"
        );
        assert_eq!(
            h.app.track_list.view.pitch.scroll_rows, list.pitch.scroll_rows,
            "{c:?}"
        );
        h.app.dialog = None;
    }
}

// ------------------------------------------------------------------------
// v1.0-beta1 (B1-0010 / B1-0181, B1-S64): the beta1 actions from the nine
// contexts. `docs/v1_0_beta1/CONTEXT_MATRIX.md` lists every cell.
// ------------------------------------------------------------------------

/// Context-free beta1 commands — Help (F1), Lua Plugins, Project Properties
/// (Change PPQN), the overlap switch, Tab — run from every context and keep
/// the selection (clips, notes, tracks) as it was.
#[test]
fn beta1_window_and_switch_commands_work_from_every_context() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let clips = h.app.track_list.selected.clone();
        let notes = h.app.selected_count();
        assert!(h.command(Command::ShowHelp), "{c:?}: F1");
        assert!(h.app.help.open);
        let want = if c == Ctx::Notes {
            "piano-roll"
        } else {
            "track-list"
        };
        assert_eq!(
            crate::app::help::TOPICS[h.app.help.current].id,
            want,
            "{c:?}: the context's topic"
        );
        assert!(h.command(Command::ShowLuaPlugins), "{c:?}");
        assert!(h.app.lua_plugins_window.open);
        assert!(h.command(Command::ProjectProperties), "{c:?}");
        assert!(
            matches!(h.app.dialog, Some(Dialog::ProjectProperties { .. })),
            "{c:?}: Project Properties (Change PPQN) opens"
        );
        h.app.dialog = None;
        let was = h.app.settings.overlap.on;
        assert!(h.command(Command::ToggleOverlapPrevention), "{c:?}");
        assert_eq!(h.app.settings.overlap.on, !was);
        assert_eq!(h.app.track_list.selected, clips, "{c:?}: clips kept");
        assert_eq!(h.app.selected_count(), notes, "{c:?}: notes kept");
        let view = h.app.main_view;
        assert!(h.command(Command::SwitchView), "{c:?}: Tab");
        assert_ne!(h.app.main_view, view, "{c:?}");
        assert!(h.command(Command::SwitchView));
        assert_eq!(h.app.selected_count(), notes, "{c:?}: notes kept");
    }
}

/// B1-0097 in the matrix: in the Track List, Slice / Copy act on the
/// selected clips (K, KK); with no clip selected they are off there (C, T,
/// S, F, TT, 0 — a note command needs the Piano Roll); in the Piano Roll
/// (N) they act on the notes. Each edit is one Undo step.
#[test]
fn beta1_slice_and_copy_follow_the_selected_clips_or_notes() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        // Inside the clip(s): the linked copy starts at 3,840.
        h.app.roll.cursor_tick = if c == Ctx::Clip { 3_960 } else { 120 };
        let clip_ctx = matches!(c, Ctx::Clip | Ctx::Clips);
        let on = clip_ctx || c == Ctx::Notes;
        assert_eq!(h.app.enabled(Command::SliceNotes), on, "{c:?}: Slice");
        assert_eq!(h.app.enabled(Command::Copy), on, "{c:?}: Copy");
        if !on {
            continue;
        }
        let steps = h.app.open.as_ref().unwrap().history.undo_len();
        let placements = h.doc().clips().layout().placements.len();
        let notes = h.doc().track(2).unwrap().notes.len();
        assert!(h.command(Command::SliceNotes), "{c:?}");
        assert_eq!(
            h.app.open.as_ref().unwrap().history.undo_len(),
            steps + 1,
            "{c:?}: one undo step"
        );
        if clip_ctx {
            assert!(
                h.doc().clips().layout().placements.len() > placements,
                "{c:?}: the clips were sliced"
            );
        } else {
            assert!(
                h.doc().track(2).unwrap().notes.len() > notes,
                "{c:?}: the notes were sliced"
            );
        }
        undo(&mut h);
        enter(&mut h, c);
        assert!(h.command(Command::Copy), "{c:?}");
        if clip_ctx {
            assert!(h.app.clip_clipboard.is_some(), "{c:?}: the clips");
        } else {
            assert!(h.app.clipboard.as_ref().is_some_and(|b| !b.is_empty()));
        }
    }
}

/// B2-A02: every cached view equals a fresh build of the document as it is
/// now — the track rows' note counts, the Track List's clips, the roll's
/// edit target (its notes exist unless the roll is locked) — and the keys
/// the running audio and the Inspector compare change with every edit.
fn fresh(h: &mut Harness, what: &str, last: &mut Option<(u64, u64)>) {
    let generation = h.app.doc_generation();
    let doc = h.app.open.as_ref().expect("a project").doc.clone();
    assert_eq!(
        h.app.note_counts.counts(&doc, generation).to_vec(),
        doc.placed_note_counts(),
        "{what}: track note counts"
    );
    let cached = h.app.track_list.model(&doc, generation);
    let built = plumspirit_engine::view::track_list::TrackListModel::build(&doc);
    assert_eq!(
        cached.row_count(),
        built.row_count(),
        "{what}: Track List rows"
    );
    for t in 0..built.row_count() {
        assert_eq!(cached.row(t), built.row(t), "{what}: clips of track {t}");
    }
    h.app.sync_clip_session();
    if !matches!(
        h.app.editor_mode(),
        super::clip_session::EditorMode::Locked(_)
    ) {
        assert!(
            plumspirit_engine::editor::note_view(&doc, h.app.edit_target()).is_ok(),
            "{what}: the roll's edit target exists"
        );
    }
    let keys = (doc.analysis_revision(), doc.export_key());
    if let Some(prev) = *last {
        assert_ne!(
            keys, prev,
            "{what}: the audio / Inspector keys follow the edit"
        );
    }
    *last = Some(keys);
}

/// B2-A02 context matrix: a linked clip moved across tracks, a cross-track
/// tool on it (Duplicate "To tracks" onto track 4) with undo / redo, and Delete Track of a parent with undo / redo — after each
/// step every cached view equals a fresh build.
#[test]
fn cached_views_equal_fresh_builds_across_clip_moves_tools_and_delete_track() {
    use plumspirit_engine::transform::ParamValue;
    let mut h = Harness::new(fixture());
    let mut last = None;
    fresh(&mut h, "start", &mut last);
    let (_, placed) = clips(&h);
    h.clip(ClipIntent::Move(vec![(placed, 4, 960)]));
    fresh(&mut h, "linked copy moved onto track 5", &mut last);
    h.set_notes(4);
    h.app.sync_clip_session();
    assert_eq!(
        h.app.edit_target(),
        plumspirit_engine::editor::NoteTarget::Track(2),
        "track 5 edits the linked content"
    );
    let before_tool = h.doc().state_fingerprint();
    h.app.open_tool("duplicate");
    let s = h.app.tool.as_mut().expect("the tool window");
    s.values.set("n01", ParamValue::Bool(true));
    s.values.set("n01.targets", ParamValue::Choice(1));
    s.values.set("n01.tracks", ParamValue::List(vec![4]));
    let ctx = h.ctx.clone();
    h.app.apply_tool(&ctx);
    assert_ne!(h.doc().state_fingerprint(), before_tool, "{}", h.status());
    // The copies sit where the moved clip plays: song 960 + 240 k on track 4.
    let copies: Vec<u64> = h
        .doc()
        .track(3)
        .unwrap()
        .notes
        .iter()
        .filter(|n| n.attrs.pitch() == 62)
        .map(|n| n.start_tick)
        .collect();
    assert_eq!(copies, (0..8u64).map(|k| 960 + k * 240).collect::<Vec<_>>());
    fresh(
        &mut h,
        "Duplicate to track 4 from the moved clip",
        &mut last,
    );
    undo(&mut h);
    assert_eq!(h.doc().state_fingerprint(), before_tool);
    last = None;
    fresh(&mut h, "undo of the tool", &mut last);
    assert!(h.command(Command::Redo));
    fresh(&mut h, "redo of the tool", &mut last);
    h.set_track(0);
    h.track_action(TrackAction::Delete);
    assert!(h.confirm_delete().is_some());
    assert_eq!(h.doc().track_count(), 3, "track 1 and its subtrack");
    fresh(&mut h, "Delete Track", &mut last);
    undo(&mut h);
    fresh(&mut h, "undo of Delete Track", &mut last);
    assert!(h.command(Command::Redo));
    fresh(&mut h, "redo of Delete Track", &mut last);
}

/// B2-A02: a v0.8 project (no clips: every track its own identity clip)
/// opens with fresh views; moving its first clip, editing it and undoing
/// keep them fresh.
#[test]
fn an_old_v08_project_keeps_fresh_views_through_clip_edits() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../plumspirit_engine/tests/fixtures/v08/multi_subchannel_v08.lup"
    );
    let bytes = std::fs::read(path).expect("the v0.8 fixture");
    let (doc, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).expect("opens");
    assert!(doc.clips_are_identity());
    let mut h = Harness::new(doc);
    let mut last = None;
    fresh(&mut h, "v0.8 project", &mut last);
    let t0 = h.id(0);
    h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 0, 1_920)]));
    fresh(&mut h, "v0.8 clip moved", &mut last);
    h.set_notes(0);
    assert!(h.command(Command::Delete) || h.app.selected_count() == 0);
    fresh(&mut h, "notes deleted in the moved clip", &mut last);
    undo(&mut h);
    fresh(&mut h, "undo", &mut last);
}

// ------------------------------------------------------- B2-F03 Add Tracks…

fn add_tracks_dialog(h: &Harness) -> super::dialogs::AddTracksDraft {
    match &h.app.dialog {
        Some(Dialog::AddTracks(d)) => (**d).clone(),
        other => panic!("the Add Tracks dialog, not {other:?}"),
    }
}

fn selected_ids(h: &Harness) -> Vec<TrackId> {
    h.app.selected_tracks.iter().copied().collect()
}

/// B2-F03 (§007): Add Tracks… (Song menu, the panel's button, Ctrl+Alt+N —
/// all `Command::AddTracks`) from every context: the dialog names the place
/// (below the active track at its level and folder; the end from the
/// Conductor) and changes nothing; confirmed, the tracks appear there in
/// one undo step, all selected with the first one active, without clips
/// (the Piano Roll is locked on them); Undo / Redo restore the ids.
#[test]
fn add_tracks_works_from_every_context() {
    use plumspirit_engine::editor::folders::AddTracksAt;
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let before = ids(&h);
        let expect = active_of(c).map_or(AddTracksAt::End, AddTracksAt::Below);
        assert!(h.command(Command::AddTracks), "{c:?}: enabled");
        let d = add_tracks_dialog(&h);
        assert_eq!(d.at, expect, "{c:?}");
        assert_eq!(d.existing, before.len());
        assert_eq!(ids(&h), before, "{c:?}: the dialog changes nothing");
        let folders = &h.doc().project().folders;
        let (start, level, folder) = match d.at {
            AddTracksAt::Below(a) => (
                h.doc().subtree_end(a),
                depth(&h, a),
                folders.folder_of(h.id(a)),
            ),
            _ => (before.len(), 0, 0),
        };
        h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(d.at, 3));
        let after = ids(&h);
        assert_eq!(after.len(), before.len() + 3, "{c:?}");
        assert_eq!(&after[..start], &before[..start], "{c:?}: nothing replaced");
        assert_eq!(&after[start + 3..], &before[start..], "{c:?}");
        for k in start..start + 3 {
            assert_eq!(depth(&h, k), level, "{c:?}: same level");
            let f = h.doc().project().folders.folder_of(h.id(k));
            assert_eq!(f, folder, "{c:?}: same folder");
            assert!(
                plumspirit_engine::editor::clips::default_pick(h.doc(), k).is_none(),
                "{c:?}: no clip"
            );
        }
        assert_eq!(h.app.active_track(), start, "{c:?}: the first is active");
        assert!(!h.app.conductor_selected(), "{c:?}");
        assert_eq!(selected_ids(&h), after[start..start + 3].to_vec(), "{c:?}");
        assert!(h.status().contains("Added 3 tracks"), "{c:?}");
        undo(&mut h);
        assert_eq!(ids(&h), before, "{c:?}: one undo step");
        assert!(h.command(Command::Redo));
        assert_eq!(ids(&h), after, "{c:?}: redo brings the same ids");
    }
}

/// B2-F03: the menus' own places — the track row's Add Tracks Below… (the
/// clicked row, not the active one), the folder header's Add Tracks to
/// Folder… (members at the folder's end, the folder expanded), the empty
/// space's / Conductor's Add Tracks… (the end) — and Ctrl+Alt+N.
#[test]
fn add_tracks_from_the_track_folder_and_empty_space_menus_and_the_keyboard() {
    use plumspirit_engine::editor::folders::AddTracksAt;
    let mut h = Harness::new(fixture());
    h.set_track(4);
    // The subtrack's row clicked while track 4 is active.
    h.track_action(TrackAction::AddTracks(Some(AddTracksAt::Below(1))));
    let d = add_tracks_dialog(&h);
    assert_eq!(d.at, AddTracksAt::Below(1));
    assert!(d.place.contains("below “Subtrack 1”"), "{}", d.place);
    h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(d.at, 2));
    assert_eq!((parent(&h, 2), parent(&h, 3)), (Some(0), Some(0)));
    assert_eq!(h.app.active_track(), 2);
    // The folder header: collapsed first; the new members follow track 3.
    let f = h.doc().project().folders.folder_of(h.id(5));
    assert_ne!(f, 0);
    h.app
        .doc_mut()
        .unwrap()
        .project_mut()
        .folders_collapsed
        .insert(f);
    h.track_action(TrackAction::AddTracks(Some(AddTracksAt::Folder(f))));
    let d = add_tracks_dialog(&h);
    assert!(
        d.place.contains(&format!("folder “{FOLDER}”")),
        "{}",
        d.place
    );
    h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(d.at, 4));
    assert_eq!(
        plumspirit_engine::editor::folders::folder_roots(h.doc(), f),
        vec![5, 6, 7, 8, 9]
    );
    assert!(
        !h.doc().project().folders_collapsed.contains(&f),
        "expanded"
    );
    // The empty space / Conductor menu: the end.
    let n = h.doc().track_count();
    h.track_action(TrackAction::AddTracks(Some(AddTracksAt::End)));
    h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(
        add_tracks_dialog(&h).at,
        1,
    ));
    assert_eq!(h.doc().track_count(), n + 1);
    assert_eq!(h.app.active_track(), n);
    assert!(h.status().contains("Added a track"));
    // Ctrl+Alt+N opens the dialog (not Ctrl+N's New Project).
    h.app.dialog = None;
    h.set_track(0);
    let ctrl_alt = egui::Modifiers {
        alt: true,
        ctrl: true,
        command: true,
        ..Default::default()
    };
    h.keybinds_frame(egui::Key::N, ctrl_alt);
    assert_eq!(add_tracks_dialog(&h).at, AddTracksAt::Below(0));
}

/// B2-F03: the count is checked before anything changes — a positive whole
/// number (separators allowed), at most what the project can still hold,
/// memory against what the PC has free; the line says the resulting count,
/// the memory and, for large counts, the rows; a large count asks once more
/// (a changed count asks again), Cancel / an unconfirmed count add nothing.
#[test]
fn add_tracks_checks_the_count_and_asks_once_more_for_large_ones() {
    use super::dialogs::{ADD_TRACKS_CONFIRM, AddTracksDraft, add_tracks_preflight};
    use plumspirit_engine::editor::folders::{AddTracksAt, new_track_bytes};
    for bad in ["", "0", "-3", "2.5", "1e3", "ten"] {
        let e = add_tracks_preflight(bad, 5, None).unwrap_err();
        assert_eq!(e, "Enter a whole number of 1 or more.", "{bad}");
    }
    let e = add_tracks_preflight("65531", 5, None).unwrap_err();
    assert!(e.contains("65,530 more can be added."), "{e}");
    let (n, line) = add_tracks_preflight("65,530", 5, None).unwrap();
    assert_eq!(n, 65_530);
    assert!(line.contains("65,535 tracks") && line.contains("65,530 more rows"));
    let (n, line) = add_tracks_preflight(" 16 ", 5, Some(1 << 30)).unwrap();
    assert_eq!(n, 16);
    assert!(
        line.starts_with("The project will have 21 tracks"),
        "{line}"
    );
    assert!(!line.contains("rows"));
    let tight = Some((2 * 1000 * new_track_bytes()) as u64 - 1);
    assert!(
        add_tracks_preflight("1000", 5, tight)
            .unwrap_err()
            .contains("memory")
    );
    let mut d = AddTracksDraft {
        at: AddTracksAt::End,
        place: String::new(),
        count: "16".into(),
        existing: 5,
        available: None,
        error: None,
        focused: true,
        confirm: None,
    };
    assert_eq!(d.press(), Some((AddTracksAt::End, 16)), "small: at once");
    d.count = ADD_TRACKS_CONFIRM.to_string();
    assert_eq!(d.press(), None, "large: asks first");
    assert_eq!(d.pending(), Some(ADD_TRACKS_CONFIRM));
    d.count = "5000".into();
    assert_eq!(d.pending(), None, "a changed count lapses");
    assert_eq!(d.press(), None);
    assert_eq!(d.press(), Some((AddTracksAt::End, 5000)));
    d.count = "0".into();
    assert_eq!(d.press(), None);
    assert!(d.error.is_some());
}

/// B2-F03: a large count through the app — 20,000 tracks at the end in one
/// step; the list scrolls to the first new track (out of view), draws only
/// the visible rows, one frame's cost stays bounded; Undo / Redo; the
/// project saves and reopens with them.
#[test]
fn a_large_add_tracks_scrolls_to_them_and_keeps_frames_bounded() {
    use plumspirit_engine::editor::folders::AddTracksAt;
    let size = egui::vec2(1280.0, 800.0);
    let mut h = Harness::new(fixture());
    h.set_track(2);
    h.app.main_view = MainView::PianoRoll;
    let _ = h.app_frame(size, 1.0);
    let t = std::time::Instant::now();
    h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(
        AddTracksAt::End,
        20_000,
    ));
    let add = t.elapsed();
    assert_eq!(h.doc().track_count(), 20_005);
    assert_eq!(h.app.active_track(), 5);
    let t = std::time::Instant::now();
    let _ = h.app_frame(size, 1.0);
    let first = t.elapsed();
    let t = std::time::Instant::now();
    let _ = h.app_frame(size, 1.0);
    let next = t.elapsed();
    let g = &h.app.row_geometry;
    let top = g.y_of(6);
    assert!(
        top >= g.list.top() && top + g.row_h <= g.list.bottom(),
        "row of track 5 (after the folder header) in view: {top} in {:?}",
        g.list
    );
    assert!(
        g.rows.len() < 100,
        "only the visible rows: {}",
        g.rows.len()
    );
    assert_eq!(g.scroll, 0.0, "in view already: no scroll");
    // From the Conductor, more at the end: row 20,006, out of view — the
    // list scrolls to it.
    h.set_conductor();
    h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(
        AddTracksAt::End,
        10,
    ));
    assert_eq!(h.app.active_track(), 20_005);
    let _ = h.app_frame(size, 1.0);
    let g = &h.app.row_geometry;
    let top = g.y_of(20_006);
    assert!(g.scroll > 0.0, "scrolled");
    assert!(
        top >= g.list.top() && top + g.row_h <= g.list.bottom(),
        "the first new row in view: {top} in {:?}",
        g.list
    );
    undo(&mut h);
    println!("add 20,000 tracks: {add:?}; first frame: {first:?}; next frame: {next:?}");
    // Debug build; measured release numbers are in docs/PERFORMANCE.md.
    assert!(add.as_secs() < 30 && first.as_secs() < 30 && next.as_secs() < 30);
    undo(&mut h);
    assert_eq!(h.doc().track_count(), 5);
    assert!(h.command(Command::Redo));
    assert_eq!(h.doc().track_count(), 20_005);
    let bytes = plumspirit_engine::native::project_to_bytes(h.doc()).unwrap();
    let (r, _) = plumspirit_engine::native::project_from_bytes(&bytes).unwrap();
    assert_eq!(ids_of(&r), ids_of(h.doc()));
}

fn ids_of(d: &PlumDocument) -> Vec<TrackId> {
    d.tracks().iter().map(|t| t.id()).collect()
}

/// B2-S27 measurement (prints; run with `--ignored --nocapture`, in
/// release for the numbers in docs/PERFORMANCE.md): the first frame after
/// Add Tracks… of 5,000 – 40,000 tracks on a project with clips grows
/// linearly with the count (it was quadratic: the analysis inputs).
#[test]
#[ignore]
fn add_tracks_first_frame_cost() {
    use plumspirit_engine::editor::folders::AddTracksAt;
    for n in [5_000usize, 10_000, 20_000, 40_000] {
        let size = egui::vec2(1280.0, 800.0);
        let mut h = Harness::new(fixture());
        h.set_track(2);
        h.app.main_view = MainView::PianoRoll;
        let _ = h.app_frame(size, 1.0);
        h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(
            AddTracksAt::End,
            n,
        ));
        let t = std::time::Instant::now();
        let _ = h.app_frame(size, 1.0);
        println!("{n} tracks added: first frame {:?}", t.elapsed());
    }
}

/// B2-F03: with Add Tracks… beside Add Track, every button under the
/// track panel stays whole inside the panel (the row wraps), at 100 % and
/// at a 150 % monitor scale.
#[test]
fn the_track_panels_buttons_all_fit_beside_add_tracks() {
    fn text_in(
        shapes: &[egui::epaint::ClippedShape],
        label: &str,
    ) -> Option<(egui::Rect, egui::Rect)> {
        fn find(s: &egui::Shape, label: &str) -> Option<egui::Rect> {
            match s {
                egui::Shape::Text(t) if t.galley.text() == label => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                _ => None,
            }
        }
        shapes
            .iter()
            .find_map(|c| find(&c.shape, label).map(|r| (r, c.clip_rect)))
    }
    for native in [1.0, 1.5] {
        let mut h = Harness::new(fixture());
        h.set_track(2);
        h.app.main_view = MainView::PianoRoll;
        let _ = h.app_frame(egui::vec2(1280.0, 800.0), native);
        let shapes = h.app_frame(egui::vec2(1280.0, 800.0), native);
        for label in ["+ Add Track", "Add Tracks…", "Delete Track"] {
            let (r, clip) = text_in(&shapes, label).unwrap_or_else(|| panic!("{label}"));
            assert!(
                clip.contains_rect(r) && r.right() <= crate::ui::track_panel::PANEL_WIDTH,
                "{native}: “{label}” at {r:?} clipped by {clip:?}"
            );
        }
    }
}

/// B2-F03: palette cycling — 32 tracks added below track 2 take the track
/// colors in order: Curated by id (24 distinct colors in a row), Rainbow-16
/// by place (each new track differs from the one before; the 17th repeats
/// the first), the tracks after them shift on by 32 places.
#[test]
fn added_tracks_cycle_the_track_palette_in_order() {
    use crate::ui::theme::{TrackColorMode, TrackPalette};
    use plumspirit_engine::editor::folders::AddTracksAt;
    let mut h = Harness::new(fixture());
    h.set_track(2);
    h.dialog_outcome(super::dialogs::DialogOutcome::AddTracks(
        AddTracksAt::Below(2),
        32,
    ));
    let ids: Vec<u32> = h.doc().tracks().iter().map(|t| t.id().0).collect();
    let new = &ids[3..35];
    for mode in [TrackColorMode::Curated, TrackColorMode::Rainbow16] {
        let p = TrackPalette::new(mode, ids.iter().copied());
        let c: Vec<egui::Color32> = new.iter().map(|&id| p.color(id, None, true, 255)).collect();
        assert!(
            c.windows(2).all(|w| w[0] != w[1]),
            "{mode:?}: neighbors differ"
        );
        match mode {
            TrackColorMode::Rainbow16 => {
                assert!((0..16).all(|k| c[k] == c[k + 16]), "16-cycle by place");
                let after = p.color(ids[35], None, true, 255);
                assert_eq!(after, c[0], "track 3 moved on by 32 places");
            }
            _ => {
                let distinct: std::collections::HashSet<_> = c[..24].iter().collect();
                assert_eq!(distinct.len(), 24, "Curated: 24 colors in a row");
            }
        }
    }
}

/// B2-S34: quick routes preserve selection lockouts and clip ownership.
#[test]
fn simple_transforms_keep_context_lockouts_and_one_undo() {
    for c in ALL {
        for cmd in [Command::FlipX, Command::FlipY] {
            let mut d = fixture();
            for i in 0..4 {
                d.track_mut(i)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&[note(201, 73)], false)
                    .unwrap();
            }
            let mut h = Harness::new(d);
            enter(&mut h, c);
            h.app.main_view = MainView::PianoRoll;
            if matches!(c, Ctx::Conductor | Ctx::Nothing) {
                assert!(!h.app.enabled(cmd), "{c:?}: no editable selection");
                continue;
            }
            h.command(Command::SelectAll);
            let before = h.doc().fingerprint();
            let targets = h.app.selection_targets().len();
            assert!(h.command(cmd), "{c:?}: quick route enabled");
            if targets > 1 {
                assert_eq!(
                    h.doc().fingerprint(),
                    before,
                    "{c:?}: never partly edit several clips"
                );
            } else {
                assert_ne!(
                    h.doc().fingerprint(),
                    before,
                    "{c:?}: quick transform applied"
                );
                undo(&mut h);
                assert_eq!(h.doc().fingerprint(), before, "{c:?}: exact Undo");
                assert!(h.command(Command::Redo));
                assert_ne!(h.doc().fingerprint(), before);
            }
        }
    }
    // No selected note: every Simple command stays disabled.
    let mut h = Harness::new(fixture());
    h.set_track(2);
    for cmd in crate::ui::menu::SIMPLE_COMMANDS {
        assert!(!h.app.enabled(cmd));
    }
}

#[test]
fn simple_scale_keeps_factor_preview_and_quantize_shortcut_opens_one_tool() {
    let mut h = Harness::new(fixture());
    h.set_notes(0);
    let before = h.doc().fingerprint();
    for (cmd, axis) in [
        (Command::ScaleX, super::dialogs::ScaleAxis::X),
        (Command::ScaleY, super::dialogs::ScaleAxis::Y),
    ] {
        assert!(h.command(cmd));
        assert!(matches!(
            h.app.dialog,
            Some(Dialog::Scale { preview: true, .. })
        ));
        assert_eq!(h.doc().fingerprint(), before, "opening never edits");
        h.app.dialog = None;
        h.app
            .apply_scale(axis, "1.5", Default::default(), &h.ctx)
            .unwrap();
        if h.doc().fingerprint() != before {
            undo(&mut h);
        }
        assert_eq!(h.doc().fingerprint(), before);
    }
    h.keybinds_frame(egui::Key::Q, egui::Modifiers::COMMAND);
    assert!(h.app.dialog.is_none(), "legacy duplicate window retired");
    assert_eq!(h.app.tool.as_ref().unwrap().def.id, "quantize");
    assert_eq!(h.doc().fingerprint(), before);
    assert!(h.command(Command::OpenTool("quantize")));
    assert_eq!(h.app.tool.as_ref().unwrap().def.id, "quantize");
    assert_eq!(
        super::keybinds::command_from_id("Quantize"),
        Some(Command::Quantize)
    );
    assert_eq!(Command::OpenTool("flip").label(), "Advanced Flip");
}

#[test]
fn simple_menu_pointer_routes_keep_quick_actions_and_remove_edit_quantize() {
    fn frame(h: &mut Harness, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        crate::ui::testkit::texts(&h.app_frame_events(egui::vec2(1500.0, 1000.0), 1.0, events))
    }
    fn click(h: &mut Harness, name: &str) {
        let t = frame(h, vec![]);
        let p = t
            .iter()
            .find(|(s, _)| s == name)
            .unwrap_or_else(|| panic!("{name}: {t:?}"))
            .1;
        frame(
            h,
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ],
        );
    }
    let mut d = fixture();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(201, 73)], false)
        .unwrap();
    let mut h = Harness::new(d);
    h.set_notes(0);
    frame(&mut h, vec![]);
    let t = frame(&mut h, vec![]);
    assert!(!t.iter().any(|(s, _)| s == "Transform"));
    let before = h.doc().fingerprint();
    click(&mut h, "Tools");
    click(&mut h, "Simple");
    click(&mut h, "Scale X…");
    assert!(matches!(
        h.app.dialog,
        Some(Dialog::Scale { preview: true, .. })
    ));
    assert_eq!(h.doc().fingerprint(), before);
    h.app.dialog = None;
    click(&mut h, "Tools");
    click(&mut h, "Simple");
    click(&mut h, "Flip Y");
    assert_ne!(h.doc().fingerprint(), before);
    undo(&mut h);
    assert_eq!(h.doc().fingerprint(), before);
    click(&mut h, "Edit");
    let t = frame(&mut h, vec![]);
    assert!(!t.iter().any(|(s, _)| s == "Quantize…" || s == "Quantizer"));
}

#[test]
fn chopper_v2_legacy_names_focus_the_existing_session() {
    let mut h = Harness::new(fixture());
    h.set_notes(0);
    assert!(h.command(Command::OpenTool("repeat")));
    h.app.tool.as_mut().unwrap().scope = 2;
    h.app.tool.as_mut().unwrap().preview = false;
    for alias in ["Note Repeat", "Notes Repeat", "Chopper V2"] {
        h.app.open_tool(alias);
        let s = h.app.tool.as_ref().unwrap();
        assert_eq!(s.def.id, "repeat");
        assert_eq!(s.spec.name, "Chopper V2");
        assert_eq!(s.scope, 2, "an alias focuses rather than resets");
        assert!(!s.preview);
    }
}

// ------------------------------------------------------------------------
// v1.0-beta2 (B2-F29, contract §023): the Track List's keyboard, through the
// shortcut dispatcher (real key and clipboard events) in every context.
// ------------------------------------------------------------------------

fn steps(h: &Harness) -> usize {
    h.app.open.as_ref().map_or(0, |o| o.history.undo_len())
}

/// Every clip the Track List draws: (id, start) by track.
fn drawn_clips(h: &Harness) -> Vec<(ClipId, u64)> {
    let m = plumspirit_engine::view::track_list::TrackListModel::build(h.doc());
    (0..h.doc().track_count())
        .flat_map(|t| {
            m.row(t)
                .iter()
                .filter(|r| !r.empty)
                .map(|r| (r.id, r.start))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn start_of(h: &Harness, id: ClipId) -> u64 {
    drawn_clips(h)
        .into_iter()
        .find(|c| c.0 == id)
        .map(|c| c.1)
        .expect("the clip is drawn")
}

const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;
const NONE: egui::Modifiers = egui::Modifiers::NONE;

/// Ctrl+C / Ctrl+V / Ctrl+X arrive as clipboard events: in the Track List
/// they act on the selected clips (each edit one Undo step) and, with no
/// clip selected, say why and leave the hidden notes alone; in the Piano
/// Roll they act on the notes.
#[test]
fn clipboard_keys_act_on_the_selected_clips_in_the_track_list() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let fp = h.doc().state_fingerprint();
        let before = steps(&h);
        let clip_ctx = matches!(c, Ctx::Clip | Ctx::Clips);
        h.shortcut_events(vec![egui::Event::Copy]);
        match c {
            Ctx::Clip | Ctx::Clips => assert!(h.app.clip_clipboard.is_some(), "{c:?}"),
            Ctx::Notes => {
                assert!(h.app.clip_clipboard.is_none());
                assert!(h.app.clipboard.as_ref().is_some_and(|b| !b.is_empty()));
            }
            _ => {
                assert!(h.app.clip_clipboard.is_none(), "{c:?}");
                assert!(
                    h.app.clipboard.as_ref().is_none_or(|b| b.is_empty()),
                    "{c:?}"
                );
                assert!(
                    h.status().contains("Copy acts on the clips"),
                    "{c:?}: {}",
                    h.status()
                );
            }
        }
        if clip_ctx {
            let n = drawn_clips(&h).len();
            h.app.roll.cursor_tick = 7_680;
            h.shortcut_events(vec![egui::Event::Paste(String::new())]);
            assert!(drawn_clips(&h).len() > n, "{c:?}: pasted clips");
            assert_eq!(steps(&h), before + 1, "{c:?}: one Undo step");
            undo(&mut h);
            assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
            enter(&mut h, c);
            let sel = h.app.track_list.selected.len();
            let n = drawn_clips(&h).len();
            h.shortcut_events(vec![egui::Event::Cut]);
            assert_eq!(drawn_clips(&h).len(), n - sel, "{c:?}: Ctrl+X deletes them");
            assert_eq!(steps(&h), before + 1, "{c:?}");
            undo(&mut h);
            assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
        } else if c == Ctx::Notes {
            let n = h.app.selected_count();
            assert!(n > 0);
            h.shortcut_events(vec![egui::Event::Cut]);
            assert_eq!(h.app.selected_count(), 0, "Ctrl+X deletes the notes");
            assert_eq!(steps(&h), before + 1);
        } else {
            h.shortcut_events(vec![egui::Event::Paste(String::new())]);
            assert!(
                h.status().contains("pastes copied clips"),
                "{c:?}: {}",
                h.status()
            );
            h.shortcut_events(vec![egui::Event::Cut]);
            assert!(
                h.status().contains("Delete acts on the clips"),
                "{c:?}: {}",
                h.status()
            );
            assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}: nothing changed");
            assert_eq!(steps(&h), before, "{c:?}");
        }
    }
}

/// Ctrl+A (and Edit → Select All) selects every clip the Track List draws
/// there, and the notes in the Piano Roll; neither touches the other's
/// selection.
#[test]
fn select_all_selects_the_clips_in_the_track_list_and_the_notes_in_the_piano_roll() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let notes = h.app.selected_count();
        let clips = h.app.track_list.selected.clone();
        h.key(egui::Key::A, CTRL);
        if c == Ctx::Notes {
            assert_eq!(h.app.track_list.selected, clips, "clips untouched");
            assert!(h.app.selected_count() >= notes);
            continue;
        }
        let all: std::collections::BTreeSet<ClipId> =
            drawn_clips(&h).into_iter().map(|c| c.0).collect();
        assert!(all.len() >= 4, "{c:?}");
        assert_eq!(h.app.track_list.selected, all, "{c:?}: every clip");
        assert_eq!(h.app.selected_count(), notes, "{c:?}: notes untouched");
        assert_eq!(h.app.main_view, MainView::TrackList, "{c:?}");
        enter(&mut h, c);
        assert!(h.command(Command::SelectAll), "{c:?}: Edit → Select All");
        assert_eq!(h.app.track_list.selected, all, "{c:?}");
        assert_eq!(h.app.main_view, MainView::TrackList, "{c:?}");
    }
}

/// ← / → nudge the selected clips one Snap step (one beat with Snap off),
/// one Undo step each, keeping their ids; ← stops at the song's start and
/// says why when a clip is already there; with no clip selected the key
/// says why; the Piano Roll leaves the arrows alone.
#[test]
fn arrows_nudge_the_selected_clips_one_undo_step_each() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let fp = h.doc().state_fingerprint();
        let before = steps(&h);
        let (primary, placed) = clips(&h);
        let sel: Vec<ClipId> = h.app.track_list.selected.iter().copied().collect();
        let starts = |h: &Harness| -> Vec<u64> { sel.iter().map(|&id| start_of(h, id)).collect() };
        let was = starts(&h);
        h.key(egui::Key::ArrowRight, NONE);
        match c {
            Ctx::Clip | Ctx::Clips => {
                let beat = 480;
                let now = starts(&h);
                assert_eq!(
                    now,
                    was.iter().map(|s| s + beat).collect::<Vec<_>>(),
                    "{c:?}"
                );
                assert_eq!(steps(&h), before + 1, "{c:?}: one Undo step");
                assert_eq!(
                    h.app
                        .track_list
                        .selected
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    sel,
                    "{c:?}: the same clips stay selected"
                );
                h.key(egui::Key::ArrowLeft, NONE);
                assert_eq!(starts(&h), was, "{c:?}: back");
                assert_eq!(steps(&h), before + 2, "{c:?}");
                // With Snap 1/8 (240 ticks) the step follows it.
                h.app.track_list.snap = Some(240);
                h.key(egui::Key::ArrowRight, NONE);
                assert_eq!(starts(&h), was.iter().map(|s| s + 240).collect::<Vec<_>>());
                undo(&mut h);
                undo(&mut h);
                undo(&mut h);
                assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
                if c == Ctx::Clips {
                    // The track's own clip starts at 0: ← has no room.
                    assert_eq!(start_of(&h, primary), 0);
                    h.key(egui::Key::ArrowLeft, NONE);
                    assert!(
                        h.status().contains("beginning of the song"),
                        "{}",
                        h.status()
                    );
                    assert_eq!(h.doc().state_fingerprint(), fp);
                } else {
                    // A step larger than the room left stops at tick 0.
                    h.app.track_list.snap = Some(100_000);
                    h.key(egui::Key::ArrowLeft, NONE);
                    assert_eq!(start_of(&h, placed), 0);
                    undo(&mut h);
                }
            }
            Ctx::Notes => {
                h.key(egui::Key::ArrowLeft, NONE);
                assert_eq!(
                    h.doc().state_fingerprint(),
                    fp,
                    "the Piano Roll leaves ← / → alone"
                );
                assert!(!h.status().contains("udg"), "{}", h.status());
            }
            _ => {
                assert!(
                    h.status().contains("to nudge them"),
                    "{c:?}: {}",
                    h.status()
                );
                assert_eq!(h.doc().state_fingerprint(), fp, "{c:?}");
                assert_eq!(steps(&h), before, "{c:?}");
            }
        }
    }
}

/// Ctrl+Shift+F (View → Zoom to Selection) frames the selected clips in
/// the Track List and the selected notes in the Piano Roll; without a
/// selection it says why and keeps the view.
#[test]
fn zoom_to_selection_frames_the_selected_clips_or_notes() {
    let screen = egui::vec2(1400.0, 900.0);
    for c in ALL {
        let mut h = Harness::new(fixture());
        h.app.zoom.animate = false;
        // Both views drawn once (their widths known), then the context.
        h.app_frame(screen, 1.0);
        enter(&mut h, c);
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        enter(&mut h, c);
        let (tl, roll) = (h.app.track_list.view, h.app.roll.view);
        h.key(
            egui::Key::F,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        );
        match c {
            Ctx::Clip | Ctx::Clips => {
                let sel = h.app.track_list.selection();
                let t0 = sel.iter().map(|r| r.start).min().unwrap();
                let t1 = sel.iter().map(|r| r.end).max().unwrap();
                let t = h.app.track_list.view.time;
                let (x0, x1) = (t.tick_to_x(t0), t.tick_to_x(t1));
                let w = h.app.track_list.width();
                assert!(x0 >= 0.0 && x1 <= w, "{c:?}: {x0} .. {x1} in {w}");
                assert!(x1 - x0 > w * 0.8, "{c:?}: the selection fills the width");
            }
            Ctx::Notes => {
                // Track 2's notes play in its own clip and in the linked copy
                // at 3,840 (the Track Aggregate): 0 .. 3,840 + 1,880.
                let t = h.app.roll.view.time;
                let (x0, x1) = (t.tick_to_x(0), t.tick_to_x(3_840 + 7 * 240 + 200));
                let w = h.app.roll.view_width();
                assert!(x0 >= 0.0 && x1 <= w, "{x0} .. {x1} in {w}");
                assert!(x1 - x0 > w * 0.8, "{x0} .. {x1} in {w}: {}", h.status());
            }
            _ => {
                assert!(
                    h.status().contains("to zoom to them"),
                    "{c:?}: {}",
                    h.status()
                );
                assert_eq!(h.app.track_list.view.time, tl.time, "{c:?}");
                assert_eq!(h.app.roll.view.time, roll.time, "{c:?}");
            }
        }
    }
}

/// Ctrl+Shift+Home (View → Jump to Playhead) brings an off-screen playhead
/// into the view shown, in both views, and resumes a paused Follow.
#[test]
fn jump_to_playhead_brings_the_playhead_into_either_view() {
    let screen = egui::vec2(1400.0, 900.0);
    for view in [MainView::TrackList, MainView::PianoRoll] {
        let mut h = Harness::new(fixture());
        h.app_frame(screen, 1.0);
        h.app.main_view = view;
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        assert_eq!(h.app.main_view, view);
        h.app.seek_to(1_920);
        h.app.track_list.view.time.origin_tick = 500_000;
        h.app.roll.view.time.origin_tick = 500_000;
        h.app.follow_suspended = true;
        let doc_before = h.doc().state_fingerprint();
        h.key(
            egui::Key::Home,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        );
        let (t, w) = if view == MainView::TrackList {
            (h.app.track_list.view.time, h.app.track_list.width())
        } else {
            (h.app.roll.view.time, h.app.roll.view_width())
        };
        let x = t.tick_to_x(1_920);
        assert!((0.0..=w).contains(&x), "{view:?}: {x} in {w}");
        assert!(!h.app.follow_suspended, "{view:?}");
        assert_eq!(
            h.app.transport.position(),
            1_920,
            "{view:?}: only the view moves"
        );
        assert_eq!(h.doc().state_fingerprint(), doc_before);
    }
}

/// The new keys are table rows (rebindable, no shared chord), the Track
/// List meanings follow the commands, and the menus name the live keys.
#[test]
fn the_track_list_keys_are_rebindable_rows_with_their_meanings() {
    use super::keybinds::{first_key, track_list_meaning};
    assert_eq!(first_key(Command::NudgeClipsLeft), "←");
    assert_eq!(first_key(Command::NudgeClipsRight), "→");
    assert_eq!(first_key(Command::ZoomToSelection), "Ctrl+Shift+F");
    assert_eq!(first_key(Command::JumpToPlayhead), "Ctrl+Shift+Home");
    for c in [
        Command::Delete,
        Command::SelectAll,
        Command::SliceNotes,
        Command::OpenTool("duplicate"),
        Command::PasteRepeat,
    ] {
        assert!(track_list_meaning(c).is_some(), "{c:?}");
        assert!(super::keybinds::bindable().contains(&c), "{c:?}");
    }
    assert!(track_list_meaning(Command::Undo).is_none());
    for c in [
        Command::NudgeClipsLeft,
        Command::NudgeClipsRight,
        Command::ZoomToSelection,
        Command::JumpToPlayhead,
    ] {
        assert!(super::keybinds::bindable().contains(&c), "{c:?}");
        assert!(!c.acts_on_notes(), "{c:?}: never a note command");
    }
}

/// L-S33 (contract §023, L-I01): Play Only Selected Tracks and Restore All
/// Tracks (L-S20) from every context. The tracks it plays are those the
/// track actions act on (the selection, else the current track — a
/// parent's own notes, as its Mute / Solo); from the Conductor with
/// nothing selected it says what to do and changes nothing. No Mute /
/// Solo flag changes, nothing is dirtied, Restore All brings the song back.
#[test]
fn play_only_selected_and_restore_all_work_from_every_context() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        let flags: Vec<(bool, bool)> = h.doc().tracks().iter().map(|t| (t.mute, t.solo)).collect();
        let dirty = h.app.open.as_ref().unwrap().dirty();
        assert!(h.app.enabled(Command::PlayOnlySelected), "{c:?}");
        assert!(
            !h.app.enabled(Command::RestoreAllTracks),
            "{c:?}: nothing to restore"
        );
        assert!(h.command(Command::PlayOnlySelected), "{c:?}");
        let want: Option<Vec<usize>> = match c {
            Ctx::Conductor => None,
            Ctx::Subtrack => Some(vec![1]),
            Ctx::FolderMember => Some(vec![3]),
            Ctx::Tracks => Some(vec![2, 4]),
            _ => Some(vec![2]),
        };
        let monitor = h.doc().project().monitor.clone();
        match &want {
            None => {
                assert_eq!(monitor, None, "{c:?}");
                let s = h.app.status.as_ref().expect("a message");
                assert!(
                    s.text.starts_with("Select the tracks to play first"),
                    "{c:?}: {}",
                    s.text
                );
                assert!(!h.app.enabled(Command::RestoreAllTracks), "{c:?}");
            }
            Some(w) => {
                let mut ids: Vec<TrackId> = w.iter().map(|&i| h.id(i)).collect();
                ids.sort_unstable();
                assert_eq!(monitor, Some(ids), "{c:?}");
                let heard: Vec<usize> = plumspirit_engine::playback::eligibility(h.doc())
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &e)| e.then_some(i))
                    .collect();
                assert_eq!(&heard, w, "{c:?}: exactly these are heard");
                assert!(h.app.enabled(Command::RestoreAllTracks), "{c:?}");
                assert!(h.command(Command::RestoreAllTracks), "{c:?}");
                assert_eq!(h.doc().project().monitor, None, "{c:?}");
            }
        }
        let after: Vec<(bool, bool)> = h.doc().tracks().iter().map(|t| (t.mute, t.solo)).collect();
        assert_eq!(after, flags, "{c:?}: no Mute / Solo flag changed");
        assert_eq!(
            h.app.open.as_ref().unwrap().dirty(),
            dirty,
            "{c:?}: not dirtied"
        );
    }
}

/// L-S33: the current track's Mute / Solo (L-F10, the toolbar's large
/// M / S and their keys) act on the track the context makes current — the
/// subtrack itself, the folder member, the clip's track — and are off for
/// the Conductor; a second press restores the flag.
#[test]
fn current_track_mute_and_solo_work_from_every_context_and_not_on_the_conductor() {
    for c in ALL {
        let mut h = Harness::new(fixture());
        enter(&mut h, c);
        for cmd in [Command::MuteCurrentTrack, Command::SoloCurrentTrack] {
            let Some(t) = active_of(c) else {
                assert!(!h.app.enabled(cmd), "{c:?}: {cmd:?} off on the Conductor");
                continue;
            };
            let flag = |h: &Harness, i: usize| {
                let tr = &h.doc().tracks()[i];
                if cmd == Command::MuteCurrentTrack {
                    tr.mute
                } else {
                    tr.solo
                }
            };
            let before: Vec<bool> = (0..h.doc().track_count()).map(|i| flag(&h, i)).collect();
            assert!(h.command(cmd), "{c:?}: {cmd:?}");
            let now: Vec<bool> = (0..h.doc().track_count()).map(|i| flag(&h, i)).collect();
            let changed: Vec<usize> = (0..now.len()).filter(|&i| now[i] != before[i]).collect();
            assert_eq!(
                changed,
                vec![t],
                "{c:?}: {cmd:?} changes the current track only"
            );
            assert!(h.command(cmd), "{c:?}: {cmd:?} again");
            let back: Vec<bool> = (0..h.doc().track_count()).map(|i| flag(&h, i)).collect();
            assert_eq!(back, before, "{c:?}: {cmd:?} twice restores");
        }
    }
}

/// L-S33: an empty project (one empty track) and a project with only the
/// Conductor selected: Play Only Selected plays the empty track or says
/// what to do; Selection Analytics Charts open without a selection and say
/// so instead of analyzing anything.
#[test]
fn launch_actions_in_an_empty_project_do_what_they_say() {
    let mut h = Harness::new(PlumDocument::new_project("empty".into(), Bpm::DEFAULT, 480));
    h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
    assert!(h.command(Command::PlayOnlySelected));
    assert_eq!(h.doc().project().monitor, Some(vec![h.id(0)]));
    assert!(h.command(Command::RestoreAllTracks));
    assert_eq!(h.doc().project().monitor, None);
    assert!(h.command(Command::MuteCurrentTrack));
    assert!(h.doc().tracks()[0].mute);
    assert!(h.command(Command::MuteCurrentTrack));
    h.set_conductor();
    assert!(h.command(Command::PlayOnlySelected));
    assert_eq!(h.doc().project().monitor, None);
    assert!(!h.app.enabled(Command::MuteCurrentTrack));
    let before = h.app.settings.selection_advanced;
    assert!(h.command(Command::ToggleSelectionCharts));
    assert_ne!(h.app.settings.selection_advanced, before);
    for _ in 0..3 {
        h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
    }
    assert!(
        h.app.sel_charts.chart.columns.is_empty(),
        "no selection: nothing analyzed"
    );
}

/// L-S33 (§023 "selection across Clips"): clips selected on several tracks
/// in the Track List — Play Only Selected Tracks plays the tracks those
/// clips are on (not only the track of the first one).
#[test]
fn play_only_selected_plays_the_tracks_of_clips_selected_across_tracks() {
    let mut h = Harness::new(fixture());
    let (primary, placed) = clips(&h);
    let other = ClipId::Primary(h.id(0));
    h.set_clips(&[other, primary, placed]);
    h.app.main_view = MainView::TrackList;
    assert!(h.command(Command::PlayOnlySelected));
    let mut want = vec![h.id(0), h.id(2)];
    want.sort_unstable();
    assert_eq!(h.doc().project().monitor, Some(want));
    assert!(h.command(Command::RestoreAllTracks));
    // In the Piano Roll the clip selection of the Track List does not count.
    h.app.main_view = MainView::PianoRoll;
    assert!(h.command(Command::PlayOnlySelected));
    assert_eq!(h.doc().project().monitor, Some(vec![h.id(0)]));
}
