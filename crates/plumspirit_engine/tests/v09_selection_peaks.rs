//! V09-S19 (V09-0024, `docs/SELECTION_PEAKS.md`): the peak profile's exact
//! Peak PPS — the highest PPS at **any** tick and the first tick reaching it
//! — against a brute force over every tick: an exact integer evaluation of
//! the canonical definition from the notes, and `AnalysisIndex::metrics`'s
//! own f64 PPS. Every block holds the maximum over its own tick range, so a
//! ranged update (V09-0056) keeps blocks that stay valid; updated profiles
//! answer exactly like fresh builds after chained edits.

mod common;

use common::corpus::{corpus, note};
use plumspirit_engine::analysis::canonical::window_lo;
use plumspirit_engine::analysis::profile::{PeakProfile, Peaks, Ratio};
use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput, gate_limits};
use plumspirit_engine::analysis::{AnalysisIndex, Scope, Timing};
use plumspirit_engine::fixtures::{FixtureShape, generate, set_dense_tempo};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::correction::{Corrected, Correction};
use plumspirit_engine::model::project::Bpm;
use plumspirit_engine::model::track::GatePercent;

const SECOND: u64 = 1_000_000_000;

fn rng(seed: u64) -> impl FnMut(u64) -> u64 {
    let mut x = seed | 1;
    move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n.max(1)
    }
}

/// The canonical PPS evaluated tick by tick from the notes' ticks, exactly
/// (frames × 2^32): prefix sums of the event times, no sweep state.
struct Brute {
    starts: Vec<u64>,
    ends: Vec<u64>,
    sfx: Vec<u128>,
    efx: Vec<u128>,
}

impl Brute {
    fn new(timing: &Timing, mut starts: Vec<u64>, mut ends: Vec<u64>) -> Brute {
        starts.sort_unstable();
        ends.sort_unstable();
        let pre = |v: &[u64]| {
            let mut out = vec![0u128];
            for &x in v {
                out.push(out.last().unwrap() + timing.map().fx_at_tick(x));
            }
            out
        };
        Brute {
            sfx: pre(&starts),
            efx: pre(&ends),
            starts,
            ends,
        }
    }

    fn of_index(idx: &AnalysisIndex, timing: &Timing) -> Brute {
        let scope = Scope::Project;
        let last = idx.last_tick(&scope);
        let (mut s, mut e) = (Vec::new(), Vec::new());
        idx.for_each_start_in(&scope, None, last, |x, w| {
            assert_eq!(w, 1);
            s.push(x)
        });
        idx.for_each_end_in(&scope, None, last, |x, w| {
            assert_eq!(w, 1);
            e.push(x)
        });
        Brute::new(timing, s, e)
    }

    fn le(v: &[u64], t: u64) -> usize {
        v.partition_point(|&x| x <= t)
    }

    fn poly(&self, t: u64) -> i128 {
        Self::le(&self.starts, t) as i128 - Self::le(&self.ends, t) as i128
    }

    /// Σ (fx(t) − fx(x)) over `v`'s ticks in `(lo, t]`.
    fn weight(v: &[u64], pre: &[u128], lo: Option<u64>, t: u64, fx: u128) -> i128 {
        let a = lo.map_or(0, |l| Self::le(v, l));
        let b = Self::le(v, t);
        (b - a) as i128 * fx as i128 - (pre[b] - pre[a]) as i128
    }

    fn pps(&self, timing: &Timing, w: u64, t: u64) -> Ratio {
        let fx = timing.map().fx_at_tick(t);
        let ka = window_lo(timing, timing.nanos_at(t), w);
        let inner = Self::weight(&self.starts, &self.sfx, ka, t, fx)
            - Self::weight(&self.ends, &self.efx, ka, t, fx);
        match ka {
            None if fx == 0 => Ratio {
                num: self.poly(t).max(0) as u128,
                den: 1,
            },
            None => Ratio {
                num: inner.max(0) as u128,
                den: fx,
            },
            Some(k) => {
                let wfx = i128::from(w) << 32;
                Ratio {
                    num: (self.poly(k) * wfx + inner).max(0) as u128,
                    den: wfx as u128,
                }
            }
        }
    }

    /// The last tick worth scanning: one window after the last event.
    fn end(&self, timing: &Timing, w: u64) -> u64 {
        let last = self.ends.last().copied().unwrap_or(0);
        timing
            .map()
            .first_tick_at_or_after_frame(timing.nanos_at(last) + w)
            + 2
    }

    /// Highest exact PPS over ticks `a..=b` and the first tick reaching it.
    fn best(&self, timing: &Timing, w: u64, a: u64, b: u64) -> (Ratio, u64) {
        let mut best = (self.pps(timing, w, a), a);
        for t in a + 1..=b {
            let v = self.pps(timing, w, t);
            if v.exceeds(&best.0) {
                best = (v, t);
            }
        }
        best
    }

    fn area_ns(&self, timing: &Timing) -> u128 {
        let n = |v: &[u64]| -> u128 { v.iter().map(|&x| u128::from(timing.nanos_at(x))).sum() };
        n(&self.ends) - n(&self.starts)
    }
}

fn same(a: Ratio, b: Ratio) -> bool {
    !a.exceeds(&b) && !b.exceeds(&a)
}

