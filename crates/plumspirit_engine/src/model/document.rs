//! The authoritative in-memory MIDI document.
//!
//! `PlumDocument` is the single source of truth: the UI renders it, the editor mutates
//! it, and the exporter serializes it. Nothing else (render caches, dialog values,
//! the original file) is authoritative.
//!
//! A document always holds exactly one [`Conductor`] (tempo, time signatures,
//! markers) beside its tracks. The conductor is not an element of the track list,
//! so no track operation (delete, reorder, undo) can remove or duplicate it, and
//! track indices and ids are unaffected by it.

use super::clip::ClipTable;
use super::conductor::{AdoptReport, Conductor, ConductorScope};
use super::events::RawKind;
use super::project::{Bpm, ProjectInfo};
use super::tempo::TempoValue;
use super::timing::{Division, TimingKind};
use super::track::{Track, TrackId};
use std::fmt;

/// Standard MIDI File format word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmfFormat {
    /// Format 0: a single multi-channel track.
    SingleTrack,
    /// Format 1: simultaneous tracks sharing one tempo map.
    Parallel,
    /// Format 2: independent sequences.
    Sequential,
}

impl SmfFormat {
    pub fn from_word(w: u16) -> Option<Self> {
        match w {
            0 => Some(SmfFormat::SingleTrack),
            1 => Some(SmfFormat::Parallel),
            2 => Some(SmfFormat::Sequential),
            _ => None,
        }
    }

    pub fn word(self) -> u16 {
        match self {
            SmfFormat::SingleTrack => 0,
            SmfFormat::Parallel => 1,
            SmfFormat::Sequential => 2,
        }
    }
}

/// A non-`MTrk` chunk found in the source file (e.g. vendor chunks). Preserved and
/// written back at the same position relative to the track chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraChunk {
    pub chunk_type: [u8; 4],
    pub data: Vec<u8>,
    /// Number of `MTrk` chunks that preceded this chunk in the source file.
    pub after_track_chunks: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentError {
    /// PlumSpirit always keeps at least one track.
    LastTrack,
    NoSuchTrack,
    /// Standard MIDI Files store the track count in 16 bits.
    TooManyTracks,
    /// A track tree that is not a valid hierarchy (details for the log).
    BadHierarchy(String),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocumentError::LastTrack => write!(
                f,
                "A MIDI file needs at least one track, so the last track cannot be deleted."
            ),
            DocumentError::NoSuchTrack => write!(f, "That track no longer exists."),
            DocumentError::TooManyTracks => write!(
                f,
                "A project can hold at most 65,535 tracks and subtracks (the limit of a MIDI file)."
            ),
            DocumentError::BadHierarchy(d) => write!(f, "The track hierarchy is invalid: {d}."),
        }
    }
}

impl std::error::Error for DocumentError {}

#[derive(Clone, Debug)]
pub struct PlumDocument {
    format: SmfFormat,
    division: Division,
    tracks: Vec<Track>,
    extra_chunks: Vec<ExtraChunk>,
    next_track_id: u32,
    /// Incremented when tracks are added/removed (note edits are tracked per store).
    structure_revision: u64,
    /// Bumped when the track list's organization (Track Folders) changes.
    layout_revision: u64,
    /// Name, SoundFont and Onion Skin settings (native `.lup` data).
    project: ProjectInfo,
    /// Tempo map, time signatures and markers of the whole project.
    conductor: Conductor,
    /// v0.9 clips: owned contents and the placements that are not a track's
    /// implicit identity (empty for a v0.8 project; `model/clip.rs`).
    clips: ClipTable,
    /// v0.9 (V09-S08): the song end through the placements, memoized.
    end_memo: EndMemo,
}

/// The song end through the placements (`content_end_tick` without the
/// Conductor), keyed by everything it reads: the placements and every
/// content's revision (`analysis_revision`) and the tracks' explicit ends.
/// The Track List and the Piano Roll read the song length every frame, and
/// with thousands of windowed clips one pass costs milliseconds (V09-S08).
/// A clone starts empty (it computes its own).
#[derive(Default)]
struct EndMemo(std::sync::Mutex<Option<(u64, u64)>>);

impl Clone for EndMemo {
    fn clone(&self) -> Self {
        EndMemo::default()
    }
}

impl fmt::Debug for EndMemo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EndMemo")
    }
}

/// A placement with the note and event stores it shows
/// ([`PlumDocument::placed_contents`]).
#[derive(Clone, Debug)]
pub struct PlacedContent<'a> {
    pub placement: super::clip::ClipPlacement,
    pub notes: &'a super::note_store::NoteStore,
    pub events: &'a super::events::EventStore,
}

impl PlacedContent<'_> {
    /// Content ticks `[lo, hi)` whose note starts / events the clip plays.
    pub fn window(&self) -> (u64, u64) {
        self.placement.content_window()
    }

    /// Song tick minus content tick (`start − offset`).
    pub fn shift(&self) -> i128 {
        i128::from(self.placement.start) - i128::from(self.placement.offset)
    }

    /// Exact number of content notes the clip plays (their start lies in the
    /// window — the start decides, as everywhere). O(1) for an open window
    /// from tick 0, else two binary searches and whole chunk lengths.
    pub fn note_count(&self) -> usize {
        let (lo, hi) = self.window();
        if hi <= lo {
            return 0;
        }
        if lo == 0 && hi == u64::MAX {
            return self.notes.len();
        }
        self.notes.count_starting_in(lo, hi - 1)
    }
}

/// The conductor scope that fits a file's format and timing.
pub fn scope_for(format: SmfFormat, division: Division) -> ConductorScope {
    match (format, division.kind()) {
        (SmfFormat::Sequential, _) => ConductorScope::Sequences,
        (_, TimingKind::Ppqn(p)) if p > 0 => ConductorScope::Timeline,
        _ => ConductorScope::MarkersOnly,
    }
}

/// Extra-chunk index shifts `(old, new)` recorded when tracks are taken out.
pub(crate) type ChunkShifts = Vec<(u32, u32)>;

impl PlumDocument {
    /// An empty metrical/SMPTE document with a single empty track and the MIDI
    /// default tempo and meter (both implicit).
    pub fn new_empty(division: Division) -> Self {
        let mut d = PlumDocument {
            format: SmfFormat::Parallel,
            division,
            tracks: Vec::new(),
            extra_chunks: Vec::new(),
            next_track_id: 0,
            structure_revision: 0,
            layout_revision: 0,
            project: ProjectInfo::default(),
            conductor: Conductor::with_defaults(scope_for(SmfFormat::Parallel, division)),
            clips: ClipTable::default(),
            end_memo: EndMemo::default(),
        };
        d.tracks.push(Track::new(TrackId(0)));
        d.next_track_id = 1;
        d
    }

    /// A document holding only `tracks` (whole subtrees, parents outside the
    /// block cleared by the caller), with this document's format, timing,
    /// Conductor and note groups: the history journal stores removed tracks as
    /// the `.lup` image of such a document.
    pub(crate) fn image_of_tracks(&self, tracks: Vec<Track>) -> PlumDocument {
        let next = tracks.iter().map(|t| t.id().0 + 1).max().unwrap_or(0);
        PlumDocument {
            format: self.format,
            division: self.division,
            tracks,
            extra_chunks: Vec::new(),
            next_track_id: next,
            structure_revision: 0,
            layout_revision: 0,
            project: ProjectInfo {
                groups: self.project.groups.clone(),
                keys: self.project.keys,
                ..ProjectInfo::default()
            },
            conductor: self.conductor.clone(),
            clips: ClipTable::default(),
            end_memo: EndMemo::default(),
        }
    }

    /// The tracks of a document image (see [`PlumDocument::image_of_tracks`]).
    pub(crate) fn into_tracks(self) -> Vec<Track> {
        self.tracks
    }

    /// A new empty project (File → New Project): one empty track, the given PPQN,
    /// and a Conductor holding the exact tempo `bpm` and 4/4 at tick 0.
    pub fn new_project(name: String, bpm: Bpm, ppqn: u32) -> Self {
        let mut d = PlumDocument::new_empty(Division::ppqn(ppqn));
        d.project.name = name;
        d.conductor = Conductor::new_project(bpm);
        d
    }

    /// Assembles a document from imported parts. Track ids are assigned in order.
    /// Tempo, time-signature and marker events of every track are moved into the
    /// Conductor (see [`Conductor::adopt`]).
    pub fn from_parts(
        format: SmfFormat,
        division: Division,
        tracks: Vec<Track>,
        extra_chunks: Vec<ExtraChunk>,
    ) -> Self {
        PlumDocument::from_parts_adopting(format, division, tracks, extra_chunks).0
    }

