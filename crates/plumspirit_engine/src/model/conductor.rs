//! The Conductor: the one project-wide authority for musical time.
//!
//! Every document has exactly one [`Conductor`]. It is not a track: it lives beside
//! the track list, owns no notes, uses no MIDI channel and never sounds. It holds
//!
//! * the **tempo map** — [`TempoPoint`]s, each either a *step* (the tempo holds
//!   until the next point) or the start of a *linear ramp* to the next point;
//! * the **time-signature map** — [`TimeSignature`] changes;
//! * **markers** — point markers and chapter spans ([`Marker`]);
//! * **master volume** — the project's global output level over time
//!   ([`MasterVolumePoint`]), written to MIDI files as Universal Real Time
//!   SysEx *Master Volume* (`F0 7F <device> 04 01 <lsb> <msb> F7`). It is not
//!   Control Change 7 (a channel's volume) and applies to every channel.
//!
//! Playback, the ruler, measure navigation, the playhead, the song length and MIDI
//! export all read these lists; nothing else holds tempo or meter.
//!
//! # Invariants (checked by [`Conductor::validate`])
//!
//! * There is always a tempo point and a time signature at tick 0.
//! * Tempo points and time signatures have strictly increasing ticks (one per
//!   tick: adding at an occupied tick edits the existing entry).
//! * Markers are sorted by `(start, id)`; a chapter's end is after its start (a
//!   zero-length chapter is stored as a point marker).
//! * Ids are unique among all conductor entries and below `next_id`; they are
//!   never reused, so undo, the UI and saved files can refer to entries by id.
//!
//! # Scope
//!
//! Which parts are authoritative depends on the timing of the file
//! ([`ConductorScope`]): metrical Format 0/1 projects (every new project) use all
//! of it. SMPTE-timed files have no tempo or bars, so their tempo and meter events
//! stay as they were and only markers move here. Format 2 files are independent
//! sequences, each with its own timing, so nothing is centralised for them.
//!
//! Edits are prepared by the `prepare_*` functions, which validate the complete
//! result and return a [`ConductorEdit`] (a small patch) without touching the
//! conductor; the undo history applies and reverts those patches.

use super::events::{RawEvent, RawKind, display_text, meta};
use super::patch::ListPatch;
use super::project::Bpm;
use super::tempo::TempoValue;
use super::track::{Track, TrackId};
use crate::midi::seqspec;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

/// Largest time-signature denominator power (2^7 = 128).
pub const MAX_DENOM_POW: u8 = 7;
/// Longest marker title, in characters.
pub const MAX_MARKER_TITLE_CHARS: usize = 500;
/// Meta event type of a Marker.
pub const META_MARKER: u8 = 0x06;
/// Meta event type of a Cue Point.
pub const META_CUE: u8 = 0x07;

/// Shape of the tempo between a point and the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Interp {
    /// The tempo holds until the next point.
    #[default]
    Step,
    /// The BPM changes linearly (in ticks) until it reaches the next point's BPM.
    Linear,
}

impl Interp {
    pub fn label(self) -> &'static str {
        match self {
            Interp::Step => "Step",
            Interp::Linear => "Linear ramp",
        }
    }
}

/// Where an imported conductor event came from, so an unchanged file exports its
/// tempo map at the same place. Only used for ordering on export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub track: TrackId,
    /// Note events before it at the same tick (see `RawEvent::anchor`).
    pub anchor: u32,
    /// Other (non-conductor) events of that track before it at the same tick.
    pub rank: u32,
    /// Conductor events of that track before it at the same tick (keeps
    /// adjacent tempo/signature/marker events in their original order).
    pub seq: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempoPoint {
    pub id: u32,
    pub tick: u64,
    pub value: TempoValue,
    /// Shape of the segment that starts at this point.
    pub interp: Interp,
    /// The MIDI default (120 BPM) assumed at tick 0 of a file without a tempo
    /// event there. Not written to exported files until it is edited.
    pub implicit: bool,
    pub origin: Option<Origin>,
}

impl TempoPoint {
    /// BPM as a float (display, ramp math).
    pub fn bpm_f64(&self) -> f64 {
        tempo_bpm_f64(self.value)
    }
}

/// BPM of a tempo value as a float (display and ramp math only).
pub fn tempo_bpm_f64(v: TempoValue) -> f64 {
    match v {
        TempoValue::Micros(us) => 60_000_000.0 / f64::from(us.max(1)),
        TempoValue::Bpm(b) => b.to_f64(),
    }
}

/// Human-readable BPM: exact for decimal BPMs, rounded for MIDI tempo values.
pub fn format_tempo(v: TempoValue) -> String {
    match v {
        TempoValue::Bpm(b) => b.to_string(),
        TempoValue::Micros(us) => match Bpm::from_micros_per_quarter(us) {
            Some(b) => {
                // MIDI tempos rarely give short decimals; show at most 4 decimals.
                let f = b.to_f64();
                let s = format!("{f:.4}");
                let s = s.trim_end_matches('0').trim_end_matches('.');
                s.to_owned()
            }
            None => "?".into(),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSignature {
    pub id: u32,
    pub tick: u64,
    /// Beats per bar (1–255).
    pub numerator: u8,
    /// The beat unit is 2^denom_pow (0 = whole note … 7 = 128th).
    pub denom_pow: u8,
    /// MIDI clocks per metronome click (kept from imported files).
    pub clocks: u8,
    /// Notated 32nd notes per MIDI quarter note (kept from imported files).
    pub thirty_seconds: u8,
    /// The 4/4 assumed at tick 0 of a file without a time signature there.
    pub implicit: bool,
    pub origin: Option<Origin>,
}

impl TimeSignature {
    pub fn denominator(&self) -> u32 {
        1u32 << self.denom_pow
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.numerator, self.denominator())
    }
}

/// Metronome clicks for a newly entered signature: one click per beat unit.
pub fn default_clocks(denom_pow: u8) -> u8 {
    // 24 MIDI clocks per quarter note.
    let clocks = 96u32 >> u32::from(denom_pow.min(MAX_DENOM_POW));
    clocks.max(1) as u8
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub id: u32,
    pub start: u64,
    /// `Some(end)` for a chapter (end > start), `None` for a point marker.
    pub end: Option<u64>,
    pub title: String,
    /// The original bytes of an imported title that is not valid UTF-8 (exported
    /// unchanged until the title is edited).
    pub raw_title: Option<Box<[u8]>>,
    /// Imported from a Cue Point (FF 07) rather than a Marker (FF 06).
    pub cue: bool,
    pub origin: Option<Origin>,
}

impl Marker {
    pub fn is_chapter(&self) -> bool {
        self.end.is_some()
    }

    /// Chapter length in ticks (0 for a point marker).
    pub fn duration(&self) -> u64 {
        self.end.map_or(0, |e| e - self.start)
    }

    /// Last tick the marker covers.
    pub fn last_tick(&self) -> u64 {
        self.end.unwrap_or(self.start)
    }

    /// The title bytes written to MIDI files.
    pub fn title_bytes(&self) -> &[u8] {
        match &self.raw_title {
            Some(b) => b,
            None => self.title.as_bytes(),
        }
    }
}

/// Largest Master Volume value (14 bits): full level.
pub const MASTER_VOLUME_MAX: u16 = 16_383;
/// SysEx device id meaning "all devices".
pub const SYSEX_ALL_DEVICES: u8 = 0x7F;

/// A Master Volume change: the project's output level from `tick` on (it holds
/// until the next point; before the first point the level is full).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterVolumePoint {
    pub id: u32,
    pub tick: u64,
    /// 14-bit level, 0 (silent) … 16383 (full); applied as a linear gain of
    /// `value / 16383`.
    pub value: u16,
    /// Device id of the SysEx message (0x7F = all devices; kept from imports).
    pub device: u8,
    pub origin: Option<Origin>,
}

impl MasterVolumePoint {
    /// Linear gain 0.0–1.0.
    pub fn gain(&self) -> f32 {
        master_gain(self.value)
    }
}

/// Linear gain of a Master Volume value.
pub fn master_gain(value: u16) -> f32 {
    f32::from(value.min(MASTER_VOLUME_MAX)) / f32::from(MASTER_VOLUME_MAX)
}

/// "78.4% (−2.1 dB)" for a Master Volume value.
pub fn format_master_volume(value: u16) -> String {
    let g = f64::from(value.min(MASTER_VOLUME_MAX)) / f64::from(MASTER_VOLUME_MAX);
    if value == 0 {
        "0% (silent)".into()
    } else {
        let db = 20.0 * g.log10();
        let db = if db.abs() < 0.05 { 0.0 } else { db };
        format!(
            "{:.1}% ({}{:.1} dB)",
            g * 100.0,
            if db < 0.0 { "−" } else { "" },
            db.abs()
        )
    }
}

/// Parses a Master Volume typed as a percentage ("80", "80%", "12.5") or a raw
/// 14-bit value with a leading `#` ("#16383").
pub fn parse_master_volume(input: &str) -> Result<u16, ConductorError> {
    let t = input.trim();
    if let Some(raw) = t.strip_prefix('#') {
        return match raw.trim().parse::<u32>() {
            Ok(v) if v <= u32::from(MASTER_VOLUME_MAX) => Ok(v as u16),
            _ => Err(ConductorError::InvalidMasterVolume),
        };
    }
    let t = t.trim_end_matches('%').trim();
    let v: f64 = t.parse().map_err(|_| ConductorError::InvalidMasterVolume)?;
    if !v.is_finite() || !(0.0..=100.0).contains(&v) || t.contains(['e', 'E']) {
        return Err(ConductorError::InvalidMasterVolume);
    }
    Ok((v / 100.0 * f64::from(MASTER_VOLUME_MAX)).round() as u16)
}

/// The SysEx payload (after `F0`, including the final `F7`) of a Master Volume
/// message.
pub fn master_volume_sysex(device: u8, value: u16) -> [u8; 7] {
    let v = value.min(MASTER_VOLUME_MAX);
    [
        0x7F,
        device & 0x7F,
        0x04,
        0x01,
        (v & 0x7F) as u8,
        ((v >> 7) & 0x7F) as u8,
        0xF7,
    ]
}

/// Decodes a Universal Real Time Master Volume SysEx payload (after `F0`).
pub fn parse_master_volume_sysex(p: &[u8]) -> Option<(u8, u16)> {
    match p {
        [0x7F, dev, 0x04, 0x01, lsb, msb, 0xF7] if *dev < 0x80 && *lsb < 0x80 && *msb < 0x80 => {
            Some((*dev, u16::from(*lsb) | (u16::from(*msb) << 7)))
        }
        _ => None,
    }
}

/// What the conductor is authoritative for (see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConductorScope {
    /// Metrical Format 0/1: tempo, meter and markers.
    Timeline,
    /// SMPTE (or zero-PPQN) Format 0/1: markers only.
    MarkersOnly,
    /// Format 2 (independent sequences): nothing is centralised.
    Sequences,
}

