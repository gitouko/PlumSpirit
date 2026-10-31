//! B1-S38 (contract §018, B1-0115 / B1-0116): Scale Y Duplicate Notes — the
//! three separated modes (Transform existing notes, Fill added rows, Copy by
//! pitch step) on the exact box scale shared by the handles and Scale Y, with
//! gate / velocity / subchannel inheritance, preflight and one undo step; and
//! the Scale Y-specific overlap cleanup — beta1 (B1-0116) removed only the
//! overlaps the scale created; beta2 (B2-F28, contract §022) cleans the
//! selection's results destructively by a labelled mode (exact duplicates by
//! default, trim or remove for broader overlaps); chords on other keys and
//! other layers always survive.

use plumspirit_engine::editor::{
    self, ExactFactor, SCALE_Y_PREVENT_DEFAULT, ScaleY, scale_y_fill_kernel,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::geom::{
    BoxScale, Fill, FillMode, GeomBox, NoteSpot, OverlapFilter, ScaleYCleanup,
};
use plumspirit_engine::transform::{
    Boundary, Growth, Kernel, Limits, Source, TNote, TrackScope, run_kernel,
};

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

fn doc_with(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b1s38".into(), Bpm::DEFAULT, 960);
    let mut v = notes.to_vec();
    v.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

const SEL: [TrackScope; 1] = [TrackScope {
    index: 0,
    all: false,
    content: None,
}];

fn sel_box(d: &PlumDocument) -> GeomBox {
    let b = d.track(0).unwrap().notes.selection_bounds().unwrap();
    GeomBox {
        t0: b.min_start,
        t1: b.max_end.max(b.min_start + 1),
        p0: b.min_pitch,
        p1: b.max_pitch,
    }
}

fn run(d: &PlumDocument, k: &BoxScale) -> plumspirit_engine::transform::TransformRun {
    let src = Source::collect(d, &SEL, 10_000_000).unwrap();
    run_kernel(k, &src, Boundary::default(), &Limits::default(), None).unwrap()
}

/// (start, end, pitch, velocity, subchannel), sorted.
fn spots(v: &[TNote]) -> Vec<(u64, u64, u8, u8, u32)> {
    let mut k: Vec<_> = v
        .iter()
        .map(|n| (n.start, n.end(), n.pitch, n.vel, n.sub))
        .collect();
    k.sort_unstable();
    k
}

/// Same-key overlaps: pairs of results on one (subchannel, channel, pitch)
/// whose intervals overlap.
fn same_key_overlaps(v: &[TNote]) -> usize {
    let mut n = 0;
    for (i, a) in v.iter().enumerate() {
        for b in &v[i + 1..] {
            if (a.sub, a.ch, a.pitch) == (b.sub, b.ch, b.pitch)
                && a.start < b.end()
                && b.start < a.end()
            {
                n += 1;
            }
        }
    }
    n
}

/// A 4-row column of art (60 – 63) at one time.
fn column() -> Vec<NoteRecord> {
    (60..=63).map(|p| rec(0, 480, p, 100, 0)).collect()
}

fn step_auto() -> Fill {
    Fill {
        mode: FillMode::Step { step: 1, copies: 0 },
        ..Fill::default()
    }
}

fn boxed(from: GeomBox, rows: u8, fill: Option<Fill>, prevent: bool) -> BoxScale {
    BoxScale {
        from,
        to: GeomBox {
            p1: from.p0 + rows - 1,
            ..from
        },
        fill,
        prevent,
        cleanup: Default::default(),
    }
}

#[test]
fn the_three_scale_y_modes_are_separate_and_exact() {
    let d = doc_with(&column());
    let b = sel_box(&d);
    // ×2 (4 rows → 8 rows), anchored at the bottom row.
    // Transform existing notes: N → N, the rows move apart.
    let r = run(&d, &boxed(b, 8, None, false));
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    assert_eq!(rows, vec![60, 62, 64, 66]);
    // Fill added rows: every row of each note's cell, once.
    let r = run(&d, &boxed(b, 8, Some(Fill::default()), false));
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    assert_eq!(rows, (60..=67).collect::<Vec<_>>());
    // Copy by pitch step, Auto: ⌈8 / 4⌉ − 1 = 1 copy one row up — the same
    // art at an integer factor.
    let r = run(&d, &boxed(b, 8, Some(step_auto()), false));
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    assert_eq!(rows, (60..=67).collect::<Vec<_>>());

    // ×2.5 (4 → 10 rows): cells are 2, 3, 2, 3 rows (deterministic floor of
    // the row edges). Fill rows: solid, never doubled.
    let r = run(&d, &boxed(b, 10, Some(Fill::default()), false));
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    assert_eq!(rows, (60..=69).collect::<Vec<_>>());
    assert_eq!(same_key_overlaps(&r.notes), 0);
    // Copy by pitch step, Auto: ⌈10 / 4⌉ − 1 = 2 copies for every note (one
    // thickness), kept inside the scaled box: 60 61 62 | 62 63 64 | 65 66 67
    // | 67 68 69 — rows 62 and 67 doubled by the step copies.
    let r = run(&d, &boxed(b, 10, Some(step_auto()), false));
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    assert_eq!(rows, vec![60, 61, 62, 62, 63, 64, 65, 66, 67, 67, 68, 69]);
    assert_eq!(same_key_overlaps(&r.notes), 2);
    // With the Scale Y cleanup (B2-F28, exact duplicates) each doubled row
    // keeps one note: the first in source order.
    let r = run(&d, &boxed(b, 10, Some(step_auto()), true));
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    assert_eq!(rows, (60..=69).collect::<Vec<_>>());
    assert_eq!(r.stats.overlaps_removed, 2);
    assert_eq!(same_key_overlaps(&r.notes), 0);
    // Deterministic: the same run twice gives the same notes.
    assert_eq!(
        spots(&run(&d, &boxed(b, 10, Some(step_auto()), true)).notes),
        spots(&r.notes)
    );
}

#[test]
fn copies_inherit_gate_velocity_and_subchannel_as_chosen() {
    // Three subchannels (ids 0, 1, 2 on MIDI channels 1 – 3) so stepping has
    // somewhere to go.
    let on = |p: u8, vel: u8, ch: u8| NoteRecord {
        attrs: NoteAttrs::new(p, vel, ch, 64, NoteOffKind::NoteOff).unwrap(),
        ..rec(100, 240, p, vel, 0)
    };
    let mut d = doc_with(&[on(60, 100, 0), on(61, 90, 1), on(62, 80, 2)]);
    d.track_mut(0).unwrap().infer_subchannels_from_channels();
    let b = sel_box(&d);
    // ×3, Fill rows, full copies keeping the subchannel: gate and velocity
    // of the source on every copy.
    let r = run(&d, &boxed(b, 9, Some(Fill::default()), false));
    assert_eq!(r.notes.len(), 9);
    for n in &r.notes {
        assert_eq!((n.start, n.dur), (100, 240), "gate inherited");
        let src_row = 60 + (n.pitch - 60) / 3;
        let (vel, sub) = [(100, 0), (90, 1), (80, 2)][usize::from(src_row - 60)];
        assert_eq!((n.vel, n.sub), (vel, sub));
    }
    // Fading copies: the scaled note keeps its velocity, copies get softer.
    let fade = Fill {
        fade: true,
        ..Fill::default()
    };
    let r = run(&d, &boxed(b, 9, Some(fade), false));
    let s = spots(&r.notes);
    assert_eq!((s[0].2, s[0].3), (60, 100));
    assert!(s[1].3 < 100 && s[2].3 < s[1].3, "{s:?}");
    // Copies step through the subchannels (one step per row).
    let step_sub = Fill {
        keep_sub: false,
        ..step_auto()
    };
    let r = run(&d, &boxed(b, 9, Some(step_sub), false));
    let s = spots(&r.notes);
    let sub_at = |row: u8| s.iter().find(|x| x.2 == row).map(|x| x.4);
    assert_eq!(
        (sub_at(60), sub_at(61), sub_at(62)),
        (Some(0), Some(1), Some(2))
    );
    // The MIDI channel follows the stepped subchannel's table entry.
    let ch61 = r.notes.iter().find(|n| n.pitch == 61).map(|n| n.ch);
    assert_eq!(ch61, Some(1));
}

#[test]
fn copy_by_pitch_step_with_a_count_reaches_past_the_box_but_not_the_keyboard() {
    // Octave copies: step 12, two copies — leaves the scaled box on purpose.
    let d = doc_with(&[rec(0, 100, 60, 100, 0), rec(0, 100, 110, 100, 0)]);
    let b = sel_box(&d);
    let k = BoxScale {
        from: b,
        to: b,
        fill: Some(Fill {
            mode: FillMode::Step {
                step: 12,
                copies: 2,
            },
            ..Fill::default()
        }),
        prevent: false,
        cleanup: Default::default(),
    };
    assert_eq!(k.per_note(), 3);
    assert_eq!(k.growth(), Growth::Expand(3));
    let src = Source::collect(&d, &SEL, 1 << 20).unwrap();
    let keys = d.project().keys;
    let r = run_kernel(
        &k,
        &src,
        Boundary::default().within(keys),
        &Limits::default(),
        None,
    )
    .unwrap();
    let rows: Vec<u8> = spots(&r.notes).iter().map(|s| s.2).collect();
    // 110 + 24 = 134 is past the 128-key keyboard: dropped, counted.
    assert_eq!(rows, vec![60, 72, 84, 110, 122]);
    assert_eq!(r.stats.pitch_dropped, 1);
}

#[test]
fn preflight_bounds_every_mode() {
    let d = doc_with(&column());
    let b = sel_box(&d);
    for (rows, fill) in [
        (8, None),
        (10, Some(Fill::default())),
        (
            10,
            Some(Fill {
                res: 2,
                ..Fill::default()
            }),
        ),
        (10, Some(step_auto())),
        (40, Some(step_auto())),
    ] {
        let k = boxed(b, rows, fill, false);
        let r = run(&d, &k);
        assert!(
            r.notes.len() as u64 <= 4 * k.per_note(),
            "{rows} rows {fill:?}: {} > 4 × {}",
            r.notes.len(),
            k.per_note()
        );
    }
}

/// B2-F28 (beta2 semantics; beta1's B1-0116 spared the source's own
/// stacks): with "Clean up Scale Y overlaps", Exact duplicates mode, results
/// that share key, layer, start and length become one note (the first in
/// source order); same-key notes with other starts or lengths and other keys
/// stay.
#[test]
fn scale_y_cleanup_keeps_other_keys_and_distinct_gates_and_collapses_exact_stacks() {
    let d = doc_with(&[
        // Same-pitch polyphony on 60 (overlapping, not identical).
        rec(0, 480, 60, 100, 0),
        rec(240, 480, 60, 90, 0),
        // An identical stack on 61.
        rec(0, 480, 61, 80, 0),
        rec(0, 480, 61, 80, 0),
        // A chord partner on 63 at the same time.
        rec(0, 480, 63, 70, 0),
    ]);
    let b = sel_box(&d);
    // ×2.5 with step copies and the cleanup.
    let r = run(&d, &boxed(b, 10, Some(step_auto()), true));
    let s = spots(&r.notes);
    // The polyphony on 60 and its copies on 61 / 62 stay as pairs.
    for row in [60u8, 61, 62] {
        let starts: Vec<u64> = s.iter().filter(|x| x.2 == row).map(|x| x.0).collect();
        assert_eq!(starts, vec![0, 240], "row {row}: {s:?}");
    }
    // Row 62 at tick 0: the copy of 60 comes first in source order and stays;
    // the stack's scaled note there goes.
    assert_eq!(
        s.iter()
            .filter(|x| (x.0, x.2) == (0, 62))
            .map(|x| x.3)
            .collect::<Vec<_>>(),
        vec![100]
    );
    // The stack lands on 63 / 64 as one note each.
    for row in [63u8, 64] {
        assert_eq!(
            s.iter()
                .filter(|x| x.2 == row)
                .map(|x| x.3)
                .collect::<Vec<_>>(),
            vec![80],
            "row {row}"
        );
    }
    // The chord partner's rows are untouched.
    assert_eq!(
        s.iter()
            .filter(|x| x.3 == 70)
            .map(|x| x.2)
            .collect::<Vec<_>>(),
        vec![67, 68, 69]
    );
    assert_eq!(r.stats.overlaps_removed, 4);
    // Transform mode, no fill: a stretch is not a shrink, nothing is cleaned
    // (N → N, the stack too).
    let r = run(&d, &boxed(b, 10, None, true));
    assert_eq!((r.notes.len(), r.stats.overlaps_removed), (5, 0));
}

#[test]
fn shrink_then_restretch_does_not_multiply_duplicates() {
    // The owner's case: art shrunk first, then stretched back with Duplicate
    // Notes. Eight rows of one long note each, at staggered times.
    let notes: Vec<NoteRecord> = (0..8u8)
        .map(|i| rec(u64::from(i) * 10, 200, 60 + i, 100, 0))
        .collect();
    let mut d = doc_with(&notes);
    let mut h = History::default();
    let limits = Limits::default();
    // Shrink ×0.5 (Transform) without prevention: pairs of rows meet on one
    // key, overlapping but not identical — the §086 cleanup keeps both.
    let half = ExactFactor::parse("0.5").unwrap();
    let e = editor::prepare_scale_y_with(&d, 0, &half, ScaleY::default(), &limits).unwrap();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: e,
            label: "Scale Y",
        },
    )
    .unwrap();
    let t = d.track(0).unwrap();
    assert_eq!(t.notes.len(), 8);
    // Restretch ×2 with Fill rows, no prevention: the overlaps double.
    let two = ExactFactor::parse("2").unwrap();
    let plain = ScaleY {
        fill: Some(Fill::default()),
        prevent_overlap: false,
        cleanup: Default::default(),
    };
    let e = editor::prepare_scale_y_with(&d, 0, &two, plain, &limits).unwrap();
    let mut dd = d.clone();
    h.apply(
        &mut dd,
        Operation::Prepared {
            edit: e,
            label: "Scale Y",
        },
    )
    .unwrap();
    let doubled = dd.track(0).unwrap().notes.len();
    assert!(doubled > 8, "{doubled}");
    // B2-F28: Exact duplicates (the default cleanup) removes nothing here —
    // the staggered rows meet on one key but never at the same start.
    let exact = ScaleY {
        fill: None,
        prevent_overlap: true,
        cleanup: ScaleYCleanup::ExactDuplicates,
    };
    let e = editor::prepare_scale_y_with(&doc_with(&notes), 0, &half, exact, &limits).unwrap();
    let mut de = doc_with(&notes);
    editor::commit(&mut de, e).unwrap();
    assert_eq!(de.track(0).unwrap().notes.len(), 8);
    // The broader labelled modes clean the owner's case on the shrink, and
    // the restretch adds no same-key overlap: Remove keeps the earlier note
    // of each pair (beta1's result), Trim keeps both and cuts the earlier
    // one at the later attack.
    for (cleanup, after_shrink, trimmed) in [
        (ScaleYCleanup::RemoveOverlapping, 4, 0),
        (ScaleYCleanup::TrimEarlier, 8, 4),
    ] {
        let mut d2 = doc_with(&notes);
        let mut h2 = History::default();
        let before = d2.fingerprint();
        let on = ScaleY {
            fill: None,
            prevent_overlap: true,
            cleanup,
        };
        let e = editor::prepare_scale_y_with(&d2, 0, &half, on, &limits).unwrap();
        h2.apply(
            &mut d2,
            Operation::Prepared {
                edit: e,
                label: "Scale Y",
            },
        )
        .unwrap();
        let shrunk = notes_of(&d2);
        assert_eq!(shrunk.len(), after_shrink, "{cleanup:?}");
        assert_eq!(same_key_overlaps(&shrunk), 0, "{cleanup:?}");
        assert_eq!(
            shrunk.iter().filter(|n| n.dur < 200).count(),
            trimmed,
            "{cleanup:?}"
        );
        let on_fill = ScaleY {
            fill: Some(Fill::default()),
            prevent_overlap: true,
            cleanup,
        };
        let e = editor::prepare_scale_y_with(&d2, 0, &two, on_fill, &limits).unwrap();
        h2.apply(
            &mut d2,
            Operation::Prepared {
                edit: e,
                label: "Scale Y",
            },
        )
        .unwrap();
        assert_eq!(same_key_overlaps(&notes_of(&d2)), 0, "{cleanup:?}");
        // One undo step per Scale Y: two undos restore the original exactly,
        // and redo replays the same result.
        let after = d2.fingerprint();
        h2.undo(&mut d2).unwrap();
        h2.undo(&mut d2).unwrap();
        assert_eq!(d2.fingerprint(), before, "{cleanup:?}");
        h2.redo(&mut d2).unwrap();
        h2.redo(&mut d2).unwrap();
        assert_eq!(d2.fingerprint(), after, "{cleanup:?}");
    }
}

