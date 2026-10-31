//! S66 measurements on the existing scheduler, bank, worker, ring and callback.
//! No device is opened. Paced callback results exclude driver/device behaviour.
//! Run the ignored measurement in release, one test thread, one voice setting
//! per process. S66_JSON lines are the raw, machine-readable evidence.

use super::engine::{self, AudioEngine, BLOCK, Command, RenderConfig};
use super::profile::{bursts, test_soundfont, wall};
use super::synth::SynthConfig;
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::playback::PlaybackIndex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
const VOICES: [usize; 6] = [256, 512, 1024, 2048, 3000, 4096];

fn cpu() -> Option<u64> {
    crate::platform::process_cpu_nanos()
}

fn cpu_delta(before: Option<u64>) -> Option<u64> {
    before.zip(cpu()).map(|(a, b)| b.saturating_sub(a))
}

fn emit(value: Value) {
    println!("S66_JSON {value}");
}

fn fixture(name: &str) -> PlumDocument {
    match name {
        "dense" => plumspirit_engine::fixtures::generate(
            plumspirit_engine::fixtures::FixtureShape::Dense,
            1_000_000,
            960,
            42,
        ),
        "wall" => wall(2000),
        "burst4096" => bursts(4096, 20, 240),
        _ => panic!("unknown S66 fixture: {name}"),
    }
}

fn offline(font: &Arc<rustysynth::SoundFont>, index: Arc<PlaybackIndex>, cfg: &SynthConfig) {
    let before = cpu();
    let started = Instant::now();
    let r = engine::render_offline_bank(font, index, 0, RATE, 480_000, cfg, None);
    let elapsed_ns = started.elapsed().as_nanos() as u64;
    let cpu_ns = cpu_delta(before);
    assert!(r.samples.iter().all(|s| s.is_finite()));
    assert_eq!(r.samples.len(), 960_000);
    assert_eq!(
        r.stats.note_ons_skipped, 0,
        "event loss must be investigated"
    );
    assert_eq!(r.clipped, 0, "default limiter must prevent hard clipping");
    emit(json!({
        "mode": "offline", "audio_frames": 480_000,
        "elapsed_ns": elapsed_ns, "process_cpu_ns": cpu_ns,
        "worst_block_ns": r.worst_block_nanos,
        "rss_sampled_max_bytes": r.sampled_rss_bytes,
        "capacity": r.capacity, "worker_threads": r.render_threads, "note_ons": r.stats.note_ons,
        "note_offs": r.stats.note_offs, "pending_offs": r.pending_offs,
        "peak_pending_offs": r.stats.peak_pending_offs,
        "deferred": r.stats.note_ons_deferred, "skipped": r.stats.note_ons_skipped,
        "late_dropped": r.stats.late_dropped, "max_lateness_frames": r.stats.max_lateness,
        "shard_overflow_blocks": r.overflow_blocks,
        "peak_pre": r.peak_pre, "peak_post": r.peak_post,
        "limited_frames": r.limited, "clipped_samples": r.clipped,
        "underruns": null, "callback_ns": null,
    }));
}

