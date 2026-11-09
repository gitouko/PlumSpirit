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
    h.run("adaptive.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
const C: &str = "{x={0,1,2,3},y={0,2,-2,0}}";
#[test]
fn adaptive_lua_outputs_are_bounded_composable_immutable_and_deterministic() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;assert(g.max_flatten_depth==24)
      local c={x={0,1,2,3},y={0,2,-2,0}};local opts={tolerance=0.001,max_points=4096,max_depth=24}
      local p=g.flatten_cubic(c,opts);assert(p.count>2 and p.count<=4096 and p.x[1]==0 and p.x[p.count]==3 and p.y[p.count]==0)
      local q=g.flatten_cubic(c,opts);for i=1,p.count do assert(p.x[i]==q.x[i] and p.y[i]==q.y[i])end
      assert(g.resample_polyline(p,32).count==33);assert(c.y[2]==2 and opts.tolerance==0.001)
      assert(g.flatten_cubic({x={1,1,1,1},y={2,2,2,2}},{tolerance=0.01,max_points=2,max_depth=0}).count==2)
      local back=g.flatten_cubic({x={0,10,-10,1},y={0,0,0,0}},{tolerance=0.001});assert(back.count>2)
    "#,
    );
    clear(&h);
}
#[test]
fn adaptive_limits_and_malformed_options_fail_explicitly_and_release_quota() {
    let h = host();
    run(
        &h,
        &format!(
            r#"
      local g=plum.geometry;local c={C}
      for _,opts in ipairs({{{{tolerance=0}},{{tolerance=-1}},{{tolerance=0/0}},{{tolerance=math.huge}},{{tolerance='0.1'}},{{}},{{tolerance=0.1,extra=true}},setmetatable({{tolerance=0.1}},{{}}),{{tolerance=0.1,max_points=1}},{{tolerance=0.1,max_points=4097}},{{tolerance=0.1,max_points=2.0}},{{tolerance=0.1,max_depth=25}},{{tolerance=0.1,max_depth=-1}}}})do assert(not pcall(g.flatten_cubic,c,opts))end
      local ok,e=pcall(g.flatten_cubic,c,{{tolerance=0.001,max_points=2}});assert(not ok and tostring(e):find('max_points'))
      ok,e=pcall(g.flatten_cubic,c,{{tolerance=0.001,max_depth=0}});assert(not ok and tostring(e):find('max_depth'))
      ok,e=pcall(g.flatten_cubic,c,{{tolerance=1e-20}});assert(not ok and tostring(e):find('precision'))
    "#
        ),
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(100));
    assert_eq!(
        h.run(
            "quota.lua",
            &format!("plum.geometry.flatten_cubic({C},{{tolerance=0.01}})")
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
    // Input fits; output/workspace preflight fails before native allocation.
    h.lua.set_app_data(NativeBudget::isolated(2048));
    assert_eq!(
        h.run(
            "quota2.lua",
            &format!("plum.geometry.flatten_cubic({C},{{tolerance=0.01}})")
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
}
#[test]
fn adaptive_cancellation_inside_native_subdivision_discards_output_and_all_reservations() {
    let h = host();
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled",
            h.lua
                .create_function(move |lua, c: Table| {
                    let opts = lua.create_table()?;
                    opts.raw_set("tolerance", 0.000001)?;
                    let mut checks = 0;
                    flatten_controlled(lua, &c, &opts, || {
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
    assert_eq!(
        h.run("cancel.lua", &format!("controlled({C})"))
            .unwrap_err()
            .category,
        Category::Canceled
    );
    clear(&h);
    run(
        &h,
        &format!("assert(plum.geometry.flatten_cubic({C},{{tolerance=0.01}}).count>2)"),
    );
    clear(&h);
}
