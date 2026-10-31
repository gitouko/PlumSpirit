//! The quiet start screen shown when no project is open.

use crate::ui::theme::{Palette, size, space};
use egui::{Button, RichText, Vec2};

/// The start screen's wordmark height (points; B1-0175).
pub const WORDMARK_HEIGHT: f32 = 96.0;

pub enum EmptyAction {
    None,
    New,
    Open,
    /// v0.8: a Recent Files entry.
    Recent(crate::recent::RecentAction),
    /// B1-0051: the quick-start card's buttons.
    QuickStart,
    HideOrientation,
}

/// B1-0051: the steps the start screen's quick-start card lists (the
/// Quick Start Help topic explains each one).
pub const ORIENTATION_STEPS: [&str; 6] = [
    "Create a project or open a MIDI file.",
    "Arrange clips in the Track List.",
    "Open a clip in the Piano Roll (double-click).",
    "Draw and edit notes; right-click a note for every tool.",
    "Play with Space.",
    "Save the project (.lup), then Export MIDI (.mid).",
];

/// What the start screen lists under "Recent".
pub struct RecentInput<'a> {
    pub entries: &'a [crate::recent::RecentEntry],
    pub missing: &'a [bool],
    pub now: u64,
}

pub fn empty_state(
    ui: &mut egui::Ui,
    wordmark: Option<egui::load::SizedTexture>,
    pal: &Palette,
    loading: Option<&str>,
    recent: RecentInput<'_>,
    orientation: bool,
) -> EmptyAction {
    let mut action = EmptyAction::None;
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, pal.roll_bg);
    let shown = recent
        .entries
        .len()
        .min(crate::ui::recent_list::START_SCREEN_ENTRIES);
    let list_h = if shown == 0 {
        0.0
    } else {
        40.0 + 22.0 * shown as f32
    };
    let card_h = if orientation && loading.is_none() {
        28.0 + 20.0 * ORIENTATION_STEPS.len() as f32 + 44.0
    } else {
        0.0
    };
    let content_h = WORDMARK_HEIGHT + 12.0 + 18.0 + 16.0 + 28.0 + 8.0 + 18.0 + list_h + card_h;
    let top_pad = ((rect.height() - content_h) / 2.0).max(space::SECTION);
    ui.vertical_centered(|ui| {
        ui.add_space(top_pad);
        // B1-0175: the plum and the wordmark (the name is in the image).
        match wordmark {
            Some(w) => {
                ui.add(egui::Image::from_texture(w).alt_text("PlumSpirit"));
            }
            None => {
                ui.label(
                    RichText::new("PlumSpirit")
                        .size(size::TITLE)
                        .color(pal.text),
                );
            }
        }
        ui.add_space(space::SECTION);
        ui.label(
            RichText::new(crate::RELEASE_LABEL)
                .size(size::SMALL)
                .color(pal.text_weak),
        );
        ui.add_space(space::NORMAL + 8.0);
        if let Some(label) = loading {
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 180.0).max(0.0) / 2.0);
                ui.spinner();
                ui.label(label);
            });
        } else {
            ui.horizontal(|ui| {
                let w = 150.0 * 2.0 + space::NORMAL;
                ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                if ui
                    .add(Button::new("New Project").min_size(Vec2::new(150.0, 28.0)))
                    .clicked()
                {
                    action = EmptyAction::New;
                }
                if ui
                    .add(Button::new("Open…").min_size(Vec2::new(150.0, 28.0)))
                    .clicked()
                {
                    action = EmptyAction::Open;
                }
            });
            ui.add_space(space::NORMAL);
            ui.label(
                RichText::new(format!(
                    "{}  ·  {}  —  opens PlumSpirit projects (.lup) and MIDI files",
                    crate::app::commands::Command::NewProject.shortcut(),
                    crate::app::commands::Command::Open.shortcut()
                ))
                .size(size::SMALL)
                .color(pal.text_weak),
            );
            if orientation {
                ui.add_space(space::SECTION + 4.0);
                if let Some(a) = orientation_card(ui, pal) {
                    action = a;
                }
            }
            if !recent.entries.is_empty() {
                ui.add_space(space::SECTION + 8.0);
                if let Some(a) = crate::ui::recent_list::start_list(
                    ui,
                    pal,
                    recent.entries,
                    recent.missing,
                    recent.now,
                ) {
                    action = EmptyAction::Recent(a);
                }
            }
        }
    });
    action
}

/// B1-0051: the dismissible quick-start card (Help → Show the Quick Start on
/// the Start Screen brings it back).
fn orientation_card(ui: &mut egui::Ui, pal: &Palette) -> Option<EmptyAction> {
    let mut action = None;
    let w = 420.0_f32.min(ui.available_width() - 16.0).max(200.0);
    ui.allocate_ui_with_layout(
        Vec2::new(w, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            egui::Frame::new()
                .fill(pal.panel_bg)
                .stroke(egui::Stroke::new(1.0, pal.panel_border))
                .corner_radius(crate::ui::theme::CORNER)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(w - 20.0);
                    ui.label(
                        RichText::new("New to PlumSpirit?")
                            .size(size::EMPHASIS)
                            .strong()
                            .color(pal.text),
                    );
                    for (i, s) in ORIENTATION_STEPS.iter().enumerate() {
                        ui.label(
                            RichText::new(format!("{}.  {s}", i + 1))
                                .size(size::BODY)
                                .color(pal.text),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button("Open the Quick Start")
                            .on_hover_text(format!(
                                "Each step in detail, in PlumSpirit Help{}.",
                                crate::app::keybinds::hint(crate::app::commands::Command::ShowHelp)
                            ))
                            .clicked()
                        {
                            action = Some(EmptyAction::QuickStart);
                        }
                        if ui
                            .button("Don't Show This Again")
                            .on_hover_text(
                                "Help → Show the Quick Start on the Start Screen brings it back.",
                            )
                            .clicked()
                        {
                            action = Some(EmptyAction::HideOrientation);
                        }
                    });
                });
        },
    );
    action
}
