//! Clips (v0.9 contract §039, §272–§274): what a clip contains (content) is
//! separate from where it is placed (placement). See `docs/CLIP_ARCHITECTURE.md`.
//!
//! * A **content** is a note store and an event store in content ticks: a
//!   track's own `notes` / `events` ([`ContentRef::Track`]) or a content the
//!   document owns here ([`ContentRef::Owned`]).
//! * A **placement** shows a content on a track: song tick `start` shows content
//!   tick `offset`, for `length` ticks (`None`: to the content's end). A content
//!   note sounds when its start lies in the window; its length is kept.
//! * Every track's own content has an implicit **primary placement**
//!   ([`ClipId::Primary`]): start 0, offset 0, open length, on its own track —
//!   the identity, exactly v0.8. The table stores a record for it only once it
//!   is changed or removed, so a v0.8 project has an empty table.
//!
//! Nothing here is per note: placements are small records; contents are the
//! stores they name, never copies of other contents.

use super::events::EventStore;
use super::note_store::NoteStore;
use super::track::TrackId;

/// Longest clip or content name, in characters.
pub const MAX_CLIP_NAME_CHARS: usize = 200;
/// Most placements / owned contents a table may hold (hostile-input bound).
pub const MAX_CLIP_RECORDS: usize = 1 << 20;

/// Identity of a placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClipId {
    /// The primary clip of a track (shows the track's own content).
    Primary(TrackId),
    /// Any other placement (linked instances, owned contents).
    Placed(u32),
}

/// Identity of a content the document owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentId(pub u32);

/// What a placement shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContentRef {
    /// A track's own notes and events.
    Track(TrackId),
    /// A content in the document's clip table.
    Owned(ContentId),
}

impl std::fmt::Display for ClipId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipId::Primary(t) => write!(f, "the primary clip of track id {}", t.0),
            ClipId::Placed(n) => write!(f, "clip {n}"),
        }
    }
}

impl std::fmt::Display for ContentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "clip content {}", self.0)
    }
}

/// A content owned by the document (a Piano Roll clip that is not a track's
/// own notes).
#[derive(Clone, Debug)]
pub struct OwnedContent {
    pub id: ContentId,
    pub notes: NoteStore,
    pub events: EventStore,
    /// Bumped when the events change (the notes carry their own revision).
    events_revision: u64,
    pub name: Option<String>,
    /// RGB colour of the clip, if chosen.
    pub color: Option<u32>,
}

impl OwnedContent {
    pub fn new(id: ContentId) -> OwnedContent {
        OwnedContent {
            id,
            notes: NoteStore::new(),
            events: EventStore::new(),
            events_revision: 0,
            name: None,
            color: None,
        }
    }

    /// Marks the events as changed.
    pub fn touch_events(&mut self) {
        self.events_revision = self.events_revision.wrapping_add(1);
    }

    pub fn events_revision(&self) -> u64 {
        self.events_revision
    }

    /// Applies a prepared events edit of this content (controllers; v0.9
    /// V09-S11). False, changing nothing, when it was prepared from another
    /// content or an older revision.
    pub(crate) fn apply_events_edit(&mut self, edit: &crate::editor::EventsEdit) -> bool {
        if edit.host != ContentRef::Owned(self.id) || edit.base_revision != self.events_revision {
            return false;
        }
        let arena_before = self.events.arena_len();
        if !edit.append.is_empty() && !self.events.append_payload_at(edit.append_at, &edit.append) {
            return false;
        }
        if !self.events.apply_patch(&edit.patch, false) {
            self.events.truncate_arena(arena_before);
            return false;
        }
        self.touch_events();
        true
    }

    /// Last content tick that holds a note end or an event.
    pub fn end_tick(&self) -> u64 {
        self.notes
            .max_end()
            .unwrap_or(0)
            .max(self.events.last_tick().unwrap_or(0))
    }
}

/// Where and how a content is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipPlacement {
    pub id: ClipId,
    /// The track the clip sits on.
    pub track: TrackId,
    pub content: ContentRef,
    /// Song tick of the window's first content tick.
    pub start: u64,
    /// The content tick shown at `start`.
    pub offset: u64,
    /// Window length in ticks (`None`: to the content's end).
    pub length: Option<u64>,
    /// A muted clip is shown but does not play, export or count.
    pub muted: bool,
    pub name: Option<String>,
    pub color: Option<u32>,
    /// Bumped by every change of this placement.
    pub revision: u64,
}

impl ClipPlacement {
    /// The implicit primary placement of track `t`: the identity.
    pub fn primary(t: TrackId) -> ClipPlacement {
        ClipPlacement {
            id: ClipId::Primary(t),
            track: t,
            content: ContentRef::Track(t),
            start: 0,
            offset: 0,
            length: None,
            muted: false,
            name: None,
            color: None,
            revision: 0,
        }
    }

    /// Equal in everything but `revision` (every field named, so a new
    /// field cannot be forgotten here).
    pub fn same_except_revision(&self, other: &ClipPlacement) -> bool {
        let ClipPlacement {
            id,
            track,
            content,
            start,
            offset,
            length,
            muted,
            name,
            color,
            revision: _,
        } = self;
        *id == other.id
            && *track == other.track
            && *content == other.content
            && *start == other.start
            && *offset == other.offset
            && *length == other.length
            && *muted == other.muted
            && *name == other.name
            && *color == other.color
    }

    /// Whether this placement shows its content exactly where it is (start 0,
    /// offset 0, open length, not muted, on the content's own track).
    pub fn is_identity(&self) -> bool {
        self.start == 0
            && self.offset == 0
            && self.length.is_none()
            && !self.muted
            && self.content == ContentRef::Track(self.track)
    }

