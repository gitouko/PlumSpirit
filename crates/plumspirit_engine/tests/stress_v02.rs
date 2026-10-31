//! v0.2 stress fixtures (contract §84/§85): native projects of every size and
//! shape, tempo- and meter-changing MIDI, dense simultaneous notes, velocity-heavy
//! editing, extreme valid BPM/PPQN and invalid input.
//!
//! The quick fixtures run with `cargo test`. The heavyweight ones are `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p plumspirit_engine --test stress_v02 -- --ignored --nocapture --test-threads=1
//! ```

mod common;

use common::*;
use plumspirit_engine::history::{History, HistoryLimits, Operation, Outcome};
use plumspirit_engine::midi::{
    export_to_bytes_with, import_bytes, plan_export, validate_exported_bytes,
};
use plumspirit_engine::model::project::{MAX_BPM_TEXT, MIN_BPM_TEXT};
use plumspirit_engine::model::timing::{MAX_NATIVE_PPQN, parse_ppqn};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, OnionMode, PlumDocument, TempoFidelity, TempoMap,
    TrackId,
};
use plumspirit_engine::native::{
    LupControl, load_project, project_from_bytes, project_to_bytes, save_project,
};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
use plumspirit_engine::view::grid::MeterMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plumspirit-stress02-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn note(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, ch, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

/// Records what the scheduler sends and checks voice balance.
#[derive(Default)]
struct Probe {
    frame: u64,
    ons: Vec<(u64, u8, u8, u8)>,
    offs: Vec<(u64, u8, u8)>,
    sounding: HashMap<(u8, u8), i64>,
}

impl SynthSink for Probe {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        self.ons.push((self.frame, channel, key, velocity));
        *self.sounding.entry((channel, key)).or_default() += 1;
    }
    fn note_off(&mut self, channel: u8, key: u8) {
        self.offs.push((self.frame, channel, key));
        *self.sounding.entry((channel, key)).or_default() -= 1;
    }
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
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

fn run_to_end(s: &mut Scheduler, p: &mut Probe, max_blocks: usize) -> usize {
    let mut blocks = 0;
    while !s.finished() && blocks < max_blocks {
        p.frame = s.frame();
        s.advance(64, p);
        blocks += 1;
    }
    blocks
}

// ---------------------------------------------------------------------------------
// Native projects
// ---------------------------------------------------------------------------------

#[test]
fn empty_project_roundtrips_through_disk() {
    let dir = temp_dir("empty");
    let doc = PlumDocument::new_project("Empty".into(), Bpm::DEFAULT, 960);
    let path = dir.join("empty.lup");
    let report = save_project(&doc, &path, LupControl::default()).unwrap();
    assert!(report.write.bytes > 0);
    let (back, _) = load_project(&path, LupControl::default()).unwrap();
    assert_eq!(back.state_fingerprint(), doc.state_fingerprint());
    assert_eq!(back.project(), doc.project());
    assert_eq!(back.note_count(), 0);
    assert_eq!(back.track_count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn many_track_onion_project_roundtrips_and_exports() {
    let tracks = 512;
    let mut doc = PlumDocument::new_project("Many tracks".into(), Bpm::parse("200").unwrap(), 480);
    for _ in 1..tracks {
        doc.add_track().unwrap();
    }
    for t in 0..tracks {
        let recs: Vec<NoteRecord> = (0..200u64)
            .map(|i| {
                note(
                    i * 120,
                    100,
                    (t % 100) as u8 + 10,
                    (i % 127) as u8 + 1,
                    (t % 16) as u8,
                )
            })
            .collect();
        doc.track_mut(t)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    // Delete one track: its id stays in the onion list (ignored when drawing).
    let deleted = doc.track(7).unwrap().id();
    doc.delete_track(7).unwrap();
    {
        let p = doc.project_mut();
        p.onion.mode = OnionMode::Specific;
        p.onion.specific = (0..300).map(TrackId).chain([deleted]).collect();
        p.onion.opacity = 55;
    }
    let bytes = project_to_bytes(&doc).unwrap();
    let (back, _) = project_from_bytes(&bytes).unwrap();
    assert_eq!(back.state_fingerprint(), doc.state_fingerprint());
    assert_eq!(back.project().onion, doc.project().onion);
    assert_eq!(back.track_count(), tracks - 1);
    // Every source the chooser offers resolves to an existing track.
    let ids: Vec<TrackId> = back.tracks().iter().map(|t| t.id()).collect();
    let sources = back.project().onion.sources(&ids, 0);
    // ids 0–299: minus the active track (id 0) and the deleted track (id 7).
    assert_eq!(sources.len(), 298);
    // MIDI export of 511 tracks: validated, and re-import keeps every note.
    let plan = plan_export(&back);
    let midi = export_to_bytes_with(&back, &plan).unwrap();
    validate_exported_bytes(&back, &plan, &midi).unwrap();
    let (re, _) = import_bytes(&midi).unwrap();
    assert_eq!(re.track_count(), tracks - 1);
    for t in 0..re.track_count() {
        assert_eq!(notes_of(&re, t), notes_of(&back, t), "track {t}");
    }
}

// ---------------------------------------------------------------------------------
// Tempo- and meter-changing MIDI
// ---------------------------------------------------------------------------------

#[test]
fn tempo_changing_midi_times_every_note_exactly() {
    let ppqn = 480u64;
    let mut rng = Rng(0xDEAD_BEEF);
    let mut conductor = TrackBytes::new();
    let mut tempos = Vec::new();
    for i in 0..2_000u64 {
        let us = 150_000 + rng.below(2_850_000) as u32; // 20–400 BPM
        tempos.push((i * 120, us));
        conductor.meta(i * 120, 0x51, &us.to_be_bytes()[1..]);
    }
    let conductor = conductor.eot(2_000 * 120);
    let mut notes = TrackBytes::new();
    for i in 0..1_000u64 {
        notes.on(i * 240, 0, 60 + (i % 24) as u8, 100);
        notes.off(i * 240 + 200, 0, 60 + (i % 24) as u8, 0);
    }
    let notes = notes.eot(1_000 * 240);
    let bytes = smf(1, ppqn as u16, &[conductor, notes]);
    let (doc, _) = import_bytes(&bytes).unwrap();
    // v0.3: the tempo changes belong to the Conductor, not to the track.
    let conductor_tempos = |d: &plumspirit_engine::model::PlumDocument| -> Vec<(u64, u32)> {
        d.conductor()
            .tempo_points()
            .iter()
            .map(|p| match p.value {
                plumspirit_engine::model::TempoValue::Micros(us) => (p.tick, us),
                other => panic!("imported tempo is not in µs: {other:?}"),
            })
            .collect()
    };
    assert_eq!(doc.tracks()[0].events.tempos().count(), 0);
    assert_eq!(conductor_tempos(&doc), tempos);

    // Independent reference: frame = Σ ticks × µs / PPQN × 48 kHz, exactly.
    let sr = 48_000u128;
    let map = TempoMap::for_document(&doc, 48_000);
    let exact_frame = |tick: u64| -> u128 {
        // numerator in µs·ticks; frame = floor(num × sr / (ppqn × 1e6))
        let mut num: u128 = 0;
        let mut at = 0u64;
        let mut us = 500_000u128;
        for &(t, u) in &tempos {
            if t >= tick {
                break;
            }
            num += u128::from(t - at) * us;
            at = t;
            us = u128::from(u);
        }
        num += u128::from(tick - at) * us;
        num * sr / (u128::from(ppqn) * 1_000_000)
    };
    for tick in (0..240_000u64).step_by(997) {
        assert_eq!(
            u128::from(map.frame_at_tick(tick)),
            exact_frame(tick),
            "tick {tick}"
        );
        assert!(map.tick_at_frame(map.frame_at_tick(tick)) <= tick);
    }
    // The scheduler starts each note in the 64-frame block containing its frame.
    let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
    let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
    let mut p = Probe::default();
    s.seek(0, &mut p);
    run_to_end(&mut s, &mut p, 10_000_000);
    assert_eq!(p.ons.len(), 1_000);
    for (i, &(frame, _, _, _)) in p.ons.iter().enumerate() {
        let f = map.frame_at_tick(i as u64 * 240);
        assert_eq!(frame, f / 64 * 64, "note {i}");
    }
    assert_eq!(p.stuck(), 0);
    // Export keeps every tempo change.
    let plan = plan_export(&doc);
    let out = export_to_bytes_with(&doc, &plan).unwrap();
    let (re, _) = import_bytes(&out).unwrap();
    assert_eq!(conductor_tempos(&re), tempos);
    // …in the first track, exactly where they were.
    assert_eq!(out, bytes);
}

#[test]
fn time_signature_changes_drive_measure_navigation() {
    let ppqn = 480u64;
    let mut t0 = TrackBytes::new();
    t0.meta(0, 0x58, &[4, 2, 24, 8]) // 4/4 for two bars
        .meta(3_840, 0x58, &[3, 2, 24, 8]) // 3/4 for two bars
        .meta(6_720, 0x58, &[7, 3, 24, 8]) // 7/8
        .meta(10_080, 0x58, &[5, 2, 24, 8]); // 5/4 (7/8 lasted two bars)
    let t0 = t0.eot(20_000);
    let mut t1 = TrackBytes::new();
    t1.on(0, 0, 60, 100).off(20_000, 0, 60, 0);
    let bytes = smf(1, ppqn as u16, &[t0, t1.eot(20_000)]);
    let (doc, _) = import_bytes(&bytes).unwrap();
    let meter = MeterMap::new(doc.division(), &doc.meter_events(1));
    let expected = [
        0u64, 1_920, 3_840, 5_280, 6_720, 8_400, 10_080, 12_480, 14_880,
    ];
    let mut at = 0;
    for w in expected.windows(2) {
        assert_eq!(meter.next_bar(at), Some(w[1]), "next after {at}");
        at = w[1];
    }
    for w in expected.windows(2).rev() {
        assert_eq!(meter.previous_bar(w[1]), w[0], "previous before {}", w[1]);
    }
    // Inside a bar, "previous" goes to that bar's start first.
    assert_eq!(meter.previous_bar(6_000), 5_280);
    assert_eq!(meter.bar_start_at_or_before(9_000), 8_400);
}

// ---------------------------------------------------------------------------------
// Playback limits
// ---------------------------------------------------------------------------------

#[test]
fn dense_simultaneous_notes_degrade_predictably() {
    // 100,000 notes starting at tick 0, every channel and pitch many times over.
    let mut doc = PlumDocument::new_project("Dense".into(), Bpm::DEFAULT, 480);
    let recs: Vec<NoteRecord> = (0..100_000u64)
        .map(|i| {
            note(
                0,
                480 + i % 480,
                (i % 128) as u8,
                100,
                ((i / 128) % 16) as u8,
            )
        })
        .collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let before = doc.state_fingerprint();
    let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
    let limits = SchedulerLimits::default();
    let mut s = Scheduler::new(idx, limits);
    let mut p = Probe::default();
    s.seek(0, &mut p);
    let started = Instant::now();
    run_to_end(&mut s, &mut p, 1_000_000);
    let elapsed = started.elapsed();
    let st = s.stats();
    // Bounded work per block, the rest skipped (and counted), nothing stuck.
    let mut per_block: HashMap<u64, usize> = HashMap::new();
    for &(f, ..) in &p.ons {
        *per_block.entry(f).or_default() += 1;
    }
    assert!(
        per_block
            .values()
            .all(|&n| n <= limits.max_note_ons_per_block)
    );
    assert_eq!(st.note_ons + st.note_ons_skipped, 100_000);
    assert!(st.note_ons_skipped > 0);
    assert!(st.peak_pending_offs <= limits.max_pending_offs);
    assert_eq!(st.note_offs, st.note_ons);
    assert_eq!(p.stuck(), 0, "every started note was released");
    assert_eq!(s.pending_offs(), 0);
    assert_eq!(
        doc.state_fingerprint(),
        before,
        "playback never changes the project"
    );
    println!(
        "dense 100k simultaneous: {} started, {} skipped, peak pending {}, scheduled in {elapsed:?}",
        st.note_ons, st.note_ons_skipped, st.peak_pending_offs
    );
}

// ---------------------------------------------------------------------------------
// Velocity-heavy editing
// ---------------------------------------------------------------------------------

fn velocity_journey(n: usize) {
    let mut rng = Rng(42);
    let mut doc = PlumDocument::new_project("Velocities".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..n as u64)
        .map(|i| {
            note(
                i * 30,
                60,
                (rng.below(128)) as u8,
                1 + rng.below(127) as u8,
                0,
            )
        })
        .collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, true)
        .unwrap();
    let original = doc.fingerprint();
    let mut h = History::new(HistoryLimits::default());
    let t = Instant::now();
    let out = h
        .apply(
            &mut doc,
            Operation::SetVelocity {
                track: 0.into(),
                velocity: 64,
            },
        )
        .unwrap();
    let apply = t.elapsed();
    assert!(matches!(out, Outcome::Notes(k) if k == n));
    assert!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .all(|v| v.attrs.velocity() == 64)
    );
    let bytes = h.bytes();
    let t = Instant::now();
    h.undo(&mut doc).unwrap();
    let undo = t.elapsed();
    assert_eq!(
        doc.fingerprint(),
        original,
        "undo restores every velocity exactly"
    );
    let t = Instant::now();
    h.redo(&mut doc).unwrap();
    let redo = t.elapsed();
    assert!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .all(|v| v.attrs.velocity() == 64)
    );
    h.undo(&mut doc).unwrap();
    assert_eq!(doc.fingerprint(), original);
    doc.track(0).unwrap().notes.validate().unwrap();
    let per_note = bytes as f64 / n as f64;
    println!(
        "velocity edit on {n} notes: apply {apply:?}, undo {undo:?}, redo {redo:?}, history {bytes} bytes ({per_note:.2} B/note)"
    );
    assert!(per_note < 2.5, "undo data per note: {per_note}");
}

#[test]
fn velocity_heavy_project_edits_and_undoes() {
    velocity_journey(200_000);
}

#[test]
#[ignore = "heavyweight: 1M-note velocity edit"]
fn velocity_heavy_project_one_million() {
    velocity_journey(1_000_000);
}

// ---------------------------------------------------------------------------------
// Extreme valid values and invalid input
// ---------------------------------------------------------------------------------

#[test]
fn extreme_bpm_and_ppqn_never_overflow() {
    for (bpm_text, ppqn) in [
        (MIN_BPM_TEXT, 1u32),
        (MIN_BPM_TEXT, MAX_NATIVE_PPQN),
        (MAX_BPM_TEXT, 1),
        (MAX_BPM_TEXT, MAX_NATIVE_PPQN),
        ("123456789.123456789", 999_983), // 18 digits, prime PPQN
    ] {
        let bpm = Bpm::parse(bpm_text).unwrap();
        let mut doc = PlumDocument::new_project("Extreme".into(), bpm, ppqn);
        // The largest gap a single MIDI delta time can store (larger ones are
        // bridged with filler events since v0.3).
        let far = 0x0FFF_FFFFu64;
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[
                    note(0, 1, 60, 100, 0),
                    note(far, u64::from(ppqn), 62, 90, 0),
                ],
                false,
            )
            .unwrap();
        // Exact in the project file.
        let (back, _) = project_from_bytes(&project_to_bytes(&doc).unwrap()).unwrap();
        assert_eq!(
            back.conductor().start_tempo(),
            plumspirit_engine::model::TempoValue::Bpm(bpm)
        );
        assert_eq!(back.state_fingerprint(), doc.state_fingerprint());
        // Timing saturates instead of wrapping.
        let map = TempoMap::for_document(&doc, 48_000);
        let f_far = map.frame_at_tick(far);
        assert!(f_far >= map.frame_at_tick(1));
        assert!(map.frame_at_tick(u64::MAX) >= f_far);
        let _ = map.tick_at_frame(u64::MAX);
        let _ = map.seconds_at_tick(u64::MAX);
        // Playback index and a seek to the far note: no panic, bounded work.
        let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
        let mut s = Scheduler::new(idx, SchedulerLimits::default());
        let mut p = Probe::default();
        s.seek(far, &mut p);
        p.frame = s.frame();
        s.advance(64, &mut p);
        // MIDI cannot store these tempos: the plan discloses it.
        let plan = plan_export(&doc);
        let fidelity = bpm.to_micros_per_quarter().fidelity;
        if bpm_text == MIN_BPM_TEXT || bpm_text == MAX_BPM_TEXT {
            assert_eq!(fidelity, TempoFidelity::Clamped, "{bpm_text}");
            assert!(plan.needs_approval(), "{bpm_text} needs approval");
        }
        if ppqn > 32_767 {
            assert!(plan.rescale.is_some(), "PPQN {ppqn} is rescaled for MIDI");
        }
        // And when approved, the export still validates.
        let midi = export_to_bytes_with(&doc, &plan).unwrap();
        validate_exported_bytes(&doc, &plan, &midi).unwrap();
    }
}

