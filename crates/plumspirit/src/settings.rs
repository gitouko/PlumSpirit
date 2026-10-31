//! Persistent user settings (`settings.json`).
//!
//! Stored: the theme, the last window size, the default SoundFont, the lane
//! layout, Follow Playhead, and (v0.4) the audition speed, the default Gate for
//! new notes, the event-graph tool and curve options, the named custom curve
//! functions and whether the status bar shows CPU / RAM. Project-specific
//! settings live in the project (`.lup`), not here.
//! The file is parsed leniently field by field, so a corrupt or partially invalid file never
//! prevents startup: bad fields fall back to defaults and the problem is logged.
//! Writes go to a temporary file in the same directory which then replaces the old
//! file, so a crash mid-write cannot leave a truncated settings file behind.
//!
//! Location: `%APPDATA%\PlumSpirit\settings.json` on Windows,
//! `$XDG_CONFIG_HOME/PlumSpirit` (or `~/.config/PlumSpirit`) elsewhere. The
//! `PLUMSPIRIT_CONFIG_DIR` environment variable overrides it (used by tests).

use crate::app::curves::{CurvePreset, CurveSettings, IntervalSetting, NoteScope, default_presets};
use crate::app::progressor::{ProgressorSettings, ScopeSetting};
use crate::piano_roll::graph::GraphTool;
use plumspirit_engine::editor::controllers::WritePolicy;
use plumspirit_engine::editor::curves::Bend;
use plumspirit_engine::editor::gate::DefaultGate;
use plumspirit_engine::model::tempo::Speed;
use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const SETTINGS_VERSION: u64 = 1;
const FILE_NAME: &str = "settings.json";

/// Velocity Lane height limits (points).
pub const MIN_LANE_HEIGHT: f32 = 56.0;
pub const MAX_LANE_HEIGHT: f32 = 400.0;
pub const DEFAULT_LANE_HEIGHT: f32 = 110.0;

/// Minimum and maximum window sizes accepted from the settings file (points).
pub const MIN_WINDOW: [f32; 2] = [720.0, 460.0];
const MAX_WINDOW: f32 = 16_384.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSetting {
    Dark,
    Light,
    System,
}

impl ThemeSetting {
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeSetting::Dark => "dark",
            ThemeSetting::Light => "light",
            ThemeSetting::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ThemeSetting::Dark),
            "light" => Some(ThemeSetting::Light),
            "system" => Some(ThemeSetting::System),
            _ => None,
        }
    }
}

/// Audio settings (v0.8 Audio Settings window; application settings, never in
/// `.lup`).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSettings {
    /// `None`: the system default device / its default rate / buffer.
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_frames: Option<u32>,
    /// Pre-Render instead of Realtime, and how far ahead (ms).
    pub prerender: bool,
    pub render_ahead_ms: u32,
    pub max_voices: u32,
    pub voice_policy: crate::audio::synth::VoicePolicy,
    pub audition_voices: u32,
    pub note_ons_per_block: u32,
    pub effects: bool,
    pub gain_db: f32,
    pub limiter: bool,
    /// v1.0-beta1 (B1-0083): playing on after a pause strikes the notes
    /// held at that tick again. Off (default): they stay silent until their
    /// end, so a held note is never heard twice; a seek always re-strikes.
    pub resume_restrike: bool,
    /// v1.0-beta1 (B1-0135): where tracks set to Internal play — the
    /// SoundFont synthesizer, a Windows MIDI device or OmniMIDI KDMAPI.
    pub default_output: crate::audio::midi_out::OutputTarget,
    /// The SoundFont library (paths, in the user's order; at most 64).
    pub soundfonts: Vec<String>,
    /// v1.0-beta2 (B2-F12): playback-only event processing (JSON
    /// `audio.event_processing`; off by default).
    pub processing: plumspirit_engine::playback::EventProcessing,
    /// v1.0-Launch (L-B03): an overloaded MIDI output keeps every event
    /// (playback waits) instead of dropping counted note-ons (JSON
    /// `audio.external_keep_all`; off by default).
    pub external_keep_all: bool,
}

/// PPQN of a new project unless the settings say otherwise (B1-0059).
pub const DEFAULT_NEW_PPQN: u32 = 960;

/// B1-0059: the Zoom X percent a new Piano Roll view may start with (the
/// pixels per tick it gives are clamped to the engine's range per project).
pub const DEFAULT_ZOOM_X_RANGE: (f64, f64) = (0.01, 100_000.0);

/// B1-0059: checks a default Zoom X percent ("150", "150 %").
pub fn parse_default_zoom_x(text: &str) -> Result<f64, String> {
    let p = crate::piano_roll::viewport::parse_percent(text)?;
    let (lo, hi) = DEFAULT_ZOOM_X_RANGE;
    if !(lo..=hi).contains(&p) {
        return Err(format!(
            "The default Zoom X must be between {} and {}.",
            crate::piano_roll::viewport::format_percent(lo),
            crate::piano_roll::viewport::format_percent(hi)
        ));
    }
    Ok(p)
}

/// B1-0062: the global UI scale range, its presets and the level below
/// which the Settings window warns that controls get physically small.
pub const UI_SCALE_RANGE: (u16, u16) = (30, 175);
pub const UI_SCALE_PRESETS: [u16; 5] = [75, 100, 125, 150, 175];
pub const UI_SCALE_SMALL_BELOW: u16 = 60;

/// B1-0062: checks a typed UI scale ("125", "125 %", "125%"): a whole
/// percent from 30 to 175.
pub fn parse_ui_scale(text: &str) -> Result<u16, String> {
    let t = text.trim();
    let t = t.strip_suffix('%').unwrap_or(t).trim();
    let (lo, hi) = UI_SCALE_RANGE;
    match t.parse::<u16>() {
        Ok(n) if (lo..=hi).contains(&n) => Ok(n),
        _ => Err(format!(
            "The UI scale must be a whole percent from {lo} % to {hi} %."
        )),
    }
}

/// B1-0059: checks a typed tile cache budget ("0" = automatic, "96", "96 MB").
pub fn parse_tile_cache_mb(text: &str) -> Result<u32, String> {
    use crate::piano_roll::dense::{MAX_CACHE_MB, MIN_CACHE_MB};
    let t = text.trim();
    let t = t
        .strip_suffix("MB")
        .or_else(|| t.strip_suffix("mb"))
        .unwrap_or(t)
        .trim();
    match t.parse::<u32>() {
        Ok(n) if n == 0 || (MIN_CACHE_MB..=MAX_CACHE_MB).contains(&n) => Ok(n),
        _ => Err(format!(
            "Enter 0 (automatic) or whole MB from {MIN_CACHE_MB} to {MAX_CACHE_MB}."
        )),
    }
}

/// B1-0059: checks a typed prefetch distance in tiles.
pub fn parse_prefetch_tiles(text: &str) -> Result<u8, String> {
    use crate::piano_roll::dense::MAX_PREFETCH_TILES;
    match text.trim().parse::<u8>() {
        Ok(n) if n <= MAX_PREFETCH_TILES => Ok(n),
        _ => Err(format!(
            "Enter a whole number of tiles from 0 to {MAX_PREFETCH_TILES}."
        )),
    }
}

/// B1-0059: `settings.json` names of the Onion layer orders.
/// L-F06: the Onion Skin a new project or an opened MIDI file starts with —
/// the one last chosen in any project (its layer order is
/// `Settings::new_project_onion_order`). A `.lup` keeps its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnionDefault {
    pub mode: plumspirit_engine::model::project::OnionMode,
    pub density: plumspirit_engine::model::project::DensityBasis,
    pub opacity: u8,
    pub per_track: bool,
}

impl Default for OnionDefault {
    fn default() -> Self {
        let c = plumspirit_engine::model::project::OnionConfig::default();
        OnionDefault {
            mode: c.mode,
            density: c.density,
            opacity: c.opacity,
            per_track: c.per_track,
        }
    }
}

const ONION_MODES: [(plumspirit_engine::model::project::OnionMode, &str); 6] = {
    use plumspirit_engine::model::project::OnionMode as M;
    [
        (M::Off, "off"),
        (M::Previous, "previous"),
        (M::Next, "next"),
        (M::PreviousAndNext, "previous_and_next"),
        (M::Specific, "specific"),
        (M::AllOthers, "all_others"),
    ]
};

fn onion_default_json(d: &OnionDefault) -> Value {
    use plumspirit_engine::model::project::DensityBasis;
    json!({
        "mode": ONION_MODES.iter().find(|m| m.0 == d.mode).map_or("off", |m| m.1),
        "density": if d.density == DensityBasis::WholeTrack { "whole_track" } else { "visible_range" },
        "opacity": d.opacity,
        "per_track": d.per_track,
    })
}

/// Lenient: each field falls back on its own.
fn parse_onion_default(v: &Value, problems: &mut Vec<String>) -> OnionDefault {
    use plumspirit_engine::model::project::{DensityBasis, OnionConfig};
    let mut d = OnionDefault::default();
    let Some(o) = v.as_object() else {
        problems.push("onion_default is not an object; using Onion Skin off".into());
        return d;
    };
    if let Some(m) = o.get("mode") {
        match m
            .as_str()
            .and_then(|k| ONION_MODES.iter().find(|x| x.1 == k))
        {
            Some(x) => d.mode = x.0,
            None => problems.push(format!(
                "onion_default.mode has an invalid value ({m}); using Off"
            )),
        }
    }
    match o.get("density").and_then(Value::as_str) {
        None => {}
        Some("whole_track") => d.density = DensityBasis::WholeTrack,
        Some("visible_range") => d.density = DensityBasis::VisibleRange,
        Some(other) => problems.push(format!(
            "onion_default.density has an invalid value ({other}); using Visible Range"
        )),
    }
    if let Some(x) = o.get("opacity") {
        match x.as_u64() {
            Some(n) => d.opacity = OnionConfig::clamp_opacity(n.min(255) as u8),
            None => problems.push(format!("onion_default.opacity has an invalid value ({x})")),
        }
    }
    if let Some(x) = o.get("per_track") {
        match x.as_bool() {
            Some(b) => d.per_track = b,
            None => problems.push(format!(
                "onion_default.per_track has an invalid value ({x})"
            )),
        }
    }
    d
}

fn onion_order_key(o: plumspirit_engine::model::project::OnionOrder) -> &'static str {
    use plumspirit_engine::model::project::OnionOrder as O;
    match o {
        O::Project => "project",
        O::DenseFirst => "dense_first",
        O::SparseFirst => "sparse_first",
    }
}

fn onion_order_from_key(k: &str) -> Option<plumspirit_engine::model::project::OnionOrder> {
    plumspirit_engine::model::project::OnionOrder::ALL
        .into_iter()
        .find(|&o| onion_order_key(o) == k)
}

/// B1-0059: checks a default Zoom Y percent; B1-0090: beyond the roll's
/// row-height limits it is clamped to them (the percent kept is the one in
/// effect).
pub fn parse_default_zoom_y(text: &str) -> Result<f64, String> {
    use crate::piano_roll::viewport::{DEFAULT_ROW_HEIGHT, parse_percent, zoom_y_from_percent};
    let p = parse_percent(text)?;
    let rh = zoom_y_from_percent(p).map_err(|e| e.replace("Zoom Y", "The default Zoom Y"))?;
    let clamped = rh / DEFAULT_ROW_HEIGHT * 100.0;
    Ok(if (clamped - p).abs() <= p * 1e-12 {
        p
    } else {
        clamped
    })
}

/// Most SoundFonts kept in the library list.
pub const MAX_SOUNDFONT_LIST: usize = 64;

impl Default for AudioSettings {
    fn default() -> Self {
        let s = crate::audio::synth::SynthConfig::default();
        AudioSettings {
            device: None,
            sample_rate: None,
            buffer_frames: None,
            prerender: false,
            render_ahead_ms: crate::audio::engine::DEFAULT_AHEAD_MS,
            max_voices: s.max_voices as u32,
            voice_policy: s.voice_policy,
            audition_voices: s.audition_voices as u32,
            note_ons_per_block: s.note_ons_per_block as u32,
            effects: s.effects,
            gain_db: s.gain_db,
            limiter: s.limiter,
            resume_restrike: false,
            default_output: Default::default(),
            soundfonts: Vec::new(),
            processing: s.processing,
            external_keep_all: s.external_keep_all,
        }
    }
}

impl AudioSettings {
    pub fn synth(&self) -> crate::audio::synth::SynthConfig {
        crate::audio::synth::SynthConfig {
            max_voices: self.max_voices as usize,
            voice_policy: self.voice_policy,
            audition_voices: self.audition_voices as usize,
            effects: self.effects,
            gain_db: self.gain_db,
            limiter: self.limiter,
            note_ons_per_block: self.note_ons_per_block as usize,
            processing: self.processing,
            external_keep_all: self.external_keep_all,
        }
        .clamped()
    }

    pub fn output(&self) -> crate::audio::output::OutputConfig {
        crate::audio::output::OutputConfig {
            device: self.device.clone(),
            sample_rate: self.sample_rate,
            buffer_frames: self.buffer_frames,
        }
    }

    /// Adds a SoundFont to the library (moved to the end if present).
    pub fn remember_soundfont(&mut self, path: &str) {
        self.soundfonts.retain(|p| p != path);
        self.soundfonts.push(path.to_owned());
        if self.soundfonts.len() > MAX_SOUNDFONT_LIST {
            self.soundfonts.remove(0);
        }
    }
}

