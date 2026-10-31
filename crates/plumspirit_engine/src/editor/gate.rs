//! Note-length (gate) presets for newly drawn notes.
//!
//! Given the PPQN `P` (ticks per quarter note): whole = 4P, half = 2P, quarter =
//! P, eighth = P/2, sixteenth = P/4, thirty-second = P/8; dotted × 3/2, triplet
//! × 2/3. Each length is the exact fraction of `P`, rounded half up to a whole
//! tick and never below one tick (so a 1/32 triplet at 96 PPQN is exactly 8
//! ticks, an eighth triplet at 1000 PPQN is 333, and a 1/32 at 5 PPQN is 1). **Measure** is the length of the measure at the
//! insertion point, from the Conductor's time signatures (an exact integer; a
//! measure shortened by a signature change is that short).

use crate::model::timing::{Division, Snap, TimingKind, default_note_length};
use crate::view::grid::MeterMap;

/// A musical note value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GatePreset {
    Measure,
    Whole,
    DottedHalf,
    Half,
    HalfTriplet,
    DottedQuarter,
    Quarter,
    QuarterTriplet,
    DottedEighth,
    Eighth,
    EighthTriplet,
    DottedSixteenth,
    Sixteenth,
    SixteenthTriplet,
    DottedThirtySecond,
    ThirtySecond,
    ThirtySecondTriplet,
}

impl GatePreset {
    pub const ALL: [GatePreset; 17] = [
        GatePreset::Measure,
        GatePreset::Whole,
        GatePreset::DottedHalf,
        GatePreset::Half,
        GatePreset::HalfTriplet,
        GatePreset::DottedQuarter,
        GatePreset::Quarter,
        GatePreset::QuarterTriplet,
        GatePreset::DottedEighth,
        GatePreset::Eighth,
        GatePreset::EighthTriplet,
        GatePreset::DottedSixteenth,
        GatePreset::Sixteenth,
        GatePreset::SixteenthTriplet,
        GatePreset::DottedThirtySecond,
        GatePreset::ThirtySecond,
        GatePreset::ThirtySecondTriplet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GatePreset::Measure => "Measure",
            GatePreset::Whole => "Whole Note",
            GatePreset::DottedHalf => "Dotted Half Note",
            GatePreset::Half => "Half Note",
            GatePreset::HalfTriplet => "Half Note Triplet",
            GatePreset::DottedQuarter => "Dotted Quarter Note",
            GatePreset::Quarter => "Quarter Note",
            GatePreset::QuarterTriplet => "Quarter Note Triplet",
            GatePreset::DottedEighth => "Dotted Eighth Note",
            GatePreset::Eighth => "Eighth Note",
            GatePreset::EighthTriplet => "Eighth Note Triplet",
            GatePreset::DottedSixteenth => "Dotted Sixteenth Note",
            GatePreset::Sixteenth => "Sixteenth Note",
            GatePreset::SixteenthTriplet => "Sixteenth Note Triplet",
            GatePreset::DottedThirtySecond => "Dotted Thirty-Second Note",
            GatePreset::ThirtySecond => "Thirty-Second Note",
            GatePreset::ThirtySecondTriplet => "Thirty-Second Note Triplet",
        }
    }

    /// Stable name for settings files.
    pub fn key(self) -> &'static str {
        match self {
            GatePreset::Measure => "measure",
            GatePreset::Whole => "1",
            GatePreset::DottedHalf => "2.",
            GatePreset::Half => "2",
            GatePreset::HalfTriplet => "2t",
            GatePreset::DottedQuarter => "4.",
            GatePreset::Quarter => "4",
            GatePreset::QuarterTriplet => "4t",
            GatePreset::DottedEighth => "8.",
            GatePreset::Eighth => "8",
            GatePreset::EighthTriplet => "8t",
            GatePreset::DottedSixteenth => "16.",
            GatePreset::Sixteenth => "16",
            GatePreset::SixteenthTriplet => "16t",
            GatePreset::DottedThirtySecond => "32.",
            GatePreset::ThirtySecond => "32",
            GatePreset::ThirtySecondTriplet => "32t",
        }
    }

    pub fn from_key(k: &str) -> Option<GatePreset> {
        GatePreset::ALL.into_iter().find(|p| p.key() == k)
    }

    /// Length in quarter notes as `(numerator, denominator)`; `None` for Measure.
    pub fn quarters(self) -> Option<(u64, u64)> {
        Some(match self {
            GatePreset::Measure => return None,
            GatePreset::Whole => (4, 1),
            GatePreset::DottedHalf => (3, 1),
            GatePreset::Half => (2, 1),
            GatePreset::HalfTriplet => (4, 3),
            GatePreset::DottedQuarter => (3, 2),
            GatePreset::Quarter => (1, 1),
            GatePreset::QuarterTriplet => (2, 3),
            GatePreset::DottedEighth => (3, 4),
            GatePreset::Eighth => (1, 2),
            GatePreset::EighthTriplet => (1, 3),
            GatePreset::DottedSixteenth => (3, 8),
            GatePreset::Sixteenth => (1, 4),
            GatePreset::SixteenthTriplet => (1, 6),
            GatePreset::DottedThirtySecond => (3, 16),
            GatePreset::ThirtySecond => (1, 8),
            GatePreset::ThirtySecondTriplet => (1, 12),
        })
    }

    /// Length in ticks at `ppqn`, for a note inserted at `at` (Measure uses the
    /// measure there). `None` without musical timing.
    pub fn ticks(self, meter: &MeterMap, at: u64) -> Option<u64> {
        let p = meter.ppqn()?;
        match self.quarters() {
            None => meter.measure_length_at(at).map(|l| l.max(1)),
            Some((n, d)) => {
                let v = (u128::from(p) * u128::from(n) * 2 + u128::from(d)) / (2 * u128::from(d));
                Some(u64::try_from(v).unwrap_or(u64::MAX).max(1))
            }
        }
    }

    /// The tick length is not an exact fraction of the PPQN (it was rounded).
    pub fn is_rounded(self, ppqn: u64) -> bool {
        self.quarters()
            .is_some_and(|(n, d)| !(u128::from(ppqn) * u128::from(n)).is_multiple_of(u128::from(d)))
    }
}

