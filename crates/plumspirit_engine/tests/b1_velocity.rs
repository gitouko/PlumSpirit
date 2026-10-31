//! v1.0-beta1 B1-0129 (owner: "velocity-1 notes play at normal loudness in a
//! MIDI player"): the bytes PlumSpirit writes carry each note's velocity
//! exactly — 1, 2, 64 and 127 — in the normal export, after a `.lup` round
//! trip and in the strict export, with Note Off or Note On 0 endings as
//! stored. (The playback side is `audio::tests::velocity_one_is_quieter...`
//! in the app.) If the file holds velocity 1, a player that sounds it at
//! normal loudness does so by its own velocity curve or SoundFont.

use plumspirit_engine::midi::error::{ImportWarning, MidiError};
use plumspirit_engine::midi::reader::{ByteSource, SmfHandler, Token, read_smf};
use plumspirit_engine::midi::strict::{StatusBytes, export_strict};
use plumspirit_engine::midi::{ExportControl, export_to_bytes, import_file};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use plumspirit_engine::native::{LupControl, load_project, save_project};

const VELOCITIES: [u8; 4] = [1, 2, 64, 127];

#[derive(Default)]
struct Ons(Vec<(u64, u8, u8)>);

impl SmfHandler for Ons {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        if let Token::Channel { tick, status, data } = t
            && status & 0xF0 == 0x90
            && data[1] > 0
        {
            self.0.push((tick, data[0], data[1]));
        }
        Ok(())
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn warning(&mut self, _: ImportWarning) {}
}

fn note_ons(bytes: &[u8]) -> Vec<(u64, u8, u8)> {
    let mut h = Ons::default();
    read_smf(
        &mut ByteSource::new(bytes, Some(bytes.len() as u64)),
        &mut h,
    )
    .unwrap();
    h.0
}

