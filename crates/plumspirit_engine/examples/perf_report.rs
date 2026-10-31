//! Performance / memory measurement harness for docs/PERFORMANCE.md.
//!
//! Run each scenario in its own process so memory numbers are not polluted:
//!
//! ```text
//! cargo run --release -p plumspirit_engine --example perf_report -- scenario 1000000 dense
//! cargo run --release -p plumspirit_engine --example perf_report -- chunks 1000000
//! cargo run --release -p plumspirit_engine --example perf_report -- v02 1000000
//! cargo run --release -p plumspirit_engine --example perf_report -- v03 1000000
//! cargo run --release -p plumspirit_engine --example perf_report -- v04 1000000
//! cargo run --release -p plumspirit_engine --example perf_report -- v05 1000000
//! cargo run --release -p plumspirit_engine --example perf_report -- v06 1000000
//! cargo run --release -p plumspirit_engine --example perf_report -- v07 1000000
//! ```
//!
//! Numbers printed here are measured on the machine that runs the command; nothing
//! is hard-coded.

use plumspirit_engine::editor::controllers::{self, ControllerKind};
use plumspirit_engine::editor::{self, Clipboard, EditControl, ExactFactor, commit};
use plumspirit_engine::editor::{QuantizeMode, QuantizeSpec};
use plumspirit_engine::fixtures::{
    FixtureShape, add_dense_controllers, generate, set_dense_tempo, set_many_markers,
};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::midi::{self, ExportControl, ImportControl};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, NoteStore, RankSetBuilder, TempoMap, TempoValue,
};
use plumspirit_engine::native::{self, LupControl};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::lod::DensityRaster;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn proc_status_kb(key: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn mem() -> String {
    match (proc_status_kb("VmRSS:"), proc_status_kb("VmHWM:")) {
        (Some(rss), Some(hwm)) => format!(
            "RSS {:.1} MB (peak {:.1} MB)",
            rss as f64 / 1024.0,
            hwm as f64 / 1024.0
        ),
        _ => "not measured on this platform".into(),
    }
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let s = Instant::now();
    let r = f();
    (r, s.elapsed())
}

fn scenario(notes: usize, shape: FixtureShape) {
    println!("## Scenario: {notes} notes, {shape:?}");
    println!("- baseline: {}", mem());
    let (mut doc, t) = time(|| generate(shape, notes, 960, 42));
    println!("- generate document: {} → {}", ms(t), mem());
    let heap = doc.heap_bytes();
    println!(
        "- document heap: {:.1} MB ({:.2} bytes/note incl. chunk slack)",
        heap as f64 / 1_048_576.0,
        heap as f64 / notes.max(1) as f64
    );

    let dir = std::env::temp_dir().join(format!("plumspirit-perf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("fixture.mid");
    let (report, t) = time(|| {
        midi::export_to_path(
            &doc,
            &midi::plan_export(&doc),
            &path,
            ExportControl::default(),
        )
        .expect("export")
    });
    println!(
        "- export (write {} + validate {}): total {}; file {:.1} MB; peak polyphony {}",
        ms(report.write_duration),
        ms(report.validate_duration),
        ms(t),
        report.write.bytes as f64 / 1_048_576.0,
        report.write.peak_polyphony
    );
    println!("- after export: {}", mem());
    drop(doc);

    let ((doc2, summary), t) =
        time(|| midi::import_file(&path, ImportControl::default()).expect("import"));
    doc = doc2;
    println!("- import: {} ({}) → {}", ms(t), summary.describe(), mem());
    let track = 1usize;
    let store = &doc.track(track).expect("track").notes;
    let end = store.max_end().unwrap_or(0);

    // Viewport queries: a 4-bar window at random positions, 1600 px wide.
    let bar = 960 * 4u64;
    let mut rng = 12345u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let iters = 500;
    let mut matched = 0usize;
    let mut scanned = 0usize;
    let (_, t) = time(|| {
        for _ in 0..iters {
            let s = next() % end.max(1);
            let st = store.query(s, s + bar * 4, |_, _| {});
            matched += st.notes_matched;
            scanned += st.notes_scanned;
        }
    });
    println!(
        "- viewport query (4 bars): {} avg; {} matched / {} scanned avg",
        ms(t / iters),
        matched / iters as usize,
        scanned / iters as usize
    );
    let (est, t) = time(|| store.estimate_in_range(0, end));
    println!("- LOD estimate over full range: {} (estimate {est})", ms(t));
    let (full, t) = time(|| store.count_in_range(0, end));
    println!(
        "- full-range query (every note visible): {} ({full} notes)",
        ms(t)
    );
    let mut raster = DensityRaster::new();
    let axis = TimeAxis::new(0, 1600.0 / end.max(1) as f64);
    raster.rebuild(store, &axis, 1600, 0, 127); // warm buffers
    let (_, t) = time(|| {
        for _ in 0..5 {
            raster.rebuild(store, &axis, 1600, 0, 127);
        }
    });
    println!(
        "- LOD density raster, whole song in 1600 columns: {} avg (buffers {:.1} MB, reused)",
        ms(t / 5),
        raster.heap_bytes() as f64 / 1_048_576.0
    );
    let axis_zoom = TimeAxis::new(end / 2, 1600.0 / (bar * 4) as f64);
    let (_, t) = time(|| raster.rebuild(store, &axis_zoom, 1600, 0, 127));
    println!("- LOD raster, 4-bar window: {}", ms(t));

    // Editing on the large track.
    let mid = end / 2;
    let tr = doc.track_mut(track).expect("track");
    let (n, t) = time(|| tr.notes.select_in_region(mid, mid + bar, 0, 127));
    println!("- box select 1 bar: {} ({n} notes)", ms(t));
    let (clip, t) = time(|| {
        Clipboard::copy_selected(tr)
            .expect("copy")
            .expect("selection")
    });
    println!(
        "- copy selection: {} ({} notes, {:.1} KB)",
        ms(t),
        clip.len(),
        clip.heap_bytes() as f64 / 1024.0
    );
    let (_, t) = time(|| {
        let e = clip
            .prepare_paste(&doc, track, mid + bar * 8)
            .expect("paste");
        commit(&mut doc, e).expect("commit");
    });
    println!("- paste selection: {}", ms(t));
    let attrs = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).expect("attrs");
    let (_, t) = time(|| {
        editor::insert_note(
            &mut doc,
            track,
            NoteRecord {
                start_tick: mid + 7,
                duration_ticks: 240,
                attrs,
                sub: Default::default(),
                group: 0,
            },
        )
        .expect("insert")
    });
    println!("- insert one note (mid-track): {}", ms(t));
    let (_, t) = time(|| editor::delete_selected(&mut doc, track).expect("delete"));
    println!("- delete one note: {}", ms(t));

    // 10% subset transforms.
    let tr = doc.track_mut(track).expect("track");
    tr.notes.clear_selection();
    tr.notes.select_in_region(0, end / 10, 0, 127);
    let subset = tr.notes.selected_count();
    let (_, t) = time(|| editor::flip_y(&mut doc, track).expect("flip y"));
    println!("- Flip Y on {subset} notes: {}", ms(t));
    let (p, tp) =
        time(|| editor::prepare_flip_x(&doc, track, EditControl::default()).expect("prep"));
    let (_, tc) = time(|| commit(&mut doc, p).expect("commit"));
    println!(
        "- Flip X on {subset} notes: prepare {} + commit {}",
        ms(tp),
        ms(tc)
    );
    // All notes: Scale X 2 then 0.5
    doc.track_mut(track).expect("track").notes.select_all();
    let all = doc.track(track).expect("track").notes.selected_count();
    let two = ExactFactor::parse("2").expect("factor");
    let (p, tp) =
        time(|| editor::prepare_scale_x(&doc, track, &two, EditControl::default()).expect("prep"));
    let (_, tc) = time(|| commit(&mut doc, p).expect("commit"));
    println!(
        "- Scale X ×2 on all {all} notes: prepare {} + commit {}",
        ms(tp),
        ms(tc)
    );
    let odd = ExactFactor::parse("0.3333333333333333333333333").expect("factor");
    let (p, tp) =
        time(|| editor::prepare_scale_x(&doc, track, &odd, EditControl::default()).expect("prep"));
    let (_, tc) = time(|| commit(&mut doc, p).expect("commit"));
    println!(
        "- Scale X ×0.333…(25 digits, 256-bit path) on all notes: prepare {} + commit {}",
        ms(tp),
        ms(tc)
    );
    let (_, t) = time(|| {
        editor::scale_y(&mut doc, track, &ExactFactor::parse("-1").expect("f")).expect("scale y")
    });
    println!("- Scale Y ×-1 on all notes: {}", ms(t));
    let (_, t) = time(|| doc.track_mut(track).expect("track").notes.select_all());
    println!("- Select All: {}", ms(t));
    let (n, t) = time(|| editor::delete_selected(&mut doc, track).expect("delete"));
    println!("- delete all {n} notes (batch compaction): {}", ms(t));
    println!("- end: {}", mem());
    let _ = std::fs::remove_dir_all(&dir);
    println!();
}

fn chunk_benchmark(notes: usize) {
    println!("## Chunk size benchmark ({notes} notes)");
    println!(
        "| chunk | build | 20k single inserts | 2000 queries | select 10% + delete | heap MB |"
    );
    println!("|---|---|---|---|---|---|");
    let doc = generate(FixtureShape::Dense, notes, 960, 7);
    let recs: Vec<NoteRecord> = doc
        .track(1)
        .expect("t")
        .notes
        .iter()
        .map(|v| v.record())
        .collect();
    let end = recs.last().map_or(0, |r| r.start_tick);
    for target in [1024usize, 2048, 4096, 8192, 16384] {
        let mut s = NoteStore::with_chunk_target(target);
        let (_, tb) = time(|| s.insert_sorted_batch(&recs, false).expect("build"));
        let mut rng = 99u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let (_, ti) = time(|| {
            for _ in 0..20_000 {
                let r = NoteRecord {
                    start_tick: next() % end.max(1),
                    duration_ticks: 10,
                    attrs: recs[0].attrs,
                    sub: Default::default(),
                    group: 0,
                };
                s.insert(r, false).expect("insert");
            }
        });
        let (_, tq) = time(|| {
            for _ in 0..2000 {
                let a = next() % end.max(1);
                s.query(a, a + 3840 * 4, |_, _| {});
            }
        });
        let (_, td) = time(|| {
            s.select_in_region(end / 3, end / 3 + end / 10, 0, 127);
            s.remove_selected()
        });
        println!(
            "| {target} | {} | {} | {} | {} | {:.1} |",
            ms(tb),
            ms(ti),
            ms(tq),
            ms(td),
            s.heap_bytes() as f64 / 1_048_576.0
        );
    }
    println!();
}

/// Counts what the scheduler emits (no synthesis).
#[derive(Default)]
struct CountSink {
    ons: u64,
    offs: u64,
    other: u64,
}

impl SynthSink for CountSink {
    fn note_on(&mut self, _: u8, _: u8, _: u8) {
        self.ons += 1;
    }
    fn note_off(&mut self, _: u8, _: u8) {
        self.offs += 1;
    }
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {
        self.other += 1;
    }
    fn silence(&mut self) {}
    fn reset(&mut self) {}
}

/// v0.2 additions: native project files, playback scheduling, undo history.
fn scenario_v02(notes: usize) {
    println!("## v0.2 scenario: {notes} notes, Dense");
    println!("- baseline: {}", mem());
    let (mut doc, t) = time(|| generate(FixtureShape::Dense, notes, 960, 42));
    println!("- generate document: {} → {}", ms(t), mem());
    let heap = doc.heap_bytes();
    println!(
        "- document heap: {:.1} MB ({:.2} bytes/note incl. chunk slack)",
        heap as f64 / 1_048_576.0,
        heap as f64 / notes.max(1) as f64
    );
    let dir = std::env::temp_dir().join(format!("plumspirit-perf02-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("fixture.lup");
    let (r, t) = time(|| native::save_project(&doc, &path, LupControl::default()).expect("save"));
    println!(
        "- .lup save (write {} + verify {} + rename): total {}; file {:.1} MB ({:.2} bytes/note)",
        ms(r.write_duration),
        ms(r.verify_duration),
        ms(t),
        r.write.bytes as f64 / 1_048_576.0,
        r.write.bytes as f64 / notes.max(1) as f64
    );
    let (_, t) = time(|| native::save_project(&doc, &path, LupControl::default()).expect("save"));
    println!("- .lup save again (replacing the file): {}", ms(t));
    let fp = doc.state_fingerprint();
    drop(doc);
    println!("- after dropping the document: {}", mem());
    let ((d2, rep), t) = time(|| native::load_project(&path, LupControl::default()).expect("load"));
    doc = d2;
    assert_eq!(doc.state_fingerprint(), fp, "reloaded project differs");
    println!(
        "- .lup load: {} ({} notes, {} events, identical fingerprint) → {}",
        ms(t),
        rep.notes,
        rep.events,
        mem()
    );

    // Playback index: shares the note chunks (no copy of the notes).
    let before = proc_status_kb("VmRSS:");
    let (idx, t) = time(|| Arc::new(PlaybackIndex::build(&doc, 48_000)));
    let after = proc_status_kb("VmRSS:");
    println!(
        "- playback index build: {} (control index {:.1} KB; RSS change {})",
        ms(t),
        idx.controls.iter().map(|c| c.heap_bytes()).sum::<usize>() as f64 / 1024.0,
        match (before, after) {
            (Some(a), Some(b)) => format!("{:+.1} MB", (b as f64 - a as f64) / 1024.0),
            _ => "n/a".into(),
        }
    );
    let end_frame = idx.tempo.frame_at_tick(idx.end_tick);
    let mut sched = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
    let mut sink = CountSink::default();
    sched.seek(0, &mut sink);
    let (_, t) = time(|| {
        while !sched.finished() {
            sched.advance(64, &mut sink);
        }
    });
    let audio_s = end_frame as f64 / 48_000.0;
    let st = sched.stats();
    println!(
        "- scheduling the whole song ({audio_s:.1} s of audio): {} = {:.4}% of real time; {} note-ons, {} skipped, {} offs, peak pending {}",
        ms(t),
        t.as_secs_f64() / audio_s * 100.0,
        st.note_ons,
        st.note_ons_skipped,
        st.note_offs,
        st.peak_pending_offs
    );
    let mid_tick = idx.end_tick / 2;
    let (_, t) = time(|| {
        for i in 0..100u64 {
            sched.seek(mid_tick / 100 * i, &mut sink);
        }
    });
    println!("- seek (100 random positions): {} avg", ms(t / 100));
    drop(sched);
    drop(idx);

    // Undo history on the large track.
    let track = 1usize;
    let mut h = History::new(HistoryLimits::for_system_memory(None));
    doc.track_mut(track).expect("track").notes.select_all();
    let all = doc.track(track).expect("track").notes.selected_count();
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::SetVelocity {
                track: track.into(),
                velocity: 64,
            },
        )
        .expect("velocity")
    });
    println!(
        "- velocity edit on all {all} notes: {}; undo data {:.2} MB ({:.2} bytes/note)",
        ms(t),
        h.bytes() as f64 / 1_048_576.0,
        h.bytes() as f64 / all.max(1) as f64
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo velocity edit: {}", ms(t));
    let (_, t) = time(|| h.redo(&mut doc).expect("redo"));
    println!("- redo velocity edit: {}", ms(t));
    let b0 = h.bytes();
    let (edit, tp) =
        time(|| editor::prepare_move(&doc, track, 960, 1, EditControl::default()).expect("move"));
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::Prepared {
                edit,
                label: "Move Notes",
            },
        )
        .expect("apply")
    });
    println!(
        "- move all notes (+1 beat, +1 semitone): prepare {} + apply {}; undo data {:.2} MB ({:.2} bytes/note)",
        ms(tp),
        ms(t),
        (h.bytes() - b0) as f64 / 1_048_576.0,
        (h.bytes() - b0) as f64 / all.max(1) as f64
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo move: {}", ms(t));
    // (A new edit drops the redo entry, so measure it in its own history.)
    let mut h = History::new(HistoryLimits::for_system_memory(None));
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::DeleteSelected {
                track: track.into(),
            },
        )
        .expect("delete")
    });
    println!(
        "- delete all notes: {}; undo data {:.2} MB ({:.2} bytes/note)",
        ms(t),
        h.bytes() as f64 / 1_048_576.0,
        h.bytes() as f64 / all.max(1) as f64
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo delete (all notes back): {}", ms(t));
    assert_eq!(doc.track(track).expect("track").notes.len(), all);
    println!("- end: {}", mem());
    let _ = std::fs::remove_dir_all(&dir);
    println!();
}

