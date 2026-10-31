//! v0.3 features at the engine level: the Conductor (tempo automation, time
//! signatures, markers and chapters), Pitch Bend / Pan events, quantize, erase,
//! numeric gate / velocity, persistence and export. Contract §236–§251.

mod common;

use common::*;
use plumspirit_engine::editor::controllers::{self, ControllerKind, collect_points};
use plumspirit_engine::editor::{
    self, EditControl, QuantizeMode, QuantizeSpec, parse_gate, parse_velocity, prepare_quantize,
    preview_quantize,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::semantic::semantic_from_bytes;
use plumspirit_engine::midi::{export_to_bytes, export_to_bytes_with, import_bytes, plan_export};
use plumspirit_engine::model::conductor::{
    ConductorError, ConductorScope, Interp, parse_denominator, parse_numerator,
};
use plumspirit_engine::model::{
    Bpm, Division, OnionMode, PlumDocument, RankSetBuilder, SoundFontRef, TempoMap, TempoValue,
    TrackId,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};
use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
use plumspirit_engine::view::grid::MeterMap;
use std::sync::Arc;

fn bpm(s: &str) -> TempoValue {
    TempoValue::Bpm(Bpm::parse(s).unwrap())
}

fn conductor_op(
    h: &mut History,
    d: &mut PlumDocument,
    edit: plumspirit_engine::model::ConductorEdit,
) {
    h.apply(
        d,
        Operation::Conductor {
            edit,
            label: "Conductor",
        },
    )
    .unwrap();
}

fn project(ppqn: u32) -> PlumDocument {
    PlumDocument::new_project("v0.3".into(), Bpm::DEFAULT, ppqn)
}

fn with_notes(ppqn: u32, recs: &[plumspirit_engine::model::NoteRecord]) -> PlumDocument {
    let mut d = project(ppqn);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(recs, false)
        .unwrap();
    d
}

fn roundtrip_lup(d: &PlumDocument) -> PlumDocument {
    let (back, report) = project_from_bytes(&project_to_bytes(d).unwrap()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    back
}

fn reimport(d: &PlumDocument) -> PlumDocument {
    let plan = plan_export(d);
    let bytes = export_to_bytes_with(d, &plan).unwrap();
    plumspirit_engine::midi::validate_exported_bytes(d, &plan, &bytes).unwrap();
    import_bytes(&bytes).unwrap().0
}

fn undo_redo_roundtrip(h: &mut History, d: &mut PlumDocument) {
    let after = d.state_fingerprint();
    h.undo(d).unwrap().expect("undo");
    let before = d.state_fingerprint();
    assert_ne!(before, after);
    h.redo(d).unwrap().expect("redo");
    assert_eq!(d.state_fingerprint(), after);
}

// ------------------------------------------------------------------ §236 Conductor

#[test]
fn every_project_has_exactly_one_conductor_that_undo_cannot_remove() {
    // New project, imported MIDI (with tempo in two tracks) and a Format 0 file.
    let new = project(960);
    let tr0 = {
        let mut t = TrackBytes::new();
        t.meta(0, 0x51, &[0x07, 0xA1, 0x20]);
        t.eot(0)
    };
    let tr1 = {
        let mut t = TrackBytes::new();
        t.on(0, 0, 60, 90)
            .off(480, 0, 60, 0)
            .meta(960, 0x51, &[0x03, 0xD0, 0x90]);
        t.eot(960)
    };
    let (imported, _) = import_bytes(&smf(1, 480, &[tr0, tr1])).unwrap();
    for d in [&new, &imported] {
        d.conductor().validate().unwrap();
        assert_eq!(d.conductor().scope(), ConductorScope::Timeline);
        assert_eq!(d.conductor().tempo_points()[0].tick, 0);
        assert_eq!(d.conductor().time_signatures()[0].tick, 0);
        // The Conductor is not a track: every track is an ordinary note track.
        assert!(d.tracks().iter().all(|t| t.events.tempos().count() == 0));
    }
    // One global tempo map for the whole song: the tempo from track 2 changes the
    // timing of track 1's notes too.
    let map = TempoMap::for_document(&imported, 48_000);
    assert_eq!(imported.conductor().tempo_points().len(), 2);
    assert!(
        (map.seconds_at_tick(1920) - (1.0 + 0.25 * 2.0)).abs() < 1e-9,
        "{}",
        map.seconds_at_tick(1920)
    );
    // Deleting every regular track but one, and undoing everything, leaves the
    // Conductor in place.
    let mut d = project(960);
    let mut h = History::default();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    let e = d
        .conductor()
        .prepare_set_tempo(None, 960, bpm("90"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    h.apply(&mut d, Operation::DeleteTrack { index: 0 })
        .unwrap();
    assert!(
        h.apply(&mut d, Operation::DeleteTrack { index: 0 })
            .is_err(),
        "the last track stays"
    );
    while h.undo(&mut d).unwrap().is_some() {}
    d.conductor().validate().unwrap();
    assert_eq!(d.conductor().tempo_points().len(), 1);
    assert_eq!(d.track_count(), 1);
    // Track ids stay valid: the regular track keeps id 0.
    assert_eq!(d.tracks()[0].id(), TrackId(0));
}

// --------------------------------------------------------------- §237 Tempo automation

#[test]
fn tempo_points_steps_ramps_and_validation() {
    let mut d = project(960);
    let mut h = History::default();
    // Initial BPM.
    let e = d
        .conductor()
        .prepare_set_start_tempo(bpm("150.25"))
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    assert_eq!(d.conductor().start_tempo(), bpm("150.25"));
    undo_redo_roundtrip(&mut h, &mut d);
    // A step and a linear ramp.
    let e = d
        .conductor()
        .prepare_set_tempo(None, 3840, bpm("100"), Interp::Linear)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_tempo(None, 7680, bpm("200"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    assert_eq!(d.conductor().bpm_at(5760), 150.0);
    undo_redo_roundtrip(&mut h, &mut d);
    // Same tick: editing the existing point, never two points at one tick.
    let e = d
        .conductor()
        .prepare_set_tempo(None, 7680, bpm("210"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    assert_eq!(d.conductor().tempo_points().len(), 3);
    assert_eq!(d.conductor().tempo_points()[2].value, bpm("210"));
    // Moving a point onto another replaces it.
    let id = d.conductor().tempo_points()[1].id;
    let e = d
        .conductor()
        .prepare_set_tempo(Some(id), 7680, bpm("99"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    assert_eq!(d.conductor().tempo_points().len(), 2);
    undo_redo_roundtrip(&mut h, &mut d);
    h.undo(&mut d).unwrap();
    // Zero and negative tempos cannot even be typed; huge ones are kept exactly.
    assert!(Bpm::parse("0").is_err());
    assert!(Bpm::parse("-5").is_err());
    assert!(Bpm::parse("NaN").is_err());
    assert!(Bpm::parse("inf").is_err());
    let e = d
        .conductor()
        .prepare_set_tempo(None, 100, bpm("1e12"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    assert_eq!(d.conductor().tempo_points()[1].value, bpm("1e12"));
    // The tick-0 tempo cannot be deleted.
    let first = d.conductor().tempo_points()[0].id;
    assert!(matches!(
        d.conductor().prepare_remove_tempo(&[first]),
        Err(ConductorError::StartRequired { .. })
    ));
    // Save / reopen keeps every point, value, shape and id.
    let back = roundtrip_lup(&d);
    assert_eq!(
        back.conductor().tempo_points(),
        d.conductor().tempo_points()
    );
}

/// Exact seconds of a linear ramp from `b0` to `b1` BPM over `len` ticks, at
/// `x` ticks into it: 60/PPQN · len/(b1−b0) · ln(1 + (b1−b0)/b0 · x/len).
fn reference_ramp_seconds(ppqn: f64, b0: f64, b1: f64, len: f64, x: f64) -> f64 {
    if b0 == b1 {
        return 60.0 / ppqn * x / b0;
    }
    60.0 / ppqn * len / (b1 - b0) * ((b0 + (b1 - b0) * x / len) / b0).ln()
}

#[test]
fn playback_timing_follows_steps_and_ramps_like_the_reference_integral() {
    let ppqn = 480u32;
    let mut d = with_notes(
        ppqn,
        &(0..64).map(|i| rec(i * 240, 120, 60)).collect::<Vec<_>>(),
    );
    let mut h = History::default();
    // 120 BPM for 2 beats, then a ramp 120 → 240 over 8 beats, then a step to 60.
    let e = d
        .conductor()
        .prepare_set_tempo(None, 960, bpm("120"), Interp::Linear)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_tempo(None, 960 + 3840, bpm("240"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_tempo(None, 960 + 3840 + 960, bpm("60"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let map = TempoMap::for_document(&d, 48_000);
    let q = f64::from(ppqn);
    let expected = |tick: u64| -> f64 {
        let t = tick as f64;
        if t <= 960.0 {
            return 60.0 / q * t / 120.0;
        }
        let base = 1.0;
        if t <= 4800.0 {
            return base + reference_ramp_seconds(q, 120.0, 240.0, 3840.0, t - 960.0);
        }
        let ramp = reference_ramp_seconds(q, 120.0, 240.0, 3840.0, 3840.0);
        if t <= 5760.0 {
            return base + ramp + 60.0 / q * (t - 4800.0) / 240.0;
        }
        base + ramp + 0.5 + 60.0 / q * (t - 5760.0) / 60.0
    };
    for tick in (0..8000u64).step_by(7) {
        let got = map.seconds_at_tick(tick);
        assert!(
            (got - expected(tick)).abs() < 1e-9,
            "tick {tick}: {got} vs {}",
            expected(tick)
        );
        // Frames and the inverse mapping agree.
        let f = map.frame_at_tick(tick);
        assert!(map.tick_at_frame(f) <= tick);
        assert!(map.tick_at_frame(f + 1) >= tick.saturating_sub(1));
    }
    // The scheduler starts every note in the block holding its exact frame.
    let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
    let mut s = Scheduler::new(idx, SchedulerLimits::default());
    let mut p = Recorder::default();
    s.seek(0, &mut p);
    while !s.finished() && p.frame < 48_000 * 60 {
        p.frame = s.frame();
        s.advance(64, &mut p);
    }
    assert_eq!(p.ons.len(), 64);
    for (i, &(frame, ..)) in p.ons.iter().enumerate() {
        let exact = (expected(i as u64 * 240) * 48_000.0).floor() as u64;
        assert!(
            frame <= exact && exact < frame + 64,
            "note {i}: block {frame}, exact {exact}"
        );
    }
    // Undo of the ramp restores the constant tempo: timing changes back.
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    let map = TempoMap::for_document(&d, 48_000);
    assert!((map.seconds_at_tick(4800) - 5.0).abs() < 1e-12);
}

#[derive(Default)]
struct Recorder {
    frame: u64,
    ons: Vec<(u64, u8, u8, u8)>,
    messages: Vec<(u64, u8, u8, u8)>,
}

impl SynthSink for Recorder {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        self.ons.push((self.frame, channel, key, velocity));
    }
    fn note_off(&mut self, _: u8, _: u8) {}
    fn channel_message(&mut self, status: u8, a: u8, b: u8) {
        self.messages.push((self.frame, status, a, b));
    }
    fn silence(&mut self) {}
    fn reset(&mut self) {
        self.messages.clear();
    }
}

// ------------------------------------------------------------------ §238 Tempo export

#[test]
fn ramp_export_error_is_measured_and_bounded_and_the_project_stays_exact() {
    for (b0, b1, beats) in [
        ("120", "120", 16u64), // constant
        ("90", "180", 64),     // ascending, long
        ("300", "40", 8),      // descending
        ("100", "101", 1),     // short
        ("4", "60000", 32),    // extreme valid range
    ] {
        let ppqn = 960u32;
        let len = beats * u64::from(ppqn);
        let mut d = project(ppqn);
        let mut h = History::default();
        let e = d.conductor().prepare_set_start_tempo(bpm(b0)).unwrap();
        conductor_op(&mut h, &mut d, e);
        let id = d.conductor().tempo_points()[0].id;
        let e = d
            .conductor()
            .prepare_set_interp(id, Interp::Linear)
            .unwrap();
        conductor_op(&mut h, &mut d, e);
        let e = d
            .conductor()
            .prepare_set_tempo(None, len, bpm(b1), Interp::Step)
            .unwrap();
        conductor_op(&mut h, &mut d, e);
        let before = d.state_fingerprint();
        let plan = plan_export(&d);
        assert!(plan.blockers.is_empty());
        let back = reimport(&d);
        // Endpoints: the ramp's end tempo is written exactly at its end.
        let pts = back.conductor().tempo_points();
        let end = pts.iter().find(|p| p.tick == len).expect("end point");
        let want = Bpm::parse(b1).unwrap().to_micros_per_quarter().micros;
        assert_eq!(end.value, TempoValue::Micros(want));
        // Measured timing error of the file against the exact ramp.
        let exact = TempoMap::for_document(&d, 48_000);
        let file = TempoMap::for_document(&back, 48_000);
        let mut worst = 0.0f64;
        for k in 0..=4000u64 {
            let t = len * k / 4000;
            worst = worst.max((exact.seconds_at_tick(t) - file.seconds_at_tick(t)).abs());
        }
        let declared = plan.tempo.max_ramp_error_ms / 1000.0;
        assert!(
            worst <= declared + 1e-5,
            "{b0}->{b1}: measured {worst}, declared {declared}"
        );
        assert!(worst <= 0.000_5 + 1e-5, "{b0}->{b1}: {worst}");
        assert!(pts.len() < 3000, "{b0}->{b1}: {} tempo events", pts.len());
        // The native project is untouched by the approximation.
        assert_eq!(d.state_fingerprint(), before);
        assert_eq!(d.conductor().tempo_points()[0].interp, Interp::Linear);
    }
}

// ---------------------------------------------------------- §239 Time signatures

#[test]
fn time_signatures_drive_bars_navigation_grid_and_survive_export() {
    let ppqn = 96u32;
    let mut d = with_notes(ppqn, &[rec(0, 96, 60), rec(5000, 96, 62)]);
    let mut h = History::default();
    // 4/4 at 0, 3/4 at bar 2, 7/8 in the middle of a bar, 5/16, and a change at
    // tick 0 (replacing 4/4).
    for (tick, n, den) in [(384u64, "3", "4"), (700, "7", "8"), (1500, "5", "16")] {
        let e = d
            .conductor()
            .prepare_set_signature(
                None,
                tick,
                parse_numerator(n).unwrap(),
                parse_denominator(den).unwrap(),
            )
            .unwrap();
        conductor_op(&mut h, &mut d, e);
        undo_redo_roundtrip(&mut h, &mut d);
    }
    let meter = MeterMap::new(d.division(), &d.meter_events(0));
    // 3/4 bar = 288 ticks; the change at 700 cuts bar 3 short (partial bar).
    assert_eq!(meter.next_bar(384), Some(672));
    assert_eq!(meter.next_bar(672), Some(700));
    assert_eq!(meter.bar_beat(700), Some((4, 1, 0)));
    // 7/8 bar = 336 ticks from 700; 5/16 = 120 ticks from 1500.
    assert_eq!(meter.next_bar(700), Some(1036));
    assert_eq!(meter.previous_bar(1500), 1372);
    assert_eq!(meter.next_bar(1500), Some(1620));
    // Continuous numbering: bars keep counting across changes.
    let (bar, ..) = meter.bar_beat(1620).unwrap();
    assert!(bar > 6);
    // Invalid values are refused before anything changes.
    assert!(parse_denominator("3").is_err());
    assert!(parse_denominator("256").is_err());
    assert!(parse_denominator("0").is_err());
    assert!(parse_numerator("0").is_err());
    assert!(d.conductor().prepare_set_signature(None, 10, 4, 8).is_err());
    // Tick-0 change replaces the initial 4/4 (never two at one tick).
    let e = d.conductor().prepare_set_signature(None, 0, 6, 3).unwrap();
    conductor_op(&mut h, &mut d, e);
    assert_eq!(d.conductor().time_signatures()[0].label(), "6/8");
    let sigs = |d: &PlumDocument| -> Vec<(u64, u8, u8)> { d.meter_events(0) };
    // Save / reopen and export / re-import.
    assert_eq!(sigs(&roundtrip_lup(&d)), sigs(&d));
    assert_eq!(sigs(&reimport(&d)), sigs(&d));
    // Unusual PPQN (7) and a signature near the song end.
    let mut d = with_notes(7, &[rec(0, 7, 60), rec(700, 7, 60)]);
    let e = d
        .conductor()
        .prepare_set_signature(None, 699, 5, 4)
        .unwrap();
    conductor_op(&mut History::default(), &mut d, e);
    assert_eq!(sigs(&reimport(&d)), sigs(&d));
}

// ------------------------------------------------------------------ §240 Markers

#[test]
fn markers_and_chapters_edit_persist_and_export() {
    let mut d = with_notes(480, &[rec(0, 480, 60)]);
    let mut h = History::default();
    // Point marker, Unicode chapter, overlapping chapters, a chapter far beyond
    // the last note.
    for (start, end, title) in [
        (0u64, None, "Intro"),
        (480, Some(4800u64), "  Verse ✨ ことば  "),
        (2400, Some(9600), "Overlap"),
        (1_000_000_000, Some(3_000_000_000), "Far chapter"),
    ] {
        let e = d
            .conductor()
            .prepare_set_marker(None, start, end, title)
            .unwrap();
        conductor_op(&mut h, &mut d, e);
        undo_redo_roundtrip(&mut h, &mut d);
    }
    let m = d.conductor().markers();
    assert_eq!(m.len(), 4);
    assert_eq!(m[1].title, "Verse ✨ ことば");
    assert_eq!(m[1].duration(), 4320);
    // Overlap is allowed; nothing was removed.
    assert_eq!(d.conductor().markers_in_range(3000, 3000).count(), 2);
    // The song lasts until the last chapter ends.
    assert!(d.duration_ticks() >= 3_000_000_000);
    // Zero-length chapter → point marker; end before start and empty titles refused.
    let e = d
        .conductor()
        .prepare_set_marker(None, 50, Some(50), "Zero")
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    assert!(
        d.conductor()
            .markers()
            .iter()
            .any(|m| m.title == "Zero" && m.end.is_none())
    );
    assert!(matches!(
        d.conductor().prepare_set_marker(None, 50, Some(10), "Bad"),
        Err(ConductorError::EndBeforeStart)
    ));
    assert!(
        d.conductor()
            .prepare_set_marker(None, 50, None, "   ")
            .is_err()
    );
    assert!(
        d.conductor()
            .prepare_set_marker(None, 50, None, "a\u{7}b")
            .is_err()
    );
    // Edit and delete with undo.
    let id = d.conductor().markers()[1].id;
    let e = d
        .conductor()
        .prepare_set_marker(Some(id), 600, Some(700), "Edited")
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    undo_redo_roundtrip(&mut h, &mut d);
    let e = d.conductor().prepare_remove_markers(&[id]).unwrap();
    conductor_op(&mut h, &mut d, e);
    undo_redo_roundtrip(&mut h, &mut d);
    h.undo(&mut d).unwrap();
    // Save / reopen keeps titles, positions and ids.
    let back = roundtrip_lup(&d);
    assert_eq!(back.conductor().markers(), d.conductor().markers());
    // MIDI: chapters come back exactly through the namespaced record…
    let bytes = export_to_bytes(&d).unwrap();
    let (again, _) = import_bytes(&bytes).unwrap();
    let key = |d: &PlumDocument| -> Vec<(u32, u64, Option<u64>, String)> {
        d.conductor()
            .markers()
            .iter()
            .map(|m| (m.id, m.start, m.end, m.title.clone()))
            .collect()
    };
    let mut a = key(&again);
    let mut b = key(&d);
    // Point marker ids are renumbered on import; chapter ids are kept.
    for v in [&mut a, &mut b] {
        for m in v.iter_mut().filter(|m| m.2.is_none()) {
            m.0 = 0;
        }
        v.sort();
    }
    assert_eq!(a, b);
    // …and other programs see a Marker at the start and one at the end.
    let s = semantic_from_bytes(&bytes).unwrap();
    let texts: Vec<String> = s.tracks[0]
        .other
        .iter()
        .filter(|(_, e)| e[0] == 0xFF && e[1] == 0x06)
        .map(|(_, e)| String::from_utf8_lossy(&e[6..]).into_owned())
        .collect();
    assert!(texts.contains(&"Overlap".to_owned()));
    assert!(texts.contains(&"Overlap (end)".to_owned()));
    // A plain MIDI file with look-alike markers gets point markers only.
    let plain = {
        let mut t = TrackBytes::new();
        t.meta(0, 0x06, b"Verse")
            .meta(960, 0x06, b"Verse (end)")
            .meta(960, 0x06, b"End: Verse");
        t.eot(960)
    };
    let (p, _) = import_bytes(&smf(1, 480, &[plain])).unwrap();
    assert_eq!(p.conductor().markers().len(), 3);
    assert!(p.conductor().markers().iter().all(|m| m.end.is_none()));
}

// ----------------------------------------------------------------- §241 Erase

#[test]
fn erasing_removes_exactly_the_hit_notes_in_one_undo_step() {
    // Overlapping notes, a selected note, and a second track that must stay.
    let recs: Vec<_> = (0..10u64)
        .map(|i| rec(i * 100, 250, 60 + (i % 3) as u8))
        .collect();
    let mut d = with_notes(480, &recs);
    d.add_track().unwrap();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(300, 300, 0, 127);
    let selected_before = d.track(0).unwrap().notes.selected_count();
    assert!(selected_before > 0);
    let before = d.state_fingerprint();
    let conductor_before = d.conductor().revision();
    // A gesture that hit notes 2, 5 and 5 again (repeated hits count once).
    let mut ranks = vec![5usize, 2, 5];
    ranks.sort_unstable();
    ranks.dedup();
    let mut b = RankSetBuilder::default();
    for r in ranks {
        b.push(r);
    }
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::EraseNotes {
            track: 0.into(),
            ranks: b.finish(),
        },
    )
    .unwrap();
    assert_eq!(d.track(0).unwrap().notes.len(), 8);
    assert_eq!(
        d.track(1).unwrap().notes.len(),
        10,
        "other tracks untouched"
    );
    assert_eq!(
        d.conductor().revision(),
        conductor_before,
        "Conductor untouched"
    );
    assert_eq!(h.undo_len(), 1);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    // Like every undone note edit, the restored notes become the selection.
    assert_eq!(d.track(0).unwrap().notes.selected_count(), 2);
    h.redo(&mut d).unwrap();
    assert_eq!(d.track(0).unwrap().notes.len(), 8);
    // A large batch: erase every other note of 200,000 in one step.
    let many: Vec<_> = (0..200_000u64)
        .map(|i| rec(i * 10, 5, (i % 128) as u8))
        .collect();
    let mut d = with_notes(480, &many);
    let mut b = RankSetBuilder::default();
    for r in (0..200_000).step_by(2) {
        b.push(r);
    }
    let t = std::time::Instant::now();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::EraseNotes {
            track: 0.into(),
            ranks: b.finish(),
        },
    )
    .unwrap();
    assert!(t.elapsed().as_secs_f64() < 5.0);
    assert_eq!(d.track(0).unwrap().notes.len(), 100_000);
    assert!(
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .all(|n| (n.start_tick / 10) % 2 == 1)
    );
    d.track(0).unwrap().notes.validate().unwrap();
}

// -------------------------------------------------------- §242/§243 Controllers

#[test]
fn pitch_bend_and_pan_edit_in_place_export_and_reach_the_synth() {
    // Imported file: pitch bend 0 / 8192 / 16383 on channel 3, pan on channel 10,
    // an unrelated CC 7 that must stay.
    let tr = {
        let mut t = TrackBytes::new();
        t.ev(0, &[0xE2, 0x00, 0x00])
            .ev(0, &[0xB9, 0x0A, 0x00])
            .ev(0, &[0xB9, 0x07, 0x64])
            .on(0, 2, 60, 100)
            .ev(480, &[0xE2, 0x00, 0x40])
            .ev(480, &[0xB9, 0x0A, 0x40])
            .ev(960, &[0xE2, 0x7F, 0x7F])
            .ev(960, &[0xB9, 0x0A, 0x7F])
            .off(1920, 2, 60, 0);
        t.eot(1920)
    };
    let (mut d, _) = import_bytes(&smf(1, 480, &[tr])).unwrap();
    let pb = ControllerKind::PitchBend;
    let pan = ControllerKind::PAN;
    let values = |d: &PlumDocument, k: ControllerKind, ch: u8| -> Vec<(u64, u16)> {
        collect_points(&d.track(0).unwrap().events, k, ch)
            .iter()
            .map(|p| (p.tick, p.value))
            .collect()
    };
    assert_eq!(values(&d, pb, 2), vec![(0, 0), (480, 8192), (960, 16383)]);
    assert_eq!(values(&d, pan, 9), vec![(0, 0), (480, 64), (960, 127)]);
    assert!(values(&d, pb, 0).is_empty(), "channels are kept apart");
    let events_before = d.track(0).unwrap().events.len();
    // Edit: change a value, add an event, move one, delete one — one undo step each.
    let mut h = History::default();
    let edit = |d: &PlumDocument, k, ch, target: Option<usize>, tick, v| {
        controllers::prepare_set(d.track(0).unwrap(), k, ch, target, tick, v).unwrap()
    };
    let idx = collect_points(&d.track(0).unwrap().events, pb, 2)[1].index;
    let e = edit(&d, pb, 2, Some(idx), 480, 12_000);
    h.apply(
        &mut d,
        Operation::Events {
            edit: e,
            label: "Pitch Bend",
        },
    )
    .unwrap();
    undo_redo_roundtrip(&mut h, &mut d);
    let e = edit(&d, pan, 9, None, 1440, 20);
    h.apply(
        &mut d,
        Operation::Events {
            edit: e,
            label: "Pan",
        },
    )
    .unwrap();
    let idx = collect_points(&d.track(0).unwrap().events, pan, 9)[1].index;
    let e = edit(&d, pan, 9, Some(idx), 600, 100);
    h.apply(
        &mut d,
        Operation::Events {
            edit: e,
            label: "Pan",
        },
    )
    .unwrap();
    undo_redo_roundtrip(&mut h, &mut d);
    let idx = collect_points(&d.track(0).unwrap().events, pb, 2)[2].index;
    let e = controllers::prepare_delete(d.track(0).unwrap(), &[idx]).unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit: e,
            label: "Delete",
        },
    )
    .unwrap();
    assert_eq!(values(&d, pb, 2), vec![(0, 0), (480, 12_000)]);
    assert_eq!(
        values(&d, pan, 9),
        vec![(0, 0), (600, 100), (960, 127), (1440, 20)]
    );
    // No duplicates: one event more (pan added), one less (bend deleted).
    assert_eq!(d.track(0).unwrap().events.len(), events_before);
    // Export / re-import and .lup keep timing, channel and values; CC 7 stays.
    for back in [reimport(&d), roundtrip_lup(&d)] {
        assert_eq!(values(&back, pb, 2), values(&d, pb, 2));
        assert_eq!(values(&back, pan, 9), values(&d, pan, 9));
        assert!(back.track(0).unwrap().events.events().iter().any(|e| e.kind
            == plumspirit_engine::model::RawKind::Channel {
                status: 0xB9,
                data: [7, 100]
            }));
    }
    // The synthesizer receives the edited events at their frames (120 BPM,
    // 480 PPQN: 50 frames per tick at 48 kHz), and a seek chases the state.
    let idx = Arc::new(PlaybackIndex::build(&d, 48_000));
    let mut s = Scheduler::new(Arc::clone(&idx), SchedulerLimits::default());
    let mut r = Recorder::default();
    s.seek(0, &mut r);
    while !s.finished() && r.frame < 48_000 * 5 {
        r.frame = s.frame();
        s.advance(64, &mut r);
    }
    let bend_12000 = (0xE2u8, (12_000 & 0x7F) as u8, (12_000 >> 7) as u8);
    let at = r
        .messages
        .iter()
        .find(|m| (m.1, m.2, m.3) == bend_12000)
        .expect("edited bend reaches the synth");
    assert!(at.0 <= 480 * 50 && 480 * 50 < at.0 + 64, "{at:?}");
    assert!(
        !r.messages
            .iter()
            .any(|m| m.1 == 0xE2 && m.2 == 0x7F && m.3 == 0x7F),
        "deleted bend is not played"
    );
    let mut r = Recorder::default();
    let mut s = Scheduler::new(idx, SchedulerLimits::default());
    s.seek(700, &mut r);
    assert!(
        r.messages.iter().any(|m| (m.1, m.2, m.3) == bend_12000),
        "bend chased on seek"
    );
    assert!(
        r.messages
            .iter()
            .any(|m| (m.1, m.2, m.3) == (0xB9, 10, 100)),
        "pan chased on seek"
    );
}

// ---------------------------------------------------------------- §245 Numeric

#[test]
fn numeric_gate_and_velocity_apply_to_the_selection_as_one_step() {
    assert_eq!(parse_gate("1"), Ok(1));
    assert_eq!(parse_gate("1,000,000,000"), Ok(1_000_000_000));
    for bad in ["0", "-5", "1.5", "abc", "", "99999999999999999999999"] {
        assert!(parse_gate(bad).is_err(), "{bad}");
    }
    assert_eq!(parse_velocity(" 127 "), Ok(127));
    for bad in ["0", "128", "-1", "NaN", "12x", ""] {
        assert!(parse_velocity(bad).is_err(), "{bad}");
    }
    let mut d = with_notes(
        960,
        &[
            rec(0, 10, 60),
            rec(100, 20, 61),
            rec(u64::MAX - 100, 50, 62),
        ],
    );
    d.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 200, 0, 127);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::SetDuration {
            track: 0.into(),
            duration: 1_000_000_000,
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::SetVelocity {
            track: 0.into(),
            velocity: 7,
        },
    )
    .unwrap();
    let t = d.track(0).unwrap();
    let sel: Vec<_> = t
        .notes
        .iter()
        .filter(|n| n.selected)
        .map(|n| (n.start_tick, n.duration_ticks, n.attrs.velocity()))
        .collect();
    assert_eq!(sel, vec![(0, 1_000_000_000, 7), (100, 1_000_000_000, 7)]);
    assert_eq!(h.undo_len(), 2);
    undo_redo_roundtrip(&mut h, &mut d);
    // A gate that would end beyond the largest tick is refused; nothing changes.
    d.track_mut(0).unwrap().notes.select_all();
    let fp = d.state_fingerprint();
    assert!(
        h.apply(
            &mut d,
            Operation::SetDuration {
                track: 0.into(),
                duration: 1000
            }
        )
        .is_err()
    );
    assert_eq!(d.state_fingerprint(), fp);
    // Values survive save and export.
    let back = roundtrip_lup(&d);
    assert_eq!(
        back.track(0)
            .unwrap()
            .notes
            .iter()
            .next()
            .unwrap()
            .duration_ticks,
        1_000_000_000
    );
}

// -------------------------------------------------------------- §246 Quantize

#[test]
fn quantize_ticks_gates_and_both_on_the_selection() {
    // Notes near tick 0, on ties, overlapping, long, huge and zero-length.
    let recs = vec![
        rec(0, 0, 60),            // imported zero-length: stays zero
        rec(5, 110, 61),          // start 5 → 0; gate 110 → 120
        rec(60, 60, 62),          // tie → 120; gate 60 → 120 (tie up)
        rec(119, 1, 63),          // → 120; gate 1 → 120 (minimum one step)
        rec(125, 10_000_001, 64), // long
        rec(130, 500, 65),        // overlaps the others
    ];
    let spec = |mode| QuantizeSpec {
        mode,
        grid: 120,
        origin: 0,
    };
    let run = |mode: QuantizeMode| -> Vec<(u64, u64)> {
        let mut d = with_notes(480, &recs);
        d.track_mut(0).unwrap().notes.select_all();
        let p = preview_quantize(&d, 0, &spec(mode));
        let edit = prepare_quantize(&d, 0, &spec(mode), EditControl::default()).unwrap();
        let mut h = History::default();
        let before = d.state_fingerprint();
        h.apply(
            &mut d,
            Operation::Prepared {
                edit,
                label: "Quantize",
            },
        )
        .unwrap();
        let out: Vec<(u64, u64)> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.duration_ticks))
            .collect();
        // The preview counted what the edit did.
        let moved = out
            .iter()
            .zip(&recs)
            .filter(|(o, r)| o.0 != r.start_tick)
            .count();
        assert_eq!(p.starts_moved, moved);
        assert_eq!(p.notes, recs.len());
        // Undo / redo reproduce the exact result.
        h.undo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), before);
        h.redo(&mut d).unwrap();
        let again: Vec<(u64, u64)> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.duration_ticks))
            .collect();
        assert_eq!(again, out);
        // Nothing else moved.
        assert_eq!(d.conductor().revision(), 0);
        out
    };
    let mut ticks = run(QuantizeMode::Ticks);
    ticks.sort();
    assert_eq!(
        ticks,
        vec![
            (0, 0),
            (0, 110),
            (120, 1),
            (120, 60),
            (120, 500),
            (120, 10_000_001)
        ]
    );
    let mut gates = run(QuantizeMode::Gates);
    gates.sort();
    assert_eq!(
        gates,
        vec![
            (0, 0),
            (5, 120),
            (60, 120),
            (119, 120),
            (125, 9_999_960),
            (130, 480)
        ]
    );
    let mut both = run(QuantizeMode::Both);
    both.sort();
    assert_eq!(
        both,
        vec![
            (0, 0),
            (0, 120),
            (120, 120),
            (120, 120),
            (120, 480),
            (120, 9_999_960)
        ]
    );
    // Only selected notes change; an empty selection is refused.
    let mut d = with_notes(480, &recs);
    assert!(prepare_quantize(&d, 0, &spec(QuantizeMode::Both), EditControl::default()).is_err());
    d.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 1000, 63, 63);
    let edit = prepare_quantize(&d, 0, &spec(QuantizeMode::Ticks), EditControl::default()).unwrap();
    assert_eq!(edit.affected_notes(), 1);
    // Grid origin: grid points at 10 + k·120.
    let mut d = with_notes(480, &[rec(75, 5, 60)]);
    d.track_mut(0).unwrap().notes.select_all();
    let o = QuantizeSpec {
        mode: QuantizeMode::Ticks,
        grid: 120,
        origin: 10,
    };
    let edit = prepare_quantize(&d, 0, &o, EditControl::default()).unwrap();
    editor::commit(&mut d, edit).unwrap();
    assert_eq!(
        d.track(0).unwrap().notes.iter().next().unwrap().start_tick,
        130
    );
    // Atomic failure: one note beyond the last grid point refuses everything.
    let mut d = with_notes(480, &[rec(10, 5, 60), rec(u64::MAX - 1, 1, 60)]);
    d.track_mut(0).unwrap().notes.select_all();
    let fp = d.state_fingerprint();
    let big = QuantizeSpec {
        mode: QuantizeMode::Ticks,
        grid: 1 << 40,
        origin: 0,
    };
    assert!(prepare_quantize(&d, 0, &big, EditControl::default()).is_err());
    assert_eq!(d.state_fingerprint(), fp);
    // Export / re-import keeps the quantized notes.
    let mut d = with_notes(480, &recs);
    d.track_mut(0).unwrap().notes.select_all();
    let edit = prepare_quantize(&d, 0, &spec(QuantizeMode::Both), EditControl::default()).unwrap();
    editor::commit(&mut d, edit).unwrap();
    let back = reimport(&d);
    let n = |d: &PlumDocument| -> Vec<(u64, u64)> {
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.duration_ticks))
            .collect()
    };
    assert_eq!(n(&back), n(&d));
}

// ---------------------------------------------------------- §249 Persistence

#[test]
fn a_project_using_every_v03_feature_saves_and_reopens_identically() {
    let mut d = with_notes(
        960,
        &(0..200u64)
            .map(|i| rec(i * 120, 240, 40 + (i % 60) as u8))
            .collect::<Vec<_>>(),
    );
    d.add_track().unwrap();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 960, 36)], false)
        .unwrap();
    let mut h = History::default();
    let e = d
        .conductor()
        .prepare_set_start_tempo(bpm("133.333"))
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let id = d.conductor().tempo_points()[0].id;
    let e = d
        .conductor()
        .prepare_set_interp(id, Interp::Linear)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_tempo(None, 7680, bpm("174.5"), Interp::Step)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_signature(None, 3840, 7, 3)
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_marker(None, 0, Some(7680), "Intro ✓")
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = d
        .conductor()
        .prepare_set_marker(None, 9000, None, "Drop")
        .unwrap();
    conductor_op(&mut h, &mut d, e);
    let e = controllers::prepare_set(
        d.track(1).unwrap(),
        ControllerKind::PitchBend,
        0,
        None,
        480,
        100,
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit: e,
            label: "Pitch Bend",
        },
    )
    .unwrap();
    let e = controllers::prepare_set(d.track(1).unwrap(), ControllerKind::PAN, 5, None, 960, 3)
        .unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit: e,
            label: "Pan",
        },
    )
    .unwrap();
    d.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 1000, 0, 127);
    h.apply(
        &mut d,
        Operation::SetVelocity {
            track: 0.into(),
            velocity: 33,
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::SetDuration {
            track: 0.into(),
            duration: 77,
        },
    )
    .unwrap();
    let p = d.project_mut();
    p.onion.mode = OnionMode::AllOthers;
    p.onion.opacity = 55;
    p.soundfont = Some(SoundFontRef {
        path: "C:\\SF\\Custom.sf2".into(),
    });
    let back = roundtrip_lup(&d);
    assert!(back.conductor().same_content(d.conductor()));
    assert_eq!(back.project(), d.project());
    assert_eq!(back.fingerprint(), d.fingerprint());
    // …and a second save is byte-identical (deterministic).
    assert_eq!(
        project_to_bytes(&back).unwrap(),
        project_to_bytes(&d).unwrap()
    );
}

