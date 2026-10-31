//! A track's notes as its clips play them (v0.9, `docs/EXPORT_PLAN.md` D2).
//!
//! A track that is not simply its own identity clip plays the notes of its
//! unmuted placements: for each, the notes of its content whose start lies in
//! the placement's content window `[lo, hi)`, moved by `shift` (song tick −
//! content tick). [`PlacedNotes`] merges them by song start without copying
//! a note: each [`PlacedLane`] is a [`NoteSnapshot`] (shared chunks) plus its
//! window and shift, read in place.
//!
//! **Order.** Notes come out by song start; at one start, lanes in their
//! placement order (primary first, then by start, then id — the order of
//! [`PlumDocument::placed_contents`](super::PlumDocument::placed_contents)),
//! and within a lane in stored order. That is exactly a stable sort by start
//! of the lanes' notes concatenated in placement order, so what export writes
//! does not depend on how the notes are merged.
//!
//! **Events.** [`PlacedEvents`] does the same for events: the track's own
//! events other than channel messages (name, text, SysEx …, where they are)
//! and each placement's channel events in its window, moved — by tick; at
//! one tick the track's own events first, then the placements in order.

use super::events::{EventStore, RawEvent, RawKind};
use super::note::NoteRecord;
use super::note_store::{NoteSnapshot, SnapshotPos};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// One placement's notes and channel events as the song plays them.
#[derive(Clone, Debug)]
pub struct PlacedLane {
    /// The content's notes (shared chunks).
    pub snap: NoteSnapshot,
    /// Content ticks whose note starts (and channel events) the placement
    /// plays.
    pub lo: u64,
    pub hi: u64,
    /// Song tick − content tick.
    pub shift: i128,
    /// The content's events (shared) and the indices `[ev_lo, ev_hi)` of
    /// those in the window; only its channel messages are played.
    pub events: EventStore,
    pub ev_lo: usize,
    pub ev_hi: usize,
}

impl PartialEq for PlacedLane {
    fn eq(&self, other: &Self) -> bool {
        self.lo == other.lo
            && self.hi == other.hi
            && self.shift == other.shift
            && self.snap.same_as(&other.snap)
            && self.events.same_as(&other.events)
            && self.ev_lo == other.ev_lo
            && self.ev_hi == other.ev_hi
    }
}

impl PlacedLane {
    /// The lane of a placed content.
    pub fn of(pc: &super::PlacedContent<'_>) -> PlacedLane {
        let (lo, hi) = pc.window();
        PlacedLane::new(pc.notes.snapshot(), pc.events.clone(), lo, hi, pc.shift())
    }

    /// A lane playing the notes and channel events of `snap` / `events` in
    /// content ticks `[lo, hi)`, moved by `shift`.
    pub fn new(snap: NoteSnapshot, events: EventStore, lo: u64, hi: u64, shift: i128) -> Self {
        let ev = events.events();
        let ev_lo = ev.partition_point(|e| e.tick < lo);
        let ev_hi = ev_lo + ev[ev_lo..].partition_point(|e| e.tick < hi);
        PlacedLane {
            snap,
            lo,
            hi,
            shift,
            events,
            ev_lo,
            ev_hi,
        }
    }

    /// The channel events the lane plays (content ticks).
    fn channel_events(&self) -> impl DoubleEndedIterator<Item = &RawEvent> {
        self.events.events()[self.ev_lo..self.ev_hi]
            .iter()
            .filter(|e| is_channel(e))
    }

