//! V09-S12 (contract §047 – §049, §218; `docs/CLIP_MERGE.md`): Merge Clips in
//! Sequence and Merge Clips Stacked. The merged clip plays exactly what the
//! source clips played (notes moved by their placements, windows applied,
//! subchannels mapped deterministically, controllers never duplicated); the
//! sources go (primaries become removed primaries, linked contents stay for
//! their other placements); one undo step restores every clip, placement,
//! content, subchannel table and note exactly, the journal replays it after a
//! restart, and a stale plan is refused.

use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::editor::clip_merge::{
    MergeKind, MergePlan, SubMap, merge_clips, plan_merge,
};
use plumspirit_engine::editor::clips::ClipEditError;
use plumspirit_engine::editor::clips::{add_clip, duplicate_clips, move_clips, resize_clip};
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, RawEvent, RawKind,
    Subchannel, SubchannelId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use std::collections::BTreeMap;

fn note(t: u64, d: u64, p: u8, ch: u8, sub: u32, group: u32) -> NoteRecord {
    NoteRecord {
        start_tick: t,
        duration_ticks: d,
        attrs: NoteAttrs::new(p, 64 + (t % 50) as u8, ch, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group,
    }
}

fn cc(t: u64, ch: u8, num: u8, v: u8) -> RawEvent {
    RawEvent {
        tick: t,
        anchor: 0,
        kind: RawKind::Channel {
            status: 0xB0 | ch,
            data: [num, v],
        },
    }
}

fn bend(t: u64, ch: u8, v: u16) -> RawEvent {
    RawEvent {
        tick: t,
        anchor: 0,
        kind: RawKind::Channel {
            status: 0xE0 | ch,
            data: [(v & 0x7F) as u8, (v >> 7) as u8],
        },
    }
}

/// What the document plays (every unmuted placement of every track): per
/// track, (song start, duration, pitch, velocity, channel, group, subchannel
/// as (table track, id)) sorted.
type Played = Vec<(u64, u64, u8, u8, u8, u32, usize, u32)>;

fn played(d: &PlumDocument, only: Option<&[ClipId]>) -> Played {
    let mut out = Played::new();
    for i in 0..d.track_count() {
        for pc in d.placed_contents(i) {
            if pc.placement.muted || only.is_some_and(|o| !o.contains(&pc.placement.id)) {
                continue;
            }
            let table = match pc.placement.content {
                ContentRef::Track(t) => d.index_of(t).unwrap(),
                ContentRef::Owned(_) => i,
            };
            let (w0, w1) = pc.window();
            for n in pc.notes.iter() {
                if n.start_tick < w0 || n.start_tick >= w1 {
                    continue;
                }
                let s = pc.placement.song_tick(n.start_tick).unwrap();
                out.push((
                    s,
                    n.duration_ticks,
                    n.attrs.pitch(),
                    n.attrs.velocity(),
                    n.attrs.channel(),
                    n.group,
                    table,
                    n.sub.0,
                ));
            }
        }
    }
    out.sort();
    out
}

/// The channel events a set of placements plays, at song ticks.
fn played_events(d: &PlumDocument, ids: &[ClipId]) -> Vec<(u64, RawKind)> {
    let mut out = Vec::new();
    for i in 0..d.track_count() {
        for pc in d.placed_contents(i) {
            if !ids.contains(&pc.placement.id) {
                continue;
            }
            let (w0, w1) = pc.window();
            for e in pc.events.events() {
                if matches!(e.kind, RawKind::Channel { .. }) && e.tick >= w0 && e.tick < w1 {
                    out.push((pc.placement.song_tick(e.tick).unwrap(), e.kind));
                }
            }
        }
    }
    out.sort_by_key(|(t, k)| (*t, format!("{k:?}")));
    out
}

fn apply(d: &mut PlumDocument, h: &mut History, op: Operation) {
    h.apply(d, op).unwrap();
    d.validate_clips().unwrap();
}

/// Three tracks: track 0 (subchannels 0, 1; CC 7 at 0 and 960), track 1
/// (subchannels 0, 2; pitch bend), track 2 (subchannels 0, 5); each with
/// notes over two bars in two subchannels and two groups.
fn song(h: &mut History) -> PlumDocument {
    let mut d = PlumDocument::new_project("merge".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    d.add_track().unwrap();
    let subs = [[0u32, 1], [0, 2], [0, 5]];
    for (i, s) in subs.iter().enumerate() {
        if s[1] != 0 {
            let op = Operation::AddSubchannels {
                track: i,
                subs: vec![Subchannel::new(SubchannelId(s[1]), 3 + i as u8)],
            };
            apply(&mut d, h, op);
        }
        let recs: Vec<NoteRecord> = (0..16u64)
            .map(|k| {
                note(
                    k * 240,
                    200 + k,
                    60 + 4 * i as u8 + (k % 3) as u8,
                    i as u8,
                    s[(k % 2) as usize],
                    if k < 8 { 0 } else { 1 + i as u32 },
                )
            })
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    // Groups 1 – 3 exist (notes of unknown groups are saved ungrouped).
    let groups = (1..=3u32)
        .map(|id| plumspirit_engine::model::GroupInfo {
            id,
            name: Some(format!("G{id}")),
            ..Default::default()
        })
        .collect();
    let op = Operation::SetGroups {
        table: Box::new(plumspirit_engine::model::GroupTable::from_entries(groups, 4).unwrap()),
        label: "Group",
    };
    apply(&mut d, h, op);
    let t0 = &mut d.track_mut(0).unwrap().events;
    t0.push(cc(0, 0, 7, 90)).unwrap();
    t0.push(cc(960, 0, 7, 70)).unwrap();
    let t1 = &mut d.track_mut(1).unwrap().events;
    t1.push(bend(240, 1, 9000)).unwrap();
    t1.push(bend(1200, 1, 8192)).unwrap();
    h.reset(true);
    d
}

/// Merge Clips in Sequence on row 0: track 0's own clip (moved to 1,920 and
/// trimmed to content 480 – 1,440), an added clip with notes at 4,800 and a
/// linked instance of track 1's content at 7,680 (its notes read track 1's
/// subchannels). The merged clip plays exactly what the three played (track
/// 1's subchannels mapped onto track 0), the sources are gone (track 0's
/// primary removed, its notes kept; track 1's content and primary
/// untouched), and Undo / Redo are exact.
#[test]
fn merge_in_sequence_plays_what_the_clips_played_with_exact_undo() {
    let mut h = History::new(HistoryLimits::default());
    let mut d = song(&mut h);
    let t0 = ClipId::Primary(d.track(0).unwrap().id());
    let t1 = ClipId::Primary(d.track(1).unwrap().id());
    let op = move_clips(&d, &[(t0, 0, 1_920)]).unwrap();
    apply(&mut d, &mut h, op);
    let op = resize_clip(&d, t0, 1_920, 480, Some(960)).unwrap();
    apply(&mut d, &mut h, op);
    let op = add_clip(&d, 0, 4_800, 1_920).unwrap();
    apply(&mut d, &mut h, op);
    let added = d.clips().contents()[0].id;
    let added_id = d.clips().placements_of(ContentRef::Owned(added))[0].id;
    for r in [note(0, 100, 80, 2, 1, 0), note(480, 100, 81, 2, 0, 1)] {
        let op = Operation::InsertNote {
            track: NoteTarget::Content {
                host: 0,
                content: added,
            },
            record: r,
        };
        apply(&mut d, &mut h, op);
    }
    let op = duplicate_clips(&d, &[(t1, 0, 7_680)], true).unwrap();
    apply(&mut d, &mut h, op);
    let linked = *d
        .clips()
        .placements_on(d.track(0).unwrap().id())
        .iter()
        .find(|p| p.content == ContentRef::Track(d.track(1).unwrap().id()))
        .map(|p| &p.id)
        .unwrap();
    let ids = [linked, added_id, t0];
    let plan = plan_merge(&d, &ids, MergeKind::Sequence, None).unwrap();
    assert_eq!(
        plan.sources.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![t0, added_id, linked],
        "merge order: by start"
    );
    assert_eq!((plan.dest, plan.rows.clone()), (0, vec![0]));
    assert_eq!(plan.start, 1_920);
    let before_sources = played(&d, Some(&ids));
    assert_eq!(plan.notes, before_sources.len());
    // Track 1's subchannels 0 and 2 on track 0 (which has 0 and 1): 0 is
    // taken -> 2, then 2 is taken (just given) -> 3.
    let from_t1: Vec<SubMap> = plan
        .mapping
        .iter()
        .copied()
        .filter(|m| m.table == 1)
        .collect();
    assert_eq!(
        from_t1,
        vec![
            SubMap {
                table: 1,
                from: SubchannelId(0),
                to: SubchannelId(2),
                added: true
            },
            SubMap {
                table: 1,
                from: SubchannelId(2),
                to: SubchannelId(3),
                added: true
            },
        ]
    );
    assert_eq!(
        plan.new_subs
            .iter()
            .map(|s| (s.id.0, s.midi_channel))
            .collect::<Vec<_>>(),
        vec![(2, 0), (3, 4)]
    );
    assert!(
        plan.new_subs[0]
            .name
            .as_deref()
            .unwrap()
            .contains("Subchannel 1")
    );
    let fp_before = d.state_fingerprint();
    let t0_notes: Vec<NoteRecord> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect();
    let events_before = played_events(&d, &ids);
    let op = merge_clips(&d, &plan).unwrap();
    apply(&mut d, &mut h, op);
    assert_eq!(h.undo_label(), Some("Merge Clips in Sequence"));
    let fp_after = d.state_fingerprint();
    // The merged clip, on row 0 at 1,920.
    let merged: Vec<_> = d
        .clips()
        .placements_on(d.track(0).unwrap().id())
        .into_iter()
        .filter(|p| matches!(p.content, ContentRef::Owned(c) if c != added))
        .collect();
    assert_eq!(merged.len(), 1);
    let m = &merged[0];
    assert_eq!(
        (m.start, m.offset, m.length),
        (1_920, 0, Some(plan.end - 1_920))
    );
    // It plays what the sources played, track 1's subchannels mapped.
    let map: BTreeMap<(usize, u32), u32> = plan
        .mapping
        .iter()
        .map(|m| ((m.table, m.from.0), m.to.0))
        .collect();
    let want: Played = {
        let mut v: Played = before_sources
            .iter()
            .map(|&(s, du, p, ve, ch, g, table, sub)| (s, du, p, ve, ch, g, 0, map[&(table, sub)]))
            .collect();
        v.sort();
        v
    };
    assert_eq!(played(&d, Some(&[m.id])), want);
    assert_eq!(
        played_events(&d, &[m.id]),
        events_before,
        "events moved, none lost"
    );
    // The sources are gone; track 0 keeps its notes (unplaced); track 1's
    // content and primary are untouched; the added content is stashed.
    for id in ids {
        assert!(d.clips().placement(id).is_none(), "{id:?}");
    }
    assert_eq!(d.clips().removed_primaries(), &[d.track(0).unwrap().id()]);
    let t0_now: Vec<NoteRecord> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect();
    assert_eq!(t0_now, t0_notes);
    assert!(d.clips().placement(t1).is_some());
    assert!(d.clips().content(added).is_none());
    let subs: Vec<u32> = d
        .track(0)
        .unwrap()
        .subchannels()
        .entries()
        .iter()
        .map(|s| s.id.0)
        .collect();
    assert_eq!(subs, vec![0, 1, 2, 3]);
    // One undo step back, exactly; redo, exactly.
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), fp_before);
    d.validate_clips().unwrap();
    assert_eq!(played(&d, Some(&ids)), before_sources);
    assert!(d.clips().content(added).is_some());
    let subs: Vec<u32> = d
        .track(0)
        .unwrap()
        .subchannels()
        .entries()
        .iter()
        .map(|s| s.id.0)
        .collect();
    assert_eq!(subs, vec![0, 1]);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), fp_after);
}

