use super::*;
use crate::lua::{
    Host, Limits, Plain, Scripts, World,
    worker::{ScriptJob, ScriptWorker},
};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument},
};
use std::{cell::RefCell, rc::Rc, time::Duration};
const SOURCE: &str = "local src=plum.images.from_rgba(2,2,string.char(255,0,0,255,0,255,0,128,0,0,255,0,10,20,30,255));local image=plum.ui.image(src,{max_side=2,filter='nearest'});";
fn world() -> World {
    let doc = Arc::new(PlumDocument::new_project(
        "Retained pixels".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    World::new(doc, history.incarnations().snapshot())
}
fn host(caps: Vec<Capability>) -> (Host, NativeBudget) {
    let h = Host::new(
        Limits {
            memory: 32 * 1024 * 1024,
            time: Duration::from_secs(10),
            ..Default::default()
        },
        caps,
    )
    .unwrap();
    h.set_world(Some(world()));
    let budget = NativeBudget::isolated(128 * 1024 * 1024);
    h.lua.set_app_data(budget.clone());
    (h, budget)
}
fn run(h: &Host, source: &str) -> Result<Plain, super::super::super::ScriptError> {
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    h.run("retained.lua", source)
}
#[test]
fn retained_pixels_share_conversion_survive_source_job_and_use_one_native_texture() {
    let (h, budget) = host(vec![Capability::UiOverlay]);
    let saved = Rc::new(RefCell::new(None::<Arc<Resource>>));
    let sink = Rc::clone(&saved);
    h.lua
        .globals()
        .set(
            "inspect_pair",
            h.lua
                .create_function(move |_, (source, retained): (AnyUserData, AnyUserData)| {
                    let source = source.borrow::<UiImage>()?;
                    let retained = retained.borrow::<UiImage>()?;
                    let a = source.0.data.lock().unwrap();
                    let b = retained.0.data.lock().unwrap();
                    assert!(Arc::ptr_eq(
                        &a.as_ref().unwrap().pixels,
                        &b.as_ref().unwrap().pixels
                    ));
                    assert_ne!(source.0.id, retained.0.id);
                    *sink.borrow_mut() = Some(Arc::clone(&retained.0));
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(&h,&format!("{SOURCE} held=plum.ui.retain_image(image);original=image;inspect_pair(image,held);assert(held:info().lifetime=='project')")).unwrap();
    h.run("next.lua","assert(not pcall(function()original:info()end));assert(held:info().width==2);original=nil;collectgarbage('collect')").unwrap();
    assert_eq!(budget.used(), METADATA_BYTES + 4 * 16);
    let resource = saved.borrow().as_ref().unwrap().clone();
    assert!(resource.alive());
    let ctx = egui::Context::default();
    let mut prepared = None;
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let mut remaining = 4;
        let first = resource.prepare(ui.ctx(), &mut remaining).unwrap();
        assert_eq!(remaining, 0);
        let second = resource.prepare(ui.ctx(), &mut remaining).unwrap();
        assert_eq!(first.texture.id(), second.texture.id());
        assert_eq!(resource.test_uploads(), 1);
        prepared = Some(first);
    });
    output.textures_delta.clear();
    release_retained(&h.lua);
    assert!(!resource.alive());
    assert!(resource.data.lock().unwrap().is_none());
    assert!(
        budget.used() >= 4 * 16,
        "native texture consumer retains pixel charge"
    );
    drop(prepared);
    assert_eq!(budget.used(), METADATA_BYTES);
    h.run(
        "close.lua",
        "assert(not pcall(function()held:info()end));held=nil;collectgarbage('collect')",
    )
    .unwrap();
    saved.borrow_mut().take();
    drop(resource);
    assert_eq!(budget.used(), 0);
}
#[test]
fn an_already_uploaded_texture_is_shared_and_host_drop_closes_its_retained_resource() {
    let (h, budget) = host(vec![Capability::UiOverlay]);
    let ctx = egui::Context::default();
    let upload = ctx.clone();
    let source_id = Rc::new(RefCell::new(None));
    let source_sink = Rc::clone(&source_id);
    h.lua
        .globals()
        .set(
            "upload_source",
            h.lua
                .create_function(move |_, u: AnyUserData| {
                    let resource = Arc::clone(&u.borrow::<UiImage>()?.0);
                    let mut out = upload.run_ui(egui::RawInput::default(), |ui| {
                        let mut remaining = 4;
                        let prepared = resource.prepare(ui.ctx(), &mut remaining).unwrap();
                        *source_sink.borrow_mut() = Some(prepared.texture.id());
                        assert_eq!(resource.test_uploads(), 1);
                    });
                    out.textures_delta.clear();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        &format!("{SOURCE}upload_source(image);held=plum.ui.retain_image(image)"),
    )
    .unwrap();
    h.lua.gc_collect().unwrap();
    let u: AnyUserData = h.lua.globals().get("held").unwrap();
    let resource = Arc::clone(&u.borrow::<UiImage>().unwrap().0);
    drop(u);
    let mut prepared = None;
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let mut remaining = 0;
        let native = resource.prepare(ui.ctx(), &mut remaining).unwrap();
        assert_eq!(Some(native.texture.id()), *source_id.borrow());
        assert_eq!(resource.test_uploads(), 0);
        prepared = Some(native);
    });
    out.textures_delta.clear();
    let doc = Arc::clone(
        &h.lua
            .app_data_ref::<crate::lua::handles::WorldSlot>()
            .unwrap()
            .0
            .as_ref()
            .unwrap()
            .doc,
    );
    h.set_world(None);
    assert_eq!(
        Arc::strong_count(&doc),
        1,
        "retained pixels hold no document root"
    );
    drop(h);
    assert!(!resource.alive());
    assert!(resource.data.lock().unwrap().is_none());
    assert_eq!(budget.used(), METADATA_BYTES + 4 * 16);
    drop(resource);
    assert_eq!(budget.used(), 4 * 16);
    drop(prepared);
    assert_eq!(budget.used(), 0);
}
#[test]
fn retention_authority_and_project_scope_are_distinct_from_panel_opening_and_job_images() {
    let (h, _) = host(vec![Capability::UiPanels]);
    assert_eq!(
        run(&h, &format!("{SOURCE}plum.ui.retain_image(image)"))
            .unwrap_err()
            .category,
        Category::Capability
    );
    let (h, _) = host(vec![Capability::UiOverlay]);
    run(&h,&format!("{SOURCE}held=plum.ui.retain_image(image);assert(not pcall(function()plum.ui.open_panel{{content={{id='x',kind='label'}}}}end))")).unwrap();
    h.run("readonly.lua", "assert(held:info().height==2)")
        .unwrap();
    assert_eq!(
        h.run("readonly-promotion.lua", "plum.ui.retain_image(held)")
            .unwrap_err()
            .category,
        Category::Capability
    );
    h.set_world(Some(world()));
    assert!(h.run("other-project.lua", "return held:info()").is_err());
    h.set_world(None);
    assert!(run(&h, &format!("{SOURCE}plum.ui.retain_image(image)")).is_err());
}
#[test]
fn failed_or_canceled_new_retention_rolls_back_without_closing_an_older_asset() {
    let (h, budget) = host(vec![Capability::UiOverlay]);
    run(&h, &format!("{SOURCE}held=plum.ui.retain_image(image)")).unwrap();
    h.lua.gc_collect().unwrap();
    let before = budget.used();
    assert_eq!(before, METADATA_BYTES + 4 * 16);
    assert!(
        run(
            &h,
            &format!("{SOURCE}failed=plum.ui.retain_image(image);error('failed')")
        )
        .is_err()
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), before + METADATA_BYTES);
    h.run("recover.lua","assert(held:info().width==2);assert(not pcall(function()failed:info()end));failed=nil;collectgarbage('collect')").unwrap();
    assert_eq!(budget.used(), before);
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "cancel_now",
            h.lua
                .create_function(move |_, ()| {
                    cancel.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        run(
            &h,
            &format!("{SOURCE}canceled=plum.ui.retain_image(image);cancel_now()")
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    h.run("recover-cancel.lua","assert(held:info().width==2);assert(not pcall(function()canceled:info()end));canceled=nil;held:close();held=nil;collectgarbage('collect')").unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn retention_aliases_are_shared_but_attempts_and_live_resources_remain_bounded() {
    let (h, _) = host(vec![Capability::UiOverlay]);
    run(&h,&format!("{SOURCE}held=plum.ui.retain_image(image);for i=2,16 do assert(plum.ui.retain_image(image):info().width==2)end;assert(not pcall(function()plum.ui.retain_image(image)end))")).unwrap();
    assert_eq!(
        h.lua
            .app_data_ref::<RetainedSlot>()
            .unwrap()
            .resources
            .len(),
        1
    );
    run(&h,"all={};for i=1,16 do local src=plum.images.from_rgba(1,1,string.char(i,0,0,255));local u=plum.ui.image(src);if i<16 then all[i]=plum.ui.retain_image(u)else assert(not pcall(function()plum.ui.retain_image(u)end))end end").unwrap();
    assert_eq!(
        h.lua
            .app_data_ref::<RetainedSlot>()
            .unwrap()
            .resources
            .len(),
        16
    );
    h.run("release-one.lua", "held:close();held=nil").unwrap();
    run(
        &h,
        &format!("{SOURCE}replacement=plum.ui.retain_image(image)"),
    )
    .unwrap();
    assert_eq!(
        h.lua
            .app_data_ref::<RetainedSlot>()
            .unwrap()
            .resources
            .len(),
        16
    );
}
#[test]
fn retained_pixel_limit_and_metadata_preflight_are_enforced_before_promotion() {
    let (h, _) = host(vec![Capability::UiOverlay]);
    for _ in 0..4 {
        run(&h,"local src=plum.images.from_rgba(1024,1024,string.rep(string.char(0,0,0,255),1024*1024));all=all or {};all[#all+1]=plum.ui.retain_image(plum.ui.image(src,{max_side=1024}))").unwrap();
    }
    assert!(run(&h, &format!("{SOURCE}plum.ui.retain_image(image)")).is_err());
    release_retained(&h.lua);
    h.lua.gc_collect().unwrap();
    let (h, budget) = host(vec![Capability::UiOverlay]);
    let blocker = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&blocker);
    let account = budget.clone();
    h.lua
        .globals()
        .set(
            "fill_budget",
            h.lua
                .create_function(move |_, ()| {
                    *slot.borrow_mut() = Some(
                        account
                            .reserve(128 * 1024 * 1024 - account.used() - (METADATA_BYTES - 1))
                            .unwrap(),
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        run(
            &h,
            &format!("{SOURCE}fill_budget();plum.ui.retain_image(image)")
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    blocker.borrow_mut().take();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn native_worker_project_release_invalidates_retained_assets_without_a_lua_callback() {
    let mut worker = ScriptWorker::spawn(
        Limits::default(),
        vec![Capability::UiOverlay],
        &Scripts::default(),
    )
    .unwrap();
    let world = world();
    worker
        .submit_with_interaction(
            ScriptJob {
                file: "retain.lua".into(),
                source: format!("{SOURCE}held=plum.ui.retain_image(image)"),
                world: Some(world.clone()),
                time_limit: None,
            },
            true,
        )
        .unwrap();
    worker.wait(Duration::from_secs(3)).unwrap().result.unwrap();
    worker
        .submit(ScriptJob {
            file: "use.lua".into(),
            source: "assert(held:info().width==2)".into(),
            world: Some(world.clone()),
            time_limit: None,
        })
        .unwrap();
    worker.wait(Duration::from_secs(3)).unwrap().result.unwrap();
    for _ in 0..100 {
        worker.release_ui_images();
    }
    worker
        .submit(ScriptJob {
            file: "closed.lua".into(),
            source: "assert(not pcall(function()held:info()end));return 'released'".into(),
            world: Some(world),
            time_limit: None,
        })
        .unwrap();
    assert_eq!(
        worker.wait(Duration::from_secs(3)).unwrap().result.unwrap(),
        Plain::String("released".into())
    );
}
#[test]
fn release_generation_rejects_preclose_queued_work_and_postcheck_commit_without_revival() {
    let (h, budget) = host(vec![Capability::UiOverlay]);
    run(&h, &format!("{SOURCE}held=plum.ui.retain_image(image)")).unwrap();
    let epoch = Arc::clone(
        &h.lua
            .app_data_ref::<RetainedSlot>()
            .unwrap()
            .owner
            .release_epoch,
    );
    let old = epoch.load(Ordering::Acquire);
    epoch.fetch_add(1, Ordering::AcqRel);
    let u: AnyUserData = h.lua.globals().get("held").unwrap();
    assert!(!u.borrow::<UiImage>().unwrap().0.alive());
    drop(u);
    set_run_epoch(&h.lua, old);
    assert_eq!(
        run(&h, &format!("{SOURCE}stale=plum.ui.retain_image(image)"))
            .unwrap_err()
            .category,
        Category::Expired
    );
    release_retained(&h.lua);
    let signal = Arc::clone(&epoch);
    h.lua
        .globals()
        .set(
            "close_generation",
            h.lua
                .create_function(move |_, ()| {
                    signal.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        &format!("{SOURCE}late=plum.ui.retain_image(image);close_generation()"),
    )
    .unwrap();
    h.run("no-revival.lua","assert(not pcall(function()held:info()end));assert(not pcall(function()late:info()end));held=nil;late=nil;collectgarbage('collect')").unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    epoch.store(u64::MAX, Ordering::Release);
    assert_eq!(
        run(&h, &format!("{SOURCE}plum.ui.retain_image(image)"))
            .unwrap_err()
            .category,
        Category::Expired
    );
}

#[test]
fn published_retained_image_example_executes_verbatim_and_survives_into_next_job() {
    let (h, budget) = host(vec![Capability::UiOverlay]);
    let docs = include_str!("../../../../docs/LUA_API.md");
    let section = docs
        .split("## Project-scoped retained UI images (API 0.2)")
        .nth(1)
        .unwrap();
    let example = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    run(&h, example).unwrap();
    h.run("next.lua", "assert(retained_guide:info().width==2);assert(retained_guide:info().lifetime=='project');retained_guide:close();retained_guide=nil;collectgarbage('collect')").unwrap();
    assert_eq!(budget.used(), 0);
}