    /// The content ticks shown: `[offset, offset + length)` (open: to the end
    /// of time).
    pub fn content_window(&self) -> (u64, u64) {
        let end = self
            .length
            .map_or(u64::MAX, |l| self.offset.saturating_add(l));
        (self.offset, end)
    }

    /// Whether a note starting at content tick `c` belongs to the clip.
    #[inline]
    pub fn shows(&self, c: u64) -> bool {
        let (a, b) = self.content_window();
        c >= a && c < b
    }

    /// The song tick of content tick `c` (`None`: before the window, or past
    /// the largest tick).
    #[inline]
    pub fn song_tick(&self, c: u64) -> Option<u64> {
        c.checked_sub(self.offset)
            .and_then(|d| self.start.checked_add(d))
    }

    /// Song ticks `[start, end)` the clip occupies for a content whose last
    /// tick is `content_end`.
    pub fn song_span(&self, content_end: u64) -> (u64, u64) {
        let len = self
            .length
            .unwrap_or_else(|| content_end.saturating_sub(self.offset));
        (self.start, self.start.saturating_add(len))
    }
}

/// The document's clips: owned contents and the placements that are not
/// implicit primaries.
#[derive(Clone, Debug, Default)]
pub struct ClipTable {
    contents: Vec<OwnedContent>,
    /// Explicit placements, in id order (changed primaries and placed clips).
    placements: Vec<ClipPlacement>,
    /// Tracks whose primary clip was removed (their own content is unplaced).
    removed_primaries: Vec<TrackId>,
    next_clip: u32,
    next_content: u32,
    /// Bumped by every placement change.
    revision: u64,
}

/// Everything in a [`ClipTable`] except the owned contents' stores: what a
/// clip operation changes (history, V09-S07). Small — placements are a few
/// dozen bytes each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipLayout {
    /// Explicit placements in id order (revisions are ignored when applied).
    pub placements: Vec<ClipPlacement>,
    /// Tracks whose primary clip is removed, sorted.
    pub removed_primaries: Vec<TrackId>,
    /// The owned contents that exist, sorted.
    pub contents: Vec<ContentId>,
    pub next_clip: u32,
    pub next_content: u32,
}

impl ClipLayout {
    /// The layout with every placement revision 0 (comparisons).
    pub fn into_unrevised(mut self) -> ClipLayout {
        for p in &mut self.placements {
            p.revision = 0;
        }
        self
    }

    pub fn heap_bytes(&self) -> usize {
        self.placements.capacity() * std::mem::size_of::<ClipPlacement>()
            + self
                .placements
                .iter()
                .map(|p| p.name.as_ref().map_or(0, String::capacity))
                .sum::<usize>()
            + self.removed_primaries.capacity() * 4
            + self.contents.capacity() * 4
    }
}

/// Records [`ClipTable::purge_tracks`] removed (to restore them exactly).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PurgedClips {
    pub placements: Vec<ClipPlacement>,
    pub removed_primaries: Vec<TrackId>,
    pub contents: Vec<ContentId>,
}

impl ClipTable {
    /// A table from stored records (the `.lup` reader). Placements must be in
    /// strictly increasing id order, removed primaries and contents in strictly
    /// increasing id order; revisions start at 0. The caller then checks the
    /// table against the tracks ([`ClipTable::validate`]).
    pub fn from_parts(
        contents: Vec<OwnedContent>,
        placements: Vec<ClipPlacement>,
        removed_primaries: Vec<TrackId>,
        next_clip: u32,
        next_content: u32,
    ) -> Result<ClipTable, String> {
        if placements.len() > MAX_CLIP_RECORDS || contents.len() > MAX_CLIP_RECORDS {
            return Err("too many clips".into());
        }
        if placements.windows(2).any(|w| w[0].id >= w[1].id) {
            return Err("clips are not in id order".into());
        }
        if removed_primaries.windows(2).any(|w| w[0] >= w[1]) {
            return Err("removed clips are not in order".into());
        }
        if contents.windows(2).any(|w| w[0].id >= w[1].id) {
            return Err("clip contents are not in id order".into());
        }
        let placements = placements
            .into_iter()
            .map(|p| ClipPlacement { revision: 0, ..p })
            .collect();
        Ok(ClipTable {
            contents,
            placements,
            removed_primaries,
            next_clip,
            next_content,
            revision: 0,
        })
    }

    /// The ids the table hands out next: (placement, content).
    pub fn next_ids(&self) -> (u32, u32) {
        (self.next_clip, self.next_content)
    }

    /// True when every track shows its own content at the identity and there
    /// is nothing else: the v0.8 model.
    pub fn is_empty(&self) -> bool {
        self.contents.is_empty() && self.placements.is_empty() && self.removed_primaries.is_empty()
    }

    /// The table without the owned contents' stores.
    pub fn layout(&self) -> ClipLayout {
        ClipLayout {
            placements: self.placements.clone(),
            removed_primaries: self.removed_primaries.clone(),
            contents: self.contents.iter().map(|c| c.id).collect(),
            next_clip: self.next_clip,
            next_content: self.next_content,
        }
    }

