//! v1.0-beta1 B1-0121 / B1-0123 / B1-0124 (contract §019): strict no-edit
//! export writes an unchanged imported project from its original file —
//! every event of every track in its order, with Running Status
//! Compression on (a compact source comes out byte for byte) or off (every
//! status byte) — and is refused, with the reason, after an edit or when
//! the source changed. Synthetic fixtures with the structures the owner's
//! tau2.5.9 / 160 RDR reports name (duplicate tempo at one tick, a tempo on
//! track 1, Note On before Note Off at one tick); the owner's own files are
//! B1-S11.

mod common;

use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::midi::compare::{compare_files, summarize};
use plumspirit_engine::midi::error::{ImportWarning, MidiError};
use plumspirit_engine::midi::reader::{ByteSource, SmfHandler, Token, read_smf};
use plumspirit_engine::midi::strict::{StatusBytes, export_strict};
use plumspirit_engine::midi::{
    ExportControl, ExportError, export_to_bytes, import_file, import_file_with,
};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use std::path::PathBuf;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "plumspirit-b1-strict-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = kind.to_vec();
    v.extend_from_slice(&(data.len() as u32).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// The owner-report structures, each switchable: the duplicate tempo at 480
/// on track 0 (`dup`), and at tick 960 Note On before Note Off (`on_first`).
fn source_with(dup: bool, on_first: bool) -> Vec<u8> {
    let mut f = b"MThd\0\0\0\x06\0\x01\0\x02\x01\xE0".to_vec();
    let mut t0: Vec<u8> = vec![
        0x00, 0xFF, 0x03, 0x04, b'C', b'o', b'n', b'd', // track name
        0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // tempo 120
        0x00, 0xFF, 0x58, 0x04, 0x04, 0x02, 0x18, 0x08, // 4/4
        0x83, 0x60, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // tempo at 480
    ];
    if dup {
        t0.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]); // the same again
    }
    t0.extend_from_slice(&[
        0x00, 0xFF, 0x06, 0x05, b'V', b'e', b'r', b's', b'e', // marker
        0x00, 0xFF, 0x2F, 0x00,
    ]);
    let mut t1: Vec<u8> = vec![
        0x00, 0xFF, 0x51, 0x03, 0x06, 0x1A, 0x80, // a tempo on track 1, tick 0
        0x00, 0xC0, 0x05, // program
        0x00, 0xB0, 0x07, 0x64, // volume
        0x00, 0x90, 0x3C, 0x64, // C4 on
        0x00, 0x3E, 0x64, // D4 on (running status)
        0x00, 0xE0, 0x00, 0x40, // bend
        0x83, 0x60, 0x90, 0x3C, 0x00, // C4 off as Note On velocity 0
        0x00, 0x3E, 0x00, // D4 off (running status)
        0x00, 0x3C, 0x50, // C4 again at the same tick
    ];
    if on_first {
        t1.extend_from_slice(&[0x83, 0x60, 0x41, 0x64, 0x00, 0x80, 0x3C, 0x40]); // F4 on, then C4 off
    } else {
        t1.extend_from_slice(&[0x83, 0x60, 0x80, 0x3C, 0x40, 0x00, 0x90, 0x41, 0x64]);
    }
    t1.extend_from_slice(&[
        0x00, 0xF0, 0x05, 0x7E, 0x7F, 0x09, 0x01, 0xF7, // SysEx
        0x00, 0xF7, 0x02, 0xF3, 0x01, // escape
        0x00, 0xFF, 0x60, 0x02, 0xAB, 0xCD, // an undefined meta type
        0x83, 0x60, 0x80, 0x41, 0x40, // F4 off
        0x00, 0xFF, 0x2F, 0x00,
    ]);
    f.extend(chunk(b"MTrk", &t0));
    f.extend(chunk(b"XFIH", b"\x01\x02\x03\x04"));
    f.extend(chunk(b"MTrk", &t1));
    f
}

/// A compact, standard Format 1 file (running status, status repeated
/// after every meta / SysEx) with what the owner's reports name.
fn compact_source() -> Vec<u8> {
    source_with(true, true)
}

