//! v1.0-beta1 B1-0095 (contract §013-A, owner report + image7): a clip moved or
//! copied to another Track / Subtrack keeps its notes, and every track counts
//! the notes its clips play — never a false zero for the track that now hosts
//! the clip, never notes a track no longer plays.

use plumspirit_engine::editor::clips::{add_clip, duplicate_clips, move_clips, resize_clip};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TrackId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};

fn note(t: u64, p: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        240,
        NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Track 1 (index 0) with 4,282 notes, its Subtrack 1 (index 1, empty) and a
/// top-level Track 2 (index 2, empty) — the owner's image7 layout.
fn song() -> (PlumDocument, History) {
    let mut d = PlumDocument::new_project("clip counts".into(), Bpm::DEFAULT, 960);
    let mut h = History::new(HistoryLimits::default());
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    d.add_track().unwrap();
    assert_eq!(d.tracks()[1].parent(), Some(d.tracks()[0].id()));
    let recs: Vec<NoteRecord> = (0..4_282u64)
        .map(|k| note(k * 30, 30 + (k % 60) as u8))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    h.reset(true);
    (d, h)
}

fn tid(d: &PlumDocument, i: usize) -> TrackId {
    d.track(i).unwrap().id()
}

/// Exact notes track `i` plays: every placement's content notes whose start
/// lies in its window (the brute-force oracle).
fn oracle(d: &PlumDocument, i: usize) -> usize {
    d.placed_contents(i)
        .iter()
        .map(|pc| {
            let (lo, hi) = pc.window();
            pc.notes
                .iter()
                .filter(|n| n.start_tick >= lo && n.start_tick < hi)
                .count()
        })
        .sum()
}

fn counts(d: &PlumDocument) -> Vec<usize> {
    let each: Vec<usize> = (0..d.track_count())
        .map(|i| d.placed_note_count(i))
        .collect();
    assert_eq!(d.placed_note_counts(), each, "one pass equals per track");
    each
}

fn assert_counts(d: &PlumDocument, want: &[usize]) {
    let got = counts(d);
    assert_eq!(got, want);
    for (i, &n) in got.iter().enumerate() {
        assert_eq!(n, oracle(d, i), "track {i} against the oracle");
    }
}

#[test]
fn a_clip_moved_to_a_subtrack_is_counted_where_it_plays_and_undo_restores_the_counts() {
    let (mut d, mut h) = song();
    assert_counts(&d, &[4_282, 0, 0]);
    let t0 = tid(&d, 0);
    // The owner's report: Track 1's clip placed on Subtrack 1 showed 0 there.
    let op = move_clips(&d, &[(ClipId::Primary(t0), 1, 1920)]).unwrap();
    h.apply(&mut d, op).unwrap();
    d.validate_clips().unwrap();
    assert_counts(&d, &[0, 4_282, 0]);
    // The notes survived in the content (not only invisible).
    assert_eq!(d.track(0).unwrap().notes.len(), 4_282);
    // On to a top-level track, then undo / redo walk the same counts.
    let op = move_clips(&d, &[(ClipId::Primary(t0), 2, 0)]).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_counts(&d, &[0, 0, 4_282]);
    h.undo(&mut d).unwrap();
    assert_counts(&d, &[0, 4_282, 0]);
    h.undo(&mut d).unwrap();
    assert_counts(&d, &[4_282, 0, 0]);
    h.redo(&mut d).unwrap();
    assert_counts(&d, &[0, 4_282, 0]);
}

#[test]
fn copies_windows_and_empty_clips_count_exactly_what_they_play() {
    let (mut d, mut h) = song();
    let t0 = tid(&d, 0);
    // A linked copy on the subtrack and an independent copy on Track 2.
    let op = duplicate_clips(&d, &[(ClipId::Primary(t0), 1, 200_000)], true).unwrap();
    h.apply(&mut d, op).unwrap();
    let op = duplicate_clips(&d, &[(ClipId::Primary(t0), 2, 400_000)], false).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_counts(&d, &[4_282, 4_282, 4_282]);
    // A window on the primary: content ticks [960, 960 + 9600) start 320 notes.
    let op = resize_clip(&d, ClipId::Primary(t0), 960, 960, Some(9_600)).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_counts(&d, &[320, 4_282, 4_282]);
    // An added empty Piano Roll clip counts nothing (no fabricated count).
    let op = add_clip(&d, 2, 0, 1920).unwrap();
    h.apply(&mut d, op).unwrap();
    assert_counts(&d, &[320, 4_282, 4_282]);
    while h.undo_len() > 0 {
        h.undo(&mut d).unwrap();
    }
    assert_counts(&d, &[4_282, 0, 0]);
}

#[test]
fn moved_clip_counts_survive_save_and_reopen() {
    let (mut d, mut h) = song();
    let t0 = tid(&d, 0);
    let op = move_clips(&d, &[(ClipId::Primary(t0), 1, 960)]).unwrap();
    h.apply(&mut d, op).unwrap();
    let dir = std::env::temp_dir().join(format!("plumspirit-b1-counts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("moved.lup");
    save_project(&d, &file, LupControl::default()).unwrap();
    let (back, _) = load_project(&file, LupControl::default()).unwrap();
    assert_counts(&back, &[0, 4_282, 0]);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    let _ = std::fs::remove_dir_all(&dir);
}
