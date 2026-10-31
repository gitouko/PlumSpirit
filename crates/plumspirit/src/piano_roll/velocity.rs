//! Velocity Lane: edits note-on velocities (1–127) under the piano roll.
//!
//! * Shares the roll's horizontal axis: a bar stands at its note's start tick.
//! * Custom painted from the same visible-note query as the roll; no widget per
//!   note and no second note database. Above [`EXACT_BARS`] visible notes the lane
//!   draws one aggregated bar per pixel column (solid to the average velocity of
//!   the notes starting there, faint to the loudest), cached until the view or the
//!   notes change — the data itself stays exact.
//! * Press on a bar and drag vertically: the value follows the pointer and is
//!   applied on release as **one** undoable edit. Dragging a bar of a selected note
//!   sets every selected note to that absolute velocity; pressing an unselected
//!   note's bar selects it first (Shift/Ctrl add it to the selection).

use super::{KEYS_WIDTH, RollAction, SCROLLBAR};
use crate::ui::theme::{Palette, size};
use egui::epaint::{Mesh, Shape};
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use plumspirit_engine::model::{NoteLoc, NoteStore};
use plumspirit_engine::view::TimeAxis;
use plumspirit_engine::view::grid::{GridLine, LineKind, MeterMap};

/// Visible notes above which bars are aggregated per pixel column.
pub const EXACT_BARS: usize = 6_000;
/// Horizontal pick tolerance around a bar, in points.
const PICK_PX: f32 = 4.0;
/// Height of the collapsed lane header.
pub const COLLAPSED_HEIGHT: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    Drag { value: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ColumnKey {
    track: u64,
    revision: u64,
    origin: u64,
    ppt_bits: u64,
    columns: usize,
}

/// Velocities of the notes starting in one physical pixel column.
#[derive(Clone, Copy, Default)]
struct Column {
    max: u8,
    count: u32,
    sum: u32,
    selected: bool,
}

impl Column {
    fn mean(&self) -> u8 {
        (self.sum.div_ceil(self.count.max(1))).min(127) as u8
    }
}

#[derive(Default)]
pub struct VelocityLane {
    gesture: Option<Gesture>,
    /// Aggregated columns (one per physical pixel) when too many notes are visible.
    columns: Vec<Column>,
    key: Option<ColumnKey>,
    grid: Vec<GridLine>,
    /// Hover readout (velocity, x).
    hover: Option<(u8, f32)>,
    /// Subchannel palettes (bars take their note's subchannel hue, v0.5).
    subs: crate::ui::theme::SubPalettes,
}

pub struct LaneInput<'a> {
    pub store: &'a NoteStore,
    /// Cache identity of the note host (`render::host_key`).
    pub key: u64,
    pub time: TimeAxis,
    pub meter: &'a MeterMap,
    pub palette: &'a Palette,
    /// Dark theme (bars use the notes' velocity colours).
    pub dark: bool,
    pub editable: bool,
    pub playhead: Option<u64>,
}

/// What the lane reports besides edits.
#[derive(Default)]
pub struct LaneOutput {
    pub actions: Vec<RollAction>,
    pub aggregated: bool,
}

fn value_at(y: f32, plot: Rect) -> u8 {
    let t = ((plot.bottom() - y) / plot.height()).clamp(0.0, 1.0);
    (1.0 + t * 126.0).round().clamp(1.0, 127.0) as u8
}

fn y_of(v: u8, plot: Rect) -> f32 {
    plot.bottom() - (f32::from(v.saturating_sub(1)) / 126.0) * plot.height()
}

