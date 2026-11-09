use super::*;
use crate::codec::Crc32;
use crate::editor::controllers::{ControllerKind, EventClip};
use crate::history::{History, HistoryLimits};
use crate::model::events::{RawEvent, RawKind};
use crate::model::{Bpm, NoteAttrs, NoteOffKind};
use crate::native::{project_from_bytes, project_to_bytes};

fn setup() -> (PlumDocument, ClipId, NoteAttrs) {
    let mut doc = PlumDocument::new_project("Source".into(), Bpm::DEFAULT, 480);
    let mut history = History::new(HistoryLimits::default());
    let sub = Subchannel {
        id: SubchannelId::MAIN,
        midi_channel: 4,
        name: Some("Strings".into()),
    };
    history
        .apply(
            &mut doc,
            Operation::EditSubchannel {
                track: 0,
                sub,
                move_notes: false,
            },
        )
        .unwrap();
    let group = |id, parent| GroupInfo {
        id,
        parent,
        name: Some(format!("Group {id}")),
        color: Some([2, 4, 8]),
        locked: id == 1,
        temporary: false,
        link: 5,
    };
    doc.project_mut().groups = GroupTable::from_entries(vec![group(1, 0), group(2, 1)], 3).unwrap();
    let attrs = NoteAttrs::new(67, 103, 4, 55, NoteOffKind::NoteOff)
        .unwrap()
        .with_muted(true);
    let notes = [
        NoteRecord {
            group: 2,
            ..NoteRecord::new(100, 101, attrs)
        },
        NoteRecord::new(
            100,
            1,
            NoteAttrs::new(64, 111, 1, 0, NoteOffKind::NoteOnZero).unwrap(),
        ),
        NoteRecord::new(199, 40, attrs),
    ];
    let track = doc.track_mut(0).unwrap();
    track.notes.insert_sorted_batch(&notes, true).unwrap();
    for (anchor, status, data) in [(0, 0xC4, [72, 0]), (1, 0xB4, [10, 64]), (2, 0xE4, [2, 117])] {
        track
            .events
            .push(RawEvent {
                tick: 100,
                anchor,
                kind: RawKind::Channel { status, data },
            })
            .unwrap();
    }
    let id = ClipId::Primary(track.id());
    let op = clips::resize_clip(&doc, id, 30, 100, Some(140)).unwrap();
    history.apply(&mut doc, op).unwrap();
    (doc, id, attrs)
}

#[test]
fn notes_roundtrip_and_paste_preserve_attributes_timing_and_group_identity() {
    let (source, _, attrs) = setup();
    let clipboard =
        Clipboard::copy_selected_with_groups(source.track(0).unwrap(), &source.project().groups)
            .unwrap()
            .unwrap();
    let origin = Origin::new(&source, 30);
    let bytes = encode(origin, &Music::Notes(clipboard.clone())).unwrap();
    assert_eq!(
        wire_len(origin, &Music::Notes(clipboard.clone())).unwrap(),
        bytes.len()
    );
    let (decoded_origin, Music::Notes(mut pasted)) = decode(&bytes).unwrap() else {
        panic!()
    };
    assert_eq!(decoded_origin, origin);
    assert_eq!(pasted, clipboard);
    let mut destination = PlumDocument::new_project("Target".into(), Bpm::DEFAULT, 960);
    destination.project_mut().groups = source.project().groups.clone();
    normalize_notes(&mut pasted, &destination, NoteTarget::Track(0), origin).unwrap();
    let (edit, groups) = pasted
        .prepare_paste_grouped(&destination, 0, 1_000)
        .unwrap();
    let mut history = History::new(HistoryLimits::default());
    history
        .apply(
            &mut destination,
            Operation::Group {
                ops: vec![
                    Operation::SetGroups {
                        table: Box::new(groups),
                        label: "Paste",
                    },
                    Operation::Prepared {
                        edit,
                        label: "Paste",
                    },
                ],
                label: "Paste",
            },
        )
        .unwrap();
    let track = destination.track(0).unwrap();
    let notes: Vec<_> = track.notes.iter().collect();
    assert_eq!(notes.len(), 3);
    assert_eq!(
        (notes[0].start_tick, notes[0].duration_ticks, notes[0].attrs),
        (1_000, 202, attrs)
    );
    assert_eq!(
        (
            notes[1].start_tick,
            notes[1].duration_ticks,
            notes[1].attrs.pitch()
        ),
        (1_000, 2, 64)
    );
    assert_eq!(notes[2].start_tick, 1_198);
    assert_ne!(notes[0].sub, SubchannelId::MAIN);
    assert_eq!(
        track
            .subchannels()
            .get(notes[0].sub)
            .unwrap()
            .name
            .as_deref(),
        Some("Strings")
    );
    let table = &destination.project().groups;
    let child = table.get(notes[0].group).unwrap();
    let parent = table.get(child.parent).unwrap();
    assert_ne!((child.id, parent.id), (2, 1));
    assert_eq!(child.link, parent.link);
    assert_ne!(child.link, 5);
    assert!(!parent.locked);
    assert_eq!(history.undo_len(), 1);
    history.undo(&mut destination).unwrap();
    assert!(destination.track(0).unwrap().notes.is_empty());
    assert_eq!(destination.track(0).unwrap().subchannels().len(), 1);
    history.redo(&mut destination).unwrap();
    assert_eq!(destination.track(0).unwrap().notes.len(), 3);
}

