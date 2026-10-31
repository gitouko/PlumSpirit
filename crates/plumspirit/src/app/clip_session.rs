//! The clip the Piano Roll edits (v0.9 V09-S11, `docs/CLIP_PIANO_ROLL.md`
//! Decision 3).
//!
//! Open (Enter, double-click, the clip menu) on a clip starts a session: the
//! roll edits that clip's content — a track's own notes or an owned content
//! — in content ticks, with song time shifted by the placement's
//! `start − offset`. A move or resize of the clip is followed; the session
//! ends when the user picks another track, when the clip (or its content)
//! no longer exists, and with the project (it lives in [`super::OpenDoc`]).
//!
//! Every note site of the app reads what it edits through
//! [`PlumSpiritApp::edit_target`] (and positions through
//! [`PlumSpiritApp::edit_cursor`] / [`PlumSpiritApp::to_content`]), so there
//! is one answer to "which notes". With no session it is **what the active
//! track plays** (v1.0-beta1 B1-0096, [`default_pick`]): its own notes while
//! its primary clip is on it — exactly as before clips for every v0.8
//! project — else the clip it hosts (a merge result, an independent copy, an
//! added clip), so a merged track never shows its stale pre-merge notes.

use super::PlumSpiritApp;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::editor::clips::is_clip;
use plumspirit_engine::model::{
    ClipId, ClipPlacement, ContentRef, NoteStore, PlumDocument, TrackId,
};

/// An opened clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipSession {
    pub clip: ClipId,
    /// The track the clip was on when last seen (a move to another row is
    /// followed).
    pub track: TrackId,
    /// B2-F18: chosen inside the Track Aggregate (a press on another clip's
    /// note), not opened from the Track List: the roll keeps showing the
    /// track's other clips.
    pub aggregate: bool,
}

/// What a session edits this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub target: NoteTarget,
    /// `start − offset` of the placement.
    pub shift: i128,
    /// B2-F17: the track the placement is on — the row the Track List shows
    /// it in and the track selected while it is edited. For a clip of
    /// another track's content (a linked copy, a moved clip) this is not the
    /// target's track: the notes are edited as their own track's.
    pub host: usize,
}

/// What a cached [`SessionView`] was resolved for: the project, its clip
/// table and track list revisions, the active track, the active track's own
/// content revision (whether it has notes decides the default pick) and the
/// session. Any edit that could move, remove or re-index the clip changes
/// one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionKey {
    generation: u64,
    clips: u64,
    structure: u64,
    active: usize,
    own: u64,
    session: Option<ClipSession>,
}

/// The resolved view and whether the active track has a clip (B2-F15).
pub type SessionCache = (SessionKey, Option<SessionView>, bool);

/// B2-A05: what the Piano Roll edits — the decision table of
/// `docs/CLIP_PIANO_ROLL.md` "Editor modes".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMode {
    /// Nothing to edit: the roll is a read-only grid with a hint.
    Locked(LockReason),
    /// A clip opened from the Track List.
    Single(SessionView),
    /// The picked track: what it plays. `None`: its own notes at the
    /// identity (as before clips). B2-S10 / S11 turn this into every clip of
    /// the track at song ticks; until then the track's [`default_pick`].
    Aggregate(Option<SessionView>),
}

/// Why the roll is locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReason {
    /// The Conductor is selected: tempo, signatures and markers only.
    Conductor,
    /// B2-F15: the track has no clip (a new track, or its clips are gone or
    /// moved away) — its invisible empty primary does not count.
    NoClip,
}

impl LockReason {
    /// The sentence the roll and the status bar show.
    pub fn hint(self) -> &'static str {
        match self {
            LockReason::Conductor => "The Conductor is selected: choose a track to edit notes.",
            LockReason::NoClip => {
                "This track has no clip: add one in the Track List (Add Piano Roll Clip) to edit notes."
            }
        }
    }
}

/// B2-F15: whether track `index` has a clip the Track List shows — a
/// placement with a window or content to span (a new track's empty primary
/// is none).
pub fn has_clip(doc: &PlumDocument, index: usize) -> bool {
    doc.placements_on(index).iter().any(|p| is_clip(doc, p))
}

/// B2-F18: the picked track's other shown clips — every clip on track
/// `active` except `focus` (the one the roll edits) — for the roll's Track
/// Aggregate view, in the Track List's order.
pub fn aggregate_parts<'a>(
    doc: &'a PlumDocument,
    active: usize,
    focus: ClipId,
) -> Vec<crate::piano_roll::RollPart<'a>> {
    doc.placements_on(active)
        .into_iter()
        .filter(|p| p.id != focus && is_clip(doc, p))
        .filter_map(|p| {
            Some(crate::piano_roll::RollPart {
                clip: p.id,
                store: doc.notes_of(p.content)?,
                key: crate::piano_roll::render::host_key(p.content),
                shift: i128::from(p.start) - i128::from(p.offset),
                window: p.content_window(),
            })
        })
        .collect()
}

/// B2-F18: one note host of the Track Aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AggregateHost {
    pub target: NoteTarget,
    /// Song tick of content tick 0.
    pub shift: i128,
    /// Content ticks whose notes play.
    pub window: (u64, u64),
    /// Song ticks the clip covers (the Track List's span).
    pub span: (u64, u64),
}

/// The note target and shift of placement `p`, if its content and the
/// track that hosts it exist. A track's own content is edited as that
/// track (its own subchannels); an owned content on the clip's track.
pub fn view_of(doc: &PlumDocument, p: &ClipPlacement) -> Option<SessionView> {
    let shift = i128::from(p.start) - i128::from(p.offset);
    let host = doc.index_of(p.track)?;
    let target = match p.content {
        ContentRef::Track(t) => NoteTarget::Track(doc.index_of(t)?),
        ContentRef::Owned(c) => {
            doc.clips().content(c)?;
            NoteTarget::Content { host, content: c }
        }
    };
    Some(SessionView {
        target,
        shift,
        host,
    })
}

/// The clip the roll shows for track `active` without an opened clip
/// (B1-0096): of the placements on the track — its own primary, the owned
/// contents it hosts and (B2-F17) clips of another track's content placed
/// here (a linked copy, a moved clip: edited as that track's notes) — the
/// first that plays notes (the primary comes first), else the first.
/// B2-F15: only clips the Track List shows count (a new track's empty
/// primary is none). `None`: the track's own primary at the identity, or
/// no clip (the roll is then locked: [`EditorMode`]) — the roll shows the
/// track's own notes at shift 0, as before clips.
pub fn default_pick(doc: &PlumDocument, active: usize) -> Option<ClipPlacement> {
    plumspirit_engine::editor::clips::default_pick(doc, active)
}

impl PlumSpiritApp {
    pub(super) fn clip_session(&self) -> Option<ClipSession> {
        self.open.as_ref().and_then(|o| o.clip_session)
    }

