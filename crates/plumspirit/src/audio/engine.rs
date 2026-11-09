//! The running audio pipeline: synthesis worker thread + output stream.
//!
//! ```text
//!  UI thread ──commands (bounded channel)──► synth worker ──samples──► Ring ──► cpal callback ──► device
//!      ▲                                     (Scheduler +                         │
//!      └──────────── position / status (atomics, seqlock) ◄───────────────────────┘
//! ```
//!
//! Ownership: the UI thread owns the transport state and this struct (and with it
//! the output stream). The worker thread owns the synthesizer and the scheduler.
//! The callback owns nothing but a handle to the lock-free [`Ring`] and a few
//! atomics: it never locks, allocates, logs, touches files or the document.

use super::midi_out::{Dispatcher, ExternalRoute, ExternalSink};
use super::ring::Ring;
use super::synth::{MasterBus, SYNTH_MASTER, SynthBank, SynthConfig};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use plumspirit_engine::model::conductor::MASTER_VOLUME_MAX;
use plumspirit_engine::playback::{
    ChannelState, PlaybackIndex, Scheduler, SchedulerLimits, SynthSink,
};
#[cfg(test)]
use rustysynth::SynthesizerSettings;
use rustysynth::{SoundFont, Synthesizer};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// An error's text without its own final period (it ends a sentence of ours).
pub fn sentence(e: &impl std::fmt::Display) -> String {
    e.to_string().trim_end().trim_end_matches('.').to_owned()
}

/// Frames rendered per scheduling block (event timing resolution).
pub const BLOCK: usize = 64;
/// Audio kept buffered ahead of the device while playing, in stereo frames
/// (≈ 85 ms at 48 kHz) — or two device callbacks, if the device asks for more.
pub const RING_FRAMES: usize = 4096;
/// Ring capacity in stereo frames (≈ 0.68 s at 48 kHz): room for devices with
/// very large callbacks; only [`RING_FRAMES`] (or two callbacks) is kept filled.
pub const RING_CAPACITY_FRAMES: usize = 32_768;
/// Voices of one RustySynth synthesizer (its verified maximum; more voices
/// run several, see `synth.rs`).
#[cfg(test)]
pub const MAX_VOICES: usize = super::synth::VOICES_PER_SHARD;
/// Audio kept rendering after the last event so releases and reverb can fade.
const TAIL_SECONDS_DIV: u32 = 2;
/// While only auditioning, the ring is kept at least this full (≈ 21 ms at
/// 48 kHz) so a new audition note is heard quickly — and at least two device
/// callbacks full, or a device that asks for larger buffers would get gaps.
const AUDITION_RING_FRAMES: usize = 1024;

/// The note-audition synthesizer: its own voices, so an audition Note Off can
/// never end a note the transport is playing (and the reverse).
struct Audition {
    synth: Synthesizer,
    /// Channel state last applied to each channel of `synth`.
    applied: [ChannelState; 16],
    /// Sounding audition notes: (channel, key, frames left until auto-off).
    notes: Vec<(u8, u8, Option<u32>)>,
    /// Frames still rendered after the last note so its release can fade.
    tail: u32,
}

impl Audition {
    fn new(font: &Arc<SoundFont>, sample_rate: u32, cfg: &SynthConfig) -> Option<Audition> {
        let st = super::synth::shard_settings(sample_rate, cfg.audition_voices, cfg.effects);
        Synthesizer::new(font, &st).ok().map(|synth| Audition {
            synth,
            applied: [ChannelState::default(); 16],
            notes: Vec::new(),
            tail: 0,
        })
    }

    fn busy(&self) -> bool {
        !self.notes.is_empty() || self.tail > 0
    }

    /// Advances auto-off timers and the release tail by `frames`.
    fn advance(&mut self, frames: u32, tail_frames: u32) {
        let synth = &mut self.synth;
        self.notes.retain_mut(|(ch, key, left)| match left {
            Some(l) if *l <= frames => {
                synth.note_off(i32::from(*ch), i32::from(*key));
                false
            }
            Some(l) => {
                *l -= frames;
                true
            }
            None => true,
        });
        if self.notes.is_empty() {
            self.tail = self.tail.saturating_sub(frames);
        } else {
            self.tail = tail_frames;
        }
    }
}

/// Commands from the UI thread to the synthesis worker.
pub enum Command {
    /// Worker-prepared isolated native voices; never a Lua callback or shared route.
    PluginMidi {
        voice: Box<super::midi_overlay::PreparedVoice>,
        life: super::midi_overlay::Life,
    },
    /// Replace the synthesizer's SoundFont at a block boundary.
    SetSoundFont(Arc<SoundFont>),
    /// Start (or restart) playback at `tick` with this index, looping `[start,
    /// end)` when `looping` is set.
    Play {
        index: Arc<PlaybackIndex>,
        tick: u64,
        generation: u64,
        looping: Option<(u64, u64)>,
        /// Strike the notes held at `tick` (a seek); `false` for a resume
        /// after a pause without re-strike (v1.0-beta1 B1-0083).
        restrike: bool,
    },
    /// Change the loop while playing (`None`: play on to the end).
    SetLoop(Option<(u64, u64)>),
    /// Mute / Solo changed while playing: which tracks sound.
    SetEligible(Arc<[bool]>),
    /// The external MIDI outputs of routes 1, 2, … (route 0 is the synthesizer).
    SetRoutes(Vec<Arc<ExternalRoute>>),
    /// The document changed while playing: continue from the current position with
    /// the new index.
    Update {
        index: Arc<PlaybackIndex>,
        generation: u64,
    },
    /// Stop rendering and silence immediately.
    Pause,
    /// Drop the old project's scheduler on this worker, retaining the device
    /// and SoundFont. A delayed request must never clear a newer playback.
    ReleaseProject {
        before_generation: u64,
        done: std::sync::mpsc::Sender<()>,
    },
    /// The reference audio to mix in while playing (v0.8 Audio Reference;
    /// `None`: none).
    SetReference(Option<Arc<super::reference::RefPlay>>),
    /// Note audition (v0.6) on the separate audition synthesizer: `state` is the
    /// channel's program / bank / controllers at the edit position (applied
    /// first); `hold`: frames until it stops by itself (`None`: until
    /// [`Command::AuditionOff`]).
    AuditionOn {
        channel: u8,
        key: u8,
        velocity: u8,
        state: Box<ChannelState>,
        hold: Option<u32>,
    },
    AuditionOff {
        channel: u8,
        key: u8,
    },
    /// Releases every audition note (focus lost, dialog, project closed).
    AuditionAllOff,
    /// Synthesis settings (v0.8 Audio Settings): voices, effects, gain,
    /// limiter, note-on budget. Voices / effects rebuild the synthesizers at
    /// a block boundary (sounding notes are re-struck).
    Configure(SynthConfig),
    /// Realtime or Pre-Render (from the next start position).
    SetRender(RenderConfig),
    Shutdown,
}

