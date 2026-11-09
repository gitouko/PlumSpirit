//! Actual headless App integration; no GUI/physical-input acceptance claim.
use super::commands::Command;
use super::test_support::Harness;
use super::*;
use crate::lua::{
    Scripts,
    plugins::{PluginManager, PluginPrefs, Status},
};
use plumspirit_engine::model::{Bpm, PlumDocument};

const ID: &str = "plumspirit.example.metadata-notebook";
fn settle(h: &mut Harness) {
    let start = std::time::Instant::now();
    loop {
        h.poll_background_jobs();
        h.app.poll_lua_plugins(&h.ctx);
        if !h.app.lua_plugins.as_ref().unwrap().is_busy() && h.app.job.is_none() {
            return;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(8),
            "App metadata worker timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
fn run(h: &mut Harness, id: &str) {
    let w = h.app.lua_world(true);
    h.app
        .lua_plugins
        .as_mut()
        .unwrap()
        .run_command(ID, id, w)
        .unwrap();
    settle(h);
}

#[test]
fn installed_notebook_uses_real_app_apply_undo_redo_and_background_lup_save() {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let root = base.join(format!(
        "plumspirit-app-metadata-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut doc = PlumDocument::new_project("App notebook".into(), Bpm::DEFAULT, 960);
    doc.project_mut()
        .groups
        .add(plumspirit_engine::model::GroupInfo::default());
    let mut h = Harness::new(doc);
    let mut manager = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    let mut prefs = PluginPrefs {
        folder: Some(root.join("plugins")),
        ..Default::default()
    };
    let example = crate::lua::examples::by_id(ID).unwrap();
    manager.install_example(example, &mut prefs).unwrap();
    assert_eq!(
        prefs.installed[ID].origin,
        crate::lua::package::Origin::Example
    );
    for cap in ["project.read", "metadata.edit"] {
        manager.set_granted(ID, cap, true, &mut prefs);
    }
    manager.set_enabled(ID, true, &mut prefs);
    h.app.settings.lua_plugins = prefs;
    h.app.lua_plugins = Some(manager);
    h.app.lua_plugins_started = true;
    settle(&mut h);
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
    let midi = plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap();
    run(&mut h, "remember");
    let table = h.doc().project().metadata.clone();
    assert_eq!(table.get(ID).unwrap().version, 2);
    assert!(!table.is_empty());
    assert!(h.command(Command::Undo));
    assert!(h.doc().project().metadata.is_empty());
    assert!(h.command(Command::Redo));
    assert_eq!(h.doc().project().metadata, table);
    run(&mut h, "remember-track");
    let table = h.doc().project().metadata.clone();
    assert_eq!(table.len(), 2);
    assert!(h.command(Command::Undo));
    assert_eq!(h.doc().project().metadata.len(), 1);
    assert!(h.command(Command::Redo));
    assert_eq!(h.doc().project().metadata, table);
    run(&mut h, "remember-group");
    let table = h.doc().project().metadata.clone();
    assert_eq!(table.len(), 3);
    assert!(h.command(Command::Undo));
    assert_eq!(h.doc().project().metadata.len(), 2);
    assert!(h.command(Command::Redo));
    assert_eq!(h.doc().project().metadata, table);
    let file = root.join("actual.lup");
    h.app.open.as_mut().unwrap().source = DocSource::Project(file.clone());
    assert!(h.command(Command::Save));
    settle(&mut h);
    let (reopened, _) = plumspirit_engine::native::load_project(&file, Default::default()).unwrap();
    assert_eq!(reopened.project().metadata, table);
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(&reopened).unwrap(),
        midi
    );
    h.app
        .install_document(reopened, DocSource::Project(file), true);
    settle(&mut h);
    run(&mut h, "inspect");
    run(&mut h, "clear-group");
    assert_eq!(h.doc().project().metadata.len(), 2);
    run(&mut h, "clear-track");
    assert_eq!(h.doc().project().metadata.len(), 1);
    run(&mut h, "clear");
    assert!(h.doc().project().metadata.is_empty());
    assert!(h.command(Command::Undo));
    assert_eq!(h.doc().project().metadata.len(), 1);
    assert!(h.command(Command::Undo));
    assert_eq!(h.doc().project().metadata.len(), 2);
    assert!(h.command(Command::Undo));
    assert_eq!(h.doc().project().metadata, table);
    drop(h);
    let resolved = root.canonicalize().unwrap();
    assert_eq!(resolved.parent(), Some(base.as_path()));
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("plumspirit-app-metadata-")
    );
    std::fs::remove_dir_all(resolved).unwrap();
}
