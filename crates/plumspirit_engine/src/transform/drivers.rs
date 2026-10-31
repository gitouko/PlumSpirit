//! Shared offset drivers (v0.9 contract §098 functions 1–2, §232, §233).
//!
//! One driver turns something about a note — its time, pitch, place in the
//! order, velocity, subchannel or phase in a repeating pattern — into a
//! signed offset: a step every so much of that input, a cycled list of
//! offsets, or a graph over the input's range. The Subchannel Offset and
//! Pitch Offset functions of every eligible tool are this one driver bound
//! to a subchannel or a pitch (instead of fifty copies of `i % n`), and the
//! subchannel it lands on follows the same wrap policies Paint V2 uses
//! ([`move_sub`], shared with `tools/paint.rs`).
//!
//! Everything here is per note, allocation-free and deterministic: the
//! offset depends only on the note, its place and the run's [`Frame`].

use super::TNote;
use super::param::Curve;
use super::source::{SourceStats, SourceTrack};

/// What drives the offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Input {
    /// Ticks after the first start of the notes.
    Time,
    /// Semitones above the lowest note.
    Pitch,
    /// Place in start order on the note's track (0 = first).
    NoteIndex,
    /// Velocity (1–127).
    Velocity,
    /// Position of the note's subchannel in its track's list.
    Subchannel,
    /// Ticks into a repeating period (the time since the first start, modulo
    /// the period).
    PatternPhase,
    /// The tool's own dimension (§098 "tool-specific dimension", §232
    /// "tools can bind it differently"): the ordinal the kernel gave the
    /// note — its copy, repeat, step, generation, ring … — through
    /// [`super::Output::set_dim`]; named per tool by
    /// [`super::definition::driver_dimension`], menu position 6.
    Tool,
}

impl Input {
    /// The generic inputs (every eligible tool's menu; [`Input::Tool`]
    /// follows them where the tool declares a dimension).
    pub const ALL: [Input; 6] = [
        Input::Time,
        Input::Pitch,
        Input::NoteIndex,
        Input::Velocity,
        Input::Subchannel,
        Input::PatternPhase,
    ];

    /// Menu names, in [`Input::ALL`] order.
    pub const NAMES: &'static [&'static str] = &[
        "Time",
        "Pitch",
        "Note index",
        "Velocity",
        "Subchannel",
        "Pattern phase",
    ];

    /// The input at menu position `i` (6: the tool's dimension; out of
    /// range: Time).
    pub fn from_index(i: usize) -> Input {
        match i {
            6 => Input::Tool,
            _ => Input::ALL.get(i).copied().unwrap_or(Input::Time),
        }
    }

    /// Whether the offset of a note can be computed from the note alone and
    /// the run's frame (everything but the note's place in the order). The
    /// tool's dimension is normalised by the run's largest value, so it is
    /// measured over a whole run like the note index.
    pub fn is_pointwise(self) -> bool {
        !matches!(self, Input::NoteIndex | Input::Tool)
    }
}

/// How the input becomes an offset.
#[derive(Clone, Debug, PartialEq)]
pub enum Mapping {
    /// `step` more at every `every` of the input (signed).
    Step(i64),
    /// The list's entries in turn, one per `every` of the input (signed;
    /// empty: no offset).
    Cycle(Vec<i64>),
    /// The graph over the input's range (0 … 1 across the notes, or across
    /// one period), from `lo` at 0 to `hi` at 1 — either may be negative.
    Graph { curve: Curve, lo: i64, hi: i64 },
}

/// A signed offset driver.
#[derive(Clone, Debug, PartialEq)]
pub struct Driver {
    pub input: Input,
    /// Length of one step / cycle entry in the input's unit: ticks (Time,
    /// Pattern phase), semitones, notes, velocity units or subchannel
    /// positions. At least 1.
    pub every: u64,
    /// Pattern phase: the repeating period in ticks (at least 1).
    pub period: u64,
    pub mapping: Mapping,
}

/// What the inputs are measured against: the notes of one run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub min_start: u64,
    pub max_start: u64,
    pub min_pitch: u8,
    pub max_pitch: u8,
}

impl Frame {
    /// The frame of a source's statistics.
    pub fn of(stats: &SourceStats) -> Frame {
        if stats.count == 0 {
            return Frame::default();
        }
        Frame {
            min_start: stats.min_start,
            max_start: stats.max_start,
            min_pitch: stats.min_pitch,
            max_pitch: stats.max_pitch,
        }
    }

