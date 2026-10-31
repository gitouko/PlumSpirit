//! Benchmark ladder (v1.0-beta1 B1-0160 / B1-0166, B1-S70).
//!
//! ```text
//! cargo run --release -p plumspirit_engine --example ladder -- \
//!     [--sizes 1000000,10000000,100000000,250000000] [--shapes dense,tracks,…] [--out DIR]
//! ```
//!
//! Deterministic fixtures built note by note into the store (no second copy of
//! the notes, so the largest sizes fit as far as memory allows), then the engine
//! operations a session goes through, each timed (wall and CPU, Linux
//! `/proc/self/stat`) with the RSS after it: analysis index + Progressor
//! queries, playback index + seeks, viewport queries + a density raster,
//! export (write + validate), import, `.lup` save (write + verify) and load,
//! a 10 % move through the undo history and its undo, and the teardown.
//! Queries report p50 / p95 / p99. Before each memory-heavy step the probe
//! compares what it needs with `MemAvailable` and prints `NOT RUN (out of
//! resources …)` instead of running it — an honest gap, never a guess.
//!
//! Shapes: `dense` (16 tracks, chords every sixteenth, 960 PPQN), `tracks`
//! (1,024 tracks), `ppqn` (30,720 PPQN), `cc` (a controller every 4 notes),
//! `keys256` (a 256-key project), `vel1` (velocity 1), `dup` (same-key
//! duplicates), `ch16` (16 channels → 16 inferred subchannels per track),
//! `clips` (1,000 linked clip placements).

use plumspirit_engine::analysis::{AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing};
use plumspirit_engine::editor::{self, EditControl};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::midi::{self, ExportControl, ImportControl};
use plumspirit_engine::model::events::{RawEvent, RawKind};
use plumspirit_engine::model::note::{NoteAttrs, NoteOffKind, NoteRecord};
use plumspirit_engine::model::project::{Bpm, KeyRange};
use plumspirit_engine::model::{ClipId, PlumDocument};
use plumspirit_engine::native::{self, LupControl};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::lod::DensityRaster;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn status_kb(key: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with(key))?;
    l.split_whitespace().nth(1)?.parse().ok()
}

fn available_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let l = s.lines().find(|l| l.starts_with("MemAvailable:"))?;
    l.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}

fn cpu_s() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / 100.0)
}

fn rss_mb() -> String {
    status_kb("VmRSS:").map_or("n/a".into(), |k| format!("{:.0}", k as f64 / 1024.0))
}

/// Runs `f`, prints "op: wall / CPU, RSS".
fn step<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let c0 = cpu_s();
    let t = Instant::now();
    let r = f();
    let wall = t.elapsed();
    let cpu = cpu_s().zip(c0).map(|(b, a)| b - a);
    println!(
        "  {name:<46} {:>10.1} ms wall {:>9} CPU   RSS {} MB",
        wall.as_secs_f64() * 1000.0,
        cpu.map_or("n/a".into(), |c| format!("{:.1} s", c)),
        rss_mb()
    );
    r
}

fn pct(name: &str, mut v: Vec<Duration>) {
    v.sort();
    let at = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize].as_secs_f64() * 1e3;
    println!(
        "  {name:<46} p50 {:.3} ms · p95 {:.3} ms · p99 {:.3} ms ({} runs)",
        at(0.5),
        at(0.95),
        at(0.99),
        v.len()
    );
}

