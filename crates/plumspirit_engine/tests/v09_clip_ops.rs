//! V09-S07 (contract §050): clip edits are undoable operations — one step
//! each, exact undo / redo, journaled across a restart — and linked
//! instances share their content while independent copies do not.

use plumspirit_engine::editor::clips::{
    add_clip, delete_clips, duplicate_clips, move_clips, recolor_clips, rename_clip, resize_clip,
    set_muted,
};
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, ClipId, ClipLayout, ContentId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord,
    PlumDocument, TrackId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use std::path::{Path, PathBuf};

fn note(t: u64, p: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        240,
        NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Three tracks with notes and a controller each.
fn song() -> PlumDocument {
    let mut d = PlumDocument::new_project("clips".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    d.add_track().unwrap();
    for i in 0..3 {
        let recs: Vec<NoteRecord> = (0..64u64)
            .map(|k| note(k * 120, 40 + (k % 30) as u8 + i as u8))
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    d
}

fn tid(d: &PlumDocument, i: usize) -> TrackId {
    d.track(i).unwrap().id()
}

fn placed(d: &PlumDocument, n: u32) -> ClipId {
    let _ = d;
    ClipId::Placed(n)
}

/// Applies `op` as one undo step and checks the table.
fn apply(d: &mut PlumDocument, h: &mut History, plan: impl FnOnce(&PlumDocument) -> Operation) {
    let before = h.undo_len();
    let op = plan(d);
    h.apply(d, op).unwrap();
    assert_eq!(h.undo_len(), before + 1, "one undo step");
    d.validate_clips().unwrap();
}

/// Every clip edit, each followed by its state.
fn clip_edits(d: &mut PlumDocument, h: &mut History) -> Vec<(u64, ClipLayout)> {
    let mut states = vec![(d.state_fingerprint(), d.clips().layout())];
    let mut push = |d: &PlumDocument| states.push((d.state_fingerprint(), d.clips().layout()));
    let (t0, t2) = (tid(d, 0), tid(d, 2));
    // Add Piano Roll Clip on track 2 (an owned, empty content).
    apply(d, h, |d| add_clip(d, 1, 3840, 1920).unwrap());
    push(d);
    assert_eq!(d.clips().contents().len(), 1);
    // Move track 1's primary clip later.
    apply(d, h, |d| {
        move_clips(d, &[(ClipId::Primary(t0), 0, 960)]).unwrap()
    });
    push(d);
    // Trim its left edge (start and offset together) and give it a window.
    apply(d, h, |d| {
        resize_clip(d, ClipId::Primary(t0), 1440, 480, Some(2400)).unwrap()
    });
    push(d);
    apply(d, h, |d| {
        set_muted(d, &[ClipId::Primary(t2)], true).unwrap()
    });
    push(d);
    apply(d, h, |d| {
        rename_clip(d, ClipId::Primary(t0), Some("Intro".into())).unwrap()
    });
    push(d);
    apply(d, h, |d| {
        recolor_clips(d, &[placed(d, 0)], Some([200, 40, 90])).unwrap()
    });
    push(d);
    // A linked instance of track 1's content on track 3, an independent copy
    // of it on track 2.
    apply(d, h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(t0), 2, 9600)], true).unwrap()
    });
    push(d);
    apply(d, h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(t0), 1, 19200)], false).unwrap()
    });
    push(d);
    assert_eq!(d.clips().contents().len(), 2);
    // Delete the added clip (its owned content goes with it) and track 3's
    // primary clip.
    apply(d, h, |d| {
        delete_clips(d, &[placed(d, 0), ClipId::Primary(t2)]).unwrap()
    });
    push(d);
    assert_eq!(d.clips().contents().len(), 1);
    states
}

#[test]
fn every_clip_edit_is_one_step_and_undo_redo_reach_each_state_exactly() {
    let mut d = song();
    let mut h = History::new(HistoryLimits::default());
    let states = clip_edits(&mut d, &mut h);
    let strip = |l: &ClipLayout| ClipLayout {
        placements: l
            .placements
            .iter()
            .map(|p| plumspirit_engine::model::ClipPlacement {
                revision: 0,
                ..p.clone()
            })
            .collect(),
        ..l.clone()
    };
    for (fp, layout) in states.iter().rev().skip(1) {
        h.undo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), *fp);
        assert_eq!(strip(&d.clips().layout()), strip(layout));
        d.validate_clips().unwrap();
    }
    assert!(
        d.clips_are_identity(),
        "back to one identity clip per track"
    );
    for (fp, layout) in states.iter().skip(1) {
        h.redo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), *fp);
        assert_eq!(strip(&d.clips().layout()), strip(layout));
    }
}

