//! The compact transport / SoundFont row under the editing toolbar.
//!
//! `|◀  ◀◀  ▶  ▶▶  ▶|   Bar 12 · Beat 3   0:23.45   [Follow]   Tempo [120] BPM   ● Output ▾   SoundFont: [name] Browse…`
//!
//! Icons are drawn as shapes (not font glyphs) so they look the same with every UI
//! font. The row only reports intents; the app applies them.

use crate::app::commands::Command;
use crate::audio::TransportState;
use crate::audio::midi_out::{OutputInfo, OutputTarget};
use crate::ui::theme::{Palette, size, space};
use egui::{Button, Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Icon {
    Start,
    PrevMeasure,
    Play,
    Pause,
    Stop,
    NextMeasure,
    End,
}

fn tri(p: &egui::Painter, c: Pos2, w: f32, h: f32, left: bool, color: Color32) {
    let (a, b) = if left {
        (c.x + w / 2.0, c.x - w / 2.0)
    } else {
        (c.x - w / 2.0, c.x + w / 2.0)
    };
    p.add(Shape::convex_polygon(
        vec![
            Pos2::new(a, c.y - h / 2.0),
            Pos2::new(b, c.y),
            Pos2::new(a, c.y + h / 2.0),
        ],
        color,
        Stroke::NONE,
    ));
}

fn paint_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let c = r.center();
    let bar = |x: f32| {
        p.rect_filled(
            Rect::from_center_size(Pos2::new(x, c.y), Vec2::new(2.0, 10.0)),
            0.0,
            color,
        )
    };
    match icon {
        Icon::Play => tri(p, c + Vec2::new(1.0, 0.0), 9.0, 11.0, false, color),
        Icon::Pause => {
            p.rect_filled(
                Rect::from_center_size(c - Vec2::new(2.5, 0.0), Vec2::new(3.0, 10.0)),
                0.0,
                color,
            );
            p.rect_filled(
                Rect::from_center_size(c + Vec2::new(2.5, 0.0), Vec2::new(3.0, 10.0)),
                0.0,
                color,
            );
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, Vec2::new(9.0, 9.0)), 0.0, color);
        }
        Icon::Start => {
            bar(c.x - 5.0);
            tri(p, c + Vec2::new(1.0, 0.0), 8.0, 10.0, true, color);
        }
        Icon::End => {
            bar(c.x + 5.0);
            tri(p, c - Vec2::new(1.0, 0.0), 8.0, 10.0, false, color);
        }
        Icon::PrevMeasure => {
            tri(p, c - Vec2::new(3.5, 0.0), 7.0, 10.0, true, color);
            tri(p, c + Vec2::new(3.5, 0.0), 7.0, 10.0, true, color);
        }
        Icon::NextMeasure => {
            tri(p, c - Vec2::new(3.5, 0.0), 7.0, 10.0, false, color);
            tri(p, c + Vec2::new(3.5, 0.0), 7.0, 10.0, false, color);
        }
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    enabled: bool,
    active: bool,
    tip: &str,
) -> bool {
    let size = Vec2::new(28.0, 22.0);
    let mut b = Button::new("").min_size(size);
    if active {
        // Fill only: a button stroke would enlarge the frame and make the row jump.
        b = b.fill(pal.accent_weak);
    }
    let resp = ui.add_enabled(enabled, b).on_hover_text(tip);
    if active {
        ui.painter().rect_stroke(
            resp.rect,
            ui.visuals().widgets.inactive.corner_radius,
            Stroke::new(1.0, pal.accent),
            egui::StrokeKind::Inside,
        );
    }
    let color = if enabled {
        pal.text
    } else {
        pal.text_weak.gamma_multiply(0.6)
    };
    paint_icon(ui.painter(), resp.rect, icon, color);
    resp.clicked()
}

/// Follow Playhead as shown in the transport row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowShown {
    Off,
    On,
    /// On, but paused because the user scrolled during playback.
    Paused,
}