fn parse_audio(v: &Value, a: &mut AudioSettings, problems: &mut Vec<String>) {
    let Some(o) = v.as_object() else {
        problems.push("audio has an invalid value; using the defaults".into());
        return;
    };
    let num = |k: &str, lo: f64, hi: f64, problems: &mut Vec<String>| -> Option<f64> {
        // `null` = "the device's default" (how `audio_json` writes an unset
        // rate / buffer): not a problem (it used to be logged at every start).
        let v = o.get(k).filter(|v| !v.is_null())?;
        match v.as_f64() {
            Some(f) if f.is_finite() && (lo..=hi).contains(&f) => Some(f),
            _ => {
                problems.push(format!(
                    "audio.{k} has an invalid value ({v}); using the default"
                ));
                None
            }
        }
    };
    let flag = |k: &str, problems: &mut Vec<String>| -> Option<bool> {
        match o.get(k)? {
            Value::Bool(b) => Some(*b),
            other => {
                problems.push(format!(
                    "audio.{k} has an invalid value ({other}); using the default"
                ));
                None
            }
        }
    };
    match o.get("device") {
        Some(Value::String(s)) if !s.trim().is_empty() => a.device = Some(s.clone()),
        Some(Value::Null) | None => {}
        Some(other) => problems.push(format!(
            "audio.device has an invalid value ({other}); using the default device"
        )),
    }
    if let Some(f) = num("sample_rate", 8_000.0, 384_000.0, problems) {
        a.sample_rate = Some(f as u32);
    }
    if let Some(f) = num("buffer_frames", 16.0, 65_536.0, problems) {
        a.buffer_frames = Some(f as u32);
    }
    if let Some(b) = flag("prerender", problems) {
        a.prerender = b;
    }
    use crate::audio::{engine as e, synth as y};
    if let Some(f) = num(
        "render_ahead_ms",
        f64::from(e::MIN_AHEAD_MS),
        f64::from(e::MAX_AHEAD_MS),
        problems,
    ) {
        a.render_ahead_ms = f as u32;
    }
    if let Some(f) = num(
        "max_voices",
        y::MIN_VOICES as f64,
        y::MAX_VOICES as f64,
        problems,
    ) {
        a.max_voices = f as u32;
    }
    if let Some(f) = num(
        "audition_voices",
        y::MIN_AUDITION_VOICES as f64,
        y::MAX_AUDITION_VOICES as f64,
        problems,
    ) {
        a.audition_voices = f as u32;
    }
    if let Some(f) = num(
        "note_ons_per_block",
        y::MIN_NOTE_ONS as f64,
        y::MAX_NOTE_ONS as f64,
        problems,
    ) {
        a.note_ons_per_block = f as u32;
    }
    match o.get("voice_policy") {
        None => {}
        Some(Value::String(s)) if y::VoicePolicy::parse(s).is_some() => {
            a.voice_policy = y::VoicePolicy::parse(s).unwrap();
        }
        Some(other) => problems.push(format!(
            "audio.voice_policy has an invalid value ({other}); using the default"
        )),
    }
    if let Some(b) = flag("effects", problems) {
        a.effects = b;
    }
    if let Some(f) = num(
        "gain_db",
        f64::from(y::MIN_GAIN_DB),
        f64::from(y::MAX_GAIN_DB),
        problems,
    ) {
        a.gain_db = f as f32;
    }
    if let Some(b) = flag("limiter", problems) {
        a.limiter = b;
    }
    if let Some(b) = flag("external_keep_all", problems) {
        a.external_keep_all = b;
    }
    if let Some(b) = flag("resume_restrike", problems) {
        a.resume_restrike = b;
    }
    match o.get("default_output") {
        None | Some(Value::Null) => {}
        Some(v) => match v
            .as_str()
            .and_then(crate::audio::midi_out::OutputTarget::from_setting)
        {
            Some(t) => a.default_output = t,
            None => problems.push(format!(
                "audio.default_output has an invalid value ({v}); using the internal synthesizer"
            )),
        },
    }
    // v1.0-beta2 (B2-F12): each option on its own; an invalid one is off.
    match o.get("event_processing") {
        None | Some(Value::Null) => {}
        Some(Value::Object(p)) => {
            match p.get("keys_88") {
                None => {}
                Some(Value::Bool(b)) => a.processing.keys_88 = *b,
                Some(v) => problems.push(format!(
                    "audio.event_processing.keys_88 has an invalid value ({v}); off"
                )),
            }
            match p.get("velocity_band") {
                None | Some(Value::Null) => {}
                Some(v) => {
                    let band = v.as_array().and_then(|b| match b.as_slice() {
                        [lo, hi] => Some((lo.as_u64()?, hi.as_u64()?)),
                        _ => None,
                    });
                    match band {
                        Some((lo, hi))
                            if lo <= 127
                                && hi <= 127
                                && plumspirit_engine::playback::EventProcessing::band_problem(
                                    lo as u8, hi as u8,
                                )
                                .is_none() =>
                        {
                            a.processing.velocity_band = Some((lo as u8, hi as u8));
                        }
                        _ => problems.push(format!(
                            "audio.event_processing.velocity_band has an invalid value ({v}); off"
                        )),
                    }
                }
            }
            // B2-F12B: what playback does with the song's system resets.
            match p.get("resets") {
                None | Some(Value::Null) => {}
                Some(v) => match v
                    .as_str()
                    .and_then(plumspirit_engine::playback::ResetPolicy::from_key)
                {
                    Some(r) => a.processing.resets = r,
                    None => problems.push(format!(
                        "audio.event_processing.resets has an invalid value ({v}); not sent"
                    )),
                },
            }
            // B2-F12D / E: whole nanoseconds of song time (exact round trip);
            // the override above 0, the delay from 0.
            for (key, min, slot) in [
                (
                    "length_override_ns",
                    1u64,
                    &mut a.processing.length_override_ns,
                ),
                ("off_delay_ns", 0u64, &mut a.processing.off_delay_ns),
            ] {
                match p.get(key) {
                    None | Some(Value::Null) => {}
                    Some(v) => match v.as_u64().filter(|&n| n >= min) {
                        Some(n) => *slot = Some(n),
                        None => problems.push(format!(
                            "audio.event_processing.{key} has an invalid value ({v}); off"
                        )),
                    },
                }
            }
        }
        Some(other) => problems.push(format!(
            "audio.event_processing has an invalid value ({other}); off"
        )),
    }
    match o.get("soundfonts") {
        Some(Value::Array(list)) => {
            a.soundfonts = list
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned)
                })
                .take(MAX_SOUNDFONT_LIST)
                .collect();
        }
        None => {}
        Some(other) => problems.push(format!(
            "audio.soundfonts has an invalid value ({other}); ignored"
        )),
    }
}

fn audio_json(a: &AudioSettings) -> Value {
    json!({
        "device": a.device,
        "sample_rate": a.sample_rate,
        "buffer_frames": a.buffer_frames,
        "prerender": a.prerender,
        "render_ahead_ms": a.render_ahead_ms,
        "max_voices": a.max_voices,
        "voice_policy": a.voice_policy.key(),
        "audition_voices": a.audition_voices,
        "note_ons_per_block": a.note_ons_per_block,
        "effects": a.effects,
        "gain_db": a.gain_db,
        "limiter": a.limiter,
        "external_keep_all": a.external_keep_all,
        "resume_restrike": a.resume_restrike,
        "default_output": a.default_output.to_setting(),
        "soundfonts": a.soundfonts,
        "event_processing": {
            "keys_88": a.processing.keys_88,
            "velocity_band": a.processing.velocity_band.map(|(lo, hi)| [lo, hi]),
            "length_override_ns": a.processing.length_override_ns,
            "off_delay_ns": a.processing.off_delay_ns,
            "resets": a.processing.resets.key(),
        },
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub theme: ThemeSetting,
    /// Last inner window size in points, if known and valid.
    pub window_size: Option<[f32; 2]>,
    /// Default SoundFont (used when a project has none, or its own is missing).
    pub soundfont: Option<String>,
    pub velocity_lane_visible: bool,
    pub velocity_lane_height: f32,
    /// Follow Playhead during playback (on by default).
    pub follow_playhead: bool,
    /// B2-F30: the Inspector follows independently, with bounded page turns.
    pub inspector_follow_playhead: bool,
    /// Continuous (smooth) following instead of page turns.
    pub smooth_follow: bool,
    /// v1.0-beta1 (B1-0080): the Edit / Anchor playhead (off by default) and
    /// whether Pause returns the playback playhead to it.
    pub anchor_playhead: bool,
    pub pause_rewinds: bool,
    /// v1.0-beta1 (B1-0059): what the New Project dialog starts with (exact
    /// BPM; PPQN as `timing::parse_ppqn` accepts).
    pub new_project_bpm: plumspirit_engine::model::Bpm,
    pub new_project_ppqn: u32,
    /// B1-0059: the New Project dialog starts with 256-Key Mode on.
    pub new_project_keys256: bool,
    /// B1-0059: Zoom X / Zoom Y (percent) of a new Piano Roll view — a
    /// project created or opened, and View → Reset Piano Roll View.
    pub default_zoom_x: f64,
    pub default_zoom_y: f64,
    /// L-F02 (contract §010): a new view (a project created or opened, Reset
    /// View) fits all keys — 128, or 256 in 256-Key Mode — instead of Zoom
    /// Y; with Lock Fit Range it stays fitted when the height changes.
    pub default_fit_keys: bool,
    /// B1-0059: automatic backups — minutes between them (0 = off), how many
    /// are kept per project, the folder (empty = `Backups` in the settings
    /// folder). `app/autobackup.rs`.
    /// B1-0059: the Onion layer order a new project (New Project, an opened
    /// MIDI file) starts with; a `.lup` keeps its own.
    pub new_project_onion_order: plumspirit_engine::model::project::OnionOrder,
    /// B1-0059: View → Hide Text Overlays (Progressor / Selection Analytics
    /// text over the notes; their own settings are kept).
    pub hide_text_overlays: bool,
    /// B1-0059 (accessibility): the high-contrast look (`theme::apply`).
    pub high_contrast: bool,
    /// B2-F05: None keeps the original theme-specific purple.
    pub ui_accent_color: Option<[u8; 3]>,
    pub track_color_mode: crate::ui::theme::TrackColorMode,
    pub track_color_manual: Vec<[u8; 3]>,
    /// B1-0062: the global UI scale in percent (30 – 175, 100 = the system's
    /// own DPI size): egui's zoom factor on top of the monitor's scale.
    pub ui_scale_percent: u16,
    /// B1-0050: the Help window's text size in percent (80 – 200).
    pub help_text_percent: u16,
    /// B1-0051: the start screen shows the quick-start card (dismissible).
    pub show_orientation: bool,
    /// B1-0059 (Performance): the Piano Roll / Player tile cache in MB (0 =
    /// automatic: 24 MB on PCs with up to 3 GB of RAM, else 48 MB) and the
    /// tiles prefetched beyond the view (`piano_roll/dense.rs`).
    pub tile_cache_mb: u32,
    pub prefetch_tiles: u8,
    /// B1-0059: Rendering Quality — visible notes drawn as exact rectangles
    /// before the dense levels take over (Fast / Balanced / Detailed).
    pub render_quality: crate::piano_roll::dense::RenderQuality,
    /// B1-0060: the user's keybinds over the defaults (changed commands only;
    /// `app/keybinds.rs` `Overrides`, applied by `keybinds::set_overrides`).
    pub keybinds: crate::app::keybinds::Overrides,
    pub autobackup_minutes: u32,
    pub autobackup_keep: u32,
    pub autobackup_folder: String,
    /// v0.9 §217 (V09-S09): the Track List's clip mini notes in their note
    /// colours (subchannel / velocity); off: one colour per clip. On by
    /// default.
    pub display_note_colors: bool,
    /// Audition speed (not the project tempo).
    pub speed: Speed,
    /// Length of newly drawn notes.
    pub default_gate: DefaultGate,
    /// Link With Default Gate: the Gate field edits the default for new notes
    /// while no note is selected.
    pub link_gate: bool,
    pub graph_tool: GraphTool,
    pub curve: CurveSettings,
    /// Named custom curve functions.
    pub curve_presets: Vec<CurvePreset>,
    /// CPU / RAM in the status bar.
    pub show_metrics: bool,
    /// v0.9 (V09-0050): MIDI export writes every status byte ("Maximum
    /// Compatibility") instead of running status. Off by default.
    pub export_explicit_status: bool,
    /// v1.0-Launch L-C04: a MIDI file written from the project's contents
    /// that would hold no controller, program change, channel pressure or
    /// pitch bend gets one Program Change 0 (the General MIDI default) at its
    /// start, so Kiva's Pre-render audio engine does not stop responding on
    /// it (`ExportOptions::player_safe_start`). On by default.
    pub export_player_safe_start: bool,
    /// v1.0-Launch L-S51: a MIDI file written from the project's contents
    /// whose loudest note is below velocity 127 gets one silent loudness
    /// reference so Piano From Above keeps its velocities
    /// (`ExportOptions::loudness_reference`). Off by default.
    pub export_loudness_reference: bool,
    /// Height of Event Graph 2 when shown (points).
    pub graph2_height: f32,
    /// Where the playhead is kept while following, in percent of the roll's
    /// width from the left (0–100; v0.5, default 50).
    pub follow_position: u8,
    /// Batch operations show a live Preview before Apply (v0.5, default on).
    pub preview: bool,
    /// v0.6: the Progressor readout (analytics overlay).
    pub progressor: ProgressorSettings,
    /// v0.9 (V09-S17): the MIDI Player (`docs/PLAYER.md` P10).
    pub player: crate::player::PlayerSettings,
    /// v0.6: piano keys light up in the colours of the notes sounding on them
    /// while playing / paused (default on).
    /// v0.9 (V09-S18): View → MIDI Inspector shown.
    pub inspector: bool,
    pub live_keys: bool,
    /// v0.6: which notes light the keys (when `key_follow_roll` is off).
    pub key_scope: ScopeSetting,
    /// L-G02 (default on): the keys show what the Piano Roll shows — the
    /// active track and its subtracks, plus the Onion Skin ghosts while the
    /// Onion is on; Onion off, no other track colours a key.
    pub key_follow_roll: bool,
    /// v0.6: clicking keys and placing notes plays them (default on).
    pub audition: bool,
    /// v0.6: how long a placed note is auditioned (milliseconds).
    pub audition_ms: u32,
    /// v0.7: Onion Skin ghosts are drawn with outlines (default on).
    pub onion_outlines: bool,
    /// v0.8: ghost outline darkness, 0 % = white … 50 % = the source colour
    /// … 100 % = black (not an opacity).
    pub onion_outline_darkness: u8,
    /// v0.7: new subchannels skip MIDI channel 10 (drums) when numbering.
    pub skip_channel_10: bool,
    /// v0.7: the Progressor shows selection analytics (default on).
    pub selection_analytics: bool,
    /// v1.0-Launch L-E01: the advanced section (the selection charts) is
    /// shown — the readout's caret or View → Selection Analytics Charts.
    pub selection_advanced: bool,
    /// L-E07: the selection charts follow the playhead (page turns) while
    /// it is inside the selection.
    pub selection_follow_playhead: bool,
    /// L-E07: a click / drag on a selection chart moves the playhead (the
    /// note selection is never touched).
    pub selection_chart_seek: bool,
    /// L-E06: the note counter counts every note of the song (on) or only
    /// the selected notes from the selection's start (off).
    pub selection_counter_song: bool,
    /// L-F06: new projects and opened MIDI files start with the Onion Skin
    /// last chosen in any project (`onion_default`, and the layer order);
    /// off: Onion Skin off with `new_project_onion_order`.
    pub onion_remember: bool,
    pub onion_default: OnionDefault,
    /// v0.9 §057: the 88-Key Guide lines (A0 = 21, C8 = 108; default off).
    pub key_guide_88: bool,
    /// v0.9 §064–§067: Note Color Preferences (Random by Seed / Manual),
    /// kept for every project.
    pub note_colors: crate::ui::theme::NoteColorPrefs,
    /// v0.9 §119: Selection Opacity in percent — how strongly selected notes
    /// take the selected-note treatment (0 = like the others, 100 = full;
    /// default 100). Independent of the Onion ghost opacity (§187).
    pub selection_opacity: u8,
    /// B1-0098: how see-through a Track List clip's body is, in percent
    /// transparency (80 = 80 % transparent = 20 % opacity, the default;
    /// 0 = the solid track color). The header bar is always opaque.
    pub clip_body_transparency: u8,
    /// B1-0115 / B1-0116: Scale Y Duplicate Notes and its overlap prevention.
    pub scale_y: ScaleYPrefs,
    /// B1-0117: the General Note Overlap Prevention (off by default).
    pub overlap: OverlapPrefs,
    /// B1-0091: Alt + corner drags rotate about the selection's center
    /// (default) or the corner opposite the dragged one.
    pub rotate_about_center: bool,
    /// B1-0087: the Piano Roll's Y zoom may go out beyond the keys (empty
    /// space below them). Off by default: zooming out stops where every key
    /// fills the height. Before v1.0-beta1 the roll always allowed it (there
    /// was no setting); a settings file without the key gets the default.
    pub y_zoom_beyond_keys: bool,
    /// B1-0089: Lock Fit Range — after Fit 128 / 256 Keys the keys stay
    /// fitted when the Event Graph panels or the window change the roll's
    /// height (on by default).
    pub lock_fit_range: bool,
    /// v0.8: renderer used from the next start (OpenGL default, Vulkan).
    pub renderer: crate::renderer::Backend,
    /// v0.8: which GPU the Vulkan renderer asks for.
    pub gpu_preference: crate::renderer::GpuPreference,
    /// v0.8: Audio Settings.
    pub audio: AudioSettings,
    /// v0.8 §069: built-in plugin settings (tools' last values, Image to
    /// MIDI / Spiderweb window settings); validated when applied
    /// (`app::plugin_memory`).
    pub plugins: Map<String, Value>,
    /// B1-0037 / B1-0059: Lua plugins — run them at all, their folder,
    /// enable on install, and per plugin: enabled, granted capabilities,
    /// origin (`lua_plugins`).
    pub lua_plugins: crate::lua::plugins::PluginPrefs,
}

/// B1-0115 / B1-0116: Scale Y's Duplicate Notes and overlap prevention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleYPrefs {
    /// The scale handles duplicate notes when a drag stretches the rows
    /// (Alt reverses it for one drag). Default off: an ordinary scale is
    /// N notes → N notes (v0.8 §190).
    pub handles_duplicate: bool,
    /// The Duplicate Notes options the handles use and the Scale Y dialog
    /// starts from (Fill added rows / Copy by pitch step, row spacing, step,
    /// copies, fading, subchannels).
    pub fill: plumspirit_engine::transform::geom::Fill,
    /// Scale Y overlap prevention: on by default
    /// ([`plumspirit_engine::editor::SCALE_Y_PREVENT_DEFAULT`]).
    pub prevent: bool,
    pub cleanup: plumspirit_engine::transform::geom::ScaleYCleanup,
}

impl Default for ScaleYPrefs {
    fn default() -> Self {
        ScaleYPrefs {
            handles_duplicate: false,
            fill: Default::default(),
            prevent: plumspirit_engine::editor::SCALE_Y_PREVENT_DEFAULT,
            cleanup: Default::default(),
        }
    }
}

fn scale_y_json(p: &ScaleYPrefs) -> Value {
    use plumspirit_engine::transform::geom::FillMode;
    let (mode, step, copies) = match p.fill.mode {
        FillMode::Rows => ("rows", 1, 0),
        FillMode::Step { step, copies } => ("step", step, copies),
    };
    json!({
        "handles_duplicate": p.handles_duplicate,
        "mode": mode,
        "fill_res": p.fill.res,
        "step": step,
        "copies": copies,
        "fade": p.fill.fade,
        "keep_sub": p.fill.keep_sub,
        "prevent": p.prevent,
        "cleanup": p.cleanup.key(),
    })
}

/// Reads `scale_y`; every field falls back on its own.
fn parse_scale_y(v: &Value, p: &mut ScaleYPrefs, problems: &mut Vec<String>) {
    use plumspirit_engine::transform::geom::{FillMode, MAX_FILL_COPIES, MAX_FILL_STEP};
    let Some(o) = v.as_object() else {
        problems.push(format!(
            "scale_y has an invalid value ({v}); using the defaults"
        ));
        return;
    };
    let mut flag = |key: &str, slot: &mut bool| match o.get(key) {
        Some(Value::Bool(b)) => *slot = *b,
        None => {}
        Some(other) => problems.push(format!(
            "scale_y.{key} has an invalid value ({other}); using the default"
        )),
    };
    flag("handles_duplicate", &mut p.handles_duplicate);
    flag("fade", &mut p.fill.fade);
    flag("keep_sub", &mut p.fill.keep_sub);
    flag("prevent", &mut p.prevent);
    // B2-F28: the Scale Y cleanup mode; unknown values keep the default.
    match o.get("cleanup") {
        None | Some(Value::Null) => {}
        Some(v) => match v
            .as_str()
            .and_then(plumspirit_engine::transform::geom::ScaleYCleanup::from_key)
        {
            Some(m) => p.cleanup = m,
            None => problems.push(format!(
                "scale_y.cleanup has an invalid value ({v}); using exact duplicates"
            )),
        },
    }
    let int = |key: &str, lo: u64, hi: u64, problems: &mut Vec<String>| -> Option<u8> {
        let v = o.get(key)?;
        match v.as_u64() {
            Some(n) if (lo..=hi).contains(&n) => Some(n as u8),
            _ => {
                problems.push(format!(
                    "scale_y.{key} has an invalid value ({v}); using the default"
                ));
                None
            }
        }
    };
    if let Some(r) = int("fill_res", 1, 12, problems) {
        p.fill.res = r;
    }
    let step = int("step", 1, u64::from(MAX_FILL_STEP), problems).unwrap_or(1);
    let copies = int("copies", 0, u64::from(MAX_FILL_COPIES), problems).unwrap_or(0);
    match o.get("mode").and_then(Value::as_str) {
        Some("rows") => p.fill.mode = FillMode::Rows,
        Some("step") => p.fill.mode = FillMode::Step { step, copies },
        None if o.get("mode").is_none() => {}
        _ => problems.push("scale_y.mode has an invalid value; using Fill added rows".into()),
    }
}

/// B1-0117: the General Note Overlap Prevention — off by default; when on,
/// run in the supported creation / manipulation contexts only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct OverlapPrefs {
    pub on: bool,
    /// What counts as an overlap, which notes may go, layers, partners.
    pub general: plumspirit_engine::transform::overlap::General,
}

