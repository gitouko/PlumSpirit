//! The compact tool row above the Event Graphs: the drawing tool for the
//! active graph, the Curve options menu, the event list and Graph 2.
//!
//! `Graph 1 ●  |  Edit  Free  Line  Gentle  Fast  S  Random  f(t)  |  Curve (menu)  |  List  |  Graph 2`

use crate::app::curves::{CurveSettings, IntervalSetting, NoteScope};
use crate::piano_roll::graph::GraphTool;
use crate::ui::theme::{Palette, size, space};
use egui::{Button, RichText, Stroke, Vec2};
use plumspirit_engine::editor::controllers::WritePolicy;
use plumspirit_engine::editor::curves::{Bend, Shape};
use plumspirit_engine::model::GraphTarget;

pub const HEIGHT: f32 = 26.0;

pub struct GraphBarInput<'a> {
    pub tool: GraphTool,
    pub active: usize,
    pub active_target: GraphTarget,
    pub graph2: bool,
    pub list: bool,
    pub editable: bool,
    /// Anchors placed and waiting for Apply.
    pub anchors_pending: bool,
    pub ppqn: Option<u64>,
    pub custom_names: Vec<&'a str>,
    /// The chosen custom function's problem, if it does not parse.
    pub custom_error: Option<&'a str>,
}

#[derive(Default)]
pub struct GraphBarOutput {
    pub tool: Option<GraphTool>,
    pub settings_changed: bool,
    pub open_functions: bool,
    pub toggle_list: bool,
    pub toggle_graph2: bool,
    pub apply_anchors: bool,
    pub cancel_anchors: bool,
}

fn toggle(
    ui: &mut egui::Ui,
    pal: &Palette,
    on: bool,
    enabled: bool,
    text: &str,
    tip: &str,
) -> bool {
    let t = if on {
        RichText::new(text)
            .size(size::SMALL)
            .color(pal.text)
            .strong()
    } else {
        RichText::new(text).size(size::SMALL)
    };
    let mut b = Button::new(t).min_size(Vec2::new(0.0, 20.0));
    if on {
        b = b.fill(pal.accent_weak);
    }
    let resp = ui.add_enabled(enabled, b).on_hover_text(tip);
    if on {
        ui.painter().rect_stroke(
            resp.rect,
            ui.visuals().widgets.inactive.corner_radius,
            Stroke::new(1.0, pal.accent),
            egui::StrokeKind::Inside,
        );
    }
    resp.clicked()
}

