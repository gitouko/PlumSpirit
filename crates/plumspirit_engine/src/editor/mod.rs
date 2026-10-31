//! Note editing: creation, deletion, movement, resizing, copy/paste and the Flip /
//! Scale transforms.
//!
//! # Atomicity
//!
//! Every operation validates its complete result *before* touching the document. An
//! operation either changes every selected note or none; a rejected request leaves
//! the document bit-for-bit unchanged (verified by fingerprint tests). No operation
//! snapshots the document.
//!
//! # Prepare / commit
//!
//! Operations that can be expensive on huge selections are split into a pure
//! `prepare_*` step, which only reads the document and may run on a worker thread,
//! and a cheap [`commit`] step that applies the prepared result on the owning
//! thread. A prepared edit remembers the note host (a track's own notes or an
//! owned content, [`target`]) and the note-store revision it was computed
//! from, so a stale result (the document changed meanwhile) is refused.

pub mod clip_merge;
pub mod clip_repeat;
pub mod clipboard;
pub mod clips;
pub mod controllers;
pub mod curves;
pub mod expr;
pub mod factor;
pub mod folders;
pub mod gate;
pub mod ops;
pub mod ppqn;
pub mod preview;
pub mod quantize;
pub mod ranges;
pub mod target;
pub mod transforms;

pub use clipboard::Clipboard;
pub use controllers::{ControllerKind, ControllerPoint, EventsEdit};
pub use factor::{ExactFactor, FactorError};
pub use ops::*;
pub use quantize::{
    QuantizeMode, QuantizePreview, QuantizeSpec, prepare_quantize, preview_quantize,
    preview_quantize_all,
};
pub use target::{EventsWork, HostView, NoteTarget, note_host, note_view, notes_mut};
pub use transforms::*;

use crate::midi::group_digits;
use crate::model::clip::ContentRef;
use crate::model::note::NoteRecord;
use crate::model::note_store::NoteStoreError;
use crate::model::track::TrackId;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Why an edit was refused. Nothing is modified when an edit returns an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    NothingSelected,
    ClipboardEmpty,
    InvalidFactor(FactorError),
    /// The result would place a note beyond the largest representable tick.
    TickOverflow {
        operation: &'static str,
    },
    /// The result would place a note before tick 0.
    NegativeTick {
        operation: &'static str,
        earliest: i128,
    },
    /// The result would leave the project's keys (0..=`top`: 127, or 255 in
    /// 256-Key Mode).
    PitchOutOfRange {
        operation: &'static str,
        lowest: i32,
        highest: i32,
        top: u8,
    },
    /// Resizing would make a note zero-length or negative.
    DurationTooShort,
    /// Note-on velocities must be 1–127 (0 would mean note-off).
    InvalidVelocity(u8),
    /// A typed velocity that is not a whole number from 1 to 127.
    VelocityText(String),
    /// A typed gate (note length) that is not a whole number of ticks ≥ 1.
    GateText(String),
    /// A controller value outside its range (pitch bend 0–16383, CC 0–127).
    InvalidController(controllers::ControllerKind),
    /// An operation would generate more events than the memory budget allows.
    TooManyEvents {
        requested: usize,
        limit: usize,
    },
    /// Curve tools do not generate channel-mode messages (CC 120–127).
    ChannelModeCurve,
    /// A typed track name that is not valid.
    TrackName(String),
    /// A curve or custom function could not be evaluated (with the reason).
    Curve(String),
    /// Track properties that cannot be applied (with the reason).
    TrackProperties(String),
    /// A subchannel or hierarchy edit that cannot be applied (full sentence).
    Invalid(String),
    /// A combined edit whose undo data would exceed the whole undo memory.
    UndoTooLarge {
        needed: usize,
        limit: usize,
    },
    /// MIDI channels are 1–16 (0–15 internally).
    InvalidChannel(u8),
    /// A quantize grid that is not a positive whole number of ticks.
    InvalidGrid(String),
    OutOfMemory {
        bytes: usize,
    },
    /// The track or its notes changed after this edit was prepared.
    Stale,
    Cancelled,
    NoSuchTrack,
}

