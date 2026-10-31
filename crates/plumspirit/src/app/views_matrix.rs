//! The §042 matrix (v0.9 V09-S11 step 4, V09-0031): the Track List and the
//! Piano Roll are two views of one document. For an opened owned clip and
//! an opened (moved) track clip, every edit made in one view — notes drawn /
//! moved / deleted, a subchannel move, a controller from the Event Graphs,
//! a track rename, a subtrack added / deleted, the clip moved / resized /
//! duplicated / deleted — is in the other view's model at once, and undo
//! restores both. Each check reads what the views actually draw from: the
//! Track List's cached `TrackListModel` and the store its mini notes
//! resolve (`track_list::clip_store`), the roll's `edit_target` /
//! `edit_shift` / `edit_notes`, and the Onion sources (`onion_sources`).

use super::clip_session::shifted;
use super::commands::Command;
use super::dialogs::Dialog;
use super::test_support::Harness;
use super::track_list::{self, ClipIntent};
use crate::piano_roll::RollAction;
use crate::piano_roll::graph::GraphAction;
use crate::ui::track_panel::TrackAction;
use plumspirit_engine::editor::{ControllerKind, NoteTarget};
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
};
use plumspirit_engine::view::tiles::SourcePlace;
use plumspirit_engine::view::track_list::ClipRect;

/// A note as both views are compared: (song tick, pitch, subchannel).
type Seen = (u64, u8, u32);

