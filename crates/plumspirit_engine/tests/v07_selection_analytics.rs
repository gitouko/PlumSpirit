//! Selection analytics (contract §132–134, TEST_MATRIX T-SELAN-*): the v0.6
//! metrics restricted to the selected notes, Peak Poly (instances, may exceed
//! 128), Unique Pitch Poly and Poly Area — each against brute force.

use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput};
use plumspirit_engine::analysis::{AnalysisIndex, Scope, Timing};
use plumspirit_engine::model::correction::Correction;
use plumspirit_engine::model::track::GatePercent;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// Deterministic pseudo-random notes (LCG).
fn noise(n: usize, seed: u64) -> Vec<NoteRecord> {
    let mut x = seed;
    let mut next = move || {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        x >> 33
    };
    let mut v: Vec<NoteRecord> = (0..n)
        .map(|_| {
            let s = next() % 20_000;
            let d = next() % 1_500;
            let p = 40 + (next() % 30) as u8;
            rec(s, d, p)
        })
        .collect();
    v.sort_by_key(|r| r.start_tick);
    v
}

fn doc(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("s".into(), Bpm::DEFAULT, 960);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(notes, false)
        .unwrap();
    d
}

/// Selects every note whose index passes `f` (store order = start order).
fn select(d: &mut PlumDocument, f: impl Fn(usize, &NoteRecord) -> bool) -> Vec<NoteRecord> {
    let all: Vec<NoteRecord> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|v| v.record())
        .collect();
    let picked: Vec<NoteRecord> = all
        .iter()
        .enumerate()
        .filter(|(i, r)| f(*i, r))
        .map(|(_, r)| *r)
        .collect();
    let store = &mut d.track_mut(0).unwrap().notes;
    store.clear_selection();
    for r in &picked {
        store.select_in_region(
            r.start_tick,
            r.start_tick + 1,
            r.attrs.pitch(),
            r.attrs.pitch(),
        );
    }
    picked
}

fn input(d: &PlumDocument, correction: Option<Correction>) -> Vec<SelectionInput> {
    vec![SelectionInput {
        id: d.track(0).unwrap().id(),
        notes: d.track(0).unwrap().notes.clone(),
        correction,
        shift: 0,
    }]
}

/// Brute force: poly and distinct pitches at `t` over `notes`.
fn brute_at(notes: &[NoteRecord], t: u64) -> (u64, u32) {
    let mut pitches = [false; 128];
    let mut poly = 0;
    for r in notes {
        if r.start_tick <= t && t < r.start_tick + r.duration_ticks {
            poly += 1;
            pitches[usize::from(r.attrs.pitch())] = true;
        }
    }
    (poly, pitches.iter().filter(|&&b| b).count() as u32)
}

#[test]
fn counts_peaks_and_poly_area_equal_brute_force() {
    let notes = noise(600, 7);
    let mut d = doc(&notes);
    // Every third note (selection by start: select_in_region picks every note
    // starting at that tick and pitch, so build the reference from the store).
    select(&mut d, |i, _| i % 3 == 0);
    let sel: Vec<NoteRecord> = {
        let mut v = Vec::new();
        d.track(0)
            .unwrap()
            .notes
            .for_each_selected(|n| v.push(n.record()));
        v
    };
    assert!(sel.len() >= 200);
    let timing = Timing::for_document(&d);
    let a = SelectionAnalysis::build(&input(&d, None), &timing, None).unwrap();
    assert_eq!(a.selected, sel.len() as u64);
    assert_eq!(a.counted, sel.len() as u64);
    // T-SELAN-PEAK / UNIQUE: maxima over every event tick.
    let mut ticks: Vec<u64> = sel
        .iter()
        .flat_map(|r| [r.start_tick, r.start_tick + r.duration_ticks])
        .collect();
    ticks.sort_unstable();
    ticks.dedup();
    let (mut pp, mut pu) = (0, 0);
    for &t in &ticks {
        let (p, u) = brute_at(&sel, t);
        pp = pp.max(p);
        pu = pu.max(u);
    }
    assert_eq!(a.peak_poly, pp);
    assert_eq!(a.peak_unique, pu);
    assert_eq!(brute_at(&sel, a.peak_poly_tick).0, pp);
    assert_eq!(brute_at(&sel, a.peak_unique_tick).1, pu);
    // T-SELAN-AREA: ∫ P(t) dt over every tick == Σ lengths.
    let (lo, hi) = (a.first_tick, a.last_tick);
    let brute_area: u128 = (lo..hi).map(|t| u128::from(brute_at(&sel, t).0)).sum();
    assert_eq!(a.area_ticks, brute_area);
    // 120 BPM, 960 PPQ: one tick = 1/1920 s.
    let secs = a.area_note_seconds();
    assert!((secs - brute_area as f64 / 1920.0).abs() < 1e-3, "{secs}");
    assert!((a.avg_poly() - brute_area as f64 / (hi - lo) as f64).abs() < 1e-6);
}

#[test]
fn position_metrics_equal_the_v06_index_of_the_selected_notes_alone() {
    let notes = noise(900, 11);
    let mut d = doc(&notes);
    select(&mut d, |i, r| i % 2 == 1 || r.attrs.pitch() > 60);
    let mut sel = Vec::new();
    d.track(0)
        .unwrap()
        .notes
        .for_each_selected(|n| sel.push(n.record()));
    // The reference: a project holding only the selected notes, analysed by
    // the unchanged v0.6 index (T-SELAN-NPS / PPS).
    let only = doc(&sel);
    let reference = AnalysisIndex::for_document(&only);
    let timing = Timing::for_document(&d);
    let a = SelectionAnalysis::build(&input(&d, None), &timing, None).unwrap();
    for tick in [0, 1, 959, 960, 4_000, 9_999, 15_000, 21_499, 40_000] {
        for w in [1_000_000_000, 250_000_000] {
            let got = a.at(&timing, tick, w);
            let want = reference.metrics(&Scope::Project, &timing, tick, w);
            assert_eq!(got.notes_passed, want.notes_passed, "{tick}");
            assert_eq!(got.poly, want.poly, "{tick}");
            assert_eq!(got.nps, want.nps, "{tick}");
            assert!((got.pps - want.pps).abs() < 1e-9, "{tick}");
            assert_eq!(got.total_notes, sel.len() as u64);
        }
    }
}

