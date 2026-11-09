use super::*;
use crate::lua::{Host, Limits};
fn host() -> Host {
    let h = Host::new(Limits::default(), []).unwrap();
    h.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    h
}
fn run(h: &Host, s: &str) {
    h.run("predicates.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn polygon_classification_has_explicit_boundaries_concavity_and_native_fill_rules() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local box={x={0,4,4,0},y={0,0,4,4}}
      for _,q in ipairs({{0,0},{4,4},{0,2},{2,0},{4,2},{2,4}})do assert(g.polygon_classify(box,q[1],q[2])=='boundary')end
      assert(g.polygon_classify(box,2,2)=='inside' and g.polygon_classify(box,-1,2)=='outside')
      assert(g.polygon_classify(box,-0.01,2,{epsilon=0.02})=='boundary')
      assert(g.polygon_classify(box,-0.03,2,{epsilon=0.02})=='outside')
      local concave={x={0,4,4,2,2,0},y={0,0,2,2,4,4}}
      assert(g.polygon_classify(concave,1,3)=='inside' and g.polygon_classify(concave,3,3)=='outside')
      local twice={x={0,4,4,0,0,4,4,0},y={0,0,4,4,0,0,4,4}}
      assert(g.polygon_classify(twice,2,2)=='outside' and g.polygon_classify(twice,2,2,{rule='non_zero'})=='inside')
      local reverse={x={0,0,4,4},y={0,4,4,0}};assert(g.polygon_classify(reverse,2,2,{rule='non_zero'})=='inside')
      local repeated={x={0,0,4,4,0,0},y={0,0,0,4,4,0}};assert(g.polygon_classify(repeated,2,2)=='inside')
      local degenerate={x={1,1,1},y={2,2,2}};assert(g.polygon_classify(degenerate,1,2)=='boundary' and g.polygon_classify(degenerate,1,3)=='outside')
      local tiny={x={0,1e-200,0},y={0,1e-200,1e-200}};assert(g.polygon_classify(tiny,0.1e-200,0.5e-200)=='inside')
      assert(box.x[1]==0 and box.y[4]==4)
    "#,
    );
    clear(&h);
}
#[test]
fn nearest_polyline_returns_finite_segment_parameters_closure_and_stable_ties() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={0,4,4},y={0,0,4}}
      local hit=g.nearest_polyline(p,2,1);assert(hit.segment==1 and hit.t==0.5 and hit.x==2 and hit.y==0 and hit.distance==1)
      hit=g.nearest_polyline(p,3,2);assert(hit.segment==2 and hit.t==0.5 and hit.x==4 and hit.y==2 and hit.distance==1)
      hit=g.nearest_polyline(p,2,2,{closed=true});assert(hit.segment==3 and math.abs(hit.distance)<1e-14)
      hit=g.nearest_polyline({x={2},y={2}},5,6);assert(hit.distance==5 and hit.segment==1 and hit.t==0)
      hit=g.nearest_polyline({x={0,0,4},y={0,0,0}},0,1);assert(hit.segment==1 and hit.distance==1)
      hit=g.nearest_polyline({x={0,1e-200},y={0,0}},0.5e-200,1e-200);assert(hit.t==0.5 and hit.distance==1e-200)
      assert(p.x[2]==4)
    "#,
    );
    clear(&h);
}
#[test]
fn predicate_arguments_limits_and_native_quota_fail_safely() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={0,1,1},y={0,0,1}}
      for _,opts in ipairs({{rule='other'},{rule=1},{epsilon=-1},{epsilon=math.huge},{epsilon='1'},{extra=true},setmetatable({},{})})do assert(not pcall(g.polygon_classify,p,0,0,opts))end
      assert(not pcall(g.polygon_classify,{x={0,1},y={0,1}},0,0))
      assert(not pcall(g.nearest_polyline,p,0,0,{closed=1}));assert(not pcall(g.nearest_polyline,p,0,0,{epsilon=0}))
      for _,fn in ipairs({g.nearest_polyline,g.polygon_classify})do
        assert(not pcall(fn,p,math.huge,0));assert(not pcall(fn,p,0,0/0));assert(not pcall(fn,p,1e13,0));assert(not pcall(fn,{x={0,1},y={0,0,1}},0,0))
        local big={x={},y={}};for i=1,4096 do big.x[i]=i;big.y[i]=i%2 end;assert(fn(big,-10,-10));big.x[4097]=0;big.y[4097]=0;assert(not pcall(fn,big,0,0))
      end
    "#,
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(10));
    for f in ["polygon_classify", "nearest_polyline"] {
        let e = h
            .run(
                "quota.lua",
                &format!("plum.geometry.{f}({{x={{0,1,0}},y={{0,0,1}}}},0,0)"),
            )
            .unwrap_err();
        assert_eq!(e.category, Category::Budget);
        clear(&h);
    }
}
#[test]
fn polygon_scan_cancellation_releases_input_and_returns_no_partial_classification() {
    let h = host();
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled",
            h.lua
                .create_function(move |lua, t: Table| {
                    let mut checks = 0;
                    classify_controlled(
                        lua,
                        &t,
                        Value::Integer(-10),
                        Value::Integer(-10),
                        None,
                        || {
                            checks += 1;
                            if checks == 3 {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            crate::lua::notes::check_budget(lua)
                        },
                    )
                })
                .unwrap(),
        )
        .unwrap();
    let e = h
        .run(
            "cancel.lua",
            "local p={x={},y={}};for i=1,4096 do p.x[i]=i;p.y[i]=i%2 end;controlled(p)",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Canceled);
    clear(&h);
    run(
        &h,
        "assert(plum.geometry.polygon_classify({x={0,1,0},y={0,0,1}},-1,-1)=='outside')",
    );
    clear(&h);
}
#[test]
fn nearest_scan_cancellation_discards_partial_best_hit_and_releases_native_input() {
    let h = host();
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled",
            h.lua
                .create_function(move |lua, t: Table| {
                    let mut checks = 0;
                    nearest_controlled(lua, &t, (-10., -10.), true, || {
                        checks += 1;
                        if checks == 3 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        crate::lua::notes::check_budget(lua)
                    })
                })
                .unwrap(),
        )
        .unwrap();
    let e = h
        .run(
            "cancel-nearest.lua",
            "local p={x={},y={}};for i=1,4096 do p.x[i]=i;p.y[i]=i%2 end;controlled(p)",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Canceled);
    clear(&h);
    run(
        &h,
        "assert(plum.geometry.nearest_polyline({x={0},y={0}},3,4).distance==5)",
    );
    clear(&h);
}
