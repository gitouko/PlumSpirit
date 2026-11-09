//! Undo / redo.
//!
//! # Design
//!
//! History never snapshots the document. Every edit goes through [`History::apply`],
//! which captures an operation-specific *patch* right before the change and then
//! applies it:
//!
//! | Operation | What is remembered |
//! |---|---|
//! | Draw, paste | positions of the new notes (+ their compact values) |
//! | Delete | positions + compact values of the removed notes |
//! | Move | positions before/after + the removed notes; the new notes are re-derived from the shift |
//! | Flip X, Scale X | positions before/after + compact old and new notes |
//! | Flip Y, Scale Y, pitch-only move | positions + 1 byte (old pitch) per note + the 128-entry pitch table |
//! | Velocity | positions + 1 byte (old velocity) per note + the new velocity |
//! | Resize | positions + the duration delta |
//! | Gate (set duration) | positions + old durations (varints) + the new duration |
//! | Erase | positions + compact values of the erased notes |
//! | Add / delete track | the track object itself (ownership moves between document and history) |
//! | Conductor (tempo, time signature, marker) | the removed and inserted entries |
//! | Pitch bend / pan events | the removed and inserted events |
//! | Project name | old and new value |
//!
//! A *position* is a note's rank in its track's stored order (see
//! `model::patch`), so undo restores notes exactly where they were, including the
//! order of notes that start on the same tick. Positions are run-length encoded;
//! note values take 6–9 bytes each.
//!
//! One user gesture is one entry: a drag, a paste of 10,000 notes or a Scale X over
//! a million notes each produce a single entry.
//!
//! # Resource bounds
//!
//! Stored bytes are counted per entry. The history keeps at most
//! [`HistoryLimits::max_bytes`] and [`HistoryLimits::max_entries`]; the oldest
//! entries are dropped first (dropping the oldest entry never breaks a newer one,
//! because every entry only depends on the state right before it). Before a large
//! edit is committed its patch is built and measured; if it alone would exceed the
//! budget the edit is **not** applied and [`HistoryError::TooLarge`] hands the
//! operation back, so the UI can ask whether to apply it without undo
//! ([`History::apply_without_undo`], which clears the history).
//!
//! # Saved state
//!
//! Each document state has an id. An entry knows the ids before and after it; undo
//! and redo move the current id. The document is clean exactly when the current id
//! equals the id recorded at the last successful save — so "save, edit, undo" is
//! clean again, and "save A, edit B, save, undo" is dirty. Ids are never reused, so
//! a saved state that was discarded (redo branch cleared, entry evicted) can never
//! be mistaken for the current one.

use crate::editor::controllers::EventsEdit;
use crate::editor::{self, EditError, NoteTarget, PreparedEdit, Shift};
use crate::model::clip::ContentRef;
use crate::model::conductor::{ConductorEdit, ConductorError};
use crate::model::document::{DocumentError, PlumDocument};
use crate::model::events::RawEvent;
use crate::model::note::{NoteAttrs, NoteRecord, SubchannelId};
use crate::model::note_store::{NoteLoc, NoteStoreError, PatchError};
use crate::model::patch::{CompactRecords, ListPatch, RankSet};
use crate::model::project::KeyRange;
use crate::model::track::{Subchannel, Track, TrackId, TrackProps};
use std::collections::VecDeque;
use std::fmt;

pub mod identity;
pub mod journal;

pub use identity::{IdKey, IdKind, IncarnationSnapshot, Incarnations, ProjectSerial};

/// Undo steps kept (as metadata; their changes may be on disk only) while a
/// persistent journal is attached.
pub const PERSISTED_MAX_ENTRIES: usize = 100_000;

/// Identity of a document state (see module docs).
pub type StateId = u64;

/// Where dragged tracks land relative to a target track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackPlace {
    /// Siblings of the target, just before it.
    Before,
    /// Siblings of the target, right after its subtree.
    After,
    /// The target's last children.
    Into,
}

/// Plans moving the subtrees rooted at `roots` (document order; roots inside
/// another listed subtree are ignored) next to / into `target`, as one
/// [`Operation::Group`] of [`Operation::MoveTracks`] (one undo step). The
/// moved subtrees keep their order. Refuses moves into a moved subtree
/// (cycles); the document validates the result again when applied.
pub fn plan_track_moves(
    doc: &PlumDocument,
    roots: &[usize],
    target: usize,
    place: TrackPlace,
) -> Result<Operation, String> {
    // Work on the shape only: (id, parent).
    let mut shape: Vec<(TrackId, Option<TrackId>)> =
        doc.tracks().iter().map(|t| (t.id(), t.parent())).collect();
    let n = shape.len();
    if target >= n || roots.iter().any(|&r| r >= n) {
        return Err("That track no longer exists.".into());
    }
    let end_of = |shape: &[(TrackId, Option<TrackId>)], i: usize| -> usize {
        let mut stack = vec![shape[i].0];
        let mut j = i + 1;
        while let Some(&(id, p)) = shape.get(j) {
            let Some(p) = p else { break };
            while stack.last().is_some_and(|&top| top != p) {
                stack.pop();
            }
            if stack.is_empty() {
                break;
            }
            stack.push(id);
            j += 1;
        }
        j
    };
    // Top-most roots in document order.
    let mut sorted: Vec<usize> = roots.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ids: Vec<TrackId> = Vec::new();
    let mut covered_until = 0usize;
    for &r in &sorted {
        if r < covered_until {
            continue;
        }
        ids.push(shape[r].0);
        covered_until = end_of(&shape, r);
    }
    let target_id = shape[target].0;
    for &rid in &ids {
        let r = shape.iter().position(|e| e.0 == rid).expect("listed");
        let e = end_of(&shape, r);
        if (r..e).contains(&target) {
            return Err(if rid == target_id {
                "A track cannot be moved onto itself.".into()
            } else {
                "A track cannot be moved into its own subtracks.".into()
            });
        }
    }
    let parent = match place {
        TrackPlace::Into => Some(target_id),
        _ => shape[target].1,
    };
    let mut ops = Vec::new();
    // Each root lands before the target, or after the previous one placed.
    let mut anchor: (TrackId, TrackPlace) = (target_id, place);
    for &rid in &ids {
        let from = shape.iter().position(|e| e.0 == rid).expect("listed");
        let count = end_of(&shape, from) - from;
        let block: Vec<(TrackId, Option<TrackId>)> = shape.drain(from..from + count).collect();
        let t = shape
            .iter()
            .position(|e| e.0 == anchor.0)
            .ok_or("the target moved")?;
        let to = match anchor.1 {
            TrackPlace::Before => t,
            TrackPlace::After | TrackPlace::Into => end_of(&shape, t),
        };
        let mut block = block;
        block[0].1 = parent;
        shape.splice(to..to, block);
        ops.push(Operation::MoveTracks { from, to, parent });
        if anchor.1 != TrackPlace::Before {
            anchor = (rid, TrackPlace::After);
        }
    }
    if ops.is_empty() {
        return Err("Select the tracks to move.".into());
    }
    Ok(Operation::Group {
        ops,
        label: "Move Tracks",
    })
}

/// Resource bounds of the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_bytes: usize,
    pub max_entries: usize,
}

const MIB: usize = 1024 * 1024;

/// L-S48: discarded changes holding at least this much are freed on a
/// short-lived thread of their own.
const RELEASE_ON_THREAD_BYTES: usize = 64 * MIB;

/// L-S48: frees discarded changes (`bytes`: what they hold) — a large set on
/// a short-lived thread: the redo branch of a 224M-note Import Into holds
/// ~4 GB, which Windows takes ~0.5 s to free; the UI thread never waits for it.
fn release_changes(changes: Vec<Change>, bytes: usize) {
    if changes.is_empty() || bytes < RELEASE_ON_THREAD_BYTES {
        return;
    }
    // If no thread can start, the closure (and the changes) is dropped here.
    let _ = std::thread::Builder::new()
        .name("plumspirit-history-free".into())
        .spawn(move || drop(changes));
}

impl HistoryLimits {
    /// 1/16 of physical memory, clamped to 64–512 MiB (64 MiB when unknown), and
    /// 2,000 entries.
    pub fn for_system_memory(total_bytes: Option<u64>) -> Self {
        let budget = total_bytes
            .map(|b| usize::try_from(b / 16).unwrap_or(usize::MAX))
            .unwrap_or(64 * MIB)
            .clamp(64 * MIB, 512 * MIB);
        HistoryLimits {
            max_bytes: budget,
            max_entries: 2_000,
        }
    }
}

impl Default for HistoryLimits {
    fn default() -> Self {
        HistoryLimits::for_system_memory(None)
    }
}

/// An edit that can be applied (and undone).
#[derive(Debug)]
pub enum Operation {
    /// A prepared note edit (move, Flip, Scale, paste). `label` names it in the
    /// Edit menu ("Undo Flip X").
    Prepared {
        edit: PreparedEdit,
        label: &'static str,
    },
    InsertNote {
        track: NoteTarget,
        record: NoteRecord,
    },
    DeleteSelected {
        track: NoteTarget,
    },
    /// Changes the duration of every selected note by `delta` ticks.
    Resize {
        track: NoteTarget,
        delta: i64,
    },
    /// Sets the note-on velocity of every selected note (1–127).
    SetVelocity {
        track: NoteTarget,
        velocity: u8,
    },
    /// Sets the duration (gate) of every selected note to `duration` ticks (≥ 1).
    SetDuration {
        track: NoteTarget,
        duration: u64,
    },
    /// Deletes the notes at `ranks` (one Erase-tool gesture). The selection of
    /// the remaining notes is kept.
    EraseNotes {
        track: NoteTarget,
        ranks: RankSet,
    },
    /// Deletes the notes equal to `records` (each record one note: an
    /// unselected one first, then a selected one), keeping the selection of
    /// the remaining notes — v0.9 V09-S39 Draw "replace under the stroke",
    /// after the tool's own edit in the same undo step. The notes are found
    /// when the operation is applied, so ranks never go stale between the
    /// parts of a group. A record without its note fails (stale).
    EraseRecords {
        track: NoteTarget,
        records: Vec<NoteRecord>,
    },
    AddTrack,
    /// Deletes a track and all of its subtracks (one undo step).
    DeleteTrack {
        index: usize,
    },
    /// Adds `count` empty subtracks as the last children of the track at `parent`.
    AddSubtracks {
        parent: usize,
        count: usize,
    },
    /// Inserts ready-made tracks (whole subtrees in depth-first order with their
    /// parents set; new ids) at `index`. The history takes ownership: nothing is
    /// copied (MIDI import into a track).
    InsertTracks {
        index: usize,
        tracks: Vec<Track>,
        label: &'static str,
    },
    /// Adds subchannels (new ids) to a track.
    AddSubchannels {
        track: usize,
        subs: Vec<Subchannel>,
    },
    /// Changes a subchannel's name or MIDI channel (same id). With
    /// `move_notes`, the notes of that subchannel move to its new MIDI channel.
    EditSubchannel {
        track: usize,
        sub: Subchannel,
        move_notes: bool,
    },
    /// Deletes a subchannel together with every note in it.
    DeleteSubchannel {
        track: usize,
        id: SubchannelId,
    },
    /// Puts the selected notes into subchannel `sub`; with `set_channel` they also
    /// take its MIDI channel.
    SetSubchannel {
        track: NoteTarget,
        sub: SubchannelId,
        set_channel: bool,
    },
    /// A prepared Conductor change (tempo map, time signature, marker).
    Conductor {
        edit: ConductorEdit,
        label: &'static str,
    },
    /// A prepared change of one track's events (controllers, track name).
    Events {
        edit: EventsEdit,
        label: &'static str,
    },
    SetName(String),
    /// Turns 256-Key Mode on (`Extended256`) or off (`Standard`), a Project
    /// setting (v0.9 §058). Turning it off is refused while a note is above
    /// key 127 or a track's key range / key correction needs the wide range.
    SetKeyRange {
        keys: KeyRange,
    },
    /// Sets a track's properties and explicit end (Track Properties, Fit End to
    /// Content).
    TrackProps {
        track: TrackId,
        props: TrackProps,
        end: Option<u64>,
        label: &'static str,
    },
    /// Sets the note-on velocity of each note at `ranks` to the matching value
    /// (velocity curves, Only Before the Note on the velocity graph).
    SetVelocities {
        track: NoteTarget,
        ranks: RankSet,
        values: Vec<u8>,
    },
    /// Moves every selected note to MIDI channel `channel` (0–15).
    SetChannel {
        track: NoteTarget,
        channel: u8,
    },
    /// Moves the subtree rooted at `from` to index `to` of the track list
    /// without it, under `parent` (`None` = top level). Drag-and-drop reorder /
    /// reparent; refused when it would create a cycle or orphan subtracks.
    MoveTracks {
        from: usize,
        to: usize,
        parent: Option<TrackId>,
    },
    /// Several operations as one undo step (all or nothing).
    Group {
        ops: Vec<Operation>,
        label: &'static str,
    },
    /// Replaces the project's note-group table (v0.7: Group / Ungroup, lock,
    /// names, paste remapping). Note group ids change through note edits.
    SetMetadata {
        table: Box<crate::model::metadata::Metadata>,
        label: &'static str,
    },
    SetGroups {
        table: Box<crate::model::GroupTable>,
        label: &'static str,
    },
    /// Replaces the project's Track Folders (v0.8: create, rename, delete,
    /// membership; track moves come as separate operations in a Group).
    SetFolders {
        folders: Box<crate::model::TrackFolders>,
        label: &'static str,
    },
    /// Moves the clip table to `layout` (v0.9 §050: add / move / resize /
    /// mute / rename / delete / duplicate clips — `editor/clips.rs` builds
    /// the layouts). `contents` are the new owned contents the layout lists
    /// (Add Piano Roll Clip, independent copies); owned contents the layout
    /// drops move into the undo record. Nothing is copied.
    SetClips {
        layout: Box<crate::model::ClipLayout>,
        contents: Vec<crate::model::OwnedContent>,
        label: &'static str,
    },
    /// B1-0103: replaces the whole timebase (division, every note and event
    /// store, the Conductor, clip placements, tick settings) with one
    /// prepared by [`crate::editor::ppqn::prepare_change_ppqn`]. Undo swaps
    /// the previous parts back; nothing is copied. Not recorded in the
    /// persistent journal (it restarts after this step).
    SetTimebase {
        timebase: Box<crate::editor::ppqn::Timebase>,
        label: &'static str,
    },
}

impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Operation::Prepared { label, .. } => label,
            Operation::InsertNote { .. } => "Draw Note",
            Operation::DeleteSelected { .. } => "Delete Notes",
            Operation::Resize { .. } => "Resize Notes",
            Operation::SetVelocity { .. } => "Change Velocity",
            Operation::SetDuration { .. } => "Change Gate",
            Operation::EraseNotes { .. } | Operation::EraseRecords { .. } => "Erase Notes",
            Operation::AddTrack => "Add Track",
            Operation::DeleteTrack { .. } => "Delete Track",
            Operation::MoveTracks { .. } => "Move Track",
            Operation::AddSubtracks { count: 1, .. } => "Add Subtrack",
            Operation::AddSubtracks { .. } => "Add Subtracks",
            Operation::InsertTracks { label, .. } => label,
            Operation::AddSubchannels { subs, .. } if subs.len() == 1 => "Add Subchannel",
            Operation::AddSubchannels { .. } => "Add Subchannels",
            Operation::EditSubchannel { .. } => "Change Subchannel",
            Operation::DeleteSubchannel { .. } => "Delete Subchannel",
            Operation::SetSubchannel { .. } => "Move Notes to Subchannel",
            Operation::Conductor { label, .. }
            | Operation::Events { label, .. }
            | Operation::TrackProps { label, .. }
            | Operation::Group { label, .. }
            | Operation::SetMetadata { label, .. }
            | Operation::SetGroups { label, .. }
            | Operation::SetFolders { label, .. }
            | Operation::SetClips { label, .. }
            | Operation::SetTimebase { label, .. } => label,
            Operation::SetName(_) => "Rename Project",
            Operation::SetKeyRange {
                keys: KeyRange::Extended256,
            } => "Turn On 256-Key Mode",
            Operation::SetKeyRange {
                keys: KeyRange::Standard,
            } => "Turn Off 256-Key Mode",
            Operation::SetVelocities { .. } => "Change Velocities",
            Operation::SetChannel { .. } => "Change Channel",
        }
    }
}

/// What an applied operation produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing changed (e.g. Scale ×1, velocity already set) — no entry recorded.
    Unchanged,
    Done,
    /// Location of a drawn note.
    Note(NoteLoc),
    /// Number of notes affected.
    Notes(usize),
    /// Index of the added track, or the track that should become active after a
    /// deletion.
    Track(usize),
}

/// Result of an undo or redo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub label: &'static str,
    /// Track whose notes or events changed (the UI shows it): the first one
    /// of a multi-track step; `None` for project-level changes.
    pub track: Option<TrackId>,
    /// Every track whose notes, events, subchannels or properties changed,
    /// sorted (v0.8): the UI keeps the active track when it is one of them.
    pub tracks: Vec<TrackId>,
    /// Tracks were added or removed.
    pub structure_changed: bool,
    /// The Conductor (tempo, meter, markers) changed.
    pub conductor_changed: bool,
    /// B1-0103: every tick was rescaled from `.0` to `.1` PPQN (Change PPQN,
    /// Preserve beats): views, playheads and time selections follow.
    pub retimed: Option<(u32, u32)>,
}

impl Applied {
    /// The track the UI shows after this step: `current` (the active track)
    /// when the step changed it — undoing a multi-track edit does not jump
    /// to another track — otherwise the first changed track.
    pub fn shown_track(&self, current: Option<TrackId>) -> Option<TrackId> {
        match current {
            Some(c) if self.tracks.binary_search(&c).is_ok() => Some(c),
            _ => self.track,
        }
    }
}

#[derive(Debug)]
pub enum HistoryError {
    Edit(EditError),
    Document(DocumentError),
    Conductor(ConductorError),
    /// Undo data for this edit would need `needed` bytes, more than the whole
    /// history budget. Nothing was changed; the operation is handed back so it can
    /// be applied without undo after the user agrees.
    TooLarge {
        needed: usize,
        limit: usize,
        operation: Box<Operation>,
    },
    /// The history did not match the document (internal error). The history has
    /// been cleared; the document is unchanged.
    Inconsistent(String),
}