    /// [`PlumDocument::from_parts`] that also reports what the Conductor adopted.
    pub fn from_parts_adopting(
        format: SmfFormat,
        division: Division,
        mut tracks: Vec<Track>,
        extra_chunks: Vec<ExtraChunk>,
    ) -> (Self, AdoptReport) {
        let next = tracks.iter().map(|t| t.id().0 + 1).max().unwrap_or(0);
        let (conductor, report) = Conductor::adopt(scope_for(format, division), &mut tracks);
        (
            PlumDocument {
                format,
                division,
                tracks,
                extra_chunks,
                next_track_id: next,
                structure_revision: 0,
                layout_revision: 0,
                project: ProjectInfo::default(),
                conductor,
                clips: ClipTable::default(),
                end_memo: EndMemo::default(),
            },
            report,
        )
    }

    /// Assembles a document loaded from a native project. `next_track_id` is kept
    /// so ids of deleted tracks are never reused. `conductor: None` (a project
    /// saved before the Conductor existed) adopts the tracks' tempo, meter and
    /// markers, and `legacy_tempo` (the old project-tempo field) becomes the tempo
    /// at tick 0.
    #[allow(clippy::too_many_arguments)] // one call site (the .lup reader); plain parts
    pub(crate) fn from_native_parts(
        format: SmfFormat,
        division: Division,
        tracks: Vec<Track>,
        extra_chunks: Vec<ExtraChunk>,
        next_track_id: u32,
        project: ProjectInfo,
        conductor: Option<Conductor>,
        legacy_tempo: Option<Bpm>,
    ) -> Result<(Self, Option<AdoptReport>), String> {
        let (mut d, report) = match conductor {
            Some(c) => {
                if c.scope() != scope_for(format, division) {
                    return Err("the Conductor does not match the project's timing".into());
                }
                let next = tracks.iter().map(|t| t.id().0 + 1).max().unwrap_or(0);
                (
                    PlumDocument {
                        format,
                        division,
                        tracks,
                        extra_chunks,
                        next_track_id: next,
                        structure_revision: 0,
                        layout_revision: 0,
                        project: ProjectInfo::default(),
                        conductor: c,
                        clips: ClipTable::default(),
                        end_memo: EndMemo::default(),
                    },
                    None,
                )
            }
            None => {
                let (mut d, r) =
                    PlumDocument::from_parts_adopting(format, division, tracks, extra_chunks);
                if let Some(b) = legacy_tempo
                    && d.conductor.timing_editable()
                {
                    d.conductor.replace_start_tempo(TempoValue::Bpm(b));
                }
                (d, Some(r))
            }
        };
        d.next_track_id = d.next_track_id.max(next_track_id);
        d.project = project;
        d.validate_hierarchy()?;
        Ok((d, report))
    }

    // ------------------------------------------------------------- hierarchy

