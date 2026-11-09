use super::*;
use crate::app::test_support::Harness;
use crate::audio::PlayheadPolicy;
use crate::lua::{
    Scripts,
    plugins::{PluginManager, PluginPrefs, Status},
};
use plumspirit_engine::history::Operation;
use plumspirit_engine::model::{Bpm, PlumDocument, TempoValue, conductor::Interp, tempo::Speed};
use std::path::PathBuf;
use std::sync::Arc;
#[path = "lua_midi_tests.rs"]
mod midi_tests;
const ID: &str = "plumspirit.example.transport-monitor";
struct Rig {
    h: Harness,
    root: PathBuf,
}
impl Rig {
    fn new() -> Self {
        Self::with_source(None)
    }
    fn with_source(source: Option<&str>) -> Self {
        Self::with_options(source, false)
    }
    fn with_options(source: Option<&str>, control: bool) -> Self {
        Self::with_caps(source, control, false)
    }
    fn with_caps(source: Option<&str>, control: bool, midi: bool) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "plumspirit-playback-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut h = Harness::new(PlumDocument::new_project(
            "Transport native".into(),
            Bpm::DEFAULT,
            960,
        ));
        let mut manager = PluginManager::new(Some(root.join("plugins")), Scripts::default());
        manager.set_data_root(Some(root.join("data")));
        let mut prefs = PluginPrefs {
            folder: Some(root.join("plugins")),
            ..Default::default()
        };
        let example =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples-lua/transport-monitor");
        let selected = if let Some(source) = source {
            let selected = root.join("source");
            std::fs::create_dir_all(&selected).unwrap();
            let mut manifest = std::fs::read_to_string(example.join("plum-plugin.toml")).unwrap();
            if control {
                manifest = manifest.replace(
                    "[\"playback.read\"]",
                    "[\"playback.read\",\"playback.control\"]",
                );
            }
            if midi {
                manifest = manifest.replace(
                    "[\"playback.read\"]",
                    "[\"playback.read\",\"playback.emit_midi\"]",
                );
            }
            std::fs::write(selected.join("plum-plugin.toml"), manifest).unwrap();
            std::fs::write(selected.join("main.lua"), source).unwrap();
            selected
        } else {
            example
        };
        manager.install(&selected, false, &mut prefs).unwrap();
        manager.set_granted(ID, "playback.read", true, &mut prefs);
        if control {
            manager.set_granted(ID, "playback.control", true, &mut prefs);
        }
        if midi {
            manager.set_granted(ID, "playback.emit_midi", true, &mut prefs);
        }
        manager.set_enabled(ID, true, &mut prefs);
        h.app.settings.lua_plugins = prefs;
        h.app.lua_plugins = Some(manager);
        h.app.lua_plugins_started = true;
        let mut r = Self { h, root };
        r.until(|r| {
            r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(ID)
                .is_some_and(|p| p.status == Status::Ready && !p.is_busy())
        });
        r.until(|r| r.logs().iter().any(|l| l.starts_with("TRANSPORT ")));
        r
    }
    fn poll(&mut self) {
        self.h.app.poll_lua_plugins(&self.h.ctx);
    }
    fn logs(&self) -> Vec<String> {
        self.h
            .app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .log
            .lines()
            .map(|l| l.text.clone())
            .collect()
    }
    fn until(&mut self, test: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.poll();
            if test(self) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "logs {:?}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut m) = self.h.app.lua_plugins.take() {
            m.stop_all(&self.h.app.settings.lua_plugins);
        }
        let root = self.root.canonicalize().unwrap();
        assert_eq!(
            root.parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plumspirit-playback-test-")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn native_transport_sampling_captures_conductor_loop_speed_and_registered_example_preset() {
    let mut r = Rig::new();
    assert!(
        !r.h.app.lua_plugins.as_ref().unwrap().observing(),
        "transport-only does not activate project-store traversal"
    );
    assert!(r.h.app.lua_plugins.as_ref().unwrap().observing_playback());
    let edit =
        r.h.doc()
            .conductor()
            .prepare_set_tempo(
                None,
                0,
                TempoValue::Bpm(Bpm::parse("120").unwrap()),
                Interp::Linear,
            )
            .unwrap();
    r.h.app
        .apply_op(Operation::Conductor {
            edit,
            label: "Tempo ramp",
        })
        .unwrap();
    let edit =
        r.h.doc()
            .conductor()
            .prepare_set_tempo(
                None,
                1920,
                TempoValue::Bpm(Bpm::parse("240").unwrap()),
                Interp::Step,
            )
            .unwrap();
    r.h.app
        .apply_op(Operation::Conductor {
            edit,
            label: "Ramp endpoint",
        })
        .unwrap();
    let edit =
        r.h.doc()
            .conductor()
            .prepare_set_signature(None, 960, 7, 3)
            .unwrap();
    r.h.app
        .apply_op(Operation::Conductor {
            edit,
            label: "Meter",
        })
        .unwrap();
    r.h.app.transport.set_playhead_policy(PlayheadPolicy {
        anchor: true,
        pause_rewinds: false,
    });
    r.h.app.seek_to(960);
    let generation = r.h.app.doc_generation();
    let doc = Arc::clone(&r.h.app.open.as_ref().unwrap().doc);
    r.h.app
        .transport
        .set_speed(Speed::parse("1.5").unwrap(), &doc, generation);
    drop(doc);
    r.h.app.transport.set_loop(Some((480, 1920)));
    r.until(|r| {
        r.logs()
            .iter()
            .any(|l| l.contains("tick=960 bpm=180.000000 speed=1.500000"))
    });
    let world = r.h.app.lua_world(true).unwrap();
    let native = world.info.playback.unwrap();
    assert_eq!(native.numerator, 7);
    assert_eq!(native.denom_pow, 3);
    r.h.app
        .lua_plugins
        .as_mut()
        .unwrap()
        .run_command(ID, "snapshot", Some(world))
        .unwrap();
    r.until(|r| {
        r.logs()
            .iter()
            .any(|l| l.starts_with("Transport stopped at tick 960;"))
    });
    let preset = r
        .root
        .join("data")
        .join(ID)
        .join("presets/Last transport snapshot.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(preset).unwrap()).unwrap();
    let s = &json["value"]["snapshot"];
    assert_eq!(s["tick"], 960);
    assert_eq!(s["anchor"], 960);
    assert_eq!(s["bpm"], 180.0);
    assert_eq!(s["speed"], 1.5);
    assert_eq!(s["meter"]["numerator"], 7);
    assert_eq!(s["meter"]["denominator"], 8);
    assert_eq!(s["loop"]["start"], 480);
    assert_eq!(s["loop"]["finish"], 1920);
    assert_eq!(
        Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc),
        1,
        "metadata events retain no document root"
    );
    let last = r.h.app.lua_playback.last;
    let sampled = r.h.app.lua_playback.sampled;
    r.h.app.sample_lua_playback();
    assert_eq!(last, r.h.app.lua_playback.last);
    assert_eq!(sampled, r.h.app.lua_playback.sampled);
}
#[test]
fn transport_sampler_is_inert_without_subscribers_and_revoke_and_close_clear_state() {
    let mut h = Harness::new(PlumDocument::new_project(
        "Inactive".into(),
        Bpm::DEFAULT,
        960,
    ));
    h.app.sample_lua_playback();
    assert!(h.app.lua_playback.last.is_none());
    let mut r = Rig::new();
    assert!(r.h.app.lua_playback.last.is_some());
    let prefs = &mut r.h.app.settings.lua_plugins;
    r.h.app
        .lua_plugins
        .as_mut()
        .unwrap()
        .set_granted(ID, "playback.read", false, prefs);
    r.h.app.sample_lua_playback();
    assert!(r.h.app.lua_playback.last.is_none());
    r.h.app.open = None;
    r.poll();
    assert!(r.h.app.lua_playback.last.is_none());
}

