use super::*;

fn execute(source: &str) -> Host {
    let host = Host::new(Limits::default(), []).unwrap();
    host.run("image-analysis.lua", source).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    host
}

#[test]
fn color_models_round_trip_and_native_palette_ties_are_exact() {
    execute(
        r#"
        local h,s,v=plum.images.rgb_to_hsv(255,0,0)
        assert(h==0 and s==1 and v==1)
        h,s,v=plum.images.rgb_to_hsl(0,255,0)
        assert(h==120 and s==1 and v==0.5)
        h,s,v=plum.images.rgb_to_hsv(128,128,128)
        assert(h==0 and s==0 and v==128/255)
        local levels={0,1,127,128,254,255}
        for _,r in ipairs(levels) do for _,g in ipairs(levels) do for _,b in ipairs(levels) do
            for _,model in ipairs({'hsv','hsl'}) do
                local h,s,v=plum.images['rgb_to_'..model](r,g,b)
                local rr,gg,bb=plum.images[model..'_to_rgb'](h,s,v)
                assert(rr==r and gg==g and bb==b)
            end
        end end end
        local r,g,b=plum.images.hsv_to_rgb(360,1,1)
        assert(r==255 and g==0 and b==0)
        r,g,b=plum.images.hsl_to_rgb(240,1,0.5)
        assert(r==0 and g==0 and b==255)
        assert(plum.images.color_distance(0,0,0,255,255,255)==1)
        assert(plum.images.color_distance(1,2,3,1,2,3)==0)
        assert(math.abs(plum.images.color_distance(0,0,0,255,0,0)-1/math.sqrt(3))<1e-15)
        local index,distance=plum.images.nearest_color({{0,0,0},{2,0,0}},1,0,0)
        assert(index==1 and distance==plum.images.color_distance(0,0,0,1,0,0))
        index,distance=plum.images.nearest_color({{0,0,0},{2,3,4}},2,3,4)
        assert(index==2 and distance==0)
        local palette={};for n=1,plum.images.max_palette do palette[n]={0,0,0} end
        assert(plum.images.nearest_color(palette,255,0,0)==1)
        for _,f in ipairs({
            function()plum.images.rgb_to_hsv(256,0,0)end,
            function()plum.images.rgb_to_hsl(0,-1,0)end,
            function()plum.images.hsv_to_rgb('0',1,1)end,
            function()plum.images.hsv_to_rgb(-1,1,1)end,
            function()plum.images.hsl_to_rgb(361,1,1)end,
            function()plum.images.hsv_to_rgb(0,1.01,1)end,
            function()plum.images.hsl_to_rgb(0,1,0/0)end,
            function()plum.images.color_distance(0,0,0,0,0,math.huge)end,
            function()plum.images.nearest_color({},0,0,0)end,
            function()plum.images.nearest_color({{0,0,0,1}},0,0,0)end,
            function()plum.images.nearest_color({{0,0,0},extra=true},0,0,0)end
        }) do assert(not pcall(f)) end
        palette[#palette+1]={0,0,0}
        assert(not pcall(function()plum.images.nearest_color(palette,0,0,0)end))
    "#,
    );
}

#[test]
fn mask_coverage_thresholds_alpha_policies_and_bounds_are_exact() {
    execute(
        r#"
        local bytes=string.char(255,0,0,255, 0,255,0,128, 0,0,255,0, 255,255,255,64)
        local image=plum.images.from_rgba(2,2,bytes)
        local a=image:mask{kind='alpha'}
        assert(a:info().format=='a8' and a:info().bytes==4 and a:info().origin=='top_left')
        assert(a:info().width==2 and a:info().height==2)
        assert(a:read{}==string.char(255,128,0,64))
        assert(a:row(1)==string.char(0,64) and a:row(0,1,1)==string.char(128))
        assert(a:read{x=1,width=1,height=2}==string.char(128,64))
        assert(image:mask{kind='alpha',invert=true}:read{}==string.char(0,127,255,191))
        assert(image:mask{kind='alpha',min=0.5}:read{}==string.char(255,255,0,0))
        assert(image:mask{kind='luminance'}:read{}==string.char(54,92,0,64))
        assert(image:mask{kind='brightness'}:read{}==string.char(85,43,0,64))
        assert(image:mask{kind='luminance',min=0.7}:read{}==string.char(0,128,0,64))
        assert(image:mask{kind='luminance',min=0.7,invert=true}:read{}==string.char(255,0,0,0))
        assert(image:mask{kind='luminance',preserve_alpha=false}:read{}==string.char(54,182,0,255))
        assert(image:mask{kind='luminance',preserve_alpha=false,alpha_min=0}:pixel(0,1)==18)
        local green=image:mask{kind='color',color={0,255,0}}
        assert(green:read{}==string.char(0,128,0,0))
        assert(image:mask{kind='color',color={0,255,0},invert=true}:read{}==string.char(255,0,0,64))
        assert(image:mask{kind='color',color={0,0,255},alpha_min=0,preserve_alpha=false}:read{}==string.char(0,0,255,0))
        assert(image:mask{kind='color',color={0,255,0},tolerance=1}:read{}==string.char(255,128,0,64))
        local bounds=a:bounds();assert(bounds.x==0 and bounds.y==0 and bounds.width==2 and bounds.height==2 and bounds.count==3)
        bounds=a:bounds(200);assert(bounds.x==0 and bounds.y==0 and bounds.width==1 and bounds.height==1 and bounds.count==1)
        bounds=green:bounds();assert(bounds.x==1 and bounds.y==0 and bounds.width==1 and bounds.height==1 and bounds.count==1)
        assert(image:mask{kind='alpha',min=0.9,max=0.9}:bounds()==nil)
        assert(image:read{width=2,height=2}==bytes)
        image:close();assert(a:pixel(1,0)==128)
        a:close();a:close();assert(not pcall(function()a:pixel(0,0)end))
    "#,
    );
}

#[test]
fn histograms_use_exact_regions_integer_sampling_and_transparency() {
    execute(
        r#"
        local bytes={}
        for n=0,7 do bytes[#bytes+1]=string.char(n==7 and 255 or n,0,0,n==2 and 0 or 255) end
        local image=plum.images.from_rgba(4,2,table.concat(bytes))
        local h=image:histogram{channel='red',alpha_min=0,max_samples=3}
        assert(h.pixels==8 and h.examined==3 and h.samples==3 and h.estimated)
        assert(h.counts[1]==1 and h.counts[3]==1 and h.counts[6]==1)
        h=image:histogram{channel='red',max_samples=3}
        assert(h.samples==2 and h.examined==3 and h.counts[3]==0)
        h=image:histogram{channel='red',x=1,width=2,height=2,bins=2}
        assert(h.x==1 and h.y==0 and h.width==2 and h.height==2)
        assert(h.pixels==4 and h.examined==4 and h.samples==3 and not h.estimated)
        assert(h.counts[1]==3 and h.counts[2]==0)
        h=image:histogram{channel='alpha',bins=2}
        assert(h.samples==8 and h.counts[1]==1 and h.counts[2]==7)
        local rgb=plum.images.from_rgba(3,1,string.char(255,0,0,255,0,255,0,255,0,0,255,255))
        for _,channel in ipairs({'hue','saturation','value','lightness','luminance','brightness','red','green','blue','alpha'}) do
            local a=rgb:histogram{channel=channel,bins=3}
            local b=rgb:histogram{channel=channel,bins=3}
            assert(a.samples==3 and a.counts[1]+a.counts[2]+a.counts[3]==3)
            for n=1,3 do assert(a.counts[n]==b.counts[n]) end
            if channel=='hue' then assert(a.counts[1]==1 and a.counts[2]==1 and a.counts[3]==1) end
        end
        assert(#rgb:histogram{bins=plum.images.max_histogram_bins}.counts==1024)
        assert(rgb:histogram().channel=='luminance')
    "#,
    );
}

#[test]
fn invalid_mask_analysis_options_and_metatables_cannot_mutate_source() {
    execute(
        r#"
        local image=plum.images.from_rgba(1,1,string.char(1,2,3,128))
        local called=false
        local options=setmetatable({kind='luminance'},{__index=function()called=true;image:close();error('unexpected metamethod')end})
        assert(image:mask(options):pixel(0,0)>=0)
        assert(image:histogram(setmetatable({},{__index=function()called=true;image:close()end})).samples==1)
        assert(not called)
        for _,options in ipairs({
            {kind='bogus'},{kind='alpha',preserve_alpha=true},{kind='alpha',min=0.8,max=0.2},
            {kind='luminance',min=0/0},{kind='alpha',max=1.1},{kind='color',color={0,0}},
            {kind='color',color={0,0,0},min=0},{kind='color',color={0,0,0},tolerance=-1},
            {kind='brightness',alpha_min=256},{kind='luminance',preserve_alpha=1},{invert=1}
        }) do assert(not pcall(function()image:mask(options)end)) end
        for _,options in ipairs({
            {bins=0},{bins=1025},{max_samples=0},{max_samples=262145},
            {channel='unknown'},{alpha_min=-1},{x=1},{width=0},{y=0/0},{extra=1}
        }) do assert(not pcall(function()image:histogram(options)end)) end
        local mask=image:mask{}
        for _,f in ipairs({
            function()mask:pixel(1,0)end,function()mask:pixel(0/0,0)end,
            function()mask:row(0,0,0)end,function()mask:row(1)end,
            function()mask:read{width=2}end,function()mask:read{extra=true}end,
            function()mask:bounds(0)end,function()mask:bounds(256)end
        }) do assert(not pcall(f)) end
        assert(image:pixel(0,0)==1)
    "#,
    );
}

#[test]
fn masks_share_native_and_creation_budgets_and_expire_eagerly() {
    let host = Host::new(Limits::default(), []).unwrap();
    let budget = NativeBudget::isolated(1029);
    host.lua.set_app_data(budget.clone());
    host.run(
        "compact-mask.lua",
        r#"
        local image=plum.images.from_rgba(1,1,string.char(1,2,3,128))
        kept_mask=image:mask{}
        assert(kept_mask:info().bytes==1 and kept_mask:pixel(0,0)==128)
        assert(not pcall(function()image:mask{}end))
        image:close();assert(kept_mask:read{}==string.char(128))
    "#,
    )
    .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(
        budget.used(),
        512,
        "Only the retained expired mask cell remains"
    );
    assert_eq!(
        host.run("expired-mask.lua", "kept_mask:pixel(0,0)")
            .unwrap_err()
            .category,
        Category::Expired
    );
    host.lua.globals().set("kept_mask", Value::Nil).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    let failure = host
        .run(
            "failed-mask.lua",
            r#"
        kept_mask=plum.images.from_rgba(1,1,string.char(0,0,0,127)):mask{}
        error('deliberate mask failure')
    "#,
        )
        .unwrap_err();
    assert!(failure.message.contains("deliberate mask failure"));
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 512);
    host.lua.globals().set("kept_mask", Value::Nil).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    let budget = NativeBudget::isolated(64 * 512 + 1024);
    host.lua.set_app_data(budget.clone());
    let error = host
        .run(
            "mask-handles.lua",
            r#"
        local image=plum.images.from_rgba(1,1,string.char(1,2,3,128))
        for n=1,64 do image:mask{}:close() end
    "#,
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(error.message.contains("At most 64 image resources"));
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}

#[test]
fn mask_tiles_and_histogram_workspace_stay_bounded() {
    let host = Host::new(
        Limits {
            memory: 16 * 1024 * 1024,
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    host.run("mask-tiles.lua",r#"
        local image=plum.images.from_rgba(1024,1025,string.rep(string.char(255,255,255,128),1024*1025))
        local mask=image:mask{kind='luminance'}
        assert(mask:info().bytes==1024*1025)
        assert(not pcall(function()mask:read{}end))
        local tile=mask:read{width=1024,height=1024}
        assert(#tile==plum.images.max_region_bytes and tile:sub(1,1)==string.char(128) and tile:sub(-1)==string.char(128))
        local h=image:histogram{channel='alpha',max_samples=1000}
        assert(h.examined==1000 and h.samples==1000 and h.estimated and h.counts[129]==1000)
        assert(mask:bounds(129)==nil and mask:bounds(128).count==1024*1025)
    "#).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    let budget = NativeBudget::isolated(524);
    host.lua.set_app_data(budget.clone());
    host.run(
        "histogram-quota.lua",
        r#"
        local image=plum.images.from_rgba(1,1,string.char(0,0,0,255))
        assert(image:histogram{bins=1}.counts[1]==1)
        assert(not pcall(function()image:histogram{bins=2}end))
        assert(image:pixel(0,0)==0)
    "#,
    )
    .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}

#[test]
fn connected_regions_have_deterministic_connectivity_roi_and_thresholds() {
    execute(
        r#"
        local bytes={}
        for y=0,2 do for x=0,2 do bytes[#bytes+1]=string.char(0,0,0,x==y and (x==1 and 128 or 255) or 0) end end
        local image=plum.images.from_rgba(3,3,table.concat(bytes))
        local mask=image:mask{}
        local four=mask:regions()
        assert(#four==3)
        for n=1,3 do local r=four[n];assert(r.x==n-1 and r.y==n-1 and r.width==1 and r.height==1 and r.count==1) end
        local eight=mask:regions{connectivity=8}
        assert(#eight==1 and eight[1].x==0 and eight[1].y==0 and eight[1].width==3 and eight[1].height==3 and eight[1].count==3)
        local opaque=mask:regions{connectivity=8,threshold=200}
        assert(#opaque==2 and opaque[1].x==0 and opaque[2].x==2)
        local roi=mask:regions{x=1,y=1,width=2,height=2,connectivity=8}
        assert(#roi==1 and roi[1].x==1 and roi[1].y==1 and roi[1].width==2 and roi[1].height==2 and roi[1].count==2)
        assert(not pcall(function()mask:regions{max_regions=2}end))
        assert(mask:pixel(1,1)==128)
        assert(#plum.images.from_rgba(1,1,string.char(0,0,0,0)):mask{}:regions()==0)
        local dense=plum.images.from_rgba(2,2,string.rep(string.char(0,0,0,255),4)):mask{}:regions{max_regions=1}
        assert(#dense==1 and dense[1].width==2 and dense[1].height==2 and dense[1].count==4)
        for _,options in ipairs({{threshold=0},{connectivity=5},{max_regions=0},{max_regions=4097},{x=3},{width=0},{extra=true}}) do
            assert(not pcall(function()mask:regions(options)end))
        end
    "#,
    );
}

#[test]
fn region_count_and_native_workspace_caps_fail_without_partial_results() {
    let host = Host::new(Limits::default(), []).unwrap();
    let budget = NativeBudget::isolated(1054);
    host.lua.set_app_data(budget.clone());
    host.run(
        "region-budget.lua",
        r#"
        local image=plum.images.from_rgba(1,1,string.char(0,0,0,255))
        local mask=image:mask{}
        assert(mask:regions{max_regions=1}[1].count==1)
        assert(not pcall(function()mask:regions{max_regions=2}end))
        assert(mask:pixel(0,0)==255)
        assert(mask:regions{max_regions=1}[1].count==1)
    "#,
    )
    .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    let host = Host::new(Limits::default(), []).unwrap();
    let error=host.run("region-count.lua",r#"
        local pixels={}
        for y=0,128 do for x=0,128 do pixels[#pixels+1]=string.char(0,0,0,(x+y)%2==0 and 255 or 0) end end
        checker=plum.images.from_rgba(129,129,table.concat(pixels)):mask{}
        assert(plum.images.max_regions==4096)
        local connected=checker:regions{connectivity=8,max_regions=1}
        assert(#connected==1 and connected[1].count==8321)
        checker:regions{connectivity=4}
    "#).unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(error.message.contains("region count exceeded"));
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 512);
    host.lua.globals().set("checker", Value::Nil).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
