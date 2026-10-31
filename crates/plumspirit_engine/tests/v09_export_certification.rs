//! V09-S23 — MIDI export certification (contract §166–§174, §245, §285;
//! `docs/EXPORT_CERTIFICATION.md`).
//!
//! Step 1: the owner's Kiva report reproduced with an emulation of Kiva's
//! loader (`tests/common/kiva.rs`), and the velocity bytes checked exactly.
//! Step 2: the independent digest. Step 3: running / explicit status.
//! Step 4: notes of one channel and key that overlap, against a model.

mod common;

use common::kiva::{KivaNote, kiva_load};
use plumspirit_engine::midi::{ExportOptions, ExportSnapshot, WideKeys, import_bytes};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use plumspirit_engine::native::project_from_bytes;

fn fixture(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// The notes of a snapshot as Kiva holds them, sorted.
fn snapshot_notes(s: &ExportSnapshot) -> Vec<KivaNote> {
    let mut v = Vec::new();
    s.for_each_note(|n| v.push((n.start, n.end, n.key, n.channel, n.velocity)));
    v.sort_unstable();
    v
}

fn every_option() -> Vec<(String, ExportOptions)> {
    let mut v = Vec::new();
    for apply_corrections in [true, false] {
        for structure_metadata in [true, false] {
            for split_subchannels in [true, false] {
                v.push((
                    format!(
                        "corrections {apply_corrections}, structure {structure_metadata}, split {split_subchannels}"
                    ),
                    ExportOptions {
                        apply_corrections,
                        structure_metadata,
                        split_subchannels,
                        wide_keys: WideKeys::Reject,
                        explicit_status: false,
                        player_safe_start: false,
                        loudness_reference: false,
                    },
                ));
            }
        }
    }
    v
}

#[test]
fn the_supplied_fixture_loads_completely_in_the_kiva_emulation() {
    let k = kiva_load(&fixture("v09/testing_purposes_only.mid"));
    assert!(k.clean(), "{:?} {:?}", k.error, k.corrupt());
    assert_eq!((k.format, k.division, k.tracks.len()), (1, 960, 2));
    let notes = k.notes();
    assert_eq!(notes.len(), 4_160);
    assert!(notes.iter().all(|n| n.4 == 1), "every note has velocity 1");
    assert_eq!(k.tracks[0].tempos, vec![(0, 500_000)]);
}

#[test]
fn the_fixtures_exports_load_completely_in_the_kiva_emulation_with_every_option() {
    let (from_mid, _) = import_bytes(&fixture("v09/testing_purposes_only.mid")).unwrap();
    let (from_lup, _) = project_from_bytes(&fixture("v09/testing_purposes_only.lup")).unwrap();
    for (what, d) in [("imported .mid", from_mid), ("saved .lup", from_lup)] {
        for (opts, o) in every_option() {
            let s = ExportSnapshot::take(&d, o);
            let bytes = s.to_bytes().unwrap();
            let k = kiva_load(&bytes);
            assert!(k.clean(), "{what}, {opts}: {:?} {:?}", k.error, k.corrupt());
            assert_eq!(
                (k.format, k.division),
                (s.format_word(), 960),
                "{what}, {opts}"
            );
            // Kiva holds exactly the notes the export plan describes (the
            // file has no same-key overlaps, so pairing cannot differ).
            let notes = k.notes();
            assert_eq!(notes, snapshot_notes(&s), "{what}, {opts}");
            assert_eq!(notes.len(), 4_160, "{what}, {opts}");
            assert!(notes.iter().all(|n| n.4 == 1), "{what}, {opts}: velocity 1");
            let tempos: Vec<(u64, u32)> = k.tracks.iter().flat_map(|t| t.tempos.clone()).collect();
            assert_eq!(tempos, s.tempo_changes(), "{what}, {opts}");
            assert_eq!(tempos, vec![(0, 500_000)], "{what}, {opts}: 120 BPM");
        }
    }
}

/// Every Note On of a file as (channel, key, velocity) bytes, read with a
/// minimal tokenizer written here (running status included).
fn note_on_bytes(bytes: &[u8]) -> Vec<(u8, u8, u8)> {
    let mut out = Vec::new();
    let mut i = 14usize;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &bytes[i + 8..i + 8 + len];
        let mut p = 0usize;
        let mut status = 0u8;
        while p < body.len() {
            while body[p] & 0x80 != 0 {
                p += 1;
            }
            p += 1; // delta
            if body[p] & 0x80 != 0 {
                status = body[p];
                p += 1;
            }
            match status {
                0xFF => {
                    let ty = body[p];
                    p += 1;
                    let mut n = 0usize;
                    loop {
                        let b = body[p];
                        p += 1;
                        n = (n << 7) | usize::from(b & 0x7F);
                        if b & 0x80 == 0 {
                            break;
                        }
                    }
                    p += n;
                    if ty == 0x2F {
                        break;
                    }
                }
                0xF0 | 0xF7 => {
                    let mut n = 0usize;
                    loop {
                        let b = body[p];
                        p += 1;
                        n = (n << 7) | usize::from(b & 0x7F);
                        if b & 0x80 == 0 {
                            break;
                        }
                    }
                    p += n;
                }
                s if s & 0xF0 == 0xC0 || s & 0xF0 == 0xD0 => p += 1,
                s => {
                    if s & 0xF0 == 0x90 && body[p + 1] > 0 {
                        out.push((s & 0x0F, body[p], body[p + 1]));
                    }
                    p += 2;
                }
            }
        }
        i += 8 + len;
    }
    out
}

