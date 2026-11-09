use super::*;
use crate::lua::plugin_api::PluginMeta;
use crate::lua::reactive::{Data, Event, Fence, Kind};
use crate::lua::worker::{ScriptJob, ScriptResult, ScriptWorker};
use crate::lua::{Capability, Host, Limits, Scripts};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};
use std::sync::atomic::{AtomicU64, Ordering};

fn worker(caps: Vec<Capability>) -> ScriptWorker {
    static ID: AtomicU64 = AtomicU64::new(0);
    ScriptWorker::spawn_plugin(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        caps,
        &Scripts::default(),
        Some(PluginMeta {
            id: format!("test.response.{}", ID.fetch_add(1, Ordering::Relaxed)),
            name: "Response test".into(),
            version: "1.0.0".into(),
            root: std::env::temp_dir(),
            data_dir: None,
            config_version: 1,
        }),
    )
    .unwrap()
}
fn script(w: &mut ScriptWorker, source: &str) -> ScriptResult {
    w.submit(ScriptJob {
        file: "response.lua".into(),
        source: source.into(),
        world: None,
        time_limit: None,
    })
    .unwrap();
    w.wait(Duration::from_secs(3)).unwrap()
}
fn fixture_world() -> (Arc<PlumDocument>, History, World, Delivery) {
    let doc = Arc::new(PlumDocument::new_project(
        "Responses".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let info = super::super::handles::ProjectInfo {
        revision: history.current_state(),
        ..Default::default()
    };
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot()).with_info(info);
    let stamp = ProjectStamp {
        serial: world.project_serial(),
        revision: world.info.revision,
    };
    let d = Delivery {
        views: [None, None],
        playback: None,
        id: "observe".into(),
        event: Event::new(
            Kind::ContentChanged,
            stamp,
            1,
            Data::Content {
                previous_revision: 0,
            },
        )
        .unwrap(),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    };
    (doc, history, world, d)
}
fn request() -> Request {
    let host = Host::new(Limits::default(), []).unwrap();
    host.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    Request {
        command: "respond".into(),
        data: Payload::read(
            &host.lua,
            host.lua
                .load("return {key=64,exact=9007199254740993}")
                .eval()
                .unwrap(),
        )
        .unwrap(),
    }
}
fn install(w: &mut ScriptWorker, body: &str) {
    let src = format!(
        r#"
        plum.plugin.command{{id='respond',name='Respond',run=function(ctx) {body} end}}
        plum.plugin.observe{{id='observe',events={{'content_changed'}},debounce_ms=20,run=function(e)
            local ok=plum.plugin.request_transaction{{command='respond',data={{key=64,exact=9007199254740993}}}}
            print('offered '..tostring(ok))
        end}};plum.plugin.__phase('ready')
    "#
    );
    let result = script(w, &src);
    assert!(result.result.is_ok(), "{:?}", result.result);
}
const INSERT: &str = "local tx=plum.transaction{label='Reactive note'};tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=ctx.request.key}});tx:commit()";

#[test]
fn event_request_and_detached_response_apply_as_one_native_undo_step() {
    let mut w = worker(Capability::ALL.to_vec());
    install(&mut w, INSERT);
    let (mut doc, mut history, world, d) = fixture_world();
    w.submit_observation(d.clone()).unwrap();
    let offered = w.wait(Duration::from_secs(3)).unwrap();
    assert!(offered.result.is_ok());
    let request = offered.plugin.response_request.unwrap();
    assert!(offered.commits.is_none() && offered.ui.is_none());
    w.submit_response(world, d.clone(), request).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(
        Arc::strong_count(&doc),
        1,
        "detached result keeps no live document root"
    );
    let guard = result.response_guard.unwrap();
    assert!(guard.matches(&doc, d.event.stamp));
    let mut commits = result.commits.unwrap();
    assert_eq!(commits.steps.len(), 1);
    commits.bind_response_base(&doc).unwrap();
    commits.apply(&mut history, &mut doc).unwrap();
    assert_eq!(doc.note_count(), 1);
    assert_eq!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .next()
            .unwrap()
            .attrs
            .pitch(),
        64
    );
    history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.note_count(), 0);
    history.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.note_count(), 1);
}

