//! B2-F06: bounded, versioned color presets; parsing and preview never mutate settings.
use crate::ui::theme::{NoteColorMode, NoteColorPrefs, NotePalette};
use serde_json::{Value, json};
use std::{io::Read, path::Path};
pub const MAX_BYTES: usize = 16 * 1024;
const FORMAT: &str = "plumspirit.note-colors";

pub fn encode(p: &NoteColorPrefs) -> Result<Vec<u8>, String> {
    if p.manual.len() > 16 {
        return Err("A note preset can store at most 16 colors.".into());
    }
    serde_json::to_vec_pretty(&json!({"format":FORMAT,"version":1,"mode":p.mode.key(),"seed":p.seed,"count":p.manual.len(),"colors":p.manual.iter().map(|c|crate::settings::hex_rgb(*c)).collect::<Vec<_>>()})).map_err(|_| "Could not encode the Note Color Preset.".to_string())
}
pub fn decode(data: &[u8]) -> Result<NoteColorPrefs, String> {
    if data.len() > MAX_BYTES {
        return Err("The preset exceeds 16 KiB.".into());
    }
    let v: Value =
        serde_json::from_slice(data).map_err(|_| "The preset is not valid JSON.".to_string())?;
    let o = v.as_object().ok_or("The preset must be a JSON object.")?;
    if o.keys()
        .any(|k| !["format", "version", "mode", "seed", "count", "colors"].contains(&k.as_str()))
    {
        return Err("The preset contains an unsupported field.".into());
    }
    if o.get("format").and_then(Value::as_str) != Some(FORMAT)
        || o.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err("This is not a supported version 1 Note Color Preset.".into());
    }
    let mode = o
        .get("mode")
        .and_then(Value::as_str)
        .and_then(NoteColorMode::from_key)
        .ok_or("The preset's note color mode is invalid.")?;
    let seed = o
        .get("seed")
        .and_then(Value::as_u64)
        .ok_or("The preset seed must be an unsigned whole number.")?;
    let colors = o
        .get("colors")
        .and_then(Value::as_array)
        .ok_or("The preset colors must be an array.")?;
    if colors.len() > 16 || o.get("count").and_then(Value::as_u64) != Some(colors.len() as u64) {
        return Err("The preset count must match its 0 to 16 colors.".into());
    }
    let manual = colors
        .iter()
        .map(|v| {
            if let Some(s) = v.as_str() {
                return crate::settings::parse_hex_rgb(s)
                    .ok_or("A preset color is not #RRGGBB.".to_string());
            }
            let a = v
                .as_array()
                .filter(|a| a.len() == 3)
                .ok_or("A preset RGB color must have three bytes.".to_string())?;
            let byte = |i: usize| {
                a[i].as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or("A preset RGB component must be from 0 to 255.".to_string())
            };
            Ok([byte(0)?, byte(1)?, byte(2)?])
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(NoteColorPrefs { mode, seed, manual })
}
pub fn load(path: &Path) -> Result<NoteColorPrefs, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("Could not open the preset: {e}."))?;
    let mut data = Vec::new();
    f.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|e| format!("Could not read the preset: {e}."))?;
    decode(&data)
}
pub fn merge(
    active: &NoteColorPrefs,
    imported: &NoteColorPrefs,
    slots: &[bool; 16],
    mode_seed: bool,
) -> NoteColorPrefs {
    let palette = NotePalette::new(active.clone());
    let source = NotePalette::new(NoteColorPrefs {
        mode: NoteColorMode::Manual,
        ..imported.clone()
    });
    let mut next = active.clone();
    if slots.iter().any(|s| *s) {
        next.manual = (0..16)
            .map(|i| {
                let p = if slots[i] { &source } else { &palette };
                let c = p.entry(i as u32, 127, true).fill;
                [c.r(), c.g(), c.b()]
            })
            .collect();
        next.mode = NoteColorMode::Manual;
    }
    if mode_seed {
        next.mode = imported.mode;
        next.seed = imported.seed;
    }
    next
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Note,
    Track,
}
pub fn track_mode(m: NoteColorMode) -> crate::ui::theme::TrackColorMode {
    use crate::ui::theme::TrackColorMode as T;
    match m {
        NoteColorMode::RandomBySeed => T::Curated,
        NoteColorMode::Manual => T::Manual,
        NoteColorMode::Rainbow16 => T::Rainbow16,
    }
}
pub fn track_adapter(mode: crate::ui::theme::TrackColorMode, colors: &[[u8; 3]]) -> NoteColorPrefs {
    use crate::ui::theme::TrackColorMode as T;
    NoteColorPrefs {
        mode: match mode {
            T::Curated => NoteColorMode::RandomBySeed,
            T::Manual => NoteColorMode::Manual,
            T::Rainbow16 => NoteColorMode::Rainbow16,
        },
        seed: 0,
        manual: colors.to_vec(),
    }
}
impl Kind {
    fn file_name(self) -> &'static str {
        if self == Self::Note {
            "note-colors.json"
        } else {
            "track-colors.json"
        }
    }
    fn label(self) -> &'static str {
        if self == Self::Note { "Note" } else { "Track" }
    }
    fn mode_label(self, p: &NoteColorPrefs) -> &'static str {
        if self == Self::Note {
            p.mode.label()
        } else {
            match track_mode(p.mode) {
                crate::ui::theme::TrackColorMode::Curated => "Curated",
                crate::ui::theme::TrackColorMode::Manual => "Manual",
                crate::ui::theme::TrackColorMode::Rainbow16 => "Rainbow Through 16 Tracks",
            }
        }
    }
    pub fn encode(self, p: &NoteColorPrefs) -> Result<Vec<u8>, String> {
        if self == Self::Note {
            return encode(p);
        }
        let mut v: Value = serde_json::from_slice(&encode(p)?)
            .map_err(|_| "Could not encode the Track Color Preset.")?;
        v["format"] = json!("plumspirit.track-colors");
        v["mode"] = json!(track_mode(p.mode).key());
        v.as_object_mut().unwrap().remove("seed");
        serde_json::to_vec_pretty(&v).map_err(|_| "Could not encode the Track Color Preset.".into())
    }
    pub fn decode(self, data: &[u8]) -> Result<NoteColorPrefs, String> {
        if self == Self::Note {
            return decode(data);
        }
        if data.len() > MAX_BYTES {
            return Err("The preset exceeds 16 KiB.".into());
        }
        let mut v: Value = serde_json::from_slice(data)
            .map_err(|_| "The preset is not valid JSON.".to_string())?;
        let o = v.as_object().ok_or("The preset must be a JSON object.")?;
        if o.keys()
            .any(|k| !["format", "version", "mode", "count", "colors"].contains(&k.as_str()))
            || o.get("format").and_then(Value::as_str) != Some("plumspirit.track-colors")
            || o.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Err("This is not a supported version 1 Track Color Preset.".into());
        }
        let mode = o
            .get("mode")
            .and_then(Value::as_str)
            .and_then(crate::ui::theme::TrackColorMode::from_key)
            .ok_or("The preset's Track color mode is invalid.")?;
        v["format"] = json!(FORMAT);
        v["mode"] = json!(track_adapter(mode, &[]).mode.key());
        v["seed"] = json!(0);
        decode(
            &serde_json::to_vec(&v)
                .map_err(|_| "Could not decode the Track Color Preset.".to_string())?,
        )
    }
    fn load(self, path: &Path) -> Result<NoteColorPrefs, String> {
        if self == Self::Note {
            return load(path);
        }
        let f =
            std::fs::File::open(path).map_err(|e| format!("Could not open the preset: {e}."))?;
        let mut bytes = Vec::new();
        f.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Could not read the preset: {e}."))?;
        self.decode(&bytes)
    }
    fn merge(
        self,
        active: &NoteColorPrefs,
        imported: &NoteColorPrefs,
        slots: &[bool; 16],
        mode_seed: bool,
    ) -> NoteColorPrefs {
        if self == Self::Note {
            return merge(active, imported, slots, mode_seed);
        }
        let mut next = active.clone();
        if slots.iter().any(|b| *b) {
            let a = crate::ui::theme::TrackPalette::new(track_mode(active.mode), 0..16)
                .with_manual(&active.manual);
            let b = crate::ui::theme::TrackPalette::new(track_mode(imported.mode), 0..16)
                .with_manual(&imported.manual);
            next.manual = (0..16)
                .map(|i| {
                    let c = if slots[i] { &b } else { &a };
                    let c = c.color(i as u32, None, true, 255);
                    [c.r(), c.g(), c.b()]
                })
                .collect();
            next.mode = NoteColorMode::Manual;
        }
        if mode_seed {
            next.mode = imported.mode;
        }
        next
    }
    fn preview(self, ui: &mut egui::Ui, p: &NoteColorPrefs, dark: bool) {
        if self == Self::Note {
            super::note_colors::preview(ui, &NotePalette::new(p.clone()), dark);
            return;
        }
        let palette =
            crate::ui::theme::TrackPalette::new(track_mode(p.mode), 0..24).with_manual(&p.manual);
        ui.horizontal_wrapped(|ui| {
            for i in 0..24 {
                let (r, resp) =
                    ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(r, 0.0, palette.color(i, None, dark, 255));
                resp.on_hover_text(format!("Track {}", i + 1));
            }
        });
    }
}

