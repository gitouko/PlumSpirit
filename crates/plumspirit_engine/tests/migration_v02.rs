//! Opening projects saved by PlumSpirit v0.2 (`.lup` format version 1).
//!
//! The fixtures in `tests/fixtures/v02/` were written by the real v0.2 engine
//! (commit c6c816d; the generator is `generate_with_v0.2.rs.txt`), together with
//! v0.2's own MIDI export of each project. A migrated project must keep every note
//! and event, get one Conductor with the tempo map v0.2 would have played, and
//! export the same music v0.2 exported.

mod common;

use plumspirit_engine::midi::{export_to_bytes, import_bytes};
use plumspirit_engine::model::{
    Bpm, OnionMode, PlumDocument, TempoMap, TempoValue, TrackId, conductor::ConductorScope,
};
use plumspirit_engine::native::{LUP_VERSION, project_from_bytes, project_to_bytes};

const FIXTURES: [&str; 4] = [
    "new_project",
    "imported",
    "imported_project_tempo",
    "many_tracks",
];

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/v02/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn open_v02(name: &str) -> PlumDocument {
    let bytes = fixture(&format!("{name}.lup"));
    assert_eq!(
        u16::from_le_bytes([bytes[8], bytes[9]]),
        1,
        "{name} is a v0.2 file"
    );
    let (doc, report) = project_from_bytes(&bytes).unwrap();
    assert_eq!(report.version, 1);
    assert_eq!(report.migrated_from, Some(1));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("PlumSpirit v0.2")),
        "{:?}",
        report.warnings
    );
    doc.conductor().validate().unwrap();
    doc
}

/// (start, duration, attributes) of one note.
type NoteKey = (u64, u64, u32);

/// (track, notes) of every track.
fn notes(d: &PlumDocument) -> Vec<(TrackId, Vec<NoteKey>)> {
    d.tracks()
        .iter()
        .map(|t| {
            (
                t.id(),
                t.notes
                    .iter()
                    .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
                    .collect(),
            )
        })
        .collect()
}

/// The Conductor's tempo map in MIDI units: (tick, µs per quarter).
fn tempo_micros(d: &PlumDocument) -> Vec<(u64, u32)> {
    d.conductor()
        .tempo_points()
        .iter()
        .filter(|p| !p.implicit)
        .map(|p| {
            let us = match p.value {
                TempoValue::Micros(us) => us,
                TempoValue::Bpm(b) => b.to_micros_per_quarter().micros,
            };
            (p.tick, us)
        })
        .collect()
}

fn signatures(d: &PlumDocument) -> Vec<(u64, u8, u8)> {
    d.conductor()
        .time_signatures()
        .iter()
        .filter(|s| !s.implicit)
        .map(|s| (s.tick, s.numerator, s.denom_pow))
        .collect()
}

fn markers(d: &PlumDocument) -> Vec<(u64, Option<u64>, String, bool)> {
    d.conductor()
        .markers()
        .iter()
        .map(|m| (m.start, m.end, m.title.clone(), m.cue))
        .collect()
}

#[test]
fn every_v02_fixture_migrates_to_one_conductor_with_the_same_music() {
    for name in FIXTURES {
        let doc = open_v02(name);
        // v0.2's own export of the same project, read by v0.3.
        let (v02_mid, _) = import_bytes(&fixture(&format!("{name}.mid"))).unwrap();
        // Our export of the migrated project, read back.
        let ours = export_to_bytes(&doc).unwrap();
        let (re, _) = import_bytes(&ours).unwrap();
        for other in [&v02_mid, &re] {
            assert_eq!(
                notes(other).into_iter().map(|t| t.1).collect::<Vec<_>>(),
                notes(&doc).into_iter().map(|t| t.1).collect::<Vec<_>>(),
                "{name}: notes"
            );
            assert_eq!(tempo_micros(other), tempo_micros(&doc), "{name}: tempo");
            assert_eq!(signatures(other), signatures(&doc), "{name}: signatures");
            assert_eq!(markers(other), markers(&doc), "{name}: markers");
            // Same playback timing, tick by tick (the project keeps an exact
            // decimal BPM, which MIDI rounds to whole µs: at most one frame).
            let (a, b) = (
                TempoMap::for_document(other, 48_000),
                TempoMap::for_document(&doc, 48_000),
            );
            for tick in (0..10_000u64).step_by(37) {
                assert!(
                    a.frame_at_tick(tick).abs_diff(b.frame_at_tick(tick)) <= 1,
                    "{name} @ {tick}"
                );
            }
        }
        // Both MIDI files play identically.
        let (a, b) = (
            TempoMap::for_document(&v02_mid, 48_000),
            TempoMap::for_document(&re, 48_000),
        );
        for tick in (0..10_000u64).step_by(37) {
            assert_eq!(
                a.frame_at_tick(tick),
                b.frame_at_tick(tick),
                "{name} @ {tick}"
            );
        }
        // Non-note events other than the Conductor's stay in their tracks.
        for (t, v) in doc.tracks().iter().zip(v02_mid.tracks()) {
            assert_eq!(
                t.events.len(),
                v.events.len(),
                "{name}: events of {:?}",
                t.id()
            );
        }
    }
}

