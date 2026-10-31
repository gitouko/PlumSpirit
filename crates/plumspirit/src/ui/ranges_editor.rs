//! The Ranges editor of Note Properties (v1.0-beta1 B1-0078, contract §010):
//! any number of range rows — add, switch, edit, reorder, remove — each with
//! its target, the part of the selection it covers, From / To through its own
//! graph (per-point smoothness and Overshoot, §017) and its blend; a strip
//! shows where the rows lie and where rows of the same property meet, and a
//! list explains, in order, which applies last. The rows are the
//! `n13.rows` text parameter (presets, Preview and Undo are the platform's).

use egui::{Color32, Rect, RichText, Sense, Stroke, Ui, Vec2};
use plumspirit_engine::editor::ranges::{
    self, Across, Blend, MAX_ROWS, MAX_VALUE, RangeRow, Target, explain, overlaps,
};

/// The editor's per-window state: the selected row (whose graph is shown).
#[derive(Clone, Copy, Debug, Default)]
struct State {
    selected: Option<usize>,
}

/// A row's color in the strip (by property).
fn tint(t: Target) -> Color32 {
    match t {
        Target::Velocities => Color32::from_rgb(0xE0, 0x8A, 0x3C),
        Target::Gates => Color32::from_rgb(0x4C, 0xA8, 0xE0),
        Target::Ticks => Color32::from_rgb(0x8E, 0x7C, 0xE8),
        Target::Transpose | Target::Keys => Color32::from_rgb(0x5C, 0xC0, 0x7A),
        Target::Channels => Color32::from_rgb(0xD8, 0x5C, 0x9A),
        Target::Subchannels => Color32::from_rgb(0xC8, 0xB0, 0x40),
        Target::Controller(_) => Color32::from_rgb(0x9A, 0x9A, 0x9A),
    }
}

/// Draws the Ranges editor for the rows text `text`; returns the new text
/// when something changed this frame. An invalid text (a hand-edited
/// preset) is shown with its error and a button to start again.
pub fn ranges_editor(ui: &mut Ui, id: egui::Id, text: &str) -> Option<String> {
    let mut rows = match ranges::parse(text) {
        Ok(r) => r,
        Err(e) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            if ui.small_button("Clear the ranges").clicked() {
                return Some(String::new());
            }
            return None;
        }
    };
    let mut st: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut changed = false;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(
                    "Rows apply top to bottom; a later row works on what the rows above left.",
                )
                .weak(),
            );
        });
        ui.horizontal(|ui| {
            let can_add = rows.len() < MAX_ROWS;
            ui.add_enabled_ui(can_add, |ui| {
                let m = ui.menu_button("Add range", |ui| {
                    for t in Target::ALL {
                        let r = ui.button(t.name());
                        remember(ui, id, add_key(t), &r);
                        if r.clicked() {
                            rows.push(RangeRow::new(t));
                            st.selected = Some(rows.len() - 1);
                            changed = true;
                            ui.close();
                        }
                    }
                });
                remember(ui, id, "add-menu", &m.response);
                m.response
                    .on_hover_text("Adds a range at the bottom (it applies after the others)");
            });
            ui.label(RichText::new(format!("{} range(s)", rows.len())).weak());
        });
        strip(ui, &rows, st.selected);
        let mut action: Option<(usize, Act)> = None;
        let few = rows.len() <= 6;
        let mut draw_rows = |ui: &mut Ui| {
            for (i, r) in rows.iter_mut().enumerate() {
                if let Some(a) = row_ui(ui, id, i, r, st.selected == Some(i), &mut changed) {
                    action = Some((i, a));
                }
            }
        };
        // A few rows sit in the tool window's own scrolling; many get a
        // scroll area of their own.
        if few {
            draw_rows(ui);
        } else {
            egui::ScrollArea::vertical()
                .id_salt(id.with("rows"))
                .max_height(300.0)
                .show(ui, draw_rows);
        }
        if let Some((i, a)) = action {
            match a {
                Act::Select => {
                    st.selected = if st.selected == Some(i) {
                        None
                    } else {
                        Some(i)
                    }
                }
                Act::Up if i > 0 => {
                    rows.swap(i, i - 1);
                    st.selected = Some(i - 1);
                    changed = true;
                }
                Act::Down if i + 1 < rows.len() => {
                    rows.swap(i, i + 1);
                    st.selected = Some(i + 1);
                    changed = true;
                }
                Act::Remove => {
                    rows.remove(i);
                    st.selected = st.selected.and_then(|s| match s.cmp(&i) {
                        std::cmp::Ordering::Less => Some(s),
                        std::cmp::Ordering::Equal => None,
                        std::cmp::Ordering::Greater => Some(s - 1),
                    });
                    changed = true;
                }
                Act::Duplicate if rows.len() < MAX_ROWS => {
                    let r = rows[i].clone();
                    rows.insert(i + 1, r);
                    st.selected = Some(i + 1);
                    changed = true;
                }
                _ => {}
            }
        }
        // The selected row's graph.
        if let Some(i) = st.selected.filter(|&i| i < rows.len()) {
            let r = &mut rows[i];
            ui.label(format!(
                "Range {} graph: {} {} at the span's start (0) → {} at its end (1)",
                i + 1,
                r.target.name(),
                r.from,
                r.to
            ));
            let gid = id.with(("graph", i));
            if let Some(c) = crate::ui::param_editors::curve_editor_overshoot(
                ui,
                gid,
                &r.graph,
                &plumspirit_engine::transform::Curve::identity(),
                true,
            ) {
                r.graph = c;
                changed = true;
            }
        }
        // Where rows of the same property meet, in order.
        let ov = overlaps(&rows);
        if !ov.is_empty() {
            ui.label(RichText::new("Overlaps").strong());
            for o in ov.iter().take(12) {
                ui.label(RichText::new(explain(&rows, o)).weak());
            }
            if ov.len() > 12 {
                ui.label(RichText::new(format!("… and {} more.", ov.len() - 12)).weak());
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, st));
    changed.then(|| ranges::to_text(&rows))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Select,
    Up,
    Down,
    Duplicate,
    Remove,
}

