//! Wave H note tools (TEST_MATRIX T-FID-H*): Note Properties, Mute, Join,
//! Group, Note Repeat, Slide — behaviour on small fixtures, plus the model
//! changes they rely on (group column, mute bit) through history, `.lup`,
//! export, playback and analytics.

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, TNote, TrackScope, TransformRun, run_kernel, tool,
};

fn rec(start: u64, dur: u64, pitch: u8, vel: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

fn doc(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("h".into(), Bpm::DEFAULT, 960);
    let mut v = notes.to_vec();
    v.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

fn run_doc(d: &PlumDocument, id: &str, set: &[(&str, ParamValue)]) -> TransformRun {
    let t = tool(id)
        .or_else(|| plumspirit_engine::transform::command_tool(id))
        .unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, val) in set {
        ps.set(k, val.clone());
    }
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(t, d, &sc, 1 << 24).unwrap();
    let mut ctx = Ctx::simple(960);
    ctx.groups = std::sync::Arc::new(d.project().groups.clone());
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None).unwrap()
}

fn run(id: &str, notes: &[NoteRecord], set: &[(&str, ParamValue)]) -> Vec<TNote> {
    run_doc(&doc(notes), id, set).notes
}

fn on(n: u8) -> (&'static str, ParamValue) {
    (
        plumspirit_engine::transform::param::ADV_KEYS[usize::from(n) - 1],
        ParamValue::Bool(true),
    )
}

#[test]
fn join_glues_touching_fragments_and_functions_change_how() {
    let frags = [
        rec(0, 100, 60, 40),
        rec(100, 200, 60, 80),
        rec(300, 50, 60, 120),
        rec(350, 150, 60, 60),
    ];
    let base = run("join", &frags, &[]);
    assert_eq!(base.len(), 1);
    assert_eq!((base[0].start, base[0].dur, base[0].vel), (0, 500, 40));
    let periodic = run("join", &frags, &[on(8), ("a8.n", ParamValue::Int(2))]);
    assert_eq!(periodic.len(), 2, "{periodic:?}");
}

fn ch(i: u32) -> ParamValue {
    ParamValue::Choice(i)
}

#[test]
fn join_overlap_union_and_property_policies() {
    let pair = [rec(0, 400, 60, 100), rec(100, 100, 60, 30)];
    // Base: overlapping fragments stay apart; Overlap union merges them.
    assert_eq!(run("join", &pair, &[]).len(), 2);
    let u = run("join", &pair, &[on(2)]);
    assert_eq!((u.len(), u[0].start, u[0].dur, u[0].vel), (1, 0, 400, 100));
    // Gap tolerance joins across a 30-tick gap only when allowed.
    let gapped = [rec(0, 100, 60, 50), rec(130, 100, 60, 50)];
    assert_eq!(run("join", &gapped, &[]).len(), 2);
    let g = run("join", &gapped, &[on(1), ("a1.gap", ParamValue::Int(30))]);
    assert_eq!((g.len(), g[0].dur), (1, 230));
    // Blend / weighted / last / longest.
    let frags = [rec(0, 100, 60, 40), rec(100, 300, 60, 120)];
    assert_eq!(run("join", &frags, &[on(3)])[0].vel, 80);
    assert_eq!(run("join", &frags, &[on(5)])[0].vel, 100);
    assert_eq!(run("join", &frags, &[on(4)])[0].vel, 120);
    assert_eq!(run("join", &frags, &[on(10)])[0].vel, 120);
}

