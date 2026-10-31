//! V09-S15 (contract §094, §219, §257; `docs/PASTE_REPEAT.md` "S15 — the
//! ten clip functions"): the Track List Paste Repeat functions, each against
//! a brute-force expectation on a small song — with every function off the
//! plan is S13's; (1) lane offset with its three edge rules; (2) the
//! acceleration graph and (3) the gap / overlap morph respace whole
//! generations; (4) the per-generation link patterns; (5) transposed
//! generations play derived contents made once per (content, transpose)
//! key, notes outside the keys left out, the source never touched. Every
//! apply is one undo step that undoes and redoes exactly, survives a `.lup`
//! round trip and a journal restart.

use plumspirit_engine::editor::clip_repeat::{
    ClipRepeatPlan, Link, apply_clip_repeat, build_plan, plan_clip_repeat, plan_repeat_clips,
    repeat_clips,
};
use plumspirit_engine::editor::clips::duplicate_clips;
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use plumspirit_engine::transform::param::Curve;
use plumspirit_engine::transform::repeat::{Decimal, RepeatSpec, RepeatUnit, Spacing};
use plumspirit_engine::transform::{ParamSet, ParamValue};

fn note(t: u64, d: u64, p: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        d,
        NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

fn apply(d: &mut PlumDocument, h: &mut History, op: Operation) {
    h.apply(d, op).unwrap();
}

/// A 480-PPQN song with five tracks: track 0 holds a bar of four notes
/// (pitches 60, 64, 67, 72, ending at 1,880), track 1 two notes (48, 55)
/// ending at 1,900; tracks 2 – 4 are empty.
fn song() -> PlumDocument {
    let mut d = PlumDocument::new_project("clips".into(), Bpm::parse("120").unwrap(), 480);
    for _ in 0..4 {
        d.add_track().unwrap();
    }
    let t0 = [
        note(0, 400, 60),
        note(480, 400, 64),
        note(960, 400, 67),
        note(1440, 440, 72),
    ];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&t0, false)
        .unwrap();
    let t1 = [note(0, 900, 48), note(960, 940, 55)];
    d.track_mut(1)
        .unwrap()
        .notes
        .insert_sorted_batch(&t1, false)
        .unwrap();
    d
}

fn primary(d: &PlumDocument, i: usize) -> ClipId {
    ClipId::Primary(d.track(i).unwrap().id())
}

fn end_to_end(count: u32) -> RepeatSpec {
    RepeatSpec {
        count,
        spacing: Spacing::EndToEnd,
        every: Decimal::whole(1),
        unit: RepeatUnit::Ticks,
    }
}

/// The functions' values: switch `n` on with `set`.
fn fx(set: &[(&str, ParamValue)]) -> ParamSet {
    let mut v = ParamSet::new();
    for &(k, ref x) in set {
        v.set(k, x.clone());
    }
    v
}

