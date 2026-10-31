//! Custom curve functions: the named formula list (user settings) and its
//! editor. Formulas are safe arithmetic in `t` (0 … 1) — see
//! `plumspirit_engine::editor::expr`; nothing else can run.

use super::curves::CurvePreset;
use crate::ui::theme::{self, Palette, space};
use egui::{Color32, Pos2, RichText, Sense, Stroke, Vec2};
use plumspirit_engine::editor::expr::{Expr, FUNCTIONS, MAX_EXPR_LEN};

#[derive(Clone, Debug)]
pub struct FunctionsDraft {
    pub presets: Vec<CurvePreset>,
    pub selected: usize,
    pub error: Option<String>,
}

impl FunctionsDraft {
    pub fn new(presets: &[CurvePreset], chosen: &str) -> Self {
        let mut presets = presets.to_vec();
        if presets.is_empty() {
            presets = super::curves::default_presets();
        }
        let selected = presets.iter().position(|p| p.name == chosen).unwrap_or(0);
        FunctionsDraft {
            presets,
            selected,
            error: None,
        }
    }

    /// Checks the list before it is saved: names unique and non-empty, every
    /// formula valid.
    pub fn validate(&self) -> Result<(), String> {
        for (i, p) in self.presets.iter().enumerate() {
            let name = p.name.trim();
            if name.is_empty() {
                return Err(format!("Function {} needs a name.", i + 1));
            }
            if name.chars().count() > 80 {
                return Err(format!("The name “{name}” is longer than 80 characters."));
            }
            if self.presets[..i].iter().any(|q| q.name.trim() == name) {
                return Err(format!(
                    "Two functions are called “{name}”; names must differ."
                ));
            }
            Expr::parse(&p.expr).map_err(|e| format!("“{name}”: {e}"))?;
        }
        Ok(())
    }

