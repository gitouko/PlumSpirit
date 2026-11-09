use super::*;
use crate::lua::{
    Host, Limits, Plain, Scripts,
    worker::{ScriptJob, ScriptWorker},
};
use std::time::{Duration, Instant};
fn host(memory: usize, native: usize) -> (Host, NativeBudget) {
    let h = Host::new(
        Limits {
            memory,
            time: Duration::from_secs(15),
            ..Default::default()
        },
        [],
    )
    .unwrap();
    let budget = NativeBudget::isolated(native);
    h.lua.set_app_data(budget.clone());
    (h, budget)
}
const SOURCE: &str = "local source=plum.images.from_rgba(4,2,string.char(255,0,0,255,255,0,0,255,0,0,0,0,0,0,255,255,0,0,255,255,0,0,255,255,0,0,255,255,0,255,0,255));";
const OPTIONS: &str = "{palette={{255,0,0},{0,0,255},{0,255,0}},ignored={false,false,true},first_key=60,start_tick='9007199254740993',tick_numerator=3,tick_denominator=2,velocity=99}";
#[test]
fn exact_rational_large_ticks_counts_palette_and_source_close_are_independent() {
    let (h, budget) = host(8 * 1024 * 1024, 16 * 1024 * 1024);
    h.run("exact.lua",&format!("{SOURCE}held=source:note_plan({OPTIONS});source:close();local info=held:info();assert(info.note_count==3 and info.end_tick==9007199254740999 and info.index_bytes==32 and info.order=='line_time' and info.palette_base==1);local a=held:next(1);assert(a.start[1]==9007199254740993 and a.gate[1]==3 and a.key[1]==61 and a.velocity[1]==99 and a.palette[1]==1);local b=held:next(1);assert(b.start[1]==9007199254740997 and b.gate[1]==2 and b.palette[1]==2);local c=held:next(1);assert(c.start[1]==9007199254740993 and c.gate[1]==4 and c.key[1]==60);assert(held:info().done and held:info().remaining==0 and #held:next().start==0);held:reset();assert(held:info().read==0 and #held:next().start==3);local p=held:palette();p[1][1]=0;assert(held:palette()[1][1]==255)")).unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(
        budget.used(),
        METADATA_BYTES,
        "expired global keeps metadata only"
    );
    h.run("expired.lua","assert(not pcall(function()held:info()end));assert(not pcall(function()held:next()end));held=nil;collectgarbage('collect')").unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn packed_pages_match_native_sorted_notes_for_both_key_modes_orientations_and_split_policies() {
    for keys in [128, 256] {
        for orientation in [im::Orientation::PianoRoll, im::Orientation::Upstream] {
            for pixel in [false, true] {
                for split in [im::SplitFrom::ImageStart, im::SplitFrom::NoteStart] {
                    let (h, _) = host(8 * 1024 * 1024, 32 * 1024 * 1024);
                    let (w, height) = (17u32, 5u32);
                    let first = if keys == 128 { 60 } else { 180 };
                    let pixels = (0..w * height)
                        .map(|i| {
                            if i % 11 == 0 {
                                [0, 0, 0, 0]
                            } else if i % 7 < 3 {
                                [255, 0, 0, 255]
                            } else {
                                [0, 0, 255, 255]
                            }
                        })
                        .collect::<Vec<_>>();
                    let bytes = pixels
                        .iter()
                        .flat_map(|p| p.iter().copied())
                        .collect::<Vec<_>>();
                    h.lua
                        .globals()
                        .set("pixels", h.lua.create_string(&bytes).unwrap())
                        .unwrap();
                    let orient = if orientation == im::Orientation::PianoRoll {
                        "piano_roll"
                    } else {
                        "upstream"
                    };
                    let cut = if split == im::SplitFrom::ImageStart {
                        "image"
                    } else {
                        "note"
                    };
                    let source = format!(
                        "local source=plum.images.from_rgba(17,5,pixels);local p=source:note_plan{{palette={{{{255,0,0}},{{0,0,255}}}},keys={keys},first_key={first},orientation='{orient}',pixel_notes={pixel},max_length=4,split='{cut}',tick_numerator=7,velocity=98}};local all={{}};for _,page in ipairs({{1,7,4096}})do p:reset();local rows={{}};while true do local b=p:next(page);if #b.start==0 then break end;for i=1,#b.start do rows[#rows+1]=string.format('%d,%d,%d,%d,%d',b.start[i],b.gate[i],b.key[i],b.velocity[i],b.palette[i]-1)end end;all[#all+1]=table.concat(rows,';')end;assert(all[1]==all[2] and all[2]==all[3]);return all[1]"
                    );
                    let Plain::String(output) = h.run("native-parity.lua", &source).unwrap() else {
                        panic!("packed text")
                    };
                    let mut actual = output
                        .split(';')
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            let n = s
                                .split(',')
                                .map(|n| n.parse::<u64>().unwrap())
                                .collect::<Vec<_>>();
                            plumspirit_engine::transform::paste::ColorNote {
                                start: n[0],
                                dur: n[1],
                                pitch: n[2] as u8,
                                vel: n[3] as u8,
                                color: n[4] as u32,
                            }
                        })
                        .collect::<Vec<_>>();
                    actual.sort_unstable();
                    let lines = if orientation == im::Orientation::PianoRoll {
                        height
                    } else {
                        w
                    };
                    let settings = im::Settings {
                        keys: if keys == 128 {
                            plumspirit_engine::model::project::KeyRange::Standard
                        } else {
                            plumspirit_engine::model::project::KeyRange::Extended256
                        },
                        first_key: first,
                        last_key: first + lines as u8 - 1,
                        orientation,
                        palette: vec![[255, 0, 0], [0, 0, 255]],
                        pixel_notes: pixel,
                        max_len: 4,
                        split,
                        ticks_per_pixel: 7,
                        velocity: 98,
                        ..Default::default()
                    };
                    let expected =
                        im::convert(&Rgba::new(w, height, pixels).unwrap(), &settings, 10000)
                            .unwrap();
                    assert_eq!(actual, expected.notes);
                }
            }
        }
    }
}
#[test]
fn strict_options_domains_and_packed_page_validation_do_not_advance_on_failure() {
    let bad_options = [
        "{}",
        "{palette={}}",
        "{palette={{1,2,3,4}}}",
        "{palette={{1,2,256}}}",
        "{palette={{1,2,3}},ignored={1}}",
        "{palette={{1,2,3}},ignored={}}",
        "{palette={{1,2,3}},keys=129}",
        "{palette={{1,2,3}},first_key=128}",
        "{palette={{1,2,3}},velocity=0}",
        "{palette={{1,2,3}},tick_denominator=0}",
        "{palette={{1,2,3}},tick_numerator=1,tick_denominator=2}",
        "{palette={{1,2,3}},start_tick='9223372036854775807'}",
        "{palette={{1,2,3}},orientation='bad'}",
        "{palette={{1,2,3}},split='bad'}",
        "{palette={{1,2,3}},pixel_notes=1}",
        "{palette={{1,2,3}},unknown=true}",
        "setmetatable({palette={{1,2,3}}},{})",
        "{palette=setmetatable({{1,2,3}},{})}",
        "{palette={setmetatable({1,2,3},{})}}",
        "{palette={[1]={1,2,3},[3]={3,2,1}}}",
        "{palette={{1,2,3}},tick_numerator=0/0}",
    ];
    for options in bad_options {
        let (h, budget) = host(8 * 1024 * 1024, 1024 * 1024);
        let e=h.run("invalid.lua",&format!("local s=plum.images.from_rgba(1,1,string.char(1,2,3,255));return s:note_plan({options})")).unwrap_err();
        assert_eq!(e.category, Category::Runtime, "{options}: {e:?}");
        h.lua.gc_collect().unwrap();
        assert_eq!(budget.used(), 0);
    }
    let (h, _) = host(8 * 1024 * 1024, 1024 * 1024);
    h.run("bad-pages.lua",&format!("{SOURCE}local p=source:note_plan({OPTIONS});for _,n in ipairs({{0,4097,1.5,math.huge,'bad',{{}}}})do assert(not pcall(function()p:next(n)end));assert(p:info().read==0)end;assert(#p:next(1).start==1);assert(p:close() and not p:close());assert(not pcall(function()p:reset()end))")).unwrap();
}
#[test]
fn attempt_pixel_work_and_native_reservations_are_bounded_and_closed_metadata_is_charged() {
    let (h, budget) = host(16 * 1024 * 1024, 16 * 1024 * 1024);
    h.run("attempts.lua","local s=plum.images.from_rgba(1,1,string.char(1,2,3,255));held={};for i=1,16 do held[i]=s:note_plan{palette={{1,2,3}}};assert(held[i]:close())end;assert(not pcall(function()s:note_plan{palette={{1,2,3}}}end));collectgarbage('collect')").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 16 * METADATA_BYTES);
    h.run("gc.lua", "held=nil;collectgarbage('collect')")
        .unwrap();
    assert_eq!(budget.used(), 0);
    let (h, budget) = host(8 * 1024 * 1024, METADATA_BYTES - 1);
    assert_eq!(h.run("preflight.lua","local s=plum.images.from_rgba(1,1,string.char(1,2,3,255));s:note_plan{palette={{1,2,3}}}").unwrap_err().category,Category::Budget);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    let (h, _) = host(16 * 1024 * 1024, 64 * 1024 * 1024);
    h.run("work.lua","local s=plum.images.from_rgba(4096,256,string.rep(string.char(1,2,3,255),4096*256));local colors={};for i=1,257 do colors[i]={i%256,0,0}end;assert(not pcall(function()s:note_plan{palette=colors,keys=256}end));local p=s:note_plan{palette={{1,2,3}},keys=256};assert(p:info().note_count==256)").unwrap();
}
#[test]
fn million_pixel_notes_stream_under_eight_mib_lua_with_exact_estimate() {
    let (h, budget) = host(8 * 1024 * 1024, 64 * 1024 * 1024);
    native_fixture(&h, 16384, 64);
    h.run("million.lua","local s=fixture_image();local p=s:note_plan{palette={{1,2,3}},pixel_notes=true,tick_numerator=3,tick_denominator=2,first_key=64};s:close();assert(p:info().note_count==1048576);local count=0;while true do local b=p:next();if #b.start==0 then break end;count=count+#b.start end;assert(count==1048576 and p:info().done);p:close()").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn real_worker_cancellation_inside_native_palette_indexing_releases_data_and_worker_recovers() {
    let mut worker = ScriptWorker::spawn(
        Limits {
            memory: 16 * 1024 * 1024,
            time: Duration::from_secs(15),
            ..Default::default()
        },
        vec![],
        &Scripts::default(),
    )
    .unwrap();
    worker.submit(ScriptJob{file:"cancel-index.lua".into(),source:"local s=plum.images.from_rgba(16384,16,string.rep(string.char(1,2,3,255),16384*16));local colors={};for i=1,1024 do colors[i]={i%256,math.floor(i/256)%256,3}end;plum.progress(.25,'native indexing');local p=s:note_plan{palette=colors,keys=256};return p:info().note_count".into(),world:None,time_limit:None}).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while worker.progress().fraction != Some(0.25) {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(10));
    worker.cancel_job();
    let result = worker.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(result.result.unwrap_err().category, Category::Canceled);
    worker.submit(ScriptJob{file:"recovery.lua".into(),source:"local s=plum.images.from_rgba(1,1,string.char(1,2,3,255));local p=s:note_plan{palette={{1,2,3}}};return p:info().note_count".into(),world:None,time_limit:None}).unwrap();
    assert_eq!(
        worker.wait(Duration::from_secs(5)).unwrap().result.unwrap(),
        Plain::Integer(1)
    );
}

fn native_fixture(h: &Host, width: u32, height: u32) {
    // Native decoded-image equivalent fixture: pixel storage is charged through the
    // actual image registry. Avoid string.rep's two4MiB temporary Lua allocations;
    // this tests streaming under8MiB, not Lua's construction of a giant binary string.
    h.lua
        .globals()
        .set(
            "fixture_image",
            h.lua
                .create_function(move |lua, ()| {
                    let (job, budget) = context(lua)?;
                    let count = size(width, height)?;
                    let allocation = budget.reserve(count * 4).map_err(|e| e.lua())?;
                    store(
                        lua,
                        job,
                        Rgba::new(width, height, vec![[1, 2, 3, 255]; count]).map_err(bad)?,
                        "rgba",
                        allocation,
                    )
                })
                .unwrap(),
        )
        .unwrap();
}
#[test]
fn cumulative_pixel_limit_does_not_reset_when_plans_close() {
    let (h, budget) = host(8 * 1024 * 1024, 64 * 1024 * 1024);
    native_fixture(&h, 16384, 128);
    h.run("pixel-quota.lua","local s=fixture_image();for i=1,8 do local p=s:note_plan{palette={{1,2,3}}};assert(p:info().note_count==128);p:close()end;assert(not pcall(function()s:note_plan{palette={{1,2,3}}}end))").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn native_and_lua_page_allocation_failures_do_not_commit_read_progress() {
    let (h, budget) = host(8 * 1024 * 1024, 16 * 1024 * 1024);
    native_fixture(&h, 4096, 2);
    let held = Rc::new(RefCell::new(None::<Reservation>));
    let sink = Rc::clone(&held);
    let pool = budget.clone();
    h.lua
        .globals()
        .set(
            "hold_budget",
            h.lua
                .create_function(move |_, ()| {
                    *sink.borrow_mut() =
                        Some(pool.reserve(16 * 1024 * 1024 - pool.used() - 16).unwrap());
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    let sink = Rc::clone(&held);
    h.lua
        .globals()
        .set(
            "release_budget",
            h.lua
                .create_function(move |_, ()| {
                    sink.borrow_mut().take();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "limit_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.gc_stop();
                    lua.set_memory_limit(lua.used_memory() + 4096)?;
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "restore_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.set_memory_limit(8 * 1024 * 1024)?;
                    lua.gc_restart();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.run("page-failure.lua","local s=fixture_image();local p=s:note_plan{palette={{1,2,3}},pixel_notes=true};hold_budget();assert(not pcall(function()p:next(4096)end));release_budget();assert(p:info().read==0);limit_vm();local ok=pcall(function()p:next(4096)end);restore_vm();assert(not ok);assert(p:info().read==0);assert(p:next(1).start[1]==0 and p:info().read==1)").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn failed_job_eagerly_expires_plan_data_and_next_job_gets_fresh_creation_quota() {
    let (h, budget) = host(8 * 1024 * 1024, 16 * 1024 * 1024);
    assert!(
        h.run(
            "fail.lua",
            &format!("{SOURCE}failed=source:note_plan({OPTIONS});error('after plan')")
        )
        .is_err()
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), METADATA_BYTES);
    h.run("recover.lua","assert(not pcall(function()failed:info()end));failed=nil;collectgarbage('collect');local s=plum.images.from_rgba(1,1,string.char(1,2,3,255));for i=1,16 do local p=s:note_plan{palette={{1,2,3}}};p:close()end").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}

fn documentation_examples() -> Vec<&'static str> {
    let section = include_str!("../../../../docs/LUA_API.md")
        .split("## Bounded image-note plans (API 0.2)")
        .nth(1)
        .unwrap()
        .split("## The sandbox")
        .next()
        .unwrap();
    section
        .split("```lua\n")
        .skip(1)
        .map(|s| s.split("```").next().unwrap())
        .collect()
}
#[test]
fn published_pure_plan_example_executes_verbatim() {
    let (h, budget) = host(8 * 1024 * 1024, 16 * 1024 * 1024);
    h.run("published-plan.lua", documentation_examples()[0])
        .unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn published_writer_composition_is_deterministic_preview_apply_one_undo_and_redo_in_both_key_modes()
{
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    let source = documentation_examples()[1];
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut d = PlumDocument::new_project("Image note plan".into(), Bpm::DEFAULT, 960);
        d.project_mut().keys = keys;
        let mut doc = Arc::new(d);
        let mut history = History::new(HistoryLimits::default());
        history.observe_ids(&doc);
        let encode = |doc: &PlumDocument| {
            plumspirit_engine::midi::export_to_bytes_with(
                doc,
                &plumspirit_engine::midi::plan_export_with(
                    doc,
                    plumspirit_engine::midi::ExportOptions::for_player(keys),
                ),
            )
            .unwrap()
        };
        let original = encode(&doc);
        let h = Host::new(
            Limits::default(),
            [Capability::ReadProject, Capability::EditNotes],
        )
        .unwrap();
        let bind = |h: &Host, doc: &Arc<PlumDocument>, history: &History| {
            h.set_world(Some(World::new(
                Arc::clone(doc),
                history.incarnations().snapshot(),
            )))
        };
        bind(&h, &doc, &history);
        h.run("published-writer-preview.lua", source).unwrap();
        let preview = h.take_commits().unwrap();
        let expected = preview.result().export_fingerprint();
        assert_eq!(preview.result().note_count(), 2);
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        bind(&h, &doc, &history);
        h.run("published-writer-apply.lua", source).unwrap();
        let mut commits = h.take_commits().unwrap();
        assert_eq!(commits.result().export_fingerprint(), expected);
        assert_eq!(commits.steps.len(), 1);
        assert_eq!(commits.steps[0].operations, 1);
        h.set_world(None);
        commits.apply(&mut history, &mut doc).unwrap();
        drop(commits);
        assert_eq!(doc.note_count(), 2);
        if keys == KeyRange::Extended256 {
            assert!(
                plumspirit_engine::midi::export_to_bytes(&doc).is_err(),
                "standard MIDI refusal is preserved"
            );
        }
        let applied = encode(&doc);
        assert_ne!(applied, original);
        history
            .undo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(encode(&doc), original);
        assert!(!history.can_undo());
        history
            .redo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(encode(&doc), applied);
    }
}

fn writer_fixture(
    keys: u16,
    limits: Limits,
) -> (
    Host,
    NativeBudget,
    Arc<plumspirit_engine::model::PlumDocument>,
    plumspirit_engine::history::History,
) {
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    let mut doc = PlumDocument::new_project("Native image writer".into(), Bpm::DEFAULT, 960);
    doc.project_mut().keys = if keys == 256 {
        KeyRange::Extended256
    } else {
        KeyRange::Standard
    };
    doc.add_track().unwrap();
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.reset(false);
    history.observe_ids(&doc);
    let h = Host::new(
        limits,
        [
            Capability::ReadProject,
            Capability::EditNotes,
            Capability::EditTracks,
        ],
    )
    .unwrap();
    let budget = NativeBudget::isolated(256 * 1024 * 1024);
    h.lua.set_app_data(budget.clone());
    h.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    (h, budget, doc, history)
}
const SMALL_PLAN: &str = "local source=plum.images.from_rgba(3,2,string.char(255,0,0,255,0,0,255,255,255,0,0,255,0,0,255,255,0,0,0,0,255,0,0,255));local plan=source:note_plan{palette={{255,0,0},{0,0,255}},first_key=60,tick_numerator=3,tick_denominator=2};source:close();";
#[test]
fn native_writer_mapped_subchannels_remaining_data_preview_apply_undo_redo_both_modes() {
    use crate::lua::World;
    for keys in [128, 256] {
        let (h, budget, mut doc, mut history) = writer_fixture(keys, Limits::default());
        let source = format!(
            "{SMALL_PLAN}local t=plum.project.current():track(1);local tx=plum.transaction{{}};local red=tx:add_subchannel(t,{{channel=3}});local blue=tx:add_subchannel(t,{{channel=15}});assert(#plan:next(1).start==1);local w=tx:note_writer(t);assert(w:append_plan(plan,{{red,blue}})==4);assert(plan:info().done and w:info().notes==4 and t:notes():count()==0);assert(w:append_plan(plan,{{red,blue}})==0);assert(w:finish()==4);tx:commit()"
        );
        h.run("mapped-preview.lua", &source).unwrap();
        let preview = h.take_commits().unwrap();
        let records = preview
            .result()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect::<Vec<_>>();
        assert_eq!(
            records
                .iter()
                .map(|r| (
                    r.start_tick,
                    r.duration_ticks,
                    r.attrs.pitch(),
                    r.sub.0,
                    r.attrs.channel()
                ))
                .collect::<Vec<_>>(),
            vec![
                (0, 1, 60, 2, 15),
                (1, 2, 61, 2, 15),
                (3, 1, 61, 1, 3),
                (3, 1, 60, 1, 3)
            ]
        );
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        h.set_world(Some(World::new(
            Arc::clone(&doc),
            history.incarnations().snapshot(),
        )));
        h.run("mapped-apply.lua", &source).unwrap();
        let mut commits = h.take_commits().unwrap();
        assert_eq!(commits.result().export_fingerprint(), expected);
        assert_eq!(commits.steps.len(), 1);
        h.set_world(None);
        commits.apply(&mut history, &mut doc).unwrap();
        drop(commits);
        assert_eq!(doc.note_count(), 4);
        history
            .undo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.note_count(), 0);
        assert_eq!(doc.track(0).unwrap().subchannels().len(), 1);
        assert!(!history.can_undo());
        history
            .redo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
        h.lua.gc_collect().unwrap();
        assert_eq!(budget.used(), 0);
    }
}
#[test]
fn native_writer_skip_only_and_partial_maps_charge_actual_output_and_consume_remaining_input() {
    let (h, _, doc, _) = writer_fixture(
        128,
        Limits {
            max_output_notes: 3,
            ..Default::default()
        },
    );
    h.run("skip-colors.lua",&format!("{SMALL_PLAN}local t=plum.project.current():track(1);local main=t:subchannels()[1];local tx=plum.transaction{{}};local w=tx:note_writer(t);assert(w:append_plan(plan,{{false,false}})==0 and plan:info().done and w:info().notes==0);plan:reset();assert(w:append_plan(plan,{{main,false}})==3);assert(w:finish()==3);plan:reset();local other=tx:note_writer(t);assert(other:append_plan(plan,{{false,false}})==0);plan:reset();assert(not pcall(function()other:append_plan(plan)end));assert(plan:info().read==0 and other:info().state=='failed');other:cancel();tx:commit()")).unwrap();
    let commits = h.take_commits().unwrap();
    assert_eq!(commits.result().note_count(), 3);
    assert_eq!(doc.note_count(), 0);
}
#[test]
fn native_writer_strict_mapping_failures_poison_private_stage_without_advancing_plan() {
    let invalid = [
        "true",
        "{}",
        "{false}",
        "{false,false,false}",
        "{[1]=false,[3]=false}",
        "{false,false,x=1}",
        "{true,false}",
        "{0,false}",
        "{t,false}",
        "{other:subchannels()[1],false}",
        "setmetatable({false,false},{})",
    ];
    for map in invalid {
        let (h, budget, doc, _) = writer_fixture(128, Limits::default());
        h.run("bad-map.lua",&format!("{SMALL_PLAN}local t=plum.project.current():track(1);local tx=plum.transaction{{}};local other=tx:add_subtracks(t,1)[1];local w=tx:note_writer(t);w:append({{{{start=7,gate=1,key=70}}}});assert(not pcall(function()w:append_plan(plan,{map})end));assert(w:info().state=='failed' and w:info().staged_bytes==0 and plan:info().read==0);assert(not pcall(function()tx:commit()end));w:cancel();tx:cancel()")).unwrap();
        assert_eq!(doc.note_count(), 0);
        assert!(h.take_commits().is_none());
        h.lua.gc_collect().unwrap();
        assert_eq!(budget.used(), 0, "{map}");
    }
}
#[test]
fn native_writer_subtrack_owned_linked_clip_local_ticks_and_high_key_validation() {
    for keys in [128, 256] {
        let (h, _, doc, _) = writer_fixture(keys, Limits::default());
        let key = if keys == 256 { 200 } else { 60 };
        h.run("image-clip.lua",&format!("local t=plum.project.current():track(1);local tx=plum.transaction{{}};local child=tx:add_subtracks(t,1)[1];local sub=tx:add_subchannel(child,{{channel=14}});local image=plum.images.from_rgba(1,1,string.char(1,2,3,255));local plan=image:note_plan{{palette={{{{1,2,3}}}},keys={keys},first_key={key},start_tick=17}};local w=tx:note_writer(child);assert(w:append_plan(plan,{{sub}})==1);w:finish();plan:reset();local c=tx:add_clip(child,10000,1000);local linked=tx:duplicate_clips(c,{{start=20000,linked=true}})[1];local writer=tx:note_writer(c:content());assert(writer:append_plan(plan,{{sub}})==1);writer:finish();assert(c:to_song(17)==10017 and linked:to_song(17)==20017 and linked:notes():count()==1);local b=linked:notes():batches(1)();assert(b.start[1]==17 and b.key[1]=={key} and b.channel[1]==14 and b.subchannel[1]==sub:id());tx:commit()")).unwrap();
        assert_eq!(h.take_commits().unwrap().result().stored_note_count(), 2);
        assert_eq!(doc.stored_note_count(), 0);
    }
    let (h, _, _, _) = writer_fixture(128, Limits::default());
    h.run("invalid-high-key.lua","local t=plum.project.current():track(1);local image=plum.images.from_rgba(1,1,string.char(1,2,3,255));local p=image:note_plan{palette={{1,2,3}},keys=256,first_key=200};local tx=plum.transaction{};local w=tx:note_writer(t);assert(not pcall(function()w:append_plan(p)end));assert(p:info().read==0 and w:info().state=='failed' and t:notes():count()==0);tx:cancel()").unwrap();
}
#[test]
fn native_writer_stale_target_and_removed_subchannel_leave_plan_unread() {
    for change in [
        "tx:insert_notes(t,{{start=20,gate=1,key=70}})",
        "tx:delete_subchannel(sub)",
    ] {
        let (h, _, doc, _) = writer_fixture(128, Limits::default());
        h.run("stale-image-target.lua",&format!("{SMALL_PLAN}local t=plum.project.current():track(1);local tx=plum.transaction{{}};local sub=tx:add_subchannel(t);local w=tx:note_writer(t);{change};assert(not pcall(function()w:append_plan(plan,{{sub,sub}})end));assert(plan:info().read==0 and w:info().state=='failed');tx:cancel()")).unwrap();
        assert!(h.take_commits().is_none());
        assert_eq!(doc.note_count(), 0);
    }
}
#[test]
fn million_native_image_notes_finish_with_eight_mib_lua_and_one_insertion() {
    let (h, budget, doc, _) = writer_fixture(
        128,
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_secs(60),
            ..Default::default()
        },
    );
    native_fixture(&h, 16384, 64);
    h.run("million-native-writer.lua","local s=fixture_image();local p=s:note_plan{palette={{1,2,3}},pixel_notes=true,tick_numerator=3,tick_denominator=2,first_key=64};s:close();local t=plum.project.current():track(1);local tx=plum.transaction{};local w=tx:note_writer(t);assert(w:append_plan(p)==1048576 and p:info().done and t:notes():count()==0);p:close();assert(w:finish()==1048576);tx:commit()").unwrap();
    let commits = h.take_commits().unwrap();
    assert_eq!(commits.result().note_count(), 1048576);
    assert_eq!(commits.steps[0].operations, 1);
    assert_eq!(doc.note_count(), 0);
    drop(commits);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}

#[test]
fn native_writer_allocation_refusal_releases_stage_and_does_not_consume_plan() {
    let (h, budget, doc, _) = writer_fixture(128, Limits::default());
    let held = Rc::new(RefCell::new(None::<Reservation>));
    let sink = held.clone();
    let pool = budget.clone();
    h.lua
        .globals()
        .set(
            "hold_native",
            h.lua
                .create_function(move |_, ()| {
                    *sink.borrow_mut() = Some(
                        pool.reserve(256 * 1024 * 1024 - pool.used() - 16)
                            .map_err(|e| e.lua())?,
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    let sink = held.clone();
    h.lua
        .globals()
        .set(
            "release_native",
            h.lua
                .create_function(move |_, ()| {
                    sink.borrow_mut().take();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.run("native-append-budget.lua",&format!("{SMALL_PLAN}local t=plum.project.current():track(1);local tx=plum.transaction{{}};local w=tx:note_writer(t);hold_native();local ok=pcall(function()w:append_plan(plan)end);release_native();assert(not ok and plan:info().read==0 and w:info().state=='failed' and w:info().staged_bytes==0);w:cancel();local next_writer=tx:note_writer(t);assert(next_writer:append_plan(plan)==5);assert(next_writer:finish()==5);tx:cancel()")).unwrap();
    assert!(h.take_commits().is_none());
    assert_eq!(doc.note_count(), 0);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn native_reader_mid_scan_cancellation_returns_no_partial_progress() {
    let (h, budget) = host(8 * 1024 * 1024, 32 * 1024 * 1024);
    native_fixture(&h, 8192, 2);
    h.lua
        .globals()
        .set(
            "interrupt_native",
            h.lua
                .create_function(|lua, plan: AnyUserData| {
                    with_native(lua, &plan, |reader| {
                        let mut count = 0;
                        reader.visit(lua, |_| {
                            count += 1;
                            if count == 4097 {
                                Err(stop(
                                    Category::Canceled,
                                    "Controlled native consumer cancellation.",
                                ))
                            } else {
                                Ok(())
                            }
                        })
                    })
                })
                .unwrap(),
        )
        .unwrap();
    h.run("mid-scan-cancel.lua","local s=fixture_image();local p=s:note_plan{palette={{1,2,3}},pixel_notes=true};assert(not pcall(interrupt_native,p));assert(p:info().read==0);assert(p:next(1).start[1]==0)").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn native_image_append_preserves_authoritative_group_ancestor_locks_at_finish() {
    let (h, _, doc, _) = writer_fixture(128, Limits::default());
    h.run("image-group-lock.lua",&format!("{SMALL_PLAN}local t=plum.project.current():track(1);local tx=plum.transaction{{}};local parent=tx:create_group{{name='Parent'}};local child=tx:create_group{{name='Child',parent=parent}};local w=tx:note_writer(t);w:append({{{{start=20,gate=1,key=70,group=child:id()}}}});assert(w:append_plan(plan)==5);tx:edit_group(parent,{{locked=true}});assert(not pcall(function()w:finish()end));assert(w:info().state=='failed' and t:notes():count()==0 and plan:info().done);tx:cancel()")).unwrap();
    assert!(h.take_commits().is_none());
    assert_eq!(doc.note_count(), 0);
}
#[test]
fn published_native_plan_writer_example_executes_verbatim() {
    let (h, _, doc, _) = writer_fixture(128, Limits::default());
    h.run("published-native-writer.lua", documentation_examples()[2])
        .unwrap();
    assert_eq!(h.take_commits().unwrap().result().note_count(), 2);
    assert_eq!(doc.note_count(), 0);
}
#[test]
fn real_worker_cancel_during_native_writer_ingestion_has_no_commit_and_recovers() {
    use crate::lua::{Capability, World};
    let (_, _, doc, history) = writer_fixture(128, Limits::default());
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    let mut worker = ScriptWorker::spawn(
        Limits {
            memory: 16 * 1024 * 1024,
            time: Duration::from_secs(15),
            ..Default::default()
        },
        vec![Capability::ReadProject, Capability::EditNotes],
        &Scripts::default(),
    )
    .unwrap();
    worker.submit(ScriptJob{file:"cancel-native-append.lua".into(),source:"local s=plum.images.from_rgba(16384,64,string.rep(string.char(1,2,3,255),16384*64));local p=s:note_plan{palette={{1,2,3}},pixel_notes=true};s:close();local tx=plum.transaction{};local w=tx:note_writer(plum.project.current():track(1));plum.progress(.5,'native writer ingestion');w:append_plan(p);w:finish();tx:commit()".into(),world:Some(world),time_limit:None}).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while worker.progress().fraction != Some(0.5) {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.cancel_job();
    let result = worker.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(result.result.unwrap_err().category, Category::Canceled);
    assert!(result.commits.is_none());
    assert_eq!(doc.note_count(), 0);
    worker
        .submit(ScriptJob {
            file: "recovered-native-append.lua".into(),
            source: "return 7".into(),
            world: None,
            time_limit: None,
        })
        .unwrap();
    assert_eq!(
        worker.wait(Duration::from_secs(5)).unwrap().result.unwrap(),
        Plain::Integer(7)
    );
}
