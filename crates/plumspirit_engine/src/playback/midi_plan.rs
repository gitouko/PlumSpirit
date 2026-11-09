//! Precomputed channel-voice plans for a dedicated, owned output sink.
//! Does not alter the song scheduler or grant access to an output device.
use super::SynthSink;
use crate::model::{TempoMap, events::channel_data_len};
use std::fmt;

pub const MAX_EVENTS: usize = 262_144;
pub const BLOCK_FRAMES: u32 = 64;
pub const MAX_BLOCK_EVENTS: usize = 64;
pub const MAX_HELD_KEYS: usize = 128;
pub const MAX_SPAN_SECONDS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub tick: u64,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}
impl Event {
    /// Independently checked at the native boundary, even for non-Lua callers.
    pub fn valid(self) -> bool {
        self.tick <= i64::MAX as u64
            && (0x80..=0xEF).contains(&self.status)
            && self.data1 < 128
            && self.data2 < 128
            && (channel_data_len(self.status) == 2 || self.data2 == 0)
            && (self.status & 0xF0 != 0xB0 || self.data1 < 120)
    }
    fn on(self) -> bool {
        self.status & 0xF0 == 0x90 && self.data2 != 0
    }
    fn off(self) -> bool {
        self.status & 0xF0 == 0x80 || self.status & 0xF0 == 0x90 && self.data2 == 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameEvent {
    pub frame: u64,
    pub event: Event,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CompileError<E> {
    Invalid(&'static str),
    Interrupted(E),
    Allocation,
}
impl<E: fmt::Display> fmt::Display for CompileError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) => f.write_str(s),
            Self::Interrupted(e) => e.fmt(f),
            Self::Allocation => f.write_str("MIDI plan allocation failed."),
        }
    }
}
impl<E: fmt::Debug + fmt::Display> std::error::Error for CompileError<E> {}

