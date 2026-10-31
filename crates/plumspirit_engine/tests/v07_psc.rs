//! `.psc` score files (TEST_MATRIX T-PSC-*).

use plumspirit_engine::codec::{Crc32, put_uvarint};
use plumspirit_engine::transform::TNote;
use plumspirit_engine::transform::psc::*;

fn note(start: u64, dur: u64, pitch: u8, sub: u32, track: u16, group: u32) -> TNote {
    TNote {
        start,
        dur,
        sub,
        group,
        track,
        pitch,
        vel: 90,
        rel: 10,
        ch: (sub % 16) as u8,
        flags: 0,
    }
}

fn score_of(notes: &[TNote]) -> Score {
    Score::from_notes(notes, 960, |t, id| {
        ((id % 16) as u8, Some(format!("T{t} S{id}")))
    })
}

/// Rewrites the CRC so a crafted body passes the checksum.
fn reseal(mut bytes: Vec<u8>) -> Vec<u8> {
    let at = bytes.len() - 12;
    bytes.truncate(at);
    let crc = Crc32::of(&bytes);
    bytes.extend_from_slice(b"CRC ");
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn one_note_and_many_notes_roundtrip_byte_identically() {
    let one = score_of(&[note(500, 120, 60, 0, 0, 0)]);
    assert_eq!(one.notes[0].start, 0, "timing is relative");
    let b = one.to_bytes();
    let back = Score::from_bytes(&b).unwrap();
    assert_eq!(back.notes, one.notes);
    assert_eq!(back.to_bytes(), b);

    let many: Vec<TNote> = (0..50_000u64)
        .map(|i| {
            note(
                1000 + i * 7,
                1 + i % 500,
                (i % 128) as u8,
                (i % 5) as u32,
                0,
                0,
            )
        })
        .collect();
    let s = score_of(&many);
    let mut b = s.to_bytes();
    for _ in 0..100 {
        let back = Score::from_bytes(&b).unwrap();
        let again = back.to_bytes();
        assert_eq!(again, b, "repeated roundtrip is byte-identical");
        b = again;
    }
    assert!(b.len() < many.len() * 12, "compact: {} bytes", b.len());
}

#[test]
fn multiple_subchannels_groups_and_tracks_are_preserved() {
    let notes = vec![
        note(0, 10, 60, 3, 0, 7),
        note(0, 10, 64, 9, 0, 7),
        note(5, 10, 67, 3, 1, 8),
        note(9, 10, 70, 0, 1, 0),
    ];
    let s = score_of(&notes);
    // Distinct (track, sub) pairs become distinct score subchannels.
    assert_eq!(s.subs.len(), 4);
    assert_eq!(
        s.groups.iter().map(|g| g.id).collect::<Vec<_>>(),
        vec![7, 8]
    );
    let back = Score::from_bytes(&s.to_bytes()).unwrap();
    assert_eq!(back.subs, s.subs);
    assert_eq!(back.subs[0].name.as_deref(), Some("T0 S3"));
    let distinct: std::collections::BTreeSet<u32> = back.notes.iter().map(|n| n.sub).collect();
    assert_eq!(distinct.len(), 4, "colours never flattened");
}

#[test]
fn automation_lanes_roundtrip() {
    let mut s = score_of(&[note(0, 10, 60, 0, 0, 0)]);
    s.lanes.push(ScoreLane {
        kind: LaneKind::Cc(10),
        channel: 2,
        points: vec![(0, 0), (240, 64), (480, 127)],
    });
    s.lanes.push(ScoreLane {
        kind: LaneKind::PitchBend,
        channel: 0,
        points: vec![(0, 8192), (960, 16383)],
    });
    let back = Score::from_bytes(&s.to_bytes()).unwrap();
    assert_eq!(back.lanes.len(), 2);
    assert_eq!(back.length(), 960);
    // A lane-only score (Save Automation As) is valid too.
    let lane_only = Score {
        ppq: 480,
        lanes: vec![ScoreLane {
            kind: LaneKind::Velocity,
            channel: 0,
            points: vec![(0, 1), (10, 127)],
        }],
        ..Score::default()
    };
    assert_eq!(
        Score::from_bytes(&lane_only.to_bytes()).unwrap().lanes,
        lane_only.lanes
    );
}

#[test]
fn malformed_files_are_refused_safely() {
    let good = score_of(&[note(0, 10, 60, 0, 0, 0), note(5, 10, 61, 1, 0, 0)]).to_bytes();
    assert!(Score::from_bytes(b"MThd").is_err());
    assert!(Score::from_bytes(&[]).is_err());
    let mut magic = good.clone();
    magic[0] = b'X';
    assert!(
        Score::from_bytes(&magic)
            .unwrap_err()
            .contains("not a PlumSpirit score")
    );
    // Truncated at every length: never panics, never accepted.
    for n in 0..good.len() {
        assert!(Score::from_bytes(&good[..n]).is_err(), "truncated at {n}");
    }
    // Any flipped byte is caught (CRC or structure).
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0x41;
        assert!(Score::from_bytes(&b).is_err(), "flip at {i}");
    }
    // A newer version is refused with a sentence.
    let mut v = good.clone();
    v[8] = 9;
    let v = reseal(v);
    assert!(Score::from_bytes(&v).unwrap_err().contains("newer"));
}

