//! PlumSpirit v0.5: subchannels, subtracks, structure-preserving MIDI export and
//! import, `.lup` version 4 and its migrations.

mod common;

use common::*;
use plumspirit_engine::editor::Clipboard;
use plumspirit_engine::history::{History, Operation, Outcome};
use plumspirit_engine::midi::{
    ExportOptions, export_to_bytes_with, import_bytes, plan_export_with, validate_exported_bytes,
};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId, TrackId,
};
use plumspirit_engine::native::{project_from_bytes, project_to_bytes};

fn project() -> PlumDocument {
    PlumDocument::new_project("v0.5".into(), Bpm::DEFAULT, 960)
}

fn note(start: u64, pitch: u8, ch: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 120,
        attrs: NoteAttrs::new(pitch, 100, ch, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

fn sub(id: u32, ch: u8) -> Subchannel {
    Subchannel::new(SubchannelId(id), ch)
}

fn roundtrip_lup(d: &PlumDocument) -> PlumDocument {
    let bytes = project_to_bytes(d).unwrap();
    let (back, report) = project_from_bytes(&bytes).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    assert_eq!(project_to_bytes(&back).unwrap(), bytes);
    back
}

fn export(d: &PlumDocument, options: ExportOptions) -> Vec<u8> {
    let plan = plan_export_with(d, options);
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let bytes = export_to_bytes_with(d, &plan).unwrap();
    validate_exported_bytes(d, &plan, &bytes).unwrap();
    bytes
}

/// Every channel status byte in a MIDI file (walks the chunks with running status).
fn channel_statuses(smf: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 14;
    while pos + 8 <= smf.len() {
        let len =
            u32::from_be_bytes([smf[pos + 4], smf[pos + 5], smf[pos + 6], smf[pos + 7]]) as usize;
        let body = &smf[pos + 8..pos + 8 + len];
        if &smf[pos..pos + 4] == b"MTrk" {
            let mut i = 0;
            let mut running = 0u8;
            while i < body.len() {
                while body[i] & 0x80 != 0 {
                    i += 1;
                }
                i += 1; // delta
                let b = body[i];
                if b == 0xFF {
                    i += 2;
                    let mut l = 0usize;
                    loop {
                        let c = body[i];
                        i += 1;
                        l = (l << 7) | usize::from(c & 0x7F);
                        if c & 0x80 == 0 {
                            break;
                        }
                    }
                    i += l;
                    running = 0;
                    continue;
                }
                if b == 0xF0 || b == 0xF7 {
                    i += 1;
                    let mut l = 0usize;
                    loop {
                        let c = body[i];
                        i += 1;
                        l = (l << 7) | usize::from(c & 0x7F);
                        if c & 0x80 == 0 {
                            break;
                        }
                    }
                    i += l;
                    running = 0;
                    continue;
                }
                let status = if b & 0x80 != 0 {
                    i += 1;
                    running = b;
                    b
                } else {
                    running
                };
                out.push(status);
                i += if matches!(status & 0xF0, 0xC0 | 0xD0) {
                    1
                } else {
                    2
                };
            }
        }
        pos += 8 + len;
    }
    out
}

fn multiset(d: &PlumDocument) -> Vec<(u64, u64, u32)> {
    let mut v: Vec<(u64, u64, u32)> = d
        .tracks()
        .iter()
        .flat_map(|t| {
            t.notes
                .iter()
                .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
        })
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn subchannels_are_unlimited_ids_separate_from_midi_channels() {
    let mut d = project();
    let mut h = History::default();
    // 1000 subchannels in one batch, all on MIDI channel 5 (index 4).
    let ids = d.track(0).unwrap().subchannels().free_ids(1000).unwrap();
    let subs: Vec<Subchannel> = ids.iter().map(|id| sub(id.0, 4)).collect();
    let before = d.state_fingerprint();
    h.apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    assert_eq!(h.undo_len(), 1, "a batch is one undo step");
    let t = d.track(0).unwrap();
    assert_eq!(t.subchannels().len(), 1001);
    assert_eq!(t.subchannels().max_id(), SubchannelId(1000));
    assert!(
        t.subchannels()
            .entries()
            .iter()
            .all(|s| s.midi_channel <= 15)
    );
    // Stable ids: undo removes them, redo restores the same ids.
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.track(0).unwrap().subchannels().len(), 1001);
    assert!(
        d.track(0)
            .unwrap()
            .subchannels()
            .contains(SubchannelId(777))
    );
    // An invalid MIDI channel (17 → index 16) is refused and changes nothing.
    let fp = d.state_fingerprint();
    let bad = Subchannel::new(SubchannelId(5000), 16);
    assert!(
        h.apply(
            &mut d,
            Operation::AddSubchannels {
                track: 0,
                subs: vec![bad]
            }
        )
        .is_err()
    );
    assert_eq!(d.state_fingerprint(), fp);
    // Duplicates are refused.
    assert!(
        h.apply(
            &mut d,
            Operation::AddSubchannels {
                track: 0,
                subs: vec![sub(3, 0)]
            }
        )
        .is_err()
    );
}

#[test]
fn notes_keep_their_subchannel_and_midi_channel_through_save_and_undo() {
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: (1..40).map(|i| sub(i, (i % 16) as u8)).collect(),
        },
    )
    .unwrap();
    for i in 0..40u32 {
        let r = note(u64::from(i) * 10, 60 + (i % 12) as u8, (i % 16) as u8, i);
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: r,
            },
        )
        .unwrap();
    }
    let t = d.track(0).unwrap();
    for (i, n) in t.notes.iter().enumerate() {
        assert_eq!(n.sub, SubchannelId(i as u32));
        assert_eq!(n.attrs.channel(), (i % 16) as u8);
    }
    t.notes.validate().unwrap();
    // .lup v4 round trip (state fingerprint includes every note's subchannel).
    let back = roundtrip_lup(&d);
    for (a, b) in back.track(0).unwrap().notes.iter().zip(t.notes.iter()) {
        assert_eq!(a.sub, b.sub);
    }
    // Undo everything, then redo everything.
    let full = d.state_fingerprint();
    while h.undo(&mut d).unwrap().is_some() {}
    assert_eq!(d.track(0).unwrap().notes.len(), 0);
    while h.redo(&mut d).unwrap().is_some() {}
    assert_eq!(d.state_fingerprint(), full);
}

#[test]
fn main_subchannel_notes_cost_no_extra_memory() {
    let mut d = project();
    let recs: Vec<NoteRecord> = (0..100_000u64).map(|i| note(i, 60, 0, 0)).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let plain = d.track(0).unwrap().notes.heap_bytes();
    let mut e = project();
    let recs: Vec<NoteRecord> = (0..100_000u64)
        .map(|i| note(i, 60, 0, (i % 2) as u32))
        .collect();
    e.track_mut(0).unwrap().subchannels();
    let mut h = History::default();
    h.apply(
        &mut e,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![sub(1, 0)],
        },
    )
    .unwrap();
    e.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mixed = e.track(0).unwrap().notes.heap_bytes();
    let per_note_plain = plain as f64 / 100_000.0;
    let per_note_mixed = mixed as f64 / 100_000.0;
    assert!(per_note_plain < 21.0, "{per_note_plain}");
    // Mixed chunks add one u32 per note.
    assert!(per_note_mixed < per_note_plain + 4.5, "{per_note_mixed}");
}

#[test]
fn deleting_a_subchannel_deletes_its_notes_and_undo_restores_both() {
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![sub(1, 3), sub(2, 3)],
        },
    )
    .unwrap();
    let recs: Vec<NoteRecord> = (0..300u64)
        .map(|i| note(i, 60, 3, (i % 3) as u32))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let before = d.state_fingerprint();
    assert_eq!(d.track(0).unwrap().notes.count_in_sub(SubchannelId(1)), 100);
    h.apply(
        &mut d,
        Operation::DeleteSubchannel {
            track: 0,
            id: SubchannelId(1),
        },
    )
    .unwrap();
    let t = d.track(0).unwrap();
    assert_eq!(t.notes.len(), 200);
    assert!(!t.subchannels().contains(SubchannelId(1)));
    // No note refers to the deleted subchannel.
    assert!(t.notes.iter().all(|n| n.sub != SubchannelId(1)));
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.track(0).unwrap().notes.len(), 200);
    // The last subchannel cannot be deleted.
    let mut one = project();
    assert!(
        h.apply(
            &mut one,
            Operation::DeleteSubchannel {
                track: 0,
                id: SubchannelId::MAIN
            }
        )
        .is_err()
    );
}