fn note(t: u64, d: u64, p: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        d,
        NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Three tracks with four notes each (track `i` at pitch 60 + i).
fn song() -> PlumDocument {
    let mut d = PlumDocument::new_project("views".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    d.add_track().unwrap();
    for i in 0..3 {
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

/// The two kinds of opened clip the matrix runs on.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// Add Piano Roll Clip on track 1 at song 1,920 (3,840 long), three
    /// notes drawn in it.
    Owned,
    /// Track 1's own clip moved to song 960.
    TrackClip,
}

/// Opens the clip of `kind`; returns its id.
fn open(h: &mut Harness, kind: Kind) -> ClipId {
    let id = match kind {
        Kind::Owned => {
            h.clip(ClipIntent::Add {
                track: 1,
                start: 1_920,
                length: 3_840,
            });
            let c = h.doc().clips().contents()[0].id;
            h.doc().clips().placements_of(ContentRef::Owned(c))[0].id
        }
        Kind::TrackClip => {
            let id = ClipId::Primary(h.id(1));
            h.clip(ClipIntent::Move(vec![(id, 1, 960)]));
            id
        }
    };
    h.clip(ClipIntent::Open(id));
    if let Kind::Owned = kind {
        for t in [0, 960, 1_920] {
            roll(h, RollAction::InsertNote(note(t, 240, 70)));
        }
    }
    id
}

fn roll(h: &mut Harness, a: RollAction) {
    let ctx = h.ctx.clone();
    h.app.apply_roll_actions(vec![a], &ctx);
}

fn undo(h: &mut Harness) {
    assert!(h.command(Command::Undo), "undo");
    h.app.sync_clip_session();
}

/// What the Track List shows of clip `id`: its rectangle (from the cached
/// model the Track List draws) and the notes it plays, from the store its
/// mini notes read.
fn track_list(h: &mut Harness, id: ClipId) -> Option<(ClipRect, Vec<Seen>)> {
    let generation = h.app.doc_generation();
    let doc = h.app.open.as_ref()?.doc.clone();
    let m = h.app.track_list.model(&doc, generation);
    let r = m.find(id)?.clone();
    let (store, _) = track_list::clip_store(&doc, &r)?;
    let len = r.end - r.start;
    let notes = store
        .iter()
        .filter(|n| n.start_tick >= r.offset && n.start_tick - r.offset < len)
        .map(|n| {
            (
                r.start + (n.start_tick - r.offset),
                n.attrs.pitch(),
                n.sub.0,
            )
        })
        .collect();
    Some((r, notes))
}

/// What the Piano Roll edits: its target, its shift and its notes in song
/// time (the content's notes; those outside the clip's window included).
fn roll_view(h: &Harness) -> (NoteTarget, i128, Vec<(u64, Seen)>) {
    let sh = h.app.edit_shift();
    let notes = h
        .app
        .edit_notes()
        .map(|s| {
            s.iter()
                .map(|n| {
                    (
                        n.start_tick,
                        (shifted(n.start_tick, sh), n.attrs.pitch(), n.sub.0),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    (h.app.edit_target(), sh, notes)
}

/// Both views agree on clip `id` (open in the roll): the roll edits the
/// clip's content on the clip's row with the clip's shift, and the notes the
/// roll shows inside the clip's window are exactly the Track List's. Returns
/// the Track List's notes.
fn agree(h: &mut Harness, id: ClipId) -> Vec<Seen> {
    h.app.sync_clip_session();
    let (r, tl) = track_list(h, id).expect("the clip is in the Track List");
    let (target, shift, notes) = roll_view(h);
    assert!(h.app.clip_session().is_some(), "the clip is open");
    // An owned content is edited on the clip's row; a track's own content as
    // that track (its subchannels), wherever its clip sits.
    assert_eq!(
        target.host_track(),
        r.content_track.unwrap_or(r.track),
        "the roll edits the clip's content on its host"
    );
    match r.content {
        ContentRef::Owned(c) => assert_eq!(
            target,
            NoteTarget::Content {
                host: r.track,
                content: c
            }
        ),
        ContentRef::Track(t) => {
            assert_eq!(target, NoteTarget::Track(h.doc().index_of(t).unwrap()));
        }
    }
    assert_eq!(
        shift,
        i128::from(r.start) - i128::from(r.offset),
        "the roll's song time is the clip's"
    );
    let len = r.end - r.start;
    let windowed: Vec<Seen> = notes
        .into_iter()
        .filter(|(c, _)| *c >= r.offset && c - r.offset < len)
        .map(|(_, s)| s)
        .collect();
    assert_eq!(
        windowed, tl,
        "the roll's notes in the window are the Track List's"
    );
    tl
}

/// Every check of the matrix for one kind of opened clip.
fn matrix(kind: Kind) {
    let mut h = Harness::new(song());
    let id = open(&mut h, kind);
    let base = agree(&mut h, id);
    assert!(!base.is_empty(), "{kind:?}");
    let shift = h.app.edit_shift();
    let host = h.app.edit_target().host_track();

    // Notes: draw / move / delete in the roll.
    roll(&mut h, RollAction::InsertNote(note(480, 240, 72)));
    let drawn = agree(&mut h, id);
    let at = shifted(480, shift);
    assert!(drawn.contains(&(at, 72, 0)), "{kind:?} draw: {drawn:?}");
    assert_eq!(drawn.len(), base.len() + 1);
    undo(&mut h);
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo draw");

    assert!(h.command(Command::SelectAll));
    roll(&mut h, RollAction::MoveSelected { dt: 240, dp: 1 });
    let moved = agree(&mut h, id);
    let want: Vec<Seen> = base.iter().map(|&(t, p, s)| (t + 240, p + 1, s)).collect();
    // A note moved past the window's end leaves the clip's window.
    assert!(
        moved.iter().all(|n| want.contains(n)),
        "{kind:?} move: {moved:?}"
    );
    undo(&mut h);
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo move");

    assert!(h.command(Command::SelectAll));
    assert!(h.command(Command::Delete));
    assert!(agree(&mut h, id).is_empty(), "{kind:?} delete");
    undo(&mut h);
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo delete");

    // A subchannel move: the host track's new subchannel.
    h.app.add_subchannels(1);
    let sub = h.doc().track(host).unwrap().subchannels().max_id();
    assert_ne!(sub, SubchannelId::MAIN);
    assert!(h.command(Command::SelectAll));
    h.app.move_selection_to_sub(sub, false);
    let subbed = agree(&mut h, id);
    assert!(
        subbed.iter().all(|&(_, _, s)| s == sub.0),
        "{kind:?} subchannel: {subbed:?}"
    );
    undo(&mut h);
    undo(&mut h);
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo subchannel");

    // A controller from Graph 1: in the events the clip's placement plays.
    let events = |h: &Harness| -> Vec<u64> {
        h.doc()
            .placed_contents(host)
            .into_iter()
            .find(|p| p.placement.id == id)
            .map(|p| p.events.events().iter().map(|e| e.tick).collect())
            .unwrap_or_default()
    };
    let before = events(&h);
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
    let after = events(&h);
    assert_eq!(after.len(), before.len() + 1, "{kind:?} CC");
    assert!(after.contains(&480), "content ticks: {after:?}");
    undo(&mut h);
    assert_eq!(events(&h), before, "{kind:?} undo CC");
    agree(&mut h, id);

    // A track rename (Track Properties): the row header and the roll's title
    // read the one name.
    h.app.open_track_properties(host);
    let Some(Dialog::TrackProperties(mut d, _)) = h.app.dialog.take() else {
        panic!("Track Properties");
    };
    d.name = "Lead".into();
    h.app.apply_track_properties(&d).unwrap();
    assert_eq!(h.doc().track(host).unwrap().name(), Some("Lead"));
    let title = h.app.session_title().unwrap();
    assert!(title.contains("Lead"), "{kind:?} title: {title}");
    agree(&mut h, id);
    undo(&mut h);
    assert_ne!(h.doc().track(host).unwrap().name(), Some("Lead"));
    assert!(!h.app.session_title().unwrap().contains("Lead"));

    // A subtrack added before the clip's row (Track List / panel menu): the
    // clip's row moves down in the Track List; the new subtrack is picked,
    // so the roll edits it (the session ends, as for any picked track).
    // Reopened, the clip is edited on its new row; deleting the subtrack
    // moves it back, and undo restores the subtrack under the open clip.
    h.track_action(TrackAction::AddSubtrack(0));
    h.app.sync_clip_session();
    let (r, tl) = track_list(&mut h, id).unwrap();
    assert_eq!(r.track, host + 1, "{kind:?} the clip's row moved down");
    assert_eq!(tl, base);
    assert!(h.app.clip_session().is_none());
    assert_eq!(
        h.app.edit_target(),
        NoteTarget::Track(1),
        "the new subtrack"
    );
    h.clip(ClipIntent::Open(id));
    assert_eq!(agree(&mut h, id), base);
    roll(&mut h, RollAction::InsertNote(note(240, 120, 75)));
    let on_new_row = agree(&mut h, id);
    assert!(on_new_row.contains(&(shifted(240, shift), 75, 0)));
    undo(&mut h);
    h.app.select_track(1);
    h.track_action(TrackAction::DeleteTracks(1));
    assert!(h.confirm_delete().is_some());
    h.app.select_track(host);
    h.clip(ClipIntent::Open(id));
    let (r, _) = track_list(&mut h, id).unwrap();
    assert_eq!(r.track, host, "{kind:?} back on its row");
    assert_eq!(agree(&mut h, id), base);
    undo(&mut h);
    let (r, _) = track_list(&mut h, id).unwrap();
    assert_eq!(r.track, host + 1, "{kind:?} undo delete subtrack");
    if h.app.clip_session().is_some() {
        agree(&mut h, id);
    }
    undo(&mut h);
    h.app.select_track(host);
    h.clip(ClipIntent::Open(id));
    assert_eq!(agree(&mut h, id), base);

    // The clip moved in the Track List: the roll follows (same notes in
    // content ticks, the new shift); to another row: the roll follows it
    // there; undo moves both back.
    let (r, _) = track_list(&mut h, id).unwrap();
    let content_before = roll_view(&h).2;
    h.clip(ClipIntent::Move(vec![(id, host, r.start + 1_920)]));
    let moved = agree(&mut h, id);
    assert_eq!(h.app.edit_shift(), shift + 1_920, "{kind:?} shift follows");
    let content_after: Vec<u64> = roll_view(&h).2.iter().map(|(c, _)| *c).collect();
    let content_was: Vec<u64> = content_before.iter().map(|(c, _)| *c).collect();
    assert_eq!(content_after, content_was, "the roll's content ticks stay");
    assert_eq!(
        moved,
        base.iter()
            .map(|&(t, p, s)| (t + 1_920, p, s))
            .collect::<Vec<_>>()
    );
    h.clip(ClipIntent::Move(vec![(id, 2, r.start)]));
    assert_eq!(agree(&mut h, id), base, "{kind:?} on row 2");
    let (r2, _) = track_list(&mut h, id).unwrap();
    assert_eq!(r2.track, 2);
    // An owned clip's host is its row; a track's clip stays its track's.
    let want_host = if let Kind::Owned = kind { 2 } else { host };
    assert_eq!(
        h.app.edit_target().host_track(),
        want_host,
        "{kind:?} followed"
    );
    undo(&mut h);
    undo(&mut h);
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo move clip");
    assert_eq!(h.app.edit_shift(), shift);

    // Resized: the window plays fewer notes (the roll still edits all of the
    // content); a left trim moves start and offset together (same shift).
    let (r, _) = track_list(&mut h, id).unwrap();
    h.clip(ClipIntent::Resize {
        id,
        start: r.start,
        offset: r.offset,
        length: Some(960),
    });
    let short = agree(&mut h, id);
    assert!(
        short.len() < base.len() && !short.is_empty(),
        "{kind:?} {short:?}"
    );
    assert!(short.iter().all(|n| base.contains(n)));
    h.clip(ClipIntent::Resize {
        id,
        start: r.start + 480,
        offset: r.offset + 480,
        length: Some(1_920),
    });
    let trimmed = agree(&mut h, id);
    assert_eq!(h.app.edit_shift(), shift, "a left trim keeps the shift");
    assert!(
        trimmed
            .iter()
            .all(|n| base.contains(n) && n.0 >= r.start + 480)
    );
    undo(&mut h);
    undo(&mut h);
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo resize");

    // A linked duplicate made in the Track List: a note drawn in the roll is
    // played by both placements.
    let (r, _) = track_list(&mut h, id).unwrap();
    let far = r.end + 7_680;
    h.clip(ClipIntent::Duplicate {
        copies: vec![(id, host, far)],
        linked: true,
    });
    let copy = h
        .doc()
        .clips()
        .placements_of(r.content)
        .into_iter()
        .find(|p| p.start == far)
        .expect("the linked copy")
        .id;
    roll(&mut h, RollAction::InsertNote(note(r.offset + 120, 60, 80)));
    let a = agree(&mut h, id);
    let (_, b) = track_list(&mut h, copy).unwrap();
    assert!(a.contains(&(r.start + 120, 80, 0)), "{kind:?} {a:?}");
    assert!(
        b.contains(&(far + 120, 80, 0)),
        "{kind:?} the linked copy: {b:?}"
    );
    undo(&mut h);
    let (_, b) = track_list(&mut h, copy).unwrap();
    assert!(!b.iter().any(|n| n.1 == 80), "{kind:?} undo in both");
    undo(&mut h);
    assert!(track_list(&mut h, copy).is_none());

    // The opened clip deleted in the Track List: the roll stops editing it
    // (with a sentence) and edits the row's track; undo brings the clip back.
    h.clip(ClipIntent::Delete(vec![id]));
    h.app.sync_clip_session();
    assert!(h.app.clip_session().is_none(), "{kind:?}");
    assert!(track_list(&mut h, id).is_none());
    assert_eq!(h.app.edit_target(), NoteTarget::Track(host));
    assert!(h.status().contains("no longer exists"), "{}", h.status());
    undo(&mut h);
    h.clip(ClipIntent::Open(id));
    assert_eq!(agree(&mut h, id), base, "{kind:?} undo delete clip");
}

#[test]
fn an_opened_owned_clip_and_the_track_list_show_every_edit_of_the_other() {
    matrix(Kind::Owned);
}

#[test]
fn an_opened_track_clip_and_the_track_list_show_every_edit_of_the_other() {
    matrix(Kind::TrackClip);
}

/// The roll's Onion ghosts are the other tracks' clips where they play
/// (V09-S11 step 4): one source per placement of an Onion track, drawn with
/// the placement's shift and window — so every ghost is where the Track
/// List draws the clip — and, before any clip is placed, the tracks' own
/// notes exactly as in v0.8.
#[test]
fn onion_ghosts_are_the_other_tracks_clips_where_they_play() {
    let mut h = Harness::new(song());
    let cfg = h.doc().project().onion.clone();
    let mut order = super::hierarchy::OnionOrderCache::default();
    // v0.8 projects: each Onion track once, its own notes where they are.
    {
        let doc = h.doc();
        let s = super::onion_sources(doc, 1, &[0, 2], &mut order, &cfg, (0, 10_000));
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|s| s.place == SourcePlace::IDENTITY));
        assert!(std::ptr::eq(s[0].store, &doc.track(0).unwrap().notes));
    }
    // Track 0's clip moved to 960, trimmed, and a linked copy at 9,600;
    // track 2 gets an added clip with two notes.
    let t0 = ClipId::Primary(h.id(0));
    h.clip(ClipIntent::Move(vec![(t0, 0, 960)]));
    h.clip(ClipIntent::Resize {
        id: t0,
        start: 1_440,
        offset: 480,
        length: Some(1_920),
    });
    h.clip(ClipIntent::Duplicate {
        copies: vec![(t0, 0, 9_600)],
        linked: true,
    });
    h.clip(ClipIntent::Add {
        track: 2,
        start: 4_800,
        length: 1_920,
    });
    let added = h.doc().clips().contents()[0].id;
    let added_id = h.doc().clips().placements_of(ContentRef::Owned(added))[0].id;
    h.clip(ClipIntent::Open(added_id));
    roll(&mut h, RollAction::InsertNote(note(0, 240, 90)));
    roll(&mut h, RollAction::InsertNote(note(960, 240, 91)));
    // The roll edits track 1's own notes; tracks 0 and 2 are ghosts.
    h.set_track(1);
    let generation = h.app.doc_generation();
    let doc = h.app.open.as_ref().unwrap().doc.clone();
    let m = h.app.track_list.model(&doc, generation);
    let s = super::onion_sources(&doc, generation, &[0, 2], &mut order, &cfg, (0, 20_000));
    let rects: Vec<&ClipRect> = [0, 2].iter().flat_map(|&t| m.row(t)).collect();
    assert_eq!(s.len(), rects.len(), "one ghost source per clip");
    for r in rects {
        let len = r.end - r.start;
        let want = SourcePlace {
            shift: i128::from(r.start) - i128::from(r.offset),
            window: (r.offset, r.offset + len),
        };
        let (store, _) = track_list::clip_store(&doc, r).unwrap();
        let src = s
            .iter()
            .find(|s| std::ptr::eq(s.store, store) && s.place.shift == want.shift)
            .unwrap_or_else(|| panic!("a ghost for {r:?}"));
        // Open windows reach the end of time; closed ones are the clip's.
        if r.content == ContentRef::Owned(added) || r.id == t0 {
            assert_eq!(src.place.window, want.window, "{r:?}");
        }
        // Every note the clip plays is a ghost where the Track List draws it.
        let ghosts: Vec<u64> = store
            .iter()
            .filter_map(|n| src.place.start(n.start_tick))
            .collect();
        let shown: Vec<u64> = store
            .iter()
            .filter(|n| n.start_tick >= r.offset && n.start_tick - r.offset < len)
            .map(|n| r.start + (n.start_tick - r.offset))
            .collect();
        assert_eq!(ghosts, shown, "{r:?}");
        assert!(!ghosts.is_empty(), "{r:?}");
    }
}

/// Dense First ranks the Onion tracks by the notes their clips play in the
/// view (song time), not by their own notes where they are: track 0 has 4
/// notes at song 0 – 3,840, track 2 has 8; with track 0's clip moved to song
/// 40,000 and a linked copy of it at 48,000, a view over 40,000 – 52,000
/// puts track 0 (8 notes there) in front of track 2 (none), and over song
/// 0 track 2 is in front. Each track's clips stay together, back to front.
#[test]
fn dense_first_ranks_onion_tracks_by_what_their_clips_play_in_view() {
    use plumspirit_engine::model::project::OnionOrder;
    let mut d = song();
    let extra: Vec<NoteRecord> = (0..4u64).map(|k| note(480 + k * 960, 240, 70)).collect();
    d.track_mut(2)
        .unwrap()
        .notes
        .insert_sorted_batch(&extra, false)
        .unwrap();
    let mut h = Harness::new(d);
    let t0 = ClipId::Primary(h.id(0));
    h.clip(ClipIntent::Move(vec![(t0, 0, 40_000)]));
    h.clip(ClipIntent::Duplicate {
        copies: vec![(t0, 0, 48_000)],
        linked: true,
    });
    let mut cfg = h.doc().project().onion.clone();
    cfg.order = OnionOrder::DenseFirst;
    let generation = h.app.doc_generation();
    let doc = h.app.open.as_ref().unwrap().doc.clone();
    let mut order = super::hierarchy::OnionOrderCache::default();
    let tracks = |s: &[crate::piano_roll::render::OnionSource<'_>]| -> Vec<u32> {
        s.iter().map(|s| s.id.0).collect()
    };
    let (a, b) = (doc.track(0).unwrap().id().0, doc.track(2).unwrap().id().0);
    let s = super::onion_sources(
        &doc,
        generation,
        &[0, 2],
        &mut order,
        &cfg,
        (40_000, 52_000),
    );
    assert_eq!(tracks(&s), vec![b, a, a], "track 0's clips in front");
    // (A fresh ranking: small count changes are held by the hysteresis.)
    let mut order = super::hierarchy::OnionOrderCache::default();
    let s = super::onion_sources(&doc, generation, &[0, 2], &mut order, &cfg, (0, 4_000));
    assert_eq!(tracks(&s), vec![a, a, b], "track 2 in front over song 0");
}

/// Onion Skin through placements at scale (V09-S11 step 4): 1,000 tracks ×
/// 1,000 notes, every track with 9 more clips of its own notes (10,000
/// clips), 999 Onion tracks. Prints the per-frame cost of building the
/// sources (one `placed_contents_by_track` pass, 9,990 ghost sources) and of
/// drawing them (dense tiles once built, inline here; exact rectangles when
/// zoomed in), next to the same project before clips (999 identity sources).
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn onion_with_clips_frame_cost() {
    use crate::diagnostics::RenderStats;
    use crate::piano_roll::Geometry;
    use crate::piano_roll::dense::DenseRenderer;
    use crate::piano_roll::render::{OnionCache, OnionPass, draw_onion};
    use crate::piano_roll::viewport::RollView;
    use plumspirit_engine::history::{History, HistoryLimits, Operation};
    use plumspirit_engine::model::ClipPlacement;
    use plumspirit_engine::view::{PitchAxis, TimeAxis};
    use std::time::{Duration, Instant};
    let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 960);
    for _ in 1..1000 {
        d.add_track().unwrap();
    }
    for ti in 0..1000usize {
        let recs: Vec<NoteRecord> = (0..1000u64)
            .map(|k| {
                note(
                    k * 60 + (ti as u64 % 7) * 13,
                    50,
                    40 + ((k + ti as u64) % 60) as u8,
                )
            })
            .collect();
        d.track_mut(ti)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let plain = d.clone();
    let mut layout = d.clips().layout();
    for ti in 0..1000usize {
        let id = d.track(ti).unwrap().id();
        for k in 0..9u64 {
            let n = layout.next_clip;
            layout.next_clip += 1;
            layout.placements.push(ClipPlacement {
                id: ClipId::Placed(n),
                track: id,
                content: ContentRef::Track(id),
                start: 61_000 + k * 61_000,
                offset: k * 960,
                length: Some(30_000 + k * 960),
                muted: k == 4,
                name: None,
                color: None,
                revision: 0,
            });
        }
    }
    layout.placements.sort_by_key(|p| p.id);
    History::new(HistoryLimits::default())
        .apply(
            &mut d,
            Operation::SetClips {
                layout: Box::new(layout),
                contents: Vec::new(),
                label: "Add Clips",
            },
        )
        .unwrap();
    let ghosts: Vec<usize> = (1..1000).collect();
    let cfg = d.project().onion.clone();
    let song_end = 61_000 * 10 + 40_000;
    let full = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
    let geometry = |ppt: f64| {
        let notes = egui::Rect::from_min_max(egui::pos2(80.0, 60.0), egui::pos2(1588.0, 888.0));
        Geometry {
            band: egui::Rect::from_min_max(egui::pos2(80.0, 0.0), egui::pos2(1588.0, 30.0)),
            ruler: egui::Rect::from_min_max(egui::pos2(80.0, 30.0), egui::pos2(1588.0, 60.0)),
            keys: egui::Rect::from_min_max(egui::pos2(0.0, 60.0), egui::pos2(80.0, 888.0)),
            notes,
            vscroll: egui::Rect::from_min_max(egui::pos2(1588.0, 60.0), full.max),
            hscroll: egui::Rect::from_min_max(egui::pos2(80.0, 888.0), full.max),
            view: RollView {
                time: TimeAxis::new(0, ppt),
                pitch: PitchAxis {
                    scroll_rows: 40.0,
                    row_height: 7.0,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            },
            ppp: 1.0,
        }
    };
    let median = |mut v: Vec<Duration>| {
        v.sort();
        v[v.len() / 2].as_secs_f64() * 1e3
    };
    for (name, doc) in [("before clips", &plain), ("10,000 clips", &d)] {
        let mut order = super::hierarchy::OnionOrderCache::default();
        let mut times = Vec::new();
        let mut count = 0;
        for _ in 0..60 {
            let t = Instant::now();
            count = super::onion_sources(doc, 1, &ghosts, &mut order, &cfg, (0, song_end)).len();
            times.push(t.elapsed());
        }
        println!(
            "onion sources, {name}: {count} sources, median {:.3} ms",
            median(times)
        );
        let sources = super::onion_sources(doc, 1, &ghosts, &mut order, &cfg, (0, song_end));
        for (zoom, ppt) in [
            ("whole song", 1500.0 / song_end as f64),
            ("a few bars", 0.05),
        ] {
            let g = geometry(ppt);
            let ctx = egui::Context::default();
            let pass = OnionPass {
                palette: crate::ui::theme::default_note_palette(),
                sources: &sources,
                dark: true,
                alpha: 90,
                outlines: Some(75),
            };
            let mut cache = OnionCache::default();
            let mut dense = DenseRenderer::new(true);
            let mut times = Vec::new();
            let mut stats = RenderStats::default();
            let t_first = Instant::now();
            let mut first = Duration::ZERO;
            for frame in 0..62 {
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let p = ui.ctx().layer_painter(egui::LayerId::background());
                    stats = RenderStats::default();
                    dense.begin_frame();
                    let t = Instant::now();
                    if frame >= 2 {
                        steady_onion_frame(
                            &p,
                            ui.ctx(),
                            &g,
                            &pass,
                            &mut cache,
                            &mut dense,
                            &mut stats,
                        );
                        times.push(t.elapsed());
                    } else {
                        draw_onion(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
                    }
                });
                out.textures_delta.clear();
                if frame == 1 {
                    first = t_first.elapsed();
                }
            }
            println!(
                "onion draw, {name}, {zoom}: first two frames {:.1} ms (inline tile builds), median {:.3} ms ({} visible tiles, {} exact primitives, estimate {})",
                first.as_secs_f64() * 1e3,
                median(times),
                stats.onion_dense.visible_tiles,
                if stats.onion_dense.visible_tiles == 0 {
                    stats.onion_primitives
                } else {
                    0
                },
                stats.onion_estimate
            );
        }
    }
}

/// One Onion frame once its tiles are built (a symbol of its own, for
/// profiling the steady state apart from the builds).
#[inline(never)]
fn steady_onion_frame(
    p: &egui::Painter,
    ctx: &egui::Context,
    g: &crate::piano_roll::Geometry,
    pass: &crate::piano_roll::render::OnionPass<'_>,
    cache: &mut crate::piano_roll::render::OnionCache,
    dense: &mut crate::piano_roll::dense::DenseRenderer,
    stats: &mut crate::diagnostics::RenderStats,
) {
    crate::piano_roll::render::draw_onion(p, ctx, g, pass, cache, dense, stats);
}

/// L-G05 baseline (L-S25): "activating onion skin for all tracks basically
/// froze plumspirit" (15,178 tracks, 2B notes). The UI thread's share of an
/// all-track Onion at many tracks (default 15,000 × 2,000 notes = 30M;
/// env PLUMSPIRIT_BENCH_ONION_TRACKS / _NOTES): building the sources (order
/// cache), the activation frame (family, hashing) and steady frames while
/// the worker builds tiles. Tile builds run on the workers, not timed here.
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn all_track_onion_frame_cost() {
    use crate::diagnostics::RenderStats;
    use crate::piano_roll::Geometry;
    use crate::piano_roll::dense::DenseRenderer;
    use crate::piano_roll::render::{OnionCache, OnionPass, draw_onion};
    use crate::piano_roll::viewport::RollView;
    use plumspirit_engine::view::{PitchAxis, TimeAxis};
    use std::time::{Duration, Instant};
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let tracks = env("PLUMSPIRIT_BENCH_ONION_TRACKS", 15_000);
    let per = env("PLUMSPIRIT_BENCH_ONION_NOTES", 2_000) as u64;
    let t = Instant::now();
    let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 1920);
    for _ in 1..tracks {
        d.add_track().unwrap();
    }
    let song_end = 1920 * 4 * 300u64;
    let step = song_end / per;
    for ti in 0..tracks {
        let recs: Vec<NoteRecord> = (0..per)
            .map(|k| {
                note(
                    k * step + (ti as u64 * 7) % step,
                    step / 2 + 1,
                    20 + ((k + ti as u64) % 90) as u8,
                )
            })
            .collect();
        d.track_mut(ti)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    println!(
        "{tracks} tracks x {per} notes ({} M) built in {:?}",
        tracks as u64 * per / 1_000_000,
        t.elapsed()
    );
    let ghosts: Vec<usize> = (1..tracks).collect();
    let cfg = d.project().onion.clone();
    let median = |mut v: Vec<Duration>| {
        v.sort();
        v[v.len() / 2].as_secs_f64() * 1e3
    };
    let mut order = super::hierarchy::OnionOrderCache::default();
    let t = Instant::now();
    let first = super::onion_sources(&d, 1, &ghosts, &mut order, &cfg, (0, song_end)).len();
    let first_t = t.elapsed();
    let mut times = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        super::onion_sources(&d, 1, &ghosts, &mut order, &cfg, (0, song_end));
        times.push(t.elapsed());
    }
    println!(
        "onion sources: {first} sources, first {:.1} ms, then median {:.3} ms",
        first_t.as_secs_f64() * 1e3,
        median(times)
    );
    let sources = super::onion_sources(&d, 1, &ghosts, &mut order, &cfg, (0, song_end));
    let full = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
    let g = Geometry::new(
        full,
        RollView {
            time: TimeAxis::new(song_end / 3, 1500.0 / (1920.0 * 16.0)),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: 6.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        },
        1.0,
    );
    let pass = OnionPass {
        palette: crate::ui::theme::default_note_palette(),
        sources: &sources,
        dark: true,
        alpha: 90,
        outlines: Some(75),
    };
    let ctx = egui::Context::default();
    let mut cache = OnionCache::default();
    let mut dense = DenseRenderer::new(false);
    let mut stats = RenderStats::default();
    let mut times = Vec::new();
    let mut ready_at = None;
    for frame in 0..400 {
        let mut ui_t = Duration::ZERO;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            stats = RenderStats::default();
            dense.begin_frame();
            let t = Instant::now();
            draw_onion(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
            ui_t = t.elapsed();
        });
        out.textures_delta.clear();
        times.push(ui_t);
        let st = stats.onion_dense;
        if ready_at.is_none() && st.visible_tiles > 0 && st.ready == st.visible_tiles {
            ready_at = Some(frame + 1);
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let mut steady = Vec::new();
    for _ in 0..30 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            stats = RenderStats::default();
            dense.begin_frame();
            let t = Instant::now();
            steady_onion_frame(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
            steady.push(t.elapsed());
        });
        out.textures_delta.clear();
    }
    println!(
        "draw_onion UI thread once every tile is ready: median {:.3} ms",
        median(steady)
    );
    // Its parts (L-S25 profile).
    let (t0, t1) = g.view.time.visible_range(f64::from(g.notes.width()));
    let mut est = Vec::new();
    let mut fam = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        let n: usize = sources
            .iter()
            .map(|s| s.store.estimate_in_range(t0, t1))
            .sum();
        est.push(t.elapsed());
        std::hint::black_box(n);
        let t = Instant::now();
        std::hint::black_box(crate::piano_roll::render::onion_family(
            &pass,
            6.0,
            plumspirit_engine::model::project::KeyRange::from_top(
                plumspirit_engine::model::note::MAX_PITCH,
            ),
        ));
        fam.push(t.elapsed());
    }
    let t = Instant::now();
    let stores: Vec<std::sync::Arc<plumspirit_engine::model::NoteStore>> = sources
        .iter()
        .map(|s| std::sync::Arc::new(s.store.clone()))
        .collect();
    let clone_t = t.elapsed();
    let t = Instant::now();
    let colors = crate::piano_roll::render::onion_tile_colors(&pass);
    println!(
        "  activation parts: stores cloned {:.2} ms, tile colors {:.2} ms",
        clone_t.as_secs_f64() * 1e3,
        t.elapsed().as_secs_f64() * 1e3
    );
    drop((stores, colors));
    let id = crate::piano_roll::render::onion_family(
        &pass,
        6.0,
        plumspirit_engine::model::project::KeyRange::from_top(
            plumspirit_engine::model::note::MAX_PITCH,
        ),
    );
    let mut dd = Vec::new();
    for _ in 0..30 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            dense.begin_frame();
            let t = Instant::now();
            dense.draw(
                &p,
                ui.ctx(),
                &g,
                crate::piano_roll::dense::Role::Onion,
                id,
                || unreachable!("the family exists"),
            );
            dd.push(t.elapsed());
        });
        out.textures_delta.clear();
    }
    println!(
        "  parts: estimate loop median {:.3} ms, onion_family median {:.3} ms, dense.draw median {:.3} ms",
        median(est),
        median(fam),
        median(dd)
    );
    println!(
        "draw_onion UI thread: activation frame {:.1} ms, max {:.1} ms, median {:.3} ms over {} frames; all visible tiles ready at frame {ready_at:?} (last tile {} us, {} visible)",
        times[0].as_secs_f64() * 1e3,
        times.iter().max().unwrap().as_secs_f64() * 1e3,
        median(times.clone()),
        times.len(),
        stats.onion_dense.last_build_micros,
        stats.onion_dense.visible_tiles
    );
    // L-S49: a new family of the same sources (the opacity slider moved one
    // step): its sources made (`make_micros`) and the whole frame.
    let mut changed = Vec::new();
    let mut made = Vec::new();
    for k in 0..6u8 {
        let pass = OnionPass {
            palette: pass.palette,
            sources: &sources,
            dark: true,
            alpha: 91 + k,
            outlines: Some(75),
        };
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            stats = RenderStats::default();
            dense.begin_frame();
            let t = Instant::now();
            draw_onion(&p, ui.ctx(), &g, &pass, &mut cache, &mut dense, &mut stats);
            changed.push(t.elapsed());
            made.push(Duration::from_micros(stats.onion_dense.make_micros));
        });
        out.textures_delta.clear();
    }
    println!(
        "opacity step (new family, same sources): frame median {:.2} ms, sources made median {:.2} ms",
        median(changed),
        median(made)
    );
}

