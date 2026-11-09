use super::*;
use crate::piano_roll::{RollView, ToolPreview};
use plumspirit_engine::model::{Bpm, PlumDocument};
use plumspirit_engine::transform::TNote;
use plumspirit_engine::view::{PitchAxis, TimeAxis};
fn doc(keys: u16, notes: &[TNote]) -> PlumDocument {
    let mut d = PlumDocument::new_project("Store preview".into(), Bpm::DEFAULT, 960);
    if keys == 256 {
        d.project_mut().keys = KeyRange::Extended256;
    }
    let recs = notes.iter().map(TNote::to_record).collect::<Vec<_>>();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&recs, true)
        .unwrap();
    d
}
fn geometry(top: u8, origin: u64, ppt: f64, scroll: f64) -> Geometry {
    Geometry::new(
        Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 650.)),
        RollView {
            time: TimeAxis::new(origin, ppt),
            pitch: PitchAxis {
                top,
                scroll_rows: scroll,
                row_height: 5.,
            },
        },
        1.,
    )
}
fn paint(
    d: &PlumDocument,
    tp: &ToolPreview<'_>,
    g: &Geometry,
    cache: &mut Option<ToolPreviewCache>,
) -> (usize, bool) {
    let ctx = egui::Context::default();
    let pal = crate::ui::theme::palette(egui::Theme::Dark);
    let mut answer = (0, false);
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.ctx().layer_painter(egui::LayerId::background());
        answer = draw_tool_preview(
            &painter,
            g,
            d.track(0).unwrap().into(),
            tp,
            pal,
            true,
            &mut crate::ui::theme::SubPalettes::default(),
            cache,
        );
    });
    out.textures_delta.clear();
    answer
}
#[test]
fn native_store_matches_original_run_vertices_colors_and_clipping_in_both_key_modes() {
    for keys in [128, 256] {
        let origin = 9007199254741000;
        let notes = (0..128)
            .map(|i| TNote {
                start: origin + i * 12,
                dur: 24,
                pitch: if keys == 256 {
                    180 + (i % 40) as u8
                } else {
                    40 + (i % 40) as u8
                },
                vel: 40 + (i % 80) as u8,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let d = doc(keys, &notes);
        let stored = &d.track(0).unwrap().notes;
        for (ppt, scroll) in [(0.7, 0.), (1.4, 5.), (2.3, 15.)] {
            let g = geometry((keys - 1) as u8, origin + 120, ppt, scroll);
            let mut a = None;
            let mut b = None;
            let run = ToolPreview::Run {
                notes: &notes,
                extra: &[],
                max_dur: 24,
                all: true,
                rev: 91,
            };
            let store = ToolPreview::Store {
                notes: stored,
                all: true,
                rev: 92,
            };
            assert!(store.all());
            assert_eq!(paint(&d, &run, &g, &mut a), paint(&d, &store, &g, &mut b));
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_eq!(a.notes.vertices, b.notes.vertices);
            assert_eq!(a.notes.indices, b.notes.indices);
            assert_eq!(a.coverage.vertices, b.coverage.vertices);
        }
    }
}
#[test]
fn store_cache_reuses_mesh_and_invalidates_for_content_view_and_key_range_changes() {
    let a = doc(
        128,
        &[TNote {
            start: 100,
            dur: 80,
            pitch: 60,
            vel: 90,
            ..Default::default()
        }],
    );
    let b = doc(
        128,
        &[TNote {
            start: 200,
            dur: 40,
            pitch: 70,
            vel: 110,
            ..Default::default()
        }],
    );
    assert_eq!(
        a.track(0).unwrap().notes.revision(),
        b.track(0).unwrap().notes.revision()
    );
    let ta = ToolPreview::Store {
        notes: &a.track(0).unwrap().notes,
        all: true,
        rev: 99,
    };
    let tb = ToolPreview::Store {
        notes: &b.track(0).unwrap().notes,
        all: true,
        rev: 99,
    };
    let mut cache = None;
    let g = geometry(127, 0, 1., 0.);
    paint(&a, &ta, &g, &mut cache);
    let first = Arc::clone(&cache.as_ref().unwrap().notes);
    paint(&a, &ta, &g, &mut cache);
    assert!(Arc::ptr_eq(&first, &cache.as_ref().unwrap().notes));
    paint(&a, &tb, &g, &mut cache);
    assert!(!Arc::ptr_eq(&first, &cache.as_ref().unwrap().notes));
    let second = Arc::clone(&cache.as_ref().unwrap().notes);
    paint(&a, &tb, &geometry(127, 100, 2., 2.), &mut cache);
    assert!(!Arc::ptr_eq(&second, &cache.as_ref().unwrap().notes));
    let mut standard = None;
    paint(&a, &ta, &g, &mut standard);
    let before = Arc::clone(&standard.as_ref().unwrap().notes);
    paint(&a, &ta, &geometry(255, 0, 1., 0.), &mut standard);
    assert!(!Arc::ptr_eq(&before, &standard.as_ref().unwrap().notes));
    let mut fresh = None;
    paint(&a, &ta, &geometry(255, 0, 1., 0.), &mut fresh);
    assert_eq!(
        standard.unwrap().notes.vertices,
        fresh.unwrap().notes.vertices
    );
}
#[test]
fn dense_and_off_row_store_previews_stop_and_cache_without_result_vector() {
    for pitch in [60, 250] {
        let n = 400_000;
        let notes = (0..n)
            .map(|i| TNote {
                start: i as u64,
                dur: 100,
                pitch,
                vel: 90,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let d = doc(256, &notes);
        drop(notes);
        let store = ToolPreview::Store {
            notes: &d.track(0).unwrap().notes,
            all: true,
            rev: 100,
        };
        let mut cache = None;
        let g = geometry(127, 0, 900. / n as f64, 0.);
        let (drawn, cut) = paint(&d, &store, &g, &mut cache);
        assert!(cut);
        assert_eq!(drawn, if pitch == 60 { PREVIEW_DRAW_BUDGET } else { 0 });
        let mesh = Arc::clone(&cache.as_ref().unwrap().notes);
        assert_eq!(paint(&d, &store, &g, &mut cache), (drawn, cut));
        assert!(Arc::ptr_eq(&mesh, &cache.as_ref().unwrap().notes));
    }
}
