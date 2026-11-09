//! Project-level state that a Standard MIDI File has no place for: the project name,
//! the SoundFont association and the Onion Skin configuration, plus the exact
//! decimal [`Bpm`] type used by the Conductor's tempo map. It lives inside
//! [`PlumDocument`](super::PlumDocument) and is stored by the native `.lup` format.

use super::track::TrackId;
use std::fmt;

// ------------------------------------------------------------------------ BPM

/// Smallest BPM PlumSpirit accepts (one quarter note every 694 days).
pub const MIN_BPM_TEXT: &str = "0.000001";
/// Largest BPM PlumSpirit accepts.
pub const MAX_BPM_TEXT: &str = "1000000000000";
/// Most significant digits a BPM may have.
pub const MAX_BPM_DIGITS: usize = 18;

const MIN_BPM_EXP: i32 = -6; // 10^-6
const MAX_BPM_EXP: i32 = 12; // 10^12

/// A tempo in beats (quarter notes) per minute, stored as an exact decimal
/// `mantissa × 10^exponent`.
///
/// Floating point is never the authoritative value: `120.5` is stored as
/// `1205 × 10^-1` and survives saving and reopening digit for digit. The mantissa is
/// normalised (no trailing zeros), so two equal tempos always compare equal.
///
/// Representable range: 0.000001 ≤ BPM ≤ 1,000,000,000,000 with at most 18
/// significant digits. That range is a limit of the representation, not a musical
/// ceiling; conversions for playback and MIDI export are separate, checked steps
/// ([`Bpm::to_micros_per_quarter`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bpm {
    mantissa: u64,
    exponent: i8,
}

/// Why a BPM could not be accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BpmError {
    Empty,
    NotANumber(String),
    Negative,
    Zero,
    DecimalComma,
    TooManyDigits,
    TooSmall,
    TooLarge,
}

impl fmt::Display for BpmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BpmError::Empty => write!(f, "Enter a tempo in BPM, for example 120."),
            BpmError::NotANumber(s) => write!(
                f,
                "\"{s}\" is not a number. Enter a tempo such as 120, 174.5 or 1e6."
            ),
            BpmError::Negative => write!(f, "The tempo must be positive."),
            BpmError::Zero => write!(f, "The tempo must be greater than 0 BPM."),
            BpmError::DecimalComma => {
                write!(f, "Use a dot as the decimal separator, for example 120.5.")
            }
            BpmError::TooManyDigits => write!(
                f,
                "PlumSpirit keeps up to {MAX_BPM_DIGITS} significant digits of a tempo."
            ),
            BpmError::TooSmall => write!(f, "The tempo must be at least {MIN_BPM_TEXT} BPM."),
            BpmError::TooLarge => {
                write!(f, "The tempo can be at most 1,000,000,000,000 BPM (10^12).")
            }
        }
    }
}

impl std::error::Error for BpmError {}

/// How a BPM maps onto the 24-bit "microseconds per quarter note" field of a MIDI
/// tempo event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempoFidelity {
    /// The MIDI value represents the BPM exactly.
    Exact,
    /// Rounded to the nearest microsecond; the resulting tempo differs from the
    /// project tempo by less than 0.1 %.
    Rounded,
    /// Rounded, and the tempo differs by 0.1 % or more (material).
    RoundedMaterially,
    /// The BPM is outside what a MIDI tempo event can store (about 3.58 to
    /// 60,000,000 BPM); the nearest storable tempo is used.
    Clamped,
}

impl TempoFidelity {
    /// Whether exporting with this conversion needs the user's explicit approval.
    pub fn is_material(self) -> bool {
        matches!(
            self,
            TempoFidelity::RoundedMaterially | TempoFidelity::Clamped
        )
    }
}

/// Result of converting a BPM to a MIDI tempo value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiTempo {
    /// Microseconds per quarter note, 1..=0xFF_FFFF.
    pub micros: u32,
    pub fidelity: TempoFidelity,
}

/// Largest value of the 24-bit MIDI tempo field.
pub const MAX_MIDI_TEMPO_MICROS: u32 = 0xFF_FFFF;
/// B1-0101: the fastest MIDI tempo (1 µs per quarter note).
pub const MIDI_BPM_MAX_TEXT: &str = "60,000,000";
/// B1-0101: the slowest tempo that still rounds to 16,777,215 µs per quarter
/// (60,000,000 / 16,777,215.5, rounded up in the 7th decimal).
pub const MIDI_BPM_MIN_TEXT: &str = "3.5762788";

impl Bpm {
    /// 120 BPM, the MIDI default tempo.
    pub const DEFAULT: Bpm = Bpm {
        mantissa: 12,
        exponent: 1,
    };

    /// `mantissa × 10^exponent`, validated and normalised.
    pub fn new(mantissa: u64, exponent: i32) -> Result<Bpm, BpmError> {
        if mantissa == 0 {
            return Err(BpmError::Zero);
        }
        let (mut m, mut e) = (mantissa, exponent);
        while m % 10 == 0 {
            m /= 10;
            e = e.checked_add(1).ok_or(BpmError::TooLarge)?;
        }
        let digits = decimal_digits(m) as i32;
        if digits as usize > MAX_BPM_DIGITS {
            return Err(BpmError::TooManyDigits);
        }
        let magnitude = e.checked_add(digits - 1).ok_or(BpmError::TooLarge)?;
        if magnitude < MIN_BPM_EXP {
            return Err(BpmError::TooSmall);
        }
        if magnitude > MAX_BPM_EXP || (magnitude == MAX_BPM_EXP && m != 1) {
            return Err(BpmError::TooLarge);
        }
        // magnitude in [-6, 12] and digits <= 18 keep e within [-23, 12].
        Ok(Bpm {
            mantissa: m,
            exponent: e as i8,
        })
    }

    /// Whole BPM value (for tests and defaults).
    pub fn from_integer(v: u64) -> Result<Bpm, BpmError> {
        Bpm::new(v, 0)
    }

    pub fn mantissa(self) -> u64 {
        self.mantissa
    }

    pub fn exponent(self) -> i32 {
        i32::from(self.exponent)
    }

