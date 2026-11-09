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
    h.lua.set_app_data(NativeBudget::isolated(8 * 1024 * 1024));
    h
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn seeded_boolean_rasters_match_independent_dense_cell_set_references() {
    let h = host();
    h.run("boolean-reference.lua",r#"
      local g=plum.geometry;local seed=73
      local function random(n)seed=(seed*48271)%2147483647;return seed%n end
      local function source()
        local r={row={},first={},last={}};local set={}
        for i=1,40 do local row=random(8);local a=random(16);local b=a+1+random(16-a);r.row[i]=row;r.first[i]=a;r.last[i]=b;for x=a,b-1 do set[row*16+x]=true end end
        return r,set
      end
      for scene=1,96 do local a,sa=source();local b,sb=source()
        for _,op in ipairs({'union','intersection','difference','xor'})do
          local r=g.raster_boolean(a,b,{columns=16,rows=8},op);local actual={};local previous_row,previous_last=-1,-1
          for i=1,r.count do assert(r.row[i]>=previous_row and (r.row[i]~=previous_row or r.first[i]>previous_last));previous_row=r.row[i];previous_last=r.last[i];for x=r.first[i],r.last[i]-1 do local k=r.row[i]*16+x;assert(not actual[k]);actual[k]=true end end
          local count=0;for k=0,127 do local x,y=sa[k]or false,sb[k]or false;local yes=op=='union' and(x or y)or op=='intersection'and(x and y)or op=='difference'and(x and not y)or op=='xor'and(x~=y);assert((actual[k]or false)==(yes or false));if yes then count=count+1 end end;assert(count==r.cell_count)
        end
      end
    "#).unwrap();
    clear(&h);
}
#[test]
fn native_raster_outputs_compose_with_empty_adjacency_and_post_union_caps() {
    let h = host();
    h.run("boolean-compose.lua",r#"
      local g=plum.geometry;local grid={columns=8,rows=4}
      local a=g.rasterize_polygon({x={0,5,5,0},y={0,0,3,3}},grid)
      local b=g.rasterize_polygon({x={3,8,8,3},y={1,1,4,4}},grid)
      local union=g.raster_boolean(a,b,grid,'union');assert(union.cell_count==26)
      local both=g.raster_boolean(a,b,grid,'intersection');assert(both.cell_count==4)
      assert(g.raster_boolean(a,b,grid,'difference').cell_count==11)
      assert(g.raster_boolean(a,b,grid,'xor').cell_count==22)
      local empty={row={},first={},last={},count=0,cell_count=0}
      assert(g.raster_boolean(a,empty,grid,'intersection').count==0)
      assert(g.raster_boolean(empty,empty,grid,'union').count==0)
      local raw={row={0,0,0},first={2,0,4},last={4,2,6},count=3,cell_count=6}
      local merged=g.raster_boolean(raw,empty,{columns=8,rows=1,max_spans=1},'union');assert(merged.count==1 and merged.first[1]==0 and merged.last[1]==6)
      assert(raw.count==3 and raw.first[1]==2 and raw.last[1]==4)
      assert(not pcall(g.raster_boolean,{row={0},first={0},last={8}},{row={0},first={3},last={5}},{columns=8,rows=1,max_spans=1},'difference'))
    "#).unwrap();
    clear(&h);
}

#[test]
fn strict_input_metadata_grid_and_integer_validation_never_mutates_sources() {
    let bad_inputs = [
        "{}",
        "{row={0},first={},last={1}}",
        "{row={0},first={0},last={0}}",
        "{row={0},first={2},last={1}}",
        "{row={-1},first={0},last={1}}",
        "{row={4},first={0},last={1}}",
        "{row={0},first={0},last={9}}",
        "{row={0},first={0/0},last={1}}",
        "{row={0},first={0.0},last={1}}",
        "{row={0},first={0},last={1},count=2}",
        "{row={0},first={0},last={1},cell_count=0}",
        "{row={0},first={0},last={1},extra=true}",
        "{row={[0]=0},first={},last={}}",
        "{row=setmetatable({0},{}),first={0},last={1}}",
        "setmetatable({row={},first={},last={}},{})",
    ];
    for source in bad_inputs {
        let h = host();
        h.run("invalid-boolean.lua",&format!("local a={{row={{0}},first={{0}},last={{1}},count=1,cell_count=1}};assert(not pcall(plum.geometry.raster_boolean,a,{source},{{columns=8,rows=4}},'union'));assert(a.row[1]==0 and a.first[1]==0 and a.last[1]==1 and a.count==1 and a.cell_count==1)")).unwrap();
        clear(&h);
    }
    let h = host();
    h.run("invalid-op-grid.lua","local e={row={},first={},last={}};for _,op in ipairs({'not',true,{},1})do assert(not pcall(plum.geometry.raster_boolean,e,e,{columns=8,rows=4},op))end;for _,grid in ipairs({{columns=0,rows=1},{columns=65537,rows=1},{columns=1,rows=4097},{columns=1,rows=1,max_spans=0},{columns=1,rows=1,cell_width=0},{columns=1,rows=1,origin_x=0/0}})do assert(not pcall(plum.geometry.raster_boolean,e,e,grid,'union'))end").unwrap();
    clear(&h);
}
#[test]
fn native_preflight_and_mid_sweep_cancel_do_not_publish_partial_results() {
    use std::cell::Cell;
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(1024));
    let error=h.run("budget.lua","return plum.geometry.raster_boolean({row={0},first={0},last={1}},{row={},first={},last={}},{columns=4,rows=1},'union')").unwrap_err();
    assert_eq!(error.category, Category::Budget);
    clear(&h);
    let a = (0..8192)
        .map(|i| Span {
            row: 0,
            first: i * 4,
            last: i * 4 + 2,
        })
        .collect::<Vec<_>>();
    let b = (0..8192)
        .map(|i| Span {
            row: 0,
            first: i * 4 + 1,
            last: i * 4 + 3,
        })
        .collect::<Vec<_>>();
    let mut out = Vec::with_capacity(16384);
    let calls = Cell::new(0);
    let result = sweep(&a, &b, Op::Xor, &mut out, 16384, || {
        calls.set(calls.get() + 1);
        if calls.get() == 5 {
            Err(stop(
                Category::Canceled,
                "Controlled raster Boolean cancellation.",
            ))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(calls.get(), 5);
    assert!(!out.is_empty() && out.len() < 16384);
    assert_eq!(
        a[0],
        Span {
            row: 0,
            first: 0,
            last: 2
        }
    );
    assert_eq!(b.len(), 8192);
}
#[test]
fn actual_vm_output_allocation_failure_leaves_inputs_usable_for_retry() {
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
    h.run("vm-boolean.lua","local a={row={},first={},last={}};local b={row={},first={},last={}};for i=1,8192 do a.row[i]=0;a.first[i]=(i-1)*4;a.last[i]=(i-1)*4+2;b.row[i]=0;b.first[i]=(i-1)*4+1;b.last[i]=(i-1)*4+3 end;local grid={columns=32768,rows=1,max_spans=65536};limit_vm();local ok=pcall(plum.geometry.raster_boolean,a,b,grid,'xor');restore_vm();assert(not ok and #a.row==8192 and b.last[8192]==32767);local result=plum.geometry.raster_boolean(a,b,grid,'xor');assert(result.count==16384 and result.cell_count==16384)").unwrap();
    clear(&h);
}

#[test]
fn published_boolean_artwork_has_deterministic_preview_apply_one_undo_redo_both_modes() {
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    use std::sync::Arc;
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Boolean raster composition (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut d = PlumDocument::new_project("Boolean example".into(), Bpm::DEFAULT, 960);
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
        h.run("published-boolean-preview.lua", source).unwrap();
        let preview = h.take_commits().unwrap();
        assert_eq!(preview.result().note_count(), 12);
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        bind(&h, &doc, &history);
        h.run("published-boolean-apply.lua", source).unwrap();
        let mut commits = h.take_commits().unwrap();
        assert_eq!(commits.result().export_fingerprint(), expected);
        assert_eq!(commits.steps[0].operations, 1);
        h.set_world(None);
        commits.apply(&mut history, &mut doc).unwrap();
        drop(commits);
        assert_eq!(doc.note_count(), 12);
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