fn add_key(t: Target) -> &'static str {
    match t {
        Target::Velocities => "add-vel",
        Target::Gates => "add-gates",
        Target::Ticks => "add-ticks",
        Target::Transpose => "add-transpose",
        Target::Keys => "add-keys",
        Target::Channels => "add-ch",
        Target::Subchannels => "add-sub",
        Target::Controller(_) => "add-cc",
    }
}

/// One row: switch, number, target, axis, blend, span, From / To and the
/// row buttons. Returns a row action.
fn row_ui(
    ui: &mut Ui,
    id: egui::Id,
    i: usize,
    r: &mut RangeRow,
    selected: bool,
    changed: &mut bool,
) -> Option<Act> {
    let mut act = None;
    let rid = id.with(("row", i));
    let frame = egui::Frame::group(ui.style()).stroke(Stroke::new(
        if selected { 1.5 } else { 1.0 },
        if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        },
    ));
    frame.show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let (sw, _) = ui.allocate_exact_size(Vec2::new(10.0, 14.0), Sense::hover());
            ui.painter().rect_filled(sw, 0.0, tint(r.target));
            if ui
                .checkbox(&mut r.on, "")
                .on_hover_text("Switches this range on or off")
                .changed()
            {
                *changed = true;
            }
            let num = ui.selectable_label(selected, format!("{}", i + 1));
            remember(ui, rid, "row-select", &num);
            if num.on_hover_text("Shows this range's graph").clicked() {
                act = Some(Act::Select);
            }
            let mut pick = Target::ALL
                .iter()
                .position(|t| std::mem::discriminant(t) == std::mem::discriminant(&r.target))
                .unwrap_or(0);
            let before = pick;
            egui::ComboBox::from_id_salt(rid.with("target"))
                .selected_text(r.target.name())
                .width(96.0)
                .show_ui(ui, |ui| {
                    for (k, t) in Target::ALL.iter().enumerate() {
                        ui.selectable_value(&mut pick, k, t.name());
                    }
                });
            if pick != before {
                let t = Target::ALL[pick];
                let (f, to) = t.defaults();
                r.target = t;
                r.from = f;
                r.to = to;
                *changed = true;
            }
            if let Target::Controller(n) = r.target {
                let mut v = i64::from(n);
                if ui
                    .add(
                        crate::ui::stepper::Stepper::new(&mut v)
                            .range(0..=i64::from(ranges::MAX_CONTROLLER))
                            .prefix("CC "),
                    )
                    .changed()
                {
                    r.target =
                        Target::Controller(v.clamp(0, i64::from(ranges::MAX_CONTROLLER)) as u8);
                    *changed = true;
                }
            }
            ui.label(RichText::new("along").weak());
            let mut a = Across::ALL.iter().position(|x| *x == r.across).unwrap_or(0);
            let b = a;
            egui::ComboBox::from_id_salt(rid.with("across"))
                .selected_text(r.across.name())
                .width(84.0)
                .show_ui(ui, |ui| {
                    for (k, x) in Across::ALL.iter().enumerate() {
                        ui.selectable_value(&mut a, k, x.name());
                    }
                });
            if a != b {
                r.across = Across::ALL[a];
                *changed = true;
            }
            let mut bl = Blend::ALL.iter().position(|x| *x == r.blend).unwrap_or(0);
            let b0 = bl;
            egui::ComboBox::from_id_salt(rid.with("blend"))
                .selected_text(r.blend.name())
                .width(84.0)
                .show_ui(ui, |ui| {
                    for (k, x) in Blend::ALL.iter().enumerate() {
                        ui.selectable_value(&mut bl, k, x.name());
                    }
                });
            if bl != b0 {
                r.blend = Blend::ALL[bl];
                *changed = true;
            }
            for (label, tip, a2) in [
                ("▲", "Moves this range up (it applies earlier)", Act::Up),
                ("▼", "Moves this range down (it applies later)", Act::Down),
                ("Copy", "Duplicates this range below itself", Act::Duplicate),
                ("×", "Removes this range", Act::Remove),
            ] {
                let resp = ui.small_button(label);
                remember(ui, rid, act_key(a2), &resp);
                if resp.on_hover_text(tip).clicked() {
                    act = Some(a2);
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            let (mut lo, mut hi) = (r.span.0 * 100.0, r.span.1 * 100.0);
            ui.label(RichText::new("Covers").weak());
            let a = ui.add(
                crate::ui::stepper::Stepper::new(&mut lo)
                    .range(0.0..=100.0)
                    .max_decimals(1)
                    .suffix(" %"),
            );
            ui.label("–");
            let b = ui.add(
                crate::ui::stepper::Stepper::new(&mut hi)
                    .range(0.0..=100.0)
                    .max_decimals(1)
                    .suffix(" %"),
            );
            if a.changed() || b.changed() {
                let (l, h) = ((lo / 100.0).clamp(0.0, 1.0), (hi / 100.0).clamp(0.0, 1.0));
                r.span = (l.min(h), l.max(h));
                *changed = true;
            }
            ui.label(RichText::new("From").weak());
            let mut f = r.from;
            let c = ui.add(crate::ui::stepper::Stepper::new(&mut f).range(-MAX_VALUE..=MAX_VALUE));
            ui.label(RichText::new("To").weak());
            let mut t = r.to;
            let d = ui.add(crate::ui::stepper::Stepper::new(&mut t).range(-MAX_VALUE..=MAX_VALUE));
            remember(ui, rid, "row-from", &c);
            remember(ui, rid, "row-to", &d);
            if c.changed() || d.changed() {
                r.from = f;
                r.to = t;
                *changed = true;
            }
            ui.label(RichText::new(r.target.unit()).weak());
        });
    });
    act
}

