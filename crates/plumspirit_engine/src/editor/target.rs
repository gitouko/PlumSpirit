//! Note hosts for editing (v0.9 V09-S11, `docs/CLIP_PIANO_ROLL.md`): what a
//! note edit changes — a track's own notes or an owned content shown on a
//! track — and the read-only view the `prepare_*` functions read it through.
//!
//! The host is resolved once per operation, never per note. A
//! [`NoteTarget::Track`] reads exactly what the editor read before clips
//! existed, so every v0.8 path is unchanged.

use super::EditError;
use crate::model::clip::{ContentId, ContentRef, OwnedContent};
use crate::model::document::PlumDocument;
use crate::model::events::EventStore;
use crate::model::note_store::NoteStore;
use crate::model::track::{SubchannelTable, Track};

/// What a note `Operation` edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteTarget {
    /// The own notes of the track at this index (the meaning before clips).
    Track(usize),
    /// An owned content, edited in a clip on the track at `host`: that
    /// track's subchannel table interprets the content's subchannel ids and
    /// gives new notes their channel.
    Content { host: usize, content: ContentId },
}

impl From<usize> for NoteTarget {
    fn from(index: usize) -> NoteTarget {
        NoteTarget::Track(index)
    }
}

impl NoteTarget {
    /// Index of the track whose subchannels and channel apply.
    pub fn host_track(self) -> usize {
        match self {
            NoteTarget::Track(i) | NoteTarget::Content { host: i, .. } => i,
        }
    }
}

/// The notes an edit is prepared from: a track's own notes, or an owned
/// content seen through the track a clip of it is on. Borrowed; nothing is
/// copied. `&Track` converts into the view of its own notes, so functions
/// that take `impl Into<HostView>` accept a track as before.
#[derive(Clone, Copy, Debug)]
pub struct HostView<'a> {
    /// The hosting track: its subchannels, channel and properties apply.
    pub track: &'a Track,
    /// The owned content, or `None` for the track's own notes.
    pub content: Option<&'a OwnedContent>,
    /// The notes edited: the content's, else the track's.
    pub notes: &'a NoteStore,
}

impl<'a> From<&'a Track> for HostView<'a> {
    fn from(track: &'a Track) -> HostView<'a> {
        HostView {
            track,
            content: None,
            notes: &track.notes,
        }
    }
}

impl<'a> From<&'a mut Track> for HostView<'a> {
    fn from(track: &'a mut Track) -> HostView<'a> {
        HostView::from(&*track)
    }
}

impl<'a> HostView<'a> {
    /// An owned content edited on `track`.
    pub fn of_content(track: &'a Track, content: &'a OwnedContent) -> HostView<'a> {
        HostView {
            track,
            content: Some(content),
            notes: &content.notes,
        }
    }

    /// The note host an edit prepared from this view writes to.
    pub fn host(&self) -> ContentRef {
        match self.content {
            Some(c) => ContentRef::Owned(c.id),
            None => ContentRef::Track(self.track.id()),
        }
    }

    /// The hosting track's subchannels.
    pub fn subchannels(&self) -> &'a SubchannelTable {
        self.track.subchannels()
    }

    /// The channel events of the content (controllers; a track's own content
    /// also holds its meta events).
    pub fn events(&self) -> &'a EventStore {
        match self.content {
            Some(c) => &c.events,
            None => &self.track.events,
        }
    }

    pub fn events_revision(&self) -> u64 {
        match self.content {
            Some(c) => c.events_revision(),
            None => self.track.events_revision(),
        }
    }
}

/// Resolves `target`: a missing track is [`EditError::NoSuchTrack`], a
/// missing content (deleted, or undone away) [`EditError::Stale`].
pub fn note_view(
    doc: &PlumDocument,
    target: impl Into<NoteTarget>,
) -> Result<HostView<'_>, EditError> {
    let target = target.into();
    let track = doc
        .track(target.host_track())
        .ok_or(EditError::NoSuchTrack)?;
    Ok(match target {
        NoteTarget::Track(_) => HostView::from(track),
        NoteTarget::Content { content, .. } => {
            HostView::of_content(track, doc.clips().content(content).ok_or(EditError::Stale)?)
        }
    })
}

/// The note host `target` names, checked like [`note_view`].
pub fn note_host(
    doc: &PlumDocument,
    target: impl Into<NoteTarget>,
) -> Result<ContentRef, EditError> {
    note_view(doc, target).map(|v| v.host())
}

/// The store `target` edits, checked like [`note_view`].
pub fn notes_mut(
    doc: &mut PlumDocument,
    target: impl Into<NoteTarget>,
) -> Result<&mut NoteStore, EditError> {
    let host = note_host(doc, target)?;
    doc.notes_of_mut(host).ok_or(EditError::Stale)
}

/// A working copy of a note host's events, for preparing several events
/// edits in sequence (each prepared against the state the previous ones
/// leave): a copy of the track (its own content) or of the owned content.
/// The notes are shared chunks, not copied.
pub struct EventsWork<'a> {
    host_track: &'a Track,
    work: Work,
}

enum Work {
    Track(Box<Track>),
    Content(Box<OwnedContent>),
}

impl<'a> EventsWork<'a> {
    pub fn of(view: HostView<'a>) -> EventsWork<'a> {
        EventsWork {
            host_track: view.track,
            work: match view.content {
                None => Work::Track(Box::new(view.track.clone())),
                Some(c) => Work::Content(Box::new(c.clone())),
            },
        }
    }

    /// The working state as a note view (to prepare the next edit from).
    pub fn view(&self) -> HostView<'_> {
        match &self.work {
            Work::Track(t) => HostView::from(&**t),
            Work::Content(c) => HostView::of_content(self.host_track, c),
        }
    }

    /// Applies a prepared edit to the working state.
    pub fn apply(&mut self, edit: &super::EventsEdit) -> bool {
        match &mut self.work {
            Work::Track(t) => t.apply_events_edit(edit),
            Work::Content(c) => c.apply_events_edit(edit),
        }
    }
}