#[test]
fn newer_positions_coalesce_without_starving_active_worker_and_revision_fences_still_cancel() {
    let source = include_str!("../../examples-lua/transport-monitor/main.lua").replace(
        "observed=observed+1",
        r#"
      if snapshot.tick==9001 then
        plum.progress(.5,'transport handler active')
        local finish=os.clock()+.10
        while os.clock()<finish do plum.yield() end
      end
      print('REV '..snapshot.revision..' tick='..snapshot.tick)
      observed=observed+1
    "#,
    );
    let mut r = Rig::with_source(Some(&source));
    r.h.app.seek_to(9001);
    r.until(|r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
    });
    let active = r.h.app.lua_playback_snapshot().unwrap();
    for tick in 9002..=10_001 {
        let s = Snapshot { tick, ..active };
        r.h.app
            .publish_lua_observation(Kind::TransportChanged, Data::Transport(s));
    }
    r.until(|r| r.logs().iter().any(|s| s.contains("tick=9001 ")));
    r.until(|r| r.logs().iter().any(|s| s.contains("tick=10001 ")));
    assert!(
        !r.logs()
            .iter()
            .any(|s| s.contains("time limit") || s.contains("canceled"))
    );
    // Actual native revision change still invalidates a running observation.
    r.h.app.seek_to(9002);
    r.until(|r| r.logs().iter().any(|s| s.contains("tick=9002 ")));
    let old_revision = r.h.app.lua_observation_stamp().unwrap().revision;
    let old_line = format!("REV {old_revision} tick=9001");
    let before = r.logs().iter().filter(|s| **s == old_line).count();
    r.h.app.seek_to(9001);
    r.until(|r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
    });
    r.h.app.apply_op(Operation::AddTrack).unwrap();
    r.poll();
    let new_revision = r.h.app.lua_observation_stamp().unwrap().revision;
    let new_line = format!("REV {new_revision} tick=9001");
    r.until(|r| r.logs().contains(&new_line));
    assert_eq!(
        r.logs().iter().filter(|s| **s == old_line).count(),
        before,
        "old revision cannot finish after native context invalidation and a new revision delivery"
    );
}

