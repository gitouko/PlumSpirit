use super::*;
use crate::lua::{Capability, Host, Limits, World};

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
    h.run("geometry.lua", s).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn affine_primitives_are_the_shared_image_mapping_and_preserve_inverse_composition() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry
      local m=g.mapping{scale_x=-2,scale_y=3,rotate=37,pivot_x=2,pivot_y=-7,translate_x=11,translate_y=5}
      local image=plum.images.mapping{scale_x=-2,scale_y=3,rotate=37,pivot_x=2,pivot_y=-7,translate_x=11,translate_y=5}
      for _,key in ipairs({'a','b','c','d','e','f'})do assert(m[key]==image[key])end
      local inv=g.inverse(m);local identity=g.compose(m,inv)
      for i=-100,100 do local x,y=i/10,i*i/100;local a,b=g.map_point(m,x,y);local u,v=g.map_point(inv,a,b);assert(math.abs(u-x)<1e-10 and math.abs(v-y)<1e-10);u,v=g.map_point(identity,x,y);assert(math.abs(u-x)<1e-10 and math.abs(v-y)<1e-10)end
      local source={x={-2,0,7},y={-3,4,9}}
      local mapped=g.map_points(m,source);assert(mapped.count==3 and source.x[1]==-2)
      local back=g.map_points(inv,mapped)
      for i=1,3 do local x,y=g.map_point(m,source.x[i],source.y[i]);assert(x==mapped.x[i] and y==mapped.y[i]);assert(math.abs(back.x[i]-source.x[i])<1e-12 and math.abs(back.y[i]-source.y[i])<1e-12)end
      assert(not pcall(g.map_points,g.mapping{scale_x=1e6,scale_y=1e6},{x={0,1e12},y={0,1}}))
      assert(not pcall(g.mapping,{scale_x=0}))
      assert(not pcall(g.map_point,m,1e13,0))
      assert(not pcall(g.inverse,{a=1,b=1,c=1,d=1,e=0,f=0}))
    "#,
    );
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn native_box_clipping_and_boundary_hit_testing_are_deterministic() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local w={x_min=-2,y_min=-3,x_max=4,y_max=5}
      assert(g.contains(w,-2,-3) and g.contains(w,4,5) and not g.contains(w,4.01,5))
      local c=g.clip_segment(w,-10,0,10,0);assert(c.x0==-2 and c.x1==4 and c.y0==0 and math.abs(c.t0-0.4)<1e-15 and math.abs(c.t1-0.7)<1e-15)
      assert(g.clip_segment(w,-10,10,10,10)==nil)
      assert(g.clip_segment(w,1,1,1,1).t1==1)
      -- Native Draw excludes a segment touching only one corner (zero parameter interval).
      assert(g.clip_segment(w,-3,-2,-1,-4)==nil)
      for i=-50,50 do local c=g.clip_segment(w,-20,i/10,20,i/10);if c then assert(c.t0<=c.t1 and g.contains(w,c.x0,c.y0) and g.contains(w,c.x1,c.y1))end end
      assert(not pcall(g.contains,{x_min=2,y_min=0,x_max=1,y_max=1},0,0))
      assert(not pcall(g.contains,setmetatable(w,{}),0,0))
    "#,
    );
}

