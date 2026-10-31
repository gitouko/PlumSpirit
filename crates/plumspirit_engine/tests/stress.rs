//! Heavyweight architecture tests on Black-MIDI-scale fixtures (contract §128–131).
//!
//! These assert *architectural* properties (bounded work, no full scans, O(n)
//! batch edits), not machine-dependent wall-clock thresholds. They are ignored by
//! default because they allocate hundreds of MB; run them with:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress -- --ignored --test-threads=1
//! ```

use plumspirit_engine::editor::{self, Clipboard, ExactFactor, commit};
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::midi::{export_to_bytes, import_bytes, validate_exported_bytes};
use plumspirit_engine::model::note_store::DEFAULT_CHUNK_TARGET;
use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::lod::{DensityRaster, EXACT_RENDER_LIMIT};
use std::time::Instant;

const TRACK: usize = 1;

#[test]
#[ignore = "heavyweight: 1M notes"]
fn one_million_note_journey() {
    let t = Instant::now();
    let mut doc = generate(FixtureShape::Dense, 1_000_000, 960, 42);
    eprintln!("generate: {:?}", t.elapsed());
    let store = &doc.track(TRACK).unwrap().notes;
    assert_eq!(store.len(), 1_000_000);
    // Compact storage: ~20 bytes per note, no per-note heap objects.
    let bytes_per_note = store.heap_bytes() as f64 / store.len() as f64;
    assert!(bytes_per_note < 24.0, "{bytes_per_note} bytes/note");
    store.validate().unwrap();

    // Viewport query in the middle scans only a bounded number of notes.
    let end = store.max_end().unwrap();
    let mid = end / 2;
    let st = store.query(mid, mid + 960 * 16, |_, _| {});
    assert!(st.notes_matched > 0);
    assert!(
        st.notes_scanned <= st.notes_matched + 3 * DEFAULT_CHUNK_TARGET,
        "query scanned {} to find {}",
        st.notes_scanned,
        st.notes_matched
    );
    assert!(st.chunks_visited <= 4, "{st:?}");
    // The estimate used for LOD decisions touches no notes and bounds the truth.
    assert!(store.estimate_in_range(0, end) >= store.count_in_range(0, end));

    // Zoomed out: LOD activates, raster work is bounded by pixels, buffers reused.
    let axis = TimeAxis::new(0, 1600.0 / end as f64);
    assert!(store.estimate_in_range(0, end) > EXACT_RENDER_LIMIT);
    let mut raster = DensityRaster::new();
    raster.rebuild(store, &axis, 1600, 0, 127);
    let cap = raster.heap_bytes();
    raster.rebuild(store, &axis, 1600, 0, 127);
    assert_eq!(raster.heap_bytes(), cap, "raster reallocated on rebuild");
    assert!(cap < 2 * 1024 * 1024);

    // Small edits touch one chunk, not the whole track.
    let fp_other = doc.track(0).unwrap().events.len();
    let t = Instant::now();
    let tr = doc.track_mut(TRACK).unwrap();
    let n = tr.notes.select_in_region(mid, mid + 960, 0, 127);
    assert!(n > 0 && n < 1000);
    let clip = Clipboard::copy_selected(tr).unwrap().unwrap();
    assert_eq!(clip.len(), n, "copy duplicates only the selection");
    let p = clip.prepare_paste(&doc, TRACK, mid + 960 * 64).unwrap();
    commit(&mut doc, p).unwrap();
    editor::insert_note(
        &mut doc,
        TRACK,
        NoteRecord {
            start_tick: mid + 5,
            duration_ticks: 100,
            attrs: NoteAttrs::new(64, 90, 3, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        },
    )
    .unwrap();
    editor::delete_selected(&mut doc, TRACK).unwrap();
    eprintln!("small edits: {:?}", t.elapsed());
    assert_eq!(doc.track(TRACK).unwrap().notes.len(), 1_000_000 + n);
    assert_eq!(doc.track(0).unwrap().events.len(), fp_other);

    // Flip Y on a subset twice restores exactly (in place, O(selected)).
    let tr = doc.track_mut(TRACK).unwrap();
    tr.notes.clear_selection();
    tr.notes.select_in_region(0, end / 10, 0, 127);
    let fp = doc.fingerprint();
    editor::flip_y(&mut doc, TRACK).unwrap();
    editor::flip_y(&mut doc, TRACK).unwrap();
    assert_eq!(doc.fingerprint(), fp);

    // Full-track transform is O(n log n) and atomic on failure.
    doc.track_mut(TRACK).unwrap().notes.select_all();
    let t = Instant::now();
    editor::scale_x(&mut doc, TRACK, &ExactFactor::parse("2").unwrap()).unwrap();
    editor::scale_x(&mut doc, TRACK, &ExactFactor::parse("0.5").unwrap()).unwrap();
    eprintln!("scale x2 + x0.5 on 1M: {:?}", t.elapsed());
    let fp = doc.fingerprint();
    assert!(editor::scale_x(&mut doc, TRACK, &ExactFactor::parse("-3").unwrap()).is_err());
    assert!(editor::scale_y(&mut doc, TRACK, &ExactFactor::parse("5").unwrap()).is_err());
    assert_eq!(
        doc.fingerprint(),
        fp,
        "failed transform changed the document"
    );
    doc.track(TRACK).unwrap().notes.validate().unwrap();

    // Export, validate, re-import.
    let t = Instant::now();
    let bytes = export_to_bytes(&doc).unwrap();
    let report =
        validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &bytes).unwrap();
    let (doc2, _) = import_bytes(&bytes).unwrap();
    eprintln!(
        "export+validate+reimport: {:?} ({} bytes)",
        t.elapsed(),
        bytes.len()
    );
    assert_eq!(doc2.note_count(), doc.note_count());
    if !report.pairing_differs() {
        assert_eq!(doc2.fingerprint(), doc.fingerprint());
    }
}

