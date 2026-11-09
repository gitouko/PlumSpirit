use super::*;
use crate::lua::{Limits, Plain, handles::World};
use plumspirit_engine::model::{Bpm, project::KeyRange};
use std::cell::Cell;
use std::time::Duration;

fn fixture(count: usize, limits: Limits) -> (Host, Arc<PlumDocument>, History) {
    let mut doc = PlumDocument::new_project("Writer".into(), Bpm::DEFAULT, 960);
    doc.add_track().unwrap();
    let records: Vec<_> = (0..count).map(|i| record(i as u64 * 10, 60)).collect();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&records, false)
        .unwrap();
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.reset(false);
    history.observe_ids(&doc);
    let host = Host::new(
        limits,
        [
            Capability::ReadProject,
            Capability::EditNotes,
            Capability::EditTracks,
        ],
    )
    .unwrap();
    host.lua
        .set_app_data(NativeBudget::isolated(256 * 1024 * 1024));
    bind(&host, &doc, &history);
    host.lua
        .globals()
        .set(
            "native_used",
            host.lua
                .create_function(|lua, ()| Ok(budget(lua)?.used()))
                .unwrap(),
        )
        .unwrap();
    (host, doc, history)
}

fn bind(host: &Host, doc: &Arc<PlumDocument>, history: &History) {
    host.set_world(Some(World::new(
        Arc::clone(doc),
        history.incarnations().snapshot(),
    )));
}
fn run(host: &Host, src: &str) -> Plain {
    host.run("writer.lua", src)
        .unwrap_or_else(|e| panic!("{e}"))
}
fn used(host: &Host) -> usize {
    budget(&host.lua).unwrap().used()
}
fn record(start: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: 1,
        attrs: NoteAttrs::new_wide(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId::MAIN,
        group: 0,
    }
}

#[test]
fn private_staging_one_finish_preview_apply_undo_redo_and_retained_accounting() {
    let (host, mut doc, mut history) = fixture(12, Limits::default());
    let original = doc.export_fingerprint();
    let preview = run(
        &host,
        r#"
        local p=plum.project.current(); local t=p:track(1)
        local tx=plum.transaction{label='Stream notes'}
        retained=tx:note_writer(t)
        assert(retained:append({start={23,11},gate={3,2},key={70,71}})==2)
        assert(t:notes():count()==12 and tx:estimate().inserted_notes==0)
        assert(tx:estimate().staged_notes==2)
        assert(retained:info().state=='open' and retained:info().staged_bytes>=56)
        assert(not pcall(function() tx:commit() end))
        assert(retained:finish()==2 and retained:info().state=='finished')
        assert(tx:estimate().staged_notes==0 and tx:estimate().inserted_notes==2)
        assert(not pcall(function() retained:cancel() end))
        local hash=p:content_hash(); tx:commit(); return hash
    "#,
    );
    assert_eq!(doc.export_fingerprint(), original);
    assert!(used(&host) > 0);
    let mut commits = host.take_commits().unwrap();
    assert!(used(&host) > 0, "pending Preview owns native reservations");
    assert_eq!(commits.steps.len(), 1);
    assert_eq!(commits.steps[0].operations, 1);
    assert_eq!(commits.result().note_count(), 14);
    assert_eq!(commits.apply(&mut history, &mut doc).unwrap(), 1);
    assert_eq!(
        Plain::String(format!("{:016x}", doc.export_fingerprint())),
        preview
    );
    drop(commits);
    assert_eq!(used(&host), 0, "Lua retained handle owns no native staging");
    history
        .undo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.export_fingerprint(), original);
    assert!(!history.can_undo());
    history
        .redo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.note_count(), 14);
    bind(&host, &doc, &history);
    assert!(
        host.run("expired.lua", "retained:info()")
            .unwrap_err()
            .message
            .contains("finished job")
    );
    assert_eq!(used(&host), 0);
}