    /// Moves the table to `layout`. Owned contents the layout does not list
    /// move into `stash`; listed contents the table lacks come out of `stash`
    /// (nothing is copied). Placements that differ from the current ones get
    /// fresh revisions. On error nothing changes.
    pub fn apply_layout(
        &mut self,
        layout: &ClipLayout,
        stash: &mut Vec<OwnedContent>,
    ) -> Result<(), String> {
        if layout.placements.windows(2).any(|w| w[0].id >= w[1].id)
            || layout.removed_primaries.windows(2).any(|w| w[0] >= w[1])
            || layout.contents.windows(2).any(|w| w[0] >= w[1])
        {
            return Err("the clip layout is not in id order".into());
        }
        if let Some(c) = layout
            .contents
            .iter()
            .find(|&&c| self.content(c).is_none() && !stash.iter().any(|o| o.id == c))
        {
            return Err(format!("{c} is missing"));
        }
        // A stashed content the table also holds would exist twice (a record
        // that does not belong to this state).
        if let Some(c) = stash.iter().find(|o| self.content(o.id).is_some()) {
            return Err(format!("{} exists twice", c.id));
        }
        let mut ids: Vec<ContentId> = stash.iter().map(|o| o.id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err("a clip content is held twice".into());
        }
        let (keep, gone): (Vec<OwnedContent>, Vec<OwnedContent>) =
            std::mem::take(&mut self.contents)
                .into_iter()
                .partition(|c| layout.contents.binary_search(&c.id).is_ok());
        self.contents = keep;
        let (back, rest): (Vec<OwnedContent>, Vec<OwnedContent>) = std::mem::take(stash)
            .into_iter()
            .partition(|c| layout.contents.binary_search(&c.id).is_ok());
        *stash = rest;
        stash.extend(gone);
        self.contents.extend(back);
        self.contents.sort_by_key(|c| c.id);
        self.revision = self.revision.wrapping_add(1);
        let rev = self.revision;
        let old = std::mem::take(&mut self.placements);
        // Both lists are in id order (checked above for the new one; the
        // table's always is): one binary search per placement, no clone to
        // compare (V09-S13: a repeat adding 500,000 clips spent seconds in a
        // linear search here).
        self.placements = layout
            .placements
            .iter()
            .map(|p| {
                let same = old
                    .binary_search_by_key(&p.id, |q| q.id)
                    .ok()
                    .map(|i| &old[i])
                    .filter(|q| q.same_except_revision(p));
                ClipPlacement {
                    revision: same.map_or(rev, |q| q.revision),
                    ..p.clone()
                }
            })
            .collect();
        self.removed_primaries = layout.removed_primaries.clone();
        self.next_clip = layout.next_clip;
        self.next_content = layout.next_content;
        Ok(())
    }

    /// Inserts a new owned content (its id must be free and below the next
    /// content id); the history's clip operations use it.
    pub fn insert_content(&mut self, c: OwnedContent) -> Result<(), String> {
        if c.id.0 >= self.next_content || self.content(c.id).is_some() {
            return Err(format!("{} cannot be added", c.id));
        }
        let at = self.contents.partition_point(|x| x.id < c.id);
        self.contents.insert(at, c);
        Ok(())
    }

