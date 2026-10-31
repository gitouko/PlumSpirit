//! Semantic parameter editors for tool windows (v0.8 §182–§188): bars for
//! ordered values, an interval grid, rhythm cells, per-entry menus, colour
//! swatches for subchannels, a point canvas (paths, polygons, wells, magnets,
//! free nodes), the four-corner projective quad, a box, a 2D lattice, a mesh,
//! an octave of pitch-class keys, pitch ranges, track lists, positions on a
//! ruler, number sets, pairs, a colour — and the curve editor. Which one a
//! parameter gets is `plumspirit_engine::transform::editors::editor`. Comma
//! text stays reachable as "Edit Raw Values" (`raw` in the tool window).
//!
//! Interaction rules (all editors): a press acts at its press origin, hit
//! radii are in screen points ([`model::HIT`]), the editor takes keyboard
//! focus on a press so Delete removes the selected point / entry instead of
//! notes, and every change is returned at once (live Preview).

pub mod model;

use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};
use model::{Bars, Cells, Map, Order, Points};
use plumspirit_engine::transform::editors::{Editor, Labels, Shape, Unit};
use plumspirit_engine::transform::param::{MAX_CURVE_POINTS, MAX_OVERSHOOT};
use plumspirit_engine::transform::{Curve, Outside};

/// What the editors may show from the project.
#[derive(Clone, Copy, Default)]
pub struct EditorCtx<'a> {
    /// Swatch colour of each subchannel position of the active track.
    pub sub_colors: &'a [Color32],
    /// Track labels ("1 · Piano"), in project order.
    pub tracks: &'a [String],
    /// The project's key range (v0.9: pitch editors span 0–255 in 256-Key
    /// Mode).
    pub keys: plumspirit_engine::model::project::KeyRange,
}

/// Ephemeral interaction state kept between frames (per editor id).
#[derive(Clone, Copy, Debug, Default)]
struct State {
    selected: Option<usize>,
    dragging: Option<usize>,
    last: Option<usize>,
    paint: Option<bool>,
    /// Press origin of a point grab and whether it passed the drag
    /// threshold ([`model::DRAG_THRESHOLD`]).
    origin: Option<(f32, f32)>,
    armed: bool,
}

fn load(ui: &Ui, id: egui::Id) -> State {
    ui.data(|d| d.get_temp::<State>(id)).unwrap_or_default()
}

fn store(ui: &Ui, id: egui::Id, s: State) {
    ui.data_mut(|d| d.insert_temp(id, s));
}

/// Pointer events of one frame for a canvas.
#[derive(Default)]
struct Ptr {
    /// Primary press that started on the canvas (at the press origin).
    press: Option<Pos2>,
    /// Pointer position while the primary button is held.
    drag: Option<Pos2>,
    released: bool,
    double: Option<Pos2>,
    secondary: Option<Pos2>,
    delete: bool,
}

fn pointer(ui: &Ui, resp: &Response) -> Ptr {
    let mut p = Ptr::default();
    let (pressed, origin, down, released, latest) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.press_origin(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.latest_pos(),
        )
    });
    if pressed
        && let Some(o) = origin
        && resp.rect.contains(o)
        && resp.hovered()
    {
        p.press = Some(o);
        resp.request_focus();
    }
    if down && (resp.is_pointer_button_down_on() || p.press.is_some()) {
        p.drag = latest;
    }
    p.released = released;
    if resp.double_clicked() {
        p.double = resp.interact_pointer_pos();
    }
    if resp.secondary_clicked() {
        p.secondary = resp.interact_pointer_pos();
    }
    if resp.has_focus() {
        p.delete = ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
        });
    }
    p
}

fn map_of(r: Rect, x: (f64, f64), y: (f64, f64)) -> Map {
    Map {
        left: r.left(),
        top: r.top(),
        right: r.right(),
        bottom: r.bottom(),
        x,
        y,
    }
}

fn pos(m: &Map, x: f64, y: f64) -> Pos2 {
    let (a, b) = m.to_screen(x, y);
    Pos2::new(a, b)
}

fn canvas(ui: &mut Ui, size: Vec2) -> (Rect, Response, egui::Painter) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(6.0));
    let vis = ui.visuals();
    painter.rect_filled(rect, 0.0, vis.extreme_bg_color);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, vis.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    (rect, resp, painter)
}

/// A plot with draggable points (v0.8, Wave Q fix): the pointer is sensed
/// over the whole allocated area while values map into it inset by the hit
/// radius, so a point drawn on the plot's border — a value at an end of its
/// range — can be grabbed anywhere within [`model::HIT`] of its centre (with
/// [`canvas`] its outer half lay outside the sensed area and a press on the
/// point's centre did nothing). Returns the plot rectangle (map values into
/// it), the response over the whole area and a painter for both.
fn point_canvas(ui: &mut Ui, size: Vec2) -> (Rect, Response, egui::Painter) {
    let (outer, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let rect = outer.shrink(model::HIT);
    let painter = ui.painter_at(outer.expand(2.0));
    let vis = ui.visuals();
    painter.rect_filled(outer, 0.0, vis.extreme_bg_color);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, vis.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Middle,
    );
    (rect, resp, painter)
}

fn grid_stroke(ui: &Ui) -> Stroke {
    Stroke::new(
        1.0,
        ui.visuals()
            .widgets
            .noninteractive
            .bg_stroke
            .color
            .gamma_multiply(0.55),
    )
}

fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.stroke.color
}

fn width(ui: &Ui) -> f32 {
    ui.available_width().clamp(200.0, 340.0)
}

/// Draws the editor for list parameter values `cur`; returns the new list
/// when it changed this frame.
#[allow(clippy::too_many_arguments)]
pub fn list_editor(
    ui: &mut Ui,
    id: egui::Id,
    editor: Editor,
    cur: &[i64],
    min: i64,
    max: i64,
    max_len: usize,
    ctx: &EditorCtx,
) -> Option<Vec<i64>> {
    let mut v = cur.to_vec();
    let changed = match editor {
        Editor::Sequence(l) => bars(ui, id, &mut v, min, max, max_len, l, BarKind::Plain, ctx),
        Editor::Intervals(l) => bars(
            ui,
            id,
            &mut v,
            min,
            max,
            max_len,
            l,
            BarKind::Intervals,
            ctx,
        ),
        Editor::Bezier => bars(
            ui,
            id,
            &mut v,
            min,
            max,
            4,
            Labels::Index,
            BarKind::Bezier,
            ctx,
        ),
        Editor::Subchannels { set: false } => bars(
            ui,
            id,
            &mut v,
            min,
            max,
            max_len,
            Labels::Index,
            BarKind::Palette,
            ctx,
        ),
        Editor::Subchannels { set: true } => sub_set(ui, &mut v, max, max_len, ctx),
        Editor::Steps(l) => steps(ui, id, &mut v, max_len, l),
        Editor::Choices(c, l) => choices(ui, id, &mut v, c, max_len, l),
        Editor::Points { per, shape } => {
            points(ui, id, &mut v, usize::from(per), shape, min, max, max_len)
        }
        Editor::PitchPoint => pitch_point(ui, id, &mut v, ctx.keys.max_pitch()),
        Editor::Quad { offsets } => quad(ui, id, &mut v, offsets, min, max),
        Editor::Rect => rect_editor(ui, id, &mut v, min, max),
        Editor::Lattice { angle } => lattice(ui, id, &mut v, angle, min, max, max_len),
        Editor::Mesh => mesh(ui, id, &mut v, min, max),
        Editor::PitchClasses => pitch_classes(ui, &mut v),
        Editor::PitchClassMap => pc_map(ui, id, &mut v, max_len),
        Editor::Ranges(l) => ranges(ui, id, &mut v, min, max, max_len, l, ctx),
        Editor::Tracks => tracks(ui, &mut v, ctx),
        Editor::Positions(u) => positions(ui, id, &mut v, u, min, max, max_len),
        Editor::NumberSet => number_set(ui, &mut v, min, max),
        Editor::IndexPairs => index_pairs(ui, id, &mut v, min, max, max_len),
        Editor::Rgb => rgb(ui, &mut v),
        Editor::Auto => false,
    };
    changed.then_some(v)
}

// ------------------------------------------------------------------ bars

#[derive(Clone, Copy, PartialEq, Eq)]
enum BarKind {
    Plain,
    Intervals,
    Palette,
    Bezier,
}

/// The value range the bars show: the whole parameter range when it is
/// small, otherwise the values with headroom (dragging past the top grows it).
fn bar_view(vals: &[i64], min: i64, max: i64, kind: BarKind, subs: usize) -> (i64, i64) {
    if kind == BarKind::Palette {
        let top = vals
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .max(subs as i64 - 1)
            .max(3)
            + 2;
        return (min.max(0), top.min(max));
    }
    if max - min <= 400 {
        return (min, max);
    }
    let lo = vals.iter().copied().min().unwrap_or(0).min(0);
    let hi = vals.iter().copied().max().unwrap_or(0).max(0);
    let pad = ((hi - lo) / 2).max(8);
    (
        (lo - if lo < 0 { pad } else { 0 }).max(min),
        (hi + pad).min(max),
    )
}

const INTERVALS: [&str; 12] = [
    "unison", "m2", "M2", "m3", "M3", "P4", "tritone", "P5", "m6", "M6", "m7", "M7",
];

fn interval_name(v: i64) -> String {
    let o = v.abs() / 12;
    let n = INTERVALS[(v.abs() % 12) as usize];
    let sign = if v < 0 { "−" } else { "+" };
    if o == 0 {
        format!("{sign}{} ({n})", v.abs())
    } else {
        format!("{sign}{} ({n} + {o} oct)", v.abs())
    }
}

fn entry_color(ctx: &EditorCtx, pos: i64) -> Option<Color32> {
    usize::try_from(pos)
        .ok()
        .and_then(|p| ctx.sub_colors.get(p).copied())
}