#[test]
fn peak_poly_counts_instances_beyond_128_and_unique_pitches_separately() {
    // 300 stacked copies of one pitch plus a 3-note chord elsewhere.
    let mut notes: Vec<NoteRecord> = (0..300).map(|_| rec(0, 960, 60)).collect();
    notes.extend([rec(2000, 100, 60), rec(2000, 100, 64), rec(2000, 100, 67)]);
    let mut d = doc(&notes);
    d.track_mut(0).unwrap().notes.select_all();
    let timing = Timing::for_document(&d);
    let a = SelectionAnalysis::build(&input(&d, None), &timing, None).unwrap();
    assert_eq!(a.peak_poly, 300);
    assert_eq!(a.peak_poly_tick, 0);
    assert_eq!(a.peak_unique, 3);
    assert_eq!(a.peak_unique_tick, 2000);
    assert_eq!(a.area_ticks, 300 * 960 + 300);
    // Average NPS: 303 notes over 2,100 ticks = 1.09375 s.
    assert!((a.avg_nps() - 303.0 / 1.093_75).abs() < 1e-6);
}

#[test]
fn touching_notes_and_zero_length_notes_follow_the_half_open_rule() {
    // [0,100) and [100,200) never overlap; a zero-length note never sounds.
    let notes = [rec(0, 100, 60), rec(100, 100, 60), rec(150, 0, 62)];
    let mut d = doc(&notes);
    d.track_mut(0).unwrap().notes.select_all();
    let timing = Timing::for_document(&d);
    let a = SelectionAnalysis::build(&input(&d, None), &timing, None).unwrap();
    assert_eq!(a.counted, 3);
    assert_eq!(a.peak_poly, 1);
    assert_eq!(a.peak_unique, 1);
    assert_eq!(a.area_ticks, 200);
}

#[test]
fn muted_and_corrected_notes_count_as_they_play() {
    let mut notes = vec![rec(0, 100, 60), rec(500, 100, 70), rec(1000, 100, 80)];
    notes[0].attrs = notes[0].attrs.with_muted(true);
    let mut d = doc(&notes);
    d.track_mut(0).unwrap().notes.select_all();
    // From tick 400: +12 keys, range up to 85 → the note at 80 (→ 92) drops.
    let c = Correction {
        from_tick: 400,
        ticks: 960,
        keys: 12,
        gate: GatePercent::IDENTITY,
        range: (0, 85),
        top: 127,
    };
    let timing = Timing::for_document(&d);
    let a = SelectionAnalysis::build(&input(&d, Some(c)), &timing, None).unwrap();
    assert_eq!(a.selected, 3);
    assert_eq!(a.counted, 1, "muted and dropped notes are not counted");
    assert_eq!(a.first_tick, 1460, "shifted by the correction");
    assert_eq!(a.area_ticks, 100);
}

#[test]
fn nothing_selected_gives_empty_totals_and_cancel_stops_the_build() {
    let notes = noise(100, 3);
    let d = doc(&notes);
    let timing = Timing::for_document(&d);
    let a = SelectionAnalysis::build(&input(&d, None), &timing, None).unwrap();
    assert_eq!((a.selected, a.counted, a.peak_poly), (0, 0, 0));
    assert_eq!(a.at(&timing, 500, 1_000_000_000).nps, 0);
    let mut d2 = doc(&noise(200_000, 5));
    d2.track_mut(0).unwrap().notes.select_all();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    assert!(SelectionAnalysis::build(&input(&d2, None), &timing, Some(&cancel)).is_none());
}

/// Measurement (release): `cargo test --release -p plumspirit_engine --test
/// v07_selection_analytics selection_analytics_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn selection_analytics_cost() {
    for n in [100_000usize, 1_000_000, 4_000_000] {
        let notes = {
            let mut v: Vec<NoteRecord> = (0..n as u64)
                .map(|i| {
                    rec(
                        i * 3 + (i * 7919) % 5,
                        200 + (i * 131) % 3000,
                        (i * 37 % 128) as u8,
                    )
                })
                .collect();
            v.sort_by_key(|r| r.start_tick);
            v
        };
        let mut d = doc(&notes);
        d.track_mut(0).unwrap().notes.select_all();
        let timing = Timing::for_document(&d);
        let inp = input(&d, None);
        let t0 = std::time::Instant::now();
        let a = SelectionAnalysis::build(&inp, &timing, None).unwrap();
        let build = t0.elapsed();
        let t1 = std::time::Instant::now();
        let mut s = 0u64;
        for k in 0..10_000u64 {
            s += a
                .at(&timing, (k * 7_777_777) % (n as u64 * 3), 1_000_000_000)
                .nps;
        }
        let q = t1.elapsed() / 10_000;
        println!(
            "selection analytics: {n} selected notes — build {build:?}, {:.2} B/note index, query {q:?} (peak poly {}, unique {}, checksum {s})",
            a.heap_bytes() as f64 / n as f64,
            a.peak_poly,
            a.peak_unique
        );
    }
}