pub struct TransportInput<'a> {
    pub has_doc: bool,
    pub state: TransportState,
    pub follow: FollowShown,
    pub position_text: &'a str,
    pub time_text: &'a str,
    /// Tempo field is editable (metrical timing).
    pub tempo_enabled: bool,
    pub tempo_tip: &'a str,
    pub soundfont_name: &'a str,
    pub soundfont_tip: &'a str,
    pub status: Option<(&'a str, bool)>,
    /// Loop: on, and whether a range is set (with its text for the tooltip).
    pub loop_on: bool,
    pub loop_text: Option<String>,
    pub speed: &'a str,
    /// B1-0135: the default output chip — label, tooltip (provider, health),
    /// warning state — and the chooser's entries.
    pub output_label: &'a str,
    pub output_tip: &'a str,
    pub output_warn: bool,
    pub output_open: bool,
    pub outputs: &'a [OutputInfo],
    pub output_current: &'a OutputTarget,
    pub outputs_listing: bool,
    /// B1-0080: the Edit / Anchor playhead is on (Stop's tooltip).
    pub anchor_on: bool,
}

#[derive(Default)]
pub struct TransportOutput {
    pub command: Option<Command>,
    /// The tempo field was committed (Enter or focus lost) with this text.
    pub tempo_committed: bool,
    pub tempo_focused: bool,
    /// The speed field was committed (Enter) with this text.
    pub speed: Option<String>,
    /// A default output was chosen / the list should be refreshed / Audio
    /// Settings should open.
    pub output_chosen: Option<OutputTarget>,
    pub outputs_refresh: bool,
    pub audio_settings: bool,
}

/// The chooser's entries (transport menu and Audio Settings): every output,
/// unavailable ones greyed with their reason; the current one stays visible.
pub fn output_entries(
    ui: &mut egui::Ui,
    outputs: &[OutputInfo],
    current: &OutputTarget,
) -> Option<OutputTarget> {
    let mut chosen = None;
    for o in outputs {
        let selected = &o.target == current;
        let mut text = o.label();
        if !o.available {
            text.push_str(" (unavailable)");
        }
        let tip = match &o.reason {
            Some(r) => format!("{} — {r}", o.kind().name()),
            None => o.kind().name().to_owned(),
        };
        let resp = ui
            .add_enabled(o.available || selected, Button::selectable(selected, text))
            .on_hover_text(&tip)
            .on_disabled_hover_text(&tip);
        if resp.clicked() && !selected {
            chosen = Some(o.target.clone());
        }
    }
    chosen
}

