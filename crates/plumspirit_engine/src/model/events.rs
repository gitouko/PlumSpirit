//! Preserved non-note MIDI events.
//!
//! Everything in a track that is not a paired note (tempo, time/key signatures,
//! controllers, program changes, pitch bend, pressure, SysEx, text, markers, unknown
//! meta events, and unmatched note on/off messages) is kept here verbatim so that
//! import → export never discards data.
//!
//! Variable-length payloads (meta text, SysEx) live in one per-track byte arena to
//! avoid one heap allocation per event. Payload bytes are never re-encoded: text is
//! kept as raw bytes (MIDI text is not guaranteed to be UTF-8) and decoded lossily
//! only for display.

use super::patch::ListPatch;
use std::fmt;
use std::sync::Arc;

/// Location of a payload inside a track's byte arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct ArenaRange {
    pub offset: u32,
    pub len: u32,
}

/// Kind and fixed-size data of a preserved event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RawKind {
    /// A channel voice message kept as-is (controllers, program change, pitch bend,
    /// pressure, and note on/off messages that could not be paired into notes).
    /// Uses 1 data byte for `0xCn`/`0xDn`, otherwise 2.
    Channel { status: u8, data: [u8; 2] },
    /// `FF type len payload`
    Meta { meta_type: u8, payload: ArenaRange },
    /// `F0 len payload` (the payload normally ends with `F7`)
    SysEx { payload: ArenaRange },
    /// `F7 len payload` (escape sequence / SysEx continuation)
    SysExEscape { payload: ArenaRange },
    /// A system common / real-time status byte found inside a track. These are not
    /// valid in Standard MIDI Files, but they are preserved rather than dropped.
    System { status: u8, data: [u8; 2] },
}

/// Number of data bytes that follow a channel voice status byte.
#[inline]
pub fn channel_data_len(status: u8) -> usize {
    match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        _ => 2,
    }
}

/// Number of data bytes that follow a system status byte (`F1..=FE` except `F7`),
/// or `None` for undefined statuses (`F4`, `F5`, `F9`, `FD`).
pub fn system_data_len(status: u8) -> Option<usize> {
    match status {
        0xF1 | 0xF3 => Some(1),
        0xF2 => Some(2),
        0xF6 | 0xF8 | 0xFA | 0xFB | 0xFC | 0xFE => Some(0),
        _ => None,
    }
}

/// A preserved event.
///
/// `anchor` records how many note on/off events at the same tick preceded this event
/// in the source file. The exporter uses it to reproduce the original interleaving of
/// notes and other events at a single tick (for example a program change placed
/// between note-offs and note-ons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RawEvent {
    pub tick: u64,
    pub anchor: u32,
    pub kind: RawKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventStoreError {
    /// The per-track payload arena would exceed 4 GiB.
    ArenaFull,
    /// Events must be appended in non-decreasing tick order.
    Unsorted,
    OutOfMemory,
}

impl fmt::Display for EventStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventStoreError::ArenaFull => {
                write!(f, "a track contains more than 4 GB of text/SysEx data")
            }
            EventStoreError::Unsorted => write!(f, "internal error: events out of order"),
            EventStoreError::OutOfMemory => write!(f, "not enough memory"),
        }
    }
}

impl std::error::Error for EventStoreError {}

/// Ordered list of preserved events for one track plus its payload arena.
///
/// Both are shared copy-on-write (v0.9, `docs/EXPORT_PLAN.md` D4): a clone
/// is two reference counts, so a document copy (an export snapshot) or an
/// export plan holding a content's events copies nothing; the first edit
/// after such a clone copies the list it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventStore {
    events: Arc<Vec<RawEvent>>,
    arena: Arc<Vec<u8>>,
}

/// Well-known meta event types.
pub mod meta {
    pub const TEXT: u8 = 0x01;
    pub const COPYRIGHT: u8 = 0x02;
    pub const TRACK_NAME: u8 = 0x03;
    pub const MARKER: u8 = 0x06;
    pub const CUE_POINT: u8 = 0x07;
    pub const END_OF_TRACK: u8 = 0x2F;
    pub const TEMPO: u8 = 0x51;
    pub const TIME_SIGNATURE: u8 = 0x58;
    pub const KEY_SIGNATURE: u8 = 0x59;
}

impl EventStore {
    pub fn new() -> Self {
        EventStore::default()
    }

