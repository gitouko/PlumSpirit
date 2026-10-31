//! B1-0034 / B1-0035: plugin settings in both scopes (user files, project
//! data), layout migration, damaged and newer files, presets,
//! notifications, and command options.

use super::plugin_api::{NoticeKind, PluginMeta};
use super::plugins::{PluginEvent, PluginManager, PluginPrefs, Status};
use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "plumspirit-config-test-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn host(data: Option<&Path>, version: u64, entry: &str) -> Host {
    let h = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    h.set_plugin(PluginMeta {
        id: "test.cfg".into(),
        name: "Config Test".into(),
        version: "1.0".into(),
        root: std::env::temp_dir(),
        data_dir: data.map(Path::to_path_buf),
        config_version: version,
    })
    .unwrap();
    h.run("main.lua", entry).unwrap();
    h.take_plugin_out();
    h
}

fn text(h: &Host, src: &str) -> String {
    match h.run("=t", src) {
        Ok(Plain::String(s)) => s,
        other => panic!("{other:?}"),
    }
}

#[test]
fn user_settings_and_presets_persist_between_sessions() {
    let dir = temp("user");
    let h = host(Some(&dir), 1, "");
    h.run(
        "=set",
        "plum.config.set('speed', 3) plum.config.set('t', { a = { 1, 2, 3 }, b = true, s = 'x' }) \
         plum.config.set('gone', 1) plum.config.remove('gone') \
         plum.presets.save('Soft', { level = 0.25 }) plum.presets.save('Loud', { level = 1 })",
    )
    .unwrap();
    let stored = std::fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(stored.contains("\"version\":1"), "{stored}");
    // A new session (a new Lua state) reads them back.
    let h = host(Some(&dir), 1, "");
    assert_eq!(
        text(
            &h,
            "local t = plum.config.get('t') return plum.config.get('speed') .. ' ' .. t.a[3] .. ' ' .. tostring(t.b) .. ' ' .. t.s \
             .. ' ' .. plum.config.get('gone', 'none') .. ' ' .. table.concat(plum.config.keys(), ',') \
             .. ' ' .. table.concat(plum.presets.list(), ',') .. ' ' .. plum.presets.load('Soft').level \
             .. ' ' .. tostring(plum.presets.load('Missing'))"
        ),
        "3 3 true x none speed,t Loud,Soft 0.25 nil"
    );
    assert_eq!(
        text(
            &h,
            "return tostring(plum.presets.delete('Loud')) .. table.concat(plum.presets.list(), ',')"
        ),
        "trueSoft"
    );
    // What cannot be stored is refused with a sentence; a failed run saves nothing.
    for bad in [
        "plum.config.set('f', function() end)",
        "plum.config.set('n', 0/0)",
        "plum.config.set('bad key!', 1)",
        "plum.presets.save('../escape', 1)",
        "plum.presets.save('con', 1)",
        "plum.presets.load('AUX')",
    ] {
        let e = h.run("=bad", bad).unwrap_err();
        assert_eq!(e.category, Category::Runtime, "{bad}: {}", e.message);
    }
    assert!(
        h.run("=fail", "plum.config.set('speed', 9) error('stop')")
            .is_err()
    );
    assert_eq!(text(&h, "return tostring(plum.config.get('speed'))"), "3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn older_layouts_migrate_and_newer_or_damaged_files_are_left_alone() {
    let dir = temp("migrate");
    std::fs::write(
        dir.join("config.json"),
        r#"{"version":1,"values":{"old":5}}"#,
    )
    .unwrap();
    let h = host(
        Some(&dir),
        2,
        "function migrate_config(old, v) return { new = v.old * 2, from = old } end",
    );
    assert_eq!(
        text(
            &h,
            "return plum.config.get('new') .. '/' .. plum.config.get('from') .. '/' .. plum.config.version"
        ),
        "10/1/2"
    );
    let out = h.take_plugin_out();
    assert!(
        out.notes
            .iter()
            .any(|n| n.contains("migrated from layout 1 to 2"))
    );
    let stored = std::fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(
        stored.contains("\"version\":2") && stored.contains("\"new\":10"),
        "{stored}"
    );
    // Saved by a newer version of the plugin: not used, not overwritten.
    let newer = r#"{"version":3,"values":{"x":1}}"#;
    std::fs::write(dir.join("config.json"), newer).unwrap();
    let h = host(Some(&dir), 2, "");
    assert_eq!(text(&h, "return tostring(plum.config.get('x'))"), "nil");
    assert!(
        h.take_plugin_out()
            .notes
            .iter()
            .any(|n| n.contains("newer version"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("config.json")).unwrap(),
        newer
    );
    // Damaged: set aside, the plugin starts empty and keeps working.
    std::fs::write(dir.join("config.json"), "{oops").unwrap();
    let h = host(Some(&dir), 2, "");
    assert_eq!(text(&h, "plum.config.set('a', 1) return 'ok'"), "ok");
    assert!(
        h.take_plugin_out()
            .notes
            .iter()
            .any(|n| n.contains("damaged"))
    );
    let aside = std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with("config.json.damaged-")
    });
    assert!(aside, "the damaged file is kept beside");
    let _ = std::fs::remove_dir_all(&dir);
}

