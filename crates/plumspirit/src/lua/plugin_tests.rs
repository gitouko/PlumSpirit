//! B1-0037 / B1-0038 / B1-0039: plugin packages (manifest, safe paths,
//! install / update / remove with rollback), the lifecycle on script workers
//! (load, register, project open / close, commands, unload, reload without
//! double registration, failures that never touch the project) and the
//! redacted plugin log.

use super::package::{Manifest, Origin, Store, safe_relative};
use super::plugins::{Level, PluginEvent, PluginManager, PluginPrefs, Status, redact};
use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A fresh empty folder for one test.
fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "plumspirit-plugin-test-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Writes a package folder with `manifest` and `files`.
fn package(at: &Path, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    std::fs::write(at.join("plum-plugin.toml"), manifest).unwrap();
    for (rel, text) in files {
        let p = at.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    at.to_path_buf()
}

const MANIFEST: &str = r#"
# A test plugin
id = "test.velocity"
name = "Velocity Tools"
version = "1.0.0"
api = "0.1"
entry = "main.lua"
author = "Tester"
license = "MIT"
description = "Doubles velocities."
capabilities = ["project.read", "notes.edit"]
"#;

const MAIN: &str = r#"
local util = plum.plugin.include("lib/util.lua")
print("entry ran " .. util.answer)
function on_load() print("on_load") end
function register(p)
  p.command{ id = "soften", name = "Soften Notes", tooltip = "Sets every velocity to 40.",
    run = function(ctx)
      local t = plum.project.current():track(1)
      local tx = plum.transaction{ label = "Soften Notes" }
      local n = tx:modify_notes(t:notes(), { velocity = 40 })
      tx:commit()
      return "softened " .. n
    end }
  p.command{ id = "late", name = "Late", run = function()
      plum.plugin.command{ id = "x", name = "X", run = function() end }
    end }
end
function on_project_open(project) print("opened " .. (project and project:name() or "?")) end
function on_project_close() print("closed") end
function on_unload() print("bye") end
"#;

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("main.lua", MAIN),
        ("lib/util.lua", "return { answer = 42 }"),
        ("presets/default.txt", "asset"),
    ]
}

#[test]
fn manifests_are_checked_with_sentences() {
    let m = Manifest::parse(MANIFEST).unwrap();
    assert_eq!(m.id, "test.velocity");
    assert_eq!(m.api, (0, 1));
    assert_eq!(m.capabilities, vec!["notes.edit", "project.read"]);
    assert!(m.compatible().is_ok());
    assert_eq!(Manifest::parse(&m.to_text()).unwrap(), m, "round trip");
    for (bad, words) in [
        (MANIFEST.replace("test.velocity", "Bad Id"), "the id"),
        (
            MANIFEST.replace("1.0.0", "1.x"),
            "numbers separated by dots",
        ),
        (MANIFEST.replace("\"0.1\"", "\"9.0\""), ""),
        (
            MANIFEST.replace("main.lua", "../escape.lua"),
            "leaves the plugin's folder",
        ),
        (
            MANIFEST.replace("main.lua", "C:\\\\evil.lua"),
            "absolute paths",
        ),
        (MANIFEST.replace("main.lua", "con.lua"), "reserved"),
        (MANIFEST.replace("main.lua", "main.dll"), ".lua file"),
        (
            MANIFEST.replace("notes.edit", "os.execute"),
            "unknown capability",
        ),
        (format!("{MANIFEST}\nhomepage = \"x\""), "unknown key"),
        (format!("{MANIFEST}\nname = \"again\""), "given twice"),
        (format!("[tool]\n{MANIFEST}"), "tables are not used"),
        (
            MANIFEST.replace("name = \"Velocity Tools\"\n", ""),
            "\u{201C}name\u{201D} is missing",
        ),
    ] {
        match Manifest::parse(&bad) {
            Ok(m) => {
                // Only the API case parses: it is refused at load.
                assert!(words.is_empty(), "{bad} parsed");
                assert!(
                    m.compatible()
                        .unwrap_err()
                        .contains("needs scripting API 9.0")
                );
            }
            Err(e) => assert!(e.contains(words), "{words}: {e}"),
        }
    }
    assert!(safe_relative("lib/a.lua").is_ok());
    assert!(safe_relative("lib\\a.lua").is_ok());
    for p in [
        "/etc/passwd",
        "\\\\server\\x",
        "a/../../b",
        "nul",
        "a/aux.txt",
        "a\u{0}b",
        "x/",
    ] {
        assert!(safe_relative(p).is_err(), "{p}");
    }
}

