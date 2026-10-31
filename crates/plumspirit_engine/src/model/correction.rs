//! Non-destructive track corrections (Track Properties).
//!
//! A track's tick, key and gate corrections and its key range never change the
//! notes stored in the project. They are resolved against the Conductor's time
//! signatures ([`Correction::resolve`]) and applied **once**, on the fly, by
//! playback and — unless the user exports the original data — by MIDI export.
//! Because the stored notes stay the original ones, saving and reopening a
//! project (or exporting it again) can never apply a correction twice.
//!
//! Policy (documented in `docs/ARCHITECTURE.md`):
//!
//! * The tick, key and gate corrections apply to notes that **start at or after
//!   the correction start** (the first tick of the chosen measure; measure 1 =
//!   the whole track). The tick correction also moves the track's channel
//!   events (controllers, program changes, pitch bend) from that point on, so
//!   they stay aligned with the notes; meta events (names, text) stay put.
//! * A note or event that the tick correction would move before tick 0 is
//!   placed at tick 0 (its length is kept) and counted in the report.
//! * The key correction transposes; a note it would move outside the project's
//!   keys (0–127, or 0–255 in 256-Key Mode) is never wrapped: playback skips
//!   it and export refuses until the correction or the notes change (both are
//!   reported).
//! * The gate correction scales lengths by a percentage (rounded half up, never
//!   below one tick for a note that had a length).
//! * The key range filters every note of the track by its sounding pitch (after
//!   the key correction): notes outside it are not played and, when corrections
//!   are applied, not exported; they stay in the project. A range ending at
//!   127 (the standard default) reaches the project's top key (255 in 256-Key
//!   Mode), like the transform boundary policies.

use super::events::{RawEvent, RawKind};
use super::note::NoteRecord;
use super::note_store::NoteSnapshot;
use super::track::{GatePercent, Track, TrackId, TrackProps};
use crate::view::grid::MeterMap;

/// A track's corrections resolved to ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Correction {
    /// Notes and channel events starting at or after this tick are corrected.
    pub from_tick: u64,
    pub ticks: i64,
    pub keys: i16,
    pub gate: GatePercent,
    /// Inclusive range of sounding pitches.
    pub range: (u8, u8),
    /// The project's highest key (127, or 255 in 256-Key Mode): the key
    /// correction never moves a note above it.
    pub top: u8,
}

/// What a correction does to one note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corrected {
    /// The note as it plays / is exported (`clamped`: moved to tick 0).
    Note { note: NoteRecord, clamped: bool },
    /// Outside the key range: not played or exported.
    OutsideRange,
    /// The key correction would move it outside the project's keys.
    PitchOverflow,
    /// Its corrected end would exceed the largest tick.
    EndOverflow,
}

impl Correction {
    /// The correction of `props` at the project's meter in a project of key
    /// range `keys`, or `None` when nothing is corrected. Without musical
    /// timing (SMPTE) the measure has no meaning and corrections apply to the
    /// whole track.
    pub fn resolve(
        props: &TrackProps,
        meter: &MeterMap,
        keys: super::project::KeyRange,
    ) -> Option<Correction> {
        if !props.corrects() {
            return None;
        }
        let from_tick = if meter.is_musical() {
            meter
                .measure_start(u64::from(props.correction_from_measure.max(1)))
                .unwrap_or(u64::MAX)
        } else {
            0
        };
        let top = keys.max_pitch();
        let (lo, hi) = props.key_range;
        Some(Correction {
            from_tick,
            ticks: props.tick_correction,
            keys: props.key_correction,
            gate: props.gate,
            // The standard top key stands for the project's top key.
            range: (
                lo,
                if hi == super::note::MAX_PITCH {
                    top
                } else {
                    hi
                },
            ),
            top,
        })
    }

    #[inline]
    pub fn applies_at(&self, tick: u64) -> bool {
        tick >= self.from_tick
    }