#[test]
fn velocity_bytes_1_2_63_64_126_127_are_written_exactly() {
    const VELS: [u8; 6] = [1, 2, 63, 64, 126, 127];
    let mut d = PlumDocument::new_project("velocity".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = VELS
        .iter()
        .enumerate()
        .flat_map(|(i, &v)| {
            // Each velocity on two channels and both Note Off encodings.
            [
                NoteRecord::new(
                    i as u64 * 960,
                    480,
                    NoteAttrs::new(60 + i as u8, v, 0, 0, NoteOffKind::NoteOff).unwrap(),
                ),
                NoteRecord::new(
                    i as u64 * 960,
                    480,
                    NoteAttrs::new(72 + i as u8, v, 9, 0, NoteOffKind::NoteOnZero).unwrap(),
                ),
            ]
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    for (opts, o) in every_option() {
        let s = ExportSnapshot::take(&d, o);
        let bytes = s.to_bytes().unwrap();
        let mut got = note_on_bytes(&bytes);
        got.sort_unstable();
        let mut want: Vec<(u8, u8, u8)> = VELS
            .iter()
            .enumerate()
            .flat_map(|(i, &v)| [(0, 60 + i as u8, v), (9, 72 + i as u8, v)])
            .collect();
        want.sort_unstable();
        assert_eq!(got, want, "{opts}: the velocity bytes in the file");
        // Kiva and PlumSpirit's import read the same velocities.
        let k = kiva_load(&bytes);
        assert!(k.clean(), "{opts}");
        let mut kv: Vec<u8> = k.notes().iter().map(|n| n.4).collect();
        kv.sort_unstable();
        let mut wv: Vec<u8> = want.iter().map(|w| w.2).collect();
        wv.sort_unstable();
        assert_eq!(kv, wv, "{opts}");
        let (back, _) = import_bytes(&bytes).unwrap();
        let mut bv: Vec<u8> = back
            .tracks()
            .iter()
            .flat_map(|t| {
                t.notes
                    .iter()
                    .map(|n| n.attrs.velocity())
                    .collect::<Vec<_>>()
            })
            .collect();
        bv.sort_unstable();
        assert_eq!(bv, wv, "{opts}: re-imported");
    }
}

#[test]
fn the_kiva_emulation_reproduces_the_loader_hazards_documented_in_the_reference() {
    // A non-track chunk after a track: Kiva's chunk scan cannot measure it
    // and the load fails.
    let mut f = common::smf(1, 480, &[common::TrackBytes::new().eot(0)]);
    f.extend_from_slice(b"XFIH");
    f.extend_from_slice(&4u32.to_be_bytes());
    f.extend_from_slice(&[1, 2, 3, 4]);
    let k = kiva_load(&f);
    assert!(
        k.error.as_deref().unwrap().contains("corrupt chunk"),
        "{:?}",
        k.error
    );
    // Format 2 is refused.
    let k = kiva_load(&common::smf(2, 480, &[common::TrackBytes::new().eot(0)]));
    assert_eq!(k.error.as_deref(), Some("Midi type 2 not supported"));
    // An F7 escape: Kiva reads F7 as a message without data, so its length
    // byte becomes the next delta and the track is misread.
    let mut t = common::TrackBytes::new();
    t.on(0, 0, 60, 100);
    t.ev(0, &[0xF7, 0x02, 0x7E, 0x7F]);
    t.off(480, 0, 60, 0);
    let k = kiva_load(&common::smf(1, 480, &[t.eot(480)]));
    assert!(!k.clean(), "an F7 escape derails the track");
    // A sequence-number meta of length 0 (valid MIDI) drops the rest of the
    // track in Kiva.
    let mut t = common::TrackBytes::new();
    t.meta(0, 0x00, &[]);
    t.on(0, 0, 60, 100).off(480, 0, 60, 0);
    let k = kiva_load(&common::smf(1, 480, &[t.eot(480)]));
    assert_eq!(k.corrupt(), vec![0]);
}

// ---------------------------------------------------------------------------
// Step 2: the independent digest (V09-0099, §166). The golden corpus runs
// it on every export (tests/v09_export_plan.rs); here it proves it can fail.

use common::smf_digest::{compare, digest, overlapping_keys, snapshot_digest};

/// Notes on keys 60 (one), 62 (nested overlap) and 64 (staggered overlap),
/// and a controller.
fn overlap_doc() -> PlumDocument {
    let mut d = PlumDocument::new_project("overlaps".into(), Bpm::DEFAULT, 480);
    let n = |s: u64, len: u64, key: u8, vel: u8| {
        NoteRecord::new(
            s,
            len,
            NoteAttrs::new(key, vel, 0, 0, NoteOffKind::NoteOff).unwrap(),
        )
    };
    let mut recs = vec![
        n(0, 480, 60, 100),
        n(0, 960, 62, 90),
        n(240, 240, 62, 80),
        n(0, 480, 64, 70),
        n(240, 480, 64, 60),
    ];
    recs.sort_by_key(|r| r.start_tick);
    let t = d.track_mut(0).unwrap();
    t.notes.insert_sorted_batch(&recs, false).unwrap();
    t.events
        .push(plumspirit_engine::model::RawEvent {
            tick: 120,
            anchor: 0,
            kind: plumspirit_engine::model::RawKind::Channel {
                status: 0xB0,
                data: [7, 99],
            },
        })
        .unwrap();
    d
}

#[test]
fn the_independent_digest_matches_and_names_the_ambiguous_keys() {
    let s = ExportSnapshot::take(&overlap_doc(), ExportOptions::default());
    let bytes = s.to_bytes().unwrap();
    let file = digest(&bytes, false).unwrap();
    let plan = snapshot_digest(&s);
    compare(&plan, &file).unwrap();
    assert_eq!(file.note_ons() as u64, s.note_count());
    // Keys 62 and 64 overlap themselves: Standard MIDI cannot say which
    // ending belongs to which start, and a first-in-first-out reader pairs
    // the nested key differently from the plan.
    let t = &plan.tracks[plan.tracks.len() - 1];
    let mut keys: Vec<(u8, u8)> = overlapping_keys(t).into_iter().collect();
    keys.sort_unstable();
    assert_eq!(keys, vec![(0, 62), (0, 64)]);
    // Only the nested key cannot be rebuilt first in, first out (step 4):
    // the staggered key 64 is compared note by note.
    assert_eq!(t.nested_keys, vec![(0, 62)]);
    let f = &file.tracks[file.tracks.len() - 1];
    let on62 = |v: &[common::smf_digest::DNote]| -> Vec<(u64, u64)> {
        v.iter().filter(|n| n.3 == 62).map(|n| (n.0, n.1)).collect()
    };
    assert_eq!(on62(&t.notes), vec![(0, 960), (240, 480)]);
    assert_eq!(on62(&f.notes), vec![(0, 480), (240, 960)]);
}

#[test]
fn the_independent_digest_finds_any_changed_byte() {
    let s = ExportSnapshot::take(&overlap_doc(), ExportOptions::default());
    let bytes = s.to_bytes().unwrap();
    let plan = snapshot_digest(&s);
    let find = |pat: &[u8]| bytes.windows(pat.len()).position(|w| w == pat).unwrap();
    // A controller value.
    let mut b = bytes.clone();
    let i = find(&[0xB0, 7, 99]);
    b[i + 2] = 98;
    let e = compare(&plan, &digest(&b, false).unwrap()).unwrap_err();
    assert!(e.contains("controllers"), "{e}");
    // A Note On velocity (key 60, the unambiguous one).
    let mut b = bytes.clone();
    let i = find(&[0x90, 60, 100]);
    b[i + 2] = 101;
    let e = compare(&plan, &digest(&b, false).unwrap()).unwrap_err();
    assert!(e.contains("ons"), "{e}");
    // A note ending's velocity.
    let mut b = bytes.clone();
    let i = find(&[0x80, 60, 0]);
    b[i + 2] = 1;
    let e = compare(&plan, &digest(&b, false).unwrap()).unwrap_err();
    assert!(e.contains("offs"), "{e}");
    // A data byte above 127 is a broken file for a standard reader.
    let mut b = bytes.clone();
    b[i + 1] = 0xBC;
    assert!(digest(&b, false).is_err());
}

// ---------------------------------------------------------------------------
// Step 3: running status and Maximum Compatibility (V09-0050, §069, §246).
// The golden corpus checks both modes on every export
// (tests/v09_export_plan.rs); here the supplied fixture, counted.

#[test]
fn both_status_modes_write_the_same_events_and_only_one_uses_running_status() {
    let (d, _) = import_bytes(&fixture("v09/testing_purposes_only.mid")).unwrap();
    let compact = ExportSnapshot::take(&d, ExportOptions::default());
    let explicit = ExportSnapshot::take(
        &d,
        ExportOptions {
            explicit_status: true,
            ..ExportOptions::default()
        },
    );
    let (cb, xb) = (compact.to_bytes().unwrap(), explicit.to_bytes().unwrap());
    let (cd, xd) = (digest(&cb, false).unwrap(), digest(&xb, false).unwrap());
    compare(&cd, &xd).unwrap();
    compare(&snapshot_digest(&explicit), &xd).unwrap();
    let running = |d: &common::smf_digest::Digest| -> usize {
        d.tracks.iter().map(|t| t.running_status).sum()
    };
    // The fixture's layer track repeats its status (the owner's file: 910
    // running-status events in track 2, MIDI_EXPORT_FIXTURE.md).
    assert_eq!(running(&cd), 910);
    assert_eq!(running(&xd), 0);
    assert_eq!(
        xb.len() - cb.len(),
        running(&cd),
        "one status byte per event"
    );
    assert_eq!(xd.note_ons(), 4_160);
    for b in [&cb, &xb] {
        let k = kiva_load(b);
        assert!(k.clean());
        assert_eq!(k.notes().len(), 4_160);
    }
}

// ---------------------------------------------------------------------------
// Step 4: notes of one channel and key that overlap (V09-0102, §172). A Note
// Off names a key, not a note; the policy (`midi::overlap`) and what each
// reader rule rebuilds are checked against a brute-force model of the
// written order.

use plumspirit_engine::midi::export::ExportControl;
use plumspirit_engine::midi::{SameKeyOverlaps, same_key_disclosure, validate_exported_bytes};

/// A note of the step-4 cases: (start, length, channel, key, velocity).
type N = (u64, u64, u8, u8, u8);

fn doc_of(notes: &[N]) -> PlumDocument {
    let mut d = PlumDocument::new_project("same key".into(), Bpm::DEFAULT, 480);
    let mut recs: Vec<NoteRecord> = notes
        .iter()
        .map(|&(s, len, ch, key, vel)| {
            NoteRecord::new(
                s,
                len,
                NoteAttrs::new(key, vel, ch, 0, NoteOffKind::NoteOff).unwrap(),
            )
        })
        .collect();
    // Stable: notes that start together keep the order given (the stored
    // order the writer starts from).
    recs.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

/// What a brute-force model of the written order predicts: whether a reader
/// that ends the oldest open note first (PlumSpirit's import) and one that
/// ends the newest first (Kiva) rebuild other notes, and the counts.
fn predict(notes: &[N]) -> (bool, bool, SameKeyOverlaps) {
    let mut fifo = false;
    let mut lifo = false;
    let mut o = SameKeyOverlaps::default();
    let mut keys: Vec<(u8, u8)> = notes.iter().map(|n| (n.2, n.3)).collect();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        // The written order of one channel and key: by start, notes that
        // start together shortest first (stable).
        let mut v: Vec<(u64, u64)> = notes
            .iter()
            .filter(|n| (n.2, n.3) == key)
            .map(|n| (n.0, n.0 + n.1))
            .collect();
        v.sort_by_key(|&(s, e)| (s, e));
        for j in 0..v.len() {
            let (sj, ej) = v[j];
            let mut inside = false;
            let mut nested = false;
            for &(si, ei) in &v[..j] {
                // `i` is written before `j`.
                fifo |= ej < ei;
                lifo |= sj < ei && ei < ej;
                if si < sj && sj < ei {
                    inside = true;
                    nested |= ej < ei;
                }
            }
            o.starts_inside += u64::from(inside);
            o.nested += u64::from(nested);
        }
    }
    (fifo, lifo, o)
}

/// Every note of a document: (start, end, channel, key, velocity), sorted.
fn doc_notes(d: &PlumDocument) -> Vec<(u64, u64, u8, u8, u8)> {
    let mut v: Vec<_> = d
        .tracks()
        .iter()
        .flat_map(|t| t.notes.iter())
        .map(|n| {
            (
                n.start_tick,
                n.end_tick(),
                n.attrs.channel(),
                n.attrs.pitch(),
                n.attrs.velocity(),
            )
        })
        .collect();
    v.sort_unstable();
    v
}

/// Exports `notes` and checks every reader against [`predict`]; returns
/// the disclosure.
fn check_same_key(name: &str, notes: &[N]) -> Option<String> {
    let (fifo, lifo, counts) = predict(notes);
    let d = doc_of(notes);
    let s = ExportSnapshot::take(&d, ExportOptions::default());
    let want: Vec<(usize, SameKeyOverlaps)> = if counts.any() {
        vec![(0, counts)]
    } else {
        Vec::new()
    };
    assert_eq!(s.same_key_overlaps(), want, "{name}: counts");
    let mut cur = std::io::Cursor::new(Vec::new());
    let stats = s.write(&mut cur, ExportControl::default()).unwrap();
    let bytes = cur.into_inner();
    assert_eq!(stats.same_key, want, "{name}: the writer's counts");
    // The validation pairs as PlumSpirit's import does.
    let v = validate_exported_bytes(s.document(), s.plan(), &bytes).unwrap();
    assert_eq!(v.pairing_differs(), fifo, "{name}: validation");
    // The independent reader: every event in order, every note exactly
    // unless a key holds nested notes.
    let sd = snapshot_digest(&s);
    compare(&sd, &digest(&bytes, false).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        sd.tracks.iter().any(|t| !t.nested_keys.is_empty()),
        fifo,
        "{name}: nested keys"
    );
    // Reopened in PlumSpirit: exactly the notes, unless nested.
    let (back, _) = import_bytes(&bytes).unwrap();
    let planned = doc_notes(&d);
    assert_eq!(doc_notes(&back) == planned, !fifo, "{name}: reimport");
    assert_eq!(doc_notes(&back).len(), planned.len(), "{name}: note count");
    // Kiva (newest first): exactly the notes unless staggered.
    let k = kiva_load(&bytes);
    assert!(k.clean(), "{name}");
    let mut kn = k.notes();
    kn.sort_unstable();
    assert_eq!(kn == snapshot_notes(&s), !lifo, "{name}: Kiva");
    same_key_disclosure(&stats.same_key, &v.tracks_with_different_pairing)
}

#[test]
fn same_key_overlaps_are_written_by_one_policy_and_each_reader_gets_what_the_model_says() {
    // (name, notes, PlumSpirit reopens other lengths, Kiva shows other
    // notes) — the expectation is spelled out, then the model must agree.
    let cases: Vec<(&str, Vec<N>, bool, bool)> = vec![
        (
            "nested",
            vec![(0, 960, 0, 60, 100), (240, 240, 0, 60, 90)],
            true,
            false,
        ),
        (
            "staggered",
            vec![(0, 480, 0, 60, 100), (240, 480, 0, 60, 90)],
            false,
            true,
        ),
        (
            "identical",
            vec![(0, 480, 0, 60, 100), (0, 480, 0, 60, 100)],
            false,
            false,
        ),
        (
            "together, longer stored first",
            vec![(0, 960, 0, 60, 100), (0, 240, 0, 60, 90)],
            false,
            true,
        ),
        (
            "zero length inside",
            vec![(0, 960, 0, 60, 100), (240, 0, 0, 60, 90)],
            true,
            false,
        ),
        (
            "zero length at the start",
            vec![(0, 960, 0, 60, 100), (0, 0, 0, 60, 90)],
            false,
            false,
        ),
        (
            "touching",
            vec![(0, 480, 0, 60, 100), (480, 480, 0, 60, 90)],
            false,
            false,
        ),
        (
            "ending together",
            vec![(0, 960, 0, 60, 100), (240, 720, 0, 60, 90)],
            false,
            false,
        ),
        (
            "staggered and nested",
            vec![
                (0, 1000, 0, 60, 100),
                (100, 1000, 0, 60, 90),
                (200, 50, 0, 60, 80),
            ],
            true,
            true,
        ),
        (
            "other keys and channels",
            vec![
                (0, 960, 0, 60, 100),
                (240, 240, 0, 61, 90),
                (240, 240, 1, 60, 80),
            ],
            false,
            false,
        ),
    ];
    for (name, notes, fifo, lifo) in &cases {
        let (pf, pl, _) = predict(notes);
        assert_eq!((pf, pl), (*fifo, *lifo), "{name}: the model");
        let disclosure = check_same_key(name, notes);
        assert_eq!(
            disclosure.is_some(),
            predict(notes).2.any(),
            "{name}: disclosure"
        );
    }
    // The disclosure of the nested case names what PlumSpirit reopens.
    let s = check_same_key("nested", &cases[0].1).unwrap();
    assert!(
        s.starts_with("1 note(s) in track 1 start while a note of the same key and channel"),
        "{s}"
    );
    assert!(
        s.ends_with("PlumSpirit pairs some notes of track 1 differently, so their lengths change."),
        "{s}"
    );
    let s = check_same_key("staggered", &cases[1].1).unwrap();
    assert!(s.ends_with("PlumSpirit reopens every note exactly."), "{s}");
}

#[test]
fn random_same_key_overlaps_match_the_model_for_every_reader() {
    // Few keys and channels, many overlapping notes: zero lengths, notes
    // that start together in any stored order, nested and staggered. Every
    // note's velocity is unique, so a reader that pairs a Note Off with
    // another Note On is always seen.
    let mut seed = 0x5EED_0102_u64;
    let mut rnd = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    let (mut fifo_seen, mut lifo_seen, mut clean_seen) = (0, 0, 0);
    for round in 0..300 {
        let count = 2 + rnd(40) as usize;
        let notes: Vec<N> = (0..count)
            .map(|i| {
                let start = rnd(30) * 10;
                let len = if rnd(6) == 0 { 0 } else { rnd(16) * 10 };
                (start, len, rnd(2) as u8, 60 + rnd(2) as u8, 1 + i as u8)
            })
            .collect();
        let (fifo, lifo, _) = predict(&notes);
        fifo_seen += usize::from(fifo);
        lifo_seen += usize::from(lifo);
        clean_seen += usize::from(!fifo && !lifo);
        check_same_key(&format!("round {round}"), &notes);
    }
    assert!(
        fifo_seen > 30 && lifo_seen > 30 && clean_seen > 5,
        "{fifo_seen} {lifo_seen} {clean_seen}"
    );
}
