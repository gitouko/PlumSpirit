//! B1-S39 (contract §018, B1-0117): the General Note Overlap Prevention —
//! the v0.9 Remove Note Overlaps service with the notes that stay in the
//! track as partners (never changed), run on tool runs, prepared edits
//! (Paste, Scale / Flip / Quantize, in-place pitch remaps) and drawn notes;
//! exact / same-start / trim / remove, generated-only, across layers,
//! against existing notes or only among results; one undo step.

use plumspirit_engine::editor::{self, Clipboard, PreparedEdit};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::geom::{BoxScale, GeomBox};
use plumspirit_engine::transform::overlap::{
    General, Mode, Policy, prevent_in_edit, prevent_in_run, prevent_note,
};
use plumspirit_engine::transform::{Boundary, Limits, Source, TrackScope, run_kernel};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// `sel` selected, `rest` unselected, one track.
fn doc_with(sel: &[NoteRecord], rest: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b1s39".into(), Bpm::DEFAULT, 960);
    let n = &mut d.track_mut(0).unwrap().notes;
    for r in rest {
        n.insert(*r, false).unwrap();
    }
    for r in sel {
        n.insert(*r, true).unwrap();
    }
    d
}

/// (start, end, pitch) of every note, sorted.
fn notes(d: &PlumDocument) -> Vec<(u64, u64, u8)> {
    let mut v: Vec<_> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|r| {
            (
                r.start_tick,
                r.start_tick + r.duration_ticks,
                r.attrs.pitch(),
            )
        })
        .collect();
    v.sort_unstable();
    v
}

fn general(mode: Mode, existing: bool) -> General {
    General {
        policy: Policy {
            mode,
            generated_only: true,
            across: false,
        },
        existing,
    }
}

const SEL: [TrackScope; 1] = [TrackScope {
    index: 0,
    all: false,
    content: None,
}];

#[test]
fn the_default_is_stated_and_non_destructive() {
    let g = General::default();
    assert_eq!(g.policy.mode, Mode::RemoveOverlaps);
    assert!(g.policy.generated_only, "a note left as it was stays");
    assert!(!g.policy.across, "subchannels / channels are layers");
    assert!(g.existing, "compared with the notes already there");
}

#[test]
fn a_tool_run_never_lands_on_notes_that_stay_and_never_changes_them() {
    // Selected: 60 and 61 at 0..480; unselected: 62 at 240..720, 64 at 0..480.
    // Scale Y ×2 (rows 60 – 61 → 60 – 63, Fill off): 61 → 62 lands on the
    // unselected 62 (overlapping in time).
    let d = doc_with(
        &[rec(0, 480, 60), rec(0, 480, 61)],
        &[rec(240, 480, 62), rec(0, 480, 64)],
    );
    let from = GeomBox {
        t0: 0,
        t1: 480,
        p0: 60,
        p1: 61,
    };
    let k = BoxScale::plain(from, GeomBox { p1: 63, ..from });
    let src = Source::collect(&d, &SEL, 1 << 20).unwrap();
    let run0 = run_kernel(&k, &src, Boundary::default(), &Limits::default(), None).unwrap();
    // Off: both results stay (one overlaps the unselected 62).
    assert_eq!(run0.notes.len(), 2);
    // Remove Overlaps against the notes already there.
    let mut run = run0.clone();
    let r = prevent_in_run(&d, &src, &mut run, general(Mode::RemoveOverlaps, true));
    assert_eq!((r.removed, run.notes.len()), (1, 1));
    assert_eq!(run.stats.overlaps_removed, 1);
    assert!(
        run.summary().contains("Overlaps: 1 removed"),
        "{}",
        run.summary()
    );
    // Only among results: nothing to remove.
    let mut run = run0.clone();
    let r = prevent_in_run(&d, &src, &mut run, general(Mode::RemoveOverlaps, false));
    assert_eq!((r.removed, run.notes.len()), (0, 2));
    // Trim: the earlier note is the unselected 62? No — the result starts
    // first (0 < 240) and is shortened to end at 240; the 62 stays whole.
    let mut run = run0.clone();
    let r = prevent_in_run(&d, &src, &mut run, general(Mode::TrimOverlaps, true));
    assert_eq!((r.removed, r.trimmed), (0, 1));
    let mut d2 = d.clone();
    let op = plumspirit_engine::transform::run::build_operation(&mut d2, &run, "Scale").unwrap();
    let before = d2.fingerprint();
    let mut h = History::default();
    h.apply(&mut d2, op).unwrap();
    assert_eq!(
        notes(&d2),
        vec![(0, 240, 62), (0, 480, 60), (0, 480, 64), (240, 720, 62)]
    );
    // One undo step.
    h.undo(&mut d2).unwrap();
    assert_eq!(d2.fingerprint(), before);
}