    /// Checks the track tree in O(tracks) without recursion: ids are unique, every
    /// parent is an earlier track that is still "open" (tracks are stored depth
    /// first, so a subtree is contiguous and directly follows its root), which also
    /// rules out cycles and self-parents.
    pub fn validate_hierarchy(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::with_capacity(self.tracks.len());
        let mut stack: Vec<TrackId> = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if !seen.insert(t.id()) {
                return Err(format!("two tracks share id {}", t.id().0));
            }
            match t.parent() {
                None => stack.clear(),
                Some(p) => {
                    while stack.last().is_some_and(|&top| top != p) {
                        stack.pop();
                    }
                    if stack.is_empty() {
                        return Err(format!(
                            "track {} names a parent that does not directly precede its subtree",
                            i + 1
                        ));
                    }
                }
            }
            stack.push(t.id());
        }
        Ok(())
    }

    /// Depth of every track (0 = top level) and whether it has subtracks, in
    /// track order. O(tracks), iterative.
    pub fn tree_rows(&self) -> Vec<(u32, bool)> {
        let n = self.tracks.len();
        let mut out = vec![(0u32, false); n];
        let mut stack: Vec<(TrackId, usize)> = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            match t.parent() {
                None => stack.clear(),
                Some(p) => {
                    while stack.last().is_some_and(|&(top, _)| top != p) {
                        stack.pop();
                    }
                    if let Some(&(_, pi)) = stack.last() {
                        out[pi].1 = true;
                    }
                }
            }
            out[i].0 = stack.len() as u32;
            stack.push((t.id(), i));
        }
        out
    }

    /// 1-based position of every track among its siblings: top-level tracks are
    /// numbered among the top-level tracks, subtracks among their parent's
    /// children. Used for the labels of unnamed tracks, so adding subtracks never
    /// renumbers "Track 3". O(tracks).
    pub fn track_ordinals(&self) -> Vec<u32> {
        let mut top = 0u32;
        let mut children: std::collections::HashMap<TrackId, u32> =
            std::collections::HashMap::new();
        self.tracks
            .iter()
            .map(|t| match t.parent() {
                None => {
                    top += 1;
                    top
                }
                Some(p) => {
                    let c = children.entry(p).or_insert(0);
                    *c += 1;
                    *c
                }
            })
            .collect()
    }

    /// What the track list calls track `index`: its name, else "Track N" (top
    /// level) or "Subtrack N" (numbered among its siblings). O(tracks).
    pub fn track_label(&self, index: usize) -> String {
        let ordinal = self.track_ordinals().get(index).copied().unwrap_or(0);
        self.tracks
            .get(index)
            .map(|t| t.label_with(ordinal))
            .unwrap_or_default()
    }

    /// [`PlumDocument::track_label`] of every track. O(tracks).
    pub fn track_labels(&self) -> Vec<String> {
        self.tracks
            .iter()
            .zip(self.track_ordinals())
            .map(|(t, o)| t.label_with(o))
            .collect()
    }

    /// Exclusive end of the subtree rooted at `index` (`index + 1` for a track
    /// without subtracks). O(subtree size).
    pub fn subtree_end(&self, index: usize) -> usize {
        let Some(root) = self.tracks.get(index) else {
            return index;
        };
        let mut stack = vec![root.id()];
        let mut j = index + 1;
        while let Some(t) = self.tracks.get(j) {
            let Some(p) = t.parent() else { break };
            while stack.last().is_some_and(|&top| top != p) {
                stack.pop();
            }
            if stack.is_empty() {
                break;
            }
            stack.push(t.id());
            j += 1;
        }
        j
    }

    /// Indices of the direct subtracks of `index`, in order.
    pub fn children_of(&self, index: usize) -> Vec<usize> {
        let Some(id) = self.tracks.get(index).map(Track::id) else {
            return Vec::new();
        };
        (index + 1..self.subtree_end(index))
            .filter(|&j| self.tracks[j].parent() == Some(id))
            .collect()
    }

    /// Names from the top-level ancestor down to `index`, e.g.
    /// `["Strings", "Violins", "Divisi"]`. O(index).
    pub fn path_names(&self, index: usize) -> Vec<String> {
        let mut out = Vec::new();
        let ordinals = self.track_ordinals();
        let mut cur = Some(index);
        while let Some(i) = cur {
            let Some(t) = self.tracks.get(i) else { break };
            out.push(t.label_with(ordinals[i]));
            cur = t
                .parent()
                .and_then(|p| self.tracks[..i].iter().rposition(|x| x.id() == p));
        }
        out.reverse();
        out
    }

    /// Inserts a block of tracks at `index` (as whole subtrees, already in
    /// depth-first order, with parents set). Ids must be new. The hierarchy is
    /// validated afterwards and the insertion undone if it is invalid. Returns the
    /// extra-chunk shifts for [`PlumDocument::take_tracks`]'s inverse.
    pub(crate) fn insert_tracks(
        &mut self,
        index: usize,
        block: Vec<Track>,
        shifted: &[(u32, u32)],
    ) -> Result<(), DocumentError> {
        if index > self.tracks.len() {
            return Err(DocumentError::NoSuchTrack);
        }
        if self.tracks.len() + block.len() > usize::from(u16::MAX) {
            return Err(DocumentError::TooManyTracks);
        }
        {
            let existing: std::collections::HashSet<TrackId> =
                self.tracks.iter().map(Track::id).collect();
            if block.iter().any(|t| existing.contains(&t.id())) {
                return Err(DocumentError::BadHierarchy(
                    "an inserted track reuses an id".into(),
                ));
            }
        }
        let count = block.len();
        let max_id = block.iter().map(|t| t.id().0).max();
        self.tracks.splice(index..index, block);
        if let Err(e) = self.validate_hierarchy() {
            self.tracks.drain(index..index + count);
            return Err(DocumentError::BadHierarchy(e));
        }
        for &(ci, amount) in shifted {
            if let Some(c) = self.extra_chunks.get_mut(ci as usize) {
                c.after_track_chunks += amount as usize;
            }
        }
        if let Some(m) = max_id {
            self.next_track_id = self.next_track_id.max(m.saturating_add(1));
        }
        self.structure_revision += 1;
        Ok(())
    }

    /// Removes `count` tracks starting at `index` (whole subtrees; the caller
    /// passes a range from [`PlumDocument::subtree_end`]) and hands them to the
    /// caller. Refuses to remove every track or to leave subtracks without their
    /// parent. Returns the tracks and the extra-chunk shifts.
    pub(crate) fn take_tracks(
        &mut self,
        index: usize,
        count: usize,
    ) -> Result<(Vec<Track>, ChunkShifts), DocumentError> {
        let end = index
            .checked_add(count)
            .filter(|&e| e <= self.tracks.len() && count > 0)
            .ok_or(DocumentError::NoSuchTrack)?;
        if count >= self.tracks.len() {
            return Err(DocumentError::LastTrack);
        }
        // The track after the block must not be a subtrack of a removed track.
        if let Some(next) = self.tracks.get(end)
            && let Some(p) = next.parent()
            && self.tracks[index..end].iter().any(|t| t.id() == p)
        {
            return Err(DocumentError::BadHierarchy(
                "deleting part of a subtree would orphan subtracks".into(),
            ));
        }
        let taken: Vec<Track> = self.tracks.drain(index..end).collect();
        // v0.9: records of clips on or of the removed tracks go with them. The
        // history records them first (`Operation::DeleteTrack` keeps them in
        // the same undo step), so here only an unrecorded deletion purges any.
        let ids: Vec<TrackId> = taken.iter().map(Track::id).collect();
        self.clips.purge_tracks(&ids);
        let mut shifted = Vec::new();
        for (ci, c) in self.extra_chunks.iter_mut().enumerate() {
            if c.after_track_chunks > index {
                let amount = c.after_track_chunks.min(end) - index;
                c.after_track_chunks -= amount;
                shifted.push((ci as u32, amount as u32));
            }
        }
        self.structure_revision += 1;
        Ok((taken, shifted))
    }

    /// Moves the subtree rooted at `from` so that it starts at index `to` of the
    /// track list *without* the subtree, and makes `parent` the root's parent
    /// (`None` = top level). Extra (non-track) chunks keep their positions. The
    /// hierarchy is validated (no cycles, no orphans, parents directly precede
    /// their subtrees); on any error nothing changes. Returns the old parent.
    pub(crate) fn move_tracks(
        &mut self,
        from: usize,
        to: usize,
        parent: Option<TrackId>,
    ) -> Result<Option<TrackId>, DocumentError> {
        if from >= self.tracks.len() {
            return Err(DocumentError::NoSuchTrack);
        }
        let end = self.subtree_end(from);
        let count = end - from;
        if let Some(p) = parent
            && self.tracks[from..end].iter().any(|t| t.id() == p)
        {
            return Err(DocumentError::BadHierarchy(
                "a track cannot be moved into its own subtree".into(),
            ));
        }
        if to > self.tracks.len() - count {
            return Err(DocumentError::NoSuchTrack);
        }
        let old_parent = self.tracks[from].parent();
        let mut block: Vec<Track> = self.tracks.drain(from..end).collect();
        block[0].set_parent(parent);
        self.tracks.splice(to..to, block);
        if let Err(e) = self.validate_hierarchy() {
            let mut back: Vec<Track> = self.tracks.drain(to..to + count).collect();
            back[0].set_parent(old_parent);
            self.tracks.splice(from..from, back);
            return Err(DocumentError::BadHierarchy(e));
        }
        self.structure_revision += 1;
        Ok(old_parent)
    }

    /// Appends `count` new empty subtracks as the last children of `parent` and
    /// returns the index of the first. Names continue the parent's numbering
    /// ("Subtrack 1", "Subtrack 2", …). All-or-nothing.
    pub(crate) fn add_subtracks(
        &mut self,
        parent: usize,
        count: usize,
    ) -> Result<(usize, Vec<Track>), DocumentError> {
        let pid = self
            .tracks
            .get(parent)
            .map(Track::id)
            .ok_or(DocumentError::NoSuchTrack)?;
        if count == 0 || self.tracks.len().saturating_add(count) > usize::from(u16::MAX) {
            return Err(DocumentError::TooManyTracks);
        }
        let first_number = self.children_of(parent).len() + 1;
        let channel = self.tracks[parent].subchannels().first().midi_channel;
        let mut block = Vec::new();
        block
            .try_reserve_exact(count)
            .map_err(|_| DocumentError::TooManyTracks)?;
        for k in 0..count {
            let id = TrackId(self.next_track_id.wrapping_add(k as u32));
            let mut t = Track::named(id, &format!("Subtrack {}", first_number + k));
            t.set_parent(Some(pid));
            t.set_subchannels(crate::model::track::SubchannelTable::single(channel));
            block.push(t);
        }
        let at = self.subtree_end(parent);
        Ok((at, block))
    }

    /// Project name, SoundFont and Onion Skin settings.
    #[inline]
    pub fn project(&self) -> &ProjectInfo {
        &self.project
    }

    /// Mutable access for settings that are not part of the undo history (Onion
    /// Skin, SoundFont association). Name changes and every Conductor change go
    /// through [`crate::history::History`] instead.
    pub fn project_mut(&mut self) -> &mut ProjectInfo {
        &mut self.project
    }

    /// Why the project cannot switch to key range `keys` (v0.9 §058): a note
    /// above its top key, or a track whose key range / key correction needs
    /// more keys. `None` when everything fits. Growing the range always fits
    /// (O(1)); shrinking it reads every note once (O(notes)).
    pub fn key_range_conflict(&self, keys: super::project::KeyRange) -> Option<String> {
        let top = keys.max_pitch();
        if top >= self.project.keys.max_pitch() {
            return None;
        }
        for (i, t) in self.tracks.iter().enumerate() {
            if t.props().validate(keys).is_err() {
                let p = t.props();
                return Some(format!(
                    "256-Key Mode stays on: {} has key range {}–{} and key correction {:+}, \
                     which need keys above {top}. Change them in Track Properties first. \
                     Nothing was changed.",
                    self.track_label(i),
                    p.key_range.0,
                    p.key_range.1,
                    p.key_correction
                ));
            }
        }
        let (mut count, mut highest, mut first) = (0usize, 0u8, None);
        for (i, t) in self.tracks.iter().enumerate() {
            for n in t.notes.iter() {
                let p = n.attrs.pitch();
                if p > top {
                    count += 1;
                    highest = highest.max(p);
                    first.get_or_insert(i);
                }
            }
        }
        let first = first?;
        let notes = if count == 1 {
            "1 note is".to_owned()
        } else {
            format!("{count} notes are")
        };
        Some(format!(
            "256-Key Mode stays on: {notes} above key {top} (the highest is {highest}, first in {}). \
             Move or delete them first. Nothing was changed.",
            self.track_label(first)
        ))
    }

    /// Tempo map, time signatures and markers.
    #[inline]
    pub fn conductor(&self) -> &Conductor {
        &self.conductor
    }

    /// History-internal access (every change is a recorded `ConductorEdit`).
    pub(crate) fn conductor_mut(&mut self) -> &mut Conductor {
        &mut self.conductor
    }

    /// The document's clips (v0.9 §039): owned contents and placements.
    pub fn clips(&self) -> &ClipTable {
        &self.clips
    }

    /// Clip operations (V09-S07) change the table through the history; the
    /// `.lup` reader installs the table it read.
    pub(crate) fn clips_mut(&mut self) -> &mut ClipTable {
        &mut self.clips
    }

    /// True while every track shows its own content at the identity and
    /// nothing else is placed (every v0.8 project): a track's notes are then
    /// exactly what it plays, exports and counts.
    pub fn clips_are_identity(&self) -> bool {
        self.clips.is_identity()
    }

    /// Every placement on track `index` with the stores it shows (its
    /// primary first, then by start; muted ones included — readers skip
    /// them). Nothing is copied. The read path of playback, analysis, export
    /// and the roll when `!clips_are_identity()` (V09-S07).
    pub fn placed_contents(&self, index: usize) -> Vec<PlacedContent<'_>> {
        self.placements_on(index)
            .into_iter()
            .filter_map(|p| {
                let (notes, events) = match p.content {
                    super::clip::ContentRef::Track(id) => {
                        let t = self.track_by_id(id)?;
                        (&t.notes, &t.events)
                    }
                    super::clip::ContentRef::Owned(id) => {
                        let c = self.clips.content(id)?;
                        (&c.notes, &c.events)
                    }
                };
                Some(PlacedContent {
                    placement: p,
                    notes,
                    events,
                })
            })
            .collect()
    }

    /// Exact number of notes track `index` plays: every placement on it (its
    /// own primary and clips of other contents; muted clips included, like a
    /// muted track's notes) counts the content notes starting in its window.
    /// The track's own notes while the clips are the identity. v1.0-beta1
    /// B1-0095: a track that hosts a clip moved from another track counts it,
    /// a track whose clip moved away no longer counts it.
    pub fn placed_note_count(&self, index: usize) -> usize {
        if self.clips_are_identity() {
            return self.tracks.get(index).map_or(0, |t| t.notes.len());
        }
        self.placed_contents(index)
            .iter()
            .map(PlacedContent::note_count)
            .sum()
    }

    /// [`placed_note_count`](Self::placed_note_count) of every track, in one
    /// pass (entry `i` is track `i`).
    pub fn placed_note_counts(&self) -> Vec<usize> {
        if self.clips_are_identity() {
            return self.tracks.iter().map(|t| t.notes.len()).collect();
        }
        self.placed_contents_by_track()
            .iter()
            .map(|pcs| pcs.iter().map(PlacedContent::note_count).sum())
            .collect()
    }

    /// [`placed_contents`](Self::placed_contents) of every track at once, in
    /// one pass: O(tracks + placements log placements + contents) instead of
    /// O(tracks × placements) for a loop over the tracks (V09-S08: the song
    /// length is read every frame). Entry `i` equals `placed_contents(i)`.
    pub fn placed_contents_by_track(&self) -> Vec<Vec<PlacedContent<'_>>> {
        use super::clip::{ClipId, ClipPlacement, ContentRef};
        use std::collections::{HashMap, HashSet};
        let index: HashMap<TrackId, usize> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id(), i))
            .collect();
        let owned: HashMap<super::clip::ContentId, &super::clip::OwnedContent> =
            self.clips.contents().iter().map(|c| (c.id, c)).collect();
        let explicit_primaries: HashSet<TrackId> = self
            .clips
            .explicit_placements()
            .iter()
            .filter_map(|p| match p.id {
                ClipId::Primary(t) => Some(t),
                ClipId::Placed(_) => None,
            })
            .collect();
        let removed: HashSet<TrackId> = self.clips.removed_primaries().iter().copied().collect();
        let mut out: Vec<Vec<PlacedContent<'_>>> =
            (0..self.tracks.len()).map(|_| Vec::new()).collect();
        let stores = |c: ContentRef| match c {
            ContentRef::Track(id) => index.get(&id).map(|&i| {
                let t = &self.tracks[i];
                (&t.notes, &t.events)
            }),
            ContentRef::Owned(id) => owned.get(&id).map(|c| (&c.notes, &c.events)),
        };
        for (i, t) in self.tracks.iter().enumerate() {
            let id = t.id();
            if !explicit_primaries.contains(&id) && !removed.contains(&id) {
                out[i].push(PlacedContent {
                    placement: ClipPlacement::primary(id),
                    notes: &t.notes,
                    events: &t.events,
                });
            }
        }
        for p in self.clips.explicit_placements() {
            let (Some(&i), Some((notes, events))) = (index.get(&p.track), stores(p.content)) else {
                continue;
            };
            out[i].push(PlacedContent {
                placement: p.clone(),
                notes,
                events,
            });
        }
        // The order of `ClipTable::placements_on`: the primary first, then by
        // start, then by id.
        for (i, v) in out.iter_mut().enumerate() {
            let primary = ClipId::Primary(self.tracks[i].id());
            v.sort_by(|a, b| {
                let (a, b) = (&a.placement, &b.placement);
                (a.id != primary, a.start, a.id).cmp(&(b.id != primary, b.start, b.id))
            });
        }
        out
    }

    /// A copy of the document without clips whose tracks hold what their
    /// unmuted placements play — the notes starting in each window and its
    /// channel events, at their song ticks — plus each track's own other
    /// events (name, text, SysEx …) where they are; itself while the clips
    /// are the identity, and a track whose only placement is its own
    /// identity clip shares its stores. The notes of moved, windowed or
    /// linked clips are **copied**: MIDI export does not use this (it reads
    /// the placements in place, `docs/EXPORT_PLAN.md` D2); it is the
    /// independent reference the export tests compare with.
    pub fn materialize_placements(&self) -> std::borrow::Cow<'_, PlumDocument> {
        use super::events::{EventStore, RawEvent, RawKind};
        if self.clips_are_identity() {
            return std::borrow::Cow::Borrowed(self);
        }
        let mut img = self.clone();
        img.clips = ClipTable::default();
        for ti in 0..self.tracks.len() {
            let placed = self.placed_contents(ti);
            let own = self.tracks[ti].id();
            if let [pc] = placed.as_slice()
                && pc.placement.is_identity()
                && pc.placement.content == super::clip::ContentRef::Track(own)
            {
                continue;
            }
            let mut recs: Vec<super::note::NoteRecord> = Vec::new();
            let mut chan: Vec<RawEvent> = Vec::new();
            for pc in placed.iter().filter(|pc| !pc.placement.muted) {
                let (lo, hi) = pc.window();
                let shift = pc.shift();
                let song = |t: u64| (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64;
                for n in pc.notes.iter() {
                    if n.start_tick >= lo && n.start_tick < hi {
                        let mut r = n.record();
                        r.start_tick = song(r.start_tick);
                        recs.push(r);
                    }
                }
                for e in pc.events.events() {
                    if matches!(e.kind, RawKind::Channel { .. }) && e.tick >= lo && e.tick < hi {
                        chan.push(RawEvent {
                            tick: song(e.tick),
                            ..*e
                        });
                    }
                }
            }
            recs.sort_by_key(|r| r.start_tick);
            let src = &self.tracks[ti].events;
            let mut events = EventStore::new();
            let mut others = src
                .events()
                .iter()
                .filter(|e| !matches!(e.kind, RawKind::Channel { .. }))
                .peekable();
            chan.sort_by_key(|e| e.tick);
            let mut chan = chan.into_iter().peekable();
            // Merge by tick; at one tick the track's own events come first.
            loop {
                let take_other = match (others.peek(), chan.peek()) {
                    (Some(o), Some(c)) => o.tick <= c.tick,
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (None, None) => break,
                };
                let e = if take_other {
                    let o = *others.next().expect("peeked");
                    let kind = match o.kind {
                        RawKind::Meta { meta_type, payload } => RawKind::Meta {
                            meta_type,
                            payload: events
                                .store_payload(src.payload(payload))
                                .expect("fits as it did"),
                        },
                        RawKind::SysEx { payload } => RawKind::SysEx {
                            payload: events
                                .store_payload(src.payload(payload))
                                .expect("fits as it did"),
                        },
                        RawKind::SysExEscape { payload } => RawKind::SysExEscape {
                            payload: events
                                .store_payload(src.payload(payload))
                                .expect("fits as it did"),
                        },
                        k => k,
                    };
                    RawEvent { kind, ..o }
                } else {
                    chan.next().expect("peeked")
                };
                events.push(e).expect("sorted by tick");
            }
            let t = &mut img.tracks[ti];
            t.notes = super::note_store::NoteStore::from_sorted_records(&recs, false)
                .expect("records from valid stores");
            t.events = events;
        }
        std::borrow::Cow::Owned(img)
    }

    /// Changes when what placed events depend on changes: the placements and
    /// the owned contents' events (0 for an identity table). Track events
    /// have their own revisions.
    pub fn clip_events_key(&self) -> u64 {
        if self.clips.is_empty() {
            return 0;
        }
        let mut h = Fnv64::new();
        h.write_u64(self.clips.revision());
        for c in self.clips.contents() {
            h.write_u64(u64::from(c.id.0));
            h.write_u64(c.events_revision());
        }
        h.finish()
    }

    /// Every placement on track `index` (its primary first, then by start).
    pub fn placements_on(&self, index: usize) -> Vec<super::clip::ClipPlacement> {
        self.tracks
            .get(index)
            .map_or_else(Vec::new, |t| self.clips.placements_on(t.id()))
    }

    /// The notes of content `c` — a track's own or an owned content. With
    /// [`Self::notes_of_mut`], the one place a note host becomes a store
    /// (V09-S11, `docs/CLIP_PIANO_ROLL.md`).
    pub fn notes_of(&self, c: super::clip::ContentRef) -> Option<&super::note_store::NoteStore> {
        match c {
            super::clip::ContentRef::Track(id) => self.track_by_id(id).map(|t| &t.notes),
            super::clip::ContentRef::Owned(id) => self.clips.content(id).map(|o| &o.notes),
        }
    }

    pub fn notes_of_mut(
        &mut self,
        c: super::clip::ContentRef,
    ) -> Option<&mut super::note_store::NoteStore> {
        match c {
            super::clip::ContentRef::Track(id) => self.track_by_id_mut(id).map(|t| &mut t.notes),
            super::clip::ContentRef::Owned(id) => self.clips.content_mut(id).map(|o| &mut o.notes),
        }
    }

    /// Applies a prepared events edit to the content it names. False,
    /// changing nothing, when the content is missing or changed since.
    pub(crate) fn apply_events_edit(&mut self, edit: &crate::editor::EventsEdit) -> bool {
        match edit.host {
            super::clip::ContentRef::Track(id) => self
                .track_by_id_mut(id)
                .is_some_and(|t| t.apply_events_edit(edit)),
            super::clip::ContentRef::Owned(id) => self
                .clips
                .content_mut(id)
                .is_some_and(|o| o.apply_events_edit(edit)),
        }
    }

    /// The channel events of content `c`.
    pub fn events_of(&self, c: super::clip::ContentRef) -> Option<&super::events::EventStore> {
        match c {
            super::clip::ContentRef::Track(id) => self.track_by_id(id).map(|t| &t.events),
            super::clip::ContentRef::Owned(id) => self.clips.content(id).map(|o| &o.events),
        }
    }

    /// Reverts (`backwards`) or re-applies an events patch on content `c`
    /// (undo / redo); its events revision moves. False when the patch does
    /// not match or the content is missing.
    pub(crate) fn patch_events_of(
        &mut self,
        c: super::clip::ContentRef,
        patch: &super::patch::ListPatch<super::events::RawEvent>,
        backwards: bool,
    ) -> bool {
        match c {
            super::clip::ContentRef::Track(id) => self
                .track_by_id_mut(id)
                .is_some_and(|t| t.patch_events(patch, backwards)),
            super::clip::ContentRef::Owned(id) => {
                let Some(o) = self.clips.content_mut(id) else {
                    return false;
                };
                if !o.events.apply_patch(patch, backwards) {
                    return false;
                }
                o.touch_events();
                true
            }
        }
    }

    /// The content revision of `c` (§272): changes with its notes and events
    /// only — moving a clip leaves it. `None` for a missing content.
    pub fn content_revision(&self, c: super::clip::ContentRef) -> Option<(u64, u64)> {
        match c {
            super::clip::ContentRef::Track(id) => self
                .track_by_id(id)
                .map(|t| (t.notes.content_revision(), t.events_revision())),
            super::clip::ContentRef::Owned(id) => self
                .clips
                .content(id)
                .map(|o| (o.notes.content_revision(), o.events_revision())),
        }
    }

    /// A key of everything MIDI export depends on (v0.9, `docs/EXPORT_PLAN.md`
    /// D5): [`analysis_revision`](Self::analysis_revision) (notes, events,
    /// placements, track structure), the Conductor (tempo, signatures,
    /// markers, chapters, Master Volume), every track's properties
    /// (corrections), explicit end and subchannel table, the key range, the
    /// timing division and the file format. View state (selection, Mute /
    /// Solo, collapse, zoom) does not change it. O(tracks + contents).
    pub fn export_key(&self) -> u64 {
        let mut h = Fnv64::new();
        h.write_u64(self.analysis_revision());
        h.write_u64(self.conductor.revision());
        h.write_u64(u64::from(self.division.encoded()));
        h.write_u64(u64::from(self.format.word()));
        h.write_u64(match self.project.keys {
            super::project::KeyRange::Standard => 0,
            super::project::KeyRange::Extended256 => 1,
        });
        h.write_u64(self.extra_chunks.len() as u64);
        for t in &self.tracks {
            h.write_u64(t.props_revision());
            h.write_u64(t.subchannels_revision());
            h.write_u64(t.end_of_track_tick.map_or(u64::MAX, |e| e));
        }
        h.finish()
    }

    /// A key of everything analysis depends on (§274): every placement and the
    /// content revision of every track and owned content. Moving a clip changes
    /// it even though no content changed.
    pub fn analysis_revision(&self) -> u64 {
        let mut h = Fnv64::new();
        h.write_u64(self.structure_revision);
        h.write_u64(self.clips.revision());
        for t in &self.tracks {
            h.write_u64(u64::from(t.id().0));
            h.write_u64(t.notes.content_revision());
            h.write_u64(t.events_revision());
        }
        for c in self.clips.contents() {
            h.write_u64(u64::from(c.id.0));
            h.write_u64(c.notes.content_revision());
            h.write_u64(c.events_revision());
        }
        h.finish()
    }

    /// Checks the clip table against the tracks (see `docs/CLIP_ARCHITECTURE.md`).
    pub fn validate_clips(&self) -> Result<(), String> {
        self.clips.validate(|id| self.track_by_id(id).is_some())
    }

    /// Changes whenever the tempo map changes (playback rebuilds its tempo map).
    #[inline]
    pub fn tempo_revision(&self) -> u64 {
        self.conductor.tempo_revision()
    }

    /// Id the next added track will get.
    pub fn next_track_id(&self) -> u32 {
        self.next_track_id
    }

    /// Format of the imported file (or Format 1 for a document created in memory).
    pub fn source_format(&self) -> SmfFormat {
        self.format
    }

    /// Format that export will write. A Format 0 source that now has more than one
    /// track is promoted to Format 1, because Format 0 cannot hold independent tracks.
    pub fn export_format(&self) -> SmfFormat {
        if self.format == SmfFormat::SingleTrack && self.tracks.len() > 1 {
            SmfFormat::Parallel
        } else {
            self.format
        }
    }

    #[inline]
    pub fn division(&self) -> Division {
        self.division
    }

    #[inline]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    #[inline]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn track_mut(&mut self, index: usize) -> Option<&mut Track> {
        self.tracks.get_mut(index)
    }

    pub fn index_of(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id() == id)
    }

    pub fn track_by_id(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id() == id)
    }

    pub fn track_by_id_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id() == id)
    }

    pub fn extra_chunks(&self) -> &[ExtraChunk] {
        &self.extra_chunks
    }

    #[inline]
    pub fn structure_revision(&self) -> u64 {
        self.structure_revision
    }

    /// Changes whenever the Track Folders change (track list layout caches).
    #[inline]
    pub fn layout_revision(&self) -> u64 {
        self.layout_revision
    }

    pub(crate) fn touch_layout(&mut self) {
        self.layout_revision += 1;
    }

    /// B1-0103 (Change PPQN): the history swaps the timebase in; the song
    /// end memo starts over.
    pub(crate) fn set_division(&mut self, d: Division) {
        self.division = d;
        self.end_memo = EndMemo::default();
        self.structure_revision = self.structure_revision.wrapping_add(1);
    }

    /// Total notes across all tracks. O(tracks) using cached per-store counts.
    pub fn note_count(&self) -> usize {
        self.tracks.iter().map(|t| t.notes.len()).sum()
    }

    /// Notes stored in the project (v0.9): every track's plus every owned
    /// clip content's (independent copies) — what a save writes. A linked
    /// clip adds none (it shows stored notes again). O(tracks + contents).
    pub fn stored_note_count(&self) -> usize {
        self.note_count()
            + self
                .clips
                .contents()
                .iter()
                .map(|c| c.notes.len())
                .sum::<usize>()
    }

    pub fn preserved_event_count(&self) -> usize {
        self.tracks.iter().map(|t| t.events.len()).sum()
    }

    /// Authoritative project duration in ticks: the latest note end, preserved
    /// event (controller, text, …), End-of-Track position of any track, or
    /// Conductor entry (tempo change, time signature, marker, chapter end).
    /// Trailing silence recorded by an End-of-Track event (imported or saved in a
    /// `.lup`) counts. Note *starts* alone never define the end. O(tracks).
    pub fn duration_ticks(&self) -> u64 {
        self.content_end_tick()
    }

    /// Latest content tick in any track or the Conductor (for scroll extents).
    /// v0.9: with clips, what the placements play — each unmuted clip to its
    /// window's end and to the end of its notes, its events moved — plus each
    /// track's own other events and explicit end (as export writes them).
    pub fn content_end_tick(&self) -> u64 {
        let tracks = if self.clips_are_identity() {
            self.tracks.iter().map(Track::content_end_tick).max()
        } else {
            let mut h = Fnv64::new();
            h.write_u64(self.analysis_revision());
            for t in &self.tracks {
                h.write_u64(t.end_of_track_tick.map_or(u64::MAX, |e| e.wrapping_add(1)));
            }
            let key = h.finish();
            let memo = self
                .end_memo
                .0
                .lock()
                .ok()
                .and_then(|m| *m)
                .filter(|&(k, _)| k == key);
            match memo {
                Some((_, end)) => Some(end),
                None => {
                    let placed = self.placed_contents_by_track();
                    let end = placed
                        .iter()
                        .enumerate()
                        .map(|(i, pcs)| self.placed_end_tick(i, pcs))
                        .max();
                    if let (Some(e), Ok(mut m)) = (end, self.end_memo.0.lock()) {
                        *m = Some((key, e));
                    }
                    end
                }
            }
        };
        tracks.unwrap_or(0).max(self.conductor.last_tick())
    }

    /// The song end of what track `index` plays through its placements
    /// `placed` (its `placed_contents`).
    fn placed_end_tick(&self, index: usize, placed: &[PlacedContent<'_>]) -> u64 {
        use super::events::RawKind;
        let t = &self.tracks[index];
        let mut end = t.end_of_track_tick.unwrap_or(0);
        if let Some(e) = t
            .events
            .events()
            .iter()
            .rev()
            .find(|e| !matches!(e.kind, RawKind::Channel { .. }))
        {
            end = end.max(e.tick);
        }
        for pc in placed {
            if pc.placement.muted {
                continue;
            }
            let (lo, hi) = pc.window();
            let shift = pc.shift();
            let song = |c: u64| (i128::from(c) + shift).clamp(0, i128::from(u64::MAX)) as u64;
            if let Some(l) = pc.placement.length {
                end = end.max(pc.placement.start.saturating_add(l));
            }
            if let Some(e) = pc.notes.max_end_of_starts_in(lo, hi) {
                end = end.max(song(e));
            }
            let ev = pc.events.events();
            let k = ev.partition_point(|e| e.tick < hi);
            if let Some(e) = ev[..k]
                .iter()
                .rev()
                .find(|e| e.tick >= lo && matches!(e.kind, RawKind::Channel { .. }))
            {
                end = end.max(song(e.tick));
            }
        }
        end
    }

    pub fn heap_bytes(&self) -> usize {
        self.tracks.iter().map(Track::heap_bytes).sum::<usize>()
            + self
                .extra_chunks
                .iter()
                .map(|c| c.data.capacity())
                .sum::<usize>()
            + self.conductor.heap_bytes()
    }

    /// Appends a new empty track (no events besides the End-of-Track that export adds)
    /// and returns its index.
    pub fn add_track(&mut self) -> Result<usize, DocumentError> {
        if self.tracks.len() >= usize::from(u16::MAX) {
            return Err(DocumentError::TooManyTracks);
        }
        let id = TrackId(self.next_track_id);
        self.next_track_id = self.next_track_id.wrapping_add(1);
        self.tracks.push(Track::new(id));
        self.structure_revision += 1;
        Ok(self.tracks.len() - 1)
    }

    /// Removes a freshly imported track (an extra "layer" chunk merged into its
    /// track); extra chunks keep their place among the remaining tracks.
    pub(crate) fn remove_imported_track(&mut self, index: usize) {
        if index < self.tracks.len() {
            self.tracks.remove(index);
            for c in &mut self.extra_chunks {
                if c.after_track_chunks > index {
                    c.after_track_chunks -= 1;
                }
            }
            self.structure_revision += 1;
        }
    }

    /// Deletes a track together with its subtracks. Refuses to delete every
    /// track. Returns the index that should become active (the track that now
    /// follows the deleted subtree, or the one before it at the end of the list).
    pub fn delete_track(&mut self, index: usize) -> Result<usize, DocumentError> {
        if index >= self.tracks.len() {
            return Err(DocumentError::NoSuchTrack);
        }
        let end = self.subtree_end(index);
        self.take_tracks(index, end - index)?;
        Ok(index.min(self.tracks.len() - 1))
    }

    /// Time signatures that govern the musical grid, as `(tick, numerator,
    /// denominator_power)` sorted by tick: the Conductor's for metrical Format 0/1
    /// projects; for Format 2 those of the given track, since each track is an
    /// independent sequence with its own meter.
    pub fn meter_events(&self, active_track: usize) -> Vec<(u64, u8, u8)> {
        match self.conductor.scope() {
            ConductorScope::Timeline => self.conductor.meter_events(),
            ConductorScope::Sequences => {
                let mut v: Vec<(u64, u8, u8)> = self
                    .tracks
                    .get(active_track)
                    .map(|t| t.events.time_signatures().collect())
                    .unwrap_or_default();
                v.sort_by_key(|e| e.0);
                v
            }
            ConductorScope::MarkersOnly => Vec::new(),
        }
    }

    /// Deterministic 64-bit fingerprint of all musical content (format, division,
    /// tracks, notes, preserved events and payloads, End-of-Track ticks, extra
    /// chunks). Selection state is excluded. Used by tests to prove that operations
    /// which must be identities (Flip twice, Scale ×1, rejected transforms, unchanged
    /// round trips) leave the document untouched.
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv64::new();
        h.write_u64(u64::from(self.format.word()));
        h.write_u64(u64::from(self.division.encoded()));
        h.write_u64(self.tracks.len() as u64);
        for t in &self.tracks {
            h.write_u64(t.notes.len() as u64);
            for n in t.notes.iter() {
                h.write_u64(n.start_tick);
                h.write_u64(n.duration_ticks);
                h.write_u64(u64::from(n.attrs.raw()));
            }
            h.write_u64(t.events.len() as u64);
            for e in t.events.events() {
                h.write_u64(e.tick);
                h.write_u64(u64::from(e.anchor));
                match e.kind {
                    RawKind::Channel { status, data } | RawKind::System { status, data } => {
                        h.write(&[status, data[0], data[1]]);
                    }
                    RawKind::Meta { meta_type, payload } => {
                        h.write(&[0xFF, meta_type]);
                        h.write_u64(u64::from(payload.len));
                        h.write(t.events.payload(payload));
                    }
                    RawKind::SysEx { payload } => {
                        h.write(&[0xF0]);
                        h.write_u64(u64::from(payload.len));
                        h.write(t.events.payload(payload));
                    }
                    RawKind::SysExEscape { payload } => {
                        h.write(&[0xF7]);
                        h.write_u64(u64::from(payload.len));
                        h.write(t.events.payload(payload));
                    }
                }
            }
            h.write_u64(t.end_of_track_tick.map_or(u64::MAX, |x| x));
        }
        for c in &self.extra_chunks {
            h.write(&c.chunk_type);
            h.write_u64(c.after_track_chunks as u64);
            h.write(&c.data);
        }
        // Conductor: musical content only (tempo values and shapes, signatures,
        // markers). Ids, origins and "implicit default" flags are not music.
        let c = &self.conductor;
        h.write(&[c.scope().code()]);
        h.write_u64(c.tempo_points().len() as u64);
        for p in c.tempo_points() {
            h.write_u64(p.tick);
            write_tempo(&mut h, p.value);
            h.write(&[p.interp as u8]);
        }
        h.write_u64(c.time_signatures().len() as u64);
        for s in c.time_signatures() {
            h.write_u64(s.tick);
            h.write(&[s.numerator, s.denom_pow, s.clocks, s.thirty_seconds]);
        }
        h.write_u64(c.markers().len() as u64);
        for m in c.markers() {
            h.write_u64(m.start);
            h.write_u64(m.end.map_or(u64::MAX, |e| e));
            h.write(&[u8::from(m.cue)]);
            h.write_u64(m.title_bytes().len() as u64);
            h.write(m.title_bytes());
        }
        h.write_u64(c.master_volume().len() as u64);
        for p in c.master_volume() {
            h.write_u64(p.tick);
            h.write(&[(p.value >> 8) as u8, p.value as u8, p.device]);
        }
        // v0.9 clips: where contents play is music (nothing for a v0.8 table).
        self.clips.hash_into(&mut |b| h.write(b));
        h.finish()
    }

    /// Every track's `notes.content_hash()`, in track order — on up to the
    /// MIDI worker count of threads for a large document (L-S29: after a
    /// 302M-note import this pass took 1.5 s on one core of eight).
    fn track_content_hashes(&self) -> Vec<u64> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let notes = self.note_count() as u64;
        let threads =
            crate::midi::workers::threads_for(Some(notes.saturating_mul(6))).min(self.tracks.len());
        if threads < 2 {
            return self.tracks.iter().map(|t| t.notes.content_hash()).collect();
        }
        let next = AtomicUsize::new(0);
        let mut out = vec![0u64; self.tracks.len()];
        let parts: Vec<Vec<(usize, u64)>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(|| {
                        let mut done = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(t) = self.tracks.get(i) else {
                                return done;
                            };
                            done.push((i, t.notes.content_hash()));
                        }
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap_or_default())
                .collect()
        });
        let mut seen = 0;
        for (i, v) in parts.into_iter().flatten() {
            out[i] = v;
            seen += 1;
        }
        if seen != self.tracks.len() {
            // A worker failed (it cannot, short of a panic): hash in place.
            return self.tracks.iter().map(|t| t.notes.content_hash()).collect();
        }
        out
    }

    /// v1.0-beta1 (B1-0121): a fast 64-bit fingerprint of everything that can
    /// change the exported MIDI file — format, division, every track's notes
    /// (channel, velocity, Note Off kind and subchannel included), events and
    /// payloads (names included), End of Track, place in the tree,
    /// subchannel table, the properties export applies, extra chunks, the
    /// Conductor's music, the clips and the key range — and nothing that
    /// cannot: selection, Mute / Solo, output routes, Onion Skin colours,
    /// note groups, ids, view and other project settings. Strict export
    /// (`midi::provenance`) compares it with the value recorded at import, so
    /// undoing every edit makes the project "the file" again. When unsure an
    /// input is included: a spurious change only costs strict export.
    pub fn export_fingerprint(&self) -> u64 {
        use crate::codec::WordHash;
        // L-S29: the notes' content hashes (a pass over every note) on the
        // MIDI workers, combined in track order below — the same value.
        let content = self.track_content_hashes();
        let mut h = WordHash::new();
        h.word(u64::from(self.format.word()));
        h.word(u64::from(self.division.encoded()));
        h.word(self.tracks.len() as u64);
        for (t, &content) in self.tracks.iter().zip(&content) {
            h.word(t.parent().map_or(0, |p| u64::from(p.0) + 1));
            h.word(t.notes.len() as u64);
            h.word(content);
            h.word(t.events.len() as u64);
            for e in t.events.events() {
                h.word(e.tick);
                h.word(u64::from(e.anchor));
                match e.kind {
                    RawKind::Channel { status, data } | RawKind::System { status, data } => {
                        h.word(u64::from_le_bytes([
                            status, data[0], data[1], 0, 0, 0, 0, 1,
                        ]));
                    }
                    RawKind::Meta { meta_type, payload } => {
                        h.word(0x2FF00 | u64::from(meta_type));
                        h.field(t.events.payload(payload));
                    }
                    RawKind::SysEx { payload } => {
                        h.word(0x3F0);
                        h.field(t.events.payload(payload));
                    }
                    RawKind::SysExEscape { payload } => {
                        h.word(0x4F7);
                        h.field(t.events.payload(payload));
                    }
                }
            }
            h.word(t.end_of_track_tick.map_or(u64::MAX, |x| x));
            h.word(t.subchannels().len() as u64);
            for sc in t.subchannels().entries() {
                h.word(u64::from(sc.id.0));
                h.word(u64::from(sc.midi_channel));
                match &sc.name {
                    Some(n) => h.field(n.as_bytes()),
                    None => h.word(u64::MAX),
                }
            }
            let p = t.props();
            h.words(&[
                u64::from(p.default_channel),
                u64::from(p.kind == super::track::TrackKind::Rhythm),
                p.key_correction as u64,
                u64::from(p.key_range.0) << 8 | u64::from(p.key_range.1),
                p.tick_correction as u64,
                u64::from(p.correction_from_measure),
                u64::from(p.gate.hundredths()),
            ]);
        }
        h.word(self.extra_chunks.len() as u64);
        for c in &self.extra_chunks {
            h.field(&c.chunk_type);
            h.word(c.after_track_chunks as u64);
            h.field(&c.data);
        }
        let c = &self.conductor;
        h.word(u64::from(c.scope().code()));
        h.word(c.tempo_points().len() as u64);
        for p in c.tempo_points() {
            h.word(p.tick);
            match p.value {
                TempoValue::Micros(us) => h.words(&[0, u64::from(us)]),
                TempoValue::Bpm(b) => h.words(&[1, b.mantissa(), b.exponent() as u64]),
            }
            h.word(p.interp as u64);
        }
        h.word(c.time_signatures().len() as u64);
        for s in c.time_signatures() {
            h.word(s.tick);
            h.word(u64::from_le_bytes([
                s.numerator,
                s.denom_pow,
                s.clocks,
                s.thirty_seconds,
                0,
                0,
                0,
                0,
            ]));
        }
        h.word(c.markers().len() as u64);
        for m in c.markers() {
            h.words(&[m.start, m.end.map_or(u64::MAX, |e| e), u64::from(m.cue)]);
            h.field(m.title_bytes());
        }
        h.word(c.master_volume().len() as u64);
        for p in c.master_volume() {
            h.words(&[p.tick, u64::from(p.value), u64::from(p.device)]);
        }
        let mut clips = WordHash::new();
        self.clips.hash_into(&mut |b| clips.field(b));
        h.word(clips.finish());
        h.word(u64::from(
            self.project.keys == super::project::KeyRange::Extended256,
        ));
        h.finish()
    }

    /// [`PlumDocument::fingerprint`] plus everything a native project stores beyond
    /// MIDI content: track ids, the next track id, Conductor ids/flags/origins and
    /// the project settings (name, SoundFont, Onion Skin, preserved unknown
    /// sections). Used to verify `.lup` round trips and undo/redo.
    pub fn state_fingerprint(&self) -> u64 {
        let mut h = Fnv64::new();
        h.write_u64(self.fingerprint());
        h.write_u64(u64::from(self.next_track_id));
        for t in &self.tracks {
            h.write_u64(u64::from(t.id().0));
            // v0.5: hierarchy and subchannels (tables and every note's subchannel).
            h.write_u64(t.parent().map_or(0, |p| u64::from(p.0) + 1));
            h.write_u64(t.subchannels().len() as u64);
            for sc in t.subchannels().entries() {
                h.write_u64(u64::from(sc.id.0));
                h.write(&[sc.midi_channel]);
                match &sc.name {
                    Some(n) => {
                        h.write_u64(n.len() as u64 + 1);
                        h.write(n.as_bytes());
                    }
                    None => h.write_u64(0),
                }
            }
            for n in t.notes.iter() {
                h.write_u64(u64::from(n.sub.0) | u64::from(n.group) << 32);
            }
            let p = t.props();
            h.write(&[
                p.default_channel,
                u8::from(p.kind == super::track::TrackKind::Rhythm),
                p.key_correction as u8,
                p.key_range.0,
                p.key_range.1,
                u8::from(t.mute),
                u8::from(t.solo),
            ]);
            // v0.9: key corrections beyond ±127 (256-Key Mode) — standard
            // values hash exactly as before.
            if i8::try_from(p.key_correction).is_err() {
                h.write_u64(p.key_correction as u64);
            }
            match &p.route {
                super::track::OutputRoute::Internal => h.write_u64(u64::MAX),
                super::track::OutputRoute::External(n) => {
                    h.write_u64(n.len() as u64);
                    h.write(n.as_bytes());
                }
            }
            h.write_u64(p.tick_correction as u64);
            h.write_u64(u64::from(p.correction_from_measure));
            h.write_u64(u64::from(p.gate.hundredths()));
            match p.onion_color {
                Some(c) => h.write(&[1, c[0], c[1], c[2]]),
                None => h.write(&[0]),
            }
        }
        // v0.7: note groups.
        let g = &self.project.groups;
        h.write_u64(u64::from(g.next_id()));
        for e in g.entries() {
            h.write_u64(u64::from(e.id));
            h.write_u64(u64::from(e.parent));
            h.write_u64(u64::from(e.link));
            h.write(&[u8::from(e.locked), u8::from(e.temporary)]);
            if let Some(c) = e.color {
                h.write(&c);
            }
            if let Some(n) = &e.name {
                h.write(n.as_bytes());
            }
        }
        let c = &self.conductor;
        h.write_u64(u64::from(c.next_id()));
        let origin = |h: &mut Fnv64, o: Option<super::conductor::Origin>| match o {
            Some(o) => {
                h.write_u64(u64::from(o.track.0));
                h.write_u64(u64::from(o.anchor));
                h.write_u64(u64::from(o.rank));
            }
            None => h.write_u64(u64::MAX),
        };
        for p in c.tempo_points() {
            h.write_u64(u64::from(p.id));
            h.write(&[u8::from(p.implicit)]);
            origin(&mut h, p.origin);
        }
        for s in c.time_signatures() {
            h.write_u64(u64::from(s.id));
            h.write(&[u8::from(s.implicit)]);
            origin(&mut h, s.origin);
        }
        for m in c.markers() {
            h.write_u64(u64::from(m.id));
            h.write(m.title.as_bytes());
            origin(&mut h, m.origin);
        }
        for m in c.master_volume() {
            h.write_u64(u64::from(m.id));
            origin(&mut h, m.origin);
        }
        let p = &self.project;
        h.write_u64(p.name.len() as u64);
        h.write(p.name.as_bytes());
        match &p.soundfont {
            Some(sf) => {
                h.write_u64(sf.path.len() as u64);
                h.write(sf.path.as_bytes());
            }
            None => h.write_u64(u64::MAX),
        }
        h.write(&[p.onion.mode.code(), p.onion.opacity]);
        h.write_u64(p.onion.specific.len() as u64);
        for id in &p.onion.specific {
            h.write_u64(u64::from(id.0));
        }
        match p.looping.range {
            Some((a, b)) => {
                h.write_u64(a);
                h.write_u64(b);
            }
            None => h.write_u64(u64::MAX),
        }
        h.write(&[u8::from(p.looping.enabled)]);
        for g in &p.graphs.slots {
            let (k, n) = g.target.code();
            h.write(&[u8::from(g.visible), k, n, g.channel]);
        }
        for u in &p.unknown_sections {
            h.write(&u.tag);
            h.write_u64(u64::from(u.flags));
            h.write_u64(u.data.len() as u64);
            h.write(&u.data);
        }
        // v0.9 256-Key Mode (standard projects hash exactly as before).
        if p.keys != super::project::KeyRange::Standard {
            h.write(b"keys");
            h.write_u64(p.keys.keys() as u64);
        }
        // v0.9 clips: names, colours, groups, next ids (nothing for a v0.8 table).
        self.clips.hash_state_into(&mut |b| h.write(b));
        h.finish()
    }
}

