//! Native-only isolated plugin MIDI voices. No Lua state crosses the audio bridge.
use super::engine::{BLOCK, Sink};
use crate::lua::bridge::Reservation;
use plumspirit_engine::{
    model::TempoMap,
    playback::midi_plan::{Cursor, Prepared, Status as CursorStatus},
};
use rustysynth::{SoundFont, Synthesizer};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

/// Conservative native charge, separate from the compiled event buffer. Native
/// samples are shared with the loaded SoundFont, never decoded or copied here.
pub const SYNTH_BYTES: usize = 32 * 1024 * 1024;
#[derive(Clone)]
pub struct Resources {
    pub tempo: Arc<TempoMap>,
    pub font: Arc<SoundFont>,
    pub sample_rate: u32,
}
impl std::fmt::Debug for Resources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiResources")
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Queued,
    Running,
    Finished,
    Canceled,
    Refused,
}
impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Canceled => "canceled",
            Self::Refused => "refused",
        }
    }
    fn decode(value: u8) -> Self {
        match value {
            1 => Self::Queued,
            2 => Self::Running,
            3 => Self::Finished,
            4 => Self::Canceled,
            5 => Self::Refused,
            _ => Self::Idle,
        }
    }
}
pub struct Life {
    pub alive: Arc<AtomicBool>,
    pub status: Arc<AtomicU8>,
    pub pending: Arc<AtomicU32>,
    pub plugin: Arc<AtomicBool>,
    pub epoch: Arc<AtomicU64>,
    pub version: u64,
}
impl Life {
    fn cancel_pending(&self) {
        if matches!(
            Status::decode(self.status.load(Ordering::Acquire)),
            Status::Queued | Status::Running
        ) {
            self.end(Status::Canceled);
        }
    }
    pub fn active(&self) -> bool {
        self.alive.load(Ordering::Acquire)
            && self.plugin.load(Ordering::Acquire)
            && self.epoch.load(Ordering::Acquire) == self.version
    }
    pub fn end(&self, status: Status) {
        self.pending.store(0, Ordering::Release);
        self.status.store(status as u8, Ordering::Release);
        self.alive.store(false, Ordering::Release);
    }
}
impl Drop for Life {
    fn drop(&mut self) {
        self.pending.store(0, Ordering::Release);
        self.cancel_pending();
    }
}
pub struct PreparedVoice {
    prepared: Prepared,
    synth: Synthesizer,
    resources: Resources,
    _charge: Reservation,
}
impl PreparedVoice {
    /// Runs on the plugin worker, with its native reservation already acquired.
    pub fn new(
        prepared: Prepared,
        resources: Resources,
        charge: Reservation,
    ) -> Result<Self, String> {
        if prepared.sample_rate() != resources.sample_rate {
            return Err("MIDI plan sample rate changed.".into());
        }
        let settings = super::synth::shard_settings(resources.sample_rate, 128, false);
        let synth = Synthesizer::new(&resources.font, &settings)
            .map_err(|e| format!("MIDI voice preparation failed: {e}"))?;
        Ok(Self {
            prepared,
            synth,
            resources,
            _charge: charge,
        })
    }
    pub fn sample_rate(&self) -> u32 {
        self.resources.sample_rate
    }
    pub fn count(&self) -> u32 {
        self.prepared.events().len() as u32
    }
    pub fn matches(&self, tempo: &Arc<TempoMap>, font: &Arc<SoundFont>) -> bool {
        Arc::ptr_eq(&self.resources.tempo, tempo) && Arc::ptr_eq(&self.resources.font, font)
    }
    pub fn start(self, frame: u64, playable_end: u64, life: Life) -> Option<Voice> {
        let first = self.prepared.first_frame();
        let sr = u64::from(self.resources.sample_rate);
        if !life.active() {
            life.end(Status::Canceled);
            return None;
        }
        if first < frame.saturating_add(sr / 4)
            || first > frame.saturating_add(sr * 5)
            || self.prepared.release_end() > playable_end
        {
            life.end(Status::Refused);
            return None;
        }
        let Ok(cursor) = Cursor::new(self.prepared, frame) else {
            life.end(Status::Refused);
            return None;
        };
        life.status.store(Status::Running as u8, Ordering::Release);
        Some(Voice {
            cursor,
            synth: self.synth,
            life,
            _resources: self.resources,
            _charge: self._charge,
        })
    }
}
pub struct Voice {
    cursor: Cursor,
    synth: Synthesizer,
    life: Life,
    _resources: Resources,
    _charge: Reservation,
}
impl Voice {
    pub fn active(&self) -> bool {
        self.life.active()
    }
    pub fn mix(&mut self, frame: u64, wrapped: bool, inter: &mut [f32; BLOCK * 2]) -> bool {
        if wrapped || !self.life.active() {
            self.cursor.cancel(&mut Sink(&mut self.synth));
            self.life.end(Status::Canceled);
            return false;
        }
        let status = self
            .cursor
            .advance(frame, BLOCK as u32, &mut Sink(&mut self.synth));
        self.life
            .pending
            .store(self.cursor.pending() as u32, Ordering::Release);
        let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
        self.synth.render(&mut left, &mut right);
        for i in 0..BLOCK {
            inter[2 * i] += left[i];
            inter[2 * i + 1] += right[i];
        }
        if status != CursorStatus::Active {
            self.life.end(if status == CursorStatus::Finished {
                Status::Finished
            } else {
                Status::Canceled
            });
            return false;
        }
        true
    }
}
impl Drop for Voice {
    fn drop(&mut self) {
        self.life.cancel_pending();
    }
}
pub struct Watch {
    pub owner: Arc<str>,
    pub project: (u64, u64),
    pub alive: Arc<AtomicBool>,
    pub status: Arc<AtomicU8>,
    pub pending: Arc<AtomicU32>,
}
impl Watch {
    pub fn status(&self) -> Status {
        Status::decode(self.status.load(Ordering::Acquire))
    }
    pub fn cancel(&self) {
        self.pending.store(0, Ordering::Release);
        self.status.store(Status::Canceled as u8, Ordering::Release);
        self.alive.store(false, Ordering::Release);
    }
}
#[cfg(test)]
mod tests;
