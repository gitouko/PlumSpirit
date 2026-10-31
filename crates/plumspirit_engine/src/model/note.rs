//! Compact per-note attributes.
//!
//! Every note in PlumSpirit is stored as three primitive values inside a note chunk:
//! `start_tick: u64`, `duration_ticks: u64` and one packed [`NoteAttrs`] `u32`.
//! No note ever owns a heap allocation.

use std::fmt;

/// Highest valid MIDI pitch / data byte value.
pub const MAX_PITCH: u8 = 127;
/// Highest pitch of a project in 256-Key Mode (v0.9 §036): the full note
/// byte. Pitches above [`MAX_PITCH`] are outside standard MIDI.
pub const MAX_PITCH_256: u8 = 255;
/// Highest valid MIDI velocity / data byte value.
pub const MAX_VELOCITY: u8 = 127;
/// Highest valid zero-based MIDI channel (channel 16 in user-facing numbering).
pub const MAX_CHANNEL: u8 = 15;

/// How a note's termination was (or will be) encoded in the MIDI stream.
///
/// Standard MIDI Files allow two spellings of "note off": a real Note Off message
/// (`0x8n key vel`) or a Note On with velocity zero (`0x9n key 0`). Black MIDI files
/// frequently use the latter together with running status because it is smaller, so
/// the importer records which spelling was used and the exporter reproduces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteOffKind {
    /// `0x8n key off_velocity`
    NoteOff,
    /// `0x9n key 0` (the off velocity is implicitly 0)
    NoteOnZero,
}

// Bit layout of the packed attribute word (bits 28..=31 are reserved and always 0;
// bits 30 and 31 are used as markers by compact encodings, never in real attributes):
//
//   bits  0..=6   pitch, low 7 bits (0..=127)
//   bits  7..=13  velocity         (1..=127 for real notes)
//   bits 14..=17  channel          (0..=15)
//   bits 18..=24  note-off velocity (0..=127, meaningful only for NoteOff)
//   bit  25       off kind         (0 = NoteOff, 1 = NoteOn velocity 0)
//   bit  26       muted            (v0.7: kept in the project, never played or exported)
//   bit  27       pitch bit 7      (v0.9 256-Key Mode: pitch 128..=255; 0 for MIDI pitches,
//                                   so every v0.8 word is unchanged)
const PITCH_SHIFT: u32 = 0;
const VELOCITY_SHIFT: u32 = 7;
const CHANNEL_SHIFT: u32 = 14;
const OFF_VELOCITY_SHIFT: u32 = 18;
const OFF_KIND_SHIFT: u32 = 25;
const MUTED_SHIFT: u32 = 26;
const PITCH_HIGH_SHIFT: u32 = 27;
const MASK7: u32 = 0x7F;
const MASK4: u32 = 0x0F;
const USED_BITS: u32 = (1 << 28) - 1;
const PITCH_BITS: u32 = (MASK7 << PITCH_SHIFT) | (1 << PITCH_HIGH_SHIFT);

/// The packed pitch bits of `pitch` (0..=255).
#[inline]
fn pitch_bits(pitch: u8) -> u32 {
    (u32::from(pitch & 0x7F) << PITCH_SHIFT) | (u32::from(pitch >> 7) << PITCH_HIGH_SHIFT)
}

/// Packed pitch / velocity / channel / note-off information for one note.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct NoteAttrs(u32);

/// Error returned when note attributes are outside their MIDI ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteAttrsError {
    Pitch(u8),
    Velocity(u8),
    Channel(u8),
    OffVelocity(u8),
}

impl fmt::Display for NoteAttrsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoteAttrsError::Pitch(v) => write!(f, "pitch {v} is outside 0..=127"),
            NoteAttrsError::Velocity(v) => write!(f, "velocity {v} is outside 1..=127"),
            NoteAttrsError::Channel(v) => write!(f, "channel index {v} is outside 0..=15"),
            NoteAttrsError::OffVelocity(v) => {
                write!(f, "note-off velocity {v} is outside 0..=127")
            }
        }
    }
}

impl std::error::Error for NoteAttrsError {}

