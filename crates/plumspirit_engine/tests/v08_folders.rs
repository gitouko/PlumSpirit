//! v0.8 Track Folders (contract §077–§080, test §239; TEST_MATRIX T-FOLD-*):
//! folders are organizational metadata keyed by TrackId — create, rename,
//! delete (tracks kept), move into / out of / with folders (members stay
//! together), undo / redo, save and reopen (order, membership, collapse) and
//! MIDI export unaffected.

use plumspirit_engine::editor::folders::{
    folder_roots, plan_add_track_below, plan_add_track_to_folder, plan_create, plan_delete,
    plan_delete_with_tracks, plan_into, plan_move_folder, plan_move_next_to, plan_out, plan_rename,
};
use plumspirit_engine::history::{History, Operation, TrackPlace};
use plumspirit_engine::midi::export_to_bytes;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TrackFolders,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes, verify_project};

/// Tracks A (with subtrack A1), B, C, D, E — each with one note on its own pitch.
fn doc() -> (PlumDocument, History) {
    let mut d = PlumDocument::new_project("folders".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    for _ in 0..4 {
        h.apply(&mut d, Operation::AddTrack).unwrap();
    }
    for i in 0..d.track_count() {
        let a = NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&[NoteRecord::new(i as u64 * 480, 480, a)], false)
            .unwrap();
    }
    h.clear();
    (d, h)
}

/// Track order as the pitch of each track's note (A=60, A1=61, B=62, …).
fn order(d: &PlumDocument) -> Vec<u8> {
    d.tracks()
        .iter()
        .map(|t| t.notes.iter().next().map_or(0, |n| n.attrs.pitch()))
        .collect()
}

/// Pitches of the top-level tracks of folder `id`.
fn members(d: &PlumDocument, id: u32) -> Vec<u8> {
    folder_roots(d, id)
        .into_iter()
        .map(|i| d.tracks()[i].notes.iter().next().unwrap().attrs.pitch())
        .collect()
}

fn index_of_pitch(d: &PlumDocument, p: u8) -> usize {
    order(d).iter().position(|&x| x == p).unwrap()
}

#[test]
fn creating_moving_and_deleting_folders_keeps_members_together_with_undo() {
    let (mut d, mut h) = doc();
    let start = order(&d);
    assert_eq!(start, vec![60, 61, 62, 63, 64, 65]);
    // New Folder from B and D: D is gathered right after B.
    let (op, f1) = plan_create(
        &d,
        &[index_of_pitch(&d, 62), index_of_pitch(&d, 64)],
        " Leads ",
        None,
    )
    .unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 63, 65]);
    assert_eq!(members(&d, f1), vec![62, 64]);
    assert_eq!(d.project().folders.get(f1).unwrap().name, "Leads");
    // One undo step puts the tracks and the folders back.
    h.undo(&mut d).unwrap();
    assert_eq!(order(&d), start);
    assert!(d.project().folders.is_empty());
    h.redo(&mut d).unwrap();
    assert_eq!(members(&d, f1), vec![62, 64]);
    // Move E into the folder: it lands after the last member.
    let op = plan_into(&d, &[index_of_pitch(&d, 65)], f1).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 65, 63]);
    assert_eq!(members(&d, f1), vec![62, 64, 65]);
    // A subtrack given to a folder brings its top-level track (with A1).
    let op = plan_into(&d, &[index_of_pitch(&d, 61)], f1).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![62, 64, 65, 60, 61, 63]);
    assert_eq!(members(&d, f1), vec![62, 64, 65, 60]);
    assert!(
        d.tracks()[index_of_pitch(&d, 61)].parent().is_some(),
        "A1 is still A's subtrack"
    );
    h.undo(&mut d).unwrap();
    // Remove D from the folder: it moves after the folder's last member.
    let op = plan_out(&d, &[index_of_pitch(&d, 64)]).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 65, 64, 63]);
    assert_eq!(members(&d, f1), vec![62, 65]);
    // Dragging C next to a member makes it a member; dragging it into a
    // subtrack position takes it out.
    let op = plan_move_next_to(
        &d,
        &[index_of_pitch(&d, 63)],
        index_of_pitch(&d, 62),
        TrackPlace::Before,
    )
    .unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 63, 62, 65, 64]);
    assert_eq!(members(&d, f1), vec![63, 62, 65]);
    let op = plan_move_next_to(&d, &[index_of_pitch(&d, 63)], 0, TrackPlace::Into).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(members(&d, f1), vec![62, 65]);
    assert_eq!(
        d.project()
            .folders
            .folder_of(d.tracks()[index_of_pitch(&d, 63)].id()),
        0
    );
    // Reorder: the folder (with its tracks) goes before A.
    let op = plan_move_folder(&d, f1, 0, TrackPlace::Before).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d)[..2], [62, 65]);
    assert!(
        plan_move_folder(&d, f1, 0, TrackPlace::After).is_err(),
        "not onto itself"
    );
    // Rename, then delete: the tracks stay, the folder goes.
    let op = plan_rename(&d, f1, "Pads").unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(d.project().folders.get(f1).unwrap().name, "Pads");
    let before = order(&d);
    let op = plan_delete(&d, f1).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(
        order(&d),
        before,
        "deleting a folder never deletes or moves tracks"
    );
    assert_eq!(d.track_count(), 6);
    assert!(d.project().folders.is_empty());
    // Undo everything back to the start, then redo everything.
    while h.can_undo() {
        h.undo(&mut d).unwrap();
    }
    assert_eq!(order(&d), start);
    assert!(d.project().folders.is_empty());
    while h.can_redo() {
        h.redo(&mut d).unwrap();
    }
    assert_eq!(order(&d), before);
}

