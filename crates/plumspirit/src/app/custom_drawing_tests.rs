mod drawing {
    use super::*;
    use plumspirit_engine::model::project::KeyRange;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "plum-api02-draw-app-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let p = self.0.canonicalize().unwrap();
            let base = std::env::temp_dir().canonicalize().unwrap();
            assert!(
                p.starts_with(&base)
                    && p != base
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("plum-api02-draw-app-")
            );
            std::fs::remove_dir_all(p).unwrap();
        }
    }
    fn setup(extended: bool) -> (Harness, Temp) {
        setup_source(extended, false)
    }
    fn setup_source(extended: bool, official: bool) -> (Harness, Temp) {
        setup_source_linked(extended, official, false)
    }
    fn setup_source_linked(extended: bool, official: bool, linked: bool) -> (Harness, Temp) {
        let temp = Temp::new();
        let src = temp.0.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("plum-plugin.toml"),
            r#"id="test.draw"
name="Drawing fixture"
version="0.2.0"
api="0.2"
capabilities=["project.read","notes.edit","tools.draw"]
"#,
        )
        .unwrap();
        let fixture = r#"
          function register(p)
            p.tool{id='stroke',name='Authored stroke',gesture='freehand',needs_selection=false,
              run=function(ctx)
                local info=ctx.stroke:info();assert(info.keys==ctx.keys)
                local notes={};for first=1,info.count,1024 do
                  for _,point in ipairs(ctx.stroke:batch(first,math.min(1024,info.count-first+1)))do
                    notes[#notes+1]={start=point.tick,gate=60,key=math.floor(point.pitch),velocity=91}
                  end
                end
                ctx.tx:insert_notes(ctx.track,notes)
              end}
          end
        "#;
        std::fs::write(
            src.join("main.lua"),
            if official {
                include_str!("../../examples-lua/custom-drawing-studio/main.lua")
            } else {
                fixture
            },
        )
        .unwrap();
        let mut d = doc();
        if extended {
            d.project_mut().keys = KeyRange::Extended256;
        }
        if linked {
            use plumspirit_engine::history::{History, HistoryLimits, Operation};
            let mut history = History::new(HistoryLimits::default());
            if extended {
                history
                    .apply(
                        &mut d,
                        Operation::AddSubtracks {
                            parent: 0,
                            count: 1,
                        },
                    )
                    .unwrap();
            } else {
                d.add_track().unwrap();
            }
            let id = d.track(1).unwrap().id();
            let mut props = d.track(1).unwrap().props().clone();
            props.default_channel = 9;
            history
                .apply(
                    &mut d,
                    Operation::TrackProps {
                        track: id,
                        props,
                        end: None,
                        label: "Fixture channel",
                    },
                )
                .unwrap();
        }
        let mut h = Harness::new(d);
        h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
        h.app.settings.lua_plugins.enable_on_install = true;
        h.app_frame(egui::vec2(1366., 768.), 1.);
        let mut prefs = h.app.settings.lua_plugins.clone();
        h.app
            .lua_plugins()
            .install(&src, false, &mut prefs)
            .unwrap();
        prefs.installed.get_mut("test.draw").unwrap().granted = vec![
            "project.read".into(),
            "notes.edit".into(),
            "tools.draw".into(),
        ];
        h.app.settings.lua_plugins = prefs;
        frames_until(&mut h, "drawing tool registration", |h| {
            !h.app.custom_tool_menu().is_empty()
        });
        h.app
            .open_custom_tool("test.draw", if official { "freehand" } else { "stroke" });
        assert_eq!(
            h.app.custom_tool.as_ref().unwrap().capture_mode(),
            Some(crate::piano_roll::CaptureMode::FreehandOrdered)
        );
        for _ in 0..3 {
            frame(&mut h, vec![]);
        }
        (h, temp)
    }
    fn frame(h: &mut Harness, events: Vec<egui::Event>) {
        h.app_frame_events(egui::vec2(1366., 768.), 1., events);
    }
    fn button(p: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn capture(h: &mut Harness) -> Arc<crate::lua::drawing::Captured> {
        let area = h.app.roll.notes_rect().unwrap();
        let a = egui::pos2(
            area.left() + area.width() * 0.72,
            area.top() + area.height() * 0.48,
        );
        let b = a + egui::vec2(80., -27.);
        let view = h.app.roll.view;
        let expected_tick = view.time.x_to_tick(f64::from(a.x - area.left()));
        let expected_pitch = (view.pitch.rows()
            - f64::from(a.y - area.top()) / view.pitch.row_height
            - view.pitch.scroll_rows)
            .clamp(0., view.pitch.rows() - 0.001);
        frame(h, vec![egui::Event::PointerMoved(a)]);
        frame(h, vec![button(a, true)]);
        assert!(h.app.custom_tool.as_ref().unwrap().stroking);
        assert!(h.app.custom_tool.as_ref().unwrap().stroke.is_none());
        frame(h, vec![egui::Event::PointerMoved(b)]);
        frame(h, vec![button(b, false)]);
        let input = h.app.custom_tool.as_ref().unwrap().stroke.clone().unwrap();
        assert_eq!(input.stroke.points[0].tick, expected_tick);
        assert!((input.stroke.points[0].pitch - expected_pitch).abs() < 1e-9);
        assert!(input.stroke.points.len() >= 2);
        assert!(input.stroke.points.windows(2).all(|w| w[0].t <= w[1].t));
        input
    }
    #[test]
    fn actual_roll_capture_zoom_scroll_preview_apply_undo_redo_in_both_key_modes() {
        for extended in [false, true] {
            let (mut h, _temp) = setup(extended);
            let before = h.doc().export_fingerprint();
            h.app.roll.view.time.origin_tick = 12_345;
            h.app.roll.view.time.pixels_per_tick = 0.25;
            h.app.roll.view.pitch.row_height = 6.;
            h.app.roll.view.pitch.scroll_rows = 5.5;
            frame(&mut h, vec![]);
            let input = capture(&mut h);
            let count = input.stroke.points.len() as u64;
            assert_eq!(input.keys, if extended { 256 } else { 128 });
            frames_until(&mut h, "drawing worker Preview", |h| {
                h.app
                    .custom_tool
                    .as_ref()
                    .is_some_and(|s| s.result.is_some())
            });
            assert_eq!(
                h.doc().export_fingerprint(),
                before,
                "preview has no persistent effects"
            );
            assert_eq!(
                h.app
                    .custom_tool
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .notes
                    .len(),
                8 + count as usize
            );
            h.app.custom_tool_action(UiAction::Apply, None, true);
            assert_eq!(h.doc().note_count(), 8 + count as usize);
            assert!(
                h.app.custom_tool.as_ref().unwrap().stroke.is_none(),
                "completed gesture clears; tool stays open"
            );
            assert_eq!(
                h.app.open.as_ref().unwrap().history.undo_label(),
                Some("Authored stroke")
            );
            let applied = h.doc().export_fingerprint();
            assert!(h.command(Command::Undo));
            assert_eq!(h.doc().export_fingerprint(), before);
            assert!(h.command(Command::Redo));
            assert_eq!(h.doc().export_fingerprint(), applied);
            assert!(h.command(Command::Undo));
            let _next = capture(&mut h);
            h.app.cancel_custom_stroke();
            for _ in 0..4 {
                frame(&mut h, vec![]);
            }
            assert_eq!(
                h.doc().export_fingerprint(),
                before,
                "canceled worker result never publishes"
            );
            h.app.custom_tool_action(UiAction::Close, None, true);
        }
    }
    #[test]
    fn escape_and_permission_revocation_drop_active_stroke_without_musical_changes() {
        let (mut h, _temp) = setup(false);
        let before = h.doc().export_fingerprint();
        let area = h.app.roll.notes_rect().unwrap();
        let a = egui::pos2(area.left() + area.width() * 0.8, area.center().y);
        frame(&mut h, vec![egui::Event::PointerMoved(a)]);
        frame(&mut h, vec![button(a, true)]);
        frame(
            &mut h,
            vec![egui::Event::PointerMoved(a + egui::vec2(30., 0.))],
        );
        frame(
            &mut h,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(!h.app.custom_tool.as_ref().unwrap().stroking);
        assert!(
            h.app
                .custom_tool
                .as_ref()
                .unwrap()
                .stroke_overlay
                .is_empty()
        );
        frame(&mut h, vec![button(a, false)]);
        assert_eq!(h.doc().export_fingerprint(), before);
        frame(&mut h, vec![button(a, true)]);
        h.app
            .settings
            .lua_plugins
            .installed
            .get_mut("test.draw")
            .unwrap()
            .granted
            .retain(|c| c != "tools.draw");
        frame(&mut h, vec![]);
        assert!(h.app.custom_tool.is_none());
        frame(&mut h, vec![button(a, false)]);
        assert_eq!(h.doc().export_fingerprint(), before);
        h.app.open_custom_tool("test.draw", "stroke");
        assert!(h.app.custom_tool.is_none());
    }
    #[test]
    fn owned_clip_stroke_keeps_song_origin_and_edits_only_its_content() {
        use crate::app::track_list::ClipIntent;
        use plumspirit_engine::model::ContentRef;
        let (mut h, _temp) = setup(true);
        h.clip(ClipIntent::Add {
            track: 0,
            start: 1920,
            length: 3840,
        });
        let content = h.doc().clips().contents()[0].id;
        let placement = h.doc().clips().placements_of(ContentRef::Owned(content))[0].id;
        h.clip(ClipIntent::Open(placement));
        h.app.open_custom_tool("test.draw", "stroke");
        for _ in 0..3 {
            frame(&mut h, vec![]);
        }
        let before = h.doc().export_fingerprint();
        let own = h
            .doc()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.record())
            .collect::<Vec<_>>();
        let input = capture(&mut h);
        assert_eq!(input.content, Some(content.0));
        assert_eq!(input.shift, 1920);
        frames_until(&mut h, "clip stroke Preview", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some())
        });
        assert_eq!(h.doc().export_fingerprint(), before);
        h.app.custom_tool_action(UiAction::Apply, None, true);
        let notes = &h.doc().clips().content(content).unwrap().notes;
        assert_eq!(notes.len(), input.stroke.points.len());
        for (note, point) in notes.iter().zip(&input.stroke.points) {
            assert_eq!(note.start_tick, point.tick);
            assert_eq!(note.attrs.pitch(), point.pitch.floor() as u8);
        }
        assert_eq!(
            h.doc()
                .track(0)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.record())
                .collect::<Vec<_>>(),
            own
        );
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().export_fingerprint(), before);
        let _input = capture(&mut h);
        h.set_conductor();
        frame(&mut h, vec![]);
        assert!(
            h.app.custom_tool.is_none(),
            "locked editor context closes drawing surface"
        );
        assert_eq!(h.doc().export_fingerprint(), before);
    }
    #[test]
    fn linked_clip_on_second_track_or_subtrack_keeps_its_default_channel_through_native_transactions()
     {
        use crate::app::track_list::ClipIntent;
        use plumspirit_engine::model::ContentRef;
        for extended in [false, true] {
            let (mut h, _temp) = setup_source_linked(extended, false, true);
            h.clip(ClipIntent::Add {
                track: 0,
                start: 100,
                length: 3840,
            });
            let content = h.doc().clips().contents()[0].id;
            let first = h.doc().clips().placements_of(ContentRef::Owned(content))[0].id;
            h.clip(ClipIntent::Duplicate {
                copies: vec![(first, 1, 1920)],
                linked: true,
            });
            let second = h
                .doc()
                .clips()
                .placements_of(ContentRef::Owned(content))
                .iter()
                .find(|p| p.track == h.id(1))
                .unwrap()
                .id;
            h.clip(ClipIntent::Open(second));
            h.app.open_custom_tool("test.draw", "stroke");
            for _ in 0..3 {
                frame(&mut h, vec![]);
            }
            let before = h.doc().export_fingerprint();
            let input = capture(&mut h);
            assert_eq!(input.track, h.id(1).0);
            assert_eq!(input.content, Some(content.0));
            assert_eq!(input.shift, 1920);
            frames_until(&mut h, "linked owner Preview", |h| {
                h.app
                    .custom_tool
                    .as_ref()
                    .is_some_and(|s| s.result.is_some() || s.error.is_some())
            });
            assert!(
                h.app.custom_tool.as_ref().unwrap().error.is_none(),
                "{:?}",
                h.app.custom_tool.as_ref().unwrap().error
            );
            assert_eq!(h.doc().export_fingerprint(), before);
            h.app.custom_tool_action(UiAction::Apply, None, true);
            let notes = &h.doc().clips().content(content).unwrap().notes;
            assert_eq!(notes.len(), input.stroke.points.len());
            assert!(
                notes.iter().all(|n| n.attrs.channel() == 9),
                "default channel belongs to the opened second placement"
            );
            assert_eq!(h.doc().track(0).unwrap().notes.len(), 8);
            assert_eq!(h.doc().track(1).unwrap().notes.len(), 0);
            let applied = h.doc().export_fingerprint();
            assert!(h.command(Command::Undo));
            assert_eq!(h.doc().export_fingerprint(), before);
            assert!(h.command(Command::Redo));
            assert_eq!(h.doc().export_fingerprint(), applied);
            assert!(h.command(Command::Undo));
            assert_eq!(h.doc().export_fingerprint(), before);
        }
    }
    fn studio_preview(
        h: &mut Harness,
        points: &[(u64, f64, f64)],
    ) -> Vec<plumspirit_engine::transform::TNote> {
        let ctx = h.ctx.clone();
        h.app.custom_stroke(points, false, &ctx);
        h.app.custom_stroke(points, true, &ctx);
        frames_until(h, "Drawing Studio Preview", |h| {
            h.app
                .custom_tool
                .as_ref()
                .is_some_and(|s| s.result.is_some() || s.error.is_some())
        });
        let s = h.app.custom_tool.as_ref().unwrap();
        assert!(s.error.is_none(), "{:?}", s.error);
        s.result.as_ref().unwrap().notes.clone()
    }
    #[test]
    fn five_actual_drawing_studio_algorithms_have_deterministic_preview_apply_and_presets() {
        for extended in [false, true] {
            let (mut h, _temp) = setup_source(extended, true);
            assert_eq!(
                h.app
                    .lua_plugins
                    .as_ref()
                    .unwrap()
                    .plugin("test.draw")
                    .unwrap()
                    .decls
                    .tools
                    .len(),
                5
            );
            let before = h.doc().export_fingerprint();
            let mut fingerprints = std::collections::BTreeSet::new();
            let points = [(240, 60.5, 0.), (480, 60.5, 0.15), (960, 60.5, 0.4)];
            for id in ["freehand", "rectangle", "spray", "eraser", "wave"] {
                h.app.open_custom_tool("test.draw", id);
                let expected = studio_preview(&mut h, &points);
                assert_eq!(
                    h.doc().export_fingerprint(),
                    before,
                    "{id} Preview is temporary"
                );
                h.app.cancel_custom_stroke();
                let replay = studio_preview(&mut h, &points);
                assert_eq!(expected, replay, "{id} captured Preview replay");
                let s = h.app.custom_tool.as_mut().unwrap();
                let preset = values_to_json(&s.decl, &s.values);
                let (values, errors) = values_from_json(&s.decl, &preset);
                assert!(errors.is_empty());
                assert_eq!(values, s.values);
                let label = s.decl.name.clone();
                h.app.custom_tool_action(UiAction::Apply, None, true);
                let after = h.doc().export_fingerprint();
                assert_ne!(after, before, "{id} applies an operation");
                fingerprints.insert(after);
                assert_eq!(
                    h.app.open.as_ref().unwrap().history.undo_label(),
                    Some(label.as_str())
                );
                if id == "eraser" {
                    assert!(h.doc().note_count() < 8);
                } else {
                    assert!(h.doc().note_count() > 8);
                }
                assert!(h.command(Command::Undo));
                assert_eq!(h.doc().export_fingerprint(), before);
                assert!(h.command(Command::Redo));
                assert_eq!(h.doc().export_fingerprint(), after);
                assert!(h.command(Command::Undo));
                assert_eq!(h.doc().export_fingerprint(), before);
            }
            assert_eq!(
                fingerprints.len(),
                5,
                "author algorithms produce distinct musical results"
            );
            h.app.open_custom_tool("test.draw", "spray");
            let first = studio_preview(&mut h, &points);
            h.app.cancel_custom_stroke();
            let s = h.app.custom_tool.as_mut().unwrap();
            let i = s.decl.params.iter().position(|p| p.key == "seed").unwrap();
            s.values[i] = ToolValue::Int(77);
            let changed = studio_preview(&mut h, &points);
            assert_ne!(first, changed, "author seed changes spray");
            h.app.custom_tool_action(UiAction::Close, None, true);
            assert_eq!(h.doc().export_fingerprint(), before);
        }
    }
    #[test]
    fn long_studio_parameter_form_keeps_apply_and_cancel_visible_at_native_scales() {
        let (mut h, _temp) = setup_source(false, true);
        let points = [(240, 60.5, 0.), (960, 64.5, 0.4)];
        studio_preview(&mut h, &points);
        for scale in [1., 1.5] {
            let size = egui::vec2(1366., 768.);
            for _ in 0..3 {
                h.app_frame(size, scale);
            }
            let shapes = h.app_frame(size, scale);
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, size / scale);
            for label in ["Apply", "Cancel", "Presets", "Shortcut"] {
                let visible = shapes.iter().any(|c| {
                    crate::ui::testkit::texts(std::slice::from_ref(c))
                        .into_iter()
                        .any(|(t, p)| t == label && c.clip_rect.contains(p) && bounds.contains(p))
                });
                assert!(visible, "{label} must remain visible at scale{scale}");
            }
        }
    }
    #[test]
    fn ordered_same_frame_down_move_up_keeps_start_interior_and_end_samples() {
        let (mut h, _temp) = setup(false);
        let before = h.doc().export_fingerprint();
        let area = h.app.roll.notes_rect().unwrap();
        let a = egui::pos2(area.left() + area.width() * 0.65, area.center().y);
        let b = a + egui::vec2(80., -40.);
        let c = a + egui::vec2(140., 10.);
        let view = h.app.roll.view;
        frame(
            &mut h,
            vec![
                egui::Event::PointerMoved(a),
                button(a, true),
                egui::Event::PointerMoved(b),
                egui::Event::PointerMoved(c),
                button(c, false),
            ],
        );
        let s = h.app.custom_tool.as_ref().unwrap();
        assert!(!s.stroking);
        let stroke = s.stroke.as_ref().expect("one-frame stroke must complete");
        assert_eq!(stroke.stroke.points.len(), 3);
        for (sample, p) in stroke.stroke.points.iter().zip([a, b, c]) {
            assert_eq!(
                sample.tick,
                view.time.x_to_tick(f64::from(p.x - area.left()))
            );
            let pitch = view.pitch.rows()
                - f64::from(p.y - area.top()) / view.pitch.row_height
                - view.pitch.scroll_rows;
            assert!((sample.pitch - pitch).abs() < 1e-9);
        }
        assert_eq!(h.doc().export_fingerprint(), before);
        // An excessive native input batch cancels rather than silently dropping a
        // release boundary and leaving a gesture captured indefinitely.
        frame(&mut h, vec![egui::Event::PointerMoved(a), button(a, true)]);
        let mut storm = vec![egui::Event::PointerMoved(b); 257];
        storm.push(button(b, false));
        frame(&mut h, storm);
        assert!(h.app.custom_tool.as_ref().unwrap().stroke.is_none());
        assert!(!h.app.custom_tool.as_ref().unwrap().stroking);
        assert_eq!(h.doc().export_fingerprint(), before);
    }
    #[test]
    fn switching_tool_sessions_invalidates_the_actual_native_preview_mesh() {
        use std::hash::{Hash, Hasher};
        fn mesh(h: &mut Harness) -> u64 {
            for _ in 0..2 {
                frame(h, vec![]);
            }
            let area = h.app.roll.notes_rect().unwrap();
            let shapes = h.app_frame(egui::vec2(1366., 768.), 1.);
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            let mut count = 0;
            for c in shapes {
                if c.clip_rect.intersects(area)
                    && let egui::Shape::Mesh(mesh) = c.shape
                {
                    for v in &mesh.vertices {
                        v.pos.x.to_bits().hash(&mut hash);
                        v.pos.y.to_bits().hash(&mut hash);
                        v.color.to_array().hash(&mut hash);
                    }
                    mesh.indices.hash(&mut hash);
                    count += mesh.vertices.len();
                }
            }
            assert!(count > 0, "actual native note mesh must be present");
            hash.finish()
        }
        let (mut h, _temp) = setup_source(false, true);
        let points = [(240, 60.5, 0.), (960, 64.5, 0.4)];
        let freehand = studio_preview(&mut h, &points);
        let first = mesh(&mut h);
        h.app.open_custom_tool("test.draw", "rectangle");
        let rectangle = studio_preview(&mut h, &points);
        let second = mesh(&mut h);
        assert_ne!(freehand, rectangle, "author algorithms are different");
        assert_ne!(
            first, second,
            "a new tool session must redraw its actual note geometry"
        );
    }
}