#[test]
fn deleting_a_track_takes_its_clips_along_and_undo_restores_them() {
    let mut d = song();
    let mut h = History::new(HistoryLimits::default());
    let (t0, t1) = (tid(&d, 0), tid(&d, 1));
    // Track 2 hosts an added clip and shows a linked instance on track 3;
    // track 1's content is linked onto track 2.
    apply(&mut d, &mut h, |d| add_clip(d, 1, 0, 960).unwrap());
    apply(&mut d, &mut h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(t1), 2, 4800)], true).unwrap()
    });
    apply(&mut d, &mut h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(t0), 1, 9600)], true).unwrap()
    });
    let with = d.state_fingerprint();
    h.apply(&mut d, Operation::DeleteTrack { index: 1 })
        .unwrap();
    d.validate_clips().unwrap();
    let c = d.clips();
    assert!(
        c.explicit_placements()
            .iter()
            .all(|p| p.track != t1 && p.content != ContentRef::Track(t1))
    );
    assert!(
        c.contents().is_empty(),
        "the added clip's content left with it"
    );
    let without = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    assert_eq!(
        d.state_fingerprint(),
        with,
        "one undo restores track and clips"
    );
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), without);
}

/// §050: a linked instance is the same content (editing it edits every
/// instance); an independent copy is its own content.
#[test]
fn linked_instances_share_content_and_independent_copies_do_not() {
    let mut d = song();
    let mut h = History::new(HistoryLimits::default());
    let t0 = tid(&d, 0);
    apply(&mut d, &mut h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(t0), 1, 7680)], true).unwrap()
    });
    apply(&mut d, &mut h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(t0), 2, 7680)], false).unwrap()
    });
    let linked = d.clips().placement(ClipId::Placed(0)).unwrap();
    let copy = d.clips().placement(ClipId::Placed(1)).unwrap();
    assert_eq!(
        linked.content,
        ContentRef::Track(t0),
        "linked: same content"
    );
    assert_eq!(copy.content, ContentRef::Owned(ContentId(0)), "independent");
    let copied = |d: &PlumDocument| {
        d.clips()
            .content(ContentId(0))
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect::<Vec<_>>()
    };
    let original: Vec<NoteRecord> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect();
    assert_eq!(copied(&d), original, "the copy starts with the same notes");
    let rev = d.content_revision(linked.content).unwrap();
    // Edit the original: the linked instance shows the edit (it is the same
    // content), the independent copy does not.
    h.apply(
        &mut d,
        Operation::InsertNote {
            track: 0.into(),
            record: note(100_000, 90),
        },
    )
    .unwrap();
    assert_ne!(d.content_revision(linked.content).unwrap(), rev);
    assert_eq!(
        d.clips().placement(ClipId::Placed(0)).unwrap().content,
        ContentRef::Track(t0)
    );
    assert_eq!(copied(&d), original, "the independent copy is unchanged");
}

#[test]
fn invalid_clip_edits_are_refused_and_change_nothing() {
    let mut d = song();
    let mut h = History::new(HistoryLimits::default());
    let fp = d.state_fingerprint();
    assert!(add_clip(&d, 0, 0, 0).is_err(), "zero length");
    assert!(add_clip(&d, 9, 0, 960).is_err(), "no such track");
    assert!(
        move_clips(&d, &[(ClipId::Placed(5), 0, 0)]).is_err(),
        "no such clip"
    );
    assert!(resize_clip(&d, ClipId::Primary(tid(&d, 0)), 0, 0, Some(0)).is_err());
    assert!(rename_clip(&d, ClipId::Primary(tid(&d, 0)), Some("x".repeat(201))).is_err());
    // A layout that names a content nobody provides is refused by the history.
    let mut bad = d.clips().layout();
    bad.contents.push(ContentId(0));
    bad.next_content = 1;
    let r = h.apply(
        &mut d,
        Operation::SetClips {
            layout: Box::new(bad),
            contents: Vec::new(),
            label: "Bad",
        },
    );
    assert!(r.is_err());
    assert_eq!(d.state_fingerprint(), fp);
    assert_eq!(h.undo_len(), 0);
}