    /// Only the key range filter is active (no reordering, no length change).
    pub fn filters_only(&self) -> bool {
        self.ticks == 0 && self.keys == 0 && self.gate.is_identity()
    }

    /// The corrected tick of an event at `tick` (clamped at 0; `true` when it
    /// was).
    #[inline]
    pub fn shift(&self, tick: u64) -> (u64, bool) {
        if !self.applies_at(tick) || self.ticks == 0 {
            return (tick, false);
        }
        let t = i128::from(tick) + i128::from(self.ticks);
        if t < 0 {
            (0, true)
        } else {
            (u64::try_from(t).unwrap_or(u64::MAX), false)
        }
    }

    /// The note as it plays.
    #[inline]
    pub fn note(&self, r: NoteRecord) -> Corrected {
        if !self.applies_at(r.start_tick) {
            let p = r.attrs.pitch();
            return if p < self.range.0 || p > self.range.1 {
                Corrected::OutsideRange
            } else {
                Corrected::Note {
                    note: r,
                    clamped: false,
                }
            };
        }
        let pitch = i16::from(r.attrs.pitch()) + self.keys;
        if !(0..=i16::from(self.top)).contains(&pitch) {
            return Corrected::PitchOverflow;
        }
        let pitch = pitch as u8;
        if pitch < self.range.0 || pitch > self.range.1 {
            return Corrected::OutsideRange;
        }
        let (start, clamped) = self.shift(r.start_tick);
        let Some(dur) = self.gate.apply(r.duration_ticks) else {
            return Corrected::EndOverflow;
        };
        if start.checked_add(dur).is_none() {
            return Corrected::EndOverflow;
        }
        let attrs = match r.attrs.with_pitch_wide(pitch) {
            Ok(a) => a,
            Err(_) => return Corrected::PitchOverflow,
        };
        Corrected::Note {
            note: NoteRecord {
                start_tick: start,
                duration_ticks: dur,
                attrs,
                sub: r.sub,
                group: r.group,
            },
            clamped,
        }
    }

    /// Short description for reports ("+120 ticks from measure 5, +12 semitones,
    /// gate 50%, keys C2–C6").
    pub fn describe(props: &TrackProps) -> String {
        let mut parts = Vec::new();
        if props.tick_correction != 0 {
            parts.push(format!("{:+} ticks", props.tick_correction));
        }
        if props.key_correction != 0 {
            parts.push(format!("{:+} semitones", props.key_correction));
        }
        if !props.gate.is_identity() {
            parts.push(format!("gate {}%", props.gate));
        }
        let mut s = parts.join(", ");
        if !s.is_empty() && props.correction_from_measure > 1 {
            s.push_str(&format!(" from measure {}", props.correction_from_measure));
        }
        if props.key_range != (0, 127) {
            if !s.is_empty() {
                s.push_str(", ");
            }
            s.push_str(&format!(
                "key range {}–{}",
                super::note::pitch_name(props.key_range.0),
                super::note::pitch_name(props.key_range.1)
            ));
        }
        s
    }
}

/// What the corrections of one track do to its current notes and events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CorrectionReport {
    pub track: Option<TrackId>,
    pub index: usize,
    /// Notes and channel events moved before tick 0 and placed at tick 0.
    pub clamped: u64,
    /// Notes the key correction would move outside the project's keys.
    pub pitch_overflow: u64,
    /// The project's key range (names the limit in [`CorrectionReport::notes`]).
    pub keys: super::project::KeyRange,
    /// Notes outside the key range (not played or exported).
    pub outside_range: u64,
    /// Notes whose corrected end would exceed the largest tick.
    pub end_overflow: u64,
    /// Latest corrected note end or event tick.
    pub content_end: u64,
}

impl CorrectionReport {
    /// Problems that make a corrected export impossible.
    pub fn blocks_export(&self) -> bool {
        self.pitch_overflow > 0 || self.end_overflow > 0
    }