    /// The session's target and shift, while it is valid — without a
    /// session, the [`default_pick`] of the active track (resolved once per
    /// document state: placement and track lookups are linear).
    pub(super) fn session_view(&self) -> Option<SessionView> {
        let s = self.clip_session();
        let doc = self.doc()?;
        let active = self.active_track();
        let key = SessionKey {
            generation: self.doc_generation(),
            clips: doc.clips().revision(),
            structure: doc.structure_revision(),
            active,
            own: doc.track(active).map_or(0, |t| t.notes.content_revision()),
            session: s,
        };
        if let Some((k, v, _)) = self.session_cache.get()
            && k == key
        {
            return v;
        }
        let v = match s {
            Some(s) => doc
                .clips()
                .placement(s.clip)
                .and_then(|p| view_of(doc, &p))
                .filter(|v| v.host == active),
            None if self.conductor_selected() => None,
            None => default_pick(doc, active)
                .and_then(|p| view_of(doc, &p))
                .filter(|v| v.host == active),
        };
        self.session_cache
            .set(Some((key, v, has_clip(doc, active))));
        v
    }

    /// B2-A05: the editor mode (resolved with [`Self::session_view`], cached
    /// with it).
    pub(super) fn editor_mode(&self) -> EditorMode {
        if self.conductor_selected() {
            return EditorMode::Locked(LockReason::Conductor);
        }
        let v = self.session_view();
        if let (Some(v), Some(s)) = (v, self.clip_session()) {
            return if s.aggregate {
                EditorMode::Aggregate(Some(v))
            } else {
                EditorMode::Single(v)
            };
        }
        match self.session_cache.get() {
            Some((_, _, false)) => EditorMode::Locked(LockReason::NoClip),
            _ => EditorMode::Aggregate(v),
        }
    }

    /// B2-F18: the clip the roll edits in Track Aggregate (the default pick,
    /// else the track's own primary); `None` outside the aggregate.
    pub(super) fn aggregate_focus(&self) -> Option<ClipId> {
        let EditorMode::Aggregate(_) = self.editor_mode() else {
            return None;
        };
        if let Some(s) = self.clip_session() {
            return Some(s.clip);
        }
        let doc = self.doc()?;
        let active = self.active_track();
        Some(match default_pick(doc, active) {
            Some(p) => p.id,
            None => ClipId::Primary(doc.track(active)?.id()),
        })
    }

    /// B2-F18: the shown clips of the picked track in Track Aggregate as
    /// (target, song shift, content window), the focus first, then every
    /// other clip in Track List order — B2-S12: linked instances of one
    /// content each with their own place and window (consumers that change
    /// notes take each target once); outside the aggregate just the edit
    /// target.
    pub(super) fn aggregate_hosts(&self) -> Vec<AggregateHost> {
        let mut focus = AggregateHost {
            target: self.edit_target(),
            shift: self.edit_shift(),
            window: (0, u64::MAX),
            span: (0, u64::MAX),
        };
        let (EditorMode::Aggregate(_), Some(doc)) = (self.editor_mode(), self.doc()) else {
            return vec![focus];
        };
        let mut out: Vec<AggregateHost> = Vec::new();
        let fid = self.aggregate_focus();
        for p in doc.placements_on(self.active_track()) {
            if !is_clip(doc, &p) {
                continue;
            }
            let Some(v) = view_of(doc, &p) else {
                continue;
            };
            let host = AggregateHost {
                target: v.target,
                shift: v.shift,
                window: p.content_window(),
                span: plumspirit_engine::editor::clips::clip_span(doc, &p),
            };
            if Some(p.id) == fid {
                focus = host;
            } else {
                out.push(host);
            }
        }
        // The track's own primary when it is the focus and also listed.
        out.retain(|h| *h != focus);
        out.insert(0, focus);
        out
    }

    /// B2-F18: the note hosts with a selection (the focus always first) —
    /// what a selection-wide edit changes.
    pub(super) fn selection_targets(&self) -> Vec<NoteTarget> {
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let hosts = self.aggregate_hosts();
        let focus = hosts.first().map(|h| h.target);
        let mut out: Vec<NoteTarget> = Vec::new();
        for t in hosts.into_iter().map(|h| h.target) {
            if !out.contains(&t)
                && (Some(t) == focus
                    || plumspirit_engine::editor::note_view(doc, t)
                        .is_ok_and(|v| v.notes.selected_count() > 0))
            {
                out.push(t);
            }
        }
        out
    }

    /// B2-F18: `make` for every selection target, as one undo step (a
    /// single target: its operation as before).
    pub(super) fn routed(
        &self,
        label: &'static str,
        make: impl Fn(NoteTarget) -> plumspirit_engine::history::Operation,
    ) -> plumspirit_engine::history::Operation {
        let mut ops: Vec<_> = self.selection_targets().into_iter().map(make).collect();
        if ops.len() == 1 {
            ops.pop().expect("one")
        } else {
            plumspirit_engine::history::Operation::Group { ops, label }
        }
    }

    /// B2-F18: where a note drawn or pasted at song tick `song` goes — the
    /// aggregate's clip playing there (the focus first), else the focus —
    /// as (target, content tick).
    pub(super) fn destination_at(&self, song: u64) -> (NoteTarget, u64) {
        let hosts = self.aggregate_hosts();
        let pick = hosts
            .iter()
            .find(|h| song >= h.span.0 && song < h.span.1)
            .or(hosts.first())
            .copied();
        match pick {
            Some(h) => (h.target, shifted(song, -h.shift)),
            None => (self.edit_target(), self.to_content(song)),
        }
    }

    /// B2-F18: clears the selection in the aggregate's clips other than
    /// the focus (a plain press or a new region replaces the selection).
    pub(super) fn clear_other_selections(&mut self) {
        let hosts = self.aggregate_hosts();
        let Some(focus) = hosts.first().map(|h| h.target) else {
            return;
        };
        let Some(d) = self.doc_mut() else {
            return;
        };
        for h in hosts.into_iter().filter(|h| h.target != focus) {
            if let Ok(n) = plumspirit_engine::editor::notes_mut(d, h.target) {
                n.clear_selection();
            }
        }
    }

    /// B2-F18: Select All in Track Aggregate — the notes every clip of the
    /// track plays (its window; linked instances: the union of theirs);
    /// else the roll's notes.
    pub(super) fn select_all_notes(&mut self) {
        let hosts = self.aggregate_hosts();
        if self.note_lock().is_some() {
            return;
        }
        let single = hosts.len() == 1;
        let Some(d) = self.doc_mut() else {
            return;
        };
        let mut cleared: Vec<NoteTarget> = Vec::new();
        for h in hosts {
            let (a, b) = h.window;
            if let Ok(n) = plumspirit_engine::editor::notes_mut(d, h.target) {
                if single || (a, b) == (0, u64::MAX) {
                    n.select_all();
                } else {
                    if !cleared.contains(&h.target) {
                        n.clear_selection();
                        cleared.push(h.target);
                    }
                    n.select_range(a, b, false);
                }
            }
        }
    }

