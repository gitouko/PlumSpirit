//! Malformed and hostile input (contract §14, §96, §119). The importer must either
//! preserve safely or reject cleanly, never panic and never allocate absurd sizes.

mod common;

use common::*;
use plumspirit_engine::midi::{
    ImportWarning, MidiErrorKind, export_to_bytes, import_bytes, validate_exported_bytes,
};

fn err_kind(bytes: &[u8]) -> MidiErrorKind {
    import_bytes(bytes).expect_err("should be rejected").kind
}

fn valid_file() -> Vec<u8> {
    let mut t = TrackBytes::new();
    t.meta(0, 0x03, b"T")
        .on(0, 0, 60, 100)
        .off(96, 0, 60, 0)
        .on(96, 1, 62, 90)
        .off(192, 1, 62, 0);
    smf(1, 96, &[t.eot(192)])
}

#[test]
fn truncated_header() {
    assert_eq!(err_kind(b""), MidiErrorKind::NotMidi);
    assert_eq!(err_kind(b"MTh"), MidiErrorKind::NotMidi);
    assert_eq!(
        err_kind(b"RIFF\x00\x00\x00\x06\x00\x01"),
        MidiErrorKind::NotMidi
    );
    assert_eq!(
        err_kind(b"MThd\x00\x00\x00\x06\x00\x01"),
        MidiErrorKind::TruncatedHeader
    );
    assert_eq!(
        err_kind(b"MThd\x00\x00\x00\x02\x00\x01\x00\x01"),
        MidiErrorKind::TruncatedHeader
    );
    assert_eq!(
        err_kind(b"MThd\x00\x00\x00\x06\x00\x07\x00\x01\x00\x60"),
        MidiErrorKind::UnsupportedFormat(7)
    );
    assert_eq!(
        err_kind(b"MThd\x00\x00\x00\x06\x00\x01\x00\x01\x00\x60"),
        MidiErrorKind::NoTracks
    );
}

#[test]
fn truncated_track_reports_byte_offset() {
    let f = valid_file();
    let cut = &f[..f.len() - 5];
    let e = import_bytes(cut).unwrap_err();
    assert!(
        matches!(
            e.kind,
            MidiErrorKind::UnexpectedEof | MidiErrorKind::EventPastChunkEnd
        ),
        "{e:?}"
    );
    assert!(e.offset.is_some());
    assert_eq!(e.track, Some(0));
    assert!(e.user_message().contains("at byte"), "{}", e.user_message());
}

#[test]
fn wrong_track_length_too_short() {
    let mut f = valid_file();
    // Declared length 5 bytes shorter than the data: an event crosses the chunk end.
    let len = u32::from_be_bytes([f[18], f[19], f[20], f[21]]) - 5;
    f[18..22].copy_from_slice(&len.to_be_bytes());
    assert_eq!(err_kind(&f), MidiErrorKind::EventPastChunkEnd);
}

#[test]
fn bogus_huge_chunk_length_does_not_allocate() {
    let mut f = valid_file();
    f[18..22].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
    // The track ends with End of Track before the (impossible) declared end: the
    // importer keeps the data and warns instead of reading 4 GB.
    let (doc, summary) = import_bytes(&f).unwrap();
    assert_eq!(doc.note_count(), 2);
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| matches!(w, ImportWarning::ChunkLengthPastEof { .. }))
    );
    // A huge meta length inside a small file is rejected before allocating.
    let mut t = TrackBytes::new();
    t.ev(0, &[0xFF, 0x01, 0xFF, 0xFF, 0xFF, 0x7F]);
    let f2 = smf(1, 96, &[t.data.clone()]);
    let k = err_kind(&f2);
    assert!(
        matches!(
            k,
            MidiErrorKind::EventPastChunkEnd | MidiErrorKind::UnexpectedEof
        ),
        "{k:?}"
    );
}

#[test]
fn truncated_and_overlong_vlq() {
    let mut t = TrackBytes::new();
    t.data.extend([0x81, 0x80, 0x80]); // VLQ never terminates before chunk end
    assert!(matches!(
        err_kind(&smf(1, 96, &[t.data.clone()])),
        MidiErrorKind::EventPastChunkEnd | MidiErrorKind::UnexpectedEof
    ));
    let mut t = TrackBytes::new();
    t.data.extend([0x81, 0x80, 0x80, 0x80, 0x00, 0x90, 60, 100]); // 5-byte VLQ
    assert_eq!(
        err_kind(&smf(1, 96, &[t.data.clone()])),
        MidiErrorKind::VlqTooLong
    );
}

#[test]
fn malformed_events() {
    // Data byte without any running status.
    let mut t = TrackBytes::new();
    t.ev(0, &[60, 100]);
    assert_eq!(
        err_kind(&smf(1, 96, &[t.eot(0)])),
        MidiErrorKind::DataWithoutStatus(60)
    );
    // Status byte where a data byte is required (missing expected bytes).
    let mut t = TrackBytes::new();
    t.ev(0, &[0x90, 60, 0x80]);
    assert_eq!(
        err_kind(&smf(1, 96, &[t.eot(0)])),
        MidiErrorKind::StatusInData(0x80)
    );
    // Undefined status byte.
    let mut t = TrackBytes::new();
    t.ev(0, &[0xF4]);
    assert_eq!(
        err_kind(&smf(1, 96, &[t.eot(0)])),
        MidiErrorKind::UndefinedStatus(0xF4)
    );
}