impl EditError {
    pub fn user_message(&self) -> String {
        match self {
            EditError::NothingSelected => "Select one or more notes first.".into(),
            EditError::Invalid(m) => m.clone(),
            EditError::ClipboardEmpty => "The clipboard is empty. Copy some notes first.".into(),
            EditError::InvalidFactor(e) => e.to_string(),
            EditError::TickOverflow { operation } => format!(
                "{operation} would move notes beyond the largest possible tick. Nothing was changed."
            ),
            EditError::NegativeTick {
                operation,
                earliest,
            } => format!(
                "{operation} would move notes before the start of the song (to tick {earliest}). Nothing was changed."
            ),
            EditError::PitchOutOfRange {
                operation,
                lowest,
                highest,
                top,
            } => {
                let detail = if *highest > i32::from(*top) {
                    if *top == crate::model::note::MAX_PITCH {
                        format!("up to pitch {highest}, above the highest MIDI note ({top})")
                    } else {
                        format!("up to pitch {highest}, above the project's highest key ({top})")
                    }
                } else {
                    format!("down to pitch {lowest}, below the lowest MIDI note (0)")
                };
                format!("{operation} would move notes {detail}. Nothing was changed.")
            }
            EditError::DurationTooShort => "Notes cannot be shorter than one tick.".into(),
            EditError::InvalidVelocity(v) => {
                format!("Velocity {v} is not valid. Note velocities range from 1 to 127.")
            }
            EditError::VelocityText(t) => t.clone(),
            EditError::GateText(t) => t.clone(),
            EditError::InvalidController(k) => match *k {
                controllers::ControllerKind::PitchBend => {
                    "Pitch bend ranges from -8192 to +8191 (0 = no bend).".into()
                }
                controllers::ControllerKind::PAN => {
                    "Pan ranges from 0 (left) to 127 (right); 64 is the center.".into()
                }
                controllers::ControllerKind::Cc(n) => format!(
                    "{} (CC {n}) values range from 0 to 127{}.",
                    k.label(),
                    if k.is_switch() {
                        " (0–63 off, 64–127 on)"
                    } else {
                        ""
                    }
                ),
            },
            EditError::TooManyEvents { requested, limit } => format!(
                "This would create {} events, more than the {} one operation may create with the available memory. Use a longer interval or a shorter range. Nothing was changed.",
                group_digits(*requested as u64),
                group_digits(*limit as u64)
            ),
            EditError::ChannelModeCurve => "Channel-mode messages (CC 120–127) are commands such as All Notes Off; add them one at a time instead of drawing curves. Nothing was changed.".into(),
            EditError::TrackName(t) | EditError::Curve(t) | EditError::TrackProperties(t) => {
                t.clone()
            }
            EditError::UndoTooLarge { needed, limit } => format!(
                "Undo information for this edit needs about {} MB, more than the {} MB undo memory. Nothing was changed.",
                needed / (1024 * 1024) + 1,
                limit / (1024 * 1024)
            ),
            EditError::InvalidChannel(c) => {
                format!(
                    "Channel {} does not exist; MIDI channels are 1 to 16.",
                    u32::from(*c) + 1
                )
            }
            EditError::InvalidGrid(t) => t.clone(),
            EditError::OutOfMemory { bytes } => format!(
                "Not enough memory for this operation (about {} MB needed). Nothing was changed.",
                bytes / (1024 * 1024) + 1
            ),
            EditError::Stale => {
                "The notes changed while the operation was running; it was not applied.".into()
            }
            EditError::Cancelled => "The operation was canceled. Nothing was changed.".into(),
            EditError::NoSuchTrack => "That track no longer exists.".into(),
        }
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for EditError {}

impl From<FactorError> for EditError {
    fn from(e: FactorError) -> Self {
        EditError::InvalidFactor(e)
    }
}

impl From<NoteStoreError> for EditError {
    fn from(e: NoteStoreError) -> Self {
        match e {
            NoteStoreError::EndOverflow { .. } => EditError::TickOverflow {
                operation: "This edit",
            },
            NoteStoreError::DurationOutOfRange => EditError::DurationTooShort,
            NoteStoreError::OutOfMemory { bytes } => EditError::OutOfMemory { bytes },
            NoteStoreError::UnsortedBatch => EditError::Stale,
        }
    }
}

/// Progress and cancellation for long-running prepares.
#[derive(Default, Clone, Copy)]
pub struct EditControl<'a> {
    pub cancel: Option<&'a AtomicBool>,
    /// Receives the number of notes processed.
    pub progress: Option<&'a AtomicU64>,
}

impl EditControl<'_> {
    pub(crate) fn tick(&self, done: u64) -> Result<(), EditError> {
        if let Some(p) = self.progress {
            p.store(done, Ordering::Relaxed);
        }
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(EditError::Cancelled);
        }
        Ok(())
    }
}

