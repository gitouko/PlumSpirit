//! B1-S37 (B1-0112, B1-0113, contract §017): per-point smoothness and
//! Overshoot through real tools — the tool's own graph changes its result,
//! Preview = Apply (the same kernel twice), out-of-range results stay within
//! the tool's / MIDI's limits, graphs that do not allow overshoot read the
//! curve pinned into 0 – 1, presets round trip and v0.9 presets are
//! unchanged.

use plumspirit_engine::model::TrackId;
use plumspirit_engine::transform::param::{Curve, ParamValue};
use plumspirit_engine::transform::preset::{Preset, parse, to_text};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::source::SourceTrack;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, Source, TNote, TransformRun, run_kernel, tool,
};

fn tn(start: u64, dur: u64, pitch: u8, vel: u8) -> TNote {
    TNote {
        start,
        dur,
        pitch,
        vel,
        rel: 64,
        ..Default::default()
    }
}

/// Runs tool `id` with `set` on `notes` (one track, subchannels 0 – 3).
fn run(id: &str, notes: Vec<TNote>, set: &[(&str, ParamValue)]) -> TransformRun {
    let track = SourceTrack {
        id: TrackId(1),
        index: 0,
        content: None,
        revision: 0,
        subs: vec![(0, 0), (1, 1), (2, 2), (3, 3)],
        all: false,
        insert_only: false,
        frame: 0,
    };
    let src = Source::from_notes(notes, vec![track]);
    let t = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    let est = k.estimate_source(&src);
    let r = run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None).unwrap();
    assert!(r.notes.len() as u64 <= est.notes);
    r
}

fn row() -> Vec<TNote> {
    (0..9).map(|i| tn(i * 480, 240, 60, 64)).collect()
}

fn vels(r: &TransformRun) -> Vec<u8> {
    let mut v: Vec<(u64, u8)> = r.notes.iter().map(|n| (n.start, n.vel)).collect();
    v.sort_unstable();
    v.into_iter().map(|x| x.1).collect()
}

fn on(k: &str) -> (&str, ParamValue) {
    (k, ParamValue::Bool(true))
}

/// Note Properties' Gradient (a3) from velocity 20 to 120 through `c`.
fn gradient(c: Curve) -> Vec<(&'static str, ParamValue)> {
    vec![
        on("a3"),
        ("a3.from", ParamValue::Int(20)),
        ("a3.to", ParamValue::Int(120)),
        ("a3.curve", ParamValue::Curve(c)),
    ]
}

#[test]
fn overshoot_reaches_past_the_nominal_range_within_midi_limits() {
    // A rise to 1.4 and back: 20 + 100·1.4 = 160 → 127 (velocity limit).
    let c = Curve::new(vec![(0.0, 0.0), (0.5, 1.4), (1.0, -0.3)]).with_overshoot(0.5);
    let r = run("note_props", row(), &gradient(c.clone()));
    let v = vels(&r);
    assert_eq!(v[0], 20);
    assert_eq!(v[4], 127, "past the nominal 120, pinned at MIDI's 127");
    assert_eq!(v[8], 1, "20 − 30 = −10 → velocity 1");
    assert_eq!(v[2], 90, "20 + 100 · 0.7 on the straight segment");
    // The same graph without overshoot ends at the nominal range.
    let flat = run("note_props", row(), &gradient(c.without_overshoot()));
    assert_eq!(vels(&flat)[4], 120);
    assert_eq!(vels(&flat)[8], 20);
    // Preview = Apply: the same kernel gives the same result.
    let again = run("note_props", row(), &gradient(c));
    assert_eq!(r.notes, again.notes);
}

#[test]
fn a_graph_that_does_not_allow_overshoot_reads_it_pinned() {
    // Note Properties' Mapping (n01) allows it; Quantize's start strength
    // (a3.start, a share of the move) does not: an overshooting graph gives
    // exactly the pinned graph's result there.
    let over = Curve::new(vec![(0.0, -1.0), (0.5, 2.5), (1.0, 0.4)]).with_overshoot(2.0);
    let notes: Vec<TNote> = (0..12)
        .map(|i| tn(i * 470 + 37, 200, 60 + i as u8, 90))
        .collect();
    let set = |c: Curve| vec![on("a3"), ("a3.start", ParamValue::Curve(c))];
    let a = run("quantize", notes.clone(), &set(over.clone()));
    let b = run("quantize", notes, &set(over.without_overshoot()));
    assert_eq!(a.notes, b.notes);
}

