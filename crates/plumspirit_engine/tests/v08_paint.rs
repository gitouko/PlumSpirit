//! v0.8 Paint (contract §164–§181, test §247; TEST_MATRIX T8-I-PAINT*):
//! Feature 1 triggers (time / pitch / notes, both directions), the wrap
//! policies, each random function deterministic per seed and changed by the
//! seed, Preview = Apply, previews never touch the project, Undo exact,
//! chord fan-out, neighbour contrast and symmetry semantics.

use plumspirit_engine::history::{History, Operation};
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Subchannel, SubchannelId,
};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ParamValue, TrackScope, TransformError, TransformRun, run_kernel, tool,
};

const ID: &str = "subpaint";

fn rec(start: u64, dur: u64, pitch: u8, vel: u8, sub: u32) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, vel, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(sub),
        group: 0,
    }
}

/// A project (960 PPQ) whose first track has subchannels 1 … `subs` (ids
/// 0 … subs−1, MIDI channels = id) and `notes` selected.
fn doc_with(notes: &[NoteRecord], subs: u32) -> PlumDocument {
    let mut d = PlumDocument::new_project("v08i".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    if subs > 1 {
        let s: Vec<Subchannel> = (1..subs)
            .map(|i| Subchannel::new(SubchannelId(i), (i % 16) as u8))
            .collect();
        h.apply(&mut d, Operation::AddSubchannels { track: 0, subs: s })
            .unwrap();
    }
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

fn run_tool(d: &PlumDocument, set: &[(&str, ParamValue)]) -> Result<TransformRun, TransformError> {
    let def = tool(ID).unwrap();
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

/// Subchannel ids of a run in (start, pitch) order.
fn subs_of(run: &TransformRun) -> Vec<u32> {
    let mut v: Vec<(u64, u8, u32)> = run
        .notes
        .iter()
        .map(|n| (n.start, n.pitch, n.sub))
        .collect();
    v.sort_unstable();
    v.into_iter().map(|x| x.2).collect()
}

fn paint(d: &PlumDocument, set: &[(&str, ParamValue)]) -> Vec<u32> {
    subs_of(&run_tool(d, set).unwrap())
}

fn doc_subs(d: &PlumDocument) -> Vec<u32> {
    let mut v: Vec<(u64, u8, u32)> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start_tick, n.attrs.pitch(), n.sub.0))
        .collect();
    v.sort_unstable();
    v.into_iter().map(|x| x.2).collect()
}

fn int(v: i64) -> ParamValue {
    ParamValue::Int(v)
}
fn ch(v: u32) -> ParamValue {
    ParamValue::Choice(v)
}
fn on() -> ParamValue {
    ParamValue::Bool(true)
}

/// Eight quarter notes (960 ticks apart) climbing from C4, subchannel 1.
fn eight() -> Vec<NoteRecord> {
    (0..8u64)
        .map(|i| rec(i * 960, 480, 60 + i as u8, 100, 0))
        .collect()
}

/// 400 notes of MIDI art (a dense irregular field) for the random functions.
fn field() -> Vec<NoteRecord> {
    let mut v = Vec::new();
    for i in 0..400u64 {
        let x = i.wrapping_mul(2_654_435_761) % 9_973;
        v.push(rec(
            (i * 97 + x % 61) % 20_000,
            60 + x % 300,
            (40 + (x % 41)) as u8,
            (1 + x % 127) as u8,
            (x % 4) as u32,
        ));
    }
    v
}

#[test]
fn feature_1_offsets_after_every_interval_of_time_pitch_or_notes() {
    let d = doc_with(&eight(), 4);
    // Time: every 1/4 (960), +1, cycling 4 subchannels.
    assert_eq!(paint(&d, &[]), vec![0, 1, 2, 3, 0, 1, 2, 3]);
    // Every 1/2, −1.
    assert_eq!(
        paint(&d, &[("every", ch(1)), ("offset", int(-1))]),
        vec![0, 0, 3, 3, 2, 2, 1, 1]
    );
    // Backward: counted from the last note.
    assert_eq!(paint(&d, &[("dir", ch(1))]), vec![3, 2, 1, 0, 3, 2, 1, 0]);
    // Pitch: every 3 semitones from the lowest.
    assert_eq!(
        paint(&d, &[("trigger", ch(1)), ("every.pitch", int(3))]),
        vec![0, 0, 0, 1, 1, 1, 2, 2]
    );
    // Notes: every 3 notes, +2.
    assert_eq!(
        paint(
            &d,
            &[
                ("trigger", ch(2)),
                ("every.notes", int(3)),
                ("offset", int(2))
            ]
        ),
        vec![0, 0, 0, 2, 2, 2, 0, 0]
    );
    // Offset 0 changes nothing.
    assert!(matches!(
        run_tool(&d, &[("offset", int(0))]).and_then(|r| build_operation(
            &mut d.clone(),
            &r,
            "Paint"
        )),
        Err(TransformError::Unchanged)
    ));
}

#[test]
fn wrap_policies_cycle_wrap_reflect_clamp_or_grow() {
    let d = doc_with(&eight(), 4);
    let range = |w: u32| -> Vec<u32> {
        paint(
            &d,
            &[("wrap", ch(w)), ("range.lo", int(2)), ("range.n", int(3))],
        )
    };
    // Range = subchannels 2 … 4 (ids 1 … 3). Notes start on id 0.
    assert_eq!(range(1), vec![3, 1, 2, 3, 1, 2, 3, 1]);
    assert_eq!(range(2), vec![2, 1, 2, 3, 2, 1, 2, 3]);
    assert_eq!(range(3), vec![1, 1, 2, 3, 3, 3, 3, 3]);
    // Unbounded: ids beyond the table, created when applied (one undo step).
    let mut d2 = d.clone();
    let mut h = History::default();
    let r = run_tool(&d2, &[("wrap", ch(4)), ("offset", int(3))]).unwrap();
    assert_eq!(subs_of(&r), vec![0, 3, 6, 9, 12, 15, 18, 21]);
    let before = d2.fingerprint();
    let op = build_operation(&mut d2, &r, "Paint").unwrap();
    h.apply(&mut d2, op).unwrap();
    assert_eq!(doc_subs(&d2), vec![0, 3, 6, 9, 12, 15, 18, 21]);
    assert!(
        d2.track(0)
            .unwrap()
            .subchannels()
            .contains(SubchannelId(21))
    );
    h.undo(&mut d2).unwrap();
    assert_eq!(d2.fingerprint(), before);
    assert!(
        !d2.track(0)
            .unwrap()
            .subchannels()
            .contains(SubchannelId(21))
    );
}

#[test]
fn every_random_function_is_deterministic_and_seed_sensitive() {
    // §165 / §247.
    let d = doc_with(&field(), 8);
    let cases: Vec<Vec<(&str, ParamValue)>> = vec![
        vec![("jitter", ParamValue::Float(80.0)), ("every", ch(4))],
        vec![("a1", on()), ("every", ch(4))],
        vec![("a2", on()), ("every", ch(4))],
        vec![("a2", on()), ("a2.mode", ch(1)), ("every", ch(4))],
        vec![("a9", on()), ("a9.gap", int(30))],
        vec![("a12", on()), ("every", ch(5)), ("a12.hold", int(3))],
        vec![("a13", on()), ("every", ch(5))],
    ];
    for c in &cases {
        let mut a = c.clone();
        a.push(("seed", int(11)));
        let mut b = c.clone();
        b.push(("seed", int(9_876_543)));
        let first = paint(&d, &a);
        assert_eq!(first, paint(&d, &a), "{c:?}: same seed, same colours");
        let other = paint(&d, &b);
        let differ = first.iter().zip(&other).filter(|(x, y)| x != y).count();
        assert!(
            differ * 10 >= first.len(),
            "{c:?}: another seed changes only {differ} of {}",
            first.len()
        );
    }
}

#[test]
fn preview_equals_apply_and_previews_never_touch_the_project() {
    // §165 / §181: the preview is the run Apply commits; computing it (and
    // cancelling) leaves the project exactly as it was.
    let mut d = doc_with(&field(), 8);
    let set: Vec<(&str, ParamValue)> = vec![
        ("a1", on()),
        ("a4", on()),
        ("a8", on()),
        ("a10", on()),
        ("a14", on()),
        ("every", ch(4)),
        ("seed", int(5)),
    ];
    let before = d.fingerprint();
    let preview = run_tool(&d, &set).unwrap();
    let again = run_tool(&d, &set).unwrap();
    assert_eq!(preview.notes, again.notes);
    assert_eq!(d.fingerprint(), before, "a preview changes nothing");
    let mut h = History::default();
    let op = build_operation(&mut d, &preview, "Paint").unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(doc_subs(&d), subs_of(&preview), "Apply = Preview");
    // Only subchannels change: times, pitches, velocities, channels stay.
    let mut a: Vec<(u64, u64, u8, u8, u8)> = d
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| {
            (
                n.start_tick,
                n.duration_ticks,
                n.attrs.pitch(),
                n.attrs.velocity(),
                n.attrs.channel(),
            )
        })
        .collect();
    a.sort_unstable();
    let mut b: Vec<(u64, u64, u8, u8, u8)> = field()
        .iter()
        .map(|n| {
            (
                n.start_tick,
                n.duration_ticks,
                n.attrs.pitch(),
                n.attrs.velocity(),
                n.attrs.channel(),
            )
        })
        .collect();
    b.sort_unstable();
    assert_eq!(a, b);
    h.undo(&mut d).unwrap();
    assert_eq!(d.fingerprint(), before, "Undo restores exactly");
}

