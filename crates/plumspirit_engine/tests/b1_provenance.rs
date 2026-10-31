//! v1.0-beta1 B1-0121 / B1-0124 (contract §014, strict no-edit export, part 1):
//! an imported project knows — cheaply and exactly — whether it still is the
//! file it came from. The import records the file's size, hash and the
//! project's export fingerprint; the fingerprint ignores everything export
//! does not write, changes with every edit that export would write, and comes
//! back with Undo; `.lup` keeps the record (`PROV`), and a changed or missing
//! source file is reported.

use plumspirit_engine::editor::controllers::prepare_rename;
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::midi::provenance::{SourceStatus, file_digest, source_status};
use plumspirit_engine::midi::{export_to_bytes, import_bytes, import_file};
use plumspirit_engine::model::{
    Bpm, Interp, NoteAttrs, NoteOffKind, NoteRecord, OutputRoute, PlumDocument, TempoValue,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use std::path::PathBuf;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "plumspirit-b1-provenance-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A two-track MIDI file with a few hundred notes on several channels.
fn midi_bytes() -> Vec<u8> {
    let mut d = PlumDocument::new_project("provenance".into(), Bpm::DEFAULT, 480);
    d.add_track().unwrap();
    for t in 0..2 {
        let recs: Vec<NoteRecord> = (0..300u64)
            .map(|k| {
                NoteRecord::new(
                    k * 40 + t as u64 * 7,
                    90 + k % 50,
                    NoteAttrs::new(
                        30 + (k % 70) as u8,
                        1 + (k % 127) as u8,
                        (k % 3) as u8 + t as u8,
                        64,
                        NoteOffKind::NoteOff,
                    )
                    .unwrap(),
                )
            })
            .collect();
        d.track_mut(t)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    export_to_bytes(&d).unwrap()
}

fn imported(name: &str) -> (PathBuf, PathBuf, PlumDocument) {
    let d = dir(name);
    let mid = d.join("song.mid");
    std::fs::write(&mid, midi_bytes()).unwrap();
    let (doc, _) = import_file(&mid, Default::default()).unwrap();
    (d, mid, doc)
}

#[test]
fn importing_a_file_records_its_size_hash_and_export_fingerprint() {
    let (_d, mid, doc) = imported("record");
    let src = doc.project().source.clone().expect("provenance recorded");
    assert_eq!(
        src.path,
        std::path::absolute(&mid).unwrap().to_str().unwrap()
    );
    let (size, hash) = file_digest(&mid).unwrap();
    assert_eq!(size, std::fs::metadata(&mid).unwrap().len());
    assert_eq!((src.size, src.hash), (size, hash));
    assert_eq!(src.fingerprint, doc.export_fingerprint());
    assert_eq!(source_status(&doc), SourceStatus::Unchanged);

    // From memory: recorded, but no file to rewrite.
    let (mem, _) = import_bytes(&midi_bytes()).unwrap();
    let s = mem.project().source.clone().expect("recorded");
    assert!(s.path.is_empty());
    assert_eq!((s.size, s.hash), (size, hash));
    assert_eq!(source_status(&mem), SourceStatus::NotImported);
    // A new project has none.
    let new = PlumDocument::new_project("n".into(), Bpm::DEFAULT, 480);
    assert_eq!(new.project().source, None);
    assert_eq!(source_status(&new), SourceStatus::NotImported);
}

#[test]
fn the_fingerprint_ignores_selection_mute_solo_routes_and_view() {
    let (_d, _mid, mut doc) = imported("ignore");
    let fp = doc.export_fingerprint();
    doc.track_mut(0).unwrap().notes.select_all();
    {
        let t = doc.track_mut(1).unwrap();
        t.mute = true;
        t.solo = true;
    }
    let mut props = doc.track(1).unwrap().props().clone();
    props.route = OutputRoute::External("Some synth".into());
    props.onion_color = Some([1, 2, 3]);
    let (id, end) = (
        doc.track(1).unwrap().id(),
        doc.track(1).unwrap().end_of_track_tick,
    );
    let mut h = History::new(HistoryLimits::default());
    h.apply(
        &mut doc,
        Operation::TrackProps {
            track: id,
            props,
            end,
            label: "Route",
        },
    )
    .unwrap();
    doc.project_mut().name = "renamed project".into();
    assert_eq!(doc.export_fingerprint(), fp);
    assert_eq!(source_status(&doc), SourceStatus::Unchanged);
}

#[test]
fn every_kind_of_exported_edit_changes_the_fingerprint_and_undo_restores_it() {
    let (_d, _mid, mut doc) = imported("edits");
    let fp = doc.export_fingerprint();
    let mut h = History::new(HistoryLimits::default());
    let mut seen = vec![fp];
    let mut check = |doc: &mut PlumDocument, h: &mut History, what: &str| {
        let now = doc.export_fingerprint();
        assert_ne!(now, fp, "{what} must change the fingerprint");
        assert!(!seen.contains(&now), "{what}: a fingerprint seen before");
        seen.push(now);
        assert_eq!(source_status(doc), SourceStatus::Edited, "{what}");
        h.undo(doc).unwrap().expect("undone");
        assert_eq!(doc.export_fingerprint(), fp, "undo of {what}");
        assert_eq!(
            source_status(doc),
            SourceStatus::Unchanged,
            "undo of {what}"
        );
    };

    let rec = NoteRecord::new(
        5,
        10,
        NoteAttrs::new(60, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
    );
    h.apply(
        &mut doc,
        Operation::InsertNote {
            track: 0.into(),
            record: rec,
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a new note");

    doc.track_mut(1).unwrap().notes.select_all();
    h.apply(
        &mut doc,
        Operation::SetVelocity {
            track: 1.into(),
            velocity: 1,
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a velocity");
    h.apply(
        &mut doc,
        Operation::SetChannel {
            track: 1.into(),
            channel: 9,
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a channel");
    h.apply(
        &mut doc,
        Operation::Resize {
            track: 1.into(),
            delta: 1,
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a duration");

    let edit = doc
        .conductor()
        .prepare_set_tempo(
            None,
            960,
            TempoValue::Bpm(Bpm::parse("133.5").unwrap()),
            Interp::Step,
        )
        .unwrap();
    h.apply(
        &mut doc,
        Operation::Conductor {
            edit,
            label: "Tempo",
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a tempo point");

    let edit = prepare_rename(doc.track(0).unwrap(), "Lead").unwrap();
    h.apply(
        &mut doc,
        Operation::Events {
            edit,
            label: "Rename",
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a track name");

    let t = doc.track(1).unwrap();
    let mut props = t.props().clone();
    props.key_correction = 12;
    let (id, end) = (t.id(), t.end_of_track_tick);
    h.apply(
        &mut doc,
        Operation::TrackProps {
            track: id,
            props,
            end,
            label: "Props",
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "a key correction");
    let props = doc.track(1).unwrap().props().clone();
    h.apply(
        &mut doc,
        Operation::TrackProps {
            track: id,
            props,
            end: Some(1_000_000),
            label: "End",
        },
    )
    .unwrap();
    check(&mut doc, &mut h, "an explicit End of Track");

    h.apply(&mut doc, Operation::AddTrack).unwrap();
    check(&mut doc, &mut h, "a new track");
    h.apply(&mut doc, Operation::DeleteTrack { index: 0 })
        .unwrap();
    check(&mut doc, &mut h, "a deleted track");
}

#[test]
fn the_fingerprint_does_not_depend_on_how_notes_are_chunked() {
    let recs: Vec<NoteRecord> = (0..20_000u64)
        .map(|k| {
            NoteRecord::new(
                k * 3,
                5,
                NoteAttrs::new((k % 128) as u8, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            )
        })
        .collect();
    let mut a = PlumDocument::new_project("a".into(), Bpm::DEFAULT, 480);
    a.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    // The same notes in much smaller chunks, inserted in pieces.
    let mut b = PlumDocument::new_project("a".into(), Bpm::DEFAULT, 480);
    b.track_mut(0).unwrap().notes = plumspirit_engine::model::NoteStore::with_chunk_target(100);
    for part in recs.chunks(777) {
        b.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(part, false)
            .unwrap();
    }
    let (na, nb) = (&a.track(0).unwrap().notes, &b.track(0).unwrap().notes);
    assert!(
        nb.chunk_count() > 2 * na.chunk_count(),
        "{} vs {}",
        nb.chunk_count(),
        na.chunk_count()
    );
    assert_eq!(na.content_hash(), nb.content_hash());
    assert_eq!(a.export_fingerprint(), b.export_fingerprint());
    // One velocity apart: different.
    let mut c = PlumDocument::new_project("a".into(), Bpm::DEFAULT, 480);
    let mut changed = recs.clone();
    changed[12_345].attrs = NoteAttrs::new(57, 99, 0, 64, NoteOffKind::NoteOff).unwrap();
    c.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&changed, false)
        .unwrap();
    assert_ne!(c.export_fingerprint(), a.export_fingerprint());
}

#[test]
fn the_source_survives_a_lup_save_and_reopen() {
    let (d, _mid, doc) = imported("lup");
    let lup = d.join("song.lup");
    save_project(&doc, &lup, LupControl::default()).unwrap();
    let (re, _) = load_project(&lup, LupControl::default()).unwrap();
    assert_eq!(re.project().source, doc.project().source);
    assert_eq!(re.export_fingerprint(), doc.export_fingerprint());
    assert_eq!(source_status(&re), SourceStatus::Unchanged);

    // A project without a source writes no PROV section (v0.9 files stay as
    // they were).
    let mut plain = doc.clone();
    plain.project_mut().source = None;
    let lup2 = d.join("plain.lup");
    save_project(&plain, &lup2, LupControl::default()).unwrap();
    let bytes = std::fs::read(&lup2).unwrap();
    assert!(!bytes.windows(4).any(|w| w == b"PROV"));
    assert!(
        std::fs::read(&lup)
            .unwrap()
            .windows(4)
            .any(|w| w == b"PROV")
    );
    let (re2, _) = load_project(&lup2, LupControl::default()).unwrap();
    assert_eq!(re2.project().source, None);
}

#[test]
fn a_changed_or_missing_source_file_is_reported() {
    let (_d, mid, doc) = imported("changed");
    assert_eq!(source_status(&doc), SourceStatus::Unchanged);
    let mut bytes = std::fs::read(&mid).unwrap();
    let last = bytes.len() - 5;
    bytes[last] ^= 0x01;
    std::fs::write(&mid, &bytes).unwrap();
    assert_eq!(source_status(&doc), SourceStatus::SourceChanged);
    bytes[last] ^= 0x01;
    bytes.push(0);
    std::fs::write(&mid, &bytes).unwrap();
    assert_eq!(source_status(&doc), SourceStatus::SourceChanged);
    std::fs::remove_file(&mid).unwrap();
    let s = source_status(&doc);
    assert!(matches!(s, SourceStatus::SourceMissing(_)), "{s:?}");
    assert!(
        s.sentence()
            .starts_with("The original MIDI file cannot be read (")
    );
    for s in [
        SourceStatus::Unchanged,
        SourceStatus::NotImported,
        SourceStatus::Edited,
        SourceStatus::SourceChanged,
    ] {
        assert!(s.sentence().ends_with('.'));
    }
}

/// Measurement (release): the fingerprint of 1,000,000 notes and the file
/// hash of their MIDI file — the import pays both once.
#[test]
#[ignore]
fn fingerprint_cost() {
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
    let bytes = export_to_bytes(&d).unwrap();
    let mut fp = Vec::new();
    let mut fh = Vec::new();
    for _ in 0..7 {
        let t = std::time::Instant::now();
        std::hint::black_box(d.export_fingerprint());
        fp.push(t.elapsed().as_secs_f64() * 1e3);
        let t = std::time::Instant::now();
        let mut h = plumspirit_engine::midi::provenance::WordHash::new();
        h.bytes(&bytes);
        std::hint::black_box(h.finish());
        fh.push(t.elapsed().as_secs_f64() * 1e3);
    }
    fp.sort_by(f64::total_cmp);
    fh.sort_by(f64::total_cmp);
    println!(
        "export_fingerprint 1M notes: median {:.2} ms; file hash {} bytes: median {:.2} ms",
        fp[3],
        bytes.len(),
        fh[3]
    );
}
