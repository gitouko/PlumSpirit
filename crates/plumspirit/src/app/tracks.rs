//! Track Properties, Mute / Solo / Play All and the track end.
//!
//! The Track Properties dialog edits a draft; OK applies the whole draft as
//! **one** undo step (name, route, channel, type, corrections, key range,
//! Onion Skin colour, track end — and, only when asked, moving notes to the
//! channel). Cancel discards it. Invalid values change nothing.
//!
//! Mute and Solo are playback settings of the project: saved in the `.lup`,
//! outside the undo history (like Onion Skin), and applied to running playback
//! immediately.

use super::PlumSpiritApp;
use super::dialogs::Dialog;
use crate::audio::midi_out;
use crate::ui::status_bar::StatusKind;
use crate::ui::theme;
use egui::RichText;
use plumspirit_engine::editor::controllers;
use plumspirit_engine::history::{Operation, Outcome};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::correction::{self, Correction};
use plumspirit_engine::model::note::{parse_pitch_in, pitch_name};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::track::{GM_DRUM_CHANNEL, MAX_TICK_CORRECTION};
use plumspirit_engine::model::{GatePercent, OutputRoute, TrackId, TrackKind, TrackProps};
use plumspirit_engine::view::grid::MeterMap;

/// The Track Properties dialog's editable copy.
#[derive(Clone, Debug)]
pub struct TrackPropsDraft {
    pub track: TrackId,
    pub index: usize,
    pub name: String,
    /// 0 = internal synthesizer, `i` = `outputs[i - 1]`.
    pub route: usize,
    /// External outputs offered (the connected ones, plus the track's own when
    /// it is not connected).
    pub outputs: Vec<(String, bool)>,
    /// 1–16 as typed: the MIDI channel of the active subchannel (new notes).
    pub channel: u8,
    /// The active subchannel whose MIDI channel `channel` edits (v0.5).
    pub sub: Option<plumspirit_engine::model::Subchannel>,
    pub rhythm: bool,
    pub tick_correction: String,
    pub from_measure: String,
    pub key_correction: String,
    pub gate: String,
    pub key_lo: String,
    pub key_hi: String,
    /// The project's key range when the dialog opened (v0.9: typed keys and
    /// names reach 255 in 256-Key Mode).
    pub keys: KeyRange,
    pub auto_color: bool,
    pub color: [u8; 3],
    /// Track end: fit to content, or a custom end (`M:B:T`, `M:T`, `M` or ticks).
    pub custom_end: bool,
    pub end: String,
    /// Content end (ticks) and its musical position, for the hint.
    pub content_end: u64,
    /// Move the notes of this track to the channel (explicit remap).
    pub move_notes: bool,
    pub notes: usize,
    pub error: Option<String>,
    pub focused: bool,
    /// Preview of how the notes will play with the corrections (v0.5).
    pub preview: bool,
    pub preview_text: String,
}

/// The correction fields of a Track Properties draft, validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraftCorrections {
    pub ticks: i64,
    pub from_measure: u32,
    pub keys: i16,
    pub gate: GatePercent,
    pub range: (u8, u8),
}

impl DraftCorrections {
    /// `props` with these corrections.
    pub fn applied_to(&self, props: &TrackProps) -> TrackProps {
        TrackProps {
            tick_correction: self.ticks,
            correction_from_measure: self.from_measure,
            key_correction: self.keys,
            gate: self.gate,
            key_range: self.range,
            ..props.clone()
        }
    }
}

