//! The paste options every colour-art plugin shares (Image to MIDI,
//! Spiderweb): where the colours go (new Subtracks / Subchannels of one Track
//! with the signed Subchannel Paste Offset / chosen destinations), the MIDI
//! channel rule, the insertion tick, and the mapping preview shown before
//! Apply (contract §031–032, §040). The engine side is
//! `plumspirit_engine::transform::paste`.

use egui::{RichText, Ui};
use plumspirit_engine::model::{PlumDocument, SubchannelId, TrackId};
use plumspirit_engine::transform::TNote;
use plumspirit_engine::transform::paste::{
    BELOW_ZERO, BelowZero, CHANNEL_RULES, ChannelRule, Dest, ExistingDest, Generated, PASTE_MODES,
    PasteMode, PastePlan, PasteSpec,
};

/// The window's paste settings (kept per plugin while PlumSpirit runs).
#[derive(Clone, Debug, PartialEq)]
pub struct PasteOptions {
    pub mode: usize,
    pub offset: i64,
    pub below: usize,
    pub channel_rule: usize,
    /// 1–16 for "One channel for all".
    pub fixed_channel: u8,
    pub at: u64,
    pub existing: Vec<ExistingDest>,
}

impl Default for PasteOptions {
    fn default() -> Self {
        PasteOptions {
            mode: 1,
            offset: 0,
            below: 0,
            channel_rule: 0,
            fixed_channel: 1,
            at: 0,
            existing: Vec::new(),
        }
    }
}

impl PasteOptions {
    pub fn spec(&self, dest: TrackId, prefix: &str, skip_10: bool) -> PasteSpec {
        PasteSpec {
            mode: match self.mode {
                0 => PasteMode::Subtracks,
                2 => PasteMode::Existing,
                _ => PasteMode::Subchannels,
            },
            dest,
            at: self.at,
            sub_offset: self.offset,
            below_zero: match self.below {
                1 => BelowZero::Clamp,
                2 => BelowZero::Drop,
                _ => BelowZero::Refuse,
            },
            channel: match self.channel_rule {
                1 => ChannelRule::ByColor,
                2 => ChannelRule::TrackChannel,
                3 => ChannelRule::Fixed(self.fixed_channel.clamp(1, 16) - 1),
                _ => ChannelRule::Next { skip_10 },
            },
            existing: self.existing.clone(),
            prefix: prefix.to_owned(),
        }
    }
}

fn combo(ui: &mut Ui, id: &str, label: &str, value: &mut usize, options: &[&str]) -> bool {
    let before = *value;
    egui::ComboBox::from_id_salt(id)
        .selected_text(options[(*value).min(options.len() - 1)])
        .show_ui(ui, |ui| {
            for (i, o) in options.iter().enumerate() {
                ui.selectable_value(value, i, *o);
            }
        });
    ui.label(label);
    *value != before
}

/// The paste controls; `cursor` is the transport cursor (the "At the cursor"
/// button). Returns whether anything changed.
pub fn options_ui(
    ui: &mut Ui,
    id: &str,
    o: &mut PasteOptions,
    doc: &PlumDocument,
    dest_index: usize,
    art: Option<&Generated>,
    cursor: u64,
) -> bool {
    let before = o.clone();
    ui.horizontal(|ui| {
        combo(ui, &format!("{id}-mode"), "", &mut o.mode, PASTE_MODES);
    });
    let dest_label = doc
        .track(dest_index)
        .map(|_| doc.track_label(dest_index))
        .unwrap_or_default();
    ui.label(RichText::new(format!("Destination: {dest_label} (the active track)")).weak());
    ui.horizontal(|ui| {
        ui.label("Insert at tick");
        ui.add(crate::ui::stepper::Stepper::new(&mut o.at).speed(10.0));
        if ui.button("At the cursor").clicked() {
            o.at = cursor;
        }
    });
    match o.mode {
        1 => {
            ui.horizontal(|ui| {
                ui.label("Subchannel Paste Offset");
                ui.add(crate::ui::stepper::Stepper::new(&mut o.offset).range(-1_000_000..=1_000_000))
                    .on_hover_text("Color 0, 1, 2 … go to Subchannels 0 + offset, 1 + offset, … (logical Subchannels, not MIDI channels). Missing ones are created.");
            });
            if o.offset < 0 {
                ui.horizontal(|ui| {
                    combo(ui, &format!("{id}-below"), "", &mut o.below, BELOW_ZERO);
                });
            }
        }
        2 => {
            if let Some(a) = art {
                existing_ui(ui, id, o, doc, dest_index, a);
            }
        }
        _ => {}
    }
    if o.mode != 2 {
        ui.horizontal(|ui| {
            combo(
                ui,
                &format!("{id}-ch"),
                "MIDI channel of new ones",
                &mut o.channel_rule,
                CHANNEL_RULES,
            );
        });
        if o.channel_rule == 3 {
            ui.horizontal(|ui| {
                ui.label("Channel");
                ui.add(crate::ui::stepper::Stepper::new(&mut o.fixed_channel).range(1..=16));
            });
        }
    }
    *o != before
}

