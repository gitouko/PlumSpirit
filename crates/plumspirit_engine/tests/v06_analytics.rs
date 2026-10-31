//! PlumSpirit v0.6: seek-aware analytics (notes passed, poly, NPS, PPS) and the
//! sounding-note queries behind the live keyboard colours.
//!
//! Every fast query is compared with a brute-force reference that walks every
//! note as it plays (tests only).

use plumspirit_engine::analysis::profile::{PeakProfile, window_peak_poly};
use plumspirit_engine::analysis::{
    AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing, TrackInput, document_corrections,
    for_each_sounding, for_each_starting,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::correction::Corrected;
use plumspirit_engine::model::track::{GatePercent, TrackProps};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId, TempoValue,
};
use std::collections::BTreeSet;

fn bpm(s: &str) -> TempoValue {
    TempoValue::Bpm(Bpm::parse(s).unwrap())
}

fn doc_with(ppqn: u32, tracks: &[Vec<NoteRecord>]) -> PlumDocument {
    let mut d = PlumDocument::new_project("v0.6".into(), Bpm::parse("120").unwrap(), ppqn);
    for (i, notes) in tracks.iter().enumerate() {
        if i > 0 {
            d.add_track().unwrap();
        }
        let mut v = notes.clone();
        v.sort_by_key(|n| n.start_tick);
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, false)
            .unwrap();
    }
    d
}

