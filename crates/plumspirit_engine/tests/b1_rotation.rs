//! B1-S40 (contract §012, B1-0091): Alt free rotation on the shared geometry
//! engine — a rotation as it looks on screen about the selection's center or
//! the opposite corner, exact corners, whole turns as identity, staircase
//! rasterization along the rotated path, a Lazy preview window that holds
//! every source it needs.

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::geom::{Corner, CornerWarp, GeomBox, WarpKind};
use plumspirit_engine::transform::{
    Boundary, Kernel, Limits, PointKernel, Source, TNote, TrackScope, run_kernel,
};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

fn doc_with(notes: &[NoteRecord]) -> PlumDocument {
    let mut d = PlumDocument::new_project("b1s40".into(), Bpm::DEFAULT, 960);
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

/// 960 ticks × 4 rows; with one row = 240 ticks on screen it looks square.
const BOX: GeomBox = GeomBox {
    t0: 0,
    t1: 960,
    p0: 60,
    p1: 63,
};

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
}

#[test]
fn a_quarter_turn_about_the_center_moves_each_corner_to_the_next() {
    let w = CornerWarp::rotation(Corner::TopRight, true, 90.0, 240.0);
    assert_eq!(w.kind, WarpKind::RotateCenter);
    assert!(w.kind.is_rotation());
    assert!((w.degrees() - 90.0).abs() < 1e-9);
    let q = w.quad(&BOX);
    // Counter-clockwise: bottom left → bottom right → top right → top left.
    assert!(close(q[0], (1.0, 0.0)), "{q:?}");
    assert!(close(q[1], (1.0, 1.0)), "{q:?}");
    assert!(close(q[2], (0.0, 1.0)), "{q:?}");
    assert!(close(q[3], (0.0, 0.0)), "{q:?}");
    assert!(w.valid(&BOX));
    // The map agrees with the corners (an affine map through its homography).
    let m = w.map_unit(&BOX, 0.0, 0.0);
    assert!(close(m, (1.0, 0.0)), "{m:?}");
    // About the corner opposite the dragged top right: the bottom left stays.
    let w = CornerWarp::rotation(Corner::TopRight, false, 90.0, 240.0);
    assert_eq!(w.pivot_unit(), (0.0, 0.0));
    let q = w.quad(&BOX);
    assert!(close(q[0], (0.0, 0.0)), "{q:?}");
    assert!(close(q[1], (0.0, 1.0)), "{q:?}");
}

#[test]
fn whole_turns_are_the_identity_and_angles_wrap() {
    for deg in [0.0, 360.0, -360.0, 720.0] {
        assert!(
            CornerWarp::rotation(Corner::BottomLeft, true, deg, 240.0).is_identity(),
            "{deg}"
        );
    }
    let w = CornerWarp::rotation(Corner::BottomLeft, true, -90.0, 240.0);
    assert!((w.degrees() - 270.0).abs() < 1e-9);
    assert!(!w.is_identity());
}

#[test]
fn the_view_aspect_keeps_the_rotation_round_on_screen() {
    // A row twice as tall on screen (480 ticks): the box looks 1:2, so a
    // quarter turn about the center sends the box's bottom left (on screen
    // half as wide as tall) outside the unit box in time.
    let w = CornerWarp::rotation(Corner::TopRight, true, 90.0, 480.0);
    let q = w.quad(&BOX);
    // Visual: width 960 px-units, height 4 × 480 = 1920 → aspect 0.5. The
    // bottom left (−0.25, −0.5 in visual units about the center) goes to
    // (0.5, −0.25): u = 0.5 + 0.5 / 0.5 = 1.5, v = 0.5 − 0.25 = 0.25.
    assert!(close(q[0], (1.5, 0.25)), "{q:?}");
}

#[test]
fn a_rotated_note_becomes_a_staircase_over_the_rows_it_crosses() {
    // The note on 61 across the whole box, a quarter turn about the center:
    // a vertical line at tick 600 over rows 60 – 63.
    let d = doc_with(&[rec(0, 960, 61)]);
    let w = CornerWarp::rotation(Corner::TopRight, true, 90.0, 240.0);
    let k = w.kernel(BOX, None, d.project().keys);
    let src = Source::collect(&d, &SEL, 1 << 20).unwrap();
    let r = run_kernel(&k, &src, Boundary::default(), &Limits::default(), None).unwrap();
    let mut rows: Vec<u8> = r.notes.iter().map(|n| n.pitch).collect();
    rows.sort_unstable();
    rows.dedup();
    assert_eq!(rows, vec![60, 61, 62, 63], "{:?}", r.notes);
    assert!(
        r.notes.iter().all(|n| (599..=601).contains(&n.start)),
        "{:?}",
        r.notes
    );
    // 45°: a diagonal staircase; every piece is on a different row or later.
    let w = CornerWarp::rotation(Corner::TopRight, true, 45.0, 240.0);
    let k = w.kernel(BOX, None, d.project().keys);
    let r = run_kernel(&k, &src, Boundary::default(), &Limits::default(), None).unwrap();
    assert!(r.notes.len() >= 3, "{:?}", r.notes);
    let mut v = r.notes.clone();
    v.sort_by_key(|n| n.start);
    assert!(v.windows(2).all(|p| p[0].pitch <= p[1].pitch), "{v:?}");
}

#[test]
fn small_angles_keep_every_note_and_the_preview_window_holds_its_sources() {
    // Dense one-row-thick art: every note stays (one piece or more), and the
    // Lazy preview's window for a strip contains every source whose result
    // reaches into the strip.
    let mut notes = Vec::new();
    for i in 0..64u64 {
        notes.push(rec(i * 15, 15, 60 + (i % 4) as u8));
    }
    let d = doc_with(&notes);
    let b = GeomBox {
        t0: 0,
        t1: 960,
        p0: 60,
        p1: 63,
    };
    for deg in [7.5, 30.0, 170.0, 300.0] {
        let w = CornerWarp::rotation(Corner::BottomRight, true, deg, 240.0);
        let k = w.kernel(b, None, d.project().keys);
        assert!(k.as_point().is_some());
        let src = Source::collect(&d, &SEL, 1 << 20).unwrap();
        for n in &src.notes {
            let mut out = plumspirit_engine::transform::Output::new(Boundary::default(), 1 << 16);
            PointKernel::map(&k, n, &mut out);
            let (res, _) = out.finish().unwrap();
            assert!(!res.is_empty(), "{deg}: {n:?} vanished");
            for r in &res {
                let (a, z) = k.source_window(r.start, r.end());
                assert!(
                    a <= n.start && n.start <= z,
                    "{deg}: source {n:?} of {r:?} outside window ({a}, {z})"
                );
            }
        }
    }
    let _ = TNote::default();
}