fn project(data: Vec<(String, String)>) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..4u64)
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
    d.project_mut().plugin_data = data;
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

fn world(doc: &Arc<PlumDocument>, h: &mut History) -> World {
    h.observe_ids(doc);
    World::new(Arc::clone(doc), h.incarnations().snapshot())
}

#[test]
fn project_settings_come_from_the_project_and_go_back_to_the_app() {
    let (doc, mut hist) = project(vec![(
        "test.cfg".into(),
        r#"{"version":1,"values":{"k":7}}"#.into(),
    )]);
    let h = host(None, 1, "");
    // No project: a sentence.
    let e = h
        .run("=none", "return plum.config.project.get('k')")
        .unwrap_err();
    assert!(e.message.contains("No project is open"), "{}", e.message);
    h.set_world(Some(world(&doc, &mut hist)));
    assert_eq!(
        text(&h, "return tostring(plum.config.project.get('k'))"),
        "7"
    );
    assert_eq!(
        h.take_plugin_out().project_data,
        None,
        "reading changes nothing"
    );
    h.run(
        "=inc",
        "plum.config.project.set('k', plum.config.project.get('k') + 1)",
    )
    .unwrap();
    let data = h.take_plugin_out().project_data.unwrap();
    assert!(
        data.contains("\"k\":8") && data.contains("\"version\":1"),
        "{data}"
    );
    // A failed run hands nothing over.
    assert!(
        h.run("=fail", "plum.config.project.set('k', 1) error('x')")
            .is_err()
    );
    assert_eq!(h.take_plugin_out().project_data, None);
    // Removing the last value removes the plugin's data.
    h.run("=rm", "plum.config.project.remove('k')").unwrap();
    assert_eq!(h.take_plugin_out().project_data.as_deref(), Some(""));
    // Without a settings folder user values last for the session only.
    h.run("=s", "plum.config.set('mem', 1)").unwrap();
    assert_eq!(text(&h, "return tostring(plum.config.get('mem'))"), "1");
    let e = h.run("=p", "plum.presets.save('x', 1)").unwrap_err();
    assert!(e.message.contains("settings folder"), "{}", e.message);
}