#[test]
fn bounded_polyline_length_resampling_endpoints_and_degenerate_paths_use_native_draw() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local p={x={0,3,3},y={0,0,4}}
      assert(g.polyline_length(p)==7)
      local s=g.resample_polyline(p,7);assert(s.count==8 and s.x[1]==0 and s.y[1]==0 and s.x[8]==3 and s.y[8]==4)
      for i=1,4 do assert(math.abs(s.x[i]-(i-1))<1e-12 and s.y[i]==0)end
      for i=5,8 do assert(s.x[i]==3 and math.abs(s.y[i]-(i-4))<1e-12)end
      local a=g.resample_polyline({x={7},y={-2}},100);assert(a.count==1 and a.x[1]==7 and a.y[1]==-2)
      a=g.resample_polyline({x={7,7,7},y={-2,-2,-2}},20);assert(a.count==21 and g.polyline_length(a)==0)
      local x,y={},{};for i=1,4096 do x[i]=i-1;y[i]=2 end
      local max=g.resample_polyline({x=x,y=y},4095);assert(max.count==4096 and max.x[4096]==4095 and max.y[1]==2)
      for i=1,10 do local a=g.resample_polyline(p,7);assert(a.x[8]==s.x[8] and a.y[8]==s.y[8])end
    "#,
    );
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn malformed_columns_limits_native_quota_and_cancellation_refuse_without_retained_memory() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry
      for _,p in ipairs({{}, {x={0},y={}}, {x={[1]=0,[3]=2},y={0,1,2}}, {x={0,k=1},y={0}}, {x={0/0},y={0}}, {x={math.huge},y={0}}, {x={1e13},y={0}}, {x={0},y={0},z=1}, {x=setmetatable({0},{}),y={0}}, {x={0},y={0},count=2}}) do assert(not pcall(g.polyline_length,p))end
      for _,n in ipairs({0,-1,4096,1.5,'2',2.0})do assert(not pcall(g.resample_polyline,{x={0,1},y={0,1}},n))end
      local x,y={},{};for i=1,4097 do x[i]=i;y[i]=i end;assert(not pcall(g.polyline_length,{x=x,y=y}))
    "#,
    );
    h.lua.set_app_data(NativeBudget::isolated(10));
    let e = h
        .run(
            "quota.lua",
            "plum.geometry.resample_polyline({x={0,1},y={0,1}},100)",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Budget);
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    h.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "cancel_geometry",
            h.lua
                .create_function(move |_, ()| {
                    cancel.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    let e = h
        .run(
            "cancel.lua",
            "cancel_geometry();plum.geometry.polyline_length({x={0,1},y={0,1}})",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Canceled);
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    run(
        &h,
        "assert(plum.geometry.polyline_length({x={0,3},y={0,4}})==5)",
    );
}

#[test]
fn author_written_example_executes_as_one_native_transaction_with_undo_redo() {
    use plumspirit_engine::history::{History, HistoryLimits};
    use plumspirit_engine::model::project::KeyRange;
    use plumspirit_engine::model::{Bpm, PlumDocument};
    use std::sync::Arc;
    for (command, key_range) in [
        "draw-path",
        "draw-cubic",
        "draw-mixed-path",
        "draw-adaptive-mask",
        "draw-adaptive-path",
        "draw-raster-cells",
    ]
    .into_iter()
    .flat_map(|command| [KeyRange::Standard, KeyRange::Extended256].map(|keys| (command, keys)))
    {
        let mut doc = Arc::new(PlumDocument::new_project(
            "Geometry".into(),
            Bpm::DEFAULT,
            960,
        ));
        Arc::get_mut(&mut doc).unwrap().project_mut().keys = key_range;
        let mut history = History::new(HistoryLimits::default());
        history.reset(true);
        history.observe_ids(&doc);
        let h = Host::new(
            Limits::default(),
            [Capability::ReadProject, Capability::EditNotes],
        )
        .unwrap();
        h.set_world(Some(World::new(
            Arc::clone(&doc),
            history.incarnations().snapshot(),
        )));
        h.set_plugin(crate::lua::plugin_api::PluginMeta {
            id: "plumspirit.example.geometry-primitives".into(),
            name: "Geometry Primitives".into(),
            version: "1".into(),
            root: std::env::temp_dir(),
            data_dir: None,
            config_version: 1,
        })
        .unwrap();
        let manifest = crate::lua::package::Manifest::parse(include_str!(
            "../../examples-lua/geometry-primitives/plum-plugin.toml"
        ))
        .unwrap();
        assert_eq!(manifest.id, "plumspirit.example.geometry-primitives");
        let source = include_str!("../../examples-lua/geometry-primitives/main.lua");
        run(
            &h,
            &format!(
                "{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('{command}',{{}})"
            ),
        );
        let preview = h.take_commits().unwrap();
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.note_count(), 0, "preview is private for {command}");
        drop(preview);
        h.set_world(Some(World::new(
            Arc::clone(&doc),
            history.incarnations().snapshot(),
        )));
        run(&h, &format!("plum.plugin.__run('{command}',{{}})"));
        let mut c = h.take_commits().unwrap();
        assert_eq!(
            c.result().export_fingerprint(),
            expected,
            "deterministic replay {command}"
        );
        assert_eq!(c.steps.len(), 1);
        assert_eq!(c.result().note_count(), 33);
        c.apply(&mut history, &mut doc).unwrap();
        drop(c);
        let hash = doc.export_fingerprint();
        assert_eq!(hash, expected, "Preview/Apply {command}");
        history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
        assert_eq!(doc.note_count(), 0);
        history.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
        assert_eq!(doc.export_fingerprint(), hash);
    }
}