#[test]
fn huge_declared_counts_are_refused_before_allocation() {
    // A NOTE section claiming a billion notes in 3 bytes, with a valid CRC.
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&960u32.to_le_bytes());
    let mut p = Vec::new();
    put_uvarint(&mut p, 1_000_000_000);
    b.extend_from_slice(b"NOTE");
    b.extend_from_slice(&(p.len() as u32).to_le_bytes());
    b.extend_from_slice(&p);
    b.extend_from_slice(b"CRC ");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    let b = reseal(b);
    assert!(Score::from_bytes(&b).unwrap_err().contains("damaged"));
    // Same for lanes and subchannels.
    for tag in [b"AUTO", b"SUBS", b"GRPS", b"META"] {
        let mut c = b.clone();
        c[16..20].copy_from_slice(tag);
        let c = reseal(c);
        assert!(Score::from_bytes(&c).is_err());
    }
}

#[test]
fn unknown_sections_are_skipped_for_forward_compatibility() {
    let good = score_of(&[note(0, 10, 60, 0, 0, 0)]).to_bytes();
    let mut b = good[..16].to_vec();
    b.extend_from_slice(b"ZZZZ");
    b.extend_from_slice(&3u32.to_le_bytes());
    b.extend_from_slice(&[1, 2, 3]);
    b.extend_from_slice(&good[16..]);
    let b = reseal(b);
    assert_eq!(Score::from_bytes(&b).unwrap().notes.len(), 1);
}

#[test]
fn placing_a_score_keeps_relative_timing_rescales_and_maps_subchannels() {
    let notes = vec![
        note(1000, 480, 60, 1, 0, 0),
        note(1480, 240, 64, 2, 0, 0),
        note(1960, 960, 67, 1, 0, 0),
    ];
    let s = score_of(&notes); // ppq 960
    // Destination at 480 PPQN, subchannels (0 ch 0), (5 ch 2).
    let dest = [(0u32, 0u8), (5, 2)];
    let plan = plan_subs(&s, &dest, SubMapping::ByChannel);
    assert_eq!(
        plan.map,
        vec![6, 5],
        "channel 1 created as id 6, channel 2 matched to id 5"
    );
    assert_eq!(plan.create, vec![(6, 1, Some("T0 S1".into()))]);
    let placed = place(&s, 10_000, 480, 2, &plan, Default::default()).unwrap();
    assert_eq!(
        placed
            .iter()
            .map(|n| (n.start, n.dur, n.pitch, n.sub))
            .collect::<Vec<_>>(),
        vec![
            (10_000, 240, 62, 6),
            (10_240, 120, 66, 5),
            (10_480, 480, 69, 6)
        ]
    );
    // Deterministic plans.
    assert_eq!(plan_subs(&s, &dest, SubMapping::ByChannel), plan);
    let fresh = plan_subs(&s, &dest, SubMapping::CreateNew);
    assert_eq!(fresh.map, vec![6, 7]);
    assert_eq!(plan_subs(&s, &dest, SubMapping::Single(5)).map, vec![5, 5]);
    // Out-of-range transposition is refused, not dropped.
    assert!(place(&s, 0, 960, 70, &plan, Default::default()).is_err());
    assert!(place(&s, u64::MAX - 10, 960, 0, &plan, Default::default()).is_err());
}