#[test]
fn detached_calculation_does_not_pin_ui_and_stale_selection_or_revision_is_rejected() {
    let mut w = worker(Capability::ALL.to_vec());
    install(
        &mut w,
        "plum.progress(.5,'calculating');pcall(function() while true do plum.yield() end end);",
    );
    let (mut doc, mut history, world, d) = fixture_world();
    w.submit_response(world, d.clone(), request()).unwrap();
    let start = Instant::now();
    while w.progress().fraction != Some(0.5) {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    history
        .apply(
            Arc::get_mut(&mut doc).expect("Lua work never pins the live root"),
            Operation::AddTrack,
        )
        .unwrap();
    d.fence.invalidate();
    w.cancel();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_err());
    assert!(result.commits.is_none());
    assert!(!result.response_guard.unwrap().matches(
        &doc,
        ProjectStamp {
            serial: d.event.stamp.serial,
            revision: history.current_state()
        }
    ));
    let (_, _, world, d) = world_with_note();
    let budget = NativeBudget::isolated(128 * 1024 * 1024);
    let (private, guard) = detach(world, &d, &budget, || Ok(())).unwrap();
    let mut changed = (*private.doc).clone();
    changed.track_mut(0).unwrap().notes.select_all();
    assert!(
        !guard.matches(&changed, d.event.stamp),
        "selection-only changes need no history revision"
    );
    drop(private);
    drop(guard);
    assert_eq!(budget.used(), 0);
}
fn world_with_note() -> (Arc<PlumDocument>, History, World, Delivery) {
    let (doc, history, mut world, d) = fixture_world();
    let mut copy = (*doc).clone();
    copy.track_mut(0)
        .unwrap()
        .notes
        .insert(
            NoteRecord {
                start_tick: 0,
                duration_ticks: 1,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            },
            false,
        )
        .unwrap();
    let doc = Arc::new(copy);
    world.doc = Arc::clone(&doc);
    (doc, history, world, d)
}

#[test]
fn response_authority_is_separate_and_cannot_chain_or_request_ui_project_config() {
    let mut w = worker(Capability::ALL.to_vec());
    install(
        &mut w,
        r#"
        assert(ctx.reactive and ctx.event.context=='metadata' and ctx.request.exact==9007199254740993)
        local rejected={
            function() plum.plugin.request_transaction{command='respond'} end,
            function() plum.selection.set_time(0,1) end,
            function() plum.config.project.set('x',1) end,
            function() plum.files.open{} end,
        }
        for i,f in ipairs(rejected) do assert(not pcall(f),'authority '..i) end
        local tx=plum.transaction{};tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=60}});tx:commit()
    "#,
    );
    let (_, _, world, d) = fixture_world();
    w.submit_response(world, d, request()).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert!(
        result.commits.is_some() && result.ui.is_none() && result.plugin.project_data.is_none()
    );
    assert!(w.poll_ui_request().is_none());
    let mut denied = worker(vec![Capability::ReadProject, Capability::ObserveProject]);
    install(&mut denied, INSERT);
    let (_, _, world, d) = fixture_world();
    denied.submit_observation(d.clone()).unwrap();
    assert_eq!(
        denied
            .wait(Duration::from_secs(3))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Capability
    );
    denied.submit_response(world, d, request()).unwrap();
    assert_eq!(
        denied
            .wait(Duration::from_secs(3))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Capability
    );
    assert!(
        script(&mut w, "return plum.plugin.__respond()")
            .result
            .is_err()
    );
    assert!(
        script(
            &mut w,
            "return plum.plugin.request_transaction{command='respond'}"
        )
        .result
        .is_err()
    );
}

#[test]
fn failed_multi_commit_explicit_selection_and_excess_operations_publish_no_partial_transaction() {
    for body in [
        format!("{INSERT};plum.transaction{{}}"),
        "local tx=plum.transaction{};for i=1,9 do tx:insert_notes(plum.project.current():track(1),{{start=i,gate=1,key=60}}) end;tx:commit()".into(),
        "local tx=plum.transaction{};tx:select_notes(plum.project.current():track(1):notes{});tx:commit()".into(),
        format!("{INSERT};error('after commit')"),
    ] {
        let mut w=worker(Capability::ALL.to_vec());install(&mut w,&body);
        let (doc,_,world,d)=fixture_world();w.submit_response(world,d,request()).unwrap();
        let result=w.wait(Duration::from_secs(3)).unwrap();assert!(result.result.is_err(),"accepted {body}");
        assert!(result.commits.is_none());assert_eq!(doc.note_count(),0);
    }
}

