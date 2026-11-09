use super::super::tests::{clear, host, run};
use super::*;
use std::cell::RefCell;
use std::rc::Rc;
fn inspect(h: &Host, f: impl Fn(&Arc<Resource>) -> mlua::Result<()> + 'static) {
    h.lua
        .globals()
        .set(
            "inspect",
            h.lua
                .create_function(move |_, u: AnyUserData| f(&u.borrow::<UiImage>()?.0))
                .unwrap(),
        )
        .unwrap();
}
const SOURCE: &str = "local src=plum.images.from_rgba(2,2,string.char(255,0,0,255,0,255,0,128,0,0,255,0,10,20,30,255));";
#[test]
fn ui_image_permission_user_scope_strict_options_and_opaque_primitive_validation() {
    let (h, _) = host(false, true);
    assert_eq!(
        h.run("denied.lua", &(SOURCE.to_owned() + "plum.ui.image(src)"))
            .unwrap_err()
            .category,
        Category::Capability
    );
    clear(&h);
    let (h, _) = host(true, false);
    assert_eq!(
        h.run("automatic.lua", &(SOURCE.to_owned() + "plum.ui.image(src)"))
            .unwrap_err()
            .category,
        Category::Capability
    );
    clear(&h);
    let (h, _) = host(true, true);
    h.lua
        .globals()
        .set(
            "validate",
            h.lua
                .create_function(|lua, t: Table| schema::parse(lua, t).map(|_| true))
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        &(SOURCE.to_owned()
            + r#"
      for _,options in ipairs{{max_side=0},{max_side=1025},{max_side=1.0},{max_side=0/0},{filter='box'},{filter=true},{unknown=1},setmetatable({},{})}do
        assert(not pcall(plum.ui.image,src,options))
      end
      assert(not pcall(plum.ui.image,{}));assert(not pcall(plum.ui.image,src:info()))
      local image=plum.ui.image(src,{filter='nearest',max_side=2})
      local function spec(p)return{content={id='canvas',kind='canvas',primitives={p}}}end
      local function primitive()return{kind='image',image=image,x1=0,y1=0,x2=1,y2=1}end
      assert(validate(spec(primitive())))
      for k,v in pairs{image=src,x1=2,u1=-1,v2=2,u2=0,y2=0,unknown=1}do
        local p=primitive();p[k]=v;assert(not pcall(validate,spec(p)),k)
      end
      local p=primitive();p.color={255,255,255,128};p.u1=.5;assert(validate(spec(p)))
      image:close();assert(not pcall(validate,spec(primitive())))
    "#),
    );
    clear(&h);
}
#[test]
fn worker_converts_exact_alpha_once_and_native_texture_is_shared_with_bounded_upload() {
    let (h, _) = host(true, true);
    let ctx = egui::Context::default();
    let other = ctx.clone();
    inspect(&h, move |r| {
        let data = r.data.lock().unwrap();
        let pixels = &data.as_ref().unwrap().pixels.image;
        assert_eq!(pixels.size, [2, 2]);
        for (actual, p) in pixels.pixels.iter().zip([
            [255, 0, 0, 255],
            [0, 255, 0, 128],
            [0, 0, 255, 0],
            [10, 20, 30, 255],
        ]) {
            assert_eq!(
                *actual,
                Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
            );
        }
        drop(data);
        let mut left = 3;
        assert!(r.prepare(&ctx, &mut left).is_none());
        assert_eq!(left, 3);
        left = 4;
        let a = r.prepare(&ctx, &mut left).unwrap();
        assert_eq!(left, 0);
        let b = r.prepare(&other, &mut left).unwrap();
        assert_eq!(a.texture.id(), b.texture.id());
        assert_eq!(r.uploads.load(Ordering::Relaxed), 1);
        Ok(())
    });
    run(
        &h,
        &(SOURCE.to_owned()
            + r#"
      local image=plum.ui.image(src)
      src:close();local i=image:info()
      assert(i.width==2 and i.height==2 and i.source_width==2 and i.source_height==2 and i.bytes==16 and i.origin=='top_left' and i.filter=='linear')
      inspect(image);assert(image:close());assert(not image:close());assert(not pcall(function()image:info()end))
    "#),
    );
    clear(&h);
}
#[test]
fn downsampling_uses_native_box_math_and_keeps_aspect_without_upscaling() {
    let (h, _) = host(true, true);
    inspect(&h, |r| {
        assert_eq!((r.width, r.height), (2, 1));
        assert_eq!(r.source, [4, 2]);
        let src =
            image_midi::Rgba::new(4, 2, (0..8).map(|i| [i * 30, 10, 200, 128]).collect()).unwrap();
        let expected = image_midi::box_resize(&src, 2, 1);
        let data = r.data.lock().unwrap();
        let actual = &data.as_ref().unwrap().pixels.image;
        for (a, p) in actual.pixels.iter().zip(expected.px) {
            assert_eq!(*a, Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]));
        }
        Ok(())
    });
    run(
        &h,
        r#"local b={};for i=0,7 do b[#b+1]=string.char(i*30,10,200,128)end
      local src=plum.images.from_rgba(4,2,table.concat(b));local image=plum.ui.image(src,{max_side=2});inspect(image)
      local tiny=plum.ui.image(src,{max_side=1024});assert(tiny:info().width==4 and tiny:info().height==2)
    "#,
    );
    clear(&h);
}
#[test]
fn image_close_job_expiry_retained_gc_and_frontend_accounting_have_explicit_lifetimes() {
    let (h, _) = host(true, true);
    let held = Rc::new(RefCell::new(None));
    let copy = Rc::clone(&held);
    inspect(&h, move |r| {
        let mut left = MAX_UPLOAD_PIXELS;
        let ctx = egui::Context::default();
        let prepared = r.prepare(&ctx, &mut left).unwrap();
        *copy.borrow_mut() = Some(prepared);
        Ok(())
    });
    run(
        &h,
        &(SOURCE.to_owned() + "retained=plum.ui.image(src);inspect(retained)"),
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        METADATA_BYTES + 4 * 16
    );
    run(
        &h,
        "assert(not pcall(function()retained:info()end));assert(not retained:close());retained=nil",
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 4 * 16);
    held.borrow_mut().take();
    clear(&h);
}
#[test]
fn image_creation_count_cumulative_pixel_and_native_preflight_limits_cannot_be_bypassed_by_close() {
    let (h, _) = host(true, true);
    run(
        &h,
        r#"local src=plum.images.from_rgba(1,1,string.char(1,2,3,255));for i=1,16 do plum.ui.image(src):close()end
      assert(not pcall(plum.ui.image,src))"#,
    );
    clear(&h);
    let (h, _) = host(true, true);
    run(
        &h,
        r#"local src=plum.images.from_rgba(1024,1024,string.rep(string.char(1,2,3,255),1024*1024))
      for i=1,4 do plum.ui.image(src,{max_side=1024}):close()end
      assert(not pcall(plum.ui.image,src,{max_side=1024}))"#,
    );
    clear(&h);
    let (h, _) = host(true, true);
    h.lua.set_app_data(NativeBudget::isolated(2048));
    assert_eq!(
        h.run(
            "image-quota.lua",
            &(SOURCE.to_owned() + "plum.ui.image(src)")
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
}

#[test]
fn cancellation_inside_native_conversion_discards_partial_pixels_and_host_recovers() {
    let (h, _) = host(true, true);
    let hits = Arc::new(AtomicUsize::new(0));
    h.lua.set_app_data(TestCheckpoint {
        hits: Arc::clone(&hits),
        cancel: h.cancel_handle(),
        at: 3,
    });
    assert_eq!(h.run("cancel-image.lua",r#"local src=plum.images.from_rgba(1024,1024,string.rep(string.char(1,2,3,255),1024*1024));plum.ui.image(src,{max_side=1024})"#).unwrap_err().category,Category::Canceled);
    assert_eq!(hits.load(Ordering::Relaxed), 3);
    clear(&h);
    h.lua.remove_app_data::<TestCheckpoint>();
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    run(
        &h,
        &(SOURCE.to_owned() + "assert(plum.ui.image(src):info().width==2)"),
    );
    clear(&h);
}

#[test]
fn renderer_side_limit_becomes_actionable_fatal_budget_error_and_cleans_resources() {
    let (h, _) = host(true, true);
    inspect(&h, |resource| {
        let ctx = egui::Context::default();
        ctx.input_mut(|i| i.max_texture_side = 1);
        let mut left = MAX_UPLOAD_PIXELS;
        assert!(resource.prepare(&ctx, &mut left).is_none());
        assert_eq!(left, MAX_UPLOAD_PIXELS);
        assert_eq!(resource.uploads.load(Ordering::Relaxed), 0);
        Ok(())
    });
    let e = h
        .run(
            "small-renderer.lua",
            &(SOURCE.to_owned() + "local image=plum.ui.image(src);inspect(image)"),
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Budget);
    assert!(e.message.contains("smaller max_side"));
    clear(&h);
}

#[test]
fn installed_image_laboratory_command_reuses_resource_for_crop_opacity_and_pointer_state() {
    let (h, rx) = host(true, true);
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: "script".into(),
        name: "Image example".into(),
        version: "0.2.0".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    // set_plugin installs the production per-id pool; parallel fixture plugins
    // called "script" must not share that pool for zero-after-job assertions.
    h.lua.set_app_data(NativeBudget::isolated(32 * 1024 * 1024));
    let driver = std::thread::spawn(move || {
        let shared = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut revision = 0;
        let mut resource_id = None;
        for step in 0..5 {
            let start = std::time::Instant::now();
            let snapshot = loop {
                if let Some(s) = shared.snapshot()
                    && s.revision > revision
                {
                    break s;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(3),
                    "example failed to publish step{step}"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            revision = snapshot.revision;
            let schema::Kind::Column(nodes) = &snapshot.root.kind else {
                panic!()
            };
            let canvas = nodes.iter().find(|n| n.id == "canvas").unwrap();
            let schema::Kind::Canvas { primitives, .. } = &canvas.kind else {
                panic!()
            };
            let (image, color, uv) = primitives
                .iter()
                .find_map(|p| match p {
                    schema::Primitive::Image {
                        resource,
                        color,
                        uv_min,
                        ..
                    } => Some((resource, color, uv_min)),
                    _ => None,
                })
                .unwrap();
            if let Some(id) = resource_id {
                assert_eq!(id, image.id);
            } else {
                resource_id = Some(image.id);
            }
            if step >= 1 {
                assert_eq!(color[3], 239);
            }
            if step >= 2 {
                assert_eq!(*uv, [0.2, 0.2]);
            }
            if step >= 3 {
                assert!(primitives.iter().any(
                    |p| matches!(p,schema::Primitive::Circle{center,..} if *center==[0.3,0.4])
                ));
            }
            let mut e = Event {
                kind: "change",
                id: String::new(),
                value: InputValue::None,
                pointer: None,
                revision,
                sequence: 0,
            };
            match step {
                0 => {
                    e.id = "opacity".into();
                    e.value = InputValue::Number(239.);
                }
                1 => {
                    e.id = "crop".into();
                    e.value = InputValue::Number(0.2);
                }
                2 | 3 => {
                    e.id = "canvas".into();
                    e.kind = if step == 2 {
                        "pointer_down"
                    } else {
                        "pointer_up"
                    };
                    e.pointer = Some(Pointer {
                        x: 0.3,
                        y: 0.4,
                        width: 600.,
                        height: 320.,
                        buttons: if step == 2 { 1 } else { 0 },
                        shift: false,
                        ctrl: false,
                        alt: false,
                        command: false,
                        scroll_x: 0.,
                        scroll_y: 0.,
                        scroll_unit: "points",
                        time: 1.,
                    });
                }
                _ => {
                    e.id = "close".into();
                    e.kind = "activate";
                }
            }
            assert!(shared.push(e));
        }
    });
    let source = include_str!("../../examples-lua/ui-image-laboratory/main.lua");
    let result=h.run("image-laboratory.lua",&format!("{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('open',{{}})"));
    driver.join().unwrap();
    result.unwrap();
    assert!(h.take_commits().is_none());
    clear(&h);
}

#[test]
fn colorized_mask_uses_existing_native_premultiplied_texture_and_bounded_upload() {
    let (h, _) = host(true, true);
    inspect(&h, |r| {
        assert_eq!((r.width, r.height), (4, 1));
        let data = r.data.lock().unwrap();
        let image = &data.as_ref().unwrap().pixels.image;
        for (actual, alpha) in image.pixels.iter().zip([0, 32, 64, 128]) {
            assert_eq!(*actual, Color32::from_rgba_unmultiplied(17, 99, 201, alpha));
        }
        drop(data);
        let ctx = egui::Context::default();
        let mut left = 4;
        assert!(r.prepare(&ctx, &mut left).is_some());
        assert_eq!(left, 0);
        assert_eq!(r.uploads.load(Ordering::Relaxed), 1);
        Ok(())
    });
    run(
        &h,
        "local source=plum.images.from_rgba(4,1,string.char(0,0,0,0,0,0,0,64,0,0,0,128,0,0,0,255));local mask=source:mask{};local colored=mask:colorize{17,99,201,128};local image=plum.ui.image(colored,{filter='nearest'});source:close();mask:close();colored:close();inspect(image);image:close()",
    );
    clear(&h);
}

#[test]
fn composite_layers_convert_once_into_one_native_ui_image() {
    let (h, _) = host(true, true);
    inspect(&h, |r| {
        assert_eq!((r.width, r.height), (1, 1));
        let data = r.data.lock().unwrap();
        assert_eq!(
            data.as_ref().unwrap().pixels.image.pixels[0],
            Color32::from_rgba_unmultiplied(127, 0, 128, 255)
        );
        Ok(())
    });
    run(
        &h,
        "local red=plum.images.composite({},{width=1,height=1,background={255,0,0,255}});local blue=plum.images.composite({},{width=1,height=1,background={0,0,255,128}});local composite=plum.images.composite({{image=red},{image=blue}},{width=1,height=1});local image=plum.ui.image(composite);red:close();blue:close();composite:close();inspect(image);image:close()",
    );
    clear(&h);
}
