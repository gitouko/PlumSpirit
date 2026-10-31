//! MIDI Inspector cost probe (v0.9 V09-S18, `docs/INSPECTOR.md`): what the
//! Inspector's numbers cost on the shared analysis at 1 M and 10 M notes —
//! the index, whole-song exact peaks through the peak profile, a bounded
//! chart's samples, the export snapshot's counts, and one small edit's
//! re-analysis. Prints `label: value` lines (median of 3 runs where timed).
//! `cargo run --release -p plumspirit_engine --example inspector_cost [notes…]`

use plumspirit_engine::analysis::profile::PeakProfile;
use plumspirit_engine::analysis::{
    AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing, TrackInput, document_corrections,
};
use plumspirit_engine::fixtures::{FixtureShape, generate, set_dense_tempo};
use plumspirit_engine::midi::{ExportOptions, ExportSnapshot};
use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
use std::time::{Duration, Instant};

fn med(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1e3)
}

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
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
    for n in sizes {
        println!("## {n} notes (Dense fixture, dense tempo map)");
        let mut doc = generate(FixtureShape::Dense, n, 960, 42);
        set_dense_tempo(&mut doc, 2_000, 960, 7);
        let rss_doc = rss_mb();
        let scope = Scope::Project;
        let (idx, t) = timed(1, || AnalysisIndex::for_document(&doc));
        println!("index build: {}", ms(t));
        println!(
            "index heap: {:.2} MB",
            idx.heap_bytes() as f64 / 1_048_576.0
        );
        let timing = Timing::for_document(&doc);
        let end = idx.last_tick(&scope);
        let (prof, t) = timed(3, || {
            PeakProfile::build(&idx, &scope, &timing, DEFAULT_WINDOW_NS, None).unwrap()
        });
        println!("peak profile build: {}", ms(t));
        println!(
            "peak profile heap: {:.2} MB",
            prof.heap_bytes() as f64 / 1_048_576.0
        );
        let ((pn, pp), t) = timed(3, || {
            (
                prof.peak_nps_so_far(&idx, &scope, &timing, end),
                prof.peak_poly_so_far(&idx, &scope, end),
            )
        });
        println!(
            "whole-song peaks from the profile: {} (nps {pn}, poly {pp})",
            ms(t)
        );
        // A bounded chart: 2,048 columns over the song's duration in seconds;
        // samples at the column ends (metrics) and the exact poly peak of each
        // column (peak_poly_between).
        let dur = timing.nanos_at(end);
        let cols = 2_048u64;
        let bounds: Vec<u64> = (0..=cols)
            .map(|k| timing.tick_at_nanos(dur / cols * k))
            .collect();
        let (samples, t) = timed(3, || {
            bounds[1..]
                .iter()
                .map(|&tk| idx.metrics(&scope, &timing, tk, DEFAULT_WINDOW_NS).nps)
                .max()
        });
        println!(
            "chart NPS samples at 2,048 column ends: {} (max sample {samples:?})",
            ms(t)
        );
        let (col_poly, t) = timed(3, || {
            bounds
                .windows(2)
                .map(|w| prof.peak_poly_between(&idx, &scope, Some(w[0]), w[1]))
                .max()
        });
        println!(
            "chart exact poly per column (2,048 x peak_poly_between): {} (max {col_poly:?})",
            ms(t)
        );
        // The Inspector's chart (docs/INSPECTOR.md I5): exact (NPS, poly)
        // maxima per column over the same 2,048 time columns.
        let (cols_v, t) = timed(3, || prof.chart_columns(&idx, &scope, &timing, &bounds));
        let top = cols_v
            .iter()
            .fold((0, 0), |m, c| (m.0.max(c.0), m.1.max(c.1)));
        println!(
            "chart_columns (2,048 exact NPS + poly columns): {} (highest column nps {}, poly {})",
            ms(t),
            top.0,
            top.1
        );
        // The export snapshot's counts.
        let (snap, t) = timed(3, || ExportSnapshot::take(&doc, ExportOptions::default()));
        println!("export snapshot take: {}", ms(t));
        let (count, t) = timed(3, || snap.note_count());
        println!("snapshot note_count (walk): {} ({count})", ms(t));
        let (nin, t) = timed(3, || idx.notes_in(&scope));
        println!("index notes_in: {} ({nin})", ms(t));
        let (eot, t) = timed(3, || snap.end_tick());
        println!("snapshot end_tick: {} (tick {eot})", ms(t));
        println!("RSS over the document: {:.1} MB", rss_mb() - rss_doc);
        // One small edit: a note added in the middle of track 0.
        let mid = end / 2;
        let rec = NoteRecord::new(
            mid,
            120,
            NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        );
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec], false)
            .unwrap();
        let corr = document_corrections(&doc);
        println!("changed chunks: {}", idx.changed_chunks(&doc, &corr));
        let inputs = TrackInput::all(&doc);
        let (idx2, t) = timed(3, || {
            AnalysisIndex::build(&inputs, Some(&idx), None).unwrap()
        });
        println!("index update after the edit: {}", ms(t));
        let (_, t) = timed(3, || {
            PeakProfile::build(&idx2, &scope, &timing, DEFAULT_WINDOW_NS, None).unwrap()
        });
        println!("peak profile rebuild after the edit: {}", ms(t));
        // The ranged update (docs/INSPECTOR.md I6): the index's delta and
        // the re-sweep of the blocks it reaches.
        let (delta, t) = timed(3, || idx2.delta_since(&idx, &scope).unwrap());
        println!(
            "index delta_since after the edit: {} ({} ticks)",
            ms(t),
            delta.len()
        );
        let (_, t) = timed(3, || {
            prof.update(&idx2, &scope, &timing, &delta, None).unwrap()
        });
        println!("peak profile ranged update after the edit: {}", ms(t));
        let p2 = prof.update(&idx2, &scope, &timing, &delta, None).unwrap();
        let dirty = p2.dirty_ranges(&delta, &timing);
        let (cols2, t) = timed(3, || {
            p2.chart_columns_update(&idx2, &scope, &timing, &bounds, &cols_v, &dirty)
        });
        println!(
            "chart_columns_update after the edit: {} ({} columns, {} dirty ranges)",
            ms(t),
            cols2.len(),
            dirty.len()
        );
        assert_eq!(cols2, p2.chart_columns(&idx2, &scope, &timing, &bounds));
        println!();
    }
}