/// Merge Clips Stacked over three rows: the destination's subchannels stay,
/// the others are mapped in (track, id) order — the same plan every time,
/// for each destination — and the merged clip plays the stack.
#[test]
fn merge_stacked_maps_subchannels_deterministically_for_every_destination() {
    let mut h = History::new(HistoryLimits::default());
    let mut d = song(&mut h);
    let ids: Vec<ClipId> = (0..3)
        .map(|i| ClipId::Primary(d.track(i).unwrap().id()))
        .collect();
    let before = played(&d, Some(&ids));
    let fp_before = d.state_fingerprint();
    let maps = |p: &MergePlan| -> Vec<(usize, u32, u32, bool)> {
        p.mapping
            .iter()
            .map(|m| (m.table, m.from.0, m.to.0, m.added))
            .collect()
    };
    let p0 = plan_merge(&d, &ids, MergeKind::Stack, None).unwrap();
    assert_eq!((p0.dest, p0.rows.clone()), (0, vec![0, 1, 2]));
    assert_eq!(
        maps(&p0),
        vec![
            (0, 0, 0, false),
            (0, 1, 1, false),
            (1, 0, 2, true),
            (1, 2, 3, true),
            (2, 0, 4, true),
            (2, 5, 5, true),
        ]
    );
    let p2 = plan_merge(&d, &ids, MergeKind::Stack, Some(2)).unwrap();
    assert_eq!(
        maps(&p2),
        vec![
            (0, 0, 6, true),
            (0, 1, 1, true),
            (1, 0, 7, true),
            (1, 2, 2, true),
            (2, 0, 0, false),
            (2, 5, 5, false),
        ]
    );
    assert_eq!(
        plan_merge(&d, &ids, MergeKind::Stack, Some(2)).unwrap(),
        p2,
        "deterministic"
    );
    assert_eq!(p2.notes, before.len());
    assert_eq!(
        p2.sources.iter().map(|s| s.row).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "merge order: by track"
    );
    let op = merge_clips(&d, &p2).unwrap();
    apply(&mut d, &mut h, op);
    assert_eq!(h.undo_label(), Some("Merge Clips Stacked"));
    let map: BTreeMap<(usize, u32), u32> = p2
        .mapping
        .iter()
        .map(|m| ((m.table, m.from.0), m.to.0))
        .collect();
    let mut want: Played = before
        .iter()
        .map(|&(s, du, p, ve, ch, g, table, sub)| (s, du, p, ve, ch, g, 2, map[&(table, sub)]))
        .collect();
    want.sort();
    let all = played(&d, None);
    assert_eq!(
        all, want,
        "everything now plays from the merged clip on row 2"
    );
    let subs: Vec<u32> = d
        .track(2)
        .unwrap()
        .subchannels()
        .entries()
        .iter()
        .map(|s| s.id.0)
        .collect();
    assert_eq!(subs, vec![0, 1, 2, 5, 6, 7]);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), fp_before);
    assert_eq!(played(&d, Some(&ids)), before);
}

