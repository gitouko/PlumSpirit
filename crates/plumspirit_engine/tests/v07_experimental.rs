//! Original experimental tools (contract §101–121; TEST_MATRIX T-EXP-*):
//! behavioural checks of each tool's defining rule on small known art. The
//! generic harness (`v07_tools.rs`) covers validity, determinism, undo and the
//! ten advanced functions of every tool.

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::param::{ADV_KEYS, ParamValue};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TNote, TrackScope, run_kernel, tool};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

fn run(id: &str, notes: &[NoteRecord], set: &[(&str, ParamValue)]) -> Result<Vec<TNote>, String> {
    let mut d = PlumDocument::new_project("x".into(), Bpm::DEFAULT, 960);
    let mut v = notes.to_vec();
    v.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    let t = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, val) in set {
        ps.set(k, val.clone());
    }
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(t, &d, &sc, 1 << 24).map_err(|e| e.user_message())?;
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).map_err(|e| e.user_message())?;
    run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
        .map(|r| r.notes)
        .map_err(|e| e.user_message())
}

fn on(n: usize) -> (&'static str, ParamValue) {
    (ADV_KEYS[n - 1], ParamValue::Bool(true))
}

fn f(x: f64) -> ParamValue {
    ParamValue::Float(x)
}

fn i(x: i64) -> ParamValue {
    ParamValue::Int(x)
}

#[test]
fn gravity_pulls_towards_the_well_repels_and_swallows_at_the_horizon() {
    let art: Vec<NoteRecord> = (0..8u64)
        .flat_map(|t| (0..8u8).map(move |p| rec(t * 480, 240, 56 + p * 2)))
        .collect();
    let centre = |v: &[TNote]| -> f64 {
        v.iter()
            .map(|n| {
                let dx = (n.start as f64 - 1800.0) / 3600.0;
                let dy = (f64::from(n.pitch) - 63.0) / 16.0;
                (dx * dx + dy * dy).sqrt()
            })
            .sum::<f64>()
            / v.len() as f64
    };
    let base = run("gravity_well", &art, &[("strength", f(0.0))]).unwrap();
    let pulled = run("gravity_well", &art, &[("strength", f(80.0))]).unwrap();
    let pushed = run("gravity_well", &art, &[("strength", f(30.0)), on(2)]).unwrap();
    assert!(centre(&pulled) < centre(&base) * 0.8, "attraction");
    assert!(centre(&pushed) > centre(&base), "repulsion");
    let eaten = run("gravity_well", &art, &[on(5), ("a5.radius", f(20.0))]).unwrap();
    assert!(
        eaten.len() < art.len() && !eaten.is_empty(),
        "event horizon"
    );
}

#[test]
fn kaleidoscope_two_sectors_are_point_symmetric_and_wedges_mask() {
    let art = [rec(0, 240, 60), rec(1200, 240, 70), rec(2160, 240, 64)];
    let out = run("kaleidoscope", &art, &[]).unwrap();
    assert_eq!(out.len(), 6);
    // The box is ticks [0, 2400) × rows [60, 71): the copy of (0, 60) lands at
    // the opposite corner.
    assert!(
        out.iter().any(|n| n.end() == 2400 && n.pitch == 70),
        "{out:?}"
    );
    let six = run("kaleidoscope", &art, &[on(1), ("a1.n", i(6))]).unwrap();
    assert_eq!(six.len(), 18);
    let masked = run("kaleidoscope", &art, &[on(10)]).unwrap();
    assert!(masked.len() < out.len());
}

#[test]
fn orbit_places_motif_copies_around_the_ellipse_and_vortex_keeps_the_original() {
    let art = [rec(0, 240, 60), rec(480, 240, 72)];
    let o = run("orbit", &art, &[("copies", i(6))]).unwrap();
    assert_eq!(o.len(), 12);
    let kept = run(
        "orbit",
        &art,
        &[("copies", i(6)), ("keep", ParamValue::Bool(true))],
    )
    .unwrap();
    assert_eq!(kept.len(), 14);
    let ring3 = run(
        "orbit",
        &art,
        &[("copies", i(6)), on(3), ("a3.rings", i(3))],
    )
    .unwrap();
    assert_eq!(ring3.len(), 36);
    let v = run("vortex_trails", &art, &[]).unwrap();
    assert_eq!(v.len(), 2 * 4, "original + three trails");
    for r in &art {
        assert!(
            v.iter()
                .any(|n| n.start == r.start_tick && n.pitch == r.attrs.pitch())
        );
    }
}

#[test]
fn pulse_particles_and_flow_keep_their_counts() {
    let art: Vec<NoteRecord> = (0..8u64).map(|t| rec(t * 480, 240, 60 + t as u8)).collect();
    let flat = run("pulse_field", &art, &[("amp", f(0.0))]).unwrap();
    assert_eq!(
        flat.iter().map(|n| (n.start, n.pitch)).collect::<Vec<_>>(),
        art.iter()
            .map(|r| (r.start_tick, r.attrs.pitch()))
            .collect::<Vec<_>>()
    );
    let burst = run("particle_burst", &art, &[]).unwrap();
    assert_eq!(burst.len(), 8 * 7);
    let still = run("flow_field", &art, &[("strength", f(0.0))]).unwrap();
    assert_eq!(still.len(), 8);
    assert!(
        still
            .iter()
            .zip(&art)
            .all(|(n, r)| n.start == r.start_tick && n.pitch == r.attrs.pitch())
    );
}
