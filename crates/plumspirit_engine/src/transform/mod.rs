//! The Note Transformation Platform (v0.7).
//!
//! One pipeline for every note tool (contract §20–29):
//!
//! ```text
//! selection ─► Source (immutable snapshot of the selected notes, columnar)
//!           ─► recipe (tool id + ParamSet) ─► Kernel (built once per parameter change)
//!           ─► preflight (estimate: output notes, RAM, undo bytes) ─► Preview
//!           ─► validation ─► commit (per-track ReplaceSelected, one undo Group)
//! ```
//!
//! * **Same kernel** for Preview and Apply: the preview is the kernel's output for
//!   the same source; Apply commits that output (or recomputes it with the same
//!   inputs, which is deterministic).
//! * **Pointwise** kernels (each note independently) also preview lazily: only the
//!   notes near the viewport are evaluated, nothing is copied (millions of
//!   selected notes cost nothing per frame).
//! * **Determinism**: randomness comes from [`rng`] keyed by the seed and the note's
//!   identity, never by evaluation order.
//! * **Bounded**: every kernel declares its growth; [`preflight`] refuses impossible
//!   expansions before anything is allocated, and [`Output`] enforces the cap and
//!   the cancel flag while generating.
//! * **Subchannels** are preserved by default: a kernel that routes notes to other
//!   subchannels uses only ids of the destination track's table.
//!
//! Detailed design: `docs/TRANSFORM_ENGINE.md`.

pub mod boundary;
pub mod definition;
pub mod drivers;
pub mod editors;
pub mod functions;
pub mod generator;
pub mod geom;
pub mod image_midi;
pub mod order;
pub mod output;
pub mod overlap;
pub mod param;
pub mod paste;
pub mod preflight;
pub mod preset;
pub mod psc;
pub mod registry;
pub mod repeat;
pub mod rng;
pub mod run;
pub mod shared;
pub mod source;
pub mod spatial;
pub mod sweep;
pub mod theory;
pub mod tools;

pub use boundary::{Boundary, LenPolicy, PitchPolicy, TickPolicy};
pub use output::{OutStats, Output};
pub use param::{Curve, Outside, ParamKind, ParamSet, ParamSpec, ParamValue, Params};
pub use preflight::{Estimate, Growth, Limits};
pub use registry::{
    Family, FunctionSpec, REMOVED_TOOLS, ToolDef, ToolSpec, command_tool, registry, removed_tool,
    tool,
};
pub use run::{Prepared, TransformRun, commit, prepare_records, run_kernel};
pub use source::{Source, SourceStats, SourceTrack, TrackScope};

use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
use crate::view::grid::MeterMap;
use std::fmt;

/// One note inside the platform: flat, 32 bytes, no heap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TNote {
    pub start: u64,
    pub dur: u64,
    pub sub: u32,
    /// Note group (0 = none).
    pub group: u32,
    /// Slot in [`Source::tracks`] (the track the note belongs to / goes to).
    pub track: u16,
    pub pitch: u8,
    pub vel: u8,
    /// Note-off (release) velocity.
    pub rel: u8,
    /// MIDI channel 0–15.
    pub ch: u8,
    /// Bit 0: terminated by Note On velocity 0. Bit 1: muted.
    pub flags: u8,
}

pub const FLAG_NOTE_ON_ZERO: u8 = 1;
/// The note is muted (stays in the project, does not play / export / count).
pub const FLAG_MUTED: u8 = 2;

impl TNote {
    pub fn from_record(r: &NoteRecord, track: u16) -> TNote {
        TNote {
            start: r.start_tick,
            dur: r.duration_ticks,
            sub: r.sub.0,
            group: r.group,
            track,
            pitch: r.attrs.pitch(),
            vel: r.attrs.velocity(),
            rel: r.attrs.off_velocity(),
            ch: r.attrs.channel(),
            flags: match r.attrs.off_kind() {
                NoteOffKind::NoteOff => 0,
                NoteOffKind::NoteOnZero => FLAG_NOTE_ON_ZERO,
            } | if r.attrs.muted() { FLAG_MUTED } else { 0 },
        }
    }