    /// Parses user input such as `120`, `174.5`, `.5`, `1e6` or `2.5E-3`.
    ///
    /// Accepts an optional leading `+`, digits with at most one `.`, and an optional
    /// exponent. Rejects zero, negative values, NaN/infinity words, commas, values
    /// outside the representable range and more than 18 significant digits.
    pub fn parse(input: &str) -> Result<Bpm, BpmError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(BpmError::Empty);
        }
        let not_number = || BpmError::NotANumber(s.chars().take(40).collect());
        let mut rest = s;
        if let Some(r) = rest.strip_prefix('+') {
            rest = r;
        } else if rest.starts_with('-') || rest.starts_with('\u{2212}') {
            // Only report "negative" for something that is otherwise a number.
            let body = rest.trim_start_matches(['-', '\u{2212}']);
            return if Bpm::parse(body).is_ok() || body.trim_start_matches(['0', '.']).is_empty() {
                Err(BpmError::Negative)
            } else {
                Err(not_number())
            };
        }
        if rest.contains(',') {
            return Err(BpmError::DecimalComma);
        }
        let (number, exp_part) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int_part, frac_part) = match number.find('.') {
            Some(i) => (&number[..i], &number[i + 1..]),
            None => (number, ""),
        };
        if (int_part.is_empty() && frac_part.is_empty())
            || !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(not_number());
        }
        let mut exponent: i64 = 0;
        if let Some(ep) = exp_part {
            let (neg, digits) = match ep.as_bytes().first() {
                Some(b'+') => (false, &ep[1..]),
                Some(b'-') => (true, &ep[1..]),
                _ => (false, ep),
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(not_number());
            }
            // Anything with more than 6 exponent digits is far outside the range.
            let trimmed = digits.trim_start_matches('0');
            if trimmed.len() > 6 {
                return Err(if neg {
                    BpmError::TooSmall
                } else {
                    BpmError::TooLarge
                });
            }
            let v: i64 = trimmed.parse().unwrap_or(0);
            exponent = if neg { -v } else { v };
        }
        // All digits, leading zeros removed; the exponent absorbs the fraction.
        let all: String = int_part.chars().chain(frac_part.chars()).collect();
        exponent -= frac_part.len() as i64;
        let significant = all.trim_start_matches('0');
        if significant.is_empty() {
            return Err(BpmError::Zero);
        }
        let without_trailing = significant.trim_end_matches('0');
        exponent += (significant.len() - without_trailing.len()) as i64;
        if without_trailing.len() > MAX_BPM_DIGITS {
            return Err(BpmError::TooManyDigits);
        }
        let mantissa: u64 = without_trailing.parse().map_err(|_| not_number())?;
        let exponent = i32::try_from(exponent).map_err(|_| {
            if exponent < 0 {
                BpmError::TooSmall
            } else {
                BpmError::TooLarge
            }
        })?;
        Bpm::new(mantissa, exponent)
    }

    /// The exact value as a reduced-free fraction `numerator / denominator`.
    pub fn as_ratio(self) -> (u128, u128) {
        let m = u128::from(self.mantissa);
        if self.exponent >= 0 {
            (m * pow10(self.exponent as u32), 1)
        } else {
            (m, pow10(self.exponent.unsigned_abs() as u32))
        }
    }

    /// Approximate value (display and non-authoritative UI math only).
    pub fn to_f64(self) -> f64 {
        self.mantissa as f64 * 10f64.powi(i32::from(self.exponent))
    }

    /// Converts to the MIDI tempo field (microseconds per quarter note), rounding to
    /// the nearest microsecond (halves up) and clamping into `1..=0xFF_FFFF`.
    pub fn to_micros_per_quarter(self) -> MidiTempo {
        // µs = 60,000,000 / bpm = 60e6 · den / num
        let (num, den) = self.as_ratio();
        let top = 60_000_000u128 * den; // <= 6e7 · 1e23 < 2^103
        let rounded = (2 * top + num) / (2 * num);
        let (micros, clamped) = if rounded < 1 {
            (1u32, true)
        } else if rounded > u128::from(MAX_MIDI_TEMPO_MICROS) {
            (MAX_MIDI_TEMPO_MICROS, true)
        } else {
            (rounded as u32, false)
        };
        let fidelity = if clamped {
            TempoFidelity::Clamped
        } else {
            let exact_top = u128::from(micros) * num; // µs · num vs 6e7 · den
            let diff = exact_top.abs_diff(top);
            if diff == 0 {
                TempoFidelity::Exact
            } else if diff * 1000 >= top {
                TempoFidelity::RoundedMaterially
            } else {
                TempoFidelity::Rounded
            }
        };
        MidiTempo { micros, fidelity }
    }

    /// B1-0101: a typed tempo for a song that may become a MIDI file — `FF 51`
    /// stores 24-bit microseconds per quarter note, so a tempo is accepted when
    /// it maps to 1 – 16,777,215 µs (60,000,000 BPM down to about 3.5762788
    /// BPM); the decimal is kept exactly, MIDI export rounds it to whole µs.
    pub fn parse_midi(input: &str) -> Result<Bpm, String> {
        let b = Bpm::parse(input).map_err(|e| e.to_string())?;
        b.check_midi()?;
        Ok(b)
    }

    /// B1-0101: refuses a tempo a MIDI file cannot store (it would be clamped).
    pub fn check_midi(self) -> Result<(), String> {
        // Faster than 1 µs per quarter note (60,000,000 BPM) would still round
        // to 1 µs up to 120,000,000 BPM: the cap is exact, not by rounding.
        let (num, den) = self.as_ratio();
        let too_fast = num > 60_000_000u128 * den;
        if too_fast || self.to_micros_per_quarter().fidelity == TempoFidelity::Clamped {
            return Err(format!(
                "A MIDI file stores a tempo as whole microseconds per quarter note (1 – 16,777,215): use {MIDI_BPM_MIN_TEXT} – {MIDI_BPM_MAX_TEXT} BPM. {self} BPM is outside that range."
            ));
        }
        Ok(())
    }

    /// B1-0101: how a MIDI file stores this tempo when it is not exact (the
    /// project keeps the exact decimal); `None` when it is exact.
    pub fn midi_note(self) -> Option<String> {
        let t = self.to_micros_per_quarter();
        if t.fidelity == TempoFidelity::Exact {
            return None;
        }
        let back = Bpm::from_micros_per_quarter(t.micros)
            .map(|b| b.to_string())
            .unwrap_or_default();
        Some(format!(
            "A MIDI file stores {} µs per quarter note ({back} BPM).",
            crate::midi::group_digits(u64::from(t.micros))
        ))
    }

    /// The tempo a MIDI tempo value represents, as an exact BPM when it fits the
    /// representation (up to 18 digits, rounded otherwise). Used to display tempos
    /// that come from MIDI files.
    pub fn from_micros_per_quarter(micros: u32) -> Option<Bpm> {
        if micros == 0 {
            return None;
        }
        // 60e6 / micros with up to 12 decimals, rounded half up.
        let scaled =
            (60_000_000u128 * 10u128.pow(12) * 2 + u128::from(micros)) / (2 * u128::from(micros));
        let mut m = scaled;
        let mut e: i32 = -12;
        while m > 0 && m.is_multiple_of(10) {
            m /= 10;
            e += 1;
        }
        while decimal_digits_u128(m) > MAX_BPM_DIGITS as u32 {
            m = (m + 5) / 10;
            e += 1;
        }
        Bpm::new(u64::try_from(m).ok()?, e).ok()
    }
}

impl Default for Bpm {
    fn default() -> Self {
        Bpm::DEFAULT
    }
}

impl fmt::Display for Bpm {
    /// Plain decimal notation without exponent, e.g. `120`, `174.5`, `0.000001`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.mantissa.to_string();
        if self.exponent >= 0 {
            f.write_str(&digits)?;
            for _ in 0..self.exponent {
                f.write_str("0")?;
            }
            Ok(())
        } else {
            let frac = self.exponent.unsigned_abs() as usize;
            if digits.len() > frac {
                let (i, d) = digits.split_at(digits.len() - frac);
                write!(f, "{i}.{d}")
            } else {
                write!(f, "0.{}{digits}", "0".repeat(frac - digits.len()))
            }
        }
    }
}