    /// The frame of a list of notes (one pass).
    pub fn of_notes(notes: &[TNote]) -> Frame {
        let mut f = Frame {
            min_start: u64::MAX,
            max_start: 0,
            min_pitch: u8::MAX,
            max_pitch: 0,
        };
        for n in notes {
            f.min_start = f.min_start.min(n.start);
            f.max_start = f.max_start.max(n.start);
            f.min_pitch = f.min_pitch.min(n.pitch);
            f.max_pitch = f.max_pitch.max(n.pitch);
        }
        if notes.is_empty() {
            Frame::default()
        } else {
            f
        }
    }
}

/// A note's place for the inputs the note itself does not carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// Place in start order on its track (0 = first).
    pub index: usize,
    /// Notes on its track.
    pub count: usize,
    /// Position of its subchannel in the track's list.
    pub sub_pos: usize,
    /// Subchannels of the track.
    pub subs: usize,
    /// The note's value of the tool's dimension ([`Input::Tool`]).
    pub dim: u32,
    /// The largest value of the tool's dimension in the run.
    pub dim_max: u32,
}

impl Place {
    /// Unit `index` of `count` units (a generator's own units: one
    /// subchannel).
    pub fn unit(index: usize, count: usize) -> Place {
        Place {
            index,
            count,
            sub_pos: 0,
            subs: 1,
            ..Place::default()
        }
    }
}

/// `a / b` in [0, 1] (0 when `b` is 0).
#[inline]
fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        (a as f64 / b as f64).clamp(0.0, 1.0)
    }
}

/// Input `input` of note `n`: its position in the input's unit and the same
/// position normalised to 0 … 1 over the notes (Pattern phase: over one
/// `period` of ticks). Shared by the drivers and the Control Sweep engine.
#[inline]
pub fn measure(input: Input, n: &TNote, place: Place, f: &Frame, period: u64) -> (u64, f64) {
    match input {
        Input::Time => {
            let x = n.start.saturating_sub(f.min_start);
            (x, ratio(x, f.max_start.saturating_sub(f.min_start)))
        }
        Input::Pitch => {
            let x = u64::from(n.pitch.saturating_sub(f.min_pitch));
            (
                x,
                ratio(x, u64::from(f.max_pitch.saturating_sub(f.min_pitch))),
            )
        }
        Input::NoteIndex => {
            let x = place.index as u64;
            (x, ratio(x, place.count.saturating_sub(1) as u64))
        }
        Input::Velocity => {
            let x = u64::from(n.vel);
            (x, ratio(x.saturating_sub(1), 126))
        }
        Input::Subchannel => {
            let x = place.sub_pos as u64;
            (x, ratio(x, place.subs.saturating_sub(1) as u64))
        }
        Input::PatternPhase => {
            let p = period.max(1);
            let x = n.start.saturating_sub(f.min_start) % p;
            (x, x as f64 / p as f64)
        }
        Input::Tool => {
            let x = u64::from(place.dim);
            (x, ratio(x, u64::from(place.dim_max)))
        }
    }
}

impl Driver {
    /// Whether every offset can be computed from the note and the frame.
    pub fn is_pointwise(&self) -> bool {
        self.input.is_pointwise()
    }

    /// The input of note `n` (see [`measure`]).
    #[inline]
    pub fn measure(&self, n: &TNote, place: Place, f: &Frame) -> (u64, f64) {
        measure(self.input, n, place, f, self.period)
    }

    /// The signed offset for note `n`.
    #[inline]
    pub fn offset(&self, n: &TNote, place: Place, f: &Frame) -> i64 {
        let (x, u) = self.measure(n, place, f);
        let k = x / self.every.max(1);
        match &self.mapping {
            Mapping::Step(s) => i64::try_from(k).unwrap_or(i64::MAX).saturating_mul(*s),
            Mapping::Cycle(v) => {
                if v.is_empty() {
                    0
                } else {
                    v[(k % v.len() as u64) as usize]
                }
            }
            Mapping::Graph { curve, lo, hi } => {
                let (lo, hi) = (*lo as f64, *hi as f64);
                (lo + curve.eval(u) * (hi - lo)).round() as i64
            }
        }
    }
}

