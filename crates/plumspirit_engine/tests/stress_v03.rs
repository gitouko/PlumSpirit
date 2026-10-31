//! v0.3 stress fixtures (contract §213): dense controller events, dense tempo
//! automation, many markers and chapters, large PPQN with huge tick positions,
//! dense same-tick notes, long sustained notes, and 1M / 5M-note projects that use
//! every v0.3 feature at once.
//!
//! The quick fixtures run with `cargo test`. The heavyweight ones are `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress_v03 -- --ignored --nocapture --test-threads=1
//! ```

mod common;

use common::*;
use plumspirit_engine::editor::controllers::{self, ControllerKind};
use plumspirit_engine::editor::{self, EditControl, QuantizeMode, QuantizeSpec};
use plumspirit_engine::fixtures::{
    FixtureShape, add_dense_controllers, generate, set_dense_tempo, set_many_markers,
};
use plumspirit_engine::history::{History, HistoryLimits, Operation, Outcome};
use plumspirit_engine::midi::{
    export_to_bytes_with, import_bytes, plan_export, validate_exported_bytes,
};
use plumspirit_engine::model::conductor::Interp;
use plumspirit_engine::model::events::RawKind;
use plumspirit_engine::model::timing::MAX_NATIVE_PPQN;
use plumspirit_engine::model::{
    Bpm, NoteRecord, PlumDocument, RankSetBuilder, TempoMap, TempoValue,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
use plumspirit_engine::view::grid::MeterMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn bpm(s: &str) -> TempoValue {
    TempoValue::Bpm(Bpm::parse(s).unwrap())
}

fn secs(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

/// Export → validate → re-import.
fn reimport(d: &PlumDocument) -> PlumDocument {
    let plan = plan_export(d);
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let bytes = export_to_bytes_with(d, &plan).unwrap();
    validate_exported_bytes(d, &plan, &bytes).unwrap();
    import_bytes(&bytes).unwrap().0
}

fn roundtrip_lup(d: &PlumDocument) -> PlumDocument {
    let (back, report) = project_from_bytes(&project_to_bytes(d).unwrap()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    back
}

/// Channel events of a track as (tick, status, data1, data2).
fn channel_events(d: &PlumDocument, track: usize) -> Vec<(u64, u8, u8, u8)> {
    d.track(track)
        .unwrap()
        .events
        .events()
        .iter()
        .filter_map(|e| match e.kind {
            RawKind::Channel { status, data } => Some((e.tick, status, data[0], data[1])),
            _ => None,
        })
        .collect()
}

/// Two events that would replace each other at one tick (same status byte and,
/// for control changes, the same controller).
fn duplicates(events: &[(u64, u8, u8, u8)]) -> usize {
    let mut seen: HashMap<(u64, u8, Option<u8>), usize> = HashMap::new();
    for &(tick, status, a, _) in events {
        let cc = (status & 0xF0 == 0xB0).then_some(a);
        *seen.entry((tick, status, cc)).or_default() += 1;
    }
    seen.values().filter(|&&n| n > 1).count()
}

/// Counts what the scheduler sends and checks that every note ends.
#[derive(Default)]
struct Probe {
    ons: u64,
    offs: u64,
    messages: u64,
    sounding: HashMap<(u8, u8), i64>,
}

impl SynthSink for Probe {
    fn note_on(&mut self, channel: u8, key: u8, _: u8) {
        self.ons += 1;
        *self.sounding.entry((channel, key)).or_default() += 1;
    }
    fn note_off(&mut self, channel: u8, key: u8) {
        self.offs += 1;
        *self.sounding.entry((channel, key)).or_default() -= 1;
    }
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {
        self.messages += 1;
    }
    fn silence(&mut self) {
        self.sounding.clear();
    }
    fn reset(&mut self) {
        self.sounding.clear();
    }
}

impl Probe {
    fn stuck(&self) -> usize {
        self.sounding.values().filter(|&&v| v != 0).count()
    }
}

fn play_to_end(d: &PlumDocument, block: u32) -> (Probe, Duration, f64) {
    let idx = Arc::new(PlaybackIndex::build(d, 48_000));
    let audio_s = idx.tempo.frame_at_tick(idx.end_tick) as f64 / 48_000.0;
    let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
    let mut p = Probe::default();
    s.seek(0, &mut p);
    let t = Instant::now();
    while !s.finished() {
        s.advance(block, &mut p);
    }
    (p, t.elapsed(), audio_s)
}

// ---------------------------------------------------------------------------------
// Dense controller events
// ---------------------------------------------------------------------------------

fn dense_controllers(n_events: usize) {
    let mut d = generate(FixtureShape::Dense, 20_000, 960, 5);
    let end = d.content_end_tick();
    add_dense_controllers(&mut d, 1, n_events, (end / n_events as u64).max(1), 9);
    let before = channel_events(&d, 1);
    assert_eq!(duplicates(&before), 0);

    // The lane reads the events in place; one edit changes one event.
    let t = Instant::now();
    let pts = controllers::collect_points(&d.track(1).unwrap().events, ControllerKind::PAN, 0);
    let collect = t.elapsed();
    assert_eq!(pts.len(), n_events / 2);
    let mut h = History::default();
    let target = pts[pts.len() / 3];
    let edit = controllers::prepare_set(
        d.track(1).unwrap(),
        ControllerKind::PAN,
        0,
        Some(target.index),
        target.tick,
        127,
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit,
            label: "Set Pan",
        },
    )
    .unwrap();
    let after = channel_events(&d, 1);
    assert_eq!(after.len(), before.len(), "an edit adds no event");
    assert_eq!(duplicates(&after), 0);
    assert_eq!(
        after.iter().zip(&before).filter(|(a, b)| a != b).count(),
        1,
        "exactly one event changed"
    );
    // Adding at an occupied tick replaces the value there instead of stacking.
    let edit = controllers::prepare_set(
        d.track(1).unwrap(),
        ControllerKind::PAN,
        0,
        None,
        target.tick,
        3,
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit,
            label: "Set Pan",
        },
    )
    .unwrap();
    assert_eq!(channel_events(&d, 1).len(), before.len());

    // MIDI and .lup keep every event exactly once.
    let t = Instant::now();
    let back = reimport(&d);
    let midi = t.elapsed();
    assert_eq!(channel_events(&back, 1), channel_events(&d, 1));
    let t = Instant::now();
    let lup = roundtrip_lup(&d);
    let native = t.elapsed();
    assert_eq!(channel_events(&lup, 1), channel_events(&d, 1));

    // Playback sends each one (plus the program change at tick 0).
    let (p, t_play, audio_s) = play_to_end(&d, 64);
    assert_eq!(p.messages as usize, before.len());
    assert_eq!(p.stuck(), 0);
    println!(
        "dense controllers: {n_events} events; lane collect {}, MIDI round trip {}, .lup round trip {}, playback {} for {audio_s:.0} s",
        secs(collect),
        secs(midi),
        secs(native),
        secs(t_play)
    );
}

#[test]
fn dense_controller_events_stay_single_through_edits_files_and_playback() {
    dense_controllers(20_000);
}

#[test]
#[ignore = "heavyweight: 1M controller events"]
fn one_million_controller_events() {
    dense_controllers(1_000_000);
}

// ---------------------------------------------------------------------------------
// Dense tempo automation
// ---------------------------------------------------------------------------------

fn dense_tempo(points: usize) {
    let mut d = generate(FixtureShape::Dense, 20_000, 960, 3);
    let end = d.content_end_tick();
    set_dense_tempo(&mut d, points, (end / points as u64).max(1), 11);
    let t = Instant::now();
    let map = TempoMap::for_document(&d, 48_000);
    let build = t.elapsed();
    // Monotonic and invertible everywhere (sampled).
    let mut last = 0u64;
    for k in 0..20_000u64 {
        let tick = end / 20_000 * k;
        let f = map.frame_at_tick(tick);
        assert!(f >= last, "time runs backwards at tick {tick}");
        last = f;
        let back = map.tick_at_frame(f);
        assert!(back <= tick && map.frame_at_tick(back) == f);
    }
    // Every note plays, in order, and ends.
    let (p, t_play, audio_s) = play_to_end(&d, 64);
    assert_eq!(p.ons, 20_000);
    assert_eq!(p.stuck(), 0);
    // Export: ramps become bounded steps (or a coarser, disclosed accuracy).
    let t = Instant::now();
    let plan = plan_export(&d);
    let t_plan = t.elapsed();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert!(plan.tempo.max_ramp_error_ms <= plan.tempo.tolerance_ms + 0.01);
    assert!(plan.tempo.ramp_events <= 500_000);
    let back = reimport(&d);
    assert_eq!(back.note_count(), d.note_count());
    // The re-imported steps time every note within the tolerance.
    let (a, b) = (
        TempoMap::for_document(&d, 48_000),
        TempoMap::for_document(&back, 48_000),
    );
    let worst = d
        .track(1)
        .unwrap()
        .notes
        .iter()
        .step_by(97)
        .map(|n| (a.seconds_at_tick(n.start_tick) - b.seconds_at_tick(n.start_tick)).abs())
        .fold(0.0f64, f64::max);
    assert!(
        worst * 1000.0 <= plan.tempo.tolerance_ms + 0.05,
        "notes moved by {:.3} ms",
        worst * 1000.0
    );
    // One more point in the dense map is a small edit that leaves notes alone.
    let notes_rev = d.track(1).unwrap().notes.revision();
    let mut h = History::default();
    let edit = d
        .conductor()
        .prepare_set_tempo(None, end / 2 + 1, bpm("333.333"), Interp::Linear)
        .unwrap();
    let t = Instant::now();
    h.apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Add Tempo Point",
        },
    )
    .unwrap();
    let t_edit = t.elapsed();
    assert_eq!(d.track(1).unwrap().notes.revision(), notes_rev);
    roundtrip_lup(&d);
    println!(
        "dense tempo: {points} points; map build {}, playback {} for {audio_s:.0} s, export plan {} ({} ramp steps, max error {:.3} ms at {:.3} ms), one edit {}",
        secs(build),
        secs(t_play),
        secs(t_plan),
        plan.tempo.ramp_events,
        plan.tempo.max_ramp_error_ms,
        plan.tempo.tolerance_ms,
        secs(t_edit)
    );
}

