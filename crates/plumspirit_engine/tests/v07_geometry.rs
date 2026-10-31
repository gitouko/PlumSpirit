//! Geometry tools (TEST_MATRIX T-GEOM-*): identity, inverse, extreme and
//! negative scale, undefined area, rasterization, Duplicate / Fill, bounds.

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

/// A 4 × 12 grid of notes: 4 beats × pitches 60..=71.
fn grid() -> Vec<NoteRecord> {
    let mut v = Vec::new();
    for b in 0..4u64 {
        for p in 60..=71u8 {
            v.push(rec(b * 960, 960, p));
        }
    }
    v
}

fn run(id: &str, notes: &[NoteRecord], set: &[(&str, ParamValue)]) -> Result<Vec<TNote>, String> {
    let mut d = PlumDocument::new_project("g".into(), Bpm::DEFAULT, 960);
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

fn f(x: f64) -> ParamValue {
    ParamValue::Float(x)
}

fn key(v: &[TNote]) -> Vec<(u64, u64, u8)> {
    let mut k: Vec<_> = v.iter().map(|n| (n.start, n.dur, n.pitch)).collect();
    k.sort_unstable();
    k
}

fn orig() -> Vec<(u64, u64, u8)> {
    let mut k: Vec<_> = grid()
        .iter()
        .map(|r| (r.start_tick, r.duration_ticks, r.attrs.pitch()))
        .collect();
    k.sort_unstable();
    k
}

#[test]
fn zero_amounts_are_the_identity_for_every_geometry_tool() {
    let g = grid();
    assert_eq!(
        key(&run("skew", &g, &[("amount", f(0.0))]).unwrap()),
        orig()
    );
    assert_eq!(
        key(&run("distort", &g, &[("amount", f(0.0))]).unwrap()),
        orig()
    );
    assert_eq!(
        key(&run("perspective", &g, &[("top", f(100.0))]).unwrap()),
        orig()
    );
    assert_eq!(
        key(&run("warp", &g, &[("dx", f(0.0)), ("dy", f(0.0))]).unwrap()),
        orig()
    );
    assert_eq!(
        key(&run("shear", &g, &[("amount", f(0.0))]).unwrap()),
        orig()
    );
}

#[test]
fn skew_shifts_rows_in_proportion_to_height_and_its_negative_undoes_it() {
    let g = grid();
    let s = run("skew", &g, &[("amount", f(50.0)), ("g.pivot_y", f(0.0))]).unwrap();
    // Bottom row stays; the top row moves by ≈ 50 % × (11.5 / 12) of the width.
    let row = |p: u8| {
        s.iter()
            .filter(|n| n.pitch == p)
            .map(|n| n.start)
            .min()
            .unwrap()
    };
    assert_eq!(row(60), 80); // half a row above the box bottom: 0.5/12 × 50 % × 3840
    assert!(row(71) > 1700 && row(71) < 1900, "{}", row(71));
    // The shift is linear in the row's height (one affine skew).
    let (r0, r6, r11) = (row(60) as f64, row(66) as f64, row(71) as f64);
    let expect = r0 + (r11 - r0) * 6.0 / 11.0;
    assert!((r6 - expect).abs() <= 2.0, "{r6} vs {expect}");
    // Durations are unchanged by a pure X skew.
    assert!(s.iter().all(|n| n.dur == 960));
}

#[test]
fn extreme_and_negative_scales_stay_valid_and_mirror() {
    let g = grid();
    for sy in [-400.0, -100.0, 0.0, 400.0] {
        let r = run(
            "shear",
            &g,
            &[("g.scale_y", f(sy)), ("b.pitch", ParamValue::Choice(1))],
        )
        .unwrap();
        assert!(r.iter().all(|n| n.pitch <= 127 && n.dur >= 1));
    }
    // −100 % Y mirrors the rows inside the box (pivot centre).
    let m = run("shear", &g, &[("amount", f(0.0)), ("g.scale_y", f(-100.0))]).unwrap();
    assert_eq!(key(&m), orig());
    let x = run("skew", &g, &[("amount", f(0.0)), ("g.scale_x", f(-100.0))]).unwrap();
    assert_eq!(
        key(&x),
        orig(),
        "mirroring the whole box in time maps the grid onto itself"
    );
    // Undefined area: wrap keeps every note inside the box.
    let w = run(
        "warp",
        &g,
        &[
            ("dy", f(100.0)),
            ("radius", f(150.0)),
            ("g.undefined", ParamValue::Choice(1)),
        ],
    )
    .unwrap();
    assert!(w.iter().all(|n| (60..=71).contains(&n.pitch)));
}

#[test]
fn vertical_stretch_with_fill_keeps_the_artwork_solid() {
    let g = grid();
    let sparse = run(
        "skew",
        &g,
        &[
            ("amount", f(0.0)),
            ("g.scale_y", f(300.0)),
            ("g.pivot_y", f(0.0)),
        ],
    )
    .unwrap();
    let filled = run(
        "skew",
        &g,
        &[
            ("amount", f(0.0)),
            ("g.scale_y", f(300.0)),
            ("g.pivot_y", f(0.0)),
            ("g.fill", ParamValue::Bool(true)),
        ],
    )
    .unwrap();
    assert_eq!(sparse.len(), 48);
    assert_eq!(
        filled.len(),
        4 * 36,
        "every target row of each beat is covered"
    );
    let rows: std::collections::BTreeSet<u8> = filled.iter().map(|n| n.pitch).collect();
    assert_eq!(rows.len(), 36);
    // Every 2nd row only.
    let half = run(
        "skew",
        &g,
        &[
            ("amount", f(0.0)),
            ("g.scale_y", f(300.0)),
            ("g.pivot_y", f(0.0)),
            ("g.fill", ParamValue::Bool(true)),
            ("g.fill_res", ParamValue::Int(2)),
        ],
    )
    .unwrap();
    assert!(
        half.len() < filled.len() && half.len() > sparse.len(),
        "{} {} {}",
        half.len(),
        filled.len(),
        sparse.len()
    );
}

#[test]
fn split_rasterization_turns_a_shear_into_staircases_and_every_tool_bounds_output() {
    let long = [rec(0, 3840, 60), rec(0, 3840, 72)];
    let mid = run("shear", &long, &[("g.raster", ParamValue::Choice(1))]).unwrap();
    assert_eq!(mid.len(), 2);
    let split = run(
        "skew",
        &long,
        &[
            ("amount", f(0.0)),
            ("g.raster", ParamValue::Choice(2)),
            ("g.segments", ParamValue::Int(8)),
            on_y(),
        ],
    )
    .unwrap();
    assert!(split.len() > 2, "{split:?}");
    // Each tool with every advanced function on stays valid and bounded.
    for id in ["skew", "distort", "perspective", "warp", "shear"] {
        let mut set: Vec<(&str, ParamValue)> = ADV_KEYS
            .iter()
            .map(|k| (*k, ParamValue::Bool(true)))
            .collect();
        set.push(("b.pitch", ParamValue::Choice(1)));
        let r = run(id, &grid(), &set).unwrap();
        assert!(!r.is_empty() && r.len() < 48 * 300, "{id}: {}", r.len());
        assert!(r.iter().all(|n| n.pitch <= 127 && n.dur >= 1), "{id}");
    }
}

fn on_y() -> (&'static str, ParamValue) {
    // Skew with a Y shear (function 2) so the split segments climb.
    (ADV_KEYS[1], ParamValue::Bool(true))
}

// ------------------------------------------------------------ twirl / wave

#[test]
fn twirl_never_outputs_illegal_pitches_and_wave_follows_its_seed() {
    let mut wide = Vec::new();
    for i in 0..64u64 {
        wide.push(rec(i * 120, 240, (i * 2) as u8));
    }
    for angle in [-999.0, -90.0, 45.0, 999.0] {
        let r = run(
            "twirl",
            &wide,
            &[("angle", f(angle)), ("b.pitch", ParamValue::Choice(0))],
        )
        .unwrap();
        assert!(r.iter().all(|n| n.pitch <= 127 && n.dur >= 1));
    }
    // Outside the ellipse nothing moves: a corner note stays.
    let t = run(
        "twirl",
        &grid(),
        &[("angle", f(180.0)), ("rx", f(20.0)), ("ry", f(20.0))],
    )
    .unwrap();
    assert!(t.iter().any(|n| (n.start, n.pitch) == (0, 60)));
    let a = run("wave", &grid(), &[("seed", ParamValue::Int(1))]).unwrap();
    let b = run("wave", &grid(), &[("seed", ParamValue::Int(1))]).unwrap();
    let c = run("wave", &grid(), &[("seed", ParamValue::Int(2))]).unwrap();
    assert_eq!(a, b);
    assert_ne!(
        key(&a),
        key(&c),
        "Randomize (another seed) draws other generators"
    );
}

// --------------------------------------------------------------- clip mask

mod mask {
    use super::*;
    use plumspirit_engine::history::History;
    use plumspirit_engine::transform::run::collect_with;
    use plumspirit_engine::transform::{TransformRun, commit};

    /// Track 0 = art (selected), track 1 = mask (base, "the track below").
    fn doc(art: &[NoteRecord], mask: &[NoteRecord]) -> PlumDocument {
        let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        let mut a = art.to_vec();
        a.sort_by_key(|r| r.start_tick);
        let mut m = mask.to_vec();
        m.sort_by_key(|r| r.start_tick);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&a, true)
            .unwrap();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(&m, false)
            .unwrap();
        d
    }

    fn run_mask(d: &PlumDocument, set: &[(&str, ParamValue)]) -> Result<TransformRun, String> {
        let t = tool("clip_mask").unwrap();
        let mut ps = ParamSet::defaults(&(t.spec)().params);
        for (k, v) in set {
            ps.set(k, v.clone());
        }
        let sc = [TrackScope {
            index: 0,
            all: false,
            content: None,
        }];
        let src = collect_with(t, d, &sc, &ps, 1 << 24).map_err(|e| e.user_message())?;
        let ctx = Ctx::simple(960);
        let b = BuildCtx {
            ctx: &ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let k = t.kernel(&ps, &b).map_err(|e| e.user_message())?;
        run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
            .map_err(|e| e.user_message())
    }

    fn slot_notes(r: &TransformRun, index: usize) -> Vec<(u64, u64, u8)> {
        let slot = r.tracks.iter().position(|t| t.index == index).unwrap() as u16;
        key(r.track_notes(slot))
    }

    #[test]
    fn the_art_is_clipped_to_the_mask_rectangles_and_the_source_stays_intact() {
        // Art: one long note per row 60..=62; mask: [500, 1500) on rows 60, 61.
        let art = [rec(0, 2000, 60), rec(0, 2000, 61), rec(0, 2000, 62)];
        let mask = [rec(500, 1000, 60), rec(500, 300, 61), rec(1200, 300, 61)];
        let d = doc(&art, &mask);
        let r = run_mask(&d, &[]).unwrap();
        // Source unchanged.
        assert_eq!(
            slot_notes(&r, 0),
            key(&art
                .iter()
                .map(|x| TNote::from_record(x, 0))
                .collect::<Vec<_>>())
        );
        // Base track gets the clipped pieces in addition to the mask.
        let base = slot_notes(&r, 1);
        assert_eq!(base, vec![(500, 300, 61), (500, 1000, 60), (1200, 300, 61)]);
        // Invert: the complement inside the art's box.
        let inv = run_mask(&d, &[(ADV_KEYS[0], ParamValue::Bool(true))]).unwrap();
        let pieces = slot_notes(&inv, 1);
        assert!(pieces.contains(&(0, 500, 60)) && pieces.contains(&(1500, 500, 60)));
        assert!(
            pieces.contains(&(0, 2000, 62)),
            "a row without mask is kept whole"
        );
        // Replace the art: the selection becomes the clipped notes.
        let rep = run_mask(&d, &[("result", ParamValue::Choice(1))]).unwrap();
        assert_eq!(
            slot_notes(&rep, 0),
            vec![(500, 300, 61), (500, 1000, 60), (1200, 300, 61)]
        );
        // Commit + undo exact; one undo step.
        let mut dd = d.clone();
        let mut h = History::default();
        let before = dd.state_fingerprint();
        commit(&mut h, &mut dd, &r, "Clipping Mask").unwrap();
        assert_eq!(dd.track(1).unwrap().notes.len(), 3 + 3);
        h.undo(&mut dd).unwrap();
        assert_eq!(dd.state_fingerprint(), before);
    }

    #[test]
    fn multi_track_union_intersection_and_subtract_combine_known_rectangles() {
        // Art [0, 2000) on row 60; base (track 2) [0, 1000); extra mask track 3
        // ("More mask tracks" defaults to 3) [500, 1500).
        let art = [rec(0, 2000, 60)];
        let mut d = doc(&art, &[rec(0, 1000, 60)]);
        d.add_track().unwrap();
        d.track_mut(2)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(500, 1000, 60)], false)
            .unwrap();
        let piece = |set: &[(&str, ParamValue)]| slot_notes(&run_mask(&d, set).unwrap(), 1);
        let on = |i: usize| (ADV_KEYS[i], ParamValue::Bool(true));
        assert_eq!(piece(&[]), vec![(0, 1000, 60)]);
        assert_eq!(piece(&[on(4)]), vec![(0, 1500, 60)], "union");
        assert_eq!(piece(&[on(5)]), vec![(500, 500, 60)], "intersection");
        assert_eq!(piece(&[on(6)]), vec![(0, 500, 60)], "subtract");
        // Inverse of the union inside the art's box.
        assert_eq!(piece(&[on(4), on(0)]), vec![(1500, 500, 60)]);
        // The extra mask track and the art stay intact; one undo step.
        let r = run_mask(&d, &[on(4)]).unwrap();
        let mut dd = d.clone();
        let mut h = History::default();
        let before = dd.state_fingerprint();
        commit(&mut h, &mut dd, &r, "Clipping Mask").unwrap();
        assert_eq!(dd.track(2).unwrap().notes.len(), 1);
        assert_eq!(dd.track(0).unwrap().notes.len(), 1);
        assert_eq!(dd.track(1).unwrap().notes.len(), 2);
        assert_eq!(h.undo_len(), 1);
        h.undo(&mut dd).unwrap();
        assert_eq!(dd.state_fingerprint(), before);
    }

    #[test]
    fn vice_versa_union_subtract_expand_and_new_subtrack() {
        let art = [rec(0, 1000, 60)];
        let mask = [rec(0, 4000, 60), rec(0, 4000, 61)];
        let d = doc(&art, &mask);
        // Vice Versa: the base notes clipped by the selection, added to the
        // selection's track.
        let vv = run_mask(&d, &[("vice", ParamValue::Bool(true))]).unwrap();
        assert_eq!(slot_notes(&vv, 0), vec![(0, 1000, 60), (0, 1000, 60)]);
        assert!(
            run_mask(
                &d,
                &[
                    ("vice", ParamValue::Bool(true)),
                    ("result", ParamValue::Choice(1))
                ]
            )
            .is_err()
        );
        // Contract the mask by 300 ticks: the piece starts 300 ticks later.
        let ct = run_mask(
            &d,
            &[
                (ADV_KEYS[3], ParamValue::Bool(true)),
                ("a4.ticks", ParamValue::Int(300)),
                ("a4.rows", ParamValue::Int(0)),
            ],
        )
        .unwrap();
        assert_eq!(slot_notes(&ct, 1), vec![(300, 700, 60)]);
        // Result to a new subtrack: commit adds one subtrack under the base.
        let ns = run_mask(&d, &[(ADV_KEYS[9], ParamValue::Bool(true))]).unwrap();
        assert_eq!(ns.new_subtrack.as_ref().map(|(_, v)| v.len()), Some(1));
        let mut dd = d.clone();
        let mut h = History::default();
        commit(&mut h, &mut dd, &ns, "Clipping Mask").unwrap();
        assert_eq!(dd.track_count(), 3);
        assert_eq!(
            dd.track(2).unwrap().parent(),
            Some(dd.track(1).unwrap().id())
        );
        assert_eq!(dd.track(2).unwrap().notes.len(), 1);
        h.undo(&mut dd).unwrap();
        assert_eq!(dd.track_count(), 2);
    }
}

#[test]
fn paint_color_changes_subchannels_not_channels_unless_asked() {
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::Subchannel;
    let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 960);
    let mut h = History::default();
    let subs: Vec<Subchannel> = (1..3)
        .map(|i| Subchannel::new(SubchannelId(i), 5 + i as u8))
        .collect();
    h.apply(&mut d, Operation::AddSubchannels { track: 0, subs })
        .unwrap();
    let v = vec![rec(0, 100, 60), rec(100, 100, 61), rec(5000, 100, 90)];
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    let t = tool("paint_color").unwrap();
    let go = |set: &[(&str, ParamValue)]| {
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
        run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None)
            .unwrap()
            .notes
    };
    let r = go(&[]);
    assert!(r.iter().all(|n| n.sub == 1 && n.ch == 0), "{r:?}");
    let r = go(&[("set_channel", ParamValue::Bool(true))]);
    assert!(r.iter().all(|n| n.sub == 1 && n.ch == 6));
    // Flood fill: only the region touching the first note.
    let r = go(&[(ADV_KEYS[9], ParamValue::Bool(true))]);
    assert_eq!(r.iter().map(|n| n.sub).collect::<Vec<_>>(), vec![1, 1, 0]);
}
