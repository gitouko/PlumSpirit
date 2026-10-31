//! Export planning: decisions that must be made — and sometimes approved by the
//! user — before a document is written as a Standard MIDI File.
//!
//! A native PlumSpirit project can hold things a MIDI file cannot always represent:
//!
//! * **Exact decimal tempos** ([`Bpm`]). MIDI stores tempo as an integer number of
//!   microseconds per quarter note in 24 bits, so most decimal BPMs are rounded and
//!   BPMs outside roughly 3.58–60,000,000 cannot be stored at all.
//! * **Linear tempo ramps.** MIDI only has tempo *changes*, so a ramp is written as
//!   a series of steps (see *Tempo ramps* below).
//! * **A PPQN above 32,767**, the largest resolution a MIDI header can declare.
//! * **Chapter spans**, which MIDI has no event for (see `seqspec`).
//!
//! [`plan_export`] inspects the document and returns an [`ExportPlan`] describing
//! exactly what will be written: the header division word, an optional tick
//! rescale and every Conductor event (tempo, time signatures, markers) of the
//! first track. [`ExportPlan::needs_approval`] tells the UI whether the conversion
//! is materially lossy (or changes the timebase) and must be confirmed;
//! [`ExportPlan::disclosures`] gives the user-facing explanation, and
//! [`ExportPlan::blockers`] lists what makes an export impossible. The document is
//! never modified — the plan is applied while streaming the file, and the export
//! validator re-reads the file against the same plan.
//!
//! # Where Conductor events go
//!
//! Format 0 and 1 files carry their tempo map in the first track, so Conductor
//! events are written into the first track, merged with its own events. An event
//! that came from the first track on import returns to its original place among
//! that track's events at its tick; everything else is placed before the notes of
//! its tick, after the track's other events there. Format 2 files keep tempo and
//! meter inside their sequences, so nothing is added to them.
//!
//! # Tempo ramps
//!
//! A linear ramp is split into steps by recursive halving until, for every step,
//! the largest difference between the exact ramp time and the step's time is at
//! most the tolerance (0.5 ms by default). Each step's tempo is chosen so the time
//! at the end of the step matches the exact ramp (rounding to whole microseconds
//! is carried forward, so it does not accumulate); the ramp's end tempo is written
//! at its end point. The total number of generated steps is bounded; a project that
//! would exceed the budget is exported with a coarser tolerance only after the user
//! approves it (the disclosure states the resulting accuracy), and refused if even
//! a one-second tolerance does not fit.
//!
//! # Tick rescaling
//!
//! `new_tick = round_half_up(tick × to / from)` in 128-bit integer arithmetic. The
//! mapping is monotonic, so events keep their order (events are streamed in their
//! original order and only their ticks are mapped). The target resolution is chosen
//! so that as many positions as possible stay exact:
//!
//! 1. Let `G` be the GCD of the PPQN and every tick in the project. A target maps
//!    every position exactly iff it is a multiple of `PPQN / G`. If such a target
//!    fits, nothing moves; PlumSpirit prefers the largest divisor of the PPQN (at
//!    least 960), then 30,720, then the largest fitting multiple.
//! 2. Otherwise the largest divisor of the PPQN that fits (at least 960) is used,
//!    so every tick that is a multiple of `PPQN / target` stays exact; if no
//!    divisor gives at least 960 PPQN, 30,720 PPQN is used. Each moved position
//!    changes by at most half a tick of the new resolution; the number of moved
//!    notes is reported.

use crate::midi::group_digits;
use crate::midi::seqspec;
use crate::model::conductor::{ConductorScope, Interp, META_CUE, META_MARKER, Origin};
use crate::model::correction::{self, Correction, CorrectionReport};
use crate::model::document::PlumDocument;
use crate::model::events::meta;
use crate::model::events::{RawEvent, RawKind};
use crate::model::note::MAX_PITCH;
use crate::model::placed::{
    PlacedEvents, PlacedLane, PlacedNotes, placed_channel_sub_pairs, placed_event_count,
    placed_last_event_tick, placed_len, placed_max_end,
};
use crate::model::project::{Bpm, KeyRange, MidiTempo, TempoFidelity};
use crate::model::tempo::{TempoValue, ramp_span_seconds};
use crate::model::timing::{MAX_DELTA_TICKS, MAX_SMF_PPQN, TimingKind};
use crate::model::track::Track;
use crate::view::grid::MeterMap;

/// Fallback target resolution when no divisor of the native PPQN fits (divisible
/// by 2^11, 3 and 5, so common subdivisions and triplets stay representable).
pub const FALLBACK_EXPORT_PPQN: u32 = 30_720;
/// Smallest divisor-based target resolution considered acceptable.
const MIN_DIVISOR_TARGET: u32 = 960;
/// Default accuracy of tempo-ramp steps: largest timing difference in seconds.
pub const RAMP_TOLERANCE_S: f64 = 0.000_5;
/// Most tempo events all ramps together may generate.
pub const RAMP_EVENT_BUDGET: usize = 500_000;
/// Largest ramp tolerance tried before an export is refused.
const MAX_RAMP_TOLERANCE_S: f64 = 1.0;
/// Most gap-filler events a single track may need (≈ 20 bytes each).
const MAX_FILLERS_PER_TRACK: u64 = 50_000_000;

/// Tick conversion from the native PPQN to a MIDI-storable one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescale {
    pub from_ppqn: u32,
    pub to_ppqn: u32,
    /// Every tick maps exactly (nothing moves).
    pub exact: bool,
    /// Notes whose start or end is not exactly representable at `to_ppqn`.
    pub notes_moved: u64,
    /// Non-note events whose tick is not exactly representable.
    pub events_moved: u64,
    /// Notes longer than zero ticks that become zero ticks long.
    pub notes_collapsed: u64,
}

/// A Conductor event written into the first track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanEvent {
    /// Document tick (the rescale, if any, is applied while streaming).
    pub tick: u64,
    /// Note events before it at this tick.
    pub anchor: u32,
    /// Place before the `rank`-th of the track's own events at this tick
    /// (`u32::MAX`: after those with the same anchor).
    pub rank: u32,
    pub kind: PlanKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanKind {
    /// `FF 51 03` with this many microseconds per quarter note.
    Tempo(u32),
    /// A meta event whose payload is `arena[offset..offset + len]`.
    Meta {
        meta_type: u8,
        offset: u32,
        len: u32,
    },
    /// A SysEx event (`F0 len payload`) whose payload is `arena[offset..offset +
    /// len]` (Universal Master Volume).
    SysEx { offset: u32, len: u32 },
    /// v1.0-Launch L-C04: Program Change 0 — the General MIDI default
    /// instrument — on this channel (0–15), the neutral control event
    /// [`ExportOptions::player_safe_start`] writes ([`DEFAULT_PROGRAM`]).
    DefaultProgram(u8),
    /// v1.0-Launch L-S51: Channel Volume (CC 7) 0 on this channel — mutes the
    /// channel of the [`LoudnessReference`] ([`VOLUME_ZERO`]).
    VolumeZero(u8),
    /// v1.0-Launch L-S51: the [`LoudnessReference`] note's Note On
    /// (velocity 127) on `channel`, `key`.
    ReferenceOn { channel: u8, key: u8 },
    /// v1.0-Launch L-S51: its Note Off (`0x80`, release velocity 0) at the
    /// same tick (a zero-length note).
    ReferenceOff { channel: u8, key: u8 },
}

/// v1.0-Launch L-S51: Channel Volume 0 (`Bn 07 00`) on each channel, as the
/// events [`PlanKind::VolumeZero`] writes.
pub static VOLUME_ZERO: [RawKind; 16] = {
    let mut a = [RawKind::Channel {
        status: 0xB0,
        data: [7, 0],
    }; 16];
    let mut ch = 0;
    while ch < 16 {
        a[ch] = RawKind::Channel {
            status: 0xB0 | ch as u8,
            data: [7, 0],
        };
        ch += 1;
    }
    a
};

/// v1.0-Launch L-S51 ([`ExportOptions::loudness_reference`]): Piano From
/// Above multiplies every Note On velocity by up to 127 ÷ the file's loudest
/// velocity, so a song whose notes are all quiet plays loud there. One
/// silent note at velocity 127 makes the loudest velocity 127 and the factor
/// 1: a zero-length note on an otherwise unused channel whose Channel Volume
/// is set to 0 at the start, on the song's lowest key (so a "keys of the song"
/// view keeps its range), at the first track's end (long after the volume
/// change, so no player can sound it before the channel is muted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoudnessReference {
    /// MIDI channel 0–15 (never one the song uses; never 10, the drum
    /// channel).
    pub channel: u8,
    pub key: u8,
    /// Document tick of the zero-length note (the first track's end).
    pub tick: u64,
    /// The loudest Note On velocity of the song itself (1–126).
    pub loudest: u8,
}

/// v1.0-Launch L-C04: Program Change 0 on each channel, as the events
/// [`PlanKind::DefaultProgram`] writes.
pub static DEFAULT_PROGRAM: [RawKind; 16] = {
    let mut a = [RawKind::Channel {
        status: 0xC0,
        data: [0, 0],
    }; 16];
    let mut ch = 0;
    while ch < 16 {
        a[ch] = RawKind::Channel {
            status: 0xC0 | ch as u8,
            data: [0, 0],
        };
        ch += 1;
    }
    a
};

/// A tempo that a MIDI file cannot store exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LossyTempo {
    pub tick: u64,
    pub bpm: Bpm,
    pub midi: MidiTempo,
}

/// How the tempo map is written.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TempoReport {
    /// Tempo events written (including ramp steps).
    pub events: usize,
    /// Exact BPMs that had to be rounded or clamped (in tick order).
    pub lossy: Vec<LossyTempo>,
    /// Linear ramps written as steps.
    pub ramps: usize,
    /// Tempo events generated for ramps.
    pub ramp_events: usize,
    /// Largest timing difference of any ramp step, in milliseconds.
    pub max_ramp_error_ms: f64,
    /// Accuracy the ramps were generated for, in milliseconds.
    pub tolerance_ms: f64,
    /// The default accuracy did not fit the event budget and was relaxed.
    pub coarsened: bool,
    /// Ramp steps outside the MIDI tempo range (stored at the nearest limit).
    pub clamped_steps: usize,
}