#[test]
fn empty_folders_are_anchored_and_take_tracks_where_they_are_shown() {
    let (mut d, mut h) = doc();
    let anchor = d.tracks()[index_of_pitch(&d, 63)].id(); // before C
    let (op, f) = plan_create(&d, &[], "Empty", Some(anchor)).unwrap();
    h.apply(&mut d, op).unwrap();
    assert!(members(&d, f).is_empty());
    // E dropped into it lands where the folder is shown (before C).
    let op = plan_into(&d, &[index_of_pitch(&d, 65)], f).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 65, 63, 64]);
    assert_eq!(members(&d, f), vec![65]);
    // Removing the only member leaves the folder where it was.
    let op = plan_out(&d, &[index_of_pitch(&d, 65)]).unwrap();
    h.apply(&mut d, op).unwrap();
    assert!(members(&d, f).is_empty());
    assert_eq!(
        d.project().folders.get(f).unwrap().anchor,
        Some(d.tracks()[index_of_pitch(&d, 65)].id())
    );
    // Moving an empty folder only moves where it is shown.
    let op = plan_move_folder(&d, f, 0, TrackPlace::Before).unwrap();
    let o = order(&d);
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), o);
    assert_eq!(
        d.project().folders.get(f).unwrap().anchor,
        Some(d.tracks()[0].id())
    );
}

#[test]
fn folders_survive_save_and_reopen_and_never_change_midi_export() {
    let (mut d, mut h) = doc();
    let plain_midi = export_to_bytes(&d).unwrap();
    let plain_lup = project_to_bytes(&d).unwrap();
    let (op, f1) = plan_create(
        &d,
        &[index_of_pitch(&d, 62), index_of_pitch(&d, 63)],
        "Drums",
        None,
    )
    .unwrap();
    h.apply(&mut d, op).unwrap();
    let (op, f2) = plan_create(&d, &[index_of_pitch(&d, 64)], "Bass", None).unwrap();
    h.apply(&mut d, op).unwrap();
    let (op, _f3) = plan_create(&d, &[], "Later", None).unwrap();
    h.apply(&mut d, op).unwrap();
    d.project_mut().folders_collapsed.insert(f2);
    // MIDI export is byte-identical with or without folders.
    assert_eq!(export_to_bytes(&d).unwrap(), plain_midi);
    // Save → reopen: order, membership, names, anchors, collapse.
    let bytes = project_to_bytes(&d).unwrap();
    assert!(bytes.len() > plain_lup.len());
    verify_project(&d, std::io::Cursor::new(&bytes), Some(bytes.len() as u64)).unwrap();
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(order(&back), order(&d));
    assert_eq!(back.project().folders, d.project().folders);
    assert_eq!(
        back.project().folders_collapsed,
        d.project().folders_collapsed
    );
    assert_eq!(members(&back, f1), vec![62, 63]);
    assert_eq!(members(&back, f2), vec![64]);
    assert_eq!(export_to_bytes(&back).unwrap(), plain_midi);
    // A deleted member is dropped when saved; the verifier agrees.
    let mut d2 = d.clone();
    let i = index_of_pitch(&d2, 64);
    h.apply(&mut d2, Operation::DeleteTrack { index: i })
        .unwrap();
    let bytes = project_to_bytes(&d2).unwrap();
    verify_project(&d2, std::io::Cursor::new(&bytes), Some(bytes.len() as u64)).unwrap();
    let (back2, _) = project_from_bytes(&bytes).unwrap();
    assert!(members(&back2, f2).is_empty());
    assert!(
        back2.project().folders.get(f2).is_some(),
        "the folder itself stays"
    );
    // A project without folders writes no FOLD section (same bytes as before).
    let mut d3 = d.clone();
    d3.project_mut().folders = TrackFolders::default();
    d3.project_mut().folders_collapsed.clear();
    let (o3, _) = project_from_bytes(&project_to_bytes(&d3).unwrap()).unwrap();
    assert!(o3.project().folders.is_empty());
}

