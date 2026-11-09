use super::*;
use crate::lua::plugins::{PluginEvent, PluginManager, PluginPrefs, Status};
use crate::lua::{Scripts, World};
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, PlumDocument};
use std::path::PathBuf;

struct Rig {
    manager: PluginManager,
    prefs: PluginPrefs,
    root: PathBuf,
}
impl Rig {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "plumspirit-service-broker-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            manager: PluginManager::new(Some(root.join("plugins")), Scripts::default()),
            prefs: PluginPrefs::default(),
            root,
        }
    }
    fn install(&mut self, id: &str, caps: &[&str], deps: &[&str], source: &str) {
        let folder = self.root.join(id);
        std::fs::create_dir_all(&folder).unwrap();
        let quote = |values: &[&str]| {
            values
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(",")
        };
        let manifest = format!(
            "id=\"{id}\"\nname=\"Broker test\"\nversion=\"1.0.0\"\napi=\"0.2\"\nentry=\"main.lua\"\ncapabilities=[{}]\nservice_dependencies=[{}]\n",
            quote(caps),
            quote(deps)
        );
        std::fs::write(folder.join("plum-plugin.toml"), manifest).unwrap();
        std::fs::write(folder.join("main.lua"), source).unwrap();
        self.manager
            .install(&folder, false, &mut self.prefs)
            .unwrap();
        for cap in caps {
            self.manager.set_granted(id, cap, true, &mut self.prefs);
        }
        self.manager.set_enabled(id, true, &mut self.prefs);
        self.settle();
        assert_eq!(
            self.manager.plugin(id).unwrap().status,
            Status::Ready,
            "{}",
            self.log(id).join("\n")
        );
    }
    fn settle(&mut self) -> Vec<PluginEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        loop {
            events.extend(self.manager.poll(&self.prefs));
            if !self.manager.is_busy() {
                return events;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "broker settles");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn log(&self, id: &str) -> Vec<String> {
        self.manager
            .plugin(id)
            .unwrap()
            .log
            .lines()
            .map(|l| l.text.clone())
            .collect()
    }
    fn command(&mut self, id: &str, world: Option<World>) -> Vec<PluginEvent> {
        self.manager.run_command(id, "run", world).unwrap();
        self.settle()
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.manager.stop_all(&self.prefs);
        let root = self.root.canonicalize().unwrap();
        let temp = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(root.parent(), Some(temp.as_path()));
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plumspirit-service-broker-")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[path = "format_services_tests.rs"]
mod format_services;
fn provider() -> &'static str {
    r#"
    function register(p)
        p.service{id='double',version='1.2',run=function(data,ctx)
            if plum.has('project.read') then assert(plum.project.current()==nil) end
            assert(not pcall(function() plum.plugin.__service() end))
            return {value=data.value*2,tick=data.tick,seed=ctx.seed}
        end}
    end
"#
}
fn client(body: &str) -> String {
    format!(
        "function register(p) p.command{{id='run',name='Run',run=function(ctx) {body} end}} end"
    )
}
#[test]
fn actual_installed_client_provider_reply_is_versioned_exact_and_plain() {
    let mut r = Rig::new();
    r.install("test.provider", &["services.provide"], &[], provider());
    r.install("test.client",&["services.call"],&["test.provider/double@1.1"],&client(r#"
        local out=plum.services.call('test.provider/double',{value=21,tick=9007199254740993},{seed=17})
        assert(out.value==42 and out.tick==9007199254740993 and math.type(out.tick)=='integer' and out.seed==17)
        print('COMPOSED EXACT 42')
    "#));
    r.command("test.client", None);
    assert!(
        r.log("test.client")
            .iter()
            .any(|l| l.contains("COMPOSED EXACT 42")),
        "{:?}",
        r.log("test.client")
    );
}
#[test]
fn services_compose_inside_one_existing_native_transaction_and_undo_redo() {
    let mut r = Rig::new();
    r.install("test.provider", &["services.provide"], &[], provider());
    r.install("test.client",&["services.call","project.read","notes.edit"],&["test.provider/double@1.0"],&client(r#"
        local tx=plum.transaction{label='Service composition'}
        local out=plum.services.call('test.provider/double',{value=32,tick=0})
        tx:insert_notes(plum.project.current():track(1),{{start=out.tick,gate=120,key=60,velocity=out.value}})
        tx:commit()
    "#));
    let mut doc = Arc::new(PlumDocument::new_project(
        "Composition".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot()).with_info(
        crate::lua::handles::ProjectInfo {
            revision: history.current_state(),
            ..Default::default()
        },
    );
    r.manager.project_opened(world.clone());
    r.settle();
    let events = r.command("test.client", Some(world));
    let mut count = 0;
    for event in events {
        if let PluginEvent::Commits { mut commits, .. } = event {
            commits.apply(&mut history, &mut doc).unwrap();
            count += 1;
        }
    }
    assert_eq!(count, 1, "{:?}", r.log("test.client"));
    assert_eq!(doc.note_count(), 1);
    assert_eq!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .next()
            .unwrap()
            .attrs
            .velocity(),
        64
    );
    history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.note_count(), 0);
    history.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.note_count(), 1);
}
#[test]
fn undeclared_disabled_and_incompatible_service_calls_are_denied() {
    let mut r = Rig::new();
    r.install("test.provider", &["services.provide"], &[], provider());
    r.install(
        "test.undeclared",
        &["services.call"],
        &[],
        &client("plum.services.call('test.provider/double',{value=1})"),
    );
    let events = r.command("test.undeclared", None);
    assert!(events.iter().any(
        |e| matches!(e,PluginEvent::Notice {text,error:true} if text.contains("not declared"))
    ));
    r.install(
        "test.incompatible",
        &["services.call"],
        &["test.provider/double@2.0"],
        &client("plum.services.call('test.provider/double',{value=1})"),
    );
    let events = r.command("test.incompatible", None);
    assert!(events.iter().any(
        |e| matches!(e,PluginEvent::Notice {text,error:true} if text.contains("incompatible"))
    ));
    r.install(
        "test.client",
        &["services.call"],
        &["test.provider/double@1.0"],
        &client("plum.services.call('test.provider/double',{value=1})"),
    );
    r.manager.set_enabled("test.provider", false, &mut r.prefs);
    r.settle();
    let events = r.command("test.client", None);
    assert!(events.iter().any(
        |e| matches!(e,PluginEvent::Notice {text,error:true} if text.contains("not enabled"))
    ));
}
#[test]
fn global_cross_root_wait_cycles_fail_instead_of_deadlocking() {
    let mut r = Rig::new();
    let make = |other: &str| {
        format!(
            r#"
        function register(p)
            p.service{{id='double',version='1.0',run=function(data) return {{value=data.value*2}} end}}
            p.command{{id='run',name='Run',run=function() local out=plum.services.call('{other}/double',{{value=21}});print('DONE '..out.value) end}}
        end
    "#
        )
    };
    r.install(
        "test.alpha",
        &["services.call", "services.provide"],
        &["test.beta/double@1.0"],
        &make("test.beta"),
    );
    r.install(
        "test.beta",
        &["services.call", "services.provide"],
        &["test.alpha/double@1.0"],
        &make("test.alpha"),
    );
    r.manager.run_command("test.alpha", "run", None).unwrap();
    r.manager.run_command("test.beta", "run", None).unwrap();
    let events = r.settle();
    assert!(events.iter().any(
        |e| matches!(e,PluginEvent::Notice {text,error:true} if text.contains("deadlock cycle"))
    ));
    assert!(
        r.log("test.alpha")
            .iter()
            .chain(r.log("test.beta").iter())
            .any(|s| s.contains("DONE 42"))
    );
}
#[test]
fn nested_provider_calls_need_delegated_permission_and_cannot_reenter_owners() {
    let mut r = Rig::new();
    r.install("test.leaf", &["services.provide"], &[], provider());
    r.install(
        "test.middle",
        &["services.provide", "services.call"],
        &["test.leaf/double@1.0"],
        r#"
        function register(p) p.service{id='double',version='1.0',run=function(data)
            local out=plum.services.call('test.leaf/double',data);return {value=out.value*2}
        end} end
    "#,
    );
    r.install("test.client",&["services.call"],&["test.middle/double@1.0"],&client("local out=plum.services.call('test.middle/double',{value=21});assert(out.value==84);print('NESTED 84')"));
    r.command("test.client", None);
    assert!(r.log("test.client").iter().any(|l| l.contains("NESTED 84")));
    r.manager
        .set_granted("test.middle", "services.call", false, &mut r.prefs);
    r.settle();
    let events = r.command("test.client", None);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PluginEvent::Notice { error: true, .. }))
    );
}
#[test]
fn manifest_dependency_versions_are_bounded_and_legacy_text_is_unchanged() {
    for bad in [
        "",
        "p/a@1.0",
        "test.owner/service",
        "test.owner/service@-1.0",
        "test.owner/a/b@1.0",
        "test.owner/service@1.0.0",
    ] {
        assert!(Dependency::parse(bad).is_err(), "{bad}");
    }
    let source = "id=\"test.owner\"\nname=\"Owner\"\nversion=\"1.0.0\"\napi=\"0.2\"\ncapabilities=[\"services.call\"]\nservice_dependencies=[\"test.provider/double@1.0\"]\n";
    let manifest = crate::lua::package::Manifest::parse(source).unwrap();
    assert_eq!(
        crate::lua::package::Manifest::parse(&manifest.to_text()).unwrap(),
        manifest
    );
    let legacy = crate::lua::package::Manifest::parse(
        "id=\"test.legacy\"\nname=\"Legacy\"\nversion=\"1.0.0\"\napi=\"0.1\"\n",
    )
    .unwrap();
    assert!(!legacy.to_text().contains("service_dependencies"));
    let duplicate = source.replace(
        "[\"test.provider/double@1.0\"]",
        "[\"test.provider/double@1.0\",\"test.provider/double@1.1\"]",
    );
    assert!(crate::lua::package::Manifest::parse(&duplicate).is_err());
}