impl fmt::Debug for Bpm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bpm({self})")
    }
}

fn pow10(e: u32) -> u128 {
    10u128.pow(e)
}

fn decimal_digits(v: u64) -> usize {
    if v == 0 { 1 } else { v.ilog10() as usize + 1 }
}

fn decimal_digits_u128(v: u128) -> u32 {
    if v == 0 { 1 } else { v.ilog10() + 1 }
}

// ------------------------------------------------------------------ Onion Skin

/// Which other tracks the piano roll shows as translucent "ghost" notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum OnionMode {
    #[default]
    Off,
    /// The track above the active track in the track list.
    Previous,
    /// The track below the active track.
    Next,
    PreviousAndNext,
    /// The tracks listed in [`OnionConfig::specific`].
    Specific,
    AllOthers,
}

impl OnionMode {
    pub const ALL: [OnionMode; 6] = [
        OnionMode::Off,
        OnionMode::Previous,
        OnionMode::Next,
        OnionMode::PreviousAndNext,
        OnionMode::Specific,
        OnionMode::AllOthers,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OnionMode::Off => "Off",
            OnionMode::Previous => "Previous Track",
            OnionMode::Next => "Next Track",
            OnionMode::PreviousAndNext => "Previous + Next",
            OnionMode::Specific => "Specific Tracks",
            OnionMode::AllOthers => "All Other Tracks",
        }
    }

    pub fn code(self) -> u8 {
        match self {
            OnionMode::Off => 0,
            OnionMode::Previous => 1,
            OnionMode::Next => 2,
            OnionMode::PreviousAndNext => 3,
            OnionMode::Specific => 4,
            OnionMode::AllOthers => 5,
        }
    }

    pub fn from_code(c: u8) -> Option<OnionMode> {
        OnionMode::ALL.get(usize::from(c)).copied()
    }
}

/// Default ghost-note opacity in percent.
pub const DEFAULT_ONION_OPACITY: u8 = 35;
/// Ghost opacity is a real percentage: 0 % (invisible) to 100 % (opaque).
pub const MIN_ONION_OPACITY: u8 = 0;
pub const MAX_ONION_OPACITY: u8 = 100;

/// The order ghost layers are drawn in (render order only: the track list is
/// never reordered). The last-drawn layer is in front.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OnionOrder {
    /// Track-list order (v0.4 behaviour).
    #[default]
    Project,
    /// The source with the most notes in front, the sparsest behind.
    DenseFirst,
    /// The sparsest source in front.
    SparseFirst,
}

impl OnionOrder {
    pub const ALL: [OnionOrder; 3] = [
        OnionOrder::Project,
        OnionOrder::DenseFirst,
        OnionOrder::SparseFirst,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OnionOrder::Project => "Project Order",
            OnionOrder::DenseFirst => "Dense First",
            OnionOrder::SparseFirst => "Sparse First",
        }
    }

    pub fn code(self) -> u8 {
        match self {
            OnionOrder::Project => 0,
            OnionOrder::DenseFirst => 1,
            OnionOrder::SparseFirst => 2,
        }
    }

    pub fn from_code(c: u8) -> Option<OnionOrder> {
        OnionOrder::ALL.get(usize::from(c)).copied()
    }
}

/// What "dense" means for [`OnionOrder`]: notes in the visible part of the song
/// (default) or in the whole track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DensityBasis {
    #[default]
    VisibleRange,
    WholeTrack,
}

impl DensityBasis {
    pub fn label(self) -> &'static str {
        match self {
            DensityBasis::VisibleRange => "Visible Range",
            DensityBasis::WholeTrack => "Whole Track",
        }
    }

    pub fn code(self) -> u8 {
        match self {
            DensityBasis::VisibleRange => 0,
            DensityBasis::WholeTrack => 1,
        }
    }

    pub fn from_code(c: u8) -> Option<DensityBasis> {
        match c {
            0 => Some(DensityBasis::VisibleRange),
            1 => Some(DensityBasis::WholeTrack),
            _ => None,
        }
    }
}

/// Orders ghost sources for drawing (back to front) by `counts` (notes per
/// source, same order as `sources`). Ties keep project order, so the result is
/// deterministic. Project order returns `sources` unchanged.
pub fn order_onion_layers(order: OnionOrder, sources: &[usize], counts: &[usize]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..sources.len()).collect();
    match order {
        OnionOrder::Project => {}
        // Drawn back to front: the densest is drawn last (in front).
        OnionOrder::DenseFirst => idx.sort_by(|&a, &b| {
            counts[a]
                .cmp(&counts[b])
                .then_with(|| sources[b].cmp(&sources[a]))
        }),
        OnionOrder::SparseFirst => idx.sort_by(|&a, &b| {
            counts[b]
                .cmp(&counts[a])
                .then_with(|| sources[b].cmp(&sources[a]))
        }),
    }
    idx.into_iter().map(|i| sources[i]).collect()
}

/// Hysteresis of the Dense / Sparse First ranking (v0.8 §117 / §118): the
/// density a source is ranked by changes only when its note count moves away
/// from the ranked value by more than this share …
pub const ONION_ORDER_HYSTERESIS: f64 = 0.125;
/// … and by more than this many notes. Smaller changes (a view boundary
/// moving across a few notes, playback scrolling) keep the shown order.
pub const ONION_ORDER_MIN_DIFF: usize = 8;

/// Stable Onion layer order (v0.8 §116 – §118, test §225).
///
/// Every ghost source is ranked by a *density snapshot*: its note count when
/// last ranked, replaced only when the count moves beyond the hysteresis
/// ([`ONION_ORDER_HYSTERESIS`], [`ONION_ORDER_MIN_DIFF`]). Sources are then
/// ordered exactly by their snapshots — Dense First puts the densest in
/// front, Sparse First the sparsest — with the TrackId as the final tie
/// breaker (the lower id in front). So the order is a total order (no
/// cycles, independent of input order), small density changes never make
/// layers swap back and forth (A/B/A/B), and real changes reorder at once.
/// O(n log n) per ranking; snapshots are kept per TrackId.
#[derive(Clone, Debug, Default)]
pub struct OnionRanking {
    snap: std::collections::HashMap<TrackId, usize>,
    order: OnionOrder,
}

impl OnionRanking {
    /// The count a source is ranked by after seeing `count` (updates the
    /// snapshot when the change is meaningful).
    fn ranked(&mut self, id: TrackId, count: usize) -> usize {
        let s = self.snap.entry(id).or_insert(count);
        let hi = (*s).max(count);
        let margin =
            ((hi as f64 * ONION_ORDER_HYSTERESIS).ceil() as usize).max(ONION_ORDER_MIN_DIFF);
        if s.abs_diff(count) > margin {
            *s = count;
        }
        *s
    }