fn on(n: u8) -> (&'static str, ParamValue) {
    (
        ["a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "a9", "a10"][usize::from(n) - 1],
        ParamValue::Bool(true),
    )
}

/// The new clips of `plan` as (track index, start, generation).
fn cells(p: &ClipRepeatPlan) -> Vec<(u32, u64, u32)> {
    p.clips
        .iter()
        .map(|c| (c.row, c.start, c.generation))
        .collect()
}

/// Applies `plan` as one undo step; checks undo / redo restore the
/// fingerprints exactly; returns the history.
fn apply_checked(d: &mut PlumDocument, plan: &ClipRepeatPlan) -> History {
    let mut h = History::new(HistoryLimits::default());
    let before = d.state_fingerprint();
    let op = apply_clip_repeat(d, plan).unwrap();
    apply(d, &mut h, op);
    let after = d.state_fingerprint();
    h.undo(d).unwrap();
    assert_eq!(d.state_fingerprint(), before, "undo restores the song");
    h.redo(d).unwrap();
    assert_eq!(d.state_fingerprint(), after, "redo restores the repeat");
    h
}

/// The placements added after id `first`: (track index, start, content).
fn added(d: &PlumDocument, first: u32) -> Vec<(usize, u64, ContentRef)> {
    d.clips()
        .explicit_placements()
        .iter()
        .filter(|p| matches!(p.id, ClipId::Placed(k) if k >= first))
        .map(|p| (d.index_of(p.track).unwrap(), p.start, p.content))
        .collect()
}

fn pitches(d: &PlumDocument, c: ContentRef) -> Vec<(u64, u8)> {
    let notes = match c {
        ContentRef::Track(t) => &d.track_by_id(t).unwrap().notes,
        ContentRef::Owned(id) => &d.clips().content(id).unwrap().notes,
    };
    notes
        .iter()
        .map(|n| (n.start_tick, n.attrs.pitch()))
        .collect()
}

#[test]
fn with_every_function_off_the_plan_and_the_result_are_s13s() {
    let d = song();
    let ids = [primary(&d, 0), primary(&d, 1)];
    for linked in [true, false] {
        let p = plan_clip_repeat(
            &d,
            &ids,
            &end_to_end(3),
            Some(480),
            linked,
            &ParamSet::new(),
        )
        .unwrap();
        let base = plan_repeat_clips(&d, &ids, &end_to_end(3), Some(480)).unwrap();
        assert_eq!(p.base, base);
        // Span 0 → 1,900, rounded up to the 480-tick snap: 1,920 per copy.
        assert_eq!(
            cells(&p),
            vec![
                (0, 1920, 1),
                (1, 1920, 1),
                (0, 3840, 2),
                (1, 3840, 2),
                (0, 5760, 3),
                (1, 5760, 3)
            ]
        );
        assert!(p.keys.is_empty() && p.derived_notes == 0);
        let mut a = d.clone();
        let mut b = d.clone();
        let mut h = History::new(HistoryLimits::default());
        apply(&mut a, &mut h, apply_clip_repeat(&d, &p).unwrap());
        apply(
            &mut b,
            &mut h,
            repeat_clips(&d, &ids, &base, linked).unwrap(),
        );
        assert_eq!(a.state_fingerprint(), b.state_fingerprint());
    }
}

#[test]
fn lane_offset_moves_copy_k_by_k_tracks_and_refuses_wraps_or_stops_at_the_edges() {
    let d = song();
    let ids = [primary(&d, 1)];
    let plan = |lanes: i64, edge: u32| {
        plan_clip_repeat(
            &d,
            &ids,
            &end_to_end(3),
            None,
            true,
            &fx(&[
                on(1),
                ("a1.lanes", ParamValue::Int(lanes)),
                ("a1.edge", ParamValue::Choice(edge)),
            ]),
        )
    };
    let rows = |p: ClipRepeatPlan| p.clips.iter().map(|c| c.row).collect::<Vec<_>>();
    // Brute force: copy k on row 1 + k × lanes; five tracks (rows 0 – 4).
    assert_eq!(rows(plan(1, 0).unwrap()), vec![2, 3, 4]);
    let e = plan(2, 0).unwrap_err();
    assert!(
        e.0.contains("Copy 2") && e.0.contains("past the last track"),
        "{}",
        e.0
    );
    assert_eq!(rows(plan(2, 1).unwrap()), vec![3, 0, 2]);
    assert_eq!(rows(plan(2, 2).unwrap()), vec![3, 4, 4]);
    let e = plan(-1, 0).unwrap_err();
    assert!(e.0.contains("past the first track"), "{}", e.0);
    assert_eq!(rows(plan(-1, 1).unwrap()), vec![0, 4, 3]);
    assert_eq!(rows(plan(-1, 2).unwrap()), vec![0, 0, 0]);
    // Applied: the copies sit on those tracks and play track 1's notes.
    let mut d2 = d.clone();
    let first = d2.clips().next_ids().0;
    let p = plan(2, 1).unwrap();
    apply_checked(&mut d2, &p);
    let t1 = ContentRef::Track(d2.track(1).unwrap().id());
    let got = added(&d2, first);
    assert_eq!(
        got.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(),
        vec![(3, 1900), (0, 3800), (2, 5700)]
    );
    assert!(got.iter().all(|x| x.2 == t1), "linked: no note copied");
    assert_eq!(d2.clips().contents().len(), 0);
}

#[test]
fn acceleration_and_the_gap_morph_respace_whole_generations() {
    let d = song();
    let ids = [primary(&d, 0), primary(&d, 1)];
    // Five copies end to end (span 1,900, no snap).
    let starts = |set: &[(&str, ParamValue)]| {
        let p = plan_clip_repeat(&d, &ids, &end_to_end(5), None, true, &fx(set)).unwrap();
        // Both clips of a generation move together.
        for pair in p.clips.chunks(2) {
            assert_eq!(pair[0].start, pair[1].start);
            assert_eq!(pair[0].generation, pair[1].generation);
        }
        p.clips
            .iter()
            .step_by(2)
            .map(|c| c.start)
            .collect::<Vec<_>>()
    };
    assert_eq!(starts(&[]), vec![1900, 3800, 5700, 7600, 9500]);
    // The flat default graph changes nothing.
    assert_eq!(starts(&[on(2)]), starts(&[]));
    // Morph 0 → −50 % of the block: gaps 1,900 − 1,900 × ½ × u.
    let m = starts(&[on(3)]);
    let mut want = Vec::new();
    let mut at = 0u64;
    for k in 0..5u32 {
        let u = f64::from(k) / 4.0;
        at += (1900.0 + 1900.0 * (-0.5 * u)).round() as u64;
        want.push(at);
    }
    assert_eq!(m, want);
    // Rising acceleration (½ → 1, top 300 %) and an opening morph together:
    // gap = 1,900 × factor + 1,900 × m(u).
    let both = starts(&[
        on(2),
        (
            "a2.curve",
            ParamValue::Curve(Curve::new(vec![(0.0, 0.5), (1.0, 1.0)])),
        ),
        ("a2.max", ParamValue::Float(300.0)),
        on(3),
        ("a3.start", ParamValue::Float(10.0)),
        ("a3.end", ParamValue::Float(10.0)),
    ]);
    let mut want = Vec::new();
    let mut at = 0u64;
    for k in 0..5u32 {
        let y = 0.5 + 0.5 * f64::from(k) / 4.0;
        let f = 1.0 + (y - 0.5) * 2.0 * 2.0;
        at += (1900.0 * f + 190.0).round() as u64;
        want.push(at);
    }
    assert_eq!(both, want);
    // A morph that brings a copy onto the previous one is a sentence: Even
    // spacing every 100 ticks, −10 % of the 1,900-tick block from copy 1.
    let e = plan_clip_repeat(
        &d,
        &ids,
        &RepeatSpec {
            count: 3,
            spacing: Spacing::Even,
            every: Decimal::whole(100),
            unit: RepeatUnit::Ticks,
        },
        None,
        true,
        &fx(&[on(3), ("a3.start", ParamValue::Float(-10.0))]),
    )
    .unwrap_err();
    assert!(
        e.0.contains("copy 1 on or before the previous one"),
        "{}",
        e.0
    );
}

#[test]
fn link_patterns_choose_linked_or_independent_per_generation() {
    let d = song();
    let ids = [primary(&d, 0)];
    let t0 = ContentRef::Track(d.track(0).unwrap().id());
    let run = |pattern: u32, every: i64, linked: bool| {
        let mut d2 = d.clone();
        let first = d2.clips().next_ids().0;
        let p = plan_clip_repeat(
            &d2,
            &ids,
            &end_to_end(5),
            None,
            linked,
            &fx(&[
                on(4),
                ("a4.pattern", ParamValue::Choice(pattern)),
                ("a4.every", ParamValue::Int(every)),
            ]),
        )
        .unwrap();
        apply_checked(&mut d2, &p);
        let got = added(&d2, first);
        let contents = d2.clips().contents().len();
        (p, got, contents)
    };
    // Alternate: generations 1, 3, 5 independent (the base choice is
    // overridden either way).
    for linked in [true, false] {
        let (p, got, n) = run(0, 2, linked);
        assert_eq!(
            p.clips.iter().map(|c| c.link).collect::<Vec<_>>(),
            vec![Link::Own, Link::Shared, Link::Own, Link::Shared, Link::Own]
        );
        assert_eq!(got[1].2, t0);
        assert_eq!(got[3].2, t0);
        for i in [0, 2, 4] {
            assert!(matches!(got[i].2, ContentRef::Owned(_)));
        }
        assert_eq!(n, 3);
    }
    // Every 3rd: generations 1 and 4.
    let (p, _, n) = run(1, 3, true);
    assert_eq!(
        p.clips
            .iter()
            .filter(|c| c.link == Link::Own)
            .map(|c| c.generation)
            .collect::<Vec<_>>(),
        vec![1, 4]
    );
    assert_eq!(n, 2);
    // New master: copy 1 independent, the rest play its content.
    let (p, got, n) = run(2, 2, true);
    assert_eq!(p.clips[0].link, Link::Own);
    assert!(p.clips[1..].iter().all(|c| c.link == Link::Same(0)));
    assert!(matches!(got[0].2, ContentRef::Owned(_)));
    assert!(got.iter().all(|x| x.2 == got[0].2));
    assert_eq!(n, 1, "one new content: the master");
}

#[test]
fn transposed_generations_play_one_derived_content_per_key_and_the_source_stays() {
    let mut d = song();
    // A linked duplicate of track 0's primary at bar 9: both selected clips
    // play the same content, so a generation needs one derived content.
    let dup_first = d.clips().next_ids().0;
    let mut h0 = History::new(HistoryLimits::default());
    let op = duplicate_clips(&d, &[(primary(&d, 0), 2, 15_360)], true).unwrap();
    apply(&mut d, &mut h0, op);
    let ids = [primary(&d, 0), ClipId::Placed(dup_first)];
    let t0 = ContentRef::Track(d.track(0).unwrap().id());
    let source = pitches(&d, t0);
    // Step 30, three generations: +30, +60, +90. Pitches 60, 64, 67, 72:
    // +60 leaves 72 out (132); +90 leaves every note out (no clip).
    let set = fx(&[on(5), ("a5.step", ParamValue::Int(30))]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(3), Some(480), true, &set).unwrap();
    assert_eq!(p.keys.len(), 2, "one key per generation, not per clip");
    assert_eq!(p.keys[0].transpose, 30);
    assert_eq!(p.keys[1].transpose, 60);
    assert_eq!(p.derived_notes, 4 + 3);
    assert_eq!(p.left_out_notes, 1);
    assert_eq!(p.empty_copies, 2, "generation 3 of both clips");
    assert_eq!(p.clips.len(), 4);
    let first = d.clips().next_ids().0;
    let contents_before = d.clips().contents().len();
    let mut d2 = d.clone();
    let h = apply_checked(&mut d2, &p);
    assert_eq!(d2.clips().contents().len(), contents_before + 2);
    assert_eq!(pitches(&d2, t0), source, "the source is never rewritten");
    let got = added(&d2, first);
    assert_eq!(got[0].2, got[1].2, "linked copies of a generation share it");
    assert_eq!(got[2].2, got[3].2);
    let g1: Vec<(u64, u8)> = source.iter().map(|&(t, p)| (t, p + 30)).collect();
    assert_eq!(pitches(&d2, got[0].2), g1);
    let g2: Vec<(u64, u8)> = source
        .iter()
        .filter(|x| x.1 + 60 <= 127)
        .map(|&(t, p)| (t, p + 60))
        .collect();
    assert_eq!(pitches(&d2, got[2].2), g2);
    // The derived copies keep the source's window (an explicit length).
    for p in d2.clips().explicit_placements() {
        if matches!(p.id, ClipId::Placed(k) if k >= first) {
            assert_eq!(p.length, Some(1880));
        }
    }
    drop(h);
    // Independent: every copy owns its content; one generation's copies
    // share the derived chunks.
    let p = plan_clip_repeat(&d, &ids, &end_to_end(2), Some(480), false, &set).unwrap();
    let mut d3 = d.clone();
    apply_checked(&mut d3, &p);
    assert_eq!(d3.clips().contents().len(), contents_before + 4);
    let got = added(&d3, first);
    assert_ne!(got[0].2, got[1].2);
    assert_eq!(pitches(&d3, got[0].2), pitches(&d3, got[1].2));
    // 256-Key Mode keeps what the standard keyboard leaves out.
    let mut wide = d.clone();
    wide.project_mut().keys = KeyRange::Extended256;
    let p = plan_clip_repeat(&wide, &ids, &end_to_end(3), Some(480), true, &set).unwrap();
    assert_eq!((p.keys.len(), p.left_out_notes, p.empty_copies), (3, 0, 0));
    // A cycled interval sequence: +2, +2 + 1 = +3, +5.
    let seq = fx(&[on(5), ("a5.seq", ParamValue::List(vec![2, 1]))]);
    let p = plan_clip_repeat(&d, &[primary(&d, 0)], &end_to_end(3), None, true, &seq).unwrap();
    assert_eq!(
        p.keys.iter().map(|k| k.transpose).collect::<Vec<_>>(),
        vec![2, 3, 5]
    );
}

#[test]
fn a_transposed_repeat_round_trips_through_lup_and_survives_a_restart_in_the_journal() {
    let dir = std::env::temp_dir().join(format!("plumspirit-clip-repeat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("song.lup");
    let jpath = dir.join("song.plh");
    let mut d = song();
    save_project(&d, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&jpath, path_key(&file)).unwrap(),
        Some(identity(&d)),
    )
    .unwrap();
    let before = d.state_fingerprint();
    let ids = [primary(&d, 0), primary(&d, 1)];
    let set = fx(&[
        on(1),
        ("a1.edge", ParamValue::Choice(1)),
        on(3),
        on(4),
        ("a4.pattern", ParamValue::Choice(2)),
        on(5),
        ("a5.step", ParamValue::Int(-5)),
    ]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(4), Some(480), true, &set).unwrap();
    let op = apply_clip_repeat(&d, &p).unwrap();
    apply(&mut d, &mut h, op);
    let after = d.state_fingerprint();
    save_project(&d, &file, LupControl::default()).unwrap();
    let (back, _) = load_project(&file, LupControl::default()).unwrap();
    assert_eq!(back.state_fingerprint(), after, ".lup round trip");
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

#[test]
fn a_plan_made_before_the_clips_changed_is_refused() {
    let mut d = song();
    let ids = [primary(&d, 0)];
    let p = build_plan(
        &d,
        &ids,
        plan_repeat_clips(&d, &ids, &end_to_end(2), None).unwrap(),
        true,
        &ParamSet::new(),
    )
    .unwrap();
    let mut h = History::new(HistoryLimits::default());
    let op = duplicate_clips(&d, &[(primary(&d, 1), 3, 0)], true).unwrap();
    apply(&mut d, &mut h, op);
    let e = apply_clip_repeat(&d, &p).unwrap_err();
    assert!(e.0.contains("plan it again"), "{}", e.0);
}

// ------------------------------------------------------------ functions 6 – 10

use plumspirit_engine::model::{Subchannel, SubchannelId};
use plumspirit_engine::transform::repeat::series::picked;
use plumspirit_engine::transform::rng::salt;

/// `song` with track 0's subchannels 1 – 3 added (MIDI channels 5 – 7) and
/// its four notes on subchannels 0, 1, 2, 3 (MIDI channel 0).
fn song_subs() -> PlumDocument {
    let mut d = PlumDocument::new_project("subs".into(), Bpm::parse("120").unwrap(), 480);
    for _ in 0..4 {
        d.add_track().unwrap();
    }
    let subs = (1..4u32)
        .map(|i| Subchannel::new(SubchannelId(i), 4 + i as u8))
        .collect();
    History::default()
        .apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    let recs: Vec<NoteRecord> = [60u8, 64, 67, 72]
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let mut r = note(i as u64 * 480, 400, p);
            r.sub = SubchannelId(i as u32);
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

/// (pitch, subchannel, MIDI channel) of a content's notes.
fn subs_of(d: &PlumDocument, c: ContentRef) -> Vec<(u8, u32, u8)> {
    let notes = match c {
        ContentRef::Track(t) => &d.track_by_id(t).unwrap().notes,
        ContentRef::Owned(id) => &d.clips().content(id).unwrap().notes,
    };
    notes
        .iter()
        .map(|n| (n.attrs.pitch(), n.sub.0, n.attrs.channel()))
        .collect()
}

#[test]
fn subchannel_remap_rotates_or_moves_whole_generations_by_a_deterministic_table() {
    let d = song_subs();
    let ids = [primary(&d, 0)];
    let t0 = ContentRef::Track(d.track(0).unwrap().id());
    let source = subs_of(&d, t0);
    // Rotate within positions 1 – 3 (ids 0, 1, 2), one entry per generation:
    // generation 1 maps 0 → 1 → 2 → 0, generation 2 by two, generation 3
    // is the identity again (it plays the source); id 3 never moves.
    let rot = fx(&[on(6), ("a6.list", ParamValue::List(vec![0, 1, 2]))]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(3), None, true, &rot).unwrap();
    assert_eq!(p.keys.len(), 2);
    assert_eq!(p.remaps.len(), 2);
    assert_eq!(p.clips[2].key, None, "generation 3: the identity");
    let first = d.clips().next_ids().0;
    let mut d2 = d.clone();
    apply_checked(&mut d2, &p);
    let got = added(&d2, first);
    let brute = |rot: usize| -> Vec<(u8, u32, u8)> {
        source
            .iter()
            .map(|&(p, s, ch)| {
                (
                    p,
                    if s < 3 {
                        ((s as usize + rot) % 3) as u32
                    } else {
                        s
                    },
                    ch,
                )
            })
            .collect()
    };
    assert_eq!(subs_of(&d2, got[0].2), brute(1));
    assert_eq!(subs_of(&d2, got[1].2), brute(2));
    assert_eq!(got[2].2, t0);
    assert_eq!(subs_of(&d2, t0), source, "the source keeps its subchannels");
    // Whole clip: generation k → the list's entry k (cycled): 3, 1, 3 —
    // generations 1 and 3 share one derived content.
    let whole = fx(&[
        on(6),
        ("a6.mode", ParamValue::Choice(1)),
        ("a6.list", ParamValue::List(vec![3, 1])),
    ]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(3), None, true, &whole).unwrap();
    assert_eq!(p.keys.len(), 2);
    assert_eq!(p.clips[0].key, p.clips[2].key);
    let mut d3 = d.clone();
    apply_checked(&mut d3, &p);
    let got = added(&d3, first);
    assert!(subs_of(&d3, got[0].2).iter().all(|x| x.1 == 3 && x.2 == 0));
    assert!(subs_of(&d3, got[1].2).iter().all(|x| x.1 == 1 && x.2 == 0));
    // With a transpose too: one key per (transpose, remap).
    let both = fx(&[
        on(5),
        ("a5.step", ParamValue::Int(1)),
        on(6),
        ("a6.list", ParamValue::List(vec![0, 1, 2])),
    ]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(3), None, true, &both).unwrap();
    assert_eq!(p.keys.len(), 3);
    assert_eq!(p.keys[2].remap, None);
    // Positions a track lacks are left out for it: with track 1 (only its
    // first subchannel) selected too, its copies keep their subchannels.
    let two = [primary(&d, 0), primary(&d, 1)];
    let p = plan_clip_repeat(&d, &two, &end_to_end(2), None, true, &fx(&[on(6)])).unwrap();
    assert!(
        p.clips
            .iter()
            .filter(|c| c.source == 1)
            .all(|c| c.key.is_none())
    );
    assert!(
        p.clips
            .iter()
            .filter(|c| c.source == 0)
            .all(|c| c.key.is_some())
    );
    // A list that changes nothing on any selected track is a sentence.
    let bad = fx(&[on(6), ("a6.list", ParamValue::List(vec![0, 9]))]);
    let e = plan_clip_repeat(&d, &ids, &end_to_end(3), None, true, &bad).unwrap_err();
    assert!(
        e.0.contains("fewer than two of the listed subchannels"),
        "{}",
        e.0
    );
    let bad = fx(&[
        on(6),
        ("a6.mode", ParamValue::Choice(1)),
        ("a6.list", ParamValue::List(vec![9])),
    ]);
    let e = plan_clip_repeat(&d, &ids, &end_to_end(3), None, true, &bad).unwrap_err();
    assert!(e.0.contains("have none of the listed"), "{}", e.0);
}

#[test]
fn the_stack_mosaic_spreads_every_generation_over_rows_of_tracks() {
    let d = song();
    let ids = [primary(&d, 0)];
    let mosaic = |extra: &[(&str, ParamValue)]| {
        let mut set = vec![on(7), ("a7.rows", ParamValue::Int(3))];
        set.extend(extra.iter().cloned());
        plan_clip_repeat(&d, &ids, &end_to_end(2), None, true, &fx(&set)).unwrap()
    };
    // Columns 0 (the source's), 1, 2 at 0, 1,880, 3,760; rows 0 – 2 one
    // track apart (the block's height); cell (0, 0) is the source.
    let p = mosaic(&[]);
    let mut want = Vec::new();
    for (k, off) in [(0u32, 0u64), (1, 1880), (2, 3760)] {
        for r in 0..3u32 {
            if (k, r) != (0, 0) {
                want.push((r, off, k));
            }
        }
    }
    assert_eq!(cells(&p), want);
    // Checkerboard and a 50 % stagger: row r starts r × 940 later.
    let p = mosaic(&[
        ("a7.pattern", ParamValue::Choice(1)),
        ("a7.stagger", ParamValue::Float(50.0)),
    ]);
    let want: Vec<(u32, u64, u32)> = want
        .into_iter()
        .filter(|&(r, _, k)| (k + r) % 2 == 0)
        .map(|(r, off, k)| (r, off + u64::from(r) * 940, k))
        .collect();
    assert_eq!(cells(&p), want);
    // A block two tracks high steps two tracks per row; the source column's
    // copies play the source's own content even with a transpose.
    let two = [primary(&d, 0), primary(&d, 1)];
    let set = fx(&[on(7), on(5)]);
    let p = plan_clip_repeat(&d, &two, &end_to_end(1), None, true, &set).unwrap();
    assert_eq!(
        cells(&p),
        vec![
            (2, 0, 0),
            (3, 0, 0),
            (0, 1900, 1),
            (1, 1900, 1),
            (2, 1900, 1),
            (3, 1900, 1)
        ]
    );
    assert_eq!((p.clips[0].key, p.clips[1].key), (None, None));
    assert!(p.clips[2..].iter().all(|c| c.key.is_some()));
    // Past the last track: Refuse unless function 1 says otherwise.
    let e = plan_clip_repeat(
        &d,
        &two,
        &end_to_end(1),
        None,
        true,
        &fx(&[on(7), ("a7.rows", ParamValue::Int(4))]),
    )
    .unwrap_err();
    assert!(e.0.starts_with("A mosaic row would move a clip"), "{}", e.0);
    // It names the mosaic's own way out first (Lane Offset is off here).
    assert!(e.0.contains("negative row step (Stack Mosaic)"), "{}", e.0);
    let mut d2 = d.clone();
    apply_checked(&mut d2, &mosaic(&[]));
}

#[test]
fn ping_pong_mirrors_the_block_on_alternate_generations() {
    let mut d = song();
    // Block: track 0's primary (0 – 1,880) and a linked copy of track 1's
    // primary on track 2 at 2,000 (2,000 – 3,900).
    let dup = d.clips().next_ids().0;
    let mut h = History::new(HistoryLimits::default());
    let op = duplicate_clips(&d, &[(primary(&d, 1), 2, 2000)], true).unwrap();
    apply(&mut d, &mut h, op);
    let ids = [primary(&d, 0), ClipId::Placed(dup)];
    let run = |mode: u32| {
        plan_clip_repeat(
            &d,
            &ids,
            &end_to_end(3),
            None,
            true,
            &fx(&[on(8), ("a8.mode", ParamValue::Choice(mode))]),
        )
        .unwrap()
    };
    // Copies every 3,900. Generations 1 and 3 reversed in time: a clip at p,
    // len long, goes to 3,900 − p − len inside its block.
    assert_eq!(
        cells(&run(0)),
        vec![
            (0, 3900 + 2020, 1),
            (2, 3900, 1),
            (0, 7800, 2),
            (2, 9800, 2),
            (0, 11_700 + 2020, 3),
            (2, 11_700, 3)
        ]
    );
    // Track order: rows mirrored within the block's rows 0 – 2.
    assert_eq!(
        cells(&run(1)),
        vec![
            (2, 3900, 1),
            (0, 5900, 1),
            (0, 7800, 2),
            (2, 9800, 2),
            (2, 11_700, 3),
            (0, 13_700, 3)
        ]
    );
    let both = run(2);
    assert_eq!((both.clips[0].row, both.clips[0].start), (2, 3900 + 2020));
    let mut d2 = d.clone();
    apply_checked(&mut d2, &both);
}

#[test]
fn omission_and_mutation_pick_whole_generations_by_seed() {
    let d = song();
    let ids = [primary(&d, 0), primary(&d, 1)];
    let half = Curve::new(vec![(0.0, 0.5), (1.0, 0.5)]);
    let run = |mode: u32, seed: i64, kinds: &[(&str, ParamValue)]| {
        let mut set = vec![
            on(9),
            ("a9.mode", ParamValue::Choice(mode)),
            ("a9.curve", ParamValue::Curve(half.clone())),
            ("a9.seed", ParamValue::Int(seed)),
        ];
        set.extend(kinds.iter().cloned());
        plan_clip_repeat(&d, &ids, &end_to_end(40), None, true, &fx(&set)).unwrap()
    };
    let gens = |p: &ClipRepeatPlan| {
        let mut g: Vec<u32> = p.clips.iter().map(|c| c.generation).collect();
        g.dedup();
        g
    };
    let a = run(0, 7, &[]);
    assert_eq!(a, run(0, 7, &[]), "deterministic");
    assert_ne!(
        gens(&a),
        gens(&run(0, 8, &[])),
        "another seed, other generations"
    );
    // Omitted: exactly the generations the KEEP draw picks; the rest keep
    // their times; both clips of a generation go together.
    let kept: Vec<u32> = (1..=40)
        .filter(|&k| !picked(7, salt::KEEP, k, 40, &half))
        .collect();
    assert_eq!(gens(&a), kept);
    assert_eq!(a.omitted as usize, 40 - kept.len());
    assert_eq!(a.clips.len(), kept.len() * 2);
    for c in &a.clips {
        assert_eq!(c.start % 1900, 0);
        assert_eq!(c.start / 1900, u64::from(c.generation));
    }
    // Mutate (mute): the MUTATE draw's generations are muted, none omitted.
    let m = run(1, 7, &[]);
    let muted: Vec<u32> = (1..=40)
        .filter(|&k| picked(7, salt::MUTATE, k, 40, &half))
        .collect();
    let mut got: Vec<u32> = m
        .clips
        .iter()
        .filter(|c| c.muted)
        .map(|c| c.generation)
        .collect();
    got.dedup();
    assert_eq!(got, muted);
    assert_eq!((m.omitted, m.mutated as usize), (0, muted.len()));
    // Transposing mutation: within ± 5, never 0, one key per amount.
    let t = run(
        1,
        7,
        &[
            ("a9.mute", ParamValue::Bool(false)),
            ("a9.transpose", ParamValue::Bool(true)),
            ("a9.range", ParamValue::Int(5)),
        ],
    );
    for c in &t.clips {
        let picked_k = muted.contains(&c.generation);
        match c.key {
            Some(k) => {
                assert!(picked_k);
                let tr = t.keys[k as usize].transpose;
                assert!(tr != 0 && tr.abs() <= 5, "{tr}");
            }
            None => assert!(!picked_k),
        }
    }
    // Omit and mutate: no omitted generation is present.
    let both = run(2, 7, &[]);
    assert!(gens(&both).iter().all(|k| kept.contains(k)));
    // Applied: the muted clips are muted placements.
    let first = d.clips().next_ids().0;
    let mut d2 = d.clone();
    apply_checked(&mut d2, &m);
    let n_muted = d2
        .clips()
        .explicit_placements()
        .iter()
        .filter(|p| matches!(p.id, ClipId::Placed(k) if k >= first) && p.muted)
        .count();
    assert_eq!(n_muted, muted.len() * 2);
}

#[test]
fn the_recursive_pattern_counts_its_blocks_and_refuses_too_many() {
    let d = song();
    let ids = [primary(&d, 0), primary(&d, 1)];
    let set = fx(&[on(10)]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(2), None, true, &set).unwrap();
    // Per first-level block 1 + 2 + 4 = 7 blocks, two clips each.
    assert_eq!(p.clips.len(), 2 * 7 * 2);
    let blocks: Vec<(u32, u64)> = p
        .clips
        .iter()
        .step_by(2)
        .map(|c| (c.generation, c.start))
        .collect();
    // Level 1 at 1,900 and 3,800 (gap 1,900); children every 950, then
    // every 475.
    for want in [
        (1, 1900),
        (2, 3800),
        (1, 2850),
        (1, 3800),
        (2, 4750),
        (2, 5700),
        (1, 3325),
        (1, 3800),
        (1, 4275),
        (1, 4750),
    ] {
        assert!(blocks.contains(&want), "{want:?} in {blocks:?}");
    }
    let e = plan_clip_repeat(
        &d,
        &ids,
        &end_to_end(1000),
        None,
        true,
        &fx(&[
            on(10),
            ("a10.depth", ParamValue::Int(4)),
            ("a10.children", ParamValue::Int(16)),
        ]),
    )
    .unwrap_err();
    assert!(e.0.contains("139,810,000 clips"), "{}", e.0);
    assert!(e.0.contains("at most 1,000,000"), "{}", e.0);
}

#[test]
fn all_ten_functions_together_apply_undo_redo_and_round_trip() {
    let dir = std::env::temp_dir().join(format!("plumspirit-clip-repeat10-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("song.lup");
    let jpath = dir.join("song.plh");
    let mut d = song_subs();
    save_project(&d, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&jpath, path_key(&file)).unwrap(),
        Some(identity(&d)),
    )
    .unwrap();
    let before = d.state_fingerprint();
    let ids = [primary(&d, 0), primary(&d, 1)];
    let set = fx(&[
        on(1),
        ("a1.edge", ParamValue::Choice(1)),
        on(2),
        on(3),
        ("a3.end", ParamValue::Float(-20.0)),
        on(4),
        on(5),
        ("a5.step", ParamValue::Int(2)),
        on(6),
        on(7),
        on(8),
        on(9),
        ("a9.mode", ParamValue::Choice(2)),
        ("a9.transpose", ParamValue::Bool(true)),
        on(10),
        ("a10.depth", ParamValue::Int(1)),
    ]);
    let p = plan_clip_repeat(&d, &ids, &end_to_end(6), Some(480), false, &set).unwrap();
    assert!(!p.clips.is_empty() && !p.keys.is_empty());
    let op = apply_clip_repeat(&d, &p).unwrap();
    apply(&mut d, &mut h, op);
    let after = d.state_fingerprint();
    assert_eq!(
        d.clips().explicit_placements().len(),
        p.clips.len(),
        "every planned clip is placed"
    );
    save_project(&d, &file, LupControl::default()).unwrap();
    let (back, _) = load_project(&file, LupControl::default()).unwrap();
    assert_eq!(back.state_fingerprint(), after, ".lup round trip");
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

/// Cost (release): ten selected clips of 2,000 notes each, 500 generations
/// over two mosaic rows (10,010 new clips) with every function on —
/// planning, building the operation (derived contents), applying it, undo
/// and redo.
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn clip_repeat_functions_cost_at_scale() {
    use std::time::Instant;
    let mut d = PlumDocument::new_project("cost".into(), Bpm::parse("120").unwrap(), 480);
    for _ in 0..19 {
        d.add_track().unwrap();
    }
    for i in 0..10 {
        let subs = (1..4u32)
            .map(|j| Subchannel::new(SubchannelId(j), j as u8))
            .collect();
        History::default()
            .apply(
                &mut d,
                Operation::AddSubchannels {
                    track: i as usize,
                    subs,
                },
            )
            .unwrap();
        let recs: Vec<NoteRecord> = (0..2000u64)
            .map(|n| {
                let mut r = note(n * 10, 8, 40 + ((n * 7 + i) % 48) as u8);
                r.sub = SubchannelId((n % 4) as u32);
                r
            })
            .collect();
        d.track_mut(i as usize)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let ids: Vec<ClipId> = (0..10).map(|i| primary(&d, i)).collect();
    let set = fx(&[
        on(1),
        ("a1.edge", ParamValue::Choice(1)),
        on(2),
        on(3),
        ("a3.end", ParamValue::Float(-20.0)),
        on(4),
        on(5),
        ("a5.step", ParamValue::Int(1)),
        ("a5.seq", ParamValue::List(vec![1, -1, 2, -2])),
        on(6),
        on(7),
        on(8),
        on(9),
        ("a9.mode", ParamValue::Choice(2)),
        (
            "a9.curve",
            ParamValue::Curve(Curve::new(vec![(0.0, 0.0), (1.0, 0.0)])),
        ),
        on(10),
        ("a10.depth", ParamValue::Int(1)),
        ("a10.children", ParamValue::Int(1)),
    ]);
    let t = Instant::now();
    let p = plan_clip_repeat(&d, &ids, &end_to_end(250), None, true, &set).unwrap();
    let plan_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let op = apply_clip_repeat(&d, &p).unwrap();
    let op_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut h = History::new(HistoryLimits {
        max_bytes: 512 << 20,
        ..HistoryLimits::default()
    });
    let t = Instant::now();
    h.apply(&mut d, op).unwrap();
    let apply_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    h.undo(&mut d).unwrap();
    let undo_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    h.redo(&mut d).unwrap();
    let redo_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "clip repeat, all ten functions: {} new clips, {} derived contents ({} notes), plan {plan_ms:.1} ms, operation {op_ms:.1} ms, apply {apply_ms:.1} ms, undo {undo_ms:.1} ms, redo {redo_ms:.1} ms",
        p.clips.len(),
        p.keys.len(),
        p.derived_notes
    );
    assert!(p.clips.len() >= 10_000);
}
