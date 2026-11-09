//! B1-0029 / B1-0033: the Notes API — streamed queries, native batch edits,
//! callbacks, inserts — with Preview = Apply, Cancel = the exact hash, and
//! Undo / Redo after a script transaction.

use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
use std::sync::Arc;

fn note(k: u64) -> NoteRecord {
    NoteRecord {
        start_tick: k * 120,
        duration_ticks: 100 + k % 7,
        attrs: NoteAttrs::new(
            60 + (k % 12) as u8,
            1 + (k % 127) as u8,
            (k % 4) as u8,
            0,
            NoteOffKind::NoteOff,
        )
        .unwrap(),
        sub: Default::default(),
        group: 0,
    }
}

fn project(notes: u64) -> (Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let recs: Vec<NoteRecord> = (0..notes).map(note).collect();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, false)
        .unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    (Arc::new(d), h)
}

const EDIT: [Capability; 2] = [Capability::ReadProject, Capability::EditNotes];

fn host_for(doc: &Arc<PlumDocument>, h: &mut History) -> Host {
    h.observe_ids(doc);
    let host = Host::new(Limits::default(), EDIT).unwrap();
    host.set_world(Some(World::new(
        Arc::clone(doc),
        h.incarnations().snapshot(),
    )));
    host
}

fn run(host: &Host, src: &str) -> Plain {
    host.run("notes.lua", src).unwrap_or_else(|e| panic!("{e}"))
}

fn hash(d: &PlumDocument) -> Plain {
    Plain::String(format!("{:016x}", d.export_fingerprint()))
}