impl HistoryError {
    pub fn user_message(&self) -> String {
        match self {
            HistoryError::Edit(e) => e.user_message(),
            HistoryError::Document(e) => e.to_string(),
            HistoryError::Conductor(e) => e.to_string(),
            HistoryError::TooLarge { needed, limit, .. } => format!(
                "Undo information for this edit needs about {} MB, more than the {} MB undo memory. Nothing was changed.",
                needed / MIB + 1,
                limit / MIB
            ),
            HistoryError::Inconsistent(d) => format!(
                "The undo history no longer matched the project ({d}) and was cleared. Your project was not changed."
            ),
        }
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for HistoryError {}

impl From<EditError> for HistoryError {
    fn from(e: EditError) -> Self {
        HistoryError::Edit(e)
    }
}

impl From<ConductorError> for HistoryError {
    fn from(e: ConductorError) -> Self {
        HistoryError::Conductor(e)
    }
}

impl From<DocumentError> for HistoryError {
    fn from(e: DocumentError) -> Self {
        HistoryError::Document(e)
    }
}

impl From<NoteStoreError> for HistoryError {
    fn from(e: NoteStoreError) -> Self {
        HistoryError::Edit(e.into())
    }
}

// ---------------------------------------------------------------- patches

#[derive(Debug)]
enum Inserted {
    Records(CompactRecords),
    /// The removed notes shifted uniformly (a move).
    Shifted(Shift),
}

#[derive(Debug)]
struct NotesPatch {
    track: ContentRef,
    /// Positions of the removed notes in the state before the edit.
    removed_ranks: RankSet,
    removed: CompactRecords,
    /// Positions of the inserted notes in the state after the edit.
    inserted_ranks: RankSet,
    inserted: Inserted,
}

#[derive(Debug)]
enum Change {
    Notes(NotesPatch),
    Pitch {
        track: ContentRef,
        ranks: RankSet,
        old: Vec<u8>,
        table: Box<crate::editor::PitchTable>,
    },
    Velocity {
        track: ContentRef,
        ranks: RankSet,
        old: Vec<u8>,
        new: u8,
    },
    Duration {
        track: ContentRef,
        ranks: RankSet,
        delta: i64,
    },
    /// Durations set to one value; `old` holds the previous durations as varints.
    Durations {
        track: ContentRef,
        ranks: RankSet,
        old: Vec<u8>,
        new: u64,
    },
    Conductor(ConductorEdit),
    Events {
        track: ContentRef,
        patch: ListPatch<RawEvent>,
    },
    /// A subtree moved from `from` to `to` (indices as in
    /// [`Operation::MoveTracks`]) with its root's parent changed.
    Move {
        from: usize,
        to: usize,
        root: TrackId,
        old_parent: Option<TrackId>,
        new_parent: Option<TrackId>,
    },
    /// A track that exists in one state and not the other. `stash` holds it while
    /// it is not in the document.
    /// A block of tracks (whole subtrees) added or removed at `index`.
    Track {
        index: usize,
        count: usize,
        /// Id of the first track of the block (verification).
        id: TrackId,
        present_after: bool,
        shifted: Vec<(u32, u32)>,
        stash: Option<Vec<Track>>,
    },
    /// Subchannel table entries removed, then added (an edit removes the old
    /// entry and adds the new one with the same id).
    Subs {
        track: TrackId,
        added: Vec<Subchannel>,
        removed: Vec<Subchannel>,
    },
    /// Subchannel of the notes at `ranks`.
    NoteSubs {
        track: ContentRef,
        ranks: RankSet,
        old: Vec<u32>,
        new: u32,
    },
    Name {
        old: String,
        new: String,
    },
    /// The project's key range (256-Key Mode on / off).
    Keys {
        old: KeyRange,
        new: KeyRange,
    },
    Props {
        track: TrackId,
        old: Box<(TrackProps, Option<u64>)>,
        new: Box<(TrackProps, Option<u64>)>,
    },
    Velocities {
        track: ContentRef,
        ranks: RankSet,
        old: Vec<u8>,
        new: Vec<u8>,
    },
    Channel {
        track: ContentRef,
        ranks: RankSet,
        old: Vec<u8>,
        new: u8,
    },
    Group(Vec<Change>),
    Metadata {
        old: Box<crate::model::metadata::Metadata>,
        new: Box<crate::model::metadata::Metadata>,
    },
    Groups {
        old: Box<crate::model::GroupTable>,
        new: Box<crate::model::GroupTable>,
    },
    Folders {
        old: Box<crate::model::TrackFolders>,
        new: Box<crate::model::TrackFolders>,
    },
    /// The clip table moved between two layouts (v0.9).
    Clips(Box<ClipsChange>),
    /// B1-0103: the timebase not in the document (swapped in and out).
    Timebase(Box<crate::editor::ppqn::Timebase>),
}

/// A clip-table change: the layouts before and after and the owned contents
/// that are not in the document in its current state (the ones the change
/// adds, before it is applied; the ones it removes, after).
#[derive(Debug)]
struct ClipsChange {
    old: crate::model::ClipLayout,
    new: crate::model::ClipLayout,
    stash: Vec<crate::model::OwnedContent>,
}

impl ClipsChange {
    fn heap_bytes(&self) -> usize {
        self.old.heap_bytes()
            + self.new.heap_bytes()
            + self
                .stash
                .iter()
                .map(|c| {
                    c.notes.heap_bytes()
                        + c.events.heap_bytes()
                        + std::mem::size_of::<crate::model::OwnedContent>()
                })
                .sum::<usize>()
    }
}

/// Moves `doc`'s clip table to `layout` (undo / redo of a clip change) and
/// checks it against the tracks; on error nothing changes.
fn move_clips(
    doc: &mut PlumDocument,
    from: &crate::model::ClipLayout,
    to: &crate::model::ClipLayout,
    stash: &mut Vec<crate::model::OwnedContent>,
) -> Result<(), String> {
    doc.clips_mut().apply_layout(to, stash)?;
    if let Err(m) = doc.validate_clips() {
        doc.clips_mut()
            .apply_layout(from, stash)
            .expect("the previous clip layout is restorable");
        return Err(m);
    }
    doc.touch_layout();
    Ok(())
}

/// What an operation did: the undo record (`None` when nothing changed) and the
/// outcome for the caller.
type Performed = (Option<Change>, Outcome);

impl Change {
    fn heap_bytes(&self) -> usize {
        match self {
            Change::Notes(p) => {
                p.removed_ranks.heap_bytes()
                    + p.removed.heap_bytes()
                    + p.inserted_ranks.heap_bytes()
                    + match &p.inserted {
                        Inserted::Records(r) => r.heap_bytes(),
                        Inserted::Shifted(_) => 0,
                    }
            }
            Change::Pitch { ranks, old, .. } => ranks.heap_bytes() + old.capacity() + 256,
            Change::Velocity { ranks, old, .. } => ranks.heap_bytes() + old.capacity(),
            Change::Duration { ranks, .. } => ranks.heap_bytes(),
            Change::Durations { ranks, old, .. } => ranks.heap_bytes() + old.capacity(),
            Change::Conductor(e) => e.heap_bytes(),
            Change::Events { patch, .. } => patch.heap_bytes(),
            Change::Track { stash, shifted, .. } => {
                shifted.capacity() * 8
                    + stash.as_ref().map_or(0, |v| {
                        v.iter()
                            .map(|t| t.heap_bytes() + std::mem::size_of::<Track>())
                            .sum()
                    })
            }
            Change::Move { .. } => 0,
            Change::Subs { added, removed, .. } => {
                (added.capacity() + removed.capacity()) * std::mem::size_of::<Subchannel>()
                    + added
                        .iter()
                        .chain(removed)
                        .map(|e| e.name.as_ref().map_or(0, String::capacity))
                        .sum::<usize>()
            }
            Change::NoteSubs { ranks, old, .. } => ranks.heap_bytes() + old.capacity() * 4,
            Change::Name { old, new } => old.capacity() + new.capacity(),
            Change::Keys { .. } => 0,
            Change::Metadata { old, new } => old.heap_bytes() + new.heap_bytes() + 64,
            Change::Groups { old, new } => old.heap_bytes() + new.heap_bytes() + 64,
            Change::Folders { old, new } => old.heap_bytes() + new.heap_bytes() + 64,
            Change::Clips(c) => c.heap_bytes() + 64,
            Change::Timebase(t) => t.heap_bytes() + 64,
            Change::Props { old, new, .. } => {
                2 * std::mem::size_of::<(TrackProps, Option<u64>)>()
                    + route_bytes(&old.0)
                    + route_bytes(&new.0)
            }
            Change::Velocities {
                ranks, old, new, ..
            } => ranks.heap_bytes() + old.capacity() + new.capacity(),
            Change::Channel { ranks, old, .. } => ranks.heap_bytes() + old.capacity(),
            Change::Group(v) => v
                .iter()
                .map(|c| c.heap_bytes() + std::mem::size_of::<Change>())
                .sum(),
        }
    }

    fn track(&self) -> Option<TrackId> {
        // A track's own notes name their track; an owned content's edit
        // names none (the Track List finds its placements).
        let own = |c: &ContentRef| match c {
            ContentRef::Track(t) => Some(*t),
            ContentRef::Owned(_) => None,
        };
        match self {
            Change::Notes(p) => own(&p.track),
            Change::Pitch { track, .. }
            | Change::Velocity { track, .. }
            | Change::Duration { track, .. }
            | Change::Durations { track, .. }
            | Change::Events { track, .. }
            | Change::Velocities { track, .. }
            | Change::Channel { track, .. }
            | Change::NoteSubs { track, .. } => own(track),
            Change::Track { id, .. } => Some(*id),
            Change::Move { root, .. } => Some(*root),
            Change::Props { track, .. } | Change::Subs { track, .. } => Some(*track),
            Change::Group(v) => v.iter().find_map(Change::track),
            Change::Conductor(_)
            | Change::Name { .. }
            | Change::Keys { .. }
            | Change::Metadata { .. }
            | Change::Groups { .. }
            | Change::Folders { .. }
            | Change::Clips(_)
            | Change::Timebase(_) => None,
        }
    }

    /// Every track this change touches, sorted, without duplicates.
    fn tracks(&self) -> Vec<TrackId> {
        fn walk(c: &Change, out: &mut Vec<TrackId>) {
            match c {
                Change::Group(v) => v.iter().for_each(|c| walk(c, out)),
                Change::Timebase(t) => out.extend(t.track_ids()),
                other => out.extend(other.track()),
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    fn structure_changed(&self) -> bool {
        match self {
            Change::Track { .. } | Change::Move { .. } => true,
            Change::Group(v) => v.iter().any(Change::structure_changed),
            _ => false,
        }
    }

    /// `(from, to)` PPQN when this change, just applied to `doc`, rescaled
    /// every tick.
    fn retimed(&self, doc: &PlumDocument) -> Option<(u32, u32)> {
        match self {
            Change::Timebase(t) if t.retimes_ticks() => Some((
                t.division().musical_ppqn()? as u32,
                doc.division().musical_ppqn()? as u32,
            )),
            Change::Group(v) => v.iter().find_map(|c| c.retimed(doc)),
            _ => None,
        }
    }

    fn conductor_changed(&self) -> bool {
        match self {
            Change::Conductor(_) | Change::Timebase(_) => true,
            Change::Group(v) => v.iter().any(Change::conductor_changed),
            _ => false,
        }
    }
}

/// Test hook: moves a decoded clip change's document to its "after" layout
/// the way redo does (validated; nothing changes on error).
#[cfg(test)]
fn move_clips_for_test(doc: &mut PlumDocument, c: &mut ClipsChange) -> Result<(), String> {
    let from = doc.clips().layout();
    move_clips(doc, &from, &c.new, &mut c.stash)
}

/// Layouts equal apart from placement revisions.
fn same_layout(a: &crate::model::ClipLayout, b: &crate::model::ClipLayout) -> bool {
    let strip = |l: &crate::model::ClipLayout| crate::model::ClipLayout {
        placements: l
            .placements
            .iter()
            .map(|p| crate::model::ClipPlacement {
                revision: 0,
                ..p.clone()
            })
            .collect(),
        ..l.clone()
    };
    strip(a) == strip(b)
}

/// Applies [`Operation::SetClips`]: the new `contents` must be listed by the
/// layout and not exist yet; the result must pass `validate_clips`. Returns
/// the owned contents the layout dropped. On error nothing changes.
fn set_clips(
    doc: &mut PlumDocument,
    old: &crate::model::ClipLayout,
    layout: &crate::model::ClipLayout,
    contents: Vec<crate::model::OwnedContent>,
) -> Result<Vec<crate::model::OwnedContent>, HistoryError> {
    let invalid = |m: String| {
        HistoryError::Edit(EditError::Invalid(format!(
            "The clips are invalid ({m}). Nothing was changed."
        )))
    };
    for c in &contents {
        if layout.contents.binary_search(&c.id).is_err() || doc.clips().content(c.id).is_some() {
            return Err(invalid(format!("{} cannot be added", c.id)));
        }
    }
    let mut stash = contents;
    move_clips(doc, old, layout, &mut stash).map_err(invalid)?;
    Ok(stash)
}

/// Removes the clip records of the tracks `ids` (about to be deleted) as a
/// clip change; `None` when they have none.
fn purge_clips(
    doc: &mut PlumDocument,
    ids: &[TrackId],
) -> Result<Option<Box<ClipsChange>>, HistoryError> {
    let old = doc.clips().layout();
    let mut probe = crate::model::ClipTable::from_parts(
        Vec::new(),
        old.placements.clone(),
        old.removed_primaries.clone(),
        old.next_clip,
        old.next_content,
    )
    .map_err(HistoryError::Inconsistent)?;
    if probe.purge_tracks(ids) == crate::model::clip::PurgedClips::default() {
        return Ok(None);
    }
    // Owned contents left without a placement leave with the records.
    let mut new = probe.layout();
    new.contents = old
        .contents
        .iter()
        .copied()
        .filter(|&c| {
            new.placements
                .iter()
                .any(|p| p.content == crate::model::ContentRef::Owned(c))
        })
        .collect();
    let mut stash = Vec::new();
    doc.clips_mut()
        .apply_layout(&new, &mut stash)
        .map_err(HistoryError::Inconsistent)?;
    Ok(Some(Box::new(ClipsChange { old, new, stash })))
}

fn route_bytes(p: &TrackProps) -> usize {
    match &p.route {
        crate::model::track::OutputRoute::External(n) => n.capacity(),
        crate::model::track::OutputRoute::Internal => 0,
    }
}

#[derive(Debug)]
struct Entry {
    label: &'static str,
    before: StateId,
    after: StateId,
    /// `None`: only in the journal (read back when undo / redo reaches it).
    change: Option<Change>,
    bytes: usize,
    /// Where the entry is in the journal (persistent history, v0.8).
    disk: Option<journal::DiskRef>,
}

impl Entry {
    fn on_disk(
        label: &'static str,
        before: StateId,
        after: StateId,
        at: journal::DiskRef,
    ) -> Entry {
        Entry {
            label,
            before,
            after,
            change: None,
            bytes: std::mem::size_of::<Entry>(),
            disk: Some(at),
        }
    }
}

/// The undo/redo history of one document.
#[derive(Debug)]
pub struct History {
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    limits: HistoryLimits,
    bytes: usize,
    next_state: StateId,
    current: StateId,
    saved: Option<StateId>,
    evicted: u64,
    /// v0.8 persistent history (see [`journal`]).
    journal: Option<journal::Journal>,
    /// Undo entries below this index are not in the journal (they precede its
    /// base); 0 when every entry is.
    journal_floor: usize,
    /// v1.0-beta1 (B1-0022): which ids vanished, for stale-handle checks;
    /// observed after every step.
    ids: Incarnations,
}

impl Default for History {
    fn default() -> Self {
        History::new(HistoryLimits::default())
    }
}

fn shift_record(r: NoteRecord, s: Shift) -> Option<NoteRecord> {
    let start = u64::try_from(i128::from(r.start_tick) + s.ticks).ok()?;
    let pitch = u8::try_from(i32::from(r.attrs.pitch()) + s.pitch).ok()?;
    Some(NoteRecord {
        start_tick: start,
        duration_ticks: r.duration_ticks,
        attrs: r.attrs.with_pitch_wide(pitch).ok()?,
        sub: r.sub,
        group: r.group,
    })
}

fn inconsistent(e: PatchError) -> String {
    e.to_string()
}

/// Turns 256-Key Mode on or off (v0.9 §058); refused with a sentence while
/// the project uses keys the new range lacks. The document is unchanged on
/// error.
fn set_key_range(doc: &mut PlumDocument, keys: KeyRange) -> Result<(), HistoryError> {
    if let Some(why) = doc.key_range_conflict(keys) {
        return Err(HistoryError::Edit(EditError::Invalid(why)));
    }
    doc.project_mut().keys = keys;
    Ok(())
}

/// Undo / redo of a key-range step: the history guarantees the content fits,
/// so a conflict means the history no longer matches the document.
fn restore_key_range(doc: &mut PlumDocument, keys: KeyRange) -> Result<(), String> {
    if let Some(why) = doc.key_range_conflict(keys) {
        return Err(format!("the key range cannot change back ({why})"));
    }
    doc.project_mut().keys = keys;
    Ok(())
}

impl History {
    pub fn new(limits: HistoryLimits) -> Self {
        History {
            undo: VecDeque::new(),
            redo: Vec::new(),
            limits,
            bytes: 0,
            next_state: 1,
            current: 0,
            saved: None,
            evicted: 0,
            journal: None,
            journal_floor: 0,
            ids: Incarnations::new(),
        }
    }

    pub fn limits(&self) -> HistoryLimits {
        self.limits
    }

    /// Starts over for a newly created (`saved = false`) or opened (`saved = true`)
    /// document.
    pub fn reset(&mut self, saved: bool) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        // A new document: its journal (if any) is attached by the caller.
        self.settle_journal(true);
        self.journal = None;
        self.journal_floor = 0;
        self.current = self.fresh_state();
        self.saved = saved.then_some(self.current);
        self.ids.reset();
    }

    /// The open project's id ledger (B1-0022, [`identity`]).
    pub fn incarnations(&self) -> &Incarnations {
        &self.ids
    }

    /// Brings the id ledger up to date with `doc` (the scripting layer calls
    /// it before taking a snapshot; every step does it itself).
    pub fn observe_ids(&mut self, doc: &PlumDocument) {
        self.ids.observe(doc);
    }

    fn fresh_state(&mut self) -> StateId {
        let s = self.next_state;
        self.next_state += 1;
        s
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.back().map(|e| e.label)
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|e| e.label)
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Entries whose change is in memory (the others are in the journal only).
    pub fn resident_entries(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .filter(|e| e.change.is_some())
            .count()
    }

    /// Labels of the undo steps (oldest first) and of the redo steps (next
    /// first): the History window.
    pub fn step_labels(&self) -> (Vec<&'static str>, Vec<&'static str>) {
        (
            self.undo.iter().map(|e| e.label).collect(),
            self.redo.iter().rev().map(|e| e.label).collect(),
        )
    }

    /// Bytes held by all entries.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Entries dropped because of the limits (diagnostics).
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Identity of the current document state.
    pub fn current_state(&self) -> StateId {
        self.current
    }

    /// Whether the document equals the last successfully saved state.
    pub fn is_at_saved_state(&self) -> bool {
        self.saved == Some(self.current)
    }

    /// Records that state `state` was saved successfully (call only after the save
    /// completed; `state` is the [`History::current_state`] the save started from).
    pub fn mark_saved(&mut self, state: StateId) {
        self.saved = Some(state);
    }

    /// Forgets the saved state (e.g. a project imported from MIDI has never been
    /// saved as a project).
    pub fn clear_saved(&mut self) {
        self.saved = None;
    }

    /// Drops every entry but keeps the current state id (used when the document
    /// changes in a way history does not cover).
    pub fn clear(&mut self) {
        let changes = self
            .undo
            .drain(..)
            .chain(self.redo.drain(..))
            .filter_map(|e| e.change)
            .collect();
        release_changes(changes, self.bytes);
        self.bytes = 0;
        self.persist_base(None);
    }

    fn record(&mut self, label: &'static str, change: Change) {
        // A new edit invalidates the redo branch.
        let (mut gone, mut gone_bytes) = (Vec::new(), 0);
        for e in self.redo.drain(..) {
            self.bytes -= e.bytes;
            gone_bytes += e.bytes;
            gone.extend(e.change);
        }
        release_changes(gone, gone_bytes);
        let bytes = change.heap_bytes() + std::mem::size_of::<Entry>();
        let before = self.current;
        let after = self.fresh_state();
        self.current = after;
        self.undo.push_back(Entry {
            label,
            before,
            after,
            change: Some(change),
            bytes,
            disk: None,
        });
        self.bytes += bytes;
    }

    /// Keeps the history within its limits, oldest first: an entry that is in
    /// the journal only gives up its in-memory change (it is read back when
    /// undo reaches it); others are dropped. The newest entry always stays.
    fn enforce_limits(&mut self) {
        let cap = if self.journal.is_some() {
            PERSISTED_MAX_ENTRIES
        } else {
            self.limits.max_entries
        };
        while self.undo.len() > 1 && self.undo.len() > cap {
            self.evict_front();
        }
        let mut resident = self
            .undo
            .iter()
            .chain(self.redo.iter())
            .filter(|e| e.change.is_some())
            .count();
        let mut i = 0usize;
        while (self.bytes > self.limits.max_bytes || resident > self.limits.max_entries)
            && i + 1 < self.undo.len()
        {
            // L-S48: an entry still being written to the journal is kept
            // until it is (it can then give up its change as usual).
            let awaited = self.undo[i].disk.is_none() && self.awaits_journal(self.undo[i].after);
            let e = &mut self.undo[i];
            if e.change.is_none() || awaited {
                i += 1;
                continue;
            }
            if e.disk.is_some() {
                release_changes(e.change.take().into_iter().collect(), e.bytes);
                let small = std::mem::size_of::<Entry>();
                self.bytes -= e.bytes - small;
                e.bytes = small;
                resident -= 1;
                i += 1;
            } else {
                // Not in the journal: only the oldest entries are, so drop it.
                self.evict_front();
                resident -= 1;
                i = 0;
            }
        }
    }

    fn evict_front(&mut self) {
        if let Some(e) = self.undo.pop_front() {
            self.bytes -= e.bytes;
            release_changes(e.change.into_iter().collect(), e.bytes);
            self.evicted += 1;
            self.journal_floor = self.journal_floor.saturating_sub(1);
        }
    }

    fn over_budget(&self, needed: usize) -> bool {
        needed > self.limits.max_bytes
    }

    fn too_large(&self, needed: usize, op: Operation) -> HistoryError {
        HistoryError::TooLarge {
            needed,
            limit: self.limits.max_bytes,
            operation: Box::new(op),
        }
    }

    /// Applies `op` and records it. On error nothing changed.
    pub fn apply(
        &mut self,
        doc: &mut PlumDocument,
        op: Operation,
    ) -> Result<Outcome, HistoryError> {
        self.ids.seed(doc);
        let r = self.apply_step(doc, op);
        self.ids.observe(doc);
        r
    }

    /// Applies `op` without recording it (after the user agreed to lose undo for
    /// an edit too large for the history). The history is cleared, because older
    /// entries cannot be undone across an unrecorded change.
    pub fn apply_without_undo(
        &mut self,
        doc: &mut PlumDocument,
        op: Operation,
    ) -> Result<Outcome, HistoryError> {
        self.ids.seed(doc);
        let r = self.apply_without_undo_step(doc, op);
        self.ids.observe(doc);
        r
    }

    /// Replaces the document with `new` — the result of edits that were
    /// checked on a copy (a script transaction too large for the undo
    /// memory) — without undo, like [`History::apply_without_undo`]: the
    /// history is cleared, the id ledger observes the change.
    pub fn replace_without_undo(&mut self, doc: &mut PlumDocument, new: PlumDocument) {
        self.ids.seed(doc);
        *doc = new;
        self.clear();
        self.current = self.fresh_state();
        self.ids.observe(doc);
    }

    /// Undoes the latest entry. `Ok(None)` when there is nothing to undo.
    pub fn undo(&mut self, doc: &mut PlumDocument) -> Result<Option<Applied>, HistoryError> {
        self.ids.seed(doc);
        let r = self.undo_step(doc);
        self.ids.observe(doc);
        r
    }

    /// Redoes the latest undone entry. `Ok(None)` when there is nothing to redo.
    pub fn redo(&mut self, doc: &mut PlumDocument) -> Result<Option<Applied>, HistoryError> {
        self.ids.seed(doc);
        let r = self.redo_step(doc);
        self.ids.observe(doc);
        r
    }

    /// [`History::apply`] without the id ledger.
    fn apply_step(
        &mut self,
        doc: &mut PlumDocument,
        op: Operation,
    ) -> Result<Outcome, HistoryError> {
        let label = op.label();
        let (change, outcome) = self.perform(doc, op)?;
        if let Some(c) = change {
            self.record(label, c);
            self.persist_last(doc);
            self.enforce_limits();
        }
        Ok(outcome)
    }

    /// Applies `op` and returns what undo needs (`None`: nothing changed). On
    /// error nothing changed.
    fn perform(
        &mut self,
        doc: &mut PlumDocument,
        op: Operation,
    ) -> Result<Performed, HistoryError> {
        match op {
            Operation::Prepared { edit, label } => self.apply_prepared(doc, edit, label),
            Operation::InsertNote { track, record } => {
                let loc = editor::insert_note(doc, track, record)?;
                let t = editor::note_view(doc, track)?;
                let inserted_ranks = t.notes.selected_ranks();
                let mut recs = CompactRecords::new();
                recs.push(&record);
                Ok((
                    Some(Change::Notes(NotesPatch {
                        track: t.host(),
                        removed_ranks: RankSet::new(),
                        removed: CompactRecords::new(),
                        inserted_ranks,
                        inserted: Inserted::Records(recs),
                    })),
                    Outcome::Note(loc),
                ))
            }
            Operation::DeleteSelected { track } => {
                let t = editor::note_view(doc, track)?;
                if t.notes.selected_count() == 0 {
                    return Ok((None, Outcome::Unchanged));
                }
                let (ranks, removed) = t.notes.try_selected_with_ranks()?;
                let needed = ranks.heap_bytes() + removed.heap_bytes();
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::DeleteSelected { track }));
                }
                let host = t.host();
                let n = editor::delete_selected(doc, track)?;
                Ok((
                    Some(Change::Notes(NotesPatch {
                        track: host,
                        removed_ranks: ranks,
                        removed,
                        inserted_ranks: RankSet::new(),
                        inserted: Inserted::Records(CompactRecords::new()),
                    })),
                    Outcome::Notes(n),
                ))
            }
            Operation::Resize { track, delta } => {
                let t = editor::note_view(doc, track)?;
                if delta == 0 || t.notes.selected_count() == 0 {
                    editor::resize_selected(doc, track, delta)?;
                    return Ok((None, Outcome::Unchanged));
                }
                let ranks = t.notes.selected_ranks();
                let host = t.host();
                let n = t.notes.selected_count();
                editor::resize_selected(doc, track, delta)?;
                Ok((
                    Some(Change::Duration {
                        track: host,
                        ranks,
                        delta,
                    }),
                    Outcome::Notes(n),
                ))
            }
            Operation::SetVelocity { track, velocity } => {
                if velocity == 0 || velocity > 127 {
                    return Err(HistoryError::Edit(EditError::InvalidVelocity(velocity)));
                }
                let t = editor::note_view(doc, track)?;
                if t.notes.selected_count() == 0 {
                    return Err(EditError::NothingSelected.into());
                }
                let old = t.notes.selected_attr_bytes(NoteAttrs::velocity)?;
                if old.iter().all(|&v| v == velocity) {
                    return Ok((None, Outcome::Unchanged));
                }
                let ranks = t.notes.selected_ranks();
                let needed = old.capacity() + ranks.heap_bytes();
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::SetVelocity { track, velocity }));
                }
                let host = t.host();
                let n = old.len();
                editor::notes_mut(doc, track)?
                    .map_selected_attrs(|a| a.with_velocity(velocity).unwrap_or(a));
                Ok((
                    Some(Change::Velocity {
                        track: host,
                        ranks,
                        old,
                        new: velocity,
                    }),
                    Outcome::Notes(n),
                ))
            }
            Operation::AddTrack => {
                let index = doc.add_track()?;
                let id = doc
                    .track(index)
                    .map(Track::id)
                    .ok_or(DocumentError::NoSuchTrack)?;
                Ok((
                    Some(Change::Track {
                        index,
                        count: 1,
                        id,
                        present_after: true,
                        shifted: Vec::new(),
                        stash: None,
                    }),
                    Outcome::Track(index),
                ))
            }
            Operation::DeleteTrack { index } => {
                if index >= doc.track_count() {
                    return Err(DocumentError::NoSuchTrack.into());
                }
                let count = doc.subtree_end(index) - index;
                if count >= doc.track_count() {
                    return Err(DocumentError::LastTrack.into());
                }
                // v0.9: clips on or of the removed tracks leave first, in the
                // same undo step, so undo restores them (§050).
                let ids: Vec<TrackId> = doc.tracks()[index..index + count]
                    .iter()
                    .map(Track::id)
                    .collect();
                let metadata = doc.project().metadata.removing_tracks(&ids);
                if let Some(table) = &metadata {
                    let needed = doc
                        .project()
                        .metadata
                        .heap_bytes()
                        .saturating_add(table.heap_bytes())
                        .saturating_add(64);
                    if self.over_budget(needed) {
                        return Err(self.too_large(needed, Operation::DeleteTrack { index }));
                    }
                }
                let clips = purge_clips(doc, &ids)?;
                let (tracks, shifted) = match doc.take_tracks(index, count) {
                    Ok(v) => v,
                    Err(e) => {
                        if let Some(mut c) = clips {
                            let _ = move_clips(doc, &c.new, &c.old, &mut c.stash);
                        }
                        return Err(e.into());
                    }
                };
                let id = tracks[0].id();
                // Ownership of the tracks moves into the history: nothing is
                // copied, and no memory beyond what the document already used is
                // needed.
                let track = Change::Track {
                    index,
                    count,
                    id,
                    present_after: false,
                    shifted,
                    stash: Some(tracks),
                };
                let change = match clips {
                    Some(c) => Change::Group(vec![Change::Clips(c), track]),
                    None => track,
                };
                let change = if let Some(new) = metadata {
                    let old = Box::new(std::mem::replace(
                        &mut doc.project_mut().metadata,
                        new.clone(),
                    ));
                    Change::Group(vec![
                        change,
                        Change::Metadata {
                            old,
                            new: Box::new(new),
                        },
                    ])
                } else {
                    change
                };
                Ok((
                    Some(change),
                    Outcome::Track(index.min(doc.track_count() - 1)),
                ))
            }
            Operation::MoveTracks { from, to, parent } => {
                let root = doc
                    .track(from)
                    .map(Track::id)
                    .ok_or(DocumentError::NoSuchTrack)?;
                let old_parent = doc.move_tracks(from, to, parent)?;
                if from == to && old_parent == parent {
                    return Ok((None, Outcome::Unchanged));
                }
                Ok((
                    Some(Change::Move {
                        from,
                        to,
                        root,
                        old_parent,
                        new_parent: parent,
                    }),
                    Outcome::Track(to),
                ))
            }
            Operation::AddSubtracks { parent, count } => {
                let (at, block) = doc.add_subtracks(parent, count)?;
                let id = block[0].id();
                doc.insert_tracks(at, block, &[])?;
                Ok((
                    Some(Change::Track {
                        index: at,
                        count,
                        id,
                        present_after: true,
                        shifted: Vec::new(),
                        stash: None,
                    }),
                    Outcome::Track(at),
                ))
            }
            Operation::InsertTracks { index, tracks, .. } => {
                let count = tracks.len();
                let Some(id) = tracks.first().map(Track::id) else {
                    return Ok((None, Outcome::Unchanged));
                };
                doc.insert_tracks(index, tracks, &[])?;
                Ok((
                    Some(Change::Track {
                        index,
                        count,
                        id,
                        present_after: true,
                        shifted: Vec::new(),
                        stash: None,
                    }),
                    Outcome::Track(index),
                ))
            }
            Operation::AddSubchannels { track, subs } => {
                let t = doc.track_mut(track).ok_or(EditError::NoSuchTrack)?;
                let id = t.id();
                t.subchannels_mut()
                    .insert_all(&subs)
                    .map_err(EditError::Invalid)?;
                Ok((
                    Some(Change::Subs {
                        track: id,
                        added: subs,
                        removed: Vec::new(),
                    }),
                    Outcome::Done,
                ))
            }
            Operation::EditSubchannel {
                track,
                sub,
                move_notes,
            } => {
                let t = doc.track(track).ok_or(EditError::NoSuchTrack)?;
                let old = t.subchannels().get(sub.id).cloned().ok_or_else(|| {
                    EditError::Invalid("That subchannel no longer exists.".into())
                })?;
                sub.validate().map_err(EditError::Invalid)?;
                let id = t.id();
                let mut changes = Vec::new();
                if move_notes {
                    let channel = sub.midi_channel;
                    let tm = doc.track_mut(track).ok_or(EditError::NoSuchTrack)?;
                    if tm.notes.select_sub(sub.id) > 0 {
                        let old_ch = tm.notes.selected_attr_bytes(NoteAttrs::channel)?;
                        if old_ch.iter().any(|&c| c != channel) {
                            let ranks = tm.notes.selected_ranks();
                            tm.notes
                                .map_selected_attrs(|a| a.with_channel(channel).unwrap_or(a));
                            changes.push(Change::Channel {
                                track: ContentRef::Track(id),
                                ranks,
                                old: old_ch,
                                new: channel,
                            });
                        }
                    }
                }
                if old != sub {
                    let tm = doc.track_mut(track).ok_or(EditError::NoSuchTrack)?;
                    tm.subchannels_mut()
                        .replace(sub.clone())
                        .map_err(EditError::Invalid)?;
                    changes.push(Change::Subs {
                        track: id,
                        added: vec![sub],
                        removed: vec![old],
                    });
                }
                if changes.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                Ok((Some(Change::Group(changes)), Outcome::Done))
            }
            Operation::DeleteSubchannel { track, id: sub } => {
                let t = doc.track(track).ok_or(EditError::NoSuchTrack)?;
                if !t.subchannels().contains(sub) {
                    return Err(
                        EditError::Invalid("That subchannel no longer exists.".into()).into(),
                    );
                }
                if t.subchannels().len() < 2 {
                    return Err(EditError::Invalid(
                        "A track keeps at least one subchannel, so its last subchannel cannot be deleted."
                            .into(),
                    )
                    .into());
                }
                let id = t.id();
                let mut changes = Vec::new();
                let tm = doc.track_mut(track).ok_or(EditError::NoSuchTrack)?;
                if tm.notes.select_sub(sub) > 0 {
                    let (ranks, removed) = tm.notes.try_selected_with_ranks()?;
                    let needed = ranks.heap_bytes() + removed.heap_bytes();
                    if self.over_budget(needed) {
                        return Err(
                            self.too_large(needed, Operation::DeleteSubchannel { track, id: sub })
                        );
                    }
                    editor::delete_selected(doc, track)?;
                    changes.push(Change::Notes(NotesPatch {
                        track: ContentRef::Track(id),
                        removed_ranks: ranks,
                        removed,
                        inserted_ranks: RankSet::new(),
                        inserted: Inserted::Records(CompactRecords::new()),
                    }));
                }
                let tm = doc.track_mut(track).ok_or(EditError::NoSuchTrack)?;
                let removed = tm
                    .subchannels_mut()
                    .remove_all(&[sub])
                    .map_err(EditError::Invalid)?;
                changes.push(Change::Subs {
                    track: id,
                    added: Vec::new(),
                    removed,
                });
                Ok((Some(Change::Group(changes)), Outcome::Done))
            }
            Operation::SetSubchannel {
                track,
                sub,
                set_channel,
            } => {
                let t = editor::note_view(doc, track)?;
                let target = t.subchannels().get(sub).cloned().ok_or_else(|| {
                    EditError::Invalid("That subchannel no longer exists.".into())
                })?;
                if t.notes.selected_count() == 0 {
                    return Err(EditError::NothingSelected.into());
                }
                let host = t.host();
                let old = t.notes.selected_subs()?;
                let ranks = t.notes.selected_ranks();
                let mut changes = Vec::new();
                if old.iter().any(|&s| s != sub.0) {
                    editor::notes_mut(doc, track)?.map_selected_subs(|_| sub.0);
                    changes.push(Change::NoteSubs {
                        track: host,
                        ranks: ranks.clone(),
                        old,
                        new: sub.0,
                    });
                }
                if set_channel {
                    let notes = editor::notes_mut(doc, track)?;
                    let old_ch = notes.selected_attr_bytes(NoteAttrs::channel)?;
                    let ch = target.midi_channel;
                    if old_ch.iter().any(|&c| c != ch) {
                        notes.map_selected_attrs(|a| a.with_channel(ch).unwrap_or(a));
                        changes.push(Change::Channel {
                            track: host,
                            ranks,
                            old: old_ch,
                            new: ch,
                        });
                    }
                }
                if changes.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                let n = editor::note_view(doc, track).map_or(0, |t| t.notes.selected_count());
                Ok((Some(Change::Group(changes)), Outcome::Notes(n)))
            }
            Operation::SetDuration { track, duration } => {
                if duration == 0 {
                    return Err(EditError::DurationTooShort.into());
                }
                let t = editor::note_view(doc, track)?;
                if t.notes.selected_count() == 0 {
                    return Err(EditError::NothingSelected.into());
                }
                let old = t.notes.selected_durations_encoded()?;
                let ranks = t.notes.selected_ranks();
                let needed = old.capacity() + ranks.heap_bytes();
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::SetDuration { track, duration }));
                }
                let host = t.host();
                let n = t.notes.selected_count();
                if !editor::notes_mut(doc, track)?.set_selected_durations(duration)? {
                    return Ok((None, Outcome::Unchanged));
                }
                Ok((
                    Some(Change::Durations {
                        track: host,
                        ranks,
                        old,
                        new: duration,
                    }),
                    Outcome::Notes(n),
                ))
            }
            Operation::EraseRecords { track, records } => {
                let ranks = ranks_of_records(doc, track, &records)?;
                self.perform(doc, Operation::EraseNotes { track, ranks })
            }
            Operation::EraseNotes { track, ranks } => {
                if ranks.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                let t = editor::note_view(doc, track)?;
                if ranks.end() > t.notes.len() {
                    return Err(EditError::Stale.into());
                }
                let needed = ranks.heap_bytes() + ranks.len().saturating_mul(9);
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::EraseNotes { track, ranks }));
                }
                let host = t.host();
                let removed = editor::notes_mut(doc, track)?
                    .remove_ranks_keeping_selection(&ranks)
                    .map_err(|_| EditError::Stale)?;
                let n = removed.len();
                Ok((
                    Some(Change::Notes(NotesPatch {
                        track: host,
                        removed_ranks: ranks,
                        removed,
                        inserted_ranks: RankSet::new(),
                        inserted: Inserted::Records(CompactRecords::new()),
                    })),
                    Outcome::Notes(n),
                ))
            }
            Operation::Conductor { edit, .. } => {
                if edit.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                doc.conductor_mut().apply(&edit)?;
                Ok((Some(Change::Conductor(edit)), Outcome::Done))
            }
            Operation::Events { edit, label } => {
                if edit.patch.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                let needed = edit.patch.heap_bytes() + edit.append.len();
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::Events { edit, label }));
                }
                if !doc.apply_events_edit(&edit) {
                    return Err(EditError::Stale.into());
                }
                let n = edit.patch.inserted().len().max(edit.patch.removed().len());
                Ok((
                    Some(Change::Events {
                        track: edit.host,
                        patch: edit.patch,
                    }),
                    Outcome::Notes(n),
                ))
            }
            Operation::TrackProps {
                track, props, end, ..
            } => {
                props
                    .validate(doc.project().keys)
                    .map_err(|m| HistoryError::Edit(EditError::TrackProperties(m)))?;
                let t = doc.track_by_id_mut(track).ok_or(EditError::Stale)?;
                let old = (t.props().clone(), t.end_of_track_tick);
                if old.0 == props && old.1 == end {
                    return Ok((None, Outcome::Unchanged));
                }
                t.set_props(props.clone(), end);
                Ok((
                    Some(Change::Props {
                        track,
                        old: Box::new(old),
                        new: Box::new((props, end)),
                    }),
                    Outcome::Done,
                ))
            }
            Operation::SetVelocities {
                track,
                ranks,
                values,
            } => {
                if ranks.len() != values.len() {
                    return Err(EditError::Stale.into());
                }
                if let Some(&v) = values.iter().find(|&&v| v == 0 || v > 127) {
                    return Err(HistoryError::Edit(EditError::InvalidVelocity(v)));
                }
                if ranks.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                let t = editor::note_view(doc, track)?;
                if ranks.end() > t.notes.len() {
                    return Err(EditError::Stale.into());
                }
                let needed = ranks.heap_bytes() + values.len() * 2;
                if self.over_budget(needed) {
                    return Err(self.too_large(
                        needed,
                        Operation::SetVelocities {
                            track,
                            ranks,
                            values,
                        },
                    ));
                }
                let host = t.host();
                let notes = editor::notes_mut(doc, track)?;
                notes.select_ranks(&ranks).map_err(|_| EditError::Stale)?;
                let old = notes.selected_attr_bytes(NoteAttrs::velocity)?;
                if old == values {
                    return Ok((None, Outcome::Unchanged));
                }
                notes.map_selected_attrs_indexed(|k, a| a.with_velocity(values[k]).unwrap_or(a));
                let n = values.len();
                Ok((
                    Some(Change::Velocities {
                        track: host,
                        ranks,
                        old,
                        new: values,
                    }),
                    Outcome::Notes(n),
                ))
            }
            Operation::SetChannel { track, channel } => {
                if channel > 15 {
                    return Err(HistoryError::Edit(EditError::InvalidChannel(channel)));
                }
                let t = editor::note_view(doc, track)?;
                if t.notes.selected_count() == 0 {
                    return Ok((None, Outcome::Unchanged));
                }
                let old = t.notes.selected_attr_bytes(NoteAttrs::channel)?;
                if old.iter().all(|&c| c == channel) {
                    return Ok((None, Outcome::Unchanged));
                }
                let ranks = t.notes.selected_ranks();
                let needed = old.capacity() + ranks.heap_bytes();
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::SetChannel { track, channel }));
                }
                let host = t.host();
                let n = old.len();
                editor::notes_mut(doc, track)?
                    .map_selected_attrs(|a| a.with_channel(channel).unwrap_or(a));
                Ok((
                    Some(Change::Channel {
                        track: host,
                        ranks,
                        old,
                        new: channel,
                    }),
                    Outcome::Notes(n),
                ))
            }
            Operation::Group { ops, .. } => {
                let mut changes: Vec<Change> = Vec::with_capacity(ops.len());
                let mut outcome = Outcome::Unchanged;
                for op in ops {
                    match self.perform(doc, op) {
                        Ok((c, o)) => {
                            if let Some(c) = c {
                                changes.push(c);
                            }
                            if o != Outcome::Unchanged {
                                outcome = o;
                            }
                        }
                        Err(e) => {
                            // All or nothing: undo the parts already applied.
                            for mut c in changes.into_iter().rev() {
                                let _ = revert(doc, &mut c);
                            }
                            return Err(match e {
                                HistoryError::TooLarge { needed, limit, .. } => {
                                    HistoryError::Edit(EditError::UndoTooLarge { needed, limit })
                                }
                                other => other,
                            });
                        }
                    }
                }
                if changes.is_empty() {
                    Ok((None, Outcome::Unchanged))
                } else {
                    Ok((Some(Change::Group(changes)), outcome))
                }
            }
            Operation::SetName(name) => {
                let old = doc.project().name.clone();
                if old == name {
                    return Ok((None, Outcome::Unchanged));
                }
                doc.project_mut().name = name.clone();
                Ok((Some(Change::Name { old, new: name }), Outcome::Done))
            }
            Operation::SetKeyRange { keys } => {
                let old = doc.project().keys;
                if old == keys {
                    return Ok((None, Outcome::Unchanged));
                }
                set_key_range(doc, keys)?;
                Ok((Some(Change::Keys { old, new: keys }), Outcome::Done))
            }
            Operation::SetMetadata { table, label } => {
                table
                    .validate_targets(doc)
                    .map_err(|m| HistoryError::Edit(EditError::Invalid(m)))?;
                table
                    .validate()
                    .map_err(|m| HistoryError::Edit(EditError::Invalid(m)))?;
                if doc.project().metadata == *table {
                    return Ok((None, Outcome::Unchanged));
                }
                let needed = doc
                    .project()
                    .metadata
                    .heap_bytes()
                    .saturating_add(table.heap_bytes())
                    .saturating_add(64);
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::SetMetadata { table, label }));
                }
                let old = Box::new(std::mem::replace(
                    &mut doc.project_mut().metadata,
                    (*table).clone(),
                ));
                Ok((Some(Change::Metadata { old, new: table }), Outcome::Done))
            }
            Operation::SetGroups { table, label } => {
                table.validate().map_err(|m| {
                    HistoryError::Edit(EditError::Invalid(format!(
                        "The note groups are invalid ({m}). Nothing was changed."
                    )))
                })?;
                if doc.project().groups == *table {
                    return Ok((None, Outcome::Unchanged));
                }
                let metadata = doc.project().metadata.removing_groups(&table);
                if let Some(new) = &metadata {
                    let needed = doc
                        .project()
                        .metadata
                        .heap_bytes()
                        .saturating_add(new.heap_bytes())
                        .saturating_add(64);
                    if self.over_budget(needed) {
                        return Err(self.too_large(needed, Operation::SetGroups { table, label }));
                    }
                }
                let old = Box::new(std::mem::replace(
                    &mut doc.project_mut().groups,
                    (*table).clone(),
                ));
                let group = Change::Groups { old, new: table };
                let change = if let Some(new) = metadata {
                    let old = Box::new(std::mem::replace(
                        &mut doc.project_mut().metadata,
                        new.clone(),
                    ));
                    Change::Group(vec![
                        group,
                        Change::Metadata {
                            old,
                            new: Box::new(new),
                        },
                    ])
                } else {
                    group
                };
                Ok((Some(change), Outcome::Done))
            }
            Operation::SetFolders { folders, .. } => {
                folders.validate().map_err(|m| {
                    HistoryError::Edit(EditError::Invalid(format!(
                        "The track folders are invalid ({m}). Nothing was changed."
                    )))
                })?;
                if doc.project().folders == *folders {
                    return Ok((None, Outcome::Unchanged));
                }
                let old = Box::new(std::mem::replace(
                    &mut doc.project_mut().folders,
                    (*folders).clone(),
                ));
                doc.touch_layout();
                Ok((Some(Change::Folders { old, new: folders }), Outcome::Done))
            }
            Operation::SetClips {
                layout, contents, ..
            } => {
                let old = doc.clips().layout();
                if same_layout(&old, &layout) && contents.is_empty() {
                    return Ok((None, Outcome::Unchanged));
                }
                let mut stash = set_clips(doc, &old, &layout, contents)?;
                stash.shrink_to_fit();
                Ok((
                    Some(Change::Clips(Box::new(ClipsChange {
                        old,
                        new: *layout,
                        stash,
                    }))),
                    Outcome::Done,
                ))
            }
            Operation::SetTimebase {
                mut timebase,
                label,
            } => {
                let need = timebase.heap_bytes();
                if self.over_budget(need) {
                    return Err(self.too_large(need, Operation::SetTimebase { timebase, label }));
                }
                crate::editor::ppqn::swap_timebase(doc, &mut timebase).map_err(|m| {
                    HistoryError::Edit(EditError::Invalid(format!(
                        "The PPQN change no longer fits the project ({m}). Nothing was changed."
                    )))
                })?;
                Ok((Some(Change::Timebase(timebase)), Outcome::Done))
            }
        }
    }

    fn apply_prepared(
        &mut self,
        doc: &mut PlumDocument,
        edit: PreparedEdit,
        label: &'static str,
    ) -> Result<Performed, HistoryError> {
        match edit {
            PreparedEdit::Identity => Ok((None, Outcome::Unchanged)),
            PreparedEdit::RemapSelectedPitch {
                host,
                base_revision,
                table,
                notes,
            } => {
                let store = doc.notes_of(host).ok_or(EditError::Stale)?;
                if store.revision() != base_revision {
                    return Err(EditError::Stale.into());
                }
                let old = store.selected_attr_bytes(NoteAttrs::pitch)?;
                let ranks = store.selected_ranks();
                let needed = old.capacity() + ranks.heap_bytes();
                if self.over_budget(needed) {
                    let edit = PreparedEdit::RemapSelectedPitch {
                        host,
                        base_revision,
                        table,
                        notes,
                    };
                    return Err(self.too_large(needed, Operation::Prepared { edit, label }));
                }
                editor::commit(
                    doc,
                    PreparedEdit::RemapSelectedPitch {
                        host,
                        base_revision,
                        table: table.clone(),
                        notes,
                    },
                )?;
                Ok((
                    Some(Change::Pitch {
                        track: host,
                        ranks,
                        old,
                        table,
                    }),
                    Outcome::Notes(notes),
                ))
            }
            PreparedEdit::ReplaceSelected {
                host,
                base_revision,
                records,
                shift,
            } => {
                let store = doc.notes_of(host).ok_or(EditError::Stale)?;
                if store.revision() != base_revision {
                    return Err(EditError::Stale.into());
                }
                let (removed_ranks, removed) = store.try_selected_with_ranks()?;
                let inserted = match shift {
                    Some(s) => Inserted::Shifted(s),
                    None => Inserted::Records(CompactRecords::try_from_sorted(&records).ok_or(
                        EditError::OutOfMemory {
                            bytes: records.len().saturating_mul(7),
                        },
                    )?),
                };
                let inserted_bytes = match &inserted {
                    Inserted::Records(r) => r.heap_bytes(),
                    Inserted::Shifted(_) => 0,
                };
                // Inserted positions are measured after the commit; estimate them
                // like the removed ones.
                let needed = removed_ranks.heap_bytes() * 2 + removed.heap_bytes() + inserted_bytes;
                let n = records.len();
                let edit = PreparedEdit::ReplaceSelected {
                    host,
                    base_revision,
                    records,
                    shift,
                };
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::Prepared { edit, label }));
                }
                editor::commit(doc, edit)?;
                let inserted_ranks = doc.notes_of(host).ok_or(EditError::Stale)?.selected_ranks();
                Ok((
                    Some(Change::Notes(NotesPatch {
                        track: host,
                        removed_ranks,
                        removed,
                        inserted_ranks,
                        inserted,
                    })),
                    Outcome::Notes(n),
                ))
            }
            PreparedEdit::ReplaceSelectedErasing {
                host,
                base_revision,
                records,
                erase,
            } => {
                let store = doc.notes_of(host).ok_or(EditError::Stale)?;
                if store.revision() != base_revision {
                    return Err(EditError::Stale.into());
                }
                let (sel_ranks, sel) = store.try_selected_with_ranks()?;
                let gone = editor::erase_ranks(store, &erase)?;
                // The removed notes: the selection and the erased ones, by rank.
                let n_removed = sel.len() + gone.len();
                let mut removed =
                    CompactRecords::try_with_capacity_notes(n_removed).map_err(|_| {
                        EditError::OutOfMemory {
                            bytes: n_removed.saturating_mul(7),
                        }
                    })?;
                let mut ranks = crate::model::patch::RankSetBuilder::default();
                let mut a = sel_ranks.iter().zip(sel.iter()).peekable();
                let mut b = gone.iter().peekable();
                loop {
                    let take_a = match (a.peek(), b.peek()) {
                        (Some(x), Some(y)) => x.0 < y.0,
                        (Some(_), None) => true,
                        (None, Some(_)) => false,
                        (None, None) => break,
                    };
                    let (rank, rec) = if take_a {
                        a.next().expect("peeked")
                    } else {
                        let g = b.next().expect("peeked");
                        (g.0, g.1)
                    };
                    ranks.push(rank);
                    if !removed.push(&rec) {
                        return Err(EditError::Stale.into());
                    }
                }
                let removed_ranks = ranks.finish();
                let inserted =
                    CompactRecords::try_from_sorted(&records).ok_or(EditError::OutOfMemory {
                        bytes: records.len().saturating_mul(7),
                    })?;
                let needed =
                    removed_ranks.heap_bytes() * 2 + removed.heap_bytes() + inserted.heap_bytes();
                let n = records.len() + erase.len();
                let edit = PreparedEdit::ReplaceSelectedErasing {
                    host,
                    base_revision,
                    records,
                    erase,
                };
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::Prepared { edit, label }));
                }
                editor::commit(doc, edit)?;
                let inserted_ranks = doc.notes_of(host).ok_or(EditError::Stale)?.selected_ranks();
                Ok((
                    Some(Change::Notes(NotesPatch {
                        track: host,
                        removed_ranks,
                        removed,
                        inserted_ranks,
                        inserted: Inserted::Records(inserted),
                    })),
                    Outcome::Notes(n),
                ))
            }
            PreparedEdit::InsertSelected {
                host,
                base_revision,
                records,
                subs_track,
                new_subs,
            } => {
                let store = doc.notes_of(host).ok_or(EditError::Stale)?;
                if store.revision() != base_revision {
                    return Err(EditError::Stale.into());
                }
                let inserted =
                    CompactRecords::try_from_sorted(&records).ok_or(EditError::OutOfMemory {
                        bytes: records.len().saturating_mul(7),
                    })?;
                let needed = inserted.heap_bytes() + records.len() / 4;
                let n = records.len();
                let added = new_subs.clone();
                let edit = PreparedEdit::InsertSelected {
                    host,
                    base_revision,
                    records,
                    subs_track,
                    new_subs,
                };
                if self.over_budget(needed) {
                    return Err(self.too_large(needed, Operation::Prepared { edit, label }));
                }
                editor::commit(doc, edit)?;
                let inserted_ranks = doc.notes_of(host).ok_or(EditError::Stale)?.selected_ranks();
                let notes = Change::Notes(NotesPatch {
                    track: host,
                    removed_ranks: RankSet::new(),
                    removed: CompactRecords::new(),
                    inserted_ranks,
                    inserted: Inserted::Records(inserted),
                });
                let change = if added.is_empty() {
                    notes
                } else {
                    // Undo removes the notes first, then the subchannels.
                    Change::Group(vec![
                        Change::Subs {
                            track: subs_track,
                            added,
                            removed: Vec::new(),
                        },
                        notes,
                    ])
                };
                Ok((Some(change), Outcome::Notes(n)))
            }
        }
    }

    /// [`History::apply_without_undo`] without the id ledger.
    fn apply_without_undo_step(
        &mut self,
        doc: &mut PlumDocument,
        op: Operation,
    ) -> Result<Outcome, HistoryError> {
        let outcome = match op {
            Operation::Prepared { edit, .. } => {
                let n = edit.affected_notes();
                if edit == PreparedEdit::Identity {
                    return Ok(Outcome::Unchanged);
                }
                editor::commit(doc, edit)?;
                Outcome::Notes(n)
            }
            Operation::InsertNote { track, record } => {
                Outcome::Note(editor::insert_note(doc, track, record)?)
            }
            Operation::DeleteSelected { track } => {
                Outcome::Notes(editor::delete_selected(doc, track)?)
            }
            Operation::Resize { track, delta } => {
                editor::resize_selected(doc, track, delta)?;
                Outcome::Done
            }
            Operation::SetVelocity { track, velocity } => {
                if velocity == 0 || velocity > 127 {
                    return Err(HistoryError::Edit(EditError::InvalidVelocity(velocity)));
                }
                let notes = editor::notes_mut(doc, track)?;
                let n = notes.selected_count();
                notes.map_selected_attrs(|a| a.with_velocity(velocity).unwrap_or(a));
                Outcome::Notes(n)
            }
            Operation::SetDuration { track, duration } => {
                if duration == 0 {
                    return Err(EditError::DurationTooShort.into());
                }
                let notes = editor::notes_mut(doc, track)?;
                let n = notes.selected_count();
                notes.set_selected_durations(duration)?;
                Outcome::Notes(n)
            }
            Operation::EraseRecords { track, records } => {
                let ranks = ranks_of_records(doc, track, &records)?;
                let removed = editor::notes_mut(doc, track)?
                    .remove_ranks_keeping_selection(&ranks)
                    .map_err(|_| EditError::Stale)?;
                Outcome::Notes(removed.len())
            }
            Operation::EraseNotes { track, ranks } => {
                let removed = editor::notes_mut(doc, track)?
                    .remove_ranks_keeping_selection(&ranks)
                    .map_err(|_| EditError::Stale)?;
                Outcome::Notes(removed.len())
            }
            Operation::AddTrack => Outcome::Track(doc.add_track()?),
            Operation::DeleteTrack { index } => {
                if index >= doc.track_count() {
                    return Err(DocumentError::NoSuchTrack.into());
                }
                let ids: Vec<_> = doc.tracks()[index..doc.subtree_end(index)]
                    .iter()
                    .map(Track::id)
                    .collect();
                let metadata = doc.project().metadata.removing_tracks(&ids);
                let result = doc.delete_track(index)?;
                if let Some(table) = metadata {
                    doc.project_mut().metadata = table;
                }
                Outcome::Track(result)
            }
            Operation::MoveTracks { from, to, parent } => {
                doc.move_tracks(from, to, parent)?;
                Outcome::Track(to)
            }
            Operation::Conductor { edit, .. } => {
                doc.conductor_mut().apply(&edit)?;
                Outcome::Done
            }
            Operation::Events { edit, .. } => {
                if !doc.apply_events_edit(&edit) {
                    return Err(EditError::Stale.into());
                }
                Outcome::Done
            }
            Operation::SetName(n) => {
                doc.project_mut().name = n;
                Outcome::Done
            }
            Operation::SetKeyRange { keys } => {
                if doc.project().keys == keys {
                    return Ok(Outcome::Unchanged);
                }
                set_key_range(doc, keys)?;
                Outcome::Done
            }
            Operation::SetMetadata { table, .. } => {
                table
                    .validate_targets(doc)
                    .map_err(|m| HistoryError::Edit(EditError::Invalid(m)))?;
                table
                    .validate()
                    .map_err(|m| HistoryError::Edit(EditError::Invalid(m)))?;
                doc.project_mut().metadata = *table;
                Outcome::Done
            }
            Operation::SetGroups { table, .. } => {
                table.validate().map_err(|m| {
                    HistoryError::Edit(EditError::Invalid(format!(
                        "The note groups are invalid ({m}). Nothing was changed."
                    )))
                })?;
                let metadata = doc.project().metadata.removing_groups(&table);
                doc.project_mut().groups = *table;
                if let Some(new) = metadata {
                    doc.project_mut().metadata = new;
                }
                Outcome::Done
            }
            Operation::SetFolders { folders, .. } => {
                folders.validate().map_err(|m| {
                    HistoryError::Edit(EditError::Invalid(format!(
                        "The track folders are invalid ({m}). Nothing was changed."
                    )))
                })?;
                doc.project_mut().folders = *folders;
                doc.touch_layout();
                Outcome::Done
            }
            Operation::SetClips {
                layout, contents, ..
            } => {
                let old = doc.clips().layout();
                set_clips(doc, &old, &layout, contents)?;
                Outcome::Done
            }
            Operation::SetTimebase { mut timebase, .. } => {
                crate::editor::ppqn::swap_timebase(doc, &mut timebase).map_err(|m| {
                    HistoryError::Edit(EditError::Invalid(format!(
                        "The PPQN change no longer fits the project ({m}). Nothing was changed."
                    )))
                })?;
                Outcome::Done
            }
            op @ (Operation::TrackProps { .. }
            | Operation::SetVelocities { .. }
            | Operation::SetChannel { .. }
            | Operation::AddSubtracks { .. }
            | Operation::InsertTracks { .. }
            | Operation::AddSubchannels { .. }
            | Operation::EditSubchannel { .. }
            | Operation::DeleteSubchannel { .. }
            | Operation::SetSubchannel { .. }
            | Operation::Group { .. }) => {
                // Their undo data is small or bounded by the note count; the
                // change record is simply discarded.
                self.perform(doc, op)?.1
            }
        };
        self.clear();
        self.current = self.fresh_state();
        self.persist_base(None);
        Ok(outcome)
    }

    /// [`History::undo`] without the id ledger.
    fn undo_step(&mut self, doc: &mut PlumDocument) -> Result<Option<Applied>, HistoryError> {
        self.settle_journal(false);
        let Some(mut e) = self.undo.pop_back() else {
            return Ok(None);
        };
        if e.change.is_none() {
            let loaded = match e.disk {
                Some(at) => self.load_change(at, true),
                None => Err("the step is not in the history".into()),
            };
            match loaded {
                Ok(c) => e.change = Some(c),
                Err(msg) => {
                    self.bytes -= e.bytes;
                    self.clear();
                    return Err(HistoryError::Inconsistent(msg));
                }
            }
        }
        let change = e.change.as_mut().expect("loaded above");
        if let Err(msg) = revert(doc, change) {
            self.bytes -= e.bytes;
            self.clear();
            return Err(HistoryError::Inconsistent(msg));
        }
        // Ownership may have moved between document and history.
        let change = e.change.as_ref().expect("loaded above");
        let nb = change.heap_bytes() + std::mem::size_of::<Entry>();
        self.bytes = self.bytes - e.bytes + nb;
        e.bytes = nb;
        self.current = e.before;
        let applied = Applied {
            label: e.label,
            track: change.track(),
            tracks: change.tracks(),
            structure_changed: change.structure_changed(),
            conductor_changed: change.conductor_changed(),
            retimed: change.retimed(doc),
        };
        self.redo.push(e);
        self.persist_cursor();
        self.enforce_limits();
        Ok(Some(applied))
    }

    /// [`History::redo`] without the id ledger.
    fn redo_step(&mut self, doc: &mut PlumDocument) -> Result<Option<Applied>, HistoryError> {
        self.settle_journal(false);
        let Some(mut e) = self.redo.pop() else {
            return Ok(None);
        };
        if e.change.is_none() {
            let loaded = match e.disk {
                Some(at) => self.load_change(at, false),
                None => Err("the step is not in the history".into()),
            };
            match loaded {
                Ok(c) => e.change = Some(c),
                Err(msg) => {
                    self.bytes -= e.bytes;
                    self.clear();
                    return Err(HistoryError::Inconsistent(msg));
                }
            }
        }
        let change = e.change.as_mut().expect("loaded above");
        if let Err(msg) = reapply(doc, change) {
            self.bytes -= e.bytes;
            self.clear();
            return Err(HistoryError::Inconsistent(msg));
        }
        let change = e.change.as_ref().expect("loaded above");
        let nb = change.heap_bytes() + std::mem::size_of::<Entry>();
        self.bytes = self.bytes - e.bytes + nb;
        e.bytes = nb;
        self.current = e.after;
        let applied = Applied {
            label: e.label,
            track: change.track(),
            tracks: change.tracks(),
            structure_changed: change.structure_changed(),
            conductor_changed: change.conductor_changed(),
            retimed: change.retimed(doc),
        };
        self.undo.push_back(e);
        self.persist_cursor();
        self.enforce_limits();
        Ok(Some(applied))
    }
}