/// v0.3 additions: Conductor (dense tempo automation, markers), ramp export,
/// controllers, quantize, erase, numeric Gate / Velocity.
fn scenario_v03(notes: usize) {
    println!("## v0.3 scenario: {notes} notes, Dense");
    println!("- baseline: {}", mem());
    let ppq = 960u64;
    let (mut doc, t) = time(|| generate(FixtureShape::Dense, notes, 960, 42));
    println!("- generate document: {} → {}", ms(t), mem());
    let track = 1usize;
    let song_end = doc.content_end_tick();

    // --- Dense tempo automation: 10,000 points, half of them linear ramps.
    let points = 10_000usize;
    let spacing = (song_end / points as u64).max(ppq);
    let (_, t) = time(|| set_dense_tempo(&mut doc, points, spacing, 7));
    println!(
        "- dense tempo: {points} points ({} ramps) every {spacing} ticks, installed in {}",
        points / 2,
        ms(t)
    );
    let (map, t) = time(|| TempoMap::for_document(&doc, 48_000));
    println!(
        "- tempo map build ({} segments): {}",
        map.segment_count(),
        ms(t)
    );
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let ticks: Vec<u64> = (0..1_000_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % song_end.max(1)
        })
        .collect();
    let (sum, t) = time(|| {
        ticks
            .iter()
            .map(|&k| map.frame_at_tick(k))
            .fold(0u64, u64::wrapping_add)
    });
    println!(
        "- tempo evaluation, tick → frame: 1,000,000 lookups in {} ({:.0} ns each; checksum {})",
        ms(t),
        t.as_nanos() as f64 / 1e6,
        sum % 1000
    );
    let end_frame = map.frame_at_tick(song_end);
    let frames: Vec<u64> = ticks.iter().map(|&k| k % end_frame.max(1)).collect();
    let (sum, t) = time(|| {
        frames
            .iter()
            .map(|&f| map.tick_at_frame(f))
            .fold(0u64, u64::wrapping_add)
    });
    println!(
        "- tempo evaluation, frame → tick: 1,000,000 lookups in {} ({:.0} ns each; checksum {})",
        ms(t),
        t.as_nanos() as f64 / 1e6,
        sum % 1000
    );
    drop(map);

    // --- Conductor editing on the big project: notes are not touched.
    let mut h = History::new(HistoryLimits::for_system_memory(None));
    let notes_rev = doc.track(track).expect("track").notes.revision();
    let mid = song_end / 2 + 17;
    let (edit, tp) = time(|| {
        doc.conductor()
            .prepare_set_tempo(
                None,
                mid,
                TempoValue::Bpm(Bpm::parse("123.456").expect("bpm")),
                Interp::Linear,
            )
            .expect("tempo")
    });
    let (_, ta) = time(|| {
        h.apply(
            &mut doc,
            Operation::Conductor {
                edit,
                label: "Add Tempo Point",
            },
        )
        .expect("apply")
    });
    assert_eq!(
        doc.track(track).expect("track").notes.revision(),
        notes_rev,
        "a tempo edit touched the notes"
    );
    let (_, tu) = time(|| h.undo(&mut doc).expect("undo"));
    let (_, tr) = time(|| h.redo(&mut doc).expect("redo"));
    println!(
        "- add one tempo point to the {}-point map: prepare {} + apply {}; undo {}, redo {}; undo data {} bytes; notes untouched",
        points,
        ms(tp),
        ms(ta),
        ms(tu),
        ms(tr),
        h.bytes()
    );

    // --- Markers: 100,000 (half chapters).
    let (_, t) = time(|| set_many_markers(&mut doc, 100_000, (song_end / 100_000).max(1)));
    println!(
        "- 100,000 markers (50,000 chapters) installed in {} → {}",
        ms(t),
        mem()
    );
    let c = doc.conductor();
    let window = ppq * 16;
    let (count, t) = time(|| {
        let mut n = 0usize;
        for i in 0..1000u64 {
            let t0 = song_end / 1000 * i;
            n += c.markers_in_range(t0, t0 + window).count();
        }
        n
    });
    println!(
        "- marker query for a 4-bar window: {} avg ({} markers per window avg)",
        ms(t / 1000),
        count / 1000
    );

    let (edit, tp) = time(|| {
        doc.conductor()
            .prepare_set_marker(None, song_end / 3, None, "New marker")
            .expect("marker")
    });
    let (_, ta) = time(|| {
        h.apply(
            &mut doc,
            Operation::Conductor {
                edit,
                label: "Add Marker",
            },
        )
        .expect("apply")
    });
    let (_, tu) = time(|| h.undo(&mut doc).expect("undo"));
    println!(
        "- add one marker among 100,000: prepare {} + apply {}; undo {}",
        ms(tp),
        ms(ta),
        ms(tu)
    );
    let (edit, tp) = time(|| {
        doc.conductor()
            .prepare_set_tempo(
                None,
                song_end / 3 + 1,
                TempoValue::Bpm(Bpm::parse("99").expect("bpm")),
                Interp::Step,
            )
            .expect("tempo")
    });
    let (_, ta) = time(|| {
        h.apply(
            &mut doc,
            Operation::Conductor {
                edit,
                label: "Add Tempo Point",
            },
        )
        .expect("apply")
    });
    println!(
        "- add one tempo point with 100,000 markers present: prepare {} + apply {}",
        ms(tp),
        ms(ta)
    );

    // --- Dense controllers: 200,000 pitch bend + pan events.
    let (_, t) =
        time(|| add_dense_controllers(&mut doc, track, 200_000, (song_end / 200_000).max(1), 3));
    println!("- 200,000 controller events added in {} → {}", ms(t), mem());
    let events = &doc.track(track).expect("track").events;
    let (pts, t) = time(|| controllers::collect_points(events, ControllerKind::PitchBend, 0));
    println!(
        "- Pitch Bend lane: collect {} points (once per events revision): {}",
        pts.len(),
        ms(t)
    );
    let target = pts[pts.len() / 2];
    let (edit, tp) = time(|| {
        controllers::prepare_set(
            doc.track(track).expect("track"),
            ControllerKind::PitchBend,
            0,
            Some(target.index),
            target.tick,
            12_000,
        )
        .expect("set")
    });
    let (_, ta) = time(|| {
        h.apply(
            &mut doc,
            Operation::Events {
                edit,
                label: "Set Pitch Bend",
            },
        )
        .expect("apply")
    });
    println!(
        "- edit one Pitch Bend point among 100,000: prepare {} + apply {}",
        ms(tp),
        ms(ta)
    );
    drop(pts);

    // --- Export with ramps, markers and controllers.
    let (plan, t) = time(|| midi::plan_export(&doc));
    println!(
        "- export plan: {} (tempo events {}, of them {} ramp steps for {} ramps; max ramp error {:.3} ms at tolerance {:.3} ms; {} markers, {} chapters)",
        ms(t),
        plan.tempo.events,
        plan.tempo.ramp_events,
        plan.tempo.ramps,
        plan.tempo.max_ramp_error_ms,
        plan.tempo.tolerance_ms,
        plan.markers,
        plan.chapters
    );
    let dir = std::env::temp_dir().join(format!("plumspirit-perf03-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mid_path = dir.join("fixture.mid");
    let (r, t) = time(|| {
        midi::export_to_path(&doc, &plan, &mid_path, ExportControl::default()).expect("export")
    });
    println!(
        "- MIDI export (write + validate + rename): {}; file {:.1} MB",
        ms(t),
        r.write.bytes as f64 / 1_048_576.0
    );
    let ((back, _), t) =
        time(|| midi::import_file(&mid_path, ImportControl::default()).expect("import"));
    println!(
        "- MIDI re-import: {} ({} tempo points, {} markers, {} notes; document heap {:.1} MB) → {}",
        ms(t),
        back.conductor().tempo_points().len(),
        back.conductor().markers().len(),
        back.note_count(),
        back.heap_bytes() as f64 / 1_048_576.0,
        mem()
    );
    drop(back);
    println!("- after dropping the re-imported copy: {}", mem());
    let lup = dir.join("fixture.lup");
    let (r, t) = time(|| native::save_project(&doc, &lup, LupControl::default()).expect("save"));
    println!(
        "- .lup save (write + verify + rename): {}; file {:.1} MB",
        ms(t),
        r.write.bytes as f64 / 1_048_576.0
    );
    let fp = doc.state_fingerprint();
    let ((d2, _), t) = time(|| native::load_project(&lup, LupControl::default()).expect("load"));
    assert_eq!(d2.state_fingerprint(), fp, "reloaded project differs");
    println!("- .lup load: {} (identical fingerprint) → {}", ms(t), mem());
    drop(d2);

    // --- Playback with dense tempo automation and controllers.
    let (idx, t) = time(|| Arc::new(PlaybackIndex::build(&doc, 48_000)));
    println!(
        "- playback index build: {} (control index {:.1} KB) → {}",
        ms(t),
        idx.controls.iter().map(|c| c.heap_bytes()).sum::<usize>() as f64 / 1024.0,
        mem()
    );
    let end_frame = idx.tempo.frame_at_tick(idx.end_tick);
    let mut sched = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
    let mut sink = CountSink::default();
    sched.seek(0, &mut sink);
    let (_, t) = time(|| {
        while !sched.finished() {
            sched.advance(64, &mut sink);
        }
    });
    let audio_s = end_frame as f64 / 48_000.0;
    println!(
        "- scheduling the whole song ({audio_s:.1} s of audio, {} tempo points): {} = {:.4}% of real time; {} note-ons, {} other channel messages",
        doc.conductor().tempo_points().len(),
        ms(t),
        t.as_secs_f64() / audio_s * 100.0,
        sink.ons,
        sink.other
    );
    let (_, t) = time(|| {
        for i in 0..100u64 {
            sched.seek(idx.end_tick / 100 * i, &mut sink);
        }
    });
    println!("- seek (100 positions): {} avg", ms(t / 100));
    drop(sched);
    drop(idx);

    // --- Quantize every note (Both, 1/16 grid). Undo budget of a 2 GB PC
    // (128 MB): quantizing 5M notes needs ~70 MB, more than the 64 MB minimum.
    let budget = HistoryLimits::for_system_memory(Some(2 << 30));
    let mut h = History::new(budget);
    doc.track_mut(track).expect("track").notes.select_all();
    let all = doc.track(track).expect("track").notes.selected_count();
    let spec = QuantizeSpec {
        mode: QuantizeMode::Both,
        grid: ppq / 4,
        origin: 0,
    };
    let (preview, t) = time(|| editor::preview_quantize(&doc, track, &spec));
    println!(
        "- quantize preview (Both, 1/16) of {all} notes: {} ({} would change)",
        ms(t),
        preview.changed
    );
    let (edit, tp) = time(|| {
        editor::prepare_quantize(&doc, track, &spec, EditControl::default()).expect("quantize")
    });
    let (_, ta) = time(|| {
        h.apply(
            &mut doc,
            Operation::Prepared {
                edit,
                label: "Quantize",
            },
        )
        .expect("apply")
    });
    println!(
        "- quantize all {all} notes: prepare {} + apply {}; undo data {:.2} MB ({:.2} bytes/note)",
        ms(tp),
        ms(ta),
        h.bytes() as f64 / 1_048_576.0,
        h.bytes() as f64 / all.max(1) as f64
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo quantize: {}", ms(t));

    // --- Numeric Gate / Velocity on every note.
    let mut h = History::new(budget);
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::SetDuration {
                track: track.into(),
                duration: 1_000_000,
            },
        )
        .expect("gate")
    });
    println!(
        "- Gate = 1,000,000 ticks on all {all} notes: {}; undo data {:.2} MB",
        ms(t),
        h.bytes() as f64 / 1_048_576.0
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo Gate: {}", ms(t));
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::SetVelocity {
                track: track.into(),
                velocity: 1,
            },
        )
        .expect("velocity")
    });
    println!("- Velocity = 1 on all {all} notes: {}", ms(t));

    // --- Erase: one sweep over a bar, then 10% of the notes in one gesture.
    doc.track_mut(track).expect("track").notes.clear_selection();
    let mut h = History::new(budget);
    let bar0 = song_end / 2;
    let (ranks, tq) = time(|| {
        // As the app does: notes hit by the sweep → ranks → one rank set.
        let store = &doc.track(track).expect("track").notes;
        let mut locs = Vec::new();
        store.query(bar0, bar0 + ppq * 4, |loc, v| {
            if v.start_tick >= bar0 && v.start_tick < bar0 + ppq * 4 {
                locs.push(loc);
            }
        });
        let mut ranks: Vec<usize> = locs.iter().filter_map(|&l| store.rank_of(l)).collect();
        ranks.sort_unstable();
        let mut b = RankSetBuilder::default();
        for &r in &ranks {
            b.push(r);
        }
        (b.finish(), ranks.len())
    });
    let (rs, n) = ranks;
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::EraseNotes {
                track: track.into(),
                ranks: rs,
            },
        )
        .expect("erase")
    });
    println!(
        "- erase sweep over one bar ({n} notes): hit test {} + erase {}",
        ms(tq),
        ms(t)
    );
    h.undo(&mut doc).expect("undo");
    let mut b = RankSetBuilder::default();
    for r in (0..all).step_by(10) {
        b.push(r);
    }
    let rs = b.finish();
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::EraseNotes {
                track: track.into(),
                ranks: rs,
            },
        )
        .expect("erase")
    });
    println!(
        "- erase every 10th note ({} notes) in one gesture: {}; undo data {:.2} MB",
        all.div_ceil(10),
        ms(t),
        h.bytes() as f64 / 1_048_576.0
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo erase: {}", ms(t));
    assert_eq!(doc.track(track).expect("track").notes.len(), all);
    println!("- end: {}", mem());
    let _ = std::fs::remove_dir_all(&dir);
    println!();
}

/// v0.4 additions: 1M controller events, graph point collection, curve
/// generation, track corrections on export, loop scheduling, live Mute / Solo,
/// range selection, time conversion, Save / Reopen and export of it all.
fn scenario_v04(notes: usize) {
    use plumspirit_engine::editor::controllers::{WritePolicy, collect_points};
    use plumspirit_engine::editor::curves::{
        Bend, CurveSpec, Interval, Shape, event_budget, interval_ticks,
    };
    use plumspirit_engine::midi::{ExportOptions, export_to_bytes_with, plan_export_with};
    use plumspirit_engine::model::duration::{TimeBasis, TimeMap};
    use plumspirit_engine::model::{GatePercent, TrackProps};
    use plumspirit_engine::playback::{PlaybackSetup, eligibility};
    use plumspirit_engine::view::grid::MeterMap;
    println!("## v0.4 scenario: {notes} notes, Dense, + 1,000,000 controller events");
    println!("- baseline: {}", mem());
    let (mut doc, t) = time(|| generate(FixtureShape::Dense, notes, 960, 42));
    println!("- generate document: {} → {}", ms(t), mem());
    let track = 1usize;
    let end = doc.content_end_tick().max(1);
    let (_, t) =
        time(|| add_dense_controllers(&mut doc, track, 1_000_000, (end / 1_000_000).max(1), 3));
    println!(
        "- add 1,000,000 controller events (Pitch Bend + Pan): {} → {}",
        ms(t),
        mem()
    );
    let tr = doc.track(track).expect("track");
    let (pb, t) = time(|| collect_points(&tr.events, ControllerKind::PitchBend, 0));
    println!(
        "- graph point collection (Pitch Bend, once per events revision): {} points in {}",
        pb.len(),
        ms(t)
    );
    let (pan, t) = time(|| collect_points(&tr.events, ControllerKind::PAN, 0));
    println!(
        "- graph point collection (Pan): {} points in {}",
        pan.len(),
        ms(t)
    );
    drop((pb, pan));
    let setup = PlaybackSetup::for_document(&doc);
    let (controls, t) = time(|| setup.control_indexes(&doc));
    println!(
        "- control index build (1 route): {} ({:.1} MB)",
        ms(t),
        controls.iter().map(|c| c.heap_bytes()).sum::<usize>() as f64 / 1_048_576.0
    );
    let tempo = Arc::new(TempoMap::for_document(&doc, 48_000));
    let (idx, t) = time(|| {
        Arc::new(PlaybackIndex::new(
            &doc,
            controls,
            Arc::clone(&tempo),
            &setup,
        ))
    });
    println!("- playback index (notes snapshot + controls): {}", ms(t));

    // --- Curve generation: 100,000 CC 7 events replacing a range, one undo step.
    let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
    h.reset(true);
    let budget = event_budget(h.limits().max_bytes);
    let meter = MeterMap::new(doc.division(), &doc.meter_events(0));
    let span = end.min(100_000 * 48);
    let ((ticks, pts), t) = time(|| {
        let ticks = interval_ticks(0, span, Interval::Ticks(48), &meter, budget).expect("ticks");
        let spec = CurveSpec {
            shape: Shape::SCurve,
            bend: Bend::EaseOut,
            from: (0, 0.0),
            to: (span, 127.0),
            seed: 1,
            random_range: (0.0, 127.0),
            custom: None,
            clamp: false,
            normalize: false,
            limits: (0, 127),
        };
        let pts: Vec<(u64, u16)> = spec
            .sample(&ticks)
            .expect("sample")
            .into_iter()
            .map(|(t, v)| (t, v as u16))
            .collect();
        (ticks.len(), pts)
    });
    println!(
        "- S-curve sampling: {ticks} events in {} (budget {})",
        ms(t),
        budget
    );
    let (edit, t) = time(|| {
        controllers::prepare_points(
            doc.track(track).unwrap(),
            ControllerKind::VOLUME,
            0,
            &pts,
            WritePolicy::Replace,
            budget,
        )
        .expect("prepare")
    });
    println!(
        "- curve edit prepared (events list of {} entries): {}",
        doc.track(track).unwrap().events.len(),
        ms(t)
    );
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::Events {
                edit,
                label: "S Curve",
            },
        )
        .expect("apply")
    });
    println!(
        "- curve applied (one undo step, {:.1} MB undo): {} → {}",
        h.bytes() as f64 / 1_048_576.0,
        ms(t),
        mem()
    );
    let (_, t) = time(|| h.undo(&mut doc).expect("undo"));
    println!("- undo curve: {}", ms(t));
    let (_, t) = time(|| h.redo(&mut doc).expect("redo"));
    println!("- redo curve: {}", ms(t));

    // --- Track corrections: key +12, tick +120, gate 50 % on the 1M-note track.
    let id = doc.track(track).unwrap().id();
    let props = TrackProps {
        key_correction: 12,
        tick_correction: 120,
        gate: GatePercent::parse("50").unwrap(),
        ..TrackProps::default()
    };
    let (_, t) = time(|| {
        h.apply(
            &mut doc,
            Operation::TrackProps {
                track: id,
                props,
                end: None,
                label: "Track Properties",
            },
        )
        .expect("props")
    });
    println!("- Track Properties change (no note copy): {}", ms(t));
    let (plan, t) = time(|| {
        plan_export_with(
            &doc,
            ExportOptions {
                apply_corrections: true,
                ..ExportOptions::default()
            },
        )
    });
    println!(
        "- export plan with corrections (preflight of every note): {}",
        ms(t)
    );
    let (bytes, t) = time(|| export_to_bytes_with(&doc, &plan).expect("export"));
    println!(
        "- MIDI export with corrections: {} ({:.1} MB)",
        ms(t),
        bytes.len() as f64 / 1_048_576.0
    );
    let plan0 = plan_export_with(
        &doc,
        ExportOptions {
            apply_corrections: false,
            ..ExportOptions::default()
        },
    );
    let (bytes0, t) = time(|| export_to_bytes_with(&doc, &plan0).expect("export"));
    println!(
        "- MIDI export without corrections: {} ({:.1} MB)",
        ms(t),
        bytes0.len() as f64 / 1_048_576.0
    );
    drop((bytes, bytes0, plan, plan0));

    // --- Loop scheduling: 60 s of audio looping 4 bars of the densest part.
    let bar = 960 * 4;
    let setup = PlaybackSetup::for_document(&doc);
    let idx2 = Arc::new(PlaybackIndex::new(
        &doc,
        setup.control_indexes(&doc),
        Arc::clone(&tempo),
        &setup,
    ));
    drop(idx);
    let mut sched = Scheduler::new(Arc::clone(&idx2), SchedulerLimits::default());
    sched.set_loop(Some((bar * 8, bar * 12)));
    let mut sink = CountSink::default();
    sched.seek(bar * 8, &mut sink);
    let blocks = 48_000 * 60 / 64;
    let (_, t) = time(|| {
        for _ in 0..blocks {
            sched.advance(64, &mut sink);
        }
    });
    let st = sched.stats();
    println!(
        "- loop scheduling, 60 s of audio: {} ({:.2}% of real time); {} wraps, {} note-ons, {} skipped, {} pending offs at the end",
        ms(t),
        t.as_secs_f64() / 60.0 * 100.0,
        st.loop_wraps,
        st.note_ons,
        st.note_ons_skipped,
        sched.pending_offs()
    );
    // --- Live Mute / Solo while notes sound.
    doc.track_mut(track).unwrap().mute = true;
    let mask: Arc<[bool]> = eligibility(&doc).into();
    let (_, t) = time(|| sched.set_eligible(Arc::clone(&mask), &mut sink));
    println!("- live Mute of the playing track: {}", ms(t));
    doc.track_mut(track).unwrap().mute = false;
    drop(sched);

    // --- Range selection over the 1M-note track.
    let (n, t) = time(|| {
        let s = &mut doc.track_mut(track).unwrap().notes;
        s.clear_selection();
        s.select_range(end / 4, end * 3 / 4, false)
    });
    println!(
        "- Select Range (onsets, middle half): {n} notes in {}",
        ms(t)
    );
    let (n, t) = time(|| {
        let s = &mut doc.track_mut(track).unwrap().notes;
        s.clear_selection();
        s.select_range(end / 4, end * 3 / 4, true)
    });
    println!("- Select Range (intersecting): {n} notes in {}", ms(t));
    doc.track_mut(track).unwrap().notes.clear_selection();

    // --- Time conversion with 10,000 tempo points (half ramps).
    set_dense_tempo(&mut doc, 10_000, (end / 10_000).max(960), 7);
    let (tm, t) = time(|| TimeMap::for_document(&doc, TimeBasis::Musical));
    println!("- Playing Time map build (10,000 tempo points): {}", ms(t));
    let (sum, t) = time(|| {
        (0..100_000u64)
            .map(|i| tm.nanos_at(i * (end / 100_000).max(1)))
            .fold(0u64, u64::wrapping_add)
    });
    println!(
        "- tick → time: 100,000 conversions in {} (checksum {sum})",
        ms(t)
    );
    let total = tm.nanos_at(end);
    let (sum, t) = time(|| {
        (0..100_000u64)
            .map(|i| tm.tick_at_nanos(i * (total / 100_000).max(1)))
            .fold(0u64, u64::wrapping_add)
    });
    println!(
        "- time → tick: 100,000 conversions in {} (checksum {sum})",
        ms(t)
    );

    // --- Save / Reopen with everything.
    let dir = std::env::temp_dir().join(format!("plumspirit-perf-v04-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lup = dir.join("v04.lup");
    let (r, t) = time(|| native::save_project(&doc, &lup, LupControl::default()).expect("save"));
    println!(
        "- .lup save (write + verify + rename): {}; file {:.1} MB",
        ms(t),
        r.write.bytes as f64 / 1_048_576.0
    );
    let fp = doc.state_fingerprint();
    let ((d2, _), t) = time(|| native::load_project(&lup, LupControl::default()).expect("load"));
    assert_eq!(d2.state_fingerprint(), fp);
    println!("- .lup load: {} (identical fingerprint) → {}", ms(t), mem());
    drop(d2);
    let _ = std::fs::remove_dir_all(&dir);
    println!("- end: {}", mem());
    println!();
}

/// Dense fixture records with `n_subs` subchannels: note `i` goes to subchannel
/// `i % n_subs` on MIDI channel `sub % 16` (1 = every note on the main
/// subchannel, channel 1).
fn sub_records(notes: usize, n_subs: u32) -> Vec<NoteRecord> {
    let doc = generate(FixtureShape::Dense, notes, 960, 42);
    doc.track(1)
        .expect("t")
        .notes
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let s = (i as u32) % n_subs;
            let mut r = v.record();
            r.attrs = r.attrs.with_channel((s % 16) as u8).expect("ch");
            r.sub = plumspirit_engine::model::SubchannelId(s);
            r
        })
        .collect()
}

fn sub_doc(notes: usize, n_subs: u32) -> plumspirit_engine::model::PlumDocument {
    use plumspirit_engine::model::{PlumDocument, Subchannel, SubchannelId};
    let mut d = PlumDocument::new_project("v05".into(), Bpm::DEFAULT, 960);
    if n_subs > 1 {
        let subs: Vec<Subchannel> = (1..n_subs)
            .map(|i| Subchannel::new(SubchannelId(i), (i % 16) as u8))
            .collect();
        History::default()
            .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
            .expect("subs");
    }
    let recs = sub_records(notes, n_subs);
    d.track_mut(0)
        .expect("t")
        .notes
        .insert_sorted_batch(&recs, false)
        .expect("insert");
    d
}