impl TempoReport {
    fn worst(&self) -> TempoFidelity {
        self.lossy
            .iter()
            .map(|l| l.midi.fidelity)
            .max_by_key(|f| match f {
                TempoFidelity::Exact => 0,
                TempoFidelity::Rounded => 1,
                TempoFidelity::RoundedMaterially => 2,
                TempoFidelity::Clamped => 3,
            })
            .unwrap_or(TempoFidelity::Exact)
    }
}

/// Everything export needs to know beyond the document itself.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ExportPlan {
    /// The 16-bit division word written to the header.
    pub division_word: u16,
    pub rescale: Option<Rescale>,
    /// Conductor events for the first track, in the order they are written.
    pub events: Vec<PlanEvent>,
    /// Payload bytes of meta events in `events`.
    pub arena: Vec<u8>,
    pub tempo: TempoReport,
    pub signatures: usize,
    pub markers: usize,
    pub chapters: usize,
    /// Master Volume points written as Universal Real Time SysEx.
    pub master_volume: usize,
    /// Per track (by index): the Track Properties correction applied on export,
    /// `None` for an unchanged track. Empty when corrections are not applied.
    pub corrections: Vec<Option<Correction>>,
    /// Per track: its events with the tick correction applied (only for tracks
    /// whose tick correction moves events).
    pub corrected_events: Vec<Option<Vec<RawEvent>>>,
    /// What the corrections do to each corrected track.
    pub correction_reports: Vec<CorrectionReport>,
    /// Tracks that have corrections but were exported without them (the user
    /// chose to export the original data).
    pub corrections_skipped: usize,
    /// Reasons the document cannot be exported at all (nothing is written).
    pub blockers: Vec<String>,
    /// The `MTrk` chunks written, in order (v0.5). Empty means "one chunk per
    /// track, every note" (plans built by hand, e.g. [`ExportPlan::passthrough`]).
    pub outputs: Vec<OutputTrack>,
    /// What happens to the track tree and the subchannels (informational).
    pub structure: StructureReport,
    /// v0.9: notes above key 127 and what happens to them.
    pub wide: WideKeyReport,
    /// v0.9 (V09-0050): every channel message is written with its status
    /// byte ([`ExportOptions::explicit_status`]).
    pub explicit_status: bool,
    /// v1.0-Launch L-C04: the channel (0–15) that gets a Program Change 0 at
    /// tick 0 because the file would otherwise hold no controller, program
    /// change, channel pressure or pitch bend
    /// ([`ExportOptions::player_safe_start`]); `None`: nothing added.
    pub player_safe_start: Option<u8>,
    /// v1.0-Launch L-S51: the silent loudness reference written
    /// ([`ExportOptions::loudness_reference`]); `None`: none.
    pub loudness_reference: Option<LoudnessReference>,
    /// v1.0-Launch L-S51: why the reference that was asked for is not written
    /// (every usable channel is in use).
    pub loudness_reference_skipped: Option<String>,
    /// v0.9 clips (`docs/EXPORT_PLAN.md` D2): per track, what a track that is
    /// not simply its own identity clip exports — its placements' notes, read
    /// in place, and its placed events. Empty while the clips are the
    /// identity; `None` for a track that exports its own notes and events.
    pub placed: Vec<Option<PlacedTrack>>,
}

/// What a track with clips exports (v0.9): the notes and channel events its
/// unmuted placements play and the track's own other events (name, text,
/// SysEx …, where they are), as `PlumDocument::materialize_placements` would
/// hold them — read in place, nothing copied (`model::placed`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedTrack {
    /// The unmuted placements, in placement order (shared notes and events).
    pub lanes: Vec<PlacedLane>,
    /// Notes the placements play, muted ones included.
    pub notes: usize,
    /// Events written before corrections ([`PlacedEvents`]).
    pub events: usize,
    /// Latest of those events' ticks.
    pub last_event: Option<u64>,
    /// Latest note end or event, or the track's explicit end (as
    /// [`Track::content_end_tick`]).
    pub end: u64,
}

impl PlacedTrack {
    fn new(track: &Track, placed: &[crate::model::PlacedContent<'_>]) -> Self {
        let lanes: Vec<PlacedLane> = placed
            .iter()
            .filter(|pc| !pc.placement.muted)
            .map(PlacedLane::of)
            .collect();
        let own = track.events.events();
        let last_event = placed_last_event_tick(own, &lanes);
        let end = placed_max_end(&lanes)
            .unwrap_or(0)
            .max(last_event.unwrap_or(0))
            .max(track.end_of_track_tick.unwrap_or(0));
        PlacedTrack {
            notes: placed_len(&lanes),
            events: placed_event_count(own, &lanes),
            last_event,
            lanes,
            end,
        }
    }
}

/// The events of one track as export writes them, in order: `(tick in
/// document ticks, event)` — the event's own tick is its content tick for
/// a placed event. Nothing is copied.
pub enum ExportEvents<'a> {
    /// A track's own events, or the plan's corrected copy of them.
    Slice(std::slice::Iter<'a, RawEvent>),
    /// A track with clips ([`PlacedEvents`]).
    Placed(PlacedEvents<'a>),
}

impl<'a> Iterator for ExportEvents<'a> {
    type Item = (u64, &'a RawEvent);

    #[inline]
    fn next(&mut self) -> Option<(u64, &'a RawEvent)> {
        match self {
            ExportEvents::Slice(it) => it.next().map(|e| (e.tick, e)),
            ExportEvents::Placed(p) => p.next(),
        }
    }
}

/// One `MTrk` chunk of the exported file. A track normally becomes one chunk;
/// when several of its subchannels share a MIDI channel and the user keeps
/// subchannels apart, the extra subchannels go into additional chunks
/// ("layers") right after it, holding only notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputTrack {
    /// Document track whose content the chunk carries.
    pub track: usize,
    /// 0: the track's own chunk (its events, plus the Conductor for track 0).
    pub layer: u32,
    /// (MIDI channel, subchannel) pairs whose notes the chunk carries, sorted;
    /// `None` = every note of the track. A layer holds at most one subchannel
    /// per MIDI channel.
    pub pairs: Option<Vec<(u8, u32)>>,
    /// `pairs` as a per-channel lookup (O(1) per note while writing).
    pub slots: Option<[Option<u32>; 16]>,
    /// PlumSpirit structure record written just before End of Track.
    pub record: Option<Vec<u8>>,
    /// Track Name written at tick 0 (extra layers only).
    pub name: Option<Vec<u8>>,
}

impl OutputTrack {
    /// Whether this chunk carries a note of subchannel `sub` on `channel`.
    #[inline]
    pub fn carries(&self, channel: u8, sub: u32) -> bool {
        match &self.slots {
            None => true,
            Some(s) => s[usize::from(channel & 0x0F)] == Some(sub),
        }
    }

    fn plain(track: usize) -> Self {
        OutputTrack {
            track,
            layer: 0,
            pairs: None,
            slots: None,
            record: None,
            name: None,
        }
    }

    /// Per-channel lookup of a layer's pairs (each channel at most once).
    fn slots_of(pairs: &[(u8, u32)]) -> [Option<u32>; 16] {
        let mut s = [None; 16];
        for &(ch, sub) in pairs {
            debug_assert!(
                s[usize::from(ch & 0x0F)].is_none(),
                "one subchannel per channel"
            );
            s[usize::from(ch & 0x0F)] = Some(sub);
        }
        s
    }
}

/// How the export represents the track tree and subchannels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructureReport {
    /// Subtracks written as ordinary (flat) MIDI tracks.
    pub subtracks: usize,
    /// Logical subchannels in the whole project.
    pub subchannels: usize,
    /// Tracks with more logical subchannels than MIDI channels in use.
    pub tracks_over_16: usize,
    /// Extra chunks added to keep subchannels that share a MIDI channel apart.
    pub extra_chunks: usize,
    /// Tracks whose subchannels sharing a MIDI channel were merged into one
    /// chunk (the user chose not to keep them apart).
    pub merged_tracks: usize,
    /// PlumSpirit structure records were written.
    pub metadata: bool,
}

/// What a Standard MIDI export does with notes above key 127 (v0.9
/// 256-Key Mode, §061): MIDI 1.0 keys stop at 127, so such a note is never
/// written as if it were a Standard MIDI note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WideKeys {
    /// Nothing is written while such notes exist (a blocker names them; the
    /// user chooses one of the other policies).
    #[default]
    Reject,
    /// They are left out of the file.
    Discard,
    /// Moved down by whole octaves into 0–127 (128 → 116, 200 → 116,
    /// 255 → 123): the pitch class stays.
    Fold,
    /// Set to key 127.
    Clamp,
    /// EXPERIMENTAL / NON-STANDARD (§062): kept, written the way Kiva reads
    /// 256-key files — the note byte unmasked (0x80–0xFF) right after an
    /// explicit Note On / Off status byte. The result is not a Standard MIDI
    /// file; PlumSpirit reads it back only with 256-key import.
    Experimental,
}

impl WideKeys {
    pub const ALL: [WideKeys; 5] = [
        WideKeys::Reject,
        WideKeys::Discard,
        WideKeys::Fold,
        WideKeys::Clamp,
        WideKeys::Experimental,
    ];

    /// The key written for `pitch` (`None`: the note is left out). Keys
    /// 0–127 are always written unchanged.
    pub fn map(self, pitch: u8) -> Option<u8> {
        if pitch <= MAX_PITCH {
            return Some(pitch);
        }
        match self {
            WideKeys::Reject | WideKeys::Discard => None,
            WideKeys::Fold => {
                let over = u32::from(pitch - MAX_PITCH);
                Some(pitch - (12 * over.div_ceil(12)) as u8)
            }
            WideKeys::Clamp => Some(MAX_PITCH),
            WideKeys::Experimental => Some(pitch),
        }
    }

    /// Whether keys above 127 are written as they are (the non-standard
    /// experimental file).
    pub fn writes_wide(self) -> bool {
        self == WideKeys::Experimental
    }