    /// The stored record (fields are validated by [`Output`], so this cannot fail
    /// for notes that came out of it; invalid values are clamped defensively).
    pub fn to_record(&self) -> NoteRecord {
        let kind = if self.flags & FLAG_NOTE_ON_ZERO != 0 {
            NoteOffKind::NoteOnZero
        } else {
            NoteOffKind::NoteOff
        };
        // The pitch was checked against the project's key range by the
        // Output (0–255 in 256-Key Mode).
        let attrs = NoteAttrs::new_wide(
            self.pitch,
            self.vel.clamp(1, 127),
            self.ch.min(15),
            self.rel.min(127),
            kind,
        )
        .expect("clamped attributes are valid")
        .with_muted(self.flags & FLAG_MUTED != 0);
        NoteRecord {
            start_tick: self.start,
            duration_ticks: self.dur.max(1),
            attrs,
            sub: SubchannelId(self.sub),
            group: self.group,
        }
    }

    #[inline]
    pub fn muted(&self) -> bool {
        self.flags & FLAG_MUTED != 0
    }

    #[inline]
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.dur)
    }
}

/// What kernels may know about the project besides the notes.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// Ticks per quarter note (960 for SMPTE files).
    pub ppq: u64,
    /// Bars and beats (bar-aligned periods, Absolute patterns).
    pub meter: MeterMap,
    /// The Piano Roll's snap step in ticks (≥ 1).
    pub snap: u64,
    /// The transport / edit cursor (where generators place material when
    /// nothing is selected).
    pub cursor: u64,
    /// The tempo map at 1 MHz (seconds ↔ ticks for millisecond parameters).
    /// `None` → 120 BPM.
    pub tempo: Option<std::sync::Arc<crate::model::TempoMap>>,
    /// The project's note groups (group-aware tools; new ids start at
    /// `groups.next_id()`).
    pub groups: std::sync::Arc<crate::model::GroupTable>,
    /// A pointer stroke drawn in the Piano Roll for brush / lasso tools (Draw,
    /// Paint, Select lassos), in absolute ticks and pitch rows.
    pub stroke: Option<std::sync::Arc<Stroke>>,
    /// The project's key range (v0.9 256-Key Mode; tools size pitch tables
    /// and limits from it).
    pub keys: crate::model::project::KeyRange,
    /// v0.9 V09-S11: the song tick of tick 0 of the notes in scope (an
    /// opened clip's `start − offset`; 0 for a track's own notes). Tempo
    /// conversions ([`Ctx::seconds_at`], [`Ctx::tick_at_seconds`]) read the
    /// song's tempo map there.
    pub shift: i128,
    /// v0.9 V09-S34: the Conductor's marker (and chapter) starts in song
    /// ticks, ascending (anchors of the Quantizer's meter-aware grid; the
    /// tools' tick = song tick − [`Ctx::shift`]).
    pub markers: std::sync::Arc<Vec<u64>>,
    /// v0.9 V09-S38: the Conductor's chapters `[start, end)` in song ticks,
    /// by start (their starts are in [`Ctx::markers`] too; Clipping Mask
    /// n10).
    pub chapters: std::sync::Arc<Vec<(u64, u64)>>,
    /// v0.9 V09-S35: the ruler's time selection `[start, end)` in the tools'
    /// ticks (song tick − [`Ctx::shift`]), if any (Riff Machine n07).
    pub time_range: Option<(u64, u64)>,
    /// v0.9 V09-S39: the project's loop range `[start, end)` in the tools'
    /// ticks (song tick − [`Ctx::shift`]), if one is set (Delete n08 crops
    /// to it).
    pub loop_range: Option<(u64, u64)>,
    /// v0.9 V09-S39: the Select tool's stored selection slots 1 – 9 (the
    /// app's, this session; Select n10 combines its result with one).
    pub selection_slots: std::sync::Arc<Vec<Vec<SlotNote>>>,
    /// v0.9 V09-S36: the active track's index (the Piano Roll's track — an
    /// opened clip's host track), when the run comes from the editor (Join
    /// n07 keeps cross-track duplicates there).
    pub active: Option<usize>,
    /// v1.0-beta2 B2-F16: the track the edited notes are shown on — the
    /// Piano Roll's track, which is an opened clip's track also when the clip
    /// shows another track's content (`active` then names the content's
    /// track). Tools that read or write "the track below / above" count from
    /// it ([`source::edit_place`]); `None`: the first scope's track.
    pub host: Option<usize>,
    /// v1.0-beta2 B2-A02: per scope (parallel to the run's scopes; empty =
    /// all 0) where that note host's content tick 0 lies on the edited
    /// notes' ticks — another scoped track's clip elsewhere in the song
    /// ([`source::SourceTrack::frame`]). The app resolves them
    /// (`app::tools::scopes_framed`).
    pub scope_frames: std::sync::Arc<Vec<i128>>,
    /// v0.9 V09-S36 (V09-0143): the enabled Control Sweeps of a tool that
    /// evaluates them itself per unit ([`definition::kernel_sweeps`]: Group
    /// Notes per group) — set by [`functions::wrap`] when it builds such a
    /// tool's kernel; `None` otherwise.
    pub sweeps: Option<std::sync::Arc<sweep::UnitSweeps>>,
    /// v0.9 V09-S47: the picture a tool's map parameter names (Displacement
    /// Map's "Picture file"), decoded by the app (the engine has no image
    /// decoders), or the decoder's sentence. `None`: no picture was asked
    /// for or none is loaded.
    pub picture: Option<Result<std::sync::Arc<image_midi::Rgba>, String>>,
    /// B1-0117: the General Note Overlap Prevention the app runs on the
    /// finished run (its preview and its Apply alike), when it is on.
    /// Kernels never read it.
    pub general_overlap: Option<overlap::General>,
}

