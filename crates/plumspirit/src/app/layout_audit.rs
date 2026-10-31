//! L-S36 (contract §025, L-F13): the first-impression layout audit. The main
//! window — Track List and Piano Roll, a dense project with short and long
//! Track names — drawn at 1366 × 768, 1920 × 1080 and 2560 × 1600, system
//! scaling 100 – 200 % and app UI 75 – 150 %: no text of the menu bar, the
//! toolbar, the track panel or the status bar may overlap another one or leave
//! the window. Observed defects only are repaired (§025).

use super::test_support::Harness;
use super::track_list::MainView;
use egui::{Pos2, Rect, Vec2};
use plumspirit_engine::fixtures::{FixtureShape, generate};

/// Every visible text with its rectangle (clipped by its clip rectangle).
fn text_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Rect)> {
    fn walk(s: &egui::Shape, clip: Rect, out: &mut Vec<(String, Rect)>) {
        match s {
            egui::Shape::Text(t) => {
                // The glyphs' rows, not the galley's layout box (a right-
                // aligned or wrapped job's box spans its whole width).
                let glyphs = t
                    .galley
                    .rows
                    .iter()
                    .filter(|r| !r.row.glyphs.is_empty())
                    .map(|r| r.rect_without_leading_space())
                    .reduce(|a, b| a.union(b));
                let Some(g) = glyphs else {
                    return;
                };
                let r = g.translate(t.pos.to_vec2()).intersect(clip);
                let text = t.galley.text().trim().to_owned();
                if r.width() > 0.5 && r.height() > 0.5 && !text.is_empty() {
                    out.push((text, r));
                }
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in shapes {
        walk(&c.shape, c.clip_rect, &mut out);
    }
    out
}

fn project() -> plumspirit_engine::model::PlumDocument {
    let mut d = generate(FixtureShape::Dense, 60_000, 960, 11);
    while d.track_count() < 12 {
        d.add_track().unwrap();
    }
    for (i, name) in [
        "Lead",
        "A very long track name that a Black MIDI artist might really use for a layer",
        "Bass",
        "Ｗｉｄｅ　ｎａｍｅ",
        "Track 5 — chords and spam",
    ]
    .iter()
    .enumerate()
    {
        let edit =
            plumspirit_engine::editor::controllers::prepare_rename(d.track(i).unwrap(), name)
                .unwrap();
        let mut h = plumspirit_engine::history::History::new(Default::default());
        h.apply(
            &mut d,
            plumspirit_engine::history::Operation::Events {
                edit,
                label: "Rename Track",
            },
        )
        .unwrap();
    }
    d
}

/// The problems of one drawn frame: texts of the fixed bars that overlap
/// one another, and texts outside the window.
fn problems(
    shapes: &[egui::epaint::ClippedShape],
    screen: Rect,
    bars: &[Rect],
) -> (Vec<String>, usize) {
    let texts = text_rects(shapes);
    let mut out = Vec::new();
    for (t, r) in &texts {
        if !screen.expand(1.0).contains_rect(*r) {
            out.push(format!("outside the window: {t:?} at {r:?}"));
        }
    }
    let in_bars: Vec<&(String, Rect)> = texts
        .iter()
        .filter(|(_, r)| bars.iter().any(|b| b.contains(r.center())))
        .collect();
    for (i, (a, ra)) in in_bars.iter().enumerate() {
        for (b, rb) in in_bars.iter().skip(i + 1) {
            let o = ra.intersect(*rb);
            // The same text drawn twice at one place is a shadow / outline.
            let same = a == b && (ra.center() - rb.center()).length() < 1.0;
            if o.width() > 2.0 && o.height() > 2.0 && !same {
                out.push(format!("overlap: {a:?} at {ra:?} and {b:?} at {rb:?}"));
            }
        }
    }
    (out, in_bars.len())
}

/// §025: the audit itself — every combination, every problem listed.
#[test]
fn the_main_window_has_no_overlapping_or_cut_off_bar_texts_at_any_size_and_scale() {
    let mut found = Vec::new();
    let mut frames = 0;
    for (w, hgt) in [(1366.0, 768.0), (1920.0, 1080.0), (2560.0, 1600.0)] {
        for native in [1.0f32, 1.25, 1.5, 2.0] {
            for ui in [75u16, 100, 150] {
                for view in [MainView::TrackList, MainView::PianoRoll] {
                    let mut h = Harness::new(project());
                    h.app.settings.ui_scale_percent = ui;
                    h.app.main_view = view;
                    h.set_track(1);
                    let physical = Vec2::new(w, hgt);
                    let mut shapes = Vec::new();
                    for _ in 0..4 {
                        h.app.apply_ui_scale(&h.ctx);
                        shapes = h.app_frame(physical, native);
                    }
                    frames += 1;
                    let ppp = native * h.ctx.zoom_factor();
                    let screen = Rect::from_min_size(Pos2::ZERO, physical / ppp);
                    // The fixed bars, as egui laid them out this frame.
                    let bars: Vec<Rect> = ["menu", "toolbar", "transport", "status"]
                        .iter()
                        .filter_map(|id| {
                            egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new(*id))
                        })
                        .map(|p| p.outer_rect)
                        .collect();
                    assert_eq!(bars.len(), 4, "every bar is drawn");
                    let (list, checked) = problems(&shapes, screen, &bars);
                    // Not vacuous: menus, toolbar, transport and status texts.
                    assert!(
                        checked >= 15,
                        "{w}x{hgt} {native} {ui} {view:?}: {checked} bar texts"
                    );
                    for p in list {
                        found.push(format!(
                            "{w}x{hgt} system {:.0} % UI {ui} % {view:?}: {p}",
                            native * 100.0
                        ));
                    }
                }
            }
        }
    }
    found.sort();
    found.dedup();
    assert_eq!(frames, 72);
    assert!(
        found.is_empty(),
        "{} layout problem(s):\n{}",
        found.len(),
        found.join("\n")
    );
}