/// Every token, End of Track and other chunk of a file, in order.
#[derive(Default)]
struct Tokens {
    header: Option<(u16, u16)>,
    items: Vec<String>,
    warnings: Vec<String>,
}

impl SmfHandler for Tokens {
    fn header(&mut self, format: u16, _n: u16, division: u16) -> Result<(), MidiError> {
        self.header = Some((format, division));
        Ok(())
    }
    fn begin_track(&mut self, index: usize) -> Result<(), MidiError> {
        self.items.push(format!("MTrk {index}"));
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        self.items.push(format!("{t:?}"));
        Ok(())
    }
    fn end_track(&mut self, _index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        self.items.push(format!("EOT {eot:?}"));
        Ok(())
    }
    fn extra_chunk(&mut self, t: [u8; 4], data: Vec<u8>, after: usize) -> Result<(), MidiError> {
        self.items.push(format!("{t:?} {data:?} {after}"));
        Ok(())
    }
    fn warning(&mut self, w: ImportWarning) {
        self.warnings.push(w.user_message());
    }
}

fn tokens(bytes: &[u8]) -> Tokens {
    let mut t = Tokens::default();
    let mut src = ByteSource::new(bytes, Some(bytes.len() as u64));
    read_smf(&mut src, &mut t).unwrap();
    t
}

fn setup(name: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
    let d = dir(name);
    let mid = d.join("source.mid");
    std::fs::write(&mid, bytes).unwrap();
    (d, mid)
}

#[test]
fn an_unchanged_import_exports_byte_for_byte_with_running_status() {
    let source = compact_source();
    let (d, mid) = setup("identical", &source);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let out = d.join("out.mid");
    let r = export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), source);
    assert!(r.identical_to_source, "{r:?}");
    assert_eq!((r.tracks, r.notes, r.status_bytes_saved), (2, 4, 4));
    assert_eq!(r.bytes, source.len() as u64);
    assert!(r.normalised.is_empty(), "{:?}", r.normalised);
    assert!(r.summary().contains("byte for byte"), "{}", r.summary());
    // Exporting over the source itself works too (it is read before the
    // temporary file replaces it).
    export_strict(&doc, &mid, StatusBytes::Running, ExportControl::default()).unwrap();
    assert_eq!(std::fs::read(&mid).unwrap(), source);
}

#[test]
fn running_status_off_writes_every_status_and_keeps_every_event() {
    let source = compact_source();
    let (d, mid) = setup("explicit", &source);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let out = d.join("out.mid");
    let r = export_strict(&doc, &out, StatusBytes::Explicit, ExportControl::default()).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(
        bytes.len(),
        source.len() + 4,
        "one status byte per running message"
    );
    assert!(!r.identical_to_source);
    assert_eq!(r.status_bytes_saved, 0);
    let (a, b) = (tokens(&source), tokens(&bytes));
    assert_eq!(a.header, b.header);
    assert_eq!(a.items, b.items);
    // And back: Running Status Compression on the explicit file restores the
    // compact bytes (strict export of the explicit file's import).
    let (doc2, _) = import_file(&out, Default::default()).unwrap();
    let out2 = d.join("again.mid");
    let r2 = export_strict(&doc2, &out2, StatusBytes::Running, ExportControl::default()).unwrap();
    assert_eq!(std::fs::read(&out2).unwrap(), source);
    assert_eq!(r2.status_bytes_saved, 4);
}

