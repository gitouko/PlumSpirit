//! Golden fixtures (contract §118), semantic round trips (§50, §201) and export
//! validation behaviour.

mod common;

use common::*;
use plumspirit_engine::editor::{self, ExactFactor};
use plumspirit_engine::midi::semantic::semantic_from_bytes;
use plumspirit_engine::midi::{
    self, ImportWarning, export_to_bytes, import_bytes, validate_exported_bytes,
};
use plumspirit_engine::model::{Bpm, NoteOffKind, PlumDocument, SmfFormat, TimingKind};

fn roundtrip_semantic(bytes: &[u8]) -> (PlumDocument, Vec<u8>) {
    let (doc, _) = import_bytes(bytes).expect("import");
    let out = export_to_bytes(&doc).expect("export");
    let a = semantic_from_bytes(bytes).unwrap();
    let b = semantic_from_bytes(&out).unwrap();
    assert_eq!(a, b, "semantic round trip differs");
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out)
        .expect("validation");
    (doc, out)
}

// 1. Format 0 basic notes
#[test]
fn golden_format0_basic_notes() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100)
        .off(96, 0, 60, 64)
        .on(96, 0, 64, 90)
        .off(192, 0, 64, 0);
    let bytes = smf(0, 96, &[t.eot(192)]);
    let (doc, _) = roundtrip_semantic(&bytes);
    assert_eq!(doc.source_format(), SmfFormat::SingleTrack);
    assert_eq!(doc.track_count(), 1);
    assert_eq!(
        notes_of(&doc, 0),
        vec![(0, 96, 60, 100, 0), (96, 96, 64, 90, 0)]
    );
    let n0 = doc.track(0).unwrap().notes.iter().next().unwrap();
    assert_eq!(n0.attrs.off_velocity(), 64);
    assert_eq!(n0.attrs.off_kind(), NoteOffKind::NoteOff);
    assert_eq!(doc.track(0).unwrap().end_of_track_tick, Some(192));
}

// 2. Format 1 two tracks (tracks are independent, channels preserved)
#[test]
fn golden_format1_two_tracks() {
    let mut conductor = TrackBytes::new();
    conductor.meta(0, 0x51, &[0x07, 0xA1, 0x20]);
    let mut a = TrackBytes::new();
    a.on(0, 0, 60, 100).off(10, 0, 60, 0);
    let mut b = TrackBytes::new();
    b.on(5, 9, 36, 127)
        .on(5, 3, 40, 1)
        .off(20, 9, 36, 0)
        .off(25, 3, 40, 0);
    let bytes = smf(1, 480, &[conductor.eot(0), a.eot(10), b.eot(30)]);
    let (doc, _) = roundtrip_semantic(&bytes);
    assert_eq!(doc.track_count(), 3);
    assert_eq!(doc.track(0).unwrap().notes.len(), 0);
    assert_eq!(notes_of(&doc, 1), vec![(0, 10, 60, 100, 0)]);
    assert_eq!(
        notes_of(&doc, 2),
        vec![(5, 15, 36, 127, 9), (5, 20, 40, 1, 3)]
    );
    assert_eq!(doc.track(2).unwrap().end_of_track_tick, Some(30));
}