/// The length of newly drawn notes (one click, no drag).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DefaultGate {
    /// The snap grid's unit (v0.3 behaviour).
    #[default]
    Grid,
    Preset(GatePreset),
    /// A typed length.
    Ticks(u64),
}

impl DefaultGate {
    /// Length for a note drawn at `at`.
    pub fn ticks(self, division: Division, snap: Snap, meter: &MeterMap, at: u64) -> u64 {
        match self {
            DefaultGate::Grid => default_note_length(division, snap),
            DefaultGate::Ticks(t) => t.max(1),
            DefaultGate::Preset(p) => p
                .ticks(meter, at)
                .unwrap_or_else(|| default_note_length(division, snap)),
        }
    }

    pub fn label(self) -> String {
        match self {
            DefaultGate::Grid => "Grid (follows Snap)".into(),
            DefaultGate::Preset(p) => p.label().into(),
            DefaultGate::Ticks(t) => format!("Custom: {} ticks", crate::midi::group_digits(t)),
        }
    }

    /// Settings text ("grid", "preset:8t", "ticks:240").
    pub fn to_key(self) -> String {
        match self {
            DefaultGate::Grid => "grid".into(),
            DefaultGate::Preset(p) => format!("preset:{}", p.key()),
            DefaultGate::Ticks(t) => format!("ticks:{t}"),
        }
    }

    pub fn from_key(s: &str) -> Option<DefaultGate> {
        if s == "grid" {
            return Some(DefaultGate::Grid);
        }
        if let Some(k) = s.strip_prefix("preset:") {
            return GatePreset::from_key(k).map(DefaultGate::Preset);
        }
        s.strip_prefix("ticks:")
            .and_then(|t| t.parse::<u64>().ok())
            .filter(|&t| t >= 1)
            .map(DefaultGate::Ticks)
    }
}

