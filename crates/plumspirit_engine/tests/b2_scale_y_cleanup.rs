//! v1.0-beta2 B2-F28 (contract §022): Scale Y "Clean up Scale Y overlaps" — a
//! destructive cleanup of the selection's results by a labelled mode (exact
//! duplicates by default; trim or remove for broader overlaps), the same in
//! Preview and Apply, one undo step, other keys / layers / unselected notes
//! untouched, never a zero-length result.

use plumspirit_engine::editor::{self, ExactFactor, ScaleY, ScaleYKernel, scale_y_run_kernel};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, NoteStore, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::geom::{ScaleYCleanup, clean_scale_y};
use plumspirit_engine::transform::{
    Boundary, Limits, Source, TNote, TrackScope, TransformRun, run_kernel,
};

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new_wide(pitch, vel, ch, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

/// Track 0 holds `selected` (selected) and `other` (not selected).
fn doc(keys: KeyRange, selected: &[NoteRecord], other: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b2s37".into(), Bpm::DEFAULT, 960);
    d.project_mut().keys = keys;
    let mut v: Vec<(NoteRecord, bool)> = selected
        .iter()
        .map(|r| (*r, true))
        .chain(other.iter().map(|r| (*r, false)))
        .collect();
    v.sort_by_key(|(r, _)| r.start_tick);
    let records: Vec<NoteRecord> = v.iter().map(|(r, _)| *r).collect();
    let mut s = NoteStore::from_sorted_records(&records, false).unwrap();
    let mut locs = Vec::new();
    s.query(0, u64::MAX, |loc, n| {
        locs.push((loc, n.start_tick, n.attrs.pitch(), n.attrs.velocity()))
    });
    for (loc, start, pitch, vel) in locs {
        let sel = v.iter().any(|(r, on)| {
            *on && (r.start_tick, r.attrs.pitch(), r.attrs.velocity()) == (start, pitch, vel)
        });
        s.set_selected(loc, sel);
    }
    d.track_mut(0).unwrap().notes = s;
    d
}

/// (start, length, pitch, velocity, channel, subchannel, selected), sorted.
fn notes(d: &PlumDocument) -> Vec<(u64, u64, u8, u8, u8, u32, bool)> {
    let mut v = Vec::new();
    d.track(0).unwrap().notes.query(0, u64::MAX, |_, n| {
        v.push((
            n.start_tick,
            n.duration_ticks,
            n.attrs.pitch(),
            n.attrs.velocity(),
            n.attrs.channel(),
            n.sub.0,
            n.selected,
        ))
    });
    v.sort_unstable();
    v
}

fn opts(cleanup: ScaleYCleanup) -> ScaleY {
    ScaleY {
        fill: None,
        prevent_overlap: true,
        cleanup,
    }
}

fn shrink(d: &PlumDocument, factor: &str, cleanup: ScaleYCleanup) -> PlumDocument {
    let f = ExactFactor::parse(factor).unwrap();
    let e = editor::prepare_scale_y_with(d, 0, &f, opts(cleanup), &Limits::default()).unwrap();
    let mut out = d.clone();
    editor::commit(&mut out, e).unwrap();
    out
}

fn t(start: u64, dur: u64, pitch: u8, vel: u8) -> TNote {
    TNote {
        start,
        dur,
        pitch,
        vel,
        ..TNote::default()
    }
}

#[test]
fn other_layers_and_keys_are_never_cleanup_partners() {
    // Same start, length and key — but another channel or subchannel, or
    // another key: every mode keeps them all.
    let base = vec![
        TNote {
            ch: 0,
            ..t(0, 100, 60, 90)
        },
        TNote {
            ch: 1,
            ..t(0, 100, 60, 80)
        },
        TNote {
            sub: 2,
            ..t(0, 100, 60, 70)
        },
        TNote {
            track: 1,
            ..t(0, 100, 60, 60)
        },
        t(0, 100, 61, 50),
    ];
    for mode in ScaleYCleanup::ALL {
        let mut v = base.clone();
        let r = clean_scale_y(&mut v, mode);
        assert_eq!((v.len(), r.removed, r.trimmed), (5, 0, 0), "{mode:?}");
    }
}

#[test]
fn exact_duplicates_keep_the_first_in_source_order_and_partial_overlaps() {
    let mut v = vec![
        t(0, 100, 60, 90),
        t(0, 100, 60, 40),  // duplicate of the first (velocity may differ)
        t(0, 50, 60, 30),   // same start, other length: stays
        t(20, 100, 60, 20), // partial overlap: stays
    ];
    let r = clean_scale_y(&mut v, ScaleYCleanup::ExactDuplicates);
    assert_eq!((r.removed, r.trimmed), (1, 0));
    let mut got: Vec<(u64, u64, u8)> = v.iter().map(|n| (n.start, n.dur, n.vel)).collect();
    got.sort_unstable();
    assert_eq!(got, vec![(0, 50, 30), (0, 100, 90), (20, 100, 20)]);
    // Source order decides: the same pair the other way round keeps 40.
    let mut w = vec![t(0, 100, 60, 40), t(0, 100, 60, 90)];
    clean_scale_y(&mut w, ScaleYCleanup::ExactDuplicates);
    assert_eq!(w.iter().map(|n| n.vel).collect::<Vec<_>>(), vec![40]);
}

#[test]
fn zero_length_notes_are_never_made_and_go_only_as_partners() {
    // A lone zero-length note stays in every mode.
    for mode in ScaleYCleanup::ALL {
        let mut v = vec![t(0, 0, 60, 90), t(500, 10, 60, 90)];
        clean_scale_y(&mut v, mode);
        assert_eq!(v.len(), 2, "{mode:?}");
    }
    // Inside another note: Exact keeps it, Remove removes it, Trim cuts the
    // longer note at its attack (never to zero: starts differ).
    let base = vec![t(0, 100, 60, 90), t(50, 0, 60, 80)];
    let mut v = base.clone();
    clean_scale_y(&mut v, ScaleYCleanup::ExactDuplicates);
    assert_eq!(v.len(), 2);
    let mut v = base.clone();
    let r = clean_scale_y(&mut v, ScaleYCleanup::RemoveOverlapping);
    assert_eq!((v.len(), r.removed), (1, 1));
    let mut v = base.clone();
    let r = clean_scale_y(&mut v, ScaleYCleanup::TrimEarlier);
    let mut got: Vec<(u64, u64)> = v.iter().map(|n| (n.start, n.dur)).collect();
    got.sort_unstable();
    assert_eq!((got, r.trimmed), (vec![(0, 50), (50, 0)], 1));
    // Trim on a dense stack: equal starts keep the longest, cuts are >= 1.
    let mut v: Vec<TNote> = (0..50u64).map(|i| t(i, 100 - i, 60, 90)).collect();
    v.push(t(0, 7, 60, 90));
    clean_scale_y(&mut v, ScaleYCleanup::TrimEarlier);
    assert!(v.iter().all(|n| n.dur >= 1));
    assert_eq!(v.len(), 50);
}

#[test]
fn a_shrink_cleans_only_the_selection_and_one_undo_restores_it() {
    // Selected: 60 and 61 at the same time collapse onto one key on a ×0.5
    // shrink. Not selected: the same key and time, and a chord partner.
    let sel = [
        rec(0, 480, 60, 100, 0, 0),
        rec(0, 480, 61, 90, 0, 0),
        rec(0, 480, 64, 80, 0, 0),
    ];
    let other = [rec(0, 480, 61, 70, 0, 0), rec(0, 480, 67, 60, 0, 0)];
    let d = doc(KeyRange::Standard, &sel, &other);
    let before = notes(&d);
    let f = ExactFactor::parse("0.5").unwrap();
    let e = editor::prepare_scale_y_with(
        &d,
        0,
        &f,
        opts(ScaleYCleanup::ExactDuplicates),
        &Limits::default(),
    )
    .unwrap();
    let mut d2 = d.clone();
    let fp = d.fingerprint();
    let mut h = History::default();
    h.apply(
        &mut d2,
        Operation::Prepared {
            edit: e,
            label: "Scale Y",
        },
    )
    .unwrap();
    let got = notes(&d2);
    // 60 / 61 → one note (the first, velocity 100); 64 moves; the unselected
    // 61 and 67 are untouched.
    let selected: Vec<_> = got.iter().filter(|n| n.6).map(|n| (n.2, n.3)).collect();
    assert_eq!(selected.len(), 2, "{got:?}");
    assert!(selected.iter().any(|&(_, v)| v == 100));
    assert!(!selected.iter().any(|&(_, v)| v == 90));
    for n in before.iter().filter(|n| !n.6) {
        assert!(got.contains(n), "unselected {n:?} kept: {got:?}");
    }
    // One undo step restores every original; redo replays it.
    let after = d2.fingerprint();
    h.undo(&mut d2).unwrap();
    assert_eq!(d2.fingerprint(), fp);
    assert_eq!(notes(&d2), before);
    h.redo(&mut d2).unwrap();
    assert_eq!(d2.fingerprint(), after);
}

/// The kernel the Preview runs on the selection.
fn preview(d: &PlumDocument, factor: &str, o: ScaleY) -> TransformRun {
    let f = ExactFactor::parse(factor).unwrap();
    let view = editor::note_view(d, 0).unwrap();
    let k: ScaleYKernel = scale_y_run_kernel(view, &f, o, d.project().keys).unwrap();
    let src = Source::collect(d, &[TrackScope::of(0usize.into(), false)], 10_000_000).unwrap();
    run_kernel(
        &k,
        &src,
        Boundary::default().within(d.project().keys),
        &Limits::default(),
        None,
    )
    .unwrap()
}

#[test]
fn the_preview_shows_exactly_what_apply_commits() {
    // A staggered column with a stack and a chord, shrunk and filled.
    let sel: Vec<NoteRecord> = (0..8u8)
        .map(|i| rec(u64::from(i) * 10, 200, 60 + i, 100 - i, 0, 0))
        .chain([rec(0, 200, 60, 50, 0, 0), rec(0, 200, 72, 40, 0, 0)])
        .collect();
    let d = doc(KeyRange::Standard, &sel, &[]);
    for mode in ScaleYCleanup::ALL {
        for (factor, fill) in [
            ("0.5", None),
            ("0.25", None),
            ("2", Some(Default::default())),
        ] {
            let o = ScaleY {
                fill,
                prevent_overlap: true,
                cleanup: mode,
            };
            let run = preview(&d, factor, o);
            let mut want: Vec<(u64, u64, u8, u8)> = run
                .notes
                .iter()
                .map(|n| (n.start, n.dur, n.pitch, n.vel))
                .collect();
            want.sort_unstable();
            let f = ExactFactor::parse(factor).unwrap();
            let e = editor::prepare_scale_y_with(&d, 0, &f, o, &Limits::default()).unwrap();
            let mut d2 = d.clone();
            editor::commit(&mut d2, e).unwrap();
            let got: Vec<(u64, u64, u8, u8)> =
                notes(&d2).iter().map(|n| (n.0, n.1, n.2, n.3)).collect();
            assert_eq!(got, want, "{mode:?} ×{factor} fill {}", fill.is_some());
            // The summary names what the cleanup did.
            let s = run.summary();
            let st = run.stats;
            if st.overlaps_removed + st.overlaps_trimmed > 0 {
                assert!(s.contains("Overlaps:"), "{s}");
            }
        }
    }
}

#[test]
fn the_cleanup_works_on_the_keys_of_a_256_key_project() {
    // The standard-key case moved up 140 keys: 200 / 201 meet on 201 under
    // ×0.5 (pivot 202), 204 → 203.
    let sel = [
        rec(0, 100, 200, 100, 0, 0),
        rec(0, 100, 201, 90, 0, 0),
        rec(0, 100, 204, 80, 0, 0),
    ];
    let d = doc(KeyRange::Extended256, &sel, &[]);
    let d2 = shrink(&d, "0.5", ScaleYCleanup::ExactDuplicates);
    let got: Vec<(u8, u8)> = notes(&d2).iter().map(|n| (n.2, n.3)).collect();
    assert_eq!(got, vec![(201, 100), (203, 80)]);
}

#[test]
fn a_large_stack_on_one_key_cleans_in_one_pass() {
    // 200,000 identical selected notes (debug build): one stays.
    let mut v: Vec<TNote> = (0..200_000u32).map(|_| t(0, 100, 60, 90)).collect();
    let started = std::time::Instant::now();
    let r = clean_scale_y(&mut v, ScaleYCleanup::ExactDuplicates);
    assert_eq!((v.len(), r.removed), (1, 199_999));
    let mut w: Vec<TNote> = (0..200_000u64).map(|i| t(i, 1_000, 60, 90)).collect();
    let r = clean_scale_y(&mut w, ScaleYCleanup::TrimEarlier);
    assert_eq!((w.len(), r.trimmed), (200_000, 199_999));
    let mut x: Vec<TNote> = (0..200_000u64).map(|i| t(i, 1_000, 60, 90)).collect();
    let r = clean_scale_y(&mut x, ScaleYCleanup::RemoveOverlapping);
    assert_eq!(x.len() as u64 + r.removed, 200_000);
    assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
}

/// Resource cost at a few million notes (release; one run).
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn scale_y_cleanup_cost() {
    for n in [1_000_000u64, 4_000_000] {
        for mode in ScaleYCleanup::ALL {
            // Black MIDI art: 128 rows of staggered long notes, every row a
            // stack of 4 (the worst case for every mode).
            let mut v: Vec<TNote> = (0..n)
                .map(|i| t((i / 512) * 10, 400, (i % 128) as u8, 90))
                .collect();
            let started = std::time::Instant::now();
            let r = clean_scale_y(&mut v, mode);
            println!(
                "{n} notes {mode:?}: {:.0} ms, {} kept, {} removed, {} trimmed",
                started.elapsed().as_secs_f64() * 1e3,
                v.len(),
                r.removed,
                r.trimmed
            );
        }
    }
}
