//! The output sink of a kernel: validates every generated note against the
//! boundary policy, enforces the note cap and the cancel flag, and counts what
//! the policy did (shown in the preflight line).

use super::boundary::{Boundary, LenPolicy, TickPolicy};
use super::{TNote, TransformError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What the boundary policy did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutStats {
    pub pitch_changed: u64,
    pub pitch_dropped: u64,
    pub tick_clamped: u64,
    pub tick_dropped: u64,
    pub len_fixed: u64,
    pub len_dropped: u64,
    /// Notes that would end beyond the largest tick (always dropped).
    pub overflow_dropped: u64,
    /// Shift applied by the "Shift all" tick policy.
    pub shifted_by: u64,
    /// Duplicates a shrinking Scale created (removed, v0.9 §086).
    pub collapsed: u64,
    /// Removed by Remove Note Overlaps (v0.9 §231).
    pub overlaps_removed: u64,
    /// Shortened by Remove Note Overlaps (v0.9 §231).
    pub overlaps_trimmed: u64,
}

impl OutStats {
    pub fn dropped(&self) -> u64 {
        self.pitch_dropped
            + self.tick_dropped
            + self.len_dropped
            + self.overflow_dropped
            + self.collapsed
            + self.overlaps_removed
    }
}

/// A controller lane a kernel writes (v0.7 events output: Slide → pitch bend;
/// the controller lanes were written by LFO, removed in v0.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    /// Control Change number 0–119.
    Cc(u8),
    PitchBend,
}

/// One controller point: value 0–127 (CC) or 0–16383 (pitch bend, 8192 centre).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TEvent {
    /// Track slot (as [`TNote::track`]).
    pub track: u16,
    pub lane: Lane,
    /// MIDI channel 0–15.
    pub ch: u8,
    pub tick: u64,
    pub value: u16,
}

/// Track slot meaning "a new subtrack" (see [`Output::new_subtrack_parent`]).
pub const NEW_SUBTRACK: u16 = u16::MAX;

/// Most controller points one run may write.
pub const MAX_EVENTS: usize = 8_000_000;

/// A position in an [`Output`] ([`Output::mark`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mark(usize, usize);

pub struct Output {
    pub notes: Vec<TNote>,
    /// Controller points (sorted and de-duplicated by the run).
    pub events: Vec<TEvent>,
    /// A new note-group table (tools that create / change groups).
    pub groups: Option<crate::model::GroupTable>,
    /// Notes emitted with `track == NEW_SUBTRACK` go to a new subtrack of
    /// this slot's track (Clipping Mask "result to a new subtrack").
    pub new_subtrack_parent: Option<u16>,
    /// A sentence the kernel wants shown with the summary (chord names, …).
    pub message: Option<String>,
    /// v0.9 V09-S39: notes outside the source the kernel removes — read
    /// from [`super::Source::context`] / [`super::Source::aux`], `track` =
    /// their slot (Draw "replace under the stroke"). The commit deletes
    /// them in the same undo step (`history::Operation::EraseRecords`).
    pub erase: Vec<TNote>,
    pub stats: OutStats,
    pub boundary: Boundary,
    limit: usize,
    cancel: Option<Arc<AtomicBool>>,
    exceeded: bool,
    cancelled: bool,
    /// Notes with a negative start, kept for the Shift policy.
    negative: Vec<(i128, TNote)>,
    tick_count: u32,
    /// V09-0143: the tool's driver dimension of each note in `notes` /
    /// `negative` (kept only while [`Output::enable_dims`] is on — an
    /// offset driver reads them; nothing else does).
    dims: Option<(Vec<u32>, Vec<u32>)>,
    /// The dimension of the notes emitted from now on ([`Output::set_dim`]).
    dim: u32,
}

