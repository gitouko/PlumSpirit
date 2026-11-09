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
    h.run("curve-path.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
const CONTROL: &str = "{x={-7,30,-40,12},y={20,-13,60,-4}}";

#[test]
fn cubic_evaluation_matches_independent_de_casteljau_and_keeps_arbitrary_domains() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry
      local function reference(c,t)
        local x,y={},{};for i=1,4 do x[i]=c.x[i];y[i]=c.y[i]end
        for n=3,1,-1 do for i=1,n do x[i]=(1-t)*x[i]+t*x[i+1];y[i]=(1-t)*y[i]+t*y[i+1]end end
        return x[1],y[1]
      end
      for _,c in ipairs({{x={-7,30,-40,12},y={20,-13,60,-4}}, {x={-1e12,1e12,1e12,-1e12},y={1e12,-1e12,1e12,-1e12}}, {x={1e12,1e12,1e12,1e12},y={-1e12,-1e12,-1e12,-1e12}}})do
        local scale=1;for j=1,4 do scale=math.max(scale,math.abs(c.x[j]),math.abs(c.y[j]))end
        local tolerance=64*2.220446049250313e-16*scale
        for i=0,100 do local t=i/100;local x,y=g.cubic_point(c,t);local rx,ry=reference(c,t);assert(math.abs(x-rx)<=tolerance);assert(math.abs(y-ry)<=tolerance)end
        local x,y=g.cubic_point(c,0);assert(x==c.x[1] and y==c.y[1]);x,y=g.cubic_point(c,1);assert(x==c.x[4] and y==c.y[4])
      end
      local c={x={-0.0,1,2,3},y={-0.0,1,2,3}};local x,y=g.cubic_point(c,0);assert(1/x==-math.huge and 1/y==-math.huge)
    "#,
    );
    clear(&h);
}

#[test]
fn batch_and_uniform_sampling_preserve_order_endpoints_composition_and_maximum() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local c={x={-7,30,-40,12},y={20,-13,60,-4},count=4}
      local ts={1,0,0.5,0.25,1};local b=g.cubic_points(c,ts);assert(b.count==5)
      for i,t in ipairs(ts)do local x,y=g.cubic_point(c,t);assert(b.x[i]==x and b.y[i]==y)end
      assert(c.x[1]==-7 and ts[1]==1)
      local s=g.sample_cubic(c,16);assert(s.count==17 and s.x[1]==-7 and s.x[17]==12 and s.y[17]==-4)
      for i=1,17 do local x,y=g.cubic_point(c,(i-1)/16);assert(s.x[i]==x and s.y[i]==y)end
      local p=g.map_points(g.mapping{translate_x=50,scale_y=-1},s);assert(p.x[1]==43 and p.y[1]==-20)
      local res=g.resample_polyline(p,32);assert(res.count==33 and res.x[1]==43 and res.x[33]==62)
      local max=g.sample_cubic(c,4095);assert(max.count==4096 and max.x[4096]==12)
      local ts={};for i=1,4096 do ts[i]=(4096-i)/4095 end;local b=g.cubic_points(c,ts);assert(b.count==4096 and b.x[1]==12 and b.x[4096]==-7)
    "#,
    );
    clear(&h);
}

#[test]
fn mixed_paths_share_joints_close_explicitly_and_preserve_degenerate_geometry() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry
      local path={start={x=-10,y=20},segments={{kind='line',x=0,y=20},{kind='cubic',x1=0,y1=30,x2=10,y2=30,x=10,y=20},{kind='line',x=10,y=10}},closed=true}
      local p=g.sample_path(path,{curve_steps=4});assert(p.count==8)
      assert(p.x[1]==-10 and p.y[1]==20 and p.x[2]==0 and p.y[2]==20)
      assert(p.x[6]==10 and p.y[6]==20 and p.x[7]==10 and p.y[7]==10 and p.x[8]==-10 and p.y[8]==20)
      local c={x={0,0,10,10},y={20,30,30,20}};for i=1,4 do local x,y=g.cubic_point(c,i/4);assert(p.x[i+2]==x and p.y[i+2]==y)end
      assert(path.closed and path.segments[1].x==0 and g.polyline_length(p)>0)
      path.closed=false;assert(g.sample_path(path,{curve_steps=4}).count==7)
      assert(g.sample_path({start={x=12,y=-4},segments={},closed=true}).count==1)
      local flat=g.sample_path({start={x=7,y=7},segments={{kind='line',x=7,y=7},{kind='cubic',x1=7,y1=7,x2=7,y2=7,x=7,y=7}},closed=true},{curve_steps=4})
      assert(flat.count==6 and g.polyline_length(flat)==0)
      local p=g.sample_path({start={x=0,y=0},segments={{kind='cubic',x1=0,y1=0,x2=1,y2=1,x=1,y=1}}});assert(p.count==33)
    "#,
    );
    clear(&h);
}