/// State shared between the UI, the worker and the callback. All lock-free,
/// except `worker_error`, which only the worker and the UI thread touch.
pub struct Shared {
    pub ring: Ring,
    /// Audition samples while Pre-Render playback fills `ring` far ahead
    /// (mixed by the callback, so auditions stay immediate).
    pub aud_ring: Ring,
    /// Pre-Render (v0.8 §137): frames that must be buffered before the
    /// callback starts consuming a new start position (0: none) …
    pub preroll: AtomicU64,
    /// … and whether that is done.
    pub prerolled: AtomicBool,
    /// Segment seqlock: audio written from ring position `w0` on starts at frame
    /// `f0` of playback generation `gen`.
    seg_seq: AtomicU64,
    seg_gen: AtomicU64,
    seg_w0: AtomicU64,
    seg_f0: AtomicU64,
    /// Last position heard: `(generation & 0xFFFF) << 48 | frame`.
    position: AtomicU64,
    pub underruns: AtomicU64,
    /// The worker is rendering audition notes while the transport is stopped.
    pub auditioning: AtomicBool,
    pub playing: AtomicBool,
    /// Set by the stream's error callback when the device failed.
    pub device_failed: AtomicBool,
    pub device_error: Mutex<Option<String>>,
    /// Generation whose playback reached the end of the song (`u64::MAX`: none).
    pub ended_generation: AtomicU64,
    pub active_voices: AtomicU32,
    pub skipped_notes: AtomicU64,
    /// v0.8 diagnostics (§145): note-ons sent late (burst spread), dropped as
    /// too late, the latest lateness (frames); voice demand / capacity and
    /// blocks where a shard's demand exceeded its voices; output peaks before
    /// / after the limiter (f32 bits, max since the UI last read them),
    /// samples beyond full scale, frames limited.
    pub deferred_notes: AtomicU64,
    pub late_dropped: AtomicU64,
    pub max_lateness: AtomicU64,
    pub voice_capacity: AtomicU32,
    #[cfg(test)]
    pub synth_render_threads: AtomicU32,
    pub overflow_blocks: AtomicU64,
    pub at_capacity: AtomicU64,
    pub not_started: AtomicU64,
    pub released_oldest: AtomicU64,
    pub peak_pre: AtomicU32,
    pub peak_post: AtomicU32,
    pub clipped: AtomicU64,
    pub limited: AtomicU64,
    /// Notes not played because a key correction moved them outside 0–127.
    pub correction_skipped: AtomicU64,
    /// v1.0-beta2 (B2-F12): notes kept from the outputs by Event Processing
    /// (outside the 88 keys; in the ignored velocity band).
    pub filtered_keys: AtomicU64,
    pub filtered_velocity: AtomicU64,
    /// B2-F12B / D / E: resets sent / ignored, notes ended by a re-strike.
    pub resets_sent: AtomicU64,
    pub resets_ignored: AtomicU64,
    pub retriggered: AtomicU64,
    pub loop_wraps: AtomicU64,
    /// Worker time per block, exponential average (nanoseconds).
    pub block_nanos: AtomicU64,
    /// Frames the device asked for in its last callback.
    pub callback_frames: AtomicU32,
    /// v1.0-Launch L-B03: times rendering waited for an external output's
    /// backlog ("keep every event" / Pre-Render).
    pub external_waits: AtomicU64,
    pub worker_error: Mutex<Option<String>>,
    /// v1.0-Launch L-B02: the device clock for external MIDI. Each callback
    /// publishes (seqlock) the ring position of the first sample it took,
    /// how many samples it took and when (nanoseconds since `epoch`); the
    /// MIDI dispatcher interpolates between callbacks so every message is
    /// sent at its own time instead of in device-period bursts.
    clock_seq: AtomicU64,
    clock_read: AtomicU64,
    clock_took: AtomicU64,
    clock_nanos: AtomicU64,
    pub epoch: Instant,
}

impl Shared {
    pub(super) fn new(ring_frames: usize) -> Self {
        Shared {
            ring: Ring::new(ring_frames.max(RING_CAPACITY_FRAMES) * 2),
            aud_ring: Ring::new(AUDITION_RING_FRAMES * 8),
            preroll: AtomicU64::new(0),
            prerolled: AtomicBool::new(true),
            seg_seq: AtomicU64::new(0),
            seg_gen: AtomicU64::new(0),
            seg_w0: AtomicU64::new(0),
            seg_f0: AtomicU64::new(0),
            position: AtomicU64::new(u64::MAX),
            underruns: AtomicU64::new(0),
            auditioning: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            device_failed: AtomicBool::new(false),
            device_error: Mutex::new(None),
            ended_generation: AtomicU64::new(u64::MAX),
            active_voices: AtomicU32::new(0),
            skipped_notes: AtomicU64::new(0),
            deferred_notes: AtomicU64::new(0),
            late_dropped: AtomicU64::new(0),
            max_lateness: AtomicU64::new(0),
            voice_capacity: AtomicU32::new(0),
            #[cfg(test)]
            synth_render_threads: AtomicU32::new(0),
            overflow_blocks: AtomicU64::new(0),
            at_capacity: AtomicU64::new(0),
            not_started: AtomicU64::new(0),
            released_oldest: AtomicU64::new(0),
            peak_pre: AtomicU32::new(0),
            peak_post: AtomicU32::new(0),
            clipped: AtomicU64::new(0),
            limited: AtomicU64::new(0),
            correction_skipped: AtomicU64::new(0),
            filtered_keys: AtomicU64::new(0),
            filtered_velocity: AtomicU64::new(0),
            resets_sent: AtomicU64::new(0),
            resets_ignored: AtomicU64::new(0),
            retriggered: AtomicU64::new(0),
            loop_wraps: AtomicU64::new(0),
            block_nanos: AtomicU64::new(0),
            callback_frames: AtomicU32::new(0),
            external_waits: AtomicU64::new(0),
            worker_error: Mutex::new(None),
            clock_seq: AtomicU64::new(0),
            clock_read: AtomicU64::new(0),
            clock_took: AtomicU64::new(0),
            clock_nanos: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// Nanoseconds since `epoch` (the clock the dispatcher and the callback
    /// share).
    pub fn now_nanos(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// L-B02: called by the device callback after it took `took` ring
    /// samples starting at `read`. Real-time safe (atomics only).
    pub(super) fn publish_clock(&self, read: u64, took: u64) {
        let now = self.now_nanos();
        let s = self.clock_seq.load(Ordering::Relaxed);
        self.clock_seq.store(s + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.clock_read.store(read, Ordering::Relaxed);
        self.clock_took.store(took, Ordering::Relaxed);
        self.clock_nanos.store(now, Ordering::Relaxed);
        self.clock_seq.store(s + 2, Ordering::Release);
    }

    /// L-B02: the latest device-clock anchor; `None` before the first
    /// callback (or while one is being written — retry).
    pub fn clock(&self) -> Option<super::midi_out::ClockAnchor> {
        for _ in 0..4 {
            let s1 = self.clock_seq.load(Ordering::Acquire);
            if s1 == 0 {
                return None;
            }
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let read = self.clock_read.load(Ordering::Relaxed);
            let took = self.clock_took.load(Ordering::Relaxed);
            let nanos = self.clock_nanos.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.clock_seq.load(Ordering::Relaxed) == s1 {
                return Some(super::midi_out::ClockAnchor { read, took, nanos });
            }
        }
        None
    }

    fn publish_segment(&self, generation: u64, w0: u64, f0: u64) {
        let s = self.seg_seq.load(Ordering::Relaxed);
        self.seg_seq.store(s + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.seg_gen.store(generation, Ordering::Relaxed);
        self.seg_w0.store(w0, Ordering::Relaxed);
        self.seg_f0.store(f0, Ordering::Relaxed);
        self.seg_seq.store(s + 2, Ordering::Release);
    }

    /// Reads the segment; `None` while it is being written (retry next callback).
    fn read_segment(&self) -> Option<(u64, u64, u64)> {
        for _ in 0..4 {
            let s1 = self.seg_seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                continue;
            }
            let g = self.seg_gen.load(Ordering::Relaxed);
            let w0 = self.seg_w0.load(Ordering::Relaxed);
            let f0 = self.seg_f0.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.seg_seq.load(Ordering::Relaxed) == s1 {
                return Some((g, w0, f0));
            }
        }
        None
    }

    /// The frame last sent to the device for `generation`, if any.
    pub fn heard_frame(&self, generation: u64) -> Option<u64> {
        let p = self.position.load(Ordering::Acquire);
        (p != u64::MAX && p >> 48 == generation & 0xFFFF).then_some(p & 0xFFFF_FFFF_FFFF)
    }
}

/// Synthesizer adapter (the audition synthesizer, offline renders).
pub(crate) struct Sink<'a>(pub(crate) &'a mut Synthesizer);

/// The transport's synthesizers as one sink for the scheduler.
pub(crate) struct BankSink<'a>(pub(crate) &'a mut SynthBank);

impl SynthSink for BankSink<'_> {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        self.0.note_on(channel, key, velocity);
    }
    fn note_off(&mut self, channel: u8, key: u8) {
        self.0.note_off(channel, key);
    }
    fn channel_message(&mut self, status: u8, d0: u8, d1: u8) {
        self.0.channel_message(status, d0, d1);
    }
    fn silence(&mut self) {
        self.0.silence();
    }
    fn reset(&mut self) {
        self.0.reset();
    }
    fn master_volume(&mut self, value: u16) {
        let gain = f32::from(value.min(MASTER_VOLUME_MAX)) / f32::from(MASTER_VOLUME_MAX);
        self.0.set_master_volume(SYNTH_MASTER * gain);
    }
}