#[test]
fn editing_a_subchannel_channel_can_move_its_notes() {
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![sub(1, 0)],
        },
    )
    .unwrap();
    let recs: Vec<NoteRecord> = (0..10u64).map(|i| note(i, 60, 0, (i % 2) as u32)).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let before = d.state_fingerprint();
    let mut s = sub(1, 9);
    s.name = Some("Drums".into());
    h.apply(
        &mut d,
        Operation::EditSubchannel {
            track: 0,
            sub: s,
            move_notes: true,
        },
    )
    .unwrap();
    let t = d.track(0).unwrap();
    for n in t.notes.iter() {
        let want = if n.sub.0 == 1 { 9 } else { 0 };
        assert_eq!(n.attrs.channel(), want);
    }
    assert_eq!(
        t.subchannels()
            .get(SubchannelId(1))
            .unwrap()
            .name
            .as_deref(),
        Some("Drums")
    );
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
}

#[test]
fn moving_selected_notes_between_subchannels_is_one_undo_step() {
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![sub(7, 6)],
        },
    )
    .unwrap();
    let recs: Vec<NoteRecord> = (0..50u64).map(|i| note(i, 60, 0, 0)).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let before = d.state_fingerprint();
    d.track_mut(0).unwrap().notes.select_all();
    h.apply(
        &mut d,
        Operation::SetSubchannel {
            track: 0.into(),
            sub: SubchannelId(7),
            set_channel: true,
        },
    )
    .unwrap();
    assert!(
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .all(|n| n.sub == SubchannelId(7) && n.attrs.channel() == 6)
    );
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.track(0).unwrap().notes.count_in_sub(SubchannelId(7)), 50);
}

#[test]
fn paste_into_another_track_creates_missing_subchannels_once() {
    let mut d = project();
    let mut h = History::default();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    let mut s = sub(4, 2);
    s.name = Some("Lead".into());
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![s],
        },
    )
    .unwrap();
    let recs: Vec<NoteRecord> = (0..8u64)
        .map(|i| note(i, 60, 2, (i % 2) as u32 * 4))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, true)
        .unwrap();
    let clip = Clipboard::copy_selected(d.track(0).unwrap())
        .unwrap()
        .unwrap();
    let before = d.state_fingerprint();
    for round in 0..2 {
        let e = clip.prepare_paste(&d, 1, 1000 * (round + 1)).unwrap();
        h.apply(
            &mut d,
            Operation::Prepared {
                edit: e,
                label: "Paste",
            },
        )
        .unwrap();
    }
    let t = d.track(1).unwrap();
    assert_eq!(
        t.subchannels().len(),
        2,
        "created once, reused by the second paste"
    );
    assert_eq!(
        t.subchannels()
            .get(SubchannelId(4))
            .unwrap()
            .name
            .as_deref(),
        Some("Lead")
    );
    assert_eq!(t.notes.count_in_sub(SubchannelId(4)), 8);
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
}

#[test]
fn subtracks_nest_batch_add_and_delete_as_whole_subtrees() {
    let mut d = project();
    let mut h = History::default();
    h.apply(&mut d, Operation::AddTrack).unwrap(); // track 1, top level
    let empty = d.fingerprint();
    // 100 subtracks under track 0 in one step.
    let first = match h
        .apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 100,
            },
        )
        .unwrap()
    {
        Outcome::Track(i) => i,
        o => panic!("{o:?}"),
    };
    assert_eq!(first, 1);
    assert_eq!(d.track_count(), 102);
    assert_eq!(h.undo_len(), 2);
    assert_eq!(d.track(1).unwrap().display_name(1), "Subtrack 1");
    assert_eq!(d.track(100).unwrap().display_name(100), "Subtrack 100");
    // Nested: 3 grandchildren under Subtrack 2 (index 2).
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 2,
            count: 3,
        },
    )
    .unwrap();
    d.validate_hierarchy().unwrap();
    let rows = d.tree_rows();
    assert_eq!(rows[0], (0, true));
    assert_eq!(rows[2], (1, true));
    assert_eq!(rows[3].0, 2);
    assert_eq!(d.subtree_end(0), 104);
    assert_eq!(d.children_of(2), vec![3, 4, 5]);
    assert_eq!(
        d.path_names(3),
        vec![
            "Track 1".to_string(),
            "Subtrack 2".into(),
            "Subtrack 1".into()
        ]
    );
    // Notes in a grandchild, then delete the top parent: the whole subtree goes.
    d.track_mut(4)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(0, 60, 0, 0)], false)
        .unwrap();
    let full = d.state_fingerprint();
    h.apply(&mut d, Operation::DeleteTrack { index: 0 })
        .unwrap();
    assert_eq!(d.track_count(), 1, "only the other top-level track remains");
    d.validate_hierarchy().unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), full);
    // Save/reopen keeps the tree.
    let back = roundtrip_lup(&d);
    assert_eq!(back.tree_rows(), d.tree_rows());
    // Undo back to the start.
    while h.undo(&mut d).unwrap().is_some() {}
    assert_eq!(d.track_count(), 1);
    h.redo(&mut d).unwrap();
    // Musically identical (the next free track id stays higher: ids are never
    // reused).
    assert_eq!(d.fingerprint(), empty);
    assert_eq!(d.track_count(), 2);
}

#[test]
fn deep_hierarchies_are_handled_without_recursion() {
    let mut d = project();
    let mut h = History::default();
    for parent in 0..5_000 {
        h.apply(&mut d, Operation::AddSubtracks { parent, count: 1 })
            .unwrap();
    }
    assert_eq!(d.track_count(), 5_001);
    d.validate_hierarchy().unwrap();
    assert_eq!(d.tree_rows()[5_000].0, 5_000);
    assert_eq!(d.subtree_end(0), 5_001);
    let back = roundtrip_lup(&d);
    assert_eq!(back.track_count(), 5_001);
    h.apply(&mut d, Operation::DeleteTrack { index: 1 })
        .unwrap();
    assert_eq!(d.track_count(), 1);
}

/// Rewrites the parent field of the `k`-th TRAK section of a `.lup` file and
/// fixes the section checksum, so only the hierarchy check can reject it.
fn set_parent_in_file(bytes: &[u8], k: usize, parent: Option<u32>) -> Vec<u8> {
    use plumspirit_engine::codec::{Crc32, get_uvarint, put_uvarint};
    let mut out = bytes[..32].to_vec();
    let mut pos = 32;
    let mut seen = 0;
    while pos < bytes.len() {
        let tag = &bytes[pos..pos + 4];
        let flags = &bytes[pos + 4..pos + 8];
        let len = u64::from_le_bytes(bytes[pos + 8..pos + 16].try_into().unwrap()) as usize;
        let mut payload = bytes[pos + 16..pos + 16 + len].to_vec();
        if tag == b"TRAK" {
            if seen == k {
                let mut p = 0;
                let id = get_uvarint(&payload, &mut p).unwrap();
                let _old = get_uvarint(&payload, &mut p).unwrap();
                let mut np = Vec::new();
                put_uvarint(&mut np, id);
                put_uvarint(&mut np, parent.map_or(0, |x| u64::from(x) + 1));
                np.extend_from_slice(&payload[p..]);
                payload = np;
            }
            seen += 1;
        }
        let mut c = Crc32::new();
        c.update(tag);
        c.update(flags);
        c.update(&(payload.len() as u64).to_le_bytes());
        c.update(&Crc32::of(&payload).to_le_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(flags);
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&c.finish().to_le_bytes());
        pos += 16 + len + 4;
    }
    out
}

