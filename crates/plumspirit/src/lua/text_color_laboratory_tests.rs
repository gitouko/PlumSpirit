use super::*;
use crate::lua::ui::schema::Primitive;
const ART_ID: &str = "plumspirit.example.text-art-studio";
const ART_MAIN: &str = include_str!("../../examples-lua/text-art-studio/main.lua");
const LAB: &str = include_str!("../../examples-lua/text-art-studio/color-laboratory.lua");
fn setup_color(t: &Temp, controlled: bool) -> (Host, Receiver<Arc<Shared>>) {
    setup_color_grants(t, controlled, false)
}
fn setup_color_grants(t: &Temp, controlled: bool, project: bool) -> (Host, Receiver<Arc<Shared>>) {
    std::fs::write(t.root.join("color-laboratory.lua"), LAB).unwrap();
    let mut grants = vec![Capability::ListFonts, Capability::UiPanels];
    if project {
        grants.push(Capability::ReadProject);
    }
    let h = Host::new(
        Limits {
            memory: 16 * 1024 * 1024,
            ..Default::default()
        },
        grants,
    )
    .unwrap();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: ART_ID.into(),
        name: "Text Art Studio".into(),
        version: "0.4.0".into(),
        root: t.root.clone(),
        data_dir: Some(t.root.join("data")),
        config_version: 1,
    })
    .unwrap();
    h.lua
        .set_app_data(NativeBudget::isolated(256 * 1024 * 1024));
    if controlled {
        std::fs::write(
            t.fonts().join("color.ttf"),
            crate::lua::text::fonts::color::tests::fixture(3, 0),
        )
        .unwrap();
        expose(&h, t.fonts());
        h.lua
            .globals()
            .get::<Table>("plum")
            .unwrap()
            .get::<Table>("text")
            .unwrap()
            .set(
                "installed_fonts",
                h.lua
                    .globals()
                    .get::<mlua::Function>("fixture_fonts")
                    .unwrap(),
            )
            .unwrap();
    }
    let (client, rx) = ui::channel(ART_ID);
    h.lua.app_data_mut::<ui::Slot>().unwrap().client = Some(client);
    let (client, _) = crate::lua::bridge::channel(ART_ID);
    h.lua.app_data_mut::<Session>().unwrap().client = Some(client);
    run(
        &h,
        &format!("{ART_MAIN}\nregister(plum.plugin);plum.plugin.__phase('ready')"),
    );
    if controlled {
        run(
            &h,
            "local c=plum.text.installed_fonts();local f=c:face(1);c:close();plum.config.set('color_glyph_inputs',{version=1,family=f.family,style=f.style,text='O',palette=0})",
        );
    }
    (h, rx)
}
fn image_rect(snapshot: &Snapshot) -> ([f64; 2], [f64; 2], Arc<crate::lua::ui::images::Resource>) {
    let Kind::Canvas { primitives, .. } = &node(&snapshot.root, "glyph_canvas").unwrap().kind
    else {
        panic!("Color canvas missing")
    };
    let images = primitives
        .iter()
        .filter_map(|p| {
            if let Primitive::Image {
                resource, min, max, ..
            } = p
            {
                Some((*min, *max, resource.clone()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        images.len(),
        1,
        "All native layers must share one canvas image"
    );
    images.into_iter().next().unwrap()
}
#[test]
fn authored_color_panel_preserves_native_layers_aspect_input_preset_and_failed_preview() {
    let t = Temp::new();
    let (h, rx) = setup_color(&t, true);
    let shots = drive_command(
        &h,
        rx,
        "color-laboratory",
        vec![
            click("preview"),
            click("save_preset"),
            Action {
                kind: "resize",
                id: "glyph_canvas",
                value: InputValue::None,
                wait: true,
            },
            text("text", "two"),
            after_text("preview"),
            click("load_preset"),
            click("save"),
            click("close"),
        ],
    );
    assert!(
        node(&shots[1].root, "status")
            .unwrap()
            .label
            .contains("3 layer glyphs")
    );
    let (_, _, first) = image_rect(&shots[1]);
    let (min, max, resized) = image_rect(&shots[3]);
    assert!(Arc::ptr_eq(&first, &resized));
    assert!(((max[0] - min[0]) * 1000. - (max[1] - min[1]) * 250.).abs() < 1e-9);
    assert!(
        node(&shots[5].root, "status")
            .unwrap()
            .label
            .contains("Previous preview retained")
    );
    let (_, _, failed) = image_rect(&shots[5]);
    assert!(Arc::ptr_eq(&first, &failed));
    drop(first);
    drop(resized);
    drop(failed);
    drop(shots);
    run(
        &h,
        "local a=plum.config.get('color_glyph_inputs');local p=plum.presets.load('Color Glyph Laboratory Inputs');assert(a.version==1 and a.text=='O'and p.text=='O'and a.family==p.family and a.style==p.style);for k in pairs(a)do assert(k=='version'or k=='family'or k=='style'or k=='text'or k=='palette')end",
    );
    clear(&h);
}
#[test]
fn reusable_authored_render_has_real_native_pixels_closes_fonts_and_keeps_exact_input() {
    let t = Temp::new();
    let (h, _) = setup_color(&t, true);
    h.lua
        .globals()
        .set(
            "inspect_color_image",
            h.lua
                .create_function(|lua, image: mlua::AnyUserData| {
                    image
                        .borrow::<crate::lua::images::LuaImage>()?
                        .with_pixels(lua, |rgba| {
                            assert_eq!((rgba.w, rgba.h), (64, 64));
                            let count = rgba.px.iter().filter(|p| p[3] != 0).count();
                            assert!(count > 100 && count < 4096);
                            assert_eq!(rgba.px[0], [0; 4]);
                            Ok(())
                        })?;
                    let fonts = lua
                        .app_data_ref::<crate::lua::text::fonts::FontSlot>()
                        .unwrap();
                    assert!(
                        fonts
                            .0
                            .iter()
                            .filter_map(|w| w.upgrade())
                            .all(|f| f.borrow().is_none())
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local m=plum.plugin.include('color-laboratory.lua');local p=plum.config.get('color_glyph_inputs');local budget={catalogs=0,images=0};local image,r=m.render(p,budget);assert(r.layer_count==3 and r.text=='O'and r.palette==0 and budget.images==7 and budget.catalogs==1);inspect_color_image(image);image:close();local bad={version=1,family=p.family,style=p.style,text='OO',palette=0};assert(not pcall(m.render,bad,budget)and bad.text=='OO'and budget.catalogs==1);p.text=utf8.char(0x10ffff);assert(not pcall(m.render,p,budget)and p.text==utf8.char(0x10ffff));p.text='O';p.palette=1;local other=m.render(p,budget);inspect_color_image(other);other:close()",
    );
    clear(&h);
}
#[cfg(windows)]
#[test]
fn actual_installed_windows_color_font_is_rendered_by_authored_panel_without_musical_commit() {
    let t = Temp::new();
    let (h, rx) = setup_color(&t, false);
    let shots = drive_command(
        &h,
        rx,
        "color-laboratory",
        vec![click("preview"), click("close")],
    );
    assert!(
        node(&shots[1].root, "rendered")
            .unwrap()
            .label
            .contains("Segoe UI Emoji")
    );
    assert!(
        node(&shots[1].root, "status")
            .unwrap()
            .label
            .contains("6 layer glyphs")
    );
    image_rect(&shots[1]);
    drop(shots);
    assert!(h.take_commits().is_none());
    clear(&h);
}

#[test]
fn authored_catalog_and_cumulative_image_budgets_refuse_before_replacing_output() {
    let t = Temp::new();
    let (h, _) = setup_color(&t, true);
    run(
        &h,
        "local m=plum.plugin.include('color-laboratory.lua');local p=plum.config.get('color_glyph_inputs');local b={catalogs=0,images=58};local ok,e=pcall(m.render,p,b);assert(not ok and tostring(e):find('Cumulative native image budget')and b.images==58 and b.catalogs==1);b.images=0;for i=1,3 do local image=m.render(p,b);image:close()end;assert(b.images==21 and b.catalogs==4);ok,e=pcall(m.render,p,b);assert(not ok and tostring(e):find('Four render/catalog attempts')and b.images==21 and b.catalogs==4)",
    );
    clear(&h);
}
#[test]
fn surface_cancellation_after_real_color_preview_keeps_saved_inputs_and_releases_resources() {
    let t = Temp::new();
    let (h, rx) = setup_color_grants(&t, true, true);
    let d = plumspirit_engine::model::PlumDocument::new_project(
        "Canceled fixed capture".into(),
        plumspirit_engine::model::Bpm::DEFAULT,
        960,
    );
    let mut history = plumspirit_engine::history::History::new(Default::default());
    history.observe_ids(&d);
    h.set_world(Some(crate::lua::handles::World::new(
        Arc::new(d),
        history.incarnations().snapshot(),
    )));
    let path = t.root.join("data/config.json");
    let before = std::fs::read(&path).unwrap();
    let driver = std::thread::spawn(move || {
        let shared = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let start = Instant::now();
        let initial = loop {
            if let Some(s) = shared.snapshot() {
                break s;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(shared.push(Event {
            kind: "activate",
            id: "preview".into(),
            value: InputValue::None,
            pointer: None,
            revision: initial.revision,
            sequence: 0
        }));
        let start = Instant::now();
        let rendered = loop {
            if let Some(s) = shared.snapshot()
                && s.revision > initial.revision
            {
                image_rect(&s);
                break s;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(node(&rendered.root, "capture").unwrap().enabled);
        assert!(shared.push(Event {
            kind: "activate",
            id: "capture".into(),
            value: InputValue::None,
            pointer: None,
            revision: rendered.revision,
            sequence: 0
        }));
        let start = Instant::now();
        loop {
            if let Some(s) = shared.snapshot()
                && s.revision > rendered.revision
            {
                assert!(
                    node(&s.root, "status")
                        .unwrap()
                        .label
                        .contains("staged for this project")
                );
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        shared.fail(ui::CANCELED);
    });
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    let result = h.run(
        "cancel-color-panel.lua",
        "plum.plugin.__run('color-laboratory',{})",
    );
    let joined = driver.join();
    assert_eq!(result.unwrap_err().category, crate::lua::Category::Canceled);
    joined.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(h.take_commits().is_none());
    assert!(
        h.take_plugin_out().project_data.is_none(),
        "Canceled capture must not publish project settings"
    );
    clear(&h);
}

#[test]
fn authored_color_capture_is_bounded_plain_exact_rgba_and_pixel_preset_is_font_independent() {
    let t = Temp::new();
    let (h, _) = setup_color(&t, true);
    run(
        &h,
        r#"local m=plum.plugin.include('color-laboratory.lua');local input=plum.config.get('color_glyph_inputs')
      local image,report=m.render(input,{catalogs=0,images=0});local capture=m.capture(image,report)
      local rgba=image:read{x=0,y=0,width=64,height=64};image:close()
      assert(#capture.hex==32768 and capture.source.text=='O' and capture.version==1)
      assert(capture.hex:gsub('..',function(pair)return string.char(tonumber(pair,16))end)==rgba)
      plum.presets.save('Color Glyph Laboratory Pixels',capture)
      local saved=m.validate_capture(plum.presets.load('Color Glyph Laboratory Pixels'));assert(saved.hex==capture.hex)
      saved.hex='00';assert(not pcall(m.validate_capture,saved));saved.hex=capture.hex;saved.source.text='OO'
      assert(not pcall(m.validate_capture,saved));capture.width=65;assert(not pcall(m.validate_capture,capture))"#,
    );
    clear(&h);
    run(
        &h,
        r#"local m=plum.plugin.include('color-laboratory.lua');local saved=m.validate_capture(plum.presets.load('Color Glyph Laboratory Pixels'))
      local bytes=saved.hex:gsub('..',function(pair)return string.char(tonumber(pair,16))end)
      local image=plum.images.from_rgba(64,64,bytes);assert(image:read{x=0,y=0,width=64,height=64}==bytes);image:close()"#,
    );
    clear(&h);
}