#[test]
fn batch_size_independent_stable_order_matches_legacy_and_exact_ticks() {
    let mut expected = None;
    for batch_size in [1, 7, 64, 512] {
        let (host, doc, _) = fixture(0, Limits::default());
        run(
            &host,
            &format!(
                r#"
            local p=plum.project.current(); local t=p:track(1); local tx=plum.transaction{{}}
            local w=tx:note_writer(t)
            for first=1,257,{batch_size} do
                local b={{}}
                for i=first,math.min(first+{batch_size}-1,257) do
                    b[#b+1]={{start=9007199254740993+(i*7)%13,gate=i,key=40+i%70,velocity=1+i%127,channel=i%16}}
                end
                w:append(b)
            end
            w:append({{{{start='9223372036854775806',gate=1,key=60}}}})
            w:finish(); tx:commit()
        "#
            ),
        );
        let commits = host.take_commits().unwrap();
        let got: Vec<_> = commits
            .result()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|v| v.record())
            .collect();
        let mut legacy: Vec<_> = (1..=257u64)
            .map(|i| NoteRecord {
                start_tick: 9_007_199_254_740_993 + (i * 7) % 13,
                duration_ticks: i,
                attrs: NoteAttrs::new(
                    40 + (i % 70) as u8,
                    1 + (i % 127) as u8,
                    (i % 16) as u8,
                    0,
                    NoteOffKind::NoteOff,
                )
                .unwrap(),
                sub: SubchannelId::MAIN,
                group: 0,
            })
            .collect();
        legacy.push(record(i64::MAX as u64 - 1, 60));
        legacy.sort_by_key(|r| r.start_tick);
        assert_eq!(got, legacy);
        let hash = commits.result().export_fingerprint();
        assert_eq!(*expected.get_or_insert(hash), hash);
        assert_eq!(doc.note_count(), 0);
        drop(commits);
        assert_eq!(used(&host), 0);
    }
}

#[test]
fn failed_tenth_batch_poison_requires_explicit_discard_and_cancel_frees_buffers() {
    let (host, doc, _) = fixture(4, Limits::default());
    let original = doc.export_fingerprint();
    run(
        &host,
        r#"
        local t=plum.project.current():track(1); local tx=plum.transaction{}
        retained=tx:note_writer(t)
        for i=1,9 do retained:append({{start=i,gate=1,key=70}}) end
        assert(not pcall(function() retained:append({{start=10,gate=1,key=70},{start=11,gate=1,key=900}}) end))
        assert(retained:info().state=='failed' and retained:info().staged_bytes==0)
        assert(t:notes():count()==4)
        assert(not pcall(function() retained:finish() end))
        assert(not pcall(function() tx:commit() end))
        retained:cancel(); retained:cancel(); tx:commit()
        assert(native_used()==0)
    "#,
    );
    assert!(host.take_commits().is_none());
    assert_eq!(doc.export_fingerprint(), original);
    assert_eq!(used(&host), 0);
    for end in ["tx:cancel()", "error('batch 10')", "-- run ends open"] {
        let result = host.run(
            "discard.lua",
            &format!(
                r#"
            local tx=plum.transaction{{}}; retained=tx:note_writer(plum.project.current():track(1))
            retained:append({{{{start=0,gate=1,key=70}}}}); {end}
        "#
            ),
        );
        assert_eq!(result.is_err(), end.starts_with("error"));
        assert!(host.take_commits().is_none());
        assert_eq!(used(&host), 0);
    }
}

