//! v0.7 track and subchannel management (TEST_MATRIX T-CHAN-AUTO, T-TRACK-MOVE):
//! automatic MIDI channels for new subchannels, moving / reparenting subtrees
//! (undo, redo, cycles, orphans, save and reopen), adding and deleting tracks
//! and subtracks (T-TRACK-ADD, T-TRACK-DEL, T-TRACK-UNDO, T-TRACK-SAVE).

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
    SubchannelTable, TrackId, next_midi_channel,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};

#[test]
fn new_subchannels_take_the_next_midi_channel_and_can_skip_channel_10() {
    // 0-based channels: 1→2, 9→10, 10→11, 16→1.
    assert_eq!(next_midi_channel(0, false), 1);
    assert_eq!(next_midi_channel(8, false), 9);
    assert_eq!(next_midi_channel(9, false), 10);
    assert_eq!(next_midi_channel(15, false), 0);
    // Skip channel 10: 9→11, 16→1.
    assert_eq!(next_midi_channel(8, true), 10);
    assert_eq!(next_midi_channel(15, true), 0);
    let t = SubchannelTable::from_entries(vec![Subchannel::new(SubchannelId(0), 6)]).unwrap();
    assert_eq!(t.auto_channels(4, false), vec![7, 8, 9, 10]);
    assert_eq!(t.auto_channels(4, true), vec![7, 8, 10, 11]);
    // Many more than 16: always valid, cycling, channel 10 never used when skipped.
    let many = t.auto_channels(1000, true);
    assert!(many.iter().all(|&c| c < 16 && c != 9));
    assert_eq!(many[9], 1, "…15, 16, 1, 2 after channel 7");
    let all = t.auto_channels(48, false);
    for w in all.windows(2) {
        assert_eq!(w[1], (w[0] + 1) % 16);
    }
}

/// Tracks A (index 0) with subtrack A1, B, C (top level).
fn doc() -> (PlumDocument, History) {
    let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    h.clear();
    (d, h)
}

fn shape(d: &PlumDocument) -> Vec<(u32, Option<u32>)> {
    d.tracks()
        .iter()
        .map(|t| (t.id().0, t.parent().map(|p| p.0)))
        .collect()
}

#[test]
fn moving_and_reparenting_subtrees_is_one_undo_step_and_survives_save() {
    let (mut d, mut h) = doc();
    let start = shape(&d);
    let ids: Vec<TrackId> = d.tracks().iter().map(|t| t.id()).collect();
    let (a, a1, b, c) = (ids[0], ids[1], ids[2], ids[3]);
    assert_eq!(start[1].1, Some(a.0));
    // Move C to the top (before A).
    h.apply(
        &mut d,
        Operation::MoveTracks {
            from: 3,
            to: 0,
            parent: None,
        },
    )
    .unwrap();
    assert_eq!(
        shape(&d),
        vec![(c.0, None), (a.0, None), (a1.0, Some(a.0)), (b.0, None)]
    );
    // Move A with its subtrack after B: the subtree moves as a whole.
    h.apply(
        &mut d,
        Operation::MoveTracks {
            from: 1,
            to: 2,
            parent: None,
        },
    )
    .unwrap();
    assert_eq!(
        shape(&d),
        vec![(c.0, None), (b.0, None), (a.0, None), (a1.0, Some(a.0))]
    );
    // Reparent B under C (as C's child, right after it).
    h.apply(
        &mut d,
        Operation::MoveTracks {
            from: 1,
            to: 1,
            parent: Some(c),
        },
    )
    .unwrap();
    assert_eq!(shape(&d)[1], (b.0, Some(c.0)));
    d.validate_hierarchy().unwrap();
    // Save and reopen.
    let bytes = project_to_bytes(&d).unwrap();
    let (back, _) = project_from_bytes(&bytes).unwrap();
    assert_eq!(shape(&back), shape(&d));
    // Undo three times: the original tree; redo: the same moves again.
    let moved = shape(&d);
    for _ in 0..3 {
        h.undo(&mut d).unwrap();
    }
    assert_eq!(shape(&d), start);
    for _ in 0..3 {
        h.redo(&mut d).unwrap();
    }
    assert_eq!(shape(&d), moved);
}

#[test]
fn moves_that_would_create_cycles_or_orphans_are_refused_without_changes() {
    let (mut d, mut h) = doc();
    let start = shape(&d);
    let a1 = d.tracks()[1].id();
    // A into its own subtrack: a cycle.
    assert!(
        h.apply(
            &mut d,
            Operation::MoveTracks {
                from: 0,
                to: 1,
                parent: Some(a1)
            }
        )
        .is_err()
    );
    assert_eq!(shape(&d), start);
    // B under A but placed after C: the parent would not precede it.
    let a = d.tracks()[0].id();
    assert!(
        h.apply(
            &mut d,
            Operation::MoveTracks {
                from: 2,
                to: 3,
                parent: Some(a)
            }
        )
        .is_err()
    );
    assert_eq!(shape(&d), start);
    // Moving the subtrack out to the top level is fine (no orphan: it moves alone).
    h.apply(
        &mut d,
        Operation::MoveTracks {
            from: 1,
            to: 3,
            parent: None,
        },
    )
    .unwrap();
    assert_eq!(shape(&d)[3], (a1.0, None));
    assert!(h.can_undo());
}

