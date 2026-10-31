//! v0.6 analytics stress (contract §235–§241): random seeks over 1M / 5M notes
//! (one track and spread over hundreds of tracks), rapid large edits with
//! incremental index updates, and massive same-tick stacks.
//!
//! The quick fixture runs with `cargo test`. The heavyweight ones are `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress_v06 -- --ignored --nocapture --test-threads=1
//! ```

use plumspirit_engine::analysis::profile::PeakProfile;
use plumspirit_engine::analysis::{
    AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing, TrackInput, for_each_sounding,
};
use plumspirit_engine::editor::transforms::prepare_scale_x;
use plumspirit_engine::editor::{
    self, Clipboard, EditControl, ExactFactor, QuantizeMode, QuantizeSpec,
};
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::time::{Duration, Instant};

fn us(d: Duration) -> String {
    format!("{:.1} µs", d.as_secs_f64() * 1e6)
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1e3)
}

fn lcg(s: &mut u64) -> u64 {
    *s = s
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *s >> 11
}

/// The Dense fixture's notes spread round-robin over `tracks` tracks.
fn spread(notes: usize, tracks: usize) -> PlumDocument {
    let src = generate(FixtureShape::Dense, notes, 960, 42);
    let mut d = PlumDocument::new_project("spread".into(), Bpm::DEFAULT, 960);
    let mut per: Vec<Vec<NoteRecord>> = vec![Vec::new(); tracks];
    for (i, v) in src.track(1).unwrap().notes.iter().enumerate() {
        per[i % tracks].push(v.record());
    }
    drop(src);
    for (k, recs) in per.iter().enumerate() {
        let ti = if k == 0 { 0 } else { d.add_track().unwrap() };
        d.track_mut(ti)
            .unwrap()
            .notes
            .insert_sorted_batch(recs, false)
            .unwrap();
    }
    d
}

fn percentiles(v: &mut [Duration]) -> (Duration, Duration, Duration) {
    v.sort_unstable();
    let n = v.len();
    (v[n / 2], v[(n * 95) / 100], v[n - 1])
}

/// Notes sounding at `t` and started by `t`, by walking every note (reference).
fn brute(doc: &PlumDocument, t: u64) -> (u64, u64) {
    let (mut passed, mut poly) = (0, 0);
    for tr in doc.tracks() {
        for v in tr.notes.iter() {
            let r = v.record();
            if r.start_tick <= t {
                passed += 1;
                if t < r.start_tick + r.duration_ticks {
                    poly += 1;
                }
            }
        }
    }
    (passed, poly)
}

fn seek_report(name: &str, doc: &PlumDocument, seeks: usize) {
    let t0 = Instant::now();
    let idx = AnalysisIndex::for_document(doc);
    let built = t0.elapsed();
    let timing = Timing::for_document(doc);
    let end = doc.content_end_tick().max(1);
    let mut s = 0x5EED;
    let mut v: Vec<Duration> = (0..seeks)
        .map(|_| {
            let t = lcg(&mut s) % end;
            let a = Instant::now();
            let m = idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
            let d = a.elapsed();
            assert!(m.poly <= m.notes_passed);
            d
        })
        .collect();
    let (p50, p95, max) = percentiles(&mut v);
    println!(
        "{name}: index {} ({:.2} B/note), {seeks} random seeks: median {}, p95 {}, max {}",
        ms(built),
        idx.heap_bytes() as f64 / doc.note_count() as f64,
        us(p50),
        us(p95),
        us(max)
    );
    // Spot-check exactness against a full walk.
    for _ in 0..3 {
        let t = lcg(&mut s) % end;
        let m = idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
        assert_eq!((m.notes_passed, m.poly), brute(doc, t), "{name} at {t}");
    }
    assert!(p95 < Duration::from_millis(2), "{name}: p95 {}", us(p95));
}

