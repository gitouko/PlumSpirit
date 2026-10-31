//! v1.0-Launch L-D05 (L-S12, contract §015): the compatibility reading of
//! deliberately out-of-spec MIDI files. Synthetic fixtures only — no
//! community file was supplied (the reported case is "A length or
//! delta-time value is longer than MIDI allows at byte 42,572 (track 2)").

mod common;

use common::*;
use plumspirit_engine::midi::{
    ImportControl, ImportSummary, ImportWarning, MidiError, MidiErrorKind, export_to_bytes,
    import_bytes, import_reader_with,
};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::project::KeyRange;

/// `v` as exactly `n` VLQ groups (leading zero groups: a non-canonical,
/// over-long encoding).
fn long_vlq(v: u64, n: usize) -> Vec<u8> {
    let mut g: Vec<u8> = (0..n)
        .map(|i| ((v >> (7 * (n - 1 - i))) & 0x7F) as u8)
        .collect();
    for b in g.iter_mut().take(n - 1) {
        *b |= 0x80;
    }
    g
}

fn compat(bytes: &[u8]) -> Result<(PlumDocument, ImportSummary), MidiError> {
    compat_len(bytes, Some(bytes.len() as u64))
}

fn compat_len(bytes: &[u8], len: Option<u64>) -> Result<(PlumDocument, ImportSummary), MidiError> {
    import_reader_with(
        bytes,
        len,
        ImportControl {
            compatibility: true,
            ..ImportControl::default()
        },
        KeyRange::Standard,
    )
}

fn format0(data: &[u8]) -> Vec<u8> {
    smf(0, 480, &[data.to_vec()])
}

const TEMPO: [u8; 7] = [0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20];

/// A track whose delta-times and one length use 5- and 6-byte quantities;
/// returns the data and the offset (in the data) of the first long one.
fn long_vlq_track() -> (Vec<u8>, usize) {
    let mut t = TEMPO.to_vec();
    let first = t.len();
    t.extend(long_vlq(0, 5));
    t.extend([0x90, 60, 100]);
    t.extend(long_vlq(480, 6));
    t.extend([0x80, 60, 0]);
    t.extend([0x00, 0xFF, 0x01]);
    t.extend(long_vlq(5, 5));
    t.extend(b"hello");
    t.extend(long_vlq(1 << 35, 6));
    t.extend([0x90, 62, 90]);
    t.extend([0x60, 0x80, 62, 0]);
    t.extend([0x00, 0xFF, 0x2F, 0x00]);
    (t, first)
}

#[test]
fn long_quantities_are_refused_strictly_and_read_exactly_in_compatibility_mode() {
    let (t, first) = long_vlq_track();
    let file = format0(&t);
    // Strict: unchanged — refused at the first long quantity, the message the
    // community saw.
    let e = import_bytes(&file).unwrap_err();
    assert_eq!(e.kind, MidiErrorKind::VlqTooLong);
    assert_eq!(e.offset, Some(22 + first as u64));
    assert_eq!(e.track, Some(0));
    assert!(e.is_repairable());
    assert_eq!(
        e.user_message(),
        format!(
            "A length or delta-time value is longer than MIDI allows at byte {} (track 1).",
            22 + first
        )
    );
    // Compatibility: the exact values.
    let (doc, summary) = compat(&file).unwrap();
    assert!(summary.compatibility);
    let notes = notes_of(&doc, 0);
    assert_eq!(
        notes,
        [(0, 480, 60, 100, 0), (480 + (1 << 35), 96, 62, 90, 0)]
    );
    assert_eq!(
        summary.warnings,
        [ImportWarning::LongVlq {
            track: 0,
            count: 4,
            first_offset: 22 + first as u64,
            longest: 6,
            largest: 1 << 35,
        }]
    );
    assert_eq!(
        summary.warnings[0].user_message(),
        format!(
            "Compatibility Mode, track 1: 4 delta-time or length value(s) used more than the 4 bytes MIDI allows (the first at byte {}, the longest 6 bytes); their exact values were read (largest 34,359,738,368).",
            22 + first
        )
    );
    // No strict-export provenance for a non-conforming source.
    assert!(doc.project().source.is_none());
    // The export is standard MIDI (gap fillers for the long delta) and reads
    // back strictly to the same notes.
    let out = export_to_bytes(&doc).unwrap();
    let (back, _) = import_bytes(&out).unwrap();
    assert_eq!(notes_of(&back, 0), notes);
    // A standard file reads identically in both modes.
    let mut ok = TrackBytes::new();
    ok.on(0, 0, 60, 100).off(480, 0, 60, 0);
    let std_file = format0(&ok.eot(480));
    let (a, sa) = import_bytes(&std_file).unwrap();
    let (b, sb) = compat(&std_file).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(sa.warnings, sb.warnings);
    assert!(!sa.compatibility && sb.compatibility);
    assert!(a.project().source.is_some());
}

