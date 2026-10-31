//! v0.4 features at the engine level: controller events (CC / Pitch Bend) and
//! Master Volume, Track Properties with non-destructive corrections, Mute /
//! Solo, End of Track, loop and graph settings, persistence, MIDI round trips
//! and v0.3 migration. Contract parts 2, 5–8, 15, 17.

mod common;

use common::*;
use plumspirit_engine::editor::controllers::{
    self, ControllerKind, WritePolicy, all_controller_events, collect_points,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::{
    ExportOptions, export_to_bytes_with, import_bytes, plan_export, plan_export_with,
    validate_exported_bytes,
};
use plumspirit_engine::model::{
    Bpm, GatePercent, GraphTarget, OutputRoute, PlumDocument, TrackKind, TrackProps,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};

fn project(ppqn: u32) -> PlumDocument {
    PlumDocument::new_project("v0.4".into(), Bpm::DEFAULT, ppqn)
}

fn with_notes(ppqn: u32, recs: &[plumspirit_engine::model::NoteRecord]) -> PlumDocument {
    let mut d = project(ppqn);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(recs, false)
        .unwrap();
    d
}

fn roundtrip_lup(d: &PlumDocument) -> PlumDocument {
    let bytes = project_to_bytes(d).unwrap();
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    // Deterministic writer: saving the reopened project gives the same bytes.
    assert_eq!(project_to_bytes(&back).unwrap(), bytes);
    back
}

fn export_with(d: &PlumDocument, apply_corrections: bool) -> Result<Vec<u8>, String> {
    let plan = plan_export_with(
        d,
        ExportOptions {
            apply_corrections,
            ..ExportOptions::default()
        },
    );
    let bytes = export_to_bytes_with(d, &plan).map_err(|e| e.to_string())?;
    validate_exported_bytes(d, &plan, &bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn reimport(bytes: &[u8]) -> PlumDocument {
    import_bytes(bytes).unwrap().0
}

fn apply(h: &mut History, d: &mut PlumDocument, op: Operation) {
    h.apply(d, op).unwrap();
}

/// Tracks' MTrk chunk bodies.
fn track_chunks(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 14;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        if &bytes[i..i + 4] == b"MTrk" {
            out.push(&bytes[i + 8..i + 8 + len]);
        }
        i += 8 + len;
    }
    out
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn rich_props() -> TrackProps {
    TrackProps {
        default_channel: 9,
        kind: TrackKind::Rhythm,
        route: OutputRoute::External("Loopback MIDI Port ✓".into()),
        tick_correction: -60,
        correction_from_measure: 2,
        key_correction: -12,
        gate: GatePercent::parse("87.5").unwrap(),
        key_range: (24, 100),
        onion_color: Some([0x12, 0xAB, 0xFE]),
    }
}

// ------------------------------------------------------------ persistence

#[test]
fn track_properties_mute_solo_ends_master_volume_loop_and_graphs_survive_save_and_reopen() {
    let mut d = with_notes(
        480,
        &[rec(0, 480, 60), rec(1920, 480, 64), rec(3840, 240, 67)],
    );
    d.add_track().unwrap();
    let mut h = History::default();
    let id = d.track(0).unwrap().id();
    apply(
        &mut h,
        &mut d,
        Operation::TrackProps {
            track: id,
            props: rich_props(),
            end: Some(99_999),
            label: "Track Properties",
        },
    );
    let edit = controllers::prepare_rename(d.track(0).unwrap(), "Drums — Ω 夜").unwrap();
    apply(
        &mut h,
        &mut d,
        Operation::Events {
            edit,
            label: "Rename",
        },
    );
    let edit = controllers::prepare_points(
        d.track(0).unwrap(),
        ControllerKind::VOLUME,
        9,
        &[(0, 100), (480, 90), (960, 30)],
        WritePolicy::Replace,
        1000,
    )
    .unwrap();
    apply(
        &mut h,
        &mut d,
        Operation::Events {
            edit,
            label: "Curve",
        },
    );
    let edit = d
        .conductor()
        .prepare_master_points(&[(0, 16_383), (1920, 8_192), (3840, 0)], true)
        .unwrap();
    apply(
        &mut h,
        &mut d,
        Operation::Conductor {
            edit,
            label: "Master Volume",
        },
    );
    d.track_mut(0).unwrap().solo = true;
    d.track_mut(1).unwrap().mute = true;
    {
        let p = d.project_mut();
        p.looping.range = Some((1920, 3840));
        p.looping.enabled = true;
        p.graphs.slots[0].target = GraphTarget::Cc(7);
        p.graphs.slots[0].channel = 9;
        p.graphs.slots[1].visible = true;
        p.graphs.slots[1].target = GraphTarget::MasterVolume;
    }
    let back = roundtrip_lup(&d);
    let t = back.track(0).unwrap();
    assert_eq!(*t.props(), rich_props());
    assert_eq!(t.end_of_track_tick, Some(99_999));
    assert_eq!(t.name(), Some("Drums — Ω 夜"));
    assert!(t.solo && !t.mute);
    assert!(back.track(1).unwrap().mute);
    assert_eq!(
        collect_points(&t.events, ControllerKind::VOLUME, 9)
            .iter()
            .map(|p| (p.tick, p.value))
            .collect::<Vec<_>>(),
        vec![(0, 100), (480, 90), (960, 30)]
    );
    assert_eq!(
        back.conductor()
            .master_volume()
            .iter()
            .map(|p| (p.tick, p.value))
            .collect::<Vec<_>>(),
        vec![(0, 16_383), (1920, 8_192), (3840, 0)]
    );
    assert_eq!(back.project().looping, d.project().looping);
    assert_eq!(back.project().graphs, d.project().graphs);
    // Undo restores every previous value, one step at a time.
    let mut d2 = d.clone();
    for _ in 0..4 {
        h.undo(&mut d2).unwrap().expect("undo");
    }
    assert_eq!(*d2.track(0).unwrap().props(), TrackProps::default());
    assert_eq!(d2.track(0).unwrap().end_of_track_tick, None);
    assert_eq!(d2.track(0).unwrap().name(), None);
    assert!(d2.conductor().master_volume().is_empty());
    for _ in 0..4 {
        h.redo(&mut d2).unwrap().expect("redo");
    }
    assert_eq!(d2.fingerprint(), d.fingerprint());
}

#[test]
fn v03_projects_open_with_default_v04_settings_and_resave_as_v04() {
    let bytes = include_bytes!("fixtures/journey_v03.lup");
    let (d, report) = project_from_bytes(bytes).expect("v0.3 project opens");
    assert_eq!(report.migrated_from, Some(2), "v0.3 wrote .lup version 2");
    assert_eq!(d.note_count(), 6);
    assert_eq!(d.track_count(), 2);
    for t in d.tracks() {
        assert_eq!(*t.props(), TrackProps::default());
        assert!(!t.mute && !t.solo);
    }
    assert_eq!(d.project().looping, Default::default());
    assert_eq!(d.project().graphs, Default::default());
    assert!(d.conductor().master_volume().is_empty());
    // Saved again, it is a current-version project with the same content.
    let back = roundtrip_lup(&d);
    assert_eq!(back.fingerprint(), d.fingerprint());
    let (_, r2) = project_from_bytes(&project_to_bytes(&back).unwrap()).unwrap();
    assert_eq!(r2.migrated_from, None);
    assert_eq!(r2.version, plumspirit_engine::native::LUP_VERSION);
}

// ------------------------------------------------------------ corrections

#[test]
fn corrections_apply_once_on_export_and_the_original_notes_can_be_exported() {
    let mut d = with_notes(
        480,
        &[rec(0, 480, 60), rec(1920, 480, 64), rec(3840, 240, 67)],
    );
    let id = d.track(0).unwrap().id();
    let props = TrackProps {
        tick_correction: 120,
        key_correction: 12,
        gate: GatePercent::parse("50").unwrap(),
        ..TrackProps::default()
    };
    let mut h = History::default();
    apply(
        &mut h,
        &mut d,
        Operation::TrackProps {
            track: id,
            props,
            end: None,
            label: "Track Properties",
        },
    );
    // The project keeps the original notes.
    assert_eq!(notes_of(&d, 0)[0], (0, 480, 60, 100, 0));
    // Default export: corrected once.
    let corrected = reimport(&export_with(&d, true).unwrap());
    let ti = corrected
        .tracks()
        .iter()
        .position(|t| !t.notes.is_empty())
        .unwrap();
    assert_eq!(
        notes_of(&corrected, ti)
            .iter()
            .map(|n| (n.0, n.1, n.2))
            .collect::<Vec<_>>(),
        vec![(120, 240, 72), (2040, 240, 76), (3960, 120, 79)]
    );
    // Original export: unchanged notes.
    let original = reimport(&export_with(&d, false).unwrap());
    let ti = original
        .tracks()
        .iter()
        .position(|t| !t.notes.is_empty())
        .unwrap();
    assert_eq!(
        notes_of(&original, ti)
            .iter()
            .map(|n| (n.0, n.1, n.2))
            .collect::<Vec<_>>(),
        vec![(0, 480, 60), (1920, 480, 64), (3840, 240, 67)]
    );
    // Save / reopen keeps original notes + settings (never applied twice).
    let back = roundtrip_lup(&d);
    assert_eq!(notes_of(&back, 0)[0], (0, 480, 60, 100, 0));
    let again = reimport(&export_with(&back, true).unwrap());
    assert_eq!(canonical_notes(&again, 0), canonical_notes(&corrected, 0));
}

#[test]
fn a_correction_after_a_measure_uses_the_time_signatures() {
    // 3/4 then 7/8 from measure 3: measure 3 starts at 2 × 1440 = 2880.
    let mut d = with_notes(480, &[rec(0, 10, 60), rec(2879, 10, 60), rec(2880, 10, 60)]);
    let first = d.conductor().time_signatures()[0].id;
    let edit = d
        .conductor()
        .prepare_set_signature(Some(first), 0, 3, 2)
        .unwrap();
    let mut h = History::default();
    apply(&mut h, &mut d, Operation::Conductor { edit, label: "3/4" });
    let id = d.track(0).unwrap().id();
    apply(
        &mut h,
        &mut d,
        Operation::TrackProps {
            track: id,
            props: TrackProps {
                key_correction: 5,
                correction_from_measure: 3,
                ..TrackProps::default()
            },
            end: None,
            label: "Track Properties",
        },
    );
    let out = reimport(&export_with(&d, true).unwrap());
    let ti = out
        .tracks()
        .iter()
        .position(|t| !t.notes.is_empty())
        .unwrap();
    let pitches: Vec<(u64, u8)> = notes_of(&out, ti).iter().map(|n| (n.0, n.2)).collect();
    assert_eq!(pitches, vec![(0, 60), (2879, 60), (2880, 65)]);
}

#[test]
fn key_overflow_blocks_the_corrected_export_but_not_the_original() {
    let mut d = with_notes(480, &[rec(0, 480, 120), rec(480, 480, 60)]);
    let id = d.track(0).unwrap().id();
    let mut h = History::default();
    apply(
        &mut h,
        &mut d,
        Operation::TrackProps {
            track: id,
            props: TrackProps {
                key_correction: 12,
                ..TrackProps::default()
            },
            end: None,
            label: "Track Properties",
        },
    );
    let plan = plan_export(&d);
    assert!(!plan.blockers.is_empty(), "a note would leave 0–127");
    assert!(export_with(&d, true).is_err());
    let original = reimport(&export_with(&d, false).unwrap());
    assert_eq!(original.note_count(), 2);
    // The project itself is untouched.
    assert_eq!(notes_of(&d, 0)[0].2, 120);
}

// ------------------------------------------------------------ controllers

#[test]
fn controller_events_survive_midi_lup_midi_without_duplicates() {
    let track = {
        let mut t = TrackBytes::new();
        t.ev(0, &[0xB0, 1, 10]) // modulation
            .ev(0, &[0xB0, 7, 100]) // volume
            .ev(0, &[0xB0, 10, 64]) // pan
            .ev(0, &[0xB0, 74, 20]) // cutoff (ignored by the built-in synth, kept)
            .on(0, 0, 60, 100)
            .ev(240, &[0xE0, 0x00, 0x50]) // pitch bend
            .ev(240, &[0xB0, 64, 127]) // hold on
            .off(480, 0, 60, 64)
            .ev(480, &[0xB0, 64, 0]) // hold off
            .ev(960, &[0xB0, 7, 80]) // controller after the last note
            .ev(960, &[0xB1, 11, 90]); // another channel
        t.eot(960)
    };
    let src = smf(1, 480, &[track]);
    let a = reimport(&src);
    let ti = a.tracks().iter().position(|t| !t.notes.is_empty()).unwrap();
    let before = all_controller_events(&a.track(ti).unwrap().events);
    assert_eq!(before.len(), 9);
    let b = roundtrip_lup(&a);
    let c = reimport(&export_with(&b, true).unwrap());
    let ti_c = c.tracks().iter().position(|t| !t.notes.is_empty()).unwrap();
    let after = all_controller_events(&c.track(ti_c).unwrap().events);
    let key = |v: &[(usize, ControllerKind, u8, u64, u16)]| {
        let mut k: Vec<(u64, ControllerKind, u8, u16)> =
            v.iter().map(|e| (e.3, e.1, e.2, e.4)).collect();
        k.sort_by_key(|e| (e.0, format!("{:?}", e.1), e.2, e.3));
        k
    };
    assert_eq!(key(&after), key(&before), "same controllers, no duplicates");
}

#[test]
fn a_replacing_curve_touches_only_its_kind_channel_and_range() {
    let mut d = project(480);
    let mut h = History::default();
    let pts = |v: &[(u64, u16)]| v.to_vec();
    for (kind, ch, p) in [
        (
            ControllerKind::VOLUME,
            0,
            pts(&[(0, 100), (480, 90), (960, 80), (1440, 70)]),
        ),
        (ControllerKind::VOLUME, 1, pts(&[(480, 5)])),
        (ControllerKind::PAN, 0, pts(&[(480, 1)])),
    ] {
        let edit =
            controllers::prepare_points(d.track(0).unwrap(), kind, ch, &p, WritePolicy::Merge, 100)
                .unwrap();
        apply(
            &mut h,
            &mut d,
            Operation::Events {
                edit,
                label: "Seed",
            },
        );
    }
    let fp = d.fingerprint();
    // Replace CC 7 ch 1 over [400, 1000] with three new points.
    let edit = controllers::prepare_points(
        d.track(0).unwrap(),
        ControllerKind::VOLUME,
        0,
        &[(400, 10), (700, 20), (1000, 30)],
        WritePolicy::Replace,
        100,
    )
    .unwrap();
    apply(
        &mut h,
        &mut d,
        Operation::Events {
            edit,
            label: "Line Curve",
        },
    );
    let ev = &d.track(0).unwrap().events;
    let vol0: Vec<(u64, u16)> = collect_points(ev, ControllerKind::VOLUME, 0)
        .iter()
        .map(|p| (p.tick, p.value))
        .collect();
    assert_eq!(
        vol0,
        vec![(0, 100), (400, 10), (700, 20), (1000, 30), (1440, 70)]
    );
    assert_eq!(collect_points(ev, ControllerKind::VOLUME, 1).len(), 1);
    assert_eq!(collect_points(ev, ControllerKind::PAN, 0).len(), 1);
    // One undo step restores everything.
    h.undo(&mut d).unwrap();
    assert_eq!(d.fingerprint(), fp);
    // Merge at an existing tick takes it over (no duplicate at one tick).
    let edit = controllers::prepare_points(
        d.track(0).unwrap(),
        ControllerKind::VOLUME,
        0,
        &[(480, 55)],
        WritePolicy::Merge,
        100,
    )
    .unwrap();
    apply(
        &mut h,
        &mut d,
        Operation::Events {
            edit,
            label: "Merge",
        },
    );
    let n480 = d
        .track(0)
        .unwrap()
        .events
        .events()
        .iter()
        .filter(|e| e.tick == 480 && ControllerKind::VOLUME.value_of(e, 0).is_some())
        .count();
    assert_eq!(n480, 1);
    // Channel-mode messages are never drawn as curves.
    assert!(
        controllers::prepare_points(
            d.track(0).unwrap(),
            ControllerKind::Cc(123),
            0,
            &[(0, 0), (10, 0)],
            WritePolicy::Replace,
            100
        )
        .is_err()
    );
}

// ------------------------------------------------------------ master volume

#[test]
fn master_volume_round_trips_as_universal_sysex() {
    let mut d = with_notes(480, &[rec(0, 1920, 60)]);
    let mut h = History::default();
    let edit = d
        .conductor()
        .prepare_master_points(&[(0, 16_383), (960, 8_192), (1920, 1)], true)
        .unwrap();
    apply(
        &mut h,
        &mut d,
        Operation::Conductor {
            edit,
            label: "Master",
        },
    );
    let bytes = export_with(&d, true).unwrap();
    // F0 len 7F 7F 04 01 lsb msb F7: 8192 = lsb 0, msb 64.
    assert_eq!(
        count(
            &bytes,
            &[0xF0, 0x07, 0x7F, 0x7F, 0x04, 0x01, 0x00, 0x40, 0xF7]
        ),
        1
    );
    assert_eq!(count(&bytes, &[0x7F, 0x7F, 0x04, 0x01]), 3);
    let back = reimport(&bytes);
    assert_eq!(
        back.conductor()
            .master_volume()
            .iter()
            .map(|p| (p.tick, p.value))
            .collect::<Vec<_>>(),
        vec![(0, 16_383), (960, 8_192), (1920, 1)]
    );
    // A project without Master Volume writes no such SysEx.
    let plain = export_with(&with_notes(480, &[rec(0, 10, 60)]), true).unwrap();
    assert_eq!(count(&plain, &[0x7F, 0x7F, 0x04, 0x01]), 0);
}

// ------------------------------------------------------------ End of Track

/// End-of-Track tick of every track chunk (and that it is the last event).
fn eot_ticks(bytes: &[u8]) -> Vec<u64> {
    let doc = plumspirit_engine::midi::semantic::semantic_from_bytes(bytes).unwrap();
    let _ = &doc;
    track_chunks(bytes)
        .iter()
        .map(|chunk| {
            assert_eq!(
                count(chunk, &[0xFF, 0x2F, 0x00]),
                1,
                "exactly one End of Track"
            );
            assert!(chunk.ends_with(&[0xFF, 0x2F, 0x00]), "End of Track is last");
            // Sum the deltas: walk the events.
            walk_ticks(chunk)
        })
        .collect()
}

fn read_vlq(b: &[u8], i: &mut usize) -> u64 {
    let mut v = 0u64;
    loop {
        let x = b[*i];
        *i += 1;
        v = (v << 7) | u64::from(x & 0x7F);
        if x & 0x80 == 0 {
            return v;
        }
    }
}

/// Absolute tick of the last event of a track chunk.
fn walk_ticks(b: &[u8]) -> u64 {
    let (mut i, mut t, mut running) = (0usize, 0u64, 0u8);
    let mut last = 0;
    while i < b.len() {
        t += read_vlq(b, &mut i);
        last = t;
        let s = b[i];
        match s {
            0xFF => {
                i += 2;
                let n = read_vlq(b, &mut i) as usize;
                i += n;
            }
            0xF0 | 0xF7 => {
                i += 1;
                let n = read_vlq(b, &mut i) as usize;
                i += n;
            }
            _ => {
                let status = if s & 0x80 != 0 {
                    i += 1;
                    running = s;
                    s
                } else {
                    running
                };
                i += if matches!(status & 0xF0, 0xC0 | 0xD0) {
                    1
                } else {
                    2
                };
            }
        }
    }
    last
}

#[test]
fn end_of_track_keeps_trailing_silence_and_fits_to_content() {
    // Notes end at 960; the file's End of Track is at 7680 (trailing silence).
    let track = {
        let mut t = TrackBytes::new();
        t.on(0, 0, 60, 100).off(960, 0, 60, 0);
        t.eot(7680)
    };
    let d0 = reimport(&smf(1, 480, &[track]));
    let ti = d0
        .tracks()
        .iter()
        .position(|t| !t.notes.is_empty())
        .unwrap();
    assert_eq!(d0.track(ti).unwrap().end_of_track_tick, Some(7680));
    assert_eq!(d0.duration_ticks(), 7680, "the song length includes it");
    let ends = eot_ticks(&export_with(&d0, true).unwrap());
    assert!(ends.contains(&7680), "{ends:?}");
    // Save / reopen keeps it.
    let back = roundtrip_lup(&d0);
    assert_eq!(back.track(ti).unwrap().end_of_track_tick, Some(7680));
    // Fit End to Content: right after the last note end; undoable.
    let mut d = d0.clone();
    let mut h = History::default();
    let (id, props) = {
        let t = d.track(ti).unwrap();
        (t.id(), t.props().clone())
    };
    apply(
        &mut h,
        &mut d,
        Operation::TrackProps {
            track: id,
            props,
            end: None,
            label: "Fit End to Content",
        },
    );
    assert_eq!(d.track(ti).unwrap().content_end_tick(), 960);
    let ends = eot_ticks(&export_with(&d, true).unwrap());
    assert!(ends.contains(&960) && !ends.contains(&7680), "{ends:?}");
    h.undo(&mut d).unwrap();
    assert_eq!(d.track(ti).unwrap().end_of_track_tick, Some(7680));
    // A controller after the last note keeps the track alive until it.
    let mut e = project(480);
    let edit = controllers::prepare_points(
        e.track(0).unwrap(),
        ControllerKind::VOLUME,
        0,
        &[(5000, 1)],
        WritePolicy::Merge,
        10,
    )
    .unwrap();
    let mut h2 = History::default();
    apply(&mut h2, &mut e, Operation::Events { edit, label: "CC" });
    e.track_mut(0)
        .unwrap()
        .notes
        .insert(rec(0, 100, 60), false)
        .unwrap();
    assert_eq!(e.track(0).unwrap().content_end_tick(), 5000);
    let ends = eot_ticks(&export_with(&e, true).unwrap());
    assert!(ends.contains(&5000), "{ends:?}");
    // An empty track ends at 0; a long sustaining note ends at its end.
    let mut f = with_notes(480, &[rec(10, 1_000_000, 60)]);
    f.add_track().unwrap();
    let ends = eot_ticks(&export_with(&f, true).unwrap());
    assert!(ends.contains(&1_000_010), "{ends:?}");
    // Repeated export is identical.
    assert_eq!(
        export_with(&f, true).unwrap(),
        export_with(&f, true).unwrap()
    );
}

// ------------------------------------------------------------ mute / solo

#[test]
fn mute_and_solo_decide_eligibility_but_never_change_export() {
    let mut d = with_notes(480, &[rec(0, 10, 60)]);
    d.add_track().unwrap();
    d.add_track().unwrap();
    for i in 1..3 {
        d.track_mut(i)
            .unwrap()
            .notes
            .insert(rec(0, 10, 60 + i as u8), false)
            .unwrap();
    }
    use plumspirit_engine::playback::eligibility;
    assert_eq!(eligibility(&d), vec![true, true, true]);
    d.track_mut(1).unwrap().mute = true;
    assert_eq!(eligibility(&d), vec![true, false, true]);
    d.track_mut(2).unwrap().solo = true;
    assert_eq!(eligibility(&d), vec![false, false, true]);
    d.track_mut(0).unwrap().solo = true;
    assert_eq!(eligibility(&d), vec![true, false, true], "several solos");
    // Mute wins over Solo.
    d.track_mut(2).unwrap().mute = true;
    assert_eq!(eligibility(&d), vec![true, false, false]);
    // Export ignores Mute / Solo (every note is written).
    let out = reimport(&export_with(&d, true).unwrap());
    assert_eq!(out.note_count(), 3);
}
