//! MIDI time division and snap arithmetic.
//!
//! All authoritative time in PlumSpirit is integer MIDI ticks (`u64`). Floating point
//! is only used for converting *relative* tick offsets into screen pixels.

/// Absolute MIDI tick position. Documentational alias; always an integer tick count.
pub type Tick = u64;

/// The largest delta time a Standard MIDI File can store in one variable-length
/// quantity (four 7-bit groups).
pub const MAX_DELTA_TICKS: u64 = 0x0FFF_FFFF;

/// Largest native PPQN a PlumSpirit project may use (2^31 − 1). This is a limit of
/// the numeric representation, not a musical ceiling: every tick computation stays
/// in checked 64/128-bit arithmetic at this resolution.
pub const MAX_NATIVE_PPQN: u32 = 0x7FFF_FFFF;

/// Largest PPQN a Standard MIDI File header can store (15 bits).
pub const MAX_SMF_PPQN: u32 = 0x7FFF;

/// Time division of a document.
///
/// Imported files keep their 16-bit `MThd` word exactly. Native projects may use a
/// PPQN larger than a MIDI header can hold ([`MAX_SMF_PPQN`]); such a project needs
/// an explicit conversion when it is exported to MIDI (see `midi::export`).
///
/// Encoding: bit 31 set = SMPTE (the raw 16-bit word in the low bits), otherwise the
/// value is the PPQN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Division(u32);

/// Interpretation of a [`Division`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingKind {
    /// Metrical timing: ticks per quarter note.
    Ppqn(u32),
    /// SMPTE timing: `frames_per_second` (24, 25, 29 = 29.97 drop-frame, 30) and
    /// ticks per frame.
    Smpte {
        frames_per_second: u8,
        ticks_per_frame: u8,
    },
}

const SMPTE_FLAG: u32 = 1 << 31;

impl Division {
    /// From the 16-bit `MThd` division word.
    pub const fn from_raw(raw: u16) -> Self {
        if raw & 0x8000 != 0 {
            Division(SMPTE_FLAG | raw as u32)
        } else {
            Division(raw as u32)
        }
    }

    /// Metrical division with `ppqn` ticks per quarter note (values above
    /// [`MAX_NATIVE_PPQN`] are clamped to it).
    pub const fn ppqn(ppqn: u32) -> Self {
        Division(if ppqn > MAX_NATIVE_PPQN {
            MAX_NATIVE_PPQN
        } else {
            ppqn
        })
    }

    /// Stable 32-bit encoding used by fingerprints and the native project format.
    #[inline]
    pub const fn encoded(self) -> u32 {
        self.0
    }

    /// Inverse of [`Division::encoded`]; `None` for invalid words.
    pub const fn from_encoded(v: u32) -> Option<Self> {
        if v & SMPTE_FLAG != 0 {
            if v & !SMPTE_FLAG > 0xFFFF || v & 0x8000 == 0 {
                return None;
            }
            Some(Division(v))
        } else {
            Some(Division(v))
        }
    }

    /// The 16-bit word a Standard MIDI File header stores, or `None` when the native
    /// PPQN is too large for the header.
    pub const fn smf_word(self) -> Option<u16> {
        if self.0 & SMPTE_FLAG != 0 {
            Some((self.0 & 0xFFFF) as u16)
        } else if self.0 <= MAX_SMF_PPQN {
            Some(self.0 as u16)
        } else {
            None
        }
    }

    pub fn kind(self) -> TimingKind {
        if self.0 & SMPTE_FLAG != 0 {
            let raw = (self.0 & 0xFFFF) as u16;
            // High byte is the negative frame rate in two's complement. Widen before
            // negating so a (malformed) 0x80 byte cannot overflow.
            let hi = i16::from((raw >> 8) as u8 as i8);
            let fps = (-hi) as u8;
            TimingKind::Smpte {
                frames_per_second: fps,
                ticks_per_frame: (raw & 0xFF) as u8,
            }
        } else {
            TimingKind::Ppqn(self.0)
        }
    }

    /// Ticks per quarter note when timing is metrical and non-zero.
    pub fn musical_ppqn(self) -> Option<u64> {
        match self.kind() {
            TimingKind::Ppqn(p) if p > 0 => Some(u64::from(p)),
            _ => None,
        }
    }