impl NoteAttrs {
    /// Builds packed attributes, validating every field.
    ///
    /// `velocity` must be 1..=127 because a Note On with velocity 0 is a note-off.
    pub fn new(
        pitch: u8,
        velocity: u8,
        channel: u8,
        off_velocity: u8,
        off_kind: NoteOffKind,
    ) -> Result<Self, NoteAttrsError> {
        if pitch > MAX_PITCH {
            return Err(NoteAttrsError::Pitch(pitch));
        }
        if velocity == 0 || velocity > MAX_VELOCITY {
            return Err(NoteAttrsError::Velocity(velocity));
        }
        if channel > MAX_CHANNEL {
            return Err(NoteAttrsError::Channel(channel));
        }
        if off_velocity > MAX_VELOCITY {
            return Err(NoteAttrsError::OffVelocity(off_velocity));
        }
        // A NoteOn-velocity-0 termination has no separate off velocity.
        let off_velocity = match off_kind {
            NoteOffKind::NoteOff => off_velocity,
            NoteOffKind::NoteOnZero => 0,
        };
        let kind_bit = match off_kind {
            NoteOffKind::NoteOff => 0,
            NoteOffKind::NoteOnZero => 1,
        };
        Ok(NoteAttrs(
            (u32::from(pitch) << PITCH_SHIFT)
                | (u32::from(velocity) << VELOCITY_SHIFT)
                | (u32::from(channel) << CHANNEL_SHIFT)
                | (u32::from(off_velocity) << OFF_VELOCITY_SHIFT)
                | (kind_bit << OFF_KIND_SHIFT),
        ))
    }

    /// Like [`NoteAttrs::new`] with any pitch 0..=255 (v0.9 256-Key Mode;
    /// pitches above 127 are not standard MIDI — the export boundary decides
    /// what happens to them).
    pub fn new_wide(
        pitch: u8,
        velocity: u8,
        channel: u8,
        off_velocity: u8,
        off_kind: NoteOffKind,
    ) -> Result<Self, NoteAttrsError> {
        let a = NoteAttrs::new(pitch & 0x7F, velocity, channel, off_velocity, off_kind)?;
        Ok(NoteAttrs((a.0 & !PITCH_BITS) | pitch_bits(pitch)))
    }

    /// The same note with another note-off velocity / encoding (a Note On
    /// velocity 0 termination has no off velocity), every other field kept —
    /// what [`NoteAttrs::new_wide`] gives with this note's pitch, velocity and
    /// channel (v1.0-beta1 B1-0161, the importer's note-off path).
    #[inline]
    pub fn with_note_off(
        self,
        off_velocity: u8,
        off_kind: NoteOffKind,
    ) -> Result<Self, NoteAttrsError> {
        if off_velocity > MAX_VELOCITY {
            return Err(NoteAttrsError::OffVelocity(off_velocity));
        }
        let (v, k) = match off_kind {
            NoteOffKind::NoteOff => (u32::from(off_velocity), 0),
            NoteOffKind::NoteOnZero => (0, 1),
        };
        let cleared = self.0 & !((MASK7 << OFF_VELOCITY_SHIFT) | (1 << OFF_KIND_SHIFT));
        Ok(NoteAttrs(
            cleared | (v << OFF_VELOCITY_SHIFT) | (k << OFF_KIND_SHIFT),
        ))
    }