#[test]
fn paste_onto_notes_already_there_removes_only_pasted_notes() {
    // Copy 60 and 62 (0..100) elsewhere; paste them at tick 0 into a track
    // holding 60 at 50..150 (overlapping) and 62 at 500..600 (apart).
    let src = doc_with(&[rec(0, 100, 60), rec(0, 100, 62)], &[]);
    let clip = Clipboard::copy_selected(editor::note_view(&src, 0).unwrap())
        .unwrap()
        .unwrap();
    let mut d = doc_with(&[], &[rec(50, 100, 60), rec(500, 100, 62)]);
    let existing = notes(&d);
    let mut e = clip.prepare_paste(&d, 0, 0).unwrap();
    let r = prevent_in_edit(&d, &mut e, general(Mode::RemoveOverlaps, true));
    // The pasted 60 meets the existing 60: the pasted one goes (existing
    // notes are never changed); the pasted 62 stays.
    assert_eq!((r.removed, e.affected_notes()), (1, 1));
    let before = d.fingerprint();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: e,
            label: "Paste",
        },
    )
    .unwrap();
    let after = notes(&d);
    assert_eq!(after.len(), 3);
    for n in &existing {
        assert!(after.contains(n), "existing note {n:?} kept");
    }
    assert!(after.contains(&(0, 100, 62)));
    h.undo(&mut d).unwrap();
    assert_eq!(d.fingerprint(), before);
    // Exact duplicates only: 0..100 is not an exact copy of 50..150.
    let mut e = clip.prepare_paste(&d, 0, 0).unwrap();
    let r = prevent_in_edit(&d, &mut e, general(Mode::ExactDuplicates, true));
    assert_eq!((r.removed, e.affected_notes()), (0, 2));
    // Pasted at 50: the 60 lands exactly on the existing 60.
    let mut e = clip.prepare_paste(&d, 0, 50).unwrap();
    let r = prevent_in_edit(&d, &mut e, general(Mode::ExactDuplicates, true));
    assert_eq!(r.removed, 1);
    // Trim: the pasted 60 (0..100, earlier) ends where the existing one
    // starts.
    let mut e = clip.prepare_paste(&d, 0, 0).unwrap();
    let r = prevent_in_edit(&d, &mut e, general(Mode::TrimOverlaps, true));
    assert_eq!((r.removed, r.trimmed), (0, 1));
    match &e {
        PreparedEdit::InsertSelected { records, .. } => {
            let t = records.iter().find(|r| r.attrs.pitch() == 60).unwrap();
            assert_eq!((t.start_tick, t.duration_ticks), (0, 50));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn layers_count_apart_unless_across() {
    // Copy a subchannel-0 note; the target track holds the same key at the
    // same time on subchannel 1 only.
    let src = doc_with(&[rec(0, 100, 60)], &[]);
    let clip = Clipboard::copy_selected(editor::note_view(&src, 0).unwrap())
        .unwrap()
        .unwrap();
    let mut other = rec(0, 100, 60);
    other.sub = SubchannelId(1);
    let d = doc_with(&[], &[other]);
    let mut e = clip.prepare_paste(&d, 0, 0).unwrap();
    assert_eq!(
        prevent_in_edit(&d, &mut e, general(Mode::RemoveOverlaps, true)).removed,
        0,
        "subchannel 1 is another layer"
    );
    let mut g = general(Mode::RemoveOverlaps, true);
    g.policy.across = true;
    let mut e = clip.prepare_paste(&d, 0, 0).unwrap();
    assert_eq!(prevent_in_edit(&d, &mut e, g).removed, 1, "across layers");
    // Away from it in time: nothing.
    let mut e = clip.prepare_paste(&d, 0, 100).unwrap();
    assert_eq!(
        prevent_in_edit(&d, &mut e, g).removed,
        0,
        "end = start is no overlap"
    );
}

#[test]
fn an_in_place_pitch_remap_becomes_a_replace_only_when_something_goes() {
    // Flip Y of 60 (0..100) / 64 (500..600), selected → 64 / 60, with an
    // unselected 64 at 50..150.
    let d = doc_with(&[rec(0, 100, 60), rec(500, 100, 64)], &[rec(50, 100, 64)]);
    let mut e = editor::prepare_flip_y(&d, 0).unwrap();
    assert!(matches!(e, PreparedEdit::RemapSelectedPitch { .. }));
    // Only among results: nothing goes, the remap stays a remap.
    let r = prevent_in_edit(&d, &mut e, general(Mode::RemoveOverlaps, false));
    assert_eq!(r.removed, 0);
    assert!(matches!(e, PreparedEdit::RemapSelectedPitch { .. }));
    // Against the unselected 64: the flipped 60 → 64 (0..100) meets it.
    let r = prevent_in_edit(&d, &mut e, general(Mode::RemoveOverlaps, true));
    assert_eq!(r.removed, 1);
    match &e {
        PreparedEdit::ReplaceSelected { records, .. } => {
            assert_eq!(records.len(), 1);
            assert_eq!((records[0].start_tick, records[0].attrs.pitch()), (500, 60));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_drawn_note_is_refused_or_shortened() {
    let d = doc_with(&[], &[rec(100, 100, 60), rec(0, 50, 61)]);
    let store = &d.track(0).unwrap().notes;
    // On top of 60 (100..200): not placed.
    assert_eq!(
        prevent_note(
            store,
            &rec(150, 100, 60),
            general(Mode::RemoveOverlaps, true)
        ),
        None
    );
    // Before it, reaching into it: Remove refuses, Trim shortens to 100.
    assert_eq!(
        prevent_note(store, &rec(0, 150, 60), general(Mode::RemoveOverlaps, true)),
        None
    );
    let t = prevent_note(store, &rec(0, 150, 60), general(Mode::TrimOverlaps, true)).unwrap();
    assert_eq!((t.start_tick, t.duration_ticks), (0, 100));
    // Elsewhere: placed as drawn.
    let free = rec(300, 100, 60);
    assert_eq!(
        prevent_note(store, &free, general(Mode::RemoveOverlaps, true)),
        Some(free)
    );
    // Only among results: a single note always stays.
    assert_eq!(
        prevent_note(
            store,
            &rec(150, 100, 60),
            general(Mode::RemoveOverlaps, false)
        ),
        Some(rec(150, 100, 60))
    );
}

/// B1-S39 measurement (release): 1,000,000 pasted notes onto 1,000,000
/// notes already there (half of them overlapping on the same key).
#[test]
#[ignore = "measurement: cargo test --release -p plumspirit_engine --test b1_general_overlap -- --ignored --nocapture"]
fn a_million_pasted_notes_against_a_million_existing() {
    let mut existing = Vec::with_capacity(1_000_000);
    let mut pasted = Vec::with_capacity(1_000_000);
    for i in 0..1_000_000u64 {
        let p = 24 + (i % 80) as u8;
        let t = (i / 80) * 240;
        existing.push(rec(t, 120, p));
        // Every second pasted note starts inside an existing one.
        pasted.push(rec(t + if i % 2 == 0 { 60 } else { 120 }, 100, p));
    }
    let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 960);
    existing.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&existing, false)
        .unwrap();
    pasted.sort_by_key(|r| r.start_tick);
    let mut e = PreparedEdit::InsertSelected {
        host: plumspirit_engine::model::ContentRef::Track(d.track(0).unwrap().id()),
        base_revision: d.track(0).unwrap().notes.revision(),
        records: pasted,
        subs_track: d.track(0).unwrap().id(),
        new_subs: Vec::new(),
    };
    let t = std::time::Instant::now();
    let r = prevent_in_edit(&d, &mut e, General::default());
    let ms = t.elapsed().as_millis();
    println!(
        "B1-S39: 1,000,000 pasted vs 1,000,000 existing: {} removed, {} kept, {} ms",
        r.removed,
        e.affected_notes(),
        ms
    );
    assert_eq!(r.removed, 500_000);
}