/// v0.5: subchannels, subtracks, Preview, Onion density, Import Into Track.
fn scenario_v05(notes: usize) {
    use plumspirit_engine::editor::preview::{
        NoteKernel, PreviewScope, summarize, visible_preview,
    };
    use plumspirit_engine::midi::merge::{ConductorPolicy, MergeMode, MergeOptions, prepare_merge};
    use plumspirit_engine::midi::{
        ExportOptions, export_to_bytes_with, import_bytes, plan_export_with,
    };
    use plumspirit_engine::model::SubchannelId;
    use plumspirit_engine::model::project::{OnionOrder, order_onion_layers};
    use plumspirit_engine::playback::PlaybackSetup;
    use std::sync::atomic::AtomicBool;
    println!("## v0.5 scenario: {notes} notes");
    println!("- baseline: {}", mem());

    // --- Bytes per note.
    println!();
    println!("### Note memory (note store heap / notes)");
    println!("| layout | heap MB | bytes/note |");
    println!("|---|---|---|");
    for (name, n_subs) in [
        ("1 subchannel (main only)", 1u32),
        ("16 subchannels", 16),
        ("64 subchannels", 64),
        ("256 subchannels", 256),
        ("1024 subchannels", 1024),
    ] {
        let d = sub_doc(notes, n_subs);
        let h = d.track(0).expect("t").notes.heap_bytes();
        println!(
            "| {name} | {:.1} | {:.2} |",
            h as f64 / 1_048_576.0,
            h as f64 / notes as f64
        );
    }
    {
        let d = generate(FixtureShape::Dense, notes, 960, 42);
        let h = d.track(1).expect("t").notes.heap_bytes();
        println!(
            "| Dense fixture as imported (16 channels → 16 inferred subchannels) | {:.1} | {:.2} |",
            h as f64 / 1_048_576.0,
            h as f64 / notes as f64
        );
    }
    println!();

    // --- Per subchannel count: selection, view, playback, export, import, .lup.
    println!("### {notes} notes across N subchannels (one track)");
    println!(
        "| N | select all | select one subchannel | used subchannels | density raster 1920 px | 100% zoom query | playback index | export plan+write | MIDI size MB | re-import (+restore) | .lup save | .lup load |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    for n_subs in [16u32, 64, 256, 1024] {
        let mut d = sub_doc(notes, n_subs);
        let end = d.content_end_tick().max(1);
        let st = &mut d.track_mut(0).expect("t").notes;
        let (_, t_all) = time(|| st.select_all());
        st.clear_selection();
        let (picked, t_one) = time(|| st.select_sub(SubchannelId(n_subs / 2)));
        assert!(picked > 0);
        st.clear_selection();
        let st = &d.track(0).expect("t").notes;
        let (used, t_used) = time(|| st.used_subs());
        assert_eq!(used.len() as u32, n_subs);
        let mut raster = DensityRaster::default();
        let axis = TimeAxis::new(0, 1920.0 / end as f64);
        let (_, t_raster) = time(|| raster.rebuild(st, &axis, 1920, 0, 127));
        let (_, t_query) = time(|| {
            let mut k = 0usize;
            st.query(end / 2, end / 2 + 3840 * 4, |_, v| {
                k += v.sub.0 as usize & 1
            });
            k
        });
        let setup = PlaybackSetup::for_document(&d);
        let tempo = Arc::new(TempoMap::for_document(&d, 48_000));
        let (_, t_play) = time(|| {
            let controls = setup.control_indexes(&d);
            PlaybackIndex::new(&d, controls, Arc::clone(&tempo), &setup)
        });
        let (bytes, t_exp) = time(|| {
            let plan = plan_export_with(&d, ExportOptions::default());
            export_to_bytes_with(&d, &plan).expect("export")
        });
        let ((back, _), t_imp) = time(|| import_bytes(&bytes).expect("import"));
        assert_eq!(back.note_count(), notes);
        assert_eq!(
            back.track(0).expect("t").subchannels().len() as u32,
            n_subs,
            "subchannels restored"
        );
        drop(back);
        let (lup, t_save) = time(|| native::project_to_bytes(&d).expect("save"));
        let (_, t_load) = time(|| native::project_from_bytes(&lup).expect("load"));
        println!(
            "| {n_subs} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {} | {} | {} |",
            ms(t_all),
            ms(t_one),
            ms(t_used),
            ms(t_raster),
            ms(t_query),
            ms(t_play),
            ms(t_exp),
            bytes.len() as f64 / 1_048_576.0,
            ms(t_imp),
            ms(t_save),
            ms(t_load)
        );
    }
    println!("- after subchannel runs: {}", mem());
    println!();

    // --- Subtracks.
    println!("### Subtracks");
    {
        let mut d =
            plumspirit_engine::model::PlumDocument::new_project("st".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        let before = d.heap_bytes();
        let (_, t) = time(|| {
            h.apply(
                &mut d,
                Operation::AddSubtracks {
                    parent: 0,
                    count: 1000,
                },
            )
            .expect("add")
        });
        println!(
            "- add 1,000 empty subtracks (one undo step): {}; document heap +{:.1} KB ({} B per subtrack)",
            ms(t),
            (d.heap_bytes() - before) as f64 / 1024.0,
            (d.heap_bytes() - before) / 1000
        );
        let (rows, t) = time(|| d.tree_rows());
        println!(
            "- tree rows (track list rebuild) for {} tracks: {}",
            rows.len(),
            ms(t)
        );
        let (_, t) = time(|| d.validate_hierarchy().expect("valid"));
        println!("- hierarchy validation: {}", ms(t));
        let (_, t) = time(|| h.undo(&mut d).expect("undo"));
        println!("- undo the 1,000 subtracks: {}", ms(t));

        // 1,000 populated subtracks: `notes` notes spread over them.
        let mut d =
            plumspirit_engine::model::PlumDocument::new_project("st".into(), Bpm::DEFAULT, 960);
        h.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 1000,
            },
        )
        .expect("add");
        let recs = sub_records(notes, 1);
        let per = notes.div_ceil(1000);
        let (_, t) = time(|| {
            for (k, part) in recs.chunks(per).enumerate() {
                let mut p = part.to_vec();
                for r in &mut p {
                    r.attrs = r.attrs.with_channel((k % 16) as u8).expect("ch");
                }
                d.track_mut(k + 1)
                    .expect("t")
                    .notes
                    .insert_sorted_batch(&p, false)
                    .expect("insert");
            }
        });
        println!(
            "- fill 1,000 subtracks with {notes} notes: {} → {}",
            ms(t),
            mem()
        );
        let (bytes, t) = time(|| {
            let plan = plan_export_with(&d, ExportOptions::default());
            export_to_bytes_with(&d, &plan).expect("export")
        });
        println!(
            "- export 1,001 tracks (tree flattened, structure records): {} ({:.1} MB)",
            ms(t),
            bytes.len() as f64 / 1_048_576.0
        );
        let ((back, _), t) = time(|| import_bytes(&bytes).expect("import"));
        assert_eq!(back.tree_rows(), d.tree_rows());
        println!("- re-import with the tree restored: {}", ms(t));
        drop(back);
        // Onion: 1,000 sources ranked by visible density (Dense First).
        let end = d.content_end_tick();
        let sources: Vec<usize> = (1..=1000).collect();
        let (order, t) = time(|| {
            let counts: Vec<usize> = sources
                .iter()
                .map(|&i| {
                    d.track(i)
                        .expect("t")
                        .notes
                        .count_starting_in(end / 2, end / 2 + 3840 * 8)
                })
                .collect();
            order_onion_layers(OnionOrder::DenseFirst, &sources, &counts)
        });
        assert_eq!(order.len(), 1000);
        println!(
            "- Onion Dense First ranking of 1,000 sources (visible range): {}",
            ms(t)
        );
        let (_, t) = time(|| {
            let counts: Vec<usize> = sources
                .iter()
                .map(|&i| d.track(i).expect("t").notes.len())
                .collect();
            order_onion_layers(OnionOrder::DenseFirst, &sources, &counts)
        });
        println!(
            "- Onion Dense First ranking of 1,000 sources (whole track): {}",
            ms(t)
        );

        // Deep nesting.
        let mut d =
            plumspirit_engine::model::PlumDocument::new_project("deep".into(), Bpm::DEFAULT, 960);
        let (_, t) = time(|| {
            for parent in 0..1000 {
                h.apply(&mut d, Operation::AddSubtracks { parent, count: 1 })
                    .expect("add");
            }
        });
        println!("- build a 1,000-level nested chain: {}", ms(t));
        let (_, t) = time(|| d.validate_hierarchy().expect("valid"));
        println!("- validate the 1,000-level chain (iterative): {}", ms(t));
    }
    println!();

    // --- Preview on a huge selection.
    println!("### Preview ({notes} selected notes)");
    {
        let mut d = sub_doc(notes, 1);
        d.track_mut(0).expect("t").notes.select_all();
        let t = d.track(0).expect("t");
        let b = t.notes.selection_bounds().expect("sel");
        let k = NoteKernel::scale_x(&b, &ExactFactor::parse("1.5").expect("f")).expect("k");
        let rss0 = proc_status_kb("VmRSS:").unwrap_or(0);
        let never = AtomicBool::new(false);
        let (s, tsum) = time(|| summarize(t, &k, PreviewScope::Selected, &never));
        let end = d.content_end_tick();
        let ((n, cut), tvis) = time(|| {
            visible_preview(
                t,
                &k,
                PreviewScope::Selected,
                end / 2,
                end / 2 + 3840 * 4,
                20_000,
                |_, _| {},
            )
        });
        let rss1 = proc_status_kb("VmRSS:").unwrap_or(0);
        println!(
            "- full preflight (Scale X 1.5): {} for {} notes, {} change",
            ms(tsum),
            s.notes,
            s.changed
        );
        println!(
            "- visible preview (4 bars at 100% zoom): {} notes in {} (budget cut: {cut})",
            n,
            ms(tvis)
        );
        println!(
            "- RSS change while previewing: {:+.1} MB (no copy of the notes)",
            (rss1 as f64 - rss0 as f64) / 1024.0
        );
        let ((qs, _), tq) = time(|| {
            let k = NoteKernel::Quantize(QuantizeSpec {
                mode: QuantizeMode::Both,
                grid: 240,
                origin: 0,
            });
            (summarize(t, &k, PreviewScope::Selected, &never), ())
        });
        println!(
            "- full preflight (Quantize 1/16, both): {} ({} change)",
            ms(tq),
            qs.changed
        );
    }
    println!();

    // --- Import Into Track.
    println!("### Import MIDI Into Current Track ({notes}-note source, 16 channels)");
    {
        let src = generate(FixtureShape::Dense, notes, 480, 42);
        let bytes = {
            let plan = midi::plan_export(&src);
            midi::export_to_bytes_with(&src, &plan).expect("export")
        };
        drop(src);
        let ((source, _), t_read) = time(|| import_bytes(&bytes).expect("import"));
        println!(
            "- read + parse + normalize ({:.1} MB file): {}",
            bytes.len() as f64 / 1_048_576.0,
            ms(t_read)
        );
        for mode in [MergeMode::AsSubtracks, MergeMode::AsSubchannels] {
            let mut dest = plumspirit_engine::model::PlumDocument::new_project(
                "dest".into(),
                Bpm::DEFAULT,
                960,
            );
            let opts = MergeOptions {
                mode,
                policy: ConductorPolicy::Keep,
                at: 0,
            };
            let (p, t_prep) = time(|| prepare_merge(&dest, 0, &source, opts).expect("prepare"));
            let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
            let (_, t_commit) = time(|| h.apply(&mut dest, p.operation).expect("commit"));
            assert_eq!(dest.note_count(), notes);
            let (_, t_undo) = time(|| h.undo(&mut dest).expect("undo"));
            println!(
                "- {mode:?}: structural mapping + PPQN 480→960 {}; commit {}; undo {}",
                ms(t_prep),
                ms(t_commit),
                ms(t_undo)
            );
        }
    }
    println!("- end: {}", mem());
}

/// Median, p95 and max of `samples` (sorted in place).
fn percentiles(samples: &mut [Duration]) -> (Duration, Duration, Duration) {
    samples.sort_unstable();
    let n = samples.len().max(1);
    let at = |q: f64| samples[((n as f64 - 1.0) * q).round() as usize];
    (at(0.5), at(0.95), samples[n - 1])
}

fn us(d: Duration) -> String {
    format!("{:.1} µs", d.as_secs_f64() * 1e6)
}

/// v0.6: analytics index (notes passed / poly / NPS / PPS), peaks, live keys.
fn scenario_v06(notes: usize) {
    use plumspirit_engine::analysis::profile::PeakProfile;
    use plumspirit_engine::analysis::{
        AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing, TrackInput, document_corrections,
        for_each_sounding,
    };
    println!("## v0.6 scenario: {notes} notes (Dense fixture)");
    println!("- baseline: {}", mem());
    {
        let empty =
            plumspirit_engine::model::PlumDocument::new_project("e".into(), Bpm::DEFAULT, 960);
        let (idx, t) = time(|| AnalysisIndex::for_document(&empty));
        println!(
            "- empty project: index {} B, build {}",
            idx.heap_bytes(),
            ms(t)
        );
    }
    let (mut doc, t_gen) = time(|| generate(FixtureShape::Dense, notes, 960, 42));
    let n = doc.note_count() as u64;
    let note_heap: usize = doc.tracks().iter().map(|t| t.notes.heap_bytes()).sum();
    println!(
        "- generated in {} ({} notes, note store {:.1} MB = {:.2} B/note)",
        ms(t_gen),
        n,
        note_heap as f64 / 1_048_576.0,
        note_heap as f64 / n.max(1) as f64
    );
    println!("- before analytics: {}", mem());
    let rss0 = proc_status_kb("VmRSS:");
    let inputs = TrackInput::all(&doc);
    let (idx, t_build) = time(|| AnalysisIndex::build(&inputs, None, None).expect("build"));
    drop(inputs);
    let (timing, t_timing) = time(|| Timing::for_document(&doc));
    let rss1 = proc_status_kb("VmRSS:");
    let heap = idx.heap_bytes();
    println!();
    println!("### Analytics index");
    println!("| measure | value |");
    println!("|---|---|");
    println!("| build (one thread) | {} |", ms(t_build));
    println!("| timing (tempo map) | {} |", ms(t_timing));
    println!("| index heap | {:.2} MB |", heap as f64 / 1_048_576.0);
    println!(
        "| index bytes / note | {:.2} |",
        heap as f64 / n.max(1) as f64
    );
    println!(
        "| of which project timeline | {:.2} MB ({:.2} B/note, {} segments) |",
        idx.timeline_heap_bytes() as f64 / 1_048_576.0,
        idx.timeline_heap_bytes() as f64 / n.max(1) as f64,
        idx.timeline_segments()
    );
    if let (Some(a), Some(b)) = (rss0, rss1) {
        println!(
            "| RSS before → after | {:.1} → {:.1} MB (Δ {:.1} MB) |",
            a as f64 / 1024.0,
            b as f64 / 1024.0,
            (b as f64 - a as f64) / 1024.0
        );
    }
    println!(
        "| tracks / notes indexed | {} / {} |",
        idx.tracks().len(),
        idx.total_notes()
    );
    println!();

    // --- Random seeks (deterministic LCG).
    let end = doc.content_end_tick().max(1);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 11
    };
    let ticks: Vec<u64> = (0..10_000).map(|_| next() % end).collect();
    let scopes = [
        ("Project", Scope::Project),
        (
            "one track",
            Scope::Tracks(std::iter::once(doc.tracks()[doc.tracks().len() / 2].id()).collect()),
        ),
    ];
    println!(
        "### Random seek: metrics (notes passed, poly, NPS, PPS; 1 s window), 10,000 positions"
    );
    println!("| scope | median | p95 | max |");
    println!("|---|---|---|---|");
    let mut check = 0u64;
    for (name, scope) in &scopes {
        let mut samples: Vec<Duration> = ticks
            .iter()
            .map(|&t| {
                let s = Instant::now();
                let m = idx.metrics(scope, &timing, t, DEFAULT_WINDOW_NS);
                let d = s.elapsed();
                check = check.wrapping_add(m.poly + m.nps + m.notes_passed);
                d
            })
            .collect();
        let (p50, p95, max) = percentiles(&mut samples);
        println!("| {name} | {} | {} | {} |", us(p50), us(p95), us(max));
    }
    // Poly / notes passed alone.
    let mut samples: Vec<Duration> = ticks
        .iter()
        .map(|&t| {
            let s = Instant::now();
            check = check
                .wrapping_add(idx.poly(&Scope::Project, t) + idx.notes_passed(&Scope::Project, t));
            s.elapsed()
        })
        .collect();
    let (p50, p95, max) = percentiles(&mut samples);
    println!(
        "| Project, poly + notes passed only | {} | {} | {} |",
        us(p50),
        us(p95),
        us(max)
    );
    // Many tracks (Black MIDI files often have hundreds): the same notes spread
    // round-robin over T tracks.
    for tracks in [64usize, 512] {
        let mut md =
            plumspirit_engine::model::PlumDocument::new_project("mt".into(), Bpm::DEFAULT, 960);
        let mut per: Vec<Vec<NoteRecord>> = vec![Vec::new(); tracks];
        for (i, v) in doc.tracks().iter().flat_map(|t| t.notes.iter()).enumerate() {
            per[i % tracks].push(v.record());
        }
        for (k, recs) in per.iter_mut().enumerate() {
            recs.sort_by_key(|r| r.start_tick);
            let ti = if k == 0 {
                0
            } else {
                md.add_track().expect("track")
            };
            md.track_mut(ti)
                .expect("t")
                .notes
                .insert_sorted_batch(recs, false)
                .expect("insert");
        }
        drop(per);
        let mi = TrackInput::all(&md);
        let (midx, tb) = time(|| AnalysisIndex::build(&mi, None, None).expect("build"));
        drop(mi);
        let mt = Timing::for_document(&md);
        let mut samples: Vec<Duration> = ticks
            .iter()
            .map(|&t| {
                let s = Instant::now();
                let m = midx.metrics(&Scope::Project, &mt, t, DEFAULT_WINDOW_NS);
                check = check.wrapping_add(m.poly + m.nps);
                s.elapsed()
            })
            .collect();
        let (p50, p95, max) = percentiles(&mut samples);
        println!(
            "| Project, same notes in {tracks} tracks (build {}, {:.2} B/note, timeline {:.2}) | {} | {} | {} |",
            ms(tb),
            midx.heap_bytes() as f64 / n.max(1) as f64,
            midx.timeline_heap_bytes() as f64 / n.max(1) as f64,
            us(p50),
            us(p95),
            us(max)
        );
        // B1-0106: the Progressor's default (Respect Track Mute/Solo) after
        // half the tracks are muted — a new scope on the same index (no
        // rebuild): the scope set itself, then each query.
        let (audible, t_scope) =
            time(|| Scope::Tracks(md.tracks().iter().step_by(2).map(|t| t.id()).collect()));
        let mut samples: Vec<Duration> = ticks
            .iter()
            .map(|&t| {
                let s = Instant::now();
                let m = midx.metrics(&audible, &mt, t, DEFAULT_WINDOW_NS);
                check = check.wrapping_add(m.poly + m.nps + m.notes_passed);
                s.elapsed()
            })
            .collect();
        let (p50, p95, max) = percentiles(&mut samples);
        println!(
            "| Audible, every other of {tracks} tracks muted (scope set {}) | {} | {} | {} |",
            us(t_scope),
            us(p50),
            us(p95),
            us(max)
        );
    }
    println!();

    // --- Sequential playback: one query per 60 fps frame over the whole song.
    let song_ns = timing.nanos_at(end);
    let frames = (song_ns / 16_666_667).max(1);
    let (_, t_seq) = time(|| {
        for f in 0..frames {
            let t = timing.tick_at_nanos(f * 16_666_667);
            let m = idx.metrics(&Scope::Project, &timing, t, DEFAULT_WINDOW_NS);
            check = check.wrapping_add(m.nps);
        }
    });
    println!(
        "- sequential playback (Progressor): {} frames at 60 fps over {:.1} s of song: {} total, {} per frame",
        frames,
        song_ns as f64 / 1e9,
        ms(t_seq),
        us(t_seq / frames as u32)
    );

    // --- Keyboard reconstruction at a seek (sounding notes at a tick).
    let corr = document_corrections(&doc);
    let mut samples: Vec<Duration> = ticks
        .iter()
        .take(2_000)
        .map(|&t| {
            let s = Instant::now();
            let mut k = 0u64;
            for (tr, c) in doc.tracks().iter().zip(&corr) {
                for_each_sounding(&tr.notes, c.as_ref(), t, |r| {
                    k += u64::from(r.attrs.pitch())
                });
            }
            check = check.wrapping_add(k);
            s.elapsed()
        })
        .collect();
    let (p50, p95, max) = percentiles(&mut samples);
    println!(
        "- keyboard rebuild at a random seek (all sounding notes, 2,000 positions): median {}, p95 {}, max {}",
        us(p50),
        us(p95),
        us(max)
    );

    // --- Peak profile (lazy; only when a peak field is shown).
    let rss2 = proc_status_kb("VmRSS:");
    let (prof, t_prof) = time(|| {
        PeakProfile::build(&idx, &Scope::Project, &timing, DEFAULT_WINDOW_NS, None)
            .expect("profile")
    });
    let rss3 = proc_status_kb("VmRSS:");
    let mut samples: Vec<Duration> = ticks
        .iter()
        .take(2_000)
        .map(|&t| {
            let s = Instant::now();
            check = check.wrapping_add(prof.peak_poly_so_far(&idx, &Scope::Project, t));
            check = check.wrapping_add(prof.peak_nps_so_far(&idx, &Scope::Project, &timing, t));
            s.elapsed()
        })
        .collect();
    let (p50, p95, max) = percentiles(&mut samples);
    println!(
        "- peak profile: build {}, {:.2} MB ({:.3} B/note), {} events; peak poly + peak NPS so far: median {}, p95 {}, max {}{}",
        ms(t_prof),
        prof.heap_bytes() as f64 / 1_048_576.0,
        prof.heap_bytes() as f64 / n.max(1) as f64,
        prof.events,
        us(p50),
        us(p95),
        us(max),
        match (rss2, rss3) {
            (Some(a), Some(b)) => format!(
                " (RSS Δ {:.1} MB, peak build buffers included)",
                (b as f64 - a as f64) / 1024.0
            ),
            _ => String::new(),
        }
    );

    // --- Edits: index update with reuse.
    println!();
    println!("### Index update after edits (reusing the previous index)");
    println!("| edit | update | pieces rebuilt | pieces reused | notes re-indexed |");
    println!("|---|---|---|---|---|");
    let mut hist = History::default();
    let ti = doc.tracks().len() / 2;
    let mut prev = idx;
    let row = |label: &str,
               doc: &plumspirit_engine::model::PlumDocument,
               prev: &mut AnalysisIndex| {
        let inputs = TrackInput::all(doc);
        let (next, t) = time(|| AnalysisIndex::build(&inputs, Some(&*prev), None).expect("update"));
        let st = next.stats;
        println!(
            "| {label} | {} | {} | {} | {} |",
            ms(t),
            st.pieces_built,
            st.pieces_reused,
            st.notes_built
        );
        *prev = next;
    };
    let rec = NoteRecord {
        start_tick: end / 2,
        duration_ticks: 240,
        attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).expect("attrs"),
        sub: Default::default(),
        group: 0,
    };
    hist.apply(
        &mut doc,
        Operation::InsertNote {
            track: ti.into(),
            record: rec,
        },
    )
    .expect("insert");
    row("insert one note", &doc, &mut prev);
    doc.track_mut(ti).expect("t").notes.select_all();
    hist.apply(
        &mut doc,
        Operation::SetVelocity {
            track: ti.into(),
            velocity: 77,
        },
    )
    .expect("velocity");
    row(
        "velocity of a whole track (no timing change)",
        &doc,
        &mut prev,
    );
    hist.apply(
        &mut doc,
        Operation::Resize {
            track: ti.into(),
            delta: 30,
        },
    )
    .expect("resize");
    row("resize a whole track", &doc, &mut prev);
    doc.track_mut(ti).expect("t").notes.clear_selection();
    hist.undo(&mut doc).expect("undo");
    row("undo the resize", &doc, &mut prev);
    let (_, t_tempo) = time(|| {
        set_dense_tempo(&mut doc, 200, 3840, 7);
        Timing::for_document(&doc)
    });
    println!(
        "| 200 tempo points (Timing only, no index rebuild) | {} | 0 | all | 0 |",
        ms(t_tempo)
    );
    println!();
    println!("- end: {} (checksum {check})", mem());
}

