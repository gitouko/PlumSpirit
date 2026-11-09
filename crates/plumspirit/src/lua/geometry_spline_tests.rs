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
    h.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    h
}
fn run(h: &Host, source: &str) {
    h.run("spline-test.lua", source).unwrap();
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn seeded_splines_match_independent_hermite_and_deboor_references() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry
      local function cat(p,t)
        if #p==1 then return p[1]end
        local function q(i)
          if i<1 then return 2*p[1]-p[2]elseif i>#p then return 2*p[#p]-p[#p-1]else return p[i]end
        end
        local z=t*(#p-1);local i=math.min(math.floor(z),#p-2);local u=z-i
        local a,b,c,d=q(i),q(i+1),q(i+2),q(i+3)
        local h00=2*u^3-3*u^2+1;local h10=u^3-2*u^2+u
        local h01=-2*u^3+3*u^2;local h11=u^3-u^2
        return h00*b+h10*(c-a)/2+h01*c+h11*(d-b)/2
      end
      local function bs(p,t)
        if #p==1 then return p[1]end
        local z=t*(#p+1);local i=math.min(math.floor(z),#p);local u=z-i
        local d={};for j=0,3 do d[j]=p[math.max(1,math.min(#p,i+j-1))]end
        for r=1,3 do for j=3,r,-1 do local a=(u+3-j)/(4-r);d[j]=(1-a)*d[j-1]+a*d[j]end end
        return d[3]
      end
      local seed=192718
      local function random()seed=(seed*1664525+1013904223)%4294967296;return seed end
      local ts={};for i=0,64 do ts[i+1]=i/64 end
      for scene=1,64 do
        local p={x={},y={}};for i=1,1+random()%32 do p.x[i]=(random()%200001-100000)/17;p.y[i]=(random()%200001-100000)/23 end
        for _,kind in ipairs({'catmull_rom','b_spline'})do
          local out=g.spline_points(p,ts,kind);local ref=kind=='catmull_rom' and cat or bs
          assert(out.count==65)
          for i,t in ipairs(ts)do
            local x,y=ref(p.x,t),ref(p.y,t)
            assert(math.abs(out.x[i]-x)<1e-9 and math.abs(out.y[i]-y)<1e-9)
          end
        end
      end
    "#,
    );
    clear(&h);
}

#[test]
fn batched_and_uniform_splines_preserve_native_values_order_and_domains() {
    let h = host();
    h.lua
        .globals()
        .set(
            "native_reference",
            h.lua
                .create_function(|_, (kind, t): (String, f64)| {
                    let p = [
                        (-17., 3.),
                        (23., -90.),
                        (141., 74.),
                        (-2., 19.),
                        (32., -16.),
                    ];
                    Ok(if kind == "catmull_rom" {
                        arcs::catmull(&p, t)
                    } else {
                        arcs::bspline(&p, t)
                    })
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={-17,23,141,-2,32},y={3,-90,74,19,-16},count=5}
      local ts={1,0,0.73,0.25,0.25,0.5}
      for _,kind in ipairs({'catmull_rom','b_spline'})do
        local b=g.spline_points(p,ts,kind)
        for i,t in ipairs(ts)do local x,y=g.spline_point(p,t,kind);local rx,ry=native_reference(kind,t);assert(x==rx and y==ry and b.x[i]==x and b.y[i]==y)end
        local s=g.sample_spline(p,{kind=kind,steps=16});assert(s.count==17)
        for i=1,17 do local x,y=g.spline_point(p,(i-1)/16,kind);assert(x==s.x[i]and y==s.y[i])end
        assert(g.sample_spline(p,{kind=kind}).count==33)
        local mapped=g.map_points(g.mapping{translate_x=100,scale_y=-2},s);assert(mapped.x[1]==100+s.x[1]and mapped.y[1]==-2*s.y[1])
        for _,q in ipairs({{x={7},y={-11}},{x={7,7,7,7},y={-11,-11,-11,-11}}})do
          local d=g.sample_spline(q,{kind=kind});for i=1,d.count do assert(math.abs(d.x[i]-7)<1e-12 and math.abs(d.y[i]+11)<1e-12)end
        end
      end
      for i=0,64 do local t=i/64;local x,y=g.spline_point({x={-7,35},y={21,-43}},t,'catmull_rom');assert(math.abs(x-(-7+42*t))<1e-12 and math.abs(y-(21-64*t))<1e-12)end
      assert(p.x[1]==-17 and p.count==5 and ts[1]==1)
    "#,
    );
    clear(&h);
}

#[test]
fn maximum_spline_controls_and_parameters_remain_bounded_and_usable() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={},y={}};local ts={}
      for i=1,4096 do p.x[i]=i;p.y[i]=-i;ts[i]=(4096-i)/4095 end
      for _,kind in ipairs({'catmull_rom','b_spline'})do
        local b=g.spline_points(p,ts,kind);assert(b.count==4096 and math.abs(b.x[1]-4096)<1e-9 and math.abs(b.x[4096]-1)<1e-9)
        assert(g.sample_spline(p,{kind=kind,steps=4095}).count==4096)
      end
      ts[4097]=0;assert(not pcall(g.spline_points,p,ts,'catmull_rom'));ts[4097]=nil
      p.x[4097]=0;p.y[4097]=0;assert(not pcall(g.spline_point,p,0.5,'b_spline'))
    "#,
    );
    clear(&h);
}

#[test]
fn invalid_spline_descriptors_parameters_and_true_overshoot_fail_atomically() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={-7,2,3,11},y={3,4,5,6}}
      for _,k in ipairs({'catmull','bspline',string.rep('x',33),false,0,{},'catmull_rom\0'})do
        assert(not pcall(g.spline_point,p,0.5,k));assert(not pcall(g.sample_spline,p,{kind=k}))
      end
      assert(not pcall(g.spline_point,p,0.5));assert(not pcall(g.sample_spline,p,{}))
      for _,t in ipairs({-0.1,1.1,0/0,math.huge,'0.5',false,{}})do assert(not pcall(g.spline_point,p,t,'catmull_rom'));assert(not pcall(g.spline_points,p,{0,t,1},'b_spline'))end
      for _,t in ipairs({{}, {0,1,extra=1},setmetatable({0,1},{})})do assert(not pcall(g.spline_points,p,t,'catmull_rom'))end
      for _,q in ipairs({{x={},y={}},{x={1},y={1,2}},{x={1},y={1},count=2},{x={math.huge},y={0}},{x={1e13},y={0}},{x={0/0},y={0}},setmetatable({x={1},y={1}},{})})do assert(not pcall(g.spline_point,q,0.5,'b_spline'))end
      for _,n in ipairs({0,-1,4096,1.5,2.0,'2',false})do assert(not pcall(g.sample_spline,p,{kind='b_spline',steps=n}))end
      for _,o in ipairs({{kind='b_spline',extra=true},setmetatable({kind='b_spline'},{})})do assert(not pcall(g.sample_spline,p,o))end
      local extreme={x={0,1e12,1e12,0},y={0,0,0,0}}
      local ok,e=pcall(g.spline_point,extreme,0.5,'catmull_rom');assert(not ok and tostring(e):find('Spline output'))
      assert(not pcall(g.spline_points,extreme,{0,0.5,1},'catmull_rom'))
      assert(extreme.x[2]==1e12 and p.x[1]==-7)
      assert(g.sample_spline(p,{kind='b_spline',steps=2}).count==3)
    "#,
    );
    clear(&h);
}

