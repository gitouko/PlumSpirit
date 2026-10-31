//! v0.8 §073: one canonical definition of Notes, Poly, NPS, PPS, Peak Poly,
//! Peak NPS, Unique Pitch Poly and Poly Area. The Progressor (project index +
//! peak profile) and Selection Analytics (index over the selected notes +
//! totals sweep) are checked against one brute-force reading of the
//! definitions in `analysis::canonical`, and against each other when the
//! whole project is selected, on random projects with corrections, muted
//! notes, zero-length notes, coincident starts / ends and tempo ramps.

use plumspirit_engine::analysis::profile::{PeakProfile, window_peak_poly};
use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput};
use plumspirit_engine::analysis::{AnalysisIndex, Scope, Timing, document_corrections};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::correction::Corrected;
use plumspirit_engine::model::{
    Bpm, GatePercent, Interp, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TempoValue,
};

const W: u64 = 1_000_000_000; // the default 1 s window

/// Small deterministic generator (tests only).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// A random project: 1–3 tracks, some with Track Properties corrections,
/// muted and zero-length notes, notes stacked on the same ticks, and a tempo
/// map with a linear ramp.
fn project(seed: u64) -> PlumDocument {
    let mut r = Lcg(seed);
    let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 96);
    let mut h = History::default();
    let tracks = 1 + r.below(3) as usize;
    for _ in 1..tracks {
        h.apply(&mut d, Operation::AddTrack).unwrap();
    }
    for ti in 0..tracks {
        let n = 50 + r.below(250);
        let mut recs: Vec<NoteRecord> = (0..n)
            .map(|_| {
                // Coarse grid: many coincident starts and ends.
                let start = r.below(400) * 12;
                let dur = match r.below(6) {
                    0 => 0,
                    1 => 12,
                    _ => r.below(30) * 12,
                };
                let a = NoteAttrs::new(
                    40 + r.below(24) as u8,
                    1 + r.below(127) as u8,
                    r.below(16) as u8,
                    64,
                    NoteOffKind::NoteOff,
                )
                .unwrap()
                .with_muted(r.below(9) == 0);
                NoteRecord::new(start, dur, a)
            })
            .collect();
        recs.sort_by_key(|n| n.start_tick);
        d.track_mut(ti)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        if r.below(2) == 0 {
            let t = d.track(ti).unwrap();
            let mut p = t.props().clone();
            p.tick_correction = r.below(200) as i64 - 60;
            p.key_correction = r.below(7) as i16 - 3;
            p.gate = GatePercent::from_hundredths(5_000 + r.below(10_000) as u32).unwrap();
            if r.below(2) == 0 {
                p.key_range = (45, 58);
            }
            p.correction_from_measure = 1 + r.below(3) as u32;
            let id = t.id();
            h.apply(
                &mut d,
                Operation::TrackProps {
                    track: id,
                    props: p,
                    end: None,
                    label: "Props",
                },
            )
            .unwrap();
        }
    }
    for (tick, bpm, interp) in [(960, 200, Interp::Linear), (2400, 90, Interp::Step)] {
        let edit = d
            .conductor()
            .prepare_set_tempo(
                None,
                tick + r.below(10) * 12,
                TempoValue::Bpm(Bpm::from_integer(bpm).unwrap()),
                interp,
            )
            .unwrap();
        h.apply(&mut d, Operation::Conductor { edit, label: "T" })
            .unwrap();
    }
    d
}

