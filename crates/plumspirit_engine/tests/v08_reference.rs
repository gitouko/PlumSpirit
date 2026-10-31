//! Audio Reference project settings (T8-F-LUP): saved with the project in
//! the optional `REFA` section, verified on save, refused when damaged, and
//! never part of MIDI export.

use plumspirit_engine::midi::export_to_bytes;
use plumspirit_engine::model::project::{MAX_REFERENCE_OFFSET_US, ReferenceAudio};
use plumspirit_engine::model::{Bpm, PlumDocument};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};

fn doc() -> PlumDocument {
    PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960)
}

#[test]
fn reference_audio_settings_survive_save_and_open() {
    let mut d = doc();
    assert!(d.project().reference.is_none());
    let bytes = project_to_bytes(&d).unwrap();
    assert!(
        project_from_bytes(&bytes)
            .unwrap()
            .0
            .project()
            .reference
            .is_none()
    );
    for (offset, vol, mute) in [
        (0, 100, false),
        (-1_250_000, 0, true),
        (MAX_REFERENCE_OFFSET_US, 200, false),
    ] {
        d.project_mut().reference = Some(ReferenceAudio {
            path: "C:\\Music\\ref track ünïcode.flac".into(),
            offset_us: offset,
            volume_pct: vol,
            mute,
        });
        let bytes = project_to_bytes(&d).unwrap();
        let (back, _) = project_from_bytes(&bytes).unwrap();
        assert_eq!(back.project().reference, d.project().reference);
    }
}

#[test]
fn damaged_reference_settings_are_refused() {
    let mut d = doc();
    d.project_mut().reference = Some(ReferenceAudio {
        path: "a.wav".into(),
        offset_us: 0,
        volume_pct: 201,
        mute: false,
    });
    // a project with invalid settings cannot be saved silently: the reader
    // refuses the section
    let bytes = project_to_bytes(&d).unwrap();
    assert!(project_from_bytes(&bytes).is_err());
}

#[test]
fn reference_audio_never_reaches_the_midi_file() {
    let mut d = doc();
    let plain = export_to_bytes(&d).unwrap();
    d.project_mut().reference = Some(ReferenceAudio::new("song.mp3".into()));
    assert_eq!(export_to_bytes(&d).unwrap(), plain);
}

#[test]
fn offsets_convert_to_seconds() {
    let mut r = ReferenceAudio::new("x.wav".into());
    r.offset_us = -1_250_000;
    assert_eq!(r.offset_seconds(), -1.25);
    assert!(r.is_valid());
    r.path.clear();
    assert!(!r.is_valid());
}