/// Every check of one profile against the brute force; returns the ticks
/// scanned.
fn check(
    what: &str,
    idx: &AnalysisIndex,
    timing: &Timing,
    prof: &PeakProfile,
    f64_too: bool,
) -> u64 {
    let brute = Brute::of_index(idx, timing);
    let w = prof.window_ns();
    let end = brute.end(timing, w);
    let Some((peak, tick)) = prof.peak_pps() else {
        assert!(brute.starts.is_empty(), "{what}: no peak but notes");
        return 0;
    };
    let (bp, bt) = brute.best(timing, w, 0, end);
    assert!(
        same(peak, bp) && tick == bt,
        "{what}: peak {} @ {tick}, brute force {} @ {bt}",
        peak.to_f64(),
        bp.to_f64()
    );
    // Every block: the maximum over its own range, first tick.
    let blocks = prof.pps_blocks();
    for (j, &(first, v, at)) in blocks.iter().enumerate() {
        let hi = blocks.get(j + 1).map_or(end, |b| b.0 - 1);
        let (bv, bat) = brute.best(timing, w, first, hi);
        assert!(
            same(v, bv) && at == bat,
            "{what}: block {j} [{first}, {hi}]: {} @ {at}, brute force {} @ {bat}",
            v.to_f64(),
            bv.to_f64()
        );
    }
    assert_eq!(
        prof.area_ns(),
        Some(brute.area_ns(timing)),
        "{what}: Poly Area"
    );
    if f64_too {
        // The f64 definition (AnalysisIndex::metrics) agrees to rounding.
        let scope = Scope::Project;
        let mut fmax = 0.0f64;
        for t in 0..=end {
            fmax = fmax.max(idx.metrics(&scope, timing, t, w).pps);
        }
        let p = peak.to_f64();
        let at = idx.metrics(&scope, timing, tick, w).pps;
        assert!(
            (fmax - p).abs() <= 1e-9 * p.max(1.0),
            "{what}: every-tick f64 max {fmax} vs {p}"
        );
        assert!(
            (at - p).abs() <= 1e-9 * p.max(1.0),
            "{what}: metrics at the peak tick {at} vs {p}"
        );
    }
    end + 1
}

fn profile(idx: &AnalysisIndex, timing: &Timing, w: u64) -> PeakProfile {
    PeakProfile::build_with(idx, &Scope::Project, timing, w, Peaks::WithPps, None).unwrap()
}

