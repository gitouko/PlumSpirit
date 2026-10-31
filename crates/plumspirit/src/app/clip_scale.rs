//! L-S46: clip edits on a Black-MIDI-sized project (hundreds of tracks,
//! millions of notes) — Select All clips, Duplicate (Linked), Undo, Redo —
//! measured as the UI thread sees them: the command plus the frames after it.

use super::test_support::Harness;
use crate::app::commands::Command;
use crate::app::track_list::MainView;
use plumspirit_engine::model::note::{NoteAttrs, NoteOffKind, NoteRecord};
use plumspirit_engine::model::{Bpm, PlumDocument};
use std::time::{Duration, Instant};

const SIZE: egui::Vec2 = egui::vec2(1280.0, 900.0);

/// `tracks` tracks of `per` notes each, Black-MIDI shaped (dense, every
/// track its own colour row).
fn project(tracks: usize, per: u64) -> PlumDocument {
    let mut d = PlumDocument::new_project("scale".into(), Bpm::DEFAULT, 960);
    for _ in 1..tracks {
        d.add_track().unwrap();
    }
    for ti in 0..tracks {
        let recs: Vec<_> = (0..per)
            .map(|k| {
                NoteRecord::new(
                    k * 8 + (ti as u64 % 8),
                    4,
                    NoteAttrs::new(
                        20 + ((k + ti as u64) % 90) as u8,
                        1 + (ti % 126) as u8,
                        (ti % 16) as u8,
                        0,
                        NoteOffKind::NoteOff,
                    )
                    .unwrap(),
                )
            })
            .collect();
        d.track_mut(ti)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    d
}

/// The slowest of `n` frames.
fn frames(h: &mut Harness, n: usize) -> Duration {
    (0..n)
        .map(|_| {
            let t = Instant::now();
            h.app_frame(SIZE, 1.0);
            t.elapsed()
        })
        .max()
        .unwrap_or_default()
}

/// Runs `f` and the frames after it; returns (the call, the slowest frame).
fn timed(h: &mut Harness, f: impl FnOnce(&mut Harness)) -> (Duration, Duration) {
    let t = Instant::now();
    f(h);
    let call = t.elapsed();
    (call, frames(h, 5))
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Frames until the analysis is current (no build running).
fn settle(h: &mut Harness) {
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        h.app_frame(SIZE, 1.0);
        if !h.app.analysis.busy() {
            h.app_frame(SIZE, 1.0);
            if !h.app.analysis.busy() {
                return;
            }
        }
        assert!(Instant::now() < deadline, "analysis never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// [`settle`], returning the slowest frame meanwhile (the frame that
/// collects the finished build included).
fn settle_timed(h: &mut Harness) -> Duration {
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut worst = Duration::ZERO;
    let mut idle = 0;
    while idle < 2 {
        let t = Instant::now();
        h.app_frame(SIZE, 1.0);
        worst = worst.max(t.elapsed());
        idle = if h.app.analysis.busy() { 0 } else { idle + 1 };
        assert!(Instant::now() < deadline, "analysis never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
    worst
}

/// L-S46 (Windows, 224M notes / 700 tracks, Progressor on: Undo of
/// Duplicate Clips (Linked) held the UI 7.3 s): with the analysis in use,
/// Undo and Redo of a linked duplicate of every clip hand the rebuild to the
/// background — the frame after them never builds the index itself — and
/// the settled analysis counts exactly what plays.
#[test]
fn undo_and_redo_of_a_linked_duplicate_of_every_clip_analyse_in_the_background() {
    let mut h = Harness::new(project(40, 3_000));
    h.app.settings.progressor.enabled = true;
    h.app.main_view = MainView::TrackList;
    settle(&mut h);
    let plain = h.app.analysis.index().unwrap().total_notes();
    assert_eq!(plain, 120_000);
    assert!(h.command(Command::SelectAll));
    assert_eq!(h.app.track_list.selected.len(), 40);
    h.chord(Command::OpenTool("duplicate"));
    assert!(!h.doc().clips_are_identity(), "one linked copy per track");
    settle(&mut h);
    assert_eq!(h.app.analysis.index().unwrap().total_notes(), 2 * plain);
    for (cmd, total) in [(Command::Undo, plain), (Command::Redo, 2 * plain)] {
        let builds = h.app.analysis.builds_started;
        assert!(h.command(cmd));
        h.app_frame(SIZE, 1.0);
        assert_eq!(
            h.app.analysis.builds_started,
            builds + 1,
            "{cmd:?}: the rebuild runs in the background"
        );
        settle(&mut h);
        assert_eq!(
            h.app.analysis.index().unwrap().total_notes(),
            total,
            "{cmd:?}"
        );
    }
}

/// `cargo test --release -p plumspirit clip_edit_cost_on_a_large_project -- --ignored --nocapture --test-threads=1`
/// (`PLUMSPIRIT_BENCH_CLIP_TRACKS` × `PLUMSPIRIT_BENCH_CLIP_NOTES`).
#[test]
#[ignore = "measurement: cargo test --release -p plumspirit clip_edit_cost_on_a_large_project -- --ignored --nocapture --test-threads=1"]
fn clip_edit_cost_on_a_large_project() {
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let tracks = env("PLUMSPIRIT_BENCH_CLIP_TRACKS", 700);
    let per = env("PLUMSPIRIT_BENCH_CLIP_NOTES", 30_000) as u64;
    let mut h = Harness::new(project(tracks, per));
    h.app.settings.progressor.enabled = true;
    h.app.main_view = MainView::TrackList;
    let open = frames(&mut h, 5);
    let t = Instant::now();
    settle(&mut h);
    let first = t.elapsed();
    let (sel, sel_f) = timed(&mut h, |h| assert!(h.command(Command::SelectAll)));
    let (dup, dup_f) = timed(&mut h, |h| h.chord(Command::OpenTool("duplicate")));
    let t = Instant::now();
    settle(&mut h);
    let dup_a = t.elapsed();
    let mut rows = vec![format!(
        "analysis: first {:.0} ms, after duplicate {:.0} ms",
        ms(first),
        ms(dup_a)
    )];
    for round in 1..=2 {
        let (u, uf) = timed(&mut h, |h| assert!(h.command(Command::Undo)));
        let t = Instant::now();
        let us = settle_timed(&mut h);
        let ua = t.elapsed();
        let (r, rf) = timed(&mut h, |h| assert!(h.command(Command::Redo)));
        let t = Instant::now();
        let rs = settle_timed(&mut h);
        let ra = t.elapsed();
        rows.push(format!(
            "round {round}: undo {:.1} ms + frames max {:.1} ms (analysis {:.0} ms, frames meanwhile max {:.1} ms); redo {:.1} ms + frames max {:.1} ms (analysis {:.0} ms, frames meanwhile max {:.1} ms)",
            ms(u),
            ms(uf),
            ms(ua),
            ms(us),
            ms(r),
            ms(rf),
            ms(ra),
            ms(rs)
        ));
    }
    // L-S46: no step marks or walks the notes on the UI thread (21M notes:
    // select all 68 ms, the first Undo 81.5 ms before; 224M on Windows:
    // 0.56 s).
    if tracks as u64 * per >= 10_000_000 {
        for (what, d) in [("select all", sel + sel_f), ("duplicate", dup + dup_f)] {
            assert!(d < Duration::from_millis(40), "{what}: {:.1} ms", ms(d));
        }
    }
    println!(
        "{tracks} tracks x {per} notes ({} notes): first frames max {:.1} ms; select all {:.1} + {:.1} ms; duplicate (linked) {:.1} + {:.1} ms; {}",
        h.doc().note_count(),
        ms(open),
        ms(sel),
        ms(sel_f),
        ms(dup),
        ms(dup_f),
        rows.join("; ")
    );
}
