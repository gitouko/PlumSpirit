//! Playback → Audio Settings… (v0.8 contract §133 – §145): one PlumSpirit
//! window with the sections Output, Render Mode, Voices, Headroom, SoundFonts
//! and Diagnostics. Only settings the engine really has are shown; every
//! value is bounded. Changes apply at once (voices / effects rebuild the
//! synthesizers between blocks; device, sample rate, buffer and a larger
//! Pre-Render buffer reopen the device stream while stopped, else at the next
//! Play).

use super::PlumSpiritApp;
use crate::audio::midi_out::{self, OutputInfo, OutputTarget};
use crate::audio::output::{self, DeviceInfo};
use crate::audio::synth;
use crate::ui::status_bar::StatusKind;
use egui::RichText;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

#[derive(Default)]
pub struct AudioWindow {
    pub open: bool,
    devices: Option<Vec<DeviceInfo>>,
    listing: Option<Receiver<Vec<DeviceInfo>>>,
    /// Peak meters (decaying display values, dBFS).
    meter_pre: f32,
    meter_post: f32,
    /// B1-0135: the MIDI outputs for the default-output chooser, listed at
    /// start-up and on Refresh on a thread (the KDMAPI probe loads a DLL).
    pub outputs: Vec<OutputInfo>,
    outputs_rx: Option<Receiver<Vec<OutputInfo>>>,
    outputs_asked: bool,
    /// v1.0-beta2 (B2-F12): the Event Processing values while an option is
    /// off (kept for when it is switched on again) and the preflight, cached
    /// by (document analysis revision, band).
    events: EventEdit,
}

/// The Event Processing panel's own values (B2-F12).
struct EventEdit {
    band_on: bool,
    band: (u8, u8),
    length_s: f64,
    delay_s: f64,
    preflight: Option<(PreflightKey, plumspirit_engine::playback::Preflight)>,
}

/// (document analysis revision, velocity band) a preflight was counted for.
type PreflightKey = (u64, (u8, u8));

impl Default for EventEdit {
    fn default() -> Self {
        EventEdit {
            band_on: false,
            band: (1, 10),
            length_s: 0.1,
            delay_s: 0.05,
            preflight: None,
        }
    }
}

/// Seconds as whole nanoseconds (at least `min`; huge values saturate).
fn seconds_ns(s: f64, min: u64) -> u64 {
    let ns = (s * 1e9).round();
    if ns >= u64::MAX as f64 {
        u64::MAX
    } else {
        (ns.max(0.0) as u64).max(min)
    }
}

/// B2-F12: the Event Processing section of Audio Settings — the stages in
/// their order, each opt-in and independent, live (the change reaches the
/// scheduler at once) and saved; `pre` is the song's preflight.
fn event_processing_ui(
    ui: &mut egui::Ui,
    p: &mut plumspirit_engine::playback::EventProcessing,
    e: &mut EventEdit,
    pre: Option<plumspirit_engine::playback::Preflight>,
) {
    use crate::ui::stepper::Stepper;
    use plumspirit_engine::midi::group_digits;
    use plumspirit_engine::playback::{EventProcessing, ResetPolicy};
    ui.label(RichText::new("Event processing (playback only)").strong());
    ui.label(
        RichText::new(
            "Not destructive: these change only what is sent to the synthesizer and the MIDI outputs while playing — never the project, the saved .lup or the exported .mid, the MIDI Player or the Inspector's counts. They apply in this order:",
        )
        .small()
        .weak(),
    );
    ui.checkbox(&mut p.keys_88, "1. Only the 88 piano keys (21 – 108; the drum channel keeps every key)")
        .on_hover_text("Notes below A0 (21) or above C8 (108) are not sent, and neither are their Note Offs, so nothing sticks. MIDI channel 10 (drums) is exempt. 256-Key projects keep every key in the project.");
    // 2. Ignored velocities.
    if let Some(b) = p.velocity_band {
        e.band_on = true;
        e.band = b;
    }
    let mut problem = None;
    ui.horizontal(|ui| {
        ui.checkbox(&mut e.band_on, "2. Ignore notes with velocity from")
            .on_hover_text("Notes whose velocity is in this range, both ends included, are not sent, and neither are their Note Offs (they cannot end another note). Example: 1 to 10 leaves out the quietest ghost notes. A Note On of velocity 0 is a Note Off.");
        let (mut lo, mut hi) = e.band;
        ui.add(Stepper::new(&mut lo).range(0..=127u8).what("the lowest ignored velocity"));
        ui.label("to");
        ui.add(Stepper::new(&mut hi).range(0..=127u8).what("the highest ignored velocity"));
        e.band = (lo, hi);
        problem = EventProcessing::band_problem(lo, hi);
        if let Some(m) = &problem {
            ui.label(RichText::new("!").color(ui.visuals().error_fg_color)).on_hover_text(m);
        }
    });
    if let Some(m) = &problem {
        ui.label(RichText::new(m).small().color(ui.visuals().error_fg_color));
    }
    p.velocity_band = (e.band_on && problem.is_none()).then_some(e.band);
    // 3. Length override.
    ui.horizontal(|ui| {
        let mut on = p.length_override_ns.is_some();
        if let Some(ns) = p.length_override_ns {
            e.length_s = ns as f64 / 1e9;
        }
        ui.checkbox(&mut on, "3. Every note lasts")
            .on_hover_text("Every sent note plays this long from its start, whatever its own length (through the tempo map). The notes in the project keep their lengths.");
        ui.add(
            Stepper::new(&mut e.length_s)
                .range(f64::MIN_POSITIVE..=1.8e10)
                .range_sentence("The length must be a positive number of seconds.")
                .speed(0.01)
                .suffix(" s"),
        );
        p.length_override_ns = on.then(|| seconds_ns(e.length_s, 1));
    });
    // 4. Note Off delay.
    ui.horizontal(|ui| {
        let mut on = p.off_delay_ns.is_some();
        if let Some(ns) = p.off_delay_ns {
            e.delay_s = ns as f64 / 1e9;
        }
        ui.checkbox(&mut on, "4. Delay every Note Off by")
            .on_hover_text(
                "Each Note Off is sent this much later (after the length above when both are on).",
            );
        ui.add(
            Stepper::new(&mut e.delay_s)
                .range(0.0..=1.8e10)
                .range_sentence("The delay must be 0 or a positive number of seconds.")
                .speed(0.01)
                .suffix(" s"),
        );
        p.off_delay_ns = on.then(|| seconds_ns(e.delay_s, 0));
    });
    if p.times_notes() {
        ui.label(RichText::new("While 3 or 4 is on, a key plays one note at a time: a note struck again on a key that is still held ends the held one first.").small().weak());
    }
    // System resets.
    ui.label("The song's system resets (GM / GS / XG):");
    ui.indent("resets", |ui| {
        ui.radio_value(&mut p.resets, ResetPolicy::Skip, "Not sent (as before)");
        ui.radio_value(&mut p.resets, ResetPolicy::Send, "Sent");
        ui.radio_value(&mut p.resets, ResetPolicy::SendChanged, "Sent, ignoring a repeat of the current mode")
            .on_hover_text("The first reset and every change of mode are sent; a reset to the mode already set is not.");
    });
    if p.resets == ResetPolicy::SendChanged {
        ui.label(RichText::new("Ignoring a repeated reset keeps the programs and controllers set before it; a song that counts on the reset to clear them may sound different.").small().color(ui.visuals().warn_fg_color));
    }
    if let Some(q) = pre {
        ui.label(
            RichText::new(format!(
                "This song: {} notes · {} outside the 88 keys · {} with velocities {} – {} · {} system resets ({} repeats)",
                group_digits(q.notes),
                group_digits(q.outside_keys),
                group_digits(q.in_band),
                e.band.0,
                e.band.1,
                group_digits(q.resets),
                group_digits(q.repeated_resets),
            ))
            .small(),
        );
    }
}