    /// Plain-language lines for dialogs (empty when nothing is notable).
    pub fn notes(&self, track_label: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.pitch_overflow > 0 {
            let range = match self.keys {
                super::project::KeyRange::Standard => "the MIDI pitch range (0–127)",
                super::project::KeyRange::Extended256 => "the project's keys (0–255)",
            };
            out.push(format!(
                "{track_label}: the key correction would move {} note(s) outside {range}. They are not played, and MIDI export with corrections is refused until the correction or the notes change.",
                crate::midi::group_digits(self.pitch_overflow)
            ));
        }
        if self.end_overflow > 0 {
            out.push(format!(
                "{track_label}: the corrections would make {} note(s) end beyond the largest possible tick.",
                crate::midi::group_digits(self.end_overflow)
            ));
        }
        if self.clamped > 0 {
            out.push(format!(
                "{track_label}: the tick correction moves {} note(s) or event(s) before the start of the song; they are placed at the start (tick 0).",
                crate::midi::group_digits(self.clamped)
            ));
        }
        if self.outside_range > 0 {
            out.push(format!(
                "{track_label}: {} note(s) are outside the key range; they stay in the project but are not played or exported with corrections.",
                crate::midi::group_digits(self.outside_range)
            ));
        }
        out
    }
}

/// Checks what `c` does to `track` (O(notes) — for Apply, export and the
/// playback report, never per frame).
pub fn check(track: &Track, index: usize, c: &Correction) -> CorrectionReport {
    check_parts(
        track.id(),
        index,
        c,
        track.notes.iter().map(|n| n.record()),
        track.events.events().iter().copied(),
    )
}

/// [`check`] of a track whose notes and events are given (v0.9: a clip
/// track's placed notes, muted ones included, and its placed events).
pub fn check_parts(
    id: TrackId,
    index: usize,
    c: &Correction,
    notes: impl Iterator<Item = NoteRecord>,
    events: impl Iterator<Item = RawEvent>,
) -> CorrectionReport {
    let mut r = CorrectionReport {
        track: Some(id),
        index,
        keys: if c.top > super::note::MAX_PITCH {
            super::project::KeyRange::Extended256
        } else {
            super::project::KeyRange::Standard
        },
        ..CorrectionReport::default()
    };
    for n in notes {
        match c.note(n) {
            Corrected::Note { note, clamped } => {
                r.clamped += u64::from(clamped);
                r.content_end = r.content_end.max(note.start_tick + note.duration_ticks);
            }
            Corrected::OutsideRange => r.outside_range += 1,
            Corrected::PitchOverflow => r.pitch_overflow += 1,
            Corrected::EndOverflow => r.end_overflow += 1,
        }
    }
    for e in events {
        let t = if matches!(e.kind, RawKind::Channel { .. }) {
            let (t, clamped) = c.shift(e.tick);
            r.clamped += u64::from(clamped);
            t
        } else {
            e.tick
        };
        r.content_end = r.content_end.max(t);
    }
    r
}

/// The track's events with the tick correction applied to its channel events
/// (sorted by corrected tick; events keep their order within a tick).
pub fn corrected_events(events: impl Iterator<Item = RawEvent>, c: &Correction) -> Vec<RawEvent> {
    let mut out: Vec<RawEvent> = events
        .map(|e| {
            if matches!(e.kind, RawKind::Channel { .. }) {
                RawEvent {
                    tick: c.shift(e.tick).0,
                    ..e
                }
            } else {
                e
            }
        })
        .collect();
    if c.ticks != 0 {
        out.sort_by_key(|e| e.tick);
    }
    out
}

/// A track's notes as they play, in corrected start order: the notes before
/// the correction start and the corrected ones are two sorted runs, merged
/// (no copy of the notes). Notes outside the key range or out of the pitch
/// range are left out, and so are muted notes.
pub type CorrectedNotes = CorrectedRuns<SnapRun, SnapRun>;

/// The notes of a snapshot between two positions.
pub struct SnapRun {
    snap: NoteSnapshot,
    pos: super::note_store::SnapshotPos,
    end: Option<super::note_store::SnapshotPos>,
}

