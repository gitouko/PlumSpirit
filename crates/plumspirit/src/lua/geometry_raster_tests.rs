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
    h.lua.set_app_data(NativeBudget::isolated(2 * 1024 * 1024));
    h
}
fn run(h: &Host, s: &str) {
    h.run("raster.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn polygon_rasterization_preserves_native_row_centres_rounding_and_thin_fallback() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local grid={columns=8,rows=4}
      local p={x={0,4,4,0},y={0,0,3,3}}
      local r=g.rasterize_polygon(p,grid);assert(r.count==3 and r.cell_count==12)
      for i=1,3 do assert(r.row[i]==i-1 and r.first[i]==0 and r.last[i]==4)end
      local tri=g.rasterize_polygon({x={0,8,4},y={0,0,4}},grid)
      assert(tri.count==4 and tri.first[1]==1 and tri.last[1]==8 and tri.row[4]==3)
      local tiny=g.rasterize_polygon({x={0.1,0.2,0.2,0.1},y={0,0,1,1}},grid)
      assert(tiny.count==1 and tiny.first[1]==0 and tiny.last[1]==1 and tiny.cell_count==1)
      local empty=g.rasterize_polygon({x={0,4,4,0},y={0,0,0.4,0.4}},grid);assert(empty.count==0 and empty.cell_count==0)
      local mapped=g.rasterize_polygon({x={10,18,18,10},y={20,20,26,26}},{origin_x=10,origin_y=20,cell_width=2,cell_height=2,columns=8,rows=4})
      assert(mapped.count==r.count and mapped.cell_count==r.cell_count)
      for i=1,r.count do assert(mapped.row[i]==r.row[i] and mapped.first[i]==r.first[i] and mapped.last[i]==r.last[i])end
      assert(p.x[1]==0 and grid.columns==8);r.first[1]=99;assert(mapped.first[1]==0)
    "#,
    );
    clear(&h);
}
#[test]
fn polylines_clip_union_closed_edges_and_keep_native_merging_rules() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local grid={columns=4,rows=4}
      local d=g.rasterize_polyline({x={0,4},y={0,4}},grid);assert(d.count==4 and d.cell_count==4)
      for i=1,4 do assert(d.row[i]==i-1 and d.first[i]==i-1 and d.last[i]==i)end
      local p=g.rasterize_polyline({x={-5,2,20},y={0.5,0.5,0.5}},grid);assert(p.count==1 and p.first[1]==0 and p.last[1]==4)
      local one=g.rasterize_polyline({x={2},y={2}},grid);assert(one.count==1 and one.row[1]==2 and one.first[1]==2 and one.last[1]==3)
      local outline={x={0.5,3.5,3.5,0.5},y={0.5,0.5,3.5,3.5}}
      local open=g.rasterize_polyline(outline,grid);local closed=g.rasterize_polyline(outline,grid,{closed=true})
      -- The closing corner merges before quantization, so the top-left cell is
      -- deliberately absent under the existing native stroke rule.
      assert(closed.cell_count>open.cell_count and closed.cell_count==11 and closed.count==6)
      outline.x[5]=outline.x[1];outline.y[5]=outline.y[1]
      local repeated=g.rasterize_polyline(outline,grid,{closed=true});assert(repeated.count==closed.count and repeated.cell_count==closed.cell_count)
      local overlap=g.rasterize_polyline({x={0,3,0,3},y={1.5,1.5,1.5,1.5}},grid);assert(overlap.count==1 and overlap.cell_count==3)
      local outside=g.rasterize_polyline({x={-10,-5},y={2,2}},grid);assert(outside.count==0)
    "#,
    );
    clear(&h);
}
#[test]
fn even_odd_self_crossing_degeneracy_and_flattened_path_composition_are_deterministic() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local grid={columns=16,rows=8}
      local twice={x={0,4,4,0,0,4,4,0},y={0,0,4,4,0,0,4,4}}
      assert(g.rasterize_polygon(twice,grid).count==0)
      assert(g.rasterize_polygon({x={2,2,2},y={1,1,1}},grid).count==0)
      local bow={x={0,4,0,4},y={0,4,4,0}};local a=g.rasterize_polygon(bow,grid);assert(a.count==4)
      local path={start={x=0,y=0},closed=true,segments={{kind='line',x=8,y=0},{kind='cubic',x1=12,y1=2,x2=4,y2=6,x=0,y=4}}}
      local points=g.flatten_path(path,{tolerance=0.001});local r=g.rasterize_polygon(points,grid);local replay=g.rasterize_polygon(points,grid)
      assert(r.count>0 and r.cell_count>0 and r.count==replay.count)
      for i=1,r.count do assert(r.row[i]==replay.row[i] and r.first[i]==replay.first[i] and r.last[i]==replay.last[i]);assert(r.first[i]>=0 and r.last[i]<=16 and r.first[i]<r.last[i]);if i>1 and r.row[i]==r.row[i-1]then assert(r.first[i]>r.last[i-1])end end
      assert(points.x[1]==0 and path.segments[2].x1==12)
    "#,
    );
    clear(&h);
}
#[test]
fn raster_bounds_work_spans_malformed_inputs_and_native_preflight_fail_atomically() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={0,4,4,0},y={0,0,4,4}}
      assert(g.max_raster_columns==65536 and g.max_raster_rows==4096 and g.max_raster_spans==65536 and g.max_raster_work==4194304)
      for _,grid in ipairs({{}, {columns=0,rows=4},{columns=65537,rows=1},{columns=4,rows=4097},{columns=4.0,rows=4},{columns=4,rows=4,cell_width=0},{columns=4,rows=4,cell_height=-1},{columns=4,rows=4,origin_x=math.huge},{columns=4,rows=4,max_spans=65537},{columns=4,rows=4,extra=1},setmetatable({columns=4,rows=4},{})})do assert(not pcall(g.rasterize_polygon,p,grid))end
      assert(not pcall(g.rasterize_polygon,{x={0,1},y={0,1}},{columns=4,rows=4}))
      assert(not pcall(g.rasterize_polygon,p,{columns=4,rows=4,cell_width=1e-300}))
      assert(not pcall(g.rasterize_polyline,p,{columns=4,rows=4},{closed=1}))
      local ok,e=pcall(g.rasterize_polygon,p,{columns=4,rows=4,max_spans=3});assert(not ok and tostring(e):find('max_spans'))
      local many={x={},y={}};for i=1,4096 do many.x[i]=i%8;many.y[i]=i%4 end
      ok,e=pcall(g.rasterize_polygon,many,{columns=4,rows=4096});assert(not ok and tostring(e):find('vertex%-row'))
      many.x[4097]=0;many.y[4097]=0;assert(not pcall(g.rasterize_polygon,many,{columns=4,rows=4}))
      local max=g.rasterize_polygon({x={0,65536,65536,0},y={0,0,4096,4096}},{columns=65536,rows=4096,max_spans=4096});assert(max.count==4096 and max.cell_count==268435456)
    "#,
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(2048));
    let e = h
        .run(
            "quota.lua",
            "plum.geometry.rasterize_polygon({x={0,4,4,0},y={0,0,4,4}},{columns=4,rows=4})",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Budget);
    clear(&h);
}
#[test]
fn cancellation_reaches_native_empty_scanlines_and_long_stroke_row_loops() {
    for fill in [true, false] {
        let h = host();
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled",
                h.lua
                    .create_function(move |lua, t: Table| {
                        let grid = lua.create_table()?;
                        grid.raw_set("columns", 4)?;
                        grid.raw_set("rows", 4096)?;
                        let mut calls = 0;
                        let result = raster_controlled(lua, &t, &grid, fill, false, || {
                            calls += 1;
                            if calls == 8 {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            crate::lua::notes::check_budget(lua)
                        });
                        assert_eq!(calls, 8);
                        result
                    })
                    .unwrap(),
            )
            .unwrap();
        let script = if fill {
            "controlled({x={1,1,1},y={0,4096,4096}})"
        } else {
            "controlled({x={1,1},y={0,4096}})"
        };
        assert_eq!(
            h.run("cancel.lua", script).unwrap_err().category,
            Category::Canceled
        );
        clear(&h);
        run(
            &h,
            "assert(plum.geometry.rasterize_polyline({x={0},y={0}},{columns=1,rows=1}).cell_count==1)",
        );
        clear(&h);
    }
}