const CONTROLS: &str = r#"
plum.require_api(0,2)
plum.plugin.observe{id='transport',events={'transport_changed'},debounce_ms=20,run=function(e)print('TRANSPORT '..e.data.tick)end}
for _,action in ipairs{'seek','speed','stop','pause','play'} do
  plum.plugin.command{id=action,name=action,run=function()
    local opts={action=action}
    if action=='seek' then opts.tick=9007199254740993 elseif action=='speed' then opts.speed='1.5' end
    local receipt=plum.playback.request(opts)
    print('CONTROL '..action..' tick='..receipt.snapshot.tick..' speed='..receipt.snapshot.speed)
  end}
end
"#;
impl Rig {
    fn queue_control(&mut self, action: &str) {
        let world = self.h.app.lua_world(true).unwrap();
        self.h
            .app
            .lua_plugins
            .as_mut()
            .unwrap()
            .run_command(ID, action, Some(world))
            .unwrap();
    }
    fn take_control(&mut self) -> crate::lua::playback::control::Request {
        let start = Instant::now();
        loop {
            if let Some(r) = self
                .h
                .app
                .lua_plugins
                .as_ref()
                .unwrap()
                .poll_playback_request()
            {
                return r;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "request missing {:?}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
#[test]
fn actual_native_control_seek_speed_pause_stop_and_empty_play_follow_owner_and_rate_limits() {
    let mut r = Rig::with_options(Some(CONTROLS), true);
    let before = r.h.app.lua_observation_stamp().unwrap();
    r.queue_control("seek");
    r.until(|r| {
        r.logs()
            .iter()
            .any(|s| s.contains("CONTROL seek tick=9007199254740993"))
    });
    assert_eq!(r.h.app.transport.position(), 9_007_199_254_740_993);
    assert_eq!(r.h.app.lua_observation_stamp(), Some(before));
    // Controlled native admission clock reset advances only this test's rate fixture.
    r.h.app.lua_playback_controls = Default::default();
    r.queue_control("speed");
    r.until(|r| {
        r.logs()
            .iter()
            .any(|s| s.contains("CONTROL speed") && s.contains("speed=1.5"))
    });
    assert_eq!(r.h.app.transport.speed().to_f64(), 1.5);
    assert_eq!(r.h.app.settings.speed.to_f64(), 1.5);
    r.queue_control("pause");
    r.until(|r| {
        !r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .is_busy()
    });
    assert!(r.logs().iter().any(|s| s.contains("rate-limited")));
    for action in ["pause", "stop", "play"] {
        r.h.app.lua_playback_controls = Default::default();
        r.queue_control(action);
        r.until(|r| {
            r.logs()
                .iter()
                .any(|s| s.starts_with(&format!("CONTROL {action} ")))
        });
    }
    assert_eq!(
        r.h.app.transport.state(),
        crate::audio::TransportState::Stopped,
        "empty native project does not start a device"
    );
    assert_eq!(r.h.doc().note_count(), 0);
    assert_eq!(r.h.app.lua_observation_stamp(), Some(before));
}
#[test]
fn queued_native_controls_reject_competing_user_seek_project_revision_and_revocation() {
    for reason in [
        "competing",
        "device_config",
        "route",
        "soundfont",
        "stale_revision",
        "replace",
        "revoke",
        "cancel",
    ] {
        let mut r = Rig::with_options(Some(CONTROLS), true);
        r.queue_control("seek");
        let mut request = r.take_control();
        match reason {
            "competing" => r.h.app.seek_to(123),
            "device_config" => {
                r.h.app
                    .transport
                    .configure_output(crate::audio::output::OutputConfig {
                        sample_rate: Some(44100),
                        ..Default::default()
                    });
            }
            "route" => {
                r.h.app
                    .transport
                    .set_default_output(crate::audio::midi_out::OutputTarget::Device(
                        "Source-only unopened route".into(),
                    ));
            }
            "soundfont" => {
                r.h.app
                    .transport
                    .load_soundfont(r.root.join("missing-test-font.sf2"));
            }
            "stale_revision" => {
                request.expected.stamp.revision += 1;
            }
            "replace" => {
                r.h.app.install_document(
                    PlumDocument::new_project("Replacement".into(), Bpm::DEFAULT, 960),
                    crate::app::DocSource::New,
                    true,
                );
            }
            "revoke" => {
                let prefs = &mut r.h.app.settings.lua_plugins;
                r.h.app.lua_plugins.as_mut().unwrap().set_granted(
                    ID,
                    "playback.control",
                    false,
                    prefs,
                );
            }
            "cancel" => {
                r.h.app.lua_plugins.as_mut().unwrap().cancel(ID);
            }
            _ => unreachable!(),
        }
        r.h.app.apply_lua_playback_control(request);
        r.until(|r| {
            !r.h.app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(ID)
                .unwrap()
                .is_busy()
        });
        assert_eq!(
            r.h.app.transport.position(),
            if reason == "competing" { 123 } else { 0 },
            "{reason}"
        );
        assert!(
            !r.logs().iter().any(|s| s.starts_with("CONTROL seek ")),
            "no stale native effect/receipt: {reason}"
        );
    }
}

const LOOPS: &str = r#"
plum.plugin.command{id='loop_on',name='Stage loop on',run=function()
  local r=plum.playback.stage_loop{start=240,finish=1920,enabled=true}
  assert(r.staged and r.start==240);print('STAGED loop on')
end}
plum.plugin.command{id='loop_off',name='Stage loop off',run=function()
  plum.playback.stage_loop{start=240,finish=1920,enabled=false};print('STAGED loop off')
end}
plum.plugin.command{id='loop_clear',name='Stage loop clear',run=function()
  plum.playback.stage_loop{clear=true};print('STAGED loop clear')
end}
plum.plugin.command{id='loop_fail',name='Fail staged loop',run=function()
  plum.playback.stage_loop{start=0,finish=100};error('failure after staging')
end}
plum.plugin.command{id='loop_cancel',name='Cancel staged loop',run=function()
  plum.playback.stage_loop{start=0,finish=100};plum.progress(.5,'staged loop waiting')
  while true do plum.yield() end
end}
"#;
impl Rig {
    fn loops() -> Self {
        Self::with_options(Some(&format!("{CONTROLS}\n{LOOPS}")), true)
    }
    fn take_loop(
        &mut self,
    ) -> (
        crate::lua::playback::loop_intent::Intent,
        Arc<crate::lua::reactive::Fence>,
    ) {
        let start = Instant::now();
        loop {
            let prefs = self.h.app.settings.lua_plugins.clone();
            let events = self.h.app.lua_plugins.as_mut().unwrap().poll(&prefs);
            for event in events {
                if let crate::lua::plugins::PluginEvent::TransportLoop {
                    plugin,
                    intent,
                    lifecycle,
                } = event
                {
                    assert_eq!(plugin, ID);
                    return (intent, lifecycle);
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "no loop completion {:?}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
#[test]
fn native_staged_loop_releases_worker_world_sets_disables_clears_and_roundtrips_existing_lup() {
    let mut r = Rig::loops();
    {
        use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
        let doc = Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc).unwrap();
        let notes: Vec<_> = (0..1000)
            .map(|i| NoteRecord {
                start_tick: i * 120,
                duration_ticks: 60,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, false)
            .unwrap();
    }
    let before_midi = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    let before_state = r.h.app.lua_observation_stamp().unwrap();
    r.queue_control("loop_on");
    let (intent, lifecycle) = r.take_loop();
    assert!(intent.valid());
    assert_eq!(
        Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc),
        1,
        "completed intent carries no World/root"
    );
    let clone = intent.clone();
    r.h.app.apply_lua_loop_intent(ID, intent, &lifecycle);
    assert!(!clone.valid(), "consumed exactly once");
    assert_eq!(r.h.app.loop_config().range, Some((240, 1920)));
    assert!(r.h.app.loop_config().enabled);
    assert_eq!(r.h.app.transport.loop_range(), Some((240, 1920)));
    assert_eq!(r.h.doc().note_count(), 1000);
    assert_eq!(
        r.h.app.lua_observation_stamp(),
        Some(before_state),
        "native project preference outside musical Undo"
    );
    let bytes = plumspirit_engine::native::project_to_bytes(r.h.doc()).unwrap();
    let (reopened, _) = plumspirit_engine::native::project_from_bytes(&bytes).unwrap();
    assert_eq!(reopened.project().looping, r.h.app.loop_config());
    assert_eq!(reopened.note_count(), 1000);
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        before_midi
    );
    r.h.app.lua_playback_controls = Default::default();
    r.queue_control("loop_off");
    r.until(|r| {
        !r.h.app.loop_config().enabled
            && !r
                .h
                .app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(ID)
                .unwrap()
                .is_busy()
    });
    assert_eq!(r.h.app.loop_config().range, Some((240, 1920)));
    assert_eq!(r.h.app.transport.loop_range(), None);
    r.h.app.lua_playback_controls = Default::default();
    r.queue_control("loop_clear");
    r.until(|r| r.h.app.loop_config().range.is_none());
    assert_eq!(r.h.doc().note_count(), 1000);
}
#[test]
fn failed_and_actually_canceled_loop_commands_leave_native_project_and_transport_untouched() {
    let mut r = Rig::loops();
    r.queue_control("loop_fail");
    r.until(|r| {
        !r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .is_busy()
    });
    assert!(r.h.app.loop_config().range.is_none());
    assert!(r.h.app.transport.loop_range().is_none());
    r.queue_control("loop_cancel");
    r.until(|r| {
        r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .progress()
            .is_some_and(|p| p.fraction == Some(0.5))
    });
    r.h.app.lua_plugins.as_mut().unwrap().cancel(ID);
    r.until(|r| {
        !r.h.app
            .lua_plugins
            .as_ref()
            .unwrap()
            .plugin(ID)
            .unwrap()
            .is_busy()
    });
    assert!(r.h.app.loop_config().range.is_none());
    assert!(r.h.app.transport.loop_range().is_none());
}
#[test]
fn completed_loop_intents_are_discarded_after_competing_action_revoke_reload_replace_or_shared_root()
 {
    for reason in ["competing", "revoke", "reload", "replace", "shared"] {
        let mut r = Rig::loops();
        r.queue_control("loop_on");
        let (intent, lifecycle) = r.take_loop();
        let mut held = None;
        match reason {
            "competing" => r.h.app.seek_to(123),
            "revoke" => {
                let prefs = &mut r.h.app.settings.lua_plugins;
                r.h.app.lua_plugins.as_mut().unwrap().set_granted(
                    ID,
                    "playback.control",
                    false,
                    prefs,
                );
            }
            "reload" => {
                let prefs = r.h.app.settings.lua_plugins.clone();
                r.h.app.lua_plugins.as_mut().unwrap().reload(ID, &prefs);
            }
            "replace" => r.h.app.install_document(
                PlumDocument::new_project("Replacement".into(), Bpm::DEFAULT, 960),
                crate::app::DocSource::New,
                true,
            ),
            "shared" => held = Some(Arc::clone(&r.h.app.open.as_ref().unwrap().doc)),
            _ => unreachable!(),
        }
        let clone = intent.clone();
        r.h.app.apply_lua_loop_intent(ID, intent, &lifecycle);
        assert!(!clone.valid(), "refused intent consumed: {reason}");
        assert!(r.h.app.loop_config().range.is_none(), "stale loop {reason}");
        assert!(r.h.app.transport.loop_range().is_none());
        if reason == "shared" {
            assert_eq!(
                Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc),
                2,
                "no COW clone just to set loop"
            );
        }
        drop(held);
    }
}

#[test]
fn disabled_loop_range_remains_in_native_snapshot_and_changes_invalidate_staged_guard() {
    let mut r = Rig::loops();
    r.h.app.set_loop_range(Some((240, 1920)), false);
    let first = r.h.app.lua_playback_snapshot().unwrap();
    assert_eq!(first.looping, None);
    assert_eq!(first.configured_loop.range, Some((240, 1920)));
    assert!(!first.configured_loop.enabled);
    r.queue_control("loop_on");
    let (intent, lifecycle) = r.take_loop();
    r.h.app.set_loop_range(Some((480, 3840)), false);
    let second = r.h.app.lua_playback_snapshot().unwrap();
    assert_ne!(first, second);
    r.h.app.apply_lua_loop_intent(ID, intent, &lifecycle);
    assert_eq!(r.h.app.loop_config().range, Some((480, 3840)));
    assert!(!r.h.app.loop_config().enabled);
}