/// A note of a stored selection slot (Select n10, V09-S39): its track and
/// the fields a selection is matched by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotNote {
    pub track: crate::model::TrackId,
    pub start: u64,
    pub dur: u64,
    pub pitch: u8,
    pub vel: u8,
    pub sub: u32,
    pub ch: u8,
}

/// One sample of a pointer stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokePoint {
    pub tick: u64,
    /// Pitch row as a real number (row `p` covers `[p, p + 1)`).
    pub pitch: f64,
    /// Seconds since the stroke began (drag speed for pressure brushes).
    pub t: f64,
}

/// A pointer stroke (the samples in drawing order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stroke {
    pub points: Vec<StrokePoint>,
}

/// Most samples a stroke keeps (longer strokes are thinned evenly).
pub const MAX_STROKE_POINTS: usize = 65_536;

/// Sample rate of [`Ctx::tempo`] (microsecond frames).
pub const TEMPO_RATE: u32 = 1_000_000;

impl Ctx {
    pub fn simple(ppq: u64) -> Ctx {
        Ctx {
            ppq: ppq.max(1),
            meter: MeterMap::new(
                crate::model::timing::Division::ppqn(ppq.clamp(1, 0x7FFF) as u32),
                &[(0, 4, 2)],
            ),
            snap: (ppq / 4).max(1),
            cursor: 0,
            tempo: None,
            groups: Default::default(),
            stroke: None,
            keys: Default::default(),
            shift: 0,
            markers: Default::default(),
            chapters: Default::default(),
            time_range: None,
            loop_range: None,
            selection_slots: Default::default(),
            active: None,
            host: None,
            scope_frames: Default::default(),
            sweeps: None,
            picture: None,
            general_overlap: None,
        }
    }