#[test]
fn dense_tempo_automation_is_monotonic_plays_and_exports_within_budget() {
    dense_tempo(2_000);
}

#[test]
#[ignore = "heavyweight: 100,000 tempo points"]
fn one_hundred_thousand_tempo_points() {
    dense_tempo(100_000);
}

// ---------------------------------------------------------------------------------
// Many markers and chapters
// ---------------------------------------------------------------------------------

fn import_markers(count: usize) -> Duration {
    let mut d = generate(FixtureShape::Dense, 1_000, 960, 1);
    set_many_markers(&mut d, count, 240);
    let plan = plan_export(&d);
    let bytes = export_to_bytes_with(&d, &plan).unwrap();
    let t = Instant::now();
    let back = import_bytes(&bytes).unwrap().0;
    let took = t.elapsed();
    assert_eq!(back.conductor().markers().len(), count);
    let chapters = back
        .conductor()
        .markers()
        .iter()
        .filter(|m| m.end.is_some())
        .count();
    assert_eq!(chapters, count / 2);
    took
}

#[test]
fn many_markers_and_chapters_import_in_linear_time_and_round_trip() {
    // Importing chapters pairs each record with its start and end markers; that
    // must not scan all markers per chapter (it once took 38 s for 50,000).
    let small = import_markers(4_000);
    let large = import_markers(16_000);
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-6);
    println!(
        "markers: 4,000 in {}, 16,000 in {} (×{ratio:.1})",
        secs(small),
        secs(large)
    );
    assert!(
        ratio < 10.0,
        "import time grows faster than linearly: ×{ratio:.1}"
    );

    let mut d = generate(FixtureShape::Dense, 1_000, 960, 1);
    set_many_markers(&mut d, 10_000, 240);
    let lup = roundtrip_lup(&d);
    assert!(lup.conductor().same_content(d.conductor()));
    let back = reimport(&d);
    let a: Vec<_> = d
        .conductor()
        .markers()
        .iter()
        .map(|m| (m.start, m.end, m.title.clone()))
        .collect();
    let b: Vec<_> = back
        .conductor()
        .markers()
        .iter()
        .map(|m| (m.start, m.end, m.title.clone()))
        .collect();
    assert_eq!(a, b);
    // Editing one marker among many is a small edit.
    let mut h = History::default();
    let target = d.conductor().markers()[5_001].clone();
    let edit = d
        .conductor()
        .prepare_set_marker(
            Some(target.id),
            target.start + 7,
            target.end.map(|e| e + 7),
            "Moved",
        )
        .unwrap();
    assert_eq!(edit.markers.removed().len(), 1);
    assert_eq!(edit.markers.inserted().len(), 1);
    h.apply(
        &mut d,
        Operation::Conductor {
            edit,
            label: "Move Marker",
        },
    )
    .unwrap();
    d.conductor().validate().unwrap();
    // Range queries only visit what overlaps the window.
    let window: Vec<_> = d
        .conductor()
        .markers_in_range(240 * 5_000, 240 * 5_010)
        .collect();
    assert!(window.len() >= 10 && window.len() <= 14, "{}", window.len());
}