fn n(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// Every note of the project as it plays (brute force).
fn played(doc: &PlumDocument) -> Vec<NoteRecord> {
    played_in(doc, &Scope::Project)
}

/// Every note of the scope's tracks as it plays (brute force).
fn played_in(doc: &PlumDocument, scope: &Scope) -> Vec<NoteRecord> {
    let corr = document_corrections(doc);
    let mut out = Vec::new();
    for (t, c) in doc.tracks().iter().zip(&corr) {
        if !scope.includes(t.id()) {
            continue;
        }
        for v in t.notes.iter() {
            let r = v.record();
            match c {
                None => out.push(r),
                Some(c) => {
                    if let Corrected::Note { note, .. } = c.note(r) {
                        out.push(note)
                    }
                }
            }
        }
    }
    out
}

struct Brute {
    notes: Vec<NoteRecord>,
    timing: Timing,
}

impl Brute {
    fn scoped(doc: &PlumDocument, scope: &Scope) -> Brute {
        Brute {
            notes: played_in(doc, scope),
            timing: Timing::for_document(doc),
        }
    }
    fn passed(&self, t: u64) -> u64 {
        self.notes.iter().filter(|n| n.start_tick <= t).count() as u64
    }
    fn poly(&self, t: u64) -> u64 {
        self.notes
            .iter()
            .filter(|n| n.start_tick <= t && t < n.start_tick + n.duration_ticks)
            .count() as u64
    }
    fn nps(&self, t: u64, w: u64) -> u64 {
        let now = self.timing.nanos_at(t);
        self.notes
            .iter()
            .filter(|n| {
                n.start_tick <= t && {
                    let x = self.timing.nanos_at(n.start_tick);
                    now < w || x > now - w
                }
            })
            .count() as u64
    }
    /// Exact sweep of poly over time (f64 seconds from the tempo map).
    fn pps(&self, t: u64, w: u64) -> f64 {
        let map = self.timing.map();
        let now = self.timing.nanos_at(t);
        let wb = map.frames_at_tick_f64(t);
        let wa = if now >= w { wb - w as f64 } else { 0.0 };
        if wb - wa <= 0.0 {
            return self.poly(t) as f64;
        }
        let mut area = 0.0;
        for nt in &self.notes {
            if nt.start_tick > t || nt.duration_ticks == 0 {
                continue;
            }
            let s = map.frames_at_tick_f64(nt.start_tick);
            let e = map.frames_at_tick_f64(nt.start_tick + nt.duration_ticks);
            let (a, b) = (s.max(wa), e.min(wb));
            if b > a {
                area += b - a;
            }
        }
        area / (wb - wa)
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1.0)
}

fn check_all(doc: &PlumDocument, ticks: &[u64], w: u64) {
    check_scope(
        doc,
        &AnalysisIndex::for_document(doc),
        &Scope::Project,
        ticks,
        w,
    );
}

fn check_scope(doc: &PlumDocument, idx: &AnalysisIndex, scope: &Scope, ticks: &[u64], w: u64) {
    let timing = Timing::for_document(doc);
    let brute = Brute::scoped(doc, scope);
    assert_eq!(idx.notes_in(scope), brute.notes.len() as u64);
    if *scope == Scope::Project {
        assert_eq!(idx.total_notes(), brute.notes.len() as u64);
    }
    for &t in ticks {
        let m = idx.metrics(scope, &timing, t, w);
        assert_eq!(m.notes_passed, brute.passed(t), "notes at {t}");
        assert_eq!(m.poly, brute.poly(t), "poly at {t}");
        assert_eq!(m.nps, brute.nps(t, w), "nps at {t}");
        let want = brute.pps(t, w);
        assert!(close(m.pps, want), "pps at {t}: {} vs {want}", m.pps);
    }
}

#[test]
fn golden_project_has_hand_computed_values() {
    // 120 BPM, 960 PPQN: 960 ticks = 0.5 s.
    // A 0.00–0.50 s, B 0.25–1.00 s, C 0.75–1.25 s.
    let d = doc_with(
        960,
        &[vec![n(0, 960, 60), n(480, 1440, 62), n(1440, 960, 64)]],
    );
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let m = |t| idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
    let expect = |t: u64, notes: u64, poly: u64, nps: u64, pps: f64| {
        let got = m(t);
        assert_eq!(
            (got.notes_passed, got.total_notes, got.poly, got.nps),
            (notes, 3, poly, nps),
            "at tick {t}"
        );
        assert!(close(got.pps, pps), "pps at {t}: {} vs {pps}", got.pps);
    };
    expect(0, 1, 1, 1, 1.0); // empty window: PPS = poly at the position
    expect(480, 2, 2, 2, 1.0); // [0, .25): 1
    expect(1440, 3, 2, 3, 1.0 / 0.75); // .25 + .5 + .25 over .75 s
    expect(1920, 3, 1, 2, 1.5); // A at 0 s is outside (0, 1]
    expect(2400, 3, 0, 1, 1.5); // B at .25 s is outside (.25, 1.25]
    expect(10_000, 3, 0, 0, 0.0);
}

#[test]
fn pps_is_the_time_weighted_average_and_uses_the_available_window() {
    // 0.0–0.5 s: poly 2; 0.5–1.0 s: poly 6 → 4.0 over the first second.
    let mut notes = vec![n(0, 1920, 60), n(0, 1920, 61)];
    for p in 0..4 {
        notes.push(n(960, 960, 62 + p));
    }
    let d = doc_with(960, &[notes]);
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let at = |t| idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
    assert!(close(at(1920).pps, 4.0), "{}", at(1920).pps);
    // At 0.25 s only 0.25 s exist: poly 2 there → 2.0, not 0.5.
    assert!(close(at(480).pps, 2.0), "{}", at(480).pps);
    // 4 notes of poly 4 for the first 0.25 s → 4.0 at 0.25 s.
    let d = doc_with(960, &[(0..4).map(|p| n(0, 960, 60 + p)).collect()]);
    let idx = AnalysisIndex::for_document(&d);
    let m = idx.metrics(
        &Scope::Project,
        &Timing::for_document(&d),
        480,
        DEFAULT_WINDOW_NS,
    );
    assert!(close(m.pps, 4.0));
}

#[test]
fn identical_pitches_count_separately_and_zero_length_notes_are_onsets_only() {
    let mut notes: Vec<NoteRecord> = (0..100).map(|_| n(100, 500, 72)).collect();
    notes.push(n(200, 0, 40)); // zero length
    let d = doc_with(960, &[notes]);
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let m = idx.metrics(&Scope::Project, &timing, 200, DEFAULT_WINDOW_NS);
    assert_eq!(m.poly, 100);
    assert_eq!(m.notes_passed, 101);
    assert_eq!(m.nps, 101);
    let mut sounding = 0;
    for_each_sounding(&d.track(0).unwrap().notes, None, 200, |r| {
        assert_eq!(r.attrs.pitch(), 72);
        sounding += 1;
    });
    assert_eq!(sounding, 100);
    // Half-open boundaries: active at start, not at end.
    assert_eq!(idx.poly(&Scope::Project, 99), 0);
    assert_eq!(idx.poly(&Scope::Project, 100), 100);
    assert_eq!(idx.poly(&Scope::Project, 599), 100);
    assert_eq!(idx.poly(&Scope::Project, 600), 0);
    check_all(
        &d,
        &[0, 99, 100, 199, 200, 201, 599, 600, 601],
        DEFAULT_WINDOW_NS,
    );
}

#[test]
fn nps_follows_the_tempo_map_not_tick_distance() {
    // A note every 96 ticks. 120 BPM: 96 ticks = 50 ms → 20 notes per second.
    // From tick 9600 on, 240 BPM: 96 ticks = 25 ms → 40 per second.
    let notes: Vec<NoteRecord> = (0..400).map(|i| n(i * 96, 48, 60)).collect();
    let mut d = doc_with(960, &[notes]);
    let mut h = History::default();
    let edit = d
        .conductor()
        .prepare_set_tempo(None, 9600, bpm("240"), Interp::Step)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Tempo",
        },
    )
    .unwrap();
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let at = |t| {
        idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS)
            .nps
    };
    assert_eq!(at(96 * 50), 20);
    assert_eq!(at(9600 + 96 * 60), 40);
    check_all(
        &d,
        &[0, 95, 96, 4800, 9599, 9600, 9601, 12_000, 30_000, 38_400],
        DEFAULT_WINDOW_NS,
    );
}

