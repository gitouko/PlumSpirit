//! v0.8 semantic editors, engine side (contract §185–§188, test §242;
//! TEST_MATRIX T8-I-QUAD): the projective quad's corner values drive the
//! Perspective tool — moving BL changes the result, previews never touch the
//! project or its history, Apply + Undo restores the source. (Pointer drags
//! on the editors themselves are tested in the app: `ui::param_editors`.)

use plumspirit_engine::history::History;
use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::transform::editors::{Editor, editor};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ParamValue, TrackScope, TransformRun, run_kernel, tool,
};

fn doc() -> PlumDocument {
    let mut d = PlumDocument::new_project("v08q".into(), Bpm::DEFAULT, 960);
    let mut v: Vec<NoteRecord> = (0..64u64)
        .map(|i| NoteRecord {
            start_tick: (i % 8) * 240,
            duration_ticks: 240,
            attrs: NoteAttrs::new(48 + (i / 8) as u8 * 2, 100, 0, 64, NoteOffKind::NoteOff)
                .unwrap(),
            sub: SubchannelId(0),
            group: 0,
        })
        .collect();
    v.sort_by_key(|n| n.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&v, true)
        .unwrap();
    d
}

fn run(d: &PlumDocument, corners: &[i64]) -> TransformRun {
    let def = tool("perspective").unwrap();
    let mut values = ParamSet::defaults(&(def.spec)().params);
    values.set("a1", ParamValue::Bool(true));
    values.set("a1.corners", ParamValue::List(corners.to_vec()));
    let scope = [TrackScope {
        index: 0,
        all: false,
        content: None,
    }];
    let src = collect_for(def, d, &scope, 10_000_000).unwrap();
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
}

#[test]
fn the_projective_quad_drives_perspective_and_previews_change_nothing() {
    let def = tool("perspective").unwrap();
    let spec = (def.spec)();
    let p = spec.params.iter().find(|p| p.key == "a1.corners").unwrap();
    assert_eq!(editor("perspective", p), Editor::Quad { offsets: false });
    let rest = [0, 0, 1000, 0, 1000, 1000, 0, 1000];
    let mut d = doc();
    let before = d.fingerprint();
    let base = run(&d, &rest);
    // Drag BL to (200, 100): only BL's values change, the result changes.
    let moved = [200, 100, 1000, 0, 1000, 1000, 0, 1000];
    let preview = run(&d, &moved);
    assert_ne!(base.notes, preview.notes, "moving BL changes the preview");
    assert_eq!(d.fingerprint(), before, "previews never touch the project");
    // Cancel = the project as it was; Apply + Undo restores the source.
    let mut h = History::default();
    assert!(!h.can_undo());
    let op = build_operation(&mut d, &preview, "Perspective").unwrap();
    h.apply(&mut d, op).unwrap();
    assert_ne!(d.fingerprint(), before);
    h.undo(&mut d).unwrap();
    assert_eq!(d.fingerprint(), before);
}