impl VelocityLane {
    pub fn is_dragging(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Drag { .. }))
    }

    pub fn cancel(&mut self) {
        self.gesture = None;
    }

    /// Nearest bar to `pos`: notes starting within a few points of the pointer;
    /// the bar whose top is closest to the pointer wins (selected notes on ties).
    fn pick(&self, input: &LaneInput<'_>, plot: Rect, pos: Pos2) -> Option<(NoteLoc, u8, bool)> {
        let x = f64::from(pos.x - plot.left());
        let t0 = input.time.x_to_tick(x - f64::from(PICK_PX));
        let t1 = input.time.x_to_tick(x + f64::from(PICK_PX));
        let mut best: Option<(f32, bool, NoteLoc, u8)> = None;
        input.store.query(t0, t1, |loc, n| {
            if n.start_tick < t0 || n.start_tick > t1 {
                return;
            }
            let v = n.attrs.velocity();
            let d = (y_of(v, plot) - pos.y).abs();
            let better = match best {
                None => true,
                Some((bd, bsel, ..)) => {
                    d < bd - 0.5 || ((d - bd).abs() <= 0.5 && n.selected && !bsel)
                }
            };
            if better {
                best = Some((d, n.selected, loc, v));
            }
        });
        best.map(|(_, sel, loc, v)| (loc, v, sel))
    }

    /// Draws the lane in `full` (the host has already drawn the header column).
    pub fn show(&mut self, ui: &mut egui::Ui, full: Rect, input: LaneInput<'_>) -> LaneOutput {
        let mut out = LaneOutput::default();
        let pal = input.palette;
        let plot_outer = Rect::from_min_max(
            Pos2::new(full.left() + KEYS_WIDTH, full.top()),
            Pos2::new(full.right() - SCROLLBAR, full.bottom()),
        );
        let plot = plot_outer.shrink2(Vec2::new(0.0, 6.0));
        let id = ui.id().with("velocity_lane");
        let resp = ui.interact(plot_outer, id, Sense::click_and_drag());
        let painter = ui.painter_at(full);

        // ------------------------------------------------------------- input
        let (pointer, pressed, released, down, modifiers, escape) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.modifiers,
                i.key_pressed(egui::Key::Escape),
            )
        });
        self.hover = None;
        if escape {
            self.gesture = None;
        }
        if pressed
            && input.editable
            && resp.contains_pointer()
            && let Some(pos) = pointer
            && let Some((loc, _, selected)) = self.pick(&input, plot, pos)
        {
            if modifiers.shift || modifiers.command {
                out.actions.push(RollAction::AddSelected(loc));
            } else if !selected {
                out.actions.push(RollAction::SelectOnly(loc));
            }
            self.gesture = Some(Gesture::Drag {
                value: value_at(pos.y, plot),
            });
        }
        if let (Some(Gesture::Drag { value }), Some(pos)) = (self.gesture.as_mut(), pointer) {
            *value = value_at(pos.y, plot);
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if let Some(Gesture::Drag { value }) = self.gesture
            && (released || !down)
        {
            out.actions.push(RollAction::SetVelocity(value));
            self.gesture = None;
        }
        if self.gesture.is_none()
            && resp.contains_pointer()
            && let Some(pos) = pointer
            && let Some((_, v, _)) = self.pick(&input, plot, pos)
        {
            self.hover = Some((v, pos.x));
            if input.editable {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
        }

        // ------------------------------------------------------------- paint
        painter.rect_filled(plot_outer, 0.0, pal.roll_bg);
        let small = FontId::proportional(size::SMALL);
        let plot_painter = painter.with_clip_rect(plot_outer);
        for v in [32u8, 64, 96, 127] {
            plot_painter.hline(
                plot.x_range(),
                y_of(v, plot).round() + 0.5,
                Stroke::new(1.0, pal.grid_sub),
            );
        }
        let (t0, t1) = input.time.visible_range(f64::from(plot.width()));
        // Bar lines for orientation (same grid as the roll).
        input.meter.lines(
            t0,
            t1,
            input.time.pixels_per_tick,
            7.0,
            None,
            &mut self.grid,
        );
        for l in self.grid.iter().filter(|l| l.kind == LineKind::Major) {
            let x = (plot.left() + input.time.tick_to_x(l.tick) as f32).round() + 0.5;
            plot_painter.vline(x, plot_outer.y_range(), Stroke::new(1.0, pal.grid_minor));
        }
        let preview = self.gesture.map(|Gesture::Drag { value }| value);
        let estimate = input.store.estimate_in_range(t0, t1);
        let aggregated = estimate > EXACT_BARS;
        out.aggregated = aggregated;
        let bottom = plot.bottom();
        if !aggregated {
            // Stems are snapped to whole physical pixels so every bar has the same width.
            let ppp = ui.ctx().pixels_per_point();
            let stem_w = (1.5 * ppp).round().max(1.0) / ppp;
            let mut normal = Mesh::default();
            let mut selected = Mesh::default();
            let palette = crate::ui::theme::note_palette(ui.ctx());
            let vp = crate::ui::theme::velocity_palette(&palette, input.dark);
            let vp = &*vp;
            let dark = input.dark;
            let subs = &mut self.subs;
            input.store.query(t0, t1, |_, n| {
                if n.start_tick < t0 {
                    return;
                }
                let x =
                    ((plot.left() + input.time.tick_to_x(n.start_tick) as f32) * ppp).floor() / ppp;
                let v = match (preview, n.selected) {
                    (Some(p), true) => p,
                    _ => n.attrs.velocity(),
                };
                let y = y_of(v, plot);
                let sp = if n.sub.0 == 0 {
                    vp
                } else {
                    subs.get(&palette, n.sub.0, dark)
                };
                let (mesh, color) = if n.selected {
                    (&mut selected, sp.selected_fill[usize::from(v)])
                } else {
                    (&mut normal, sp.fill[usize::from(v)])
                };
                // The stem sits in the note's first pixel column.
                mesh.add_colored_rect(
                    Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + stem_w, bottom)),
                    color,
                );
                mesh.add_colored_rect(
                    Rect::from_center_size(Pos2::new(x + stem_w * 0.5, y), Vec2::splat(5.0)),
                    color,
                );
            });
            plot_painter.add(Shape::mesh(normal));
            plot_painter.add(Shape::mesh(selected));
        } else {
            // One bar per physical column: solid up to the average velocity of the
            // notes starting there, faint up to the loudest one.
            let ppp = ui.ctx().pixels_per_point();
            let columns = ((plot.width() * ppp).round() as usize).clamp(1, 8192);
            let key = ColumnKey {
                track: input.key,
                revision: input.store.revision(),
                origin: input.time.origin_tick,
                ppt_bits: input.time.pixels_per_tick.to_bits(),
                columns,
            };
            if self.key != Some(key) {
                self.columns.clear();
                self.columns.resize(columns, Column::default());
                let cols = &mut self.columns;
                let ppt = input.time.pixels_per_tick * f64::from(ppp);
                let origin = input.time.origin_tick;
                input.store.query(t0, t1, |_, n| {
                    if n.start_tick < t0 {
                        return;
                    }
                    let c = (((n.start_tick - origin) as f64 * ppt) as usize).min(columns - 1);
                    let e = &mut cols[c];
                    let v = n.attrs.velocity();
                    e.max = e.max.max(v);
                    e.count = e.count.saturating_add(1);
                    e.sum = e.sum.saturating_add(u32::from(v));
                    e.selected |= n.selected;
                });
                self.key = Some(key);
            }
            let mut normal = Mesh::default();
            let mut selected = Mesh::default();
            let faint = |c: Color32| c.gamma_multiply(0.35);
            // v0.9 §119: selected columns at the Selection Opacity.
            let selected_color = crate::ui::theme::note_palette(ui.ctx())
                .selection_mix(pal.note_fill, pal.note_selected_fill);
            for (c, col) in self.columns.iter().enumerate() {
                if col.count == 0 {
                    continue;
                }
                let (mean, max) = match (preview, col.selected) {
                    (Some(p), true) => (p, p),
                    _ => (col.mean(), col.max),
                };
                let x = plot.left() + c as f32 / ppp;
                let (mesh, color) = if col.selected {
                    (&mut selected, selected_color)
                } else {
                    (&mut normal, pal.note_fill)
                };
                if max > mean {
                    mesh.add_colored_rect(
                        Rect::from_min_max(
                            Pos2::new(x, y_of(max, plot)),
                            Pos2::new(x + 1.0 / ppp, y_of(mean, plot)),
                        ),
                        faint(color),
                    );
                }
                mesh.add_colored_rect(
                    Rect::from_min_max(
                        Pos2::new(x, y_of(mean, plot)),
                        Pos2::new(x + 1.0 / ppp, bottom),
                    ),
                    color,
                );
            }
            plot_painter.add(Shape::mesh(normal));
            plot_painter.add(Shape::mesh(selected));
        }
        // Scale labels go over the stems (on a faint backing) so notes at the
        // very start of the view do not hide them.
        for v in [127u8, 64, 1] {
            if plot.height() >= 40.0 || v != 64 {
                let galley = plot_painter.layout_no_wrap(
                    v.to_string(),
                    small.clone(),
                    pal.text_weak.gamma_multiply(0.8),
                );
                let pos = Pos2::new(plot.left() + 3.0, y_of(v, plot) - galley.size().y / 2.0);
                plot_painter.rect_filled(
                    Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(2.0, 0.0)),
                    0.0,
                    pal.roll_bg.gamma_multiply(0.75),
                );
                plot_painter.galley(pos, galley, pal.text_weak);
            }
        }
        if let Some(t) = input.playhead {
            super::render::draw_playhead(
                &plot_painter,
                plot_outer,
                plot.left(),
                &input.time,
                pal,
                t,
            );
        }
        // Numeric readout.
        let readout = match (preview, self.hover) {
            (Some(v), _) => pointer.map(|p| (format!("Velocity {v}"), p.x)),
            (None, Some((v, x))) => Some((format!("Velocity {v}"), x)),
            _ => None,
        };
        if let Some((text, x)) = readout {
            let pos = Pos2::new((x + 8.0).min(plot.right() - 80.0), plot_outer.top() + 3.0);
            let galley = painter.layout_no_wrap(text, small, pal.text);
            let bg = Rect::from_min_size(pos, galley.size()).expand(3.0);
            painter.rect_filled(bg, 0.0, pal.overlay_bg);
            painter.galley(pos, galley, pal.text);
        }
        out
    }
}