#[test]
fn invalid_bpm_and_ppqn_input_is_rejected_with_a_reason() {
    for bad in [
        "",
        " ",
        "0",
        "0.0",
        "-120",
        "abc",
        "12..5",
        "1.2.3",
        "NaN",
        "inf",
        "1e13",
        "1000000000001",
        "0.0000009",
        "1234567890.1234567891", // 20 significant digits
    ] {
        let e = Bpm::parse(bad);
        assert!(e.is_err(), "BPM {bad:?} must be rejected");
        assert!(!e.unwrap_err().to_string().is_empty());
    }
    for ok in [
        "1",
        "120",
        "174.5",
        "0.000001",
        "1000000000000",
        "1e3",
        " 90 ",
    ] {
        assert!(Bpm::parse(ok).is_ok(), "BPM {ok:?} is valid");
    }
    for bad in ["", "0", "-1", "abc", "1.5", "2147483648", "99999999999"] {
        let e = parse_ppqn(bad);
        assert!(e.is_err(), "PPQN {bad:?} must be rejected");
        assert!(!e.unwrap_err().is_empty());
    }
    for (ok, v) in [
        ("960", 960),
        ("2147483647", MAX_NATIVE_PPQN),
        ("1_000", 1_000),
        ("1", 1),
    ] {
        assert_eq!(parse_ppqn(ok), Ok(v), "PPQN {ok:?}");
    }
}