/// v0.7: the note transformation platform on a selection of `notes` notes
/// (contract §159–160): tool open, preview (lazy viewport and full run with
/// preflight), Apply, Undo / Redo, undo memory, `.psc`, geometry, mask,
/// generators, selection analytics.
fn scenario_v07(notes: usize) {
    use plumspirit_engine::analysis::Timing;
    use plumspirit_engine::analysis::selection::{SelectionAnalysis, SelectionInput};
    use plumspirit_engine::transform::param::ParamValue;
    use plumspirit_engine::transform::psc::Score;
    use plumspirit_engine::transform::registry::BuildCtx;
    use plumspirit_engine::transform::run::{build_operation, collect_with};
    use plumspirit_engine::transform::{
        Ctx, Limits, Output, ParamSet, TNote, TrackScope, run_kernel, tool,
    };
    println!("## v0.7 scenario: {notes} selected notes (Dense fixture, one track)");
    println!("- baseline: {}", mem());
    let mut doc = generate(FixtureShape::Dense, notes, 960, 42);
    // A mask track (the Clipping Mask base): a band of 24 rows over the song.
    // The Dense fixture keeps its notes in track 1 (track 0 is the old
    // tempo track).
    const ART: usize = 1;
    let end = doc.track(ART).expect("t").notes.max_end().unwrap_or(0);
    doc.add_track().expect("track");
    let band: Vec<NoteRecord> = (48u8..72)
        .map(|p| {
            NoteRecord::new(
                0,
                end.max(1),
                NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).expect("attrs"),
            )
        })
        .collect();
    let mask = doc.track_count() - 1;
    doc.track_mut(mask)
        .expect("t")
        .notes
        .insert_sorted_batch(&band, false)
        .expect("band");
    doc.track_mut(ART).expect("t").notes.select_all();
    println!("- fixture: {} → {}", group(notes as u64), mem());
    let mut history = History::new(HistoryLimits::default());
    let limits = Limits::default();
    let scopes = [TrackScope {
        index: ART,
        all: false,
        content: None,
    }];
    println!();
    println!(
        "| tool | open (source + kernel) | lazy 4 bars | full run (preflight est.) | out notes | Apply | undo bytes | Undo | Redo | RSS after |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    let cases: &[(&str, &[(&str, ParamValue)])] = &[
        ("transpose", &[("semis", ParamValue::Int(7))]),
        // Humanize's defaults are zero and the fixture is on the 1/16 grid:
        // identity results are not applied, so both get real changes.
        (
            "humanize",
            &[
                ("tick.min", ParamValue::Int(-30)),
                ("tick.max", ParamValue::Int(30)),
                ("vel.min", ParamValue::Int(-10)),
                ("vel.max", ParamValue::Int(10)),
            ],
        ),
        ("quantize", &[("grid", ParamValue::Choice(2))]),
        ("legato", &[]),
        ("arpeggiate", &[]),
        ("repeat", &[]),
        ("twirl", &[]),
        ("wave", &[]),
        ("clip_mask", &[]),
    ];
    for (id, set) in cases {
        let def = tool(id).expect("tool");
        let (built, t_open) = time(|| {
            let mut ps = ParamSet::defaults(&(def.spec)().params);
            for (k, v) in set.iter() {
                ps.set(k, v.clone());
            }
            let src = collect_with(def, &doc, &scopes, &ps, 1 << 27).expect("source");
            let mut ctx = Ctx::simple(960);
            ctx.groups = Arc::new(doc.project().groups.clone());
            let k = {
                let b = BuildCtx {
                    ctx: &ctx,
                    stats: &src.stats,
                    tracks: &src.tracks,
                };
                def.kernel(&ps, &b).expect("kernel")
            };
            (ps, src, k)
        });
        let (ps, src, k) = built;
        // Lazy viewport preview (4 bars in the middle), pointwise kernels only.
        let lazy = k.as_point().map(|pk| {
            let (t0, t1) = (end / 2, end / 2 + 960 * 16);
            let (a, b) = pk.source_window(t0, t1);
            let (n, t) = time(|| {
                let mut out = Output::new(def.boundary(&ps), 1 << 22);
                doc.track(ART).expect("t").notes.query(a, b, |_, v| {
                    if v.selected {
                        pk.map(&TNote::from_record(&v.record(), 0), &mut out);
                    }
                });
                out.notes.len()
            });
            format!("{} ({} notes)", ms(t), group(n as u64))
        });
        let est = k.estimate(&src.stats);
        let (run, t_run) = time(|| run_kernel(k.as_ref(), &src, def.boundary(&ps), &limits, None));
        let run = match run {
            Ok(r) => r,
            Err(e) => {
                println!(
                    "| {id} | {} | — | refused: {} | | | | | | |",
                    ms(t_open),
                    e.user_message()
                );
                continue;
            }
        };
        let out_n = run.notes.len();
        drop(src);
        let before = history.bytes();
        let (r, t_apply) = time(|| {
            let op = build_operation(&mut doc, &run, "perf").expect("op");
            history.apply(&mut doc, op).expect("apply")
        });
        let _ = r;
        let undo_bytes = history.bytes().saturating_sub(before);
        drop(run);
        let (_, t_undo) = time(|| history.undo(&mut doc).expect("undo"));
        let (_, t_redo) = time(|| history.redo(&mut doc).expect("redo"));
        let rss = mem();
        // Back to the fixture for the next tool (and a fresh selection).
        history.undo(&mut doc).expect("undo");
        history.clear();
        doc.track_mut(ART).expect("t").notes.select_all();
        println!(
            "| {id} | {} | {} | {} (≤ {} notes, {:.1} MB temp, {:.1} MB undo) | {} | {} | {:.1} MB | {} | {} | {} |",
            ms(t_open),
            lazy.unwrap_or_else(|| "— (worker)".into()),
            ms(t_run),
            group(est.notes),
            est.temp_bytes as f64 / 1_048_576.0,
            est.undo_bytes as f64 / 1_048_576.0,
            group(out_n as u64),
            ms(t_apply),
            undo_bytes as f64 / 1_048_576.0,
            ms(t_undo),
            ms(t_redo),
            rss
        );
    }
    // Preflight refusal of an impossible expansion (nothing allocated).
    {
        let def = tool("repeat").expect("tool");
        let mut ps = ParamSet::defaults(&(def.spec)().params);
        for (k, v) in [
            ("len", ParamValue::Choice(11)),
            ("len.ticks", ParamValue::Int(1)),
            ("a10", ParamValue::Bool(true)),
            ("a10.depth", ParamValue::Int(4)),
            ("a10.div", ParamValue::Int(8)),
        ] {
            ps.set(k, v);
        }
        let src = collect_with(def, &doc, &scopes, &ps, 1 << 27).expect("source");
        let ctx = Ctx::simple(960);
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let before = mem();
        let (r, t) = time(|| {
            def.kernel(&ps, &b)
                .and_then(|k| run_kernel(k.as_ref(), &src, def.boundary(&ps), &limits, None))
        });
        println!();
        println!(
            "- preflight, Note Repeat every tick, recursive 4 × 8: {} in {} ({} → {})",
            match &r {
                Ok(run) => format!("ran ({} notes)", group(run.notes.len() as u64)),
                Err(e) => format!("refused: {}", e.user_message()),
            },
            ms(t),
            before,
            mem()
        );
    }
    // .psc save / load of the whole selection.
    {
        let mut v = Vec::with_capacity(notes);
        doc.track(ART)
            .expect("t")
            .notes
            .for_each_selected(|n| v.push(TNote::from_record(&n.record(), 0)));
        let (score, t_build) = time(|| Score::from_notes(&v, 960, |_, _| (0, None)));
        let (bytes, t_save) = time(|| score.to_bytes());
        let (back, t_load) = time(|| Score::from_bytes(&bytes).expect("load"));
        assert_eq!(back.notes.len(), v.len());
        println!(
            "- .psc: {} notes → {:.1} MB ({:.2} B/note); build {}, write {}, read + validate {}",
            group(v.len() as u64),
            bytes.len() as f64 / 1_048_576.0,
            bytes.len() as f64 / v.len().max(1) as f64,
            ms(t_build),
            ms(t_save),
            ms(t_load)
        );
    }
    // Selection analytics of the whole selection.
    {
        let timing = Timing::for_document(&doc);
        let t = doc.track(ART).expect("t");
        let inp = [SelectionInput {
            id: t.id(),
            notes: t.notes.clone(),
            correction: None,
            shift: 0,
        }];
        let (a, tb) = time(|| SelectionAnalysis::build(&inp, &timing, None).expect("sel"));
        let (m, tq) = time(|| a.at(&timing, end / 2, 1_000_000_000));
        println!(
            "- selection analytics: build {}, query {} (peak poly {}, unique {}, NPS {} at the middle)",
            ms(tb),
            ms(tq),
            group(a.peak_poly),
            a.peak_unique,
            group(m.nps)
        );
    }
    println!("- end: {}", mem());
}

/// v0.9 measurements in their own process (the scenario's memory figures stay
/// comparable with older builds): the Scale shrink cleanup (§086) — the scale
/// handles' kernel stretched (no cleanup) and shrunk (cleanup) over the same
/// notes, and Scale X ×0.5 (cleanup) against ×2.
fn scenario_v09(notes: usize) {
    println!("## v0.9: {notes} notes, Dense");
    let mut doc = generate(FixtureShape::Dense, notes, 960, 42);
    let track = (0..doc.track_count())
        .max_by_key(|&i| doc.track(i).map_or(0, |t| t.notes.len()))
        .expect("track");
    doc.track_mut(track).expect("track").notes.select_all();
    {
        use plumspirit_engine::transform::geom::{BoxScale, GeomBox};
        use plumspirit_engine::transform::{Boundary, Limits, Source, TrackScope, run_kernel};
        let b = doc
            .track(track)
            .expect("track")
            .notes
            .selection_bounds()
            .expect("selection");
        let from = GeomBox {
            t0: b.min_start,
            t1: b.max_end.max(b.min_start + 1),
            p0: b.min_pitch,
            p1: b.max_pitch,
        };
        let scope = [TrackScope {
            index: track,
            all: false,
            content: None,
        }];
        let src = Source::collect(&doc, &scope, 1 << 28).expect("source");
        let w = from.t1 - from.t0;
        for (name, t1) in [
            ("x2 stretch", from.t0 + w * 2),
            ("x0.5 shrink", from.t0 + w / 2),
            ("x0.01 shrink", from.t0 + (w / 100).max(1)),
        ] {
            let k = BoxScale {
                from,
                to: GeomBox { t1, ..from },
                fill: None,
                prevent: false,
                cleanup: Default::default(),
            };
            let (r, t) = time(|| {
                run_kernel(&k, &src, Boundary::default(), &Limits::default(), None).expect("run")
            });
            println!(
                "- Scale handles {name} on all notes: {} ({} out, {} collapsed)",
                ms(t),
                r.notes.len(),
                r.stats.collapsed
            );
        }
    }
    // V09-S31: the mandatory functions around Transpose by 0 (identity) on
    // every note: the platform's added cost per run.
    {
        use plumspirit_engine::transform::definition::find;
        use plumspirit_engine::transform::registry::BuildCtx;
        use plumspirit_engine::transform::{
            Ctx, Limits, ParamSet, ParamValue, Source, TrackScope, run_kernel, tool,
        };
        let scope = [TrackScope {
            index: track,
            all: false,
            content: None,
        }];
        let src = Source::collect(&doc, &scope, 1 << 28).expect("source");
        let def = tool("transpose").expect("transpose");
        let specs = find("transpose").expect("definition").specs();
        let ctx = Ctx::simple(960);
        let on = ParamValue::Bool(true);
        let cases: [(&str, Vec<(&str, ParamValue)>); 6] = [
            ("off", vec![]),
            ("subchannel driver (time)", vec![("m.sub", on.clone())]),
            (
                "pitch driver (note index)",
                vec![
                    ("m.pitch", on.clone()),
                    ("m.pitch.by", ParamValue::Choice(2)),
                    ("m.pitch.map", ParamValue::Choice(1)),
                    ("m.pitch.cycle", ParamValue::List(vec![0, 12])),
                    ("b.pitch", ParamValue::Choice(1)),
                ],
            ),
            ("remove overlaps (exact)", vec![("m.ovl", on.clone())]),
            (
                "control sweep (semitones over time)",
                vec![
                    ("m.sweep", on.clone()),
                    ("m.sweep.semis", on.clone()),
                    ("b.pitch", ParamValue::Choice(1)),
                ],
            ),
            (
                "all four",
                vec![
                    ("m.sub", on.clone()),
                    ("m.pitch", on.clone()),
                    ("m.ovl", on.clone()),
                    ("m.sweep", on.clone()),
                    ("m.sweep.semis", on.clone()),
                    ("b.pitch", ParamValue::Choice(1)),
                ],
            ),
        ];
        for (name, pairs) in cases {
            let mut set = ParamSet::defaults(&specs);
            for (k, v) in pairs {
                set.set(k, v);
            }
            let b = BuildCtx {
                ctx: &ctx,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let mut best = Duration::MAX;
            let mut out = 0;
            for _ in 0..3 {
                let (r, t) = time(|| {
                    let k = def.kernel(&set, &b).expect("kernel");
                    run_kernel(
                        k.as_ref(),
                        &src,
                        def.boundary(&set),
                        &Limits::default(),
                        None,
                    )
                    .expect("run")
                });
                best = best.min(t);
                out = r.notes.len();
            }
            println!(
                "- Mandatory functions on Transpose 0, {name}: {} ({} out)",
                ms(best),
                out
            );
        }
    }
    expansion_cases(&doc, track, None, None);
    for f in ["2", "0.5"] {
        let factor = ExactFactor::parse(f).expect("factor");
        let (p, t) = time(|| {
            editor::prepare_scale_x(&doc, track, &factor, EditControl::default()).expect("prep")
        });
        println!(
            "- Scale X x{f} prepare on all notes: {} ({} out)",
            ms(t),
            p.affected_notes()
        );
    }
    println!("- end: {}", mem());
}

/// V09-S34: the tool expansion's new functions, each alone and all on,
/// against the tool with them off (best of 3; same fixture). `only` keeps
/// one tool; `pick` = "off" measures just the "off" case (an older build
/// without the functions, for A/B: `perf_report expansion <notes> <tool>
/// off`), a function id just "off" and that function (profiling one).
/// A tool expansion case beyond the defaults: tool, name (starting with
/// the function id it switches on), integer settings.
type ExtraCase = (&'static str, &'static str, &'static [(&'static str, i64)]);

