//! v1.0-beta1 (B1-0034): plugin commands in the app — the options dialog
//! of a command that declares `params` (the custom tools' native form) and
//! the running indicator (progress, Cancel) shown while a command runs.

use super::PlumSpiritApp;
use super::custom_tools::param_form;
use crate::lua::custom::{ToolValue, values_literal};
use crate::lua::plugin_api::CommandDecl;
use crate::lua::plugins::Status;
use crate::ui::status_bar::StatusKind;
use egui::RichText;
use std::collections::HashMap;
use std::time::Duration;

/// A command waits this long before the indicator appears (short commands
/// never flash it).
const INDICATOR_DELAY: Duration = Duration::from_millis(300);

/// The options dialog of one command.
pub struct CommandDialog {
    plugin: String,
    plugin_name: String,
    decl: CommandDecl,
    values: Vec<ToolValue>,
    /// Enter that opened the dialog must not run it.
    first_frame: bool,
}

/// The last options used per (plugin, command), for this run of the app.
#[derive(Default)]
pub struct CommandMemory(HashMap<(String, String), Vec<ToolValue>>);

impl PlumSpiritApp {
    /// Runs a plugin command: at once, or after its options dialog when it
    /// declares options.
    pub(super) fn run_lua_command(&mut self, plugin: &str, command: &str, dark: bool) {
        if let Some((mode, id)) = super::lua_formats::parse_action(command) {
            self.start_lua_format(plugin, id, mode);
            return;
        }
        if self.lua_formats.reviewing() {
            self.set_status(
                "Apply or Cancel the import before running another command.",
                StatusKind::Warning,
            );
            return;
        }
        let decl = self.lua_plugins.as_ref().and_then(|m| {
            let p = m.plugin(plugin)?;
            let d = p.decls.commands.iter().find(|c| c.id == command)?;
            Some((p.name(), d.clone()))
        });
        if let Some((plugin_name, decl)) = decl
            && !decl.params.is_empty()
        {
            let key = (plugin.to_owned(), command.to_owned());
            let values = self
                .lua_command_memory
                .0
                .get(&key)
                .filter(|v| v.len() == decl.params.len())
                .cloned()
                .unwrap_or_else(|| decl.params.iter().map(|p| p.default.clone()).collect());
            self.lua_command_dialog = Some(CommandDialog {
                plugin: plugin.to_owned(),
                plugin_name,
                decl,
                values,
                first_frame: true,
            });
            return;
        }
        self.start_lua_command(plugin, command, "nil".into(), dark);
    }

    fn start_lua_command(&mut self, plugin: &str, command: &str, params: String, dark: bool) {
        let world = self.lua_world(dark);
        let r = self
            .lua_plugins()
            .run_command_with(plugin, command, params, world);
        if let Err(e) = r {
            self.set_status(e, StatusKind::Warning);
        }
    }

    /// The options dialog and the running indicator (every frame).
    pub(super) fn plugin_commands_ui(&mut self, ctx: &egui::Context) {
        self.command_dialog_ui(ctx);
        self.command_indicator_ui(ctx);
    }

    fn command_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(d) = self.lua_command_dialog.as_mut() else {
            return;
        };
        let keys = self
            .open
            .as_ref()
            .map_or(127, |o| o.doc.project().keys.max_pitch());
        let mut open = true;
        let mut run = false;
        let mut cancel = false;
        let first = std::mem::replace(&mut d.first_frame, false);
        egui::Window::new(format!("{} — {}", d.decl.name, d.plugin_name))
            .id(egui::Id::new("plumspirit-plugin-command"))
            .open(&mut open)
            .default_width(360.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                if !d.decl.tooltip.is_empty() {
                    ui.label(&d.decl.tooltip);
                }
                ui.label(
                    RichText::new(format!("Command from the plugin {}.", d.plugin_name))
                        .small()
                        .weak(),
                );
                ui.separator();
                param_form(ui, &d.decl.params, &mut d.values, keys);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("Run")
                        .on_hover_text("Runs the command with these options.")
                        .clicked()
                    {
                        run = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .button("Reset")
                        .on_hover_text("Every option back to its default.")
                        .clicked()
                    {
                        d.values = d.decl.params.iter().map(|p| p.default.clone()).collect();
                    }
                });
            });
        if !first
            && ctx.input(|i| i.key_pressed(egui::Key::Enter))
            && !ctx.egui_wants_keyboard_input()
        {
            run = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if run {
            self.confirm_command_dialog();
        } else if cancel || !open {
            self.lua_command_dialog = None;
        }
    }

    /// The options dialog's Run: the command runs with its values (kept
    /// for the next time).
    pub(super) fn confirm_command_dialog(&mut self) {
        let Some(d) = self.lua_command_dialog.take() else {
            return;
        };
        let params = values_literal(&d.decl.params, &d.values);
        self.lua_command_memory
            .0
            .insert((d.plugin.clone(), d.decl.id.clone()), d.values);
        let dark = self.dark_mode;
        self.start_lua_command(&d.plugin, &d.decl.id, params, dark);
    }

    /// "Plugin: Command…" with its progress and Cancel, above the status
    /// bar while a command runs (the Lua Plugins window shows the same).
    fn command_indicator_ui(&mut self, ctx: &egui::Context) {
        let Some(m) = self.lua_plugins.as_ref() else {
            return;
        };
        let running: Vec<_> = m
            .plugins()
            .iter()
            .filter(|p| p.status == Status::Ready)
            .filter_map(|p| {
                let (name, elapsed) = p.running_command()?;
                (elapsed >= INDICATOR_DELAY).then(|| (p.id.clone(), p.name(), name, p.progress()))
            })
            .collect();
        if running.is_empty() {
            return;
        }
        let mut cancel: Option<String> = None;
        egui::Area::new(egui::Id::new("plumspirit-plugin-running"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    for (id, plugin, command, progress) in &running {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("{plugin}: {command}…"));
                            if let Some(pr) = progress {
                                if let Some(f) = pr.fraction {
                                    ui.add(
                                        egui::ProgressBar::new(f as f32)
                                            .corner_radius(crate::ui::theme::CORNER)
                                            .desired_width(90.0),
                                    );
                                }
                                if !pr.message.is_empty() {
                                    ui.label(RichText::new(&pr.message).weak());
                                }
                            }
                            if ui
                                .button("Cancel")
                                .on_hover_text("Stops the command; nothing it did is applied.")
                                .clicked()
                            {
                                cancel = Some(id.clone());
                            }
                        });
                    }
                });
            });
        if let Some(id) = cancel {
            self.lua_plugins().cancel(&id);
        }
    }
}
