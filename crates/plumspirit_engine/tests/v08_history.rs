//! v0.8 persistent undo / redo (contract §084–§093, tests §237–§238;
//! TEST_MATRIX T-HIST-*): the journal survives "exit" and "restart", undo and
//! redo after reopening reach exact document states, unsaved steps can be
//! redone, the RAM window stays bounded, compaction keeps the live path, and a
//! damaged / foreign / stale / newer journal never breaks the project.

use plumspirit_engine::editor::folders::plan_create;
use plumspirit_engine::editor::{self, EditControl, clipboard::Clipboard};
use plumspirit_engine::history::journal::{Journal, JournalProblem, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::{
    Bpm, Interp, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, TempoValue, Track, TrackId,
};
use plumspirit_engine::native::{LupControl, load_project, save_project};
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plumspirit-hist-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn note(t: u64, p: u8) -> NoteRecord {
    NoteRecord::new(
        t,
        240,
        NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
    )
}

fn start(d: &Path) -> (PlumDocument, History, PathBuf, PathBuf) {
    let mut doc = PlumDocument::new_project("hist".into(), Bpm::DEFAULT, 960);
    let recs: Vec<NoteRecord> = (0..200u64)
        .map(|i| note(i * 120, 40 + (i % 40) as u8))
        .collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let file = d.join("song.lup");
    save_project(&doc, &file, LupControl::default()).unwrap();
    let jpath = d.join("song.plh");
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(identity(&doc))).unwrap();
    (doc, h, file, jpath)
}

fn save(doc: &PlumDocument, h: &mut History, file: &Path) {
    save_project(doc, file, LupControl::default()).unwrap();
    let s = h.current_state();
    h.mark_saved(s);
    h.persist_saved(s, identity(doc));
}

/// Reopens the file and its journal like the app does after a restart.
fn reopen(
    file: &Path,
    jpath: &Path,
) -> (
    PlumDocument,
    History,
    plumspirit_engine::history::journal::RestoreReport,
) {
    let (doc, _) = load_project(file, LupControl::default()).unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let (j, scan) = Journal::open(jpath, path_key(file)).unwrap().unwrap();
    let report = h.restore_journal(j, scan, identity(&doc)).unwrap();
    (doc, h, report)
}

