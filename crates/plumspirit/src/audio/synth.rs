//! The synthesis back end of the worker (v0.8 contract §131 – §140).
//!
//! * [`SynthBank`] — more voices than one RustySynth synthesizer allows:
//!   RustySynth 1.3.6 accepts 8 – 256 voices per synthesizer (verified in
//!   `SynthesizerSettings::check_maximum_polyphony`), so a larger Maximum
//!   Voices runs several synthesizers ("shards"). Every note goes to the shard
//!   with the fewest sounding notes (ties: the key's latest shard, then the
//!   first), and the bank remembers which shards hold each (channel, key), so
//!   a Note-Off reaches every one of them and releases the key exactly as one
//!   synthesizer would; the load is balanced whatever keys a song uses and
//!   however often it repeats one, and the assignment depends only on the
//!   event order (deterministic renders). v1.0-beta2 (B2-S33): before, every
//!   repeat of a sounding key stayed on that key's shard, which overfilled one
//!   shard while others had spare voices (docs/PERFORMANCE.md "Synthesizer
//!   voice ladder"). The drum channel keeps that rule (a sounding key's repeats
//!   stay on its shard), because RustySynth's exclusive classes (an open hi-hat
//!   cut by the next one) act within one synthesizer. Channel messages (program,
//!   controllers, pitch bend) go to every shard. The shards' outputs are summed (reverb and chorus are
//!   linear, so the sum equals one synthesizer with all voices).
//! * [`MasterBus`] — floating-point headroom after everything is mixed
//!   (synthesizer, audition, reference audio): Master Output Gain, an
//!   optional transparent safety limiter (instant attack on the stereo-linked
//!   peak, 150 ms release, threshold −1 dBFS, no look-ahead) and meters before
//!   and after it; samples still beyond full scale are counted (clipping is
//!   never hidden). The device callback keeps its final clamp only as the last
//!   safety net.

use super::engine::BLOCK;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

/// RustySynth's largest polyphony per synthesizer (verified, see module docs).
/// The highest key a synthesizer or MIDI output receives (MIDI 1.0 keys
/// 0–127). v0.9 256-Key Mode keys above it are never sent: the scheduler
/// skips them and every sink ignores them (never `key & 127`).
pub const MIDI_TOP_KEY: u8 = plumspirit_engine::model::note::MAX_PITCH;

pub const VOICES_PER_SHARD: usize = 256;
/// The drum channel (MIDI channel 10), whose sounding keys keep their shard.
const DRUM_CHANNEL: u8 = plumspirit_engine::playback::EventProcessing::DRUM_CHANNEL;
const NO_HELD_KEY: u16 = u16::MAX;
// A key's shards are a 16-bit mask.
const _: () = assert!(MAX_VOICES / VOICES_PER_SHARD <= 16);
/// Bounds of the Maximum Voices setting.
pub const MIN_VOICES: usize = 64;
pub const MAX_VOICES: usize = 4_096;
/// Bounds of the audition voice reserve.
pub const MIN_AUDITION_VOICES: usize = 16;
pub const MAX_AUDITION_VOICES: usize = 256;
/// Bounds of the note-on budget per 64-frame block.
pub const MIN_NOTE_ONS: usize = 256;
pub const MAX_NOTE_ONS: usize = 16_384;
/// Master Output Gain bounds (dB).
pub const MIN_GAIN_DB: f32 = -24.0;
pub const MAX_GAIN_DB: f32 = 12.0;
/// Limiter threshold: −1 dBFS.
pub const LIMITER_THRESHOLD: f32 = 0.891_250_9;
/// Limiter release time constant (seconds).
const LIMITER_RELEASE_S: f32 = 0.15;
/// RustySynth's own default output level (Master Volume 100 % maps to it).
pub const SYNTH_MASTER: f32 = 0.5;

/// Admission policy at a shard's held-note bound. Actual voices, release
/// tails and sustain remain managed by RustySynth; it exposes no voice count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoicePolicy {
    #[default]
    Quietest,
    KeepSounding,
    Oldest,
}

impl VoicePolicy {
    pub const ALL: [Self; 3] = [Self::Quietest, Self::KeepSounding, Self::Oldest];

    pub fn key(self) -> &'static str {
        match self {
            Self::Quietest => "quietest",
            Self::KeepSounding => "keep_sounding",
            Self::Oldest => "oldest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Quietest => "RustySynth (quietest voice first)",
            Self::KeepSounding => "Keep sounding notes",
            Self::Oldest => "Release oldest held key first",
        }
    }
}

/// Synthesis settings (validated; application settings, never in `.lup`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SynthConfig {
    /// Voices of the transport (shards of up to 256).
    pub max_voices: usize,
    pub voice_policy: VoicePolicy,
    /// Voices of the note-audition synthesizer.
    pub audition_voices: usize,
    /// SoundFont reverb and chorus (RustySynth `enable_reverb_and_chorus`).
    pub effects: bool,
    /// Master Output Gain in dB.
    pub gain_db: f32,
    pub limiter: bool,
    /// Note-ons per 64-frame block before the rest of a burst is deferred.
    pub note_ons_per_block: usize,
    /// v1.0-beta2 (B2-F12): playback-only event processing (internal
    /// synthesizer and external outputs; never the project or export).
    pub processing: plumspirit_engine::playback::EventProcessing,
    /// v1.0-Launch (L-B03): when an external MIDI output cannot keep up,
    /// keep every event and let playback wait for it (true), or keep time
    /// and drop new note-ons, counted and reported (false, Realtime's
    /// default). Pre-Render always keeps every event.
    pub external_keep_all: bool,
}

impl Default for SynthConfig {
    fn default() -> Self {
        SynthConfig {
            max_voices: 512,
            voice_policy: VoicePolicy::default(),
            audition_voices: 64,
            effects: true,
            gain_db: -3.0,
            limiter: true,
            note_ons_per_block: plumspirit_engine::playback::DEFAULT_NOTE_ONS_PER_BLOCK,
            processing: plumspirit_engine::playback::EventProcessing::default(),
            external_keep_all: false,
        }
    }
}