impl Iterator for SnapRun {
    type Item = NoteRecord;

    #[inline]
    fn next(&mut self) -> Option<NoteRecord> {
        if Some(self.pos) == self.end {
            return None;
        }
        let (r, next) = self.snap.next_at(self.pos)?;
        self.pos = next;
        Some(r)
    }
}

impl CorrectedRuns<SnapRun, SnapRun> {
    pub fn new(snap: NoteSnapshot, c: Correction) -> Self {
        let split = snap.lower_bound(c.from_tick);
        let a = SnapRun {
            snap: snap.clone(),
            pos: super::note_store::SnapshotPos::default(),
            end: Some(split),
        };
        let b = SnapRun {
            snap,
            pos: split,
            end: None,
        };
        CorrectedRuns::from_runs(a, b, c)
    }
}

/// [`CorrectedNotes`] over any source given as its two sorted runs: `a` the
/// notes starting before the correction start, `b` the others (v0.9: a clip
/// track's [`PlacedNotes`](super::placed::PlacedNotes) split at the
/// correction start).
pub struct CorrectedRuns<A, B> {
    a: A,
    b: B,
    c: Correction,
    next_a: Option<NoteRecord>,
    next_b: Option<NoteRecord>,
}

impl<A: Iterator<Item = NoteRecord>, B: Iterator<Item = NoteRecord>> CorrectedRuns<A, B> {
    pub fn from_runs(a: A, b: B, c: Correction) -> Self {
        let mut it = CorrectedRuns {
            a,
            b,
            c,
            next_a: None,
            next_b: None,
        };
        it.next_a = Self::pull(&mut it.a, &it.c);
        it.next_b = Self::pull(&mut it.b, &it.c);
        it
    }

    fn pull<I: Iterator<Item = NoteRecord>>(run: &mut I, c: &Correction) -> Option<NoteRecord> {
        for r in run.by_ref() {
            if r.attrs.muted() {
                continue;
            }
            if let Corrected::Note { note, .. } = c.note(r) {
                return Some(note);
            }
        }
        None
    }
}