#[test]
#[ignore = "heavyweight: 200,000 markers"]
fn two_hundred_thousand_markers() {
    let took = import_markers(200_000);
    println!(
        "markers: 200,000 (100,000 chapters) imported in {}",
        secs(took)
    );
}

// ---------------------------------------------------------------------------------
// Large PPQN and huge tick positions
// ---------------------------------------------------------------------------------

#[test]
fn large_ppqn_and_conductor_entries_at_huge_ticks() {
    let far = 1u64 << 60;
    let ppqn = MAX_NATIVE_PPQN;
    let mut d = PlumDocument::new_project("far".into(), Bpm::DEFAULT, ppqn);
    let recs = [
        NoteRecord {
            start_tick: 0,
            duration_ticks: u64::from(ppqn),
            attrs: attrs(60, 100, 0),
            sub: Default::default(),
            group: 0,
        },
        NoteRecord {
            start_tick: far,
            duration_ticks: u64::from(ppqn) * 4,
            attrs: attrs(64, 1, 0),
            sub: Default::default(),
            group: 0,
        },
    ];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::default();
    // A ramp from the slowest tempo into the far point.
    let edit = d
        .conductor()
        .prepare_set_tempo(None, far - 1_000, bpm("0.01"), Interp::Linear)
        .unwrap();
    h.apply(&mut d, Operation::Conductor { edit, label: "t" })
        .unwrap();
    let edit = d
        .conductor()
        .prepare_set_tempo(None, far, bpm("99999"), Interp::Step)
        .unwrap();
    h.apply(&mut d, Operation::Conductor { edit, label: "t" })
        .unwrap();
    let edit = d
        .conductor()
        .prepare_set_signature(None, far, 7, 3)
        .unwrap();
    h.apply(&mut d, Operation::Conductor { edit, label: "s" })
        .unwrap();
    let edit = d
        .conductor()
        .prepare_set_marker(None, far, Some(far + u64::from(ppqn)), "Far")
        .unwrap();
    h.apply(&mut d, Operation::Conductor { edit, label: "m" })
        .unwrap();
    d.conductor().validate().unwrap();

    // Timing math never overflows and stays monotonic across the huge gap.
    let map = TempoMap::for_document(&d, 48_000);
    let a = map.frame_at_tick(far - 1_000);
    let b = map.frame_at_tick(far);
    let c = map.frame_at_tick(far + 1_000_000);
    assert!(a <= b && b <= c);
    // Bars and navigation at the far signature.
    let meter = MeterMap::new(d.division(), &d.conductor().meter_events());
    assert_eq!(meter.bar_start_at_or_before(far + 1), far);
    let next = meter.next_bar(far).unwrap();
    assert_eq!(next - far, u64::from(ppqn) * 7 / 2);
    assert_eq!(meter.previous_bar(next), far);

    // .lup keeps everything exactly.
    let back = roundtrip_lup(&d);
    assert!(back.conductor().same_content(d.conductor()));
    // MIDI cannot store this PPQN: export rescales (with approval) and keeps the
    // order of events; far ticks need gap fillers.
    let plan = plan_export(&d);
    assert!(plan.rescale.is_some());
    assert!(plan.needs_approval());
}