pub struct PresetUi {
    path: String,
    pub pending: Option<NoteColorPrefs>,
    replace: bool,
    slots: [bool; 16],
    mode_seed: bool,
    message: Option<(String, bool)>,
}
impl Default for PresetUi {
    fn default() -> Self {
        Self {
            path: String::new(),
            pending: None,
            replace: true,
            slots: [true; 16],
            mode_seed: false,
            message: None,
        }
    }
}
impl PresetUi {
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        active: &NoteColorPrefs,
        dark: bool,
        kind: Kind,
    ) -> Option<NoteColorPrefs> {
        let mut result = None;
        ui.separator();
        ui.label(egui::RichText::new(format!("{} Color Presets", kind.label())).strong());
        ui.add(
            egui::TextEdit::singleline(&mut self.path)
                .hint_text("Preset file (.json)")
                .desired_width(280.0),
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Import Preset…")
                .on_hover_text("Read and validate a preset, then preview it before applying.")
                .clicked()
            {
                let path = if self.path.trim().is_empty() {
                    super::files::pick_color_preset(false, kind.file_name())
                } else {
                    Some(self.path.clone().into())
                };
                if let Some(path) = path {
                    self.path = path.display().to_string();
                    match kind.load(&path) {
                        Ok(p) => {
                            self.pending = Some(p);
                            self.slots = [true; 16];
                            self.message = None;
                        }
                        Err(e) => {
                            self.pending = None;
                            self.message = Some((e, true));
                        }
                    }
                }
            }
            if ui
                .button("Export Preset…")
                .on_hover_text("Save only this color panel to a versioned JSON file.")
                .clicked()
            {
                let path = super::files::pick_color_preset(true, kind.file_name()).or_else(|| {
                    (!super::files::NATIVE_DIALOGS && !self.path.trim().is_empty())
                        .then(|| self.path.clone().into())
                });
                if let Some(path) = path {
                    self.path = path.display().to_string();
                    self.message = Some(
                        match kind
                            .encode(active)
                            .and_then(|data| std::fs::write(path, data).map_err(|e| e.to_string()))
                        {
                            Ok(()) => (format!("{} Color Preset saved.", kind.label()), false),
                            Err(e) => (format!("Could not save the preset: {e}."), true),
                        },
                    );
                }
            }
            if ui
                .button(format!("Reset {} Preferences", kind.label()))
                .on_hover_text(
                    "Restore this color panel; the other color panel and UI Accent Color are kept.",
                )
                .clicked()
            {
                result = Some(NoteColorPrefs::default());
                self.pending = None;
            }
        });
        if let Some(p) = self.pending.clone() {
            ui.label(format!(
                "Import preview: {} · {} stored colors",
                kind.mode_label(&p),
                p.manual.len()
            ));
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(
                    &mut self.replace,
                    true,
                    format!("Replace {} Preferences", kind.label()),
                );
                ui.radio_value(
                    &mut self.replace,
                    false,
                    "Merge Selected Colors into Manual",
                );
            });
            if !self.replace {
                ui.horizontal_wrapped(|ui| {
                    for (i, on) in self.slots.iter_mut().enumerate() {
                        ui.checkbox(on, (i + 1).to_string());
                    }
                });
                ui.checkbox(
                    &mut self.mode_seed,
                    if kind == Kind::Note {
                        "Use Imported Mode and Seed"
                    } else {
                        "Use Imported Mode"
                    },
                );
            }
            let preview = if self.replace {
                p.clone()
            } else {
                kind.merge(active, &p, &self.slots, self.mode_seed)
            };
            kind.preview(ui, &preview, dark);
            ui.horizontal(|ui| {
                if ui.button("Apply Import").clicked() {
                    result = Some(preview);
                }
                if ui.button("Cancel Import").clicked() {
                    self.pending = None;
                }
            });
            if result.is_some() {
                self.pending = None;
            }
        }
        if let Some((m, error)) = &self.message {
            ui.label(egui::RichText::new(m).small().color(if *error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            }));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn track_presets_round_trip_independently_and_reject_note_files_and_bad_metadata() {
        use crate::ui::theme::TrackColorMode as T;
        for mode in [T::Curated, T::Rainbow16, T::Manual] {
            for n in [0, 1, 16] {
                let p = track_adapter(mode, &vec![[11, 120, 230]; n]);
                let bytes = Kind::Track.encode(&p).unwrap();
                assert_eq!(Kind::Track.decode(&bytes).unwrap(), p);
                assert!(decode(&bytes).is_err());
            }
        }
        assert!(
            Kind::Track
                .decode(&encode(&NoteColorPrefs::default()).unwrap())
                .is_err()
        );
        let good: Value = serde_json::from_slice(
            &Kind::Track
                .encode(&track_adapter(T::Manual, &[[3, 7, 11]]))
                .unwrap(),
        )
        .unwrap();
        for (k, v) in [
            ("version", json!(99)),
            ("mode", json!("random")),
            ("count", json!(2)),
            ("colors", json!([[300, 1, 2]])),
            ("colors", json!(["#QQ0000"])),
            ("seed", json!(0)),
            ("ui_accent_color", json!("#FFFFFF")),
        ] {
            let mut bad = good.clone();
            bad[k] = v;
            assert!(
                Kind::Track
                    .decode(&serde_json::to_vec(&bad).unwrap())
                    .is_err(),
                "{bad}"
            );
        }
        assert!(Kind::Track.decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    }
    #[test]
    fn track_merge_preserves_unchecked_colors_and_note_preferences() {
        use crate::ui::theme::{TrackColorMode as T, TrackPalette};
        let active = track_adapter(T::Curated, &[]);
        let imported = track_adapter(T::Manual, &[[150, 20, 230]; 16]);
        let mut slots = [false; 16];
        slots[3] = true;
        let p = Kind::Track.merge(&active, &imported, &slots, false);
        assert_eq!(track_mode(p.mode), T::Manual);
        let before = TrackPalette::new(T::Curated, 0..16);
        for i in 0..16 {
            let c = before.color(i, None, true, 255);
            assert_eq!(
                p.manual[i as usize],
                if i == 3 {
                    [150, 20, 230]
                } else {
                    [c.r(), c.g(), c.b()]
                }
            );
        }
        assert_eq!(
            Kind::Track.merge(&active, &imported, &[false; 16], false),
            active
        );
    }

    #[test]
    fn every_note_mode_round_trips_exactly_and_malformed_presets_are_rejected() {
        for mode in NoteColorMode::ALL {
            for n in [0, 3, 16] {
                let p = NoteColorPrefs {
                    mode,
                    seed: u64::MAX,
                    manual: vec![[2, 40, 210]; n],
                };
                assert_eq!(decode(&encode(&p).unwrap()).unwrap(), p);
            }
        }
        let good: Value =
            serde_json::from_slice(&encode(&NoteColorPrefs::default()).unwrap()).unwrap();
        for (key, value) in [
            ("version", json!(2)),
            ("format", json!("other")),
            ("mode", json!("bad")),
            ("seed", json!(-1)),
            ("count", json!(16)),
            ("colors", json!(["#GG0000"])),
            ("colors", json!([[256, 0, 0]])),
            ("unexpected", json!(true)),
        ] {
            let mut bad = good.clone();
            if key == "colors" {
                bad["count"] = json!(value.as_array().map_or(0, Vec::len));
            }
            bad[key] = value;
            assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err(), "{bad}");
        }
        assert!(decode(&vec![b' '; MAX_BYTES + 1]).is_err());
        assert!(decode(b"[]").is_err());
        assert!(decode(b"{broken}").is_err());
        let mut rgb = good;
        rgb["colors"] = json!([[0, 128, 255]]);
        rgb["count"] = json!(1);
        assert_eq!(
            decode(&serde_json::to_vec(&rgb).unwrap()).unwrap().manual,
            [[0, 128, 255]]
        );
    }
    #[test]
    fn merging_changes_only_selected_color_slots_and_requested_mode_seed() {
        let active = NoteColorPrefs {
            mode: NoteColorMode::Manual,
            seed: 71,
            manual: vec![[20, 80, 140]; 16],
        };
        let imported = NoteColorPrefs {
            mode: NoteColorMode::Rainbow16,
            seed: 999,
            manual: vec![[220, 40, 70]; 16],
        };
        let mut slots = [false; 16];
        slots[7] = true;
        let m = merge(&active, &imported, &slots, false);
        assert_eq!(m.seed, 71);
        assert_eq!(m.mode, NoteColorMode::Manual);
        for i in 0..16 {
            assert_eq!(
                m.manual[i],
                if i == 7 { [220, 40, 70] } else { [20, 80, 140] }
            );
        }
        assert_eq!(merge(&active, &imported, &[false; 16], false), active);
        assert_eq!(
            merge(&active, &imported, &[false; 16], true).mode,
            NoteColorMode::Rainbow16
        );
    }
    #[test]
    fn oversized_files_are_bounded_and_a_preview_does_not_apply_itself() {
        let path =
            std::env::temp_dir().join(format!("plumspirit-colors-big-{}", std::process::id()));
        std::fs::write(&path, vec![b'x'; MAX_BYTES + 100]).unwrap();
        assert!(load(&path).is_err());
        let ctx = egui::Context::default();
        let active = NoteColorPrefs::default();
        let mut state = PresetUi {
            pending: Some(NoteColorPrefs {
                mode: NoteColorMode::Rainbow16,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut applied = None;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                applied = state.draw(ui, &active, true, Kind::Note);
            });
        });
        output.textures_delta.clear();
        assert!(applied.is_none());
        assert!(state.pending.is_some());
        assert_eq!(active, Default::default());
    }
}