/// A uniform move: every note shifted by the same ticks and semitones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shift {
    pub ticks: i128,
    pub pitch: i32,
}

/// A pitch for every key 0–255 (v0.9 256-Key Mode). Entries outside the
/// project's key range are the identity and never consulted.
pub type PitchTable = [u8; 256];

/// The identity [`PitchTable`].
pub fn identity_pitch_table() -> Box<PitchTable> {
    let mut t = Box::new([0u8; 256]);
    for (p, slot) in t.iter_mut().enumerate() {
        *slot = p as u8;
    }
    t
}

/// The result of a `prepare_*` call, applied with [`commit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedEdit {
    /// Nothing to do (for example Scale by exactly 1).
    Identity,
    /// Replace the selected notes with `records` (sorted by start); the new notes
    /// are selected.
    ReplaceSelected {
        host: ContentRef,
        base_revision: u64,
        records: Vec<NoteRecord>,
        /// Set when `records` are the selected notes shifted uniformly (a move), so
        /// the undo history can store the shift instead of a second copy.
        shift: Option<Shift>,
    },
    /// v0.9 V09-S39 (Draw "replace under the stroke"): replace the selected
    /// notes with `records` (sorted by start) and delete the unselected notes
    /// equal to `erase` (each record one note, found when applied) — one
    /// change, undone and redone as one patch like [`PreparedEdit::ReplaceSelected`].
    ReplaceSelectedErasing {
        host: ContentRef,
        base_revision: u64,
        records: Vec<NoteRecord>,
        erase: Vec<NoteRecord>,
    },
    /// Rewrite the pitch of every selected note through `table` (time order is
    /// unaffected, so no index rebuild is needed).
    RemapSelectedPitch {
        host: ContentRef,
        base_revision: u64,
        table: Box<PitchTable>,
        /// Number of selected notes when prepared (for reporting).
        notes: usize,
    },
    /// Insert `records` (sorted by start) as the new selection.
    InsertSelected {
        host: ContentRef,
        base_revision: u64,
        records: Vec<NoteRecord>,
        /// The track whose subchannel table interprets the records (the host
        /// itself, or the track an owned content is edited on).
        subs_track: TrackId,
        /// Subchannels `subs_track` lacks, added first (paste from another
        /// track). Every record's subchannel is in that table or in this list.
        new_subs: Vec<crate::model::track::Subchannel>,
    },
}

impl PreparedEdit {
    /// Number of notes the commit will write.
    pub fn affected_notes(&self) -> usize {
        match self {
            PreparedEdit::Identity => 0,
            PreparedEdit::ReplaceSelected { records, .. }
            | PreparedEdit::InsertSelected { records, .. } => records.len(),
            PreparedEdit::ReplaceSelectedErasing { records, erase, .. } => {
                records.len() + erase.len()
            }
            PreparedEdit::RemapSelectedPitch { notes, .. } => *notes,
        }
    }
}