fn paced(
    font: &Arc<rustysynth::SoundFont>,
    index: Arc<PlaybackIndex>,
    cfg: &SynthConfig,
    prerender: bool,
) {
    let setup_cpu = cpu();
    let setup = Instant::now();
    let render = RenderConfig::new(prerender, 1000, RATE);
    let e = AudioEngine::headless_render(RATE, *cfg, render);
    assert!(e.send(Command::SetSoundFont(Arc::clone(font))));
    assert!(e.send(Command::Play {
        index,
        tick: 0,
        generation: 1,
        looping: None,
        restrike: true
    }));
    e.wait_playing(true);
    let sh = &e.shared;
    assert!(sh.playing.load(Ordering::Acquire), "worker did not start");
    let target = if prerender {
        render.ahead_frames
    } else {
        engine::RING_FRAMES
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while sh.ring.available() / 2 < target || !sh.prerolled.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "worker failed to fill the initial buffer"
        );
        assert!(e.worker_alive(), "worker exited while filling");
        std::thread::sleep(Duration::from_millis(1));
    }
    let setup_ns = setup.elapsed().as_nanos() as u64;
    let setup_cpu_ns = cpu_delta(setup_cpu);
    engine::take_peak(&sh.peak_pre);
    engine::take_peak(&sh.peak_post);
    let mut out = [0f32; 1024];
    let mut last = (0.0, 0.0);
    // Include voice/release-tail build-up before drawing sustained-load conclusions.
    let callbacks = (RATE as usize * 6).div_ceil(512);
    let frames = callbacks * 512;
    let (mut callback_ns, mut callback_max_ns, mut wake_late_max_ns) = (0u64, 0u64, 0u64);
    let (mut ahead_min, mut ahead_max) = (usize::MAX, 0usize);
    let (mut ema_sum, mut ema_max) = (0u128, 0u64);
    let mut peak = 0f32;
    let mut rss = crate::diagnostics::process_memory_bytes();
    let before = cpu();
    let started = Instant::now();
    for i in 0..callbacks {
        let due = started + Duration::from_secs_f64((i * 512) as f64 / f64::from(RATE));
        if let Some(left) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(left);
        }
        wake_late_max_ns =
            wake_late_max_ns.max(Instant::now().saturating_duration_since(due).as_nanos() as u64);
        let ahead = sh.ring.available() / 2;
        ahead_min = ahead_min.min(ahead);
        ahead_max = ahead_max.max(ahead);
        let t = Instant::now();
        e.profile_callback(&mut out, &mut last);
        let ns = t.elapsed().as_nanos() as u64;
        callback_ns += ns;
        callback_max_ns = callback_max_ns.max(ns);
        for s in out {
            assert!(s.is_finite());
            peak = peak.max(s.abs());
        }
        let ema = sh.block_nanos.load(Ordering::Relaxed);
        ema_sum += u128::from(ema);
        ema_max = ema_max.max(ema);
        if i % 16 == 0
            && let Some(now) = crate::diagnostics::process_memory_bytes()
        {
            rss = Some(rss.unwrap_or(0).max(now));
        }
    }
    let end = started + Duration::from_secs_f64(frames as f64 / f64::from(RATE));
    if let Some(left) = end.checked_duration_since(Instant::now()) {
        std::thread::sleep(left);
    }
    let elapsed_ns = started.elapsed().as_nanos() as u64;
    let cpu_ns = cpu_delta(before);
    assert_eq!(sh.skipped_notes.load(Ordering::Relaxed), 0);
    assert_eq!(sh.clipped.load(Ordering::Relaxed), 0);
    emit(json!({
        "mode": if prerender { "prerender_paced" } else { "realtime_paced" },
        "window": "from_transport_start_after_initial_fill",
        "audio_frames": frames, "elapsed_ns": elapsed_ns, "process_cpu_ns": cpu_ns,
        "setup_fill_ns": setup_ns, "setup_process_cpu_ns": setup_cpu_ns,
        "configured_ahead_frames": if prerender { render.ahead_frames } else { engine::RING_FRAMES },
        "callback_frames": 512, "callback_count": callbacks,
        "callback_ns": callback_ns, "callback_max_ns": callback_max_ns,
        "wake_late_max_ns": wake_late_max_ns,
        "worker_block_ema_mean_ns": (ema_sum / callbacks as u128) as u64,
        "worker_block_ema_max_ns": ema_max,
        "worker_threads": sh.synth_render_threads.load(Ordering::Relaxed),
        "ahead_min_frames": ahead_min, "ahead_max_frames": ahead_max,
        "ring_capacity_frames": sh.ring.capacity() / 2,
        "rss_sampled_max_bytes": rss,
        "underruns": sh.underruns.load(Ordering::Relaxed),
        "capacity": sh.voice_capacity.load(Ordering::Relaxed),
        "pending_offs": sh.active_voices.load(Ordering::Relaxed),
        "deferred": sh.deferred_notes.load(Ordering::Relaxed),
        "skipped": sh.skipped_notes.load(Ordering::Relaxed),
        "late_dropped": sh.late_dropped.load(Ordering::Relaxed),
        "max_lateness_frames": sh.max_lateness.load(Ordering::Relaxed),
        "shard_overflow_blocks": sh.overflow_blocks.load(Ordering::Relaxed),
        "peak_pre": engine::take_peak(&sh.peak_pre),
        "peak_post": engine::take_peak(&sh.peak_post), "callback_peak": peak,
        "limited_frames": sh.limited.load(Ordering::Relaxed),
        "clipped_samples": sh.clipped.load(Ordering::Relaxed),
    }));
}