#[test]
fn queries_count_and_stream_exactly_what_the_filter_selects() {
    let (mut doc, mut h) = project(1000);
    // Select every 10th note.
    {
        let d = Arc::get_mut(&mut doc).unwrap();
        let mut b = plumspirit_engine::model::RankSetBuilder::default();
        for r in (0..1000).step_by(10) {
            b.push(r);
        }
        d.track_mut(0)
            .unwrap()
            .notes
            .select_ranks(&b.finish())
            .unwrap();
    }
    let brute = |f: &dyn Fn(u64, &NoteRecord) -> bool| -> i64 {
        (0..1000u64).filter(|&k| f(k, &note(k))).count() as i64
    };
    let host = host_for(&doc, &mut h);
    let t = r#"local t = plum.project.current():track(1) "#;
    let count = |filter: &str| run(&host, &format!("{t} return t:notes{filter}:count()"));
    assert_eq!(count("()"), Plain::Integer(1000));
    assert_eq!(
        count("{ start_tick = 1200, end_tick = 2400 }"),
        Plain::Integer(brute(&|_, r| (1200..2400).contains(&{ r.start_tick })))
    );
    assert_eq!(
        count("{ keys = { 60, 62 }, channel = 1 }"),
        Plain::Integer(brute(
            &|_, r| r.attrs.pitch() <= 62 && r.attrs.channel() == 1
        ))
    );
    assert_eq!(
        count("{ velocity = { 100, 127 } }"),
        Plain::Integer(brute(&|_, r| r.attrs.velocity() >= 100))
    );
    assert_eq!(
        count("{ selected = true }"),
        Plain::Integer(100),
        "selection"
    );
    assert_eq!(
        count("{ keys = 61 }"),
        Plain::Integer(brute(&|_, r| r.attrs.pitch() == 61))
    );
    // Batches of 7 cover the query once, in stored order, with exact values.
    let r = run(
        &host,
        &format!(
            r#"{t}
            local n, last, sum, sel = 0, -1, 0, 0
            for b in t:notes{{ end_tick = 60000 }}:batches(7) do
                assert(b.count <= 7 and #b.start == b.count)
                for i = 1, b.count do
                    assert(b.index[i] == n + 1, "stored order")
                    assert(b.start[i] >= last)
                    assert(math.type(b.start[i]) == "integer")
                    last = b.start[i]
                    sum = sum + b.key[i] * b.velocity[i] + b.channel[i] + b.gate[i]
                    if b.selected[i] then sel = sel + 1 end
                    n = n + 1
                end
            end
            return n * 1000000 + sel * 10000 + (sum % 10000)"#
        ),
    );
    let mut sum = 0i64;
    let mut n = 0;
    for k in 0..500u64 {
        let r = note(k);
        sum += i64::from(r.attrs.pitch()) * i64::from(r.attrs.velocity())
            + i64::from(r.attrs.channel())
            + r.duration_ticks as i64;
        n += 1;
    }
    assert_eq!(
        r,
        Plain::Integer(n * 1_000_000 + 50 * 10_000 + sum % 10_000)
    );
    // each() and handles().
    assert_eq!(
        run(
            &host,
            &format!(
                r#"{t}
                local n = 0
                for nt in t:notes{{ keys = 60 }}:each() do
                    assert(nt.key == 60 and nt.start % 1440 == 0)
                    n = n + 1
                end
                local hs = t:notes{{ start_tick = 0, end_tick = 360 }}:handles()
                assert(#hs == 3 and hs[1]:kind() == "note" and hs[1]:valid())
                return n"#
            )
        ),
        Plain::Integer(brute(&|_, r| r.attrs.pitch() == 60))
    );
    // Only the asked columns.
    assert_eq!(
        run(
            &host,
            &format!(
                "{t} local b = t:notes():batches{{ size = 10, columns = {{ 'key' }} }}() return (b.start == nil and #b.key == 10) and 1 or 0"
            )
        ),
        Plain::Integer(1)
    );
    // A one-note batch.
    assert_eq!(
        run(
            &host,
            &format!("{t} local b = t:notes{{ end_tick = 1 }}:batches(1)() return b.count")
        ),
        Plain::Integer(1)
    );
    // Wrong arguments are sentences.
    let e = host
        .run(
            "n.lua",
            &format!("{t} return t:notes{{ keys = {{ 70, 60 }} }}"),
        )
        .unwrap_err();
    assert!(e.message.contains("keys must be"), "{e}");
    let e = host
        .run("n.lua", &format!("{t} return t:notes():batches(0)"))
        .unwrap_err();
    assert!(e.message.contains("batch holds"), "{e}");
}

#[test]
fn native_edits_preview_equals_apply_and_undo_restores() {
    let (mut doc, mut h) = project(500);
    let before = hash(&doc);
    let host = host_for(&doc, &mut h);
    let preview = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:track(1)
           local tx = plum.transaction{ label = "Brighten" }
           local n = tx:modify_notes(t:notes{ channel = 2 }, { velocity_add = 5, transpose = 12, shift = 960 })
           assert(n == 125, n)
           -- Reads see the change: transposed notes are now 72 - 83.
           assert(t:notes{ keys = { 72, 83 } }:count() == 125)
           assert(t:notes{ channel = 2, keys = { 60, 71 } }:count() == 0)
           assert(p:selected_note_count() == 125, "changed notes are selected")
           local d = tx:delete_notes(t:notes{ channel = 3 })
           assert(d == 125)
           local e = tx:estimate()
           assert(e.removed_notes == 125 and e.inserted_notes == 125, e.removed_notes)
           local preview = p:content_hash()
           tx:commit()
           return preview"#,
    );
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(hash(&doc), preview, "Preview = Apply");
    assert_eq!(doc.note_count(), 375);
    // Exactly the expected notes.
    let mut expect: Vec<NoteRecord> = (0..500u64)
        .map(note)
        .filter(|r| r.attrs.channel() != 3)
        .map(|r| {
            if r.attrs.channel() == 2 {
                NoteRecord {
                    start_tick: r.start_tick + 960,
                    attrs: r
                        .attrs
                        .with_pitch_wide(r.attrs.pitch() + 12)
                        .unwrap()
                        .with_velocity((r.attrs.velocity() + 5).min(127))
                        .unwrap(),
                    ..r
                }
            } else {
                r
            }
        })
        .collect();
    expect.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
    let mut got: Vec<NoteRecord> = doc
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|v| v.record())
        .collect();
    got.sort_by_key(|r| (r.start_tick, r.attrs.pitch()));
    assert_eq!(got, expect);
    // One Undo step back to the exact hash; Redo forward again.
    let d = Arc::get_mut(&mut doc).unwrap();
    h.undo(d).unwrap().unwrap();
    assert_eq!(hash(d), before);
    h.redo(d).unwrap().unwrap();
    assert_eq!(hash(d), preview);
}

