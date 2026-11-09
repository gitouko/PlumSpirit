use super::*;
use crate::audio::PlayheadPolicy;
use crate::lua::plugin_api::{Decls, PluginMeta};
use crate::lua::reactive::{Data, Delivery, Event, Fence, Inbox, Kind, ObserverDecl};
use crate::lua::worker::{ScriptJob, ScriptWorker};
use crate::lua::{Category, Host, Limits, Plain, Scripts, World, handles::ProjectInfo};
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, tempo::Speed};
use std::time::{Duration, Instant};

fn fixture() -> (World, Snapshot) {
    let doc = Arc::new(PlumDocument::new_project(
        "Transport snapshot".into(),
        Bpm::DEFAULT,
        2_147_483_647,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    let mut transport = Transport::default();
    transport.set_playhead_policy(PlayheadPolicy {
        anchor: true,
        pause_rewinds: false,
    });
    transport.seek(9_007_199_254_740_993, &doc, 0);
    transport.set_speed(Speed::parse("1.25").unwrap(), &doc, 0);
    transport.set_loop(Some((9_007_199_254_740_990, 9_007_199_254_741_999)));
    let stamp = ProjectStamp {
        serial: world.project_serial(),
        revision: 42,
    };
    let snapshot = Snapshot::capture(&transport, &doc, stamp);
    let world = world.with_info(ProjectInfo {
        revision: 42,
        playback: Some(snapshot),
        ..Default::default()
    });
    (world, snapshot)
}
fn worker(caps: Vec<Capability>) -> ScriptWorker {
    ScriptWorker::spawn_plugin(
        Limits::default(),
        caps,
        &Scripts::default(),
        Some(PluginMeta {
            id: "test.transport".into(),
            name: "Transport Test".into(),
            version: "1.0.0".into(),
            root: std::env::temp_dir(),
            data_dir: None,
            config_version: 1,
        }),
    )
    .unwrap()
}
fn run(worker: &mut ScriptWorker, source: &str) -> crate::lua::worker::ScriptResult {
    worker
        .submit(ScriptJob {
            file: "transport.lua".into(),
            source: source.into(),
            world: None,
            time_limit: None,
        })
        .unwrap();
    worker.wait(Duration::from_secs(4)).unwrap()
}
fn delivery(snapshot: Snapshot) -> Delivery {
    Delivery {
        views: [None, None],
        playback: None,
        id: "transport".into(),
        event: Event::new(
            Kind::TransportChanged,
            snapshot.stamp,
            1,
            Data::Transport(snapshot),
        )
        .unwrap(),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    }
}
#[test]
fn native_snapshot_keeps_exact_ticks_loop_anchor_high_ppqn_and_straight_author_tables() {
    let (world, snapshot) = fixture();
    assert!(snapshot.valid());
    let host = Host::new(Limits::default(), [Capability::ReadPlayback]).unwrap();
    host.set_world(Some(world));
    let result=host.run("snapshot.lua",r#"
      assert(plum.has('playback.read') and not plum.has('project.read'))
      local p=plum.playback.snapshot()
      assert(p.schema==1 and p.state=='stopped' and not p.running and p.revision==42)
      assert(math.type(p.tick)=='integer' and p.tick==9007199254740993 and p.anchor==p.tick)
      assert(p.loop.start==9007199254740990 and p.loop.finish==9007199254741999)
      assert(p.speed==1.25 and p.bpm==120 and p.meter.numerator==4 and p.meter.denominator==4)
      assert(p.division.kind=='ppqn' and p.division.ppqn==2147483647)
      p.tick=0;p.loop.start=0;p.meter.numerator=99
      local again=plum.playback.snapshot();assert(again.tick==9007199254740993 and again.loop.start==9007199254740990 and again.meter.numerator==4)
      assert(not pcall(plum.project.current),'transport read must not expose notes')
      return again.tick
    "#).unwrap();
    assert_eq!(result, Plain::Integer(9_007_199_254_740_993));
}
#[test]
fn playback_permission_missing_context_nonfinite_and_tick_overflow_are_explicit() {
    let host = Host::new(Limits::default(), []).unwrap();
    assert_eq!(
        host.run("denied.lua", "return plum.playback.snapshot()")
            .unwrap_err()
            .category,
        Category::Capability
    );
    let host = Host::new(Limits::default(), [Capability::ReadPlayback]).unwrap();
    assert_eq!(
        host.run("none.lua", "return plum.playback.snapshot()")
            .unwrap(),
        Plain::Nil
    );
    let (_, snapshot) = fixture();
    for bits in [
        f64::NAN.to_bits(),
        f64::INFINITY.to_bits(),
        (-1.0f64).to_bits(),
        0f64.to_bits(),
    ] {
        let invalid = Snapshot {
            bpm_bits: bits,
            ..snapshot
        };
        assert!(!invalid.valid());
        assert!(
            Event::new(
                Kind::TransportChanged,
                invalid.stamp,
                1,
                Data::Transport(invalid)
            )
            .is_none()
        );
        let invalid = Snapshot {
            speed_bits: bits,
            ..snapshot
        };
        assert!(!invalid.valid());
    }
    let mut invalid = snapshot;
    invalid.looping = Some((10, 10));
    assert!(!invalid.valid());
    let foreign = ProjectStamp {
        revision: 43,
        ..snapshot.stamp
    };
    assert!(
        Event::new(
            Kind::TransportChanged,
            foreign,
            1,
            Data::Transport(snapshot)
        )
        .is_none()
    );
    let large = Snapshot {
        tick: u64::MAX,
        ..snapshot
    };
    assert!(large.table(&host.lua).is_err());
    for state in [
        TransportState::Stopped,
        TransportState::Playing,
        TransportState::Paused,
        TransportState::Seeking,
        TransportState::AudioUnavailable,
    ] {
        let s = Snapshot { state, ..snapshot };
        let t = s.table(&host.lua).unwrap();
        assert_eq!(t.get::<bool>("running").unwrap(), state.is_running());
    }
    let smpte = Snapshot {
        division: Division::from_raw(0xE350),
        ..snapshot
    };
    let t = smpte
        .table(&host.lua)
        .unwrap()
        .get::<Table>("division")
        .unwrap();
    assert_eq!(t.get::<String>("kind").unwrap(), "smpte");
    assert_eq!(t.get::<u8>("frames_per_second").unwrap(), 29);
}
#[test]
fn transport_observer_worker_needs_only_playback_read_and_cannot_elevate() {
    let (_, snapshot) = fixture();
    let mut w = worker(vec![Capability::ReadPlayback]);
    let r = run(
        &mut w,
        r#"
      plum.plugin.observe{id='transport',events={'transport_changed'},debounce_ms=20,run=function(e)
        assert(e.kind=='transport_changed' and e.read_only and not e.transaction_allowed)
        assert(e.data.tick==9007199254740993 and e.data.loop.finish==9007199254741999)
        local s=plum.playback.snapshot();assert(s.tick==e.data.tick and s.revision==e.revision)
        e.data.tick=0;assert(plum.playback.snapshot().tick==9007199254740993)
        assert(not pcall(plum.project.current));assert(not pcall(function()plum.transaction{}end))
        assert(not pcall(function()plum.plugin.command{id='bad',name='Bad',run=function()end}end))
        print('transport exact isolated')
      end}
      assert(not pcall(function()plum.plugin.observe{id='mixed',events={'transport_changed','content_changed'},run=function()end}end))
      plum.plugin.__phase('ready')
    "#,
    );
    assert!(r.result.is_ok(), "{:?}", r.result);
    w.submit_observation(delivery(snapshot)).unwrap();
    let r = w.wait(Duration::from_secs(4)).unwrap();
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert_eq!(r.output, vec!["transport exact isolated"]);
    assert!(r.commits.is_none() && r.ui.is_none());
    let stale = delivery(snapshot);
    stale.fence.invalidate();
    w.submit_observation(stale).unwrap();
    assert_eq!(
        w.wait(Duration::from_secs(4))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Expired
    );
    let stale = delivery(snapshot);
    stale.lifecycle.invalidate();
    w.submit_observation(stale).unwrap();
    assert_eq!(
        w.wait(Duration::from_secs(4))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Expired
    );
}
#[test]
fn project_observation_grants_do_not_imply_playback_permission() {
    let mut w = worker(vec![Capability::ReadProject, Capability::ObserveProject]);
    let r = run(
        &mut w,
        "plum.plugin.observe{id='transport',events={'transport_changed'},run=function()end}",
    );
    assert_eq!(r.result.unwrap_err().category, Category::Capability);
}
#[test]
fn transport_storm_coalesces_into_one_bounded_exact_snapshot_and_expires_by_revision() {
    let (_, snapshot) = fixture();
    let mut inbox = Inbox::default();
    let defs = Decls {
        observers: vec![ObserverDecl {
            id: "transport".into(),
            kinds: vec![Kind::TransportChanged],
            debounce_ms: 20,
        }],
        ..Default::default()
    };
    let budget = super::super::bridge::NativeBudget::isolated(128 * 1024);
    let now = Instant::now();
    for i in 1..=10_000 {
        let s = Snapshot {
            tick: snapshot.tick + i,
            ..snapshot
        };
        let e = Event::new(Kind::TransportChanged, s.stamp, i, Data::Transport(s)).unwrap();
        inbox.push(&defs, &e, now, &budget).unwrap();
    }
    assert_eq!(inbox.len(), 1);
    assert!(budget.used() < 64 * 1024);
    let d = inbox
        .take(now + Duration::from_millis(25), Some(snapshot.stamp))
        .unwrap();
    assert_eq!(
        (d.event.first_sequence, d.event.sequence, d.event.coalesced),
        (1, 10_000, 9999)
    );
    assert!(!d.event.transaction_allowed);
    assert!(matches!(d.event.data,Data::Transport(s) if s.tick==snapshot.tick+10_000));
    inbox
        .push(&defs, &d.event, now + Duration::from_millis(26), &budget)
        .unwrap();
    assert!(!inbox.due(now + Duration::from_millis(100)));
    assert!(
        inbox
            .take(
                now + Duration::from_millis(200),
                Some(ProjectStamp {
                    revision: 43,
                    ..snapshot.stamp
                })
            )
            .is_none()
    );
    inbox.clear();
    assert!(!d.valid());
    assert_eq!(budget.used(), 0);
}
