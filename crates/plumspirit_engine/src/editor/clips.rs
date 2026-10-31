//! Clip edits (v0.9 §050, V09-S07) planned as undoable operations.
//!
//! Every edit reads the document's clip table, builds the layout it should
//! have afterwards and returns one [`Operation::SetClips`] — one undo step,
//! journaled like every edit. Nothing here touches the document; the history
//! applies the layout and validates it (`ClipTable::validate`). Nothing is
//! copied per note: a linked duplicate is one more placement of the same
//! content; an independent duplicate shares the copied stores' chunks until
//! one side is edited (copy-on-write).

use crate::history::Operation;
use crate::model::events::{EventStore, RawKind};
use crate::model::{
    ClipId, ClipLayout, ClipPlacement, ContentId, ContentRef, OwnedContent, PlumDocument, TrackId,
};

/// Why a clip edit cannot be planned (a full sentence for the status bar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipEditError(pub String);

impl std::fmt::Display for ClipEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) type Planned = Result<Operation, ClipEditError>;

fn err<T>(m: &str) -> Result<T, ClipEditError> {
    Err(ClipEditError(m.into()))
}

pub(crate) fn track_id(doc: &PlumDocument, index: usize) -> Result<TrackId, ClipEditError> {
    doc.track(index)
        .map(|t| t.id())
        .ok_or_else(|| ClipEditError("That track does not exist.".into()))
}

/// The placement `id` as it is now.
pub(crate) fn placement(doc: &PlumDocument, id: ClipId) -> Result<ClipPlacement, ClipEditError> {
    doc.clips()
        .placement(id)
        .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))
}

/// Puts `p` into `layout` (replacing the record with its id). A primary back
/// at the identity without name or colour is implicit again.
pub(crate) fn put(layout: &mut ClipLayout, p: ClipPlacement) {
    if let ClipId::Primary(t) = p.id {
        layout.removed_primaries.retain(|&r| r != t);
    }
    let implicit = matches!(p.id, ClipId::Primary(_))
        && p.is_identity()
        && p.name.is_none()
        && p.color.is_none();
    // The placements are sorted by id: a binary search instead of a scan per
    // record (B1-0164: duplicating 10,000 clips in one step was quadratic).
    match layout.placements.binary_search_by(|q| q.id.cmp(&p.id)) {
        Ok(at) if implicit => {
            layout.placements.remove(at);
        }
        Ok(at) => layout.placements[at] = p,
        Err(_) if implicit => {}
        Err(at) => layout.placements.insert(at, p),
    }
}

/// Removes placement `id` from `layout` (a primary is recorded as removed)
/// and any owned content left without a placement.
pub(crate) fn take(layout: &mut ClipLayout, id: ClipId) {
    layout.placements.retain(|q| q.id != id);
    if let ClipId::Primary(t) = id
        && let Err(at) = layout.removed_primaries.binary_search(&t)
    {
        layout.removed_primaries.insert(at, t);
    }
    let placements = &layout.placements;
    layout
        .contents
        .retain(|&c| placements.iter().any(|p| p.content == ContentRef::Owned(c)));
}

pub(crate) fn new_clip_id(layout: &mut ClipLayout) -> Result<ClipId, ClipEditError> {
    let n = layout.next_clip;
    layout.next_clip = n
        .checked_add(1)
        .ok_or_else(|| ClipEditError("No more clips can be added to this project.".into()))?;
    Ok(ClipId::Placed(n))
}

pub(crate) fn new_content_id(layout: &mut ClipLayout) -> Result<ContentId, ClipEditError> {
    let n = layout.next_content;
    layout.next_content = n
        .checked_add(1)
        .ok_or_else(|| ClipEditError("No more clips can be added to this project.".into()))?;
    let id = ContentId(n);
    let at = layout.contents.partition_point(|&c| c < id);
    layout.contents.insert(at, id);
    Ok(id)
}

pub(crate) fn set_clips(
    layout: ClipLayout,
    contents: Vec<OwnedContent>,
    label: &'static str,
) -> Operation {
    Operation::SetClips {
        layout: Box::new(layout),
        contents,
        label,
    }
}