#[test]
fn malformed_hierarchies_in_lup_files_are_refused_without_panicking() {
    let mut d = project();
    let mut h = History::default();
    h.apply(&mut d, Operation::AddTrack).unwrap(); // id 1
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap(); // id 2, child of 0, stored at index 1
    let bytes = project_to_bytes(&d).unwrap();
    // The helper itself leaves a valid file valid.
    let same = set_parent_in_file(&bytes, 1, Some(0));
    assert_eq!(
        project_from_bytes(&same).unwrap().0.state_fingerprint(),
        d.state_fingerprint()
    );
    // A → B and B → A: track 0 (id 0) names its own child (id 2) as parent.
    let cycle = set_parent_in_file(&bytes, 0, Some(2));
    assert!(project_from_bytes(&cycle).is_err());
    // A track that is its own parent.
    let own = set_parent_in_file(&bytes, 0, Some(0));
    assert!(project_from_bytes(&own).is_err());
    // A parent that does not exist.
    let missing = set_parent_in_file(&bytes, 1, Some(99));
    assert!(project_from_bytes(&missing).is_err());
    // A parent that comes after its subtrack (not a depth-first tree).
    let apart = set_parent_in_file(&bytes, 1, Some(1));
    assert!(project_from_bytes(&apart).is_err());
    // Damaging any byte is detected (checksums), never a panic.
    for i in (32..bytes.len()).step_by(5) {
        let mut b = bytes.clone();
        b[i] ^= 0x5A;
        let _ = project_from_bytes(&b);
    }
}

#[test]
fn legacy_v04_projects_migrate_to_top_level_tracks_with_inferred_subchannels() {
    // A v0.4 file: saved by the previous writer is not available here, so build
    // the equivalent state: a MIDI import gives the same inference.
    let mut s = TrackBytes::new();
    s.on(0, 0, 60, 100).on(0, 4, 62, 100).on(0, 9, 36, 100);
    s.off(10, 0, 60, 0).off(10, 4, 62, 0).off(10, 9, 36, 0);
    let t = s.eot(20);
    let mut single = TrackBytes::new();
    single.on(0, 2, 60, 100).off(10, 2, 60, 0);
    let u = single.eot(20);
    let (doc, summary) = import_bytes(&smf(1, 480, &[t, u])).unwrap();
    // The Open MIDI mapping summary (§72) reports what the file holds and what
    // it became.
    assert_eq!(summary.source_chunks, 2);
    assert_eq!(
        summary.channels_used,
        (1 << 0) | (1 << 2) | (1 << 4) | (1 << 9)
    );
    assert_eq!((summary.subchannel_count, summary.subtrack_count), (4, 0));
    assert_eq!(
        summary.structure_lines(),
        [
            "Source MIDI: 2 track(s), 4 MIDI channel(s) used, 4 note(s).".to_string(),
            "PlumSpirit result: 2 top-level track(s), 4 subchannel(s), 0 subtrack(s).".to_string(),
        ]
    );
    assert_eq!(summary.structure_short(), "4 subchannel(s), 0 subtrack(s)");
    let a = doc.track(0).unwrap();
    assert_eq!(a.subchannels().len(), 3);
    let chans: Vec<u8> = a
        .subchannels()
        .entries()
        .iter()
        .map(|e| e.midi_channel)
        .collect();
    assert_eq!(chans, vec![0, 4, 9]);
    for n in a.notes.iter() {
        let want = match n.attrs.channel() {
            0 => 0,
            4 => 1,
            _ => 2,
        };
        assert_eq!(n.sub, SubchannelId(want));
    }
    let b = doc.track(1).unwrap();
    assert_eq!(b.subchannels().len(), 1);
    assert_eq!(b.subchannels().first().midi_channel, 2);
    assert!(b.parent().is_none() && a.parent().is_none());
}

#[test]
fn flat_projects_export_without_structure_data() {
    let mut s = TrackBytes::new();
    s.on(0, 0, 60, 100).on(5, 3, 62, 100);
    s.off(10, 0, 60, 0).off(15, 3, 62, 0);
    let t = s.eot(20);
    let file = smf(1, 480, &[t]);
    let (doc, _) = import_bytes(&file).unwrap();
    let out = export(&doc, ExportOptions::default());
    let plan = plan_export_with(&doc, ExportOptions::default());
    assert!(!plan.structure.metadata);
    assert!(plan.structure_summary().is_empty());
    // Byte-identical to what v0.4 wrote (no records, no extra chunks).
    assert_eq!(out, file);
}

fn structured_doc() -> PlumDocument {
    let mut d = project();
    let mut h = History::default();
    // Track 0: subchannels 0..20, several sharing MIDI channel 1.
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: (1..20).map(|i| sub(i, (i % 3) as u8)).collect(),
        },
    )
    .unwrap();
    let recs: Vec<NoteRecord> = (0..2000u64)
        .map(|i| {
            note(
                i * 7,
                40 + (i % 50) as u8,
                ((i % 20) % 3) as u8,
                (i % 20) as u32,
            )
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 3,
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 2,
            count: 2,
        },
    )
    .unwrap();
    for i in 1..d.track_count() {
        let recs: Vec<NoteRecord> = (0..100u64)
            .map(|k| note(k * 11 + i as u64, 60, (i % 16) as u8, 0))
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    d.validate_hierarchy().unwrap();
    d
}

#[test]
fn structured_export_keeps_every_note_valid_channels_and_restores_exactly() {
    let d = structured_doc();
    let plan = plan_export_with(&d, ExportOptions::default());
    assert!(plan.structure.metadata);
    assert_eq!(plan.structure.subtracks, 5);
    // 20 subchannels over 3 channels → 7 layers for track 0.
    assert_eq!(plan.structure.extra_chunks, 6);
    assert!(!plan.structure_summary().is_empty());
    let bytes = export(&d, ExportOptions::default());
    // No channel status ever encodes a channel above 16.
    for s in channel_statuses(&bytes) {
        assert!((0x80..=0xEF).contains(&s), "{s:#x}");
    }
    let (back, summary) = import_bytes(&bytes).unwrap();
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.user_message().contains("restored")),
        "{:?}",
        summary.warnings
    );
    assert_eq!(back.track_count(), d.track_count());
    assert_eq!(back.tree_rows(), d.tree_rows());
    assert_eq!(multiset(&back), multiset(&d));
    // The mapping summary counts the restored structure, and the file's chunks
    // (extra layers included) as the source.
    let subs: usize = d.tracks().iter().map(|t| t.subchannels().len()).sum();
    let subtracks = d.tracks().iter().filter(|t| t.parent().is_some()).count();
    assert_eq!(summary.subchannel_count, subs);
    assert_eq!(summary.subtrack_count, subtracks);
    assert_eq!(summary.source_chunks, d.track_count() + 6);
    assert!(
        summary.structure_lines()[1].contains(&format!(
            "{} top-level track(s)",
            d.track_count() - subtracks
        )),
        "{:?}",
        summary.structure_lines()
    );
    for (a, b) in back.tracks().iter().zip(d.tracks()) {
        assert_eq!(a.subchannels(), b.subchannels());
        let mut x: Vec<(u64, u32, u32)> = a
            .notes
            .iter()
            .map(|n| (n.start_tick, n.attrs.raw(), n.sub.0))
            .collect();
        let mut y: Vec<(u64, u32, u32)> = b
            .notes
            .iter()
            .map(|n| (n.start_tick, n.attrs.raw(), n.sub.0))
            .collect();
        x.sort_unstable();
        y.sort_unstable();
        assert_eq!(x, y, "every note keeps its subchannel");
    }
    // Re-exporting the re-imported project writes no duplicate records.
    let again = export(&back, ExportOptions::default());
    let (third, _) = import_bytes(&again).unwrap();
    assert_eq!(third.tree_rows(), d.tree_rows());
    assert_eq!(multiset(&third), multiset(&d));
}

#[test]
fn without_structure_data_the_same_file_opens_as_flat_tracks_with_all_notes() {
    let d = structured_doc();
    let bytes = export(
        &d,
        ExportOptions {
            structure_metadata: false,
            ..ExportOptions::default()
        },
    );
    let (back, _) = import_bytes(&bytes).unwrap();
    // Layers become their own tracks; nothing is nested; nothing is lost.
    assert_eq!(back.track_count(), d.track_count() + 6);
    assert!(back.tracks().iter().all(|t| t.parent().is_none()));
    assert_eq!(multiset(&back), multiset(&d));
    // Not keeping subchannels apart: one chunk per track, still every note.
    let merged = export(
        &d,
        ExportOptions {
            split_subchannels: false,
            ..ExportOptions::default()
        },
    );
    let (m, _) = import_bytes(&merged).unwrap();
    assert_eq!(m.track_count(), d.track_count());
    assert_eq!(multiset(&m), multiset(&d));
    assert_eq!(m.tree_rows(), d.tree_rows());
}