/// Caller reserves this much native buffer memory before compilation. Both the
/// source buffer and this result coexist; do not release the source charge early.
pub fn compiled_bytes(count: usize) -> Option<usize> {
    (count > 0 && count <= MAX_EVENTS).then(|| count * size_of::<FrameEvent>())
}
#[derive(Debug)]
pub struct Prepared {
    events: Vec<FrameEvent>,
    sample_rate: u32,
    release_end: u64,
}
impl Prepared {
    pub fn events(&self) -> &[FrameEvent] {
        &self.events
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn first_frame(&self) -> u64 {
        self.events[0].frame
    }
    pub fn release_end(&self) -> u64 {
        self.release_end
    }
    /// No sort: equal-tick/equal-frame event order remains authored order.
    /// Validation is completed before allocating the bounded result buffer.
    pub fn compile<E>(
        events: &[Event],
        tempo: &TempoMap,
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<Self, CompileError<E>> {
        let invalid = CompileError::Invalid;
        if compiled_bytes(events.len()).is_none() {
            return Err(invalid(
                "A scheduled plan requires 1 through 262144 events.",
            ));
        }
        let sample_rate = tempo.sample_rate();
        if !(8000..=384000).contains(&sample_rate) {
            return Err(invalid("Unsupported MIDI plan sample rate."));
        }
        let mut held = [false; 16 * 128];
        let mut held_count = 0usize;
        let mut window = [0u64; MAX_BLOCK_EVENTS];
        let mut previous_tick = 0;
        let mut previous_frame = 0;
        let mut first_frame = 0;
        for (i, event) in events.iter().copied().enumerate() {
            if i.is_multiple_of(256) {
                check().map_err(CompileError::Interrupted)?;
            }
            if !event.valid() {
                return Err(invalid("Invalid or unsupported channel-voice MIDI event."));
            }
            if event.tick < previous_tick {
                return Err(invalid("MIDI plan ticks must be nondecreasing."));
            }
            let frame = tempo.frame_at_tick(event.tick);
            if frame == u64::MAX || frame < previous_frame {
                return Err(invalid("MIDI plan frame conversion overflowed."));
            }
            if i == 0 {
                first_frame = frame;
            }
            if frame - first_frame > u64::from(sample_rate) * MAX_SPAN_SECONDS {
                return Err(invalid("MIDI plan span exceeds 60 seconds."));
            }
            if i >= MAX_BLOCK_EVENTS
                && frame - window[i % MAX_BLOCK_EVENTS] < u64::from(BLOCK_FRAMES)
            {
                return Err(invalid(
                    "MIDI plan exceeds 64 events in a sliding 64-frame window.",
                ));
            }
            window[i % MAX_BLOCK_EVENTS] = frame;
            let key = usize::from(event.status & 15) * 128 + usize::from(event.data1);
            if event.on() {
                if held[key] {
                    return Err(invalid(
                        "Overlapping plugin note-ons for the same channel/key are unsupported.",
                    ));
                }
                held[key] = true;
                held_count += 1;
                if held_count > MAX_HELD_KEYS {
                    return Err(invalid("MIDI plan exceeds 128 simultaneously held keys."));
                }
            } else if event.off() {
                if !held[key] {
                    return Err(invalid("Plugin note-off has no matching note-on."));
                }
                held[key] = false;
                held_count -= 1;
            }
            previous_tick = event.tick;
            previous_frame = frame;
        }
        if held_count != 0 {
            return Err(invalid(
                "Every plugin note-on requires a matching note-off.",
            ));
        }
        let release_end = previous_frame
            .checked_add(u64::from(sample_rate / 2))
            .ok_or_else(|| invalid("MIDI plan release tail overflowed."))?;
        check().map_err(CompileError::Interrupted)?;
        let mut compiled = Vec::new();
        compiled
            .try_reserve_exact(events.len())
            .map_err(|_| CompileError::Allocation)?;
        for (i, event) in events.iter().copied().enumerate() {
            if i.is_multiple_of(256) {
                check().map_err(CompileError::Interrupted)?;
            }
            compiled.push(FrameEvent {
                frame: tempo.frame_at_tick(event.tick),
                event,
            });
        }
        check().map_err(CompileError::Interrupted)?;
        Ok(Self {
            events: compiled,
            sample_rate,
            release_end,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    Finished,
    Canceled,
    ClockChanged,
}
/// Only pass a dedicated plugin-owned sink. Its silence operation must never
/// reach song notes or a shared external MIDI channel. No allocation at dispatch.
pub struct Cursor {
    prepared: Prepared,
    next: usize,
    expected_frame: u64,
    status: Status,
}
impl Cursor {
    pub fn new(prepared: Prepared, start_frame: u64) -> Result<Self, Prepared> {
        if start_frame > prepared.first_frame() {
            return Err(prepared);
        }
        Ok(Self {
            prepared,
            next: 0,
            expected_frame: start_frame,
            status: Status::Active,
        })
    }
    pub fn status(&self) -> Status {
        self.status
    }
    pub fn pending(&self) -> usize {
        self.prepared.events.len() - self.next
    }
    pub fn cancel(&mut self, sink: &mut dyn SynthSink) {
        if self.status == Status::Active {
            sink.silence();
            self.status = Status::Canceled;
        }
    }
    pub fn advance(&mut self, frame: u64, frames: u32, sink: &mut dyn SynthSink) -> Status {
        if self.status != Status::Active {
            return self.status;
        }
        let end = frame.checked_add(u64::from(frames));
        if frame != self.expected_frame || frames == 0 || frames > BLOCK_FRAMES || end.is_none() {
            sink.silence();
            self.status = Status::ClockChanged;
            return self.status;
        }
        let end = end.unwrap();
        let timed = sink.timed();
        while let Some(event) = self
            .prepared
            .events
            .get(self.next)
            .filter(|e| e.frame < end)
        {
            if timed {
                sink.at_offset((event.frame - frame) as u32);
            }
            let event = event.event;
            if event.on() {
                sink.note_on(event.status & 15, event.data1, event.data2);
            } else if event.off() {
                sink.note_off(event.status & 15, event.data1);
            } else {
                sink.channel_message(event.status, event.data1, event.data2);
            }
            self.next += 1;
        }
        self.expected_frame = end;
        if end >= self.prepared.release_end {
            if timed {
                sink.at_offset(
                    (self.prepared.release_end.saturating_sub(frame)).min(u64::from(frames)) as u32,
                );
            }
            sink.silence();
            self.status = Status::Finished;
        }
        self.status
    }
}

#[cfg(test)]
mod tests;