/// Parses and checks the correction fields (shared by OK and the Preview).
pub fn parse_corrections(
    d: &TrackPropsDraft,
    meter: &MeterMap,
) -> Result<DraftCorrections, String> {
    let ticks: i64 = d
        .tick_correction
        .trim()
        .replace([',', '_', '+'], "")
        .replace('\u{2212}', "-")
        .parse()
        .map_err(|_| {
            "The tick correction must be a whole number of ticks, e.g. 120 or -60.".to_owned()
        })?;
    if ticks.unsigned_abs() > MAX_TICK_CORRECTION.unsigned_abs() {
        return Err("The tick correction is too large.".into());
    }
    let from_measure: u32 = d
        .from_measure
        .trim()
        .parse()
        .ok()
        .filter(|&m| m >= 1)
        .ok_or("“After measure” must be a measure number from 1.")?;
    if from_measure > 1 && !meter.is_musical() {
        return Err(
            "This project has no measures (SMPTE timing); leave “After measure” at 1.".into(),
        );
    }
    let key: i64 = d
        .key_correction
        .trim()
        .replace('+', "")
        .replace('\u{2212}', "-")
        .parse()
        .map_err(|_| {
            "The key correction must be a whole number of semitones, e.g. 12 or -12.".to_owned()
        })?;
    // The project's key range decides the exact limit (TrackProps::validate).
    if !(-255..=255).contains(&key) {
        return Err("The key correction must be between −255 and +255 semitones.".into());
    }
    let gate = GatePercent::parse(&d.gate)?;
    let lo = parse_pitch_in(&d.key_lo, d.keys)?;
    let hi = parse_pitch_in(&d.key_hi, d.keys)?;
    if lo > hi {
        return Err(format!(
            "The key range must go from a lower to a higher note ({} is above {}).",
            pitch_name(lo),
            pitch_name(hi)
        ));
    }
    Ok(DraftCorrections {
        ticks,
        from_measure,
        keys: key as i16,
        gate,
        range: (lo, hi),
    })
}

/// Parses a track end typed as ticks ("7,680") or a musical position.
fn parse_end(text: &str, meter: &MeterMap) -> Result<u64, String> {
    let t = text.trim();
    let digits = t.replace([',', '_'], "");
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return digits
            .parse::<u64>()
            .map_err(|_| "The track end is beyond the largest possible tick.".to_owned());
    }
    if meter.is_musical() {
        meter.parse_position(t)
    } else {
        Err(format!("“{t}” is not a tick position."))
    }
}

impl PlumSpiritApp {
    /// Opens Track Properties for track `index`.
    pub(super) fn open_track_properties(&mut self, index: usize) {
        let Some(t) = self.doc().and_then(|d| d.track(index)) else {
            return;
        };
        let p = t.props().clone();
        let mut outputs: Vec<(String, bool)> = midi_out::available_outputs()
            .into_iter()
            .map(|n| (n, true))
            .collect();
        let route = match &p.route {
            OutputRoute::Internal => 0,
            OutputRoute::External(name) => match outputs.iter().position(|(n, _)| n == name) {
                Some(i) => i + 1,
                None => {
                    outputs.push((name.clone(), false));
                    outputs.len()
                }
            },
        };
        let content_end = t.content_only_end_tick();
        let end = t.end_of_track_tick;
        let sub = self.active_sub_of(index);
        let draft = TrackPropsDraft {
            sub: sub.clone(),
            track: t.id(),
            index,
            name: t.name().unwrap_or_default().to_owned(),
            route,
            outputs,
            channel: sub.as_ref().map_or(p.default_channel, |s| s.midi_channel) + 1,
            rhythm: p.kind == TrackKind::Rhythm,
            tick_correction: p.tick_correction.to_string(),
            from_measure: p.correction_from_measure.to_string(),
            key_correction: p.key_correction.to_string(),
            gate: p.gate.to_string(),
            key_lo: pitch_name(p.key_range.0),
            key_hi: pitch_name(p.key_range.1),
            keys: self.doc().map(|d| d.project().keys).unwrap_or_default(),
            auto_color: p.onion_color.is_none(),
            color: p.onion_color.unwrap_or_else(|| {
                let palette = theme::TrackPalette::new(
                    self.settings.track_color_mode,
                    self.doc()
                        .into_iter()
                        .flat_map(|d| d.tracks())
                        .map(|t| t.id().0),
                )
                .with_manual(&self.settings.track_color_manual);
                let c = palette.color(t.id().0, None, true, 255);
                [c.r(), c.g(), c.b()]
            }),
            custom_end: end.is_some_and(|e| e > content_end),
            end: end.unwrap_or(content_end).to_string(),
            content_end,
            move_notes: false,
            notes: t.notes.len(),
            error: None,
            focused: false,
            preview: self.settings.preview,
            preview_text: String::new(),
        };
        let meter = self.meter_map().cloned();
        self.dialog = Some(Dialog::TrackProperties(Box::new(draft), meter));
    }

