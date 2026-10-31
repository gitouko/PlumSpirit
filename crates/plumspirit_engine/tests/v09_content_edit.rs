//! V09-S11 step 2b (contract §041, §042; `docs/CLIP_PIANO_ROLL.md`): every
//! note edit — the direct operations, prepared edits (move, Flip, Quantize,
//! paste), a transform tool and a controller edit — can target an owned
//! content edited on a track (`NoteTarget::Content`). The content's one
//! store changes; the tracks' own notes and other contents never do. Undo /
//! redo are exact, the journal replays the steps after a restart, linked
//! placements play the edit, only that content's revision moves, and a
//! deleted content is refused, changing nothing.

use plumspirit_engine::editor::clips::{delete_clips, duplicate_clips};
use plumspirit_engine::editor::{
    self, Clipboard, ControllerKind, EditControl, EditError, NoteTarget, QuantizeMode, QuantizeSpec,
};
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryError, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, ClipId, ContentId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument,
    RankSetBuilder, Subchannel, SubchannelId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use plumspirit_engine::transform::registry::BuildCtx;
use plumspirit_engine::transform::run::{build_operation, collect_for};
use plumspirit_engine::transform::{
    Ctx, Limits, ParamSet, ParamValue, TrackScope, run_kernel, tool,
};
use std::path::{Path, PathBuf};

