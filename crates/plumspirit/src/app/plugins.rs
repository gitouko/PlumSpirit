//! Built-in plugins in the app (v0.8): Help → About with every plugin's
//! credit (contract §069, §276). The plugins themselves live in the tool
//! windows (HZ-Chop, SAFOR) or their own windows.

use egui::RichText;

impl super::PlumSpiritApp {
    /// Opens a built-in plugin's window.
    pub(super) fn open_plugin(&mut self, id: plumspirit_engine::plugins::PluginId) {
        use plumspirit_engine::plugins::PluginId;
        match id {
            PluginId::ImageToMidi => self.open_image_to_midi(),
            PluginId::HzChop => self.open_tool("hz_chop"),
            PluginId::Safor => self.open_tool("safor"),
            PluginId::Spiderweb => self.open_spiderweb(),
            PluginId::AudioReference => self.open_reference(),
        }
    }

    /// Help → About PlumSpirit and Built-in Plugins. Modeless.
    pub(super) fn about_window_ui(&mut self, ctx: &egui::Context) {
        if !self.about_window {
            return;
        }
        let mut open = true;
        // B1-0175: the official logo beside the name.
        let logo = self.brand.sized(ctx, crate::assets::Brand::Logo, 64.0);
        egui::Window::new("About PlumSpirit")
            .id(egui::Id::new("plumspirit-about"))
            .open(&mut open)
            .default_width(460.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::from_texture(logo).alt_text("PlumSpirit logo"));
                    ui.vertical(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(format!(
                                "{} (version {})",
                                crate::RELEASE_NAME,
                                env!("CARGO_PKG_VERSION")
                            ))
                            .strong(),
                        );
                        ui.label("A MIDI sequencer for Black MIDI.");
                    });
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Built-in plugins").strong());
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for p in plumspirit_engine::plugins::PLUGINS {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!("{} · {}", p.full_name, p.kind.label()))
                                    .strong(),
                            );
                            ui.label(p.credit());
                            ui.label(RichText::new(format!("Original: {}", p.upstream)).weak());
                            ui.label(RichText::new(format!("License: {}", p.licence)).weak());
                            ui.label(p.help);
                        }
                    });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Third-party software and licenses: THIRD_PARTY_NOTICES.md and THIRD_PARTY_LICENSES.txt next to PlumSpirit.exe.",
                    )
                    .weak(),
                );
            });
        if !open {
            self.about_window = false;
        }
    }
}

/// "Original by …" and its licence, for the top of a plugin window.
pub fn credit_line(p: &plumspirit_engine::plugins::PluginInfo) -> String {
    format!("{}. Original license: {}.", p.credit(), p.licence)
}

/// The credit line shown at the top of a plugin's tool window.
pub fn tool_credit(tool_id: &str) -> Option<String> {
    plumspirit_engine::plugins::for_tool(tool_id).map(credit_line)
}