#[test]
fn note_repeat_subdivides_inside_the_span() {
    // One-beat note, repeat 1/16 (240 ticks at 960 PPQN) → 4 notes inside the beat.
    let r = run("repeat", &[rec(0, 960, 60, 100)], &[("len", ch(4))]);
    assert_eq!(
        r.iter().map(|n| (n.start, n.dur)).collect::<Vec<_>>(),
        vec![(0, 240), (240, 240), (480, 240), (720, 240)]
    );
    // A gap shortens each repeat.
    let g = run(
        "repeat",
        &[rec(0, 960, 60, 100)],
        &[("len", ch(4)), on(2), ("a2.gap", ParamValue::Int(40))],
    );
    assert!(g.iter().all(|n| n.dur == 200));
    // A partial last repeat stays inside the note unless overflow is on.
    let p = run("repeat", &[rec(0, 600, 60, 100)], &[("len", ch(4))]);
    assert_eq!(p.last().map(|n| (n.start, n.dur)), Some((480, 120)));
    let o = run(
        "repeat",
        &[rec(0, 600, 60, 100)],
        &[("len", ch(4)), ("overflow", ParamValue::Bool(true))],
    );
    assert_eq!(o.last().map(|n| (n.start, n.dur)), Some((480, 240)));
    // Recursion is bounded by depth (4 × 2² = 16 notes) and keeps the span.
    let rr = run(
        "repeat",
        &[rec(0, 960, 60, 100)],
        &[
            ("len", ch(4)),
            on(10),
            ("a10.depth", ParamValue::Int(2)),
            ("a10.div", ParamValue::Int(2)),
        ],
    );
    assert_eq!(rr.len(), 16);
    assert!(rr.iter().all(|n| n.end() <= 960));
    // Acceleration keeps the span: the first and last repeats touch the edges.
    let a = run("repeat", &[rec(0, 960, 60, 100)], &[("len", ch(4)), on(5)]);
    assert_eq!(a.first().unwrap().start, 0);
    assert_eq!(a.last().unwrap().end(), 960);
    assert!(a[1].start != 240);
}

#[test]
fn mute_notes_sets_the_note_flag_in_one_click() {
    // v0.9 §117: the rule-based Advanced Mute is gone; Mute Notes /
    // Unmute Notes / toggle act on the selected notes, undoable.
    assert!(tool("mute").is_none());
    assert!(plumspirit_engine::transform::removed_tool("mute").is_some());
    let notes = [
        rec(0, 100, 60, 20),
        rec(100, 100, 64, 90),
        rec(200, 100, 67, 120),
    ];
    let all = run("mute_notes", &notes, &[]);
    assert!(all.iter().all(|n| n.muted()));
    let d = doc(&notes);
    let r = run_doc(&d, "mute_notes", &[]);
    let mut h = plumspirit_engine::history::History::default();
    let mut d2 = d.clone();
    plumspirit_engine::transform::commit(&mut h, &mut d2, &r, "Mute").unwrap();
    assert!(d2.track(0).unwrap().notes.iter().all(|v| v.attrs.muted()));
    let un = run_doc(&d2, "mute_notes", &[("action", ch(1))]);
    assert!(un.notes.iter().all(|n| !n.muted()));
    let toggled = run_doc(&d2, "mute_notes", &[("action", ch(2))]);
    assert!(toggled.notes.iter().all(|n| !n.muted()));
    h.undo(&mut d2).unwrap();
    assert_eq!(d2.state_fingerprint(), d.state_fingerprint());
}