// 3-8. tempo, time signatures, program/control changes, SysEx, track name, text
#[test]
fn golden_meta_and_channel_events_survive() {
    let mut t = TrackBytes::new();
    t.meta(0, 0x03, b"Piano \xe9") // Latin-1 byte: kept raw, displayed lossily
        .meta(0, 0x51, &[0x07, 0xA1, 0x20])
        .meta(0, 0x58, &[3, 2, 24, 8])
        .meta(0, 0x59, &[0xFE, 0])
        .ev(0, &[0xC0, 5])
        .ev(0, &[0xB0, 7, 100])
        .sysex(0, &[0x7E, 0x7F, 0x09, 0x01, 0xF7])
        .on(0, 0, 60, 90)
        .meta(48, 0x51, &[0x06, 0x1A, 0x80])
        .ev(48, &[0xE0, 0x00, 0x60])
        .ev(50, &[0xD0, 30])
        .ev(51, &[0xA0, 60, 12])
        .meta(60, 0x01, b"hello")
        .meta(60, 0x06, b"Chorus")
        .meta(60, 0x02, b"(c) nobody")
        .meta(61, 0x7F, &[0x00, 0x00, 0x41, 1, 2])
        .meta(62, 0x60, &[9, 9, 9]) // undefined meta type: preserved
        .off(96, 0, 60, 0)
        .meta(200, 0x58, &[7, 3, 24, 8]);
    let bytes = smf(1, 96, &[t.eot(300)]);
    let (doc, _) = roundtrip_semantic(&bytes);
    let tr = doc.track(0).unwrap();
    assert_eq!(tr.name(), Some("Piano é"));
    assert_eq!(tr.events.track_name(), Some(&b"Piano \xe9"[..]));
    assert_eq!(tr.notes.len(), 1);
    // 17 events; the 2 tempos, 2 time signatures and the Marker belong to the
    // Conductor (and are written back at their original places, as the semantic
    // round trip above shows).
    assert_eq!(tr.events.len(), 12);
    let c = doc.conductor();
    assert_eq!(
        c.tempo_points()
            .iter()
            .map(|p| (p.tick, p.value))
            .collect::<Vec<_>>(),
        vec![
            (0, plumspirit_engine::model::TempoValue::Micros(500_000)),
            (48, plumspirit_engine::model::TempoValue::Micros(400_000))
        ]
    );
    assert_eq!(c.markers().len(), 1);
    assert_eq!(
        (c.markers()[0].start, c.markers()[0].title.as_str()),
        (60, "Chorus")
    );
    let meters = doc.meter_events(0);
    assert_eq!(meters, vec![(0, 3, 2), (200, 7, 3)]);
}

// 9. Note On velocity zero termination is preserved
#[test]
fn golden_note_on_zero_termination() {
    let mut t = TrackBytes::new();
    // Running status: 90 3C 64, 3C 00 (Note On vel 0 via running status)
    t.ev(0, &[0x90, 60, 100])
        .ev(10, &[60, 0])
        .ev(10, &[62, 70])
        .ev(20, &[62, 0]);
    let bytes = smf(0, 96, &[t.eot(20)]);
    let (doc, out) = roundtrip_semantic(&bytes);
    let kinds: Vec<NoteOffKind> = doc
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.attrs.off_kind())
        .collect();
    assert_eq!(
        kinds,
        vec![NoteOffKind::NoteOnZero, NoteOffKind::NoteOnZero]
    );
    // Export keeps running status: the whole track is written with one status byte.
    let status_bytes = out[22..].iter().filter(|b| **b == 0x90).count();
    assert_eq!(status_bytes, 1, "{out:02X?}");
}

// 10. Overlapping same-pitch notes pair FIFO
#[test]
fn golden_overlapping_same_pitch_fifo() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 10)
        .on(10, 0, 60, 20)
        .off(20, 0, 60, 30)
        .off(30, 0, 60, 40);
    let bytes = smf(1, 96, &[t.eot(30)]);
    let (doc, _) = roundtrip_semantic(&bytes);
    let n: Vec<(u64, u64, u8, u8)> = doc
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| {
            (
                n.start_tick,
                n.end_tick(),
                n.attrs.velocity(),
                n.attrs.off_velocity(),
            )
        })
        .collect();
    assert_eq!(n, vec![(0, 20, 10, 30), (10, 30, 20, 40)]);
}

// 11. Zero-duration note
#[test]
fn golden_zero_duration_note() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100)
        .off(0, 0, 60, 0)
        .on(0, 0, 60, 90)
        .off(5, 0, 60, 0);
    let bytes = smf(1, 96, &[t.eot(5)]);
    let (doc, _) = roundtrip_semantic(&bytes);
    assert_eq!(
        notes_of(&doc, 0),
        vec![(0, 0, 60, 100, 0), (0, 5, 60, 90, 0)]
    );
    // Unchanged round trip keeps the zero duration.
    let (doc2, _) = import_bytes(&export_to_bytes(&doc).unwrap()).unwrap();
    assert_eq!(notes_of(&doc2, 0), notes_of(&doc, 0));
}

// 12. Empty track (only End of Track)
#[test]
fn golden_empty_track() {
    let mut t0 = TrackBytes::new();
    t0.on(0, 0, 60, 100).off(10, 0, 60, 0);
    let mut empty = TrackBytes::new();
    let bytes = smf(1, 96, &[t0.eot(10), empty.eot(0)]);
    let (doc, out) = roundtrip_semantic(&bytes);
    assert_eq!(doc.track(1).unwrap().notes.len(), 0);
    assert_eq!(doc.track(1).unwrap().events.len(), 0);
    // Empty track exports as exactly "00 FF 2F 00".
    let tail = &out[out.len() - 12..];
    assert_eq!(tail, b"MTrk\x00\x00\x00\x04\x00\xFF\x2F\x00");
}