#[test]
fn an_event_past_its_chunk_end_is_dropped_and_the_next_chunk_still_reads() {
    // Track 1 ends inside a Note On (no velocity byte); track 2 follows.
    let mut a = TrackBytes::new();
    a.on(0, 0, 60, 100).off(96, 0, 60, 0);
    let mut t1 = a.data.clone();
    let cut_at = t1.len();
    t1.extend([0x00, 0x90, 64]);
    let mut b = TrackBytes::new();
    b.on(0, 1, 70, 80).off(240, 1, 70, 0);
    let t2 = b.eot(240);
    let file = smf(1, 480, &[t1.clone(), t2]);
    let e = import_bytes(&file).unwrap_err();
    assert_eq!(e.kind, MidiErrorKind::EventPastChunkEnd);
    assert!(e.is_repairable());
    let (doc, s) = compat(&file).unwrap();
    assert_eq!(notes_of(&doc, 0), [(0, 96, 60, 100, 0)]);
    assert_eq!(notes_of(&doc, 1), [(0, 240, 70, 80, 1)]);
    assert!(
        s.warnings.contains(&ImportWarning::EventCutAtChunkEnd {
            track: 0,
            offset: 22 + cut_at as u64,
            dropped: 3,
        }),
        "{:?}",
        s.warnings
    );
    // A meta whose length runs past the chunk: dropped the same way, never
    // read from the next chunk.
    let mut t1 = a.data.clone();
    t1.extend([0x00, 0xFF, 0x01, 0x40]);
    t1.extend(b"short");
    let file = smf(1, 480, &[t1.clone(), b.data.clone()]);
    let (doc, s) = compat(&file).unwrap();
    assert_eq!(notes_of(&doc, 1), [(0, 240, 70, 80, 1)]);
    assert!(s.warnings.iter().any(|w| matches!(
        w,
        ImportWarning::EventCutAtChunkEnd {
            track: 0,
            dropped: 9,
            ..
        }
    )));
}

#[test]
fn a_file_cut_inside_a_track_keeps_every_complete_event() {
    let mut a = TrackBytes::new();
    a.on(0, 0, 60, 100)
        .off(96, 0, 60, 0)
        .on(96, 0, 62, 100)
        .off(192, 0, 62, 0);
    let full = format0(&a.eot(192));
    // Cut inside the second Note Off (after its status byte).
    let cut = full.len() - 6;
    let file = &full[..cut];
    let e = import_bytes(file).unwrap_err();
    assert_eq!(e.kind, MidiErrorKind::UnexpectedEof);
    assert!(e.is_repairable());
    for len in [Some(cut as u64), None] {
        let (doc, s) = compat_len(file, len).unwrap();
        // The second note never ends: kept as a raw Note On (as usual).
        assert_eq!(notes_of(&doc, 0), [(0, 96, 60, 100, 0)]);
        let w = s
            .warnings
            .iter()
            .find(|w| matches!(w, ImportWarning::TrackCutOff { .. }))
            .unwrap_or_else(|| panic!("{:?}", s.warnings));
        assert_eq!(
            *w,
            ImportWarning::TrackCutOff {
                track: 0,
                offset: cut as u64,
                dropped: 2,
                missing: 6,
            }
        );
        assert!(w.user_message().starts_with(&format!(
            "Compatibility Mode, track 1: the file ends at byte {cut}, inside this track (6 byte(s) short"
        )));
    }
}

