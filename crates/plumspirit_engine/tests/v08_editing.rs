//! v0.8 editing ergonomics (contract §160–§163, §189–§200, tests §243–§245;
//! TEST_MATRIX T8-H-*): ordinary Scale is N → N and Fill is explicit,
//! Subchannel Offset is relative and unbounded (creating missing subchannels,
//! never wrapping MIDI channels), Note Repeat's cumulative Pitch Offset and its
//! order with the Pitch Cycle, Join merges only one note chain, one-click Mute /
//! Unmute with Undo.

use plumspirit_engine::editor::factor::ExactFactor;
use plumspirit_engine::editor::{self, EditControl};
use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::geom::{BoxScale, Fill, GeomBox};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::{
    Boundary, Ctx, Limits, ParamSet, ParamValue, Source, TrackScope, TransformError, TransformRun,
    run_kernel, tool,
};

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, ch: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, ch, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

fn doc_with(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("v08h".into(), Bpm::DEFAULT, 960);
    let mut v = notes.to_vec();
    v.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

const SEL: [TrackScope; 1] = [TrackScope {
    index: 0,
    all: false,
    content: None,
}];

fn run_tool(
    d: &PlumDocument,
    id: &str,
    set: &[(&str, ParamValue)],
) -> Result<TransformRun, TransformError> {
    let def = tool(id)
        .or_else(|| plumspirit_engine::transform::command_tool(id))
        .unwrap();
    let mut values = ParamSet::defaults(&(def.spec)().params);
    for (k, v) in set {
        values.set(k, v.clone());
    }
    let src = collect_for(def, d, &SEL, 10_000_000)?;
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&values, &b)?;
    run_kernel(
        k.as_ref(),
        &src,
        def.boundary(&values),
        &Limits::default(),
        None,
    )
}

/// Runs tool `id` and commits it through the history (one undo step).
fn apply_tool(d: &mut PlumDocument, h: &mut History, id: &str, set: &[(&str, ParamValue)]) {
    let run = run_tool(d, id, set).unwrap();
    let op = build_operation(d, &run, "t").unwrap();
    h.apply(d, op).unwrap();
}

fn notes(d: &PlumDocument) -> Vec<NoteRecord> {
    d.track(0)
        .unwrap()
        .notes
        .iter()
        .map(|v| NoteRecord {
            start_tick: v.start_tick,
            duration_ticks: v.duration_ticks,
            attrs: v.attrs,
            sub: v.sub,
            group: v.group,
        })
        .collect()
}

fn hundred() -> Vec<NoteRecord> {
    let mut s = 12345u64;
    (0..100)
        .map(|i| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            rec(
                i * 97 + (s >> 60),
                60 + (s >> 58) % 400,
                40 + ((s >> 33) % 30) as u8,
                90,
                0,
                0,
            )
        })
        .collect()
}

#[test]
fn ordinary_scale_keeps_n_notes_and_fill_is_explicit() {
    // §243: 100 selected notes → 100 after every ordinary scale.
    let d = doc_with(&hundred());
    let b = d.track(0).unwrap().notes.selection_bounds().unwrap();
    let from = GeomBox {
        t0: b.min_start,
        t1: b.max_end,
        p0: b.min_pitch,
        p1: b.max_pitch,
    };
    let to = GeomBox {
        t0: from.t0,
        t1: from.t0 + (from.t1 - from.t0) * 9 / 5,
        p0: from.p0,
        p1: (from.p0 + (from.p1 - from.p0) * 3).min(127),
    };
    let src = Source::collect(&d, &SEL, 10_000_000).unwrap();
    // Direct handles (X and Y stretched), Fill off: N → N.
    let plain = BoxScale {
        from,
        to,
        fill: None,
        prevent: false,
        cleanup: Default::default(),
    };
    let r = run_kernel(&plain, &src, Boundary::default(), &Limits::default(), None).unwrap();
    assert_eq!(r.notes.len(), 100);
    // Fill only when explicitly enabled: more notes, as intended.
    let filled = BoxScale {
        from,
        to,
        fill: Some(Fill::default()),
        prevent: false,
        cleanup: Default::default(),
    };
    let r = run_kernel(&filled, &src, Boundary::default(), &Limits::default(), None).unwrap();
    assert!(r.notes.len() > 100, "{}", r.notes.len());
    // The Scale dialog (X and Y, exact factors) and its repetition.
    let mut d = doc_with(&hundred());
    let mut h = History::default();
    for (axis, text) in [("x", "1.5"), ("y", "2"), ("x", "1.5"), ("y", "0.5")] {
        let f = ExactFactor::parse(text).unwrap();
        let p = if axis == "x" {
            editor::prepare_scale_x(&d, 0, &f, EditControl::default()).unwrap()
        } else {
            editor::prepare_scale_y(&d, 0, &f).unwrap()
        };
        h.apply(
            &mut d,
            Operation::Prepared {
                edit: p,
                label: "Scale",
            },
        )
        .unwrap();
        assert_eq!(d.track(0).unwrap().notes.len(), 100, "scale {axis} {text}");
    }
}