#[test]
fn damaged_structure_data_is_ignored_not_trusted() {
    let d = structured_doc();
    let bytes = export(&d, ExportOptions::default());
    // Drop the last chunk (a layer / subtrack): its record is missing now, so the
    // structure is ignored and the file opens flat, keeping the notes it has.
    let mut pos = 14;
    let mut starts = Vec::new();
    while pos + 8 <= bytes.len() {
        starts.push(pos);
        let len = u32::from_be_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8 + len;
    }
    let mut cut = bytes[..*starts.last().unwrap()].to_vec();
    let n = u16::from_be_bytes([cut[10], cut[11]]) - 1;
    cut[10..12].copy_from_slice(&n.to_be_bytes());
    // Re-number: point the second record at a node that does not exist by
    // removing the parent chunk instead would also work; here a subtrack's
    // parent is still present, so remove the FIRST chunk instead.
    let first = starts[0];
    let second = starts[1];
    let mut no_first = bytes[..first].to_vec();
    no_first.extend_from_slice(&bytes[second..]);
    let n2 = u16::from_be_bytes([no_first[10], no_first[11]]) - 1;
    no_first[10..12].copy_from_slice(&n2.to_be_bytes());
    let (back, summary) = import_bytes(&no_first).unwrap();
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.user_message().contains("ignored")),
        "{:?}",
        summary.warnings
    );
    assert!(back.tracks().iter().all(|t| t.parent().is_none()));
    back.validate_hierarchy().unwrap();
    // Foreign files never get a fabricated hierarchy.
    let (plain, _) = import_bytes(&cut).unwrap();
    plain.validate_hierarchy().unwrap();
    let _ = TrackId(0);
}

// ------------------------------------------------------------ import into track

use plumspirit_engine::midi::merge::{ConductorPolicy, MergeMode, MergeOptions, prepare_merge};

/// Source: Track A (ch 1), Track B (ch 1), Track C (ch 2 and 3), with a tempo
/// change and a marker in the tempo track.
fn source_smf(ppqn: u16) -> Vec<u8> {
    let p = u64::from(ppqn);
    let mut tempo = TrackBytes::new();
    tempo.meta(0, 0x51, &[0x07, 0xA1, 0x20]); // 120 BPM
    tempo.meta(p * 4, 0x51, &[0x05, 0x16, 0x15]); // ~180 BPM
    tempo.meta(p * 4, 0x06, b"Drop");
    let t0 = tempo.eot(p * 8);
    let mut a = TrackBytes::new();
    a.meta(0, 0x03, b"Lead");
    a.on(0, 0, 60, 100)
        .off(p, 0, 60, 0)
        .on(p, 0, 62, 90)
        .off(p * 2, 0, 62, 0);
    let ta = a.eot(p * 2);
    let mut b = TrackBytes::new();
    b.meta(0, 0x03, b"Bass");
    b.on(1, 0, 36, 100).off(p + 1, 0, 36, 0);
    let tb = b.eot(p * 2);
    let mut c = TrackBytes::new();
    c.meta(0, 0x03, b"Pad");
    c.on(3, 1, 64, 80)
        .on(3, 2, 67, 80)
        .ev(p / 2, &[0xB1, 7, 90]);
    c.off(p * 3, 1, 64, 0).off(p * 3, 2, 67, 0);
    let tc = c.eot(p * 3);
    smf(1, ppqn, &[t0, ta, tb, tc])
}

fn dest_project(ppqn: u32) -> PlumDocument {
    let mut d = PlumDocument::new_project("Merge".into(), Bpm::DEFAULT, ppqn);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(0, 72, 5, 0)], false)
        .unwrap();
    d
}

#[test]
fn import_into_track_as_subtracks_creates_one_child_per_source_track() {
    let (src, _) = import_bytes(&source_smf(960)).unwrap();
    let mut d = dest_project(960);
    let before = d.state_fingerprint();
    let music = d.fingerprint();
    let p = prepare_merge(
        &d,
        0,
        &src,
        MergeOptions {
            mode: MergeMode::AsSubtracks,
            policy: ConductorPolicy::Keep,
            at: 0,
        },
    )
    .unwrap();
    // Preparing changed nothing (Cancel = drop the prepared result).
    assert_eq!(d.state_fingerprint(), before);
    assert_eq!(p.summary.new_subtracks, 3, "{:?}", p.summary);
    assert_eq!(p.summary.empty_tracks_skipped, 1);
    assert!(!p.summary.lines().is_empty());
    let mut h = History::default();
    h.apply(&mut d, p.operation).unwrap();
    assert_eq!(h.undo_len(), 1, "one undo step");
    assert_eq!(d.track_count(), 4);
    assert_eq!(d.children_of(0), vec![1, 2, 3]);
    let names: Vec<String> = (1..4)
        .map(|i| d.track(i).unwrap().display_name(i))
        .collect();
    assert_eq!(names, ["Lead", "Bass", "Pad"]);
    // Pad has two subchannels (channels 2 and 3).
    assert_eq!(d.track(3).unwrap().subchannels().len(), 2);
    assert_eq!(d.note_count(), 1 + 5);
    // The Conductor kept its tempo; the source marker was added.
    assert_eq!(d.conductor().tempo_points().len(), 1);
    assert_eq!(d.conductor().markers().len(), 1);
    h.undo(&mut d).unwrap();
    // Everything inserted is gone (track ids are never reused, so only the
    // next free id differs).
    assert_eq!(d.fingerprint(), music);
    assert_eq!(d.track_count(), 1);
    assert!(d.conductor().markers().is_empty());
    h.redo(&mut d).unwrap();
    assert_eq!(d.track_count(), 4);
    d.validate_hierarchy().unwrap();
}

#[test]
fn import_into_track_as_subchannels_keeps_sources_apart_and_channels_intact() {
    let (src, _) = import_bytes(&source_smf(960)).unwrap();
    let mut d = dest_project(960);
    let before = d.state_fingerprint();
    let p = prepare_merge(
        &d,
        0,
        &src,
        MergeOptions {
            mode: MergeMode::AsSubchannels,
            policy: ConductorPolicy::Ignore,
            at: 0,
        },
    )
    .unwrap();
    assert_eq!(p.summary.new_subchannels, 4);
    let mut h = History::default();
    h.apply(&mut d, p.operation).unwrap();
    assert_eq!(h.undo_len(), 1);
    let t = d.track(0).unwrap();
    assert_eq!(d.track_count(), 1);
    assert_eq!(t.subchannels().len(), 5);
    let names: Vec<(String, u8)> = t.subchannels().entries()[1..]
        .iter()
        .map(|s| (s.display_name(), s.midi_channel))
        .collect();
    assert_eq!(
        names,
        [
            ("Lead · Ch 1".to_string(), 0),
            ("Bass · Ch 1".to_string(), 0),
            ("Pad · Ch 2".to_string(), 1),
            ("Pad · Ch 3".to_string(), 2)
        ]
    );
    // Lead and Bass both use MIDI channel 1 but different subchannels.
    let lead = t.subchannels().entries()[1].id;
    let bass = t.subchannels().entries()[2].id;
    assert_ne!(lead, bass);
    assert_eq!(t.notes.count_in_sub(lead), 2);
    assert_eq!(t.notes.count_in_sub(bass), 1);
    for n in t.notes.iter() {
        assert!(n.attrs.channel() <= 15);
    }
    // The CC of the Pad track was merged into the destination's events.
    assert!(t.events.events().iter().any(|e| matches!(
        e.kind,
        plumspirit_engine::model::RawKind::Channel { status: 0xB1, .. }
    )));
    assert_eq!(d.conductor().tempo_points().len(), 1);
    assert!(d.conductor().markers().is_empty(), "Ignore adds nothing");
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.track(0).unwrap().subchannels().len(), 5);
}