    /// OK in Track Properties: validates everything, then applies it as one undo
    /// step.
    pub(super) fn apply_track_properties(&mut self, d: &TrackPropsDraft) -> Result<(), String> {
        let meter = self.meter_map().cloned().ok_or("No project is open.")?;
        let doc = self
            .open
            .as_ref()
            .map(|o| o.doc.clone())
            .ok_or("No project is open.")?;
        let index = doc.index_of(d.track).ok_or("The track no longer exists.")?;
        let t = doc.track(index).ok_or("The track no longer exists.")?;
        let name = controllers::parse_track_name(&d.name).map_err(|e| e.user_message())?;
        let corr = parse_corrections(d, &meter)?;
        if !(1..=16).contains(&d.channel) {
            return Err("The channel must be 1 to 16.".into());
        }
        let route = match d.route {
            0 => OutputRoute::Internal,
            i => OutputRoute::External(
                d.outputs
                    .get(i - 1)
                    .map(|o| o.0.clone())
                    .ok_or("Choose an output.")?,
            ),
        };
        let props = TrackProps {
            default_channel: d.channel - 1,
            kind: if d.rhythm {
                TrackKind::Rhythm
            } else {
                TrackKind::Melody
            },
            route,
            tick_correction: corr.ticks,
            correction_from_measure: corr.from_measure,
            key_correction: corr.keys,
            gate: corr.gate,
            key_range: corr.range,
            onion_color: (!d.auto_color).then_some(d.color),
        };
        props.validate(doc.project().keys)?;
        let content = t.content_only_end_tick();
        let end = if d.custom_end {
            let e = parse_end(&d.end, &meter)?;
            if e < content {
                return Err(format!(
                    "The track end ({}) is before the track's last note end or event ({}). Content is never cut off by the track end; choose a later end or Fit to Content.",
                    meter.format_position(e),
                    meter.format_position(content)
                ));
            }
            Some(e)
        } else {
            None
        };
        let mut ops: Vec<Operation> = Vec::new();
        if props != *t.props() || end != t.end_of_track_tick {
            ops.push(Operation::TrackProps {
                track: d.track,
                props: props.clone(),
                end,
                label: "Track Properties",
            });
        }
        if t.name().unwrap_or_default() != name {
            let edit = controllers::prepare_rename(t, &name).map_err(|e| e.user_message())?;
            ops.push(Operation::Events {
                edit,
                label: "Rename Track",
            });
        }
        // The channel for new notes is the active subchannel's MIDI channel.
        if let Some(sc) = &d.sub
            && sc.midi_channel != d.channel - 1
            && t.subchannels().contains(sc.id)
        {
            let mut new_sc = sc.clone();
            new_sc.midi_channel = d.channel - 1;
            ops.push(Operation::EditSubchannel {
                track: index,
                sub: new_sc,
                move_notes: false,
            });
        }
        let move_notes = d.move_notes && !t.notes.is_empty();
        drop(doc);
        if move_notes {
            // Explicit remap: every note of the track to the channel.
            if let Some(tr) = self.doc_mut().and_then(|doc| doc.track_mut(index)) {
                tr.notes.select_all();
            }
            ops.push(Operation::SetChannel {
                track: index.into(),
                channel: d.channel - 1,
            });
        }
        if ops.is_empty() {
            return Ok(());
        }
        let out = self.apply_op(Operation::Group {
            ops,
            label: "Track Properties",
        });
        if out.is_some_and(|o| o != Outcome::Unchanged) {
            self.meter = None;
            // Preflight the corrections so problems are reported right away.
            let report = self.doc().and_then(|doc| {
                let t = doc.track(index)?;
                let c = Correction::resolve(t.props(), &meter, doc.project().keys)?;
                Some(correction::check(t, index, &c))
            });
            let label = self
                .doc()
                .map(|doc| doc.track_label(index))
                .unwrap_or_default();
            match report.map(|r| r.notes(&label)).filter(|l| !l.is_empty()) {
                Some(lines) => self.set_status_details(
                    "Track Properties applied — some notes are affected by the corrections.",
                    StatusKind::Warning,
                    lines,
                ),
                None => self.set_status("Track Properties applied.", StatusKind::Info),
            }
        }
        Ok(())
    }

    /// Fit End to Content: the track's End of Track goes right after its last
    /// note end or event (nothing is deleted).
    pub(super) fn fit_track_end(&mut self, index: usize) {
        let Some(t) = self.doc().and_then(|d| d.track(index)) else {
            return;
        };
        if t.end_of_track_tick.is_none() {
            self.set_status("The track end already fits its content.", StatusKind::Info);
            return;
        }
        let (id, props) = (t.id(), t.props().clone());
        let before = t.content_end_tick();
        let after = t.content_only_end_tick();
        if self
            .apply_op(Operation::TrackProps {
                track: id,
                props,
                end: None,
                label: "Fit End to Content",
            })
            .is_some()
        {
            self.set_status(
                format!(
                    "Track end fitted to its content: tick {} → {}.",
                    group_digits(before),
                    group_digits(after)
                ),
                StatusKind::Info,
            );
        }
    }

