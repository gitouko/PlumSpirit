//! Installed author code, real worker/native transactions; fixed manufactured log.
use super::*;
const ID: &str = "plumspirit.example.midi-input-laboratory";
pub(crate) fn capture() -> serde_json::Value {
    serde_json::json!({"version":1,"count":10,"name":"Manufactured fixture","truncated":false,
        "status":[0x91,0x91,0xb2,0xe3,0x91,0xb2,0xe3,0x81,0xe3,0x92],
        "data1":[60,60,1,0,60,1,0,60,127,72],"data2":[90,70,0,0,0,127,64,20,127,100],
        "time":[1000,1010,1020,1030,1040,1050,1060,1070,1080,1090],
        "driver_ms":[1000,1010,1020,1030,1040,1050,1060,1070,1080,1090],
        "sequence":[1,2,3,4,5,6,7,8,9,10],"received_ns":[0,1,2,3,4,5,6,7,8,9],
        "out_of_order":[false,false,false,false,false,false,false,false,false,false],
        "driver_overrun":[false,false,false,false,false,false,false,false,false,false]})
}
fn store(doc: &mut Arc<PlumDocument>, capture: serde_json::Value) {
    Arc::make_mut(doc).project_mut().plugin_data = vec![(
        ID.into(),
        serde_json::json!({"version":1,"values":{"midi_input_capture":capture}}).to_string(),
    )];
}
fn cleanup(root: PathBuf) {
    let root = root.canonicalize().unwrap();
    let base = std::env::temp_dir().canonicalize().unwrap();
    assert!(
        root.parent() == Some(base.as_path())
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plumspirit-example-test-midi-lab")
    );
    std::fs::remove_dir_all(root).unwrap();
}
fn run(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    values: &[ToolValue],
    preview: bool,
    generation: u64,
    doc: &Arc<PlumDocument>,
    history: &mut History,
) -> Result<Option<Commits>, String> {
    let decl = &m.plugin(ID).unwrap().decls.tools[0];
    m.run_tool(
        ID,
        ToolJob {
            stroke: None,
            tool: decl.id.clone(),
            params: params_literal(decl, values),
            track: doc.track(0).unwrap().id().0,
            content: None,
            preview,
            seed: 17,
            keys: u16::from(doc.project().keys.max_pitch()) + 1,
            generation,
            world: Some(world(doc, history)),
        },
    )
    .unwrap();
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
        .expect("MIDI Laboratory tool result")
}
#[test]
fn installed_midi_laboratory_three_author_mappings_have_exact_preview_apply_undo_redo_both_key_modes()
 {
    for keys in [128, 256] {
        for mode in 0..3 {
            let (mut doc, mut history) = project(0, 60, 0);
            if keys == 256 {
                Arc::get_mut(&mut doc).unwrap().project_mut().keys =
                    plumspirit_engine::model::project::KeyRange::Extended256;
            }
            store(&mut doc, capture());
            let (mut m, prefs, root) = with_example(
                &format!("midi-lab-mapping-{keys}-{mode}"),
                ID,
                &doc,
                &mut history,
            );
            let p = m.plugin(ID).unwrap();
            assert_eq!((p.decls.commands.len(), p.decls.tools.len()), (5, 1));
            let mut values = p.decls.tools[0].defaults();
            assert_eq!(values.len(), 11);
            let start = 9_007_199_254_741_001;
            values[0] = ToolValue::Choice(mode);
            values[1] = ToolValue::Int(start);
            if keys == 256 {
                if mode == 0 {
                    values[3] = ToolValue::Int(128);
                } else {
                    values[4] = ToolValue::Int(200);
                }
            }
            let original = doc.export_fingerprint();
            let first = run(&mut m, &prefs, &values, true, 1, &doc, &mut history)
                .unwrap()
                .unwrap();
            let signature = first.result().export_fingerprint();
            let mut notes = Vec::new();
            first
                .result()
                .track(0)
                .unwrap()
                .notes
                .query(0, u64::MAX, |_, n| {
                    notes.push((
                        n.start_tick,
                        n.duration_ticks,
                        n.attrs.pitch(),
                        n.attrs.velocity(),
                        n.attrs.channel(),
                        n.sub.0,
                    ))
                });
            let offset = if keys == 256 { 128 } else { 0 };
            let base = if keys == 256 { 200 } else { 60 };
            let expected = match mode {
                0 => vec![
                    (start as u64, 80, 60 + offset, 90, 1, 0),
                    (start as u64 + 20, 120, 60 + offset, 70, 1, 0),
                    (start as u64 + 180, 120, 72 + offset, 100, 2, 0),
                ],
                1 => vec![
                    (start as u64 + 40, 120, base, 96, 2, 0),
                    (start as u64 + 100, 120, base + 36, 96, 2, 0),
                ],
                _ => vec![
                    (start as u64 + 60, 120, base - 36, 96, 3, 0),
                    (start as u64 + 120, 120, base, 96, 3, 0),
                    (start as u64 + 160, 120, base + 36, 96, 3, 0),
                ],
            };
            assert_eq!(notes, expected);
            assert_eq!(doc.export_fingerprint(), original);
            let second = run(&mut m, &prefs, &values, true, 2, &doc, &mut history)
                .unwrap()
                .unwrap();
            assert_eq!(second.result().export_fingerprint(), signature);
            drop(first);
            drop(second);
            let mut apply = run(&mut m, &prefs, &values, false, 3, &doc, &mut history)
                .unwrap()
                .unwrap();
            assert_eq!(apply.result().export_fingerprint(), signature);
            assert_eq!(apply.steps.len(), 1);
            assert_eq!(apply.steps[0].operations, 1);
            apply.apply(&mut history, &mut doc).unwrap();
            drop(apply);
            assert_eq!(doc.export_fingerprint(), signature);
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
            assert_eq!(doc.export_fingerprint(), signature);
            drop(m);
            cleanup(root);
        }
    }
}
#[test]
fn captured_project_and_preset_roundtrip_exact_source_data_and_default_midi_export() {
    let (mut doc, mut history) = project(0, 60, 0);
    store(&mut doc, capture());
    let midi = plumspirit_engine::midi::export_to_bytes(&doc).unwrap();
    let bytes = plumspirit_engine::native::lup::project_to_bytes(&doc).unwrap();
    let (loaded, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert_eq!(loaded.project().plugin_data, doc.project().plugin_data);
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(&loaded).unwrap(),
        midi
    );
    let (mut m, prefs, root) = with_example("midi-lab-preset", ID, &doc, &mut history);
    let original = doc.project().plugin_data.clone();
    for command in ["save", "clear", "load"] {
        m.run_command(ID, command, Some(world(&doc, &mut history)))
            .unwrap();
        let events = settle(&mut m, &prefs, &mut || world(&doc, &mut history));
        for event in events {
            if let PluginEvent::ProjectData { plugin, text } = event {
                assert_eq!(plugin, ID);
                let data = &mut Arc::make_mut(&mut doc).project_mut().plugin_data;
                data.retain(|(id, _)| id != ID);
                if !text.is_empty() {
                    data.push((ID.into(), text));
                }
            }
        }
        if command == "clear" {
            assert!(doc.project().plugin_data.is_empty());
        }
    }
    let restored: serde_json::Value =
        serde_json::from_str(&doc.project().plugin_data[0].1).unwrap();
    let before: serde_json::Value = serde_json::from_str(&original[0].1).unwrap();
    assert_eq!(restored, before);
    assert_eq!(doc.note_count(), 0);
    assert!(!history.can_undo());
    drop(m);
    cleanup(root);
}
#[test]
fn malformed_order_overflow_outside_and_missing_capture_fail_atomically_and_worker_recovers() {
    let (mut doc, mut history) = project(0, 60, 0);
    store(&mut doc, capture());
    let (mut m, prefs, root) = with_example("midi-lab-errors", ID, &doc, &mut history);
    let mut values = m.plugin(ID).unwrap().decls.tools[0].defaults();
    let original = doc.export_fingerprint();
    for mode in 0..5 {
        let mut c = capture();
        match mode {
            0 => c["version"] = serde_json::json!(2),
            1 => c["sequence"][0] = serde_json::json!(i64::MAX),
            2 => c["time"][0] = serde_json::json!(-1),
            3 => c["status"][0] = serde_json::json!(0xf0),
            _ => c["count"] = serde_json::json!(4096),
        }
        store(&mut doc, c);
        assert!(run(&mut m, &prefs, &values, true, mode + 1, &doc, &mut history).is_err());
        assert_eq!(doc.export_fingerprint(), original);
        assert!(!history.can_undo());
    }
    store(&mut doc, capture());
    values[1] = ToolValue::Int(i64::MAX);
    assert!(run(&mut m, &prefs, &values, true, 10, &doc, &mut history).is_err());
    assert_eq!(doc.export_fingerprint(), original);
    values[1] = ToolValue::Int(0);
    values[3] = ToolValue::Int(200);
    assert!(run(&mut m, &prefs, &values, true, 11, &doc, &mut history).is_err());
    values[9] = ToolValue::Choice(1);
    let clamped = run(&mut m, &prefs, &values, true, 12, &doc, &mut history)
        .unwrap()
        .unwrap();
    clamped
        .result()
        .track(0)
        .unwrap()
        .notes
        .query(0, u64::MAX, |_, n| assert_eq!(n.attrs.pitch(), 127));
    drop(clamped);
    values[3] = ToolValue::Int(0);
    values[9] = ToolValue::Choice(0);
    values[10] = ToolValue::Bool(false);
    let recovered = run(&mut m, &prefs, &values, true, 13, &doc, &mut history)
        .unwrap()
        .unwrap();
    recovered
        .result()
        .track(0)
        .unwrap()
        .notes
        .query(0, u64::MAX, |_, n| assert_eq!(n.attrs.channel(), 0));
    drop(recovered);
    Arc::make_mut(&mut doc).project_mut().plugin_data.clear();
    assert!(run(&mut m, &prefs, &values, true, 14, &doc, &mut history).is_err());
    assert_eq!(doc.export_fingerprint(), original);
    drop(m);
    cleanup(root);
}