/// The collapsed lane: a thin header that expands the lane when clicked.
pub fn show_collapsed(ui: &mut egui::Ui, full: Rect, pal: &Palette, label: &str) -> bool {
    let resp = ui.interact(
        full,
        ui.id().with("velocity_lane_collapsed"),
        Sense::click(),
    );
    let p = ui.painter_at(full);
    p.rect_filled(full, 0.0, pal.ruler_bg);
    p.hline(
        full.x_range(),
        full.top() + 0.5,
        Stroke::new(1.0, pal.panel_border),
    );
    let color = if resp.hovered() {
        pal.text
    } else {
        pal.ruler_text
    };
    // Right-pointing triangle: "expand".
    let c = Pos2::new(full.left() + 10.0, full.center().y);
    p.add(Shape::convex_polygon(
        vec![
            c + Vec2::new(-2.5, -4.0),
            c + Vec2::new(2.5, 0.0),
            c + Vec2::new(-2.5, 4.0),
        ],
        color,
        Stroke::NONE,
    ));
    p.text(
        Pos2::new(full.left() + 18.0, full.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(size::SMALL),
        color,
    );
    resp.on_hover_text("Show the lane").clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_mapping_is_integer_and_bounded() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 10.0), Pos2::new(100.0, 136.0));
        assert_eq!(value_at(136.0, plot), 1);
        assert_eq!(value_at(10.0, plot), 127);
        assert_eq!(value_at(-50.0, plot), 127);
        assert_eq!(value_at(500.0, plot), 1);
        for v in 1..=127u8 {
            assert_eq!(value_at(y_of(v, plot), plot), v, "round trip {v}");
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn lane_frame_cost() {
        use plumspirit_engine::fixtures::{FixtureShape, generate};
        use plumspirit_engine::model::Division;
        use std::time::Instant;
        let doc = generate(FixtureShape::Dense, 1_000_000, 960, 42);
        let store = &doc.track(1).unwrap().notes;
        let end = store.max_end().unwrap();
        let meter = MeterMap::new(Division::ppqn(960), &[]);
        let ctx = egui::Context::default();
        let mut lane = VelocityLane::default();
        let full = Rect::from_min_size(Pos2::new(0.0, 600.0), Vec2::new(1280.0, 110.0));
        for (name, ppt) in [
            ("4 bars (exact stems)", 1200.0 / (960.0 * 16.0)),
            ("whole song (aggregated)", 1200.0 / end as f64),
        ] {
            let time = TimeAxis::new(0, ppt);
            let mut times = Vec::new();
            for _ in 0..3 {
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let t = Instant::now();
                    let out = lane.show(
                        ui,
                        full,
                        LaneInput {
                            store,
                            key: 1,
                            time,
                            meter: &meter,
                            palette: &crate::ui::theme::DARK,
                            dark: true,
                            editable: true,
                            playhead: None,
                        },
                    );
                    times.push((t.elapsed(), out.aggregated));
                });
            }
            println!(
                "velocity lane, {name}: first frame {:.2} ms, steady {:.2} ms (aggregated: {})",
                times[0].0.as_secs_f64() * 1e3,
                times[2].0.as_secs_f64() * 1e3,
                times[2].1
            );
        }
    }
}
