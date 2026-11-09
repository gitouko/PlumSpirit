use super::*;
use crate::lua::plugin_api::PluginMeta;
use crate::lua::worker::{ScriptJob, ScriptResult, ScriptWorker};
use crate::lua::{Limits, Plain, Scripts};
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, PlumDocument};

fn stamp(revision: u64) -> ProjectStamp {
    ProjectStamp {
        serial: 77,
        revision,
    }
}
fn event(revision: u64) -> Event {
    Event::new(
        Kind::ContentChanged,
        stamp(revision),
        revision,
        Data::Content {
            previous_revision: revision.saturating_sub(1),
        },
    )
    .unwrap()
}
fn decls(count: usize, kinds: Vec<Kind>) -> Decls {
    Decls {
        observers: (0..count)
            .map(|i| ObserverDecl {
                id: format!("observe-{i}"),
                kinds: kinds.clone(),
                debounce_ms: 100,
            })
            .collect(),
        ..Default::default()
    }
}
fn worker(caps: Vec<Capability>) -> ScriptWorker {
    ScriptWorker::spawn_plugin(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Limits::default()
        },
        caps,
        &Scripts::default(),
        Some(PluginMeta {
            id: "test.observe".into(),
            name: "Test observer".into(),
            version: "1.0.0".into(),
            root: std::env::temp_dir(),
            data_dir: None,
            config_version: 1,
        }),
    )
    .unwrap()
}
fn script(worker: &mut ScriptWorker, source: &str) -> ScriptResult {
    worker
        .submit(ScriptJob {
            file: "reactive.lua".into(),
            source: source.into(),
            world: None,
            time_limit: None,
        })
        .unwrap();
    worker
        .wait(Duration::from_secs(5))
        .expect("worker finishes")
}
fn delivery(revision: u64) -> Delivery {
    Delivery {
        views: [None, None],
        playback: None,
        id: "observe".into(),
        event: event(revision),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    }
}
const CAPS: [Capability; 3] = [
    Capability::ReadProject,
    Capability::ObserveProject,
    Capability::EditNotes,
];

#[test]
fn latest_state_coalescing_preserves_sequence_range_and_has_a_maximum_delay() {
    let mut inbox = Inbox::default();
    let budget = NativeBudget::isolated(64 * 1024);
    let start = Instant::now();
    let defs = decls(1, vec![Kind::ContentChanged]);
    for n in 1..=4000 {
        inbox
            .push(
                &defs,
                &event(n),
                start + Duration::from_millis(n - 1),
                &budget,
            )
            .unwrap();
    }
    assert_eq!(inbox.len(), 1);
    assert!(budget.used() > 0);
    let d = inbox
        .take(start + Duration::from_millis(4000), Some(stamp(4000)))
        .unwrap();
    assert_eq!(
        (d.event.first_sequence, d.event.sequence, d.event.coalesced),
        (1, 4000, 3999)
    );
    assert_eq!(
        d.event.data,
        Data::Content {
            previous_revision: 0
        }
    );
    inbox.clear();
    assert!(!d.valid());
    assert_eq!(budget.used(), 0);
}

#[test]
fn queue_caps_overflow_stale_revisions_and_native_quota_are_explicit() {
    let mut inbox = Inbox::default();
    let now = Instant::now();
    let defs = decls(32, vec![Kind::ContentChanged, Kind::Undo, Kind::Redo]);
    let budget = NativeBudget::isolated(64 * 1024);
    for kind in [Kind::ContentChanged, Kind::Undo, Kind::Redo] {
        let data = if kind == Kind::ContentChanged {
            Data::Content {
                previous_revision: 0,
            }
        } else {
            Data::History {
                previous_revision: 0,
            }
        };
        inbox
            .push(
                &defs,
                &Event::new(kind, stamp(1), 1, data).unwrap(),
                now,
                &budget,
            )
            .unwrap();
    }
    assert_eq!(inbox.len(), MAX_PENDING);
    assert_eq!(inbox.dropped, 32);
    assert!(
        inbox
            .take(now + Duration::from_secs(1), Some(stamp(2)))
            .is_none()
    );
    assert_eq!(inbox.dropped, 96);
    assert_eq!(inbox.len(), 0);
    inbox.clear();
    assert_eq!(budget.used(), 0);
    let denied = NativeBudget::isolated(0);
    assert!(inbox.push(&defs, &event(1), now, &denied).is_err());
    assert_eq!(inbox.len(), 0);
    assert_eq!(denied.used(), 0);
}

