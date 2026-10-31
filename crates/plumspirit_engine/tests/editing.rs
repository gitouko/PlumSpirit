//! Transforms (§51–57, §120), copy/paste (§121), tracks (§122), repeated operations
//! (§126), data integrity after failed transforms (§136) and property tests (§200).

mod common;

use common::*;
use plumspirit_engine::editor::{
    self, Clipboard, EditControl, EditError, ExactFactor, PreparedEdit, commit, prepare_flip_x,
    prepare_scale_x,
};
use plumspirit_engine::model::{
    Division, DocumentError, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument,
};

fn doc_with(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_empty(Division::ppqn(96));
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&sorted, true)
        .unwrap();
    d
}

fn f(s: &str) -> ExactFactor {
    ExactFactor::parse(s).unwrap()
}

fn spans(d: &PlumDocument) -> Vec<(u64, u64, u8)> {
    let mut v: Vec<_> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start_tick, n.end_tick(), n.attrs.pitch()))
        .collect();
    v.sort_unstable();
    v
}

// ----------------------------------------------------------------- Flip X

#[test]
fn flip_x_single_multiple_overlapping_zero() {
    let mut d = doc_with(&[rec(10, 5, 60)]);
    editor::flip_x(&mut d, 0).unwrap();
    assert_eq!(spans(&d), vec![(10, 15, 60)]); // one note mirrors onto itself

    let mut d = doc_with(&[
        rec(0, 10, 60),
        rec(5, 10, 61),
        rec(30, 0, 62),
        rec(40, 20, 63),
    ]);
    editor::flip_x(&mut d, 0).unwrap();
    // Bounds [0, 60]: new_start = 60 - old_end
    assert_eq!(
        spans(&d),
        vec![(0, 20, 63), (30, 30, 62), (45, 55, 61), (50, 60, 60)]
    );
    assert_eq!(
        d.track(0).unwrap().notes.selected_count(),
        4,
        "selection kept"
    );
    d.track(0).unwrap().notes.validate().unwrap();
}

#[test]
fn flip_x_huge_ticks() {
    let base = u64::MAX - 1_000;
    let mut d = doc_with(&[rec(base, 10, 60), rec(base + 500, 400, 61)]);
    editor::flip_x(&mut d, 0).unwrap();
    assert_eq!(
        spans(&d),
        vec![(base, base + 400, 61), (base + 890, base + 900, 60)]
    );
}