#[test]
fn damage_without_a_deterministic_repair_is_still_refused() {
    let track = |body: &[u8]| {
        let mut t = body.to_vec();
        t.extend([0x00, 0xFF, 0x2F, 0x00]);
        format0(&t)
    };
    let cases: Vec<(&str, Vec<u8>, MidiErrorKind)> = vec![
        (
            "an 11-byte quantity",
            track(&[&[0x80u8; 10][..], &[0x00, 0x90, 60, 1]].concat()),
            MidiErrorKind::VlqTooLong,
        ),
        (
            "a quantity above 64 bits",
            track(&[&[0xFFu8; 9][..], &[0x7F, 0x90, 60, 1]].concat()),
            MidiErrorKind::VlqTooLong,
        ),
        (
            "ticks past 64 bits",
            track(
                &[
                    long_vlq(1 << 63, 10),
                    vec![0x90, 60, 1],
                    long_vlq(1 << 63, 10),
                    vec![0x80, 60, 0],
                ]
                .concat(),
            ),
            MidiErrorKind::TickOverflow,
        ),
        (
            "data without a status",
            track(&[0x00, 0x40, 0x40]),
            MidiErrorKind::DataWithoutStatus(0x40),
        ),
        (
            "an undefined status",
            track(&[0x00, 0xF4]),
            MidiErrorKind::UndefinedStatus(0xF4),
        ),
        (
            "a status in data",
            track(&[0x00, 0x90, 60, 0x90]),
            MidiErrorKind::StatusInData(0x90),
        ),
    ];
    for (what, file, kind) in cases {
        let e = compat(&file).map(|_| ()).unwrap_err();
        assert_eq!(e.kind, kind, "{what}");
    }
}

#[test]
fn workers_and_the_sequential_reader_repair_identically() {
    // Over 4 MB (the import decodes tracks on workers): many tracks with long
    // quantities, one cut at its chunk end.
    let mut tracks = Vec::new();
    for k in 0..12u8 {
        let mut t = TEMPO.to_vec();
        for i in 0..40_000u32 {
            t.extend(long_vlq(u64::from(i % 3), 5));
            t.extend([0x90 | (k & 15), 30 + (i % 60) as u8, 100]);
            t.extend(long_vlq(10, 6));
            t.extend([0x80 | (k & 15), 30 + (i % 60) as u8, 0]);
        }
        if k == 5 {
            t.extend([0x00, 0xFF, 0x01, 0x7F, b'x']);
        } else {
            t.extend([0x00, 0xFF, 0x2F, 0x00]);
        }
        tracks.push(t);
    }
    let file = smf(1, 480, &tracks);
    assert!(file.len() > 4 << 20);
    let (a, sa) = compat(&file).unwrap();
    let (b, sb) = compat_len(&file, None).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(sa.warnings, sb.warnings);
    assert_eq!(sa.note_count, 12 * 40_000);
    assert_eq!(
        sa.warnings
            .iter()
            .filter(|w| matches!(w, ImportWarning::LongVlq { count: 80_000, .. }))
            .count(),
        12
    );
    assert!(sa.warnings.iter().any(|w| matches!(
        w,
        ImportWarning::EventCutAtChunkEnd {
            track: 5,
            dropped: 5,
            ..
        }
    )));
}

#[test]
fn mutated_files_never_panic_or_hang_in_compatibility_mode() {
    let (t, _) = long_vlq_track();
    let mut a = TrackBytes::new();
    for i in 0..200u64 {
        a.on(i * 10, (i % 16) as u8, 40 + (i % 50) as u8, 90)
            .meta(i * 10, 0x01, b"t");
        a.off(i * 10 + 5, (i % 16) as u8, 40 + (i % 50) as u8, 0);
    }
    let sources = [format0(&t), smf(1, 96, &[a.eot(2000), t.clone()])];
    let mut rng = Rng(0x5EED_CAFE);
    let (mut ok, mut refused) = (0u32, 0u32);
    for src in &sources {
        for _ in 0..2000 {
            let mut m = src.clone();
            match rng.below(4) {
                0 => m.truncate(1 + rng.below(m.len() as u64) as usize),
                _ => {
                    for _ in 0..1 + rng.below(4) {
                        let at = rng.below(m.len() as u64) as usize;
                        m[at] = rng.next() as u8;
                    }
                }
            }
            match compat(&m) {
                Ok((doc, _)) => {
                    ok += 1;
                    // Whatever was read exports as standard MIDI.
                    let out = export_to_bytes(&doc).unwrap();
                    assert!(import_bytes(&out).is_ok());
                }
                Err(_) => refused += 1,
            }
        }
    }
    assert_eq!(ok + refused, 4000);
    assert!(
        ok > 1000,
        "most single-byte damage is repaired or harmless ({ok})"
    );
}
