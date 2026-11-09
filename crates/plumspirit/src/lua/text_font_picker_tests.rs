//! Actual author picker on native font files, driven through bounded worker UI events.
use super::*;
use crate::lua::ui::{
    self, Event, InputValue, Shared,
    schema::{Kind, Node, Snapshot},
};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
const MAIN: &str = include_str!("../../examples-lua/font-resources/main.lua");
const PICKER: &str = include_str!("../../examples-lua/font-resources/font-picker.lua");
fn setup(t: &Temp, caps: &[Capability]) -> (Host, Receiver<Arc<Shared>>) {
    std::fs::write(t.root.join("font-picker.lua"), PICKER).unwrap();
    let h = Host::new(
        Limits {
            memory: 32 * 1024 * 1024,
            ..Default::default()
        },
        caps.iter().copied(),
    )
    .unwrap();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: "plumspirit.example.font-resources".into(),
        name: "Font Resource Inspector".into(),
        version: "1.0.1".into(),
        root: t.root.clone(),
        data_dir: Some(t.root.join("data")),
        config_version: 1,
    })
    .unwrap();
    h.lua
        .set_app_data(NativeBudget::isolated(128 * 1024 * 1024));
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
    let (client, rx) = ui::channel("plumspirit.example.font-resources");
    h.lua.app_data_mut::<ui::Slot>().unwrap().client = Some(client);
    let (client, _) = crate::lua::bridge::channel("plumspirit.example.font-resources");
    {
        let mut s = h.lua.app_data_mut::<Session>().unwrap();
        s.client = Some(client);
        s.interactive = true;
    }
    run(
        &h,
        &format!("{MAIN}\nregister(plum.plugin);plum.plugin.__phase('ready')"),
    );
    (h, rx)
}
struct Action {
    kind: &'static str,
    id: &'static str,
    value: InputValue,
    wait: bool,
}
fn click(id: &'static str) -> Action {
    Action {
        kind: "activate",
        id,
        value: InputValue::None,
        wait: true,
    }
}
fn text(id: &'static str, value: &str) -> Action {
    Action {
        kind: "change",
        id,
        value: InputValue::Text(value.into()),
        wait: true,
    }
}
fn after_text(id: &'static str) -> Action {
    Action {
        wait: false,
        ..click(id)
    }
}
fn node<'a>(n: &'a Node, id: &str) -> Option<&'a Node> {
    if n.id == id {
        return Some(n);
    }
    match &n.kind {
        Kind::Column(v) | Kind::Row(v) | Kind::Group(v) => v.iter().find_map(|n| node(n, id)),
        _ => None,
    }
}
fn drive(h: &Host, rx: Receiver<Arc<Shared>>, actions: Vec<Action>) -> Vec<Arc<Snapshot>> {
    drive_command(h, rx, "choose-installed", actions)
}
fn drive_command(
    h: &Host,
    rx: Receiver<Arc<Shared>>,
    command: &str,
    actions: Vec<Action>,
) -> Vec<Arc<Snapshot>> {
    let driver = std::thread::spawn(move || {
        let shared = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut revision = 0;
        let mut snapshots = Vec::new();
        for a in actions {
            let start = Instant::now();
            let snapshot = loop {
                if let Some(s) = shared.snapshot()
                    && (s.revision > revision || !a.wait)
                {
                    break s;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "picker did not update before {} (closed={})",
                    a.id,
                    shared.closed()
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            revision = snapshot.revision;
            assert!(
                node(&snapshot.root, a.id).is_some_and(|n| n.enabled),
                "requested enabled control {}",
                a.id
            );
            snapshots.push(snapshot);
            assert!(shared.push(Event {
                kind: a.kind,
                id: a.id.into(),
                value: a.value,
                pointer: if a.kind == "resize" {
                    Some(ui::Pointer {
                        x: 0.,
                        y: 0.,
                        width: 1000.,
                        height: 250.,
                        buttons: 0,
                        shift: false,
                        ctrl: false,
                        alt: false,
                        command: false,
                        scroll_x: 0.,
                        scroll_y: 0.,
                        scroll_unit: "point",
                        time: 0.,
                    })
                } else {
                    None
                },
                revision,
                sequence: 0
            }));
        }
        snapshots
    });
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    let result = h.run(
        "font-picker-command.lua",
        &format!("plum.plugin.__run('{command}',{{}})"),
    );
    let joined = driver.join();
    result.unwrap();
    let snapshots = joined.unwrap();
    assert!(h.take_commits().is_none());
    snapshots
}
#[test]
fn authored_chooser_previews_native_geometry_and_saves_one_plain_versioned_choice_with_report() {
    let t = Temp::new();
    t.bundled();
    let (h, rx) = setup(&t, &[Capability::ListFonts, Capability::UiPanels]);
    run(
        &h,
        "plum.config.set('last_report',{sentinel=42});plum.config.set('font_choice',{family='missing',style='Regular'})",
    );
    let shots = drive(
        &h,
        rx,
        vec![
            click("preview"),
            click("save_preset"),
            Action {
                kind: "change",
                id: "faces",
                value: InputValue::Choice(1),
                wait: true,
            },
            click("choose"),
        ],
    );
    let canvas = node(&shots[1].root, "glyph").unwrap();
    let Kind::Canvas { primitives, .. } = &canvas.kind else {
        panic!("native outline canvas")
    };
    assert!(!primitives.is_empty() && primitives.len() <= 32);
    assert!(
        node(&shots[1].root, "preview_status")
            .unwrap()
            .label
            .contains("Glyph")
    );
    drop(shots);
    run(
        &h,
        r#"local c=plum.config.get('font_choice');assert(c.version==1 and c.family:find('Ubuntu')and c.face_index==0 and c.report.font.source=='installed'and #c.report.coverage==6);assert(plum.config.get('last_report').sentinel==42);local p=plum.presets.load('Installed Font Choice');assert(p.version==1 and p.family:find('Hack')and p.report==nil);for k in pairs(c)do assert(k~='path'and k~='handle'and k~='catalog')end"#,
    );
    clear(&h);
}
#[test]
fn preset_restore_invalid_scalar_and_cancel_leave_saved_choice_and_existing_report_unchanged() {
    let t = Temp::new();
    t.bundled();
    let (h, rx) = setup(&t, &[Capability::ListFonts, Capability::UiPanels]);
    run(
        &h,
        "plum.config.set('font_choice',{family='Previous',style='Regular',sentinel=9});plum.config.set('last_report',{sentinel=42});plum.presets.save('Installed Font Choice',{version=1,family='Hack',style='Regular',face_index=0})",
    );
    let shots = drive(
        &h,
        rx,
        vec![
            click("load_preset"),
            text("scalar", "ab"),
            after_text("preview"),
            click("cancel"),
        ],
    );
    assert!(
        node(&shots[3].root, "preview_status")
            .unwrap()
            .label
            .contains("exactly one Unicode scalar")
    );
    drop(shots);
    run(
        &h,
        "local c=plum.config.get('font_choice');assert(c.family=='Previous'and c.sentinel==9 and plum.config.get('last_report').sentinel==42)",
    );
    clear(&h);
}
#[test]
fn author_search_pages_and_missing_saved_choice_are_explicit_and_cancel_writes_nothing() {
    let t = Temp::new();
    collection(&t, "many.ttc", 128);
    let (h, rx) = setup(&t, &[Capability::ListFonts, Capability::UiPanels]);
    run(
        &h,
        "plum.config.set('font_choice',{family='Unavailable',style='Regular'})",
    );
    let shots = drive(
        &h,
        rx,
        vec![
            click("next"),
            text("query", "not-a-family"),
            after_text("search"),
            click("cancel"),
        ],
    );
    assert!(
        node(&shots[0].root, "status")
            .unwrap()
            .label
            .contains("Saved choice is not in this page")
    );
    assert!(
        node(&shots[1].root, "status")
            .unwrap()
            .label
            .contains("Page 2")
    );
    assert!(!node(&shots[3].root, "choose").unwrap().enabled);
    drop(shots);
    run(
        &h,
        "assert(plum.config.get('font_choice').family=='Unavailable'and plum.config.get('last_report')==nil)",
    );
    clear(&h);
}
#[test]
fn chooser_authority_and_surface_cancellation_expire_catalog_and_preserve_preferences() {
    for caps in [vec![Capability::ListFonts], vec![Capability::UiPanels]] {
        let t = Temp::new();
        t.bundled();
        let (h, _) = setup(&t, &caps);
        let e = h
            .run(
                "denied-picker.lua",
                "plum.plugin.__run('choose-installed',{})",
            )
            .unwrap_err();
        assert!(e.message.contains("Grant installed-font listing"));
        clear(&h);
    }
    let t = Temp::new();
    t.bundled();
    let (h, rx) = setup(&t, &[Capability::ListFonts, Capability::UiPanels]);
    run(
        &h,
        "plum.config.set('font_choice',{family='Previous',style='Regular'})",
    );
    let driver = std::thread::spawn(move || {
        let shared = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        shared.fail(ui::CANCELED);
    });
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    let result = h.run(
        "canceled-picker.lua",
        "plum.plugin.__run('choose-installed',{})",
    );
    let joined = driver.join();
    assert_eq!(result.unwrap_err().category, Category::Canceled);
    joined.unwrap();
    run(
        &h,
        "assert(plum.config.get('font_choice').family=='Previous')",
    );
    clear(&h);
}

#[test]
fn unsupported_scalar_has_an_explicit_missing_message_and_never_saves_a_replacement() {
    let t = Temp::new();
    t.bundled();
    let (h, rx) = setup(&t, &[Capability::ListFonts, Capability::UiPanels]);
    run(
        &h,
        "plum.config.set('font_choice',{family='Previous',style='Regular'})",
    );
    let scalar = char::from_u32(0x10ffff).unwrap().to_string();
    let shots = drive(
        &h,
        rx,
        vec![
            text("scalar", &scalar),
            after_text("preview"),
            click("cancel"),
        ],
    );
    assert!(
        node(&shots[2].root, "preview_status")
            .unwrap()
            .label
            .contains("no glyph")
    );
    let Kind::Canvas { primitives, .. } = &node(&shots[2].root, "glyph").unwrap().kind else {
        panic!("canvas")
    };
    assert!(primitives.is_empty());
    drop(shots);
    run(
        &h,
        "assert(plum.config.get('font_choice').family=='Previous')",
    );
    clear(&h);
}

#[path = "text_color_laboratory_tests.rs"]
mod color_laboratory;
