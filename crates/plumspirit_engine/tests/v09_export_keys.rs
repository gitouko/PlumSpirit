//! v0.9 Standard MIDI boundary (contract §061, §243; slice V09-S22 step 1):
//! a 256-Key Mode project never writes a key above 127 into a Standard MIDI
//! file silently — the export refuses (default) or applies the policy the user
//! chose (Discard / Fold / Clamp); a standard project exports exactly as
//! before.
//!
//! Step 2 (§062, §244): the experimental non-standard 256-key file — keys
//! above 127 written Kiva's way (unmasked key byte after an explicit Note On /
//! Off status), read back by PlumSpirit only with the 256-key import, refused
//! by name by the normal import, and checked against an emulation of Kiva's
//! track parser (docs/references/v0_9/KIVA.md).

mod common;

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::{
    ExportError, ExportOptions, ImportWarning, MidiErrorKind, WideKeys, export_to_bytes,
    export_to_bytes_with, import_bytes, import_bytes_with, plan_export_with,
};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::track::TrackProps;
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};

fn rec(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord::new(
        start,
        240,
        NoteAttrs::new_wide(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Track 1: keys 60 / 127 / 128 / 200 / 255; track 2: key 100 with a key
/// correction of +100 (sounds — and exports — at 200).
fn wide_doc() -> PlumDocument {
    let mut d = PlumDocument::new_project("wide".into(), Bpm::DEFAULT, 480);
    d.project_mut().keys = KeyRange::Extended256;
    d.add_track().unwrap();
    let notes: Vec<NoteRecord> = [60u8, 127, 128, 200, 255]
        .iter()
        .enumerate()
        .map(|(i, &p)| rec(i as u64 * 480, p))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&notes, false)
        .unwrap();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 100), rec(480, 20)], false)
        .unwrap();
    let props = TrackProps {
        key_correction: 100,
        ..TrackProps::default()
    };
    let track = d.track(1).unwrap().id();
    History::default()
        .apply(
            &mut d,
            Operation::TrackProps {
                track,
                props,
                end: None,
                label: "Track Properties",
            },
        )
        .unwrap();
    d
}

fn options(wide_keys: WideKeys) -> ExportOptions {
    ExportOptions {
        wide_keys,
        ..ExportOptions::default()
    }
}

/// Every exported key, sorted (all tracks).
fn keys_of(bytes: &[u8]) -> Vec<u8> {
    let (doc, _) = import_bytes(bytes).expect("the file is valid Standard MIDI");
    assert_eq!(doc.project().keys, KeyRange::Standard);
    let mut v: Vec<u8> = doc
        .tracks()
        .iter()
        .flat_map(|t| t.notes.iter().map(|n| n.attrs.pitch()).collect::<Vec<_>>())
        .collect();
    v.sort_unstable();
    v
}

/// No Note On / Off in the file carries a data byte above 0x7F.
fn no_wide_note_bytes(bytes: &[u8]) {
    // Every channel event the importer accepted is 7-bit; a data byte ≥ 0x80
    // would have been read as a status byte and broken the import.
    let (doc, summary) = import_bytes(bytes).unwrap();
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
    assert!(
        doc.tracks()
            .iter()
            .all(|t| t.notes.iter().all(|n| n.attrs.pitch() <= 127))
    );
}