/// Draw, Delete, Transpose, Track Move, a large paste, track add / delete,
/// tempo, name, folders, velocity — returning each state's identity.
fn edits(doc: &mut PlumDocument, h: &mut History) -> Vec<u64> {
    let mut ids = vec![identity(doc)];
    let push = |doc: &PlumDocument, ids: &mut Vec<u64>| ids.push(identity(doc));
    // Draw.
    h.apply(
        doc,
        Operation::InsertNote {
            track: 0.into(),
            record: note(50_000, 72),
        },
    )
    .unwrap();
    push(doc, &mut ids);
    // Delete.
    doc.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(0, 2_000, 0, 127);
    h.apply(doc, Operation::DeleteSelected { track: 0.into() })
        .unwrap();
    push(doc, &mut ids);
    // Transpose (+3 semitones) and move in time.
    doc.track_mut(0)
        .unwrap()
        .notes
        .select_in_region(5_000, 12_000, 0, 127);
    let p = editor::prepare_move(doc, 0, 480, 3, EditControl::default()).unwrap();
    h.apply(
        doc,
        Operation::Prepared {
            edit: p,
            label: "Transpose",
        },
    )
    .unwrap();
    push(doc, &mut ids);
    // Velocity.
    h.apply(
        doc,
        Operation::SetVelocity {
            track: 0.into(),
            velocity: 64,
        },
    )
    .unwrap();
    push(doc, &mut ids);
    // Add tracks, a subtrack, move a track (subtree), delete one.
    h.apply(doc, Operation::AddTrack).unwrap();
    push(doc, &mut ids);
    h.apply(
        doc,
        Operation::AddSubtracks {
            parent: 0,
            count: 2,
        },
    )
    .unwrap();
    push(doc, &mut ids);
    let last = doc.track_count() - 1;
    h.apply(
        doc,
        Operation::MoveTracks {
            from: last,
            to: 0,
            parent: None,
        },
    )
    .unwrap();
    push(doc, &mut ids);
    // A large paste (one step, compact records): 100,000 notes.
    {
        let i = (0..doc.track_count())
            .find(|&i| doc.track(i).unwrap().notes.len() > 100)
            .unwrap();
        let big: Vec<NoteRecord> = (0..100_000u64)
            .map(|k| note(k * 7, 30 + (k % 60) as u8))
            .collect();
        let mut scratch = Track::new(TrackId(9_999));
        scratch.notes.insert_sorted_batch(&big, false).unwrap();
        scratch.notes.select_in_region(0, u64::MAX, 0, 127);
        let clip = Clipboard::copy_selected(&scratch).unwrap().unwrap();
        let p = clip.prepare_paste(doc, i, 100_000).unwrap();
        h.apply(
            doc,
            Operation::Prepared {
                edit: p,
                label: "Paste",
            },
        )
        .unwrap();
        push(doc, &mut ids);
    }
    // Delete the track that has the subtracks (its whole subtree is stashed).
    let parent = (0..doc.track_count())
        .find(|&i| doc.subtree_end(i) - i == 3)
        .unwrap();
    h.apply(doc, Operation::DeleteTrack { index: parent })
        .unwrap();
    push(doc, &mut ids);
    // Tempo (Conductor).
    let e = doc
        .conductor()
        .prepare_set_tempo(
            None,
            1920,
            TempoValue::Bpm(Bpm::parse("140").unwrap()),
            Interp::Linear,
        )
        .unwrap();
    h.apply(
        doc,
        Operation::Conductor {
            edit: e,
            label: "Tempo",
        },
    )
    .unwrap();
    push(doc, &mut ids);
    h.apply(doc, Operation::SetName("Renamed".into())).unwrap();
    push(doc, &mut ids);
    let (op, _) = plan_create(doc, &[0], "Folder", None).unwrap();
    h.apply(doc, op).unwrap();
    push(doc, &mut ids);
    ids
}