    pub(super) fn toggle_mute(&mut self, index: usize) {
        if let Some(t) = self.doc_mut().and_then(|d| d.track_mut(index)) {
            t.mute = !t.mute;
        }
    }

    /// v0.8 §204: Mute (`on`) or unmute every track of the selection that
    /// `clicked` belongs to (or `clicked` alone). Project setting, no undo,
    /// like the M buttons.
    pub(super) fn mute_selection(&mut self, clicked: usize, on: bool) {
        let tracks = self.action_tracks(clicked);
        let Some(d) = self.doc_mut() else {
            return;
        };
        for i in tracks {
            if let Some(t) = d.track_mut(i) {
                t.mute = on;
            }
        }
    }

    /// v0.8 §204: Solo exactly the selection that `clicked` belongs to.
    pub(super) fn solo_selection(&mut self, clicked: usize) {
        let tracks = self.action_tracks(clicked);
        let Some(d) = self.doc_mut() else {
            return;
        };
        for i in 0..d.track_count() {
            let on = tracks.contains(&i);
            if let Some(t) = d.track_mut(i) {
                t.solo = on;
            }
        }
    }

    /// Solo `index`; with Ctrl (add), other soloed tracks stay soloed.
    pub(super) fn toggle_solo(&mut self, index: usize, add: bool) {
        let Some(d) = self.doc_mut() else {
            return;
        };
        let now = !d.track(index).is_some_and(|t| t.solo);
        if !add && now {
            for i in 0..d.track_count() {
                if let Some(t) = d.track_mut(i) {
                    t.solo = false;
                }
            }
        }
        if let Some(t) = d.track_mut(index) {
            t.solo = now;
        }
    }

    /// Play All: clears every Mute and Solo (and a monitor set).
    pub(super) fn play_all(&mut self) {
        let Some(d) = self.doc_mut() else {
            return;
        };
        d.project_mut().monitor = None;
        for i in 0..d.track_count() {
            if let Some(t) = d.track_mut(i) {
                t.mute = false;
                t.solo = false;
            }
        }
        self.set_status("Play All: every track plays.", StatusKind::Info);
    }

    /// L-F08 (contract §016): Play Only Selected Tracks — exactly the
    /// selected tracks (or the current one) play, as a transient monitor set:
    /// no Mute / Solo flag changes, nothing is saved or dirtied, outside
    /// undo; Restore All Tracks ends it. Applies during playback at once
    /// (the transport follows `playback::eligibility`).
    pub(super) fn play_only_selected(&mut self) {
        // L-S33 (§023 "selection across Clips"): clips selected on several
        // tracks in the Track List play their tracks.
        let clip_tracks: Vec<usize> = match self.doc() {
            Some(d)
                if self.main_view == super::track_list::MainView::TrackList
                    && self.selected_tracks.len() <= 1 =>
            {
                let mut v: Vec<usize> = self
                    .track_list
                    .selected
                    .iter()
                    .filter_map(|&c| d.clips().placement(c).map(|p| p.track))
                    .filter_map(|t| d.tracks().iter().position(|x| x.id() == t))
                    .collect();
                v.sort_unstable();
                v.dedup();
                v
            }
            _ => Vec::new(),
        };
        let tracks = if clip_tracks.len() > 1 {
            clip_tracks
        } else if self.conductor_selected() && self.selected_tracks.is_empty() {
            Vec::new()
        } else {
            self.action_tracks(self.active_track())
        };
        let Some(d) = self.doc_mut() else {
            return;
        };
        let mut ids: Vec<plumspirit_engine::model::TrackId> = tracks
            .iter()
            .filter_map(|&i| d.track(i).map(|t| t.id()))
            .collect();
        if ids.is_empty() {
            self.set_status(
                "Select the tracks to play first (click a track, Ctrl+click for more).",
                StatusKind::Info,
            );
            return;
        }
        ids.sort_unstable();
        ids.dedup();
        let n = ids.len();
        d.project_mut().monitor = Some(ids);
        self.set_status(
            format!(
                "Playing only the {} selected track{} (the song's Mute / Solo are kept). Restore All Tracks{} plays everything again.",
                group_digits(n as u64),
                if n == 1 { "" } else { "s" },
                super::keybinds::hint(super::commands::Command::RestoreAllTracks)
            ),
            StatusKind::Info,
        );
    }

