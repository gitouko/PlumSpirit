//! The small editing toolbar: tools, snap, Zoom X / Zoom Y and the Gate /
//! Velocity fields of the selected notes.
//!
//! Numeric fields show the current value while not being edited. Typing never
//! changes anything until Enter; Escape (or clicking elsewhere) puts the current
//! value back. While a field has focus, the editor's keyboard shortcuts are off
//! (the app checks `egui_wants_keyboard_input`), so typing "3" or Delete edits
//! the text, not the notes.

use crate::app::commands::Command;
use crate::piano_roll::Tool;
use crate::ui::theme::{Palette, size, space};
use egui::{Button, Key, RichText, Stroke};
use plumspirit_engine::editor::gate::{DefaultGate, GatePreset};
use plumspirit_engine::model::{Division, Snap};

/// Text of a numeric field and whether the user is editing it.
#[derive(Clone, Debug, Default)]
pub struct NumericField {
    pub text: String,
    pub editing: bool,
    /// Take the keyboard focus on the next frame.
    pub focus_next: bool,
}

/// A single-line numeric field: shows `current` unless focused; returns the
/// typed text when Enter commits it.
pub fn numeric_field(
    ui: &mut egui::Ui,
    field: &mut NumericField,
    current: &str,
    hint: &str,
    width: f32,
    enabled: bool,
    tip: &str,
) -> Option<String> {
    if !field.editing {
        field.text.clear();
        field.text.push_str(current);
    }
    let resp = ui
        .add_enabled(
            enabled,
            egui::TextEdit::singleline(&mut field.text)
                .desired_width(width)
                .hint_text(hint),
        )
        .on_hover_text(tip);
    if field.focus_next {
        field.focus_next = false;
        resp.request_focus();
    }
    if resp.has_focus() {
        field.editing = true;
    }
    if resp.lost_focus() {
        field.editing = false;
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            return Some(field.text.clone());
        }
    }
    None
}

/// A menu button with a painted down-arrow (not a font glyph, so it looks the
/// same with every UI font).
pub fn dropdown<R>(
    ui: &mut egui::Ui,
    text: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    dropdown_with(
        ui,
        text,
        egui::PopupCloseBehavior::CloseOnClick,
        add_contents,
    )
}

/// A dropdown that holds settings: it stays open while its own controls
/// (including combo boxes inside it) are used, and closes on a click outside.
pub fn settings_dropdown<R>(
    ui: &mut egui::Ui,
    text: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    dropdown_with(
        ui,
        text,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        add_contents,
    )
}

fn dropdown_with<R>(
    ui: &mut egui::Ui,
    text: &str,
    close: egui::PopupCloseBehavior,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    let label = if text.is_empty() {
        "    ".to_owned()
    } else {
        format!("{text}     ")
    };
    let (response, _) =
        egui::containers::menu::MenuButton::new(RichText::new(label).size(size::SMALL))
            .config(egui::containers::menu::MenuConfig::new().close_behavior(close))
            .ui(ui, add_contents);
    let rect = response.rect;
    let c = egui::pos2(rect.right() - 9.0, rect.center().y + 0.5);
    let color = if response.enabled() {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            c + egui::vec2(-3.5, -2.0),
            c + egui::vec2(3.5, -2.0),
            c + egui::vec2(0.0, 2.5),
        ],
        color,
        Stroke::NONE,
    ));
    response
}

pub struct ToolbarInput<'a> {
    /// v0.9 (V09-S08): the view the central area shows (Zoom X / Y apply to
    /// it; the note tools and Gate / Velocity belong to the Piano Roll).
    pub main_view: crate::app::track_list::MainView,
    pub tool: Tool,
    pub snap: Snap,
    pub division: Option<Division>,
    pub has_doc: bool,
    /// Note tools apply (a regular track is active and nothing is running).
    pub note_tools: bool,
    pub zoom_x: &'a str,
    pub zoom_y: &'a str,
    /// Selected notes (0: the Gate / Velocity fields are disabled).
    pub selected: usize,
    /// The selection's gate / velocity when every selected note has the same one.
    pub gate: Option<u64>,
    pub velocity: Option<u8>,
    /// Length of newly drawn notes, and its ticks at the edit position.
    pub default_gate: DefaultGate,
    pub default_gate_ticks: u64,
    /// Link With Default Gate.
    pub link_gate: bool,
    /// Preset lengths at the edit position, in ticks (`None`: no musical timing).
    pub preset_ticks: &'a [(GatePreset, Option<u64>)],
    /// v0.8 icon actions: what they would do now (state-aware, §201).
    pub can_undo: bool,
    pub can_redo: bool,
    pub can_mute: bool,
    pub can_unmute: bool,
    /// B1-0064: the Onion Skin button and menu (None: no project).
    pub onion: Option<crate::ui::menu::OnionMenuState>,
}