/// No duplicate controllers (CLAUDE.md, §049): at one tick a later source's
/// Control Change of a number on a channel drops an earlier source's of the
/// same number and channel (counted), other numbers, channels and ticks stay,
/// and duplicates inside one source (imported files) are kept as they are.
#[test]
fn stacked_controllers_never_duplicate_and_later_sources_win() {
    let mut h = History::new(HistoryLimits::default());
    let mut d = song(&mut h);
    // Track 1: CC 7 on channel 0 at 960 (track 0 has one there too), CC 10 at
    // 960, CC 7 on channel 1 at 960, and an imported duplicate CC 11 pair.
    let mut t1 = plumspirit_engine::model::EventStore::new();
    for e in [
        bend(240, 1, 9000),
        cc(480, 0, 11, 1),
        cc(480, 0, 11, 2),
        cc(960, 0, 7, 33),
        cc(960, 0, 10, 64),
        cc(960, 1, 7, 50),
        bend(1200, 1, 8192),
    ] {
        t1.push(e).unwrap();
    }
    d.track_mut(1).unwrap().events = t1;
    let ids: Vec<ClipId> = (0..2)
        .map(|i| ClipId::Primary(d.track(i).unwrap().id()))
        .collect();
    let before = played_events(&d, &ids);
    let p = plan_merge(&d, &ids, MergeKind::Stack, None).unwrap();
    assert_eq!(p.dropped_events, 1);
    assert_eq!(p.events, before.len() - 1);
    let op = merge_clips(&d, &p).unwrap();
    apply(&mut d, &mut h, op);
    let merged = d
        .clips()
        .placements_on(d.track(0).unwrap().id())
        .into_iter()
        .find(|p| matches!(p.content, ContentRef::Owned(_)))
        .unwrap();
    let after = played_events(&d, &[merged.id]);
    let at_960: Vec<_> = after
        .iter()
        .filter(|(t, _)| *t == 960)
        .map(|(_, k)| *k)
        .collect();
    assert_eq!(
        at_960.len(),
        3,
        "CC 7 ch 1 (from track 1), CC 10, CC 7 ch 2 — track 0's CC 7 dropped"
    );
    assert!(at_960.contains(&RawKind::Channel {
        status: 0xB0,
        data: [7, 33]
    }));
    assert!(!at_960.contains(&RawKind::Channel {
        status: 0xB0,
        data: [7, 70]
    }));
    assert_eq!(
        after.iter().filter(|(t, _)| *t == 480).count(),
        2,
        "imported pair kept"
    );
    assert_eq!(after.len(), before.len() - 1);
}