#[test]
fn regions_preserve_holes_winding_orientation_shared_edges_and_native_polygon_parity() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local grid={columns=8,rows=8}
      local outer={x={0,8,8,0},y={0,0,8,8}}
      local inner={x={2,6,6,2},y={2,2,6,6}}
      local reversed={x={2,2,6,6},y={2,6,6,2}}
      local function cells(r)local t={}for i=1,r.count do for x=r.first[i],r.last[i]-1 do t[r.row[i]*8+x]=true end end return t end
      local hole=g.rasterize_region({outer,inner},grid)
      assert(hole.cell_count==48 and hole.count==12)
      local c=cells(hole);assert(c[0] and c[2*8+1] and not c[2*8+2] and not c[5*8+5] and c[6*8+2])
      local nz=g.rasterize_region({outer,inner},grid,{rule='non_zero'});assert(nz.cell_count==64 and nz.count==8)
      local nh=g.rasterize_region({outer,reversed},grid,{rule='non_zero'});assert(nh.cell_count==48)
      local eo=g.rasterize_region({outer,reversed},grid,{rule='even_odd'});assert(eo.cell_count==48)
      assert(g.rasterize_region({outer,outer},grid).count==0)
      assert(g.rasterize_region({outer,outer},grid,{rule='non_zero'}).cell_count==64)
      local left={x={0,4,4,0},y={0,0,4,4}};local right={x={4,8,8,4},y={0,0,4,4}}
      assert(g.rasterize_region({left,right},grid).cell_count==32)
      for _,p in ipairs({outer,inner,{x={0,8,4},y={0,0,8}},{x={0,8,0,8},y={0,8,8,0}}, {x={0.1,0.2,0.2,0.1},y={0,0,1,1}}})do
        local a=g.rasterize_polygon(p,grid);local b=g.rasterize_region({p},grid)
        assert(a.count==b.count and a.cell_count==b.cell_count)
        for i=1,a.count do assert(a.row[i]==b.row[i] and a.first[i]==b.first[i] and a.last[i]==b.last[i])end
      end
      local mapped={};for i,p in ipairs({outer,inner})do mapped[i]={x={},y={}};for j=1,#p.x do mapped[i].x[j]=p.x[j]*2+10;mapped[i].y[j]=p.y[j]*3+20 end end
      local mr=g.rasterize_region(mapped,{columns=8,rows=8,origin_x=10,origin_y=20,cell_width=2,cell_height=3})
      assert(mr.count==hole.count and mr.cell_count==hole.cell_count)
      for i=1,hole.count do assert(mr.row[i]==hole.row[i] and mr.first[i]==hole.first[i] and mr.last[i]==hole.last[i])end
      assert(outer.x[1]==0);mr.first[1]=99;assert(hole.first[1]==0)
    "#,
    );
    clear(&h);
}
#[test]
fn region_total_input_work_output_limits_and_private_native_quota_are_bounded() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={0,4,4,0},y={0,0,4,4}};local grid={columns=4,rows=4}
      assert(g.max_region_contours==256)
      for _,v in ipairs({{},setmetatable({p},{}),{[2]=p},{p,extra=p},{{x={0,1},y={0,1}}},{{x={0,1,2},y={0,1}}}})do assert(not pcall(g.rasterize_region,v,grid))end
      for _,o in ipairs({{rule='winding'},{rule=1},{extra=true},setmetatable({},{})})do assert(not pcall(g.rasterize_region,{p},grid,o))end
      local cs={};for i=1,257 do cs[i]=p end;assert(not pcall(g.rasterize_region,cs,grid))
      local big={x={},y={}};for i=1,4096 do big.x[i]=i%4;big.y[i]=i%4 end
      assert(not pcall(g.rasterize_region,{big,p},grid))
      local ok,e=pcall(g.rasterize_region,{big},{columns=4,rows=4096});assert(not ok and tostring(e):find('vertex%-row'))
      ok,e=pcall(g.rasterize_region,{p},{columns=4,rows=4,max_spans=3});assert(not ok and tostring(e):find('max_spans'))
      assert(not pcall(g.rasterize_region,{p},{columns=4,rows=4,cell_width=1e-300}))
      local off={x={-8,-4,-4,-8},y={0,0,4,4}};assert(g.rasterize_region({off},grid).count==0)
      local max=g.rasterize_region({{x={0,65536,65536,0},y={0,0,4096,4096}}},{columns=65536,rows=4096,max_spans=4096})
      assert(max.count==4096 and max.cell_count==268435456)
    "#,
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(4096));
    assert_eq!(
        h.run(
            "region-quota.lua",
            "plum.geometry.rasterize_region({{x={0,4,4,0},y={0,0,4,4}}},{columns=4,rows=4})"
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
}
#[test]
fn region_cancel_during_empty_scan_rows_discards_and_releases_then_host_reuses() {
    let h = host();
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled_region",
            h.lua
                .create_function(move |lua, (cs, grid): (Table, Table)| {
                    let mut calls = 0;
                    let r = region_controlled(lua, &cs, &grid, None, || {
                        calls += 1;
                        if calls == 12 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        crate::lua::notes::check_budget(lua)
                    });
                    assert_eq!(calls, 12);
                    r
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run(
            "region-cancel.lua",
            "controlled_region({{x={1,1,1},y={0,4096,4096}}},{columns=4,rows=4096})"
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    clear(&h);
    run(
        &h,
        "assert(plum.geometry.rasterize_region({{x={0,4,4,0},y={0,0,4,4}}},{columns=4,rows=4}).cell_count==16)",
    );
    clear(&h);
}

#[test]
fn region_seeded_single_contours_match_native_fill_and_total_vertex_boundary_is_usable() {
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(8 * 1024 * 1024));
    run(
        &h,
        r#"
      local g=plum.geometry;local seed=71831
      local function rnd()seed=(seed*48271)%2147483647;return seed/2147483647 end
      for sample=1,128 do
        local p={x={},y={}};for i=1,3+sample%29 do p.x[i]=rnd()*30-7;p.y[i]=rnd()*20-5 end
        local a=g.rasterize_polygon(p,{columns=16,rows=12});local b=g.rasterize_region({p},{columns=16,rows=12})
        assert(a.count==b.count and a.cell_count==b.cell_count,'seeded fill '..sample)
        for i=1,a.count do assert(a.row[i]==b.row[i] and a.first[i]==b.first[i] and a.last[i]==b.last[i])end
      end
      local many={};local p={x={0,4,4,0},y={0,0,4,4}};for i=1,256 do many[i]=p end
      assert(g.rasterize_region(many,{columns=4,rows=4}).count==0)
      local big={x={},y={}};for i=1,4096 do local j=(i-1)%4+1;big.x[i]=p.x[j];big.y[i]=p.y[j]end
      assert(g.rasterize_region({big},{columns=4,rows=1}).count==0)
      assert(g.rasterize_region({big},{columns=4,rows=1},{rule='non_zero'}).cell_count==4)
    "#,
    );
    clear(&h);
}