#[test]
fn strict_export_keeps_what_the_normal_export_rewrites() {
    let source = compact_source();
    let (_d, mid) = setup("versus", &source);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    // The normal export writes from the editable model: the same music, but
    // not the source's structure — the three things the owner reported for
    // tau2.5.9 / 160 RDR (B1-0120), reproduced here:
    let normal = export_to_bytes(&doc).unwrap();
    let (n, src) = (tokens(&normal).items, tokens(&source).items);
    assert_ne!(n, src);
    let tempo = |items: &[String], tick: u64| {
        items
            .iter()
            .filter(|s| s.starts_with(&format!("Meta {{ tick: {tick}, meta_type: 81")))
            .count()
    };
    // 1. the duplicate tempo at tick 480 is dropped,
    assert_eq!((tempo(&src, 480), tempo(&n, 480)), (2, 1));
    // 2. track 1's tempo moves into track 0 (and replaces the tick-0 one),
    let track1 = |items: &[String]| {
        let i = items.iter().position(|s| s == "MTrk 1").unwrap();
        items[i..]
            .iter()
            .filter(|s| s.contains("meta_type: 81"))
            .count()
    };
    assert_eq!((track1(&src), track1(&n)), (1, 0));
    // 3. at tick 960 the Note Off is written before the Note On.
    let at960 = |items: &[String]| -> Vec<String> {
        items
            .iter()
            .filter(|s| s.starts_with("Channel { tick: 960"))
            .cloned()
            .collect()
    };
    let (a, b) = (at960(&src), at960(&n));
    assert!(
        a[0].contains("status: 144") && b[0].contains("status: 128"),
        "{a:?} {b:?}"
    );
    // Strict export keeps all three (the tests above).
}

#[test]
fn an_edit_refuses_strict_export_and_undo_brings_it_back() {
    let source = compact_source();
    let (d, mid) = setup("edit", &source);
    let (mut doc, _) = import_file(&mid, Default::default()).unwrap();
    let out = d.join("out.mid");
    std::fs::write(&out, b"previous").unwrap();
    let mut h = History::new(HistoryLimits::default());
    let rec = NoteRecord::new(
        10,
        10,
        NoteAttrs::new(70, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
    );
    h.apply(
        &mut doc,
        Operation::InsertNote {
            track: 1.into(),
            record: rec,
        },
    )
    .unwrap();
    let e = export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap_err();
    assert!(matches!(e, ExportError::Blocked(_)), "{e:?}");
    assert!(
        e.user_message().contains("MIDI edits"),
        "{}",
        e.user_message()
    );
    assert!(e.user_message().ends_with("Nothing was written."));
    assert_eq!(std::fs::read(&out).unwrap(), b"previous");
    h.undo(&mut doc).unwrap();
    export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), source);
}

#[test]
fn a_changed_or_missing_source_refuses_strict_export() {
    let source = compact_source();
    let (d, mid) = setup("changed", &source);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let out = d.join("out.mid");
    let mut other = source.clone();
    let i = other.len() - 6;
    other[i] ^= 0x01;
    std::fs::write(&mid, &other).unwrap();
    let e = export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap_err();
    assert!(
        e.user_message().contains("has changed"),
        "{}",
        e.user_message()
    );
    std::fs::remove_file(&mid).unwrap();
    let e = export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap_err();
    assert!(
        e.user_message().contains("cannot be read"),
        "{}",
        e.user_message()
    );
    assert!(!out.exists());
    // No temporary file is left behind.
    let left: Vec<_> = std::fs::read_dir(&d).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_lenient_source_keeps_its_events_and_reports_what_was_normalised() {
    let mut f = b"MThd\0\0\0\x08\0\x01\0\x03\x01\xE0\xAA\xBB".to_vec();
    f.extend(chunk(
        b"MTrk",
        &[
            0x00, 0x90, 0x3C, 0x64, // C4 on
            0x00, 0xFF, 0x01, 0x01, b'x', // text
            0x83, 0x60, 0x3C, 0x00, // running status across the meta (non-standard)
            0x00, 0xFF, 0x2F, 0x00, 0x00, 0x00, // two bytes after End of Track
        ],
    ));
    f.extend(chunk(
        b"MTrk",
        &[0x00, 0x90, 0x40, 0x64, 0x83, 0x60, 0x40, 0x00],
    )); // no EOT
    f.extend_from_slice(b"\0\0\0"); // trailing bytes
    let (d, mid) = setup("lenient", &f);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let out = d.join("out.mid");
    let r = export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap();
    assert!(!r.identical_to_source);
    let all = r.normalised.join(" | ");
    assert!(r.normalised.len() >= 6, "{all}");
    assert!(all.contains("Track 2 had no End of Track"), "{all}");
    let (a, b) = (tokens(&f), tokens(&std::fs::read(&out).unwrap()));
    let expected: Vec<String> = a
        .items
        .iter()
        .map(|s| {
            if s == "EOT None" {
                "EOT Some(480)".to_owned()
            } else {
                s.clone()
            }
        })
        .collect();
    assert_eq!(b.items, expected);
    assert!(
        b.warnings.is_empty(),
        "the written file is standard: {:?}",
        b.warnings
    );
}

#[test]
fn strict_export_survives_a_lup_save_and_reopen() {
    let source = compact_source();
    let (d, mid) = setup("lup", &source);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let lup = d.join("p.lup");
    save_project(&doc, &lup, LupControl::default()).unwrap();
    let (re, _) = load_project(&lup, LupControl::default()).unwrap();
    let out = d.join("out.mid");
    export_strict(&re, &out, StatusBytes::Running, ExportControl::default()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), source);
}