impl OverlapPrefs {
    /// The prevention to run, when it is on.
    pub fn active(&self) -> Option<plumspirit_engine::transform::overlap::General> {
        self.on.then_some(self.general)
    }
}

/// Settings-file names of the overlap modes (`overlap::Mode::ALL` order).
const OVERLAP_MODES: [&str; 4] = ["exact", "same_start", "trim", "remove"];

fn overlap_json(p: &OverlapPrefs) -> Value {
    use plumspirit_engine::transform::overlap::Mode;
    let mode = Mode::ALL
        .iter()
        .position(|m| *m == p.general.policy.mode)
        .unwrap_or(3);
    json!({
        "on": p.on,
        "mode": OVERLAP_MODES[mode],
        "generated_only": p.general.policy.generated_only,
        "across": p.general.policy.across,
        "existing": p.general.existing,
    })
}

/// Reads `overlap_prevention`; every field falls back on its own.
fn lua_plugins_json(p: &crate::lua::plugins::PluginPrefs) -> Value {
    let mut installed = Map::new();
    for (id, st) in &p.installed {
        installed.insert(
            id.clone(),
            json!({
                "enabled": st.enabled,
                "granted": st.granted,
                "origin": st.origin.key(),
            }),
        );
    }
    json!({
        "run_plugins": p.run_plugins,
        "folder": p.folder.as_ref().map(|f| f.to_string_lossy().into_owned()),
        "enable_on_install": p.enable_on_install,
        "installed": Value::Object(installed),
        "tool_keys": p.tool_keys,
    })
}

/// Reads `lua_plugins`; every field falls back on its own, a damaged entry
/// of one plugin is dropped (it shows as disabled, nothing granted).
fn parse_lua_plugins(
    v: &Value,
    p: &mut crate::lua::plugins::PluginPrefs,
    problems: &mut Vec<String>,
) {
    use crate::lua::package::{Origin, check_id};
    use crate::lua::plugins::PluginState;
    let Some(o) = v.as_object() else {
        problems.push(format!(
            "lua_plugins has an invalid value ({v}); using the defaults"
        ));
        return;
    };
    for (key, slot) in [
        ("run_plugins", &mut p.run_plugins),
        ("enable_on_install", &mut p.enable_on_install),
    ] {
        match o.get(key) {
            Some(Value::Bool(b)) => *slot = *b,
            None => {}
            Some(other) => problems.push(format!(
                "lua_plugins.{key} has an invalid value ({other}); using the default"
            )),
        }
    }
    match o.get("folder") {
        None | Some(Value::Null) => {}
        Some(Value::String(f)) if Path::new(f).is_absolute() => p.folder = Some(PathBuf::from(f)),
        Some(other) => problems.push(format!(
            "lua_plugins.folder is not an absolute folder ({other}); using the default"
        )),
    }
    if let Some(Value::Object(keys)) = o.get("tool_keys") {
        for (tool, chord) in keys {
            match chord.as_str() {
                Some(c) if crate::app::keybinds::parse_chord(c).is_ok() && tool.contains('/') => {
                    p.tool_keys.insert(tool.clone(), c.to_owned());
                }
                _ => problems.push(format!(
                    "lua_plugins.tool_keys.{tool} is not a shortcut; it was dropped"
                )),
            }
        }
    }
    if let Some(Value::Object(inst)) = o.get("installed") {
        for (id, e) in inst {
            if check_id(id).is_err() {
                problems.push(format!(
                    "lua_plugins.installed has an invalid plugin id ({id}); ignored"
                ));
                continue;
            }
            let enabled = e.get("enabled").and_then(Value::as_bool);
            let granted: Option<Vec<String>> =
                e.get("granted").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|c| crate::lua::Capability::parse(c).is_some())
                        .map(str::to_owned)
                        .collect()
                });
            let origin = e
                .get("origin")
                .and_then(Value::as_str)
                .and_then(Origin::parse)
                .unwrap_or(Origin::LocalFolder);
            match (enabled, granted) {
                (Some(enabled), Some(granted)) => {
                    p.installed.insert(
                        id.clone(),
                        PluginState {
                            enabled,
                            granted,
                            origin,
                        },
                    );
                }
                _ => problems.push(format!(
                    "lua_plugins.installed.{id} is damaged; the plugin is disabled until you enable it again"
                )),
            }
        }
    }
}

fn parse_overlap(v: &Value, p: &mut OverlapPrefs, problems: &mut Vec<String>) {
    use plumspirit_engine::transform::overlap::Mode;
    let Some(o) = v.as_object() else {
        problems.push(format!(
            "overlap_prevention has an invalid value ({v}); using the defaults"
        ));
        return;
    };
    let mut flag = |key: &str, slot: &mut bool| match o.get(key) {
        Some(Value::Bool(b)) => *slot = *b,
        None => {}
        Some(other) => problems.push(format!(
            "overlap_prevention.{key} has an invalid value ({other}); using the default"
        )),
    };
    flag("on", &mut p.on);
    flag("generated_only", &mut p.general.policy.generated_only);
    flag("across", &mut p.general.policy.across);
    flag("existing", &mut p.general.existing);
    match o.get("mode") {
        None => {}
        Some(m) => match m
            .as_str()
            .and_then(|t| OVERLAP_MODES.iter().position(|x| *x == t))
        {
            Some(i) => p.general.policy.mode = Mode::ALL[i],
            None => problems.push(format!(
                "overlap_prevention.mode has an invalid value ({m}); using remove"
            )),
        },
    }
}

/// Default length of a note-placement audition.
pub const DEFAULT_AUDITION_MS: u32 = 250;
pub const MIN_AUDITION_MS: u32 = 50;
pub const MAX_AUDITION_MS: u32 = 2000;
/// Choices in View → Placed Note Sound Length.
pub const AUDITION_LENGTHS: [u32; 4] = [100, 250, 400, 800];

/// Default Follow position: the centre of the piano roll.
pub const DEFAULT_FOLLOW_POSITION: u8 = 50;
/// Default Onion outline darkness (percent): darker than the ghost colour.
pub const DEFAULT_OUTLINE_DARKNESS: u8 = 75;
/// B1-0098: a clip body's default transparency — the owner's "80 % transparent"
/// read literally (20 % opacity).
pub const DEFAULT_CLIP_BODY_TRANSPARENCY: u8 = 80;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeSetting::System,
            window_size: None,
            soundfont: None,
            velocity_lane_visible: true,
            velocity_lane_height: DEFAULT_LANE_HEIGHT,
            follow_playhead: true,
            inspector_follow_playhead: true,
            smooth_follow: false,
            anchor_playhead: false,
            pause_rewinds: false,
            new_project_bpm: plumspirit_engine::model::Bpm::DEFAULT,
            new_project_ppqn: DEFAULT_NEW_PPQN,
            new_project_keys256: false,
            default_zoom_x: 100.0,
            default_zoom_y: 100.0,
            default_fit_keys: false,
            new_project_onion_order: Default::default(),
            hide_text_overlays: false,
            high_contrast: false,
            ui_accent_color: None,
            track_color_mode: Default::default(),
            track_color_manual: Vec::new(),
            ui_scale_percent: 100,
            help_text_percent: 100,
            show_orientation: true,
            tile_cache_mb: 0,
            prefetch_tiles: crate::piano_roll::dense::DEFAULT_PREFETCH_TILES,
            render_quality: Default::default(),
            keybinds: Vec::new(),
            autobackup_minutes: crate::app::AUTOBACKUP_DEFAULT_MINUTES,
            autobackup_keep: crate::app::AUTOBACKUP_DEFAULT_KEEP,
            autobackup_folder: String::new(),
            display_note_colors: true,
            speed: Speed::NORMAL,
            default_gate: DefaultGate::Grid,
            link_gate: true,
            graph_tool: GraphTool::Edit,
            curve: CurveSettings::default(),
            curve_presets: default_presets(),
            show_metrics: true,
            export_explicit_status: false,
            export_player_safe_start: true,
            export_loudness_reference: false,
            graph2_height: DEFAULT_LANE_HEIGHT,
            follow_position: DEFAULT_FOLLOW_POSITION,
            preview: true,
            progressor: ProgressorSettings::default(),
            player: crate::player::PlayerSettings::default(),
            inspector: false,
            live_keys: true,
            key_scope: ScopeSetting::Project,
            key_follow_roll: true,
            audition: true,
            audition_ms: DEFAULT_AUDITION_MS,
            onion_outlines: true,
            onion_outline_darkness: DEFAULT_OUTLINE_DARKNESS,
            skip_channel_10: false,
            selection_analytics: true,
            selection_advanced: false,
            selection_follow_playhead: true,
            selection_chart_seek: true,
            selection_counter_song: false,
            onion_remember: true,
            onion_default: OnionDefault::default(),
            key_guide_88: false,
            note_colors: crate::ui::theme::NoteColorPrefs::default(),
            selection_opacity: crate::ui::theme::DEFAULT_SELECTION_OPACITY,
            clip_body_transparency: DEFAULT_CLIP_BODY_TRANSPARENCY,
            scale_y: ScaleYPrefs::default(),
            overlap: OverlapPrefs::default(),
            rotate_about_center: true,
            y_zoom_beyond_keys: false,
            lock_fit_range: true,
            renderer: crate::renderer::Backend::OpenGl,
            gpu_preference: crate::renderer::GpuPreference::Automatic,
            audio: AudioSettings::default(),
            plugins: Map::new(),
            lua_plugins: crate::lua::plugins::PluginPrefs::default(),
        }
    }
}