impl SynthConfig {
    /// The same settings brought into their bounds (absurd input never
    /// allocates: at most 4,096 voices = 16 shards).
    pub fn clamped(mut self) -> SynthConfig {
        self.max_voices = self.max_voices.clamp(MIN_VOICES, MAX_VOICES);
        self.audition_voices = self
            .audition_voices
            .clamp(MIN_AUDITION_VOICES, MAX_AUDITION_VOICES);
        self.note_ons_per_block = self.note_ons_per_block.clamp(MIN_NOTE_ONS, MAX_NOTE_ONS);
        // An invalid velocity band (the settings refuse one) processes nothing.
        if let Some((lo, hi)) = self.processing.velocity_band
            && plumspirit_engine::playback::EventProcessing::band_problem(lo, hi).is_some()
        {
            self.processing.velocity_band = None;
        }
        self.gain_db = if self.gain_db.is_finite() {
            self.gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        self
    }

    /// (shards, voices per shard) for [`Self::max_voices`].
    pub fn shards(&self) -> (usize, usize) {
        let v = self.max_voices.clamp(MIN_VOICES, MAX_VOICES);
        let n = v.div_ceil(VOICES_PER_SHARD);
        (n, (v / n).clamp(8, VOICES_PER_SHARD))
    }

    /// The bank must be rebuilt (voices or effects changed).
    pub fn needs_rebuild(&self, other: &SynthConfig) -> bool {
        self.max_voices != other.max_voices
            || self.effects != other.effects
            || self.audition_voices != other.audition_voices
    }
}

/// RustySynth settings of one shard.
pub fn shard_settings(sample_rate: u32, voices: usize, effects: bool) -> SynthesizerSettings {
    let mut s = SynthesizerSettings::new(sample_rate as i32);
    s.maximum_polyphony = voices.clamp(8, VOICES_PER_SHARD);
    s.enable_reverb_and_chorus = effects;
    s
}

/// Several synthesizers playing as one (see module docs).
pub struct SynthBank {
    groups: Vec<RenderGroup>,
    group_size: usize,
    helpers: Vec<RenderHelper>,
    voices_per_shard: usize,
    /// Sounding notes per (channel, key) and shard as the scheduler sent them
    /// (note-on minus note-off), at `slot * shards + shard`, and per shard —
    /// the voice demand (notes; a layered preset uses more than one voice per
    /// note, so this is a lower bound).
    held: Vec<u32>,
    /// Shards holding each (channel, key): bit `s` for shard `s` (at most 16).
    key_mask: Vec<u16>,
    /// Shard of each (channel, key)'s latest note.
    key_shard: Vec<u8>,
    per_shard: Vec<u32>,
    pub voice_policy: VoicePolicy,
    /// Intrusive first-strike order per shard: previous / next key for each
    /// held slot, head / tail per shard. No per-note allocations or scans.
    held_links: Vec<[u16; 2]>,
    held_ends: Vec<[u16; 2]>,
    /// Note-ons at the chosen shard's held-note bound (not actual steals).
    pub at_capacity: u64,
    pub not_started: u64,
    /// Held instances released by Oldest (one Note-Off releases a whole key).
    pub released_oldest: u64,
    /// Shards whose demand exceeded their voices since the last read.
    pub overflow_blocks: u64,
    /// Test-only: apply every event at once on the calling thread (the
    /// dispatch before B2-S33 part 3), for measurements in one process.
    #[cfg(test)]
    pub dispatch_now: bool,
}

/// A shard's state and scratch travel together; nothing is shared while rendering.
struct RenderShard {
    synth: Synthesizer,
    left: [f32; BLOCK],
    right: [f32; BLOCK],
}

/// A synthesizer event for the shards of one group, applied in that group's
/// render lane before it renders (v1.0-beta2 B2-S33: same-block dispatch in
/// the lanes; each shard still receives its events in their order, before the
/// same render, so the output is the same as applying them at once).
#[derive(Clone, Copy, Debug)]
enum ShardEvent {
    /// `shard` is the index within the group.
    NoteOn {
        shard: u8,
        channel: u8,
        key: u8,
        velocity: u8,
    },
    NoteOff {
        shard: u8,
        channel: u8,
        key: u8,
    },
    /// Every shard of the group.
    Channel {
        status: u8,
        d0: u8,
        d1: u8,
    },
}

/// Events queued for a group before it renders (preallocated; drained in place).
const EVENTS_PER_GROUP: usize = 4_096;

/// The shards a lane renders and the events they receive first.
#[derive(Default)]
struct RenderGroup {
    shards: Vec<RenderShard>,
    events: Vec<ShardEvent>,
}

impl RenderGroup {
    fn apply_events(&mut self) {
        for e in self.events.drain(..) {
            match e {
                ShardEvent::NoteOn {
                    shard,
                    channel,
                    key,
                    velocity,
                } => self.shards[usize::from(shard)].synth.note_on(
                    i32::from(channel),
                    i32::from(key),
                    i32::from(velocity),
                ),
                ShardEvent::NoteOff {
                    shard,
                    channel,
                    key,
                } => self.shards[usize::from(shard)]
                    .synth
                    .note_off(i32::from(channel), i32::from(key)),
                ShardEvent::Channel { status, d0, d1 } => {
                    for s in &mut self.shards {
                        s.synth.process_midi_message(
                            i32::from(status & 0x0F),
                            i32::from(status & 0xF0),
                            i32::from(d0),
                            i32::from(d1),
                        );
                    }
                }
            }
        }
    }
}

fn render_group(group: &mut RenderGroup) {
    group.apply_events();
    for shard in &mut group.shards {
        shard.synth.render(&mut shard.left, &mut shard.right);
    }
}

/// One lane per shard, up to the CPUs and four lanes; a one-shard bank stays
/// serial and the synth worker participates, with at most three helper
/// threads. v1.0-beta2 (B2-S33) measured on 2 vCPU: a 256-voice shard renders
/// in ~110 µs and the handoff costs ~15 – 25 µs a block, so two lanes render
/// 2- and 4-shard banks ~40 % faster at p50 (v0.9 used one lane per four
/// shards); docs/PERFORMANCE.md "B2-S33 keep / revert record".
pub(crate) fn render_lanes(shards: usize, cpus: usize) -> usize {
    shards.clamp(1, 4).min(cpus.max(1))
}

struct RenderHelper {
    jobs: SyncSender<Option<RenderGroup>>,
    completed: Receiver<RenderGroup>,
    thread: Option<JoinHandle<()>>,
}

impl RenderHelper {
    fn start(index: usize) -> std::io::Result<Self> {
        let (jobs, receive) = sync_channel::<Option<RenderGroup>>(1);
        let (complete, completed) = sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!("plumspirit-shards-{index}"))
            .spawn(move || {
                while let Ok(Some(mut group)) = receive.recv() {
                    render_group(&mut group);
                    if complete.send(group).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs,
            completed,
            thread: Some(thread),
        })
    }