fn act_key(a: Act) -> &'static str {
    match a {
        Act::Select => "row-select",
        Act::Up => "row-up",
        Act::Down => "row-down",
        Act::Duplicate => "row-duplicate",
        Act::Remove => "row-remove",
    }
}

/// The strip: one lane per row (switched-off rows faint), its span filled
/// in its property's color; where rows of the same property meet, outlined.
fn strip(ui: &mut Ui, rows: &[RangeRow], selected: Option<usize>) {
    if rows.is_empty() {
        return;
    }
    let lanes = rows.len().min(24);
    let lane_h = (60.0 / lanes as f32).clamp(3.0, 10.0);
    let w = ui.available_width().clamp(200.0, 340.0);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(w, lane_h * lanes as f32 + 4.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let x = |u: f64| rect.left() + 2.0 + (rect.width() - 4.0) * u as f32;
    for (i, r) in rows.iter().take(lanes).enumerate() {
        let y0 = rect.top() + 2.0 + lane_h * i as f32;
        let c = tint(r.target).gamma_multiply(if r.on { 0.85 } else { 0.25 });
        let span = Rect::from_min_max(
            egui::pos2(x(r.span.0), y0 + 0.5),
            egui::pos2(x(r.span.1).max(x(r.span.0) + 2.0), y0 + lane_h - 0.5),
        );
        p.rect_filled(span, 0.0, c);
        if selected == Some(i) {
            p.rect_stroke(
                span,
                0.0,
                Stroke::new(1.2, ui.visuals().text_color()),
                egui::StrokeKind::Middle,
            );
        }
    }
    let ov = overlaps(rows);
    for o in &ov {
        if o.last >= lanes {
            continue;
        }
        for k in [o.first, o.last] {
            let y0 = rect.top() + 2.0 + lane_h * k as f32;
            let r = Rect::from_min_max(
                egui::pos2(x(o.span.0), y0),
                egui::pos2(x(o.span.1).max(x(o.span.0) + 2.0), y0 + lane_h),
            );
            p.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, ui.visuals().warn_fg_color),
                egui::StrokeKind::Inside,
            );
        }
    }
    let tip = if ov.is_empty() {
        "Each lane is a range (top = first); its bar is the part of the selection it covers."
            .to_owned()
    } else {
        format!(
            "Each lane is a range (top = first); outlined: rows of the same property that meet. {}",
            explain(rows, &ov[0])
        )
    };
    resp.on_hover_text(tip);
}