#[test]
fn linear_bpm_ramps_match_the_reference_conversion() {
    let notes: Vec<NoteRecord> = (0..2000)
        .map(|i| n(i * 37, 100 + (i % 7) * 50, 60))
        .collect();
    let mut d = doc_with(480, &[notes]);
    let mut h = History::default();
    for (tick, v, interp) in [
        (0u64, bpm("90"), Interp::Linear),
        (20_000, bpm("300"), Interp::Linear),
        (50_000, bpm("60"), Interp::Step),
    ] {
        let edit = d
            .conductor()
            .prepare_set_tempo(None, tick, v, interp)
            .unwrap();
        h.apply(
            &mut d,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        )
        .unwrap();
    }
    let ticks: Vec<u64> = (0..80).map(|i| i * 997).collect();
    check_all(&d, &ticks, DEFAULT_WINDOW_NS);
    check_all(&d, &ticks, 250_000_000);
}

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Random tracks (some with corrections, one subtrack), random notes.
fn random_doc(seed: u64, tracks: usize, notes: usize) -> PlumDocument {
    let mut s = seed | 1;
    let mut all = Vec::new();
    for _ in 0..tracks {
        let mut v = Vec::new();
        for _ in 0..notes {
            let start = rng(&mut s) % 40_000;
            let dur = match rng(&mut s) % 10 {
                0 => 0,
                1 => rng(&mut s) % 20_000,
                _ => rng(&mut s) % 400,
            };
            v.push(n(start, dur, (rng(&mut s) % 100) as u8 + 10));
        }
        all.push(v);
    }
    let mut d = doc_with(480, &all);
    let mut h = History::default();
    // A subtrack under track 0 with notes of its own.
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    let sub = (0..d.track_count())
        .find(|&i| d.track(i).unwrap().parent().is_some())
        .unwrap();
    let mut v: Vec<NoteRecord> = (0..notes)
        .map(|_| n(rng(&mut s) % 40_000, rng(&mut s) % 300, 50))
        .collect();
    v.sort_by_key(|x| x.start_tick);
    d.track_mut(sub)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, false)
        .unwrap();
    // Corrections on track 1 (shift back, gate, key range) and 2 (shift on).
    for (ti, props) in [
        (
            1usize,
            TrackProps {
                tick_correction: -700,
                correction_from_measure: 3,
                key_correction: 5,
                gate: GatePercent::parse("150").unwrap(),
                key_range: (20, 90),
                ..TrackProps::default()
            },
        ),
        (
            2,
            TrackProps {
                tick_correction: 333,
                gate: GatePercent::parse("40").unwrap(),
                ..TrackProps::default()
            },
        ),
    ] {
        if ti < tracks {
            let id = d.track(ti).unwrap().id();
            h.apply(
                &mut d,
                Operation::TrackProps {
                    track: id,
                    props,
                    end: None,
                    label: "Track Properties",
                },
            )
            .unwrap();
        }
    }
    let edit = d
        .conductor()
        .prepare_set_tempo(None, 15_000, bpm("173.25"), Interp::Linear)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Tempo",
        },
    )
    .unwrap();
    let edit = d
        .conductor()
        .prepare_set_tempo(None, 25_000, bpm("61"), Interp::Step)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Tempo",
        },
    )
    .unwrap();
    d
}