#[test]
fn lifecycle_clear_invalidates_inflight_and_close_never_attaches_to_new_project() {
    let mut inbox = Inbox::default();
    let now = Instant::now();
    let budget = NativeBudget::isolated(64 * 1024);
    let defs = decls(1, vec![Kind::ContentChanged, Kind::ProjectClosed]);
    inbox.push(&defs, &event(1), now, &budget).unwrap();
    let d = inbox
        .take(now + Duration::from_millis(100), Some(stamp(1)))
        .unwrap();
    inbox.clear();
    assert!(!d.valid());
    let closed = Event::new(Kind::ProjectClosed, stamp(1), 2, Data::Closed).unwrap();
    inbox.push(&defs, &closed, now, &budget).unwrap();
    assert!(
        inbox
            .take(
                now + Duration::from_secs(1),
                Some(ProjectStamp {
                    serial: 88,
                    revision: 1
                })
            )
            .is_none()
    );
    inbox.push(&defs, &closed, now, &budget).unwrap();
    assert!(inbox.take(now + Duration::from_secs(2), None).is_some());
    inbox.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn typed_payloads_are_exact_and_subscription_inputs_are_bounded() {
    assert!(Event::new(Kind::Undo, stamp(1), 1, Data::Closed).is_none());
    let mut w = worker(CAPS.to_vec());
    let result = script(
        &mut w,
        r#"
        local p=plum.plugin
        p.observe{id='observe',events={'content_changed'},debounce_ms=20,run=function(e)
            assert(e.schema==1 and e.kind=='content_changed' and e.subscription=='observe')
            assert(e.context=='metadata' and e.dropped==42)
            assert(e.project==77 and e.revision==9007199254740993)
            assert(math.type(e.revision)=='integer' and e.read_only and e.data.previous_revision==9007199254740992)
            print('typed exact')
        end}
        local invalid={
            function() p.observe{id=string.rep('a',65),events={'undo'},run=function()end} end,
            function() p.observe{id='bad',events={'missing'},run=function()end} end,
            function() p.observe{id='bad',events={'undo'},debounce_ms=19,run=function()end} end,
            function() p.observe{id='bad',events=setmetatable({'undo'},{}),run=function()end} end,
            function() p.observe{id='bad',events={[1]='undo',[3]='redo'},run=function()end} end,
            function() p.observe{id='observe',events={'undo'},run=function()end} end,
        }
        for i,f in ipairs(invalid) do assert(not pcall(f),'accepted '..i) end
        p.__phase('ready')
        assert(not pcall(function() p.observe{id='late',events={'undo'},run=function()end}end))
    "#,
    );
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(result.decls.unwrap().observers.len(), 1);
    let mut event = delivery(9_007_199_254_740_993);
    event.dropped = 42;
    w.submit_observation(event).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(result.output, vec!["typed exact"]);
}

#[test]
fn observer_authority_stays_read_only_even_through_commands_and_phase_spoofing() {
    let mut w = worker(Capability::ALL.to_vec());
    let result = script(
        &mut w,
        r#"
        local p=plum.plugin
        p.command{id='edit',name='Edit',run=function() plum.transaction{} end}
        p.observe{id='observe',events={'content_changed'},run=function(e)
            e.read_only=false;p.__phase('load')
            assert(not pcall(function() p.observe{id='nested',events={'undo'},run=function()end}end))
            local forbidden={
                function() plum.transaction{} end,
                function() plum.playback.request{action='stop'} end,
                function() plum.playback.stage_loop{start=0,finish=100} end,
                function() p.__run('edit') end,
                function() plum.selection.set_time(0,10) end,
                function() plum.config.project.set('value',1) end,
                function() plum.config.project.remove('value') end,
            }
            for i,f in ipairs(forbidden) do local ok,err=pcall(f);assert(not ok and tostring(err):find('read%-only'),'bypass '..i) end
            assert(plum.project.current()==nil,'metadata observation retains no document root')
            p.__phase('ready');print('sealed observer')
        end};p.__phase('ready')
    "#,
    );
    assert!(result.result.is_ok(), "{:?}", result.result);
    w.submit_observation(delivery(1)).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(result.output, vec!["sealed observer"]);
    assert!(
        result.commits.is_none() && result.ui.is_none() && result.plugin.project_data.is_none()
    );
    // Normal user work retains API01 edit authority after the observer scope ends.
    let mut doc = Arc::new(PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960));
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    h.observe_ids(&doc);
    w.submit(ScriptJob{file:"normal.lua".into(),source:"local tx=plum.transaction{};tx:insert_notes(plum.project.current():track(1),{{start=0,gate=1,key=60}});tx:commit()".into(),world:Some(super::super::World::new(Arc::clone(&doc),h.incarnations().snapshot())),time_limit:None}).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    result.commits.unwrap().apply(&mut h, &mut doc).unwrap();
    assert_eq!(doc.note_count(), 1);
}