fn expansion_cases(
    doc: &plumspirit_engine::model::PlumDocument,
    track: usize,
    only: Option<&str>,
    pick: Option<&str>,
) {
    use plumspirit_engine::transform::definition::find;
    use plumspirit_engine::transform::registry::BuildCtx;
    use plumspirit_engine::transform::{
        Ctx, Limits, ParamSet, ParamValue, Source, TrackScope, run_kernel, tool,
    };
    let scope = [TrackScope {
        index: track,
        all: false,
        content: None,
    }];
    let src = Source::collect(doc, &scope, 1 << 28).expect("source");
    let ctx = Ctx::simple(960);
    const ALL11: &[&str] = &[
        "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
    ];
    // Note Repeat's ids skip n04 / n06 / n11 (n12 – n14 took their places).
    const REP11: &[&str] = &[
        "n01", "n02", "n03", "n12", "n05", "n13", "n07", "n08", "n09", "n10", "n14",
    ];
    // Chord Stamp has no n04 (n12 took its place).
    const CS11: &[&str] = &[
        "n01", "n02", "n03", "n12", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
    ];
    const HUM_RANGES: &[(&str, i64)] = &[
        ("tick.min", -30),
        ("tick.max", 30),
        ("gate.min", -20),
        ("gate.max", 20),
        ("vel.min", -10),
        ("vel.max", 10),
    ];
    // Paint V2 has seven new functions (14 + 4 + 7 = 25).
    const SP7: &[&str] = &["n01", "n02", "n03", "n04", "n05", "n06", "n07"];
    // Pulse Field has no n04 (n12 took its place; V09-S41).
    const PF11: &[&str] = &[
        "n01", "n02", "n03", "n05", "n06", "n07", "n08", "n09", "n10", "n11", "n12",
    ];
    // Duplicate has no n06 (n12 took its place).
    const DUP11: &[&str] = &[
        "n01", "n02", "n03", "n04", "n05", "n12", "n07", "n08", "n09", "n10", "n11",
    ];
    let expansion: &[(&str, &[&str])] = &[
        ("arpeggiate", ALL11),
        ("articulate", ALL11),
        ("chop", ALL11),
        ("claw", ALL11),
        ("quantize", ALL11),
        ("randomize", ALL11),
        ("strum", ALL11),
        ("flam", ALL11),
        ("limit", ALL11),
        ("chord_progression", ALL11),
        ("chord_stamp", CS11),
        ("riff", ALL11),
        ("slide", ALL11),
        ("humanize", ALL11),
        ("transpose", ALL11),
        ("legato", ALL11),
        ("note_props", ALL11),
        ("join", ALL11),
        ("repeat", REP11),
        ("group", ALL11),
        ("skew", ALL11),
        ("distort", ALL11),
        ("perspective", ALL11),
        ("warp", ALL11),
        ("shear", ALL11),
        ("twirl", ALL11),
        ("wave", ALL11),
        ("clip_mask", ALL11),
        ("paint_color", ALL11),
        ("subpaint", SP7),
        ("delete", ALL11),
        ("select", ALL11),
        ("draw", ALL11),
        ("paint", ALL11),
        ("snap_scale", ALL11),
        ("flip", ALL11),
        ("duplicate", DUP11),
        ("stretch", ALL11),
        ("gravity_well", ALL11),
        ("kaleidoscope", ALL11),
        ("orbit", ALL11),
        ("vortex_trails", ALL11),
        ("pulse_field", PF11),
        ("particle_burst", ALL11),
        ("flow_field", ALL11),
    ];
    // Clipping Mask's layers: every fourth selected note, twice as long, as
    // the base track (a second track slot, the clipped pieces' destination)
    // and every eighth, a beat later, as the extra mask track.
    let clip_src = only.is_none_or(|o| o == "clip_mask").then(|| {
        let mut s = src.clone();
        let mut base = s.tracks[0].clone();
        base.index += 1;
        s.tracks.push(base);
        s.aux = Arc::new(vec![
            src.notes
                .iter()
                .step_by(4)
                .map(|n| plumspirit_engine::transform::TNote {
                    track: 1,
                    dur: n.dur.saturating_mul(2),
                    ..*n
                })
                .collect(),
            src.notes
                .iter()
                .step_by(8)
                .map(|n| plumspirit_engine::transform::TNote {
                    track: u16::MAX,
                    start: n.start + 960,
                    ..*n
                })
                .collect(),
        ]);
        s.aux_slot = Some(1);
        s
    });
    // Shear's references (n01's contour track, n08's onsets): every fourth
    // selected note as the track, every sixteenth as unselected notes.
    let shear_src = only.is_none_or(|o| o == "shear").then(|| {
        let mut s = src.clone();
        s.context = Arc::new(src.notes.iter().step_by(16).copied().collect());
        s.aux = Arc::new(vec![
            src.notes.iter().step_by(4).copied().collect(),
            Vec::new(),
        ]);
        s
    });
    // Delete's references (n07): every sixteenth selected note as an
    // unselected note of the track; its crop region (n08): the middle half
    // of the selection as the time selection.
    // (Select's n01 grows from the same unselected notes; its n08 takes
    // the marker section holding the cursor — markers at the selection's
    // quarters, the cursor in the middle.)
    // (Draw reads them as the active track's unselected notes: n01's
    // chords, n08's existing notes; the Note Brush as the notes its
    // editing functions change and n02's outlines.)
    let del_src = only
        .is_none_or(|o| o == "delete" || o == "select" || o == "draw" || o == "paint")
        .then(|| {
            let mut s = src.clone();
            s.context = Arc::new(src.notes.iter().step_by(16).copied().collect());
            s
        });
    // Duplicate's prepared parts: the next track as n01's destination, a
    // CC 7 point at every 64th selected note's start (n08), every
    // 250,000th note as n03's reference (a few chord changes: one copy of
    // the selection each). (Not for the off case alone: the A/B of the
    // v0.8 path runs on the plain source in both builds.)
    let dup_src = (only.is_none_or(|o| o == "duplicate") && pick != Some("off")).then(|| {
        let mut s = src.clone();
        let mut dest = s.tracks[0].clone();
        dest.index += 1;
        dest.insert_only = true;
        s.tracks.push(dest);
        s.routes = Arc::new(vec![(0, 1)]);
        s.aux_events = Arc::new(
            src.notes
                .iter()
                .step_by(64)
                .enumerate()
                .map(|(k, n)| plumspirit_engine::transform::output::TEvent {
                    track: 0,
                    lane: plumspirit_engine::transform::output::Lane::Cc(7),
                    ch: 0,
                    tick: n.start,
                    value: (k % 128) as u16,
                })
                .collect(),
        );
        s.aux = Arc::new(vec![src.notes.iter().step_by(250_000).copied().collect()]);
        s
    });
    let del_ctx = {
        let mut c = Ctx::simple(960);
        let (a, z) = (src.stats.min_start, src.stats.max_end);
        let q = z.saturating_sub(a) / 4;
        c.time_range = Some((a + q, z - q));
        c
    };
    let sel_ctx = {
        let mut c = Ctx::simple(960);
        let (a, z) = (src.stats.min_start, src.stats.max_end);
        let q = z.saturating_sub(a) / 4;
        c.markers = Arc::new(vec![a + q, z - q]);
        c.cursor = a + 2 * q;
        c
    };
    // Draw's pointer stroke: 4,096 samples of a wave (±20 rows round key
    // 64) across the selection's span, drawn in 41 s.
    let draw_ctx = {
        let mut c = Ctx::simple(960);
        let (a, z) = (src.stats.min_start, src.stats.max_end);
        let points = (0..4096u64)
            .map(|k| plumspirit_engine::transform::StrokePoint {
                tick: a + z.saturating_sub(a) * k / 4095,
                pitch: 64.0 + 20.0 * (k as f64 / 50.0).sin(),
                t: k as f64 * 0.01,
            })
            .collect();
        c.stroke = Some(Arc::new(plumspirit_engine::transform::Stroke { points }));
        c
    };
    for (id, fns) in expansion {
        if only.is_some_and(|o| o != *id) {
            continue;
        }
        let src = match (&shear_src, &clip_src, &del_src, &dup_src) {
            (Some(s), _, _, _) if *id == "shear" => s,
            (_, Some(s), _, _) if *id == "clip_mask" => s,
            (_, _, Some(s), _)
                if *id == "delete" || *id == "select" || *id == "draw" || *id == "paint" =>
            {
                s
            }
            (_, _, _, Some(s)) if *id == "duplicate" => s,
            _ => &src,
        };
        let ctx = match *id {
            "delete" => &del_ctx,
            "select" => &sel_ctx,
            "draw" | "paint" => &draw_ctx,
            _ => &ctx,
        };
        let def = tool(id).expect("tool");
        let specs = find(id).expect("definition").specs();
        let mut cases: Vec<(String, Vec<&str>)> = vec![("off".into(), vec![])];
        // Settings beyond the defaults (a case that does the work the
        // defaults skip on this fixture).
        let extra: &[ExtraCase] = &[
            ("flam", "n11 at 100 notes per second", &[("n11.nps", 100)]),
            ("limit", "n03 at 2 sounding notes", &[("n03.most", 2)]),
            ("limit", "n04 at 20 onsets per second", &[("n04.most", 20)]),
            ("limit", "n07 at 960 ticks", &[("n07.dist", 960)]),
            ("limit", "a10 harmonic window (v0.8, contextual)", &[]),
            ("chord_progression", "n08 per beat", &[("n08.per", 1)]),
            (
                "chord_progression",
                "n08 per melody note",
                &[("n08.per", 2)],
            ),
            (
                "chord_progression",
                "n10 at 32 voices",
                &[("n10.voices", 32)],
            ),
            ("chord_stamp", "n11 at 8 voices", &[("n11.most", 8)]),
            (
                "chord_stamp",
                "n05 with 32 partials",
                &[("n05.partials", 32)],
            ),
            // The Riff Machine's v0.8 pipeline over the selection itself
            // (the default generates 4 bars and keeps the selection).
            ("riff", "off, from the selected notes", &[("source", 0)]),
            ("riff", "n07 at 1,024 bars", &[("n07.bars", 1024)]),
            ("riff", "n08 at 8 variants", &[("n08.count", 8)]),
            (
                "riff",
                "n07+n02+n08 at 1,024 bars, 8 layers, 4 variants",
                &[("n07.bars", 1024), ("n02.layers", 8), ("n08.count", 4)],
            ),
            // Humanize's ranges are 0 by default (the identity): the v0.8
            // per-note rule with ranges, and its contextual density path.
            ("humanize", "off, with ranges", HUM_RANGES),
            (
                "humanize",
                "a6 density with ranges (v0.8, contextual)",
                HUM_RANGES,
            ),
            // Transpose's base is 0 semitones (the identity); every case
            // clamps notes moved outside the keys (TR_BASE).
            ("transpose", "off, +5 semitones", &[("semis", 5)]),
            (
                "transpose",
                "a9 sequence +5 semitones (v0.8, contextual)",
                &[("semis", 5)],
            ),
            (
                "transpose",
                "n10 at 8 layers over the whole keyboard",
                &[("n10.lo", 0), ("n10.hi", 127), ("n10.layers", 8)],
            ),
            (
                "transpose",
                "n03 allocating channels",
                &[("n03.channels", 1)],
            ),
            (
                "transpose",
                "n04+n05+n07 per bar, per chord",
                &[("n04.scope", 1), ("n05.per", 0)],
            ),
            // Legato's base is the v0.8 legato to the next onset.
            ("legato", "a5 chord-aware (v0.8)", &[]),
            ("legato", "n03 writing CC 64", &[("n03.mode", 1)]),
            ("legato", "n04 at 8 notes", &[("n04.most", 8)]),
            ("legato", "n11 over 16 onsets", &[("n11.n", 16)]),
            ("legato", "n05+n07+n06 meeting, ties, retrigger", &[]),
            // Note Properties' base sets nothing (the identity).
            (
                "note_props",
                "vel.on set velocity 90 (v0.8)",
                &[("vel", 90)],
            ),
            ("note_props", "a10 macro staccato (v0.8)", &[]),
            (
                "note_props",
                "n06 at 64 notes per key",
                &[("n06.window", 64), ("n06.per", 1)],
            ),
            ("note_props", "n09+n01+n03 controller, mapping, cycle", &[]),
            // Note Repeat's base repeats every 1/16 (Dense notes ≤ a beat).
            ("repeat", "off, every 1/32 (v0.8)", &[("len", 5)]),
            ("repeat", "n02 at 8 echoes", &[("n02.echoes", 8)]),
            ("repeat", "n08 up to 64 per beat", &[("n08.most", 64)]),
            ("repeat", "n12+n13+n14 controller, Doppler, overlap", &[]),
            ("repeat", "n07+n10+n09 voices, delayed, length cycle", &[]),
            // Join's base joins touching fragments of a chain.
            ("join", "a1+a2 gap 120, union (v0.8)", &[("a1.gap", 120)]),
            // Dense holds 96 starts a second, its chains far apart (a key and
            // channel every ~28,000 ticks): joins need a wide gap.
            (
                "join",
                "n04 at 10 notes per second, gaps up to 100,000 ticks",
                &[("n04.nps", 10), ("n04.gap", 100_000)],
            ),
            ("join", "n11 at 1 fragment per bar", &[("n11.most", 1)]),
            // Group's base groups the whole selection (one group); most
            // functions work across groups, so Auto-group makes them.
            (
                "group",
                "a6+a7+a8 pivot, mirror, transpose (v0.8)",
                &[("a7.axis", 1)],
            ),
            ("group", "n01 by chords", &[("n01.by", 1)]),
            ("group", "n01 by bars", &[("n01.by", 2)]),
            ("group", "n01 connected shapes", &[("n01.by", 4)]),
            (
                "group",
                "n01+n02+n03+n11 per bar: reverse, stagger, series",
                &[("n01.by", 2), ("n11.order", 1)],
            ),
            (
                "group",
                "n01+n06+n09 by bars, split at 16, outline of 4",
                &[("n01.by", 2), ("n06.largest", 16)],
            ),
            ("group", "n01+n04 per bar, repeated", &[("n01.by", 2)]),
            ("group", "n01+n07 per bar, shape copy", &[("n01.by", 2)]),
            (
                "group",
                "n01+n05+n08+n10 by chords: fit, degrees, map",
                &[("n01.by", 1), ("n05.fit", 2)],
            ),
            (
                "group",
                "n01+n03+m.sweep+m.sweep.stagger by chords, swept per group",
                &[("n01.by", 1)],
            ),
            // Skew's base is the v0.8 X skew of 50 % (one note per note).
            (
                "skew",
                "off, split into 8 segments (v0.8)",
                &[("g.raster", 2)],
            ),
            ("skew", "n04 per bar", &[("n04.unit", 1)]),
            ("skew", "n04 per phrase", &[("n04.unit", 3)]),
            (
                "skew",
                "n05 in pitch, one octave",
                &[("n05.axis", 1), ("n05.span", 3), ("n05.n", 1)],
            ),
            ("skew", "n06+n08+n07 real time, fit, depth", &[]),
            // All eleven with Fit to a far tick (the default target, the
            // next bar line, is refused once End skew lengthens notes past
            // it on Dense's song-wide box).
            (
                "skew",
                "n01+n02+n03+n04+n05+n06+n07+n08+n09+n10+n11 Fit to tick 2^40",
                &[("n08.target", 1), ("n08.tick", 1 << 40)],
            ),
            // Distort's base is the v0.8 bulge of 40 %.
            ("distort", "a1+a3+a5+a8+a10 (v0.8 path)", &[]),
            ("distort", "n05 per chord", &[("n05.tile", 2)]),
            ("distort", "n08 at 256 shards", &[("n08.shards", 256)]),
            (
                "distort",
                "n10 runs closer than a beat",
                &[("n10.within", 1), ("n10.gap", 960)],
            ),
            // Perspective's base is the v0.8 top edge of 50 %.
            ("perspective", "a3+a8+a10 (v0.8 path)", &[]),
            ("perspective", "n08+a10 occlusion of the echoes", &[]),
            ("perspective", "n04+n09+n10 three copies", &[]),
            // Warp's base is the v0.8 push of 20 % around the centre.
            ("warp", "a1+a2+a3+a9 (v0.8 path)", &[]),
            (
                "warp",
                "n02 bounding rows of the unselected notes",
                &[("n02.ref", 2), ("n02.fit", 1)],
            ),
            ("warp", "n10 at 16 iterations", &[("n10.iter", 16)]),
            ("warp", "n07+n05+n09 polar mesh, noise, path", &[]),
            ("shear", "a1+a2+a6+a9 (v0.8 path)", &[]),
            ("shear", "n01 contour of a track", &[("n01.of", 2)]),
            (
                "shear",
                "n10 at a fixed period of a beat",
                &[("n10.stretch", 1), ("n10.period", 960)],
            ),
            (
                "shear",
                "n09+n11 16 copies in colour",
                &[("n09.copies", 16)],
            ),
            ("shear", "n02+n08+n07 lengths, pulse, band", &[]),
            ("twirl", "a1+a4+a6+a9 (v0.8 path)", &[]),
            ("twirl", "n07 at a resolution of 3 rows", &[("n07.res", 3)]),
            (
                "twirl",
                "n02 bar vortices alternating",
                &[("n02.unit", 2), ("n02.alt", 1)],
            ),
            ("twirl", "n09 in velocity order", &[("n09.order", 2)]),
            ("twirl", "n06+n05+n10 street, sector, turbulence", &[]),
            // Wave's base: five generators (the v0.8 kernel).
            (
                "wave",
                "a4+a5+a6+a9 sweep, envelope, interference, vector (v0.8)",
                &[],
            ),
            ("wave", "n01 in milliseconds", &[("n01.unit", 2)]),
            ("wave", "n03 at phrases", &[("n03.at", 1)]),
            ("wave", "n03 at chords", &[("n03.at", 2)]),
            ("wave", "n02 with 30-tick segments", &[("n02.seg", 30)]),
            ("wave", "n06+n07+n11 Lissajous, fold, distortion", &[]),
            ("wave", "n05+n04 gate, velocity", &[]),
            ("wave", "m.sweep+m.sweep.sy swept per note", &[]),
            // E0422 (V09-S39 step 0): the S34 – S36 tools' Control Sweeps
            // per level inside one run, the generators' per unit (each
            // sweep's default graph: up to 64 levels over the selection).
            ("arpeggiate", "m.sweep+m.sweep.gate swept per level", &[]),
            ("articulate", "n01+m.sweep+m.sweep.gap swept per level", &[]),
            ("chop", "n01+m.sweep+m.sweep.pulses swept per level", &[]),
            (
                "claw",
                "n01+m.sweep+m.sweep.end swept per level (a grid per level)",
                &[],
            ),
            ("quantize", "n01+m.sweep+m.sweep.swing swept per level", &[]),
            (
                "randomize",
                "n02+m.sweep+m.sweep.note_note swept per slot (generate)",
                &[("mode", 2)],
            ),
            ("strum", "n01+m.sweep+m.sweep.real swept per level", &[]),
            ("flam", "n01+m.sweep+m.sweep.interval swept per level", &[]),
            ("limit", "n01+m.sweep+m.sweep.vel_hi swept per level", &[]),
            (
                "chord_progression",
                "n02+m.sweep+m.sweep.borrow swept per chord",
                &[],
            ),
            (
                "chord_stamp",
                "n02+m.sweep+m.sweep.voices_soft swept per level",
                &[],
            ),
            ("riff", "n01+m.sweep+m.sweep.interval swept per phrase", &[]),
            (
                "slide",
                "n01+m.sweep+m.sweep.glide_time swept per level",
                &[],
            ),
            (
                "humanize",
                "n01+m.sweep+m.sweep.rubato swept per level",
                &[],
            ),
            ("transpose", "m.sweep+m.sweep.semis swept per level", &[]),
            (
                "legato",
                "n01+m.sweep+m.sweep.rest_max swept per level",
                &[],
            ),
            (
                "note_props",
                "n01+m.sweep+m.sweep.cross_lo swept per level",
                &[],
            ),
            ("join", "n01+m.sweep+m.sweep.window swept per level", &[]),
            ("repeat", "n01+m.sweep+m.sweep.count swept per level", &[]),
            (
                "clip_mask",
                "a1+a3+a5+a8+a9 invert, expand, union, by subchannel, animated (v0.8)",
                &[],
            ),
            (
                "clip_mask",
                "n08 outside parts removed (the v0.9 kernel, neutral)",
                &[("n08.mode", 2)],
            ),
            ("clip_mask", "n06 at 64 copies", &[("n06.count", 64)]),
            ("clip_mask", "n07 over ±2 rows", &[("n07.rows", 2)]),
            ("clip_mask", "n09+n05 outline of the any-key gate", &[]),
            ("clip_mask", "n08+n03+n11 split, fade, transfer", &[]),
            (
                "clip_mask",
                "n04+m.sweep+m.sweep.offset_x offset swept by levels",
                &[],
            ),
            (
                "paint_color",
                "a2+a3+a7+a8+a10 palette, gradient, scatter, density, regions (v0.8)",
                &[],
            ),
            (
                "paint_color",
                "n09 stack depth over any key",
                &[("n09.count", 1)],
            ),
            (
                "paint_color",
                "n10 blur over ±4 rows, 8 iterations",
                &[("n10.rows", 4), ("n10.iter", 8)],
            ),
            (
                "paint_color",
                "n02 edges by 8 neighbours, interior painted",
                &[("n02.hood", 1), ("n02.interior", 1)],
            ),
            (
                "paint_color",
                "m.sweep+m.sweep.subchannel swept per note",
                &[],
            ),
            // Paint V2's base moves the subchannel every quarter note.
            ("subpaint", "a1+a9+a10 walk, regions, contrast (v0.8)", &[]),
            (
                "subpaint",
                "n01 cyclic, 16 generations",
                &[("n01.rule", 2), ("n01.gens", 16)],
            ),
            ("subpaint", "n04 at 100 steps", &[("n04.steps", 100)]),
            (
                "subpaint",
                "n02+n07 Markov by pitch, phrases by length",
                &[("n02.order", 1), ("n07.by", 2)],
            ),
            (
                "subpaint",
                "m.sweep+m.sweep.flash+n05 swept per note",
                &[("n05.ref", 1)],
            ),
            ("delete", "a4+a7+a8 cells, overlaps, duplicates (v0.8)", &[]),
            (
                "delete",
                "n01 covered within 240 ticks",
                &[("n01.tol", 240)],
            ),
            ("delete", "n09 isolated over ±12 rows", &[("n09.rows", 12)]),
            (
                "delete",
                "n11 quietest first, 1000 left",
                &[("n11.most", 1000), ("n11.first", 1)],
            ),
            (
                "delete",
                "n10 quieter of seconds and tritones",
                &[("n10.which", 2)],
            ),
            (
                "delete",
                "m.sweep+m.sweep.probability+a6 swept per note",
                &[],
            ),
            ("select", "a3+a6+a8 filter, density, component (v0.8)", &[]),
            ("select", "n01 shrink", &[("n01.mode", 1)]),
            (
                "select",
                "n05 overlaps across subchannels",
                &[("n05.kind", 3)],
            ),
            ("select", "n06 outliers by length", &[("n06.prop", 2)]),
            (
                "select",
                "n07 line from the lowest note",
                &[("n07.start", 2)],
            ),
            (
                "select",
                "m.sweep+m.sweep.metric_tol+n03 swept per note",
                &[],
            ),
            // Draw's base: one note per 1/16 along the stroke, the 1M
            // selected notes passing through.
            (
                "draw",
                "a2+a3+a7+a9 contour, magnet, symmetry, noise (v0.8)",
                &[],
            ),
            ("draw", "n01 chord list", &[("n01.follow", 2)]),
            ("draw", "n06 at a subchannel's onsets", &[("n06.ref", 1)]),
            (
                "draw",
                "n08 skipping occupied steps",
                &[("n08.existing", 2)],
            ),
            ("draw", "n07 every shape", &[("n07.shapes", 4)]),
            (
                "draw",
                "n09+n10+n11+n04+n05 rhythm, tremolo, colour, arpeggio, lines",
                &[],
            ),
            (
                "draw",
                "m.sweep+m.sweep.gate+m.sweep.time_jitter+a9 swept per position",
                &[],
            ),
            // The Note Brush's base: one note per 1/16 cell the stroke
            // passes, the 1M selected notes passing through; its editing
            // functions change the selection and the unselected notes under
            // the stroke.
            (
                "paint",
                "a3+a7+a10 chord, velocity gradient, trail (v0.8)",
                &[],
            ),
            ("paint", "n01 five rows tall", &[("n01.rows", 5)]),
            ("paint", "n02 whole rows", &[("n02.fill", 1)]),
            ("paint", "n03 radius 12", &[("n03.radius", 12)]),
            (
                "paint",
                "n05+n06+n10+n11 smudge, recolour, length, velocity",
                &[],
            ),
            ("paint", "n03+n04+n08+n09 disc, tiles, dither, scale", &[]),
            (
                "paint",
                "m.sweep+m.sweep.gate+m.sweep.soft_radius+n03 swept per cell",
                &[],
            ),
            // Snap to Scale's base: C major, the 1M notes snapped one by one
            // (n05 reads the selection itself without a document).
            (
                "snap_scale",
                "a2+a4+a5+a8 weights, melodic, chord-aware, ties (v0.8)",
                &[],
            ),
            ("snap_scale", "n01 every bar", &[("n01.window", 1)]),
            (
                "snap_scale",
                "n01 all scales every 4 bars",
                &[("n01.cands", 2), ("n01.window", 2)],
            ),
            (
                "snap_scale",
                "n03 allocating free channels",
                &[("n03.channels", 1)],
            ),
            ("snap_scale", "n06 degree triads", &[("n06.chord", 1)]),
            ("snap_scale", "n09 per beat", &[("n09.per", 1)]),
            (
                "snap_scale",
                "m.sweep+m.sweep.passing+m.sweep.shift+n02+n11 swept per note",
                &[],
            ),
            // Flip's base: the time mirror of the whole selection in one box.
            ("flip", "a2+a3+a4 diagonal, per chord, per bar (v0.8)", &[]),
            ("flip", "n01 overlay", &[("n01.place", 2)]),
            ("flip", "n06 in ticks", &[("n06.in", 1)]),
            (
                "flip",
                "n09 depth 6 alternating",
                &[("n09.depth", 6), ("n09.axis", 2)],
            ),
            ("flip", "n02+n07+n10 half, colours, swing", &[]),
            (
                "flip",
                "m.sweep+m.sweep.region_size+m.sweep.axis_range+n08+n11 swept per note",
                &[],
            ),
            // Duplicate's base: one copy after the selection (v0.8).
            (
                "duplicate",
                "a1+a3+a4+a5+a7 four copies, steps, decay, advance, mirror (v0.8)",
                &[("a1.count", 4)],
            ),
            ("duplicate", "n01 four layers", &[("n01.count", 4)]),
            ("duplicate", "n03 reference chord changes", &[("n03.at", 2)]),
            (
                "duplicate",
                "n02 inversion, four entries",
                &[("n02.kind", 2), ("n02.entries", 4)],
            ),
            ("duplicate", "n08 with pitch bend", &[("n08.include", 2)]),
            (
                "duplicate",
                "n09+n10+n04 loudest share, stutter, rotation",
                &[("n09.by", 1)],
            ),
            (
                "duplicate",
                "m.sweep+m.sweep.path_copies+m.sweep.keep+n05+n09 swept per note",
                &[],
            ),
            // Score Stretch's base: ×2 around the selection's start (v0.8).
            (
                "stretch",
                "a5+a6+a8 curve, per-subchannel factors, seconds (v0.8)",
                &[],
            ),
            (
                "stretch",
                "n01+n04 4 → 2 beats per bar, the original NPS",
                &[("n01.to", 2)],
            ),
            (
                "stretch",
                "n01+n04 4 → 2 beats per bar, a limit of 1,000, outline",
                &[("n01.to", 2), ("n04.at", 1), ("n04.keep", 1)],
            ),
            (
                "stretch",
                "n11 at the next onset of any key",
                &[("n11.at", 1)],
            ),
            (
                "stretch",
                "m.sweep+m.sweep.factor_after+m.sweep.stop_length+n10+n08 swept per note",
                &[],
            ),
            // Gravity Well's base: one well at the centre, 50 % (v0.8).
            ("gravity_well", "a1+a2+a4+a8 (v0.8 path)", &[]),
            ("gravity_well", "n01 at 64 steps", &[("n01.steps", 64)]),
            ("gravity_well", "n07 eight wells", &[("n07.wells", 8)]),
            ("gravity_well", "n08 at chord roots", &[("n08.at", 2)]),
            (
                "gravity_well",
                "n02+n03+n04+n06+n11 lens, tide, spiral, potential, dilation",
                &[],
            ),
            (
                "gravity_well",
                "m.sweep+m.sweep.strength+m.sweep.tidal+n03 swept per note",
                &[],
            ),
            // Kaleidoscope's base: two sectors (point symmetry) around the
            // box's centre (v0.8).
            ("kaleidoscope", "a1+a2+a4 (v0.8 path)", &[]),
            ("kaleidoscope", "n01 p6m", &[("n01.group", 16)]),
            ("kaleidoscope", "n04 outline", &[("n04.mode", 1)]),
            ("kaleidoscope", "n11 per bar", &[("n11.unit", 1)]),
            (
                "kaleidoscope",
                "n02+n03+n05+n07+n08+n10 spin, delay, transpose, key, break, pattern",
                &[],
            ),
            (
                "kaleidoscope",
                "m.sweep+m.sweep.turns+m.sweep.perturbation+n02+n08 swept per note",
                &[],
            ),
            // Orbit's base: eight copies on a circle, motif size 35 % (v0.8).
            ("orbit", "a1+a3+a5 (v0.8 path)", &[]),
            ("orbit", "n06 at 5 %", &[("n06.spacing", 5)]),
            ("orbit", "n10 four moons", &[("n10.moons", 4)]),
            (
                "orbit",
                "n02+n03+n04+n08+n09 tilt, Kepler, spin, planet, precession",
                &[],
            ),
            (
                "orbit",
                "m.sweep+m.sweep.tilt+m.sweep.spin+n02+n04 swept per note",
                &[],
            ),
            // Vortex Trails' base: three trails turned 30° apart, radius
            // 60 % (v0.8).
            ("vortex_trails", "a1+a8+a10 (v0.8 path)", &[]),
            ("vortex_trails", "n03 core", &[]),
            ("vortex_trails", "n09 both sides", &[("n09.side", 2)]),
            ("vortex_trails", "n05+n07+n10 rhythm, comet, strobe", &[]),
            (
                "vortex_trails",
                "n01+n02+n04+n06+n08+n11 delay, transpose, merger, tilt, depth, morph",
                &[],
            ),
            (
                "vortex_trails",
                "m.sweep+m.sweep.delay+m.sweep.tilt+n01+n06 swept per note",
                &[],
            ),
            ("join", "n06+n01 expression over beat windows", &[]),
            (
                "join",
                "n02+n03+n04+n11 gates under targets",
                &[("n04.nps", 10), ("n04.gap", 100_000), ("n11.most", 2)],
            ),
        ];
        match pick {
            Some("off") => {}
            Some(f) => cases.extend(
                fns.iter()
                    .filter(|x| **x == f)
                    .map(|x| ((*x).to_owned(), vec![*x])),
            ),
            None => {
                cases.extend(fns.iter().map(|f| ((*f).to_owned(), vec![*f])));
                cases.push(("all new".into(), fns.to_vec()));
            }
        }
        for (t, name, _) in extra {
            if t == id && pick.is_none_or(|f| name.starts_with(f)) && pick != Some("off") {
                // The leading ids (joined by '+') are switched on.
                let ids = name.split(' ').next().unwrap_or("");
                cases.push(((*name).to_owned(), ids.split('+').collect()));
            }
        }
        for (name, on) in cases {
            let mut set = ParamSet::defaults(&specs);
            if *id == "transpose" {
                // TR_BASE: moved notes are clamped, not refused.
                set.set("b.pitch", ParamValue::Choice(1));
            }
            for f in &on {
                set.set(f, ParamValue::Bool(true));
            }
            if let Some((_, _, ints)) = extra.iter().find(|e| e.0 == *id && e.1 == name) {
                for (k, v) in *ints {
                    let choice = specs.iter().any(|p| {
                        p.key == *k
                            && matches!(
                                p.kind,
                                plumspirit_engine::transform::param::ParamKind::Choice(_)
                            )
                    });
                    set.set(
                        k,
                        if choice {
                            ParamValue::Choice(*v as u32)
                        } else {
                            ParamValue::Int(*v)
                        },
                    );
                }
            }
            let b = BuildCtx {
                ctx,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let mut best = Duration::MAX;
            let mut out = Ok((0, 0));
            for _ in 0..3 {
                let (r, t) = time(|| {
                    def.kernel(&set, &b).and_then(|k| {
                        run_kernel(
                            k.as_ref(),
                            src,
                            def.boundary(&set),
                            &Limits::default(),
                            None,
                        )
                    })
                });
                best = best.min(t);
                out = r
                    .map(|r| (r.notes.len(), r.events.len()))
                    .map_err(|e| e.user_message());
            }
            match out {
                Ok((n, 0)) => println!("- {id} {name}: {} ({} out)", ms(best), group(n as u64)),
                Ok((n, e)) => println!(
                    "- {id} {name}: {} ({} out, {} events)",
                    ms(best),
                    group(n as u64),
                    group(e as u64)
                ),
                Err(e) => println!("- {id} {name}: refused in {}: {e}", ms(best)),
            }
        }
    }
}

/// V09-S43 / S44: tool `id` on `n` selected Dense notes through the
/// platform (kernel build, the run, the output's sort), best of 3 per case;
/// a case runs on its first `limit` notes (Fill's shared bound counts 129
/// rows a note).
type ToolCase = (
    &'static str,
    Vec<(&'static str, plumspirit_engine::transform::ParamValue)>,
    usize,
);

fn tool_cases(n: usize, id: &str, title: &str, cases: Vec<ToolCase>) {
    use plumspirit_engine::transform::registry::BuildCtx;
    use plumspirit_engine::transform::{
        Ctx, Limits, ParamSet, Source, TrackScope, run_kernel, tool,
    };
    let mut doc = generate(FixtureShape::Dense, n, 960, 42);
    let track = (0..doc.track_count())
        .max_by_key(|&i| doc.track(i).map_or(0, |t| t.notes.len()))
        .expect("track");
    doc.track_mut(track).expect("track").notes.select_all();
    let scope = [TrackScope {
        index: track,
        all: false,
        content: None,
    }];
    let all = Source::collect(&doc, &scope, 1 << 28).expect("source");
    let ctx = Ctx::simple(960);
    let def = tool(id).expect("tool");
    let specs = (def.spec)().params;
    println!(
        "## {title}: {} selected notes, Dense",
        group(all.notes.len() as u64)
    );
    for (name, set_kv, limit) in cases {
        let part;
        let src = if limit < all.notes.len() {
            part = Source::from_notes(all.notes[..limit].to_vec(), all.tracks.clone());
            &part
        } else {
            &all
        };
        let mut set = ParamSet::defaults(&specs);
        for (k, v) in set_kv {
            set.set(k, v);
        }
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let mut best = Duration::MAX;
        let mut out = Ok(0);
        for _ in 0..3 {
            let (r, d) = time(|| {
                def.kernel(&set, &b).and_then(|k| {
                    run_kernel(
                        k.as_ref(),
                        src,
                        def.boundary(&set),
                        &Limits::default(),
                        None,
                    )
                })
            });
            best = best.min(d);
            out = r.map(|r| r.notes.len()).map_err(|e| e.user_message());
        }
        match out {
            Ok(m) => println!(
                "- {name}: {} ({} out; {} ns per source note)",
                ms(best),
                group(m as u64),
                best.as_nanos() / src.notes.len().max(1) as u128
            ),
            Err(e) => println!("- {name}: refused in {}: {e}", ms(best)),
        }
    }
    println!("- {}", mem());
}

/// V09-S43: the 3D tool.
fn tool3d_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let t = |k: &'static str, v: &str| (k, ParamValue::Text(v.into()));
    let all = usize::MAX;
    tool_cases(
        n,
        "tool3d",
        "v0.9 3D tool (V09-S43)",
        vec![
            (
                "defaults (radial depth, camera 25° / 15°, perspective, sampled at the middle)",
                vec![],
                all,
            ),
            (
                "split along the projected path (8 segments)",
                vec![c("g.raster", 2)],
                all,
            ),
            (
                "depth from time + depth-based velocity / gate / subchannel / scale",
                vec![
                    c("z.source", 2),
                    on("d.vel"),
                    on("d.gate"),
                    on("d.sub"),
                    on("d.scale"),
                ],
                all,
            ),
            (
                "noise depth + nearer notes hide farther ones",
                vec![c("z.source", 8), c("occl", 1)],
                all,
            ),
            (
                "custom field formula",
                vec![
                    c("z.source", 10),
                    t("z.field", "0.5 + 0.5*sin(2*pi*u)*cos(2*pi*v)"),
                ],
                all,
            ),
            (
                "Fill of stretched rows (camera pitch 60°; the first 100,000 notes)",
                vec![("cam.pitch", ParamValue::Float(60.0)), on("g.fill")],
                100_000,
            ),
        ],
    );
}

/// V09-S44: Warp Transform.
fn warp_transform_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let all = usize::MAX;
    tool_cases(
        n,
        "warp_transform",
        "v0.9 Warp Transform (V09-S44)",
        vec![
            (
                "scale 70 % from the left + rotate 15° + shear (Transform Existing)",
                vec![
                    f("scale_x", 70.0),
                    c("anchor_x", 1),
                    f("rotate", 15.0),
                    f("shear_x", 20.0),
                ],
                all,
            ),
            (
                "swivel 30° + tilt 20° + perspective 0.5 (Transform Existing)",
                vec![f("swivel", 30.0), f("tilt", 20.0), f("persp", 0.5)],
                all,
            ),
            (
                "shift past the edge + Wrap X Tile (Transform Existing folds, N → N)",
                vec![("shift_x", ParamValue::Int(1 << 20)), c("wrap_x", 1)],
                all,
            ),
            (
                "quantized to the snap",
                vec![f("scale_x", 37.0), c("quant", 1)],
                all,
            ),
            (
                "Fill / Duplicate: squeeze 25 % from the left + Tile (4 copies)",
                vec![
                    f("scale_x", 25.0),
                    c("anchor_x", 1),
                    c("density", 1),
                    c("wrap_x", 1),
                ],
                all,
            ),
            (
                "Fill / Duplicate: split along the path (8) + Scale Y 200 % + rotate 10° (the first 250,000 notes)",
                vec![
                    c("density", 1),
                    c("raster", 2),
                    f("scale_y", 200.0),
                    f("rotate", 10.0),
                ],
                250_000,
            ),
        ],
    );
}