/// Where an offset subchannel lands (the policies of Paint V2, §164–§181).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubWrap {
    /// Around the track's own subchannel list (by position).
    Cycle,
    /// Within `n` subchannels from logical subchannel `lo` (0-based), wrapping.
    Wrap { lo: i64, n: i64 },
    /// Within the range, bouncing back at its ends.
    Reflect { lo: i64, n: i64 },
    /// Within the range, stopping at its ends.
    Clamp { lo: i64, n: i64 },
    /// Any subchannel from 1 up (missing ones are created when applied).
    Unbounded,
}

/// Menu names of the wrap policies, in [`SubWrap::from_choice`] order.
pub const SUB_WRAPS: &[&str] = &[
    "Cycle the track's subchannels",
    "Wrap within a range",
    "Reflect within a range",
    "Clamp to a range",
    "Unbounded (new subchannels as needed)",
];

impl SubWrap {
    /// The policy at menu position `c` with the range starting at logical
    /// subchannel `lo` (0-based) and `n` wide.
    pub fn from_choice(c: usize, lo: i64, n: i64) -> SubWrap {
        let (lo, n) = (lo.max(0), n.max(1));
        match c {
            0 => SubWrap::Cycle,
            1 => SubWrap::Wrap { lo, n },
            2 => SubWrap::Reflect { lo, n },
            3 => SubWrap::Clamp { lo, n },
            _ => SubWrap::Unbounded,
        }
    }

    /// Whether results can use subchannels the track does not have yet.
    pub fn may_create(self) -> bool {
        !matches!(self, SubWrap::Cycle)
    }
}