#[test]
fn snapshot_preflight_quota_cancellation_and_lifecycle_are_bounded() {
    let (doc, _, world, d) = fixture_world();
    let budget = NativeBudget::isolated(0);
    assert_eq!(
        detach(world.clone(), &d, &budget, || Ok(()))
            .err()
            .unwrap()
            .category,
        Category::Budget
    );
    assert_eq!(budget.used(), 0);
    let budget = NativeBudget::isolated(128 * 1024 * 1024);
    assert!(
        detach(world, &d, &budget, || Err(failure(
            Category::Canceled,
            "canceled"
        )))
        .is_err()
    );
    assert_eq!(budget.used(), 0);
    assert_eq!(Arc::strong_count(&doc), 1);
    let mut many = (*doc).clone();
    for _ in 1..4097 {
        many.add_track().unwrap();
    }
    let (_, history, mut world, d) = self::fixture_world();
    world.doc = Arc::new(many);
    assert_eq!(
        detach(world, &d, &budget, || Ok(()))
            .err()
            .unwrap()
            .category,
        Category::Budget
    );
    drop(history);
    let mut w = worker(Capability::ALL.to_vec());
    install(&mut w, INSERT);
    let (_, _, world, d) = self::fixture_world();
    d.lifecycle.invalidate();
    w.submit_response(world, d, request()).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(result.result.unwrap_err().category, Category::Expired);
    assert!(result.commits.is_none());
}

#[test]
fn shared_causal_gate_blocks_child_and_history_revisions_without_growing_a_log() {
    let mut gate = Gate::default();
    let now = Instant::now();
    let stamp = ProjectStamp {
        serial: 7,
        revision: 1,
    };
    assert!(gate.request(stamp, now));
    assert!(!gate.request(stamp, now + Duration::from_millis(499)));
    let child = ProjectStamp {
        revision: 2,
        ..stamp
    };
    gate.block(Some(child));
    assert!(!gate.request(child, now + Duration::from_secs(1)));
    assert!(gate.request(
        ProjectStamp {
            revision: 3,
            ..stamp
        },
        now + Duration::from_secs(1)
    ));
    gate.block(Some(stamp));
    assert!(!gate.request(stamp, now + Duration::from_secs(2)));
    gate.clear();
    assert!(gate.request(stamp, now + Duration::from_secs(2)));
}

#[test]
fn requests_are_one_per_delivery_and_causal_flags_cannot_be_spoofed_in_lua() {
    let mut w = worker(Capability::ALL.to_vec());
    let result = script(
        &mut w,
        r#"
        plum.plugin.command{id='respond',name='Respond',run=function()end}
        plum.plugin.observe{id='observe',events={'content_changed'},run=function(e)
            e.transaction_allowed=true
            local t={};t.self=t
            if e.revision==0 then
                assert(not pcall(function() plum.plugin.request_transaction{command='missing'} end))
                assert(not pcall(function() plum.plugin.request_transaction{command='respond',data=t} end))
                assert(plum.plugin.request_transaction{command='respond',data={integer=9007199254740993}})
                assert(not pcall(function() plum.plugin.request_transaction{command='respond'} end))
            else assert(not plum.plugin.request_transaction{command='respond'}) end
        end};plum.plugin.__phase('ready')
    "#,
    );
    assert!(result.result.is_ok(), "{:?}", result.result);
    let (_, _, _, d) = fixture_world();
    w.submit_observation(d.clone()).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert!(result.plugin.response_request.is_some());
    let mut blocked = d;
    blocked.event.stamp.revision = 1;
    blocked.event.transaction_allowed = false;
    w.submit_observation(blocked).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert!(result.plugin.response_request.is_none());
}