/// PPQN of a division, if metrical.
pub fn ppqn_of(division: Division) -> Option<u64> {
    match division.kind() {
        TimingKind::Ppqn(p) if p > 0 => Some(u64::from(p)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_follow_the_ppqn_with_documented_rounding() {
        for ppqn in [96u32, 480, 960, 1_000_000, 5] {
            let m = MeterMap::new(Division::ppqn(ppqn), &[(0, 4, 2)]);
            let p = u64::from(ppqn);
            let t = |g: GatePreset| g.ticks(&m, 0).unwrap();
            let r = |n: u64, d: u64| ((p * n * 2 + d) / (2 * d)).max(1);
            assert_eq!(t(GatePreset::Quarter), p);
            assert_eq!(t(GatePreset::Whole), 4 * p);
            assert_eq!(t(GatePreset::Half), 2 * p);
            assert_eq!(t(GatePreset::DottedHalf), 3 * p);
            assert_eq!(t(GatePreset::Eighth), r(1, 2));
            assert_eq!(t(GatePreset::Sixteenth), r(1, 4));
            assert_eq!(t(GatePreset::ThirtySecond), r(1, 8));
            assert_eq!(t(GatePreset::DottedQuarter), r(3, 2));
            assert_eq!(t(GatePreset::QuarterTriplet), r(2, 3));
            assert_eq!(t(GatePreset::EighthTriplet), r(1, 3));
            assert_eq!(t(GatePreset::SixteenthTriplet), r(1, 6));
            assert_eq!(t(GatePreset::ThirtySecondTriplet), r(1, 12));
            assert_eq!(t(GatePreset::HalfTriplet), r(4, 3));
            assert_eq!(t(GatePreset::DottedEighth), r(3, 4));
            assert_eq!(t(GatePreset::DottedSixteenth), r(3, 8));
            assert_eq!(t(GatePreset::DottedThirtySecond), r(3, 16));
            assert_eq!(t(GatePreset::Measure), 4 * p);
            for g in GatePreset::ALL {
                assert!(t(g) >= 1, "{g:?} at {ppqn}");
                assert_eq!(GatePreset::from_key(g.key()), Some(g));
            }
        }
        // 96 PPQN: 1/32 triplet = 8 exactly; 5 PPQN: 1/32 = 0.625 → 1.
        let m96 = MeterMap::new(Division::ppqn(96), &[]);
        assert_eq!(GatePreset::ThirtySecondTriplet.ticks(&m96, 0), Some(8));
        assert!(!GatePreset::ThirtySecondTriplet.is_rounded(96));
        let m5 = MeterMap::new(Division::ppqn(5), &[]);
        assert_eq!(GatePreset::ThirtySecond.ticks(&m5, 0), Some(1));
        assert!(GatePreset::ThirtySecond.is_rounded(5));
        // 1000 PPQN eighth triplet: 333.33 → 333.
        let m1k = MeterMap::new(Division::ppqn(1000), &[]);
        assert_eq!(GatePreset::EighthTriplet.ticks(&m1k, 0), Some(333));
        assert!(GatePreset::EighthTriplet.is_rounded(1000));
    }

    #[test]
    fn measure_follows_the_signature_at_the_insertion_point() {
        let m = MeterMap::new(
            Division::ppqn(480),
            &[(0, 4, 2), (1920, 7, 3), (1920 + 1680 + 100, 3, 4)],
        );
        assert_eq!(GatePreset::Measure.ticks(&m, 100), Some(1920));
        assert_eq!(GatePreset::Measure.ticks(&m, 2000), Some(1680));
        // The measure cut short by a signature change.
        assert_eq!(GatePreset::Measure.ticks(&m, 1920 + 1680 + 50), Some(100));
        // 3/16 (denominator power 4) = 3 × 120.
        assert_eq!(GatePreset::Measure.ticks(&m, 1920 + 1680 + 150), Some(360));
        let smpte = MeterMap::new(Division::from_raw(0xE728), &[]);
        assert_eq!(GatePreset::Quarter.ticks(&smpte, 0), None);
        let d = DefaultGate::Preset(GatePreset::Quarter);
        assert_eq!(
            d.ticks(Division::from_raw(0xE728), Snap::Off, &smpte, 0),
            40
        );
    }

    #[test]
    fn default_gate_settings_round_trip() {
        for g in [
            DefaultGate::Grid,
            DefaultGate::Ticks(240),
            DefaultGate::Preset(GatePreset::DottedSixteenth),
            DefaultGate::Preset(GatePreset::Measure),
        ] {
            assert_eq!(DefaultGate::from_key(&g.to_key()), Some(g));
        }
        assert_eq!(DefaultGate::from_key("ticks:0"), None);
        assert_eq!(DefaultGate::from_key("preset:7"), None);
    }
}