/// Moves the document from an entry's "after" state to its "before" state.
fn revert(doc: &mut PlumDocument, change: &mut Change) -> Result<(), String> {
    match change {
        Change::Notes(p) => {
            let inserted = decode_inserted(p).ok_or("stored notes are damaged")?;
            swap_notes(
                doc,
                p.track,
                (&p.inserted_ranks, &inserted),
                (&p.removed_ranks, &p.removed),
            )
        }
        Change::Pitch {
            track, ranks, old, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            if t.selected_count() != old.len() {
                return Err("note count differs".into());
            }
            t.map_selected_attrs_indexed(|k, a| a.with_pitch_wide(old[k]).unwrap_or(a));
            Ok(())
        }
        Change::Velocity {
            track, ranks, old, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            if t.selected_count() != old.len() {
                return Err("note count differs".into());
            }
            t.map_selected_attrs_indexed(|k, a| a.with_velocity(old[k]).unwrap_or(a));
            Ok(())
        }
        Change::Duration {
            track,
            ranks,
            delta,
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            t.add_selected_duration(delta.checked_neg().ok_or("duration out of range")?)
                .map_err(|e| e.to_string())
        }
        Change::Durations {
            track, ranks, old, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            t.restore_selected_durations(old).map_err(inconsistent)
        }
        Change::Conductor(edit) => doc.conductor_mut().revert(edit).map_err(|e| e.to_string()),
        Change::Events { track, patch } => {
            if doc.notes_of(*track).is_none() {
                return Err("a track is missing".into());
            }
            if !doc.patch_events_of(*track, patch, true) {
                return Err("the controller events do not match".into());
            }
            Ok(())
        }
        Change::Props { track, old, .. } => {
            let t = doc
                .track_by_id_mut(*track)
                .ok_or_else(|| "a track is missing".to_owned())?;
            t.set_props(old.0.clone(), old.1);
            Ok(())
        }
        Change::Velocities {
            track, ranks, old, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            if t.selected_count() != old.len() {
                return Err("note count differs".into());
            }
            t.map_selected_attrs_indexed(|k, a| a.with_velocity(old[k]).unwrap_or(a));
            Ok(())
        }
        Change::Channel {
            track, ranks, old, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            if t.selected_count() != old.len() {
                return Err("note count differs".into());
            }
            t.map_selected_attrs_indexed(|k, a| a.with_channel(old[k]).unwrap_or(a));
            Ok(())
        }
        Change::Group(v) => {
            for i in (0..v.len()).rev() {
                if let Err(e) = revert(doc, &mut v[i]) {
                    // Put the already-reverted parts back.
                    for c in v[i + 1..].iter_mut() {
                        let _ = reapply(doc, c);
                    }
                    return Err(e);
                }
            }
            Ok(())
        }
        Change::Track {
            index,
            count,
            present_after,
            shifted,
            stash,
            id,
        } => toggle_track(doc, *index, *count, *id, !*present_after, shifted, stash),
        Change::Move {
            from,
            to,
            root,
            old_parent,
            ..
        } => move_back(doc, *to, *from, *root, *old_parent),
        Change::Subs {
            track,
            added,
            removed,
        } => swap_subs(doc, *track, added, removed),
        Change::NoteSubs {
            track, ranks, old, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            if t.selected_count() != old.len() {
                return Err("note count differs".into());
            }
            t.map_selected_subs(|k| old[k]);
            Ok(())
        }
        Change::Name { old, .. } => {
            doc.project_mut().name = old.clone();
            Ok(())
        }
        Change::Keys { old, .. } => restore_key_range(doc, *old),
        Change::Metadata { old, .. } => {
            doc.project_mut().metadata = (**old).clone();
            Ok(())
        }
        Change::Groups { old, .. } => {
            doc.project_mut().groups = (**old).clone();
            Ok(())
        }
        Change::Folders { old, .. } => {
            doc.project_mut().folders = (**old).clone();
            doc.touch_layout();
            Ok(())
        }
        Change::Clips(c) => {
            let c = &mut **c;
            move_clips(doc, &c.new, &c.old, &mut c.stash)
        }
        Change::Timebase(t) => crate::editor::ppqn::swap_timebase(doc, t),
    }
}