#[test]
fn random_positions_match_a_brute_force_reference() {
    for seed in [1u64, 7, 42, 99] {
        let d = random_doc(seed, 4, 1500);
        let mut s = seed * 31 + 5;
        let mut ticks: Vec<u64> = (0..300).map(|_| rng(&mut s) % 62_000).collect();
        ticks.extend([0, 1, 14_999, 15_000, 15_001, 24_999, 25_000, 25_001, 61_999]);
        check_all(&d, &ticks, DEFAULT_WINDOW_NS);
        check_all(&d, &ticks[..60], 333_333_333);
    }
}

#[test]
fn sounding_and_starting_queries_match_the_notes_as_they_play() {
    let d = random_doc(5, 4, 2000);
    let corr = document_corrections(&d);
    let mut s = 77u64;
    for _ in 0..200 {
        let t = rng(&mut s) % 60_000;
        let lo = t.saturating_sub(rng(&mut s) % 3000);
        for (ti, track) in d.tracks().iter().enumerate() {
            let c = corr[ti].as_ref();
            let mut want: Vec<NoteRecord> = Vec::new();
            let mut want_start: Vec<NoteRecord> = Vec::new();
            for v in track.notes.iter() {
                let r = match c {
                    None => v.record(),
                    Some(c) => match c.note(v.record()) {
                        Corrected::Note { note, .. } => note,
                        _ => continue,
                    },
                };
                if r.start_tick <= t && t < r.start_tick + r.duration_ticks {
                    want.push(r);
                }
                if r.start_tick > lo && r.start_tick <= t {
                    want_start.push(r);
                }
            }
            let mut got = Vec::new();
            for_each_sounding(&track.notes, c, t, |r| got.push(r));
            let key = |r: &NoteRecord| (r.start_tick, r.duration_ticks, r.attrs.raw(), r.sub.0);
            want.sort_by_key(key);
            got.sort_by_key(key);
            assert_eq!(got, want, "track {ti} at {t}");
            let mut got = Vec::new();
            for_each_starting(&track.notes, c, Some(lo), t, |r| got.push(r));
            want_start.sort_by_key(key);
            got.sort_by_key(key);
            assert_eq!(got, want_start, "track {ti} in ({lo}, {t}]");
        }
    }
}

#[test]
fn scopes_select_tracks_and_subtracks() {
    let d = random_doc(11, 3, 500);
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let ids: Vec<_> = d.tracks().iter().map(|t| t.id()).collect();
    let mut sum_notes = 0;
    let mut sum_poly = 0;
    for id in &ids {
        let scope = Scope::Tracks(BTreeSet::from([*id]));
        let m = idx.metrics(&scope, &timing, 20_000, DEFAULT_WINDOW_NS);
        sum_notes += m.total_notes;
        sum_poly += m.poly;
    }
    let all = idx.metrics(&Scope::Project, &timing, 20_000, DEFAULT_WINDOW_NS);
    assert_eq!(sum_notes, all.total_notes, "every note counted once");
    assert_eq!(sum_poly, all.poly);
    assert_eq!(all.total_notes, played(&d).len() as u64);
}