#[test]
fn muted_notes_do_not_play_export_or_count_but_save_and_reload() {
    use plumspirit_engine::analysis::{AnalysisIndex, DEFAULT_WINDOW_NS, Scope, Timing};
    use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
    let notes = [
        rec(0, 480, 60, 100),
        rec(0, 480, 64, 100),
        rec(960, 480, 67, 100),
    ];
    // Only the E4 is selected and muted.
    let mut d0 = doc(&notes);
    {
        let store = &mut d0.track_mut(0).unwrap().notes;
        store.clear_selection();
        store.select_in_region(0, 0, 64, 64);
    }
    let r = run_doc(&d0, "mute_notes", &[]);
    let mut d = d0.clone();
    let mut h = plumspirit_engine::history::History::default();
    plumspirit_engine::transform::commit(&mut h, &mut d, &r, "Mute").unwrap();
    assert_eq!(
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .filter(|v| v.attrs.muted())
            .count(),
        1
    );
    // Playback: two notes sound.
    #[derive(Default)]
    struct P(Vec<u8>);
    impl SynthSink for P {
        fn note_on(&mut self, _: u8, key: u8, _: u8) {
            self.0.push(key);
        }
        fn note_off(&mut self, _: u8, _: u8) {}
        fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
        fn silence(&mut self) {}
        fn reset(&mut self) {}
    }
    let idx = std::sync::Arc::new(PlaybackIndex::build(&d, 48_000));
    let mut s = Scheduler::new(idx, SchedulerLimits::default());
    let mut p = P::default();
    s.seek(0, &mut p);
    let mut blocks = 0;
    while !s.finished() && blocks < 100_000 {
        s.advance(64, &mut p);
        blocks += 1;
    }
    p.0.sort_unstable();
    assert_eq!(p.0, vec![60, 67]);
    // Seeking into the muted note does not restrike it.
    let idx = std::sync::Arc::new(PlaybackIndex::build(&d, 48_000));
    let mut s = Scheduler::new(idx, SchedulerLimits::default());
    let mut p = P::default();
    s.seek(4_000, &mut p);
    assert!(!p.0.contains(&64), "{:?}", p.0);
    // Analytics: two notes in total, poly 1 at tick 100.
    let a = AnalysisIndex::for_document(&d);
    let m = a.metrics(
        &Scope::Project,
        &Timing::for_document(&d),
        100,
        DEFAULT_WINDOW_NS,
    );
    assert_eq!(m.total_notes, 2);
    assert_eq!(m.poly, 1);
    // Export: the muted note is not written.
    let bytes = plumspirit_engine::midi::export::export_to_bytes(&d).unwrap();
    let (re, _) = plumspirit_engine::midi::import::import_bytes(&bytes).unwrap();
    let pitches: Vec<u8> = re
        .tracks()
        .iter()
        .flat_map(|t| t.notes.iter().map(|v| v.attrs.pitch()).collect::<Vec<_>>())
        .collect();
    assert_eq!(pitches.len(), 2);
    assert!(!pitches.contains(&64));
    // .lup keeps it, muted.
    let b = plumspirit_engine::native::project_to_bytes(&d).unwrap();
    let (back, rep) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert_eq!(rep.migrated_from, None);
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    plumspirit_engine::native::lup::verify_project(&d, &b[..], Some(b.len() as u64)).unwrap();
}

#[test]
fn groups_are_created_saved_reloaded_undone_and_locked() {
    let notes = [
        rec(0, 100, 60, 90),
        rec(100, 100, 62, 90),
        rec(200, 100, 64, 90),
    ];
    let d0 = doc(&notes);
    let r = run_doc(
        &d0,
        "group",
        &[on(4), ("a4.name", ParamValue::Text("Riff".into()))],
    );
    assert!(r.notes.iter().all(|n| n.group == 1));
    let mut d = d0.clone();
    let mut h = plumspirit_engine::history::History::default();
    plumspirit_engine::transform::commit(&mut h, &mut d, &r, "Group").unwrap();
    assert_eq!(h.undo_len(), 1, "one undo step for notes + table");
    let g = d.project().groups.get(1).unwrap();
    assert_eq!(g.name.as_deref(), Some("Riff"));
    assert!(d.track(0).unwrap().notes.iter().all(|v| v.group == 1));
    // Save / reopen keeps groups exactly.
    let b = plumspirit_engine::native::project_to_bytes(&d).unwrap();
    let (back, _) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    assert_eq!(back.project().groups, d.project().groups);
    // Undo / redo exact.
    let after = d.state_fingerprint();
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), d0.state_fingerprint());
    assert!(d.project().groups.is_empty());
    h.redo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), after);
    // Lock the group: transforms leave its notes alone.
    let lock = run_doc(&d, "group", &[("action", ch(2)), on(3)]);
    plumspirit_engine::transform::commit(&mut h, &mut d, &lock, "Lock").unwrap();
    assert!(d.project().groups.get(1).unwrap().locked);
    let tr = run_doc(&d, "transpose", &[]);
    let pitches: Vec<u8> = tr.notes.iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, vec![60, 62, 64], "locked notes are passed through");
    // Unlock is possible although the notes are locked.
    let un = run_doc(&d, "group", &[("action", ch(2)), on(3), ("a3.mode", ch(1))]);
    plumspirit_engine::transform::commit(&mut h, &mut d, &un, "Unlock").unwrap();
    assert!(!d.project().groups.get(1).unwrap().locked);
    // Temporary groups are not saved: their notes reopen ungrouped.
    let t = run_doc(&d0, "group", &[on(2)]);
    let mut dt = d0.clone();
    plumspirit_engine::transform::commit(&mut h, &mut dt, &t, "Temp").unwrap();
    let b = plumspirit_engine::native::project_to_bytes(&dt).unwrap();
    plumspirit_engine::native::lup::verify_project(&dt, &b[..], Some(b.len() as u64)).unwrap();
    let (back, _) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert!(back.project().groups.is_empty());
    assert!(back.track(0).unwrap().notes.iter().all(|v| v.group == 0));
}