    /// Song tick of content tick `c`. Inside the window it is exact (a window
    /// never starts before the placement's offset, so the result is never
    /// negative); it saturates at `u64::MAX`.
    #[inline]
    pub fn to_song(&self, c: u64) -> u64 {
        (i128::from(c) + self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }

    /// The content window limited to song ticks `[from, to)`.
    fn content_range(&self, from: u64, to: u64) -> (u64, u64) {
        let c = |song: u64| (i128::from(song) - self.shift).clamp(0, i128::from(u64::MAX)) as u64;
        let lo = self.lo.max(c(from));
        let hi = if to == u64::MAX {
            self.hi
        } else {
            self.hi.min(c(to))
        };
        (lo, hi.max(lo))
    }

    /// Notes the lane plays (muted ones included).
    pub fn len(&self) -> usize {
        self.snap.count_in(self.lo, self.hi)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Latest song tick a note of the lane ends at (muted ones included).
    pub fn max_end(&self) -> Option<u64> {
        let e = if self.lo == 0 && self.hi == u64::MAX {
            self.snap.max_end()
        } else {
            self.snap.max_end_in(self.lo, self.hi)
        }?;
        Some(self.to_song(e))
    }

    /// (MIDI channel, subchannel) combinations of the lane's notes, sorted.
    pub fn channel_sub_pairs(&self) -> Vec<(u8, u32)> {
        self.snap.channel_sub_pairs_in(self.lo, self.hi)
    }
}

#[inline]
fn is_channel(e: &RawEvent) -> bool {
    matches!(e.kind, RawKind::Channel { .. })
}

/// Number of events a track with these lanes plays: its own events other
/// than channel messages (`own`) and the lanes' channel events.
pub fn placed_event_count(own: &[RawEvent], lanes: &[PlacedLane]) -> usize {
    own.iter().filter(|e| !is_channel(e)).count()
        + lanes
            .iter()
            .map(|l| l.channel_events().count())
            .sum::<usize>()
}

/// Latest tick of an event a track with these lanes plays.
pub fn placed_last_event_tick(own: &[RawEvent], lanes: &[PlacedLane]) -> Option<u64> {
    let own = own.iter().rev().find(|e| !is_channel(e)).map(|e| e.tick);
    let placed = lanes
        .iter()
        .filter_map(|l| l.channel_events().next_back().map(|e| l.to_song(e.tick)))
        .max();
    own.max(placed)
}

/// Number of notes the lanes play (muted ones included).
pub fn placed_len(lanes: &[PlacedLane]) -> usize {
    lanes.iter().map(PlacedLane::len).sum()
}

/// Latest song tick a note of the lanes ends at.
pub fn placed_max_end(lanes: &[PlacedLane]) -> Option<u64> {
    lanes.iter().filter_map(PlacedLane::max_end).max()
}

/// (MIDI channel, subchannel) combinations of the lanes' notes, sorted and
/// unique.
pub fn placed_channel_sub_pairs(lanes: &[PlacedLane]) -> Vec<(u8, u32)> {
    let mut v: Vec<(u8, u32)> = lanes
        .iter()
        .flat_map(PlacedLane::channel_sub_pairs)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// One lane's notes in `[pos, end)`.
struct Cursor<'a> {
    lane: &'a PlacedLane,
    pos: SnapshotPos,
    end: SnapshotPos,
}

impl Cursor<'_> {
    /// The next note in song ticks.
    #[inline]
    fn pull(&mut self) -> Option<NoteRecord> {
        if self.pos == self.end {
            return None;
        }
        let (mut r, next) = self.lane.snap.next_at(self.pos)?;
        self.pos = next;
        r.start_tick = self.lane.to_song(r.start_tick);
        Some(r)
    }
}

/// The notes of some lanes merged by song start (see the module docs), muted
/// ones included. Memory: one cursor per lane and a heap of at most one
/// entry per lane.
pub struct PlacedNotes<'a> {
    cursors: Vec<Cursor<'a>>,
    /// The next note of each cursor.
    heads: Vec<Option<NoteRecord>>,
    /// (song start, lane) of every cursor with a head; unused with one lane.
    heap: BinaryHeap<Reverse<(u64, usize)>>,
}

impl<'a> PlacedNotes<'a> {
    /// Every note of `lanes`.
    pub fn new(lanes: &'a [PlacedLane]) -> Self {
        PlacedNotes::range(lanes, 0, u64::MAX)
    }

    /// The notes of `lanes` whose song start lies in `[from, to)` (`to =
    /// u64::MAX`: no upper bound).
    pub fn range(lanes: &'a [PlacedLane], from: u64, to: u64) -> Self {
        let mut cursors: Vec<Cursor<'a>> = lanes
            .iter()
            .map(|lane| {
                let (lo, hi) = lane.content_range(from, to);
                let (pos, end) = lane.snap.window(lo, hi);
                Cursor { lane, pos, end }
            })
            .collect();
        let heads: Vec<Option<NoteRecord>> = cursors.iter_mut().map(Cursor::pull).collect();
        let heap = if cursors.len() > 1 {
            heads
                .iter()
                .enumerate()
                .filter_map(|(i, h)| h.map(|r| Reverse((r.start_tick, i))))
                .collect()
        } else {
            BinaryHeap::new()
        };
        PlacedNotes {
            cursors,
            heads,
            heap,
        }
    }
}

impl Iterator for PlacedNotes<'_> {
    type Item = NoteRecord;

    #[inline]
    fn next(&mut self) -> Option<NoteRecord> {
        if self.cursors.len() == 1 {
            let r = self.heads[0].take()?;
            self.heads[0] = self.cursors[0].pull();
            return Some(r);
        }
        let Reverse((_, i)) = self.heap.pop()?;
        let r = self.heads[i].take();
        self.heads[i] = self.cursors[i].pull();
        if let Some(n) = self.heads[i] {
            self.heap.push(Reverse((n.start_tick, i)));
        }
        r
    }
}

