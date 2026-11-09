//! v1.0-beta1 (B1-0037 – B1-0039): the Lua Plugins window (Plugins → Lua
//! Plugins…, Settings → Plugins) and the app's side of the plugin
//! lifecycle: the project snapshot plugins read, the transactions their
//! commands commit (applied here, on the UI thread, through the undo
//! history), their selection requests, and the menu of their commands.
//!
//! Installing is always a user action on a local folder or `.lua` file;
//! nothing is granted by installing: the user ticks each capability the
//! plugin asks for and enables it.

use super::PlumSpiritApp;
use crate::lua::package::Origin;
use crate::lua::plugins::{Level, PluginEvent, PluginManager, Status};
use crate::lua::{Capability, World};
use crate::ui::status_bar::StatusKind;
use egui::RichText;
use std::sync::Arc;

/// The window's state.
#[derive(Default)]
pub struct LuaPluginsWindow {
    pub open: bool,
    search: String,
    /// A typed folder or `.lua` path to install from (every platform; the
    /// buttons beside it open the system dialogs where there are some).
    path: String,
    /// The plugin whose details are shown.
    selected: Option<String>,
    /// The plugin whose log is shown.
    log_for: Option<String>,
    /// The last result (text, is an error).
    message: Option<(String, bool)>,
    /// Asked: remove this plugin?
    confirm_remove: Option<String>,
}

/// One plugin command for the Plugins menu: (plugin id, plugin name,
/// [(command id, name, tooltip)]).
pub type MenuPlugin = (String, String, Vec<(String, String, String)>);

impl PlumSpiritApp {
    /// The plugin manager, created on first use from the settings' plugins
    /// folder (startup stays fast: nothing is scanned before).
    pub(super) fn lua_plugins(&mut self) -> &mut PluginManager {
        if self.lua_plugins.is_none() {
            let root = self
                .settings
                .lua_plugins
                .store_root(self.config_dir.as_deref());
            let mut m = PluginManager::new(root, self.scripts.clone());
            // B1-0035: settings and presets live with the app's settings,
            // whatever folder the plugins are installed in.
            if let Some(dir) = self.config_dir.as_deref() {
                m.set_data_root(Some(dir.join("plugin-data")));
            }
            m.start(&self.settings.lua_plugins);
            self.lua_plugins = Some(m);
        }
        self.lua_plugins.as_mut().expect("just created")
    }

    /// The open project as plugins read it (`None`: no project).
    pub(super) fn lua_world(&mut self, dark: bool) -> Option<World> {
        let playback = self.lua_playback_snapshot();
        let playback_resources = self.transport.midi_resources();
        let views = self.lua_view_snapshots();
        let time = self.time_selection;
        let selected_tracks: Vec<_> = self.selected_tracks.iter().copied().collect();
        let selected_clips: Vec<_> = self.track_list.selected.iter().copied().collect();
        let mut collapsed: Vec<u32> = self.collapsed.iter().map(|t| t.0).collect();
        collapsed.sort_unstable();
        let o = self.open.as_mut()?;
        o.history.observe_ids(&o.doc);
        let dirty = o.dirty();
        let (file, file_type) = match &o.source {
            super::DocSource::New => (None, crate::lua::handles::FileType::New),
            super::DocSource::Midi(p) => (
                Some(p.to_string_lossy().into_owned()),
                crate::lua::handles::FileType::Midi,
            ),
            super::DocSource::Project(p) => (
                Some(p.to_string_lossy().into_owned()),
                crate::lua::handles::FileType::Lup,
            ),
        };
        let active_track = if o.conductor_selected {
            None
        } else {
            o.doc.track(o.active_track).map(|t| t.id())
        };
        let info = crate::lua::handles::ProjectInfo {
            file,
            file_type,
            dirty,
            revision: o.history.current_state(),
            dark,
            note_palette: Arc::new(super::note_colors::palette_of(&self.settings)),
            collapsed,
            active_track,
            selected_tracks,
            selected_clips,
            time_selection: time,
            playback,
            playback_resources,
            views,
        };
        Some(World::new(Arc::clone(&o.doc), o.history.incarnations().snapshot()).with_info(info))
    }

    /// Runs a plugin command on the open project.
    /// The commands of every ready plugin (the Plugins menu).
    pub(super) fn lua_menu(&self) -> Vec<MenuPlugin> {
        let Some(m) = &self.lua_plugins else {
            return Vec::new();
        };
        m.plugins()
            .iter()
            .filter(|p| p.status == Status::Ready && (!p.decls.commands.is_empty()||!p.decls.formats.is_empty()))
            .map(|p| {
                (
                    p.id.clone(),
                    p.name(),
                    p.decls
                        .commands
                        .iter()
                        .map(|c| {
                            // A command with options opens a dialog first.
                            let name = if c.params.is_empty() {
                                c.name.clone()
                            } else {
                                format!("{}…", c.name)
                            };
                            (c.id.clone(), name, c.tooltip.clone())
                        })
                        .chain(p.decls.formats.iter().flat_map(|f|[crate::lua::formats::Mode::Import,crate::lua::formats::Mode::Export].into_iter().filter(|mode|f.supports(*mode)).map(|mode| {
                            (super::lua_formats::action(mode,&f.id),format!("{} {}…",if mode==crate::lua::formats::Mode::Import {"Import"}else {"Export"},f.name),
                            "Choose a file for this plugin's registered format. Imports are reviewed before Apply.".into())
                        })))
                        .collect(),
                )
            })
            .collect()
    }