#[test]
fn group_instance_clones_are_linked_and_ungroup_goes_one_level_up() {
    let notes = [rec(0, 100, 60, 90), rec(300, 100, 62, 90)];
    let r = run_doc(
        &doc(&notes),
        "group",
        &[
            on(9),
            ("a9.count", ParamValue::Int(2)),
            ("a9.gap", ParamValue::Int(100)),
        ],
    );
    // Original group 1 + clones 2, 3 offset by span 400 + gap 100.
    let starts: Vec<(u64, u32)> = r.notes.iter().map(|n| (n.start, n.group)).collect();
    assert_eq!(
        starts,
        vec![(0, 1), (300, 1), (500, 2), (800, 2), (1000, 3), (1300, 3)]
    );
    let t = r.groups.unwrap();
    assert!(t.entries().iter().all(|g| g.link == 1), "{t:?}");
    // Nested grouping, then ungroup one level.
    let mut d = doc(&notes);
    let mut h = plumspirit_engine::history::History::default();
    let a = run_doc(&d, "group", &[]);
    plumspirit_engine::transform::commit(&mut h, &mut d, &a, "G").unwrap();
    let b = run_doc(&d, "group", &[on(1)]);
    plumspirit_engine::transform::commit(&mut h, &mut d, &b, "G2").unwrap();
    assert_eq!(d.project().groups.get(1).unwrap().parent, 2);
    let u = run_doc(&d, "group", &[("action", ch(1))]);
    assert!(u.notes.iter().all(|n| n.group == 2));
}

#[test]
fn slide_writes_pitch_bend_glides_and_respects_the_bend_range() {
    use plumspirit_engine::transform::output::Lane;
    let notes = [rec(0, 480, 60, 90), rec(480, 960, 62, 90)];
    let r = run_doc(&doc(&notes), "slide", &[]);
    // Notes unchanged; one glide from 60 into 62: starts 2 semitones below
    // (bend 0 at ±2) and ends at the centre.
    assert_eq!(r.notes.len(), 2);
    let pb: Vec<(u64, u16)> = r
        .events
        .iter()
        .filter(|e| e.lane == Lane::PitchBend)
        .map(|e| (e.tick, e.value))
        .collect();
    assert_eq!(pb.first(), Some(&(480, 0)));
    assert_eq!(pb.last().map(|p| p.1), Some(8192));
    assert!(pb.windows(2).all(|w| w[0].1 <= w[1].1), "monotone glide");
    // A 7-semitone glide exceeds ±2: skipped (message), nothing written.
    let wide = [rec(0, 480, 60, 90), rec(480, 960, 67, 90)];
    let r = run_doc(&doc(&wide), "slide", &[]);
    assert!(r.events.is_empty());
    assert!(r.message.unwrap().contains("exceed the bend range"));
    // With range 12 and RPN 0 it is written, with the RPN first.
    let r = run_doc(
        &doc(&wide),
        "slide",
        &[
            ("range", ParamValue::Int(12)),
            ("rpn", ParamValue::Bool(true)),
        ],
    );
    assert!(r.events.iter().any(|e| e.lane == Lane::Cc(101)));
    assert!(
        r.events
            .iter()
            .any(|e| e.lane == Lane::Cc(6) && e.value == 12)
    );
    // Committing writes the events into the track (one undo step).
    let mut d = doc(&wide);
    let before = d.state_fingerprint();
    let mut h = plumspirit_engine::history::History::default();
    plumspirit_engine::transform::commit(&mut h, &mut d, &r, "Slide").unwrap();
    assert!(d.track(0).unwrap().events.len() >= 4 + 16);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
    // Note steps instead of bend: a chromatic run into the target.
    let steps = run_doc(&doc(&wide), "slide", &[on(10)]);
    assert!(steps.events.is_empty());
    assert!(steps.notes.len() > 3);
    assert_eq!(steps.notes.last().unwrap().pitch, 67);
}