/// The events a track with clips plays (see the module docs), as `(song
/// tick, event)`: the event's own tick is its content tick. Nothing is
/// copied; memory: one index and at most one heap entry per lane.
pub struct PlacedEvents<'a> {
    own: std::iter::Peekable<std::slice::Iter<'a, RawEvent>>,
    lanes: &'a [PlacedLane],
    /// Next index of each lane's events (`ev_hi` when done).
    next: Vec<usize>,
    /// (song tick, lane) of each lane's next channel event.
    heap: BinaryHeap<Reverse<(u64, usize)>>,
}

impl<'a> PlacedEvents<'a> {
    /// `own`: the track's own events (its channel messages are skipped; the
    /// placements bring the channel events).
    pub fn new(own: &'a [RawEvent], lanes: &'a [PlacedLane]) -> Self {
        let mut it = PlacedEvents {
            own: own.iter().peekable(),
            lanes,
            next: lanes.iter().map(|l| l.ev_lo).collect(),
            heap: BinaryHeap::with_capacity(lanes.len()),
        };
        for i in 0..lanes.len() {
            it.queue(i);
        }
        it
    }

    /// Moves lane `i` to its next channel event and queues it.
    #[inline]
    fn queue(&mut self, i: usize) {
        let l = &self.lanes[i];
        let ev = l.events.events();
        let mut k = self.next[i];
        while k < l.ev_hi && !is_channel(&ev[k]) {
            k += 1;
        }
        self.next[i] = k;
        if k < l.ev_hi {
            self.heap.push(Reverse((l.to_song(ev[k].tick), i)));
        }
    }
}