#[test]
fn rejecting_is_the_default_and_names_the_notes() {
    let d = wide_doc();
    let plan = plan_export_with(&d, ExportOptions::default());
    assert_eq!(plan.wide.policy, WideKeys::Reject);
    // 128, 200, 255 on track 1, 100 + 100 = 200 on track 2.
    assert_eq!((plan.wide.notes, plan.wide.highest), (4, 255));
    let b = &plan.blockers;
    assert_eq!(b.len(), 1, "{b:?}");
    assert!(
        b[0].contains("4 note(s) are above key 127 (the highest is 255)")
            && b[0].contains("nothing was written")
            && b[0].contains("Discard Out-of-range Notes"),
        "{}",
        b[0]
    );
    match export_to_bytes(&d) {
        Err(ExportError::Blocked(m)) => assert_eq!(&m, &b[0]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn discard_fold_and_clamp_write_valid_standard_files() {
    let d = wide_doc();
    // Track 2's 20 + 100 = 120 stays; 100 + 100 = 200 is wide.
    let cases = [
        (WideKeys::Discard, vec![60, 120, 127]),
        // 128 → 116, 200 → 116, 255 → 123, (100 + 100) 200 → 116.
        (WideKeys::Fold, vec![60, 116, 116, 116, 120, 123, 127]),
        (WideKeys::Clamp, vec![60, 120, 127, 127, 127, 127, 127]),
    ];
    for (policy, want) in cases {
        let plan = plan_export_with(&d, options(policy));
        assert!(plan.blockers.is_empty(), "{policy:?}: {:?}", plan.blockers);
        assert_eq!((plan.wide.notes, plan.wide.highest), (4, 255));
        let bytes = export_to_bytes_with(&d, &plan).unwrap();
        no_wide_note_bytes(&bytes);
        assert_eq!(keys_of(&bytes), want, "{policy:?}");
    }
}

#[test]
fn the_fold_keeps_the_pitch_class() {
    for p in 128u8..=255 {
        let f = WideKeys::Fold.map(p).unwrap();
        assert!((116..=127).contains(&f), "{p} → {f}");
        assert_eq!(f % 12, p % 12);
    }
    for p in 0u8..=127 {
        for w in WideKeys::ALL {
            assert_eq!(w.map(p), Some(p));
        }
    }
    assert_eq!(WideKeys::Clamp.map(200), Some(127));
    assert_eq!(WideKeys::Discard.map(200), None);
}

#[test]
fn standard_projects_and_256_key_projects_without_wide_notes_export_as_before() {
    // A standard project: no count, no blocker, the same bytes for every
    // policy.
    let mut d = PlumDocument::new_project("std".into(), Bpm::DEFAULT, 480);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 0), rec(480, 64), rec(960, 127)], false)
        .unwrap();
    let base = export_to_bytes(&d).unwrap();
    for w in WideKeys::ALL {
        let plan = plan_export_with(&d, options(w));
        assert_eq!(plan.wide.notes, 0);
        assert_eq!(export_to_bytes_with(&d, &plan).unwrap(), base);
    }
    // The same notes in 256-Key Mode: nothing above 127, the same file.
    d.project_mut().keys = KeyRange::Extended256;
    assert_eq!(export_to_bytes(&d).unwrap(), base);
}