// ------------------------------------------------------ §250 Repeated execution

#[test]
fn repeated_edits_undo_and_saves_stay_consistent() {
    let mut d = with_notes(
        480,
        &(0..50u64).map(|i| rec(i * 37, 50, 60)).collect::<Vec<_>>(),
    );
    let mut h = History::default();
    for round in 0..5u64 {
        // Add / delete a marker.
        let e = d
            .conductor()
            .prepare_set_marker(None, round * 100, None, "M")
            .unwrap();
        conductor_op(&mut h, &mut d, e);
        let id = d.conductor().markers().last().unwrap().id;
        let e = d.conductor().prepare_remove_markers(&[id]).unwrap();
        conductor_op(&mut h, &mut d, e);
        // Change the tempo.
        let e = d
            .conductor()
            .prepare_set_start_tempo(bpm(&format!("{}", 100 + round)))
            .unwrap();
        conductor_op(&mut h, &mut d, e);
        // Quantize (the second pass on aligned notes changes nothing).
        d.track_mut(0).unwrap().notes.select_all();
        let spec = QuantizeSpec {
            mode: QuantizeMode::Both,
            grid: 60,
            origin: 0,
        };
        let edit = prepare_quantize(&d, 0, &spec, EditControl::default()).unwrap();
        h.apply(
            &mut d,
            Operation::Prepared {
                edit,
                label: "Quantize",
            },
        )
        .unwrap();
        let again = prepare_quantize(&d, 0, &spec, EditControl::default()).unwrap();
        assert_eq!(again, editor::PreparedEdit::Identity, "round {round}");
        // Save / reopen.
        d = roundtrip_lup(&d);
        d.conductor().validate().unwrap();
    }
    // Undo everything, redo everything.
    let end = d.state_fingerprint();
    let mut n = 0;
    while h.undo(&mut d).unwrap().is_some() {
        n += 1;
    }
    // 5 rounds × (add marker, delete marker, tempo) + the first quantize; later
    // quantize passes change nothing and record nothing.
    assert_eq!(n, 16);
    while h.redo(&mut d).unwrap().is_some() {}
    assert_eq!(d.state_fingerprint(), end);
}