    /// The choice as the export dialog names it.
    pub fn label(self) -> &'static str {
        match self {
            WideKeys::Reject => "Reject Standard Export",
            WideKeys::Discard => "Discard Out-of-range Notes",
            WideKeys::Fold => "Fold Down by Octaves",
            WideKeys::Clamp => "Clamp to Key 127",
            WideKeys::Experimental => "Experimental 256-Key MIDI (non-standard)",
        }
    }
}

/// The notes above key 127 an export meets — as stored in a 256-Key Mode
/// project, or after a track's key correction the export applies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WideKeyReport {
    pub notes: u64,
    /// The highest such key (0 when there is none).
    pub highest: u8,
    pub policy: WideKeys,
    /// Preserved Note On / Off events (unmatched ones, kept as raw events by
    /// a 256-key import) whose key byte is above 127. Only the experimental
    /// file writes them; every standard policy leaves them out.
    pub raw_events: u64,
}

impl WideKeyReport {
    /// Whether the export meets anything above key 127.
    pub fn any(&self) -> bool {
        self.notes > 0 || self.raw_events > 0
    }

    /// Whether the written file is the non-standard experimental one (keys
    /// above 127 exist and the policy writes them).
    pub fn non_standard(&self) -> bool {
        self.any() && self.policy.writes_wide()
    }
}

/// Choices the user makes for an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    /// Apply each track's corrections (tick / key / gate / key range), so the
    /// file sounds like PlumSpirit's playback. Off: export the original notes.
    pub apply_corrections: bool,
    /// Write PlumSpirit structure records (track tree and subchannels) that other
    /// programs ignore and PlumSpirit uses to restore them.
    pub structure_metadata: bool,
    /// Keep subchannels that share a MIDI channel in separate chunks (layers),
    /// so other programs can tell them apart too. Off: they share one chunk.
    pub split_subchannels: bool,
    /// v0.9: notes above key 127 (see [`WideKeys`]).
    pub wide_keys: WideKeys,
    /// v0.9 §069 / §246 (V09-0050): "Maximum Compatibility" — every channel
    /// message gets its status byte. Off (default): running status, the
    /// standard compact form ("compressed"). Both are valid MIDI and hold the
    /// same events.
    pub explicit_status: bool,
    /// v1.0-Launch L-C04 (§005): when the file would hold no controller,
    /// program change, channel pressure or pitch bend at all, write one
    /// Program Change 0 (the General MIDI default instrument, so nothing
    /// sounds different) at tick 0 on the lowest channel the notes use.
    /// Kiva v1.1.18-1m's Pre-render audio engine stops responding on a file
    /// that gives it no audio and no control event (every note below its
    /// Audio Note Threshold and no control message —
    /// `docs/references/v1_0_launch/KIVA_NOTES.md`). No note, velocity or
    /// timing changes. Off here (the engine's plain export); the app turns it
    /// on from its settings (default on).
    pub player_safe_start: bool,
    /// v1.0-Launch L-S51 (owner request 2026-10-30): when the song's loudest
    /// note is below velocity 127, add one silent loudness reference
    /// ([`LoudnessReference`]) so Piano From Above's volume correction leaves
    /// the velocities as they are. Adds one note (zero length, muted channel)
    /// that every player counts; off by default, in the engine and the app.
    pub loudness_reference: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions {
            apply_corrections: true,
            structure_metadata: true,
            split_subchannels: true,
            wide_keys: WideKeys::Reject,
            explicit_status: false,
            player_safe_start: false,
            loudness_reference: false,
        }
    }
}

impl ExportOptions {
    /// The options of the file the MIDI Player shows (v0.9 V09-S17,
    /// `docs/PLAYER.md` P1): the default export, except that a 256-Key Mode
    /// project keeps every key ([`WideKeys::Experimental`]), so the Player
    /// shows all 256 keys and drops none.
    pub fn for_player(keys: KeyRange) -> ExportOptions {
        let wide_keys = match keys {
            KeyRange::Extended256 => WideKeys::Experimental,
            KeyRange::Standard => ExportOptions::default().wide_keys,
        };
        ExportOptions {
            wide_keys,
            ..ExportOptions::default()
        }
    }
}

impl ExportPlan {
    /// A plan that writes the tracks as they are (no Conductor events).
    pub fn passthrough(division_word: u16) -> Self {
        ExportPlan {
            division_word,
            ..ExportPlan::default()
        }
    }

    /// Maps a document tick to the tick written to the file.
    #[inline]
    pub fn map_tick(&self, tick: u64) -> u64 {
        match &self.rescale {
            None => tick,
            Some(r) => rescale_tick(tick, r.from_ppqn, r.to_ppqn),
        }
    }

    /// Payload of a planned meta event.
    pub fn payload(&self, offset: u32, len: u32) -> &[u8] {
        &self.arena[offset as usize..offset as usize + len as usize]
    }