    /// B2-F18: makes `clip` (another clip of the picked track) the edited
    /// one inside the Track Aggregate and selects its note `loc` — the view
    /// keeps every note where it is on screen (song time).
    pub(super) fn focus_aggregate_clip(
        &mut self,
        clip: ClipId,
        loc: plumspirit_engine::model::NoteLoc,
        add: bool,
    ) {
        let Some(doc) = self.doc() else {
            return;
        };
        let active = self.active_track();
        let Some(p) = doc.clips().placement(clip) else {
            return;
        };
        if doc.index_of(p.track) != Some(active) || !is_clip(doc, &p) {
            return;
        }
        let others: Vec<ContentRef> = doc
            .placements_on(active)
            .iter()
            .filter(|q| is_clip(doc, q))
            .map(|q| q.content)
            .collect();
        let old_shift = self.edit_shift();
        let track = p.track;
        if let Some(o) = self.open.as_mut() {
            o.clip_session = Some(ClipSession {
                clip,
                track,
                aggregate: true,
            });
        }
        self.roll.cancel_gesture();
        let new_shift = self.edit_shift();
        let t = &mut self.roll.view.time;
        t.origin_tick = shifted(t.origin_tick, old_shift - new_shift);
        // One selection over the aggregate: a plain press replaces it in
        // every clip of the track; Shift / Ctrl adds to it.
        if let Some(d) = self.doc_mut() {
            if !add {
                for c in others {
                    if let Some(n) = d.notes_of_mut(c) {
                        n.clear_selection();
                    }
                }
            }
            if let Some(n) = d.notes_of_mut(p.content) {
                n.set_selected(loc, true);
            }
        }
    }

    /// Why the roll cannot edit notes now (`None`: it can).
    pub(super) fn note_lock(&self) -> Option<LockReason> {
        match self.editor_mode() {
            EditorMode::Locked(r) => Some(r),
            _ => None,
        }
    }

    /// What the Piano Roll's note edits change: the opened clip's content,
    /// else what the active track plays ([`default_pick`]).
    pub(super) fn edit_target(&self) -> NoteTarget {
        self.session_view()
            .map_or(NoteTarget::Track(self.active_track()), |v| v.target)
    }

    /// Song tick of content tick 0 of what the roll edits (0 without a
    /// session).
    pub(super) fn edit_shift(&self) -> i128 {
        self.session_view().map_or(0, |v| v.shift)
    }

    /// Song tick `t` on the roll's axis (content ticks of the opened clip).
    pub(super) fn to_content(&self, t: u64) -> u64 {
        shifted(t, -self.edit_shift())
    }

    /// The roll's tick `c` in song time.
    pub(super) fn to_song(&self, c: u64) -> u64 {
        shifted(c, self.edit_shift())
    }

    /// A song-time range (the time selection) on the roll's axis.
    pub(super) fn range_to_content(&self, r: (u64, u64)) -> (u64, u64) {
        (self.to_content(r.0), self.to_content(r.1))
    }

    /// The edit cursor (song time) on the roll's axis: where pastes,
    /// slices and inserted scores go.
    pub(super) fn edit_cursor(&self) -> u64 {
        self.to_content(self.roll.cursor_tick)
    }

    /// The notes the roll edits.
    pub(super) fn edit_notes(&self) -> Option<&NoteStore> {
        let doc = self.doc()?;
        plumspirit_engine::editor::note_view(doc, self.edit_target())
            .ok()
            .map(|v| v.notes)
    }

    /// The notes the roll edits, for changes outside the history
    /// (selection); `None` while a job shares the document.
    pub(super) fn edit_notes_mut(&mut self) -> Option<&mut NoteStore> {
        // B2-A01: a locked roll selects and changes no notes.
        if self.note_lock().is_some() {
            return None;
        }
        let target = self.edit_target();
        let doc = self.doc_mut()?;
        plumspirit_engine::editor::notes_mut(doc, target).ok()
    }

    /// Opens clip `id` in the Piano Roll (a session; `docs/CLIP_PIANO_ROLL.md`).
    pub(super) fn open_clip_session(&mut self, id: ClipId) -> bool {
        let Some(doc) = self.doc() else {
            return false;
        };
        let Some(p) = doc.clips().placement(id) else {
            return false;
        };
        let Some(v) = view_of(doc, &p) else {
            return false;
        };
        let name = clip_label(doc, &p);
        let reveal = (p.offset, doc.duration_ticks());
        self.select_track(v.host);
        if let Some(o) = self.open.as_mut() {
            o.clip_session = Some(ClipSession {
                clip: id,
                track: p.track,
                aggregate: false,
            });
        }
        self.roll.cancel_gesture();
        self.set_main_view(super::track_list::MainView::PianoRoll);
        self.roll
            .reveal(reveal.0, shifted(reveal.1, -v.shift).max(reveal.0));
        self.set_status(
            format!("Editing {name} in the Piano Roll."),
            StatusKind::Info,
        );
        true
    }

    /// Ends the session (the roll shows what the active track plays).
    pub(super) fn end_clip_session(&mut self) {
        if let Some(o) = self.open.as_mut()
            && o.clip_session.take().is_some()
        {
            self.roll.cancel_gesture();
        }
    }

    /// Once per frame, before the views: follows a moved clip, ends the
    /// session when the clip or its content is gone (with a sentence) or the
    /// user picked another track.
    pub(super) fn sync_clip_session(&mut self) {
        let Some(s) = self.clip_session() else {
            return;
        };
        let Some(doc) = self.doc() else {
            return;
        };
        let Some(p) = doc.clips().placement(s.clip) else {
            self.end_clip_session();
            self.set_status(
                "The clip open in the Piano Roll no longer exists; the Piano Roll shows what the track plays.",
                StatusKind::Info,
            );
            return;
        };
        let Some(v) = view_of(doc, &p) else {
            self.end_clip_session();
            self.set_status(
                "The clip open in the Piano Roll no longer has its notes; the Piano Roll shows what the track plays.",
                StatusKind::Info,
            );
            return;
        };
        if p.track != s.track {
            // Moved to another row: the roll follows it.
            self.select_track(v.host);
            if let Some(o) = self.open.as_mut() {
                o.clip_session = Some(ClipSession {
                    clip: s.clip,
                    track: p.track,
                    aggregate: s.aggregate,
                });
            }
        } else if v.host != self.active_track() || self.conductor_selected() {
            self.end_clip_session();
        }
    }

    /// The opened (or default-picked) clip's name for titles and the
    /// status bar.
    pub(super) fn session_title(&self) -> Option<String> {
        let doc = self.doc()?;
        let p = match self.clip_session() {
            Some(s) => doc.clips().placement(s.clip)?,
            None => default_pick(doc, self.active_track())?,
        };
        Some(clip_label(doc, &p))
    }
}