#[test]
fn import_into_track_converts_ppqn_exactly_with_documented_rounding() {
    // 480 → 960: exact doubling.
    let (src, _) = import_bytes(&source_smf(480)).unwrap();
    let d = dest_project(960);
    let opts = MergeOptions {
        mode: MergeMode::AsSubtracks,
        policy: ConductorPolicy::Ignore,
        at: 960,
    };
    let p = prepare_merge(&d, 0, &src, opts).unwrap();
    assert_eq!(p.summary.ppqn, Some((480, 960)));
    assert_eq!(p.summary.notes_rounded, 0);
    let mut d2 = d.clone();
    History::default().apply(&mut d2, p.operation).unwrap();
    // Bass note: source tick 1 → 2, shifted by 960.
    let bass = d2.track(2).unwrap().notes.iter().next().unwrap();
    assert_eq!(bass.start_tick, 962);
    assert_eq!(bass.duration_ticks, 960);
    // 960 → 480: odd ticks round half up; start and end convert separately.
    let (src, _) = import_bytes(&source_smf(960)).unwrap();
    let d = dest_project(480);
    let p = prepare_merge(&d, 0, &src, MergeOptions { at: 0, ..opts }).unwrap();
    assert!(p.summary.notes_rounded >= 2, "{:?}", p.summary);
    assert!(p.summary.max_rounding_ticks <= 0.5);
    let mut d3 = d.clone();
    History::default().apply(&mut d3, p.operation).unwrap();
    let bass = d3.track(2).unwrap().notes.iter().next().unwrap();
    // 1 × 480/960 = 0.5 → 1 (half up); end 961 → 480.5 → 481.
    assert_eq!((bass.start_tick, bass.end_tick()), (1, 481));
    let pad: Vec<u64> = d3
        .track(3)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.start_tick)
        .collect();
    assert_eq!(pad, vec![2, 2]); // 3 → 1.5 → 2
}

#[test]
fn conductor_merge_policies_never_duplicate_the_tempo_map() {
    let (src, _) = import_bytes(&source_smf(960)).unwrap();
    let base = dest_project(960);
    let run = |policy: ConductorPolicy| {
        let mut d = base.clone();
        let p = prepare_merge(
            &d,
            0,
            &src,
            MergeOptions {
                mode: MergeMode::AsSubtracks,
                policy,
                at: 0,
            },
        )
        .unwrap();
        let summary = p.summary.clone();
        History::default().apply(&mut d, p.operation).unwrap();
        (d, summary)
    };
    let (keep, _) = run(ConductorPolicy::Keep);
    assert_eq!(keep.conductor().tempo_points().len(), 1);
    assert_eq!(keep.conductor().markers().len(), 1);
    let (ignore, _) = run(ConductorPolicy::Ignore);
    assert_eq!(ignore.conductor().tempo_points().len(), 1);
    assert!(ignore.conductor().markers().is_empty());
    let (merge, s) = run(ConductorPolicy::Merge);
    // Source 120 BPM at 0 equals the project's: no conflict; ~180 at bar 2 added.
    assert_eq!(merge.conductor().tempo_points().len(), 2, "{s:?}");
    assert_eq!(s.conflict_count, 0);
    let (replace, _) = run(ConductorPolicy::Replace);
    assert_eq!(replace.conductor().tempo_points().len(), 2);
    // Merging again: every source entry collides with itself → no duplicates.
    let mut twice = merge.clone();
    let p = prepare_merge(
        &twice,
        0,
        &src,
        MergeOptions {
            mode: MergeMode::AsSubtracks,
            policy: ConductorPolicy::Merge,
            at: 0,
        },
    )
    .unwrap();
    History::default().apply(&mut twice, p.operation).unwrap();
    assert_eq!(twice.conductor().tempo_points().len(), 2);
    // A conflicting tempo at the same tick is reported and the project's kept.
    let mut other = base.clone();
    let e = other
        .conductor()
        .prepare_set_start_tempo(plumspirit_engine::model::TempoValue::Bpm(
            Bpm::parse("99").unwrap(),
        ))
        .unwrap();
    History::default()
        .apply(
            &mut other,
            Operation::Conductor {
                edit: e,
                label: "Tempo",
            },
        )
        .unwrap();
    let mut src2 = src.clone();
    let _ = &mut src2;
    let p = prepare_merge(
        &other,
        0,
        &src,
        MergeOptions {
            mode: MergeMode::AsSubtracks,
            policy: ConductorPolicy::Merge,
            at: 0,
        },
    )
    .unwrap();
    assert_eq!(p.summary.conflict_count, 1, "{:?}", p.summary);
}

#[test]
fn smpte_and_format_2_merges_are_refused_with_an_explanation() {
    let mut t = TrackBytes::new();
    t.on(0, 0, 60, 100).off(10, 0, 60, 0);
    let smpte = smf(1, 0xE728, &[t.eot(20)]); // 25 fps, 40 ticks/frame
    let (src, _) = import_bytes(&smpte).unwrap();
    let d = dest_project(960);
    let e = prepare_merge(
        &d,
        0,
        &src,
        MergeOptions {
            mode: MergeMode::AsSubtracks,
            policy: ConductorPolicy::Keep,
            at: 0,
        },
    )
    .unwrap_err();
    assert!(e.contains("SMPTE"), "{e}");
}

// ------------------------------------------------------------------- preview

use plumspirit_engine::editor::EditControl;
use plumspirit_engine::editor::factor::ExactFactor;
use plumspirit_engine::editor::preview::{
    NoteKernel, PreviewGeneration, PreviewScope, summarize, visible_preview,
};
use plumspirit_engine::editor::quantize::{QuantizeMode, QuantizeSpec, prepare_quantize};
use plumspirit_engine::editor::transforms::{
    prepare_flip_x, prepare_scale_x, prepare_scale_y, scale_y_kernel,
};
use std::sync::atomic::AtomicBool;

