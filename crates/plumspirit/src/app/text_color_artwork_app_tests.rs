mod color_artwork {
    use super::*;
    use crate::app::lua_panels::font_picker_tests::{click, image_count, panel_status};
    use std::sync::Arc;
    fn idle(h: &mut Harness) {
        frames_until(h, "color artwork worker idle", |h| {
            h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
    }
    fn close_panel(h: &mut Harness, scale: f32) {
        click(h, "close", scale);
        frames_until(h, "color panel closes", |h| {
            h.app.lua_panels.is_empty() && h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
    }
    fn open_panel(h: &mut Harness) {
        h.app.run_lua_command(ID, "color-laboratory", true);
        frames_until(h, "color panel opens", |h| !h.app.lua_panels.is_empty());
    }
    fn preview(h: &mut Harness) {
        frames_until(h, "captured glyph preview", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some())
        });
    }
    #[test]
    fn actual_color_glyph_capture_preset_replacement_fences_old_preview_then_applies_one_undo_both_modes()
     {
        for (wide, scale) in [(false, 1.), (true, 1.5)] {
            let (mut h, temp) = setup(wide);
            idle(&mut h);
            h.app.settings.note_colors.mode = crate::ui::theme::NoteColorMode::Manual;
            h.app.settings.note_colors.manual = vec![[0, 0, 0]; 16];
            h.app.settings.note_colors.manual[7] = [255, 0, 0];
            h.app.settings.note_colors.manual[10] = [255, 255, 0];
            {
                let o = h.app.open.as_mut().unwrap();
                o.history
                    .apply(
                        Arc::get_mut(&mut o.doc).unwrap(),
                        plumspirit_engine::history::Operation::AddSubchannels {
                            track: 0,
                            subs: vec![
                                plumspirit_engine::model::Subchannel::new(
                                    plumspirit_engine::model::SubchannelId(7),
                                    9,
                                ),
                                plumspirit_engine::model::Subchannel::new(
                                    plumspirit_engine::model::SubchannelId(42),
                                    3,
                                ),
                            ],
                        },
                    )
                    .unwrap();
                o.history.reset(false);
            }
            let musical = h.doc().export_fingerprint();
            open_panel(&mut h);
            click(&mut h, "preview", scale);
            let wait = Instant::now();
            while image_count(&h) != 1 {
                h.app_frame(egui::vec2(1500., 1000.), scale);
                assert!(
                    wait.elapsed() < Duration::from_secs(10),
                    "Original glyph render failed: {}",
                    panel_status(&h)
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            click(&mut h, "capture", scale);
            click(&mut h, "save_pixels", scale);
            close_panel(&mut h, scale);
            frames_until(&mut h, "project receives fixed glyph pixels", |h| {
                !h.doc().project().plugin_data.is_empty()
            });
            let stored = &h
                .doc()
                .project()
                .plugin_data
                .iter()
                .find(|p| p.0 == ID)
                .unwrap()
                .1;
            let json: serde_json::Value = serde_json::from_str(stored).unwrap();
            let capture = &json["values"]["color_glyph_capture"];
            assert_eq!(capture["hex"].as_str().unwrap().len(), 64 * 64 * 8);
            assert_eq!(capture["source"]["text"], "😀");
            assert_eq!(h.doc().export_fingerprint(), musical);
            assert!(!h.app.enabled(Command::Undo));
            h.app.open_custom_tool(ID, "color-artwork");
            if wide {
                h.app.custom_tool.as_mut().unwrap().values[2] = ToolValue::Int(200);
            }
            preview(&mut h);
            let old = h.app.custom_tool.as_ref().unwrap().result.as_ref().unwrap();
            let old_revision = old.rev;
            let old_generation = h
                .app
                .custom_tool
                .as_ref()
                .unwrap()
                .requested
                .as_ref()
                .unwrap()
                .1;
            let old_signature = old.commits.result().export_fingerprint();
            assert!(old.notes.len() > 32);
            // A plain pixel preset is independent of the installed font and input preset.
            let file = temp
                .0
                .join("config/plugin-data")
                .join(ID)
                .join("presets/Color Glyph Laboratory Pixels.json");
            let mut preset: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            let hex = preset["value"]["hex"].as_str().unwrap();
            let mut replacement = String::with_capacity(hex.len());
            let rgb = crate::app::note_colors::palette_of(&h.app.settings)
                .entry(7, 100, true)
                .fill;
            let rgb = format!("{:02x}{:02x}{:02x}", rgb.r(), rgb.g(), rgb.b());
            for px in hex.as_bytes().chunks_exact(8) {
                replacement.push_str(&rgb);
                replacement.push_str(std::str::from_utf8(&px[6..]).unwrap());
            }
            preset["value"]["hex"] = serde_json::json!(replacement);
            std::fs::write(&file, serde_json::to_vec(&preset).unwrap()).unwrap();
            // Simulate another bounded native reader keeping the source alive. Receipt
            // must release the old kept preview and block Apply until data can land.
            let host = crate::lua::Host::new(
                crate::lua::Limits::default(),
                [
                    crate::lua::Capability::ReadProject,
                    crate::lua::Capability::EditNotes,
                ],
            )
            .unwrap();
            host.set_world(h.app.lua_world(true));
            host.run("late-preview-fixture.lua","local t=plum.transaction{};t:insert_notes(plum.project.current():track(1),{{start=13,gate=1,key=1,velocity=1}});t:commit()").unwrap();
            let late = host.take_commits().unwrap();
            host.set_world(None);
            let pinned = Arc::clone(&h.app.open.as_ref().unwrap().doc);
            open_panel(&mut h);
            click(&mut h, "load_pixels", scale);
            close_panel(&mut h, scale);
            assert!(!h.app.lua_project_data.is_empty());
            assert!(h.app.custom_tool.as_ref().unwrap().result.is_none());
            h.app
                .custom_tool_result(ID, old_generation, true, Ok(Some(late)));
            assert!(
                h.app.custom_tool.as_ref().unwrap().result.is_none(),
                "A late old-generation worker reply must be discarded"
            );
            h.app.custom_tool_action(UiAction::Apply, None, true);
            assert_eq!(h.doc().export_fingerprint(), musical);
            assert!(!h.app.enabled(Command::Undo));
            drop(pinned);
            preview(&mut h);
            assert!(h.app.lua_project_data.is_empty());
            let fresh = h.app.custom_tool.as_ref().unwrap().result.as_ref().unwrap();
            assert_ne!(fresh.rev, old_revision);
            let expected = fresh.commits.result().export_fingerprint();
            assert_ne!(expected, old_signature);
            assert!(
                fresh
                    .notes
                    .iter()
                    .filter(|n| n.vel == 100)
                    .all(|n| n.sub == 7)
            );
            h.app.custom_tool.as_mut().unwrap().preset_name = "Captured color sizing".into();
            h.app
                .custom_tool_action(UiAction::SavePreset, Some(&temp.0), true);
            h.app.custom_tool.as_mut().unwrap().values[0] = ToolValue::Int(3);
            h.app.custom_tool_action(
                UiAction::LoadPreset("Captured color sizing".into()),
                Some(&temp.0),
                true,
            );
            assert_eq!(
                h.app.custom_tool.as_ref().unwrap().values[0],
                ToolValue::Int(32)
            );
            preview(&mut h);
            h.app.custom_tool_action(UiAction::Apply, None, true);
            frames_until(&mut h, "native glyph applies", |h| {
                h.doc().export_fingerprint() == expected
            });
            assert_eq!(
                h.app.open.as_ref().unwrap().history.undo_label(),
                Some("Text Art Studio — Color Glyph Artwork")
            );
            assert!(h.command(Command::Undo));
            assert_eq!(h.doc().export_fingerprint(), musical);
            assert!(h.command(Command::Redo));
            assert_eq!(h.doc().export_fingerprint(), expected);
            let data = h.doc().project().plugin_data.clone();
            h.app.custom_tool_action(UiAction::Close, None, true);
            idle(&mut h);
            let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
            let (reopened, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
            assert_eq!(reopened.project().plugin_data, data);
            assert_eq!(reopened.export_fingerprint(), expected);
            drop(h);
            drop(temp);
        }
    }
}