/// V09-S45: Trails.
fn trails_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let all = usize::MAX;
    tool_cases(
        n,
        "trails",
        "v0.9 Trails (V09-S45)",
        vec![
            ("defaults: 6 trails, Auto Fade (7 copies)", vec![], all),
            (
                "4 trails: scale 90 % + rotate 5° + tumble 10° + Z 10 %, feedback 110 %",
                vec![
                    i("count", 4),
                    f("scale_x", 90.0),
                    f("scale_y", 90.0),
                    f("rotate", 5.0),
                    f("tumble", 10.0),
                    f("pos_z", 10.0),
                    f("fb.scale", 110.0),
                    f("fb.rot", 110.0),
                    f("fb.z", 110.0),
                ],
                all,
            ),
            (
                "3 before + 3 after, Manual Fade 80 %, gate 90 %, subchannel +1 per trail",
                vec![
                    i("count", 3),
                    i("before", 3),
                    c("fade", 2),
                    f("gate", 90.0),
                    i("sub_step", 1),
                ],
                all,
            ),
            (
                "seeded time jitter on every trail, Current time",
                vec![("jitter", ParamValue::Bool(true)), c("state", 1)],
                all,
            ),
            (
                "100 trails (the first 100,000 notes)",
                vec![i("count", 100)],
                100_000,
            ),
        ],
    );
}

/// V09-S46: Note Outline.
fn note_outline_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let b = |k: &'static str, v: bool| (k, ParamValue::Bool(v));
    let all = usize::MAX;
    tool_cases(
        n,
        "note_outline",
        "v0.9 Note Outline (V09-S46)",
        vec![
            ("defaults: outer, 1 cell / 1 row, the art kept", vec![], all),
            (
                "inner, 2 cells / 1 row, the art kept and carved",
                vec![c("mode", 1), i("thick_x", 2)],
                all,
            ),
            (
                "both, 3 cells / 2 rows, offset 1, cut corners",
                vec![
                    c("mode", 2),
                    i("thick_x", 3),
                    i("thick_y", 2),
                    i("offset", 1),
                    c("corners", 1),
                ],
                all,
            ),
            (
                "simplification 2 on the quantized art, the outline only",
                vec![i("simplify", 2), b("quantize", true), b("keep", false)],
                all,
            ),
            (
                "dashes of 60 every 120 ticks, the outline only",
                vec![i("dash", 60), i("gap", 60), b("keep", false)],
                all,
            ),
        ],
    );
}

/// V09-S47: Displacement Map — `n` selected Dense notes moved by a second
/// Dense fixture's notes (the map track) or a 1024 × 1024 picture, best of 3
/// per case (kernel build, the field, the run, the output's sort).
fn displacement_map_bench(n: usize) {
    use plumspirit_engine::transform::image_midi::Rgba;
    use plumspirit_engine::transform::registry::BuildCtx;
    use plumspirit_engine::transform::{
        Ctx, Limits, ParamSet, ParamValue, Source, TNote, TrackScope, run_kernel, tool,
    };
    let collect = |seed: u64| {
        let mut doc = generate(FixtureShape::Dense, n, 960, seed);
        let track = (0..doc.track_count())
            .max_by_key(|&i| doc.track(i).map_or(0, |t| t.notes.len()))
            .expect("track");
        doc.track_mut(track).expect("track").notes.select_all();
        let scope = [TrackScope {
            index: track,
            all: false,
            content: None,
        }];
        Source::collect(&doc, &scope, 1 << 28).expect("source")
    };
    let mut src = collect(42);
    let map: Vec<TNote> = collect(7)
        .notes
        .into_iter()
        .map(|t| TNote { track: 1, ..t })
        .collect();
    let mut tracks = src.tracks.clone();
    tracks.push(tracks[0].clone());
    src.tracks = tracks;
    src.aux = std::sync::Arc::new(vec![map]);
    src.aux_slot = Some(1);
    let side = 1024u32;
    let px = (0..side * side)
        .map(|i| {
            let (x, y) = (i % side, i / side);
            [(x / 4) as u8, (y / 4) as u8, ((x ^ y) & 255) as u8, 255]
        })
        .collect();
    let pic = std::sync::Arc::new(Rgba::new(side, side, px).expect("picture"));
    let def = tool("displacement_map").expect("tool");
    let specs = (def.spec)().params;
    println!(
        "## v0.9 Displacement Map (V09-S47): {} selected notes, Dense; map track {} notes",
        group(src.notes.len() as u64),
        group(src.aux[0].len() as u64)
    );
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let cases: Vec<(&str, Vec<(&'static str, ParamValue)>)> = vec![
        (
            "track map: density (time) / velocity (pitch), sampled at the middle",
            vec![],
        ),
        ("track map with blur 4", vec![i("blur", 4)]),
        (
            "track map, split along the field (4 segments)",
            vec![c("g.raster", 2), i("g.segments", 4)],
        ),
        (
            "1024 × 1024 picture stretched: luma / green",
            vec![c("source", 3), c("v.picture", 2)],
        ),
        (
            "Vice Versa: the selection maps, the map track's notes move",
            vec![("vice", ParamValue::Bool(true))],
        ),
    ];
    for (name, kv) in cases {
        let mut set = ParamSet::defaults(&specs);
        for (k, v) in kv {
            set.set(k, v);
        }
        let mut ctx = Ctx::simple(960);
        ctx.picture = Some(Ok(pic.clone()));
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let mut best = Duration::MAX;
        let mut out = Ok(0);
        for _ in 0..3 {
            let (r, d) = time(|| {
                def.kernel(&set, &b).and_then(|k| {
                    run_kernel(
                        k.as_ref(),
                        &src,
                        def.boundary(&set),
                        &Limits::default(),
                        None,
                    )
                })
            });
            best = best.min(d);
            out = r.map(|r| r.notes.len()).map_err(|e| e.user_message());
        }
        match out {
            Ok(m) => println!(
                "- {name}: {} ({} out; {} ns per source note)",
                ms(best),
                group(m as u64),
                best.as_nanos() / src.notes.len().max(1) as u128
            ),
            Err(e) => println!("- {name}: refused in {}: {e}", ms(best)),
        }
    }
    println!("- {}", mem());
}

/// V09-S48: Tiny Planet.
fn tiny_planet_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let all = usize::MAX;
    tool_cases(
        n,
        "tiny_planet",
        "v0.9 Tiny Planet (V09-S48)",
        vec![
            (
                "defaults: one turn, a planet (Transform Existing)",
                vec![],
                all,
            ),
            (
                "latitude −45°, longitude 30°, stereographic, inverted, subchannel phase",
                vec![
                    f("latitude", -45.0),
                    f("longitude", 30.0),
                    c("radius.map", 1),
                    ("invert", ParamValue::Bool(true)),
                    c("phase", 1),
                ],
                all,
            ),
            (
                "a turn a bar with a 10 % spiral",
                vec![c("turn", 1), c("turn.value", 0), f("spiral", 10.0)],
                all,
            ),
            (
                "follow the arcs (8 segments)",
                vec![c("density", 1), i("density.segments", 8)],
                all,
            ),
        ],
    );
}

/// V09-S49: Ripple.
fn ripple_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let all = usize::MAX;
    tool_cases(
        n,
        "ripple",
        "v0.9 Ripple (V09-S49)",
        vec![
            ("defaults: one source, sine", vec![], all),
            (
                "8 sources (sum), perpendicular height, Super twirl, triangle",
                vec![i("sources", 8), f("perp", 4.0), c("twirl", 2), c("wave", 1)],
                all,
            ),
            (
                "chaos 40 %, light 60 %, stretch 200 % at 30°, circle wave",
                vec![
                    f("chaos", 40.0),
                    f("light", 60.0),
                    f("stretch", 200.0),
                    f("stretch.angle", 30.0),
                    c("wave", 4),
                ],
                all,
            ),
            (
                "split along the waves (6 segments), radius 60 %",
                vec![c("g.raster", 2), i("g.segments", 6), f("radius", 60.0)],
                all,
            ),
        ],
    );
}

/// V09-S50: Tumours.
fn tumours_bench(n: usize) {
    use plumspirit_engine::transform::ParamValue;
    let c = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let all = usize::MAX;
    tool_cases(
        n,
        "tumours",
        "v0.9 Tumours (V09-S50)",
        vec![
            ("defaults: radius 3, runs", vec![], all),
            (
                "lobes 6, asymmetry, bulge pressure, density + curvature reactive",
                vec![
                    on("c02_lobe_count"),
                    i("c02_lobe_count.count", 6),
                    on("c03_lobe_asymmetry"),
                    on("c04_bulge_pressure"),
                    on("c15_density_reactive_growth"),
                    on("c16_curvature_reactive_growth"),
                ],
                all,
            ),
            (
                "branches (depth 3), nodules, daughters, roughness, Bridge fusion",
                vec![
                    on("c05_branch_probability"),
                    f("c05_branch_probability.probability", 5.0),
                    on("c06_branch_depth"),
                    on("c07_nodule_frequency"),
                    on("c22_daughter_growth"),
                    on("c09_edge_roughness"),
                    on("c13_fusion"),
                ],
                all,
            ),
            (
                "veins, cavities, mutation, cells, 4 growth rings",
                vec![
                    on("c10_vein_density"),
                    on("c12_cavity_void_generation"),
                    on("c19_subchannel_mutation"),
                    on("c20_velocity_mutation"),
                    on("c21_gate_mutation"),
                    c("form", 1),
                    on("c25_evolution_morph"),
                    c("c25_evolution_morph.output", 2),
                ],
                all,
            ),
        ],
    );
}

fn group(v: u64) -> String {
    plumspirit_engine::midi::group_digits(v)
}