#[test]
fn per_point_smoothness_shapes_a_tools_result() {
    let pts = vec![(0.0, 0.0), (0.3, 0.9), (0.7, 0.2), (1.0, 1.0)];
    let straight = Curve::new(pts.clone());
    let smooth = Curve::new(pts.clone()).with_smoothness(1.0);
    // Smooth graph with a corner at the second point.
    let corner = smooth.clone().with_local(1, Some(0.0));
    let (a, b, c) = (
        vels(&run("note_props", row(), &gradient(straight))),
        vels(&run("note_props", row(), &gradient(smooth))),
        vels(&run("note_props", row(), &gradient(corner.clone()))),
    );
    assert_ne!(a, b);
    assert_ne!(b, c, "the corner changes the notes near it");
    // Every point keeps its value: notes on the points get it exactly.
    for v in [&a, &b, &c] {
        assert_eq!(v[0], 20);
        assert_eq!(v[8], 120);
    }
    // Mixed points at 0 / 50 / 100 % global smoothness stay deterministic.
    for s in [0.0, 0.5, 1.0] {
        let m = Curve::new(pts.clone())
            .with_smoothness(s)
            .with_local(2, Some(1.0))
            .with_local(1, Some(0.0));
        let x = run("note_props", row(), &gradient(m.clone()));
        let y = run("note_props", row(), &gradient(m));
        assert_eq!(x.notes, y.notes);
    }
}

#[test]
fn repeat_gates_can_overshoot_the_slot() {
    // Note Repeat's gate graph (a4) at 1.5 lengthens each repeat past its
    // slot; without overshoot it stops at the slot.
    let notes = vec![tn(0, 1920, 60, 100)];
    let set = |c: Curve| vec![on("a4"), ("a4.curve", ParamValue::Curve(c))];
    let flat = run("repeat", notes.clone(), &set(Curve::flat(1.0)));
    let over = run("repeat", notes, &set(Curve::flat(1.5).with_overshoot(0.5)));
    let max_flat = flat.notes.iter().map(|n| n.dur).max().unwrap();
    let max_over = over.notes.iter().map(|n| n.dur).max().unwrap();
    assert_eq!(flat.notes.len(), over.notes.len());
    assert!(max_over > max_flat, "{max_over} > {max_flat}");
}

#[test]
fn presets_round_trip_and_v09_presets_are_unchanged() {
    let t = tool("note_props").unwrap();
    let specs = (t.spec)().params;
    let mut values = ParamSet::defaults(&specs);
    let c = Curve::new(vec![(0.1, -0.25), (0.5, 1.2), (0.9, 0.5)])
        .with_smoothness(0.5)
        .with_overshoot(0.5)
        .with_local(1, Some(0.0))
        .sanitized();
    values.set("a3.curve", ParamValue::Curve(c.clone()));
    let p = Preset {
        tool: "note_props".into(),
        name: "Overshoot".into(),
        values,
    };
    let text = to_text(&p);
    assert!(
        text.contains(
            "p.a3.curve=0.5^0.5;0.100000,-0.250000;0.500000,1.200000,0;0.900000,0.500000"
        ),
        "{text}"
    );
    let (back, skipped) = parse(&text, "note_props", &specs).unwrap();
    assert!(skipped.is_empty());
    assert_eq!(back.values.get_raw("a3.curve"), Some(&ParamValue::Curve(c)));
    // A v0.9 preset line reads as before (no overshoot, no local values).
    let v09 = "PlumSpirit Preset 1\ntool=note_props\nname=Old\np.a3.curve=0.5@extend;0.000000,0.100000;1.000000,0.900000\n";
    let (old, _) = parse(v09, "note_props", &specs).unwrap();
    match old.values.get_raw("a3.curve") {
        Some(ParamValue::Curve(c)) => {
            assert_eq!(c.overshoot, 0.0);
            assert!(c.local.is_empty());
            assert_eq!(
                c.to_text(),
                "0.5@extend;0.000000,0.100000;1.000000,0.900000"
            );
        }
        other => panic!("{other:?}"),
    }
    // Invalid values refuse the preset.
    for bad in ["0^9;0,0;1,1", "0;0,0,1.5;1,1", "0^0.5;0,0,a;1,1"] {
        let t = format!("PlumSpirit Preset 1\ntool=note_props\nname=Bad\np.a3.curve={bad}\n");
        assert!(parse(&t, "note_props", &specs).is_err(), "{bad}");
    }
}

#[test]
fn every_overshoot_graph_is_a_curve() {
    // The declared graphs: curves only; at least the audited set.
    let mut n = 0;
    for d in plumspirit_engine::transform::definition::definitions() {
        for p in &d.params {
            if p.spec.overshoot {
                assert!(
                    matches!(
                        p.spec.kind,
                        plumspirit_engine::transform::param::ParamKind::Curve
                    ),
                    "{} {}",
                    d.id,
                    p.spec.key
                );
                n += 1;
            }
        }
    }
    assert!(n >= 55, "{n} overshoot graphs");
}