fn note(t: u64, p: u8, v: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        240,
        NoteAttrs::new(p, v, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

/// Two tracks with notes; track 1's notes are copied into content A (on
/// track 1 at 10,000, and linked onto track 0 at 20,000), track 2's into
/// content B (on track 0 at 30,000).
struct Song {
    doc: PlumDocument,
    a: ContentId,
    b: ContentId,
}

fn song(h: &mut History) -> Song {
    let mut d = PlumDocument::new_project("content".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    for (i, n) in [(0usize, 32u64), (1, 16)] {
        let recs: Vec<NoteRecord> = (0..n)
            .map(|k| note(k * 120 + 7, 40 + (k % 24) as u8 + i as u8, 60 + k as u8))
            .collect();
        d.track_mut(i)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
    }
    let t0 = d.track(0).unwrap().id();
    let t1 = d.track(1).unwrap().id();
    let op = duplicate_clips(&d, &[(ClipId::Primary(t0), 1, 10_000)], false).unwrap();
    h.apply(&mut d, op).unwrap();
    let a = d.clips().contents()[0].id;
    let placed = d.clips().placements_of(ContentRef::Owned(a))[0].id;
    let op = duplicate_clips(&d, &[(placed, 0, 20_000)], true).unwrap();
    h.apply(&mut d, op).unwrap();
    let op = duplicate_clips(&d, &[(ClipId::Primary(t1), 0, 30_000)], false).unwrap();
    h.apply(&mut d, op).unwrap();
    let b = d.clips().contents().iter().find(|c| c.id != a).unwrap().id;
    assert_eq!(d.clips().content(a).unwrap().notes.len(), 32);
    assert_eq!(d.clips().content(b).unwrap().notes.len(), 16);
    Song { doc: d, a, b }
}

/// Everything an edit of content A must leave alone: both tracks' own notes
/// and content B, as records.
fn untouched(d: &PlumDocument, b: ContentId) -> Vec<Vec<NoteRecord>> {
    let mut v: Vec<Vec<NoteRecord>> = (0..d.track_count())
        .map(|i| {
            d.track(i)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.record())
                .collect()
        })
        .collect();
    v.push(
        d.clips()
            .content(b)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect(),
    );
    v
}

fn select_all(d: &mut PlumDocument, target: NoteTarget) {
    editor::notes_mut(d, target).unwrap().select_all();
}

fn ranks(r: &[usize]) -> plumspirit_engine::model::RankSet {
    let mut b = RankSetBuilder::default();
    for &k in r {
        b.push(k);
    }
    b.finish()
}

/// One edit of the sequence: selection set up first (not undoable state),
/// then the operation.
type Step = (&'static str, fn(&mut PlumDocument, NoteTarget) -> Operation);

fn steps() -> Vec<Step> {
    vec![
        ("draw", |_, t| Operation::InsertNote {
            track: t,
            record: note(5_000, 70, 90),
        }),
        ("velocity", |d, t| {
            select_all(d, t);
            Operation::SetVelocity {
                track: t,
                velocity: 33,
            }
        }),
        ("gate", |_, t| Operation::SetDuration {
            track: t,
            duration: 100,
        }),
        ("resize", |_, t| Operation::Resize {
            track: t,
            delta: 20,
        }),
        ("channel", |_, t| Operation::SetChannel {
            track: t,
            channel: 5,
        }),
        ("add subchannel to the host", |_, _| {
            Operation::AddSubchannels {
                track: 1,
                subs: vec![Subchannel::new(SubchannelId(1), 3)],
            }
        }),
        ("subchannel", |_, t| Operation::SetSubchannel {
            track: t,
            sub: SubchannelId(1),
            set_channel: true,
        }),
        ("velocities", |_, t| Operation::SetVelocities {
            track: t,
            ranks: ranks(&[0, 1, 2]),
            values: vec![10, 20, 30],
        }),
        ("erase", |_, t| Operation::EraseNotes {
            track: t,
            ranks: ranks(&[3, 4]),
        }),
        ("move", |d, t| {
            select_all(d, t);
            Operation::Prepared {
                edit: editor::prepare_move(d, t, 120, 2, EditControl::default()).unwrap(),
                label: "Move",
            }
        }),
        ("transpose only", |d, t| Operation::Prepared {
            edit: editor::prepare_move(d, t, 0, -1, EditControl::default()).unwrap(),
            label: "Move",
        }),
        ("flip", |d, t| Operation::Prepared {
            edit: editor::prepare_flip_x(d, t, EditControl::default()).unwrap(),
            label: "Flip X",
        }),
        ("quantize", |d, t| Operation::Prepared {
            edit: editor::prepare_quantize(
                d,
                t,
                &QuantizeSpec {
                    mode: QuantizeMode::Ticks,
                    grid: 240,
                    origin: 0,
                },
                EditControl::default(),
            )
            .unwrap(),
            label: "Quantize",
        }),
        ("paste", |d, t| {
            let view = editor::note_view(d, t).unwrap();
            let clip = Clipboard::copy_selected(view).unwrap().unwrap();
            Operation::Prepared {
                edit: clip.prepare_paste(d, t, 60_000).unwrap(),
                label: "Paste",
            }
        }),
        ("transform tool", |d, t| {
            let def = tool("transpose").unwrap();
            let mut values = ParamSet::defaults(&(def.spec)().params);
            values.set("semis", ParamValue::Int(5));
            let scope = [TrackScope::of(t, false)];
            let src = collect_for(def, d, &scope, 10_000_000).unwrap();
            let ctx = Ctx::simple(960);
            let b = BuildCtx {
                ctx: &ctx,
                stats: &src.stats,
                tracks: &src.tracks,
            };
            let k = def.kernel(&values, &b).unwrap();
            let run = run_kernel(
                k.as_ref(),
                &src,
                def.boundary(&values),
                &Limits::default(),
                None,
            )
            .unwrap();
            build_operation(d, &run, "Transpose").unwrap()
        }),
        ("controller", |d, t| {
            let view = editor::note_view(d, t).unwrap();
            Operation::Events {
                edit: editor::controllers::prepare_set(
                    view,
                    ControllerKind::Cc(7),
                    0,
                    None,
                    480,
                    100,
                )
                .unwrap(),
                label: "Set Controller",
            }
        }),
        ("delete", |d, t| {
            editor::notes_mut(d, t)
                .unwrap()
                .select_ranks(&ranks(&[0, 5]))
                .unwrap();
            Operation::DeleteSelected { track: t }
        }),
    ]
}

/// Runs every step on content A (edited on track 1), checking that each is
/// one undo step that changes A only; returns the state after each step.
fn edit_content(s: &mut Song, h: &mut History) -> Vec<u64> {
    let target = NoteTarget::Content {
        host: 1,
        content: s.a,
    };
    let mut states = vec![s.doc.state_fingerprint()];
    for (name, step) in steps() {
        let keep = untouched(&s.doc, s.b);
        let host_subs = s.doc.track(1).unwrap().subchannels().len();
        let rev = s.doc.content_revision(ContentRef::Owned(s.a)).unwrap();
        let others = (
            s.doc.content_revision(ContentRef::Owned(s.b)).unwrap(),
            s.doc
                .content_revision(ContentRef::Track(s.doc.track(0).unwrap().id()))
                .unwrap(),
            s.doc
                .content_revision(ContentRef::Track(s.doc.track(1).unwrap().id()))
                .unwrap(),
        );
        let before = h.undo_len();
        let op = step(&mut s.doc, target);
        h.apply(&mut s.doc, op)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(h.undo_len(), before + 1, "{name}: one undo step");
        assert_eq!(
            untouched(&s.doc, s.b),
            keep,
            "{name}: only content A changes"
        );
        assert_eq!(
            (
                s.doc.content_revision(ContentRef::Owned(s.b)).unwrap(),
                s.doc
                    .content_revision(ContentRef::Track(s.doc.track(0).unwrap().id()))
                    .unwrap(),
                s.doc
                    .content_revision(ContentRef::Track(s.doc.track(1).unwrap().id()))
                    .unwrap(),
            ),
            others,
            "{name}: other contents keep their revision"
        );
        if s.doc.track(1).unwrap().subchannels().len() == host_subs {
            assert_ne!(
                s.doc.content_revision(ContentRef::Owned(s.a)).unwrap(),
                rev,
                "{name}: content A's revision moves"
            );
        }
        s.doc.validate_clips().unwrap();
        let st = s.doc.state_fingerprint();
        assert_ne!(Some(&st), states.last(), "{name}: the document changed");
        states.push(st);
    }
    states
}

fn undo_redo_exactly(d: &mut PlumDocument, h: &mut History, states: &[u64]) {
    for want in states.iter().rev().skip(1) {
        h.undo(d).unwrap();
        assert_eq!(d.state_fingerprint(), *want);
        d.validate_clips().unwrap();
    }
    for want in states.iter().skip(1) {
        h.redo(d).unwrap();
        assert_eq!(d.state_fingerprint(), *want);
    }
}

#[test]
fn every_kind_of_note_edit_changes_only_the_owned_content_with_exact_undo() {
    let mut h = History::default();
    let mut s = song(&mut h);
    let a0: Vec<NoteRecord> = s
        .doc
        .clips()
        .content(s.a)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect();
    let states = edit_content(&mut s, &mut h);
    let a = s.doc.clips().content(s.a).unwrap();
    assert_ne!(a.notes.iter().map(|n| n.record()).collect::<Vec<_>>(), a0);
    // The controller went to the content's events, not the track's.
    assert_eq!(a.events.len(), 1);
    assert_eq!(s.doc.track(1).unwrap().events.len(), 0);
    // The subchannel move used the hosting track's table (channel 3).
    assert!(a.notes.iter().any(|n| n.sub == SubchannelId(1)));
    undo_redo_exactly(&mut s.doc, &mut h, &states);
}

#[test]
fn a_track_target_still_edits_the_tracks_own_notes() {
    let mut h = History::default();
    let mut s = song(&mut h);
    let a = untouched(&s.doc, s.a);
    select_all(&mut s.doc, NoteTarget::Track(0));
    h.apply(
        &mut s.doc,
        Operation::SetVelocity {
            track: 0.into(),
            velocity: 11,
        },
    )
    .unwrap();
    assert!(
        s.doc
            .track(0)
            .unwrap()
            .notes
            .iter()
            .all(|n| n.attrs.velocity() == 11)
    );
    let after = untouched(&s.doc, s.a);
    assert_ne!(after[0], a[0]);
    assert_eq!(after[1..], a[1..], "track 1 and content A unchanged");
}

// ------------------------------------------------------------- journal

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plumspirit-content-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn reopen(file: &Path, jpath: &Path) -> (PlumDocument, History) {
    let (doc, _) = load_project(file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(jpath, path_key(file)).unwrap().unwrap();
    h.restore_journal(j, scan, identity(&doc)).unwrap();
    (doc, h)
}

/// The journal writes a content host (`2^32 + id`) for every step: after a
/// restart each one undoes and redoes exactly.
#[test]
fn content_edits_survive_a_restart_in_the_journal() {
    let d0 = dir("restart");
    let file = d0.join("song.lup");
    let jpath = d0.join("song.plh");
    let mut h = History::new(HistoryLimits::default());
    let mut s = song(&mut h);
    save_project(&s.doc, &file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(identity(&s.doc))).unwrap();
    let states = edit_content(&mut s, &mut h);
    save_project(&s.doc, &file, LupControl::default()).unwrap();
    let st = h.current_state();
    h.mark_saved(st);
    h.persist_saved(st, identity(&s.doc));
    drop(h);
    let (mut doc, mut h) = reopen(&file, &jpath);
    assert_eq!(doc.state_fingerprint(), *states.last().unwrap());
    for want in states.iter().rev().skip(1) {
        h.undo(&mut doc).unwrap();
        assert_eq!(doc.state_fingerprint(), *want);
    }
    assert!(!h.can_undo());
    while h.can_redo() {
        h.redo(&mut doc).unwrap();
    }
    assert_eq!(doc.state_fingerprint(), *states.last().unwrap());
    let _ = std::fs::remove_dir_all(&d0);
}

// ------------------------------------------------------------- §042

/// Notes the exported image plays (every placement flattened).
fn exported_notes(d: &PlumDocument) -> usize {
    let img = d.materialize_placements();
    (0..img.track_count())
        .map(|i| img.track(i).unwrap().notes.len())
        .sum()
}

#[test]
fn both_linked_placements_play_an_edit_of_their_content() {
    let mut h = History::default();
    let mut s = song(&mut h);
    assert_eq!(s.doc.clips().placements_of(ContentRef::Owned(s.a)).len(), 2);
    let before = exported_notes(&s.doc);
    // Edited through the placement on track 0 this time.
    let target = NoteTarget::Content {
        host: 0,
        content: s.a,
    };
    h.apply(
        &mut s.doc,
        Operation::InsertNote {
            track: target,
            record: note(100, 90, 90),
        },
    )
    .unwrap();
    assert_eq!(
        exported_notes(&s.doc),
        before + 2,
        "the one new note plays in both placements"
    );
    let placed: Vec<_> = (0..s.doc.track_count())
        .flat_map(|i| {
            s.doc
                .placed_contents(i)
                .into_iter()
                .filter(|p| p.placement.content == ContentRef::Owned(s.a))
                .map(|p| p.notes.len())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(placed, vec![33, 33], "both placements read the one store");
    h.undo(&mut s.doc).unwrap();
    assert_eq!(exported_notes(&s.doc), before);
}

// ------------------------------------------------------------- refusals

#[test]
fn an_edit_of_a_deleted_content_is_refused_and_changes_nothing() {
    let mut h = History::default();
    let mut s = song(&mut h);
    let target = NoteTarget::Content {
        host: 1,
        content: s.a,
    };
    select_all(&mut s.doc, target);
    // Prepared before the content goes away.
    let moved = editor::prepare_move(&s.doc, target, 10, 0, EditControl::default()).unwrap();
    let ids: Vec<ClipId> = s
        .doc
        .clips()
        .placements_of(ContentRef::Owned(s.a))
        .iter()
        .map(|p| p.id)
        .collect();
    let op = delete_clips(&s.doc, &ids).unwrap();
    h.apply(&mut s.doc, op).unwrap();
    assert!(s.doc.clips().content(s.a).is_none(), "no orphan content");
    let fp = s.doc.state_fingerprint();
    let n = h.undo_len();
    let stale = |r: Result<_, HistoryError>| {
        assert!(
            matches!(r, Err(HistoryError::Edit(EditError::Stale))),
            "{r:?}"
        )
    };
    stale(h.apply(
        &mut s.doc,
        Operation::Prepared {
            edit: moved,
            label: "Move",
        },
    ));
    stale(h.apply(
        &mut s.doc,
        Operation::InsertNote {
            track: target,
            record: note(0, 60, 60),
        },
    ));
    stale(h.apply(
        &mut s.doc,
        Operation::SetVelocity {
            track: target,
            velocity: 5,
        },
    ));
    stale(h.apply(&mut s.doc, Operation::DeleteSelected { track: target }));
    assert_eq!(
        editor::note_view(&s.doc, target).err(),
        Some(EditError::Stale)
    );
    assert_eq!(s.doc.state_fingerprint(), fp, "nothing changed");
    assert_eq!(h.undo_len(), n, "nothing recorded");
    // Undoing the delete brings the content back, and with it the target.
    h.undo(&mut s.doc).unwrap();
    assert!(editor::note_view(&s.doc, target).is_ok());
    // A host track that does not exist is refused like before.
    assert_eq!(
        editor::note_view(
            &s.doc,
            NoteTarget::Content {
                host: 9,
                content: s.a
            }
        )
        .err(),
        Some(EditError::NoSuchTrack)
    );
}