#[test]
fn cancel_or_an_error_halfway_leaves_the_exact_hash() {
    let (doc, mut h) = project(200);
    let before = hash(&doc);
    let host = host_for(&doc, &mut h);
    let r = run(
        &host,
        r#"local p = plum.project.current()
           local t = p:track(1)
           local before = p:content_hash()
           local tx = plum.transaction{}
           tx:modify_notes(t:notes(), { velocity = 1 })
           assert(p:content_hash() ~= before)
           tx:cancel()
           return p:content_hash()"#,
    );
    assert_eq!(r, before, "Cancel: the reads are back to the document");
    assert!(host.take_commits().is_none());
    let e = host
        .run(
            "half.lua",
            r#"local t = plum.project.current():track(1)
               local tx = plum.transaction{}
               tx:delete_notes(t:notes{ end_tick = 6000 })
               tx:modify_notes(t:notes(), { transpose = 100 })"#,
        )
        .unwrap_err();
    assert!(e.message.contains("outside the project's keys"), "{e}");
    assert!(e.message.contains("nothing was changed"), "{e}");
    assert!(host.take_commits().is_none());
    assert_eq!(hash(&doc), before);
}

#[test]
fn callbacks_inserts_and_duplicates_validate_every_note() {
    let (mut doc, mut h) = project(24);
    let host = host_for(&doc, &mut h);
    let r = run(
        &host,
        r#"local t = plum.project.current():track(1)
           local tx = plum.transaction{ label = "Script notes" }
           -- A callback: keep, change or delete each note.
           local changed = tx:modify_notes(t:notes(), function(n)
               if n.key == 60 then return false end           -- delete
               if n.key == 61 then return { velocity = 127, gate = 480 } end
               return nil                                     -- keep
           end)
           assert(changed == 2, changed)
           assert(t:notes{ keys = 60 }:count() == 0)
           assert(t:notes{ keys = 61, velocity = 127 }:count() == 2)
           -- Inserts: a list and struct-of-arrays.
           assert(tx:insert_notes(t, { { start = 0, gate = 10, key = 30 }, { start = 5, gate = 10, key = 31, velocity = 9, channel = 15 } }) == 2)
           assert(tx:insert_notes(t, { start = { 7, 3 }, gate = { 1, 1 }, key = { 40, 41 } }) == 2)
           assert(t:notes{ keys = { 30, 41 } }:count() == 4)
           assert(t:notes{ keys = 31, channel = 15, velocity = 9 }:count() == 1)
           -- Duplicates an octave up.
           assert(tx:duplicate_notes(t:notes{ keys = { 30, 31 } }, { transpose = 12, shift = 1 }) == 2)
           assert(t:notes{ keys = { 42, 43 } }:count() == 2)
           tx:commit()
           return t:notes():count()"#,
    );
    assert_eq!(r, Plain::Integer(24 - 2 + 4 + 2));
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut h, &mut doc).unwrap(), 1);
    assert_eq!(doc.note_count(), 28);
    // Validation: every refusal is a sentence, and nothing is changed.
    let host = host_for(&doc, &mut h);
    for (src, words) in [
        ("{ { start = 0, gate = 1, key = 200 } }", "highest key"),
        (
            "{ { start = 0, gate = 1, key = 60, velocity = 0 } }",
            "velocity is 1 - 127",
        ),
        (
            "{ { start = 0, gate = 1, key = 60, subchannel = 7 } }",
            "no subchannel 8",
        ),
        ("{ { start = -1, gate = 1, key = 60 } }", "negative"),
        ("{ { start = 0, key = 60 } }", "needs a gate"),
    ] {
        let e = host
            .run(
                "bad.lua",
                &format!(
                    "local t = plum.project.current():track(1) plum.transaction{{}}:insert_notes(t, {src})"
                ),
            )
            .unwrap_err();
        assert!(e.message.contains(words), "{src}: {e}");
        assert!(host.take_commits().is_none());
    }
}