/// Applies a prepared edit. Fails with [`EditError::Stale`] (changing nothing) if
/// the note host was deleted or its notes changed since preparation.
pub fn commit(doc: &mut crate::model::PlumDocument, edit: PreparedEdit) -> Result<(), EditError> {
    match edit {
        PreparedEdit::Identity => Ok(()),
        PreparedEdit::ReplaceSelected {
            host,
            base_revision,
            records,
            ..
        } => {
            let keys = doc.project().keys;
            let notes = doc.notes_of_mut(host).ok_or(EditError::Stale)?;
            if notes.revision() != base_revision {
                return Err(EditError::Stale);
            }
            check_sorted_records(&records)?;
            check_keys(&records, keys)?;
            notes.remove_selected();
            notes.insert_sorted_batch(&records, true)?;
            Ok(())
        }
        PreparedEdit::ReplaceSelectedErasing {
            host,
            base_revision,
            records,
            erase,
        } => {
            let keys = doc.project().keys;
            let notes = doc.notes_of_mut(host).ok_or(EditError::Stale)?;
            if notes.revision() != base_revision {
                return Err(EditError::Stale);
            }
            check_sorted_records(&records)?;
            check_keys(&records, keys)?;
            let gone = erase_ranks(notes, &erase)?;
            let mut all: Vec<usize> = notes.selected_ranks().iter().collect();
            all.extend(gone.iter().map(|g| g.0));
            all.sort_unstable();
            let mut b = crate::model::patch::RankSetBuilder::default();
            for r in all {
                b.push(r);
            }
            notes
                .select_ranks(&b.finish())
                .map_err(|_| EditError::Stale)?;
            notes.remove_selected();
            notes.insert_sorted_batch(&records, true)?;
            Ok(())
        }
        PreparedEdit::RemapSelectedPitch {
            host,
            base_revision,
            table,
            ..
        } => {
            let notes = doc.notes_of_mut(host).ok_or(EditError::Stale)?;
            if notes.revision() != base_revision {
                return Err(EditError::Stale);
            }
            notes.map_selected_attrs(|a| {
                // Table entries were validated against the project's key
                // range during prepare.
                a.with_pitch_wide(table[usize::from(a.pitch())])
                    .unwrap_or(a)
            });
            Ok(())
        }
        PreparedEdit::InsertSelected {
            host,
            base_revision,
            records,
            subs_track,
            new_subs,
        } => {
            let keys = doc.project().keys;
            if doc.notes_of(host).ok_or(EditError::Stale)?.revision() != base_revision {
                return Err(EditError::Stale);
            }
            check_sorted_records(&records)?;
            check_keys(&records, keys)?;
            let table = doc
                .track_by_id(subs_track)
                .ok_or(EditError::Stale)?
                .subchannels()
                .swapped(&[], &new_subs)
                .map_err(EditError::Invalid)?;
            if records.iter().any(|r| !table.contains(r.sub)) {
                return Err(EditError::Stale);
            }
            let notes = doc.notes_of_mut(host).ok_or(EditError::Stale)?;
            notes.clear_selection();
            notes.insert_sorted_batch(&records, true)?;
            if !new_subs.is_empty()
                && let Some(t) = doc.track_by_id_mut(subs_track)
            {
                *t.subchannels_mut() = table;
            }
            Ok(())
        }
    }
}

/// Re-checks the invariants `insert_sorted_batch` relies on so that the removal in
/// `ReplaceSelected` can never be followed by a failing insertion.
/// Every record's pitch is a key of the project (v0.9: a standard project
/// never receives a pitch above 127 — a paste or score from a 256-key project
/// is refused, never clamped).
fn check_keys(
    records: &[NoteRecord],
    keys: crate::model::project::KeyRange,
) -> Result<(), EditError> {
    let top = keys.max_pitch();
    if let Some(hi) = records
        .iter()
        .map(|r| r.attrs.pitch())
        .filter(|&p| p > top)
        .max()
    {
        let lo = records.iter().map(|r| r.attrs.pitch()).min().unwrap_or(hi);
        return Err(EditError::PitchOutOfRange {
            operation: "This edit",
            lowest: i32::from(lo),
            highest: i32::from(hi),
            top,
        });
    }
    Ok(())
}