// 13. SMPTE timing preserved exactly
#[test]
fn golden_smpte_timing() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100).off(40, 0, 60, 0);
    let bytes = smf(0, 0xE728, &[t.eot(1000)]);
    let (doc, out) = roundtrip_semantic(&bytes);
    assert_eq!(doc.division().smf_word(), Some(0xE728));
    assert_eq!(
        doc.division().kind(),
        TimingKind::Smpte {
            frames_per_second: 25,
            ticks_per_frame: 40
        }
    );
    assert_eq!(&out[12..14], &[0xE7, 0x28]);
}

// 14. Multiple events at the same tick keep their interleaving
#[test]
fn golden_same_tick_order_preserved_exactly() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100)
        .off(96, 0, 60, 0)
        .ev(96, &[0xC0, 12]) // program change between the note-off and the note-on
        .on(96, 0, 62, 100)
        .ev(96, &[0xB0, 64, 127]) // sustain after the note-on
        .meta(96, 0x06, b"M")
        .off(192, 0, 62, 0);
    let bytes = smf(1, 96, &[t.eot(192)]);
    let (doc, out) = import_bytes(&bytes)
        .map(|(d, _)| {
            let o = export_to_bytes(&d).unwrap();
            (d, o)
        })
        .unwrap();
    // Byte-exact here because the source used no running status and PlumSpirit's
    // canonical order (ends, then starts) matches the source at every tick.
    assert_eq!(out, bytes, "same-tick interleaving changed");
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
}

#[test]
fn format0_promotes_to_format1_when_track_added() {
    // Contract §202
    let mut t = TrackBytes::new();
    t.meta(0, 0x51, &[0x07, 0xA1, 0x20])
        .on(0, 0, 60, 100)
        .off(96, 0, 60, 0);
    let bytes = smf(0, 96, &[t.eot(96)]);
    let (mut doc, _) = import_bytes(&bytes).unwrap();
    let ti = doc.add_track().unwrap();
    editor::insert_note(&mut doc, ti, rec(48, 24, 72)).unwrap();
    let out = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    let (doc2, _) = import_bytes(&out).unwrap();
    assert_eq!(doc2.source_format(), SmfFormat::Parallel);
    assert_eq!(doc2.track_count(), 2);
    assert_eq!(notes_of(&doc2, 0), vec![(0, 96, 60, 100, 0)]);
    assert_eq!(notes_of(&doc2, 1), vec![(48, 24, 72, 100, 0)]);
    let sem_orig = semantic_from_bytes(&bytes).unwrap();
    let sem_new = semantic_from_bytes(&out).unwrap();
    assert_eq!(sem_orig.tracks[0], sem_new.tracks[0], "first track changed");
}

#[test]
fn moving_a_note_leaves_other_events_untouched() {
    // Contract §201: critical regression test.
    let mut t = TrackBytes::new();
    t.meta(0, 0x51, &[0x07, 0xA1, 0x20])
        .ev(0, &[0xC0, 3])
        .ev(0, &[0xB0, 10, 64])
        .meta(0, 0x01, b"text")
        .sysex(0, &[0x41, 0x10, 0xF7])
        .on(0, 0, 60, 100)
        .on(0, 0, 64, 100)
        .off(96, 0, 60, 0)
        .off(96, 0, 64, 0)
        .ev(100, &[0xB0, 11, 90]);
    let bytes = smf(1, 96, &[t.eot(200)]);
    let (mut doc, _) = import_bytes(&bytes).unwrap();
    // Select only the C4 note and move it +48 ticks, +2 semitones.
    doc.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 0, 60, 60);
    editor::move_selected(&mut doc, 0, 48, 2).unwrap();
    let out = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    let a = semantic_from_bytes(&bytes).unwrap();
    let b = semantic_from_bytes(&out).unwrap();
    assert_eq!(
        a.tracks[0].other, b.tracks[0].other,
        "non-note events changed"
    );
    assert_eq!(a.tracks[0].end_of_track, b.tracks[0].end_of_track);
    let (doc2, _) = import_bytes(&out).unwrap();
    assert_eq!(
        notes_of(&doc2, 0),
        vec![(0, 96, 64, 100, 0), (48, 96, 62, 100, 0)]
    );
}

