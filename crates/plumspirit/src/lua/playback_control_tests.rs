use super::*;
use crate::audio::Transport;
use crate::lua::worker::{ScriptJob, ScriptWorker};
use crate::lua::{Limits, Plain, Scripts, World, handles::ProjectInfo};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument},
};
fn fixture() -> (World, Snapshot) {
    let doc = Arc::new(PlumDocument::new_project(
        "Control fixture".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    let stamp = crate::lua::bridge::ProjectStamp {
        serial: world.project_serial(),
        revision: 0,
    };
    let snapshot = Snapshot::capture(&Transport::default(), &doc, stamp);
    (
        world.with_info(ProjectInfo {
            playback: Some(snapshot),
            ..Default::default()
        }),
        snapshot,
    )
}
fn worker(caps: Vec<Capability>) -> ScriptWorker {
    ScriptWorker::spawn(Limits::default(), caps, &Scripts::default()).unwrap()
}
fn submit(w: &mut ScriptWorker, source: &str, interactive: bool) {
    w.submit_with_interaction(
        ScriptJob {
            file: "controls.lua".into(),
            source: source.into(),
            world: Some(fixture().0),
            time_limit: None,
        },
        interactive,
    )
    .unwrap();
}
fn request(w: &ScriptWorker) -> Request {
    let now = Instant::now();
    loop {
        if let Some(r) = w.poll_playback_request() {
            return r;
        }
        assert!(now.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn caps() -> Vec<Capability> {
    vec![Capability::ReadPlayback, Capability::ControlPlayback]
}
#[test]
fn strict_control_schema_bounds_types_ticks_speed_and_implemented_actions() {
    let lua = Lua::new();
    for (source, want) in [
        ("{action='play'}", Action::Play),
        ("{action='pause'}", Action::Pause),
        ("{action='stop'}", Action::Stop),
        (
            "{action='seek',tick='9007199254740993'}",
            Action::Seek(9_007_199_254_740_993),
        ),
        (
            "{action='speed',speed='1.5'}",
            Action::Speed(Speed::parse("1.5").unwrap()),
        ),
    ] {
        assert_eq!(
            parse(lua.load(format!("return {source}")).eval().unwrap()).unwrap(),
            want
        );
    }
    for source in [
        "setmetatable({action='play'},{})",
        "{action='play',tick=1}",
        "{action='seek',tick=-1}",
        "{action='seek',tick=1/0}",
        "{action='seek',tick=1.5}",
        "{action='seek',tick='18446744073709551615'}",
        "{action='speed',speed=1.5}",
        "{action='speed',speed='nan'}",
        "{action='speed',speed=string.rep('1',65)}",
        "{action='loop',start=0,finish=10}",
        "{action=function()end}",
        "{action='stop',[1]=true}",
    ] {
        assert!(
            parse(lua.load(format!("return {source}")).eval().unwrap()).is_err(),
            "accepted {source}"
        );
    }
}
#[test]
fn native_control_admission_limits_each_plugin_and_global_rate_and_prunes_fixed_memory() {
    let mut a = Admission::default();
    let now = Instant::now();
    let one: Arc<str> = Arc::from("one");
    let two: Arc<str> = Arc::from("two");
    assert_eq!(a.admit(&one, now), Ok(()));
    assert_eq!(
        a.admit(&two, now + Duration::from_millis(124)),
        Err(Error::Busy)
    );
    assert_eq!(a.admit(&two, now + Duration::from_millis(125)), Ok(()));
    assert_eq!(
        a.admit(&one, now + Duration::from_millis(249)),
        Err(Error::Busy)
    );
    assert_eq!(a.admit(&one, now + Duration::from_millis(250)), Ok(()));
    for i in 0..1000 {
        a.admit(
            &Arc::from(format!("plugin-{i}")),
            now + Duration::from_millis(1000 + i * 125),
        )
        .unwrap();
        assert!(a.plugins.len() <= 8);
    }
}
#[test]
fn worker_control_requires_both_grants_and_explicit_native_invocation() {
    for granted in [
        vec![],
        vec![Capability::ReadPlayback],
        vec![Capability::ControlPlayback],
    ] {
        let mut w = worker(granted);
        submit(&mut w, "return plum.playback.request{action='stop'}", true);
        assert_eq!(
            w.wait(Duration::from_secs(3))
                .unwrap()
                .result
                .unwrap_err()
                .category,
            Category::Capability
        );
        assert!(w.poll_playback_request().is_none());
    }
    let mut w = worker(caps());
    submit(&mut w, "return plum.playback.request{action='stop'}", false);
    assert_eq!(
        w.wait(Duration::from_secs(3))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Capability
    );
    assert!(w.poll_playback_request().is_none());
}
#[test]
fn native_request_receipts_update_private_guards_and_thirty_two_is_a_hard_job_cap() {
    let mut w = worker(caps());
    submit(
        &mut w,
        r#"
      for i=1,32 do local r=plum.playback.request{action='seek',tick=i}
        assert(r.request_id==i and r.snapshot.tick==i and r.snapshot.action_revision==i)
        assert(plum.playback.snapshot().tick==0,'captured run snapshot remains immutable')
      end
      local ok,err=pcall(function()plum.playback.request{action='stop'}end)
      assert(not ok and tostring(err):find('budget'));return 'bounded'
    "#,
        true,
    );
    let mut held = None;
    for i in 1..=32 {
        let r = request(&w);
        assert_eq!(r.id, i);
        assert_eq!(r.expected.action_revision, (i - 1) as u64);
        assert_eq!(r.action, Action::Seek(i as u64));
        held = Some(Arc::clone(&r.lease));
        let s = Snapshot {
            tick: i as u64,
            action_revision: i as u64,
            ..r.expected
        };
        r.respond(Ok(s));
    }
    let result = w.wait(Duration::from_secs(3)).unwrap();
    assert_eq!(result.result.unwrap(), Plain::String("bounded".into()));
    assert!(result.commits.is_none());
    assert!(held.unwrap().check().is_err());
    assert!(w.poll_playback_request().is_none());
    submit(
        &mut w,
        "local r=plum.playback.request{action='stop'};return r.request_id",
        true,
    );
    let r = request(&w);
    let snapshot = r.expected;
    r.respond(Ok(snapshot));
    assert_eq!(
        w.wait(Duration::from_secs(3)).unwrap().result.unwrap(),
        Plain::Integer(1)
    );
}
#[test]
fn waiting_control_cancellation_releases_job_and_stale_native_queue_cannot_apply() {
    let mut w = worker(caps());
    submit(
        &mut w,
        "return plum.playback.request{action='seek',tick=123}",
        true,
    );
    let r = request(&w);
    let lease = Arc::clone(&r.lease);
    w.cancel();
    assert_eq!(
        w.wait(Duration::from_secs(3))
            .unwrap()
            .result
            .unwrap_err()
            .category,
        Category::Canceled
    );
    assert!(lease.check().is_err());
    r.respond(Err(Error::Expired));
    submit(&mut w, "return plum.playback.snapshot().state", true);
    assert_eq!(
        w.wait(Duration::from_secs(3)).unwrap().result.unwrap(),
        Plain::String("stopped".into())
    );
}
#[test]
fn one_slot_channel_backpressure_and_plugin_scope_are_enforced_before_effects() {
    let mut w = worker(caps());
    submit(
        &mut w,
        "local r=plum.playback.request{action='stop'};return r.snapshot.state",
        true,
    );
    let r = request(&w);
    let lease = Arc::clone(&r.lease);
    let expected = r.expected;
    let (client, rx) = channel("script");
    client.sender.try_send(r).ok().unwrap();
    let budget = crate::lua::bridge::NativeBudget::isolated(8192);
    assert_eq!(
        client.request(
            Arc::clone(&lease),
            2,
            expected,
            Action::Stop,
            budget.reserve(4096).unwrap()
        ),
        Err(Error::Busy)
    );
    assert_eq!(
        budget.used(),
        0,
        "failed admission releases its reservation"
    );
    let (wrong, _) = channel("different-plugin");
    assert_eq!(
        wrong.request(
            lease,
            2,
            expected,
            Action::Stop,
            budget.reserve(4096).unwrap()
        ),
        Err(Error::Expired)
    );
    assert_eq!(budget.used(), 0);
    rx.try_recv().unwrap().respond(Ok(expected));
    assert_eq!(
        w.wait(Duration::from_secs(3)).unwrap().result.unwrap(),
        Plain::String("stopped".into())
    );
    assert!(rx.try_recv().is_err());
}
