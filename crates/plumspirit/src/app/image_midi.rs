//! The Image to MIDI window (v0.8 built-in plugin; engine:
//! `plumspirit_engine::transform::image_midi`). Opening a picture decodes it
//! on a worker (bounded: size checked before decoding); every settings change
//! recomputes the processed picture, the palette and the notes on a worker
//! (generation ids: an older result never replaces a newer one). The result
//! is previewed on the active track and pasted with the shared paste options
//! as one undo step.

use super::paste_ui::{self, PasteOptions};
use super::{PlumSpiritApp, StatusKind};
use egui::{RichText, Ui};
use plumspirit_engine::transform::TNote;
use plumspirit_engine::transform::image_midi::{
    self as im, ImageTransform, ORIENTATIONS, Orientation, Rgba, SPLITS, Settings, SplitFrom,
};
use plumspirit_engine::transform::paste::{self, Generated, PastePlan};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// Most notes one conversion may make.
const NOTE_LIMIT: usize = 20_000_000;
/// Largest picture file read.
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

const PALETTE_MODES: &[&str] = &[
    "Automatic (median cut)",
    "Automatic, refined (Clusterise)",
    "My own colors",
];

/// Decodes a picture with bounded memory (dimensions refused before the
/// pixels are allocated).
pub fn decode(path: &Path) -> Result<Rgba, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("The picture could not be read: {e}."))?
        .len();
    if len > MAX_FILE_BYTES {
        return Err("The picture file is larger than 512 MB.".into());
    }
    let file =
        std::fs::File::open(path).map_err(|e| format!("The picture could not be opened: {e}."))?;
    crate::image_decode::decode(
        std::io::BufReader::new(file),
        image::ImageFormat::from_path(path).ok(),
        crate::image_decode::BUILTIN,
        |_| Ok(()),
        || Ok(()),
    )
    .map(|decoded| decoded.rgba)
}

/// What a conversion produced.
pub struct Converted {
    pub size: (u32, u32),
    pub thumb: egui::ColorImage,
    pub palette: Vec<[u8; 3]>,
    pub art: Arc<Generated>,
    pub counts: Vec<u64>,
    pub took: Duration,
}

/// The inputs of one conversion.
#[derive(Clone, Debug, PartialEq)]
struct Inputs {
    settings: Settings,
    palette_mode: usize,
    palette_size: usize,
    seed: u64,
}

fn run_conversion(src: &Rgba, inp: &Inputs) -> Result<Converted, String> {
    let started = Instant::now();
    let mut s = inp.settings.clone();
    let img = im::prepare(src, &s)?;
    if inp.palette_mode < 2 {
        let mut p = im::median_cut(&img, inp.palette_size, s.alpha_min);
        if inp.palette_mode == 1 {
            p = im::clusterise(&img, &p, s.alpha_min, inp.seed);
        }
        s.ignored.resize(p.len(), false);
        s.palette = p;
    }
    if s.palette.is_empty() {
        return Err("The picture has no opaque pixels (or the palette is empty).".into());
    }
    let art = im::convert(&img, &s, NOTE_LIMIT)?;
    let counts = art.counts();
    // Thumbnail: the palette-mapped picture, at most 512 pixels wide.
    let idx = im::index_pixels(&img, &s);
    let scale = (f64::from(img.w) / 512.0)
        .max(f64::from(img.h) / 512.0)
        .max(1.0);
    let (tw, th) = (
        ((f64::from(img.w) / scale) as usize).max(1),
        ((f64::from(img.h) / scale) as usize).max(1),
    );
    let mut thumb = egui::ColorImage::new([tw, th], vec![egui::Color32::TRANSPARENT; tw * th]);
    for y in 0..th {
        for x in 0..tw {
            let sx = ((x as f64 * scale) as u32).min(img.w - 1);
            let sy = ((y as f64 * scale) as u32).min(img.h - 1);
            let k = idx[sy as usize * img.w as usize + sx as usize];
            if k != u32::MAX {
                let c = im::display_color(&s, k as usize);
                thumb.pixels[y * tw + x] = egui::Color32::from_rgb(c[0], c[1], c[2]);
            }
        }
    }
    Ok(Converted {
        size: (img.w, img.h),
        thumb,
        palette: s.palette,
        art: Arc::new(art),
        counts,
        took: started.elapsed(),
    })
}