    /// Ghost sources (`sources`, stable ids `ids`, note counts `counts`, all
    /// in the same order) in drawing order, back to front. Project order
    /// returns `sources` unchanged; switching between Dense and Sparse First
    /// starts from fresh snapshots.
    pub fn order(
        &mut self,
        order: OnionOrder,
        sources: &[usize],
        ids: &[TrackId],
        counts: &[usize],
    ) -> Vec<usize> {
        if order != self.order {
            self.snap.clear();
            self.order = order;
        }
        if order == OnionOrder::Project || sources.len() < 2 {
            return sources.to_vec();
        }
        // Forget sources no longer shown (bounded by the project's tracks).
        if self.snap.len() > ids.len() * 2 + 64 {
            let keep: std::collections::HashSet<TrackId> = ids.iter().copied().collect();
            self.snap.retain(|k, _| keep.contains(k));
        }
        let ranked: Vec<usize> = ids
            .iter()
            .zip(counts)
            .map(|(&id, &c)| self.ranked(id, c))
            .collect();
        let mut idx: Vec<usize> = (0..sources.len()).collect();
        // Back to front: the front source sorts last.
        idx.sort_by(|&a, &b| {
            let by_count = match order {
                OnionOrder::DenseFirst => ranked[a].cmp(&ranked[b]),
                _ => ranked[b].cmp(&ranked[a]),
            };
            by_count.then_with(|| ids[b].cmp(&ids[a]))
        });
        idx.into_iter().map(|i| sources[i]).collect()
    }
}

/// Project-scoped Onion Skin settings. Visual only: they never change notes,
/// channels, playback or export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnionConfig {
    pub mode: OnionMode,
    /// Layer order (v0.5).
    pub order: OnionOrder,
    /// Density measure used by [`OnionOrder::DenseFirst`] / `SparseFirst`.
    pub density: DensityBasis,
    /// Source tracks for [`OnionMode::Specific`], by stable id. Ids of deleted tracks
    /// are ignored when drawing (and kept, so undoing the deletion restores them).
    pub specific: Vec<TrackId>,
    /// Ghost opacity in percent (0–100).
    pub opacity: u8,
    /// B1-0064 (Domino "Per Track Settings"): each track keeps its own mode
    /// and list; making a track active brings back what it last used.
    pub per_track: bool,
    /// What each track last used (by id, sorted), kept while `per_track` is
    /// off so switching it back on restores them.
    pub track_entries: Vec<OnionTrackEntry>,
}

/// B1-0064: one track's own Onion mode and list (Per-Track Settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnionTrackEntry {
    pub track: TrackId,
    pub mode: OnionMode,
    pub specific: Vec<TrackId>,
}

impl Default for OnionConfig {
    fn default() -> Self {
        OnionConfig {
            mode: OnionMode::Off,
            order: OnionOrder::Project,
            density: DensityBasis::VisibleRange,
            specific: Vec::new(),
            opacity: DEFAULT_ONION_OPACITY,
            per_track: false,
            track_entries: Vec::new(),
        }
    }
}

impl OnionConfig {
    /// Track indices whose notes are drawn as ghosts while `active` is the active
    /// track, in drawing order (track order; the active track is never included).
    /// `ids` lists the document's track ids in order.
    pub fn sources(&self, ids: &[TrackId], active: usize) -> Vec<usize> {
        self.sources_at(ids, Some(active))
    }

    /// [`Self::sources`] for a selected row that may be the Conductor
    /// (`active == None`, B2-D01). The Conductor owns no notes and belongs to
    /// the whole song: no musical track is excluded, and it has no neighbour
    /// among the tracks, so the relative modes show the whole song (a first
    /// track that only carried a MIDI file's tempo would otherwise leave the
    /// view empty); Specific keeps the user's own list.
    ///
    /// | Mode | Track `a` active | Conductor active |
    /// |---|---|---|
    /// | Previous | `a − 1` | every track |
    /// | Next | `a + 1` | every track |
    /// | Previous and Next | `a − 1`, `a + 1` | every track |
    /// | Specific | the chosen tracks except `a` | all the chosen tracks |
    /// | All Others | every track except `a` | every track |
    /// An active index past the last track has no sources.
    pub fn sources_at(&self, ids: &[TrackId], active: Option<usize>) -> Vec<usize> {
        let n = ids.len();
        if active.is_some_and(|a| a >= n) {
            return Vec::new();
        }
        let others = |i: usize| Some(i) != active;
        let (Some(a), mode) = (active, self.mode) else {
            return match self.mode {
                OnionMode::Off => Vec::new(),
                OnionMode::Specific => ids
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| self.specific.contains(id))
                    .map(|(i, _)| i)
                    .collect(),
                _ => (0..n).collect(),
            };
        };
        let above = a.checked_sub(1);
        let below = (a + 1 < n).then_some(a + 1);
        match mode {
            OnionMode::Off => Vec::new(),
            OnionMode::Previous => above.into_iter().collect(),
            OnionMode::Next => below.into_iter().collect(),
            OnionMode::PreviousAndNext => above.into_iter().chain(below).collect(),
            OnionMode::Specific => ids
                .iter()
                .enumerate()
                .filter(|&(i, id)| others(i) && self.specific.contains(id))
                .map(|(i, _)| i)
                .collect(),
            OnionMode::AllOthers => (0..n).filter(|&i| others(i)).collect(),
        }
    }

    pub fn clamp_opacity(v: u8) -> u8 {
        v.clamp(MIN_ONION_OPACITY, MAX_ONION_OPACITY)
    }

    /// B1-0064 (Per-Track Settings): the active track changed from `old` to
    /// `new`. The mode and list in force are kept for `old`, and `new`'s own
    /// come back (a track without its own keeps what is in force). Nothing
    /// happens while `per_track` is off. Returns whether anything changed.
    pub fn switch_track(&mut self, old: Option<TrackId>, new: TrackId) -> bool {
        if !self.per_track || old == Some(new) {
            return false;
        }
        let before = (self.mode, self.specific.clone(), self.track_entries.len());
        if let Some(old) = old {
            let entry = OnionTrackEntry {
                track: old,
                mode: self.mode,
                specific: self.specific.clone(),
            };
            match self.track_entries.binary_search_by_key(&old, |e| e.track) {
                Ok(i) => self.track_entries[i] = entry,
                Err(i) => self.track_entries.insert(i, entry),
            }
        }
        let mut changed_entries = self.track_entries.len() != before.2;
        if let Ok(i) = self.track_entries.binary_search_by_key(&new, |e| e.track) {
            let e = &self.track_entries[i];
            self.mode = e.mode;
            self.specific = e.specific.clone();
        }
        changed_entries |= (self.mode, &self.specific) != (before.0, &before.1);
        changed_entries
    }

    /// B1-0064: shows (`on`) or hides the tracks at `tracks` as ghosts while
    /// `active` is the active track, starting from what is drawn now: a mode
    /// other than Specific Tracks becomes Specific Tracks with its current
    /// sources, so clicking one track keeps the others as they were. The
    /// active track is never its own ghost; ids of deleted tracks in the list
    /// are kept (undoing the deletion brings them back). No source left:
    /// Off (the list is kept for the next Specific Tracks).
    pub fn with_tracks(
        &self,
        ids: &[TrackId],
        active: usize,
        tracks: &[usize],
        on: bool,
    ) -> OnionConfig {
        self.with_tracks_at(ids, Some(active), tracks, on)
    }

    /// L-G03 (Replace): exactly the tracks at `tracks` as ghosts while
    /// `active` is the active track (`None`: the Conductor) — every other
    /// source, including an earlier multi-track list, is dropped. The active
    /// track is never its own ghost; nothing left: unchanged (`None`).
    pub fn with_only_tracks_at(
        &self,
        ids: &[TrackId],
        active: Option<usize>,
        tracks: &[usize],
    ) -> Option<OnionConfig> {
        let only: Vec<TrackId> = ids
            .iter()
            .enumerate()
            .filter(|&(i, _)| Some(i) != active && tracks.contains(&i))
            .map(|(_, &id)| id)
            .collect();
        if only.is_empty() {
            return None;
        }
        let mut next = self.clone();
        next.specific = only;
        next.mode = OnionMode::Specific;
        Some(next)
    }

    /// L-G03 (Per-Track Settings): the mode and list in force become every
    /// track's — the tracks' own remembered settings are dropped, so making
    /// another track active keeps this one. Returns whether any was dropped.
    pub fn use_for_every_track(&mut self) -> bool {
        let had = !self.track_entries.is_empty();
        self.track_entries.clear();
        had
    }

    /// [`Self::with_tracks`] for a selected row that may be the Conductor
    /// (`None`, B2-D01): then no track is skipped as "the active one".
    pub fn with_tracks_at(
        &self,
        ids: &[TrackId],
        active: Option<usize>,
        tracks: &[usize],
        on: bool,
    ) -> OnionConfig {
        let mut set: Vec<TrackId> = if self.mode == OnionMode::Specific {
            self.specific.clone()
        } else {
            self.sources_at(ids, active)
                .into_iter()
                .map(|i| ids[i])
                .collect()
        };
        for &t in tracks {
            let Some(&id) = ids.get(t) else { continue };
            if Some(t) == active {
                continue;
            }
            let has = set.contains(&id);
            if on && !has {
                set.push(id);
            } else if !on && has {
                set.retain(|&x| x != id);
            }
        }
        // Track order for the ids that exist, then any of deleted tracks.
        let mut ordered: Vec<TrackId> = ids.iter().copied().filter(|id| set.contains(id)).collect();
        ordered.extend(set.iter().copied().filter(|id| !ids.contains(id)));
        let mut next = self.clone();
        let shown = ids
            .iter()
            .enumerate()
            .any(|(i, id)| Some(i) != active && ordered.contains(id));
        next.specific = ordered;
        next.mode = if shown {
            OnionMode::Specific
        } else {
            OnionMode::Off
        };
        next
    }
}

