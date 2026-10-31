//! v0.8 Wave M audio profiling harness (contract §130, §229, §230): renders
//! dense material through the real worker path (Scheduler + synthesizer, the
//! same block size and master level as the worker) without a device and
//! reports event loss and signal level separately.
//!
//! `cargo test --release -p plumspirit audio_profile -- --ignored --nocapture`
//! (SoundFont: `PLUMSPIRIT_TEST_SF2`, else GeneralUser GS in
//! `target/debug/SoundFonts`, else `/usr/share/sounds/sf2/TimGM6mb.sf2`).

use plumspirit_engine::model::{
    Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use std::path::PathBuf;

/// The test SoundFont, if one is available.
pub fn test_soundfont() -> Option<std::sync::Arc<rustysynth::SoundFont>> {
    test_soundfont_with_path().map(|(_, font)| font)
}

/// The test SoundFont and the file it came from.
pub fn test_soundfont_with_path() -> Option<(PathBuf, std::sync::Arc<rustysynth::SoundFont>)> {
    let candidates = [
        std::env::var_os("PLUMSPIRIT_TEST_SF2").map(PathBuf::from),
        Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug/SoundFonts/GeneralUser-GS.sf2"),
        ),
        Some(PathBuf::from("/usr/share/sounds/sf2/TimGM6mb.sf2")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(|p| super::soundfont::load(&p).ok().map(|l| (p, l.font)))
}

/// `count` notes starting together every quarter note for `beats` beats,
/// spread over 16 channels and all 128 keys (duplicates beyond 2,048).
pub fn bursts(count: usize, beats: u64, len_ticks: u64) -> PlumDocument {
    let mut d = PlumDocument::new_project("burst".into(), Bpm::DEFAULT, 480);
    let mut recs = Vec::with_capacity(count * beats as usize);
    for b in 0..beats {
        for i in 0..count {
            recs.push(NoteRecord {
                start_tick: b * 480,
                duration_ticks: len_ticks,
                attrs: NoteAttrs::new(
                    (i % 128) as u8,
                    100,
                    ((i / 128) % 16) as u8,
                    0,
                    NoteOffKind::NoteOff,
                )
                .unwrap(),
                sub: SubchannelId::MAIN,
                group: 0,
            });
        }
    }
    recs.sort_by_key(|r| r.start_tick);
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

/// 128-note chords on 16 channels every 1/32 (v0.6's "wall").
pub fn wall(steps: u64) -> PlumDocument {
    let mut d = PlumDocument::new_project("wall".into(), Bpm::DEFAULT, 480);
    let mut recs = Vec::new();
    for step in 0..steps {
        for k in 0..128u64 {
            recs.push(NoteRecord {
                start_tick: step * 60,
                duration_ticks: 240,
                attrs: NoteAttrs::new(k as u8, 100, (k % 16) as u8, 0, NoteOffKind::NoteOff)
                    .unwrap(),
                sub: SubchannelId::MAIN,
                group: 0,
            });
        }
    }
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::engine::{BLOCK, synth_settings};
    use plumspirit_engine::playback::{PlaybackIndex, Scheduler, SchedulerLimits, SynthSink};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct S<'a>(&'a mut rustysynth::Synthesizer);
    impl SynthSink for S<'_> {
        fn note_on(&mut self, c: u8, k: u8, v: u8) {
            self.0.note_on(c.into(), k.into(), v.into());
        }
        fn note_off(&mut self, c: u8, k: u8) {
            self.0.note_off(c.into(), k.into());
        }
        fn channel_message(&mut self, st: u8, a: u8, b: u8) {
            self.0
                .process_midi_message((st & 15).into(), (st & 0xF0).into(), a.into(), b.into());
        }
        fn silence(&mut self) {
            self.0.note_off_all(true);
        }
        fn reset(&mut self) {
            self.0.reset();
            self.0.set_master_volume(0.5);
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn audio_profile() {
        let Some(font) = test_soundfont() else {
            println!("skipped: no SoundFont");
            return;
        };
        let sr = 48_000u32;
        let cases: Vec<(String, PlumDocument, f64)> = vec![
            (
                "Dense fixture 1M notes".into(),
                plumspirit_engine::fixtures::generate(
                    plumspirit_engine::fixtures::FixtureShape::Dense,
                    1_000_000,
                    960,
                    42,
                ),
                10.0,
            ),
            ("Wall 128-note chords / 1/32".into(), wall(2_000), 10.0),
            ("Burst 256 every beat".into(), bursts(256, 20, 240), 10.0),
            (
                "Burst 1,024 every beat".into(),
                bursts(1_024, 20, 240),
                10.0,
            ),
            (
                "Burst 4,096 every beat".into(),
                bursts(4_096, 20, 240),
                10.0,
            ),
            (
                "Burst 16,384 every beat".into(),
                bursts(16_384, 20, 240),
                10.0,
            ),
        ];
        println!("SoundFont: {} presets", font.get_presets().len());
        for limits in [SchedulerLimits::V07, SchedulerLimits::default()] {
            println!(
                "\n== limits: {} note-ons / block, {} pending offs, lateness {} frames; 256 voices; master 0.5",
                limits.max_note_ons_per_block, limits.max_pending_offs, limits.max_lateness_frames
            );
            for (name, doc, secs) in &cases {
                let idx = Arc::new(PlaybackIndex::build(doc, sr));
                let mut synth = rustysynth::Synthesizer::new(&font, &synth_settings(sr)).unwrap();
                synth.set_master_volume(0.5);
                let mut sched = Scheduler::new(idx, limits);
                sched.seek(0, &mut S(&mut synth));
                let (mut l, mut r) = ([0f32; BLOCK], [0f32; BLOCK]);
                let blocks = (f64::from(sr) * secs / BLOCK as f64) as usize;
                let mut worst = Duration::ZERO;
                let (mut peak, mut clipped, mut samples) = (0f32, 0u64, 0u64);
                let started = Instant::now();
                for _ in 0..blocks {
                    let t = Instant::now();
                    sched.advance(BLOCK as u32, &mut S(&mut synth));
                    synth.render(&mut l, &mut r);
                    worst = worst.max(t.elapsed());
                    for s in l.iter().chain(r.iter()) {
                        let a = s.abs();
                        peak = peak.max(a);
                        clipped += u64::from(a > 1.0);
                        samples += 1;
                    }
                }
                let total = started.elapsed();
                let st = sched.stats();
                println!(
                    "{name}: {:.1}% of real time; worst block {:.0} µs (block {:.0} µs); note-ons {} scheduled ({} deferred, max {} frames late), {} skipped; peak sounding notes {}; peak |sample| {:.2} ({:+.1} dBFS); clipped samples {} ({:.3}%)",
                    total.as_secs_f64() / secs * 100.0,
                    worst.as_secs_f64() * 1e6,
                    BLOCK as f64 / f64::from(sr) * 1e6,
                    st.note_ons,
                    st.note_ons_deferred,
                    st.max_lateness,
                    st.note_ons_skipped,
                    st.peak_pending_offs,
                    peak,
                    20.0 * f64::from(peak.max(1e-9)).log10(),
                    clipped,
                    clipped as f64 / samples as f64 * 100.0,
                );
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn audio_profile_v08() {
        use crate::audio::engine::render_offline_bank;
        use crate::audio::synth::SynthConfig;
        let Some(font) = test_soundfont() else {
            println!("skipped: no SoundFont");
            return;
        };
        let cases: Vec<(&str, PlumDocument)> = vec![
            (
                "Dense fixture 1M",
                plumspirit_engine::fixtures::generate(
                    plumspirit_engine::fixtures::FixtureShape::Dense,
                    1_000_000,
                    960,
                    42,
                ),
            ),
            ("Wall", wall(2_000)),
            ("Burst 1,024", bursts(1_024, 20, 240)),
            ("Burst 4,096", bursts(4_096, 20, 240)),
            ("Burst 16,384", bursts(16_384, 20, 240)),
        ];
        for voices in [512usize, 1_024, 2_048] {
            for effects in [true, false] {
                let cfg = SynthConfig {
                    max_voices: voices,
                    effects,
                    ..SynthConfig::default()
                };
                println!(
                    "\n== v0.8: {voices} voices ({} shards), effects {effects}, gain {} dB, limiter {}",
                    cfg.shards().0,
                    cfg.gain_db,
                    cfg.limiter
                );
                for (name, doc) in &cases {
                    let idx = Arc::new(PlaybackIndex::build(doc, 48_000));
                    let t = Instant::now();
                    let r = render_offline_bank(&font, idx, 0, 48_000, 480_000, &cfg, None);
                    let st = r.stats;
                    println!(
                        "{name}: {:.1}% of real time; {} scheduled ({} deferred, max {:.1} ms late), {} skipped; voice-limited blocks {}; peak {:+.1} dBFS → {:+.1} dBFS; limited {:.1}% of frames; clipped {}",
                        t.elapsed().as_secs_f64() / 10.0 * 100.0,
                        st.note_ons,
                        st.note_ons_deferred,
                        st.max_lateness as f64 / 48.0,
                        st.note_ons_skipped,
                        r.overflow_blocks,
                        20.0 * f64::from(r.peak_pre.max(1e-9)).log10(),
                        20.0 * f64::from(r.peak_post.max(1e-9)).log10(),
                        r.limited as f64 / 480_000.0 * 100.0,
                        r.clipped,
                    );
                }
            }
        }
    }

    fn tiny() -> Arc<rustysynth::SoundFont> {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        Arc::new(rustysynth::SoundFont::new(&mut &bytes[..]).unwrap())
    }

    /// §229: bursts of 256 / 1,024 / 4,096 / 16,384 simultaneous starts with
    /// the default settings: every note is scheduled (bursts beyond the
    /// per-block budget are spread, not dropped), voice demand beyond the
    /// voices is reported, nothing is silently lost.
    #[test]
    fn audio_event_density_bursts_lose_nothing_silently() {
        use crate::audio::engine::render_offline_bank;
        use crate::audio::synth::SynthConfig;
        let font = tiny();
        let cfg = SynthConfig::default();
        for count in [256usize, 1_024, 4_096, 16_384] {
            let doc = bursts(count, 2, 240);
            let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
            let started = Instant::now();
            let r = render_offline_bank(&font, idx, 0, 48_000, 48_000, &cfg, None);
            let st = r.stats;
            assert_eq!(st.note_ons as usize, count * 2, "{count}: all scheduled");
            assert_eq!(st.note_ons_skipped, 0, "{count}");
            if count > cfg.note_ons_per_block {
                assert!(
                    st.note_ons_deferred > 0,
                    "{count}: spread over blocks (reported)"
                );
                assert!(st.max_lateness <= 2_400);
            }
            if count > r.capacity {
                assert!(
                    r.overflow_blocks > 0,
                    "{count}: voice demand above {} reported",
                    r.capacity
                );
            }
            println!(
                "burst {count}: {} scheduled, {} deferred (max {} frames late), {} skipped, voice-limited blocks {}, {:?}",
                st.note_ons,
                st.note_ons_deferred,
                st.max_lateness,
                st.note_ons_skipped,
                r.overflow_blocks,
                started.elapsed()
            );
        }
    }

    /// v1.0-beta2 (B2-S33, contract §012): Pre-Render is event-preserving —
    /// a 4,096-note burst starts in its own block, nothing deferred or skipped;
    /// real time spreads it over blocks of at most the budget (counted).
    #[test]
    fn pre_render_starts_every_note_in_its_own_block() {
        use crate::audio::engine::limits_for;
        use crate::audio::synth::SynthConfig;
        #[derive(Default)]
        struct Count(u64);
        impl SynthSink for Count {
            fn note_on(&mut self, _: u8, _: u8, _: u8) {
                self.0 += 1;
            }
            fn note_off(&mut self, _: u8, _: u8) {}
            fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
            fn silence(&mut self) {}
            fn reset(&mut self) {}
        }
        let cfg = SynthConfig::default();
        let idx = Arc::new(PlaybackIndex::build(&bursts(4_096, 2, 240), 48_000));
        for prerender in [false, true] {
            let limits = limits_for(&cfg, 48_000, prerender);
            let mut sched = Scheduler::new(Arc::clone(&idx), limits);
            let mut sink = Count::default();
            sched.seek(0, &mut sink);
            let mut per_block = Vec::new();
            for _ in 0..1_500 {
                let before = sink.0;
                sched.advance(BLOCK as u32, &mut sink);
                per_block.push(sink.0 - before);
            }
            let st = sched.stats();
            assert_eq!(st.note_ons, 8_192, "prerender {prerender}: every note");
            assert_eq!(st.note_ons_skipped, 0);
            if prerender {
                assert_eq!(st.note_ons_deferred, 0);
                assert_eq!(st.max_lateness, 0);
                assert_eq!(per_block[0], 4_096, "the whole burst in its own block");
            } else {
                assert!(st.note_ons_deferred > 0);
                assert!(
                    per_block
                        .iter()
                        .all(|&n| n <= cfg.note_ons_per_block as u64)
                );
            }
        }
    }

    /// §230: dense chords — the peak before output conversion, headroom and
    /// limiter: no hard clipping at the defaults, and with the limiter off
    /// overload is counted, never hidden.
    #[test]
    fn audio_signal_dense_chords_never_hard_clip_at_defaults() {
        use crate::audio::engine::render_offline_bank;
        use crate::audio::synth::{LIMITER_THRESHOLD, SynthConfig};
        let font = tiny();
        let doc = wall(200);
        let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
        let def = render_offline_bank(
            &font,
            Arc::clone(&idx),
            0,
            48_000,
            96_000,
            &SynthConfig::default(),
            None,
        );
        assert_eq!(def.clipped, 0);
        assert!(def.peak_post <= LIMITER_THRESHOLD + 1e-6);
        assert!(def.samples.iter().all(|s| s.abs() <= 1.0));
        // +12 dB drives it hard: the limiter holds; the pre meter shows it.
        let hot = SynthConfig {
            gain_db: 12.0,
            ..SynthConfig::default()
        };
        let lim = render_offline_bank(&font, Arc::clone(&idx), 0, 48_000, 96_000, &hot, None);
        assert!(
            lim.peak_pre > 1.0,
            "pre-limiter overload visible: {}",
            lim.peak_pre
        );
        assert!(lim.limited > 0);
        assert_eq!(lim.clipped, 0);
        let raw = render_offline_bank(
            &font,
            idx,
            0,
            48_000,
            96_000,
            &SynthConfig {
                limiter: false,
                ..hot
            },
            None,
        );
        assert!(raw.clipped > 0, "without the limiter, clipping is counted");
        println!(
            "dense chords: defaults peak {:.2} → {:.2}; +12 dB peak {:.2} → {:.2} (limited {} frames); no limiter clipped {} samples",
            def.peak_pre, def.peak_post, lim.peak_pre, lim.peak_post, lim.limited, raw.clipped
        );
    }

    /// Shards: 1,024 voices sound more notes than 256 do — same events, the
    /// notes are spread over four synthesizers — and renders are
    /// deterministic (same input → same samples).
    #[test]
    fn shards_add_voices_and_render_deterministically() {
        use crate::audio::engine::render_offline_bank;
        use crate::audio::synth::SynthConfig;
        let font = tiny();
        let doc = bursts(1_024, 2, 480);
        let idx = Arc::new(PlaybackIndex::build(&doc, 48_000));
        let one = SynthConfig {
            max_voices: 256,
            ..SynthConfig::default()
        };
        let four = SynthConfig {
            max_voices: 1_024,
            limiter: false,
            gain_db: -24.0,
            ..SynthConfig::default()
        };
        let a = render_offline_bank(&font, Arc::clone(&idx), 0, 48_000, 24_000, &one, None);
        let b = render_offline_bank(&font, Arc::clone(&idx), 0, 48_000, 24_000, &four, None);
        assert_eq!(a.capacity, 256);
        assert_eq!(b.capacity, 1_024);
        assert!(a.overflow_blocks > 0);
        assert_eq!(
            b.overflow_blocks, 0,
            "1,024 notes fit 1,024 voices (spread over the keys)"
        );
        let b2 = render_offline_bank(&font, idx, 0, 48_000, 24_000, &four, None);
        assert_eq!(b.samples, b2.samples, "deterministic");
    }
}