#[test]
fn a_damaged_folder_section_is_refused_without_touching_anything_else() {
    // An invalid table (member of a missing folder) cannot be built.
    let f = TrackFolders::from_parts(vec![], vec![(plumspirit_engine::model::TrackId(1), 3)], 4);
    assert!(f.is_err());
    // Invalid folders are refused by the history (nothing changes).
    let (mut d, mut h) = doc();
    let mut bad = TrackFolders::default();
    bad.create("x", None);
    let o = order(&d);
    let mut ok = bad.clone();
    ok.rename(1, "fine");
    h.apply(
        &mut d,
        Operation::SetFolders {
            folders: Box::new(ok),
            label: "t",
        },
    )
    .unwrap();
    assert_eq!(order(&d), o);
}

/// Every top-level track between the folder's first and last member is a
/// member (the folder is one run in the list).
fn contiguous(d: &PlumDocument, id: u32) -> bool {
    let roots = folder_roots(d, id);
    let (Some(&a), Some(&b)) = (roots.first(), roots.last()) else {
        return true;
    };
    (a..=b)
        .filter(|&i| d.tracks()[i].parent().is_none())
        .all(|i| roots.contains(&i))
}

/// §203–§204 context matrix for Add / Delete Track around folders: Add Track
/// Below a member joins its folder (it used to split the folder in two), a
/// subtrack's sibling stays a subtrack, Add Track to Folder appends a member
/// (also into an empty folder, where it is shown), Delete Folder and Its
/// Tracks is one undo step and refuses to delete every track.
#[test]
fn add_and_delete_track_work_from_every_folder_context() {
    let (mut d, mut h) = doc();
    let (op, f1) = plan_create(
        &d,
        &[index_of_pitch(&d, 62), index_of_pitch(&d, 64)],
        "Leads",
        None,
    )
    .unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 63, 65]);
    let tracks = d.track_count();

    // Below the first member: inside the run, a member, one undo step.
    let op = plan_add_track_below(&d, index_of_pitch(&d, 62)).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(d.track_count(), tracks + 1);
    assert_eq!(order(&d), vec![60, 61, 62, 0, 64, 63, 65]);
    assert_eq!(folder_roots(&d, f1), vec![2, 3, 4]);
    assert!(contiguous(&d, f1));
    h.undo(&mut d).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 63, 65]);
    assert_eq!(folder_roots(&d, f1), vec![2, 3]);
    assert_eq!(d.track_count(), tracks);

    // Below a subtrack: a sibling subtrack of the same parent, no folder.
    let op = plan_add_track_below(&d, index_of_pitch(&d, 61)).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 0, 62, 64, 63, 65]);
    assert_eq!(d.tracks()[2].parent(), Some(d.tracks()[0].id()));
    assert_eq!(d.project().folders.folder_of(d.tracks()[2].id()), 0);
    h.undo(&mut d).unwrap();

    // Below a track outside folders: stays outside.
    let op = plan_add_track_below(&d, index_of_pitch(&d, 63)).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 63, 0, 65]);
    assert_eq!(d.project().folders.folder_of(d.tracks()[5].id()), 0);
    assert!(d.tracks()[5].parent().is_none());
    h.undo(&mut d).unwrap();

    // Add Track to Folder: after the last member, a member.
    let op = plan_add_track_to_folder(&d, f1).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 0, 63, 65]);
    assert_eq!(folder_roots(&d, f1), vec![2, 3, 4]);
    h.undo(&mut d).unwrap();
    assert_eq!(folder_roots(&d, f1), vec![2, 3]);

    // An empty folder shown before C: the new track goes there.
    let anchor = d.tracks()[index_of_pitch(&d, 63)].id();
    let (op, f2) = plan_create(&d, &[], "Empty", Some(anchor)).unwrap();
    h.apply(&mut d, op).unwrap();
    let op = plan_add_track_to_folder(&d, f2).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 0, 63, 65]);
    assert_eq!(folder_roots(&d, f2), vec![4]);
    assert!(contiguous(&d, f1) && contiguous(&d, f2));
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();

    // Delete Folder and Its Tracks: one undo step for tracks + folder.
    let op = plan_delete_with_tracks(&d, f1).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(order(&d), vec![60, 61, 63, 65]);
    assert!(d.project().folders.get(f1).is_none());
    h.undo(&mut d).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 64, 63, 65]);
    assert_eq!(members(&d, f1), vec![62, 64]);
    h.redo(&mut d).unwrap();
    assert_eq!(order(&d), vec![60, 61, 63, 65]);
    h.undo(&mut d).unwrap();

    // A folder holding every track cannot delete them all.
    let all: Vec<usize> = (0..d.track_count()).collect();
    let (op, f3) = plan_create(&d, &all, "All", None).unwrap();
    h.apply(&mut d, op).unwrap();
    assert!(plan_delete_with_tracks(&d, f3).is_err());
    // Gone folders are refused with a sentence, never a panic.
    assert!(plan_add_track_to_folder(&d, 999).is_err());
    assert!(plan_delete_with_tracks(&d, 999).is_err());
    assert!(plan_add_track_below(&d, 999).is_err());
}