fn preview_doc(n: u64) -> PlumDocument {
    let mut d = project();
    let recs: Vec<NoteRecord> = (0..n)
        .map(|i| {
            let mut r = note(
                1000 + i * 37 + (i % 7),
                30 + (i % 60) as u8,
                (i % 16) as u8,
                0,
            );
            r.duration_ticks = 20 + (i % 90);
            r
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

/// Applies `edit` and returns the multiset of the track's notes.
fn applied(
    mut d: PlumDocument,
    edit: plumspirit_engine::editor::PreparedEdit,
) -> Vec<(u64, u64, u32)> {
    History::default()
        .apply(&mut d, Operation::Prepared { edit, label: "T" })
        .unwrap();
    let mut v: Vec<_> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
        .collect();
    v.sort_unstable();
    v
}

/// The preview's result for every note (a viewport covering everything).
fn previewed(d: &PlumDocument, k: &NoteKernel) -> Vec<(u64, u64, u32)> {
    let t = d.track(0).unwrap();
    let mut v = Vec::new();
    let (n, cut) = visible_preview(
        t,
        k,
        PreviewScope::Selected,
        0,
        u64::MAX,
        usize::MAX,
        |_, q| {
            let q = q.expect("no note is removed");
            v.push((q.start_tick, q.duration_ticks, q.attrs.raw()))
        },
    );
    assert!(!cut);
    assert_eq!(n, t.notes.selected_count());
    v.sort_unstable();
    v
}

#[test]
fn preview_equals_apply_and_never_changes_the_document() {
    let base = {
        let mut d = preview_doc(5_000);
        d.track_mut(0).unwrap().notes.select_all();
        d
    };
    let fp = base.state_fingerprint();
    let t = base.track(0).unwrap();
    let b = t.notes.selection_bounds().unwrap();
    let never = AtomicBool::new(false);
    let cases: Vec<(NoteKernel, plumspirit_engine::editor::PreparedEdit)> = vec![
        (
            NoteKernel::flip_x(&b),
            prepare_flip_x(&base, 0, EditControl::default()).unwrap(),
        ),
        (
            NoteKernel::scale_x(&b, &ExactFactor::parse("2.5").unwrap()).unwrap(),
            prepare_scale_x(
                &base,
                0,
                &ExactFactor::parse("2.5").unwrap(),
                EditControl::default(),
            )
            .unwrap(),
        ),
        (
            NoteKernel::scale_x(&b, &ExactFactor::parse("0.333").unwrap()).unwrap(),
            prepare_scale_x(
                &base,
                0,
                &ExactFactor::parse("0.333").unwrap(),
                EditControl::default(),
            )
            .unwrap(),
        ),
        (
            scale_y_kernel(t, &ExactFactor::parse("-0.5").unwrap(), Default::default()).unwrap(),
            prepare_scale_y(&base, 0, &ExactFactor::parse("-0.5").unwrap()).unwrap(),
        ),
        (
            NoteKernel::Quantize(QuantizeSpec {
                mode: QuantizeMode::Both,
                grid: 240,
                origin: 0,
            }),
            prepare_quantize(
                &base,
                0,
                &QuantizeSpec {
                    mode: QuantizeMode::Both,
                    grid: 240,
                    origin: 0,
                },
                EditControl::default(),
            )
            .unwrap(),
        ),
    ];
    for (k, edit) in cases {
        let s = summarize(t, &k, PreviewScope::Selected, &never);
        assert!(s.error.is_none(), "{k:?}");
        assert_eq!(s.notes, 5_000);
        assert!(s.changed > 0);
        // Preview did not touch anything.
        assert_eq!(base.state_fingerprint(), fp);
        // What was previewed is exactly what Apply commits.
        assert_eq!(previewed(&base, &k), applied(base.clone(), edit), "{k:?}");
    }
}

#[test]
fn visible_preview_visits_only_notes_near_the_viewport() {
    let mut d = preview_doc(200_000);
    d.track_mut(0).unwrap().notes.select_all();
    let t = d.track(0).unwrap();
    let b = t.notes.selection_bounds().unwrap();
    let k = NoteKernel::scale_x(&b, &ExactFactor::parse("2").unwrap()).unwrap();
    // A viewport of 10,000 ticks after scaling covers ~5,000 source ticks ≈ 135 notes.
    let mut visited = 0;
    let (n, cut) = visible_preview(
        t,
        &k,
        PreviewScope::Selected,
        1_000_000,
        1_010_000,
        50_000,
        |_, q| {
            let q = q.expect("no note is removed");
            visited += 1;
            assert!(q.start_tick <= 1_010_000 && q.end_tick().unwrap() >= 1_000_000);
        },
    );
    assert!(!cut);
    assert_eq!(n, visited);
    assert!(n > 50 && n < 400, "{n}");
    // Every note that is visible after scaling is visited (no misses).
    let expected = t
        .notes
        .iter()
        .filter(|v| {
            let q = k.map(&v.record()).unwrap();
            q.start_tick <= 1_010_000 && q.end_tick().unwrap() >= 1_000_000
        })
        .count();
    assert_eq!(n, expected);
    // A budget bounds the work and says the preview is simplified.
    let (n, cut) = visible_preview(t, &k, PreviewScope::Selected, 0, u64::MAX, 1_000, |_, _| {});
    assert_eq!(n, 1_000);
    assert!(cut);
}

#[test]
fn stale_preview_generations_are_recognised() {
    let g = PreviewGeneration::default();
    let a = g.next(); // Scale = 2.0
    let b = g.next(); // user types 2.5
    // A's worker finishes after B was requested: it must be discarded.
    assert!(!g.is_current(a));
    assert!(g.is_current(b));
}

#[test]
fn a_refused_preview_explains_why_and_apply_refuses_too() {
    let mut d = preview_doc(100);
    d.track_mut(0).unwrap().notes.select_all();
    let t = d.track(0).unwrap();
    let e = scale_y_kernel(t, &ExactFactor::parse("10").unwrap(), Default::default()).unwrap_err();
    assert!(e.user_message().contains("pitch") || e.user_message().contains("note"));
    assert!(prepare_scale_y(&d, 0, &ExactFactor::parse("10").unwrap()).is_err());
    let b = t.notes.selection_bounds().unwrap();
    assert!(NoteKernel::scale_x(&b, &ExactFactor::parse("-2").unwrap()).is_err());
}

#[test]
fn gate_and_velocity_field_previews_equal_what_the_fields_commit() {
    let mut base = preview_doc(3_000);
    base.track_mut(0).unwrap().notes.select_all();
    let fp = base.state_fingerprint();
    let commit = |op: Operation| {
        let mut d = base.clone();
        History::default().apply(&mut d, op).unwrap();
        let mut v: Vec<_> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.duration_ticks, n.attrs.raw()))
            .collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        previewed(&base, &NoteKernel::SetDuration(777)),
        commit(Operation::SetDuration {
            track: 0.into(),
            duration: 777
        })
    );
    assert_eq!(
        previewed(&base, &NoteKernel::SetVelocity(9)),
        commit(Operation::SetVelocity {
            track: 0.into(),
            velocity: 9
        })
    );
    assert_eq!(base.state_fingerprint(), fp);
    // Invalid values are refused by both.
    let t = base.track(0).unwrap();
    let never = AtomicBool::new(false);
    assert!(
        summarize(
            t,
            &NoteKernel::SetDuration(0),
            PreviewScope::Selected,
            &never
        )
        .error
        .is_some()
    );
    assert!(
        History::default()
            .apply(
                &mut base.clone(),
                Operation::SetDuration {
                    track: 0.into(),
                    duration: 0
                }
            )
            .is_err()
    );
}

#[test]
fn correction_preview_shows_exactly_how_notes_play_and_finds_every_visible_one() {
    use plumspirit_engine::editor::preview::KernelOutcome;
    use plumspirit_engine::model::correction::{Corrected, Correction};
    use plumspirit_engine::model::track::GatePercent;
    let d = preview_doc(20_000);
    let fp = d.state_fingerprint();
    let t = d.track(0).unwrap();
    let max_dur = t.notes.snapshot().max_duration();
    let never = AtomicBool::new(false);
    for (ticks, keys, gate, range, from) in [
        (480i64, 0i16, 10_000u32, (0u8, 127u8), 0u64),
        (-700, 12, 25_000, (40, 90), 50_000),
        (0, -30, 5_000, (0, 127), 0),
    ] {
        let c = Correction {
            from_tick: from,
            ticks,
            keys,
            gate: GatePercent::from_hundredths(gate).unwrap(),
            range,
            top: 127,
        };
        let k = NoteKernel::Correction { c, max_dur };
        // The preview's per-note result is the playback/export rule itself.
        let mut removed = 0;
        for v in t.notes.iter() {
            let r = v.record();
            match (k.outcome(&r).unwrap(), c.note(r)) {
                (KernelOutcome::Note(a), Corrected::Note { note, .. }) => assert_eq!(a, note),
                (KernelOutcome::Removed, Corrected::OutsideRange | Corrected::PitchOverflow) => {
                    removed += 1
                }
                other => panic!("{other:?}"),
            }
        }
        let s = summarize(t, &k, PreviewScope::All, &never);
        assert_eq!(s.removed, removed);
        assert_eq!(s.notes, 20_000);
        // Every note whose played form (or, if silent, whose own extent) reaches
        // a viewport is visited: compare with a full scan.
        for (t0, t1) in [(200_000u64, 210_000u64), (0, 5_000), (700_000, 800_000)] {
            let mut seen = 0usize;
            visible_preview(t, &k, PreviewScope::All, t0, t1, usize::MAX, |_, _| {
                seen += 1
            });
            let want = t
                .notes
                .iter()
                .filter(|v| {
                    let r = v.record();
                    let (s, e) = match k.outcome(&r).unwrap() {
                        KernelOutcome::Note(q) => (q.start_tick, q.end_tick().unwrap()),
                        KernelOutcome::Removed => (r.start_tick, r.end_tick().unwrap()),
                    };
                    s <= t1 && e >= t0
                })
                .count();
            assert_eq!(seen, want, "{ticks} {keys} {gate} {t0}..{t1}");
        }
    }
    assert_eq!(d.state_fingerprint(), fp);
}

#[test]
fn onion_layers_order_by_visible_density_with_stable_ties_and_persist() {
    use plumspirit_engine::model::project::{
        DensityBasis, OnionMode, OnionOrder, order_onion_layers,
    };
    // Back-to-front drawing order: Dense First draws the densest last (in front).
    let sources = [3usize, 5, 7, 9];
    let counts = [10usize, 500, 10, 0];
    assert_eq!(
        order_onion_layers(OnionOrder::DenseFirst, &sources, &counts),
        vec![9, 7, 3, 5]
    );
    assert_eq!(
        order_onion_layers(OnionOrder::SparseFirst, &sources, &counts),
        vec![5, 7, 3, 9]
    );
    assert_eq!(
        order_onion_layers(OnionOrder::Project, &sources, &counts),
        sources.to_vec()
    );
    // Ties keep project order deterministically (the earlier track in front),
    // whatever order the counts arrive in, run after run.
    for _ in 0..3 {
        assert_eq!(
            order_onion_layers(OnionOrder::DenseFirst, &[1, 2, 3], &[4, 4, 4]),
            vec![3, 2, 1]
        );
    }

    // Subtracks are independent sources: a parent's density is its own notes
    // only (no double counting of its children).
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 2,
        },
    )
    .unwrap();
    let fill = |d: &mut PlumDocument, i: usize, n: u64, step: u64| {
        let recs: Vec<NoteRecord> = (0..n).map(|k| note(k * step, 60, 0, 0)).collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    };
    fill(&mut d, 0, 10, 100); // parent: 10 notes in 0..1000
    fill(&mut d, 1, 300, 10); // subtrack 1: 300 notes in 0..3000
    fill(&mut d, 2, 50, 1000); // subtrack 2: 50 notes in 0..50,000
    let count = |d: &PlumDocument, i: usize, t0: u64, t1: u64| {
        d.track(i).unwrap().notes.count_starting_in(t0, t1)
    };
    assert_eq!(count(&d, 0, 0, 999), 10);
    let visible = |t0, t1| {
        let c: Vec<usize> = (0..3).map(|i| count(&d, i, t0, t1)).collect();
        order_onion_layers(OnionOrder::DenseFirst, &[0, 1, 2], &c)
    };
    // In 0..2,000 subtrack 1 is densest; in 10,000..40,000 only subtrack 2 has notes.
    assert_eq!(visible(0, 2_000).last(), Some(&1));
    assert_eq!(visible(10_000, 40_000).last(), Some(&2));

    // Opacity 0 / 25 / 50 / 100, order and density basis survive save/reopen.
    for (opacity, order, density) in [
        (0u8, OnionOrder::DenseFirst, DensityBasis::VisibleRange),
        (25, OnionOrder::SparseFirst, DensityBasis::WholeTrack),
        (50, OnionOrder::Project, DensityBasis::VisibleRange),
        (100, OnionOrder::DenseFirst, DensityBasis::WholeTrack),
    ] {
        let mut e = d.clone();
        let ids: Vec<_> = e.tracks().iter().map(|t| t.id()).collect();
        {
            let o = &mut e.project_mut().onion;
            o.mode = OnionMode::Specific;
            o.specific = vec![ids[1], ids[2]];
            o.opacity = opacity;
            o.order = order;
            o.density = density;
        }
        let back = roundtrip_lup(&e);
        assert_eq!(back.project().onion, e.project().onion);
    }
}