#[test]
fn chord_fan_out_spreads_voices_across_subchannels() {
    // A four-note chord and a three-note chord, offset 0 (Feature 1 idle).
    let notes = [
        rec(0, 960, 60, 100, 0),
        rec(0, 960, 64, 100, 0),
        rec(0, 960, 67, 100, 0),
        rec(0, 960, 72, 100, 0),
        rec(960, 960, 62, 100, 0),
        rec(960, 960, 65, 100, 0),
        rec(960, 960, 69, 100, 0),
    ];
    let d = doc_with(&notes, 8);
    let base = [("offset", int(0)), ("a8", on()), ("a8.center", int(3))];
    // Centred: 4 voices → −1 … +2 around 3; 3 voices → −1 … +1.
    assert_eq!(paint(&d, &base), vec![2, 3, 4, 5, 2, 3, 4]);
    let mut desc = base.to_vec();
    desc.push(("a8.order", ch(1)));
    assert_eq!(paint(&d, &desc), vec![5, 4, 3, 2, 4, 3, 2]);
    let mut wrap = base.to_vec();
    wrap.push(("a8.wrap", int(2)));
    assert_eq!(paint(&d, &wrap), vec![3, 4, 3, 4, 3, 4, 3]);
}

#[test]
fn neighbour_contrast_separates_overlapping_notes() {
    // A block of overlapping notes on adjacent rows, all one colour.
    let notes: Vec<NoteRecord> = (0..24u64)
        .map(|i| rec(i / 6 * 100, 400, 60 + (i % 6) as u8, 100, 0))
        .collect();
    let d = doc_with(&notes, 6);
    let flat = paint(&d, &[("offset", int(0))]);
    assert!(flat.iter().all(|&s| s == 0));
    let run = run_tool(
        &d,
        &[("offset", int(0)), ("a10", on()), ("a10.radius", int(1))],
    )
    .unwrap();
    // Every note differs from its nearest neighbour above and below at the
    // same onset.
    let mut v: Vec<(u64, u8, u32)> = run
        .notes
        .iter()
        .map(|n| (n.start, n.pitch, n.sub))
        .collect();
    v.sort_unstable();
    for w in v.windows(2) {
        if w[0].0 == w[1].0 && w[1].1 == w[0].1 + 1 {
            assert_ne!(w[0].2, w[1].2, "{w:?}");
        }
    }
}