#[test]
fn history_survives_restart_and_reaches_exact_states() {
    let d = dir("restart");
    let (mut doc, mut h, file, jpath) = start(&d);
    let ids = edits(&mut doc, &mut h);
    let n = ids.len() - 1;
    assert_eq!(h.undo_len(), n);
    save(&doc, &mut h, &file);
    drop(h); // exit
    // Restart: reopen the file, restore the history.
    let (mut doc, mut h, report) = reopen(&file, &jpath);
    assert_eq!(identity(&doc), ids[n]);
    assert_eq!(
        (report.undo_steps, report.redo_steps, report.unsaved_steps),
        (n, 0, 0)
    );
    assert!(h.is_at_saved_state());
    // Undo through every prior-session step, exact at each state.
    for k in (0..n).rev() {
        h.undo(&mut doc).unwrap().unwrap();
        assert_eq!(identity(&doc), ids[k], "after undoing to state {k}");
    }
    assert!(!h.can_undo());
    assert!(!h.is_at_saved_state());
    // Redo all the way back.
    for (k, id) in ids.iter().enumerate().skip(1) {
        h.redo(&mut doc).unwrap().unwrap();
        assert_eq!(identity(&doc), *id, "after redoing to state {k}");
    }
    assert!(h.is_at_saved_state(), "the savepoint survives the restart");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unsaved_steps_of_a_session_can_be_redone_after_reopening() {
    let d = dir("unsaved");
    let (mut doc, mut h, file, jpath) = start(&d);
    h.apply(
        &mut doc,
        Operation::InsertNote {
            track: 0.into(),
            record: note(9, 99),
        },
    )
    .unwrap();
    save(&doc, &mut h, &file);
    let saved_id = identity(&doc);
    h.apply(
        &mut doc,
        Operation::InsertNote {
            track: 0.into(),
            record: note(19, 98),
        },
    )
    .unwrap();
    h.apply(&mut doc, Operation::SetName("Later".into()))
        .unwrap();
    let last_id = identity(&doc);
    drop(h); // exit without saving (or a crash)
    let (mut doc, mut h, report) = reopen(&file, &jpath);
    assert_eq!(identity(&doc), saved_id, "the file holds the saved state");
    assert_eq!(
        (report.undo_steps, report.redo_steps, report.unsaved_steps),
        (1, 2, 2)
    );
    h.redo(&mut doc).unwrap();
    h.redo(&mut doc).unwrap();
    assert_eq!(identity(&doc), last_id, "the last session's work is back");
    // Branching: undo one, a new edit discards the old redo path for good.
    h.undo(&mut doc).unwrap();
    h.apply(&mut doc, Operation::SetName("Other".into()))
        .unwrap();
    assert!(!h.can_redo());
    save(&doc, &mut h, &file);
    drop(h);
    let (mut doc, mut h, report) = reopen(&file, &jpath);
    assert_eq!(
        report.redo_steps, 0,
        "the discarded branch is not offered again"
    );
    assert_eq!(report.undo_steps, 3);
    h.undo(&mut doc).unwrap();
    assert_eq!(doc.project().name, "hist");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn only_a_bounded_window_stays_in_memory_and_compaction_keeps_the_live_path() {
    let d = dir("window");
    let (mut doc, _, file, jpath) = start(&d);
    // A tiny RAM budget: most steps live on disk only.
    let mut h = History::new(HistoryLimits {
        max_bytes: 64 * 1024,
        max_entries: 5,
    });
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(identity(&doc))).unwrap();
    let mut ids = vec![identity(&doc)];
    for i in 0..60u64 {
        h.apply(
            &mut doc,
            Operation::InsertNote {
                track: 0.into(),
                record: note(1_000_000 + i, 60),
            },
        )
        .unwrap();
        ids.push(identity(&doc));
    }
    assert_eq!(h.undo_len(), 60, "every step is still undoable");
    assert!(h.resident_entries() <= 5, "{}", h.resident_entries());
    // Undo 10 (read back from disk), then a new branch, then compact.
    for k in (50..60).rev() {
        h.undo(&mut doc).unwrap();
        assert_eq!(identity(&doc), ids[k]);
    }
    h.apply(&mut doc, Operation::SetName("branch".into()))
        .unwrap();
    let before = h.journal().unwrap().disk_bytes();
    let (b, a) = h.compact_journal(None).unwrap();
    assert_eq!(b, before);
    assert!(a < b, "compaction dropped the discarded branch: {a} < {b}");
    // Everything still undoes exactly after compaction.
    h.undo(&mut doc).unwrap();
    assert_eq!(identity(&doc), ids[50]);
    for k in (0..50).rev() {
        h.undo(&mut doc).unwrap();
        assert_eq!(identity(&doc), ids[k]);
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn damaged_foreign_stale_and_newer_journals_never_break_the_project() {
    let d = dir("damage");
    let (mut doc, mut h, file, jpath) = start(&d);
    for i in 0..5u64 {
        h.apply(
            &mut doc,
            Operation::InsertNote {
                track: 0.into(),
                record: note(77 + i, 70),
            },
        )
        .unwrap();
    }
    save(&doc, &mut h, &file);
    h.apply(&mut doc, Operation::SetName("tail".into()))
        .unwrap();
    drop(h);
    let good = std::fs::read(&jpath).unwrap();
    // 1. Truncated (a crash while writing the last record): repaired.
    std::fs::write(&jpath, &good[..good.len() - 3]).unwrap();
    let (mut doc2, mut h2, report) = reopen(&file, &jpath);
    assert!(report.repaired_bytes > 0);
    assert_eq!(report.undo_steps, 5);
    assert_eq!(report.redo_steps, 0, "the cut record is gone");
    h2.undo(&mut doc2).unwrap();
    // 2. A flipped byte inside a step: the scan stops before it (small
    //    records are checked) and the project opens with the steps before.
    let mut bad = good.clone();
    let mid = good.len() / 2;
    bad[mid] ^= 0x55;
    std::fs::write(&jpath, &bad).unwrap();
    let (doc3, _) = load_project(&file, LupControl::default()).unwrap();
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    let mut h3 = History::default();
    h3.reset(true);
    match h3.restore_journal(j, scan, identity(&doc3)) {
        Ok(r) => assert!(r.repaired_bytes > 0 || r.undo_steps <= 5),
        Err((_, p)) => assert!(
            matches!(p, JournalProblem::Stale | JournalProblem::Corrupt(_)),
            "{p:?}"
        ),
    }
    // 3. Unknown (newer) history version: not used, not changed.
    let mut newer = good.clone();
    newer[8] = 99;
    std::fs::write(&jpath, &newer).unwrap();
    match Journal::open(&jpath, path_key(&file)).unwrap() {
        Err(JournalProblem::UnsupportedVersion(99)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&jpath).unwrap(), newer, "left untouched");
    // 4. Another project's journal.
    std::fs::write(&jpath, &good).unwrap();
    match Journal::open(&jpath, path_key(&d.join("other.lup"))).unwrap() {
        Err(JournalProblem::WrongProject) => {}
        other => panic!("{other:?}"),
    }
    // 5. Stale: the project file was changed elsewhere.
    let (mut other, _) = load_project(&file, LupControl::default()).unwrap();
    other.project_mut().name = "changed elsewhere".into();
    save_project(&other, &file, LupControl::default()).unwrap();
    let (doc5, _) = load_project(&file, LupControl::default()).unwrap();
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    let mut h5 = History::default();
    h5.reset(true);
    match h5.restore_journal(j, scan, identity(&doc5)) {
        Err((_, JournalProblem::Stale)) => {}
        other => panic!("{:?}", other.err().map(|e| e.1)),
    }
    assert!(!h5.can_undo());
    let _ = std::fs::remove_dir_all(&d);
}

/// Measurement (release): journaling a one-million-note paste, undoing it
/// from disk after a restart, and the identity pass at open / save.
#[test]
#[ignore]
fn journal_cost_of_a_million_note_paste() {
    let d = dir("perf");
    let (mut doc, mut h, file, jpath) = start(&d);
    let big: Vec<NoteRecord> = (0..1_000_000u64)
        .map(|k| note(k * 3, 20 + (k % 90) as u8))
        .collect();
    let mut scratch = Track::new(TrackId(9_999));
    scratch.notes.insert_sorted_batch(&big, false).unwrap();
    scratch.notes.select_in_region(0, u64::MAX, 0, 127);
    let clip = Clipboard::copy_selected(&scratch).unwrap().unwrap();
    let p = clip.prepare_paste(&doc, 0, 0).unwrap();
    let t = std::time::Instant::now();
    h.apply(
        &mut doc,
        Operation::Prepared {
            edit: p,
            label: "Paste",
        },
    )
    .unwrap();
    let apply = t.elapsed();
    let t = std::time::Instant::now();
    let id = identity(&doc);
    let ident = t.elapsed();
    save(&doc, &mut h, &file);
    let bytes = std::fs::metadata(&jpath).unwrap().len();
    drop(h);
    let t = std::time::Instant::now();
    let (mut doc, mut h, r) = reopen(&file, &jpath);
    let reopen_t = t.elapsed();
    assert_eq!(identity(&doc), id);
    let t = std::time::Instant::now();
    h.undo(&mut doc).unwrap();
    let undo = t.elapsed();
    println!(
        "1M-note paste: apply+journal {apply:?}; journal {bytes} bytes; identity {ident:?}; reopen+restore {reopen_t:?} ({} steps); undo from disk {undo:?}",
        r.undo_steps
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// §209 / §274: Compact History runs on a worker — planning reads no bodies,
/// the copy never touches the live history, a history changed meanwhile
/// refuses the result (temporary file removed, journal unchanged and still
/// working), cancelling removes the temporary file, and a finished
/// compaction undoes exactly from disk-only steps.
#[test]
fn compaction_on_a_worker_is_refused_when_the_history_changed_meanwhile() {
    let d = dir("compact-worker");
    let (mut doc, _, file, jpath) = start(&d);
    let mut h = History::new(HistoryLimits {
        max_bytes: 64 * 1024,
        max_entries: 4,
    });
    h.reset(true);
    let j = Journal::create(&jpath, path_key(&file)).unwrap();
    h.attach_new_journal(j, Some(identity(&doc))).unwrap();
    let mut ids = vec![identity(&doc)];
    for i in 0..30u64 {
        h.apply(
            &mut doc,
            Operation::InsertNote {
                track: 0.into(),
                record: note(2_000_000 + i, 61),
            },
        )
        .unwrap();
        ids.push(identity(&doc));
    }
    for _ in 0..5 {
        h.undo(&mut doc).unwrap();
    }
    h.apply(&mut doc, Operation::SetName("branch".into()))
        .unwrap();
    ids.truncate(26);
    let tmp = jpath.with_extension("plh.tmp");

    // Planned, copied on a thread, but an edit lands before it is installed.
    let job = h.plan_compaction(None).unwrap();
    assert!(job.records() > 0);
    let worker = std::thread::spawn(move || job.run(&std::sync::atomic::AtomicBool::new(false)));
    let done = worker.join().unwrap().unwrap();
    assert!(tmp.is_file());
    h.apply(&mut doc, Operation::SetName("meanwhile".into()))
        .unwrap();
    let size = h.journal().unwrap().disk_bytes();
    assert!(h.finish_compaction(done).is_err());
    assert!(!tmp.exists(), "the refused copy is removed");
    assert_eq!(h.journal().unwrap().disk_bytes(), size, "journal untouched");

    // Cancelled: nothing installed, no temporary file left.
    let job = h.plan_compaction(None).unwrap();
    assert!(job.run(&std::sync::atomic::AtomicBool::new(true)).is_err());
    assert!(!tmp.exists());

    // A clean run installs; every step still undoes exactly (most from disk).
    let job = h.plan_compaction(None).unwrap();
    let done = std::thread::spawn(move || job.run(&std::sync::atomic::AtomicBool::new(false)))
        .join()
        .unwrap()
        .unwrap();
    let (b, a) = h.finish_compaction(done).unwrap();
    assert!(a < b, "{a} < {b}");
    h.undo(&mut doc).unwrap(); // "meanwhile"
    h.undo(&mut doc).unwrap(); // "branch"
    assert_eq!(identity(&doc), ids[25]);
    for k in (0..25).rev() {
        h.undo(&mut doc).unwrap();
        assert_eq!(identity(&doc), ids[k]);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// L-S48 (562M notes on the owner's PC: the Apply of a 224M-note Import Into
/// held the window 5.4 s serializing its tracks into the journal): a track
/// block of more than `DEFER_NOTES` notes is written on a worker; Undo, Redo
/// and a new edit made meanwhile queue behind it; after "exit" the history
/// restores every step exactly.
#[test]
fn a_large_track_block_is_journaled_in_the_background_and_survives_restart() {
    let d = dir("deferred");
    let (mut doc, mut h, file, jpath) = start(&d);
    let ids0 = identity(&doc);
    let first = doc.next_track_id();
    let tracks: Vec<Track> = (0..2u32)
        .map(|k| {
            let mut t = Track::new(TrackId(first + k));
            let recs: Vec<NoteRecord> = (0..600_000u64)
                .map(|i| note(i * 30, 30 + ((i + u64::from(k)) % 60) as u8))
                .collect();
            t.notes.insert_sorted_batch(&recs, false).unwrap();
            t
        })
        .collect();
    let index = doc.track_count();
    h.apply(
        &mut doc,
        Operation::InsertTracks {
            index,
            tracks,
            label: "Import MIDI Into Track",
        },
    )
    .unwrap();
    assert!(
        h.journal().unwrap().writing(),
        "the block goes to the worker"
    );
    let id1 = identity(&doc);
    assert_eq!(doc.note_count(), 200 + 1_200_000);
    // Meanwhile (the worker may still be writing): Undo, Redo, a new edit.
    h.undo(&mut doc).unwrap().unwrap();
    assert_eq!(identity(&doc), ids0);
    h.redo(&mut doc).unwrap().unwrap();
    assert_eq!(identity(&doc), id1);
    h.apply(
        &mut doc,
        Operation::InsertNote {
            track: 0.into(),
            record: note(90_000, 64),
        },
    )
    .unwrap();
    let id2 = identity(&doc);
    // The app polls every frame; a save waits for the writer.
    h.poll_journal();
    save(&doc, &mut h, &file);
    assert!(!h.journal().unwrap().writing());
    assert_eq!(h.journal().unwrap().failures().0, 0);
    drop(h); // exit
    let (mut doc, mut h, report) = reopen(&file, &jpath);
    assert_eq!(identity(&doc), id2);
    assert_eq!((report.undo_steps, report.redo_steps), (2, 0));
    h.undo(&mut doc).unwrap().unwrap();
    assert_eq!(identity(&doc), id1);
    h.undo(&mut doc).unwrap().unwrap();
    assert_eq!(identity(&doc), ids0);
    h.redo(&mut doc).unwrap().unwrap();
    assert_eq!(
        identity(&doc),
        id1,
        "the block is read back from the journal"
    );
    assert_eq!(doc.note_count(), 200 + 1_200_000);
    let _ = std::fs::remove_dir_all(&d);
}

/// L-S48: Import MIDI Into Track as the app applies it — one group step (the
/// track block and the Conductor) — journals its large block in the
/// background and restores exactly.
#[test]
fn a_large_import_into_group_is_journaled_in_the_background() {
    use plumspirit_engine::midi::merge::{ConductorPolicy, MergeMode, MergeOptions, prepare_merge};
    let d = dir("deferred-group");
    let (mut doc, mut h, file, jpath) = start(&d);
    let ids0 = identity(&doc);
    let mut src = PlumDocument::new_project("src".into(), Bpm::DEFAULT, 480);
    let recs: Vec<NoteRecord> = (0..1_200_000u64)
        .map(|i| note(i * 15, 30 + (i % 60) as u8))
        .collect();
    src.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let p = prepare_merge(
        &doc,
        0,
        &src,
        MergeOptions {
            mode: MergeMode::AsSubtracks,
            policy: ConductorPolicy::Keep,
            at: 0,
        },
    )
    .unwrap();
    let before = h.journal().unwrap().disk_bytes();
    h.apply(&mut doc, p.operation).unwrap();
    assert!(
        h.journal().unwrap().writing(),
        "the block goes to the worker"
    );
    assert_eq!(h.journal().unwrap().disk_bytes(), before);
    let id1 = identity(&doc);
    assert_eq!(doc.note_count(), 200 + 1_200_000);
    h.undo(&mut doc).unwrap().unwrap();
    h.redo(&mut doc).unwrap().unwrap();
    save(&doc, &mut h, &file);
    assert_eq!(h.journal().unwrap().failures().0, 0);
    drop(h);
    let (mut doc, mut h, report) = reopen(&file, &jpath);
    assert_eq!(identity(&doc), id1);
    assert_eq!((report.undo_steps, report.redo_steps), (1, 0));
    h.undo(&mut doc).unwrap().unwrap();
    assert_eq!(identity(&doc), ids0);
    h.redo(&mut doc).unwrap().unwrap();
    assert_eq!(identity(&doc), id1);
    let _ = std::fs::remove_dir_all(&d);
}

/// L-S48: a journal dropped while a large entry is being written finishes it
/// and the records queued behind it (nothing is cut off).
#[test]
fn a_journal_dropped_while_writing_finishes_its_records() {
    let d = dir("deferred-drop");
    let (mut doc, mut h, file, jpath) = start(&d);
    let first = doc.next_track_id();
    let mut t = Track::new(TrackId(first));
    let recs: Vec<NoteRecord> = (0..1_100_000u64)
        .map(|i| note(i * 30, 30 + (i % 60) as u8))
        .collect();
    t.notes.insert_sorted_batch(&recs, false).unwrap();
    let index = doc.track_count();
    h.apply(
        &mut doc,
        Operation::InsertTracks {
            index,
            tracks: vec![t],
            label: "Paste",
        },
    )
    .unwrap();
    h.undo(&mut doc).unwrap().unwrap();
    let j = h.detach_journal().unwrap();
    assert!(!j.writing(), "detaching waits for the writer");
    drop(j);
    drop(h);
    // Unsaved session: the step can be redone after reopening.
    let (mut doc, mut h, report) = reopen(&file, &jpath);
    assert_eq!((report.undo_steps, report.redo_steps), (0, 1));
    h.redo(&mut doc).unwrap().unwrap();
    assert_eq!(doc.note_count(), 200 + 1_100_000);
    let _ = std::fs::remove_dir_all(&d);
}
