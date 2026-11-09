use super::*;

fn host(limit: Option<usize>) -> Host {
    let host = Host::new(Limits::default(), []).unwrap();
    if let Some(limit) = limit {
        host.lua.set_app_data(NativeBudget::isolated(limit));
    }
    host.lua
        .globals()
        .set(
            "native_bytes",
            host.lua
                .create_function(|lua, ()| Ok(lua.app_data_ref::<NativeBudget>().unwrap().used()))
                .unwrap(),
        )
        .unwrap();
    host.lua
        .globals()
        .set(
            "cache_entries",
            host.lua
                .create_function(|lua, ()| Ok(lua.app_data_ref::<ImageSlot>().unwrap().cache.len()))
                .unwrap(),
        )
        .unwrap();
    host
}

#[test]
fn immutable_cached_results_share_buffers_keep_independent_handles_and_expire() {
    let host = host(Some(8 + 4 * HANDLE_BYTES));
    host.run(
        "image-cache.lua",
        r#"
        local image=plum.images.from_rgba(1,1,string.char(1,2,3,128))
        local a=image:transform{}
        assert(native_bytes()==8+2*512)
        local b=image:transform{}
        assert(a~=b and native_bytes()==8+3*512 and b:pixel(0,0)==1)
        a:close();assert(native_bytes()==8+3*512 and b:pixel(0,0)==1)
        b:close();assert(native_bytes()==4+3*512)
        kept_cached=image:transform{} -- expired weak entry must be recomputed
        assert(native_bytes()==8+4*512)
        image:close();assert(native_bytes()==4+4*512 and kept_cached:pixel(0,0)==1)
    "#,
    )
    .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(
        host.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    assert!(
        host.lua
            .app_data_ref::<ImageSlot>()
            .unwrap()
            .cache
            .is_empty()
    );
    assert_eq!(
        host.run("expired-cache.lua", "kept_cached:pixel(0,0)")
            .unwrap_err()
            .category,
        Category::Expired
    );
    host.lua.globals().set("kept_cached", Value::Nil).unwrap();
    host.lua.gc_collect().unwrap();
    host.lua
        .set_app_data(NativeBudget::isolated(64 * HANDLE_BYTES + 8));
    let error = host
        .run(
            "cached-handle-limit.lua",
            r#"
        local image=plum.images.from_rgba(1,1,string.char(1,2,3,128))
        local a=image:transform{}
        for n=1,63 do image:transform{}:close() end
    "#,
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(error.message.contains("At most 64 image resources"));
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn cache_bounds_source_identity_and_exact_settings_are_enforced() {
    let host = host(None);
    host.run("cache-identity.lua",r#"
        local a=plum.images.from_rgba(1,1,string.char(1,0,0,255))
        local b=plum.images.from_rgba(1,1,string.char(2,0,0,255))
        assert(a:transform{}:pixel(0,0)==1 and b:transform{}:pixel(0,0)==2)
        local held={}
        for n=0,16 do held[n+1]=a:transform{rotate=n*360} end
        assert(cache_entries()<=plum.images.max_cache_entries and plum.images.max_cache_entries==16)
        collectgarbage("collect");local before=native_bytes()
        local again=a:transform{rotate=16*360}
        assert(native_bytes()==before+512 and again:pixel(0,0)==1)
        local first=a:transform{rotate=0} -- oldest setting was evicted, original result remains valid
        assert(native_bytes()==before+4+2*512 and held[1]:pixel(0,0)==1 and first:pixel(0,0)==1)
        local resized=a:resize(2,1,'nearest')
        collectgarbage("collect");local before=native_bytes()
        local box=a:resize(2,1,'box') -- filter is part of identity
        assert(native_bytes()==before+8+512)
        local same=a:resize(2,1,'nearest')
        assert(native_bytes()==before+8+2*512 and same~=resized)
    "#).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn affine_composition_inverse_pivots_and_pixel_mapping_are_exact() {
    let host = host(None);
    host.run("affine-image.lua",r#"
        local p=plum.images
        local translate=p.mapping{translate_x=1,translate_y=2}
        local scale=p.mapping{scale_x=2,scale_y=3}
        local composed=p.compose(translate,scale)
        local x,y=p.map_point(composed,3,4);assert(x==8 and y==18)
        x,y=p.map_point(p.inverse(composed),x,y);assert(math.abs(x-3)<1e-12 and math.abs(y-4)<1e-12)
        local a=string.char(255,0,0,255);local b=string.char(0,255,0,128)
        local c=string.char(0,0,255,0);local d=string.char(255,255,255,64)
        local t=string.char(0,0,0,0)
        local image=p.from_rgba(2,2,a..b..c..d)
        assert(image:warp{matrix=p.mapping{},width=2,height=2}:read{width=2,height=2}==a..b..c..d)
        local moved=image:warp{matrix=p.mapping{translate_x=1},width=3,height=2}
        assert(moved:row(0)==t..a..b and moved:row(1)==t..c..d)
        local rotated=image:warp{matrix=p.mapping{rotate=90,pivot_x=1,pivot_y=1},width=2,height=2}
        assert(rotated:read{width=2,height=2}==c..a..d..b)
        local flipped=image:warp{matrix=p.mapping{scale_x=-1,pivot_x=1},width=2,height=2}
        assert(flipped:read{width=2,height=2}==b..a..d..c)
        local line=p.from_rgba(2,1,a..b)
        local expected={transparent=t..t..a..b..t..t,clamp=a..a..a..b..b..b,wrap=a..b..a..b..a..b,mirror=b..a..a..b..b..a}
        for edge,bytes in pairs(expected) do
            assert(line:warp{matrix=p.mapping{translate_x=2},width=6,height=1,edge=edge}:row(0)==bytes)
        end
        assert(image:read{width=2,height=2}==a..b..c..d)
    "#).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn contain_cover_stretch_preserve_declared_aspect_and_return_reusable_maps() {
    let host = host(None);
    host.run("image-fit.lua",r#"
        local a=string.char(255,0,0,255);local b=string.char(0,255,0,128);local t=string.char(0,0,0,0)
        local image=plum.images.from_rgba(2,1,a..b)
        local contain,map=image:fit(4,4)
        assert(map.a==2 and map.d==2 and map.e==0 and map.f==1)
        assert(contain:row(0)==string.rep(t,4) and contain:row(3)==string.rep(t,4))
        assert(contain:row(1)==a..a..b..b and contain:row(2)==a..a..b..b)
        collectgarbage("collect");local before=native_bytes()
        local same=image:warp{matrix=map,width=4,height=4}
        assert(native_bytes()==before+512 and same:row(1)==contain:row(1))
        local cover,fill=image:fit(4,4,'cover')
        assert(fill.a==4 and fill.d==4 and fill.e==-2 and fill.f==0)
        local stretch,s=image:fit(4,4,'stretch')
        assert(s.a==2 and s.d==4 and s.e==0 and s.f==0)
        for n=0,3 do assert(cover:row(n)==a..a..b..b and stretch:row(n)==a..a..b..b) end
        for _,m in ipairs({map,fill,s}) do
            local x,y=plum.images.map_point(m,1,0.5);assert(x==2 and y==2)
        end
    "#).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn affine_limits_and_bad_options_fail_without_source_changes() {
    let host = host(None);
    host.run("bad-affine.lua",r#"
        local p=plum.images
        local image=p.from_rgba(1,1,string.char(1,2,3,4))
        for _,options in ipairs({{scale_x=0},{rotate=0/0},{translate_x=math.huge},{pivot_x=1e13},{scale_x='2'},{scale_x=1e-13},{extra=0}}) do
            assert(not pcall(function()p.mapping(options)end))
        end
        local id=p.mapping{}
        for _,matrix in ipairs({{}, {a=1,b=0,c=0,d=0,e=0,f=0}, {a=1,b=1,c=1,d=1.000000000000001,e=0,f=0}, {a=1,b=0,c=0,d=1,e=0,f=0,extra=1}}) do
            assert(not pcall(function()p.inverse(matrix)end))
        end
        assert(not pcall(function()p.map_point(id,0/0,0)end))
        assert(not pcall(function()p.map_point(p.mapping{scale_x=1e6},1e10,0)end))
        assert(not pcall(function()p.compose(p.mapping{scale_x=1e12,scale_y=1e12},p.mapping{scale_x=2,scale_y=2})end))
        for _,options in ipairs({{matrix=id,width=0,height=1},{matrix=id,width=1,height=1,edge='bad'},{matrix=id,width=1,height=1,filter='cubic'}}) do
            assert(not pcall(function()image:warp(options)end))
        end
        assert(not pcall(function()image:warp{matrix=p.mapping{scale_x=1e-12,scale_y=1e-12},width=2,height=2}end))
        assert(not pcall(function()image:fit(1,1,'bad')end))
        assert(image:pixel(0,0)==1)
    "#).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