    /// Attributes used for notes drawn by the user in v0.1:
    /// MIDI channel 1 (index 0), velocity 100, Note Off with velocity 0.
    pub fn default_for_pitch(pitch: u8) -> Result<Self, NoteAttrsError> {
        NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff)
    }

    /// Reconstructs attributes from a raw word previously produced by [`NoteAttrs::raw`].
    /// Returns `None` if reserved bits are set or the velocity is zero.
    pub fn from_raw(raw: u32) -> Option<Self> {
        if raw & !USED_BITS != 0 {
            return None;
        }
        let a = NoteAttrs(raw);
        if a.velocity() == 0 {
            return None;
        }
        if a.off_kind() == NoteOffKind::NoteOnZero && a.off_velocity() != 0 {
            return None;
        }
        Some(a)
    }

    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }

    /// The pitch, 0..=255 (above 127 only in 256-Key Mode projects).
    #[inline]
    pub fn pitch(self) -> u8 {
        (((self.0 >> PITCH_SHIFT) & MASK7) | (((self.0 >> PITCH_HIGH_SHIFT) & 1) << 7)) as u8
    }

    /// Whether the pitch is above standard MIDI (128..=255).
    #[inline]
    pub fn is_extended(self) -> bool {
        (self.0 >> PITCH_HIGH_SHIFT) & 1 == 1
    }

    #[inline]
    pub fn velocity(self) -> u8 {
        ((self.0 >> VELOCITY_SHIFT) & MASK7) as u8
    }

    #[inline]
    pub fn channel(self) -> u8 {
        ((self.0 >> CHANNEL_SHIFT) & MASK4) as u8
    }

    #[inline]
    pub fn off_velocity(self) -> u8 {
        ((self.0 >> OFF_VELOCITY_SHIFT) & MASK7) as u8
    }

    #[inline]
    pub fn off_kind(self) -> NoteOffKind {
        if (self.0 >> OFF_KIND_SHIFT) & 1 == 1 {
            NoteOffKind::NoteOnZero
        } else {
            NoteOffKind::NoteOff
        }
    }

    /// Returns a copy with a different MIDI pitch (0..=127). Only the pitch bits
    /// change, so channel, velocities and the off encoding are preserved by
    /// construction.
    #[inline]
    pub fn with_pitch(self, pitch: u8) -> Result<Self, NoteAttrsError> {
        if pitch > MAX_PITCH {
            return Err(NoteAttrsError::Pitch(pitch));
        }
        Ok(self.with_pitch_wide_unchecked(pitch))
    }

    /// Returns a copy with any pitch 0..=255 (256-Key Mode).
    #[inline]
    pub fn with_pitch_wide(self, pitch: u8) -> Result<Self, NoteAttrsError> {
        Ok(self.with_pitch_wide_unchecked(pitch))
    }

    #[inline]
    fn with_pitch_wide_unchecked(self, pitch: u8) -> Self {
        NoteAttrs((self.0 & !PITCH_BITS) | pitch_bits(pitch))
    }
}

impl NoteAttrs {
    /// Whether the note is muted (v0.7 Mute tool): it stays in the project and is
    /// drawn, but never plays, exports or counts in analytics.
    #[inline]
    pub fn muted(self) -> bool {
        (self.0 >> MUTED_SHIFT) & 1 == 1
    }

    /// Returns a copy with the mute flag set or cleared.
    #[inline]
    pub fn with_muted(self, muted: bool) -> Self {
        NoteAttrs((self.0 & !(1 << MUTED_SHIFT)) | (u32::from(muted) << MUTED_SHIFT))
    }

    /// Returns a copy with a different note-on velocity (1..=127). Velocity 0 is
    /// refused because a Note On with velocity 0 means note-off.
    #[inline]
    pub fn with_velocity(self, velocity: u8) -> Result<Self, NoteAttrsError> {
        if velocity == 0 || velocity > MAX_VELOCITY {
            return Err(NoteAttrsError::Velocity(velocity));
        }
        Ok(NoteAttrs(
            (self.0 & !(MASK7 << VELOCITY_SHIFT)) | (u32::from(velocity) << VELOCITY_SHIFT),
        ))
    }

    /// Returns a copy on another MIDI channel (0..=15).
    #[inline]
    pub fn with_channel(self, channel: u8) -> Result<Self, NoteAttrsError> {
        if channel > 15 {
            return Err(NoteAttrsError::Channel(channel));
        }
        Ok(NoteAttrs(
            (self.0 & !(MASK4 << CHANNEL_SHIFT)) | (u32::from(channel) << CHANNEL_SHIFT),
        ))
    }
}

impl fmt::Debug for NoteAttrs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoteAttrs")
            .field("pitch", &self.pitch())
            .field("velocity", &self.velocity())
            .field("channel", &self.channel())
            .field("off_velocity", &self.off_velocity())
            .field("off_kind", &self.off_kind())
            .field("muted", &self.muted())
            .finish()
    }
}

