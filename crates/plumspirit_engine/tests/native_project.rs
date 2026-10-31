//! `.lup` native project tests: complete round trips, corruption, versions,
//! unknown sections, large note counts, failed writes, overwrite protection and
//! MIDI → .lup → MIDI.

mod common;

use common::*;
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::midi::{self, import_bytes};
use plumspirit_engine::model::{
    Bpm, OnionConfig, OnionMode, PlumDocument, SoundFontRef, TrackId, project::UnknownSection,
};
use plumspirit_engine::native::{
    LUP_MAGIC, LupControl, LupError, load_project, project_from_bytes, project_to_bytes,
    save_project,
};
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plumspirit-lup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A project exercising every stored field.
fn rich_project() -> PlumDocument {
    let tr0 = {
        let mut t = TrackBytes::new();
        t.meta(0, 0x03, b"Conductor \xE9") // Latin-1 name bytes, kept verbatim
            .meta(0, 0x51, &[0x07, 0xA1, 0x20])
            .meta(0, 0x58, &[3, 2, 24, 8])
            .meta(1920, 0x51, &[0x03, 0xD0, 0x90])
            .meta(2880, 0x58, &[7, 3, 24, 8])
            .meta(3000, 0x06, b"Drop");
        t.eot(9000)
    };
    let tr1 = {
        let mut t = TrackBytes::new();
        t.ev(0, &[0xB0, 0x00, 0x01]) // bank
            .ev(0, &[0xC0, 0x05]) // program
            .ev(0, &[0xB0, 0x07, 0x64]) // volume
            .on(0, 0, 60, 100)
            .ev(10, &[0xE0, 0x00, 0x50]) // pitch bend
            .off(480, 0, 60, 64)
            .on(480, 9, 36, 127)
            .ev(480, &[0xB9, 0x40, 0x7F]) // sustain
            .ev(500, &[0xD0, 0x33]) // channel pressure
            .sysex(600, &[0x7E, 0x7F, 0x09, 0x01, 0xF7])
            .ev(700, &[0x90, 70, 1]) // never released → preserved raw
            .ev(720, &[0x90, 71, 90]); // running status off (explicit)
        t.ev(900, &[0x80, 71, 0]);
        t.ev(960, &[0x99, 36, 0]); // Note On velocity 0 termination
        t.eot(960)
    };
    let mut bytes = smf(1, 480, &[tr0, tr1]);
    // A vendor chunk between the header and the first track.
    let mut withx = bytes[..14].to_vec();
    withx.extend(b"XFIH");
    withx.extend(3u32.to_be_bytes());
    withx.extend([1, 2, 3]);
    withx.extend(&bytes[14..]);
    bytes = withx;
    let (mut doc, _) = import_bytes(&bytes).expect("import");
    let p = doc.project_mut();
    p.name = "Rich Project ✓".into();
    p.soundfont = Some(SoundFontRef {
        path: "C:\\SoundFonts\\GeneralUser GS.sf2".into(),
    });
    p.onion = OnionConfig {
        mode: OnionMode::Specific,
        specific: vec![TrackId(1), TrackId(77)],
        opacity: 42,
        ..Default::default()
    };
    doc
}

#[test]
fn roundtrip_preserves_every_field() {
    let doc = rich_project();
    let bytes = project_to_bytes(&doc).unwrap();
    assert_eq!(&bytes[..8], &LUP_MAGIC);
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert_eq!(back.state_fingerprint(), doc.state_fingerprint());
    assert_eq!(back.fingerprint(), doc.fingerprint());
    assert_eq!(back.project(), doc.project());
    assert_eq!(back.track_count(), 2);
    assert_eq!(back.track(0).unwrap().name(), doc.track(0).unwrap().name());
    assert_eq!(back.extra_chunks(), doc.extra_chunks());
    assert_eq!(back.source_format(), doc.source_format());
    assert_eq!(back.division(), doc.division());
    assert!(back.conductor().same_content(doc.conductor()));
    assert_eq!(back.conductor().next_id(), doc.conductor().next_id());
    assert_eq!(report.version, plumspirit_engine::native::LUP_VERSION);
    assert_eq!(report.migrated_from, None);
    assert!(report.warnings.is_empty());
    for t in back.tracks() {
        t.notes.validate().unwrap();
    }
    // Re-saving is byte-identical (deterministic writer).
    assert_eq!(project_to_bytes(&back).unwrap(), bytes);
}