#[allow(clippy::too_many_arguments)]
fn bars(
    ui: &mut Ui,
    id: egui::Id,
    v: &mut Vec<i64>,
    min: i64,
    max: i64,
    max_len: usize,
    labels: Labels,
    kind: BarKind,
    ctx: &EditorCtx,
) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    let (lo, hi) = bar_view(v, min, max, kind, ctx.sub_colors.len());
    ui.vertical(|ui| {
        let (rect, resp, p) = canvas(ui, Vec2::new(width(ui), 84.0));
        let m = map_of(rect, (0.0, 1.0), (lo as f64 - 0.5, hi as f64 + 0.5));
        let mut b = Bars::new(v.clone(), min.max(lo), max.min(hi), max_len);
        b.selected = st.selected.filter(|&s| s < v.len());
        b.last = st.last;
        let ev = pointer(ui, &resp);
        if let Some(o) = ev.press {
            changed |= b.press(&m, o.x, o.y);
        } else if let Some(q) = ev.drag
            && b.last.is_some()
        {
            changed |= b.drag_to(&m, q.x, q.y);
        }
        if ev.released {
            b.release();
        }
        if ev.delete && b.selected.is_some() && v.len() > 1 {
            let i = b.selected.unwrap_or(0);
            b.vals.remove(i);
            b.selected = None;
            changed = true;
        }
        // Grid.
        let g = grid_stroke(ui);
        if lo < 0 && hi > 0 {
            let y = pos(&m, 0.0, 0.0).y;
            p.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.2, g.color.gamma_multiply(1.6)),
            );
        }
        if kind == BarKind::Intervals {
            let mut s = (lo / 12) * 12;
            while s <= hi {
                if s != 0 {
                    let y = pos(&m, 0.0, s as f64).y;
                    p.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], g);
                }
                s += 12;
            }
        }
        let n = b.vals.len().max(1);
        let w = rect.width() / n as f32;
        let base = 0i64.clamp(lo, hi) as f64;
        let hover = resp.hover_pos();
        let mut tip = None;
        for (i, &val) in b.vals.iter().enumerate() {
            let x0 = rect.left() + w * i as f32;
            let r = Rect::from_x_y_ranges(
                (x0 + 1.0)..=(x0 + w - 1.0).max(x0 + 1.5),
                pos(&m, 0.0, val.max(lo).min(hi) as f64)
                    .y
                    .min(pos(&m, 0.0, base).y)
                    ..=pos(&m, 0.0, val.max(lo).min(hi) as f64)
                        .y
                        .max(pos(&m, 0.0, base).y),
            );
            let fill = match kind {
                BarKind::Palette => entry_color(ctx, val).unwrap_or(accent(ui).gamma_multiply(0.6)),
                _ if labels == Labels::Subchannel => {
                    entry_color(ctx, i as i64).unwrap_or(accent(ui).gamma_multiply(0.6))
                }
                _ => accent(ui).gamma_multiply(0.6),
            };
            p.rect_filled(r.expand2(Vec2::new(0.0, 0.5)), 0.0, fill);
            if b.selected == Some(i) {
                p.rect_stroke(
                    r.expand(1.0),
                    0.0,
                    Stroke::new(1.5, ui.visuals().text_color()),
                    egui::StrokeKind::Outside,
                );
            }
            if n <= 16 && w >= 14.0 {
                let t = match (kind, labels) {
                    (BarKind::Palette, _) => format!("{}", val + 1),
                    (_, Labels::PitchClass | Labels::Move | Labels::Motion) => labels.label(i),
                    _ => format!("{}", i + 1),
                };
                p.text(
                    Pos2::new(x0 + w / 2.0, rect.bottom() - 2.0),
                    egui::Align2::CENTER_BOTTOM,
                    t,
                    egui::FontId::proportional(9.0),
                    ui.visuals().weak_text_color(),
                );
            }
            if let Some(h) = hover
                && h.x >= x0
                && h.x < x0 + w
            {
                tip = Some(match kind {
                    BarKind::Intervals => format!("{}: {}", labels.label(i), interval_name(val)),
                    BarKind::Palette => format!("{}: subchannel {}", labels.label(i), val + 1),
                    _ => format!("{}: {val}", labels.label(i)),
                });
            }
        }
        if kind == BarKind::Bezier && b.vals.len() == 4 {
            let c = b.vals.iter().map(|&x| x as f64).collect::<Vec<_>>();
            let pts: Vec<Pos2> = (0..=40)
                .map(|k| {
                    let t = k as f64 / 40.0;
                    let u = 1.0 - t;
                    let y = u * u * u * c[0]
                        + 3.0 * u * u * t * c[1]
                        + 3.0 * u * t * t * c[2]
                        + t * t * t * c[3];
                    Pos2::new(rect.left() + rect.width() * t as f32, pos(&m, 0.0, y).y)
                })
                .collect();
            p.add(egui::Shape::line(
                pts,
                Stroke::new(1.5, ui.visuals().text_color()),
            ));
        }
        let resp = match tip {
            Some(t) => resp.on_hover_text(t),
            None => resp.on_hover_text("Drag to draw values · click a bar to select it"),
        };
        let _ = resp;
        ui.horizontal(|ui| {
            if kind != BarKind::Bezier {
                if ui
                    .small_button("−")
                    .on_hover_text("Remove the last entry")
                    .clicked()
                    && b.vals.len() > 1
                {
                    changed |= b.pop();
                }
                if ui.small_button("+").on_hover_text("Add an entry").clicked() {
                    let last = b.vals.last().copied().unwrap_or(0);
                    let nv = if kind == BarKind::Palette {
                        last + 1
                    } else {
                        last
                    };
                    changed |= b.push(nv);
                }
                ui.label(
                    egui::RichText::new(format!("{} entries", b.vals.len()))
                        .weak()
                        .small(),
                );
            }
            if let Some(i) = b.selected.filter(|&i| i < b.vals.len()) {
                ui.label(labels.label(i));
                let mut x = b.vals[i];
                let disp = if kind == BarKind::Palette { 1 } else { 0 };
                let mut shown = x + disp;
                if ui
                    .add(
                        crate::ui::stepper::Stepper::new(&mut shown)
                            .range((min + disp)..=(max + disp)),
                    )
                    .changed()
                {
                    x = shown - disp;
                    changed |= b.set_selected(x);
                }
            }
        });
        st.selected = b.selected;
        st.last = b.last;
        *v = b.vals;
    });
    store(ui, id, st);
    changed
}

// ------------------------------------------------------------------ cells

fn steps(ui: &mut Ui, id: egui::Id, v: &mut Vec<i64>, max_len: usize, labels: Labels) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    ui.vertical(|ui| {
        let n = v.len().max(1);
        let cw = (width(ui) / n as f32).clamp(10.0, 26.0);
        let (rect, resp, p) = canvas(ui, Vec2::new(cw * n as f32, 26.0));
        let m = map_of(rect, (0.0, 1.0), (0.0, 1.0));
        let mut c = Cells {
            on: v.iter().map(|&x| x != 0).collect(),
            paint: st.paint,
        };
        let ev = pointer(ui, &resp);
        if let Some(o) = ev.press {
            changed |= c.press(&m, o.x);
        } else if let Some(q) = ev.drag {
            changed |= c.drag_to(&m, q.x);
        }
        if ev.released {
            c.release();
        }
        for (i, &on) in c.on.iter().enumerate() {
            let r = Rect::from_min_size(
                Pos2::new(rect.left() + cw * i as f32 + 1.5, rect.top() + 3.0),
                Vec2::new(cw - 3.0, rect.height() - 6.0),
            );
            if on {
                p.rect_filled(r, 0.0, accent(ui));
            } else {
                p.rect_stroke(r, 0.0, grid_stroke(ui), egui::StrokeKind::Inside);
            }
        }
        let hov = resp.hover_pos().and_then(|h| c.index_at(&m, h.x)).map(|i| {
            format!(
                "{}: {}",
                labels.label(i),
                if c.on[i] { "on" } else { "off" }
            )
        });
        resp.on_hover_text(hov.unwrap_or_else(|| "Click to switch a cell · drag to paint".into()));
        ui.horizontal(|ui| {
            if ui.small_button("−").clicked() && c.on.len() > 1 {
                c.on.pop();
                changed = true;
            }
            if ui.small_button("+").clicked() && c.on.len() < max_len {
                c.on.push(false);
                changed = true;
            }
            ui.label(
                egui::RichText::new(format!("{} cells", c.on.len()))
                    .weak()
                    .small(),
            );
        });
        st.paint = c.paint;
        *v = c.on.iter().map(|&b| i64::from(b)).collect();
    });
    store(ui, id, st);
    changed
}

fn choices(
    ui: &mut Ui,
    id: egui::Id,
    v: &mut Vec<i64>,
    names: &[&str],
    max_len: usize,
    labels: Labels,
) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            let mut remove = None;
            for (i, x) in v.iter_mut().enumerate().take(64) {
                let mut c = (*x).clamp(0, names.len() as i64 - 1) as usize;
                let before = c;
                ui.label(egui::RichText::new(labels.label(i)).weak().small());
                egui::ComboBox::from_id_salt(id.with(i))
                    .selected_text(names[c])
                    .width(88.0)
                    .show_ui(ui, |ui| {
                        for (k, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut c, k, *n);
                        }
                    });
                if c != before {
                    *x = c as i64;
                    changed = true;
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Remove this entry")
                    .clicked()
                {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove.filter(|_| v.len() > 1) {
                v.remove(i);
                changed = true;
            }
        });
        if ui.small_button("+ entry").clicked() && v.len() < max_len {
            v.push(v.last().copied().unwrap_or(0));
            changed = true;
        }
    });
    changed
}

/// A set of subchannel positions as swatches (sorted, unique).
fn sub_set(ui: &mut Ui, v: &mut Vec<i64>, max: i64, max_len: usize, ctx: &EditorCtx) -> bool {
    let mut changed = false;
    let shown = (v.iter().copied().max().unwrap_or(0) + 2)
        .max(ctx.sub_colors.len() as i64)
        .max(4)
        .min(max + 1)
        .min(64);
    ui.horizontal_wrapped(|ui| {
        for pos in 0..shown {
            let on = v.contains(&pos);
            let col = entry_color(ctx, pos).unwrap_or(ui.visuals().widgets.inactive.bg_fill);
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(
                r.shrink(2.0),
                0.0,
                if on { col } else { col.gamma_multiply(0.25) },
            );
            if on {
                p.rect_stroke(
                    r.shrink(1.0),
                    0.0,
                    Stroke::new(1.5, ui.visuals().text_color()),
                    egui::StrokeKind::Inside,
                );
            }
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                format!("{}", pos + 1),
                egui::FontId::proportional(10.0),
                ui.visuals().strong_text_color(),
            );
            if resp
                .on_hover_text(format!(
                    "Subchannel {} ({})",
                    pos + 1,
                    if on { "included" } else { "not included" }
                ))
                .clicked()
            {
                if on {
                    v.retain(|&x| x != pos);
                } else if v.len() < max_len {
                    v.push(pos);
                    v.sort_unstable();
                }
                changed = true;
            }
        }
    });
    changed
}

// ------------------------------------------------------------------ points

/// The value box shown for point lists: the parameter range clipped to a
/// margin around the selection box (0 … 1000 ‰).
fn point_view(min: i64, max: i64) -> (f64, f64) {
    ((min as f64).max(-600.0), (max as f64).min(1600.0))
}

fn draw_box(ui: &Ui, p: &egui::Painter, m: &Map) {
    let a = pos(m, 0.0, 0.0);
    let b = pos(m, 1000.0, 1000.0);
    let r = Rect::from_two_pos(a, b);
    p.rect_stroke(
        r,
        0.0,
        Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Middle,
    );
    let g = grid_stroke(ui);
    for k in 1..4 {
        let t = k as f64 * 250.0;
        p.line_segment([pos(m, t, 0.0), pos(m, t, 1000.0)], g);
        p.line_segment([pos(m, 0.0, t), pos(m, 1000.0, t)], g);
    }
}

fn handle(ui: &Ui, p: &egui::Painter, at: Pos2, selected: bool, label: Option<&str>) {
    let c = if selected {
        accent(ui)
    } else {
        Color32::TRANSPARENT
    };
    p.circle(
        at,
        if selected { 5.0 } else { 4.0 },
        c,
        Stroke::new(1.3, ui.visuals().text_color()),
    );
    if let Some(l) = label {
        p.text(
            at + Vec2::new(6.0, -6.0),
            egui::Align2::LEFT_BOTTOM,
            l,
            egui::FontId::proportional(9.0),
            ui.visuals().text_color(),
        );
    }
}

