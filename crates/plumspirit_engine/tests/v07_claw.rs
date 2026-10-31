//! Claw Machine fixtures from the owner's FL Studio video
//! (docs/references/v0_7/CLAW_REFERENCE.md, T-FID-CLAW).

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::param::ParamValue;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_for;
use plumspirit_engine::transform::{Ctx, Limits, ParamSet, TNote, TrackScope, run_kernel, tool};

/// The video's staircase: 16 notes C4… starting every 1/16 bar, all ending at bar 2.
fn staircase() -> Vec<NoteRecord> {
    (0..16u64)
        .map(|i| NoteRecord {
            start_tick: i * 240,
            duration_ticks: 3840 - i * 240,
            attrs: NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        })
        .collect()
}

fn claw(notes: &[NoteRecord], set: &[(&str, ParamValue)]) -> Vec<TNote> {
    let mut d = PlumDocument::new_project("claw".into(), Bpm::DEFAULT, 960);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(notes, true)
        .unwrap();
    let t = tool("claw").unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    let sc = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(t, &d, &sc, 1 << 20).unwrap();
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    let mut v = run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
        .unwrap()
        .notes;
    v.sort_by_key(|n| n.pitch);
    v
}

/// Measured onset fractions (CLAW_REFERENCE fixture 1), index → (left, right).
const MEASURED: &[(usize, f64, f64)] = &[
    (0, 0.0, 0.0),
    (1, 0.0044, 0.224),
    (2, 0.013, 0.395),
    (4, 0.035, 0.640),
    (6, 0.066, 0.781),
    (8, 0.123, 0.873),
    (10, 0.215, 0.930),
    (12, 0.364, 0.965),
    (14, 0.605, 0.982),
    (15, 0.781, 0.991),
];

#[test]
fn the_staircase_matches_the_video_at_both_knob_extremes() {
    for (dist, col) in [(-100.0, 0), (100.0, 1)] {
        let out = claw(&staircase(), &[("dist", ParamValue::Float(dist))]);
        assert_eq!(out.len(), 16);
        for &(i, left, right) in MEASURED {
            let want = if col == 0 { left } else { right } * 3840.0;
            let got = out[i].start as f64;
            assert!(
                (got - want).abs() <= 0.012 * 3840.0,
                "dist {dist}: note {i} at {got}, measured {want}"
            );
            // Ends on the period boundary stay there.
            assert_eq!(out[i].end(), 3840, "dist {dist}: note {i} end");
        }
        // Order is preserved.
        assert!(out.windows(2).all(|w| w[0].start <= w[1].start));
    }
}

#[test]
fn a_16_beat_period_collapses_the_first_bar_toward_the_start() {
    let out = claw(
        &staircase(),
        &[
            ("dist", ParamValue::Float(-100.0)),
            ("period", ParamValue::Choice(4)),
        ],
    );
    // Bar 1 is u ≤ 0.25 of the period: everything lands within ≈ 0.035 of it.
    let p = 4.0 * 3840.0;
    assert!(out.iter().all(|n| (n.end() as f64) <= 0.036 * p));
}

#[test]
fn identity_trash_stretch_and_short_notes() {
    let id = claw(&staircase(), &[]);
    let mut orig: Vec<(u64, u64)> = staircase()
        .iter()
        .map(|r| (r.start_tick, r.duration_ticks))
        .collect();
    orig.sort();
    let mut got: Vec<(u64, u64)> = id.iter().map(|n| (n.start, n.dur)).collect();
    got.sort();
    assert_eq!(got, orig, "0 % distortion is the identity");
    // Trash every 1/2 removes the notes starting in the second half.
    let half = claw(&staircase(), &[("trash", ParamValue::Choice(1))]);
    assert_eq!(half.len(), 8);
    assert!(half.iter().all(|n| n.start < 1920));
    // Remove short notes: under +100 % the last notes become tiny.
    let short = claw(
        &staircase(),
        &[
            ("dist", ParamValue::Float(100.0)),
            ("short", ParamValue::Bool(true)),
            ("short.ticks", ParamValue::Int(200)),
        ],
    );
    assert!(short.len() < 16 && short.iter().all(|n| n.dur >= 200));
    // Stretch to compensate: quarter notes with the 2nd half trashed are
    // stretched to fill their period.
    let quarters: Vec<NoteRecord> = (0..4u64)
        .map(|i| NoteRecord {
            start_tick: i * 960,
            duration_ticks: 480,
            attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        })
        .collect();
    let st = claw(
        &quarters,
        &[
            ("trash", ParamValue::Choice(1)),
            ("stretch", ParamValue::Bool(true)),
        ],
    );
    assert_eq!(
        st.iter().map(|n| (n.start, n.dur)).collect::<Vec<_>>(),
        vec![(0, 960), (960, 2880)]
    );
}