type Job<T> = (u64, Receiver<(u64, Result<T, String>)>);

/// The Image to MIDI window state.
pub struct ImageMidi {
    pub open: bool,
    /// A picture chosen in a dialog, decoded next frame.
    pending: Option<PathBuf>,
    path: Option<PathBuf>,
    source: Option<Arc<Rgba>>,
    decoding: Option<Job<Rgba>>,
    settings: Settings,
    palette_mode: usize,
    palette_size: usize,
    seed: u64,
    keep_aspect: bool,
    time_pixels: u32,
    random_colors: bool,
    split: usize,
    orientation: usize,
    scale: (f64, f64),
    pub paste: PasteOptions,
    generation: u64,
    computing: Option<Job<Converted>>,
    inputs: Option<Inputs>,
    pub result: Option<Converted>,
    texture: Option<egui::TextureHandle>,
    plan_key: String,
    pub plan: Option<PastePlan>,
    pub preview: Vec<TNote>,
    pub show_preview: bool,
    error: Option<String>,
}

impl Default for ImageMidi {
    fn default() -> Self {
        ImageMidi {
            open: false,
            pending: None,
            path: None,
            source: None,
            decoding: None,
            settings: Settings::default(),
            palette_mode: 0,
            palette_size: 16,
            seed: 1,
            keep_aspect: true,
            time_pixels: 512,
            random_colors: false,
            split: 1,
            orientation: 0,
            scale: (100.0, 100.0),
            paste: PasteOptions::default(),
            generation: 0,
            computing: None,
            inputs: None,
            result: None,
            texture: None,
            plan_key: String::new(),
            plan: None,
            preview: Vec::new(),
            show_preview: true,
            error: None,
        }
    }
}

