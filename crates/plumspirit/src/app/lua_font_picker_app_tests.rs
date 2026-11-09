//! Actual installed sample worker and headless native App UI on Windows host fonts.
use crate::app::{commands::Command, test_support::Harness};
use crate::lua::plugins::Status;
use plumspirit_engine::model::{Bpm, PlumDocument};
use std::path::PathBuf;
use std::time::{Duration, Instant};
const ID: &str = "plumspirit.example.font-resources";
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "plum-font-picker-app-{}-{}",
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
                    .starts_with("plum-font-picker-app-")
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
fn frames(h: &mut Harness, ready: impl Fn(&Harness) -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !ready(h) {
        h.app_frame(egui::vec2(1500., 1000.), 1.);
        assert!(
            Instant::now() < until,
            "Font picker App condition: {}",
            h.status()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn setup(wide: bool) -> (Harness, Temp) {
    let temp = Temp::new();
    let mut d = PlumDocument::new_project("Font picker read only".into(), Bpm::DEFAULT, 960);
    if wide {
        d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let mut h = Harness::new(d);
    h.app.config_dir = Some(temp.0.join("config"));
    h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
    h.app.settings.lua_plugins.enable_on_install = true;
    h.app_frame(egui::vec2(1500., 1000.), 1.);
    let mut prefs = h.app.settings.lua_plugins.clone();
    h.app
        .lua_plugins()
        .install_example(crate::lua::examples::by_id(ID).unwrap(), &mut prefs)
        .unwrap();
    h.app.settings.lua_plugins = prefs;
    frames(&mut h, |h| {
        h.app.lua_plugins.as_ref().is_some_and(|m| {
            !m.is_busy() && m.plugin(ID).is_some_and(|p| p.status == Status::Ready)
        })
    });
    (h, temp)
}
fn open(h: &mut Harness) {
    h.app.run_lua_command(ID, "choose-installed", true);
    frames(h, |h| !h.app.lua_panels.is_empty());
    for _ in 0..3 {
        h.app_frame(egui::vec2(1500., 1000.), 1.);
    }
}
pub(in crate::app) fn click(h: &mut Harness, id: &str, scale: f32) {
    for _ in 0..3 {
        h.app_frame(egui::vec2(1500., 1000.), scale);
    }
    let pos = h.app.lua_panels[0].widget_rects[id].center();
    h.app_frame_events(
        egui::vec2(1500., 1000.),
        scale,
        vec![egui::Event::PointerMoved(pos)],
    );
    for pressed in [true, false] {
        h.app_frame_events(
            egui::vec2(1500., 1000.),
            scale,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            }],
        );
    }
}
#[test]
fn installed_picker_native_choose_dpi_cancel_and_reopen_keep_musical_document_and_undo_unchanged() {
    for (wide, scale) in [(false, 1.), (true, 1.5)] {
        let (mut h, temp) = setup(wide);
        let before = h.doc().state_fingerprint();
        assert!(!h.app.enabled(Command::Undo));
        let menu = h.app.lua_menu();
        let item = menu.iter().find(|p| p.0 == ID).unwrap();
        assert_eq!(item.2.len(), 4);
        assert!(item.2.iter().any(|c| c.0 == "choose-installed"));
        open(&mut h);
        let view = &h.app.lua_panels[0];
        assert_eq!(view.snapshot.title, "Installed Font Chooser");
        assert!(
            view.widget_rects
                .keys()
                .filter(|k| k.starts_with("faces:"))
                .count()
                < 16,
            "list renders bounded visible rows"
        );
        assert!(view.snapshot.root.id == "font_root");
        click(&mut h, "choose", scale);
        frames(&mut h, |h| {
            h.app.lua_panels.is_empty() && h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
        let file = temp
            .0
            .join("config")
            .join("plugin-data")
            .join(ID)
            .join("config.json");
        let bytes = std::fs::read(&file)
            .expect("native Choose must persist a real installed font preference");
        let data: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(data["values"]["font_choice"]["version"], 1);
        assert_eq!(
            data["values"]["font_choice"]["report"]["font"]["source"],
            "installed"
        );
        assert_eq!(h.doc().state_fingerprint(), before);
        assert!(!h.app.enabled(Command::Undo));
        open(&mut h);
        click(&mut h, "cancel", scale);
        frames(&mut h, |h| {
            h.app.lua_panels.is_empty() && h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
        });
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert_eq!(h.doc().state_fingerprint(), before);
        drop(h);
        drop(temp);
    }
}

#[path = "lua_color_laboratory_app_tests.rs"]
mod color_laboratory;

pub(in crate::app) fn image_count(h: &Harness) -> usize {
    h.app.lua_panels.first().map_or(0, |view| view.images.len())
}

pub(in crate::app) fn panel_status(h: &Harness) -> String {
    fn find(n: &crate::lua::ui::schema::Node) -> Option<String> {
        if n.id == "status" {
            return Some(n.label.clone());
        }
        match &n.kind {
            crate::lua::ui::schema::Kind::Column(children)
            | crate::lua::ui::schema::Kind::Row(children)
            | crate::lua::ui::schema::Kind::Group(children) => children.iter().find_map(find),
            _ => None,
        }
    }
    h.app
        .lua_panels
        .first()
        .and_then(|view| find(&view.snapshot.root))
        .unwrap_or_default()
}
