//! Bulk editing at millions of notes (v1.0-beta1 B1-0164, B1-S69).
//!
//! ```text
//! cargo run --release -p plumspirit_engine --example bulk_edit_profile -- 1000000 10000000
//! ```
//!
//! For each note count N (one dense track of N notes, everything selected),
//! every step goes through the undo history the way the app applies it:
//!
//! * Piano Roll: copy the selection, paste it after the end (prepare + apply),
//!   undo / redo the paste, move all notes, delete the pasted half;
//! * Track List: copy the clip, paste it (independent content), duplicate it
//!   linked and independent, slice it in the middle, undo / redo.
//!
//! Each line gives the time, the undo data and the process's peak RSS (Linux).
//! Comparing N and 10 × N shows whether an operation grows faster than its
//! input (a ratio near 10 is linear).

use plumspirit_engine::editor::clips::{copy_clips, duplicate_clips, paste_clips, slice_clips};
use plumspirit_engine::editor::{self, Clipboard, EditControl};
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::ClipId;
use std::time::{Duration, Instant};

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

fn peak_rss_mb() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or("n/a".into(), |kb| format!("{:.0}", kb / 1024.0))
}

fn line(what: &str, d: Duration, h: &History) {
    println!(
        "  {what:<44} {:>9.1} ms   undo {:>8.1} MB   peak RSS {} MB",
        d.as_secs_f64() * 1000.0,
        h.bytes() as f64 / 1_048_576.0,
        peak_rss_mb()
    );
}

fn main() {
    let counts: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let counts = if counts.is_empty() {
        vec![1_000_000]
    } else {
        counts
    };
    for n in counts {
        println!("## {n} notes (one dense track, 960 PPQN)");
        let (mut doc, t) = time(|| generate(FixtureShape::Dense, n, 960, 42));
        println!("  build: {:.1} ms", t.as_secs_f64() * 1000.0);
        let track = 1usize;
        let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
        let end = doc.track(track).expect("track").content_end_tick();

        // Piano Roll.
        doc.track_mut(track).expect("track").notes.select_all();
        let (clip, t) = time(|| {
            Clipboard::copy_selected(doc.track(track).expect("track"))
                .expect("copy")
                .expect("selection")
        });
        line(&format!("copy {} notes", clip.len()), t, &h);
        let (_, t) = time(|| {
            let e = clip.prepare_paste(&doc, track, end + 960).expect("paste");
            h.apply(
                &mut doc,
                Operation::Prepared {
                    edit: e,
                    label: "Paste",
                },
            )
            .expect("apply")
        });
        line("paste after the end (prepare + apply)", t, &h);
        let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
        line("undo paste", t, &h);
        let (_, t) = time(|| h.redo(&mut doc).expect("redo"));
        line("redo paste", t, &h);
        let (_, t) = time(|| {
            let e =
                editor::prepare_move(&doc, track, 960, 0, EditControl::default()).expect("move");
            h.apply(
                &mut doc,
                Operation::Prepared {
                    edit: e,
                    label: "Move",
                },
            )
            .expect("apply")
        });
        line("move the pasted notes (+1 beat)", t, &h);
        let (_, t) = time(|| {
            h.apply(
                &mut doc,
                Operation::DeleteSelected {
                    track: track.into(),
                },
            )
            .expect("delete")
        });
        line("delete the pasted notes", t, &h);
        drop(clip);

        // Track List.
        let id = ClipId::Primary(doc.track(track).expect("track").id());
        let (cb, t) = time(|| copy_clips(&doc, &[id]).expect("copy clips"));
        line("copy the clip", t, &h);
        let (_, t) = time(|| {
            let op = paste_clips(&doc, &cb, track, end + 960).expect("paste clips");
            h.apply(&mut doc, op).expect("apply")
        });
        line("paste the clip (independent content)", t, &h);
        let (_, t) = time(|| {
            let op = duplicate_clips(&doc, &[(id, track, 2 * end + 2000)], true).expect("dup");
            h.apply(&mut doc, op).expect("apply")
        });
        line("duplicate the clip linked", t, &h);
        let (_, t) = time(|| {
            let op = duplicate_clips(&doc, &[(id, track, 3 * end + 3000)], false).expect("dup");
            h.apply(&mut doc, op).expect("apply")
        });
        line("duplicate the clip independent", t, &h);
        let (_, t) = time(|| {
            let op = slice_clips(&doc, &[id], end / 2).expect("slice");
            h.apply(&mut doc, op).expect("apply")
        });
        line("slice the clip in the middle", t, &h);
        let (_, t) = time(|| {
            for _ in 0..4 {
                h.undo(&mut doc).expect("undo");
            }
        });
        line("undo slice + duplicates + paste", t, &h);
        let (_, t) = time(|| {
            for _ in 0..4 {
                h.redo(&mut doc).expect("redo");
            }
        });
        line("redo them", t, &h);
        // Many clips in one step (the Track List's Select All + Duplicate /
        // Slice): the clip table must not grow quadratically.
        for clips in [1_000usize, 10_000, 100_000] {
            let base_start = 4 * end + 4000;
            let copies: Vec<(ClipId, usize, u64)> = (0..clips)
                .map(|k| (id, track, base_start + k as u64 * (end + 10)))
                .collect();
            let (_, t) = time(|| {
                let op = duplicate_clips(&doc, &copies, true).expect("dup many");
                h.apply(&mut doc, op).expect("apply")
            });
            line(
                &format!("duplicate the clip {clips} times linked (one step)"),
                t,
                &h,
            );
            let placed = doc.clips().layout().placements.len();
            let all: Vec<ClipId> = doc
                .clips()
                .layout()
                .placements
                .iter()
                .map(|p| p.id)
                .collect();
            let (_, t) = time(|| {
                let op = slice_clips(&doc, &all, base_start + end / 4).expect("slice in many");
                h.apply(&mut doc, op).expect("apply")
            });
            line(&format!("slice with all {placed} clips selected"), t, &h);
            h.undo(&mut doc).expect("undo slice");
            let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
            line(&format!("undo it ({placed} placements before)"), t, &h);
        }
        println!("  notes stored {}", doc.note_count());
    }
}
