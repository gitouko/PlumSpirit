use super::*;
use crate::{
    audio::{
        engine::{AudioEngine, Command},
        synth::SynthConfig,
    },
    lua::bridge::NativeBudget,
};
use plumspirit_engine::{
    model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument},
    playback::{PlaybackIndex, midi_plan::Event},
};
use std::time::{Duration, Instant};

pub(crate) fn font() -> Arc<SoundFont> {
    Arc::new(SoundFont::new(&mut &crate::audio::soundfont::tests::tiny_sf2()[..]).unwrap())
}
fn document() -> PlumDocument {
    let mut doc = PlumDocument::new_project("MIDI voice isolation".into(), Bpm::DEFAULT, 480);
    let note = NoteRecord {
        start_tick: 0,
        duration_ticks: 3840,
        attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    };
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note], false)
        .unwrap();
    doc
}
fn life() -> Life {
    Life {
        alive: Arc::new(AtomicBool::new(true)),
        status: Arc::new(AtomicU8::new(Status::Queued as u8)),
        pending: Arc::new(std::sync::atomic::AtomicU32::new(2)),
        plugin: Arc::new(AtomicBool::new(true)),
        epoch: Arc::new(AtomicU64::new(0)),
        version: 0,
    }
}
fn voice(resources: Resources, first: u64, last: u64) -> (PreparedVoice, NativeBudget) {
    let events = [
        Event {
            tick: first,
            status: 0x90,
            data1: 60,
            data2: 100,
        },
        Event {
            tick: last,
            status: 0x80,
            data1: 60,
            data2: 0,
        },
    ];
    let p = Prepared::compile(&events, &resources.tempo, || Ok::<(), ()>(())).unwrap();
    let budget = NativeBudget::isolated(64 * 1024 * 1024);
    let charge = budget.reserve(SYNTH_BYTES + 4096).unwrap();
    (PreparedVoice::new(p, resources, charge).unwrap(), budget)
}
fn wait_status(status: &AtomicU8, want: Status) {
    let end = Instant::now() + Duration::from_secs(3);
    while Status::decode(status.load(Ordering::Acquire)) != want {
        assert!(
            Instant::now() < end,
            "status {:?}, wanted {want:?}",
            Status::decode(status.load(Ordering::Acquire))
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn engine(index: Arc<PlaybackIndex>, font: Arc<SoundFont>) -> AudioEngine {
    let e = AudioEngine::headless_with(
        48000,
        SynthConfig {
            effects: false,
            limiter: false,
            gain_db: -20.0,
            max_voices: 64,
            ..Default::default()
        },
    );
    assert!(e.send(Command::SetSoundFont(font)));
    assert!(e.send(Command::Play {
        index,
        tick: 0,
        generation: 1,
        looping: None,
        restrike: false
    }));
    e
}
#[test]
fn real_native_headless_output_mixes_isolated_voices_without_ending_same_key_song_note() {
    let doc = document();
    let index = Arc::new(PlaybackIndex::build(&doc, 48000));
    let font = font();
    let resources = Resources {
        tempo: Arc::clone(&index.tempo),
        font: Arc::clone(&font),
        sample_rate: 48000,
    };
    let (voice, budget) = voice(resources, 480, 960); // .5s on,1s off;1.5s terminal cleanup
    let life = life();
    let state = Arc::clone(&life.status);
    let alive = Arc::clone(&life.alive);
    let song = engine(Arc::clone(&index), Arc::clone(&font));
    let mixed = engine(index, font);
    assert!(mixed.send(Command::PluginMidi {
        voice: Box::new(voice),
        life
    }));
    wait_status(&state, Status::Running);
    let baseline = song.pump(96000, &mut (0.0, 0.0));
    let actual = mixed.pump(96000, &mut (0.0, 0.0));
    assert!(
        baseline[160000..].iter().any(|x| x.abs() > 0.000001),
        "song sustains after plugin off"
    );
    assert!(
        baseline[50000..90000]
            .iter()
            .zip(&actual[50000..90000])
            .any(|(a, b)| (a - b).abs() > 0.000001),
        "actual plugin signal is mixed"
    );
    assert_eq!(
        &actual[160000..],
        &baseline[160000..],
        "plugin note-off must not end the song's same-channel/key voice"
    );
    wait_status(&state, Status::Finished);
    assert!(!alive.load(Ordering::Acquire));
    assert_eq!(budget.used(), 0);
    assert_eq!(doc.note_count(), 1);
    assert_eq!(mixed.shared.active_voices.load(Ordering::Acquire), 1);
}
#[test]
fn native_worker_refuses_late_out_of_range_wrong_map_font_sample_rate_and_prerender_plans() {
    let doc = document();
    let index = Arc::new(PlaybackIndex::build(&doc, 48000));
    let font = font();
    for case in 0..5 {
        let resources = Resources {
            tempo: if case == 2 {
                Arc::new((*index.tempo).clone())
            } else {
                Arc::clone(&index.tempo)
            },
            font: if case == 3 {
                self::font()
            } else {
                Arc::clone(&font)
            },
            sample_rate: 48000,
        };
        let (voice, budget) = voice(
            resources,
            if case == 0 { 1 } else { 480 },
            if case == 1 { 20000 } else { 960 },
        );
        let life = life();
        let state = Arc::clone(&life.status);
        let e = if case == 4 {
            let e = AudioEngine::headless_render(
                48000,
                SynthConfig::default(),
                crate::audio::engine::RenderConfig::new(true, 250, 48000),
            );
            e.send(Command::SetSoundFont(Arc::clone(&font)));
            e.send(Command::Play {
                index: Arc::clone(&index),
                tick: 0,
                generation: 1,
                looping: None,
                restrike: false,
            });
            e
        } else {
            engine(Arc::clone(&index), Arc::clone(&font))
        };
        e.send(Command::PluginMidi {
            voice: Box::new(voice),
            life,
        });
        wait_status(&state, Status::Refused);
        assert_eq!(budget.used(), 0, "case {case}");
    }
    let resources = Resources {
        tempo: Arc::new(PlaybackIndex::build(&doc, 44100).tempo.as_ref().clone()),
        font: Arc::clone(&font),
        sample_rate: 44100,
    };
    let (voice, budget) = voice(resources, 480, 960);
    let life = life();
    let state = Arc::clone(&life.status);
    let e = engine(index, font);
    e.send(Command::PluginMidi {
        voice: Box::new(voice),
        life,
    });
    wait_status(&state, Status::Refused);
    assert_eq!(budget.used(), 0);
}
#[test]
fn accepted_native_stream_cleans_up_on_lifecycle_epoch_and_transport_changes() {
    let doc = document();
    let index = Arc::new(PlaybackIndex::build(&doc, 48000));
    let font = font();
    for reason in 0..5 {
        let resources = Resources {
            tempo: Arc::clone(&index.tempo),
            font: Arc::clone(&font),
            sample_rate: 48000,
        };
        let (voice, budget) = voice(resources, 480, 2880);
        let life = life();
        let plugin = Arc::clone(&life.plugin);
        let epoch = Arc::clone(&life.epoch);
        let alive = Arc::clone(&life.alive);
        let state = Arc::clone(&life.status);
        let e = engine(Arc::clone(&index), Arc::clone(&font));
        e.send(Command::PluginMidi {
            voice: Box::new(voice),
            life,
        });
        wait_status(&state, Status::Running);
        e.pump(30000, &mut (0.0, 0.0)); // plugin key is held
        match reason {
            0 => plugin.store(false, Ordering::Release),
            1 => {
                epoch.fetch_add(1, Ordering::AcqRel);
            }
            2 => {
                e.send(Command::Pause);
            }
            3 => {
                e.send(Command::SetLoop(Some((0, 1920))));
            }
            _ => alive.store(false, Ordering::Release),
        }
        // A full render ring must not postpone lifecycle cleanup until another block.
        wait_status(&state, Status::Canceled);
        assert!(!alive.load(Ordering::Acquire));
        let end = Instant::now() + Duration::from_secs(2);
        while budget.used() != 0 {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
#[test]
fn owned_voice_loop_wrap_and_clock_discontinuity_cancel_without_allocating_output_notes() {
    let doc = document();
    let index = PlaybackIndex::build(&doc, 48000);
    let font = font();
    for wrapped in [false, true] {
        let resources = Resources {
            tempo: Arc::clone(&index.tempo),
            font: Arc::clone(&font),
            sample_rate: 48000,
        };
        let (pending, budget) = voice(resources, 480, 960);
        let life = life();
        let state = Arc::clone(&life.status);
        let mut voice = pending.start(0, 48000 * 4, life).unwrap();
        let mut block = [0.0; BLOCK * 2];
        assert!(!voice.mix(if wrapped { 0 } else { 64 }, wrapped, &mut block));
        wait_status(&state, Status::Canceled);
        drop(voice);
        assert_eq!(budget.used(), 0);
        assert_eq!(doc.note_count(), 1);
    }
}
#[test]
fn canceled_queued_command_releases_accounting_even_when_never_started() {
    assert!(
        size_of::<Command>() <= 128,
        "plugin output must preserve small native command messages"
    );
    let doc = document();
    let index = PlaybackIndex::build(&doc, 48000);
    let font = font();
    let (voice, budget) = voice(
        Resources {
            tempo: Arc::clone(&index.tempo),
            font,
            sample_rate: 48000,
        },
        480,
        960,
    );
    let life = life();
    let state = Arc::clone(&life.status);
    let alive = Arc::clone(&life.alive);
    let queued = Command::PluginMidi {
        voice: Box::new(voice),
        life,
    };
    drop(queued);
    assert_eq!(budget.used(), 0);
    assert_eq!(
        Status::decode(state.load(Ordering::Acquire)),
        Status::Canceled
    );
    assert!(!alive.load(Ordering::Acquire));
}

#[test]
fn externally_revoked_alive_flag_finalizes_status_even_without_another_render_block() {
    let doc = document();
    let index = PlaybackIndex::build(&doc, 48000);
    for started in [false, true] {
        let (prepared, budget) = voice(
            Resources {
                tempo: Arc::clone(&index.tempo),
                font: font(),
                sample_rate: 48000,
            },
            480,
            960,
        );
        let life = life();
        let alive = Arc::clone(&life.alive);
        let state = Arc::clone(&life.status);
        if started {
            let voice = prepared.start(0, 48000 * 4, life).unwrap();
            alive.store(false, Ordering::Release);
            drop(voice);
        } else {
            let queued = Command::PluginMidi {
                voice: Box::new(prepared),
                life,
            };
            alive.store(false, Ordering::Release);
            drop(queued);
        }
        assert_eq!(
            Status::decode(state.load(Ordering::Acquire)),
            Status::Canceled
        );
        assert_eq!(budget.used(), 0);
    }
}