/// Text fields owned by the app (so an edit survives between frames).
pub struct ToolbarFields<'a> {
    pub zoom_x: &'a mut NumericField,
    pub zoom_y: &'a mut NumericField,
    pub gate: &'a mut NumericField,
    pub velocity: &'a mut NumericField,
    /// v0.7 Custom Snap entry.
    pub custom_snap: &'a mut NumericField,
}

#[derive(Default)]
pub struct ToolbarOutput {
    pub command: Option<Command>,
    pub snap: Option<Snap>,
    /// v0.7: Custom Snap text committed with Enter.
    pub custom_snap: Option<String>,
    pub zoom_x: Option<String>,
    pub zoom_y: Option<String>,
    pub gate: Option<String>,
    pub velocity: Option<String>,
    pub default_gate: Option<DefaultGate>,
    /// Set the selected notes to this many ticks (Gate presets menu: Set selected notes to).
    pub set_selected_gate: Option<u64>,
    pub link_gate: Option<bool>,
    /// B1-0064: the toolbar's Onion menu (opacity / outline darkness; its
    /// commands arrive in `command`).
    pub onion: crate::ui::menu::OnionMenuOut,
}

fn tool_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    active: bool,
    enabled: bool,
    label: &str,
    tip: &str,
    icon: crate::ui::icons::Icon,
) -> bool {
    // Room for the icon painted at the left of the label.
    let label = format!("     {label}");
    let text = if active {
        RichText::new(label).color(pal.text).strong()
    } else {
        RichText::new(label)
    };
    let mut b = Button::new(text).min_size(egui::vec2(52.0, 22.0));
    if active {
        // Fill only; the outline is painted so the button (and the toolbar) keeps
        // its size when hovered.
        b = b.fill(pal.accent_weak);
    }
    let resp = ui.add_enabled(enabled, b).on_hover_text(tip);
    let ir = egui::Rect::from_center_size(
        egui::pos2(resp.rect.left() + 13.0, resp.rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    let ic = if enabled { pal.text } else { pal.text_weak };
    crate::ui::icons::paint(ui.painter(), ir, icon, ic);
    if active {
        ui.painter().rect_stroke(
            resp.rect,
            ui.visuals().widgets.inactive.corner_radius,
            Stroke::new(1.0, pal.accent),
            egui::StrokeKind::Inside,
        );
    }
    resp.clicked()
}

/// B1-0062: one group of a wrapping bar (a label with its field, a button
/// cluster) kept together on one row: when its width measured on the last
/// frame no longer fits after the controls before it, the group starts the
/// next row. (A child `Ui` placed in a wrapping layout would otherwise wrap
/// inside the sliver left at the end of the row.) `enabled: false` greys the
/// group out like `add_enabled_ui`.
pub fn wrap_group<R>(
    ui: &mut egui::Ui,
    salt: &str,
    enabled: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let id = ui.id().with(("wrap_group", salt));
    let last: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    let row_started = ui.cursor().min.x > ui.max_rect().min.x + 0.5;
    if row_started && last > ui.available_size_before_wrap().x {
        ui.end_row();
    }
    let r = ui.horizontal(|ui| ui.add_enabled_ui(enabled, add).inner);
    let w = r.response.rect.width();
    if (w - last).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, w));
        // The first frame (or a changed width) is laid out again with the
        // measured size, so a wrap never shows for one frame.
        if last == 0.0 || w > last {
            ui.ctx().request_discard("wrap_group width");
        }
    }
    r.inner
}

fn separator(ui: &mut egui::Ui) {
    // B1-0057: compact group gaps keep the primary toolbar on one row.
    ui.add_space(space::MICRO);
    ui.separator();
}