    /// The chunks to write (one per track when the plan has no layout).
    pub fn layout(&self, doc: &PlumDocument) -> std::borrow::Cow<'_, [OutputTrack]> {
        if self.outputs.is_empty() {
            std::borrow::Cow::Owned((0..doc.track_count()).map(OutputTrack::plain).collect())
        } else {
            std::borrow::Cow::Borrowed(&self.outputs)
        }
    }

    /// Number of `MTrk` chunks written.
    pub fn chunk_count(&self, doc: &PlumDocument) -> usize {
        if self.outputs.is_empty() {
            doc.track_count()
        } else {
            self.outputs.len()
        }
    }

    /// The file format written: Format 0 becomes Format 1 when there are
    /// several chunks.
    pub fn format(&self, doc: &PlumDocument) -> crate::model::document::SmfFormat {
        use crate::model::document::SmfFormat;
        let f = doc.export_format();
        if f == SmfFormat::SingleTrack && self.chunk_count(doc) > 1 {
            SmfFormat::Parallel
        } else {
            f
        }
    }

    /// Plain-language notes about how subtracks and subchannels are written
    /// (shown before exporting; not a reason to refuse or to ask for approval).
    pub fn structure_summary(&self) -> Vec<String> {
        let r = &self.structure;
        let mut out = Vec::new();
        if r.subtracks > 0 {
            out.push(format!(
                "MIDI files have no subtracks: the {} subtrack(s) are written as ordinary MIDI tracks right after their parent track (in the order of the track list), with every note and event.",
                group_digits(r.subtracks as u64)
            ));
        }
        if r.extra_chunks > 0 {
            out.push(format!(
                "Standard MIDI has 16 MIDI channels, and some tracks have several subchannels on the same MIDI channel. To keep them apart, {} extra MIDI track(s) hold those subchannels' notes. Every note keeps its MIDI channel (1–16); nothing is lost.",
                group_digits(r.extra_chunks as u64)
            ));
        } else if r.merged_tracks > 0 {
            out.push(format!(
                "{} track(s) have several subchannels on the same MIDI channel; they are written together in their track, so other programs (and a PlumSpirit that re-opens the file) cannot tell those subchannels apart. Every note is kept.",
                group_digits(r.merged_tracks as u64)
            ));
        }
        if r.metadata {
            out.push(
                "PlumSpirit adds its own data (ignored by other programs) so that opening the file in PlumSpirit restores the subtracks and subchannels. Subchannel colors are PlumSpirit's own: other MIDI players choose their own colors."
                    .into(),
            );
        }
        out
    }

    /// Latest document tick of any planned event (0 when none).
    pub fn last_tick(&self) -> u64 {
        self.events.last().map_or(0, |e| e.tick)
    }

    /// The correction applied to track `index`, if any.
    #[inline]
    pub fn correction(&self, index: usize) -> Option<&Correction> {
        self.corrections.get(index).and_then(Option::as_ref)
    }

    /// Track `index`'s events as written, in order (its placed events with
    /// clips; corrected when the tick correction moves them; without the
    /// keys above 127 a standard file leaves out).
    pub fn events_of<'a>(&'a self, track: &'a Track, index: usize) -> ExportEvents<'a> {
        match self.corrected_events.get(index).and_then(Option::as_ref) {
            Some(v) => ExportEvents::Slice(v.iter()),
            None => self.uncorrected_events(track, index),
        }
    }

    /// Track `index`'s events before corrections: its placed events with
    /// clips, otherwise its own.
    fn uncorrected_events<'a>(&'a self, track: &'a Track, index: usize) -> ExportEvents<'a> {
        match self.placed(index) {
            Some(p) => ExportEvents::Placed(PlacedEvents::new(track.events.events(), &p.lanes)),
            None => ExportEvents::Slice(track.events.events().iter()),
        }
    }

    /// Latest tick of track `index`'s events before corrections.
    fn uncorrected_last_event(&self, track: &Track, index: usize) -> Option<u64> {
        match self.placed(index) {
            Some(p) => p.last_event,
            None => track.events.last_tick(),
        }
    }

    /// Number of track `index`'s events written before corrections (preserved
    /// events of the write statistics).
    pub fn event_count(&self, track: &Track, index: usize) -> usize {
        self.placed(index).map_or(track.events.len(), |p| p.events)
    }

    /// What track `index` exports when it is not simply its own identity
    /// clip (v0.9).
    #[inline]
    pub fn placed(&self, index: usize) -> Option<&PlacedTrack> {
        self.placed.get(index).and_then(Option::as_ref)
    }

    /// Notes stored in what track `index` exports, muted ones included.
    pub fn stored_notes(&self, track: &Track, index: usize) -> usize {
        self.placed(index).map_or(track.notes.len(), |p| p.notes)
    }

    /// Latest note end, event or explicit end of what track `index`
    /// exports, before corrections ([`Track::content_end_tick`]).
    pub fn content_end(&self, track: &Track, index: usize) -> u64 {
        self.placed(index)
            .map_or_else(|| track.content_end_tick(), |p| p.end)
    }

    /// Track `index`'s notes before corrections, muted ones included (the
    /// plan's own passes over the notes).
    fn notes_as_stored<'a>(
        &'a self,
        track: &'a Track,
        index: usize,
    ) -> Box<dyn Iterator<Item = crate::model::NoteRecord> + 'a> {
        match self.placed(index) {
            Some(p) => Box::new(PlacedNotes::new(&p.lanes)),
            None => Box::new(track.notes.iter().map(|n| n.record())),
        }
    }

    /// Latest corrected content tick of track `index` (notes and events), when
    /// it is corrected.
    pub fn corrected_content_end(&self, index: usize) -> Option<u64> {
        self.correction(index)?;
        self.correction_reports
            .iter()
            .find(|r| r.index == index)
            .map(|r| r.content_end)
    }

    /// True when the user must explicitly approve this export: the timebase
    /// changes, a tempo cannot be stored within 0.1 %, or tempo ramps had to be
    /// written less accurately than the default, or (v0.9) keys above 127 are
    /// left out, moved or written non-standard.
    pub fn needs_approval(&self) -> bool {
        self.rescale.is_some()
            || self.tempo.worst().is_material()
            || self.tempo.coarsened
            || self.tempo.clamped_steps > 0
            || self
                .correction_reports
                .iter()
                .any(|r| r.clamped > 0 || r.outside_range > 0)
            || (self.wide.any() && self.wide.policy != WideKeys::Reject)
    }

    /// What the export does with keys above 127 (v0.9 §061 / §062), never
    /// silent: `None` when the export meets no such key, or when the policy
    /// is [`WideKeys::Reject`] (the blocker explains it).
    pub fn wide_disclosure(&self) -> Option<String> {
        let w = &self.wide;
        if !w.any() {
            return None;
        }
        let h = w.highest;
        let notes = group_digits(w.notes);
        let left_out = if w.raw_events > 0 {
            format!(
                " {} unmatched Note On / Off event(s) above key 127 are left out.",
                group_digits(w.raw_events)
            )
        } else {
            String::new()
        };
        let standard = "a Standard MIDI file holds keys 0–127 only";
        Some(match w.policy {
            WideKeys::Reject => return None,
            WideKeys::Discard => format!(
                "{notes} note(s) above key 127 (the highest is {h}) are left out of the file: {standard}.{left_out} Your project keeps them."
            ),
            WideKeys::Fold => format!(
                "{notes} note(s) above key 127 (the highest is {h}) are moved down by whole octaves into keys 116–127 (each keeps its pitch class): {standard}.{left_out} Your project keeps the original keys."
            ),
            WideKeys::Clamp => format!(
                "{notes} note(s) above key 127 (the highest is {h}) are written at key 127: {standard}.{left_out} Your project keeps the original keys."
            ),
            WideKeys::Experimental => {
                let unmatched = if w.raw_events > 0 {
                    format!(
                        " and {} unmatched Note On / Off event(s)",
                        group_digits(w.raw_events)
                    )
                } else {
                    String::new()
                };
                format!(
                    "EXPERIMENTAL / NON-STANDARD FILE: {notes} note(s){unmatched} above key 127 (the highest is {h}) are written with 8-bit key numbers, the way Kiva reads 256-key MIDI files. This is not a Standard MIDI file: other programs may refuse it or misread those notes, and PlumSpirit opens it only as a 256-Key Mode project."
                )
            }
        })
    }

    /// Plain-language description of every conversion (empty when the file
    /// represents the project exactly).
    pub fn disclosures(&self) -> Vec<String> {
        let mut out = self.blockers.clone();
        out.extend(self.wide_disclosure());
        if let Some(r) = &self.rescale {
            let mut s = format!(
                "This project uses {} PPQN, but a MIDI file can store at most {} PPQN. It will be written at {} PPQN.",
                group_digits(u64::from(r.from_ppqn)),
                group_digits(u64::from(MAX_SMF_PPQN)),
                group_digits(u64::from(r.to_ppqn)),
            );
            if r.exact {
                s.push_str(" Every note and event keeps its exact position.");
            } else {
                s.push_str(&format!(
                    " {} note(s) and {} other event(s) move by at most half a tick at the new resolution.",
                    group_digits(r.notes_moved),
                    group_digits(r.events_moved)
                ));
                if r.notes_collapsed > 0 {
                    s.push_str(&format!(
                        " {} very short note(s) become zero ticks long.",
                        group_digits(r.notes_collapsed)
                    ));
                }
            }
            out.push(s);
        }
        let t = &self.tempo;
        if let Some(first) = t.lossy.first() {
            let written = Bpm::from_micros_per_quarter(first.midi.micros)
                .map_or_else(|| "?".to_owned(), |b| b.to_string());
            let others = if t.lossy.len() > 1 {
                format!(
                    " ({} tempo value(s) in total are converted like this.)",
                    t.lossy.len()
                )
            } else {
                String::new()
            };
            let s = match t.worst() {
                TempoFidelity::Clamped => format!(
                    "Some tempos are outside what a MIDI tempo event can store (about 3.58 to 60,000,000 BPM); the nearest storable tempo is written.{others}"
                ),
                TempoFidelity::RoundedMaterially => format!(
                    "MIDI stores tempo in whole microseconds per beat. {} BPM becomes {} µs, which plays at {} BPM — for some tempos that is a difference of 0.1% or more.{others}",
                    first.bpm,
                    group_digits(u64::from(first.midi.micros)),
                    written
                ),
                _ => format!(
                    "MIDI stores tempo in whole microseconds per beat, so {} BPM is written as {} µs ({} BPM).{others}",
                    first.bpm,
                    group_digits(u64::from(first.midi.micros)),
                    written
                ),
            };
            out.push(s);
        }
        if t.ramps > 0 {
            let mut s = format!(
                "MIDI files have no tempo ramps: {} linear tempo ramp(s) are written as {} tempo changes. The playback timing differs from the exact ramp by at most {:.3} ms.",
                group_digits(t.ramps as u64),
                group_digits(t.ramp_events as u64),
                t.max_ramp_error_ms
            );
            if t.coarsened {
                s.push_str(&format!(
                    " To stay within {} tempo events, the ramps use a coarser accuracy ({:.1} ms) than the default ({:.1} ms).",
                    group_digits(RAMP_EVENT_BUDGET as u64),
                    t.tolerance_ms,
                    RAMP_TOLERANCE_S * 1000.0
                ));
            }
            if t.clamped_steps > 0 {
                s.push_str(&format!(
                    " {} ramp step(s) are outside the MIDI tempo range and use the nearest storable tempo.",
                    group_digits(t.clamped_steps as u64)
                ));
            }
            s.push_str(" Your project keeps the exact ramps.");
            out.push(s);
        }
        let corrected = self.corrections.iter().flatten().count();
        if corrected > 0 {
            out.push(format!(
                "{corrected} track(s) are exported with their Track Properties corrections applied (tick, key and gate corrections, key ranges), so the file plays like PlumSpirit. Your project keeps the original notes and the correction settings."
            ));
            for r in &self.correction_reports {
                if !r.blocks_export() {
                    out.extend(r.notes(&format!("Track {}", r.index + 1)));
                }
            }
        }
        if self.corrections_skipped > 0 {
            out.push(format!(
                "{} track(s) have Track Properties corrections; they are exported without them (the original notes), so the file does not sound like PlumSpirit's playback of those tracks.",
                self.corrections_skipped
            ));
        }
        if self.master_volume > 0 {
            out.push(format!(
                "The {} Master Volume change(s) are written as Universal Real Time SysEx Master Volume messages (F0 7F 7F 04 01 …). Many synthesizers follow them; some ignore them.",
                self.master_volume
            ));
        }
        if self.chapters > 0 {
            out.push(format!(
                "MIDI has no chapter spans: each of the {} chapter(s) is written as a Marker at its start and a Marker at its end, plus PlumSpirit data that restores the chapter exactly when the file is opened in PlumSpirit. Other programs show the two markers.",
                self.chapters
            ));
        }
        out
    }
}

/// `round_half_up(tick × to / from)`.
#[inline]
pub fn rescale_tick(tick: u64, from: u32, to: u32) -> u64 {
    let from = u128::from(from.max(1));
    let v = (u128::from(tick) * u128::from(to) * 2 + from) / (2 * from);
    // `to < from`, so the result never exceeds `tick`.
    v as u64
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Divisors of `n` in increasing order (trial division; `n <= 2^31`).
fn divisors(n: u64) -> Vec<u64> {
    let mut small = Vec::new();
    let mut large = Vec::new();
    let mut i = 1u64;
    while i * i <= n {
        if n.is_multiple_of(i) {
            small.push(i);
            if i != n / i {
                large.push(n / i);
            }
        }
        i += 1;
    }
    small.extend(large.into_iter().rev());
    small
}

/// Ticks of every exported Conductor entry (for the rescale analysis).
fn conductor_ticks(doc: &PlumDocument) -> Vec<u64> {
    let c = doc.conductor();
    let mut v: Vec<u64> = Vec::new();
    if c.timing_editable() {
        v.extend(
            c.tempo_points()
                .iter()
                .filter(|p| !p.implicit)
                .map(|p| p.tick),
        );
        v.extend(
            c.time_signatures()
                .iter()
                .filter(|s| !s.implicit)
                .map(|s| s.tick),
        );
    }
    if c.markers_editable() {
        for m in c.markers() {
            v.push(m.start);
            if let Some(e) = m.end {
                v.push(e);
            }
        }
    }
    v.extend(c.master_volume().iter().map(|p| p.tick));
    v
}

/// Chooses a target resolution for `ppqn > 32,767` (see module docs).
///
/// With `G = gcd(ppqn, every tick)`, a target `to` maps every tick exactly iff
/// `q = ppqn / G` divides `to`. Preference among exact targets: the largest
/// divisor of the PPQN (at least 960), then 30,720, then the largest multiple of
/// `q`. Without an exact target: the largest divisor of the PPQN (at least 960),
/// otherwise 30,720.
fn choose_target(doc: &PlumDocument, plan: &ExportPlan, ppqn: u32) -> u32 {
    let p = u64::from(ppqn);
    let max = u64::from(MAX_SMF_PPQN);
    // gcd of the PPQN and every position in the project.
    let mut g = p;
    for t in conductor_ticks(doc) {
        g = gcd(g, t);
    }
    'outer: for (i, t) in doc.tracks().iter().enumerate() {
        if g == 1 {
            break;
        }
        for n in super::export::export_notes(t, i, plan) {
            g = gcd(gcd(g, n.start_tick), n.duration_ticks);
            if g == 1 {
                break 'outer;
            }
        }
        for (tick, _) in plan.events_of(t, i) {
            g = gcd(g, tick);
        }
        g = gcd(g, super::export::planned_eot_unmapped(t, i, plan));
    }
    let q = p / g.max(1);
    let fitting: Vec<u64> = divisors(p)
        .into_iter()
        .filter(|&d| d <= max && d >= u64::from(MIN_DIVISOR_TARGET))
        .collect();
    if q <= max {
        if let Some(&d) = fitting.iter().rev().find(|&&d| d.is_multiple_of(q)) {
            return d as u32;
        }
        if u64::from(FALLBACK_EXPORT_PPQN).is_multiple_of(q) {
            return FALLBACK_EXPORT_PPQN;
        }
        return (max / q * q) as u32;
    }
    fitting.last().map_or(FALLBACK_EXPORT_PPQN, |&d| d as u32)
}