#[test]
fn strict_batches_reject_holes_unknown_fields_metatables_nonfinite_and_overflow() {
    let (host, _, _) = fixture(0, Limits::default());
    for payload in [
        "'wrong type'",
        "{[1]={start=1,gate=1,key=60},[3]={start=3,gate=1,key=60}}",
        "{{start=1,gate=1,key=60,color=5}}",
        "{start={1,2},gate={1},key={60,61}}",
        "{start={1},gate=1,key={60}}",
        "{{start=0/0,gate=1,key=60}}",
        "{{start=math.huge,gate=1,key=60}}",
        "{{start='18446744073709551615',gate=1,key=60}}",
        "{{start=0,gate=1,key=128}}",
        "{{start=0,gate=1,key=60,subchannel=99}}",
        "setmetatable({}, {__index=function() error('callback') end})",
        "{setmetatable({start=0,gate=1,key=60}, {})}",
        "{start=setmetatable({0},{}),gate={1},key={60}}",
        "{start={[1]=0, extra=7},gate={1},key={60}}",
        "{{start=string.rep('0',65),gate=1,key=60}}",
    ] {
        run(
            &host,
            &format!(
                r#"
            local tx=plum.transaction{{}}; local w=tx:note_writer(plum.project.current():track(1))
            w:append({{{{start=0,gate=1,key=60}}}})
            assert(not pcall(function() w:append({payload}) end), 'accepted bad batch')
            assert(w:info().state=='failed')
            assert(not pcall(function() tx:commit() end)); w:cancel(); tx:cancel()
        "#
            ),
        );
        assert_eq!(used(&host), 0, "{payload}");
    }
    run(
        &host,
        r#"
        local tx=plum.transaction{}; local w=tx:note_writer(plum.project.current():track(1))
        local b={start={},gate={},key={}}
        for i=1,65537 do b.start[i]=i; b.gate[i]=1; b.key[i]=60 end
        assert(not pcall(function() w:append(b) end)); w:cancel(); tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
}

#[test]
fn output_budget_counts_concurrent_finished_canceled_and_legacy_writes_once() {
    let (host, _, _) = fixture(
        0,
        Limits {
            max_output_notes: 4,
            ..Limits::default()
        },
    );
    run(
        &host,
        r#"
        local t=plum.project.current():track(1); local tx=plum.transaction{}
        local a=tx:note_writer(t); local b=tx:note_writer(t)
        a:append({{start=0,gate=1,key=60}}); a:finish()
        -- Same-target b opened before a finished, so it is stale. Discard it.
        b:cancel(); b=tx:note_writer(t)
        b:append({{start=1,gate=1,key=60}}); b:cancel()
        assert(tx:insert_notes(t,{{start=2,gate=1,key=60}})==1)
        local c=tx:note_writer(t); c:append({{start=3,gate=1,key=60}}); c:finish()
        assert(not pcall(function() tx:insert_notes(t,{{start=4,gate=1,key=60}}) end))
        local d=tx:note_writer(t)
        assert(not pcall(function() d:append({{start=4,gate=1,key=60}}) end)); d:cancel()
        assert(t:notes():count()==3); tx:commit()
        local tx2=plum.transaction{}; local e=tx2:note_writer(t)
        assert(not pcall(function() e:append({{start=5,gate=1,key=60}}) end)); tx2:cancel()
    "#,
    );
    let commits = host.take_commits().unwrap();
    assert_eq!(commits.result().note_count(), 3);
    drop(commits);
    assert_eq!(used(&host), 0);
    let (host, _, _) = fixture(
        0,
        Limits {
            max_output_notes: 1,
            ..Limits::default()
        },
    );
    run(
        &host,
        r#"
        local t=plum.project.current():track(1); local tx=plum.transaction{}
        local w=tx:note_writer(t); w:append({{start=0,gate=1,key=60}}); tx:cancel()
        tx=plum.transaction{}; w=tx:note_writer(t)
        assert(not pcall(function() w:append({{start=0,gate=1,key=60}}) end)); tx:cancel()
    "#,
    );
}

#[test]
fn stale_abandoned_wrong_transaction_and_permission_cannot_publish() {
    let (host, doc, mut history) = fixture(3, Limits::default());
    run(
        &host,
        r#"
        local t=plum.project.current():track(1); local tx=plum.transaction{}
        local w=tx:note_writer(t); w:append({{start=0,gate=1,key=60}})
        tx:insert_notes(t,{{start=1,gate=1,key=61}})
        assert(not pcall(function() w:finish() end)); w:cancel(); tx:cancel()
        tx=plum.transaction{}; w=tx:note_writer(t); w:append({{start=0,gate=1,key=60}})
        w=nil; collectgarbage('collect'); assert(not pcall(function() tx:commit() end)); tx:cancel()
        tx=plum.transaction{}; retained=tx:note_writer(t); tx:cancel()
        tx=plum.transaction{}
        assert(not pcall(function() retained:append({}) end)); retained:cancel()
        local writers={}
        for i=1,32 do writers[i]=tx:note_writer(t); writers[i]:cancel() end
        assert(not pcall(function() tx:note_writer(t) end)); tx:cancel()
    "#,
    );
    assert!(host.take_commits().is_none());
    assert_eq!(used(&host), 0);
    let restricted = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    history.observe_ids(&doc);
    bind(&restricted, &doc, &history);
    let err = restricted
        .run(
            "denied.lua",
            "plum.transaction{}:note_writer(plum.project.current():track(1))",
        )
        .unwrap_err();
    assert_eq!(err.category, Category::Capability);
}

#[test]
fn extended_keys_subtracks_subchannels_and_linked_owned_clip_local_time() {
    let (host, mut doc, mut history) = fixture(0, Limits::default());
    host.set_world(None);
    Arc::get_mut(&mut doc).unwrap().project_mut().keys = KeyRange::Extended256;
    bind(&host, &doc, &history);
    run(
        &host,
        r#"
        local p=plum.project.current(); local t=p:track(1); local tx=plum.transaction{}
        local subtrack=tx:add_subtracks(t,1)[1]
        local sub=tx:add_subchannel(subtrack,{channel=15})
        local w=tx:note_writer(subtrack)
        w:append({start={9007199254740993},gate={1},key={255},subchannel={sub:id()}})
        w:finish()
        local clip=tx:add_clip(t,10000,1000)
        local linked=tx:duplicate_clips(clip,{start=20000,linked=true})[1]
        local c=clip:content(); w=tx:note_writer(c)
        w:append({{start=17,gate=2,key=200}}); w:finish()
        assert(linked:content()==c and linked:notes():count()==1)
        assert(clip:to_song(17)==10017 and linked:to_song(17)==20017)
        local batch=c:notes():batches(1)(); assert(batch.start[1]==17 and batch.key[1]==200)
        tx:commit()
    "#,
    );
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.result().stored_note_count(), 2);
    commits.apply(&mut history, &mut doc).unwrap();
    drop(commits);
    assert_eq!(doc.stored_note_count(), 2);
    history
        .undo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.note_count(), 0);
    history
        .redo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.stored_note_count(), 2);
    host.lua.gc_collect().unwrap(); // Drop unreachable transaction-born handle tokens.
    assert_eq!(used(&host), 0);
}