#[test]
fn edits_update_the_index_and_undo_restores_exact_values() {
    let mut d = random_doc(3, 3, 3000);
    let timing = Timing::for_document(&d);
    let idx0 = AnalysisIndex::for_document(&d);
    let probe =
        |idx: &AnalysisIndex| idx.metrics(&Scope::Project, &timing, 20_000, DEFAULT_WINDOW_NS);
    let before = probe(&idx0);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::InsertNote {
            track: 0.into(),
            record: n(19_990, 100, 60),
        },
    )
    .unwrap();
    let corr = document_corrections(&d);
    assert!(!idx0.describes(&d, &corr));
    assert!(idx0.changed_chunks(&d, &corr) <= 2);
    let idx1 = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx0), None).unwrap();
    assert!(idx1.describes(&d, &corr));
    assert!(idx1.stats.pieces_built <= 2, "{:?}", idx1.stats);
    let after = probe(&idx1);
    assert_eq!(after.total_notes, before.total_notes + 1);
    assert_eq!(after.notes_passed, before.notes_passed + 1);
    assert_eq!(after.poly, before.poly + 1);
    assert_eq!(after.nps, before.nps + 1);
    assert_eq!(probe(&idx1), probe(&AnalysisIndex::for_document(&d)));
    h.undo(&mut d).unwrap();
    let idx2 = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx1), None).unwrap();
    assert_eq!(probe(&idx2), before);
    h.redo(&mut d).unwrap();
    let idx3 = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx2), None).unwrap();
    assert_eq!(probe(&idx3), after);
}

#[test]
fn velocity_edits_reuse_every_piece_and_tempo_edits_need_no_rebuild() {
    let mut d = random_doc(9, 2, 20_000);
    let idx0 = AnalysisIndex::for_document(&d);
    let mut h = History::default();
    d.track_mut(0).unwrap().notes.select_all();
    h.apply(
        &mut d,
        Operation::SetVelocity {
            track: 0.into(),
            velocity: 3,
        },
    )
    .unwrap();
    let corr = document_corrections(&d);
    assert!(!idx0.describes(&d, &corr), "chunks were rewritten");
    let idx1 = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx0), None).unwrap();
    assert_eq!(idx1.stats.pieces_built, 0, "{:?}", idx1.stats);
    // Tempo: notes unchanged, so the index still describes the document; only
    // the time mapping (and with it NPS / PPS) changes.
    let t0 = Timing::for_document(&d);
    let m0 = idx1.metrics(&Scope::Project, &t0, 10_000, DEFAULT_WINDOW_NS);
    let edit = d
        .conductor()
        .prepare_set_tempo(None, 0, bpm("60"), Interp::Step)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Tempo",
        },
    )
    .unwrap();
    let corr = document_corrections(&d);
    assert!(idx1.describes(&d, &corr));
    let t1 = Timing::for_document(&d);
    let m1 = idx1.metrics(&Scope::Project, &t1, 10_000, DEFAULT_WINDOW_NS);
    assert_eq!(m1.notes_passed, m0.notes_passed);
    assert_eq!(m1.poly, m0.poly);
    assert_ne!(m1.nps, m0.nps, "slower tempo: a second holds fewer ticks");
    check_all(&d, &[0, 1000, 30_000, 45_000], DEFAULT_WINDOW_NS);
}

#[test]
fn analytics_memory_is_small_per_note() {
    let notes: Vec<NoteRecord> = (0..200_000u64).map(|i| n(i * 3, 40 + i % 90, 60)).collect();
    let d = doc_with(960, &[notes]);
    let idx = AnalysisIndex::for_document(&d);
    let per_note = idx.heap_bytes() as f64 / 200_000.0;
    eprintln!("distinct ticks: {per_note:.2} bytes per note");
    assert!(per_note < 4.0, "{per_note:.2} bytes per note");
    // Black MIDI chords (16 notes per tick, shared lengths) cost far less.
    let chords: Vec<NoteRecord> = (0..200_000u64)
        .map(|i| n((i / 16) * 120, 60 + (i / 16) % 4 * 30, (40 + i % 16) as u8))
        .collect();
    let d = doc_with(960, &[chords]);
    let idx = AnalysisIndex::for_document(&d);
    let per_note = idx.heap_bytes() as f64 / 200_000.0;
    eprintln!("chords: {per_note:.2} bytes per note");
    assert!(per_note < 1.0, "{per_note:.2} bytes per note");
}

/// Scopes covering every query plan: the whole project, a sum over a few
/// tracks, and the project minus a few excluded tracks.
fn plans(d: &PlumDocument) -> Vec<Scope> {
    let ids: Vec<_> = d.tracks().iter().map(|t| t.id()).collect();
    vec![
        Scope::Project,
        Scope::Tracks(BTreeSet::from([ids[1]])),
        Scope::Tracks(ids.iter().copied().skip(1).collect()),
        Scope::Tracks(ids.iter().copied().collect()),
    ]
}