/// Runs the press / drag / release / add / delete cycle of a point model.
fn drive(ui: &Ui, resp: &Response, m: &Map, pts: &mut Points, st: &mut State) -> bool {
    pts.selected = st.selected.filter(|&s| s < pts.pts.len());
    pts.dragging = st.dragging.filter(|&s| s < pts.pts.len());
    pts.origin = st.origin;
    pts.armed = st.armed;
    let ev = pointer(ui, resp);
    let mut changed = false;
    if let Some(o) = ev.press {
        pts.press(m, o.x, o.y);
    } else if let Some(q) = ev.drag {
        changed |= pts.drag_to(m, q.x, q.y);
    }
    if ev.released {
        pts.release();
    }
    if let Some(d) = ev.double {
        changed |= pts.add_at(m, d.x, d.y);
    }
    if let Some(s) = ev.secondary
        && let Some(i) = pts.hit(m, s.x, s.y)
    {
        pts.selected = Some(i);
        changed |= pts.delete_selected();
    }
    if ev.delete {
        changed |= pts.delete_selected();
    }
    // Keyboard focus follows the selection: Delete then removes the point,
    // never notes; with nothing selected the app's shortcuts work again. A
    // field that holds the focus (typing a point's x / y) keeps it.
    if pts.selected.is_some() {
        if !resp.has_focus() && ui.memory(|m| m.focused().is_none()) {
            resp.request_focus();
        }
    } else if resp.has_focus() {
        resp.surrender_focus();
    }
    st.selected = pts.selected;
    st.dragging = pts.dragging;
    st.origin = pts.origin;
    st.armed = pts.armed;
    changed
}

#[allow(clippy::too_many_arguments)]
fn points(
    ui: &mut Ui,
    id: egui::Id,
    v: &mut Vec<i64>,
    per: usize,
    shape: Shape,
    min: i64,
    max: i64,
    max_len: usize,
) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    ui.vertical(|ui| {
        let w = width(ui).min(260.0);
        let (rect, resp, p) = point_canvas(ui, Vec2::new(w, w * 0.62));
        let view = point_view(min, max);
        let m = map_of(rect, view, view);
        draw_box(ui, &p, &m);
        let mut pts = Points::new(model::pairs(v, per), Order::Free, (min as f64, max as f64), (min as f64, max as f64));
        pts.max_points = max_len / per;
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            model::write_pairs(v, &pts.pts, per);
            changed = true;
        }
        let line = Stroke::new(1.4, accent(ui));
        let sp: Vec<Pos2> = pts.pts.iter().map(|&(x, y)| pos(&m, x, y)).collect();
        match shape {
            Shape::Path if sp.len() > 1 => {
                p.add(egui::Shape::line(sp.clone(), line));
            }
            Shape::Polygon if sp.len() > 2 => {
                p.add(egui::Shape::closed_line(sp.clone(), line));
            }
            _ => {}
        }
        for (k, &q) in sp.iter().enumerate() {
            // Extra values: a push vector (x, y) or a direction (°).
            if per == 4 {
                let (dx, dy) = (v[k * 4 + 2] as f64, v[k * 4 + 3] as f64);
                let (x, y) = pts.pts[k];
                p.arrow(q, pos(&m, x + dx, y + dy) - q, Stroke::new(1.2, ui.visuals().text_color()));
            } else if per == 3 {
                let a = (v[k * 3 + 2] as f64).to_radians();
                p.arrow(q, Vec2::new(a.cos() as f32, -a.sin() as f32) * 18.0, Stroke::new(1.2, ui.visuals().text_color()));
            }
            handle(ui, &p, q, pts.selected == Some(k), Some(&format!("{}", k + 1)));
        }
        resp.on_hover_text("Drag points · double-click to add · right-click or Delete to remove · the frame is the selection (0 … 1000 ‰)");
        ui.horizontal(|ui| {
            if ui.small_button("+ point").clicked() && pts.add_middle() {
                model::write_pairs(v, &pts.pts, per);
                changed = true;
            }
            if let Some(k) = pts.selected.filter(|&k| k < pts.pts.len()) {
                if ui.small_button("− point").clicked() && pts.delete_selected() {
                    model::write_pairs(v, &pts.pts, per);
                    changed = true;
                } else {
                    ui.label(format!("Point {}", k + 1));
                    let names = ["x", "y", if per == 3 { "°" } else { "push x" }, "push y"];
                    for e in 0..per {
                        let mut x = v[k * per + e];
                        ui.label(egui::RichText::new(names[e]).weak());
                        if ui.add(crate::ui::stepper::Stepper::new(&mut x).range(min..=max)).changed() {
                            v[k * per + e] = x;
                            changed = true;
                        }
                    }
                }
            }
            st.selected = pts.selected;
        });
    });
    store(ui, id, st);
    changed
}

/// The pivot editor (time ‰, pitch) over the project's keys 0–`top`.
fn pitch_point(ui: &mut Ui, id: egui::Id, v: &mut Vec<i64>, top: u8) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    v.resize(2, 0);
    let topf = f64::from(top);
    ui.vertical(|ui| {
        let (rect, resp, p) = point_canvas(ui, Vec2::new(width(ui).min(240.0), 110.0));
        let m = map_of(rect, (0.0, 1000.0), (0.0, topf));
        let g = grid_stroke(ui);
        for o in 1..=(i32::from(top) / 12) {
            let y = pos(&m, 0.0, f64::from(o * 12)).y;
            p.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], g);
        }
        let mut pts = Points::new(
            vec![(v[0] as f64, v[1] as f64)],
            Order::Free,
            (0.0, 1000.0),
            (0.0, topf),
        );
        pts.fixed = true;
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            v[0] = pts.pts[0].0.round() as i64;
            v[1] = pts.pts[0].1.round() as i64;
            changed = true;
        }
        handle(ui, &p, pos(&m, v[0] as f64, v[1] as f64), true, None);
        resp.on_hover_text("Drag the pivot: across = time (‰ of the selection), up = pitch");
        ui.horizontal(|ui| {
            ui.label("x ‰");
            changed |= ui
                .add(crate::ui::stepper::Stepper::new(&mut v[0]).range(0..=1000))
                .changed();
            ui.label("pitch");
            changed |= ui
                .add(
                    crate::ui::stepper::Stepper::new(&mut v[1])
                        .range(0..=i64::from(top))
                        .custom_formatter(|n, _| {
                            plumspirit_engine::model::note::pitch_name(n.clamp(0.0, topf) as u8)
                        }),
                )
                .changed();
        });
    });
    store(ui, id, st);
    changed
}

const CORNERS: [&str; 4] = ["BL", "BR", "TR", "TL"];
const BOX: [(f64, f64); 4] = [(0.0, 0.0), (1000.0, 0.0), (1000.0, 1000.0), (0.0, 1000.0)];

/// The four-corner projective editor (§187): drag BL / BR / TR / TL, or
/// type them.
fn quad(ui: &mut Ui, id: egui::Id, v: &mut Vec<i64>, offsets: bool, min: i64, max: i64) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    v.resize(8, 0);
    ui.vertical(|ui| {
        let w = width(ui).min(240.0);
        let (rect, resp, p) = canvas(ui, Vec2::new(w, w * 0.75));
        let m = map_of(rect, (-500.0, 1500.0), (-500.0, 1500.0));
        draw_box(ui, &p, &m);
        let abs: Vec<(f64, f64)> = (0..4)
            .map(|k| {
                let (x, y) = (v[k * 2] as f64, v[k * 2 + 1] as f64);
                if offsets {
                    (BOX[k].0 + x, BOX[k].1 + y)
                } else {
                    (x, y)
                }
            })
            .collect();
        let (lo, hi) = (min as f64, max as f64);
        let bounds = if offsets {
            // Offsets are bounded per corner; the canvas clamps the rest.
            (-500.0f64.max(lo), 1500.0f64.min(hi + 1000.0))
        } else {
            (lo.max(-500.0), hi.min(1500.0))
        };
        let mut pts = Points::new(abs, Order::Free, bounds, bounds);
        pts.fixed = true;
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            for k in 0..4 {
                let (x, y) = pts.pts[k];
                let (x, y) = if offsets {
                    (x - BOX[k].0, y - BOX[k].1)
                } else {
                    (x, y)
                };
                v[k * 2] = (x.round() as i64).clamp(min, max);
                v[k * 2 + 1] = (y.round() as i64).clamp(min, max);
            }
            changed = true;
        }
        let sp: Vec<Pos2> = pts.pts.iter().map(|&(x, y)| pos(&m, x, y)).collect();
        p.add(egui::Shape::convex_polygon(
            sp.clone(),
            accent(ui).gamma_multiply(0.18),
            Stroke::new(1.5, accent(ui)),
        ));
        for (k, &q) in sp.iter().enumerate() {
            handle(ui, &p, q, pts.selected == Some(k), Some(CORNERS[k]));
        }
        resp.on_hover_text("Drag a corner (BL, BR, TR, TL) · the frame is the selection");
        egui::Grid::new(id.with("n"))
            .num_columns(5)
            .spacing([4.0, 2.0])
            .show(ui, |ui| {
                for k in 0..4 {
                    ui.label(CORNERS[k]);
                    for e in 0..2 {
                        ui.label(egui::RichText::new(if e == 0 { "x" } else { "y" }).weak());
                        if ui
                            .add(
                                crate::ui::stepper::Stepper::new(&mut v[k * 2 + e])
                                    .range(min..=max),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    }
                    ui.end_row();
                }
            });
    });
    store(ui, id, st);
    changed
}

fn rect_editor(ui: &mut Ui, id: egui::Id, v: &mut Vec<i64>, min: i64, max: i64) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    v.resize(4, 0);
    ui.vertical(|ui| {
        let w = width(ui).min(220.0);
        let (rect, resp, p) = point_canvas(ui, Vec2::new(w, w * 0.62));
        let m = map_of(rect, (min as f64, max as f64), (min as f64, max as f64));
        draw_box(ui, &p, &m);
        let mut pts = Points::new(
            vec![(v[0] as f64, v[1] as f64), (v[2] as f64, v[3] as f64)],
            Order::Free,
            (min as f64, max as f64),
            (min as f64, max as f64),
        );
        pts.fixed = true;
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            v[0] = pts.pts[0].0.round() as i64;
            v[1] = pts.pts[0].1.round() as i64;
            v[2] = pts.pts[1].0.round() as i64;
            v[3] = pts.pts[1].1.round() as i64;
            changed = true;
        }
        let r = Rect::from_two_pos(
            pos(&m, v[0] as f64, v[1] as f64),
            pos(&m, v[2] as f64, v[3] as f64),
        );
        p.rect(
            r,
            0.0,
            accent(ui).gamma_multiply(0.18),
            Stroke::new(1.5, accent(ui)),
            egui::StrokeKind::Middle,
        );
        handle(
            ui,
            &p,
            pos(&m, v[0] as f64, v[1] as f64),
            st.selected == Some(0),
            Some("1"),
        );
        handle(
            ui,
            &p,
            pos(&m, v[2] as f64, v[3] as f64),
            st.selected == Some(1),
            Some("2"),
        );
        resp.on_hover_text("Drag the two corners of the box (‰ of the selection)");
        ui.horizontal(|ui| {
            for (k, n) in ["x0", "y0", "x1", "y1"].iter().enumerate() {
                ui.label(egui::RichText::new(*n).weak());
                changed |= ui
                    .add(crate::ui::stepper::Stepper::new(&mut v[k]).range(min..=max))
                    .changed();
            }
        });
    });
    store(ui, id, st);
    changed
}