fn project() -> PlumDocument {
    let mut d = PlumDocument::new_project("velocity".into(), Bpm::DEFAULT, 480);
    let recs: Vec<NoteRecord> = VELOCITIES
        .iter()
        .enumerate()
        .flat_map(|(i, &v)| {
            [NoteOffKind::NoteOff, NoteOffKind::NoteOnZero]
                .into_iter()
                .enumerate()
                .map(move |(j, off)| {
                    NoteRecord::new(
                        (i * 2 + j) as u64 * 480,
                        240,
                        NoteAttrs::new(60 + i as u8, v, 0, 64, off).unwrap(),
                    )
                })
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

fn expected() -> Vec<(u64, u8, u8)> {
    VELOCITIES
        .iter()
        .enumerate()
        .flat_map(|(i, &v)| (0..2).map(move |j| ((i * 2 + j) as u64 * 480, 60 + i as u8, v)))
        .collect()
}

#[test]
fn every_velocity_is_written_exactly_in_every_export() {
    let d = project();
    let bytes = export_to_bytes(&d).unwrap();
    assert_eq!(note_ons(&bytes), expected());

    let dir = std::env::temp_dir().join(format!("plumspirit-b1-velocity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Through .lup and back.
    let lup = dir.join("v.lup");
    save_project(&d, &lup, LupControl::default()).unwrap();
    let (re, _) = load_project(&lup, LupControl::default()).unwrap();
    assert_eq!(note_ons(&export_to_bytes(&re).unwrap()), expected());
    // Imported, then written strictly and normally.
    let mid = dir.join("v.mid");
    std::fs::write(&mid, &bytes).unwrap();
    let (imported, _) = import_file(&mid, Default::default()).unwrap();
    let velocities: Vec<u8> = imported
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.attrs.velocity())
        .collect();
    assert_eq!(velocities, vec![1, 1, 2, 2, 64, 64, 127, 127]);
    let out = dir.join("strict.mid");
    export_strict(
        &imported,
        &out,
        StatusBytes::Running,
        ExportControl::default(),
    )
    .unwrap();
    assert_eq!(note_ons(&std::fs::read(&out).unwrap()), expected());
    assert_eq!(note_ons(&export_to_bytes(&imported).unwrap()), expected());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every Note On (velocity > 0) as `(track, tick, channel, key, velocity)`.
#[derive(Default)]
struct TrackOns {
    track: usize,
    ons: Vec<(usize, u64, u8, u8, u8)>,
}

impl SmfHandler for TrackOns {
    fn header(&mut self, _: u16, _: u16, _: u16) -> Result<(), MidiError> {
        Ok(())
    }
    fn begin_track(&mut self, i: usize) -> Result<(), MidiError> {
        self.track = i;
        Ok(())
    }
    fn token(&mut self, t: Token<'_>) -> Result<(), MidiError> {
        if let Token::Channel { tick, status, data } = t
            && status & 0xF0 == 0x90
            && data[1] > 0
        {
            self.ons
                .push((self.track, tick, status & 0x0F, data[0], data[1]));
        }
        Ok(())
    }
    fn end_track(&mut self, _: usize, _: Option<u64>) -> Result<(), MidiError> {
        Ok(())
    }
    fn extra_chunk(&mut self, _: [u8; 4], _: Vec<u8>, _: usize) -> Result<(), MidiError> {
        Ok(())
    }
    fn warning(&mut self, _: ImportWarning) {}
}

fn track_ons(bytes: &[u8]) -> Vec<(usize, u64, u8, u8, u8)> {
    let mut h = TrackOns::default();
    read_smf(
        &mut ByteSource::new(bytes, Some(bytes.len() as u64)),
        &mut h,
    )
    .unwrap();
    h.ons
}

/// The same Note Ons regardless of track order: sorted `(tick, channel, key, velocity)`.
fn music(ons: &[(usize, u64, u8, u8, u8)]) -> Vec<(u64, u8, u8, u8)> {
    let mut m: Vec<_> = ons.iter().map(|&(_, t, c, k, v)| (t, c, k, v)).collect();
    m.sort_unstable();
    m
}

fn histogram(ons: &[(usize, u64, u8, u8, u8)]) -> Vec<(u8, usize)> {
    let mut h = [0usize; 128];
    for o in ons {
        h[o.4 as usize] += 1;
    }
    (0..128u8)
        .filter(|&v| h[v as usize] > 0)
        .map(|v| (v, h[v as usize]))
        .collect()
}

/// v1.0-beta1 B1-0196: the owner's own velocity-1 test files
/// (`PLUMSPIRIT_OWNER_VELOCITY="testing purposes only.mid;testing purposes
/// only.lup" cargo test -p plumspirit_engine --test b1_velocity owner --
/// --ignored --nocapture`). The `.mid` is imported and written by the normal
/// export, after a `.lup` round trip and by the strict export: every Note On
/// keeps its tick, channel, key and velocity (strict: in the same track and
/// order). The `.lup` opens and exports with the velocities its notes hold.
#[test]
#[ignore = "needs the owner's files (PLUMSPIRIT_OWNER_VELOCITY)"]
fn owner_velocity_fixture_keeps_every_velocity() {
    let Ok(list) = std::env::var("PLUMSPIRIT_OWNER_VELOCITY") else {
        println!("PLUMSPIRIT_OWNER_VELOCITY is not set");
        return;
    };
    let dir = std::env::temp_dir().join(format!("plumspirit-b1-owner-vel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for path in list.split(';').filter(|p| !p.is_empty()) {
        let p = std::path::Path::new(path);
        let is_lup = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lup"));
        if is_lup {
            let (doc, _) = load_project(p, LupControl::default()).unwrap();
            let mut stored = [0usize; 128];
            for t in 0..doc.track_count() {
                for n in doc.track(t).unwrap().notes.iter() {
                    stored[n.attrs.velocity() as usize] += 1;
                }
            }
            let stored: Vec<(u8, usize)> = (0..128u8)
                .filter(|&v| stored[v as usize] > 0)
                .map(|v| (v, stored[v as usize]))
                .collect();
            let out = track_ons(&export_to_bytes(&doc).unwrap());
            println!(
                "{path}: stored velocities {stored:?}; exported {:?}",
                histogram(&out)
            );
            assert_eq!(histogram(&out), stored, "{path}");
            continue;
        }
        let src_bytes = std::fs::read(p).unwrap();
        let src = track_ons(&src_bytes);
        let (doc, _) = import_file(p, Default::default()).unwrap();
        let normal = track_ons(&export_to_bytes(&doc).unwrap());
        assert_eq!(music(&normal), music(&src), "{path}: normal export");
        let lup = dir.join("round.lup");
        save_project(&doc, &lup, LupControl::default()).unwrap();
        let (re, _) = load_project(&lup, LupControl::default()).unwrap();
        let round = track_ons(&export_to_bytes(&re).unwrap());
        assert_eq!(music(&round), music(&src), "{path}: through .lup");
        let strict = dir.join("strict.mid");
        export_strict(
            &doc,
            &strict,
            StatusBytes::Running,
            ExportControl::default(),
        )
        .unwrap();
        let st = track_ons(&std::fs::read(&strict).unwrap());
        assert_eq!(st, src, "{path}: strict export, same tracks and order");
        println!(
            "{path}: {} Note Ons, velocities {:?}; normal / .lup / strict exports identical",
            src.len(),
            histogram(&src)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