#[test]
fn subchannel_offset_is_relative_unbounded_and_never_wraps_midi_channels() {
    // Subchannels 1, 3, 5 (ids 0, 2, 4) on MIDI channels 1, 2, 3; +2 → 3, 5, 7.
    let mut d = doc_with(&[
        rec(0, 100, 60, 100, 0, 0),
        rec(100, 100, 61, 100, 1, 2),
        rec(200, 100, 62, 100, 2, 4),
    ]);
    let mut h = History::default();
    let on = |v| ParamValue::Bool(v);
    apply_tool(
        &mut d,
        &mut h,
        "note_props",
        &[("suboff.on", on(true)), ("suboff", ParamValue::Int(2))],
    );
    let n = notes(&d);
    assert_eq!(n.iter().map(|r| r.sub.0).collect::<Vec<_>>(), vec![2, 4, 6]);
    assert_eq!(
        n.iter().map(|r| r.attrs.channel()).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "MIDI channels are untouched"
    );
    // Subchannel 7 (id 6) did not exist: it was created in the same step, on
    // the channel of its note.
    let t = d.track(0).unwrap();
    let s6 = t
        .subchannels()
        .entries()
        .iter()
        .find(|s| s.id == SubchannelId(6));
    assert_eq!(s6.map(|s| s.midi_channel), Some(2));
    // Far beyond 16: no wrapping through MIDI channels.
    apply_tool(
        &mut d,
        &mut h,
        "note_props",
        &[("suboff.on", on(true)), ("suboff", ParamValue::Int(100))],
    );
    assert_eq!(
        notes(&d).iter().map(|r| r.sub.0).collect::<Vec<_>>(),
        vec![102, 104, 106]
    );
    // Below subchannel 1: refused with a sentence, nothing changes.
    let before = d.fingerprint();
    let e = run_tool(
        &d,
        "note_props",
        &[("suboff.on", on(true)), ("suboff", ParamValue::Int(-103))],
    )
    .unwrap_err();
    assert!(e.user_message().contains("below"), "{}", e.user_message());
    assert_eq!(d.fingerprint(), before);
    // Undo restores the subchannels exactly (and the created table entries).
    h.undo(&mut d).unwrap();
    h.undo(&mut d).unwrap();
    assert_eq!(
        notes(&d).iter().map(|r| r.sub.0).collect::<Vec<_>>(),
        vec![0, 2, 4]
    );
    assert!(
        !d.track(0)
            .unwrap()
            .subchannels()
            .entries()
            .iter()
            .any(|s| s.id == SubchannelId(6))
    );
}

#[test]
fn repeat_pitch_offset_is_cumulative_and_composes_with_the_pitch_cycle() {
    // One note of 4 sixteenths at pitch 60; 1/16 repeats.
    let d = doc_with(&[rec(0, 960, 60, 100, 0, 0)]);
    let pitches = |set: &[(&str, ParamValue)]| -> Vec<u8> {
        let mut v: Vec<(u64, u8)> = run_tool(&d, "repeat", set)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start, n.pitch))
            .collect();
        v.sort_unstable();
        v.into_iter().map(|x| x.1).collect()
    };
    assert_eq!(
        pitches(&[]),
        vec![60, 60, 60, 60],
        "offset 0 = v0.7 behaviour"
    );
    assert_eq!(
        pitches(&[("pitch.step", ParamValue::Int(2))]),
        vec![60, 62, 64, 66]
    );
    assert_eq!(
        pitches(&[("pitch.step", ParamValue::Int(-3))]),
        vec![60, 57, 54, 51]
    );
    // Order: base + r × offset + cycle[r] (cycle 0, 12).
    assert_eq!(
        pitches(&[
            ("pitch.step", ParamValue::Int(2)),
            ("a6", ParamValue::Bool(true)),
            ("a6.cycle", ParamValue::List(vec![0, 12])),
        ]),
        vec![60, 74, 64, 78]
    );
}