// ----------------------------------------------------------------- SoundFont

/// The SoundFont a project was last played with. Only the path is stored, never
/// the SoundFont data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundFontRef {
    /// Absolute path as the user selected it.
    pub path: String,
}

// ------------------------------------------------------- reference audio

/// Most reference volume (percent).
pub const MAX_REFERENCE_VOLUME_PCT: u16 = 200;
/// Largest reference offset either way (microseconds): ten hours.
pub const MAX_REFERENCE_OFFSET_US: i64 = 36_000_000_000;

/// The reference audio played with the project (v0.8 Audio Reference). A
/// project setting (saved in `.lup`, outside undo); never part of MIDI export.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReferenceAudio {
    /// The file as the user chose it (absolute).
    pub path: String,
    /// Project time (µs from project zero) where the reference's start plays;
    /// negative: the reference starts before the project.
    pub offset_us: i64,
    /// 0 – [`MAX_REFERENCE_VOLUME_PCT`].
    pub volume_pct: u16,
    pub mute: bool,
}

impl ReferenceAudio {
    pub fn new(path: String) -> ReferenceAudio {
        ReferenceAudio {
            path,
            offset_us: 0,
            volume_pct: 100,
            mute: false,
        }
    }
    pub fn is_valid(&self) -> bool {
        !self.path.is_empty()
            && self.volume_pct <= MAX_REFERENCE_VOLUME_PCT
            && self.offset_us.abs() <= MAX_REFERENCE_OFFSET_US
    }
    /// Offset in seconds.
    pub fn offset_seconds(&self) -> f64 {
        self.offset_us as f64 / 1e6
    }
}

// ------------------------------------------------------ transport and graphs

/// The section loop: playback repeats `[start, end)` while `enabled`. Project
/// transport configuration (saved in `.lup`; never changes notes or export).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct LoopConfig {
    /// Half-open tick range, `end > start`.
    pub range: Option<(u64, u64)>,
    pub enabled: bool,
}

impl LoopConfig {
    /// The active loop range (enabled and set).
    pub fn active(&self) -> Option<(u64, u64)> {
        self.range.filter(|_| self.enabled)
    }

    /// Validates a loop range.
    pub fn check(start: u64, end: u64) -> Result<(u64, u64), String> {
        if end <= start {
            Err("The loop must end after it starts.".into())
        } else {
            Ok((start, end))
        }
    }
}

/// What an event graph slot shows (persisted with the project).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GraphTarget {
    Velocity,
    PitchBend,
    /// Control Change 0–127 on the slot's channel.
    Cc(u8),
    /// The project's Master Volume (global).
    MasterVolume,
    /// The Conductor's tempo.
    Bpm,
}

impl GraphTarget {
    pub fn code(self) -> (u8, u8) {
        match self {
            GraphTarget::Velocity => (0, 0),
            GraphTarget::PitchBend => (1, 0),
            GraphTarget::Cc(n) => (2, n),
            GraphTarget::MasterVolume => (3, 0),
            GraphTarget::Bpm => (4, 0),
        }
    }

    pub fn from_code(kind: u8, n: u8) -> Option<GraphTarget> {
        match kind {
            0 => Some(GraphTarget::Velocity),
            1 => Some(GraphTarget::PitchBend),
            2 if n <= 127 => Some(GraphTarget::Cc(n)),
            3 => Some(GraphTarget::MasterVolume),
            4 => Some(GraphTarget::Bpm),
            _ => None,
        }
    }
}

/// One event graph slot below the piano roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GraphSlot {
    pub visible: bool,
    pub target: GraphTarget,
    /// MIDI channel (0–15) of channel-message targets.
    pub channel: u8,
}

/// Event Graph 1 and 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GraphConfig {
    pub slots: [GraphSlot; 2],
}

impl Default for GraphConfig {
    fn default() -> Self {
        GraphConfig {
            slots: [
                GraphSlot {
                    visible: true,
                    target: GraphTarget::Velocity,
                    channel: 0,
                },
                GraphSlot {
                    visible: false,
                    target: GraphTarget::Cc(1),
                    channel: 0,
                },
            ],
        }
    }
}

// -------------------------------------------------------------- ProjectInfo

/// A `.lup` section this version of PlumSpirit does not understand but was marked
/// optional; it is kept and written back unchanged on save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownSection {
    pub tag: [u8; 4],
    pub flags: u32,
    pub data: Vec<u8>,
}

/// Default name of a new project.
pub const DEFAULT_PROJECT_NAME: &str = "Untitled";
/// Longest project name, in characters.
pub const MAX_PROJECT_NAME_CHARS: usize = 200;

