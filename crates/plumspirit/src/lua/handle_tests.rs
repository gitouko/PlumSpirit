//! B1-0022 / B1-0023: stable handles with freshness checks, and exact
//! 64-bit ticks in Lua.

use super::handles::Resolved;
use super::*;
use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::editor::clips::add_clip;
use plumspirit_engine::editor::controllers::prepare_rename;
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, ClipId, ContentRef, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, RawEvent, RawKind,
    Subchannel,
};
use std::sync::Arc;

fn host() -> Host {
    Host::new(Limits::default(), [Capability::ReadProject]).expect("host")
}

fn project() -> (PlumDocument, History) {
    let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (d, h)
}

fn world(d: &PlumDocument, h: &mut History) -> World {
    h.observe_ids(d);
    World::new(Arc::new(d.clone()), h.incarnations().snapshot())
}

fn note(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 240,
        attrs: NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

/// `return <expr>` in a host that reads `w`, with the handle as global `x`.
fn eval(host: &Host, w: &World, x: Handle, expr: &str) -> Result<Plain, ScriptError> {
    host.set_world(Some(w.clone()));
    host.set_global_handle("x", x).unwrap();
    host.run("h.lua", &format!("return {expr}"))
}

fn expired(r: Result<Plain, ScriptError>) -> ScriptError {
    let e = r.expect_err("the handle should be expired");
    assert_eq!(e.category, Category::Expired, "{e}");
    assert_eq!(e.category.name(), "expired");
    e
}

#[test]
fn a_track_handle_expires_with_its_track_and_undo_does_not_revive_it() {
    let (mut d, mut h) = project();
    let lua = host();
    let w = world(&d, &mut h);
    let t = w.track_at(1).unwrap();
    let id = d.track(1).unwrap().id();
    assert_eq!(
        eval(&lua, &w, t, "x:valid()").unwrap(),
        Plain::Boolean(true)
    );
    assert_eq!(
        eval(&lua, &w, t, "x:kind()").unwrap(),
        Plain::String("track".into())
    );
    assert_eq!(
        eval(&lua, &w, t, "x:check() == x").unwrap(),
        Plain::Boolean(true)
    );
    // Deleting the track expires the handle.
    h.apply(&mut d, Operation::DeleteTrack { index: 1 })
        .unwrap();
    let w2 = world(&d, &mut h);
    assert_eq!(
        eval(&lua, &w2, t, "x:valid()").unwrap(),
        Plain::Boolean(false)
    );
    let e = expired(eval(&lua, &w2, t, "x:check()"));
    assert!(e.message.contains("track handle is expired"), "{e}");
    assert_eq!(e.line, Some(1), "the error names the script line: {e}");
    // Undo brings the track (same id) back, but not the old handle: it may
    // never be confused with what now has the id.
    h.undo(&mut d).unwrap().unwrap();
    assert_eq!(d.track(1).unwrap().id(), id);
    let w3 = world(&d, &mut h);
    assert!(!t.is_valid(&w3));
    let fresh = w3.track(id).unwrap();
    assert!(fresh.is_valid(&w3));
    assert_ne!(fresh.key(), t.key());
    assert_eq!(
        eval(&lua, &w3, fresh, "x:valid()").unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn a_subtrack_handle_reports_its_kind() {
    let (mut d, mut h) = project();
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 0,
            count: 1,
        },
    )
    .unwrap();
    let w = world(&d, &mut h);
    let lua = host();
    let i = (0..d.track_count())
        .find(|&i| d.track(i).unwrap().parent().is_some())
        .expect("a subtrack");
    let s = w.track_at(i).unwrap();
    assert_eq!(
        eval(&lua, &w, s, "x:kind()").unwrap(),
        Plain::String("subtrack".into())
    );
    assert_eq!(s.family(), "track");
}

#[test]
fn a_clip_id_handed_out_again_never_resolves_to_the_new_clip() {
    let (mut d, mut h) = project();
    let lua = host();
    let op = add_clip(&d, 0, 9600, 960).unwrap();
    h.apply(&mut d, op).unwrap();
    let w = world(&d, &mut h);
    let placed = |d: &PlumDocument| {
        d.clips()
            .explicit_placements()
            .iter()
            .find_map(|p| match (p.id, p.content) {
                (ClipId::Placed(_), ContentRef::Owned(c)) => Some((p.id, c)),
                _ => None,
            })
            .unwrap()
    };
    let (clip_id, content_id) = placed(&d);
    let clip = w.clip(clip_id).unwrap();
    let content = w.content(content_id).unwrap();
    assert!(clip.is_valid(&w) && content.is_valid(&w));
    assert_eq!(
        eval(&lua, &w, content, "x:kind()").unwrap(),
        Plain::String("clip_content".into())
    );
    // Undo the creation, then create another clip: it gets the same ids.
    h.undo(&mut d).unwrap().unwrap();
    let op = add_clip(&d, 1, 0, 480).unwrap();
    h.apply(&mut d, op).unwrap();
    let (again, again_content) = placed(&d);
    assert_eq!(
        (again, again_content),
        (clip_id, content_id),
        "the engine hands the ids out again"
    );
    let w2 = world(&d, &mut h);
    assert!(!clip.is_valid(&w2), "the old clip handle is expired");
    assert!(!content.is_valid(&w2), "the old content handle is expired");
    expired(eval(&lua, &w2, clip, "x:check()"));
    let new_clip = w2.clip(again).unwrap();
    assert!(new_clip.is_valid(&w2));
    assert_ne!(new_clip.key(), clip.key());
    assert_eq!(
        eval(&lua, &w2, new_clip, "x:valid()").unwrap(),
        Plain::Boolean(true)
    );
    // A primary clip is its track's own content.
    let t0 = d.track(0).unwrap().id();
    let primary = w2.clip(ClipId::Primary(t0)).unwrap();
    assert!(primary.is_valid(&w2));
}

#[test]
fn a_subchannel_id_taken_again_does_not_revive_the_old_handle() {
    let (mut d, mut h) = project();
    let free = |d: &PlumDocument| d.track(0).unwrap().subchannels().free_ids(1).unwrap()[0];
    let id = free(&d);
    let add = |id| Operation::AddSubchannels {
        track: 0,
        subs: vec![Subchannel::new(id, 3)],
    };
    h.apply(&mut d, add(id)).unwrap();
    let t0 = d.track(0).unwrap().id();
    let w = world(&d, &mut h);
    let s = w.subchannel(t0, id).unwrap();
    assert!(s.is_valid(&w));
    h.apply(&mut d, Operation::DeleteSubchannel { track: 0, id })
        .unwrap();
    assert_eq!(free(&d), id, "the id is free again");
    h.apply(&mut d, add(id)).unwrap();
    let w2 = world(&d, &mut h);
    assert!(!s.is_valid(&w2));
    let lua = host();
    expired(eval(&lua, &w2, s, "x:check()"));
    assert!(w2.subchannel(t0, id).unwrap().is_valid(&w2));
}

#[test]
fn a_folder_id_handed_out_again_after_undo_is_a_different_folder() {
    let (mut d, mut h) = project();
    let create = |d: &PlumDocument| {
        let mut f = d.project().folders.clone();
        let id = f.create("Drums", None);
        (
            id,
            Operation::SetFolders {
                folders: Box::new(f),
                label: "New Folder",
            },
        )
    };
    let (id, op) = create(&d);
    h.apply(&mut d, op).unwrap();
    let w = world(&d, &mut h);
    let f = w.folder(id).unwrap();
    assert!(f.is_valid(&w));
    h.undo(&mut d).unwrap().unwrap();
    let (again, op) = create(&d);
    assert_eq!(again, id, "Undo restored the folder counter");
    h.apply(&mut d, op).unwrap();
    let w2 = world(&d, &mut h);
    assert!(!f.is_valid(&w2));
    assert!(w2.folder(id).unwrap().is_valid(&w2));
}

#[test]
fn a_note_handle_survives_selection_but_expires_when_the_notes_change() {
    let (mut d, mut h) = project();
    let tick = 1u64 << 62;
    h.apply(
        &mut d,
        Operation::InsertNote {
            track: NoteTarget::Track(0),
            record: note(tick, 64),
        },
    )
    .unwrap();
    let host_ref = ContentRef::Track(d.track(0).unwrap().id());
    let loc = d.track(0).unwrap().notes.iter_with_loc().next().unwrap().0;
    let w = world(&d, &mut h);
    let n = w.note(host_ref, loc).unwrap();
    match n.resolve(&w) {
        Ok(Resolved::Note { note, .. }) => {
            // B1-0023: a tick of 2^62 reaches a script exactly.
            assert_eq!(note.start_tick, tick);
            assert_eq!(ticks::to_lua(note.start_tick).unwrap(), 1i64 << 62);
        }
        _ => panic!("the note resolves"),
    }
    // Selecting the note changes no note value: the handle stays valid.
    d.track_mut(0).unwrap().notes.select_all();
    let w2 = world(&d, &mut h);
    assert!(n.is_valid(&w2));
    assert_eq!(w2.note(host_ref, loc).unwrap().key(), n.key());
    // Any change of the track's notes expires it (notes have no ids).
    h.apply(
        &mut d,
        Operation::InsertNote {
            track: NoteTarget::Track(0),
            record: note(0, 60),
        },
    )
    .unwrap();
    let w3 = world(&d, &mut h);
    assert!(!n.is_valid(&w3));
    let lua = host();
    let e = expired(eval(&lua, &w3, n, "x:check()"));
    assert!(e.message.contains("note handle is expired"), "{e}");
    assert_eq!(
        eval(&lua, &w3, n, "x:kind()").unwrap(),
        Plain::String("note".into())
    );
}

#[test]
fn an_event_handle_expires_when_the_events_change() {
    let (mut d, mut h) = project();
    let cc = RawEvent {
        tick: 480,
        anchor: 0,
        kind: RawKind::Channel {
            status: 0xB0,
            data: [7, 100],
        },
    };
    d.track_mut(0).unwrap().events.push(cc).unwrap();
    let host_ref = ContentRef::Track(d.track(0).unwrap().id());
    let w = world(&d, &mut h);
    let ev = w.event(host_ref, 0).unwrap();
    assert!(ev.is_valid(&w));
    assert!(w.event(host_ref, 5).is_none(), "no event at that index");
    let edit = prepare_rename(d.track(0).unwrap(), "Lead").unwrap();
    h.apply(
        &mut d,
        Operation::Events {
            edit,
            label: "Rename Track",
        },
    )
    .unwrap();
    let w2 = world(&d, &mut h);
    assert!(!ev.is_valid(&w2));
    let lua = host();
    expired(eval(&lua, &w2, ev, "x:check()"));
}

#[test]
fn handles_of_a_closed_project_are_expired_and_say_so() {
    let (d, mut h) = project();
    let w = world(&d, &mut h);
    let t = w.track_at(0).unwrap();
    let p = w.project();
    let c = w.conductor();
    assert!(p.is_valid(&w) && c.is_valid(&w));
    // Another project opens in the same history.
    h.reset(true);
    let w2 = world(&d, &mut h);
    assert_ne!(w2.project_serial(), w.project_serial());
    for x in [t, p, c] {
        assert!(!x.is_valid(&w2));
    }
    let lua = host();
    let e = expired(eval(&lua, &w2, t, "x:check()"));
    assert!(e.message.contains("no longer open"), "{e}");
    // No project at all.
    lua.set_world(None);
    lua.set_global_handle("x", t).unwrap();
    assert_eq!(
        lua.run("h.lua", "return x:valid()").unwrap(),
        Plain::Boolean(false)
    );
    let e = expired(lua.run("h.lua", "return x:check()"));
    assert!(e.message.contains("No project is open"), "{e}");
}

#[test]
fn handles_compare_by_object_and_keys_are_table_keys() {
    let (d, mut h) = project();
    let w = world(&d, &mut h);
    let lua = host();
    lua.set_world(Some(w.clone()));
    lua.set_global_handle("a", w.track_at(1).unwrap()).unwrap();
    lua.set_global_handle("b", w.track_at(1).unwrap()).unwrap();
    lua.set_global_handle("c", w.track_at(0).unwrap()).unwrap();
    assert_eq!(
        lua.run(
            "h.lua",
            "local t = {} t[a:key()] = 1 t[b:key()] = 2 t[c:key()] = 3 \
             local n = 0 for _ in pairs(t) do n = n + 1 end \
             return (a == b) and not (a == c) and rawequal(a, b) == false and n == 2 \
             and tostring(a):find('track', 1, true) ~= nil"
        )
        .unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn the_ledger_costs_nothing_until_an_id_vanishes() {
    let (mut d, mut h) = project();
    for k in 0..50u64 {
        h.apply(
            &mut d,
            Operation::InsertNote {
                track: NoteTarget::Track(0),
                record: note(k * 240, 60),
            },
        )
        .unwrap();
    }
    assert_eq!(h.incarnations().vanished_ids(), 0);
    h.apply(&mut d, Operation::DeleteTrack { index: 1 })
        .unwrap();
    // The track and its subchannel.
    assert_eq!(h.incarnations().vanished_ids(), 2);
}

// ------------------------------------------------------------- ticks

#[test]
fn ticks_past_2_to_the_62_round_trip_exactly() {
    let lua = host();
    let ok = |src: &str| lua.run("t.lua", src).unwrap();
    assert_eq!(
        ok(
            "local t = plum.ticks.check(1 << 62) return math.type(t) == 'integer' and t == 4611686018427387904"
        ),
        Plain::Boolean(true)
    );
    assert_eq!(
        ok("return plum.ticks.add(1 << 62, (1 << 62) - 1)"),
        Plain::Integer(i64::MAX)
    );
    assert_eq!(ok("return plum.ticks.max"), Plain::Integer(i64::MAX));
    assert_eq!(
        ok("return plum.ticks.parse('9223372036854775807')"),
        Plain::Integer(i64::MAX)
    );
    assert_eq!(
        ok("return plum.ticks.check('4_611_686_018_427_387_904')"),
        Plain::Integer(1 << 62)
    );
    assert_eq!(ok("return plum.ticks.check(2^53)"), Plain::Integer(1 << 53));
    assert_eq!(ok("return plum.ticks.check(960.0)"), Plain::Integer(960));
    assert_eq!(
        ok("return plum.ticks.muldiv(1 << 62, 3, 4)"),
        Plain::Integer(3 << 60)
    );
    assert_eq!(
        ok("return plum.ticks.muldiv(9223372036854775807, 960, 960)"),
        Plain::Integer(i64::MAX)
    );
    assert_eq!(ok("return plum.ticks.muldiv(7, 1, 2)"), Plain::Integer(3));
    assert_eq!(
        ok("return plum.ticks.muldiv(7, 1, 2, 'round')"),
        Plain::Integer(4)
    );
    assert_eq!(
        ok("return plum.ticks.muldiv(7, 1, 2, 'ceil')"),
        Plain::Integer(4)
    );
    assert_eq!(ok("return plum.ticks.sub(10, 10)"), Plain::Integer(0));
    assert_eq!(
        ok("return plum.ticks.mul(1 << 31, 1 << 31)"),
        Plain::Integer(1 << 62)
    );
}

#[test]
fn tick_values_that_would_lose_precision_are_refused() {
    let lua = host();
    let refused = |src: &str, words: &str| {
        let e = lua.run("t.lua", src).expect_err(src);
        assert_eq!(e.category, Category::Runtime, "{src}: {e}");
        assert!(e.message.contains(words), "{src}: {e}");
        assert_eq!(e.line, Some(1), "{src}: {e}");
    };
    // Lua's own integers wrap on overflow; plum.ticks does not.
    refused("return plum.ticks.add(1 << 62, 1 << 62)", "2^63 - 1");
    refused("return plum.ticks.mul(1 << 32, 1 << 31)", "2^63 - 1");
    refused("return plum.ticks.sub(1, 2)", "before tick 0");
    refused("return plum.ticks.muldiv(1 << 62, 2, 1)", "2^63 - 1");
    refused("return plum.ticks.muldiv(1, 1, 0)", "must not be 0");
    refused("return plum.ticks.muldiv(1, 1, 3, 'up')", "is not");
    refused("return plum.ticks.check(2^60)", "above 2^53");
    refused("return plum.ticks.check(2.5)", "whole number");
    refused("return plum.ticks.check(-1)", "negative");
    refused("return plum.ticks.check(0/0)", "whole number");
    refused(
        "return plum.ticks.check('9223372036854775808')",
        "past 2^63 - 1",
    );
    refused("return plum.ticks.check('12a')", "not a decimal");
    refused("return plum.ticks.check({})", "not table");
    // From the document side: a u64 tick past i64::MAX is an error, never a
    // wrapped number.
    assert!(ticks::to_lua(u64::MAX).is_err());
    assert!(ticks::to_lua(i64::MAX as u64 + 1).is_err());
    assert_eq!(ticks::to_lua(i64::MAX as u64).unwrap(), i64::MAX);
}
