use super::*;
use crate::{
    audio::{
        Transport, TransportState,
        midi_overlay::{Life, Resources},
    },
    lua::{
        Limits, Plain, Scripts, World,
        handles::ProjectInfo,
        worker::{ScriptJob, ScriptWorker},
    },
};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument, TempoMap},
};
use std::{sync::atomic::AtomicU8, time::Duration};
const STAGE: &str = r#"
    local w=plum.playback.plan_writer{max_events=2}
    w:append{tick={960,1920},status={0x90,0x80},data1={60,60},data2={100,0}}
    local p=w:finish();local s=p:stage{output='internal'}
    assert(s.staged and s.output=='internal' and s.count==2)
"#;
#[test]
fn published_permissioned_midi_example_executes_verbatim_with_captured_native_resources() {
    let docs = include_str!("../../../../../../docs/LUA_API.md");
    let section = docs
        .split("## Isolated native MIDI plan output (API 0.2)")
        .nth(1)
        .unwrap();
    let example = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let h = host(caps(), true);
    h.run("published-midi.lua", example).unwrap();
    assert!(h.take_plugin_out().midi_intent.is_some());
    assert!(h.take_commits().is_none());
}
fn world() -> World {
    let doc = Arc::new(PlumDocument::new_project(
        "MIDI staging fixture".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    let mut snapshot = Snapshot::capture(
        &Transport::default(),
        &doc,
        crate::lua::bridge::ProjectStamp {
            serial: world.project_serial(),
            revision: 0,
        },
    );
    snapshot.state = TransportState::Playing;
    let font = Arc::new(
        rustysynth::SoundFont::new(&mut &crate::audio::soundfont::tests::tiny_sf2()[..]).unwrap(),
    );
    world.with_info(ProjectInfo {
        playback: Some(snapshot),
        playback_resources: Some(Resources {
            tempo: Arc::new(TempoMap::for_document(&doc, 48000)),
            font,
            sample_rate: 48000,
        }),
        ..Default::default()
    })
}
fn caps() -> Vec<Capability> {
    vec![
        Capability::ReadPlayback,
        Capability::EmitMidi,
        Capability::ReadProject,
        Capability::EditNotes,
        Capability::ControlPlayback,
    ]
}
fn host(grants: Vec<Capability>, interactive: bool) -> Host {
    let h = Host::new(
        Limits {
            time: Duration::from_secs(10),
            ..Default::default()
        },
        grants,
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    h.set_world(Some(world()));
    h.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = interactive;
    h
}
fn accepted(intent: &Intent) -> Life {
    Life {
        alive: Arc::new(AtomicBool::new(true)),
        status: Arc::new(AtomicU8::new(1)),
        pending: Arc::new(std::sync::atomic::AtomicU32::new(2)),
        plugin: Arc::new(AtomicBool::new(true)),
        epoch: intent.cancel_epoch(),
        version: intent.version(),
    }
}
#[test]
fn staged_midi_requires_both_separate_grants_explicit_invocation_and_strict_internal_output() {
    for grants in [
        vec![],
        vec![Capability::ReadPlayback],
        vec![Capability::EmitMidi],
    ] {
        let h = host(grants, true);
        assert_eq!(
            h.run("grants.lua", STAGE).unwrap_err().category,
            Category::Capability
        );
        assert!(h.take_plugin_out().midi_intent.is_none());
    }
    assert_eq!(
        host(caps(), false)
            .run("automatic.lua", STAGE)
            .unwrap_err()
            .category,
        Category::Capability
    );
    host(caps(),true).run("options.lua",r#"
        local w=plum.playback.plan_writer{max_events=2}
        w:append{tick={960,1920},status={0x90,0x80},data1={60,60},data2={100,0}}
        local p=w:finish()
        for _,o in ipairs{{},{output='external'},{output='internal',route=1},setmetatable({output='internal'},{}),{[1]='internal'}}do
            assert(not pcall(function()p:stage(o)end))
        end
        assert(p:info().count==2)
    "#).unwrap();
}
#[test]
fn intent_is_root_free_one_shot_and_native_charge_follows_the_prepared_voice() {
    let h = host(caps(), true);
    let budget = NativeBudget::isolated(64 * 1024 * 1024);
    h.lua.set_app_data(budget.clone());
    h.run(
        "stage.lua",
        &format!("{STAGE}\nassert(not pcall(function()p:stage{{output='internal'}}end));p:close()"),
    )
    .unwrap();
    h.lua.gc_collect().unwrap();
    let intent = h.take_plugin_out().midi_intent.unwrap();
    let clone = intent.clone();
    assert_eq!(
        budget.used(),
        SYNTH_BYTES + 2 * size_of::<plumspirit_engine::playback::midi_plan::FrameEvent>() + 4096
    );
    assert!(h.take_commits().is_none());
    let voice = intent.take_voice().unwrap();
    assert!(!intent.valid());
    assert!(clone.take_voice().is_none());
    drop(intent);
    drop(clone);
    assert!(budget.used() > SYNTH_BYTES);
    drop(voice);
    assert_eq!(budget.used(), 0);
}
#[test]
fn standalone_staging_excludes_transactions_controls_and_loop_intents_without_changing_api01() {
    let h = host(caps(), true);
    h.run(
        "transaction.lua",
        &format!(
            "local tx=plum.transaction{{}};assert(not pcall(function(){STAGE}end));tx:cancel()"
        ),
    )
    .unwrap();
    h.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    h.run("standalone.lua",&format!("{STAGE}\nassert(not pcall(function()plum.transaction{{}}end));assert(not pcall(function()plum.playback.request{{action='pause'}}end));assert(not pcall(function()plum.playback.stage_loop{{clear=true}}end))" )).unwrap();
    assert!(h.take_commits().is_none());
    assert!(h.take_plugin_out().midi_intent.is_some());
    h.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    h.run(
        "old-transaction.lua",
        "local tx=plum.transaction{};tx:cancel()",
    )
    .unwrap();
}
#[test]
fn failed_superseded_canceled_and_budget_denied_jobs_never_publish_or_leak_prepared_voices() {
    let h = host(caps(), true);
    let budget = NativeBudget::isolated(64 * 1024 * 1024);
    h.lua.set_app_data(budget.clone());
    assert!(
        h.run("failed.lua", &format!("{STAGE}\nerror('failed')"))
            .is_err()
    );
    assert!(h.take_plugin_out().midi_intent.is_none());
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    h.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    h.run("pending.lua", STAGE).unwrap();
    let pending = h.take_plugin_out().midi_intent.unwrap();
    assert!(pending.valid());
    h.run("next.lua", "return 1").unwrap();
    assert!(!pending.valid());
    assert!(pending.take_voice().is_none());
    drop(pending);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    let tiny = NativeBudget::isolated(SYNTH_BYTES - 1);
    h.lua.set_app_data(tiny.clone());
    h.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    assert_eq!(
        h.run("preflight.lua", STAGE).unwrap_err().category,
        Category::Budget
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(tiny.used(), 0);
    assert!(h.take_plugin_out().midi_intent.is_none());
}
#[test]
fn accepted_life_survives_read_only_runs_and_global_cancel_cannot_be_revived_by_flag_reuse() {
    let h = host(caps(), true);
    let scripts = Scripts::default();
    scripts.add(&h);
    h.run("accepted.lua", STAGE).unwrap();
    let intent = h.take_plugin_out().midi_intent.unwrap();
    let life = accepted(&intent);
    let voice = intent.take_voice().unwrap();
    h.run("read.lua", "return plum.playback.snapshot().midi_output")
        .unwrap();
    assert!(life.active());
    assert_eq!(scripts.cancel_all(), 1);
    assert!(!life.active());
    assert_eq!(
        h.run("new.lua", "return 'next'").unwrap(),
        Plain::String("next".into())
    );
    assert!(!life.active());
    drop(voice);
}
#[test]
fn explicit_midi_cancellation_is_bounded_and_invalidates_only_own_native_epoch() {
    let h = host(caps(), true);
    h.run("accepted.lua", STAGE).unwrap();
    let intent = h.take_plugin_out().midi_intent.unwrap();
    let life = accepted(&intent);
    let voice = intent.take_voice().unwrap();
    h.lua
        .app_data_mut::<crate::lua::bridge::Session>()
        .unwrap()
        .interactive = true;
    h.run(
        "cancel.lua",
        r#"
        for i=1,32 do assert(plum.playback.cancel_midi().cancellation_requested)end
        assert(not pcall(function()plum.playback.cancel_midi()end))
    "#,
    )
    .unwrap();
    assert!(!life.active());
    drop(voice);
}
#[test]
fn real_worker_cancel_epoch_stays_canceled_after_another_worker_job() {
    let mut w = ScriptWorker::spawn(Limits::default(), caps(), &Scripts::default()).unwrap();
    w.submit_with_interaction(
        ScriptJob {
            file: "midi.lua".into(),
            source: STAGE.into(),
            world: Some(world()),
            time_limit: None,
        },
        true,
    )
    .unwrap();
    let r = w.wait(Duration::from_secs(3)).unwrap();
    r.result.unwrap();
    let intent = r.plugin.midi_intent.unwrap();
    let life = accepted(&intent);
    let voice = intent.take_voice().unwrap();
    assert!(life.active());
    w.cancel();
    assert!(!life.active());
    w.submit(ScriptJob {
        file: "read.lua".into(),
        source: "return 1".into(),
        world: Some(world()),
        time_limit: None,
    })
    .unwrap();
    w.wait(Duration::from_secs(3)).unwrap().result.unwrap();
    assert!(!life.active());
    drop(voice);
}
#[test]
fn captured_tick_offsets_use_native_speed_map_and_reject_non_finite_or_missing_authority() {
    let h = host(vec![Capability::ReadPlayback], false);
    h.run("offset.lua",r#"
        assert(plum.playback.tick_after(0)==0)
        assert(plum.playback.tick_after(0.5)==960)
        assert(plum.playback.tick_after(1.5)==2880)
        assert(plum.playback.tick_after(60)==115200)
        for _,v in ipairs{-1,61,0/0,1/0}do assert(not pcall(function()plum.playback.tick_after(v)end))end
    "#).unwrap();
    assert_eq!(
        host(vec![], false)
            .run("offset-denied.lua", "return plum.playback.tick_after(1)")
            .unwrap_err()
            .category,
        Category::Capability
    );
    use crate::lua::reactive::{Data, Delivery, Event, Fence, Kind, Scope};
    let mut observed = h
        .lua
        .app_data_ref::<crate::lua::handles::WorldSlot>()
        .unwrap()
        .0
        .as_ref()
        .unwrap()
        .info
        .playback
        .unwrap();
    observed.tick = 1920;
    h.lua.app_data_mut::<Scope>().unwrap().delivery = Some(Delivery {
        views: [None, None],
        playback: None,
        id: "clock".into(),
        event: Event::new(
            Kind::TransportChanged,
            observed.stamp,
            1,
            Data::Transport(observed),
        )
        .unwrap(),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    });
    h.run(
        "event-offset.lua",
        "assert(plum.playback.tick_after(0)==1920);assert(plum.playback.tick_after(0.5)==2880)",
    )
    .unwrap();
    h.lua
        .app_data_mut::<Scope>()
        .unwrap()
        .delivery
        .as_mut()
        .unwrap()
        .event
        .data = Data::Transport(Snapshot {
        action_revision: observed.action_revision + 1,
        ..observed
    });
    assert_eq!(
        h.run("event-stale-map.lua", "return plum.playback.tick_after(1)")
            .unwrap_err()
            .category,
        Category::Expired
    );
}