    /// Human-readable description, e.g. "960 PPQN" or "SMPTE 25 fps × 40".
    pub fn describe(self) -> String {
        match self.kind() {
            TimingKind::Ppqn(p) => format!("{} PPQN", crate::midi::group_digits(u64::from(p))),
            TimingKind::Smpte {
                frames_per_second,
                ticks_per_frame,
            } => format!("SMPTE {frames_per_second} fps × {ticks_per_frame} ticks"),
        }
    }
}

/// Parses a user-entered PPQN: a whole number from 1 to [`MAX_NATIVE_PPQN`].
pub fn parse_ppqn(input: &str) -> Result<u32, String> {
    let s = input.trim().replace(['_', ','], "");
    if s.is_empty() {
        return Err("Enter a PPQN (ticks per quarter note), for example 960.".into());
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "\"{}\" is not a whole number. PPQN counts ticks per quarter note, for example 96, 960 or 3840.",
            input.trim()
        ));
    }
    match s.parse::<u64>() {
        Ok(0) => Err("PPQN must be at least 1.".into()),
        Ok(v) if v <= u64::from(MAX_NATIVE_PPQN) => Ok(v as u32),
        _ => Err(format!(
            "PPQN can be at most {} in PlumSpirit.",
            crate::midi::group_digits(u64::from(MAX_NATIVE_PPQN))
        )),
    }
}

/// Snap resolution for manual note placement. Musical values are only meaningful
/// with PPQN timing; `Frame` only with SMPTE timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Snap {
    Off,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    Frame,
    /// v0.7 Custom Snap: an exact whole number of ticks (parsed from ticks or
    /// from an exact fraction of a whole note, see [`parse_custom_snap`]).
    Custom(u64),
}

impl Snap {
    pub const MUSICAL: [Snap; 5] = [
        Snap::Off,
        Snap::Quarter,
        Snap::Eighth,
        Snap::Sixteenth,
        Snap::ThirtySecond,
    ];
    pub const SMPTE: [Snap; 2] = [Snap::Off, Snap::Frame];

    pub fn label(self) -> &'static str {
        match self {
            Snap::Off => "Off",
            Snap::Quarter => "1/4",
            Snap::Eighth => "1/8",
            Snap::Sixteenth => "1/16",
            Snap::ThirtySecond => "1/32",
            Snap::Frame => "Frame",
            Snap::Custom(_) => "Custom",
        }
    }

    /// The label with the custom value ("Custom (160)").
    pub fn display(self) -> String {
        match self {
            Snap::Custom(t) => format!("{} t", crate::midi::group_digits(t)),
            s => s.label().to_string(),
        }
    }

    /// Snap unit in ticks, or `None` when snapping is off or not applicable to the
    /// timing mode. Subdivisions use integer division of the PPQN (never below 1 tick);
    /// e.g. 1/32 at 96 PPQN = 12 ticks, at 100 PPQN = 12 ticks (100/8 rounded down).
    pub fn unit_ticks(self, division: Division) -> Option<u64> {
        match (self, division.kind()) {
            (Snap::Off, _) => None,
            (Snap::Custom(t), _) => Some(t.max(1)),
            (
                Snap::Frame,
                TimingKind::Smpte {
                    ticks_per_frame, ..
                },
            ) => Some(u64::from(ticks_per_frame).max(1)),
            (Snap::Frame, TimingKind::Ppqn(_)) => None,
            (_, TimingKind::Smpte { .. }) => None,
            (s, TimingKind::Ppqn(p)) => {
                if p == 0 {
                    return None;
                }
                let div = match s {
                    Snap::Quarter => 1,
                    Snap::Eighth => 2,
                    Snap::Sixteenth => 4,
                    Snap::ThirtySecond => 8,
                    Snap::Off | Snap::Frame | Snap::Custom(_) => unreachable!("handled above"),
                };
                Some((u64::from(p) / div).max(1))
            }
        }
    }

    /// Whether this snap choice is valid for the timing mode.
    pub fn applies_to(self, division: Division) -> bool {
        self == Snap::Off || self.unit_ticks(division).is_some()
    }
}

/// Largest Custom Snap (ticks).
pub const MAX_CUSTOM_SNAP: u64 = 1 << 40;