fn doc_with(ppqn: u32, bpm: &str, notes: &[(u64, u64)]) -> PlumDocument {
    let mut d = PlumDocument::new_project("t".into(), Bpm::parse(bpm).unwrap(), ppqn);
    d.add_track().unwrap();
    let mut recs: Vec<_> = notes
        .iter()
        .enumerate()
        .map(|(k, &(s, len))| note(s, len, (40 + k % 60) as u8, 0))
        .collect();
    recs.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

#[test]
fn peak_pps_equals_every_tick_on_dense_tempo_songs_with_several_windows() {
    let mut ticks = 0;
    for seed in 1..=24u64 {
        let mut d = generate(
            FixtureShape::Dense,
            150 + 40 * seed as usize,
            96,
            seed * 7919,
        );
        set_dense_tempo(&mut d, 10 + seed as usize, 48 + seed * 5, seed);
        let idx = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        for w in [1u64, 37_000_000, 250_000_000, SECOND, 3 * SECOND] {
            let prof = profile(&idx, &timing, w);
            ticks += check(
                &format!("seed {seed} window {w}"),
                &idx,
                &timing,
                &prof,
                w == SECOND,
            );
        }
    }
    assert!(ticks > 100_000, "{ticks}");
}

#[test]
fn peak_pps_of_known_polyphony_transitions() {
    // 120 BPM, 96 PPQN: one tick = 1/192 s. Window 1 s = 192 ticks.
    // Four notes sounding over ticks [0, 384) (two seconds), then one.
    let d = doc_with(
        96,
        "120",
        &[(0, 384), (0, 384), (0, 384), (0, 384), (384, 960)],
    );
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let prof = profile(&idx, &timing, SECOND);
    check("transition", &idx, &timing, &prof, true);
    let (peak, tick) = prof.peak_pps().unwrap();
    // PPS is 4 from tick 0 (time 0: the poly) and stays 4 until the window
    // reaches past tick 384: the first tick reaching it is 0.
    assert_eq!((peak.to_f64(), tick), (4.0, 0));
    // A burst: 10 notes of 1/4 s at 1 s, otherwise silence: the area is
    // 2.5 note·s, so PPS peaks at 2.5 from the burst's end (tick 240) until
    // the window leaves its start.
    let d = doc_with(96, "120", &[(192, 48); 10]);
    let idx = AnalysisIndex::for_document(&d);
    let prof = profile(&idx, &Timing::for_document(&d), SECOND);
    check("burst", &idx, &Timing::for_document(&d), &prof, true);
    let (peak, tick) = prof.peak_pps().unwrap();
    assert_eq!((peak.to_f64(), tick), (2.5, 240));
}

#[test]
fn peak_pps_edge_cases_zero_lengths_tick_zero_short_songs_and_ties() {
    let cases: Vec<(&str, PlumDocument, u64)> = vec![
        (
            "zero lengths only",
            doc_with(96, "120", &[(10, 0), (10, 0), (50, 0)]),
            SECOND,
        ),
        ("one note at tick 0", doc_with(96, "120", &[(0, 1)]), SECOND),
        (
            "shorter than the window",
            doc_with(960, "90", &[(0, 30), (5, 700), (17, 3), (400, 1)]),
            10 * SECOND,
        ),
        (
            "a repeating pattern (ties)",
            doc_with(
                96,
                "120",
                &(0..40u64).map(|k| (k * 48, 24)).collect::<Vec<_>>(),
            ),
            SECOND,
        ),
        (
            "a long note and a stack",
            doc_with(96, "77.25", &[(0, 5_000), (900, 10), (900, 10), (900, 10)]),
            250_000_000,
        ),
        (
            "huge window",
            doc_with(96, "200", &[(3, 40), (100, 400), (1_000, 1)]),
            120 * SECOND,
        ),
        (
            "sub-tick window",
            doc_with(96, "60", &[(3, 40), (4, 1), (40, 2)]),
            1,
        ),
    ];
    for (what, d, w) in cases {
        let idx = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        let prof = profile(&idx, &timing, w);
        // metrics()'s f64 sum loses ~1e-16 of the song time per term: with a
        // sub-tick window that is ~1e-8 of the value, so only the exact check.
        check(what, &idx, &timing, &prof, w >= 1_000_000);
    }
    // No notes: no peak; a profile without PPS answers None.
    let d = doc_with(96, "120", &[]);
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    assert_eq!(profile(&idx, &timing, SECOND).peak_pps(), None);
    let d = doc_with(96, "120", &[(0, 10)]);
    let idx = AnalysisIndex::for_document(&d);
    let p = PeakProfile::build(
        &idx,
        &Scope::Project,
        &Timing::for_document(&d),
        SECOND,
        None,
    )
    .unwrap();
    assert_eq!((p.peak_pps(), p.area_ns()), (None, None));
}

#[test]
fn peak_pps_on_the_corpus_equals_every_tick() {
    let mut checked = 0;
    for (name, d) in corpus() {
        let idx = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        let brute = Brute::of_index(&idx, &timing);
        if brute.end(&timing, SECOND) > 400_000 {
            continue; // the long sparse timelines: covered by the ranged tests
        }
        let prof = profile(&idx, &timing, SECOND);
        check(&name, &idx, &timing, &prof, false);
        checked += 1;
    }
    assert!(checked >= 15, "{checked} corpus projects");
}

#[test]
fn peak_poly_and_peak_nps_first_ticks_match_a_sweep() {
    for seed in 1..=12u64 {
        let mut d = generate(FixtureShape::Dense, 300, 96, seed * 31);
        set_dense_tempo(&mut d, 8, 96, seed);
        let idx = AnalysisIndex::for_document(&d);
        let timing = Timing::for_document(&d);
        let prof = profile(&idx, &timing, SECOND);
        let brute = Brute::of_index(&idx, &timing);
        let end = brute.end(&timing, SECOND);
        let (mut bp, mut bpt, mut bn, mut bnt) = (0i128, 0u64, 0u64, 0u64);
        for t in 0..=end {
            let p = brute.poly(t);
            if p > bp {
                (bp, bpt) = (p, t);
            }
            let lo = window_lo(&timing, timing.nanos_at(t), SECOND);
            let n = (Brute::le(&brute.starts, t) - lo.map_or(0, |k| Brute::le(&brute.starts, k)))
                as u64;
            if n > bn {
                (bn, bnt) = (n, t);
            }
        }
        assert_eq!(
            prof.peak_poly_first(&idx, &Scope::Project),
            (bp as u64, bpt),
            "seed {seed} poly"
        );
        assert_eq!(
            prof.peak_nps_first(&idx, &Scope::Project, &timing),
            (bn, bnt),
            "seed {seed} nps"
        );
    }
}

// ---- the ranged update keeps exact PPS blocks (V09-0056) ----

/// Applies `edit`; the previous profile updated by the index's delta must
/// pass every brute-force check and answer like a fresh build. Returns
/// whether the ranged path was taken.
fn step(
    what: &str,
    d: &mut PlumDocument,
    state: &mut (AnalysisIndex, PeakProfile),
    edit: impl FnOnce(&mut PlumDocument),
) -> bool {
    edit(d);
    let timing = Timing::for_document(d);
    let inputs = plumspirit_engine::analysis::TrackInput::all(d);
    let idx = AnalysisIndex::build(&inputs, Some(&state.0), None).unwrap();
    let w = state.1.window_ns();
    let built = profile(&idx, &timing, w);
    let scope = Scope::Project;
    let (updated, ranged) = match idx.delta_since(&state.0, &scope) {
        Some(delta) => {
            let ranged = !delta.is_empty() && state.1.update_blocks(&delta, &timing) != usize::MAX;
            (
                state.1.update(&idx, &scope, &timing, &delta, None).unwrap(),
                ranged,
            )
        }
        None => (built.clone(), false),
    };
    check(&format!("{what} (updated)"), &idx, &timing, &updated, false);
    assert_eq!(
        updated.peak_pps().map(|p| p.1),
        built.peak_pps().map(|p| p.1),
        "{what}"
    );
    assert!(
        same(updated.peak_pps().unwrap().0, built.peak_pps().unwrap().0),
        "{what}"
    );
    assert_eq!(updated.area_ns(), built.area_ns(), "{what}");
    assert_eq!(updated.peak_poly(), built.peak_poly(), "{what}");
    assert_eq!(updated.peak_nps(), built.peak_nps(), "{what}");
    *state = (idx, updated);
    ranged
}

fn edit_rounds(name: &str, d: &mut PlumDocument, w: u64, seed: u64, rounds: usize) -> usize {
    let timing = Timing::for_document(d);
    let idx = AnalysisIndex::for_document(d);
    let p = profile(&idx, &timing, w);
    let mut state = (idx, p);
    let mut r = rng(seed);
    let mut ranged = 0;
    let tracks = d.track_count();
    for round in 0..rounds {
        let last = state.0.last_tick(&Scope::Project).max(960);
        let t = r(tracks as u64) as usize;
        let what = format!("{name} round {round}");
        let ok = match r(5) {
            0 | 1 => step(&format!("{what}: insert"), d, &mut state, |d| {
                let mut recs: Vec<_> = (0..1 + r(4))
                    .map(|_| note(r(last + 200), r(150), (30 + r(60)) as u8, 0))
                    .collect();
                recs.sort_by_key(|n| n.start_tick);
                d.track_mut(t)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&recs, false)
                    .unwrap();
            }),
            2 => step(&format!("{what}: delete a region"), d, &mut state, |d| {
                let a = r(last);
                let s = &mut d.track_mut(t).unwrap().notes;
                s.clear_selection();
                s.select_in_region(a, a + r(last / 20 + 10), 0, 127);
                s.remove_selected();
            }),
            3 => step(&format!("{what}: durations"), d, &mut state, |d| {
                let a = r(last);
                let s = &mut d.track_mut(t).unwrap().notes;
                s.clear_selection();
                s.select_in_region(a, a + r(last / 30 + 10), 0, 127);
                let _ = s.set_selected_durations(1 + r(300));
                s.clear_selection();
            }),
            _ => step(&format!("{what}: selection only"), d, &mut state, |d| {
                d.track_mut(t).unwrap().notes.select_all();
            }),
        };
        ranged += usize::from(ok);
    }
    ranged
}