#[test]
fn spline_native_preflight_and_mid_evaluation_cancellation_release_every_buffer() {
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(10));
    for s in [
        "plum.geometry.spline_point({x={1,2},y={3,4}},0.5,'catmull_rom')",
        "plum.geometry.sample_spline({x={1,2},y={3,4}},{kind='b_spline',steps=4095})",
    ] {
        assert_eq!(
            h.run("spline-quota.lua", s).unwrap_err().category,
            Category::Budget
        );
        clear(&h);
    }
    h.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled_spline",
            h.lua
                .create_function(move |lua, t: Table| {
                    let mut calls = 0;
                    evaluate_controlled(
                        lua,
                        &t,
                        Kind::CatmullRom,
                        4096,
                        |i| Ok(i as f64 / 4095.),
                        || {
                            calls += 1;
                            if calls == 4 {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            crate::lua::notes::check_budget(lua)
                        },
                    )
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run(
            "spline-cancel.lua",
            "controlled_spline{x={1,5,20,7},y={3,9,1,-2}}"
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    clear(&h);
    run(
        &h,
        "assert(plum.geometry.sample_spline({x={1,2},y={3,4}},{kind='b_spline'}).count==33)",
    );
    clear(&h);
}

#[test]
fn actual_vm_output_refusal_preserves_spline_inputs_for_retry() {
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
    run(
        &h,
        r#"
      local p={x={1,5,20,7},y={3,9,1,-2}};local opts={kind='catmull_rom',steps=4095}
      limit_vm();local ok=pcall(plum.geometry.sample_spline,p,opts);restore_vm();assert(not ok and p.x[3]==20)
      local out=plum.geometry.sample_spline(p,opts);assert(out.count==4096 and out.x[1]==1 and out.x[4096]==7)
    "#,
    );
    clear(&h);
}

#[test]
fn published_authored_spline_melody_has_preview_apply_one_undo_redo_in_both_key_modes() {
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    use std::sync::Arc;
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("### Native spline evaluation (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut d = PlumDocument::new_project("Spline example".into(), Bpm::DEFAULT, 960);
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
        h.run("spline-preview.lua", source).unwrap();
        let preview = h.take_commits().unwrap();
        let count = preview.result().note_count();
        assert_eq!(count, 25);
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        bind(&h, &doc, &history);
        h.run("spline-apply.lua", source).unwrap();
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