#[test]
fn flip_twice_restores() {
    let mut rng = Rng(5);
    for _ in 0..40 {
        let n = 1 + rng.below(200) as usize;
        let notes: Vec<NoteRecord> = (0..n)
            .map(|_| NoteRecord {
                start_tick: rng.below(10_000),
                duration_ticks: rng.below(500),
                attrs: NoteAttrs::new(
                    rng.below(128) as u8,
                    1 + rng.below(127) as u8,
                    rng.below(16) as u8,
                    rng.below(128) as u8,
                    NoteOffKind::NoteOff,
                )
                .unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        let mut d = doc_with(&notes);
        let before_canon = canonical_notes(&d, 0);
        let before_fp = d.fingerprint();
        editor::flip_x(&mut d, 0).unwrap();
        editor::flip_x(&mut d, 0).unwrap();
        assert_eq!(canonical_notes(&d, 0), before_canon, "Flip X twice");
        editor::flip_y(&mut d, 0).unwrap();
        editor::flip_y(&mut d, 0).unwrap();
        assert_eq!(canonical_notes(&d, 0), before_canon, "Flip Y twice");
        // Flip Y is in place: exact fingerprint identity when applied twice.
        let mut d2 = doc_with(&notes);
        editor::flip_y(&mut d2, 0).unwrap();
        editor::flip_y(&mut d2, 0).unwrap();
        assert_eq!(d2.fingerprint(), before_fp);
        d.track(0).unwrap().notes.validate().unwrap();
    }
}

// ----------------------------------------------------------------- Flip Y

#[test]
fn flip_y_ranges() {
    let mut d = doc_with(&[rec(0, 1, 64), rec(1, 1, 64)]);
    editor::flip_y(&mut d, 0).unwrap();
    assert_eq!(spans(&d), vec![(0, 1, 64), (1, 2, 64)]); // one pitch: unchanged
    let mut d = doc_with(&[rec(0, 1, 0), rec(1, 1, 127), rec(2, 1, 100)]);
    editor::flip_y(&mut d, 0).unwrap();
    assert_eq!(spans(&d), vec![(0, 1, 127), (1, 2, 0), (2, 3, 27)]);
}

// ----------------------------------------------------------------- Scale X

#[test]
fn scale_x_cases() {
    let notes = [rec(100, 10, 60), rec(120, 20, 61)];
    // factor 1: exact identity (document fingerprint unchanged)
    let mut d = doc_with(&notes);
    let fp = d.fingerprint();
    editor::scale_x(&mut d, 0, &f("1")).unwrap();
    assert_eq!(d.fingerprint(), fp);
    // factor 2 around pivot 100
    let mut d = doc_with(&notes);
    editor::scale_x(&mut d, 0, &f("2")).unwrap();
    assert_eq!(spans(&d), vec![(100, 120, 60), (140, 180, 61)]);
    // factor 0.5 (130 -> 115? no: 120->110, 140->120; 110 -> 105)
    let mut d = doc_with(&notes);
    editor::scale_x(&mut d, 0, &f("0.5")).unwrap();
    assert_eq!(spans(&d), vec![(100, 105, 60), (110, 120, 61)]);
    // negative factor: mirrored around the pivot, endpoints normalized
    let mut d = doc_with(&notes);
    editor::scale_x(&mut d, 0, &f("-1")).unwrap();
    assert_eq!(spans(&d), vec![(60, 80, 61), (90, 100, 60)]);
    // rounding: 0.3 * 5 = 1.5 exactly -> 2 (away from pivot)
    let mut d = doc_with(&[rec(0, 5, 60)]);
    editor::scale_x(&mut d, 0, &f("0.3")).unwrap();
    assert_eq!(spans(&d), vec![(0, 2, 60)]);
}

#[test]
fn scale_x_rejections_leave_document_untouched() {
    let notes = [rec(100, 10, 60), rec(120, 20, 61)];
    let mut d = doc_with(&notes);
    let fp = d.fingerprint();
    // Negative ticks: -3 around pivot 100 sends tick 140 to -20.
    let e = editor::scale_x(&mut d, 0, &f("-3")).unwrap_err();
    assert!(matches!(e, EditError::NegativeTick { .. }), "{e:?}");
    assert_eq!(d.fingerprint(), fp);
    // Overflow: huge factor
    let e = editor::scale_x(&mut d, 0, &f("1e30")).unwrap_err();
    assert!(matches!(e, EditError::TickOverflow { .. }), "{e:?}");
    assert_eq!(d.fingerprint(), fp);
    // Overflow near u64::MAX
    let mut d2 = doc_with(&[rec(u64::MAX / 2, 10, 60), rec(u64::MAX / 2 + 100, 10, 60)]);
    let fp2 = d2.fingerprint();
    assert!(editor::scale_x(&mut d2, 0, &f("1e18")).is_err());
    assert_eq!(d2.fingerprint(), fp2);
    // Gaps beyond one MIDI delta time are no longer refused (v0.3): the 20-tick
    // note scaled by 1e8 needs a 2e9-tick Note On -> Note Off delta, which
    // export bridges with filler events (see midi_roundtrip.rs).
    let mut wide = d.clone();
    editor::scale_x(&mut wide, 0, &f("1e8")).unwrap();
    assert_eq!(
        spans(&wide),
        vec![(100, 1_000_000_100, 60), (2_000_000_100, 4_000_000_100, 61)]
    );
    // Unlimited magnitude is still accepted when representable.
    editor::scale_x(&mut d, 0, &f("10000")).unwrap();
    assert_eq!(spans(&d), vec![(100, 100_100, 60), (200_100, 400_100, 61)]);
}

// ----------------------------------------------------------------- Scale Y

#[test]
fn scale_y_cases() {
    let notes = [rec(0, 1, 60), rec(1, 1, 64), rec(2, 1, 62)];
    let mut d = doc_with(&notes);
    let fp = d.fingerprint();
    editor::scale_y(&mut d, 0, &f("1")).unwrap();
    assert_eq!(d.fingerprint(), fp);
    // factor 2 around pivot 62: 60 -> 58, 64 -> 66, 62 -> 62
    editor::scale_y(&mut d, 0, &f("2")).unwrap();
    assert_eq!(
        spans(&d).iter().map(|s| s.2).collect::<Vec<_>>(),
        vec![58, 66, 62]
    );
    // negative factor: -1 equals flip
    let mut d = doc_with(&notes);
    editor::scale_y(&mut d, 0, &f("-1")).unwrap();
    assert_eq!(
        spans(&d).iter().map(|s| s.2).collect::<Vec<_>>(),
        vec![64, 60, 62]
    );
    // deterministic rounding at half-semitone pivot: 60,61 pivot 60.5, factor 2 -> 59, 62
    let mut d = doc_with(&[rec(0, 1, 60), rec(1, 1, 61)]);
    editor::scale_y(&mut d, 0, &f("2")).unwrap();
    assert_eq!(
        spans(&d).iter().map(|s| s.2).collect::<Vec<_>>(),
        vec![59, 62]
    );
    // out of range: rejected, never clamped or wrapped
    let mut d = doc_with(&[rec(0, 1, 0), rec(1, 1, 127)]);
    let fp = d.fingerprint();
    let e = editor::scale_y(&mut d, 0, &f("1.01")).unwrap_err();
    assert!(
        matches!(
            e,
            EditError::PitchOutOfRange {
                lowest: -1,
                highest: 128,
                ..
            }
        ),
        "{e:?}"
    );
    assert_eq!(d.fingerprint(), fp);
    let e = editor::scale_y(&mut d, 0, &f("1e50")).unwrap_err();
    assert!(matches!(e, EditError::PitchOutOfRange { .. }), "{e:?}");
    assert_eq!(d.fingerprint(), fp);
}

#[test]
fn transforms_preserve_channel_velocity_offdata_and_other_tracks() {
    let a = NoteAttrs::new(60, 33, 7, 44, NoteOffKind::NoteOnZero).unwrap();
    let mut d = doc_with(&[
        NoteRecord {
            start_tick: 10,
            duration_ticks: 10,
            attrs: a,
            sub: Default::default(),
            group: 0,
        },
        rec(30, 5, 62),
    ]);
    let other = d.add_track().unwrap();
    editor::insert_note(&mut d, other, rec(0, 1, 1)).unwrap();
    let other_fp: Vec<_> = notes_of(&d, other);
    d.track_mut(0).unwrap().notes.select_all();
    for op in 0..4 {
        match op {
            0 => editor::flip_x(&mut d, 0).unwrap(),
            1 => editor::flip_y(&mut d, 0).unwrap(),
            2 => editor::scale_x(&mut d, 0, &f("3")).unwrap(),
            _ => editor::scale_y(&mut d, 0, &f("-1")).unwrap(),
        }
        let n = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .find(|n| n.attrs.velocity() == 33)
            .unwrap();
        assert_eq!(n.attrs.channel(), 7);
        assert_eq!(n.attrs.off_kind(), NoteOffKind::NoteOnZero);
        assert_eq!(notes_of(&d, other), other_fp);
    }
}

#[test]
fn nothing_selected_is_an_error_not_a_change() {
    let mut d = doc_with(&[rec(0, 1, 60)]);
    d.track_mut(0).unwrap().notes.clear_selection();
    let fp = d.fingerprint();
    assert_eq!(editor::flip_x(&mut d, 0), Err(EditError::NothingSelected));
    assert_eq!(
        editor::scale_y(&mut d, 0, &f("2")),
        Err(EditError::NothingSelected)
    );
    assert_eq!(d.fingerprint(), fp);
}

#[test]
fn stale_prepared_edit_is_refused() {
    let mut d = doc_with(&[rec(0, 10, 60), rec(20, 10, 61)]);
    let prepared = prepare_flip_x(&d, 0, EditControl::default()).unwrap();
    // The document changes before commit (e.g. the user drew a note meanwhile).
    editor::insert_note(&mut d, 0, rec(50, 1, 70)).unwrap();
    let fp = d.fingerprint();
    assert_eq!(commit(&mut d, prepared), Err(EditError::Stale));
    assert_eq!(d.fingerprint(), fp);
    // A prepared edit for a deleted track is refused too.
    let t1 = d.add_track().unwrap();
    editor::insert_note(&mut d, t1, rec(0, 1, 60)).unwrap();
    let p = prepare_scale_x(&d, t1, &f("2"), EditControl::default()).unwrap();
    d.delete_track(t1).unwrap();
    assert_eq!(commit(&mut d, p), Err(EditError::Stale));
}

// --------------------------------------------------------------- move / resize

#[test]
fn move_and_resize() {
    let mut d = doc_with(&[rec(10, 10, 60), rec(15, 5, 70)]);
    editor::move_selected(&mut d, 0, 5, -2).unwrap();
    assert_eq!(spans(&d), vec![(15, 25, 58), (20, 25, 68)]);
    // Pitch overflow rejected (not wrapped)
    let fp = d.fingerprint();
    assert!(matches!(
        editor::move_selected(&mut d, 0, 0, 60),
        Err(EditError::PitchOutOfRange { .. })
    ));
    assert!(matches!(
        editor::move_selected(&mut d, 0, -100, 0),
        Err(EditError::NegativeTick { .. })
    ));
    assert_eq!(d.fingerprint(), fp);
    // Resize: min duration 5 -> delta >= -4
    assert_eq!(
        editor::resize_selected(&mut d, 0, -5),
        Err(EditError::DurationTooShort)
    );
    editor::resize_selected(&mut d, 0, -4).unwrap();
    assert_eq!(spans(&d), vec![(15, 21, 58), (20, 21, 68)]);
    editor::resize_selected(&mut d, 0, 100).unwrap();
    assert_eq!(spans(&d), vec![(15, 121, 58), (20, 121, 68)]);
    d.track(0).unwrap().notes.validate().unwrap();
}

// ------------------------------------------------------------------ clipboard

#[test]
fn copy_paste_behaviour() {
    let a = NoteAttrs::new(60, 45, 9, 12, NoteOffKind::NoteOff).unwrap();
    let mut d = doc_with(&[
        NoteRecord {
            start_tick: 100,
            duration_ticks: 30,
            attrs: a,
            sub: Default::default(),
            group: 0,
        },
        rec(160, 10, 64),
        rec(160, 5, 67),
    ]);
    let clip = Clipboard::copy_selected(d.track(0).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(clip.len(), 3);
    // Paste into another track, three times; each paste becomes the selection.
    let t1 = d.add_track().unwrap();
    for (i, at) in [0u64, 1000, 2000].iter().enumerate() {
        let e = clip.prepare_paste(&d, t1, *at).unwrap();
        commit(&mut d, e).unwrap();
        let t = d.track(t1).unwrap();
        assert_eq!(t.notes.len(), 3 * (i + 1));
        assert_eq!(t.notes.selected_count(), 3);
        let mut sel = Vec::new();
        t.notes
            .for_each_selected(|v| sel.push((v.start_tick, v.duration_ticks, v.attrs.pitch())));
        assert_eq!(
            sel,
            vec![(*at, 30, 60), (at + 60, 10, 64), (at + 60, 5, 67)]
        );
        t.notes.validate().unwrap();
    }
    let pasted = d.track(t1).unwrap().notes.iter().next().unwrap();
    assert_eq!(
        (
            pasted.attrs.channel(),
            pasted.attrs.velocity(),
            pasted.attrs.off_velocity()
        ),
        (9, 45, 12)
    );
    // Clipboard survives deleting the source notes and the source track.
    editor::delete_selected(&mut d, 0).unwrap();
    d.delete_track(0).unwrap();
    let e = clip.prepare_paste(&d, 0, 5).unwrap();
    commit(&mut d, e).unwrap();
    assert_eq!(d.track(0).unwrap().notes.len(), 12);
    // Pasting past the end of the tick range is refused.
    assert!(matches!(
        clip.prepare_paste(&d, 0, u64::MAX - 10),
        Err(EditError::TickOverflow { .. })
    ));
}

#[test]
fn copy_duplicates_only_selected_notes() {
    let notes: Vec<NoteRecord> = (0..10_000u64).map(|i| rec(i, 1, (i % 128) as u8)).collect();
    let mut d = doc_with(&notes);
    d.track_mut(0).unwrap().notes.clear_selection();
    d.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(100, 199, 0, 127);
    let clip = Clipboard::copy_selected(d.track(0).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(clip.len(), d.track(0).unwrap().notes.selected_count());
    assert!(clip.len() <= 101);
}

// --------------------------------------------------------------------- tracks

#[test]
fn track_operations() {
    let mut d = doc_with(&[rec(0, 1, 60)]);
    for _ in 0..50 {
        d.add_track().unwrap();
    }
    assert_eq!(d.track_count(), 51);
    // Independent notes per track
    editor::insert_note(&mut d, 7, rec(5, 5, 70)).unwrap();
    assert_eq!(d.track(7).unwrap().notes.len(), 1);
    assert_eq!(d.track(8).unwrap().notes.len(), 0);
    assert_eq!(d.track(0).unwrap().notes.len(), 1);
    // Delete middle, delete active (last), repeated add/delete cycles
    assert_eq!(d.delete_track(25), Ok(25));
    assert_eq!(d.delete_track(49), Ok(48));
    for _ in 0..100 {
        let i = d.add_track().unwrap();
        d.delete_track(i).unwrap();
    }
    while d.track_count() > 1 {
        d.delete_track(0).unwrap();
    }
    assert_eq!(d.delete_track(0), Err(DocumentError::LastTrack));
}

#[test]
fn repeated_transform_same_selection() {
    let mut d = doc_with(&[rec(0, 10, 60), rec(10, 10, 62)]);
    for _ in 0..5 {
        editor::scale_x(&mut d, 0, &f("2")).unwrap();
    }
    assert_eq!(spans(&d), vec![(0, 320, 60), (320, 640, 62)]);
    for _ in 0..5 {
        editor::scale_x(&mut d, 0, &f("0.5")).unwrap();
    }
    assert_eq!(spans(&d), vec![(0, 10, 60), (10, 20, 62)]);
}

#[test]
fn prepared_edit_identity() {
    let d = doc_with(&[rec(0, 10, 60)]);
    let p = prepare_scale_x(&d, 0, &f("1.0"), EditControl::default()).unwrap();
    assert_eq!(p, PreparedEdit::Identity);
}