impl ConductorScope {
    pub fn code(self) -> u8 {
        match self {
            ConductorScope::Timeline => 0,
            ConductorScope::MarkersOnly => 1,
            ConductorScope::Sequences => 2,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        match c {
            0 => Some(ConductorScope::Timeline),
            1 => Some(ConductorScope::MarkersOnly),
            2 => Some(ConductorScope::Sequences),
            _ => None,
        }
    }

    /// Why tempo/meter cannot be edited in this scope (for the UI).
    pub fn timing_note(self) -> Option<&'static str> {
        match self {
            ConductorScope::Timeline => None,
            ConductorScope::MarkersOnly => Some(
                "This file uses SMPTE timing (real time, no beats), so it has no tempo or bars to edit.",
            ),
            ConductorScope::Sequences => Some(
                "This is a Format 2 MIDI file: every track is an independent sequence with its own tempo and time signatures, which are kept as they are.",
            ),
        }
    }
}

/// Why a conductor edit was refused. Nothing changes when one is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConductorError {
    /// The tick-0 tempo / signature cannot be deleted or moved away from 0.
    StartRequired {
        what: &'static str,
    },
    NoSuchEntry,
    InvalidMasterVolume,
    InvalidNumerator,
    InvalidDenominator,
    InvalidTempo,
    EmptyTitle,
    TitleTooLong,
    TitleControlChars,
    EndBeforeStart,
    TickOverflow,
    /// Tempo or meter cannot be edited in this project (SMPTE / Format 2).
    NotEditable(&'static str),
    /// The conductor changed after the edit was prepared.
    Stale,
    TooManyEntries,
}

impl fmt::Display for ConductorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConductorError::StartRequired { what } => write!(
                f,
                "The song needs a {what} at its very beginning (tick 0). Change its value instead of deleting or moving it."
            ),
            ConductorError::NoSuchEntry => write!(f, "That entry no longer exists."),
            ConductorError::InvalidMasterVolume => write!(
                f,
                "Enter the master volume as a percentage from 0 to 100 (or a raw value from #0 to #16383)."
            ),
            ConductorError::InvalidNumerator => {
                write!(
                    f,
                    "The number of beats per bar must be a whole number from 1 to 255."
                )
            }
            ConductorError::InvalidDenominator => write!(
                f,
                "The beat unit must be 1, 2, 4, 8, 16, 32, 64 or 128 (a power of two, as in MIDI files)."
            ),
            ConductorError::InvalidTempo => write!(f, "The tempo must be greater than 0 BPM."),
            ConductorError::EmptyTitle => write!(f, "Enter a title for the marker."),
            ConductorError::TitleTooLong => write!(
                f,
                "A marker title can be at most {MAX_MARKER_TITLE_CHARS} characters."
            ),
            ConductorError::TitleControlChars => {
                write!(f, "A marker title cannot contain control characters.")
            }
            ConductorError::EndBeforeStart => {
                write!(f, "The chapter must end after it starts.")
            }
            ConductorError::TickOverflow => {
                write!(f, "That position is beyond the largest possible tick.")
            }
            ConductorError::NotEditable(why) => f.write_str(why),
            ConductorError::Stale => write!(
                f,
                "The tempo map, time signatures or markers changed meanwhile; nothing was changed."
            ),
            ConductorError::TooManyEntries => {
                write!(f, "The Conductor cannot hold more entries.")
            }
        }
    }
}

impl std::error::Error for ConductorError {}

/// A validated change to the conductor (see [`Conductor::apply`]).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ConductorEdit {
    pub tempo: ListPatch<TempoPoint>,
    pub meter: ListPatch<TimeSignature>,
    pub markers: ListPatch<Marker>,
    pub master: ListPatch<MasterVolumePoint>,
    /// `(before, after)` of the id counter.
    pub next_id: (u32, u32),
    /// Conductor revision the edit was prepared from.
    pub base_revision: u64,
}

impl ConductorEdit {
    pub fn is_empty(&self) -> bool {
        self.tempo.is_empty()
            && self.meter.is_empty()
            && self.markers.is_empty()
            && self.master.is_empty()
    }

    pub fn heap_bytes(&self) -> usize {
        self.tempo.heap_bytes()
            + self.meter.heap_bytes()
            + self.markers.heap_bytes()
            + self.master.heap_bytes()
            + self
                .markers
                .items()
                .map(|m| m.title.capacity() + m.raw_title.as_ref().map_or(0, |r| r.len()))
                .sum::<usize>()
    }

    pub fn touches_tempo(&self) -> bool {
        !self.tempo.is_empty()
    }

    pub fn touches_meter(&self) -> bool {
        !self.meter.is_empty()
    }

    pub fn touches_markers(&self) -> bool {
        !self.markers.is_empty()
    }

    pub fn touches_master(&self) -> bool {
        !self.master.is_empty()
    }
}

/// See module docs.
#[derive(Clone, Debug)]
pub struct Conductor {
    scope: ConductorScope,
    tempo: Vec<TempoPoint>,
    meter: Vec<TimeSignature>,
    markers: Vec<Marker>,
    /// `marker_reach[i]` = the latest tick any of `markers[..=i]` reaches (prefix
    /// maximum), so viewport queries skip markers that end before the view.
    marker_reach: Vec<u64>,
    master: Vec<MasterVolumePoint>,
    next_id: u32,
    revision: u64,
    tempo_revision: u64,
    meter_revision: u64,
    marker_revision: u64,
    master_revision: u64,
}

/// The MIDI default tempo (500,000 µs per quarter note = 120 BPM).
pub const DEFAULT_TEMPO: TempoValue = TempoValue::Micros(500_000);

impl Conductor {
    /// A conductor with the MIDI defaults (implicit 120 BPM and 4/4 at tick 0).
    pub fn with_defaults(scope: ConductorScope) -> Self {
        Conductor {
            scope,
            tempo: vec![TempoPoint {
                id: 1,
                tick: 0,
                value: DEFAULT_TEMPO,
                interp: Interp::Step,
                implicit: true,
                origin: None,
            }],
            meter: vec![TimeSignature {
                id: 2,
                tick: 0,
                numerator: 4,
                denom_pow: 2,
                clocks: 24,
                thirty_seconds: 8,
                implicit: true,
                origin: None,
            }],
            markers: Vec::new(),
            marker_reach: Vec::new(),
            master: Vec::new(),
            next_id: 3,
            revision: 0,
            tempo_revision: 0,
            meter_revision: 0,
            marker_revision: 0,
            master_revision: 0,
        }
    }

    /// The conductor of a new project: `bpm` and 4/4 at tick 0, both explicit.
    pub fn new_project(bpm: Bpm) -> Self {
        let mut c = Conductor::with_defaults(ConductorScope::Timeline);
        c.tempo[0].value = TempoValue::Bpm(bpm);
        c.tempo[0].implicit = false;
        c.meter[0].implicit = false;
        c
    }

    /// Assembles a conductor from stored parts (native projects, adoption).
    /// Validates every invariant.
    pub fn from_parts(
        scope: ConductorScope,
        tempo: Vec<TempoPoint>,
        meter: Vec<TimeSignature>,
        markers: Vec<Marker>,
        next_id: u32,
    ) -> Result<Self, String> {
        Conductor::from_parts_with_master(scope, tempo, meter, markers, Vec::new(), next_id)
    }

    /// [`Conductor::from_parts`] with Master Volume points.
    pub fn from_parts_with_master(
        scope: ConductorScope,
        tempo: Vec<TempoPoint>,
        meter: Vec<TimeSignature>,
        markers: Vec<Marker>,
        master: Vec<MasterVolumePoint>,
        next_id: u32,
    ) -> Result<Self, String> {
        let mut c = Conductor {
            scope,
            tempo,
            meter,
            markers,
            marker_reach: Vec::new(),
            master,
            next_id,
            revision: 0,
            tempo_revision: 0,
            meter_revision: 0,
            marker_revision: 0,
            master_revision: 0,
        };
        c.validate()?;
        c.refresh_reach();
        Ok(c)
    }

    #[inline]
    pub fn scope(&self) -> ConductorScope {
        self.scope
    }

    /// Tempo and meter are authoritative and editable here.
    pub fn timing_editable(&self) -> bool {
        self.scope == ConductorScope::Timeline
    }

    pub fn markers_editable(&self) -> bool {
        self.scope != ConductorScope::Sequences
    }