/// Parses a Custom Snap (v0.7, contract §138) into an exact tick count, never
/// through floating point:
/// * `160`, `160 t`, `160 ticks` — ticks;
/// * `a/b` — that fraction of a whole note (`4 × PPQN × a / b`), e.g. `1/12`
///   (eighth triplets), `3/16`; a trailing `T` makes it a triplet (× 2/3), e.g.
///   `1/8T`; a trailing `.` makes it dotted (× 3/2), e.g. `1/8.`.
///
/// Fractions must give a whole number of ticks at `division`; otherwise the
/// error says what the value would be. SMPTE timing accepts ticks only.
pub fn parse_custom_snap(text: &str, division: Division) -> Result<u64, String> {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() {
        return Err("Enter a snap value: ticks (e.g. 160) or a fraction of a whole note (e.g. 1/12, 1/8T, 3/16).".into());
    }
    let ticks_part = t
        .strip_suffix("ticks")
        .or_else(|| t.strip_suffix("tick"))
        .map(str::trim);
    let plain = |s: &str| -> Option<u64> {
        s.chars()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())
            .flatten()
    };
    let out_of_range = || {
        format!(
            "A snap must be 1 to {} ticks.",
            crate::midi::group_digits(MAX_CUSTOM_SNAP)
        )
    };
    if let Some(v) = ticks_part
        .and_then(plain)
        .or_else(|| plain(&t))
        .or_else(|| {
            t.strip_suffix('t')
                .map(str::trim)
                .filter(|s| !s.contains('/'))
                .and_then(plain)
        })
    {
        return if (1..=MAX_CUSTOM_SNAP).contains(&v) {
            Ok(v)
        } else {
            Err(out_of_range())
        };
    }
    let Some((a, b)) = t.split_once('/') else {
        return Err(format!(
            "“{}” is not a snap value: use ticks (e.g. 160) or a fraction (e.g. 1/12).",
            text.trim()
        ));
    };
    let (b, num_mul, den_mul) = if let Some(x) = b.trim().strip_suffix('t') {
        (x, 2u128, 3u128)
    } else if let Some(x) = b.trim().strip_suffix('.') {
        (x, 3, 2)
    } else {
        (b, 1, 1)
    };
    let (Some(a), Some(b)) = (plain(a.trim()), plain(b.trim())) else {
        return Err(format!(
            "“{}” is not a fraction of whole numbers.",
            text.trim()
        ));
    };
    if a == 0 || b == 0 {
        return Err("The fraction must be greater than zero.".into());
    }
    let Some(ppq) = division.musical_ppqn() else {
        return Err("This project uses SMPTE timing: enter the snap in ticks.".into());
    };
    let num = 4u128 * u128::from(ppq) * u128::from(a) * num_mul;
    let den = u128::from(b) * den_mul;
    if !num.is_multiple_of(den) {
        let whole = num / den;
        let frac = (num % den) * 100 / den;
        return Err(format!(
            "{} of a whole note is {}.{:02} ticks at {} PPQN, not a whole number of ticks. Choose another fraction or enter ticks.",
            text.trim(),
            whole,
            frac,
            ppq
        ));
    }
    let v = u64::try_from(num / den).map_err(|_| out_of_range())?;
    if (1..=MAX_CUSTOM_SNAP).contains(&v) {
        Ok(v)
    } else {
        Err(out_of_range())
    }
}

/// Rounds `tick` down to a multiple of `unit`.
#[inline]
pub fn snap_floor(tick: u64, unit: u64) -> u64 {
    if unit <= 1 { tick } else { tick - tick % unit }
}

/// Rounds `tick` to the nearest multiple of `unit` (ties up), saturating at the
/// largest multiple that fits in `u64`.
pub fn snap_nearest(tick: u64, unit: u64) -> u64 {
    if unit <= 1 {
        return tick;
    }
    let down = snap_floor(tick, unit);
    let rem = tick - down;
    if rem * 2 >= unit {
        down.checked_add(unit).unwrap_or(down)
    } else {
        down
    }
}