#[test]
fn new_project_with_extreme_values_roundtrips() {
    for (bpm, ppqn) in [
        ("120", 960u32),
        ("0.000001", 1),
        ("1e12", 2_147_483_647),
        ("123456789.123456789", 100_000),
        ("0.5", 3),
    ] {
        let mut doc =
            PlumDocument::new_project(format!("BPM {bpm}"), Bpm::parse(bpm).unwrap(), ppqn);
        doc.add_track().unwrap();
        doc.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(0, 1, 0), rec(u64::MAX - 5, 5, 127)], false)
            .unwrap();
        let (back, _) = project_from_bytes(&project_to_bytes(&doc).unwrap()).unwrap();
        assert_eq!(back.state_fingerprint(), doc.state_fingerprint(), "{bpm}");
        assert_eq!(
            back.conductor().start_tempo(),
            plumspirit_engine::model::TempoValue::Bpm(Bpm::parse(bpm).unwrap())
        );
        assert_eq!(back.division().musical_ppqn(), Some(u64::from(ppqn)));
        // Empty project: one time signature event, no notes.
        let empty = PlumDocument::new_project("E".into(), Bpm::DEFAULT, ppqn);
        let (b2, _) = project_from_bytes(&project_to_bytes(&empty).unwrap()).unwrap();
        assert_eq!(b2.state_fingerprint(), empty.state_fingerprint());
        assert_eq!(b2.note_count(), 0);
    }
}

#[test]
fn deleted_track_ids_are_not_reused_after_reload() {
    let mut doc = PlumDocument::new_project("x".into(), Bpm::DEFAULT, 960);
    doc.add_track().unwrap();
    doc.add_track().unwrap();
    doc.delete_track(2).unwrap();
    let (mut back, _) = project_from_bytes(&project_to_bytes(&doc).unwrap()).unwrap();
    let i = back.add_track().unwrap();
    assert_eq!(back.track(i).unwrap().id(), TrackId(3));
}

#[test]
fn corruption_is_detected() {
    let doc = rich_project();
    let good = project_to_bytes(&doc).unwrap();
    // Flip one bit at a spread of offsets: every change must be rejected (never
    // accepted as a different project, never a panic).
    let step = (good.len() / 97).max(1);
    for i in (0..good.len()).step_by(step) {
        let mut bad = good.clone();
        bad[i] ^= 0x10;
        match project_from_bytes(&bad) {
            Err(_) => {}
            Ok((d, _)) => panic!("bit flip at {i} accepted (fp {:x})", d.state_fingerprint()),
        }
    }
    // Truncation at every length is reported, not a panic.
    for len in 0..good.len() {
        assert!(
            project_from_bytes(&good[..len]).is_err(),
            "truncated to {len}"
        );
    }
    assert!(matches!(
        project_from_bytes(&good[..good.len() - 1]),
        Err(LupError::Truncated { .. })
    ));
    // Trailing data is rejected.
    let mut long = good.clone();
    long.push(0);
    assert!(matches!(
        project_from_bytes(&long),
        Err(LupError::Malformed { .. })
    ));
    // A MIDI file or random bytes are not projects.
    assert_eq!(
        project_from_bytes(b"MThd\0\0\0\x06").unwrap_err(),
        LupError::NotAProject
    );
    assert_eq!(project_from_bytes(b"").unwrap_err(), LupError::NotAProject);
}

