//! Notes of the same channel and key that overlap in one exported chunk
//! (v0.9 V09-S23 step 4, V09-0102, contract §172; the policy and what each
//! reader gets: `docs/EXPORT_CERTIFICATION.md`).
//!
//! A MIDI Note Off names a channel and a key, not a note. When notes of the
//! same channel and key overlap in one track chunk, the reader decides which
//! open note each ending closes: PlumSpirit's import (and the validation
//! after every export) ends the **oldest** open note first; Kiva ends the
//! **newest** (`docs/references/v0_9/KIVA.md`).
//!
//! # The export order (deterministic)
//!
//! * Notes are written by start tick. Endings at a tick come before the
//!   starts at that tick — a note that ends where the next one begins never
//!   overlaps it — and endings at one tick in the order their notes started
//!   (`midi::export` module docs).
//! * At one tick the notes keep the project's order, except that notes of
//!   the same channel and key are written **shortest first**
//!   ([`NoteWriteOrder`]). Simultaneous Note Ons can be written in any order,
//!   and this one lets a reader that ends the oldest note first give each of
//!   them its own length, velocity and ending.
//!
//! With this order such a reader rebuilds every note of a channel and key
//! exactly unless one of them starts after another and ends before it
//! (*nested*): a Standard MIDI file cannot express that for this reader, and
//! no order of the events changes it. A reader that ends the newest note
//! first rebuilds nested notes exactly and pairs *staggered* ones (a note
//! that starts inside another and ends after it) differently. No order
//! serves both rules; the file holds exactly the Note On and Note Off events
//! written, and [`SameKeyOverlaps`] counts the notes readers may pair
//! differently, for the disclosure ([`same_key_disclosure`]).

use super::error::group_digits;
use crate::model::note::NoteAttrs;
use crate::model::note_store::NoteView;

#[inline]
fn key_index(a: NoteAttrs) -> usize {
    (usize::from(a.channel()) << 8) | usize::from(a.pitch())
}

/// Notes of one chunk (or one document track, summed over its chunks) that
/// readers may pair differently: see the module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SameKeyOverlaps {
    /// Notes that start while a note of the same channel and key that
    /// started earlier is still sounding (notes that start together are not
    /// counted: every reader rebuilds the same lengths for them).
    pub starts_inside: u64,
    /// Of those, the notes that also end before such a note ends (nested).
    /// When a chunk has any, a reader that ends the oldest open note first —
    /// PlumSpirit's import — pairs some of its notes differently.
    pub nested: u64,
}

impl SameKeyOverlaps {
    pub fn any(&self) -> bool {
        self.starts_inside > 0
    }

    pub fn add(&mut self, o: SameKeyOverlaps) {
        self.starts_inside += o.starts_inside;
        self.nested += o.nested;
    }
}

/// What the writer remembers per channel and key.
#[derive(Clone, Copy, Default)]
struct KeyState {
    /// Latest end of every note seen.
    all: u64,
    /// The last tick at which several notes started on this channel and key
    /// (set by [`NoteWriteOrder::count_together`]; a lone note leaves it).
    tick: u64,
    /// For `tick`: the latest end of the notes that started before it, the
    /// latest end of its notes seen so far, and whether two of them came
    /// longer first.
    before: u64,
    at_tick: u64,
    out_of_order: bool,
}

/// A [`KeyState`] per channel and key (keys above 127 only reach the
/// experimental 256-key file). A channel's 256 entries are allocated on its
/// first note: a chunk usually uses one or two channels, so a chunk costs
/// kilobytes, not a full 16 × 256 table.
struct KeyTable {
    channels: [Option<Box<[KeyState; 256]>>; 16],
}

impl KeyTable {
    fn new() -> Self {
        KeyTable {
            channels: Default::default(),
        }
    }

    #[inline]
    fn get(&mut self, a: NoteAttrs) -> &mut KeyState {
        let c = self.channels[usize::from(a.channel() & 0x0F)].get_or_insert_with(new_channel);
        &mut c[usize::from(a.pitch())]
    }
}