#[test]
fn join_merges_only_one_note_chain() {
    let joined = |recs: &[NoteRecord]| -> Vec<(u64, u64, u8, u8)> {
        let d = doc_with(recs);
        let mut v: Vec<_> = run_tool(&d, "join", &[])
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start, n.dur, n.pitch, n.vel))
            .collect();
        v.sort_unstable();
        v
    };
    // C5, D5, E5 in a row: three notes, never one.
    let melody = [
        rec(0, 240, 72, 100, 0, 0),
        rec(240, 240, 74, 100, 0, 0),
        rec(480, 240, 76, 100, 0, 0),
    ];
    assert_eq!(joined(&melody).len(), 3);
    // Two touching fragments of one note: earliest start to latest end, the
    // first fragment's velocity.
    assert_eq!(
        joined(&[rec(0, 240, 60, 100, 0, 0), rec(240, 480, 60, 50, 0, 0)]),
        vec![(0, 720, 60, 100)]
    );
    // Not one chain: another MIDI channel, another subchannel.
    assert_eq!(
        joined(&[rec(0, 240, 60, 100, 0, 0), rec(240, 240, 60, 100, 1, 0)]).len(),
        2
    );
    assert_eq!(
        joined(&[rec(0, 240, 60, 100, 0, 0), rec(240, 240, 60, 100, 0, 1)]).len(),
        2
    );
    // Another note group, or muted vs playing: not joined by default.
    let mut g = rec(240, 240, 60, 100, 0, 0);
    g.group = 1;
    let mut dg = doc_with(&[rec(0, 240, 60, 100, 0, 0)]);
    {
        let mut table = dg.project().groups.clone();
        table.insert(plumspirit_engine::model::GroupInfo {
            id: 1,
            ..Default::default()
        });
        dg.project_mut().groups = table;
        dg.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[g], true)
            .unwrap();
    }
    assert_eq!(run_tool(&dg, "join", &[]).unwrap().notes.len(), 2);
    let mut m = rec(240, 240, 60, 100, 0, 0);
    m.attrs = m.attrs.with_muted(true);
    assert_eq!(joined(&[rec(0, 240, 60, 100, 0, 0), m]).len(), 2);
}

#[test]
fn mute_and_unmute_notes_are_one_click_and_undoable() {
    // §244: select, Mute Notes, Unmute Notes, undo both.
    let mut d = doc_with(&hundred());
    let mut h = History::default();
    let muted = |d: &PlumDocument| {
        d.track(0)
            .unwrap()
            .notes
            .iter()
            .filter(|v| v.attrs.muted())
            .count()
    };
    apply_tool(
        &mut d,
        &mut h,
        "mute_notes",
        &[("action", ParamValue::Choice(0))],
    );
    assert_eq!(muted(&d), 100);
    apply_tool(
        &mut d,
        &mut h,
        "mute_notes",
        &[("action", ParamValue::Choice(1))],
    );
    assert_eq!(muted(&d), 0);
    h.undo(&mut d).unwrap();
    assert_eq!(muted(&d), 100);
    h.undo(&mut d).unwrap();
    assert_eq!(muted(&d), 0);
    // v0.9 §117: no Advanced Mute tool any more.
    assert!(tool("mute").is_none());
}

