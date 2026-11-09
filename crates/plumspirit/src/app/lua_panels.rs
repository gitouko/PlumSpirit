//! Host-rendered plugin windows. This module never executes Lua.
use super::PlumSpiritApp as App;
use crate::lua::{
    Capability,
    bridge::ProjectStamp,
    ui::{
        self as api, Event, InputValue, Pointer, Shared,
        schema::{Kind, Node, Primitive, Snapshot, TreeItem},
    },
};
#[cfg(test)]
use egui::Color32;
use egui::{Pos2, Rect, Sense, Vec2};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
const MAX_FRONTEND_PANELS: usize = 8;
#[derive(Default)]
struct CanvasState {
    size: [f64; 2],
    buttons: u8,
    last: Option<Pointer>,
    absolute: Option<Pos2>,
    rect: Option<Rect>,
}
pub(super) struct View {
    shared: Arc<Shared>,
    snapshot: Arc<Snapshot>,
    values: BTreeMap<String, InputValue>,
    trees: BTreeMap<String, BTreeSet<String>>,
    canvases: BTreeMap<String, CanvasState>,
    visible_canvases: BTreeSet<String>,
    images: BTreeMap<u64, api::images::Prepared>,
    visible_images: BTreeSet<u64>,
    upload_pixels: usize,
    panel_rect: Option<Rect>,
    panel_layer: Option<egui::LayerId>,
    corner_drag: Option<(Pos2, Vec2)>,
    resize_input_frame: Option<u64>,
    #[cfg(test)]
    widget_rects: BTreeMap<String, Rect>,
}
// Preserve straight RGB, including at alpha zero, without an8bit premultiplied
// intermediate on every native picker frame.
fn picker_color(value: [u8; 4]) -> egui::ecolor::Hsva {
    egui::ecolor::Hsva::from_rgba_unmultiplied(
        egui::ecolor::linear_f32_from_gamma_u8(value[0]),
        egui::ecolor::linear_f32_from_gamma_u8(value[1]),
        egui::ecolor::linear_f32_from_gamma_u8(value[2]),
        f32::from(value[3]) / 255.0,
    )
}
impl View {
    pub(super) fn cancel(&self) {
        self.shared.fail(api::CANCELED);
    }
    fn new(shared: Arc<Shared>) -> Option<Self> {
        let snapshot = shared.snapshot()?;
        let mut view = Self {
            shared,
            snapshot,
            values: BTreeMap::new(),
            trees: BTreeMap::new(),
            canvases: BTreeMap::new(),
            visible_canvases: BTreeSet::new(),
            images: BTreeMap::new(),
            visible_images: BTreeSet::new(),
            upload_pixels: 0,
            panel_rect: None,
            panel_layer: None,
            corner_drag: None,
            resize_input_frame: None,
            #[cfg(test)]
            widget_rects: BTreeMap::new(),
        };
        view.reset();
        view.emit("opened", "", InputValue::None, None);
        Some(view)
    }
    fn reset(&mut self) {
        fn walk(
            n: &Node,
            values: &mut BTreeMap<String, InputValue>,
            canvases: &mut BTreeSet<String>,
            trees: &mut BTreeMap<String, BTreeSet<String>>,
        ) {
            let value = match &n.kind {
                Kind::Column(nodes) | Kind::Row(nodes) | Kind::Group(nodes) => {
                    for n in nodes {
                        walk(n, values, canvases, trees);
                    }
                    None
                }
                Kind::Toggle(v) => Some(InputValue::Bool(*v)),
                Kind::Color(v) => Some(InputValue::Color(*v)),
                Kind::Tree {
                    items, selected, ..
                } => {
                    fn expanded(items: &[TreeItem], out: &mut BTreeSet<String>) {
                        for i in items {
                            if i.expanded {
                                out.insert(i.id.clone());
                            }
                            expanded(&i.children, out);
                        }
                    }
                    let mut out = BTreeSet::new();
                    expanded(items, &mut out);
                    trees.insert(n.id.clone(), out);
                    Some(InputValue::Text(selected.clone()))
                }

                Kind::Number { value, .. } => Some(InputValue::Number(*value)),
                Kind::Text(v) => Some(InputValue::Text(v.clone())),
                Kind::Choice { selected, .. } | Kind::List { selected, .. } => {
                    Some(InputValue::Choice(*selected))
                }
                Kind::Tabs { children, selected } => {
                    for n in children {
                        walk(n, values, canvases, trees);
                    }
                    Some(InputValue::Choice(*selected))
                }
                Kind::Canvas { .. } => {
                    canvases.insert(n.id.clone());
                    None
                }
                _ => None,
            };
            if let Some(v) = value {
                values.insert(n.id.clone(), v);
            }
        }
        self.values.clear();
        self.trees.clear();
        let mut ids = BTreeSet::new();
        walk(
            &self.snapshot.root,
            &mut self.values,
            &mut ids,
            &mut self.trees,
        );
        let removed = self
            .canvases
            .iter()
            .filter(|(id, c)| !ids.contains(*id) && c.buttons != 0)
            .map(|(id, c)| (id.clone(), c.last.clone()))
            .collect::<Vec<_>>();
        for (id, p) in removed {
            self.emit("pointer_cancel", &id, InputValue::None, p);
        }
        self.canvases.retain(|id, _| ids.contains(id));
    }
    fn emit(&self, kind: &'static str, id: &str, value: InputValue, pointer: Option<Pointer>) {
        self.shared.push(Event {
            kind,
            id: id.into(),
            value,
            pointer,
            revision: self.snapshot.revision,
            sequence: 0,
        });
    }
    fn completed_resize(&mut self, ctx: &egui::Context) -> Option<Vec2> {
        let rect = self.panel_rect?;
        let layer = self.panel_layer?;
        let frame = ctx.cumulative_frame_nr();
        if self.resize_input_frame == Some(frame) {
            return None;
        }
        self.resize_input_frame = Some(frame);
        let radius = ctx.global_style().interaction.resize_grab_radius_corner;
        let grip = Rect::from_center_size(rect.right_bottom(), Vec2::splat(2. * radius));
        let events = ctx.input(|input| {
            if !input.focused || input.events.len() > 256 {
                return None;
            }
            Some(
                input
                    .events
                    .iter()
                    .filter(|e| {
                        matches!(
                            e,
                            egui::Event::PointerButton {
                                button: egui::PointerButton::Primary,
                                ..
                            } | egui::Event::PointerGone
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        });
        let Some(events) = events else {
            self.corner_drag = None;
            return None;
        };
        let mut completed = None;
        for event in events {
            match event {
                egui::Event::PointerButton {
                    pos, pressed: true, ..
                } => {
                    self.corner_drag = (grip.contains(pos) && ctx.layer_id_at(pos) == Some(layer))
                        .then_some((pos, rect.size()));
                }
                egui::Event::PointerButton {
                    pos,
                    pressed: false,
                    ..
                } => {
                    if let Some((start, size)) = self.corner_drag.take() {
                        let delta = pos - start;
                        let size = size + delta;
                        if delta.length_sq() >= 1. && size.x.is_finite() && size.y.is_finite() {
                            completed =
                                Some(size.clamp(Vec2::new(280., 160.), Vec2::new(2048., 1536.)));
                        }
                    }
                }
                egui::Event::PointerGone => self.corner_drag = None,
                _ => {}
            }
        }
        // Keep the authorized starting size across frame boundaries. Native
        // Window still provides live resizing; this preserves its final size
        // even if press/move were coalesced before native dragging could latch.
        completed
    }
    fn show(&mut self, ctx: &egui::Context, upload_pixels: &mut usize) {
        self.upload_pixels = *upload_pixels;
        self.visible_images.clear();
        self.visible_canvases.clear();
        #[cfg(test)]
        self.widget_rects.clear();
        if let Some(snapshot) = self.shared.snapshot()
            && snapshot.revision != self.snapshot.revision
        {
            self.snapshot = snapshot;
            self.reset();
        }
        let snapshot = Arc::clone(&self.snapshot);
        let mut open = true;
        let mut window = egui::Window::new(&snapshot.title)
            .id(egui::Id::new((
                "plum-lua-panel",
                self.shared.plugin.as_ref(),
                self.shared.job,
                self.shared.index,
            )))
            .open(&mut open)
            .default_size([snapshot.width, snapshot.height])
            .min_size([280., 160.])
            .max_size([2048., 1536.])
            .resizable(true)
            .collapsible(false);
        if let Some(size) = self.completed_resize(ctx) {
            // Public Resize sizing persists the desired size. Fix it for this
            // one frame only, then restore the normal resizable Window builder.
            window = window.fixed_size(size);
        }
        let window_response = window.show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.weak("Plugin panel")
                    .on_hover_text(self.shared.plugin.as_ref());
                if ui.button("Cancel plugin").clicked() {
                    self.shared.fail(api::CANCELED);
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("content")
                .auto_shrink([false, false])
                .show(ui, |ui| self.node(ui, &snapshot.root));
        });
        self.panel_rect = window_response.as_ref().map(|r| r.response.rect);
        self.panel_layer = window_response.as_ref().map(|r| r.response.layer_id);
        // A retained canvas may still exist in a hidden tab. Its old pointer
        // capture and rectangle must not survive the visibility transition.
        let hidden = self
            .canvases
            .iter()
            .filter(|(id, _)| !self.visible_canvases.contains(*id))
            .map(|(id, c)| (id.clone(), c.last.clone(), c.buttons != 0))
            .collect::<Vec<_>>();
        for (id, pointer, captured) in hidden {
            if let Some(c) = self.canvases.get_mut(&id) {
                c.buttons = 0;
                c.absolute = None;
                c.rect = None;
                c.last = None;
            }
            if captured {
                self.emit("pointer_cancel", &id, InputValue::None, pointer);
            }
        }
        if !open {
            self.shared.fail(api::CANCELED);
        }
        self.images.retain(|id, _| self.visible_images.contains(id));
        *upload_pixels = self.upload_pixels;
    }
    fn node(&mut self, ui: &mut egui::Ui, node: &Node) {
        ui.push_id(&node.id, |ui| {
            ui.add_enabled_ui(node.enabled, |ui| {
                let response = match &node.kind {
                    Kind::Column(nodes) => {
                        ui.vertical(|ui| {
                            for n in nodes {
                                self.node(ui, n);
                            }
                        })
                        .response
                    }
                    Kind::Row(nodes) => {
                        ui.horizontal_wrapped(|ui| {
                            for n in nodes {
                                self.node(ui, n);
                            }
                        })
                        .response
                    }
                    Kind::Group(nodes) => {
                        ui.group(|ui| {
                            if !node.label.is_empty() {
                                ui.strong(&node.label);
                            }
                            for n in nodes {
                                self.node(ui, n);
                            }
                        })
                        .response
                    }
                    Kind::Tabs { children, .. } => {
                        let mut selected = match self.values.get(&node.id) {
                            Some(InputValue::Choice(i)) => *i,
                            _ => 0,
                        };
                        let old = selected;
                        let response = ui
                            .vertical(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for (i, child) in children.iter().enumerate() {
                                        let r = ui
                                            .add_enabled_ui(child.enabled, |ui| {
                                                ui.selectable_label(selected == i, &child.label)
                                            })
                                            .inner;
                                        #[cfg(test)]
                                        self.widget_rects
                                            .insert(format!("{}:{}", node.id, child.id), r.rect);
                                        if r.clicked() {
                                            selected = i;
                                        }
                                    }
                                });
                                ui.separator();
                                self.node(ui, &children[selected]);
                            })
                            .response;
                        if selected != old {
                            self.changed(&node.id, InputValue::Choice(selected));
                        }
                        response
                    }
                    Kind::Label => ui.label(&node.label),
                    Kind::Button => {
                        let r = ui.button(&node.label);
                        if r.clicked() {
                            self.emit("activate", &node.id, InputValue::None, None);
                        }
                        r
                    }
                    Kind::Toggle(_) => {
                        let mut value =
                            matches!(self.values.get(&node.id), Some(InputValue::Bool(true)));
                        let r = ui.checkbox(&mut value, &node.label);
                        if r.changed() {
                            self.changed(&node.id, InputValue::Bool(value));
                        }
                        r
                    }
                    Kind::Color(_) => {
                        let value = match self.values.get(&node.id) {
                            Some(InputValue::Color(c)) => *c,
                            _ => [255; 4],
                        };
                        let mut color = picker_color(value);
                        let r = ui
                            .horizontal(|ui| {
                                ui.label(&node.label);
                                egui::color_picker::color_edit_button_hsva(
                                    ui,
                                    &mut color,
                                    egui::color_picker::Alpha::OnlyBlend,
                                )
                            })
                            .inner;
                        if r.changed() {
                            self.changed(
                                &node.id,
                                InputValue::Color(color.to_srgba_unmultiplied()),
                            );
                        }
                        r
                    }
                    Kind::Tree { items, height, .. } => self.tree(ui, node, items, *height),
                    Kind::Number {
                        min,
                        max,
                        step,
                        slider,
                        ..
                    } => {
                        let mut value = match self.values.get(&node.id) {
                            Some(InputValue::Number(v)) => *v,
                            _ => *min,
                        };
                        let r = if *slider {
                            ui.add(
                                egui::Slider::new(&mut value, *min..=*max)
                                    .step_by(*step)
                                    .text(&node.label),
                            )
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(&node.label);
                                ui.add(
                                    egui::DragValue::new(&mut value)
                                        .range(*min..=*max)
                                        .speed(*step),
                                )
                            })
                            .inner
                        };
                        if r.changed() && value.is_finite() {
                            self.changed(&node.id, InputValue::Number(value.clamp(*min, *max)));
                        }
                        r
                    }
                    Kind::Text(_) => {
                        let mut value = match self.values.get(&node.id) {
                            Some(InputValue::Text(v)) => v.clone(),
                            _ => String::new(),
                        };
                        let r = ui
                            .horizontal(|ui| {
                                ui.label(&node.label);
                                ui.add(
                                    egui::TextEdit::singleline(&mut value)
                                        .char_limit(4096)
                                        .desired_width(ui.available_width().max(80.)),
                                )
                            })
                            .inner;
                        if r.changed() {
                            let mut end = value.len().min(4096);
                            while !value.is_char_boundary(end) {
                                end -= 1;
                            }
                            value.truncate(end);
                            self.changed(&node.id, InputValue::Text(value));
                        }
                        r
                    }
                    Kind::Choice { choices, .. } => {
                        let mut value = match self.values.get(&node.id) {
                            Some(InputValue::Choice(v)) => *v,
                            _ => 0,
                        };
                        let old = value;
                        let r = ui
                            .horizontal(|ui| {
                                ui.label(&node.label);
                                egui::ComboBox::from_id_salt("choice")
                                    .selected_text(&choices[value])
                                    .show_ui(ui, |ui| {
                                        for (i, text) in choices.iter().enumerate() {
                                            ui.selectable_value(&mut value, i, text);
                                        }
                                    })
                                    .response
                            })
                            .inner;
                        if value != old {
                            self.changed(&node.id, InputValue::Choice(value));
                        }
                        r
                    }
                    Kind::List {
                        choices, height, ..
                    } => {
                        let mut value = match self.values.get(&node.id) {
                            Some(InputValue::Choice(i)) => *i,
                            _ => 0,
                        };
                        let old = value;
                        let response = ui
                            .vertical(|ui| {
                                if !node.label.is_empty() {
                                    ui.label(&node.label);
                                }
                                let row_height = ui
                                    .text_style_height(&egui::TextStyle::Body)
                                    .max(ui.spacing().interact_size.y);
                                egui::ScrollArea::vertical()
                                    .id_salt("list")
                                    .max_height(*height)
                                    .auto_shrink([false, false])
                                    .show_rows(ui, row_height, choices.len(), |ui, range| {
                                        for i in range {
                                            let r = ui
                                                .push_id(i, |ui| {
                                                    ui.add(
                                                        egui::Button::selectable(
                                                            value == i,
                                                            &choices[i],
                                                        )
                                                        .truncate(),
                                                    )
                                                    .on_hover_text(&choices[i])
                                                })
                                                .inner;
                                            #[cfg(test)]
                                            self.widget_rects
                                                .insert(format!("{}:{}", node.id, i + 1), r.rect);
                                            if r.clicked() {
                                                value = i;
                                            }
                                        }
                                    });
                            })
                            .response;
                        if value != old {
                            self.changed(&node.id, InputValue::Choice(value));
                        }
                        response
                    }
                    Kind::Canvas {
                        height,
                        interactive,
                        primitives,
                    } => self.canvas(
                        ui,
                        node,
                        *height,
                        *interactive && ui.is_enabled(),
                        primitives,
                    ),
                };
                #[cfg(test)]
                self.widget_rects.insert(node.id.clone(), response.rect);
                if !node.tip.is_empty() {
                    response.on_hover_text(&node.tip);
                }
            });
        });
    }
    fn tree(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        items: &[TreeItem],
        height: f32,
    ) -> egui::Response {
        fn visible<'a>(
            items: &'a [TreeItem],
            expanded: &BTreeSet<String>,
            depth: usize,
            enabled: bool,
            out: &mut Vec<(usize, bool, &'a TreeItem)>,
        ) {
            for item in items {
                let enabled = enabled && item.enabled;
                out.push((depth, enabled, item));
                if expanded.contains(&item.id) {
                    visible(&item.children, expanded, depth + 1, enabled, out);
                }
            }
        }
        let mut expanded = self.trees.remove(&node.id).unwrap_or_default();
        let mut rows = Vec::new();
        visible(items, &expanded, 0, true, &mut rows);
        let mut selected = match self.values.get(&node.id) {
            Some(InputValue::Text(s)) => s.clone(),
            _ => String::new(),
        };
        let old = selected.clone();
        let row_height = ui
            .text_style_height(&egui::TextStyle::Body)
            .max(ui.spacing().interact_size.y);
        let response = ui
            .vertical(|ui| {
                if !node.label.is_empty() {
                    ui.label(&node.label);
                }
                egui::ScrollArea::vertical()
                    .id_salt("tree")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, rows.len(), |ui, range| {
                        for row in range {
                            let (depth, enabled, item) = rows[row];
                            ui.push_id(&item.id, |ui| {
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_space(depth as f32 * 16.0);
                                        if !item.children.is_empty() {
                                            let open = expanded.contains(&item.id);
                                            let r = ui.add(egui::Button::new("").min_size(
                                                Vec2::splat(ui.spacing().interact_size.y),
                                            ));
                                            egui::collapsing_header::paint_default_icon(
                                                ui,
                                                if open { 1.0 } else { 0.0 },
                                                &r,
                                            );
                                            r.widget_info(|| {
                                                egui::WidgetInfo::labeled(
                                                    egui::WidgetType::Button,
                                                    ui.is_enabled(),
                                                    if open {
                                                        "Collapse branch"
                                                    } else {
                                                        "Expand branch"
                                                    },
                                                )
                                            });
                                            #[cfg(test)]
                                            self.widget_rects.insert(
                                                format!("{}:expand:{}", node.id, item.id),
                                                r.rect,
                                            );
                                            if r.clicked() {
                                                if open {
                                                    expanded.remove(&item.id);
                                                } else {
                                                    expanded.insert(item.id.clone());
                                                }
                                                self.emit(
                                                    "expand",
                                                    &node.id,
                                                    InputValue::Expansion {
                                                        item: item.id.clone(),
                                                        expanded: !open,
                                                    },
                                                    None,
                                                );
                                            }
                                        } else {
                                            ui.add_space(ui.spacing().interact_size.y);
                                        }
                                        let r = ui.add(
                                            egui::Button::selectable(
                                                selected == item.id,
                                                &item.label,
                                            )
                                            .truncate(),
                                        );
                                        #[cfg(test)]
                                        self.widget_rects.insert(
                                            format!("{}:item:{}", node.id, item.id),
                                            r.rect,
                                        );
                                        if r.clicked() {
                                            selected = item.id.clone();
                                        }
                                        r.on_hover_text(if item.tip.is_empty() {
                                            &item.label
                                        } else {
                                            &item.tip
                                        });
                                    });
                                });
                            });
                        }
                    });
            })
            .response;
        self.trees.insert(node.id.clone(), expanded);
        if selected != old {
            self.changed(&node.id, InputValue::Text(selected));
        }
        response
    }
    fn changed(&mut self, id: &str, value: InputValue) {
        self.values.insert(id.into(), value.clone());
        self.emit("change", id, value, None);
    }
    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        height: f32,
        interactive: bool,
        primitives: &[Primitive],
    ) -> egui::Response {
        self.visible_canvases.insert(node.id.clone());
        let size = Vec2::new(ui.available_width().clamp(64., 2048.), height);
        let (rect, response) = ui.allocate_exact_size(
            size,
            if interactive {
                Sense::drag()
            } else {
                Sense::hover()
            },
        );
        let painter = ui.painter_at(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, 0., ui.visuals().extreme_bg_color);
        let shapes = super::canvas::display_list(
            ui,
            &painter,
            rect,
            primitives,
            super::canvas::Images {
                cached: &mut self.images,
                visible: &mut self.visible_images,
                upload_pixels: &mut self.upload_pixels,
            },
        );
        painter.extend(shapes);
        let current_size = [f64::from(rect.width()), f64::from(rect.height())];
        let mut state = self.canvases.remove(&node.id).unwrap_or_default();
        if state.size != current_size {
            state.size = current_size;
            self.emit(
                "resize",
                &node.id,
                InputValue::None,
                Some(Pointer {
                    x: 0.,
                    y: 0.,
                    width: current_size[0],
                    height: current_size[1],
                    buttons: 0,
                    shift: false,
                    ctrl: false,
                    alt: false,
                    command: false,
                    scroll_x: 0.,
                    scroll_y: 0.,
                    scroll_unit: "points",
                    time: 0.,
                }),
            );
        }
        state.rect = Some(rect);
        if interactive {
            // Ordered native samples, bounded before cloning; no per-OS-event Lua.
            let (events, mut modifiers, time, focused, latest) = ui.input(|i| {
                (
                    i.events.iter().take(257).cloned().collect::<Vec<_>>(),
                    i.modifiers,
                    i.time,
                    i.focused,
                    i.pointer.latest_pos(),
                )
            });
            let hit = |p: Pos2| {
                rect.intersect(ui.clip_rect()).contains(p)
                    && ui.ctx().layer_id_at(p) == Some(ui.layer_id())
            };
            if events.len() > 256 && (state.buttons != 0 || latest.is_some_and(hit)) {
                self.shared.fail(api::OVERFLOW);
            }
            if !focused && state.buttons != 0 {
                state.buttons = 0;
                self.emit(
                    "pointer_cancel",
                    &node.id,
                    InputValue::None,
                    state.last.clone(),
                );
            }
            let mut position = state.absolute.or(latest);
            for event in events.into_iter().take(256) {
                if !focused {
                    break;
                }
                let make = |pos: Pos2, buttons: u8, modifiers: egui::Modifiers| Pointer {
                    x: f64::from(pos.x - rect.left()) / current_size[0],
                    y: f64::from(pos.y - rect.top()) / current_size[1],
                    width: current_size[0],
                    height: current_size[1],
                    buttons,
                    shift: modifiers.shift,
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    command: modifiers.command,
                    scroll_x: 0.,
                    scroll_y: 0.,
                    scroll_unit: "points",
                    time,
                };
                match event {
                    egui::Event::PointerMoved(pos) => {
                        position = Some(pos);
                        if state.buttons != 0 || hit(pos) {
                            let p = make(pos, state.buttons, modifiers);
                            self.emit("pointer_move", &node.id, InputValue::None, Some(p.clone()));
                            state.last = Some(p);
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: mods,
                    } => {
                        position = Some(pos);
                        modifiers = mods;
                        let bit = match button {
                            egui::PointerButton::Primary => 1,
                            egui::PointerButton::Secondary => 2,
                            egui::PointerButton::Middle => 4,
                            _ => 0,
                        };
                        if bit != 0 && pressed && hit(pos) {
                            state.buttons |= bit;
                            let p = make(pos, state.buttons, modifiers);
                            self.emit("pointer_down", &node.id, InputValue::None, Some(p.clone()));
                            state.last = Some(p);
                        } else if bit != 0 && !pressed && state.buttons & bit != 0 {
                            state.buttons &= !bit;
                            let p = make(pos, state.buttons, modifiers);
                            self.emit("pointer_up", &node.id, InputValue::None, Some(p.clone()));
                            state.last = Some(p);
                        }
                    }
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers: mods,
                        ..
                    } => {
                        modifiers = mods;
                        if let Some(pos) = position
                            && hit(pos)
                        {
                            let mut p = make(pos, state.buttons, modifiers);
                            p.scroll_x = f64::from(delta.x);
                            p.scroll_y = f64::from(delta.y);
                            p.scroll_unit = match unit {
                                egui::MouseWheelUnit::Point => "points",
                                egui::MouseWheelUnit::Line => "lines",
                                egui::MouseWheelUnit::Page => "pages",
                            };
                            self.emit("scroll", &node.id, InputValue::None, Some(p));
                            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
                        }
                    }
                    egui::Event::PointerGone | egui::Event::WindowFocused(false) => {
                        if state.buttons != 0 {
                            state.buttons = 0;
                            self.emit(
                                "pointer_cancel",
                                &node.id,
                                InputValue::None,
                                state.last.clone(),
                            );
                        }
                        position = None;
                    }
                    egui::Event::Key {
                        modifiers: mods, ..
                    } => modifiers = mods,
                    _ => {}
                }
            }
            state.absolute = position;
        } else if state.buttons != 0 {
            state.buttons = 0;
            self.emit(
                "pointer_cancel",
                &node.id,
                InputValue::None,
                state.last.clone(),
            );
        }
        self.canvases.insert(node.id.clone(), state);
        response
    }
}
impl App {
    pub(super) fn lua_panel_windows(&mut self, ctx: &egui::Context) {
        self.reset_lua_upload_budget(ctx);
        let current = self.open.as_ref().map(|o| ProjectStamp {
            serial: o.history.incarnations().project(),
            revision: o.history.current_state(),
        });
        let permitted = |plugin: &str| {
            let prefs = &self.settings.lua_plugins;
            let state = prefs.installed.get(plugin);
            prefs.run_plugins
                && state.is_some_and(|s| s.enabled)
                && self.lua_plugins.as_ref().is_some_and(|m| {
                    m.plugins().iter().any(|p| {
                        p.id == plugin && p.effective(state).contains(&Capability::UiPanels)
                    })
                })
        };
        self.lua_panels.retain(|v| {
            if !v.shared.closed() {
                if !permitted(&v.shared.plugin) {
                    v.shared.fail(api::DENIED);
                } else if let Some(reason) = v.shared.invalid_reason(current) {
                    v.shared.fail(reason);
                }
            }
            !v.shared.closed()
        });
        // One bounded request per active worker; cap the work done in this frame.
        for _ in 0..16 {
            let Some(request) = self
                .lua_plugins
                .as_ref()
                .and_then(|m| m.poll_panel_request())
            else {
                break;
            };
            if !permitted(&request.plugin) {
                request.fail(api::DENIED);
            } else if let Some(reason) = request.invalid_reason(current) {
                request.fail(reason);
            } else if self.lua_panels.len() == MAX_FRONTEND_PANELS {
                request.fail(api::OVERFLOW);
            } else if let Some(view) = View::new(Arc::clone(&request)) {
                self.lua_panels.push(view);
            } else {
                request.fail(api::EXPIRED);
            }
        }
        for view in &mut self.lua_panels {
            view.show(ctx, &mut self.lua_upload_pixels);
        }
        if !self.lua_panels.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
#[path = "lua_font_picker_app_tests.rs"]
pub(super) mod font_picker_tests;

#[cfg(test)]
mod tests {
    use super::*;
    const SPEC: &str = "{title='Native Fixture',width=640,height=480,content={id='root',kind='column',children={{id='toggle',kind='toggle',label='Enabled'},{id='text',kind='text',label='Text',value='hello'},{id='graph',kind='canvas',height=160,primitives={{kind='line',x1=0,y1=0,x2=1,y2=1,color={255,0,0,255}},{kind='circle',x=0.5,y=0.5,radius=6,fill=true},{kind='rect',x1=0.1,y1=0.1,x2=0.2,y2=0.2,fill=true},{kind='text',x=0,y=0,text='axis'}}}}}}";
    fn frame(
        ctx: &egui::Context,
        view: &mut View,
        events: Vec<egui::Event>,
        time: f64,
        width: f32,
        ppp: f32,
        discard: bool,
    ) -> egui::FullOutput {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 800.))),
            events,
            time: Some(time),
            focused: true,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(ppp);
        let mut output = ctx.run_ui(input, |root| {
            let mut upload_pixels = api::images::MAX_UPLOAD_PIXELS;
            view.show(root.ctx(), &mut upload_pixels);
            if discard && root.ctx().current_pass_index() == 0 {
                root.ctx().request_discard("test multipass layout");
            }
        });
        // Headless CPU integration intentionally has no GPU texture consumer.
        output.textures_delta.clear();
        output
    }
    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }
    fn warm(ctx: &egui::Context, view: &mut View) {
        for i in 0..3 {
            let _ = frame(ctx, view, vec![], i as f64 * 0.02, 1200., 1., false);
        }
    }
    #[test]
    fn headless_canvas_keeps_ordered_down_move_up_positions_capture_and_clipping() {
        crate::lua::ui::native_panel(SPEC, |shared| {
            let ctx = egui::Context::default();
            let mut v = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut v);
            shared.test_events();
            let r = v.canvases["graph"].rect.unwrap();
            let down = r.lerp_inside(Vec2::new(0.25, 0.75));
            let end = r.lerp_inside(Vec2::new(1.25, 1.25));
            let output = frame(
                &ctx,
                &mut v,
                vec![
                    egui::Event::PointerMoved(down),
                    button(down, true),
                    egui::Event::PointerMoved(end),
                    button(end, false),
                ],
                0.1,
                1200.,
                1.,
                true,
            );
            let events = shared.test_events();
            let downs = events
                .iter()
                .filter(|e| e.kind == "pointer_down")
                .collect::<Vec<_>>();
            let ups = events
                .iter()
                .filter(|e| e.kind == "pointer_up")
                .collect::<Vec<_>>();
            assert_eq!(
                downs.len(),
                1,
                "one press despite ordered rapid input and a forced layout pass"
            );
            assert_eq!(ups.len(), 1);
            let p = downs[0].pointer.as_ref().unwrap();
            assert!((p.x - 0.25).abs() < 1e-6 && (p.y - 0.75).abs() < 1e-6 && p.buttons == 1);
            let p = ups[0].pointer.as_ref().unwrap();
            assert!((p.x - 1.25).abs() < 1e-6 && (p.y - 1.25).abs() < 1e-6 && p.buttons == 0);
            assert!(downs[0].sequence < ups[0].sequence);
            assert_eq!(v.canvases["graph"].buttons, 0);
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            let vertices = primitives
                .iter()
                .map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                    _ => 0,
                })
                .sum::<usize>();
            assert!(vertices > 0 && vertices < 100000);
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(end)],
                0.12,
                1200.,
                1.,
                false,
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .all(|e| e.kind != "pointer_move")
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn headless_controls_deliver_typed_toggle_text_and_native_canvas_units() {
        crate::lua::ui::native_panel(SPEC, |shared| {
            let ctx = egui::Context::default();
            let mut v = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut v);
            shared.test_events();
            let pos = v.widget_rects["toggle"].center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(pos), button(pos, true)],
                0.10,
                1200.,
                1.,
                false,
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![button(pos, false)],
                0.12,
                1200.,
                1.,
                false,
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| e.id == "toggle" && e.value == InputValue::Bool(true))
            );
            let pos = v.widget_rects["text"].center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(pos), button(pos, true)],
                0.14,
                1200.,
                1.,
                false,
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![button(pos, false), egui::Event::Text("é".into())],
                0.16,
                1200.,
                1.,
                false,
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| matches!(&e.value,InputValue::Text(s) if s.contains('é')))
            );
            let pos = v.canvases["graph"].rect.unwrap().center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: Vec2::new(0., 2.),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                0.18,
                1200.,
                1.,
                false,
            );
            let scroll = shared
                .test_events()
                .into_iter()
                .find(|e| e.kind == "scroll")
                .unwrap()
                .pointer
                .unwrap();
            assert_eq!(scroll.scroll_unit, "lines");
            assert_eq!(scroll.scroll_y, 2.);
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn headless_narrow_viewport_and_dpi_keep_normalized_coordinates_and_resize_events() {
        crate::lua::ui::native_panel(SPEC, |shared| {
            let ctx = egui::Context::default();
            let mut v = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut v);
            shared.test_events();
            let old = v.canvases["graph"].size;
            for i in 0..4 {
                let _ = frame(&ctx, &mut v, vec![], 0.1 + i as f64 * 0.02, 420., 2., false);
            }
            let new = v.canvases["graph"].size;
            assert!(new[0] < old[0] && new[0] >= 64.);
            assert!(shared.test_events().iter().any(|e| e.kind == "resize"));
            let r = v.canvases["graph"].rect.unwrap();
            let pos = r.lerp_inside(Vec2::new(0.75, 0.25));
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(pos), button(pos, true)],
                0.2,
                420.,
                2.,
                false,
            );
            let event = shared
                .test_events()
                .into_iter()
                .find(|e| e.kind == "pointer_down")
                .unwrap();
            let p = event.pointer.unwrap();
            assert!((p.x - 0.75).abs() < 1e-6 && (p.y - 0.25).abs() < 1e-6 && p.width == new[0]);
            Ok(())
        })
        .unwrap();
    }
    const LAYOUT_SPEC: &str = r#"(function()
      local choices={};for i=1,512 do choices[i]='Item '..i end
      return {title='Layout fixture',width=640,height=520,content={id='tabs',kind='tabs',children={
        {id='graph_tab',kind='group',label='Graph',children={{id='graph',kind='canvas',height=160,primitives={}}}},
        {id='list_tab',kind='group',label='Data',children={{id='data',kind='list',height=96,choices=choices}}},
        {id='disabled_tab',kind='group',label='Disabled',enabled=false,children={}},
      }}}
    end)()"#;
    #[test]
    fn tabs_deliver_native_selection_and_hidden_canvases_cancel_capture() {
        crate::lua::ui::native_panel(LAYOUT_SPEC, |shared| {
            let ctx = egui::Context::default();
            let mut v = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut v);
            shared.test_events();
            let graph = v.canvases["graph"].rect.unwrap();
            let pos = graph.center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(pos), button(pos, true)],
                0.1,
                1200.,
                1.,
                false,
            );
            assert_eq!(v.canvases["graph"].buttons, 1);
            shared.test_events();
            // The same authoritative value replacement used by a worker snapshot
            // hides the captured canvas before the physical release arrives.
            v.values.insert("tabs".into(), InputValue::Choice(1));
            let _ = frame(&ctx, &mut v, vec![], 0.12, 1200., 1., false);
            assert_eq!(v.canvases["graph"].buttons, 0);
            assert!(v.canvases["graph"].rect.is_none());
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| e.kind == "pointer_cancel" && e.id == "graph")
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(pos), button(pos, false)],
                0.14,
                1200.,
                1.,
                false,
            );
            assert!(
                !shared
                    .test_events()
                    .iter()
                    .any(|e| e.id == "graph" && e.kind.starts_with("pointer"))
            );
            let disabled = v.widget_rects["tabs:disabled_tab"].center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(disabled), button(disabled, true)],
                0.16,
                1200.,
                1.,
                false,
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![button(disabled, false)],
                0.18,
                1200.,
                1.,
                false,
            );
            assert!(
                !shared
                    .test_events()
                    .iter()
                    .any(|e| e.kind == "change" && e.id == "tabs")
            );
            let tab = v.widget_rects["tabs:graph_tab"].center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(tab), button(tab, true)],
                0.2,
                1200.,
                1.,
                false,
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![button(tab, false)],
                0.22,
                1200.,
                1.,
                false,
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| e.id == "tabs" && matches!(e.value, InputValue::Choice(0)))
            );
            assert!(v.canvases["graph"].rect.is_some());
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn virtualized_list_delivers_typed_rows_and_bounds_rendering_while_scrolling() {
        crate::lua::ui::native_panel(LAYOUT_SPEC, |shared| {
            let ctx = egui::Context::default();
            let mut v = View::new(Arc::clone(shared)).unwrap();
            v.values.insert("tabs".into(), InputValue::Choice(1));
            warm(&ctx, &mut v);
            shared.test_events();
            let rows = |v: &View| {
                v.widget_rects
                    .keys()
                    .filter(|k| k.starts_with("data:"))
                    .count()
            };
            assert!(
                rows(&v) > 0 && rows(&v) < 16,
                "only viewport rows, not512widgets"
            );
            let item = v.widget_rects["data:3"].center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(item), button(item, true)],
                0.1,
                1200.,
                1.,
                false,
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![button(item, false)],
                0.12,
                1200.,
                1.,
                false,
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| e.id == "data" && matches!(e.value, InputValue::Choice(2)))
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![
                    egui::Event::PointerMoved(item),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0., -1000000.),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                0.14,
                1200.,
                1.,
                false,
            );
            for i in 0..20 {
                let _ = frame(
                    &ctx,
                    &mut v,
                    vec![],
                    0.16 + i as f64 * 0.02,
                    1200.,
                    1.,
                    false,
                );
                assert!(rows(&v) < 16);
            }
            assert!(!v.widget_rects.contains_key("data:1"));
            assert!(
                v.widget_rects.contains_key("data:512"),
                "last bounded item is reachable"
            );
            let last = v.widget_rects["data:512"].center();
            let _ = frame(
                &ctx,
                &mut v,
                vec![egui::Event::PointerMoved(last), button(last, true)],
                0.6,
                1200.,
                1.,
                false,
            );
            let _ = frame(
                &ctx,
                &mut v,
                vec![button(last, false)],
                0.62,
                1200.,
                1.,
                false,
            );
            assert!(
                !shared.closed(),
                "last list row must pass the typed event validator"
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| e.id == "data" && matches!(e.value, InputValue::Choice(511)))
            );
            Ok(())
        })
        .unwrap();
    }
    const IMAGE_SPEC: &str = "{title='Image fixture',content={id='canvas',kind='canvas',height=180,primitives={{kind='image',image=plum.ui.image(plum.images.from_rgba(2,2,string.char(255,0,0,255,0,255,0,128,0,0,255,0,10,20,30,255))),x1=-.5,y1=-.5,x2=1.5,y2=1.5,u1=.25,v1=.25,u2=.75,v2=.75,color={255,128,255,128}},{kind='line',x1=0,y1=0,x2=1,y2=1,color={255,0,0,255}}}}}";
    #[test]
    fn native_canvas_image_mesh_uv_tint_clipping_texture_reuse_and_close_are_real() {
        crate::lua::ui::native_panel(IMAGE_SPEC, |shared| {
            let snapshot=shared.snapshot().unwrap();
            let resource=match &snapshot.root.kind {Kind::Canvas{primitives,..}=>match &primitives[0]{Primitive::Image{resource,..}=>Arc::clone(resource),_=>panic!()},_=>panic!()};
            let ctx=egui::Context::default();
            ctx.all_styles_mut(|s|s.animation_time=0.0);
            let mut first=View::new(Arc::clone(shared)).unwrap();warm(&ctx,&mut first);
            assert_eq!(first.images.len(),1);assert_eq!(resource.test_uploads(),1);
            let id=first.images[&resource.id].texture.id();
            let output=frame(&ctx,&mut first,vec![],0.1,1200.,1.,false);
            let canvas=first.canvases["canvas"].rect.unwrap();
            let meshes=ctx.tessellate(output.shapes,output.pixels_per_point);
            let mesh=meshes.iter().find(|p|matches!(&p.primitive,egui::epaint::Primitive::Mesh(m) if m.texture_id==id)).expect("actual textured canvas mesh");
            assert!(canvas.expand(1.).contains_rect(mesh.clip_rect),"texture is clipped to the canvas/viewport");
            let egui::epaint::Primitive::Mesh(m)=&mesh.primitive else{panic!()};
            let tint=Color32::from_rgba_unmultiplied(255,128,255,128);
            assert!(m.vertices.iter().all(|v|v.color==tint),"actual tint: {:?}, expected {:?}",m.vertices.iter().map(|v|v.color).collect::<Vec<_>>(),tint);
            assert!(m.vertices.iter().all(|v|(0.25..=0.75).contains(&v.uv.x)&&(0.25..=0.75).contains(&v.uv.y)));
            assert!(m.vertices.iter().any(|v|v.uv.x==0.25)&&m.vertices.iter().any(|v|v.uv.x==0.75));
            let mut second=View::new(Arc::clone(shared)).unwrap();warm(&ctx,&mut second);
            assert_eq!(second.images[&resource.id].texture.id(),id);assert_eq!(resource.test_uploads(),1);
            resource.test_close();
            let output=frame(&ctx,&mut first,vec![],0.2,1200.,1.,false);
            assert!(first.images.is_empty());
            assert!(!ctx.tessellate(output.shapes,output.pixels_per_point).iter().any(|p|matches!(&p.primitive,egui::epaint::Primitive::Mesh(m) if m.texture_id==id)));
            frame(&ctx,&mut second,vec![],0.22,1200.,1.,false);assert!(second.images.is_empty());
            Ok(())
        }).unwrap();
    }
    #[test]
    fn offscreen_image_does_not_allocate_a_texture_or_consume_upload_budget() {
        let spec = IMAGE_SPEC.replace("x1=-.5,y1=-.5,x2=1.5,y2=1.5", "x1=2,y1=0,x2=3,y2=1");
        crate::lua::ui::native_panel(&spec, |shared| {
            let ctx = egui::Context::default();
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            assert!(view.images.is_empty());
            assert_eq!(view.upload_pixels, api::images::MAX_UPLOAD_PIXELS);
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn coalesced_corner_drag_keeps_its_final_size() {
        for ppp in [1.0, 1.5] {
            crate::lua::ui::native_panel(SPEC, move |shared| {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|s| s.animation_time = 0.0);
                let mut view = View::new(Arc::clone(shared)).unwrap();
                for i in 0..3 {
                    frame(&ctx, &mut view, vec![], i as f64 * 0.02, 1500., ppp, false);
                }
                let before = view.panel_rect.unwrap();
                let before_canvas = view.canvases["graph"].rect.unwrap();
                let source = Arc::clone(&view.snapshot);
                let start = before.right_bottom() - egui::vec2(3., 3.);
                let end = start + egui::vec2(190., 90.);
                frame(
                    &ctx,
                    &mut view,
                    vec![egui::Event::PointerMoved(start)],
                    0.1,
                    1500.,
                    ppp,
                    false,
                );
                // A responsive native adapter must preserve a drag completed
                // before the next paint, just as the custom canvas already does.
                frame(
                    &ctx,
                    &mut view,
                    vec![
                        button(start, true),
                        egui::Event::PointerMoved(end),
                        button(end, false),
                    ],
                    0.12,
                    1500.,
                    ppp,
                    false,
                );
                for i in 0..20 {
                    frame(
                        &ctx,
                        &mut view,
                        vec![],
                        0.14 + i as f64 * 0.02,
                        1500.,
                        ppp,
                        false,
                    );
                }
                let after = view.panel_rect.unwrap();
                assert!(
                    after.width() > before.width() + 100.,
                    "completed resize lost: {before:?} -> {after:?}, ppp{ppp}"
                );
                let canvas = view.canvases["graph"].rect.unwrap();
                assert!(canvas.width() > before_canvas.width() + 100.);
                assert!(shared.test_events().iter().any(|e| {
                    e.kind == "resize"
                        && e.id == "graph"
                        && e.pointer
                            .as_ref()
                            .is_some_and(|p| (p.width - f64::from(canvas.width())).abs() < 0.01)
                }));
                assert!(
                    Arc::ptr_eq(&source, &view.snapshot),
                    "Resize must not change author data"
                );
                let pos = canvas.min + egui::vec2(canvas.width() * 0.23, canvas.height() * 0.67);
                frame(
                    &ctx,
                    &mut view,
                    vec![
                        egui::Event::PointerMoved(pos),
                        button(pos, true),
                        button(pos, false),
                    ],
                    0.7,
                    1500.,
                    ppp,
                    false,
                );
                let event = shared
                    .test_events()
                    .into_iter()
                    .rev()
                    .find(|e| e.kind == "pointer_down" && e.id == "graph")
                    .unwrap();
                let p = event.pointer.unwrap();
                assert!((p.x - 0.23).abs() < 1e-6 && (p.y - 0.67).abs() < 1e-6);
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn coalesced_resize_keeps_exact_outer_size_through_layout_discard() {
        crate::lua::ui::native_panel(SPEC, |shared| {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            let before = view.panel_rect.unwrap();
            let start = before.right_bottom() - egui::vec2(3., 3.);
            let end = start + egui::vec2(5., 4.);
            frame(
                &ctx,
                &mut view,
                vec![egui::Event::PointerMoved(start)],
                0.1,
                1500.,
                1.,
                false,
            );
            frame(
                &ctx,
                &mut view,
                vec![
                    button(start, true),
                    egui::Event::PointerMoved(end),
                    button(end, false),
                ],
                0.12,
                1500.,
                1.,
                true,
            );
            for i in 0..3 {
                frame(
                    &ctx,
                    &mut view,
                    vec![],
                    0.14 + i as f64 * 0.02,
                    1500.,
                    1.,
                    false,
                );
            }
            let after = view.panel_rect.unwrap();
            assert!(
                (after.width() - before.width() - 5.).abs() < 1.,
                "Completed resize changed its requested outer size: {before:?} -> {after:?}"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn corner_resize_survives_press_move_and_release_in_different_frame_partitions() {
        for split in [0, 1, 2, 3] {
            crate::lua::ui::native_panel(SPEC, move |shared| {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|s| s.animation_time = 0.0);
                let mut view = View::new(Arc::clone(shared)).unwrap();
                warm(&ctx, &mut view);
                let before = view.panel_rect.unwrap();
                let start = before.right_bottom() - egui::vec2(3., 3.);
                let end = start + egui::vec2(190., 90.);
                let press = vec![egui::Event::PointerMoved(start), button(start, true)];
                let moves = vec![egui::Event::PointerMoved(end)];
                let release = vec![button(end, false)];
                let frames = match split {
                    0 => vec![[press, moves, release].concat()],
                    1 => vec![[press, moves].concat(), release],
                    2 => vec![press, [moves, release].concat()],
                    _ => vec![press, moves, release],
                };
                for (i, events) in frames.into_iter().enumerate() {
                    frame(
                        &ctx,
                        &mut view,
                        events,
                        0.1 + i as f64 * 0.02,
                        1500.,
                        1.,
                        false,
                    );
                }
                for i in 0..3 {
                    frame(
                        &ctx,
                        &mut view,
                        vec![],
                        0.3 + i as f64 * 0.02,
                        1500.,
                        1.,
                        false,
                    );
                }
                let after = view.panel_rect.unwrap();
                assert!(
                    (after.width() - before.width() - 190.).abs() < 1.,
                    "Lost resize in frame partition{split}: {before:?} -> {after:?}"
                );
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn coalesced_corner_resize_can_grow_then_shrink_without_changing_author_data() {
        crate::lua::ui::native_panel(SPEC, |shared| {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            let before = view.panel_rect.unwrap();
            let source = Arc::clone(&view.snapshot);
            for (i, delta) in [egui::vec2(190., 90.), egui::vec2(-100., -50.)]
                .into_iter()
                .enumerate()
            {
                let start = view.panel_rect.unwrap().right_bottom() - egui::vec2(3., 3.);
                let end = start + delta;
                let time = 0.1 + i as f64 * 0.2;
                frame(
                    &ctx,
                    &mut view,
                    vec![egui::Event::PointerMoved(start)],
                    time,
                    1500.,
                    1.,
                    false,
                );
                frame(
                    &ctx,
                    &mut view,
                    vec![
                        button(start, true),
                        egui::Event::PointerMoved(end),
                        button(end, false),
                    ],
                    time + 0.02,
                    1500.,
                    1.,
                    false,
                );
                for j in 0..3 {
                    frame(
                        &ctx,
                        &mut view,
                        vec![],
                        time + 0.04 + j as f64 * 0.02,
                        1500.,
                        1.,
                        false,
                    );
                }
            }
            let after = view.panel_rect.unwrap();
            assert!(
                (after.width() - before.width() - 90.).abs() < 1.,
                "Grow/shrink width drift: {before:?} -> {after:?}"
            );
            assert!(
                (after.height() - before.height() - 40.).abs() < 1.,
                "Grow/shrink height drift: {before:?} -> {after:?}"
            );
            assert!(Arc::ptr_eq(&source, &view.snapshot));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_foreground_control_covering_the_grip_does_not_resize_the_panel() {
        crate::lua::ui::native_panel(SPEC, |shared| {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            let before = view.panel_rect.unwrap();
            let start = before.right_bottom() - egui::vec2(3., 3.);
            let end = start + egui::vec2(190., 90.);
            for i in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1500., 800.))),
                    events: if i == 2 {
                        vec![
                            button(start, true),
                            egui::Event::PointerMoved(end),
                            button(end, false),
                        ]
                    } else {
                        vec![]
                    },
                    time: Some(0.1 + i as f64 * 0.02),
                    focused: true,
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |root| {
                    let mut upload = api::images::MAX_UPLOAD_PIXELS;
                    view.show(root.ctx(), &mut upload);
                    egui::Area::new(egui::Id::new("cover-native-grip"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(start - egui::vec2(12., 12.))
                        .show(root.ctx(), |ui| {
                            ui.allocate_exact_size(egui::vec2(40., 40.), Sense::click());
                        });
                });
                output.textures_delta.clear();
            }
            assert_eq!(view.panel_rect.unwrap(), before);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn unfinished_corner_capture_is_canceled_on_focus_loss_or_pointer_gone() {
        for gone in [false, true] {
            crate::lua::ui::native_panel(SPEC, move |shared| {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|s| s.animation_time = 0.0);
                let mut view = View::new(Arc::clone(shared)).unwrap();
                warm(&ctx, &mut view);
                let before = view.panel_rect.unwrap();
                let start = before.right_bottom() - egui::vec2(3., 3.);
                frame(
                    &ctx,
                    &mut view,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                    0.1,
                    1500.,
                    1.,
                    false,
                );
                assert!(view.corner_drag.is_some());
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1500., 800.))),
                    events: if gone {
                        vec![egui::Event::PointerGone]
                    } else {
                        vec![]
                    },
                    time: Some(0.12),
                    focused: gone,
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |root| {
                    let mut upload = api::images::MAX_UPLOAD_PIXELS;
                    view.show(root.ctx(), &mut upload);
                });
                output.textures_delta.clear();
                assert!(view.corner_drag.is_none());
                let mut captured = None;
                let input = egui::RawInput {
                    events: vec![button(start + egui::vec2(190., 90.), false)],
                    time: Some(0.14),
                    focused: true,
                    ..Default::default()
                };
                let mut output =
                    ctx.run_ui(input, |root| captured = view.completed_resize(root.ctx()));
                output.textures_delta.clear();
                assert!(
                    captured.is_none(),
                    "A stale release must not finish the canceled resize"
                );
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn completed_resize_requires_focus_matching_grip_and_bounded_complete_input() {
        for case in 0..5 {
            crate::lua::ui::native_panel(SPEC, move |shared| {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|s| s.animation_time = 0.0);
                let mut view = View::new(Arc::clone(shared)).unwrap();
                warm(&ctx, &mut view);
                let start = view.panel_rect.unwrap().right_bottom() - egui::vec2(3., 3.);
                let end = start + egui::vec2(190., 90.);
                let mut events = vec![
                    button(start, true),
                    egui::Event::PointerMoved(end),
                    button(end, false),
                ];
                match case {
                    1 => events[0] = button(start - egui::vec2(100., 100.), true),
                    2 => {
                        events.pop();
                    }
                    3 => {
                        events.extend(std::iter::repeat_n(egui::Event::PointerMoved(end), 257));
                    }
                    4 => events[2] = button(egui::pos2(f32::INFINITY, end.y), false),
                    _ => {}
                }
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1500., 800.))),
                    events,
                    time: Some(0.2),
                    focused: case != 0,
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |root| {
                    assert!(
                        view.completed_resize(root.ctx()).is_none(),
                        "Invalid resize accepted for case{case}"
                    )
                });
                output.textures_delta.clear();
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn native_window_corner_drag_resizes_canvas_and_preserves_normalized_author_coordinates() {
        for ppp in [1.0, 1.5] {
            crate::lua::ui::native_panel(SPEC, move |shared| {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|s| s.animation_time = 0.0);
                let mut view = View::new(Arc::clone(shared)).unwrap();
                for i in 0..3 {
                    frame(&ctx, &mut view, vec![], i as f64 * 0.02, 1500., ppp, false);
                }
                let before = view.panel_rect.unwrap();
                let before_canvas = view.canvases["graph"].rect.unwrap();
                let start = before.right_bottom() - egui::vec2(3., 3.);
                let end = start + egui::vec2(190., 90.);
                frame(
                    &ctx,
                    &mut view,
                    vec![egui::Event::PointerMoved(start)],
                    0.1,
                    1500.,
                    ppp,
                    false,
                );
                frame(
                    &ctx,
                    &mut view,
                    vec![button(start, true)],
                    0.12,
                    1500.,
                    ppp,
                    false,
                );
                frame(
                    &ctx,
                    &mut view,
                    vec![egui::Event::PointerMoved(end)],
                    0.14,
                    1500.,
                    ppp,
                    false,
                );
                frame(
                    &ctx,
                    &mut view,
                    vec![button(end, false)],
                    0.16,
                    1500.,
                    ppp,
                    false,
                );
                frame(&ctx, &mut view, vec![], 0.18, 1500., ppp, false);
                for i in 0..20 {
                    frame(
                        &ctx,
                        &mut view,
                        vec![],
                        0.2 + i as f64 * 0.02,
                        1500.,
                        ppp,
                        false,
                    );
                }
                let after = view.panel_rect.unwrap();
                let canvas = view.canvases["graph"].rect.unwrap();
                assert!(
                    after.width() > before.width() + 100.,
                    "native corner drag failed: {before:?} -> {after:?}, ppp{ppp}"
                );
                assert!(canvas.width() > before_canvas.width() + 100.);
                let events = shared.test_events();
                assert!(events.iter().any(|e| {
                    e.kind == "resize"
                        && e.id == "graph"
                        && e.pointer
                            .as_ref()
                            .is_some_and(|p| (p.width - f64::from(canvas.width())).abs() < 0.01)
                }));
                let pos = canvas.min + egui::vec2(canvas.width() * 0.23, canvas.height() * 0.67);
                frame(
                    &ctx,
                    &mut view,
                    vec![egui::Event::PointerMoved(pos)],
                    0.62,
                    1500.,
                    ppp,
                    false,
                );
                frame(
                    &ctx,
                    &mut view,
                    vec![button(pos, true)],
                    0.64,
                    1500.,
                    ppp,
                    false,
                );
                frame(
                    &ctx,
                    &mut view,
                    vec![button(pos, false)],
                    0.66,
                    1500.,
                    ppp,
                    false,
                );
                let events = shared.test_events();
                let p = events
                    .iter()
                    .find(|e| e.kind == "pointer_down")
                    .unwrap()
                    .pointer
                    .as_ref()
                    .unwrap();
                assert!((p.x - 0.23).abs() < 0.00001 && (p.y - 0.67).abs() < 0.00001);
                Ok(())
            })
            .unwrap();
        }
    }
    const TREE_SPEC: &str = "(function()local items={};for i=1,510 do items[i]={id='l'..i,label='Leaf '..i}end;return{content={id='tree',kind='tree',height=160,items={{id='root',label='Branch',children=items},{id='locked',label='Disabled',enabled=false}}}}end)()";
    #[test]
    fn native_tree_expansion_selection_virtualization_last_row_and_disabled_input() {
        crate::lua::ui::native_panel(TREE_SPEC, |shared| {
            let ctx = egui::Context::default();
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            shared.test_events();
            assert!(!view.widget_rects.contains_key("tree:item:l1"));
            let arrow = view.widget_rects["tree:expand:root"].center();
            frame(
                &ctx,
                &mut view,
                vec![egui::Event::PointerMoved(arrow), button(arrow, true)],
                0.1,
                1200.,
                1.,
                false,
            );
            frame(
                &ctx,
                &mut view,
                vec![button(arrow, false)],
                0.12,
                1200.,
                1.,
                false,
            );
            let events = shared.test_events();
            assert!(events.iter().any(|e| e.kind == "expand"
                && matches!(&e.value,InputValue::Expansion{item,expanded:true}if item=="root")));
            frame(&ctx, &mut view, vec![], 0.14, 1200., 1., false);
            let rows = |v: &View| {
                v.widget_rects
                    .keys()
                    .filter(|s| s.starts_with("tree:item:"))
                    .count()
            };
            assert!(rows(&view) < 16);
            let item = view.widget_rects["tree:item:l1"].center();
            frame(
                &ctx,
                &mut view,
                vec![egui::Event::PointerMoved(item), button(item, true)],
                0.16,
                1200.,
                1.,
                false,
            );
            frame(
                &ctx,
                &mut view,
                vec![button(item, false)],
                0.18,
                1200.,
                1.,
                false,
            );
            assert!(
                shared.test_events().iter().any(
                    |e| e.kind == "change" && matches!(&e.value,InputValue::Text(s) if s=="l1")
                )
            );
            frame(
                &ctx,
                &mut view,
                vec![
                    egui::Event::PointerMoved(item),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0., -1000000.),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                0.2,
                1200.,
                1.,
                false,
            );
            for i in 0..20 {
                frame(
                    &ctx,
                    &mut view,
                    vec![],
                    0.22 + i as f64 * 0.02,
                    1200.,
                    1.,
                    false,
                );
                assert!(rows(&view) < 16);
            }
            assert!(view.widget_rects.contains_key("tree:item:l510"));
            assert!(!view.widget_rects.contains_key("tree:item:l1"));
            let disabled = view.widget_rects["tree:item:locked"].center();
            shared.test_events();
            frame(
                &ctx,
                &mut view,
                vec![egui::Event::PointerMoved(disabled), button(disabled, true)],
                0.7,
                1200.,
                1.,
                false,
            );
            frame(
                &ctx,
                &mut view,
                vec![button(disabled, false)],
                0.72,
                1200.,
                1.,
                false,
            );
            assert!(!shared.test_events().iter().any(|e| e.kind == "change"));
            let last = view.widget_rects["tree:item:l510"].center();
            frame(
                &ctx,
                &mut view,
                vec![egui::Event::PointerMoved(last), button(last, true)],
                0.74,
                1200.,
                1.,
                false,
            );
            frame(
                &ctx,
                &mut view,
                vec![button(last, false)],
                0.76,
                1200.,
                1.,
                false,
            );
            assert!(
                shared
                    .test_events()
                    .iter()
                    .any(|e| matches!(&e.value,InputValue::Text(s) if s=="l510"))
            );
            assert!(!shared.closed());
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn native_gradient_contains_four_authored_rgba_corners_and_color_keeps_zero_alpha_bytes() {
        let spec = "{content={id='root',kind='column',children={{id='tone',kind='color',label='Tone',value={1,2,3,0}},{id='canvas',kind='canvas',height=160,primitives={{kind='gradient',x1=0,y1=0,x2=1,y2=1,top_left={255,0,0,255},top_right={0,255,0,255},bottom_left={0,0,255,128},bottom_right={0,0,0,0}}}}}}}";
        crate::lua::ui::native_panel(spec, |shared| {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            assert!(matches!(
                view.values["tone"],
                InputValue::Color([1, 2, 3, 0])
            ));
            assert!(view.images.is_empty());
            let output = frame(&ctx, &mut view, vec![], 0.1, 1200., 1., false);
            let canvas = view.canvases["canvas"].rect.unwrap();
            let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
            let mesh = meshes
                .iter()
                .filter(|p| canvas.expand(1.).contains_rect(p.clip_rect))
                .find_map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m)
                        if m.vertices.iter().any(|v| v.color == Color32::RED) =>
                    {
                        Some(m)
                    }
                    _ => None,
                })
                .unwrap();
            for color in [
                Color32::RED,
                Color32::GREEN,
                Color32::from_rgba_unmultiplied(0, 0, 255, 128),
                Color32::TRANSPARENT,
            ] {
                assert!(
                    mesh.vertices.iter().any(|v| v.color == color),
                    "missing authored corner {color:?}"
                );
            }
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn virtual_tree_rows_keep_fixed_height_for_multiline_author_labels() {
        let spec = TREE_SPEC.replace(
            "label='Leaf '..i",
            "label='Leaf '..i..string.char(10)..'Second line'..string.char(10)..'Third line'",
        );
        crate::lua::ui::native_panel(&spec, |shared| {
            let ctx = egui::Context::default();
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            let arrow = view.widget_rects["tree:expand:root"].center();
            frame(
                &ctx,
                &mut view,
                vec![egui::Event::PointerMoved(arrow), button(arrow, true)],
                0.1,
                1200.,
                1.,
                false,
            );
            frame(
                &ctx,
                &mut view,
                vec![button(arrow, false)],
                0.12,
                1200.,
                1.,
                false,
            );
            frame(&ctx, &mut view, vec![], 0.14, 1200., 1., false);
            for (id, r) in &view.widget_rects {
                if id.starts_with("tree:item:l") {
                    assert!(
                        r.height() <= 26.,
                        "virtualized row {id} grew to{}",
                        r.height()
                    );
                }
            }
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn virtual_list_rows_keep_fixed_height_for_multiline_author_labels() {
        let spec = "(function()local c={};for i=1,512 do c[i]='Item '..i..string.char(10)..'Second'..string.char(10)..'Third'end;return{content={id='data',kind='list',height=160,choices=c}}end)()";
        crate::lua::ui::native_panel(spec, |shared| {
            let ctx = egui::Context::default();
            let mut view = View::new(Arc::clone(shared)).unwrap();
            warm(&ctx, &mut view);
            for (id, r) in &view.widget_rects {
                if id.starts_with("data:") {
                    assert!(r.height() <= 26., "virtual row {id} grew to{}", r.height());
                }
            }
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn straight_rgba_picker_roundtrips_rgb_at_every_alpha_without_premultiplied_byte_drift() {
        for i in 0u16..=255 {
            let rgb = [i as u8, ((i * 73) % 256) as u8, ((i * 131) % 256) as u8];
            for a in [0, 1, 2, 32, 64, 127, 128, 254, 255] {
                let input = [rgb[0], rgb[1], rgb[2], a];
                assert_eq!(picker_color(input).to_srgba_unmultiplied(), input);
                let mut edited = picker_color([rgb[0], rgb[1], rgb[2], 255]);
                edited.a = f32::from(a) / 255.0;
                assert_eq!(
                    edited.to_srgba_unmultiplied(),
                    input,
                    "alpha-only edit must preserve authored RGB"
                );
            }
        }
        // This exact palette exposed the native8bit premultiply roundtrip in Windows.
        for a in [0, 1, 128, 255] {
            assert_eq!(
                picker_color([60, 158, 255, a]).to_srgba_unmultiplied(),
                [60, 158, 255, a]
            );
        }
    }
}