fn analyse_rescale(doc: &PlumDocument, plan: &ExportPlan, from: u32, to: u32) -> Rescale {
    let exact_at = |t: u64| (u128::from(t) * u128::from(to)).is_multiple_of(u128::from(from));
    let mut r = Rescale {
        from_ppqn: from,
        to_ppqn: to,
        exact: true,
        notes_moved: 0,
        events_moved: 0,
        notes_collapsed: 0,
    };
    for (i, t) in doc.tracks().iter().enumerate() {
        for n in super::export::export_notes(t, i, plan) {
            let end = n.end_tick();
            if !exact_at(n.start_tick) || !exact_at(end) {
                r.notes_moved += 1;
                if n.duration_ticks > 0
                    && rescale_tick(n.start_tick, from, to) == rescale_tick(end, from, to)
                {
                    r.notes_collapsed += 1;
                }
            }
        }
        for (tick, _) in plan.events_of(t, i) {
            if !exact_at(tick) {
                r.events_moved += 1;
            }
        }
        if !exact_at(super::export::planned_eot_unmapped(t, i, plan)) {
            r.events_moved += 1;
        }
    }
    r.events_moved += conductor_ticks(doc)
        .into_iter()
        .filter(|&t| !exact_at(t))
        .count() as u64;
    r.exact = r.notes_moved == 0 && r.events_moved == 0;
    r
}

/// One step of a ramp: offset from the ramp start, tempo, timing error (s).
struct RampStep {
    at: u64,
    micros: u32,
    error_s: f64,
    clamped: bool,
}

/// Splits a linear ramp from `b0` to `b1` BPM over `len` ticks into tempo steps
/// whose timing stays within `tol_s` seconds of the exact ramp (see module docs).
/// `None` when more than `*budget` steps would be needed.
fn ramp_steps(
    ppqn: u64,
    b0: f64,
    b1: f64,
    len: u64,
    tol_s: f64,
    budget: &mut usize,
) -> Option<Vec<RampStep>> {
    let slope = (b1 - b0) / len as f64;
    let seconds = |a: u64, b: u64| ramp_span_seconds(ppqn, b0, b1, len, a as f64, b as f64);
    // Largest deviation between the exact time curve and a constant tempo that
    // matches its mean over [x, y] (the chord): at the tick where the exact
    // tempo equals the mean tempo.
    let chord_error = |x: u64, y: u64| -> f64 {
        let span = seconds(x, y);
        let c = span / (y - x) as f64; // seconds per tick
        let bpm_star = 60.0 / (ppqn as f64 * c);
        let t_star = ((bpm_star - b0) / slope).clamp(x as f64, y as f64);
        let exact = ramp_span_seconds(ppqn, b0, b1, len, x as f64, t_star);
        (exact - c * (t_star - x as f64)).abs()
    };
    let mut accepted: Vec<(u64, u64, f64)> = Vec::new();
    let mut stack: Vec<(u64, u64)> = vec![(0, len)];
    while let Some((x, y)) = stack.pop() {
        let e = chord_error(x, y);
        if e <= tol_s || y - x <= 1 {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            accepted.push((x, y, e));
        } else {
            let m = x + (y - x) / 2;
            // Right half first so the left half is processed (and accepted) first.
            stack.push((m, y));
            stack.push((x, m));
        }
    }
    // Step tempos: match the exact time at the end of each step; the rounding to
    // whole microseconds is carried into the next step.
    let q = ppqn as f64;
    let mut exact_end = 0.0f64; // seconds from the ramp start, exact
    let mut written = 0.0f64; // seconds from the ramp start, as written
    let mut out = Vec::with_capacity(accepted.len());
    for (x, y, chord) in accepted {
        exact_end += seconds(x, y);
        let span = (y - x) as f64;
        // µs per quarter note so that `span` ticks take the remaining time.
        let ideal = (exact_end - written) * 1e6 * q / span;
        let rounded = ideal.round();
        let (micros, clamped) = if !rounded.is_finite() || rounded < 1.0 {
            (1u32, true)
        } else if rounded > f64::from(0xFF_FFFFu32) {
            (0xFF_FFFF, true)
        } else {
            (rounded as u32, false)
        };
        written += f64::from(micros) * span / q / 1e6;
        let drift = (exact_end - written).abs();
        out.push(RampStep {
            at: x,
            micros,
            error_s: chord + drift,
            clamped,
        });
    }
    Some(out)
}

fn micros_of(v: TempoValue) -> (u32, Option<MidiTempo>, Option<Bpm>) {
    match v {
        TempoValue::Micros(us) => (us, None, None),
        TempoValue::Bpm(b) => {
            let m = b.to_micros_per_quarter();
            (m.micros, Some(m), Some(b))
        }
    }
}

struct PlanBuilder<'d> {
    first_track: Option<crate::model::track::TrackId>,
    /// Planned events with their tie-break (original order, kind order).
    events: Vec<(PlanEvent, (u32, u32))>,
    arena: Vec<u8>,
    doc: &'d PlumDocument,
}

impl PlanBuilder<'_> {
    /// (anchor, rank, seq) of an event: where it came from in the first track,
    /// or after the track's own events (before the notes) at its tick.
    fn place(&self, origin: Option<Origin>) -> (u32, u32, u32) {
        match origin {
            Some(o) if Some(o.track) == self.first_track => (o.anchor, o.rank, o.seq),
            _ => (0, u32::MAX, u32::MAX),
        }
    }

    fn push(&mut self, tick: u64, origin: Option<Origin>, kind: PlanKind, order: u32) {
        let (anchor, rank, seq) = self.place(origin);
        self.events.push((
            PlanEvent {
                tick,
                anchor,
                rank,
                kind,
            },
            (seq, order),
        ));
    }

    fn meta(&mut self, meta_type: u8, bytes: &[u8]) -> Option<PlanKind> {
        let offset = u32::try_from(self.arena.len()).ok()?;
        let len = u32::try_from(bytes.len()).ok()?;
        offset.checked_add(len)?;
        self.arena.extend_from_slice(bytes);
        Some(PlanKind::Meta {
            meta_type,
            offset,
            len,
        })
    }

    fn sysex(&mut self, bytes: &[u8]) -> Option<PlanKind> {
        let offset = u32::try_from(self.arena.len()).ok()?;
        let len = u32::try_from(bytes.len()).ok()?;
        offset.checked_add(len)?;
        self.arena.extend_from_slice(bytes);
        Some(PlanKind::SysEx { offset, len })
    }
}

/// Works out how `doc` will be written as a MIDI file, with the default options
/// (corrections applied). See [`plan_export_with`].
pub fn plan_export(doc: &PlumDocument) -> ExportPlan {
    plan_export_with(doc, ExportOptions::default())
}

/// Works out how `doc` will be written as a MIDI file. O(notes) only when the PPQN
/// needs rescaling or tracks have corrections; otherwise O(tracks + events +
/// Conductor entries).
pub fn plan_export_with(doc: &PlumDocument, options: ExportOptions) -> ExportPlan {
    // v0.9 clips: the plan describes what is written — the placements as
    // they play, read in place (`plan_sources`).
    let mut plan = ExportPlan {
        explicit_status: options.explicit_status,
        ..ExportPlan::default()
    };
    plan_sources(doc, &mut plan);
    plan_corrections(doc, options, &mut plan);
    plan_wide_keys(doc, options, &mut plan);
    let division = doc.division();
    let (division_word, rescale) = match division.smf_word() {
        Some(w) => (w, None),
        None => {
            // Only metrical divisions can exceed the header range.
            let from = match division.kind() {
                TimingKind::Ppqn(p) => p,
                TimingKind::Smpte { .. } => unreachable!("SMPTE words always fit"),
            };
            let to = choose_target(doc, &plan, from);
            (to as u16, Some(analyse_rescale(doc, &plan, from, to)))
        }
    };
    plan.division_word = division_word;
    plan.rescale = rescale;
    build_conductor_events(doc, &mut plan);
    plan_loudness_reference(doc, options, &mut plan);
    plan_player_safe_start(doc, options, &mut plan);
    plan_layout(doc, options, &mut plan);
    // Long silences are split with gap fillers; refuse spans no file can hold.
    for (i, t) in doc.tracks().iter().enumerate() {
        let mut end = plan
            .content_end(t, i)
            .max(plan.corrected_content_end(i).unwrap_or(0));
        if i == 0 {
            end = end.max(plan.last_tick());
        }
        let fillers = plan.map_tick(end) / MAX_DELTA_TICKS;
        if fillers > MAX_FILLERS_PER_TRACK {
            plan.blockers.push(format!(
                "Track {} reaches tick {}. A MIDI file can only bridge {} ticks per event, so it would need {} filler events — too many for a MIDI file. Move those notes or events closer to the start.",
                i + 1,
                group_digits(end),
                group_digits(MAX_DELTA_TICKS),
                group_digits(fillers)
            ));
        }
    }
    plan
}

/// v1.0-Launch L-C04: whether any written chunk holds a control event —
/// controller, program change, channel pressure or pitch bend (what Kiva's
/// loader counts as one). Stops at the first; O(events) only for a file
/// without any.
pub fn has_control_event(doc: &PlumDocument, plan: &ExportPlan) -> bool {
    doc.tracks().iter().enumerate().any(|(i, t)| {
        plan.events_of(t, i).any(|(_, e)| {
            matches!(e.kind, RawKind::Channel { status, .. }
                if matches!(status & 0xF0, 0xB0 | 0xC0 | 0xD0 | 0xE0))
        })
    })
}