#[test]
fn the_ranged_update_keeps_exact_pps_blocks_after_chained_edits() {
    let mut ranged = 0;
    for seed in 1..=10u64 {
        // Long enough for many blocks (256 event ticks each).
        let mut d = generate(FixtureShape::Dense, 6_000, 96, seed * 101);
        set_dense_tempo(&mut d, 12, 400, seed);
        let w = [250_000_000, SECOND, 2 * SECOND][seed as usize % 3];
        ranged += edit_rounds(&format!("dense {seed}"), &mut d, w, seed ^ 0x51, 6);
    }
    assert!(ranged >= 25, "{ranged} ranged updates");
}

#[test]
fn the_ranged_update_is_exact_around_bursts_on_a_long_song() {
    // 960 PPQN at 120 BPM, a note every two ticks on two tracks: about
    // seven blocks per second, so a burst's window reaches several blocks.
    let mut d = PlumDocument::new_project("dense".into(), Bpm::parse("120").unwrap(), 960);
    d.add_track().unwrap();
    d.add_track().unwrap();
    let mut r = rng(77);
    for t in 0..2u64 {
        let mut recs: Vec<_> = (0..6_000u64)
            .map(|k| note(k * 4 + t * 2 + r(2), 1 + r(200), (20 + r(90)) as u8, 0))
            .collect();
        recs.sort_by_key(|n| n.start_tick);
        d.track_mut(t as usize)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let idx = AnalysisIndex::for_document(&d);
    let timing = Timing::for_document(&d);
    let p = profile(&idx, &timing, SECOND);
    let mut state = (idx, p);
    let mut ranged = 0;
    for (what, at, len, n) in [
        ("long burst", 5_000u64, 4_000u64, 300u64),
        ("short burst", 12_001, 7, 200),
        ("burst at the end", 23_990, 3_000, 50),
    ] {
        ranged += usize::from(step(what, &mut d, &mut state, |d| {
            let recs: Vec<_> = (0..n).map(|k| note(at, len, (k % 120) as u8, 0)).collect();
            d.track_mut(1)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }));
    }
    ranged += usize::from(step("long burst removed", &mut d, &mut state, |d| {
        let s = &mut d.track_mut(1).unwrap().notes;
        s.clear_selection();
        s.select_in_region(5_000, 5_000, 0, 127);
        s.remove_selected();
    }));
    ranged += usize::from(step("durations under a region", &mut d, &mut state, |d| {
        let s = &mut d.track_mut(0).unwrap().notes;
        s.clear_selection();
        s.select_in_region(16_000, 16_600, 0, 127);
        let _ = s.set_selected_durations(2_500);
        s.clear_selection();
    }));
    assert!(ranged >= 4, "{ranged}");
}

// ---- Selection Analytics: instant Peak NPS / PPS (V09-0024) ----

/// The track holding the notes (the generated fixtures keep them in track
/// 1, the hand-made songs in track 0).
fn nt(d: &PlumDocument) -> usize {
    (0..d.track_count())
        .find(|&i| !d.track(i).unwrap().notes.is_empty())
        .unwrap_or(0)
}

/// The selected notes of track 0 as they play: (start, end) in song ticks.
fn played(d: &PlumDocument, correction: Option<Correction>, shift: i128) -> (Vec<u64>, Vec<u64>) {
    let (mut s, mut e) = (Vec::new(), Vec::new());
    d.track(nt(d)).unwrap().notes.for_each_selected(|n| {
        let mut r = n.record();
        if r.attrs.muted() {
            return;
        }
        r.start_tick = (i128::from(r.start_tick) + shift).clamp(0, i128::from(u64::MAX)) as u64;
        let r = match correction {
            None => r,
            Some(c) => match c.note(r) {
                Corrected::Note { note, .. } => note,
                _ => return,
            },
        };
        s.push(r.start_tick);
        e.push(r.start_tick + r.duration_ticks);
    });
    (s, e)
}

fn selection(
    d: &PlumDocument,
    correction: Option<Correction>,
    shift: i128,
    w: u64,
) -> SelectionAnalysis {
    let inputs = vec![SelectionInput {
        id: d.track(nt(d)).unwrap().id(),
        notes: d.track(nt(d)).unwrap().notes.clone(),
        correction,
        shift,
    }];
    SelectionAnalysis::build_for(&inputs, &Timing::for_document(d), w, None).unwrap()
}

/// Every peak of `a` against a brute force over every tick of the played
/// selected notes.
fn check_selection(
    what: &str,
    d: &PlumDocument,
    a: &SelectionAnalysis,
    played: (Vec<u64>, Vec<u64>),
) {
    let timing = Timing::for_document(d);
    let w = a.window_ns;
    check_gates(what, d, a, &played);
    let brute = Brute::new(&timing, played.0, played.1);
    assert_eq!(a.counted, brute.starts.len() as u64, "{what}: counted");
    if brute.starts.is_empty() {
        assert_eq!((a.peak_nps, a.peak_pps, a.peak_poly), (0, 0.0, 0), "{what}");
        return;
    }
    let end = brute.end(&timing, w);
    let (bp, bt) = brute.best(&timing, w, 0, end);
    assert_eq!(
        (a.peak_pps, a.peak_pps_tick),
        (bp.to_f64(), bt),
        "{what}: Peak PPS"
    );
    let (mut bn, mut bnt, mut bpoly, mut bpt) = (0u64, 0u64, 0i128, 0u64);
    for t in 0..=end {
        let lo = window_lo(&timing, timing.nanos_at(t), w);
        let n =
            (Brute::le(&brute.starts, t) - lo.map_or(0, |k| Brute::le(&brute.starts, k))) as u64;
        if n > bn {
            (bn, bnt) = (n, t);
        }
        if brute.poly(t) > bpoly {
            (bpoly, bpt) = (brute.poly(t), t);
        }
    }
    assert_eq!((a.peak_nps, a.peak_nps_tick), (bn, bnt), "{what}: Peak NPS");
    assert_eq!(
        (a.peak_poly, a.peak_poly_tick),
        (bpoly as u64, bpt),
        "{what}: Peak Poly"
    );
    assert_eq!(a.area_ns, brute.area_ns(&timing), "{what}: Poly Area");
    assert_eq!(a.first_tick, brute.starts[0], "{what}: first start");
    assert_eq!(a.last_tick, *brute.ends.last().unwrap(), "{what}: last end");
    // The readout at the peak tick shows the same PPS (f64 definition).
    let at = a.at(&timing, a.peak_pps_tick, w).pps;
    assert!(
        (at - a.peak_pps).abs() <= 1e-9 * a.peak_pps.max(1.0),
        "{what}: {at} vs {}",
        a.peak_pps
    );
}

#[test]
fn selection_peak_nps_and_pps_equal_every_tick_for_subsets_corrections_and_clip_shifts() {
    let (mut checked, mut nonempty) = (0, 0);
    for seed in 1..=8u64 {
        let mut d = generate(FixtureShape::Dense, 900, 96, seed * 13);
        set_dense_tempo(&mut d, 9, 120, seed);
        let mut r = rng(seed);
        let last = d
            .track(nt(&d))
            .unwrap()
            .notes
            .iter()
            .map(|n| {
                let r = n.record();
                r.start_tick + r.duration_ticks
            })
            .max()
            .unwrap_or(0);
        let s = &mut d.track_mut(nt(&d)).unwrap().notes;
        s.clear_selection();
        // Two regions and a pitch band: an uneven subset.
        let a0 = r(last);
        s.select_in_region(a0, a0 + last / 3, 0, 127);
        s.select_in_region(r(last), last, 40, 70);
        let corrections = [
            None,
            Some(Correction {
                from_tick: last / 2,
                ticks: 300,
                keys: 5,
                gate: GatePercent::IDENTITY,
                range: (0, 120),
                top: 127,
            }),
        ];
        for correction in corrections {
            for shift in [0i128, 777, -150] {
                for w in [250_000_000, 1_000_000_000] {
                    let a = selection(&d, correction, shift, w);
                    nonempty += usize::from(a.counted > 100);
                    let what = format!(
                        "seed {seed} corr {} shift {shift} w {w}",
                        correction.is_some()
                    );
                    check_selection(&what, &d, &a, played(&d, correction, shift));
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 96);
    assert!(nonempty >= 90, "{nonempty} non-empty selections");
}

#[test]
fn selection_peaks_of_known_onsets_and_polyphony_without_playback() {
    // 120 BPM, 960 PPQN: one second = 1,920 ticks. 10 notes at 0, 40 at
    // 5,000 (half a second long), 35 at 5,100 (tenth of a second).
    let mut notes: Vec<(u64, u64)> = vec![(0, 100); 10];
    notes.extend(std::iter::repeat_n((5_000, 960), 40));
    notes.extend(std::iter::repeat_n((5_100, 192), 35));
    let mut d = doc_with(960, "120", &notes);
    d.track_mut(nt(&d)).unwrap().notes.select_all();
    let a = selection(&d, None, 0, SECOND);
    assert_eq!((a.peak_nps, a.peak_nps_tick), (75, 5_100));
    assert_eq!((a.peak_poly, a.peak_poly_tick), (75, 5_100));
    check_selection("all", &d, &a, played(&d, None, 0));
    // Area of the 1 s window over the stack: 40 x 0.5 s + 35 x 0.1 s = 23.5
    // note·s, reached once every note has ended (tick 5,960) and kept while
    // the window still holds the first starts.
    assert_eq!((a.peak_pps, a.peak_pps_tick), (23.5, 5_960));
    // Deselect the 5,100 stack: the peaks follow the selection alone.
    let s = &mut d.track_mut(nt(&d)).unwrap().notes;
    s.clear_selection();
    s.select_in_region(0, 5_001, 0, 127);
    let a = selection(&d, None, 0, SECOND);
    assert_eq!((a.peak_nps, a.peak_nps_tick), (40, 5_000));
    assert_eq!((a.peak_pps, a.peak_pps_tick), (20.0, 5_960));
    check_selection("first two stacks", &d, &a, played(&d, None, 0));
    // Nothing selected: empty peaks.
    d.track_mut(nt(&d)).unwrap().notes.clear_selection();
    let a = selection(&d, None, 0, SECOND);
    assert_eq!(
        (a.counted, a.peak_nps, a.peak_pps, a.peak_poly),
        (0, 0, 0.0, 0)
    );
}

/// B1-0105: the gate range and the short / medium / long counts against a
/// brute force over the played selected notes (pairs in played order).
fn check_gates(what: &str, d: &PlumDocument, a: &SelectionAnalysis, played: &(Vec<u64>, Vec<u64>)) {
    let gates: Vec<u64> = played.0.iter().zip(&played.1).map(|(s, e)| e - s).collect();
    let Some(g) = a.gate else {
        assert!(
            gates.is_empty(),
            "{what}: no gate range for {} notes",
            gates.len()
        );
        return;
    };
    assert_eq!(g.min, *gates.iter().min().unwrap(), "{what}: shortest gate");
    assert_eq!(g.max, *gates.iter().max().unwrap(), "{what}: longest gate");
    assert_eq!(g.count, gates.len() as u64, "{what}: gates");
    assert_eq!(
        g.sum,
        gates.iter().map(|&x| u128::from(x)).sum::<u128>(),
        "{what}: gate sum"
    );
    let (sl, ll) = gate_limits(d.division().musical_ppqn().unwrap_or(480));
    for (lo, hi) in [
        (sl, ll),
        (0, 0),
        (1, 1),
        (g.min, g.max),
        (g.max + 1, g.max + 1),
        (5, 3),
    ] {
        let c = a.gate_counts(lo, hi);
        let hi = hi.max(lo);
        let short = gates.iter().filter(|&&x| x < lo).count() as u64;
        let long = gates.iter().filter(|&&x| x >= hi).count() as u64;
        assert_eq!(
            (c.short, c.medium, c.long),
            (short, gates.len() as u64 - short - long, long),
            "{what}: counts at {lo} / {hi}"
        );
    }
    let timing = Timing::for_document(d);
    let spans = timing.map().spans(a.first_tick, a.last_tick);
    let constant = spans.len() == 1 && spans[0].linear.is_some();
    match g.nanos {
        Some((lo, hi)) => {
            assert!(constant, "{what}: time only at one tempo");
            let at = |x: u64| timing.nanos_at(a.first_tick + x) - timing.nanos_at(a.first_tick);
            assert_eq!((lo, hi), (at(g.min), at(g.max)), "{what}");
        }
        None => assert!(!constant, "{what}: one constant tempo gives a time"),
    }
}

/// B1-0105: gates of known notes — 1-tick spam next to held notes, at 960
/// PPQN: the range, the classes and the time at a constant 120 BPM.
#[test]
fn selection_gate_range_of_known_notes_with_classes_and_time() {
    let mut d = doc_with(
        960,
        "120",
        &[(0, 1), (0, 1), (10, 7), (20, 240), (40, 960), (100, 3_840)],
    );
    let t = nt(&d);
    d.track_mut(t).unwrap().notes.select_all();
    let a = selection(&d, None, 0, SECOND);
    let g = a.gate.expect("notes selected");
    assert_eq!(
        (g.min, g.max, g.count, g.sum),
        (1, 3_840, 6, 1 + 1 + 7 + 240 + 960 + 3_840)
    );
    assert_eq!(gate_limits(960), (240, 960));
    assert_eq!(gate_limits(0), (1, 1));
    let c = a.gate_counts(240, 960);
    assert_eq!(
        (c.short, c.medium, c.long),
        (3, 1, 2),
        "short < 1/16, long >= 1 beat"
    );
    // 120 BPM at 960 PPQN: 1 tick = 520,833.3 ns; 3,840 ticks = 2 s.
    let (lo, hi) = g.nanos.expect("one tempo");
    assert!((520_833..=520_834).contains(&lo), "{lo}");
    assert_eq!(hi, 2 * SECOND);
    assert!((g.average() - 841.5).abs() < 1e-9);
    check_gates("known", &d, &a, &played(&d, None, 0));
    // Nothing selected: no range, no counts.
    d.track_mut(t).unwrap().notes.clear_selection();
    let a = selection(&d, None, 0, SECOND);
    assert_eq!(a.gate, None);
    assert_eq!(a.gate_counts(240, 960), Default::default());
}

// ---- the incremental update (V09-0056) ----

/// Every value a selection analysis shows.
fn values(a: &SelectionAnalysis) -> Vec<u128> {
    vec![
        a.selected.into(),
        a.counted.into(),
        a.first_tick.into(),
        a.last_tick.into(),
        a.span_ns.into(),
        a.peak_poly.into(),
        a.peak_poly_tick.into(),
        a.peak_unique.into(),
        a.peak_unique_tick.into(),
        a.area_ticks,
        a.area_ns,
        a.peak_nps.into(),
        a.peak_nps_tick.into(),
        a.peak_pps.to_bits().into(),
        a.peak_pps_tick.into(),
        a.window_ns.into(),
        a.gate.map_or(0, |g| g.min.into()),
        a.gate.map_or(0, |g| g.max.into()),
        a.gate.map_or(0, |g| g.count.into()),
        a.gate.map_or(0, |g| g.sum),
        a.gate
            .and_then(|g| g.nanos)
            .map_or(u128::MAX, |(l, h)| u128::from(l) << 64 | u128::from(h)),
        a.gate_counts(120, 480).short.into(),
        a.gate_counts(120, 480).long.into(),
        a.gate_counts(7, 7).short.into(),
    ]
}

fn inputs_of(d: &PlumDocument, correction: Option<Correction>, shift: i128) -> Vec<SelectionInput> {
    vec![SelectionInput {
        id: d.track(nt(d)).unwrap().id(),
        notes: d.track(nt(d)).unwrap().notes.clone(),
        correction,
        shift,
    }]
}

/// Applies `edit`, updates `prev` from it and checks that the update equals
/// a fresh build in every value (and every position metric sampled), and
/// that its peaks equal the brute force. Returns what the update re-did.
#[allow(clippy::too_many_arguments)]
fn sel_step(
    what: &str,
    d: &mut PlumDocument,
    prev: &mut SelectionAnalysis,
    correction: Option<Correction>,
    shift: i128,
    w: u64,
    brute: bool,
    edit: impl FnOnce(&mut PlumDocument),
) -> plumspirit_engine::analysis::selection::UpdateStats {
    edit(d);
    let timing = Timing::for_document(d);
    let inputs = inputs_of(d, correction, shift);
    let up = prev.update(&inputs, &timing, w, None).unwrap();
    let built = SelectionAnalysis::build_for(&inputs, &timing, w, None).unwrap();
    assert_eq!(values(&up), values(&built), "{what}: update == build");
    let last = built.last_tick + 50;
    for k in 0..40u64 {
        let t = last * k / 39;
        assert_eq!(
            up.at(&timing, t, w),
            built.at(&timing, t, w),
            "{what}: metrics at {t}"
        );
    }
    if brute {
        check_selection(what, d, &up, played(d, correction, shift));
    }
    let stats = up.update_stats;
    *prev = up;
    stats
}

/// A note location of the n-th selected / unselected note of track 0.
fn nth_note(
    d: &PlumDocument,
    selected: bool,
    n: usize,
) -> Option<plumspirit_engine::model::note_store::NoteLoc> {
    let s = &d.track(nt(d)).unwrap().notes;
    let mut k = 0;
    for ci in 0..s.chunk_count() {
        for i in 0.. {
            let loc = plumspirit_engine::model::note_store::NoteLoc {
                chunk: ci as u32,
                index: i,
            };
            let Some(v) = s.get(loc) else { break };
            if v.selected == selected {
                if k == n {
                    return Some(loc);
                }
                k += 1;
            }
        }
    }
    None
}

#[test]
fn the_selection_update_equals_a_fresh_build_after_chained_edits_and_reanalyses_only_what_changed()
{
    let mut d = generate(FixtureShape::Dense, 24_000, 96, 4242);
    set_dense_tempo(&mut d, 30, 900, 5);
    let w = SECOND;
    let last = d
        .track(nt(&d))
        .unwrap()
        .notes
        .iter()
        .map(|n| n.start_tick)
        .max()
        .unwrap();
    {
        let s = &mut d.track_mut(nt(&d)).unwrap().notes;
        s.clear_selection();
        s.select_in_region(last / 10, last * 7 / 10, 0, 127);
    }
    let chunks = d.track(nt(&d)).unwrap().notes.chunk_count();
    assert!(chunks >= 4, "{chunks} chunks");
    let timing = Timing::for_document(&d);
    let mut a = SelectionAnalysis::build_for(&inputs_of(&d, None, 0), &timing, w, None).unwrap();
    assert!(a.update_stats.full);
    let mut r = rng(99);

    // Ctrl+click: one more note selected far from the others.
    let st = sel_step("select one note", &mut d, &mut a, None, 0, w, true, |d| {
        let loc = nth_note(d, false, 7).unwrap();
        d.track_mut(nt(d)).unwrap().notes.set_selected(loc, true);
    });
    assert!(!st.full && st.pieces_built == 1, "{st:?}");
    assert!(
        st.profile_blocks.is_some() && st.unique_blocks.is_some(),
        "{st:?}"
    );
    // Deselect a few notes inside the selection.
    let st = sel_step(
        "deselect three notes",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            for n in [100, 900, 1500] {
                let loc = nth_note(d, true, n).unwrap();
                d.track_mut(nt(d)).unwrap().notes.set_selected(loc, false);
            }
        },
    );
    assert!(st.pieces_built <= 3 && st.pieces_reused > 0, "{st:?}");
    assert!(st.profile_blocks.is_some(), "{st:?}");
    // Unselected notes drawn into the selected region: the touched chunks
    // are new allocations with the same selected content — nothing to sweep.
    let st = sel_step(
        "unselected notes added",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            let mut recs: Vec<_> = (0..5)
                .map(|k| {
                    note(
                        last / 3 + k * 700 + r(50),
                        30 + r(90),
                        (30 + r(60)) as u8,
                        0,
                    )
                })
                .collect();
            recs.sort_by_key(|n| n.start_tick);
            d.track_mut(nt(d))
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        },
    );
    assert_eq!(st.pieces_built, 0, "{st:?}");
    assert_eq!(
        (st.profile_blocks, st.unique_blocks),
        (Some(0), Some(0)),
        "{st:?}"
    );
    // The same, then restored ("undo"): the restored chunks are the old
    // allocations — reused by key or by content.
    let before = d.track(nt(&d)).unwrap().notes.clone();
    sel_step(
        "more unselected notes",
        &mut d,
        &mut a,
        None,
        0,
        w,
        false,
        |d| {
            let recs = vec![note(last / 2, 400, 61, 0), note(last / 2 + 3, 10, 62, 0)];
            d.track_mut(nt(d))
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        },
    );
    let st = sel_step("undo", &mut d, &mut a, None, 0, w, true, |d| {
        d.track_mut(nt(d)).unwrap().notes = before;
    });
    assert_eq!(st.pieces_built, 0, "{st:?}");
    // Paste: selected notes inserted (the old selection kept).
    let st = sel_step(
        "selected notes pasted",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            let recs: Vec<_> = (0..40u64)
                .map(|k| note(last * 6 / 10 + k * 3, 200, (40 + k % 30) as u8, 0))
                .collect();
            d.track_mut(nt(d))
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, true)
                .unwrap();
        },
    );
    assert!(!st.full && st.pieces_built <= 2, "{st:?}");
    // Unselected notes erased (the Erase tool keeps the others' selection):
    // the selected content is the same — nothing built, nothing swept.
    let st = sel_step(
        "unselected notes erased",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            let t = nt(d);
            let s = &d.track(t).unwrap().notes;
            let mut b = plumspirit_engine::model::patch::RankSetBuilder::default();
            for (rank, v) in s.iter().enumerate() {
                if !v.selected
                    && v.start_tick > last * 8 / 10
                    && v.start_tick < last * 8 / 10 + 3_000
                {
                    b.push(rank);
                }
            }
            let ranks = b.finish();
            assert!(ranks.len() > 10, "{}", ranks.len());
            d.track_mut(t)
                .unwrap()
                .notes
                .remove_ranks_keeping_selection(&ranks)
                .unwrap();
        },
    );
    assert_eq!(st.pieces_built, 0, "{st:?}");
    assert_eq!(
        (st.profile_blocks, st.unique_blocks),
        (Some(0), Some(0)),
        "{st:?}"
    );
    // A big paste into one region: only that region's chunks are built.
    let st = sel_step(
        "20,000 selected notes pasted in one region",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            let at = last * 4 / 10;
            let recs: Vec<_> = (0..20_000u64)
                .map(|k| note(at + k / 8, 10 + k % 90, (30 + k % 60) as u8, 0))
                .collect();
            d.track_mut(nt(d))
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, true)
                .unwrap();
        },
    );
    // Built: the pasted notes plus the chunk(s) receiving them (at most two
    // chunks of 4,096); every other chunk reused.
    assert!(!st.full && st.notes_built <= 20_000 + 2 * 4_096, "{st:?}");
    assert!(st.pieces_reused >= 3, "{st:?}");
    assert!(
        st.profile_blocks.is_some() && st.unique_blocks.is_some(),
        "{st:?}"
    );
    // Transpose the selection: pitches only — the timing profile is kept,
    // Unique Pitch Poly is swept again.
    let st = sel_step(
        "transpose the selection",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            d.track_mut(nt(d))
                .unwrap()
                .notes
                .map_selected_attrs(|x| x.with_pitch((x.pitch() + 7).min(127)).unwrap());
        },
    );
    assert_eq!((st.pieces_built, st.profile_blocks), (0, Some(0)), "{st:?}");
    // Durations of every selected note: a change everywhere.
    sel_step(
        "durations of the selection",
        &mut d,
        &mut a,
        None,
        0,
        w,
        true,
        |d| {
            let _ = d.track_mut(nt(d)).unwrap().notes.set_selected_durations(37);
        },
    );
    // Another window: only the profile is rebuilt.
    let st = sel_step(
        "window 0.5 s",
        &mut d,
        &mut a,
        None,
        0,
        500_000_000,
        true,
        |_| {},
    );
    assert_eq!(
        (st.pieces_built, st.profile_blocks, st.unique_blocks),
        (0, None, Some(0)),
        "{st:?}"
    );
    // A correction or a shift: other inputs, a full build.
    let c = Some(Correction {
        from_tick: last / 2,
        ticks: -120,
        keys: -3,
        gate: GatePercent::IDENTITY,
        range: (0, 127),
        top: 127,
    });
    let st = sel_step(
        "correction",
        &mut d,
        &mut a,
        c,
        0,
        500_000_000,
        true,
        |_| {},
    );
    assert!(st.full, "{st:?}");
    let st = sel_step(
        "corrected: one note deselected",
        &mut d,
        &mut a,
        c,
        0,
        500_000_000,
        true,
        |d| {
            let loc = nth_note(d, true, 333).unwrap();
            d.track_mut(nt(d)).unwrap().notes.set_selected(loc, false);
        },
    );
    assert!(!st.full && st.pieces_built <= 2, "{st:?}");
    let st = sel_step(
        "clip shift",
        &mut d,
        &mut a,
        c,
        960,
        500_000_000,
        true,
        |_| {},
    );
    assert!(st.full, "{st:?}");
    let st = sel_step(
        "shifted: a region added",
        &mut d,
        &mut a,
        c,
        960,
        500_000_000,
        true,
        |d| {
            d.track_mut(nt(d)).unwrap().notes.select_in_region(
                last * 8 / 10,
                last * 8 / 10 + 500,
                0,
                127,
            );
        },
    );
    assert!(!st.full, "{st:?}");
    // Everything deselected, then one note.
    sel_step(
        "nothing selected",
        &mut d,
        &mut a,
        c,
        960,
        500_000_000,
        false,
        |d| {
            d.track_mut(nt(d)).unwrap().notes.clear_selection();
        },
    );
    sel_step("one note", &mut d, &mut a, c, 960, 500_000_000, true, |d| {
        let loc = nth_note(d, false, 2_000).unwrap();
        d.track_mut(nt(d)).unwrap().notes.set_selected(loc, true);
    });
}

