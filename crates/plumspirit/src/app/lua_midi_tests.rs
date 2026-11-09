//! Native App/manager + headless synthesis integration; no desktop/device claim.
use super::*;
use crate::audio::{
    Transport, engine::AudioEngine, midi_overlay::Status as MidiStatus, soundfont::LoadedSoundFont,
};
const MIDI: &str = r#"
plum.plugin.command{id='midi',name='MIDI',run=function()
  local w=plum.playback.plan_writer{max_events=2}
  w:append{tick={plum.playback.tick_after(1),plum.playback.tick_after(2)},status={0x90,0x80},data1={60,60},data2={100,0}}
  local plan=w:finish();assert(plan:stage{output='internal'}.staged);plan:close()
end}
plum.plugin.command{id='inspect',name='Inspect',run=function()print('MIDI STATE '..plum.playback.snapshot().midi_output)end}
plum.plugin.command{id='cancel_midi',name='Cancel MIDI',run=function()assert(plum.playback.cancel_midi().cancellation_requested)end}
"#;
impl Rig {
    fn midi() -> Self {
        Self::midi_source(MIDI)
    }
    fn midi_source(source: &str) -> Self {
        let mut r = Self::with_caps(Some(&format!("{CONTROLS}\n{source}")), false, true);
        {
            use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
            let d = Arc::get_mut(&mut r.h.app.open.as_mut().unwrap().doc).unwrap();
            let notes: Vec<_> = (0..1000)
                .map(|i| NoteRecord {
                    start_tick: i * 120,
                    duration_ticks: 60,
                    attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    sub: Default::default(),
                    group: 0,
                })
                .collect();
            d.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&notes, false)
                .unwrap();
        }
        let font = Arc::new(
            rustysynth::SoundFont::new(&mut &crate::audio::soundfont::tests::tiny_sf2()[..])
                .unwrap(),
        );
        let loaded = LoadedSoundFont {
            path: r.root.join("synthetic.sf2"),
            font,
            file_bytes: 0,
            sample_bytes: 0,
            presets: 1,
            name: "Synthetic native integration".into(),
        };
        r.h.app.transport = Transport::for_test(AudioEngine::headless(48000), loaded);
        let o = r.h.app.open.as_ref().unwrap();
        r.h.app.transport.play(&o.doc, o.generation);
        assert!(r.h.app.transport.midi_resources().is_some());
        r
    }
    fn take_midi(
        &mut self,
    ) -> (
        crate::lua::playback::midi::Intent,
        Arc<crate::lua::reactive::Fence>,
    ) {
        let start = Instant::now();
        loop {
            let prefs = self.h.app.settings.lua_plugins.clone();
            for event in self.h.app.lua_plugins.as_mut().unwrap().poll(&prefs) {
                if let crate::lua::plugins::PluginEvent::TransportMidi {
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
                "no MIDI event {:?}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
#[test]
fn native_owner_accepts_worker_plan_defers_observers_and_preserves_project_history_and_midi() {
    let mut r = Rig::midi();
    let before = plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap();
    let stamp = r.h.app.lua_observation_stamp();
    r.queue_control("midi");
    let (intent, lifecycle) = r.take_midi();
    assert!(intent.valid());
    assert_eq!(
        Arc::strong_count(&r.h.app.open.as_ref().unwrap().doc),
        1,
        "native handoff retains no World"
    );
    r.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
    r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Running);
    assert_eq!(r.h.app.lua_observation_stamp(), stamp);
    assert_eq!(r.h.doc().note_count(), 1000);
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(r.h.doc()).unwrap(),
        before
    );
    // Read-only observations and native user inspections preserve the accepted stream.
    r.queue_control("inspect");
    r.until(|r| r.logs().iter().any(|l| l.contains("MIDI STATE running")));
    assert_eq!(r.h.app.transport.midi_status(), MidiStatus::Running);
    r.queue_control("cancel_midi");
    r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Canceled);
    assert_eq!(r.h.app.lua_observation_stamp(), stamp);
    assert!(r.h.app.transport.state().is_running());
}
#[test]
fn native_pending_midi_checks_permissions_competing_actions_revision_lifecycle_and_busy() {
    for reason in 0..6 {
        let mut r = Rig::midi();
        r.queue_control("midi");
        let (intent, lifecycle) = r.take_midi();
        let replay = intent.clone();
        let stamp = r.h.app.lua_observation_stamp();
        match reason {
            0 => {
                let doc = Arc::clone(&r.h.app.open.as_ref().unwrap().doc);
                r.h.app.transport.seek(100, &doc, 1);
            }
            1 => {
                r.h.app.lua_plugins.as_mut().unwrap().set_granted(
                    ID,
                    "playback.emit_midi",
                    false,
                    &mut r.h.app.settings.lua_plugins,
                );
            }
            2 => lifecycle.invalidate(),
            3 => {
                r.h.app.install_document(
                    PlumDocument::new_project("Replacement".into(), Bpm::DEFAULT, 960),
                    crate::app::DocSource::New,
                    true,
                );
            }
            4 => {
                r.h.app.dialog = Some(crate::app::dialogs::Dialog::Message {
                    title: "Busy".into(),
                    body: "Native test".into(),
                    is_error: false,
                });
            }
            _ => {
                r.h.app
                    .lua_playback_controls
                    .admit(intent.plugin(), Instant::now())
                    .unwrap();
            }
        }
        r.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
        assert!(!replay.valid());
        assert!(replay.take_voice().is_none());
        assert_ne!(r.h.app.transport.midi_status(), MidiStatus::Running);
        assert!(
            r.logs()
                .iter()
                .any(|l| l.contains("Staged MIDI plan discarded")),
            "reason{reason} {:?}",
            r.logs()
        );
        if reason != 3 {
            assert_eq!(r.h.app.lua_observation_stamp(), stamp);
            assert_eq!(r.h.doc().note_count(), 1000);
        }
    }
}
#[test]
fn active_midi_authority_fence_cancels_revoked_disabled_global_off_and_project_replaced_streams() {
    for reason in 0..4 {
        let mut r = Rig::midi();
        r.queue_control("midi");
        let (intent, lifecycle) = r.take_midi();
        r.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
        r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Running);
        match reason {
            0 => {
                r.h.app
                    .settings
                    .lua_plugins
                    .installed
                    .get_mut(ID)
                    .unwrap()
                    .granted
                    .retain(|c| c != "playback.emit_midi");
            }
            1 => {
                r.h.app
                    .settings
                    .lua_plugins
                    .installed
                    .get_mut(ID)
                    .unwrap()
                    .enabled = false;
            }
            2 => {
                r.h.app.settings.lua_plugins.run_plugins = false;
            }
            _ => {
                r.h.app.install_document(
                    PlumDocument::new_project("Replace active".into(), Bpm::DEFAULT, 960),
                    crate::app::DocSource::New,
                    true,
                );
            }
        }
        r.h.app.poll_lua_playback_controls();
        assert!(
            !r.h.app
                .transport
                .midi_watch()
                .is_some_and(|w| w.alive.load(std::sync::atomic::Ordering::Acquire))
        );
    }
}
#[test]
fn authored_pattern_example_runs_through_native_owner_saves_its_preset_and_reports_pending_events()
{
    let example = include_str!("../../examples-lua/midi-pattern-laboratory/main.lua");
    let mut r = Rig::midi_source(example);
    let world = r.h.app.lua_world(true).unwrap();
    r.h.app
        .lua_plugins
        .as_mut()
        .unwrap()
        .run_command_with(ID, "pattern", "{root=60}".into(), Some(world))
        .unwrap();
    let (intent, lifecycle) = r.take_midi();
    r.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
    r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Running);
    assert_eq!(r.h.app.transport.midi_pending(), 17);
    r.queue_control("state");
    r.until(|r| {
        r.logs()
            .iter()
            .any(|l| l.contains("Native plugin MIDI stream: running"))
    });
    assert_eq!(r.h.app.transport.midi_pending(), 17);
    r.queue_control("cancel");
    r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Canceled);
    assert_eq!(r.h.app.transport.midi_pending(), 0);
    assert_eq!(r.h.doc().note_count(), 1000);
    // Preset storage is managed by native plugin data paths; inspect the actual
    // recursively bounded fixture tree rather than assuming the path above.
    fn find(path: &std::path::Path) -> bool {
        std::fs::read_dir(path)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                if e.file_type().unwrap().is_dir() {
                    find(&e.path())
                } else {
                    e.file_name()
                        .to_string_lossy()
                        .contains("Last authored pattern")
                        && std::fs::read_to_string(e.path())
                            .unwrap()
                            .contains("intervals")
                }
            })
    }
    assert!(find(&r.root), "authored preset persisted");
}
#[test]
fn native_busy_policy_refuses_a_second_stream_and_idle_plugin_cancel_stops_accepted_output() {
    let mut r = Rig::midi();
    r.queue_control("midi");
    let (intent, lifecycle) = r.take_midi();
    r.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
    r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Running);
    r.h.app.lua_playback_controls = Default::default();
    r.queue_control("midi");
    let (intent, lifecycle) = r.take_midi();
    r.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
    assert!(
        r.logs()
            .iter()
            .any(|l| l.contains("Another plugin MIDI stream is active"))
    );
    assert_eq!(r.h.app.transport.midi_status(), MidiStatus::Running);
    r.h.app.lua_plugins.as_mut().unwrap().cancel(ID);
    r.until(|r| r.h.app.transport.midi_status() == MidiStatus::Canceled);
    assert!(r.h.app.transport.state().is_running());
    assert_eq!(r.h.doc().note_count(), 1000);
}