/// Node offsets of a square mesh (row by row from the bottom).
fn mesh(ui: &mut Ui, id: egui::Id, v: &mut Vec<i64>, min: i64, max: i64) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    let s = (((v.len() / 2) as f64).sqrt() as usize).max(2);
    v.resize(s * s * 2, 0);
    let base = |k: usize| -> (f64, f64) {
        let (i, j) = (k % s, k / s);
        (
            i as f64 * 1000.0 / (s - 1) as f64,
            j as f64 * 1000.0 / (s - 1) as f64,
        )
    };
    ui.vertical(|ui| {
        let w = width(ui).min(240.0);
        let (rect, resp, p) = canvas(ui, Vec2::new(w, w * 0.75));
        let m = map_of(rect, (-400.0, 1400.0), (-400.0, 1400.0));
        draw_box(ui, &p, &m);
        let abs: Vec<(f64, f64)> = (0..s * s)
            .map(|k| (base(k).0 + v[k * 2] as f64, base(k).1 + v[k * 2 + 1] as f64))
            .collect();
        let mut pts = Points::new(abs, Order::Free, (-1000.0, 2000.0), (-1000.0, 2000.0));
        pts.fixed = true;
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            for k in 0..s * s {
                v[k * 2] = ((pts.pts[k].0 - base(k).0).round() as i64).clamp(min, max);
                v[k * 2 + 1] = ((pts.pts[k].1 - base(k).1).round() as i64).clamp(min, max);
            }
            changed = true;
        }
        let sp: Vec<Pos2> = pts.pts.iter().map(|&(x, y)| pos(&m, x, y)).collect();
        let line = Stroke::new(1.2, accent(ui));
        for j in 0..s {
            for i in 0..s {
                let k = j * s + i;
                if i + 1 < s {
                    p.line_segment([sp[k], sp[k + 1]], line);
                }
                if j + 1 < s {
                    p.line_segment([sp[k], sp[k + s]], line);
                }
            }
        }
        for (k, &q) in sp.iter().enumerate() {
            handle(ui, &p, q, pts.selected == Some(k), None);
        }
        resp.on_hover_text("Drag mesh nodes (offsets in ‰ of the selection)");
        if let Some(k) = st.selected.filter(|&k| k < s * s) {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Node {} (row {}, column {})",
                    k + 1,
                    k / s + 1,
                    k % s + 1
                ));
                for e in 0..2 {
                    ui.label(egui::RichText::new(if e == 0 { "dx" } else { "dy" }).weak());
                    changed |= ui
                        .add(crate::ui::stepper::Stepper::new(&mut v[k * 2 + e]).range(min..=max))
                        .changed();
                }
            });
        }
    });
    store(ui, id, st);
    changed
}

/// Values on a square grid (bottom row first): press a cell to select it and
/// drag up / down to change it (angles: drag toward the direction).
fn lattice(
    ui: &mut Ui,
    id: egui::Id,
    v: &mut Vec<i64>,
    angle: bool,
    min: i64,
    max: i64,
    max_len: usize,
) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    let s = ((v.len() as f64).sqrt() as usize).max(1);
    v.resize(s * s, 0);
    ui.vertical(|ui| {
        let side = width(ui).min(200.0);
        let (rect, resp, p) = canvas(ui, Vec2::splat(side));
        let cell = side / s as f32;
        let at = |q: Pos2| -> Option<usize> {
            if !rect.contains(q) {
                return None;
            }
            let i = (((q.x - rect.left()) / cell) as usize).min(s - 1);
            let jr = (((q.y - rect.top()) / cell) as usize).min(s - 1);
            Some((s - 1 - jr) * s + i)
        };
        let center = |k: usize| -> Pos2 {
            let (i, j) = (k % s, k / s);
            Pos2::new(
                rect.left() + cell * (i as f32 + 0.5),
                rect.bottom() - cell * (j as f32 + 0.5),
            )
        };
        let ev = pointer(ui, &resp);
        if let Some(o) = ev.press {
            st.selected = at(o);
            st.dragging = st.selected;
            st.last = Some(o.y as usize);
        } else if let (Some(q), Some(k)) = (ev.drag, st.dragging) {
            let nv = if angle {
                let d = q - center(k);
                if d.length() > 4.0 {
                    (f64::from(-d.y).atan2(f64::from(d.x)).to_degrees().round() as i64)
                        .clamp(min, max)
                } else {
                    v[k]
                }
            } else {
                // 100 points of travel = the whole range (at most), from the
                // value at the press.
                let span = (max - min).min(400) as f32;
                let y0 = st.last.unwrap_or(q.y as usize) as f32;
                let steps = ((y0 - q.y) / 100.0 * span).round() as i64;
                (v[k] + steps).clamp(min, max)
            };
            if nv != v[k] {
                v[k] = nv;
                changed = true;
                if !angle {
                    st.last = Some(q.y as usize);
                }
            }
        }
        if ev.released {
            st.dragging = None;
        }
        let (lo, hi) = (
            v.iter().copied().min().unwrap_or(0).min(0),
            v.iter().copied().max().unwrap_or(1).max(1),
        );
        for (k, &val) in v.iter().enumerate() {
            let c = center(k);
            let r = Rect::from_center_size(c, Vec2::splat(cell - 2.0));
            if angle {
                p.rect_stroke(r, 0.0, grid_stroke(ui), egui::StrokeKind::Inside);
                let a = (val as f32).to_radians();
                p.arrow(
                    c - Vec2::new(a.cos(), -a.sin()) * cell * 0.3,
                    Vec2::new(a.cos(), -a.sin()) * cell * 0.6,
                    Stroke::new(1.4, accent(ui)),
                );
            } else {
                let t = (val - lo) as f32 / (hi - lo).max(1) as f32;
                p.rect_filled(r, 0.0, accent(ui).gamma_multiply(0.15 + 0.85 * t));
                if cell >= 22.0 {
                    p.text(
                        c,
                        egui::Align2::CENTER_CENTER,
                        format!("{}", val),
                        egui::FontId::proportional(9.0),
                        ui.visuals().strong_text_color(),
                    );
                }
            }
            if st.selected == Some(k) {
                p.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.6, ui.visuals().text_color()),
                    egui::StrokeKind::Outside,
                );
            }
        }
        resp.on_hover_text(if angle {
            "Press a cell and drag toward the direction it should point"
        } else {
            "Press a cell and drag up / down to change it (bottom row = low pitch, left = start)"
        });
        ui.horizontal(|ui| {
            let mut side_n = s;
            ui.label("Grid");
            let suffix = format!(" × {s}");
            let top = ((max_len as f64).sqrt() as usize).max(1);
            if ui
                .add(
                    crate::ui::stepper::Stepper::new(&mut side_n)
                        .range(1..=top)
                        .suffix(suffix),
                )
                .changed()
                && side_n != s
            {
                // Resample (nearest) to the new side.
                let old = v.clone();
                *v = (0..side_n * side_n)
                    .map(|k| {
                        let (i, j) = (k % side_n, k / side_n);
                        let oi = (i * s / side_n).min(s - 1);
                        let oj = (j * s / side_n).min(s - 1);
                        old[oj * s + oi]
                    })
                    .collect();
                st.selected = None;
                changed = true;
            }
            if let Some(k) = st.selected.filter(|&k| k < v.len()) {
                ui.label(format!("Cell row {}, column {}", k / s + 1, k % s + 1));
                changed |= ui
                    .add(crate::ui::stepper::Stepper::new(&mut v[k]).range(min..=max))
                    .changed();
            }
        });
    });
    store(ui, id, st);
    changed
}

// ------------------------------------------------------------------ pitch

const PC: [&str; 12] = [
    "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
];
const BLACK: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

fn pitch_classes(ui: &mut Ui, v: &mut Vec<i64>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for pc in 0..12i64 {
            let on = v.contains(&pc);
            let (r, resp) = ui.allocate_exact_size(Vec2::new(20.0, 30.0), Sense::click());
            let base = if BLACK[pc as usize] {
                Color32::from_gray(40)
            } else {
                Color32::from_gray(225)
            };
            let p = ui.painter();
            p.rect_filled(r, 0.0, base);
            if on {
                p.rect_filled(r.shrink(3.0), 0.0, accent(ui));
            }
            p.text(
                Pos2::new(r.center().x, r.bottom() - 3.0),
                egui::Align2::CENTER_BOTTOM,
                PC[pc as usize],
                egui::FontId::proportional(8.5),
                if BLACK[pc as usize] || on {
                    Color32::WHITE
                } else {
                    Color32::BLACK
                },
            );
            if resp
                .on_hover_text(format!(
                    "{} ({})",
                    PC[pc as usize],
                    if on { "on" } else { "off" }
                ))
                .clicked()
            {
                if on {
                    v.retain(|&x| x != pc);
                } else {
                    v.push(pc);
                    v.sort_unstable();
                }
                changed = true;
            }
        }
    });
    changed
}

fn pc_map(ui: &mut Ui, id: egui::Id, v: &mut Vec<i64>, max_len: usize) -> bool {
    let mut changed = false;
    if v.len() % 2 == 1 {
        v.pop();
    }
    ui.vertical(|ui| {
        let mut remove = None;
        for k in 0..v.len() / 2 {
            ui.horizontal(|ui| {
                for e in 0..2 {
                    let mut c = v[k * 2 + e].clamp(0, 11) as usize;
                    let before = c;
                    egui::ComboBox::from_id_salt(id.with((k, e)))
                        .selected_text(PC[c])
                        .width(46.0)
                        .show_ui(ui, |ui| {
                            for (i, n) in PC.iter().enumerate() {
                                ui.selectable_value(&mut c, i, *n);
                            }
                        });
                    if c != before {
                        v[k * 2 + e] = c as i64;
                        changed = true;
                    }
                    if e == 0 {
                        ui.label("→");
                    }
                }
                if ui.small_button("×").clicked() {
                    remove = Some(k);
                }
            });
        }
        if let Some(k) = remove {
            v.drain(k * 2..k * 2 + 2);
            changed = true;
        }
        if ui.small_button("+ pair").clicked() && v.len() + 2 <= max_len {
            v.extend([0, 0]);
            changed = true;
        }
    });
    changed
}