/// v0.9 §118: Slice Notes is one click — the selected notes that strictly
/// contain a cut are cut there (the edit cursor, or a time selection's start
/// and end), every piece keeps all attributes, notes the cut only touches or
/// misses stay whole, unselected notes are untouched, one undo step.
#[test]
fn slice_notes_cuts_the_selected_notes_at_the_cursor_in_one_click() {
    use plumspirit_engine::transform::command_tool;
    let at = |v: &[i64]| [("at", ParamValue::List(v.to_vec()))];
    let def = command_tool("slice_notes").expect("Slice Notes");
    let spec = (def.spec)();
    assert_eq!(spec.name, "Slice Notes");
    assert!(spec.advanced.is_empty(), "no advanced functions");
    assert!(tool("slice_notes").is_none(), "a command, not a tool");
    assert!(
        tool("slice").is_some(),
        "the v0.8 Slice tool stays loadable"
    );

    // Subchannel 3 (id 2) on MIDI channel 2, in note group 1, muted.
    let mut odd = rec(0, 960, 64, 33, 1, 2);
    odd.group = 1;
    odd.attrs = odd.attrs.with_muted(true);
    let base = [rec(0, 960, 60, 100, 0, 0), odd, rec(300, 300, 67, 80, 0, 0)];
    let mut d = doc_with(&base);
    {
        let mut table = d.project().groups.clone();
        table.insert(plumspirit_engine::model::GroupInfo {
            id: 1,
            ..Default::default()
        });
        d.project_mut().groups = table;
        // An unselected note crossing every cut: never touched.
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(0, 2000, 72, 90, 0, 0)], false)
            .unwrap();
    }
    let before = notes(&d);
    let mut h = History::default();

    // Cursor inside: the two long notes are cut, the short one (300–600)
    // is not crossed by 240.
    let run = run_tool(&d, "slice_notes", &at(&[240])).unwrap();
    assert_eq!(run.notes.len(), 5);
    assert_eq!(run.message.as_deref(), Some("2 note(s) cut into 4 pieces."));
    apply_tool(&mut d, &mut h, "slice_notes", &at(&[240]));
    let mut after: Vec<_> = notes(&d)
        .iter()
        .map(|r| {
            (
                r.start_tick,
                r.duration_ticks,
                r.attrs.pitch(),
                r.attrs.velocity(),
                r.attrs.channel(),
                r.sub.0,
                r.group,
                r.attrs.muted(),
            )
        })
        .collect();
    after.sort_unstable();
    assert_eq!(
        after,
        vec![
            (0, 240, 60, 100, 0, 0, 0, false),
            (0, 240, 64, 33, 1, 2, 1, true),
            (0, 2000, 72, 90, 0, 0, 0, false),
            (240, 720, 60, 100, 0, 0, 0, false),
            (240, 720, 64, 33, 1, 2, 1, true),
            (300, 300, 67, 80, 0, 0, 0, false),
        ],
        "pieces keep pitch, velocity, channel, subchannel, group and mute"
    );
    // One undo step restores the notes exactly.
    h.undo(&mut d).unwrap();
    assert_eq!(notes(&d), before);

    // At a note's edge, or outside every note: nothing to apply.
    for edge in [0, 960, 5000] {
        let run = run_tool(&d, "slice_notes", &at(&[edge])).unwrap();
        assert_eq!(run.notes.len(), 3, "cut at {edge}");
        assert_eq!(
            run.message.as_deref(),
            Some("No selected note crosses the cut position.")
        );
        assert!(matches!(
            build_operation(&mut d, &run, "t"),
            Err(TransformError::Unchanged)
        ));
    }

    // Two cuts (a time selection 240–720, given in any order): the long
    // notes become three pieces, the short note (300–600) lies inside the
    // selection and stays whole.
    apply_tool(&mut d, &mut h, "slice_notes", &at(&[720, 240]));
    let mut spans: Vec<(u64, u64, u8)> = notes(&d)
        .iter()
        .filter(|r| r.attrs.pitch() != 72)
        .map(|r| (r.start_tick, r.duration_ticks, r.attrs.pitch()))
        .collect();
    spans.sort_unstable();
    assert_eq!(
        spans,
        vec![
            (0, 240, 60),
            (0, 240, 64),
            (240, 480, 60),
            (240, 480, 64),
            (300, 300, 67),
            (720, 240, 60),
            (720, 240, 64),
        ]
    );
    h.undo(&mut d).unwrap();
    assert_eq!(notes(&d), before);

    // The preflight counts the pieces exactly (only crossed notes grow): 16
    // cuts over the selection estimate the real output, not 17 × the notes.
    let def = command_tool("slice_notes").unwrap();
    let mut values = ParamSet::defaults(&(def.spec)().params);
    let many: Vec<i64> = (1..=16).map(|k| k * 50).collect();
    values.set("at", ParamValue::List(many.clone()));
    let src = collect_for(def, &d, &SEL, 10_000_000).unwrap();
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&values, &b).unwrap();
    let out = run_tool(&d, "slice_notes", &at(&many)).unwrap().notes.len() as u64;
    // 0–960 twice: 16 cuts each (17 pieces); 300–600: cuts 350…550 (5).
    assert_eq!(out, 17 + 17 + 6);
    assert_eq!(k.estimate_source(&src).notes, out);
    assert_eq!(k.estimate(&src.stats).notes, 3 * 17, "the stats-only bound");
}

