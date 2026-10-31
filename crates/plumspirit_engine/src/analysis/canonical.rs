//! The canonical definitions of PlumSpirit's note statistics (v0.8 §073).
//!
//! Every readout — the Progressor, its peaks, Selection Analytics, the live
//! keyboard — answers from these definitions and the primitives below;
//! nothing redefines them. `tests/v08_metrics.rs` checks every readout against
//! a literal reading of this list.
//!
//! * A note is counted **as it plays**: Track Properties corrections applied
//!   once, muted notes and notes a correction drops (outside the key range,
//!   overflowing) left out. Ghost (Onion) notes are never counted.
//! * A note **sounds** over the half-open interval `[start, end)` of ticks. A
//!   zero-length note never sounds but is still a note.
//! * **Notes** at `t`: notes whose start is `<= t`.
//! * **Poly** at `t`: note *instances* sounding at `t` (two notes of the same
//!   pitch count twice) = starts `<= t` − ends `<= t`.
//! * **NPS** at `t`: note starts in the trailing window `(t − W, t]` of musical
//!   time (`W` = the Progressor's window, 1 s by default); while less than `W`
//!   has passed since the song start, every start `<= t` ([`window_lo`]).
//! * **PPS** at `t`: the time-weighted average Poly over that window,
//!   `∫ Poly dτ / W` — the Poly Area of the window per window length
//!   (`AnalysisIndex::metrics`).
//! * **Peak Poly**: the highest Poly at any tick of a range ([`PolySweep`]).
//! * **Peak NPS**: the highest NPS at any note start of a range.
//! * **Unique Pitch Poly**: the highest number of *distinct* pitches sounding
//!   at once ([`PitchPoly`]; at most 128, 256 in 256-Key Mode).
//! * **Poly Area** (Sustain Poly): `∫ Poly dt` = the sum of the notes'
//!   lengths, in note·ticks and in note·nanoseconds of musical time
//!   ([`note_area`]).
//!
//! Not a metric: `midi::export::WriteStats::peak_polyphony` is the exporter's
//! pending note-off count per track (a buffer bound), not Poly.

use super::Timing;

/// The tick after which a trailing window of `window_ns` ending at musical
/// time `now_ns` begins: the window is `(lo, t]`. `None`: the window reaches
/// back to the song start (every start up to `t` counts).
#[inline]
pub fn window_lo(timing: &Timing, now_ns: u64, window_ns: u64) -> Option<u64> {
    (now_ns >= window_ns).then(|| timing.tick_at_nanos(now_ns - window_ns))
}

/// A note's contribution to Poly Area: (note·ticks, note·nanoseconds).
#[inline]
pub fn note_area(timing: &Timing, start: u64, end: u64) -> (u128, u128) {
    (
        u128::from(end - start),
        u128::from(timing.nanos_at(end) - timing.nanos_at(start)),
    )
}

/// Poly and Peak Poly in tick order: one [`PolySweep::tick`] per distinct
/// event tick with the number of notes starting and ending there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolySweep {
    /// Poly after the last tick fed.
    pub poly: u64,
    /// Highest Poly seen (including the starting value).
    pub peak: u64,
    /// First tick where `peak` was reached (`None`: the starting value).
    pub peak_tick: Option<u64>,
}

impl PolySweep {
    /// A sweep that begins with `poly` notes sounding.
    pub fn starting_at(poly: u64) -> PolySweep {
        PolySweep {
            poly,
            peak: poly,
            peak_tick: None,
        }
    }

    /// Every event at `tick`: `starts` notes begin and `ends` notes end
    /// (half-open, so an end and a start on the same tick never overlap; a
    /// zero-length note is one start and one end). Returns the Poly at `tick`.
    #[inline]
    pub fn tick(&mut self, tick: u64, starts: u64, ends: u64) -> u64 {
        self.poly = self.poly + starts - ends;
        if self.poly > self.peak {
            self.peak = self.poly;
            self.peak_tick = Some(tick);
        }
        self.poly
    }
}

/// Unique Pitch Poly in tick order: per tick, [`PitchPoly::end`] the notes
/// that stop, [`PitchPoly::start`] the notes that begin (zero-length notes
/// neither), then [`PitchPoly::settle`].
#[derive(Clone, Debug)]
pub struct PitchPoly {
    /// Sounding notes per pitch, all 256 keys (v0.9 256-Key Mode; 1 KiB).
    per_pitch: [u32; 256],
    distinct: u32,
    pub peak: u32,
    pub peak_tick: u64,
}

impl Default for PitchPoly {
    fn default() -> Self {
        PitchPoly {
            per_pitch: [0; 256],
            distinct: 0,
            peak: 0,
            peak_tick: 0,
        }
    }
}

impl PitchPoly {
    #[inline]
    pub fn start(&mut self, pitch: u8) {
        let c = &mut self.per_pitch[usize::from(pitch)];
        if *c == 0 {
            self.distinct += 1;
        }
        *c += 1;
    }

    #[inline]
    pub fn end(&mut self, pitch: u8) {
        let c = &mut self.per_pitch[usize::from(pitch)];
        *c -= 1;
        if *c == 0 {
            self.distinct -= 1;
        }
    }

    /// Distinct pitches sounding now.
    #[inline]
    pub fn distinct(&self) -> u32 {
        self.distinct
    }

    /// The tick's events are in: records a new peak (first tick wins).
    #[inline]
    pub fn settle(&mut self, tick: u64) {
        if self.distinct > self.peak {
            self.peak = self.distinct;
            self.peak_tick = tick;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poly_is_half_open_and_counts_instances() {
        let mut s = PolySweep::default();
        assert_eq!(s.tick(0, 2, 0), 2); // two notes of one pitch: two
        assert_eq!(s.tick(10, 1, 1), 2); // one ends as one starts: no overlap
        assert_eq!(s.tick(20, 1, 1), 2); // a zero-length note never sounds
        assert_eq!(s.tick(30, 0, 2), 0);
        assert_eq!((s.peak, s.peak_tick), (2, Some(0)));
    }

    #[test]
    fn unique_pitch_poly_counts_distinct_pitches() {
        let mut u = PitchPoly::default();
        u.start(60);
        u.start(60);
        u.start(64);
        u.settle(0);
        assert_eq!(u.peak, 2);
        u.end(60);
        u.settle(5);
        assert_eq!(u.peak, 2);
        u.end(60);
        u.end(64);
        u.settle(9);
        assert_eq!((u.peak, u.peak_tick), (2, 0));
        // 256-Key Mode: pitch 200 is its own key (v0.8 masked it onto 72).
        let mut u = PitchPoly::default();
        u.start(72);
        u.start(200);
        u.start(255);
        u.settle(0);
        assert_eq!(u.peak, 3);
    }
}