/// Default length of a note created by a click without drag: one snap unit, or when
/// snap is off a sixteenth note (PPQN) / one frame (SMPTE) / 1 tick if neither applies.
pub fn default_note_length(division: Division, snap: Snap) -> u64 {
    if let Some(u) = snap.unit_ticks(division) {
        return u;
    }
    match division.kind() {
        TimingKind::Ppqn(p) if p > 0 => (u64::from(p) / 4).max(1),
        TimingKind::Smpte {
            ticks_per_frame, ..
        } => u64::from(ticks_per_frame).max(1),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_snap_parses_ticks_and_exact_fractions_only() {
        let d = Division::ppqn(960);
        assert_eq!(parse_custom_snap("160", d), Ok(160));
        assert_eq!(parse_custom_snap(" 160 ticks ", d), Ok(160));
        assert_eq!(parse_custom_snap("160t", d), Ok(160));
        assert_eq!(parse_custom_snap("1/12", d), Ok(320));
        assert_eq!(parse_custom_snap("1/8T", d), Ok(320));
        assert_eq!(parse_custom_snap("3/16", d), Ok(720));
        assert_eq!(parse_custom_snap("1/8.", d), Ok(720));
        assert!(parse_custom_snap("1/7", d).unwrap_err().contains("548.57"));
        assert!(parse_custom_snap("0", d).is_err());
        assert!(parse_custom_snap("abc", d).is_err());
        assert!(parse_custom_snap("1/0", d).is_err());
        assert_eq!(Snap::Custom(320).unit_ticks(d), Some(320));
        // SMPTE: ticks only.
        let s = Division::from_raw(0xE728);
        assert!(parse_custom_snap("1/4", s).is_err());
        assert_eq!(parse_custom_snap("40", s), Ok(40));
    }

    #[test]
    fn division_kinds() {
        assert_eq!(Division::from_raw(960).kind(), TimingKind::Ppqn(960));
        // -25 fps, 40 ticks per frame => 0xE728
        let d = Division::from_raw(0xE728);
        assert_eq!(
            d.kind(),
            TimingKind::Smpte {
                frames_per_second: 25,
                ticks_per_frame: 40
            }
        );
        assert_eq!(d.musical_ppqn(), None);
        let d30 = Division::from_raw(0xE250);
        assert_eq!(
            d30.kind(),
            TimingKind::Smpte {
                frames_per_second: 30,
                ticks_per_frame: 0x50
            }
        );
        assert_eq!(Division::from_raw(0).musical_ppqn(), None);
        // Native PPQN beyond the MIDI header range.
        let big = Division::ppqn(100_000);
        assert_eq!(big.kind(), TimingKind::Ppqn(100_000));
        assert_eq!(big.smf_word(), None);
        assert_eq!(Division::ppqn(32_767).smf_word(), Some(32_767));
        assert_eq!(d.smf_word(), Some(0xE728));
        for x in [Division::ppqn(1), big, d, Division::ppqn(MAX_NATIVE_PPQN)] {
            assert_eq!(Division::from_encoded(x.encoded()), Some(x));
        }
        assert_eq!(Division::from_encoded(SMPTE_FLAG | 0x10000), None);
        assert_eq!(Division::from_encoded(SMPTE_FLAG | 0x0100), None);
    }

    #[test]
    fn ppqn_input() {
        assert_eq!(parse_ppqn("960"), Ok(960));
        assert_eq!(parse_ppqn(" 1,000,000 "), Ok(1_000_000));
        assert_eq!(parse_ppqn("2147483647"), Ok(MAX_NATIVE_PPQN));
        for bad in [
            "",
            "0",
            "-5",
            "96.5",
            "abc",
            "2147483648",
            "1e3",
            "99999999999999999999999",
        ] {
            assert!(parse_ppqn(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn snap_units() {
        let d = Division::ppqn(960);
        assert_eq!(Snap::Quarter.unit_ticks(d), Some(960));
        assert_eq!(Snap::Sixteenth.unit_ticks(d), Some(240));
        assert_eq!(Snap::ThirtySecond.unit_ticks(d), Some(120));
        assert_eq!(Snap::Off.unit_ticks(d), None);
        assert_eq!(Snap::Frame.unit_ticks(d), None);
        assert_eq!(Snap::ThirtySecond.unit_ticks(Division::ppqn(3)), Some(1));
        let s = Division::from_raw(0xE728);
        assert_eq!(Snap::Frame.unit_ticks(s), Some(40));
        assert_eq!(Snap::Quarter.unit_ticks(s), None);
        assert_eq!(default_note_length(d, Snap::Off), 240);
        assert_eq!(default_note_length(s, Snap::Off), 40);
    }

    #[test]
    fn snapping() {
        assert_eq!(snap_floor(250, 240), 240);
        assert_eq!(snap_nearest(359, 240), 240);
        assert_eq!(snap_nearest(360, 240), 480);
        assert_eq!(snap_nearest(u64::MAX, 240), u64::MAX - u64::MAX % 240);
        assert_eq!(snap_nearest(17, 1), 17);
    }
}