#[test]
fn clip_roundtrip_paste_save_and_undo_preserve_content_and_window() {
    let (source, clip, attrs) = setup();
    let origin = Origin::new(&source, 30);
    let native = clips::copy_clips(&source, &[clip]).unwrap();
    let portable = ClipBlock::capture(&source, &[clip], native).unwrap();
    let bytes = encode(origin, &Music::Clips(portable)).unwrap();
    let (origin, Music::Clips(block)) = decode(&bytes).unwrap() else {
        panic!()
    };
    let mut target = PlumDocument::new_project("Target".into(), Bpm::DEFAULT, 960);
    let before = target.state_fingerprint();
    let mut history = History::new(HistoryLimits::default());
    for tick in [60, 200] {
        let op = block.clone().prepare(&target, origin, 0, tick).unwrap();
        history.apply(&mut target, op).unwrap();
    }
    let (reopened, _) = project_from_bytes(&project_to_bytes(&target).unwrap()).unwrap();
    let layout = reopened.clips().layout();
    let [a, b] = layout.placements.as_slice() else {
        panic!()
    };
    assert_ne!(a.content, b.content);
    assert_eq!((a.start, a.offset, a.length), (60, 200, Some(280)));
    let ContentRef::Owned(id) = a.content else {
        panic!()
    };
    let content = reopened.clips().content(id).unwrap();
    let n = content.notes.iter().next().unwrap();
    assert_eq!((n.start_tick, n.duration_ticks, n.attrs), (200, 202, attrs));
    assert_eq!(
        reopened.project().groups.get(n.group).unwrap().color,
        Some([2, 4, 8])
    );
    assert_eq!(
        reopened
            .track(0)
            .unwrap()
            .subchannels()
            .get(n.sub)
            .unwrap()
            .midi_channel,
        4
    );
    assert_eq!(
        content
            .events
            .events()
            .iter()
            .map(|e| (e.tick, e.anchor))
            .collect::<Vec<_>>(),
        [(200, 0), (200, 1), (200, 2)]
    );
    assert_eq!(history.undo_len(), 2);
    history.undo(&mut target).unwrap();
    history.undo(&mut target).unwrap();
    assert_eq!(target.state_fingerprint(), before);
    history.redo(&mut target).unwrap();
    assert_eq!(target.clips().layout().placements.len(), 1);
}

