mod image_art_studio {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const ID: &str = "plumspirit.example.image-art-studio";
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "plum-api02-image-art-app-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let p = self.0.canonicalize().unwrap();
            let base = std::env::temp_dir().canonicalize().unwrap();
            assert!(
                p.starts_with(&base)
                    && p != base
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("plum-api02-image-art-app-")
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
        h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
        h.app.settings.lua_plugins.enable_on_install = true;
        h.app_frame(egui::vec2(1366., 768.), 1.);
        let mut prefs = h.app.settings.lua_plugins.clone();
        h.app
            .lua_plugins()
            .install_example(crate::lua::examples::by_id(ID).unwrap(), &mut prefs)
            .unwrap();
        h.app.settings.lua_plugins = prefs;
        frames_until(&mut h, "image studio loads", |h| {
            h.app.custom_tool_menu().iter().any(|p| p.0 == ID)
        });
        (h, temp)
    }
    #[test]
    fn actual_native_artwork_form_preview_apply_cancel_and_parameter_preset_both_key_modes() {
        for extended in [false, true] {
            let (mut h, temp) = setup(extended);
            let original = h.doc().export_fingerprint();
            h.app.open_custom_tool(ID, "artwork");
            if extended {
                h.app.custom_tool.as_mut().unwrap().values[1] = ToolValue::Int(200);
            }
            frames_until(&mut h, "image artwork preview", |h| {
                h.app
                    .custom_tool
                    .as_ref()
                    .is_some_and(|s| s.result.is_some())
            });
            let (expected, count) = {
                let s = h.app.custom_tool.as_ref().unwrap().result.as_ref().unwrap();
                assert!(s.notes.len() > 8);
                (s.commits.result().export_fingerprint(), s.notes.len())
            };
            assert_eq!(h.doc().export_fingerprint(), original);
            h.app_frame(egui::vec2(900., 680.), 1.5);
            assert_eq!(h.doc().export_fingerprint(), original);
            h.app.custom_tool.as_mut().unwrap().preset_name = "Studio dimensions".into();
            h.app
                .custom_tool_action(UiAction::SavePreset, Some(&temp.0), true);
            assert!(
                h.app
                    .custom_tool
                    .as_ref()
                    .unwrap()
                    .presets
                    .iter()
                    .any(|p| p == "Studio dimensions")
            );
            h.app.custom_tool_action(UiAction::Apply, None, true);
            frames_until(&mut h, "image artwork applies", |h| {
                h.doc().export_fingerprint() == expected
            });
            assert_eq!(h.doc().note_count(), count);
            assert!(h.command(Command::Undo));
            assert_eq!(h.doc().export_fingerprint(), original);
            assert!(h.command(Command::Redo));
            assert_eq!(h.doc().export_fingerprint(), expected);
            h.app.custom_tool_action(UiAction::Close, None, true);
            h.app.open_custom_tool(ID, "artwork");
            h.app.custom_tool.as_mut().unwrap().values[2] = ToolValue::Int(8);
            h.app.custom_tool_action(
                UiAction::LoadPreset("Studio dimensions".into()),
                Some(&temp.0),
                true,
            );
            assert_eq!(
                h.app.custom_tool.as_ref().unwrap().values[2],
                ToolValue::Int(32)
            );
            h.app.custom_tool_action(UiAction::Close, None, true);
            for _ in 0..3 {
                h.app_frame(egui::vec2(1366., 768.), 1.);
            }
            assert_eq!(h.doc().export_fingerprint(), expected);
            drop(h);
            drop(temp);
        }
    }
    #[test]
    fn actual_native_mask_preview_kept_commits_apply_and_undo_without_live_mutation() {
        let (mut h, temp) = setup(false);
        let original = h.doc().export_fingerprint();
        h.app.open_custom_tool(ID, "mask");
        h.app.custom_tool.as_mut().unwrap().values[1] = ToolValue::Int(60);
        h.app.custom_tool.as_mut().unwrap().values[3] = ToolValue::Int(1);
        frames_until(&mut h, "image mask native preview", |h| {
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
        h.app.custom_tool_action(UiAction::Apply, None, true);
        frames_until(&mut h, "image mask applies", |h| {
            h.doc().export_fingerprint() == expected
        });
        assert!(
            h.doc()
                .track(0)
                .unwrap()
                .notes
                .iter()
                .all(|n| n.attrs.velocity() == 100)
        );
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().export_fingerprint(), original);
        h.app.custom_tool_action(UiAction::Close, None, true);
        drop(h);
        drop(temp);
    }
}