#[test]
fn provider_capabilities_are_intersected_and_restored_after_reply() {
    let mut r = Rig::new();
    r.install("test.provider",&["services.provide","services.call","project.read","notes.edit"],&[],r#"
        function register(p)
            p.service{id='inspect',version='1.0',run=function(data)
                assert(plum.has('services.call'))
                assert(not plum.has('project.read') and not plum.has('notes.edit') and not plum.has('services.provide'))
                assert(plum.capabilities()['project.read']==false)
                assert(not pcall(function() return plum.project.current() end))
                return {confined=true}
            end}
            p.command{id='run',name='Run',run=function()
                assert(plum.has('project.read') and plum.has('notes.edit') and plum.has('services.provide'))
                print('BASE AUTHORITY RESTORED')
            end}
        end
    "#);
    r.install(
        "test.client",
        &["services.call"],
        &["test.provider/inspect@1.0"],
        &client(
            "assert(plum.services.call('test.provider/inspect',{}).confined);print('CONFINED')",
        ),
    );
    r.command("test.client", None);
    assert!(
        r.log("test.client").iter().any(|l| l.contains("CONFINED")),
        "{:?}",
        r.log("test.client")
    );
    r.command("test.provider", None);
    assert!(
        r.log("test.provider")
            .iter()
            .any(|l| l.contains("BASE AUTHORITY RESTORED"))
    );
}
#[test]
fn service_timeout_and_disable_cancel_workers_and_release_queued_calls() {
    let mut r = Rig::new();
    r.install(
        "test.provider",
        &["services.provide"],
        &[],
        r#"
        function register(p) p.service{id='slow',version='1.0',run=function(data)
            plum.progress(.5,'provider busy');while true do plum.yield() end
        end} end
    "#,
    );
    r.install(
        "test.client",
        &["services.call"],
        &["test.provider/slow@1.0"],
        &client("plum.services.call('test.provider/slow',{}, {timeout_ms=40})"),
    );
    let events = r.command("test.client", None);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PluginEvent::Notice { error: true, .. }))
    );
    assert!(!r.manager.plugin("test.provider").unwrap().is_busy());
    r.install(
        "test.other",
        &["services.call"],
        &["test.provider/slow@1.0"],
        &client("plum.services.call('test.provider/slow',{}, {timeout_ms=250})"),
    );
    r.manager.run_command("test.other", "run", None).unwrap();
    let start = Instant::now();
    loop {
        r.manager.poll(&r.prefs);
        if r.manager
            .plugin("test.provider")
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    r.manager.set_enabled("test.provider", false, &mut r.prefs);
    let events = r.settle();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PluginEvent::Notice { error: true, .. }))
    );
}
#[test]
fn bound_project_calls_drop_on_close_and_nested_owner_cycles_are_rejected() {
    let mut r = Rig::new();
    r.install(
        "test.provider",
        &["services.provide"],
        &[],
        r#"
        function register(p) p.service{id='slow',version='1.0',run=function(data)
            plum.progress(.5,'bound provider');while true do plum.yield() end
        end} end
    "#,
    );
    r.install(
        "test.client",
        &["services.call", "project.read"],
        &["test.provider/slow@1.0"],
        &client("plum.services.call('test.provider/slow',{})"),
    );
    let doc = Arc::new(PlumDocument::new_project(
        "Bound call".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    r.manager.project_opened(world.clone());
    r.settle();
    r.manager
        .run_command("test.client", "run", Some(world))
        .unwrap();
    let start = Instant::now();
    loop {
        r.manager.poll(&r.prefs);
        if r.manager
            .plugin("test.provider")
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    r.manager.project_closed();
    r.settle();
    assert_eq!(Arc::strong_count(&doc), 1);
    r.install("test.cycle",&["services.provide","services.call"],&["test.cycle/self@1.0"],r#"
        function register(p)
            p.service{id='self',version='1.0',run=function(data) return data end}
            p.command{id='run',name='Run',run=function() plum.services.call('test.cycle/self',{}) end}
        end
    "#);
    let events = r.command("test.cycle", None);
    assert!(
        events
            .iter()
            .any(|e| matches!(e,PluginEvent::Notice {text,error:true} if text.contains("reenter")))
    );
}
#[test]
fn root_tree_and_frozen_declaration_budgets_cannot_be_reset_from_lua() {
    let mut r = Rig::new();
    r.install("test.provider", &["services.provide"], &[], provider());
    r.install("test.client",&["services.call","services.provide","project.read","project.observe"],&["test.provider/double@1.0"],&client(r#"
        for i=1,64 do assert(plum.services.call('test.provider/double',{value=i}).value==i*2) end
        assert(not pcall(function() plum.services.call('test.provider/double',{value=1}) end))
        plum.plugin.__phase('load')
        assert(not pcall(function() plum.plugin.service{id='late',version='1.0',run=function()end} end))
        assert(not pcall(function() plum.plugin.observe{id='late',events={'undo'},run=function()end} end))
        print('TREE AND DECLARATIONS BOUNDED')
    "#));
    r.command("test.client", None);
    assert!(
        r.log("test.client")
            .iter()
            .any(|l| l.contains("TREE AND DECLARATIONS BOUNDED")),
        "{:?}",
        r.log("test.client")
    );
}

#[test]
fn provider_queue_budget_and_four_computing_worker_limit_are_enforced() {
    let mut r = Rig::new();
    let slow = r#"function register(p) p.service{id='work',version='1.0',run=function(data)
        plum.progress(.5,'computing');local stop=os.clock()+0.06;while os.clock()<stop do plum.yield() end;return {value=42}
    end} end"#;
    r.install("test.provider", &["services.provide"], &[], slow);
    for i in 0..12 {
        let id = format!("test.queue{i}");
        r.install(
            &id,
            &["services.call"],
            &["test.provider/work@1.0"],
            &client("plum.services.call('test.provider/work',{})"),
        );
    }
    for i in 0..12 {
        r.manager
            .run_command(&format!("test.queue{i}"), "run", None)
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(5));
    let events = r.settle();
    assert!(events.iter().any(|e|matches!(e,PluginEvent::Notice {text,error:true} if text.contains("eight-call queue budget"))));
    for i in 0..5 {
        let provider = format!("test.compute{i}");
        let consumer = format!("test.consumer{i}");
        r.install(&provider, &["services.provide"], &[], slow);
        let dep = format!("{provider}/work@1.0");
        let body = format!("plum.services.call('{provider}/work',{{}})");
        r.install(&consumer, &["services.call"], &[&dep], &client(&body));
    }
    for i in 0..5 {
        r.manager
            .run_command(&format!("test.consumer{i}"), "run", None)
            .unwrap();
    }
    let start = Instant::now();
    let mut saw_computing = false;
    while r.manager.is_busy() {
        r.manager.poll(&r.prefs);
        let active = (0..5)
            .filter(|i| {
                r.manager
                    .plugin(&format!("test.compute{i}"))
                    .unwrap()
                    .progress()
                    .is_some_and(|p| p.fraction == Some(0.5))
            })
            .count();
        assert!(active <= 4);
        saw_computing |= active > 0;
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(saw_computing);
}
#[test]
fn native_ready_freeze_survives_a_replaced_lua_phase_function() {
    let mut r = Rig::new();
    r.install("test.frozen",&["services.provide"],&[],r#"
        function register(p)
            p.command{id='run',name='Run',run=function()
                assert(not pcall(function() p.service{id='late',version='1.0',run=function()end} end))
                print('NATIVE FREEZE')
            end}
            p.__phase=function() end
        end
    "#);
    r.command("test.frozen", None);
    assert!(
        r.log("test.frozen")
            .iter()
            .any(|l| l.contains("NATIVE FREEZE")),
        "{:?}",
        r.log("test.frozen")
    );
}
#[test]
fn malformed_client_values_and_reply_failures_leave_no_partial_transaction() {
    let mut r = Rig::new();
    r.install("test.bad",&["services.provide"],&[],r#"
        function register(p) p.service{id='bad',version='1.0',run=function() return function()end end} end
    "#);
    r.install(
        "test.client",
        &["services.call"],
        &["test.bad/bad@1.0"],
        &client(
            r#"
        local invalid={
            function() plum.services.call('test.bad/bad',{}, {seed=.5}) end,
            function() plum.services.call('test.bad/bad',{}, {timeout_ms=10.5}) end,
            function() plum.services.call('test.bad/bad',{}, {timeout_ms=251}) end,
            function() plum.services.call('test.bad/bad',function()end) end,
            function() plum.services.call('test.bad/bad',{}) end,
        }
        for i,f in ipairs(invalid) do assert(not pcall(f),'accepted '..i) end
        print('BAD DATA REFUSED')
    "#,
        ),
    );
    r.command("test.client", None);
    assert!(
        r.log("test.client")
            .iter()
            .any(|l| l.contains("BAD DATA REFUSED"))
    );
}

#[test]
fn installed_laboratory_packages_generate_deterministic_notes_with_undo() {
    let mut r = Rig::new();
    for id in [
        "plumspirit.example.note-service",
        "plumspirit.example.service-composer",
    ] {
        let example = super::super::examples::by_id(id).unwrap();
        let report = r.manager.install_example(example, &mut r.prefs).unwrap();
        assert_eq!(
            r.prefs.installed[id].origin,
            super::super::package::Origin::Example
        );
        assert!(!r.prefs.installed[id].enabled && r.prefs.installed[id].granted.is_empty());
        let id = report.manifest.id;
        for cap in report.manifest.capabilities {
            r.manager.set_granted(&id, &cap, true, &mut r.prefs);
        }
        r.manager.set_enabled(&id, true, &mut r.prefs);
        r.settle();
        assert_eq!(r.manager.plugin(&id).unwrap().status, Status::Ready);
    }
    let mut doc = Arc::new(PlumDocument::new_project(
        "Laboratory".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot()).with_info(
        crate::lua::handles::ProjectInfo {
            revision: history.current_state(),
            ..Default::default()
        },
    );
    r.manager.project_opened(world.clone());
    r.settle();
    r.manager
        .run_command(
            "plumspirit.example.service-composer",
            "generate",
            Some(world),
        )
        .unwrap();
    for event in r.settle() {
        if let PluginEvent::Commits { mut commits, .. } = event {
            commits.apply(&mut history, &mut doc).unwrap();
        }
    }
    assert_eq!(
        doc.note_count(),
        16,
        "{:?}",
        r.log("plumspirit.example.service-composer")
    );
    let expected: Vec<_> = doc
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect();
    history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.note_count(), 0);
    history.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect::<Vec<_>>(),
        expected
    );
    Arc::get_mut(&mut doc)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes
        .select_all();
    history.observe_ids(&doc);
    r.manager.observation_context(Some(ProjectStamp {
        serial: history.incarnations().project(),
        revision: history.current_state(),
    }));
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot()).with_info(
        crate::lua::handles::ProjectInfo {
            revision: history.current_state(),
            ..Default::default()
        },
    );
    r.manager
        .run_command(
            "plumspirit.example.service-composer",
            "transform",
            Some(world),
        )
        .unwrap();
    for event in r.settle() {
        if let PluginEvent::Commits { mut commits, .. } = event {
            commits.apply(&mut history, &mut doc).unwrap();
        }
    }
    let changed: Vec<_> = doc
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect();
    assert_eq!(changed.len(), expected.len());
    for (before, after) in expected.iter().zip(&changed) {
        let after_time = (after.start_tick, after.duration_ticks);
        let before_time = (before.start_tick, before.duration_ticks);
        assert_eq!(after_time, before_time);
        assert_eq!(after.attrs.pitch(), before.attrs.pitch() + 7);
        assert_eq!(after.attrs.velocity(), before.attrs.velocity());
        assert_eq!(after.group, before.group);
        assert_eq!(after.sub, before.sub);
    }
    history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect::<Vec<_>>(),
        expected
    );
}
#[test]
fn global_graph_budget_and_long_nested_chains_are_bounded() {
    let frame = Frame {
        leases: Vec::new(),
        lives: Vec::new(),
        path: vec!["test.root".into()],
        deadline: Instant::now() + Duration::from_secs(1),
        project: None,
        authority: Arc::new(BTreeSet::new()),
        calls: Arc::new(AtomicUsize::new(0)),
        call_id: 0,
    };
    let mut graph = Graph::default();
    for i in 0..MAX_PENDING {
        graph
            .admit(&format!("test.c{i}"), &format!("test.p{i}"), &frame)
            .unwrap();
    }
    assert_eq!(
        graph
            .admit("test.extra", "test.target", &frame)
            .unwrap_err()
            .category,
        Category::Budget
    );
    let life = Fence::live();
    let mut expired = frame.clone();
    expired.lives.push(Arc::clone(&life));
    let mut graph = Graph::default();
    graph.admit("test.a", "test.b", &expired).unwrap();
    life.invalidate();
    graph.clean(None);
    assert!(graph.admit("test.b", "test.a", &frame).is_ok());
    let mut r = Rig::new();
    r.install("test.leaf", &["services.provide"], &[], provider());
    let mut target = "test.leaf".to_owned();
    for i in 0..5 {
        let owner = format!("test.chain{i}");
        let dep = format!("{target}/double@1.0");
        let source = format!(
            "function register(p) p.service{{id='double',version='1.0',run=function(data) return plum.services.call('{target}/double',data) end}} end"
        );
        r.install(
            &owner,
            &["services.provide", "services.call"],
            &[&dep],
            &source,
        );
        target = owner;
    }
    let dep = format!("{target}/double@1.0");
    r.install("test.client",&["services.call"],&[&dep],&client(&format!("assert(plum.services.call('{target}/double',{{value=21}}).value==42);print('DEEP REPLY')")));
    r.command("test.client", None);
    assert!(
        r.log("test.client")
            .iter()
            .any(|l| l.contains("DEEP REPLY")),
        "{:?}",
        r.log("test.client")
    );
}

#[test]
fn provider_reload_expires_inflight_work_and_a_later_call_uses_the_new_version() {
    let mut r = Rig::new();
    r.install(
        "test.provider",
        &["services.provide"],
        &[],
        r#"
        function register(p) p.service{id='slow',version='1.0',run=function()
            plum.progress(.5,'old provider');while true do plum.yield() end
        end} end
    "#,
    );
    r.install(
        "test.client",
        &["services.call"],
        &["test.provider/slow@1.0"],
        &client("plum.services.call('test.provider/slow',{})"),
    );
    r.manager.run_command("test.client", "run", None).unwrap();
    let start = Instant::now();
    loop {
        r.manager.poll(&r.prefs);
        if r.manager
            .plugin("test.provider")
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    let dir = r.manager.plugin("test.provider").unwrap().dir.clone();
    std::fs::write(dir.join("main.lua"),"function register(p) p.service{id='slow',version='2.0',run=function() return {value=42} end} end").unwrap();
    r.manager.reload("test.provider", &r.prefs);
    let events = r.settle();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PluginEvent::Notice { error: true, .. }))
    );
    let events = r.command("test.client", None);
    assert!(events.iter().any(
        |e| matches!(e,PluginEvent::Notice {text,error:true} if text.contains("incompatible"))
    ));
}
#[test]
fn an_uncaught_service_failure_discards_even_an_already_committed_private_step() {
    let mut r = Rig::new();
    r.install("test.bad",&["services.provide"],&[],r#"
        function register(p) p.service{id='fail',version='1.0',run=function() error('expected provider failure') end} end
    "#);
    r.install("test.client",&["services.call","project.read","notes.edit"],&["test.bad/fail@1.0"],&client(r#"
        local tx=plum.transaction{};tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=60}});tx:commit()
        plum.services.call('test.bad/fail',{})
    "#));
    let doc = Arc::new(PlumDocument::new_project(
        "Failed composition".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    r.manager.project_opened(world.clone());
    r.settle();
    let events = r.command("test.client", Some(world));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PluginEvent::Commits { .. }))
    );
    assert_eq!(doc.note_count(), 0);
    assert_eq!(Arc::strong_count(&doc), 1);
}