#[test]
fn every_scope_plan_matches_a_brute_force_reference() {
    for seed in [4u64, 21] {
        let d = random_doc(seed, 5, 900);
        let idx = AnalysisIndex::for_document(&d);
        let mut s = seed * 7 + 3;
        let mut ticks: Vec<u64> = (0..120).map(|_| rng(&mut s) % 62_000).collect();
        ticks.extend([0, 1, 15_000, 15_001, 25_000, 25_001]);
        for scope in plans(&d) {
            check_scope(&d, &idx, &scope, &ticks, DEFAULT_WINDOW_NS);
            check_scope(&d, &idx, &scope, &ticks[..30], 250_000_000);
        }
    }
}

#[test]
fn peaks_match_a_brute_force_sweep() {
    for (seed, scope_i) in [(2u64, 0usize), (13, 0), (5, 1), (6, 2)] {
        let d = random_doc(seed, 3, 1200);
        let scope = plans(&d)[scope_i].clone();
        let scope = &scope;
        let idx = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        let brute = Brute::scoped(&d, scope);
        let prof = PeakProfile::build(&idx, scope, &timing, DEFAULT_WINDOW_NS, None).unwrap();
        // Brute force, once: poly after every event tick (sweep) and NPS at
        // every onset tick.
        let mut events: Vec<(u64, i64)> = Vec::new();
        for n in &brute.notes {
            events.push((n.start_tick, 1));
            events.push((n.start_tick + n.duration_ticks, -1));
        }
        events.sort_unstable();
        let mut poly_at: Vec<(u64, u64)> = Vec::new();
        let mut p = 0i64;
        let mut i = 0;
        while i < events.len() {
            let t = events[i].0;
            while i < events.len() && events[i].0 == t {
                p += events[i].1;
                i += 1;
            }
            poly_at.push((t, p as u64));
        }
        let mut onsets: Vec<u64> = brute.notes.iter().map(|n| n.start_tick).collect();
        onsets.sort_unstable();
        let mut nps_at: Vec<(u64, u64)> = Vec::new();
        for (k, &x) in onsets.iter().enumerate() {
            if onsets.get(k + 1) == Some(&x) {
                continue;
            }
            nps_at.push((x, brute.nps(x, DEFAULT_WINDOW_NS)));
        }
        let poly_of = |t: u64| -> u64 {
            let j = poly_at.partition_point(|&(x, _)| x <= t);
            if j == 0 { 0 } else { poly_at[j - 1].1 }
        };
        let mut s = seed + 100;
        for _ in 0..80 {
            let t = rng(&mut s) % 62_000;
            let want_poly = poly_at
                .iter()
                .filter(|&&(x, _)| x <= t)
                .map(|&(_, v)| v)
                .max()
                .unwrap_or(0);
            assert_eq!(
                prof.peak_poly_so_far(&idx, scope, t),
                want_poly,
                "poly at {t}"
            );
            let want_nps = nps_at
                .iter()
                .filter(|&&(x, _)| x <= t)
                .map(|&(_, v)| v)
                .max()
                .unwrap_or(0);
            assert_eq!(
                prof.peak_nps_so_far(&idx, scope, &timing, t),
                want_nps,
                "nps at {t}"
            );
            let lo = t.checked_sub(rng(&mut s) % 9000);
            let want = poly_at
                .iter()
                .filter(|&&(x, _)| lo.is_none_or(|l| x > l) && x <= t)
                .map(|&(_, v)| v)
                .chain(lo.map(poly_of))
                .max()
                .unwrap_or(0);
            assert_eq!(
                window_peak_poly(&idx, scope, lo, t),
                want,
                "window ({lo:?}, {t}]"
            );
            assert_eq!(
                prof.peak_poly_between(&idx, scope, lo, t),
                want,
                "profile window ({lo:?}, {t}]"
            );
        }
    }
}