#[test]
fn inserting_a_score_with_automation_is_one_undoable_step() {
    use plumspirit_engine::editor::PreparedEdit;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::{Bpm, PlumDocument, Subchannel, SubchannelId};
    let mut s = score_of(&[note(0, 480, 60, 0, 0, 0), note(480, 480, 62, 3, 0, 0)]);
    s.lanes.push(ScoreLane {
        kind: LaneKind::Cc(10),
        channel: 0,
        points: vec![(0, 10), (480, 100)],
    });
    s.lanes.push(ScoreLane {
        kind: LaneKind::PitchBend,
        channel: 0,
        points: vec![(0, 8192), (240, 9000)],
    });
    s.lanes.push(ScoreLane {
        kind: LaneKind::Velocity,
        channel: 0,
        points: vec![(0, 20), (480, 120)],
    });
    let mut d = PlumDocument::new_project("ins".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    let before = d.state_fingerprint();
    let t = d.track(0).unwrap();
    let dest: Vec<(u32, u8)> = t
        .subchannels()
        .entries()
        .iter()
        .map(|e| (e.id.0, e.midi_channel))
        .collect();
    let plan = plan_subs(&s, &dest, SubMapping::ByChannel);
    let mut placed = place(&s, 1920, 960, 0, &plan, Default::default()).unwrap();
    apply_velocity_lanes(&s, &mut placed, 1920, 960);
    assert_eq!(
        placed.iter().map(|n| n.vel).collect::<Vec<_>>(),
        vec![20, 120]
    );
    let records: Vec<_> = placed.iter().map(|n| n.to_record()).collect();
    let new_subs: Vec<Subchannel> = plan
        .create
        .iter()
        .map(|(id, ch, _)| Subchannel::new(SubchannelId(*id), *ch))
        .collect();
    let mut ops = vec![Operation::Prepared {
        edit: PreparedEdit::InsertSelected {
            host: plumspirit_engine::model::ContentRef::Track(t.id()),
            base_revision: t.notes.revision(),
            records,
            subs_track: t.id(),
            new_subs,
        },
        label: "Insert Score",
    }];
    for e in lane_edits(t, &s, 1920, 960, 1 << 20).unwrap() {
        ops.push(Operation::Events {
            edit: e,
            label: "Insert Score",
        });
    }
    h.apply(
        &mut d,
        Operation::Group {
            ops,
            label: "Insert Score",
        },
    )
    .unwrap();
    assert_eq!(h.undo_len(), 1);
    let t = d.track(0).unwrap();
    assert_eq!(t.notes.len(), 2);
    assert_eq!(
        t.subchannels().len(),
        2,
        "the channel-3 subchannel was created"
    );
    assert_eq!(t.events.events().len(), 4, "2 pan + 2 bend events");
    let after = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
}

/// v0.9 256-Key Mode (V09-S21 step 2): a score holding pitches 128–255 is
/// written as score version 2 (the same layout; v0.8 refuses it as newer), a
/// standard score stays version 1 byte for byte; wide pitches reopen exactly
/// and insert only into a 256-key project — a standard project refuses the
/// score with a sentence, never clamping it.
#[test]
fn wide_scores_round_trip_and_insert_only_where_their_keys_exist() {
    use plumspirit_engine::model::project::KeyRange;
    let keys = [0u8, 127, 128, 200, 255];
    let notes: Vec<TNote> = keys
        .iter()
        .enumerate()
        .map(|(i, &p)| note(i as u64 * 100, 50, p, 0, 0, 0))
        .collect();
    let s = score_of(&notes);
    assert_eq!(s.version(), VERSION_256);
    let b = s.to_bytes();
    assert_eq!(u16::from_le_bytes([b[8], b[9]]), VERSION_256);
    let back = Score::from_bytes(&b).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.to_bytes(), b);
    // A standard score: version 1.
    let std = score_of(&notes[..2]);
    assert_eq!(std.version(), VERSION);
    assert_eq!(
        u16::from_le_bytes([std.to_bytes()[8], std.to_bytes()[9]]),
        VERSION
    );
    // The same note bytes labelled version 1: damage, not a guess.
    let mut v1 = b.clone();
    v1[8..10].copy_from_slice(&VERSION.to_le_bytes());
    assert!(
        Score::from_bytes(&reseal(v1))
            .unwrap_err()
            .contains("damaged")
    );
    // Newer than this release: refused by name.
    let mut v3 = b.clone();
    v3[8..10].copy_from_slice(&(VERSION_256 + 1).to_le_bytes());
    assert!(
        Score::from_bytes(&reseal(v3))
            .unwrap_err()
            .contains("newer")
    );
    // Insertion follows the destination's keys.
    let plan = plan_subs(&s, &[(0, 0)], SubMapping::ByChannel);
    let e = place(&s, 0, 960, 0, &plan, KeyRange::Standard).unwrap_err();
    assert!(
        e.to_string().contains("255") && e.to_string().contains("127"),
        "{e}"
    );
    let placed = place(&s, 0, 960, 0, &plan, KeyRange::Extended256).unwrap();
    let mut got: Vec<u8> = placed.iter().map(|n| n.pitch).collect();
    got.sort();
    assert_eq!(got, keys);
    let e = place(&s, 0, 960, 1, &plan, KeyRange::Extended256).unwrap_err();
    assert!(e.to_string().contains("0–255"), "{e}");
    // Transposed down into the standard keys, it fits a standard project.
    let low = score_of(&notes[2..]);
    assert!(
        place(
            &low,
            0,
            960,
            -128,
            &plan_subs(&low, &[(0, 0)], SubMapping::ByChannel),
            KeyRange::Standard
        )
        .is_ok()
    );
}