#[test]
fn exports_keep_every_channel_in_the_midi_range_for_every_channel_layout() {
    // (name, subchannels as (id, channel), note channel/sub for note k)
    type Layout = (&'static str, Vec<(u32, u8)>, fn(u64) -> (u8, u32));
    let layouts: Vec<Layout> = vec![
        ("one physical channel", vec![], |_| (0, 0)),
        (
            "all 16 channels",
            (1..16).map(|i| (i, i as u8)).collect(),
            |k| ((k % 16) as u8, (k % 16) as u32),
        ),
        (
            "40 subchannels over 16 channels",
            (1..40).map(|i| (i, (i % 16) as u8)).collect(),
            |k| (((k % 40) % 16) as u8, (k % 40) as u32),
        ),
        (
            "300 subchannels repeating channel 10",
            (1..300).map(|i| (i, 9)).collect(),
            |k| (9, (k % 300) as u32),
        ),
    ];
    for (name, subs, of) in layouts {
        let mut d = project();
        if !subs.is_empty() {
            History::default()
                .apply(
                    &mut d,
                    Operation::AddSubchannels {
                        track: 0,
                        subs: subs.iter().map(|&(i, c)| sub(i, c)).collect(),
                    },
                )
                .unwrap();
        }
        // The main subchannel's channel follows the layout too.
        let recs: Vec<NoteRecord> = (0..3_000u64)
            .map(|k| {
                let (ch, s) = of(k);
                note(k * 5, 30 + (k % 70) as u8, ch, s)
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        for options in [
            ExportOptions::default(),
            ExportOptions {
                structure_metadata: false,
                ..ExportOptions::default()
            },
            ExportOptions {
                split_subchannels: false,
                structure_metadata: false,
                ..ExportOptions::default()
            },
        ] {
            let bytes = export(&d, options);
            let statuses = channel_statuses(&bytes);
            assert!(!statuses.is_empty());
            // Every channel message is a valid status with a channel 1–16, and
            // the note channels are exactly the ones the notes carry.
            assert!(statuses.iter().all(|s| (0x80..=0xEF).contains(s)), "{name}");
            let mut used: Vec<u8> = statuses
                .iter()
                .filter(|s| matches!(*s & 0xF0, 0x80 | 0x90))
                .map(|s| s & 0x0F)
                .collect();
            used.sort_unstable();
            used.dedup();
            let mut want: Vec<u8> = recs.iter().map(|r| r.attrs.channel()).collect();
            want.sort_unstable();
            want.dedup();
            assert_eq!(used, want, "{name} {options:?}");
            // Nothing is lost.
            let (back, _) = import_bytes(&bytes).unwrap();
            assert_eq!(multiset(&back), multiset(&d), "{name} {options:?}");
        }
    }
}

#[test]
fn notes_in_subchannels_missing_from_the_table_still_save_and_export_loadably() {
    // A hand-built document (or a bug) can give notes subchannel ids the track's
    // table lacks; saving and exporting must still produce files that open.
    let mut d = project();
    let recs: Vec<NoteRecord> = (0..64u64)
        .map(|i| note(i * 10, 60, (i % 4) as u8, (i % 4) as u32 * 7))
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    assert_eq!(d.track(0).unwrap().subchannels().len(), 1);
    let bytes = project_to_bytes(&d).unwrap();
    let (back, _) = project_from_bytes(&bytes).unwrap();
    let ids: Vec<u32> = back
        .track(0)
        .unwrap()
        .subchannels()
        .entries()
        .iter()
        .map(|e| e.id.0)
        .collect();
    assert_eq!(ids, vec![0, 7, 14, 21]);
    assert_eq!(multiset(&back), multiset(&d));
    // MIDI: the structure records list the same subchannels and restore.
    let midi = export(&d, ExportOptions::default());
    let (m, summary) = import_bytes(&midi).unwrap();
    assert!(
        summary
            .warnings
            .iter()
            .all(|w| !w.user_message().contains("ignored")),
        "{:?}",
        summary.warnings
    );
    assert_eq!(m.track(0).unwrap().subchannels().len(), 4);
    assert_eq!(multiset(&m), multiset(&d));
}

#[test]
fn a_project_saved_by_v04_opens_with_inferred_subchannels_and_exports_like_v04() {
    // Written by the v0.4 engine (tests/fixtures/make_v04_fixture.rs.txt).
    let bytes = include_bytes!("fixtures/journey_v04.lup");
    let v04_midi = include_bytes!("fixtures/journey_v04.mid");
    let (d, report) = project_from_bytes(bytes).unwrap();
    assert_eq!(report.migrated_from, Some(3));
    assert_eq!(d.track_count(), 3);
    assert!(d.tracks().iter().all(|t| t.parent().is_none()));
    assert_eq!(d.note_count(), 32);
    // Track 1: channels 1, 5, 10 → three subchannels "Ch 1/5/10", notes in them.
    let a = d.track(0).unwrap();
    let subs: Vec<(u32, u8, Option<&str>)> = a
        .subchannels()
        .entries()
        .iter()
        .map(|e| (e.id.0, e.midi_channel, e.name.as_deref()))
        .collect();
    assert_eq!(
        subs,
        vec![
            (0, 0, Some("Ch 1")),
            (1, 4, Some("Ch 5")),
            (2, 9, Some("Ch 10"))
        ]
    );
    for v in a.notes.iter() {
        let want = match v.attrs.channel() {
            0 => 0,
            4 => 1,
            9 => 2,
            c => panic!("channel {c}"),
        };
        assert_eq!(v.sub.0, want);
    }
    // Track 2: one channel → the main subchannel on its channel for new notes;
    // its v0.4 properties are kept.
    let b = d.track(1).unwrap();
    assert_eq!(b.subchannels().len(), 1);
    assert_eq!(b.subchannels().entries()[0].midi_channel, 2);
    assert_eq!(b.props().key_correction, 12);
    assert!(b.notes.iter().all(|v| v.sub.0 == 0));
    // Track 3 (empty, muted).
    assert!(d.track(2).unwrap().mute);
    // Conductor and Onion Skin.
    assert_eq!(d.conductor().tempo_points().len(), 3);
    let o = &d.project().onion;
    assert_eq!(o.opacity, 50);
    assert_eq!(o.specific, vec![b.id()]);
    assert_eq!(
        o.order,
        plumspirit_engine::model::project::OnionOrder::Project
    );
    // Exported by v0.5 exactly as v0.4 exported it (flat, inferable: no
    // PlumSpirit structure data).
    let plan = plan_export_with(&d, ExportOptions::default());
    assert!(!plan.structure.metadata);
    let midi = export_to_bytes_with(&d, &plan).unwrap();
    assert_eq!(
        midi.as_slice(),
        v04_midi.as_slice(),
        "export differs from v0.4's"
    );
    // Saving writes version 4; reopening is exact.
    let back = roundtrip_lup(&d);
    assert_eq!(back.track(0).unwrap().subchannels(), a.subchannels());
}

#[test]
fn unnamed_tracks_keep_their_numbers_when_subtracks_are_added() {
    let mut d = project();
    let mut h = History::default();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    h.apply(&mut d, Operation::AddTrack).unwrap();
    assert_eq!(d.track_labels(), vec!["Track 1", "Track 2", "Track 3"]);
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 2,
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 4,
            count: 1,
        },
    )
    .unwrap();
    // Track 2 and 3 are still "Track 2" and "Track 3" (not renumbered by the
    // subtracks above them); subtracks are numbered among their siblings.
    assert_eq!(
        d.track_labels(),
        vec![
            "Track 1",
            "Subtrack 1",
            "Subtrack 2",
            "Track 2",
            "Track 3",
            "Subtrack 1"
        ]
    );
    assert_eq!(d.track_label(4), "Track 3");
    assert_eq!(d.track_ordinals(), vec![1, 1, 2, 2, 3, 1]);
}

#[derive(Default)]
struct OnCounter {
    ons: Vec<(u8, u8)>,
}

impl plumspirit_engine::playback::SynthSink for OnCounter {
    fn note_on(&mut self, channel: u8, key: u8, _velocity: u8) {
        self.ons.push((channel, key));
    }
    fn note_off(&mut self, _: u8, _: u8) {}
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
    fn silence(&mut self) {}
    fn reset(&mut self) {}
}

/// Every note-on of a full playback of `d`, sorted.
fn played(d: &PlumDocument) -> Vec<(u8, u8)> {
    use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits};
    let idx = std::sync::Arc::new(PlaybackIndex::build(d, 48_000));
    let mut s = Scheduler::new(idx, SchedulerLimits::default());
    let mut p = OnCounter::default();
    s.seek(0, &mut p);
    let mut guard = 0;
    while !s.finished() && guard < 1_000_000 {
        s.advance(256, &mut p);
        guard += 1;
    }
    p.ons.sort_unstable();
    p.ons
}