    /// Bumped by every placement change (layout caches).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn contents(&self) -> &[OwnedContent] {
        &self.contents
    }

    /// The owned content `id` (binary search: contents are kept in strictly
    /// increasing id order — `from_parts` checks it, `insert_content` keeps
    /// it, `new_content` appends the next id).
    pub fn content(&self, id: ContentId) -> Option<&OwnedContent> {
        let i = self.contents.binary_search_by_key(&id, |c| c.id).ok()?;
        self.contents.get(i)
    }

    pub fn content_mut(&mut self, id: ContentId) -> Option<&mut OwnedContent> {
        let i = self.contents.binary_search_by_key(&id, |c| c.id).ok()?;
        self.contents.get_mut(i)
    }

    /// The explicit placements (implicit primaries are not listed).
    pub fn explicit_placements(&self) -> &[ClipPlacement] {
        &self.placements
    }

    /// Tracks whose primary clip was removed.
    pub fn removed_primaries(&self) -> &[TrackId] {
        &self.removed_primaries
    }

    /// The placement `id` as it is now (an implicit primary is the identity;
    /// `None` for a removed primary or an unknown id).
    pub fn placement(&self, id: ClipId) -> Option<ClipPlacement> {
        // Sorted by id (B1-0164: a lookup per selected clip was a scan each).
        if let Ok(i) = self.placements.binary_search_by(|p| p.id.cmp(&id)) {
            return Some(self.placements[i].clone());
        }
        match id {
            ClipId::Primary(t) if !self.removed_primaries.contains(&t) => {
                Some(ClipPlacement::primary(t))
            }
            _ => None,
        }
    }

    /// Every placement hosted on track `t`, the primary first, then in start
    /// order (ties by id).
    pub fn placements_on(&self, t: TrackId) -> Vec<ClipPlacement> {
        let mut out: Vec<ClipPlacement> = Vec::new();
        let explicit_primary = self.placements.iter().any(|p| p.id == ClipId::Primary(t));
        if !explicit_primary && !self.removed_primaries.contains(&t) {
            out.push(ClipPlacement::primary(t));
        }
        let mut rest: Vec<ClipPlacement> = self
            .placements
            .iter()
            .filter(|p| p.track == t)
            .cloned()
            .collect();
        rest.sort_by(|a, b| {
            (a.id != ClipId::Primary(t), a.start, a.id).cmp(&(
                b.id != ClipId::Primary(t),
                b.start,
                b.id,
            ))
        });
        out.extend(rest);
        out
    }

    /// Every placement that shows `content`, on any track.
    pub fn placements_of(&self, content: ContentRef) -> Vec<ClipPlacement> {
        let mut out: Vec<ClipPlacement> = self
            .placements
            .iter()
            .filter(|p| p.content == content)
            .cloned()
            .collect();
        if let ContentRef::Track(t) = content
            && !out.iter().any(|p| p.id == ClipId::Primary(t))
            && !self.removed_primaries.contains(&t)
        {
            out.insert(0, ClipPlacement::primary(t));
        }
        out
    }

    /// A new owned content (empty), with a fresh id.
    pub fn new_content(&mut self) -> ContentId {
        let id = ContentId(self.next_content);
        self.next_content += 1;
        self.contents.push(OwnedContent::new(id));
        id
    }

    /// A fresh placement id.
    pub fn new_clip_id(&mut self) -> ClipId {
        let id = ClipId::Placed(self.next_clip);
        self.next_clip += 1;
        id
    }

    /// B1-0103: swaps the explicit placements with `other` (Change PPQN
    /// retimes every placement at once; ids and order are the caller's).
    /// Every placement gets the bumped table revision.
    pub(crate) fn swap_placements(&mut self, other: &mut Vec<ClipPlacement>) {
        std::mem::swap(&mut self.placements, other);
        self.revision = self.revision.wrapping_add(1);
        for p in &mut self.placements {
            p.revision = self.revision;
        }
    }

    /// Sets placement `p` (adds it, or replaces the placement with its id),
    /// bumping its revision and the table's. A primary set back to the
    /// identity is stored implicitly again.
    pub fn set_placement(&mut self, mut p: ClipPlacement) {
        self.revision = self.revision.wrapping_add(1);
        let old = self.placements.binary_search_by(|q| q.id.cmp(&p.id)).ok();
        // Revisions come from the table's counter: they only grow and never
        // repeat, also across undo / redo (a cache keyed on one never sees
        // two states under it).
        p.revision = self.revision;
        if let ClipId::Primary(t) = p.id {
            self.removed_primaries.retain(|&r| r != t);
        }
        let implicit = matches!(p.id, ClipId::Primary(_))
            && p.is_identity()
            && p.name.is_none()
            && p.color.is_none();
        match (old, implicit) {
            (Some(i), true) => {
                self.placements.remove(i);
            }
            (Some(i), false) => self.placements[i] = p,
            (None, true) => {}
            (None, false) => {
                let at = self.placements.partition_point(|q| q.id < p.id);
                self.placements.insert(at, p);
            }
        }
    }

    /// Removes placement `id` (a primary is recorded as removed). Returns the
    /// placement it was. An owned content left without a placement is
    /// removed too (returned second).
    pub fn remove_placement(
        &mut self,
        id: ClipId,
    ) -> Option<(ClipPlacement, Option<OwnedContent>)> {
        let p = self.placement(id)?;
        self.revision = self.revision.wrapping_add(1);
        self.placements.retain(|q| q.id != id);
        if let ClipId::Primary(t) = id
            && !self.removed_primaries.contains(&t)
        {
            self.removed_primaries.push(t);
            self.removed_primaries.sort();
        }
        let orphan = match p.content {
            ContentRef::Owned(c) if !self.placements.iter().any(|q| q.content == p.content) => {
                let i = self.contents.iter().position(|x| x.id == c)?;
                Some(self.contents.remove(i))
            }
            _ => None,
        };
        Some((p, orphan))
    }

    /// Removes every record of the tracks `ids` (placements hosted on them or
    /// showing their content, removed-primary marks) and the owned contents
    /// left without placements; returns what was removed.
    pub fn purge_tracks(&mut self, ids: &[TrackId]) -> PurgedClips {
        let mut out = PurgedClips::default();
        let hit = |p: &ClipPlacement| {
            ids.contains(&p.track) || matches!(p.content, ContentRef::Track(t) if ids.contains(&t))
        };
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.placements)
            .into_iter()
            .partition(|p| hit(p));
        self.placements = keep;
        out.placements = gone;
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.removed_primaries)
            .into_iter()
            .partition(|t| ids.contains(t));
        self.removed_primaries = keep;
        out.removed_primaries = gone;
        let placements = &self.placements;
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.contents)
            .into_iter()
            .partition(|c| {
                !placements
                    .iter()
                    .any(|p| p.content == ContentRef::Owned(c.id))
            });
        self.contents = keep;
        out.contents = gone.iter().map(|c| c.id).collect();
        if !out.placements.is_empty() || !out.removed_primaries.is_empty() {
            self.revision = self.revision.wrapping_add(1);
        }
        out
    }

    /// Whether every placement is the identity (no record at all).
    pub fn is_identity(&self) -> bool {
        self.is_empty()
    }

    /// Checks the table's invariants against the tracks that exist.
    pub fn validate(&self, track_exists: impl Fn(TrackId) -> bool) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for p in &self.placements {
            if !ids.insert(p.id) {
                return Err(format!("{} is placed twice", p.id));
            }
            if let ClipId::Placed(n) = p.id
                && n >= self.next_clip
            {
                return Err(format!(
                    "clip {n} is not below the next id {}",
                    self.next_clip
                ));
            }
            if !track_exists(p.track) {
                return Err(format!(
                    "{} sits on a missing track (id {})",
                    p.id, p.track.0
                ));
            }
            match p.content {
                ContentRef::Track(t) if !track_exists(t) => {
                    return Err(format!("{} shows a missing track (id {})", p.id, t.0));
                }
                ContentRef::Owned(c) if self.content(c).is_none() => {
                    return Err(format!("{} shows a missing {c}", p.id));
                }
                _ => {}
            }
            if let ClipId::Primary(t) = p.id
                && p.content != ContentRef::Track(t)
            {
                return Err(format!(
                    "the primary clip of track id {} shows another content",
                    t.0
                ));
            }
            if let Some(l) = p.length
                && (l == 0 || p.start.checked_add(l).is_none())
            {
                return Err(format!(
                    "{} has an invalid length ({l} ticks at tick {})",
                    p.id, p.start
                ));
            }
            if matches!(p.id, ClipId::Primary(_))
                && p.is_identity()
                && p.name.is_none()
                && p.color.is_none()
            {
                return Err(format!("{} is stored although it is unchanged", p.id));
            }
            check_label(p.name.as_deref(), p.color).map_err(|m| format!("{}: {m}", p.id))?;
        }
        let mut cids = std::collections::HashSet::new();
        for c in &self.contents {
            if !cids.insert(c.id) || c.id.0 >= self.next_content {
                return Err(format!(
                    "{} appears twice or is not below the next id",
                    c.id
                ));
            }
            if !self
                .placements
                .iter()
                .any(|p| p.content == ContentRef::Owned(c.id))
            {
                return Err(format!("{} is not placed anywhere", c.id));
            }
            check_label(c.name.as_deref(), c.color).map_err(|m| format!("{}: {m}", c.id))?;
        }
        for t in &self.removed_primaries {
            if !track_exists(*t) {
                return Err(format!(
                    "a removed clip belongs to a missing track (id {})",
                    t.0
                ));
            }
            if self.placements.iter().any(|p| p.id == ClipId::Primary(*t)) {
                return Err(format!(
                    "the primary clip of track id {} is both removed and placed",
                    t.0
                ));
            }
        }
        Ok(())
    }

    /// Feeds every record into `h` (fingerprints; nothing for an empty table).
    pub(crate) fn hash_into(&self, h: &mut impl FnMut(&[u8])) {
        if self.is_empty() {
            return;
        }
        let id = |c: ClipId| match c {
            ClipId::Primary(t) => (0u8, t.0),
            ClipId::Placed(n) => (1u8, n),
        };
        for p in &self.placements {
            let (k, n) = id(p.id);
            h(&[k]);
            h(&n.to_le_bytes());
            h(&p.track.0.to_le_bytes());
            match p.content {
                ContentRef::Track(t) => {
                    h(&[0]);
                    h(&t.0.to_le_bytes());
                }
                ContentRef::Owned(c) => {
                    h(&[1]);
                    h(&c.0.to_le_bytes());
                }
            }
            h(&p.start.to_le_bytes());
            h(&p.offset.to_le_bytes());
            h(&p.length.map_or(u64::MAX, |l| l).to_le_bytes());
            h(&[u8::from(p.muted)]);
            match &p.name {
                Some(n) => {
                    h(&(n.len() as u64 + 1).to_le_bytes());
                    h(n.as_bytes());
                }
                None => h(&0u64.to_le_bytes()),
            }
            h(&p.color.unwrap_or(u32::MAX).to_le_bytes());
        }
        for t in &self.removed_primaries {
            h(&t.0.to_le_bytes());
        }
        for c in &self.contents {
            h(&c.id.0.to_le_bytes());
            h(&(c.notes.len() as u64).to_le_bytes());
            for n in c.notes.iter() {
                h(&n.start_tick.to_le_bytes());
                h(&n.duration_ticks.to_le_bytes());
                h(&n.attrs.raw().to_le_bytes());
                h(&n.sub.0.to_le_bytes());
            }
            hash_events(h, &c.events);
        }
    }

    /// Feeds what the table stores beyond music into `h` (names and colours
    /// of contents, note groups, the next ids; nothing for an empty table) —
    /// `PlumDocument::state_fingerprint`.
    pub(crate) fn hash_state_into(&self, h: &mut impl FnMut(&[u8])) {
        if self.is_empty() {
            return;
        }
        h(&self.next_clip.to_le_bytes());
        h(&self.next_content.to_le_bytes());
        for c in &self.contents {
            h(&c.id.0.to_le_bytes());
            match &c.name {
                Some(n) => {
                    h(&(n.len() as u64 + 1).to_le_bytes());
                    h(n.as_bytes());
                }
                None => h(&0u64.to_le_bytes()),
            }
            h(&c.color.unwrap_or(u32::MAX).to_le_bytes());
            for n in c.notes.iter() {
                h(&n.group.to_le_bytes());
            }
        }
    }
}

