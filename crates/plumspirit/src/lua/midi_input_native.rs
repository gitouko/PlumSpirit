//! Bounded packet admission only: no Lua, device calls, callback allocation or
//! blocking producer lock. The future WinMM callback supplies packet/time values.
use plumspirit_engine::model::events::channel_data_len;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, TryLockError};

pub const MAX_QUEUE: usize = 4096;
pub const MAX_BATCH: usize = 256;
pub const RATE_PER_SECOND: u64 = 32768;
pub const BURST: u64 = 4096;
const SECOND: u128 = 1_000_000_000;
const WRAP: u64 = 1_u64 << 32;
const HALF: u64 = WRAP / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    NoteOn,
    NoteOff,
    PolyPressure,
    ControlChange,
    ProgramChange,
    ChannelPressure,
    PitchBend,
    Clock,
    Start,
    Continue,
    Stop,
    ActiveSense,
    Reset,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::NoteOn => "note_on",
            Self::NoteOff => "note_off",
            Self::PolyPressure => "poly_pressure",
            Self::ControlChange => "cc",
            Self::ProgramChange => "program",
            Self::ChannelPressure => "channel_pressure",
            Self::PitchBend => "pitch_bend",
            Self::Clock => "clock",
            Self::Start => "start",
            Self::Continue => "continue",
            Self::Stop => "stop",
            Self::ActiveSense => "active_sense",
            Self::Reset => "reset",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
    pub kind: Kind,
}
impl Message {
    /// WinMM expands running status. Unused bytes are unspecified and ignored.
    pub fn decode(word: u32) -> Option<Self> {
        let status = word as u8;
        if (0x80..=0xef).contains(&status) {
            let data1 = (word >> 8) as u8;
            let data2 = if channel_data_len(status) == 2 {
                (word >> 16) as u8
            } else {
                0
            };
            if data1 > 127 || data2 > 127 {
                return None;
            }
            let kind = match status & 0xf0 {
                0x80 => Kind::NoteOff,
                0x90 if data2 == 0 => Kind::NoteOff,
                0x90 => Kind::NoteOn,
                0xa0 => Kind::PolyPressure,
                0xb0 => Kind::ControlChange,
                0xc0 => Kind::ProgramChange,
                0xd0 => Kind::ChannelPressure,
                0xe0 => Kind::PitchBend,
                _ => return None,
            };
            Some(Self {
                status,
                data1,
                data2,
                kind,
            })
        } else {
            let kind = match status {
                0xf8 => Kind::Clock,
                0xfa => Kind::Start,
                0xfb => Kind::Continue,
                0xfc => Kind::Stop,
                0xfe => Kind::ActiveSense,
                0xff => Kind::Reset,
                _ => return None,
            };
            Some(Self {
                status,
                data1: 0,
                data2: 0,
                kind,
            })
        }
    }
    pub fn channel(self) -> Option<u8> {
        ((0x80..=0xef).contains(&self.status)).then_some(self.status & 15)
    }
    pub fn bend(self) -> Option<i16> {
        (self.kind == Kind::PitchBend)
            .then_some((i16::from(self.data2) * 128 + i16::from(self.data1)) - 8192)
    }
    pub fn note_on_zero(self) -> bool {
        self.status & 0xf0 == 0x90 && self.data2 == 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub driver_ms: u32,
    pub timestamp_ms: u64,
    pub received_ns: u64,
    pub out_of_order: bool,
    pub driver_overrun: bool,
    pub message: Message,
}
#[derive(Default)]
struct Clock {
    high: Option<u64>,
}
impl Clock {
    fn extend(&mut self, raw: u32) -> Option<(u64, bool)> {
        let Some(high) = self.high else {
            self.high = Some(u64::from(raw));
            return Some((u64::from(raw), false));
        };
        let mut candidate = (high & !(WRAP - 1)) | u64::from(raw);
        if candidate < high && high - candidate > HALF {
            candidate = candidate.checked_add(WRAP)?;
        } else if candidate > high && candidate - high > HALF && candidate >= WRAP {
            candidate -= WRAP;
        }
        let late = candidate < high;
        self.high = Some(high.max(candidate));
        Some((candidate, late))
    }
}
struct Rate {
    last: u64,
    tokens: u64,
    remainder: u128,
}
impl Default for Rate {
    fn default() -> Self {
        Self {
            last: 0,
            tokens: BURST,
            remainder: 0,
        }
    }
}
impl Rate {
    fn admit(&mut self, now: u64) -> bool {
        if now > self.last {
            let credit = u128::from(now - self.last) * u128::from(RATE_PER_SECOND) + self.remainder;
            let refill = (credit / SECOND).min(u128::from(BURST)) as u64;
            self.tokens = (self.tokens + refill).min(BURST);
            self.remainder = if self.tokens == BURST {
                0
            } else {
                credit % SECOND
            };
            self.last = now;
        }
        if self.tokens == 0 {
            false
        } else {
            self.tokens -= 1;
            true
        }
    }
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open = 0,
    Closed = 1,
    Disconnected = 2,
    Exhausted = 3,
    Poisoned = 4,
}
impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Disconnected => "disconnected",
            Self::Exhausted => "exhausted",
            Self::Poisoned => "failed",
        }
    }
    fn of(n: u8) -> Self {
        match n {
            0 => Self::Open,
            1 => Self::Closed,
            2 => Self::Disconnected,
            3 => Self::Exhausted,
            _ => Self::Poisoned,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    QueueFull,
    RateLimited,
    Contended,
    Invalid,
    Closed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Allocation,
    BatchSize,
    Poisoned,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub seen: u64,
    pub accepted: u64,
    pub queue_dropped: u64,
    pub rate_dropped: u64,
    pub invalid: u64,
    pub contended: u64,
    pub driver_overruns: u64,
    pub clock_regressions: u64,
    pub queued: usize,
    pub capacity: usize,
    pub status: Status,
}
pub struct Batch {
    pub events: Vec<Event>,
    pub stats: Stats,
}
struct State {
    queue: VecDeque<Event>,
    capacity: usize,
    clock: Clock,
    rate: Rate,
    sequence: u64,
    accepted: u64,
    queue_dropped: u64,
    rate_dropped: u64,
    clock_regressions: u64,
}
pub struct Capture {
    state: Mutex<State>,
    status: AtomicU8,
    invalid: AtomicU64,
    contended: AtomicU64,
    driver_overruns: AtomicU64,
}
impl Capture {
    pub fn reservation_bytes(capacity: usize) -> Option<usize> {
        (1..=MAX_QUEUE)
            .contains(&capacity)
            .then_some(capacity)?
            .checked_mul(std::mem::size_of::<Event>() * 2)?
            .checked_add(8192)
    }
    pub fn new(capacity: usize) -> Result<Self, Error> {
        if !(1..=MAX_QUEUE).contains(&capacity) {
            return Err(Error::Capacity);
        }
        let mut queue = VecDeque::new();
        queue
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Allocation)?;
        Ok(Self {
            state: Mutex::new(State {
                queue,
                capacity,
                clock: Clock::default(),
                rate: Rate::default(),
                sequence: 0,
                accepted: 0,
                queue_dropped: 0,
                rate_dropped: 0,
                clock_regressions: 0,
            }),
            status: AtomicU8::new(0),
            invalid: AtomicU64::new(0),
            contended: AtomicU64::new(0),
            driver_overruns: AtomicU64::new(0),
        })
    }
    pub fn status(&self) -> Status {
        Status::of(self.status.load(Ordering::Acquire))
    }
    /// Nonblocking revocation. The worker's next drain discards queued events.
    pub fn close(&self, reason: Status) {
        if reason != Status::Open {
            let _ =
                self.status
                    .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
        }
    }
    fn atomic_count(&self, counter: &AtomicU64) {
        if counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .is_err()
        {
            self.close(Status::Exhausted);
        }
    }
    fn count(&self, counter: &mut u64) -> bool {
        if let Some(n) = counter.checked_add(1) {
            *counter = n;
            true
        } else {
            self.close(Status::Exhausted);
            false
        }
    }
    /// A driver error notification is invalid even if its bytes resemble a message.
    pub fn driver_error(&self) {
        if self.status() == Status::Open {
            self.atomic_count(&self.invalid);
        }
    }
    /// Callback admission never allocates, takes a blocking lock, calls Lua or a device API.
    pub fn ingest(
        &self,
        word: u32,
        driver_ms: u32,
        received_ns: u64,
        driver_overrun: bool,
    ) -> Admission {
        if self.status() != Status::Open {
            return Admission::Closed;
        }
        if driver_overrun {
            self.atomic_count(&self.driver_overruns);
        }
        let Some(message) = Message::decode(word) else {
            self.atomic_count(&self.invalid);
            return Admission::Invalid;
        };
        let mut s = match self.state.try_lock() {
            Ok(s) => s,
            Err(TryLockError::WouldBlock) => {
                self.atomic_count(&self.contended);
                return Admission::Contended;
            }
            Err(TryLockError::Poisoned(_)) => {
                self.close(Status::Poisoned);
                return Admission::Closed;
            }
        };
        if self.status() != Status::Open {
            return Admission::Closed;
        }
        if !self.count(&mut s.sequence) {
            s.queue.clear();
            return Admission::Closed;
        }
        let Some((timestamp_ms, out_of_order)) = s.clock.extend(driver_ms) else {
            self.close(Status::Exhausted);
            s.queue.clear();
            return Admission::Closed;
        };
        if out_of_order && !self.count(&mut s.clock_regressions) {
            s.queue.clear();
            return Admission::Closed;
        }
        if !s.rate.admit(received_ns) {
            self.count(&mut s.rate_dropped);
            return Admission::RateLimited;
        }
        if s.queue.len() == s.capacity {
            self.count(&mut s.queue_dropped);
            return Admission::QueueFull;
        }
        if !self.count(&mut s.accepted) {
            s.queue.clear();
            return Admission::Closed;
        }
        let sequence = s.sequence;
        s.queue.push_back(Event {
            sequence,
            driver_ms,
            timestamp_ms,
            received_ns,
            out_of_order,
            driver_overrun,
            message,
        });
        Admission::Accepted
    }
    fn snapshot(&self, s: &State) -> Stats {
        Stats {
            seen: s.sequence,
            accepted: s.accepted,
            queue_dropped: s.queue_dropped,
            rate_dropped: s.rate_dropped,
            invalid: self.invalid.load(Ordering::Relaxed),
            contended: self.contended.load(Ordering::Relaxed),
            driver_overruns: self.driver_overruns.load(Ordering::Relaxed),
            clock_regressions: s.clock_regressions,
            queued: s.queue.len(),
            capacity: s.capacity,
            status: self.status(),
        }
    }
    /// Worker-only; bounded fallible allocation occurs before the short queue lock.
    pub fn drain(&self, limit: usize) -> Result<Batch, Error> {
        if !(1..=MAX_BATCH).contains(&limit) {
            return Err(Error::BatchSize);
        }
        let mut events = Vec::new();
        events
            .try_reserve_exact(limit)
            .map_err(|_| Error::Allocation)?;
        let mut s = self.state.lock().map_err(|_| {
            self.close(Status::Poisoned);
            Error::Poisoned
        })?;
        if self.status() != Status::Open {
            s.queue.clear();
        }
        while events.len() < limit {
            let Some(event) = s.queue.pop_front() else {
                break;
            };
            events.push(event);
        }
        let mut stats = self.snapshot(&s);
        if stats.status != Status::Open {
            events.clear();
            s.queue.clear();
            stats.queued = 0;
        }
        Ok(Batch { events, stats })
    }
    /// Worker-only bounded snapshot, without allocating an empty drain buffer.
    pub fn statistics(&self) -> Result<Stats, Error> {
        let mut s = self.state.lock().map_err(|_| {
            self.close(Status::Poisoned);
            Error::Poisoned
        })?;
        let mut stats = self.snapshot(&s);
        if stats.status != Status::Open {
            s.queue.clear();
            stats.queued = 0;
        }
        Ok(stats)
    }
}

#[cfg(test)]
#[path = "midi_input_native_tests.rs"]
mod tests;