/// v0.9 §082: Duplicate Up / Down One Octave — every selected note stays and
/// gets a copy ±12 semitones away at the same time with all its attributes;
/// copies beyond the project's keys are left out and reported (a 256-Key
/// project keeps them above 127); one undo step.
#[test]
fn duplicate_one_octave_adds_a_copy_an_octave_up_or_down() {
    use plumspirit_engine::model::project::KeyRange;
    use plumspirit_engine::transform::command_tool;
    let semis = |v: i64| [("semis", ParamValue::Int(v))];
    let spec = (command_tool("duplicate_octave").expect("command").spec)();
    assert!(spec.advanced.is_empty());
    assert!(tool("duplicate_octave").is_none(), "a command, not a tool");
    let mut odd = rec(480, 240, 64, 33, 1, 2);
    odd.group = 1;
    odd.attrs = odd.attrs.with_muted(true);
    let base = [rec(0, 480, 60, 100, 0, 0), odd, rec(960, 10, 120, 90, 0, 0)];
    let mut d = doc_with(&base);
    {
        let mut table = d.project().groups.clone();
        table.insert(plumspirit_engine::model::GroupInfo {
            id: 1,
            ..Default::default()
        });
        d.project_mut().groups = table;
        // Unselected notes are untouched.
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(2000, 10, 50, 90, 0, 0)], false)
            .unwrap();
    }
    let before = notes(&d);
    let key = |r: &NoteRecord| {
        (
            r.start_tick,
            r.duration_ticks,
            r.attrs.pitch(),
            r.attrs.velocity(),
            r.attrs.channel(),
            r.sub.0,
            r.group,
            r.attrs.muted(),
        )
    };
    let mut h = History::default();
    // Up: 120 + 12 is beyond key 127 in a standard project — left out.
    let run = run_tool(&d, "duplicate_octave", &semis(12)).unwrap();
    assert_eq!(run.notes.len(), 5);
    assert_eq!(run.stats.pitch_dropped, 1);
    apply_tool(&mut d, &mut h, "duplicate_octave", &semis(12));
    let mut got: Vec<_> = notes(&d).iter().map(key).collect();
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            (0, 480, 60, 100, 0, 0, 0, false),
            (0, 480, 72, 100, 0, 0, 0, false),
            (480, 240, 64, 33, 1, 2, 1, true),
            (480, 240, 76, 33, 1, 2, 1, true),
            (960, 10, 120, 90, 0, 0, 0, false),
            (2000, 10, 50, 90, 0, 0, 0, false),
        ]
    );
    h.undo(&mut d).unwrap();
    assert_eq!(notes(&d), before);
    // Down.
    apply_tool(&mut d, &mut h, "duplicate_octave", &semis(-12));
    let pitches: Vec<u8> = {
        let mut v: Vec<u8> = notes(&d).iter().map(|r| r.attrs.pitch()).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(pitches, vec![48, 50, 52, 60, 64, 108, 120]);
    h.undo(&mut d).unwrap();
    assert_eq!(notes(&d), before);
    // A 256-Key Mode project keeps the copy above 127.
    d.project_mut().keys = KeyRange::Extended256;
    let run = run_tool(&d, "duplicate_octave", &semis(12)).unwrap();
    assert_eq!(run.notes.len(), 6);
    assert!(run.notes.iter().any(|n| n.pitch == 132));
    // A step of 0 is refused with a sentence.
    assert!(matches!(
        run_tool(&d, "duplicate_octave", &semis(0)),
        Err(TransformError::Invalid(_))
    ));
}
