//! v0.9 direct transform gestures (contract §084–§085, tests §258): Ctrl+corner
//! drag = Skew, Ctrl+Shift+corner drag = free Perspective, on the canonical
//! geometry engine — every corner, the anchored corners exactly where they
//! were, the same notes as the Skew / Perspective tools, one undo step, a
//! conservative viewport window for the lazy preview.

use plumspirit_engine::history::History;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::geom::{Corner, CornerWarp, GeomBox, WarpKind};
use plumspirit_engine::transform::param::ADV_KEYS;
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::{
    Boundary, Ctx, Kernel, Limits, ParamSet, ParamValue, Source, TNote, TrackScope, run_kernel,
    tool,
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

/// 4 beats × pitches 60..=71 (box 3840 ticks × 12 rows), with varied
/// velocities, MIDI channels and subchannels.
fn grid() -> Vec<NoteRecord> {
    let mut v = Vec::new();
    for b in 0..4u64 {
        for p in 60..=71u8 {
            let i = b as u8 * 12 + (p - 60);
            v.push(rec(b * 960, 960, p, 40 + i, i % 3, u32::from(i % 2) * 2));
        }
    }
    v
}

fn doc_with(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("v09g".into(), Bpm::DEFAULT, 960);
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

/// The selection's box as the Piano Roll's handles see it.
fn sel_box(d: &PlumDocument) -> GeomBox {
    let b = d.track(0).unwrap().notes.selection_bounds().unwrap();
    GeomBox {
        t0: b.min_start,
        t1: b.max_end.max(b.min_start + 1),
        p0: b.min_pitch,
        p1: b.max_pitch,
    }
}

type Key = (u64, u64, u8, u8, u8, u32);

fn key(v: &[TNote]) -> Vec<Key> {
    let mut k: Vec<_> = v
        .iter()
        .map(|n| (n.start, n.dur, n.pitch, n.vel, n.ch, n.sub))
        .collect();
    k.sort_unstable();
    k
}

fn run_warp(d: &PlumDocument, w: CornerWarp) -> Vec<TNote> {
    let src = Source::collect(d, &SEL, 10_000_000).unwrap();
    let k = w.kernel(sel_box(d), None, d.project().keys);
    run_kernel(&k, &src, Boundary::default(), &Limits::default(), None)
        .unwrap()
        .notes
}

fn run_tool(d: &PlumDocument, id: &str, set: &[(&str, ParamValue)]) -> Vec<TNote> {
    let def = tool(id).unwrap();
    let mut values = ParamSet::defaults(&(def.spec)().params);
    for (k, v) in set {
        values.set(k, v.clone());
    }
    let src = collect_for(def, d, &SEL, 10_000_000).unwrap();
    let ctx = Ctx::simple(960);
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = def.kernel(&values, &b).unwrap();
    run_kernel(
        k.as_ref(),
        &src,
        def.boundary(&values),
        &Limits::default(),
        None,
    )
    .unwrap()
    .notes
}

fn f(x: f64) -> ParamValue {
    ParamValue::Float(x)
}

/// Absolute position of a box corner (ticks, pitch rows; the top edge is
/// `p1 + 1`).
fn at(b: &GeomBox, c: Corner) -> (f64, f64) {
    let (u, v) = c.unit();
    b.from_unit(u, v)
}

fn close(a: (f64, f64), e: (f64, f64)) -> bool {
    (a.0 - e.0).abs() < 1e-6 && (a.1 - e.1).abs() < 1e-6
}

const SKEWS: [WarpKind; 2] = [WarpKind::SkewX, WarpKind::SkewY];

// ------------------------------------------------------------------ Skew

#[test]
fn ctrl_corner_skew_moves_the_dragged_edge_and_keeps_the_two_opposing_corners() {
    let d = doc_with(&grid());
    let b = sel_box(&d);
    assert_eq!((b.t0, b.t1, b.p0, b.p1), (0, 3840, 60, 71));
    for c in Corner::ALL {
        for kind in SKEWS {
            for (dt, dr) in [(960, 3), (-1920, -6), (7, 1), (100_000, 40)] {
                let w = CornerWarp {
                    kind,
                    corner: c,
                    dt,
                    dr,
                };
                assert!(w.valid(&b), "{w:?}");
                let outline = w.corners(&b);
                let mut anchored = 0;
                for k in Corner::ALL {
                    let (u, v) = k.unit();
                    let (mu, mv) = w.map_unit(&b, u, v);
                    let got = b.from_unit(mu, mv);
                    let p = at(&b, k);
                    // X skew: the dragged corner's horizontal edge slides by dt;
                    // Y skew: its vertical edge slides by dr.
                    let same_edge = match kind {
                        WarpKind::SkewX => k.is_top() == c.is_top(),
                        _ => k.is_right() == c.is_right(),
                    };
                    let expect = match (same_edge, kind) {
                        (false, _) => p,
                        (true, WarpKind::SkewX) => (p.0 + dt as f64, p.1),
                        (true, _) => (p.0, p.1 + dr as f64),
                    };
                    assert!(
                        close(got, expect),
                        "{w:?} corner {k:?}: {got:?} vs {expect:?}"
                    );
                    assert!(close(outline[k as usize], got), "outline = the map");
                    if close(got, p) {
                        anchored += 1;
                    }
                }
                assert_eq!(anchored, 2, "{w:?}: exactly the two opposing corners stay");
            }
        }
    }
}

#[test]
fn skew_gesture_gives_exactly_the_skew_tool_result_at_every_corner() {
    // Drags whose amounts are exact in binary (¼ and ½ of the box), so the
    // tool's percent parameters carry the same numbers.
    let d = doc_with(&grid());
    for c in Corner::ALL {
        for (dt, dr) in [(960, 3), (-1920, -6)] {
            // X skew: Amount = dt / width, the pivot on the anchored edge.
            let x = CornerWarp {
                kind: WarpKind::SkewX,
                corner: c,
                dt,
                dr,
            };
            let amount = dt as f64 / 3840.0 * 100.0;
            let tool = if c.is_top() {
                run_tool(&d, "skew", &[("amount", f(amount)), ("g.pivot_y", f(0.0))])
            } else {
                run_tool(
                    &d,
                    "skew",
                    &[("amount", f(-amount)), ("g.pivot_y", f(100.0))],
                )
            };
            let gesture = run_warp(&d, x);
            assert_eq!(gesture.len(), 48, "N notes stay N notes");
            assert_eq!(key(&gesture), key(&tool), "{x:?}");
            // Y skew: the Y shear advanced function, the pivot on the anchored
            // edge, no X skew.
            let y = CornerWarp {
                kind: WarpKind::SkewY,
                ..x
            };
            let a = dr as f64 / 12.0 * 100.0;
            let on = (ADV_KEYS[1], ParamValue::Bool(true));
            let tool = if c.is_right() {
                run_tool(
                    &d,
                    "skew",
                    &[
                        ("amount", f(0.0)),
                        on,
                        ("a2.amount", f(a)),
                        ("g.pivot_x", f(0.0)),
                    ],
                )
            } else {
                run_tool(
                    &d,
                    "skew",
                    &[
                        ("amount", f(0.0)),
                        on,
                        ("a2.amount", f(-a)),
                        ("g.pivot_x", f(100.0)),
                    ],
                )
            };
            assert_eq!(key(&run_warp(&d, y)), key(&tool), "{y:?}");
        }
    }
}

#[test]
fn skew_moves_notes_in_proportion_and_keeps_every_attribute() {
    let d = doc_with(&grid());
    // Top right dragged one beat later: the bottom row moves by ½ / 12 of it,
    // the top row by 11.5 / 12; lengths stay.
    let out = run_warp(
        &d,
        CornerWarp {
            kind: WarpKind::SkewX,
            corner: Corner::TopRight,
            dt: 960,
            dr: 0,
        },
    );
    let first = |p: u8| {
        out.iter()
            .filter(|n| n.pitch == p)
            .map(|n| n.start)
            .min()
            .unwrap()
    };
    assert_eq!(first(60), 40);
    assert_eq!(first(71), 920);
    assert!(out.iter().all(|n| n.dur == 960));
    // Bottom left dragged one beat earlier: the bottom row moves earlier,
    // the top row stays nearly in place (clamped at tick 0 by the boundary).
    let out = run_warp(
        &d,
        CornerWarp {
            kind: WarpKind::SkewX,
            corner: Corner::BottomLeft,
            dt: -960,
            dr: 0,
        },
    );
    let last = |p: u8| {
        out.iter()
            .filter(|n| n.pitch == p)
            .map(|n| n.start)
            .max()
            .unwrap()
    };
    assert_eq!(last(60), 2880 - 920);
    assert_eq!(last(71), 2880 - 40);
    // Right edge three rows up (Y skew, left edge anchored): the first beat
    // (middle at ⅛ of the width) stays on its row, the last (⅞) climbs 3.
    let out = run_warp(
        &d,
        CornerWarp {
            kind: WarpKind::SkewY,
            corner: Corner::BottomRight,
            dt: 0,
            dr: 3,
        },
    );
    let rows = |t: u64| {
        let mut v: Vec<u8> = out
            .iter()
            .filter(|n| n.start == t)
            .map(|n| n.pitch)
            .collect();
        v.sort_unstable();
        v
    };
    assert_eq!(rows(0), (60..=71).collect::<Vec<_>>());
    assert_eq!(rows(2880), (63..=74).collect::<Vec<_>>());
    // Velocity, MIDI channel and subchannel of every note are kept (the
    // multiset of attributes is unchanged).
    let attrs = |v: &[(u8, u8, u32)]| {
        let mut v = v.to_vec();
        v.sort_unstable();
        v
    };
    let before: Vec<_> = grid()
        .iter()
        .map(|r| (r.attrs.velocity(), r.attrs.channel(), r.sub.0))
        .collect();
    let after: Vec<_> = out.iter().map(|n| (n.vel, n.ch, n.sub)).collect();
    assert_eq!(attrs(&after), attrs(&before));
}

#[test]
fn one_gesture_is_one_undo_step() {
    let mut d = doc_with(&grid());
    let original = key(&run_warp(
        &d,
        CornerWarp {
            kind: WarpKind::Perspective,
            corner: Corner::TopLeft,
            dt: 0,
            dr: 0,
        },
    ));
    let mut h = History::default();
    let src = Source::collect(&d, &SEL, 10_000_000).unwrap();
    let w = CornerWarp {
        kind: WarpKind::SkewX,
        corner: Corner::TopLeft,
        dt: 480,
        dr: 0,
    };
    let k = w.kernel(sel_box(&d), None, d.project().keys);
    let run = run_kernel(&k, &src, Boundary::default(), &Limits::default(), None).unwrap();
    let op = build_operation(&mut d, &run, "Skew Selection").unwrap();
    h.apply(&mut d, op).unwrap();
    assert_eq!(h.undo_len(), 1);
    assert_eq!(h.undo_label(), Some("Skew Selection"));
    assert_eq!(d.track(0).unwrap().notes.len(), 48);
    h.undo(&mut d).unwrap();
    let identity = CornerWarp {
        kind: WarpKind::Perspective,
        corner: Corner::TopLeft,
        dt: 0,
        dr: 0,
    };
    assert_eq!(
        key(&run_warp(&d, identity)),
        original,
        "Undo restores every note"
    );
}

/// The lazy preview only maps the source notes in `source_window(t0, t1)`:
/// every note whose result can reach `[t0, t1]` must be among them.
fn assert_window_is_conservative(d: &PlumDocument, w: CornerWarp) {
    let b = sel_box(d);
    let k = w.kernel(b, None, d.project().keys);
    let p = k.as_point().expect("pointwise");
    let src = Source::collect(d, &SEL, 10_000_000).unwrap();
    let span = i64::try_from(b.t1).unwrap() + w.dt.abs() + 2000;
    for t0 in (-2000..span).step_by(173) {
        for len in [0u64, 1, 50, 700, 5000] {
            let t0 = t0.max(0) as u64;
            let t1 = t0 + len;
            let (a, e) = p.source_window(t0, t1);
            for n in &src.notes {
                let mut out =
                    plumspirit_engine::transform::Output::new(Boundary::default(), 1 << 16);
                p.map(n, &mut out);
                let reaches = out.notes.iter().any(|q| q.start <= t1 && q.end() >= t0);
                let inside = n.start <= e && n.end() >= a;
                assert!(
                    !reaches || inside,
                    "{w:?} window [{t0}, {t1}] -> [{a}, {e}] misses note {}..{}",
                    n.start,
                    n.end()
                );
            }
        }
    }
}

#[test]
fn skew_preview_window_is_conservative_and_bounded() {
    let d = doc_with(&grid());
    for c in Corner::ALL {
        for kind in SKEWS {
            for (dt, dr) in [(960, 3), (-1920, -6)] {
                let w = CornerWarp {
                    kind,
                    corner: c,
                    dt,
                    dr,
                };
                assert_window_is_conservative(&d, w);
                // Viewport-bounded: a window far right of the box asks for
                // nothing beyond the drag distance.
                let k = w.kernel(sel_box(&d), None, d.project().keys);
                let (a, _) = k.as_point().unwrap().source_window(100_000, 101_000);
                assert!(a >= 100_000 - 1921, "{w:?}: {a}");
            }
        }
    }
}

// ----------------------------------------------------------- Perspective

fn perspective(corner: Corner, dt: i64, dr: i64) -> CornerWarp {
    CornerWarp {
        kind: WarpKind::Perspective,
        corner,
        dt,
        dr,
    }
}

#[test]
fn ctrl_shift_corner_perspective_moves_the_dragged_corner_and_keeps_the_three_others() {
    let d = doc_with(&grid());
    let b = sel_box(&d);
    for c in Corner::ALL {
        // Outward, inward, far outward, a tick and a row.
        let (sx, sy) = (
            if c.is_right() { 1 } else { -1 },
            if c.is_top() { 1 } else { -1 },
        );
        for (dt, dr) in [(384, 3), (-768, -2), (5000 * sx, 30 * sy), (-3, 1), (1, -1)] {
            let w = perspective(c, dt, dr);
            assert!(w.valid(&b), "{w:?}");
            let mut anchored = 0;
            for k in Corner::ALL {
                let (u, v) = k.unit();
                let (mu, mv) = w.map_unit(&b, u, v);
                let got = b.from_unit(mu, mv);
                let p = at(&b, k);
                let expect = if k == c {
                    (p.0 + dt as f64, p.1 + dr as f64)
                } else {
                    p
                };
                assert!(
                    close(got, expect),
                    "{w:?} corner {k:?}: {got:?} vs {expect:?}"
                );
                assert!(close(w.corners(&b)[k as usize], got), "outline = the map");
                if close(got, p) {
                    anchored += 1;
                }
            }
            assert_eq!(anchored, 3, "{w:?}: the three other corners stay");
            // The plane is projected (straight lines stay straight), not bent:
            // the box's centre lands where the quad's diagonals cross.
            let q = w.corners(&b);
            let (cx, cy) = {
                let (mu, mv) = w.map_unit(&b, 0.5, 0.5);
                b.from_unit(mu, mv)
            };
            let cross = |a: (f64, f64), e: (f64, f64), p: (f64, f64)| {
                (e.0 - a.0) * (p.1 - a.1) - (e.1 - a.1) * (p.0 - a.0)
            };
            let scale = b.width() * b.rows();
            assert!(cross(q[0], q[2], (cx, cy)).abs() / scale < 1e-9, "{w:?}");
            assert!(cross(q[1], q[3], (cx, cy)).abs() / scale < 1e-9, "{w:?}");
        }
    }
}

#[test]
fn perspective_gesture_gives_exactly_the_perspective_tool_result_at_every_corner() {
    // Drags that are whole ‰ of the box (the tool's corner grid): the tool's
    // Four-corner projective function with the same quad gives the same notes.
    let d = doc_with(&grid());
    for c in Corner::ALL {
        for (dt, dr) in [(384, 3), (-768, -6), (1152, -3)] {
            let w = perspective(c, dt, dr);
            let mut corners = vec![0i64, 0, 1000, 0, 1000, 1000, 0, 1000];
            let i = c as usize;
            corners[2 * i] += dt * 1000 / 3840;
            corners[2 * i + 1] += dr * 1000 / 12;
            let tool = run_tool(
                &d,
                "perspective",
                &[
                    (ADV_KEYS[0], ParamValue::Bool(true)),
                    ("a1.corners", ParamValue::List(corners)),
                ],
            );
            let gesture = run_warp(&d, w);
            assert_eq!(gesture.len(), 48, "N notes stay N notes");
            assert_eq!(key(&gesture), key(&tool), "{w:?}");
        }
    }
}

#[test]
fn a_perspective_corner_cannot_fold_the_plane() {
    let d = doc_with(&grid());
    let b = sel_box(&d);
    for c in Corner::ALL {
        // Dragged across the diagonal to the opposite corner's side: the quad
        // would be concave (the plane folds) — not a valid gesture.
        let dt = if c.is_right() { -3840 } else { 3840 };
        let dr = if c.is_top() { -12 } else { 12 };
        assert!(!perspective(c, dt, dr).valid(&b), "{c:?}");
        // Onto the diagonal: degenerate, also refused.
        assert!(!perspective(c, dt / 2, dr / 2).valid(&b), "{c:?}");
        // Just short of it: fine.
        assert!(perspective(c, dt * 4 / 10, dr * 4 / 10).valid(&b), "{c:?}");
        // Identity drags change nothing and are not committed.
        assert!(perspective(c, 0, 0).is_identity());
    }
}

#[test]
fn perspective_preview_window_is_conservative_and_bounded() {
    let d = doc_with(&grid());
    for c in Corner::ALL {
        // Outward and inward drags (the Piano Roll never produces a folded
        // quad, see `a_perspective_corner_cannot_fold_the_plane`).
        let (sx, sy) = (
            if c.is_right() { 1 } else { -1 },
            if c.is_top() { 1 } else { -1 },
        );
        for (dt, dr) in [
            (384, 3),
            (-768, -6),
            (6000 * sx, 20 * sy),
            (-1500 * sx, 9 * sy),
        ] {
            let w = perspective(c, dt, dr);
            assert!(w.valid(&sel_box(&d)), "{w:?}");
            assert_window_is_conservative(&d, w);
            // A view entirely outside the warped box needs no source notes.
            let k = w.kernel(sel_box(&d), None, d.project().keys);
            let (a, e) = k.as_point().unwrap().source_window(50_000, 60_000);
            assert!(a > e, "{w:?}: [{a}, {e}]");
        }
    }
    // A narrow view near the anchored left edge only needs the notes near it.
    let w = perspective(Corner::TopRight, 960, 3);
    let k = w.kernel(sel_box(&d), None, d.project().keys);
    let (a, e) = k.as_point().unwrap().source_window(0, 100);
    assert!(a == 0 && e < 400, "[{a}, {e}]");
}

// ------------------------------------------------- Scale shrink cleanup

/// Notes for the shrink cleanup (box 0 – 40 ticks × pitches 60 – 63):
/// A / B overlap on 60 (not identical) and collapse together under ÷10;
/// D and D′ are an intentional identical pair on 61 that G joins under ÷10;
/// E (subchannel 1) and F (MIDI channel 2) sit on D's spot but differ;
/// H / I overlap on 63 and stay apart.
fn shrink_set() -> Vec<NoteRecord> {
    vec![
        rec(0, 10, 60, 100, 0, 0),  // A
        rec(2, 10, 60, 90, 0, 0),   // B → A's spot
        rec(20, 10, 61, 100, 0, 0), // D
        rec(20, 10, 61, 100, 0, 0), // D′ (identical to D)
        rec(20, 10, 61, 80, 0, 2),  // E (another subchannel)
        rec(20, 10, 61, 80, 1, 0),  // F (another MIDI channel)
        rec(22, 9, 61, 70, 0, 0),   // G → D's spot
        rec(30, 10, 63, 100, 0, 0), // H
        rec(35, 5, 63, 100, 0, 0),  // I
    ]
}

fn spots(v: &[TNote]) -> Vec<(u64, u64, u8, u8, u32, u8)> {
    let mut k: Vec<_> = v
        .iter()
        .map(|n| (n.start, n.end(), n.pitch, n.ch, n.sub, n.vel))
        .collect();
    k.sort_unstable();
    k
}

fn run_box(
    d: &PlumDocument,
    to: GeomBox,
    fill: Option<plumspirit_engine::transform::geom::Fill>,
) -> plumspirit_engine::transform::TransformRun {
    let src = Source::collect(d, &SEL, 10_000_000).unwrap();
    let k = plumspirit_engine::transform::geom::BoxScale {
        from: sel_box(d),
        to,
        fill,
        prevent: false,
        cleanup: Default::default(),
    };
    run_kernel(&k, &src, Boundary::default(), &Limits::default(), None).unwrap()
}

#[test]
fn shrinking_scale_handles_remove_only_the_duplicates_the_shrink_creates() {
    let d = doc_with(&shrink_set());
    let b = sel_box(&d);
    assert_eq!((b.t0, b.t1, b.p0, b.p1), (0, 40, 60, 63));
    // X ÷ 10: B lands on A, G on D / D′.
    let r = run_box(&d, GeomBox { t1: 4, ..b }, None);
    assert_eq!(r.stats.collapsed, 2, "{}", r.summary());
    assert!(
        r.summary()
            .contains("2 duplicate(s) the shrink created removed.")
    );
    assert_eq!(
        spots(&r.notes),
        vec![
            (0, 1, 60, 0, 0, 100), // A stays (the earlier of two different notes)
            (2, 3, 61, 0, 0, 100), // D and D′: the intentional pair stays whole
            (2, 3, 61, 0, 0, 100),
            (2, 3, 61, 0, 2, 80),  // E: another subchannel
            (2, 3, 61, 1, 0, 80),  // F: another MIDI channel
            (3, 4, 63, 0, 0, 100), // H and I overlap but are not identical
            (4, 5, 63, 0, 0, 100),
        ]
    );
    // Without a shrink nothing is removed (a stretch never collapses notes),
    // and the identical pair stays a pair.
    let r = run_box(&d, GeomBox { t1: 80, ..b }, None);
    assert_eq!((r.notes.len(), r.stats.collapsed), (9, 0));
    // Y: rows 60 – 63 onto 60 – 61 (two rows each): A / B and D-row notes
    // never meet (different times); same-time notes on merged rows do.
    let stack = [
        rec(0, 10, 60, 100, 0, 0),
        rec(0, 10, 61, 90, 0, 0), // merges with 60
        rec(0, 10, 62, 100, 0, 0),
        rec(0, 10, 62, 100, 0, 0), // identical pair on 62 ...
        rec(0, 10, 63, 80, 0, 0),  // ... joined by 63
    ];
    let d = doc_with(&stack);
    let b = sel_box(&d);
    let r = run_box(&d, GeomBox { p1: 61, ..b }, None);
    assert_eq!(r.stats.collapsed, 2, "{}", r.summary());
    assert_eq!(
        spots(&r.notes),
        vec![
            (0, 10, 60, 0, 0, 100),
            (0, 10, 61, 0, 0, 100),
            (0, 10, 61, 0, 0, 100)
        ]
    );
    // With Duplicate / Fill (Alt) the same rule holds for the copies.
    let fill = Some(plumspirit_engine::transform::geom::Fill::default());
    let r = run_box(&d, GeomBox { p1: 61, ..b }, fill);
    assert_eq!(r.notes.len(), 3, "{:?}", spots(&r.notes));
}

#[test]
fn scale_x_and_y_shrinks_remove_new_duplicates_keep_intentional_ones_and_undo_exactly() {
    use plumspirit_engine::editor::factor::ExactFactor;
    use plumspirit_engine::editor::{self, EditControl, PreparedEdit};
    use plumspirit_engine::history::Operation;
    let all = |d: &PlumDocument| {
        let mut v: Vec<_> = d
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| {
                (
                    n.start_tick,
                    n.duration_ticks,
                    n.attrs.pitch(),
                    n.attrs.channel(),
                    n.sub.0,
                    n.attrs.velocity(),
                )
            })
            .collect();
        v.sort_unstable();
        v
    };
    // Scale X × 0.1 (pivot: the selection's start).
    let mut d = doc_with(&shrink_set());
    let original = all(&d);
    let mut h = History::default();
    let f = ExactFactor::parse("0.1").unwrap();
    let p = editor::prepare_scale_x(&d, 0, &f, EditControl::default()).unwrap();
    assert_eq!(p.affected_notes(), 7, "B and G removed");
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: p,
            label: "Scale X",
        },
    )
    .unwrap();
    let after = all(&d);
    assert_eq!(after.len(), 7);
    // D and D′ (velocity 100 on 61, subchannel 1, channel 1) are both there.
    assert_eq!(
        after
            .iter()
            .filter(|n| n.2 == 61 && n.3 == 0 && n.4 == 0)
            .count(),
        2
    );
    assert!(
        after.iter().all(|n| n.5 != 90 && n.5 != 70),
        "B and G are the ones removed"
    );
    h.undo(&mut d).unwrap();
    assert_eq!(all(&d), original, "Undo restores every note");
    // Scale X × 2 (a stretch): N → N.
    let p = editor::prepare_scale_x(
        &d,
        0,
        &ExactFactor::parse("2").unwrap(),
        EditControl::default(),
    )
    .unwrap();
    assert_eq!(p.affected_notes(), 9);
    // Scale Y × 0.5 around the middle of 60 – 64: 60 and 61 both land on 61.
    let stack = [
        rec(0, 10, 60, 100, 0, 0),
        rec(0, 10, 61, 90, 0, 0),
        rec(0, 10, 64, 100, 0, 0),
        rec(0, 10, 64, 100, 0, 0),
        rec(50, 10, 62, 100, 0, 0),
    ];
    let mut d = doc_with(&stack);
    let original = all(&d);
    let p = editor::prepare_scale_y(&d, 0, &ExactFactor::parse("0.5").unwrap()).unwrap();
    assert!(matches!(p, PreparedEdit::ReplaceSelected { .. }));
    assert_eq!(p.affected_notes(), 4);
    let mut h = History::default();
    h.apply(
        &mut d,
        Operation::Prepared {
            edit: p,
            label: "Scale Y",
        },
    )
    .unwrap();
    assert_eq!(
        all(&d),
        vec![
            (0, 10, 61, 0, 0, 100),
            (0, 10, 63, 0, 0, 100),
            (0, 10, 63, 0, 0, 100),
            (50, 10, 62, 0, 0, 100),
        ]
    );
    h.undo(&mut d).unwrap();
    assert_eq!(all(&d), original);
    // Nothing collapses: the fast in-place pitch remap is kept.
    let d = doc_with(&[rec(0, 10, 60, 100, 0, 0), rec(20, 10, 64, 100, 0, 0)]);
    let p = editor::prepare_scale_y(&d, 0, &ExactFactor::parse("0.5").unwrap()).unwrap();
    assert!(matches!(p, PreparedEdit::RemapSelectedPitch { .. }));
}