/// The notes of track 0 as `TNote`s (start, length, pitch, velocity).
fn notes_of(d: &PlumDocument) -> Vec<TNote> {
    d.track(0)
        .unwrap()
        .notes
        .iter()
        .map(|r| TNote {
            start: r.start_tick,
            dur: r.duration_ticks,
            pitch: r.attrs.pitch(),
            vel: r.attrs.velocity(),
            ..TNote::default()
        })
        .collect()
}

/// B2-F28 (beta2; beta1 spared overlaps the source already had): on a
/// shrink the cleanup treats the selection's own overlaps and the ones the
/// shrink creates alike; each mode does only what its label says.
#[test]
fn on_a_shrink_the_cleanup_modes_treat_source_and_created_overlaps_alike() {
    // 60 / 61 meet on one key under ×0.5 at overlapping times: created.
    // 64 / 64 overlap already.
    let notes = [
        rec(0, 100, 60, 100, 0),
        rec(50, 100, 61, 90, 0),
        rec(0, 100, 64, 80, 0),
        rec(50, 100, 64, 70, 0),
    ];
    let d = doc_with(&notes);
    let half = ExactFactor::parse("0.5").unwrap();
    let shrink = |cleanup| {
        let opts = ScaleY {
            fill: None,
            prevent_overlap: true,
            cleanup,
        };
        let e = editor::prepare_scale_y_with(&d, 0, &half, opts, &Limits::default()).unwrap();
        let mut d2 = d.clone();
        editor::commit(&mut d2, e).unwrap();
        let mut got: Vec<(u64, u64, u8, u8)> = d2
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|r| {
                (
                    r.start_tick,
                    r.duration_ticks,
                    r.attrs.pitch(),
                    r.attrs.velocity(),
                )
            })
            .collect();
        got.sort_unstable();
        got
    };
    // 60 and 61 → 61 (pivot 62); 64 → 63. No exact duplicates: all four stay.
    assert_eq!(shrink(ScaleYCleanup::ExactDuplicates).len(), 4);
    // Remove: the earlier note of each pair stays, created or not.
    assert_eq!(
        shrink(ScaleYCleanup::RemoveOverlapping),
        vec![(0, 100, 61, 100), (0, 100, 63, 80)]
    );
    // Trim: both stay, the earlier ones end at the later attack.
    assert_eq!(
        shrink(ScaleYCleanup::TrimEarlier),
        vec![
            (0, 50, 61, 100),
            (0, 50, 63, 80),
            (50, 100, 61, 90),
            (50, 100, 63, 70)
        ]
    );
    // Off: the default Scale Y keeps all four (only identical results go).
    let e = editor::prepare_scale_y(&d, 0, &half).unwrap();
    let mut d3 = d.clone();
    editor::commit(&mut d3, e).unwrap();
    assert_eq!(d3.track(0).unwrap().notes.len(), 4);
    const { assert!(SCALE_Y_PREVENT_DEFAULT, "B1-0116: the stated default is On") };
}

