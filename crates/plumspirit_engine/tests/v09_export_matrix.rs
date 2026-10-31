//! V09-S23 step 5 — the reader matrix over an adverse set (V09-0099,
//! contract §166, §170, §171, §173, §174, §296; `docs/EXPORT_CERTIFICATION.md`).
//!
//! "No single oracle": every adverse project is exported in both status
//! modes and read back by PlumSpirit's import (and re-export), the
//! independent Rust reader (`common::smf_digest`), the Kiva loader emulation
//! (`common::kiva`) and a second standard parser, Python's mido
//! (`tools/mido_check.py`; a test tool, skipped when not installed unless
//! `PLUMSPIRIT_REQUIRE_MIDO=1`). Malformed input never panics any reader.
//! The golden corpus gets the same treatment in `tests/v09_export_plan.rs`.

mod common;

use common::TrackBytes;
use common::kiva::kiva_load;
use common::mido::{digest_lines, mido_lines, scratch_dir};
use common::smf_digest::{compare, digest, snapshot_digest};
use plumspirit_engine::midi::semantic::semantic_from_bytes_with;
use plumspirit_engine::midi::{
    ExportOptions, ExportSnapshot, export_to_bytes, import_bytes, validate_exported_bytes,
};
use plumspirit_engine::model::events::{RawEvent, RawKind};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use plumspirit_engine::native::project_from_bytes;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn fixture(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn note(start: u64, len: u64, ch: u8, key: u8, vel: u8, off_vel: u8, on0: bool) -> NoteRecord {
    let kind = if on0 {
        NoteOffKind::NoteOnZero
    } else {
        NoteOffKind::NoteOff
    };
    NoteRecord::new(
        start,
        len,
        NoteAttrs::new(key, vel, ch, off_vel, kind).unwrap(),
    )
}

fn put(d: &mut PlumDocument, track: usize, mut recs: Vec<NoteRecord>) {
    recs.sort_by_key(|r| r.start_tick);
    d.track_mut(track)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
}

fn channel(tick: u64, status: u8, a: u8, b: u8) -> RawEvent {
    RawEvent {
        tick,
        anchor: 0,
        kind: RawKind::Channel {
            status,
            data: [a, b],
        },
    }
}

fn project() -> PlumDocument {
    PlumDocument::new_project("adverse".into(), Bpm::DEFAULT, 960)
}

/// A file a project imported from: SysEx with its terminator, a SysEx
/// escape (`F7`), system messages, text, marker, key signature, SMPTE offset,
/// a Sequencer-Specific and an unlisted meta type, Note-On-zero endings, an
/// unknown chunk after the tracks — everything export must preserve.
fn hazard_file() -> Vec<u8> {
    let mut t0 = TrackBytes::new();
    t0.meta(0, 0x03, b"conductor")
        .meta(0, 0x51, &[0x07, 0xA1, 0x20])
        .meta(0, 0x58, &[3, 3, 24, 8])
        .meta(0, 0x59, &[0xFE, 1])
        .meta(0, 0x54, &[0x61, 0, 0, 0, 0])
        .meta(480, 0x06, b"verse");
    let t0 = t0.eot(960);
    let mut t1 = TrackBytes::new();
    t1.meta(0, 0x01, b"text")
        .sysex(0, &[0x7E, 0x7F, 0x09, 0x01, 0xF7])
        .ev(10, &[0xF7, 0x02, 0x43, 0x10])
        .ev(20, &[0xF8])
        .ev(30, &[0xF2, 0x10, 0x20])
        .meta(40, 0x7F, &[0x00, 0x00, 0x41, 1, 2])
        .meta(50, 0x60, &[9, 9])
        .on(100, 2, 60, 1)
        .ev(110, &[0x92, 62, 127])
        .ev(200, &[0x92, 60, 0])
        .off(300, 2, 62, 127)
        .ev(300, &[0xC2, 5])
        .ev(310, &[0xD2, 40])
        .ev(320, &[0xA2, 62, 30]);
    let t1 = t1.eot(400);
    let mut bytes = common::smf(1, 480, &[t0, t1]);
    bytes.extend(b"XFIH");
    bytes.extend(3u32.to_be_bytes());
    bytes.extend([1, 2, 3]);
    bytes
}

/// The adverse set (§296): (name, project).
fn adverse() -> Vec<(String, PlumDocument)> {
    let mut v: Vec<(String, PlumDocument)> = Vec::new();
    v.push(("zero notes".into(), project()));
    {
        let mut d = project();
        d.track_mut(0)
            .unwrap()
            .events
            .push(channel(480, 0xB0, 7, 100))
            .unwrap();
        v.push(("a controller and no notes".into(), d));
    }
    {
        let mut d = project();
        put(&mut d, 0, vec![note(0, 480, 0, 60, 100, 0, false)]);
        v.push(("one note".into(), d));
    }
    {
        // Velocity 1 and 127, on the first and last channel, ended by Note
        // Off velocity 0 / 127 and by Note On velocity 0.
        let mut d = project();
        let mut recs = Vec::new();
        for (i, &(vel, off, on0)) in [
            (1, 0, false),
            (127, 127, false),
            (1, 0, true),
            (127, 0, true),
        ]
        .iter()
        .enumerate()
        {
            for ch in [0, 15] {
                recs.push(note(i as u64 * 240, 120, ch, 21 + i as u8, vel, off, on0));
            }
        }
        put(&mut d, 0, recs);
        v.push(("velocity 1 and 127".into(), d));
    }
    {
        // Same-key overlaps (step 4): nested, staggered, starting together
        // stored longer first, a zero-length note inside a longer one.
        let mut d = project();
        put(
            &mut d,
            0,
            vec![
                note(0, 960, 0, 60, 100, 0, false),
                note(240, 240, 0, 60, 90, 0, false),
                note(0, 480, 0, 62, 80, 0, false),
                note(240, 480, 0, 62, 70, 0, false),
                note(1000, 960, 0, 64, 60, 0, false),
                note(1000, 240, 0, 64, 50, 0, false),
                note(2000, 960, 0, 65, 40, 0, false),
                note(2100, 0, 0, 65, 30, 0, false),
            ],
        );
        v.push(("same-key overlaps".into(), d));
    }
    {
        // Many tracks: 300 (more than one file of 256 chunks would need),
        // every channel, a program change each.
        let mut d = project();
        for _ in 1..300 {
            d.add_track().unwrap();
        }
        for i in 0..300usize {
            let ch = (i % 16) as u8;
            put(
                &mut d,
                i,
                (0..4u64)
                    .map(|k| {
                        note(
                            k * 120 + i as u64,
                            100,
                            ch,
                            30 + (i % 60) as u8,
                            64,
                            0,
                            false,
                        )
                    })
                    .collect(),
            );
            d.track_mut(i)
                .unwrap()
                .events
                .push(channel(0, 0xC0 | ch, (i % 128) as u8, 0))
                .unwrap();
        }
        v.push(("300 tracks".into(), d));
    }
    {
        // Controller-heavy: every kind of channel message, many at the same
        // ticks as notes.
        let mut d = project();
        put(
            &mut d,
            0,
            (0..2_000u64)
                .map(|k| note(k * 30, 25, (k % 3) as u8, 40 + (k % 50) as u8, 90, 0, false))
                .collect(),
        );
        let ev = &mut d.track_mut(0).unwrap().events;
        for k in 0..20_000u64 {
            let ch = (k % 3) as u8;
            let e = match k % 5 {
                0 => channel(k * 3, 0xB0 | ch, (k % 120) as u8, (k % 128) as u8),
                1 => channel(k * 3, 0xE0 | ch, (k % 128) as u8, (k / 7 % 128) as u8),
                2 => channel(k * 3, 0xC0 | ch, (k % 128) as u8, 0),
                3 => channel(k * 3, 0xD0 | ch, (k % 128) as u8, 0),
                _ => channel(k * 3, 0xA0 | ch, 40 + (k % 50) as u8, (k % 128) as u8),
            };
            ev.push(e).unwrap();
        }
        v.push(("controller-heavy".into(), d));
    }
    {
        // Long duration and deltas beyond a Standard MIDI delta time
        // (0x0FFFFFFF ticks): gap fillers.
        let mut d = project();
        put(
            &mut d,
            0,
            vec![
                note(0, 400_000_000, 0, 48, 100, 0, false),
                note(10, 20, 0, 50, 100, 0, false),
                note(900_000_000, 960, 0, 52, 100, 0, false),
            ],
        );
        d.track_mut(0)
            .unwrap()
            .events
            .push(channel(700_000_000, 0xB0, 64, 127))
            .unwrap();
        v.push(("long notes and large deltas".into(), d));
    }
    for (name, path) in [
        (
            "16 subchannels in one track",
            "v08/multi_subchannel_v08.lup",
        ),
        ("tempo automation", "v08/tempo_automation_v08.lup"),
    ] {
        v.push((name.into(), project_from_bytes(&fixture(path)).unwrap().0));
    }
    v.push((
        "imported hazards".into(),
        import_bytes(&hazard_file()).unwrap().0,
    ));
    {
        // SMPTE time (25 frames × 40 ticks per second), Format 0.
        let mut t = TrackBytes::new();
        t.on(0, 0, 60, 100).off(1000, 0, 60, 0);
        let bytes = common::smf(0, 0xE728, &[t.eot(1000)]);
        v.push((
            "SMPTE division, Format 0".into(),
            import_bytes(&bytes).unwrap().0,
        ));
    }
    v
}

fn options() -> Vec<(&'static str, ExportOptions)> {
    let mut v = Vec::new();
    for explicit_status in [false, true] {
        for split_subchannels in [false, true] {
            v.push((
                match (explicit_status, split_subchannels) {
                    (false, false) => "compact",
                    (true, false) => "explicit",
                    (false, true) => "compact, layers",
                    (true, true) => "explicit, layers",
                },
                ExportOptions {
                    explicit_status,
                    split_subchannels,
                    ..ExportOptions::default()
                },
            ));
        }
    }
    v
}

/// Notes as (start, end, channel, key, velocity, off velocity, Note On 0),
/// sorted.
fn file_notes(d: &common::smf_digest::Digest) -> Vec<common::smf_digest::DNote> {
    let mut v: Vec<_> = d.tracks.iter().flat_map(|t| t.notes.clone()).collect();
    v.sort_unstable();
    v
}

#[test]
fn every_adverse_export_reads_the_same_in_every_reader() {
    let dir = scratch_dir("matrix");
    let mut files = Vec::new();
    let mut checked = 0;
    for (name, d) in adverse() {
        for (opts, o) in options() {
            let what = format!("{name} ({opts})");
            let s = ExportSnapshot::take(&d, o);
            let bytes = s
                .to_bytes()
                .unwrap_or_else(|e| panic!("{what}: {}", e.user_message()));
            // The writer's own check.
            validate_exported_bytes(s.document(), s.plan(), &bytes)
                .unwrap_or_else(|e| panic!("{what}: {}", e.user_message()));
            // The independent reader.
            let fd = digest(&bytes, false).unwrap_or_else(|e| panic!("{what}: {e}"));
            let sd = snapshot_digest(&s);
            compare(&sd, &fd).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(fd.note_ons() as u64, s.note_count(), "{what}");
            assert!(
                fd.tracks
                    .iter()
                    .all(|t| t.unended.is_empty() && t.unmatched_offs.is_empty()),
                "{what}: every Note On is ended and every ending has a note"
            );
            if o.explicit_status {
                assert!(fd.tracks.iter().all(|t| t.running_status == 0), "{what}");
            }
            // PlumSpirit's import: the same notes (first in, first out),
            // and exporting what it read gives the same file content.
            let (back, _) = import_bytes(&bytes).unwrap_or_else(|e| panic!("{what}: {e}"));
            let again = export_to_bytes(&back).unwrap();
            let ad = digest(&again, false).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(file_notes(&ad), file_notes(&fd), "{what}: reimported notes");
            let sem = |b: &[u8]| semantic_from_bytes_with(b, false).unwrap();
            let (a, b) = (sem(&bytes), sem(&again));
            assert_eq!(
                (a.format, a.division, a.total_notes()),
                (b.format, b.division, b.total_notes()),
                "{what}: reimport header and notes"
            );
            // Kiva's loader: the whole file, every note — except the
            // documented hazards an imported file brought (KIVA.md).
            let k = kiva_load(&bytes);
            if name == "imported hazards" {
                assert!(
                    !k.clean(),
                    "{what}: Kiva misreads F7 escapes and system data"
                );
            } else {
                assert!(k.clean(), "{what}: {:?} {:?}", k.error, k.corrupt());
                assert_eq!(k.notes().len() as u64, s.note_count(), "{what}");
            }
            let path = dir.join(format!("{}.mid", files.len()));
            std::fs::write(&path, &bytes).unwrap();
            files.push((what, path, digest_lines(&fd)));
            checked += 1;
        }
    }
    assert_eq!(checked, 12 * 4);
    // The second standard parser.
    let paths: Vec<_> = files.iter().map(|f| f.1.clone()).collect();
    if let Some(got) = mido_lines(&paths) {
        for ((what, _, want), got) in files.iter().zip(&got) {
            if got != want {
                let first = got
                    .lines()
                    .zip(want.lines())
                    .find(|(a, b)| a != b)
                    .map(|(a, b)| format!("mido {a:?} vs digest {b:?}"))
                    .unwrap_or_else(|| format!("length {} vs {}", got.len(), want.len()));
                panic!("{what}: {first}");
            }
        }
        eprintln!(
            "mido read all {} adverse exports like the digest",
            files.len()
        );
        // The disagreement that found a mido bug: as shipped, mido 1.3.3
        // drops the delta time of an unknown meta type (the imported file's
        // `FF 60` at tick 50), so the events after it come 10 ticks early.
        let hazards: Vec<_> = files
            .iter()
            .filter(|f| f.0.starts_with("imported hazards"))
            .collect();
        let raw = common::mido::mido_lines_with(
            &hazards.iter().map(|f| f.1.clone()).collect::<Vec<_>>(),
            true,
        )
        .unwrap();
        for ((what, _, want), raw) in hazards.iter().zip(&raw) {
            assert!(raw.contains("on 90 2 60 1"), "{what}: {raw}");
            assert!(want.contains("on 100 2 60 1"), "{what}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Malformed MIDI input that every reader must refuse or read without a
/// panic: (name, bytes).
fn malformed() -> Vec<(&'static str, Vec<u8>)> {
    let good = {
        let mut t = TrackBytes::new();
        t.on(0, 0, 60, 100).off(96, 0, 60, 0);
        t.eot(96)
    };
    let file =
        |format: u16, division: u16, tracks: &[Vec<u8>]| common::smf(format, division, tracks);
    let mut v: Vec<(&'static str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("header only", b"MThd".to_vec()),
        ("header length 5", {
            let mut b = file(1, 96, std::slice::from_ref(&good));
            b[7] = 5;
            b
        }),
        ("two tracks declared, one present", {
            let mut b = file(1, 96, std::slice::from_ref(&good));
            b[11] = 2;
            b
        }),
        ("track length past the end", {
            let mut b = file(1, 96, std::slice::from_ref(&good));
            b[21] = 0xFF;
            b
        }),
        ("truncated in an event", {
            let mut b = file(1, 96, std::slice::from_ref(&good));
            b.truncate(b.len() - 5);
            b
        }),
        (
            "five-byte delta time",
            file(
                1,
                96,
                &[[
                    vec![0x81, 0x81, 0x81, 0x81, 0x01, 0x90, 60, 1],
                    good.clone(),
                ]
                .concat()],
            ),
        ),
        (
            "running status first",
            file(1, 96, &[[vec![0x00, 60, 100], good.clone()].concat()]),
        ),
        (
            "data byte above 127",
            file(
                1,
                96,
                &[[vec![0x00, 0x90, 200, 100], good.clone()].concat()],
            ),
        ),
        (
            "meta length past the end",
            file(1, 96, &[vec![0x00, 0xFF, 0x01, 0x7F, b'a']]),
        ),
        (
            "SysEx length past the end",
            file(1, 96, &[vec![0x00, 0xF0, 0x7F, 1, 2]]),
        ),
        (
            "no End of Track",
            file(1, 96, &[vec![0x00, 0x90, 60, 100, 0x10, 0x80, 60, 0]]),
        ),
        (
            "events after End of Track",
            file(1, 96, &[[good.clone(), vec![0x00, 0x90, 61, 1]].concat()]),
        ),
        (
            "undefined status F4",
            file(1, 96, &[[vec![0x00, 0xF4], good.clone()].concat()]),
        ),
        ("Format 2", file(2, 96, &[good.clone(), good.clone()])),
        ("Format 7", file(7, 96, std::slice::from_ref(&good))),
        ("division 0", file(1, 0, std::slice::from_ref(&good))),
        ("zero tracks", file(1, 96, &[])),
        (
            "a RIFF wrapper",
            [
                b"RIFF".to_vec(),
                vec![0; 8],
                file(1, 96, std::slice::from_ref(&good)),
            ]
            .concat(),
        ),
        ("a chunk that is not MTrk first", {
            let mut b = file(1, 96, &[]);
            b[11] = 1;
            b.extend(b"JUNK");
            b.extend(2u32.to_be_bytes());
            b.extend([0, 0]);
            b.extend(b"MTrk");
            b.extend((good.len() as u32).to_be_bytes());
            b.extend(&good);
            b
        }),
    ];
    let mut seed = 0x0099_u64;
    let mut garbage = Vec::new();
    for _ in 0..512 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        garbage.push((seed >> 56) as u8);
    }
    v.push(("random bytes", garbage.clone()));
    v.push((
        "random bytes after a header",
        [file(1, 96, &[]), garbage].concat(),
    ));
    v
}

/// Runs every reader on `bytes`; a panic is the failure.
fn no_reader_panics(what: &str, bytes: &[u8]) {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = import_bytes(bytes);
        let _ = plumspirit_engine::midi::import_bytes_with(
            bytes,
            plumspirit_engine::model::project::KeyRange::Extended256,
        );
        let _ = digest(bytes, false);
        let _ = digest(bytes, true);
        let _ = kiva_load(bytes);
        let _ = semantic_from_bytes_with(bytes, false);
    }));
    assert!(r.is_ok(), "{what}: a reader panicked");
}

#[test]
fn malformed_input_never_panics_any_reader() {
    let cases = malformed();
    for (name, bytes) in &cases {
        no_reader_panics(name, bytes);
        // PlumSpirit refuses or reads with a warning; it never invents.
        if let Ok((d, summary)) = import_bytes(bytes) {
            for t in d.tracks() {
                t.notes.validate().unwrap();
            }
            let _ = summary;
        }
    }
    // Mutations of two adverse exports (overlaps, controllers): truncation,
    // flipped, inserted and removed bytes.
    let sets = adverse();
    let bases: Vec<Vec<u8>> = sets
        .iter()
        .filter(|(n, _)| n == "same-key overlaps" || n == "imported hazards")
        .map(|(_, d)| export_to_bytes(d).unwrap())
        .collect();
    assert_eq!(bases.len(), 2);
    let mut seed = 0x5EED_0099_u64;
    let mut rnd = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n.max(1)
    };
    let mut mutated = 0;
    for base in &bases {
        for _ in 0..1_500 {
            let mut f = base.clone();
            let i = rnd(f.len() as u64) as usize;
            match rnd(4) {
                0 => f.truncate(i),
                1 => f[i] = rnd(256) as u8,
                2 => f.insert(i, rnd(256) as u8),
                _ => {
                    f.remove(i);
                }
            }
            no_reader_panics("mutation", &f);
            mutated += 1;
        }
    }
    assert_eq!(mutated, 3_000);
    // The second standard parser refuses the broken files too (and does
    // not crash): every malformed case gets an `error` line or a reading.
    let dir = scratch_dir("malformed");
    let paths: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(i, (_, b))| {
            let p = dir.join(format!("{i}.mid"));
            std::fs::write(&p, b).unwrap();
            p
        })
        .collect();
    let mido = mido_lines(&paths);
    // What each reader does with each case (printed for
    // docs/EXPORT_CERTIFICATION.md; run with --nocapture).
    eprintln!("| case | PlumSpirit import | independent reader | Kiva emulation | mido |");
    eprintln!("|---|---|---|---|---|");
    for (i, (name, bytes)) in cases.iter().enumerate() {
        let ps = match import_bytes(bytes) {
            Err(e) => format!("refused: {:?}", e.kind),
            Ok((_, s)) if s.warnings.is_empty() => "read".into(),
            Ok((_, s)) => format!("read, {} warning(s)", s.warnings.len()),
        };
        let dg = match digest(bytes, false) {
            Err(_) => "refused",
            Ok(_) => "read",
        };
        let k = kiva_load(bytes);
        let kv = if let Some(e) = &k.error {
            format!("fails: {e}")
        } else if k.clean() {
            "loads".into()
        } else {
            "track(s) cut short".into()
        };
        let md = mido.as_ref().map_or("-".into(), |m| {
            if m[i].starts_with("error ") {
                "refused".to_string()
            } else {
                "read".to_string()
            }
        });
        eprintln!("| {name} | {ps} | {dg} | {kv} | {md} |");
    }
    if let Some(got) = &mido {
        let refused = got.iter().filter(|g| g.starts_with("error ")).count();
        eprintln!("mido refused {refused} of {} malformed files", got.len());
        assert!(refused * 2 > got.len(), "mido refuses most malformed files");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