// ---------------------------------------------------------------------------------
// Dense same-tick notes and long sustained notes
// ---------------------------------------------------------------------------------

#[test]
fn dense_same_tick_notes_quantize_and_erase_in_one_step() {
    let n = 50_000usize;
    let mut rng = Rng(77);
    let recs: Vec<NoteRecord> = (0..n)
        .map(|i| NoteRecord {
            start_tick: 960,
            duration_ticks: 1 + rng.below(2_000),
            attrs: attrs((i % 128) as u8, 1 + (i % 127) as u8, (i % 16) as u8),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    let mut sorted = recs.clone();
    sorted.sort_by_key(|r| r.start_tick);
    let mut d = PlumDocument::new_project("same".into(), Bpm::DEFAULT, 960);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&sorted, false)
        .unwrap();
    let before = d.state_fingerprint();
    let mut h = History::default();
    d.track_mut(0).unwrap().notes.select_all();
    // Ticks: already on the grid → nothing to do.
    let ticks = QuantizeSpec {
        mode: QuantizeMode::Ticks,
        grid: 240,
        origin: 0,
    };
    assert_eq!(editor::preview_quantize(&d, 0, &ticks).changed, 0);
    // Gates: every duration snaps to a multiple of 240 (at least one step).
    let gates = QuantizeSpec {
        mode: QuantizeMode::Gates,
        ..ticks
    };
    let edit = editor::prepare_quantize(&d, 0, &gates, EditControl::default()).unwrap();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit,
            label: "Quantize",
        },
    )
    .unwrap();
    assert!(
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .all(|v| v.duration_ticks % 240 == 0 && v.duration_ticks >= 240 && v.start_tick == 960)
    );
    assert_eq!(d.track(0).unwrap().notes.len(), n);
    // Erase every note in one gesture, then undo both steps.
    let mut b = RankSetBuilder::default();
    b.push_run(0, n);
    let out = h
        .apply(
            &mut d,
            Operation::EraseNotes {
                track: 0.into(),
                ranks: b.finish(),
            },
        )
        .unwrap();
    assert_eq!(out, Outcome::Notes(n));
    assert_eq!(d.track(0).unwrap().notes.len(), 0);
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    // Playback degrades predictably (voice limit), with no stuck notes.
    let (p, _, _) = play_to_end(&d, 64);
    assert_eq!(p.stuck(), 0);
}