/// A clip or content name has 1–[`MAX_CLIP_NAME_CHARS`] characters and is
/// not only spaces; a colour is 0xRRGGBB.
fn check_label(name: Option<&str>, color: Option<u32>) -> Result<(), String> {
    if let Some(n) = name
        && (n.trim().is_empty() || n.chars().count() > MAX_CLIP_NAME_CHARS)
    {
        return Err(format!(
            "a name must have 1 to {MAX_CLIP_NAME_CHARS} characters"
        ));
    }
    if let Some(c) = color
        && c > 0x00FF_FFFF
    {
        return Err(format!("color {c:#x} is not RGB"));
    }
    Ok(())
}

/// Feeds an event store into a hash, every event in full.
fn hash_events(h: &mut impl FnMut(&[u8]), events: &EventStore) {
    use super::events::RawKind;
    h(&(events.len() as u64).to_le_bytes());
    for e in events.events() {
        h(&e.tick.to_le_bytes());
        h(&e.anchor.to_le_bytes());
        match e.kind {
            RawKind::Channel { status, data } | RawKind::System { status, data } => {
                h(&[status, data[0], data[1]]);
            }
            RawKind::Meta { meta_type, payload } => {
                h(&[0xFF, meta_type]);
                h(&payload.len.to_le_bytes());
                h(events.payload(payload));
            }
            RawKind::SysEx { payload } => {
                h(&[0xF0]);
                h(&payload.len.to_le_bytes());
                h(events.payload(payload));
            }
            RawKind::SysExEscape { payload } => {
                h(&[0xF7]);
                h(&payload.len.to_le_bytes());
                h(events.payload(payload));
            }
        }
    }
}