#[test]
fn playback_includes_every_subtrack_once_and_mute_is_per_track() {
    // Track 0 [Sub 1 [Sub 1.1], Sub 2]; each with its own pitch and channel; one
    // subchannel sharing a MIDI channel with another.
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 2,
        },
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 1,
            count: 1,
        },
    )
    .unwrap();
    assert_eq!(d.tree_rows()[2].0, 2);
    h.apply(
        &mut d,
        Operation::AddSubchannels {
            track: 0,
            subs: vec![sub(1, 0)],
        },
    )
    .unwrap();
    for (i, pitch) in [(0usize, 60u8), (1, 62), (2, 64), (3, 65)] {
        let recs: Vec<NoteRecord> = (0..4u64)
            .map(|k| {
                note(
                    k * 480,
                    pitch,
                    i as u8,
                    if i == 0 { (k % 2) as u32 } else { 0 },
                )
            })
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let all = played(&d);
    assert_eq!(all.len(), 16, "every note of every subtrack exactly once");
    for (ch, pitch) in [(0u8, 60u8), (1, 62), (2, 64), (3, 65)] {
        assert_eq!(all.iter().filter(|o| **o == (ch, pitch)).count(), 4);
    }
    // Muting the parent silences only the parent's own notes.
    d.track_mut(0).unwrap().mute = true;
    let p = played(&d);
    assert_eq!(p.len(), 12);
    assert!(p.iter().all(|o| o.1 != 60));
    // Muting the nested subtrack silences only it.
    d.track_mut(0).unwrap().mute = false;
    d.track_mut(2).unwrap().mute = true;
    let p = played(&d);
    assert_eq!(p.len(), 12);
    assert!(p.iter().all(|o| o.1 != 64));
    // Solo on a subtrack plays just that subtrack.
    d.track_mut(2).unwrap().mute = false;
    d.track_mut(3).unwrap().solo = true;
    assert_eq!(played(&d), vec![(3, 65); 4]);
}

/// Note-ons and note-offs with the audio block they happened in.
#[derive(Default)]
struct Timeline {
    block: u64,
    events: Vec<(u64, bool, u8, u8)>,
}

impl plumspirit_engine::playback::SynthSink for Timeline {
    fn note_on(&mut self, channel: u8, key: u8, _velocity: u8) {
        self.events.push((self.block, true, channel, key));
    }
    fn note_off(&mut self, channel: u8, key: u8) {
        self.events.push((self.block, false, channel, key));
    }
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
    fn silence(&mut self) {}
    fn reset(&mut self) {}
}

fn timeline(d: &PlumDocument) -> Vec<(u64, bool, u8, u8)> {
    use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits};
    let idx = std::sync::Arc::new(PlaybackIndex::build(d, 48_000));
    let mut s = Scheduler::new(idx, SchedulerLimits::default());
    let mut t = Timeline::default();
    s.seek(0, &mut t);
    while !s.finished() && t.block < 1_000_000 {
        s.advance(256, &mut t);
        t.block += 1;
    }
    t.events
}

#[test]
fn muting_a_parent_never_cuts_a_child_note_on_the_same_channel_and_key() {
    // Parent and subtrack both play key 60 on MIDI channel 1; the parent's note
    // is long and ends inside the child's note.
    let mut d = project();
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(
            &[NoteRecord {
                duration_ticks: 1_440,
                ..note(0, 60, 0, 0)
            }],
            false,
        )
        .unwrap();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(
            &[NoteRecord {
                duration_ticks: 1_920,
                ..note(960, 60, 0, 0)
            }],
            false,
        )
        .unwrap();
    let child_alone = {
        let mut c = d.clone();
        c.track_mut(0).unwrap().notes.select_all();
        c.track_mut(0).unwrap().notes.remove_selected();
        timeline(&c)
    };
    d.track_mut(0).unwrap().mute = true;
    // With the parent muted, the child sounds exactly as if it were alone: its
    // note-on and its own note-off at the same moments.
    assert_eq!(timeline(&d), child_alone);
    assert_eq!(child_alone.iter().filter(|e| e.1).count(), 1);
    assert_eq!(child_alone.iter().filter(|e| !e.1).count(), 1);
    // Solo of the child: the same.
    d.track_mut(0).unwrap().mute = false;
    d.track_mut(1).unwrap().solo = true;
    assert_eq!(timeline(&d), child_alone);
}