impl<'a> Iterator for PlacedEvents<'a> {
    type Item = (u64, &'a RawEvent);

    #[inline]
    fn next(&mut self) -> Option<(u64, &'a RawEvent)> {
        while self.own.next_if(|e| is_channel(e)).is_some() {}
        let own = self.own.peek().map(|e| e.tick);
        let lane = self.heap.peek().map(|Reverse((t, _))| *t);
        match (own, lane) {
            (Some(o), Some(l)) if o <= l => {}
            (Some(_), None) => {}
            (_, Some(_)) => {
                let Reverse((tick, i)) = self.heap.pop().expect("peeked");
                let lanes = self.lanes;
                let e = &lanes[i].events.events()[self.next[i]];
                self.next[i] += 1;
                self.queue(i);
                return Some((tick, e));
            }
            (None, None) => return None,
        }
        self.own.next().map(|e| (e.tick, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::{NoteAttrs, NoteOffKind};
    use crate::model::note_store::NoteStore;

    fn store(starts: &[u64], pitch: u8) -> NoteStore {
        let recs: Vec<NoteRecord> = starts
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                NoteRecord::new(
                    s,
                    10 + i as u64,
                    NoteAttrs::new(pitch + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        NoteStore::from_sorted_records(&recs, false).unwrap()
    }

    /// The reference: every lane's window notes, shifted, concatenated in
    /// lane order, stable-sorted by start.
    fn reference(lanes: &[PlacedLane]) -> Vec<NoteRecord> {
        let mut v = Vec::new();
        for l in lanes {
            let (mut pos, end) = l.snap.window(l.lo, l.hi);
            while pos != end {
                let (mut r, next) = l.snap.next_at(pos).unwrap();
                pos = next;
                r.start_tick = l.to_song(r.start_tick);
                v.push(r);
            }
        }
        v.sort_by_key(|r| r.start_tick);
        v
    }

    #[test]
    fn the_merge_equals_a_stable_sort_of_the_lanes_in_placement_order() {
        let a = store(&[0, 0, 5, 10, 10, 20, 40], 40);
        let b = store(&[0, 3, 10, 10, 30], 60);
        let none = EventStore::new();
        let lanes = vec![
            PlacedLane::new(a.snapshot(), none.clone(), 0, u64::MAX, 0),
            PlacedLane::new(b.snapshot(), none.clone(), 3, 30, 7),
            PlacedLane::new(a.snapshot(), none, 5, 21, 5),
        ];
        let got: Vec<NoteRecord> = PlacedNotes::new(&lanes).collect();
        assert_eq!(got, reference(&lanes));
        assert_eq!(got.len(), placed_len(&lanes));
        assert_eq!(
            placed_max_end(&lanes),
            reference(&lanes)
                .iter()
                .map(|r| r.start_tick + r.duration_ticks)
                .max()
        );
        // A song range is the matching slice of the merge.
        for (from, to) in [(0, 10), (10, 15), (15, u64::MAX), (12, 12), (0, u64::MAX)] {
            let part: Vec<NoteRecord> = PlacedNotes::range(&lanes, from, to).collect();
            let want: Vec<NoteRecord> = reference(&lanes)
                .into_iter()
                .filter(|r| r.start_tick >= from && r.start_tick < to)
                .collect();
            assert_eq!(part, want, "[{from}, {to})");
        }
        // One lane: the fast path.
        let one = &lanes[1..2];
        assert_eq!(PlacedNotes::new(one).collect::<Vec<_>>(), reference(one));
        assert_eq!(placed_channel_sub_pairs(&lanes), vec![(0, 0)]);
        assert!(PlacedNotes::new(&[]).next().is_none());
    }

    #[test]
    fn window_counts_ends_and_pairs_match_a_scan_across_chunk_boundaries() {
        // Several chunks: the boundary-chunk scans and whole-chunk maxima.
        let starts: Vec<u64> = (0..20_000u64).map(|k| k / 3 * 2).collect();
        let s = {
            let recs: Vec<NoteRecord> = starts
                .iter()
                .enumerate()
                .map(|(i, &t)| {
                    NoteRecord::new(
                        t,
                        (i as u64 * 7919) % 500,
                        NoteAttrs::new(
                            (i % 100) as u8,
                            100,
                            (i % 3) as u8,
                            0,
                            NoteOffKind::NoteOff,
                        )
                        .unwrap(),
                    )
                })
                .collect();
            NoteStore::from_sorted_records(&recs, false).unwrap()
        };
        let snap = s.snapshot();
        for (lo, hi) in [
            (0, u64::MAX),
            (1, 2),
            (100, 9_000),
            (5_000, 5_001),
            (13_332, 13_334),
            (9, 9),
        ] {
            let notes: Vec<_> = s
                .iter()
                .filter(|n| n.start_tick >= lo && n.start_tick < hi)
                .collect();
            assert_eq!(snap.count_in(lo, hi), notes.len(), "[{lo}, {hi})");
            assert_eq!(
                snap.max_end_in(lo, hi),
                notes.iter().map(|n| n.end_tick()).max()
            );
            let mut pairs: Vec<(u8, u32)> =
                notes.iter().map(|n| (n.attrs.channel(), n.sub.0)).collect();
            pairs.sort_unstable();
            pairs.dedup();
            assert_eq!(snap.channel_sub_pairs_in(lo, hi), pairs);
        }
    }

    fn cc(tick: u64, value: u8) -> RawEvent {
        RawEvent {
            tick,
            anchor: 0,
            kind: RawKind::Channel {
                status: 0xB0,
                data: [7, value],
            },
        }
    }

    #[test]
    fn placed_events_merge_own_events_first_then_the_lanes_in_order() {
        let mut own = EventStore::new();
        let name = own.store_payload(b"Lead").unwrap();
        let text = RawKind::Meta {
            meta_type: crate::model::events::meta::TEXT,
            payload: name,
        };
        for e in [
            RawEvent {
                tick: 0,
                anchor: 0,
                kind: text,
            },
            cc(0, 1),
            cc(10, 2),
            RawEvent {
                tick: 10,
                anchor: 0,
                kind: text,
            },
            RawEvent {
                tick: 99,
                anchor: 0,
                kind: text,
            },
        ] {
            own.push(e).unwrap();
        }
        let mut content = EventStore::new();
        for (t, v) in [(0, 10), (5, 11), (10, 12), (20, 13), (30, 14)] {
            content.push(cc(t, v)).unwrap();
        }
        let notes = NoteStore::new().snapshot();
        let lanes = vec![
            PlacedLane::new(notes.clone(), own.clone(), 0, u64::MAX, 0),
            PlacedLane::new(notes.clone(), content.clone(), 5, 30, 5),
            PlacedLane::new(notes, content, 0, 11, 0),
        ];
        let got: Vec<(u64, u8)> = PlacedEvents::new(own.events(), &lanes)
            .map(|(t, e)| match e.kind {
                RawKind::Channel { data, .. } => (t, data[1]),
                _ => (t, 0),
            })
            .collect();
        // Reference: own non-channel events, then each lane's channel events
        // in its window, moved, concatenated in lane order; a stable sort by
        // tick with the own events first.
        assert_eq!(
            got,
            vec![
                (0, 0),
                (0, 1),
                (0, 10),
                (5, 11),
                (10, 0),
                (10, 2),
                (10, 11),
                (10, 12),
                (15, 12),
                (25, 13),
                (99, 0)
            ]
        );
        assert_eq!(placed_event_count(own.events(), &lanes), got.len());
        assert_eq!(placed_last_event_tick(own.events(), &lanes), Some(99));
        assert_eq!(placed_last_event_tick(&[], &lanes[1..]), Some(25));
    }
}