    /// Every frame: plugin results, project open / close, repaint while
    /// busy. Cheap when no plugin runs.
    pub(super) fn poll_lua_plugins(&mut self, ctx: &egui::Context) {
        self.capture_lua_views();
        self.collect_lua_overlays();
        self.poll_lua_file_dialog(ctx);
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.lua_plugins.is_none() {
            if !self.lua_plugins_started {
                // Plugins load after the first frames are on screen.
                self.lua_plugins_started = true;
                self.lua_plugins();
            } else {
                return;
            }
        }
        // The plugins folder changed in Settings: start over from it.
        let root = self
            .settings
            .lua_plugins
            .store_root(self.config_dir.as_deref());
        if self
            .lua_plugins
            .as_ref()
            .is_some_and(|m| m.store_root() != root.as_deref())
        {
            let prefs = self.settings.lua_plugins.clone();
            if let Some(m) = self.lua_plugins.as_mut() {
                m.stop_all(&prefs);
            }
            self.lua_plugins = None;
            self.lua_plugins_generation = None;
            self.lua_plugins();
        }
        // Project open / close (by document generation).
        let generation = self.open.as_ref().map(|o| o.generation);
        if generation != self.lua_plugins_generation {
            self.lua_automation.clear();
            let had = self.lua_plugins_generation.is_some();
            self.lua_plugins_generation = generation;
            if had {
                self.lua_plugins().project_closed();
            }
            if generation.is_some()
                && let Some(w) = self.lua_world(dark)
            {
                self.lua_plugins().project_opened(w);
            }
        }
        self.sample_lua_surfaces();
        self.sample_lua_observation();
        self.poll_lua_playback_controls();
        self.sample_lua_playback();
        let prefs = self.settings.lua_plugins.clone();
        let events = self.lua_plugins().poll(&prefs);
        for e in events {
            match e {
                PluginEvent::TransportLoop {
                    plugin,
                    intent,
                    lifecycle,
                } => self.apply_lua_loop_intent(&plugin, intent, &lifecycle),
                PluginEvent::TransportMidi {
                    plugin,
                    intent,
                    lifecycle,
                } => self.apply_lua_midi_intent(&plugin, intent, &lifecycle),
                PluginEvent::DiscardMidi(intent) => super::project::reap(intent),
                PluginEvent::Commits {
                    plugin,
                    command,
                    commits,
                } => self.apply_lua_commits(&plugin, &command, commits),
                PluginEvent::FormatResult {
                    plugin,
                    format,
                    mode,
                    selected,
                    result,
                    lifecycle,
                } => self.lua_format_result(&plugin, &format, mode, selected, result, lifecycle),
                PluginEvent::Select { ui, .. } => self.apply_lua_selection(ui),
                PluginEvent::ResponseRequest {
                    plugin,
                    delivery,
                    request,
                } => self.start_lua_response(&plugin, delivery, request, dark),
                PluginEvent::ResponseResult {
                    plugin,
                    delivery,
                    commits,
                    guard,
                    notices,
                } => self.apply_lua_response(&plugin, delivery, commits, *guard, notices),
                PluginEvent::Notice { text, error } => self.set_status(
                    text,
                    if error {
                        StatusKind::Error
                    } else {
                        StatusKind::Info
                    },
                ),
                PluginEvent::ObservationNotice {
                    delivery,
                    text,
                    error,
                } => {
                    if self.lua_observation_delivery_valid(&delivery) {
                        self.set_status(
                            text,
                            if error {
                                StatusKind::Error
                            } else {
                                StatusKind::Info
                            },
                        );
                    }
                }
                PluginEvent::NeedWorld => {
                    if let Some(w) = self.lua_world(dark) {
                        self.lua_plugins().supply_world(w);
                    }
                }
                PluginEvent::DeclsChanged => {}
                PluginEvent::ProjectData { plugin, text } => {
                    if let Some(g) = self.open.as_ref().map(|o| o.generation) {
                        self.custom_project_data_staged();
                        self.lua_project_data.push((g, plugin, text));
                    }
                }
                PluginEvent::ToolResult {
                    plugin,
                    generation,
                    preview,
                    result,
                    ..
                } => self.custom_tool_result(&plugin, generation, preview, result),
            }
        }
        self.store_lua_project_data();
        self.maintain_lua_format();
        self.poll_lua_file_dialog(ctx);
        self.poll_lua_midi_input(ctx);
        if let Some(manager) = self.lua_plugins.as_ref() {
            if manager.is_busy() || self.lua_observation_sampling() {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            } else if manager.observing()
                || manager.observing_playback()
                || manager.observing_surfaces()
            {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
    }

    /// Permission/revision checks continue while a native picker has focus.
    fn poll_lua_file_dialog(&mut self, ctx: &egui::Context) {
        use crate::lua::bridge::{BridgeError, ProjectStamp};
        let current = self.open.as_ref().map(|o| ProjectStamp {
            serial: o.history.incarnations().project(),
            revision: o.history.current_state(),
        });
        let allowed = |plugin: &str, save: bool| {
            let prefs = &self.settings.lua_plugins;
            let state = prefs.installed.get(plugin);
            prefs.run_plugins
                && state.is_some_and(|s| s.enabled)
                && self.lua_plugins.as_ref().is_some_and(|m| {
                    m.plugins().iter().any(|p| {
                        p.id == plugin
                            && p.effective(state).contains(&if save {
                                Capability::WriteUserFile
                            } else {
                                Capability::ReadUserFile
                            })
                    })
                })
        };
        if let Some(dialog) = self.lua_file_dialog.as_ref() {
            let permitted = dialog.plugin().is_some_and(|id| allowed(id, dialog.save()));
            if self
                .lua_file_dialog
                .as_mut()
                .unwrap()
                .poll(current, permitted)
            {
                self.lua_file_dialog = None;
            }
        }
        if self.lua_file_dialog.is_none()
            && !self.lua_midi_input.pending()
            && self.dialog.is_none()
            && let Some(request) = self
                .lua_plugins
                .as_ref()
                .and_then(PluginManager::poll_ui_request)
        {
            if !request.lease.matches_project(current) {
                request.reject(BridgeError::Expired);
            } else if !allowed(request.lease.plugin.as_ref(), request.spec.save) {
                request.reject(BridgeError::NotUserInvoked);
            } else {
                self.lua_file_dialog = super::lua_file_dialog::NativeFileDialog::start(request);
            }
        }
        if self.lua_file_dialog.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// B1-0035: `plum.config.project` values a run changed, stored in the
    /// open project (`.lup` section `PLUG`; a project setting outside undo
    /// that marks the project changed). Waits while a job shares the
    /// document; data for a project that has been closed is dropped.
    fn store_lua_project_data(&mut self) {
        if self.lua_project_data.is_empty() {
            return;
        }
        let generation = self.open.as_ref().map(|o| o.generation);
        let pending = std::mem::take(&mut self.lua_project_data);
        let mut keep = Vec::new();
        for (g, plugin, text) in pending {
            if Some(g) != generation {
                continue;
            }
            match self.doc_mut() {
                Some(d) => set_plugin_data(&mut d.project_mut().plugin_data, plugin, text),
                None => keep.push((g, plugin, text)),
            }
        }
        self.lua_project_data = keep;
    }

    /// A command's transactions, through the undo history (one step each).
    fn apply_lua_commits(
        &mut self,
        plugin: &str,
        command: &str,
        mut commits: crate::lua::tx::Commits,
    ) {
        self.release_preview_worker();
        self.release_tool_worker();
        let Some(o) = self.open.as_mut() else {
            return;
        };
        if commits.needs_confirmation() {
            // B1-0026: a multi-million-note deletion needs the user's yes;
            // a plugin command never deletes that much silently.
            self.set_status(
                format!(
                    "{plugin}: {command} would remove {} notes; nothing was changed (run it from the Lua Plugins window to confirm).",
                    plumspirit_engine::midi::group_digits(commits.removed_notes())
                ),
                StatusKind::Warning,
            );
            return;
        }
        match commits.apply(&mut o.history, &mut o.doc) {
            Ok(_) => {}
            Err(e) => self.set_status(
                format!("{plugin}: {command}: {}", e.user_message()),
                StatusKind::Warning,
            ),
        }
    }

    /// A command's selection request, applied to the UI.
    fn apply_lua_selection(&mut self, ui: crate::lua::selection::UiSelection) {
        if let Some(Some(id)) = ui.active_track
            && let Some(o) = self.open.as_mut()
            && let Some(i) = o.doc.index_of(id)
        {
            o.active_track = i;
            o.conductor_selected = false;
        }
        if let Some(tracks) = ui.tracks {
            self.selected_tracks = tracks.into_iter().collect();
        }
        if let Some(clips) = ui.clips {
            self.track_list.selected = clips.into_iter().collect();
        }
        if let Some(t) = ui.time {
            self.time_selection = t;
        }
    }

    /// The Lua Plugins window.
    pub(super) fn lua_plugins_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.lua_plugins_window.open {
            return;
        }
        let mut open = true;
        let mut actions: Vec<UiAction> = Vec::new();
        let prefs = self.settings.lua_plugins.clone();
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .ok();
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .ok();
        // The manager exists once the window is shown.
        self.lua_plugins();
        let m = self.lua_plugins.as_ref().expect("created above");
        let overlay_controls = &mut self.lua_overlays;
        let w = &mut self.lua_plugins_window;
        let mut want_help = false;
        egui::Window::new("Lua Plugins")
            .id(egui::Id::new("plumspirit-lua-plugins"))
            .open(&mut open)
            .default_width(620.0)
            .default_height(520.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        "Plugins are Lua scripts from the community. They run in a sandbox and can only do what you allow below. Install only plugins you trust.",
                    );
                    want_help |= super::help::help_button(ui, "lua-plugins");
                });
                ui.horizontal(|ui| {
                    ui.label("Plugins folder:");
                    ui.label(
                        RichText::new(
                            m.store_root()
                                .map_or_else(|| "(unavailable)".into(), |p| p.display().to_string()),
                        )
                        .monospace(),
                    );
                });
                if !prefs.run_plugins {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Lua plugins are switched off (Settings → Plugins). Installed plugins are kept but do not run.",
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Install from:");
                    ui.add(
                        egui::TextEdit::singleline(&mut w.path)
                            .hint_text("a plugin folder or a .lua file")
                            .desired_width(300.0),
                    )
                    .on_hover_text("Type or paste the full path of a plugin folder (with plum-plugin.toml) or of a single .lua file.");
                    if ui
                        .button("Install")
                        .on_hover_text("Copies the plugin into the plugins folder. Nothing is allowed or enabled until you choose so.")
                        .clicked()
                    {
                        actions.push(UiAction::InstallPath(w.path.trim().to_owned()));
                    }
                    if cfg!(any(windows, target_os = "macos")) {
                        if ui.button("Folder…").on_hover_text("Choose a plugin folder.").clicked() {
                            actions.push(UiAction::PickFolder);
                        }
                        if ui.button("Lua File…").on_hover_text("Choose a single .lua file.").clicked() {
                            actions.push(UiAction::PickFile);
                        }
                    }
                    if ui
                        .button("Refresh")
                        .on_hover_text("Reads the plugins folder again and reloads plugins whose files changed (after you edited a script).")
                        .clicked()
                    {
                        actions.push(UiAction::Refresh);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Examples:")
                        .on_hover_text("Plugins that ship with PlumSpirit, to try the scripting API or to read as a starting point for your own.");
                    for ex in &crate::lua::examples::EXAMPLES {
                        let installed = m.plugin(ex.id).is_some();
                        let text = if installed {
                            format!("Reinstall {}", ex.name)
                        } else {
                            format!("Install {}", ex.name)
                        };
                        if ui
                            .button(text)
                            .on_hover_text(format!(
                                "{} Installing copies it into the plugins folder like any plugin; review what it asks to do, then enable it. Reinstalling restores its original files.",
                                ex.summary
                            ))
                            .clicked()
                        {
                            actions.push(UiAction::InstallExample(ex.id));
                        }
                    }
                });
                overlay_controls.controls(ui);
                if let Some((text, error)) = &w.message {
                    let color = if *error {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.colored_label(color, text);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    ui.add(egui::TextEdit::singleline(&mut w.search).desired_width(220.0))
                        .on_hover_text("Finds plugins by name, id, author or description.");
                });
                let list = m.search(&w.search);
                if list.is_empty() {
                    ui.label(if m.plugins().is_empty() {
                        "No plugins are installed yet."
                    } else {
                        "No plugin matches the search."
                    });
                }
                egui::ScrollArea::vertical()
                    .id_salt("lua-plugins-list")
                    .max_height(ui.available_height() - 8.0)
                    .show(ui, |ui| {
                        for p in list {
                            let state = prefs.installed.get(&p.id);
                            let enabled = state.is_some_and(|s| s.enabled);
                            ui.horizontal(|ui| {
                                let mut on = enabled;
                                let broken = matches!(p.status, Status::Broken(_));
                                if ui
                                    .add_enabled(!broken, egui::Checkbox::new(&mut on, ""))
                                    .on_hover_text("Enable or disable this plugin.")
                                    .changed()
                                {
                                    actions.push(UiAction::Enable(p.id.clone(), on));
                                }
                                let selected = w.selected.as_deref() == Some(p.id.as_str());
                                let title = match &p.manifest {
                                    Ok(mf) => format!("{} {}", mf.name, mf.version),
                                    Err(_) => p.id.clone(),
                                };
                                if ui.selectable_label(selected, RichText::new(title).strong()).clicked() {
                                    w.selected = if selected { None } else { Some(p.id.clone()) };
                                }
                                let status = p.status.label();
                                let color = match p.status {
                                    Status::Failed(_) | Status::Broken(_) | Status::Incompatible(_) => {
                                        ui.visuals().error_fg_color
                                    }
                                    _ => ui.visuals().weak_text_color(),
                                };
                                ui.colored_label(color, status);
                                if let Some(pr) = p.progress() {
                                    let f = pr.fraction.unwrap_or(0.0) as f32;
                                    ui.add(
                                        egui::ProgressBar::new(f)
                                            .corner_radius(crate::ui::theme::CORNER)
                                            .desired_width(80.0),
                                    );
                                    if ui.button("Cancel").clicked() {
                                        actions.push(UiAction::Cancel(p.id.clone()));
                                    }
                                }
                            });
                            if w.selected.as_deref() == Some(p.id.as_str()) {
                                ui.indent(("lua-plugin-details", &p.id), |ui| {
                                    details(ui, p, state, &mut actions, w);
                                });
                            }
                            ui.add_space(2.0);
                        }
                    });
            });
        // The log window.
        if let Some(id) = w.log_for.clone() {
            let mut log_open = true;
            if let Some(p) = m.plugin(&id) {
                egui::Window::new(format!("Plugin Log — {}", p.name()))
                    .id(egui::Id::new(("plumspirit-lua-log", &id)))
                    .open(&mut log_open)
                    .default_width(560.0)
                    .default_height(320.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button("Copy for a Bug Report")
                                .on_hover_text("Copies the log with your user name and folders removed, ready to share.")
                                .clicked()
                            {
                                let version = p.manifest.as_ref().map_or("?", |mf| mf.version.as_str());
                                let header = format!(
                                    "PlumSpirit {} — plugin {} {} ({})",
                                    env!("CARGO_PKG_VERSION"),
                                    p.id,
                                    version,
                                    p.status.label()
                                );
                                ui.ctx().copy_text(p.log.report(&header, home.as_deref(), user.as_deref()));
                            }
                            if ui.button("Clear").clicked() {
                                actions.push(UiAction::ClearLog(id.clone()));
                            }
                        });
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for l in p.log.lines() {
                                    let color = match l.level {
                                        Level::Error => ui.visuals().error_fg_color,
                                        Level::Warning => ui.visuals().warn_fg_color,
                                        Level::Output => ui.visuals().text_color(),
                                        Level::Info => ui.visuals().weak_text_color(),
                                    };
                                    ui.colored_label(
                                        color,
                                        RichText::new(format!("{} [{}] {}", l.at, l.level.label(), l.text))
                                            .monospace(),
                                    );
                                }
                            });
                    });
            }
            if !log_open {
                w.log_for = None;
            }
        }
        // Remove confirmation.
        if let Some(id) = w.confirm_remove.clone() {
            let mut keep = true;
            egui::Window::new("Remove Plugin")
                .id(egui::Id::new("plumspirit-lua-remove"))
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Remove \u{201C}{id}\u{201D}? Its files are deleted from the plugins folder and its settings are forgotten."
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            actions.push(UiAction::Remove(id.clone()));
                            keep = false;
                        }
                        if ui.button("Keep").clicked() {
                            keep = false;
                        }
                    });
                });
            if !keep {
                w.confirm_remove = None;
            }
        }
        if !open {
            self.lua_plugins_window.open = false;
        }
        if want_help {
            self.open_help("lua-plugins");
        }
        for a in actions {
            self.lua_plugins_action(a, frame);
        }
    }

    fn lua_plugins_action(&mut self, a: UiAction, frame: &eframe::Frame) {
        let mut prefs = self.settings.lua_plugins.clone();
        let mut message: Option<(String, bool)> = None;
        match a {
            UiAction::InstallPath(path) => {
                if path.is_empty() {
                    message = Some((
                        "Type the path of a plugin folder or a .lua file first.".into(),
                        true,
                    ));
                } else {
                    let p = std::path::PathBuf::from(&path);
                    message = Some(self.install_lua_plugin(&p, &mut prefs));
                }
            }
            UiAction::InstallExample(id) => {
                if let Some(ex) = crate::lua::examples::by_id(id) {
                    message = Some(match self.lua_plugins().install_example(ex, &mut prefs) {
                        Ok(r) => {
                            self.lua_plugins_window.selected = Some(r.manifest.id.clone());
                            (
                                if r.replaced.is_some() {
                                    format!(
                                        "Restored the example {} {}.",
                                        r.manifest.name, r.manifest.version
                                    )
                                } else {
                                    format!(
                                        "Installed the example {} {}. Review what it asks to do, then enable it.",
                                        r.manifest.name, r.manifest.version
                                    )
                                },
                                false,
                            )
                        }
                        Err(e) => (e, true),
                    });
                }
            }
            UiAction::PickFolder => {
                if let Some(p) = super::files::pick_folder(frame, "Install Plugin Folder") {
                    message = Some(self.install_lua_plugin(&p, &mut prefs));
                }
            }
            UiAction::PickFile => {
                if let Some(p) = super::files::pick_lua(frame) {
                    message = Some(self.install_lua_plugin(&p, &mut prefs));
                }
            }
            UiAction::UpdatePath(id, path) => {
                let p = std::path::PathBuf::from(path.trim());
                let file = p.is_file();
                message = Some(match self.lua_plugins().update(&id, &p, file, &mut prefs) {
                    Ok(r) => (
                        format!(
                            "Updated {} to {} (from {}).",
                            r.manifest.name,
                            r.manifest.version,
                            r.replaced.as_deref().unwrap_or("?")
                        ),
                        false,
                    ),
                    Err(e) => (e, true),
                });
            }
            UiAction::Refresh => {
                self.lua_plugins().refresh(&prefs);
                message = Some(("The plugins folder was read again.".into(), false));
            }
            UiAction::Enable(id, on) => self.lua_plugins().set_enabled(&id, on, &mut prefs),
            UiAction::Grant(id, cap, on) => {
                self.lua_plugins().set_granted(&id, &cap, on, &mut prefs)
            }
            UiAction::Reload(id) => self.lua_plugins().reload(&id, &prefs),
            UiAction::Cancel(id) => self.lua_plugins().cancel(&id),
            UiAction::Remove(id) => {
                message = Some(match self.lua_plugins().remove(&id, &mut prefs) {
                    Ok(()) => (format!("Removed \u{201C}{id}\u{201D}."), false),
                    Err(e) => (e, true),
                });
                if self.lua_plugins_window.selected.as_deref() == Some(id.as_str()) {
                    self.lua_plugins_window.selected = None;
                }
            }
            UiAction::ConfirmRemove(id) => self.lua_plugins_window.confirm_remove = Some(id),
            UiAction::ShowLog(id) => self.lua_plugins_window.log_for = Some(id),
            UiAction::ClearLog(id) => {
                if let Some(m) = self.lua_plugins.as_mut()
                    && let Some(p) = m.plugin_log_mut(&id)
                {
                    p.clear();
                }
            }
        }
        if message.is_some() {
            self.lua_plugins_window.message = message;
        }
        if prefs != self.settings.lua_plugins {
            self.settings.lua_plugins = prefs;
            self.save_settings();
        }
    }

    fn install_lua_plugin(
        &mut self,
        p: &std::path::Path,
        prefs: &mut crate::lua::plugins::PluginPrefs,
    ) -> (String, bool) {
        let file = p.is_file();
        match self.lua_plugins().install(p, file, prefs) {
            Ok(r) => {
                self.lua_plugins_window.selected = Some(r.manifest.id.clone());
                (
                    format!(
                        "Installed {} {}. Review what it asks to do, then enable it.",
                        r.manifest.name, r.manifest.version
                    ),
                    false,
                )
            }
            Err(e) => (e, true),
        }
    }
}