/// L-G05 (L-S25): the whole app's frames when "Onion Skin: All Other
/// Tracks" is switched on in a project of many tracks (default 15,000 ×
/// 2,000 notes; env PLUMSPIRIT_BENCH_ONION_TRACKS / _NOTES), Piano Roll at
/// a 16-beat view, a frame every 16 ms while the workers build tiles: the
/// activation frame, the worst and the median frame, and the steady frame.
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn all_track_onion_app_frame_cost() {
    use plumspirit_engine::model::OnionMode;
    use std::time::{Duration, Instant};
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let tracks = env("PLUMSPIRIT_BENCH_ONION_TRACKS", 15_000);
    let per = env("PLUMSPIRIT_BENCH_ONION_NOTES", 2_000) as u64;
    let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 1920);
    for _ in 1..tracks {
        d.add_track().unwrap();
    }
    let song_end = 1920 * 4 * 300u64;
    let step = song_end / per;
    for ti in 0..tracks {
        let recs: Vec<NoteRecord> = (0..per)
            .map(|k| {
                note(
                    k * step + (ti as u64 * 7) % step,
                    step / 2 + 1,
                    20 + ((k + ti as u64) % 90) as u8,
                )
            })
            .collect();
        d.track_mut(ti)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let mut h = Harness::new(d);
    let size = egui::vec2(1600.0, 900.0);
    for _ in 0..3 {
        h.app_frame(size, 1.0);
    }
    h.app.main_view = track_list::MainView::PianoRoll;
    for _ in 0..3 {
        h.app_frame(size, 1.0);
    }
    // PLUMSPIRIT_BENCH_ONION_FIT=1: the whole song in view (the Fit button),
    // as on the owner's PC in E0075; otherwise a 16-beat view.
    h.app.roll.view.time = if env("PLUMSPIRIT_BENCH_ONION_FIT", 0) == 1 {
        plumspirit_engine::view::TimeAxis::new(0, 1300.0 / song_end as f64)
    } else {
        plumspirit_engine::view::TimeAxis::new(song_end / 3, 1300.0 / (1920.0 * 16.0))
    };
    for _ in 0..30 {
        h.app_frame(size, 1.0);
        std::thread::sleep(Duration::from_millis(16));
    }
    let median = |mut v: Vec<Duration>| {
        v.sort();
        v[v.len() / 2].as_secs_f64() * 1e3
    };
    let mut base = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        h.app_frame(size, 1.0);
        base.push(t.elapsed());
    }
    h.command(Command::Onion(OnionMode::AllOthers));
    let mut times = Vec::new();
    let mut ready_at = None;
    for frame in 0..600 {
        let t = Instant::now();
        h.app_frame(size, 1.0);
        times.push(t.elapsed());
        if frame == 0 && env("PLUMSPIRIT_BENCH_ONION_STATS", 0) == 1 {
            println!("activation frame stats: {:?}", h.app.roll.stats);
        }
        let st = h.app.roll.stats.onion_dense;
        if ready_at.is_none() && st.visible_tiles > 0 && st.ready == st.visible_tiles {
            ready_at = Some(frame + 1);
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let mut steady = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        h.app_frame(size, 1.0);
        steady.push(t.elapsed());
    }
    // PLUMSPIRIT_BENCH_ONION_REPEAT=n: switch Onion off and on n more times
    // and time each activation frame (caches kept vs rebuilt).
    for k in 0..env("PLUMSPIRIT_BENCH_ONION_REPEAT", 0) {
        h.command(Command::Onion(OnionMode::Off));
        h.app_frame(size, 1.0);
        h.command(Command::Onion(OnionMode::AllOthers));
        let t = Instant::now();
        h.app_frame(size, 1.0);
        println!(
            "re-activation {k}: {:.1} ms",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    let st = h.app.roll.stats;
    println!(
        "onion {:?}, {} ghosts, estimate {}, dense {:?}, view {:?}",
        h.doc().project().onion.mode,
        h.app.onion_ghosts().len(),
        st.onion_estimate,
        st.onion_dense,
        h.app.roll.view.time
    );
    println!(
        "{tracks} tracks x {per} notes: app frame before Onion median {:.2} ms; Onion All Others: activation frame {:.1} ms, worst {:.1} ms, median {:.2} ms over {} frames until every visible tile was ready ({ready_at:?}); steady median {:.2} ms",
        median(base),
        times[0].as_secs_f64() * 1e3,
        times.iter().max().unwrap().as_secs_f64() * 1e3,
        median(times.clone()),
        times.len(),
        median(steady)
    );
}