#[test]
fn export_is_deterministic_and_stable_across_cycles() {
    for seed in 1..6u64 {
        let bytes = generate_dense_smf(3000, 480, seed);
        let (doc1, _) = import_bytes(&bytes).unwrap();
        let e1 = export_to_bytes(&doc1).unwrap();
        let e1b = export_to_bytes(&doc1).unwrap();
        assert_eq!(e1, e1b, "repeated export differs");
        let (doc2, _) = import_bytes(&e1).unwrap();
        let e2 = export_to_bytes(&doc2).unwrap();
        assert_eq!(e1, e2, "export(import(export(import))) not stable");
        assert_eq!(doc1.fingerprint(), doc2.fingerprint());
        assert_eq!(
            semantic_from_bytes(&bytes).unwrap(),
            semantic_from_bytes(&e1).unwrap()
        );
    }
}

#[test]
fn validation_detects_corrupted_output() {
    let bytes = generate_dense_smf(500, 96, 9);
    let (doc, _) = import_bytes(&bytes).unwrap();
    let good = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &good).unwrap();
    // Flip data bytes at a few positions inside the track; every corruption must be
    // detected (either as a mismatch or as unreadable data).
    let mut rng = Rng(77);
    let mut detected = 0;
    for _ in 0..200 {
        let mut bad = good.clone();
        let pos = 30 + rng.below((bad.len() - 34) as u64) as usize;
        bad[pos] ^= 1 << rng.below(7);
        if bad == good {
            continue;
        }
        assert!(
            validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &bad)
                .is_err(),
            "corruption at {pos} not detected"
        );
        detected += 1;
    }
    assert!(detected > 150);
}

#[test]
fn nested_same_pitch_overlap_reports_repairing_not_failure() {
    let mut doc = PlumDocument::new_empty(plumspirit_engine::model::Division::ppqn(96));
    editor::insert_note(&mut doc, 0, rec(0, 100, 60)).unwrap();
    editor::insert_note(&mut doc, 0, rec(10, 5, 60)).unwrap(); // nested inside the first
    let out = export_to_bytes(&doc).unwrap();
    let report =
        validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    assert_eq!(report.tracks_with_different_pairing, vec![0]);
    let (doc2, _) = import_bytes(&out).unwrap();
    assert_eq!(
        notes_of(&doc2, 0)
            .iter()
            .map(|n| (n.0, n.1))
            .collect::<Vec<_>>(),
        vec![(0, 15), (10, 90)]
    );
}

#[test]
fn unknown_chunks_and_system_messages_preserved() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100).ev(5, &[0xF8]).off(10, 0, 60, 0);
    let track = t.eot(10);
    let mut bytes = smf(1, 96, &[track]);
    bytes.extend(b"XFIH");
    bytes.extend(3u32.to_be_bytes());
    bytes.extend([1, 2, 3]);
    let (doc, summary) = import_bytes(&bytes).unwrap();
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| matches!(w, ImportWarning::UnknownChunk { .. }))
    );
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| matches!(w, ImportWarning::SystemMessages { .. }))
    );
    let out = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    assert_eq!(out, bytes);
}