#[test]
#[ignore = "heavyweight: dense 1M notes in four beats"]
fn extreme_density_uses_lod_and_bounded_memory() {
    let doc = generate(FixtureShape::ExtremeDensity, 1_000_000, 960, 5);
    let store = &doc.track(TRACK).unwrap().notes;
    let end = store.max_end().unwrap();
    assert!(end < 960 * 5);
    let axis = TimeAxis::new(0, 1600.0 / end as f64);
    assert!(
        store.estimate_in_range(0, end) > EXACT_RENDER_LIMIT,
        "LOD must activate"
    );
    let mut raster = DensityRaster::new();
    raster.rebuild(store, &axis, 1600, 0, 127);
    assert_eq!(raster.notes_rasterized(), store.len());
    assert!(
        raster.heap_bytes() < 2 * 1024 * 1024,
        "frame memory must not scale with notes"
    );
    // Zooming in reduces the work proportionally: a 1-tick window holds only the
    // notes that truly overlap that tick (here ~2% of the song). In this fixture
    // thousands of notes overlap every tick, so no zoom level can separate them and
    // the raster stays the right representation.
    let near = store.count_in_range(100, 100);
    assert!(near < store.len() / 20, "{near}");
    // On ordinary dense material, zooming in to one bar switches back to exact
    // rectangles (the LOD is visual only and depends on what is on screen).
    let dense = generate(FixtureShape::Dense, 1_000_000, 960, 6);
    let s = &dense.track(TRACK).unwrap().notes;
    let mid = s.max_end().unwrap() / 2;
    assert!(s.estimate_in_range(0, s.max_end().unwrap()) > EXACT_RENDER_LIMIT);
    assert!(s.estimate_in_range(mid, mid + 960 * 4) < EXACT_RENDER_LIMIT);
}

#[test]
#[ignore = "heavyweight: long sparse timeline"]
fn long_sparse_timeline_is_precise_and_bounded() {
    let doc = generate(FixtureShape::LongSparse, 20_000, 960, 9);
    let store = &doc.track(TRACK).unwrap().notes;
    let end = store.max_end().unwrap();
    assert!(end > 3_000_000_000_000, "{end}");
    // A query near the far end does not scan from tick 0.
    let st = store.query(end - 1000, end, |_, _| {});
    assert!(st.notes_scanned <= DEFAULT_CHUNK_TARGET, "{st:?}");
    // Coordinates are exact relative to a far origin.
    let axis = TimeAxis::new(end - 10_000, 0.01);
    assert_eq!(axis.tick_to_x(end), 100.0);
    // Exportable (every gap below the delta limit) and round-trips.
    let bytes = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &bytes).unwrap();
}

#[test]
#[ignore = "heavyweight: repeated open / edit / export cycles"]
fn repeated_cycles_do_not_leak_state() {
    let small = export_to_bytes(&generate(FixtureShape::Dense, 2_000, 480, 1)).unwrap();
    let large = export_to_bytes(&generate(FixtureShape::Dense, 300_000, 960, 2)).unwrap();
    for round in 0..3 {
        for bytes in [&small, &large, &small] {
            let (mut doc, _) = import_bytes(bytes).unwrap();
            let before = doc.fingerprint();
            doc.track_mut(TRACK).unwrap().notes.select_all();
            editor::flip_x(&mut doc, TRACK).unwrap();
            editor::flip_x(&mut doc, TRACK).unwrap();
            let out1 = export_to_bytes(&doc).unwrap();
            let out2 = export_to_bytes(&doc).unwrap();
            assert_eq!(out1, out2, "round {round}: repeated export differs");
            let (re, _) = import_bytes(&out1).unwrap();
            assert_eq!(re.note_count(), doc.note_count());
            // Flip X twice keeps the note multiset.
            let mut a: Vec<_> = re
                .track(TRACK)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.record())
                .collect();
            let mut b: Vec<_> = import_bytes(bytes)
                .unwrap()
                .0
                .track(TRACK)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.record())
                .collect();
            a.sort_by_key(|r| (r.start_tick, r.duration_ticks, r.attrs.raw()));
            b.sort_by_key(|r| (r.start_tick, r.duration_ticks, r.attrs.raw()));
            assert_eq!(a, b);
            let _ = before;
        }
    }
}