pub fn graph_bar(
    ui: &mut egui::Ui,
    input: &GraphBarInput<'_>,
    cs: &mut CurveSettings,
    pal: &Palette,
) -> GraphBarOutput {
    let mut out = GraphBarOutput::default();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.spacing_mut().button_padding = Vec2::new(5.0, 1.0);
        ui.add_space(space::MICRO);
        ui.label(
            RichText::new(format!("Graph {} ●", input.active + 1))
                .size(size::SMALL)
                .color(pal.accent),
        )
        .on_hover_text("The active graph: tools, curves, selection and Delete apply here. Click a graph to make it active.");
        ui.separator();
        let velocity = input.active_target == GraphTarget::Velocity;
        for t in GraphTool::ALL {
            let mut tip = t.tip().to_owned();
            if velocity && t != GraphTool::Edit {
                tip.push_str(" On the Velocity graph the curve sets the velocity of each note it covers.");
            }
            if toggle(ui, pal, input.tool == t, input.editable, t.label(), &tip) {
                out.tool = Some(t);
            }
        }
        if input.anchors_pending {
            ui.separator();
            if ui
                .add(Button::new(RichText::new("Apply Curve").size(size::SMALL).strong()))
                .on_hover_text("Write the curve between the anchors (Enter)")
                .clicked()
            {
                out.apply_anchors = true;
            }
            if ui
                .add(Button::new(RichText::new("Cancel").size(size::SMALL)))
                .on_hover_text("Discard the anchors (Esc)")
                .clicked()
            {
                out.cancel_anchors = true;
            }
        }
        ui.separator();
        let before = cs.clone();
        crate::ui::toolbar::settings_dropdown(ui, "Curve", |ui| {
            ui.set_min_width(250.0);
            let small = |s: &str| RichText::new(s).size(size::SMALL);
            ui.label(small("Interval (event spacing)").strong());
            // Submenus, not combo boxes: egui keeps one popup open at a time,
            // so a combo box inside this menu would close the menu.
            ui.menu_button(cs.interval.label(), |ui| {
                    for (i, label) in IntervalSetting::PRESETS {
                        let text = match (i, input.ppqn) {
                            (IntervalSetting::Note(n, d), Some(p)) => {
                                format!("{label} ({} ticks)", ((p * n * 2 + d) / (2 * d)).max(1))
                            }
                            _ => label.to_owned(),
                        };
                        ui.selectable_value(&mut cs.interval, i, text);
                    }
                    let mut custom = matches!(cs.interval, IntervalSetting::Ticks(_));
                    if ui.checkbox(&mut custom, "Custom ticks").changed() && custom {
                        cs.interval = IntervalSetting::Ticks(input.ppqn.map_or(120, |p| (p / 4).max(1)));
                    }
            });
            if let IntervalSetting::Ticks(t) = &mut cs.interval {
                ui.horizontal(|ui| {
                    ui.label(small("Every"));
                    ui.add(crate::ui::stepper::Stepper::new(t).range(1..=u64::MAX / 4).speed(1.0));
                    ui.label(small("ticks"));
                });
            }
            ui.separator();
            ui.label(small("Gentle / Fast bend").strong());
            ui.horizontal(|ui| {
                ui.radio_value(&mut cs.bend, Bend::EaseOut, "Fast start");
                ui.radio_value(&mut cs.bend, Bend::EaseIn, "Fast end");
            });
            ui.separator();
            ui.label(small("Existing events").strong());
            ui.radio_value(&mut cs.policy, WritePolicy::Replace, WritePolicy::Replace.label())
                .on_hover_text("Events of this graph's type and channel between the first and last new event are replaced; other controllers, channels and tracks are untouched.");
            ui.radio_value(&mut cs.policy, WritePolicy::Merge, WritePolicy::Merge.label())
                .on_hover_text("Existing events stay; at a tick the curve writes to, the event takes the new value (never two at one tick).");
            ui.separator();
            ui.checkbox(&mut cs.before_notes, "Only Before the Note")
                .on_hover_text("Controller curves write one event right before each note start in the curve (ordered before the notes of that tick) instead of at every interval step. Velocity curves always apply to the notes themselves.");
            ui.add_enabled_ui(cs.before_notes || velocity, |ui| {
                ui.radio_value(&mut cs.scope, NoteScope::SelectedOrAll, "Selected notes (all when none is selected)");
                ui.radio_value(&mut cs.scope, NoteScope::All, "Every note in the curve's range");
                ui.add_enabled(
                    cs.before_notes && !velocity,
                    egui::Checkbox::new(&mut cs.same_channel, "Only notes on the graph's channel"),
                );
            });
            ui.separator();
            ui.label(small("Random").strong());
            ui.horizontal(|ui| {
                ui.label(small("Seed"));
                ui.add(crate::ui::stepper::Stepper::new(&mut cs.seed).speed(1.0));
                if ui.small_button("New").on_hover_text("A different seed").clicked() {
                    cs.seed = plumspirit_engine::editor::curves::splitmix64(cs.seed ^ 0xA5A5);
                }
            });
            ui.checkbox(&mut cs.advance_seed, "New seed after each random curve");
            ui.label(small("The values lie between the two ends of the drag. The same seed gives the same events.").weak());
            ui.separator();
            ui.label(small("Custom function").strong());
            ui.menu_button(cs.custom.clone(), |ui| {
                for n in &input.custom_names {
                    ui.selectable_value(&mut cs.custom, (*n).to_owned(), *n);
                }
            });
            if let Some(e) = input.custom_error {
                ui.label(small(e).color(pal.error));
            }
            ui.checkbox(&mut cs.normalize, "Normalize endpoints")
                .on_hover_text("Rescale the formula so it starts at the start value and ends at the end value (the formula text is unchanged).");
            ui.checkbox(&mut cs.clamp, "Clamp values outside the range")
                .on_hover_text("Without this, a formula that leaves the graph's range is refused and nothing changes.");
            if ui.button("Custom Functions…").clicked() {
                out.open_functions = true;
                ui.close();
            }
            ui.separator();
            ui.checkbox(&mut cs.anchors, "Anchors: adjust the ends before applying")
                .on_hover_text("After drawing, the two ends stay as handles you can drag; Enter or Apply Curve writes the events, Esc discards them.");
        })
        .on_hover_text("Curve options: interval, bend, Replace / Merge, Only Before the Note, random seed, custom function, anchors");
        out.settings_changed = *cs != before;
        let hint = match input.tool.shape() {
            Some(Shape::Custom) => format!("f(t) = {}", cs.custom),
            Some(_) | None if input.tool == GraphTool::Edit => String::new(),
            _ => cs.interval.label(),
        };
        if !hint.is_empty() {
            ui.label(RichText::new(hint).size(size::SMALL).color(pal.text_weak));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(space::MICRO);
            if toggle(ui, pal, input.graph2, true, "Graph 2", "Show or hide Event Graph 2 (it keeps its own type and channel)") {
                out.toggle_graph2 = true;
            }
            if toggle(ui, pal, input.list, true, "List", "Event List: the active graph's events as rows (position, type, channel, value)") {
                out.toggle_list = true;
            }
        });
    });
    out
}