    /// B1-0103: the same events at the ticks `map` gives (a non-decreasing
    /// map keeps them sorted; the payload arena is shared, nothing is copied
    /// but the event list). `map` errors stop it.
    pub(crate) fn retimed<E>(
        &self,
        mut map: impl FnMut(u64) -> Result<u64, E>,
    ) -> Result<EventStore, E> {
        let mut events = Vec::with_capacity(self.events.len());
        for e in self.events.iter() {
            events.push(RawEvent {
                tick: map(e.tick)?,
                ..*e
            });
        }
        debug_assert!(events.windows(2).all(|w| w[0].tick <= w[1].tick));
        Ok(EventStore {
            events: Arc::new(events),
            arena: Arc::clone(&self.arena),
        })
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[inline]
    pub fn events(&self) -> &[RawEvent] {
        &self.events
    }

    pub fn last_tick(&self) -> Option<u64> {
        self.events.last().map(|e| e.tick)
    }

    /// Copies `bytes` into the arena.
    pub fn store_payload(&mut self, bytes: &[u8]) -> Result<ArenaRange, EventStoreError> {
        let offset = u32::try_from(self.arena.len()).map_err(|_| EventStoreError::ArenaFull)?;
        let len = u32::try_from(bytes.len()).map_err(|_| EventStoreError::ArenaFull)?;
        offset.checked_add(len).ok_or(EventStoreError::ArenaFull)?;
        let arena = Arc::make_mut(&mut self.arena);
        arena
            .try_reserve(bytes.len())
            .map_err(|_| EventStoreError::OutOfMemory)?;
        arena.extend_from_slice(bytes);
        Ok(ArenaRange { offset, len })
    }

    /// Reserves `len` arena bytes and returns a mutable slice to fill (used by the
    /// streaming importer to read payloads straight into the arena).
    pub fn reserve_payload(
        &mut self,
        len: usize,
    ) -> Result<(ArenaRange, &mut [u8]), EventStoreError> {
        let offset = u32::try_from(self.arena.len()).map_err(|_| EventStoreError::ArenaFull)?;
        let len32 = u32::try_from(len).map_err(|_| EventStoreError::ArenaFull)?;
        offset
            .checked_add(len32)
            .ok_or(EventStoreError::ArenaFull)?;
        let arena = Arc::make_mut(&mut self.arena);
        arena
            .try_reserve(len)
            .map_err(|_| EventStoreError::OutOfMemory)?;
        let start = arena.len();
        arena.resize(start + len, 0);
        Ok((ArenaRange { offset, len: len32 }, &mut arena[start..]))
    }

    /// Drops arena bytes reserved after `range.offset` (used when a payload read fails).
    pub fn truncate_arena(&mut self, len: usize) {
        if len < self.arena.len() {
            Arc::make_mut(&mut self.arena).truncate(len);
        }
    }

    pub fn arena_len(&self) -> usize {
        self.arena.len()
    }

    #[inline]
    pub fn payload(&self, r: ArenaRange) -> &[u8] {
        let s = r.offset as usize;
        &self.arena[s..s + r.len as usize]
    }

    /// Appends an event; ticks must be non-decreasing.
    pub fn push(&mut self, event: RawEvent) -> Result<(), EventStoreError> {
        if let Some(last) = self.events.last()
            && last.tick > event.tick
        {
            return Err(EventStoreError::Unsorted);
        }
        let events = Arc::make_mut(&mut self.events);
        events
            .try_reserve(1)
            .map_err(|_| EventStoreError::OutOfMemory)?;
        events.push(event);
        Ok(())
    }

    /// Inserts events at explicit positions. `items` must be sorted by position; each
    /// position refers to the index in the list *before* any of these insertions.
    /// Used by the importer to put unmatched note-ons back at their original place.
    pub fn insert_at_positions(&mut self, items: &[(usize, RawEvent)]) {
        if items.is_empty() {
            return;
        }
        let old = std::mem::take(&mut self.events);
        let mut out = Vec::with_capacity(old.len() + items.len());
        let mut it = items.iter().peekable();
        for (i, &e) in old.iter().enumerate() {
            while let Some((pos, ev)) = it.peek() {
                if *pos <= i {
                    out.push(*ev);
                    it.next();
                } else {
                    break;
                }
            }
            out.push(e);
        }
        out.extend(it.map(|(_, e)| *e));
        debug_assert!(out.windows(2).all(|w| w[0].tick <= w[1].tick));
        self.events = Arc::new(out);
    }

    /// Full MIDI byte encoding of an event (status + data, or `FF`/`F0`/`F7` framed
    /// with a variable-length length). Appends to `out`.
    pub fn encode_event(&self, kind: &RawKind, out: &mut Vec<u8>) {
        match *kind {
            RawKind::Channel { status, data } => {
                out.push(status);
                out.extend_from_slice(&data[..channel_data_len(status)]);
            }
            RawKind::System { status, data } => {
                out.push(status);
                out.extend_from_slice(&data[..system_data_len(status).unwrap_or(0)]);
            }
            RawKind::Meta { meta_type, payload } => {
                out.push(0xFF);
                out.push(meta_type);
                crate::midi::vlq::write_vlq(u64::from(payload.len), out);
                out.extend_from_slice(self.payload(payload));
            }
            RawKind::SysEx { payload } => {
                out.push(0xF0);
                crate::midi::vlq::write_vlq(u64::from(payload.len), out);
                out.extend_from_slice(self.payload(payload));
            }
            RawKind::SysExEscape { payload } => {
                out.push(0xF7);
                crate::midi::vlq::write_vlq(u64::from(payload.len), out);
                out.extend_from_slice(self.payload(payload));
            }
        }
    }

    /// Raw bytes of the first Track Name meta event, if any.
    pub fn track_name(&self) -> Option<&[u8]> {
        self.events.iter().find_map(|e| match e.kind {
            RawKind::Meta {
                meta_type: meta::TRACK_NAME,
                payload,
            } => Some(self.payload(payload)),
            _ => None,
        })
    }

    /// Iterates time signature events as `(tick, numerator, denominator_power)`.
    /// Malformed time signatures (payload shorter than 2 bytes, numerator 0 or
    /// denominator power > 6) are skipped for display purposes but still preserved.
    pub fn time_signatures(&self) -> impl Iterator<Item = (u64, u8, u8)> + '_ {
        self.events.iter().filter_map(move |e| match e.kind {
            RawKind::Meta {
                meta_type: meta::TIME_SIGNATURE,
                payload,
            } => {
                let p = self.payload(payload);
                if p.len() >= 2 && p[0] > 0 && p[1] <= 6 {
                    Some((e.tick, p[0], p[1]))
                } else {
                    None
                }
            }
            _ => None,
        })
    }