/// The scheduler limits of a synthesis configuration at `sample_rate`.
///
/// Real time keeps the per-block note-on budget (a larger burst is spread
/// over the next blocks) and skips a note more than 50 ms late, both counted.
/// v1.0-beta2 (B2-S33, contract §012 quality modes): Pre-Render is
/// event-preserving — every note starts in its own block (no budget, nothing
/// skipped for lateness), because the ring absorbs a block that takes longer
/// than real time. The sounding-note bound (memory) applies to both.
pub fn limits_for(cfg: &SynthConfig, sample_rate: u32, prerender: bool) -> SchedulerLimits {
    let cfg = cfg.clamped();
    SchedulerLimits {
        processing: cfg.processing,
        max_note_ons_per_block: if prerender {
            usize::MAX
        } else {
            cfg.note_ons_per_block
        },
        max_pending_offs: plumspirit_engine::playback::DEFAULT_PENDING_OFFS,
        max_lateness_frames: if prerender {
            u64::MAX
        } else {
            // 50 ms.
            u64::from(sample_rate) / 20
        },
    }
}

/// Publishes an f32 peak as the maximum since the last read.
fn publish_peak(slot: &AtomicU32, v: f32) {
    if v.is_finite() && v > 0.0 {
        slot.fetch_max(v.to_bits(), Ordering::Relaxed);
    }
}

/// Reads and resets a published peak.
pub fn take_peak(slot: &AtomicU32) -> f32 {
    f32::from_bits(slot.swap(0, Ordering::Relaxed))
}

impl SynthSink for Sink<'_> {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        if key <= super::synth::MIDI_TOP_KEY {
            self.0
                .note_on(i32::from(channel), i32::from(key), i32::from(velocity));
        }
    }
    fn note_off(&mut self, channel: u8, key: u8) {
        if key <= super::synth::MIDI_TOP_KEY {
            self.0.note_off(i32::from(channel), i32::from(key));
        }
    }
    fn channel_message(&mut self, status: u8, d0: u8, d1: u8) {
        self.0.process_midi_message(
            i32::from(status & 0x0F),
            i32::from(status & 0xF0),
            i32::from(d0),
            i32::from(d1),
        );
    }
    fn silence(&mut self) {
        self.0.note_off_all(true);
    }
    fn reset(&mut self) {
        self.0.reset();
        self.0.set_master_volume(SYNTH_MASTER);
    }
    /// Universal Master Volume: a linear gain on the whole output.
    fn master_volume(&mut self, value: u16) {
        let gain = f32::from(value.min(MASTER_VOLUME_MAX)) / f32::from(MASTER_VOLUME_MAX);
        self.0.set_master_volume(SYNTH_MASTER * gain);
    }
}

/// Route 0 while no SoundFont is loaded (L-B06): the song plays on the
/// external outputs; what the internal synthesizer would play is not heard.
struct NoSynth;

impl SynthSink for NoSynth {
    fn note_on(&mut self, _: u8, _: u8, _: u8) {}
    fn note_off(&mut self, _: u8, _: u8) {}
    fn channel_message(&mut self, _: u8, _: u8, _: u8) {}
    fn silence(&mut self) {}
    fn reset(&mut self) {}
}

/// Runs `f` with the synthesizer as route 0 and the external outputs as routes
/// 1, 2, …, each stamped with the ring position `stamp` of the current block.
/// Without a synthesizer route 0 is silent (L-B06).
fn with_sinks<R>(
    synth: Option<&mut SynthBank>,
    externals: &mut [ExternalSink],
    stamp: u64,
    f: impl FnOnce(&mut [&mut dyn SynthSink]) -> R,
) -> R {
    let (mut bank, mut none);
    let internal: &mut dyn SynthSink = match synth {
        Some(s) => {
            bank = BankSink(s);
            &mut bank
        }
        None => {
            none = NoSynth;
            &mut none
        }
    };
    let mut list: Vec<&mut dyn SynthSink> = Vec::with_capacity(externals.len() + 1);
    list.push(internal);
    for e in externals.iter_mut() {
        e.set_block(stamp);
        list.push(e);
    }
    f(&mut list)
}

/// One full RustySynth synthesizer (256 voices, effects on): offline renders
/// and measurements.
#[cfg(test)]
pub fn synth_settings(sample_rate: u32) -> SynthesizerSettings {
    super::synth::shard_settings(sample_rate, MAX_VOICES, true)
}

struct Worker {
    shared: Arc<Shared>,
    rx: Receiver<Command>,
    sample_rate: u32,
    synth: Option<SynthBank>,
    font: Option<Arc<SoundFont>>,
    config: SynthConfig,
    master: MasterBus,
    /// Output stage of the separate audition stream (Pre-Render playback).
    aud_master: MasterBus,
    render: RenderConfig,
    sched: Option<Scheduler>,
    playing: bool,
    generation: u64,
    end_frame: u64,
    externals: Vec<ExternalSink>,
    /// Sends external MIDI (started with the first external route).
    dispatcher: Option<Dispatcher>,
    audition: Option<Audition>,
    plugin_midi: Option<super::midi_overlay::Voice>,
    /// Reference audio mixed into the transport's blocks.
    reference: Option<Arc<super::reference::RefPlay>>,
}

/// Frames to keep buffered ahead: at least `min`, and two device callbacks
/// (a device asking for more than is buffered would get gaps).
fn ahead_target(shared: &Shared, min: usize) -> usize {
    (shared.callback_frames.load(Ordering::Relaxed) as usize * 2).clamp(min, RING_CAPACITY_FRAMES)
}

/// Render mode (v0.8 §137 / §138).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderConfig {
    /// Pre-Render: render far ahead into a bounded buffer and start playing
    /// once it is filled (accuracy over latency).
    pub prerender: bool,
    /// Frames rendered ahead in Pre-Render mode.
    pub ahead_frames: usize,
}

/// Pre-Render bounds (milliseconds) and default.
pub const MIN_AHEAD_MS: u32 = 1_000;
pub const MAX_AHEAD_MS: u32 = 60_000;
pub const DEFAULT_AHEAD_MS: u32 = 8_000;

impl RenderConfig {
    pub fn new(prerender: bool, ahead_ms: u32, sample_rate: u32) -> RenderConfig {
        let ms = ahead_ms.clamp(MIN_AHEAD_MS, MAX_AHEAD_MS);
        RenderConfig {
            prerender,
            ahead_frames: (u64::from(ms) * u64::from(sample_rate) / 1000) as usize,
        }
    }

    /// Ring frames this mode needs.
    pub fn ring_frames(&self) -> usize {
        if self.prerender {
            (self.ahead_frames + 2 * RING_FRAMES).max(RING_CAPACITY_FRAMES)
        } else {
            RING_CAPACITY_FRAMES
        }
    }

    /// Memory of the Pre-Render buffer (bytes): stereo f32.
    pub fn buffer_bytes(&self) -> usize {
        self.ring_frames().next_power_of_two() * 2 * 4
    }
}

impl Worker {
    fn audition_busy(&self) -> bool {
        self.audition.as_ref().is_some_and(Audition::busy)
    }
    fn set_error(&self, msg: String) {
        if let Ok(mut e) = self.shared.worker_error.lock() {
            *e = Some(msg);
        }
    }