/// Stable identity of a PlumSpirit **subchannel** inside its track.
///
/// Subchannels are application-level groups of notes (each has its own display
/// colour and a *physical* MIDI channel for new notes). They are not MIDI channels:
/// a track may have any number of them, while a note's physical channel stays
/// 0–15 in [`NoteAttrs`]. The id is unique within its track and never reused for a
/// different subchannel while one with that id exists; the user sees it as the
/// subchannel *number* (`id + 1`). The first subchannel of every track is
/// [`SubchannelId::MAIN`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubchannelId(pub u32);

impl SubchannelId {
    /// The default insertion group of a new track ("Subchannel 1").
    pub const MAIN: SubchannelId = SubchannelId(0);

    /// User-facing number (1-based).
    #[inline]
    pub fn number(self) -> u64 {
        u64::from(self.0) + 1
    }

    /// From a user-facing number (1 … 4,294,967,296).
    pub fn from_number(n: u64) -> Option<SubchannelId> {
        n.checked_sub(1)
            .and_then(|v| u32::try_from(v).ok())
            .map(SubchannelId)
    }
}

impl fmt::Display for SubchannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

/// A self-contained note value used when notes move between containers
/// (clipboard, batch insertion, transform jobs). Inside a track, notes are stored in
/// structure-of-arrays chunks instead.
///
/// Packed to 4-byte alignment: 28 bytes instead of 32 (v0.7 added `group`),
/// so bulk buffers of records (prepare / clipboard / undo) stay small. Fields
/// are read by value (no references to the packed `u64` fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C, packed(4))]
pub struct NoteRecord {
    pub start_tick: u64,
    pub duration_ticks: u64,
    pub attrs: NoteAttrs,
    /// The note's PlumSpirit subchannel (not its MIDI channel, which is in `attrs`).
    pub sub: SubchannelId,
    /// The note's group (v0.7; 0 = not grouped). Ids are per track.
    pub group: u32,
}

impl NoteRecord {
    /// A note in the main subchannel.
    #[inline]
    pub fn new(start_tick: u64, duration_ticks: u64, attrs: NoteAttrs) -> Self {
        NoteRecord {
            start_tick,
            duration_ticks,
            attrs,
            sub: SubchannelId::MAIN,
            group: 0,
        }
    }

    /// The same note in another subchannel.
    #[inline]
    pub fn in_sub(self, sub: SubchannelId) -> Self {
        NoteRecord { sub, ..self }
    }

    /// End tick (`start + duration`), or `None` if it would overflow `u64`.
    /// Notes stored in a track always satisfy the no-overflow invariant.
    #[inline]
    pub fn end_tick(&self) -> Option<u64> {
        self.start_tick.checked_add(self.duration_ticks)
    }
}

/// Octave numbering convention used for pitch labels: MIDI note 60 is "C4"
/// (scientific pitch notation). MIDI 0 is therefore "C-1" and 127 is "G9".
pub const MIDDLE_C_OCTAVE: i32 = 4;

const PITCH_CLASS_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// True for pitches that are black keys on a piano keyboard.
#[inline]
pub fn is_black_key(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

/// Label such as `C4` or `F#-1` for a MIDI pitch using the [`MIDDLE_C_OCTAVE`] convention.
pub fn pitch_name(pitch: u8) -> String {
    let octave = i32::from(pitch / 12) - 1 + (MIDDLE_C_OCTAVE - 4);
    format!("{}{}", PITCH_CLASS_NAMES[usize::from(pitch % 12)], octave)
}

/// Parses a MIDI pitch typed as a number (0–127) or a name such as `C4`,
/// `F#-1`, `Bb3` (middle C = C4 = 60).
pub fn parse_pitch(input: &str) -> Result<u8, String> {
    parse_pitch_in(input, super::project::KeyRange::Standard)
}

/// [`parse_pitch`] in a project of key range `keys` (v0.9: 0–255 in 256-Key
/// Mode, where names go on past G9 — G#9 = 128 … D#20 = 255).
pub fn parse_pitch_in(input: &str, keys: super::project::KeyRange) -> Result<u8, String> {
    let top = keys.max_pitch();
    let t = input.trim();
    let bad = || {
        format!(
            "“{}” is not a note: use 0–{top} or a name such as C4, F#2 or Bb-1.",
            t.chars().take(20).collect::<String>()
        )
    };
    if let Ok(n) = t.parse::<i64>() {
        return u8::try_from(n).ok().filter(|&p| p <= top).ok_or_else(bad);
    }
    let mut chars = t.chars();
    let letter = chars.next().ok_or_else(bad)?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return Err(bad()),
    };
    let rest = chars.as_str();
    let (acc, octave) = match rest.chars().next() {
        Some('#' | '♯') => (1, &rest[rest.chars().next().map_or(0, char::len_utf8)..]),
        Some('b' | '♭') => (-1, &rest[rest.chars().next().map_or(0, char::len_utf8)..]),
        _ => (0, rest),
    };
    let octave: i32 = octave
        .trim()
        .replace('\u{2212}', "-")
        .parse()
        .map_err(|_| bad())?;
    let p = (octave + 1 - (MIDDLE_C_OCTAVE - 4)) * 12 + base + acc;
    u8::try_from(p).ok().filter(|&p| p <= top).ok_or_else(bad)
}