    fn unique_name(&self, base: &str) -> String {
        let mut n = 1;
        loop {
            let candidate = if n == 1 {
                base.to_owned()
            } else {
                format!("{base} {n}")
            };
            if !self.presets.iter().any(|p| p.name == candidate) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Samples `f(t)` for the preview (the same evaluator the curves use).
pub fn sample(expr: &Expr, n: usize) -> Result<Vec<(f64, f64)>, String> {
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            expr.eval(t).map(|v| (t, v)).map_err(|e| e.to_string())
        })
        .collect()
}

fn plot(ui: &mut egui::Ui, pts: &[(f64, f64)], pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(260.0, 130.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, pal.roll_bg);
    p.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, pal.panel_border),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(8.0);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for &(_, v) in pts {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if hi - lo < 1e-12 {
        hi = lo + 1.0;
    }
    let to = |t: f64, v: f64| {
        Pos2::new(
            inner.left() + t as f32 * inner.width(),
            inner.bottom() - ((v - lo) / (hi - lo)) as f32 * inner.height(),
        )
    };
    // 0 and 1 guides.
    for g in [0.0, 1.0] {
        let y = to(0.0, g).y;
        p.hline(inner.x_range(), y, Stroke::new(1.0, pal.grid_sub));
    }
    let path: Vec<Pos2> = pts.iter().map(|&(t, v)| to(t, v)).collect();
    if path.len() >= 2 {
        p.add(egui::Shape::line(path, Stroke::new(1.5, pal.accent)));
    }
    p.text(
        inner.left_top(),
        egui::Align2::LEFT_TOP,
        format!("{hi:.3}"),
        egui::FontId::proportional(theme::size::SMALL),
        pal.text_weak,
    );
    p.text(
        inner.left_bottom(),
        egui::Align2::LEFT_BOTTOM,
        format!("{lo:.3}"),
        egui::FontId::proportional(theme::size::SMALL),
        pal.text_weak,
    );
}

/// The Custom Functions editor body.
pub fn functions_ui(ui: &mut egui::Ui, d: &mut FunctionsDraft, pal: &Palette) {
    let small = |s: &str| RichText::new(s).size(theme::size::SMALL);
    ui.horizontal_top(|ui| {
        // The vertical separator takes the row's whole height: cap it, or the
        // row fills the window and pushes the help text and OK off screen.
        ui.set_max_height(270.0);
        ui.vertical(|ui| {
            ui.set_width(150.0);
            egui::ScrollArea::vertical()
                .id_salt("functions-list")
                .max_height(230.0)
                .show(ui, |ui| {
                    for (i, p) in d.presets.iter().enumerate() {
                        if ui.selectable_label(d.selected == i, p.name.as_str()).clicked() {
                            d.selected = i;
                        }
                    }
                });
            ui.add_space(space::MICRO);
            ui.horizontal(|ui| {
                if ui.button("Add").clicked() && d.presets.len() < crate::settings::MAX_CURVE_PRESETS {
                    let name = d.unique_name("New function");
                    d.presets.push(CurvePreset {
                        name,
                        expr: "t".into(),
                    });
                    d.selected = d.presets.len() - 1;
                }
                if ui
                    .add_enabled(d.presets.len() > 1, egui::Button::new("Delete"))
                    .clicked()
                {
                    d.presets.remove(d.selected);
                    d.selected = d.selected.min(d.presets.len() - 1);
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            let Some(p) = d.presets.get_mut(d.selected) else {
                return;
            };
            ui.label(small("Name (rename here)"));
            ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(260.0).char_limit(80));
            ui.label(small("f(t) =   (t goes from 0 at the start anchor to 1 at the end anchor)"));
            ui.add(
                egui::TextEdit::singleline(&mut p.expr)
                    .desired_width(260.0)
                    .font(egui::TextStyle::Monospace)
                    .char_limit(MAX_EXPR_LEN),
            );
            match Expr::parse(&p.expr) {
                Ok(e) => match sample(&e, 120) {
                    Ok(pts) => {
                        plot(ui, &pts, pal);
                        let (f0, f1) = (pts[0].1, pts[pts.len() - 1].1);
                        let ends = if (f0 - 0.0).abs() < 1e-9 && (f1 - 1.0).abs() < 1e-9 {
                            "f(0) = 0 and f(1) = 1: the curve ends exactly on the anchors.".to_owned()
                        } else {
                            format!("f(0) = {f0:.4}, f(1) = {f1:.4}: the curve does not end on the anchors unless Normalize Endpoints is on (Curve menu).")
                        };
                        ui.label(small(&ends).color(pal.text_weak));
                    }
                    Err(err) => {
                        ui.label(small(&err).color(pal.error));
                    }
                },
                Err(err) => {
                    ui.label(small(&err.to_string()).color(pal.error));
                }
            }
        });
    });
    ui.add_space(space::MICRO);
    ui.label(
        small(&format!(
            "Allowed: numbers, t, pi, e, + − * / ^, parentheses and {}. Examples: t*t · sqrt(t) · sin(pi*t/2) · (1-cos(pi*t))/2 · t*t*(3-2*t). A value of 0 maps to the start anchor's value, 1 to the end anchor's.",
            FUNCTIONS.join(", ")
        ))
        .color(pal.text_weak),
    );
    if let Some(e) = &d.error {
        ui.colored_label(Color32::from_rgb(0xD0, 0x50, 0x50), e.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_function_list_is_validated_before_saving() {
        let mut d = FunctionsDraft::new(&[], "");
        assert!(d.validate().is_ok());
        d.presets.push(CurvePreset {
            name: "Linear".into(),
            expr: "t".into(),
        });
        assert!(d.validate().unwrap_err().contains("names must differ"));
        d.presets.pop();
        d.presets.push(CurvePreset {
            name: "Bad".into(),
            expr: "system(t)".into(),
        });
        assert!(d.validate().is_err());
        assert!(sample(&Expr::parse("t*t").unwrap(), 4).unwrap()[2].1 - 0.25 < 1e-12);
    }
}
