use super::*;
use crate::lua::{Limits, World, bridge::ProjectStamp, handles::ProjectInfo};
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument},
};
use std::{cell::Cell, rc::Rc};
const LINE: &str = "local view=plum.ui.view_snapshot('player');plum.ui.publish_overlay(view,{id='guide',primitives={{kind='line',x1=0.25,y1=0.5,x2=0.75,y2=0.5,color={31,117,233,127},width=3}}})";
fn fixture(caps: Vec<Capability>) -> (Host, Mailbox, NativeBudget, Arc<AtomicU64>) {
    let doc = Arc::new(PlumDocument::new_project(
        "Overlays".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let world = World::new(Arc::clone(&doc), history.incarnations().snapshot());
    let stamp = ProjectStamp {
        serial: world.project_serial(),
        revision: 0,
    };
    let rect = egui::Rect::from_min_size(egui::pos2(50., 60.), egui::vec2(800., 400.));
    let view = visual::Snapshot {
        stamp,
        revision: 9,
        facts: visual::Facts::Player(visual::PlayerFrame {
            rect,
            rain: egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), 400.)),
            keyboard: egui::Rect::from_min_max(egui::pos2(rect.left(), 400.), rect.max),
            keys: 128,
            mapping: crate::player::rain::Mapping::Time,
            seconds_ahead: 4.,
            pixels_per_point: 1.5,
            stamp,
        }),
    };
    let h = Host::new(
        Limits {
            time: Duration::from_secs(10),
            ..Default::default()
        },
        caps,
    )
    .unwrap();
    h.set_world(Some(world.with_info(ProjectInfo {
        views: [None, Some(view)],
        ..Default::default()
    })));
    let budget = NativeBudget::isolated(64 * 1024 * 1024);
    h.lua.set_app_data(budget.clone());
    let epoch = Arc::new(AtomicU64::new(0));
    images::initialize_release_epoch(&h.lua, Arc::clone(&epoch));
    let (slot, rx) = channel(Arc::clone(&epoch));
    h.lua.set_app_data(slot);
    (h, rx, budget, epoch)
}
fn host() -> (Host, Mailbox, NativeBudget, Arc<AtomicU64>) {
    fixture(vec![Capability::UiOverlay, Capability::ReadPlayback])
}
fn advance(h: &Host) {
    h.lua.app_data_mut::<Slot>().unwrap().last = Some(Instant::now() - MIN_REFRESH);
}
#[test]
fn successful_frames_are_native_immutable_root_free_and_saved_views_still_expire() {
    let (h, rx, budget, _) = host();
    let staged = Rc::new(Cell::new(false));
    let signal = Rc::clone(&staged);
    h.lua
        .globals()
        .set(
            "inspect_stage",
            h.lua
                .create_function(move |lua, ()| {
                    let slot = lua.app_data_ref::<Slot>().unwrap();
                    assert_eq!(slot.pending.len(), 1);
                    signal.set(true);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.run(
        "success.lua",
        &format!("{LINE};held=view;inspect_stage();return 'staged'"),
    )
    .unwrap();
    assert!(staged.get());
    let batch = rx.try_recv().unwrap();
    assert_eq!(batch.frames.len(), 1);
    let f = &batch.frames[0];
    assert!(f.active());
    assert_eq!(f.id, "guide");
    assert_eq!(f.view.revision, 9);
    match &f.primitives.as_ref().unwrap().items[0] {
        schema::Primitive::Line {
            points,
            color,
            width,
            ..
        } => {
            assert_eq!(points, &vec![[0.25, 0.5], [0.75, 0.5]]);
            assert_eq!(*color, [31, 117, 233, 127]);
            assert_eq!(*width, 3.);
        }
        _ => panic!("line"),
    }
    h.run(
        "expired.lua",
        "assert(not pcall(function()held:info()end));held=nil;collectgarbage('collect')",
    )
    .unwrap();
    let doc = Arc::clone(&h.world().unwrap().doc);
    h.set_world(None);
    h.lua.gc_collect().unwrap();
    assert_eq!(Arc::strong_count(&doc), 1);
    drop(batch);
    assert_eq!(budget.used(), 0);
}
#[test]
fn failed_and_canceled_jobs_discard_all_staged_frames_and_leave_worker_reusable() {
    let (h, rx, budget, _) = host();
    assert!(
        h.run("fail.lua", &format!("{LINE};error('after stage')"))
            .is_err()
    );
    assert!(rx.try_recv().is_err());
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
        h.run("cancel.lua", &format!("{LINE};cancel_now()"))
            .unwrap_err()
            .category,
        Category::Canceled
    );
    assert!(rx.try_recv().is_err());
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    h.run("recover.lua", LINE).unwrap();
    assert_eq!(rx.try_recv().unwrap().frames.len(), 1);
}
#[test]
fn ids_attempts_geometry_refresh_and_mailbox_are_bounded_without_partial_batches() {
    let (h, rx, budget, _) = host();
    h.run("four.lua","local v=plum.ui.view_snapshot('player');for i=1,4 do plum.ui.publish_overlay(v,{id='layer'..i,primitives={}})end;assert(not pcall(function()plum.ui.publish_overlay(v,{id='fifth',primitives={}})end))").unwrap();
    let first = rx.try_recv().unwrap();
    assert_eq!(first.frames.len(), 4);
    assert_eq!(
        h.run("rate.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    advance(&h);
    assert_eq!(
        h.run("ids.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    h.run(
        "clear.lua",
        "plum.ui.clear_overlay(plum.ui.view_snapshot('player'),'layer1')",
    )
    .unwrap();
    assert!(rx.try_recv().unwrap().frames[0].primitives.is_none());
    advance(&h);
    h.run("new.lua", LINE).unwrap(); // deliberately leave mailbox full
    advance(&h);
    assert_eq!(
        h.run("full.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    assert_eq!(rx.try_recv().unwrap().frames[0].id, "guide");
    assert!(rx.try_recv().is_err());
    advance(&h);
    h.run("coalesce.lua","local v=plum.ui.view_snapshot('player');plum.ui.publish_overlay(v,{id='guide',primitives={}});plum.ui.clear_overlay(v,'guide')").unwrap();
    let b = rx.try_recv().unwrap();
    assert_eq!(b.frames.len(), 1);
    assert!(b.frames[0].primitives.is_none());
    drop(b);
    drop(first);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn strict_shared_parser_and_attempt_preflight_reject_invalid_unbounded_and_fake_input() {
    let (h, rx, budget, _) = host();
    h.run("invalid.lua","local v=plum.ui.view_snapshot('player');assert(not pcall(function()plum.ui.publish_overlay(v,{id='bad',unknown=true,primitives={}})end));assert(not pcall(function()plum.ui.publish_overlay(v,{id='bad',primitives={{kind='line',x1=0/0,y1=0,x2=1,y2=1}}})end));assert(not pcall(function()plum.ui.publish_overlay(v,{id='bad',primitives=setmetatable({},{})})end));local many={};for i=1,2049 do many[i]={kind='circle',x=0,y=0,radius=1}end;assert(not pcall(function()plum.ui.publish_overlay(v,{id='bad',primitives=many})end));assert(not pcall(function()plum.ui.publish_overlay(v,{id='valid',primitives={}})end))").unwrap();
    assert!(rx.try_recv().is_err());
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    assert!(
        h.run(
            "fake.lua",
            "plum.ui.publish_overlay({}, {id='fake',primitives={}})"
        )
        .is_err()
    );
    for source in [
        "local v=plum.ui.view_snapshot('player');plum.ui.publish_overlay(v,{id='../bad',primitives={}})",
        "local v=plum.ui.view_snapshot('player');plum.ui.publish_overlay(v,{id=123,primitives={}})",
        "local v=plum.ui.view_snapshot('player');plum.ui.publish_overlay(v,{id='bad',primitives={[2]={kind='text',x=0,y=0,text='hole'}}})",
    ] {
        assert!(h.run("bad.lua", source).is_err());
    }
}
#[test]
fn only_explicitly_retained_pixels_can_outlive_the_job_in_an_overlay() {
    let (h, rx, budget, _) = host();
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    let source = "local src=plum.images.from_rgba(1,1,string.char(255,0,0,128));local image=plum.ui.image(src);local v=plum.ui.view_snapshot('player');";
    let err=h.run("job-image.lua",&format!("{source}plum.ui.publish_overlay(v,{{id='guide',primitives={{{{kind='image',image=image,x1=0,y1=0,x2=1,y2=1}}}}}})")).unwrap_err();
    assert_eq!(err.category, Category::Runtime);
    assert!(err.message.contains("promoted"));
    assert!(rx.try_recv().is_err());
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    h.run("retained.lua",&format!("{source}held=plum.ui.retain_image(image);plum.ui.publish_overlay(v,{{id='guide',primitives={{{{kind='image',image=held,x1=0,y1=0,x2=1,y2=1}}}}}})")).unwrap();
    let b = rx.try_recv().unwrap();
    let schema::Primitive::Image { resource, .. } =
        &b.frames[0].primitives.as_ref().unwrap().items[0]
    else {
        panic!("image")
    };
    assert!(resource.alive());
    h.run(
        "close.lua",
        "held:close();held=nil;collectgarbage('collect')",
    )
    .unwrap();
    assert!(!resource.alive());
    drop(b);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn native_generation_and_host_drop_invalidate_accepted_frames_and_old_queued_work() {
    let (h, rx, _, epoch) = host();
    h.run("publish.lua", LINE).unwrap();
    let b = rx.try_recv().unwrap();
    assert!(b.frames[0].active());
    epoch.store(1, Ordering::Release);
    assert!(!b.frames[0].active());
    advance(&h);
    images::set_run_epoch(&h.lua, 0);
    assert_eq!(
        h.run("old-job.lua", LINE).unwrap_err().category,
        Category::Expired
    );
    assert!(rx.try_recv().is_err());
    advance(&h);
    h.run("new-generation.lua", LINE).unwrap();
    let next = rx.try_recv().unwrap();
    assert!(next.frames[0].active());
    drop(h);
    assert!(!next.frames[0].active());
}
#[test]
fn native_budget_backpressure_and_surface_authority_refuse_without_retaining_new_pixels() {
    let (h, rx, budget, _) = host();
    let held = budget.reserve(64 * 1024 * 1024 - FRAME_BYTES).unwrap();
    assert_eq!(
        h.run("budget.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    assert!(rx.try_recv().is_err());
    drop(held);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    h.run("queued.lua", LINE).unwrap();
    advance(&h);
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    assert!(h.run("rollback.lua","local src=plum.images.from_rgba(1,1,string.char(0,0,0,255));failed=plum.ui.retain_image(plum.ui.image(src));local v=plum.ui.view_snapshot('player');plum.ui.publish_overlay(v,{id='second',primitives={{kind='image',image=failed,x1=0,y1=0,x2=1,y2=1}}})").is_err());
    h.run(
        "closed.lua",
        "assert(not pcall(function()failed:info()end));failed=nil;collectgarbage('collect')",
    )
    .unwrap();
    drop(rx.try_recv().unwrap());
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    for caps in [vec![Capability::UiOverlay], vec![Capability::ReadPlayback]] {
        let (h, rx, _, _) = fixture(caps);
        assert_eq!(
            h.run("denied.lua", LINE).unwrap_err().category,
            Category::Capability
        );
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn published_overlay_example_executes_verbatim_with_native_accounting() {
    let (h, rx, budget, _) = host();
    let docs = include_str!("../../../../docs/LUA_API.md");
    let example = docs
        .split("## Staged editor overlay frames (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-overlay.lua", example).unwrap();
    let b = rx.try_recv().unwrap();
    assert_eq!(b.frames[0].primitives.as_ref().unwrap().items.len(), 2);
    drop(b);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn retained_overlay_renderer_limit_is_an_actionable_error_in_a_later_worker_job() {
    let (h, rx, budget, _) = host();
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    h.run("small-renderer.lua","local src=plum.images.from_rgba(2,1,string.char(255,0,0,255,0,255,0,255));held=plum.ui.retain_image(plum.ui.image(src));plum.ui.publish_overlay(plum.ui.view_snapshot('player'),{id='guide',primitives={{kind='image',image=held,x1=0,y1=0,x2=1,y2=1}}})").unwrap();
    let b = rx.try_recv().unwrap();
    let schema::Primitive::Image { resource, .. } =
        &b.frames[0].primitives.as_ref().unwrap().items[0]
    else {
        panic!("image")
    };
    let ctx = egui::Context::default();
    ctx.input_mut(|i| i.max_texture_side = 1);
    let mut remaining = images::MAX_UPLOAD_PIXELS;
    assert!(resource.prepare(&ctx, &mut remaining).is_none());
    let err = h
        .run("inspect-fault.lua", "return held:info()")
        .unwrap_err();
    assert_eq!(err.category, Category::Budget);
    assert!(err.message.contains("smaller max_side"));
    h.run(
        "cleanup.lua",
        "held:close();held=nil;collectgarbage('collect')",
    )
    .unwrap();
    drop(b);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}

#[test]
fn metadata_only_jobs_cannot_reset_live_id_or_refresh_budgets() {
    let (h, rx, _, _) = host();
    h.run("four.lua","local v=plum.ui.view_snapshot('player');for i=1,4 do plum.ui.publish_overlay(v,{id='layer'..i,primitives={}})end").unwrap();
    drop(rx.try_recv().unwrap());
    let world = h.world().unwrap();
    h.set_world(None);
    h.run("metadata-only.lua", "return 1").unwrap();
    h.set_world(Some(world));
    assert_eq!(
        h.run("rate-still-bounded.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    advance(&h);
    assert_eq!(
        h.run("ids-still-bounded.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    assert_eq!(h.lua.app_data_ref::<Slot>().unwrap().ids.len(), 4);
    assert!(rx.try_recv().is_err());
}

#[test]
fn readonly_surface_delivery_uses_exact_native_views_and_can_publish_without_a_document_root() {
    use crate::lua::reactive::{Data, Delivery, Event, Fence, Kind, Scope};
    let (h, rx, budget, _) = host();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: "test.overlay.surface".into(),
        name: "Surface".into(),
        version: "0.2".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    h.run("register.lua","plum.plugin.observe{id='surface',events={'surface_changed'},run=function(e)assert(e.kind=='surface_changed' and e.context=='metadata' and e.read_only and not e.transaction_allowed);assert(e.data.surface=='player' and e.data.visible and e.data.view_revision==9);local ok,p=pcall(plum.project.current);assert(not ok or p==nil);local v=assert(plum.ui.view_snapshot('player'));assert(v:info().view_revision==9);assert(not pcall(function()plum.ui.view_snapshot('piano_roll')end));plum.ui.publish_overlay(v,{id='event-guide',primitives={{kind='line',x1=0,y1=0,x2=1,y2=1}}});held=v end};plum.plugin.__phase('ready')").unwrap();
    let world = h.world().unwrap();
    let view = world.info.views[1].unwrap();
    let doc = Arc::clone(&world.doc);
    h.set_world(None);
    drop(world);
    assert_eq!(Arc::strong_count(&doc), 1);
    let d = Delivery {
        views: [None, Some(view)],
        playback: None,
        id: "surface".into(),
        event: Event::new(
            Kind::SurfaceChanged,
            view.stamp,
            1,
            Data::Surface {
                index: 1,
                view_revision: Some(9),
            },
        )
        .unwrap(),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    };
    h.lua.app_data_mut::<Scope>().unwrap().delivery = Some(d.clone());
    h.run("surface.lua", "plum.plugin.__observe()").unwrap();
    let frame = rx.try_recv().unwrap();
    assert_eq!(frame.frames[0].view, view);
    assert_eq!(Arc::strong_count(&doc), 1);
    d.fence.invalidate();
    assert_eq!(
        h.run("expired-surface.lua", "plum.plugin.__observe()")
            .unwrap_err()
            .category,
        Category::Expired
    );
    h.lua.app_data_mut::<Scope>().unwrap().delivery = None;
    h.run(
        "expired-view.lua",
        "assert(not pcall(function()held:info()end));held=nil;collectgarbage('collect')",
    )
    .unwrap();
    drop(frame);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn surface_delivery_rejects_wrong_revision_foreign_snapshot_and_unauthorized_surface() {
    use crate::lua::reactive::{Data, Delivery, Event, Fence, Kind, Scope};
    let (h, _, _, _) = host();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: "test.overlay.invalid".into(),
        name: "Invalid".into(),
        version: "0.2".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    h.run("register.lua","plum.plugin.observe{id='surface',events={'surface_changed'},run=function()error('must not run')end};plum.plugin.__phase('ready')").unwrap();
    let view = h.world().unwrap().info.views[1].unwrap();
    h.set_world(None);
    let base = Delivery {
        views: [None, Some(view)],
        playback: None,
        id: "surface".into(),
        event: Event::new(
            Kind::SurfaceChanged,
            view.stamp,
            1,
            Data::Surface {
                index: 1,
                view_revision: Some(9),
            },
        )
        .unwrap(),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    };
    let mut bad = base.clone();
    bad.views[1].as_mut().unwrap().revision += 1;
    h.lua.app_data_mut::<Scope>().unwrap().delivery = Some(bad);
    assert_eq!(
        h.run("wrong-view.lua", "plum.plugin.__observe()")
            .unwrap_err()
            .category,
        Category::Expired
    );
    let mut bad = base.clone();
    bad.views[1].as_mut().unwrap().stamp.serial += 1;
    h.lua.app_data_mut::<Scope>().unwrap().delivery = Some(bad);
    assert_eq!(
        h.run("foreign.lua", "plum.plugin.__observe()")
            .unwrap_err()
            .category,
        Category::Expired
    );
    let mut bad = base;
    bad.views = [None, None];
    bad.event = Event::new(
        Kind::SurfaceChanged,
        view.stamp,
        2,
        Data::Surface {
            index: 0,
            view_revision: None,
        },
    )
    .unwrap();
    h.lua.app_data_mut::<Scope>().unwrap().delivery = Some(bad);
    assert_eq!(
        h.run("unauthorized.lua", "plum.plugin.__observe()")
            .unwrap_err()
            .category,
        Category::Capability
    );
}

#[test]
fn mailbox_pending_flag_tracks_native_consumption_failure_and_consumer_drop() {
    let (h, rx, _, _) = host();
    let flag = rx.pending_flag();
    assert!(!flag.load(Ordering::Acquire));
    h.run("first.lua", LINE).unwrap();
    assert!(flag.load(Ordering::Acquire));
    advance(&h);
    assert_eq!(
        h.run("full.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    assert!(flag.load(Ordering::Acquire));
    let held = rx.try_recv().unwrap();
    assert!(
        !flag.load(Ordering::Acquire),
        "native receipt acknowledges even while accepted frame data remains owned"
    );
    advance(&h);
    h.run("next.lua", LINE).unwrap();
    assert!(flag.load(Ordering::Acquire));
    drop(rx);
    assert!(!flag.load(Ordering::Acquire));
    advance(&h);
    assert_eq!(
        h.run("closed.lua", LINE).unwrap_err().category,
        Category::Budget
    );
    assert!(!flag.load(Ordering::Acquire));
    drop(held);
}

#[test]
fn published_surface_subscription_executes_verbatim_in_player_only_root_free_scope() {
    use crate::lua::reactive::{Data, Delivery, Event, Fence, Kind, Scope};
    let (h, rx, budget, _) = host();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: "test.overlay.published".into(),
        name: "Published".into(),
        version: "0.2".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    let docs = include_str!("../../../../docs/LUA_API.md");
    let source = docs
        .split("## Surface notifications and exact reactive view delivery (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run(
        "published-subscription.lua",
        &format!("{source}\nplum.plugin.__phase('ready')"),
    )
    .unwrap();
    let view = h.world().unwrap().info.views[1].unwrap();
    h.set_world(None);
    h.lua.app_data_mut::<Scope>().unwrap().delivery = Some(Delivery {
        views: [None, Some(view)],
        playback: None,
        id: "surface-guide".into(),
        event: Event::new(
            Kind::SurfaceChanged,
            view.stamp,
            1,
            Data::Surface {
                index: 1,
                view_revision: Some(view.revision),
            },
        )
        .unwrap(),
        fence: Fence::live(),
        lifecycle: Fence::live(),
        dropped: 0,
    });
    h.run("published-callback.lua", "plum.plugin.__observe()")
        .unwrap();
    let frame = rx.try_recv().unwrap();
    assert_eq!(frame.frames[0].id, "reactive-guide");
    drop(frame);
    h.lua.app_data_mut::<Scope>().unwrap().delivery = None;
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