/// Each command says why it cannot run, and nothing is planned.
#[test]
fn ineligible_selections_are_refused_with_a_sentence() {
    let mut h = History::new(HistoryLimits::default());
    let mut d = song(&mut h);
    let p: Vec<ClipId> = (0..3)
        .map(|i| ClipId::Primary(d.track(i).unwrap().id()))
        .collect();
    let msg = |r: Result<MergePlan, ClipEditError>| r.unwrap_err().0;
    assert!(msg(plan_merge(&d, &p[..1], MergeKind::Sequence, None)).contains("at least two"));
    assert!(msg(plan_merge(&d, &[p[0], p[0]], MergeKind::Stack, None)).contains("at least two"));
    assert!(msg(plan_merge(&d, &p, MergeKind::Sequence, None)).contains("single row"));
    // Two clips of one row cannot be stacked.
    let op = add_clip(&d, 0, 9_600, 960).unwrap();
    apply(&mut d, &mut h, op);
    let added = d.clips().explicit_placements()[0].id;
    assert!(
        msg(plan_merge(&d, &[p[0], added], MergeKind::Stack, None)).contains("two or more rows")
    );
    // Rows that do not overlap in time.
    let op = move_clips(&d, &[(p[1], 1, 50_000)]).unwrap();
    apply(&mut d, &mut h, op);
    assert!(msg(plan_merge(&d, &[p[0], p[1]], MergeKind::Stack, None)).contains("overlap"));
    // A muted clip.
    let op = plumspirit_engine::editor::clips::set_muted(&d, &[p[2]], true).unwrap();
    apply(&mut d, &mut h, op);
    assert!(msg(plan_merge(&d, &[p[0], p[2]], MergeKind::Stack, None)).contains("Muted"));
    // A destination that is not one of the rows.
    assert!(msg(plan_merge(&d, &[p[0], added], MergeKind::Sequence, Some(2))).contains("rows"));
}