#[cfg(windows)]
#[test]
#[ignore = "Manual Windows output device test. Set PLUMSPIRIT_TEST_SF2 to the verified SoundFont."]
fn actual_windows_output_consumes_authored_midi_and_cancels_a_held_note() {
    use crate::audio::{output::OutputConfig, soundfont, synth::SynthConfig};
    use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
    use std::sync::atomic::Ordering;

    let path = std::env::var_os("PLUMSPIRIT_TEST_SF2").expect("Set the verified test SoundFont");
    let soundfont = soundfont::load(std::path::Path::new(&path)).expect("Load test SoundFont");
    let source = format!(
        "{CONTROLS}\n{}",
        MIDI.replace("tick_after(2)", "tick_after(8)")
    );
    let mut rig = Rig::with_caps(Some(&source), false, true);
    // Keep the native transport active without playing a project note during
    // the measurement. The only early samples come from the authored plan.
    let open = rig.h.app.open.as_mut().unwrap();
    let document = Arc::get_mut(&mut open.doc).unwrap();
    let note = NoteRecord::new(
        30 * 960,
        96,
        NoteAttrs::new(60, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
    );
    document
        .track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note], false)
        .unwrap();
    let fingerprint = document.export_fingerprint();

    let engine = AudioEngine::start(
        SynthConfig {
            gain_db: -36.0,
            effects: false,
            ..Default::default()
        },
        &OutputConfig::default(),
        (false, 1000),
    )
    .expect("Open the default Windows audio output");
    let device = engine.device_name.clone();
    let sample_rate = engine.sample_rate;
    let callback = Arc::clone(&engine.shared);
    rig.h.app.transport = Transport::for_test(engine, soundfont);
    let open = rig.h.app.open.as_ref().unwrap();
    rig.h.app.transport.play(&open.doc, open.generation);
    rig.until(|_| {
        callback
            .clock()
            .is_some_and(|anchor| anchor.read > 0 && anchor.took > 0)
    });
    let first_read = callback.clock().unwrap().read;
    assert_eq!(
        f32::from_bits(callback.peak_post.load(Ordering::Acquire)),
        0.0
    );

    rig.queue_control("midi");
    let (intent, lifecycle) = rig.take_midi();
    rig.h.app.apply_lua_midi_intent(ID, intent, &lifecycle);
    rig.until(|r| r.h.app.transport.midi_status() == MidiStatus::Running);
    rig.until(|_| f32::from_bits(callback.peak_post.load(Ordering::Acquire)) > 1e-7);
    let peak = f32::from_bits(callback.peak_post.load(Ordering::Acquire));
    assert!(
        peak.is_finite() && peak < 0.1,
        "Unexpected output peak {peak}"
    );
    // The worker publishes the meter after writing the rendered block. Wait
    // for the physical device callback to consume those samples before cancel.
    let rendered_samples = callback.ring.write_pos();
    assert!(rendered_samples > first_read);
    rig.until(|_| {
        callback
            .clock()
            .is_some_and(|anchor| anchor.took > 0 && anchor.read + anchor.took >= rendered_samples)
    });
    assert_eq!(
        rig.h.app.transport.midi_pending(),
        1,
        "The authored note-off is still scheduled"
    );

    rig.queue_control("cancel_midi");
    rig.until(|r| r.h.app.transport.midi_status() == MidiStatus::Canceled);
    assert_eq!(rig.h.app.transport.midi_pending(), 0);
    assert!(rig.h.app.transport.state().is_running());
    let before_loop = callback.loop_wraps.load(Ordering::Acquire);
    let open = rig.h.app.open.as_ref().unwrap();
    rig.h.app.transport.set_loop(Some((0, 960)));
    rig.h.app.transport.seek(0, &open.doc, open.generation);
    rig.until(|_| callback.loop_wraps.load(Ordering::Acquire) > before_loop);
    assert!(!callback.device_failed.load(Ordering::Acquire));
    rig.h.app.transport.pause();
    assert_eq!(
        rig.h.app.transport.state(),
        crate::audio::TransportState::Paused
    );
    rig.h.app.transport.stop();
    assert_eq!(
        rig.h.app.transport.state(),
        crate::audio::TransportState::Stopped
    );
    rig.h.app.transport.shutdown();
    assert_eq!(rig.h.doc().export_fingerprint(), fingerprint);
    println!(
        "Windows device {device} at {sample_rate} Hz consumed through sample {rendered_samples}. Authored peak {peak:.8}, queued note-off canceled, loop wrapped, pause/stop released output."
    );
}
