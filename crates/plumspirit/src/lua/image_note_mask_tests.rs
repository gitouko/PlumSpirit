use super::*;
use crate::lua::{Host, Limits};
use std::time::Duration;
fn host(native: usize) -> (Host, NativeBudget) {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_secs(15),
            ..Default::default()
        },
        [],
    )
    .unwrap();
    let b = NativeBudget::isolated(native);
    h.lua.set_app_data(b.clone());
    (h, b)
}
fn mapping(policy: Policy, orientation: im::Orientation) -> Mapping {
    Mapping {
        start: 9007199254740993,
        end: 9007199254741026,
        first: 180,
        last: 183,
        orientation,
        policy,
        threshold: 128,
    }
}
fn reference(
    pixels: &[u8],
    width: usize,
    height: usize,
    m: Mapping,
    start: u64,
    gate: u64,
    key: u8,
) -> (f64, bool) {
    if key < m.first || key > m.last {
        return (0., false);
    }
    let (time, lines) = if m.orientation == im::Orientation::PianoRoll {
        (width, height)
    } else {
        (height, width)
    };
    let pitch = if m.orientation == im::Orientation::PianoRoll {
        m.last - key
    } else {
        key - m.first
    };
    let line = (((f64::from(pitch) + 0.5) * lines as f64)
        / f64::from(u16::from(m.last) - u16::from(m.first) + 1))
    .floor() as usize;
    let (lo, hi) = (u128::from(m.start), u128::from(m.end));
    let span = hi - lo;
    let mut samples = Vec::new();
    for i in 0..time {
        let selected = match m.policy {
            Policy::Start | Policy::Center => {
                let tick = u128::from(start)
                    + if matches!(m.policy, Policy::Center) {
                        u128::from(gate) / 2
                    } else {
                        0
                    };
                tick >= lo
                    && tick < hi
                    && (tick - lo) * (time as u128) >= span * i as u128
                    && (tick - lo) * (time as u128) < span * (i + 1) as u128
            }
            _ => {
                let left = u128::from(start).max(lo);
                let right = (u128::from(start) + u128::from(gate.max(1))).min(hi);
                right > left
                    && (left - lo) * (time as u128) < span * (i + 1) as u128
                    && (right - lo) * (time as u128) > span * i as u128
            }
        };
        if selected {
            samples.push(if m.orientation == im::Orientation::PianoRoll {
                pixels[line * width + i]
            } else {
                pixels[(height - 1 - i) * width + line]
            });
        }
    }
    if samples.is_empty() {
        return (0., false);
    }
    let sum = samples.iter().map(|v| u64::from(*v)).sum::<u64>();
    let count = samples.len() as u64;
    let matched = match m.policy {
        Policy::Any => samples.iter().any(|v| *v >= m.threshold),
        Policy::All => samples.iter().all(|v| *v >= m.threshold),
        _ => sum >= u64::from(m.threshold) * count,
    };
    (sum as f64 / count as f64, matched)
}
#[test]
fn native_prefix_sampling_matches_independent_pixel_intersections_for_all_policies_and_orientations()
 {
    let pixels = (0..68)
        .map(|i| ((i * 73 + i / 3) % 256) as u8)
        .collect::<Vec<_>>();
    let budget = NativeBudget::isolated(1024 * 1024);
    for orientation in [im::Orientation::PianoRoll, im::Orientation::Upstream] {
        for policy in [
            Policy::Start,
            Policy::Center,
            Policy::Any,
            Policy::All,
            Policy::Average,
        ] {
            let m = mapping(policy, orientation);
            let lines = if orientation == im::Orientation::PianoRoll {
                4
            } else {
                17
            };
            let s = build(
                17,
                4,
                &pixels,
                m,
                budget.reserve((68 + lines) * 8).unwrap(),
                || Ok(()),
            )
            .unwrap();
            for key in [0, 179, 180, 181, 182, 183, 184, 255] {
                for start in 9007199254740983..9007199254741036 {
                    for gate in [0, 1, 2, 7, 33, 90, u64::MAX] {
                        assert_eq!(
                            s.sample(start, gate, key),
                            reference(&pixels, 17, 4, m, start, gate, key),
                            "key{key},start{start},gate{gate},{}",
                            policy.name()
                        );
                    }
                }
            }
        }
    }
    assert_eq!(budget.used(), 0);
}
#[test]
fn public_scalar_and_packed_sampling_source_close_and_eager_job_expiry() {
    let (h, b) = host(8 * 1024 * 1024);
    h.run("sampler.lua","local image=plum.images.from_rgba(4,1,string.char(0,0,0,0,0,0,0,64,0,0,0,128,0,0,0,255));local mask=image:mask{kind='alpha'};held=mask:note_sampler{start_tick='9007199254740993',end_tick='9007199254741005',first_key=200,last_key=200,sample='any',threshold=128};image:close();mask:close();local i=held:info();assert(i.time_pixels==4 and i.pitch_pixels==1 and i.index_bytes==40 and i.first_key==200);local coverage,matched=held:sample(9007199254740993,12,200);assert(coverage==111.75 and matched);local b=held:sample_batch{start={9007199254740993,9007199254740996,9007199254741005},gate={12,3,1},key={200,200,200}};assert(b.coverage[1]==111.75 and b.matched[1] and b.coverage[2]==64 and not b.matched[2] and b.coverage[3]==0 and not b.matched[3]);local c,ok=held:sample(9007199254740993,12,60);assert(c==0 and not ok)").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), METADATA);
    h.run("expired-sampler.lua","assert(not pcall(function()held:sample(0,1,60)end));assert(not held:close());held=nil;collectgarbage('collect')").unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn strict_domain_policy_batch_and_resource_limit_validation() {
    let invalid = [
        "{}",
        "{end_tick=0}",
        "{end_tick=1,start_tick=1}",
        "{end_tick=1,first_key=200,last_key=127}",
        "{end_tick=1,last_key=256}",
        "{end_tick=1,sample='max'}",
        "{end_tick=1,orientation='other'}",
        "{end_tick=1,threshold=-1}",
        "{end_tick=1,threshold=0/0}",
        "{end_tick=1,extra=true}",
        "setmetatable({end_tick=1},{})",
    ];
    for opts in invalid {
        let (h, b) = host(8 * 1024 * 1024);
        assert!(h.run("invalid.lua",&format!("local i=plum.images.from_rgba(1,1,string.char(1,2,3,255));local m=i:mask{{kind='alpha'}};m:note_sampler({opts})")).is_err());
        h.lua.gc_collect().unwrap();
        assert_eq!(b.used(), 0);
    }
    let (h, b) = host(8 * 1024 * 1024);
    h.run("strict-batch.lua","local i=plum.images.from_rgba(1,1,string.char(1,2,3,255));local m=i:mask{kind='alpha'};local s=m:note_sampler{end_tick=1};for _,v in ipairs({{}, {start={0},gate={},key={60}}, {start={0},gate={1},key={60},extra=true}, {start={[2]=0},gate={1},key={60}},setmetatable({start={},gate={},key={}},{})})do assert(not pcall(function()s:sample_batch(v)end))end;assert(not pcall(function()s:sample(0,1,256)end));assert(not pcall(function()s:sample(0/0,1,60)end));assert(#s:sample_batch{start={},gate={},key={}}.coverage==0);assert(s:close() and not s:close());for j=2,16 do local v=m:note_sampler{end_tick=1};v:close()end;assert(not pcall(function()m:note_sampler{end_tick=1}end))").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn native_preflight_cancellation_and_failed_job_release_index_storage() {
    let (h, b) = host(5000);
    assert!(h.run("denied.lua","local i=plum.images.from_rgba(512,1,string.rep(string.char(1,2,3,255),512));local m=i:mask{kind='alpha'};m:note_sampler{end_tick=512}").is_err());
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
    let budget = NativeBudget::isolated(1024 * 1024);
    let cells = 16384;
    let calls = Cell::new(0);
    let result = build(
        16384,
        1,
        &vec![255; cells],
        mapping(Policy::Any, im::Orientation::PianoRoll),
        budget.reserve((cells + 1) * 8).unwrap(),
        || {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                Err(stop(Category::Canceled, "Controlled prefix cancellation."))
            } else {
                Ok(())
            }
        },
    );
    assert!(result.is_err());
    assert_eq!(calls.get(), 3);
    assert_eq!(budget.used(), 0);
    let (h, b) = host(8 * 1024 * 1024);
    assert!(h.run("failed-job.lua","local i=plum.images.from_rgba(1,1,string.char(1,2,3,255));local m=i:mask{kind='alpha'};held=m:note_sampler{end_tick=1};error('after sampler')").is_err());
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), METADATA);
    h.run(
        "recover.lua",
        "assert(not pcall(function()held:info()end));held=nil;collectgarbage('collect')",
    )
    .unwrap();
    assert_eq!(b.used(), 0);
}