#[test]
#[ignore = "S66 measurement: release, explicit SoundFont, one test thread"]
fn audio_s66_profile() -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err("profile the optimized release build".into());
    }
    let font = test_soundfont().expect("S66 requires a real SoundFont; set PLUMSPIRIT_TEST_SF2");
    let voices = std::env::var("PLUMSPIRIT_AUDIO_VOICES").map_or_else(
        |_| VOICES.to_vec(),
        |v| {
            v.split(',')
                .map(|s| s.parse::<usize>().expect("voice count"))
                .collect()
        },
    );
    assert!(!voices.is_empty() && voices.iter().all(|n| VOICES.contains(n)));
    let case = std::env::var("PLUMSPIRIT_AUDIO_CASE").unwrap_or("wall".into());
    let mode = std::env::var("PLUMSPIRIT_AUDIO_MODE").unwrap_or("offline".into());
    assert!(["offline", "paced"].contains(&mode.as_str()));
    let doc = fixture(&case);
    let index = Arc::new(PlaybackIndex::build(&doc, RATE));
    for voices in voices {
        for effects in [true, false] {
            let cfg = SynthConfig {
                max_voices: voices,
                effects,
                ..SynthConfig::default()
            };
            let (shards, per) = cfg.shards();
            emit(json!({ "config": true, "voices": voices, "shards": shards,
                "voices_per_shard": per, "effects": effects, "fixture": case,
                "sample_rate": RATE, "synth_block_frames": BLOCK,
                "presets": font.get_presets().len(), "gain_db": cfg.gain_db,
                "limiter": cfg.limiter, "note_ons_per_block": cfg.note_ons_per_block }));
            match mode.as_str() {
                "offline" => offline(&font, Arc::clone(&index), &cfg),
                "paced" => {
                    paced(&font, Arc::clone(&index), &cfg, false);
                    paced(&font, Arc::clone(&index), &cfg, true);
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

#[test]
fn profiling_callback_counts_starvation_without_waiting_for_the_worker() {
    let e = AudioEngine::headless(RATE);
    e.shared.playing.store(true, Ordering::Release);
    let mut out = [1f32; 1024];
    e.profile_callback(&mut out, &mut (0.0, 0.0));
    assert_eq!(e.shared.underruns.load(Ordering::Relaxed), 1);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn offline_profile_counters_preserve_the_rendered_samples() {
    let font = Arc::new(
        rustysynth::SoundFont::new(&mut &super::soundfont::tests::tiny_sf2()[..]).unwrap(),
    );
    let index = Arc::new(PlaybackIndex::build(&bursts(256, 2, 240), RATE));
    let cfg = SynthConfig::default();
    let a = engine::render_offline_bank(&font, Arc::clone(&index), 0, RATE, 48_000, &cfg, None);
    let b = engine::render_offline_bank(&font, index, 0, RATE, 48_000, &cfg, None);
    assert_eq!(a.samples, b.samples);
    assert_eq!(a.stats.note_ons, 512);
    assert_eq!(a.stats.note_offs, 512);
    assert_eq!(a.pending_offs, 0);
    assert!(a.worst_block_nanos > 0);
}