/// The ranks of the notes equal to `records` in `track` (each record one
/// note, an unselected one first — [`Operation::EraseRecords`]). O(records ×
/// notes sounding at their starts).
fn ranks_of_records(
    doc: &PlumDocument,
    track: NoteTarget,
    records: &[NoteRecord],
) -> Result<RankSet, EditError> {
    let t = editor::note_view(doc, track)?;
    let store = &t.notes;
    let mut ranks: Vec<usize> = Vec::with_capacity(records.len());
    let mut taken = std::collections::HashSet::new();
    for r in records {
        let mut pick: Option<(bool, usize)> = None;
        store.query(r.start_tick, r.start_tick, |loc, v| {
            if v.start_tick != r.start_tick || v.record() != *r {
                return;
            }
            let Some(rank) = store.rank_of(loc) else {
                return;
            };
            if taken.contains(&rank) {
                return;
            }
            // An unselected note wins over a selected one; then stored order.
            match pick {
                Some((false, _)) => {}
                Some((true, _)) if v.selected => {}
                _ => pick = Some((v.selected, rank)),
            }
        });
        let (_, rank) = pick.ok_or(EditError::Stale)?;
        taken.insert(rank);
        ranks.push(rank);
    }
    ranks.sort_unstable();
    let mut b = crate::model::patch::RankSetBuilder::default();
    for r in ranks {
        b.push(r);
    }
    Ok(b.finish())
}