#[test]
fn graph_types_roundtrip_and_refuse_tick_collisions_and_mixed_timebases() {
    let doc = PlumDocument::new_project("Graph".into(), Bpm::DEFAULT, 480);
    let origin = Origin::new(&doc, 320);
    for kind in [
        ControllerKind::Cc(0),
        ControllerKind::Cc(127),
        ControllerKind::PitchBend,
    ] {
        let clip = EventClip {
            kind: Some(kind),
            points: vec![(0, 0), (320, kind.max())],
        };
        let bytes = encode(
            origin,
            &Music::Controller {
                clip: clip.clone(),
                channel: 15,
            },
        )
        .unwrap();
        let (
            origin_after,
            Music::Controller {
                clip: after,
                channel,
            },
        ) = decode(&bytes).unwrap()
        else {
            panic!()
        };
        assert_eq!((origin_after, after, channel), (origin, clip, 15));
    }
    let mut points = vec![(0, 16_383), (320, 12_345)];
    let (_, Music::Master(pasted)) =
        decode(&encode(origin, &Music::Master(points.clone())).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(pasted, points);
    assert!(normalize_points(&mut points, origin.division, Division::from_raw(0xE728)).is_err());
    assert!(
        normalize_points(
            &mut [(0, 0), (1, 1)],
            Division::ppqn(960),
            Division::ppqn(1)
        )
        .is_err()
    );
    normalize_points(&mut points, origin.division, Division::ppqn(721)).unwrap();
    assert_eq!(points, [(0, 16_383), (481, 12_345)]);
}

#[test]
fn hostile_payloads_are_bounded_and_refused() {
    let (doc, _, _) = setup();
    let origin = Origin::new(&doc, 0);
    let bytes = encode(origin, &Music::Master(vec![(0, 16_383), (80, 0)])).unwrap();
    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end]).is_err());
    }
    for offset in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[offset] ^= 1;
        assert!(decode(&bad).is_err(), "corruption at byte {offset}");
    }
    // A valid checksum does not grant permission to allocate from a forged count.
    let mut bad = bytes.clone();
    bad[29] = 127;
    let end = bad.len() - 4;
    let crc = Crc32::of(&bad[..end]).to_le_bytes();
    bad[end..].copy_from_slice(&crc);
    assert!(decode(&bad).is_err());
    let mut bad = bytes.clone();
    bad[11] = 1;
    let crc = Crc32::of(&bad[..end]).to_le_bytes();
    bad[end..].copy_from_slice(&crc);
    assert!(decode(&bad).is_err());
}

#[test]
fn extended_keys_and_tick_overflow_refuse_without_mutating_destination() {
    let mut source = PlumDocument::new_project("256".into(), Bpm::DEFAULT, 1);
    source.project_mut().keys = KeyRange::Extended256;
    source
        .track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(
            &[
                NoteRecord::new(
                    0,
                    1,
                    NoteAttrs::new_wide(220, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                ),
                NoteRecord::new(u64::MAX - 1, 1, NoteAttrs::default_for_pitch(71).unwrap()),
            ],
            true,
        )
        .unwrap();
    let origin = Origin::new(&source, 0);
    let notes = Clipboard::copy_selected(source.track(0).unwrap())
        .unwrap()
        .unwrap();
    let (_, Music::Notes(mut notes)) =
        decode(&encode(origin, &Music::Notes(notes)).unwrap()).unwrap()
    else {
        panic!()
    };
    let mut dest = PlumDocument::new_project("Target".into(), Bpm::DEFAULT, 960);
    assert!(
        normalize_notes(&mut notes, &dest, NoteTarget::Track(0), origin)
            .unwrap_err()
            .contains("256-Key")
    );
    dest.project_mut().keys = KeyRange::Extended256;
    let before = dest.state_fingerprint();
    assert!(
        normalize_notes(&mut notes, &dest, NoteTarget::Track(0), origin)
            .unwrap_err()
            .contains("largest tick")
    );
    assert_eq!(dest.state_fingerprint(), before);
}

#[test]
#[ignore = "release-mode timing and resource measurement"]
fn black_midi_clipboard_scale() {
    use std::time::Instant;

    let doc = PlumDocument::new_project("Benchmark".into(), Bpm::DEFAULT, 480);
    let origin = Origin::new(&doc, 0);
    for count in [1, 1_000, 100_000, 1_000_000, 4_000_000] {
        let attrs = NoteAttrs::default_for_pitch(64).unwrap();
        let notes = Clipboard {
            records: (0..count)
                .map(|i| NoteRecord::new(i * 2, 8, attrs))
                .collect(),
            subs: vec![Subchannel::new(SubchannelId::MAIN, 0)],
            groups: vec![],
        };
        let start = Instant::now();
        let bytes = encode(origin, &Music::Notes(notes)).unwrap();
        let encode = start.elapsed();
        let start = Instant::now();
        let (_, Music::Notes(notes)) = decode(&bytes).unwrap() else {
            panic!()
        };
        let decode = start.elapsed();
        let start = Instant::now();
        let edit = notes.prepare_paste(&doc, 0, 480).unwrap();
        assert_eq!(edit.affected_notes(), count as usize);
        eprintln!(
            "{count} notes, {} bytes, encode {encode:?}, decode {decode:?}, prepare {:?}",
            bytes.len(),
            start.elapsed()
        );
    }
}
