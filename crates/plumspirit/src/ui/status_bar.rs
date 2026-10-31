//! Bottom status bar: active track, note counts, pointer position and job status.

use crate::ui::theme::{Palette, size, space};
use egui::RichText;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Success,
    Warning,
    Error,
}

pub struct StatusInput<'a> {
    pub left: String,
    pub position: String,
    pub job: Option<(String, Option<f32>)>,
    /// L-H02: the running job may be canceled safely (loading: nothing is
    /// written) — a Cancel link is shown with it.
    pub job_cancelable: bool,
    pub message: Option<(&'a str, StatusKind)>,
    pub has_details: bool,
    /// "CPU: 3.2%  |  RAM: 184 MiB / 2.0 GiB" (bottom right), when shown.
    pub metrics: Option<String>,
    /// "Last: Saved project — 1.31 s" (how long the last significant operation
    /// took), when any.
    pub last_task: Option<String>,
}

/// What the status bar asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusOut {
    /// The "Details" link was clicked.
    pub details: bool,
    /// L-H02: the running job's Cancel link was clicked.
    pub cancel_job: bool,
}

pub fn status_bar(ui: &mut egui::Ui, input: &StatusInput<'_>, pal: &Palette) -> StatusOut {
    let mut details = false;
    let mut cancel_job = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = space::NORMAL;
        let small = |s: &str| RichText::new(s).size(size::SMALL);
        let width = ui.available_width();
        let left_width = (width * 0.35).min(420.0);
        let row_height = ui.text_style_height(&egui::TextStyle::Small);
        let left = if input.position.is_empty() { input.left.clone() } else { format!("{} · {}", input.left, input.position) };
        let mut hint = left.clone();
        if width < 900.0 && let Some(m) = &input.metrics { hint.push_str(&format!("\n{m}")); }
        if width < 1100.0 && let Some(t) = &input.last_task { hint.push_str(&format!("\n{t}")); }
        ui.allocate_ui_with_layout(egui::vec2(left_width,row_height),egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
            ui.add(egui::Label::new(small(&left).color(pal.text)).truncate()).on_hover_text(hint);
        });
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),row_height),egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
            if width >= 900.0 && let Some(m) = &input.metrics {
                ui.label(small(m).color(pal.text_weak)).on_hover_text(
                    "CPU: PlumSpirit's share of the whole computer's processor time over the last second (100% = every logical CPU busy). RAM: PlumSpirit's working set — the physical memory it uses now, including shared pages — and the computer's total memory. Measured locally once per second; N/A when the system does not report it.",
                );
                ui.separator();
            }
            if width >= 1100.0 && input.job.is_none()
                && let Some(t) = &input.last_task
            {
                ui.label(small(t).color(pal.text_weak)).on_hover_text(
                    "How long the last significant operation took (monotonic clock, from start to finish). View → Recent Operations lists the last 20.",
                );
                ui.separator();
            }
            if let Some((label, fraction)) = &input.job {
                if input.job_cancelable
                    && ui
                        .link(small("Cancel"))
                        .on_hover_text("Stop loading. Nothing has been written; the project open before stays as it is.")
                        .clicked()
                {
                    cancel_job = true;
                }
                if let Some(f) = fraction {
                    ui.add(
                        egui::ProgressBar::new(*f)
                            .corner_radius(super::theme::CORNER)
                            .desired_width(120.0)
                            .desired_height(10.0),
                    );
                    ui.add(egui::Label::new(small(&format!("{label} · {:.0}%", f * 100.0)).color(pal.text)).truncate()).on_hover_text(label);
                } else {
                    ui.spinner();
                    ui.add(egui::Label::new(small(label).color(pal.text)).truncate()).on_hover_text(label);
                }
            } else if let Some((msg, kind)) = input.message {
                if input.has_details && ui.link(small("Details")).clicked() {
                    details = true;
                }
                let color = match kind {
                    StatusKind::Info | StatusKind::Success => pal.text_weak,
                    StatusKind::Warning => pal.warning,
                    StatusKind::Error => pal.error,
                };
                ui.add(egui::Label::new(small(msg).color(color)).truncate()).on_hover_text(msg);
            }
        });
    });
    StatusOut {
        details,
        cancel_job,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_status_fields_do_not_overlap_at_narrow_widths() {
        for width in [400.0, 548.0, 960.0, 1366.0] {
            let ctx = egui::Context::default();
            let input = super::StatusInput {
                left: "Track 1 of 92 · 14,650,390 notes in project · 10,000 selected".into(),
                position: "Bar 123 · Beat 4 · Tick 1,234".into(),
                job: None,
                job_cancelable: false,
                message: Some((
                    "Project data released; saved files remain on disk.",
                    super::StatusKind::Info,
                )),
                has_details: true,
                metrics: Some("CPU: 15.5% | RAM: 1.5 GiB / 63.7 GiB".into()),
                last_task: Some("Last: Imported MIDI — 1.25 s".into()),
            };
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 80.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        super::status_bar(ui, &input, &crate::ui::theme::DARK);
                    });
                },
            );
            out.textures_delta.clear();
            let mut rects = Vec::new();
            for shape in &out.shapes {
                if let egui::Shape::Text(t) = &shape.shape {
                    if t.galley.text().starts_with("Track 1 of 92") {
                        assert!(
                            t.pos.x >= shape.clip_rect.left() - 1.0,
                            "primary text must start inside its region at {width}"
                        );
                    }
                    let rect = t
                        .galley
                        .rect
                        .translate(t.pos.to_vec2())
                        .intersect(shape.clip_rect);
                    if rect.is_positive() {
                        rects.push(rect);
                    }
                }
            }
            assert!(
                rects.len() >= 3,
                "primary text, status message and Details must be visible at {width}"
            );
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    assert!(!a.intersects(*b), "{width}: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    /// L-H02: a loading job shows a Cancel link next to its stage and
    /// progress (never 100 % while it runs); a click asks to cancel it.
    #[test]
    fn a_loading_job_offers_cancel_and_a_click_asks_for_it() {
        let ctx = egui::Context::default();
        let run = |cancelable: bool, events: Vec<egui::Event>| {
            let input = super::StatusInput {
                left: "Track 1".into(),
                position: String::new(),
                job: Some(("Loading a.mid: reading… 3.0 s".into(), Some(0.42))),
                job_cancelable: cancelable,
                message: None,
                has_details: false,
                metrics: None,
                last_task: None,
            };
            let mut got = super::StatusOut::default();
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1366.0, 40.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        got = super::status_bar(ui, &input, &crate::ui::theme::DARK);
                    });
                },
            );
            out.textures_delta.clear();
            let cancel = out.shapes.iter().find_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text() == "Cancel" => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()).center())
                }
                _ => None,
            });
            let texts: Vec<String> = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            (got, cancel, texts)
        };
        let (_, none, texts) = run(false, vec![]);
        assert!(none.is_none());
        assert!(
            texts
                .iter()
                .any(|t| t.contains("reading") && t.contains("42%")),
            "{texts:?}"
        );
        let (got, at, _) = run(true, vec![]);
        assert!(!got.cancel_job);
        let at = at.expect("a Cancel link");
        run(true, vec![egui::Event::PointerMoved(at)]);
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (got, ..) = run(true, vec![press(true), press(false)]);
        assert!(got.cancel_job);
    }
}