/// (low, high) pitch pairs as vertical ranges: drag the nearer end.
#[allow(clippy::too_many_arguments)]
fn ranges(
    ui: &mut Ui,
    id: egui::Id,
    v: &mut Vec<i64>,
    min: i64,
    max: i64,
    max_len: usize,
    labels: Labels,
    ctx: &EditorCtx,
) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    if v.len() % 2 == 1 {
        v.pop();
    }
    ui.vertical(|ui| {
        let n = (v.len() / 2).max(1);
        let lo = (v.iter().copied().min().unwrap_or(48) - 6).clamp(min, max) as f64;
        let hi = (v.iter().copied().max().unwrap_or(72) + 6).clamp(min, max) as f64;
        let (rect, resp, p) = canvas(
            ui,
            Vec2::new(width(ui).min(36.0 * n as f32 + 8.0).max(120.0), 110.0),
        );
        let m = map_of(rect, (0.0, n as f64), (lo, hi));
        let w = rect.width() / n as f32;
        let ev = pointer(ui, &resp);
        if let Some(o) = ev.press {
            let k = (((o.x - rect.left()) / w) as usize).min(n - 1);
            let (_, val) = m.to_value(o.x, o.y);
            let near_high = (val - v[k * 2 + 1] as f64).abs() < (val - v[k * 2] as f64).abs();
            st.selected = Some(k);
            st.dragging = Some(k * 2 + usize::from(near_high));
        } else if let (Some(q), Some(e)) = (ev.drag, st.dragging) {
            let (_, val) = m.to_value(q.x, q.y);
            let nv = (val.round() as i64).clamp(min, max);
            if v[e] != nv {
                v[e] = nv;
                changed = true;
            }
        }
        if ev.released {
            st.dragging = None;
        }
        for k in 0..v.len() / 2 {
            let (a, b) = (
                v[k * 2].min(v[k * 2 + 1]) as f64,
                v[k * 2].max(v[k * 2 + 1]) as f64,
            );
            let x0 = rect.left() + w * k as f32;
            let r = Rect::from_x_y_ranges(
                (x0 + 4.0)..=(x0 + w - 4.0),
                pos(&m, 0.0, b).y..=pos(&m, 0.0, a).y,
            );
            let fill = if labels == Labels::Subchannel {
                entry_color(ctx, k as i64)
            } else {
                None
            }
            .unwrap_or(accent(ui).gamma_multiply(0.6));
            p.rect_filled(r, 0.0, fill);
            if st.selected == Some(k) {
                p.rect_stroke(
                    r.expand(1.0),
                    0.0,
                    Stroke::new(1.5, ui.visuals().text_color()),
                    egui::StrokeKind::Outside,
                );
            }
        }
        resp.on_hover_text("Drag the top or bottom of a range");
        ui.horizontal(|ui| {
            if ui.small_button("−").clicked() && v.len() > 2 {
                v.truncate(v.len() - 2);
                changed = true;
            }
            if ui.small_button("+").clicked() && v.len() + 2 <= max_len {
                let (a, b) = (
                    v.get(v.len().wrapping_sub(2)).copied().unwrap_or(48),
                    v.last().copied().unwrap_or(72),
                );
                v.extend([a, b]);
                changed = true;
            }
            if let Some(k) = st.selected.filter(|&k| k * 2 + 1 < v.len()) {
                ui.label(labels.label(k));
                for e in 0..2 {
                    changed |= ui
                        .add(
                            crate::ui::stepper::Stepper::new(&mut v[k * 2 + e])
                                .range(min..=max)
                                .custom_formatter(|n, _| {
                                    plumspirit_engine::model::note::pitch_name(
                                        n.clamp(0.0, f64::from(ctx.keys.max_pitch())) as u8,
                                    )
                                }),
                        )
                        .changed();
                }
            }
        });
    });
    store(ui, id, st);
    changed
}

// ------------------------------------------------------------------ misc

fn tracks(ui: &mut Ui, v: &mut Vec<i64>, ctx: &EditorCtx) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        if ctx.tracks.is_empty() {
            ui.label(egui::RichText::new("No project open.").weak());
        }
        for (i, name) in ctx.tracks.iter().enumerate().take(256) {
            let n = i as i64 + 1;
            let mut on = v.contains(&n);
            if ui.checkbox(&mut on, name).changed() {
                if on {
                    v.push(n);
                    v.sort_unstable();
                } else {
                    v.retain(|&x| x != n);
                }
                changed = true;
            }
        }
    });
    changed
}

/// Positions on a ruler (ascending): drag markers, double-click to add,
/// right-click / Delete to remove.
fn positions(
    ui: &mut Ui,
    id: egui::Id,
    v: &mut Vec<i64>,
    unit: Unit,
    min: i64,
    max: i64,
    max_len: usize,
) -> bool {
    let mut st = load(ui, id);
    let mut changed = false;
    let vmax = v.iter().copied().max().unwrap_or(0);
    let (lo, hi, step) = match unit {
        Unit::Percent => (0, 100, 25),
        // The parameter's range (0–255 for a keyed pitch in 256-Key Mode).
        Unit::Pitch => (0, max.clamp(0, 255), 12),
        Unit::Beat48 => (0, (vmax + vmax / 4).max(192).min(max), 48),
        Unit::Ticks => (
            0,
            (vmax + vmax / 4).max(3840).min(max),
            (vmax.max(3840) / 8).max(1),
        ),
    };
    let (lo, hi) = (lo.max(min), hi.max(lo + 1));
    ui.vertical(|ui| {
        let (rect, resp, p) = point_canvas(ui, Vec2::new(width(ui), 34.0));
        let m = map_of(rect, (lo as f64, hi as f64), (0.0, 1.0));
        let g = grid_stroke(ui);
        let mut t = lo;
        while t <= hi {
            let x = pos(&m, t as f64, 0.0).x;
            p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], g);
            t += step.max(1);
        }
        let mut pts = Points::new(
            v.iter().map(|&x| (x as f64, 0.5)).collect(),
            Order::Free,
            (min as f64, max as f64),
            (0.5, 0.5),
        );
        pts.max_points = max_len;
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            let mut nv: Vec<i64> = pts.pts.iter().map(|q| q.0.round() as i64).collect();
            let sel = st.selected.map(|k| nv[k]);
            nv.sort_unstable();
            st.selected = sel.and_then(|x| nv.iter().position(|&y| y == x));
            st.dragging = st.dragging.and(st.selected);
            *v = nv;
            changed = true;
        }
        for (k, &x) in v.iter().enumerate() {
            let q = pos(&m, x as f64, 0.5);
            p.line_segment(
                [
                    Pos2::new(q.x, rect.top() + 3.0),
                    Pos2::new(q.x, rect.bottom() - 3.0),
                ],
                Stroke::new(2.0, accent(ui)),
            );
            handle(ui, &p, q, st.selected == Some(k), None);
        }
        let unit_name = match unit {
            Unit::Ticks => "ticks",
            Unit::Beat48 => "1/48 beat (grid = beats)",
            Unit::Percent => "% of the span",
            Unit::Pitch => "pitch (grid = octaves)",
        };
        resp.on_hover_text(format!(
            "Drag markers · double-click to add · right-click / Delete to remove — {unit_name}"
        ));
        ui.horizontal(|ui| {
            if let Some(k) = st.selected.filter(|&k| k < v.len()) {
                ui.label(format!("Marker {}", k + 1));
                if ui
                    .add(crate::ui::stepper::Stepper::new(&mut v[k]).range(min..=max))
                    .changed()
                {
                    v.sort_unstable();
                    changed = true;
                }
            } else {
                ui.label(
                    egui::RichText::new(format!("{} markers", v.len()))
                        .weak()
                        .small(),
                );
            }
        });
    });
    store(ui, id, st);
    changed
}

fn number_set(ui: &mut Ui, v: &mut Vec<i64>, min: i64, max: i64) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for n in min..=max.min(min + 128) {
            let mut on = v.contains(&n);
            if ui.toggle_value(&mut on, format!("{n}")).changed() {
                if on {
                    v.push(n);
                    v.sort_unstable();
                } else {
                    v.retain(|&x| x != n);
                }
                changed = true;
            }
        }
    });
    changed
}

fn index_pairs(
    ui: &mut Ui,
    _id: egui::Id,
    v: &mut Vec<i64>,
    min: i64,
    max: i64,
    max_len: usize,
) -> bool {
    let mut changed = false;
    if v.len() % 2 == 1 {
        v.pop();
    }
    ui.vertical(|ui| {
        let mut remove = None;
        for k in 0..v.len() / 2 {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(crate::ui::stepper::Stepper::new(&mut v[k * 2]).range(min..=max))
                    .changed();
                ui.label("↔");
                changed |= ui
                    .add(crate::ui::stepper::Stepper::new(&mut v[k * 2 + 1]).range(min..=max))
                    .changed();
                if ui.small_button("×").clicked() {
                    remove = Some(k);
                }
            });
        }
        if let Some(k) = remove {
            v.drain(k * 2..k * 2 + 2);
            changed = true;
        }
        if ui.small_button("+ pair").clicked() && v.len() + 2 <= max_len {
            v.extend([min, min + 1]);
            changed = true;
        }
    });
    changed
}

fn rgb(ui: &mut Ui, v: &mut Vec<i64>) -> bool {
    v.resize(3, 0);
    let mut c = [
        v[0].clamp(0, 255) as u8,
        v[1].clamp(0, 255) as u8,
        v[2].clamp(0, 255) as u8,
    ];
    if ui.color_edit_button_srgb(&mut c).changed() {
        *v = c.iter().map(|&x| i64::from(x)).collect();
        return true;
    }
    false
}

// ------------------------------------------------------------------ curve

/// The curve editor (v0.8 §183, v0.9 §103–§108): select, drag past a small
/// threshold, numeric X / Y for every point, add (double click or "+",
/// anywhere — before the first point or after the last makes a new end),
/// delete (right-click, Delete or "−", the ends too, down to two points),
/// Duplicate, Reset, Shift snaps to 0.05, Smoothness 0–100 % and what the
/// curve does outside its points. No point is pinned. Returns the new curve
/// when it changed.
#[cfg(test)]
pub fn curve_editor(ui: &mut Ui, id: egui::Id, c: &Curve, default: &Curve) -> Option<Curve> {
    curve_editor_opts(ui, id, c, default, true, false)
}

/// [`curve_editor`] for a graph that may overshoot its nominal 0 – 1 range
/// (B1-0113, `ParamSpec::overshoot`): an Overshoot control, the plot widened
/// to the effective range with the nominal band marked.
pub fn curve_editor_overshoot(
    ui: &mut Ui,
    id: egui::Id,
    c: &Curve,
    default: &Curve,
    overshoot: bool,
) -> Option<Curve> {
    curve_editor_opts(ui, id, c, default, true, overshoot)
}

/// Size of the curve editor's plot area (points).
pub const CURVE_CANVAS: Vec2 = Vec2::new(260.0, 110.0);

/// [`curve_editor`]; `full` false for straight, clamped curves (Spiderweb's
/// velocity envelope evaluates its points that way): no Smoothness or
/// outside policy controls.
pub fn curve_editor_with(
    ui: &mut Ui,
    id: egui::Id,
    c: &Curve,
    default: &Curve,
    full: bool,
) -> Option<Curve> {
    curve_editor_opts(ui, id, c, default, full, false)
}

/// B1-0112: each point's own smoothness after the points changed: a point
/// that kept its exact place keeps its value, a new one follows the graph;
/// a drag (same count) keeps every value with its index.
fn realign_local(
    old: &[(f64, f64)],
    local: &[Option<f64>],
    new: &[(f64, f64)],
) -> Vec<Option<f64>> {
    if local.iter().all(Option::is_none) {
        return Vec::new();
    }
    if old.len() == new.len() {
        return local.to_vec();
    }
    new.iter()
        .map(|q| {
            old.iter()
                .position(|o| o == q)
                .and_then(|i| local.get(i).copied().flatten())
        })
        .collect()
}