/// The song tick span a placement covers.
pub fn clip_span(doc: &PlumDocument, p: &ClipPlacement) -> (u64, u64) {
    let end = doc
        .content_revision(p.content)
        .map(|_| content_end(doc, p.content))
        .unwrap_or(0);
    p.song_span(end)
}

/// Whether placement `p` is a clip: it spans at least one song tick (a new
/// track's empty primary is none).
pub fn is_clip(doc: &PlumDocument, p: &ClipPlacement) -> bool {
    let (a, b) = clip_span(doc, p);
    b > a
}

/// Notes placement `p` plays (its content's notes starting in its window).
fn plays(doc: &PlumDocument, p: &ClipPlacement) -> usize {
    let Some(notes) = doc.notes_of(p.content) else {
        return 0;
    };
    let (lo, hi) = p.content_window();
    match (lo, hi) {
        _ if hi <= lo => 0,
        (0, u64::MAX) => notes.len(),
        _ => notes.count_starting_in(lo, hi - 1),
    }
}

/// The clip track `track`'s notes are edited in when no clip is opened
/// (v1.0-beta1 B1-0096): the first clip on the track that plays notes, else
/// its first clip (its own primary first, then by start). `None`: the
/// track's own primary at the identity, or no clip — its own notes at
/// shift 0, as before clips. The Piano Roll's default and the destination
/// of tools that write to another track (B2-F16) read it.
pub fn default_pick(doc: &PlumDocument, track: usize) -> Option<ClipPlacement> {
    let own = doc.track(track)?.id();
    let hosted: Vec<ClipPlacement> = doc
        .placements_on(track)
        .into_iter()
        .filter(|p| is_clip(doc, p))
        .collect();
    let pick = hosted
        .iter()
        .find(|p| plays(doc, p) > 0)
        .or(hosted.first())?
        .clone();
    let identity = pick.id == ClipId::Primary(own) && pick.start == 0 && pick.offset == 0;
    (!identity).then_some(pick)
}

fn content_end(doc: &PlumDocument, c: ContentRef) -> u64 {
    match c {
        ContentRef::Track(t) => doc.track_by_id(t).map_or(0, |t| t.content_only_end_tick()),
        ContentRef::Owned(id) => doc.clips().content(id).map_or(0, OwnedContent::end_tick),
    }
}

/// Add Piano Roll Clip: a new empty clip on track `track` at `start`, `length`
/// ticks long (≥ 1).
pub fn add_clip(doc: &PlumDocument, track: usize, start: u64, length: u64) -> Planned {
    let host = track_id(doc, track)?;
    if length == 0 || start.checked_add(length).is_none() {
        return err("A clip needs a length of at least one tick.");
    }
    let mut layout = doc.clips().layout();
    let content = new_content_id(&mut layout)?;
    let id = new_clip_id(&mut layout)?;
    put(
        &mut layout,
        ClipPlacement {
            id,
            track: host,
            content: ContentRef::Owned(content),
            start,
            offset: 0,
            length: Some(length),
            muted: false,
            name: None,
            color: None,
            revision: 0,
        },
    );
    Ok(set_clips(
        layout,
        vec![OwnedContent::new(content)],
        "Add Piano Roll Clip",
    ))
}

/// Move Clips: each clip to a track and a start (its window is kept).
pub fn move_clips(doc: &PlumDocument, moves: &[(ClipId, usize, u64)]) -> Planned {
    let mut layout = doc.clips().layout();
    for &(id, track, start) in moves {
        let mut p = placement(doc, id)?;
        p.track = track_id(doc, track)?;
        if let Some(l) = p.length
            && start.checked_add(l).is_none()
        {
            return err("A clip cannot be moved that far.");
        }
        p.start = start;
        put(&mut layout, p);
    }
    Ok(set_clips(
        layout,
        Vec::new(),
        if moves.len() == 1 {
            "Move Clip"
        } else {
            "Move Clips"
        },
    ))
}