    #[inline]
    pub fn tempo_points(&self) -> &[TempoPoint] {
        &self.tempo
    }

    #[inline]
    pub fn time_signatures(&self) -> &[TimeSignature] {
        &self.meter
    }

    #[inline]
    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Master Volume changes, by tick.
    #[inline]
    pub fn master_volume(&self) -> &[MasterVolumePoint] {
        &self.master
    }

    /// Master Volume can be edited (not for Format 2 sequences, which keep their
    /// own messages).
    pub fn master_editable(&self) -> bool {
        self.scope != ConductorScope::Sequences
    }

    /// Master Volume value in effect at `tick` (full level before the first point).
    pub fn master_at(&self, tick: u64) -> u16 {
        let i = self.master.partition_point(|p| p.tick <= tick);
        i.checked_sub(1)
            .map_or(MASTER_VOLUME_MAX, |j| self.master[j].value)
    }

    pub fn master_by_id(&self, id: u32) -> Option<(usize, &MasterVolumePoint)> {
        self.master.iter().enumerate().find(|(_, p)| p.id == id)
    }

    /// Changes whenever the Master Volume changes.
    pub fn master_revision(&self) -> u64 {
        self.master_revision
    }

    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    /// Changes whenever anything in the conductor changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Changes whenever the tempo map changes.
    pub fn tempo_revision(&self) -> u64 {
        self.tempo_revision
    }

    /// Changes whenever the time-signature map changes.
    pub fn meter_revision(&self) -> u64 {
        self.meter_revision
    }

    /// Changes whenever markers change.
    pub fn marker_revision(&self) -> u64 {
        self.marker_revision
    }

    /// The tempo at tick 0.
    pub fn start_tempo(&self) -> TempoValue {
        self.tempo[0].value
    }

    /// Time signatures as `(tick, numerator, denominator power)` for the grid.
    pub fn meter_events(&self) -> Vec<(u64, u8, u8)> {
        self.meter
            .iter()
            .map(|s| (s.tick, s.numerator, s.denom_pow))
            .collect()
    }

    /// Latest tick any conductor entry reaches (a chapter's end counts), for the
    /// song length. Implicit defaults at tick 0 count as 0.
    pub fn last_tick(&self) -> u64 {
        let t = self.tempo.last().map_or(0, |p| p.tick);
        let m = self.meter.last().map_or(0, |s| s.tick);
        let k = self
            .markers
            .iter()
            .map(Marker::last_tick)
            .max()
            .unwrap_or(0);
        let v = self.master.last().map_or(0, |p| p.tick);
        t.max(m).max(k).max(v)
    }

    /// Index of the tempo point in effect at `tick`.
    pub fn tempo_index_at(&self, tick: u64) -> usize {
        self.tempo
            .partition_point(|p| p.tick <= tick)
            .saturating_sub(1)
    }

    /// BPM at `tick` (display): the point's value for steps, interpolated inside
    /// linear ramps.
    pub fn bpm_at(&self, tick: u64) -> f64 {
        let i = self.tempo_index_at(tick);
        let p = &self.tempo[i];
        match (p.interp, self.tempo.get(i + 1)) {
            (Interp::Linear, Some(n)) if n.tick > p.tick => {
                let b0 = p.bpm_f64();
                let b1 = n.bpm_f64();
                let f = (tick - p.tick) as f64 / (n.tick - p.tick) as f64;
                b0 + (b1 - b0) * f
            }
            _ => p.bpm_f64(),
        }
    }

    pub fn tempo_by_id(&self, id: u32) -> Option<(usize, &TempoPoint)> {
        self.tempo.iter().enumerate().find(|(_, p)| p.id == id)
    }

    pub fn signature_by_id(&self, id: u32) -> Option<(usize, &TimeSignature)> {
        self.meter.iter().enumerate().find(|(_, s)| s.id == id)
    }

    pub fn marker_by_id(&self, id: u32) -> Option<(usize, &Marker)> {
        self.markers.iter().enumerate().find(|(_, m)| m.id == id)
    }

    /// Time signature in effect at `tick`.
    pub fn signature_at(&self, tick: u64) -> &TimeSignature {
        let i = self
            .meter
            .partition_point(|s| s.tick <= tick)
            .saturating_sub(1);
        &self.meter[i]
    }