#[test]
fn random_chained_selection_updates_equal_fresh_builds() {
    for seed in 1..=6u64 {
        let mut d = generate(FixtureShape::Dense, 3_000, 96, seed * 977);
        set_dense_tempo(&mut d, 12, 600, seed);
        let mut r = rng(seed ^ 0xBEEF);
        let last = d
            .track(nt(&d))
            .unwrap()
            .notes
            .iter()
            .map(|n| n.start_tick)
            .max()
            .unwrap();
        d.track_mut(nt(&d))
            .unwrap()
            .notes
            .select_in_region(r(last), last, 0, 127);
        let timing = Timing::for_document(&d);
        let mut a =
            SelectionAnalysis::build_for(&inputs_of(&d, None, 0), &timing, SECOND, None).unwrap();
        let mut ranged = 0;
        for round in 0..12 {
            let what = format!("seed {seed} round {round}");
            let k = r(6);
            let st = sel_step(
                &what,
                &mut d,
                &mut a,
                None,
                0,
                SECOND,
                round % 4 == 0,
                |d| {
                    let s = &mut d.track_mut(nt(d)).unwrap().notes;
                    match k {
                        0 => {
                            let t = r(last);
                            s.select_in_region(t, t + r(300), 0, 127);
                        }
                        1 => {
                            let n = s.selected_count();
                            if n > 0 {
                                let loc = nth_note(d, true, r(n as u64) as usize).unwrap();
                                d.track_mut(nt(d)).unwrap().notes.set_selected(loc, false);
                            }
                        }
                        2 => {
                            let mut recs: Vec<_> = (0..1 + r(6))
                                .map(|_| note(r(last + 100), r(200), (20 + r(90)) as u8, 0))
                                .collect();
                            recs.sort_by_key(|n| n.start_tick);
                            s.insert_sorted_batch(&recs, r(2) == 0).unwrap();
                        }
                        3 => s.map_selected_attrs(|x| x.with_pitch((x.pitch() + 1) % 120).unwrap()),
                        4 => {
                            let t = r(last);
                            let n = s.select_in_region(t, t + r(200), 0, 127);
                            if n > 0 {
                                let _ = s.set_selected_durations(1 + r(150));
                            }
                        }
                        _ => s.select_all(),
                    }
                },
            );
            ranged += usize::from(!st.full && st.profile_blocks.is_some());
        }
        assert!(ranged >= 3, "seed {seed}: {ranged} ranged updates");
    }
}