/// Custom curve functions kept at most (names and formulas are short).
pub const MAX_CURVE_PRESETS: usize = 200;

/// Result of loading: the settings plus any problems found (for the log).
pub struct Loaded {
    pub settings: Settings,
    pub problems: Vec<String>,
}

/// Directory holding settings and logs.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PLUMSPIRIT_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("PlumSpirit"))
    }
    #[cfg(not(windows))]
    {
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(x).join("PlumSpirit"));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("PlumSpirit"))
    }
}

fn valid_size(v: &Value) -> Option<[f32; 2]> {
    let arr = v.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    let w = arr[0].as_f64()? as f32;
    let h = arr[1].as_f64()? as f32;
    let ok = |x: f32, min: f32| x.is_finite() && (min..=MAX_WINDOW).contains(&x);
    (ok(w, MIN_WINDOW[0]) && ok(h, MIN_WINDOW[1])).then_some([w, h])
}

/// Parses settings text. Never fails: invalid input yields defaults plus problems.
pub fn parse(text: &str) -> Loaded {
    let mut settings = Settings::default();
    let mut problems = Vec::new();
    let root: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!(
                "settings file is not valid JSON ({e}); using defaults"
            ));
            return Loaded { settings, problems };
        }
    };
    let Some(obj) = root.as_object() else {
        problems.push("settings file does not contain a JSON object; using defaults".into());
        return Loaded { settings, problems };
    };
    match obj.get("version").and_then(Value::as_u64) {
        Some(v) if v > SETTINGS_VERSION => problems.push(format!(
            "settings version {v} is newer than this build supports ({SETTINGS_VERSION}); known fields are still read"
        )),
        Some(_) => {}
        None => problems.push("settings file has no version; assuming 1".into()),
    }
    match obj.get("plugins") {
        None => {}
        Some(Value::Object(m))
            if serde_json::to_string(m)
                .is_ok_and(|t| t.len() <= crate::app::plugin_memory::MAX_BYTES) =>
        {
            settings.plugins = m.clone();
        }
        Some(_) => problems.push("plugins has an invalid value; using the defaults".into()),
    }
    if let Some(v) = obj.get("audio") {
        parse_audio(v, &mut settings.audio, &mut problems);
    }
    if let Some(v) = obj.get("scale_y") {
        parse_scale_y(v, &mut settings.scale_y, &mut problems);
    }
    if let Some(v) = obj.get("overlap_prevention") {
        parse_overlap(v, &mut settings.overlap, &mut problems);
    }
    if let Some(v) = obj.get("lua_plugins") {
        parse_lua_plugins(v, &mut settings.lua_plugins, &mut problems);
    }
    if let Some(v) = obj.get("note_colors") {
        parse_note_colors(v, &mut settings.note_colors, &mut problems);
    }
    match obj.get("renderer") {
        Some(Value::String(s)) => match crate::renderer::Backend::parse(s) {
            Some(b) => settings.renderer = b,
            None => problems.push(format!("unknown renderer \"{s}\"; using OpenGL")),
        },
        Some(other) => problems.push(format!(
            "renderer has an invalid value ({other}); using OpenGL"
        )),
        None => {}
    }
    match obj.get("gpu_preference") {
        Some(Value::String(s)) => match crate::renderer::GpuPreference::parse(s) {
            Some(g) => settings.gpu_preference = g,
            None => problems.push(format!("unknown gpu_preference \"{s}\"; using automatic")),
        },
        Some(other) => problems.push(format!(
            "gpu_preference has an invalid value ({other}); using automatic"
        )),
        None => {}
    }
    match obj.get("theme") {
        Some(Value::String(s)) => match ThemeSetting::parse(s) {
            Some(t) => settings.theme = t,
            None => problems.push(format!("unknown theme \"{s}\"; using system")),
        },
        Some(other) => problems.push(format!(
            "theme has an invalid value ({other}); using system"
        )),
        None => {}
    }
    if let Some(v) = obj.get("window_size") {
        match valid_size(v) {
            Some(s) => settings.window_size = Some(s),
            None => problems.push(format!(
                "window_size {v} is invalid; using the default size"
            )),
        }
    }
    match obj.get("soundfont") {
        Some(Value::String(s)) if !s.trim().is_empty() => settings.soundfont = Some(s.clone()),
        Some(Value::Null) | None => {}
        Some(other) => problems.push(format!("soundfont has an invalid value ({other}); ignored")),
    }
    match obj.get("velocity_lane_visible") {
        Some(Value::Bool(b)) => settings.velocity_lane_visible = *b,
        None => {}
        Some(other) => problems.push(format!(
            "velocity_lane_visible has an invalid value ({other}); using true"
        )),
    }
    for (key, field) in [
        ("follow_playhead", &mut settings.follow_playhead),
        (
            "inspector_follow_playhead",
            &mut settings.inspector_follow_playhead,
        ),
        ("smooth_follow", &mut settings.smooth_follow),
        ("anchor_playhead", &mut settings.anchor_playhead),
        ("pause_rewinds", &mut settings.pause_rewinds),
        ("display_note_colors", &mut settings.display_note_colors),
    ] {
        match obj.get(key) {
            Some(Value::Bool(b)) => *field = *b,
            None => {}
            Some(other) => problems.push(format!(
                "{key} has an invalid value ({other}); using the default"
            )),
        }
    }
    // B1-0059: New Project defaults (each falls back alone).
    match obj.get("new_project_bpm") {
        None => {}
        Some(v) => match v
            .as_str()
            .and_then(|t| plumspirit_engine::model::Bpm::parse(t).ok())
        {
            Some(b) => settings.new_project_bpm = b,
            None => problems.push(format!(
                "new_project_bpm has an invalid value ({v}); using 120"
            )),
        },
    }
    match obj.get("new_project_ppqn") {
        None => {}
        Some(v) => match v
            .as_u64()
            .and_then(|n| plumspirit_engine::model::timing::parse_ppqn(&n.to_string()).ok())
        {
            Some(p) => settings.new_project_ppqn = p,
            None => problems.push(format!(
                "new_project_ppqn has an invalid value ({v}); using {DEFAULT_NEW_PPQN}"
            )),
        },
    }
    match obj.get("new_project_keys256") {
        None => {}
        Some(Value::Bool(b)) => settings.new_project_keys256 = *b,
        Some(v) => problems.push(format!(
            "new_project_keys256 has an invalid value ({v}); using off"
        )),
    }
    for (key, parse, slot) in [
        (
            "default_zoom_x",
            parse_default_zoom_x as fn(&str) -> Result<f64, String>,
            &mut settings.default_zoom_x,
        ),
        (
            "default_zoom_y",
            parse_default_zoom_y,
            &mut settings.default_zoom_y,
        ),
    ] {
        match obj.get(key) {
            None => {}
            Some(v) => match v.as_f64().and_then(|p| parse(&p.to_string()).ok()) {
                Some(p) => *slot = p,
                None => problems.push(format!("{key} has an invalid value ({v}); using 100 %")),
            },
        }
    }
    if let Some(v) = obj.get("onion_default") {
        settings.onion_default = parse_onion_default(v, &mut problems);
    }
    match obj.get("new_project_onion_order") {
        None => {}
        Some(v) => match v.as_str().and_then(onion_order_from_key) {
            Some(o) => settings.new_project_onion_order = o,
            None => problems.push(format!(
                "new_project_onion_order has an invalid value ({v}); using Project Order"
            )),
        },
    }
    match obj.get("high_contrast") {
        None => {}
        Some(Value::Bool(b)) => settings.high_contrast = *b,
        Some(v) => problems.push(format!(
            "high_contrast has an invalid value ({v}); using off"
        )),
    }
    if let Some(v) = obj.get("track_color_manual") {
        let mut colors = crate::ui::theme::NoteColorPrefs::default();
        let mut track_problems = Vec::new();
        parse_note_colors(
            &serde_json::json!({"manual":v}),
            &mut colors,
            &mut track_problems,
        );
        problems.extend(
            track_problems
                .into_iter()
                .map(|m| m.replace("note", "Track")),
        );
        settings.track_color_manual = colors.manual;
    }
    if let Some(v) = obj.get("track_color_mode") {
        match v
            .as_str()
            .and_then(crate::ui::theme::TrackColorMode::from_key)
        {
            Some(m) => settings.track_color_mode = m,
            None => problems.push("Track color mode is invalid; using the curated palette.".into()),
        }
    }
    match obj.get("ui_accent_color") {
        None | Some(Value::Null) => {}
        Some(v) => match v.as_str().and_then(parse_hex_rgb) {
            Some(c) => settings.ui_accent_color = Some(c),
            None => problems.push("UI Accent Color is invalid; using the default purple.".into()),
        },
    }
    match obj.get("ui_scale_percent") {
        None => {}
        Some(v) => match v.as_u64().and_then(|n| parse_ui_scale(&n.to_string()).ok()) {
            Some(p) => settings.ui_scale_percent = p,
            None => problems.push(format!(
                "ui_scale_percent has an invalid value ({v}); using 100 %"
            )),
        },
    }
    match obj.get("help_text_percent") {
        None => {}
        Some(v) => match v.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| {
            (crate::app::help::HELP_TEXT_MIN..=crate::app::help::HELP_TEXT_MAX).contains(p)
        }) {
            Some(p) => settings.help_text_percent = p,
            None => problems.push(format!(
                "help_text_percent has an invalid value ({v}); using 100 %"
            )),
        },
    }
    match obj.get("show_orientation") {
        None => {}
        Some(Value::Bool(b)) => settings.show_orientation = *b,
        Some(v) => problems.push(format!(
            "show_orientation has an invalid value ({v}); showing the quick start"
        )),
    }
    match obj.get("hide_text_overlays") {
        None => {}
        Some(Value::Bool(b)) => settings.hide_text_overlays = *b,
        Some(v) => problems.push(format!(
            "hide_text_overlays has an invalid value ({v}); showing them"
        )),
    }
    match obj.get("tile_cache_mb") {
        None => {}
        Some(v) => match v
            .as_u64()
            .and_then(|n| parse_tile_cache_mb(&n.to_string()).ok())
        {
            Some(n) => settings.tile_cache_mb = n,
            None => problems.push(format!(
                "tile_cache_mb has an invalid value ({v}); using automatic"
            )),
        },
    }
    // B1-0060: each command's keybinds fall back alone (an unknown command or
    // an unreadable chord is left out with a problem; the others apply).
    match obj.get("keybinds") {
        None => {}
        Some(v @ Value::Object(_)) => {
            let (o, p) = crate::app::keybinds::overrides_from_json(v);
            settings.keybinds = o;
            problems.extend(p);
        }
        Some(v) => problems.push(format!(
            "keybinds has an invalid value ({v}); using the default shortcuts"
        )),
    }
    match obj.get("render_quality") {
        None => {}
        Some(v) => match v
            .as_str()
            .and_then(crate::piano_roll::dense::RenderQuality::from_key)
        {
            Some(q) => settings.render_quality = q,
            None => problems.push(format!(
                "render_quality has an invalid value ({v}); using balanced"
            )),
        },
    }
    match obj.get("prefetch_tiles") {
        None => {}
        Some(v) => match v
            .as_u64()
            .and_then(|n| parse_prefetch_tiles(&n.to_string()).ok())
        {
            Some(n) => settings.prefetch_tiles = n,
            None => problems.push(format!(
                "prefetch_tiles has an invalid value ({v}); using {}",
                crate::piano_roll::dense::DEFAULT_PREFETCH_TILES
            )),
        },
    }
    // B1-0059: automatic backups (each falls back alone).
    match obj.get("autobackup_minutes") {
        None => {}
        Some(v) => match v
            .as_u64()
            .and_then(|n| crate::app::parse_autobackup_interval(&n.to_string()).ok())
        {
            Some(m) => settings.autobackup_minutes = m,
            None => problems.push(format!(
                "autobackup_minutes has an invalid value ({v}); using {}",
                crate::app::AUTOBACKUP_DEFAULT_MINUTES
            )),
        },
    }
    match obj.get("autobackup_keep") {
        None => {}
        Some(v) => match v
            .as_u64()
            .and_then(|n| crate::app::parse_autobackup_keep(&n.to_string()).ok())
        {
            Some(n) => settings.autobackup_keep = n,
            None => problems.push(format!(
                "autobackup_keep has an invalid value ({v}); using {}",
                crate::app::AUTOBACKUP_DEFAULT_KEEP
            )),
        },
    }
    match obj.get("autobackup_folder") {
        None => {}
        Some(v) => match v
            .as_str()
            .and_then(|t| crate::app::parse_autobackup_folder(t).ok())
        {
            Some(f) => settings.autobackup_folder = f,
            None => problems.push(format!(
                "autobackup_folder has an invalid value ({v}); using the default folder"
            )),
        },
    }
    match obj.get("velocity_lane_height").and_then(Value::as_f64) {
        Some(h) if h.is_finite() => {
            settings.velocity_lane_height = (h as f32).clamp(MIN_LANE_HEIGHT, MAX_LANE_HEIGHT)
        }
        Some(_) => problems.push("velocity_lane_height is invalid; using the default".into()),
        None => {
            if obj.contains_key("velocity_lane_height") {
                problems.push("velocity_lane_height is invalid; using the default".into());
            }
        }
    }
    parse_v04(obj, &mut settings, &mut problems);
    parse_v05(obj, &mut settings, &mut problems);
    parse_v06(obj, &mut settings, &mut problems);
    Loaded { settings, problems }
}

/// The v0.6 fields (each one falls back on its own).
fn parse_v06(obj: &Map<String, Value>, settings: &mut Settings, problems: &mut Vec<String>) {
    if let Some(v) = obj.get("progressor") {
        settings.progressor = ProgressorSettings::from_json(v, problems);
    }
    if let Some(v) = obj.get("player") {
        settings.player = crate::player::PlayerSettings::from_json(v, problems);
    }
    match obj.get("inspector") {
        Some(Value::Bool(b)) => settings.inspector = *b,
        None => {}
        Some(other) => problems.push(format!(
            "inspector has an invalid value ({other}); using off"
        )),
    }
    for (key, field) in [
        ("live_keys", &mut settings.live_keys),
        ("audition", &mut settings.audition),
        ("key_colors_follow_roll", &mut settings.key_follow_roll),
    ] {
        match obj.get(key) {
            Some(Value::Bool(b)) => *field = *b,
            None => {}
            Some(other) => problems.push(format!("{key} has an invalid value ({other}); using on")),
        }
    }
    match obj.get("key_scope") {
        None => {}
        Some(v) => match v.as_str().and_then(ScopeSetting::parse) {
            Some(s) => settings.key_scope = s,
            None => problems.push(format!(
                "key_scope has an invalid value ({v}); using project"
            )),
        },
    }
    match obj.get("audition_ms") {
        None => {}
        Some(v) => match v.as_u64() {
            Some(ms) => {
                settings.audition_ms =
                    (ms.min(u64::from(u32::MAX)) as u32).clamp(MIN_AUDITION_MS, MAX_AUDITION_MS)
            }
            None => problems.push(format!(
                "audition_ms has an invalid value ({v}); using {DEFAULT_AUDITION_MS}"
            )),
        },
    }
}

