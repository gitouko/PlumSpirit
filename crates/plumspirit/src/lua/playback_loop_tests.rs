use super::*;
use crate::audio::Transport;
use crate::lua::worker::{ScriptJob, ScriptWorker};
use crate::lua::{Limits, Plain, Scripts, World, handles::ProjectInfo};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument},
};
use std::time::Duration;
fn world() -> World {
    let doc = Arc::new(PlumDocument::new_project(
        "Loop worker".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut h = History::new(HistoryLimits::default());
    h.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), h.incarnations().snapshot());
    let snapshot = Snapshot::capture(
        &Transport::default(),
        &doc,
        crate::lua::bridge::ProjectStamp {
            serial: world.project_serial(),
            revision: 0,
        },
    );
    world.with_info(ProjectInfo {
        playback: Some(snapshot),
        ..Default::default()
    })
}
fn caps() -> Vec<Capability> {
    vec![
        Capability::ReadPlayback,
        Capability::ControlPlayback,
        Capability::ReadProject,
        Capability::EditNotes,
    ]
}
fn worker(grants: Vec<Capability>) -> ScriptWorker {
    ScriptWorker::spawn(Limits::default(), grants, &Scripts::default()).unwrap()
}
fn run(w: &mut ScriptWorker, source: &str, interactive: bool) -> crate::lua::worker::ScriptResult {
    w.submit_with_interaction(
        ScriptJob {
            file: "loop.lua".into(),
            source: source.into(),
            world: Some(world()),
            time_limit: None,
        },
        interactive,
    )
    .unwrap();
    w.wait(Duration::from_secs(3)).unwrap()
}
#[test]
fn loop_parser_enforces_strict_plain_exact_nonempty_ranges_and_explicit_clear() {
    let lua = Lua::new();
    for (source, want) in [
        (
            "{start='9007199254740993',finish='9007199254741993'}",
            Setting {
                range: Some((9_007_199_254_740_993, 9_007_199_254_741_993)),
                enabled: true,
            },
        ),
        (
            "{start=0,finish=100,enabled=false}",
            Setting {
                range: Some((0, 100)),
                enabled: false,
            },
        ),
        (
            "{clear=true}",
            Setting {
                range: None,
                enabled: false,
            },
        ),
    ] {
        assert_eq!(
            parse(lua.load(format!("return {source}")).eval().unwrap()).unwrap(),
            want
        );
    }
    for source in [
        "{}",
        "{start=1,finish=1}",
        "{start=2,finish=1}",
        "{start=-1,finish=1}",
        "{start=0,finish=1/0}",
        "{start=0,finish=1,enabled=1}",
        "{start=0,finish=1,clear=false}",
        "{clear=true,start=0}",
        "{clear='true'}",
        "{start=0,finish=1,extra=true}",
        "setmetatable({start=0,finish=1},{})",
        "{[1]=true,clear=true}",
    ] {
        assert!(
            parse(lua.load(format!("return {source}")).eval().unwrap()).is_err(),
            "accepted {source}"
        );
    }
}
#[test]
fn loop_staging_requires_both_grants_and_explicit_invocation_and_has_one_intent() {
    for grants in [
        vec![],
        vec![Capability::ReadPlayback],
        vec![Capability::ControlPlayback],
    ] {
        let mut w = worker(grants);
        let r = run(&mut w, "plum.playback.stage_loop{start=0,finish=100}", true);
        assert_eq!(r.result.unwrap_err().category, Category::Capability);
        assert!(r.plugin.loop_intent.is_none());
    }
    let mut w = worker(caps());
    let r = run(
        &mut w,
        "plum.playback.stage_loop{start=0,finish=100}",
        false,
    );
    assert_eq!(r.result.unwrap_err().category, Category::Capability);
    assert!(r.plugin.loop_intent.is_none());
    let r = run(
        &mut w,
        r#"
      local s=plum.playback.stage_loop{start=9007199254740993,finish=9007199254741993,enabled=false}
      assert(s.staged and not s.enabled and math.type(s.start)=='integer')
      s.finish=1
      assert(not pcall(function()plum.playback.stage_loop{clear=true}end))
      assert(not pcall(function()plum.transaction{}end))
      return 'staged'
    "#,
        true,
    );
    assert_eq!(r.result.unwrap(), Plain::String("staged".into()));
    assert!(r.commits.is_none());
    let intent = r.plugin.loop_intent.unwrap();
    assert!(intent.valid());
    assert_eq!(
        intent.setting(),
        Setting {
            range: Some((9_007_199_254_740_993, 9_007_199_254_741_993)),
            enabled: false
        }
    );
    assert!(intent.claim());
    assert!(!intent.claim());
    assert!(!intent.valid());
}
#[test]
fn failed_canceled_or_superseded_runs_never_publish_or_revive_loop_intents() {
    let mut w = worker(caps());
    let r = run(
        &mut w,
        "plum.playback.stage_loop{start=0,finish=100};error('failed after stage')",
        true,
    );
    assert!(r.result.is_err());
    assert!(r.plugin.loop_intent.is_none());
    let r = run(&mut w, "plum.playback.stage_loop{start=0,finish=100}", true);
    let intent = r.plugin.loop_intent.unwrap();
    assert!(intent.valid());
    w.cancel();
    assert!(!intent.valid());
    assert!(run(&mut w, "return 1", true).result.is_ok());
    assert!(
        !intent.valid(),
        "new run clears reusable cancel flag but cannot revive old intent"
    );
    let r = run(&mut w, "plum.playback.stage_loop{clear=true}", true);
    let newer = r.plugin.loop_intent.unwrap();
    assert!(newer.valid());
    assert!(run(&mut w, "return 2", true).result.is_ok());
    assert!(!newer.valid(), "superseded scope cannot publish later");
}
#[test]
fn open_or_committed_musical_work_is_excluded_but_a_canceled_transaction_can_stage() {
    let mut w = worker(caps());
    let r = run(
        &mut w,
        r#"
       local tx=plum.transaction{}
       local ok,err=pcall(function()plum.playback.stage_loop{start=0,finish=100}end)
       assert(not ok and tostring(err):find('without musical transactions'))
       tx:cancel()
       plum.playback.stage_loop{clear=true}
    "#,
        true,
    );
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert!(r.plugin.loop_intent.is_some() && r.commits.is_none());
    let r = run(
        &mut w,
        r#"
       local tx=plum.transaction{}
       local t=plum.project.current():tracks()[1]
       tx:insert_notes(t,{{start=0,gate=60,key=60,velocity=100}})
       tx:commit()
       local ok,err=pcall(function()plum.playback.stage_loop{start=0,finish=100}end)
       assert(not ok and tostring(err):find('without musical transactions'))
    "#,
        true,
    );
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert!(r.plugin.loop_intent.is_none() && r.commits.is_some());
}
#[test]
fn loop_intent_native_charge_survives_clones_and_releases_on_failure_and_last_drop() {
    let host = Host::new(Limits::default(), caps()).unwrap();
    host.set_world(Some(world()));
    let budget = crate::lua::bridge::NativeBudget::isolated(8192);
    host.lua.set_app_data(budget.clone());
    host.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    host.run(
        "account.lua",
        "held=plum.playback.stage_loop{start=0,finish=100}",
    )
    .unwrap();
    let intent = host.take_plugin_out().loop_intent.unwrap();
    let cloned = intent.clone();
    assert_eq!(budget.used(), 4096);
    drop(intent);
    assert_eq!(budget.used(), 4096);
    drop(cloned);
    assert_eq!(budget.used(), 0);
    host.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    assert!(
        host.run(
            "fail.lua",
            "plum.playback.stage_loop{clear=true};error('failed')"
        )
        .is_err()
    );
    assert!(host.take_plugin_out().loop_intent.is_none());
    assert_eq!(budget.used(), 0);
    host.lua
        .set_app_data(crate::lua::bridge::NativeBudget::isolated(2048));
    host.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    assert_eq!(
        host.run("quota.lua", "plum.playback.stage_loop{clear=true}")
            .unwrap_err()
            .category,
        Category::Budget
    );
    assert!(host.take_plugin_out().loop_intent.is_none());
}