    /// Markers that intersect `[t0, t1]`, in order. Work is bounded by the markers
    /// near the view: markers ending before `t0` are skipped by binary search on
    /// the prefix maximum of their ends.
    pub fn markers_in_range(&self, t0: u64, t1: u64) -> impl Iterator<Item = &Marker> + '_ {
        self.markers_in_range_indexed(t0, t1).map(|(_, m)| m)
    }

    /// [`Conductor::markers_in_range`] with each marker's position in
    /// [`Conductor::markers`].
    pub fn markers_in_range_indexed(
        &self,
        t0: u64,
        t1: u64,
    ) -> impl Iterator<Item = (usize, &Marker)> + '_ {
        let lo = self.marker_reach.partition_point(|&r| r < t0);
        let hi = self.markers.partition_point(|m| m.start <= t1).max(lo);
        self.markers[lo..hi]
            .iter()
            .enumerate()
            .map(move |(k, m)| (lo + k, m))
            .filter(move |(_, m)| m.last_tick() >= t0)
    }

    /// Heap bytes used (diagnostics).
    pub fn heap_bytes(&self) -> usize {
        self.tempo.capacity() * std::mem::size_of::<TempoPoint>()
            + self.meter.capacity() * std::mem::size_of::<TimeSignature>()
            + self.markers.capacity() * std::mem::size_of::<Marker>()
            + self
                .markers
                .iter()
                .map(|m| m.title.capacity() + m.raw_title.as_ref().map_or(0, |r| r.len()))
                .sum::<usize>()
            + self.master.capacity() * std::mem::size_of::<MasterVolumePoint>()
    }

    /// Checks every invariant (see module docs).
    pub fn validate(&self) -> Result<(), String> {
        self.check_tempo()?;
        self.check_meter()?;
        self.check_markers()?;
        self.check_master()?;
        let mut ids: Vec<u32> = self.ids().collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err("two conductor entries share an id".into());
        }
        if ids.last().is_some_and(|&m| m >= self.next_id) || ids.first() == Some(&0) {
            return Err("a conductor entry id is out of range".into());
        }
        Ok(())
    }

    /// Every entry id (tempo points, then signatures, then markers).
    fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.tempo
            .iter()
            .map(|p| p.id)
            .chain(self.meter.iter().map(|s| s.id))
            .chain(self.markers.iter().map(|m| m.id))
            .chain(self.master.iter().map(|p| p.id))
    }

    fn check_master(&self) -> Result<(), String> {
        if !self.master.is_empty() && !self.master_editable() {
            return Err("Format 2 projects keep master volume in their sequences".into());
        }
        if self.master.windows(2).any(|w| w[0].tick >= w[1].tick) {
            return Err("master volume points are not in strictly increasing tick order".into());
        }
        if self
            .master
            .iter()
            .any(|p| p.value > MASTER_VOLUME_MAX || p.device > 0x7F)
        {
            return Err("a master volume point is out of range".into());
        }
        Ok(())
    }

    fn check_tempo(&self) -> Result<(), String> {
        if self.tempo.first().map(|p| p.tick) != Some(0) {
            return Err("the tempo map has no tempo at tick 0".into());
        }
        if self.tempo.windows(2).any(|w| w[0].tick >= w[1].tick) {
            return Err("tempo points are not in strictly increasing tick order".into());
        }
        for p in &self.tempo {
            match p.value {
                TempoValue::Micros(0) => return Err("a tempo point has a zero tempo".into()),
                TempoValue::Micros(us) if us > 0xFF_FFFF => {
                    return Err("a tempo point is outside the MIDI tempo range".into());
                }
                _ => {}
            }
            if p.implicit && (p.tick != 0 || p.value != DEFAULT_TEMPO) {
                return Err("only the default tempo at tick 0 can be implicit".into());
            }
        }
        Ok(())
    }

    fn check_meter(&self) -> Result<(), String> {
        if self.meter.first().map(|s| s.tick) != Some(0) {
            return Err("there is no time signature at tick 0".into());
        }
        if self.meter.windows(2).any(|w| w[0].tick >= w[1].tick) {
            return Err("time signatures are not in strictly increasing tick order".into());
        }
        for s in &self.meter {
            if s.numerator == 0 || s.denom_pow > MAX_DENOM_POW {
                return Err("a time signature is invalid".into());
            }
            if s.implicit && (s.tick != 0 || s.numerator != 4 || s.denom_pow != 2) {
                return Err("only 4/4 at tick 0 can be implicit".into());
            }
        }
        Ok(())
    }

    fn check_markers(&self) -> Result<(), String> {
        if self
            .markers
            .windows(2)
            .any(|w| (w[0].start, w[0].id) >= (w[1].start, w[1].id))
        {
            return Err("markers are not sorted".into());
        }
        for m in &self.markers {
            if let Some(e) = m.end
                && e <= m.start
            {
                return Err("a chapter ends before it starts".into());
            }
        }
        Ok(())
    }

    /// Validation after a patch, in O(entries) without copying or sorting the
    /// lists: the state before was valid, so only the lists the edit changed
    /// are checked, and ids only for the entries the patch put in (`put`).
    fn check_after(&self, edit: &ConductorEdit, backwards: bool) -> Result<(), String> {
        if edit.touches_tempo() {
            self.check_tempo()?;
        }
        if edit.touches_meter() {
            self.check_meter()?;
        }
        if edit.touches_markers() {
            self.check_markers()?;
        }
        if edit.touches_master() {
            self.check_master()?;
        }
        fn put_ids<T: Clone + PartialEq>(
            p: &ListPatch<T>,
            backwards: bool,
            id: impl Fn(&T) -> u32,
        ) -> Vec<u32> {
            let side = if backwards { p.removed() } else { p.inserted() };
            side.iter().map(|(_, t)| id(t)).collect()
        }
        let mut put = put_ids(&edit.tempo, backwards, |p| p.id);
        put.extend(put_ids(&edit.meter, backwards, |s| s.id));
        put.extend(put_ids(&edit.markers, backwards, |m| m.id));
        put.extend(put_ids(&edit.master, backwards, |p| p.id));
        put.sort_unstable();
        if put.windows(2).any(|w| w[0] == w[1]) {
            return Err("two conductor entries share an id".into());
        }
        let mut seen = vec![false; put.len()];
        let mut max = 0u32;
        for id in self.ids() {
            if id == 0 {
                return Err("a conductor entry id is out of range".into());
            }
            max = max.max(id);
            if let Ok(k) = put.binary_search(&id) {
                if seen[k] {
                    return Err("two conductor entries share an id".into());
                }
                seen[k] = true;
            }
        }
        if max >= self.next_id {
            return Err("a conductor entry id is out of range".into());
        }
        Ok(())
    }

    /// Content equality (ids, flags, origins and values), ignoring revision counters.
    pub fn same_content(&self, other: &Conductor) -> bool {
        self.scope == other.scope
            && self.tempo == other.tempo
            && self.meter == other.meter
            && self.markers == other.markers
            && self.master == other.master
            && self.next_id == other.next_id
    }

    fn refresh_reach(&mut self) {
        self.marker_reach.clear();
        let mut m = 0u64;
        for k in &self.markers {
            m = m.max(k.last_tick());
            self.marker_reach.push(m);
        }
    }

    /// B1-0103: a conductor that replaces `prev` as a whole (Change PPQN)
    /// continues every revision counter after it.
    pub(crate) fn continue_revisions(&mut self, prev: &Conductor) {
        self.revision = prev.revision.wrapping_add(1);
        self.tempo_revision = prev.tempo_revision.wrapping_add(1);
        self.meter_revision = prev.meter_revision.wrapping_add(1);
        self.marker_revision = prev.marker_revision.wrapping_add(1);
        self.master_revision = prev.master_revision.wrapping_add(1);
    }

    // ------------------------------------------------------------ patching

    fn bump(&mut self, edit: &ConductorEdit) {
        self.revision = self.revision.wrapping_add(1);
        if edit.touches_tempo() {
            self.tempo_revision = self.tempo_revision.wrapping_add(1);
        }
        if edit.touches_meter() {
            self.meter_revision = self.meter_revision.wrapping_add(1);
        }
        if edit.touches_markers() {
            self.marker_revision = self.marker_revision.wrapping_add(1);
        }
        if edit.touches_master() {
            self.master_revision = self.master_revision.wrapping_add(1);
        }
    }

    /// Applies a prepared edit. Refuses (changing nothing) when the edit does not
    /// belong to this state or its result would break an invariant.
    pub fn apply(&mut self, edit: &ConductorEdit) -> Result<(), ConductorError> {
        if edit.base_revision != self.revision {
            return Err(ConductorError::Stale);
        }
        self.patch(edit, false)
    }

    /// Undoes an applied edit (the conductor must be in the edit's "after" state).
    pub fn revert(&mut self, edit: &ConductorEdit) -> Result<(), ConductorError> {
        self.patch(edit, true)
    }

    /// Re-applies an edit during redo (no revision check: the undo history owns
    /// the order of states).
    pub fn reapply(&mut self, edit: &ConductorEdit) -> Result<(), ConductorError> {
        self.patch(edit, false)
    }

    /// Applies (or, `backwards`, reverts) `edit` in place. The lists are not
    /// copied: each one is transformed (a patch that does not match leaves its
    /// list unchanged), and every change is undone again when a later list or
    /// the validation refuses, so a refused edit changes nothing.
    fn patch(&mut self, edit: &ConductorEdit, backwards: bool) -> Result<(), ConductorError> {
        fn run<T: Clone + PartialEq>(p: &ListPatch<T>, list: &mut Vec<T>, backwards: bool) -> bool {
            if p.is_empty() {
                true
            } else if backwards {
                p.revert(list)
            } else {
                p.apply(list)
            }
        }
        if !run(&edit.tempo, &mut self.tempo, backwards) {
            return Err(ConductorError::Stale);
        }
        if !run(&edit.meter, &mut self.meter, backwards) {
            run(&edit.tempo, &mut self.tempo, !backwards);
            return Err(ConductorError::Stale);
        }
        if !run(&edit.markers, &mut self.markers, backwards) {
            run(&edit.meter, &mut self.meter, !backwards);
            run(&edit.tempo, &mut self.tempo, !backwards);
            return Err(ConductorError::Stale);
        }
        if !run(&edit.master, &mut self.master, backwards) {
            run(&edit.markers, &mut self.markers, !backwards);
            run(&edit.meter, &mut self.meter, !backwards);
            run(&edit.tempo, &mut self.tempo, !backwards);
            return Err(ConductorError::Stale);
        }
        let old_next = self.next_id;
        self.next_id = if backwards {
            edit.next_id.0
        } else {
            edit.next_id.1
        };
        if self.check_after(edit, backwards).is_err() {
            run(&edit.master, &mut self.master, !backwards);
            run(&edit.markers, &mut self.markers, !backwards);
            run(&edit.meter, &mut self.meter, !backwards);
            run(&edit.tempo, &mut self.tempo, !backwards);
            self.next_id = old_next;
            return Err(ConductorError::Stale);
        }
        debug_assert_eq!(self.validate(), Ok(()));
        if edit.touches_markers() {
            self.refresh_reach();
        }
        self.bump(edit);
        Ok(())
    }

    fn fresh_edit(&self) -> ConductorEdit {
        ConductorEdit {
            next_id: (self.next_id, self.next_id),
            base_revision: self.revision,
            ..ConductorEdit::default()
        }
    }

    fn alloc_id(edit: &mut ConductorEdit) -> Result<u32, ConductorError> {
        let id = edit.next_id.1;
        edit.next_id.1 = id.checked_add(1).ok_or(ConductorError::TooManyEntries)?;
        Ok(id)
    }

    fn require_timing(&self) -> Result<(), ConductorError> {
        match self.scope.timing_note() {
            None => Ok(()),
            Some(why) => Err(ConductorError::NotEditable(why)),
        }
    }

    fn require_markers(&self) -> Result<(), ConductorError> {
        if self.markers_editable() {
            Ok(())
        } else {
            Err(ConductorError::NotEditable(
                ConductorScope::Sequences.timing_note().unwrap_or_default(),
            ))
        }
    }

    // ---------------------------------------------------------------- tempo

    /// Adds a tempo point at `tick` (editing the point already there, if any), or
    /// — with `target` — changes the point with that id (moving it to `tick`
    /// replaces a point already at that tick). The tick-0 point stays at 0.
    pub fn prepare_set_tempo(
        &self,
        target: Option<u32>,
        tick: u64,
        value: TempoValue,
        interp: Interp,
    ) -> Result<ConductorEdit, ConductorError> {
        self.require_timing()?;
        if matches!(value, TempoValue::Micros(0)) {
            return Err(ConductorError::InvalidTempo);
        }
        let mut edit = self.fresh_edit();
        let at = self.tempo.iter().position(|p| p.tick == tick);
        match target {
            None => match at {
                Some(i) => {
                    let old = self.tempo[i];
                    let new = TempoPoint {
                        value,
                        interp,
                        implicit: false,
                        ..old
                    };
                    if new == old {
                        return Ok(edit);
                    }
                    edit.tempo.remove(i, old);
                    edit.tempo.insert(i, new);
                }
                None => {
                    let id = Self::alloc_id(&mut edit)?;
                    let i = self.tempo.partition_point(|p| p.tick < tick);
                    edit.tempo.insert(
                        i,
                        TempoPoint {
                            id,
                            tick,
                            value,
                            interp,
                            implicit: false,
                            origin: None,
                        },
                    );
                }
            },
            Some(id) => {
                let (i, &old) = self.tempo_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
                if i == 0 && tick != 0 {
                    return Err(ConductorError::StartRequired { what: "tempo" });
                }
                let new = TempoPoint {
                    tick,
                    value,
                    interp,
                    implicit: false,
                    ..old
                };
                if new == old {
                    return Ok(edit);
                }
                // Build the new list: remove the point and any other point at the
                // destination tick, then insert.
                let mut removed: Vec<(usize, TempoPoint)> = vec![(i, old)];
                if let Some(j) = at
                    && j != i
                {
                    removed.push((j, self.tempo[j]));
                }
                removed.sort_by_key(|r| r.0);
                let mut after: Vec<TempoPoint> = self
                    .tempo
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| !removed.iter().any(|r| r.0 == *k))
                    .map(|(_, p)| *p)
                    .collect();
                let pos = after.partition_point(|p| p.tick < tick);
                after.insert(pos, new);
                if after.first().map(|p| p.tick) != Some(0) {
                    return Err(ConductorError::StartRequired { what: "tempo" });
                }
                for (k, p) in removed {
                    edit.tempo.remove(k, p);
                }
                edit.tempo.insert(pos, new);
            }
        }
        Ok(edit)
    }

    /// Changes only the value of the tempo at tick 0 (New Project / the tempo field).
    pub fn prepare_set_start_tempo(
        &self,
        value: TempoValue,
    ) -> Result<ConductorEdit, ConductorError> {
        let p = self.tempo[0];
        self.prepare_set_tempo(Some(p.id), 0, value, p.interp)
    }

    /// Changes the shape of the segment starting at the point `id`.
    pub fn prepare_set_interp(
        &self,
        id: u32,
        interp: Interp,
    ) -> Result<ConductorEdit, ConductorError> {
        let (_, p) = self.tempo_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
        self.prepare_set_tempo(Some(id), p.tick, p.value, interp)
    }

    /// Deletes tempo points. The tick-0 point cannot be deleted.
    pub fn prepare_remove_tempo(&self, ids: &[u32]) -> Result<ConductorEdit, ConductorError> {
        self.require_timing()?;
        let mut edit = self.fresh_edit();
        let mut idx: Vec<usize> = Vec::with_capacity(ids.len());
        for &id in ids {
            let (i, _) = self.tempo_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
            if i == 0 {
                return Err(ConductorError::StartRequired { what: "tempo" });
            }
            idx.push(i);
        }
        idx.sort_unstable();
        idx.dedup();
        for i in idx {
            edit.tempo.remove(i, self.tempo[i]);
        }
        Ok(edit)
    }

    // -------------------------------------------------------------- meter

    /// Adds a time signature at `tick` (editing the one already there), or — with
    /// `target` — changes the signature with that id. The tick-0 signature stays
    /// at 0. Imported metronome settings are kept while the signature is unchanged.
    pub fn prepare_set_signature(
        &self,
        target: Option<u32>,
        tick: u64,
        numerator: u8,
        denom_pow: u8,
    ) -> Result<ConductorEdit, ConductorError> {
        self.require_timing()?;
        if numerator == 0 {
            return Err(ConductorError::InvalidNumerator);
        }
        if denom_pow > MAX_DENOM_POW {
            return Err(ConductorError::InvalidDenominator);
        }
        let mut edit = self.fresh_edit();
        let at = self.meter.iter().position(|s| s.tick == tick);
        let make = |base: TimeSignature, tick: u64| {
            let same = base.numerator == numerator && base.denom_pow == denom_pow;
            TimeSignature {
                tick,
                numerator,
                denom_pow,
                clocks: if same {
                    base.clocks
                } else {
                    default_clocks(denom_pow)
                },
                thirty_seconds: if same { base.thirty_seconds } else { 8 },
                implicit: false,
                ..base
            }
        };
        match target {
            None => match at {
                Some(i) => {
                    let old = self.meter[i];
                    let new = make(old, tick);
                    if new == old {
                        return Ok(edit);
                    }
                    edit.meter.remove(i, old);
                    edit.meter.insert(i, new);
                }
                None => {
                    let id = Self::alloc_id(&mut edit)?;
                    let i = self.meter.partition_point(|s| s.tick < tick);
                    edit.meter.insert(
                        i,
                        TimeSignature {
                            id,
                            tick,
                            numerator,
                            denom_pow,
                            clocks: default_clocks(denom_pow),
                            thirty_seconds: 8,
                            implicit: false,
                            origin: None,
                        },
                    );
                }
            },
            Some(id) => {
                let (i, &old) = self
                    .signature_by_id(id)
                    .ok_or(ConductorError::NoSuchEntry)?;
                if i == 0 && tick != 0 {
                    return Err(ConductorError::StartRequired {
                        what: "time signature",
                    });
                }
                let new = make(old, tick);
                if new == old {
                    return Ok(edit);
                }
                let mut removed: Vec<(usize, TimeSignature)> = vec![(i, old)];
                if let Some(j) = at
                    && j != i
                {
                    removed.push((j, self.meter[j]));
                }
                removed.sort_by_key(|r| r.0);
                let mut after: Vec<TimeSignature> = self
                    .meter
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| !removed.iter().any(|r| r.0 == *k))
                    .map(|(_, s)| *s)
                    .collect();
                let pos = after.partition_point(|s| s.tick < tick);
                after.insert(pos, new);
                if after.first().map(|s| s.tick) != Some(0) {
                    return Err(ConductorError::StartRequired {
                        what: "time signature",
                    });
                }
                for (k, s) in removed {
                    edit.meter.remove(k, s);
                }
                edit.meter.insert(pos, new);
            }
        }
        Ok(edit)
    }

    /// Deletes time signatures. The tick-0 signature cannot be deleted.
    pub fn prepare_remove_signatures(&self, ids: &[u32]) -> Result<ConductorEdit, ConductorError> {
        self.require_timing()?;
        let mut edit = self.fresh_edit();
        let mut idx: Vec<usize> = Vec::with_capacity(ids.len());
        for &id in ids {
            let (i, _) = self
                .signature_by_id(id)
                .ok_or(ConductorError::NoSuchEntry)?;
            if i == 0 {
                return Err(ConductorError::StartRequired {
                    what: "time signature",
                });
            }
            idx.push(i);
        }
        idx.sort_unstable();
        idx.dedup();
        for i in idx {
            edit.meter.remove(i, self.meter[i]);
        }
        Ok(edit)
    }

    // ------------------------------------------------------------ markers

    /// Adds a marker, or — with `target` — changes the marker with that id.
    /// `end: Some(e)` makes a chapter; a chapter with `e == start` is stored as a
    /// point marker (documented policy). The title is trimmed and must not be empty.
    pub fn prepare_set_marker(
        &self,
        target: Option<u32>,
        start: u64,
        end: Option<u64>,
        title: &str,
    ) -> Result<ConductorEdit, ConductorError> {
        self.require_markers()?;
        let title = validate_title(title)?;
        let end = match end {
            Some(e) if e < start => return Err(ConductorError::EndBeforeStart),
            Some(e) if e == start => None,
            other => other,
        };
        let mut edit = self.fresh_edit();
        // Index of the marker being replaced (it leaves the list before the new
        // one is placed); the list itself is not copied.
        let mut replaced: Option<usize> = None;
        let new = match target {
            None => Marker {
                id: Self::alloc_id(&mut edit)?,
                start,
                end,
                title,
                raw_title: None,
                cue: false,
                origin: None,
            },
            Some(id) => {
                let (i, old) = self.marker_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
                let keep_raw = old.title == title;
                let new = Marker {
                    id,
                    start,
                    end,
                    raw_title: if keep_raw {
                        old.raw_title.clone()
                    } else {
                        None
                    },
                    title,
                    // A cue point that becomes a chapter is written as a marker.
                    cue: old.cue && end.is_none(),
                    origin: old.origin,
                };
                if &new == old {
                    return Ok(edit);
                }
                edit.markers.remove(i, old.clone());
                replaced = Some(i);
                new
            }
        };
        // Position in the list after the replaced marker was taken out.
        let key = (new.start, new.id);
        let before = self.markers.partition_point(|m| (m.start, m.id) < key);
        let pos = match replaced {
            Some(i) if i < before => before - 1,
            _ => before,
        };
        edit.markers.insert(pos, new);
        Ok(edit)
    }

    /// Deletes markers.
    pub fn prepare_remove_markers(&self, ids: &[u32]) -> Result<ConductorEdit, ConductorError> {
        self.require_markers()?;
        let mut edit = self.fresh_edit();
        let mut idx: Vec<usize> = Vec::with_capacity(ids.len());
        for &id in ids {
            let (i, _) = self.marker_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
            idx.push(i);
        }
        idx.sort_unstable();
        idx.dedup();
        for i in idx {
            edit.markers.remove(i, self.markers[i].clone());
        }
        Ok(edit)
    }

    // ------------------------------------------------------- master volume

    fn require_master(&self) -> Result<(), ConductorError> {
        if self.master_editable() {
            Ok(())
        } else {
            Err(ConductorError::NotEditable(
                "This is a Format 2 MIDI file: every sequence keeps its own master volume messages, which stay as they are.",
            ))
        }
    }

    /// Adds a Master Volume point at `tick` (editing the one already there), or —
    /// with `target` — changes the point with that id (moving it onto another
    /// point replaces that one).
    pub fn prepare_set_master(
        &self,
        target: Option<u32>,
        tick: u64,
        value: u16,
    ) -> Result<ConductorEdit, ConductorError> {
        self.require_master()?;
        if value > MASTER_VOLUME_MAX {
            return Err(ConductorError::InvalidMasterVolume);
        }
        let mut edit = self.fresh_edit();
        let at = self.master.binary_search_by_key(&tick, |p| p.tick).ok();
        match target {
            None => match at {
                Some(i) => {
                    let old = self.master[i];
                    let new = MasterVolumePoint { value, ..old };
                    if new != old {
                        edit.master.remove(i, old);
                        edit.master.insert(i, new);
                    }
                }
                None => {
                    let id = Self::alloc_id(&mut edit)?;
                    let i = self.master.partition_point(|p| p.tick < tick);
                    edit.master.insert(
                        i,
                        MasterVolumePoint {
                            id,
                            tick,
                            value,
                            device: SYSEX_ALL_DEVICES,
                            origin: None,
                        },
                    );
                }
            },
            Some(id) => {
                let (i, &old) = self.master_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
                let new = MasterVolumePoint { tick, value, ..old };
                if new == old {
                    return Ok(edit);
                }
                let mut removed = vec![i];
                if let Some(j) = at
                    && j != i
                {
                    removed.push(j);
                }
                removed.sort_unstable();
                let before = self.master.partition_point(|p| p.tick < tick);
                let pos = before - removed.iter().filter(|&&r| r < before).count();
                for r in removed {
                    edit.master.remove(r, self.master[r]);
                }
                edit.master.insert(pos, new);
            }
        }
        Ok(edit)
    }

    /// Deletes Master Volume points.
    pub fn prepare_remove_master(&self, ids: &[u32]) -> Result<ConductorEdit, ConductorError> {
        self.require_master()?;
        let mut edit = self.fresh_edit();
        let mut idx: Vec<usize> = Vec::with_capacity(ids.len());
        for &id in ids {
            let (i, _) = self.master_by_id(id).ok_or(ConductorError::NoSuchEntry)?;
            idx.push(i);
        }
        idx.sort_unstable();
        idx.dedup();
        for i in idx {
            edit.master.remove(i, self.master[i]);
        }
        Ok(edit)
    }

    /// Writes Master Volume `points` (`(tick, value)`, strictly increasing ticks)
    /// in one edit. `replace`: every point from the first to the last new tick is
    /// replaced; otherwise points at the same ticks take the new values.
    pub fn prepare_master_points(
        &self,
        points: &[(u64, u16)],
        replace: bool,
    ) -> Result<ConductorEdit, ConductorError> {
        self.require_master()?;
        if points.windows(2).any(|w| w[0].0 >= w[1].0)
            || points.iter().any(|p| p.1 > MASTER_VOLUME_MAX)
        {
            return Err(ConductorError::InvalidMasterVolume);
        }
        let mut edit = self.fresh_edit();
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return Ok(edit);
        };
        let (lo, hi) = if replace {
            (
                self.master.partition_point(|p| p.tick < first.0),
                self.master.partition_point(|p| p.tick <= last.0),
            )
        } else {
            (0, 0)
        };
        let mut kept_ids: Vec<Option<(u32, u8, Option<Origin>)>> = vec![None; points.len()];
        let mut removed: Vec<usize> = (lo..hi).collect();
        for (k, &(t, _)) in points.iter().enumerate() {
            if let Ok(i) = self.master.binary_search_by_key(&t, |p| p.tick) {
                let p = self.master[i];
                kept_ids[k] = Some((p.id, p.device, p.origin));
                if !replace {
                    removed.push(i);
                }
            }
        }
        removed.sort_unstable();
        removed.dedup();
        for &i in &removed {
            edit.master.remove(i, self.master[i]);
        }
        for (k, &(t, v)) in points.iter().enumerate() {
            let (id, device, origin) = match kept_ids[k] {
                Some(x) => x,
                None => (Self::alloc_id(&mut edit)?, SYSEX_ALL_DEVICES, None),
            };
            let old_pos = self.master.partition_point(|p| p.tick < t);
            let removed_before = removed.partition_point(|&r| r < old_pos);
            edit.master.insert(
                old_pos - removed_before + k,
                MasterVolumePoint {
                    id,
                    tick: t,
                    value: v,
                    device,
                    origin,
                },
            );
        }
        Ok(edit)
    }

    /// Writes tempo points (`(tick, value, shape)`, strictly increasing ticks) in
    /// one edit (curve tools on the BPM graph). `replace`: every point from the
    /// first to the last new tick is replaced (the song's first point is updated,
    /// never removed); otherwise points at the same ticks take the new values.
    pub fn prepare_tempo_points(
        &self,
        points: &[(u64, TempoValue, Interp)],
        replace: bool,
    ) -> Result<ConductorEdit, ConductorError> {
        self.require_timing()?;
        if points.windows(2).any(|w| w[0].0 >= w[1].0)
            || points.iter().any(|p| matches!(p.1, TempoValue::Micros(0)))
        {
            return Err(ConductorError::InvalidTempo);
        }
        let mut edit = self.fresh_edit();
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return Ok(edit);
        };
        let (lo, hi) = if replace {
            (
                self.tempo.partition_point(|p| p.tick < first.0),
                self.tempo.partition_point(|p| p.tick <= last.0),
            )
        } else {
            (0, 0)
        };
        let mut kept: Vec<Option<TempoPoint>> = vec![None; points.len()];
        let mut removed: Vec<usize> = (lo..hi).collect();
        for (k, &(t, _, _)) in points.iter().enumerate() {
            if let Ok(i) = self.tempo.binary_search_by_key(&t, |p| p.tick) {
                kept[k] = Some(self.tempo[i]);
                if !replace {
                    removed.push(i);
                }
            }
        }
        removed.sort_unstable();
        removed.dedup();
        // The tick-0 point stays (only its value may change).
        if removed.first() == Some(&0) && points[0].0 != 0 {
            removed.remove(0);
        }
        for &i in &removed {
            edit.tempo.remove(i, self.tempo[i]);
        }
        for (k, &(t, value, interp)) in points.iter().enumerate() {
            let p = match kept[k] {
                Some(old) => TempoPoint {
                    value,
                    interp,
                    implicit: false,
                    ..old
                },
                None => TempoPoint {
                    id: Self::alloc_id(&mut edit)?,
                    tick: t,
                    value,
                    interp,
                    implicit: false,
                    origin: None,
                },
            };
            let old_pos = self.tempo.partition_point(|q| q.tick < t);
            let removed_before = removed.partition_point(|&r| r < old_pos);
            edit.tempo.insert(old_pos - removed_before + k, p);
        }
        Ok(edit)
    }
}

