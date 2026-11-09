use crate::app::{
    lua_views::tests::{ID, Rig},
    track_list::MainView,
};
use std::time::{Duration, Instant};
fn marker(shape: &egui::Shape) -> bool {
    match shape {
        egui::Shape::Path(p) => {
            p.stroke.color
                == egui::epaint::ColorMode::Solid(egui::Color32::from_rgba_unmultiplied(
                    31, 117, 233, 127,
                ))
        }
        egui::Shape::Vec(v) => v.iter().any(marker),
        _ => false,
    }
}
fn run(r: &mut Rig, command: &str) {
    run_plugin(r, ID, command)
}
fn run_plugin(r: &mut Rig, id: &str, command: &str) {
    std::thread::sleep(Duration::from_millis(55)); // real native20Hz admission, not a mocked clock
    let world = r.h.app.lua_world(true).unwrap();
    r.h.app
        .lua_plugins
        .as_mut()
        .unwrap()
        .run_command(id, command, Some(world))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        r.h.app.poll_lua_plugins(&r.h.ctx);
        let p = r.h.app.lua_plugins.as_ref().unwrap().plugin(id).unwrap();
        if !p.is_busy() {
            assert!(
                !p.log
                    .lines()
                    .any(|l| l.level == crate::lua::plugins::Level::Error),
                "{:?}",
                p.log.lines().map(|l| l.text.clone()).collect::<Vec<_>>()
            );
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn frame(r: &mut Rig, width: f32, ppp: f32) -> Vec<egui::epaint::ClippedShape> {
    r.h.app_frame(egui::vec2(width, 800.), ppp)
}
#[test]
fn real_roll_overlay_is_clipped_after_note_meshes_keeps_colors_and_drops_after_zoom_resize_or_revoke()
 {
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    let bytes = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    let stamp = r.h.app.lua_observation_stamp();
    let baseline = frame(&mut r, 1280., 1.);
    let rect = r.h.app.roll.notes_rect().unwrap();
    let meshes = |shapes: &[egui::epaint::ClippedShape]| {
        shapes
            .iter()
            .filter(|s| s.clip_rect == rect && matches!(s.shape, egui::Shape::Mesh(_)))
            .map(|s| format!("{:?}", s.shape))
            .collect::<Vec<_>>()
    };
    let before = meshes(&baseline);
    assert!(!before.is_empty());
    run(&mut r, "overlay_piano_roll");
    let shapes = frame(&mut r, 1280., 1.);
    assert_eq!(r.h.app.lua_overlays.views.len(), 1);
    assert_eq!(
        meshes(&shapes),
        before,
        "native note meshes/colors unchanged"
    );
    let at = shapes
        .iter()
        .position(|s| marker(&s.shape))
        .expect("actual Lua-authored native overlay shape");
    assert_eq!(shapes[at].clip_rect, rect);
    assert!(
        shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.clip_rect == rect && matches!(s.shape, egui::Shape::Mesh(_)))
            .all(|(i, _)| i < at)
    );
    let egui::Shape::Vec(list) = &shapes[at].shape else {
        panic!("reserved overlay slot")
    };
    let egui::Shape::Path(line) = &list[0] else {
        panic!("authored line")
    };
    assert_eq!(
        line.points,
        vec![
            egui::pos2(rect.left() + rect.width() * 0.25, rect.center().y),
            egui::pos2(rect.left() + rect.width() * 0.75, rect.center().y)
        ]
    );
    assert_eq!(line.stroke.width, 3.);
    r.h.app.roll.view.time.zoom_around(100., 1.);
    let shapes = frame(&mut r, 1280., 1.);
    assert!(!shapes.iter().any(|s| marker(&s.shape)));
    assert!(r.h.app.lua_overlays.views.is_empty());
    run(&mut r, "overlay_piano_roll");
    assert!(frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    assert!(!frame(&mut r, 900., 1.5).iter().any(|s| marker(&s.shape)));
    assert!(r.h.app.lua_overlays.views.is_empty());
    run(&mut r, "overlay_piano_roll");
    assert!(frame(&mut r, 900., 1.5).iter().any(|s| marker(&s.shape)));
    r.h.app.lua_plugins.as_mut().unwrap().set_granted(
        ID,
        "ui.overlay",
        false,
        &mut r.h.app.settings.lua_plugins,
    );
    assert!(!frame(&mut r, 900., 1.5).iter().any(|s| marker(&s.shape)));
    assert!(r.h.app.lua_overlays.views.is_empty());
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        bytes
    );
    assert_eq!(r.h.app.lua_observation_stamp(), stamp);
    assert_eq!(r.h.doc().note_count(), 1000);
}
#[test]
fn native_individual_disable_clear_hidden_surface_and_player_policy_preserve_the_project() {
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    let bytes = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    run(&mut r, "overlay_piano_roll");
    assert!(frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    r.h.app
        .lua_overlays
        .disabled
        .insert((ID.into(), 0, "guide".into()));
    assert!(!frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    assert_eq!(r.h.app.lua_overlays.views.len(), 1);
    r.h.app.lua_overlays.disabled.clear();
    assert!(frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    run(&mut r, "overlay_clear");
    assert!(!frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    assert!(r.h.app.lua_overlays.views.is_empty());
    run(&mut r, "overlay_piano_roll");
    r.h.app.main_view = MainView::TrackList;
    assert!(!frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    assert!(r.h.app.lua_overlays.views.is_empty());
    r.h.app.settings.player.enabled = true;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    run(&mut r, "overlay_player");
    let shapes = frame(&mut r, 1280., 1.);
    assert!(shapes.iter().any(|s| marker(&s.shape)));
    assert_eq!(r.h.app.lua_overlays.views[0].frame.view.facts.index(), 1);
    r.h.app.settings.player.enabled = false;
    r.h.app.main_view = MainView::TrackList;
    assert!(!frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
    assert!(r.h.app.lua_overlays.views.is_empty());
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        bytes
    );
}

#[test]
fn overlay_does_not_capture_native_note_selection_in_either_key_mode() {
    for extended in [false, true] {
        let mut r = Rig::new_with_keys(extended);
        r.h.app.main_view = MainView::PianoRoll;
        r.h.app.roll.tool = crate::piano_roll::Tool::Select;
        r.h.app.roll.view.pitch.scroll_rows = if extended { 180. } else { 52. };
        r.h.app.roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.3);
        for _ in 0..3 {
            r.frame(1280., 1.);
        }
        let before = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
        run(&mut r, "overlay_piano_roll");
        assert!(frame(&mut r, 1280., 1.).iter().any(|s| marker(&s.shape)));
        let crate::lua::visual::Facts::Roll(view) = r.h.app.lua_view_snapshots()[0].unwrap().facts
        else {
            panic!("roll")
        };
        assert_eq!(
            u16::from(view.view.pitch.top) + 1,
            if extended { 256 } else { 128 }
        );
        let pos = egui::pos2(
            view.rect.left() + 3.,
            view.rect.top()
                + view.view.pitch.pitch_top_y(60) as f32
                + view.view.pitch.row_height as f32 * 0.5,
        );
        assert!(view.rect.contains(pos));
        for pressed in [true, false] {
            r.h.app_frame_events(
                egui::vec2(1280., 800.),
                1.,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(
            r.h.doc().track(0).unwrap().notes.selected_count(),
            1,
            "native selection receives input through overlay"
        );
        assert_eq!(
            plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
            before
        );
    }
}
#[test]
fn installed_overlay_laboratory_executes_actual_checker_refresh_clear_and_player_commands() {
    const EX: &str = "plumspirit.example.editor-overlay-laboratory";
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    let before = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    let m = r.h.app.lua_plugins.as_mut().unwrap();
    let prefs = &mut r.h.app.settings.lua_plugins;
    m.install_example(crate::lua::examples::by_id(EX).unwrap(), prefs)
        .unwrap();
    assert!(prefs.installed[EX].granted.is_empty());
    for c in ["ui.overlay", "project.read", "playback.read"] {
        m.set_granted(EX, c, true, prefs);
    }
    m.set_enabled(EX, true, prefs);
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        r.h.app.poll_lua_plugins(&r.h.ctx);
        if r.h
            .app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(EX)
            .unwrap()
            .status
            == crate::lua::plugins::Status::Ready
        {
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    run_plugin(&mut r, EX, "checker");
    frame(&mut r, 1280., 1.);
    let v =
        r.h.app
            .lua_overlays
            .views
            .iter()
            .find(|v| v.plugin == EX)
            .unwrap();
    let crate::lua::ui::schema::Primitive::Image { resource, .. } =
        &v.frame.primitives.as_ref().unwrap().items[0]
    else {
        panic!("image guide")
    };
    let resource = std::sync::Arc::clone(resource);
    assert!(resource.alive());
    assert_eq!(resource.test_uploads(), 1);
    assert_eq!(v.textures.len(), 1);
    r.h.app.roll.view.time.zoom_around(100., 1.);
    frame(&mut r, 1280., 1.);
    assert!(r.h.app.lua_overlays.views.is_empty());
    run_plugin(&mut r, EX, "refresh");
    frame(&mut r, 1280., 1.);
    assert_eq!(
        resource.test_uploads(),
        1,
        "retained image reuses its native texture after stale view replacement"
    );
    run_plugin(&mut r, EX, "clear_piano_roll");
    frame(&mut r, 1280., 1.);
    assert!(r.h.app.lua_overlays.views.is_empty());
    r.h.app.settings.player.enabled = true;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    run_plugin(&mut r, EX, "player");
    frame(&mut r, 1280., 1.);
    assert_eq!(r.h.app.lua_overlays.views[0].frame.view.facts.index(), 1);
    run_plugin(&mut r, EX, "clear_player");
    frame(&mut r, 1280., 1.);
    assert!(r.h.app.lua_overlays.views.is_empty());
    r.h.app
        .lua_plugins
        .as_mut()
        .unwrap()
        .set_enabled(EX, false, &mut r.h.app.settings.lua_plugins);
    let end = Instant::now() + Duration::from_secs(5);
    while resource.alive() {
        r.h.app.poll_lua_plugins(&r.h.ctx);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        before
    );
}

#[test]
fn native_image_upload_allowance_is_shared_across_surfaces_panels_and_layout_passes() {
    let mut r = Rig::new();
    let mut passes = 0;
    let mut frame_id = None;
    let ctx = r.h.ctx.clone();
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        passes += 1;
        r.h.app.reset_lua_upload_budget(ui.ctx());
        if passes == 1 {
            assert_eq!(
                r.h.app.lua_upload_pixels,
                crate::lua::ui::images::MAX_UPLOAD_PIXELS
            );
            frame_id = r.h.app.lua_upload_frame;
            r.h.app.lua_upload_pixels = 7;
            ui.ctx()
                .request_discard("verify one upload allowance across native layout passes");
        } else {
            assert_eq!(r.h.app.lua_upload_frame, frame_id);
            assert_eq!(r.h.app.lua_upload_pixels, 7);
        }
        r.h.app.lua_panel_windows(ui.ctx());
        assert_eq!(r.h.app.lua_upload_pixels, 7);
    });
    drop(output);
    assert!(passes >= 2, "exercise actual egui multipass layout");
    let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
        r.h.app.reset_lua_upload_budget(ui.ctx());
        assert_ne!(r.h.app.lua_upload_frame, frame_id);
        assert_eq!(
            r.h.app.lua_upload_pixels,
            crate::lua::ui::images::MAX_UPLOAD_PIXELS
        );
    });
}

#[test]
fn native_global_eight_frame_limit_refuses_whole_batch_without_partial_replacement() {
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    let ids = [
        "test.overlay.first",
        "test.overlay.second",
        "test.overlay.third",
    ];
    for (i, id) in ids.iter().enumerate() {
        let source = r.root.join(format!("capacity-{i}"));
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("plum-plugin.toml"),format!("id=\"{id}\"\nname=\"Capacity fixture\"\nversion=\"1.0\"\napi=\"0.2\"\ncapabilities=[\"ui.overlay\",\"project.read\"]\n")).unwrap();
        std::fs::write(source.join("main.lua"),"plum.require_api(0,2);for _,n in ipairs({1,3,4})do plum.plugin.command{id='count'..n,name='Count '..n,run=function()local view=assert(plum.ui.view_snapshot('piano_roll'));for i=1,n do plum.ui.publish_overlay(view,{id='layer'..i,primitives={{kind='text',x=0,y=i/10,text='capacity'}}})end end}end").unwrap();
        let m = r.h.app.lua_plugins.as_mut().unwrap();
        let prefs = &mut r.h.app.settings.lua_plugins;
        m.install(&source, false, prefs).unwrap();
        for c in ["ui.overlay", "project.read"] {
            m.set_granted(id, c, true, prefs);
        }
        m.set_enabled(id, true, prefs);
    }
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        r.h.app.poll_lua_plugins(&r.h.ctx);
        if ids.iter().all(|id| {
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(id)
                .unwrap()
                .status
                == crate::lua::plugins::Status::Ready
        }) {
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    for (i, count) in [4, 3, 1].into_iter().enumerate() {
        run_plugin(&mut r, ids[i], &format!("count{count}"));
        frame(&mut r, 1280., 1.);
    }
    assert_eq!(r.h.app.lua_overlays.views.len(), 8);
    let original = std::sync::Arc::clone(
        &r.h.app
            .lua_overlays
            .views
            .iter()
            .find(|v| v.plugin == ids[2])
            .unwrap()
            .frame,
    );
    run_plugin(&mut r, ids[2], "count4");
    frame(&mut r, 1280., 1.);
    assert_eq!(r.h.app.lua_overlays.views.len(), 8);
    let third =
        r.h.app
            .lua_overlays
            .views
            .iter()
            .filter(|v| v.plugin == ids[2])
            .collect::<Vec<_>>();
    assert_eq!(third.len(), 1);
    assert!(
        std::sync::Arc::ptr_eq(&third[0].frame, &original),
        "whole refused batch leaves prior frame intact"
    );
}

fn install_source(r: &mut Rig, id: &str, caps: &[&str], source: &str) {
    let dir = r.root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let names = caps
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    std::fs::write(dir.join("plum-plugin.toml"),format!("id=\"{id}\"\nname=\"Surface observer fixture\"\nversion=\"0.2\"\napi=\"0.2\"\ncapabilities=[{names}]\n")).unwrap();
    std::fs::write(dir.join("main.lua"), source).unwrap();
    let m = r.h.app.lua_plugins.as_mut().unwrap();
    let prefs = &mut r.h.app.settings.lua_plugins;
    m.install(&dir, false, prefs).unwrap();
    for c in caps {
        m.set_granted(id, c, true, prefs);
    }
    m.set_enabled(id, true, prefs);
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        r.h.app.poll_lua_plugins(&r.h.ctx);
        if r.h
            .app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(id)
            .unwrap()
            .status
            == crate::lua::plugins::Status::Ready
        {
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn frame_until(r: &mut Rig, what: &str, mut condition: impl FnMut(&Rig) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        frame(r, 1280., 1.);
        if condition(r) {
            break;
        }
        assert!(Instant::now() < end, "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn player_only_surface_observer_receives_authorized_root_free_geometry_and_transport() {
    const OBS: &str = "test.overlay.player-observer";
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    let before = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    install_source(
        &mut r,
        OBS,
        &["ui.overlay", "playback.read"],
        r#"
      plum.require_api(0,2)
      plum.plugin.observe{id='surface',events={'surface_changed'},debounce_ms=20,run=function(e)
        assert(e.data.surface=='player' and e.read_only and not e.transaction_allowed)
        assert(not plum.has('project.read'))
        local view=plum.ui.view_snapshot('player')
        if e.data.visible then
          assert(view and view:info().view_revision==e.data.view_revision)
          local s=assert(plum.playback.snapshot());assert(s.project==e.project)
          plum.ui.publish_overlay(view,{id='observed',primitives={{kind='text',x=.1,y=.2,text='Observed Player '..s.state}}})
          print('PLAYER_SURFACE_VISIBLE')
        else assert(view==nil);print('PLAYER_SURFACE_HIDDEN')end
      end}
    "#,
    );
    frame_until(&mut r, "authorized hidden Player notice", |r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(OBS)
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("PLAYER_SURFACE_HIDDEN"))
    });
    r.h.app.settings.player.enabled = true;
    frame_until(&mut r, "Player-only actual native observed overlay", |r| {
        r.h.app.lua_overlays.views.iter().any(|v| v.plugin == OBS)
    });
    let v =
        r.h.app
            .lua_overlays
            .views
            .iter()
            .find(|v| v.plugin == OBS)
            .unwrap();
    assert_eq!(v.frame.view.facts.index(), 1);
    r.h.app.settings.player.enabled = false;
    r.h.app.main_view = MainView::TrackList;
    frame(&mut r, 1280., 1.);
    assert!(r.h.app.lua_overlays.views.is_empty());
    let p = r.h.app.lua_plugins.as_ref().unwrap().plugin(OBS).unwrap();
    assert!(
        !p.log
            .lines()
            .any(|l| l.level == crate::lua::plugins::Level::Error),
        "{:?}",
        p.log.lines().map(|l| l.text.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        before
    );
}
#[test]
fn native_view_change_cancels_running_geometry_without_pausing_later_surface_delivery() {
    const OBS: &str = "test.overlay.stale-observer";
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    install_source(
        &mut r,
        OBS,
        &["ui.overlay", "project.read"],
        r#"
      plum.require_api(0,2);local first=true
      plum.plugin.observe{id='surface',events={'surface_changed'},debounce_ms=20,run=function(e)
        if not e.data.visible then return end
        local view=assert(plum.ui.view_snapshot('piano_roll'))
        if first then first=false;plum.progress(.25,'captured stale surface');while true do plum.yield()end end
        assert(plum.project.current()==nil)
        plum.ui.publish_overlay(view,{id='current',primitives={{kind='text',x=.1,y=.2,text='Current '..e.data.view_revision}}})
        print('CURRENT_SURFACE')
      end}
    "#,
    );
    frame_until(&mut r, "first callback reports captured geometry", |r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(OBS)
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.25))
    });
    let old = r.h.app.lua_view_snapshots()[0].unwrap();
    r.h.app.roll.view.time.zoom_around(100., 1.);
    frame_until(
        &mut r,
        "new callback publishes after native stale cancellation",
        |r| r.h.app.lua_overlays.views.iter().any(|v| v.plugin == OBS),
    );
    let current = r.h.app.lua_view_snapshots()[0].unwrap();
    assert!(current.revision > old.revision);
    let v =
        r.h.app
            .lua_overlays
            .views
            .iter()
            .find(|v| v.plugin == OBS)
            .unwrap();
    assert_eq!(v.frame.view, current);
    assert!(
        !r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(OBS)
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("paused")),
        "stale cancellation is not observer failure"
    );
}

#[test]
fn official_authored_automatic_refresh_reuses_guide_after_zoom_in_both_key_modes_and_player() {
    const EX: &str = "plumspirit.example.editor-overlay-laboratory";
    for extended in [false, true] {
        let mut r = Rig::new_with_keys(extended);
        r.h.app.main_view = MainView::PianoRoll;
        for _ in 0..3 {
            r.frame(1280., 1.);
        }
        let before = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
        let m = r.h.app.lua_plugins.as_mut().unwrap();
        let prefs = &mut r.h.app.settings.lua_plugins;
        m.install_example(crate::lua::examples::by_id(EX).unwrap(), prefs)
            .unwrap();
        for cap in ["ui.overlay", "project.read", "playback.read"] {
            m.set_granted(EX, cap, true, prefs);
        }
        m.set_enabled(EX, true, prefs);
        frame_until(&mut r, "official example ready", |r| {
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(EX)
                .unwrap()
                .status
                == crate::lua::plugins::Status::Ready
        });
        run_plugin(&mut r, EX, "checker");
        frame(&mut r, 1280., 1.);
        run_plugin(&mut r, EX, "auto_roll");
        frame(&mut r, 1280., 1.);
        let initial = r.h.app.lua_view_snapshots()[0].unwrap();
        let image =
            r.h.app
                .lua_overlays
                .views
                .iter()
                .find(|v| v.plugin == EX)
                .and_then(|v| {
                    v.frame
                        .primitives
                        .as_ref()
                        .unwrap()
                        .items
                        .iter()
                        .find_map(|p| {
                            if let crate::lua::ui::schema::Primitive::Image { resource, .. } = p {
                                Some(std::sync::Arc::clone(resource))
                            } else {
                                None
                            }
                        })
                })
                .unwrap();
        r.h.app.roll.view.time.zoom_around(100., 1.);
        r.h.app.roll.view.pitch.scroll_rows += 0.5;
        frame_until(
            &mut r,
            "author automatically republishes the current zoomed guide",
            |r| {
                r.h.app
                    .lua_overlays
                    .views
                    .iter()
                    .any(|v| v.plugin == EX && v.frame.view.revision > initial.revision)
            },
        );
        assert_eq!(image.test_uploads(), 1);
        assert!(image.alive());
        run_plugin(&mut r, EX, "clear_piano_roll");
        frame(&mut r, 1280., 1.);
        r.h.app.roll.view.time.zoom_around(100., -1.);
        let end = Instant::now() + Duration::from_millis(250);
        while Instant::now() < end {
            frame(&mut r, 1280., 1.);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            r.h.app.lua_overlays.views.is_empty(),
            "clear also disables the author's auto-guide mode"
        );
        r.h.app.settings.player.enabled = true;
        for _ in 0..3 {
            r.frame(1280., 1.);
        }
        run_plugin(&mut r, EX, "auto_player");
        frame(&mut r, 1280., 1.);
        let first_player = r.h.app.lua_view_snapshots()[1].unwrap();
        r.h.app.settings.player.fall_seconds += 1.;
        frame_until(
            &mut r,
            "Player source change refreshes the author's captured readout",
            |r| {
                r.h.app.lua_overlays.views.iter().any(|v| {
                    v.plugin == EX
                        && v.frame.view.facts.index() == 1
                        && v.frame.view.revision > first_player.revision
                })
            },
        );
        let o = r.h.app.open.as_ref().unwrap();
        r.h.app.transport.seek(1234, &o.doc, o.generation);
        frame_until(
            &mut r,
            "transport-only source refresh updates the current Player overlay",
            |r| {
                r.h.app.lua_overlays.views.iter().filter(|v|v.plugin==EX && v.frame.view.facts.index()==1).any(|v|v.frame.primitives.as_ref().unwrap().items.iter().any(|p|matches!(p,crate::lua::ui::schema::Primitive::Text{text,..} if text.contains("tick 1234"))))
            },
        );
        assert_eq!(
            plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
            before
        );
        let p = r.h.app.lua_plugins.as_ref().unwrap().plugin(EX).unwrap();
        assert!(
            !p.log
                .lines()
                .any(|l| l.level == crate::lua::plugins::Level::Error),
            "{:?}",
            p.log.lines().map(|l| l.text.clone()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn pending_native_frame_defers_metadata_worker_and_hidden_surface_consumption_releases_it() {
    use crate::lua::reactive::{Data, Kind};
    const OBS: &str = "test.overlay.mailbox-observer";
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280., 1.);
    }
    install_source(
        &mut r,
        OBS,
        &["ui.overlay", "project.read"],
        r#"
      plum.require_api(0,2)
      plum.plugin.command{id='publish',name='Publish',run=function()
        plum.ui.publish_overlay(assert(plum.ui.view_snapshot('piano_roll')),{id='pending',primitives={{kind='text',x=.1,y=.2,text='pending'}}})
      end}
      plum.plugin.observe{id='surface',events={'surface_changed'},debounce_ms=20,run=function(e)
        print(e.data.visible and 'EVENT_AFTER_CONSUME' or 'SURFACE_HIDDEN')
      end}
    "#,
    );
    frame_until(&mut r, "initial metadata delivered", |r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(OBS)
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("EVENT_AFTER_CONSUME"))
    });
    let current = r.h.app.lua_view_snapshots()[0].unwrap();
    let world = r.h.app.lua_world(true).unwrap();
    let prefs = r.h.app.settings.lua_plugins.clone();
    let m = r.h.app.lua_plugins.as_mut().unwrap();
    m.plugin_log_mut(OBS).unwrap().clear();
    m.run_command(OBS, "publish", Some(world)).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while m.plugin(OBS).unwrap().user_job_pending() {
        m.poll(&prefs);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(m.publish_observation(
        Kind::SurfaceChanged,
        current.stamp,
        Data::Surface {
            index: 0,
            view_revision: Some(current.revision)
        }
    ));
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        m.poll(&prefs);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !m.plugin(OBS)
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("EVENT_AFTER_CONSUME")),
        "metadata cannot race an unconsumed command frame"
    );
    assert!(m.poll_overlay_request().is_some());
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        m.poll(&prefs);
        if m.plugin(OBS)
            .unwrap()
            .log
            .lines()
            .any(|l| l.text.contains("EVENT_AFTER_CONSUME"))
        {
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    let world = r.h.app.lua_world(true).unwrap();
    let m = r.h.app.lua_plugins.as_mut().unwrap();
    m.run_command(OBS, "publish", Some(world)).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while m.plugin(OBS).unwrap().user_job_pending() {
        m.poll(&prefs);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    r.h.app.main_view = MainView::TrackList;
    frame_until(
        &mut r,
        "hidden surface drops/acknowledges stale submission and delivers metadata",
        |r| {
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(OBS)
                .unwrap()
                .log
                .lines()
                .any(|l| l.text.contains("SURFACE_HIDDEN"))
        },
    );
    assert!(r.h.app.lua_overlays.views.is_empty());
    assert!(
        !r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(OBS)
            .unwrap()
            .log
            .lines()
            .any(|l| l.level == crate::lua::plugins::Level::Error)
    );
}