impl<A: Iterator<Item = NoteRecord>, B: Iterator<Item = NoteRecord>> Iterator
    for CorrectedRuns<A, B>
{
    type Item = NoteRecord;

    fn next(&mut self) -> Option<NoteRecord> {
        let take_a = match (&self.next_a, &self.next_b) {
            (Some(a), Some(b)) => a.start_tick <= b.start_tick,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        if take_a {
            let r = self.next_a.take();
            self.next_a = Self::pull(&mut self.a, &self.c);
            r
        } else {
            let r = self.next_b.take();
            self.next_b = Self::pull(&mut self.b, &self.c);
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::events::EventStore;
    use crate::model::note::{NoteAttrs, NoteOffKind};
    use crate::model::note_store::NoteStore;
    use crate::model::timing::Division;
    use crate::model::track::TrackId;

    fn note(s: u64, d: u64, p: u8) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    fn track(notes: &[NoteRecord]) -> Track {
        let mut s = NoteStore::new();
        s.insert_sorted_batch(notes, false).unwrap();
        let mut ev = EventStore::new();
        for (t, st) in [(0u64, 0xC0u8), (480, 0xB0), (2000, 0xB0)] {
            ev.push(RawEvent {
                tick: t,
                anchor: 0,
                kind: RawKind::Channel {
                    status: st,
                    data: [7, 90],
                },
            })
            .unwrap();
        }
        Track::from_parts(TrackId(1), s, ev, None)
    }

    fn meter() -> MeterMap {
        MeterMap::new(Division::ppqn(480), &[(0, 4, 2)])
    }

    #[test]
    fn identity_properties_resolve_to_no_correction() {
        assert_eq!(
            Correction::resolve(&TrackProps::default(), &meter(), Default::default()),
            None
        );
    }

    #[test]
    fn corrections_apply_from_the_start_measure_and_never_wrap_pitches() {
        let props = TrackProps {
            tick_correction: -60,
            correction_from_measure: 2,
            key_correction: 12,
            gate: GatePercent::parse("50").unwrap(),
            ..TrackProps::default()
        };
        let c = Correction::resolve(&props, &meter(), Default::default()).unwrap();
        assert_eq!(c.from_tick, 1920);
        // Before measure 2: unchanged.
        assert_eq!(
            c.note(note(0, 480, 60)),
            Corrected::Note {
                note: note(0, 480, 60),
                clamped: false
            }
        );
        // From measure 2: shifted, transposed, half as long.
        assert_eq!(
            c.note(note(1920, 480, 60)),
            Corrected::Note {
                note: note(1860, 240, 72),
                clamped: false
            }
        );
        // Pitch 120 + 12 does not wrap.
        assert_eq!(c.note(note(1920, 480, 120)), Corrected::PitchOverflow);
        // Measure 1 with a negative correction clamps at 0.
        let c0 = Correction::resolve(
            &TrackProps {
                tick_correction: -60,
                ..TrackProps::default()
            },
            &meter(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            c0.note(note(10, 480, 60)),
            Corrected::Note {
                note: note(0, 480, 60),
                clamped: true
            }
        );
        // Key range filters by the sounding pitch.
        let cr = Correction::resolve(
            &TrackProps {
                key_correction: 12,
                key_range: (60, 71),
                ..TrackProps::default()
            },
            &meter(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(cr.note(note(0, 1, 60)), Corrected::OutsideRange); // sounds 72
        assert!(matches!(cr.note(note(0, 1, 59)), Corrected::Note { .. })); // sounds 71
    }

    #[test]
    fn corrected_notes_merge_both_runs_in_start_order() {
        let t = track(&[
            note(0, 100, 60),
            note(1000, 100, 61),
            note(1920, 100, 62),
            note(2000, 100, 63),
        ]);
        let c = Correction::resolve(
            &TrackProps {
                tick_correction: -1500,
                correction_from_measure: 2,
                ..TrackProps::default()
            },
            &meter(),
            Default::default(),
        )
        .unwrap();
        let got: Vec<(u64, u8)> = CorrectedNotes::new(t.notes.snapshot(), c)
            .map(|n| (n.start_tick, n.attrs.pitch()))
            .collect();
        // 1920 → 420 and 2000 → 500 land between 0 and 1000.
        assert_eq!(got, vec![(0, 60), (420, 62), (500, 63), (1000, 61)]);
        let r = check(&t, 0, &c);
        assert_eq!(r.clamped, 0);
        assert_eq!(r.content_end, 1100);
        // Channel events from measure 2 move with the notes (2000 → 500).
        let ev = corrected_events(t.events.events().iter().copied(), &c);
        let ticks: Vec<u64> = ev.iter().map(|e| e.tick).collect();
        assert_eq!(ticks, vec![0, 480, 500]);
        // The stored notes are untouched.
        assert_eq!(
            t.notes.iter().map(|n| n.start_tick).collect::<Vec<_>>(),
            vec![0, 1000, 1920, 2000]
        );
    }

    #[test]
    fn reports_count_every_problem() {
        let t = track(&[note(0, 100, 60), note(0, 100, 125), note(5, 100, 10)]);
        let c = Correction::resolve(
            &TrackProps {
                tick_correction: -10,
                key_correction: 5,
                key_range: (20, 127),
                ..TrackProps::default()
            },
            &meter(),
            Default::default(),
        )
        .unwrap();
        let r = check(&t, 2, &c);
        assert_eq!(r.pitch_overflow, 1); // 125 + 5
        assert_eq!(r.outside_range, 1); // 10 + 5 = 15 < 20
        assert_eq!(r.clamped, 2); // the note at 0 and the program change at 0
        assert!(r.blocks_export());
        assert_eq!(r.notes("Track 3").len(), 3);
    }
}