/// v1.0-Launch L-C04 ([`ExportOptions::player_safe_start`]): adds Program
/// Change 0 at tick 0 — after the Conductor's tick-0 events, before the
/// first note — on the lowest channel the notes use (channel 1 without
/// notes) when the file would hold no control event.
fn plan_player_safe_start(doc: &PlumDocument, options: ExportOptions, plan: &mut ExportPlan) {
    // A loudness reference already writes a control event (its Channel
    // Volume) and a loud note.
    if !options.player_safe_start
        || plan.loudness_reference.is_some()
        || has_control_event(doc, plan)
    {
        return;
    }
    let channel = doc
        .tracks()
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let pairs = match plan.placed(i) {
                Some(p) => placed_channel_sub_pairs(&p.lanes),
                None => t.notes.channel_sub_pairs(),
            };
            pairs.iter().map(|&(ch, _)| ch).min()
        })
        .min()
        .unwrap_or(0)
        & 0x0F;
    let at = plan
        .events
        .iter()
        .position(|e| e.tick > 0 || e.anchor > 0)
        .unwrap_or(plan.events.len());
    plan.events.insert(
        at,
        PlanEvent {
            tick: 0,
            anchor: 0,
            rank: u32::MAX,
            kind: PlanKind::DefaultProgram(channel),
        },
    );
    plan.player_safe_start = Some(channel);
}

/// v1.0-Launch L-S51 ([`ExportOptions::loudness_reference`]): plans the
/// silent loudness reference when the song's loudest Note On is below 127.
/// O(notes) (stops at the first velocity-127 note), only when asked for.
fn plan_loudness_reference(doc: &PlumDocument, options: ExportOptions, plan: &mut ExportPlan) {
    if !options.loudness_reference || doc.track_count() == 0 {
        return;
    }
    let mut loudest = 0u8;
    let mut lowest = u8::MAX;
    let mut used = [false; 16];
    'tracks: for (i, t) in doc.tracks().iter().enumerate() {
        for n in plan.notes_as_stored(t, i) {
            let a = n.attrs;
            used[usize::from(a.channel() & 0x0F)] = true;
            if a.muted() {
                continue; // not written
            }
            loudest = loudest.max(a.velocity());
            lowest = lowest.min(a.pitch());
            if loudest >= 127 {
                break 'tracks;
            }
        }
        for (_, e) in plan.events_of(t, i) {
            if let RawKind::Channel { status, .. } = e.kind {
                used[usize::from(status & 0x0F)] = true;
            }
        }
    }
    if loudest == 0 || loudest >= 127 {
        return; // no notes, or the file already has a velocity-127 note
    }
    let Some(channel) = [15u8, 14, 13, 12, 11, 10, 8, 7, 6, 5, 4, 3, 2, 1, 0]
        .into_iter()
        .find(|&c| !used[usize::from(c)])
    else {
        plan.loudness_reference_skipped = Some(
            "No loudness reference for Piano From Above was added: it needs a MIDI channel the song does not use, and every channel except the drum channel is in use.".into(),
        );
        return;
    };
    let tick = doc
        .track(0)
        .map_or(0, |t0| super::export::planned_eot_unmapped(t0, 0, plan))
        .max(1);
    // Channel Volume 0 after the Conductor's tick-0 events.
    let at = plan
        .events
        .iter()
        .position(|e| e.tick > 0 || e.anchor > 0)
        .unwrap_or(plan.events.len());
    plan.events.insert(
        at,
        PlanEvent {
            tick: 0,
            anchor: 0,
            rank: u32::MAX,
            kind: PlanKind::VolumeZero(channel),
        },
    );
    // The zero-length note after every other event at its tick.
    let at = plan
        .events
        .iter()
        .position(|e| e.tick > tick)
        .unwrap_or(plan.events.len());
    let key = lowest.min(127);
    for (k, kind) in [
        PlanKind::ReferenceOn { channel, key },
        PlanKind::ReferenceOff { channel, key },
    ]
    .into_iter()
    .enumerate()
    {
        plan.events.insert(
            at + k,
            PlanEvent {
                tick,
                anchor: u32::MAX,
                rank: u32::MAX,
                kind,
            },
        );
    }
    plan.loudness_reference = Some(LoudnessReference {
        channel,
        key,
        tick,
        loudest,
    });
}

/// v0.9 clips: what each track that is not simply its own identity clip
/// exports ([`PlacedTrack`]). O(tracks + placements + events of the clip
/// tracks + chunks); no note is copied.
fn plan_sources(doc: &PlumDocument, plan: &mut ExportPlan) {
    if doc.clips_are_identity() {
        return;
    }
    plan.placed = doc
        .placed_contents_by_track()
        .iter()
        .zip(doc.tracks())
        .map(|(placed, t)| {
            if let [pc] = placed.as_slice()
                && pc.placement.is_identity()
                && pc.placement.content == crate::model::ContentRef::Track(t.id())
            {
                return None;
            }
            Some(PlacedTrack::new(t, placed))
        })
        .collect();
}

/// Counts the notes above key 127 the export would meet (after the
/// corrections it applies) and refuses the export while the policy is
/// [`WideKeys::Reject`]. A standard project cannot hold such notes: O(1);
/// otherwise one pass over the notes and the preserved events.
///
/// Preserved Note On / Off events with a key byte above 127 (unmatched notes
/// of a 256-key import) are written only by [`WideKeys::Experimental`]; the
/// standard policies leave them out (the track's written events become a
/// filtered copy in `corrected_events`), since folding or clamping an
/// unmatched event could pair it with a different note.
fn plan_wide_keys(doc: &PlumDocument, options: ExportOptions, plan: &mut ExportPlan) {
    plan.wide = WideKeyReport {
        policy: options.wide_keys,
        ..WideKeyReport::default()
    };
    if doc.project().keys == KeyRange::Standard {
        return;
    }
    let (mut notes, mut highest, mut raw) = (0u64, 0u8, 0u64);
    for (i, t) in doc.tracks().iter().enumerate() {
        for n in super::export::export_notes_as_stored(t, i, plan) {
            let p = n.attrs.pitch();
            if p > MAX_PITCH {
                notes += 1;
                highest = highest.max(p);
            }
        }
        let mut wide_raw = 0u64;
        for (_, e) in plan.events_of(t, i).filter(|(_, e)| is_wide_raw(e)) {
            wide_raw += 1;
            if let RawKind::Channel { data, .. } = e.kind {
                highest = highest.max(data[0]);
            }
        }
        if wide_raw == 0 {
            continue;
        }
        raw += wide_raw;
        if !options.wide_keys.writes_wide() {
            let kept: Vec<RawEvent> = plan
                .events_of(t, i)
                .filter(|(_, e)| !is_wide_raw(e))
                .map(|(tick, e)| RawEvent { tick, ..*e })
                .collect();
            if plan.corrected_events.len() < doc.track_count() {
                plan.corrected_events.resize(doc.track_count(), None);
            }
            plan.corrected_events[i] = Some(kept);
        }
    }
    plan.wide.notes = notes;
    plan.wide.highest = highest;
    plan.wide.raw_events = raw;
    if (notes > 0 || raw > 0) && options.wide_keys == WideKeys::Reject {
        let what = match (notes, raw) {
            (_, 0) => format!(
                "{} note(s) are above key 127 (the highest is {highest}).",
                group_digits(notes)
            ),
            (0, _) => format!(
                "{} unmatched Note On / Off event(s) are above key 127 (the highest is {highest}).",
                group_digits(raw)
            ),
            _ => format!(
                "{} note(s) and {} unmatched Note On / Off event(s) are above key 127 (the highest is {highest}).",
                group_digits(notes),
                group_digits(raw)
            ),
        };
        plan.blockers.push(format!(
            "{what} A Standard MIDI file holds keys 0–127 only, so nothing was written. Choose what happens to them: {}, {} or {} — or write an {} file.",
            WideKeys::Discard.label(),
            WideKeys::Fold.label(),
            WideKeys::Clamp.label(),
            WideKeys::Experimental.label()
        ));
    }
}

/// A preserved Note On / Off whose key byte is above 127 (only a 256-key
/// import keeps such events).
#[inline]
fn is_wide_raw(e: &RawEvent) -> bool {
    matches!(e.kind, RawKind::Channel { status, data } if status & 0xE0 == 0x80 && data[0] > MAX_PITCH)
}

/// Whether plain MIDI reproduces a track's subchannels: MIDI import gives a
/// track one subchannel per MIDI channel its notes use (numbered in channel
/// order, named "Ch N"), or a single unnamed main subchannel.
fn inferable(t: &Track, pairs: &[(u8, u32)]) -> bool {
    let mut channels: Vec<u8> = pairs.iter().map(|p| p.0).collect();
    channels.dedup();
    if channels.len() != pairs.len() {
        return false; // a channel with several subchannels
    }
    let subs = t.subchannels().entries();
    if channels.len() <= 1 {
        return subs.len() == 1
            && subs[0].id.0 == 0
            && subs[0].name.is_none()
            && pairs.iter().all(|p| p.1 == 0);
    }
    subs.len() == channels.len()
        && subs
            .iter()
            .zip(&channels)
            .enumerate()
            .all(|(i, (sc, &ch))| {
                sc.id.0 == i as u32
                    && sc.midi_channel == ch
                    && sc.name.as_deref() == Some(format!("Ch {}", u16::from(ch) + 1).as_str())
            })
        && pairs.iter().enumerate().all(|(i, p)| p.1 == i as u32)
}