#[test]
fn note_properties_set_mixed_fields_and_create_missing_groups() {
    let notes = [rec(0, 100, 60, 20), rec(100, 200, 64, 90)];
    let r = run_doc(
        &doc(&notes),
        "note_props",
        &[
            ("vel.on", ParamValue::Bool(true)),
            ("vel", ParamValue::Int(77)),
            ("grp.on", ParamValue::Bool(true)),
            ("grp", ParamValue::Int(5)),
        ],
    );
    assert!(r.notes.iter().all(|n| n.vel == 77 && n.group == 5));
    // Unset fields stay mixed.
    assert_eq!(
        r.notes.iter().map(|n| n.dur).collect::<Vec<_>>(),
        vec![100, 200]
    );
    assert!(r.groups.unwrap().get(5).is_some());
    // Relative mode and the lock mask.
    let rel = run(
        "note_props",
        &notes,
        &[
            on(1),
            ("a1.vel", ParamValue::Int(10)),
            ("a1.pitch", ParamValue::Int(2)),
        ],
    );
    assert_eq!(
        rel.iter().map(|n| (n.vel, n.pitch)).collect::<Vec<_>>(),
        vec![(30, 62), (100, 66)]
    );
    let locked = run(
        "note_props",
        &notes,
        &[on(1), ("a1.vel", ParamValue::Int(10)), on(7)],
    );
    assert_eq!(
        locked.iter().map(|n| n.vel).collect::<Vec<_>>(),
        vec![20, 90]
    );
    // Normalization: loudest → 110.
    let nm = run("note_props", &notes, &[on(5)]);
    assert_eq!(nm.iter().map(|n| n.vel).max(), Some(110));
}

#[test]
fn group_notes_options_of_generators_register_saved_groups() {
    let notes = [rec(0, 960, 60, 90), rec(960, 960, 64, 90)];
    let d0 = doc(&notes);
    let r = run_doc(&d0, "chop", &[("group", ParamValue::Bool(true))]);
    let t = r.groups.as_ref().expect("new groups registered");
    assert_eq!(t.len(), 2, "one group per chopped note");
    let ids: std::collections::BTreeSet<u32> = r.notes.iter().map(|n| n.group).collect();
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![1, 2]);
    let mut d = d0.clone();
    let mut h = plumspirit_engine::history::History::default();
    plumspirit_engine::transform::commit(&mut h, &mut d, &r, "Chop").unwrap();
    let b = plumspirit_engine::native::project_to_bytes(&d).unwrap();
    let (back, _) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert_eq!(back.state_fingerprint(), d.state_fingerprint());
    // A paste of grouped notes creates fresh groups (same nesting), one undo step.
    let clip = plumspirit_engine::editor::clipboard::Clipboard::copy_selected_with_groups(
        d.track(0).unwrap(),
        &d.project().groups,
    )
    .unwrap()
    .unwrap();
    let (edit, table) = clip.prepare_paste_grouped(&d, 0, 10_000).unwrap();
    let before = d.state_fingerprint();
    h.apply(
        &mut d,
        plumspirit_engine::history::Operation::Group {
            ops: vec![
                plumspirit_engine::history::Operation::SetGroups {
                    table: Box::new(table),
                    label: "Paste",
                },
                plumspirit_engine::history::Operation::Prepared {
                    edit,
                    label: "Paste",
                },
            ],
            label: "Paste",
        },
    )
    .unwrap();
    let pasted: std::collections::BTreeSet<u32> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .filter(|v| v.start_tick >= 10_000)
        .map(|v| v.group)
        .collect();
    assert_eq!(pasted.into_iter().collect::<Vec<_>>(), vec![3, 4]);
    h.undo(&mut d).unwrap();
    assert_eq!(d.state_fingerprint(), before);
}
