//! Explicit isolated Windows measurement cases. Never part of routine parallel tests.
use super::*;
use crate::lua::bridge::NativeBudget;
use plumspirit_engine::fixtures::{FixtureShape, generate};
use plumspirit_engine::history::{History, HistoryLimits};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
const VM: usize = 8 * 1024 * 1024;
const NATIVE: usize = 256 * 1024 * 1024;
fn memory() -> serde_json::Value {
    serde_json::json!({"working_set_bytes":crate::platform::working_set_bytes(),
        "private_commit_bytes":crate::platform::private_commit_bytes(),
        "os_process_peak_working_set_bytes":crate::platform::peak_working_set_bytes()})
}
#[derive(Default)]
struct Samples {
    lua_max: usize,
    native_max: usize,
    rows: Vec<serde_json::Value>,
}
fn host() -> (Host, NativeBudget, Arc<Mutex<Samples>>) {
    let h = Host::new(
        Limits {
            memory: VM,
            time: Duration::from_secs(60),
            ..Default::default()
        },
        [Capability::ReadProject, Capability::EditNotes],
    )
    .unwrap();
    let native = NativeBudget::isolated(NATIVE);
    h.lua.set_app_data(native.clone());
    let samples = Arc::new(Mutex::new(Samples::default()));
    let counter = samples.clone();
    let budget = native.clone();
    h.lua.globals().set("perf_sample",h.lua.create_function(move|lua,label:String|{
        let vm=lua.used_memory();let used=budget.used();let mut s=counter.lock().unwrap();
        s.lua_max=s.lua_max.max(vm);s.native_max=s.native_max.max(used);
        s.rows.push(serde_json::json!({"label":label,"lua_used_bytes":vm,"native_reserved_bytes":used,"process":memory()}));
        Ok(())
    }).unwrap()).unwrap();
    (h, native, samples)
}
fn run(h: &Host, source: &str) -> (f64, Plain) {
    let t = Instant::now();
    let value = h
        .run("manual-scaling.lua", source)
        .unwrap_or_else(|e| panic!("{e}"));
    (t.elapsed().as_secs_f64() * 1000., value)
}
fn cancel(h: &Host, source: &str) -> f64 {
    let (tx, rx) = mpsc::channel();
    let sent = std::cell::Cell::new(false);
    h.lua
        .globals()
        .set(
            "perf_ready",
            h.lua
                .create_function(move |_, ()| {
                    if !sent.replace(true) {
                        tx.send(()).unwrap();
                    }
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    let flag = h.cancel_handle();
    let stamp = Arc::new(Mutex::new(None));
    let clock = stamp.clone();
    let thread = std::thread::spawn(move || {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("The job must begin before cancellation");
        std::thread::sleep(Duration::from_millis(2));
        *clock.lock().unwrap() = Some(Instant::now());
        flag.store(true, Ordering::Release);
    });
    let error = h.run("manual-cancellation.lua", source).unwrap_err();
    let returned = Instant::now();
    thread.join().unwrap();
    assert_eq!(error.category, Category::Canceled, "{error}");
    assert!(h.take_commits().is_none());
    let started = stamp.lock().unwrap().unwrap();
    returned.saturating_duration_since(started).as_secs_f64() * 1000.
}
fn notes(count: usize) -> serde_json::Value {
    assert!([1, 100, 10_000, 100_000, 1_000_000, 10_000_000].contains(&count));
    let before = memory();
    let needed = count.saturating_mul(80).saturating_add(NATIVE);
    if crate::platform::available_memory_bytes().is_some_and(|available| available < needed as u64)
    {
        return serde_json::json!({"case":"notes","source_notes":count,"outcome":"refused_setup_headroom","estimated_setup_and_native_bytes":needed,"available_physical_bytes":crate::platform::available_memory_bytes(),"process_before":before});
    }
    let setup = Instant::now();
    // Existing deterministic native fixture, not an alternative note-store builder.
    let mut doc = generate(FixtureShape::Dense, count, 960, 89451);
    let source = doc
        .tracks()
        .iter()
        .position(|t| !t.notes.is_empty())
        .unwrap()
        + 1;
    doc.add_track().unwrap();
    let dest = doc.track_count();
    let heap = doc
        .tracks()
        .iter()
        .map(|t| t.notes.heap_bytes())
        .sum::<usize>();
    let mut doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.reset(false);
    history.observe_ids(&doc);
    let setup_ms = setup.elapsed().as_secs_f64() * 1000.;
    let (h, native, samples) = host();
    h.set_world(Some(World::new(
        doc.clone(),
        history.incarnations().snapshot(),
    )));
    h.lua.globals().set("source_index", source).unwrap();
    h.lua.globals().set("dest_index", dest).unwrap();
    h.lua.globals().set("expected", count).unwrap();
    let mut scans = Vec::new();
    for _ in 0..3 {
        let (ms, result) = run(
            &h,
            r#"local q=plum.project.current():track(source_index):notes();local n,batches=0,0
          perf_sample('scan-start')
          for b in q:batches{size=4096,columns={'start','gate','key','velocity','channel','subchannel'}}do
            n=n+#b.start;batches=batches+1
            if batches%16==0 then perf_sample('scan-64k')end
          end
          assert(n==expected);perf_sample('scan-end');return n"#,
        );
        assert_eq!(result, Plain::Integer(count as i64));
        scans.push(ms);
        h.lua.gc_collect().unwrap();
        assert_eq!(native.used(), 0);
    }
    let mut indexed = Vec::new();
    let mut hits = None;
    for _ in 0..3 {
        let (ms, value) = run(
            &h,
            r#"local t=plum.project.current():track(source_index);local total=0
          for i=1,1000 do total=total+t:notes{start_tick=10000,end_tick=10120}:count()end
          perf_sample('indexed-end');return total"#,
        );
        if let Some(ref previous) = hits {
            assert_eq!(&value, previous);
        } else {
            hits = Some(value);
        }
        indexed.push(ms);
        h.lua.gc_collect().unwrap();
        assert_eq!(native.used(), 0);
    }
    let output = count.min(1_000_000);
    h.lua.globals().set("output_count", output).unwrap();
    let stage = Instant::now();
    let staged = h.run(
        "manual-writer.lua",
        r#"local p=plum.project.current();local tx=plum.transaction{label='Measured stream'}
      local w=tx:note_writer(p:track(dest_index));local b={start={},gate={},key={}}
      for first=0,output_count-1,4096 do local count=math.min(4096,output_count-first)
        for i=1,count do b.start[i]=first+i;b.gate[i]=2;b.key[i]=40+(first+i)%80 end
        for i=count+1,#b.start do b.start[i]=nil;b.gate[i]=nil;b.key[i]=nil end
        w:append(b)
      end
      assert(w:finish()==output_count);perf_sample('writer-private-staged');tx:commit()"#,
    );
    let stage_ms = stage.elapsed().as_secs_f64() * 1000.;
    let transaction = match staged {
        Ok(_) => {
            let mut commits = h.take_commits().unwrap();
            assert_eq!(doc.note_count(), count);
            assert_eq!(commits.result().note_count(), count + output);
            assert_eq!(commits.steps.len(), 1);
            let native_kept = native.used();
            h.set_world(None);
            let start = Instant::now();
            commits.apply(&mut history, &mut doc).unwrap();
            let apply_ms = start.elapsed().as_secs_f64() * 1000.;
            drop(commits);
            let start = Instant::now();
            history
                .undo(Arc::get_mut(&mut doc).unwrap())
                .unwrap()
                .unwrap();
            let undo_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(doc.note_count(), count);
            let start = Instant::now();
            history
                .redo(Arc::get_mut(&mut doc).unwrap())
                .unwrap()
                .unwrap();
            let redo_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(doc.note_count(), count + output);
            serde_json::json!({"outcome":"applied_undo_redo","output_notes":output,"stage_ms":stage_ms,"apply_ms":apply_ms,"undo_ms":undo_ms,"redo_ms":redo_ms,"native_kept_bytes":native_kept})
        }
        Err(e) => {
            assert!(h.take_commits().is_none());
            assert_eq!(doc.note_count(), count);
            serde_json::json!({"outcome":"refused","category":e.category.name(),"message":e.message,"stage_ms":stage_ms,"output_notes":output})
        }
    };
    history.observe_ids(&doc);
    h.set_world(Some(World::new(
        doc.clone(),
        history.incarnations().snapshot(),
    )));
    let cancel_ms = cancel(
        &h,
        r#"local t=plum.project.current():track(source_index);perf_ready()
      while true do for b in t:notes():batches{size=4096,columns={'start','key','velocity'}}do local n=#b.start end end"#,
    );
    h.set_world(None);
    h.lua.gc_collect().unwrap();
    assert_eq!(native.used(), 0);
    let s = samples.lock().unwrap();
    serde_json::json!({"case":"notes","source_notes":count,"fixture":"existing Dense/960PPQ/seed89451, plus empty output Track","fixture_ms":setup_ms,"note_store_heap_bytes":heap,
        "scan_six_columns_ms":scans,"indexed_1000_queries_ms":indexed,"transaction":transaction,"cancel_request_to_return_ms":cancel_ms,
        "lua_enforced_limit_bytes":VM,"native_limit_bytes":NATIVE,"sampled_lua_max_bytes":s.lua_max,"sampled_native_max_bytes":s.native_max,
        "native_after_gc_bytes":native.used(),"process_before":before,"process_after":memory(),"samples":s.rows})
}
fn images(width: u32, height: u32) -> serde_json::Value {
    assert!([(64, 64), (1024, 1024), (4096, 4096), (16384, 1024)].contains(&(width, height)));
    let before = memory();
    let (h, native, samples) = host();
    h.lua.globals().set("width", width).unwrap();
    h.lua.globals().set("height", height).unwrap();
    let source = r#"local spans={row={},first={},last={}};for i=1,height do spans.row[i]=i-1;spans.first[i]=0;spans.last[i]=width end
      perf_sample('image-start');local mask=plum.images.mask_from_spans(spans,{width=width,height=height});perf_sample('A8')
      local image=mask:colorize{200,90,20,255};mask:close();perf_sample('RGBA')
      local composite=plum.images.composite({{image=image}},{width=width,height=height});perf_sample('composite')
      local small=composite:resize(64,64,'box');image:close();composite:close();perf_sample('resized')
      local plan=small:note_plan{palette={{200,90,20}},keys=128,first_key=24,tick_numerator=30}
      small:close();local rows=plan:next(4096);assert(#rows.start==64);plan:close();perf_sample('plan-end');return #rows.start"#;
    let mut times = Vec::new();
    for _ in 0..3 {
        let (ms, n) = run(&h, source);
        assert_eq!(n, Plain::Integer(64));
        times.push(ms);
        h.lua.gc_collect().unwrap();
        assert_eq!(native.used(), 0);
    }
    let cancel_ms = cancel(
        &h,
        r#"local spans={row={},first={},last={}};for i=1,height do spans.row[i]=i-1;spans.first[i]=0;spans.last[i]=width end
      local mask=plum.images.mask_from_spans(spans,{width=width,height=height});perf_ready()
      local image=mask:colorize{200,90,20,255};image:close()
      while true do plum.yield()end"#,
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(native.used(), 0);
    let s = samples.lock().unwrap();
    serde_json::json!({"case":"images","width":width,"height":height,"pixels":u64::from(width)*u64::from(height),"fixture":"fully covered authored row spans; A8/colorize/composite/box64/note plan; not decoder benchmark",
        "pipeline_ms":times,"cancel_request_to_return_ms":cancel_ms,"cancel_scenario":"one native colorize, then cooperative yield if already complete; cumulative handle cap remains unchanged","lua_enforced_limit_bytes":VM,"native_limit_bytes":NATIVE,
        "sampled_lua_max_bytes":s.lua_max,"sampled_native_max_bytes":s.native_max,"native_after_gc_bytes":native.used(),"process_before":before,"process_after":memory(),"samples":s.rows})
}
#[test]
#[ignore = "Manual Windows scaling: set PLUMSPIRIT_PERF_CASE and run this exact test in a fresh process"]
fn isolated_native_api02_scaling_case() {
    let case =
        std::env::var("PLUMSPIRIT_PERF_CASE").expect("Set an explicit notes:N or images:WxH case");
    let (family, number) = case.split_once(':').unwrap();
    let result = match family {
        "notes" => notes(number.parse().unwrap()),
        "images" => {
            let (w, h) = number.split_once('x').unwrap();
            images(w.parse().unwrap(), h.parse().unwrap())
        }
        _ => panic!("Unknown measurement case"),
    };
    println!(
        "PLUMSPIRIT_API02_MEASUREMENT {}",
        serde_json::json!({"schema":1,"case":case,"profile":if cfg!(debug_assertions){"debug"}else{"optimized"},"debug_assertions":cfg!(debug_assertions),"execution":"isolated native Windows test process; no GUI/audio claim; CRT/toolchain recorded separately","result":result})
    );
}
