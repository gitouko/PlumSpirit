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
fn every_coverage_byte_matches_independent_straight_alpha_reference_and_preserves_rgb() {
    let (h, b) = host(8 * 1024 * 1024);
    h.run("mask-color-reference.lua",r#"
      local pixels={};for c=0,255 do pixels[#pixels+1]=string.char(0,0,0,c)end
      local source=plum.images.from_rgba(256,1,table.concat(pixels));local mask=source:mask{};source:close()
      for _,a in ipairs({0,1,127,128,254,255})do
        local image=mask:colorize{13,57,201,a};local expected={}
        for c=0,255 do expected[#expected+1]=string.char(13,57,201,math.floor(c*a/255+.5))end
        assert(image:read{width=256,height=1}==table.concat(expected))
        assert(image:info().format=='rgba'and image:info().bytes==1024 and image:info().alpha=='straight')
        local r,g,b,alpha=image:pixel(0,0);assert(r==13 and g==57 and b==201 and alpha==0)
        assert(mask:pixel(127,0)==127);image:close()
      end
      mask:close()
    "#).unwrap();
    assert!(h.take_commits().is_none());
    clear(&h, &b);
}
#[test]
fn procedural_geometry_mask_colors_feed_existing_transform_and_note_plan_without_lua_pixels() {
    let (h, b) = host(8 * 1024 * 1024);
    h.run("mask-color-plan.lua",r#"
      local mask=plum.images.mask_from_spans({row={0,1},first={0,1},last={1,2}},{width=2,height=2})
      local image=mask:colorize{255,0,0,255};mask:close()
      local turned=image:transform{rotate=90};local r,g,b,a=turned:pixel(1,0);assert(r==255 and a==255)
      local plan=image:note_plan{palette={{255,0,0}},start_tick='9007199254740993',tick_numerator=3,first_key=200,keys=256}
      image:close();turned:close();local rows=plan:next(4)
      assert(#rows.start==2 and rows.start[1]==9007199254740993 and rows.start[2]==9007199254740996)
      assert(rows.key[1]==201 and rows.key[2]==200 and rows.gate[1]==3 and rows.gate[2]==3)
      plan:close()
    "#).unwrap();
    clear(&h, &b);
}
#[test]
fn strict_color_options_preflight_and_retained_job_cleanup_are_explicit() {
    let (h, b) = host(1024 * 1024);
    h.run("mask-color-invalid.lua",r#"
      local m=plum.images.mask_from_spans({row={0},first={0},last={1}},{width=1,height=1})
      for _,color in ipairs({{}, {1,2,3},{1,2,3,4,5},{0,0,0,255.0},{0,0,0,256},{-1,0,0,255},{0/0,0,0,255},{1,2,3,4,extra=true},setmetatable({1,2,3,4},{})})do assert(not pcall(function()m:colorize(color)end))end
      held=m:colorize{1,2,3,4};m:close();assert(held:info().bytes==4)
    "#).unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), super::super::HANDLE_BYTES);
    assert_eq!(
        h.run("expired-colored-mask.lua", "return held:pixel(0,0)")
            .unwrap_err()
            .category,
        Category::Expired
    );
    h.lua.globals().set("held", Value::Nil).unwrap();
    clear(&h, &b);
    let (h, b) = host(2 * 512 + 1 + 3);
    // Direct fixture avoids geometry scratch so the four-byte output is the
    // precise denied allocation, not an unrelated input parser budget.
    h.lua
        .globals()
        .set(
            "fixture_mask",
            h.lua
                .create_function(|lua, ()| {
                    let (job, b) = context(lua)?;
                    let handle = b.reserve(HANDLE_BYTES).map_err(|e| e.lua())?;
                    let pixels = b.reserve(1).map_err(|e| e.lua())?;
                    store(
                        lua,
                        job,
                        StoredMask {
                            width: 1,
                            height: 1,
                            pixels: vec![255],
                            _allocation: pixels,
                        },
                        handle,
                    )
                })
                .unwrap(),
        )
        .unwrap();
    h.run("mask-color-quota.lua","local m=fixture_mask();local ok,e=pcall(function()m:colorize{1,2,3,4}end);assert(not ok and tostring(e):lower():find('budget'));assert(m:pixel(0,0)==255);m:close()").unwrap();
    clear(&h, &b);
}
#[test]
fn large_colorization_keeps_pixels_native_and_cancellation_discards_partial_rgba() {
    let (h, b) = host(96 * 1024 * 1024);
    h.run("large-native-mask-color.lua","local m=plum.images.mask_from_spans({row={4095},first={0},last={4096}},{width=4096,height=4096});local i=m:colorize{1,2,3,127};assert(i:info().bytes==67108864);local r,g,b,a=i:pixel(4095,4095);assert(r==1 and g==2 and b==3 and a==127);r,g,b,a=i:pixel(0,0);assert(r==1 and g==2 and b==3 and a==0)").unwrap();
    clear(&h, &b);
    let coverage = vec![255; 65536];
    let mut checks = 0;
    let error = color_pixels(&coverage, [1, 2, 3, 4], || {
        checks += 1;
        if checks == 4 {
            Err(stop(Category::Canceled, "controlled color fill"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(checks, 4);
    assert!(crate::lua::error_text(&error).contains("controlled color fill"));
    assert_eq!(coverage[0], 255);
}
#[test]
fn colorized_images_share_creation_limit_and_retained_metadata_cannot_be_closed_away() {
    let (h, b) = host(1024 * 1024);
    h.run(
        "colorize-count.lua",
        r#"
      local m=plum.images.mask_from_spans({row={},first={},last={}},{width=1,height=1});held={}
      for i=1,63 do held[i]=m:colorize{0,0,0,0};held[i]:close()end
      assert(not pcall(function()m:colorize{0,0,0,0}end))
    "#,
    )
    .unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 63 * 512);
    h.lua.globals().set("held", Value::Nil).unwrap();
    clear(&h, &b);
}

#[test]
fn published_mask_colorization_example_has_defined_straight_bytes() {
    let (h, b) = host(1024 * 1024);
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Native mask colorization (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-colorize.lua", source).unwrap();
    clear(&h, &b);
}