/// The notes of `d` as they play, `(start, end, pitch)`: corrections applied
/// once, muted notes and notes a correction drops left out. `only_selected`:
/// the selected notes.
fn played(d: &PlumDocument, only_selected: bool) -> Vec<(u64, u64, u8)> {
    let corr = document_corrections(d);
    let mut out = Vec::new();
    for (t, c) in d.tracks().iter().zip(corr) {
        for v in t.notes.iter() {
            if v.attrs.muted() || (only_selected && !v.selected) {
                continue;
            }
            let r = v.record();
            match c {
                None => out.push((
                    r.start_tick,
                    r.start_tick + r.duration_ticks,
                    r.attrs.pitch(),
                )),
                Some(cr) => {
                    if let Corrected::Note { note, .. } = cr.note(r) {
                        out.push((
                            note.start_tick,
                            note.start_tick + note.duration_ticks,
                            note.attrs.pitch(),
                        ));
                    }
                }
            }
        }
    }
    out
}

/// The definitions, read literally.
struct Brute<'a> {
    notes: &'a [(u64, u64, u8)],
    timing: &'a Timing,
}

impl Brute<'_> {
    fn notes_le(&self, t: u64) -> u64 {
        self.notes.iter().filter(|n| n.0 <= t).count() as u64
    }
    /// Sounding over [start, end).
    fn poly(&self, t: u64) -> u64 {
        self.notes.iter().filter(|n| n.0 <= t && t < n.1).count() as u64
    }
    fn unique(&self, t: u64) -> u64 {
        let mut seen = [false; 128];
        for n in self.notes.iter().filter(|n| n.0 <= t && t < n.1) {
            seen[usize::from(n.2)] = true;
        }
        seen.iter().filter(|&&s| s).count() as u64
    }
    /// Starts in the trailing window (t − W, t] of musical time.
    fn nps(&self, t: u64) -> u64 {
        let now = self.timing.nanos_at(t);
        match (now >= W).then(|| self.timing.tick_at_nanos(now - W)) {
            Some(lo) => self.notes.iter().filter(|n| n.0 > lo && n.0 <= t).count() as u64,
            None => self.notes_le(t),
        }
    }
    fn starts(&self) -> Vec<u64> {
        let mut s: Vec<u64> = self.notes.iter().map(|n| n.0).collect();
        s.sort_unstable();
        s.dedup();
        s
    }
    fn peak_poly(&self, upto: u64) -> u64 {
        self.starts()
            .into_iter()
            .filter(|&t| t <= upto)
            .map(|t| self.poly(t))
            .max()
            .unwrap_or(0)
    }
    fn peak_unique(&self) -> u64 {
        self.starts()
            .into_iter()
            .map(|t| self.unique(t))
            .max()
            .unwrap_or(0)
    }
    fn peak_nps(&self, upto: u64) -> u64 {
        self.starts()
            .into_iter()
            .filter(|&t| t <= upto)
            .map(|t| self.nps(t))
            .max()
            .unwrap_or(0)
    }
    fn area(&self) -> (u128, u128) {
        self.notes.iter().fold((0, 0), |(a, b), n| {
            (
                a + u128::from(n.1 - n.0),
                b + u128::from(self.timing.nanos_at(n.1) - self.timing.nanos_at(n.0)),
            )
        })
    }
}

fn selection(d: &PlumDocument, timing: &Timing) -> SelectionAnalysis {
    let corr = document_corrections(d);
    let inputs: Vec<SelectionInput> = d
        .tracks()
        .iter()
        .zip(corr)
        .map(|(t, correction)| SelectionInput {
            id: t.id(),
            notes: t.notes.clone(),
            correction,
            shift: 0,
        })
        .collect();
    SelectionAnalysis::build(&inputs, timing, None).unwrap()
}