impl AudioWindow {
    /// Lists the devices on a thread (enumeration can take a moment).
    fn refresh(&mut self) {
        let (tx, rx) = channel();
        let ok = std::thread::Builder::new()
            .name("plumspirit-audio-devices".into())
            .spawn(move || {
                let _ = tx.send(output::list_devices());
            })
            .is_ok();
        if ok {
            self.listing = Some(rx);
        }
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.listing
            && let Ok(list) = rx.try_recv()
        {
            self.devices = Some(list);
            self.listing = None;
        }
    }

    /// Looks for MIDI devices and OmniMIDI on a thread; the UI repaints when
    /// the list arrives.
    pub fn refresh_outputs(&mut self, ctx: &egui::Context) {
        if self.outputs_rx.is_some() {
            return;
        }
        self.outputs_asked = true;
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let ok = std::thread::Builder::new()
            .name("plumspirit-midi-outputs".into())
            .spawn(move || {
                let _ = tx.send(midi_out::list_outputs());
                ctx.request_repaint();
            })
            .is_ok();
        if ok {
            self.outputs_rx = Some(rx);
        }
    }

    /// Takes a finished listing; starts the first one (start-up).
    pub fn poll_outputs(&mut self, ctx: &egui::Context) {
        if !self.outputs_asked {
            self.refresh_outputs(ctx);
        }
        if let Some(rx) = &self.outputs_rx {
            match rx.try_recv() {
                Ok(list) => {
                    self.outputs = list;
                    self.outputs_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.outputs_rx = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub fn outputs_listing(&self) -> bool {
        self.outputs_rx.is_some()
    }

    /// The chooser's entries: the listed outputs (Internal alone until the
    /// first listing arrives) with `current` kept when it went away.
    pub fn output_choices(&self, current: &OutputTarget) -> Vec<OutputInfo> {
        let base = if self.outputs.is_empty() {
            midi_out::outputs_from(&[], Err(String::new()), false)
                .into_iter()
                .take(1)
                .collect()
        } else {
            self.outputs.clone()
        };
        midi_out::with_selected(base, current)
    }
}

fn db(v: f32) -> f32 {
    20.0 * v.max(1e-6).log10()
}

/// Largest send delay of an external output in ms (the ring is stereo).
fn lag_ms(out: &midi_out::OutputStatus, sample_rate: u32) -> f64 {
    out.max_lag_samples as f64 / 2.0 * 1000.0 / f64::from(sample_rate.max(1))
}

/// B1-0137: Audio Settings → Diagnostics as plain text (Copy report), so a
/// run on the owner's Windows PC can be profiled: the settings in force,
/// the output provider, then every engine counter on its own line —
/// capacity, sends, drops, lag, underruns and clipping kept apart.
pub fn diagnostics_report(
    a: &crate::settings::AudioSettings,
    d: Option<&crate::audio::AudioDiagnostics>,
    out: &midi_out::OutputStatus,
    soundfont: Option<&str>,
) -> String {
    let mut r = format!(
        "PlumSpirit {} ({} {}) audio diagnostics\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let mut line = |k: &str, v: String| r.push_str(&format!("{k}: {v}\n"));
    let cfg = a.synth();
    let (shards, per) = cfg.shards();
    line(
        "Render mode",
        if a.prerender {
            format!("Pre-Render, {} s ahead", a.render_ahead_ms / 1000)
        } else {
            "Realtime".to_owned()
        },
    );
    line(
        "Voices",
        format!(
            "{} ({shards} × {per}) · audition reserve {} · note-ons per block {}",
            a.max_voices, a.audition_voices, a.note_ons_per_block
        ),
    );
    line("Voice policy", a.voice_policy.label().to_owned());
    line(
        "Effects / gain / limiter",
        format!(
            "{} · {:+.1} dB · {}",
            if a.effects { "on" } else { "off" },
            a.gain_db,
            if a.limiter { "on" } else { "off" }
        ),
    );
    line("SoundFont", soundfont.unwrap_or("none").to_owned());
    // B2-F12: the playback-only Event Processing settings.
    let p = &a.processing;
    line(
        "Event processing",
        if p.is_off() {
            "off".to_owned()
        } else {
            format!(
                "88 keys {} · ignored velocities {} · length {} · Note Off delay {} · resets {}",
                if p.keys_88 { "on" } else { "off" },
                p.velocity_band
                    .map_or("off".to_owned(), |(lo, hi)| format!("{lo} – {hi}")),
                p.length_override_ns
                    .map_or("own".to_owned(), |n| format!("{} s", n as f64 / 1e9)),
                p.off_delay_ns
                    .map_or("none".to_owned(), |n| format!("{} s", n as f64 / 1e9)),
                p.resets.key(),
            )
        },
    );
    line(
        "MIDI output",
        format!(
            "{} ({}) · {}",
            out.target.label(),
            out.target.kind().name(),
            out.health_text()
        ),
    );
    match d {
        None => line(
            "Engine",
            "not started yet (press Play or audition a note once)".to_owned(),
        ),
        Some(d) => {
            let ms = |f: f64| f * 1000.0 / f64::from(d.sample_rate.max(1));
            line(
                "MIDI output messages",
                format!(
                    "{} sent · {} dropped (queue full) · worst send delay {:.1} ms",
                    out.sent,
                    out.dropped,
                    lag_ms(out, d.sample_rate)
                ),
            );
            line(
                "Device",
                format!("{} · {} Hz × {} ch", d.device, d.sample_rate, d.channels),
            );
            line(
                "Callback",
                format!(
                    "{} frames ({:.1} ms){}",
                    d.callback_frames,
                    ms(f64::from(d.callback_frames)),
                    d.buffer.map_or(String::new(), |b| format!(" · fixed {b}"))
                ),
            );
            line(
                "Buffered ahead",
                format!(
                    "{:.0} ms of {:.0} ms",
                    ms(d.buffered as f64),
                    ms(d.ring_frames as f64)
                ),
            );
            line(
                "Synthesis load",
                format!("{:.0} % of real time", d.synth_load * 100.0),
            );
            line("Underruns", d.underruns.to_string());
            line(
                "Scheduled notes / configured voices",
                format!("{} / {}", d.sounding, d.capacity),
            );
            line(
                "Blocks above held-note bound",
                d.overflow_blocks.to_string(),
            );
            line("Note-ons at held-note bound", d.at_capacity.to_string());
            line(
                "Notes not started (voice policy)",
                d.not_started.to_string(),
            );
            line(
                "Held notes released (oldest policy)",
                d.released_oldest.to_string(),
            );
            line(
                "Note-ons deferred",
                format!("{} (latest {:.1} ms late)", d.deferred, d.max_lateness_ms),
            );
            line("Note-ons dropped as too late", d.late_dropped.to_string());
            line("Note-ons skipped in all", d.skipped.to_string());
            line(
                "Kept out by Event Processing",
                format!(
                    "{} outside the 88 keys · {} in the ignored velocities",
                    d.filtered_keys, d.filtered_velocity
                ),
            );
            line(
                "System resets / re-struck keys",
                format!(
                    "{} sent · {} ignored as repeats · {} held notes ended by a re-strike",
                    d.resets_sent, d.resets_ignored, d.retriggered
                ),
            );
            line(
                "Output peak",
                format!("{:+.1} dBFS → {:+.1} dBFS", db(d.peak_pre), db(d.peak_post)),
            );
            line("Clipped samples", d.clipped.to_string());
            line("Limited frames", d.limited.to_string());
            for n in &d.notes {
                line("Note", n.clone());
            }
        }
    }
    r
}

/// A horizontal level bar from −60 to +12 dBFS with the 0 dBFS mark.
fn voice_policy_ui(ui: &mut egui::Ui, policy: &mut crate::audio::synth::VoicePolicy) {
    ui.label(egui::RichText::new("Voice policy").strong());
    for p in crate::audio::synth::VoicePolicy::ALL {
        ui.radio_value(policy, p, p.label());
    }
    ui.label(egui::RichText::new("At a shard's held-note bound: keep sounding notes refuses the new note; oldest sends a Note Off for the oldest held key in that shard, including its repeats. Both apply in Realtime and Pre-Render. External MIDI outputs use their own voice management.").small().weak());
    ui.label(egui::RichText::new("Held notes are a lower bound on voices: layered presets, sustain and release tails still use RustySynth's own quietest-first stealing. Actual voices and internal steals are not exposed by RustySynth. Policy counters count notes, not voices.").small().weak());
}

fn meter(ui: &mut egui::Ui, label: &str, value_db: f32, over: bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(180.0, 10.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let t = ((value_db + 60.0) / 72.0).clamp(0.0, 1.0);
        let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * t, rect.height()));
        let c = if over {
            egui::Color32::from_rgb(0xD0, 0x40, 0x40)
        } else if value_db > -6.0 {
            egui::Color32::from_rgb(0xD8, 0xA0, 0x30)
        } else {
            egui::Color32::from_rgb(0x4C, 0xA0, 0x6A)
        };
        p.rect_filled(fill, 0.0, c);
        let zero = rect.left() + rect.width() * (60.0 / 72.0);
        p.vline(
            zero,
            rect.y_range(),
            egui::Stroke::new(1.0, ui.visuals().text_color()),
        );
        ui.label(
            RichText::new(if value_db <= -59.0 {
                "−∞".to_owned()
            } else {
                format!("{value_db:+.1} dBFS")
            })
            .small()
            .monospace(),
        );
    });
}

impl PlumSpiritApp {
    /// Sends the audio settings to the transport (start-up and every change).
    pub(super) fn apply_audio_settings(&mut self) {
        let a = &self.settings.audio;
        self.transport.configure_synth(a.synth());
        self.transport.configure_output(a.output());
        self.transport
            .configure_render(a.prerender, a.render_ahead_ms);
        self.transport.set_resume_restrike(a.resume_restrike);
        self.transport.set_default_output(a.default_output.clone());
    }

    /// The default output was chosen (transport chooser or Audio Settings).
    pub(super) fn choose_default_output(&mut self, target: OutputTarget) {
        if self.settings.audio.default_output == target {
            return;
        }
        let label = target.label();
        self.settings.audio.default_output = target;
        self.apply_audio_settings();
        self.save_settings();
        self.set_status(
            format!("Tracks set to Internal now play on {label}."),
            StatusKind::Info,
        );
    }

    /// The transport chip: label, tooltip (provider, device, health) and
    /// whether it needs attention.
    pub(super) fn output_chip(&self) -> (String, String, bool, bool) {
        let st = self.transport.output_status();
        let full = st.target.label();
        let label = if full.chars().count() > 28 {
            format!("{}…", full.chars().take(27).collect::<String>())
        } else {
            full.clone()
        };
        let tip = format!(
            "Output: {full}\nProvider: {}\nState: {}\n\nTracks set to Internal in Track Properties play here; a track routed to a device keeps it. Click to choose; the internal synthesizer always works.",
            st.target.kind().name(),
            st.health_text()
        );
        let open = matches!(
            st.health,
            midi_out::OutputHealth::Open | midi_out::OutputHealth::Internal
        );
        (label, tip, st.is_warning(), open)
    }

    pub(super) fn open_audio_settings(&mut self) {
        self.audio_window.open = true;
        if self.audio_window.devices.is_none() && self.audio_window.listing.is_none() {
            self.audio_window.refresh();
        }
    }

    /// B2-F12: the Event Processing preflight of the open song, counted again
    /// only when the song or the band changes.
    fn event_preflight(&mut self) -> Option<plumspirit_engine::playback::Preflight> {
        let band = self.audio_window.events.band;
        let doc = self.doc()?;
        let key = (doc.analysis_revision(), band);
        if let Some((k, p)) = self.audio_window.events.preflight
            && k == key
        {
            return Some(p);
        }
        let p = plumspirit_engine::playback::EventProcessing::preflight(doc, band);
        self.audio_window.events.preflight = Some((key, p));
        Some(p)
    }

    pub(super) fn audio_settings_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.audio_window.open {
            return;
        }
        self.audio_window.poll();
        let preflight = self.event_preflight();
        let mut open = true;
        let before = self.settings.audio.clone();
        let diag = self.transport.audio_diagnostics();
        let mut add_soundfont = false;
        let mut use_soundfont: Option<String> = None;
        let current_sf = self
            .transport
            .soundfont()
            .map(|s| s.path.to_string_lossy().into_owned());
        self.audio_window.poll_outputs(ctx);
        let output_choices = self
            .audio_window
            .output_choices(&self.settings.audio.default_output);
        let output_status = self.transport.output_status();
        let outputs_listing = self.audio_window.outputs_listing();
        let mut refresh_outputs = false;
        let mut copy_report = false;
        let mut want_help = false;
        egui::Window::new("Audio Settings")
            .open(&mut open)
            .resizable(true)
            .default_width(460.0)
            .default_height(600.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(620.0).show(ui, |ui| {
                    let a = &mut self.settings.audio;
                    // ------------------------------------------------ Output
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Output").strong());
                        want_help |= super::help::help_button(ui, "playback");
                    });
                    let devices = self.audio_window.devices.clone().unwrap_or_default();
                    egui::Grid::new("audio-output").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Device");
                        ui.horizontal(|ui| {
                            let shown = a.device.clone().unwrap_or_else(|| "System default".into());
                            egui::ComboBox::from_id_salt("audio-device")
                                .selected_text(shown)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut a.device, None, "System default");
                                    for d in &devices {
                                        let label = if d.is_default { format!("{} (default)", d.name) } else { d.name.clone() };
                                        ui.selectable_value(&mut a.device, Some(d.name.clone()), label);
                                    }
                                });
                            if ui.small_button("Refresh").clicked() {
                                self.audio_window.refresh();
                            }
                        });
                        ui.end_row();
                        let dev = devices.iter().find(|d| match &a.device {
                            Some(n) => &d.name == n,
                            None => d.is_default,
                        });
                        ui.label("Sample rate");
                        let rates = dev.map(|d| d.rates.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("audio-rate")
                            .selected_text(a.sample_rate.map_or("Device default".into(), |r| format!("{r} Hz")))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut a.sample_rate, None, "Device default");
                                for r in rates {
                                    ui.selectable_value(&mut a.sample_rate, Some(r), format!("{r} Hz"));
                                }
                            });
                        ui.end_row();
                        ui.label("Buffer");
                        let rate = a.sample_rate.or(dev.and_then(|d| d.default_rate)).unwrap_or(48_000);
                        let buffers = dev.map(|d| d.buffers.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("audio-buffer")
                            .selected_text(a.buffer_frames.map_or("Device default".into(), |b| {
                                format!("{b} frames ({:.1} ms)", output::latency_ms(b, rate))
                            }))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut a.buffer_frames, None, "Device default");
                                for b in buffers {
                                    ui.selectable_value(
                                        &mut a.buffer_frames,
                                        Some(b),
                                        format!("{b} frames ({:.1} ms)", output::latency_ms(b, rate)),
                                    );
                                }
                            });
                        ui.end_row();
                    });
                    if self.audio_window.listing.is_some() {
                        ui.label(RichText::new("Looking for audio devices…").small().weak());
                    } else if devices.is_empty() {
                        ui.label(RichText::new("No audio output device was found.").small().weak());
                    }
                    if let Some(d) = &diag {
                        for n in &d.notes {
                            ui.label(RichText::new(n).small());
                        }
                    }
                    ui.separator();
                    // ------------------------------------ MIDI output (B1-0135)
                    ui.label(RichText::new("MIDI output").strong());
                    ui.horizontal(|ui| {
                        ui.label("Tracks set to Internal play on");
                        egui::ComboBox::from_id_salt("audio-default-output")
                            .selected_text(a.default_output.label())
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                if let Some(t) = crate::ui::transport_bar::output_entries(ui, &output_choices, &a.default_output) {
                                    a.default_output = t;
                                }
                            });
                        if ui.small_button("Refresh").on_hover_text("Look for MIDI devices and OmniMIDI again.").clicked() {
                            refresh_outputs = true;
                        }
                    });
                    ui.label(
                        RichText::new(format!("{} · {}", output_status.target.kind().name(), output_status.health_text()))
                            .small()
                            .color(if output_status.is_warning() { ui.visuals().warn_fg_color } else { ui.visuals().weak_text_color() }),
                    );
                    if outputs_listing {
                        ui.label(RichText::new("Looking for MIDI outputs…").small().weak());
                    }
                    ui.label(RichText::new("A track routed to a device in Track Properties keeps that device. When an output cannot be opened or stops working, its tracks play on the internal synthesizer and a notice says so.").small().weak());
                    // L-B03: what an overloaded MIDI output does.
                    ui.add_enabled_ui(!a.prerender, |ui| {
                        ui.checkbox(&mut a.external_keep_all, "Keep every event (playback may wait)")
                            .on_hover_text("When a MIDI output cannot take events as fast as the song sends them: on, playback waits for it (no event is skipped; the playhead may slow down). Off (default), playback keeps time and new note-ons beyond the output's backlog are not sent — they are counted, shown here and in a notice; the project is never changed. Pre-Render always keeps every event.");
                    });
                    ui.separator();
                    // ------------------------------------------- Render mode
                    ui.label(RichText::new("Render mode").strong());
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut a.prerender, false, "Realtime")
                            .on_hover_text("Low latency for editing: quick Play, seek and note audition.");
                        ui.radio_value(&mut a.prerender, true, "Pre-Render")
                            .on_hover_text("Renders ahead into a bounded buffer before playing: every note starts on time in its own block (no note-on budget, nothing skipped for lateness), even where a dense passage takes longer than real time to render. The voice limit still applies.");
                    });
                    ui.label(RichText::new("Realtime keeps to each 1.3 ms block and counts what it spreads, skips or limits (Diagnostics); Pre-Render plays every event on time; MIDI outputs (OmniMIDI / KDMAPI, devices) use their own polyphony.").small().weak());
                    ui.add_enabled_ui(a.prerender, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Render ahead");
                            let mut secs = a.render_ahead_ms as f32 / 1000.0;
                            if ui
                                .add(crate::ui::stepper::Stepper::new(&mut secs).range(1.0..=60.0).speed(0.001).suffix(" s"))
                                .changed()
                            {
                                a.render_ahead_ms = (secs * 1000.0) as u32;
                            }
                            let rate = diag.as_ref().map_or(48_000, |d| d.sample_rate);
                            let r = crate::audio::engine::RenderConfig::new(true, a.render_ahead_ms, rate);
                            ui.label(RichText::new(format!("buffer {}", crate::diagnostics::format_bytes(r.buffer_bytes() as u64))).small().weak());
                        });
                    });
                    ui.separator();
                    // ------------------------------------------------ Voices
                    ui.label(RichText::new("Voices").strong());
                    egui::Grid::new("audio-voices").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Maximum voices");
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut a.max_voices, synth::MIN_VOICES as u32..=synth::MAX_VOICES as u32).step_by(64.0));
                            let (n, per) = a.synth().shards();
                            ui.label(RichText::new(format!("{n} × {per}")).small().weak())
                                .on_hover_text("RustySynth plays at most 256 voices per synthesizer; more voices run several synthesizers (more CPU).");
                        });
                        ui.end_row();
                        ui.label("Audition reserve");
                        ui.add(egui::Slider::new(&mut a.audition_voices, synth::MIN_AUDITION_VOICES as u32..=synth::MAX_AUDITION_VOICES as u32))
                            .on_hover_text("Voices of the separate note-audition synthesizer (never taken from the song).");
                        ui.end_row();
                        ui.label("Note-ons per block");
                        ui.add(egui::Slider::new(&mut a.note_ons_per_block, synth::MIN_NOTE_ONS as u32..=synth::MAX_NOTE_ONS as u32).logarithmic(true))
                            .on_hover_text("Realtime: notes started per 1.3 ms block; a larger burst is spread over the next blocks (never dropped unless it would be more than 50 ms late). Pre-Render starts every note in its own block.");
                        ui.end_row();
                    });
                    voice_policy_ui(ui, &mut a.voice_policy);
                    ui.separator();
                    // ---------------------------------------------- Headroom
                    ui.label(RichText::new("Headroom").strong());
                    ui.horizontal(|ui| {
                        ui.label("Master output gain");
                        crate::ui::stepper::slider(ui, &mut a.gain_db, synth::MIN_GAIN_DB..=synth::MAX_GAIN_DB, "dB");
                    });
                    ui.checkbox(&mut a.limiter, "Safety limiter (−1 dBFS)")
                        .on_hover_text("Transparent below −1 dBFS; above it, gain is reduced instantly and recovers in 150 ms. Off: overload is counted as clipping.");
                    ui.separator();
                    // ------------------------------------------- Resume (B1-0083)
                    ui.label(RichText::new("Pause and resume").strong());
                    ui.checkbox(&mut a.resume_restrike, "Re-strike held notes when resuming")
                        .on_hover_text("Pausing ends every sound. Off (default): playing on from the same place leaves the notes that were held there silent until they end, so no note is heard twice. On: they sound again from the resume point. Moving the playhead always sounds the notes held at the new place.");
                    if let Some(d) = &diag {
                        let w = &mut self.audio_window;
                        let decay = |shown: f32, new: f32| if new >= shown { new } else { (shown - 1.5).max(new) };
                        w.meter_pre = decay(w.meter_pre, db(d.peak_pre));
                        w.meter_post = decay(w.meter_post, db(d.peak_post));
                        meter(ui, "Before limiter", w.meter_pre, w.meter_pre > 0.0);
                        meter(ui, "Output        ", w.meter_post, d.clipped > 0);
                        ui.label(
                            RichText::new(format!(
                                "Clipped samples: {} · limited frames: {}",
                                d.clipped, d.limited
                            ))
                            .small(),
                        );
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    ui.separator();
                    // ------------------------------- Event processing (B2-F12)
                    event_processing_ui(ui, &mut a.processing, &mut self.audio_window.events, preflight);
                    ui.separator();
                    // -------------------------------------------- SoundFonts
                    ui.label(RichText::new("SoundFonts").strong());
                    ui.checkbox(&mut a.effects, "SoundFont effects (reverb and chorus)");
                    let mut remove = None;
                    let mut swap = None;
                    let n = a.soundfonts.len();
                    for (i, p) in a.soundfonts.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let name = std::path::Path::new(p)
                                .file_name()
                                .map_or(p.clone(), |f| f.to_string_lossy().into_owned());
                            let active = current_sf.as_deref() == Some(p.as_str());
                            let missing = !std::path::Path::new(p).is_file();
                            let mut label = RichText::new(name);
                            if active {
                                label = label.strong();
                            }
                            if missing {
                                label = label.weak().strikethrough();
                            }
                            if ui
                                .add_enabled(!missing, egui::Button::selectable(active, label))
                                .on_hover_text(if missing { format!("{p}\nThe file is missing.") } else { p.clone() })
                                .clicked()
                            {
                                use_soundfont = Some(p.clone());
                            }
                            if ui.add_enabled(i > 0, egui::Button::new("↑").small()).clicked() {
                                swap = Some((i, i - 1));
                            }
                            if ui.add_enabled(i + 1 < n, egui::Button::new("↓").small()).clicked() {
                                swap = Some((i, i + 1));
                            }
                            if ui.small_button("Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some((x, y)) = swap {
                        a.soundfonts.swap(x, y);
                    }
                    if let Some(i) = remove {
                        a.soundfonts.remove(i);
                    }
                    if ui.button("Add SoundFont…").clicked() {
                        add_soundfont = true;
                    }
                    ui.label(RichText::new("Click a SoundFont to use it (for this project and as the default). Only the one in use is loaded.").small().weak());
                    ui.separator();
                    // ------------------------------------------- Diagnostics
                    egui::CollapsingHeader::new("Diagnostics").default_open(false).show(ui, |ui| {
                        if ui
                            .small_button("Copy report")
                            .on_hover_text("Copies these diagnostics and the audio settings as text (for a bug report or a profiling run).")
                            .clicked()
                        {
                            copy_report = true;
                        }
                        let rate = diag.as_ref().map_or(48_000, |d| d.sample_rate);
                        egui::Grid::new("audio-diag-midi").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                            for (k, v) in [
                                ("MIDI output", output_status.target.label()),
                                ("Provider", output_status.target.kind().name().to_owned()),
                                ("State", output_status.health_text()),
                                (
                                    "Messages",
                                    format!(
                                        "{} sent · {} dropped · worst send delay {:.1} ms",
                                        output_status.sent,
                                        output_status.dropped,
                                        lag_ms(&output_status, rate)
                                    ),
                                ),
                            ] {
                                ui.label(RichText::new(k).small());
                                ui.label(RichText::new(v).small().monospace());
                                ui.end_row();
                            }
                        });
                        match &diag {
                            None => {
                                ui.label(RichText::new("Starts with the first Play or note audition.").small().weak());
                            }
                            Some(d) => {
                                egui::Grid::new("audio-diag").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                        ui.label(RichText::new(k).small());
                                        ui.label(RichText::new(v).small().monospace());
                                        ui.end_row();
                                    };
                                    let ms = |f: f64| f * 1000.0 / f64::from(d.sample_rate.max(1));
                                    row(ui, "Device", format!("{} · {} Hz × {} ch", d.device, d.sample_rate, d.channels));
                                    row(ui, "Callback", format!("{} frames ({:.1} ms){}", d.callback_frames, ms(f64::from(d.callback_frames)), d.buffer.map_or(String::new(), |b| format!(" · fixed {b}"))));
                                    row(ui, "Buffered ahead", format!("{:.0} ms of {:.0} ms", ms(d.buffered as f64), ms(d.ring_frames as f64)));
                                    if let Some(p) = d.preroll {
                                        row(ui, "Pre-Render", format!("filling {:.0} %", p * 100.0));
                                    }
                                    row(ui, "Synthesis load", format!("{:.0} % of real time", d.synth_load * 100.0));
                                    row(ui, "Underruns", d.underruns.to_string());
                                    row(ui, "Scheduled notes / voices", format!("{} / {}", d.sounding, d.capacity));
                                    row(ui, "Blocks above held-note bound", d.overflow_blocks.to_string());
                                    row(ui, "Note-ons at held-note bound", d.at_capacity.to_string());
                                    row(ui, "Notes not started (voice policy)", d.not_started.to_string());
                                    row(ui, "Held notes released (oldest policy)", d.released_oldest.to_string());
                                    row(ui, "Note-ons deferred", format!("{} (latest {:.1} ms late)", d.deferred, d.max_lateness_ms));
                                    row(ui, "Note-ons dropped", format!("{} too late · {} skipped in all", d.late_dropped, d.skipped));
                                    if d.filtered_keys + d.filtered_velocity > 0 {
                                        row(ui, "Kept out by Event Processing", format!("{} outside the 88 keys · {} in the ignored velocities", d.filtered_keys, d.filtered_velocity));
                                    }
                                    if d.resets_sent + d.resets_ignored + d.retriggered > 0 {
                                        row(ui, "System resets / re-struck keys", format!("{} sent · {} ignored as repeats · {} held notes ended by a re-strike", d.resets_sent, d.resets_ignored, d.retriggered));
                                    }
                                    row(ui, "Output peak", format!("{:+.1} dBFS → {:+.1} dBFS", db(d.peak_pre), db(d.peak_post)));
                                    row(ui, "Clipped samples", d.clipped.to_string());
                                });
                            }
                        }
                    });
                });
            });
        self.audio_window.open = open;
        if want_help {
            self.open_help("playback");
        }
        if refresh_outputs {
            self.audio_window.refresh_outputs(ctx);
        }
        if copy_report {
            let text = diagnostics_report(
                &self.settings.audio,
                diag.as_ref(),
                &output_status,
                current_sf.as_deref(),
            );
            ctx.copy_text(text);
            self.set_status(
                "Audio diagnostics copied to the clipboard.",
                StatusKind::Info,
            );
        }
        if self.settings.audio != before {
            self.settings.audio.max_voices =
                (self.settings.audio.max_voices / 64 * 64).max(synth::MIN_VOICES as u32);
            self.apply_audio_settings();
            self.save_settings();
            let a = &self.settings.audio;
            if a.output() != before.output() && self.transport.state().is_running() {
                self.set_status(
                    "Audio device settings take effect at the next Play.",
                    StatusKind::Info,
                );
            }
        }
        if let Some(p) = use_soundfont {
            self.soundfont_chosen(PathBuf::from(p));
        }
        if add_soundfont {
            self.choose_soundfont(frame);
        }
    }
}