/// Decides the `MTrk` chunks: one per track, plus layers for subchannels that
/// share a MIDI channel, and the structure records. O(notes) (one pass per
/// track to find its channel/subchannel combinations).
fn plan_layout(doc: &PlumDocument, options: ExportOptions, plan: &mut ExportPlan) {
    use crate::midi::seqspec::TrackNodeRecord;
    let rows = doc.tree_rows();
    let sequences = doc.conductor().scope() == ConductorScope::Sequences;
    let pairs: Vec<Vec<(u8, u32)>> = doc
        .tracks()
        .iter()
        .enumerate()
        .map(|(i, t)| match plan.placed(i) {
            Some(p) => placed_channel_sub_pairs(&p.lanes),
            None => t.notes.channel_sub_pairs(),
        })
        .collect();
    let needs_structure = doc
        .tracks()
        .iter()
        .zip(&pairs)
        .any(|(t, p)| t.parent().is_some() || !inferable(t, p));
    let metadata = options.structure_metadata && needs_structure;
    let mut report = StructureReport {
        metadata,
        ..StructureReport::default()
    };
    let mut outputs = Vec::with_capacity(doc.track_count());
    let ordinals = doc.track_ordinals();
    for (i, (t, p)) in doc.tracks().iter().zip(&pairs).enumerate() {
        if rows[i].0 > 0 {
            report.subtracks += 1;
        }
        report.subchannels += t.subchannels().len();
        if t.subchannels().len() > 16 {
            report.tracks_over_16 += 1;
        }
        // Group the combinations by channel: layer k takes each channel's k-th
        // subchannel.
        let mut layers: Vec<Vec<(u8, u32)>> = vec![Vec::new()];
        let mut prev: Option<u8> = None;
        let mut k = 0usize;
        for &(ch, sub) in p {
            k = if prev == Some(ch) { k + 1 } else { 0 };
            prev = Some(ch);
            if layers.len() <= k {
                layers.push(Vec::new());
            }
            layers[k].push((ch, sub));
        }
        let collides = layers.len() > 1;
        let split = collides && options.split_subchannels && !sequences;
        if collides && !split {
            report.merged_tracks += 1;
        }
        let used_layers = if split { layers.len() } else { 1 };
        for (k, layer) in layers.iter().take(used_layers).enumerate() {
            let record = metadata.then(|| {
                TrackNodeRecord {
                    node: t.id().0,
                    parent: t.parent().map(|p| p.0),
                    layer: k as u32,
                    map: layer.clone(),
                    subs: if k == 0 {
                        t.subchannels_for_saving()
                            .entries()
                            .iter()
                            .map(|e| {
                                (
                                    e.id.0,
                                    e.midi_channel,
                                    e.name.clone().map(String::into_bytes),
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                }
                .encode()
            });
            let name = (k > 0)
                .then(|| format!("{} (layer {})", t.label_with(ordinals[i]), k + 1).into_bytes());
            outputs.push(OutputTrack {
                track: i,
                layer: k as u32,
                pairs: split.then(|| layer.clone()),
                slots: split.then(|| OutputTrack::slots_of(layer)),
                record,
                name,
            });
        }
        report.extra_chunks += used_layers - 1;
    }
    if outputs.len() > usize::from(u16::MAX) {
        plan.blockers.push(format!(
            "Keeping the subchannels apart needs {} MIDI tracks, more than the 65,535 a MIDI file can hold. Export again without keeping subchannels apart.",
            group_digits(outputs.len() as u64)
        ));
    }
    plan.outputs = outputs;
    plan.structure = report;
}

/// Resolves every track's corrections (when applied) and checks them.
fn plan_corrections(doc: &PlumDocument, options: ExportOptions, plan: &mut ExportPlan) {
    if !doc.tracks().iter().any(|t| t.props().corrects()) {
        return;
    }
    if !options.apply_corrections {
        plan.corrections_skipped = doc.tracks().iter().filter(|t| t.props().corrects()).count();
        return;
    }
    let meter = MeterMap::new(doc.division(), &doc.meter_events(0));
    plan.corrections = vec![None; doc.track_count()];
    plan.corrected_events = vec![None; doc.track_count()];
    for (i, t) in doc.tracks().iter().enumerate() {
        let Some(c) = Correction::resolve(t.props(), &meter, doc.project().keys) else {
            continue;
        };
        let report = correction::check_parts(
            t.id(),
            i,
            &c,
            plan.notes_as_stored(t, i),
            plan.uncorrected_events(t, i)
                .map(|(tick, e)| RawEvent { tick, ..*e }),
        );
        if report.blocks_export() {
            plan.blockers.extend(
                report
                    .notes(&format!("Track {}", i + 1))
                    .into_iter()
                    .take(2),
            );
        }
        if c.ticks != 0 && c.from_tick <= plan.uncorrected_last_event(t, i).unwrap_or(0) {
            let corrected = correction::corrected_events(
                plan.uncorrected_events(t, i)
                    .map(|(tick, e)| RawEvent { tick, ..*e }),
                &c,
            );
            plan.corrected_events[i] = Some(corrected);
        }
        plan.corrections[i] = Some(c);
        plan.correction_reports.push(report);
    }
    if plan
        .correction_reports
        .iter()
        .any(CorrectionReport::blocks_export)
    {
        plan.blockers.push(
            "Export the original notes instead (without Track Properties corrections), or change the corrections.".into(),
        );
    }
}

fn build_conductor_events(doc: &PlumDocument, plan: &mut ExportPlan) {
    let c = doc.conductor();
    if c.scope() == ConductorScope::Sequences {
        return;
    }
    let mut b = PlanBuilder {
        first_track: doc.track(0).map(|t| t.id()),
        events: Vec::new(),
        arena: Vec::new(),
        doc,
    };
    let _ = b.doc;
    if c.scope() == ConductorScope::Timeline {
        let ppqn = doc.division().musical_ppqn().unwrap_or(1);
        for s in c.time_signatures().iter().filter(|s| !s.implicit) {
            if let Some(kind) = b.meta(
                meta::TIME_SIGNATURE,
                &[s.numerator, s.denom_pow, s.clocks, s.thirty_seconds],
            ) {
                b.push(s.tick, s.origin, kind, 0);
                plan.signatures += 1;
            }
        }
        // Tempo: steps are written as they are; ramps as generated steps. The
        // tolerance is relaxed (with approval) only when the budget requires it.
        let points = c.tempo_points();
        let ramps: Vec<usize> = (0..points.len())
            .filter(|&i| {
                points[i].interp == Interp::Linear
                    && points.get(i + 1).is_some_and(|n| {
                        n.tick > points[i].tick && points[i].bpm_f64() != n.bpm_f64()
                    })
            })
            .collect();
        let mut tol = RAMP_TOLERANCE_S;
        let generated: Option<Vec<Vec<RampStep>>> = loop {
            let mut budget = RAMP_EVENT_BUDGET;
            let mut all = Vec::with_capacity(ramps.len());
            let mut ok = true;
            for &i in &ramps {
                let (p, n) = (&points[i], &points[i + 1]);
                match ramp_steps(
                    ppqn,
                    p.bpm_f64(),
                    n.bpm_f64(),
                    n.tick - p.tick,
                    tol,
                    &mut budget,
                ) {
                    Some(steps) => all.push(steps),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                break Some(all);
            }
            if tol >= MAX_RAMP_TOLERANCE_S {
                break None;
            }
            tol = (tol * 4.0).min(MAX_RAMP_TOLERANCE_S);
            plan.tempo.coarsened = true;
        };
        plan.tempo.tolerance_ms = tol * 1000.0;
        let Some(generated) = generated else {
            plan.blockers.push(format!(
                "The tempo ramps of this project would need more than {} tempo events in a MIDI file, even at one-second accuracy. Replace some ramps with steps and export again.",
                group_digits(RAMP_EVENT_BUDGET as u64)
            ));
            return;
        };
        let mut ramp_iter = ramps.iter().zip(generated);
        let mut next_ramp = ramp_iter.next();
        for (i, p) in points.iter().enumerate() {
            if p.implicit {
                continue;
            }
            if let Some((&ri, steps)) = next_ramp.as_ref().map(|(ri, s)| (*ri, s))
                && ri == i
            {
                plan.tempo.ramps += 1;
                for (k, st) in steps.iter().enumerate() {
                    let origin = if k == 0 { p.origin } else { None };
                    b.push(p.tick + st.at, origin, PlanKind::Tempo(st.micros), 1);
                    plan.tempo.ramp_events += 1;
                    plan.tempo.max_ramp_error_ms =
                        plan.tempo.max_ramp_error_ms.max(st.error_s * 1000.0);
                    if st.clamped {
                        plan.tempo.clamped_steps += 1;
                    }
                }
                next_ramp = ramp_iter.next();
                continue;
            }
            let (us, fidelity, bpm) = micros_of(p.value);
            if let (Some(f), Some(bpm)) = (fidelity, bpm)
                && f.fidelity != TempoFidelity::Exact
            {
                plan.tempo.lossy.push(LossyTempo {
                    tick: p.tick,
                    bpm,
                    midi: f,
                });
            }
            b.push(p.tick, p.origin, PlanKind::Tempo(us), 1);
        }
    }
    for m in c.markers() {
        match m.end {
            None => {
                let ty = if m.cue { META_CUE } else { META_MARKER };
                if let Some(kind) = b.meta(ty, m.title_bytes()) {
                    b.push(m.start, m.origin, kind, 2);
                    plan.markers += 1;
                }
            }
            Some(end) => {
                let rec = seqspec::chapter_record(m.id, end - m.start, m.title_bytes());
                let end_label = seqspec::end_label(m.title_bytes());
                if let (Some(k1), Some(k2), Some(k3)) = (
                    b.meta(META_MARKER, m.title_bytes()),
                    b.meta(seqspec::META_SEQUENCER_SPECIFIC, &rec),
                    b.meta(META_MARKER, &end_label),
                ) {
                    b.push(m.start, m.origin, k1, 2);
                    b.push(m.start, m.origin, k2, 3);
                    b.push(end, None, k3, 4);
                    plan.chapters += 1;
                }
            }
        }
    }
    for p in c.master_volume() {
        let payload = crate::model::conductor::master_volume_sysex(p.device, p.value);
        if let Some(kind) = b.sysex(&payload) {
            b.push(p.tick, p.origin, kind, 5);
            plan.master_volume += 1;
        }
    }
    // Final order: tick, then placement among the track's events, then the
    // original order of imported events, then kind.
    let mut events = b.events;
    events.sort_by_key(|(e, order)| (e.tick, e.anchor, e.rank, *order));
    // With a rescale two tempo steps can land on one tick; the later one wins.
    let mut out: Vec<PlanEvent> = Vec::with_capacity(events.len());
    for (e, _) in events {
        if let (Some(last), PlanKind::Tempo(_)) = (out.last_mut(), e.kind)
            && matches!(last.kind, PlanKind::Tempo(_))
            && plan_map(plan, last.tick) == plan_map(plan, e.tick)
            && plan.rescale.is_some()
        {
            *last = e;
            continue;
        }
        out.push(e);
    }
    plan.tempo.events = out
        .iter()
        .filter(|e| matches!(e.kind, PlanKind::Tempo(_)))
        .count();
    plan.events = out;
    plan.arena = b.arena;
}

fn plan_map(plan: &ExportPlan, tick: u64) -> u64 {
    plan.map_tick(tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::conductor::Interp;
    use crate::model::note::{NoteAttrs, NoteRecord};
    use crate::model::tempo::TempoMap;
    use crate::model::timing::Division;

    fn rec(s: u64, d: u64) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::default_for_pitch(60).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    fn set_start(d: &mut PlumDocument, bpm: &str) {
        let e = d
            .conductor()
            .prepare_set_start_tempo(TempoValue::Bpm(Bpm::parse(bpm).unwrap()))
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
    }

    fn tempos(p: &ExportPlan) -> Vec<(u64, u32)> {
        p.events
            .iter()
            .filter_map(|e| match e.kind {
                PlanKind::Tempo(us) => Some((e.tick, us)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rescale_rounding_is_monotonic_and_half_up() {
        assert_eq!(rescale_tick(0, 100_000, 25_000), 0);
        assert_eq!(rescale_tick(2, 100_000, 25_000), 1); // 0.5 → 1
        assert_eq!(rescale_tick(1, 100_000, 25_000), 0); // 0.25 → 0
        assert_eq!(rescale_tick(u64::MAX, 100_000, 25_000), u64::MAX / 4 + 1);
        let mut prev = 0;
        for t in 0..10_000u64 {
            let v = rescale_tick(t, 99_991, 30_720);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn start_tempo_and_signature_are_planned_with_fidelity() {
        let mut d = PlumDocument::new_project("T".into(), Bpm::parse("120").unwrap(), 960);
        let p = plan_export(&d);
        assert_eq!(p.division_word, 960);
        assert!(p.rescale.is_none());
        assert_eq!(tempos(&p), vec![(0, 500_000)]);
        assert_eq!(p.signatures, 1);
        assert!(!p.needs_approval());
        assert!(p.disclosures().is_empty());
        set_start(&mut d, "0.5");
        let p = plan_export(&d);
        assert!(p.needs_approval());
        assert_eq!(p.disclosures().len(), 1);
        set_start(&mut d, "123.456");
        let p = plan_export(&d);
        assert!(!p.needs_approval());
        assert!(
            p.disclosures()[0].contains("486,003"),
            "{:?}",
            p.disclosures()
        );
        // An imported file without a tempo event writes none (implicit default).
        let e = PlumDocument::new_empty(Division::ppqn(96));
        assert!(tempos(&plan_export(&e)).is_empty());
        assert_eq!(plan_export(&e).signatures, 0);
    }

    #[test]
    fn plan_rescale_exact_and_lossy() {
        // 96,000 PPQN with every tick a multiple of 12 → 32,000 PPQN exactly.
        let mut d = PlumDocument::new_project("T".into(), Bpm::DEFAULT, 96_000);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(0, 96_000), rec(24_000, 12)], false)
            .unwrap();
        let p = plan_export(&d);
        let r = p.rescale.as_ref().unwrap();
        assert_eq!((r.to_ppqn, r.exact), (32_000, true), "{r:?}");
        assert_eq!(u32::from(p.division_word), r.to_ppqn);
        assert!(p.needs_approval());
        // An odd tick forces a lossy conversion at the largest divisor of 96,000.
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(24_000, 1)], false)
            .unwrap();
        let p = plan_export(&d);
        let r = p.rescale.unwrap();
        assert_eq!(r.to_ppqn, 32_000);
        assert!(!r.exact);
        assert_eq!(r.notes_moved, 1);
        assert_eq!(r.notes_collapsed, 1);
        // A prime PPQN falls back to 30,720.
        let mut d = PlumDocument::new_project("T".into(), Bpm::DEFAULT, 1_000_003);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[rec(7, 1)], false)
            .unwrap();
        assert_eq!(
            plan_export(&d).rescale.unwrap().to_ppqn,
            FALLBACK_EXPORT_PPQN
        );
        // SMPTE never rescales and writes no tempo.
        let s = PlumDocument::new_empty(Division::from_raw(0xE728));
        let p = plan_export(&s);
        assert_eq!(p.division_word, 0xE728);
        assert!(tempos(&p).is_empty());
    }

    /// Exported time at `tick` for a list of (tick, µs) steps.
    fn step_seconds(steps: &[(u64, u32)], ppqn: f64, tick: u64) -> f64 {
        let mut t = 0.0;
        for (i, &(at, us)) in steps.iter().enumerate() {
            if at >= tick {
                break;
            }
            let end = steps.get(i + 1).map_or(tick, |n| n.0.min(tick));
            t += (end - at) as f64 * f64::from(us) / ppqn / 1e6;
        }
        t
    }

    fn ramp_doc(b0: &str, b1: &str, len: u64, ppqn: u32) -> PlumDocument {
        let mut d = PlumDocument::new_project("R".into(), Bpm::parse(b0).unwrap(), ppqn);
        let id = d.conductor().tempo_points()[0].id;
        let e = d
            .conductor()
            .prepare_set_interp(id, Interp::Linear)
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
        let e = d
            .conductor()
            .prepare_set_tempo(
                None,
                len,
                TempoValue::Bpm(Bpm::parse(b1).unwrap()),
                Interp::Step,
            )
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
        d
    }

    #[test]
    fn ramps_are_approximated_within_the_declared_error() {
        for (b0, b1, len, ppqn) in [
            ("100", "200", 960 * 4 * 16, 960u32), // ascending, long
            ("240", "60", 960 * 4 * 4, 960),      // descending
            ("120", "121", 96, 96),               // short, gentle
            ("30", "3000", 480 * 64, 480),        // steep
        ] {
            let d = ramp_doc(b0, b1, len, ppqn);
            let p = plan_export(&d);
            let steps = tempos(&p);
            assert_eq!(p.tempo.ramps, 1);
            assert!(!p.tempo.coarsened);
            // Endpoints: a step at the ramp start and the end tempo at the end.
            assert_eq!(steps[0].0, 0);
            let end = *steps.last().unwrap();
            assert_eq!(end.0, len);
            assert_eq!(
                end.1,
                Bpm::parse(b1).unwrap().to_micros_per_quarter().micros
            );
            // Timing error against the exact tempo map, sampled densely.
            let exact = TempoMap::from_points(48_000, ppqn, d.conductor().tempo_points());
            let mut worst = 0.0f64;
            for i in 0..=2000u64 {
                let t = len * i / 2000;
                let e = (step_seconds(&steps, f64::from(ppqn), t) - exact.seconds_at_tick(t)).abs();
                worst = worst.max(e);
            }
            assert!(worst <= RAMP_TOLERANCE_S + 2e-6, "{b0}->{b1}: {worst}");
            assert!(p.tempo.max_ramp_error_ms <= RAMP_TOLERANCE_S * 1000.0 + 0.01);
            // Not an event explosion: far fewer steps than ticks.
            assert!(steps.len() < 2000, "{b0}->{b1}: {} steps", steps.len());
            assert!(p.disclosures().iter().any(|l| l.contains("tempo ramp")));
            // The project itself keeps the exact ramp.
            assert_eq!(d.conductor().tempo_points()[0].interp, Interp::Linear);
        }
    }

    #[test]
    fn a_constant_ramp_is_a_single_step() {
        let d = ramp_doc("120", "120", 9600, 960);
        let p = plan_export(&d);
        assert_eq!(p.tempo.ramps, 0);
        assert_eq!(tempos(&p), vec![(0, 500_000), (9600, 500_000)]);
    }

    /// A conductor with `n` alternating steep ramps (10 ↔ 3000 BPM, 64 beats each).
    fn many_ramps(n: u32) -> PlumDocument {
        let mut d = PlumDocument::new_project("R".into(), Bpm::parse("10").unwrap(), 960);
        let c = d.conductor();
        let mut tempo = Vec::with_capacity(n as usize + 1);
        for i in 0..=n {
            let bpm = if i % 2 == 0 { "10" } else { "3000" };
            tempo.push(crate::model::conductor::TempoPoint {
                id: 10 + i,
                tick: u64::from(i) * 960 * 64,
                value: TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                interp: if i < n { Interp::Linear } else { Interp::Step },
                implicit: false,
                origin: None,
            });
        }
        let conductor = crate::model::conductor::Conductor::from_parts(
            c.scope(),
            tempo,
            c.time_signatures().to_vec(),
            Vec::new(),
            n + 11,
        )
        .unwrap();
        *d.conductor_mut() = conductor;
        d
    }

    #[test]
    fn huge_ramp_counts_are_coarsened_with_approval_or_refused() {
        // 4,000 steep ramps need about 147 steps each at 0.5 ms: over the budget.
        let d = many_ramps(4_000);
        let p = plan_export(&d);
        assert!(p.blockers.is_empty(), "{:?}", p.blockers);
        assert_eq!(p.tempo.ramps, 4_000);
        assert!(p.tempo.ramp_events <= RAMP_EVENT_BUDGET);
        assert!(p.tempo.coarsened);
        assert!(p.needs_approval());
        assert!(p.tempo.tolerance_ms > 0.5);
        assert!(p.tempo.max_ramp_error_ms <= p.tempo.tolerance_ms + 0.01);
        assert!(
            p.disclosures()
                .iter()
                .any(|l| l.contains("coarser accuracy"))
        );
        // More ramps than the budget has events: refused, nothing is written.
        let d = many_ramps(RAMP_EVENT_BUDGET as u32 + 1);
        let p = plan_export(&d);
        assert_eq!(p.blockers.len(), 1, "{:?}", p.blockers);
        assert!(p.blockers[0].contains("tempo ramps"));
        assert!(
            p.events
                .iter()
                .all(|e| !matches!(e.kind, PlanKind::Tempo(_)))
        );
    }
}