/// The v0.5 fields (each one falls back on its own).
fn parse_v05(obj: &Map<String, Value>, settings: &mut Settings, problems: &mut Vec<String>) {
    if let Some(v) = obj.get("onion_outline_darkness") {
        match v.as_f64() {
            Some(f) if f.is_finite() && (0.0..=100.0).contains(&f) => {
                settings.onion_outline_darkness = f.round() as u8
            }
            _ => problems.push(format!(
                "onion_outline_darkness has an invalid value ({v}); using {DEFAULT_OUTLINE_DARKNESS}"
            )),
        }
    }
    if let Some(v) = obj.get("selection_opacity") {
        match v.as_f64() {
            Some(f) if f.is_finite() && (0.0..=100.0).contains(&f) => {
                settings.selection_opacity = f.round() as u8
            }
            _ => problems.push(format!(
                "selection_opacity has an invalid value ({v}); using {}",
                crate::ui::theme::DEFAULT_SELECTION_OPACITY
            )),
        }
    }
    if let Some(v) = obj.get("clip_body_transparency") {
        match v.as_f64() {
            Some(f) if f.is_finite() && (0.0..=100.0).contains(&f) => {
                settings.clip_body_transparency = f.round() as u8
            }
            _ => problems.push(format!(
                "clip_body_transparency has an invalid value ({v}); using {DEFAULT_CLIP_BODY_TRANSPARENCY}"
            )),
        }
    }
    if let Some(v) = obj.get("follow_position") {
        match v.as_f64() {
            Some(f) if f.is_finite() && (0.0..=100.0).contains(&f) => {
                settings.follow_position = f.round() as u8
            }
            _ => problems.push(format!(
                "follow_position has an invalid value ({v}); using {DEFAULT_FOLLOW_POSITION}"
            )),
        }
    }
    match obj.get("preview") {
        Some(Value::Bool(b)) => settings.preview = *b,
        None => {}
        Some(other) => problems.push(format!("preview has an invalid value ({other}); using on")),
    }
    // v0.7 switches and the v0.9 88-Key Guide (each falls back on its own).
    let switches: [(&str, &mut bool); 13] = [
        ("onion_outlines", &mut settings.onion_outlines),
        ("skip_channel_10", &mut settings.skip_channel_10),
        ("selection_analytics", &mut settings.selection_analytics),
        ("selection_advanced", &mut settings.selection_advanced),
        (
            "selection_follow_playhead",
            &mut settings.selection_follow_playhead,
        ),
        ("selection_chart_seek", &mut settings.selection_chart_seek),
        (
            "selection_counter_song",
            &mut settings.selection_counter_song,
        ),
        ("onion_remember", &mut settings.onion_remember),
        ("default_fit_keys", &mut settings.default_fit_keys),
        ("key_guide_88", &mut settings.key_guide_88),
        ("rotate_about_center", &mut settings.rotate_about_center),
        ("y_zoom_beyond_keys", &mut settings.y_zoom_beyond_keys),
        ("lock_fit_range", &mut settings.lock_fit_range),
    ];
    for (key, slot) in switches {
        match obj.get(key) {
            Some(Value::Bool(b)) => *slot = *b,
            None => {}
            Some(other) => problems.push(format!(
                "{key} has an invalid value ({other}); using the default"
            )),
        }
    }
}

/// `#RRGGBB` for application color preferences.
pub fn hex_rgb([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

pub fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

fn note_colors_json(p: &crate::ui::theme::NoteColorPrefs) -> Value {
    json!({
        "mode": p.mode.key(),
        "seed": p.seed,
        "manual": p.manual.iter().map(|c| hex_rgb(*c)).collect::<Vec<_>>(),
    })
}

/// v0.9 Note Color Preferences; each field falls back on its own (a file
/// written before v0.9 has none: PlumSpirit's own colours).
fn parse_note_colors(
    v: &Value,
    p: &mut crate::ui::theme::NoteColorPrefs,
    problems: &mut Vec<String>,
) {
    use crate::ui::theme::{MANUAL_NOTE_COLORS, NoteColorMode};
    let Some(o) = v.as_object() else {
        problems.push("note_colors has an invalid value; using PlumSpirit's colors".into());
        return;
    };
    match o.get("mode") {
        None => {}
        Some(Value::String(m)) if NoteColorMode::from_key(m).is_some() => {
            p.mode = NoteColorMode::from_key(m).unwrap_or_default();
        }
        Some(other) => problems.push(format!(
            "note_colors.mode has an invalid value ({other}); using Random by Seed"
        )),
    }
    match o.get("seed") {
        None => {}
        Some(s) => match s.as_u64() {
            Some(n) => p.seed = n,
            None => problems.push(format!(
                "note_colors.seed has an invalid value ({s}); using 0"
            )),
        },
    }
    match o.get("manual") {
        None => {}
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for it in items {
                match it.as_str().and_then(parse_hex_rgb) {
                    Some(c) if out.len() < MANUAL_NOTE_COLORS => out.push(c),
                    Some(_) => {}
                    None => {
                        problems.push(format!("a Manual note color is invalid ({it}); skipped"))
                    }
                }
            }
            if items.len() > MANUAL_NOTE_COLORS {
                problems.push(format!(
                    "note_colors.manual holds {} colors; the first {MANUAL_NOTE_COLORS} are used",
                    items.len()
                ));
            }
            p.manual = out;
        }
        Some(other) => problems.push(format!(
            "note_colors.manual has an invalid value ({other}); using the defaults"
        )),
    }
}

/// The v0.4 fields (each one falls back on its own).
fn parse_v04(obj: &Map<String, Value>, settings: &mut Settings, problems: &mut Vec<String>) {
    let text = |k: &str| obj.get(k).and_then(Value::as_str);
    let bad = |problems: &mut Vec<String>, k: &str, v: Option<&Value>| {
        if let Some(v) = v {
            problems.push(format!("{k} has an invalid value ({v}); using the default"));
        }
    };
    match text("speed").map(Speed::parse) {
        Some(Ok(s)) => settings.speed = s,
        _ => bad(problems, "speed", obj.get("speed")),
    }
    match text("default_gate").and_then(DefaultGate::from_key) {
        Some(g) => settings.default_gate = g,
        None => bad(problems, "default_gate", obj.get("default_gate")),
    }
    match text("graph_tool").and_then(GraphTool::from_key) {
        Some(t) => settings.graph_tool = t,
        None => bad(problems, "graph_tool", obj.get("graph_tool")),
    }
    for (key, field) in [
        ("link_gate", &mut settings.link_gate),
        ("show_metrics", &mut settings.show_metrics),
        (
            "export_explicit_status",
            &mut settings.export_explicit_status,
        ),
        (
            "export_player_safe_start",
            &mut settings.export_player_safe_start,
        ),
        (
            "export_loudness_reference",
            &mut settings.export_loudness_reference,
        ),
    ] {
        match obj.get(key) {
            Some(Value::Bool(b)) => *field = *b,
            None => {}
            Some(other) => problems.push(format!(
                "{key} has an invalid value ({other}); using the default"
            )),
        }
    }
    match obj.get("graph2_height").and_then(Value::as_f64) {
        Some(h) if h.is_finite() => {
            settings.graph2_height = (h as f32).clamp(MIN_LANE_HEIGHT, MAX_LANE_HEIGHT)
        }
        _ => bad(problems, "graph2_height", obj.get("graph2_height")),
    }
    if let Some(c) = obj.get("curve") {
        match c.as_object() {
            Some(c) => parse_curve(c, &mut settings.curve, problems),
            None => problems.push("curve settings are invalid; using the defaults".into()),
        }
    }
    match obj.get("curve_presets") {
        None => {}
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for it in items.iter().take(MAX_CURVE_PRESETS) {
                match (
                    it.get("name").and_then(Value::as_str),
                    it.get("expr").and_then(Value::as_str),
                ) {
                    (Some(n), Some(e))
                        if !n.trim().is_empty() && n.chars().count() <= 80 && e.len() <= 1000 =>
                    {
                        out.push(CurvePreset {
                            name: n.trim().to_owned(),
                            expr: e.to_owned(),
                        })
                    }
                    _ => problems.push(format!(
                        "a custom curve function is invalid ({it}); skipped"
                    )),
                }
            }
            settings.curve_presets = out;
        }
        Some(other) => problems.push(format!(
            "curve_presets has an invalid value ({other}); using the examples"
        )),
    }
}

fn parse_curve(c: &Map<String, Value>, cs: &mut CurveSettings, problems: &mut Vec<String>) {
    let s = |k: &str| c.get(k).and_then(Value::as_str);
    let b = |k: &str| c.get(k).and_then(Value::as_bool);
    let note = |problems: &mut Vec<String>, k: &str| {
        problems.push(format!("curve.{k} is invalid; using the default"))
    };
    match s("bend") {
        Some("ease_out") => cs.bend = Bend::EaseOut,
        Some("ease_in") => cs.bend = Bend::EaseIn,
        None => {}
        Some(_) => note(problems, "bend"),
    }
    match s("interval").map(IntervalSetting::from_key) {
        Some(Some(i)) => cs.interval = i,
        None => {}
        Some(None) => note(problems, "interval"),
    }
    match s("scope") {
        Some("selected_or_all") => cs.scope = NoteScope::SelectedOrAll,
        Some("all") => cs.scope = NoteScope::All,
        None => {}
        Some(_) => note(problems, "scope"),
    }
    match s("policy") {
        Some("replace") => cs.policy = WritePolicy::Replace,
        Some("merge") => cs.policy = WritePolicy::Merge,
        None => {}
        Some(_) => note(problems, "policy"),
    }
    match c.get("seed").map(Value::as_u64) {
        Some(Some(v)) => cs.seed = v,
        None => {}
        Some(None) => note(problems, "seed"),
    }
    if let Some(n) = s("custom") {
        cs.custom = n.to_owned();
    }
    for (k, f) in [
        ("before_notes", &mut cs.before_notes),
        ("same_channel", &mut cs.same_channel),
        ("advance_seed", &mut cs.advance_seed),
        ("clamp", &mut cs.clamp),
        ("normalize", &mut cs.normalize),
        ("anchors", &mut cs.anchors),
    ] {
        if let Some(v) = b(k) {
            *f = v;
        } else if c.contains_key(k) {
            problems.push(format!("curve.{k} is invalid; using the default"));
        }
    }
}

/// Loads settings from `dir`. A missing file is normal (first launch).
pub fn load(dir: Option<&Path>) -> Loaded {
    let Some(dir) = dir else {
        return Loaded {
            settings: Settings::default(),
            problems: vec![
                "no configuration directory available; settings will not persist".into(),
            ],
        };
    };
    let path = dir.join(FILE_NAME);
    match fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            parse(&text)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First launch: defaults suited to this PC (§258).
            let mut settings = Settings::default();
            machine_defaults(
                &mut settings,
                std::thread::available_parallelism().ok().map(|n| n.get()),
            );
            Loaded {
                settings,
                problems: Vec::new(),
            }
        }
        Err(e) => Loaded {
            settings: Settings::default(),
            problems: vec![format!(
                "could not read {}: {e}; using defaults",
                path.display()
            )],
        },
    }
}

/// Voices on PCs with at most two logical processors (§258).
pub const LOW_END_VOICES: u32 = 256;

/// v0.8 §258: conservative first-launch defaults for weak PCs — on at most two
/// logical processors the synthesizer starts with 256 voices instead of 512
/// (each 256-voice shard costs up to a third of a slow core on dense walls).
/// OpenGL and Realtime are already the defaults everywhere; everything stays
/// selectable (Playback → Audio Settings). An existing settings file is never
/// changed.
pub fn machine_defaults(s: &mut Settings, cpus: Option<usize>) {
    if cpus.is_some_and(|n| n <= 2) {
        s.audio.max_voices = s.audio.max_voices.min(LOW_END_VOICES);
    }
}

