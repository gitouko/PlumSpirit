use super::*;
use crate::lua::{Limits, Plain};
fn input(points: &[(u64, f64, f64)], shift: i128) -> Arc<Captured> {
    Captured::new(
        points,
        1,
        None,
        shift,
        256,
        960,
        120,
        [true, false, true, false],
    )
    .unwrap()
}
fn host(grant: bool, budget: usize) -> Host {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        if grant {
            vec![Capability::DrawingTools]
        } else {
            vec![]
        },
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(budget));
    h.lua
        .globals()
        .set(
            "native_stroke",
            h.lua
                .create_function(|lua, (track, content, keys): (u32, Option<u32>, u16)| {
                    for_tool(lua, track, content, keys)
                })
                .unwrap(),
        )
        .unwrap();
    h
}
#[test]
fn exact_native_samples_relative_coordinates_and_independent_batches() {
    let h = host(true, 4 * 1024 * 1024);
    let base = 9_007_199_254_741_002;
    let s = input(
        &[
            (base, 128., 0.),
            (base + 1, 130., 0.5),
            (base + 2, 129., 1.),
        ],
        -i128::from(base) - 1,
    );
    set_input(&h.lua, Some(s));
    let result = h
        .run(
            "stroke.lua",
            r#"
      kept=native_stroke(1,nil,256)
      local i=kept:info();assert(i.count==3 and i.space=='target_local' and i.keys==256)
      assert(i.ppq==960 and i.snap==120 and i.shift and i.alt and not i.pressure_supported)
      local a=kept:point(1);local b=kept:point(2);local c=kept:point(3)
      assert(math.type(a.tick)=='integer' and b.tick-a.tick==1 and c.tick-b.tick==1)
      assert(a.song_tick==nil and a.song_tick_text=='-1' and b.song_tick==0 and c.song_tick==1)
      assert(a.x==0 and b.x==.5 and c.x==1 and a.y==.5)
      assert(a.start and not a.finish and c.finish and not c.start)
      assert(math.abs(b.speed-math.sqrt(4+(1/960)^2)/.5)<1e-12)
      local batch=kept:batch(1,3);batch[2].pitch=9
      assert(kept:point(2).pitch==130 and kept:batch(2,1)[1].pitch==130)
      for _,f in ipairs{function()kept:point(0)end,function()kept:point(1.0)end,
        function()kept:batch(1,1025)end,function()kept:batch(3,2)end}do assert(not pcall(f))end
      return 'exact'
    "#,
        )
        .unwrap();
    assert_eq!(result, Plain::String("exact".into()));
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    h.run(
        "expired.lua",
        "assert(not pcall(function()kept:point(1)end));kept=nil;collectgarbage('collect')",
    )
    .unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn native_capture_limits_and_nonfinite_or_unordered_samples_are_refused() {
    for points in [
        vec![],
        vec![(0, f64::NAN, 0.)],
        vec![(0, f64::INFINITY, 0.)],
        vec![(0, 4097., 0.)],
        vec![(0, 1., -1.)],
        vec![(0, 1., f64::NAN)],
        vec![(0, 1., 1.), (1, 2., 0.)],
        vec![(u64::MAX, 1., 0.)],
        vec![(0, 1., 86401.)],
    ] {
        assert!(Captured::new(&points, 1, None, 0, 128, 960, 120, [false; 4]).is_err());
    }
    let points = vec![(10, 60., 0.); MAX_STROKE_POINTS];
    assert_eq!(input(&points, 0).stroke.points.len(), MAX_STROKE_POINTS);
    let mut too_many = points;
    too_many.push((10, 60., 0.));
    assert!(Captured::new(&too_many, 1, None, 0, 128, 960, 120, [false; 4]).is_err());
    for (keys, ppq, snap, shift) in [
        (127, 960, 120, 0),
        (128, 0, 120, 0),
        (256, 960, 0, 0),
        (128, 960, 120, i128::MIN),
    ] {
        assert!(
            Captured::new(&[(0, 1., 0.)], 1, None, shift, keys, ppq, snap, [false; 4]).is_err()
        );
    }
}
#[test]
fn permission_native_target_identity_close_and_quota_are_enforced() {
    let h = host(false, 4 * 1024 * 1024);
    set_input(&h.lua, Some(input(&[(0, 1., 0.)], 0)));
    assert_eq!(
        h.run("denied.lua", "native_stroke(1,nil,256)")
            .unwrap_err()
            .category,
        Category::Capability
    );
    let h = host(true, 4 * 1024 * 1024);
    for source in [
        "native_stroke(2,nil,256)",
        "native_stroke(1,1,256)",
        "native_stroke(1,nil,128)",
    ] {
        set_input(&h.lua, Some(input(&[(0, 1., 0.)], 0)));
        assert_eq!(
            h.run("stale.lua", source).unwrap_err().category,
            Category::Expired
        );
    }
    set_input(&h.lua, Some(input(&[(0, 1., 0.)], 0)));
    h.run("close.lua","local s=native_stroke(1,nil,256);assert(s:close());assert(not s:close());assert(not pcall(function()s:info()end));assert(not pcall(function()native_stroke(1,nil,256)end))").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    let h = host(true, HANDLE_BYTES + 512);
    set_input(&h.lua, Some(input(&[(0, 1., 0.)], 0)));
    assert_eq!(
        h.run("quota.lua", "native_stroke(1,nil,256)")
            .unwrap_err()
            .category,
        Category::Budget
    );
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    assert!(h.run("no-input.lua", "native_stroke(1,nil,256)").is_err());
}
#[test]
fn bounded_batch_reads_cover_maximum_capture_with_small_vm_and_cancel_cleanup() {
    let h = host(true, 4 * 1024 * 1024);
    let points = (0..MAX_STROKE_POINTS)
        .map(|i| (i as u64, (i % 256) as f64, i as f64 / 1000.))
        .collect::<Vec<_>>();
    set_input(&h.lua, Some(input(&points, 0)));
    h.run(
        "stream.lua",
        r#"local s=native_stroke(1,nil,256);local sum=0
      for first=1,s:info().count,1024 do
        local b=s:batch(first,1024);for _,p in ipairs(b)do sum=sum+p.tick end
        collectgarbage('collect')
      end
      assert(sum==65535*65536//2);assert(s:close())
    "#,
    )
    .unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "native_cancel",
            h.lua
                .create_function(move |_, ()| {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    set_input(&h.lua, Some(input(&[(0, 1., 0.)], 0)));
    assert_eq!(h.run("cancel.lua","retained=native_stroke(1,nil,256);native_cancel();pcall(function()retained:batch(1,1)end)").unwrap_err().category,Category::Canceled);
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    h.run(
        "reuse.lua",
        "retained=nil;collectgarbage('collect');return true",
    )
    .unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