/// A named Piano Roll view (Zoom tool, v0.7), saved with the project. The
/// view's scale values are stored as exact `f64` bit patterns.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ZoomRegion {
    pub name: String,
    pub origin_tick: u64,
    pub ppt_bits: u64,
    /// Rows scrolled below pitch 127's row (f64 bits; v0.9: negative in
    /// 256-Key Mode when keys above 127 are on top, so the meaning never
    /// depends on the mode).
    pub scroll_bits: u64,
    pub row_bits: u64,
}

/// The Track List's view of a project (v0.9, V09-0118): horizontal zoom and
/// scroll and the row height. Display settings: saved with the project
/// (`.lup` section `TLST`) but never an edit — changing them does not make
/// the project unsaved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackListView {
    pub origin_tick: u64,
    /// Pixels per tick (f64 bits).
    pub ppt_bits: u64,
    /// Row height in points (f32 bits).
    pub row_bits: u32,
}

/// The Track List's row height limits (points).
pub const TRACK_LIST_MIN_ROW: f32 = 22.0;
/// B1-0090: a row may fill a large screen (clip notes stay readable).
pub const TRACK_LIST_MAX_ROW: f32 = 960.0;

impl TrackListView {
    pub fn new(origin_tick: u64, pixels_per_tick: f64, row_height: f32) -> TrackListView {
        TrackListView {
            origin_tick,
            ppt_bits: pixels_per_tick.to_bits(),
            row_bits: row_height.to_bits(),
        }
    }

    pub fn pixels_per_tick(&self) -> f64 {
        f64::from_bits(self.ppt_bits)
    }

    pub fn row_height(&self) -> f32 {
        f32::from_bits(self.row_bits)
    }

    /// A zoom the time axis accepts unchanged and a row height in range.
    pub fn is_valid(&self) -> bool {
        let p = self.pixels_per_tick();
        let r = self.row_height();
        p.is_finite()
            && p > 0.0
            && crate::view::clamp_zoom(p) == p
            && r.is_finite()
            && (TRACK_LIST_MIN_ROW..=TRACK_LIST_MAX_ROW).contains(&r)
    }
}

/// Most named zoom regions per project.
pub const MAX_ZOOM_REGIONS: usize = 64;
/// Longest zoom region name, in characters.
pub const MAX_ZOOM_NAME_CHARS: usize = 64;

impl ZoomRegion {
    pub fn new(
        name: &str,
        origin_tick: u64,
        ppt: f64,
        scroll_rows: f64,
        row_height: f64,
    ) -> ZoomRegion {
        ZoomRegion {
            name: name.chars().take(MAX_ZOOM_NAME_CHARS).collect(),
            origin_tick,
            ppt_bits: ppt.to_bits(),
            scroll_bits: scroll_rows.to_bits(),
            row_bits: row_height.to_bits(),
        }
    }

    pub fn pixels_per_tick(&self) -> f64 {
        f64::from_bits(self.ppt_bits)
    }

    pub fn scroll_rows(&self) -> f64 {
        f64::from_bits(self.scroll_bits)
    }

    pub fn row_height(&self) -> f64 {
        f64::from_bits(self.row_bits)
    }

    /// Finite, positive scales and a non-empty name.
    pub fn is_valid(&self) -> bool {
        let (p, s, r) = (
            self.pixels_per_tick(),
            self.scroll_rows(),
            self.row_height(),
        );
        p.is_finite()
            && p > 0.0
            && s.is_finite()
            && r.is_finite()
            && r > 0.0
            && !self.name.trim().is_empty()
    }
}

/// The pitch range of a project (v0.9 §036, §058): standard MIDI keys 0–127,
/// or 256-Key Mode with pitches 0–255 really stored (not faked visually).
/// Off by default; pitch-indexed structures size themselves from it, so a
/// standard project pays nothing for the mode (§206).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyRange {
    #[default]
    Standard,
    Extended256,
}

impl KeyRange {
    /// Number of keys: 128 or 256.
    pub fn keys(self) -> usize {
        match self {
            KeyRange::Standard => 128,
            KeyRange::Extended256 => 256,
        }
    }

    /// Highest pitch: 127 or 255.
    pub fn max_pitch(self) -> u8 {
        (self.keys() - 1) as u8
    }

    /// Whether `pitch` is inside the range.
    pub fn contains(self, pitch: u8) -> bool {
        usize::from(pitch) < self.keys()
    }

    /// The range whose highest key is `top` (a view's top row): 127 or less
    /// is the standard keyboard, anything above is 256-Key Mode.
    pub fn from_top(top: u8) -> KeyRange {
        if top > KeyRange::Standard.max_pitch() {
            KeyRange::Extended256
        } else {
            KeyRange::Standard
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    /// Standard keys or 256-Key Mode (v0.9).
    pub keys: KeyRange,
    pub soundfont: Option<SoundFontRef>,
    pub onion: OnionConfig,
    /// Section loop (transport configuration).
    pub looping: LoopConfig,
    /// Event graph slots.
    pub graphs: GraphConfig,
    /// Note groups (v0.7; the notes carry the ids).
    pub groups: super::group::GroupTable,
    /// Named Piano Roll views (Zoom tool, v0.7).
    pub zoom_regions: Vec<ZoomRegion>,
    /// Reference audio (v0.8).
    pub reference: Option<ReferenceAudio>,
    /// Track Folders (v0.8; changed through the undo history).
    pub folders: super::folder::TrackFolders,
    /// Collapsed folders (view state outside undo, saved with the project).
    pub folders_collapsed: std::collections::BTreeSet<u32>,
    /// The Track List's zoom, scroll and row height (v0.9; display settings,
    /// never dirtying).
    pub track_list: Option<TrackListView>,
    /// v1.0-beta1 (B1-0121): the MIDI file the project was imported from, for
    /// strict no-edit export (`midi::provenance`). Saved in `.lup` (`PROV`);
    /// never part of MIDI export or of the undo history.
    pub source: Option<SourceProvenance>,
    /// v1.0-beta1 (B1-0035): Lua plugins' per-project data — (plugin id,
    /// JSON text) sorted by id. Saved in `.lup` (`PLUG`); outside the undo
    /// history like Mute / Solo (it dirties the project); never exported.
    pub plugin_data: Vec<(String, String)>,
    /// Sparse typed transactional API02 metadata, separate from legacy settings.
    pub metadata: super::metadata::Metadata,
    pub unknown_sections: Vec<UnknownSection>,
    /// v1.0-Launch L-F08 (contract §016): Play Only Selected Tracks — a
    /// transient monitor set (track ids, sorted). While set, exactly these
    /// tracks play, whatever their Mute / Solo; Restore All Tracks clears
    /// it. Never saved (not in `.lup`), never dirties the project, outside
    /// undo; the song's own Mute / Solo flags are not touched.
    pub monitor: Option<Vec<super::track::TrackId>>,
}

/// The imported file and the project's export fingerprint at that moment
/// (`midi::provenance`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceProvenance {
    /// The file as opened (absolute; empty: imported from memory, no file to
    /// rewrite).
    pub path: String,
    pub size: u64,
    /// `midi::provenance::WordHash` of the file's bytes.
    pub hash: u64,
    /// `PlumDocument::export_fingerprint` right after the import.
    pub fingerprint: u64,
}

impl Default for ProjectInfo {
    fn default() -> Self {
        ProjectInfo {
            name: DEFAULT_PROJECT_NAME.to_owned(),
            keys: KeyRange::Standard,
            soundfont: None,
            onion: OnionConfig::default(),
            looping: LoopConfig::default(),
            graphs: GraphConfig::default(),
            groups: super::group::GroupTable::new(),
            zoom_regions: Vec::new(),
            reference: None,
            folders: Default::default(),
            folders_collapsed: Default::default(),
            track_list: None,
            source: None,
            plugin_data: Vec::new(),
            metadata: Default::default(),
            unknown_sections: Vec::new(),
            monitor: None,
        }
    }
}

/// Validates a project name typed by the user: trimmed, non-empty, at most
/// [`MAX_PROJECT_NAME_CHARS`] characters, no control characters.
pub fn parse_project_name(input: &str) -> Result<String, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("Enter a project name.".into());
    }
    if t.chars().count() > MAX_PROJECT_NAME_CHARS {
        return Err(format!(
            "The project name can be at most {MAX_PROJECT_NAME_CHARS} characters."
        ));
    }
    if t.chars().any(char::is_control) {
        return Err("The project name cannot contain control characters.".into());
    }
    Ok(t.to_owned())
}