#[test]
fn a_256_key_file_keeps_its_wide_notes() {
    let mut f = b"MThd\0\0\0\x06\0\x01\0\x01\x01\xE0".to_vec();
    f.extend(chunk(
        b"MTrk",
        &[
            0x00, 0x90, 0xC8, 0x40, // key 200 on (explicit status, Kiva's rule)
            0x00, 0x90, 0x3C, 0x40, // the next message repeats its status
            0x00, 0x3E, 0x40, // running status again
            0x83, 0x60, 0x80, 0xC8, 0x40, // key 200 off
            0x00, 0x80, 0x3C, 0x40, //
            0x00, 0x3E, 0x40, //
            0x00, 0xFF, 0x2F, 0x00,
        ],
    ));
    let (d, mid) = setup("wide", &f);
    let (doc, _) = import_file_with(&mid, Default::default(), KeyRange::Extended256).unwrap();
    let out = d.join("out.mid");
    let r = export_strict(&doc, &out, StatusBytes::Running, ExportControl::default()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), f);
    assert_eq!(r.status_bytes_saved, 2);
}

/// Measurement (release): strict export of a 1,000,000-note file against the
/// normal export of the same project.
#[test]
#[ignore]
fn strict_export_cost() {
    use plumspirit_engine::model::{Bpm, PlumDocument};
    let recs: Vec<NoteRecord> = (0..1_000_000u64)
        .map(|k| {
            NoteRecord::new(
                k * 2,
                3 + k % 40,
                NoteAttrs::new(
                    (k % 128) as u8,
                    1 + (k % 127) as u8,
                    (k % 16) as u8,
                    64,
                    NoteOffKind::NoteOff,
                )
                .unwrap(),
            )
        })
        .collect();
    let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 960);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let dir = dir("cost");
    let mid = dir.join("big.mid");
    std::fs::write(&mid, export_to_bytes(&d).unwrap()).unwrap();
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let plan = plumspirit_engine::midi::plan_export(&doc);
    let (mut strict, mut normal) = (Vec::new(), Vec::new());
    for i in 0..5 {
        let t = std::time::Instant::now();
        export_strict(
            &doc,
            &dir.join(format!("s{i}.mid")),
            StatusBytes::Running,
            ExportControl::default(),
        )
        .unwrap();
        strict.push(t.elapsed().as_secs_f64() * 1e3);
        let t = std::time::Instant::now();
        plumspirit_engine::midi::export_to_path(
            &doc,
            &plan,
            &dir.join(format!("n{i}.mid")),
            ExportControl::default(),
        )
        .unwrap();
        normal.push(t.elapsed().as_secs_f64() * 1e3);
    }
    strict.sort_by(f64::total_cmp);
    normal.sort_by(f64::total_cmp);
    println!(
        "1M notes, {} bytes: strict export median {:.1} ms; normal export (write + validate) median {:.1} ms",
        std::fs::metadata(&mid).unwrap().len(),
        strict[2],
        normal[2]
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------- comparator (B1-0125)

#[test]
fn the_comparator_tells_identical_bytes_from_the_same_events() {
    let source = compact_source();
    let (d, mid) = setup("cmp-same", &source);
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    let (on, off) = (d.join("on.mid"), d.join("off.mid"));
    export_strict(&doc, &on, StatusBytes::Running, ExportControl::default()).unwrap();
    export_strict(&doc, &off, StatusBytes::Explicit, ExportControl::default()).unwrap();
    let r = compare_files(&mid, &on, KeyRange::Standard).unwrap();
    assert!(r.byte_identical && r.same_events, "{:?}", r.lines());
    let r = compare_files(&mid, &off, KeyRange::Standard).unwrap();
    assert!(!r.byte_identical && r.same_events, "{:?}", r.lines());
    assert!(
        r.lines()[2].contains("status-byte encoding only"),
        "{:?}",
        r.lines()
    );
    assert_eq!(r.a.totals(), r.b.totals());
    let t = r.a.totals();
    assert_eq!(
        (t.note_on, t.note_off, t.note_on_zero, t.tempo),
        (4, 2, 2, 4)
    );
}

#[test]
fn the_comparator_names_each_difference_the_owner_reported() {
    let d = dir("cmp-diff");
    let write = |name: &str, b: &[u8]| {
        let p = d.join(name);
        std::fs::write(&p, b).unwrap();
        p
    };
    let src = write("src.mid", &compact_source());
    // The duplicate tempo dropped: track 1 (chunk 0), its 5th event.
    let no_dup = write("nodup.mid", &source_with(false, true));
    let r = compare_files(&src, &no_dup, KeyRange::Standard).unwrap();
    assert!(!r.same_events);
    let f = r.first_difference.clone().unwrap();
    assert_eq!((f.track, f.index), (0, 4), "{:?}", r.lines());
    assert!(f.a.unwrap().contains("Tempo 500000"));
    assert!(f.b.unwrap().contains("meta FF 06"));
    assert_eq!(
        (r.a.tracks[0].counts.tempo, r.b.tracks[0].counts.tempo),
        (3, 2)
    );
    // Note Off written before Note On at tick 960 (same per-tick multiset).
    let off_first = write("offfirst.mid", &source_with(true, false));
    let r = compare_files(&src, &off_first, KeyRange::Standard).unwrap();
    let f = r.first_difference.clone().unwrap();
    assert_eq!((f.track, f.index), (1, 9), "{:?}", r.lines());
    assert!(f.a.as_deref().unwrap().contains("Note On key 65"));
    assert!(f.b.as_deref().unwrap().contains("Note Off key 60"));
    assert_eq!(r.a.totals(), r.b.totals(), "counts alone cannot see it");
    // The normal export: track 1's tempo now in track 0.
    let (doc, _) = import_file(&src, Default::default()).unwrap();
    let normal = write("normal.mid", &export_to_bytes(&doc).unwrap());
    let r = compare_files(&src, &normal, KeyRange::Standard).unwrap();
    assert!(!r.same_events);
    assert_eq!(
        (r.a.tracks[1].counts.tempo, r.b.tracks[1].counts.tempo),
        (1, 0)
    );
    assert!(
        r.lines()
            .iter()
            .any(|l| l.starts_with("Track 2: counts differ")),
        "{:?}",
        r.lines()
    );
}

#[test]
fn a_difference_past_the_first_block_is_located_exactly() {
    let build = |changed: bool| {
        let mut t = common::TrackBytes::new();
        for i in 0..100_000u64 {
            let vel = if changed && i == 75_000 { 99 } else { 100 };
            let key = 40 + (i % 40) as u8;
            if i == 0 {
                t.ev(0, &[0x90, key, vel]);
            } else {
                t.ev(i * 2, &[key, vel]);
            }
            t.ev(i * 2 + 1, &[key, 0]);
        }
        common::smf(0, 480, &[t.eot(200_000)])
    };
    let d = dir("cmp-big");
    let (a, b) = (d.join("a.mid"), d.join("b.mid"));
    std::fs::write(&a, build(false)).unwrap();
    std::fs::write(&b, build(true)).unwrap();
    let r = compare_files(&a, &b, KeyRange::Standard).unwrap();
    let f = r.first_difference.clone().unwrap();
    assert_eq!((f.track, f.index), (0, 150_000));
    assert!(f.a.unwrap().ends_with("velocity 100, channel 1"));
    assert!(f.b.unwrap().ends_with("velocity 99, channel 1"));
    assert_eq!(r.a.tracks[0].tokens, 200_000);
    let same = compare_files(&a, &a, KeyRange::Standard).unwrap();
    assert!(same.byte_identical && same.same_events);
}

#[test]
fn malformed_input_is_an_error_never_a_panic() {
    let d = dir("cmp-bad");
    let good = d.join("good.mid");
    std::fs::write(&good, compact_source()).unwrap();
    let mut cut = compact_source();
    cut.truncate(40);
    for (name, bytes) in [
        ("cut.mid", cut),
        ("empty.mid", Vec::new()),
        ("text.mid", b"not a midi file at all".to_vec()),
    ] {
        let p = d.join(name);
        std::fs::write(&p, &bytes).unwrap();
        assert!(
            compare_files(&good, &p, KeyRange::Standard).is_err(),
            "{name}"
        );
        assert!(summarize(&p, KeyRange::Standard).is_err(), "{name}");
    }
    assert!(compare_files(&good, &d.join("missing.mid"), KeyRange::Standard).is_err());
}

/// The independent reader (`tests/common/smf_digest.rs`, written from the
/// SMF specification, nothing shared with `midi::reader`) counts the same
/// events per track as the comparator.
#[test]
fn an_independent_reader_counts_the_same_events() {
    let d = dir("cmp-readerb");
    for (i, bytes) in [
        compact_source(),
        source_with(false, false),
        common::generate_dense_smf(5_000, 960, 7),
    ]
    .into_iter()
    .enumerate()
    {
        let p = d.join(format!("{i}.mid"));
        std::fs::write(&p, &bytes).unwrap();
        let a = summarize(&p, KeyRange::Standard).unwrap();
        let b = common::smf_digest::digest(&bytes, false).unwrap();
        assert_eq!(a.tracks.len(), b.tracks.len());
        for (ta, tb) in a.tracks.iter().zip(&b.tracks) {
            let c = ta.counts;
            assert_eq!(c.note_on as usize, tb.ons.len(), "{i}");
            assert_eq!((c.note_off + c.note_on_zero) as usize, tb.offs.len(), "{i}");
            assert_eq!(c.control_change as usize, tb.controllers.len(), "{i}");
            assert_eq!(c.program_change as usize, tb.programs.len(), "{i}");
            assert_eq!(c.pitch_bend as usize, tb.pitch_bends.len(), "{i}");
            assert_eq!(c.tempo as usize, tb.tempos.len(), "{i}");
            assert_eq!(ta.end_of_track, tb.end_of_track, "{i}");
        }
    }
}

/// Measurement (release): the comparator on a 4,000,000-event file against
/// its every-status twin (the slow case: same events, different bytes).
#[test]
#[ignore]
fn comparator_cost() {
    let bytes = common::generate_dense_smf(2_000_000, 960, 11);
    let d = dir("cmp-cost");
    let a = d.join("a.mid");
    std::fs::write(&a, &bytes).unwrap();
    let (doc, _) = import_file(&a, Default::default()).unwrap();
    let b = d.join("b.mid");
    export_strict(&doc, &b, StatusBytes::Explicit, ExportControl::default()).unwrap();
    let mut t = Vec::new();
    for _ in 0..3 {
        let s = std::time::Instant::now();
        let r = compare_files(&a, &b, KeyRange::Standard).unwrap();
        assert!(r.same_events && !r.byte_identical);
        t.push(s.elapsed().as_secs_f64() * 1e3);
    }
    t.sort_by(f64::total_cmp);
    let events: u64 = summarize(&a, KeyRange::Standard)
        .unwrap()
        .tracks
        .iter()
        .map(|t| t.tokens)
        .sum();
    println!(
        "comparator: {events} events per file ({} + {} bytes): median {:.0} ms ({:.1} M events/s per file)",
        std::fs::metadata(&a).unwrap().len(),
        std::fs::metadata(&b).unwrap().len(),
        t[1],
        2.0 * events as f64 / t[1] / 1e3
    );
    std::fs::remove_dir_all(&d).ok();
}

/// Peak resident memory of this process (Linux `VmHWM`; 0 elsewhere), MB.
fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

/// v1.0-beta1 B1-0120 / 0126 / 0127 (MIDI-02 / 03 / 04) on the owner's own
/// files: `PLUMSPIRIT_OWNER_MIDI="a.mid;b.mid" cargo test --release -p
/// plumspirit_engine --test b1_strict_export owner_files -- --ignored
/// --nocapture`. Each file is imported unchanged and exported strictly with
/// Running Status Compression ON and OFF; both exports must hold the
/// source's events in the source's order in every track (the streaming
/// comparator), ON byte for byte when the source uses running status. The
/// normal export's differences are printed for the record.
#[test]
#[ignore = "needs the owner's files (PLUMSPIRIT_OWNER_MIDI)"]
fn owner_files_export_strictly_with_the_sources_events() {
    let Ok(list) = std::env::var("PLUMSPIRIT_OWNER_MIDI") else {
        println!("PLUMSPIRIT_OWNER_MIDI is not set");
        return;
    };
    let dir = dir("owner");
    for path in list.split(';').filter(|p| !p.is_empty()) {
        let src = PathBuf::from(path);
        let t = std::time::Instant::now();
        let (doc, _) =
            plumspirit_engine::midi::import_file_with(&src, Default::default(), KeyRange::Standard)
                .unwrap();
        let import = t.elapsed();
        assert_eq!(
            plumspirit_engine::midi::provenance::strict_candidate(&doc),
            plumspirit_engine::midi::provenance::SourceStatus::Unchanged,
            "{path}"
        );
        for (name, status) in [("on", StatusBytes::Running), ("off", StatusBytes::Explicit)] {
            let out = dir.join(format!("strict-{name}.mid"));
            let r = export_strict(&doc, &out, status, ExportControl::default()).unwrap();
            let t = std::time::Instant::now();
            let cmp = compare_files(&src, &out, KeyRange::Standard).unwrap();
            println!(
                "{path} — strict, Running Status {name}: {}; compare {:.2} s: byte identical {}, same events {}",
                r.summary(),
                t.elapsed().as_secs_f64(),
                cmp.byte_identical,
                cmp.same_events
            );
            assert!(cmp.same_events, "{path} RSB {name}: {:?}", cmp.lines());
            std::fs::remove_file(&out).ok();
        }
        let out = dir.join("normal.mid");
        let plan = plumspirit_engine::midi::plan_export(&doc);
        plumspirit_engine::midi::export_to_path(&doc, &plan, &out, ExportControl::default())
            .unwrap();
        let cmp = compare_files(&src, &out, KeyRange::Standard).unwrap();
        println!(
            "{path} — normal export: same events {}; import {:.2} s; peak RSS {:.0} MB",
            cmp.same_events,
            import.as_secs_f64(),
            peak_rss_mb()
        );
        for l in cmp.lines().iter().take(12) {
            println!("    {l}");
        }
        std::fs::remove_file(&out).ok();
        drop(doc);
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// v1.0-beta1 B1-0120: which tracks of an owner pair (`PLUMSPIRIT_OWNER_PAIR=
/// "source.mid;export.mid"`) hold different event streams (block digests
/// of the ordered tokens), with their counts — the per-track record behind
/// the comparator's first difference.
#[test]
#[ignore = "needs the owner's files (PLUMSPIRIT_OWNER_PAIR)"]
fn owner_pair_tracks_that_differ() {
    let Ok(pair) = std::env::var("PLUMSPIRIT_OWNER_PAIR") else {
        println!("PLUMSPIRIT_OWNER_PAIR is not set");
        return;
    };
    let (a, b) = pair.split_once(';').unwrap();
    let sa = summarize(std::path::Path::new(a), KeyRange::Standard).unwrap();
    let sb = summarize(std::path::Path::new(b), KeyRange::Standard).unwrap();
    let differ: Vec<usize> = (0..sa.tracks.len().max(sb.tracks.len()))
        .filter(|&i| sa.tracks.get(i) != sb.tracks.get(i))
        .collect();
    println!(
        "{} of {} tracks differ: {differ:?}",
        differ.len(),
        sa.tracks.len()
    );
    for &i in differ.iter().take(20) {
        let (x, y) = (sa.tracks.get(i), sb.tracks.get(i));
        println!(
            "  track {i}: tokens {:?} / {:?}; same counts {}",
            x.map(|t| t.tokens),
            y.map(|t| t.tokens),
            x.map(|t| &t.counts) == y.map(|t| &t.counts)
        );
    }
}
