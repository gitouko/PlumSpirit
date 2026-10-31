//! Test helpers for widgets and menus (v0.9 V09-S10): draw a menu builder
//! as a plain list in a headless egui context, read its labels and click
//! one — the same click path a user's pointer takes.

use egui::{Pos2, Rect, Vec2};

/// Every text shape of `shapes` with its centre.
pub fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Pos2)> {
    fn walk(s: &egui::Shape, out: &mut Vec<(String, Pos2)>) {
        match s {
            egui::Shape::Text(t) => out.push((
                t.galley.text().to_owned(),
                t.pos + t.galley.rect.center().to_vec2(),
            )),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// Draws `build` (a menu builder returning what was chosen) in a panel and
/// clicks the item `label` (`None`: only draws). Returns the choice and the
/// labels shown. Panics when `label` is not shown.
pub fn run_menu<T>(
    build: &dyn Fn(&mut egui::Ui) -> Option<T>,
    label: Option<&str>,
) -> (Option<T>, Vec<String>) {
    let ctx = egui::Context::default();
    let mut got = None;
    let frame = |events: Vec<egui::Event>, got: &mut Option<T>| {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 900.0))),
            events,
            ..Default::default()
        };
        let mut o = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                if let Some(a) = build(ui) {
                    *got = Some(a);
                }
            });
        });
        o.textures_delta.clear();
        o.shapes
    };
    frame(vec![], &mut got);
    let shown = texts(&frame(vec![], &mut got));
    let labels: Vec<String> = shown.iter().map(|(t, _)| t.clone()).collect();
    if let Some(l) = label {
        let p = shown
            .iter()
            .find(|(t, _)| t == l)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| panic!("{l:?} not in {labels:?}"));
        let b = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(p)], &mut got);
        frame(vec![b(true)], &mut got);
        frame(vec![b(false)], &mut got);
    }
    (got, labels)
}