    /// L-F08: ends Play Only Selected Tracks — back to the song's own Mute
    /// / Solo, untouched.
    pub(super) fn restore_all_tracks(&mut self) {
        let Some(d) = self.doc_mut() else {
            return;
        };
        if d.project_mut().monitor.take().is_some() {
            self.set_status(
                "All tracks: the song's own Mute / Solo apply again.",
                StatusKind::Info,
            );
        }
    }
}

/// The Track Properties dialog body. Returns true when Enter was pressed in a
/// field.
pub fn track_properties_ui(
    ui: &mut egui::Ui,
    d: &mut TrackPropsDraft,
    meter: Option<&MeterMap>,
) -> bool {
    let mut enter = false;
    let small = |s: &str| RichText::new(s).size(theme::size::SMALL);
    let field = |ui: &mut egui::Ui, text: &mut String, hint: &str, w: f32, focus: bool| -> bool {
        let r = ui.add(
            egui::TextEdit::singleline(text)
                .desired_width(w)
                .hint_text(hint),
        );
        if focus {
            r.request_focus();
        }
        r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    };
    egui::Grid::new("track-props")
        .num_columns(2)
        .max_col_width((ui.available_width() - 140.0).max(160.0))
        .spacing([12.0, 7.0])
        .show(ui, |ui| {
            ui.label("Name");
            enter |= field(ui, &mut d.name, &format!("Track {}", d.index + 1), 260.0, !d.focused);
            ui.end_row();

            ui.label("Output");
            let current = match d.route {
                0 => "Internal SoundFont Synthesizer".to_owned(),
                i => d
                    .outputs
                    .get(i - 1)
                    .map(|(n, ok)| {
                        if *ok {
                            n.clone()
                        } else {
                            format!("{n} (not connected)")
                        }
                    })
                    .unwrap_or_default(),
            };
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("track-output")
                    .width(260.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.route, 0, "Internal SoundFont Synthesizer");
                        for (i, (n, ok)) in d.outputs.iter().enumerate() {
                            let label = if *ok {
                                n.clone()
                            } else {
                                format!("{n} (not connected)")
                            };
                            ui.selectable_value(&mut d.route, i + 1, label);
                        }
                    });
                let note = if !midi_out::EXTERNAL_SUPPORTED {
                    "External MIDI outputs are available in the Windows version."
                } else if d.outputs.iter().all(|o| !o.1) {
                    "No external MIDI outputs are connected."
                } else {
                    "An external output plays this track instead of the internal synthesizer (never both)."
                };
                ui.label(small(note).weak());
            });
            ui.end_row();

            ui.label("Channel");
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("track-channel")
                    .width(56.0)
                    .selected_text(d.channel.to_string())
                    .show_ui(ui, |ui| {
                        for c in 1..=16u8 {
                            ui.selectable_value(&mut d.channel, c, c.to_string());
                        }
                    });
                let for_what = match &d.sub {
                    Some(sc) => format!("for new notes in {}", sc.display_name()),
                    None => "for new notes and events".to_owned(),
                };
                ui.label(small(&for_what).weak()).on_hover_text("The real MIDI channel (1–16). Each subchannel has its own; this edits the active subchannel's.");
            });
            ui.end_row();

            ui.label("");
            ui.add_enabled(
                d.notes > 0,
                egui::Checkbox::new(
                    &mut d.move_notes,
                    format!(
                        "Also move this track's {} note(s) to channel {}",
                        group_digits(d.notes as u64),
                        d.channel
                    ),
                ),
            )
            .on_hover_text("Rewrites the channel of every note of the track (undoable). Without this, existing notes keep their channels.");
            ui.end_row();

            ui.label("Type");
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut d.rhythm, false, "Melody");
                if ui
                    .radio_value(&mut d.rhythm, true, "Rhythm (GM drums)")
                    .on_hover_text("General MIDI drums use channel 10. Choosing Rhythm sets the channel for new notes to 10; existing notes are unchanged.")
                    .clicked()
                {
                    d.channel = GM_DRUM_CHANNEL + 1;
                }
            });
            ui.end_row();

            ui.add_sized([80.0, 1.0], egui::Separator::default().horizontal());
            ui.label(small("Corrections (applied when playing and, by default, exporting; the notes stay unchanged)").weak());
            ui.end_row();

            ui.label("Tick correction");
            ui.horizontal_wrapped(|ui| {
                enter |= field(ui, &mut d.tick_correction, "0", 90.0, false);
                ui.label(small("ticks (+ later, − earlier)").weak());
            });
            ui.end_row();

            ui.label("After measure");
            ui.horizontal_wrapped(|ui| {
                enter |= field(ui, &mut d.from_measure, "1", 60.0, false);
                let hint = match (meter, d.from_measure.trim().parse::<u64>()) {
                    (Some(m), Ok(n)) if n >= 1 => m
                        .measure_start(n)
                        .map(|t| format!("from tick {}", group_digits(t)))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                ui.label(small(&format!("corrections start at this measure {hint}")).weak());
            });
            ui.end_row();

            ui.label("Key correction");
            ui.horizontal_wrapped(|ui| {
                enter |= field(ui, &mut d.key_correction, "0", 60.0, false);
                ui.label(small("semitones (12 = one octave up)").weak());
            });
            ui.end_row();

            ui.label("Gate correction");
            ui.horizontal_wrapped(|ui| {
                enter |= field(ui, &mut d.gate, "100", 60.0, false);
                ui.label(small("% of each note's length (100 = unchanged)").weak());
            });
            ui.end_row();

            ui.label("Key range");
            ui.horizontal_wrapped(|ui| {
                enter |= field(ui, &mut d.key_lo, "C-1", 52.0, false);
                ui.label("to");
                enter |= field(ui, &mut d.key_hi, "G9", 52.0, false);
                ui.label(small("notes outside do not play").weak());
            });
            ui.end_row();

            ui.add_sized([80.0, 1.0], egui::Separator::default().horizontal());
            ui.label("");
            ui.end_row();

            ui.label("Track color");
            ui.horizontal_wrapped(|ui| {
                if d.auto_color {
                    // The automatic colour as this theme shows it (the one
                    // resolver: track list, ghosts, keyboard).
                    let c=theme::note_palette(ui.ctx()).track_color(d.track.0,None,ui.ctx().theme()==egui::Theme::Dark,255); d.color=[c.r(),c.g(),c.b()];
                }
                ui.checkbox(&mut d.auto_color, "Automatic");
                ui.add_enabled_ui(!d.auto_color, |ui| {
                    egui::color_picker::color_edit_button_srgb(ui, &mut d.color);
                });
            });
            ui.end_row();

            ui.label("Track end");
            ui.vertical(|ui| {
                ui.radio_value(&mut d.custom_end, false, "Fit to content");
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut d.custom_end, true, "Custom end:");
                    ui.add_enabled_ui(d.custom_end, |ui| {
                        enter |= field(ui, &mut d.end, "M:B:T or ticks", 120.0, false);
                    });
                });
                let content = meter.map_or_else(
                    || group_digits(d.content_end),
                    |m| format!("{} (tick {})", m.format_position(d.content_end), group_digits(d.content_end)),
                );
                ui.label(small(&format!("Content ends at {content}; a custom end adds silence after it.")).weak());
            });
            ui.end_row();
        });
    d.focused = true;
    enter
}