/// What the window asked for this frame.
enum UiAction {
    InstallPath(String),
    /// One of PlumSpirit's example plugins, by manifest id.
    InstallExample(&'static str),
    PickFolder,
    PickFile,
    UpdatePath(String, String),
    Refresh,
    Enable(String, bool),
    Grant(String, String, bool),
    Reload(String),
    Cancel(String),
    ConfirmRemove(String),
    Remove(String),
    ShowLog(String),
    ClearLog(String),
}

/// What a capability lets a plugin do, for the checkbox's tooltip.
fn capability_words(c: Capability) -> &'static str {
    match c {
        Capability::ReadMidiInput => {
            "Discover MIDI input names and request access to a device you select and explicitly approve for each command. Receives bounded short MIDI messages on the Lua worker; access ends on command completion, cancellation, project change or permission revocation."
        }
        Capability::UiOverlay => {
            "Draw bounded worker-authored editor overlays, receive bounded surface notifications, read authorized visible coordinates and retain project-scoped UI image pixels. Piano Roll geometry also needs project read; Player geometry also needs playback read. No note editing, input interception, file access or GPU access."
        }
        Capability::EmitMidi => {
            "Send bounded precomputed MIDI plans to isolated internal playback voices from explicit commands. Requires playback read, running playback and a SoundFont. Does not edit notes or access shared external MIDI routes."
        }
        Capability::ControlPlayback => {
            "Request play, pause, stop, seek and audition speed from explicitly invoked plugin commands; requests are bounded and rejected after competing actions. Playback read permission is also required."
        }
        Capability::ReadPlayback => {
            "Read captured playback position, state, loop, speed and Conductor timing; receive bounded worker notifications. This does not control playback or read notes."
        }
        Capability::ReadProject => {
            "Read the open project: tracks, clips, notes, events, the Conductor and the selection."
        }
        Capability::EditNotes => {
            "Change notes and controller events (always through Preview / Undo)."
        }
        Capability::EditTracks => "Add, remove and change tracks, subtracks, folders and clips.",
        Capability::EditConductor => "Change the tempo, time signatures and markers.",
        Capability::DrawingTools => {
            "Receive completed pointer strokes only while you select this plugin's drawing tool in the Piano Roll (tools.draw)."
        }
        Capability::UiPanels => {
            "Open custom panels and interactive canvases when you invoke the plugin; close the window to cancel its job."
        }
        Capability::ReadUserFile => "Read a file you choose in a dialog.",
        Capability::ListFonts => {
            "List installed font families/styles and load their font data for creative tools. This does not allow arbitrary file access."
        }
        Capability::WriteUserFile => "Write a file you choose in a dialog.",
        Capability::ObserveProject => {
            "Receive bounded project-change notifications on a worker; observation handlers cannot edit the project."
        }
        Capability::EditMetadata => {
            "Save or remove this plugin's typed project attributes through Preview / Undo."
        }
        Capability::CallServices => {
            "Call declared services in other enabled plugins using bounded plain data; provider project/UI authority is restricted."
        }
        Capability::ProvideServices => {
            "Publish bounded plain-data services on the plugin worker; no project or UI writes in service jobs."
        }
        Capability::AutomateProject => {
            "Allow event handlers to request bounded automatic note edits with Undo; note-edit permission is also required."
        }
    }
}