/// Existing mapping: a destination per colour.
fn existing_ui(
    ui: &mut Ui,
    id: &str,
    o: &mut PasteOptions,
    doc: &PlumDocument,
    dest_index: usize,
    art: &Generated,
) {
    let counts = art.counts();
    if o.existing.len() < art.colors.len() {
        o.existing.resize(art.colors.len(), ExistingDest::Skip);
    }
    let Some(dest) = doc.track(dest_index) else {
        return;
    };
    let subs: Vec<(SubchannelId, String)> = dest
        .subchannels()
        .entries()
        .iter()
        .map(|e| (e.id, e.display_name()))
        .collect();
    let label = |d: &ExistingDest| -> String {
        match d {
            ExistingDest::Skip => "Leave out".into(),
            ExistingDest::Track(t) => doc
                .index_of(*t)
                .map(|i| doc.track_label(i))
                .unwrap_or_else(|| "(missing track)".into()),
            ExistingDest::Subchannel(s) => subs
                .iter()
                .find(|x| x.0 == *s)
                .map(|x| format!("{} (this track)", x.1))
                .unwrap_or_else(|| "(missing subchannel)".into()),
        }
    };
    egui::ScrollArea::vertical()
        .id_salt(format!("{id}-existing"))
        .max_height(160.0)
        .show(ui, |ui| {
            for (c, info) in art.colors.iter().enumerate() {
                if counts.get(c).copied().unwrap_or(0) == 0 {
                    continue;
                }
                ui.horizontal(|ui| {
                    swatch(ui, info.rgb);
                    ui.label(format!("{} ({})", info.name, counts[c]));
                    let cur = o.existing[c];
                    egui::ComboBox::from_id_salt((id, "ex", c))
                        .selected_text(label(&cur))
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut o.existing[c],
                                ExistingDest::Skip,
                                "Leave out",
                            );
                            for (sid, name) in &subs {
                                ui.selectable_value(
                                    &mut o.existing[c],
                                    ExistingDest::Subchannel(*sid),
                                    format!("{name} (this track)"),
                                );
                            }
                            ui.separator();
                            for i in 0..doc.track_count() {
                                if let Some(t) = doc.track(i) {
                                    ui.selectable_value(
                                        &mut o.existing[c],
                                        ExistingDest::Track(t.id()),
                                        doc.track_label(i),
                                    );
                                }
                            }
                        });
                });
            }
        });
}

pub fn swatch(ui: &mut Ui, rgb: [u8; 3]) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(r, 0.0, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    ui.painter().rect_stroke(
        r,
        0.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Inside,
    );
}

/// The mapping preview: colour → destination, before Apply.
pub fn mapping_ui(ui: &mut Ui, id: &str, plan: &PastePlan, art: &Generated) {
    egui::CollapsingHeader::new(format!("Mapping ({} colors)", plan.colors.len()))
        .id_salt(format!("{id}-mapping"))
        .default_open(plan.colors.len() <= 8)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(format!("{id}-mapping-scroll"))
                .max_height(160.0)
                .show(ui, |ui| {
                    for p in &plan.colors {
                        let info = &art.colors[p.color as usize];
                        ui.horizontal(|ui| {
                            swatch(ui, info.rgb);
                            let to = match &p.dest {
                                Dest::NewSubtrack { name, channel, .. } => {
                                    format!("new Subtrack “{name}”, channel {}", channel + 1)
                                }
                                Dest::Subchannel { id, channel, new } => format!(
                                    "{} Subchannel {}, channel {}",
                                    if *new { "new" } else { "existing" },
                                    id.number(),
                                    channel + 1
                                ),
                                Dest::Track { channel, .. } => {
                                    format!("chosen track, channel {}", channel + 1)
                                }
                                Dest::Skipped(why) => format!("left out ({why})"),
                            };
                            ui.label(format!("{} · {} notes → {to}", info.name, p.notes));
                        });
                    }
                });
        });
}

/// The notes to draw on the destination track as the preview: absolute ticks,
/// the destination Subchannel's id (new Subtracks: the colour number, so each
/// colour is drawn apart).
pub fn preview_notes(art: &Generated, plan: &PastePlan, at: u64) -> Vec<TNote> {
    let mut map: Vec<Option<(u32, u8)>> = vec![None; art.colors.len()];
    for p in &plan.colors {
        map[p.color as usize] = match p.dest {
            Dest::Subchannel { id, channel, .. } => Some((id.0, channel)),
            Dest::NewSubtrack { channel, .. } => Some((p.color, channel)),
            Dest::Track { sub, channel, .. } => Some((sub.0, channel)),
            Dest::Skipped(_) => None,
        };
    }
    let mut v: Vec<TNote> = art
        .notes
        .iter()
        .filter_map(|n| {
            let (sub, ch) = map.get(n.color as usize).copied().flatten()?;
            Some(TNote {
                start: n.start.saturating_add(at),
                dur: n.dur.max(1),
                sub,
                pitch: n.pitch,
                vel: n.vel,
                ch,
                rel: 64,
                ..Default::default()
            })
        })
        .collect();
    v.sort_by_key(|n| (n.start, n.pitch));
    v
}