/// Resize Clip: the window `[offset, offset + length)` of the content shown
/// from song tick `start` (`length: None` = to the content's end). Trimming
/// the left edge moves `start` and `offset` together.
pub fn resize_clip(
    doc: &PlumDocument,
    id: ClipId,
    start: u64,
    offset: u64,
    length: Option<u64>,
) -> Planned {
    let mut p = placement(doc, id)?;
    if length.is_some_and(|l| l == 0 || start.checked_add(l).is_none()) {
        return err("A clip needs a length of at least one tick.");
    }
    p.start = start;
    p.offset = offset;
    p.length = length;
    let mut layout = doc.clips().layout();
    put(&mut layout, p);
    Ok(set_clips(layout, Vec::new(), "Resize Clip"))
}

/// Mute Clips / Unmute Clips: a muted clip is shown but does not play,
/// export or count.
pub fn set_muted(doc: &PlumDocument, ids: &[ClipId], muted: bool) -> Planned {
    let mut layout = doc.clips().layout();
    for &id in ids {
        let mut p = placement(doc, id)?;
        p.muted = muted;
        put(&mut layout, p);
    }
    Ok(set_clips(
        layout,
        Vec::new(),
        if muted { "Mute Clips" } else { "Unmute Clips" },
    ))
}

/// Rename Clip (`None` clears the name).
pub fn rename_clip(doc: &PlumDocument, id: ClipId, name: Option<String>) -> Planned {
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if name
        .as_ref()
        .is_some_and(|n| n.chars().count() > crate::model::clip::MAX_CLIP_NAME_CHARS)
    {
        return err("A clip name can have at most 200 characters.");
    }
    let mut p = placement(doc, id)?;
    p.name = name;
    let mut layout = doc.clips().layout();
    put(&mut layout, p);
    Ok(set_clips(layout, Vec::new(), "Rename Clip"))
}

/// Clip Color (`None` = the track's colour).
pub fn recolor_clips(doc: &PlumDocument, ids: &[ClipId], color: Option<[u8; 3]>) -> Planned {
    let mut layout = doc.clips().layout();
    for &id in ids {
        let mut p = placement(doc, id)?;
        p.color = color.map(|[r, g, b]| u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b));
        put(&mut layout, p);
    }
    Ok(set_clips(layout, Vec::new(), "Clip Color"))
}

/// Delete Clips: a primary clip is removed (its track's notes stay, unplaced
/// until it is restored); an owned content goes with its last clip.
pub fn delete_clips(doc: &PlumDocument, ids: &[ClipId]) -> Planned {
    let mut layout = doc.clips().layout();
    for &id in ids {
        placement(doc, id)?;
        take(&mut layout, id);
    }
    Ok(set_clips(
        layout,
        Vec::new(),
        if ids.len() == 1 {
            "Delete Clip"
        } else {
            "Delete Clips"
        },
    ))
}

/// Duplicate Clips (§050). `linked`: each copy is another placement of the
/// same content — editing either edits both. Otherwise each copy gets its own
/// content (an independent copy of the notes and channel events) — editing
/// one never changes the other. Copies go to `(track, start)` per clip.
pub fn duplicate_clips(
    doc: &PlumDocument,
    copies: &[(ClipId, usize, u64)],
    linked: bool,
) -> Planned {
    let mut layout = doc.clips().layout();
    let mut contents = Vec::new();
    for &(id, track, start) in copies {
        let src = placement(doc, id)?;
        let host = track_id(doc, track)?;
        if src.length.is_some_and(|l| start.checked_add(l).is_none()) {
            return err("A clip cannot be placed that far.");
        }
        let content = if linked {
            src.content
        } else {
            let cid = new_content_id(&mut layout)?;
            contents.push(copy_content(doc, src.content, cid)?);
            ContentRef::Owned(cid)
        };
        let new_id = new_clip_id(&mut layout)?;
        put(
            &mut layout,
            ClipPlacement {
                id: new_id,
                track: host,
                content,
                start,
                revision: 0,
                ..src
            },
        );
    }
    Ok(set_clips(
        layout,
        contents,
        if linked {
            "Duplicate Clips (Linked)"
        } else {
            "Duplicate Clips"
        },
    ))
}