impl ImageMidi {
    /// v0.8 §069: the window's settings for `settings.json` (not the picture).
    pub fn memory(&self) -> serde_json::Value {
        let s = &self.settings;
        let palette: Vec<String> = s
            .palette
            .iter()
            .take(256)
            .map(|c| format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
            .collect();
        let ignored: Vec<bool> = s.ignored.iter().take(256).copied().collect();
        serde_json::json!({
            "first_key": s.first_key,
            "last_key": s.last_key,
            "ticks_per_pixel": s.ticks_per_pixel,
            "velocity": s.velocity,
            "alpha_min": s.alpha_min,
            "max_len": s.max_len,
            "pixel_notes": s.pixel_notes,
            "rotate": s.transform.rotate,
            "flip_x": s.transform.flip_x,
            "flip_y": s.transform.flip_y,
            "palette": palette,
            "ignored": ignored,
            "palette_mode": self.palette_mode,
            "palette_size": self.palette_size,
            "seed": self.seed,
            "keep_aspect": self.keep_aspect,
            "time_pixels": self.time_pixels,
            "random_colors": self.random_colors,
            "split": self.split,
            "orientation": self.orientation,
            "scale_x": self.scale.0,
            "scale_y": self.scale.1,
            "paste": super::plugin_memory::paste_json(&self.paste),
        })
    }

    /// Reads [`ImageMidi::memory`] back (invalid values keep their defaults).
    pub fn restore(&mut self, v: &serde_json::Value) {
        use super::plugin_memory::{flag, int, num, paste_restore};
        let Some(o) = v.as_object() else {
            return;
        };
        let s = &mut self.settings;
        // 0–255: a 256-Key Mode project's keys (fitted to the open project's
        // keyboard when the window shows).
        if let (Some(a), Some(b)) = (int(o, "first_key", 0, 255), int(o, "last_key", 0, 255))
            && a <= b
        {
            s.first_key = a as u8;
            s.last_key = b as u8;
        }
        if let Some(i) = int(o, "ticks_per_pixel", 1, 1_000_000) {
            s.ticks_per_pixel = i as u64;
        }
        if let Some(i) = int(o, "velocity", 1, 127) {
            s.velocity = i as u8;
        }
        if let Some(i) = int(o, "alpha_min", 0, 255) {
            s.alpha_min = i as u8;
        }
        if let Some(i) = int(o, "max_len", 0, 1_000_000) {
            s.max_len = i as u32;
        }
        if let Some(b) = flag(o, "pixel_notes") {
            s.pixel_notes = b;
        }
        if let Some(f) = num(o, "rotate", -360.0, 360.0) {
            s.transform.rotate = f;
        }
        if let Some(b) = flag(o, "flip_x") {
            s.transform.flip_x = b;
        }
        if let Some(b) = flag(o, "flip_y") {
            s.transform.flip_y = b;
        }
        if let Some(list) = o.get("palette").and_then(|p| p.as_array()) {
            let colours: Option<Vec<[u8; 3]>> = list
                .iter()
                .take(256)
                .map(|c| {
                    let t = c.as_str()?;
                    let n = u32::from_str_radix(t, 16).ok().filter(|_| t.len() == 6)?;
                    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
                })
                .collect();
            if let Some(c) = colours {
                let ignored: Vec<bool> = o
                    .get("ignored")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().map(|b| b.as_bool().unwrap_or(false)).collect())
                    .unwrap_or_default();
                s.ignored = (0..c.len())
                    .map(|i| ignored.get(i).copied().unwrap_or(false))
                    .collect();
                s.palette = c;
            }
        }
        if let Some(i) = int(o, "palette_mode", 0, PALETTE_MODES.len() as i64 - 1) {
            self.palette_mode = i as usize;
        }
        if let Some(i) = int(o, "palette_size", 1, 256) {
            self.palette_size = i as usize;
        }
        if let Some(i) = o.get("seed").and_then(|x| x.as_u64()) {
            self.seed = i;
        }
        if let Some(b) = flag(o, "keep_aspect") {
            self.keep_aspect = b;
        }
        if let Some(i) = int(o, "time_pixels", 1, i64::from(im::MAX_TIME_PIXELS)) {
            self.time_pixels = i as u32;
        }
        if let Some(b) = flag(o, "random_colors") {
            self.random_colors = b;
        }
        if let Some(i) = int(o, "split", 0, 1) {
            self.split = i as usize;
        }
        if let Some(i) = int(o, "orientation", 0, 1) {
            self.orientation = i as usize;
        }
        if let (Some(x), Some(y)) = (
            num(o, "scale_x", -1000.0, 1000.0),
            num(o, "scale_y", -1000.0, 1000.0),
        ) && x != 0.0
            && y != 0.0
        {
            self.scale = (x, y);
        }
        paste_restore(&mut self.paste, o.get("paste"));
    }

    /// Sets the project's keyboard; first / last keys above its top key move
    /// down to it.
    pub fn fit_keys(&mut self, keys: plumspirit_engine::model::project::KeyRange) {
        let s = &mut self.settings;
        s.keys = keys;
        let top = keys.max_pitch();
        s.last_key = s.last_key.min(top);
        s.first_key = s.first_key.min(s.last_key);
    }

    fn inputs(&self) -> Inputs {
        let mut s = self.settings.clone();
        s.time_pixels = (!self.keep_aspect).then_some(self.time_pixels.max(1));
        s.split = if self.split == 0 {
            SplitFrom::ImageStart
        } else {
            SplitFrom::NoteStart
        };
        s.orientation = if self.orientation == 1 {
            Orientation::Upstream
        } else {
            Orientation::PianoRoll
        };
        s.random_colors = self.random_colors.then_some(self.seed);
        s.transform.scale_x = self.scale.0 / 100.0;
        s.transform.scale_y = self.scale.1 / 100.0;
        if self.palette_mode < 2 {
            s.palette.clear();
        }
        Inputs {
            settings: s,
            palette_mode: self.palette_mode,
            palette_size: self.palette_size,
            seed: self.seed,
        }
    }

    /// Starts decoding `path` on a worker.
    fn load(&mut self, path: &Path, ctx: &egui::Context) {
        self.path = Some(path.to_owned());
        self.source = None;
        self.result = None;
        self.texture = None;
        self.inputs = None;
        self.error = None;
        self.generation += 1;
        let g = self.generation;
        let (tx, rx) = channel();
        let p = path.to_owned();
        let c = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("plumspirit-image-decode".into())
            .spawn(move || {
                let _ = tx.send((g, decode(&p)));
                c.request_repaint();
            });
        match spawned {
            Ok(_) => self.decoding = Some((g, rx)),
            Err(e) => self.error = Some(format!("The picture could not be opened: {e}.")),
        }
    }

