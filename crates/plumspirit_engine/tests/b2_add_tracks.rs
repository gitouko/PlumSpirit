//! B2-F03 (contract §007): Add Tracks… — `count` empty tracks in one undo
//! step at a deterministic place (the end, below a track / subtrack at its
//! level, inside a folder), new ids in order, no clips (the Piano Roll's
//! lockout), refusals before any change, save / reopen, a large count.

use plumspirit_engine::editor::clips::default_pick;
use plumspirit_engine::editor::folders::{
    AddTracksAt, MAX_TRACKS, folder_roots, plan_add_tracks, plan_create,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TrackId};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};

/// Tracks A (with subtracks A1, A2), B, C — each with one note on its own
/// pitch (A=60, A1=61, A2=62, B=63, C=64).
fn doc() -> (PlumDocument, History) {
    let mut d = PlumDocument::new_project("add".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 2,
        },
    )
    .unwrap();
    for _ in 0..2 {
        h.apply(&mut d, Operation::AddTrack).unwrap();
    }
    for i in 0..d.track_count() {
        let a = NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&[NoteRecord::new(0, 480, a)], false)
            .unwrap();
    }
    h.clear();
    (d, h)
}

/// Each track's note pitch, 0 for an empty (new) track.
fn order(d: &PlumDocument) -> Vec<u8> {
    d.tracks()
        .iter()
        .map(|t| t.notes.iter().next().map_or(0, |n| n.attrs.pitch()))
        .collect()
}

/// Plans Add Tracks… and applies it as one history step.
fn add(d: &mut PlumDocument, h: &mut History, at: AddTracksAt, count: usize) {
    let op = plan_add_tracks(d, at, count).unwrap();
    h.apply(d, op).unwrap();
}

fn ids(d: &PlumDocument) -> Vec<u32> {
    d.tracks().iter().map(|t| t.id().0).collect()
}

fn index_of_pitch(d: &PlumDocument, p: u8) -> usize {
    order(d).iter().position(|&x| x == p).unwrap()
}

#[test]
fn counts_1_2_16_and_100_add_that_many_empty_tracks_at_the_end_in_one_undo_step() {
    for count in [1usize, 2, 16, 100] {
        let (mut d, mut h) = doc();
        let (before_ids, before_fp) = (ids(&d), d.fingerprint());
        let first = d.next_track_id();
        add(&mut d, &mut h, AddTracksAt::End, count);
        assert_eq!(d.track_count(), 5 + count);
        assert_eq!(&order(&d)[..5], &[60, 61, 62, 63, 64], "nothing replaced");
        let new: Vec<u32> = ids(&d)[5..].to_vec();
        assert_eq!(new, (first..first + count as u32).collect::<Vec<_>>());
        for i in 5..d.track_count() {
            let t = d.track(i).unwrap();
            assert!(t.parent().is_none() && t.notes.is_empty());
            assert!(default_pick(&d, i).is_none(), "no clip: the roll is locked");
        }
        assert_eq!(h.undo_len(), 1, "one undo step");
        d.validate_hierarchy().unwrap();
        h.undo(&mut d).unwrap();
        assert_eq!((ids(&d), d.fingerprint()), (before_ids.clone(), before_fp));
        h.redo(&mut d).unwrap();
        assert_eq!(&ids(&d)[5..], &new[..], "redo brings the same ids back");
    }
}