#[cfg(test)]
mod tests {
    use super::parse_end;
    use plumspirit_engine::model::Division;
    use plumspirit_engine::view::grid::MeterMap;

    #[test]
    fn track_ends_parse_as_ticks_or_positions() {
        let m = MeterMap::new(Division::ppqn(480), &[(0, 4, 2)]);
        assert_eq!(parse_end("7,680", &m), Ok(7680));
        assert_eq!(parse_end("5", &m), Ok(5));
        assert_eq!(parse_end("5:0", &m), Ok(4 * 1920));
        assert_eq!(parse_end("2:2:0", &m), Ok(1920 + 480));
        assert!(parse_end("x", &m).is_err());
    }
}

/// v1.0-Launch L-F08 … L-F10 (contract §016).
#[cfg(test)]
mod monitor_tests {
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use crate::app::progressor::ScopeSetting;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use plumspirit_engine::playback::eligibility;

    const SIZE: egui::Vec2 = egui::vec2(1366.0, 768.0);

    /// Five tracks; track `i` has `i + 1` notes.
    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("m".into(), Bpm::DEFAULT, 960);
        for _ in 1..5 {
            d.add_track().unwrap();
        }
        for i in 0..5 {
            let v: Vec<NoteRecord> = (0..=i as u64)
                .map(|k| {
                    NoteRecord::new(
                        k * 480,
                        240,
                        NoteAttrs::new(60 + i as u8, 90, 0, 64, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        d
    }

    /// Flopp: "if i want to select only two to be unmuted i have to manually
    /// mute everything else". Two selected tracks are heard alone with one
    /// command and the whole song again with another, without a single Mute
    /// / Solo flag changing; readouts follow; nothing is saved or dirtied.
    #[test]
    fn play_only_selected_hears_exactly_the_selection_and_restore_all_brings_the_song_back() {
        let mut h = Harness::new(doc());
        h.app_frame(SIZE, 1.0);
        h.app.doc_mut().unwrap().track_mut(2).unwrap().mute = true;
        h.app.doc_mut().unwrap().track_mut(4).unwrap().solo = true;
        let flags = |h: &Harness| -> Vec<(bool, bool)> {
            h.doc().tracks().iter().map(|t| (t.mute, t.solo)).collect()
        };
        let song = flags(&h);
        let dirty = h.app.open.as_ref().unwrap().dirty();
        assert_eq!(eligibility(h.doc()), vec![false, false, false, false, true]);
        assert!(
            !h.app.enabled(Command::RestoreAllTracks),
            "nothing to restore"
        );
        // Select tracks 1 and 3: Play Only Selected Tracks.
        h.set_tracks(&[1, 3]);
        assert!(h.command(Command::PlayOnlySelected));
        assert_eq!(eligibility(h.doc()), vec![false, true, false, true, false]);
        assert_eq!(flags(&h), song, "no Mute / Solo flag changed");
        assert_eq!(h.app.open.as_ref().unwrap().dirty(), dirty, "not dirtied");
        // The readouts' Audible scope (Progressor, keyboard colors) follow.
        let audible = crate::app::keys::scope_tracks(h.doc(), ScopeSetting::Audible, 0).unwrap();
        let want: std::collections::BTreeSet<_> = [1, 3].iter().map(|&i| h.id(i)).collect();
        assert_eq!(audible, want);
        // The track panel offers Restore All while monitoring.
        let shapes = h.app_frame(SIZE, 1.0);
        let texts = crate::ui::testkit::texts(&shapes);
        assert!(texts.iter().any(|t| t.0 == "Restore All"), "{texts:?}");
        // Selecting other tracks does not change what is heard.
        h.set_track(0);
        assert_eq!(eligibility(h.doc()), vec![false, true, false, true, false]);
        // Never saved: a .lup has the song's flags and no monitor.
        let bytes = plumspirit_engine::native::lup::project_to_bytes(h.doc()).unwrap();
        let (back, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
        assert_eq!(back.project().monitor, None);
        assert_eq!(
            back.tracks()
                .iter()
                .map(|t| (t.mute, t.solo))
                .collect::<Vec<_>>(),
            song
        );
        // Restore All Tracks: the song's own Mute / Solo again.
        assert!(h.command(Command::RestoreAllTracks));
        assert_eq!(eligibility(h.doc()), vec![false, false, false, false, true]);
        assert_eq!(flags(&h), song);
        // The current track's Mute / Solo (L-F10).
        h.set_track(4);
        assert!(h.command(Command::MuteCurrentTrack));
        assert!(h.doc().track(4).unwrap().mute);
        assert_eq!(
            eligibility(h.doc()),
            vec![true, true, false, true, false],
            "Mute wins over Solo: the muted solo restricts nothing"
        );
        assert!(h.command(Command::MuteCurrentTrack));
        h.set_track(0);
        assert!(h.command(Command::SoloCurrentTrack));
        assert!(h.doc().track(0).unwrap().solo && !h.doc().track(4).unwrap().solo);
        // Play All clears a monitor set too.
        h.set_tracks(&[1, 3]);
        assert!(h.command(Command::PlayOnlySelected));
        assert!(h.command(Command::PlayAll));
        assert_eq!(h.doc().project().monitor, None);
        assert_eq!(eligibility(h.doc()), vec![true; 5]);
    }
}
