//! Native headless egui/App + real plugin worker; no physical MIDI or desktop UI.
use super::*;
use crate::app::test_support::Harness;
use crate::lua::bridge::NativeBudget;
use crate::lua::midi_input::windows::Device;
use crate::lua::plugins::Status;
use plumspirit_engine::model::{Bpm, PlumDocument};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
const ID: &str = "test.midi-consent";
type ChoiceReceiver =
    Receiver<Result<Option<crate::lua::midi_input::windows::Approved>, BridgeError>>;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "plum-midi-consent-app-{}-{}",
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
                    .starts_with("plum-midi-consent-app-")
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
fn frames(h: &mut Harness, condition: impl Fn(&Harness) -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !condition(h) {
        h.app_frame(egui::vec2(1440., 1000.), 1.);
        assert!(Instant::now() < until, "native consent App timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn setup(keys: u16) -> (Harness, Temp) {
    let temp = Temp::new();
    let mut doc = PlumDocument::new_project("MIDI input consent".into(), Bpm::DEFAULT, 960);
    if keys == 256 {
        doc.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let mut h = Harness::new(doc);
    h.ctx.all_styles_mut(|s| s.animation_time = 0.0);
    h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
    let package = temp.0.join("package");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(
        package.join("plum-plugin.toml"),
        r#"id="test.midi-consent"
name="MIDI consent fixture"
version="1.0.0"
api="0.2"
entry="main.lua"
capabilities=["midi.input"]
"#,
    )
    .unwrap();
    std::fs::write(package.join("main.lua"),"function register(p) p.command{id='listen',name='Listen',run=function()return plum.midi_input.open()end};p.command{id='ping',name='Ping',run=function()print('ready')end} end").unwrap();
    let mut prefs = h.app.settings.lua_plugins.clone();
    h.app
        .lua_plugins()
        .install(&package, false, &mut prefs)
        .unwrap();
    h.app
        .lua_plugins()
        .set_granted(ID, "midi.input", true, &mut prefs);
    h.app.lua_plugins().set_enabled(ID, true, &mut prefs);
    h.app.settings.lua_plugins = prefs;
    frames(&mut h, |h| {
        h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .is_some_and(|p| p.status == Status::Ready)
            && !h.app.lua_plugins.as_ref().unwrap().is_busy()
    });
    (h, temp)
}
fn stamp(h: &Harness) -> ProjectStamp {
    let o = h.app.open.as_ref().unwrap();
    ProjectStamp {
        serial: o.history.incarnations().project(),
        revision: o.history.current_state(),
    }
}
fn fixture(h: &mut Harness) -> (Arc<JobLease>, NativeBudget, ChoiceReceiver) {
    let lease = JobLease::fixture(ID, Some(stamp(h)), Duration::from_secs(10), true);
    let budget = NativeBudget::isolated(1024 * 1024);
    let (request, rx) = ConsentRequest::fixture(
        Arc::clone(&lease),
        vec![Device::fixture(0, "First"), Device::fixture(3, "Second")],
        &budget,
    );
    h.app.lua_midi_input.request = Some(request);
    h.app.lua_midi_input.selected = None;
    (lease, budget, rx)
}
fn click(h: &mut Harness, p: egui::Pos2, scale: f32) {
    h.app_frame_events(
        egui::vec2(1440., 1000.),
        scale,
        vec![egui::Event::PointerMoved(p)],
    );
    let events = vec![
        egui::Event::PointerMoved(p),
        egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ];
    h.app_frame_events(egui::vec2(1440., 1000.), scale, events);
    h.app_frame_events(
        egui::vec2(1440., 1000.),
        scale,
        vec![egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
}
fn warm(h: &mut Harness, scale: f32) {
    for _ in 0..3 {
        h.app_frame(egui::vec2(1440., 1000.), scale);
    }
}

#[test]
fn native_device_choice_requires_explicit_selection_and_click_approval_at_both_key_modes_and_dpi() {
    for (keys, scale) in [(128, 1.), (256, 1.5), (256, 2.)] {
        let (mut h, _temp) = setup(keys);
        let source = Arc::clone(&h.app.open.as_ref().unwrap().doc);
        let (lease, budget, rx) = fixture(&mut h);
        warm(&mut h, scale);
        assert!(h.app.lua_midi_input.selected.is_none());
        assert!(!h.app.lua_midi_input.controls.allow_enabled);
        assert!(rx.try_recv().is_err());
        let combo = h.app.lua_midi_input.controls.combo.unwrap().center();
        click(&mut h, combo, scale);
        h.app_frame(egui::vec2(1440., 1000.), scale);
        assert_eq!(h.app.lua_midi_input.controls.devices.len(), 2);
        let second = h.app.lua_midi_input.controls.devices[1].center();
        click(&mut h, second, scale);
        h.app_frame(egui::vec2(1440., 1000.), scale);
        assert_eq!(h.app.lua_midi_input.selected, Some(1));
        assert!(h.app.lua_midi_input.controls.allow_enabled);
        let allow = h.app.lua_midi_input.controls.allow.unwrap().center();
        click(&mut h, allow, scale);
        let approved = rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(approved.device().slot, 3);
        assert_eq!(approved.device().name(), "Second");
        assert!(!h.app.lua_midi_input.pending());
        assert_eq!(h.app.lua_midi_input.active.len(), 1);
        assert!(lease.check().is_ok());
        assert_eq!(budget.used(), 0);
        assert!(Arc::ptr_eq(&source, &h.app.open.as_ref().unwrap().doc));
        h.app.lua_midi_input.clear();
        assert!(lease.check().is_err());
        assert!(lease.cancellation_requested());
    }
}

#[test]
fn native_cancel_and_escape_return_none_without_admitting_a_device_lease() {
    for escape in [false, true] {
        let (mut h, _temp) = setup(128);
        let (_, budget, rx) = fixture(&mut h);
        warm(&mut h, 1.);
        if escape {
            h.app_frame_events(
                egui::vec2(1440., 1000.),
                1.,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        } else {
            let cancel = h.app.lua_midi_input.controls.cancel.unwrap().center();
            click(&mut h, cancel, 1.);
        }
        assert!(
            rx.recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(h.app.lua_midi_input.active.is_empty());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn approved_native_lease_is_revoked_by_permission_disable_project_close_revision_and_cleanup() {
    for mode in 0..6 {
        let (mut h, _temp) = setup(128);
        let (lease, budget, rx) = fixture(&mut h);
        h.app.lua_midi_input.selected = Some(0);
        warm(&mut h, 1.);
        let allow = h.app.lua_midi_input.controls.allow.unwrap().center();
        click(&mut h, allow, 1.);
        assert!(
            rx.recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .is_some()
        );
        assert_eq!(budget.used(), 0);
        match mode {
            0 => {
                let mut p = h.app.settings.lua_plugins.clone();
                h.app
                    .lua_plugins()
                    .set_granted(ID, "midi.input", false, &mut p);
                h.app.settings.lua_plugins = p;
            }
            1 => {
                let mut p = h.app.settings.lua_plugins.clone();
                h.app.lua_plugins().set_enabled(ID, false, &mut p);
                h.app.settings.lua_plugins = p;
            }
            2 => {
                h.app.open = None;
            }
            3 => {
                let doc = PlumDocument::new_project("Replacement".into(), Bpm::DEFAULT, 960);
                h.app
                    .install_document(doc, crate::app::DocSource::New, true);
            }
            4 => h.app.lua_midi_input.clear(),
            _ => {
                let before = stamp(&h);
                assert!(h.command(crate::app::commands::Command::AddTrack));
                assert_ne!(stamp(&h).revision, before.revision);
            }
        }
        h.app_frame(egui::vec2(1440., 1000.), 1.);
        assert!(lease.check().is_err());
        assert!(lease.cancellation_requested());
        assert!(h.app.lua_midi_input.active.is_empty());
    }
}

#[test]
fn real_worker_pending_consent_expires_on_reload_and_replacement_plugin_remains_usable() {
    let (mut h, _temp) = setup(128);
    h.app.run_lua_command(ID, "listen", true);
    frames(&mut h, |h| h.app.lua_midi_input.pending());
    let old = Arc::clone(&h.app.lua_midi_input.request.as_ref().unwrap().lease);
    assert!(h.app.lua_midi_input.selected.is_none());
    let prefs = h.app.settings.lua_plugins.clone();
    h.app.lua_plugins().reload(ID, &prefs);
    frames(&mut h, |h| {
        !h.app.lua_midi_input.pending() && !h.app.lua_plugins.as_ref().unwrap().is_busy()
    });
    assert!(old.check().is_err());
    assert_eq!(
        h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .status,
        Status::Ready
    );
    h.app.run_lua_command(ID, "ping", true);
    frames(&mut h, |h| !h.app.lua_plugins.as_ref().unwrap().is_busy());
    assert_eq!(
        h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .status,
        Status::Ready
    );
}

#[test]
fn pending_native_choice_cannot_outlive_permission_or_project_and_state_drop_cancels() {
    for mode in 0..3 {
        let (mut h, _temp) = setup(128);
        let (lease, budget, rx) = fixture(&mut h);
        match mode {
            0 => {
                let mut p = h.app.settings.lua_plugins.clone();
                h.app
                    .lua_plugins()
                    .set_granted(ID, "midi.input", false, &mut p);
                h.app.settings.lua_plugins = p;
            }
            1 => {
                h.app.open = None;
            }
            _ => h.app.lua_midi_input.clear(),
        }
        h.app_frame(egui::vec2(1440., 1000.), 1.);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(BridgeError::Expired)
        ));
        assert!(lease.check().is_err());
        assert_eq!(budget.used(), 0);
    }
}