/// V09-S51: the generator pen on a zig-zag stroke of `n` segments between
/// rows 60 and 61 (one note per segment after merging), best of five.
fn generator_engine(n: usize) {
    use plumspirit_engine::transform::generator::{Band, Brush, Counter, Draw, Emitter, Pen};
    use plumspirit_engine::transform::{Boundary, Output, TNote};
    let pts: Vec<(f64, f64)> = (0..=n)
        .map(|i| (i as f64 * 120.0, 60.5 + (i % 2) as f64))
        .collect();
    let brush = Brush {
        vel: 100,
        gate: 1.0,
        sub: 0,
    };
    let band = Band::around(128);
    let best = |f: &mut dyn FnMut() -> usize| -> (usize, Duration) {
        let mut best = Duration::MAX;
        let mut k = 0;
        for _ in 0..5 {
            let (c, t) = time(&mut *f);
            k = c;
            best = best.min(t);
        }
        (k, best)
    };
    println!("## v0.9 generator engine: a stroke of {n} segments, step 60");
    let (count, tc) = best(&mut || {
        let mut pen = Pen::new(Counter::default(), 60, band);
        pen.stroke(&pts, brush);
        pen.finish().notes as usize
    });
    println!("- counting pass: {} ({count} notes)", ms(tc));
    let tpl = TNote {
        rel: 64,
        ..TNote::default()
    };
    let mut kept: Vec<TNote> = Vec::new();
    let (made, te) = best(&mut || {
        let mut out = Output::new(Boundary::default(), usize::MAX);
        {
            let mut pen = Pen::new(
                Emitter {
                    out: &mut out,
                    tpl,
                    subs: &[],
                },
                60,
                band,
            );
            pen.stroke(&pts, brush);
            pen.finish();
        }
        let k = out.notes.len();
        kept = std::mem::take(&mut out.notes);
        k
    });
    println!("- emitting pass: {} ({made} notes)", ms(te));
    let (plain, tp) = best(&mut || {
        let mut out = Output::new(Boundary::default(), usize::MAX);
        for x in &kept {
            out.emit(
                i128::from(x.start),
                i128::from(x.dur),
                i64::from(x.pitch),
                i64::from(x.vel),
                x,
            );
        }
        out.notes.len()
    });
    println!("- plain Output::emit loop: {} ({plain} notes)", ms(tp));
    println!("- {}", mem());
}