#[test]
fn existing_alpha_color_brightness_luminance_and_transparency_policies_compose() {
    let (h, b) = host(8 * 1024 * 1024);
    h.run("mask-composition.lua","local i=plum.images.from_rgba(3,1,string.char(255,255,255,255,255,255,255,64,255,255,255,0));for _,kind in ipairs({'alpha','color','brightness','luminance'})do local options={kind=kind};if kind=='color'then options.color={255,255,255}end;local m=i:mask(options);local s=m:note_sampler{end_tick=3,first_key=60,last_key=60};for x=0,2 do local value,ok=s:sample(x,1,60);assert(value==m:pixel(x,0) and ok==(x==0))end;s:close();m:close()end;local m=i:mask{kind='color',color={255,255,255},preserve_alpha=false};local s=m:note_sampler{end_tick=3,first_key=60,last_key=60,threshold=0};local value,yes=s:sample(1,1,60);assert(value==255 and yes);value,yes=s:sample(3,1,60);assert(value==0 and not yes);i:close();m:close();s:close()").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn key_range_centers_and_packed_limit_include_both_native_key_domains() {
    let (h, b) = host(8 * 1024 * 1024);
    h.run("key-domain.lua","local i=plum.images.from_rgba(1,2,string.char(0,0,0,255,0,0,0,64));local m=i:mask{kind='alpha'};for _,keys in ipairs({128,256})do local s=m:note_sampler{end_tick=1,first_key=0,last_key=keys-1};for k=0,keys-1 do local value,yes=s:sample(0,1,k);assert(value==(k<keys/2 and 64 or 255) and yes==(k>=keys/2))end;s:close()end;local s=m:note_sampler{end_tick=1};local batch={start={},gate={},key={}};for n=1,4097 do batch.start[n]=0;batch.gate[n]=1;batch.key[n]=60 end;assert(not pcall(function()s:sample_batch(batch)end));batch.start[4097]=nil;batch.gate[4097]=nil;batch.key[4097]=nil;local result=s:sample_batch(batch);assert(#result.coverage==4096 and #result.matched==4096)").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn cumulative_sampler_cell_work_is_not_refunded_by_close() {
    let (h, b) = host(64 * 1024 * 1024);
    h.lua
        .globals()
        .set(
            "fixture_image",
            h.lua
                .create_function(|lua, ()| {
                    let (job, budget) = context(lua)?;
                    let count = size(16384, 128)?;
                    let allocation = budget.reserve(count * 4).map_err(|e| e.lua())?;
                    store(
                        lua,
                        job,
                        Rgba::new(16384, 128, vec![[1, 2, 3, 255]; count]).map_err(bad)?,
                        "rgba",
                        allocation,
                    )
                })
                .unwrap(),
        )
        .unwrap();
    h.run("cumulative-sampler.lua","local i=fixture_image();local m=i:mask{kind='alpha'};i:close();for n=1,8 do local s=m:note_sampler{end_tick=16384};assert(s:info().index_bytes==(16384+1)*128*8);s:close()end;assert(not pcall(function()m:note_sampler{end_tick=16384}end))").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn published_note_sampler_example_executes_verbatim() {
    let (h, b) = host(8 * 1024 * 1024);
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Image-mask note sampling (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-note-sampler.lua", source).unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}

fn query_fixture(
    keys: u16,
    grants: Vec<crate::lua::Capability>,
) -> (
    Host,
    Arc<plumspirit_engine::model::PlumDocument>,
    plumspirit_engine::history::History,
) {
    use crate::lua::World;
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{
            Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, RankSetBuilder, SubchannelId,
            project::KeyRange,
        },
    };
    let mut doc = PlumDocument::new_project("Mask queries".into(), Bpm::DEFAULT, 960);
    doc.add_track().unwrap();
    doc.project_mut().keys = if keys == 256 {
        KeyRange::Extended256
    } else {
        KeyRange::Standard
    };
    let first = if keys == 256 { 180 } else { 60 };
    let origin = 9007199254740993;
    let records = (0..1200)
        .map(|i| NoteRecord {
            start_tick: origin + i * 3,
            duration_ticks: if i % 113 == 0 { 100000 } else { 1 + i % 20 },
            attrs: NoteAttrs::new_wide(
                first + (i % 12) as u8,
                1 + (i % 127) as u8,
                (i % 4) as u8,
                0,
                NoteOffKind::NoteOff,
            )
            .unwrap()
            .with_muted(i % 3 == 0),
            sub: SubchannelId::MAIN,
            group: 0,
        })
        .collect::<Vec<_>>();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&records, false)
        .unwrap();
    let mut selected = RankSetBuilder::default();
    for r in (0..1200).step_by(5) {
        selected.push(r);
    }
    doc.track_mut(0)
        .unwrap()
        .notes
        .select_ranks(&selected.finish())
        .unwrap();
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_secs(15),
            ..Default::default()
        },
        grants,
    )
    .unwrap();
    h.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    (h, doc, history)
}
#[test]
fn indexed_mask_queries_match_existing_filters_and_scalar_evaluation_in_both_key_modes() {
    use crate::lua::Capability;
    for keys in [128, 256] {
        let (h, doc, _) = query_fixture(keys, vec![Capability::ReadProject]);
        let first = if keys == 256 { 180 } else { 60 };
        let before = doc.export_fingerprint();
        h.run("indexed-count.lua",&format!("local t=plum.project.current():track(1);local image=plum.images.from_rgba(4,2,string.char(0,0,0,255,0,0,0,0,0,0,0,64,0,0,0,255,0,0,0,128,0,0,0,255,0,0,0,0,0,0,0,255));local mask=image:mask{{kind='alpha'}};local filters={{{{}},{{keys={{{first}+2,{first}+7}}}},{{channel=2}},{{subchannel=0}},{{subchannel=1}},{{velocity={{40,80}}}},{{selected=true}},{{group=0}},{{group=1}},{{muted=true}},{{muted=false}},{{start_tick=9007199254741113,end_tick=9007199254741146}},{{end_tick=0}}}};for _,orientation in ipairs({{'piano_roll','upstream'}})do for _,policy in ipairs({{'start','center','any','all','average'}})do local s=mask:note_sampler{{start_tick=9007199254741113,end_tick=9007199254741146,first_key={first},last_key={first}+11,orientation=orientation,sample=policy}};for _,filter in ipairs(filters)do local query=t:notes(filter);local expected=0;for n in query:each()do local _,yes=s:sample(n.start,n.gate,n.key);if yes then expected=expected+1 end end;assert(s:count_notes(query)==expected)end;s:close()end end")).unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        assert!(h.take_commits().is_none());
    }
}
#[test]
fn indexed_query_owned_linked_clip_and_subtrack_use_their_native_local_domain() {
    use crate::lua::Capability;
    let (h, _, _) = query_fixture(
        256,
        vec![
            Capability::ReadProject,
            Capability::EditNotes,
            Capability::EditTracks,
        ],
    );
    h.run("clip-mask-query.lua","local t=plum.project.current():track(1);local tx=plum.transaction{};local child=tx:add_subtracks(t,1)[1];local sub=tx:add_subchannel(child,{channel=15});tx:insert_notes(child,{{start=17,gate=3,key=200,subchannel=sub:id()}});local clip=tx:add_clip(child,10000,1000);local linked=tx:duplicate_clips(clip,{start=20000,linked=true})[1];tx:insert_notes(clip:content(),{{start=17,gate=3,key=200,subchannel=sub:id()}});local i=plum.images.from_rgba(1,1,string.char(0,0,0,255));local m=i:mask{kind='alpha'};local s=m:note_sampler{start_tick=17,end_tick=20,first_key=200,last_key=200,sample='any'};assert(s:count_notes(child:notes{subchannel=sub:id()})==1 and s:count_notes(clip:notes())==1 and s:count_notes(linked:notes())==1);assert(s:count_notes(linked:notes{start_tick=10017})==0);tx:cancel()").unwrap();
    assert!(h.take_commits().is_none());
}
#[test]
fn indexed_query_requires_project_read_and_live_sampler_query_handles() {
    use crate::lua::{Capability, Category};
    let (h, doc, history) = query_fixture(128, vec![]);
    let world = crate::lua::World::new(doc, history.incarnations().snapshot());
    let handle = world.track(world.doc.track(0).unwrap().id()).unwrap();
    h.lua
        .globals()
        .set(
            "query",
            h.lua
                .create_userdata(crate::lua::notes::LuaNoteQuery {
                    host: handle,
                    filter: Default::default(),
                })
                .unwrap(),
        )
        .unwrap();
    let err=h.run("denied-count.lua","local i=plum.images.from_rgba(1,1,string.char(0,0,0,255));local m=i:mask{kind='alpha'};local s=m:note_sampler{end_tick=1};return s:count_notes(query)").unwrap_err();
    assert_eq!(err.category, Category::Capability);
    let (h, _, _) = query_fixture(128, vec![Capability::ReadProject]);
    h.run("retained-query.lua","query=plum.project.current():track(1):notes();local i=plum.images.from_rgba(1,1,string.char(0,0,0,255));local m=i:mask{kind='alpha'};held=m:note_sampler{end_tick=1};assert(not pcall(function()held:count_notes(5)end))").unwrap();
    h.run(
        "expired-query.lua",
        "assert(not pcall(function()held:count_notes(query)end))",
    )
    .unwrap();
}

#[test]
fn published_indexed_count_example_executes_without_musical_changes() {
    let (h, doc, _) = query_fixture(128, vec![crate::lua::Capability::ReadProject]);
    let before = doc.export_fingerprint();
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Image-mask note sampling (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(2)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-indexed-mask-count.lua", source).unwrap();
    assert_eq!(doc.export_fingerprint(), before);
    assert!(h.take_commits().is_none());
}