    /// Ticks of one 4/4 bar.
    pub fn bar(&self) -> u64 {
        self.ppq * 4
    }

    /// Song tick of `tick` (with [`Ctx::shift`]).
    fn song_tick(&self, tick: u64) -> u64 {
        (i128::from(tick) + self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }

    pub fn seconds_at(&self, tick: u64) -> f64 {
        let tick = self.song_tick(tick);
        match &self.tempo {
            Some(t) => t.seconds_at_tick(tick),
            None => tick as f64 / self.ppq as f64 * 0.5,
        }
    }

    pub fn tick_at_seconds(&self, s: f64) -> u64 {
        let song = match &self.tempo {
            Some(t) => t.tick_at_frame((s.max(0.0) * f64::from(TEMPO_RATE)).round() as u64),
            None => (s.max(0.0) * 2.0 * self.ppq as f64).round() as u64,
        };
        (i128::from(song) - self.shift).clamp(0, i128::from(u64::MAX)) as u64
    }
}

/// Why a transformation cannot run or apply (full sentences for the UI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransformError {
    NothingSelected,
    /// Preflight: the result would be too large.
    TooLarge(String),
    Cancelled,
    /// Parameters that cannot work (full sentence).
    Invalid(String),
    /// The document changed after the preview was computed.
    Stale,
    /// The undo history refused (with its message).
    History(String),
    /// The result equals the notes it would replace: nothing to apply (no
    /// undo step, the project stays unchanged).
    Unchanged,
}

impl TransformError {
    pub fn user_message(&self) -> String {
        match self {
            TransformError::NothingSelected => "Select one or more notes first.".into(),
            TransformError::TooLarge(m)
            | TransformError::Invalid(m)
            | TransformError::History(m) => m.clone(),
            TransformError::Cancelled => "The operation was canceled.".into(),
            TransformError::Unchanged => {
                "Nothing to apply: the result is the same as the selected notes.".into()
            }
            TransformError::Stale => {
                "The notes changed while the preview was computed; the preview has been refreshed."
                    .into()
            }
        }
    }
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for TransformError {}

/// A kernel: the single authoritative rule of one tool for one parameter set.
pub trait Kernel: Send + Sync {
    /// How the output size relates to the input (preflight).
    fn growth(&self) -> Growth;

    /// Upper bound of output notes for this source (default from [`Kernel::growth`]).
    fn estimate(&self, stats: &SourceStats) -> Estimate {
        Estimate::from_growth(self.growth(), stats)
    }

    /// Upper bound of output notes with the source notes at hand — what the
    /// preflight of [`run::run_kernel`] checks. Default: [`Kernel::estimate`]
    /// from the statistics. A kernel whose growth depends on where the notes
    /// lie (Slice Notes: only the notes a cut crosses grow) counts it here in
    /// one pass without allocating, so a large selection with few affected
    /// notes is not refused by a worst-case bound.
    fn estimate_source(&self, src: &Source) -> Estimate {
        self.estimate(&src.stats)
    }

    /// Pointwise kernels expose their per-note rule (lazy viewport preview).
    fn as_point(&self) -> Option<&dyn PointKernel> {
        None
    }

    /// Remove Note Overlaps to run on the finished result (v0.9 §231; the
    /// platform's mandatory function — never pointwise).
    fn overlaps(&self) -> Option<overlap::Policy> {
        None
    }

    /// Evaluates the whole source.
    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let p = self
            .as_point()
            .ok_or_else(|| TransformError::Invalid("This tool has no rule.".into()))?;
        for n in &src.notes {
            p.map(n, out);
            out.check()?;
        }
        Ok(())
    }
}

/// A rule applied to each note independently (0, 1 or a few outputs per note).
pub trait PointKernel: Send + Sync {
    fn map(&self, n: &TNote, out: &mut Output);

    /// A source tick window `[a, b]` such that every note whose output can
    /// intersect `[t0, t1]` intersects `[a, b]` (conservative).
    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64);
}