#[test]
fn native_preflight_denial_poison_and_internal_copy_sort_cancellation_are_bounded() {
    let (host, doc, history) = fixture(0, Limits::default());
    // Enough for metadata and a tiny stage, insufficient for planner peak.
    let metadata = std::mem::size_of::<Entry>() * 4 + 128;
    host.lua
        .set_app_data(NativeBudget::isolated(metadata + 64 * RECORD_BYTES));
    run(
        &host,
        r#"
        local tx=plum.transaction{}; local w=tx:note_writer(plum.project.current():track(1))
        w:append({{start=0,gate=1,key=60}})
        assert(not pcall(function() w:finish() end)); assert(w:info().state=='failed')
        assert(not pcall(function() tx:commit() end)); w:cancel(); tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
    host.lua.set_app_data(NativeBudget::isolated(metadata + 10));
    run(
        &host,
        r#"
        local tx=plum.transaction{}; local w=tx:note_writer(plum.project.current():track(1))
        assert(not pcall(function() w:append({{start=0,gate=1,key=60}}) end)); w:cancel(); tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
    let budget = NativeBudget::isolated(4 * 1024 * 1024);
    let mut builder = Builder {
        target: World::new(Arc::clone(&doc), history.incarnations().snapshot()).project(),
        host: NoteHost {
            track: doc.track(0).unwrap().id(),
            content: None,
        },
        revision: 0,
        channel: 0,
        records: Vec::new(),
        allocation: None,
        sorted: false,
    };
    builder.grow(9000, &budget, &|| Ok(())).unwrap();
    builder
        .records
        .extend((0..9000).rev().map(|i| record(i, 60)));
    let base = budget.used();
    let calls = Cell::new(0);
    let check = || {
        calls.set(calls.get() + 1);
        if calls.get() == 3 {
            Err(stop(Category::Canceled, "controlled checkpoint"))
        } else {
            Ok(())
        }
    };
    assert!(builder.grow(18000, &budget, &check).is_err());
    assert_eq!(builder.records.len(), 9000);
    assert_eq!(budget.used(), base);
    calls.set(0);
    assert!(sort(&mut builder.records, &budget, &check).is_err());
    assert_eq!(budget.used(), base);
    drop(builder);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_after_native_apply_discards_whole_private_transaction_even_if_caught() {
    use std::sync::atomic::Ordering;
    let (host, doc, _) = fixture(4, Limits::default());
    let cancel = Arc::clone(&host.cancel);
    host.lua
        .globals()
        .set(
            "controlled_finish",
            host.lua
                .create_function(move |lua, ud: AnyUserData| {
                    let writer = ud.borrow::<Writer>()?;
                    let result =
                        finish_controlled(lua, &writer, || cancel.store(true, Ordering::Release));
                    cancel.store(false, Ordering::Release);
                    result
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &host,
        r#"
        local p=plum.project.current(); local t=p:track(1); local before=p:content_hash()
        local tx=plum.transaction{}; tx:insert_notes(t,{{start=900,gate=1,key=60}})
        local w=tx:note_writer(t); w:append({{start=901,gate=1,key=61}})
        assert(not pcall(controlled_finish,w)); assert(not tx:is_open())
        assert(not pcall(function() tx:commit() end))
        assert(t:notes():count()==4 and p:content_hash()==before and native_used()==0)
    "#,
    );
    assert_eq!(doc.note_count(), 4);
    assert!(host.take_commits().is_none());
    assert_eq!(used(&host), 0);
}

#[test]
fn overlapping_snapshot_stream_and_multiple_targets_keep_existing_defaults() {
    let (host, _, _) = fixture(100, Limits::default());
    run(
        &host,
        r#"
        local p=plum.project.current(); local t=p:track(1); local tx=plum.transaction{}
        local other=tx:add_subtracks(t,1)[1]
        local a=tx:note_writer(t); local b=tx:note_writer(other)
        local iter=t:notes():batches{size=7,columns={'start','gate','key','velocity','channel','subchannel'}}
        for source in iter do
            for i=1,source.count do source.key[i]=source.key[i]+1 end
            -- Project only supported columns; query metadata is not writer input.
            a:append{start=source.start,gate=source.gate,key=source.key,
                velocity=source.velocity,channel=source.channel,subchannel=source.subchannel}
        end
        b:append({{start=1,gate=1,key=72}}); a:finish(); b:finish()
        assert(t:notes():count()==200 and other:notes():count()==1 and iter()==nil)
        local empty=tx:note_writer(other)
        assert(empty:append({})==0 and empty:finish()==0)
        tx:commit()
    "#,
    );
    let commits = host.take_commits().unwrap();
    assert_eq!(commits.result().stored_note_count(), 201);
    let notes = &commits.result().track(0).unwrap().notes;
    assert_eq!(notes.iter().filter(|n| n.attrs.pitch() == 60).count(), 100);
    assert_eq!(notes.iter().filter(|n| n.attrs.pitch() == 61).count(), 100);
    drop(commits);
    host.lua.gc_collect().unwrap(); // Retained handle tokens count until Lua releases them.
    assert_eq!(used(&host), 0);
}

#[test]
fn one_million_notes_stream_with_eight_mib_vm_and_no_batch_reindexing() {
    let (host, doc, _) = fixture(
        0,
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_secs(60),
            ..Limits::default()
        },
    );
    run(
        &host,
        r#"
        local t=plum.project.current():track(1); local tx=plum.transaction{}; local w=tx:note_writer(t)
        local b={start={},gate={},key={}}
        for first=0,999999,4096 do
            local count=math.min(4096,1000000-first)
            for i=1,count do b.start[i]=first+i; b.gate[i]=2; b.key[i]=40+(first+i)%80 end
            for i=count+1,#b.start do b.start[i]=nil; b.gate[i]=nil; b.key[i]=nil end
            w:append(b)
        end
        assert(t:notes():count()==0 and w:info().notes==1000000)
        assert(w:finish()==1000000 and t:notes():count()==1000000); tx:commit()
    "#,
    );
    let commits = host.take_commits().unwrap();
    assert_eq!(commits.result().note_count(), 1_000_000);
    assert_eq!(commits.steps[0].operations, 1);
    assert_eq!(doc.note_count(), 0);
    assert!(used(&host) > 60_000_000 && used(&host) < 256 * 1024 * 1024);
    drop(commits);
    assert_eq!(used(&host), 0);
}

#[test]
fn published_streaming_example_executes_and_matches_apply() {
    let docs = include_str!("../../../../docs/LUA_API.md").replace("\r\n", "\n");
    let section = docs
        .split("### Streaming note writers (API 0.2)")
        .nth(1)
        .unwrap();
    let source = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let (host, mut doc, mut history) = fixture(0, Limits::default());
    let preview = run(&host, source);
    let mut commits = host.take_commits().unwrap();
    assert_eq!(commits.result().stored_note_count(), 4096);
    commits.apply(&mut history, &mut doc).unwrap();
    drop(commits);
    assert_eq!(
        Plain::String(format!("{:016x}", doc.export_fingerprint())),
        preview
    );
    assert_eq!(used(&host), 0);
}