/// Test hook (as the curve editor's): where a control was drawn.
#[inline]
fn remember(ui: &Ui, id: egui::Id, what: &'static str, r: &egui::Response) {
    #[cfg(test)]
    ui.data_mut(|d| d.insert_temp(id.with(what), r.rect));
    #[cfg(not(test))]
    let _ = (ui, id, what, r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect};

    /// The Ranges editor driven frame by frame (the text it returns is fed
    /// back in, as the tool window does).
    struct Rig {
        ctx: egui::Context,
        id: egui::Id,
        text: String,
    }

    impl Rig {
        fn new(text: &str) -> Rig {
            let mut r = Rig {
                ctx: egui::Context::default(),
                id: egui::Id::new("ranges-test"),
                text: text.to_owned(),
            };
            r.frame(vec![]);
            r.frame(vec![]);
            r
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 900.0))),
                events,
                ..Default::default()
            };
            let id = self.id;
            let mut text = self.text.clone();
            let mut out = self.ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    if let Some(t) = ranges_editor(ui, id, &text) {
                        text = t;
                    }
                });
            });
            out.textures_delta.clear();
            self.text = text;
        }

        fn rect(&self, id: egui::Id, what: &'static str) -> Option<Rect> {
            self.ctx.data(|d| d.get_temp::<Rect>(id.with(what)))
        }

        fn click(&mut self, p: Pos2) {
            let b = |pressed| egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(p)]);
            self.frame(vec![b(true)]);
            self.frame(vec![b(false)]);
            self.frame(vec![]);
        }

        fn rows(&self) -> Vec<RangeRow> {
            ranges::parse(&self.text).unwrap()
        }

        fn add(&mut self, what: &'static str) {
            let at = self
                .rect(self.id, "add-menu")
                .expect("the Add range button is remembered")
                .center();
            self.click(at);
            let item = self.rect(self.id, what).expect("the menu is open");
            self.click(item.center());
        }

        fn row_ctl(&self, i: usize, what: &'static str) -> Pos2 {
            self.rect(self.id.with(("row", i)), what)
                .unwrap_or_else(|| panic!("row {i} {what} not drawn"))
                .center()
        }
    }

    #[test]
    fn rows_are_added_reordered_duplicated_and_removed() {
        let mut r = Rig::new("");
        r.add("add-vel");
        assert_eq!(r.rows().len(), 1);
        assert_eq!(r.rows()[0].target, Target::Velocities);
        r.add("add-gates");
        let t: Vec<Target> = r.rows().iter().map(|x| x.target).collect();
        assert_eq!(t, vec![Target::Velocities, Target::Gates]);
        // Move the gates row up: it now applies first.
        r.click(r.row_ctl(1, "row-up"));
        let t: Vec<Target> = r.rows().iter().map(|x| x.target).collect();
        assert_eq!(t, vec![Target::Gates, Target::Velocities]);
        // Duplicate the velocity row; the copy overlaps it and is listed.
        r.click(r.row_ctl(1, "row-duplicate"));
        assert_eq!(r.rows().len(), 3);
        assert_eq!(ranges::overlaps(&r.rows()).len(), 1);
        // Remove the first row.
        r.click(r.row_ctl(0, "row-remove"));
        let t: Vec<Target> = r.rows().iter().map(|x| x.target).collect();
        assert_eq!(t, vec![Target::Velocities, Target::Velocities]);
        // An invalid text is shown with a way out.
        let mut bad = Rig::new("1~vel~nowhere");
        assert_eq!(bad.text, "1~vel~nowhere");
        bad.frame(vec![]);
        assert!(ranges::parse(&bad.text).is_err());
    }
}