#[cfg(test)]
mod tests {
    /// B1-0064 Per-Track Settings: every track gets back its own mode and
    /// list when it becomes active; off, nothing is swapped.
    #[test]
    fn per_track_onion_settings_follow_the_active_track() {
        let (a, b, c) = (TrackId(1), TrackId(2), TrackId(3));
        let mut o = OnionConfig {
            mode: OnionMode::Specific,
            specific: vec![c],
            ..OnionConfig::default()
        };
        // Off: nothing kept or swapped.
        assert!(!o.switch_track(Some(a), b));
        assert!(o.track_entries.is_empty());
        o.per_track = true;
        // a → b: a keeps [c]; b has nothing of its own, so [c] stays in force.
        assert!(o.switch_track(Some(a), b));
        assert_eq!(o.track_entries.len(), 1);
        assert_eq!(o.specific, vec![c]);
        // b chooses Previous + Next; back to a brings [c]; to b again P+N.
        o.mode = OnionMode::PreviousAndNext;
        o.switch_track(Some(b), a);
        assert_eq!((o.mode, o.specific.clone()), (OnionMode::Specific, vec![c]));
        o.switch_track(Some(a), b);
        assert_eq!(o.mode, OnionMode::PreviousAndNext);
        assert_eq!(
            o.track_entries.iter().map(|e| e.track).collect::<Vec<_>>(),
            vec![a, b],
            "sorted by id"
        );
        // The same track again changes nothing; off keeps the entries.
        assert!(!o.switch_track(Some(b), b));
        o.per_track = false;
        assert!(!o.switch_track(Some(b), a));
        assert_eq!(o.mode, OnionMode::PreviousAndNext);
        assert_eq!(o.track_entries.len(), 2);
    }

    /// B1-0064: one track at a time from any mode; the active track never;
    /// the last one hidden turns Onion off and keeps the list.
    #[test]
    fn onion_tracks_are_shown_and_hidden_one_by_one_from_any_mode() {
        let ids: Vec<TrackId> = (10..15).map(TrackId).collect();
        let base = OnionConfig {
            mode: OnionMode::PreviousAndNext,
            ..OnionConfig::default()
        };
        // Previous + Next of track 2 = tracks 1 and 3; add track 4.
        let c = base.with_tracks(&ids, 2, &[4], true);
        assert_eq!(c.mode, OnionMode::Specific);
        assert_eq!(c.specific, vec![ids[1], ids[3], ids[4]]);
        assert_eq!(c.sources(&ids, 2), vec![1, 3, 4]);
        // The active track cannot be added; hiding 1 and 3 keeps 4.
        let c = c.with_tracks(&ids, 2, &[2, 1, 3], false);
        assert_eq!(c.specific, vec![ids[4]]);
        let c2 = c.with_tracks(&ids, 2, &[2], true);
        assert_eq!(c2.specific, vec![ids[4]], "never its own ghost");
        // The last one hidden: Off, nothing drawn.
        let off = c.with_tracks(&ids, 2, &[4], false);
        assert_eq!(off.mode, OnionMode::Off);
        assert!(off.sources(&ids, 2).is_empty());
        // From Off, a click shows exactly that track.
        let one = OnionConfig::default().with_tracks(&ids, 0, &[3], true);
        assert_eq!(
            (one.mode, one.specific.clone()),
            (OnionMode::Specific, vec![ids[3]])
        );
        // Ids of deleted tracks stay in the list; opacity / order untouched.
        let kept = OnionConfig {
            mode: OnionMode::Specific,
            specific: vec![TrackId(99), ids[1]],
            opacity: 80,
            ..OnionConfig::default()
        }
        .with_tracks(&ids, 0, &[2], true);
        assert_eq!(kept.specific, vec![ids[1], ids[2], TrackId(99)]);
        assert_eq!(kept.opacity, 80);
        // Out-of-range indices are ignored.
        assert_eq!(
            base.with_tracks(&ids, 2, &[9], true).specific,
            vec![ids[1], ids[3]]
        );
    }

    use super::*;

