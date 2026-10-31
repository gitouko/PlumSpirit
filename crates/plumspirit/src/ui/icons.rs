//! Small original vector icons (v0.8 §158–§159), drawn with egui primitives
//! in the text colour — no image assets, nothing copied from other software.

use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Select,
    Draw,
    Erase,
    Undo,
    Redo,
    Mute,
    Unmute,
    /// v1.0-beta1 (B1-0058): Settings.
    Gear,
}

/// Paints `icon` centred in `r` (about 14 × 14 points).
pub fn paint(p: &Painter, r: Rect, icon: Icon, c: Color32) {
    let s = Stroke::new(1.4, c);
    let o = r.center();
    let at = |x: f32, y: f32| o + Vec2::new(x, y);
    match icon {
        Icon::Select => {
            // An arrow pointer.
            let pts = vec![
                at(-4.0, -7.0),
                at(-4.0, 5.0),
                at(-1.0, 2.5),
                at(1.5, 7.0),
                at(3.5, 6.0),
                at(1.0, 1.5),
                at(5.0, 1.5),
            ];
            p.add(Shape::closed_line(pts, s));
        }
        Icon::Draw => {
            // A pencil: shaft, tip, eraser band.
            p.line_segment([at(-5.0, 5.0), at(4.0, -4.0)], Stroke::new(3.2, c));
            p.add(Shape::convex_polygon(
                vec![at(-7.0, 7.0), at(-6.3, 3.6), at(-3.6, 6.3)],
                c,
                Stroke::NONE,
            ));
            p.line_segment([at(3.0, -5.0), at(5.0, -3.0)], Stroke::new(1.0, c));
        }
        Icon::Erase => {
            // An eraser block, tilted, over a baseline.
            let pts = vec![
                at(-6.0, 2.0),
                at(0.0, -4.0),
                at(6.0, 2.0),
                at(2.0, 6.0),
                at(-2.0, 6.0),
            ];
            p.add(Shape::closed_line(pts, s));
            p.line_segment([at(-3.0, -1.0), at(3.0, 5.0)], s);
        }
        Icon::Undo | Icon::Redo => {
            let flip = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let pt = |x: f32, y: f32| at(x * flip, y);
            // A hooked arrow: an arc from the right back to the left.
            let arc: Vec<Pos2> = (0..=12)
                .map(|k| {
                    let a = std::f32::consts::PI * (k as f32 / 12.0);
                    pt(5.0 * a.cos(), 1.5 - 4.5 * a.sin())
                })
                .collect();
            p.add(Shape::line(arc, s));
            p.add(Shape::convex_polygon(
                vec![pt(-5.0, 6.0), pt(-8.0, 1.5), pt(-2.0, 1.5)],
                c,
                Stroke::NONE,
            ));
        }
        Icon::Mute | Icon::Unmute => {
            // A note head and stem; Mute: a slash across it, hollow head.
            let head = at(-2.0, 4.0);
            if icon == Icon::Mute {
                p.circle_stroke(head, 3.0, s);
            } else {
                p.circle_filled(head, 3.0, c);
            }
            p.line_segment([at(1.0, 4.0), at(1.0, -7.0)], s);
            p.line_segment([at(1.0, -7.0), at(5.0, -4.5)], s);
            if icon == Icon::Mute {
                p.line_segment([at(-7.0, 7.0), at(7.0, -7.0)], Stroke::new(1.6, c));
            }
        }
        Icon::Gear => {
            // A ring with eight teeth and a hole.
            p.circle_stroke(o, 4.2, s);
            p.circle_stroke(o, 1.6, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let (x, y) = (a.cos(), a.sin());
                p.line_segment(
                    [at(4.2 * x, 4.2 * y), at(6.6 * x, 6.6 * y)],
                    Stroke::new(2.0, c),
                );
            }
        }
    }
}

/// A small icon button (24 × 22) with a tooltip; returns its response.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, enabled: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(24.0, 22.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let v = ui.visuals();
    let w = if !enabled {
        &v.widgets.noninteractive
    } else if resp.is_pointer_button_down_on() {
        &v.widgets.active
    } else if resp.hovered() {
        &v.widgets.hovered
    } else {
        &v.widgets.inactive
    };
    let p = ui.painter();
    p.rect_filled(rect, w.corner_radius, w.weak_bg_fill);
    let c = if enabled {
        w.fg_stroke.color
    } else {
        v.widgets
            .noninteractive
            .fg_stroke
            .color
            .gamma_multiply(0.45)
    };
    paint(p, rect, icon, c);
    if resp.has_focus() {
        // B1-0069: keyboard focus is always visible.
        let a = v.widgets.active.bg_stroke;
        p.rect_stroke(rect, w.corner_radius, a, egui::StrokeKind::Inside);
    }
    resp.on_hover_text(tip)
}