// ------------------------------------------------------- §251 Property tests

#[test]
fn random_tempo_maps_are_monotonic_invertible_and_match_the_reference() {
    let mut rng = Rng(0xC0FF_EE00_1234_5678);
    for case in 0..60 {
        let ppqn = 1 + rng.below(4000) as u32;
        let mut d = project(ppqn);
        let mut tick = 0u64;
        let mut pts: Vec<(u64, f64, bool)> = Vec::new();
        for i in 0..(1 + rng.below(12)) {
            let b = 5.0 + rng.below(100_000) as f64 / 100.0;
            let linear = rng.below(2) == 0;
            let value = TempoValue::Bpm(Bpm::parse(&format!("{b}")).unwrap());
            let interp = if linear { Interp::Linear } else { Interp::Step };
            let mut h = History::default();
            let e = if i == 0 {
                let e = d.conductor().prepare_set_start_tempo(value).unwrap();
                conductor_op(&mut h, &mut d, e);
                d.conductor()
                    .prepare_set_interp(d.conductor().tempo_points()[0].id, interp)
                    .unwrap()
            } else {
                d.conductor()
                    .prepare_set_tempo(None, tick, value, interp)
                    .unwrap()
            };
            conductor_op(&mut h, &mut d, e);
            pts.push((tick, b, linear));
            tick += 1 + rng.below(u64::from(ppqn) * 16);
        }
        let map = TempoMap::for_document(&d, 44_100);
        // Reference: piecewise closed forms.
        let reference = |t: u64| -> f64 {
            let q = f64::from(ppqn);
            let mut s = 0.0;
            for (i, &(t0, b0, lin)) in pts.iter().enumerate() {
                if t <= t0 {
                    break;
                }
                let next = pts.get(i + 1);
                let seg_end = next.map_or(u64::MAX, |n| n.0);
                let x = (t.min(seg_end) - t0) as f64;
                match (lin, next) {
                    (true, Some(&(t1, b1, _))) => {
                        s += reference_ramp_seconds(q, b0, b1, (t1 - t0) as f64, x)
                    }
                    _ => s += 60.0 / q * x / b0,
                }
            }
            s
        };
        let mut prev = 0u64;
        for k in 0..400u64 {
            let t = (tick + 1000) * k / 400;
            let got = map.seconds_at_tick(t);
            let want = reference(t);
            assert!(
                (got - want).abs() <= 1e-9 * want.max(1.0),
                "case {case} tick {t}: {got} vs {want}"
            );
            let f = map.frame_at_tick(t);
            assert!(f >= prev, "monotonic");
            prev = f;
            // The playhead tick at a frame is the last tick that has started.
            let back = map.tick_at_frame(f);
            assert!(back >= t && map.frame_at_tick(back) == f);
            assert!(map.frame_at_tick(back.saturating_add(1)) > f || back == u64::MAX);
        }
    }
}

