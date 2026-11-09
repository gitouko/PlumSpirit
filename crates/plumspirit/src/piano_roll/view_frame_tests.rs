//! Actual native egui capture; not an interactive Windows execution test.
use super::*;
use plumspirit_engine::{
    history::History,
    model::{Bpm, ClipId, NoteOffKind, NoteRecord, project::KeyRange},
};
#[test]
fn authorized_native_view_capture_preserves_context_notes_and_existing_input_across_zoom_resize_keys_and_clip_shift()
 {
    for extended in [false, true] {
        for scale in [1.0, 1.5] {
            let mut doc = PlumDocument::new_project("View capture".into(), Bpm::DEFAULT, 960);
            if extended {
                doc.project_mut().keys = KeyRange::Extended256;
            }
            let key = if extended { 200 } else { 60 };
            let origin = 9_007_199_254_740_993;
            let note = NoteRecord::new(
                origin + 960,
                960,
                NoteAttrs::new_wide(key, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            );
            doc.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&[note], false)
                .unwrap();
            let track = doc.track(0).unwrap().id();
            let op = plumspirit_engine::editor::clips::duplicate_clips(
                &doc,
                &[(ClipId::Primary(track), 0, 9700)],
                false,
            )
            .unwrap();
            History::default().apply(&mut doc, op).unwrap();
            let content = doc.clips().contents()[0].id;
            let before_count = doc.note_count();
            let meter = MeterMap::new(doc.division(), &doc.conductor().meter_events());
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let run =
                |roll: &mut PianoRoll, width: f32, shift: i128, locked: bool, conductor: bool| {
                    let raw = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 700.0))),
                        ..Default::default()
                    };
                    let mut actions = Vec::new();
                    let mut output = ctx.run_ui(raw, |ui| {
                        let mut input =
                            super::tests::session_input(&doc, &meter, 0, Some(content), shift);
                        input.locked = locked.then_some("Native locked fixture");
                        input.conductor_active = conductor;
                        actions = roll.show(ui, input, 1);
                    });
                    output.textures_delta.clear();
                    actions
                };
            let mut roll = PianoRoll::default();
            run(&mut roll, 1280.0, 9700, false, false);
            assert!(roll.view_frame().is_none());
            roll.capture_view(true);
            roll.view.time = plumspirit_engine::view::TimeAxis::new(origin, 0.5);
            roll.view.pitch.scroll_rows =
                f64::from(doc.project().keys.max_pitch()) - f64::from(key) - 4.25;
            for (width, shift, locked, conductor) in [
                (1280.0, 9700, false, false),
                (900.0, -1200, true, false),
                (1100.0, 0, false, true),
            ] {
                let actions = run(&mut roll, width, shift, locked, conductor);
                assert!(
                    actions.is_empty(),
                    "passive capture must not create editing actions"
                );
                let frame = roll.view_frame().unwrap();
                assert!(frame.matches(&doc));
                assert_eq!(frame.rect, roll.notes_rect().unwrap());
                assert_eq!(frame.track, track);
                assert_eq!(frame.content, Some(content));
                assert_eq!(frame.shift, shift);
                assert_eq!(frame.locked, locked);
                assert_eq!(frame.conductor, conductor);
                assert_eq!(frame.view.pitch.top, doc.project().keys.max_pitch());
                assert_eq!(frame.pixels_per_point, scale);
                assert_eq!(frame.view.time.origin_tick, origin);
                assert_eq!(frame.view.time.tick_to_x(origin + 2), 1.0);
                roll.view.time.zoom_around(0.0, 0.5); // keep the native exact anchor for the next resize
            }
            roll.capture_view(false);
            assert!(roll.view_frame().is_none());
            assert_eq!(doc.note_count(), before_count);
            let mut starts = Vec::new();
            doc.track(0)
                .unwrap()
                .notes
                .query(origin, origin + 2000, |_, n| starts.push(n.start_tick));
            assert_eq!(starts, vec![origin + 960]);
        }
    }
}
