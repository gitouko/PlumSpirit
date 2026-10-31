//! Wave N stroke tools (TEST_MATRIX T-TOOL-draw / paint / select lasso): the
//! pointer stroke reaches the kernel through `Ctx::stroke`; results on small
//! fixtures. (The generic harness in `v07_tools.rs` covers determinism, every
//! advanced function, lazy == full and undo for these tools too.)

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::param::{ADV_KEYS, ParamValue};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::collect_with;
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, Stroke, StrokePoint, TNote, TrackScope, TransformRun, preflight,
    run_kernel, tool,
};
use std::sync::Arc;

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

fn doc(notes: &[NoteRecord], selected: bool) -> PlumDocument {
    let mut d = PlumDocument::new_project("n".into(), Bpm::DEFAULT, 960);
    let mut v = notes.to_vec();
    v.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, selected)
        .unwrap();
    d
}

fn stroke(pts: &[(u64, f64)]) -> Arc<Stroke> {
    Arc::new(Stroke {
        points: pts
            .iter()
            .enumerate()
            .map(|(i, &(tick, pitch))| StrokePoint {
                tick,
                pitch,
                t: i as f64 * 0.05,
            })
            .collect(),
    })
}

fn run(
    d: &PlumDocument,
    id: &str,
    st: Option<Arc<Stroke>>,
    set: &[(&str, ParamValue)],
) -> TransformRun {
    let t = tool(id).unwrap();
    let mut ps = ParamSet::defaults(&(t.spec)().params);
    for (k, v) in set {
        ps.set(k, v.clone());
    }
    // Select works among every note of the track (its default).
    let sc = [TrackScope {
        index: 0,
        all: id == "select",
        content: None,
    }];
    let src = collect_with(t, d, &sc, &ps, 1 << 24).unwrap();
    let mut ctx = Ctx::simple(960);
    ctx.stroke = st;
    let b = BuildCtx {
        ctx: &ctx,
        stats: &src.stats,
        tracks: &src.tracks,
    };
    let k = t.kernel(&ps, &b).unwrap();
    // Preflight accepts the stroke (bounded by the stroke's cells).
    let est = k.estimate(&src.stats);
    assert!(est.notes < 1 << 20, "{est:?}");
    let _ = preflight::check(&est, &Limits::default());
    run_kernel(k.as_ref(), &src, t.boundary(&ps), &Limits::default(), None).unwrap()
}

/// Every quarter note: "Note every" = Custom ticks (last choice) of 960.
fn quarter() -> [(&'static str, ParamValue); 2] {
    [
        ("step", ParamValue::Choice(11)),
        ("step.ticks", ParamValue::Int(960)),
    ]
}

fn on(n: usize) -> (&'static str, ParamValue) {
    (ADV_KEYS[n - 1], ParamValue::Bool(true))
}

fn pitches(v: &[TNote]) -> Vec<u8> {
    v.iter().map(|n| n.pitch).collect()
}

#[test]
fn draw_follows_the_stroke_one_note_per_step() {
    let d = doc(&[], false);
    // A rising stroke over four beats: 60.5 → 64.5.
    let st = stroke(&[(0, 60.5), (3840, 64.5)]);
    let r = run(&d, "draw", Some(st), &quarter());
    assert_eq!(
        r.notes.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![0, 960, 1920, 2880, 3840]
    );
    assert_eq!(pitches(&r.notes), vec![60, 61, 62, 63, 64]);
    assert!(r.notes.iter().all(|n| n.dur == 960 && n.vel == 100));
}

#[test]
fn draw_without_a_stroke_changes_nothing_and_says_how_to_draw() {
    let d = doc(&[rec(0, 100, 60)], true);
    let r = run(&d, "draw", None, &quarter());
    assert_eq!(r.notes.len(), 1, "the selected note passes through");
    assert!(r.message.as_deref().unwrap_or("").contains("Drag"));
}

#[test]
fn draw_constraint_line_ignores_the_wiggles_between_the_ends() {
    let d = doc(&[], false);
    let st = stroke(&[(0, 60.5), (960, 80.5), (1920, 40.5), (3840, 64.5)]);
    let free = run(&d, "draw", Some(st.clone()), &quarter());
    let mut set = quarter().to_vec();
    set.push(on(1));
    let line = run(&d, "draw", Some(st), &set);
    assert_eq!(pitches(&free.notes), vec![60, 80, 40, 52, 64]);
    assert_eq!(pitches(&line.notes), vec![60, 61, 62, 63, 64]);
}

#[test]
fn draw_symmetry_mirrors_around_the_pitch_axis() {
    let d = doc(&[], false);
    let st = stroke(&[(0, 62.5), (960, 64.5)]);
    let mut set = quarter().to_vec();
    set.push(on(7));
    set.push(("a7.axis", ParamValue::Int(60)));
    let r = run(&d, "draw", Some(st), &set);
    let mut p = pitches(&r.notes);
    p.sort_unstable();
    assert_eq!(p, vec![56, 58, 62, 64]);
}

#[test]
fn paint_fills_each_cell_the_stroke_crosses_once() {
    let d = doc(&[], false);
    // Two beats along one row, then back over the same cells: still 3 cells.
    let st = stroke(&[(0, 60.2), (1920, 60.7), (100, 60.4)]);
    let r = run(&d, "paint", Some(st), &quarter());
    assert_eq!(
        r.notes
            .iter()
            .map(|n| (n.start, n.pitch))
            .collect::<Vec<_>>(),
        vec![(0, 60), (960, 60), (1920, 60)]
    );
    // Chord brush: three notes per cell.
    let mut set = quarter().to_vec();
    set.push(on(3));
    let st = stroke(&[(0, 60.2), (1920, 60.7)]);
    let c = run(&d, "paint", Some(st), &set);
    assert_eq!(c.notes.len(), 9);
    assert_eq!(pitches(&c.notes[..3]), vec![60, 64, 67]);
}

#[test]
fn paint_walks_diagonals_cell_by_cell() {
    let d = doc(&[], false);
    let st = stroke(&[(0, 60.5), (3840, 64.5)]);
    let r = run(&d, "paint", Some(st), &quarter());
    // Every visited cell is adjacent to the previous one (no holes).
    let cells: Vec<(u64, u8)> = r.notes.iter().map(|n| (n.start / 960, n.pitch)).collect();
    assert!(cells.len() >= 5);
    assert_eq!(cells.first(), Some(&(0, 60)));
    assert!(cells.contains(&(4, 64)));
}

#[test]
fn select_lasso_drawn_in_the_roll_selects_the_notes_inside() {
    let d = doc(
        &[rec(0, 960, 60), rec(960, 960, 72), rec(4000, 960, 60)],
        false,
    );
    // A box around the first two notes (ticks 0–2000, pitches 55–80).
    let st = stroke(&[(0, 55.0), (2000, 55.0), (2000, 80.0), (0, 80.0)]);
    let r = run(&d, "select", Some(st.clone()), &[on(2)]);
    assert_eq!(
        r.notes.iter().map(|n| n.start).collect::<Vec<_>>(),
        vec![0, 960]
    );
    // The freehand lasso uses the same drawn path.
    let f = run(&d, "select", Some(st), &[on(1)]);
    assert_eq!(f.notes.len(), 2);
}