/// The full export (temporary file → re-read → validate → rename) accepts a
/// policy's result and checks it note for note; a rejected export leaves an
/// existing file untouched.
#[test]
fn exports_to_a_path_validate_the_policys_result() {
    use plumspirit_engine::midi::{ExportControl, export_to_path};
    let dir = std::env::temp_dir().join(format!("plumspirit-v09export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("wide.mid");
    std::fs::write(&dest, b"old").unwrap();
    let d = wide_doc();
    let rejected = plan_export_with(&d, ExportOptions::default());
    assert!(matches!(
        export_to_path(&d, &rejected, &dest, ExportControl::default()),
        Err(ExportError::Blocked(_))
    ));
    assert_eq!(std::fs::read(&dest).unwrap(), b"old");
    for policy in [WideKeys::Discard, WideKeys::Fold, WideKeys::Clamp] {
        let plan = plan_export_with(&d, options(policy));
        let report = export_to_path(&d, &plan, &dest, ExportControl::default())
            .unwrap_or_else(|e| panic!("{policy:?}: {e:?}"));
        assert_eq!(report.path, dest);
        no_wide_note_bytes(&std::fs::read(&dest).unwrap());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Step 2: the experimental non-standard 256-key file (§062, §244)
// ---------------------------------------------------------------------------

/// A note as both readers see it: (start, end, key, channel, velocity).
type Seen = (u64, u64, u8, u8, u8);

fn rec_full(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8, off: NoteOffKind) -> NoteRecord {
    let off_vel = if off == NoteOffKind::NoteOff { 64 } else { 0 };
    NoteRecord::new(
        start,
        dur,
        NoteAttrs::new_wide(pitch, vel, ch, off_vel, off).unwrap(),
    )
}

/// Keys 0 / 127 / 128 / 200 / 255 one after another; a chord at tick 2400 on
/// one channel mixing standard and wide keys (running status around the wide
/// events); velocity 1; Note-On-velocity-0 endings for wide keys; a second
/// track on another channel. 9 notes above 127 (the highest 255).
fn experimental_doc() -> PlumDocument {
    let mut d = PlumDocument::new_project("exp".into(), Bpm::DEFAULT, 480);
    d.project_mut().keys = KeyRange::Extended256;
    d.add_track().unwrap();
    let mut a: Vec<NoteRecord> = [0u8, 127, 128, 200, 255]
        .iter()
        .enumerate()
        .map(|(i, &p)| rec_full(i as u64 * 480, 240, p, 100, 0, NoteOffKind::NoteOff))
        .collect();
    for (j, &p) in [60u8, 130, 61, 250, 62].iter().enumerate() {
        let off = if j % 2 == 0 {
            NoteOffKind::NoteOff
        } else {
            NoteOffKind::NoteOnZero
        };
        a.push(rec_full(
            2400,
            120 + j as u64 * 10,
            p,
            1 + j as u8 * 30,
            0,
            off,
        ));
    }
    a.push(rec_full(3000, 60, 129, 1, 0, NoteOffKind::NoteOnZero));
    // Running status between standard keys, interrupted by a wide one.
    for (j, &p) in [70u8, 71, 140, 72, 73].iter().enumerate() {
        a.push(rec_full(
            3600,
            200 + j as u64,
            p,
            80,
            0,
            NoteOffKind::NoteOff,
        ));
    }
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&a, false)
        .unwrap();
    let b = [
        rec_full(0, 480, 64, 90, 5, NoteOffKind::NoteOff),
        rec_full(240, 480, 192, 90, 5, NoteOffKind::NoteOff),
        rec_full(240, 120, 191, 90, 9, NoteOffKind::NoteOnZero),
    ];
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&b, false)
        .unwrap();
    d
}

/// Every note of the document (all tracks), sorted.
fn seen_of(doc: &PlumDocument) -> Vec<Seen> {
    let mut v: Vec<Seen> = doc
        .tracks()
        .iter()
        .flat_map(|t| {
            t.notes
                .iter()
                .map(|n| {
                    (
                        n.start_tick,
                        n.end_tick(),
                        n.attrs.pitch(),
                        n.attrs.channel(),
                        n.attrs.velocity(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    v.sort_unstable();
    v
}

/// (start, duration, packed attributes) of every note, sorted: equal lists
/// mean the same keys, velocities, channels and Note Off encodings.
fn exact_of(doc: &PlumDocument) -> Vec<(u64, u64, u32)> {
    let mut v: Vec<(u64, u64, u32)> = doc
        .tracks()
        .iter()
        .flat_map(|t| {
            t.notes
                .iter()
                .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
                .collect::<Vec<_>>()
        })
        .collect();
    v.sort_unstable();
    v
}

fn experimental(d: &PlumDocument) -> Vec<u8> {
    let plan = plan_export_with(d, options(WideKeys::Experimental));
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    export_to_bytes_with(d, &plan).unwrap()
}

/// What Kiva's loader reads (the shared emulation, `tests/common/kiva.rs`,
/// docs/references/v0_9/KIVA.md).
#[derive(Debug, Default)]
struct KivaRead {
    notes: Vec<Seen>,
    /// Tracks whose rest was dropped as "Corrupt Track".
    corrupt: Vec<usize>,
    /// Every Note On / Off in file order: (key, explicit status byte).
    note_events: Vec<(u8, bool)>,
    /// For each event right after a wide note event: whether its status byte
    /// was explicit (`None`: not a channel event).
    after_wide: Vec<Option<bool>>,
}

fn kiva_read(bytes: &[u8]) -> KivaRead {
    let f = common::kiva::kiva_load(bytes);
    assert!(f.error.is_none(), "{:?}", f.error);
    KivaRead {
        notes: f.notes(),
        corrupt: f.corrupt(),
        note_events: f.note_events(),
        after_wide: f.after_wide(),
    }
}

/// A format-1 file at 480 PPQN from raw track bodies (End of Track added).
fn smf(tracks: &[&[u8]]) -> Vec<u8> {
    let mut v = b"MThd".to_vec();
    v.extend_from_slice(&6u32.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
    v.extend_from_slice(&480u16.to_be_bytes());
    for t in tracks {
        let mut body = t.to_vec();
        body.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        v.extend_from_slice(b"MTrk");
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend_from_slice(&body);
    }
    v
}

#[test]
fn the_experimental_file_round_trips_keys_0_127_128_200_255() {
    let d = experimental_doc();
    let plan = plan_export_with(&d, options(WideKeys::Experimental));
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert_eq!((plan.wide.notes, plan.wide.highest), (9, 255));
    assert!(plan.wide.non_standard());
    assert!(plan.needs_approval());
    let disclosed = plan.disclosures();
    assert!(
        disclosed.iter().any(|l| l.starts_with(
            "EXPERIMENTAL / NON-STANDARD FILE: 9 note(s) above key 127 (the highest is 255)"
        ) && l.contains("not a Standard MIDI file")),
        "{disclosed:?}"
    );
    let bytes = export_to_bytes_with(&d, &plan).unwrap();
    let (back, summary) = import_bytes_with(&bytes, KeyRange::Extended256).unwrap();
    // Only the informational line about PlumSpirit's own structure data.
    assert!(
        summary
            .warnings
            .iter()
            .all(|w| matches!(w, ImportWarning::Structure(_))),
        "{:?}",
        summary.warnings
    );
    assert_eq!(back.project().keys, KeyRange::Extended256);
    assert_eq!(summary.keys, KeyRange::Extended256);
    assert_eq!(summary.wide_notes, 9);
    assert!(
        summary
            .describe()
            .contains("non-standard 256-key file (9 note(s) above key 127)")
    );
    // Keys, velocities, channels, lengths and Note Off encodings all survive.
    assert_eq!(exact_of(&back), exact_of(&d));
    // The same file written again from the re-read project is identical.
    assert_eq!(experimental(&back), bytes);
}

#[test]
fn every_wide_note_event_has_its_own_status_byte() {
    let bytes = experimental(&experimental_doc());
    let k = kiva_read(&bytes);
    let wide: Vec<_> = k.note_events.iter().filter(|(key, _)| *key > 127).collect();
    // 9 notes above 127: a Note On and an ending each.
    assert_eq!(wide.len(), 18);
    assert!(wide.iter().all(|(_, explicit)| *explicit), "{wide:?}");
    // Running status is used elsewhere in the file (the standard events of
    // the chord), but never for the event after a wide one.
    assert!(k.note_events.iter().any(|(_, explicit)| !explicit));
    assert!(!k.after_wide.is_empty());
    assert!(
        k.after_wide.iter().all(|e| *e != Some(false)),
        "{:?}",
        k.after_wide
    );
}

#[test]
fn a_kiva_rule_reader_reads_the_same_notes() {
    let d = experimental_doc();
    let k = kiva_read(&experimental(&d));
    assert!(
        k.corrupt.is_empty(),
        "tracks dropped as corrupt: {:?}",
        k.corrupt
    );
    assert_eq!(k.notes, seen_of(&d));
    // With a key correction (+100 on track 2): Kiva reads the keys the file
    // sounds at, which is what PlumSpirit's 256-key import opens.
    let w = wide_doc();
    let bytes = experimental(&w);
    let k = kiva_read(&bytes);
    assert!(k.corrupt.is_empty(), "{:?}", k.corrupt);
    let (back, _) = import_bytes_with(&bytes, KeyRange::Extended256).unwrap();
    assert_eq!(k.notes, seen_of(&back));
    let keys: Vec<u8> = k.notes.iter().map(|n| n.2).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![60, 120, 127, 128, 200, 200, 255]);
}

/// The emulator itself follows the reference rules (KIVA.md "Fixtures
/// derived"), so agreeing with it means something.
#[test]
fn the_kiva_rule_emulator_follows_the_reference_rules() {
    // An unmasked key after an explicit status; velocity 1 is a note.
    let k = kiva_read(&smf(&[&[0x00, 0x90, 0xC8, 0x01, 0x60, 0x80, 0xC8, 0x00]]));
    assert_eq!(k.notes, vec![(0, 96, 200, 0, 1)]);
    // Under running status a byte ≥ 0x80 is a command, not a key: the "note"
    // is misread, which is why a writer must repeat the status.
    let k = kiva_read(&smf(&[&[0x00, 0x90, 0x40, 0x64, 0x00, 0xC8, 0x64]]));
    assert_ne!(k.note_events.last(), Some(&(200, false)));
    // Unknown meta type / wrong length: the rest of the track is dropped.
    // (The note's release is in the dropped part: Kiva ends it where the
    // track stopped.)
    let k = kiva_read(&smf(&[&[
        0x00, 0x90, 0x40, 0x64, 0x10, 0xFF, 0x60, 0x00, 0x10, 0x80, 0x40, 0x00,
    ]]));
    assert_eq!(k.corrupt, vec![0]);
    assert_eq!(k.notes, vec![(0, 16, 0x40, 0, 0x64)]);
    let k = kiva_read(&smf(&[&[0x00, 0xFF, 0x51, 0x02, 0x07, 0xA1]]));
    assert_eq!(k.corrupt, vec![0]);
    // Running status after a meta continues the meta (0xFF), not the notes.
    // (The "release" is read as a meta event of type 0x40 — corrupt — so the
    // note never ends; Kiva ends it where the track stopped.)
    let k = kiva_read(&smf(&[&[
        0x00, 0x90, 0x40, 0x64, 0x00, 0xFF, 0x01, 0x00, 0x00, 0x40, 0x00,
    ]]));
    assert_eq!(k.corrupt, vec![0]);
    assert_eq!(k.notes, vec![(0, 0, 0x40, 0, 0x64)], "{:?}", k.notes);
    // Same-key overlaps close last-in-first-out.
    let k = kiva_read(&smf(&[&[
        0x00, 0x90, 0x40, 0x64, 0x0A, 0x40, 0x65, 0x0A, 0x80, 0x40, 0x00, 0x0A, 0x40, 0x00,
    ]]));
    assert_eq!(
        k.notes,
        vec![(0, 30, 0x40, 0, 0x64), (10, 20, 0x40, 0, 0x65)]
    );
}

#[test]
fn the_standard_import_names_a_256_key_file_instead_of_misreading_it() {
    let bytes = experimental(&experimental_doc());
    let e = import_bytes(&bytes).unwrap_err();
    assert_eq!(e.kind, MidiErrorKind::WideKeyNote(128));
    assert!(e.is_wide_key_file());
    let m = e.user_message();
    assert!(
        m.contains("A note at key 128")
            && m.contains("non-standard 256-key MIDI file")
            && m.contains("256-Key Mode"),
        "{m}"
    );
    // A misplaced status byte (no valid velocity after it) is still corruption.
    let e = import_bytes(&smf(&[&[0x00, 0x90, 0xC8, 0x90, 0x40]])).unwrap_err();
    assert_eq!(e.kind, MidiErrorKind::StatusInData(0xC8));
    // A Kiva-style file from elsewhere opens with the 256-key import.
    let kiva = smf(&[&[0x00, 0x91, 0xFF, 0x7F, 0x83, 0x60, 0x81, 0xFF, 0x00]]);
    assert!(import_bytes(&kiva).unwrap_err().is_wide_key_file());
    let (doc, summary) = import_bytes_with(&kiva, KeyRange::Extended256).unwrap();
    assert_eq!(seen_of(&doc), vec![(0, 480, 255, 1, 127)]);
    assert_eq!(summary.wide_notes, 1);
    // A standard file opened with the 256-key import reads the same notes
    // (into a 256-Key Mode project).
    let std_bytes = export_to_bytes(&{
        let mut d = PlumDocument::new_project("std".into(), Bpm::DEFAULT, 480);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(0, 0), rec(480, 127)], false)
            .unwrap();
        d
    })
    .unwrap();
    let (a, _) = import_bytes(&std_bytes).unwrap();
    let (b, s) = import_bytes_with(&std_bytes, KeyRange::Extended256).unwrap();
    assert_eq!(exact_of(&a), exact_of(&b));
    assert_eq!((b.project().keys, s.wide_notes), (KeyRange::Extended256, 0));
}

#[test]
fn unmatched_wide_events_are_written_only_by_the_experimental_file() {
    // Track 1: a wide Note On never released and a wide Note Off with no
    // note (kept as raw events by the 256-key import), plus a normal note.
    let file = smf(&[&[
        0x00, 0x90, 0x3C, 0x64, // C4 on
        0x00, 0x90, 0xD0, 0x50, // key 208 on, never released
        0x83, 0x60, 0x80, 0x3C, 0x00, // C4 off at 480
        0x00, 0x80, 0xE0, 0x00, // key 224 off without a note
    ]]);
    let (d, summary) = import_bytes_with(&file, KeyRange::Extended256).unwrap();
    assert_eq!(seen_of(&d), vec![(0, 480, 60, 0, 100)]);
    assert_eq!(summary.wide_notes, 0);
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.user_message().contains("never released"))
    );
    let reject = plan_export_with(&d, ExportOptions::default());
    assert_eq!(
        (
            reject.wide.notes,
            reject.wide.raw_events,
            reject.wide.highest
        ),
        (0, 2, 224)
    );
    assert!(
        reject.blockers[0].starts_with(
            "2 unmatched Note On / Off event(s) are above key 127 (the highest is 224)."
        ),
        "{:?}",
        reject.blockers
    );
    for policy in [WideKeys::Discard, WideKeys::Fold, WideKeys::Clamp] {
        let plan = plan_export_with(&d, options(policy));
        assert!(plan.blockers.is_empty());
        let disclosed = plan.disclosures();
        assert!(
            disclosed.iter().any(
                |l| l.contains("2 unmatched Note On / Off event(s) above key 127 are left out")
            ),
            "{policy:?}: {disclosed:?}"
        );
        let bytes = export_to_bytes_with(&d, &plan).unwrap();
        let (back, s) = import_bytes(&bytes).expect("a valid Standard MIDI file");
        assert!(s.warnings.is_empty(), "{policy:?}: {:?}", s.warnings);
        assert_eq!(seen_of(&back), vec![(0, 480, 60, 0, 100)]);
        // The project still holds them.
        assert_eq!(d.tracks()[0].events.len(), 2);
    }
    // The experimental file writes them back unchanged.
    let bytes = experimental(&d);
    let (back, s) = import_bytes_with(&bytes, KeyRange::Extended256).unwrap();
    assert!(
        s.warnings
            .iter()
            .any(|w| w.user_message().contains("never released"))
    );
    assert_eq!(
        back.tracks()[0].events.events(),
        d.tracks()[0].events.events()
    );
    assert_eq!(experimental(&back), bytes);
}

#[test]
fn the_experimental_export_to_a_path_is_validated() {
    use plumspirit_engine::midi::{ExportControl, export_to_path};
    let dir = std::env::temp_dir().join(format!("plumspirit-v09exp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("experimental.mid");
    for d in [experimental_doc(), wide_doc()] {
        let plan = plan_export_with(&d, options(WideKeys::Experimental));
        let report = export_to_path(&d, &plan, &dest, ExportControl::default())
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(report.validation.notes_checked, d.note_count() as u64);
        assert!(!report.validation.pairing_differs());
        assert!(report.plan.wide.non_standard());
        let (back, _) = plumspirit_engine::midi::import_file_with(
            &dest,
            plumspirit_engine::midi::ImportControl::default(),
            KeyRange::Extended256,
        )
        .unwrap();
        assert_eq!(seen_of(&back).len(), d.note_count());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_experimental_choice_without_wide_notes_writes_a_standard_file() {
    // A 256-Key Mode project whose keys all lie in 0–127: no disclosure, no
    // approval, the same bytes as the default export, readable as Standard
    // MIDI.
    let mut d = PlumDocument::new_project("std".into(), Bpm::DEFAULT, 480);
    d.project_mut().keys = KeyRange::Extended256;
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[rec(0, 0), rec(480, 64), rec(960, 127)], false)
        .unwrap();
    let plan = plan_export_with(&d, options(WideKeys::Experimental));
    assert!(!plan.wide.non_standard() && !plan.needs_approval());
    assert!(plan.disclosures().is_empty(), "{:?}", plan.disclosures());
    let bytes = export_to_bytes_with(&d, &plan).unwrap();
    assert_eq!(bytes, export_to_bytes(&d).unwrap());
    no_wide_note_bytes(&bytes);
}

/// The file for the owner's Kiva check (§244: "a fixture must open in the
/// target Kiva build"): the experimental export of [`experimental_doc`] —
/// keys 0 / 127 / 128 / 200 / 255 one after another (channel 1), a chord at
/// bar 2 beat 2 mixing standard and wide keys, key 129 at velocity 1, a chord
/// with key 140 just after bar 2 beat 4, and a second track on channels 6 / 10 with
/// keys 64 / 192 / 191. Regenerate with `PLUMSPIRIT_WRITE_FIXTURES=1`.
#[test]
fn the_kiva_check_fixture_is_the_experimental_export() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/v09/kiva_256_keys_experimental.mid"
    );
    let bytes = experimental(&experimental_doc());
    if std::env::var_os("PLUMSPIRIT_WRITE_FIXTURES").is_some() {
        std::fs::write(path, &bytes).unwrap();
    }
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    let k = kiva_read(&bytes);
    assert!(k.corrupt.is_empty());
    assert_eq!(k.notes.len(), experimental_doc().note_count());
}