/// Skips a step whose memory need exceeds what is available.
fn fits(name: &str, need: u64) -> bool {
    match available_bytes() {
        Some(a) if need > a * 9 / 10 => {
            println!(
                "  {name:<46} NOT RUN (out of resources: needs about {:.1} GB, {:.1} GB available)",
                need as f64 / 1e9,
                a as f64 / 1e9
            );
            false
        }
        _ => true,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

struct NullSink;
impl SynthSink for NullSink {
    fn note_on(&mut self, _: u8, _: u8, _: u8) {}
    fn note_off(&mut self, _: u8, _: u8) {}
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
    fn silence(&mut self) {}
    fn reset(&mut self) {}
}

/// Builds a fixture of `n` notes straight into the stores (batches of 1M).
fn build(shape: &str, n: usize) -> PlumDocument {
    let ppqn: u32 = if shape == "ppqn" { 30_720 } else { 960 };
    let tracks = match shape {
        "tracks" => 1024,
        _ => 16,
    };
    let mut doc = PlumDocument::new_project(format!("ladder {shape}"), Bpm::DEFAULT, ppqn);
    while doc.track_count() < tracks + 1 {
        doc.add_track().expect("track");
    }
    if shape == "keys256" {
        doc.project_mut().keys = KeyRange::Extended256;
    }
    let ppq = u64::from(ppqn);
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let per_track = n.div_ceil(tracks);
    for ti in 0..tracks {
        let count = per_track.min(n.saturating_sub(ti * per_track));
        let mut t = 0u64;
        let mut done = 0usize;
        let mut batch: Vec<NoteRecord> = Vec::with_capacity(count.min(1 << 20));
        let mut cc_tick = 0u64;
        while done < count {
            batch.clear();
            let take = (count - done).min(1 << 20);
            for k in 0..take {
                let i = done + k;
                if i.is_multiple_of(12) {
                    t += ppq / 4;
                }
                let dup = shape == "dup" && i % 2 == 1;
                let pitch = if shape == "keys256" {
                    (rng.next() % 256) as u8
                } else if dup {
                    batch.last().map_or(60, |r: &NoteRecord| r.attrs.pitch())
                } else {
                    (24 + rng.next() % 80) as u8
                };
                let velocity = if shape == "vel1" {
                    1
                } else {
                    1 + (rng.next() % 127) as u8
                };
                let channel = if shape == "ch16" {
                    (i % 16) as u8
                } else {
                    (ti % 16) as u8
                };
                let attrs = if pitch > 127 {
                    NoteAttrs::new_wide(pitch, velocity, channel, 0, NoteOffKind::NoteOff)
                } else {
                    NoteAttrs::new(pitch, velocity, channel, 0, NoteOffKind::NoteOff)
                }
                .expect("attrs");
                batch.push(NoteRecord {
                    start_tick: t,
                    duration_ticks: 1 + rng.next() % ppq,
                    attrs,
                    sub: Default::default(),
                    group: 0,
                });
            }
            let track = doc.track_mut(ti + 1).expect("track");
            track
                .notes
                .append_sorted(&batch, count - done)
                .expect("append");
            if shape == "cc" {
                for (k, r) in batch.iter().enumerate() {
                    if k.is_multiple_of(4) && r.start_tick >= cc_tick {
                        cc_tick = r.start_tick;
                        let ev = RawEvent {
                            tick: r.start_tick,
                            anchor: 0,
                            kind: RawKind::Channel {
                                status: 0xB0 | (ti % 16) as u8,
                                data: [1, (k % 128) as u8],
                            },
                        };
                        track.events.push(ev).expect("cc");
                    }
                }
            }
            done += take;
        }
        if shape == "ch16" {
            doc.track_mut(ti + 1)
                .expect("track")
                .infer_subchannels_from_channels();
        }
    }
    if shape == "clips" {
        // 1,000 linked one-bar copies of track 1's clip on every track, one
        // undo step (the Track List's Duplicate): the clip is first cut to its
        // first bar, so each copy plays one bar, not the whole track again.
        let first = doc.track(1).expect("track").id();
        let end = doc.track(1).expect("track").content_end_tick();
        let bar = ppq * 4;
        let mut h = History::new(HistoryLimits::for_system_memory(available_bytes()));
        let cut =
            plumspirit_engine::editor::clips::slice_clips(&doc, &[ClipId::Primary(first)], bar)
                .expect("slice");
        h.apply(&mut doc, cut).expect("apply slice");
        let copies: Vec<(ClipId, usize, u64)> = (0..1000u64)
            .map(|k| {
                (
                    ClipId::Primary(first),
                    1 + (k as usize) % tracks,
                    end + bar + k * bar,
                )
            })
            .collect();
        let op =
            plumspirit_engine::editor::clips::duplicate_clips(&doc, &copies, true).expect("clips");
        h.apply(&mut doc, op).expect("apply clips");
    }
    doc
}

fn run(shape: &str, n: usize, dir: &std::path::Path) {
    println!("## {shape}, {n} notes");
    // About 17 B / note in the store plus chunk slack; the index about 6 B.
    let store = n as u64 * 20;
    if !fits("build", store) {
        return;
    }
    let mut doc = step("build (straight into the stores)", || build(shape, n));
    println!(
        "  heap {:.0} MB ({:.1} B/note)",
        doc.heap_bytes() as f64 / 1_048_576.0,
        doc.heap_bytes() as f64 / n as f64
    );
    let mut rng = Rng(77);

    if fits("analysis index", n as u64 * 8) {
        let idx = step("analysis index build", || AnalysisIndex::for_document(&doc));
        let timing = Timing::for_document(&doc);
        let last = idx.last_tick(&Scope::Project).max(1);
        let mut v = Vec::new();
        for _ in 0..1000 {
            let tick = rng.next() % last;
            let t = Instant::now();
            std::hint::black_box(idx.metrics(&Scope::Project, &timing, tick, DEFAULT_WINDOW_NS));
            v.push(t.elapsed());
        }
        pct("Progressor metrics at a random tick", v);
        step("analysis index teardown", || drop(idx));
    }

    if fits("playback index", n as u64 * 4) {
        let idx = step("playback index build", || {
            Arc::new(PlaybackIndex::build(&doc, 48_000))
        });
        let mut sched = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
        let mut sink = NullSink;
        let mut v = Vec::new();
        for _ in 0..200 {
            let tick = rng.next() % idx.end_tick.max(1);
            let t = Instant::now();
            sched.seek(tick, &mut sink);
            sched.advance(512, &mut sink);
            v.push(t.elapsed());
        }
        pct("seek + 512 frames at a random position", v);
        drop(sched);
        drop(idx);
    }

    {
        let store = &doc.track(1).expect("track").notes;
        let end = store.max_end().unwrap_or(1).max(1);
        let window = match doc.division().kind() {
            plumspirit_engine::model::timing::TimingKind::Ppqn(p) => u64::from(p.max(1)) * 16,
            _ => 960 * 16,
        };
        let mut v = Vec::new();
        for _ in 0..1000 {
            let s = rng.next() % end;
            let t = Instant::now();
            std::hint::black_box(store.query(s, s + window, |_, _| {}));
            v.push(t.elapsed());
        }
        pct("viewport query, 4 bars, track 1", v);
        let mut raster = DensityRaster::new();
        let axis = TimeAxis::new(0, 1600.0 / end as f64);
        let mut v = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            raster.rebuild(store, &axis, 1600, 0, 127);
            v.push(t.elapsed());
        }
        pct("density raster, whole song, 1,600 px", v);
    }

    let mid = dir.join(format!("ladder-{shape}-{n}.mid"));
    let exported = if fits("export", 256 << 20) {
        step("export (plan + write + validate)", || {
            midi::export_to_path(
                &doc,
                &midi::plan_export(&doc),
                &mid,
                ExportControl::default(),
            )
            .map(|r| (r.write_duration, r.validate_duration))
        })
    } else {
        Err(midi::ExportError::Cancelled)
    };
    match &exported {
        Ok((w, v)) => println!(
            "    write {:.0} ms + validate {:.0} ms",
            w.as_secs_f64() * 1e3,
            v.as_secs_f64() * 1e3
        ),
        Err(e) if *e != midi::ExportError::Cancelled => {
            println!("    export refused: {}", e.user_message())
        }
        Err(_) => {}
    }

    let lup = dir.join(format!("ladder-{shape}-{n}.lup"));
    if fits("save .lup", 256 << 20) {
        match step(".lup save (write + verify + rename)", || {
            native::save_project(&doc, &lup, LupControl::default())
        }) {
            Ok(r) => println!(
                "    write {:.0} ms + verify {:.0} ms, {:.0} MB",
                r.write_duration.as_secs_f64() * 1e3,
                r.verify_duration.as_secs_f64() * 1e3,
                r.write.bytes as f64 / 1e6
            ),
            Err(e) => println!("    save failed: {}", e.user_message()),
        }
    }

    // A 10 % move through the history and its undo.
    {
        let tr = doc.track_mut(1).expect("track");
        let end = tr.notes.max_end().unwrap_or(0);
        tr.notes.select_in_region(0, end / 10, 0, 255);
        let mut h = History::new(HistoryLimits::for_system_memory(available_bytes()));
        step("move 10 % of track 1 (+1 beat, through undo)", || {
            let e =
                editor::prepare_move(&doc, 1usize, 960, 0, EditControl::default()).expect("move");
            h.apply(
                &mut doc,
                Operation::Prepared {
                    edit: e,
                    label: "Move",
                },
            )
            .expect("apply")
        });
        step("undo the move", || h.undo(&mut doc).expect("undo"));
    }

    // Reopening: the imported / loaded copies live next to the original.
    if exported.is_ok() && fits("import (a second copy)", store) {
        let (d, _) = step("import the exported file", || {
            midi::import_file(&mid, ImportControl::default()).expect("import")
        });
        step("teardown of the imported copy", || drop(d));
    }
    if lup.exists() && fits("load .lup (a second copy)", store) {
        let (d, _) = step(".lup load", || {
            native::load_project(&lup, LupControl::default()).expect("load")
        });
        assert_eq!(d.note_count(), doc.note_count());
        step("teardown of the loaded copy", || drop(d));
    }
    let _ = std::fs::remove_file(&mid);
    let _ = std::fs::remove_file(&lup);
    step("teardown (drop the project)", || drop(doc));
    println!(
        "  peak RSS {:.0} MB",
        status_kb("VmHWM:").unwrap_or(0) as f64 / 1024.0
    );
}

fn main() {
    let mut sizes = vec![1_000_000usize, 10_000_000, 100_000_000, 250_000_000];
    let mut shapes: Vec<String> = vec!["dense".into()];
    let mut dir = std::env::temp_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sizes" => {
                sizes = args
                    .next()
                    .expect("sizes")
                    .split(',')
                    .map(|s| s.parse().expect("size"))
                    .collect()
            }
            "--shapes" => {
                shapes = args
                    .next()
                    .expect("shapes")
                    .split(',')
                    .map(String::from)
                    .collect()
            }
            "--out" => dir = PathBuf::from(args.next().expect("dir")),
            _ => {}
        }
    }
    println!(
        "threads {} · MemAvailable {:.1} GB",
        std::thread::available_parallelism().map_or(1, |n| n.get()),
        available_bytes().unwrap_or(0) as f64 / 1e9
    );
    for shape in &shapes {
        for &n in &sizes {
            run(shape, n, &dir);
        }
    }
}
