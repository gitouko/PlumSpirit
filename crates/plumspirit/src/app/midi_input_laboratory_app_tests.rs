mod midi_input_laboratory_app {
    //! Native tool App integration using a stored manufactured capture, no device I/O.
    use super::*;
    const ID: &str = "plumspirit.example.midi-input-laboratory";
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "plum-midi-lab-app-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let p = self.0.canonicalize().unwrap();
            let base = std::env::temp_dir().canonicalize().unwrap();
            assert!(
                p.parent() == Some(base.as_path())
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("plum-midi-lab-app-")
            );
            std::fs::remove_dir_all(p).unwrap();
        }
    }
    fn setup(extended: bool, linked: Option<bool>) -> (Harness, Temp) {
        let temp = Temp::new();
        let mut d = doc();
        if extended {
            d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
        }
        if let Some(subtrack) = linked {
            let mut history = plumspirit_engine::history::History::new(Default::default());
            if subtrack {
                history
                    .apply(
                        &mut d,
                        plumspirit_engine::history::Operation::AddSubtracks {
                            parent: 0,
                            count: 1,
                        },
                    )
                    .unwrap();
            } else {
                d.add_track().unwrap();
            }
            let track = d.track(1).unwrap().id();
            let mut props = d.track(1).unwrap().props().clone();
            props.default_channel = 9;
            history
                .apply(
                    &mut d,
                    plumspirit_engine::history::Operation::TrackProps {
                        track,
                        props,
                        end: None,
                        label: "Fixture channel",
                    },
                )
                .unwrap();
        }
        let capture = serde_json::json!({"version":1,"count":4,"name":"Stored fixture","truncated":false,
            "status":[0x91,0x81,0xb2,0xe3],"data1":[64,64,1,0],"data2":[90,0,127,64],
            "time":[100,200,300,400],"driver_ms":[100,200,300,400],"sequence":[1,2,3,4],"received_ns":[0,1,2,3],
            "out_of_order":[false,false,false,false],"driver_overrun":[false,false,false,false]});
        d.project_mut().plugin_data = vec![(
            ID.into(),
            serde_json::json!({"version":1,"values":{"midi_input_capture":capture}}).to_string(),
        )];
        let mut h = Harness::new(d);
        h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
        h.app.settings.lua_plugins.enable_on_install = true;
        h.app_frame(egui::vec2(1366., 768.), 1.);
        let mut prefs = h.app.settings.lua_plugins.clone();
        h.app
            .lua_plugins()
            .install_example(crate::lua::examples::by_id(ID).unwrap(), &mut prefs)
            .unwrap();
        h.app.settings.lua_plugins = prefs;
        frames_until(&mut h, "MIDI Laboratory loads", |h| {
            h.app.custom_tool_menu().iter().any(|p| p.0 == ID)
                && !h.app.lua_plugins.as_ref().unwrap().is_busy()
        });
        (h, temp)
    }
    fn preview(h: &mut Harness) {
        frames_until(h, "MIDI Laboratory Preview", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some() || s.error.is_some())
        });
        assert!(
            h.app.custom_tool.as_ref().unwrap().error.is_none(),
            "{:?}",
            h.app.custom_tool.as_ref().unwrap().error
        );
    }
    #[test]
    fn native_midi_art_preview_cancel_apply_undo_redo_and_parameter_presets_keep_exact_large_ticks()
    {
        for extended in [false, true] {
            let (mut h, temp) = setup(extended, None);
            let before = h.doc().export_fingerprint();
            h.app.open_custom_tool(ID, "replay");
            let start = 9_007_199_254_741_001;
            h.app.custom_tool.as_mut().unwrap().values[1] = ToolValue::Int(start);
            if extended {
                h.app.custom_tool.as_mut().unwrap().values[3] = ToolValue::Int(128);
            }
            preview(&mut h);
            let expected = h
                .app
                .custom_tool
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .commits
                .result()
                .export_fingerprint();
            assert_eq!(
                h.app
                    .custom_tool
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .notes
                    .len(),
                9
            );
            assert_eq!(h.doc().export_fingerprint(), before);
            h.app_frame(egui::vec2(1100., 800.), 1.5);
            assert_eq!(
                h.app.custom_tool.as_ref().unwrap().values[1],
                ToolValue::Int(start)
            );
            h.app.custom_tool.as_mut().unwrap().preset_name = "Exact MIDI mapping".into();
            h.app
                .custom_tool_action(UiAction::SavePreset, Some(&temp.0), true);
            h.app.custom_tool_action(UiAction::Close, None, true);
            frames_until(&mut h, "Canceled MIDI art releases snapshot", |h| {
                !h.app.lua_plugins.as_ref().unwrap().is_busy()
            });
            assert_eq!(h.doc().export_fingerprint(), before);
            h.app.open_custom_tool(ID, "replay");
            h.app.custom_tool_action(
                UiAction::LoadPreset("Exact MIDI mapping".into()),
                Some(&temp.0),
                true,
            );
            assert_eq!(
                h.app.custom_tool.as_ref().unwrap().values[1],
                ToolValue::Int(start)
            );
            preview(&mut h);
            assert_eq!(
                h.app
                    .custom_tool
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .commits
                    .result()
                    .export_fingerprint(),
                expected
            );
            h.app.custom_tool_action(UiAction::Apply, None, true);
            frames_until(&mut h, "MIDI art applies", |h| {
                h.doc().export_fingerprint() == expected
            });
            assert!(h.command(Command::Undo));
            assert_eq!(h.doc().export_fingerprint(), before);
            assert!(h.command(Command::Redo));
            assert_eq!(h.doc().export_fingerprint(), expected);
            h.app.custom_tool_action(UiAction::Close, None, true);
            drop(h);
            drop(temp);
        }
    }
    #[test]
    fn captured_midi_replay_targets_linked_clip_local_content_and_opened_track_or_subtrack_defaults()
     {
        use crate::app::track_list::ClipIntent;
        use plumspirit_engine::model::ContentRef;
        for subtrack in [false, true] {
            for extended in [false, true] {
                let (mut h, temp) = setup(extended, Some(subtrack));
                h.clip(ClipIntent::Add {
                    track: 0,
                    start: 100,
                    length: 3840,
                });
                assert!(
                    !h.doc().clips().contents().is_empty(),
                    "Clip Add refused: {}",
                    h.status()
                );
                let content = h.doc().clips().contents()[0].id;
                let first = h.doc().clips().placements_of(ContentRef::Owned(content))[0].id;
                h.clip(ClipIntent::Duplicate {
                    copies: vec![(first, 1, 1920)],
                    linked: true,
                });
                let second = h
                    .doc()
                    .clips()
                    .placements_of(ContentRef::Owned(content))
                    .iter()
                    .find(|p| p.track == h.id(1))
                    .unwrap()
                    .id;
                h.clip(ClipIntent::Open(second));
                let before = h.doc().export_fingerprint();
                h.app.open_custom_tool(ID, "replay");
                h.app.custom_tool.as_mut().unwrap().values[1] = ToolValue::Int(37);
                h.app.custom_tool.as_mut().unwrap().values[10] = ToolValue::Bool(false);
                if extended {
                    h.app.custom_tool.as_mut().unwrap().values[3] = ToolValue::Int(128);
                }
                preview(&mut h);
                assert_eq!(h.doc().export_fingerprint(), before);
                let expected = h
                    .app
                    .custom_tool
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .commits
                    .result()
                    .export_fingerprint();
                h.app.custom_tool_action(UiAction::Apply, None, true);
                frames_until(&mut h, "MIDI art linked Clip apply", |h| {
                    h.doc().export_fingerprint() == expected
                });
                let notes = &h.doc().clips().content(content).unwrap().notes;
                assert_eq!(notes.len(), 1);
                let note = notes.iter().next().unwrap();
                assert_eq!(note.start_tick, 37);
                assert_eq!(note.attrs.channel(), 9);
                assert_eq!(note.attrs.pitch(), if extended { 192 } else { 64 });
                assert_eq!(h.doc().track(0).unwrap().notes.len(), 8);
                assert_eq!(h.doc().track(1).unwrap().notes.len(), 0);
                assert_eq!(
                    h.doc()
                        .clips()
                        .placements_of(ContentRef::Owned(content))
                        .len(),
                    2
                );
                assert!(h.command(Command::Undo));
                assert_eq!(h.doc().export_fingerprint(), before);
                assert!(h.command(Command::Redo));
                assert_eq!(h.doc().export_fingerprint(), expected);
                h.app.custom_tool_action(UiAction::Close, None, true);
                drop(h);
                drop(temp);
            }
        }
    }
}