#[test]
fn symmetry_mirrors_the_second_half() {
    let d = doc_with(&eight(), 8);
    // Time trigger every 1/4, +1: 0 1 2 3 4 5 6 7; mirrored around the
    // middle: 0 1 2 3 3 2 1 0.
    assert_eq!(paint(&d, &[("a11", on())]), vec![0, 1, 2, 3, 3, 2, 1, 0]);
    // Complement (cycling 8): −3 −2 −1 −0 → 5 6 7 0.
    assert_eq!(
        paint(&d, &[("a11", on()), ("a11.rel", ch(1))]),
        vec![0, 1, 2, 3, 5, 6, 7, 0]
    );
    // Same: the first half repeats.
    assert_eq!(
        paint(&d, &[("a11", on()), ("a11.rel", ch(2))]),
        vec![0, 1, 2, 3, 0, 1, 2, 3]
    );
}

#[test]
fn graphs_and_field_map_properties_to_offsets() {
    let d = doc_with(&eight(), 8);
    // Pitch graph 0 … 7 over the selection's pitch range (Feature 1 idle).
    assert_eq!(
        paint(&d, &[("offset", int(0)), ("a4", on())]),
        vec![0, 1, 2, 3, 4, 5, 6, 7]
    );
    // Time graph as an absolute subchannel: 1 … 8 → ids 0 … 7.
    assert_eq!(
        paint(
            &d,
            &[
                ("offset", int(0)),
                ("a3", on()),
                ("a3.mode", ch(1)),
                ("a3.lo", int(1)),
                ("a3.hi", int(8))
            ]
        ),
        vec![0, 1, 2, 3, 4, 5, 6, 7]
    );
    // A 2 × 2 field: low-left 0, high-right 3 (cells).
    let f = ParamValue::List(vec![0, 1, 2, 3]);
    assert_eq!(
        paint(
            &d,
            &[
                ("offset", int(0)),
                ("a14", on()),
                ("a14.field", f),
                ("a14.mode", ch(1))
            ]
        ),
        vec![0, 0, 0, 0, 3, 3, 3, 3]
    );
}