fn write_tempo(h: &mut Fnv64, v: TempoValue) {
    match v {
        TempoValue::Micros(us) => {
            h.write(&[0]);
            h.write_u64(u64::from(us));
        }
        TempoValue::Bpm(b) => {
            h.write(&[1]);
            h.write_u64(b.mantissa());
            h.write_u64(b.exponent() as u64);
        }
    }
}

/// FNV-1a, 64-bit. Deterministic across runs and platforms (unlike `DefaultHasher`).
struct Fnv64(u64);

impl Fnv64 {
    fn new() -> Self {
        Fnv64(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.write(&v.to_le_bytes());
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_add_delete_policy() {
        let mut d = PlumDocument::new_empty(Division::ppqn(480));
        assert_eq!(d.delete_track(0), Err(DocumentError::LastTrack));
        let i = d.add_track().unwrap();
        assert_eq!(i, 1);
        let id1 = d.track(1).unwrap().id();
        d.add_track().unwrap();
        // Delete the middle track: the next one (old index 2) becomes active.
        assert_eq!(d.delete_track(1), Ok(1));
        assert!(d.index_of(id1).is_none());
        // Delete the last in the list: active moves to the previous one.
        assert_eq!(d.delete_track(1), Ok(0));
        assert_eq!(d.track_count(), 1);
        assert_eq!(d.delete_track(5), Err(DocumentError::NoSuchTrack));
        // Ids are never reused.
        d.add_track().unwrap();
        let ids: Vec<u32> = d.tracks().iter().map(|t| t.id().0).collect();
        assert_eq!(ids, vec![0, 3]);
    }

    #[test]
    fn format0_promotion_rule() {
        let mut d = PlumDocument::from_parts(
            SmfFormat::SingleTrack,
            Division::ppqn(96),
            vec![Track::new(TrackId(0))],
            vec![],
        );
        assert_eq!(d.export_format(), SmfFormat::SingleTrack);
        d.add_track().unwrap();
        assert_eq!(d.export_format(), SmfFormat::Parallel);
        d.delete_track(1).unwrap();
        assert_eq!(d.export_format(), SmfFormat::SingleTrack);
    }

    /// L-S29: the export fingerprint's per-track content hashes, taken on
    /// worker threads for a large document, are exactly the ones taken in
    /// place, in track order.
    #[test]
    fn parallel_content_hashes_equal_the_sequential_ones() {
        use crate::model::{NoteAttrs, NoteOffKind, NoteRecord};
        let mut d = PlumDocument::new_empty(Division::ppqn(960));
        for _ in 0..9 {
            d.add_track().unwrap();
        }
        for t in 0..10usize {
            let recs: Vec<NoteRecord> = (0..90_000u64 + t as u64 * 1_000)
                .map(|k| {
                    NoteRecord::new(
                        k * 3 + t as u64,
                        5 + k % 50,
                        NoteAttrs::new(
                            (k % 120) as u8,
                            (k % 127 + 1) as u8,
                            0,
                            0,
                            NoteOffKind::NoteOff,
                        )
                        .unwrap(),
                    )
                })
                .collect();
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        assert!(d.note_count() > 900_000);
        let seq: Vec<u64> = d.tracks().iter().map(|t| t.notes.content_hash()).collect();
        assert_eq!(d.track_content_hashes(), seq);
        // A different note in one track changes that track's hash only.
        let fp = d.export_fingerprint();
        let r = NoteRecord::new(
            1,
            1,
            NoteAttrs::new(1, 1, 0, 0, NoteOffKind::NoteOff).unwrap(),
        );
        d.track_mut(7)
            .unwrap()
            .notes
            .insert_sorted_batch(&[r], false)
            .unwrap();
        let after = d.track_content_hashes();
        assert_eq!(&after[..7], &seq[..7]);
        assert_ne!(after[7], seq[7]);
        assert_ne!(d.export_fingerprint(), fp);
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::history::{History, HistoryLimits};
    use crate::model::clip::ContentRef;
    use crate::model::events::{RawEvent, RawKind};
    use crate::model::patch::ListPatch;
    use crate::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn note(s: u64) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: 100,
            attrs: NoteAttrs::new(64, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    /// V09-S11: `notes_of` / `notes_of_mut` / `events_of` / `patch_events_of`
    /// reach a track's own content and an owned content alike; an edit of
    /// one moves only that content's revision; a missing host is `None`.
    #[test]
    fn note_hosts_reach_track_and_owned_contents() {
        let mut d = PlumDocument::new_project("h".into(), Bpm::DEFAULT, 480);
        let op = crate::editor::clips::add_clip(&d, 0, 960, 1920).unwrap();
        History::new(HistoryLimits::default())
            .apply(&mut d, op)
            .unwrap();
        let owned = ContentRef::Owned(d.clips().contents()[0].id);
        let own = ContentRef::Track(d.track(0).unwrap().id());
        let (before_own, before_owned) = (
            d.content_revision(own).unwrap(),
            d.content_revision(owned).unwrap(),
        );
        d.notes_of_mut(owned)
            .unwrap()
            .insert_sorted_batch(&[note(0), note(240)], false)
            .unwrap();
        assert_eq!(d.notes_of(owned).unwrap().len(), 2);
        assert_eq!(
            d.notes_of(own).unwrap().len(),
            0,
            "the track's own notes untouched"
        );
        assert_eq!(d.content_revision(own).unwrap(), before_own);
        assert_ne!(d.content_revision(owned).unwrap().0, before_owned.0);
        // Events: a patch applied and reverted on the owned content.
        let mut patch = ListPatch::new();
        patch.insert(
            0,
            RawEvent {
                tick: 10,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xB0,
                    data: [7, 90],
                },
            },
        );
        assert!(d.patch_events_of(owned, &patch, false));
        assert_eq!(d.events_of(owned).unwrap().len(), 1);
        assert_ne!(d.content_revision(owned).unwrap().1, before_owned.1);
        assert!(d.patch_events_of(owned, &patch, true));
        assert_eq!(d.events_of(owned).unwrap().len(), 0);
        assert_eq!(
            d.events_of(own).unwrap().len(),
            d.track(0).unwrap().events.len()
        );
        // Missing hosts.
        let gone = ContentRef::Owned(crate::model::ContentId(99));
        assert!(d.notes_of(gone).is_none() && d.notes_of_mut(gone).is_none());
        assert!(!d.patch_events_of(gone, &patch, false));
        assert!(d.notes_of(ContentRef::Track(TrackId(99))).is_none());
    }
}
