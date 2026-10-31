//! B1-S41 (contract §014-A, B1-0100 – B1-0102): PPQN and BPM boundaries.
//! Standard MIDI PPQN is 1 – 32,767 (the division word's high bit selects
//! SMPTE); a larger project PPQN (the owner asked 65,535) is kept in the
//! project and rescaled — with a disclosure — on MIDI export, never written as
//! an invalid header. Tempos map to 24-bit µs per quarter note (60,000,000
//! BPM = 1 µs). SMPTE divisions import and export as themselves.

mod common;

use common::*;
use plumspirit_engine::midi::{
    export_to_bytes_with, import_bytes, plan_export, validate_exported_bytes,
};
use plumspirit_engine::model::project::{
    MAX_MIDI_TEMPO_MICROS, MIDI_BPM_MAX_TEXT, MIDI_BPM_MIN_TEXT,
};
use plumspirit_engine::model::timing::{MAX_SMF_PPQN, TimingKind, parse_ppqn};
use plumspirit_engine::model::{Bpm, PlumDocument, TempoFidelity};

fn doc_at(ppqn: u32, notes: &[(u64, u64, u8)]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b1s41".into(), Bpm::DEFAULT, ppqn);
    let v: Vec<_> = notes.iter().map(|&(s, l, p)| rec(s, l, p)).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, false)
        .unwrap();
    d
}

fn header_division(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[12], bytes[13]])
}

#[test]
fn the_standard_ppqn_ends_at_32767_and_larger_ones_are_rescaled_with_a_disclosure() {
    assert_eq!(MAX_SMF_PPQN, 32_767);
    // 32,767 is standard: written as it is, nothing to disclose.
    let d = doc_at(32_767, &[(0, 32_767, 60), (32_767, 1, 61)]);
    let plan = plan_export(&d);
    assert!(plan.rescale.is_none());
    let bytes = export_to_bytes_with(&d, &plan).unwrap();
    assert_eq!(header_division(&bytes), 32_767);
    validate_exported_bytes(&d, &plan, &bytes).unwrap();
    // The owner's 65,535 and 32,768: kept in the project, rescaled on export
    // to a standard division, said before writing; the header's high bit is
    // never set (that would mean SMPTE).
    for ppqn in [32_768u32, 65_535, 96_000, 2_147_483_647] {
        assert!(
            parse_ppqn(&ppqn.to_string()).is_ok(),
            "{ppqn} is a project PPQN"
        );
        let q = u64::from(ppqn);
        let d = doc_at(ppqn, &[(0, q, 60), (q, q / 2, 62), (q * 3 / 2, 1, 64)]);
        let plan = plan_export(&d);
        let r = plan.rescale.clone().expect("rescaled");
        assert!(
            r.to_ppqn >= 1 && r.to_ppqn <= 32_767,
            "{ppqn} → {}",
            r.to_ppqn
        );
        assert!(plan.needs_approval());
        assert!(
            plan.disclosures()
                .iter()
                .any(|s| s.contains("at most 32,767 PPQN")),
            "{ppqn}: {:?}",
            plan.disclosures()
        );
        let bytes = export_to_bytes_with(&d, &plan).unwrap();
        let w = header_division(&bytes);
        assert_eq!(w & 0x8000, 0, "{ppqn}: never an SMPTE-looking word");
        assert_eq!(u32::from(w), r.to_ppqn);
        validate_exported_bytes(&d, &plan, &bytes).unwrap();
        let (back, _) = import_bytes(&bytes).unwrap();
        assert_eq!(back.note_count(), 3);
    }
    // Outside the representation: refused with a sentence.
    assert!(parse_ppqn("0").is_err());
    assert!(
        parse_ppqn("2147483648")
            .unwrap_err()
            .contains("2,147,483,647")
    );
}

