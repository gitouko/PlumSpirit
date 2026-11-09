mod text_art_studio {
    use super::*;
    const ID: &str = "plumspirit.example.text-art-studio";
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "plum-text-art-app-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let p = self.0.canonicalize().unwrap();
            let base = std::env::temp_dir().canonicalize().unwrap();
            assert!(
                p.parent() == Some(base.as_path())
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("plum-text-art-app-"))
            );
            std::fs::remove_dir_all(p).unwrap();
        }
    }
    fn setup(extended: bool) -> (Harness, Temp) {
        let temp = Temp::new();
        let mut d = doc();
        if extended {
            d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
            d.track_mut(0)
                .unwrap()
                .notes
                .map_selected_attrs(|a| a.with_pitch_wide(200).unwrap());
        }
        let mut h = Harness::new(d);
        h.app.config_dir = Some(temp.0.join("config"));
        h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
        h.app.settings.lua_plugins.enable_on_install = true;
        h.app_frame(egui::vec2(1366., 768.), 1.);
        let mut prefs = h.app.settings.lua_plugins.clone();
        h.app
            .lua_plugins()
            .install_example(crate::lua::examples::by_id(ID).unwrap(), &mut prefs)
            .unwrap();
        h.app.settings.lua_plugins = prefs;
        frames_until(&mut h, "Text Art Studio loads", |h| {
            h.app.custom_tool_menu().iter().any(|p| p.0 == ID)
        });
        (h, temp)
    }
    #[test]
    fn opt_in_native_multiline_editor_accepts_enter_and_legacy_text_stays_singleline() {
        fn edit(multiline: bool) -> (String, f32) {
            let ctx = egui::Context::default();
            let mut value = ToolValue::Text("ab".into());
            let default = value.clone();
            let kind = ToolParamKind::Text {
                max_len: 128,
                multiline,
            };
            let mut height = 0.;
            for pass in 0..2 {
                let raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(500., 300.),
                    )),
                    events: if pass == 1 {
                        vec![egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                };
                let mut output = ctx.run_ui(raw, |ui| {
                    let id = if multiline {
                        ui.id().with(("lua_multiline", 0usize))
                    } else {
                        ui.next_auto_id()
                    };
                    if pass == 0 {
                        ui.memory_mut(|m| m.request_focus(id));
                    }
                    let top = ui.next_widget_position().y;
                    param_control(ui, 0, &kind, &mut value, &default, 127);
                    height = ui.next_widget_position().y - top;
                });
                output.textures_delta.clear();
            }
            let ToolValue::Text(text) = value else {
                panic!("text parameter changed type")
            };
            (text, height)
        }
        let (old, one) = edit(false);
        let (new, multi) = edit(true);
        assert_eq!(old, "ab");
        assert!(new.contains('\n'));
        assert_eq!(new.replace('\n', ""), "ab");
        assert!(multi > one * 2.);
    }
    fn exercise_composition(extended: bool, text: &str, font: usize) {
        let (mut h, temp) = setup(extended);
        let original = h.doc().export_fingerprint();
        h.app.open_custom_tool(ID, "composition");
        h.app.custom_tool.as_mut().unwrap().values[0] = ToolValue::Text(text.into());
        h.app.custom_tool.as_mut().unwrap().values[3] = ToolValue::Choice(1);
        h.app.custom_tool.as_mut().unwrap().values[1] = ToolValue::Choice(font);
        frames_until(&mut h, "multiline text preview", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some())
        });
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
        assert_ne!(expected, original);
        assert_eq!(h.doc().export_fingerprint(), original);
        h.app_frame(egui::vec2(820., 620.), 1.5);
        assert_eq!(h.doc().export_fingerprint(), original);
        h.app.custom_tool.as_mut().unwrap().preset_name = "Two lines".into();
        h.app
            .custom_tool_action(UiAction::SavePreset, Some(&temp.0), true);
        assert!(
            h.app
                .custom_tool
                .as_ref()
                .unwrap()
                .presets
                .iter()
                .any(|s| s == "Two lines")
        );
        h.app.custom_tool_action(UiAction::Apply, None, true);
        frames_until(&mut h, "text art applies", |h| {
            h.doc().export_fingerprint() == expected
        });
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().export_fingerprint(), original);
        assert!(h.command(Command::Redo));
        assert_eq!(h.doc().export_fingerprint(), expected);
        h.app.custom_tool_action(UiAction::Close, None, true);
        h.app.open_custom_tool(ID, "composition");
        h.app.custom_tool.as_mut().unwrap().values[0] = ToolValue::Text("changed".into());
        h.app.custom_tool_action(
            UiAction::LoadPreset("Two lines".into()),
            Some(&temp.0),
            true,
        );
        assert_eq!(
            h.app.custom_tool.as_ref().unwrap().values[0],
            ToolValue::Text(text.into())
        );
        assert_eq!(
            h.app.custom_tool.as_ref().unwrap().values[3],
            ToolValue::Choice(1)
        );
        assert_eq!(
            h.app.custom_tool.as_ref().unwrap().values[1],
            ToolValue::Choice(font)
        );
        h.app.custom_tool_action(UiAction::Close, None, true);
        frames_until(&mut h, "text tool cancellation settles", |h| {
            h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
        assert_eq!(h.doc().export_fingerprint(), expected);
        drop(h);
        drop(temp);
    }
    #[test]
    fn native_multiline_art_form_keeps_preview_private_applies_one_undo_and_restores_parameter_preset()
     {
        for extended in [false, true] {
            exercise_composition(extended, "O\r\nO", 0);
        }
    }
    #[test]
    fn native_mixed_font_choices_preview_apply_cancel_undo_and_restore_plain_parameter_presets() {
        for extended in [false, true] {
            for font in [3, 4] {
                exercise_composition(extended, "A😀A\r\nπ🔥", font);
            }
        }
    }
    include!("text_color_artwork_app_tests.rs");
}