    /// (Re)creates the transport and audition synthesizers for the current
    /// SoundFont and settings — between blocks, on the thread that owns them;
    /// sounding notes are re-struck. On failure the previous ones stay.
    fn rebuild_synth(&mut self) {
        let Some(sf) = self.font.clone() else {
            return;
        };
        match SynthBank::new(&sf, self.sample_rate, &self.config) {
            Ok(new) => {
                // The audition synthesizer is replaced too: no audition voice of
                // the old SoundFont / settings survives.
                self.audition = Audition::new(&sf, self.sample_rate, &self.config);
                self.shared
                    .voice_capacity
                    .store(new.capacity() as u32, Ordering::Relaxed);
                #[cfg(test)]
                self.shared
                    .synth_render_threads
                    .store(new.render_threads() as u32, Ordering::Relaxed);
                let resume = self
                    .sched
                    .as_ref()
                    .filter(|_| self.playing)
                    .map(|s| s.index().tempo.first_tick_at_or_after_frame(s.frame()));
                // L-B06: the first SoundFont of a playback that started on
                // external outputs only: the synthesizer joins at the
                // playhead; the outputs and the buffered audio are untouched.
                let first = self.synth.is_none();
                self.synth = Some(new); // the old synthesizers (and samples) are released here
                if first && let Some(sched) = self.sched.as_mut().filter(|_| self.playing) {
                    let stamp = self.shared.ring.write_pos();
                    with_sinks(self.synth.as_mut(), &mut self.externals, stamp, |s| {
                        sched.join_route(0, s)
                    });
                } else if let Some(tick) = resume {
                    self.start_at(tick, self.generation, true);
                }
            }
            Err(e) => self.set_error(e),
        }
    }

    /// Meters and counters of the output stage for the UI.
    fn publish_master(&mut self) {
        let (pre, post) = self.master.take_meters();
        publish_peak(&self.shared.peak_pre, pre);
        publish_peak(&self.shared.peak_post, post);
        self.shared
            .clipped
            .store(self.master.clipped, Ordering::Relaxed);
        self.shared
            .limited
            .store(self.master.limited, Ordering::Relaxed);
    }

    /// Starts audio for `generation` at `tick` (flushing what was buffered).
    fn start_at(&mut self, tick: u64, generation: u64, restrike: bool) {
        let Some(sched) = self.sched.as_mut() else {
            return;
        };
        let synth = self.synth.as_mut();
        let w0 = self.shared.ring.flush();
        // Pre-Render may have limited unheard future audio. A seek starts a
        // fresh envelope in both modes, independent of how far ahead it ran.
        self.master.reset_envelope();
        with_sinks(synth, &mut self.externals, w0, |s| {
            sched.seek_multi_with(tick, s, restrike)
        });
        let idx = sched.index();
        self.end_frame = idx
            .tempo
            .frame_at_tick(idx.end_tick)
            .saturating_add(u64::from(self.sample_rate / TAIL_SECONDS_DIV));
        self.shared.publish_segment(generation, w0, sched.frame());
        // Pre-Render: the callback waits until this much is buffered (or the
        // song has been rendered to its end).
        let preroll = if self.render.prerender {
            (self.render.ahead_frames as u64).min(self.end_frame.saturating_sub(sched.frame()))
        } else {
            0
        };
        self.shared.preroll.store(preroll, Ordering::Release);
        self.shared.prerolled.store(preroll == 0, Ordering::Release);
        self.generation = generation;
        self.playing = true;
        self.shared.playing.store(true, Ordering::Release);
    }

    fn handle(&mut self, cmd: Command) -> bool {
        if matches!(
            &cmd,
            Command::SetSoundFont(_)
                | Command::SetRender(_)
                | Command::Configure(_)
                | Command::Play { .. }
                | Command::SetLoop(_)
                | Command::SetEligible(_)
                | Command::SetRoutes(_)
                | Command::Update { .. }
                | Command::Pause
                | Command::ReleaseProject { .. }
                | Command::Shutdown
        ) {
            self.plugin_midi = None;
        }
        match cmd {
            Command::PluginMidi { voice, life } => {
                if !life.active() {
                    life.end(super::midi_overlay::Status::Canceled);
                    return true;
                }
                let valid = self.playing
                    && !self.render.prerender
                    && self.plugin_midi.is_none()
                    && voice.sample_rate() == self.sample_rate
                    && self
                        .sched
                        .as_ref()
                        .zip(self.font.as_ref())
                        .is_some_and(|(s, f)| voice.matches(&s.index().tempo, f));
                if valid {
                    self.plugin_midi = voice.start(
                        self.sched.as_ref().expect("checked scheduler").frame(),
                        self.end_frame,
                        life,
                    );
                } else {
                    life.end(super::midi_overlay::Status::Refused);
                }
            }
            Command::Shutdown => return false,
            Command::SetSoundFont(sf) => {
                self.font = Some(Arc::clone(&sf));
                self.rebuild_synth();
            }
            Command::SetRender(r) => {
                self.render = r;
                if let Some(s) = self.sched.as_mut() {
                    s.set_limits(limits_for(&self.config, self.sample_rate, r.prerender));
                }
            }
            Command::Configure(cfg) => {
                let cfg = cfg.clamped();
                let rebuild = cfg.needs_rebuild(&self.config);
                self.config = cfg;
                self.master.configure(&cfg);
                self.aud_master.configure(&cfg);
                if let Some(bank) = self.synth.as_mut() {
                    bank.voice_policy = cfg.voice_policy;
                }
                if let Some(s) = self.sched.as_mut() {
                    s.set_limits(limits_for(&cfg, self.sample_rate, self.render.prerender));
                }
                if rebuild && self.font.is_some() {
                    self.rebuild_synth();
                }
            }
            Command::Play {
                index,
                tick,
                generation,
                looping,
                restrike,
            } => {
                // L-B06: without a SoundFont the song still plays on the
                // external outputs (the transport only starts so when no
                // audible track needs the internal synthesizer); the
                // synthesizer joins at the playhead once one is loaded
                // (`rebuild_synth`).
                let mut sched = Scheduler::new(
                    index,
                    limits_for(&self.config, self.sample_rate, self.render.prerender),
                );
                sched.set_loop(looping);
                self.sched = Some(sched);
                self.start_at(tick, generation, restrike);
            }
            Command::SetLoop(range) => {
                if let Some(s) = self.sched.as_mut() {
                    s.set_loop(range);
                }
            }
            Command::SetEligible(mask) => {
                if let Some(sched) = self.sched.as_mut() {
                    let stamp = self.shared.ring.write_pos();
                    with_sinks(self.synth.as_mut(), &mut self.externals, stamp, |s| {
                        sched.set_eligible_multi(mask, s)
                    });
                }
            }
            Command::SetRoutes(routes) => {
                for old in &self.externals {
                    old.route.flush_now();
                }
                if !routes.is_empty() && self.dispatcher.is_none() {
                    match Dispatcher::start(&self.shared, self.sample_rate) {
                        Ok(d) => self.dispatcher = Some(d),
                        Err(e) => self.set_error(e),
                    }
                }
                if let Some(d) = &self.dispatcher {
                    d.set_routes(routes.clone());
                }
                self.externals = routes.into_iter().map(ExternalSink::new).collect();
            }
            Command::Update { index, generation } => {
                if let Some(sched) = self.sched.as_mut() {
                    let synth = self.synth.as_mut();
                    // Continue seamlessly after the audio already buffered: no flush,
                    // re-seek at the worker's position with the new data.
                    let tick = sched
                        .index()
                        .tempo
                        .first_tick_at_or_after_frame(sched.frame());
                    let w0 = self.shared.ring.write_pos();
                    with_sinks(synth, &mut self.externals, w0, |s| {
                        sched.set_index_multi(index, tick, s)
                    });
                    let idx = sched.index();
                    self.end_frame = idx
                        .tempo
                        .frame_at_tick(idx.end_tick)
                        .saturating_add(u64::from(self.sample_rate / TAIL_SECONDS_DIV));
                    self.shared.publish_segment(generation, w0, sched.frame());
                    self.generation = generation;
                }
            }
            Command::AuditionOn {
                channel,
                key,
                velocity,
                state,
                hold,
            } => {
                let tail = self.sample_rate + self.sample_rate / 2;
                if let Some(a) = self.audition.as_mut() {
                    let ch = channel & 0x0F;
                    let target = *state;
                    {
                        let mut sink = Sink(&mut a.synth);
                        ChannelState::transition(
                            &a.applied[usize::from(ch)],
                            &target,
                            ch,
                            &mut sink,
                        );
                    }
                    a.applied[usize::from(ch)] = target;
                    // Retrigger: the same key ends first (one voice per key).
                    if let Some(i) = a.notes.iter().position(|n| n.0 == ch && n.1 == key) {
                        a.synth.note_off(i32::from(ch), i32::from(key));
                        a.notes.remove(i);
                    }
                    a.synth
                        .note_on(i32::from(ch), i32::from(key), i32::from(velocity.max(1)));
                    a.notes.push((ch, key, hold));
                    a.tail = tail;
                    if !self.playing {
                        self.shared.auditioning.store(true, Ordering::Release);
                    }
                }
            }
            Command::AuditionOff { channel, key } => {
                if let Some(a) = self.audition.as_mut() {
                    let ch = channel & 0x0F;
                    if let Some(i) = a.notes.iter().position(|n| n.0 == ch && n.1 == key) {
                        a.synth.note_off(i32::from(ch), i32::from(key));
                        a.notes.remove(i);
                    }
                }
            }
            Command::AuditionAllOff => {
                if let Some(a) = self.audition.as_mut() {
                    a.synth.note_off_all(false);
                    a.notes.clear();
                }
            }
            Command::SetReference(r) => {
                self.reference = r;
            }
            Command::Pause => {
                self.playing = false;
                self.shared.playing.store(false, Ordering::Release);
                let w = self.shared.ring.flush();
                if let Some(sched) = self.sched.as_mut() {
                    with_sinks(self.synth.as_mut(), &mut self.externals, w, |s| {
                        sched.silence_multi(s)
                    });
                }
            }
            Command::ReleaseProject {
                before_generation,
                done,
            } => {
                if self.generation < before_generation {
                    self.playing = false;
                    self.shared.playing.store(false, Ordering::Release);
                    let w = self.shared.ring.flush();
                    if let Some(sched) = self.sched.as_mut() {
                        with_sinks(self.synth.as_mut(), &mut self.externals, w, |s| {
                            sched.silence_multi(s)
                        });
                    }
                    self.sched = None;
                    self.reference = None;
                }
                let _ = done.send(());
            }
        }
        true
    }