#[test]
fn direct_columns_match_rows_defaults_large_ticks_and_stable_equal_starts() {
    let (doc, mut history) = project(0);
    let rows = host_for(&doc, &mut history);
    let columns = host_for(&doc, &mut history);
    let row_hash = run(
        &rows,
        r#"
        local p=plum.project.current();local tx=plum.transaction{}
        tx:insert_notes(p:track(1),{
            {start='9007199254740993',gate=1,key=64,velocity=9,channel=15},
            {start='9007199254740992',gate=2,key=65},
            {start='9007199254740993',gate=3,key=67,velocity=127}
        })
        tx:commit();return p:content_hash()
    "#,
    );
    let column_hash = run(
        &columns,
        r#"
        local p=plum.project.current();local tx=plum.transaction{}
        tx:insert_notes(p:track(1),{
            start={'9007199254740993','9007199254740992','9007199254740993'},
            gate={1,2,3},key={64,65,67},velocity={9,100,127},channel={15,0,0}
        })
        tx:commit();return p:content_hash()
    "#,
    );
    assert_eq!(row_hash, column_hash);
    let row_commits = rows.take_commits().unwrap();
    let column_commits = columns.take_commits().unwrap();
    let records = |commits: &tx::Commits| {
        commits
            .result()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect::<Vec<_>>()
    };
    assert_eq!(records(&row_commits), records(&column_commits));
    let got = records(&column_commits);
    assert_eq!(
        got.iter().map(|r| r.attrs.pitch()).collect::<Vec<_>>(),
        [65, 64, 67]
    );
    assert_eq!({ got[0].start_tick }, 9_007_199_254_740_992);
    assert_eq!({ got[1].start_tick }, 9_007_199_254_740_993);
    assert_eq!(got[0].attrs.velocity(), 100);
    assert_eq!(got[0].sub, Default::default());
    columns.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    for (payload, words) in [
        ("{start={0,1},gate={1},key={60,61}}", "needs a gate"),
        (
            "{start={0},gate={1},key={60},velocity={0}}",
            "velocity is 1 - 127",
        ),
        ("{start={0},gate={1},key={200}}", "highest key"),
        (
            "{start={0},gate={1},key={60},subchannel={7}}",
            "no subchannel",
        ),
    ] {
        let error=columns.run("invalid-columns.lua",&format!("local p=plum.project.current();plum.transaction{{}}:insert_notes(p:track(1),{payload})")).unwrap_err();
        assert!(error.message.contains(words), "{}", error.message);
        assert!(columns.take_commits().is_none());
        assert_eq!(doc.note_count(), 0);
    }
}

#[test]
fn column_insert_65536_notes_fits_eight_mib_vm_with_gc_stopped() {
    let (mut doc, mut history) = project(0);
    history.observe_ids(&doc);
    let host = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Limits::default()
        },
        EDIT,
    )
    .unwrap();
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    host.lua.gc_stop();
    let preview = run(
        &host,
        r#"
        local p=plum.project.current();local track=p:track(1)
        local starts,gates,keys={},{},{}
        for n=1,65536 do starts[n]=n*2;gates[n]=1;keys[n]=60 end
        local tx=plum.transaction{label='Column allocation fixture'}
        assert(tx:insert_notes(track,{start=starts,gate=gates,key=keys})==65536)
        tx:commit();return p:content_hash()
    "#,
    );
    assert!(host.lua.used_memory() < 8 * 1024 * 1024);
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.apply(&mut history, &mut doc).unwrap(), 1);
    assert_eq!(doc.note_count(), 65536);
    assert_eq!(hash(&doc), preview);
    let notes = &doc.track(0).unwrap().notes;
    let first = notes.iter().next().unwrap().record();
    let last = notes.iter().last().unwrap().record();
    assert_eq!((first.start_tick, last.start_tick), (2, 131072));
    assert_eq!(first.attrs.velocity(), 100);
    history
        .undo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.note_count(), 0);
    history
        .redo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(hash(&doc), preview);
}

#[test]
fn note_writes_need_the_notes_edit_capability() {
    let (doc, mut h) = project(4);
    h.observe_ids(&doc);
    let host = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        h.incarnations().snapshot(),
    )));
    let e = host
        .run(
            "w.lua",
            "local t = plum.project.current():track(1) plum.transaction{}:delete_notes(t:notes())",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e}");
    assert!(e.message.contains("notes.edit"), "{e}");
}