#[test]
fn notifications_are_capped_cleaned_and_checked() {
    let h = host(None, 1, "");
    h.run(
        "=n",
        "plum.ui.notify('Done.') plum.ui.notify('Careful\\n!', 'warning') plum.ui.notify('   ') \
         for i = 1, 10 do plum.ui.notify('n' .. i, 'error') end",
    )
    .unwrap();
    let n = h.take_plugin_out().notices;
    assert_eq!(n.len(), plugin_api::MAX_NOTICES);
    assert_eq!(n[0], ("Done.".into(), NoticeKind::Info));
    assert_eq!(n[1], ("Careful!".into(), NoticeKind::Warning));
    assert_eq!(n[2].1, NoticeKind::Error);
    let e = h.run("=bad", "plum.ui.notify('x', 'loud')").unwrap_err();
    assert!(
        e.message.contains("\"info\", \"warning\" or \"error\""),
        "{}",
        e.message
    );
}

const OPTIONS_PLUGIN: &str = r#"
function register(p)
  p.command{ id = "level", name = "Set Level", tooltip = "Sets every velocity.",
    params = { { key = "amount", label = "Velocity", kind = "int", min = 1, max = 127, default = 40 },
               { key = "label", kind = "text", default = "Level" } },
    run = function(ctx)
      local t = plum.project.current():track(1)
      local tx = plum.transaction{ label = ctx.params.label }
      tx:modify_notes(t:notes(), { velocity = ctx.params.amount })
      tx:commit()
      plum.config.set('last', ctx.params.amount)
      plum.ui.notify('Set to ' .. ctx.params.amount .. ' (' .. ctx.params.label .. ').')
    end }
end
"#;

#[test]
fn command_options_reach_the_command_and_its_settings_are_kept() {
    let root = temp("options");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("plum-plugin.toml"),
        "id = \"test.level\"\nname = \"Level\"\nversion = \"1.0.0\"\napi = \"0.1\"\nentry = \"main.lua\"\ncapabilities = [\"project.read\", \"notes.edit\"]\n",
    )
    .unwrap();
    std::fs::write(src.join("main.lua"), OPTIONS_PLUGIN).unwrap();
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    m.set_data_root(Some(root.join("plugin-data")));
    let mut prefs = PluginPrefs::default();
    m.install(&src, false, &mut prefs).unwrap();
    m.set_granted("test.level", "project.read", true, &mut prefs);
    m.set_granted("test.level", "notes.edit", true, &mut prefs);
    m.set_enabled("test.level", true, &mut prefs);
    settle(&mut m, &prefs);
    let p = m.plugin("test.level").unwrap();
    assert_eq!(
        p.status,
        Status::Ready,
        "{:?}",
        p.log.lines().map(|l| l.text.clone()).collect::<Vec<_>>()
    );
    assert_eq!(p.decls.commands[0].params.len(), 2);
    let (doc, mut hist) = project(Vec::new());
    for (params, velocity, label) in [
        ("{amount=90,label=\"Loud\",}".to_owned(), 90, "Loud"),
        ("nil".to_owned(), 40, "Level"),
    ] {
        m.run_command_with("test.level", "level", params, Some(world(&doc, &mut hist)))
            .unwrap();
        let events = settle(&mut m, &prefs);
        let notice = events.iter().find_map(|e| match e {
            PluginEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(
            notice.as_deref(),
            Some(format!("Level: Set to {velocity} ({label}).").as_str())
        );
        let commits = events
            .into_iter()
            .find_map(|e| match e {
                PluginEvent::Commits { commits, .. } => Some(commits),
                _ => None,
            })
            .expect("the command committed");
        let after = commits.result();
        let t = after.track(0).unwrap();
        let v: Vec<u8> = t.notes.iter().map(|n| n.attrs.velocity()).collect();
        assert!(v.iter().all(|&x| x == velocity), "{v:?}");
    }
    let stored = std::fs::read_to_string(root.join("plugin-data/test.level/config.json")).unwrap();
    assert!(stored.contains("\"last\":40"), "{stored}");
    let _ = std::fs::remove_dir_all(&root);
}

fn settle(m: &mut PluginManager, prefs: &PluginPrefs) -> Vec<PluginEvent> {
    let t = Instant::now();
    let mut out = Vec::new();
    loop {
        out.extend(m.poll(prefs));
        if !m.is_busy() {
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