    /// Iterates tempo events as `(tick, microseconds per quarter note)`. Tempo
    /// events with a payload shorter than 3 bytes or a zero tempo are skipped for
    /// playback (they are still preserved for export).
    pub fn tempos(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.events.iter().filter_map(move |e| match e.kind {
            RawKind::Meta {
                meta_type: meta::TEMPO,
                payload,
            } => {
                let p = self.payload(payload);
                let us = if p.len() >= 3 {
                    u32::from_be_bytes([0, p[0], p[1], p[2]])
                } else {
                    0
                };
                (us > 0).then_some((e.tick, us))
            }
            _ => None,
        })
    }

    pub fn heap_bytes(&self) -> usize {
        self.events.capacity() * std::mem::size_of::<RawEvent>() + self.arena.capacity()
    }

    /// Removes the events at `indices` (strictly increasing). Their payload bytes
    /// stay in the arena until the store is rebuilt (they are never written).
    pub(crate) fn remove_sorted_indices(&mut self, indices: &[usize]) {
        if indices.is_empty() {
            return;
        }
        let mut k = 0usize;
        let mut i = 0usize;
        Arc::make_mut(&mut self.events).retain(|_| {
            let drop = k < indices.len() && indices[k] == i;
            if drop {
                k += 1;
            }
            i += 1;
            !drop
        });
    }

    /// Appends `bytes` at arena offset `at` (the arena length an edit was prepared
    /// for). Returns false, changing nothing, when the arena has another length
    /// (the edit is stale) or would exceed 4 GiB. Arena bytes are never removed
    /// while the store lives, so payload ranges recorded by undo stay valid.
    pub(crate) fn append_payload_at(&mut self, at: u32, bytes: &[u8]) -> bool {
        if self.arena.len() != at as usize {
            return false;
        }
        self.store_payload(bytes).is_ok()
    }