#[test]
fn permission_expiry_cancellation_and_unsubscribe_are_worker_scoped() {
    for caps in [
        vec![Capability::ReadProject],
        vec![Capability::ObserveProject],
    ] {
        let mut w = worker(caps);
        let result = script(
            &mut w,
            "plum.plugin.observe{id='observe',events={'undo'},run=function()end}",
        );
        assert_eq!(result.result.unwrap_err().category, Category::Capability);
    }
    let mut w = worker(CAPS.to_vec());
    assert!(
        script(
            &mut w,
            r#"
        plum.plugin.observe{id='observe',events={'content_changed'},run=function()
            plum.progress(.5,'observing');while true do plum.yield() end
        end};plum.plugin.__phase('ready')
    "#
        )
        .result
        .is_ok()
    );
    let d = delivery(1);
    d.fence.invalidate();
    w.submit_observation(d).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(result.result.unwrap_err().category, Category::Expired);
    let d = delivery(2);
    let fence = Arc::clone(&d.fence);
    w.submit_observation(d).unwrap();
    let start = Instant::now();
    while w.progress().fraction != Some(0.5) {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    fence.invalidate();
    w.cancel();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(matches!(
        result.result.unwrap_err().category,
        Category::Canceled | Category::Expired
    ));
    assert!(result.commits.is_none() && result.ui.is_none());
    let result = script(&mut w, "plum.plugin.unobserve('observe');return 'normal'");
    assert_eq!(result.result.unwrap(), Plain::String("normal".into()));
    assert!(result.decls.unwrap().observers.is_empty());
}

fn manager(
    source: &str,
) -> (
    super::super::plugins::PluginManager,
    super::super::plugins::PluginPrefs,
    std::path::PathBuf,
) {
    use super::super::plugins::{PluginManager, PluginPrefs};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "plumspirit-observer-manager-{}-{nonce}",
        std::process::id()
    ));
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("plum-plugin.toml"),
        r#"
        id="test.observe"
        name="Observer Manager Test"
        version="1.0.0"
        api="0.2"
        entry="main.lua"
        capabilities=["project.read","project.observe","notes.edit"]
    "#,
    )
    .unwrap();
    std::fs::write(src.join("main.lua"), source).unwrap();
    let mut m = PluginManager::new(Some(root.join("plugins")), Scripts::default());
    let mut prefs = PluginPrefs::default();
    m.install(&src, false, &mut prefs).unwrap();
    for cap in ["project.read", "project.observe", "notes.edit"] {
        m.set_granted("test.observe", cap, true, &mut prefs);
    }
    m.set_enabled("test.observe", true, &mut prefs);
    settle(&mut m, &prefs);
    assert_eq!(
        m.plugin("test.observe").unwrap().status,
        super::super::plugins::Status::Ready
    );
    (m, prefs, root)
}
fn settle(
    m: &mut super::super::plugins::PluginManager,
    prefs: &super::super::plugins::PluginPrefs,
) -> Vec<super::super::plugins::PluginEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        events.extend(m.poll(prefs));
        if !m.is_busy() {
            return events;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "manager settles");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn cleanup(root: std::path::PathBuf) {
    let resolved = root.canonicalize().unwrap();
    let temp = std::env::temp_dir().canonicalize().unwrap();
    assert_eq!(resolved.parent(), Some(temp.as_path()));
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("plumspirit-observer-manager-")
    );
    std::fs::remove_dir_all(&resolved).unwrap();
}
fn manager_world() -> (Arc<PlumDocument>, super::super::World) {
    let doc = Arc::new(PlumDocument::new_project(
        "Observed".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut h = History::new(HistoryLimits::default());
    h.observe_ids(&doc);
    let world = super::super::World::new(Arc::clone(&doc), h.incarnations().snapshot());
    (doc, world)
}
fn log_has(m: &super::super::plugins::PluginManager, text: &str) -> bool {
    m.plugin("test.observe")
        .unwrap()
        .log
        .lines()
        .any(|l| l.text.contains(text))
}

#[test]
fn manager_delivers_coalesced_events_and_stale_results_never_reach_ui_or_block_edits() {
    let (mut m, prefs, root) = manager(
        r#"
        function register(p)
            p.observe{id='observe',events={'content_changed'},debounce_ms=20,run=function(e)
                if e.revision==1 then
                    plum.progress(.5,'entered')
                    pcall(function() while true do plum.yield() end end)
                    print('STALE OUTPUT');plum.ui.notify('STALE NOTICE')
                else print('fresh '..e.revision..' count '..e.coalesced) end
            end}
        end
    "#,
    );
    let (mut doc, world) = manager_world();
    let project = world.project_serial();
    m.project_opened(world);
    settle(&mut m, &prefs);
    m.publish_observation(
        Kind::ContentChanged,
        ProjectStamp {
            serial: project,
            revision: 1,
        },
        Data::Content {
            previous_revision: 0,
        },
    );
    let start = Instant::now();
    while m
        .plugin("test.observe")
        .unwrap()
        .progress()
        .is_none_or(|p| p.fraction != Some(0.5))
    {
        m.poll(&prefs);
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        Arc::get_mut(&mut doc).is_some(),
        "metadata observer owns no live document root"
    );
    for revision in 2..=20 {
        m.publish_observation(
            Kind::ContentChanged,
            ProjectStamp {
                serial: project,
                revision,
            },
            Data::Content {
                previous_revision: revision - 1,
            },
        );
    }
    let events = settle(&mut m, &prefs);
    assert!(log_has(&m, "fresh 20 count 18"));
    assert!(!log_has(&m, "STALE OUTPUT"));
    assert!(!events.iter().any(|e|matches!(e,super::super::plugins::PluginEvent::Notice{text,..} if text.contains("STALE NOTICE"))));
    assert!(events.iter().all(|e| !matches!(
        e,
        super::super::plugins::PluginEvent::Commits { .. }
            | super::super::plugins::PluginEvent::Select { .. }
            | super::super::plugins::PluginEvent::ProjectData { .. }
    )));
    drop(m);
    cleanup(root);
}

#[test]
fn manager_close_reload_permission_revocation_and_failure_clean_up_subscriptions() {
    let (mut m, mut prefs, root) = manager(
        r#"
        function register(p)
            p.observe{id='observe',events={'project_opened','project_closed','content_changed'},debounce_ms=20,
                run=function(e) print('EVENT '..e.kind);if e.kind=='content_changed' then error('observer failed') end end}
            p.command{id='alive',name='Alive',run=function()return 'command alive'end}
        end
    "#,
    );
    let (_, world) = manager_world();
    let project = world.project_serial();
    m.project_opened(world);
    settle(&mut m, &prefs);
    assert!(log_has(&m, "EVENT project_opened"));
    assert!(m.observing());
    m.publish_observation(
        Kind::ContentChanged,
        ProjectStamp {
            serial: project,
            revision: 1,
        },
        Data::Content {
            previous_revision: 0,
        },
    );
    settle(&mut m, &prefs);
    assert!(
        !m.observing(),
        "a failed handler pauses automatic observation until reload"
    );
    m.run_command("test.observe", "alive", None).unwrap();
    let events = settle(&mut m, &prefs);
    assert!(events.iter().any(|e|matches!(e,super::super::plugins::PluginEvent::Notice{text,..} if text.contains("command alive"))));
    m.reload("test.observe", &prefs);
    // A reloaded lifecycle waits for a fresh world. Supply it explicitly.
    let (_, world) = manager_world();
    m.supply_world(world);
    settle(&mut m, &prefs);
    assert!(m.observing());
    m.project_closed();
    settle(&mut m, &prefs);
    assert!(log_has(&m, "EVENT project_closed"));
    m.set_granted("test.observe", "project.observe", false, &mut prefs);
    settle(&mut m, &prefs);
    assert!(!m.observing());
    assert!(m.plugin("test.observe").unwrap().decls.observers.is_empty());
    drop(m);
    cleanup(root);
}

#[test]
fn expired_callback_cannot_unsubscribe_after_catching_cancellation() {
    let mut w = worker(CAPS.to_vec());
    assert!(
        script(
            &mut w,
            r#"
        plum.plugin.observe{id='observe',events={'content_changed'},run=function(e)
            if e.revision==1 then
                plum.progress(.5,'entered')
                pcall(function() while true do plum.yield() end end)
                assert(not pcall(function() plum.plugin.unobserve('observe') end))
            else print('subscription retained') end
        end};plum.plugin.__phase('ready')
    "#
        )
        .result
        .is_ok()
    );
    let d = delivery(1);
    let fence = Arc::clone(&d.fence);
    w.submit_observation(d).unwrap();
    let start = Instant::now();
    while w.progress().fraction != Some(0.5) {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    fence.invalidate();
    w.cancel();
    assert!(w.wait(Duration::from_secs(3)).unwrap().result.is_err());
    w.submit_observation(delivery(2)).unwrap();
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(result.output, vec!["subscription retained"]);
}

#[test]
fn surface_events_validate_typed_indices_and_coalesce_independently_with_bounded_metadata() {
    assert!(
        Event::new(
            Kind::SurfaceChanged,
            stamp(1),
            1,
            Data::Surface {
                index: 2,
                view_revision: None
            }
        )
        .is_none()
    );
    assert!(
        Event::new(
            Kind::SurfaceChanged,
            stamp(1),
            1,
            Data::Surface {
                index: 0,
                view_revision: Some(0)
            }
        )
        .is_none()
    );
    let mut inbox = Inbox::default();
    let defs = decls(1, vec![Kind::SurfaceChanged]);
    let budget = NativeBudget::isolated(128 * 1024);
    let now = Instant::now();
    for sequence in 1..=4000u64 {
        let index = (sequence % 2) as u8;
        let event = Event::new(
            Kind::SurfaceChanged,
            stamp(1),
            sequence,
            Data::Surface {
                index,
                view_revision: Some(sequence),
            },
        )
        .unwrap();
        assert!(!event.transaction_allowed);
        inbox
            .push(
                &defs,
                &event,
                now + Duration::from_millis(sequence / 20),
                &budget,
            )
            .unwrap();
    }
    assert_eq!(inbox.len(), 2);
    let first = inbox
        .take(now + Duration::from_secs(1), Some(stamp(1)))
        .unwrap();
    let second = inbox
        .take(now + Duration::from_secs(2), Some(stamp(1)))
        .unwrap();
    assert_eq!(first.event.coalesced, 1999);
    assert_eq!(second.event.coalesced, 1999);
    assert_ne!(first.event.data, second.event.data);
    assert!(matches!(
        first.event.data,
        Data::Surface {
            index: 1,
            view_revision: Some(3999)
        }
    ));
    assert!(matches!(
        second.event.data,
        Data::Surface {
            index: 0,
            view_revision: Some(4000)
        }
    ));
    inbox.clear();
    assert_eq!(budget.used(), 0);
}
