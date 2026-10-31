//! B1-0028: the Clips API on the real clip model — reads, local <-> song
//! time, every Track List edit through transactions (one Undo step), linked
//! vs independent content, expired handles, Preview = Apply, capabilities.

use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::sync::Arc;

/// Two tracks; track 1 has 48 notes, one every 120 ticks (content 0 – 5,760).
fn project() -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let recs: Vec<NoteRecord> = (0..48u64)
        .map(|k| NoteRecord {
            start_tick: k * 120,
            duration_ticks: 120,
            attrs: NoteAttrs::new(60 + (k % 12) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

const EDIT: [Capability; 3] = [
    Capability::ReadProject,
    Capability::EditTracks,
    Capability::EditNotes,
];

fn host_for(doc: &Arc<PlumDocument>, h: &mut History, caps: &[Capability]) -> Host {
    h.observe_ids(doc);
    let host = Host::new(Limits::default(), caps.iter().copied()).unwrap();
    host.set_world(Some(World::new(
        Arc::clone(doc),
        h.incarnations().snapshot(),
    )));
    host
}

fn run(host: &Host, src: &str) -> Plain {
    host.run("clips.lua", src).unwrap_or_else(|e| panic!("{e}"))
}

fn layout(d: &PlumDocument) -> plumspirit_engine::model::ClipLayout {
    d.clips().layout().into_unrevised()
}

#[test]
fn clips_are_found_by_track_and_time_with_exact_local_and_song_ticks() {
    let (doc, mut h) = project();
    let host = host_for(&doc, &mut h, &EDIT);
    let r = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:track(1)
           local cs = t:clips()
           assert(#cs == 1, #cs)
           local c = cs[1]
           assert(c:kind() == "clip" and c:is_primary() and c:content_kind() == "track")
           assert(c:start_tick() == 0 and c:offset() == 0 and c:length() == nil)
           assert(c:song_length() == 5760 and c:end_tick() == 5760, c:end_tick())
           local from, to = c:window()
           assert(from == 0 and to == nil)
           assert(c:track() == t and c:content() == t and c:linked_count() == 1)
           assert(not c:mute() and c:name() == "")
           assert(c:to_song(240) == 240 and c:to_local(600) == 600 and c:contains(5759) and not c:contains(5760))
           assert(#t:clips_at(5000) == 1 and #t:clips_at(6000) == 0)
           assert(#p:clips() == 2, "the empty track has its primary clip too")
           assert(#p:clips{ track = p:track(2) } == 1)
           -- By time: the empty track's primary clip covers no tick.
           assert(#p:clips{ start_tick = 6000 } == 0 and #p:clips{ end_tick = 1 } == 1)
           assert(c:notes():count() == 48 and c:notes{ end_tick = 600 }:count() == 5)
           -- Huge ticks stay exact (no float rounding).
           local ok, err = pcall(function() return c:to_local(-1) end)
           assert(not ok)
           return c:to_song(plum.max_integer - 1)"#,
    );
    assert_eq!(r, Plain::Integer(i64::MAX - 1));
}

#[test]
fn every_clip_edit_goes_through_the_planners_as_one_undo_step() {
    let (mut doc, mut h) = project();
    let before = layout(&doc);
    let fp = doc.fingerprint();
    let host = host_for(&doc, &mut h, &EDIT);
    let r = run(
        &host,
        r#"local p = plum.project.current()
           local t1, t2 = p:track(1), p:track(2)
           local tx = plum.transaction{ label = "Arrange Clips" }
           local prim = t1:clips()[1]
           -- New empty clip on track 2.
           local empty = tx:add_clip(t2, 9600, 1920)
           assert(empty:track() == t2 and empty:start_tick() == 9600 and empty:length() == 1920)
           assert(empty:content_kind() == "owned" and empty:notes():count() == 0)
           -- Linked copy right after the block (like Ctrl+D) and an independent one.
           local linked = tx:duplicate_clips(prim)[1]
           assert(linked:start_tick() == 5760, linked:start_tick())
           assert(linked:content() == t1 and prim:linked_count() == 2)
           assert(prim:linked_clips()[1] == linked)
           local copy = tx:duplicate_clips(prim, { linked = false, start = 20000, track = t2 })[1]
           assert(copy:content_kind() == "owned" and copy:track() == t2 and copy:start_tick() == 20000)
           assert(copy:notes():count() == 48)
           -- Resize: trimming the left edge keeps the notes where they play.
           tx:resize_clip(linked, { start = 5760 + 600 })
           assert(linked:offset() == 600 and linked:start_tick() == 6360, linked:offset())
           assert(linked:end_tick() == 5760 + 5760, "the right edge stayed")
           assert(linked:to_song(600) == 6360 and linked:to_local(6360) == 600)
           assert(linked:notes():count() == 48 - 5)
           -- Slice the copy: the right part is a new clip showing the same content.
           local right = tx:slice_clips(copy, 22000)[1]
           assert(copy:end_tick() == 22000 and right:start_tick() == 22000)
           assert(right:content() == copy:content() and right:offset() == 2000)
           assert(copy:notes():count() + right:notes():count() == 48)
           -- Move both parts together by a shift.
           tx:move_clips({ copy, right }, { shift = 1000 })
           assert(copy:start_tick() == 21000 and right:start_tick() == 23000)
           -- Mute, rename, color.
           tx:set_muted(right, true)
           tx:rename_clip(copy, "Left half")
           tx:rename(right, "Right half")
           tx:recolor_clips({ copy, right }, { r = 10, g = 200, b = 30 })
           assert(right:mute() and copy:name() == "Left half" and right:name() == "Right half")
           local c = copy:color()
           assert(c.r == 10 and c.g == 200 and c.b == 30 and c.custom)
           -- Merge in sequence (the muted part is not merged: unmute first).
           tx:set_muted(right, false)
           local merged = tx:merge_clips({ copy, right }, { mode = "sequence" })
           assert(merged ~= nil and merged:notes():count() == 48)
           assert(not copy:valid() and not right:valid(), "merged clips are gone")
           -- Delete the empty clip.
           tx:delete_clips(empty)
           assert(not empty:valid())
           tx:commit()
           return #p:clips()"#,
    );
    // Track 1: primary + linked; track 2: primary + merged.
    assert_eq!(r, Plain::Integer(4));
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.steps.len(), 1);
    assert_eq!(commits.steps[0].label, "Arrange Clips");
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(h.undo_len(), 1, "one Undo step");
    let after = layout(&doc);
    assert_ne!(after, before);
    assert_eq!(doc.placed_note_count(0), 48 + 43);
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap().unwrap();
    assert_eq!(d.fingerprint(), fp);
    let undone = layout(d);
    assert_eq!(undone.placements, before.placements);
    assert_eq!(undone.contents, before.contents);
    h.redo(d).unwrap().unwrap();
    assert_eq!(layout(d).placements, after.placements);
}

#[test]
fn a_failed_clip_edit_changes_nothing_and_old_handles_expire() {
    let (mut doc, mut h) = project();
    let fp = doc.fingerprint();
    let host = host_for(&doc, &mut h, &EDIT);
    // Refusals are sentences; the run fails as a whole.
    for (src, words) in [
        (
            "tx:add_clip(p:track(2), 0, 0)",
            "length of at least one tick",
        ),
        (
            "tx:merge_clips(p:track(1):clips(), { mode = \"diagonal\" })",
            "\"sequence\" or \"stacked\"",
        ),
        (
            "tx:move_clips(p:track(1):clips(), { shift = -1 })",
            "before the song's start",
        ),
        (
            "tx:resize_clip(p:track(1):clips()[1], { end_tick = 0 })",
            "after the clip's start",
        ),
    ] {
        let e = host
            .run(
                "bad.lua",
                &format!(
                    "local p = plum.project.current() local tx = plum.transaction{{}} {src} tx:commit()"
                ),
            )
            .unwrap_err();
        assert_eq!(e.category, Category::Runtime, "{src}: {e}");
        assert!(e.message.contains(words), "{src}: {}", e.message);
        assert!(host.take_commits().is_none());
        assert_eq!(doc.fingerprint(), fp);
    }
    // A clip deleted and committed: its handle (kept as a key) is expired.
    run(
        &host,
        r#"local p = plum.project.current()
           local tx = plum.transaction{}
           kept = tx:duplicate_clips(p:track(1):clips()[1])[1]
           tx:commit()"#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    let host2 = host_for(&doc, &mut h, &EDIT);
    run(
        &host2,
        r#"local p = plum.project.current()
           local c = p:track(1):clips()[2]
           local tx = plum.transaction{}
           tx:delete_clips(c)
           tx:commit()"#,
    );
    host2
        .take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    h.observe_ids(&doc);
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        h.incarnations().snapshot(),
    )));
    let e = host.run("old.lua", "return kept:start_tick()").unwrap_err();
    assert_eq!(e.category, Category::Expired, "{e}");
    assert_eq!(
        host.run("old.lua", "return kept:valid()").unwrap(),
        Plain::Boolean(false)
    );
}

#[test]
fn clip_writes_need_tracks_edit_and_reads_need_project_read() {
    let (doc, mut h) = project();
    let reader = host_for(&doc, &mut h, &[Capability::ReadProject]);
    let e = reader
        .run(
            "w.lua",
            "local p = plum.project.current() local tx = plum.transaction{} tx:add_clip(p:track(1), 0, 10)",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
    assert!(e.message.contains("tracks.edit"), "{}", e.message);
    let none = host_for(&doc, &mut h, &[]);
    let e = none
        .run("r.lua", "return plum.project.current():clips()")
        .unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
}

#[test]
fn preview_equals_apply_for_a_clip_transaction() {
    // What the script reads inside the open transaction (the preview) is
    // exactly what the commit produces.
    let (mut doc, mut h) = project();
    let host = host_for(&doc, &mut h, &EDIT);
    let preview = run(
        &host,
        r#"local p = plum.project.current()
           local tx = plum.transaction{}
           local l = tx:duplicate_clips(p:track(1):clips()[1], { start = 7000 })[1]
           tx:slice_clips(l, 9000)
           tx:resize_clip(p:track(1):clips()[1], { length = 3000 })
           local h = p:content_hash()
           tx:commit()
           return h"#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    let host = host_for(&doc, &mut h, &EDIT);
    assert_eq!(
        run(&host, "return plum.project.current():content_hash()"),
        preview
    );
    assert!(
        doc.clips()
            .placement(ClipId::Primary(doc.track(0).unwrap().id()))
            .unwrap()
            .length
            == Some(3000)
    );
}
