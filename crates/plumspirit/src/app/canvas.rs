//! Shared native canvas display-list generation. Never executes Lua or hit testing.
use crate::lua::ui::{images, schema::Primitive};
use egui::{Color32, Pos2, Rect, Shape, Stroke};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Images<'a> {
    pub cached: &'a mut BTreeMap<u64, images::Prepared>,
    pub visible: &'a mut BTreeSet<u64>,
    pub upload_pixels: &'a mut usize,
}
/// Uses the caller's actual native clip/style/texture state. The bounded parsed
/// primitives are shared by panels and future reserved editor overlay layers.
pub(super) fn display_list(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: Rect,
    primitives: &[Primitive],
    images: Images<'_>,
) -> Vec<Shape> {
    let point = |p: [f64; 2]| {
        Pos2::new(
            rect.left() + (p[0] * f64::from(rect.width())) as f32,
            rect.top() + (p[1] * f64::from(rect.height())) as f32,
        )
    };
    let color = |c: &[u8; 4]| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
    let mut shapes = Vec::with_capacity(primitives.len());
    for primitive in primitives {
        match primitive {
            Primitive::Gradient { min, max, colors } => {
                let a = point(*min);
                let b = point(*max);
                let positions = [a, egui::pos2(b.x, a.y), egui::pos2(a.x, b.y), b];
                let mut mesh = egui::Mesh::default();
                for (p, c) in positions.into_iter().zip(colors) {
                    mesh.colored_vertex(p, color(c));
                }
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(2, 1, 3);
                shapes.push(Shape::mesh(mesh));
            }
            Primitive::Image {
                resource,
                min,
                max,
                uv_min,
                uv_max,
                color: c,
            } => {
                let r = Rect::from_min_max(point(*min), point(*max));
                if r.intersect(painter.clip_rect()).is_positive() && resource.alive() {
                    images.visible.insert(resource.id);
                    if !images.cached.contains_key(&resource.id)
                        && let Some(image) = resource.prepare(ui.ctx(), images.upload_pixels)
                    {
                        images.cached.insert(resource.id, image);
                    }
                    if let Some(image) = images.cached.get(&resource.id) {
                        shapes.push(Shape::image(
                            image.texture.id(),
                            r,
                            Rect::from_min_max(
                                egui::pos2(uv_min[0] as f32, uv_min[1] as f32),
                                egui::pos2(uv_max[0] as f32, uv_max[1] as f32),
                            ),
                            color(c),
                        ));
                    }
                }
            }
            Primitive::Line {
                points,
                color: c,
                width,
                closed,
            } => {
                let points = points.iter().copied().map(point).collect();
                shapes.push(if *closed {
                    Shape::closed_line(points, Stroke::new(*width, color(c)))
                } else {
                    Shape::line(points, Stroke::new(*width, color(c)))
                });
            }
            Primitive::Rect {
                min,
                max,
                color: c,
                fill,
                width,
            } => {
                let r = Rect::from_min_max(point(*min), point(*max));
                shapes.push(if *fill {
                    Shape::rect_filled(r, 0.0, color(c))
                } else {
                    Shape::rect_stroke(
                        r,
                        0.0,
                        Stroke::new(*width, color(c)),
                        egui::StrokeKind::Inside,
                    )
                });
            }
            Primitive::Circle {
                center,
                radius,
                color: c,
                fill,
                width,
            } => {
                shapes.push(if *fill {
                    Shape::circle_filled(point(*center), *radius, color(c))
                } else {
                    Shape::circle_stroke(point(*center), *radius, Stroke::new(*width, color(c)))
                });
            }
            Primitive::Text {
                point: p,
                text,
                color: c,
            } => {
                let galley = painter.layout_no_wrap(
                    text.clone(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    color(c),
                );
                if !galley.is_empty() {
                    shapes.push(Shape::galley(point(*p), galley, color(c)));
                }
            }
        }
    }
    shapes
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_text_matches_native_painter_opacity_including_empty_text() {
        for opacity in [1.0, 0.35, 0.0] {
            let ctx = egui::Context::default();
            let rect = Rect::from_min_size(Pos2::new(80.0, 90.0), egui::vec2(400.0, 200.0));
            let color = [160, 40, 230, 128];
            let primitives = [
                Primitive::Text {
                    point: [0.25, 0.5],
                    text: "native text α".into(),
                    color,
                },
                Primitive::Text {
                    point: [0.5, 0.5],
                    text: String::new(),
                    color,
                },
            ];
            let mut cache = BTreeMap::new();
            let mut visible = BTreeSet::new();
            let mut upload = images::MAX_UPLOAD_PIXELS;
            let mut count = 0;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    let mut painter = ui.painter_at(rect);
                    painter.set_opacity(opacity);
                    let shapes = display_list(
                        ui,
                        &painter,
                        rect,
                        &primitives,
                        Images {
                            cached: &mut cache,
                            visible: &mut visible,
                            upload_pixels: &mut upload,
                        },
                    );
                    count = shapes.len();
                    painter.text(
                        Pos2::new(180.0, 190.0),
                        egui::Align2::LEFT_TOP,
                        "native text α",
                        egui::TextStyle::Body.resolve(ui.style()),
                        Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
                    );
                    painter.text(
                        Pos2::new(280.0, 190.0),
                        egui::Align2::LEFT_TOP,
                        "",
                        egui::TextStyle::Body.resolve(ui.style()),
                        Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
                    );
                    painter.extend(shapes);
                },
            );
            let texts: Vec<_> = out
                .shapes
                .iter()
                .filter(|s| matches!(s.shape, Shape::Text(_)))
                .collect();
            if opacity == 0.0 {
                assert!(texts.is_empty());
            } else {
                assert_eq!(texts.len(), count * 2);
                assert_eq!(&texts[..count], &texts[count..]);
            }
            out.textures_delta.clear();
        }
    }
    #[test]
    fn generated_shapes_fit_an_existing_reserved_native_layer_below_selection_and_keep_clip() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_max(egui::pos2(100.0, 80.0), egui::pos2(500.0, 280.0));
        let primitives = [
            Primitive::Line {
                points: vec![[0.0, 0.25], [1.0, 0.25]],
                color: [0, 0, 255, 128],
                width: 2.0,
                closed: false,
            },
            Primitive::Text {
                point: [0.25, 0.5],
                text: "author guide".into(),
                color: [255, 255, 255, 200],
            },
        ];
        let mut cache = BTreeMap::new();
        let mut visible = BTreeSet::new();
        let mut upload = images::MAX_UPLOAD_PIXELS;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                ..Default::default()
            },
            |ui| {
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 0.0, Color32::RED);
                let reserved = painter.add(Shape::Noop);
                painter.circle_filled(rect.center(), 3.0, Color32::GREEN);
                let shapes = display_list(
                    ui,
                    &painter,
                    rect,
                    &primitives,
                    Images {
                        cached: &mut cache,
                        visible: &mut visible,
                        upload_pixels: &mut upload,
                    },
                );
                assert_eq!(shapes.len(), 2);
                let Shape::Path(path) = &shapes[0] else {
                    panic!("native line shape")
                };
                assert_eq!(
                    path.points,
                    vec![egui::pos2(100.0, 130.0), egui::pos2(500.0, 130.0)]
                );
                assert_eq!(
                    path.stroke.color,
                    egui::epaint::ColorMode::Solid(Color32::from_rgba_unmultiplied(0, 0, 255, 128))
                );
                let Shape::Text(text) = &shapes[1] else {
                    panic!("native text shape")
                };
                assert_eq!(text.pos, egui::pos2(200.0, 180.0));
                assert_eq!(text.galley.job.text, "author guide");
                painter.set(reserved, Shape::Vec(shapes));
            },
        );
        let mut red = None;
        let mut group = None;
        let mut green = None;
        for (i, clipped) in out.shapes.iter().enumerate() {
            match &clipped.shape {
                Shape::Rect(shape) if shape.fill == Color32::RED => red = Some(i),
                Shape::Vec(shapes) if shapes.len() == 2 => {
                    assert_eq!(clipped.clip_rect, rect);
                    group = Some(i);
                }
                Shape::Circle(shape) if shape.fill == Color32::GREEN => green = Some(i),
                _ => {}
            }
        }
        assert!(red.unwrap() < group.unwrap() && group.unwrap() < green.unwrap());
        assert!(cache.is_empty() && visible.is_empty());
        assert_eq!(upload, images::MAX_UPLOAD_PIXELS);
        out.textures_delta.clear();
    }
}