#[test]
#[ignore = "measurement (release): cargo test --release -p plumspirit_engine --test v08_paint -- --ignored --nocapture"]
fn paint_cost_of_a_million_notes() {
    let notes: Vec<NoteRecord> = (0..1_000_000u64)
        .map(|i| {
            rec(
                i * 7,
                30,
                (30 + i % 70) as u8,
                (1 + i % 127) as u8,
                (i % 4) as u32,
            )
        })
        .collect();
    let d = doc_with(&notes, 16);
    for (name, set) in [
        ("feature 1", vec![("every", ch(4))]),
        (
            "walk + momentum",
            vec![("every", ch(4)), ("a1", on()), ("a13", on())],
        ),
        (
            "density + fan + regions",
            vec![("a7", on()), ("a8", on()), ("a9", on())],
        ),
        ("contrast", vec![("a10", on())]),
        ("symmetry + field", vec![("a11", on()), ("a14", on())]),
    ] {
        let t = std::time::Instant::now();
        let r = run_tool(&d, &set).unwrap();
        println!("{name}: {} notes in {:?}", r.notes.len(), t.elapsed());
    }
}

/// v0.9 §087 / §212: the v0.8 Paint is shown as "Paint V2"; its stable id
/// `subpaint` is unchanged, so a preset saved by v0.8 still loads and the v0.7
/// Note Brush keeps the id `paint`.
#[test]
fn paint_is_shown_as_paint_v2_and_keeps_its_id_and_presets() {
    use plumspirit_engine::transform::preset;
    let def = tool(ID).expect("subpaint is registered");
    let spec = (def.spec)();
    assert_eq!(spec.name, "Paint V2");
    assert_eq!(spec.id, "subpaint");
    assert_eq!(spec.advanced.len(), 14, "nothing else is reorganised");
    // §213: the owner's "Paint Extra" is Paint V2 in the Iterations section;
    // it is still a Piano Roll tool. B1-0099 moved Duplicate V2 (the former
    // Paste Repeat (notes)) there too, after it.
    use plumspirit_engine::transform::registry::{Family, registry};
    assert_eq!(spec.family, Family::Iterations);
    assert_eq!(Family::Iterations.label(), "Iterations");
    let iterations: Vec<&str> = registry()
        .iter()
        .filter(|d| (d.spec)().family == Family::Iterations)
        .map(|d| d.id)
        .collect();
    assert_eq!(
        iterations,
        vec!["repeat", "subpaint", "paste_repeat_notes"],
        "B2-F14: Chopper V2, Paint V2, then Duplicate V2"
    );
    let d = plumspirit_engine::transform::definition::definition(def);
    assert_eq!(
        d.category,
        plumspirit_engine::transform::definition::Category::PianoRoll
    );
    assert_eq!((tool("paint").unwrap().spec)().name, "Note Brush");
    // A preset written by v0.8 (tool id, never the display name).
    let v08 = "PlumSpirit Preset 1\ntool=subpaint\nname=Every 3 notes\np.every.notes=3\np.offset=2\np.trigger=2\n";
    let (p, skipped) = preset::parse(v08, ID, &spec.params).expect("v0.8 preset loads");
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(p.tool, "subpaint");
    assert_eq!(p.values.get_raw("offset"), Some(&ParamValue::Int(2)));
    assert_eq!(
        preset::parse(&preset::to_text(&p), ID, &spec.params)
            .unwrap()
            .0,
        p
    );
}