#[test]
fn migration_keeps_ids_settings_and_the_original_file_untouched() {
    for name in FIXTURES {
        let original = fixture(&format!("{name}.lup"));
        let (doc, _) = project_from_bytes(&original).unwrap();
        // Reading never changes the source bytes (the file is only read).
        assert_eq!(fixture(&format!("{name}.lup")), original);
        // Saving writes the current format; reopening needs no migration and
        // gives exactly the migrated state; saving again is byte-identical.
        let v2 = project_to_bytes(&doc).unwrap();
        assert_eq!(u16::from_le_bytes([v2[8], v2[9]]), LUP_VERSION);
        let (again, report) = project_from_bytes(&v2).unwrap();
        assert_eq!(report.migrated_from, None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(again.state_fingerprint(), doc.state_fingerprint(), "{name}");
        assert!(again.conductor().same_content(doc.conductor()));
        assert_eq!(project_to_bytes(&again).unwrap(), v2);
    }
    // Track ids (including gaps from deleted tracks) and next id survive.
    let many = open_v02("many_tracks");
    assert_eq!(many.track_count(), 68);
    let ids: Vec<u32> = many.tracks().iter().map(|t| t.id().0).collect();
    assert!(
        !ids.contains(&3) && !ids.contains(&40) && !ids.contains(&65),
        "{ids:?}"
    );
    assert_eq!(many.next_track_id(), 71);
    assert_eq!(many.project().onion.mode, OnionMode::AllOthers);
    assert_eq!(many.project().onion.opacity, 60);
    // SoundFont reference and exact decimal tempo.
    let new = open_v02("new_project");
    assert_eq!(
        new.project().soundfont.as_ref().map(|s| s.path.as_str()),
        Some("C:\\SoundFonts\\GeneralUser GS.sf2")
    );
    assert_eq!(new.project().name, "Legacy ✓");
}

#[test]
fn a_v02_new_project_gets_its_exact_tempo_and_meter_in_the_conductor() {
    let doc = open_v02("new_project");
    let c = doc.conductor();
    assert_eq!(c.scope(), ConductorScope::Timeline);
    assert_eq!(c.tempo_points().len(), 1);
    assert_eq!(
        c.start_tempo(),
        TempoValue::Bpm(Bpm::parse("150.5").unwrap())
    );
    assert!(!c.tempo_points()[0].implicit);
    assert_eq!(signatures(&doc), vec![(0, 4, 2)]);
    // The raw 4/4 event moved out of the track: it is not written twice.
    assert!(doc.tracks()[0].events.is_empty());
    let (re, _) = import_bytes(&export_to_bytes(&doc).unwrap()).unwrap();
    assert_eq!(signatures(&re), vec![(0, 4, 2)]);
}

#[test]
fn a_v02_import_gets_the_tempo_that_played_and_keeps_controllers_in_place() {
    let doc = open_v02("imported");
    // 500,000 and 400,000 µs at tick 0: v0.2 played the later one.
    assert_eq!(
        tempo_micros(&doc),
        vec![(0, 400_000), (960, 600_000), (1920, 250_000)]
    );
    assert_eq!(signatures(&doc), vec![(0, 3, 2), (2880, 7, 3)]);
    assert_eq!(
        markers(&doc),
        vec![
            (0, None, "Intro".into(), false),
            (1440, None, "Drop".into(), false),
            (3000, None, "Hit".into(), true)
        ]
    );
    // Track 0 keeps only its name; track 1 keeps pan, program and pitch bend.
    assert_eq!(doc.tracks()[0].events.len(), 1);
    assert_eq!(doc.tracks()[0].name(), Some("Conductor é"));
    assert_eq!(doc.tracks()[1].events.len(), 3);
    // With a project tempo, v0.2 wrote that tempo at tick 0 on export; the
    // Conductor starts with it (exact) and keeps the later changes.
    let doc = open_v02("imported_project_tempo");
    assert_eq!(
        doc.conductor().start_tempo(),
        TempoValue::Bpm(Bpm::parse("123.456").unwrap())
    );
    assert_eq!(
        tempo_micros(&doc),
        vec![(0, 486_003), (960, 600_000), (1920, 250_000)]
    );
}

#[test]
fn a_damaged_v02_file_is_refused_without_a_partial_project() {
    let mut bytes = fixture("imported.lup");
    let n = bytes.len();
    bytes[n - 10] ^= 0x55;
    assert!(project_from_bytes(&bytes).is_err());
    bytes.truncate(n / 2);
    assert!(project_from_bytes(&bytes).is_err());
}