/// What [`Conductor::adopt`] found in the tracks of an imported file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptReport {
    /// Tempo changes that became Conductor tempo points.
    pub tempo: usize,
    /// Tempo events at a tick where a later one (in track order) takes effect.
    pub tempo_shadowed: usize,
    /// …of which had a different value (a real conflict).
    pub tempo_conflicts: usize,
    pub meter: usize,
    pub meter_shadowed: usize,
    pub meter_conflicts: usize,
    /// Marker / Cue Point events that became point markers.
    pub markers: usize,
    /// PlumSpirit chapter records that became chapters.
    pub chapters: usize,
    /// Universal Master Volume messages that became Master Volume points.
    pub master: usize,
    /// Master Volume messages at a tick where a later one takes effect.
    pub master_shadowed: usize,
    /// Adopted events that were not in the first track (they are written to the
    /// first track on export).
    pub from_other_tracks: usize,
}

impl AdoptReport {
    /// Plain-language notes for the import summary (empty when nothing notable).
    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.tempo_shadowed > 0 {
            out.push(format!(
                "{} tempo event(s) shared a tick with a later tempo event and never took effect{}; the Conductor keeps the one that plays (the last in track order).",
                self.tempo_shadowed,
                if self.tempo_conflicts > 0 {
                    format!(" ({} with a different tempo)", self.tempo_conflicts)
                } else {
                    String::new()
                }
            ));
        }
        if self.meter_shadowed > 0 {
            out.push(format!(
                "{} time signature(s) shared a tick with a later one{}; the Conductor keeps the last in track order.",
                self.meter_shadowed,
                if self.meter_conflicts > 0 {
                    format!(" ({} different)", self.meter_conflicts)
                } else {
                    String::new()
                }
            ));
        }
        if self.master_shadowed > 0 {
            out.push(format!(
                "{} Master Volume message(s) shared a tick with a later one; the Conductor keeps the last in track order.",
                self.master_shadowed
            ));
        }
        if self.from_other_tracks > 0 {
            out.push(format!(
                "{} tempo, time-signature, marker or master-volume event(s) came from tracks other than the first; they now belong to the Conductor and are exported in the first track.",
                self.from_other_tracks
            ));
        }
        out
    }
}