#[cfg(test)]
mod event_processing_tests {
    use super::{EventEdit, event_processing_ui};
    use plumspirit_engine::playback::{EventProcessing, Preflight, ResetPolicy};

    fn frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        p: &mut EventProcessing,
        e: &mut EventEdit,
        pre: Option<Preflight>,
    ) -> Vec<(String, egui::Pos2)> {
        let input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| event_processing_ui(ui, p, e, pre));
        out.textures_delta.clear();
        crate::ui::testkit::texts(&out.shapes)
    }

    fn click(at: egui::Pos2) -> Vec<egui::Event> {
        let b = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        vec![egui::Event::PointerMoved(at), b(true), b(false)]
    }

    fn at(t: &[(String, egui::Pos2)], start: &str) -> egui::Pos2 {
        t.iter()
            .find(|(s, _)| s.starts_with(start))
            .unwrap_or_else(|| panic!("{start}: {t:?}"))
            .1
    }

    #[test]
    fn voice_policy_choices_apply_and_explain_their_limits() {
        use crate::audio::synth::VoicePolicy;
        let ctx = egui::Context::default();
        let mut policy = VoicePolicy::default();
        let mut frame = |events| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(650.0, 500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| super::voice_policy_ui(ui, &mut policy),
            );
            out.textures_delta.clear();
            (crate::ui::testkit::texts(&out.shapes), policy)
        };
        let (t, p) = frame(vec![]);
        assert_eq!(p, VoicePolicy::Quietest);
        assert!(t.iter().any(|(s, _)| s.contains("lower bound on voices")));
        for p in [
            VoicePolicy::KeepSounding,
            VoicePolicy::Oldest,
            VoicePolicy::Quietest,
        ] {
            let (t, _) = frame(vec![]);
            let (_, got) = frame(click(at(&t, p.label())));
            assert_eq!(got, p);
        }
    }

    /// B2-F12 panel: the stages in their order with the non-destructive
    /// note, each switched on and off on its own and applied to the
    /// settings at once; an invalid band is refused in words and applies
    /// nothing; the resets' choice states its risk; the preflight counts.
    #[test]
    fn the_event_processing_panel_switches_each_stage_on_its_own() {
        let ctx = egui::Context::default();
        let mut p = EventProcessing::default();
        let mut e = EventEdit::default();
        let pre = Preflight {
            notes: 12_345,
            outside_keys: 7,
            in_band: 89,
            resets: 3,
            repeated_resets: 1,
        };
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        let order: Vec<f32> = [
            "1. Only the 88",
            "2. Ignore notes",
            "3. Every note lasts",
            "4. Delay every",
        ]
        .iter()
        .map(|s| at(&t, s).y)
        .collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "the stages in their order"
        );
        assert!(t.iter().any(|(s, _)| s.starts_with("Not destructive")));
        assert!(t.iter().any(|(s, _)| s == "This song: 12,345 notes · 7 outside the 88 keys · 89 with velocities 1 – 10 · 3 system resets (1 repeats)"), "{t:?}");
        assert!(p.is_off(), "nothing on until asked");
        // 1. on.
        frame(
            &ctx,
            click(at(&t, "1. Only the 88")),
            &mut p,
            &mut e,
            Some(pre),
        );
        assert!(p.keys_88 && p.velocity_band.is_none());
        // 2. on with the default band, then an inverted band: refused.
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        frame(
            &ctx,
            click(at(&t, "2. Ignore notes")),
            &mut p,
            &mut e,
            Some(pre),
        );
        assert_eq!(p.velocity_band, Some((1, 10)));
        e.band = (10, 5);
        p.velocity_band = None;
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        assert_eq!(p.velocity_band, None);
        assert!(
            t.iter()
                .any(|(s, _)| s == "The lowest velocity (10) must not be above the highest (5)."),
            "{t:?}"
        );
        e.band = (0, 0);
        frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        assert_eq!(p.velocity_band, Some((0, 0)), "on again once valid");
        // 3. and 4.
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        frame(
            &ctx,
            click(at(&t, "3. Every note lasts")),
            &mut p,
            &mut e,
            Some(pre),
        );
        assert_eq!(p.length_override_ns, Some(100_000_000));
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        frame(
            &ctx,
            click(at(&t, "4. Delay every")),
            &mut p,
            &mut e,
            Some(pre),
        );
        assert_eq!(p.off_delay_ns, Some(50_000_000));
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        assert!(t.iter().any(|(s, _)| s.starts_with("While 3 or 4 is on")));
        // Off again: the values stay for next time.
        frame(
            &ctx,
            click(at(&t, "3. Every note lasts")),
            &mut p,
            &mut e,
            Some(pre),
        );
        assert_eq!(p.length_override_ns, None);
        assert_eq!(e.length_s, 0.1);
        // Resets.
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        frame(
            &ctx,
            click(at(&t, "Sent, ignoring")),
            &mut p,
            &mut e,
            Some(pre),
        );
        assert_eq!(p.resets, ResetPolicy::SendChanged);
        let t = frame(&ctx, vec![], &mut p, &mut e, Some(pre));
        assert!(
            t.iter()
                .any(|(s, _)| s.starts_with("Ignoring a repeated reset keeps"))
        );
        frame(&ctx, click(at(&t, "Not sent")), &mut p, &mut e, Some(pre));
        assert_eq!(p.resets, ResetPolicy::Skip);
        assert_eq!(super::seconds_ns(1e-12, 1), 1, "never 0 for an override");
        assert_eq!(super::seconds_ns(1e30, 0), u64::MAX);
    }
}