#[test]
fn orphan_note_events_are_preserved_not_invented() {
    let mut t = TrackBytes::new();
    t.off(0, 0, 50, 10) // orphan Note Off
        .on(5, 0, 60, 100)
        .off(10, 0, 60, 0)
        .on(20, 2, 70, 90); // orphan Note On (never released)
    let bytes = smf(1, 96, &[t.eot(40)]);
    let (doc, summary) = import_bytes(&bytes).unwrap();
    assert_eq!(notes_of(&doc, 0), vec![(5, 5, 60, 100, 0)]);
    assert!(
        summary
            .warnings
            .contains(&ImportWarning::UnmatchedNoteOff { track: 0, count: 1 })
    );
    assert!(
        summary
            .warnings
            .contains(&ImportWarning::UnmatchedNoteOn { track: 0, count: 1 })
    );
    assert_eq!(doc.track(0).unwrap().events.len(), 2);
    let out = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    assert_eq!(out, bytes, "orphans must round-trip byte-exactly here");
}

#[test]
fn unexpected_end_of_track_placement() {
    // EOT in the middle of a chunk: later bytes are ignored with a warning,
    // like MIDI players do.
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100)
        .off(10, 0, 60, 0)
        .meta(10, 0x2F, &[])
        .on(20, 0, 61, 100)
        .off(30, 0, 61, 0);
    let (doc, summary) = import_bytes(&smf(1, 96, &[t.data.clone()])).unwrap();
    assert_eq!(doc.note_count(), 1);
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| matches!(w, ImportWarning::DataAfterEndOfTrack { .. }))
    );
    // Missing EOT entirely: warning, one is written on export.
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100).off(10, 0, 60, 0);
    let (doc, summary) = import_bytes(&smf(1, 96, &[t.data.clone()])).unwrap();
    assert!(
        summary
            .warnings
            .contains(&ImportWarning::MissingEndOfTrack { track: 0 })
    );
    let out = export_to_bytes(&doc).unwrap();
    validate_exported_bytes(&doc, &plumspirit_engine::midi::plan_export(&doc), &out).unwrap();
    assert!(out.ends_with(&[0x00, 0xFF, 0x2F, 0x00]));
}

#[test]
fn header_quirks_are_tolerated() {
    let f = valid_file();
    // Extra header bytes, declared track count wrong, trailing garbage.
    let mut g = b"MThd\x00\x00\x00\x08\x00\x01\x00\x05\x00\x60\xAA\xBB".to_vec();
    g.extend_from_slice(&f[14..]);
    g.extend([0x00, 0x01, 0x02]);
    let (doc, summary) = import_bytes(&g).unwrap();
    assert_eq!(doc.note_count(), 2);
    assert!(
        summary
            .warnings
            .contains(&ImportWarning::LongHeader { extra: 2 })
    );
    assert!(
        summary
            .warnings
            .contains(&ImportWarning::TrackCountMismatch {
                declared: 5,
                found: 1
            })
    );
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| matches!(w, ImportWarning::TrailingBytes { .. }))
    );
}

#[test]
fn running_status_across_meta_is_accepted_with_warning() {
    let mut t = TrackBytes::new();
    t.ev(0, &[0x90, 60, 100])
        .meta(0, 0x01, b"x")
        .ev(5, &[60, 0]);
    let (doc, summary) = import_bytes(&smf(1, 96, &[t.eot(5)])).unwrap();
    assert_eq!(notes_of(&doc, 0), vec![(0, 5, 60, 100, 0)]);
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| matches!(w, ImportWarning::RunningStatusAcrossMeta { .. }))
    );
}

#[test]
fn fuzz_mutations_never_panic() {
    let seeds = [valid_file(), generate_dense_smf(300, 96, 5)];
    let mut rng = Rng(0xDEAD_BEEF);
    for base in &seeds {
        for _ in 0..4000 {
            let mut f = base.clone();
            match rng.below(4) {
                0 => {
                    let n = rng.below(f.len() as u64) as usize;
                    f.truncate(n);
                }
                1 => {
                    for _ in 0..1 + rng.below(4) {
                        let i = rng.below(f.len() as u64) as usize;
                        f[i] = rng.next() as u8;
                    }
                }
                2 => {
                    let i = rng.below(f.len() as u64) as usize;
                    f.insert(i, rng.next() as u8);
                }
                _ => {
                    let i = rng.below(f.len() as u64) as usize;
                    f.remove(i);
                }
            }
            // Must not panic; if it imports, it must export and validate.
            if let Ok((doc, _)) = import_bytes(&f) {
                for t in doc.tracks() {
                    t.notes.validate().unwrap();
                }
                if let Ok(out) = export_to_bytes(&doc) {
                    validate_exported_bytes(
                        &doc,
                        &plumspirit_engine::midi::plan_export(&doc),
                        &out,
                    )
                    .unwrap();
                }
            }
        }
    }
}
