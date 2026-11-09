//! Native App frames and plugin authority; no Windows desktop/device claim.
use super::*;
use crate::{
    app::test_support::Harness,
    lua::{
        Scripts,
        plugins::{PluginManager, PluginPrefs},
    },
};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
pub(in crate::app) const ID: &str = "test.api02.view-frame";
pub(in crate::app) struct Rig {
    pub(in crate::app) h: Harness,
    pub(in crate::app) root: PathBuf,
}
impl Rig {
    pub(in crate::app) fn new() -> Self {
        Self::new_with_keys(false)
    }
    pub(in crate::app) fn new_with_keys(extended: bool) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "plumspirit-view-frame-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("source")).unwrap();
        std::fs::write(
            root.join("source/plum-plugin.toml"),
            format!(
                r#"id="{ID}"
name="View frame native fixture"
version="1.0"
api="0.2"
capabilities=["ui.overlay","project.read","playback.read"]
"#
            ),
        )
        .unwrap();
        std::fs::write(root.join("source/main.lua"),r#"plum.require_api(0,2)
plum.plugin.command{id='inspect',name='Inspect',run=function()local s=plum.ui.view_snapshot('piano_roll');print(s and s:info().surface or 'hidden')end}
plum.plugin.command{id='retain',name='Retain',run=function()
  local image=plum.images.from_rgba(1,1,string.char(255,0,0,128))
  retained=plum.ui.retain_image(plum.ui.image(image));print('RETAIN_READY')
end}
plum.plugin.command{id='retained_info',name='Retained info',run=function()
  local ok=pcall(function()assert(retained:info().width==1)end)
  print(ok and 'RETAIN_OK' or 'RETAIN_EXPIRED')
end}
for _,surface in ipairs({'piano_roll','player'})do
  plum.plugin.command{id='overlay_'..surface,name='Overlay '..surface,run=function()
    local view=assert(plum.ui.view_snapshot(surface))
    plum.ui.publish_overlay(view,{id='guide',primitives={
      {kind='line',x1=0.25,y1=0.5,x2=0.75,y2=0.5,width=3,color={31,117,233,127}},
      {kind='text',x=0.25,y=0.5,text='Authored overlay',color={31,117,233,127}}
    }})
    print('OVERLAY_'..surface)
  end}
end
plum.plugin.command{id='overlay_clear',name='Clear roll overlay',run=function()
  plum.ui.clear_overlay(assert(plum.ui.view_snapshot('piano_roll')),'guide');print('OVERLAY_CLEARED')
end}
"#).unwrap();
        let mut doc = PlumDocument::new_project("Native view".into(), Bpm::DEFAULT, 960);
        if extended {
            doc.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
        }
        let notes: Vec<_> = (0..1000)
            .map(|i| NoteRecord {
                start_tick: i * 120,
                duration_ticks: 60,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, false)
            .unwrap();
        let mut h = Harness::new(doc);
        let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
        m.set_data_root(Some(root.join("data")));
        let mut prefs = PluginPrefs {
            folder: Some(root.join("plugins")),
            ..Default::default()
        };
        m.install(&root.join("source"), false, &mut prefs).unwrap();
        assert!(prefs.installed[ID].granted.is_empty());
        for cap in ["ui.overlay", "project.read", "playback.read"] {
            m.set_granted(ID, cap, true, &mut prefs);
        }
        m.set_enabled(ID, true, &mut prefs);
        h.app.settings.lua_plugins = prefs;
        h.app.lua_plugins = Some(m);
        h.app.lua_plugins_started = true;
        let mut r = Self { h, root };
        let end = Instant::now() + Duration::from_secs(5);
        while r
            .h
            .app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .status
            != Status::Ready
        {
            r.h.app.poll_lua_plugins(&r.h.ctx);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        r.frame(1280.0, 1.0); // native New/Open starts in Track List on its first frame
        r
    }
    pub(in crate::app) fn frame(&mut self, width: f32, scale: f32) {
        self.h.app_frame(egui::vec2(width, 800.0), scale);
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut m) = self.h.app.lua_plugins.take() {
            m.stop_all(&self.h.app.settings.lua_plugins);
        }
        let root = self.root.canonicalize().unwrap();
        assert_eq!(
            root.parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plumspirit-view-frame-test-")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn native_app_captures_only_visible_authorized_roll_and_player_and_fences_zoom_resize_revoke() {
    let mut r = Rig::new();
    let before = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    let stamp = r.h.app.lua_observation_stamp();
    r.h.app.main_view = MainView::PianoRoll;
    for _ in 0..3 {
        r.frame(1280.0, 1.0);
    }
    assert!(r.h.app.lua_views.enabled, "authorized capture flag");
    assert_eq!(
        r.h.app.main_view,
        MainView::PianoRoll,
        "native view is visible"
    );
    assert!(r.h.app.roll.view_frame().is_some(), "roll actually drawn");
    let first = r.h.app.lua_view_snapshots()[0].expect("native roll frame");
    let Facts::Roll(f) = first.facts else {
        unreachable!()
    };
    assert!(f.matches(r.h.doc()));
    assert_eq!(f.track, r.h.doc().track(0).unwrap().id());
    assert_eq!(f.rect, r.h.app.roll.notes_rect().unwrap());
    assert!(r.h.app.lua_view_snapshots()[1].is_none());
    r.h.app.roll.view.time.zoom_around(100.0, 1.0);
    r.h.app.roll.view.pitch.scroll_rows += 0.5;
    r.frame(900.0, 1.0);
    let changed = r.h.app.lua_view_snapshots()[0].unwrap();
    assert!(changed.revision > first.revision);
    let same = r.h.app.lua_view_snapshots()[0].unwrap();
    assert_eq!(changed, same);
    let world = r.h.app.lua_world(true).unwrap();
    assert_eq!(world.info.views[0], Some(same));
    drop(world);
    r.h.app.settings.player.enabled = true;
    for _ in 0..3 {
        r.frame(1280.0, 1.0);
    }
    let views = r.h.app.lua_view_snapshots();
    assert!(views[0].is_none());
    let player = views[1].expect("native Player frame");
    let Facts::Player(f) = player.facts else {
        unreachable!()
    };
    assert!(f.rect.contains_rect(f.rain) && f.rect.contains_rect(f.keyboard));
    assert_eq!(f.keys, 128);
    r.h.app.settings.player.enabled = false;
    r.h.app.main_view = MainView::TrackList;
    r.frame(1280.0, 1.0);
    assert_eq!(r.h.app.lua_view_snapshots(), [None, None]);
    r.h.app.lua_plugins.as_mut().unwrap().set_granted(
        ID,
        "ui.overlay",
        false,
        &mut r.h.app.settings.lua_plugins,
    );
    r.h.app.main_view = MainView::PianoRoll;
    r.frame(1280.0, 1.0);
    assert!(!r.h.app.lua_views.enabled);
    assert!(r.h.app.roll.view_frame().is_none());
    assert_eq!(r.h.app.lua_view_snapshots(), [None, None]);
    assert_eq!(r.h.app.lua_observation_stamp(), stamp);
    assert_eq!(r.h.doc().note_count(), 1000);
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        before
    );
}
#[test]
fn default_disabled_native_capture_leaves_view_metadata_empty() {
    let mut h = Harness::new(PlumDocument::new_project(
        "No overlay permissions".into(),
        Bpm::DEFAULT,
        960,
    ));
    h.app_frame(egui::vec2(1280.0, 800.0), 1.0);
    h.app.main_view = MainView::PianoRoll;
    h.app_frame(egui::vec2(1280.0, 800.0), 1.0);
    assert!(!h.app.lua_views.enabled);
    assert!(h.app.roll.view_frame().is_none());
    assert_eq!(h.app.lua_view_snapshots(), [None, None]);
}
#[test]
fn native_project_edit_rejects_old_rendered_geometry_until_a_new_frame_is_drawn() {
    let mut r = Rig::new();
    r.h.app.main_view = MainView::PianoRoll;
    r.frame(1280.0, 1.0);
    let before = r.h.app.lua_view_snapshots()[0].unwrap();
    r.h.app
        .apply_op(plumspirit_engine::history::Operation::InsertNote {
            track: plumspirit_engine::editor::NoteTarget::Track(0),
            record: NoteRecord::new(
                120001,
                120,
                NoteAttrs::new(72, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            ),
        })
        .unwrap();
    assert!(
        r.h.app.lua_view_snapshots()[0].is_none(),
        "old rendered context cannot name the edited document"
    );
    let world = r.h.app.lua_world(true).unwrap();
    assert!(world.info.views[0].is_none());
    drop(world);
    r.frame(1280.0, 1.0);
    let after = r.h.app.lua_view_snapshots()[0].unwrap();
    assert!(after.revision > before.revision);
    assert_ne!(after.stamp, before.stamp);
    assert_eq!(r.h.doc().note_count(), 1001);
}
#[test]
fn native_manager_project_close_releases_retained_pixels_even_without_a_lua_close_hook() {
    let mut r = Rig::new();
    let run = |r: &mut Rig, command: &str, expected: &str| {
        let world = r.h.app.lua_world(true).unwrap();
        r.h.app
            .lua_plugins
            .as_mut()
            .unwrap()
            .run_command(ID, command, Some(world))
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            r.h.app.poll_lua_plugins(&r.h.ctx);
            let p = r.h.app.lua_plugins.as_ref().unwrap().plugin(ID).unwrap();
            if !p.is_busy() && p.log.lines().any(|l| l.text.contains(expected)) {
                break;
            }
            assert!(
                Instant::now() < end,
                "logs {:?}",
                p.log.lines().map(|l| l.text.clone()).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let stamp = r.h.app.lua_observation_stamp();
    run(&mut r, "retain", "RETAIN_READY");
    run(&mut r, "retained_info", "RETAIN_OK");
    r.h.app.lua_plugins.as_mut().unwrap().project_closed();
    // Probe the original identity deliberately: closing/reopening ownership must
    // never revive the previous asset merely because its project serial matches.
    run(&mut r, "retained_info", "RETAIN_EXPIRED");
    assert_eq!(r.h.app.lua_observation_stamp(), stamp);
    assert_eq!(r.h.doc().note_count(), 1000);
}