    /// Polls workers and starts a conversion when the inputs changed.
    fn update(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.pending.take() {
            self.load(&p, ctx);
        }
        if let Some((g, rx)) = &self.decoding {
            if let Ok((rg, r)) = rx.try_recv() {
                if rg == *g {
                    match r {
                        Ok(img) => self.source = Some(Arc::new(img)),
                        Err(e) => self.error = Some(e),
                    }
                }
                self.decoding = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        if let Some((g, rx)) = &self.computing {
            if let Ok((rg, r)) = rx.try_recv() {
                if rg == *g {
                    match r {
                        Ok(c) => {
                            // My own colours start from the automatic ones.
                            if self.palette_mode < 2 {
                                self.settings.palette = c.palette.clone();
                            }
                            self.settings.ignored.resize(c.palette.len(), false);
                            self.texture = Some(ctx.load_texture(
                                "plumspirit-image-midi",
                                c.thumb.clone(),
                                egui::TextureOptions::NEAREST,
                            ));
                            self.result = Some(c);
                            self.error = None;
                        }
                        Err(e) => {
                            self.result = None;
                            self.error = Some(e);
                        }
                    }
                    self.plan_key.clear();
                }
                self.computing = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        let Some(src) = self.source.clone() else {
            return;
        };
        let inp = self.inputs();
        if self.inputs.as_ref() == Some(&inp) {
            return;
        }
        self.inputs = Some(inp.clone());
        self.generation += 1;
        let g = self.generation;
        let (tx, rx) = channel();
        let c = ctx.clone();
        if std::thread::Builder::new()
            .name("plumspirit-image-midi".into())
            .spawn(move || {
                let _ = tx.send((g, run_conversion(&src, &inp)));
                c.request_repaint();
            })
            .is_ok()
        {
            self.computing = Some((g, rx));
        }
    }

    fn settings_ui(&mut self, ui: &mut Ui) {
        let s = &mut self.settings;
        egui::CollapsingHeader::new("Picture")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("im-orient")
                        .selected_text(ORIENTATIONS[self.orientation])
                        .show_ui(ui, |ui| {
                            for (i, o) in ORIENTATIONS.iter().enumerate() {
                                ui.selectable_value(&mut self.orientation, i, *o);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Keys from");
                    let top = s.keys.max_pitch();
                    ui.add(crate::ui::stepper::Stepper::new(&mut s.first_key).range(0..=top));
                    ui.label("to");
                    ui.add(crate::ui::stepper::Stepper::new(&mut s.last_key).range(0..=top));
                })
                .response
                .on_hover_text("The picture's height (or width in the original orientation) is fitted to these keys: one key per pixel.");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.keep_aspect, "Keep the aspect ratio");
                    if !self.keep_aspect {
                        ui.label("Pixels in time");
                        ui.add(
                            crate::ui::stepper::Stepper::new(&mut self.time_pixels)
                                .range(1..=im::MAX_TIME_PIXELS),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Ticks per pixel");
                    ui.add(crate::ui::stepper::Stepper::new(&mut s.ticks_per_pixel).range(1..=1_000_000));
                    ui.label("Velocity");
                    ui.add(crate::ui::stepper::Stepper::new(&mut s.velocity).range(1..=127));
                });
                ui.horizontal(|ui| {
                    ui.label("Transparent below alpha");
                    ui.add(crate::ui::stepper::Stepper::new(&mut s.alpha_min).range(0..=255));
                });
            });
        egui::CollapsingHeader::new("Rotate, flip, scale")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Rotate");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut s.transform.rotate)
                            .range(-360.0..=360.0)
                            .suffix("°")
                            .speed(0.5),
                    );
                    ui.checkbox(&mut s.transform.flip_x, "Flip X");
                    ui.checkbox(&mut s.transform.flip_y, "Flip Y");
                });
                ui.horizontal(|ui| {
                    ui.label("Scale X");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut self.scale.0)
                            .range(-1000.0..=1000.0)
                            .suffix(" %"),
                    );
                    ui.label("Scale Y");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut self.scale.1)
                            .range(-1000.0..=1000.0)
                            .suffix(" %"),
                    );
                })
                .response
                .on_hover_text("Negative values mirror the picture.");
                if ui.button("Reset").clicked() {
                    s.transform = ImageTransform::default();
                    self.scale = (100.0, 100.0);
                }
            });
        egui::CollapsingHeader::new("Colors")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("im-pal")
                        .selected_text(PALETTE_MODES[self.palette_mode])
                        .show_ui(ui, |ui| {
                            for (i, o) in PALETTE_MODES.iter().enumerate() {
                                ui.selectable_value(&mut self.palette_mode, i, *o);
                            }
                        });
                    if self.palette_mode < 2 {
                        ui.label("Colors");
                        ui.add(
                            crate::ui::stepper::Stepper::new(&mut self.palette_size).range(1..=256),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.random_colors, "Random display colors");
                    ui.label("Seed");
                    ui.add(crate::ui::stepper::Stepper::new(&mut self.seed));
                });
                let editable = self.palette_mode == 2;
                let mut remove = None;
                egui::ScrollArea::vertical()
                    .id_salt("im-palette")
                    .max_height(150.0)
                    .show(ui, |ui| {
                        let counts = self.result.as_ref().map(|r| r.counts.clone());
                        for (k, c) in s.palette.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                if editable {
                                    ui.color_edit_button_srgb(c);
                                } else {
                                    paste_ui::swatch(ui, *c);
                                }
                                let mut ign = s.ignored.get(k).copied().unwrap_or(false);
                                if ui.checkbox(&mut ign, "Background (no notes)").changed() {
                                    if s.ignored.len() <= k {
                                        s.ignored.resize(k + 1, false);
                                    }
                                    s.ignored[k] = ign;
                                }
                                let n =
                                    counts.as_ref().and_then(|v| v.get(k).copied()).unwrap_or(0);
                                ui.label(RichText::new(format!("{n} notes")).weak());
                                if editable && ui.small_button("Remove").clicked() {
                                    remove = Some(k);
                                }
                            });
                        }
                    });
                if let Some(k) = remove {
                    s.palette.remove(k);
                    if k < s.ignored.len() {
                        s.ignored.remove(k);
                    }
                }
                if editable && s.palette.len() < im::MAX_PALETTE && ui.button("Add color").clicked()
                {
                    s.palette.push([255, 255, 255]);
                    s.ignored.push(false);
                }
            });
        egui::CollapsingHeader::new("Notes")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Longest note (pixels, 0 = no limit)");
                    ui.add(crate::ui::stepper::Stepper::new(&mut s.max_len).range(0..=1_000_000));
                });
                if s.max_len > 0 {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("im-split")
                            .selected_text(SPLITS[self.split])
                            .show_ui(ui, |ui| {
                                for (i, o) in SPLITS.iter().enumerate() {
                                    ui.selectable_value(&mut self.split, i, *o);
                                }
                            });
                    });
                }
                ui.checkbox(&mut s.pixel_notes, "One note per pixel (no joining)")
                    .on_hover_text("Normally neighboring pixels of one color form one note. This makes a note of every pixel: many more notes.");
            });
    }
}

