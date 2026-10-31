//! Plugins → Audio Reference (v0.8, contract §054–§061; behaviour studied
//! from dominohook's Reference Audio by Khang — no licence, so this is an
//! original implementation). A recording plays with the project, in sync
//! through the Conductor's tempo map (see `audio/reference.rs`); its file,
//! offset, volume and mute are project settings (saved in `.lup`, outside
//! Undo) and never reach a MIDI file.

use super::{PlumSpiritApp, StatusKind};
use crate::audio::reference::{self, RefAudio};
use egui::{Color32, RichText, Sense, Stroke};
use plumspirit_engine::model::project::{
    MAX_REFERENCE_OFFSET_US, MAX_REFERENCE_VOLUME_PCT, ReferenceAudio,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// A change to the reference settings (applied after the window is drawn).
type RefEdit = Box<dyn FnOnce(&mut ReferenceAudio)>;

struct Loading {
    /// The project's path for it (what it is loaded for).
    key: String,
    rx: Receiver<Result<RefAudio, String>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

/// The window and the decoded recording.
#[derive(Default)]
pub struct ReferenceWin {
    pub open: bool,
    /// (the project's path for it, the recording, the file it was read from).
    loaded: Option<(String, Arc<RefAudio>, PathBuf)>,
    loading: Option<Loading>,
    /// (the project's path for it, what went wrong; `true`: the file is missing).
    problem: Option<(String, String, bool)>,
    /// A file chosen to become the reference (dialog, prompt, drop).
    pub pending: Option<PathBuf>,
    /// The reference time to put at the edit cursor (Align).
    align_at: f64,
}

impl ReferenceWin {
    /// Close Project, on the reaper: release decoded audio and any load result.
    pub(super) fn release(mut self) {
        if let Some(j) = self.loading.take() {
            j.cancel.store(true, Ordering::Relaxed);
            drop(j.rx.into_iter().last());
        }
    }
}

/// `m:ss.s` (or `h:mm:ss.s`).
pub fn clock(seconds: f64) -> String {
    let neg = seconds < 0.0;
    let s = seconds.abs();
    let h = (s / 3600.0).floor();
    let m = ((s - h * 3600.0) / 60.0).floor();
    let r = s - h * 3600.0 - m * 60.0;
    let sign = if neg { "−" } else { "" };
    if h > 0.0 {
        format!("{sign}{h}:{m:02}:{r:04.1}")
    } else {
        format!("{sign}{m}:{r:04.1}")
    }
}

/// Where a reference file can be: as saved, else next to the project
/// (a moved project folder).
pub fn resolve(saved: &str, project_file: Option<&Path>) -> Option<PathBuf> {
    let p = PathBuf::from(saved);
    if p.is_file() {
        return Some(p);
    }
    let name = Path::new(&saved.replace('\\', "/")).file_name()?.to_owned();
    let near = project_file?.parent()?.join(name);
    near.is_file().then_some(near)
}

pub fn is_audio_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        reference::AUDIO_EXTENSIONS
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

impl PlumSpiritApp {
    pub(super) fn open_reference(&mut self) {
        self.reference_win.open = true;
    }

    /// Every frame (window open or not): loads what the project names,
    /// keeps the transport's reference in step with the settings.
    pub(super) fn reference_frame(&mut self, ctx: &egui::Context) {
        // a chosen file becomes the project's reference
        if let Some(path) = self.reference_win.pending.take() {
            let text = path.to_string_lossy().into_owned();
            match self.doc_mut() {
                Some(d) => {
                    let r = d
                        .project_mut()
                        .reference
                        .get_or_insert_with(|| ReferenceAudio::new(String::new()));
                    if r.path != text {
                        r.path = text;
                        r.offset_us = 0;
                    }
                    self.reference_win.problem = None;
                    self.reference_win.open = true;
                }
                None => self.set_status(
                    "Open a project (and wait for the current job) before loading reference audio.",
                    StatusKind::Warning,
                ),
            }
        }
        let wanted = self.doc().and_then(|d| d.project().reference.clone());
        let project_file = self.open.as_ref().and_then(|o| match &o.source {
            super::DocSource::Project(p) | super::DocSource::Midi(p) => Some(p.clone()),
            super::DocSource::New => None,
        });
        let w = &mut self.reference_win;
        // a finished load
        if let Some(l) = &w.loading {
            match l.rx.try_recv() {
                Ok(r) => {
                    let key = l.key.clone();
                    let took = l.started.elapsed();
                    w.loading = None;
                    match r {
                        Ok(a) => {
                            log::info!(
                                "reference audio: {} {} Hz, {:.1} s, {} MB, read in {:.0} ms",
                                a.format,
                                a.rate,
                                a.duration_seconds(),
                                a.memory_bytes() >> 20,
                                took.as_secs_f64() * 1000.0
                            );
                            let file = resolve(&key, project_file.as_deref())
                                .unwrap_or_else(|| key.clone().into());
                            w.loaded = Some((key, Arc::new(a), file));
                        }
                        Err(e) => w.problem = Some((key, e, false)),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let key = l.key.clone();
                    w.loading = None;
                    w.problem = Some((
                        key,
                        "Reading the recording stopped unexpectedly.".into(),
                        false,
                    ));
                }
            }
        }
        match &wanted {
            None => {
                if let Some(l) = w.loading.take() {
                    l.cancel.store(true, Ordering::Relaxed);
                }
                w.loaded = None;
                w.problem = None;
            }
            Some(r) => {
                let key = r.path.clone();
                let have = w.loaded.as_ref().is_some_and(|l| l.0 == key);
                let busy = w.loading.as_ref().is_some_and(|l| l.key == key);
                let failed = w.problem.as_ref().is_some_and(|p| p.0 == key);
                if !have && !busy && !failed {
                    if let Some(l) = w.loading.take() {
                        l.cancel.store(true, Ordering::Relaxed);
                    }
                    w.loaded = None;
                    match resolve(&key, project_file.as_deref()) {
                        None => {
                            w.problem = Some((key, "The file was not found.".into(), true));
                        }
                        Some(file) => {
                            let budget = reference::memory_budget(
                                crate::platform::total_memory_bytes().unwrap_or(2 << 30),
                            );
                            let cancel = Arc::new(AtomicBool::new(false));
                            let c2 = cancel.clone();
                            let (tx, rx) = channel();
                            let c = ctx.clone();
                            let spawned = std::thread::Builder::new()
                                .name("plumspirit-reference".into())
                                .spawn(move || {
                                    let _ = tx.send(reference::decode_file(&file, budget, &c2));
                                    c.request_repaint();
                                });
                            match spawned {
                                Ok(_) => {
                                    w.loading = Some(Loading {
                                        key,
                                        rx,
                                        cancel,
                                        started: Instant::now(),
                                    })
                                }
                                Err(e) => {
                                    w.problem =
                                        Some((key, format!("Reading could not start: {e}."), false))
                                }
                            }
                        }
                    }
                }
            }
        }
        // what plays
        let audio = match (&wanted, &w.loaded) {
            (Some(r), Some(l)) if l.0 == r.path => Some(l.1.clone()),
            _ => None,
        };
        let (offset, gain) = wanted.as_ref().map_or((0.0, 0.0), |r| {
            (
                r.offset_seconds(),
                if r.mute {
                    0.0
                } else {
                    f32::from(r.volume_pct) / 100.0
                },
            )
        });
        self.transport.set_reference(audio, offset, gain);
    }

    /// Asks for a recording (native dialog, or the in-app prompt).
    fn request_reference_path(&mut self, frame: &eframe::Frame) {
        use super::dialogs::{Dialog, PromptPurpose};
        use super::files;
        if !files::NATIVE_DIALOGS {
            self.dialog = Some(Dialog::PathPrompt {
                purpose: PromptPurpose::OpenReference,
                text: self
                    .last_dir
                    .as_ref()
                    .map(|d| format!("{}/", d.display()))
                    .unwrap_or_default(),
                focused: false,
            });
            return;
        }
        self.perf.skip_current_frame();
        let dir = self.last_dir.clone();
        if let Some(p) = files::pick_open(frame, files::FileKind::Audio, dir.as_deref()) {
            self.reference_win.pending = Some(p);
        }
    }

    fn edit_reference(&mut self, f: impl FnOnce(&mut ReferenceAudio)) {
        if let Some(d) = self.doc_mut()
            && let Some(r) = d.project_mut().reference.as_mut()
        {
            f(r);
        }
    }

    /// Seconds of project time at a tick (the Conductor's tempo map).
    fn project_seconds(&self, tick: u64) -> f64 {
        self.doc().map_or(0.0, |d| {
            plumspirit_engine::model::TempoMap::for_document_at(
                d,
                plumspirit_engine::transform::TEMPO_RATE,
                plumspirit_engine::model::Speed::NORMAL,
            )
            .seconds_at_tick(tick)
        })
    }

    /// The window (once per frame).
    pub(super) fn reference_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.reference_win.open {
            return;
        }
        if self.open.is_none() {
            self.reference_win.open = false;
            return;
        }
        let info =
            plumspirit_engine::plugins::info(plumspirit_engine::plugins::PluginId::AudioReference);
        let wanted = self.doc().and_then(|d| d.project().reference.clone());
        let cursor = self.roll.cursor_tick;
        let cursor_s = self.project_seconds(cursor);
        let play_s = self.project_seconds(self.transport.position());
        let mut open = true;
        let mut load = false;
        let mut remove = false;
        let mut edits: Vec<RefEdit> = Vec::new();
        let mut stop_loading = false;
        egui::Window::new("Audio Reference — Plugins")
            .id(egui::Id::new("plumspirit-reference"))
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                ui.label(RichText::new(super::plugins::credit_line(info)).small());
                ui.add_space(4.0);
                let w = &mut self.reference_win;
                let Some(r) = wanted.clone() else {
                    ui.label("Play a recording with the project, in time with the tempo map.");
                    ui.label(
                        RichText::new("WAV, FLAC, Ogg Vorbis or MP3. You can also drop the file on the window.")
                            .weak(),
                    );
                    if ui.button("Load Reference Audio…").clicked() {
                        load = true;
                    }
                    return;
                };
                let name = Path::new(&r.path.replace('\\', "/"))
                    .file_name()
                    .map_or_else(|| r.path.clone(), |n| n.to_string_lossy().into_owned());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Remove").on_hover_text("The project plays without reference audio.").clicked() {
                            remove = true;
                        }
                        if ui.button("Change…").clicked() {
                            load = true;
                        }
                    });
                });
                ui.label(RichText::new(&r.path).weak().small());
                if let Some(l) = &w.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Reading… {:.1} s", l.started.elapsed().as_secs_f64()));
                        if ui.button("Stop").clicked() {
                            stop_loading = true;
                        }
                    });
                }
                if let Some((_, msg, missing)) = w.problem.as_ref().filter(|p| p.0 == r.path) {
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                    if *missing && ui.button("Relocate…").on_hover_text("Find the file where it is now.").clicked() {
                        load = true;
                    }
                }
                let audio = w.loaded.as_ref().filter(|l| l.0 == r.path).map(|l| l.1.clone());
                if let Some(a) = &audio {
                    ui.label(
                        RichText::new(format!(
                            "{} · {} Hz · {} · {} · {:.1} MB in memory",
                            a.format,
                            a.rate,
                            if a.channels == 1 { "mono".to_owned() } else { format!("{} channels", a.channels) },
                            clock(a.duration_seconds()),
                            a.memory_bytes() as f64 / 1048576.0
                        ))
                        .weak(),
                    );
                }
                ui.separator();
                // volume / mute
                ui.horizontal(|ui| {
                    ui.label("Volume");
                    let mut v = r.volume_pct;
                    if ui
                        .add(egui::Slider::new(&mut v, 0..=MAX_REFERENCE_VOLUME_PCT).suffix(" %"))
                        .changed()
                    {
                        edits.push(Box::new(move |x| x.volume_pct = v));
                    }
                    let mut m = r.mute;
                    if ui.checkbox(&mut m, "Mute").changed() {
                        edits.push(Box::new(move |x| x.mute = m));
                    }
                });
                // offset
                ui.horizontal(|ui| {
                    ui.label("Starts at");
                    let mut s = r.offset_seconds();
                    let lim = MAX_REFERENCE_OFFSET_US as f64 / 1e6;
                    if ui
                        .add(
                            crate::ui::stepper::Stepper::new(&mut s)
                                .range(-lim..=lim)
                                .speed(0.001)
                                .max_decimals(3)
                                .suffix(" s"),
                        )
                        .on_hover_text("Project time where the recording's start plays (negative: it started before the project).")
                        .changed()
                    {
                        let us = (s * 1e6).round() as i64;
                        edits.push(Box::new(move |x| x.offset_us = us));
                    }
                    if ui.add_enabled(r.offset_us != 0, egui::Button::new("Reset")).clicked() {
                        edits.push(Box::new(|x| x.offset_us = 0));
                    }
                });
                // align: reference time Y at the edit cursor
                ui.horizontal_wrapped(|ui| {
                    ui.label("Align: recording time");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut w.align_at)
                            .range(0.0..=1e6)
                            .speed(0.001)
                            .max_decimals(3)
                            .suffix(" s"),
                    );
                    ui.label(format!("plays at the edit cursor ({})", clock(cursor_s)));
                    if ui
                        .button("Align")
                        .on_hover_text("Click the waveform to pick the recording time.")
                        .clicked()
                    {
                        let us = ((cursor_s - w.align_at) * 1e6).round() as i64;
                        let us = us.clamp(-MAX_REFERENCE_OFFSET_US, MAX_REFERENCE_OFFSET_US);
                        edits.push(Box::new(move |x| x.offset_us = us));
                    }
                });
                // waveform overview
                if let Some(a) = &audio {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), Sense::click());
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
                    let col = ui.visuals().widgets.inactive.fg_stroke.color;
                    let n = a.peaks.len().max(1);
                    let mid = rect.center().y;
                    let h = rect.height() / 2.0 - 2.0;
                    let cols = rect.width().max(1.0) as usize;
                    for x in 0..cols {
                        let (c0, c1) = (x * n / cols, ((x + 1) * n / cols).max(x * n / cols + 1).min(n));
                        let (lo, hi) = a.peaks[c0..c1]
                            .iter()
                            .fold((0i16, 0i16), |(lo, hi), q| (lo.min(q[0]), hi.max(q[1])));
                        let fx = rect.left() + x as f32 + 0.5;
                        p.line_segment(
                            [
                                egui::pos2(fx, mid - f32::from(hi) / 32768.0 * h),
                                egui::pos2(fx, mid - f32::from(lo) / 32768.0 * h - 0.5),
                            ],
                            Stroke::new(1.0, col),
                        );
                    }
                    let dur = a.duration_seconds().max(1e-9);
                    let at = |t: f64| rect.left() + (t / dur).clamp(0.0, 1.0) as f32 * rect.width();
                    let off = r.offset_seconds();
                    // the edit cursor and the playhead, in recording time
                    for (t, c) in [
                        (cursor_s - off, Color32::from_rgb(90, 160, 255)),
                        (play_s - off, Color32::from_rgb(255, 110, 90)),
                    ] {
                        if (0.0..=dur).contains(&t) {
                            p.vline(at(t), rect.y_range(), Stroke::new(1.5, c));
                        }
                    }
                    p.vline(at(w.align_at), rect.y_range(), Stroke::new(1.0, Color32::from_rgb(120, 200, 120)));
                    if resp.clicked()
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        w.align_at = f64::from((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * dur;
                    }
                    resp.on_hover_text("Blue: the edit cursor · red: the playhead · green: the time to align (click to pick it).");
                }
                ui.label(
                    RichText::new("Plays with the project in time with every tempo change, loop and speed; never part of the exported MIDI.")
                        .weak()
                        .small(),
                );
            });
        if !open {
            self.reference_win.open = false;
        }
        if stop_loading && let Some(l) = self.reference_win.loading.take() {
            l.cancel.store(true, Ordering::Relaxed);
            let key = l.key.clone();
            self.reference_win.problem = Some((key, "Stopped.".into(), false));
        }
        for e in edits {
            self.edit_reference(e);
        }
        if remove && let Some(d) = self.doc_mut() {
            d.project_mut().reference = None;
        }
        if load {
            self.request_reference_path(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_read_like_a_player() {
        assert_eq!(clock(0.0), "0:00.0");
        assert_eq!(clock(83.25), "1:23.2");
        assert_eq!(clock(3725.5), "1:02:05.5");
        assert_eq!(clock(-1.25), "−0:01.2");
    }

    #[test]
    fn a_moved_project_finds_its_recording_beside_it() {
        let dir = std::env::temp_dir().join(format!("ps-ref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("take 1.wav");
        std::fs::write(&file, b"x").unwrap();
        let project = dir.join("song.lup");
        assert_eq!(resolve(file.to_str().unwrap(), None), Some(file.clone()));
        assert_eq!(
            resolve("C:\\Old Folder\\take 1.wav", Some(&project)),
            Some(dir.join("take 1.wav"))
        );
        assert_eq!(resolve("/nowhere/else.wav", Some(&project)), None);
        assert!(is_audio_file(Path::new("a.MP3")) && !is_audio_file(Path::new("a.mid")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