#[derive(Clone, Debug)]
enum Found {
    Tempo(u32),
    Signature([u8; 4]),
    Marker {
        cue: bool,
        bytes: Vec<u8>,
    },
    Chapter {
        id: u32,
        length: u64,
        title: Vec<u8>,
    },
    MasterVolume {
        device: u8,
        value: u16,
    },
}

#[derive(Clone, Debug)]
struct Candidate {
    tick: u64,
    track: usize,
    event: usize,
    origin: Origin,
    found: Found,
    consumed: bool,
}

fn classify(e: &RawEvent, events: &super::events::EventStore, timeline: bool) -> Option<Found> {
    let (meta_type, payload) = match e.kind {
        RawKind::Meta { meta_type, payload } => (meta_type, payload),
        RawKind::SysEx { payload } => {
            return parse_master_volume_sysex(events.payload(payload))
                .map(|(device, value)| Found::MasterVolume { device, value });
        }
        _ => return None,
    };
    let p = events.payload(payload);
    match meta_type {
        meta::TEMPO if timeline && p.len() == 3 => {
            let us = u32::from_be_bytes([0, p[0], p[1], p[2]]);
            (us > 0).then_some(Found::Tempo(us))
        }
        meta::TIME_SIGNATURE if timeline && p.len() == 4 && p[0] > 0 && p[1] <= MAX_DENOM_POW => {
            Some(Found::Signature([p[0], p[1], p[2], p[3]]))
        }
        META_MARKER | META_CUE => Some(Found::Marker {
            cue: meta_type == META_CUE,
            bytes: p.to_vec(),
        }),
        seqspec::META_SEQUENCER_SPECIFIC => match seqspec::parse(p) {
            Some((seqspec::TYPE_CHAPTER, body)) => {
                let (id, length, title) = seqspec::parse_chapter(body)?;
                (length > 0 && e.tick.checked_add(length).is_some()).then(|| Found::Chapter {
                    id,
                    length,
                    title: title.to_vec(),
                })
            }
            _ => None,
        },
        _ => None,
    }
}