/// B1-0097: Slice Clips at song tick `at` (the edit cursor). Every listed
/// clip that `at` falls strictly inside becomes two clips: the left part keeps
/// its id with its window ending at `at`, the right part is a new clip of the
/// same content (the same notes — like a linked copy) starting at `at` with
/// the rest of the window. Nothing is cut from the content: dragging either
/// edge back shows every note again. One undo step; refused when `at` is in
/// none of them.
pub fn slice_clips(doc: &PlumDocument, ids: &[ClipId], at: u64) -> Planned {
    let mut layout = doc.clips().layout();
    let mut sliced = 0usize;
    for &id in ids {
        let p = placement(doc, id)?;
        let (s, e) = clip_span(doc, &p);
        if !(s < at && at < e) {
            continue;
        }
        let left_len = at - s;
        let right_offset = p
            .offset
            .checked_add(left_len)
            .ok_or_else(|| ClipEditError("A clip cannot be sliced that far.".into()))?;
        let right = ClipPlacement {
            id: new_clip_id(&mut layout)?,
            start: at,
            offset: right_offset,
            length: p.length.map(|l| l - left_len),
            revision: 0,
            ..p.clone()
        };
        let left = ClipPlacement {
            length: Some(left_len),
            ..p
        };
        put(&mut layout, left);
        put(&mut layout, right);
        sliced += 1;
    }
    if sliced == 0 {
        return err("Put the edit cursor inside a selected clip to slice it there.");
    }
    Ok(set_clips(
        layout,
        Vec::new(),
        if sliced == 1 {
            "Slice Clip"
        } else {
            "Slice Clips"
        },
    ))
}

/// B1-0097: clips copied in the Track List (Copy / Cut), kept by the app.
/// Each entry holds its own copy of the clip's content (chunks shared with
/// the source until either side changes), so a Paste works after the source
/// was cut, deleted or edited.
#[derive(Clone, Debug, Default)]
pub struct ClipClipboard {
    pub entries: Vec<ClipEntry>,
}

/// One copied clip: where it was relative to the copied block (track rows
/// below the block's first track, ticks after its first start) and its
/// window, name, color, mute and content.
#[derive(Clone, Debug)]
pub struct ClipEntry {
    pub rows_below: usize,
    pub after: u64,
    pub offset: u64,
    pub length: Option<u64>,
    pub muted: bool,
    pub name: Option<String>,
    pub color: Option<u32>,
    pub content: OwnedContent,
}

impl ClipClipboard {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Track rows the block spans (its first track counts as one).
    pub fn rows(&self) -> usize {
        self.entries
            .iter()
            .map(|e| e.rows_below + 1)
            .max()
            .unwrap_or(0)
    }
}

/// B1-0097: Copy Clips — the clips `ids` with their contents.
pub fn copy_clips(doc: &PlumDocument, ids: &[ClipId]) -> Result<ClipClipboard, ClipEditError> {
    let mut picked = Vec::with_capacity(ids.len());
    for &id in ids {
        let p = placement(doc, id)?;
        let row = doc
            .index_of(p.track)
            .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
        picked.push((row, p));
    }
    if picked.is_empty() {
        return err("Select the clips to copy first.");
    }
    let top = picked.iter().map(|(r, _)| *r).min().unwrap_or(0);
    let first = picked.iter().map(|(_, p)| p.start).min().unwrap_or(0);
    let mut entries = Vec::with_capacity(picked.len());
    for (row, p) in picked {
        entries.push(ClipEntry {
            rows_below: row - top,
            after: p.start - first,
            offset: p.offset,
            length: p.length,
            muted: p.muted,
            name: p.name.clone(),
            color: p.color,
            content: copy_content(doc, p.content, ContentId(0))?,
        });
    }
    Ok(ClipClipboard { entries })
}