/// The subchannel (id, MIDI channel) of note `n` moved by `off` under `wrap`
/// (`base`: an absolute starting point instead of the note's own subchannel
/// — a table position for [`SubWrap::Cycle`], a logical subchannel
/// otherwise). With `set_channel` the note takes the subchannel's MIDI
/// channel (when the track has it), else it keeps its own.
pub fn move_sub(
    wrap: SubWrap,
    set_channel: bool,
    tr: Option<&SourceTrack>,
    n: &TNote,
    off: i64,
    base: Option<i64>,
) -> (u32, u8) {
    let (lo, cnt) = match wrap {
        SubWrap::Cycle => {
            let Some(t) = tr.filter(|t| !t.subs.is_empty()) else {
                return (n.sub, n.ch);
            };
            let len = t.subs.len() as i64;
            let cur = base.unwrap_or_else(|| t.sub_index(n.sub).unwrap_or(0) as i64);
            let (id, ch) = t.subs[(cur.saturating_add(off)).rem_euclid(len) as usize];
            return (id, if set_channel { ch } else { n.ch });
        }
        SubWrap::Wrap { lo, n } | SubWrap::Reflect { lo, n } | SubWrap::Clamp { lo, n } => {
            (lo, n.max(1))
        }
        SubWrap::Unbounded => (0, 1),
    };
    // Kept within ±2⁶² so a saturated offset minus the range's start never
    // overflows (V09-S38: the far-end harness); results there are unchanged.
    const FAR: i64 = 1 << 62;
    let v = base
        .unwrap_or(i64::from(n.sub))
        .saturating_add(off)
        .clamp(-FAR, FAR);
    let id = match wrap {
        SubWrap::Wrap { .. } => lo + (v - lo).rem_euclid(cnt),
        SubWrap::Reflect { .. } => {
            if cnt == 1 {
                lo
            } else {
                let period = 2 * (cnt - 1);
                let x = (v - lo).rem_euclid(period);
                lo + if x < cnt { x } else { period - x }
            }
        }
        SubWrap::Clamp { .. } => v.clamp(lo, lo + cnt - 1),
        _ => v.max(0),
    };
    let id = id.clamp(0, i64::from(u32::MAX)) as u32;
    let ch = if set_channel {
        tr.and_then(|t| t.sub_channel(id)).unwrap_or(n.ch)
    } else {
        n.ch
    };
    (id, ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: u64, pitch: u8, vel: u8, sub: u32) -> TNote {
        TNote {
            start,
            dur: 10,
            sub,
            group: 0,
            track: 0,
            pitch,
            vel,
            rel: 64,
            ch: 0,
            flags: 0,
        }
    }

    fn frame() -> Frame {
        Frame {
            min_start: 100,
            max_start: 1100,
            min_pitch: 40,
            max_pitch: 80,
        }
    }

    fn driver(input: Input, every: u64, mapping: Mapping) -> Driver {
        Driver {
            input,
            every,
            period: 400,
            mapping,
        }
    }

    #[test]
    fn steps_count_whole_intervals_of_each_input() {
        let f = frame();
        let p = Place {
            index: 7,
            count: 10,
            sub_pos: 2,
            subs: 4,
            ..Place::default()
        };
        let n = note(650, 52, 90, 3);
        let at = |input, every| driver(input, every, Mapping::Step(-3)).offset(&n, p, &f);
        assert_eq!(at(Input::Time, 100), -15, "550 ticks → 5 steps");
        assert_eq!(at(Input::Pitch, 4), -9, "12 semitones → 3 steps");
        assert_eq!(at(Input::NoteIndex, 2), -9, "index 7 → 3 steps");
        assert_eq!(at(Input::Velocity, 30), -9, "90 → 3 steps");
        assert_eq!(at(Input::Subchannel, 1), -6, "position 2");
        assert_eq!(
            at(Input::PatternPhase, 100),
            -3,
            "550 mod 400 = 150 → 1 step"
        );
        // A zero interval counts as one.
        assert_eq!(at(Input::Subchannel, 0), -6);
        // Before the frame's start (a note outside it) counts as 0.
        let early = note(0, 10, 1, 0);
        let d = driver(Input::Time, 1, Mapping::Step(5));
        assert_eq!(d.offset(&early, p, &f), 0);
        assert!(!driver(Input::NoteIndex, 1, Mapping::Step(1)).is_pointwise());
        assert!(d.is_pointwise());
    }

    #[test]
    fn cycles_repeat_their_signed_entries() {
        let f = frame();
        let d = driver(Input::Time, 100, Mapping::Cycle(vec![0, 2, -1]));
        let got: Vec<i64> = (0..7)
            .map(|k| d.offset(&note(100 + k * 100 + 50, 60, 64, 0), Place::default(), &f))
            .collect();
        assert_eq!(got, vec![0, 2, -1, 0, 2, -1, 0]);
        let empty = driver(Input::Time, 100, Mapping::Cycle(vec![]));
        assert_eq!(empty.offset(&note(900, 60, 64, 0), Place::default(), &f), 0);
        // Pattern phase restarts every period (400 ticks).
        let ph = driver(Input::PatternPhase, 100, Mapping::Cycle(vec![1, 2, 3, 4]));
        let a = ph.offset(&note(100 + 150, 60, 64, 0), Place::default(), &f);
        let b = ph.offset(&note(100 + 550, 60, 64, 0), Place::default(), &f);
        assert_eq!((a, b), (2, 2));
    }

    #[test]
    fn graphs_map_the_normalised_input_into_a_signed_range() {
        let f = frame();
        let g = |input| {
            driver(
                input,
                1,
                Mapping::Graph {
                    curve: Curve::line(0.0, 1.0),
                    lo: -12,
                    hi: 12,
                },
            )
        };
        let p = Place {
            index: 0,
            count: 5,
            sub_pos: 3,
            subs: 4,
            ..Place::default()
        };
        let d = g(Input::Time);
        assert_eq!(d.offset(&note(100, 60, 64, 0), p, &f), -12);
        assert_eq!(d.offset(&note(600, 60, 64, 0), p, &f), 0);
        assert_eq!(d.offset(&note(1100, 60, 64, 0), p, &f), 12);
        assert_eq!(g(Input::Pitch).offset(&note(0, 80, 64, 0), p, &f), 12);
        assert_eq!(g(Input::Subchannel).offset(&note(0, 80, 64, 0), p, &f), 12);
        assert_eq!(g(Input::NoteIndex).offset(&note(0, 80, 64, 0), p, &f), -12);
        assert_eq!(g(Input::Velocity).offset(&note(0, 80, 127, 0), p, &f), 12);
        assert_eq!(g(Input::Velocity).offset(&note(0, 80, 1, 0), p, &f), -12);
        // Phase: half way through the period.
        assert_eq!(g(Input::PatternPhase).offset(&note(300, 0, 1, 0), p, &f), 0);
        // A single note (empty ranges) sits at the graph's start.
        let one = Frame::of_notes(&[note(5, 60, 64, 0)]);
        assert_eq!(d.offset(&note(5, 60, 64, 0), Place::default(), &one), -12);
        // Decreasing graphs and reversed ranges are fine.
        let down = driver(
            Input::Time,
            1,
            Mapping::Graph {
                curve: Curve::line(1.0, 0.0),
                lo: 5,
                hi: -5,
            },
        );
        assert_eq!(down.offset(&note(100, 0, 1, 0), p, &f), -5);
    }

    #[test]
    fn frames_come_from_statistics_or_notes() {
        let notes = [
            note(300, 70, 1, 0),
            note(100, 40, 1, 0),
            note(900, 55, 1, 0),
        ];
        let f = Frame::of_notes(&notes);
        assert_eq!(
            f,
            Frame {
                min_start: 100,
                max_start: 900,
                min_pitch: 40,
                max_pitch: 70
            }
        );
        let mut s = SourceStats::default();
        for n in &notes {
            s.add(n);
        }
        assert_eq!(Frame::of(&s), f);
        assert_eq!(Frame::of(&SourceStats::default()), Frame::default());
        assert_eq!(Frame::of_notes(&[]), Frame::default());
    }

    #[test]
    fn subchannels_land_by_the_wrap_policy() {
        let tr = SourceTrack {
            id: crate::model::TrackId(0),
            index: 0,
            revision: 0,
            subs: vec![(0, 0), (1, 1), (5, 2)],
            all: false,
            insert_only: false,
            frame: 0,
            content: None,
        };
        let n = note(0, 60, 64, 1);
        let mv = |w, off| move_sub(w, false, Some(&tr), &n, off, None).0;
        // Cycle: by table position (1 is at position 1).
        assert_eq!(mv(SubWrap::Cycle, 1), 5);
        assert_eq!(mv(SubWrap::Cycle, -2), 5);
        assert_eq!(mv(SubWrap::Cycle, 3), 1);
        // Ranges over logical subchannels 2 … 4 (0-based lo 2, 3 wide).
        let r = |c| SubWrap::from_choice(c, 2, 3);
        assert_eq!(mv(r(1), 0), 4, "1 wraps into 2 … 4");
        assert_eq!(mv(r(1), 4), 2);
        assert_eq!(mv(r(2), 4), 3, "reflect: 5 → 3");
        assert_eq!(mv(r(3), 9), 4);
        assert_eq!(mv(r(3), -9), 2);
        assert_eq!(mv(SubWrap::Unbounded, 40), 41);
        assert_eq!(mv(SubWrap::Unbounded, -40), 0);
        assert!(SubWrap::Unbounded.may_create() && !SubWrap::Cycle.may_create());
        // Channels: kept, or the subchannel's own.
        assert_eq!(
            move_sub(SubWrap::Cycle, true, Some(&tr), &n, 1, None),
            (5, 2)
        );
        assert_eq!(
            move_sub(SubWrap::Unbounded, true, Some(&tr), &n, 4, None),
            (5, 2)
        );
        assert_eq!(
            move_sub(SubWrap::Unbounded, true, Some(&tr), &n, 5, None),
            (6, 0)
        );
        // An absolute base.
        assert_eq!(
            move_sub(SubWrap::Cycle, false, Some(&tr), &n, 0, Some(2)).0,
            5
        );
        // No track: the note stays where it is in Cycle mode.
        assert_eq!(move_sub(SubWrap::Cycle, false, None, &n, 3, None), (1, 0));
    }

    /// V09-0143: the tool's dimension is measured from the note's place
    /// (the ordinal its kernel gave it), normalised by the run's largest;
    /// it is menu position 6 and needs the whole run.
    #[test]
    fn the_tools_dimension_is_the_kernels_ordinal() {
        assert_eq!(Input::from_index(6), Input::Tool);
        assert_eq!(Input::from_index(7), Input::Time);
        assert!(!Input::Tool.is_pointwise());
        let n = note(100, 60, 100, 0);
        let f = Frame::default();
        let at = |dim, dim_max| Place {
            dim,
            dim_max,
            ..Place::default()
        };
        assert_eq!(measure(Input::Tool, &n, at(3, 4), &f, 1), (3, 0.75));
        assert_eq!(measure(Input::Tool, &n, at(0, 0), &f, 1), (0, 0.0));
        let d = Driver {
            input: Input::Tool,
            every: 2,
            period: 1,
            mapping: Mapping::Step(-5),
        };
        // Every 2 units one step of −5.
        let offs: Vec<i64> = (0..6).map(|k| d.offset(&n, at(k, 5), &f)).collect();
        assert_eq!(offs, vec![0, 0, -5, -5, -10, -10]);
        let g = Driver {
            mapping: Mapping::Graph {
                curve: Curve::line(0.0, 1.0),
                lo: 0,
                hi: 8,
            },
            ..d
        };
        assert_eq!(g.offset(&n, at(2, 4), &f), 4);
    }
}