/// Kept out of the writer's per-note path.
#[cold]
#[inline(never)]
fn new_channel() -> Box<[KeyState; 256]> {
    Box::new([KeyState::default(); 256])
}

/// A chunk's notes (sorted by start) in the order their Note Ons are
/// written, counting [`SameKeyOverlaps`] on the way when asked to
/// ([`NoteWriteOrder::counting`]). O(1) extra work per note: a tick's notes
/// are buffered only when more than one starts there, and only the notes of
/// a channel and key found out of order (longer first) are sorted. Memory: a
/// 10 KB table per channel used, and the largest group of notes that start
/// together.
pub struct NoteWriteOrder<'a> {
    notes: Box<dyn Iterator<Item = NoteView> + 'a>,
    next: Option<NoteView>,
    /// Notes of the current tick (when it has several), in write order;
    /// already counted.
    batch: Vec<NoteView>,
    at: usize,
    keys: KeyTable,
    /// Scratch space of [`NoteWriteOrder::shortest_first`].
    places: Vec<(usize, usize)>,
    group: Vec<NoteView>,
    counting: bool,
    overlaps: SameKeyOverlaps,
}

impl<'a> NoteWriteOrder<'a> {
    pub fn new(mut notes: Box<dyn Iterator<Item = NoteView> + 'a>) -> Self {
        let next = notes.next();
        NoteWriteOrder {
            notes,
            next,
            batch: Vec::new(),
            at: 0,
            keys: KeyTable::new(),
            places: Vec::new(),
            group: Vec::new(),
            counting: false,
            overlaps: SameKeyOverlaps::default(),
        }
    }

    /// Also counts [`SameKeyOverlaps`] (the writer and the snapshot's report
    /// do; a reader of the events, such as the validation, need not). Call
    /// before the first note is taken.
    pub fn counting(mut self) -> Self {
        self.counting = true;
        self
    }

    /// The overlaps among the notes yielded so far (all of them once the
    /// iterator is exhausted); zero unless [`NoteWriteOrder::counting`].
    pub fn overlaps(&self) -> SameKeyOverlaps {
        self.overlaps
    }

    /// Start tick of the next note, without taking it.
    #[inline]
    pub fn peek_start(&self) -> Option<u64> {
        match self.batch.get(self.at) {
            Some(n) => Some(n.start_tick),
            None => self.next.as_ref().map(|n| n.start_tick),
        }
    }

    /// Counts `n`, the only note starting at its tick. Notes arrive by start
    /// tick, so every note seen before started earlier and `all` is all it
    /// needs (`tick` and `before` belong to
    /// [`NoteWriteOrder::count_together`]).
    #[inline]
    fn count(&mut self, n: &NoteView) {
        let s = self.keys.get(n.attrs);
        let end = n.end_tick();
        if n.start_tick < s.all {
            self.overlaps.starts_inside += 1;
            if end < s.all {
                self.overlaps.nested += 1;
            }
        } else {
            s.all = end;
            return;
        }
        if end > s.all {
            s.all = end;
        }
    }

    /// Counts the batch — several notes starting at tick `t` (the count does
    /// not depend on their order). Returns true when two notes of one
    /// channel and key came longer first.
    fn count_together(&mut self, t: u64) -> bool {
        let mut inside = 0u64;
        let mut nested = 0u64;
        let mut out_of_order = false;
        for n in &self.batch {
            let s = self.keys.get(n.attrs);
            if s.tick != t {
                s.tick = t;
                s.before = s.all;
                s.at_tick = 0;
                s.out_of_order = false;
            }
            let end = n.end_tick();
            if t < s.before {
                inside += 1;
                nested += u64::from(end < s.before);
            }
            s.all = s.all.max(end);
            if end < s.at_tick {
                s.out_of_order = true;
                out_of_order = true;
            } else {
                s.at_tick = end;
            }
        }
        // (Without counting, `all` is not kept for lone notes and these
        // counts mean nothing: they are dropped.)
        if self.counting {
            self.overlaps.starts_inside += inside;
            self.overlaps.nested += nested;
        }
        out_of_order
    }

