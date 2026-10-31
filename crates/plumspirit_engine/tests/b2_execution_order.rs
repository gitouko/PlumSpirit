//! v1.0-beta2 B2-F01 (slice B2-S16): the user chooses the order in which a
//! tool's reorderable functions run — the `order` key of the tool's values
//! (stable function ids). No key = the declared order (today's results). The
//! order is semantic: two functions that do not commute give each order's own
//! exact notes; invalid orders are refused with a sentence.

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::transform::definition::{self, ORDER_KEY};
use plumspirit_engine::transform::output::Output;
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_in;
use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TrackScope, run_kernel, tool};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// One track holding `notes`, all selected.
fn doc(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b2".into(), Bpm::DEFAULT, 960);
    let mut v = notes.to_vec();
    v.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

/// The tool's values: its defaults (mandatory controls included), then `set`.
fn values(id: &str, set: &[(&str, ParamValue)]) -> ParamSet {
    let def = definition::find(id).unwrap();
    let mut ps = ParamSet::defaults(&def.specs());
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    ps
}

fn text(s: &str) -> ParamValue {
    ParamValue::Text(s.into())
}

/// Runs `id` over the selection: the result's (start, pitch, subchannel), or
/// the refusal's sentence.
fn run(d: &PlumDocument, id: &str, ps: &ParamSet) -> Result<Vec<(u64, u8, u32)>, String> {
    let t = tool(id).unwrap();
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let ctx = Ctx::simple(960);
    let src = collect_in(t, d, &sc, ps, 1 << 24, Some(&ctx)).map_err(|e| e.user_message())?;
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(ps, &b).map_err(|e| e.user_message())?;
    let r = run_kernel(k.as_ref(), &src, t.boundary(ps), &Limits::default(), None)
        .map_err(|e| e.user_message())?;
    let mut v: Vec<(u64, u8, u32)> = r.notes.iter().map(|n| (n.start, n.pitch, n.sub)).collect();
    v.sort_unstable();
    Ok(v)
}

/// Transpose with the frequency shift (+440 Hz at A4 = 440: A4 → A5, a shift
/// of an octave) and the octave spread into C4 … B4 by pitch class (a clamp
/// into one octave), in the order `order` (None: the declared order).
fn shift_and_clamp(order: Option<&str>) -> ParamSet {
    let mut set = vec![
        ("n08", ParamValue::Bool(true)),
        ("n08.hz", ParamValue::Float(440.0)),
        ("n08.a4", ParamValue::Float(440.0)),
        ("n06", ParamValue::Bool(true)),
        ("n06.lo", ParamValue::Int(60)),
        ("n06.hi", ParamValue::Int(71)),
        ("n06.mode", ParamValue::Choice(2)),
    ];
    if let Some(o) = order {
        set.push((ORDER_KEY, text(o)));
    }
    values("transpose", &set)
}

#[test]
fn shift_then_clamp_and_clamp_then_shift_give_their_own_exact_notes() {
    let d = doc(&[rec(0, 480, 69), rec(960, 480, 64)]);
    // Declared order: n08 (shift) before n06 (clamp): 69 → 81 → 69,
    // 64 → 79 (329.6 + 440 Hz) → 67.
    let declared = run(&d, "transpose", &shift_and_clamp(None)).unwrap();
    assert_eq!(declared, vec![(0, 69, 0), (960, 67, 0)]);
    let explicit = run(&d, "transpose", &shift_and_clamp(Some("n08,n06"))).unwrap();
    assert_eq!(explicit, declared);
    // Clamp first: 69 and 64 stay in the band, then the shift: 81, 79.
    let swapped = run(&d, "transpose", &shift_and_clamp(Some("n06,n08"))).unwrap();
    assert_eq!(swapped, vec![(0, 81, 0), (960, 79, 0)]);
}

#[test]
fn the_whole_source_path_honours_the_order_like_the_per_note_path() {
    // n02 from the lowest note forces the whole-source pipeline (`run9`);
    // factor 1 leaves pitches as they are, so only the order differs.
    let d = doc(&[rec(0, 480, 69), rec(960, 480, 64)]);
    let mut ps = shift_and_clamp(Some("n06,n08"));
    ps.set("n02", ParamValue::Bool(true));
    ps.set("n02.factor", ParamValue::Float(1.0));
    ps.set("n02.ref", ParamValue::Choice(1));
    assert_eq!(
        run(&d, "transpose", &ps).unwrap(),
        vec![(0, 81, 0), (960, 79, 0)]
    );
    ps.set(ORDER_KEY, text("n08,n06"));
    assert_eq!(
        run(&d, "transpose", &ps).unwrap(),
        vec![(0, 69, 0), (960, 67, 0)]
    );
}

#[test]
fn the_per_note_rule_equals_the_whole_run_with_a_custom_order() {
    // Preview = Apply: the lazy viewport preview maps note by note through
    // the kernel's point rule; the commit runs the whole source.
    let d = doc(&[rec(0, 480, 69), rec(960, 480, 64), rec(1920, 480, 50)]);
    let ps = shift_and_clamp(Some("n06,n08"));
    let t = tool("transpose").unwrap();
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let ctx = Ctx::simple(960);
    let src = collect_in(t, &d, &sc, &ps, 1 << 24, Some(&ctx)).unwrap();
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    let point = k.as_point().expect("both functions are per-note rules");
    let mut out = Output::new(t.boundary(&ps), 1 << 20);
    for n in &src.notes {
        point.map(n, &mut out);
    }
    let (mut a, _) = out.finish().unwrap();
    a.sort_by_key(|n| (n.start, n.pitch));
    let whole = run(&d, "transpose", &ps).unwrap();
    let a: Vec<(u64, u8, u32)> = a.iter().map(|n| (n.start, n.pitch, n.sub)).collect();
    assert_eq!(a, whole);
    // 50 → 62 in the band → 77.85 → 78.
    assert_eq!(whole, vec![(0, 81, 0), (960, 79, 0), (1920, 78, 0)]);
}

/// Transpose (0 semitones: the identity) with the Pitch Offset Driver driven
/// by the note index (+12 per note) and the Subchannel Offset Driver driven by
/// the pitch (+1 subchannel per 12 semitones above the lowest, unbounded).
fn drivers(order: Option<&str>) -> ParamSet {
    let mut set = vec![
        ("m.pitch", ParamValue::Bool(true)),
        ("m.pitch.by", ParamValue::Choice(2)),
        ("m.pitch.map", ParamValue::Choice(0)),
        ("m.pitch.every.n", ParamValue::Int(1)),
        ("m.pitch.step", ParamValue::Int(12)),
        ("m.sub", ParamValue::Bool(true)),
        ("m.sub.by", ParamValue::Choice(1)),
        ("m.sub.map", ParamValue::Choice(0)),
        ("m.sub.every.n", ParamValue::Int(12)),
        ("m.sub.step", ParamValue::Int(1)),
        ("m.sub.wrap", ParamValue::Choice(4)),
    ];
    if let Some(o) = order {
        set.push((ORDER_KEY, text(o)));
    }
    values("transpose", &set)
}

#[test]
fn the_offset_drivers_run_in_the_chosen_order() {
    let d = doc(&[rec(0, 480, 60), rec(960, 480, 60)]);
    // Declared: pitch first (60, 72), then the subchannel reads the moved
    // pitch (72 is 12 above the lowest: +1).
    let declared = run(&d, "transpose", &drivers(None)).unwrap();
    assert_eq!(declared, vec![(0, 60, 0), (960, 72, 1)]);
    // Subchannel first: both notes are at the lowest pitch (+0), then the
    // pitch offset.
    let swapped = run(
        &d,
        "transpose",
        &drivers(Some("m_sub_offset,m_pitch_offset")),
    )
    .unwrap();
    assert_eq!(swapped, vec![(0, 60, 0), (960, 72, 0)]);
}

/// The identity Transpose with a pitch offset by note index (−2 per note) and
/// Remove Note Overlaps (exact duplicates, only notes the tool changed).
fn offset_and_overlaps(order: Option<&str>) -> ParamSet {
    let mut set = vec![
        ("m.pitch", ParamValue::Bool(true)),
        ("m.pitch.by", ParamValue::Choice(2)),
        ("m.pitch.map", ParamValue::Choice(0)),
        ("m.pitch.every.n", ParamValue::Int(1)),
        ("m.pitch.step", ParamValue::Int(-2)),
        ("m.ovl", ParamValue::Bool(true)),
        ("m.ovl.mode", ParamValue::Choice(0)),
        ("m.ovl.generated", ParamValue::Bool(true)),
    ];
    if let Some(o) = order {
        set.push((ORDER_KEY, text(o)));
    }
    values("transpose", &set)
}

#[test]
fn overlaps_removed_before_or_after_a_pitch_offset_give_their_own_notes() {
    // C4 and D4 struck together; the offset moves the second note (D4) onto C4.
    let d = doc(&[rec(0, 480, 60), rec(0, 480, 62)]);
    // Declared: the offset makes an exact duplicate, then the overlaps go
    // (the moved copy; the untouched C4 is the user's own).
    let declared = run(&d, "transpose", &offset_and_overlaps(None)).unwrap();
    assert_eq!(declared, vec![(0, 60, 0)]);
    // Overlaps first find none; the offset then stacks the two notes.
    let swapped = run(
        &d,
        "transpose",
        &offset_and_overlaps(Some("m_overlaps,m_pitch_offset")),
    )
    .unwrap();
    assert_eq!(swapped, vec![(0, 60, 0), (0, 60, 0)]);
    // Overlaps listed last: the declared behaviour.
    let last = run(
        &d,
        "transpose",
        &offset_and_overlaps(Some("m_pitch_offset,m_overlaps")),
    )
    .unwrap();
    assert_eq!(last, declared);
}

#[test]
fn an_invalid_order_is_refused_with_a_sentence_never_replaced_by_the_declared_one() {
    let d = doc(&[rec(0, 480, 69)]);
    let mut ps = shift_and_clamp(Some("n08,n06,n08"));
    let e = run(&d, "transpose", &ps).unwrap_err();
    assert!(e.contains("Frequency shift") && e.contains("twice"), "{e}");
    // n04 (pitch order permutation) runs at its own stage of the pipeline.
    ps.set(ORDER_KEY, text("n04,n06,n08"));
    let e = run(&d, "transpose", &ps).unwrap_err();
    assert!(
        e.contains("Pitch order permutation") && e.contains("fixed place"),
        "{e}"
    );
    // The Control Sweep chooses parameters before the tool runs.
    ps.set(ORDER_KEY, text("m_sweep,n06"));
    let e = run(&d, "transpose", &ps).unwrap_err();
    assert!(
        e.contains("Control Sweep") && e.contains("fixed place"),
        "{e}"
    );
    ps.set(ORDER_KEY, text("n06;n08"));
    let e = run(&d, "transpose", &ps).unwrap_err();
    assert!(
        e.contains("execution order") && e.contains("damaged"),
        "{e}"
    );
    ps.set(ORDER_KEY, ParamValue::Int(3));
    let e = run(&d, "transpose", &ps).unwrap_err();
    assert!(
        e.contains("execution order") && e.contains("damaged"),
        "{e}"
    );
}

#[test]
fn unknown_ids_are_left_out_and_missing_ones_keep_their_declared_neighbours() {
    let d = doc(&[rec(0, 480, 69)]);
    // A function this version does not have is left out.
    let ps = shift_and_clamp(Some("n99,n06,n08"));
    assert_eq!(run(&d, "transpose", &ps).unwrap(), vec![(0, 81, 0)]);
    // "n06, n01": n02, n08, n09 (missing) follow n01, their declared
    // predecessor — so n06 still runs before n08.
    let ps = shift_and_clamp(Some("n06, n01"));
    assert_eq!(run(&d, "transpose", &ps).unwrap(), vec![(0, 81, 0)]);
    let def = definition::find("transpose").unwrap();
    let plan = def.plan(&ps).unwrap();
    assert_eq!(
        plan.sequence("pitch").unwrap(),
        ["n06", "n01", "n02", "n08", "n09"]
    );
    let plan = def.plan(&shift_and_clamp(Some("n99,n06,n08"))).unwrap();
    assert_eq!(plan.notices.len(), 1, "{:?}", plan.notices);
    assert!(plan.notices[0].contains("n99"));
}

#[test]
fn the_execution_order_shows_the_chosen_order() {
    let def = definition::find("transpose").unwrap();
    let ids =
        |ps: &ParamSet| -> Vec<&str> { def.execution_order(ps).iter().map(|f| f.id).collect() };
    let mut ps = shift_and_clamp(None);
    ps.set("m.pitch", ParamValue::Bool(true));
    ps.set("m.ovl", ParamValue::Bool(true));
    ps.set("n04", ParamValue::Bool(true));
    assert_eq!(
        ids(&ps),
        ["n04", "n08", "n06", "m_pitch_offset", "m_overlaps"]
    );
    ps.set(ORDER_KEY, text("n06,n08,m_overlaps,m_pitch_offset"));
    assert_eq!(
        ids(&ps),
        ["n04", "n06", "n08", "m_overlaps", "m_pitch_offset"]
    );
    // Groups: Transpose's pitch maps and the platform's Output group.
    let groups = def.groups();
    let keys: Vec<&str> = groups.iter().map(|g| g.key).collect();
    assert_eq!(keys, ["pitch", "output"]);
    assert_eq!(groups[0].members, ["n01", "n02", "n08", "n09", "n06"]);
    assert_eq!(
        groups[1].members,
        ["m_pitch_offset", "m_sub_offset", "m_overlaps"]
    );
}

#[test]
fn every_definition_declares_groups_without_collisions() {
    for def in definition::definitions() {
        assert!(def.problems().is_empty(), "{:?}", def.problems());
        // Every declared member is one of the tool's functions (no typo
        // silently leaves one out).
        for spec in plumspirit_engine::transform::order::kernel_groups(def.id) {
            for m in spec.members {
                assert!(
                    def.function(m).is_some(),
                    "{}: {m} is not a function",
                    def.id
                );
            }
        }
        let mut seen = std::collections::HashSet::new();
        for g in def.groups() {
            assert!(
                g.members.len() >= 2,
                "{}: group {} too small",
                def.id,
                g.key
            );
            for m in &g.members {
                assert!(
                    def.function(m).is_some(),
                    "{}: {m} is not a function",
                    def.id
                );
                assert!(seen.insert(*m), "{}: {m} in two groups", def.id);
            }
        }
        // No key: the declared order (today's) for every switch on.
        let mut all = ParamSet::defaults(&def.specs());
        for f in &def.functions {
            all.set(f.switch, ParamValue::Bool(true));
        }
        let plan = def.plan(&all).unwrap();
        assert!(!plan.custom);
        for g in def.groups() {
            assert_eq!(plan.sequence(g.key).unwrap(), g.members.as_slice());
        }
    }
}

/// Runs `id` and gives the result's (start, pitch, velocity).
fn run_v(d: &PlumDocument, id: &str, ps: &ParamSet) -> Result<Vec<(u64, u8, u8)>, String> {
    let t = tool(id).unwrap();
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let ctx = Ctx::simple(960);
    let src = collect_in(t, d, &sc, ps, 1 << 24, Some(&ctx)).map_err(|e| e.user_message())?;
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(ps, &b).map_err(|e| e.user_message())?;
    let r = run_kernel(k.as_ref(), &src, t.boundary(ps), &Limits::default(), None)
        .map_err(|e| e.user_message())?;
    let mut v: Vec<(u64, u8, u8)> = r.notes.iter().map(|n| (n.start, n.pitch, n.vel)).collect();
    v.sort_unstable();
    Ok(v)
}

#[test]
fn note_properties_value_passes_run_in_the_chosen_order() {
    // n03 cycles the velocities 100, 20 over the notes; n10 sets velocity 1
    // where the velocity is at least 50.
    let d = doc(&[rec(0, 480, 60), rec(960, 480, 62)]);
    let set = |order: Option<&str>| {
        let mut v = vec![
            ("n03", ParamValue::Bool(true)),
            ("n03.prop", ParamValue::Choice(0)),
            ("n03.values", ParamValue::List(vec![100, 20])),
            ("n03.per", ParamValue::Choice(0)),
            ("n10", ParamValue::Bool(true)),
            ("n10.if", ParamValue::Choice(1)),
            ("n10.test", ParamValue::Choice(1)),
            ("n10.value", ParamValue::Int(50)),
            ("n10.then", ParamValue::Choice(0)),
            ("n10.to", ParamValue::Int(1)),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("note_props", &v)
    };
    // Declared: the cycle first (100, 20), then the condition (100 → 1).
    assert_eq!(
        run_v(&d, "note_props", &set(None)).unwrap(),
        vec![(0, 60, 1), (960, 62, 20)]
    );
    // The condition first (both written velocities are 100 → 1), then the
    // cycle writes 100, 20.
    assert_eq!(
        run_v(&d, "note_props", &set(Some("n10,n03"))).unwrap(),
        vec![(0, 60, 100), (960, 62, 20)]
    );
}

#[test]
fn humanize_swing_and_rubato_compose_in_the_chosen_order() {
    // Off-beat notes over two bars: the swing (a constant 70 %) moves them,
    // the rubato warps time (30 ms of smooth noise, anchored at bar lines).
    let notes: Vec<NoteRecord> = (0..32).map(|i| rec(i * 240, 120, 60)).collect();
    let d = doc(&notes);
    let set = |swing: bool, rubato: bool, order: Option<&str>| {
        let mut v = vec![
            ("n09", ParamValue::Bool(swing)),
            ("n09.lo", ParamValue::Float(70.0)),
            ("n09.hi", ParamValue::Float(70.0)),
            ("n01", ParamValue::Bool(rubato)),
            ("n01.depth", ParamValue::Float(60.0)),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("humanize", &v)
    };
    let declared = run(&d, "humanize", &set(true, true, None)).unwrap();
    assert_eq!(
        run(&d, "humanize", &set(true, true, Some("n09,n01"))).unwrap(),
        declared
    );
    let swapped = run(&d, "humanize", &set(true, true, Some("n01,n09"))).unwrap();
    assert_ne!(
        swapped, declared,
        "swing then rubato must differ from rubato then swing"
    );
    // Both orders keep the notes in their order (each map is monotone).
    for v in [&declared, &swapped] {
        assert!(v.windows(2).all(|w| w[0].0 <= w[1].0));
        assert_eq!(v.len(), 32);
    }
    // One map alone: the order has nothing to change.
    for (sw, ru) in [(true, false), (false, true)] {
        assert_eq!(
            run(&d, "humanize", &set(sw, ru, Some("n01,n09"))).unwrap(),
            run(&d, "humanize", &set(sw, ru, None)).unwrap()
        );
    }
}

/// [`doc`] with subchannels 0 … `subs` − 1 on the track (colours are places
/// in that list).
fn doc_subs(notes: &[NoteRecord], subs: u32) -> PlumDocument {
    let mut d = PlumDocument::new_project("b2".into(), Bpm::DEFAULT, 960);
    let s: Vec<Subchannel> = (1..subs)
        .map(|i| Subchannel::new(SubchannelId(i), (i % 16) as u8))
        .collect();
    History::default()
        .apply(&mut d, Operation::AddSubchannels { track: 0, subs: s })
        .unwrap();
    let mut v = notes.to_vec();
    v.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

#[test]
fn paint_color_overrides_in_the_chosen_order() {
    // C4 on beat 1, E4 on beat 2. n04: class C → colour 1, class E → 2;
    // n07 (quarter grid): beat 1 → colour 3, beat 2 → 0.
    let d = doc_subs(&[rec(0, 480, 60), rec(960, 480, 64)], 4);
    let set = |order: Option<&str>| {
        let mut v = vec![
            ("n04", ParamValue::Bool(true)),
            (
                "n04.classes",
                ParamValue::List(vec![1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]),
            ),
            ("n07", ParamValue::Bool(true)),
            ("n07.grid", ParamValue::Choice(2)),
            ("n07.palette", ParamValue::List(vec![3, 0])),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("paint_color", &v)
    };
    // Declared: the metric paint (later) wins.
    assert_eq!(
        run(&d, "paint_color", &set(None)).unwrap(),
        vec![(0, 60, 3), (960, 64, 0)]
    );
    // The pitch classes last: they win.
    assert_eq!(
        run(&d, "paint_color", &set(Some("n07,n04"))).unwrap(),
        vec![(0, 60, 1), (960, 64, 2)]
    );
}

#[test]
fn select_steps_run_in_the_chosen_order() {
    // Two chords: C4 + G4, then D4 + A4. Voice select keeps the top voices;
    // the line trace starts at the lowest note of the result's first onset.
    let d = doc(&[
        rec(0, 480, 60),
        rec(0, 480, 67),
        rec(480, 480, 62),
        rec(480, 480, 69),
    ]);
    let set = |order: Option<&str>| {
        let mut v = vec![
            ("n02", ParamValue::Bool(true)),
            ("n02.voice", ParamValue::Choice(0)),
            ("n07", ParamValue::Bool(true)),
            ("n07.start", ParamValue::Choice(2)),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("select", &v)
    };
    // Declared: the top voices (G4, A4), then the line through them.
    assert_eq!(
        run(&d, "select", &set(None)).unwrap(),
        vec![(0, 67, 0), (480, 69, 0)]
    );
    // The line first (C4 → D4), then only top voices stay: none of the line.
    assert_eq!(run(&d, "select", &set(Some("n07,n02"))).unwrap(), vec![]);
}

#[test]
fn duplicate_copy_pitch_maps_run_in_the_chosen_order() {
    // One copy of E4: n12 a fifth up per copy, n07 snaps to C major's tones.
    let d = doc(&[rec(0, 480, 64)]);
    let set = |order: Option<&str>| {
        let mut v = vec![
            ("n12", ParamValue::Bool(true)),
            ("n12.motion", ParamValue::Choice(0)),
            ("n07", ParamValue::Bool(true)),
            ("n07.chords", text("C")),
            ("n07.fit", ParamValue::Choice(1)),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("duplicate", &v)
    };
    let copies = |ps: &ParamSet| -> Vec<u8> {
        run(&d, "duplicate", ps)
            .unwrap()
            .iter()
            .filter(|n| n.0 > 0)
            .map(|n| n.1)
            .collect()
    };
    // Declared: E4 + 7 = B4, snapped to C5.
    assert_eq!(copies(&set(None)), vec![72]);
    // Snapped first (E4 is a chord tone), then a fifth up: B4.
    assert_eq!(copies(&set(Some("n07,n12"))), vec![71]);
}

fn rec_v(start: u64, dur: u64, pitch: u8, vel: u8) -> NoteRecord {
    NoteRecord {
        attrs: NoteAttrs::new(pitch, vel, 0, 64, NoteOffKind::NoteOff).unwrap(),
        ..rec(start, dur, pitch)
    }
}

#[test]
fn limit_output_shaping_runs_in_the_chosen_order() {
    // C4 (100) and E4 (50) for a beat from 0, G4 (90) and C5 (40) from half
    // a beat. n03: one note sounding, the quietest goes; n04: one onset per
    // tick, the lowest goes.
    let d = doc(&[
        rec_v(0, 960, 60, 100),
        rec_v(0, 960, 64, 50),
        rec_v(480, 480, 67, 90),
        rec_v(480, 480, 72, 40),
    ]);
    let set = |order: Option<&str>| {
        let mut v = vec![
            ("n03", ParamValue::Bool(true)),
            ("n03.most", ParamValue::Int(1)),
            ("n03.first", ParamValue::Choice(0)),
            ("n04", ParamValue::Bool(true)),
            ("n04.most", ParamValue::Int(1)),
            ("n04.per", ParamValue::Choice(0)),
            ("n04.first", ParamValue::Choice(3)),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("limit", &v)
    };
    // Declared (n03 then n04): the cap leaves C4 alone; one onset each.
    assert_eq!(run(&d, "limit", &set(None)).unwrap(), vec![(0, 60, 0)]);
    // Onsets first: E4 and C5 stay (the lowest of each onset goes), then the
    // cap keeps the louder E4.
    assert_eq!(
        run(&d, "limit", &set(Some("n04,n03"))).unwrap(),
        vec![(0, 64, 0)]
    );
}

#[test]
fn paint_v2_colour_stages_run_in_the_chosen_order() {
    // n03: every note takes letter B (subchannel 2); n07: every phrase
    // takes palette entry 3 (subchannel 3).
    let d = doc_subs(&[rec(0, 480, 60), rec(960, 480, 62)], 4);
    let set = |order: Option<&str>| {
        let mut v = vec![
            ("n03", ParamValue::Bool(true)),
            ("n03.axiom", text("B")),
            ("n03.rules", text("A=A")),
            ("n03.iters", ParamValue::Int(1)),
            ("n07", ParamValue::Bool(true)),
            ("n07.by", ParamValue::Choice(0)),
            ("n07.palette", ParamValue::List(vec![3])),
        ];
        if let Some(o) = order {
            v.push((ORDER_KEY, text(o)));
        }
        values("subpaint", &v)
    };
    // Declared: the phrase colour (later) wins.
    assert_eq!(
        run(&d, "subpaint", &set(None)).unwrap(),
        vec![(0, 60, 2), (960, 62, 2)]
    );
    assert_eq!(
        run(&d, "subpaint", &set(Some("n07,n03"))).unwrap(),
        vec![(0, 60, 1), (960, 62, 1)]
    );
}

/// B2-F01 (`perf = true`): what a custom order costs against the declared one —
/// Transpose's pitch maps in one pass over the notes, the Output group with
/// Remove Note Overlaps moved before a driver (one sorted copy of the
/// output for the decisions). Release:
/// `cargo test --release -p plumspirit_engine --test b2_execution_order execution_order_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn execution_order_cost() {
    let n: u64 = std::env::var("B2_ORDER_NOTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let notes: Vec<NoteRecord> = (0..n)
        .map(|i| rec(i * 24, 48, 36 + (i * 7 % 60) as u8))
        .collect();
    let d = doc(&notes);
    let once = |ps: &ParamSet| -> f64 {
        let t = std::time::Instant::now();
        let r = run(&d, "transpose", ps).unwrap();
        assert_eq!(r.len() as u64, n);
        t.elapsed().as_secs_f64() * 1000.0
    };
    // Alternating runs (a warm-up first), the median of seven each.
    let ab = |a: &ParamSet, b: &ParamSet| -> (f64, f64) {
        once(a);
        once(b);
        let (mut x, mut y) = (Vec::new(), Vec::new());
        for _ in 0..7 {
            x.push(once(a));
            y.push(once(b));
        }
        x.sort_by(f64::total_cmp);
        y.sort_by(f64::total_cmp);
        (x[3], y[3])
    };
    let (maps_default, maps_custom) = ab(&shift_and_clamp(None), &shift_and_clamp(Some("n06,n08")));
    // Offsets that keep every note on the keyboard: the pitch cycles 0, 12,
    // 7 by note index; the subchannel steps with time; overlaps removed.
    let mut out_default = offset_and_overlaps(None);
    out_default.set("m.pitch.map", ParamValue::Choice(1));
    out_default.set("m.sub", ParamValue::Bool(true));
    let mut out_custom = out_default.clone();
    out_custom.set(ORDER_KEY, text("m_overlaps,m_sub_offset,m_pitch_offset"));
    let (post_default, post_custom) = ab(&out_default, &out_custom);
    println!(
        "B2-F01 order cost, {n} notes (alternating, median of 7, ms): pitch maps declared {maps_default:.1} / custom {maps_custom:.1}; Output group declared {post_default:.1} / overlaps first {post_custom:.1}"
    );
}

#[test]
fn the_window_lists_fixed_stretches_and_groups_in_run_order() {
    use plumspirit_engine::transform::definition::Phase;
    let def = definition::find("transpose").unwrap();
    let ps = values("transpose", &[]);
    let shape: Vec<(bool, Vec<&str>)> = def
        .phases(&ps)
        .iter()
        .map(|p| match p {
            Phase::Fixed { functions, .. } => (false, functions.iter().map(|f| f.id).collect()),
            Phase::Group { members, .. } => (true, members.iter().map(|f| f.id).collect()),
        })
        .collect();
    let mut tool_stage = vec![
        "a01", "a02", "a03", "a04", "a05", "a06", "a07", "a08", "a09", "a10",
    ];
    tool_stage.extend(["n04", "n05", "n07"]);
    assert_eq!(
        shape,
        vec![
            (false, vec!["m_sweep"]),
            (false, tool_stage),
            (true, vec!["n01", "n02", "n08", "n09", "n06"]),
            (false, vec!["n10", "n11", "n03"]),
            (true, vec!["m_pitch_offset", "m_sub_offset", "m_overlaps"]),
        ]
    );
    // The Control Sweep's reason is its own.
    match &def.phases(&ps)[0] {
        Phase::Fixed { reason, .. } => assert!(reason.contains("parameters")),
        p => panic!("{p:?}"),
    }
}

#[test]
fn a_generator_with_25_functions_reorders_quickly_and_stays_valid() {
    // Lines: 25 functions, a five-member group reversed by single moves.
    let def = definition::find("gen.lines").unwrap();
    assert_eq!(def.functions.len(), 25);
    let mut ps = values("gen.lines", &[]);
    for f in &def.functions {
        ps.set(f.switch, ParamValue::Bool(true));
    }
    let t = std::time::Instant::now();
    let members = def.groups()[0].members.clone();
    let mut reversed = members.clone();
    reversed.reverse();
    // Each place in turn gets its member by single moves up (10 moves).
    let mut moves = 0;
    for (p, want) in reversed.iter().enumerate() {
        loop {
            let seq = def
                .plan(&ps)
                .unwrap()
                .sequence("variants")
                .unwrap()
                .to_vec();
            let at = seq.iter().position(|m| m == want).unwrap();
            if at == p {
                break;
            }
            let o = def.moved_order(&ps, want, -1).unwrap();
            ps.set(ORDER_KEY, text(&o));
            moves += 1;
        }
    }
    assert_eq!(moves, 10);
    let plan = def.plan(&ps).unwrap();
    assert_eq!(plan.sequence("variants").unwrap(), reversed.as_slice());
    assert_eq!(def.execution_order(&ps).len(), 25);
    assert!(t.elapsed().as_millis() < 500, "{:?}", t.elapsed());
}