#[test]
fn below_a_track_or_subtrack_the_new_tracks_follow_its_subtree_at_its_level() {
    // Below A (with subtracks): after A2, top level.
    let (mut d, mut h) = doc();
    add(&mut d, &mut h, AddTracksAt::Below(0), 3);
    assert_eq!(order(&d), vec![60, 61, 62, 0, 0, 0, 63, 64]);
    assert!((3..6).all(|i| d.track(i).unwrap().parent().is_none()));
    d.validate_hierarchy().unwrap();
    // Below subtrack A1: siblings under A, between A1 and A2.
    let (mut d, mut h) = doc();
    let a = d.track(0).unwrap().id();
    add(&mut d, &mut h, AddTracksAt::Below(1), 2);
    assert_eq!(order(&d), vec![60, 61, 0, 0, 62, 63, 64]);
    assert!((2..4).all(|i| d.track(i).unwrap().parent() == Some(a)));
    assert_eq!(d.subtree_end(0), 5, "A's subtree holds them");
    d.validate_hierarchy().unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(order(&d), vec![60, 61, 62, 63, 64]);
}

#[test]
fn inside_a_folder_or_beside_its_member_the_new_tracks_join_the_folder() {
    let (mut d, mut h) = doc();
    let (op, f) = plan_create(&d, &[index_of_pitch(&d, 63)], "Leads", None).unwrap();
    h.apply(&mut d, op).unwrap();
    // The folder's context: after its last track, as members.
    add(&mut d, &mut h, AddTracksAt::Folder(f), 2);
    assert_eq!(order(&d), vec![60, 61, 62, 63, 0, 0, 64]);
    assert_eq!(folder_roots(&d, f), vec![3, 4, 5]);
    // Below a member (B): in the folder, right after B.
    add(&mut d, &mut h, AddTracksAt::Below(3), 1);
    assert_eq!(folder_roots(&d, f), vec![3, 4, 5, 6]);
    assert_eq!(order(&d), vec![60, 61, 62, 63, 0, 0, 0, 64]);
    // One step each: undo takes the members away again.
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(folder_roots(&d, f), vec![3]);
    assert_eq!(order(&d), vec![60, 61, 62, 63, 64]);
    // An empty folder shown before C: the tracks go there.
    let (op, g) = plan_create(&d, &[], "Empty", Some(d.track(4).unwrap().id())).unwrap();
    h.apply(&mut d, op).unwrap();
    add(&mut d, &mut h, AddTracksAt::Folder(g), 2);
    assert_eq!(order(&d), vec![60, 61, 62, 63, 0, 0, 64]);
    assert_eq!(folder_roots(&d, g), vec![4, 5]);
}

#[test]
fn refusals_are_sentences_and_change_nothing() {
    let (d, _) = doc();
    for (at, count) in [
        (AddTracksAt::End, 0),
        (AddTracksAt::End, MAX_TRACKS),
        (AddTracksAt::Below(99), 1),
        (AddTracksAt::Folder(42), 1),
    ] {
        let e = plan_add_tracks(&d, at, count).unwrap_err();
        assert!(e.ends_with('.') && e.len() > 20, "{e}");
    }
    let e = plan_add_tracks(&d, AddTracksAt::End, MAX_TRACKS).unwrap_err();
    assert!(e.contains("65,530 more"), "{e}");
}

#[test]
fn new_tracks_save_and_reopen_with_their_place_parents_and_folder() {
    let (mut d, mut h) = doc();
    let (op, f) = plan_create(&d, &[index_of_pitch(&d, 64)], "Pads", None).unwrap();
    h.apply(&mut d, op).unwrap();
    add(&mut d, &mut h, AddTracksAt::Below(1), 2);
    add(&mut d, &mut h, AddTracksAt::Folder(f), 3);
    let (r, _) = project_from_bytes(&project_to_bytes(&d).unwrap()).unwrap();
    assert_eq!(ids(&r), ids(&d));
    let parents = |d: &PlumDocument| -> Vec<Option<TrackId>> {
        d.tracks().iter().map(|t| t.parent()).collect()
    };
    assert_eq!(parents(&r), parents(&d));
    assert_eq!(folder_roots(&r, f), folder_roots(&d, f));
    assert_eq!(r.next_track_id(), d.next_track_id());
}