impl Conductor {
    /// Builds the Conductor of an imported file by moving its global events out
    /// of the tracks: tempo and time signatures (metrical timing only), Marker and
    /// Cue Point events, and PlumSpirit chapter records (with the start and end
    /// Markers written for them). Other events are untouched.
    ///
    /// Several tempo events (or signatures) at one tick: the last in (track,
    /// file) order is the one a player applies, so it is kept; the others never
    /// took effect and are dropped, and the report says how many (and how many
    /// differed). Chapters are only reconstructed from PlumSpirit's own records —
    /// a Marker that merely looks like an end marker stays a point marker.
    pub fn adopt(scope: ConductorScope, tracks: &mut [Track]) -> (Conductor, AdoptReport) {
        let mut report = AdoptReport::default();
        if scope == ConductorScope::Sequences {
            return (Conductor::with_defaults(scope), report);
        }
        let timeline = scope == ConductorScope::Timeline;
        let mut cands: Vec<Candidate> = Vec::new();
        for (ti, t) in tracks.iter().enumerate() {
            let mut cur_tick = u64::MAX;
            let mut kept_at_tick = 0u32;
            let mut adopted_at_tick = 0u32;
            for (ei, e) in t.events.events().iter().enumerate() {
                if e.tick != cur_tick {
                    cur_tick = e.tick;
                    kept_at_tick = 0;
                    adopted_at_tick = 0;
                }
                match classify(e, &t.events, timeline) {
                    Some(found) => {
                        cands.push(Candidate {
                            tick: e.tick,
                            track: ti,
                            event: ei,
                            origin: Origin {
                                track: t.id(),
                                anchor: e.anchor,
                                rank: kept_at_tick,
                                seq: adopted_at_tick,
                            },
                            found,
                            consumed: false,
                        });
                        adopted_at_tick = adopted_at_tick.saturating_add(1);
                    }
                    None => kept_at_tick = kept_at_tick.saturating_add(1),
                }
            }
        }
        if cands.is_empty() {
            return (Conductor::with_defaults(scope), report);
        }
        // Deterministic order: tick, then track, then position in the track.
        cands.sort_by_key(|c| (c.tick, c.track, c.event));
        report.from_other_tracks = cands.iter().filter(|c| c.track != 0).count();

        // Tempo and meter: the last event at each tick wins.
        let mut tempo_found: Vec<(u64, u32, Origin)> = Vec::new();
        let mut meter_found: Vec<(u64, [u8; 4], Origin)> = Vec::new();
        let mut master_found: Vec<(u64, u8, u16, Origin)> = Vec::new();
        for c in &cands {
            match c.found {
                Found::MasterVolume { device, value } => {
                    if let Some(last) = master_found.last_mut()
                        && last.0 == c.tick
                    {
                        report.master_shadowed += 1;
                        *last = (c.tick, device, value, c.origin);
                    } else {
                        master_found.push((c.tick, device, value, c.origin));
                    }
                }
                Found::Tempo(us) => {
                    if let Some(last) = tempo_found.last_mut()
                        && last.0 == c.tick
                    {
                        report.tempo_shadowed += 1;
                        if last.1 != us {
                            report.tempo_conflicts += 1;
                        }
                        *last = (c.tick, us, c.origin);
                    } else {
                        tempo_found.push((c.tick, us, c.origin));
                    }
                }
                Found::Signature(p) => {
                    if let Some(last) = meter_found.last_mut()
                        && last.0 == c.tick
                    {
                        report.meter_shadowed += 1;
                        if last.1[..2] != p[..2] {
                            report.meter_conflicts += 1;
                        }
                        *last = (c.tick, p, c.origin);
                    } else {
                        meter_found.push((c.tick, p, c.origin));
                    }
                }
                _ => {}
            }
        }

        // Chapters: a record plus (when still present) its start and end markers.
        // Plain Marker candidates are indexed by (track, tick, hash of the title)
        // so that each chapter finds its markers in constant time (files with
        // many chapters would otherwise take quadratic time); within one key the
        // earliest candidate with exactly that title is taken, as a linear search
        // in file order would.
        fn title_hash(bytes: &[u8]) -> u64 {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            h.finish()
        }
        let mut marker_index: HashMap<(usize, u64, u64), VecDeque<usize>> = HashMap::new();
        for (i, c) in cands.iter().enumerate() {
            if let Found::Marker { cue: false, bytes } = &c.found {
                marker_index
                    .entry((c.track, c.tick, title_hash(bytes)))
                    .or_default()
                    .push_back(i);
            }
        }
        let mut take = |track: usize, tick: u64, text: &[u8]| -> Option<usize> {
            let q = marker_index.get_mut(&(track, tick, title_hash(text)))?;
            let pos = q.iter().position(
                |&j| matches!(&cands[j].found, Found::Marker { bytes, .. } if bytes == text),
            )?;
            q.remove(pos)
        };
        let mut consumed: Vec<usize> = Vec::new();
        let mut chapters: Vec<(u64, u64, u32, Vec<u8>, Origin)> = Vec::new();
        for (i, c) in cands.iter().enumerate() {
            let Found::Chapter { id, length, title } = &c.found else {
                continue;
            };
            let (tick, track) = (c.tick, c.track);
            let end = tick + length;
            let end_text = seqspec::end_label(title);
            let mut origin = c.origin;
            if let Some(j) = take(track, tick, title) {
                consumed.push(j);
                origin = cands[j].origin;
            }
            if let Some(j) = take(track, end, &end_text) {
                consumed.push(j);
            }
            consumed.push(i);
            chapters.push((tick, end, *id, title.clone(), origin));
        }
        for j in consumed {
            cands[j].consumed = true;
        }

        // Ids: chapter ids from PlumSpirit records are kept when valid and unique;
        // everything else is numbered after them.
        let mut used: HashSet<u32> = HashSet::new();
        for ch in &mut chapters {
            if ch.2 == 0 || !used.insert(ch.2) {
                ch.2 = 0;
            }
        }
        let mut next_id = used
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        let mut alloc = || {
            let id = next_id;
            next_id = next_id.saturating_add(1);
            id
        };

        let mut tempo: Vec<TempoPoint> = Vec::with_capacity(tempo_found.len() + 1);
        if tempo_found.first().is_none_or(|t| t.0 != 0) {
            tempo.push(TempoPoint {
                id: alloc(),
                tick: 0,
                value: DEFAULT_TEMPO,
                interp: Interp::Step,
                implicit: true,
                origin: None,
            });
        }
        for (tick, us, origin) in &tempo_found {
            tempo.push(TempoPoint {
                id: alloc(),
                tick: *tick,
                value: TempoValue::Micros(*us),
                interp: Interp::Step,
                implicit: false,
                origin: Some(*origin),
            });
        }
        let mut meter: Vec<TimeSignature> = Vec::with_capacity(meter_found.len() + 1);
        if meter_found.first().is_none_or(|m| m.0 != 0) {
            meter.push(TimeSignature {
                id: alloc(),
                tick: 0,
                numerator: 4,
                denom_pow: 2,
                clocks: 24,
                thirty_seconds: 8,
                implicit: true,
                origin: None,
            });
        }
        for (tick, p, origin) in &meter_found {
            meter.push(TimeSignature {
                id: alloc(),
                tick: *tick,
                numerator: p[0],
                denom_pow: p[1],
                clocks: p[2],
                thirty_seconds: p[3],
                implicit: false,
                origin: Some(*origin),
            });
        }
        let mut markers: Vec<Marker> = Vec::new();
        for (start, end, id, title, origin) in chapters {
            let (text, raw) = imported_title(&title);
            markers.push(Marker {
                id: if id == 0 { alloc() } else { id },
                start,
                end: Some(end),
                title: text,
                raw_title: raw,
                cue: false,
                origin: Some(origin),
            });
            report.chapters += 1;
        }
        for c in cands.iter().filter(|c| !c.consumed) {
            if let Found::Marker { cue, bytes } = &c.found {
                let (text, raw) = imported_title(bytes);
                markers.push(Marker {
                    id: alloc(),
                    start: c.tick,
                    end: None,
                    title: text,
                    raw_title: raw,
                    cue: *cue,
                    origin: Some(c.origin),
                });
                report.markers += 1;
            }
        }
        markers.sort_by_key(|m| (m.start, m.id));
        let master: Vec<MasterVolumePoint> = master_found
            .iter()
            .map(|&(tick, device, value, origin)| MasterVolumePoint {
                id: alloc(),
                tick,
                value,
                device,
                origin: Some(origin),
            })
            .collect();
        report.tempo = tempo_found.len();
        report.meter = meter_found.len();
        report.master = master.len();
        let conductor = match Conductor::from_parts_with_master(
            scope, tempo, meter, markers, master, next_id,
        ) {
            Ok(c) => c,
            // Cannot happen (built to satisfy every invariant); keep the tracks
            // unchanged rather than lose data if it ever did.
            Err(_) => return (Conductor::with_defaults(scope), AdoptReport::default()),
        };
        // Every candidate was adopted (or shadowed): remove them from the tracks.
        let mut per_track: Vec<Vec<usize>> = vec![Vec::new(); tracks.len()];
        for c in &cands {
            per_track[c.track].push(c.event);
        }
        for (t, mut idx) in tracks.iter_mut().zip(per_track) {
            if idx.is_empty() {
                continue;
            }
            idx.sort_unstable();
            t.events.remove_sorted_indices(&idx);
        }
        (conductor, report)
    }

    /// Sets the tempo at tick 0 (migration of the v0.2 project tempo; not an
    /// undoable edit).
    pub(crate) fn replace_start_tempo(&mut self, value: TempoValue) {
        self.tempo[0].value = value;
        self.tempo[0].implicit = false;
        self.revision = self.revision.wrapping_add(1);
        self.tempo_revision = self.tempo_revision.wrapping_add(1);
    }
}

/// Trims and checks a marker title typed by the user.
pub fn validate_title(input: &str) -> Result<String, ConductorError> {
    let t = input.trim();
    if t.is_empty() {
        return Err(ConductorError::EmptyTitle);
    }
    if t.chars().count() > MAX_MARKER_TITLE_CHARS {
        return Err(ConductorError::TitleTooLong);
    }
    if t.chars().any(char::is_control) {
        return Err(ConductorError::TitleControlChars);
    }
    Ok(t.to_owned())
}