/// A plan made before the document changed is refused (the Preview is
/// stale), so Apply never builds from old numbers.
#[test]
fn a_stale_plan_is_refused() {
    let mut h = History::new(HistoryLimits::default());
    let mut d = song(&mut h);
    let ids: Vec<ClipId> = (0..2)
        .map(|i| ClipId::Primary(d.track(i).unwrap().id()))
        .collect();
    let plan = plan_merge(&d, &ids, MergeKind::Stack, None).unwrap();
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(100, 10, 99, 0, 0, 0)], false)
        .unwrap();
    assert!(merge_clips(&d, &plan).unwrap_err().0.contains("changed"));
    let plan = plan_merge(&d, &ids, MergeKind::Stack, None).unwrap();
    assert!(merge_clips(&d, &plan).is_ok());
}

/// The merge is journaled like every edit: after a restart it undoes and
/// redoes exactly.
#[test]
fn a_merge_survives_a_restart_in_the_journal() {
    let dir = std::env::temp_dir().join(format!("plumspirit-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("song.lup");
    let jpath = dir.join("song.plh");
    let mut h = History::new(HistoryLimits::default());
    let mut d = song(&mut h);
    save_project(&d, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&jpath, path_key(&file)).unwrap(),
        Some(identity(&d)),
    )
    .unwrap();
    let before = d.state_fingerprint();
    let ids: Vec<ClipId> = (0..3)
        .map(|i| ClipId::Primary(d.track(i).unwrap().id()))
        .collect();
    let plan = plan_merge(&d, &ids, MergeKind::Stack, Some(1)).unwrap();
    let op = merge_clips(&d, &plan).unwrap();
    apply(&mut d, &mut h, op);
    let after = d.state_fingerprint();
    save_project(&d, &file, LupControl::default()).unwrap();
    let st = h.current_state();
    h.mark_saved(st);
    h.persist_saved(st, identity(&d));
    drop(h);
    let (mut d, _) = load_project(&file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&d)).unwrap();
    assert_eq!(d.state_fingerprint(), after);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merge cost at scale (release): two 500,000-note clips stacked and the
/// same pair in sequence; prints plan and build times and the merged store's
/// bytes per note (the Preview's estimate constant).
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn merge_cost_one_million_notes() {
    use std::time::Instant;
    let mut d = PlumDocument::new_project("big".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    for i in 0..2 {
        let recs: Vec<NoteRecord> = (0..500_000u64)
            .map(|k| note(k * 3 + i as u64, 2, 30 + (k % 60) as u8, 0, 0, 0))
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let ids: Vec<ClipId> = (0..2)
        .map(|i| ClipId::Primary(d.track(i).unwrap().id()))
        .collect();
    let t = Instant::now();
    let plan = plan_merge(&d, &ids, MergeKind::Stack, None).unwrap();
    let plan_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let op = merge_clips(&d, &plan).unwrap();
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut h = History::new(HistoryLimits::default());
    let t = Instant::now();
    h.apply(&mut d, op).unwrap();
    let apply_ms = t.elapsed().as_secs_f64() * 1e3;
    let c = &d.clips().contents()[0];
    let per_note = c.notes.heap_bytes() as f64 / c.notes.len() as f64;
    println!(
        "merge stacked 2 x 500,000 notes: plan: {plan_ms:.1} ms, build: {build_ms:.1} ms, apply: {apply_ms:.1} ms, stored {per_note:.1} bytes per note (estimate {} B)",
        plan.bytes
    );
    assert_eq!(c.notes.len(), 1_000_000);
    h.undo(&mut d).unwrap();
    let op = move_clips(&d, &[(ids[1], 0, 2_000_000)]).unwrap();
    apply(&mut d, &mut h, op);
    let t = Instant::now();
    let plan = plan_merge(&d, &ids, MergeKind::Sequence, None).unwrap();
    let plan_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let op = merge_clips(&d, &plan).unwrap();
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    println!("merge in sequence 2 x 500,000 notes: plan: {plan_ms:.1} ms, build: {build_ms:.1} ms");
    apply(&mut d, &mut h, op);
}

/// The Stacked eligibility (every clip overlaps a clip of another row) equals
/// a brute-force check over random selections of 2 – 40 clips on 4 rows
/// (more than 16 clips use the clip-table map), and `check_merge` agrees
/// with `plan_merge`.
#[test]
fn stacked_eligibility_equals_a_brute_force_overlap_check() {
    use plumspirit_engine::editor::clip_merge::check_merge;
    let mut rng = plumspirit_engine::transform::rng::Rng::new(0x5_12);
    let mut seen = [0usize; 2];
    for trial in 0..120 {
        let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 960);
        for _ in 0..3 {
            d.add_track().unwrap();
        }
        let mut h = History::new(HistoryLimits::default());
        let n = 2 + (trial % 39) as i64;
        let spread = if trial % 3 == 0 { 400_000 } else { 40_000 };
        let mut ids = Vec::new();
        for _ in 0..n {
            let row = rng.range_i64(0, 3) as usize;
            let start = rng.range_i64(0, spread) as u64;
            let len = rng.range_i64(1, 6_000) as u64;
            let first = d.clips().next_ids().0;
            let op = add_clip(&d, row, start, len).unwrap();
            apply(&mut d, &mut h, op);
            ids.push(ClipId::Placed(first));
        }
        let spans: Vec<(usize, u64, u64)> = ids
            .iter()
            .map(|&id| {
                let p = d.clips().placement(id).unwrap();
                (
                    d.index_of(p.track).unwrap(),
                    p.start,
                    p.start + p.length.unwrap(),
                )
            })
            .collect();
        let rows: std::collections::BTreeSet<usize> = spans.iter().map(|s| s.0).collect();
        let all = spans
            .iter()
            .all(|a| spans.iter().any(|b| b.0 != a.0 && a.1 < b.2 && b.1 < a.2));
        let want = rows.len() >= 2 && all;
        seen[usize::from(want)] += 1;
        let got = check_merge(&d, &ids, MergeKind::Stack);
        assert_eq!(got.is_ok(), want, "trial {trial}: {got:?}");
        assert_eq!(
            plan_merge(&d, &ids, MergeKind::Stack, None).is_ok(),
            want,
            "trial {trial}"
        );
    }
    assert!(
        seen[0] > 10 && seen[1] > 10,
        "both outcomes covered: {seen:?}"
    );
}
