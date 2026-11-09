//! Bounded native editor layers. No callbacks, hit regions or document mutation.
use super::{PlumSpiritApp, canvas};
use crate::lua::{
    Capability,
    overlays::{self, Frame},
    ui::images,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
struct View {
    plugin: String,
    frame: Arc<Frame>,
    textures: BTreeMap<u64, images::Prepared>,
}
#[derive(Default)]
pub(super) struct Overlays {
    views: Vec<View>,
    disabled: BTreeSet<(String, usize, String)>,
}
fn permitted(app: &PlumSpiritApp, plugin: &str, index: usize) -> bool {
    let prefs = &app.settings.lua_plugins;
    let state = prefs.installed.get(plugin);
    prefs.run_plugins
        && state.is_some_and(|s| s.enabled)
        && app
            .lua_plugins
            .as_ref()
            .and_then(|m| m.plugin(plugin))
            .is_some_and(|p| {
                let caps = p.effective(state);
                p.status == crate::lua::plugins::Status::Ready
                    && caps.contains(&Capability::UiOverlay)
                    && caps.contains(&if index == 0 {
                        Capability::ReadProject
                    } else {
                        Capability::ReadPlayback
                    })
            })
}
impl PlumSpiritApp {
    pub(super) fn reset_lua_upload_budget(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        if self.lua_upload_frame != Some(frame) {
            self.lua_upload_frame = Some(frame);
            self.lua_upload_pixels = images::MAX_UPLOAD_PIXELS;
        }
    }
    pub(super) fn paint_lua_overlays(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        index: usize,
        slot: Option<egui::layers::ShapeIdx>,
    ) {
        self.reset_lua_upload_budget(ui.ctx());
        if !self.lua_views.enabled && self.lua_overlays.views.is_empty() {
            return;
        }
        self.collect_lua_overlays();
        let current = self.lua_view_snapshots();
        let mut state = std::mem::take(&mut self.lua_overlays);
        state.views.retain(|v| {
            v.frame.active()
                && permitted(self, &v.plugin, v.frame.view.facts.index())
                && current[v.frame.view.facts.index()] == Some(v.frame.view)
        });
        let mut shapes = Vec::new();

        for v in &mut state.views {
            if v.frame.view.facts.index() != index
                || state
                    .disabled
                    .contains(&(v.plugin.clone(), index, v.frame.id.clone()))
            {
                continue;
            }
            let mut visible = BTreeSet::new();
            shapes.extend(canvas::display_list(
                ui,
                painter,
                v.frame.view.facts.rect(),
                &v.frame.primitives.as_ref().expect("visible frame").items,
                canvas::Images {
                    cached: &mut v.textures,
                    visible: &mut visible,
                    upload_pixels: &mut self.lua_upload_pixels,
                },
            ));
            v.textures.retain(|id, _| visible.contains(id));
        }
        if let Some(slot) = slot {
            painter.set(slot, egui::Shape::Vec(shapes));
        } else {
            painter.extend(shapes);
        }
        self.lua_overlays = state;
    }
    pub(super) fn collect_lua_overlays(&mut self) {
        self.prune_lua_overlays();
        let current = self.lua_view_snapshots();
        // One atomic batch per worker, at most8batches per native surface pass.
        for _ in 0..overlays::MAX_VISIBLE {
            let Some((plugin, batch)) = self
                .lua_plugins
                .as_ref()
                .and_then(|m| m.poll_overlay_request())
            else {
                break;
            };
            if batch.frames.iter().any(|f| {
                !f.active()
                    || !permitted(self, &plugin, f.view.facts.index())
                    || current[f.view.facts.index()] != Some(f.view)
            }) {
                continue;
            }
            let new = batch
                .frames
                .iter()
                .filter(|f| {
                    f.primitives.is_some()
                        && !self.lua_overlays.views.iter().any(|v| {
                            v.plugin == plugin
                                && v.frame.id == f.id
                                && v.frame.view.facts.index() == f.view.facts.index()
                        })
                })
                .count();
            let removed = batch
                .frames
                .iter()
                .filter(|f| {
                    f.primitives.is_none()
                        && self.lua_overlays.views.iter().any(|v| {
                            v.plugin == plugin
                                && v.frame.id == f.id
                                && v.frame.view.facts.index() == f.view.facts.index()
                        })
                })
                .count();
            if self.lua_overlays.views.len() + new - removed > overlays::MAX_VISIBLE {
                continue;
            }
            for frame in batch.frames {
                let old = self.lua_overlays.views.iter().position(|v| {
                    v.plugin == plugin
                        && v.frame.id == frame.id
                        && v.frame.view.facts.index() == frame.view.facts.index()
                });
                let textures = old
                    .map(|i| self.lua_overlays.views.remove(i).textures)
                    .unwrap_or_default();
                if frame.primitives.is_some() {
                    self.lua_overlays.views.push(View {
                        plugin: plugin.clone(),
                        frame,
                        textures,
                    });
                }
            }
        }
    }
    pub(super) fn prune_lua_overlays(&mut self) {
        if self.lua_overlays.views.is_empty() {
            return;
        }
        let current = self.lua_view_snapshots();
        let mut state = std::mem::take(&mut self.lua_overlays);
        state.views.retain(|v| {
            v.frame.active()
                && permitted(self, &v.plugin, v.frame.view.facts.index())
                && current[v.frame.view.facts.index()] == Some(v.frame.view)
        });
        self.lua_overlays = state;
    }
}
impl Overlays {
    pub(super) fn controls(&mut self, ui: &mut egui::Ui) {
        if self.views.is_empty() {
            return;
        }
        ui.collapsing("Editor overlays", |ui| {
            ui.label("Disable an overlay here without changing its plugin's other features.");
            for v in &self.views {
                let key = (
                    v.plugin.clone(),
                    v.frame.view.facts.index(),
                    v.frame.id.clone(),
                );
                let mut enabled = !self.disabled.contains(&key);
                if ui
                    .checkbox(
                        &mut enabled,
                        format!(
                            "{} / {} ({})",
                            v.plugin,
                            v.frame.id,
                            if key.1 == 0 { "Piano Roll" } else { "Player" }
                        ),
                    )
                    .changed()
                {
                    if enabled {
                        self.disabled.remove(&key);
                    } else {
                        if self.disabled.len() >= 64 {
                            self.disabled.pop_first();
                        }
                        self.disabled.insert(key);
                    }
                }
            }
        });
    }
}
#[cfg(test)]
#[path = "lua_overlay_tests.rs"]
mod tests;