/// The point smoothing choices (B1-0112): the graph's, Smooth, Not Smooth,
/// a custom degree.
const POINT_SMOOTHING: [&str; 4] = ["Graph", "Smooth", "Not smooth", "Custom"];

fn point_smoothing_choice(v: Option<f64>) -> usize {
    match v {
        None => 0,
        Some(s) if s >= 1.0 => 1,
        Some(s) if s <= 0.0 => 2,
        Some(_) => 3,
    }
}

/// The curve editor with every option: `full` (Smoothness, Outside and the
/// per-point smoothing) and `overshoot` (B1-0113, the Overshoot control).
pub fn curve_editor_opts(
    ui: &mut Ui,
    id: egui::Id,
    c: &Curve,
    default: &Curve,
    full: bool,
    overshoot: bool,
) -> Option<Curve> {
    let mut st = load(ui, id);
    let mut curve = c.clone();
    if !overshoot && curve.overshoot > 0.0 {
        curve = curve.without_overshoot();
    }
    let mut changed = false;
    ui.vertical(|ui| {
        let size = Vec2::new(width(ui).min(CURVE_CANVAS.x), CURVE_CANVAS.y);
        let (rect, resp, p) = point_canvas(ui, size);
        // B1-0113: the drawing bounds — the nominal 0 – 1, widened to the
        // effective range while the graph overshoots.
        let (ylo, yhi) = curve.y_range();
        let m = map_of(rect, (0.0, 1.0), (ylo, yhi));
        remember(ui, id, "plot", &resp.clone().with_new_rect(rect));
        let g = grid_stroke(ui);
        for k in 1..4 {
            let t = k as f64 / 4.0;
            p.line_segment([pos(&m, t, ylo), pos(&m, t, yhi)], g);
            p.line_segment([pos(&m, 0.0, t), pos(&m, 1.0, t)], g);
        }
        if yhi > 1.0 {
            // The nominal band's edges, stronger than the grid.
            let band = Stroke::new(1.2, accent(ui).gamma_multiply(0.45));
            for y in [0.0, 1.0] {
                p.line_segment([pos(&m, 0.0, y), pos(&m, 1.0, y)], band);
            }
        }
        let mut pts = Points::new(curve.points.clone(), Order::Curve, (0.0, 1.0), (ylo, yhi));
        pts.max_points = MAX_CURVE_POINTS;
        if ui.input(|i| i.modifiers.shift) {
            pts.snap = Some(0.05);
        }
        let before = curve.points.clone();
        if drive(ui, &resp, &m, &mut pts, &mut st) {
            curve.local = realign_local(&before, &curve.local, &pts.pts);
            curve.points = pts.pts.clone();
            changed = true;
        }
        draw_curve(ui, &p, &m, &curve);
        for (i, &(x, y)) in curve.points.iter().enumerate() {
            // Not Smooth points (corners) are marked with a square.
            if curve.local.get(i).copied().flatten() == Some(0.0) {
                let at = pos(&m, x, y);
                p.rect_stroke(
                    Rect::from_center_size(at, Vec2::splat(9.0)),
                    0.0,
                    Stroke::new(1.0, ui.visuals().text_color()),
                    egui::StrokeKind::Middle,
                );
            }
            handle(ui, &p, pos(&m, x, y), st.selected == Some(i), None);
        }
        resp.on_hover_text(
            "Drag any point (Shift snaps) · double-click to add · right-click / Delete to remove",
        );
        // A button's point operation on the current selection.
        fn act(pts: &mut Points, st: &mut State, f: fn(&mut Points) -> bool) -> bool {
            pts.selected = st.selected;
            let done = f(pts);
            st.selected = pts.selected;
            st.dragging = None;
            done
        }
        ui.horizontal(|ui| {
            let r = ui.small_button("+");
            remember(ui, id, "add", &r);
            if r.on_hover_text("Add a point in the widest gap").clicked()
                && act(&mut pts, &mut st, Points::add_middle)
            {
                curve.local = realign_local(&curve.points, &curve.local, &pts.pts);
                curve.points = pts.pts.clone();
                changed = true;
            }
            pts.selected = st.selected;
            let r = ui.add_enabled(pts.can_delete(), egui::Button::new("−").small());
            remember(ui, id, "delete", &r);
            if r.on_hover_text("Remove the selected point (a curve keeps two)")
                .clicked()
                && act(&mut pts, &mut st, Points::delete_selected)
            {
                curve.local = realign_local(&curve.points, &curve.local, &pts.pts);
                curve.points = pts.pts.clone();
                changed = true;
            }
            let can_dup = st.selected.is_some() && curve.points.len() < MAX_CURVE_POINTS;
            let r = ui.add_enabled(can_dup, egui::Button::new("Duplicate").small());
            remember(ui, id, "duplicate", &r);
            if r.on_hover_text("Copy the selected point into the wider gap beside it")
                .clicked()
                && act(&mut pts, &mut st, Points::duplicate_selected)
            {
                curve.local = realign_local(&curve.points, &curve.local, &pts.pts);
                curve.points = pts.pts.clone();
                changed = true;
            }
            let r = ui.small_button("Reset");
            remember(ui, id, "reset", &r);
            if r.on_hover_text("Back to this graph's default").clicked() {
                curve = default.clone();
                st = State::default();
                changed = true;
            }
        });
        if full {
            ui.horizontal(|ui| {
                let mut pct = curve.smoothness * 100.0;
                let r = ui.add(
                    crate::ui::stepper::Stepper::new(&mut pct)
                        .range(0.0..=100.0)
                        .speed(0.5)
                        .max_decimals(0)
                        .prefix("Smoothness ")
                        .suffix(" %"),
                );
                remember(ui, id, "smoothness", &r);
                if r.on_hover_text(
                    "0 % straight lines, 100 % the smoothest path; the points never move",
                )
                .changed()
                {
                    curve.smoothness = (pct / 100.0).clamp(0.0, 1.0);
                    changed = true;
                }
                ui.label(egui::RichText::new("Outside").weak())
                    .on_hover_text(
                        "What the graph gives before its first point and after its last",
                    );
                let combo = egui::ComboBox::from_id_salt(id.with("outside"))
                    .selected_text(curve.outside.name())
                    .width(72.0)
                    .show_ui(ui, |ui| {
                        for o in Outside::ALL {
                            let r = ui.selectable_label(curve.outside == o, o.name());
                            remember(ui, id, o.key(), &r);
                            if r.on_hover_text(outside_help(o)).clicked() && curve.outside != o {
                                curve.outside = o;
                                changed = true;
                            }
                        }
                    });
                remember(ui, id, "outside", &combo.response);
            });
            if overshoot {
                ui.horizontal(|ui| {
                    let mut pct = curve.overshoot * 100.0;
                    let r = ui.add(
                        crate::ui::stepper::Stepper::new(&mut pct)
                            .range(0.0..=MAX_OVERSHOOT * 100.0)
                            .speed(1.0)
                            .max_decimals(0)
                            .prefix("Overshoot ")
                            .suffix(" %"),
                    );
                    remember(ui, id, "overshoot", &r);
                    if r.on_hover_text(
                        "Lets the graph go beyond its nominal range (the marked band) by this share of it; the tool still keeps its results within MIDI's limits",
                    )
                    .changed()
                    {
                        curve.overshoot = (pct / 100.0).clamp(0.0, MAX_OVERSHOOT);
                        curve = curve.sanitized();
                        changed = true;
                    }
                    if curve.overshoot > 0.0 {
                        let (lo, hi) = curve.y_range();
                        ui.label(
                            egui::RichText::new(format!(
                                "Effective range {:.0} % … {:.0} %",
                                lo * 100.0,
                                hi * 100.0
                            ))
                            .weak(),
                        );
                    }
                });
            }
        }
        if let Some(i) = st.selected.filter(|&i| i < curve.points.len()) {
            let (ylo, yhi) = curve.y_range();
            ui.horizontal(|ui| {
                let (mut x, mut y) = curve.points[i];
                ui.label(format!("Point {}", i + 1));
                ui.label(egui::RichText::new("x").weak());
                let rx = ui.add(
                    crate::ui::stepper::Stepper::new(&mut x)
                        .range(0.0..=1.0)
                        .speed(0.005)
                        .max_decimals(3),
                );
                ui.label(egui::RichText::new("y").weak());
                let ry = ui.add(
                    crate::ui::stepper::Stepper::new(&mut y)
                        .range(ylo..=yhi)
                        .speed(0.005)
                        .max_decimals(3),
                );
                remember(ui, id, "x", &rx);
                remember(ui, id, "y", &ry);
                if rx.changed() || ry.changed() {
                    pts.pts = curve.points.clone();
                    pts.selected = Some(i);
                    if pts.set_selected(x, y) {
                        curve.points = pts.pts.clone();
                        changed = true;
                    }
                }
            });
            // B1-0112: this point's own smoothing.
            if full && curve.points.len() >= 3 {
                ui.horizontal(|ui| {
                    let cur = curve.local.get(i).copied().flatten();
                    let mut pick = point_smoothing_choice(cur);
                    ui.label(egui::RichText::new("Smoothing").weak())
                        .on_hover_text("How the graph passes this point: as the whole graph's Smoothness, Smooth, Not Smooth (a corner) or a custom degree; the point itself never moves");
                    let combo = egui::ComboBox::from_id_salt(id.with(("point-smooth", i)))
                        .selected_text(if pick == 0 {
                            format!("Graph ({:.0} %)", curve.smoothness * 100.0)
                        } else {
                            POINT_SMOOTHING[pick].to_owned()
                        })
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for (k, name) in POINT_SMOOTHING.iter().enumerate() {
                                let r = ui.selectable_label(pick == k, *name);
                                remember(ui, id, ["ps-graph", "ps-smooth", "ps-corner", "ps-custom"][k], &r);
                                if r.clicked() {
                                    pick = k;
                                }
                            }
                        });
                    remember(ui, id, "point-smoothing", &combo.response);
                    let mut next = match pick {
                        0 => None,
                        1 => Some(1.0),
                        2 => Some(0.0),
                        _ => Some(cur.filter(|s| *s > 0.0 && *s < 1.0).unwrap_or(0.5)),
                    };
                    if pick == 3 {
                        let mut pct = next.unwrap_or(0.5) * 100.0;
                        let r = ui.add(
                            crate::ui::stepper::Stepper::new(&mut pct)
                                .range(1.0..=99.0)
                                .speed(0.5)
                                .max_decimals(0)
                                .suffix(" %"),
                        );
                        remember(ui, id, "point-degree", &r);
                        if r.changed() {
                            next = Some((pct / 100.0).clamp(0.01, 0.99));
                        }
                    }
                    if next != cur {
                        curve = curve.clone().with_local(i, next);
                        changed = true;
                    }
                });
            }
        }
    });
    store(ui, id, st);
    changed.then(|| curve.sanitized())
}