#[test]
fn a_stack_of_200k_notes_on_one_tick_is_counted_exactly() {
    let mut d = PlumDocument::new_project("stack".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..200_000u64)
        .map(|i| NoteRecord {
            start_tick: 960,
            duration_ticks: 1 + i % 960,
            attrs: NoteAttrs::new((i % 128) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    for t in [959, 960, 961, 1400, 1918, 1919, 1920, 5000] {
        let m = idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
        assert_eq!((m.notes_passed, m.poly), brute(&d, t), "at {t}");
    }
    // The keyboard reconstruction visits each sounding note once.
    let mut n = 0u64;
    for_each_sounding(&d.track(0).unwrap().notes, None, 1000, |_| n += 1);
    assert_eq!(n, brute(&d, 1000).1);
    // Stacked ticks compress to almost nothing.
    assert!(
        idx.heap_bytes() < 200_000,
        "{} bytes for 200k stacked notes",
        idx.heap_bytes()
    );
}

#[test]
#[ignore = "heavyweight: run with --release --ignored --nocapture"]
fn random_seeks_over_one_and_five_million_notes() {
    let d = generate(FixtureShape::Dense, 1_000_000, 960, 42);
    seek_report("1M notes, 1 track", &d, 20_000);
    drop(d);
    let d = spread(1_000_000, 1024);
    seek_report("1M notes, 1,024 tracks", &d, 20_000);
    drop(d);
    let d = generate(FixtureShape::Dense, 5_000_000, 960, 42);
    seek_report("5M notes, 1 track", &d, 20_000);
    drop(d);
    let d = spread(5_000_000, 512);
    seek_report("5M notes, 512 tracks", &d, 20_000);
}

#[test]
#[ignore = "heavyweight: run with --release --ignored --nocapture"]
fn rapid_large_edits_keep_the_index_exact_and_incremental() {
    let mut d = generate(FixtureShape::Dense, 1_000_000, 960, 42);
    let timing = Timing::for_document(&d);
    let mut h = History::default();
    let mut idx = AnalysisIndex::for_document(&d);
    let end = d.content_end_tick();
    let track = 1;
    let bar = 3840u64;
    let mut s = 77u64;
    let probes: Vec<u64> = (0..40).map(|_| lcg(&mut s) % (end + bar * 600)).collect();
    let step = |label: &str, d: &PlumDocument, idx: &mut AnalysisIndex| {
        let t = Instant::now();
        let next = AnalysisIndex::build(&TrackInput::all(d), Some(idx), None).unwrap();
        let took = t.elapsed();
        let fresh = AnalysisIndex::for_document(d);
        for &p in &probes {
            assert_eq!(
                next.metrics(&Scope::Project, &timing, p, DEFAULT_WINDOW_NS),
                fresh.metrics(&Scope::Project, &timing, p, DEFAULT_WINDOW_NS),
                "{label} at {p}"
            );
        }
        println!(
            "{label}: index update {} (pieces built {}, timeline {}, segments rewritten {}, delta {} ticks)",
            ms(took),
            next.stats.pieces_built,
            if next.stats.timeline_full {
                "rebuilt"
            } else {
                "patched"
            },
            next.stats.segments_rewritten,
            next.stats.delta_events
        );
        *idx = next;
    };
    // Large paste: ~100,000 notes (520 bars) copied from the middle, pasted after the end.
    let n =
        d.track_mut(track)
            .unwrap()
            .notes
            .select_in_region(end / 2, end / 2 + bar * 520, 0, 127);
    let clip = Clipboard::copy_selected(d.track(track).unwrap())
        .unwrap()
        .unwrap();
    let e = clip.prepare_paste(&d, track, end + bar).unwrap();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: e,
            label: "Paste",
        },
    )
    .unwrap();
    step(&format!("paste {n} notes"), &d, &mut idx);
    // Large delete (the pasted notes are selected after a paste).
    let sel = d.track(track).unwrap().notes.selected_count();
    h.apply(
        &mut d,
        Operation::DeleteSelected {
            track: track.into(),
        },
    )
    .unwrap();
    step(&format!("delete {sel} notes"), &d, &mut idx);
    h.undo(&mut d).unwrap();
    step("undo the delete", &d, &mut idx);
    h.redo(&mut d).unwrap();
    step("redo the delete", &d, &mut idx);
    // Quantize a quarter of the song.
    d.track_mut(track)
        .unwrap()
        .notes
        .select_in_region(0, end / 4, 0, 127);
    let spec = QuantizeSpec {
        mode: QuantizeMode::Both,
        grid: 960,
        origin: 0,
    };
    let e = editor::prepare_quantize(&d, track, &spec, EditControl::default()).unwrap();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: e,
            label: "Quantize",
        },
    )
    .unwrap();
    step("quantize a quarter of the song (Both, 1/4)", &d, &mut idx);
    // Scale X of one bar's notes (x2).
    d.track_mut(track).unwrap().notes.clear_selection();
    d.track_mut(track)
        .unwrap()
        .notes
        .select_in_region(end / 2, end / 2 + bar, 0, 127);
    let e = prepare_scale_x(
        &d,
        track,
        &ExactFactor::parse("2").unwrap(),
        EditControl::default(),
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: e,
            label: "Scale",
        },
    )
    .unwrap();
    step("scale X one bar of notes (x2)", &d, &mut idx);
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    step("undo scale + quantize", &d, &mut idx);
}

#[test]
#[ignore = "heavyweight: run with --release --ignored --nocapture"]
fn a_two_million_note_stack_and_its_peaks_are_exact() {
    let mut d = PlumDocument::new_project("stack".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..2_000_000u64)
        .map(|i| NoteRecord {
            start_tick: 960 * (i / 1_000_000),
            duration_ticks: 1 + i % 5000,
            attrs: NoteAttrs::new((i % 128) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    let mut sorted = recs;
    sorted.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&sorted, false)
        .unwrap();
    drop(sorted);
    let t = Instant::now();
    let idx = AnalysisIndex::for_document(&d);
    let built = t.elapsed();
    let timing = Timing::for_document(&d);
    for t in [0, 959, 960, 961, 4999, 5000, 5959, 5960, 9000] {
        let m = idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
        assert_eq!((m.notes_passed, m.poly), brute(&d, t), "at {t}");
    }
    let prof = PeakProfile::build(&idx, &Scope::Project, &timing, DEFAULT_WINDOW_NS, None).unwrap();
    // Peak poly: both stacks overlap from tick 960 (all of the second stack
    // plus the first stack's notes longer than 960 ticks).
    let want = brute(&d, 960).1;
    assert_eq!(prof.peak_poly_so_far(&idx, &Scope::Project, 10_000), want);
    assert_eq!(
        prof.peak_nps_so_far(&idx, &Scope::Project, &timing, 10_000),
        2_000_000,
        "both stacks start within one second"
    );
    println!(
        "2M-note stack: index {} ({} bytes), peak poly {want}",
        ms(built),
        idx.heap_bytes()
    );
}
