//! B1-0025 / B1-0036: budgets (instructions, output notes), progress and
//! yield, and scripts on a worker thread with the UI thread never blocked.

use super::worker::{ScriptJob, ScriptWorker};
use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn project(notes: u64) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let recs: Vec<NoteRecord> = (0..notes)
        .map(|k| NoteRecord {
            start_tick: k * 10,
            duration_ticks: 10,
            attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
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

fn world(doc: &Arc<PlumDocument>, h: &mut History) -> World {
    h.observe_ids(doc);
    World::new(Arc::clone(doc), h.incarnations().snapshot())
}

const EDIT: [Capability; 2] = [Capability::ReadProject, Capability::EditNotes];

#[test]
fn an_instruction_budget_stops_a_script_with_its_own_category() {
    let h = Host::new(
        Limits {
            instructions: Some(1_000_000),
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    let e = h
        .run("spin.lua", "local n = 0\nwhile true do n = n + 1 end")
        .unwrap_err();
    assert_eq!(e.category, Category::Budget, "{e}");
    assert_eq!(e.category.name(), "budget");
    assert!(e.message.contains("1000000 instructions"), "{e}");
    assert_eq!(e.line, Some(2));
    // Each run has its own count.
    assert_eq!(
        h.run(
            "ok.lua",
            "local s = 0 for i = 1, 1000 do s = s + i end return s"
        )
        .unwrap(),
        Plain::Integer(500_500)
    );
}

#[test]
fn output_notes_are_counted_before_anything_is_built() {
    let (doc, mut h) = project(100);
    let host = Host::new(
        Limits {
            max_output_notes: 150,
            ..Limits::default()
        },
        EDIT,
    )
    .unwrap();
    host.set_world(Some(world(&doc, &mut h)));
    // 100 changed + 60 duplicated > 150: refused at the duplicate, before
    // a note is built; the run fails and nothing is committed.
    let e = host
        .run(
            "big.lua",
            r#"local t = plum.project.current():track(1)
               local tx = plum.transaction{}
               assert(tx:modify_notes(t:notes(), { velocity = 1 }) == 100)
               tx:duplicate_notes(t:notes{ end_tick = 600 }, { shift = 1 })"#,
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Budget, "{e}");
    assert!(e.message.contains("160 notes"), "{e}");
    assert_eq!(e.line, Some(4));
    assert!(host.take_commits().is_none());
    // Within the budget it works.
    host.run(
        "ok.lua",
        r#"local t = plum.project.current():track(1)
           local tx = plum.transaction{}
           tx:duplicate_notes(t:notes{ end_tick = 500 }, { shift = 1 })
           tx:commit()"#,
    )
    .unwrap();
    assert!(host.take_commits().is_some());
}

#[test]
fn a_script_on_a_worker_never_blocks_the_ui_thread() {
    let (doc, mut h) = project(1000);
    let scripts = Scripts::default();
    let mut w = ScriptWorker::spawn(
        Limits {
            time: Duration::from_secs(60),
            ..Limits::default()
        },
        EDIT.to_vec(),
        &scripts,
    )
    .unwrap();
    // A script that reports progress, then waits to be canceled.
    w.submit(ScriptJob {
        file: "long.lua".into(),
        source: r#"plum.progress(0.5, "half way")
                   while true do plum.yield() end"#
            .into(),
        world: Some(world(&doc, &mut h)),
        time_limit: None,
    })
    .unwrap();
    assert!(w.is_busy());
    // The UI thread polls without blocking and sees the progress.
    let started = Instant::now();
    let mut seen = false;
    while started.elapsed() < Duration::from_secs(10) {
        let t = Instant::now();
        assert!(w.poll().is_none());
        assert!(t.elapsed() < Duration::from_millis(50), "poll never blocks");
        let p = w.progress();
        if p.fraction == Some(0.5) && p.message == "half way" {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(seen, "progress reached the UI thread");
    let refused = w.submit(ScriptJob {
        file: "x.lua".into(),
        source: "return 1".into(),
        world: None,
        time_limit: None,
    });
    assert!(refused.is_err(), "one job at a time");
    // Closing PlumSpirit: the app's registry cancels it.
    assert_eq!(scripts.cancel_all(), 1);
    let r = w.wait(Duration::from_secs(10)).expect("the job ends");
    assert_eq!(r.result.unwrap_err().category, Category::Canceled);
    assert!(!w.is_busy());
    // The next job works and its commits come back for the UI thread.
    w.submit(ScriptJob {
        file: "edit.lua".into(),
        source: r#"print("editing")
                   local tx = plum.transaction{ label = "Worker edit" }
                   tx:modify_notes(plum.project.current():track(1):notes(), { velocity = 9 })
                   tx:commit()
                   return 7"#
            .into(),
        world: Some(world(&doc, &mut h)),
        time_limit: None,
    })
    .unwrap();
    let r = w.wait(Duration::from_secs(30)).expect("the job ends");
    assert_eq!(r.result.unwrap(), Plain::Integer(7));
    assert_eq!(r.output, vec!["editing".to_owned()]);
    let mut commits = r.commits.expect("commits");
    let mut doc = doc;
    // The worker kept no snapshot: the UI thread can apply at once.
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(h.undo_label(), Some("Worker edit"));
    // Dropping a busy worker never waits.
    w.submit(ScriptJob {
        file: "spin.lua".into(),
        source: "while true do end".into(),
        world: None,
        time_limit: None,
    })
    .unwrap();
    let t = Instant::now();
    drop(w);
    assert!(t.elapsed() < Duration::from_millis(100));
}