    fn run(mut self) {
        let mut left = [0f32; BLOCK];
        let mut right = [0f32; BLOCK];
        let mut inter = [0f32; BLOCK * 2];
        loop {
            // Commands: block while idle, poll while playing or auditioning.
            let first = if self.playing || self.audition_busy() {
                match self.rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            } else {
                match self.rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            if let Some(c) = first {
                if !self.handle(c) {
                    return;
                }
                while let Ok(c) = self.rx.try_recv() {
                    if !self.handle(c) {
                        return;
                    }
                }
            }
            if self
                .plugin_midi
                .as_ref()
                .is_some_and(|voice| !voice.active())
            {
                self.plugin_midi = None;
            }
            if !self.playing {
                self.render_audition_only(&mut left, &mut right, &mut inter);
                continue;
            }
            let Some(sched) = self.sched.as_mut() else {
                self.playing = false;
                continue;
            };
            let mut synth = self.synth.as_mut();
            // L-B06: an output that finished opening while playing joins at
            // the worker's position (only its own route is re-positioned).
            for r in 0..self.externals.len() {
                if self.externals[r].route.begin_join() {
                    let stamp = self.shared.ring.write_pos();
                    with_sinks(synth.as_deref_mut(), &mut self.externals, stamp, |s| {
                        sched.join_route(r + 1, s)
                    });
                }
            }
            let mut rendered = 0;
            let prerender = self.render.prerender;
            let target = if prerender {
                self.render
                    .ahead_frames
                    .min(self.shared.ring.free() / 2 + self.shared.ring.available() / 2 - BLOCK)
                    * 2
            } else {
                ahead_target(&self.shared, RING_FRAMES) * 2
            };
            // L-B03: "keep every event" (and Pre-Render): no rendering ahead
            // while an external output has a full backlog — playback waits
            // for the device instead of dropping note-ons.
            let keep_all = prerender || self.config.external_keep_all;
            let backlog = |externals: &[ExternalSink]| {
                keep_all
                    && externals
                        .iter()
                        .any(|e| e.route.pending_len() >= super::midi_out::KEEP_ALL_HIGH_WATER)
            };
            if backlog(&self.externals) {
                self.shared.external_waits.fetch_add(1, Ordering::Relaxed);
                // Pre-Render's start must not wait for a buffer the backlog
                // keeps from filling.
                self.shared.prerolled.store(true, Ordering::Release);
            }
            // Bounded batch so commands (pause, seek) are handled promptly.
            while rendered < 16
                && self.shared.ring.available() < target
                && self.shared.ring.free() >= BLOCK * 2
                && !backlog(&self.externals)
            {
                let t = Instant::now();
                let stamp = self.shared.ring.write_pos();
                let first = sched.frame();
                let wrap = with_sinks(synth.as_deref_mut(), &mut self.externals, stamp, |s| {
                    sched.advance_multi(BLOCK as u32, s)
                });
                match synth.as_deref_mut() {
                    Some(synth) => synth.render(&mut inter),
                    None => inter.fill(0.0),
                }
                if let Some(voice) = self.plugin_midi.as_mut()
                    && !voice.mix(first, wrap.is_some(), &mut inter)
                {
                    self.plugin_midi = None;
                }
                // Audition notes are mixed in (their own synthesizer) — in
                // Pre-Render they go to their own short ring instead.
                if let Some(a) = self.audition.as_mut().filter(|a| a.busy() && !prerender) {
                    let (mut al, mut ar) = ([0f32; BLOCK], [0f32; BLOCK]);
                    a.synth.render(&mut al, &mut ar);
                    for i in 0..BLOCK {
                        inter[2 * i] += al[i];
                        inter[2 * i + 1] += ar[i];
                    }
                    a.advance(BLOCK as u32, self.sample_rate + self.sample_rate / 2);
                }
                // The reference recording, from this block's playback frames.
                if let Some(r) = &self.reference {
                    r.mix(
                        self.sample_rate,
                        first,
                        wrap.as_ref().map(|w| (w.offset, w.frame)),
                        &mut inter,
                    );
                }
                // Headroom: gain, limiter, meters (after everything is mixed).
                self.master.process(&mut inter);
                self.shared.ring.push(&inter);
                if let Some(w) = wrap {
                    // Loop: audio from this point of the block is the loop start.
                    self.shared.publish_segment(
                        self.generation,
                        stamp + 2 * u64::from(w.offset),
                        w.frame,
                    );
                    self.shared.loop_wraps.fetch_add(1, Ordering::Relaxed);
                }
                let ns = t.elapsed().as_nanos() as u64;
                let avg = self.shared.block_nanos.load(Ordering::Relaxed);
                self.shared
                    .block_nanos
                    .store(avg - avg / 16 + ns / 16, Ordering::Relaxed);
                rendered += 1;
                if !self.shared.prerolled.load(Ordering::Relaxed)
                    && (self.shared.ring.available() / 2) as u64
                        >= self.shared.preroll.load(Ordering::Relaxed)
                {
                    self.shared.prerolled.store(true, Ordering::Release);
                }
                if sched.finished() && sched.frame() >= self.end_frame {
                    self.shared.prerolled.store(true, Ordering::Release);
                    self.playing = false;
                    self.shared.playing.store(false, Ordering::Release);
                    self.shared
                        .ended_generation
                        .store(self.generation, Ordering::Release);
                    self.plugin_midi = None;
                    break;
                }
            }
            let st = sched.stats();
            let sh = &self.shared;
            sh.skipped_notes
                .store(st.note_ons_skipped, Ordering::Relaxed);
            sh.correction_skipped
                .store(st.correction_skipped, Ordering::Relaxed);
            sh.filtered_keys.store(st.filtered_keys, Ordering::Relaxed);
            sh.filtered_velocity
                .store(st.filtered_velocity, Ordering::Relaxed);
            sh.resets_sent.store(st.resets_sent, Ordering::Relaxed);
            sh.resets_ignored
                .store(st.resets_ignored, Ordering::Relaxed);
            sh.retriggered.store(st.retriggered, Ordering::Relaxed);
            sh.active_voices
                .store(sched.pending_offs() as u32, Ordering::Relaxed);
            sh.deferred_notes
                .store(st.note_ons_deferred, Ordering::Relaxed);
            sh.late_dropped.store(st.late_dropped, Ordering::Relaxed);
            sh.max_lateness.store(st.max_lateness, Ordering::Relaxed);
            if let Some(synth) = synth {
                sh.overflow_blocks
                    .store(synth.overflow_blocks, Ordering::Relaxed);
                sh.at_capacity.store(synth.at_capacity, Ordering::Relaxed);
                sh.not_started.store(synth.not_started, Ordering::Relaxed);
                sh.released_oldest
                    .store(synth.released_oldest, Ordering::Relaxed);
            }
            self.publish_master();
            if prerender {
                self.render_audition_side(&mut left, &mut right, &mut inter);
            }
            if rendered == 0 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Pre-Render playback: audition notes into their own short ring (the
    /// callback mixes it), so they are heard at once although the transport
    /// audio is buffered seconds ahead.
    fn render_audition_side(
        &mut self,
        left: &mut [f32; BLOCK],
        right: &mut [f32; BLOCK],
        inter: &mut [f32; BLOCK * 2],
    ) {
        let tail = self.sample_rate + self.sample_rate / 2;
        let Some(a) = self.audition.as_mut().filter(|a| a.busy()) else {
            return;
        };
        let ring = &self.shared.aud_ring;
        let target = ahead_target(&self.shared, AUDITION_RING_FRAMES) * 2;
        let mut n = 0;
        while n < 16 && ring.available() < target && ring.free() >= BLOCK * 2 {
            a.synth.render(left, right);
            for i in 0..BLOCK {
                inter[2 * i] = left[i];
                inter[2 * i + 1] = right[i];
            }
            self.aud_master.process(inter);
            ring.push(inter);
            a.advance(BLOCK as u32, tail);
            n += 1;
            if !a.busy() {
                break;
            }
        }
    }
}

impl Worker {
    /// While the transport is stopped: renders audition notes (and their
    /// release) only, keeping the ring short so they are heard quickly.
    fn render_audition_only(
        &mut self,
        left: &mut [f32; BLOCK],
        right: &mut [f32; BLOCK],
        inter: &mut [f32; BLOCK * 2],
    ) {
        let tail = self.sample_rate + self.sample_rate / 2;
        let Some(a) = self.audition.as_mut().filter(|a| a.busy()) else {
            self.shared.auditioning.store(false, Ordering::Release);
            return;
        };
        self.shared.auditioning.store(true, Ordering::Release);
        let target = ahead_target(&self.shared, AUDITION_RING_FRAMES);
        let mut rendered = 0;
        while rendered < 16
            && self.shared.ring.available() < target * 2
            && self.shared.ring.free() >= BLOCK * 2
        {
            a.synth.render(left, right);
            for i in 0..BLOCK {
                inter[2 * i] = left[i];
                inter[2 * i + 1] = right[i];
            }
            self.master.process(inter);
            self.shared.ring.push(inter);
            a.advance(BLOCK as u32, tail);
            rendered += 1;
            if !a.busy() {
                self.shared.auditioning.store(false, Ordering::Release);
                break;
            }
        }
        if rendered == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Converts and writes one callback buffer. Real-time safe.
fn fill<T: SizedSample + FromSample<f32>>(
    data: &mut [T],
    channels: usize,
    shared: &Shared,
    last: &mut (f32, f32),
) {
    let ring = &shared.ring;
    ring.skip_stale();
    let frames = data.len() / channels.max(1);
    shared
        .callback_frames
        .store(frames.min(u32::MAX as usize) as u32, Ordering::Relaxed);
    // Pre-Render: nothing is consumed until the start is buffered.
    let waiting = !shared.prerolled.load(Ordering::Acquire);
    let avail = if waiting { 0 } else { ring.available() / 2 };
    let n = frames.min(avail);
    // L-B02: where this buffer starts in the ring (the external MIDI clock).
    let first = ring.read_pos();
    let aud = &shared.aud_ring;
    for f in 0..frames {
        let (l, r) = if f < n {
            (ring.pop(), ring.pop())
        } else {
            // Out of audio (paused, seeking or underrun): glide to silence instead of
            // jumping, which would click.
            (last.0 * 0.995, last.1 * 0.995)
        };
        *last = (l, r);
        let (l, r) = if aud.available() >= 2 {
            (l + aud.pop(), r + aud.pop())
        } else {
            (l, r)
        };
        let (l, r) = (l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0));
        let out = &mut data[f * channels..(f + 1) * channels];
        match channels {
            1 => out[0] = T::from_sample((l + r) * 0.5),
            _ => {
                out[0] = T::from_sample(l);
                out[1] = T::from_sample(r);
                for o in &mut out[2..] {
                    *o = T::from_sample(0.0);
                }
            }
        }
    }
    if n < frames && !waiting && shared.playing.load(Ordering::Relaxed) {
        shared.underruns.fetch_add(1, Ordering::Relaxed);
    }
    shared.publish_clock(first, 2 * n as u64);
    if let Some((g, w0, f0)) = shared.read_segment() {
        let read = ring.read_pos();
        if read >= w0 {
            let frame = f0 + (read - w0) / 2;
            shared.position.store(
                ((g & 0xFFFF) << 48) | (frame & 0xFFFF_FFFF_FFFF),
                Ordering::Release,
            );
        }
    }
}

/// The audio pipeline for one output device.
pub struct AudioEngine {
    pub shared: Arc<Shared>,
    tx: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
    /// The device stream (`None` only for the headless test engine).
    stream: Option<cpal::Stream>,
    /// Whether the device stream is running (it is paused while the transport idles,
    /// so an idle editor costs no audio CPU).
    active: bool,
    pause_supported: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
    /// What was asked for (a different request needs a new engine).
    pub output: super::output::OutputConfig,
    /// Ring capacity (Pre-Render needs more than this → a new engine).
    pub ring_frames: usize,
    /// Fixed buffer size in use (`None`: the device default).
    pub buffer: Option<u32>,
    /// Choices that could not be honoured (shown in Audio Settings).
    pub notes: Vec<String>,
}

impl AudioEngine {
    /// Opens the chosen (else the default) output device. Errors are
    /// user-readable; choices that could not be honoured are in `notes`.
    pub fn start(
        synth: SynthConfig,
        output: &super::output::OutputConfig,
        render_ms: (bool, u32),
    ) -> Result<AudioEngine, String> {
        let (device, device_name, format, config, notes) = super::output::resolve(output)?;
        for n in &notes {
            log::warn!("audio: {n}");
        }
        let sample_rate = config.sample_rate;
        if !(16_000..=192_000).contains(&sample_rate) {
            return Err(format!(
                "The audio device uses an unsupported sample rate ({sample_rate} Hz)."
            ));
        }
        let channels = config.channels;
        let render = RenderConfig::new(render_ms.0, render_ms.1, sample_rate);
        let shared = Arc::new(Shared::new(render.ring_frames()));
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, &shared),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, &shared),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, &shared),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, &shared),
            cpal::SampleFormat::F64 => build::<f64>(&device, &config, &shared),
            other => Err(format!(
                "The audio device uses an unsupported sample format ({other})."
            )),
        }?;
        stream
            .play()
            .map_err(|e| format!("The audio stream could not start: {}.", sentence(&e)))?;
        let (tx, handle) = spawn_worker(&shared, sample_rate, synth, render)?;
        log::info!(
            "audio: {device_name}, {sample_rate} Hz, {channels} channel(s), {format}; ring {} frames, {} voices, gain {} dB, limiter {}",
            RING_FRAMES,
            synth.clamped().max_voices,
            synth.clamped().gain_db,
            synth.limiter
        );
        Ok(AudioEngine {
            shared,
            tx,
            worker: Some(handle),
            stream: Some(stream),
            active: true,
            pause_supported: true,
            sample_rate,
            channels,
            device_name,
            output: output.clone(),
            ring_frames: render.ring_frames(),
            buffer: match config.buffer_size {
                cpal::BufferSize::Fixed(f) => Some(f),
                cpal::BufferSize::Default => None,
            },
            notes,
        })
    }