fn details(
    ui: &mut egui::Ui,
    p: &crate::lua::plugins::Plugin,
    state: Option<&crate::lua::plugins::PluginState>,
    actions: &mut Vec<UiAction>,
    w: &mut LuaPluginsWindow,
) {
    match &p.manifest {
        Err(e) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        Ok(m) => {
            egui::Grid::new(("lua-plugin-grid", &p.id))
                .num_columns(2)
                .spacing([12.0, 2.0])
                .show(ui, |ui| {
                    ui.label("Id");
                    ui.label(RichText::new(&m.id).monospace());
                    ui.end_row();
                    if !m.author.is_empty() {
                        ui.label("Author");
                        ui.label(&m.author);
                        ui.end_row();
                    }
                    if !m.license.is_empty() {
                        ui.label("License");
                        ui.label(&m.license);
                        ui.end_row();
                    }
                    ui.label("Needs");
                    ui.label(format!("scripting API {}.{} or later", m.api.0, m.api.1));
                    ui.end_row();
                    ui.label("Origin");
                    ui.label(state.map_or(Origin::LocalFolder, |s| s.origin).label());
                    ui.end_row();
                });
            if !m.description.is_empty() {
                ui.label(&m.description);
            }
            if m.capabilities.is_empty() {
                ui.label("It asks for nothing beyond the sandbox (it cannot read or change the project).");
            } else {
                ui.label("It asks to be allowed to:");
                for c in &m.capabilities {
                    let Some(cap) = Capability::parse(c) else {
                        continue;
                    };
                    let mut on = state.is_some_and(|s| s.granted.contains(c));
                    if ui
                        .checkbox(&mut on, format!("{c} — {}", capability_words(cap)))
                        .changed()
                    {
                        actions.push(UiAction::Grant(p.id.clone(), c.clone(), on));
                    }
                }
            }
            for dependency in &m.service_dependencies {
                ui.label(format!("Service dependency: {}", dependency.text()));
            }
            for service in &p.decls.services {
                ui.label(format!(
                    "Provides: {} v{}",
                    service.namespace,
                    service.version.text()
                ));
            }
            if !p.decls.commands.is_empty() {
                ui.label(format!(
                    "Commands (Plugins menu): {}",
                    p.decls
                        .commands
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    if let Status::Failed(why) | Status::Incompatible(why) = &p.status {
        ui.colored_label(ui.visuals().error_fg_color, why);
    }
    ui.horizontal(|ui| {
        if ui.button("Log").on_hover_text("What the plugin printed and every error, with times.").clicked() {
            actions.push(UiAction::ShowLog(p.id.clone()));
        }
        if ui
            .button("Reload")
            .on_hover_text("Stops the plugin and loads its scripts again (after you edited them).")
            .clicked()
        {
            actions.push(UiAction::Reload(p.id.clone()));
        }
        if ui
            .button("Update from Path")
            .on_hover_text("Replaces this plugin with the folder or .lua file typed in Install from (same id).")
            .clicked()
        {
            actions.push(UiAction::UpdatePath(p.id.clone(), w.path.clone()));
        }
        if ui.button("Remove…").clicked() {
            actions.push(UiAction::ConfirmRemove(p.id.clone()));
        }
    });
}

/// Sets (`text` empty: removes) a plugin's project data, keeping the list
/// sorted by plugin id as `.lup` stores it.
fn set_plugin_data(list: &mut Vec<(String, String)>, plugin: String, text: String) {
    match list.binary_search_by(|(id, _)| id.as_str().cmp(&plugin)) {
        Ok(i) if text.is_empty() => {
            list.remove(i);
        }
        Ok(i) => list[i].1 = text,
        Err(_) if text.is_empty() => {}
        Err(i) => list.insert(i, (plugin, text)),
    }
}

#[cfg(test)]
mod tests {
    //! The app's side: install from a typed path, allow, enable, the command
    //! in the Plugins menu, its transaction through the undo history, the
    //! Settings switch.
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use super::UiAction;
    use crate::lua::plugins::Status;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::time::{Duration, Instant};

    #[test]
    fn worker_world_captures_actual_note_palette_without_live_settings_access() {
        use crate::ui::theme::{NoteColorMode, NotePalette};
        let mut h = Harness::new(doc());
        h.app.settings.note_colors.mode = NoteColorMode::Manual;
        h.app.settings.note_colors.manual = vec![[11, 27, 173]];
        let w = h.app.lua_world(false).unwrap();
        let expected = NotePalette::new(h.app.settings.note_colors.clone())
            .entry(0, 100, false)
            .fill;
        h.app.settings.note_colors.manual[0] = [250, 190, 2];
        let host = crate::lua::Host::new(
            crate::lua::Limits::default(),
            [crate::lua::Capability::ReadProject],
        )
        .unwrap();
        host.set_world(Some(w));
        let script = format!(
            "local c=plum.project.current():track(1):subchannels()[1]:note_color();return c.r=={} and c.g=={} and c.b=={}",
            expected.r(),
            expected.g(),
            expected.b()
        );
        assert_eq!(
            host.run("captured-palette.lua", &script).unwrap(),
            crate::lua::Plain::Boolean(true)
        );
        assert_ne!(
            expected,
            NotePalette::new(h.app.settings.note_colors.clone())
                .entry(0, 100, false)
                .fill
        );
        assert!(!h.app.enabled(Command::Undo));
    }

    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..20u64)
            .map(|k| NoteRecord {
                start_tick: k * 240,
                duration_ticks: 200,
                attrs: NoteAttrs::new(64, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    fn frames_until(h: &mut Harness, what: &str, ok: impl Fn(&Harness) -> bool) {
        let t = Instant::now();
        while !ok(h) {
            h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
            assert!(t.elapsed() < Duration::from_secs(30), "{what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_plugin_installed_allowed_and_enabled_runs_its_command_as_one_undo_step() {
        let root =
            std::env::temp_dir().join(format!("plumspirit-app-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("plum-plugin.toml"),
            "id = \"test.raise\"\nname = \"Raise\"\nversion = \"1.0\"\napi = \"0.1\"\ncapabilities = [\"project.read\", \"notes.edit\"]\n",
        )
        .unwrap();
        std::fs::write(
            src.join("main.lua"),
            "function register(p) p.command{ id = 'up', name = 'Raise an Octave', run = function() \
               local t = plum.project.current():track(1) \
               local tx = plum.transaction{ label = 'Raise an Octave' } \
               tx:modify_notes(t:notes(), { transpose = 12 }) tx:commit() end }              p.command{ id = 'mark', name = 'Mark Project', params = { { key = 'n', kind = 'int', min = 1, max = 9, default = 3 } },                run = function(ctx) plum.config.project.set('times', ctx.params.n) end } end",
        )
        .unwrap();
        let mut h = Harness::new(doc());
        h.app.settings.lua_plugins.folder = Some(root.join("plugins"));
        assert!(h.command(Command::ShowLuaPlugins));
        h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
        let frame = eframe::Frame::_new_kittest();
        h.app
            .lua_plugins_action(UiAction::InstallPath(src.display().to_string()), &frame);
        let msg = h.app.lua_plugins_window.message.clone().unwrap();
        assert!(!msg.1 && msg.0.contains("Installed Raise 1.0"), "{msg:?}");
        assert!(!h.app.settings.lua_plugins.installed["test.raise"].enabled);
        for cap in ["project.read", "notes.edit"] {
            h.app.lua_plugins_action(
                UiAction::Grant("test.raise".into(), cap.into(), true),
                &frame,
            );
        }
        h.app
            .lua_plugins_action(UiAction::Enable("test.raise".into(), true), &frame);
        frames_until(&mut h, "the plugin loads", |h| {
            h.app
                .lua_plugins
                .as_ref()
                .and_then(|m| m.plugin("test.raise"))
                .is_some_and(|p| p.status == Status::Ready)
        });
        let menu = h.app.lua_menu();
        assert_eq!(menu.len(), 1);
        assert_eq!(menu[0].2[0].1, "Raise an Octave");
        assert_eq!(menu[0].2[1].1, "Mark Project…", "options open a dialog");
        let before = h.doc().fingerprint();
        h.app.run_lua_command("test.raise", "up", true);
        let t = Instant::now();
        while h.doc().fingerprint() == before {
            h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "the command runs: {} / {:?}",
                h.status(),
                h.app
                    .lua_plugins
                    .as_ref()
                    .unwrap()
                    .plugin("test.raise")
                    .unwrap()
                    .log
                    .lines()
                    .map(|l| l.text.clone())
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let o = h.app.open.as_ref().unwrap();
        assert_eq!(o.history.undo_label(), Some("Raise an Octave"));
        let mut pitches = Vec::new();
        h.doc()
            .track(0)
            .unwrap()
            .notes
            .query(0, u64::MAX, |_, v| pitches.push(v.attrs.pitch()));
        assert!(pitches.iter().all(|&p| p == 76), "{pitches:?}");
        // B1-0034 / B1-0035: a command with options asks first; its project
        // settings land in the project outside undo and mark it changed.
        h.app.run_lua_command("test.raise", "mark", true);
        assert!(h.app.lua_command_dialog.is_some());
        let label = h
            .app
            .open
            .as_ref()
            .unwrap()
            .history
            .undo_label()
            .map(str::to_owned);
        let key = super::super::settings_key(h.doc());
        h.app.confirm_command_dialog();
        frames_until(&mut h, "the project data arrives", |h| {
            !h.doc().project().plugin_data.is_empty()
        });
        let data = &h.doc().project().plugin_data;
        assert_eq!(data[0].0, "test.raise");
        assert!(data[0].1.contains("\"times\":3"), "{data:?}");
        assert_ne!(
            super::super::settings_key(h.doc()),
            key,
            "the project is changed"
        );
        let o = h.app.open.as_ref().unwrap();
        assert_eq!(
            o.history.undo_label().map(str::to_owned),
            label,
            "not an undo step"
        );
        let mut list = vec![("b".to_owned(), "1".to_owned())];
        super::set_plugin_data(&mut list, "a".into(), "2".into());
        super::set_plugin_data(&mut list, "c".into(), "3".into());
        super::set_plugin_data(&mut list, "b".into(), String::new());
        super::set_plugin_data(&mut list, "z".into(), String::new());
        assert_eq!(
            list,
            vec![("a".into(), "2".into()), ("c".into(), "3".into())]
        );
        // Settings → Plugins → Run Lua Plugins off stops every plugin.
        assert!(h.command(Command::ToggleRunLuaPlugins));
        frames_until(&mut h, "plugins stop", |h| {
            h.app
                .lua_plugins
                .as_ref()
                .and_then(|m| m.plugin("test.raise"))
                .is_some_and(|p| p.status == Status::Off || p.status == Status::Disabled)
        });
        assert!(h.app.lua_menu().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_examples_install_from_the_window_and_appear_in_their_menus() {
        let base = std::env::temp_dir().canonicalize().unwrap();
        let root = base.join(format!("plumspirit-app-examples-{}", std::process::id()));
        let cleanup = || {
            if root.exists() {
                let resolved = root.canonicalize().unwrap();
                assert_eq!(resolved.parent(), Some(base.as_path()));
                assert_eq!(resolved.file_name(), root.file_name());
                std::fs::remove_dir_all(resolved).unwrap();
            }
        };
        cleanup();
        let mut h = Harness::new(doc());
        h.app.settings.lua_plugins.folder = Some(root.join("plugins"));
        assert!(h.command(Command::ShowLuaPlugins));
        h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
        let frame = eframe::Frame::_new_kittest();
        for ex in &crate::lua::examples::EXAMPLES {
            h.app
                .lua_plugins_action(UiAction::InstallExample(ex.id), &frame);
            let msg = h.app.lua_plugins_window.message.clone().unwrap();
            assert!(
                !msg.1 && msg.0.starts_with("Installed the example"),
                "{msg:?}"
            );
            let st = &h.app.settings.lua_plugins.installed[ex.id];
            assert!(
                !st.enabled && st.granted.is_empty(),
                "nothing allowed by installing"
            );
            assert_eq!(st.origin, crate::lua::package::Origin::Example);
            let requested = h
                .app
                .lua_plugins
                .as_ref()
                .unwrap()
                .plugin(ex.id)
                .unwrap()
                .manifest
                .as_ref()
                .unwrap()
                .capabilities
                .clone();
            for cap in requested {
                h.app
                    .lua_plugins_action(UiAction::Grant(ex.id.into(), cap, true), &frame);
            }
            h.app
                .lua_plugins_action(UiAction::Enable(ex.id.into(), true), &frame);
        }
        frames_until(&mut h, "the examples load", |h| {
            h.app.lua_plugins.as_ref().is_some_and(|m| {
                crate::lua::examples::EXAMPLES
                    .iter()
                    .all(|e| m.plugin(e.id).is_some_and(|p| p.status == Status::Ready))
            })
        });
        let tools = h.app.custom_tool_menu();
        assert_eq!(tools.len(), 5);
        let legacy = tools
            .iter()
            .find(|t| t.0 == "plumspirit.example.graph-shift")
            .unwrap();
        assert_eq!(legacy.2[0].1, "Graph Shift", "Tools > Custom");
        let studio = tools
            .iter()
            .find(|t| t.0 == "plumspirit.example.custom-drawing-studio")
            .unwrap();
        assert_eq!(studio.2.len(), 5);
        let image = tools
            .iter()
            .find(|t| t.0 == "plumspirit.example.image-art-studio")
            .unwrap();
        assert_eq!(image.2.len(), 2);
        let text = tools
            .iter()
            .find(|t| t.0 == "plumspirit.example.text-art-studio")
            .unwrap();
        assert_eq!(text.2.len(), 2);
        assert_eq!(text.2[0].0, "composition");
        assert_eq!(text.2[1].0, "color-artwork");
        let midi = tools
            .iter()
            .find(|t| t.0 == "plumspirit.example.midi-input-laboratory")
            .unwrap();
        assert_eq!(midi.2.len(), 1);
        assert_eq!(midi.2[0].0, "replay");
        let menu = h.app.lua_menu();
        let fonts = menu
            .iter()
            .find(|p| p.0 == "plumspirit.example.font-resources")
            .unwrap();
        assert_eq!(fonts.2.len(), 4);
        assert!(fonts.2.iter().any(|c| c.0 == "choose-installed"));
        for (id, command) in [
            ("plumspirit.example.file-transform", "uppercase-text"),
            ("plumspirit.example.geometry-primitives", "draw-path"),
            ("plumspirit.example.metadata-notebook", "remember"),
            ("plumspirit.example.service-composer", "generate"),
            ("plumspirit.example.reactive-monitor", "summary"),
            ("plumspirit.example.reactive-velocity", "clamp"),
        ] {
            let item = menu
                .iter()
                .find(|p| p.0 == id)
                .expect("Embedded sample command menu");
            assert!(
                item.2.iter().any(|c| c.0 == command),
                "Missing command {id}/{command}"
            );
        }
        let manager = h.app.lua_plugins.as_ref().unwrap();
        assert_eq!(
            manager
                .plugin("plumspirit.example.note-service")
                .unwrap()
                .decls
                .services
                .len(),
            2
        );
        assert_eq!(
            manager
                .plugin("plumspirit.example.reactive-monitor")
                .unwrap()
                .decls
                .observers
                .len(),
            1
        );
        assert_eq!(
            manager
                .plugin("plumspirit.example.reactive-velocity")
                .unwrap()
                .decls
                .observers
                .len(),
            1
        );
        let transport = menu
            .iter()
            .find(|p| p.0 == "plumspirit.example.transport-monitor")
            .unwrap();
        assert_eq!(transport.2[0].1, "Capture Transport Snapshot");
        let cc = menu
            .iter()
            .find(|p| p.0 == "plumspirit.example.cc-batch")
            .expect("CC Batch in the Plugins menu");
        let names: Vec<&str> = cc.2.iter().map(|c| c.1.as_str()).collect();
        assert_eq!(
            names,
            vec!["Write CC Over Selection…", "Clear CC Over Selection…"],
            "options dialogs end in …"
        );
        // Reinstalling restores the files and keeps the user's decisions.
        h.app.lua_plugins_action(
            UiAction::InstallExample("plumspirit.example.cc-batch"),
            &frame,
        );
        let msg = h.app.lua_plugins_window.message.clone().unwrap();
        assert!(msg.0.starts_with("Restored the example"), "{msg:?}");
        assert!(h.app.settings.lua_plugins.installed["plumspirit.example.cc-batch"].enabled);
        drop(h);
        cleanup();
    }
}