#[test]
fn progressor_and_selection_analytics_share_one_definition_of_every_metric() {
    for seed in 1..=40u64 {
        let mut d = project(seed);
        for i in 0..d.track_count() {
            d.track_mut(i).unwrap().notes.select_all();
        }
        let timing = Timing::for_document(&d);
        let notes = played(&d, false);
        let b = Brute {
            notes: &notes,
            timing: &timing,
        };
        let idx = AnalysisIndex::for_document(&d);
        let prof = PeakProfile::build(&idx, &Scope::Project, &timing, W, None).unwrap();
        let sel = selection(&d, &timing);
        let last = notes.iter().map(|n| n.1).max().unwrap_or(0);

        // Totals of the selection (= the whole project).
        assert_eq!(sel.counted, notes.len() as u64, "seed {seed}: counted");
        assert_eq!(
            sel.counted,
            idx.total_notes(),
            "seed {seed}: one note count"
        );
        assert_eq!(sel.peak_poly, b.peak_poly(last), "seed {seed}: Peak Poly");
        assert_eq!(
            u64::from(sel.peak_unique),
            b.peak_unique(),
            "seed {seed}: Unique Pitch Poly"
        );
        assert_eq!(
            (sel.area_ticks, sel.area_ns),
            b.area(),
            "seed {seed}: Poly Area"
        );
        // Peaks of the Progressor = the selection's when everything is selected.
        assert_eq!(
            prof.peak_poly_so_far(&idx, &Scope::Project, last),
            sel.peak_poly,
            "seed {seed}: Progressor Peak Poly"
        );
        assert_eq!(
            window_peak_poly(&idx, &Scope::Project, None, last),
            sel.peak_poly,
            "seed {seed}: exact window peak"
        );

        // Position metrics at many ticks: identical readouts.
        let mut r = Lcg(seed * 77);
        let mut ticks: Vec<u64> = (0..60).map(|_| r.below(last + 50)).collect();
        ticks.extend(
            notes
                .iter()
                .take(20)
                .flat_map(|n| [n.0, n.1, n.1.saturating_sub(1)]),
        );
        for t in ticks {
            let p = idx.metrics(&Scope::Project, &timing, t, W);
            let s = sel.at(&timing, t, W);
            assert_eq!(p, s, "seed {seed} tick {t}: same metrics");
            assert_eq!(p.notes_passed, b.notes_le(t), "seed {seed} tick {t}: Notes");
            assert_eq!(p.poly, b.poly(t), "seed {seed} tick {t}: Poly");
            assert_eq!(p.nps, b.nps(t), "seed {seed} tick {t}: NPS");
            assert_eq!(
                prof.peak_poly_so_far(&idx, &Scope::Project, t),
                b.peak_poly(t),
                "seed {seed} tick {t}: Peak Poly so far"
            );
            assert_eq!(
                prof.peak_nps_so_far(&idx, &Scope::Project, &timing, t),
                b.peak_nps(t),
                "seed {seed} tick {t}: Peak NPS so far"
            );
        }
    }
}

#[test]
fn a_partial_selection_follows_the_same_definitions() {
    for seed in 100..=130u64 {
        let mut d = project(seed);
        let mut r = Lcg(seed);
        for i in 0..d.track_count() {
            let lo = r.below(2400);
            d.track_mut(i)
                .unwrap()
                .notes
                .select_range(lo, lo + 1 + r.below(2400), true);
        }
        let timing = Timing::for_document(&d);
        let notes = played(&d, true);
        let b = Brute {
            notes: &notes,
            timing: &timing,
        };
        let sel = selection(&d, &timing);
        let last = notes.iter().map(|n| n.1).max().unwrap_or(0);
        assert_eq!(sel.counted, notes.len() as u64, "seed {seed}");
        assert_eq!(sel.peak_poly, b.peak_poly(last), "seed {seed}: Peak Poly");
        assert_eq!(u64::from(sel.peak_unique), b.peak_unique(), "seed {seed}");
        assert_eq!((sel.area_ticks, sel.area_ns), b.area(), "seed {seed}");
        for t in (0..last + 30).step_by(37) {
            let m = sel.at(&timing, t, W);
            assert_eq!(m.poly, b.poly(t), "seed {seed} tick {t}");
            assert_eq!(m.nps, b.nps(t), "seed {seed} tick {t}");
            assert_eq!(m.notes_passed, b.notes_le(t), "seed {seed} tick {t}");
        }
    }
}