/// B1-0097: Paste Clips — independent copies of the clipboard's clips with
/// the block's first clip at song tick `at` on track `track`, the others on
/// the same rows and ticks relative to it. Each paste gets new contents
/// (editing a pasted clip never changes another); one undo step. Refused
/// when the block needs more rows than there are below `track`.
pub fn paste_clips(doc: &PlumDocument, clip: &ClipClipboard, track: usize, at: u64) -> Planned {
    if clip.is_empty() {
        return err("Copy clips first: the clip clipboard is empty.");
    }
    if track + clip.rows() > doc.track_count() {
        return err(&format!(
            "The copied clips need {} track rows from the active track down; there are {}.",
            clip.rows(),
            doc.track_count().saturating_sub(track)
        ));
    }
    let mut layout = doc.clips().layout();
    let mut contents = Vec::with_capacity(clip.entries.len());
    for e in &clip.entries {
        let host = track_id(doc, track + e.rows_below)?;
        let start = at
            .checked_add(e.after)
            .filter(|s| e.length.is_none_or(|l| s.checked_add(l).is_some()))
            .ok_or_else(|| ClipEditError("A clip cannot be placed that far.".into()))?;
        let cid = new_content_id(&mut layout)?;
        let mut content = e.content.clone();
        content.id = cid;
        contents.push(content);
        let id = new_clip_id(&mut layout)?;
        put(
            &mut layout,
            ClipPlacement {
                id,
                track: host,
                content: ContentRef::Owned(cid),
                start,
                offset: e.offset,
                length: e.length,
                muted: e.muted,
                name: e.name.clone(),
                color: e.color,
                revision: 0,
            },
        );
    }
    Ok(set_clips(
        layout,
        contents,
        if clip.entries.len() == 1 {
            "Paste Clip"
        } else {
            "Paste Clips"
        },
    ))
}

/// An independent copy of content `c` with id `id`: its notes (chunks shared
/// until either side changes) and its channel events.
pub(crate) fn copy_content(
    doc: &PlumDocument,
    c: ContentRef,
    id: ContentId,
) -> Result<OwnedContent, ClipEditError> {
    let (notes, events, name, color) = match c {
        ContentRef::Track(t) => {
            let t = doc
                .track_by_id(t)
                .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
            // The copy keeps its source's name (V09-S08: it no longer shows
            // its track's notes, so the Track List would otherwise name it
            // after the row it sits on).
            let name = t
                .name()
                .map(|n| {
                    n.trim()
                        .chars()
                        .take(crate::model::clip::MAX_CLIP_NAME_CHARS)
                        .collect::<String>()
                })
                .filter(|n| !n.is_empty());
            (t.notes.clone(), &t.events, name, None)
        }
        ContentRef::Owned(o) => {
            let o = doc
                .clips()
                .content(o)
                .ok_or_else(|| ClipEditError("That clip no longer exists.".into()))?;
            (o.notes.clone(), &o.events, o.name.clone(), o.color)
        }
    };
    let mut out = OwnedContent::new(id);
    out.notes = notes;
    out.notes.clear_selection();
    out.events = channel_events(events)
        .map_err(|_| ClipEditError("The clip's events could not be copied.".into()))?;
    out.name = name;
    out.color = color;
    Ok(out)
}