/// V09-S52: Lines through the platform (its kernel, then `run_kernel`: the
/// counting preflight and the emitting pass) at about `n` generated notes,
/// per function family; best of three. A box of `n` one-tick steps over
/// the whole keyboard.
fn lines_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.lines").expect("gen.lines");
    let doc = PlumDocument::new_project("lines".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let cases: Vec<Case> = vec![
        (
            "c06 a note every grid unit (the diagonal, n steps)",
            vec![
                on("c06_note_spacing_density"),
                f("c06_note_spacing_density.every", 1.0),
                len(n),
            ],
        ),
        (
            "c09 velocity gradient on the diagonal of n steps (at most 1,024 pieces)",
            vec![on("c09_velocity_gradient"), len(n)],
        ),
        (
            "c25 depth 10 + c10 subchannel gradient 0 – 4,095 (one-unit pieces)",
            vec![
                on("c25_recursive_line_subdivision"),
                i("c25_recursive_line_subdivision.depth", 10),
                on("c10_subchannel_gradient"),
                i("c10_subchannel_gradient.to", 4095),
                len(n),
            ],
        ),
        (
            "c11 dashes of one step every two (a horizontal of 2n steps)",
            vec![
                on("c03_polyline_segments"),
                ("c03_polyline_segments.heights", ParamValue::List(vec![50])),
                on("c11_dash_pattern"),
                i("c11_dash_pattern.dash", 1),
                i("c11_dash_pattern.gap", 1),
                len(2 * n),
            ],
        ),
        (
            "c05 32 copies × c16 fan 4 × c06 (n / 128 steps each)",
            vec![
                on("c05_parallel_copies"),
                i("c05_parallel_copies.count", 32),
                i("c05_parallel_copies.pitch", 1),
                on("c16_radial_fan"),
                i("c16_radial_fan.count", 4),
                f("c16_radial_fan.spread", 10.0),
                on("c06_note_spacing_density"),
                f("c06_note_spacing_density.every", 1.0),
                len(n / 128),
            ],
        ),
        (
            "c04 thickness 8 + c11 dashes (a horizontal of n / 4 steps; one polygon per dash)",
            vec![
                on("c03_polyline_segments"),
                ("c03_polyline_segments.heights", ParamValue::List(vec![50])),
                on("c04_line_thickness"),
                i("c04_line_thickness.rows", 8),
                on("c11_dash_pattern"),
                i("c11_dash_pattern.dash", 1),
                i("c11_dash_pattern.gap", 1),
                len(n / 4),
            ],
        ),
        (
            "c18 fill to the bottom + c06 (the diagonal of n / 64 steps)",
            vec![
                on("c18_scanline_fill_between_boundaries"),
                on("c06_note_spacing_density"),
                f("c06_note_spacing_density.every", 1.0),
                len(n / 64),
            ],
        ),
        (
            "c06 + c20 scale + c21 grid 50 % + c22 beat accents (the maps)",
            vec![
                on("c06_note_spacing_density"),
                f("c06_note_spacing_density.every", 1.0),
                on("c20_scale_pitch_snap"),
                on("c21_time_grid_snap"),
                f("c21_time_grid_snap.strength", 50.0),
                on("c22_intersection_accent"),
                ("c22_intersection_accent.lines", ParamValue::Bool(false)),
                len(n),
            ],
        ),
        (
            "c25 depth 12 × c19 32 passes × c24 reflect + c06 (a 131,073-point path)",
            vec![
                on("c25_recursive_line_subdivision"),
                i("c25_recursive_line_subdivision.depth", 12),
                on("c19_alternating_direction"),
                i("c19_alternating_direction.passes", 32),
                on("c24_reflect_boundary"),
                on("c06_note_spacing_density"),
                f("c06_note_spacing_density.every", 1.0),
                len(n),
            ],
        ),
    ];
    println!("## v0.9 Lines generator (V09-S52): about {n} notes, one-tick steps, keys 0 – 127");
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// Runs generator `def` per case (keys 0 – 127, one-tick steps) through the
/// platform: its kernel, the counting preflight alone, then a fresh kernel
/// through `run_kernel`; best of three.
fn gen_cases(
    def: &plumspirit_engine::transform::ToolDef,
    c: &plumspirit_engine::transform::Ctx,
    src: &plumspirit_engine::transform::Source,
    cases: Vec<(
        &'static str,
        Vec<(&'static str, plumspirit_engine::transform::ParamValue)>,
    )>,
) {
    use plumspirit_engine::transform::registry::BuildCtx;
    use plumspirit_engine::transform::{Limits, ParamSet, ParamValue, run_kernel};
    for (name, kv) in cases {
        let mut set = ParamSet::defaults(&(def.spec)().params);
        for (k, v) in [
            ("gen.step", ParamValue::Choice(11)),
            ("gen.step_ticks", ParamValue::Int(1)),
            ("gen.low", ParamValue::Int(0)),
            ("gen.high", ParamValue::Int(127)),
        ] {
            set.set(k, v);
        }
        for (k, v) in kv {
            set.set(k, v);
        }
        let b = BuildCtx {
            ctx: c,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let mut best_count = Duration::MAX;
        let mut best_run = Duration::MAX;
        let mut notes = 0;
        let mut bound = 0;
        for _ in 0..3 {
            let k = def.kernel(&set, &b).expect("kernel");
            bound = k.estimate(&src.stats).notes;
            let (_, tc) = time(|| k.estimate_source(src).notes);
            best_count = best_count.min(tc);
            // A fresh kernel: the run builds the path and counts again.
            let k = def.kernel(&set, &b).expect("kernel");
            let (r, tr) = time(|| {
                run_kernel(
                    k.as_ref(),
                    src,
                    def.boundary(&set),
                    &Limits::default(),
                    None,
                )
                .expect("run")
            });
            best_run = best_run.min(tr);
            notes = r.notes.len();
        }
        let per = best_run.as_nanos() as f64 / notes.max(1) as f64;
        println!(
            "- {name}: {} notes (bound {}) — counting pass {}, whole run {} ({per:.0} ns per note)",
            group(notes as u64),
            group(bound),
            ms(best_count),
            ms(best_run)
        );
    }
}

/// V09-S53: Rectangle / Square through the platform per function family at
/// about `n` generated notes (keys 0 – 127, one-tick steps); best of three.
fn rect_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.rect").expect("gen.rect");
    let doc = PlumDocument::new_project("rect".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let cases: Vec<Case> = vec![
        (
            "c15 vertical stripes every 2 steps (n / 63 steps × 128 rows)",
            vec![on("c15_vertical_stripe_fill"), len(n / 63)],
        ),
        (
            "c16 crosshatch every 4 steps (n / 63 steps)",
            vec![on("c16_crosshatch_fill"), len(n / 63)],
        ),
        (
            "c05 filled between c17 grid lines every 4 (n / 24 steps)",
            vec![on("c05_filled_mode"), on("c17_grid_fill"), len(n / 24)],
        ),
        (
            "c20 every 2nd cell × c06 border 64 (the whole box is outline; n / 64 steps)",
            vec![
                on("c20_per_edge_density"),
                on("c06_border_thickness"),
                i("c06_border_thickness.size", 64),
                len(n / 64),
            ],
        ),
        (
            "c23 dashes 1 / 1 + c19 per-edge subchannels × c06 border 64 (n / 64 steps)",
            vec![
                on("c23_dashed_border_phase"),
                i("c23_dashed_border_phase.dash", 1),
                i("c23_dashed_border_phase.gap", 1),
                on("c19_per_edge_subchannels"),
                on("c06_border_thickness"),
                i("c06_border_thickness.size", 64),
                len(n / 64),
            ],
        ),
        (
            "c10 shear 30° + c11 perspective + c15 stripes (n / 47 steps)",
            vec![
                on("c10_shear"),
                on("c11_perspective"),
                on("c15_vertical_stripe_fill"),
                len(n / 47),
            ],
        ),
        (
            "c05 filled + c21 velocity 127 → 1 + c22 gate around (1,024 pieces per row; n steps)",
            vec![
                on("c05_filled_mode"),
                on("c21_velocity_gradient"),
                i("c21_velocity_gradient.to", 1),
                on("c22_gate_gradient"),
                ch("c22_gate_gradient.direction", 2),
                len(n),
            ],
        ),
        (
            "c12 64 nested × c13 spacing 1 + c07 radius 64 + c09 (many rings; n steps)",
            vec![
                on("c12_nested_rectangles"),
                i("c12_nested_rectangles.count", 64),
                on("c13_concentric_spacing"),
                i("c13_concentric_spacing.spacing", 1),
                on("c07_rounded_corners"),
                f("c07_rounded_corners.radius", 64.0),
                on("c09_rotation"),
                len(n),
            ],
        ),
        (
            "c24 depth 8 + c25 hole + c16 + c18 + c15 + c17 (unions and walls; n / 48 steps)",
            vec![
                on("c24_recursive_subdivision"),
                i("c24_recursive_subdivision.depth", 8),
                on("c25_hole_negative_interior_mask"),
                on("c16_crosshatch_fill"),
                on("c18_diagonal_corner_lines"),
                on("c15_vertical_stripe_fill"),
                i("c15_vertical_stripe_fill.spacing", 7),
                on("c17_grid_fill"),
                i("c17_grid_fill.spacing", 9),
                len(n / 48),
            ],
        ),
    ];
    println!(
        "## v0.9 Rectangle / Square generator (V09-S53): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S54: Circles through the platform per function family at about `n`
/// generated notes (keys 0 – 127, one-tick steps: the inscribed ellipse is
/// as wide as the box and 128 rows tall); best of three.
fn circles_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, Curve, ParamValue, TrackScope, tool};
    let def = tool("gen.circles").expect("gen.circles");
    let doc = PlumDocument::new_project("circles".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let flat =
        |k: &'static str, v: f64| (k, ParamValue::Curve(Curve::new(vec![(0.0, v), (1.0, v)])));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let cases: Vec<Case> = vec![
        (
            "c06 filled + c12 radial density ½ × c21 10 centres in a row (~2M cells, each kept or not; n / 50 steps)",
            vec![
                on("c06_filled_disc"),
                on("c12_radial_density_graph"),
                flat("c12_radial_density_graph.curve", 0.5),
                on("c21_multiple_centers"),
                i("c21_multiple_centers.count", 10),
                len(n / 50),
            ],
        ),
        (
            "c05 band 64 + c16 arc dashes 1 / 1 × c21 10 centres in a row (all outline, cell by cell; n / 50 steps)",
            vec![
                on("c05_ring_thickness"),
                i("c05_ring_thickness.size", 64),
                on("c16_dashed_arc_pattern"),
                i("c16_dashed_arc_pattern.dash", 1),
                i("c16_dashed_arc_pattern.gap", 1),
                on("c21_multiple_centers"),
                i("c21_multiple_centers.count", 10),
                len(n / 50),
            ],
        ),
        (
            "c18 chords at 30° every 2 units (line features; n / 25 steps)",
            vec![
                on("c18_chord_line_fill"),
                f("c18_chord_line_fill.angle", 30.0),
                i("c18_chord_line_fill.spacing", 2),
                len(n / 25),
            ],
        ),
        (
            "c06 filled + c11 angular density ½ + c15 subchannel by angle + c14 gate by radius × c21 10 centres (n / 50 steps)",
            vec![
                on("c06_filled_disc"),
                on("c11_angular_density_graph"),
                flat("c11_angular_density_graph.curve", 0.5),
                on("c15_subchannel_by_angle"),
                on("c14_gate_by_radius"),
                on("c21_multiple_centers"),
                i("c21_multiple_centers.count", 10),
                len(n / 50),
            ],
        ),
        (
            "c21 64 centres in a grid × c07 8 rings + c13 velocity by radius (1,152 rings; n / 50 steps)",
            vec![
                on("c21_multiple_centers"),
                i("c21_multiple_centers.count", 64),
                ch("c21_multiple_centers.layout", 2),
                on("c07_concentric_rings"),
                i("c07_concentric_rings.count", 8),
                on("c13_velocity_by_radius"),
                len(n / 50),
            ],
        ),
        (
            "c19 64 waves + c20 roughness + c07 32 rings (star-shaped rings, active edge tables; n / 50 steps)",
            vec![
                on("c19_wave_modulated_radius"),
                i("c19_wave_modulated_radius.waves", 64),
                on("c20_seeded_radius_roughness"),
                on("c07_concentric_rings"),
                i("c07_concentric_rings.count", 32),
                len(n / 50),
            ],
        ),
        (
            "c25 packing 16 × depth 2 (272 circles) + c10 spiral 16 turns (n / 50 steps)",
            vec![
                on("c25_recursive_circle_packing"),
                i("c25_recursive_circle_packing.count", 16),
                i("c25_recursive_circle_packing.depth", 2),
                on("c10_spiral_fill"),
                f("c10_spiral_fill.turns", 16.0),
                len(n / 50),
            ],
        ),
    ];
    println!("## v0.9 Circles generator (V09-S54): about {n} notes, one-tick steps, keys 0 – 127");
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S56: Polygons through the platform per function family at about
/// `n` generated notes (keys 0 – 127, one-tick steps: the base hexagon is as
/// wide as the box and 128 rows tall); best of three.
fn polygons_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.polygons").expect("gen.polygons");
    let doc = PlumDocument::new_project("polygons".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let list = |k: &'static str, v: &[i64]| (k, ParamValue::List(v.to_vec()));
    let cases: Vec<Case> = vec![
        (
            "c20 density 2 (a note every 2nd cell along each edge of the base hexagon's outline; n × 1.15 steps)",
            vec![
                on("c20_per_edge_density"),
                list("c20_per_edge_density.every", &[2]),
                len(n * 23 / 20),
            ],
        ),
        (
            "c20 density 2 + c18 subchannels by edge + c21 velocity / gate along each edge + c19 accents (brushes cut per part; n × 1.15 steps)",
            vec![
                on("c20_per_edge_density"),
                list("c20_per_edge_density.every", &[2]),
                on("c18_per_edge_subchannels"),
                on("c21_velocity_gate_edge_gradients"),
                on("c19_vertex_accents"),
                len(n * 23 / 20),
            ],
        ),
        (
            "c06 star + c13 16 concentric + c20 density 2 (general rings, nearest edges by the tree; n / 9 steps)",
            vec![
                on("c06_star_inner_radius_mode"),
                on("c13_concentric_polygons"),
                i("c13_concentric_polygons.count", 16),
                on("c20_per_edge_density"),
                list("c20_per_edge_density.every", &[2]),
                len(n / 9),
            ],
        ),
        (
            "c04 radius 60 + c22 twist 180° + c25 morph 50 % + c13 16 concentric + c20 density 2 (dense twisted rings, ≤ 64 pieces per edge, cut by the dashes)",
            vec![
                on("c04_radius"),
                f("c04_radius.radius", 60.0),
                on("c22_twist_skew"),
                f("c22_twist_skew.twist", 180.0),
                on("c25_seeded_vertex_perturbation_shape_morph"),
                f("c25_seeded_vertex_perturbation_shape_morph.amount", 0.0),
                f("c25_seeded_vertex_perturbation_shape_morph.morph", 50.0),
                on("c13_concentric_polygons"),
                i("c13_concentric_polygons.count", 16),
                on("c20_per_edge_density"),
                list("c20_per_edge_density.every", &[2]),
                len(240),
            ],
        ),
        (
            "c01 64 sides + c11 triangulated, 4 subdivisions + c10 spokes (2,900 line features; n / 4 steps)",
            vec![
                on("c01_side_count"),
                i("c01_side_count.sides", 64),
                on("c11_triangulated_fill"),
                i("c11_triangulated_fill.levels", 4),
                on("c10_radial_spokes"),
                len(n / 4),
            ],
        ),
        (
            "c24 hexagonal packing of radius 2, gap 1 + c08 filled + c19 vertex accents (about 2,000 tiles, near the cap; 330 steps)",
            vec![
                on("c24_tessellation_tiling"),
                ch("c24_tessellation_tiling.layout", 1),
                f("c24_tessellation_tiling.radius", 2.0),
                f("c24_tessellation_tiling.gap", 1.0),
                on("c08_filled_polygon"),
                on("c19_vertex_accents"),
                len(330),
            ],
        ),
    ];
    println!("## v0.9 Polygons generator (V09-S56): about {n} notes, one-tick steps, keys 0 – 127");
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S57: Custom Shape through the platform per function family at about
/// `n` generated notes (keys 0 – 127, one-tick steps: the base arrow is as
/// wide as the box and 128 rows tall); best of three.
fn custom_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.custom").expect("gen.custom");
    let doc = PlumDocument::new_project("custom".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let list = |k: &'static str, v: &[i64]| (k, ParamValue::List(v.to_vec()));
    let cases: Vec<Case> = vec![
        (
            "c11 density 2 (a note every 2nd cell along each edge of the base arrow's outline; n steps)",
            vec![
                on("c11_per_edge_density"),
                list("c11_per_edge_density.every", &[2]),
                len(n),
            ],
        ),
        (
            "c11 density 2 + c13 subchannels + c14 per-vertex velocity + c15 gate profile (brushes cut per cell; n steps)",
            vec![
                on("c11_per_edge_density"),
                list("c11_per_edge_density.every", &[2]),
                on("c13_per_edge_subchannel"),
                on("c14_per_vertex_velocity"),
                on("c15_gate_profile"),
                len(n),
            ],
        ),
        (
            "c12 per-vertex density 50 % on 3,000 × 63 tiles (dithered cell by cell along each edge — a piece is cut into at most 4,096 parts; ~330 tiles, n / 2 steps)",
            vec![
                on("c12_per_vertex_density"),
                list("c12_per_vertex_density.share", &[50]),
                on("c22_repeat_tile"),
                f("c22_repeat_tile.w", 3000.0),
                f("c22_repeat_tile.h", 63.0),
                f("c22_repeat_tile.gap", 1.0),
                len(n / 2),
            ],
        ),
        (
            "c17 both axes + c18 8 copies + c11 density 2 (a union of 32 instances; its exact boundary; n / 4 steps)",
            vec![
                on("c17_mirror_symmetry"),
                ch("c17_mirror_symmetry.axis", 2),
                on("c18_radial_symmetry"),
                i("c18_radial_symmetry.count", 8),
                on("c11_per_edge_density"),
                list("c11_per_edge_density.every", &[2]),
                len(n / 4),
            ],
        ),
        (
            "c02 Bezier + c11 density 2 (edges flattened on the grid — up to 4,096 points each — nearest edges by the tree; n steps)",
            vec![
                on("c02_bezier_handles_per_vertex"),
                on("c11_per_edge_density"),
                list("c11_per_edge_density.every", &[2]),
                len(n),
            ],
        ),
        (
            "c23 mesh warp + c11 density 2 (edges cut into 64 pieces, warped; n steps)",
            vec![
                on("c23_mesh_warp"),
                on("c11_per_edge_density"),
                list("c11_per_edge_density.every", &[2]),
                len(n),
            ],
        ),
        (
            "c19 + c20 + c21 booleans + c06 hole + c11 density 2 (n steps)",
            vec![
                on("c19_shape_union"),
                on("c20_shape_subtract"),
                on("c21_shape_intersection"),
                on("c06_interior_holes"),
                on("c11_per_edge_density"),
                list("c11_per_edge_density.every", &[2]),
                len(n),
            ],
        ),
        (
            "c10 crosshatch every 2 (interior windows; n / 50 steps)",
            vec![
                on("c10_configurable_fill_strategy"),
                ch("c10_configurable_fill_strategy.pattern", 4),
                i("c10_configurable_fill_strategy.spacing", 2),
                len(n / 50),
            ],
        ),
        (
            "c22 tiles 4 × 4, gap 1 + c08 filled + c09 thickness 1 (about 2,000 copies, near the cap; 200 steps)",
            vec![
                on("c22_repeat_tile"),
                f("c22_repeat_tile.w", 4.0),
                f("c22_repeat_tile.h", 4.0),
                f("c22_repeat_tile.gap", 1.0),
                on("c08_filled_generation"),
                len(200),
            ],
        ),
    ];
    println!(
        "## v0.9 Custom Shape generator (V09-S57): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S58: Parametric Equation through the platform per function family at
/// about `n` generated notes (keys 0 – 127, one-tick steps: the base
/// sawtooth is as wide as the box and rises over ~102 of its 128 rows);
/// best of three. Formula evaluations per sample are the new cost.
fn parametric_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.parametric").expect("gen.parametric");
    let doc = PlumDocument::new_project("parametric".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let tx = |k: &'static str, v: &str| (k, ParamValue::Text(v.to_string()));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    // Strokes merge each row's run into one note (few notes); the density
    // functions draw a note every grid unit along the curve — c15 at 0 %
    // keeps them all — so ~1M notes need a curve ~1M grid units long.
    let all = || {
        vec![
            on("c15_derivative_based_density"),
            f("c15_derivative_based_density.strength", 0.0),
        ]
    };
    let with = |mut v: Vec<(&'static str, ParamValue)>, more: Vec<(&'static str, ParamValue)>| {
        v.extend(more);
        v
    };
    let cases: Vec<Case> = vec![
        (
            "base strokes (3 curved teeth, 256 samples; each row's run one note — a reference; n steps)",
            vec![len(n)],
        ),
        (
            "c15 derivative density 100 % (a note every grid unit kept by its speed share; n steps)",
            vec![on("c15_derivative_based_density"), len(n)],
        ),
        (
            "c15 0 % (every grid unit) + c17 velocity + c18 gate + c19 subchannel formulas (per sample, blended per note; n steps)",
            with(
                all(),
                vec![
                    on("c17_velocity_formula"),
                    on("c18_gate_formula"),
                    on("c19_subchannel_formula"),
                    len(n),
                ],
            ),
        ),
        (
            "c16 curvature density, least share 100 % + c06 adaptive, tolerance 0.25 (n steps)",
            vec![
                on("c16_curvature_based_density"),
                f("c16_curvature_based_density.floor", 100.0),
                on("c06_adaptive_sampling"),
                f("c06_adaptive_sampling.tolerance", 0.25),
                len(n),
            ],
        ),
        (
            "c15 0 % + c05 16,384 samples (the sample cap: 64x the base's formula evaluations; n steps)",
            with(
                all(),
                vec![
                    on("c05_sample_count"),
                    i("c05_sample_count.n", 16_384),
                    len(n),
                ],
            ),
        ),
        (
            "c15 0 % + c09 two more layers (three curves; n / 3 steps)",
            with(
                all(),
                vec![
                    on("c09_multiple_equation_layers"),
                    i("c09_multiple_equation_layers.count", 2),
                    len(n / 3),
                ],
            ),
        ),
        (
            "c15 0 % + c07 polar rose R = 0.6 + 0.35 cos(10 pi t), 4,096 samples (n / 4 steps)",
            with(
                all(),
                vec![
                    on("c07_polar_equation_mode"),
                    on("c05_sample_count"),
                    i("c05_sample_count.n", 4096),
                    len(n / 4),
                ],
            ),
        ),
        (
            "c15 0 % + c24 noise 6 % + c22 reflect, extent 2 (n / 2 steps)",
            with(
                all(),
                vec![
                    on("c24_deterministic_noise_functions"),
                    on("c22_wrap_reflect_boundaries"),
                    ch("c22_wrap_reflect_boundaries.mode", 1),
                    len(n / 2),
                ],
            ),
        ),
        (
            "c15 0 % + c23 morph A -> B, 16 curves (n / 16 steps)",
            with(
                all(),
                vec![
                    on("c23_formula_a_b_morph"),
                    f("c23_formula_a_b_morph.amount", 100.0),
                    i("c23_formula_a_b_morph.steps", 16),
                    len(n / 16),
                ],
            ),
        ),
        (
            "c15 0 % + c25 library Spiral, 4,096 samples + c20 breaks + c21 domain clipping (n / 8 steps)",
            with(
                all(),
                vec![
                    on("c25_saved_equation_presets_expression_validation"),
                    tx(
                        "c25_saved_equation_presets_expression_validation.name",
                        "Spiral",
                    ),
                    on("c05_sample_count"),
                    i("c05_sample_count.n", 4096),
                    on("c20_discontinuity_detection"),
                    on("c21_domain_clipping"),
                    len(n / 8),
                ],
            ),
        ),
    ];
    println!(
        "## v0.9 Parametric Equation generator (V09-S58): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S59: the L-system through the platform per function family at about
/// `n` generated notes (keys 0 – 127, one-tick steps: the base plant is
/// stretched over the box). Strokes merge each row's run, so the cases
/// sample a note every grid unit (c13 at 100 %) along plants long enough
/// for ~1M notes.
fn lsystem_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.lsystem").expect("gen.lsystem");
    let doc = PlumDocument::new_project("lsystem".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let all = || {
        vec![
            on("c13_density_per_depth"),
            f("c13_density_per_depth.factor", 100.0),
        ]
    };
    let with = |mut v: Vec<(&'static str, ParamValue)>, more: Vec<(&'static str, ParamValue)>| {
        v.extend(more);
        v
    };
    let cases: Vec<Case> = vec![
        (
            "base strokes (the plant, 1,488 segments; each row's run one note — a reference; n steps)",
            vec![len(n)],
        ),
        (
            "c13 100 % (a note every grid unit along the base plant; n / 24 steps)",
            with(all(), vec![len(n / 24)]),
        ),
        (
            "c03 7 rewrites + c13 100 % (79,680 segments; n / 90 steps)",
            with(
                all(),
                vec![
                    on("c03_iteration_depth"),
                    i("c03_iteration_depth.depth", 7),
                    len(n / 90),
                ],
            ),
        ),
        (
            "c08 stochastic + c09 weights + c19 noise + c18 tropism + c13 100 % (seeded; n / 24 steps)",
            with(
                all(),
                vec![
                    on("c08_stochastic_rules_with_seed"),
                    on("c09_rule_weights"),
                    on("c19_growth_noise"),
                    on("c18_tropism_direction_bias"),
                    len(n / 24),
                ],
            ),
        ),
        (
            "c21 both mirrors + c13 100 % (four copies; n / 48 steps)",
            with(
                all(),
                vec![
                    on("c21_mirror_symmetry"),
                    ch("c21_mirror_symmetry.axis", 3),
                    len(n / 48),
                ],
            ),
        ),
        (
            "c24 forest of 8 + c23 joins + c13 100 % (n / 60 steps)",
            with(
                all(),
                vec![
                    on("c24_multi_axiom_forest"),
                    i("c24_multi_axiom_forest.count", 8),
                    on("c23_branch_join"),
                    len(n / 60),
                ],
            ),
        ),
        (
            "c25 evolution, 6 generations + c13 100 % (n / 40 steps)",
            with(
                all(),
                vec![
                    on("c03_iteration_depth"),
                    on("c25_rule_set_morph_evolution"),
                    ch("c25_rule_set_morph_evolution.mode", 1),
                    len(n / 40),
                ],
            ),
        ),
        (
            "c22 wrap, extent 4 + c13 100 % (segments cut at every edge; n / 96 steps)",
            with(
                all(),
                vec![
                    on("c22_wrap_reflect"),
                    f("c22_wrap_reflect.extent", 4.0),
                    len(n / 96),
                ],
            ),
        ),
        (
            "c12 thickness 4 rows, 60 % per depth + c14 / c15 / c16 brushes (bands; n steps)",
            vec![
                on("c12_thickness_per_depth"),
                f("c12_thickness_per_depth.rows", 4.0),
                on("c14_velocity_per_depth"),
                on("c15_gate_per_depth"),
                on("c16_subchannel_per_depth"),
                len(n),
            ],
        ),
    ];
    println!(
        "## v0.9 Fractal / L-System generator (V09-S59): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S60: the spirograph through the platform per function family at
/// about `n` generated notes (keys 0 – 127, one-tick steps: the base
/// stretched over the box). Strokes merge each row's run, so the cases
/// sample a note every grid unit (c19 with a least share of 100 %).
fn spirograph_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.spirograph").expect("gen.spirograph");
    let doc = PlumDocument::new_project("spirograph".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let all = || {
        vec![
            on("c19_density_by_curvature"),
            f("c19_density_by_curvature.floor", 100.0),
        ]
    };
    let with = |mut v: Vec<(&'static str, ParamValue)>, more: Vec<(&'static str, ParamValue)>| {
        v.extend(more);
        v
    };
    let cases: Vec<Case> = vec![
        (
            "base strokes (five loops along time; each row's run one note — a reference; n steps)",
            vec![len(n)],
        ),
        (
            "c19 least share 100 % (a note every grid unit along the base; n / 2 steps)",
            with(all(), vec![len(n / 2)]),
        ),
        (
            "c19 curvature density, least share 10 % (n / 2 steps)",
            vec![on("c19_density_by_curvature"), len(n / 2)],
        ),
        (
            "c19 100 % + c20 velocity by radius + c21 gate by phase (n / 2 steps)",
            with(
                all(),
                vec![
                    on("c20_velocity_by_radius"),
                    on("c21_gate_by_harmonic_phase"),
                    len(n / 2),
                ],
            ),
        ),
        (
            // V09-S60 step D: a stroke whose brush changes (a piece per
            // grid unit, sent to the pen in parts of 2¹⁶ points).
            "c20 velocity by radius + c21 gate by phase on strokes (pieces of a grid unit; n / 2 steps)",
            vec![
                on("c20_velocity_by_radius"),
                on("c21_gate_by_harmonic_phase"),
                len(n / 2),
            ],
        ),
        (
            "c01 epitrochoid + c19 100 % (a closed rosette; n / 4 steps)",
            with(all(), vec![on("c01_epitrochoid_mode"), len(n / 4)]),
        ),
        (
            "c14 Fourier sum 1, -3, 5 + c19 100 % (n / 2 steps)",
            with(all(), vec![on("c14_fourier_harmonic_sum"), len(n / 2)]),
        ),
        (
            "c23 four orbits + c22 layers + c19 100 % (n / 6 steps)",
            with(
                all(),
                vec![
                    on("c23_multi_orbit_superposition"),
                    i("c23_multi_orbit_superposition.count", 4),
                    on("c22_subchannel_by_harmonic_layer"),
                    len(n / 6),
                ],
            ),
        ),
        (
            "c24 six sectors + c19 100 % (n / 12 steps)",
            with(
                all(),
                vec![
                    on("c24_symmetry_sector_replication"),
                    i("c24_symmetry_sector_replication.sectors", 6),
                    len(n / 12),
                ],
            ),
        ),
        (
            "c25 morph to Rose, 8 curves + c19 100 % (n / 16 steps)",
            with(
                all(),
                vec![
                    on("c25_harmonic_preset_morph"),
                    i("c25_harmonic_preset_morph.steps", 8),
                    len(n / 16),
                ],
            ),
        ),
        (
            "c15 harmonics + c17 radial + c18 angular modulation + c19 100 % (n / 2 steps)",
            with(
                all(),
                vec![
                    on("c15_harmonic_amplitudes"),
                    on("c17_radial_modulation"),
                    on("c18_angular_modulation"),
                    len(n / 2),
                ],
            ),
        ),
    ];
    println!(
        "## v0.9 Harmonic / Spirograph generator (V09-S60): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

/// V09-S61: the growth through the platform per function family (keys 0 –
/// 127, one-tick steps). Strokes along up to 1,024 live tips; the cases
/// grow long enough (iterations, branching) for ~1M notes where they can —
/// the growth's simulation (neighbour queries) is part of the run.
fn growth_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, ParamValue, TrackScope, tool};
    let def = tool("gen.growth").expect("gen.growth");
    let doc = PlumDocument::new_project("growth".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let ch = |k: &'static str, v: u32| (k, ParamValue::Choice(v));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let dense = || {
        vec![
            on("c02_iterations"),
            i("c02_iterations.n", 300),
            on("c04_branch_probability"),
            on("c03_growth_step"),
            f("c03_growth_step.step", 1.0),
        ]
    };
    let with = |mut v: Vec<(&'static str, ParamValue)>, more: Vec<(&'static str, ParamValue)>| {
        v.extend(more);
        v
    };
    let cases: Vec<Case> = vec![
        ("base (one seed, 60 iterations; n steps)", vec![len(n)]),
        (
            "c02 300 iterations + c04 20 % + c03 1 % steps (up to 1,024 live tips; n / 4 steps)",
            with(dense(), vec![len(n / 4)]),
        ),
        (
            "the same + c08 neighbours (a query of up to 256 points a step; n / 4 steps)",
            with(dense(), vec![on("c08_neighbor_radius"), len(n / 4)]),
        ),
        (
            "the same + c09 density + c10 collisions + c11 merges + c12 loops (tips end early — a reference; n / 4 steps)",
            with(
                dense(),
                vec![
                    on("c09_density_target"),
                    on("c10_collision_avoidance"),
                    on("c11_branch_merge"),
                    on("c12_loop_formation"),
                    len(n / 4),
                ],
            ),
        ),
        (
            "the same + c22 image field (n / 4 steps)",
            with(dense(), vec![on("c22_image_field"), len(n / 4)]),
        ),
        (
            "the same + c24 six radial copies (n / 16 steps)",
            with(
                dense(),
                vec![
                    on("c24_symmetry_mirrored_growth"),
                    ch("c24_symmetry_mirrored_growth.mode", 3),
                    len(n / 16),
                ],
            ),
        ),
        (
            "the same + c25 four stages along time (n / 8 steps)",
            with(
                dense(),
                vec![on("c25_evolution_timeline_saved_growth_stages"), len(n / 8)],
            ),
        ),
        (
            "base + c15 thickness + c16 velocity + c17 gate evolution (bands cut a quad per grid unit — the dense growth's strokes would make ~10⁷ notes; n / 4 steps)",
            vec![
                on("c15_thickness_evolution"),
                on("c16_velocity_evolution"),
                on("c17_gate_evolution"),
                len(n / 4),
            ],
        ),
    ];
    println!(
        "## v0.9 Procedural Growth / Topology generator (V09-S61): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

fn arcs_bench(n: usize) {
    use plumspirit_engine::model::PlumDocument;
    use plumspirit_engine::transform::run::collect_for;
    use plumspirit_engine::transform::{Ctx, Curve, ParamValue, TrackScope, tool};
    let def = tool("gen.arcs").expect("gen.arcs");
    let doc = PlumDocument::new_project("arcs".into(), Bpm::DEFAULT, 960);
    let mut c = Ctx::simple(960);
    c.active = Some(0);
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, &doc, &scope, 10_000_000).expect("source");
    let on = |k: &'static str| (k, ParamValue::Bool(true));
    let f = |k: &'static str, v: f64| (k, ParamValue::Float(v));
    let i = |k: &'static str, v: i64| (k, ParamValue::Int(v));
    let flat =
        |k: &'static str, v: f64| (k, ParamValue::Curve(Curve::new(vec![(0.0, v), (1.0, v)])));
    let len = |m: usize| i("gen.length", m as i64);
    type Case = (&'static str, Vec<(&'static str, ParamValue)>);
    let cases: Vec<Case> = vec![
        (
            "c11 density 1: a centred one-step note every grid unit along the base (n steps)",
            vec![
                on("c11_density_along_curve"),
                flat("c11_density_along_curve.curve", 1.0),
                len(n),
            ],
        ),
        (
            "c10 adaptive sampling (widest 2) + c12 velocity along c06 Catmull-Rom (n steps)",
            vec![
                on("c06_catmull_rom_smooth_path"),
                on("c10_adaptive_note_sampling"),
                f("c10_adaptive_note_sampling.widest", 2.0),
                on("c12_velocity_along_curve"),
                len(n),
            ],
        ),
        (
            "c08 thickness 16 + c12 velocity + c20 modulation (band quads per grid unit; n / 16 steps)",
            vec![
                on("c08_stroke_thickness"),
                i("c08_stroke_thickness.rows", 16),
                on("c12_velocity_along_curve"),
                on("c20_curvature_driven_modulation"),
                len(n / 16),
            ],
        ),
        (
            "c09 32 parallels x c15 dashes 2 / 2 (a stroke per dash; n / 256 steps)",
            vec![
                on("c09_parallel_curves"),
                i("c09_parallel_curves.count", 32),
                on("c15_dash_pattern"),
                f("c15_dash_pattern.dash", 2.0),
                f("c15_dash_pattern.gap", 2.0),
                len(n / 256),
            ],
        ),
        (
            "c24 branches 4 x depth 4 (341 copies) x c22 64 loops (n / 256 steps)",
            vec![
                on("c24_recursive_branch_curves"),
                i("c24_recursive_branch_curves.branches", 4),
                i("c24_recursive_branch_curves.depth", 4),
                on("c22_loop_cusp_mode"),
                i("c22_loop_cusp_mode.turns", 64),
                len(n / 256),
            ],
        ),
        (
            "c25 ribbon 32 wide along the base + c13 gate (quads; n / 16 steps)",
            vec![
                on("c25_ribbon_fill_from_curve"),
                f("c25_ribbon_fill_from_curve.width", 32.0),
                on("c13_gate_along_curve"),
                len(n / 16),
            ],
        ),
        (
            "c23 32 morphs x c18 8 chained arcs (256 curves; n / 64 steps)",
            vec![
                on("c23_curve_a_b_morph"),
                i("c23_curve_a_b_morph.count", 32),
                on("c18_tangent_aligned_duplication"),
                i("c18_tangent_aligned_duplication.count", 8),
                len(n / 64),
            ],
        ),
    ];
    println!(
        "## v0.9 Arcs / Curves generator (V09-S55): about {n} notes, one-tick steps, keys 0 – 127"
    );
    gen_cases(def, &c, &src, cases);
    println!("- {}", mem());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("scenario");
    let notes: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100_000);
    match mode {
        "chunks" => chunk_benchmark(notes),
        "v02" => scenario_v02(notes),
        "v03" => scenario_v03(notes),
        "v04" => scenario_v04(notes),
        "v05" => scenario_v05(notes),
        "v06" => scenario_v06(notes),
        "v07" => scenario_v07(notes),
        "v09" => scenario_v09(notes),
        // `expansion <notes> [tool] [off | nXX]`: the tool expansion cases
        // alone (off only, or off and one function).
        "expansion" => {
            let mut doc = generate(FixtureShape::Dense, notes, 960, 42);
            let track = (0..doc.track_count())
                .max_by_key(|&i| doc.track(i).map_or(0, |t| t.notes.len()))
                .expect("track");
            doc.track_mut(track).expect("track").notes.select_all();
            println!("## v0.9 tool expansion: {notes} notes, Dense");
            expansion_cases(
                &doc,
                track,
                args.get(3).map(String::as_str),
                args.get(4).map(String::as_str),
            );
        }
        // `generator <notes>` (V09-S51): the generator engine's pen at
        // about <notes> generated notes — the counting pass, the emitting
        // pass and a plain `Output::emit` loop of the same notes.
        "generator" => generator_engine(notes),
        // `lines <notes>` (V09-S52): Lines through the platform per function
        // family at about <notes> generated notes.
        "lines" => lines_bench(notes),
        // `rect <notes>` (V09-S53): Rectangle / Square per function family.
        "rect" => rect_bench(notes),
        // `circles <notes>` (V09-S54): Circles per function family.
        "circles" => circles_bench(notes),
        "arcs" => arcs_bench(notes),
        // `polygons <notes>` (V09-S56): Polygons per function family.
        "polygons" => polygons_bench(notes),
        // `custom <notes>` (V09-S57): Custom Shape per function family.
        "custom" => custom_bench(notes),
        // `parametric <notes>` (V09-S58): Parametric Equation per function family.
        "parametric" => parametric_bench(notes),
        // `lsystem <notes>` (V09-S59): the L-system per function family.
        "lsystem" => lsystem_bench(notes),
        // `spirograph <notes>` (V09-S60): the spirograph per function family.
        "spirograph" => spirograph_bench(notes),
        // `growth <notes>` (V09-S61): the growth per function family.
        "growth" => growth_bench(notes),
        // `tool3d <notes>` (V09-S43): the 3D tool on Dense notes.
        "tool3d" => tool3d_bench(notes),
        // `warp_transform <notes>` (V09-S44): Warp Transform on Dense notes.
        "warp_transform" => warp_transform_bench(notes),
        // `trails <notes>` (V09-S45): Trails on Dense notes.
        "trails" => trails_bench(notes),
        // `note_outline <notes>` (V09-S46): Note Outline on Dense notes.
        "note_outline" => note_outline_bench(notes),
        // `displacement_map <notes>` (V09-S47): Displacement Map on Dense notes.
        "displacement_map" => displacement_map_bench(notes),
        // `tiny_planet <notes>` (V09-S48): Tiny Planet on Dense notes.
        "tiny_planet" => tiny_planet_bench(notes),
        // `ripple <notes>` (V09-S49): Ripple on Dense notes.
        "ripple" => ripple_bench(notes),
        // `tumours <notes>` (V09-S50): Tumours on Dense notes.
        "tumours" => tumours_bench(notes),
        // `write <notes> <path>`: writes a Dense fixture file for import-only runs.
        "write" => {
            let path = args.get(3).expect("path");
            let doc = generate(FixtureShape::Dense, notes, 960, 42);
            midi::export_to_path(
                &doc,
                &midi::plan_export(&doc),
                std::path::Path::new(path),
                ExportControl::default(),
            )
            .expect("export");
            println!("wrote {path}");
        }
        // `import <path>`: import alone, so peak memory is not polluted by generation.
        "import" => {
            let path = args.get(2).expect("path");
            println!("- baseline: {}", mem());
            let ((doc, summary), t) = time(|| {
                midi::import_file(std::path::Path::new(path), ImportControl::default())
                    .expect("import")
            });
            println!(
                "- import only: {} ({}); document heap {:.1} MB → {}",
                ms(t),
                summary.describe(),
                doc.heap_bytes() as f64 / 1_048_576.0,
                mem()
            );
        }
        _ => {
            let shape = match args.get(3).map(String::as_str) {
                Some("extreme") => FixtureShape::ExtremeDensity,
                Some("sparse") => FixtureShape::LongSparse,
                _ => FixtureShape::Dense,
            };
            scenario(notes, shape);
        }
    }
}