    /// Applies (or, `backwards`, reverts) an edit of events. Refuses — changing
    /// nothing — when the store is not in the expected state, an event refers to
    /// payload bytes outside the arena, or the result would not be in tick order.
    pub(crate) fn apply_patch(&mut self, patch: &ListPatch<RawEvent>, backwards: bool) -> bool {
        let arena = self.arena.len();
        let valid = |e: &RawEvent| match e.kind {
            RawKind::Channel { .. } | RawKind::System { .. } => true,
            RawKind::Meta { payload, .. }
            | RawKind::SysEx { payload }
            | RawKind::SysExEscape { payload } => {
                (payload.offset as usize).saturating_add(payload.len as usize) <= arena
            }
        };
        if !patch.items().all(valid) {
            return false;
        }
        let events = Arc::make_mut(&mut self.events);
        let ok = if backwards {
            patch.revert(events)
        } else {
            patch.apply(events)
        };
        if !ok {
            return false;
        }
        let placed = if backwards {
            patch.removed()
        } else {
            patch.inserted()
        };
        let sorted = placed.iter().all(|(i, _)| {
            let t = events[*i].tick;
            (*i == 0 || events[*i - 1].tick <= t) && events.get(*i + 1).is_none_or(|n| t <= n.tick)
        });
        if !sorted {
            // Undo what was just done so the store is unchanged.
            let _ = if backwards {
                patch.apply(events)
            } else {
                patch.revert(events)
            };
            return false;
        }
        true
    }

    /// Moves every event to `f(tick)`; `f` must be monotonic (non-decreasing) so
    /// the order stays valid (tick conversion when importing into a track).
    pub(crate) fn map_ticks(&mut self, mut f: impl FnMut(u64) -> u64) {
        let events = Arc::make_mut(&mut self.events);
        for e in events.iter_mut() {
            e.tick = f(e.tick);
        }
        debug_assert!(events.windows(2).all(|w| w[0].tick <= w[1].tick));
    }

    pub fn shrink_to_fit(&mut self) {
        // Shared lists are left as they are (shrinking would copy them).
        if let Some(e) = Arc::get_mut(&mut self.events) {
            e.shrink_to_fit();
        }
        if let Some(a) = Arc::get_mut(&mut self.arena) {
            a.shrink_to_fit();
        }
    }

    /// True when both stores share their event list and arena (nothing
    /// changed in between; v0.9 export plans).
    pub fn same_as(&self, other: &EventStore) -> bool {
        Arc::ptr_eq(&self.events, &other.events) && Arc::ptr_eq(&self.arena, &other.arena)
    }
}

/// Decodes MIDI text bytes for display: UTF-8 when valid, otherwise Latin-1.
/// The original bytes are never modified.
pub fn display_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_roundtrip_and_encoding() {
        let mut s = EventStore::new();
        let r = s.store_payload(b"Lead \xff").unwrap();
        s.push(RawEvent {
            tick: 0,
            anchor: 0,
            kind: RawKind::Meta {
                meta_type: meta::TRACK_NAME,
                payload: r,
            },
        })
        .unwrap();
        assert_eq!(s.track_name(), Some(&b"Lead \xff"[..]));
        assert_eq!(display_text(s.track_name().unwrap()), "Lead \u{ff}");
        let mut out = Vec::new();
        s.encode_event(&s.events()[0].kind, &mut out);
        assert_eq!(out, b"\xff\x03\x06Lead \xff");
        assert!(
            s.push(RawEvent {
                tick: 0,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xC0,
                    data: [5, 0]
                }
            })
            .is_ok()
        );
        out.clear();
        s.encode_event(&s.events()[1].kind, &mut out);
        assert_eq!(out, vec![0xC0, 5]);
    }

    #[test]
    fn insert_positions() {
        let mut s = EventStore::new();
        for t in [0u64, 10, 20] {
            s.push(RawEvent {
                tick: t,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xB0,
                    data: [7, t as u8],
                },
            })
            .unwrap();
        }
        let ins = |t: u64| RawEvent {
            tick: t,
            anchor: 0,
            kind: RawKind::Channel {
                status: 0x90,
                data: [60, 1],
            },
        };
        s.insert_at_positions(&[(0, ins(0)), (2, ins(15)), (3, ins(30))]);
        let ticks: Vec<u64> = s.events().iter().map(|e| e.tick).collect();
        assert_eq!(ticks, vec![0, 0, 10, 15, 20, 30]);
    }
}