/// `t` moved by `shift`, clamped to the tick range.
pub fn shifted(t: u64, shift: i128) -> u64 {
    (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64
}

/// "clip 'Name' on Track 2" (the clip's name, else its content's, else the
/// track's).
fn clip_label(doc: &PlumDocument, p: &ClipPlacement) -> String {
    let track = doc.index_of(p.track);
    let track_name = track
        .and_then(|i| doc.track(i))
        .map(|t| t.name().unwrap_or("").to_owned())
        .unwrap_or_default();
    let content_name = match p.content {
        ContentRef::Owned(c) => doc.clips().content(c).and_then(|o| o.name.clone()),
        ContentRef::Track(_) => None,
    };
    let name = p.name.clone().or(content_name).filter(|n| !n.is_empty());
    let on = match (track, track_name.is_empty()) {
        (Some(i), true) => format!("Track {}", i + 1),
        (_, false) => track_name,
        (None, true) => "a track".to_owned(),
    };
    match name {
        Some(n) => format!("clip \u{201c}{n}\u{201d} on {on}"),
        None => format!("a clip on {on}"),
    }
}

#[cfg(test)]
mod tests {
    //! V09-S11 steps 3a / 3b on the app harness: Open on a clip starts a
    //! session, the roll's edits land in the clip's content (owned or a
    //! track's own) at content ticks, the Track List sees them at once, undo
    //! restores them, and the session follows a moved clip and ends when the
    //! clip goes or the user picks another track.
    use super::*;
    use crate::app::commands::Command;
    use crate::app::test_support::Harness;
    use crate::app::track_list::{ClipIntent, MainView};
    use crate::piano_roll::RollAction;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn note(t: u64, d: u64, p: u8) -> NoteRecord {
        NoteRecord::new(
            t,
            d,
            NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        )
    }

    /// Two tracks with four notes each.
    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("session".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        for i in 0..2 {
            let recs: Vec<NoteRecord> = (0..4u64)
                .map(|k| note(k * 960, 480, 60 + i as u8))
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    fn own_notes(h: &Harness, i: usize) -> Vec<NoteRecord> {
        h.doc()
            .track(i)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect()
    }

    /// The owned content `c`'s notes as (start, pitch, velocity).
    fn content(h: &Harness, c: plumspirit_engine::model::ContentId) -> Vec<(u64, u8, u8)> {
        h.doc()
            .clips()
            .content(c)
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.start_tick, n.attrs.pitch(), n.attrs.velocity()))
            .collect()
    }

    fn roll(h: &mut Harness, a: RollAction) {
        let ctx = h.ctx.clone();
        h.app.apply_roll_actions(vec![a], &ctx);
    }

    /// Add Piano Roll Clip → Open → draw, select, move, set velocity,
    /// delete — every edit lands in the clip's content in content ticks, the
    /// tracks' own notes never change, the Track List shows the notes at
    /// once, and undo walks back to the empty clip, then removes it (which
    /// ends the session with a sentence).
    #[test]
    fn an_added_clip_opens_in_the_roll_and_its_edits_land_in_its_content() {
        let mut h = Harness::new(song());
        let before = (own_notes(&h, 0), own_notes(&h, 1));
        h.clip(ClipIntent::Add {
            track: 1,
            start: 1_920,
            length: 3_840,
        });
        let c = h.doc().clips().contents()[0].id;
        let id = h.doc().clips().placements_of(ContentRef::Owned(c))[0].id;
        h.clip(ClipIntent::Open(id));
        assert_eq!(h.app.main_view, MainView::PianoRoll);
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Content {
                host: 1,
                content: c
            }
        );
        assert_eq!(h.app.edit_shift(), 1_920);
        assert!(h.status().contains("Editing"), "{}", h.status());
        // Draw at content tick 240 (song 2,160).
        roll(&mut h, RollAction::InsertNote(note(240, 480, 72)));
        assert_eq!(content(&h, c), vec![(240, 72, 100)]);
        // The Track List's view of the document plays it at song 2,160.
        let placed: Vec<u64> = h
            .doc()
            .placed_contents(1)
            .into_iter()
            .filter(|p| p.placement.content == ContentRef::Owned(c))
            .flat_map(|p| {
                p.notes
                    .iter()
                    .filter_map(|n| p.placement.song_tick(n.start_tick))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(placed, vec![2_160]);
        h.refresh_track_list();
        // Select All, move, velocity: the content only.
        assert!(h.command(Command::SelectAll));
        assert_eq!(h.app.selected_count(), 1);
        roll(&mut h, RollAction::MoveSelected { dt: 240, dp: 2 });
        assert_eq!(content(&h, c), vec![(480, 74, 100)]);
        roll(&mut h, RollAction::SetVelocity(33));
        assert_eq!(content(&h, c), vec![(480, 74, 33)]);
        assert!(h.command(Command::Delete));
        assert!(content(&h, c).is_empty());
        assert_eq!((own_notes(&h, 0), own_notes(&h, 1)), before);
        // Undo the four edits: the drawn note is back, then gone.
        for want in [
            vec![(480, 74, 33)],
            vec![(480, 74, 100)],
            vec![(240, 72, 100)],
            vec![],
        ] {
            assert!(h.command(Command::Undo));
            h.app.sync_clip_session();
            assert_eq!(content(&h, c), want);
            assert_eq!(h.app.edit_target().host_track(), 1);
        }
        // Undo the Add: the clip and its content are gone; the session ends.
        assert!(h.command(Command::Undo));
        h.app.sync_clip_session();
        assert!(h.app.clip_session().is_none());
        assert_eq!(h.app.edit_target(), NoteTarget::Track(1));
        assert!(h.status().contains("no longer exists"), "{}", h.status());
        assert_eq!((own_notes(&h, 0), own_notes(&h, 1)), before);
    }

    /// A clip moved to another row keeps its session (its track becomes
    /// active, the shift follows); picking another track ends it.
    #[test]
    fn a_moved_clip_keeps_its_session_and_picking_a_track_ends_it() {
        let mut h = Harness::new(song());
        h.clip(ClipIntent::Add {
            track: 0,
            start: 960,
            length: 1_920,
        });
        let c = h.doc().clips().contents()[0].id;
        let id = h.doc().clips().placements_of(ContentRef::Owned(c))[0].id;
        h.clip(ClipIntent::Open(id));
        assert_eq!(h.app.edit_shift(), 960);
        h.clip(ClipIntent::Move(vec![(id, 1, 3_840)]));
        h.app.sync_clip_session();
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Content {
                host: 1,
                content: c
            }
        );
        assert_eq!(h.app.active_track(), 1);
        assert_eq!(h.app.edit_shift(), 3_840);
        // Edits still land in the content.
        roll(&mut h, RollAction::InsertNote(note(0, 240, 64)));
        assert_eq!(content(&h, c), vec![(0, 64, 100)]);
        // The user picks track 0 in the panel: the roll edits its own notes.
        h.set_track(0);
        h.app.sync_clip_session();
        assert!(h.app.clip_session().is_none());
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        assert_eq!(h.app.edit_shift(), 0);
    }

    /// Event Graphs edit the opened clip's own events (V09-S11 3c): a
    /// controller set in Graph 1 lands in the content's events, not the
    /// track's; undo removes it.
    #[test]
    fn graph_edits_in_an_opened_clip_write_its_own_events() {
        use crate::piano_roll::graph::GraphAction;
        use plumspirit_engine::editor::ControllerKind;
        let mut h = Harness::new(song());
        h.clip(ClipIntent::Add {
            track: 0,
            start: 960,
            length: 1_920,
        });
        let c = h.doc().clips().contents()[0].id;
        let id = h.doc().clips().placements_of(ContentRef::Owned(c))[0].id;
        h.clip(ClipIntent::Open(id));
        let track_events = h.doc().track(0).unwrap().events.len();
        h.app.apply_graph_action(
            0,
            GraphAction::SetController {
                kind: ControllerKind::Cc(7),
                channel: 0,
                target: None,
                tick: 480,
                value: 90,
            },
        );
        let ev = &h.doc().clips().content(c).unwrap().events;
        assert_eq!(ev.len(), 1);
        assert_eq!(ev.events()[0].tick, 480, "content ticks");
        assert_eq!(h.doc().track(0).unwrap().events.len(), track_events);
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().clips().content(c).unwrap().events.len(), 0);
    }

    /// Track 1 with notes and a new, empty Track 2 (the docx page-5 song).
    fn new_track_song() -> PlumDocument {
        let mut d = PlumDocument::new_project("track 2".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..4u64).map(|k| note(k * 960, 480, 60)).collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d.add_track().unwrap();
        d
    }

    /// The clips Track `i` hosts other than its own primary.
    fn hosted(h: &Harness, i: usize) -> Vec<ClipPlacement> {
        let own = h.id(i);
        h.doc()
            .placements_on(i)
            .into_iter()
            .filter(|p| p.id != ClipId::Primary(own))
            .collect()
    }

    /// B2-F17 (SA-12-H1, docx page 5): a Clip on the new Track 2 that plays
    /// Track 1's notes (a linked duplicate keeps `ContentRef::Track(1)`) is
    /// counted on Track 2 and must show — and edit — those notes in the Piano
    /// Roll, picked by default and when opened from the Track List.
    #[test]
    fn a_linked_clip_on_a_new_track_shows_its_notes_in_the_roll() {
        let mut h = Harness::new(new_track_song());
        let t0 = h.id(0);
        h.clip(ClipIntent::Duplicate {
            copies: vec![(ClipId::Primary(t0), 1, 7_680)],
            linked: true,
        });
        let clip = hosted(&h, 1);
        assert_eq!(clip.len(), 1, "the copy is on Track 2");
        assert_eq!(clip[0].content, ContentRef::Track(t0));
        assert_eq!(
            h.doc().placed_note_counts(),
            vec![4, 4],
            "Track List counts"
        );
        // Track 2 picked in the panel: the roll shows the clip's notes.
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0), "default pick");
        assert_eq!(h.app.edit_shift(), 7_680);
        assert_eq!(h.app.edit_notes().map(|n| n.len()), Some(4));
        // ... and draws them (Track 2's own store is empty: no chunks).
        let screen = egui::vec2(1366.0, 768.0);
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0), "after frames");
        let st = &h.app.roll.stats;
        assert!(
            st.chunks_total > 0,
            "the roll draws the clip's notes: {st:?}"
        );
        // Opened from the Track List: the same.
        h.clip(ClipIntent::Open(clip[0].id));
        h.app.sync_clip_session();
        assert_eq!(h.app.active_track(), 1);
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0), "opened");
        assert_eq!(h.app.edit_shift(), 7_680);
        // An edit lands in the shared content at content ticks.
        roll(&mut h, RollAction::InsertNote(note(240, 240, 72)));
        assert_eq!(own_notes(&h, 0).len(), 5);
        assert!(own_notes(&h, 1).is_empty(), "Track 2's raw store untouched");
        // Saved and reopened: Track 2 picks the same clip.
        let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
        let (doc, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
        let p = default_pick(&doc, 1).expect("the linked clip");
        assert_eq!(
            view_of(&doc, &p).map(|v| (v.target, v.host)),
            Some((NoteTarget::Track(0), 1))
        );
        // Undo the edit, then the duplicate: Track 2 is empty again; redo
        // brings the clip back into the roll.
        // (Undo selects the track it changed: pick Track 2 again.)
        assert!(h.command(Command::Undo));
        assert!(h.command(Command::Undo));
        h.set_track(1);
        h.app.sync_clip_session();
        assert!(hosted(&h, 1).is_empty());
        assert_eq!(h.app.edit_target(), NoteTarget::Track(1));
        assert!(h.command(Command::Redo));
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        assert_eq!(h.app.edit_notes().map(|n| n.len()), Some(4));
    }

    /// B2-F17: Track 1's own Clip moved onto the new Track 2 (it keeps
    /// `ContentRef::Track(1)`) shows its notes there; moved back, Track 1
    /// shows them again.
    #[test]
    fn a_clip_moved_to_a_new_track_shows_its_notes_there_and_back() {
        let mut h = Harness::new(new_track_song());
        let t0 = h.id(0);
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 1, 1_920)]));
        assert_eq!(h.doc().placed_note_counts(), vec![0, 4]);
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        assert_eq!(h.app.edit_shift(), 1_920);
        assert_eq!(h.app.edit_notes().map(|n| n.len()), Some(4));
        // B2-F15: Track 1's clip moved away — Track 1 has no clip and is
        // locked (it no longer shows the moved notes at the identity).
        h.set_track(0);
        h.app.sync_clip_session();
        assert_eq!(h.app.editor_mode(), EditorMode::Locked(LockReason::NoClip));
        roll(&mut h, RollAction::InsertNote(note(0, 240, 64)));
        assert_eq!(own_notes(&h, 0).len(), 4, "refused");
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 0, 0)]));
        h.set_track(0);
        assert_eq!(h.app.editor_mode(), EditorMode::Aggregate(None));
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        assert_eq!(h.app.edit_shift(), 0);
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(h.app.edit_notes().map(|n| n.len()), Some(0));
    }

    /// B2-F15 (fixture from B2-S06): a new Track with no Clip locks the
    /// Piano Roll — an edit there does not reach the Track's raw notes, the
    /// status says how to start, and the roll shows the hint instead of
    /// notes; markers stay editable. Adding a clip unlocks it; the Conductor
    /// is locked for notes too (B2-A05).
    #[test]
    fn an_empty_new_track_locks_the_roll() {
        let mut h = Harness::new(new_track_song());
        h.set_track(1);
        h.app.sync_clip_session();
        assert_eq!(h.app.editor_mode(), EditorMode::Locked(LockReason::NoClip));
        roll(&mut h, RollAction::InsertNote(note(0, 240, 64)));
        assert!(own_notes(&h, 1).is_empty(), "nothing reached the raw store");
        assert!(h.status().contains("no clip"), "{}", h.status());
        // The roll says so at the top (and draws no notes of the track).
        let screen = egui::vec2(1366.0, 768.0);
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app_frame(screen, 1.0);
        let shown = crate::ui::testkit::texts(&h.app_frame(screen, 1.0));
        assert!(
            shown
                .iter()
                .any(|(t, p)| t.contains("has no clip") && p.y < screen.y / 2.0),
            "the roll's hint (top), not only the status bar"
        );
        assert_eq!(h.app.roll.stats.chunks_total, 0);
        // Markers, signatures, the loop and seeking are not note edits.
        assert!(!RollAction::AddMarkerAt(960).touches_notes());
        assert!(!RollAction::Seek(960).touches_notes());
        // An added clip unlocks the track: it is what the roll edits.
        h.clip(ClipIntent::Add {
            track: 1,
            start: 0,
            length: 1_920,
        });
        h.set_track(1);
        h.app.sync_clip_session();
        let mode = h.app.editor_mode();
        assert!(matches!(mode, EditorMode::Aggregate(Some(_))), "{mode:?}");
        // The Conductor: notes locked.
        h.set_conductor();
        assert_eq!(
            h.app.editor_mode(),
            EditorMode::Locked(LockReason::Conductor)
        );
        roll(&mut h, RollAction::InsertNote(note(0, 240, 64)));
        assert_eq!(own_notes(&h, 0).len(), 4);
    }

    /// B2-A01 (RISK-A): one editability gate for every path — with the roll
    /// locked (a new track without a clip, the Conductor, a track whose clip
    /// was moved away) no note command changes any notes, while the same
    /// commands do on a track with a clip (the control).
    #[test]
    fn locked_contexts_refuse_every_note_command() {
        let cmds = [
            Command::SelectAll,
            Command::Paste,
            Command::Cut,
            Command::Delete,
            Command::Quantize,
            Command::GroupNotes,
            Command::MuteNotes,
            Command::SliceNotes,
            Command::PaintCurrentSubchannel,
            Command::DuplicateOctaveUp,
            Command::FlipX,
            Command::FlipY,
            Command::PasteRepeat,
            Command::RepeatLastTool,
        ];
        // The commands that changed notes (one at a time, so a Paste and a
        // Cut cannot cancel out).
        let run_all = |h: &mut Harness| -> Vec<Command> {
            // The commands as the Piano Roll's (the Track List has its own).
            h.app.main_view = MainView::PianoRoll;
            let mut changed = Vec::new();
            for c in cmds {
                let f = h.doc().fingerprint();
                h.command(c);
                h.app.dialog = None;
                if h.doc().fingerprint() != f {
                    changed.push(c);
                }
            }
            changed
        };
        // Contexts: the empty new Track 2, the Conductor, Track 1 after its
        // clip moved to Track 2.
        for ctx in 0..3 {
            let mut h = Harness::new(new_track_song());
            // Something on the clipboard to paste.
            h.app.main_view = MainView::PianoRoll;
            h.set_track(0);
            assert!(h.command(Command::SelectAll));
            assert!(h.command(Command::Copy));
            match ctx {
                0 => h.set_track(1),
                1 => h.set_conductor(),
                _ => {
                    let t0 = h.id(0);
                    h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 1, 1_920)]));
                    h.set_track(0);
                }
            }
            h.app.sync_clip_session();
            assert!(h.app.note_lock().is_some(), "context {ctx} is locked");
            assert_eq!(run_all(&mut h), vec![], "context {ctx}: notes unchanged");
            // Nothing selected either (a moved-away clip's notes included).
            assert_eq!(h.app.selected_count(), 0, "context {ctx}: no selection");
            assert!(own_notes(&h, 1).is_empty() || ctx == 2);
        }
        // Control: Track 1 with its clip — the commands do edit.
        let mut h = Harness::new(new_track_song());
        h.app.main_view = MainView::PianoRoll;
        h.set_track(0);
        assert!(h.command(Command::SelectAll));
        assert!(h.command(Command::Copy));
        assert!(h.app.note_lock().is_none());
        let changed = run_all(&mut h);
        assert!(
            changed.contains(&Command::Paste) && changed.contains(&Command::Cut),
            "the control edits: {changed:?}"
        );
    }

    /// B2-F18 (read path, B2-S10): a track with its own notes and an added
    /// clip shows both in the Piano Roll at their song ticks — the edited
    /// clip (the default pick) and the other as an aggregate part, with the
    /// window of each; opening a clip shows that clip alone (Single).
    #[test]
    fn a_track_with_two_clips_shows_both_in_the_roll() {
        let mut h = Harness::new(new_track_song());
        h.clip(ClipIntent::Add {
            track: 0,
            start: 7_680,
            length: 1_920,
        });
        let c = h.doc().clips().contents()[0].id;
        let id = h.doc().clips().placements_of(ContentRef::Owned(c))[0].id;
        h.clip(ClipIntent::Open(id));
        roll(&mut h, RollAction::InsertNote(note(0, 240, 70)));
        roll(&mut h, RollAction::InsertNote(note(480, 240, 71)));
        // A note past the clip's window: stored, not played, not drawn.
        roll(&mut h, RollAction::InsertNote(note(5_000, 240, 72)));
        assert_eq!(content(&h, c).len(), 3);
        // Back to the track (the session ends): Track Aggregate, focus = its
        // own notes.
        h.app.end_clip_session();
        h.set_track(0);
        h.app.sync_clip_session();
        assert_eq!(h.app.editor_mode(), EditorMode::Aggregate(None));
        assert_eq!(h.app.aggregate_focus(), Some(ClipId::Primary(h.id(0))));
        let parts = aggregate_parts(h.doc(), 0, ClipId::Primary(h.id(0)));
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].clip, parts[0].shift), (id, 7_680));
        let screen = egui::vec2(1366.0, 768.0);
        h.app_frame(screen, 1.0);
        h.app.main_view = MainView::PianoRoll;
        h.app.roll.view.time = plumspirit_engine::view::TimeAxis::new(0, 0.08);
        h.app.roll.view.pitch.scroll_rows = 127.0 - 80.0;
        h.app_frame(screen, 1.0);
        h.app_frame(screen, 1.0);
        let st = h.app.roll.stats;
        assert!(!st.lod, "{st:?}");
        // Own notes + the clip's window, each a bordered rectangle (2
        // primitives); the note past the window is not drawn.
        assert_eq!(st.primitives, 2 * (4 + 2), "{st:?}");
        // A press on the clip's note focuses it inside the aggregate: the
        // roll edits it, the track's own clip becomes the other part, the
        // view keeps song time and the note is the selection.
        let origin = h.app.roll.view.time.origin_tick;
        let mut loc = None;
        h.doc()
            .clips()
            .content(c)
            .unwrap()
            .notes
            .query(0, 1, |l, n| {
                if n.start_tick == 0 {
                    loc = Some(l);
                }
            });
        roll(
            &mut h,
            RollAction::FocusClip {
                clip: id,
                loc: loc.expect("the clip's first note"),
                add: false,
            },
        );
        assert!(matches!(
            h.app.editor_mode(),
            EditorMode::Aggregate(Some(_))
        ));
        assert_eq!(
            h.app.edit_target(),
            NoteTarget::Content {
                host: 0,
                content: c
            }
        );
        assert_eq!(h.app.edit_shift(), 7_680);
        assert_eq!(
            h.app.roll.view.time.origin_tick,
            origin.saturating_sub(7_680)
        );
        assert_eq!(h.app.selected_count(), 1);
        let parts = aggregate_parts(h.doc(), 0, id);
        assert_eq!(
            parts.iter().map(|q| q.clip).collect::<Vec<_>>(),
            vec![ClipId::Primary(h.id(0))]
        );
        // Edits go to the focused clip (B2-S11 routes by owner).
        roll(&mut h, RollAction::SetVelocity(33));
        assert_eq!(content(&h, c)[0], (0, 70, 33));
        // Opened, the clip is shown alone (Single Clip).
        h.clip(ClipIntent::Open(id));
        h.app.sync_clip_session();
        assert!(matches!(h.app.editor_mode(), EditorMode::Single(_)));
        assert_eq!(h.app.aggregate_focus(), None);
    }

    /// B2-F18 (B2-S11): in Track Aggregate, selection-wide edits change the
    /// selected notes of every clip of the track as one undo step; a drawn
    /// note and a paste go to the clip playing there; a plain press
    /// replaces the selection in every clip.
    #[test]
    fn aggregate_edits_reach_every_clip_of_the_track() {
        let mut h = Harness::new(new_track_song());
        h.clip(ClipIntent::Add {
            track: 0,
            start: 7_680,
            length: 1_920,
        });
        let c = h.doc().clips().contents()[0].id;
        let id = h.doc().clips().placements_of(ContentRef::Owned(c))[0].id;
        h.clip(ClipIntent::Open(id));
        roll(&mut h, RollAction::InsertNote(note(0, 240, 70)));
        roll(&mut h, RollAction::InsertNote(note(480, 240, 71)));
        roll(&mut h, RollAction::InsertNote(note(5_000, 240, 72)));
        h.app.end_clip_session();
        h.set_track(0);
        h.app.sync_clip_session();
        h.app.main_view = MainView::PianoRoll;
        assert_eq!(h.app.editor_mode(), EditorMode::Aggregate(None));
        // Select All: the notes both clips play (not the one past the window).
        assert!(h.command(Command::SelectAll));
        assert_eq!(h.app.selected_count(), 4 + 2);
        // Velocity, then Delete: both clips, one undo step each.
        roll(&mut h, RollAction::SetVelocity(20));
        assert!(own_notes(&h, 0).iter().all(|r| r.attrs.velocity() == 20));
        assert_eq!(content(&h, c)[..2], [(0, 70, 20), (480, 71, 20)]);
        assert_eq!(
            content(&h, c)[2],
            (5_000, 72, 100),
            "past the window: untouched"
        );
        assert!(h.command(Command::Delete));
        assert!(own_notes(&h, 0).is_empty());
        assert_eq!(content(&h, c), vec![(5_000, 72, 100)]);
        assert!(h.command(Command::Undo));
        assert_eq!(own_notes(&h, 0).len(), 4);
        assert_eq!(content(&h, c).len(), 3, "one step restores both clips");
        // Move up a key: both clips, one step.
        assert!(h.command(Command::SelectAll));
        roll(&mut h, RollAction::MoveSelected { dt: 0, dp: 1 });
        assert!(own_notes(&h, 0).iter().all(|r| r.attrs.pitch() == 61));
        assert_eq!((content(&h, c)[0].1, content(&h, c)[1].1), (71, 72));
        assert!(h.command(Command::Undo));
        assert!(own_notes(&h, 0).iter().all(|r| r.attrs.pitch() == 60));
        // Copy: every clip's selected notes at their distances in the song
        // (so Cut = Copy + Delete stays consistent).
        assert!(h.command(Command::SelectAll));
        assert!(h.command(Command::Copy));
        let starts: Vec<u64> = h
            .app
            .clipboard
            .as_ref()
            .unwrap()
            .records()
            .iter()
            .map(|r| r.start_tick)
            .collect();
        assert_eq!(starts, vec![0, 960, 1_920, 2_880, 7_680, 8_160]);
        // Edits that are not routed (Flip, Quantize, tools…) are refused
        // while the selection spans both clips — no partial edit.
        let before = h.doc().fingerprint();
        assert!(h.command(Command::FlipY));
        assert_eq!(h.doc().fingerprint(), before);
        assert!(h.status().contains("one clip at a time"), "{}", h.status());
        // A plain press on a note of the focus replaces the selection
        // everywhere.
        let mut first = None;
        h.doc()
            .track(0)
            .unwrap()
            .notes
            .query(0, 1, |l, _| first = Some(l));
        roll(&mut h, RollAction::SelectOnly(first.unwrap()));
        assert_eq!(h.app.selected_count(), 1);
        assert!(h.doc().clips().content(c).unwrap().notes.selected_count() == 0);
        // Drawn inside the clip (song 7,920): into the clip at content 240.
        roll(&mut h, RollAction::InsertNote(note(7_920, 240, 80)));
        assert!(content(&h, c).contains(&(240, 80, 100)));
        assert_eq!(own_notes(&h, 0).len(), 4, "not into the track's own notes");
        // Paste at the edit cursor inside the clip: into the clip.
        assert!(h.command(Command::Copy));
        h.app.roll.cursor_tick = 7_680 + 960;
        assert!(h.command(Command::Paste));
        assert!(content(&h, c).iter().any(|n| n.0 == 960 && n.1 == 60));
        assert_eq!(own_notes(&h, 0).len(), 4);
    }

    /// B2-S12 (B2-F18 matrix): two linked instances of one content on the
    /// track, each playing its own part (windows [0, 1920) and [1920,
    /// 3840)): Select All selects the notes both play — the shared notes
    /// once — a note drawn inside the second instance lands under the
    /// pointer in the content, and Delete removes them in both places as
    /// one undo step.
    #[test]
    fn linked_instances_in_the_aggregate_keep_their_own_window_and_place() {
        let mut h = Harness::new(new_track_song());
        h.clip(ClipIntent::Add {
            track: 0,
            start: 7_680,
            length: 3_840,
        });
        let c = h.doc().clips().contents()[0].id;
        let a = h.doc().clips().placements_of(ContentRef::Owned(c))[0].id;
        h.clip(ClipIntent::Open(a));
        for k in 0..4u64 {
            roll(
                &mut h,
                RollAction::InsertNote(note(k * 960, 240, 70 + k as u8)),
            );
        }
        h.clip(ClipIntent::Duplicate {
            copies: vec![(a, 0, 15_360)],
            linked: true,
        });
        let b = h
            .doc()
            .clips()
            .placements_of(ContentRef::Owned(c))
            .into_iter()
            .find(|p| p.id != a)
            .unwrap()
            .id;
        h.clip(ClipIntent::Resize {
            id: a,
            start: 7_680,
            offset: 0,
            length: Some(1_920),
        });
        h.clip(ClipIntent::Resize {
            id: b,
            start: 17_280,
            offset: 1_920,
            length: Some(1_920),
        });
        h.app.end_clip_session();
        h.set_track(0);
        h.app.sync_clip_session();
        h.app.main_view = MainView::PianoRoll;
        assert_eq!(h.app.editor_mode(), EditorMode::Aggregate(None));
        assert!(h.command(Command::SelectAll));
        assert_eq!(
            h.app.selected_count(),
            4 + 4,
            "both windows, shared notes once"
        );
        // Drawn at song 17,380 (inside the second instance, content 2,020).
        roll(&mut h, RollAction::InsertNote(note(17_380, 120, 90)));
        assert!(
            content(&h, c).contains(&(2_020, 90, 100)),
            "{:?}",
            content(&h, c)
        );
        assert_eq!(own_notes(&h, 0).len(), 4);
        assert!(h.command(Command::SelectAll));
        assert_eq!(h.app.selected_count(), 4 + 5);
        assert!(h.command(Command::Delete));
        assert!(content(&h, c).is_empty() && own_notes(&h, 0).is_empty());
        assert!(h.command(Command::Undo));
        assert_eq!((content(&h, c).len(), own_notes(&h, 0).len()), (5, 4));
    }

    /// B2-S12 (B2-F18 matrix): two clips overlapping in song time — a note
    /// drawn where both play goes to the focus when it covers the tick, else
    /// to the first in Track List order; a muted clip stays in the
    /// aggregate (Select All, edits).
    #[test]
    fn overlapping_clips_route_to_the_focus_else_the_first_and_mute_keeps_them() {
        let mut h = Harness::new(new_track_song());
        for start in [7_680, 9_600] {
            h.clip(ClipIntent::Add {
                track: 0,
                start,
                length: 3_840,
            });
        }
        let cs: Vec<_> = h.doc().clips().contents().iter().map(|c| c.id).collect();
        let at = |h: &Harness, c| h.doc().clips().placements_of(ContentRef::Owned(c))[0].clone();
        let (x, y) = (at(&h, cs[0]), at(&h, cs[1]));
        assert_eq!((x.start, y.start), (7_680, 9_600));
        h.clip(ClipIntent::Open(y.id));
        roll(&mut h, RollAction::InsertNote(note(0, 120, 75)));
        h.app.end_clip_session();
        h.set_track(0);
        h.app.sync_clip_session();
        h.app.main_view = MainView::PianoRoll;
        // Song 10,000: both play; the focus (the track's own clip) does not.
        roll(&mut h, RollAction::InsertNote(note(10_000, 120, 80)));
        assert_eq!(content(&h, cs[0]), vec![(2_320, 80, 100)]);
        assert_eq!(content(&h, cs[1]).len(), 1);
        // Focus the second clip (a press on its note): now it gets them.
        let mut loc = None;
        h.doc()
            .clips()
            .content(cs[1])
            .unwrap()
            .notes
            .query(0, 1, |l, _| loc = Some(l));
        h.app.focus_aggregate_clip(y.id, loc.unwrap(), false);
        assert_eq!(h.app.aggregate_focus(), Some(y.id));
        roll(
            &mut h,
            RollAction::InsertNote(note(10_000 - 9_600, 120, 81)),
        );
        assert!(content(&h, cs[1]).contains(&(400, 81, 100)));
        assert_eq!(content(&h, cs[0]).len(), 1);
        // Muted: still shown and edited in the aggregate.
        h.clip(ClipIntent::Mute(vec![x.id], true));
        assert!(h.command(Command::SelectAll));
        assert_eq!(h.app.selected_count(), 4 + 1 + 2);
        // View switch: the Track List and back keep the focus and selection.
        h.app.main_view = MainView::TrackList;
        h.app.sync_clip_session();
        h.app.main_view = MainView::PianoRoll;
        h.app.sync_clip_session();
        assert_eq!(h.app.aggregate_focus(), Some(y.id));
        assert_eq!(h.app.selected_count(), 4 + 1 + 2);
        // Picking another track ends the focus; back here: the default pick.
        h.set_track(1);
        h.app.sync_clip_session();
        h.set_track(0);
        h.app.sync_clip_session();
        assert_ne!(h.app.aggregate_focus(), Some(y.id));
        // Save / reopen (.lup bytes): the clips and the mute come back and
        // the roll reopens as the aggregate around the default pick.
        let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
        let (doc, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
        let mut r = Harness::new(doc);
        r.set_track(0);
        r.app.sync_clip_session();
        r.app.main_view = MainView::PianoRoll;
        assert_eq!(r.app.editor_mode(), EditorMode::Aggregate(None));
        assert!(r.doc().clips().placements_of(ContentRef::Owned(cs[0]))[0].muted);
        assert!(r.command(Command::SelectAll));
        assert_eq!(r.app.selected_count(), 4 + 1 + 2);
    }

    /// A track's own clip moved to song tick 960 opens with shift 960: the
    /// roll edits the track's notes in their own ticks, the edit cursor and
    /// the time selection (song time) are moved onto that axis, and Delete
    /// Track ends the session.
    #[test]
    fn a_moved_track_clip_edits_the_track_at_its_own_ticks() {
        let mut h = Harness::new(song());
        let t0 = h.id(0);
        h.clip(ClipIntent::Move(vec![(ClipId::Primary(t0), 0, 960)]));
        h.clip(ClipIntent::Open(ClipId::Primary(t0)));
        assert_eq!(h.app.edit_target(), NoteTarget::Track(0));
        assert_eq!(h.app.edit_shift(), 960);
        // The edit cursor at song 2,880 is content tick 1,920.
        h.app.roll.cursor_tick = 2_880;
        assert_eq!(h.app.edit_cursor(), 1_920);
        // The time selection song 960 – 2,880 is content 0 – 1,920: the
        // notes starting at 0 and 960.
        h.app.time_selection = Some((960, 2_880));
        h.app.select_in_time_selection(&[0]);
        let sel: Vec<u64> = h
            .doc()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .filter(|n| n.selected)
            .map(|n| n.start_tick)
            .collect();
        assert_eq!(sel, vec![0, 960]);
        // Paste at the cursor: the copies start at content 1,920.
        assert!(h.command(Command::Copy));
        assert!(h.command(Command::Paste));
        let starts: Vec<u64> = own_notes(&h, 0).iter().map(|r| r.start_tick).collect();
        assert_eq!(starts, vec![0, 960, 1_920, 1_920, 2_880, 2_880]);
        // Delete Track ends the session (the clip is gone with its track).
        h.set_track(1);
        h.app.select_track(0);
        h.clip(ClipIntent::Open(ClipId::Primary(t0)));
        assert!(h.app.clip_session().is_some());
        h.app.delete_tracks(&[0]);
        h.app.sync_clip_session();
        assert!(h.app.clip_session().is_none());
    }
}