impl Output {
    pub fn new(boundary: Boundary, limit: usize) -> Output {
        Output {
            dims: None,
            dim: 0,
            notes: Vec::new(),
            events: Vec::new(),
            groups: None,
            new_subtrack_parent: None,
            message: None,
            erase: Vec::new(),
            stats: OutStats::default(),
            boundary,
            limit,
            cancel: None,
            exceeded: false,
            cancelled: false,
            negative: Vec::new(),
            tick_count: 0,
        }
    }

    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Output {
        self.cancel = Some(cancel);
        self
    }

    /// Room for `n` notes (fails gracefully instead of aborting).
    pub fn reserve(&mut self, n: usize) -> Result<(), TransformError> {
        let n = n.min(self.limit);
        self.notes.try_reserve(n).map_err(|_| {
            TransformError::TooLarge(format!(
                "Not enough memory for {} notes.",
                crate::midi::group_digits(n as u64)
            ))
        })
    }

    pub fn len(&self) -> usize {
        self.notes.len() + self.negative.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// True once the cap was reached (further notes are ignored).
    pub fn full(&self) -> bool {
        self.exceeded || self.cancelled
    }

    /// Reads the cancel flag now (kernels that iterate long without
    /// emitting — Paint V2's automaton and reaction-diffusion — call it per
    /// generation or step) and returns the cancel / cap error once set.
    pub fn poll(&mut self) -> Result<(), TransformError> {
        if let Some(c) = &self.cancel
            && c.load(Ordering::Relaxed)
        {
            self.cancelled = true;
        }
        self.check()
    }

    /// Where the notes emitted from now on begin (for [`Output::adjust_since`]).
    pub fn mark(&self) -> Mark {
        Mark(self.notes.len(), self.negative.len())
    }

    /// Calls `f` on every note emitted since `mark` with its number (0 = the
    /// first emitted since), the boundary policy and the statistics; a note
    /// `f` returns false for is removed. Notes still waiting for the Shift
    /// policy (negative starts) are visited with `start` 0. The platform's
    /// post-steps (offset drivers) adjust a kernel's output this way.
    pub fn adjust_since(
        &mut self,
        mark: Mark,
        mut f: impl FnMut(usize, &mut TNote, &Boundary, &mut OutStats) -> bool,
    ) {
        let Mark(a, b) = mark;
        let boundary = self.boundary;
        let stats = &mut self.stats;
        let mut i = 0;
        // In place: no allocation (this runs once per note in a lazy preview).
        // The kept dimensions move with their notes (V09-0143).
        let mut dims = self.dims.as_mut();
        let from = a.min(self.notes.len());
        let mut w = from;
        for r in from..self.notes.len() {
            let mut n = self.notes[r];
            if f(i, &mut n, &boundary, stats) {
                self.notes[w] = n;
                if let Some((pos, _)) = dims.as_deref_mut()
                    && let Some(&d) = pos.get(r)
                    && w < pos.len()
                {
                    pos[w] = d;
                }
                w += 1;
            }
            i += 1;
        }
        self.notes.truncate(w);
        let from = b.min(self.negative.len());
        let mut w2 = from;
        for r in from..self.negative.len() {
            let (s, mut n) = self.negative[r];
            let start = n.start;
            n.start = 0;
            let keep = f(i, &mut n, &boundary, stats);
            n.start = start;
            if keep {
                self.negative[w2] = (s, n);
                if let Some((_, neg)) = dims.as_deref_mut()
                    && let Some(&d) = neg.get(r)
                    && w2 < neg.len()
                {
                    neg[w2] = d;
                }
                w2 += 1;
            }
            i += 1;
        }
        self.negative.truncate(w2);
        if let Some((pos, neg)) = dims {
            pos.truncate(w);
            neg.truncate(w2);
        }
    }

    /// v1.0-beta2 B2-F01: Remove Note Overlaps on the notes emitted since
    /// `mark`, in the middle of a run (the user ordered it before an offset
    /// driver): the decisions [`super::overlap::apply`] takes on the
    /// finished result — on the starts the finish will give the notes (the
    /// Shift policy's move included), in canonical order — applied in place
    /// (removed notes go, trimmed ones keep their place), counted in the
    /// statistics.
    pub fn overlaps_since(
        &mut self,
        mark: Mark,
        source: &[TNote],
        protected: &[TNote],
        policy: super::overlap::Policy,
    ) {
        let Mark(a, b) = mark;
        let (a, b) = (a.min(self.notes.len()), b.min(self.negative.len()));
        let shift = self
            .negative
            .iter()
            .map(|(s, _)| *s)
            .min()
            .map_or(0, |m| (-m).clamp(0, i128::from(u64::MAX)) as u64);
        let mut tmp: Vec<(TNote, usize)> =
            Vec::with_capacity(self.notes.len() - a + self.negative.len() - b);
        for (i, n) in self.notes[a..].iter().enumerate() {
            let mut m = *n;
            m.start = n.start.saturating_add(shift);
            tmp.push((m, i));
        }
        let base = self.notes.len() - a;
        for (k, (s, n)) in self.negative[b..].iter().enumerate() {
            let mut m = *n;
            m.start = (s + i128::from(shift)).clamp(0, i128::from(u64::MAX)) as u64;
            tmp.push((m, base + k));
        }
        // Ties broken by emission order: deterministic without a stable sort.
        tmp.sort_unstable_by(|x, y| {
            super::overlap::canonical_order(&x.0, &y.0).then(x.1.cmp(&y.1))
        });
        let mut sorted: Vec<TNote> = tmp.iter().map(|t| t.0).collect();
        let (keep, report) = super::overlap::decide(&mut sorted, source, protected, policy);
        let mut verdict = vec![(true, 0u64); tmp.len()];
        for (k, (_, i)) in tmp.iter().enumerate() {
            verdict[*i] = (keep[k], sorted[k].dur);
        }
        self.adjust_since(mark, |i, n, _, _| {
            let (k, dur) = verdict[i];
            if k {
                n.dur = dur;
            }
            k
        });
        self.stats.overlaps_removed += report.removed;
        self.stats.overlaps_trimmed += report.trimmed;
    }

    /// For the notes emitted since `mark` (in [`Output::adjust_since`]'s
    /// order): each note's place in start order on its track slot (ties by
    /// pitch, then emission order) and the number of notes per slot.
    pub fn places_since(&self, mark: Mark) -> (Vec<usize>, std::collections::HashMap<u16, usize>) {
        let Mark(a, b) = mark;
        let mut keyed: Vec<(u16, i128, u8, usize)> = self.notes[a.min(self.notes.len())..]
            .iter()
            .map(|n| (n.track, i128::from(n.start), n.pitch))
            .chain(
                self.negative[b.min(self.negative.len())..]
                    .iter()
                    .map(|(s, n)| (n.track, *s, n.pitch)),
            )
            .enumerate()
            .map(|(i, (t, s, p))| (t, s, p, i))
            .collect();
        keyed.sort_unstable();
        let mut place = vec![0; keyed.len()];
        let mut counts = std::collections::HashMap::new();
        for (t, _, _, i) in keyed {
            let c = counts.entry(t).or_insert(0usize);
            place[i] = *c;
            *c += 1;
        }
        (place, counts)
    }

    /// Emits a controller point (values clamped to the lane's range). Returns
    /// false once the output is full.
    pub fn emit_event(&mut self, track: u16, lane: Lane, ch: u8, tick: i128, value: i64) -> bool {
        if self.full() {
            return false;
        }
        if self.events.len() >= MAX_EVENTS {
            self.exceeded = true;
            return false;
        }
        if tick < 0 || tick > i128::from(crate::model::MAX_DELTA_TICKS) * 1024 {
            self.stats.tick_dropped += 1;
            return true;
        }
        let max = match lane {
            Lane::Cc(_) => 127,
            Lane::PitchBend => 16_383,
        };
        self.events.push(TEvent {
            track,
            lane,
            ch: ch.min(15),
            tick: tick as u64,
            value: value.clamp(0, max) as u16,
        });
        true
    }

    /// Emits a note computed in wide integers; `tpl` supplies every other field
    /// (subchannel, channel, release, group, track, flags). Returns false once
    /// the output is full or cancelled.
    #[inline]
    pub fn emit(&mut self, start: i128, dur: i128, pitch: i64, vel: i64, tpl: &TNote) -> bool {
        if self.full() {
            return false;
        }
        self.tick_count = self.tick_count.wrapping_add(1);
        if self.tick_count & 0xFFFF == 0
            && let Some(c) = &self.cancel
            && c.load(Ordering::Relaxed)
        {
            self.cancelled = true;
            return false;
        }
        let (p, changed) = self.boundary.pitch(pitch);
        let Some(p) = p else {
            self.stats.pitch_dropped += 1;
            return true;
        };
        if changed {
            self.stats.pitch_changed += 1;
        }
        let mut dur = dur;
        if dur <= 0 {
            match self.boundary.len {
                LenPolicy::MinOne => {
                    self.stats.len_fixed += 1;
                    dur = 1;
                }
                LenPolicy::Discard => {
                    self.stats.len_dropped += 1;
                    return true;
                }
            }
        }
        let mut n = *tpl;
        n.pitch = p;
        n.vel = vel.clamp(1, 127) as u8;
        n.rel = n.rel.min(127);
        n.ch = n.ch.min(15);
        if start < 0 {
            match self.boundary.tick {
                TickPolicy::Clamp => {
                    self.stats.tick_clamped += 1;
                    return self.push_checked(0, dur, n);
                }
                TickPolicy::Discard => {
                    self.stats.tick_dropped += 1;
                    return true;
                }
                TickPolicy::Shift => {
                    if dur > u64::MAX as i128 {
                        self.stats.overflow_dropped += 1;
                        return true;
                    }
                    n.dur = dur as u64;
                    self.negative.push((start, n));
                    if let Some((_, neg)) = &mut self.dims {
                        neg.push(self.dim);
                    }
                    if self.len() > self.limit {
                        self.exceeded = true;
                        return false;
                    }
                    return true;
                }
            }
        }
        self.push_checked(start, dur, n)
    }

    fn push_checked(&mut self, start: i128, dur: i128, mut n: TNote) -> bool {
        let end = start + dur;
        if start > u64::MAX as i128 || end > u64::MAX as i128 {
            self.stats.overflow_dropped += 1;
            return true;
        }
        n.start = start as u64;
        n.dur = dur as u64;
        if self.len() >= self.limit {
            self.exceeded = true;
            return false;
        }
        self.notes.push(n);
        if let Some((pos, _)) = &mut self.dims {
            pos.push(self.dim);
        }
        true
    }

    /// Keeps the notes from index `from` on (not those waiting for the Shift
    /// policy) for which `keep` holds — it may change them — in order, with
    /// their driver dimensions (a kernel's own thinning or cutting pass).
    pub fn retain_notes_from(&mut self, from: usize, mut keep: impl FnMut(&mut TNote) -> bool) {
        let from = from.min(self.notes.len());
        let mut w = from;
        for r in from..self.notes.len() {
            let mut n = self.notes[r];
            if keep(&mut n) {
                self.notes[w] = n;
                if let Some((pos, _)) = &mut self.dims
                    && let Some(&d) = pos.get(r)
                    && w < pos.len()
                {
                    pos[w] = d;
                }
                w += 1;
            }
        }
        self.notes.truncate(w);
        if let Some((pos, _)) = &mut self.dims {
            pos.truncate(w);
        }
    }

    /// V09-0143: the tool-specific driver dimension of the notes emitted
    /// from now on — the ordinal the kernel gives them (copy, repeat, step,
    /// generation, ring …; [`super::definition::driver_dimension`] names
    /// it). Cheap; recorded only while an offset driver reads it.
    #[inline]
    pub fn set_dim(&mut self, d: u32) {
        self.dim = d;
    }

    /// Starts keeping each emitted note's dimension (the notes already
    /// emitted get 0). The platform's offset drivers turn it on before the
    /// kernel runs when one is driven by the tool's dimension.
    pub fn enable_dims(&mut self) {
        let (pos, neg) = self.dims.get_or_insert_with(Default::default);
        pos.resize(self.notes.len(), 0);
        neg.resize(self.negative.len(), 0);
        self.dim = 0;
    }

    /// The dimensions of the notes emitted since `mark`, in
    /// [`Output::adjust_since`]'s order (0 where none was kept; a kernel
    /// that rewrote `notes` in place keeps its notes' values only if it
    /// kept the order).
    pub fn dims_since(&self, mark: Mark) -> Vec<u32> {
        let Mark(a, b) = mark;
        let n = self.notes.len().saturating_sub(a) + self.negative.len().saturating_sub(b);
        let Some((pos, neg)) = &self.dims else {
            return vec![0; n];
        };
        let at = |v: &[u32], i: usize| v.get(i).copied().unwrap_or(0);
        (a..self.notes.len())
            .map(|i| at(pos, i))
            .chain((b..self.negative.len()).map(|i| at(neg, i)))
            .collect()
    }

    /// Emits a note that is already valid (identity / copies).
    #[inline]
    pub fn push(&mut self, n: TNote) -> bool {
        self.emit(
            i128::from(n.start),
            i128::from(n.dur),
            i64::from(n.pitch),
            i64::from(n.vel),
            &n,
        )
    }

    /// Error to propagate when the cap was hit or the run cancelled.
    pub fn check(&self) -> Result<(), TransformError> {
        if self.boundary.pitch == super::PitchPolicy::Refuse && self.stats.pitch_dropped > 0 {
            return Err(TransformError::Invalid(format!(
                "{} note(s) would leave the pitch range {}–{}, so nothing was changed. Choose another boundary policy to clamp, wrap or remove them.",
                crate::midi::group_digits(self.stats.pitch_dropped),
                self.boundary.lo,
                self.boundary.hi.max(self.boundary.lo)
            )));
        }
        if self.cancelled {
            return Err(TransformError::Cancelled);
        }
        if self.exceeded {
            return Err(TransformError::TooLarge(format!(
                "The result would have more than {} notes. Reduce the settings or select fewer notes.",
                crate::midi::group_digits(self.limit as u64)
            )));
        }
        Ok(())
    }

    /// Finishes: applies the Shift policy, sorts by (track, start, pitch, …).
    pub fn finish(self) -> Result<(Vec<TNote>, OutStats), TransformError> {
        self.finish_with_message().map(|(n, s, _)| (n, s))
    }

    /// Like [`Output::finish`], also returning the kernel's message.
    pub fn finish_with_message(
        mut self,
    ) -> Result<(Vec<TNote>, OutStats, Option<String>), TransformError> {
        self.check()?;
        if !self.negative.is_empty() {
            let min = self.negative.iter().map(|(s, _)| *s).min().unwrap_or(0);
            let shift = (-min) as u128;
            if shift > u64::MAX as u128 {
                return Err(TransformError::Invalid(
                    "The result starts too far before tick 0 to be shifted.".into(),
                ));
            }
            let shift = shift as u64;
            self.stats.shifted_by = shift;
            let mut moved = Vec::with_capacity(self.notes.len() + self.negative.len());
            for (s, mut n) in std::mem::take(&mut self.negative) {
                n.start = (s + shift as i128) as u64;
                if n.start.checked_add(n.dur).is_some() {
                    moved.push(n);
                } else {
                    self.stats.overflow_dropped += 1;
                }
            }
            for mut n in std::mem::take(&mut self.notes) {
                match n
                    .start
                    .checked_add(shift)
                    .and_then(|s| s.checked_add(n.dur).map(|_| s))
                {
                    Some(s) => {
                        n.start = s;
                        moved.push(n);
                    }
                    None => self.stats.overflow_dropped += 1,
                }
            }
            self.notes = moved;
        }
        sort_notes(&mut self.notes);
        Ok((self.notes, self.stats, self.message))
    }
}

/// Canonical order: track slot, start, pitch, then everything else (stable and
/// deterministic regardless of the order kernels emitted notes in).
pub fn sort_notes(v: &mut [TNote]) {
    v.sort_unstable_by(|a, b| {
        (
            a.track, a.start, a.pitch, a.dur, a.sub, a.vel, a.ch, a.rel, a.group, a.flags,
        )
            .cmp(&(
                b.track, b.start, b.pitch, b.dur, b.sub, b.vel, b.ch, b.rel, b.group, b.flags,
            ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::boundary::{PitchPolicy, TickPolicy};

    fn note() -> TNote {
        TNote {
            start: 10,
            dur: 5,
            pitch: 60,
            vel: 100,
            ..TNote::default()
        }
    }

    #[test]
    fn the_sink_applies_policies_and_the_cap() {
        let mut o = Output::new(Boundary::default(), 3);
        assert!(o.emit(-5, 10, 60, 300, &note()));
        assert!(o.emit(0, 0, 60, 0, &note()));
        assert!(o.emit(0, 1, 200, 1, &note()));
        assert!(o.emit(1, 1, 60, 1, &note()));
        assert!(!o.emit(2, 1, 60, 1, &note()));
        assert!(o.check().is_err());
        assert_eq!(o.stats.tick_clamped, 1);
        assert_eq!(o.stats.len_fixed, 1);
        assert_eq!(o.stats.pitch_dropped, 1);
        assert_eq!(o.notes[0].vel, 127);
        assert_eq!(o.notes[1].vel, 1);
    }

    #[test]
    fn shift_moves_everything_and_overflow_is_dropped() {
        let b = Boundary {
            tick: TickPolicy::Shift,
            pitch: PitchPolicy::Clamp,
            ..Boundary::default()
        };
        let mut o = Output::new(b, 100);
        o.emit(-10, 5, 60, 100, &note());
        o.emit(0, 5, 61, 100, &note());
        o.emit(i128::from(u64::MAX) - 2, 5, 61, 100, &note());
        let (v, s) = o.finish().unwrap();
        assert_eq!(s.shifted_by, 10);
        assert_eq!(v.iter().map(|n| n.start).collect::<Vec<_>>(), vec![0, 10]);
        assert_eq!(s.overflow_dropped, 1);
    }

    /// V09-0143: the driver dimension is kept per note only while enabled,
    /// in `adjust_since`'s order (the notes, then the ones waiting for the
    /// Shift policy), and moves with the notes `adjust_since` keeps.
    #[test]
    fn dimensions_follow_their_notes_through_adjustments() {
        let b = Boundary {
            tick: TickPolicy::Shift,
            pitch: PitchPolicy::Clamp,
            ..Boundary::default()
        };
        let mut o = Output::new(b, 100);
        o.set_dim(9);
        o.push(note()); // before enabling: 0
        let mark = o.mark();
        o.enable_dims();
        for (k, start) in [(1u32, 20i128), (2, -5), (3, 30), (4, 40), (5, -7)] {
            o.set_dim(k);
            o.emit(start, 5, 60 + k as i64, 100, &note());
        }
        assert_eq!(o.dims_since(Mark(0, 0)), vec![0, 1, 3, 4, 2, 5]);
        assert_eq!(o.dims_since(mark), vec![1, 3, 4, 2, 5]);
        // Drop the notes of dimension 3 and 2; the rest keep theirs.
        let dims = o.dims_since(mark);
        o.adjust_since(mark, |i, _, _, _| dims[i] != 3 && dims[i] != 2);
        assert_eq!(o.dims_since(mark), vec![1, 4, 5]);
        let pitches: Vec<u8> = o.notes[1..].iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, vec![61, 64]);
        // Not enabled: zeros, never stale values.
        let mut q = Output::new(Boundary::default(), 10);
        q.set_dim(7);
        q.push(note());
        assert_eq!(q.dims_since(Mark(0, 0)), vec![0]);
    }
}