// ------------------------------------------------------------- restart

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plumspirit-clips-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn reopen(file: &Path, jpath: &Path) -> (PlumDocument, History) {
    let (doc, _) = load_project(file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(jpath, path_key(file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&doc)).unwrap();
    (doc, h)
}

/// The journal keeps clip edits (layouts and the owned contents they add or
/// remove): after "restart" every step undoes and redoes exactly.
#[test]
fn clip_history_survives_a_restart() {
    let d0 = dir("restart");
    let file = d0.join("song.lup");
    let jpath = d0.join("song.plh");
    let mut doc = song();
    save_project(&doc, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(identity(&doc))).unwrap();
    let states = clip_edits(&mut doc, &mut h);
    // Delete a track that hosts clips (clip records and the track in one step).
    h.apply(&mut doc, Operation::DeleteTrack { index: 1 })
        .unwrap();
    assert_eq!(states.len(), 10);
    save_project(&doc, &file, LupControl::default()).unwrap();
    let s = h.current_state();
    h.mark_saved(s);
    h.persist_saved(s, identity(&doc));
    let end = doc.state_fingerprint();
    let mut back = Vec::new();
    while h.can_undo() {
        h.undo(&mut doc).unwrap();
        back.push(doc.state_fingerprint());
    }
    while h.can_redo() {
        h.redo(&mut doc).unwrap();
    }
    assert_eq!(doc.state_fingerprint(), end);
    drop(h);
    // Restart: reopen the saved file with its journal.
    let (mut doc, mut h) = reopen(&file, &jpath);
    assert_eq!(doc.state_fingerprint(), end, "the saved file has the clips");
    assert!(!doc.clips_are_identity());
    for want in &back {
        h.undo(&mut doc).unwrap();
        assert_eq!(doc.state_fingerprint(), *want);
        doc.validate_clips().unwrap();
    }
    assert!(doc.clips_are_identity());
    while h.can_redo() {
        h.redo(&mut doc).unwrap();
    }
    assert_eq!(doc.state_fingerprint(), end);
    let _ = std::fs::remove_dir_all(&d0);
}

/// GUI journey M2 (V09-S15): an undone clip edit gives its content id back,
/// and the next new content takes it. Its content revision must still be
/// one no other note state had, or every cache keyed by (content id,
/// content revision) — the clip mini notes, analysis, subchannels, render —
/// serves the removed content's data for it (the merged clip drew the notes
/// of an undone Paste Repeat's transposed copy until the project reopened).
#[test]
fn a_reused_content_id_never_brings_back_a_content_revision() {
    let mut d = song();
    let mut h = History::new(HistoryLimits::default());
    let next = d.clips().next_ids().1;
    // An independent copy of track 1: content `next`.
    apply(&mut d, &mut h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(tid(d, 0)), 0, 20_000)], false).unwrap()
    });
    let first = d
        .content_revision(ContentRef::Owned(ContentId(next)))
        .unwrap();
    h.undo(&mut d).unwrap();
    // An independent copy of track 3 (other notes) gets the same id back.
    apply(&mut d, &mut h, |d| {
        duplicate_clips(d, &[(ClipId::Primary(tid(d, 2)), 2, 20_000)], false).unwrap()
    });
    assert_eq!(d.clips().next_ids().1, next + 1, "the id was reused");
    let second = d
        .content_revision(ContentRef::Owned(ContentId(next)))
        .unwrap();
    assert_ne!(first.0, second.0, "a new note state, a new revision");
    // A copy whose notes are its source's keeps the source's revision (the
    // same note state; its content key differs anyway).
    assert_eq!(second.0, d.track(2).unwrap().notes.content_revision());
    // Every track built its notes on its own: no two share a revision.
    let mut revs: Vec<u64> = (0..d.track_count())
        .map(|i| d.track(i).unwrap().notes.content_revision())
        .collect();
    revs.sort_unstable();
    revs.dedup();
    assert_eq!(revs.len(), d.track_count());
}