/// A Format 0 file with `division` and one note (track: note on / off at
/// ticks 0 / 10, end).
fn smf(division: u16) -> Vec<u8> {
    let mut t = vec![
        0x00, 0x90, 60, 100, 0x0A, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00,
    ];
    let mut f = b"MThd".to_vec();
    f.extend_from_slice(&6u32.to_be_bytes());
    f.extend_from_slice(&0u16.to_be_bytes());
    f.extend_from_slice(&1u16.to_be_bytes());
    f.extend_from_slice(&division.to_be_bytes());
    f.extend_from_slice(b"MTrk");
    f.extend_from_slice(&(t.len() as u32).to_be_bytes());
    f.append(&mut t);
    f
}

#[test]
fn smpte_divisions_import_and_export_as_themselves() {
    // −24, −25, −29 (29.97 drop frame) and −30 fps.
    for (fps, tpf) in [(24u8, 4u8), (25, 40), (29, 100), (30, 80)] {
        let word = (u16::from((-(fps as i8)) as u8) << 8) | u16::from(tpf);
        let (doc, _) = import_bytes(&smf(word)).unwrap();
        let div = doc.division();
        assert_eq!(
            div.kind(),
            TimingKind::Smpte {
                frames_per_second: fps,
                ticks_per_frame: tpf
            }
        );
        assert_eq!(div.musical_ppqn(), None, "never read as a PPQN");
        assert_eq!(div.smf_word(), Some(word));
        let plan = plan_export(&doc);
        assert!(plan.rescale.is_none());
        let bytes = export_to_bytes_with(&doc, &plan).unwrap();
        assert_eq!(header_division(&bytes), word, "{fps} fps kept");
        assert_eq!(doc.note_count(), 1);
    }
}

#[test]
fn tempos_map_to_24_bit_microseconds_with_the_cap_and_the_floor_checked() {
    let b = |s: &str| Bpm::parse(s).unwrap();
    // The cap: 1 µs per quarter note, exact.
    let t = b(MIDI_BPM_MAX_TEXT.replace(',', "").as_str()).to_micros_per_quarter();
    assert_eq!((t.micros, t.fidelity), (1, TempoFidelity::Exact));
    assert!(Bpm::parse_midi("60000000").is_ok());
    assert!(Bpm::parse_midi("60000001").is_err());
    // The floor: the slowest typed tempo still stores 16,777,215 µs.
    let t = b(MIDI_BPM_MIN_TEXT).to_micros_per_quarter();
    assert_eq!(t.micros, MAX_MIDI_TEMPO_MICROS);
    assert_ne!(t.fidelity, TempoFidelity::Clamped);
    assert!(Bpm::parse_midi("3.5").is_err());
    // A project tempo outside (from an older project) still exports, clamped
    // and disclosed — never as an invalid tempo.
    let d = PlumDocument::new_project("fast".into(), b("1e9"), 960);
    let plan = plan_export(&d);
    assert!(plan.needs_approval(), "a clamped tempo is confirmed");
    let bytes = export_to_bytes_with(&d, &plan).unwrap();
    validate_exported_bytes(&d, &plan, &bytes).unwrap();
    let pos = bytes
        .windows(3)
        .position(|w| w == [0xFF, 0x51, 0x03])
        .unwrap();
    let us = u32::from_be_bytes([0, bytes[pos + 3], bytes[pos + 4], bytes[pos + 5]]);
    assert_eq!(us, 1);
    // Rounding is said: 123.456 BPM is stored as 486,003 µs.
    assert!(b("123.456").midi_note().unwrap().contains("486,003"));
}

#[test]
fn time_math_at_the_extremes_never_overflows() {
    // The largest project PPQN with the fastest and slowest MIDI tempos:
    // seconds of the last representable tick stay finite and ordered.
    for bpm in ["60000000", MIDI_BPM_MIN_TEXT, "120"] {
        let d = PlumDocument::new_project("x".into(), Bpm::parse(bpm).unwrap(), 2_147_483_647);
        let map = plumspirit_engine::model::TempoMap::for_document_at(
            &d,
            1_000_000,
            plumspirit_engine::model::Speed::NORMAL,
        );
        let a = map.seconds_at_tick(0);
        let z = map.seconds_at_tick(u64::MAX >> 1);
        assert!(a == 0.0 && z.is_finite() && z > a, "{bpm}: {z}");
    }
}