    /// Buffers and counts the notes starting at the tick of `first` and
    /// `second` (the first two), puts them in write order and returns the
    /// first.
    #[inline(never)]
    fn gather(&mut self, first: NoteView, second: NoteView) -> NoteView {
        let t = first.start_tick;
        self.batch.clear();
        self.batch.push(first);
        self.batch.push(second);
        self.next = loop {
            match self.notes.next() {
                Some(m) if m.start_tick == t => self.batch.push(m),
                other => break other,
            }
        };
        if self.count_together(t) {
            self.shortest_first();
        }
        self.at = 1;
        self.batch[0]
    }

    /// Reorders the notes of each channel and key found out of order among
    /// the places they hold in the batch, shortest first (equal lengths keep
    /// their order); every other note stays where it is.
    #[cold]
    fn shortest_first(&mut self) {
        self.places.clear();
        for (i, n) in self.batch.iter().enumerate() {
            let s = self.keys.get(n.attrs);
            if s.out_of_order && s.tick == n.start_tick {
                self.places.push((key_index(n.attrs), i));
            }
        }
        self.places.sort_unstable();
        for run in self.places.chunk_by(|a, b| a.0 == b.0) {
            self.group.clear();
            self.group.extend(run.iter().map(|&(_, i)| self.batch[i]));
            self.group.sort_by_key(|n| n.duration_ticks);
            for (&(_, i), n) in run.iter().zip(&self.group) {
                self.batch[i] = *n;
            }
        }
    }
}

impl Iterator for NoteWriteOrder<'_> {
    type Item = NoteView;

    #[inline]
    fn next(&mut self) -> Option<NoteView> {
        if let Some(&n) = self.batch.get(self.at) {
            self.at += 1;
            return Some(n);
        }
        let n = self.next.take()?;
        match self.notes.next() {
            Some(m) if m.start_tick == n.start_tick => Some(self.gather(n, m)),
            other => {
                self.next = other;
                if self.counting {
                    self.count(&n);
                }
                Some(n)
            }
        }
    }
}

/// "1, 3 and 7" (1-based), at most eight numbers and a count of the rest.
fn track_list(tracks: &[usize]) -> String {
    const SHOWN: usize = 8;
    let names: Vec<String> = tracks
        .iter()
        .take(SHOWN)
        .map(|t| (t + 1).to_string())
        .collect();
    if tracks.len() > SHOWN {
        return format!(
            "{} and {} more",
            names.join(", "),
            group_digits((tracks.len() - SHOWN) as u64)
        );
    }
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.concat(),
    }
}

