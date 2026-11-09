//! Host-owned device approval. Only bounded native data is rendered, never Lua.
use super::PlumSpiritApp as App;
use crate::lua::bridge::{BridgeError, JobLease, ProjectStamp};
use crate::lua::midi_input::ConsentRequest;
use crate::lua::{Capability, plugins::PluginManager};
use std::sync::{Arc, Weak};

const MAX_ACTIVE_LEASES: usize = 8;
#[derive(Default)]
pub(super) struct State {
    request: Option<ConsentRequest>,
    selected: Option<usize>,
    active: Vec<Weak<JobLease>>,
    #[cfg(test)]
    controls: Controls,
}
#[cfg(test)]
#[derive(Default)]
struct Controls {
    combo: Option<egui::Rect>,
    devices: Vec<egui::Rect>,
    allow: Option<egui::Rect>,
    cancel: Option<egui::Rect>,
    allow_enabled: bool,
}
fn revoke(lease: &JobLease) {
    lease.cancel_from_surface();
    lease.finish();
}
impl State {
    pub(super) fn pending(&self) -> bool {
        self.request.is_some()
    }
    pub(super) fn clear(&mut self) {
        if let Some(request) = self.request.take() {
            revoke(&request.lease);
            request.reject(BridgeError::Expired);
        }
        for lease in self.active.drain(..).filter_map(|w| w.upgrade()) {
            revoke(&lease);
        }
        self.selected = None;
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.clear();
    }
}
impl App {
    fn midi_input_allowed(&self, plugin: &str) -> bool {
        let prefs = &self.settings.lua_plugins;
        let state = prefs.installed.get(plugin);
        prefs.run_plugins
            && state.is_some_and(|s| s.enabled)
            && self
                .lua_plugins
                .as_ref()
                .and_then(|m| m.plugin(plugin))
                .is_some_and(|p| p.effective(state).contains(&Capability::ReadMidiInput))
    }
    pub(super) fn poll_lua_midi_input(&mut self, ctx: &egui::Context) {
        let current = self.open.as_ref().map(|o| ProjectStamp {
            serial: o.history.incarnations().project(),
            revision: o.history.current_state(),
        });
        let mut active = std::mem::take(&mut self.lua_midi_input.active);
        active.retain(|weak| {
            if let Some(lease) = weak.upgrade() {
                if lease.matches_project(current) && self.midi_input_allowed(&lease.plugin) {
                    return true;
                } else {
                    revoke(&lease);
                }
            }
            false
        });
        self.lua_midi_input.active = active;
        if self.lua_midi_input.request.as_ref().is_some_and(|r| {
            !r.lease.matches_project(current) || !self.midi_input_allowed(&r.lease.plugin)
        }) && let Some(request) = self.lua_midi_input.request.take()
        {
            revoke(&request.lease);
            request.reject(BridgeError::Expired);
            self.lua_midi_input.selected = None;
        }
        if !self.lua_midi_input.pending()
            && self.dialog.is_none()
            && self.lua_file_dialog.is_none()
            && let Some(request) = self
                .lua_plugins
                .as_ref()
                .and_then(PluginManager::poll_midi_input_request)
        {
            if !request.lease.matches_project(current) {
                request.reject(BridgeError::Expired);
            } else if !self.midi_input_allowed(&request.lease.plugin) {
                request.reject(BridgeError::NotUserInvoked);
            } else if self.lua_midi_input.active.len() >= MAX_ACTIVE_LEASES {
                request.reject(BridgeError::Busy);
            } else {
                self.lua_midi_input.request = Some(request);
                self.lua_midi_input.selected = None;
            }
        }
        let Some(request) = self.lua_midi_input.request.as_ref() else {
            return;
        };
        let mut open = true;
        let mut choice = None;
        #[cfg(test)]
        {
            self.lua_midi_input.controls = Controls::default();
        }
        egui::Window::new("MIDI input — PlumSpirit").id(egui::Id::new("native-midi-input-consent"))
            .collapsible(false).resizable(false).default_width(440.0).open(&mut open).show(ctx, |ui| {
                ui.label("Allow this plugin to read a MIDI input during this command?");
                ui.monospace(request.lease.plugin.as_ref());
                ui.label("Choose and approve one device. Access ends when the command finishes, is canceled, or its project or permission changes.");
                let inventory = &request.devices.inventory;
                if inventory.devices.is_empty() {
                    ui.label("Windows reported no available MIDI inputs. Connect a device and invoke the command again.");
                } else {
                    let combo=egui::ComboBox::from_id_salt("native-midi-input-device")
                        .selected_text(self.lua_midi_input.selected.and_then(|i| inventory.devices.get(i)).map(|d| format!("{}: {}",d.slot+1,d.name())).unwrap_or_else(|| "Choose one device".into()))
                        .show_ui(ui, |ui| {
                            for (i,d) in inventory.devices.iter().enumerate() {
                                let row=ui.selectable_value(&mut self.lua_midi_input.selected,Some(i),format!("{}: {}",d.slot+1,d.name()));
                                #[cfg(test)]
                                { self.lua_midi_input.controls.devices.push(row.rect); }
                                #[cfg(not(test))]
                                let _=row;
                            }
                        });
                    #[cfg(test)]
                    { self.lua_midi_input.controls.combo=Some(combo.response.rect); }
                    #[cfg(not(test))]
                    let _=combo;
                }
                if inventory.truncated { ui.label("Only the first 256 Windows device slots are listed."); }
                if inventory.unavailable != 0 { ui.label(format!("{} device descriptors were unavailable.",inventory.unavailable)); }
                ui.horizontal(|ui| {
                    let allow=ui.add_enabled(self.lua_midi_input.selected.is_some(),egui::Button::new("Allow selected device"));
                    #[cfg(test)]
                    { self.lua_midi_input.controls.allow=Some(allow.rect);self.lua_midi_input.controls.allow_enabled=allow.enabled(); }
                    if allow.clicked() {
                        choice = Some(self.lua_midi_input.selected);
                    }
                    let cancel=ui.button("Cancel");
                    #[cfg(test)]
                    { self.lua_midi_input.controls.cancel=Some(cancel.rect); }
                    if cancel.clicked() { choice = Some(None); }
                });
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            choice = Some(None);
        }
        if let Some(index) = choice
            && let Some(request) = self.lua_midi_input.request.take()
        {
            let lease = Arc::downgrade(&request.lease);
            let allowed = self.midi_input_allowed(&request.lease.plugin);
            if request.respond(current, allowed, index) == Ok(true)
                && !self
                    .lua_midi_input
                    .active
                    .iter()
                    .any(|w| Weak::ptr_eq(w, &lease))
            {
                self.lua_midi_input.active.push(lease);
            }
            self.lua_midi_input.selected = None;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

#[cfg(test)]
#[path = "lua_midi_input_tests.rs"]
mod tests;
