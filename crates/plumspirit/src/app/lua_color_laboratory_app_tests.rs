use super::*;
const ART_ID: &str = "plumspirit.example.text-art-studio";
fn setup_color(wide: bool) -> (Harness, Temp) {
    let temp = Temp::new();
    let mut doc =
        PlumDocument::new_project("Read-only original color glyph".into(), Bpm::DEFAULT, 960);
    if wide {
        doc.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let mut h = Harness::new(doc);
    h.app.config_dir = Some(temp.0.join("config"));
    h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
    h.app.settings.lua_plugins.enable_on_install = true;
    h.app_frame(egui::vec2(1500., 1000.), 1.);
    let mut prefs = h.app.settings.lua_plugins.clone();
    h.app
        .lua_plugins()
        .install_example(crate::lua::examples::by_id(ART_ID).unwrap(), &mut prefs)
        .unwrap();
    h.app.settings.lua_plugins = prefs;
    frames(&mut h, |h| {
        h.app.lua_plugins.as_ref().is_some_and(|m| {
            !m.is_busy() && m.plugin(ART_ID).is_some_and(|p| p.status == Status::Ready)
        })
    });
    (h, temp)
}
#[test]
fn installed_color_glyph_native_panel_upload_save_close_and_dpi_keep_musical_source_unchanged() {
    for (wide, scale) in [(false, 1.), (true, 1.5)] {
        let (mut h, temp) = setup_color(wide);
        let original = h.doc().state_fingerprint();
        assert!(!h.app.enabled(Command::Undo));
        h.app.run_lua_command(ART_ID, "color-laboratory", true);
        frames(&mut h, |h| !h.app.lua_panels.is_empty());
        assert_eq!(
            h.app.lua_panels[0].snapshot.title,
            "Text Art Studio — Color Glyph Laboratory"
        );
        click(&mut h, "preview", scale);
        frames(&mut h, |h| {
            h.app
                .lua_panels
                .first()
                .is_some_and(|p| p.images.len() == 1)
        });
        assert_eq!(h.doc().state_fingerprint(), original);
        assert!(!h.app.enabled(Command::Undo));
        click(&mut h, "save", scale);
        click(&mut h, "close", scale);
        frames(&mut h, |h| {
            h.app.lua_panels.is_empty() && h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
        let file = temp
            .0
            .join("config/plugin-data")
            .join(ART_ID)
            .join("config.json");
        let bytes = std::fs::read(&file).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let value = &json["values"]["color_glyph_inputs"];
        assert_eq!(value["family"], "Segoe UI Emoji");
        assert_eq!(value["text"], "😀");
        assert_eq!(value["palette"], 0);
        assert_eq!(value["version"], 1);
        h.app.run_lua_command(ART_ID, "color-laboratory", true);
        frames(&mut h, |h| !h.app.lua_panels.is_empty());
        click(&mut h, "close", scale);
        frames(&mut h, |h| {
            h.app.lua_panels.is_empty() && h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert_eq!(h.doc().state_fingerprint(), original);
        assert!(!h.app.enabled(Command::Undo));
        drop(h);
        drop(temp);
    }
}
