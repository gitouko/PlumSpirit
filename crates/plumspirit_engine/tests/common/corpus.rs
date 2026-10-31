//! The golden export corpus (v0.9 V09-S16 / V09-S23 / V09-S17): saved and
//! imported projects, generated clip projects, corrections, a PPQN
//! rescale, gap fillers, layers and 256-key policies — shared by
//! `tests/v09_export_plan.rs` (golden bytes, snapshot = file, readers) and
//! `tests/v09_player.rs` (the Player's statistics).

use plumspirit_engine::editor::clips::{
    add_clip, delete_clips, duplicate_clips, move_clips, resize_clip, set_muted,
};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::midi::{
    ExportOptions, WideKeys, import_bytes, import_bytes_with, plan_export_with,
};
use plumspirit_engine::model::events::{RawEvent, RawKind};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::track::{GatePercent, TrackProps};
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument,
};
use plumspirit_engine::native::project_from_bytes;

pub fn fixture(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

pub fn lup(path: &str) -> PlumDocument {
    project_from_bytes(&fixture(path))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .0
}

pub fn mid(path: &str) -> PlumDocument {
    import_bytes(&fixture(path))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .0
}

pub fn note(start: u64, len: u64, pitch: u8, ch: u8) -> NoteRecord {
    NoteRecord::new(
        start,
        len,
        NoteAttrs::new(pitch, 90 + pitch % 30, ch, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Three tracks of notes (one muted note, a zero-length note, same-pitch
/// overlaps) and a controller ramp each, at `ppqn` (positions scale with it).
pub fn song(ppqn: u32) -> PlumDocument {
    let q = u64::from(ppqn);
    let mut d = PlumDocument::new_project("plan".into(), Bpm::parse("140").unwrap(), ppqn);
    d.add_track().unwrap();
    d.add_track().unwrap();
    for i in 0..3usize {
        let mut recs: Vec<NoteRecord> = (0..48u64)
            .map(|k| {
                note(
                    k * q / 4,
                    q / 2 + (k % 3) * q / 8,
                    36 + (k * 7 % 40) as u8 + i as u8,
                    i as u8,
                )
            })
            .collect();
        recs.push(note(3 * q, 0, 70, i as u8));
        recs.push(note(3 * q, q, 71, i as u8));
        recs.push(note(3 * q + q / 8, q, 71, i as u8));
        recs.sort_by_key(|r| r.start_tick);
        if i == 1 {
            recs[5].attrs = recs[5].attrs.with_muted(true);
        }
        let t = d.track_mut(i).unwrap();
        t.notes.insert_sorted_batch(&recs, false).unwrap();
        for k in 0..16u64 {
            t.events
                .push(RawEvent {
                    tick: k * q / 2,
                    anchor: 0,
                    kind: RawKind::Channel {
                        status: 0xB0 | i as u8,
                        data: [7, (k * 8) as u8],
                    },
                })
                .unwrap();
        }
    }
    d
}

pub fn apply(d: &mut PlumDocument, op: Operation) {
    History::default().apply(d, op).unwrap();
    d.validate_clips().unwrap();
}

pub fn primary(d: &PlumDocument, track: usize) -> ClipId {
    ClipId::Primary(d.track(track).unwrap().id())
}

/// The id of the placement added last.
pub fn last_placed(d: &PlumDocument) -> ClipId {
    d.clips()
        .explicit_placements()
        .iter()
        .map(|p| p.id)
        .filter(|id| matches!(id, ClipId::Placed(_)))
        .max_by_key(|id| match id {
            ClipId::Placed(n) => *n,
            ClipId::Primary(_) => 0,
        })
        .expect("a placed clip")
}

/// Track 1 moved later, a linked copy of it on track 3, a trimmed window of
/// track 2, an independent copy of track 2 on track 1, a muted linked copy.
pub fn clip_song(ppqn: u32) -> PlumDocument {
    let q = u64::from(ppqn);
    let mut d = song(ppqn);
    let (p0, p1) = (primary(&d, 0), primary(&d, 1));
    let op = move_clips(&d, &[(p0, 0, q)]).unwrap();
    apply(&mut d, op);
    let op = duplicate_clips(&d, &[(p0, 2, 20 * q)], true).unwrap();
    apply(&mut d, op);
    let op = resize_clip(&d, p1, q / 2, q, Some(5 * q)).unwrap();
    apply(&mut d, op);
    let op = duplicate_clips(&d, &[(p1, 0, 30 * q)], false).unwrap();
    apply(&mut d, op);
    let op = duplicate_clips(&d, &[(p0, 1, 40 * q)], true).unwrap();
    apply(&mut d, op);
    let muted = last_placed(&d);
    let op = set_muted(&d, &[muted], true).unwrap();
    apply(&mut d, op);
    assert!(!d.clips_are_identity());
    d
}

pub fn props(d: &mut PlumDocument, track: usize, props: TrackProps) {
    let track = d.track(track).unwrap().id();
    apply(
        d,
        Operation::TrackProps {
            track,
            props,
            end: None,
            label: "Track Properties",
        },
    );
}

/// Every case: (name, document).
pub fn corpus() -> Vec<(String, PlumDocument)> {
    let mut v: Vec<(String, PlumDocument)> = Vec::new();
    for name in [
        "empty_v08",
        "simple_midi_v08",
        "large_tree_v08",
        "multi_subchannel_v08",
        "cc_heavy_v08",
        "tempo_automation_v08",
        "keys256_v09",
    ] {
        v.push((name.into(), lup(&format!("v08/{name}.lup"))));
    }
    v.push((
        "testing_purposes_only.lup".into(),
        lup("v09/testing_purposes_only.lup"),
    ));
    for path in [
        "v02/imported.mid",
        "v02/imported_project_tempo.mid",
        "v02/many_tracks.mid",
        "v02/new_project.mid",
        "v08/simple_midi_v08.mid",
        "journey_v04.mid",
        "v09/testing_purposes_only.mid",
    ] {
        v.push((path.into(), mid(path)));
    }
    v.push((
        "v09/kiva_256_keys_experimental.mid".into(),
        import_bytes_with(
            &fixture("v09/kiva_256_keys_experimental.mid"),
            KeyRange::Extended256,
        )
        .unwrap()
        .0,
    ));

    // Generated clip projects.
    v.push(("song".into(), song(960)));
    v.push(("clips".into(), clip_song(960)));
    {
        // An owned clip with notes, and a deleted primary whose linked copy
        // stays.
        let mut d = clip_song(960);
        let op = add_clip(&d, 2, 50 * 960, 4 * 960).unwrap();
        apply(&mut d, op);
        let id = d.clips().contents().last().unwrap().id;
        let recs: Vec<NoteRecord> = (0..12u64)
            .map(|k| note(k * 240, 200, 80 + k as u8, 9))
            .collect();
        d.notes_of_mut(ContentRef::Owned(id))
            .unwrap()
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let p2 = primary(&d, 2);
        let op = delete_clips(&d, &[p2]).unwrap();
        apply(&mut d, op);
        v.push(("clips_owned_deleted_primary".into(), d));
    }
    {
        let mut d = clip_song(960);
        props(
            &mut d,
            0,
            TrackProps {
                tick_correction: 37,
                key_correction: 5,
                gate: GatePercent::from_hundredths(15_000).unwrap(),
                ..TrackProps::default()
            },
        );
        props(
            &mut d,
            2,
            TrackProps {
                tick_correction: -20,
                correction_from_measure: 2,
                key_range: (40, 90),
                gate: GatePercent::from_hundredths(5_000).unwrap(),
                ..TrackProps::default()
            },
        );
        v.push(("clips_corrections".into(), d));
    }
    v.push(("clips_ppqn_96000".into(), clip_song(96_000)));
    {
        // A linked copy beyond the largest delta time: gap fillers.
        let mut d = clip_song(960);
        let p0 = primary(&d, 0);
        let op = duplicate_clips(&d, &[(p0, 1, 600_000_000)], true).unwrap();
        apply(&mut d, op);
        v.push(("clips_gap_fillers".into(), d));
    }
    for name in [
        "multi_subchannel_v08",
        "tempo_automation_v08",
        "large_tree_v08",
        "keys256_v09",
    ] {
        // Clips on a saved project: layers (split subchannels), tempo ramps,
        // the track tree.
        let mut d = lup(&format!("v08/{name}.lup"));
        let n = d.track_count();
        let busiest = (0..n)
            .max_by_key(|&i| d.track(i).unwrap().notes.len())
            .unwrap();
        let p = primary(&d, busiest);
        let op = move_clips(&d, &[(p, busiest, 777)]).unwrap();
        apply(&mut d, op);
        let other = (busiest + 1) % n;
        let op = duplicate_clips(&d, &[(p, other, 50_000)], true).unwrap();
        apply(&mut d, op);
        v.push((format!("{name}+clips"), d));
    }
    v
}

/// Option combinations: corrections × structure metadata × split
/// subchannels with the default 256-key policy, and every other 256-key
/// policy (with and without corrections) when the project has notes above
/// key 127.
pub fn combos(d: &PlumDocument) -> Vec<(String, ExportOptions)> {
    let mut v = Vec::new();
    for apply_corrections in [true, false] {
        for structure_metadata in [true, false] {
            for split_subchannels in [true, false] {
                let o = ExportOptions {
                    apply_corrections,
                    structure_metadata,
                    split_subchannels,
                    wide_keys: WideKeys::Reject,
                    explicit_status: false,
                    player_safe_start: false,
                    loudness_reference: false,
                };
                v.push((
                    format!(
                        "c{}s{}l{}",
                        u8::from(apply_corrections),
                        u8::from(structure_metadata),
                        u8::from(split_subchannels)
                    ),
                    o,
                ));
            }
        }
    }
    if plan_export_with(d, ExportOptions::default()).wide.notes > 0 {
        for (name, w) in [
            ("discard", WideKeys::Discard),
            ("fold", WideKeys::Fold),
            ("clamp", WideKeys::Clamp),
            ("experimental", WideKeys::Experimental),
        ] {
            for apply_corrections in [true, false] {
                v.push((
                    format!("c{}keys={name}", u8::from(apply_corrections)),
                    ExportOptions {
                        wide_keys: w,
                        apply_corrections,
                        ..ExportOptions::default()
                    },
                ));
            }
        }
    }
    v
}