#[test]
fn random_controller_edits_round_trip_through_midi_and_lup() {
    let mut rng = Rng(0x5EED_0000_0000_0001);
    for _ in 0..30 {
        let mut d = with_notes(480, &[rec(0, 4800, 60)]);
        let mut h = History::default();
        for _ in 0..40 {
            let kind = if rng.below(2) == 0 {
                ControllerKind::PitchBend
            } else {
                ControllerKind::PAN
            };
            let ch = rng.below(16) as u8;
            let tick = rng.below(5000);
            let value = rng.below(u64::from(kind.max()) + 1) as u16;
            let pts = collect_points(&d.track(0).unwrap().events, kind, ch);
            let target = (!pts.is_empty() && rng.below(3) == 0)
                .then(|| pts[rng.below(pts.len() as u64) as usize].index);
            let e = if !pts.is_empty() && rng.below(6) == 0 {
                controllers::prepare_delete(d.track(0).unwrap(), &[pts[0].index]).unwrap()
            } else {
                controllers::prepare_set(d.track(0).unwrap(), kind, ch, target, tick, value)
                    .unwrap()
            };
            h.apply(
                &mut d,
                Operation::Events {
                    edit: e,
                    label: "x",
                },
            )
            .unwrap();
            // Never two events of one kind/channel at one tick.
            for k in [ControllerKind::PitchBend, ControllerKind::PAN] {
                for c in 0..16 {
                    let raw = d
                        .track(0)
                        .unwrap()
                        .events
                        .events()
                        .iter()
                        .filter(|e| k.value_of(e, c).is_some())
                        .map(|e| e.tick)
                        .collect::<Vec<_>>();
                    let mut dedup = raw.clone();
                    dedup.dedup();
                    assert_eq!(raw, dedup);
                }
            }
        }
        let back = reimport(&d);
        let lup = roundtrip_lup(&d);
        for k in [ControllerKind::PitchBend, ControllerKind::PAN] {
            for c in 0..16 {
                let want = collect_points(&d.track(0).unwrap().events, k, c);
                for other in [&back, &lup] {
                    let got = collect_points(&other.track(0).unwrap().events, k, c);
                    let strip = |v: &[plumspirit_engine::editor::ControllerPoint]| {
                        v.iter().map(|p| (p.tick, p.value)).collect::<Vec<_>>()
                    };
                    assert_eq!(strip(&got), strip(&want));
                }
            }
        }
        while h.undo(&mut d).unwrap().is_some() {}
        assert!(d.track(0).unwrap().events.is_empty());
    }
}

#[test]
fn smpte_files_keep_their_timing_and_only_markers_are_editable() {
    let tr = {
        let mut t = TrackBytes::new();
        t.meta(0, 0x06, b"Cue A")
            .on(0, 0, 60, 100)
            .off(100, 0, 60, 0);
        t.eot(100)
    };
    let (d, _) = import_bytes(&smf(1, 0xE728, &[tr])).unwrap();
    assert_eq!(d.division(), Division::from_raw(0xE728));
    assert_eq!(d.conductor().scope(), ConductorScope::MarkersOnly);
    assert!(
        d.conductor()
            .prepare_set_tempo(None, 10, bpm("100"), Interp::Step)
            .is_err()
    );
    assert!(d.conductor().prepare_set_signature(None, 10, 3, 2).is_err());
    assert!(
        d.conductor()
            .prepare_set_marker(None, 10, None, "Cue B")
            .is_ok()
    );
    assert_eq!(d.conductor().markers().len(), 1);
    // Round trip keeps the SMPTE division and the marker.
    let back = reimport(&d);
    assert_eq!(back.division(), d.division());
    assert_eq!(back.conductor().markers()[0].title, "Cue A");
}
