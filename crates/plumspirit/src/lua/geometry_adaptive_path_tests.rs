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
fn run(h: &Host, s: &str) {
    h.run("adaptive-path.lua", s)
        .unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn adaptive_mixed_paths_equal_independent_segment_composition_without_duplicate_joints() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry
      local path={start={x=-4,y=2},closed=true,segments={
        {kind='line',x=0,y=0},
        {kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0},
        {kind='line',x=-1,y=-2},
        {kind='cubic',x1=2,y1=5,x2=-3,y2=4,x=-1,y=-2},
        {kind='line',x=-2,y=1}}}
      local opts={tolerance=0.001};local actual=g.flatten_path(path,opts)
      local expected={x={path.start.x},y={path.start.y},count=1}
      local x,y=path.start.x,path.start.y
      local function append(a,b)expected.count=expected.count+1;expected.x[expected.count]=a;expected.y[expected.count]=b end
      for _,s in ipairs(path.segments)do
        if s.kind=='line' then append(s.x,s.y)
        else local c=g.flatten_cubic({x={x,s.x1,s.x2,s.x},y={y,s.y1,s.y2,s.y}},opts)
          for i=2,c.count do append(c.x[i],c.y[i])end
        end
        x,y=s.x,s.y
      end
      append(path.start.x,path.start.y)
      assert(actual.count==expected.count)
      local replay=g.flatten_path(path,opts)
      for i=1,actual.count do assert(actual.x[i]==expected.x[i] and actual.y[i]==expected.y[i]);assert(actual.x[i]==replay.x[i] and actual.y[i]==replay.y[i])end
      assert(actual.x[1]==-4 and actual.y[1]==2 and actual.x[actual.count]==-4 and actual.y[actual.count]==2)
      assert(g.resample_polyline(actual,32).count==33 and g.polyline_length(actual)>0)
      -- Independent dense de Casteljau evaluation against the assembled path.
      x,y=path.start.x,path.start.y
      for _,s in ipairs(path.segments)do
        if s.kind=='cubic' then
          for j=0,64 do
            local t=j/64;local xs={x,s.x1,s.x2,s.x};local ys={y,s.y1,s.y2,s.y}
            for n=3,1,-1 do for i=1,n do xs[i]=(1-t)*xs[i]+t*xs[i+1];ys[i]=(1-t)*ys[i]+t*ys[i+1]end end
            assert(g.nearest_polyline(actual,xs[1],ys[1]).distance<=opts.tolerance+1e-12)
          end
        end
        x,y=s.x,s.y
      end
      assert(path.start.x==-4 and path.segments[2].x1==1 and opts.tolerance==0.001)
      actual.x[2]=99;assert(path.segments[1].x==0 and replay.x[2]==0)
    "#,
    );
    clear(&h);
}
#[test]
fn adaptive_empty_lines_degenerates_and_closure_keep_exact_authored_endpoints() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local opts={tolerance=0.001,max_points=1,max_depth=0}
      local p=g.flatten_path({start={x=-0.0,y=2},segments={},closed=true},opts);assert(p.count==1 and 1/p.x[1]==-math.huge and p.y[1]==2)
      local path={start={x=-3,y=7},segments={{kind='line',x=-3,y=7},{kind='cubic',x1=-3,y1=7,x2=-3,y2=7,x=-3,y=7}},closed=true}
      p=g.flatten_path(path,{tolerance=0.001,max_points=3,max_depth=0});assert(p.count==3 and g.polyline_length(p)==0)
      path={start={x=0,y=0},closed=true,segments={{kind='line',x=1,y=2}}}
      p=g.flatten_path(path,{tolerance=1e-100,max_points=3,max_depth=0});assert(p.count==3 and p.x[2]==1 and p.y[2]==2 and p.x[3]==0)
      path.segments[2]={kind='line',x=0,y=0};assert(g.flatten_path(path,{tolerance=0.001,max_points=3}).count==3)
      path.closed=false;assert(g.flatten_path(path,{tolerance=0.001}).count==3)
      local straight={start={x=0,y=0},segments={{kind='cubic',x1=1,y1=1,x2=2,y2=2,x=3,y=3}}}
      p=g.flatten_path(straight,{tolerance=0.001,max_depth=0,max_points=2});assert(p.count==2 and p.x[2]==3)
    "#,
    );
    clear(&h);
}
#[test]
fn whole_path_point_caps_reserve_suffix_endpoints_closure_and_multiple_cubics() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local tolerance=0.001
      local a={x={0,1,2,3},y={0,2,-2,0}};local b={x={3,4,5,6},y={0,2,-2,0}}
      local na=g.flatten_cubic(a,{tolerance=tolerance}).count;local nb=g.flatten_cubic(b,{tolerance=tolerance}).count
      local path={start={x=0,y=0},closed=true,segments={{kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0},{kind='cubic',x1=4,y1=2,x2=5,y2=-2,x=6,y=0},{kind='line',x=7,y=-1}}}
      local required=na+nb+1
      local p=g.flatten_path(path,{tolerance=tolerance,max_points=required});assert(p.count==required and p.x[na]==3 and p.x[na+nb-1]==6 and p.x[p.count-1]==7 and p.x[p.count]==0)
      local ok,e=pcall(g.flatten_path,path,{tolerance=tolerance,max_points=required-1});assert(not ok and tostring(e):find('max_points'))
      local loop={kind='cubic',x1=2,y1=3,x2=-2,y2=3,x=0,y=0}
      local c=g.flatten_cubic({x={0,2,-2,0},y={0,3,3,0}},{tolerance=0.00001})
      assert(4*(c.count-1)+1>4096)
      path={start={x=0,y=0},segments={loop,loop,loop},closed=true}
      required=3*(c.count-1)+1;assert(g.flatten_path(path,{tolerance=0.00001,max_points=required}).count==required)
      path.segments[4]=loop;ok,e=pcall(g.flatten_path,path,{tolerance=0.00001});assert(not ok and tostring(e):find('max_points'))
      path={start={x=0,y=0},closed=true,segments={{kind='line',x=1,y=1}}}
      ok,e=pcall(g.flatten_path,path,{tolerance=0.1,max_points=2});assert(not ok and tostring(e):find('Closing'))
    "#,
    );
    clear(&h);
}
#[test]
fn adaptive_path_segment_limits_and_fixed_sampler_regression_are_preserved() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local path={start={x=0,y=0},segments={}}
      for i=1,256 do path.segments[i]={kind='line',x=i,y=-i}end
      assert(g.flatten_path(path,{tolerance=0.001,max_points=257,max_depth=0}).count==257)
      path.closed=true;assert(g.flatten_path(path,{tolerance=0.001,max_points=258,max_depth=0}).count==258)
      path.segments[257]={kind='line',x=0,y=0};assert(not pcall(g.flatten_path,path,{tolerance=0.001}));path.segments[257]=nil;path.closed=false
      for i=1,256 do path.segments[i]={kind='cubic',x1=i-0.75,y1=0,x2=i-0.25,y2=0,x=i,y=0}end
      local p=g.flatten_path(path,{tolerance=0.001,max_points=257,max_depth=0});assert(p.count==257 and p.x[257]==256)
      local fixed=g.sample_path(path,{curve_steps=1});assert(fixed.count==p.count);for i=1,p.count do assert(fixed.x[i]==p.x[i] and fixed.y[i]==p.y[i])end
      local path={start={x=0,y=0},segments={{kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0}}}
      local fixed=g.sample_path(path,{curve_steps=4});assert(fixed.count==5)
      for i=1,5 do local x,y=g.cubic_point({x={0,1,2,3},y={0,2,-2,0}},(i-1)/4);assert(fixed.x[i]==x and fixed.y[i]==y)end
    "#,
    );
    clear(&h);
}
#[test]
fn malformed_adaptive_paths_and_options_fail_without_mutating_inputs() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local start={x=0,y=0};local line={kind='line',x=1,y=1};local path={start=start,segments={line}}
      for _,opts in ipairs({{}, {tolerance=0},{tolerance=-1},{tolerance=math.huge},{tolerance=0/0},{tolerance='0.1'},{tolerance=0.1,curve_steps=32},{tolerance=0.1,max_points=0},{tolerance=0.1,max_points=4097},{tolerance=0.1,max_points=2.0},{tolerance=0.1,max_depth=25},{tolerance=0.1,max_depth=0.0},setmetatable({tolerance=0.1},{})})do assert(not pcall(g.flatten_path,path,opts))end
      for _,p in ipairs({{start=start,segments={line},closed=1},{start=start,segments={{kind='spline',x=1,y=1}}},{start=start,segments={{kind='line',x=1,y=1,x1=2}}},{start=start,segments={{kind='cubic',x=1,y=1}}},{start=start,segments={{kind='line',x=math.huge,y=0}}},{start={x=1e13,y=0},segments={}},{start=start,segments={line,extra=1}},{start=start,segments=setmetatable({line},{})},setmetatable({start=start,segments={}},{})})do assert(not pcall(g.flatten_path,p,{tolerance=0.01}))end
      local curve={start=start,segments={{kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0}}}
      local ok,e=pcall(g.flatten_path,curve,{tolerance=0.001,max_depth=0});assert(not ok and tostring(e):find('max_depth'))
      ok,e=pcall(g.flatten_path,curve,{tolerance=1e-20});assert(not ok and tostring(e):find('precision'))
      assert(start.x==0 and line.x==1 and path.segments[1]==line)
    "#,
    );
    clear(&h);
}
#[test]
fn adaptive_path_native_preflight_and_late_failure_release_all_private_buffers() {
    let h = host();
    for budget in [10, 2048] {
        h.lua.set_app_data(NativeBudget::isolated(budget));
        let e=h.run("quota-path.lua","plum.geometry.flatten_path({start={x=0,y=0},segments={{kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0}}},{tolerance=0.001})").unwrap_err();
        assert_eq!(e.category, Category::Budget);
        clear(&h);
    }
    h.lua.set_app_data(NativeBudget::isolated(4096));
    run(
        &h,
        "assert(plum.geometry.flatten_path({start={x=0,y=0},segments={}},{tolerance=0.001}).count==1)",
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    run(&h, "saved='unchanged'");
    let e=h.run("late-failure.lua","saved=plum.geometry.flatten_path({start={x=0,y=0},segments={{kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0},{kind='cubic',x1=1e12,y1=1e12,x2=1e12,y2=1e12,x=1e12,y=1e12}}},{tolerance=0.001})").unwrap_err();
    assert_eq!(e.category, Category::Runtime);
    clear(&h);
    run(
        &h,
        "assert(saved=='unchanged');assert(plum.geometry.flatten_path({start={x=0,y=0},segments={{kind='line',x=1,y=1}}},{tolerance=0.001}).count==2)",
    );
    clear(&h);
}
#[test]
fn adaptive_path_internal_cancellation_discards_native_subdivision_copy_and_line_prefixes() {
    // Determine native check count so the second case cancels after64 copied samples.
    let mut native_checks = 0;
    let p = native::flatten_cubic(
        [(0., 0.), (1., 2.), (2., -2.), (3., 0.)],
        0.000001,
        4096,
        24,
        || {
            native_checks += 1;
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    assert!(p.len() > 65);
    for (phase, target, lines) in [
        ("subdivision", 5, false),
        ("copy", native_checks + 4, false),
        ("line-prefix", 66, true),
    ] {
        let h = host();
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled",
                h.lua
                    .create_function(move |lua, t: Table| {
                        let opts = lua.create_table()?;
                        opts.raw_set("tolerance", 0.000001)?;
                        let mut calls = 0;
                        let result = flatten_path_controlled(lua, &t, &opts, || {
                            calls += 1;
                            if calls == target {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            crate::lua::notes::check_budget(lua)
                        });
                        assert_eq!(calls, target, "cancel phase {phase}");
                        result
                    })
                    .unwrap(),
            )
            .unwrap();
        let source = if lines {
            "local p={start={x=0,y=0},segments={}};for i=1,256 do p.segments[i]={kind='line',x=i,y=-i}end;controlled(p)"
        } else {
            "controlled({start={x=0,y=0},segments={{kind='cubic',x1=1,y1=2,x2=2,y2=-2,x=3,y=0}}})"
        };
        assert_eq!(
            h.run(phase, source).unwrap_err().category,
            Category::Canceled,
            "{phase}"
        );
        clear(&h);
        run(
            &h,
            "assert(plum.geometry.flatten_path({start={x=0,y=0},segments={}},{tolerance=0.1}).count==1)",
        );
        clear(&h);
    }
}