/// The disclosure for a file whose chunks hold notes that start inside a
/// note of the same channel and key (`None` when none does). `tracks`:
/// document track and its overlaps ([`WriteStats::same_key`]);
/// `reopen_differently`: the document tracks PlumSpirit's import pairs
/// differently (the validation's finding after an export, or the tracks with
/// nested notes before one).
///
/// [`WriteStats::same_key`]: super::export::WriteStats::same_key
pub fn same_key_disclosure(
    tracks: &[(usize, SameKeyOverlaps)],
    reopen_differently: &[usize],
) -> Option<String> {
    let with: Vec<usize> = tracks
        .iter()
        .filter(|(_, o)| o.any())
        .map(|&(t, _)| t)
        .collect();
    if with.is_empty() {
        return None;
    }
    let mut total = SameKeyOverlaps::default();
    for (_, o) in tracks {
        total.add(*o);
    }
    let one_track = with.len() == 1;
    let mut s = format!(
        "{} note(s) in {} {} start while a note of the same key and channel is still sounding. A MIDI file does not say which Note On each Note Off ends, so programs pair such notes by their own rule (PlumSpirit ends the oldest one first, Kiva the newest) and can show different lengths. The file holds exactly the note events PlumSpirit wrote.",
        group_digits(total.starts_inside),
        if one_track { "track" } else { "tracks" },
        track_list(&with),
    );
    if total.nested > 0 {
        s.push_str(&format!(
            " {} of them end before the note they start inside.",
            group_digits(total.nested)
        ));
    }
    if reopen_differently.is_empty() {
        s.push_str(" PlumSpirit reopens every note exactly.");
    } else {
        s.push_str(&format!(
            " When the file is reopened, PlumSpirit pairs some notes of {} {} differently, so their lengths change{}.",
            if reopen_differently.len() == 1 {
                "track"
            } else {
                "tracks"
            },
            track_list(reopen_differently),
            if reopen_differently.len() < with.len() {
                "; the other tracks reopen exactly"
            } else {
                ""
            }
        ));
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::NoteOffKind;
    use crate::model::note::SubchannelId;

    fn note(start: u64, len: u64, key: u8, vel: u8) -> NoteView {
        NoteView {
            start_tick: start,
            duration_ticks: len,
            attrs: NoteAttrs::new(key, vel, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId::default(),
            group: 0,
            selected: false,
        }
    }

    fn run(notes: Vec<NoteView>) -> (Vec<(u64, u64, u8, u8)>, SameKeyOverlaps) {
        let mut o = NoteWriteOrder::new(Box::new(notes.into_iter())).counting();
        let v = o
            .by_ref()
            .map(|n| {
                (
                    n.start_tick,
                    n.duration_ticks,
                    n.attrs.pitch(),
                    n.attrs.velocity(),
                )
            })
            .collect();
        (v, o.overlaps())
    }

    #[test]
    fn notes_that_start_together_on_one_key_are_written_shortest_first_and_nothing_else_moves() {
        let (v, o) = run(vec![
            note(0, 100, 60, 1),
            note(0, 10, 62, 2),
            note(0, 50, 60, 3),
            note(0, 0, 60, 4),
            note(0, 20, 61, 5),
            note(0, 50, 60, 6),
            note(5, 1, 60, 7),
        ]);
        // Key 60 holds places 0, 2, 3 and 5: lengths 0, 50, 50 (stored
        // order kept) and 100; keys 62 and 61 stay where they were.
        assert_eq!(
            v,
            vec![
                (0, 0, 60, 4),
                (0, 10, 62, 2),
                (0, 50, 60, 3),
                (0, 50, 60, 6),
                (0, 20, 61, 5),
                (0, 100, 60, 1),
                (5, 1, 60, 7),
            ]
        );
        // The note at 5 starts inside the 50- and 100-tick notes and ends
        // before them.
        assert_eq!(
            o,
            SameKeyOverlaps {
                starts_inside: 1,
                nested: 1
            }
        );
    }

    #[test]
    fn an_order_that_is_already_right_is_kept_exactly() {
        let notes = vec![
            note(0, 10, 60, 1),
            note(0, 10, 60, 2),
            note(0, 20, 60, 3),
            note(0, 5, 61, 4),
            note(3, 1, 61, 5),
        ];
        let (v, _) = run(notes.clone());
        let expected: Vec<_> = notes
            .iter()
            .map(|n| {
                (
                    n.start_tick,
                    n.duration_ticks,
                    n.attrs.pitch(),
                    n.attrs.velocity(),
                )
            })
            .collect();
        assert_eq!(v, expected);
    }

    #[test]
    fn overlaps_are_counted_by_kind() {
        // Staggered: starts inside, ends after.
        let (_, o) = run(vec![note(0, 100, 60, 1), note(50, 100, 60, 2)]);
        assert_eq!((o.starts_inside, o.nested), (1, 0));
        // Nested.
        let (_, o) = run(vec![note(0, 100, 60, 1), note(50, 10, 60, 2)]);
        assert_eq!((o.starts_inside, o.nested), (1, 1));
        // Ending together counts as starting inside, not nested.
        let (_, o) = run(vec![note(0, 100, 60, 1), note(50, 50, 60, 2)]);
        assert_eq!((o.starts_inside, o.nested), (1, 0));
        // Identical and simultaneous notes: not counted.
        let (_, o) = run(vec![note(0, 100, 60, 1), note(0, 100, 60, 2)]);
        assert!(!o.any());
        let (_, o) = run(vec![note(0, 100, 60, 1), note(0, 30, 60, 2)]);
        assert!(!o.any());
        // Touching notes and other keys: not counted.
        let (_, o) = run(vec![
            note(0, 100, 60, 1),
            note(50, 100, 61, 2),
            note(100, 5, 60, 3),
        ]);
        assert!(!o.any());
        // A zero-length note inside a longer one is nested; at its start or
        // its end it is not.
        let (_, o) = run(vec![note(0, 100, 60, 1), note(40, 0, 60, 2)]);
        assert_eq!((o.starts_inside, o.nested), (1, 1));
        let (_, o) = run(vec![
            note(0, 100, 60, 1),
            note(0, 0, 60, 2),
            note(100, 0, 60, 3),
        ]);
        assert!(!o.any());
        // Other channels are other keys.
        let mut b = note(50, 10, 60, 2);
        b.attrs = NoteAttrs::new(60, 2, 1, 0, NoteOffKind::NoteOff).unwrap();
        let (_, o) = run(vec![note(0, 100, 60, 1), b]);
        assert!(!o.any());
    }

    #[test]
    fn keys_above_127_have_their_own_entries() {
        let w = |start: u64, len: u64, key: u8| NoteView {
            attrs: NoteAttrs::new_wide(key, 9, 15, 0, NoteOffKind::NoteOff).unwrap(),
            ..note(start, len, 0, 1)
        };
        let (v, o) = run(vec![w(0, 9, 255), w(0, 2, 255), w(0, 1, 127), w(1, 1, 255)]);
        assert_eq!(v.iter().map(|n| n.1).collect::<Vec<_>>(), vec![2, 9, 1, 1]);
        assert_eq!((o.starts_inside, o.nested), (1, 1));
    }

    #[test]
    fn the_disclosure_names_the_tracks_and_what_reopens_differently() {
        let o = |i: u64, n: u64| SameKeyOverlaps {
            starts_inside: i,
            nested: n,
        };
        assert_eq!(same_key_disclosure(&[], &[]), None);
        assert_eq!(same_key_disclosure(&[(0, o(0, 0))], &[]), None);
        let s = same_key_disclosure(&[(0, o(3, 0))], &[]).unwrap();
        assert!(s.starts_with("3 note(s) in track 1 start while"), "{s}");
        assert!(s.ends_with("PlumSpirit reopens every note exactly."), "{s}");
        let s = same_key_disclosure(&[(0, o(1_200, 2)), (4, o(1, 0))], &[0]).unwrap();
        assert!(
            s.starts_with("1,201 note(s) in tracks 1 and 5 start"),
            "{s}"
        );
        assert!(s.contains(" 2 of them end before"), "{s}");
        assert!(
            s.ends_with(
                "PlumSpirit pairs some notes of track 1 differently, so their lengths change; the other tracks reopen exactly."
            ),
            "{s}"
        );
        let many: Vec<(usize, SameKeyOverlaps)> = (0..12).map(|t| (t, o(1, 1))).collect();
        let all: Vec<usize> = (0..12).collect();
        let s = same_key_disclosure(&many, &all).unwrap();
        assert!(
            s.contains("tracks 1, 2, 3, 4, 5, 6, 7, 8 and 4 more"),
            "{s}"
        );
        assert!(s.ends_with("so their lengths change."), "{s}");
    }
}