/// The defensible large count: up to the project's limit (65,535 tracks, a
/// MIDI file's 16-bit track count) in one insertion, undone and redone;
/// one more is refused.
#[test]
fn the_largest_count_fills_the_project_in_one_step_and_one_more_is_refused() {
    let (mut d, mut h) = doc();
    let room = MAX_TRACKS - d.track_count();
    let t = std::time::Instant::now();
    add(&mut d, &mut h, AddTracksAt::Below(1), room);
    let add = t.elapsed();
    assert_eq!(d.track_count(), MAX_TRACKS);
    assert_eq!(d.subtree_end(0), 3 + room, "all under A, in order");
    d.validate_hierarchy().unwrap();
    assert!(plan_add_tracks(&d, AddTracksAt::End, 1).is_err());
    let t = std::time::Instant::now();
    h.undo(&mut d).unwrap();
    h.redo(&mut d).unwrap();
    let cycle = t.elapsed();
    assert_eq!(d.track_count(), MAX_TRACKS);
    println!("add {room} tracks: {add:?}; undo + redo: {cycle:?}");
    // Generous for a debug build on a slow CI machine; measured numbers
    // are in docs/PERFORMANCE.md.
    assert!(add.as_secs() < 20 && cycle.as_secs() < 20);
}

/// B2-S27: with clips (not the identity) the analysis inputs of every track
/// come from one pass over the placements (a per-track lookup was
/// quadratic in the track count — 20 s for the first frame after adding
/// 40,000 tracks); each track's lanes still equal its unmuted placements.
#[test]
fn analysis_inputs_of_many_tracks_with_clips_equal_each_tracks_placements() {
    use plumspirit_engine::analysis::TrackInput;
    use plumspirit_engine::editor::clips::{duplicate_clips, set_muted};
    use plumspirit_engine::model::ClipId;
    let (mut d, mut h) = doc();
    let b = d.track(3).unwrap().id();
    // B's clip linked onto C at bar 3, and onto B itself at bar 5 (muted).
    let op = duplicate_clips(&d, &[(ClipId::Primary(b), 4, 3840)], true).unwrap();
    h.apply(&mut d, op).unwrap();
    let op = duplicate_clips(&d, &[(ClipId::Primary(b), 3, 7680)], true).unwrap();
    h.apply(&mut d, op).unwrap();
    let muted = d
        .clips()
        .layout()
        .placements
        .iter()
        .find(|p| p.start == 7680)
        .map(|p| p.id)
        .unwrap();
    let op = set_muted(&d, &[muted], true).unwrap();
    h.apply(&mut d, op).unwrap();
    add(&mut d, &mut h, AddTracksAt::Below(1), 20_000);
    assert!(!d.clips_are_identity());
    let t = std::time::Instant::now();
    let inputs = TrackInput::all(&d);
    println!(
        "analysis inputs of {} tracks: {:?}",
        d.track_count(),
        t.elapsed()
    );
    assert_eq!(inputs.len(), d.track_count());
    for (i, input) in inputs.iter().enumerate() {
        assert_eq!(input.id, d.track(i).unwrap().id());
        let want: Vec<(u64, u64, i128)> = d
            .placed_contents(i)
            .iter()
            .filter(|pc| !pc.placement.muted)
            .map(|pc| {
                let (lo, hi) = pc.window();
                (lo, hi, pc.shift())
            })
            .collect();
        let got: Vec<(u64, u64, i128)> = input
            .lanes
            .as_ref()
            .unwrap()
            .iter()
            .map(|l| (l.lo, l.hi, l.shift))
            .collect();
        assert_eq!(got, want, "track {i}");
    }
    let c = index_of_pitch(&d, 64);
    assert_eq!(
        inputs[c].lanes.as_ref().unwrap().len(),
        2,
        "C plays B's clip"
    );
    let b = index_of_pitch(&d, 63);
    assert_eq!(
        inputs[b].lanes.as_ref().unwrap().len(),
        1,
        "the muted copy is left out"
    );
}