#[test]
fn installs_updates_and_removals_leave_the_store_whole() {
    let root = temp("store");
    let store = Store::new(root.join("plugins"));
    let src = package(&root.join("src"), MANIFEST, &files());
    let r = store.install_folder(&src, false).unwrap();
    assert_eq!(
        (r.files, r.replaced.clone()),
        (4, None),
        "the manifest and three files"
    );
    assert!(store.root.join("test.velocity/lib/util.lua").is_file());
    let e = store.install_folder(&src, false).unwrap_err();
    assert!(e.contains("already installed"), "{e}");
    // A broken update changes nothing.
    let broken = package(
        &root.join("broken"),
        &MANIFEST
            .replace("1.0.0", "2.0.0")
            .replace("main.lua", "missing.lua"),
        &files(),
    );
    assert!(store.install_folder(&broken, true).is_err());
    let scanned = store.scan();
    assert_eq!(scanned.len(), 1);
    assert_eq!(scanned[0].manifest.as_ref().unwrap().version, "1.0.0");
    // A good update replaces it and reports the old version.
    let v2 = package(
        &root.join("v2"),
        &MANIFEST.replace("1.0.0", "2.0.0"),
        &files(),
    );
    let r = store.install_folder(&v2, true).unwrap();
    assert_eq!(r.replaced.as_deref(), Some("1.0.0"));
    assert_eq!(store.scan()[0].manifest.as_ref().unwrap().version, "2.0.0");
    // No staging folder is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&store.root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty());
    // A single file with its capabilities line.
    let lua = root.join("My Script!.lua");
    std::fs::write(&lua, "-- plum-capabilities: project.read\nprint('hi')\n").unwrap();
    let r = store.install_file(&lua, false).unwrap();
    assert_eq!(r.manifest.id, "local.my-script");
    assert_eq!(r.manifest.capabilities, vec!["project.read"]);
    assert_eq!(store.scan().len(), 2);
    // Removing.
    store.remove("local.my-script").unwrap();
    assert!(
        store
            .remove("local.my-script")
            .unwrap_err()
            .contains("not installed")
    );
    assert!(
        store.remove("../plugins").is_err(),
        "ids are checked before any path is built"
    );
    assert_eq!(store.scan().len(), 1);
    // Links are refused (a plugin cannot reach outside its folder).
    #[cfg(unix)]
    {
        let linked = package(
            &root.join("linked"),
            &MANIFEST.replace("test.velocity", "test.linked"),
            &files(),
        );
        std::os::unix::fs::symlink("/etc", linked.join("etc")).unwrap();
        let e = store.install_folder(&linked, false).unwrap_err();
        assert!(e.contains("is a link"), "{e}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn project() -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..10u64)
        .map(|k| NoteRecord {
            start_tick: k * 120,
            duration_ticks: 100,
            attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

fn world(doc: &Arc<PlumDocument>, h: &mut History) -> World {
    h.observe_ids(doc);
    World::new(Arc::clone(doc), h.incarnations().snapshot())
}

/// Polls until nothing runs; returns every event. `world` answers
/// [`PluginEvent::NeedWorld`].
fn settle_with(
    m: &mut PluginManager,
    prefs: &PluginPrefs,
    world: &mut dyn FnMut() -> Option<World>,
) -> Vec<PluginEvent> {
    let t = Instant::now();
    let mut out = Vec::new();
    loop {
        for e in m.poll(prefs) {
            if matches!(e, PluginEvent::NeedWorld)
                && let Some(w) = world()
            {
                m.supply_world(w);
            }
            out.push(e);
        }
        if !m.is_busy() && !m.plugins().iter().any(|p| p.status == Status::Unloading) {
            out.extend(m.poll(prefs));
            return out;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "plugins never settled"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle(m: &mut PluginManager, prefs: &PluginPrefs) -> Vec<PluginEvent> {
    settle_with(m, prefs, &mut || None)
}

fn log_has(m: &PluginManager, id: &str, text: &str) -> bool {
    m.plugin(id)
        .unwrap()
        .log
        .lines()
        .any(|l| l.text.contains(text))
}

#[test]
fn a_plugin_loads_registers_runs_commands_and_unloads_cleanly() {
    let root = temp("lifecycle");
    let src = package(&root.join("src"), MANIFEST, &files());
    let scripts = Scripts::default();
    let mut m = PluginManager::new(Some(root.join("plugins")), scripts.clone());
    let mut prefs = PluginPrefs::default();
    m.install(&src, false, &mut prefs).unwrap();
    let st = &prefs.installed["test.velocity"];
    assert!(
        !st.enabled && st.granted.is_empty(),
        "nothing is enabled or granted by installing"
    );
    assert_eq!(st.origin, Origin::LocalFolder);
    assert_eq!(m.plugin("test.velocity").unwrap().status, Status::Disabled);
    // Grant, then enable: the plugin loads with what was granted.
    m.set_granted("test.velocity", "project.read", true, &mut prefs);
    m.set_granted("test.velocity", "notes.edit", true, &mut prefs);
    let (mut doc, mut h) = project();
    m.project_opened(world(&doc, &mut h));
    m.set_enabled("test.velocity", true, &mut prefs);
    let ev = {
        let (d2, h2) = (&doc, &mut h);
        settle_with(&mut m, &prefs, &mut || Some(world(d2, h2)))
    };
    assert!(ev.iter().any(|e| matches!(e, PluginEvent::NeedWorld)));
    let p = m.plugin("test.velocity").unwrap();
    assert_eq!(
        p.status,
        Status::Ready,
        "{:?}",
        p.log.lines().collect::<Vec<_>>()
    );
    assert!(ev.iter().any(|e| matches!(e, PluginEvent::DeclsChanged)));
    let ids: Vec<&str> = p.decls.commands.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["soften", "late"]);
    assert_eq!(p.decls.commands[0].tooltip, "Sets every velocity to 40.");
    for t in ["entry ran 42", "on_load", "opened Song"] {
        assert!(log_has(&m, "test.velocity", t), "{t}");
    }
    // A command: its transaction comes back for the UI thread.
    m.run_command("test.velocity", "soften", Some(world(&doc, &mut h)))
        .unwrap();
    let ev = settle_with(&mut m, &prefs, &mut || Some(world(&doc, &mut h)));
    let mut applied = false;
    for e in ev {
        match e {
            PluginEvent::Commits {
                commits: mut c,
                command,
                ..
            } => {
                assert_eq!(command, "soften");
                c.apply(&mut h, &mut doc).unwrap();
                applied = true;
            }
            PluginEvent::Notice { text, error } => {
                assert!(!error && text.contains("softened 10"), "{text}");
            }
            _ => {}
        }
    }
    assert!(applied);
    assert_eq!(h.undo_label(), Some("Soften Notes"));
    // Registering after loading is refused, in the log, nothing changes.
    m.run_command("test.velocity", "late", None).unwrap();
    let ev = settle_with(&mut m, &prefs, &mut || Some(world(&doc, &mut h)));
    assert!(
        ev.iter()
            .any(|e| matches!(e, PluginEvent::Notice { error: true, .. }))
    );
    assert!(log_has(
        &m,
        "test.velocity",
        "must be registered while the plugin loads"
    ));
    assert_eq!(m.plugin("test.velocity").unwrap().decls.commands.len(), 2);
    // Reload: on_unload, a new state, the same registrations (not doubled).
    m.reload("test.velocity", &prefs);
    settle_with(&mut m, &prefs, &mut || Some(world(&doc, &mut h)));
    let p = m.plugin("test.velocity").unwrap();
    assert_eq!(p.status, Status::Ready);
    assert_eq!(p.decls.commands.len(), 2, "no double registration");
    assert!(log_has(&m, "test.velocity", "bye"));
    // Closing the project calls on_project_close.
    m.project_closed();
    settle_with(&mut m, &prefs, &mut || Some(world(&doc, &mut h)));
    assert!(log_has(&m, "test.velocity", "closed"));
    // Disabling unloads; withdrawing a capability reloads with less.
    m.set_enabled("test.velocity", false, &mut prefs);
    settle_with(&mut m, &prefs, &mut || Some(world(&doc, &mut h)));
    let p = m.plugin("test.velocity").unwrap();
    assert_eq!(p.status, Status::Disabled);
    assert!(p.decls.commands.is_empty());
    assert!(!prefs.installed["test.velocity"].enabled);
    // Remove: gone from the store and the settings.
    m.remove("test.velocity", &mut prefs).unwrap();
    assert!(m.plugin("test.velocity").is_none());
    assert!(!prefs.installed.contains_key("test.velocity"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn broken_plugins_fail_with_their_reason_and_never_touch_the_project() {
    let root = temp("broken");
    let scripts = Scripts::default();
    let mut m = PluginManager::new(Some(root.join("plugins")), scripts);
    let mut prefs = PluginPrefs {
        enable_on_install: true,
        ..PluginPrefs::default()
    };
    // A syntax error: Failed, with the file and line.
    let syntax = package(
        &root.join("syntax"),
        &MANIFEST.replace("test.velocity", "test.syntax"),
        &[("main.lua", "local x = \nfunction(")],
    );
    m.install(&syntax, false, &mut prefs).unwrap();
    // A newer API: Incompatible, never loaded.
    let newer = package(
        &root.join("newer"),
        &MANIFEST
            .replace("test.velocity", "test.newer")
            .replace("\"0.1\"", "\"0.99\""),
        &[("main.lua", "print('should not run')")],
    );
    m.install(&newer, false, &mut prefs).unwrap();
    // A hook that edits: the change is not applied.
    let editing = package(
        &root.join("edit"),
        &MANIFEST.replace("test.velocity", "test.edit"),
        &[(
            "main.lua",
            "function on_project_open(p) local tx = plum.transaction{} tx:delete_notes(p:track(1):notes()) tx:commit() end",
        )],
    );
    m.install(&editing, false, &mut prefs).unwrap();
    // A register() that throws: Failed, no commands.
    let throws = package(
        &root.join("throws"),
        &MANIFEST.replace("test.velocity", "test.throws"),
        &[(
            "main.lua",
            "function register(p) p.command{ id = 'a', name = 'A', run = function() end } error('nope') end",
        )],
    );
    m.install(&throws, false, &mut prefs).unwrap();
    let (doc, mut h) = project();
    let fp = doc.fingerprint();
    m.project_opened(world(&doc, &mut h));
    let ev = {
        let (d2, h2) = (&doc, &mut h);
        settle_with(&mut m, &prefs, &mut || Some(world(d2, h2)))
    };
    assert!(
        ev.iter().all(|e| !matches!(e, PluginEvent::Commits { .. })),
        "nothing reaches the project"
    );
    assert_eq!(doc.fingerprint(), fp);
    match &m.plugin("test.syntax").unwrap().status {
        Status::Failed(why) => assert!(why.contains("main.lua, line 2"), "{why}"),
        s => panic!("{s:?}"),
    }
    match &m.plugin("test.newer").unwrap().status {
        Status::Incompatible(why) => assert!(why.contains("needs scripting API 0.99"), "{why}"),
        s => panic!("{s:?}"),
    }
    assert!(!log_has(&m, "test.newer", "should not run"));
    assert_eq!(m.plugin("test.edit").unwrap().status, Status::Ready);
    assert!(log_has(
        &m,
        "test.edit",
        "lifecycle hooks may only read the project"
    ));
    let t = m.plugin("test.throws").unwrap();
    assert!(
        matches!(&t.status, Status::Failed(w) if w.contains("nope")),
        "{:?}",
        t.status
    );
    assert!(t.decls.commands.is_empty());
    // A broken manifest on disk is listed as such.
    std::fs::create_dir_all(root.join("plugins/test.bad")).unwrap();
    std::fs::write(root.join("plugins/test.bad/plum-plugin.toml"), "id = 5").unwrap();
    m.refresh(&prefs);
    assert!(matches!(
        m.plugin("test.bad").unwrap().status,
        Status::Broken(_)
    ));
    // Plugins switched off: everything stops.
    prefs.run_plugins = false;
    m.stop_all(&prefs);
    settle(&mut m, &prefs);
    assert!(
        m.plugins()
            .iter()
            .all(|p| matches!(p.status, Status::Off | Status::Broken(_) | Status::Disabled))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shared_logs_are_redacted() {
    let home = "C:\\Users\\Ouko";
    let text = "Error in C:\\Users\\Ouko\\AppData\\Roaming\\PlumSpirit\\plugins\\x\\main.lua:12: boom; also D:\\Music\\Ouko song.mid and /home/ouko/plug/a.lua (user Ouko)";
    let r = redact(text, Some(home), Some("Ouko"));
    assert!(!r.to_lowercase().contains("ouko"), "{r}");
    assert!(r.contains("main.lua:12"), "{r}");
    assert!(r.contains("<path>/a.lua"), "{r}");
    let mut log = super::plugins::PluginLog::default();
    log.push(Level::Error, text);
    let rep = log.report(
        "Plugin test.velocity 1.0.0 (C:\\Users\\Ouko)",
        Some(home),
        Some("Ouko"),
    );
    assert!(!rep.to_lowercase().contains("ouko"), "{rep}");
    assert!(rep.contains("[error]"));
    for _ in 0..(super::plugins::LOG_LINES + 10) {
        log.push(Level::Info, "x");
    }
    assert_eq!(log.len(), super::plugins::LOG_LINES, "bounded");
}