#[cfg(test)]
mod report_tests {
    use super::diagnostics_report;
    use crate::audio::AudioDiagnostics;
    use crate::audio::midi_out::{OutputHealth, OutputStatus, OutputTarget};
    use crate::settings::AudioSettings;

    /// B1-0137: the copied report keeps capacity, sends, drops, lag,
    /// underruns and clipping on separate lines, before and after the engine
    /// started.
    #[test]
    fn the_diagnostics_report_keeps_every_quantity_apart() {
        let a = AudioSettings::default();
        let mut out = OutputStatus {
            target: OutputTarget::Kdmapi,
            health: OutputHealth::Open,
            dropped: 3,
            sent: 1_000,
            max_lag_samples: 96,
            waiting: 0,
        };
        let idle = diagnostics_report(&a, None, &out, None);
        assert!(idle.contains("Engine: not started yet"));
        assert!(idle.contains(
            "MIDI output: OmniMIDI (KDMAPI) (OmniMIDI KDMAPI) · open · 3 messages dropped"
        ));
        assert!(idle.contains("Render mode: Realtime"));
        let d = AudioDiagnostics {
            device: "Speakers".into(),
            sample_rate: 48_000,
            channels: 2,
            underruns: 4,
            sounding: 900,
            capacity: 1_024,
            late_dropped: 7,
            skipped: 9,
            clipped: 11,
            at_capacity: 17,
            not_started: 13,
            released_oldest: 19,
            ..Default::default()
        };
        out.health = OutputHealth::Open;
        let r = diagnostics_report(&a, Some(&d), &out, Some("C:\\SF\\GeneralUser GS.sf2"));
        for want in [
            "MIDI output messages: 1000 sent · 3 dropped (queue full) · worst send delay 1.0 ms",
            "Underruns: 4",
            "Voice policy: RustySynth (quietest voice first)",
            "Note-ons at held-note bound: 17",
            "Notes not started (voice policy): 13",
            "Held notes released (oldest policy): 19",
            "Scheduled notes / configured voices: 900 / 1024",
            "Note-ons dropped as too late: 7",
            "Note-ons skipped in all: 9",
            "Clipped samples: 11",
            "SoundFont: C:\\SF\\GeneralUser GS.sf2",
            "Event processing: off",
            "System resets / re-struck keys: 0 sent · 0 ignored as repeats · 0 held notes ended by a re-strike",
        ] {
            assert!(r.contains(want), "{want:?} missing from\n{r}");
        }
        assert_eq!(r.lines().filter(|l| l.contains("Underruns")).count(), 1);
    }
}