    /// Sends a command without blocking the UI. Returns false when the worker is
    /// gone or its queue is full (the caller treats that as an audio failure).
    pub fn send(&self, cmd: Command) -> bool {
        match self.tx.try_send(cmd) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                log::warn!("audio command queue full");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Run on the project reaper: reliable even if the bounded command queue
    /// is full, with no queue wait on the UI or audio callback thread.
    pub fn project_release(&self, generation: u64) -> impl FnOnce() + Send + 'static {
        let sender = self.tx.clone();
        move || {
            let (tx, rx) = std::sync::mpsc::channel();
            if sender
                .send(Command::ReleaseProject {
                    before_generation: generation,
                    done: tx,
                })
                .is_ok()
            {
                let _ = rx.recv();
            }
        }
    }

    /// Starts or pauses the device stream. Failing to start is an error; failing to
    /// pause (unsupported by some hosts) only means the stream keeps outputting silence.
    pub fn set_active(&mut self, on: bool) -> Result<(), String> {
        if self.active == on {
            return Ok(());
        }
        let Some(stream) = &self.stream else {
            self.active = on;
            return Ok(());
        };
        if on {
            stream
                .play()
                .map_err(|e| format!("The audio stream could not start: {}.", sentence(&e)))?;
        } else if self.pause_supported
            && let Err(e) = stream.pause()
        {
            log::info!(
                "the audio device cannot pause; its stream keeps running silently while idle ({e})"
            );
            self.pause_supported = false;
        }
        self.active = on;
        Ok(())
    }