/// Decodes an imported marker title: the text, plus the original bytes when they
/// are not valid UTF-8 (so they are written back unchanged).
pub fn imported_title(bytes: &[u8]) -> (String, Option<Box<[u8]>>) {
    let text = display_text(bytes);
    if text.as_bytes() == bytes {
        (text, None)
    } else {
        (text, Some(bytes.to_vec().into_boxed_slice()))
    }
}

/// Parses a time-signature denominator typed by the user (1, 2, 4 … 128) into its
/// power of two.
pub fn parse_denominator(input: &str) -> Result<u8, ConductorError> {
    let v: u32 = input
        .trim()
        .parse()
        .map_err(|_| ConductorError::InvalidDenominator)?;
    if v == 0 || !v.is_power_of_two() || v > 128 {
        return Err(ConductorError::InvalidDenominator);
    }
    Ok(v.trailing_zeros() as u8)
}

/// Parses a time-signature numerator typed by the user (1–255).
pub fn parse_numerator(input: &str) -> Result<u8, ConductorError> {
    match input.trim().parse::<u32>() {
        Ok(v) if (1..=255).contains(&v) => Ok(v as u8),
        _ => Err(ConductorError::InvalidNumerator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bpm(s: &str) -> TempoValue {
        TempoValue::Bpm(Bpm::parse(s).unwrap())
    }

    /// Prepares an edit on `c`, applies it and checks every invariant.
    macro_rules! apply {
        ($c:ident, $($prepare:tt)+) => {{
            let e = $c.$($prepare)+.unwrap();
            $c.apply(&e).unwrap();
            $c.validate().unwrap();
            e
        }};
    }

    #[test]
    fn a_new_project_has_an_explicit_start_tempo_and_four_four() {
        let c = Conductor::new_project(Bpm::parse("174.5").unwrap());
        c.validate().unwrap();
        assert_eq!(c.start_tempo(), bpm("174.5"));
        assert!(!c.tempo_points()[0].implicit);
        assert_eq!(c.time_signatures()[0].label(), "4/4");
        assert!(c.timing_editable());
    }

    #[test]
    fn adding_at_an_occupied_tick_edits_the_existing_point() {
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        apply!(c, prepare_set_tempo(None, 960, bpm("140"), Interp::Step));
        let id = c.tempo_points()[1].id;
        apply!(c, prepare_set_tempo(None, 960, bpm("150"), Interp::Linear));
        assert_eq!(c.tempo_points().len(), 2);
        assert_eq!(c.tempo_points()[1].id, id);
        assert_eq!(c.tempo_points()[1].value, bpm("150"));
        assert_eq!(c.tempo_points()[1].interp, Interp::Linear);
    }

    #[test]
    fn the_start_tempo_and_signature_cannot_be_removed_or_moved() {
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        let t0 = c.tempo_points()[0].id;
        let s0 = c.time_signatures()[0].id;
        assert!(matches!(
            c.prepare_remove_tempo(&[t0]),
            Err(ConductorError::StartRequired { .. })
        ));
        assert!(matches!(
            c.prepare_set_tempo(Some(t0), 5, bpm("90"), Interp::Step),
            Err(ConductorError::StartRequired { .. })
        ));
        assert!(matches!(
            c.prepare_remove_signatures(&[s0]),
            Err(ConductorError::StartRequired { .. })
        ));
        // Changing its value is fine.
        apply!(c, prepare_set_start_tempo(bpm("90")));
        assert_eq!(c.start_tempo(), bpm("90"));
    }

    #[test]
    fn moving_a_point_onto_another_replaces_it_and_undo_restores_both() {
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        apply!(c, prepare_set_tempo(None, 100, bpm("100"), Interp::Step));
        apply!(c, prepare_set_tempo(None, 200, bpm("200"), Interp::Step));
        let before = c.clone();
        let a = c.tempo_points()[1].id;
        let e = apply!(c, prepare_set_tempo(Some(a), 200, bpm("111"), Interp::Step));
        assert_eq!(c.tempo_points().len(), 2);
        assert_eq!(c.tempo_points()[1].id, a);
        c.revert(&e).unwrap();
        assert!(c.same_content(&before));
        c.reapply(&e).unwrap();
        assert_eq!(c.tempo_points().len(), 2);
    }

    #[test]
    fn stale_edits_are_refused() {
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        let e1 = c
            .prepare_set_tempo(None, 10, bpm("99"), Interp::Step)
            .unwrap();
        let e2 = c
            .prepare_set_tempo(None, 20, bpm("98"), Interp::Step)
            .unwrap();
        c.apply(&e1).unwrap();
        assert_eq!(c.apply(&e2), Err(ConductorError::Stale));
        assert_eq!(c.tempo_points().len(), 2);
    }

    #[test]
    fn an_edit_refused_by_validation_changes_nothing() {
        // Edits are applied in place; one whose result would be invalid (here a
        // marker reusing a tempo point's id, next to a valid tempo change) must
        // leave every list exactly as it was.
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        apply!(c, prepare_set_marker(None, 100, None, "A"));
        let before = c.clone();
        let mut e = c
            .prepare_set_tempo(None, 480, bpm("90"), Interp::Linear)
            .unwrap();
        let dup = Marker {
            id: c.tempo_points()[0].id,
            start: 50,
            end: None,
            title: "dup".into(),
            raw_title: None,
            cue: false,
            origin: None,
        };
        e.markers.insert(0, dup);
        assert_eq!(c.apply(&e), Err(ConductorError::Stale));
        assert!(c.same_content(&before));
        assert_eq!(c.revision(), before.revision());
        // The same holds going backwards: reverting an edit that is not the
        // last one applied is refused without side effects.
        let e1 = apply!(c, prepare_set_tempo(None, 960, bpm("80"), Interp::Step));
        let _e2 = apply!(c, prepare_remove_tempo(&[c.tempo_points()[1].id]));
        let after = c.clone();
        assert!(c.revert(&e1).is_err());
        assert!(c.same_content(&after));
    }

    #[test]
    fn signatures_validate_and_keep_imported_clicks() {
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        assert_eq!(
            c.prepare_set_signature(None, 0, 0, 2),
            Err(ConductorError::InvalidNumerator)
        );
        assert_eq!(
            c.prepare_set_signature(None, 0, 3, 8),
            Err(ConductorError::InvalidDenominator)
        );
        apply!(c, prepare_set_signature(None, 3840, 7, 3));
        let s = c.time_signatures()[1];
        assert_eq!((s.label(), s.clocks), ("7/8".to_owned(), 12));
        apply!(c, prepare_set_signature(None, 7200, 5, 4));
        assert_eq!(c.time_signatures()[2].clocks, 6);
        assert_eq!(c.signature_at(7199).label(), "7/8");
        assert_eq!(parse_denominator("16"), Ok(4));
        assert!(parse_denominator("12").is_err());
        assert!(parse_denominator("256").is_err());
        assert_eq!(parse_numerator("255"), Ok(255));
        assert!(parse_numerator("0").is_err());
    }

    #[test]
    fn markers_and_chapters() {
        let mut c = Conductor::new_project(Bpm::DEFAULT);
        apply!(c, prepare_set_marker(None, 960, Some(9600), "  Verse ✨ "));
        apply!(c, prepare_set_marker(None, 0, None, "Intro"));
        apply!(c, prepare_set_marker(None, 4800, Some(20_000), "Overlap"));
        let m = &c.markers()[1];
        assert_eq!(m.title, "Verse ✨");
        assert_eq!(m.duration(), 8640);
        assert_eq!(c.markers()[0].title, "Intro");
        assert_eq!(c.last_tick(), 20_000);
        assert_eq!(
            c.prepare_set_marker(None, 10, Some(5), "x"),
            Err(ConductorError::EndBeforeStart)
        );
        assert_eq!(
            c.prepare_set_marker(None, 10, None, "   "),
            Err(ConductorError::EmptyTitle)
        );
        // Zero-length chapter becomes a point marker.
        let e = c.prepare_set_marker(None, 50, Some(50), "Hit").unwrap();
        c.apply(&e).unwrap();
        assert!(
            c.markers()
                .iter()
                .any(|m| m.title == "Hit" && m.end.is_none())
        );
        let visible: Vec<&str> = c
            .markers_in_range(9000, 9100)
            .map(|m| m.title.as_str())
            .collect();
        assert_eq!(visible, vec!["Verse ✨", "Overlap"]);
    }

    #[test]
    fn linear_ramps_interpolate_the_displayed_bpm() {
        let mut c = Conductor::new_project(Bpm::parse("100").unwrap());
        let id = c.tempo_points()[0].id;
        apply!(c, prepare_set_interp(id, Interp::Linear));
        apply!(c, prepare_set_tempo(None, 1000, bpm("200"), Interp::Step));
        assert!((c.bpm_at(500) - 150.0).abs() < 1e-9);
        assert!((c.bpm_at(2000) - 200.0).abs() < 1e-9);
    }

    #[test]
    fn smpte_and_format_2_scopes_refuse_timing_edits() {
        let c = Conductor::with_defaults(ConductorScope::MarkersOnly);
        assert!(matches!(
            c.prepare_set_tempo(None, 5, bpm("99"), Interp::Step),
            Err(ConductorError::NotEditable(_))
        ));
        assert!(c.prepare_set_marker(None, 5, None, "ok").is_ok());
        let s = Conductor::with_defaults(ConductorScope::Sequences);
        assert!(s.prepare_set_marker(None, 5, None, "no").is_err());
    }
}
