//! The installed authored tool uses the production plugin worker/transaction path.
use super::*;
const ID: &str = "plumspirit.example.text-art-studio";
fn tool(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    values: &[ToolValue],
    (preview, generation): (bool, u64),
    doc: &Arc<PlumDocument>,
    history: &mut History,
) -> Result<Option<Commits>, String> {
    let decl = m.plugin(ID).unwrap().decls.tools[0].clone();
    m.run_tool(
        ID,
        ToolJob {
            stroke: None,
            tool: decl.id.clone(),
            params: params_literal(&decl, values),
            track: doc.track(0).unwrap().id().0,
            content: None,
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
fn result(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    doc: &Arc<PlumDocument>,
    history: &mut History,
    generation: u64,
) -> Result<Option<Commits>, String> {
    settle(m, prefs, &mut || world(doc, history))
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::ToolResult {
                generation: g,
                result,
                ..
            } if g == generation => Some(result),
            _ => None,
        })
        .expect("Text Art Studio worker result")
}
fn cleanup(root: PathBuf) {
    let p = root.canonicalize().unwrap();
    let base = std::env::temp_dir().canonicalize().unwrap();
    assert!(
        p.parent() == Some(base.as_path())
            && p.file_name().is_some_and(|n| n
                .to_string_lossy()
                .starts_with("plumspirit-example-test-text-"))
    );
    std::fs::remove_dir_all(p).unwrap();
}
#[test]
fn installed_multiline_art_composition_has_deterministic_native_preview_apply_one_undo_redo_both_modes()
 {
    for keys in [128, 256] {
        let mut signatures = Vec::new();
        for (font, filled) in [(0, true), (1, true), (0, false)] {
            let (mut doc, mut history) = project(0, 60, 0);
            if keys == 256 {
                Arc::get_mut(&mut doc).unwrap().project_mut().keys =
                    plumspirit_engine::model::project::KeyRange::Extended256;
            }
            let (mut m, prefs, root) = with_example(
                &format!("text-composition-{keys}-{font}-{filled}"),
                ID,
                &doc,
                &mut history,
            );
            let p = m.plugin(ID).unwrap();
            assert_eq!(p.decls.commands.len(), 10);
            assert_eq!(p.decls.tools.len(), 2);
            assert!(matches!(
                p.decls.tools[0].params[0].kind,
                super::custom::ToolParamKind::Text {
                    multiline: true,
                    max_len: 128
                }
            ));
            let mut values = p.decls.tools[0].defaults();
            values[1] = ToolValue::Choice(font);
            values[8] = ToolValue::Bool(filled);
            values[9] = ToolValue::Int(120);
            values[10] = ToolValue::Int(30);
            values[11] = ToolValue::Int(24);
            values[12] = ToolValue::Int(102);
            let original = doc.export_fingerprint();
            let preview = tool(&mut m, &prefs, &values, (true, 1), &doc, &mut history)
                .unwrap()
                .unwrap();
            let expected = preview.result().export_fingerprint();
            let count = preview.result().note_count();
            assert!(count > 100 && count <= 256 * keys);
            assert_eq!(doc.export_fingerprint(), original);
            let mut high = false;
            preview
                .result()
                .track(0)
                .unwrap()
                .notes
                .query(0, u64::MAX, |_, n| {
                    assert!(n.start_tick >= 120 && n.start_tick < 120 + 256 * 30);
                    assert_eq!({ n.duration_ticks }, 24);
                    assert_eq!(n.attrs.velocity(), 102);
                    assert!(u16::from(n.attrs.pitch()) < keys as u16);
                    high |= n.attrs.pitch() > 127;
                });
            assert_eq!(high, keys == 256);
            drop(preview);
            let mut apply = tool(&mut m, &prefs, &values, (false, 2), &doc, &mut history)
                .unwrap()
                .unwrap();
            assert_eq!(apply.result().export_fingerprint(), expected);
            assert_eq!(apply.steps.len(), 1);
            assert_eq!(apply.steps[0].operations, 1);
            apply.apply(&mut history, &mut doc).unwrap();
            drop(apply);
            assert_eq!(doc.note_count(), count);
            history
                .undo(Arc::get_mut(&mut doc).unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(doc.export_fingerprint(), original);
            assert!(!history.can_undo());
            history
                .redo(Arc::get_mut(&mut doc).unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(doc.export_fingerprint(), expected);
            signatures.push(expected);
            drop(m);
            cleanup(root);
        }
        assert_ne!(signatures[0], signatures[1]);
        assert_ne!(signatures[0], signatures[2]);
    }
}
#[test]
fn invalid_missing_glyph_and_hard_line_inputs_leave_source_and_project_unchanged_then_worker_recovers()
 {
    let (doc, mut history) = project(0, 60, 0);
    let (mut m, prefs, root) = with_example("text-invalid", ID, &doc, &mut history);
    let original = doc.export_fingerprint();
    let mut values = m.plugin(ID).unwrap().decls.tools[0].defaults();
    values[0] = ToolValue::Text("Original \u{10ffff}".into());
    assert!(
        tool(&mut m, &prefs, &values, (true, 1), &doc, &mut history)
            .err()
            .expect("Expected authored tool refusal")
            .contains("missing glyphs")
    );
    assert_eq!(values[0], ToolValue::Text("Original \u{10ffff}".into()));
    assert_eq!(doc.export_fingerprint(), original);
    values[0] = ToolValue::Text("a\n".repeat(8));
    assert!(
        tool(&mut m, &prefs, &values, (true, 2), &doc, &mut history)
            .err()
            .expect("Expected authored tool refusal")
            .contains("max_lines")
    );
    assert_eq!(doc.export_fingerprint(), original);
    values[0] = ToolValue::Text(String::new());
    assert!(
        tool(&mut m, &prefs, &values, (true, 3), &doc, &mut history)
            .unwrap()
            .is_none()
    );
    assert_eq!(doc.export_fingerprint(), original);
    values[0] = ToolValue::Text("office\r\né".into());
    assert!(
        tool(&mut m, &prefs, &values, (true, 4), &doc, &mut history)
            .unwrap()
            .unwrap()
            .result()
            .note_count()
            > 0
    );
    assert_eq!(doc.export_fingerprint(), original);
    drop(m);
    cleanup(root);
}
#[test]
fn authored_multiline_art_targets_owned_linked_clip_content_in_local_ticks() {
    for mixed in [false, true] {
        let (mut doc, mut history) = project(0, 60, 0);
        Arc::get_mut(&mut doc).unwrap().project_mut().keys =
            plumspirit_engine::model::project::KeyRange::Extended256;
        let setup = Host::new(
            Limits::default(),
            [
                Capability::ReadProject,
                Capability::EditNotes,
                Capability::EditTracks,
            ],
        )
        .unwrap();
        setup.set_world(Some(world(&doc, &mut history)));
        setup.run("text-clip-fixture.lua","local tx=plum.transaction{};local child=tx:add_subtracks(plum.project.current():track(1),1)[1];local c=tx:add_clip(child,10000,20000);tx:duplicate_clips(c,{start=50000,linked=true});tx:insert_notes(c:content(),{{start=1,gate=2,key=200,velocity=70}});tx:commit()").unwrap();
        let mut prepared = setup.take_commits().unwrap();
        setup.set_world(None);
        prepared.apply(&mut history, &mut doc).unwrap();
        drop(prepared);
        history.reset(false);
        let content = doc.clips().contents()[0].id;
        let track = doc.track(1).unwrap().id().0;
        let (mut m, prefs, root) = with_example("text-clips", ID, &doc, &mut history);
        let decl = m.plugin(ID).unwrap().decls.tools[0].clone();
        let mut values = decl.defaults();
        if mixed {
            values[0] = ToolValue::Text("A😀A\nπ🔥".into());
            values[1] = ToolValue::Choice(3);
        }
        values[9] = ToolValue::Int(120);
        values[10] = ToolValue::Int(30);
        let original = doc.export_fingerprint();
        let mut expected = None;
        for (preview, generation) in [(true, 1), (false, 2)] {
            m.run_tool(
                ID,
                ToolJob {
                    stroke: None,
                    tool: decl.id.clone(),
                    params: params_literal(&decl, &values),
                    track,
                    content: Some(content.0),
                    preview,
                    seed: 1,
                    keys: 256,
                    generation,
                    world: Some(world(&doc, &mut history)),
                },
            )
            .unwrap();
            let mut commits = result(&mut m, &prefs, &doc, &mut history, generation)
                .unwrap()
                .unwrap();
            let signature = commits.result().export_fingerprint();
            assert_eq!(commits.result().track(0).unwrap().notes.len(), 0);
            assert_eq!(commits.result().track(1).unwrap().notes.len(), 0);
            let records = commits
                .result()
                .notes_of(plumspirit_engine::model::ContentRef::Owned(content))
                .unwrap();
            assert!(records.len() > 100);
            for note in records.iter() {
                let n = note.record();
                if n.attrs.velocity() != 70 {
                    assert!(n.start_tick >= 120 && n.start_tick < 8000);
                    assert!(n.attrs.pitch() > 127);
                }
            }
            assert_eq!(doc.export_fingerprint(), original);
            if preview {
                expected = Some(signature);
            } else {
                assert_eq!(Some(signature), expected);
                commits.apply(&mut history, &mut doc).unwrap();
            }
            drop(commits);
        }
        assert_eq!(doc.clips().explicit_placements().len(), 2);
        history
            .undo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), original);
        history
            .redo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(Some(doc.export_fingerprint()), expected);
        drop(m);
        cleanup(root);
    }
}

#[test]
fn installed_composition_shapes_mixed_latin_greek_cyrillic_without_losing_native_transactions() {
    for keys in [128, 256] {
        let (mut doc, mut history) = project(0, 60, 0);
        if keys == 256 {
            Arc::get_mut(&mut doc).unwrap().project_mut().keys =
                plumspirit_engine::model::project::KeyRange::Extended256;
        }
        let (mut m, prefs, root) =
            with_example(&format!("text-script-{keys}"), ID, &doc, &mut history);
        let mut values = m.plugin(ID).unwrap().decls.tools[0].defaults();
        values[0] = ToolValue::Text("AΩЖ\nЖΩA".into());
        let original = doc.export_fingerprint();
        let preview = tool(&mut m, &prefs, &values, (true, 1), &doc, &mut history)
            .unwrap()
            .unwrap();
        let expected = preview.result().export_fingerprint();
        assert!(preview.result().note_count() > 50);
        assert_ne!(expected, original);
        drop(preview);
        assert_eq!(doc.export_fingerprint(), original);
        let mut apply = tool(&mut m, &prefs, &values, (false, 2), &doc, &mut history)
            .unwrap()
            .unwrap();
        assert_eq!(apply.result().export_fingerprint(), expected);
        assert_eq!(apply.steps.len(), 1);
        apply.apply(&mut history, &mut doc).unwrap();
        drop(apply);
        history.undo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), original);
        history.redo(Arc::make_mut(&mut doc)).unwrap().unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
        drop(m);
        cleanup(root);
    }
}