#[test]
fn automatic_output_and_mutation_families_have_explicit_bounds() {
    let host = Host::new(Limits::default(), Capability::ALL).unwrap();
    host.set_plugin(PluginMeta {
        id: "test.response.bounds".into(),
        name: "Bounds".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    let (_, _, world, d) = fixture_world();
    host.lua.set_app_data(super::super::reactive::Scope {
        delivery: Some(d),
        request: Some(request()),
    });
    host.set_world(Some(world));
    host.register("check_response_output", |lua, _| {
        super::super::notes::check_output(lua, 0, MAX_NOTES + 1)?;
        Ok(mlua::MultiValue::new())
    })
    .unwrap();
    let result = host.run(
        "bounds.lua",
        r#"
        assert(not pcall(function() plum.check_response_output() end))
        local tx=plum.transaction{}
        assert(not pcall(function() tx:add_track{} end))
        assert(not pcall(function() tx:create_group{} end))
        assert(not pcall(function() tx:insert_events(plum.project.current():track(1),{}) end))
        tx:cancel()
    "#,
    );
    assert!(result.is_ok(), "{result:?}");
    assert!(host.take_commits().is_none());
}

#[test]
fn automatic_note_iterators_are_native_accounted_bounded_and_eagerly_released() {
    let host = Host::new(Limits::default(), Capability::ALL).unwrap();
    host.set_plugin(PluginMeta {
        id: "test.response.cursor-lifetime".into(),
        name: "Cursor lifetime".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    host.lua.set_app_data(budget.clone());
    let (_, _, world, d) = world_with_note();
    host.set_world(Some(world));
    host.lua.set_app_data(super::super::reactive::Scope {
        delivery: Some(d),
        request: Some(request()),
    });
    assert!(
        host.run(
            "cursors.lua",
            r#"
        held={};local q=plum.project.current():track(1):notes{}
        for i=1,32 do held[i]=q:batches(1) end
        assert(not pcall(function() q:each() end))
    "#
        )
        .is_ok()
    );
    assert_eq!(
        budget.used(),
        0,
        "retained Lua closures cannot retain native automatic cursors after the job"
    );
    assert!(
        host.run(
            "retained.lua",
            "for _,it in ipairs(held) do assert(it()==nil) end"
        )
        .is_ok()
    );
    assert_eq!(budget.used(), 0);
    // The same snapshot cannot allocate an iterator when the native pool is denied.
    host.lua.set_app_data(NativeBudget::isolated(0));
    assert!(
        host.run(
            "denied.lua",
            "return plum.project.current():track(1):notes{}:batches(1)"
        )
        .is_err()
    );
}

#[test]
fn automatic_responses_preserve_authoritative_native_group_locks() {
    use plumspirit_engine::model::GroupInfo;
    let mut doc = PlumDocument::new_project("Locked response".into(), Bpm::DEFAULT, 960);
    let id = doc.project_mut().groups.add(GroupInfo {
        locked: true,
        ..Default::default()
    });
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert(
            NoteRecord {
                start_tick: 0,
                duration_ticks: 1,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: id,
            },
            false,
        )
        .unwrap();
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot()).with_info(
        super::super::handles::ProjectInfo {
            revision: history.current_state(),
            ..Default::default()
        },
    );
    let stamp = ProjectStamp {
        serial: world.project_serial(),
        revision: world.info.revision,
    };
    let mut delivery = fixture_world().3;
    delivery.event.stamp = stamp;
    let mut w = worker(Capability::ALL.to_vec());
    install(
        &mut w,
        "local tx=plum.transaction{};tx:modify_notes(plum.project.current():track(1):notes{},{velocity=64});tx:commit()",
    );
    w.submit_response(world, delivery, request()).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_err());
    assert!(result.commits.is_none());
    assert_eq!(
        doc.track(0)
            .unwrap()
            .notes
            .iter()
            .next()
            .unwrap()
            .attrs
            .velocity(),
        100
    );
}

#[test]
fn automatic_mutation_queries_stop_at_the_matched_note_limit_before_applying() {
    let host = Host::new(Limits::default(), Capability::ALL).unwrap();
    host.set_plugin(PluginMeta {
        id: "test.response.query-bound".into(),
        name: "Query bound".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    let mut doc = PlumDocument::new_project("Query bound".into(), Bpm::DEFAULT, 960);
    let records: Vec<_> = (0..100_001)
        .map(|i| NoteRecord {
            start_tick: i,
            duration_ticks: 1,
            attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&records, false)
        .unwrap();
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    let (_, _, _, d) = fixture_world();
    host.lua.set_app_data(super::super::reactive::Scope {
        delivery: Some(d),
        request: Some(request()),
    });
    let error=host.run("query-bound.lua","local tx=plum.transaction{};tx:delete_notes(plum.project.current():track(1):notes{});tx:commit()").unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(error.message.contains("mutation query"));
    assert!(host.take_commits().is_none());
    assert_eq!(doc.note_count(), 100_001);
}

#[test]
#[ignore = "manual automatic snapshot quota/timing fixture; run explicitly"]
fn automatic_snapshot_preflight_timings_and_conservative_refusal_are_measured() {
    for count in [100_000, 500_000, 1_000_000] {
        let mut doc = PlumDocument::new_project("Snapshot timing".into(), Bpm::DEFAULT, 960);
        let records: Vec<_> = (0..count)
            .map(|i| NoteRecord {
                start_tick: i,
                duration_ticks: 1,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&records, false)
            .unwrap();
        drop(records);
        let doc = Arc::new(doc);
        let mut history = History::new(HistoryLimits::default());
        history.observe_ids(&doc);
        let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
        let mut d = fixture_world().3;
        d.event.stamp = ProjectStamp {
            serial: world.project_serial(),
            revision: world.info.revision,
        };
        let budget = NativeBudget::isolated(256 * 1024 * 1024);
        let start = Instant::now();
        let result = detach(world, &d, &budget, || Ok(()));
        println!(
            "automatic snapshot notes={count}, time={:?}, accepted={}, reserved_native_bytes={}",
            start.elapsed(),
            result.is_ok(),
            budget.used()
        );
        if count < 1_000_000 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.as_ref().err().unwrap().category, Category::Budget);
        }
        assert_eq!(Arc::strong_count(&doc), 1);
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}