/// The ranks of the unselected notes equal to `records` (each record one
/// note), sorted, with their records ([`PreparedEdit::ReplaceSelectedErasing`]).
/// A record without its note is stale. O(records × notes sounding at their
/// starts).
pub(crate) fn erase_ranks(
    store: &crate::model::NoteStore,
    records: &[NoteRecord],
) -> Result<Vec<(usize, NoteRecord)>, EditError> {
    let mut out: Vec<(usize, NoteRecord)> = Vec::with_capacity(records.len());
    let mut taken = std::collections::HashSet::new();
    for r in records {
        let mut pick = None;
        store.query(r.start_tick, r.start_tick, |loc, v| {
            if pick.is_some() || v.selected || v.start_tick != r.start_tick || v.record() != *r {
                return;
            }
            if let Some(rank) = store.rank_of(loc)
                && !taken.contains(&rank)
            {
                pick = Some(rank);
            }
        });
        let rank = pick.ok_or(EditError::Stale)?;
        taken.insert(rank);
        out.push((rank, *r));
    }
    out.sort_unstable_by_key(|x| x.0);
    Ok(out)
}

fn check_sorted_records(records: &[NoteRecord]) -> Result<(), EditError> {
    for w in records.windows(2) {
        if w[0].start_tick > w[1].start_tick {
            return Err(EditError::Stale);
        }
    }
    if records.iter().any(|r| r.end_tick().is_none()) {
        return Err(EditError::TickOverflow {
            operation: "This edit",
        });
    }
    Ok(())
}

/// Parses a typed note velocity: a whole number from 1 to 127. Out-of-range
/// values are refused, never clamped.
pub fn parse_velocity(input: &str) -> Result<u8, EditError> {
    let t = input.trim();
    match t.parse::<i64>() {
        Ok(v) if (1..=127).contains(&v) => Ok(v as u8),
        Ok(0) => Err(EditError::VelocityText(
            "Velocity 0 would turn the note off; note velocities range from 1 to 127.".into(),
        )),
        Ok(_) => Err(EditError::VelocityText(
            "Note velocities range from 1 to 127.".into(),
        )),
        Err(_) => Err(EditError::VelocityText(format!(
            "\"{}\" is not a whole number. Enter a velocity from 1 to 127.",
            t.chars().take(40).collect::<String>()
        ))),
    }
}

/// Parses a typed gate (note length) in ticks: a whole number ≥ 1 up to the
/// largest tick. Separators `_` and `,` are allowed. Nothing is wrapped or clamped.
pub fn parse_gate(input: &str) -> Result<u64, EditError> {
    let t = input.trim();
    let s = t.replace(['_', ','], "");
    if s.is_empty() {
        return Err(EditError::GateText(
            "Enter a gate length in ticks, for example 240.".into(),
        ));
    }
    if let Some(rest) = s.strip_prefix('-')
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(EditError::GateText("A gate cannot be negative.".into()));
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(EditError::GateText(format!(
            "\"{}\" is not a whole number of ticks.",
            t.chars().take(40).collect::<String>()
        )));
    }
    match s.parse::<u64>() {
        Ok(0) => Err(EditError::GateText(
            "The gate must be at least 1 tick.".into(),
        )),
        Ok(v) => Ok(v),
        Err(_) => Err(EditError::GateText(format!(
            "The gate can be at most {} ticks (the largest tick PlumSpirit can store).",
            group_digits(u64::MAX)
        ))),
    }
}

/// Reserves a vector for `n` records, failing gracefully instead of aborting.
pub(crate) fn try_records(n: usize) -> Result<Vec<NoteRecord>, EditError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| EditError::OutOfMemory {
        bytes: n.saturating_mul(std::mem::size_of::<NoteRecord>()),
    })?;
    Ok(v)
}