/// All 256 pitch labels (v0.9: names go on above G9 = 127 for 256-Key Mode),
/// computed once so renderers never format strings per note.
pub fn pitch_labels() -> Vec<String> {
    (0..=MAX_PITCH_256).map(pitch_name).collect()
}

/// What hovering a key shows (v0.9 §063): its number and its name, e.g.
/// `Key 60 · C4`, `Key 200 · G#15`.
pub fn key_label(pitch: u8) -> String {
    format!("Key {pitch} · {}", pitch_name(pitch))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.9 §036 / §058: true 256-key pitch in the same 32-bit word (bit 27
    /// is the 8th pitch bit — no RAM cost); standard constructors keep the
    /// MIDI range.
    #[test]
    fn pitch_is_eight_bits_wide_and_every_other_field_survives() {
        assert_eq!(std::mem::size_of::<NoteAttrs>(), 4);
        for p in [0u8, 1, 60, 127, 128, 129, 200, 254, 255] {
            for kind in [NoteOffKind::NoteOff, NoteOffKind::NoteOnZero] {
                let a = NoteAttrs::new_wide(p, 77, 9, 33, kind)
                    .unwrap()
                    .with_muted(true);
                assert_eq!(a.pitch(), p);
                assert_eq!(a.velocity(), 77);
                assert_eq!(a.channel(), 9);
                assert_eq!(a.off_kind(), kind);
                assert_eq!(
                    a.off_velocity(),
                    if kind == NoteOffKind::NoteOff { 33 } else { 0 }
                );
                assert!(a.muted());
                assert_eq!(a.is_extended(), p > MAX_PITCH);
                assert_eq!(NoteAttrs::from_raw(a.raw()), Some(a));
                // Other setters keep the wide pitch.
                assert_eq!(a.with_velocity(5).unwrap().pitch(), p);
                assert_eq!(a.with_channel(2).unwrap().pitch(), p);
                assert_eq!(a.with_muted(false).pitch(), p);
                // Changing the pitch clears both pitch parts.
                assert_eq!(a.with_pitch(60).unwrap().pitch(), 60);
                assert_eq!(a.with_pitch_wide(255).unwrap().pitch(), 255);
                assert_eq!(a.with_pitch_wide(3).unwrap().pitch(), 3);
            }
        }
        // The standard (MIDI) constructors and setter still refuse 128+.
        assert_eq!(
            NoteAttrs::new(128, 100, 0, 0, NoteOffKind::NoteOff),
            Err(NoteAttrsError::Pitch(128))
        );
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        assert_eq!(a.with_pitch(200), Err(NoteAttrsError::Pitch(200)));
        // A standard note's raw word is unchanged from v0.8 (bit 27 clear).
        assert_eq!(a.raw() >> 27, 0);
        assert_eq!(
            NoteAttrs::new_wide(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            a
        );
        // Reserved bits 28–31 are still refused.
        assert_eq!(NoteAttrs::from_raw(a.raw() | 1 << 28), None);
    }

    #[test]
    fn pitches_parse_from_numbers_and_names() {
        assert_eq!(parse_pitch("60"), Ok(60));
        assert_eq!(parse_pitch("C4"), Ok(60));
        assert_eq!(parse_pitch("c-1"), Ok(0));
        assert_eq!(parse_pitch("G9"), Ok(127));
        assert_eq!(parse_pitch("F#3"), Ok(54));
        assert_eq!(parse_pitch("Bb3"), Ok(58));
        for p in 0..=127u8 {
            assert_eq!(parse_pitch(&pitch_name(p)), Ok(p));
        }
        for bad in ["", "128", "-1", "H4", "C", "G#9", "C10x"] {
            assert!(parse_pitch(bad).is_err(), "{bad}");
        }
    }

    /// v0.9 §058 / §063: in 256-Key Mode typed keys and names reach 255
    /// (G#9 = 128 … D#20 = 255); every key's label parses back; the standard
    /// range still refuses 128 with its own range in the sentence.
    #[test]
    fn keys_above_127_parse_and_label_in_256_key_mode() {
        use super::super::project::KeyRange;
        let wide = KeyRange::Extended256;
        assert_eq!(parse_pitch_in("200", wide), Ok(200));
        assert_eq!(parse_pitch_in("G#9", wide), Ok(128));
        assert_eq!(parse_pitch_in("D#20", wide), Ok(255));
        assert!(parse_pitch_in("256", wide).unwrap_err().contains("0–255"));
        assert!(parse_pitch_in("E20", wide).is_err());
        assert!(parse_pitch("200").unwrap_err().contains("0–127"));
        let labels = pitch_labels();
        for p in 0..=255u8 {
            assert_eq!(parse_pitch_in(&labels[usize::from(p)], wide), Ok(p));
        }
        assert_eq!(key_label(60), "Key 60 · C4");
        assert_eq!(key_label(200), "Key 200 · G#15");
        assert_eq!(key_label(255), "Key 255 · D#20");
    }

    #[test]
    fn pack_roundtrip_all_fields() {
        for pitch in [0u8, 1, 60, 126, 127] {
            for vel in [1u8, 64, 127] {
                for ch in [0u8, 9, 15] {
                    for off in [0u8, 40, 127] {
                        let a = NoteAttrs::new(pitch, vel, ch, off, NoteOffKind::NoteOff).unwrap();
                        assert_eq!(a.pitch(), pitch);
                        assert_eq!(a.velocity(), vel);
                        assert_eq!(a.channel(), ch);
                        assert_eq!(a.off_velocity(), off);
                        assert_eq!(a.off_kind(), NoteOffKind::NoteOff);
                        assert_eq!(NoteAttrs::from_raw(a.raw()), Some(a));

                        let z =
                            NoteAttrs::new(pitch, vel, ch, off, NoteOffKind::NoteOnZero).unwrap();
                        assert_eq!(z.off_kind(), NoteOffKind::NoteOnZero);
                        assert_eq!(z.off_velocity(), 0);
                        assert_eq!(z.pitch(), pitch);
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(NoteAttrs::new(128, 1, 0, 0, NoteOffKind::NoteOff).is_err());
        assert!(NoteAttrs::new(1, 0, 0, 0, NoteOffKind::NoteOff).is_err());
        assert!(NoteAttrs::new(1, 128, 0, 0, NoteOffKind::NoteOff).is_err());
        assert!(NoteAttrs::new(1, 1, 16, 0, NoteOffKind::NoteOff).is_err());
        assert!(NoteAttrs::new(1, 1, 0, 128, NoteOffKind::NoteOff).is_err());
        assert!(NoteAttrs::from_raw(1 << 30).is_none());
    }

    #[test]
    fn with_pitch_preserves_everything_else() {
        let a = NoteAttrs::new(10, 77, 5, 33, NoteOffKind::NoteOff).unwrap();
        let b = a.with_pitch(99).unwrap();
        assert_eq!(b.pitch(), 99);
        assert_eq!(b.velocity(), 77);
        assert_eq!(b.channel(), 5);
        assert_eq!(b.off_velocity(), 33);
        assert!(a.with_pitch(128).is_err());
    }

    #[test]
    fn pitch_names_use_c4_as_middle_c() {
        assert_eq!(pitch_name(60), "C4");
        assert_eq!(pitch_name(61), "C#4");
        assert_eq!(pitch_name(0), "C-1");
        assert_eq!(pitch_name(127), "G9");
        assert_eq!(pitch_labels().len(), 256);
        assert!(is_black_key(61));
        assert!(!is_black_key(60));
    }
}