impl PlumSpiritApp {
    pub(super) fn open_image_to_midi(&mut self) {
        self.image_midi.open = true;
        // On the roll's axis (V09-S11).
        self.image_midi.paste.at = self.edit_cursor();
    }

    /// The window (once per frame).
    pub(super) fn image_midi_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.image_midi.open {
            return;
        }
        self.image_midi.update(ctx);
        let ti = self.active_track();
        // "At the cursor": the edit cursor on the roll's axis (V09-S11).
        let cursor = self.edit_cursor();
        let skip10 = self.settings.skip_channel_10;
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            self.image_midi.open = false;
            return;
        };
        // v0.9: the picture's keys follow the project's keyboard (up to 255 in
        // 256-Key Mode); keys the project no longer has are pulled in.
        self.image_midi.fit_keys(doc.project().keys);
        let dest = doc.track(ti).map(|t| t.id());
        // The mapping plan (when the result, the options or the track change).
        if let (Some(r), Some(dest)) = (&self.image_midi.result, dest) {
            let spec = self.image_midi.paste.spec(dest, "Image", skip10);
            let t = doc.track(ti).expect("checked");
            let key = format!(
                "{:p}|{:?}|{}|{}|{}",
                Arc::as_ptr(&r.art),
                spec,
                t.subchannels_revision(),
                t.notes.revision(),
                doc.track_count()
            );
            if key != self.image_midi.plan_key {
                let plan = paste::plan(&doc, &r.art, &spec);
                self.image_midi.preview = paste_ui::preview_notes(&r.art, &plan, spec.at);
                self.image_midi.plan = Some(plan);
                self.image_midi.plan_key = key;
            }
        }
        let mut open = true;
        let mut apply = false;
        let mut choose = false;
        let info =
            plumspirit_engine::plugins::info(plumspirit_engine::plugins::PluginId::ImageToMidi);
        egui::Window::new("Image to MIDI — Plugins")
            .id(egui::Id::new("plumspirit-image-midi"))
            .open(&mut open)
            .resizable(true)
            .default_width(400.0)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-440.0, 70.0))
            .show(ctx, |ui| {
                let w = &mut self.image_midi;
                ui.label(RichText::new(info.help).weak());
                ui.label(RichText::new(super::plugins::credit_line(info)).small());
                ui.horizontal(|ui| {
                    if ui.button("Open Picture…").clicked() {
                        choose = true;
                    }
                    match (&w.path, &w.source) {
                        (Some(p), Some(s)) => {
                            ui.label(format!(
                                "{} · {} × {}",
                                p.file_name()
                                    .map(|n| n.to_string_lossy())
                                    .unwrap_or_default(),
                                s.w,
                                s.h
                            ));
                        }
                        (Some(_), None) if w.decoding.is_some() => {
                            ui.spinner();
                            ui.label("Reading the picture…");
                        }
                        _ => {
                            ui.label(RichText::new("No picture yet.").weak());
                        }
                    }
                });
                let screen = ui.ctx().content_rect();
                egui::ScrollArea::vertical()
                    .max_height((screen.height() * 0.62).max(160.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if let Some(t) = &w.texture {
                            let size = t.size_vec2();
                            let scale = (360.0 / size.x).min(160.0 / size.y).min(4.0);
                            ui.image((t.id(), size * scale));
                        }
                        w.settings_ui(ui);
                        egui::CollapsingHeader::new("Paste into the Piano Roll")
                            .default_open(true)
                            .show(ui, |ui| {
                                let art = w.result.as_ref().map(|r| r.art.clone());
                                paste_ui::options_ui(
                                    ui,
                                    "im-paste",
                                    &mut w.paste,
                                    &doc,
                                    ti,
                                    art.as_deref(),
                                    cursor,
                                );
                                if let (Some(plan), Some(a)) = (&w.plan, &art) {
                                    paste_ui::mapping_ui(ui, "im", plan, a);
                                }
                            });
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut w.show_preview, "Preview");
                    if w.computing.is_some() {
                        ui.spinner();
                        ui.label(RichText::new("converting…").weak());
                    }
                });
                if let Some(e) = &w.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                } else if let (Some(r), Some(plan)) = (&w.result, &w.plan) {
                    ui.label(format!(
                        "{} × {} pixels processed in {}. {}",
                        r.size.0,
                        r.size.1,
                        super::tasks::format_task_duration(r.took),
                        plan.summary()
                    ));
                    if let Some(e) = &plan.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                }
                ui.horizontal(|ui| {
                    let can = w
                        .plan
                        .as_ref()
                        .is_some_and(|p| p.error.is_none() && p.notes > 0)
                        && w.computing.is_none();
                    if ui.add_enabled(can, egui::Button::new("Apply")).clicked() {
                        apply = true;
                    }
                    if ui.button("Close").clicked() {
                        w.open = false;
                    }
                });
            });
        if !open {
            self.image_midi.open = false;
        }
        // The document is shared while `doc` lives: an edit needs it unique.
        drop(doc);
        if choose {
            self.request_image_path(frame);
        }
        if apply {
            self.apply_image_midi();
        }
    }

    fn apply_image_midi(&mut self) {
        // Image to MIDI writes a track's own notes (and may add subtracks);
        // an opened clip's own notes are not a destination (V09-S11).
        if matches!(
            self.edit_target(),
            plumspirit_engine::editor::NoteTarget::Content { .. }
        ) {
            self.set_status(
                "Image to MIDI pastes into a track's own notes: select the track in the track panel (instead of the open clip) and apply again.",
                StatusKind::Info,
            );
            return;
        }
        let ti = self.active_track();
        let skip10 = self.settings.skip_channel_10;
        let Some(r) = &self.image_midi.result else {
            return;
        };
        let art = r.art.clone();
        let Some(doc) = self.doc() else {
            return;
        };
        let Some(dest) = doc.track(ti).map(|t| t.id()) else {
            return;
        };
        let spec = self.image_midi.paste.spec(dest, "Image", skip10);
        let plan = paste::plan(doc, &art, &spec);
        let started = Instant::now();
        match paste::build(doc, &art, &spec, &plan, "Image to MIDI") {
            Ok(op) => {
                if self.apply_op(op).is_some() {
                    self.tasks.record(
                        "Image to MIDI",
                        started.elapsed(),
                        super::tasks::TaskOutcome::Done,
                    );
                    self.set_status(
                        format!("Image to MIDI: {}", plan.summary()),
                        StatusKind::Info,
                    );
                    self.image_midi.plan_key.clear();
                }
            }
            Err(e) => self.show_error("Image to MIDI was not applied.", e.user_message()),
        }
    }

    /// Asks for a picture (native dialog, or the in-app prompt).
    pub(super) fn request_image_path(&mut self, frame: &eframe::Frame) {
        use super::dialogs::{Dialog, PromptPurpose};
        use super::files;
        if !files::NATIVE_DIALOGS {
            self.dialog = Some(Dialog::PathPrompt {
                purpose: PromptPurpose::OpenImage,
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
        if let Some(p) = files::pick_open(frame, files::FileKind::Image, dir.as_deref()) {
            self.load_image(&p);
        }
    }

    pub(super) fn load_image(&mut self, path: &Path) {
        self.image_midi.open = true;
        self.image_midi.pending = Some(path.to_owned());
    }
}

/// The Image to MIDI preview on the active track.
pub fn image_preview(w: &ImageMidi) -> Option<crate::piano_roll::ToolPreview<'_>> {
    if !w.open || !w.show_preview || w.preview.is_empty() {
        return None;
    }
    Some(crate::piano_roll::ToolPreview::Run {
        notes: &w.preview,
        extra: &[],
        max_dur: w.preview.iter().map(|n| n.dur).max().unwrap_or(0),
        all: false,
        rev: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::decode;
    use plumspirit_engine::codec::Crc32;

    #[test]
    fn native_conversion_keeps_exact_notes_palette_counts_and_thumbnail_in_both_key_modes_and_orientations()
     {
        use super::*;
        use plumspirit_engine::{model::project::KeyRange, transform::paste::ColorNote};
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let green = [0, 255, 0, 255];
        let clear = [0, 0, 0, 0];
        for keys in [KeyRange::Standard, KeyRange::Extended256] {
            let first = if keys == KeyRange::Standard { 60 } else { 200 };
            for orientation in [Orientation::PianoRoll, Orientation::Upstream] {
                let (w, h, pixels) = if orientation == Orientation::PianoRoll {
                    (4, 2, vec![red, red, clear, blue, blue, blue, blue, green])
                } else {
                    (2, 4, vec![blue, green, clear, blue, red, blue, red, blue])
                };
                let image = Rgba::new(w, h, pixels).unwrap();
                let settings = Settings {
                    keys,
                    first_key: first,
                    last_key: first + 1,
                    orientation,
                    time_pixels: Some(4),
                    ticks_per_pixel: 7,
                    alpha_min: 128,
                    palette: vec![[255, 0, 0], [0, 0, 255], [0, 255, 0]],
                    ignored: vec![false, false, true],
                    velocity: 99,
                    ..Default::default()
                };
                let result = run_conversion(
                    &image,
                    &Inputs {
                        settings,
                        palette_mode: 2,
                        palette_size: 3,
                        seed: 0,
                    },
                )
                .unwrap();
                let (a, b) = if orientation == Orientation::PianoRoll {
                    (first + 1, first)
                } else {
                    (first, first + 1)
                };
                let mut expected = vec![
                    ColorNote {
                        start: 0,
                        dur: 14,
                        pitch: a,
                        vel: 99,
                        color: 0,
                    },
                    ColorNote {
                        start: 21,
                        dur: 7,
                        pitch: a,
                        vel: 99,
                        color: 1,
                    },
                    ColorNote {
                        start: 0,
                        dur: 21,
                        pitch: b,
                        vel: 99,
                        color: 1,
                    },
                ];
                expected.sort_unstable();
                assert_eq!(result.art.notes, expected);
                assert_eq!(result.counts, vec![1, 2, 0]);
                assert_eq!(result.size, (w, h));
                assert_eq!(result.palette, vec![[255, 0, 0], [0, 0, 255], [0, 255, 0]]);
                let expected_pixels = image
                    .px
                    .iter()
                    .map(|p| {
                        if *p == clear || *p == green {
                            egui::Color32::TRANSPARENT
                        } else {
                            egui::Color32::from_rgb(p[0], p[1], p[2])
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(result.thumb.pixels, expected_pixels);
                assert_eq!(result.art.colors[0].name, "#FF0000");
                assert_eq!(result.art.colors[1].name, "#0000FF");
            }
        }
    }

    /// v0.9 256-Key Mode (S21 step 4b): the window takes the project's
    /// keyboard; keys a standard project lacks are pulled down to 127, and
    /// remembered keys up to 255 are restored.
    #[test]
    fn the_picture_keys_follow_the_projects_keyboard() {
        use super::ImageMidi;
        use plumspirit_engine::model::project::KeyRange;
        let mut w = ImageMidi::default();
        w.fit_keys(KeyRange::Extended256);
        w.settings.first_key = 120;
        w.settings.last_key = 240;
        assert_eq!(w.inputs().settings.keys, KeyRange::Extended256);
        let mem = w.memory();
        w.fit_keys(KeyRange::Standard);
        assert_eq!((w.settings.first_key, w.settings.last_key), (120, 127));
        let mut back = ImageMidi::default();
        back.restore(&mem);
        assert_eq!(
            (back.settings.first_key, back.settings.last_key),
            (120, 240)
        );
        back.fit_keys(KeyRange::Standard);
        assert_eq!(
            (back.settings.first_key, back.settings.last_key),
            (120, 127)
        );
    }

    fn tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("plumspirit-img-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// A PNG with only a (valid) IHDR claiming `w` × `h` and one empty IDAT.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut c = Crc32::new();
            c.update(kind);
            c.update(data);
            out.extend_from_slice(&c.finish().to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        chunk(b"IEND", &[]);
        out
    }

    /// §216 / §219: bad pictures are refused with sentences, never a panic;
    /// a header claiming a gigantic picture is refused before its pixels
    /// are allocated; a missing file is a sentence too.
    #[test]
    fn bad_huge_and_missing_pictures_are_refused_with_sentences() {
        let garbage = tmp("garbage.png", b"this is not a picture at all");
        let e = decode(&garbage).unwrap_err();
        assert!(e.starts_with("The picture could not be"), "{e}");
        assert!(e.ends_with('.'), "{e}");
        let truncated = tmp("truncated.png", &png_header(4, 4)[..20]);
        assert!(decode(&truncated).is_err());
        let huge = tmp("huge.png", &png_header(1_000_000, 1_000_000));
        let started = std::time::Instant::now();
        let e = decode(&huge).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{e}");
        assert!(e.starts_with("The picture could not be decoded"), "{e}");
        let missing = std::env::temp_dir().join("plumspirit-no-such-picture.png");
        let e = decode(&missing).unwrap_err();
        assert!(e.starts_with("The picture could not be read"), "{e}");
    }
}
