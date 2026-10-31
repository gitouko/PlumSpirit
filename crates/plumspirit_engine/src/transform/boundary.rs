//! Boundary policies (contract §27): what happens to a result outside the
//! representable space — before tick 0, outside the pitch range, with a length
//! of zero or less, or beyond the largest tick. Every tool states its policy;
//! the [`super::Output`] sink applies it uniformly and counts what it did.

use super::param::{ParamSpec, Params, choice};
use crate::model::note::MAX_PITCH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchPolicy {
    /// Drop the note (counted in the preflight).
    Discard,
    /// Pin to the nearest valid pitch.
    Clamp,
    /// Move by octaves into the range (keeps the pitch class).
    WrapOctave,
    /// Wrap around the range (`lo + (p − lo) mod size`, "Undefined area: wrap").
    WrapAround,
    /// Mirror at the range ends.
    Reflect,
    /// Refuse the whole operation (nothing changes) and say how many notes
    /// would leave the range.
    Refuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickPolicy {
    /// Before tick 0 → tick 0 (length kept).
    Clamp,
    Discard,
    /// Shift the whole result so its earliest note starts at 0 ("Expand").
    Shift,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LenPolicy {
    /// Length ≤ 0 → 1 tick.
    MinOne,
    Discard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boundary {
    pub pitch: PitchPolicy,
    pub tick: TickPolicy,
    pub len: LenPolicy,
    /// Allowed pitch range (inclusive). Standard MIDI: 0–127; 0–255 in a
    /// 256-Key Mode project ([`Boundary::within`]).
    pub lo: u8,
    pub hi: u8,
}

impl Default for Boundary {
    fn default() -> Self {
        Boundary {
            pitch: PitchPolicy::Discard,
            tick: TickPolicy::Clamp,
            len: LenPolicy::MinOne,
            lo: 0,
            hi: MAX_PITCH,
        }
    }
}

pub const PITCH_POLICIES: &[&str] = &[
    "Discard",
    "Clamp",
    "Wrap by octave",
    "Wrap around",
    "Reflect",
    "Refuse the operation",
];
pub const TICK_POLICIES: &[&str] = &["Clamp to 0", "Discard", "Shift all (expand)"];
pub const LEN_POLICIES: &[&str] = &["At least 1 tick", "Discard"];

/// The three standard boundary controls (keys `b.pitch`, `b.tick`, `b.len`).
pub fn specs(pitch_default: PitchPolicy) -> Vec<ParamSpec> {
    vec![
        choice(
            "b.pitch",
            "Outside the key range",
            PITCH_POLICIES,
            pitch_default as u32,
        )
        .help("What happens to notes moved outside the project's keys (0–127, or 0–255 in 256-Key Mode)."),
        choice("b.tick", "Before tick 0", TICK_POLICIES, 0).help(
            "What happens to notes moved before the start of the song: clamp them to tick 0, discard them, or shift the whole result later so its earliest note starts at 0.",
        ),
        choice("b.len", "Length ≤ 0", LEN_POLICIES, 0).help(
            "What happens to notes shortened to zero or less: keep them 1 tick long, or discard them.",
        ),
    ]
}

impl Boundary {
    /// The policy for a project of key range `keys`: the standard upper
    /// limit (127) becomes the range's highest key; a narrower custom range
    /// stays.
    pub fn within(self, keys: crate::model::project::KeyRange) -> Boundary {
        let hi = if self.hi == MAX_PITCH {
            keys.max_pitch()
        } else {
            self.hi.min(keys.max_pitch())
        };
        Boundary { hi, ..self }
    }

    pub fn from_params(p: &Params) -> Boundary {
        Boundary {
            pitch: match p.choice("b.pitch") {
                1 => PitchPolicy::Clamp,
                2 => PitchPolicy::WrapOctave,
                3 => PitchPolicy::WrapAround,
                4 => PitchPolicy::Reflect,
                5 => PitchPolicy::Refuse,
                _ => PitchPolicy::Discard,
            },
            tick: match p.choice("b.tick") {
                1 => TickPolicy::Discard,
                2 => TickPolicy::Shift,
                _ => TickPolicy::Clamp,
            },
            len: match p.choice("b.len") {
                1 => LenPolicy::Discard,
                _ => LenPolicy::MinOne,
            },
            lo: 0,
            hi: MAX_PITCH,
        }
    }

    /// The pitch after the policy, or `None` (discard). `changed` reports
    /// whether the policy had to act.
    pub fn pitch(&self, p: i64) -> (Option<u8>, bool) {
        let lo = i64::from(self.lo);
        let hi = i64::from(self.hi.max(self.lo));
        if (lo..=hi).contains(&p) {
            return (Some(p as u8), false);
        }
        let size = hi - lo + 1;
        let v = match self.pitch {
            PitchPolicy::Discard | PitchPolicy::Refuse => return (None, true),
            PitchPolicy::Clamp => p.clamp(lo, hi),
            PitchPolicy::WrapAround => lo + (p - lo).rem_euclid(size),
            PitchPolicy::WrapOctave => {
                if size < 12 {
                    p.clamp(lo, hi)
                } else if p > hi {
                    let k = (p - hi + 11) / 12;
                    p - 12 * k
                } else {
                    let k = (lo - p + 11) / 12;
                    p + 12 * k
                }
            }
            PitchPolicy::Reflect => {
                if size == 1 {
                    lo
                } else {
                    let period = 2 * (size - 1);
                    let m = (p - lo).rem_euclid(period);
                    lo + if m < size { m } else { period - m }
                }
            }
        };
        (Some(v.clamp(lo, hi) as u8), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pitch_policy_lands_in_range_or_discards() {
        for pol in [
            PitchPolicy::Discard,
            PitchPolicy::Clamp,
            PitchPolicy::WrapOctave,
            PitchPolicy::WrapAround,
            PitchPolicy::Reflect,
            PitchPolicy::Refuse,
        ] {
            let b = Boundary {
                pitch: pol,
                ..Boundary::default()
            };
            for p in -1000..1000 {
                let (v, changed) = b.pitch(p);
                assert_eq!(changed, !(0..=127).contains(&p));
                if let Some(v) = v {
                    assert!(v <= 127);
                } else {
                    assert!(matches!(pol, PitchPolicy::Discard | PitchPolicy::Refuse));
                }
            }
        }
        let b = |pitch| Boundary {
            pitch,
            ..Boundary::default()
        };
        assert_eq!(b(PitchPolicy::WrapOctave).pitch(130).0, Some(118));
        assert_eq!(b(PitchPolicy::WrapOctave).pitch(-1).0, Some(11));
        assert_eq!(b(PitchPolicy::WrapAround).pitch(128).0, Some(0));
        assert_eq!(b(PitchPolicy::Reflect).pitch(128).0, Some(126));
        assert_eq!(b(PitchPolicy::Reflect).pitch(-2).0, Some(2));
        assert_eq!(b(PitchPolicy::Clamp).pitch(-50).0, Some(0));
    }
}