#[test]
fn explicit_fallback_art_produces_distinct_faces_and_native_transactions_in_both_key_modes() {
    for keys in [128, 256] {
        let mut signatures = Vec::new();
        for choice in [3, 4] {
            let (mut doc, mut history) = project(0, 60, 0);
            if keys == 256 {
                Arc::get_mut(&mut doc).unwrap().project_mut().keys =
                    plumspirit_engine::model::project::KeyRange::Extended256;
            }
            let (mut m, prefs, root) = with_example(
                &format!("text-fallback-{keys}-{choice}"),
                ID,
                &doc,
                &mut history,
            );
            let mut values = m.plugin(ID).unwrap().decls.tools[0].defaults();
            values[0] = ToolValue::Text("AV😀A\nπ🔥".into());
            values[1] = ToolValue::Choice(choice);
            values[9] = ToolValue::Int(1000000000000);
            let original = doc.export_fingerprint();
            let preview = tool(&mut m, &prefs, &values, (true, 1), &doc, &mut history)
                .unwrap()
                .unwrap();
            let signature = preview.result().export_fingerprint();
            assert_ne!(signature, original);
            assert!(preview.result().note_count() > 100);
            preview
                .result()
                .track(0)
                .unwrap()
                .notes
                .query(0, u64::MAX, |_, n| {
                    assert!(n.start_tick >= 1000000000000);
                    assert!(u16::from(n.attrs.pitch()) < keys);
                });
            drop(preview);
            assert_eq!(doc.export_fingerprint(), original);
            let mut apply = tool(&mut m, &prefs, &values, (false, 2), &doc, &mut history)
                .unwrap()
                .unwrap();
            assert_eq!(apply.result().export_fingerprint(), signature);
            assert_eq!(apply.steps.len(), 1);
            apply.apply(&mut history, &mut doc).unwrap();
            drop(apply);
            history.undo(Arc::make_mut(&mut doc)).unwrap().unwrap();
            assert_eq!(doc.export_fingerprint(), original);
            history.redo(Arc::make_mut(&mut doc)).unwrap().unwrap();
            assert_eq!(doc.export_fingerprint(), signature);
            signatures.push(signature);
            drop(m);
            cleanup(root);
        }
        assert_ne!(
            signatures[0], signatures[1],
            "Primary font actually changes mixed artwork"
        );
    }
}
#[test]
fn missing_or_uncomposed_fallback_clusters_preserve_source_then_worker_recovers() {
    let (doc, mut history) = project(0, 60, 0);
    let (mut m, prefs, root) = with_example("text-fallback-reject", ID, &doc, &mut history);
    let mut values = m.plugin(ID).unwrap().decls.tools[0].defaults();
    values[1] = ToolValue::Choice(3);
    let original = doc.export_fingerprint();
    values[0] = ToolValue::Text("A😀A".into());
    values[1] = ToolValue::Choice(0);
    assert!(
        tool(&mut m, &prefs, &values, (true, 0), &doc, &mut history)
            .err()
            .expect("Single face stays single face")
            .contains("missing glyphs")
    );
    assert_eq!(doc.export_fingerprint(), original);
    values[1] = ToolValue::Choice(3);
    for (i, text) in ["👍🏽", "👨‍👩‍👧‍👦", "1️⃣", "Original \u{10ffff}"]
        .into_iter()
        .enumerate()
    {
        values[0] = ToolValue::Text(text.into());
        let error = tool(
            &mut m,
            &prefs,
            &values,
            (true, i as u64 + 1),
            &doc,
            &mut history,
        )
        .err()
        .expect("Unsupported appearance must be refused");
        assert!(
            error.contains("missing glyphs") || error.contains("compose this emoji"),
            "{error}"
        );
        assert_eq!(values[0], ToolValue::Text(text.into()));
        assert_eq!(doc.export_fingerprint(), original);
        assert!(!history.can_undo());
    }
    values[0] = ToolValue::Text("A😀A 🇺🇸".into());
    let preview = tool(&mut m, &prefs, &values, (true, 8), &doc, &mut history)
        .unwrap()
        .unwrap();
    assert!(preview.result().note_count() > 100);
    drop(preview);
    assert_eq!(doc.export_fingerprint(), original);
    drop(m);
    cleanup(root);
}

#[path = "text_color_artwork_tests.rs"]
mod color_artwork;