#[test]
fn maximum_path_segments_points_and_closure_are_preflighted() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;assert(g.max_path_segments==256)
      local segments={};for i=1,256 do segments[i]={kind='line',x=i,y=-i}end
      local path={start={x=0,y=0},segments=segments};local p=g.sample_path(path);assert(p.count==257 and p.x[257]==256)
      segments[257]={kind='line',x=257,y=0};assert(not pcall(g.sample_path,path));segments[257]=nil
      local curve={kind='cubic',x1=0,y1=1,x2=1,y2=0,x=1,y=1};local path={start={x=0,y=0},segments={curve}}
      local p=g.sample_path(path,{curve_steps=4095});assert(p.count==4096 and p.x[4096]==1)
      path.closed=true;local ok,e=pcall(g.sample_path,path,{curve_steps=4095});assert(not ok and tostring(e):find('Closing'))
      curve.x=0;curve.y=0;assert(g.sample_path(path,{curve_steps=4095}).count==4096)
      path.closed=false;path.segments={curve,curve};ok,e=pcall(g.sample_path,path,{curve_steps=2048});assert(not ok and tostring(e):find('4096'))
    "#,
    );
    clear(&h);
}

#[test]
fn malformed_curve_path_inputs_and_parameter_domains_fail_without_output() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local c={x={0,0,1,1},y={0,1,0,1}}
      for _,t in ipairs({-1,1.1,0/0,math.huge,'0.5',false,{}})do assert(not pcall(g.cubic_point,c,t));assert(not pcall(g.cubic_points,c,{0,t,1}))end
      for _,n in ipairs({0,-1,4096,1.5,'2',2.0})do assert(not pcall(g.sample_cubic,c,n))end
      for _,p in ipairs({{x={0,1},y={0,1}}, {x={0,1,2,3},y={0,1,2,3},count=3}, {x={0,1,2,math.huge},y={0,1,2,3}}, {x={0,1,2,3},y={0,1,2,1e13}}, setmetatable(c,{})})do assert(not pcall(g.cubic_point,p,0.5))end
      setmetatable(c,nil)
      assert(not pcall(g.cubic_points,c,{0,1,extra=2}));assert(not pcall(g.cubic_points,c,setmetatable({0,1},{})));assert(not pcall(g.cubic_points,c,{}))
      local start={x=0,y=0};local line={kind='line',x=1,y=1}
      for _,p in ipairs({{start=start,segments={line},closed=1},{start=start,segments={{kind='spline',x=1,y=1}}},{start=start,segments={{kind='line',x=1,y=1,x1=2}}},{start=start,segments={{kind='cubic',x=1,y=1}}},{start=start,segments={line,extra=1}},{start=start,segments=setmetatable({line},{})},setmetatable({start=start,segments={}},{})})do assert(not pcall(g.sample_path,p))end
      local p={start=start,segments={line}};assert(not pcall(g.sample_path,p,{tolerance=0.01}));assert(not pcall(g.sample_path,p,{curve_steps=0}));assert(not pcall(g.sample_path,p,setmetatable({},{})))
      local cycle={kind='line',y=0};cycle.x=cycle;assert(not pcall(g.sample_path,{start=start,segments={cycle}}))
    "#,
    );
    clear(&h);
}

#[test]
fn native_quota_and_internal_sampling_cancellation_release_all_buffers() {
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(10));
    for s in [
        format!("plum.geometry.cubic_point({CONTROL},0.5)"),
        format!("plum.geometry.sample_cubic({CONTROL},4095)"),
        "plum.geometry.sample_path{start={x=0,y=0},segments={{kind='line',x=1,y=1}}}".into(),
    ] {
        let e = h.run("quota.lua", &s).unwrap_err();
        assert_eq!(e.category, Category::Budget);
        clear(&h);
    }
    h.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled_curve",
            h.lua
                .create_function(move |lua, t: Table| {
                    let mut calls = 0;
                    sample_uniform_controlled(lua, &t, 4095, || {
                        calls += 1;
                        if calls == 3 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        crate::lua::notes::check_budget(lua)
                    })
                })
                .unwrap(),
        )
        .unwrap();
    let e = h
        .run("cancel-inside.lua", &format!("controlled_curve({CONTROL})"))
        .unwrap_err();
    assert_eq!(e.category, Category::Canceled);
    clear(&h);
    run(
        &h,
        &format!("assert(plum.geometry.sample_cubic({CONTROL},16).count==17)"),
    );
    clear(&h);
}
