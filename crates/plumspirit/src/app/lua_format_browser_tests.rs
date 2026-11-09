use super::*;
use crate::lua::{Capability, Host, Limits, World, plugin_api::PluginMeta};
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};
use std::sync::Arc;

fn staged(keys: u16) -> (Arc<PlumDocument>, crate::lua::tx::Commits, History) {
    let mut d = PlumDocument::new_project("Before import".into(), Bpm::DEFAULT, 960);
    if keys == 256 {
        d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let doc = Arc::new(d);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    let host = Host::new(Limits::default(), Capability::ALL).unwrap();
    host.set_plugin(PluginMeta {
        id: "author.staged-review".into(),
        name: "Review".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    host.run("rich-staged.lua",&format!(r#"
        local p=plum.project.current();local tx=plum.transaction{{label='Rich staged import'}}
        local t=tx:add_track{{name='New Track'}};local sub=tx:add_subtracks(t,1)[1]
        local w=tx:note_writer(sub);w:append{{{{start=9007199254741001,gate=17,key={key},velocity=93}}}};w:finish()
        tx:insert_events(sub,{{{{kind='cc',tick=9007199254741001,controller=7,value=80,channel=3}},{{kind='pitch_bend',tick=9007199254741002,value=-8192,channel=3}}}})
        local clip=tx:add_clip(t,12000,960);local w=tx:note_writer(clip:content());w:append{{{{start=37,gate=13,key={key},velocity=91}}}};w:finish()
        tx:insert_events(clip:content(),{{{{kind='cc',tick=38,controller=10,value=64}}}})
        tx:duplicate_clips(clip,{{linked=true,start=24000,track=sub}})
        tx:set_tempo(0,144);tx:set_time_signature(0,7,8);tx:add_marker(19,'Staged chapter',100)
        local g=tx:create_group{{name='Staged group'}};tx:set_metadata(g,{{role='art'}},{{version=1}})
        tx:set_project_metadata({{exact=9007199254741001,number=3.25,text='音🎵'}},{{version=2}})
        tx:commit()
    "#,key=if keys==256{200}else{60})).unwrap();
    let commits = host.take_commits().unwrap();
    drop(host);
    (doc, commits, history)
}
fn frame(
    browser: &mut Browser,
    doc: &PlumDocument,
    ctx: &egui::Context,
    size: egui::Vec2,
    scale: f32,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.set_pixels_per_point(scale);
    let mut out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |_ui| browser.show(ctx, doc, 123, true),
    );
    out.textures_delta.clear();
    out
}
#[test]
fn new_track_and_owned_linked_clip_preview_uses_exact_native_keys_coordinates_and_never_edits_source()
 {
    for keys in [128, 256] {
        let (source, commits, _) = staged(keys);
        let before = source.state_fingerprint();
        let d = commits.result();
        let staged = d.state_fingerprint();
        let ctx = egui::Context::default();
        let mut b = Browser::default();
        b.select_track(d, 2);
        for (size, scale) in [
            (egui::vec2(1280., 900.), 1.),
            (egui::vec2(900., 700.), 1.5),
            (egui::vec2(1500., 900.), 2.),
        ] {
            frame(&mut b, d, &ctx, size, scale, Vec::new());
            frame(&mut b, d, &ctx, size, scale, Vec::new());
            assert_eq!(b.target(), plumspirit_engine::editor::NoteTarget::Track(2));
            assert_eq!(b.roll.view.time.origin_tick, 9007199254741001);
            assert_eq!(
                b.roll.view.time.tick_to_x(9007199254741003),
                2. * b.roll.view.time.pixels_per_tick
            );
            assert_eq!(b.roll.view.pitch.top, if keys == 256 { 255 } else { 127 });
            assert!(b.roll.notes_rect().is_some());
        }
        let size = egui::vec2(1500., 900.);
        let pos = b.roll.notes_rect().unwrap().center();
        frame(
            &mut b,
            d,
            &ctx,
            size,
            2.,
            vec![egui::Event::PointerMoved(pos)],
        );
        let zoom = b.roll.view.time.pixels_per_tick;
        frame(
            &mut b,
            d,
            &ctx,
            size,
            2.,
            vec![egui::Event::MouseWheel {
                phase: egui::TouchPhase::Move,
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0., 100.),
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            }],
        );
        assert!(
            b.roll.view.time.pixels_per_tick > zoom,
            "native Ctrl-wheel changes preview X zoom"
        );
        let origin = b.roll.view.time.origin_tick;
        frame(
            &mut b,
            d,
            &ctx,
            size,
            2.,
            vec![egui::Event::MouseWheel {
                phase: egui::TouchPhase::Move,
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0., -100.),
                modifiers: egui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
            }],
        );
        assert_ne!(
            b.roll.view.time.origin_tick, origin,
            "native Shift-wheel pans preview X"
        );
        let tick = 9007199254741001;
        assert_eq!(
            b.roll.view.time.x_to_tick(b.roll.view.time.tick_to_x(tick)),
            tick
        );
        let p = d
            .clips()
            .explicit_placements()
            .iter()
            .position(|p| p.start == 24000)
            .unwrap();
        b.select_clip(d, p);
        frame(&mut b, d, &ctx, egui::vec2(1280., 900.), 1., Vec::new());
        frame(&mut b, d, &ctx, egui::vec2(1280., 900.), 1., Vec::new());
        assert_eq!(b.shift, 24000);
        assert_eq!(b.roll.shift, 24000);
        assert!(b.content.is_some());
        assert_eq!(b.track, 2);
        assert_eq!(b.roll.view.time.origin_tick, 37);
        assert_eq!(
            b.roll.view.time.tick_to_x(39),
            2. * b.roll.view.time.pixels_per_tick
        );
        assert_eq!(
            plumspirit_engine::editor::note_view(d, b.target())
                .unwrap()
                .notes
                .len(),
            1
        );
        let pos = b.roll.notes_rect().unwrap().center();
        for pressed in [true, false] {
            frame(
                &mut b,
                d,
                &ctx,
                egui::vec2(1280., 900.),
                1.,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        assert_eq!(source.state_fingerprint(), before);
        assert_eq!(d.state_fingerprint(), staged);
    }
}
#[test]
fn events_groups_and_attribute_views_are_bounded_native_rows_with_exact_typed_excerpts() {
    let (_, commits, _) = staged(256);
    let d = commits.result();
    let ctx = egui::Context::default();
    let mut b = Browser::default();
    b.select_track(d, 2);
    for tab in [
        Tab::Events,
        Tab::Settings,
        Tab::Clips,
        Tab::Conductor,
        Tab::Groups,
        Tab::Attributes,
    ] {
        b.tab = tab;
        let out = frame(&mut b, d, &ctx, egui::vec2(1280., 900.), 1., Vec::new());
        assert!(out.shapes.len() < 1000);
        assert!(b.rows <= 80);
        assert!(
            b.rows > 0,
            "selected native data tab renders actual records"
        );
    }
    let mut excerpt = Excerpt::new(8192);
    value(
        &mut excerpt,
        &d.project()
            .metadata
            .get("author.staged-review")
            .unwrap()
            .value,
    )
    .unwrap();
    assert!(
        excerpt.text.contains("9007199254741001")
            && excerpt.text.contains("3.25")
            && excerpt.text.contains("音🎵")
    );
    let mut excerpt = Excerpt::new(128);
    assert!(value(&mut excerpt, &Value::Text("🎵".repeat(16384))).is_err());
    assert_eq!(excerpt.text.chars().count(), 128);
    let input = Value::Text("音🎵\n".repeat(4000));
    let mut collected = String::new();
    for page in 0..48 {
        let mut excerpt = Excerpt::page(8192, page * 8192);
        let complete = value(&mut excerpt, &input).is_ok();
        collected.push_str(&excerpt.text);
        if complete {
            break;
        }
    }
    assert_eq!(
        collected,
        format!("{:?}", "音🎵\n".repeat(4000)),
        "bounded value pages preserve the complete typed string"
    );
    assert_eq!(
        short("音\n🎵".repeat(1000).as_str(), 80).chars().count(),
        81
    );
}

#[test]
fn rich_private_result_can_be_browsed_then_applied_as_exactly_one_undo_or_discarded() {
    for cancel in [false, true] {
        let (mut source, mut commits, mut history) = staged(256);
        let original = source.as_ref().clone();
        let before = source.state_fingerprint();
        let after = commits.result().state_fingerprint();
        let ctx = egui::Context::default();
        let mut b = Browser::default();
        b.select_clip(commits.result(), 0);
        frame(
            &mut b,
            commits.result(),
            &ctx,
            egui::vec2(1280., 900.),
            1.,
            Vec::new(),
        );
        assert_eq!(source.state_fingerprint(), before);
        assert_eq!(
            Arc::strong_count(&source),
            2,
            "browser retains no source root"
        );
        b.open = false;
        if cancel {
            drop(commits);
            assert_eq!(source.state_fingerprint(), before);
            assert!(!history.can_undo());
            assert_eq!(Arc::strong_count(&source), 1);
        } else {
            commits.apply(&mut history, &mut source).unwrap();
            drop(commits);
            assert_eq!(source.state_fingerprint(), after);
            assert_eq!(history.undo_len(), 1);
            history.undo(Arc::get_mut(&mut source).unwrap()).unwrap();
            // Native Track IDs are never reissued after Undo. Compare restored
            // values separately from that deliberately monotonic allocator.
            assert_eq!(source.fingerprint(), original.fingerprint());
            assert_eq!(source.project(), original.project());
            assert_eq!(source.track_count(), original.track_count());
            assert_eq!(
                source.track(0).unwrap().id(),
                original.track(0).unwrap().id()
            );
            assert_eq!(
                source.track(0).unwrap().props(),
                original.track(0).unwrap().props()
            );
            assert!(source.clips().is_empty());
            assert_eq!(
                source.conductor().tempo_points(),
                original.conductor().tempo_points()
            );
            assert_eq!(
                source.conductor().time_signatures(),
                original.conductor().time_signatures()
            );
            assert_eq!(source.conductor().markers(), original.conductor().markers());
            assert_eq!(
                source.conductor().master_volume(),
                original.conductor().master_volume()
            );
            assert!(source.next_track_id() > original.next_track_id());
            history.redo(Arc::get_mut(&mut source).unwrap()).unwrap();
            assert_eq!(source.state_fingerprint(), after);
        }
    }
}
#[test]
fn a_large_native_event_store_only_builds_visible_rows_and_content_preview_is_not_a_note_vector() {
    use plumspirit_engine::model::events::RawEvent;
    let mut d = PlumDocument::new_project("Dense review".into(), Bpm::DEFAULT, 960);
    let t = d.track_mut(0).unwrap();
    for i in 0..100_000 {
        t.events
            .push(RawEvent {
                tick: i,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xb0,
                    data: [7, 80],
                },
            })
            .unwrap();
    }
    let recs = (0..100_000)
        .map(|i| {
            NoteRecord::new(
                i / 50_000,
                17,
                NoteAttrs::new(60, 80, 0, 0, NoteOffKind::NoteOff).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    t.notes.insert_sorted_batch(&recs, false).unwrap();
    let mut b = Browser::default();
    b.select_track(&d, 0);
    b.tab = Tab::Events;
    let ctx = egui::Context::default();
    let out = frame(&mut b, &d, &ctx, egui::vec2(1280., 900.), 1., Vec::new());
    assert!(b.rows > 0 && b.rows < 20);
    assert!(out.shapes.len() < 500);
    b.tab = Tab::Notes;
    frame(&mut b, &d, &ctx, egui::vec2(1280., 900.), 1., Vec::new());
    assert_eq!(d.note_count(), 100_000);
    assert_eq!(d.track(0).unwrap().notes.selected_count(), 0);
}