#[test]
fn long_sustained_notes_survive_gates_ramps_and_playback() {
    let ppqn = 960u64;
    let long = ppqn * 4 * 10_000; // 10,000 bars
    let mut d = PlumDocument::new_project("long".into(), Bpm::DEFAULT, 960);
    let recs = [
        NoteRecord {
            start_tick: 0,
            duration_ticks: long,
            attrs: attrs(48, 90, 0),
            sub: Default::default(),
            group: 0,
        },
        NoteRecord {
            start_tick: ppqn,
            duration_ticks: long - ppqn,
            attrs: attrs(55, 90, 1),
            sub: Default::default(),
            group: 0,
        },
    ];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    // Steep ramps under the sustained notes.
    let mut h = History::default();
    for (i, v) in ["600", "30", "900"].iter().enumerate() {
        let edit = d
            .conductor()
            .prepare_set_tempo(None, long / 4 * (i as u64 + 1), bpm(v), Interp::Linear)
            .unwrap();
        h.apply(&mut d, Operation::Conductor { edit, label: "t" })
            .unwrap();
    }
    // A huge valid gate works; one that would end past the last tick is refused
    // without changing anything.
    d.track_mut(0).unwrap().notes.select_all();
    let fp = d.state_fingerprint();
    assert!(
        h.apply(
            &mut d,
            Operation::SetDuration {
                track: 0.into(),
                duration: u64::MAX,
            },
        )
        .is_err()
    );
    assert_eq!(d.state_fingerprint(), fp);
    h.apply(
        &mut d,
        Operation::SetDuration {
            track: 0.into(),
            duration: long * 2,
        },
    )
    .unwrap();
    assert!(
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .all(|v| v.duration_ticks == long * 2)
    );
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), fp);
    // Exactly two notes sound for the whole song and both end.
    let (p, _, audio_s) = play_to_end(&d, 4096);
    assert_eq!((p.ons, p.offs), (2, 2));
    assert_eq!(p.stuck(), 0);
    assert!(audio_s > 1_000.0);
    // Files keep them.
    let back = reimport(&d);
    assert_eq!(back.note_count(), 2);
    roundtrip_lup(&d);
}

