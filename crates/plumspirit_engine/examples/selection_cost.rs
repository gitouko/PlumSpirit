//! Selection Analytics cost probe (v0.9 V09-S19, `docs/SELECTION_PEAKS.md`):
//! what a selection's analysis costs at 1 M and 10 M selected notes today —
//! the build (index of the selected notes, peak profile with Peak NPS /
//! PPS, Unique Pitch Poly blocks), profiles of that index without and with
//! Peak PPS — and small changes: a full rebuild against the incremental
//! update (`SelectionAnalysis::update`). Prints `label: value` lines
//! (median of 3 runs where timed).
//! `cargo run --release -p plumspirit_engine --example selection_cost [notes…]`

use plumspirit_engine::analysis::profile::{PeakProfile, Peaks};
use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput};
use plumspirit_engine::analysis::{AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing};
use plumspirit_engine::fixtures::{FixtureShape, generate, set_dense_tempo};
use plumspirit_engine::model::note_store::NoteLoc;
use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::time::{Duration, Instant};

fn med(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1e3)
}

fn timed<T>(runs: usize, mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut times = Vec::new();
    let mut out = None;
    for _ in 0..runs {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed());
    }
    (out.unwrap(), med(times))
}

/// An edit of the document, by label.
type Change = Box<dyn Fn(&mut PlumDocument)>;

fn inputs(doc: &PlumDocument) -> Vec<SelectionInput> {
    doc.tracks()
        .iter()
        .map(|t| SelectionInput {
            id: t.id(),
            notes: t.notes.clone(),
            correction: None,
            shift: 0,
        })
        .collect()
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![1_000_000, 10_000_000]
    } else {
        sizes
    };
    let scope = Scope::Project;
    for n in sizes {
        println!("## {n} notes (Dense fixture, dense tempo map), all selected");
        let mut doc = generate(FixtureShape::Dense, n, 960, 42);
        set_dense_tempo(&mut doc, 2_000, 960, 7);
        for i in 0..doc.track_count() {
            doc.track_mut(i).unwrap().notes.select_all();
        }
        let timing = Timing::for_document(&doc);
        let ins = inputs(&doc);
        let (a, t) = timed(3, || SelectionAnalysis::build(&ins, &timing, None).unwrap());
        println!("selection build (index + totals sweep): {}", ms(t));
        println!(
            "selection heap: {:.2} MB",
            a.heap_bytes() as f64 / 1_048_576.0
        );
        // B1-0105: the gate range is read with the build; the classes are
        // rank queries on the pieces' gates.
        let (c, t) = timed(3, || a.gate_counts(240, 960));
        println!(
            "gate counts (short / medium / long): {} ({} / {} / {}; range {:?})",
            ms(t),
            c.short,
            c.medium,
            c.long,
            a.gate.map(|g| (g.min, g.max))
        );
        let (idx, t) = timed(3, || AnalysisIndex::for_document(&doc));
        println!(
            "document index build (proxy for the selection index): {}",
            ms(t)
        );
        let (prof, t) = timed(3, || {
            PeakProfile::build(&idx, &scope, &timing, DEFAULT_WINDOW_NS, None).unwrap()
        });
        println!("peak profile build (NPS + poly): {}", ms(t));
        let end = idx.last_tick(&scope);
        println!(
            "profile peak nps {} poly {} (sweep peak poly {})",
            prof.peak_nps_so_far(&idx, &scope, &timing, end),
            prof.peak_poly_so_far(&idx, &scope, end),
            a.peak_poly
        );
        let (pp, t) = timed(3, || {
            PeakProfile::build_with(
                &idx,
                &scope,
                &timing,
                DEFAULT_WINDOW_NS,
                Peaks::WithPps,
                None,
            )
            .unwrap()
        });
        let (v, tk) = pp.peak_pps().unwrap();
        println!(
            "peak profile build with Peak PPS: {} -> {:.3} @ tick {tk} (metrics there {:.3})",
            ms(t),
            v.to_f64(),
            idx.metrics(&scope, &timing, tk, DEFAULT_WINDOW_NS).pps
        );
        // Small changes, each from the same analysis: today's full rebuild
        // against the incremental update (docs/SELECTION_PEAKS.md).
        let base = SelectionAnalysis::build_for(&ins, &timing, DEFAULT_WINDOW_NS, None).unwrap();
        let notes_track = 1;
        let changes: Vec<(&str, Change)> = vec![
            (
                "an unselected note added",
                Box::new(move |d: &mut PlumDocument| {
                    let rec = NoteRecord::new(
                        end / 2,
                        120,
                        NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                    );
                    d.track_mut(notes_track)
                        .unwrap()
                        .notes
                        .insert_sorted_batch(&[rec], false)
                        .unwrap();
                }),
            ),
            (
                "one note deselected (Ctrl+click)",
                Box::new(move |d: &mut PlumDocument| {
                    let s = &mut d.track_mut(notes_track).unwrap().notes;
                    let c = s.chunk_count() / 2;
                    s.set_selected(
                        NoteLoc {
                            chunk: c as u32,
                            index: 5,
                        },
                        false,
                    );
                }),
            ),
            (
                "a selected note pasted",
                Box::new(move |d: &mut PlumDocument| {
                    let rec = NoteRecord::new(
                        end / 3,
                        480,
                        NoteAttrs::new(72, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                    );
                    d.track_mut(notes_track)
                        .unwrap()
                        .notes
                        .insert_sorted_batch(&[rec], true)
                        .unwrap();
                }),
            ),
            (
                "the selection transposed (+1)",
                Box::new(move |d: &mut PlumDocument| {
                    d.track_mut(notes_track)
                        .unwrap()
                        .notes
                        .map_selected_attrs(|x| x.with_pitch((x.pitch() + 1).min(127)).unwrap());
                }),
            ),
            (
                "the window changed to 0.5 s",
                Box::new(|_: &mut PlumDocument| {}),
            ),
        ];
        for (k, (what, change)) in changes.iter().enumerate() {
            let mut d2 = doc.clone();
            change(&mut d2);
            let ins2 = inputs(&d2);
            let w = if k == 4 {
                500_000_000
            } else {
                DEFAULT_WINDOW_NS
            };
            let (_, full) = timed(3, || {
                SelectionAnalysis::build_for(&ins2, &timing, w, None).unwrap()
            });
            let (up, t) = timed(3, || base.update(&ins2, &timing, w, None).unwrap());
            let st = up.update_stats;
            println!(
                "{what}: rebuild {} -> update {} (pieces built {} reused {}, profile blocks {:?}, unique blocks {:?})",
                ms(full),
                ms(t),
                st.pieces_built,
                st.pieces_reused,
                st.profile_blocks,
                st.unique_blocks
            );
        }
        println!();
    }
}