#[test]
fn several_selected_subtrees_move_together_in_one_step() {
    use plumspirit_engine::history::{TrackPlace, plan_track_moves};
    let (mut d, mut h) = doc();
    let start = shape(&d);
    let ids: Vec<u32> = start.iter().map(|e| e.0).collect();
    let (a, a1, b, c) = (ids[0], ids[1], ids[2], ids[3]);
    // A (with A1) and C dropped before B: A, A1, C, B. A1 is inside A: ignored.
    let op = plan_track_moves(&d, &[0, 1, 3], 2, TrackPlace::Before).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(
        shape(&d),
        vec![(a, None), (a1, Some(a)), (c, None), (b, None)]
    );
    // B and C into A: they become A's last children, in order.
    let op = plan_track_moves(&d, &[2, 3], 0, TrackPlace::Into).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(
        shape(&d),
        vec![(a, None), (a1, Some(a)), (c, Some(a)), (b, Some(a))]
    );
    // Dropping A into its own child is refused.
    assert!(plan_track_moves(&d, &[0], 1, TrackPlace::Into).is_err());
    // One undo step per drop.
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(shape(&d), start);
}

#[test]
fn named_zoom_regions_are_saved_with_the_project() {
    use plumspirit_engine::model::project::ZoomRegion;
    let (mut d, _) = doc();
    d.project_mut()
        .zoom_regions
        .push(ZoomRegion::new("Intro", 960, 0.125, 40.5, 9.0));
    d.project_mut()
        .zoom_regions
        .push(ZoomRegion::new("Drop", 61_440, 0.03125, 12.0, 4.5));
    let bytes = project_to_bytes(&d).unwrap();
    let (back, _) = project_from_bytes(&bytes).unwrap();
    assert_eq!(back.project().zoom_regions, d.project().zoom_regions);
    assert_eq!(back.project().zoom_regions[1].pixels_per_tick(), 0.03125);
    // A damaged region (non-finite scale) is refused on load.
    let mut bad = d.clone();
    bad.project_mut().zoom_regions[0].ppt_bits = f64::NAN.to_bits();
    let bytes = project_to_bytes(&bad).unwrap();
    assert!(project_from_bytes(&bytes).is_err());
}

fn note(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 240,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

#[test]
fn adding_and_deleting_several_subtrees_is_undoable_and_survives_save() {
    let (mut d, mut h) = doc();
    // Notes in every track so a restore is visible in the fingerprint.
    for i in 0..d.track_count() {
        d.track_mut(i)
            .unwrap()
            .notes
            .insert(note(480 * i as u64, 60 + i as u8), false)
            .unwrap();
    }
    let ids: Vec<TrackId> = d.tracks().iter().map(|t| t.id()).collect();
    let (a, a1, b, c) = (ids[0], ids[1], ids[2], ids[3]);
    // Add: two subtracks under B (one step), then a track at the end.
    // Undoing an addition does not hand its track ids back (ids are never
    // reused), so the comparison leaves out the id counter.
    let before_add = (d.fingerprint(), shape(&d));
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 2,
            count: 2,
        },
    )
    .unwrap();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    assert_eq!(d.track_count(), 7);
    let b_kids: Vec<u32> = d
        .tracks()
        .iter()
        .filter(|t| t.parent() == Some(b))
        .map(|t| t.id().0)
        .collect();
    assert_eq!(b_kids.len(), 2);
    d.validate_hierarchy().unwrap();
    let added = d.state_fingerprint();
    // Delete the subtrees of A (A + A1) and B (B + its two subtracks) together,
    // last root first as the track panel does: one undo step.
    let undo_before = h.undo_len();
    h.apply(
        &mut d,
        Operation::Group {
            ops: vec![
                Operation::DeleteTrack { index: 2 },
                Operation::DeleteTrack { index: 0 },
            ],
            label: "Delete Tracks",
        },
    )
    .unwrap();
    assert_eq!(h.undo_len(), undo_before + 1);
    let left: Vec<u32> = d.tracks().iter().map(|t| t.id().0).collect();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0], c.0);
    assert!(!left.contains(&a1.0) && !left.contains(&a.0) && !left.contains(&b.0));
    d.validate_hierarchy().unwrap();
    let deleted = d.state_fingerprint();
    // Save and reopen the result.
    let (back, _) = project_from_bytes(&project_to_bytes(&d).unwrap()).unwrap();
    assert_eq!(back.state_fingerprint(), deleted);
    // Undo brings both subtrees back exactly (ids, parents, notes).
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), added);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), deleted);
    for _ in 0..3 {
        h.undo(&mut d).unwrap();
    }
    assert_eq!((d.fingerprint(), shape(&d)), before_add);
    // Deleted track ids are never reused by later additions.
    h.redo(&mut d).unwrap();
    h.redo(&mut d).unwrap();
    h.redo(&mut d).unwrap();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    let newest = d.tracks().last().unwrap().id().0;
    assert!(ids.iter().all(|t| t.0 != newest));
}
