use super::*;
use crate::lua::{Host, Limits};
fn host() -> (Host, NativeBudget) {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    let b = NativeBudget::isolated(160 * 1024 * 1024);
    h.lua.set_app_data(b.clone());
    (h, b)
}
fn clear(h: &Host, b: &NativeBudget) {
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), 0);
}
#[test]
fn all_alpha_pairs_match_independent_normalized_source_over_and_known_vectors() {
    assert_eq!(over([255, 0, 0, 255], [0, 0, 255, 128]), [127, 0, 128, 255]);
    assert_eq!(over([9, 8, 7, 0], [1, 2, 3, 0]), [9, 8, 7, 0]);
    for sa in 0..=255u16 {
        for da in 0..=255u16 {
            let src = [(sa * 17 % 256) as u8, 7, 231, sa as u8];
            let dst = [83, (da * 13 % 256) as u8, 29, da as u8];
            let actual = over(dst, src);
            if sa == 0 {
                assert_eq!(actual, dst);
                continue;
            }
            let s = f64::from(sa) / 255.;
            let d = f64::from(da) / 255.;
            let a = s + d * (1. - s);
            let mut expected = [0; 4];
            for i in 0..3 {
                expected[i] =
                    ((f64::from(src[i]) * s + f64::from(dst[i]) * d * (1. - s)) / a + 0.5 + 1e-9)
                        .floor() as u8;
            }
            expected[3] = (a * 255. + 0.5 + 1e-9).floor() as u8;
            assert_eq!(actual, expected, "alpha{sa}/{da}");
        }
    }
}
#[test]
fn ordered_layers_clipping_and_offscreen_exclusion_match_independent_dense_reference() {
    let (h, b) = host();
    h.run("composite-reference.lua",r#"
      local seed=97;local function random(n)seed=seed*48271%2147483647;return seed%n end
      local function over(d,s)
        if s[4]==0 then return d end
        local sa,da=s[4]/255,d[4]/255;local a=sa+da*(1-sa);local out={}
        for c=1,3 do out[c]=math.floor((s[c]*sa+d[c]*da*(1-sa))/a+0.5+1e-9)end
        out[4]=math.floor(a*255+0.5+1e-9);return out
      end
      for scene=1,12 do
        local expected={};local bg={3,17,91,random(256)};for i=1,35 do expected[i]={table.unpack(bg)}end
        local layers={};local originals={}
        for k=1,4 do local w,h=random(6)+1,random(5)+1;local x,y=random(13)-6,random(11)-5;local bytes={};local pixels={}
          for i=1,w*h do local p={random(256),random(256),random(256),random(256)};pixels[i]=p;bytes[i]=string.char(table.unpack(p))end
          local image=plum.images.from_rgba(w,h,table.concat(bytes));layers[k]={image=image,x=x,y=y};originals[k]=image:read{width=w,height=h}
          for sy=0,h-1 do for sx=0,w-1 do local dx,dy=x+sx,y+sy;if dx>=0 and dx<7 and dy>=0 and dy<5 then local i=dy*7+dx+1;expected[i]=over(expected[i],pixels[sy*w+sx+1])end end end
        end
        local image=plum.images.composite(layers,{width=7,height=5,background=bg});local bytes={}
        for i=1,35 do bytes[i]=string.char(table.unpack(expected[i]))end
        assert(image:read{width=7,height=5}==table.concat(bytes))
        for k=1,4 do local i=layers[k].image;local info=i:info();assert(i:read{width=info.width,height=info.height}==originals[k]);i:close()end
        image:close();collectgarbage('collect')
      end
    "#).unwrap();
    clear(&h, &b);
}
#[test]
fn strict_descriptors_sizes_scopes_and_layer_limits_do_not_publish_partial_images() {
    let (h, b) = host();
    h.run("invalid-composite.lua",r#"
      local c=plum.images.composite;local s=plum.images.from_rgba(1,1,string.char(1,2,3,255))
      for _,layers in ipairs({{{image=s,x=0.0}},{{image=s,y=2147483648}},{{image=s,x=-2147483649}},{{image=s,extra=1}},{{image='bad'}},{{image=s},extra=1},setmetatable({{image=s}},{}),{setmetatable({image=s},{})},{[0]={image=s}},{{image=s},[3]={image=s}}})do assert(not pcall(c,layers,{width=1,height=1}))end
      for _,o in ipairs({{}, {width=0,height=1},{width=1,height=16385},{width=1.0,height=1},{width=16384,height=16384},{width=1,height=1,extra=true},{width=1,height=1,background={1,2,3}},setmetatable({width=1,height=1},{})})do assert(not pcall(c,{{image=s}},o))end
      local layers={};for i=1,32 do layers[i]={image=s,x=2147483647,y=-2147483648}end
      local out=c(layers,{width=1,height=1,background={17,23,31,0}});assert(out:row(0)==string.char(17,23,31,0));layers[33]={image=s};assert(not pcall(c,layers,{width=1,height=1}))
      s:close();assert(not pcall(c,{{image=s}},{width=1,height=1}));held=out
    "#).unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(b.used(), HANDLE_BYTES);
    assert_eq!(
        h.run(
            "expired-composite.lua",
            "return plum.images.composite({{image=held}},{width=1,height=1})"
        )
        .unwrap_err()
        .category,
        Category::Expired
    );
    h.lua.globals().set("held", Value::Nil).unwrap();
    clear(&h, &b);
}
#[test]
fn maximum_native_canvas_and_work_caps_fit_small_vm_and_fail_before_excess_work() {
    let (h, b) = host();
    h.run("large-composite.lua",r#"
      local c=plum.images.composite;assert(plum.images.max_composite_layers==32 and plum.images.max_composite_work==67108864)
      local src=c({},{width=4096,height=4096,background={1,2,3,255}})
      local layers={{image=src},{image=src},{image=src}}
      local out=c(layers,{width=4096,height=4096});assert(out:info().bytes==67108864 and out:pixel(4095,4095)==1)
      layers[4]={image=src};local ok,e=pcall(c,layers,{width=4096,height=4096});assert(not ok and tostring(e):find('pixel operations'))
      out:close();src:close()
    "#).unwrap();
    clear(&h, &b);
}
#[test]
fn native_preflight_and_mid_fill_cancellation_release_all_transient_snapshots() {
    let (h, b) = host();
    let ledger = b.clone();
    let held = Rc::new(RefCell::new(None));
    let sink = held.clone();
    h.lua
        .globals()
        .set(
            "hold_native",
            h.lua
                .create_function(move |_, ()| {
                    let leave = std::mem::size_of::<Layer>() + 1024 + HANDLE_BYTES + 3;
                    *sink.borrow_mut() = Some(
                        ledger
                            .reserve(160 * 1024 * 1024 - ledger.used() - leave)
                            .map_err(|e| e.lua())?,
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "release_native",
            h.lua
                .create_function(move |_, ()| {
                    held.borrow_mut().take();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.run("composite-quota.lua","local s=plum.images.composite({},{width=1,height=1,background={1,2,3,255}});hold_native();local ok,e=pcall(plum.images.composite,{{image=s}},{width=1,height=1});release_native();assert(not ok and tostring(e):lower():find('budget'));assert(plum.images.composite({{image=s}},{width=1,height=1}):pixel(0,0)==1)").unwrap();
    clear(&h, &b);
    for target in [10, 25] {
        let (h, b) = host();
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled_composite",
                h.lua
                    .create_function(move |lua, (input, options): (Table, Table)| {
                        let mut n = 0;
                        controlled(lua, input, options, || {
                            n += 1;
                            if n == target {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        })
                    })
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(h.run("cancel-composite.lua","local source=plum.images.from_rgba(256,256,string.rep(string.char(1,2,3,255),65536));result=controlled_composite({{image=source}},{width=256,height=256})").unwrap_err().category,Category::Canceled);
        assert!(matches!(
            h.lua.globals().get::<Value>("result").unwrap(),
            Value::Nil
        ));
        clear(&h, &b);
        h.run(
            "composite-recover.lua",
            "assert(plum.images.composite({},{width=1,height=1}):pixel(0,0)==0)",
        )
        .unwrap();
        clear(&h, &b);
    }
}

#[test]
fn vm_userdata_publication_failure_drops_native_result_and_allows_next_job() {
    let (h, b) = host();
    h.lua
        .globals()
        .set(
            "fail_publication",
            h.lua
                .create_function(|lua, (input, options): (Table, Table)| {
                    let mut calls = 0;
                    controlled(lua, input, options, || {
                        calls += 1;
                        notes::check_budget(lua)?;
                        if calls == 4 {
                            lua.gc_stop();
                            lua.set_memory_limit(lua.used_memory() + 32)?;
                        }
                        Ok(())
                    })
                })
                .unwrap(),
        )
        .unwrap();
    let error = h
        .run(
            "composite-vm-publication.lua",
            "result=fail_publication({},{width=16,height=16})",
        )
        .unwrap_err();
    h.lua.set_memory_limit(8 * 1024 * 1024).unwrap();
    h.lua.gc_restart();
    assert_eq!(error.category, Category::Memory);
    clear(&h, &b);
    assert!(matches!(
        h.lua.globals().get::<Value>("result").unwrap(),
        Value::Nil
    ));
    h.run(
        "composite-vm-recover.lua",
        "assert(plum.images.composite({},{width=1,height=1}):pixel(0,0)==0)",
    )
    .unwrap();
    clear(&h, &b);
}

#[test]
fn published_native_composite_example_preserves_layer_order_and_exact_bytes() {
    let (h, b) = host();
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Native image composition (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-composite.lua", source).unwrap();
    assert!(h.take_commits().is_none());
    clear(&h, &b);
}