#[test]
fn export_to_path_safety() {
    // Contract §48/§97/§137
    let dir = std::env::temp_dir().join(format!("plumspirit-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = generate_dense_smf(1000, 480, 3);
    let (mut doc, _) = import_bytes(&bytes).unwrap();

    // 1. Unicode path with spaces; export twice to the same path (overwrite).
    let dest = dir.join("Überlied — test ♪.mid");
    for _ in 0..2 {
        let report = midi::export_to_path(
            &doc,
            &midi::plan_export(&doc),
            &dest,
            midi::ExportControl::default(),
        )
        .unwrap();
        assert_eq!(report.write.notes, 1000);
        let (re, _) = midi::import_file(&dest, Default::default()).unwrap();
        assert_eq!(re.fingerprint(), doc.fingerprint());
    }
    // 2. Invalid destination directory: error, document unchanged, nothing created.
    let fp = doc.fingerprint();
    let bad = dir.join("missing-dir").join("x.mid");
    let err = midi::export_to_path(
        &doc,
        &midi::plan_export(&doc),
        &bad,
        midi::ExportControl::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        midi::ExportError::Io {
            stage: midi::ExportStage::CreateTemp,
            ..
        }
    ));
    assert_eq!(doc.fingerprint(), fp);

    // 3. Unserializable document (a note so far away that no MIDI file can
    //    bridge the silence before it): existing destination not replaced, temp
    //    file cleaned up.
    let before = std::fs::read(&dest).unwrap();
    let ti = doc.add_track().unwrap();
    editor::insert_note(&mut doc, ti, rec(0, 1, 60)).unwrap();
    editor::insert_note(&mut doc, ti, rec(u64::MAX - 10, 1, 60)).unwrap();
    let plan = midi::plan_export(&doc);
    assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
    let err = midi::export_to_path(&doc, &plan, &dest, midi::ExportControl::default()).unwrap_err();
    assert!(matches!(err, midi::ExportError::Blocked(_)), "{err:?}");
    assert_eq!(std::fs::read(&dest).unwrap(), before);
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gaps_longer_than_a_midi_delta_are_bridged_and_reimport_exactly() {
    // Delta times are at most 0x0FFF_FFFF ticks. A note 2^40 ticks in, a note
    // 3·2^30 ticks long and a far-away controller all need filler events.
    let mut doc = PlumDocument::new_project("Far".into(), Bpm::DEFAULT, 960);
    let far = 1u64 << 40;
    editor::insert_note(&mut doc, 0, rec(0, 1, 60)).unwrap();
    editor::insert_note(&mut doc, 0, rec(far, 3 << 30, 61)).unwrap();
    let ti = doc.add_track().unwrap();
    editor::insert_note(&mut doc, ti, rec(far * 2, 5, 62)).unwrap();
    let plan = midi::plan_export(&doc);
    assert!(plan.blockers.is_empty());
    assert!(!plan.needs_approval());
    let bytes = midi::export_to_bytes_with(&doc, &plan).unwrap();
    validate_exported_bytes(&doc, &plan, &bytes).unwrap();
    // Every delta in the file is within the limit (the reader enforces it) and
    // the fillers are PlumSpirit Sequencer-Specific events.
    let semantic = plumspirit_engine::midi::semantic::semantic_from_bytes(&bytes).unwrap();
    let fillers: usize = semantic
        .tracks
        .iter()
        .map(|t| {
            t.other
                .iter()
                .filter(|(_, e)| e[0] == 0xFF && e[1] == 0x7F)
                .count()
        })
        .sum();
    assert!(fillers >= ((far * 2) >> 28) as usize, "{fillers}");
    // Re-import drops the fillers: the same notes, no extra events.
    let (back, _) = import_bytes(&bytes).unwrap();
    let notes = |d: &PlumDocument| -> Vec<Vec<(u64, u64, u32)>> {
        d.tracks()
            .iter()
            .map(|t| {
                t.notes
                    .iter()
                    .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
                    .collect()
            })
            .collect()
    };
    assert_eq!(notes(&back), notes(&doc));
    assert!(back.tracks().iter().all(|t| t.events.is_empty()));
    assert_eq!(back.conductor().tempo_points().len(), 1);
    // Exporting the re-imported file gives the same bytes.
    assert_eq!(export_to_bytes(&back).unwrap(), bytes);
}

#[test]
fn scale_and_flip_then_export_reimport() {
    let bytes = generate_dense_smf(2000, 960, 11);
    let (mut doc, _) = import_bytes(&bytes).unwrap();
    let t = &mut doc.track_mut(0).unwrap().notes;
    t.select_in_region(0, 20_000, 40, 90);
    editor::flip_x(&mut doc, 0).unwrap();
    editor::flip_y(&mut doc, 0).unwrap();
    editor::scale_x(&mut doc, 0, &ExactFactor::parse("1.5").unwrap()).unwrap();
    editor::scale_y(&mut doc, 0, &ExactFactor::parse("0.5").unwrap()).unwrap();
    let out = export_to_bytes(&doc).unwrap();
    let report =
        validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    let (doc2, _) = import_bytes(&out).unwrap();
    assert_eq!(doc2.note_count(), doc.note_count());
    if !report.pairing_differs() {
        assert_eq!(doc2.fingerprint(), doc.fingerprint());
    }
    // Non-note events untouched by note transforms.
    let a = semantic_from_bytes(&bytes).unwrap();
    let b = semantic_from_bytes(&out).unwrap();
    assert_eq!(a.tracks[0].other, b.tracks[0].other);
}
