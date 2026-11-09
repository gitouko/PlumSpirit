use super::*;
use plumspirit_engine::history::Operation;
use plumspirit_engine::model::{ContentRef, Subchannel, SubchannelId};
const START: u64 = (1u64 << 53) + 17;
fn capture() -> serde_json::Value {
    use std::fmt::Write;
    let palette = crate::ui::theme::NotePalette::default();
    let mut hex = String::with_capacity(64 * 64 * 8);
    for y in 0..64 {
        for x in 0..64 {
            let c = palette.entry(if x < 32 { 7 } else { 42 }, 100, true).fill;
            let alpha = if (8..56).contains(&x) && (8..56).contains(&y) {
                255
            } else {
                0
            };
            write!(
                &mut hex,
                "{:02x}{:02x}{:02x}{:02x}",
                c.r(),
                c.g(),
                c.b(),
                alpha
            )
            .unwrap();
        }
    }
    serde_json::json!({"version":1,"width":64,"height":64,"hex":hex,
        "source":{"version":1,"family":"Controlled pixel fixture","style":"","text":"O","palette":0}})
}
fn store(d: &mut Arc<PlumDocument>, value: serde_json::Value) {
    Arc::make_mut(d).project_mut().plugin_data = vec![(
        ID.into(),
        serde_json::json!({"version":1,"values":{"color_glyph_capture":value}}).to_string(),
    )];
}
fn color_run(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    values: &[ToolValue],
    (preview, generation): (bool, u64),
    doc: &Arc<PlumDocument>,
    history: &mut History,
    (track, content): (u32, Option<u32>),
) -> Result<Option<Commits>, String> {
    let decl = m
        .plugin(ID)
        .unwrap()
        .decls
        .tools
        .iter()
        .find(|t| t.id == "color-artwork")
        .unwrap()
        .clone();
    m.run_tool(
        ID,
        ToolJob {
            stroke: None,
            tool: decl.id.clone(),
            params: params_literal(&decl, values),
            track,
            content,
            preview,
            seed: 1,
            keys: u16::from(doc.project().keys.max_pitch()) + 1,
            generation,
            world: Some(world(doc, history)),
        },
    )
    .unwrap();
    result(m, prefs, doc, history, generation)
}
#[test]
fn fixed_color_pixels_generate_exact_native_routes_without_font_or_ui_grants_all_targets() {
    for keys in [128, 256] {
        for target in ["track", "subtrack", "linked"] {
            for pixels in [false, true] {
                let (mut doc, mut history) = project(0, 60, 0);
                if keys == 256 {
                    Arc::make_mut(&mut doc).project_mut().keys =
                        plumspirit_engine::model::project::KeyRange::Extended256;
                }
                let index = if target == "track" {
                    0
                } else {
                    history
                        .apply(
                            Arc::make_mut(&mut doc),
                            Operation::AddSubtracks {
                                parent: 0,
                                count: 1,
                            },
                        )
                        .unwrap();
                    1
                };
                history
                    .apply(
                        Arc::make_mut(&mut doc),
                        Operation::AddSubchannels {
                            track: index,
                            subs: vec![
                                Subchannel::new(SubchannelId(7), 9),
                                Subchannel::new(SubchannelId(42), 3),
                            ],
                        },
                    )
                    .unwrap();
                let content = if target == "linked" {
                    use plumspirit_engine::editor::clips::{add_clip, duplicate_clips};
                    let op = add_clip(&doc, 0, 10000, 20000).unwrap();
                    history.apply(Arc::make_mut(&mut doc), op).unwrap();
                    let c = doc.clips().contents()[0].id;
                    let first = doc.clips().placements_of(ContentRef::Owned(c))[0].id;
                    let op = duplicate_clips(&doc, &[(first, index, 50000)], true).unwrap();
                    history.apply(Arc::make_mut(&mut doc), op).unwrap();
                    Some(c)
                } else {
                    None
                };
                history.reset(false);
                store(&mut doc, capture());
                let track = doc.track(index).unwrap().id().0;
                let original = doc.export_fingerprint();
                let (mut m, mut prefs, root) = with_example(
                    &format!("text-color-{keys}-{target}-{pixels}"),
                    ID,
                    &doc,
                    &mut history,
                );
                // The fixed musical source needs neither installed fonts nor a UI surface.
                prefs
                    .installed
                    .get_mut(ID)
                    .unwrap()
                    .granted
                    .retain(|c| c == "notes.edit" || c == "project.read");
                m.reload(ID, &prefs);
                settle(&mut m, &prefs, &mut || world(&doc, &mut history));
                assert_eq!(m.plugin(ID).unwrap().status, Status::Ready);
                let decl = m.plugin(ID).unwrap().decls.tools[1].clone();
                let mut values = decl.defaults();
                let first = if keys == 256 { 200 } else { 40 };
                values[2] = ToolValue::Int(first);
                values[3] = ToolValue::Int(START as i64);
                values[4] = ToolValue::Int(7);
                values[7] = ToolValue::Bool(pixels);
                let preview = color_run(
                    &mut m,
                    &prefs,
                    &values,
                    (true, 1),
                    &doc,
                    &mut history,
                    (track, content.map(|c| c.0)),
                )
                .unwrap()
                .unwrap();
                let expected = preview.result().export_fingerprint();
                let host = content
                    .map(ContentRef::Owned)
                    .unwrap_or(ContentRef::Track(plumspirit_engine::model::TrackId(track)));
                let notes = preview.result().notes_of(host).unwrap();
                assert_eq!(notes.len(), if pixels { 576 } else { 48 });
                for n in notes.iter() {
                    let n = n.record();
                    assert_eq!(n.attrs.velocity(), 100);
                    assert!((first + 4..=first + 27).contains(&i64::from(n.attrs.pitch())));
                    assert!(n.start_tick >= START + 28 && n.start_tick < START + 196);
                    assert_eq!({ n.duration_ticks }, if pixels { 7 } else { 84 });
                    assert!(matches!((n.sub.0, n.attrs.channel()), (7, 9) | (42, 3)));
                }
                assert_eq!(doc.export_fingerprint(), original);
                drop(preview);
                let mut apply = color_run(
                    &mut m,
                    &prefs,
                    &values,
                    (false, 2),
                    &doc,
                    &mut history,
                    (track, content.map(|c| c.0)),
                )
                .unwrap()
                .unwrap();
                assert_eq!(apply.result().export_fingerprint(), expected);
                assert_eq!(apply.steps.len(), 1);
                assert_eq!(apply.steps[0].operations, 1);
                apply.apply(&mut history, &mut doc).unwrap();
                drop(apply);
                if let Some(c) = content {
                    assert_eq!(doc.clips().placements_of(ContentRef::Owned(c)).len(), 2);
                    assert_eq!(doc.track(0).unwrap().notes.len(), 0);
                    assert_eq!(doc.track(1).unwrap().notes.len(), 0);
                }
                history
                    .undo(Arc::get_mut(&mut doc).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(doc.export_fingerprint(), original);
                history
                    .redo(Arc::get_mut(&mut doc).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(doc.export_fingerprint(), expected);
                let bytes = plumspirit_engine::native::lup::project_to_bytes(&doc).unwrap();
                let (reopened, _) =
                    plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
                assert_eq!(reopened.project().plugin_data, doc.project().plugin_data);
                assert_eq!(reopened.export_fingerprint(), expected);
                drop(m);
                cleanup(root);
            }
        }
    }
}
#[test]
fn invalid_fixed_capture_and_pitch_preserve_source_and_worker_recovers() {
    let (mut doc, mut history) = project(0, 60, 0);
    let mut bad = capture();
    bad["hex"] = serde_json::json!("00");
    store(&mut doc, bad);
    let (mut m, prefs, root) = with_example("text-color-invalid", ID, &doc, &mut history);
    let decl = m.plugin(ID).unwrap().decls.tools[1].clone();
    let mut values = decl.defaults();
    let track = doc.track(0).unwrap().id().0;
    let before = doc.export_fingerprint();
    assert!(
        color_run(
            &mut m,
            &prefs,
            &values,
            (true, 1),
            &doc,
            &mut history,
            (track, None),
        )
        .err()
        .unwrap()
        .contains("Invalid bounded glyph pixels")
    );
    assert_eq!(doc.export_fingerprint(), before);
    store(&mut doc, capture());
    values[2] = ToolValue::Int(200);
    assert!(
        color_run(
            &mut m,
            &prefs,
            &values,
            (true, 2),
            &doc,
            &mut history,
            (track, None),
        )
        .err()
        .unwrap()
        .contains("outside this project's key mode")
    );
    assert_eq!(doc.export_fingerprint(), before);
    values[2] = ToolValue::Int(40);
    let preview = color_run(
        &mut m,
        &prefs,
        &values,
        (true, 3),
        &doc,
        &mut history,
        (track, None),
    )
    .unwrap()
    .unwrap();
    assert_eq!(preview.result().note_count(), 24);
    drop(preview);
    assert_eq!(doc.export_fingerprint(), before);
    drop(m);
    cleanup(root);
}