pub fn transport_bar(
    ui: &mut egui::Ui,
    input: &TransportInput<'_>,
    tempo_text: &mut String,
    speed_field: &mut crate::ui::toolbar::NumericField,
    pal: &Palette,
) -> TransportOutput {
    let mut out = TransportOutput::default();
    // B1-0062: wraps instead of clipping when the window is narrow in points.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::MICRO;
        let d = input.has_doc;
        let playing = input.state.is_running();
        use crate::app::keybinds::hint;
        let tip = format!("Go to the beginning{}", hint(Command::GoToStart));
        if icon_button(ui, pal, Icon::Start, d, false, &tip) {
            out.command = Some(Command::GoToStart);
        }
        let tip = format!("Previous measure{}", hint(Command::PreviousMeasure));
        if icon_button(ui, pal, Icon::PrevMeasure, d, false, &tip) {
            out.command = Some(Command::PreviousMeasure);
        }
        let (icon, tip) = if playing {
            (Icon::Pause, format!("Pause{}", hint(Command::PlayPause)))
        } else {
            (Icon::Play, format!("Play{}", hint(Command::PlayPause)))
        };
        if icon_button(ui, pal, icon, d, playing, &tip) {
            out.command = Some(Command::PlayPause);
        }
        let stop_tip = format!(
            "Stop{}: back to {}",
            hint(Command::Stop),
            if input.anchor_on {
                "the Edit / Anchor playhead"
            } else {
                "where playback started"
            }
        );
        if icon_button(ui, pal, Icon::Stop, d, false, &stop_tip) {
            out.command = Some(Command::Stop);
        }
        let tip = format!("Next measure{}", hint(Command::NextMeasure));
        if icon_button(ui, pal, Icon::NextMeasure, d, false, &tip) {
            out.command = Some(Command::NextMeasure);
        }
        let tip = format!("Go to the end of the song{}", hint(Command::GoToEnd));
        if icon_button(ui, pal, Icon::End, d, false, &tip) {
            out.command = Some(Command::GoToEnd);
        }
        ui.add_space(space::NORMAL);
        let pos_resp = ui.add_sized(
            Vec2::new(104.0, 22.0),
            egui::Label::new(RichText::new(input.position_text).color(pal.text)).truncate(),
        );
        pos_resp.on_hover_text("Playback / edit position");
        ui.add_sized(
            Vec2::new(58.0, 22.0),
            egui::Label::new(RichText::new(input.time_text).color(pal.text_weak).size(size::SMALL)),
        );
        // Follow Playhead toggle.
        let (text, active, tip) = match input.follow {
            FollowShown::On => (
                RichText::new("Follow").color(pal.text),
                true,
                "Follow Playhead is on: the piano roll scrolls with playback. Scrolling by hand pauses it. Click to turn it off.",
            ),
            FollowShown::Paused => (
                RichText::new("Follow (paused)").color(pal.warning),
                false,
                "Follow Playhead is paused because you scrolled the view during playback. Click to follow the playhead again.",
            ),
            FollowShown::Off => (
                RichText::new("Follow").color(pal.text_weak),
                false,
                "Follow Playhead is off: the view stays where you put it. Click to turn it on.",
            ),
        };
        let mut b = Button::new(text).min_size(Vec2::new(60.0, 22.0));
        if active {
            b = b.fill(pal.accent_weak);
        }
        let resp = ui.add_enabled(d, b).on_hover_text(tip);
        if active {
            ui.painter().rect_stroke(
                resp.rect,
                ui.visuals().widgets.inactive.corner_radius,
                Stroke::new(1.0, pal.accent),
                egui::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            out.command = Some(Command::ToggleFollow);
        }
        // Loop toggle.
        let (text, tip) = match (&input.loop_text, input.loop_on) {
            (Some(r), true) => (
                RichText::new("Loop").color(pal.text),
                format!("Loop is on: playback repeats {r}. Click to turn it off{}. Shift+drag on the ruler to set a new range.", hint(Command::ToggleLoop)),
            ),
            (Some(r), false) => (
                RichText::new("Loop").color(pal.text_weak),
                format!("Loop is off (range {r}). Click to turn it on{}.", hint(Command::ToggleLoop)),
            ),
            (None, _) => (
                RichText::new("Loop").color(pal.text_weak),
                "No loop range yet: click to loop the selection (a Select Range, selected notes or a chapter), or Shift+drag on the ruler.".to_owned(),
            ),
        };
        let mut b = Button::new(text).min_size(Vec2::new(44.0, 22.0));
        if input.loop_on {
            b = b.fill(pal.accent_weak);
        }
        let resp = ui.add_enabled(d, b).on_hover_text(tip);
        if input.loop_on {
            ui.painter().rect_stroke(
                resp.rect,
                ui.visuals().widgets.inactive.corner_radius,
                Stroke::new(1.0, pal.accent),
                egui::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            out.command = Some(Command::ToggleLoop);
        }
        ui.add_space(space::MICRO);
        out.speed = crate::ui::toolbar::wrap_group(ui, "speed", true, |ui| {
            ui.label("Speed");
            let typed = crate::ui::toolbar::numeric_field(
                ui,
                speed_field,
                input.speed,
                "1",
                46.0,
                true,
                "Playback speed (audition only; the project tempo and exported files are unchanged). Any positive number, e.g. 0.25, 0.5, 2, 10, 1000; press Enter. Notes keep their pitch; SoundFont envelopes (attack, release) are not time-stretched.",
            );
            ui.label(RichText::new("×").color(pal.text_weak));
            typed
        });
        ui.add_space(space::NORMAL);
        ui.separator();
        crate::ui::toolbar::wrap_group(ui, "tempo", d && input.tempo_enabled, |ui| {
            ui.label("Tempo");
            let resp = ui
                .add(
                    egui::TextEdit::singleline(tempo_text)
                        .desired_width(66.0)
                        .hint_text("120"),
                )
                .on_hover_text(input.tempo_tip);
            out.tempo_focused = resp.has_focus();
            if resp.lost_focus() {
                out.tempo_committed = true;
            }
            ui.label(RichText::new("BPM").color(pal.text_weak));
        });
        ui.add_space(space::NORMAL);
        ui.separator();
        // Default output (B1-0135): provider and health, a chooser.
        let dot = if input.output_warn {
            pal.warning
        } else if input.output_open {
            Color32::from_rgb(0x4C, 0xA0, 0x6A)
        } else {
            pal.text_weak
        };
        let mut chip = egui::text::LayoutJob::default();
        chip.append("● ", 0.0, egui::TextFormat::simple(egui::FontId::proportional(size::SMALL), dot));
        chip.append(
            input.output_label,
            0.0,
            egui::TextFormat::simple(egui::FontId::proportional(size::BODY), pal.text),
        );
        chip.wrap = egui::text::TextWrapping::truncate_at_width(150.0);
        let resp = ui
            .menu_button(chip, |ui| {
                ui.label(RichText::new("Tracks set to Internal play on").small().weak());
                if let Some(t) = output_entries(ui, input.outputs, input.output_current) {
                    out.output_chosen = Some(t);
                    ui.close();
                }
                if input.outputs_listing {
                    ui.label(RichText::new("Looking for MIDI outputs…").small().weak());
                }
                ui.separator();
                if ui.button("Refresh").on_hover_text("Look for MIDI devices and OmniMIDI again.").clicked() {
                    out.outputs_refresh = true;
                }
                if ui
                    .button("Audio Settings…")
                    .on_hover_text("The audio device, buffer, render-ahead and synthesizer threads.")
                    .clicked()
                {
                    out.audio_settings = true;
                    ui.close();
                }
            })
            .response;
        resp.on_hover_text(input.output_tip);
        ui.separator();
        // Remaining width: the name truncates, Browse… stays visible; when
        // less than a readable name is left (B1-0062: large UI scale), the
        // SoundFont group starts the next row.
        let browse_w = 72.0;
        let status_w = if input.status.is_some() { 150.0 } else { 0.0 };
        if ui.available_size_before_wrap().x < 80.0 + 100.0 + browse_w {
            ui.end_row();
        }
        ui.label("SoundFont");
        let name_w = (ui.available_size_before_wrap().x - browse_w - status_w - 16.0)
            .clamp(28.0, 320.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(name_w, 22.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, pal.roll_bg);
        p.rect_stroke(rect, 0.0, Stroke::new(1.0, pal.panel_border), egui::StrokeKind::Inside);
        let mut job = egui::text::LayoutJob::simple_singleline(
            input.soundfont_name.to_owned(),
            egui::FontId::proportional(size::BODY),
            pal.text,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(name_w - 12.0);
        let galley = p.layout_job(job);
        p.galley(
            Pos2::new(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text,
        );
        resp.on_hover_text(input.soundfont_tip);
        if ui
            .add_sized(Vec2::new(browse_w, 22.0), Button::new("Browse…"))
            .on_hover_text("Choose an SF2 SoundFont for this project (also becomes the default for new projects)")
            .clicked()
        {
            out.command = Some(Command::ChooseSoundFont);
        }
        if let Some((text, warn)) = input.status {
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .size(size::SMALL)
                        .color(if warn { pal.warning } else { pal.text_weak }),
                )
                .truncate(),
            );
        }
    });
    out
}