/// Rewrites the header CRC after editing header fields.
fn fix_header_crc(b: &mut [u8]) {
    let crc = plumspirit_engine::codec::Crc32::of(&b[..28]);
    b[28..32].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn versions_are_distinguished() {
    let good = project_to_bytes(&rich_project()).unwrap();
    let mut newer = good.clone();
    let next = plumspirit_engine::native::lup::LUP_VERSION_CLIPS + 1;
    newer[8..10].copy_from_slice(&next.to_le_bytes());
    fix_header_crc(&mut newer);
    assert_eq!(
        project_from_bytes(&newer).unwrap_err(),
        LupError::NewerVersion(next)
    );
    assert!(
        project_from_bytes(&newer)
            .unwrap_err()
            .user_message()
            .contains("Unsupported .lup version")
    );
    let mut zero = good.clone();
    zero[8..10].copy_from_slice(&0u16.to_le_bytes());
    fix_header_crc(&mut zero);
    assert!(matches!(
        project_from_bytes(&zero),
        Err(LupError::Malformed { .. })
    ));
}

/// Appends a raw section and bumps the header's section count.
fn with_section(base: &[u8], tag: &[u8; 4], flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = base.to_vec();
    let count = u32::from_le_bytes([b[16], b[17], b[18], b[19]]) + 1;
    b[16..20].copy_from_slice(&count.to_le_bytes());
    fix_header_crc(&mut b);
    b.extend(tag);
    b.extend(flags.to_le_bytes());
    b.extend((payload.len() as u64).to_le_bytes());
    b.extend(payload);
    b.extend(checksum(tag, flags, payload).to_le_bytes());
    b
}

/// Section checksum as specified in docs/LUP_FORMAT.md.
fn checksum(tag: &[u8; 4], flags: u32, payload: &[u8]) -> u32 {
    use plumspirit_engine::codec::Crc32;
    let mut c = Crc32::new();
    c.update(tag);
    c.update(&flags.to_le_bytes());
    c.update(&(payload.len() as u64).to_le_bytes());
    c.update(&Crc32::of(payload).to_le_bytes());
    c.finish()
}

#[test]
fn unknown_sections_optional_kept_mandatory_rejected() {
    let doc = rich_project();
    let base = project_to_bytes(&doc).unwrap();
    let opt = with_section(&base, b"FUTR", 0, b"future data");
    let (back, report) = project_from_bytes(&opt).unwrap();
    assert_eq!(back.fingerprint(), doc.fingerprint());
    assert_eq!(
        back.project().unknown_sections,
        vec![UnknownSection {
            tag: *b"FUTR",
            flags: 0,
            data: b"future data".to_vec()
        }]
    );
    assert_eq!(report.warnings.len(), 1);
    // Saved again, the section is still there, unchanged.
    let again = project_to_bytes(&back).unwrap();
    let (back2, _) = project_from_bytes(&again).unwrap();
    assert_eq!(
        back2.project().unknown_sections,
        back.project().unknown_sections
    );
    // A mandatory unknown section refuses to load.
    let man = with_section(&base, b"MUST", 1, b"x");
    assert_eq!(
        project_from_bytes(&man).unwrap_err(),
        LupError::UnsupportedSection("MUST".into())
    );
}

/// Keeps only the sections whose tags are listed (and fixes the header).
fn keep_sections(full: &[u8], tags: &[&[u8; 4]]) -> Vec<u8> {
    filter_sections(full, |t| tags.iter().any(|x| t == *x))
}

fn filter_sections(full: &[u8], keep: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut out = full[..32].to_vec();
    let mut pos = 32usize;
    let mut kept = 0u32;
    while pos < full.len() {
        let len = u64::from_le_bytes(full[pos + 8..pos + 16].try_into().unwrap()) as usize;
        let end = pos + 16 + len + 4;
        if keep(&full[pos..pos + 4]) {
            out.extend(&full[pos..end]);
            kept += 1;
        }
        pos = end;
    }
    out[16..20].copy_from_slice(&kept.to_le_bytes());
    fix_header_crc(&mut out);
    out
}

#[test]
fn a_version_2_project_without_a_conductor_is_refused() {
    let full = project_to_bytes(&rich_project()).unwrap();
    let out = filter_sections(&full, |t| t != b"COND");
    assert!(out.len() < full.len());
    let err = project_from_bytes(&out).unwrap_err();
    assert!(
        matches!(&err, LupError::Malformed { detail, .. } if detail.contains("Conductor")),
        "{err:?}"
    );
}

#[test]
fn missing_optional_sections_load_with_defaults() {
    // Build a file with only META, COND and TRAK by dropping ONIO and SFNT.
    let mut doc = PlumDocument::new_project("Min".into(), Bpm::DEFAULT, 96);
    doc.project_mut().onion.mode = OnionMode::AllOthers;
    let full = project_to_bytes(&doc).unwrap();
    let out = keep_sections(&full, &[b"META", b"COND", b"TRAK"]);
    let (back, _) = project_from_bytes(&out).unwrap();
    assert_eq!(back.fingerprint(), doc.fingerprint());
    assert_eq!(back.project().onion, OnionConfig::default());
    assert_eq!(back.project().soundfont, None);
}

#[test]
fn hostile_counts_do_not_allocate() {
    // A track declaring 2^60 notes in a tiny section must be refused up front.
    let doc = PlumDocument::new_project("h".into(), Bpm::DEFAULT, 96);
    let good = project_to_bytes(&doc).unwrap();
    // Locate the TRAK payload and replace it.
    let mut pos = 32usize;
    let mut out = good[..32].to_vec();
    while pos < good.len() {
        let len = u64::from_le_bytes(good[pos + 8..pos + 16].try_into().unwrap()) as usize;
        let end = pos + 16 + len + 4;
        if &good[pos..pos + 4] == b"TRAK" {
            let mut payload = vec![0u8]; // id 0
            plumspirit_engine::codec::put_uvarint(&mut payload, 1 << 60);
            payload.extend([0, 0, 0, 0, 0, 0]);
            out.extend(b"TRAK");
            out.extend(1u32.to_le_bytes());
            out.extend((payload.len() as u64).to_le_bytes());
            out.extend(&payload);
            out.extend(checksum(b"TRAK", 1, &payload).to_le_bytes());
        } else {
            out.extend(&good[pos..end]);
        }
        pos = end;
    }
    let err = project_from_bytes(&out).unwrap_err();
    assert!(matches!(err, LupError::Malformed { .. }), "{err:?}");
}

#[test]
fn large_projects_roundtrip() {
    for n in [100_000usize, 1_000_000] {
        let mut doc = generate(FixtureShape::Dense, n, 960, 11);
        doc.project_mut().name = format!("{n} notes");
        let started = std::time::Instant::now();
        let bytes = project_to_bytes(&doc).unwrap();
        let write = started.elapsed();
        let started = std::time::Instant::now();
        let (back, _) = project_from_bytes(&bytes).unwrap();
        let read = started.elapsed();
        assert_eq!(back.state_fingerprint(), doc.state_fingerprint());
        let bpn = bytes.len() as f64 / n as f64;
        println!(
            "{n} notes: {} bytes ({bpn:.2} B/note), write {write:?}, read {read:?}",
            bytes.len()
        );
        assert!(bpn < 10.0, "{bpn} bytes per note");
        for t in back.tracks() {
            t.notes.validate().unwrap();
        }
    }
}

#[test]
fn save_is_transactional() {
    let dir = temp_dir("save");
    let dest = dir.join("song.lup");
    let doc = rich_project();
    // First save, then repeated saves of changed content.
    let r = save_project(&doc, &dest, LupControl::default()).unwrap();
    assert_eq!(r.write.bytes, std::fs::metadata(&dest).unwrap().len());
    let mut doc2 = doc.clone();
    for i in 0..5u64 {
        doc2.track_mut(1)
            .unwrap()
            .notes
            .insert(rec(i * 7, 3, 50), false)
            .unwrap();
        save_project(&doc2, &dest, LupControl::default()).unwrap();
        let (back, _) = load_project(&dest, LupControl::default()).unwrap();
        assert_eq!(back.state_fingerprint(), doc2.state_fingerprint());
    }
    // No temporary files are left behind.
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");

    // Failed write: destination folder does not exist → error, nothing created.
    let missing = dir.join("no-such-folder").join("x.lup");
    let err = save_project(&doc, &missing, LupControl::default()).unwrap_err();
    assert!(matches!(err, LupError::Io { .. }), "{err:?}");
    assert!(!missing.exists());
    // Destination is a folder → refused, folder untouched.
    let folder = dir.join("folder.lup");
    std::fs::create_dir(&folder).unwrap();
    assert!(save_project(&doc, &folder, LupControl::default()).is_err());
    assert!(folder.is_dir());

    // Cancelled save leaves the previous file byte-for-byte intact.
    let before = std::fs::read(&dest).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let big = generate(FixtureShape::Dense, 200_000, 960, 3);
    let err = save_project(
        &big,
        &dest,
        LupControl {
            cancel: Some(&cancel),
            progress_notes: None,
        },
    )
    .unwrap_err();
    assert_eq!(err, LupError::Cancelled);
    assert_eq!(std::fs::read(&dest).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn midi_to_lup_to_midi_keeps_everything() {
    // Import MIDI → edit → save .lup → reopen → export MIDI: all non-note events
    // survive and the notes are the edited ones.
    let doc = rich_project();
    let original_midi = midi::export_to_bytes(&doc).unwrap();
    let before = midi::semantic::semantic_from_bytes(&original_midi).unwrap();
    let mut edited = doc.clone();
    edited
        .track_mut(1)
        .unwrap()
        .notes
        .insert(rec(100, 50, 64), false)
        .unwrap();
    let (reopened, _) = project_from_bytes(&project_to_bytes(&edited).unwrap()).unwrap();
    let out = midi::export_to_bytes(&reopened).unwrap();
    let after = midi::semantic::semantic_from_bytes(&out).unwrap();
    assert_eq!(after.format, before.format);
    assert_eq!(after.division, before.division);
    assert_eq!(after.extra_chunks, before.extra_chunks);
    for (a, b) in after.tracks.iter().zip(&before.tracks) {
        assert_eq!(a.other, b.other, "non-note events must be identical");
        assert_eq!(a.end_of_track, b.end_of_track);
    }
    assert_eq!(after.total_notes(), before.total_notes() + 1);
    let (reimported, _) = import_bytes(&out).unwrap();
    assert_eq!(reimported.fingerprint(), reopened.fingerprint());
}

/// v0.9 (V09-0118): the Track List's view (`TLST`) round-trips; a project
/// without one saves byte for byte as before; hostile sections are refused
/// with a sentence; a newer layout is skipped (the default view is shown).
#[test]
fn the_track_list_view_round_trips_and_hostile_sections_are_refused() {
    use plumspirit_engine::codec::put_uvarint;
    use plumspirit_engine::model::project::TrackListView;
    let mut doc = rich_project();
    let base = project_to_bytes(&doc).unwrap();
    assert!(!base.windows(4).any(|w| w == b"TLST"));
    let v = TrackListView::new(123_456, 0.0625, 57.5);
    assert!(v.is_valid());
    doc.project_mut().track_list = Some(v);
    let bytes = project_to_bytes(&doc).unwrap();
    assert!(bytes.windows(4).any(|w| w == b"TLST"));
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert_eq!(back.project().track_list, Some(v));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(back.fingerprint(), doc.fingerprint());
    assert_eq!(
        project_to_bytes(&back).unwrap(),
        bytes,
        "saved again unchanged"
    );
    doc.project_mut().track_list = None;
    assert_eq!(project_to_bytes(&doc).unwrap(), base, "no view: as before");
    // Hand-made sections on top of the file without a view.
    let payload = |vals: &[u64]| {
        let mut p = Vec::new();
        for &x in vals {
            put_uvarint(&mut p, x);
        }
        p
    };
    let good = payload(&[1, 960, 0.125f64.to_bits(), u64::from(40.0f32.to_bits())]);
    let (b, _) = project_from_bytes(&with_section(&base, b"TLST", 0, &good)).unwrap();
    assert_eq!(
        b.project().track_list,
        Some(TrackListView::new(960, 0.125, 40.0))
    );
    // Twice: refused.
    let twice = with_section(&with_section(&base, b"TLST", 0, &good), b"TLST", 0, &good);
    let e = project_from_bytes(&twice).unwrap_err();
    assert!(matches!(e, LupError::Malformed { .. }), "{e:?}");
    assert!(
        e.user_message().contains("Track List"),
        "{}",
        e.user_message()
    );
    // Invalid values: refused.
    for bad in [
        payload(&[1, 0, f64::NAN.to_bits(), u64::from(34.0f32.to_bits())]),
        payload(&[1, 0, 0.0f64.to_bits(), u64::from(34.0f32.to_bits())]),
        payload(&[1, 0, (-0.5f64).to_bits(), u64::from(34.0f32.to_bits())]),
        payload(&[1, 0, 1000.0f64.to_bits(), u64::from(34.0f32.to_bits())]),
        payload(&[1, 0, 0.125f64.to_bits(), u64::from(10.0f32.to_bits())]),
        payload(&[1, 0, 0.125f64.to_bits(), u64::from(2000.0f32.to_bits())]),
        payload(&[1, 0, 0.125f64.to_bits(), u64::from(f32::NAN.to_bits())]),
        payload(&[1, 0, 0.125f64.to_bits(), 1 << 40]),
        payload(&[1, 0, 0.125f64.to_bits()]),
        {
            let mut p = good.clone();
            p.push(7);
            p
        },
    ] {
        let e = project_from_bytes(&with_section(&base, b"TLST", 0, &bad)).unwrap_err();
        assert!(matches!(e, LupError::Malformed { .. }), "{bad:?}: {e:?}");
    }
    // A newer layout (version 2, anything after it): skipped, default view.
    let newer = payload(&[2, 1, 2, 3, 4, 5, 6]);
    let (b, report) = project_from_bytes(&with_section(&base, b"TLST", 0, &newer)).unwrap();
    assert_eq!(b.project().track_list, None);
    assert!(b.project().unknown_sections.is_empty());
    assert!(report.warnings.is_empty());
    // A bad value can never be saved silently: the verifier compares it.
    let mut d = rich_project();
    d.project_mut().track_list = Some(TrackListView::new(0, f64::NAN, 34.0));
    assert!(project_from_bytes(&project_to_bytes(&d).unwrap()).is_err());
}

/// B1-0064: Per-Track Onion settings travel in their own optional section
/// (ONPT): saved, read back, verified by the transactional save; a project
/// that never used them has no such section (saved as before); a file
/// without it reads as off.
#[test]
fn per_track_onion_settings_roundtrip_in_an_optional_section() {
    use plumspirit_engine::model::project::OnionTrackEntry;
    let plain = rich_project();
    let plain_bytes = project_to_bytes(&plain).unwrap();
    assert!(!plain_bytes.windows(4).any(|w| w == b"ONPT"));
    let mut doc = rich_project();
    {
        let o = &mut doc.project_mut().onion;
        o.per_track = true;
        o.track_entries = vec![
            OnionTrackEntry {
                track: TrackId(0),
                mode: OnionMode::PreviousAndNext,
                specific: vec![],
            },
            OnionTrackEntry {
                track: TrackId(1),
                mode: OnionMode::Specific,
                specific: vec![TrackId(0), TrackId(77)],
            },
        ];
    }
    let bytes = project_to_bytes(&doc).unwrap();
    assert!(bytes.windows(4).any(|w| w == b"ONPT"));
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert_eq!(back.project().onion, doc.project().onion);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(project_to_bytes(&back).unwrap(), bytes, "deterministic");
    // The transactional save reads the file back and checks the section.
    let dir = temp_dir("onpt");
    let path = dir.join("per_track.lup");
    save_project(&doc, &path, LupControl::default()).unwrap();
    let (loaded, _) = load_project(&path, LupControl::default()).unwrap();
    assert_eq!(loaded.project().onion, doc.project().onion);
    // Without the section (an older writer): per-track off, nothing kept.
    let without = filter_sections(&bytes, |t| t != b"ONPT");
    let (old, _) = project_from_bytes(&without).unwrap();
    assert!(!old.project().onion.per_track);
    assert!(old.project().onion.track_entries.is_empty());
    assert_eq!(old.project().onion.specific, doc.project().onion.specific);
}

/// B1-0035: Lua plugins' project data round-trips through `.lup` (`PLUG`),
/// the post-save verifier checks it, and damaged data refuses the file.
#[test]
fn plugin_project_data_roundtrips_and_is_verified() {
    let mut doc = rich_project();
    doc.project_mut().plugin_data = vec![
        (
            "com.example.a".into(),
            "{\"version\":1,\"values\":{\"x\":3}}".into(),
        ),
        ("local.tool".into(), "{\"version\":2,\"values\":{}}".into()),
    ];
    let bytes = project_to_bytes(&doc).unwrap();
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert!(report.warnings.is_empty());
    assert_eq!(back.project().plugin_data, doc.project().plugin_data);
    assert_eq!(project_to_bytes(&back).unwrap(), bytes, "deterministic");
    // A file saved to disk passes the verifier.
    let dir = temp_dir("plugin-data");
    let dest = dir.join("p.lup");
    save_project(&doc, &dest, LupControl::default()).unwrap();
    let (loaded, _) = load_project(&dest, LupControl::default()).unwrap();
    assert_eq!(loaded.project().plugin_data, doc.project().plugin_data);
    // Without plugin data no section is written (older files stay identical).
    let plain = rich_project();
    let plain_bytes = project_to_bytes(&plain).unwrap();
    assert!(!plain_bytes.windows(4).any(|w| w == b"PLUG"));
    // Unsorted / damaged ids refuse the file.
    let mut bad = Vec::new();
    // layout 1, two entries out of order.
    bad.push(1u8);
    bad.push(2u8);
    for (id, data) in [("zz.plugin", "{}"), ("aa.plugin", "{}")] {
        bad.push(id.len() as u8);
        bad.extend_from_slice(id.as_bytes());
        bad.push(data.len() as u8);
        bad.extend_from_slice(data.as_bytes());
    }
    let damaged = with_section(&plain_bytes, b"PLUG", 0, &bad);
    assert!(project_from_bytes(&damaged).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