#[test]
fn the_dialog_kernel_and_the_commit_agree() {
    // Scale Y's Duplicate Notes kernel is the one the Preview evaluates.
    let d = doc_with(&column());
    let t = editor::note_view(&d, 0).unwrap();
    let f = ExactFactor::parse("2.5").unwrap();
    let mut k = scale_y_fill_kernel(t, &f, step_auto(), d.project().keys).unwrap();
    k.prevent = true;
    let preview = run(&d, &k);
    let opts = ScaleY {
        fill: Some(step_auto()),
        prevent_overlap: true,
        cleanup: Default::default(),
    };
    let e = editor::prepare_scale_y_with(&d, 0, &f, opts, &Limits::default()).unwrap();
    let mut d2 = d.clone();
    editor::commit(&mut d2, e).unwrap();
    let mut got: Vec<(u64, u64, u8)> = d2
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
    got.sort_unstable();
    let want: Vec<(u64, u64, u8)> = spots(&preview.notes)
        .iter()
        .map(|s| (s.0, s.1, s.2))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn the_overlap_filter_is_exact_on_unsorted_input_too() {
    let spot = |start: u64, end: u64, pitch: u8| NoteSpot {
        track: 0,
        sub: 0,
        ch: 0,
        pitch,
        start,
        end,
    };
    // Pushed out of start order: the later-start copy first.
    let mut f = OverlapFilter::default();
    f.push(spot(50, 150, 61), 1, spot(50, 150, 60)); // copy of a 60
    f.push(spot(0, 100, 61), 0, spot(0, 100, 61)); // a scaled 61, overlapping
    f.push(spot(200, 300, 61), 1, spot(200, 300, 60)); // apart: stays
    assert_eq!(f.finish(), vec![0], "the copy loses to the scaled note");
}

/// B1-S38 measurement (release): 1,000,000 notes of dense art (128 rows ×
/// 7,813 columns, 16 rows tall) stretched ×2.5 with Copy by pitch step and
/// overlap prevention.
#[test]
#[ignore = "measurement: cargo test --release -p plumspirit_engine --test b1_scale_y_dup -- --ignored --nocapture"]
fn a_million_notes_stretch_with_step_copies_and_prevention() {
    let mut v = Vec::with_capacity(1_000_000);
    'outer: for col in 0..u64::MAX {
        for row in 0..16u8 {
            if v.len() == 1_000_000 {
                break 'outer;
            }
            v.push(rec(col * 120, 100, 40 + row, 100, 0));
        }
    }
    let d = doc_with(&v);
    let b = sel_box(&d);
    let k = boxed(b, 40, Some(step_auto()), true);
    let t = std::time::Instant::now();
    let r = run(&d, &k);
    let ms = t.elapsed().as_millis();
    println!(
        "B1-S38: 1,000,000 notes x2.5 Copy by pitch step + prevention -> {} notes, {} overlaps removed, {} ms",
        r.notes.len(),
        r.stats.collapsed,
        ms
    );
    assert_eq!(same_key_overlaps_sorted(&r.notes), 0);
}

/// [`same_key_overlaps`] in O(n log n) for the measurement.
fn same_key_overlaps_sorted(v: &[TNote]) -> usize {
    let mut k: Vec<_> = v
        .iter()
        .map(|n| (n.sub, n.ch, n.pitch, n.start, n.end()))
        .collect();
    k.sort_unstable();
    k.windows(2)
        .filter(|w| (w[0].0, w[0].1, w[0].2) == (w[1].0, w[1].1, w[1].2) && w[1].3 < w[0].4)
        .count()
}