/// The channel messages of `events` (controllers, pitch bend, program …) —
/// what a clip content holds besides notes.
fn channel_events(
    events: &EventStore,
) -> Result<EventStore, crate::model::events::EventStoreError> {
    let mut out = EventStore::new();
    for e in events.events() {
        if let RawKind::Channel { .. } = e.kind {
            out.push(*e)?;
        }
    }
    out.shrink_to_fit();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn note(t: u64, p: u8) -> NoteRecord {
        NoteRecord::new(
            t,
            100,
            NoteAttrs::new(p, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
        )
    }

    /// §050 both ways: editing an independent copy never changes the source,
    /// and editing the source never changes the copy; the copy shares the
    /// source's chunks until then (nothing copied per note).
    #[test]
    fn independent_copies_are_independent_in_both_directions() {
        let mut doc = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..100u64).map(|k| note(k * 30, 60)).collect();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let t0 = doc.track(0).unwrap().id();
        let mut h = History::default();
        let op = duplicate_clips(&doc, &[(ClipId::Primary(t0), 0, 9600)], false).unwrap();
        h.apply(&mut doc, op).unwrap();
        let src = |d: &PlumDocument| d.track(0).unwrap().notes.len();
        let copy = |d: &PlumDocument| d.clips().content(ContentId(0)).unwrap().notes.len();
        assert_eq!((src(&doc), copy(&doc)), (100, 100));
        // The project stores both (what a save writes); a linked clip adds
        // nothing (S08 GUI journey: the status bar showed only the tracks').
        assert_eq!((doc.note_count(), doc.stored_note_count()), (100, 200));
        let op = duplicate_clips(&doc, &[(ClipId::Primary(t0), 0, 19200)], true).unwrap();
        h.apply(&mut doc, op).unwrap();
        assert_eq!(doc.stored_note_count(), 200);
        h.undo(&mut doc).unwrap();
        // Edit the copy (as the clip Piano Roll will, V09-S11).
        doc.clips_mut()
            .content_mut(ContentId(0))
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(5, 70)], false)
            .unwrap();
        assert_eq!((src(&doc), copy(&doc)), (100, 101));
        // Edit the source.
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(7, 71), note(8, 72)], false)
            .unwrap();
        assert_eq!((src(&doc), copy(&doc)), (102, 101));
    }

    /// An independent copy of a track's clip is named after the track (it no
    /// longer shows that track's notes); an unnamed track gives no name, a
    /// long name is cut to the clip-name limit.
    #[test]
    fn an_independent_copy_keeps_its_source_tracks_name() {
        let mut doc = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 960);
        doc.add_track().unwrap();
        let (t0, t1) = (doc.track(0).unwrap().id(), doc.track(1).unwrap().id());
        let name_track = |doc: &mut PlumDocument, i: usize, name: &str| {
            use crate::model::events::{RawEvent, RawKind, meta};
            let t = doc.track_mut(i).unwrap();
            let payload = t.events.store_payload(name.as_bytes()).unwrap();
            t.events
                .push(RawEvent {
                    tick: 0,
                    anchor: 0,
                    kind: RawKind::Meta {
                        meta_type: meta::TRACK_NAME,
                        payload,
                    },
                })
                .unwrap();
            t.refresh_name();
        };
        name_track(&mut doc, 0, "Bass");
        name_track(&mut doc, 1, &"x".repeat(300));
        let mut h = History::default();
        let op = duplicate_clips(
            &doc,
            &[
                (ClipId::Primary(t0), 1, 9600),
                (ClipId::Primary(t1), 0, 9600),
            ],
            false,
        )
        .unwrap();
        h.apply(&mut doc, op).unwrap();
        let c = doc.clips().contents();
        assert_eq!(c[0].name.as_deref(), Some("Bass"));
        assert_eq!(
            c[1].name.as_ref().unwrap().chars().count(),
            crate::model::clip::MAX_CLIP_NAME_CHARS
        );
        doc.validate_clips().unwrap();
        let mut d2 = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 960);
        let t = d2.track(0).unwrap().id();
        let op = duplicate_clips(&d2, &[(ClipId::Primary(t), 0, 960)], false).unwrap();
        History::default().apply(&mut d2, op).unwrap();
        assert_eq!(d2.clips().contents()[0].name, None);
    }

    /// Placing a primary back at the identity without a name or colour leaves
    /// no record (the table is the v0.8 model again).
    #[test]
    fn moving_a_clip_back_leaves_no_record() {
        let mut doc = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 960);
        let t0 = doc.track(0).unwrap().id();
        let mut h = History::default();
        let op = move_clips(&doc, &[(ClipId::Primary(t0), 0, 480)]).unwrap();
        h.apply(&mut doc, op).unwrap();
        assert!(!doc.clips_are_identity());
        let op = move_clips(&doc, &[(ClipId::Primary(t0), 0, 0)]).unwrap();
        h.apply(&mut doc, op).unwrap();
        assert!(doc.clips_are_identity());
    }

    fn played(d: &PlumDocument) -> usize {
        (0..d.track_count())
            .map(|i| {
                d.placed_contents(i)
                    .iter()
                    .map(|c| c.note_count())
                    .sum::<usize>()
            })
            .sum()
    }

    fn two_tracks() -> PlumDocument {
        let mut doc = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 960);
        doc.add_track().unwrap();
        for (i, n) in [(0usize, 100u64), (1, 50)] {
            let recs: Vec<NoteRecord> = (0..n).map(|k| note(k * 30, 60 + i as u8)).collect();
            doc.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        doc
    }

    /// B1-0097: Slice at the edit cursor keeps every note playing exactly
    /// once (two windows of the same content, nothing copied or cut), and
    /// one Undo brings the single clip back.
    #[test]
    fn slicing_a_clip_keeps_every_note_once_and_undo_restores_it() {
        let mut doc = two_tracks();
        let t0 = doc.track(0).unwrap().id();
        let mut h = History::default();
        let op = slice_clips(&doc, &[ClipId::Primary(t0)], 1500).unwrap();
        assert_eq!(op.label(), "Slice Clip");
        h.apply(&mut doc, op).unwrap();
        doc.validate_clips().unwrap();
        let on = doc.clips().placements_on(t0);
        assert_eq!(on.len(), 2);
        let (l, r) = (&on[0], &on[1]);
        assert_eq!((l.start, l.offset, l.length), (0, 0, Some(1500)));
        assert_eq!((r.start, r.offset, r.length), (1500, 1500, None));
        assert_eq!(r.content, l.content, "the same notes");
        assert_eq!(played(&doc), 150);
        assert_eq!(doc.stored_note_count(), 150, "nothing copied");
        // Slicing the right part again, and a cursor outside every clip.
        let op = slice_clips(&doc, &[r.id], 2400).unwrap();
        h.apply(&mut doc, op).unwrap();
        assert_eq!(played(&doc), 150);
        assert!(slice_clips(&doc, &[ClipId::Primary(t0)], 0).is_err());
        assert!(slice_clips(&doc, &[ClipId::Primary(t0)], 1_000_000).is_err());
        h.undo(&mut doc).unwrap();
        h.undo(&mut doc).unwrap();
        assert!(doc.clips_are_identity());
    }

    /// B1-0097: Cut (Copy, then Delete) and Paste: the pasted clips are
    /// independent copies on the same relative rows and ticks, they play
    /// the same notes, and a block that needs more rows is refused.
    #[test]
    fn cut_and_paste_clips_as_independent_copies_on_relative_rows() {
        let mut doc = two_tracks();
        let (t0, t1) = (doc.track(0).unwrap().id(), doc.track(1).unwrap().id());
        let mut h = History::default();
        let op = move_clips(&doc, &[(ClipId::Primary(t1), 1, 480)]).unwrap();
        h.apply(&mut doc, op).unwrap();
        let ids = [ClipId::Primary(t0), ClipId::Primary(t1)];
        let clip = copy_clips(&doc, &ids).unwrap();
        assert_eq!(clip.rows(), 2);
        assert_eq!(clip.entries[1].after, 480);
        let op = delete_clips(&doc, &ids).unwrap();
        h.apply(&mut doc, op).unwrap();
        assert_eq!(played(&doc), 0);
        assert!(paste_clips(&doc, &clip, 1, 0).is_err(), "needs two rows");
        let op = paste_clips(&doc, &clip, 0, 9600).unwrap();
        assert_eq!(op.label(), "Paste Clips");
        h.apply(&mut doc, op).unwrap();
        doc.validate_clips().unwrap();
        assert_eq!(played(&doc), 150);
        let p0 = doc.clips().placements_on(t0);
        let p1 = doc.clips().placements_on(t1);
        assert_eq!((p0.len(), p1.len()), (1, 1));
        assert_eq!((p0[0].start, p1[0].start), (9600, 10_080));
        assert!(matches!(p0[0].content, ContentRef::Owned(_)));
        assert_ne!(p0[0].content, p1[0].content);
        // One undo step each.
        h.undo(&mut doc).unwrap();
        assert_eq!(played(&doc), 0);
        h.undo(&mut doc).unwrap();
        assert_eq!(played(&doc), 150);
    }
}
