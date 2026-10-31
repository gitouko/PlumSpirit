//! Multi-select list of Onion Skin source tracks ("Specific Tracks" mode).

use crate::ui::theme::{self, Palette, size, space};
use egui::{Id, Modal, RichText, Sense, Vec2};
use plumspirit_engine::model::{PlumDocument, TrackId};

pub enum ChooserOutcome {
    Open,
    Cancel,
    Done,
}

const ROW_H: f32 = 24.0;

/// Shows the chooser. `selection` is edited in place; the caller applies it on
/// `Done`. The active track cannot be its own ghost and is shown disabled.
pub fn show(
    ctx: &egui::Context,
    doc: &PlumDocument,
    ordinals: &[u32],
    active: usize,
    selection: &mut Vec<TrackId>,
    pal: &Palette,
    dark: bool,
) -> ChooserOutcome {
    let mut outcome = ChooserOutcome::Open;
    let modal = Modal::new(Id::new("plumspirit-onion-chooser")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        ui.set_max_width(420.0);
        ui.label(
            RichText::new("Onion Skin Tracks")
                .size(size::EMPHASIS)
                .strong(),
        );
        ui.add_space(space::MICRO);
        ui.label(
            RichText::new(
                "Checked tracks are drawn as translucent ghosts behind the active track.",
            )
            .size(size::SMALL)
            .color(pal.text_weak),
        );
        ui.add_space(space::NORMAL);
        let n = doc.track_count();
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .show_rows(ui, ROW_H, n, |ui, range| {
                for i in range {
                    let Some(t) = doc.track(i) else { continue };
                    let id = t.id();
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        let is_active = i == active;
                        let mut on = selection.contains(&id);
                        let name = t.label_with(ordinals.get(i).copied().unwrap_or(0));
                        let label = if is_active {
                            format!("{}  {name} (active)", i + 1)
                        } else {
                            format!("{}  {name}", i + 1)
                        };
                        let (sw, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
                        ui.painter().rect_filled(
                            sw,
                            0.0,
                            theme::note_palette(ui.ctx()).track_color(
                                id.0,
                                t.props().onion_color,
                                dark,
                                255,
                            ),
                        );
                        let resp = ui.add_enabled(!is_active, egui::Checkbox::new(&mut on, label));
                        if resp.changed() {
                            if on {
                                if !selection.contains(&id) {
                                    selection.push(id);
                                }
                            } else {
                                selection.retain(|x| *x != id);
                            }
                        }
                    });
                }
            });
        ui.add_space(space::NORMAL);
        ui.horizontal(|ui| {
            if ui.button("Select All").clicked() {
                selection.clear();
                selection.extend(
                    doc.tracks()
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != active)
                        .map(|(_, t)| t.id()),
                );
            }
            if ui.button("Clear").clicked() {
                selection.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                if ui.button("Done").clicked() {
                    outcome = ChooserOutcome::Done;
                }
                if ui.button("Cancel").clicked() {
                    outcome = ChooserOutcome::Cancel;
                }
            });
        });
    });
    if modal.should_close() && matches!(outcome, ChooserOutcome::Open) {
        outcome = ChooserOutcome::Cancel;
    }
    outcome
}