// ---------------------------------------------------------------------------------
// Big projects using every v0.3 feature
// ---------------------------------------------------------------------------------

fn everything(notes: usize) {
    let t0 = Instant::now();
    let mut d = generate(FixtureShape::Dense, notes, 960, 42);
    let end = d.content_end_tick();
    set_dense_tempo(&mut d, 5_000, end / 5_000, 2);
    set_many_markers(&mut d, 10_000, end / 10_000);
    add_dense_controllers(&mut d, 1, 100_000, end / 100_000, 4);
    let t_setup = t0.elapsed();
    // The undo budget of a 2 GB PC (1/16 of RAM = 128 MB): quantizing 5M notes
    // needs about 70 MB of undo data, more than the 64 MB minimum.
    let mut h = History::new(HistoryLimits::for_system_memory(Some(2 << 30)));
    let notes_rev = d.track(1).unwrap().notes.revision();

    // Conductor edits leave the notes alone.
    let edit = d
        .conductor()
        .prepare_set_signature(None, end / 2, 5, 2)
        .unwrap();
    let t = Instant::now();
    h.apply(&mut d, Operation::Conductor { edit, label: "s" })
        .unwrap();
    let t_sig = t.elapsed();
    assert_eq!(d.track(1).unwrap().notes.revision(), notes_rev);

    // Quantize everything (Both), undo, redo.
    d.track_mut(1).unwrap().notes.select_all();
    let spec = QuantizeSpec {
        mode: QuantizeMode::Both,
        grid: 120,
        origin: 0,
    };
    let t = Instant::now();
    let edit = editor::prepare_quantize(&d, 1, &spec, EditControl::default()).unwrap();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit,
            label: "Quantize",
        },
    )
    .unwrap();
    let t_q = t.elapsed();
    let quantized = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), quantized);

    // Erase 10% in one gesture.
    d.track_mut(1).unwrap().notes.clear_selection();
    let mut b = RankSetBuilder::default();
    for r in (0..notes).step_by(10) {
        b.push(r);
    }
    let t = Instant::now();
    h.apply(
        &mut d,
        Operation::EraseNotes {
            track: 1.into(),
            ranks: b.finish(),
        },
    )
    .unwrap();
    let t_erase = t.elapsed();
    assert_eq!(d.track(1).unwrap().notes.len(), notes - notes.div_ceil(10));

    // Save / reopen, export / re-import.
    let t = Instant::now();
    let lup = roundtrip_lup(&d);
    let t_lup = t.elapsed();
    drop(lup);
    let t = Instant::now();
    let back = reimport(&d);
    let t_mid = t.elapsed();
    assert_eq!(back.note_count(), d.note_count());
    assert_eq!(channel_events(&back, 1).len(), channel_events(&d, 1).len());
    assert_eq!(back.conductor().markers().len(), 10_000);
    drop(back);

    // Play the whole song.
    let (p, t_play, audio_s) = play_to_end(&d, 64);
    assert_eq!(p.stuck(), 0);
    println!(
        "{notes} notes + 5,000 tempo points + 10,000 markers + 100,000 controllers: setup {}, signature edit {}, quantize all {}, erase 10% {}, .lup round trip {}, MIDI round trip {}, scheduling {} for {audio_s:.0} s of audio",
        secs(t_setup),
        secs(t_sig),
        secs(t_q),
        secs(t_erase),
        secs(t_lup),
        secs(t_mid),
        secs(t_play)
    );
}

#[test]
#[ignore = "heavyweight: 1M notes with every v0.3 feature"]
fn one_million_notes_with_every_v03_feature() {
    everything(1_000_000);
}

#[test]
#[ignore = "heavyweight: 5M notes with every v0.3 feature"]
fn five_million_notes_with_every_v03_feature() {
    everything(5_000_000);
}