#[test]
fn random_edits_keep_the_incremental_index_equal_to_a_fresh_one() {
    use plumspirit_engine::editor::{self, EditControl};
    let mut d = random_doc(17, 4, 2500);
    let mut h = History::default();
    let mut idx = AnalysisIndex::for_document(&d);
    let mut s = 1234u64;
    let probes: Vec<u64> = (0..60).map(|_| rng(&mut s) % 64_000).collect();
    let timing = Timing::for_document(&d);
    let same = |a: &AnalysisIndex, b: &AnalysisIndex, step: usize, scopes: &[Scope]| {
        for &t in &probes {
            for scope in scopes {
                let (x, y) = (
                    a.metrics(scope, &timing, t, DEFAULT_WINDOW_NS),
                    b.metrics(scope, &timing, t, DEFAULT_WINDOW_NS),
                );
                assert_eq!(x.notes_passed, y.notes_passed, "step {step} t {t}");
                assert_eq!(x.poly, y.poly, "step {step} t {t}");
                assert_eq!(x.nps, y.nps, "step {step} t {t}");
                assert_eq!(x.total_notes, y.total_notes, "step {step}");
                assert!(close(x.pps, y.pps), "step {step} t {t}");
            }
        }
    };
    for step in 0..40 {
        let tracks = d.track_count();
        let ti = (rng(&mut s) as usize) % tracks;
        let kind = rng(&mut s) % 9;
        let t0 = rng(&mut s) % 40_000;
        let result = match kind {
            0 | 1 => h
                .apply(
                    &mut d,
                    Operation::InsertNote {
                        track: ti.into(),
                        record: n(t0, rng(&mut s) % 900, 60),
                    },
                )
                .map(|_| ()),
            2 => {
                d.track_mut(ti)
                    .unwrap()
                    .notes
                    .select_in_region(t0, t0 + 500, 0, 127);
                h.apply(&mut d, Operation::DeleteSelected { track: ti.into() })
                    .map(|_| ())
            }
            3 => {
                d.track_mut(ti)
                    .unwrap()
                    .notes
                    .select_in_region(t0, t0 + 2000, 0, 127);
                match editor::prepare_move(
                    &d,
                    ti,
                    (rng(&mut s) % 3000) as i128 - 1500,
                    1,
                    EditControl::default(),
                ) {
                    Ok(p) => h
                        .apply(
                            &mut d,
                            Operation::Prepared {
                                edit: p,
                                label: "Move",
                            },
                        )
                        .map(|_| ()),
                    Err(_) => Ok(()),
                }
            }
            4 => {
                d.track_mut(ti)
                    .unwrap()
                    .notes
                    .select_in_region(t0, t0 + 3000, 0, 127);
                h.apply(
                    &mut d,
                    Operation::Resize {
                        track: ti.into(),
                        delta: 37,
                    },
                )
                .map(|_| ())
            }
            5 => {
                d.track_mut(ti).unwrap().notes.select_all();
                h.apply(
                    &mut d,
                    Operation::SetVelocity {
                        track: ti.into(),
                        velocity: 9,
                    },
                )
                .map(|_| ())
            }
            6 => {
                let id = d.track(ti).unwrap().id();
                let props = TrackProps {
                    tick_correction: (rng(&mut s) % 900) as i64 - 450,
                    gate: GatePercent::parse("80").unwrap(),
                    ..TrackProps::default()
                };
                h.apply(
                    &mut d,
                    Operation::TrackProps {
                        track: id,
                        props,
                        end: None,
                        label: "Props",
                    },
                )
                .map(|_| ())
            }
            7 => h.undo(&mut d).map(|_| ()),
            _ => {
                if tracks > 3
                    && d.track(ti).unwrap().parent().is_none()
                    && d.subtree_end(ti) == ti + 1
                {
                    h.apply(&mut d, Operation::DeleteTrack { index: ti })
                        .map(|_| ())
                } else {
                    h.apply(&mut d, Operation::AddTrack).map(|_| ())
                }
            }
        };
        if result.is_err() {
            continue;
        }
        for i in 0..d.track_count() {
            d.track_mut(i).unwrap().notes.clear_selection();
        }
        let next = AnalysisIndex::build(&TrackInput::all(&d), Some(&idx), None).unwrap();
        let fresh = AnalysisIndex::for_document(&d);
        same(&next, &fresh, step, &plans(&d));
        assert_eq!(next.total_notes(), played(&d).len() as u64);
        if kind == 0 || kind == 5 {
            assert!(
                !next.stats.timeline_full,
                "small edit used a delta: {:?}",
                next.stats
            );
        }
        idx = next;
    }
}