    pub fn worker_alive(&self) -> bool {
        self.worker.as_ref().is_some_and(|h| !h.is_finished())
    }
}

fn spawn_worker(
    shared: &Arc<Shared>,
    sample_rate: u32,
    config: SynthConfig,
    render: RenderConfig,
) -> Result<(SyncSender<Command>, JoinHandle<()>), String> {
    let (tx, rx) = sync_channel::<Command>(64);
    let config = config.clamped();
    let worker = Worker {
        shared: Arc::clone(shared),
        rx,
        sample_rate,
        synth: None,
        font: None,
        config,
        master: MasterBus::new(sample_rate, &config),
        aud_master: MasterBus::new(sample_rate, &config),
        render,
        sched: None,
        playing: false,
        generation: 0,
        end_frame: 0,
        externals: Vec::new(),
        dispatcher: None,
        audition: None,
        plugin_midi: None,
        reference: None,
    };
    let handle = std::thread::Builder::new()
        .name("plumspirit-synth".into())
        .spawn(move || worker.run())
        .map_err(|e| format!("The synthesis thread could not start: {}.", sentence(&e)))?;
    Ok((tx, handle))
}

#[cfg(test)]
impl AudioEngine {
    /// The real worker and ring without an output device: tests play the device's
    /// role by calling [`AudioEngine::pump`].
    pub fn headless(sample_rate: u32) -> AudioEngine {
        Self::headless_with(sample_rate, SynthConfig::default())
    }

    /// [`AudioEngine::headless`] with synthesis settings.
    pub fn headless_with(sample_rate: u32, config: SynthConfig) -> AudioEngine {
        Self::headless_render(sample_rate, config, RenderConfig::default())
    }

    /// [`AudioEngine::headless`] with synthesis and render settings.
    pub fn headless_render(
        sample_rate: u32,
        config: SynthConfig,
        render: RenderConfig,
    ) -> AudioEngine {
        let shared = Arc::new(Shared::new(render.ring_frames()));
        let (tx, handle) = spawn_worker(&shared, sample_rate, config, render).expect("worker");
        AudioEngine {
            shared,
            tx,
            worker: Some(handle),
            stream: None,
            active: true,
            pause_supported: true,
            sample_rate,
            channels: 2,
            device_name: "headless test device".into(),
            output: Default::default(),
            ring_frames: render.ring_frames(),
            buffer: None,
            notes: Vec::new(),
        }
    }

    /// Acts as a device callback that never starves while audio is being produced:
    /// takes `frames` stereo frames through the same `fill` the real callback uses.
    pub fn pump(&self, frames: usize, last: &mut (f32, f32)) -> Vec<f32> {
        let mut out = vec![0f32; frames * 2];
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(256);
            let deadline = Instant::now() + Duration::from_secs(10);
            // (flushed audio does not count: like the device, take only what
            // is still to be heard)
            while (self.shared.playing.load(Ordering::Acquire)
                || self.shared.auditioning.load(Ordering::Acquire))
                && {
                    self.shared.ring.skip_stale();
                    self.shared.ring.available() / 2 < n
                }
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_micros(100));
            }
            fill(&mut out[done * 2..(done + n) * 2], 2, &self.shared, last);
            done += n;
        }
        out
    }

    /// The real callback without `pump`'s starvation-preventing wait.
    /// Profiling supplies a device cadence and a reusable output buffer.
    pub fn profile_callback(&self, out: &mut [f32], last: &mut (f32, f32)) {
        fill(out, 2, &self.shared, last);
    }

    /// Waits until the worker has started the playback `generation` (test helper).
    /// Waits until the worker is (or is no longer) rendering (test helper).
    pub fn wait_playing(&self, playing: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.shared.playing.load(Ordering::Acquire) != playing && Instant::now() < deadline {
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Waits until the worker renders audition notes (test helper: the device
    /// stand-in only waits for audio the worker is known to produce).
    pub fn wait_auditioning(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.shared.auditioning.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_micros(100));
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.try_send(Command::Pause);
        let _ = self.tx.send(Command::Shutdown);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        // The stream is dropped after the worker has stopped.
    }
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels);
    let cb_shared = Arc::clone(shared);
    let err_shared = Arc::clone(shared);
    let mut last = (0f32, 0f32);
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                fill(data, channels, &cb_shared, &mut last);
            },
            move |err: cpal::Error| {
                use cpal::ErrorKind as K;
                match err.kind() {
                    // Recoverable or informational: keep playing.
                    K::Xrun | K::DeviceChanged | K::RealtimeDenied => {}
                    _ => {
                        if let Ok(mut e) = err_shared.device_error.lock() {
                            *e = Some(err.to_string());
                        }
                        err_shared.device_failed.store(true, Ordering::Release);
                    }
                }
            },
            None,
        )
        .map_err(|e| format!("The audio stream could not be opened: {}.", sentence(&e)))
}

/// What an offline render through the worker's path reports.
#[cfg(test)]
pub struct OfflineRender {
    pub samples: Vec<f32>,
    pub stats: plumspirit_engine::playback::SchedulerStats,
    pub peak_pre: f32,
    pub peak_post: f32,
    pub clipped: u64,
    pub limited: u64,
    pub overflow_blocks: u64,
    pub capacity: usize,
    pub render_threads: usize,
    pub worst_block_nanos: u64,
    pub sampled_rss_bytes: Option<u64>,
    pub pending_offs: usize,
}