pub fn toolbar(
    ui: &mut egui::Ui,
    input: &ToolbarInput<'_>,
    fields: ToolbarFields<'_>,
    pal: &Palette,
) -> ToolbarOutput {
    let mut out = ToolbarOutput::default();
    use crate::app::track_list::MainView;
    let in_track_list = input.main_view == MainView::TrackList;
    // B1-0062: wraps onto more rows when the window is narrow in points (a
    // large UI scale) instead of clipping the controls on the right.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::MICRO;
        // v0.9 (V09-S08): Track List (the primary window) / Piano Roll.
        for (v, label, tip) in [
            (
                MainView::TrackList,
                "Track List",
                "Every track's clips on a timeline: arrange the song (View → Track List)",
            ),
            (
                MainView::PianoRoll,
                "Piano Roll",
                "The notes of the active track (View → Piano Roll; double-click a clip in the Track List to open it)",
            ),
        ] {
            if ui
                .add_enabled(
                    input.has_doc,
                    Button::selectable(input.main_view == v, label),
                )
                .on_hover_text(tip)
                .clicked()
                && input.main_view != v
            {
                out.command = Some(Command::MainView(v));
            }
        }
        separator(ui);
        let tools_on = input.has_doc && input.note_tools;
        use crate::ui::icons::{Icon, icon_button};
        use crate::app::keybinds::hint;
        for (tool, label, tip, icon) in [
            (
                Tool::Select,
                "Select",
                "click a note to select it, Ctrl+click to toggle, drag on empty space to box-select, drag notes to move, drag a note's right edge to resize.",
                Icon::Select,
            ),
            (
                Tool::Draw,
                "Draw",
                "click or drag on empty space to create a note.",
                Icon::Draw,
            ),
            (
                Tool::Erase,
                "Erase",
                "click a note to delete it, or drag across notes to delete every note you touch. One drag is one undo step; Escape cancels the drag.",
                Icon::Erase,
            ),
        ] {
            let tip = format!("{label} tool{}: {tip}", hint(Command::Tool(tool)));
            if tool_button(ui, pal, input.tool == tool, tools_on, label, &tip, icon) {
                out.command = Some(Command::Tool(tool));
            }
        }
        separator(ui);
        // v0.8 §158: the high-frequency actions as icons (enabled only when
        // they would do something now). B1-0057: Mute and Unmute share one
        // state-aware button; Repeat Last Tool (F5) lives in Edit and the
        // note menu — the primary toolbar stays on one row.
        let settings_tip = format!("Settings…{}: every option in one window", hint(Command::OpenSettings));
        if icon_button(ui, Icon::Gear, true, &settings_tip).clicked() {
            out.command = Some(Command::OpenSettings);
        }
        let mute = if !input.can_mute && input.can_unmute {
            (
                Icon::Unmute,
                true,
                format!(
                    "Unmute Notes{}: the selected muted notes play and export again",
                    hint(Command::UnmuteNotes)
                ),
                Command::UnmuteNotes,
            )
        } else {
            (
                Icon::Mute,
                input.can_mute,
                format!(
                    "Mute Notes{}: the selected notes stay but do not play or export{}",
                    hint(Command::MuteNotes),
                    match Command::UnmuteNotes.shortcut() {
                        s if s.is_empty() => String::new(),
                        s => format!(" ({s} unmutes)"),
                    }
                ),
                Command::MuteNotes,
            )
        };
        for (icon, enabled, tip, cmd) in [
            (Icon::Undo, input.can_undo, format!("Undo{}", hint(Command::Undo)), Command::Undo),
            (Icon::Redo, input.can_redo, format!("Redo{}", hint(Command::Redo)), Command::Redo),
            mute,
        ] {
            if icon_button(ui, icon, enabled && input.has_doc, &tip).clicked() {
                out.command = Some(cmd);
            }
        }
        separator(ui);
        let options: &[Snap] = match input.division.map(|d| d.kind()) {
            Some(plumspirit_engine::model::TimingKind::Smpte { .. }) => &Snap::SMPTE,
            _ => &Snap::MUSICAL,
        };
        wrap_group(ui, "snap", input.has_doc, |ui| {
            ui.label("Snap");
            let mut snap = input.snap;
            let mut custom = false;
            egui::ComboBox::from_id_salt("snap")
                .width(52.0)
                .selected_text(snap.display())
                .show_ui(ui, |ui| {
                    for s in options {
                        ui.selectable_value(&mut snap, *s, s.label());
                    }
                    if ui
                        .selectable_label(matches!(snap, Snap::Custom(_)), "Custom…")
                        .on_hover_text("Type any snap: ticks (160) or an exact fraction of a whole note (1/12, 1/8T, 3/16, 1/8.).")
                        .clicked()
                    {
                        custom = true;
                    }
                })
                .response
                .on_hover_text(
                    "Snap: the grid that drawn and moved notes, the edit cursor and selections land on. Custom… takes any ticks or note fraction.",
                );
            if snap != input.snap {
                out.snap = Some(snap);
            }
            if custom {
                fields.custom_snap.editing = true;
                fields.custom_snap.focus_next = true;
                fields.custom_snap.text.clear();
            }
            if matches!(input.snap, Snap::Custom(_)) || fields.custom_snap.editing {
                let current = match input.snap {
                    Snap::Custom(t) => t.to_string(),
                    _ => String::new(),
                };
                out.custom_snap = numeric_field(
                    ui,
                    fields.custom_snap,
                    &current,
                    "1/12",
                    56.0,
                    input.has_doc,
                    "Custom Snap: ticks (e.g. 160) or a fraction of a whole note (1/12 = eighth triplets, 1/8T, 3/16, 1/8. dotted). Exact: a fraction that is not a whole number of ticks is refused. Press Enter.",
                );
            }
        });
        separator(ui);
        // B1-0064: Onion Skin on the toolbar — the button switches it on /
        // off (Ctrl+Shift+O), ▾ holds every Onion option (View → Onion Skin).
        if let Some(st) = input.onion {
            wrap_group(ui, "onion", true, |ui| {
                let on = st.mode != plumspirit_engine::model::project::OnionMode::Off;
                // B1-0057: one fixed width (the mode is in the tooltip, the ▾
                // menu and the track list's colour boxes), so switching it on
                // never pushes the toolbar onto a second row.
                let mode = if on {
                    format!("On: {}.", st.mode.label())
                } else {
                    "Off.".to_owned()
                };
                if ui
                    .add(egui::Button::selectable(on, "Onion"))
                    .on_hover_text(format!(
                        "Onion Skin — {mode} Other tracks' notes as faint ghosts in the Piano Roll. Click to switch it on / off ({}); ▾ for the mode, tracks, order and opacity. The color boxes in the track list show and hide single tracks.",
                        Command::ToggleOnion.shortcut()
                    ))
                    .clicked()
                {
                    out.command = Some(Command::ToggleOnion);
                }
                settings_dropdown(ui, "", |ui| {
                    crate::ui::menu::onion_items(ui, &st, &mut out.onion);
                })
                .on_hover_text("Onion Skin options: mode, tracks, outlines, layer order, opacity");
            });
            separator(ui);
        }
        // B1-0063: each zoom box is followed by its own [−][+] pair (the
        // shared stepper buttons), the same for X and Y.
        wrap_group(ui, "zoom", input.has_doc, |ui| {
            ui.label("X");
            out.zoom_x = numeric_field(
                ui,
                fields.zoom_x,
                input.zoom_x,
                "100%",
                52.0,
                input.has_doc,
                if in_track_list {
                    "Zoom X of the Track List (time): 100% = 32 points per quarter note. Type any positive percentage and press Enter. Ctrl+wheel zooms around the pointer."
                } else {
                    "Zoom X (time): 100% = 32 points per quarter note. Type any positive percentage and press Enter. Ctrl+wheel zooms around the pointer."
                },
            );
            match crate::ui::stepper::buttons_with(
                ui,
                input.has_doc,
                "Zoom X out (Ctrl+wheel)",
                "Zoom X in (Ctrl+wheel)",
            ) {
                Some(s) if s.0 < 0.0 => out.command = Some(Command::ZoomOut),
                Some(_) => out.command = Some(Command::ZoomIn),
                None => {}
            }
            ui.add_space(space::MICRO);
            ui.label("Y");
            out.zoom_y = numeric_field(
                ui,
                fields.zoom_y,
                input.zoom_y,
                "100%",
                46.0,
                input.has_doc,
                if in_track_list {
                    "Row height of the Track List: 100% = 34 points. Type a percentage from 65% to 352% and press Enter."
                } else {
                    "Zoom Y (pitch): 100% = 12 points per semitone. Type a percentage from 5% to 4000% and press Enter. Ctrl+Shift+wheel zooms around the pointer."
                },
            );
            match crate::ui::stepper::buttons_with(
                ui,
                input.has_doc,
                "Zoom Y out (Ctrl+Shift+wheel)",
                "Zoom Y in (Ctrl+Shift+wheel)",
            ) {
                Some(s) if s.0 < 0.0 => out.command = Some(Command::ZoomOutY),
                Some(_) => out.command = Some(Command::ZoomInY),
                None => {}
            }
            ui.add_space(space::MICRO);
            if ui
                .add(Button::new("1:1").min_size(egui::vec2(26.0, 22.0)))
                .on_hover_text(format!(
                    "Reset Zoom: X and Y back to 100% ({})",
                    Command::ResetZoom.shortcut()
                ))
                .clicked()
            {
                out.command = Some(Command::ResetZoom);
            }
            if ui
                .add(Button::new("Fit").min_size(egui::vec2(26.0, 22.0)))
                .on_hover_text(if in_track_list {
                    "Fit Song: show the whole song across the Track List"
                } else {
                    "Fit Song: show the whole song across the piano roll"
                })
                .clicked()
            {
                out.command = Some(Command::FitSong);
            }
        });
        if in_track_list {
            // B1-0057: Gate and Velocity edit notes; the Track List edits
            // clips, so its toolbar leaves them out (their commands and
            // shortcuts are unchanged).
            return;
        }
        separator(ui);
        let notes_on = input.has_doc && input.note_tools && input.selected > 0;
        let tip_suffix = if input.selected == 0 {
            " Select notes first."
        } else {
            ""
        };
        // Link With Default Gate: without a selection the Gate field shows and
        // edits the length of new notes.
        let default_mode = input.selected == 0 && input.link_gate;
        let gate_on = input.has_doc && input.note_tools && (input.selected > 0 || input.link_gate);
        wrap_group(ui, "gate", gate_on, |ui| {
            ui.label("Gate");
            let (current, hint, tip) = if default_mode {
                (
                    input.default_gate_ticks.to_string(),
                    "new",
                    format!(
                        "Gate: length of newly drawn notes ({}; {} ticks here). Type a number of ticks and press Enter to set a custom default (Link With Default Gate is on). Select notes to change their length instead.",
                        input.default_gate.label(),
                        input.default_gate_ticks
                    ),
                )
            } else {
                (
                    input.gate.map(|g| g.to_string()).unwrap_or_default(),
                    if input.selected > 0 { "mixed" } else { "—" },
                    format!("Gate: length of the selected notes in ticks (any whole number ≥ 1). Press Enter to apply to every selected note; starts stay where they are.{tip_suffix}"),
                )
            };
            out.gate = numeric_field(ui, fields.gate, &current, hint, 56.0, gate_on, &tip);
            dropdown(ui, "", |ui| {
                ui.set_min_width(230.0);
                ui.label(RichText::new("Length of new notes (ticks)").strong().size(size::SMALL));
                let mut g = input.default_gate;
                ui.radio_value(&mut g, DefaultGate::Grid, "Grid (follows Snap)");
                for (p, ticks) in input.preset_ticks {
                    let text = match ticks {
                        Some(t) => format!("{} ({t})", p.label()),
                        None => p.label().to_owned(),
                    };
                    ui.add_enabled_ui(ticks.is_some(), |ui| {
                        ui.radio_value(&mut g, DefaultGate::Preset(*p), text);
                    });
                }
                if let DefaultGate::Ticks(t) = input.default_gate {
                    ui.radio_value(&mut g, DefaultGate::Ticks(t), format!("Custom: {t} ticks"));
                }
                if g != input.default_gate {
                    out.default_gate = Some(g);
                    ui.close();
                }
                ui.separator();
                ui.add_enabled_ui(input.selected > 0, |ui| {
                    ui.menu_button("Set selected notes to", |ui| {
                        for (p, ticks) in input.preset_ticks {
                            if let Some(t) = ticks
                                && ui.button(format!("{} ({t})", p.label())).clicked()
                            {
                                out.set_selected_gate = Some(*t);
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                let mut link = input.link_gate;
                if ui
                    .checkbox(&mut link, "Link With Default Gate")
                    .on_hover_text("On: with no note selected, the Gate field shows and sets the length of new notes. Off: the Gate field only changes selected notes; choose the length of new notes here.")
                    .changed()
                {
                    out.link_gate = Some(link);
                }
                ui.label(
                    RichText::new("Changing the length of new notes never resizes existing notes.")
                        .size(size::SMALL)
                        .weak(),
                );
            })
            .on_hover_text("Gate presets: the length of new notes, or set the selected notes to a note value");
        });
        wrap_group(ui, "velocity", notes_on, |ui| {
            let hint = if input.selected > 0 { "mixed" } else { "—" };
            ui.add_space(space::MICRO);
            ui.label("Vel");
            let current = input.velocity.map(|v| v.to_string()).unwrap_or_default();
            out.velocity = numeric_field(
                ui,
                fields.velocity,
                &current,
                hint,
                44.0,
                notes_on,
                &format!("Velocity (1–127) of the selected notes. Press Enter to apply to every selected note.{tip_suffix}"),
            );
        });
    });
    out
}
