use super::*;
use crate::lua::{Host, Limits};
fn host() -> Host {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(16 * 1024 * 1024));
    h
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn horizontal_native_caps_have_exact_distinct_cells_and_mapped_grids_agree() {
    let h = host();
    h.run("stroke-caps.lua",r#"
      local g=plum.geometry;local p={x={2,6},y={2.1,2.1}};local grid={columns=8,rows=5}
      local butt=g.rasterize_stroke(p,grid,{width_cells=4.2,cap='butt'});assert(butt.count==4 and butt.cell_count==16)
      local square=g.rasterize_stroke(p,grid,{width_cells=4.2,cap='square'});assert(square.count==4 and square.cell_count==32)
      local round=g.rasterize_stroke(p,grid,{width_cells=4.2});assert(round.count==4 and round.cell_count==30)
      assert(round.first[1]==1 and round.last[1]==7)
      local mapped=g.rasterize_stroke({x={104,112},y={206.3,206.3}},{columns=8,rows=5,origin_x=100,origin_y=200,cell_width=2,cell_height=3},{width_cells=4.2})
      assert(mapped.cell_count==round.cell_count);for i=1,round.count do assert(mapped.row[i]==round.row[i]and mapped.first[i]==round.first[i]and mapped.last[i]==round.last[i])end
      assert(p.x[1]==2 and p.y[1]==2.1 and g.stroke_miter_limit==4)
    "#).unwrap();
    clear(&h);
}
#[test]
fn closed_duplicate_and_degenerate_paths_keep_native_caps_and_join_semantics() {
    let h = host();
    h.run("stroke-paths.lua",r#"
      local g=plum.geometry;local grid={columns=16,rows=16};local p={x={3,3,10,10},y={10,3,3,10}}
      local function same(a,b)assert(a.count==b.count and a.cell_count==b.cell_count);for i=1,a.count do assert(a.row[i]==b.row[i]and a.first[i]==b.first[i]and a.last[i]==b.last[i])end end
      for _,join in ipairs({'round','bevel','miter'})do
        local a=g.rasterize_stroke(p,grid,{width_cells=3,join=join,closed=true,cap='butt'})
        local b=g.rasterize_stroke({x={3,3,3,10,10,3},y={10,10,3,3,10,10}},grid,{width_cells=3,join=join,closed=true,cap='square'});same(a,b)
        local open=g.rasterize_stroke(p,grid,{width_cells=3,join=join});assert(open.cell_count<a.cell_count)
      end
      local dot={x={5,5},y={5,5}};assert(g.rasterize_stroke(dot,grid,{width_cells=3,cap='butt'}).count==0)
      assert(g.rasterize_stroke(dot,grid,{width_cells=3,cap='round'}).cell_count>0)
      assert(g.rasterize_stroke(dot,grid,{width_cells=3,cap='square'}).cell_count>0)
    "#).unwrap();
    clear(&h);
}

#[test]
fn seeded_axis_aligned_caps_match_independent_continuous_intervals_and_grid_rounding() {
    let h = host();
    h.run("stroke-reference.lua",r#"
      local g=plum.geometry;local seed=19;local function rand(n)seed=seed*48271%2147483647;return seed%n end
      for scene=1,96 do local a=rand(50)/10+2.11;local b=a+rand(70)/10+3.17;local y=rand(80)/10+1.13;local width=(rand(31)+8)/10+.07;local radius=width/2
        for _,cap in ipairs({'round','butt','square'})do
          local result=g.rasterize_stroke({x={a,b},y={y,y}},{columns=32,rows=16},{width_cells=width,cap=cap})
          local expected={};local cells=0
          for row=0,15 do local dy=row+.5-y;if math.abs(dy)<radius then
            local extension=cap=='square'and radius or cap=='round'and math.sqrt(radius*radius-dy*dy)or 0
            local first=math.floor(a-extension+.5);local last=math.max(first+1,math.floor(b+extension+.5));first=math.max(0,math.min(32,first));last=math.max(0,math.min(32,last));if first<last then expected[#expected+1]={row,first,last};cells=cells+last-first end
          end end
          assert(result.count==#expected and result.cell_count==cells)
          for i,p in ipairs(expected)do assert(result.row[i]==p[1]and result.first[i]==p[2]and result.last[i]==p[3])end
        end
      end
    "#).unwrap();
    clear(&h);
}
#[test]
fn invalid_precision_work_and_span_limits_fail_atomically_and_maximum_points_are_usable() {
    let h = host();
    h.run("stroke-limits.lua",r#"
      local g=plum.geometry;local p={x={2,6},y={2,2}};local grid={columns=8,rows=8}
      for _,options in ipairs({{}, {width_cells=0},{width_cells=-1},{width_cells=0/0},{width_cells=math.huge},{width_cells=1e-300},{width_cells=1,closed=1},{width_cells=1,join='other'},{width_cells=1,cap='other'},{width_cells=1,extra=true},setmetatable({width_cells=1},{})})do assert(not pcall(g.rasterize_stroke,p,grid,options))end
      assert(not pcall(g.rasterize_stroke,{x={1e12},y={0}},grid,{width_cells=1e-5}))
      assert(not pcall(g.rasterize_stroke,p,{columns=8,rows=8,cell_width=1e-308},{width_cells=1}))
      assert(not pcall(g.rasterize_stroke,{x={2,2},y={0,8}},{columns=8,rows=8,max_spans=1},{width_cells=2}))
      local path={x={},y={}};for i=1,4096 do path.x[i]=i-1;path.y[i]=.5 end
      local result=g.rasterize_stroke(path,{columns=65536,rows=256},{width_cells=1});assert(result.count>0)
      path.x[4097]=4096;path.y[4097]=.5;assert(not pcall(g.rasterize_stroke,path,{columns=65536,rows=1},{width_cells=1}))
      path={x={},y={}};for i=1,512 do path.x[i]=i;path.y[i]=.5 end;assert(not pcall(g.rasterize_stroke,path,{columns=1024,rows=4096},{width_cells=1}))
      assert(p.x[1]==2 and p.y[1]==2)
    "#).unwrap();
    clear(&h);
}
#[test]
fn native_preflight_and_empty_row_cancellation_release_the_shared_geom_workspace() {
    use std::cell::Cell;
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(16 * 1024));
    let e=h.run("stroke-budget.lua","return plum.geometry.rasterize_stroke({x={0,8},y={1,1}},{columns=8,rows=8},{width_cells=2})").unwrap_err();
    assert_eq!(e.category, Category::Budget);
    clear(&h);
    let h = host();
    h.lua
        .globals()
        .set(
            "interrupt_stroke",
            h.lua
                .create_function(|lua, (p, grid, options): (Table, Table, Table)| {
                    let calls = Cell::new(0);
                    let result = controlled(lua, &p, &grid, &options, || {
                        calls.set(calls.get() + 1);
                        if calls.get() == 8 {
                            Err(stop(
                                Category::Canceled,
                                "Controlled empty stroke row cancellation.",
                            ))
                        } else {
                            Ok(())
                        }
                    });
                    assert_eq!(calls.get(), 8);
                    result
                })
                .unwrap(),
        )
        .unwrap();
    h.run("stroke-cancel.lua","local p={x={0,8},y={-5000,-5000}};assert(not pcall(interrupt_stroke,p,{columns=8,rows=256},{width_cells=2}));assert(p.x[1]==0 and p.y[1]==-5000)").unwrap();
    clear(&h);
}
#[test]
fn actual_vm_output_failure_allows_retry_of_the_same_native_stroke() {
    let h = host();
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
    h.run("stroke-vm.lua","local p={x={1,1},y={0,4096}};local grid={columns=4,rows=4096,max_spans=4096};local options={width_cells=1};limit_vm();local ok=pcall(plum.geometry.rasterize_stroke,p,grid,options);restore_vm();assert(not ok and p.y[2]==4096);local result=plum.geometry.rasterize_stroke(p,grid,options);assert(result.count==4096 and result.cell_count==4096)").unwrap();
    clear(&h);
}

#[test]
fn published_thick_stroke_artwork_has_preview_apply_one_undo_redo_in_both_key_modes() {
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    use std::sync::Arc;
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Thick-stroke note-cell rasterization (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut d = PlumDocument::new_project("Stroke example".into(), Bpm::DEFAULT, 960);
        d.project_mut().keys = keys;
        let mut doc = Arc::new(d);
        let mut history = History::new(HistoryLimits::default());
        history.reset(false);
        history.observe_ids(&doc);
        let h = Host::new(
            Limits {
                memory: 8 * 1024 * 1024,
                ..Default::default()
            },
            [Capability::ReadProject, Capability::EditNotes],
        )
        .unwrap();
        h.lua.set_app_data(NativeBudget::isolated(8 * 1024 * 1024));
        let bind = |h: &Host, doc: &Arc<PlumDocument>, history: &History| {
            h.set_world(Some(World::new(
                Arc::clone(doc),
                history.incarnations().snapshot(),
            )))
        };
        bind(&h, &doc, &history);
        h.run("stroke-preview.lua", source).unwrap();
        let preview = h.take_commits().unwrap();
        let count = preview.result().note_count();
        assert!(count > 0 && count <= 64);
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        bind(&h, &doc, &history);
        h.run("stroke-apply.lua", source).unwrap();
        let mut commits = h.take_commits().unwrap();
        assert_eq!(commits.result().export_fingerprint(), expected);
        assert_eq!(commits.steps[0].operations, 1);
        h.set_world(None);
        commits.apply(&mut history, &mut doc).unwrap();
        drop(commits);
        assert_eq!(doc.note_count(), count);
        history
            .undo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.note_count(), 0);
        assert!(!history.can_undo());
        history
            .redo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
        clear(&h);
    }
}