    fn stop(&mut self) -> thread::Result<()> {
        if let Some(thread) = self.thread.take() {
            let _ = self.jobs.send(None);
            thread.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for RenderHelper {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl SynthBank {
    pub fn new(
        font: &Arc<SoundFont>,
        sample_rate: u32,
        cfg: &SynthConfig,
    ) -> Result<SynthBank, String> {
        Self::with_parallelism(
            font,
            sample_rate,
            cfg,
            thread::available_parallelism().map_or(1, usize::from),
        )
    }

    fn with_parallelism(
        font: &Arc<SoundFont>,
        sample_rate: u32,
        cfg: &SynthConfig,
        cpus: usize,
    ) -> Result<SynthBank, String> {
        Self::with_helpers(
            font,
            sample_rate,
            cfg,
            |n| render_lanes(n, cpus),
            RenderHelper::start,
        )
    }

    /// A bank rendering on exactly `lanes` lanes (at most one per shard):
    /// measurements comparing lane counts in one process.
    #[cfg(test)]
    pub fn with_lanes(
        font: &Arc<SoundFont>,
        sample_rate: u32,
        cfg: &SynthConfig,
        lanes: usize,
    ) -> Result<SynthBank, String> {
        Self::with_helpers(
            font,
            sample_rate,
            cfg,
            |n| lanes.clamp(1, n),
            RenderHelper::start,
        )
    }

    fn with_helpers(
        font: &Arc<SoundFont>,
        sample_rate: u32,
        cfg: &SynthConfig,
        lanes_of: impl FnOnce(usize) -> usize,
        mut start: impl FnMut(usize) -> std::io::Result<RenderHelper>,
    ) -> Result<SynthBank, String> {
        let (n, per) = cfg.shards();
        let mut shards = Vec::with_capacity(n);
        for _ in 0..n {
            let mut s = Synthesizer::new(font, &shard_settings(sample_rate, per, cfg.effects))
                .map_err(|e| format!("The synthesizer could not start: {e}"))?;
            s.set_master_volume(SYNTH_MASTER);
            shards.push(RenderShard {
                synth: s,
                left: [0.0; BLOCK],
                right: [0.0; BLOCK],
            });
        }
        let mut helpers = Vec::new();
        for index in 1..lanes_of(n) {
            match start(index) {
                Ok(helper) => helpers.push(helper),
                // Thread resources unavailable: the exact serial path remains valid.
                Err(_) => {
                    helpers.clear();
                    break;
                }
            }
        }
        let group_size = n.div_ceil(helpers.len() + 1);
        let mut shards = shards.into_iter();
        let groups = (0..=helpers.len())
            .map(|_| RenderGroup {
                shards: shards.by_ref().take(group_size).collect(),
                events: Vec::with_capacity(EVENTS_PER_GROUP),
            })
            .collect();
        Ok(SynthBank {
            groups,
            group_size,
            helpers,
            voices_per_shard: per,
            held: vec![0; 16 * 128 * n],
            key_mask: vec![0; 16 * 128],
            key_shard: vec![0; 16 * 128],
            per_shard: vec![0; n],
            voice_policy: cfg.voice_policy,
            held_links: vec![[NO_HELD_KEY; 2]; 16 * 128 * n],
            held_ends: vec![[NO_HELD_KEY; 2]; n],
            at_capacity: 0,
            not_started: 0,
            released_oldest: 0,
            overflow_blocks: 0,
            #[cfg(test)]
            dispatch_now: false,
        })
    }

    pub fn capacity(&self) -> usize {
        self.per_shard.len() * self.voices_per_shard
    }

    #[cfg(test)]
    pub fn render_threads(&self) -> usize {
        self.helpers.len() + 1
    }

    /// Notes held per shard (the voice demand the bank balances on).
    #[cfg(test)]
    pub fn shard_demand(&self) -> &[u32] {
        &self.per_shard
    }

    /// Queues `event` for the group of shard `index` (`shard` = its place there).
    fn queue(&mut self, index: usize, event: impl FnOnce(u8) -> ShardEvent) {
        let group = &mut self.groups[index / self.group_size];
        group.events.push(event((index % self.group_size) as u8));
        #[cfg(test)]
        if self.dispatch_now {
            group.apply_events();
        }
    }

    /// Applies every queued event now (before an operation that acts on the
    /// synthesizers directly), keeping each shard's order.
    fn apply_queued(&mut self) {
        for g in &mut self.groups {
            g.apply_events();
        }
    }

    fn shards(&mut self) -> impl Iterator<Item = &mut Synthesizer> {
        self.apply_queued();
        self.groups
            .iter_mut()
            .flat_map(|g| g.shards.iter_mut())
            .map(|s| &mut s.synth)
    }

    fn slot(channel: u8, key: u8) -> usize {
        usize::from(channel & 15) * 128 + usize::from(key & 127)
    }

    /// The least loaded shard (ties: the first).
    fn least_loaded(&self) -> usize {
        let mut best = 0;
        for (i, &n) in self.per_shard.iter().enumerate() {
            if n < self.per_shard[best] {
                best = i;
            }
        }
        best
    }

    pub fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        // v0.9: MIDI keys stop at 127; a 256-Key Mode key above it stays
        // silent (never folded onto key & 127).
        if key > MIDI_TOP_KEY {
            return;
        }
        if velocity == 0 {
            // RustySynth plays a zero-velocity Note-On as a Note-Off.
            self.note_off(channel, key);
            return;
        }
        let k = Self::slot(channel, key);
        let latest = usize::from(self.key_shard[k]);
        let s = if self.key_mask[k] != 0 && channel & 15 == DRUM_CHANNEL {
            latest
        } else {
            let least = self.least_loaded();
            if self.key_mask[k] != 0 && self.per_shard[latest] == self.per_shard[least] {
                latest
            } else {
                least
            }
        };
        if self.per_shard[s] as usize >= self.voices_per_shard {
            self.at_capacity += 1;
            match self.voice_policy {
                VoicePolicy::Quietest => {}
                VoicePolicy::KeepSounding => {
                    self.not_started += 1;
                    return;
                }
                VoicePolicy::Oldest => {
                    let n = self.per_shard.len();
                    let oldest = usize::from(self.held_ends[s][0]);
                    self.queue(s, |shard| ShardEvent::NoteOff {
                        shard,
                        channel: (oldest / 128) as u8,
                        key: (oldest % 128) as u8,
                    });
                    self.released_oldest += u64::from(self.held[oldest * n + s]);
                    self.release_shard(oldest, s);
                }
            }
        }
        self.key_shard[k] = s as u8;
        self.queue(s, |shard| ShardEvent::NoteOn {
            shard,
            channel,
            key,
            velocity,
        });
        let slot = k * self.per_shard.len() + s;
        if self.held[slot] == 0 {
            let tail = self.held_ends[s][1];
            self.held_links[slot] = [tail, NO_HELD_KEY];
            if tail == NO_HELD_KEY {
                self.held_ends[s][0] = k as u16;
            } else {
                self.held_links[usize::from(tail) * self.per_shard.len() + s][1] = k as u16;
            }
            self.held_ends[s][1] = k as u16;
        }
        let h = &mut self.held[slot];
        *h = h.saturating_add(1);
        self.key_mask[k] |= 1 << s;
        self.per_shard[s] += 1;
    }

    pub fn note_off(&mut self, channel: u8, key: u8) {
        if key > MIDI_TOP_KEY {
            return;
        }
        let k = Self::slot(channel, key);
        let mask = self.key_mask[k];
        if mask == 0 {
            // Nothing held (released, sustained or never struck): the key's
            // latest shard gets the Note-Off, as one synthesizer would.
            let s = usize::from(self.key_shard[k]);
            self.queue(s, |shard| ShardEvent::NoteOff {
                shard,
                channel,
                key,
            });
            return;
        }
        for s in 0..self.per_shard.len() {
            if mask & (1 << s) != 0 {
                self.queue(s, |shard| ShardEvent::NoteOff {
                    shard,
                    channel,
                    key,
                });
            }
        }
        self.release_count(k);
    }

    /// RustySynth releases every voice of a key on a Note-Off: the key's held
    /// counts go to zero on every shard holding it.
    fn release_count(&mut self, k: usize) {
        let mut mask = self.key_mask[k];
        while mask != 0 {
            let s = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            self.release_shard(k, s);
        }
    }

    fn release_shard(&mut self, k: usize, s: usize) {
        let n = self.per_shard.len();
        let [prev, next] = self.held_links[k * n + s];
        if prev == NO_HELD_KEY {
            self.held_ends[s][0] = next;
        } else {
            self.held_links[usize::from(prev) * n + s][1] = next;
        }
        if next == NO_HELD_KEY {
            self.held_ends[s][1] = prev;
        } else {
            self.held_links[usize::from(next) * n + s][0] = prev;
        }
        let h = &mut self.held[k * n + s];
        self.per_shard[s] = self.per_shard[s].saturating_sub(*h);
        *h = 0;
        self.key_mask[k] &= !(1 << s);
    }

    pub fn channel_message(&mut self, status: u8, d0: u8, d1: u8) {
        for g in 0..self.groups.len() {
            self.queue(g * self.group_size, |_| ShardEvent::Channel {
                status,
                d0,
                d1,
            });
        }
        // All Notes Off / All Sound Off release every key of the channel.
        if status & 0xF0 == 0xB0 && (d0 == 120 || d0 == 123) {
            for key in 0..128u8 {
                self.release_count(Self::slot(status & 15, key));
            }
        }
    }

    pub fn silence(&mut self) {
        for s in self.shards() {
            s.note_off_all(true);
        }
        self.held.fill(0);
        self.key_mask.fill(0);
        self.per_shard.fill(0);
        self.held_ends.fill([NO_HELD_KEY; 2]);
    }

    pub fn reset(&mut self) {
        for s in self.shards() {
            s.reset();
            s.set_master_volume(SYNTH_MASTER);
        }
        self.held.fill(0);
        self.key_mask.fill(0);
        self.per_shard.fill(0);
        self.held_ends.fill([NO_HELD_KEY; 2]);
    }

    pub fn set_master_volume(&mut self, v: f32) {
        for s in self.shards() {
            s.set_master_volume(v);
        }
    }

    /// Renders one block into `out` (interleaved stereo, overwritten).
    pub fn render(&mut self, out: &mut [f32; BLOCK * 2]) {
        // One bounded job per helper, allocated at bank construction. Each
        // group's queued events are applied in its lane before it renders;
        // every group returns before the next event. The device callback never
        // enters this path.
        for (helper, group) in self.helpers.iter().zip(self.groups.iter_mut().skip(1)) {
            helper
                .jobs
                .send(Some(std::mem::take(group)))
                .unwrap_or_else(|_| panic!("synth render helper exited"));
        }
        render_group(&mut self.groups[0]);
        for (helper, group) in self.helpers.iter().zip(self.groups.iter_mut().skip(1)) {
            *group = helper.completed.recv().expect("synth render helper exited");
        }
        out.fill(0.0);
        // Keep the original shard/sample addition order, including effects tails.
        for (i, s) in self.groups.iter().flat_map(|g| &g.shards).enumerate() {
            for f in 0..BLOCK {
                out[2 * f] += s.left[f];
                out[2 * f + 1] += s.right[f];
            }
            if self.per_shard[i] as usize > self.voices_per_shard {
                self.overflow_blocks += 1;
            }
        }
    }
}

/// Output stage after all mixing (see module docs).
pub struct MasterBus {
    gain: f32,
    limiter: bool,
    /// Current limiter gain (1 = no reduction).
    env: f32,
    release: f32,
    /// Peaks since the last [`MasterBus::take_meters`] (after gain; before /
    /// after the limiter).
    pub peak_pre: f32,
    pub peak_post: f32,
    /// Samples still beyond full scale after the limiter (or without it).
    pub clipped: u64,
    /// Frames the limiter reduced.
    pub limited: u64,
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl MasterBus {
    pub fn new(sample_rate: u32, cfg: &SynthConfig) -> MasterBus {
        let mut m = MasterBus {
            gain: 1.0,
            limiter: true,
            env: 1.0,
            release: 1.0 - (-1.0 / (LIMITER_RELEASE_S * sample_rate.max(1) as f32)).exp(),
            peak_pre: 0.0,
            peak_post: 0.0,
            clipped: 0,
            limited: 0,
        };
        m.configure(cfg);
        m
    }

    /// Discard gain history from audio flushed by a seek or playback restart.
    /// Keep the diagnostic counters, just as the bank's counters are kept.
    pub fn reset_envelope(&mut self) {
        self.env = 1.0;
    }

    pub fn configure(&mut self, cfg: &SynthConfig) {
        let c = cfg.clamped();
        self.gain = db_to_gain(c.gain_db);
        self.limiter = c.limiter;
        if !self.limiter {
            self.env = 1.0;
        }
    }

    /// Applies gain and limiter to interleaved stereo `buf` in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            let (mut l, mut r) = (fr[0] * self.gain, fr[1] * self.gain);
            let p = l.abs().max(r.abs());
            self.peak_pre = self.peak_pre.max(p);
            if self.limiter {
                let target = if p > LIMITER_THRESHOLD {
                    LIMITER_THRESHOLD / p
                } else {
                    1.0
                };
                // Instant attack, smooth release.
                let released = self.env + (1.0 - self.env) * self.release;
                self.env = released.min(target);
                if self.env < 0.9999 {
                    self.limited += 1;
                }
                l *= self.env;
                r *= self.env;
            }
            let q = l.abs().max(r.abs());
            self.peak_post = self.peak_post.max(q);
            if l.abs() > 1.0 {
                self.clipped += 1;
            }
            if r.abs() > 1.0 {
                self.clipped += 1;
            }
            fr[0] = l;
            fr[1] = r;
        }
    }

    /// (peak before, peak after the limiter) since the last call.
    pub fn take_meters(&mut self) -> (f32, f32) {
        let m = (self.peak_pre, self.peak_post);
        self.peak_pre = 0.0;
        self.peak_post = 0.0;
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_parallelism_is_bounded_and_one_shard_stays_serial() {
        for shards in 1..=16 {
            for cpus in [0, 1, 2, 3, 8, usize::MAX] {
                let lanes = render_lanes(shards, cpus);
                assert!((1..=4).contains(&lanes));
                assert!(lanes <= cpus.max(1));
                assert!(lanes <= shards, "at most one lane per shard");
            }
        }
        assert_eq!(render_lanes(1, 8), 1);
        assert_eq!(render_lanes(2, 8), 2);
        assert_eq!(render_lanes(4, 2), 2);
        assert_eq!(render_lanes(4, 8), 4);
        assert_eq!(render_lanes(16, 8), 4);
    }

    #[test]
    fn parallel_shards_match_serial_through_controls_tails_and_reset() {
        let Some(font) = super::super::profile::test_soundfont() else {
            eprintln!("No test SoundFont; parallel audio comparison not exercised");
            return;
        };
        for voices in [512, 1024, 2048, 2304, 3000, 4096] {
            for effects in [false, true] {
                let cfg = SynthConfig {
                    max_voices: voices,
                    effects,
                    ..SynthConfig::default()
                };
                let mut serial = SynthBank::with_parallelism(&font, 48_000, &cfg, 1).unwrap();
                let mut parallel = SynthBank::with_parallelism(&font, 48_000, &cfg, 8).unwrap();
                // B2-S33: events applied at once, as before they were queued per lane.
                let mut at_once = SynthBank::with_parallelism(&font, 48_000, &cfg, 8).unwrap();
                at_once.dispatch_now = true;
                assert!(!parallel.helpers.is_empty());
                let allocations: Vec<_> = parallel
                    .groups
                    .iter()
                    .map(|g| {
                        (
                            g.shards.as_ptr(),
                            g.shards.len(),
                            g.events.as_ptr(),
                            g.events.capacity(),
                        )
                    })
                    .collect();
                let mut heard = false;
                let mut heard_tail = false;
                for block in 0..512 {
                    for bank in [&mut serial, &mut parallel, &mut at_once] {
                        match block {
                            0 => {
                                for ch in 0..16 {
                                    bank.channel_message(
                                        0xC0 | ch,
                                        if ch % 2 == 0 { 0 } else { 40 },
                                        0,
                                    );
                                    bank.channel_message(0xB0 | ch, 91, 80);
                                    bank.channel_message(0xB0 | ch, 93, 50);
                                    bank.channel_message(0xB0 | ch, 64, 127);
                                }
                                // Repeated keys and layered presets also exercise capacity pressure.
                                for i in 0..4096 {
                                    bank.note_on((i / 128 % 16) as u8, (i % 128) as u8, 80);
                                }
                                bank.note_on(0, 200, 100); // Extended keys stay silent.
                            }
                            32 => {
                                for ch in 0..16 {
                                    bank.note_off(ch, 60);
                                    bank.note_on(ch, 61, 0);
                                }
                            }
                            64 => {
                                bank.channel_message(0xE1, 0, 80);
                                bank.channel_message(0xB1, 10, 15);
                                bank.channel_message(0xB2, 11, 40);
                                bank.set_master_volume(0.3);
                            }
                            128 => {
                                for ch in 0..16 {
                                    bank.channel_message(0xB0 | ch, 123, 0);
                                }
                            }
                            256 => {
                                for ch in 0..16 {
                                    bank.channel_message(0xB0 | ch, 64, 0);
                                }
                            }
                            400 => bank.silence(),
                            448 => bank.reset(),
                            452 => bank.note_on(0, 60, 100),
                            480 => bank.channel_message(0xB0, 120, 0),
                            _ => {}
                        }
                    }
                    let (mut a, mut b, mut c) =
                        ([0.0; BLOCK * 2], [0.0; BLOCK * 2], [0.0; BLOCK * 2]);
                    serial.render(&mut a);
                    parallel.render(&mut b);
                    at_once.render(&mut c);
                    assert!(b.iter().all(|v| v.is_finite()));
                    assert_eq!(
                        c.map(f32::to_bits),
                        b.map(f32::to_bits),
                        "queued = at once: voices={voices} effects={effects} block={block}"
                    );
                    assert_eq!(
                        a.map(f32::to_bits),
                        b.map(f32::to_bits),
                        "voices={voices} effects={effects} block={block}"
                    );
                    assert_eq!(serial.held, parallel.held);
                    assert_eq!(serial.per_shard, parallel.per_shard);
                    assert_eq!(serial.key_shard, parallel.key_shard);
                    assert_eq!(serial.key_mask, parallel.key_mask);
                    assert_eq!(serial.overflow_blocks, parallel.overflow_blocks);
                    heard |= block < 128 && b.iter().any(|v| *v != 0.0);
                    heard_tail |= (256..400).contains(&block) && b.iter().any(|v| *v != 0.0);
                    if (448..452).contains(&block) {
                        assert!(b.iter().all(|v| *v == 0.0), "reset clears tails");
                    }
                }
                assert!(
                    heard && heard_tail,
                    "notes and release tails must actually sound"
                );
                assert_eq!(
                    allocations,
                    parallel
                        .groups
                        .iter()
                        .map(|g| {
                            (
                                g.shards.as_ptr(),
                                g.shards.len(),
                                g.events.as_ptr(),
                                g.events.capacity(),
                            )
                        })
                        .collect::<Vec<_>>(),
                    "handoffs reuse the same bounded backing storage"
                );
            }
        }
    }

    #[test]
    fn render_helpers_join_on_rebuild_and_spawn_failure_falls_back() {
        let Some(font) = super::super::profile::test_soundfont() else {
            eprintln!("No test SoundFont; helper lifecycle not exercised");
            return;
        };
        let references = Arc::strong_count(&font);
        for voices in [4096, 3000, 2304, 2048] {
            let cfg = SynthConfig {
                max_voices: voices,
                ..SynthConfig::default()
            };
            let mut bank = SynthBank::with_parallelism(&font, 48_000, &cfg, 8).unwrap();
            bank.note_on(0, 60, 100);
            bank.render(&mut [0.0; BLOCK * 2]);
            for helper in &mut bank.helpers {
                helper.stop().expect("helper joined cleanly");
                assert!(helper.thread.is_none());
                helper.stop().unwrap();
            }
            drop(bank);
            assert_eq!(
                Arc::strong_count(&font),
                references,
                "no retained font or shard"
            );
        }
        let cfg = SynthConfig {
            max_voices: 4096,
            ..SynthConfig::default()
        };
        let mut attempted = 0;
        let mut fallback = SynthBank::with_helpers(
            &font,
            48_000,
            &cfg,
            |n| render_lanes(n, 8),
            |i| {
                attempted += 1;
                if i == 2 {
                    Err(std::io::Error::other("injected thread resource failure"))
                } else {
                    RenderHelper::start(i)
                }
            },
        )
        .unwrap();
        assert_eq!(attempted, 2);
        assert!(fallback.helpers.is_empty());
        assert_eq!(fallback.groups.len(), 1);
        assert_eq!(fallback.capacity(), 4096);
        let mut serial = SynthBank::with_parallelism(&font, 48_000, &cfg, 1).unwrap();
        for bank in [&mut serial, &mut fallback] {
            bank.note_on(0, 60, 100);
        }
        for _ in 0..64 {
            let (mut a, mut b) = ([0.0; BLOCK * 2], [0.0; BLOCK * 2]);
            serial.render(&mut a);
            fallback.render(&mut b);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn parallel_transport_rebuilds_and_releases_all_font_references() {
        use super::super::engine::{AudioEngine, Command};
        use plumspirit_engine::playback::PlaybackIndex;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let Some(font) = super::super::profile::test_soundfont() else {
            eprintln!("No test SoundFont; parallel transport lifecycle not exercised");
            return;
        };
        let references = Arc::strong_count(&font);
        let cfg = SynthConfig {
            max_voices: 4096,
            ..SynthConfig::default()
        };
        let e = AudioEngine::headless_with(48_000, cfg);
        let index = Arc::new(PlaybackIndex::build(
            &super::super::profile::bursts(256, 20, 240),
            48_000,
        ));
        assert!(e.send(Command::SetSoundFont(Arc::clone(&font))));
        assert!(e.send(Command::Play {
            index: Arc::clone(&index),
            tick: 0,
            generation: 1,
            looping: None,
            restrike: true
        }));
        e.wait_playing(true);
        let mut last = (0.0, 0.0);
        for (voices, effects) in [(512, false), (2048, true), (4096, false), (3000, true)] {
            let cfg = SynthConfig {
                max_voices: voices,
                effects,
                ..cfg
            };
            assert!(e.send(Command::Configure(cfg)));
            let (shards, per) = cfg.shards();
            let bound = render_lanes(
                shards,
                thread::available_parallelism().map_or(1, usize::from),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while e.shared.voice_capacity.load(Ordering::Relaxed) != (shards * per) as u32
                || !(1..=bound)
                    .contains(&(e.shared.synth_render_threads.load(Ordering::Relaxed) as usize))
            {
                assert!(
                    e.worker_alive() && Instant::now() < deadline,
                    "rebuild stalled"
                );
                thread::sleep(Duration::from_millis(1));
            }
            let threads = e.shared.synth_render_threads.load(Ordering::Relaxed) as usize;
            assert!((1..=bound).contains(&threads));
            let out = e.pump(1024, &mut last);
            assert!(out.iter().all(|s| s.is_finite()));
            assert!(
                out.iter().any(|s| s.abs() > 1e-6),
                "rebuild must keep sounding"
            );
        }
        assert!(e.send(Command::Pause));
        e.wait_playing(false);
        assert!(e.send(Command::SetSoundFont(Arc::clone(&font))));
        assert!(e.send(Command::Play {
            index,
            tick: 0,
            generation: 2,
            looping: None,
            restrike: true
        }));
        e.wait_playing(true);
        assert!(e.pump(1024, &mut last).iter().any(|s| s.abs() > 1e-6));
        drop(e); // joins the synth worker, which drops/joins all render helpers
        assert_eq!(Arc::strong_count(&font), references);
    }

    /// v1.0-beta2 (B2-S33): repeats of a sounding key go to the least-loaded
    /// shard; one Note-Off (or All Notes Off) releases the key on every shard
    /// holding it, and the bank sounds as one synthesizer with those voices
    /// would. The drum channel's repeats keep their shard.
    #[test]
    fn repeats_of_a_key_spread_over_shards_and_release_as_one_synthesizer() {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        let font = Arc::new(SoundFont::new(&mut &bytes[..]).unwrap());
        let cfg = |voices| SynthConfig {
            max_voices: voices,
            effects: false,
            limiter: false,
            ..SynthConfig::default()
        };
        let mut one = SynthBank::with_parallelism(&font, 48_000, &cfg(256), 1).unwrap();
        let mut two = SynthBank::with_parallelism(&font, 48_000, &cfg(512), 1).unwrap();
        for _ in 0..10 {
            one.note_on(0, 60, 100);
            two.note_on(0, 60, 100);
        }
        assert_eq!(two.per_shard, [5, 5], "repeats spread evenly");
        assert_eq!(two.key_mask[SynthBank::slot(0, 60)], 0b11);
        let (mut a, mut b) = ([0.0; BLOCK * 2], [0.0; BLOCK * 2]);
        let mut heard = false;
        for block in 0..400 {
            if block == 100 {
                one.note_off(0, 60);
                two.note_off(0, 60);
                assert_eq!(two.per_shard, [0, 0]);
                assert_eq!(two.key_mask[SynthBank::slot(0, 60)], 0);
            }
            one.render(&mut a);
            two.render(&mut b);
            heard |= block < 100 && b.iter().any(|v| *v != 0.0);
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() <= 1e-5, "block {block}: {x} vs {y}");
            }
            if block > 300 {
                assert!(
                    b.iter().all(|v| *v == 0.0),
                    "block {block}: released on both shards"
                );
            }
        }
        assert!(heard);
        // All Notes Off reaches every shard holding the key.
        for _ in 0..4 {
            two.note_on(3, 64, 90);
        }
        assert_eq!(two.per_shard, [2, 2]);
        two.channel_message(0xB3, 123, 0);
        assert_eq!(two.per_shard, [0, 0]);
        assert_eq!(two.key_mask[SynthBank::slot(3, 64)], 0);
        // Drum channel: a sounding key's repeats stay on its shard.
        for _ in 0..3 {
            two.note_on(9, 42, 100);
        }
        assert_eq!(two.per_shard, [3, 0]);
        two.note_on(9, 46, 100);
        assert_eq!(two.per_shard, [3, 1], "another drum key: least loaded");
        // A zero-velocity Note-On releases like a Note-Off.
        two.note_on(9, 42, 0);
        assert_eq!(two.per_shard, [0, 1]);
    }

    #[test]
    fn voice_policy_bounds_demand_and_releases_only_the_oldest_shards_key() {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        let font = Arc::new(SoundFont::new(&mut &bytes[..]).unwrap());
        let cfg = SynthConfig {
            max_voices: 64,
            effects: false,
            ..SynthConfig::default()
        };
        for policy in [
            VoicePolicy::Quietest,
            VoicePolicy::KeepSounding,
            VoicePolicy::Oldest,
        ] {
            let mut bank = SynthBank::new(
                &font,
                48_000,
                &SynthConfig {
                    voice_policy: policy,
                    ..cfg
                },
            )
            .unwrap();
            for key in 0..64 {
                bank.note_on(0, key, 100);
            }
            bank.note_on(0, 64, 100);
            assert_eq!(bank.at_capacity, 1);
            assert_eq!(
                bank.not_started,
                u64::from(policy == VoicePolicy::KeepSounding)
            );
            assert_eq!(
                bank.released_oldest,
                u64::from(policy == VoicePolicy::Oldest)
            );
            assert_eq!(
                bank.per_shard[0],
                if policy == VoicePolicy::Quietest {
                    65
                } else {
                    64
                }
            );
            assert_eq!(bank.key_mask[0] == 0, policy == VoicePolicy::Oldest);
            let mut out = [0.0; BLOCK * 2];
            bank.render(&mut out);
            assert!(out.iter().all(|v| v.is_finite()));
            bank.note_off(0, 64);
            bank.channel_message(0xB0, 123, 0);
            assert_eq!(bank.per_shard, [0]);
        }
        // A repeated key can occupy two shards. Oldest releases that key only
        // in the chosen shard, and counts every held instance released there.
        let mut bank = SynthBank::new(
            &font,
            48_000,
            &SynthConfig {
                max_voices: 512,
                voice_policy: VoicePolicy::Oldest,
                ..cfg
            },
        )
        .unwrap();
        for _ in 0..512 {
            bank.note_on(0, 60, 100);
        }
        bank.note_on(1, 61, 100);
        assert_eq!(bank.per_shard, [1, 256]);
        assert_eq!(bank.key_mask[60], 2);
        assert_eq!(bank.released_oldest, 256);
        bank.note_off(0, 60);
        assert_eq!(bank.per_shard, [1, 0]);
        bank.silence();
        assert_eq!(bank.per_shard, [0, 0]);
        bank.reset();
        bank.note_on(0, 62, 100);
        assert_eq!(bank.per_shard, [1, 0]);
    }

    #[test]
    fn voice_policy_is_identical_across_lanes_through_sustain_and_resets() {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        let font = Arc::new(SoundFont::new(&mut &bytes[..]).unwrap());
        for policy in VoicePolicy::ALL {
            let cfg = SynthConfig {
                max_voices: 384,
                voice_policy: policy,
                ..SynthConfig::default()
            };
            let mut serial = SynthBank::with_lanes(&font, 48_000, &cfg, 1).unwrap();
            let mut parallel = SynthBank::with_lanes(&font, 48_000, &cfg, 2).unwrap();
            serial.dispatch_now = true;
            for block in 0..200 {
                for bank in [&mut serial, &mut parallel] {
                    match block {
                        0 | 100 => {
                            bank.channel_message(0xB0, 64, 127);
                            for i in 0..600 {
                                bank.note_on((i / 128) as u8, (i % 128) as u8, 100);
                            }
                        }
                        20 => {
                            for key in 0..128 {
                                bank.note_off(0, key);
                            }
                        }
                        30 => bank.channel_message(0xB0, 64, 0),
                        40 => bank.channel_message(0xB1, 123, 0),
                        50 => bank.channel_message(0xB2, 120, 0),
                        60 => bank.silence(),
                        80 => bank.reset(),
                        120 => {
                            bank.channel_message(0xC0, 1, 0);
                            bank.channel_message(0xE0, 0, 96);
                        }
                        140 => bank.note_on(0, 60, 0),
                        160 => bank.reset(),
                        _ => {}
                    }
                }
                let mut a = [0.0; BLOCK * 2];
                let mut b = [0.0; BLOCK * 2];
                serial.render(&mut a);
                parallel.render(&mut b);
                assert_eq!(a, b, "{policy:?}, block {block}");
                assert_eq!(serial.per_shard, parallel.per_shard);
                assert_eq!(
                    (
                        serial.at_capacity,
                        serial.not_started,
                        serial.released_oldest
                    ),
                    (
                        parallel.at_capacity,
                        parallel.not_started,
                        parallel.released_oldest
                    )
                );
            }
        }
    }

    #[test]
    fn collapsing_identical_same_key_strikes_changes_the_sound() {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        let font = Arc::new(SoundFont::new(&mut &bytes[..]).unwrap());
        let cfg = SynthConfig {
            effects: false,
            ..SynthConfig::default()
        };
        let mut original = SynthBank::new(&font, 48_000, &cfg).unwrap();
        let mut collapsed = SynthBank::new(&font, 48_000, &cfg).unwrap();
        original.note_on(0, 69, 100);
        original.note_on(0, 69, 100);
        collapsed.note_on(0, 69, 100);
        let (mut a, mut b) = ([0.0; BLOCK * 2], [0.0; BLOCK * 2]);
        let (mut energy_a, mut energy_b) = (0.0f64, 0.0f64);
        for _ in 0..100 {
            original.render(&mut a);
            collapsed.render(&mut b);
            for (a, b) in a.iter().zip(b) {
                energy_a += f64::from(*a).powi(2);
                energy_b += f64::from(b).powi(2);
            }
        }
        let ratio = (energy_a / energy_b).sqrt();
        println!("Same-key two strikes / collapsed one: RMS ratio {ratio:.6}");
        assert!(
            (ratio - 2.0).abs() < 1e-5,
            "collapsing loses 6 dB even with identical strikes"
        );
    }

    #[test]
    fn voice_policy_counts_dense_repeated_keys_without_a_sixteen_bit_wrap() {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        let font = Arc::new(SoundFont::new(&mut &bytes[..]).unwrap());
        let mut bank = SynthBank::new(
            &font,
            48_000,
            &SynthConfig {
                max_voices: 64,
                ..SynthConfig::default()
            },
        )
        .unwrap();
        for _ in 0..70_000 {
            bank.note_on(0, 60, 100);
        }
        bank.note_off(0, 60);
        assert_eq!(bank.per_shard, [0], "all instances released together");
        bank.voice_policy = VoicePolicy::Oldest;
        for key in 0..65 {
            bank.note_on(0, key, 100);
        }
        assert_eq!(bank.per_shard, [64]);
        assert_eq!(bank.released_oldest, 1);
    }

    #[test]
    fn voice_policy_oldest_follows_first_strike_after_key_removal_and_reuse() {
        let bytes = crate::audio::soundfont::tests::tiny_sf2();
        let font = Arc::new(SoundFont::new(&mut &bytes[..]).unwrap());
        let mut bank = SynthBank::new(
            &font,
            48_000,
            &SynthConfig {
                max_voices: 64,
                voice_policy: VoicePolicy::Oldest,
                ..SynthConfig::default()
            },
        )
        .unwrap();
        bank.note_on(0, 127, 100);
        for key in 0..63 {
            bank.note_on(0, key, 100);
        }
        bank.note_off(0, 10); // Unlink a middle key, then put it at the tail.
        bank.note_on(0, 10, 100);
        bank.note_on(0, 126, 100);
        assert_eq!(bank.key_mask[127], 0, "first strike, regardless of pitch");
        assert_eq!(bank.key_mask[10], 1, "re-struck key stays");
        bank.note_off(0, 126); // Tail removal.
        bank.note_off(0, 0); // Head removal.
        bank.note_on(0, 124, 100);
        bank.note_on(0, 125, 100);
        bank.note_on(0, 126, 100);
        assert_eq!(bank.key_mask[1], 0, "the next oldest key");
        bank.channel_message(0xB0, 120, 0);
        assert_eq!(bank.held_ends, [[NO_HELD_KEY; 2]]);
        bank.note_on(0, 60, 100);
        assert_eq!(bank.held_ends, [[60; 2]]);
    }

    #[test]
    fn settings_are_bounded_and_shards_cover_the_voices() {
        let c = SynthConfig {
            max_voices: 10_000_000,
            voice_policy: VoicePolicy::default(),
            audition_voices: 0,
            effects: true,
            gain_db: f32::NAN,
            limiter: true,
            external_keep_all: false,
            note_ons_per_block: 1,
            processing: plumspirit_engine::playback::EventProcessing::default(),
        }
        .clamped();
        assert_eq!(c.max_voices, MAX_VOICES);
        assert_eq!(c.audition_voices, MIN_AUDITION_VOICES);
        assert_eq!(c.note_ons_per_block, MIN_NOTE_ONS);
        assert_eq!(c.gain_db, 0.0);
        for (v, want) in [
            (64, (1, 64)),
            (256, (1, 256)),
            (384, (2, 192)),
            (512, (2, 256)),
            (4_096, (16, 256)),
        ] {
            let c = SynthConfig {
                max_voices: v,
                ..SynthConfig::default()
            };
            assert_eq!(c.shards(), want, "{v}");
        }
        assert!(SynthConfig::default().needs_rebuild(&SynthConfig {
            max_voices: 1_024,
            ..SynthConfig::default()
        }));
        assert!(!SynthConfig::default().needs_rebuild(&SynthConfig {
            gain_db: -6.0,
            ..SynthConfig::default()
        }));
    }

    #[test]
    fn the_limiter_keeps_peaks_under_full_scale_and_meters_both_sides() {
        let cfg = SynthConfig {
            gain_db: 6.0,
            ..SynthConfig::default()
        };
        let mut m = MasterBus::new(48_000, &cfg);
        // A loud burst (2.0 = +6 dBFS, then +6 dB gain) then quiet signal.
        let mut buf: Vec<f32> = (0..9_600)
            .map(|i| {
                if i < 4_800 {
                    if i % 2 == 0 { 2.0 } else { -2.0 }
                } else {
                    0.1
                }
            })
            .collect();
        m.process(&mut buf);
        assert!(buf.iter().all(|s| s.abs() <= LIMITER_THRESHOLD + 1e-6));
        assert_eq!(m.clipped, 0, "nothing beyond full scale with the limiter");
        assert!(m.limited > 0);
        let (pre, post) = m.take_meters();
        assert!((pre - 2.0 * db_to_gain(6.0)).abs() < 1e-3, "{pre}");
        assert!(post <= LIMITER_THRESHOLD + 1e-6);
        // Quiet material is left alone once the release has recovered.
        let mut quiet = vec![0.1f32; 48_000 * 2];
        m.process(&mut quiet);
        let tail = quiet[quiet.len() - 2];
        assert!((tail - 0.1 * db_to_gain(6.0)).abs() < 1e-3, "{tail}");
        // Without the limiter, overload is counted, never hidden.
        let mut m = MasterBus::new(
            48_000,
            &SynthConfig {
                limiter: false,
                gain_db: 0.0,
                ..SynthConfig::default()
            },
        );
        let mut buf = vec![1.5f32, -1.5, 0.5, 0.5];
        m.process(&mut buf);
        assert_eq!(m.clipped, 2);
        assert_eq!(buf[0], 1.5, "the bus does not clamp (the counter tells)");
    }

    #[test]
    fn default_gain_is_a_safe_attenuation() {
        let c = SynthConfig::default();
        assert!(c.gain_db < 0.0 && c.limiter);
        assert!((db_to_gain(-6.0) - 0.501).abs() < 0.001);
    }
}
