//! B1-0026 / B1-0027: script transactions — Tracks / Subtracks / Folders
//! writes, one Undo step per commit, nothing changed by a failed or open
//! transaction, confirmation for multi-million-note deletions.

use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::sync::Arc;

fn note(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 240,
        attrs: NoteAttrs::new(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

/// Two tracks; track 1 has `notes` notes.
fn project(notes: u64) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let recs: Vec<NoteRecord> = (0..notes).map(|k| note(k * 10, 60)).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

fn host_for(doc: &Arc<PlumDocument>, h: &mut History, caps: &[Capability]) -> Host {
    h.observe_ids(doc);
    let host = Host::new(Limits::default(), caps.iter().copied()).unwrap();
    host.set_world(Some(World::new(
        Arc::clone(doc),
        h.incarnations().snapshot(),
    )));
    host
}

const EDIT: [Capability; 2] = [Capability::ReadProject, Capability::EditTracks];

fn names(d: &PlumDocument) -> Vec<String> {
    (0..d.track_count()).map(|i| d.track_label(i)).collect()
}

#[test]
fn a_transaction_edits_tracks_and_folders_as_one_undo_step() {
    let (mut doc, mut h) = project(8);
    let before = doc.fingerprint();
    let shape = |d: &PlumDocument| {
        (
            d.tracks()
                .iter()
                .map(|t| (t.id(), t.parent()))
                .collect::<Vec<_>>(),
            d.project().folders.clone(),
            names(d),
        )
    };
    let before_shape = shape(&doc);
    let host = host_for(&doc, &mut h, &EDIT);
    let key = host
        .run(
            "arrange.lua",
            r#"
            local p = plum.project.current()
            local piano, other = p:track(1), p:track(2)
            local tx = plum.transaction{ label = "Arrange" }
            assert(tx:is_open())
            local bass = tx:add_track{ after = piano, name = "Bass" }
            -- Reads see the transaction's own changes.
            assert(bass:name() == "Bass" and bass:index() == 2, bass:index())
            assert(p:track_count() == 3)
            local subs = tx:add_subtracks(bass, 2)
            assert(#subs == 2 and subs[1]:parent() == bass and subs[2]:kind() == "subtrack")
            tx:rename(piano, "Grand")
            assert(piano:name() == "Grand")
            local f = tx:create_folder("Low end", { bass })
            assert(f:name() == "Low end" and bass:folder() == f)
            tx:rename(f, "Bottom")
            assert(f:name() == "Bottom")
            tx:move_tracks(other, piano, "before")
            assert(other:index() == 1 and piano:index() == 2)
            tx:delete_tracks(subs[2])
            assert(not subs[2]:valid() and subs[1]:valid())
            local e = tx:estimate()
            assert(e.operations == 8 and e.removed_notes == 0 and not e.needs_confirmation, e.operations)
            -- The document itself is untouched until the host applies the commit.
            tx:commit()
            assert(not tx:is_open())
            return bass:key()
        "#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        names(&doc),
        vec!["Track 1", "Track 2"],
        "nothing applied yet"
    );
    let mut commits = host.take_commits().expect("one commit");
    assert_eq!(commits.steps.len(), 1);
    assert_eq!(commits.steps[0].label, "Arrange");
    assert!(!commits.needs_confirmation());
    let second = doc.track(1).unwrap().id();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    // Labels number unnamed tracks by position: the moved second track is
    // now "Track 1".
    assert_eq!(
        names(&doc),
        vec!["Track 1", "Grand", "Bass", "Subtrack 1"],
        "the commit did what the script saw"
    );
    assert_eq!(doc.track(0).unwrap().id(), second);
    assert_eq!(h.undo_len(), 1, "one Undo step");
    assert_eq!(h.undo_label(), Some("Arrange"));
    // A handle taken inside the transaction names the committed track.
    h.observe_ids(&doc);
    let w = World::new(Arc::clone(&doc), h.incarnations().snapshot());
    let bass = w.track_at(2).unwrap();
    assert_eq!(Plain::String(bass.key()), key);
    drop(w);
    // Undo restores the project exactly.
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap().unwrap();
    // (Track ids are never handed out twice, so only the next free id differs.)
    assert_eq!(d.fingerprint(), before);
    assert_eq!(shape(d).0, before_shape.0);
    assert_eq!(shape(d).2, before_shape.2);
    assert!(shape(d).1.folders().is_empty() && before_shape.1.folders().is_empty());
    h.redo(d).unwrap().unwrap();
    assert_eq!(names(d), vec!["Track 1", "Grand", "Bass", "Subtrack 1"]);
}

#[test]
fn a_failed_script_or_an_open_transaction_changes_nothing() {
    let (doc, mut h) = project(4);
    let host = host_for(&doc, &mut h, &EDIT);
    // Fails halfway through.
    let e = host
        .run(
            "half.lua",
            r#"local tx = plum.transaction{}
               tx:add_track{}
               tx:commit()
               error("stopped halfway")"#,
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Runtime);
    assert!(
        host.take_commits().is_none(),
        "a failed run commits nothing"
    );
    // Ends with the transaction open: canceled.
    host.run(
        "open.lua",
        "local tx = plum.transaction{} tx:add_track{} return 1",
    )
    .unwrap();
    assert!(host.take_commits().is_none());
    // Canceled explicitly; the closed transaction refuses more writes.
    let e = host
        .run(
            "cancel.lua",
            "local tx = plum.transaction{} tx:add_track{} tx:cancel() tx:add_track{}",
        )
        .unwrap_err();
    assert!(e.message.contains("closed"), "{e}");
    assert!(host.take_commits().is_none());
    // A refused change is a script error at the line that asked for it.
    let e = host
        .run(
            "last.lua",
            r#"local p = plum.project.current()
               local tx = plum.transaction{}
               tx:delete_tracks({ p:track(1), p:track(2) })"#,
        )
        .unwrap_err();
    assert!(e.message.contains("refused"), "{e}");
    assert_eq!(e.line, Some(3), "{e}");
    assert!(host.take_commits().is_none());
    // One transaction at a time.
    let e = host
        .run(
            "two.lua",
            "local a = plum.transaction{} local b = plum.transaction{}",
        )
        .unwrap_err();
    assert!(e.message.contains("already open"), "{e}");
}

#[test]
fn writes_need_the_tracks_edit_capability() {
    let (doc, mut h) = project(1);
    let host = host_for(&doc, &mut h, &[Capability::ReadProject]);
    let e = host
        .run("w.lua", "local tx = plum.transaction{} tx:add_track{}")
        .unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
    assert!(e.message.contains("tracks.edit"), "{e}");
}

#[test]
fn two_transactions_in_one_run_are_two_undo_steps() {
    let (mut doc, mut h) = project(1);
    let host = host_for(&doc, &mut h, &EDIT);
    host.run(
        "two.lua",
        r#"local a = plum.transaction{ label = "First" }
           a:add_track{ name = "A" }
           a:commit()
           -- The second one starts from the first one's result.
           assert(plum.project.current():track_count() == 3)
           local b = plum.transaction{ label = "Second" }
           b:add_track{ name = "B" }
           b:commit()"#,
    )
    .unwrap();
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.steps.len(), 2);
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 2);
    assert_eq!(names(&doc), vec!["Track 1", "Track 2", "A", "B"]);
    assert_eq!(h.undo_label(), Some("Second"));
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap();
    assert_eq!(names(d), vec!["Track 1", "Track 2", "A"]);
}

#[test]
fn a_commit_is_refused_when_the_project_changed_meanwhile() {
    let (doc, mut h) = project(1);
    let host = host_for(&doc, &mut h, &EDIT);
    host.run(
        "c.lua",
        "local tx = plum.transaction{} tx:add_track{} tx:commit()",
    )
    .unwrap();
    let mut commits = host.take_commits().unwrap();
    let mut other = Arc::new((*doc).clone());
    let e = commits.apply(&mut h, &mut other).unwrap_err();
    assert_eq!(e, super::tx::CommitError::ProjectChanged);
    assert!(e.user_message().contains("changed while the script ran"));
    assert_eq!(other.track_count(), 2);
}

#[test]
fn deleting_a_million_notes_needs_the_users_confirmation() {
    let (mut doc, mut h) = project(1_000_000);
    let host = host_for(&doc, &mut h, &EDIT);
    let r = host
        .run(
            "big.lua",
            r#"local tx = plum.transaction{ label = "Clear" }
               tx:delete_tracks(plum.project.current():track(1))
               local e = tx:estimate()
               tx:commit()
               return e.needs_confirmation and e.removed_notes == 1000000"#,
        )
        .unwrap();
    assert_eq!(r, Plain::Boolean(true));
    let mut commits = host.take_commits().unwrap();
    assert!(commits.needs_confirmation());
    assert_eq!(commits.removed_notes(), 1_000_000);
    // Confirmed: applied as one step.
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(doc.note_count(), 0);
}

#[test]
fn conductor_writes_are_exact_and_need_conductor_edit() {
    let (mut doc, mut h) = project(1);
    let caps = [
        Capability::ReadProject,
        Capability::EditTracks,
        Capability::EditConductor,
    ];
    let host = host_for(&doc, &mut h, &caps);
    host.run(
        "tempo.lua",
        r#"local p = plum.project.current()
           local tx = plum.transaction{ label = "Tempo map" }
           tx:set_tempo(1920, "143.5")
           tx:set_tempo(3840, 90, { ramp = true })
           tx:set_tempo(0, 128.25)
           tx:set_time_signature(1920, 7, 8)
           tx:add_marker(0, "Intro")
           tx:add_marker(1920, "Verse", 5760)
           local t = p:tempos()
           assert(#t == 3 and t[1].bpm == "128.25" and t[2].bpm == "143.5" and t[3].bpm == "90", t[1].bpm)
           assert(t[2].micros == 418118, t[2].micros)
           assert(t[3].ramp == true and t[2].ramp == false)
           local s = p:time_signatures()
           assert(#s == 2 and s[2].numerator == 7 and s[2].denominator == 8)
           local m = p:markers()
           assert(#m == 2 and m[2].chapter and m[2].end_tick == 5760)
           tx:remove_tempo(3840)
           tx:remove_time_signature(1920)
           tx:remove_markers(0)
           assert(#p:tempos() == 2 and #p:time_signatures() == 1 and #p:markers() == 1)
           -- Refused, with the engine's sentence.
           local ok, err = pcall(function() tx:remove_tempo(999) end)
           assert(not ok and tostring(err):find("no tempo change at tick 999", 1, true), tostring(err))
           ok, err = pcall(function() tx:set_time_signature(0, 3, 6) end)
           assert(not ok and tostring(err):find("beat unit", 1, true))
           tx:commit()"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    let c = doc.conductor();
    assert_eq!(c.tempo_points().len(), 2);
    assert_eq!(c.markers().len(), 1);
    // Without conductor.edit.
    let host = host_for(&doc, &mut h, &EDIT);
    let e = host
        .run("t.lua", "plum.transaction{}:set_tempo(0, 100)")
        .unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
    assert!(e.message.contains("conductor.edit"), "{e}");
}

#[test]
fn subchannels_are_added_edited_and_deleted_with_their_notes() {
    let (mut doc, mut h) = project(10);
    let host = host_for(&doc, &mut h, &EDIT);
    let r = host
        .run(
            "subs.lua",
            r#"local t = plum.project.current():track(1)
               local tx = plum.transaction{ label = "Strings" }
               local s = tx:add_subchannel(t, { channel = 3, name = "Strings" })
               assert(s:number() == 2 and s:channel() == 3 and s:name() == "Strings")
               tx:edit_subchannel(s, { channel = 9, name = "Drums" })
               assert(s:channel() == 9 and s:name() == "Drums")
               local ok, err = pcall(function() tx:edit_subchannel(s, { channel = 16 }) end)
               assert(not ok and tostring(err):find("0 - 15", 1, true))
               local main = t:subchannels()[1]
               tx:delete_subchannel(s)
               assert(not s:valid() and #t:subchannels() == 1)
               local e = tx:estimate()
               tx:commit()
               return e.removed_notes"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r, Plain::Integer(0), "the new subchannel had no notes");
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(doc.track(0).unwrap().subchannels().len(), 1);
    // Deleting the main subchannel's notes is counted.
    let host = host_for(&doc, &mut h, &EDIT);
    let r = host
        .run(
            "del.lua",
            r#"local t = plum.project.current():track(1)
               local tx = plum.transaction{}
               local s2 = tx:add_subchannel(t)
               tx:delete_subchannel(t:subchannels()[1])
               return tx:estimate().removed_notes"#,
        )
        .unwrap();
    assert_eq!(r, Plain::Integer(10));
}