/// Moves the document from an entry's "before" state to its "after" state.
fn reapply(doc: &mut PlumDocument, change: &mut Change) -> Result<(), String> {
    match change {
        Change::Notes(p) => {
            let inserted = decode_inserted(p).ok_or("stored notes are damaged")?;
            swap_notes(
                doc,
                p.track,
                (&p.removed_ranks, &p.removed),
                (&p.inserted_ranks, &inserted),
            )
        }
        Change::Pitch {
            track,
            ranks,
            table,
            ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            t.map_selected_attrs(|a| {
                a.with_pitch_wide(table[usize::from(a.pitch())])
                    .unwrap_or(a)
            });
            Ok(())
        }
        Change::Velocity {
            track, ranks, new, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            let v = *new;
            t.map_selected_attrs(|a| a.with_velocity(v).unwrap_or(a));
            Ok(())
        }
        Change::Duration {
            track,
            ranks,
            delta,
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            t.add_selected_duration(*delta).map_err(|e| e.to_string())
        }
        Change::Durations {
            track, ranks, new, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            t.set_selected_durations(*new)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        Change::Conductor(edit) => doc.conductor_mut().reapply(edit).map_err(|e| e.to_string()),
        Change::Events { track, patch } => {
            if doc.notes_of(*track).is_none() {
                return Err("a track is missing".into());
            }
            if !doc.patch_events_of(*track, patch, false) {
                return Err("the controller events do not match".into());
            }
            Ok(())
        }
        Change::Props { track, new, .. } => {
            let t = doc
                .track_by_id_mut(*track)
                .ok_or_else(|| "a track is missing".to_owned())?;
            t.set_props(new.0.clone(), new.1);
            Ok(())
        }
        Change::Velocities {
            track, ranks, new, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            if t.selected_count() != new.len() {
                return Err("note count differs".into());
            }
            t.map_selected_attrs_indexed(|k, a| a.with_velocity(new[k]).unwrap_or(a));
            Ok(())
        }
        Change::Channel {
            track, ranks, new, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            let c = *new;
            t.map_selected_attrs(|a| a.with_channel(c).unwrap_or(a));
            Ok(())
        }
        Change::Group(v) => {
            for i in 0..v.len() {
                if let Err(e) = reapply(doc, &mut v[i]) {
                    for c in v[..i].iter_mut().rev() {
                        let _ = revert(doc, c);
                    }
                    return Err(e);
                }
            }
            Ok(())
        }
        Change::Track {
            index,
            count,
            present_after,
            shifted,
            stash,
            id,
        } => toggle_track(doc, *index, *count, *id, *present_after, shifted, stash),
        Change::Move {
            from,
            to,
            root,
            new_parent,
            ..
        } => move_back(doc, *from, *to, *root, *new_parent),
        Change::Subs {
            track,
            added,
            removed,
        } => swap_subs(doc, *track, removed, added),
        Change::NoteSubs {
            track, ranks, new, ..
        } => {
            let t = store_mut(doc, *track)?;
            t.select_ranks(ranks).map_err(inconsistent)?;
            let v = *new;
            t.map_selected_subs(|_| v);
            Ok(())
        }
        Change::Name { new, .. } => {
            doc.project_mut().name = new.clone();
            Ok(())
        }
        Change::Keys { new, .. } => restore_key_range(doc, *new),
        Change::Metadata { new, .. } => {
            doc.project_mut().metadata = (**new).clone();
            Ok(())
        }
        Change::Groups { new, .. } => {
            doc.project_mut().groups = (**new).clone();
            Ok(())
        }
        Change::Folders { new, .. } => {
            doc.project_mut().folders = (**new).clone();
            doc.touch_layout();
            Ok(())
        }
        Change::Clips(c) => {
            let c = &mut **c;
            move_clips(doc, &c.old, &c.new, &mut c.stash)
        }
        Change::Timebase(t) => crate::editor::ppqn::swap_timebase(doc, t),
    }
}

/// Moves the subtree whose root (`root`) is at `at` to `to` under `parent`.
fn move_back(
    doc: &mut PlumDocument,
    at: usize,
    to: usize,
    root: TrackId,
    parent: Option<TrackId>,
) -> Result<(), String> {
    if doc.track(at).map(Track::id) != Some(root) {
        return Err("the moved track is not where it was".into());
    }
    doc.move_tracks(at, to, parent)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The store of note host `c` (a track's own notes or an owned content's;
/// `docs/CLIP_PIANO_ROLL.md`).
fn store_mut(
    doc: &mut PlumDocument,
    c: ContentRef,
) -> Result<&mut crate::model::note_store::NoteStore, String> {
    doc.notes_of_mut(c).ok_or_else(|| match c {
        ContentRef::Track(_) => "a track is missing".to_owned(),
        ContentRef::Owned(_) => "a clip's content is missing".to_owned(),
    })
}

fn decode_inserted(p: &NotesPatch) -> Option<CompactRecords> {
    match &p.inserted {
        Inserted::Records(r) => Some(r.clone()),
        Inserted::Shifted(s) => {
            let mut out = CompactRecords::try_with_capacity_notes(p.removed.len()).ok()?;
            for r in p.removed.iter() {
                if !out.push(&shift_record(r, *s)?) {
                    return None;
                }
            }
            (out.len() == p.removed.len()).then_some(out)
        }
    }
}

/// Removes the notes at `take.0` (which must equal `take.1`) and inserts `put.1` at
/// `put.0`. On failure of the second step the first is rolled back.
fn swap_notes(
    doc: &mut PlumDocument,
    track: ContentRef,
    take: (&RankSet, &CompactRecords),
    put: (&RankSet, &CompactRecords),
) -> Result<(), String> {
    let store = store_mut(doc, track)?;
    if take.0.len() != take.1.len() || put.0.len() != put.1.len() {
        return Err("note count differs".into());
    }
    if take.0.end() > store.len() {
        return Err("note positions out of range".into());
    }
    let removed = store.remove_ranks(take.0).map_err(inconsistent)?;
    if removed != take.0.len() {
        return Err("note count differs".into());
    }
    if let Err(e) = store.insert_at_ranks(put.0, put.1) {
        // Put back what was removed so the document is unchanged.
        let _ = store.insert_at_ranks(take.0, take.1);
        return Err(inconsistent(e));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn toggle_track(
    doc: &mut PlumDocument,
    index: usize,
    count: usize,
    id: TrackId,
    make_present: bool,
    shifted: &mut Vec<(u32, u32)>,
    stash: &mut Option<Vec<Track>>,
) -> Result<(), String> {
    if make_present {
        let t = stash.take().ok_or("the tracks are not in the history")?;
        if t.len() != count {
            return Err("the stored tracks are damaged".into());
        }
        let s = std::mem::take(shifted);
        if let Err(e) = doc.insert_tracks(index, t, &s) {
            return Err(e.to_string());
        }
        *shifted = s;
        Ok(())
    } else {
        if doc.track(index).map(Track::id) != Some(id) {
            return Err("the track moved".into());
        }
        let (t, s) = doc.take_tracks(index, count).map_err(|e| e.to_string())?;
        *shifted = s;
        *stash = Some(t);
        Ok(())
    }
}

/// Removes the `remove` entries (by id) from a track's subchannel table, then
/// adds the `add` entries. All-or-nothing.
fn swap_subs(
    doc: &mut PlumDocument,
    track: TrackId,
    remove: &[Subchannel],
    add: &[Subchannel],
) -> Result<(), String> {
    let t = doc
        .track_by_id_mut(track)
        .ok_or("the track no longer exists")?;
    let next = t.subchannels().swapped(remove, add)?;
    *t.subchannels_mut() = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::ExactFactor;
    use crate::editor::{
        EditControl, prepare_flip_x, prepare_move, prepare_scale_x, prepare_scale_y,
    };
    use crate::model::note::NoteOffKind;
    use crate::model::project::Bpm;
    use crate::model::timing::Division;

    fn rec(s: u64, d: u64, p: u8) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new(p, 90, p % 16, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    fn doc_with(n: u64) -> PlumDocument {
        let mut d = PlumDocument::new_empty(Division::ppqn(96));
        let recs: Vec<NoteRecord> = (0..n)
            .map(|i| rec(i / 3 * 10, 5 + i % 7, (i % 100) as u8))
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    fn fp(d: &PlumDocument) -> u64 {
        d.state_fingerprint()
    }

    fn check(d: &PlumDocument) {
        for t in d.tracks() {
            t.notes.validate().unwrap();
        }
    }

    /// v0.8: undo / redo of a multi-track step report every changed track,
    /// so the app keeps the active track when it was one of them instead of
    /// jumping to the first changed track.
    #[test]
    fn a_multi_track_step_reports_every_track_and_keeps_a_changed_active_track() {
        let mut d = doc_with(30);
        let extra: Vec<usize> = (0..3).map(|_| d.add_track().unwrap()).collect();
        for &t in &extra {
            let recs: Vec<NoteRecord> = (0..10u64).map(|i| rec(i * 10, 5, 60)).collect();
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let untouched = d.track(extra[2]).unwrap().id();
        let changed = [0, extra[0], extra[1]];
        let ids: Vec<TrackId> = changed.iter().map(|&t| d.track(t).unwrap().id()).collect();
        for &t in &changed {
            d.track_mut(t).unwrap().notes.select_all();
        }
        let mut h = History::default();
        let ops = changed
            .iter()
            .map(|&track| Operation::SetVelocity {
                track: track.into(),
                velocity: 33,
            })
            .collect();
        h.apply(
            &mut d,
            Operation::Group {
                ops,
                label: "Velocity of three tracks",
            },
        )
        .unwrap();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        for undo in [true, false] {
            let a = if undo { h.undo(&mut d) } else { h.redo(&mut d) }
                .unwrap()
                .unwrap();
            assert_eq!(a.tracks, sorted);
            assert_eq!(a.track, Some(ids[0]));
            // The active track was changed by the step: it stays active.
            assert_eq!(a.shown_track(Some(ids[2])), Some(ids[2]));
            // Another active track: the first changed track is shown.
            assert_eq!(a.shown_track(Some(untouched)), Some(ids[0]));
            assert_eq!(a.shown_track(None), Some(ids[0]));
        }
        // A one-track step reports just that track.
        d.track_mut(extra[2]).unwrap().notes.select_all();
        h.apply(
            &mut d,
            Operation::SetVelocity {
                track: (extra[2]).into(),
                velocity: 20,
            },
        )
        .unwrap();
        let a = h.undo(&mut d).unwrap().unwrap();
        assert_eq!(a.tracks, vec![untouched]);
        assert_eq!(a.shown_track(Some(ids[0])), Some(untouched));
    }

    /// Applies `op`, then undo/redo three times, checking exact states.
    fn roundtrip(d: &mut PlumDocument, h: &mut History, op: Operation) -> Outcome {
        let before = fp(d);
        let out = h.apply(d, op).unwrap();
        check(d);
        let after = fp(d);
        if out != Outcome::Unchanged {
            assert_ne!(before, after);
        }
        for _ in 0..3 {
            h.undo(d).unwrap().unwrap();
            check(d);
            assert_eq!(fp(d), before);
            h.redo(d).unwrap().unwrap();
            check(d);
            assert_eq!(fp(d), after);
        }
        out
    }

    #[test]
    fn every_note_operation_round_trips() {
        let mut d = doc_with(20_000);
        let mut h = History::default();
        h.reset(true);
        // Draw one note.
        roundtrip(
            &mut d,
            &mut h,
            Operation::InsertNote {
                track: 0.into(),
                record: rec(15, 3, 64),
            },
        );
        // Select a scattered set and move it (tick + pitch).
        let sel = |d: &mut PlumDocument| {
            let s = &mut d.track_mut(0).unwrap().notes;
            s.clear_selection();
            s.select_in_region(1000, 30_000, 20, 70);
        };
        sel(&mut d);
        let e = prepare_move(&d, 0, 17, 3, EditControl::default()).unwrap();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Prepared {
                edit: e,
                label: "Move Notes",
            },
        );
        sel(&mut d);
        let e = prepare_move(&d, 0, 0, -2, EditControl::default()).unwrap();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Prepared {
                edit: e,
                label: "Move Notes",
            },
        );
        sel(&mut d);
        let e = prepare_flip_x(&d, 0, EditControl::default()).unwrap();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Prepared {
                edit: e,
                label: "Flip X",
            },
        );
        sel(&mut d);
        let f = ExactFactor::parse("1.37").unwrap();
        let e = prepare_scale_x(&d, 0, &f, EditControl::default()).unwrap();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Prepared {
                edit: e,
                label: "Scale X",
            },
        );
        sel(&mut d);
        let f = ExactFactor::parse("-0.5").unwrap();
        let e = prepare_scale_y(&d, 0, &f).unwrap();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Prepared {
                edit: e,
                label: "Scale Y",
            },
        );
        sel(&mut d);
        roundtrip(
            &mut d,
            &mut h,
            Operation::Resize {
                track: 0.into(),
                delta: 4,
            },
        );
        sel(&mut d);
        roundtrip(
            &mut d,
            &mut h,
            Operation::SetVelocity {
                track: 0.into(),
                velocity: 7,
            },
        );
        // Paste (insert selected) of 10,000 notes = one entry.
        let paste: Vec<NoteRecord> = (0..10_000u64).map(|i| rec(i * 2, 1, 60)).collect();
        let e = PreparedEdit::InsertSelected {
            host: ContentRef::Track(d.track(0).unwrap().id()),
            base_revision: d.track(0).unwrap().notes.revision(),
            records: paste,
            subs_track: d.track(0).unwrap().id(),
            new_subs: Vec::new(),
        };
        let n_before = h.undo_len();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Prepared {
                edit: e,
                label: "Paste",
            },
        );
        assert_eq!(h.undo_len(), n_before + 1);
        sel(&mut d);
        roundtrip(
            &mut d,
            &mut h,
            Operation::DeleteSelected { track: 0.into() },
        );
        // Now undo everything back to the start and redo it all.
        let end = fp(&d);
        let mut states = vec![end];
        while h.can_undo() {
            h.undo(&mut d).unwrap();
            check(&d);
            states.push(fp(&d));
        }
        assert!(h.is_at_saved_state());
        assert_eq!(fp(&d), fp(&doc_with(20_000)));
        while h.can_redo() {
            h.redo(&mut d).unwrap();
        }
        assert_eq!(fp(&d), end);
        assert!(!h.is_at_saved_state());
    }

    #[test]
    fn savepoint_semantics_and_branching() {
        let mut d = doc_with(10);
        let mut h = History::default();
        h.reset(true);
        assert!(h.is_at_saved_state());
        // Save A, edit B, undo B → clean.
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: rec(1, 1, 1),
            },
        )
        .unwrap();
        assert!(!h.is_at_saved_state());
        h.undo(&mut d).unwrap();
        assert!(h.is_at_saved_state());
        // Save A, edit B, save B, undo → dirty.
        h.redo(&mut d).unwrap();
        h.mark_saved(h.current_state());
        assert!(h.is_at_saved_state());
        h.undo(&mut d).unwrap();
        assert!(!h.is_at_saved_state());
        h.redo(&mut d).unwrap();
        assert!(h.is_at_saved_state());
        // Create A, create B, undo B, create C → redo B no longer available.
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: rec(2, 1, 2),
            },
        )
        .unwrap();
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: rec(3, 1, 3),
            },
        )
        .unwrap();
        h.undo(&mut d).unwrap();
        assert!(h.can_redo());
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: 0.into(),
                record: rec(4, 1, 4),
            },
        )
        .unwrap();
        assert!(!h.can_redo());
        assert_eq!(h.redo(&mut d).unwrap(), None);
        // The saved state (B-branch) stays distinguishable after branching.
        while h.can_undo() {
            h.undo(&mut d).unwrap();
        }
        assert!(!h.is_at_saved_state());
    }

    #[test]
    fn track_delete_transfers_ownership() {
        let mut d = doc_with(5_000);
        let mut h = History::default();
        h.reset(true);
        let i = match h.apply(&mut d, Operation::AddTrack).unwrap() {
            Outcome::Track(i) => i,
            o => panic!("{o:?}"),
        };
        d.track_mut(i)
            .unwrap()
            .notes
            .insert(rec(0, 1, 60), false)
            .unwrap();
        // Direct mutation above is not in history; record from a clean point.
        h.clear();
        let before = fp(&d);
        let ids: Vec<TrackId> = d.tracks().iter().map(Track::id).collect();
        h.apply(&mut d, Operation::DeleteTrack { index: 0 })
            .unwrap();
        assert_eq!(d.track_count(), 1);
        assert!(
            h.bytes() > 5_000 * 20,
            "the deleted track is held by the history"
        );
        h.undo(&mut d).unwrap();
        assert_eq!(fp(&d), before);
        assert_eq!(d.tracks().iter().map(Track::id).collect::<Vec<_>>(), ids);
        h.redo(&mut d).unwrap();
        assert_eq!(d.track_count(), 1);
        h.undo(&mut d).unwrap();
        assert_eq!(fp(&d), before);
        // Out-of-range and last-track deletions are refused.
        assert!(matches!(
            h.apply(&mut d, Operation::DeleteTrack { index: 5 }),
            Err(HistoryError::Document(DocumentError::NoSuchTrack))
        ));
        let mut one = PlumDocument::new_empty(Division::ppqn(96));
        assert!(matches!(
            h.apply(&mut one, Operation::DeleteTrack { index: 0 }),
            Err(HistoryError::Document(DocumentError::LastTrack))
        ));
    }

    #[test]
    fn budget_eviction_and_too_large() {
        let mut d = doc_with(50_000);
        let mut h = History::new(HistoryLimits {
            max_bytes: 200_000,
            max_entries: 5,
        });
        h.reset(false);
        for i in 0..10u64 {
            h.apply(
                &mut d,
                Operation::InsertNote {
                    track: 0.into(),
                    record: rec(i, 1, 5),
                },
            )
            .unwrap();
        }
        assert_eq!(h.undo_len(), 5);
        assert!(h.evicted() >= 5);
        // Remaining entries still undo correctly after eviction.
        while h.can_undo() {
            h.undo(&mut d).unwrap();
        }
        check(&d);
        assert_eq!(d.track(0).unwrap().notes.len(), 50_005);
        // Deleting 50,000 notes needs more than 200 KB: refused, nothing changed.
        d.track_mut(0).unwrap().notes.select_all();
        let before = fp(&d);
        let err = h
            .apply(&mut d, Operation::DeleteSelected { track: 0.into() })
            .unwrap_err();
        let HistoryError::TooLarge {
            operation, needed, ..
        } = err
        else {
            panic!("expected TooLarge");
        };
        assert!(needed > 200_000);
        assert_eq!(fp(&d), before);
        // The user agrees: apply without undo; history is cleared.
        h.apply_without_undo(&mut d, *operation).unwrap();
        assert_eq!(d.track(0).unwrap().notes.len(), 0);
        assert!(!h.can_undo() && !h.can_redo());
    }

    #[test]
    fn tempo_name_and_velocity_validation() {
        let mut d = PlumDocument::new_project("A".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        h.reset(false);
        let b = Bpm::parse("174.5").unwrap();
        let edit = d
            .conductor()
            .prepare_set_start_tempo(crate::model::TempoValue::Bpm(b))
            .unwrap();
        roundtrip(
            &mut d,
            &mut h,
            Operation::Conductor {
                edit,
                label: "Change Tempo",
            },
        );
        assert_eq!(
            d.conductor().start_tempo(),
            crate::model::TempoValue::Bpm(b)
        );
        roundtrip(&mut d, &mut h, Operation::SetName("B".into()));
        assert_eq!(d.project().name, "B");
        assert_eq!(
            h.apply(&mut d, Operation::SetName("B".into())).unwrap(),
            Outcome::Unchanged
        );
        d.track_mut(0)
            .unwrap()
            .notes
            .insert(rec(0, 1, 1), true)
            .unwrap();
        for bad in [0u8, 128] {
            assert!(
                h.apply(
                    &mut d,
                    Operation::SetVelocity {
                        track: 0.into(),
                        velocity: bad
                    }
                )
                .is_err()
            );
        }
        assert_eq!(h.undo_label(), Some("Rename Project"));
    }
}