/// B1-0029 bounded batch use at 10M notes (release):
/// `cargo test --release -p plumspirit notes_at_10m -- --ignored --nocapture`
#[test]
#[ignore]
fn notes_at_10m_stream_and_edit_in_bounded_batches() {
    use std::time::Instant;
    let n = 10_000_000u64;
    let t = Instant::now();
    let (mut doc, _) = project(n);
    // The project's history as on a PC with 8 GB (1/16 of RAM, at most 512 MB).
    let mut h = History::new(HistoryLimits::for_system_memory(Some(8 << 30)));
    h.reset(false);
    println!(
        "fixture {n} notes: {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    h.observe_ids(&doc);
    let host = Host::new(
        Limits {
            memory: 256 * 1024 * 1024,
            time: std::time::Duration::from_secs(120),
            ..Limits::default()
        },
        EDIT,
    )
    .unwrap();
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        h.incarnations().snapshot(),
    )));
    let t = Instant::now();
    let r = run(
        &host,
        "return plum.project.current():track(1):notes():count()",
    );
    assert_eq!(r, Plain::Integer(n as i64));
    println!(
        "count (range only): {:.2} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let r = run(
        &host,
        "return plum.project.current():track(1):notes{ channel = 1 }:count()",
    );
    assert_eq!(r, Plain::Integer((n / 4) as i64));
    println!(
        "count (filtered, every note visited): {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let r = run(
        &host,
        r#"local s = 0
           for b in plum.project.current():track(1):notes():batches(65536) do
               s = s + b.count
           end
           return s"#,
    );
    assert_eq!(r, Plain::Integer(n as i64));
    println!(
        "stream all in 65,536-note batches (Lua tables): {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let r = run(
        &host,
        r#"local s = 0
           for b in plum.project.current():track(1):notes():batches{ size = 65536, columns = { "start", "key" } } do
               s = s + #b.key
           end
           return s"#,
    );
    assert_eq!(r, Plain::Integer(n as i64));
    println!(
        "stream all, two columns (start, key): {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    run(
        &host,
        r#"local tx = plum.transaction{ label = "Velocity +1" }
           tx:modify_notes(plum.project.current():track(1):notes(), { velocity_add = 1 })
           tx:commit()"#,
    );
    println!(
        "native modify of every note (working copy): {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let mut commits = host.take_commits().unwrap();
    let t = Instant::now();
    commits.apply(&mut h, &mut doc).unwrap();
    println!(
        "commit as one Undo step: {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(doc.note_count(), n as usize);
}

/// B1-0024 / B1-0029: a native note loop stops at its next check when the
/// run is canceled (here by a host function, as closing PlumSpirit would).
#[test]
fn a_native_note_loop_stops_when_the_run_is_canceled() {
    let (doc, mut h) = project(300_000);
    let host = host_for(&doc, &mut h);
    let flag = host.cancel_handle();
    host.register("trip", move |_, _| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(mlua::MultiValue::new())
    })
    .unwrap();
    let e = host
        .run(
            "cancel.lua",
            "plum.trip() return plum.project.current():track(1):notes{ channel = 1 }:count()",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Canceled, "{e}");
}

/// An edit too large for the project's undo memory is applied without undo
/// after the user agrees (like any PlumSpirit edit), to exactly the preview.
#[test]
fn a_commit_too_large_for_undo_can_be_applied_without_undo() {
    let (mut doc, _) = project(20_000);
    let mut h = History::new(HistoryLimits {
        max_bytes: 4096,
        max_entries: 2_000,
    });
    h.reset(false);
    let host = host_for(&doc, &mut h);
    let preview = run(
        &host,
        r#"local p = plum.project.current()
           local tx = plum.transaction{ label = "Big" }
           tx:modify_notes(p:track(1):notes(), { velocity = 64 })
           local hash = p:content_hash()
           tx:commit()
           return hash"#,
    );
    let mut commits = host.take_commits().unwrap();
    let e = commits.apply(&mut h, &mut doc).unwrap_err();
    assert!(
        matches!(e, super::tx::CommitError::TooLargeForUndo { step: 0, .. }),
        "{e:?}"
    );
    assert!(e.user_message().contains("undo"), "{}", e.user_message());
    assert_ne!(hash(&doc), preview, "nothing applied yet");
    commits.apply_without_undo(&mut h, &mut doc).unwrap();
    assert_eq!(hash(&doc), preview, "exactly the preview");
    assert!(!h.can_undo(), "the history was cleared");
}