pub fn to_json(settings: &Settings) -> String {
    let mut obj = Map::new();
    obj.insert("version".into(), json!(SETTINGS_VERSION));
    obj.insert("theme".into(), json!(settings.theme.as_str()));
    obj.insert("renderer".into(), json!(settings.renderer.as_str()));
    obj.insert("audio".into(), audio_json(&settings.audio));
    obj.insert("scale_y".into(), scale_y_json(&settings.scale_y));
    obj.insert("overlap_prevention".into(), overlap_json(&settings.overlap));
    obj.insert(
        "lua_plugins".into(),
        lua_plugins_json(&settings.lua_plugins),
    );
    if !settings.plugins.is_empty() {
        obj.insert("plugins".into(), Value::Object(settings.plugins.clone()));
    }
    obj.insert(
        "gpu_preference".into(),
        json!(settings.gpu_preference.as_str()),
    );
    if let Some([w, h]) = settings.window_size {
        obj.insert("window_size".into(), json!([w.round(), h.round()]));
    }
    if let Some(sf) = &settings.soundfont {
        obj.insert("soundfont".into(), json!(sf));
    }
    obj.insert(
        "velocity_lane_visible".into(),
        json!(settings.velocity_lane_visible),
    );
    obj.insert(
        "velocity_lane_height".into(),
        json!(settings.velocity_lane_height.round()),
    );
    obj.insert("follow_playhead".into(), json!(settings.follow_playhead));
    obj.insert(
        "inspector_follow_playhead".into(),
        json!(settings.inspector_follow_playhead),
    );
    obj.insert("smooth_follow".into(), json!(settings.smooth_follow));
    obj.insert("anchor_playhead".into(), json!(settings.anchor_playhead));
    obj.insert(
        "new_project_bpm".into(),
        json!(settings.new_project_bpm.to_string()),
    );
    obj.insert("new_project_ppqn".into(), json!(settings.new_project_ppqn));
    obj.insert(
        "new_project_keys256".into(),
        json!(settings.new_project_keys256),
    );
    obj.insert("default_zoom_x".into(), json!(settings.default_zoom_x));
    obj.insert("default_zoom_y".into(), json!(settings.default_zoom_y));
    obj.insert("default_fit_keys".into(), json!(settings.default_fit_keys));
    obj.insert("tile_cache_mb".into(), json!(settings.tile_cache_mb));
    obj.insert("prefetch_tiles".into(), json!(settings.prefetch_tiles));
    obj.insert(
        "render_quality".into(),
        json!(settings.render_quality.key()),
    );
    obj.insert(
        "new_project_onion_order".into(),
        json!(onion_order_key(settings.new_project_onion_order)),
    );
    obj.insert(
        "hide_text_overlays".into(),
        json!(settings.hide_text_overlays),
    );
    obj.insert("high_contrast".into(), json!(settings.high_contrast));
    obj.insert(
        "track_color_manual".into(),
        json!(
            settings
                .track_color_manual
                .iter()
                .map(|c| hex_rgb(*c))
                .collect::<Vec<_>>()
        ),
    );
    obj.insert(
        "track_color_mode".into(),
        json!(settings.track_color_mode.key()),
    );
    obj.insert(
        "ui_accent_color".into(),
        json!(settings.ui_accent_color.map(hex_rgb)),
    );
    obj.insert("ui_scale_percent".into(), json!(settings.ui_scale_percent));
    obj.insert(
        "help_text_percent".into(),
        json!(settings.help_text_percent),
    );
    obj.insert("show_orientation".into(), json!(settings.show_orientation));
    if !settings.keybinds.is_empty() {
        obj.insert(
            "keybinds".into(),
            crate::app::keybinds::overrides_to_json(&settings.keybinds),
        );
    }
    obj.insert(
        "autobackup_minutes".into(),
        json!(settings.autobackup_minutes),
    );
    obj.insert("autobackup_keep".into(), json!(settings.autobackup_keep));
    obj.insert(
        "autobackup_folder".into(),
        json!(settings.autobackup_folder),
    );
    obj.insert("pause_rewinds".into(), json!(settings.pause_rewinds));
    obj.insert(
        "display_note_colors".into(),
        json!(settings.display_note_colors),
    );
    obj.insert("follow_position".into(), json!(settings.follow_position));
    obj.insert(
        "selection_opacity".into(),
        json!(settings.selection_opacity),
    );
    obj.insert(
        "clip_body_transparency".into(),
        json!(settings.clip_body_transparency),
    );
    obj.insert("preview".into(), json!(settings.preview));
    obj.insert("onion_outlines".into(), json!(settings.onion_outlines));
    obj.insert(
        "onion_outline_darkness".into(),
        json!(settings.onion_outline_darkness),
    );
    obj.insert("skip_channel_10".into(), json!(settings.skip_channel_10));
    obj.insert(
        "selection_analytics".into(),
        json!(settings.selection_analytics),
    );
    obj.insert(
        "selection_advanced".into(),
        json!(settings.selection_advanced),
    );
    obj.insert(
        "selection_follow_playhead".into(),
        json!(settings.selection_follow_playhead),
    );
    obj.insert(
        "selection_chart_seek".into(),
        json!(settings.selection_chart_seek),
    );
    obj.insert(
        "selection_counter_song".into(),
        json!(settings.selection_counter_song),
    );
    obj.insert("onion_remember".into(), json!(settings.onion_remember));
    obj.insert(
        "onion_default".into(),
        onion_default_json(&settings.onion_default),
    );
    obj.insert("key_guide_88".into(), json!(settings.key_guide_88));
    obj.insert(
        "rotate_about_center".into(),
        json!(settings.rotate_about_center),
    );
    obj.insert(
        "y_zoom_beyond_keys".into(),
        json!(settings.y_zoom_beyond_keys),
    );
    obj.insert("lock_fit_range".into(), json!(settings.lock_fit_range));
    obj.insert(
        "note_colors".into(),
        note_colors_json(&settings.note_colors),
    );
    crate::app::progressor::insert_json(&mut obj, &settings.progressor);
    obj.insert("player".into(), settings.player.to_json());
    obj.insert("inspector".into(), json!(settings.inspector));
    obj.insert("live_keys".into(), json!(settings.live_keys));
    obj.insert("key_scope".into(), json!(settings.key_scope.key()));
    obj.insert(
        "key_colors_follow_roll".into(),
        json!(settings.key_follow_roll),
    );
    obj.insert("audition".into(), json!(settings.audition));
    obj.insert("audition_ms".into(), json!(settings.audition_ms));
    obj.insert(
        "speed".into(),
        json!(settings.speed.to_string().trim_end_matches('×')),
    );
    obj.insert("default_gate".into(), json!(settings.default_gate.to_key()));
    obj.insert("link_gate".into(), json!(settings.link_gate));
    obj.insert("graph_tool".into(), json!(settings.graph_tool.key()));
    obj.insert("show_metrics".into(), json!(settings.show_metrics));
    obj.insert(
        "export_explicit_status".into(),
        json!(settings.export_explicit_status),
    );
    obj.insert(
        "export_player_safe_start".into(),
        json!(settings.export_player_safe_start),
    );
    obj.insert(
        "export_loudness_reference".into(),
        json!(settings.export_loudness_reference),
    );
    obj.insert(
        "graph2_height".into(),
        json!(settings.graph2_height.round()),
    );
    let c = &settings.curve;
    obj.insert(
        "curve".into(),
        json!({
            "bend": match c.bend { Bend::EaseOut => "ease_out", Bend::EaseIn => "ease_in" },
            "interval": c.interval.key(),
            "before_notes": c.before_notes,
            "scope": match c.scope { NoteScope::SelectedOrAll => "selected_or_all", NoteScope::All => "all" },
            "same_channel": c.same_channel,
            "policy": match c.policy { WritePolicy::Replace => "replace", WritePolicy::Merge => "merge" },
            "seed": c.seed,
            "advance_seed": c.advance_seed,
            "custom": c.custom,
            "clamp": c.clamp,
            "normalize": c.normalize,
            "anchors": c.anchors,
        }),
    );
    obj.insert(
        "curve_presets".into(),
        Value::Array(
            settings
                .curve_presets
                .iter()
                .map(|p| json!({"name": p.name, "expr": p.expr}))
                .collect(),
        ),
    );
    let mut s = serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// Saves settings atomically (temporary file + rename in the same directory).
pub fn save(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(to_json(settings).as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("plumspirit-settings-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// L-G02: the keys follow the Piano Roll by default — also for a
    /// settings file written before the option existed (its `key_scope` is
    /// kept for when another choice is made).
    #[test]
    fn keyboard_colors_follow_the_roll_by_default_and_old_files_keep_their_scope() {
        assert!(Settings::default().key_follow_roll);
        let l = parse(r#"{"version":1,"key_scope":"audible"}"#);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert!(l.settings.key_follow_roll);
        assert_eq!(l.settings.key_scope, ScopeSetting::Audible);
        let l = parse(r#"{"version":1,"key_scope":"audible","key_colors_follow_roll":false}"#);
        assert!(!l.settings.key_follow_roll);
    }

    #[test]
    fn save_and_reload_each_theme() {
        let dir = temp_dir("roundtrip");
        for theme in [
            ThemeSetting::Dark,
            ThemeSetting::Light,
            ThemeSetting::System,
        ] {
            let s = Settings {
                theme,
                window_size: Some([1366.0, 768.0]),
                soundfont: Some("C:\\SF\\GeneralUser GS.sf2".into()),
                velocity_lane_visible: theme != ThemeSetting::Dark,
                velocity_lane_height: 150.0,
                follow_playhead: theme == ThemeSetting::Light,
                inspector_follow_playhead: theme != ThemeSetting::Light,
                smooth_follow: theme == ThemeSetting::System,
                anchor_playhead: theme != ThemeSetting::Dark,
                pause_rewinds: theme == ThemeSetting::Light,
                new_project_bpm: plumspirit_engine::model::Bpm::parse("174.5").unwrap(),
                new_project_ppqn: 3_840,
                new_project_keys256: theme != ThemeSetting::Dark,
                default_zoom_x: 0.25,
                default_zoom_y: 37.5,
                default_fit_keys: theme == ThemeSetting::Light,
                new_project_onion_order: if theme == ThemeSetting::Light {
                    plumspirit_engine::model::project::OnionOrder::SparseFirst
                } else {
                    plumspirit_engine::model::project::OnionOrder::DenseFirst
                },
                hide_text_overlays: theme != ThemeSetting::System,
                high_contrast: theme == ThemeSetting::Dark,
                ui_accent_color: Some([12, 180, 95]),
                track_color_mode: crate::ui::theme::TrackColorMode::Rainbow16,
                track_color_manual: vec![[50, 100, 150]; 16],
                ui_scale_percent: if theme == ThemeSetting::Dark { 30 } else { 175 },
                help_text_percent: if theme == ThemeSetting::Dark { 80 } else { 160 },
                show_orientation: theme == ThemeSetting::System,
                tile_cache_mb: if theme == ThemeSetting::Dark { 0 } else { 256 },
                prefetch_tiles: 4,
                render_quality: if theme == ThemeSetting::Light {
                    crate::piano_roll::dense::RenderQuality::Detailed
                } else {
                    crate::piano_roll::dense::RenderQuality::Fast
                },
                keybinds: if theme == ThemeSetting::Dark {
                    Vec::new()
                } else {
                    // In command-id order, as settings.json keeps them.
                    vec![
                        (crate::app::commands::Command::OpenTool("humanize"), vec![]),
                        (
                            crate::app::commands::Command::Undo,
                            vec![(
                                egui::Modifiers::COMMAND | egui::Modifiers::ALT,
                                egui::Key::Z,
                            )],
                        ),
                    ]
                },
                autobackup_minutes: if theme == ThemeSetting::Dark { 0 } else { 45 },
                autobackup_keep: 12,
                autobackup_folder: std::env::temp_dir()
                    .join("PlumSpirit Backups")
                    .to_string_lossy()
                    .into_owned(),
                // Off survives a save / load (the default is on).
                display_note_colors: theme == ThemeSetting::Light,
                speed: Speed::parse("2.5").unwrap(),
                default_gate: DefaultGate::Ticks(333),
                link_gate: theme == ThemeSetting::Dark,
                graph_tool: GraphTool::FreeHand,
                curve: CurveSettings {
                    before_notes: true,
                    seed: 99,
                    interval: IntervalSetting::Ticks(7),
                    ..CurveSettings::default()
                },
                curve_presets: vec![CurvePreset {
                    name: "Mine ✓".into(),
                    expr: "t^3".into(),
                }],
                show_metrics: false,
                export_explicit_status: true,
                export_player_safe_start: false,
                export_loudness_reference: true,
                graph2_height: 90.0,
                follow_position: 30,
                preview: theme != ThemeSetting::Light,
                progressor: ProgressorSettings {
                    enabled: theme == ThemeSetting::Light,
                    template: "LET'S GO ✓ {notes}\n{pps}".into(),
                    corner: crate::app::progressor::Corner::BottomLeft,
                    offset: [12.5, -4.0],
                    font_size: 18.0,
                    text_color: Some([1, 2, 3]),
                    text_opacity: 70,
                    background: false,
                    background_opacity: 40,
                    align: crate::app::progressor::TextAlign::Center,
                    line_spacing: 1.5,
                    number_format: crate::app::progressor::NumberFormat::Compact,
                    pps_decimals: 3,
                    window_ms: 2500,
                    scope: ScopeSetting::Audible,
                    selection_template: "Σ {notes}\n{unique_poly} ✓".into(),
                    collapsed: theme == ThemeSetting::Light,
                    selection_collapsed: theme == ThemeSetting::Dark,
                    selection_offset: [24.0, -6.0],
                },
                player: crate::player::PlayerSettings {
                    enabled: theme == ThemeSetting::Light,
                    template: "♪ {passed} / {total_notes}\n{fps} FPS".into(),
                    fall_seconds: 7.25,
                    mapping: if theme == ThemeSetting::Light {
                        crate::player::rain::Mapping::Tick
                    } else {
                        crate::player::rain::Mapping::Time
                    },
                    gradient: 40,
                    glow: 15,
                },
                inspector: theme != ThemeSetting::Light,
                live_keys: theme == ThemeSetting::Dark,
                key_scope: ScopeSetting::ActiveTrack,
                key_follow_roll: theme == ThemeSetting::Dark,
                audition: theme == ThemeSetting::System,
                audition_ms: 400,
                onion_outlines: theme != ThemeSetting::Dark,
                onion_outline_darkness: 33,
                renderer: crate::renderer::Backend::Vulkan,
                gpu_preference: crate::renderer::GpuPreference::Integrated,
                audio: AudioSettings {
                    device: Some("Speakers (Realtek) ✓".into()),
                    sample_rate: Some(44_100),
                    buffer_frames: Some(512),
                    prerender: true,
                    render_ahead_ms: 12_000,
                    max_voices: 1_536,
                    voice_policy: crate::audio::synth::VoicePolicy::Oldest,
                    audition_voices: 32,
                    note_ons_per_block: 4_096,
                    effects: false,
                    gain_db: -7.5,
                    limiter: false,
                    resume_restrike: true,
                    external_keep_all: theme == ThemeSetting::Dark,
                    processing: plumspirit_engine::playback::EventProcessing {
                        keys_88: theme != ThemeSetting::Light,
                        velocity_band: (theme == ThemeSetting::Dark).then_some((0, 9)),
                        ..Default::default()
                    },
                    default_output: match theme {
                        ThemeSetting::Light => crate::audio::midi_out::OutputTarget::Kdmapi,
                        ThemeSetting::Dark => crate::audio::midi_out::OutputTarget::Device(
                            "Roland SC-88 Pro ✓".into(),
                        ),
                        ThemeSetting::System => crate::audio::midi_out::OutputTarget::Internal,
                    },
                    soundfonts: vec!["C:\\SF\\a.sf2".into(), "D:\\b.sf2".into()],
                },
                skip_channel_10: theme == ThemeSetting::Light,
                selection_analytics: theme != ThemeSetting::System,
                selection_advanced: theme == ThemeSetting::Dark,
                selection_follow_playhead: theme != ThemeSetting::Dark,
                selection_chart_seek: theme == ThemeSetting::System,
                selection_counter_song: theme == ThemeSetting::Light,
                onion_remember: theme != ThemeSetting::Light,
                onion_default: OnionDefault {
                    mode: plumspirit_engine::model::project::OnionMode::AllOthers,
                    density: plumspirit_engine::model::project::DensityBasis::WholeTrack,
                    opacity: 61,
                    per_track: theme == ThemeSetting::Dark,
                },
                key_guide_88: theme == ThemeSetting::Dark,
                note_colors: crate::ui::theme::NoteColorPrefs {
                    mode: if theme == ThemeSetting::Light {
                        crate::ui::theme::NoteColorMode::Manual
                    } else {
                        crate::ui::theme::NoteColorMode::RandomBySeed
                    },
                    seed: u64::MAX - 3,
                    manual: vec![[0x12, 0xAB, 0xEF], [0, 0, 0], [255, 255, 255]],
                },
                selection_opacity: match theme {
                    ThemeSetting::Light => 0,
                    ThemeSetting::Dark => 37,
                    ThemeSetting::System => 100,
                },
                clip_body_transparency: match theme {
                    ThemeSetting::Light => 0,
                    ThemeSetting::Dark => 55,
                    ThemeSetting::System => 100,
                },
                scale_y: ScaleYPrefs {
                    handles_duplicate: theme != ThemeSetting::System,
                    fill: plumspirit_engine::transform::geom::Fill {
                        res: 3,
                        fade: theme == ThemeSetting::Light,
                        keep_sub: theme != ThemeSetting::Dark,
                        merge: true,
                        mode: if theme == ThemeSetting::Dark {
                            plumspirit_engine::transform::geom::FillMode::Step {
                                step: 12,
                                copies: 2,
                            }
                        } else {
                            plumspirit_engine::transform::geom::FillMode::Rows
                        },
                    },
                    prevent: theme == ThemeSetting::System,
                    cleanup: plumspirit_engine::transform::geom::ScaleYCleanup::TrimEarlier,
                },
                rotate_about_center: theme == ThemeSetting::Light,
                y_zoom_beyond_keys: theme == ThemeSetting::Dark,
                lock_fit_range: theme != ThemeSetting::Dark,
                overlap: OverlapPrefs {
                    on: theme != ThemeSetting::Light,
                    general: plumspirit_engine::transform::overlap::General {
                        policy: plumspirit_engine::transform::overlap::Policy {
                            mode: match theme {
                                ThemeSetting::Light => {
                                    plumspirit_engine::transform::overlap::Mode::ExactDuplicates
                                }
                                ThemeSetting::Dark => {
                                    plumspirit_engine::transform::overlap::Mode::TrimOverlaps
                                }
                                ThemeSetting::System => {
                                    plumspirit_engine::transform::overlap::Mode::SamePitchStart
                                }
                            },
                            generated_only: theme == ThemeSetting::Dark,
                            across: theme != ThemeSetting::Dark,
                        },
                        existing: theme == ThemeSetting::System,
                    },
                },
                plugins: {
                    let mut m = Map::new();
                    m.insert("tool:safor".into(), json!({"values": "x", "scope": 1}));
                    m
                },
                lua_plugins: crate::lua::plugins::PluginPrefs {
                    run_plugins: theme != ThemeSetting::Light,
                    folder: (theme == ThemeSetting::Dark)
                        .then(|| std::env::temp_dir().join("plumspirit plugins")),
                    enable_on_install: theme == ThemeSetting::System,
                    installed: [(
                        "com.example.tool".to_owned(),
                        crate::lua::plugins::PluginState {
                            enabled: theme != ThemeSetting::System,
                            granted: vec!["notes.edit".into(), "project.read".into()],
                            origin: crate::lua::package::Origin::LocalFile,
                        },
                    )]
                    .into_iter()
                    .collect(),
                    tool_keys: [("com.example.tool/shift".to_owned(), "Ctrl+Alt+1".to_owned())]
                        .into_iter()
                        .collect(),
                },
            };
            save(&dir, &s).unwrap();
            let loaded = load(Some(&dir));
            assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
            assert_eq!(loaded.settings, s);
        }
        // No temporary files are left behind.
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("settings.json")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn numeric_preferences_reopen_without_decimal_truncation() {
        let dir = temp_dir("precision");
        let settings = Settings {
            speed: Speed::parse("0.123456789012345").unwrap(),
            progressor: ProgressorSettings {
                offset: ["-2.5000001".parse().unwrap(), 1e-9],
                line_spacing: 1.1234567,
                ..Default::default()
            },
            ..Default::default()
        };
        save(&dir, &settings).unwrap();
        let loaded = load(Some(&dir));
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings.speed, settings.speed);
        assert_eq!(
            loaded.settings.progressor.offset,
            settings.progressor.offset
        );
        assert_eq!(
            loaded.settings.progressor.line_spacing,
            settings.progressor.line_spacing
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_malformed_files_fall_back() {
        let dir = temp_dir("missing");
        let l = load(Some(&dir));
        // First launch: the defaults for this PC (§258).
        let mut first = Settings::default();
        machine_defaults(
            &mut first,
            std::thread::available_parallelism().ok().map(|n| n.get()),
        );
        assert_eq!(l.settings, first);
        assert!(l.problems.is_empty());
        for bad in [
            "",
            "{",
            "[1,2]",
            "null",
            "{\"theme\": 5}",
            "{\"version\":1,\"theme\":\"purple\"}",
        ] {
            let l = parse(bad);
            assert_eq!(l.settings.theme, ThemeSetting::System, "{bad}");
            assert!(!l.problems.is_empty(), "{bad}");
        }
        // One bad field does not discard the good ones; unknown fields are ignored.
        let l = parse(r#"{"version":1,"theme":"Light","window_size":[5,5],"future":true}"#);
        assert_eq!(l.settings.theme, ThemeSetting::Light);
        assert_eq!(l.settings.window_size, None);
        let l = parse(r#"{"version":7,"theme":"dark","window_size":[1920,1080]}"#);
        assert_eq!(l.settings.theme, ThemeSetting::Dark);
        assert_eq!(l.settings.window_size, Some([1920.0, 1080.0]));
        // v0.2 fields: bad values fall back, heights are clamped.
        let l = parse(
            r#"{"version":1,"soundfont":5,"velocity_lane_visible":"x","velocity_lane_height":5000}"#,
        );
        assert_eq!(l.settings.soundfont, None);
        assert!(l.settings.velocity_lane_visible);
        assert_eq!(l.settings.velocity_lane_height, MAX_LANE_HEIGHT);
        assert_eq!(l.problems.len(), 2);
        // v0.3 fields: Follow Playhead defaults on; bad values fall back.
        assert!(l.settings.follow_playhead);
        assert!(l.settings.inspector_follow_playhead);
        let bad_inspector =
            parse(r#"{"version":1,"inspector_follow_playhead":"yes","follow_playhead":false}"#);
        assert!(bad_inspector.settings.inspector_follow_playhead);
        assert!(!bad_inspector.settings.follow_playhead);
        assert_eq!(bad_inspector.problems.len(), 1);
        assert_eq!(l.settings.follow_position, DEFAULT_FOLLOW_POSITION);
        assert!(l.settings.preview);
        for (text, want) in [("0", 0), ("25", 25), ("75.4", 75), ("100", 100)] {
            let l = parse(&format!(r#"{{"version":1,"follow_position":{text}}}"#));
            assert_eq!(l.settings.follow_position, want, "{text}");
            assert!(l.problems.is_empty());
        }
        for bad in ["-1", "101", "\"x\"", "null"] {
            let l = parse(&format!(r#"{{"version":1,"follow_position":{bad}}}"#));
            assert_eq!(l.settings.follow_position, DEFAULT_FOLLOW_POSITION, "{bad}");
            assert!(!l.problems.is_empty());
        }
        let l = parse(r#"{"version":1,"preview":false}"#);
        assert!(!l.settings.preview);
        let l = parse(r#"{"version":1,"follow_playhead":false,"smooth_follow":"yes"}"#);
        assert!(!l.settings.follow_playhead);
        assert!(!l.settings.smooth_follow);
        assert_eq!(l.problems.len(), 1);
        // v0.4 fields: each bad value falls back alone.
        let l = parse(
            r#"{"version":1,"speed":"0","default_gate":"preset:8t","graph_tool":"nope","curve":{"seed":5,"policy":"x"},"curve_presets":[{"name":"a","expr":"t"},{"name":""}]}"#,
        );
        assert_eq!(l.settings.speed, Speed::NORMAL);
        assert_eq!(
            l.settings.default_gate,
            DefaultGate::Preset(plumspirit_engine::editor::gate::GatePreset::EighthTriplet)
        );
        assert_eq!(l.settings.graph_tool, GraphTool::Edit);
        assert_eq!(l.settings.curve.seed, 5);
        assert_eq!(l.settings.curve.policy, WritePolicy::Replace);
        assert_eq!(l.settings.curve_presets.len(), 1);
        assert_eq!(l.problems.len(), 4, "{:?}", l.problems);
    }

    /// B1-0135: the default output survives a restart; a bad value falls back
    /// to the internal synthesizer alone (other audio values are kept).
    #[test]
    fn a_bad_default_output_falls_back_to_internal_alone() {
        use crate::audio::midi_out::OutputTarget;
        let l = parse(
            r#"{"version": 1, "audio": {"default_output": "device:Synth A", "max_voices": 512}}"#,
        );
        assert_eq!(
            l.settings.audio.default_output,
            OutputTarget::Device("Synth A".into())
        );
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        for bad in [r#""device:""#, "7", r#""midi""#] {
            let l = parse(&format!(
                r#"{{"version": 1, "audio": {{"default_output": {bad}, "max_voices": 512}}}}"#
            ));
            assert_eq!(l.settings.audio.default_output, OutputTarget::Internal);
            assert_eq!(l.settings.audio.max_voices, 512);
            assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        }
        let l = parse(r#"{"version": 1, "audio": {}}"#);
        assert_eq!(l.settings.audio.default_output, OutputTarget::Internal);
    }

    /// B1-0059: New Project BPM / PPQN fall back alone.
    #[test]
    fn bad_new_project_defaults_fall_back_alone() {
        let l = parse(r#"{"version": 1, "new_project_bpm": "174.5", "new_project_ppqn": 3840}"#);
        assert_eq!(l.settings.new_project_bpm.to_string(), "174.5");
        assert_eq!(l.settings.new_project_ppqn, 3_840);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        let l = parse(
            r#"{"version": 1, "new_project_bpm": "-1", "new_project_ppqn": 0, "follow_playhead": false}"#,
        );
        assert_eq!(
            l.settings.new_project_bpm,
            plumspirit_engine::model::Bpm::DEFAULT
        );
        assert_eq!(l.settings.new_project_ppqn, DEFAULT_NEW_PPQN);
        assert!(!l.settings.follow_playhead, "other values kept");
        assert_eq!(l.problems.len(), 2, "{:?}", l.problems);
    }

    /// B1-0059: the new-project Onion order and Hide Text Overlays fall back alone.
    #[test]
    fn bad_onion_order_and_overlay_values_fall_back_alone() {
        use plumspirit_engine::model::project::OnionOrder;
        let l = parse(
            r#"{"version": 1, "new_project_onion_order": "sparse_first", "hide_text_overlays": true}"#,
        );
        assert_eq!(l.settings.new_project_onion_order, OnionOrder::SparseFirst);
        assert!(l.settings.hide_text_overlays);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        let l = parse(
            r#"{"version": 1, "new_project_onion_order": "densest", "hide_text_overlays": "yes", "follow_playhead": false}"#,
        );
        assert_eq!(l.settings.new_project_onion_order, OnionOrder::Project);
        assert!(!l.settings.hide_text_overlays);
        assert!(!l.settings.follow_playhead, "other values kept");
        assert_eq!(l.problems.len(), 2, "{:?}", l.problems);
        for o in OnionOrder::ALL {
            assert_eq!(onion_order_from_key(onion_order_key(o)), Some(o));
        }
    }

    /// B1-0062: the UI scale is a whole percent from 30 to 175; a bad
    /// stored value falls back to 100 % alone.
    #[test]
    fn ui_scale_is_validated_and_falls_back_alone() {
        assert_eq!(parse_ui_scale("125"), Ok(125));
        assert_eq!(parse_ui_scale(" 30 % "), Ok(30));
        assert_eq!(parse_ui_scale("175%"), Ok(175));
        for bad in ["29", "176", "0", "-50", "12.5", "abc", "", "100 px"] {
            assert!(parse_ui_scale(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_ui_scale("200").unwrap_err(),
            "The UI scale must be a whole percent from 30 % to 175 %."
        );
        let l = parse(r#"{"version": 1, "ui_scale_percent": 150}"#);
        assert_eq!(l.settings.ui_scale_percent, 150);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        for bad in ["10", "300", "\"big\"", "1.5"] {
            let l = parse(&format!(
                r#"{{"version": 1, "ui_scale_percent": {bad}, "follow_playhead": false}}"#
            ));
            assert_eq!(l.settings.ui_scale_percent, 100, "{bad}");
            assert!(!l.settings.follow_playhead, "other values kept");
            assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        }
        assert_eq!(Settings::default().ui_scale_percent, 100);
    }

    /// B1-0059: the tile cache and prefetch values fall back alone.
    #[test]
    fn bad_cache_and_prefetch_values_fall_back_alone() {
        let l = parse(r#"{"version": 1, "tile_cache_mb": 96, "prefetch_tiles": 0}"#);
        assert_eq!(
            (l.settings.tile_cache_mb, l.settings.prefetch_tiles),
            (96, 0)
        );
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        let l = parse(
            r#"{"version": 1, "tile_cache_mb": 4, "prefetch_tiles": 9, "render_quality": "ultra"}"#,
        );
        let d = Settings::default();
        assert_eq!(l.settings.tile_cache_mb, d.tile_cache_mb);
        assert_eq!(l.settings.prefetch_tiles, d.prefetch_tiles);
        assert_eq!(l.settings.render_quality, d.render_quality);
        assert_eq!(l.problems.len(), 3, "{:?}", l.problems);
        assert_eq!(parse_tile_cache_mb("96 MB"), Ok(96));
        assert_eq!(parse_tile_cache_mb("0"), Ok(0));
        assert!(parse_tile_cache_mb("5000").is_err());
        assert!(parse_prefetch_tiles("-1").is_err());
    }

    /// B1-0060: keybinds fall back per command and per chord.
    #[test]
    fn bad_keybinds_fall_back_alone() {
        use crate::app::commands::Command;
        let l = parse(
            r#"{"version": 1, "keybinds": {"Undo": ["Ctrl+Alt+Z", "Hyper+Q"], "NoSuch": ["F4"], "Redo": 3, "ToggleOnionOutlines": []}, "follow_playhead": false}"#,
        );
        // Kept in command-id order (the order settings.json stores them).
        assert_eq!(
            l.settings.keybinds.iter().map(|k| k.0).collect::<Vec<_>>(),
            vec![Command::ToggleOnionOutlines, Command::Undo]
        );
        assert!(l.settings.keybinds[0].1.is_empty(), "unbound is kept");
        assert_eq!(
            l.settings.keybinds[1].1.len(),
            1,
            "the readable chord is kept"
        );
        assert!(!l.settings.follow_playhead, "other values kept");
        assert_eq!(l.problems.len(), 3, "{:?}", l.problems);
        let l = parse(r#"{"version": 1, "keybinds": ["Undo"]}"#);
        assert!(l.settings.keybinds.is_empty());
        assert_eq!(l.problems.len(), 1);
    }

    /// B1-0059: the automatic backup values fall back alone.
    #[test]
    fn bad_autobackup_values_fall_back_alone() {
        let l = parse(r#"{"version": 1, "autobackup_minutes": 0, "autobackup_keep": 20}"#);
        assert_eq!(l.settings.autobackup_minutes, 0, "0 = off is valid");
        assert_eq!(l.settings.autobackup_keep, 20);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        let l = parse(
            r#"{"version": 1, "autobackup_minutes": 999, "autobackup_keep": 0, "autobackup_folder": "relative", "smooth_follow": true}"#,
        );
        let d = Settings::default();
        assert_eq!(l.settings.autobackup_minutes, d.autobackup_minutes);
        assert_eq!(l.settings.autobackup_keep, d.autobackup_keep);
        assert_eq!(l.settings.autobackup_folder, "");
        assert!(l.settings.smooth_follow, "other values kept");
        assert_eq!(l.problems.len(), 3, "{:?}", l.problems);
    }

    /// B1-0059: the new-project key range and the default zoom fall back alone.
    #[test]
    fn bad_key_range_and_zoom_defaults_fall_back_alone() {
        let l = parse(
            r#"{"version": 1, "new_project_keys256": true, "default_zoom_x": 0.5, "default_zoom_y": 250}"#,
        );
        assert!(l.settings.new_project_keys256);
        assert_eq!(l.settings.default_zoom_x, 0.5);
        assert_eq!(l.settings.default_zoom_y, 250.0);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        let l = parse(
            r#"{"version": 1, "new_project_keys256": 1, "default_zoom_x": -4, "default_zoom_y": 0, "pause_rewinds": true}"#,
        );
        assert!(!l.settings.new_project_keys256);
        assert_eq!(l.settings.default_zoom_x, 100.0);
        assert_eq!(l.settings.default_zoom_y, 100.0);
        assert!(l.settings.pause_rewinds, "other values kept");
        assert_eq!(l.problems.len(), 3, "{:?}", l.problems);
        assert!(parse_default_zoom_x("1e6").is_err());
        // B1-0090: beyond the row limits the default is clamped, not refused.
        let min = plumspirit_engine::view::MIN_ROW_HEIGHT
            / crate::piano_roll::viewport::DEFAULT_ROW_HEIGHT
            * 100.0;
        assert_eq!(parse_default_zoom_y("4"), Ok(min), "0.5-point rows");
        assert!(parse_default_zoom_y("0").is_err());
        assert_eq!(parse_default_zoom_y("50 %"), Ok(50.0));
    }

    #[test]
    fn weak_pcs_start_with_fewer_voices_and_others_keep_the_default() {
        let mut s = Settings::default();
        let normal = s.audio.max_voices;
        machine_defaults(&mut s, Some(8));
        assert_eq!(s.audio.max_voices, normal);
        machine_defaults(&mut s, None);
        assert_eq!(s.audio.max_voices, normal);
        machine_defaults(&mut s, Some(2));
        assert_eq!(s.audio.max_voices, LOW_END_VOICES);
        assert!(LOW_END_VOICES < normal);
        // OpenGL and Realtime stay the defaults.
        assert_eq!(s.renderer, crate::renderer::Backend::OpenGl);
        assert!(!s.audio.prerender);
    }

    /// B1-0098: the clip body transparency survives a restart, defaults to
    /// 80 % (20 % opacity) for older files, and a bad value falls back alone.
    #[test]
    fn clip_body_transparency_persists_and_defaults_to_eighty_percent() {
        let dir = temp_dir("clip-transparency");
        let s = Settings {
            clip_body_transparency: 35,
            ..Settings::default()
        };
        save(&dir, &s).unwrap();
        let back = load(Some(&dir));
        assert!(back.problems.is_empty(), "{:?}", back.problems);
        assert_eq!(back.settings.clip_body_transparency, 35);
        let old = parse(r#"{"version": 1, "theme": "dark"}"#);
        assert_eq!(old.settings.clip_body_transparency, 80);
        for bad in ["-1", "101", "\"x\"", "null"] {
            let l = parse(&format!(
                r#"{{"version":1,"clip_body_transparency":{bad},"follow_position":20}}"#
            ));
            assert_eq!(l.settings.clip_body_transparency, 80, "{bad}");
            assert_eq!(l.settings.follow_position, 20);
            assert_eq!(l.problems.len(), 1, "{bad}");
        }
    }

    #[test]
    fn manual_track_colors_persist_without_overwriting_note_or_accent_preferences() {
        let dir = temp_dir("manual-tracks");
        let mut s = Settings {
            track_color_mode: crate::ui::theme::TrackColorMode::Manual,
            track_color_manual: vec![[24, 47, 180]; 3],
            ui_accent_color: Some([0, 180, 120]),
            ..Default::default()
        };
        s.note_colors.seed = 757;
        save(&dir, &s).unwrap();
        let loaded = load(Some(&dir));
        assert!(loaded.problems.is_empty());
        assert_eq!(loaded.settings, s);
    }

    #[test]
    fn track_rainbow_preference_round_trips_and_bad_modes_fall_back() {
        let dir = temp_dir("track-rainbow");
        let s = Settings {
            track_color_mode: crate::ui::theme::TrackColorMode::Rainbow16,
            ..Default::default()
        };
        save(&dir, &s).unwrap();
        let loaded = load(Some(&dir));
        assert!(loaded.problems.is_empty());
        assert_eq!(loaded.settings, s);
        let bad = parse(r#"{"version":1,"track_color_mode":"wrong","follow_position":20}"#);
        assert_eq!(bad.settings.track_color_mode, Default::default());
        assert_eq!(bad.settings.follow_position, 20);
        assert_eq!(bad.problems.len(), 1);
    }

    /// B2-F12: the event-processing options survive a restart, each invalid
    /// one is off on its own with a problem line, and they reach the
    /// scheduler's limits (an invalid band in memory processes nothing).
    /// B2-F28: the Scale Y cleanup mode survives a restart; an unknown one is
    /// exact duplicates with a problem line.
    #[test]
    fn scale_y_cleanup_mode_survives_restart_and_an_unknown_one_is_exact() {
        use plumspirit_engine::transform::geom::ScaleYCleanup;
        let dir = temp_dir("scale-y-cleanup");
        for m in ScaleYCleanup::ALL {
            let mut s = Settings::default();
            s.scale_y.cleanup = m;
            save(&dir, &s).unwrap();
            let loaded = load(Some(&dir));
            assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
            assert_eq!(loaded.settings.scale_y.cleanup, m);
        }
        let got = parse(r#"{"version":1,"scale_y":{"cleanup":"loudest"}}"#);
        assert_eq!(got.settings.scale_y.cleanup, ScaleYCleanup::ExactDuplicates);
        assert_eq!(got.problems.len(), 1, "{:?}", got.problems);
        assert_eq!(
            Settings::default().scale_y.cleanup,
            ScaleYCleanup::ExactDuplicates
        );
    }

    #[test]
    fn event_processing_survives_restart_and_bad_values_are_off_alone() {
        use plumspirit_engine::playback::EventProcessing;
        let dir = temp_dir("event-processing");
        for p in [
            EventProcessing::default(),
            EventProcessing {
                keys_88: true,
                velocity_band: None,
                ..Default::default()
            },
            EventProcessing {
                keys_88: false,
                velocity_band: Some((0, 127)),
                ..Default::default()
            },
            EventProcessing {
                keys_88: true,
                velocity_band: Some((12, 12)),
                ..Default::default()
            },
            EventProcessing {
                length_override_ns: Some(1),
                off_delay_ns: Some(0),
                ..Default::default()
            },
            EventProcessing {
                length_override_ns: Some(123_456_789_012),
                off_delay_ns: Some(u64::MAX),
                ..Default::default()
            },
            EventProcessing {
                resets: plumspirit_engine::playback::ResetPolicy::SendChanged,
                ..Default::default()
            },
            EventProcessing {
                resets: plumspirit_engine::playback::ResetPolicy::Send,
                keys_88: true,
                ..Default::default()
            },
        ] {
            let mut s = Settings::default();
            s.audio.processing = p;
            save(&dir, &s).unwrap();
            let loaded = load(Some(&dir));
            assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
            assert_eq!(loaded.settings, s);
            let limits =
                crate::audio::engine::limits_for(&loaded.settings.audio.synth(), 48_000, false);
            assert_eq!(limits.processing, p);
        }
        for (json, keys, band, problems) in [
            (
                r#"{"keys_88":"yes","velocity_band":[1,9]}"#,
                false,
                Some((1, 9)),
                1,
            ),
            (r#"{"keys_88":true,"velocity_band":[9,1]}"#, true, None, 1),
            (r#"{"keys_88":true,"velocity_band":[0,128]}"#, true, None, 1),
            (r#"{"velocity_band":[3]}"#, false, None, 1),
            (r#"{"velocity_band":null}"#, false, None, 0),
            (
                r#"{"length_override_ns":0,"off_delay_ns":-5}"#,
                false,
                None,
                2,
            ),
            (
                r#"{"length_override_ns":2.5,"off_delay_ns":"1"}"#,
                false,
                None,
                2,
            ),
            (r#"{"resets":"always"}"#, false, None, 1),
            (r#"7"#, false, None, 1),
        ] {
            let text = format!(r#"{{"version":1,"audio":{{"event_processing":{json}}}}}"#);
            let got = parse(&text);
            assert_eq!(got.settings.audio.processing.keys_88, keys, "{json}");
            assert_eq!(got.settings.audio.processing.velocity_band, band, "{json}");
            assert_eq!(got.problems.len(), problems, "{json}: {:?}", got.problems);
        }
        let mut cfg = crate::audio::synth::SynthConfig::default();
        cfg.processing.velocity_band = Some((90, 10));
        assert_eq!(cfg.clamped().processing.velocity_band, None);
    }

    #[test]
    fn voice_policy_survives_restart_and_invalid_values_fall_back_alone() {
        use crate::audio::synth::VoicePolicy;
        let dir = temp_dir("voice-policy");
        for policy in VoicePolicy::ALL {
            let mut s = Settings::default();
            s.audio.voice_policy = policy;
            save(&dir, &s).unwrap();
            let got = load(Some(&dir));
            assert!(got.problems.is_empty());
            assert_eq!(got.settings.audio.voice_policy, policy);
            assert_eq!(got.settings.audio.synth().voice_policy, policy);
        }
        for value in ["null", "17", "\"newest\""] {
            let got = parse(&format!(
                r#"{{"version":1,"audio":{{"voice_policy":{value},"max_voices":1024}}}}"#
            ));
            assert_eq!(got.settings.audio.voice_policy, VoicePolicy::Quietest);
            assert_eq!(got.settings.audio.max_voices, 1024);
            assert_eq!(got.problems.len(), 1);
        }
        assert_eq!(
            parse(r#"{"version":1,"audio":{}}"#)
                .settings
                .audio
                .voice_policy,
            VoicePolicy::Quietest
        );
    }

    #[test]
    fn rainbow_note_mode_survives_a_restart_without_losing_manual_settings() {
        let dir = temp_dir("rainbow16");
        let mut s = Settings::default();
        s.note_colors.mode = crate::ui::theme::NoteColorMode::Rainbow16;
        s.note_colors.manual = vec![[19, 37, 91]; 16];
        s.note_colors.seed = 183;
        save(&dir, &s).unwrap();
        let loaded = load(Some(&dir));
        assert!(loaded.problems.is_empty());
        assert_eq!(loaded.settings, s);
    }

    #[test]
    fn ui_accent_color_survives_restart_and_bad_colors_fall_back_alone() {
        let dir = temp_dir("ui-accent");
        for color in [
            None,
            Some([0, 255, 128]),
            Some([255, 255, 255]),
            Some([0, 0, 0]),
        ] {
            let s = Settings {
                ui_accent_color: color,
                ..Settings::default()
            };
            save(&dir, &s).unwrap();
            let back = load(Some(&dir));
            assert!(back.problems.is_empty(), "{:?}", back.problems);
            assert_eq!(back.settings, s);
        }
        assert_eq!(parse(r#"{"version":1}"#).settings.ui_accent_color, None);
        for bad in [
            "7",
            "[]",
            "{}",
            "true",
            "\"#ABC\"",
            "\"#GG0000\"",
            "\"#11223344\"",
            "\"#☃123\"",
        ] {
            let l = parse(&format!(
                r#"{{"version":1,"ui_accent_color":{bad},"follow_position":20}}"#
            ));
            assert_eq!(l.settings.ui_accent_color, None);
            assert_eq!(l.settings.follow_position, 20);
            assert_eq!(l.problems.len(), 1, "{bad}");
        }
        let valid = parse(r##"{"version":1,"ui_accent_color":"#00ff80"}"##);
        assert_eq!(valid.settings.ui_accent_color, Some([0, 255, 128]));
        assert!(valid.problems.is_empty());
    }

    /// v0.9 §119: Selection Opacity survives a restart; a v0.8 file gets
    /// 100 % (the v0.8 look); a bad value falls back alone.
    #[test]
    fn selection_opacity_persists_and_old_files_get_the_full_treatment() {
        let dir = temp_dir("selection-opacity");
        let s = Settings {
            selection_opacity: 42,
            ..Settings::default()
        };
        save(&dir, &s).unwrap();
        let back = load(Some(&dir));
        assert!(back.problems.is_empty(), "{:?}", back.problems);
        assert_eq!(back.settings.selection_opacity, 42);
        let old = parse(r#"{"version": 1, "theme": "dark"}"#);
        assert_eq!(old.settings.selection_opacity, 100);
        for (text, want) in [("0", 0), ("12.6", 13), ("100", 100)] {
            let l = parse(&format!(r#"{{"version":1,"selection_opacity":{text}}}"#));
            assert_eq!(l.settings.selection_opacity, want, "{text}");
            assert!(l.problems.is_empty());
        }
        for bad in ["-1", "101", "\"x\"", "null"] {
            let l = parse(&format!(
                r#"{{"version":1,"selection_opacity":{bad},"follow_position":20}}"#
            ));
            assert_eq!(l.settings.selection_opacity, 100, "{bad}");
            assert_eq!(l.settings.follow_position, 20);
            assert_eq!(l.problems.len(), 1, "{bad}");
        }
    }

    /// v0.9 §064–§067, §215: the note colour preferences survive a restart;
    /// a v0.8 file gets PlumSpirit's own colours; bad fields fall back alone.
    #[test]
    fn note_colour_preferences_persist_and_old_files_get_the_defaults() {
        use crate::ui::theme::{NoteColorMode, NoteColorPrefs};
        let dir = temp_dir("note-colors");
        let s = Settings {
            note_colors: NoteColorPrefs {
                mode: NoteColorMode::Manual,
                seed: 123_456_789_012,
                manual: (0..16u8).map(|i| [i * 16, 255 - i, 7]).collect(),
            },
            ..Settings::default()
        };
        save(&dir, &s).unwrap();
        let back = load(Some(&dir));
        assert!(back.problems.is_empty(), "{:?}", back.problems);
        assert_eq!(back.settings.note_colors, s.note_colors);
        assert!(to_json(&s).contains("\"manual\": ["));
        // A file from v0.8 (no note_colors): Random by Seed, seed 0.
        let old = parse(r#"{"version": 1, "theme": "dark", "key_guide_88": true}"#);
        assert!(old.problems.is_empty(), "{:?}", old.problems);
        assert_eq!(old.settings.note_colors, NoteColorPrefs::default());
        assert!(crate::ui::theme::NotePalette::new(old.settings.note_colors).is_default());
        // Bad fields fall back one by one; the rest of the file still loads.
        let many: Vec<String> = (0..20).map(|i| format!("\"#0000{i:02X}\"")).collect();
        let text = format!(
            r##"{{"version": 1, "theme": "light", "note_colors": {{"mode": "rainbow", "seed": -4, "manual": ["#GG0000", "#112233", 5, {}]}}}}"##,
            many.join(", ")
        );
        let bad = parse(&text);
        assert_eq!(bad.settings.theme, ThemeSetting::Light);
        let n = &bad.settings.note_colors;
        assert_eq!((n.mode, n.seed), (NoteColorMode::RandomBySeed, 0));
        assert_eq!(n.manual.len(), crate::ui::theme::MANUAL_NOTE_COLORS);
        assert_eq!(n.manual[0], [0x11, 0x22, 0x33]);
        assert_eq!(n.manual[1], [0, 0, 0]);
        let joined = bad.problems.join("\n");
        for want in [
            "note_colors.mode",
            "note_colors.seed",
            "#GG0000",
            "the first 16 are used",
        ] {
            assert!(joined.contains(want), "{want}: {joined}");
        }
        let not_object = parse(r#"{"version": 1, "note_colors": 3}"#);
        assert_eq!(not_object.settings.note_colors, NoteColorPrefs::default());
        assert_eq!(not_object.problems.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