/// Renders `frames` of `index` from `tick` exactly like the real-time worker
/// does (real-time scheduler limits, synthesizer shards, master bus) — v0.8
/// tests.
#[cfg(test)]
pub fn render_offline_bank(
    font: &Arc<SoundFont>,
    index: Arc<PlaybackIndex>,
    tick: u64,
    sample_rate: u32,
    frames: usize,
    cfg: &SynthConfig,
    looping: Option<(u64, u64)>,
) -> OfflineRender {
    let mut bank = SynthBank::new(font, sample_rate, cfg).expect("bank");
    let mut master = MasterBus::new(sample_rate, cfg);
    let mut sched = Scheduler::new(index, limits_for(cfg, sample_rate, false));
    sched.set_loop(looping);
    sched.seek(tick, &mut BankSink(&mut bank));
    let mut out = Vec::with_capacity(frames * 2);
    let mut inter = [0f32; BLOCK * 2];
    let (mut peak_pre, mut peak_post) = (0f32, 0f32);
    let mut done = 0;
    let mut worst_block_nanos = 0;
    let mut sampled_rss_bytes = crate::diagnostics::process_memory_bytes();
    while done < frames {
        let started = Instant::now();
        sched.advance(BLOCK as u32, &mut BankSink(&mut bank));
        bank.render(&mut inter);
        master.process(&mut inter);
        let (a, b) = master.take_meters();
        peak_pre = peak_pre.max(a);
        peak_post = peak_post.max(b);
        out.extend_from_slice(&inter[..2 * BLOCK.min(frames - done)]);
        worst_block_nanos = worst_block_nanos.max(started.elapsed().as_nanos() as u64);
        done += BLOCK;
        if done % (BLOCK * 256) == 0
            && let Some(rss) = crate::diagnostics::process_memory_bytes()
        {
            sampled_rss_bytes = Some(sampled_rss_bytes.unwrap_or(0).max(rss));
        }
    }
    if let Some(rss) = crate::diagnostics::process_memory_bytes() {
        sampled_rss_bytes = Some(sampled_rss_bytes.unwrap_or(0).max(rss));
    }
    OfflineRender {
        samples: out,
        stats: sched.stats(),
        peak_pre,
        peak_post,
        clipped: master.clipped,
        limited: master.limited,
        overflow_blocks: bank.overflow_blocks,
        capacity: bank.capacity(),
        render_threads: bank.render_threads(),
        worst_block_nanos,
        sampled_rss_bytes,
        pending_offs: sched.pending_offs(),
    }
}

/// Renders `frames` of `index` from `tick` offline (tests and benchmarks). Returns
/// interleaved stereo samples.
#[cfg(test)]
pub fn render_offline(
    font: &Arc<SoundFont>,
    index: Arc<PlaybackIndex>,
    tick: u64,
    sample_rate: u32,
    frames: usize,
) -> Vec<f32> {
    render_offline_loop(font, index, tick, sample_rate, frames, None)
}

/// [`render_offline`] with a loop.
#[cfg(test)]
pub fn render_offline_loop(
    font: &Arc<SoundFont>,
    index: Arc<PlaybackIndex>,
    tick: u64,
    sample_rate: u32,
    frames: usize,
    looping: Option<(u64, u64)>,
) -> Vec<f32> {
    let mut synth = Synthesizer::new(font, &synth_settings(sample_rate)).expect("synth");
    let mut sched = Scheduler::new(index, SchedulerLimits::default());
    sched.set_loop(looping);
    sched.seek(tick, &mut Sink(&mut synth));
    let mut out = Vec::with_capacity(frames * 2);
    let mut l = [0f32; BLOCK];
    let mut r = [0f32; BLOCK];
    let mut done = 0;
    while done < frames {
        sched.advance(BLOCK as u32, &mut Sink(&mut synth));
        synth.render(&mut l, &mut r);
        for i in 0..BLOCK.min(frames - done) {
            out.push(l[i]);
            out.push(r[i]);
        }
        done += BLOCK;
    }
    out
}

#[cfg(test)]
mod callback_tests {
    use super::*;

    #[test]
    fn callback_ignores_worker_locks_and_incomplete_position_publication() {
        let shared = Arc::new(Shared::new(RING_FRAMES));
        shared.playing.store(true, Ordering::Release);
        shared.prerolled.store(true, Ordering::Release);
        shared.ring.push(&[0.25, -0.5]);
        shared.aud_ring.push(&[0.1, 0.2]);
        shared.seg_seq.store(1, Ordering::Release); // publisher suspended midway
        let worker_error = shared.worker_error.lock().unwrap();
        let device_error = shared.device_error.lock().unwrap();
        let callback_shared = Arc::clone(&shared);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let callback = std::thread::spawn(move || {
            let mut out = [0.0f32; 128];
            fill(&mut out, 2, &callback_shared, &mut (0.0, 0.0));
            tx.send(out).unwrap();
        });
        let outcome = rx.recv_timeout(Duration::from_secs(2));
        // Release before join even on failure, so a regression cannot hang the suite.
        drop(device_error);
        drop(worker_error);
        callback.join().unwrap();
        let out = outcome.expect("callback waited for a worker lock or position publisher");
        assert!((out[0] - 0.35).abs() < 1e-6);
        assert!((out[1] + 0.3).abs() < 1e-6);
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(
            out[2] > 0.0 && out[126] < out[2],
            "underrun glides to silence"
        );
        assert_eq!(shared.callback_frames.load(Ordering::Relaxed), 64);
        assert_eq!(shared.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(shared.position.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn preroll_holds_transport_and_position_but_audition_stays_immediate() {
        let shared = Shared::new(RING_FRAMES);
        shared.playing.store(true, Ordering::Release);
        shared.prerolled.store(false, Ordering::Release);
        shared.publish_segment(7, 0, 123);
        shared.ring.push(&[0.25, -0.5, 0.5, 0.25]);
        shared.aud_ring.push(&[0.3, -0.4]);
        let mut out = [0.0f32; 4];
        let mut last = (0.0, 0.0);
        fill(&mut out, 2, &shared, &mut last);
        assert_eq!(out, [0.3, -0.4, 0.0, 0.0]);
        assert_eq!(shared.ring.available(), 4);
        assert_eq!(shared.heard_frame(7), Some(123));
        assert_eq!(shared.underruns.load(Ordering::Relaxed), 0);
        shared.prerolled.store(true, Ordering::Release);
        fill(&mut out, 2, &shared, &mut last);
        assert_eq!(out, [0.25, -0.5, 0.5, 0.25]);
        assert_eq!(shared.heard_frame(7), Some(125));
        assert_eq!(shared.ring.available(), 0);
        shared.playing.store(false, Ordering::Release);
        fill(&mut out, 2, &shared, &mut last);
        assert_eq!(
            shared.underruns.load(Ordering::Relaxed),
            0,
            "paused silence is not starvation"
        );
    }

    #[test]
    fn callback_formats_channels_clamp_and_consumed_position_are_correct() {
        for channels in [1, 2, 6] {
            let shared = Shared::new(RING_FRAMES);
            shared.prerolled.store(true, Ordering::Release);
            shared.publish_segment(19, 0, 123);
            shared.ring.push(&[2.0, -2.0, 0.5, 0.25]);
            let mut out = vec![f32::NAN; channels * 2];
            fill(&mut out, channels, &shared, &mut (0.0, 0.0));
            assert_eq!(shared.heard_frame(19), Some(125));
            assert_eq!(shared.callback_frames.load(Ordering::Relaxed), 2);
            if channels == 1 {
                assert_eq!(out, [0.0, 0.375]);
            } else {
                assert_eq!(&out[..2], &[1.0, -1.0]);
                assert_eq!(&out[channels..channels + 2], &[0.5, 0.25]);
                assert!(out[2..channels].iter().all(|v| *v == 0.0));
                assert!(out[channels + 2..].iter().all(|v| *v == 0.0));
            }
        }
        macro_rules! format {
            ($ty:ty, $max:expr, $min:expr, $zero:expr) => {{
                let shared = Shared::new(RING_FRAMES);
                shared.prerolled.store(true, Ordering::Release);
                shared.ring.push(&[2.0, -2.0, 0.0, 0.0]);
                let mut out = [<$ty>::default(); 4];
                fill(&mut out, 2, &shared, &mut (0.0, 0.0));
                assert_eq!(out, [$max, $min, $zero, $zero]);
            }};
        }
        format!(i16, i16::MAX, i16::MIN, 0);
        format!(u16, u16::MAX, u16::MIN, 32768);
        format!(i32, i32::MAX, i32::MIN, 0);
        format!(f64, 1.0, -1.0, 0.0);
    }
}