    fn b(s: &str) -> Bpm {
        Bpm::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn bpm_parse_and_display() {
        for (input, shown) in [
            ("120", "120"),
            ("0.5", "0.5"),
            (".5", "0.5"),
            ("5.", "5"),
            ("360", "360"),
            ("1000", "1000"),
            ("10000", "10000"),
            ("100000", "100000"),
            ("1e6", "1000000"),
            ("1E6", "1000000"),
            ("+174.500", "174.5"),
            ("2.5e-3", "0.0025"),
            ("0.000001", "0.000001"),
            ("1e12", "1000000000000"),
            ("123456789.123456789", "123456789.123456789"),
            ("007", "7"),
        ] {
            assert_eq!(b(input).to_string(), shown, "{input}");
        }
        assert_eq!(b("120"), b("120.000"));
        assert_eq!(b("1.2e2"), b("120"));
        assert_eq!(b("120"), Bpm::DEFAULT);
    }

    #[test]
    fn bpm_rejects_invalid() {
        use BpmError::*;
        for (input, err) in [
            ("", Empty),
            ("   ", Empty),
            ("0", Zero),
            ("0.000", Zero),
            ("-120", Negative),
            ("-0", Negative),
            ("NaN", NotANumber("NaN".into())),
            ("inf", NotANumber("inf".into())),
            ("infinity", NotANumber("infinity".into())),
            ("120,5", DecimalComma),
            ("1.2.3", NotANumber("1.2.3".into())),
            ("12a", NotANumber("12a".into())),
            ("1e", NotANumber("1e".into())),
            ("e5", NotANumber("e5".into())),
            ("0.0000001", TooSmall),
            ("1.0000000000001e12", TooLarge),
            ("2e12", TooLarge),
            ("1e999999999", TooLarge),
            ("1e-999999999", TooSmall),
            ("1234567890.1234567891", TooManyDigits),
        ] {
            assert_eq!(Bpm::parse(input), Err(err), "{input}");
        }
    }

    /// B1-0101: typed tempos are checked against what MIDI can store.
    #[test]
    fn midi_tempo_range_is_checked_and_rounding_disclosed() {
        assert_eq!(Bpm::parse_midi("60000000"), Ok(b("60000000")));
        assert!(Bpm::parse_midi("60000000.0000001").is_err());
        assert!(Bpm::parse_midi("1e9").unwrap_err().contains("60,000,000"));
        assert_eq!(Bpm::parse_midi(MIDI_BPM_MIN_TEXT), Ok(b(MIDI_BPM_MIN_TEXT)));
        assert_eq!(
            b(MIDI_BPM_MIN_TEXT).to_micros_per_quarter().micros,
            MAX_MIDI_TEMPO_MICROS
        );
        assert!(Bpm::parse_midi("3.5762785").is_err());
        assert!(Bpm::parse_midi("0.5").is_err());
        assert!(Bpm::parse_midi("abc").is_err());
        assert_eq!(b("120").midi_note(), None);
        assert_eq!(
            b("123.456").midi_note().as_deref(),
            Some("A MIDI file stores 486,003 µs per quarter note (123.45602804921 BPM).")
        );
    }

    #[test]
    fn bpm_to_midi_tempo() {
        let t = b("120").to_micros_per_quarter();
        assert_eq!((t.micros, t.fidelity), (500_000, TempoFidelity::Exact));
        let t = b("1e6").to_micros_per_quarter();
        assert_eq!((t.micros, t.fidelity), (60, TempoFidelity::Exact));
        let t = b("123.456").to_micros_per_quarter();
        assert_eq!(t.micros, 486_003); // 486,003.11…
        assert_eq!(t.fidelity, TempoFidelity::Rounded);
        let t = b("700000").to_micros_per_quarter(); // 85.71 µs → 86: 0.33 % off
        assert_eq!(
            (t.micros, t.fidelity),
            (86, TempoFidelity::RoundedMaterially)
        );
        let t = b("0.5").to_micros_per_quarter(); // 120 s per quarter: too slow
        assert_eq!(
            (t.micros, t.fidelity),
            (MAX_MIDI_TEMPO_MICROS, TempoFidelity::Clamped)
        );
        let t = b("1e9").to_micros_per_quarter(); // 0.06 µs: too fast
        assert_eq!((t.micros, t.fidelity), (1, TempoFidelity::Clamped));
        // Boundary: 60,000,000 BPM = 1 µs exactly.
        let t = b("60000000").to_micros_per_quarter();
        assert_eq!((t.micros, t.fidelity), (1, TempoFidelity::Exact));
        assert!(TempoFidelity::Clamped.is_material());
        assert!(!TempoFidelity::Rounded.is_material());
        assert_eq!(Bpm::from_micros_per_quarter(500_000), Some(b("120")));
        assert_eq!(
            Bpm::from_micros_per_quarter(486_003).map(|x| x.to_string()),
            Some("123.45602804921".into())
        );
        assert_eq!(Bpm::from_micros_per_quarter(0), None);
        // Extremes stay within the representation.
        assert!(Bpm::from_micros_per_quarter(MAX_MIDI_TEMPO_MICROS).is_some());
        assert_eq!(Bpm::from_micros_per_quarter(1), Some(b("60000000")));
    }

    #[test]
    fn onion_sources() {
        let ids = [TrackId(0), TrackId(5), TrackId(2), TrackId(9)];
        let mut c = OnionConfig::default();
        assert!(c.sources(&ids, 1).is_empty());
        c.mode = OnionMode::Previous;
        assert_eq!(c.sources(&ids, 0), Vec::<usize>::new());
        assert_eq!(c.sources(&ids, 2), vec![1]);
        c.mode = OnionMode::Next;
        assert_eq!(c.sources(&ids, 3), Vec::<usize>::new());
        assert_eq!(c.sources(&ids, 0), vec![1]);
        c.mode = OnionMode::PreviousAndNext;
        assert_eq!(c.sources(&ids, 1), vec![0, 2]);
        assert_eq!(c.sources(&ids, 0), vec![1]);
        c.mode = OnionMode::Specific;
        c.specific = vec![TrackId(9), TrackId(5), TrackId(77)];
        assert_eq!(c.sources(&ids, 1), vec![3]); // the active track is excluded
        assert_eq!(c.sources(&ids, 0), vec![1, 3]);
        c.mode = OnionMode::AllOthers;
        assert_eq!(c.sources(&ids, 2), vec![0, 1, 3]);
        assert!(c.sources(&ids, 9).is_empty());
        for m in OnionMode::ALL {
            assert_eq!(OnionMode::from_code(m.code()), Some(m));
        }
        assert_eq!(OnionMode::from_code(6), None);
    }

    /// B2-D01: with the Conductor selected no musical track is "active" and
    /// none is excluded: the relative modes show the whole song, Specific its
    /// list; with a track active, `sources_at(Some(a))` is `sources(a)`.
    #[test]
    fn conductor_onion_sources_exclude_no_musical_track() {
        let ids = [TrackId(0), TrackId(5), TrackId(2), TrackId(9)];
        let mut c = OnionConfig::default();
        assert!(c.sources_at(&ids, None).is_empty(), "Off shows nothing");
        let expect = [
            (OnionMode::Previous, vec![0, 1, 2, 3]),
            (OnionMode::Next, vec![0, 1, 2, 3]),
            (OnionMode::PreviousAndNext, vec![0, 1, 2, 3]),
            (OnionMode::AllOthers, vec![0, 1, 2, 3]),
        ];
        for (mode, want) in expect {
            c.mode = mode;
            assert_eq!(c.sources_at(&ids, None), want, "{mode:?}");
            assert!(
                c.sources_at(&[], None).is_empty(),
                "{mode:?} without tracks"
            );
            for a in 0..=ids.len() {
                assert_eq!(
                    c.sources_at(&ids, Some(a)),
                    c.sources(&ids, a),
                    "{mode:?} {a}"
                );
            }
        }
        c.mode = OnionMode::Specific;
        c.specific = vec![TrackId(0), TrackId(9), TrackId(77)];
        assert_eq!(
            c.sources_at(&ids, None),
            vec![0, 3],
            "the first track is not excluded"
        );
        assert_eq!(c.sources_at(&ids, Some(0)), vec![3]);
        // One track (the owner's 060457 case): it is the Conductor's source.
        c.mode = OnionMode::PreviousAndNext;
        assert_eq!(c.sources_at(&ids[..1], None), vec![0]);
        assert!(
            c.sources(&ids[..1], 0).is_empty(),
            "the track itself has no ghost"
        );
    }

    #[test]
    fn project_names() {
        assert_eq!(parse_project_name("  My Song "), Ok("My Song".into()));
        assert!(parse_project_name("   ").is_err());
        assert!(parse_project_name("a\u{7}b").is_err());
        assert!(parse_project_name(&"x".repeat(201)).is_err());
        assert!(parse_project_name(&"ü".repeat(200)).is_ok());
    }
}