/// Test hook: where a control of an editor was drawn, so the interaction
/// tests press the real widget (nothing is stored outside tests).
#[inline]
fn remember(ui: &Ui, id: egui::Id, what: &'static str, r: &Response) {
    #[cfg(test)]
    ui.data_mut(|d| d.insert_temp(id.with(what), r.rect));
    #[cfg(not(test))]
    let _ = (ui, id, what, r);
}

fn outside_help(o: Outside) -> &'static str {
    match o {
        Outside::Clamp => "Hold the first / last point's value",
        Outside::Extend => "Continue the end segments' slope",
        Outside::Repeat => "Repeat the curve between the first and last point",
        Outside::Reflect => "Mirror the curve back and forth",
    }
}

/// The curve's path over the whole plot: between its points in the accent
/// colour, outside them (the policy) fainter, so moved ends stay visible.
fn draw_curve(ui: &Ui, p: &egui::Painter, m: &Map, c: &Curve) {
    let (Some(&(x0, _)), Some(&(x1, _))) = (c.points.first(), c.points.last()) else {
        return;
    };
    let strong = Stroke::new(1.6, accent(ui));
    let weak = Stroke::new(1.2, accent(ui).gamma_multiply(0.4));
    let path = |a: f64, b: f64| -> Vec<Pos2> {
        let n = (((b - a) * 96.0).ceil() as usize).max(1);
        (0..=n)
            .map(|k| {
                let x = a + (b - a) * k as f64 / n as f64;
                pos(m, x, c.eval(x))
            })
            .collect()
    };
    if x0 > 0.0 {
        p.add(egui::Shape::line(path(0.0, x0), weak));
    }
    if x1 < 1.0 {
        p.add(egui::Shape::line(path(x1, 1.0), weak));
    }
    p.add(egui::Shape::line(path(x0, x1), strong));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `f` inside a central panel for each frame's events; returns what
    /// the last frame returned.
    fn frames<T>(events: Vec<Vec<egui::Event>>, mut f: impl FnMut(&mut Ui) -> T) -> Vec<T> {
        let ctx = egui::Context::default();
        let mut out = Vec::new();
        for ev in events {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events: ev,
                ..Default::default()
            };
            let mut r = None;
            let mut full = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    r = Some(f(ui));
                });
            });
            full.textures_delta.clear();
            out.push(r.expect("frame ran"));
        }
        out
    }

    fn moved(p: Pos2) -> egui::Event {
        egui::Event::PointerMoved(p)
    }

    fn button(p: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn dragging_a_curve_point_with_the_pointer_moves_it() {
        // §184: a real egui pointer drag on the editor.
        let start = Curve::new(vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        // Frame 1 learns where the canvas is.
        let mut rect = Rect::NOTHING;
        let mut cur = start.clone();
        let id = egui::Id::new("curve-test");
        let probe = frames(vec![vec![]], |ui| {
            let r = ui.cursor();
            let _ = curve_editor(ui, id, &cur, &start);
            r
        });
        rect = rect.union(Rect::from_min_size(probe[0].min, Vec2::new(260.0, 110.0)));
        let w = rect.width().min(260.0);
        let m = map_of(
            Rect::from_min_size(rect.min, Vec2::new(w, 110.0)).shrink(model::HIT),
            (0.0, 1.0),
            (0.0, 1.0),
        );
        let a = pos(&m, 0.5, 0.5);
        let b = pos(&m, 0.75, 0.25);
        let mid = a + (b - a) * 0.5;
        let results = frames(
            vec![
                vec![moved(a)],
                vec![button(a, true)],
                vec![moved(mid)],
                vec![moved(b)],
                vec![button(b, false)],
            ],
            |ui| {
                if let Some(c) = curve_editor(ui, id, &cur, &start) {
                    cur = c;
                }
                cur.clone()
            },
        );
        let end = results.last().unwrap();
        let (x, y) = end.points[1];
        assert!(
            (x - 0.75).abs() < 0.02 && (y - 0.25).abs() < 0.02,
            "{:?}",
            end.points
        );
        assert_eq!(end.points[0], (0.0, 0.0));
        assert_eq!(end.points[2], (1.0, 1.0));
    }

    #[test]
    fn a_point_on_the_plot_border_is_grabbed_at_its_centre_and_just_outside() {
        // Wave Q: a value at the end of its range is drawn on the plot's
        // border; a press on its centre, or outside the border within the hit
        // radius, must grab it (before, only its inner half could be grabbed).
        let start = Curve::new(vec![(0.0, 0.0), (0.5, 1.0), (1.0, 1.0)]);
        for dy in [0.0f32, -(model::HIT - 2.0)] {
            let id = egui::Id::new(("edge-test", dy as i32));
            let mut cur = start.clone();
            let probe = frames(vec![vec![]], |ui| {
                let r = ui.cursor();
                let _ = curve_editor(ui, id, &cur, &start);
                r
            });
            let m = map_of(
                Rect::from_min_size(probe[0].min, Vec2::new(260.0, 110.0)).shrink(model::HIT),
                (0.0, 1.0),
                (0.0, 1.0),
            );
            let a = pos(&m, 0.5, 1.0) + Vec2::new(0.0, dy);
            let b = pos(&m, 0.5, 0.5);
            let results = frames(
                vec![
                    vec![moved(a)],
                    vec![button(a, true)],
                    vec![moved(a + (b - a) * 0.5)],
                    vec![moved(b)],
                    vec![button(b, false)],
                ],
                |ui| {
                    if let Some(c) = curve_editor(ui, id, &cur, &start) {
                        cur = c;
                    }
                    cur.clone()
                },
            );
            let (x, y) = results.last().unwrap().points[1];
            assert!(
                (x - 0.5).abs() < 0.03 && (0.4..0.65).contains(&y),
                "press {dy} px from the center: {:?}",
                results.last().unwrap().points
            );
        }
    }

    #[test]
    fn dragging_the_bl_corner_changes_exactly_its_values() {
        // §242: drag BL; only BL's two values change.
        let id = egui::Id::new("quad-test");
        let mut v: Vec<i64> = vec![0, 0, 1000, 0, 800, 1000, 200, 1000];
        let probe = frames(vec![vec![]], |ui| {
            let r = ui.cursor();
            let _ = list_editor(
                ui,
                id,
                Editor::Quad { offsets: false },
                &v,
                -2000,
                3000,
                8,
                &EditorCtx::default(),
            );
            r
        });
        let w = 240.0f32.min(340.0);
        let m = map_of(
            Rect::from_min_size(probe[0].min, Vec2::new(w, w * 0.75)),
            (-500.0, 1500.0),
            (-500.0, 1500.0),
        );
        let a = pos(&m, 0.0, 0.0);
        let b = pos(&m, 200.0, 100.0);
        let res = frames(
            vec![
                vec![moved(a)],
                vec![button(a, true)],
                vec![moved(a + (b - a) * 0.5)],
                vec![moved(b)],
                vec![button(b, false)],
            ],
            |ui| {
                if let Some(n) = list_editor(
                    ui,
                    id,
                    Editor::Quad { offsets: false },
                    &v,
                    -2000,
                    3000,
                    8,
                    &EditorCtx::default(),
                ) {
                    v = n;
                }
                v.clone()
            },
        );
        let end = res.last().unwrap();
        assert!(
            (end[0] - 200).abs() <= 10 && (end[1] - 100).abs() <= 10,
            "{end:?}"
        );
        assert_eq!(&end[2..], &[1000, 0, 800, 1000, 200, 1000]);
    }

    // ------------------------------------------------ v0.9 S29: curve widget

    /// One curve editor driven frame by frame at a display scale, like the
    /// app drives it (the curve it returns is fed back in).
    struct Rig {
        ctx: egui::Context,
        id: egui::Id,
        cur: Curve,
        def: Curve,
        full: bool,
        /// B1-0113: the parameter allows overshoot.
        overshoot: bool,
        changes: usize,
    }

    impl Rig {
        fn new(ppp: f32, start: Curve) -> Rig {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let mut r = Rig {
                ctx,
                id: egui::Id::new(("curve-dpi", (ppp * 100.0) as i32)),
                def: start.clone(),
                cur: start,
                full: true,
                overshoot: false,
                changes: 0,
            };
            r.frame(vec![]);
            r.frame(vec![]);
            assert_eq!(r.ctx.pixels_per_point(), ppp);
            r
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events,
                ..Default::default()
            };
            let (id, def, full, over) = (self.id, self.def.clone(), self.full, self.overshoot);
            let mut cur = self.cur.clone();
            let mut n = 0;
            let mut out = self.ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    if let Some(c) = curve_editor_opts(ui, id, &cur, &def, full, over) {
                        cur = c;
                        n += 1;
                    }
                });
            });
            out.textures_delta.clear();
            self.cur = cur;
            self.changes += n;
        }

        fn rect(&self, what: &'static str) -> Option<Rect> {
            self.ctx.data(|d| d.get_temp::<Rect>(self.id.with(what)))
        }

        fn forget(&self, what: &'static str) {
            self.ctx.data_mut(|d| d.remove::<Rect>(self.id.with(what)));
        }

        fn at(&self, x: f64, y: f64) -> Pos2 {
            let m = map_of(
                self.rect("plot").expect("plot drawn"),
                (0.0, 1.0),
                self.cur.y_range(),
            );
            pos(&m, x, y)
        }

        fn state(&self) -> State {
            self.ctx
                .data(|d| d.get_temp::<State>(self.id))
                .unwrap_or_default()
        }

        fn press(&mut self, p: Pos2) {
            self.frame(vec![moved(p)]);
            self.frame(vec![button(p, true)]);
        }

        fn release(&mut self, p: Pos2) {
            self.frame(vec![button(p, false)]);
            self.frame(vec![]);
        }

        fn click(&mut self, p: Pos2) {
            self.press(p);
            self.release(p);
        }

        /// Press at `a`, move to `b` in `steps`, release.
        fn drag(&mut self, a: Pos2, b: Pos2, steps: usize) {
            self.press(a);
            for k in 1..=steps {
                self.frame(vec![moved(a + (b - a) * (k as f32 / steps as f32))]);
            }
            self.release(b);
        }

        fn secondary_click(&mut self, p: Pos2) {
            let b = |pressed| egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![moved(p)]);
            self.frame(vec![b(true)]);
            self.frame(vec![b(false)]);
            self.frame(vec![]);
        }

        fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
            let e = |pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            self.frame(vec![e(true), e(false)]);
        }

        /// Clicks a DragValue (text mode), replaces its text, presses Enter.
        fn type_into(&mut self, what: &'static str, text: &str) {
            let r = self
                .rect(what)
                .unwrap_or_else(|| panic!("{what} not drawn"));
            self.click(r.center());
            self.key(egui::Key::A, egui::Modifiers::COMMAND);
            self.frame(vec![egui::Event::Text(text.into())]);
            self.key(egui::Key::Enter, egui::Modifiers::NONE);
            self.frame(vec![]);
        }

        fn near(&self, i: usize, x: f64, y: f64, tol: f64) -> bool {
            let (px, py) = self.cur.points[i];
            (px - x).abs() <= tol && (py - y).abs() <= tol
        }
    }

    const SCALES: [f32; 4] = [1.0, 1.25, 1.5, 2.0];

    fn three() -> Curve {
        Curve::new(vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)])
    }

    #[test]
    fn a_press_selects_a_point_and_jitter_below_the_threshold_never_moves_it() {
        // v0.9 §106 / §251: press, drag threshold, release — at every scale.
        for ppp in SCALES {
            let mut r = Rig::new(ppp, three());
            let a = r.at(0.5, 0.5);
            // A plain click selects.
            r.click(a);
            assert_eq!(r.state().selected, Some(1), "{ppp}");
            assert_eq!(
                (r.cur.clone(), r.changes),
                (three(), 0),
                "{ppp}: a click moved it"
            );
            // Jitter under the threshold (2 points) is not a drag.
            r.drag(a, a + Vec2::new(2.0, -1.0), 2);
            assert_eq!(r.cur, three(), "{ppp}: jitter moved the point");
            assert_eq!(r.state().selected, Some(1));
            // Past the threshold it follows; the release keeps the place.
            let b = r.at(0.7, 0.2);
            r.drag(a, b, 4);
            assert!(r.near(1, 0.7, 0.2, 0.01), "{ppp}: {:?}", r.cur.points);
            assert!(r.state().dragging.is_none() && !r.state().armed);
            let after = r.cur.clone();
            r.frame(vec![moved(r.at(0.3, 0.9))]);
            assert_eq!(
                r.cur, after,
                "{ppp}: moving after release changed the curve"
            );
        }
    }

    #[test]
    fn end_points_drag_freely_and_stay_where_they_are_dropped() {
        // v0.9 §104 / §251 at every scale: first away from x = 0, last inward.
        for ppp in SCALES {
            let mut r = Rig::new(ppp, three());
            let (a, b) = (r.at(0.0, 0.0), r.at(0.2, 0.3));
            r.drag(a, b, 4);
            assert!(r.near(0, 0.2, 0.3, 0.01), "{ppp}: {:?}", r.cur.points);
            let (a, b) = (r.at(1.0, 1.0), r.at(0.8, 0.6));
            r.drag(a, b, 4);
            assert!(r.near(2, 0.8, 0.6, 0.01), "{ppp}: {:?}", r.cur.points);
            // Kept through more frames and through sanitize / text.
            r.frame(vec![]);
            assert!(r.near(0, 0.2, 0.3, 0.01) && r.near(2, 0.8, 0.6, 0.01));
            let back = Curve::from_text(&r.cur.to_text()).unwrap();
            assert_eq!(back.points.len(), 3);
            assert!((back.points[0].0 - r.cur.points[0].0).abs() < 1e-6);
            // An end cannot pass its neighbour.
            let (a, b) = (r.at(r.cur.points[0].0, r.cur.points[0].1), r.at(0.95, 0.3));
            r.drag(a, b, 4);
            assert!(
                r.cur.points[0].0 < r.cur.points[1].0,
                "{ppp}: {:?}",
                r.cur.points
            );
        }
    }

    #[test]
    fn points_are_added_deleted_and_duplicated_with_the_pointer_keys_and_buttons() {
        // v0.9 §106 / §251: add, delete (ends too, down to two), duplicate.
        for ppp in SCALES {
            let mut r = Rig::new(ppp, three());
            // Double-click in empty space adds a point there, selected.
            let p = r.at(0.25, 0.8);
            r.frame(vec![moved(p)]);
            for _ in 0..2 {
                r.frame(vec![button(p, true)]);
                r.frame(vec![button(p, false)]);
            }
            r.frame(vec![]);
            assert_eq!(r.cur.points.len(), 4, "{ppp}: {:?}", r.cur.points);
            assert!(r.near(1, 0.25, 0.8, 0.01), "{ppp}: {:?}", r.cur.points);
            assert_eq!(r.state().selected, Some(1));
            // Right-click removes the first point (an end).
            r.secondary_click(r.at(0.0, 0.0));
            assert_eq!(r.cur.points.len(), 3, "{ppp}");
            assert!(r.near(0, 0.25, 0.8, 0.01), "{ppp}: {:?}", r.cur.points);
            // Select the last point; Delete removes it.
            r.click(r.at(1.0, 1.0));
            assert_eq!(r.state().selected, Some(2));
            r.key(egui::Key::Delete, egui::Modifiers::NONE);
            assert_eq!(r.cur.points.len(), 2, "{ppp}: Delete key");
            // Two points stay: "−" is disabled, Delete does nothing.
            r.click(r.at(0.5, 0.5));
            assert_eq!(r.state().selected, Some(1));
            r.key(egui::Key::Delete, egui::Modifiers::NONE);
            assert_eq!(r.cur.points.len(), 2);
            r.click(r.rect("delete").unwrap().center());
            assert_eq!(r.cur.points.len(), 2, "{ppp}: '−' removed below two");
            // Duplicate copies the selected point into the wider gap.
            r.click(r.at(0.5, 0.5));
            r.click(r.rect("duplicate").unwrap().center());
            assert_eq!(r.cur.points.len(), 3, "{ppp}: Duplicate");
            assert!(r.near(2, 0.75, 0.5, 1e-9), "{ppp}: {:?}", r.cur.points);
            assert_eq!(r.state().selected, Some(2));
            // "+" adds into the widest gap; Reset restores the default.
            r.click(r.rect("add").unwrap().center());
            assert_eq!(r.cur.points.len(), 4, "{ppp}: '+'");
            r.click(r.rect("reset").unwrap().center());
            assert_eq!(r.cur, three(), "{ppp}: Reset");
            assert_eq!(r.state().selected, None);
        }
    }

    #[test]
    fn typed_coordinates_smoothness_and_the_outside_policy_change_only_what_they_name() {
        // v0.9 §106 / §107 / §252: numeric x / y (an end's x too),
        // Smoothness %, the Outside choice — at every scale.
        for ppp in SCALES {
            let mut r = Rig::new(ppp, three());
            r.click(r.at(0.0, 0.0));
            assert_eq!(r.state().selected, Some(0));
            r.type_into("x", "0.15");
            assert!(
                r.near(0, 0.15, 0.0, 1e-9),
                "{ppp}: x typed: {:?}",
                r.cur.points
            );
            r.type_into("y", "0.4");
            assert!(
                r.near(0, 0.15, 0.4, 1e-9),
                "{ppp}: y typed: {:?}",
                r.cur.points
            );
            assert_eq!(
                r.state().selected,
                Some(0),
                "{ppp}: typing kept the selection"
            );
            let pts = r.cur.points.clone();
            // Smoothness: the path changes, the points do not.
            let mid = r.cur.eval(0.3);
            r.type_into("smoothness", "50");
            assert_eq!(r.cur.smoothness, 0.5, "{ppp}");
            assert_eq!(r.cur.points, pts, "{ppp}: smoothness moved a point");
            assert_ne!(r.cur.eval(0.3), mid);
            // Outside: open the list, pick Reflect.
            r.click(r.rect("outside").unwrap().center());
            let refl = r.rect("reflect").expect("the list is open");
            r.click(refl.center());
            assert_eq!(r.cur.outside, Outside::Reflect, "{ppp}");
            assert_eq!(r.cur.points, pts);
            assert_eq!(r.cur.smoothness, 0.5);
            // Before the first point the curve now mirrors.
            assert!((r.cur.eval(0.1) - r.cur.eval(0.2)).abs() < 1e-12);
            r.forget("reflect");
        }
    }

    /// B1-0112 / B1-0113 (§017): a selected point's own smoothing (Not
    /// Smooth, Custom, back to the graph's) never moves it and follows it
    /// through an added point; Overshoot widens the range the point can be
    /// typed or dragged into, only where the parameter allows it.
    #[test]
    fn each_point_gets_its_own_smoothing_and_allowed_graphs_overshoot() {
        for ppp in [1.0, 1.5] {
            let mut r = Rig::new(ppp, three());
            r.overshoot = true;
            r.frame(vec![]);
            r.click(r.at(0.5, 0.5));
            assert_eq!(r.state().selected, Some(1), "{ppp}");
            r.click(r.rect("point-smoothing").expect("drawn").center());
            r.click(r.rect("ps-corner").expect("the list is open").center());
            assert_eq!(r.cur.local.get(1).copied().flatten(), Some(0.0), "{ppp}");
            assert_eq!(r.cur.points, three().points, "{ppp}: the point moved");
            r.forget("ps-corner");
            r.click(r.rect("point-smoothing").unwrap().center());
            r.click(r.rect("ps-custom").expect("open").center());
            r.forget("ps-custom");
            r.type_into("point-degree", "25");
            assert_eq!(r.cur.local.get(1).copied().flatten(), Some(0.25), "{ppp}");
            // "+" adds a point in the widest gap; point 2's value follows it.
            let p2 = r.cur.points[1];
            r.click(r.rect("add").unwrap().center());
            let k = r.cur.points.iter().position(|q| *q == p2).unwrap();
            assert_eq!(r.cur.points.len(), 4);
            assert_eq!(r.cur.local.get(k).copied().flatten(), Some(0.25), "{ppp}");
            assert_eq!(r.cur.local.iter().flatten().count(), 1);
            // Back to the graph's smoothing.
            r.click(r.at(p2.0, p2.1));
            r.click(r.rect("point-smoothing").unwrap().center());
            r.click(r.rect("ps-graph").expect("open").center());
            r.forget("ps-graph");
            assert!(r.cur.local.iter().all(Option::is_none), "{ppp}");
            // Overshoot 50 %: y can now be 1.4.
            r.type_into("overshoot", "50");
            assert_eq!(r.cur.overshoot, 0.5, "{ppp}");
            assert_eq!(r.cur.y_range(), (-0.5, 1.5));
            r.type_into("y", "1.4");
            let i = r.state().selected.unwrap();
            assert!(
                (r.cur.points[i].1 - 1.4).abs() < 1e-9,
                "{ppp}: {:?}",
                r.cur.points
            );
            // Dragging below 0 reaches the effective range's bottom.
            let at = r.at(r.cur.points[i].0, 1.4);
            let to = r.at(r.cur.points[i].0, -0.45);
            r.drag(at, to, 6);
            assert!(r.cur.points[i].1 < 0.0, "{ppp}: {:?}", r.cur.points);
        }
        // A graph that does not allow overshoot: no control; invalid typed y is refused.
        let mut r = Rig::new(1.0, three());
        r.forget("overshoot");
        r.frame(vec![]);
        assert!(r.rect("overshoot").is_none());
        r.click(r.at(0.5, 0.5));
        r.type_into("y", "1.4");
        assert_eq!(
            r.cur.points[1].1, 0.5,
            "out-of-domain typed values do not silently clamp"
        );
    }

    #[test]
    fn the_plain_editor_has_no_smoothness_or_policy_but_free_ends() {
        // Spiderweb's velocity envelope (straight, clamped).
        let mut r = Rig::new(1.0, Curve::flat(1.0));
        r.full = false;
        r.frame(vec![]);
        r.forget("smoothness");
        r.forget("outside");
        r.frame(vec![]);
        assert!(r.rect("smoothness").is_none() && r.rect("outside").is_none());
        let (a, b) = (r.at(0.0, 1.0), r.at(0.3, 0.5));
        r.drag(a, b, 4);
        assert!(r.near(0, 0.3, 0.5, 0.01), "{:?}", r.cur.points);
        assert_eq!((r.cur.smoothness, r.cur.outside), (0.0, Outside::Clamp));
    }
}