/// Test documents and the flatten oracle shared by the consumers' tests
/// (playback, analysis, export, tiles — V09-S07).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::model::PlumDocument;
    use crate::model::events::{RawEvent, RawKind};
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord};
    use crate::model::project::Bpm;

    pub(crate) fn note(s: u64, d: u64, p: u8, ch: u8) -> NoteRecord {
        NoteRecord {
            start_tick: s,
            duration_ticks: d,
            attrs: NoteAttrs::new(p, 40 + p % 80, ch, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        }
    }

    pub(crate) fn cc(tick: u64, ch: u8, n: u8, v: u8) -> RawEvent {
        RawEvent {
            tick,
            anchor: 0,
            kind: RawKind::Channel {
                status: 0xB0 | ch,
                data: [n, v],
            },
        }
    }

    /// Three tracks of notes and controllers at 480 PPQN.
    pub(crate) fn base() -> PlumDocument {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        d.add_track().unwrap();
        for ti in 0..3usize {
            let recs: Vec<NoteRecord> = (0..48u64)
                .map(|k| note(k * 120, 60 + (k % 5) * 50, 48 + (k % 24) as u8, ti as u8))
                .collect();
            let t = d.track_mut(ti).unwrap();
            t.notes.insert_sorted_batch(&recs, false).unwrap();
            for k in 0..12u64 {
                t.events
                    .push(cc(k * 480, ti as u8, 7, (k * 10) as u8))
                    .unwrap();
            }
        }
        d
    }

    /// Every kind of placement: a moved + windowed primary, a linked instance
    /// on another track, a removed primary, an owned content placed twice (one
    /// muted), a named primary at the identity.
    pub(crate) fn clipped() -> PlumDocument {
        let mut d = base();
        let (t0, t1, t2) = (
            d.track(0).unwrap().id(),
            d.track(1).unwrap().id(),
            d.track(2).unwrap().id(),
        );
        let c = d.clips_mut();
        c.set_placement(ClipPlacement {
            start: 1000,
            offset: 600,
            length: Some(2400),
            ..ClipPlacement::primary(t0)
        });
        let link = c.new_clip_id();
        c.set_placement(ClipPlacement {
            id: link,
            track: t2,
            start: 7000,
            ..ClipPlacement::primary(t0)
        });
        c.remove_placement(ClipId::Primary(t1));
        let cid = c.new_content();
        for (i, (start, muted)) in [(200u64, false), (3000, true), (9000, false)]
            .into_iter()
            .enumerate()
        {
            let id = c.new_clip_id();
            c.set_placement(ClipPlacement {
                id,
                track: t1,
                content: ContentRef::Owned(cid),
                start,
                offset: if i == 2 { 240 } else { 0 },
                muted,
                ..ClipPlacement::primary(t1)
            });
        }
        let o = c.content_mut(cid).unwrap();
        let recs: Vec<NoteRecord> = (0..20u64)
            .map(|k| note(k * 60, 90, 70 + k as u8, 5))
            .collect();
        o.notes.insert_sorted_batch(&recs, false).unwrap();
        o.events.push(cc(0, 5, 10, 20)).unwrap();
        o.events.push(cc(480, 5, 10, 100)).unwrap();
        c.set_placement(ClipPlacement {
            name: Some("Named".into()),
            ..ClipPlacement::primary(t2)
        });
        d.validate_clips().unwrap();
        d
    }

    /// The same music in plain tracks: each host track holds the notes and
    /// channel events its unmuted placements play, at their song ticks.
    pub(crate) fn flatten(d: &PlumDocument) -> PlumDocument {
        let mut f = d.clone();
        for ti in 0..d.track_count() {
            let mut recs: Vec<NoteRecord> = Vec::new();
            let mut evs: Vec<RawEvent> = Vec::new();
            for pc in d.placed_contents(ti) {
                if pc.placement.muted {
                    continue;
                }
                let (lo, hi) = pc.window();
                let shift = pc.shift();
                for n in pc.notes.iter() {
                    if n.start_tick >= lo && n.start_tick < hi {
                        let mut r = n.record();
                        r.start_tick = (i128::from(r.start_tick) + shift) as u64;
                        recs.push(r);
                    }
                }
                for e in pc.events.events() {
                    if matches!(e.kind, RawKind::Channel { .. }) && e.tick >= lo && e.tick < hi {
                        let mut e = *e;
                        e.tick = (i128::from(e.tick) + shift) as u64;
                        evs.push(e);
                    }
                }
            }
            recs.sort_by_key(|r| r.start_tick);
            evs.sort_by_key(|e| e.tick);
            let t = f.track_mut(ti).unwrap();
            t.notes = NoteStore::from_sorted_records(&recs, false).unwrap();
            let mut es = EventStore::new();
            for e in evs {
                es.push(e).unwrap();
            }
            t.events = es;
        }
        *f.clips_mut() = ClipTable::default();
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u32) -> TrackId {
        TrackId(n)
    }

    #[test]
    fn primaries_are_implicit_identities_until_changed() {
        let mut c = ClipTable::default();
        assert!(c.is_empty() && c.is_identity());
        let p = c.placement(ClipId::Primary(t(3))).unwrap();
        assert!(p.is_identity());
        assert_eq!(c.placements_on(t(3)), vec![ClipPlacement::primary(t(3))]);
        // Moving the primary stores it; moving it back makes it implicit again.
        let mut moved = p.clone();
        moved.start = 960;
        c.set_placement(moved);
        assert!(!c.is_empty());
        assert_eq!(c.placement(ClipId::Primary(t(3))).unwrap().start, 960);
        assert_eq!(c.placement(ClipId::Primary(t(3))).unwrap().revision, 1);
        let mut back = c.placement(ClipId::Primary(t(3))).unwrap();
        back.start = 0;
        c.set_placement(back);
        assert!(c.is_empty(), "the identity is implicit again");
        assert!(c.validate(|_| true).is_ok());
    }

    #[test]
    fn windows_map_content_ticks_to_song_ticks() {
        let p = ClipPlacement {
            start: 1000,
            offset: 200,
            length: Some(500),
            ..ClipPlacement::primary(t(0))
        };
        assert_eq!(p.content_window(), (200, 700));
        assert!(!p.shows(199) && p.shows(200) && p.shows(699) && !p.shows(700));
        assert_eq!(p.song_tick(200), Some(1000));
        assert_eq!(p.song_tick(450), Some(1250));
        assert_eq!(p.song_tick(100), None);
        assert_eq!(p.song_span(9999), (1000, 1500));
        let open = ClipPlacement::primary(t(0));
        assert_eq!(open.song_span(4800), (0, 4800));
        assert!(open.shows(u64::MAX - 1));
        let far = ClipPlacement {
            start: u64::MAX - 5,
            ..ClipPlacement::primary(t(0))
        };
        assert_eq!(far.song_tick(10), None, "past the largest tick");
    }

    #[test]
    fn linked_instances_share_content_and_owned_contents_leave_with_their_last_clip() {
        let mut c = ClipTable::default();
        // A linked instance of track 1's own content on track 2.
        let link = c.new_clip_id();
        c.set_placement(ClipPlacement {
            id: link,
            track: t(2),
            start: 3840,
            ..ClipPlacement::primary(t(1))
        });
        assert_eq!(c.placements_of(ContentRef::Track(t(1))).len(), 2);
        assert_eq!(
            c.placements_on(t(2)).len(),
            2,
            "its own primary and the link"
        );
        // An owned content in two placements.
        let content = c.new_content();
        let a = c.new_clip_id();
        let b = c.new_clip_id();
        for (id, start) in [(a, 0), (b, 960)] {
            c.set_placement(ClipPlacement {
                id,
                track: t(1),
                content: ContentRef::Owned(content),
                start,
                ..ClipPlacement::primary(t(1))
            });
        }
        assert!(c.validate(|_| true).is_ok());
        assert!(
            c.remove_placement(a).unwrap().1.is_none(),
            "b still shows it"
        );
        let (_, orphan) = c.remove_placement(b).unwrap();
        assert_eq!(orphan.map(|o| o.id), Some(content));
        assert!(c.content(content).is_none());
        assert!(c.validate(|_| true).is_ok());
    }

    #[test]
    fn removed_primaries_hide_a_tracks_own_content() {
        let mut c = ClipTable::default();
        let (p, _) = c.remove_placement(ClipId::Primary(t(4))).unwrap();
        assert!(p.is_identity());
        assert!(c.placement(ClipId::Primary(t(4))).is_none());
        assert!(c.placements_on(t(4)).is_empty());
        assert_eq!(c.removed_primaries(), &[t(4)]);
        // Placing it again restores it.
        c.set_placement(ClipPlacement::primary(t(4)));
        assert!(c.is_empty());
    }

    #[test]
    fn purging_tracks_removes_their_records_and_returns_them() {
        let mut c = ClipTable::default();
        let link = c.new_clip_id();
        c.set_placement(ClipPlacement {
            id: link,
            track: t(2),
            ..ClipPlacement::primary(t(1))
        });
        c.remove_placement(ClipId::Primary(t(3)));
        let rev = c.revision();
        let purged = c.purge_tracks(&[t(1), t(3)]);
        assert_eq!(purged.placements.len(), 1, "the link shows track 1");
        assert_eq!(purged.removed_primaries, vec![t(3)]);
        assert!(c.is_empty());
        assert!(c.revision() > rev);
        assert_eq!(c.purge_tracks(&[t(9)]), PurgedClips::default());
    }

    #[test]
    fn invalid_tables_are_reported() {
        let mut c = ClipTable::default();
        let id = c.new_clip_id();
        c.set_placement(ClipPlacement {
            id,
            track: t(7),
            ..ClipPlacement::primary(t(1))
        });
        assert!(
            c.validate(|x| x != t(7))
                .unwrap_err()
                .contains("missing track")
        );
        assert!(
            c.validate(|x| x != t(1))
                .unwrap_err()
                .contains("shows a missing track")
        );
        let mut bad = c.placement(id).unwrap();
        bad.length = Some(0);
        c.set_placement(bad);
        assert!(
            c.validate(|_| true)
                .unwrap_err()
                .contains("invalid length (0 ticks")
        );
        let mut named = c.placement(id).unwrap();
        named.length = None;
        named.name = Some("   ".into());
        c.set_placement(named.clone());
        assert!(
            c.validate(|_| true)
                .unwrap_err()
                .contains("1 to 200 characters")
        );
        named.name = Some("x".repeat(MAX_CLIP_NAME_CHARS + 1));
        c.set_placement(named.clone());
        assert!(
            c.validate(|_| true)
                .unwrap_err()
                .contains("1 to 200 characters")
        );
        named.name = Some("Lead".into());
        named.color = Some(0x0100_0000);
        c.set_placement(named.clone());
        assert!(c.validate(|_| true).unwrap_err().contains("is not RGB"));
        named.color = Some(0x00FF_8000);
        c.set_placement(named);
        assert!(c.validate(|_| true).is_ok());
    }

    #[test]
    fn tables_from_stored_records_must_be_in_id_order() {
        let p = |id, track| ClipPlacement {
            id,
            start: 480,
            ..ClipPlacement::primary(t(track))
        };
        let ok = ClipTable::from_parts(
            Vec::new(),
            vec![p(ClipId::Primary(t(2)), 2), p(ClipId::Primary(t(5)), 5)],
            vec![t(1), t(3)],
            0,
            0,
        )
        .unwrap();
        assert_eq!(ok.explicit_placements().len(), 2);
        assert_eq!(ok.next_ids(), (0, 0));
        assert!(ok.validate(|_| true).is_ok());
        assert!(
            ClipTable::from_parts(
                Vec::new(),
                vec![p(ClipId::Primary(t(5)), 5), p(ClipId::Primary(t(2)), 2)],
                Vec::new(),
                0,
                0
            )
            .is_err()
        );
        assert!(ClipTable::from_parts(Vec::new(), Vec::new(), vec![t(3), t(3)], 0, 0).is_err());
        assert!(
            ClipTable::from_parts(
                vec![
                    OwnedContent::new(ContentId(1)),
                    OwnedContent::new(ContentId(0))
                ],
                Vec::new(),
                Vec::new(),
                0,
                2
            )
            .is_err()
        );
    }

    // ---------------------------------------------------------- in a document

    use crate::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId};

    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("clips".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        let notes: Vec<NoteRecord> = (0..8u64)
            .map(|i| NoteRecord {
                start_tick: i * 240,
                duration_ticks: 120,
                attrs: NoteAttrs::new(60 + i as u8, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                sub: SubchannelId(0),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&notes, false)
            .unwrap();
        d
    }

    /// §040: a v0.8 document is one identity clip per track, costs nothing and
    /// fingerprints as before (an empty table adds nothing).
    #[test]
    fn a_v08_document_is_one_identity_clip_per_track() {
        let d = doc();
        assert!(d.clips_are_identity());
        assert!(d.validate_clips().is_ok());
        for i in 0..d.track_count() {
            let id = d.track(i).unwrap().id();
            assert_eq!(d.placements_on(i), vec![ClipPlacement::primary(id)]);
        }
        let mut e = d.clone();
        let f = d.fingerprint();
        // Changing a primary and changing it back leaves the document as it was.
        let id = d.track(0).unwrap().id();
        let mut p = e.clips().placement(ClipId::Primary(id)).unwrap();
        p.start = 480;
        e.clips_mut().set_placement(p.clone());
        assert_ne!(e.fingerprint(), f, "where a clip plays is music");
        p.start = 0;
        e.clips_mut().set_placement(p);
        assert_eq!(e.fingerprint(), f);
        assert_eq!(e.state_fingerprint(), d.state_fingerprint());
    }

    /// §272–§274: moving a clip changes the placement and analysis revisions
    /// but not the content revision; editing notes changes the content and
    /// analysis revisions but not the placement revision.
    #[test]
    fn content_placement_and_analysis_revisions_are_separate() {
        let mut d = doc();
        let id = d.track(0).unwrap().id();
        let content = ContentRef::Track(id);
        let (c0, p0, a0) = (
            d.content_revision(content).unwrap(),
            d.clips().revision(),
            d.analysis_revision(),
        );
        // Move the clip.
        let mut p = d.clips().placement(ClipId::Primary(id)).unwrap();
        p.start = 3840;
        d.clips_mut().set_placement(p);
        let (c1, p1, a1) = (
            d.content_revision(content).unwrap(),
            d.clips().revision(),
            d.analysis_revision(),
        );
        assert_eq!(c1, c0, "moving does not touch the content");
        assert_ne!(p1, p0);
        assert_ne!(a1, a0, "global NPS / poly may change");
        // Edit a note.
        let extra = NoteRecord {
            start_tick: 5000,
            duration_ticks: 10,
            attrs: NoteAttrs::new(70, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        };
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[extra], false)
            .unwrap();
        assert_ne!(d.content_revision(content).unwrap(), c1);
        assert_eq!(d.clips().revision(), p1, "no placement changed");
        assert_ne!(d.analysis_revision(), a1);
        // An owned content has its own revision.
        let oc = d.clips_mut().new_content();
        let clip = d.clips_mut().new_clip_id();
        d.clips_mut().set_placement(ClipPlacement {
            id: clip,
            track: id,
            content: ContentRef::Owned(oc),
            start: 0,
            ..ClipPlacement::primary(id)
        });
        let r = d.content_revision(ContentRef::Owned(oc)).unwrap();
        d.clips_mut()
            .content_mut(oc)
            .unwrap()
            .notes
            .insert_sorted_batch(&[extra], false)
            .unwrap();
        assert_ne!(d.content_revision(ContentRef::Owned(oc)).unwrap(), r);
        assert!(d.validate_clips().is_ok());
        assert!(
            d.content_revision(ContentRef::Owned(ContentId(99)))
                .is_none()
        );
    }

    /// Selection changes are not content changes.
    #[test]
    fn selecting_notes_changes_no_clip_revision() {
        let mut d = doc();
        let id = d.track(0).unwrap().id();
        let (c, a) = (
            d.content_revision(ContentRef::Track(id)),
            d.analysis_revision(),
        );
        d.track_mut(0).unwrap().notes.select_all();
        assert_eq!(d.content_revision(ContentRef::Track(id)), c);
        assert_eq!(d.analysis_revision(), a);
    }
}
