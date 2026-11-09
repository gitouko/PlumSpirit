use super::*;
use crate::lua::{Host, Limits};
fn host(native: usize) -> (Host, NativeBudget) {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    let b = NativeBudget::isolated(native);
    h.lua.set_app_data(b.clone());
    (h, b)
}
fn clear(h: &Host, b: &NativeBudget) {
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn original_spans_match_independent_dense_reference_without_changing_input() {
    let (h, b) = host(4 * 1024 * 1024);
    h.run("span-mask-reference.lua",r#"
      local seed=71;local function random(n)seed=(seed*48271)%2147483647;return seed%n end
      for scene=1,64 do
        local spans={row={},first={},last={},count=64};local cells={};local count=0
        for i=1,64 do local row=random(16);local a=random(32);local z=a+1+random(32-a)
          spans.row[i]=row;spans.first[i]=a;spans.last[i]=z
          for x=a,z-1 do local key=row*32+x;if not cells[key]then cells[key]=true;count=count+1 end end
        end
        spans.cell_count=count
        local m=plum.images.mask_from_spans(spans,{width=32,height=16})
        local info=m:info();assert(info.width==32 and info.height==16 and info.bytes==512 and info.format=='a8' and info.origin=='top_left')
        local bytes=m:read{};assert(#bytes==512)
        for key=0,511 do assert(bytes:byte(key+1)==(cells[key]and 255 or 0))end
        local bounds=m:bounds();assert(bounds and bounds.count==count)
        assert(spans.count==64 and #spans.row==64 and spans.cell_count==count)
        m:close();assert(not pcall(function()m:pixel(0,0)end));collectgarbage('collect')
      end
    "#).unwrap();
    clear(&h, &b);
}
#[test]
fn geometry_boolean_and_empty_masks_compose_without_rgba_or_file_authority() {
    let (h, b) = host(4 * 1024 * 1024);
    h.run("geometry-mask.lua",r#"
      local g=plum.geometry;local grid={columns=8,rows=4}
      local a=g.rasterize_polygon({x={0,5,5,0},y={0,0,3,3}},grid)
      local c=g.rasterize_polygon({x={3,8,8,3},y={1,1,4,4}},grid)
      local r=g.raster_boolean(a,c,grid,'xor')
      local m=plum.images.mask_from_spans(r,{width=8,height=4})
      assert(m:bounds().count==22 and m:pixel(3,1)==0 and m:pixel(0,0)==255)
      local empty=plum.images.mask_from_spans({row={},first={},last={},cell_count=0},{width=2,height=3})
      assert(empty:bounds()==nil and empty:read{}==string.rep(string.char(0),6))
      local vertical=plum.images.mask_from_spans({row={16383},first={0},last={1}},{width=1,height=16384})
      assert(vertical:pixel(0,16383)==255 and vertical:bounds().y==16383)
    "#).unwrap();
    clear(&h, &b);
}
#[test]
fn strict_span_and_size_validation_does_not_publish_a_partial_resource() {
    let (h, b) = host(4 * 1024 * 1024);
    h.run("invalid-span-mask.lua",r#"
      local make=plum.images.mask_from_spans;local good={row={0},first={0},last={1},count=1,cell_count=1}
      for _,o in ipairs({{}, {width=0,height=1},{width=1,height=0},{width=16385,height=1},
        {width=16384,height=16384},{width=1.0,height=1},{width=1,height=0/0},
        {width='1',height=1},{width=1,height=1,extra=true},setmetatable({width=1,height=1},{})})do
        assert(not pcall(make,good,o))
      end
      for _,s in ipairs({{}, {row={0},first={},last={1}},{row={-1},first={0},last={1}},
        {row={4},first={0},last={1}},{row={0},first={0},last={9}},
        {row={0},first={0},last={0}},{row={0},first={1},last={0}},
        {row={0},first={0.0},last={1}},{row={0},first={0/0},last={1}},
        {row={0},first={0},last={1},count=2},{row={0},first={0},last={1},cell_count=2},
        {row={0},first={0},last={1},extra=true},{row={[0]=0},first={},last={}},
        {row=setmetatable({0},{}),first={0},last={1}},setmetatable(good,{})})do
        assert(not pcall(make,s,{width=8,height=4}))
      end
      setmetatable(good,nil)
      local huge={row={},first={},last={}};for i=1,65537 do huge.row[i]=0;huge.first[i]=0;huge.last[i]=1 end
      assert(not pcall(make,huge,{width=8,height=4}))
      local m=make(good,{width=8,height=4});assert(m:bounds().count==1 and good.count==1)
    "#).unwrap();
    clear(&h, &b);
}
#[test]
fn maximum_pixels_fit_small_vm_and_native_preflight_recovers_without_publication() {
    let (h, b) = host(32 * 1024 * 1024);
    h.run(
        "large-span-mask.lua",
        r#"
      local s={row={},first={},last={}};for i=1,4096 do s.row[i]=i-1;s.first[i]=0;s.last[i]=4096 end
      local m=plum.images.mask_from_spans(s,{width=4096,height=4096})
      assert(m:info().bytes==16777216 and m:pixel(4095,4095)==255 and #m:row(2048)==4096)
      assert(not pcall(function()m:read{}end));m:close()
    "#,
    )
    .unwrap();
    clear(&h, &b);
    let (h, b) = host(HANDLE_BYTES + 1024 + 1);
    let error = h
        .run(
            "native-span-mask-quota.lua",
            "return plum.images.mask_from_spans({row={0},first={0},last={1}},{width=1,height=1})",
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    clear(&h, &b);
    h.run("recover-span-mask.lua","assert(plum.images.mask_from_spans({row={},first={},last={}},{width=1,height=1}):pixel(0,0)==0)").unwrap();
    clear(&h, &b);
}
#[test]
fn retained_closed_and_expired_masks_are_charged_and_share_native_resource_limits() {
    let (h, b) = host(1024 * 1024);
    h.run("retain-span-masks.lua",r#"
      retained={};local empty={row={},first={},last={}}
      for i=1,64 do
        local m
        if i%2==0 then m=plum.images.from_rgba(1,1,string.char(0,0,0,0));m:close()
        else m=plum.images.mask_from_spans(empty,{width=1,height=1});m:close();retained[#retained+1]=m end
      end
      assert(not pcall(plum.images.mask_from_spans,empty,{width=1,height=1}))
      assert(not pcall(plum.images.from_rgba,1,1,string.char(0,0,0,0)))
    "#).unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 32 * HANDLE_BYTES);
    let e = h
        .run("old-span-mask.lua", "return retained[1]:info()")
        .unwrap_err();
    assert_eq!(e.category, Category::Expired);
    h.lua.globals().set("retained", Value::Nil).unwrap();
    clear(&h, &b);
    let e=h.run("failed-retained-mask.lua","held=plum.images.mask_from_spans({row={0},first={0},last={1}},{width=1,height=1});error('after mask')").unwrap_err();
    assert!(e.message.contains("after mask"));
    assert_eq!(b.used(), HANDLE_BYTES);
    h.lua.globals().set("held", Value::Nil).unwrap();
    clear(&h, &b);
}
#[test]
fn native_mask_zero_and_coverage_fill_cancel_before_publishing_any_bytes() {
    let spans = [crate::lua::geometry::RasterSpan {
        row: 0,
        first: 0,
        last: 16384,
    }];
    // First cancellation falls during bounded zero-fill; second during span fill.
    for stop_at in [3, 4] {
        let mut checks = 0;
        let e = span_pixels(2 * 65536, 16384, &spans, || {
            checks += 1;
            if checks == stop_at {
                Err(stop(Category::Canceled, "controlled A8 cancel"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(checks, stop_at);
        assert!(crate::lua::error_text(&e).contains("controlled A8 cancel"));
    }
    assert_eq!(
        span_pixels(
            4,
            4,
            &[crate::lua::geometry::RasterSpan {
                row: 0,
                first: 1,
                last: 3
            }],
            || Ok(())
        )
        .unwrap(),
        [0, 255, 255, 0]
    );
}

#[test]
fn published_geometry_mask_example_executes_with_exact_ticks_and_no_grants() {
    let (h, b) = host(4 * 1024 * 1024);
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Geometry span masks (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-span-mask.lua", source).unwrap();
    assert!(h.take_commits().is_none());
    clear(&h, &b);
}
