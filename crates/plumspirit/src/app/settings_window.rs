//! v1.0-beta1 (B1-0058): one Settings window (toolbar gear, Edit → Settings…,
//! Ctrl+,) with every application option in sections and a search over labels
//! and tooltips. Each entry runs the same command or setter as its menu item,
//! so there is one implementation and no second copy of any state; options
//! with their own window (Audio Settings, Note Colors, …) open it from here.
//! Changes apply at once; "Reset section" / "Restore all defaults" bring the
//! values of `Settings::default()` back the same way. Inventory:
//! `docs/v1_0_beta1/SETTINGS_MAP.md`.

use super::PlumSpiritApp;
use super::commands::Command;
use crate::renderer::{Backend, GpuPreference};
use crate::settings::{AUDITION_LENGTHS, Settings, ThemeSetting};
use egui::RichText;
use plumspirit_engine::plugins::PluginId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    /// L-F03: the groups of contract §011.
    Interface,
    PianoRoll,
    TrackList,
    Playback,
    Rendering,
    NotesColors,
    ImportExport,
    Performance,
    Plugins,
    Shortcuts,
    Diagnostics,
}

impl Section {
    pub const ALL: [Section; 12] = [
        Section::Appearance,
        Section::Interface,
        Section::PianoRoll,
        Section::TrackList,
        Section::Playback,
        Section::Rendering,
        Section::NotesColors,
        Section::ImportExport,
        Section::Performance,
        Section::Plugins,
        Section::Shortcuts,
        Section::Diagnostics,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Interface => "Interface & Accessibility",
            Section::PianoRoll => "Piano Roll",
            Section::TrackList => "Track List",
            Section::Playback => "Playback & External MIDI",
            Section::Rendering => "Rendering / Onion",
            Section::NotesColors => "Notes / Colors",
            Section::ImportExport => "Import / Export",
            Section::Performance => "Performance / Memory",
            Section::Plugins => "Plugins",
            Section::Shortcuts => "Shortcuts",
            Section::Diagnostics => "Advanced Diagnostics",
        }
    }
}

/// What an entry does when used (or reset), through the existing paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Run(Command),
    /// L-F06: new projects / MIDI files start with the Onion Skin last used.
    OnionRemember(bool),
    /// L-F02: a new view fits all keys (or starts at Default Zoom Y).
    DefaultFitKeys(bool),
    Renderer(Backend),
    Gpu(GpuPreference),
    PauseRewinds(bool),
    InspectorFollow(bool),
    SelectionOpacity(u8),
    OutlineDarkness(u8),
    /// B1-0098: a Track List clip body's transparency in percent.
    ClipTransparency(u8),
    AudioSettings,
    NoteColors,
    /// B1-0059: the key range the New Project dialog starts with.
    NewProjectKeys256(bool),
    /// B1-0059: Rendering Quality (the exact-rectangle limit).
    Quality(crate::piano_roll::dense::RenderQuality),
    /// B1-0059: the Onion layer order of new projects.
    NewOnionOrder(plumspirit_engine::model::project::OnionOrder),
    /// B1-0062: the global UI scale in percent.
    UiScale(u16),
    UiAccent(Option<[u8; 3]>),
}

/// B1-0098: what a transparency means as opacity, spelled out.
pub fn opacity_words(transparency: u8) -> String {
    format!("= {} % opacity", 100 - transparency.min(100))
}

/// B1-0062: the label (and draft key) of the UI scale entry.
pub const UI_SCALE_LABEL: &str = "UI Scale";
pub const UI_ACCENT_LABEL: &str = "UI Accent Color";

/// One radio choice: label, whether it is the current one, its action.
pub type Choice = (&'static str, fn(&Settings) -> bool, Action);

#[derive(Clone, Copy)]
pub enum Item {
    /// A checkbox: its state, and the toggle that flips it.
    Toggle(fn(&Settings) -> bool, Command),
    /// Radio buttons: (label, is this one selected, action).
    Radio(&'static [Choice]),
    /// A percent slider (0–100): value, and the action for a new value.
    Percent(fn(&Settings) -> u8, fn(u8) -> Action),
    /// B1-0098: a transparency slider (0–100 % transparent) that also
    /// states the opacity it means, so "80 %" can't be read both ways.
    Transparency(fn(&Settings) -> u8, fn(u8) -> Action),
    /// Opens the window that holds the option(s).
    Open(Action),
    /// A short explanation (no control).
    Note(&'static str),
    /// A validated text field: the current value as text, and how a typed
    /// value is checked and stored (Err: the sentence shown, nothing stored).
    Text(
        fn(&Settings) -> String,
        fn(&mut Settings, &str) -> Result<(), String>,
    ),
    /// B1-0062: the UI scale — presets, a typed percent, Reset to 100 % and
    /// the small-controls warning.
    UiScale,
    UiAccent,
    /// L-F04: the renderer in use (API, GPU, driver) and whether a restart
    /// is needed — what View → Renderer used to show.
    RendererStatus,
}

fn plugins_folder_text(s: &Settings) -> String {
    s.lua_plugins
        .folder
        .as_ref()
        .map(|f| f.display().to_string())
        .unwrap_or_default()
}

/// B1-0059: an absolute, existing folder, or empty for the default.
fn set_plugins_folder(s: &mut Settings, t: &str) -> Result<(), String> {
    let t = t.trim();
    if t.is_empty() {
        s.lua_plugins.folder = None;
        return Ok(());
    }
    let p = std::path::PathBuf::from(t);
    if !p.is_absolute() {
        return Err("Type a full folder path (like C:\\Users\\you\\PlumSpirit Plugins), or leave it empty for the default.".into());
    }
    if !p.is_dir() {
        return Err("That folder does not exist; create it first.".into());
    }
    s.lua_plugins.folder = Some(p);
    Ok(())
}

fn set_new_bpm(s: &mut Settings, t: &str) -> Result<(), String> {
    s.new_project_bpm = plumspirit_engine::model::Bpm::parse_midi(t)?;
    Ok(())
}

fn set_new_ppqn(s: &mut Settings, t: &str) -> Result<(), String> {
    s.new_project_ppqn = plumspirit_engine::model::timing::parse_ppqn(t)?;
    Ok(())
}

fn zoom_text(p: f64) -> String {
    // Plain digits (no grouping), so the text reads back as the same value.
    let t = format!("{p}");
    format!("{t} %")
}

fn set_zoom_x(s: &mut Settings, t: &str) -> Result<(), String> {
    s.default_zoom_x = crate::settings::parse_default_zoom_x(t)?;
    Ok(())
}

fn set_zoom_y(s: &mut Settings, t: &str) -> Result<(), String> {
    s.default_zoom_y = crate::settings::parse_default_zoom_y(t)?;
    Ok(())
}

fn set_tile_cache(s: &mut Settings, t: &str) -> Result<(), String> {
    s.tile_cache_mb = crate::settings::parse_tile_cache_mb(t)?;
    Ok(())
}

fn set_prefetch(s: &mut Settings, t: &str) -> Result<(), String> {
    s.prefetch_tiles = crate::settings::parse_prefetch_tiles(t)?;
    Ok(())
}

fn set_backup_minutes(s: &mut Settings, t: &str) -> Result<(), String> {
    s.autobackup_minutes = super::autobackup::parse_interval(t)?;
    Ok(())
}

fn set_backup_keep(s: &mut Settings, t: &str) -> Result<(), String> {
    s.autobackup_keep = super::autobackup::parse_keep(t)?;
    Ok(())
}

fn set_backup_folder(s: &mut Settings, t: &str) -> Result<(), String> {
    s.autobackup_folder = super::autobackup::parse_folder(t)?;
    Ok(())
}

const QUALITY: &[Choice] = {
    use crate::piano_roll::dense::RenderQuality as Q;
    &[
        (
            "Fast (exact notes up to 6,000 visible)",
            |s| s.render_quality == Q::Fast,
            Action::Quality(Q::Fast),
        ),
        (
            "Balanced (12,000; default)",
            |s| s.render_quality == Q::Balanced,
            Action::Quality(Q::Balanced),
        ),
        (
            "Detailed (48,000)",
            |s| s.render_quality == Q::Detailed,
            Action::Quality(Q::Detailed),
        ),
    ]
};

const ROTATION_PIVOTS: &[Choice] = &[
    (
        "Selection center (default)",
        |s| s.rotate_about_center,
        Action::Run(Command::RotatePivot(true)),
    ),
    (
        "Opposite corner",
        |s| !s.rotate_about_center,
        Action::Run(Command::RotatePivot(false)),
    ),
];

const OVERLAP_MODES: &[Choice] = {
    use plumspirit_engine::transform::overlap::Mode as M;
    &[
        (
            "Exact duplicates",
            |s| s.overlap.general.policy.mode == M::ExactDuplicates,
            Action::Run(Command::OverlapMode(0)),
        ),
        (
            "Same key and start",
            |s| s.overlap.general.policy.mode == M::SamePitchStart,
            Action::Run(Command::OverlapMode(1)),
        ),
        (
            "Overlaps: shorten the earlier",
            |s| s.overlap.general.policy.mode == M::TrimOverlaps,
            Action::Run(Command::OverlapMode(2)),
        ),
        (
            "Overlaps: remove the later (default)",
            |s| s.overlap.general.policy.mode == M::RemoveOverlaps,
            Action::Run(Command::OverlapMode(3)),
        ),
    ]
};

const NEW_ONION_ORDER: &[Choice] = {
    use plumspirit_engine::model::project::OnionOrder as O;
    &[
        (
            "Project Order",
            |s| s.new_project_onion_order == O::Project,
            Action::NewOnionOrder(O::Project),
        ),
        (
            "Dense First",
            |s| s.new_project_onion_order == O::DenseFirst,
            Action::NewOnionOrder(O::DenseFirst),
        ),
        (
            "Sparse First",
            |s| s.new_project_onion_order == O::SparseFirst,
            Action::NewOnionOrder(O::SparseFirst),
        ),
    ]
};

const NEW_ONION: &[Choice] = &[
    (
        "The Onion Skin last used (default)",
        |s| s.onion_remember,
        Action::OnionRemember(true),
    ),
    (
        "Onion Skin off",
        |s| !s.onion_remember,
        Action::OnionRemember(false),
    ),
];

/// L-F02: what a new Piano Roll view shows vertically.
const DEFAULT_Y_VIEW: &[Choice] = &[
    (
        "Default Zoom Y (above)",
        |s| !s.default_fit_keys,
        Action::DefaultFitKeys(false),
    ),
    (
        "Fit All Keys (128, or 256 in 256-Key Mode)",
        |s| s.default_fit_keys,
        Action::DefaultFitKeys(true),
    ),
];

/// L-F04: View → Keyboard Colors Show moved here.
const KEY_SCOPE: &[Choice] = {
    use crate::app::progressor::ScopeSetting as K;
    &[
        (
            "What the Piano Roll Shows (Active Track + Onion Ghosts)",
            |s| s.key_follow_roll,
            Action::Run(Command::KeyFollowRoll),
        ),
        (
            "All Tracks (Mute / Solo Ignored)",
            |s| !s.key_follow_roll && s.key_scope == K::Project,
            Action::Run(Command::KeyScope(K::Project)),
        ),
        (
            "Audible Tracks (Mute / Solo Respected)",
            |s| !s.key_follow_roll && s.key_scope == K::Audible,
            Action::Run(Command::KeyScope(K::Audible)),
        ),
        (
            "Active Track + Subtracks",
            |s| !s.key_follow_roll && s.key_scope == K::ActiveTrack,
            Action::Run(Command::KeyScope(K::ActiveTrack)),
        ),
    ]
};

const NEW_KEYS: &[Choice] = &[
    (
        "128 keys (MIDI 0–127)",
        |s| !s.new_project_keys256,
        Action::NewProjectKeys256(false),
    ),
    (
        "256-Key Mode (keys 0–255)",
        |s| s.new_project_keys256,
        Action::NewProjectKeys256(true),
    ),
];

#[derive(Clone, Copy)]
pub struct Entry {
    pub section: Section,
    pub label: &'static str,
    pub tip: &'static str,
    pub item: Item,
}

const fn e(section: Section, label: &'static str, tip: &'static str, item: Item) -> Entry {
    Entry {
        section,
        label,
        tip,
        item,
    }
}

const THEMES: &[Choice] = &[
    (
        "Dark",
        |s| s.theme == ThemeSetting::Dark,
        Action::Run(Command::Theme(ThemeSetting::Dark)),
    ),
    (
        "Light",
        |s| s.theme == ThemeSetting::Light,
        Action::Run(Command::Theme(ThemeSetting::Light)),
    ),
    (
        "System",
        |s| s.theme == ThemeSetting::System,
        Action::Run(Command::Theme(ThemeSetting::System)),
    ),
];

const AUDITION: &[Choice] = &[
    (
        "100 ms",
        |s| s.audition_ms == AUDITION_LENGTHS[0],
        Action::Run(Command::AuditionLength(AUDITION_LENGTHS[0])),
    ),
    (
        "250 ms (default)",
        |s| s.audition_ms == AUDITION_LENGTHS[1],
        Action::Run(Command::AuditionLength(AUDITION_LENGTHS[1])),
    ),
    (
        "400 ms",
        |s| s.audition_ms == AUDITION_LENGTHS[2],
        Action::Run(Command::AuditionLength(AUDITION_LENGTHS[2])),
    ),
    (
        "800 ms",
        |s| s.audition_ms == AUDITION_LENGTHS[3],
        Action::Run(Command::AuditionLength(AUDITION_LENGTHS[3])),
    ),
];

const PAUSE: &[Choice] = &[
    (
        "Pause holds the place (Play resumes there)",
        |s| !s.pause_rewinds,
        Action::PauseRewinds(false),
    ),
    (
        "Pause returns to the anchor",
        |s| s.pause_rewinds,
        Action::PauseRewinds(true),
    ),
];

const RENDERERS: &[Choice] = &[
    (
        "OpenGL",
        |s| s.renderer == Backend::OpenGl,
        Action::Renderer(Backend::OpenGl),
    ),
    (
        "Vulkan",
        |s| s.renderer == Backend::Vulkan,
        Action::Renderer(Backend::Vulkan),
    ),
];

const GPUS: &[Choice] = &[
    (
        "Automatic",
        |s| s.gpu_preference == GpuPreference::Automatic,
        Action::Gpu(GpuPreference::Automatic),
    ),
    (
        "Integrated GPU",
        |s| s.gpu_preference == GpuPreference::Integrated,
        Action::Gpu(GpuPreference::Integrated),
    ),
    (
        "Dedicated GPU",
        |s| s.gpu_preference == GpuPreference::Dedicated,
        Action::Gpu(GpuPreference::Dedicated),
    ),
];

/// Every entry, in display order. Each command / setting appears once.
pub fn entries() -> &'static [Entry] {
    use Section as S;
    const LIST: &[Entry] = &[
        // ------------------------------------------------------------ General
        e(
            S::PianoRoll,
            "Preview Batch Edits",
            "Batch operations show a live Preview before Apply.",
            Item::Toggle(|s| s.preview, Command::TogglePreview),
        ),
        e(
            S::PianoRoll,
            "New Subchannels Skip MIDI Channel 10",
            "New subchannels skip channel 10 (drums) when numbering their MIDI channels.",
            Item::Toggle(|s| s.skip_channel_10, Command::ToggleSkipChannel10),
        ),
        e(
            S::ImportExport,
            "New Project BPM",
            "The tempo the New Project dialog starts with (any positive BPM, kept exactly, e.g. 120, 174.5, 1e6). Press Enter to apply.",
            Item::Text(|s| s.new_project_bpm.to_string(), set_new_bpm),
        ),
        e(
            S::ImportExport,
            "New Project PPQN",
            "The ticks per quarter note the New Project dialog starts with (1 to 2,147,483,647, e.g. 960). Press Enter to apply.",
            Item::Text(|s| s.new_project_ppqn.to_string(), set_new_ppqn),
        ),
        e(
            S::ImportExport,
            "New Project Keys",
            "The key range the New Project dialog starts with. 256-Key Mode stores keys 0–255 (keys above 127 are not Standard MIDI notes); Project Properties changes it per project.",
            Item::Radio(NEW_KEYS),
        ),
        e(
            S::ImportExport,
            "Safe Saving",
            "Save writes a temporary file next to the project, flushes it to the disk, reads it back and checks every note, event and setting, and only then replaces the project file in one step. If anything fails, the old file is left exactly as it was and the project stays marked unsaved.",
            Item::Note(
                "Saving is always safe: written to a temporary file, checked, then swapped in.",
            ),
        ),
        e(
            S::ImportExport,
            "Automatic Backup Every (minutes)",
            "While a project has unsaved changes, a verified copy is written this often (0 = off). The project itself, its file and its unsaved state are not changed; editing goes on while the copy is written. Press Enter to apply.",
            Item::Text(|s| s.autobackup_minutes.to_string(), set_backup_minutes),
        ),
        e(
            S::ImportExport,
            "Backups Kept per Project",
            "The newest backups of each project that are kept (1 to 100); older ones of that project are deleted. Press Enter to apply.",
            Item::Text(|s| s.autobackup_keep.to_string(), set_backup_keep),
        ),
        e(
            S::ImportExport,
            "Backup Folder",
            "Where backups are written. Empty: the Backups folder next to PlumSpirit's settings (%APPDATA%\\PlumSpirit\\Backups on Windows). Files are named \"<project> [<id>] backup <UTC date and time>.lup\" and open like any project. Press Enter to apply.",
            Item::Text(|s| s.autobackup_folder.clone(), set_backup_folder),
        ),
        e(
            S::ImportExport,
            "MIDI Export: Use Running Status Compression",
            "On: a message leaves out its status byte when it repeats the previous one (the standard compact form). Off: every message carries its status byte, for players that mishandle running status.",
            Item::Toggle(|s| !s.export_explicit_status, Command::ToggleExplicitStatus),
        ),
        e(
            S::ImportExport,
            "MIDI Export: Add Program Change 0 When the File Has No Control Event",
            "On: a file written from the project's contents that would hold no controller, program change or pitch bend gets one Program Change 0 (the General MIDI default instrument, so nothing sounds different) on its first channel at the start. Kiva's default Pre-render audio engine stops responding on a file without any control event whose notes are all below its Audio Note Threshold. No note, velocity or timing changes; files exported unchanged from their source MIDI are not affected.",
            Item::Toggle(
                |s| s.export_player_safe_start,
                Command::TogglePlayerSafeStart,
            ),
        ),
        e(
            S::ImportExport,
            "MIDI Export: Add a Silent Loudness Reference for Piano From Above",
            "Piano From Above scales a file's velocities so that its loudest note plays at 127, so a song whose notes are all quiet plays loud there. On: when the song's loudest note is below 127, the file gets one silent reference note at velocity 127 (zero length, on a MIDI channel the song does not use, whose volume is set to 0) so Piano From Above plays the velocities as written. Players count it as one extra note; the project is not changed. Off by default.",
            Item::Toggle(
                |s| s.export_loudness_reference,
                Command::ToggleLoudnessReference,
            ),
        ),
        // --------------------------------------------------------- Appearance
        e(
            S::Appearance,
            "Theme",
            "Dark, Light, or follow the system setting.",
            Item::Radio(THEMES),
        ),
        e(
            S::Appearance,
            UI_ACCENT_LABEL,
            "The color of focus rings, active controls, tabs and selection frames. Pick a color or type #RRGGBB and press Enter. Shades adjust for contrast in each theme. Note and Track colors stay as set.",
            Item::UiAccent,
        ),
        e(
            S::Appearance,
            "High Contrast",
            "Full black / white text, stronger weak text and borders and thicker outlines in menus, panels and windows (WCAG AAA text, AA weak text). Note colors are not changed.",
            Item::Toggle(|s| s.high_contrast, Command::ToggleHighContrast),
        ),
        e(
            S::Interface,
            UI_SCALE_LABEL,
            "The size of everything in PlumSpirit — menus, icons, text, panels, dialogs, the keyboard and graphs — from 30 % to 175 % of the system's own size (100 %). The Piano Roll's zoom and the notes are not changed. Type a whole percent and press Enter, or pick a preset. Reset to 100 % goes back (its shortcut, Reset UI Scale to 100 %, is in Keyboard Shortcuts).",
            Item::UiScale,
        ),
        e(
            S::NotesColors,
            "General Color Preferences…",
            "Random by Seed or Manual note colors, for every project.",
            Item::Open(Action::NoteColors),
        ),
        e(
            S::NotesColors,
            "Selection Opacity",
            "How strongly selected notes take the selected-note look (0 % = like the others, 100 % = full).",
            Item::Percent(|s| s.selection_opacity, Action::SelectionOpacity),
        ),
        // --------------------------------------------------------- Piano Roll
        e(
            S::PianoRoll,
            "Event Graphs",
            "The velocity / controller lane under the roll.",
            Item::Toggle(|s| s.velocity_lane_visible, Command::ToggleVelocityLane),
        ),
        e(
            S::PianoRoll,
            "Default Zoom X",
            "Horizontal zoom of a new view (a project created or opened, and View → Reset Piano Roll View): 100 % = 32 points per quarter note. 0.01 % to 100,000 %; the roll's own limits apply. Press Enter to apply.",
            Item::Text(|s| zoom_text(s.default_zoom_x), set_zoom_x),
        ),
        e(
            S::PianoRoll,
            "Default Zoom Y",
            "Vertical zoom of a new view: 100 % = 12-point key rows (about 4 % to 4,000 %). Press Enter to apply.",
            Item::Text(|s| zoom_text(s.default_zoom_y), set_zoom_y),
        ),
        e(
            S::PianoRoll,
            "Default Y View",
            "What a new view (a project created or opened, Reset View) shows vertically: the Default Zoom Y, or every key fitted to the height (Fit 128 Keys, Fit All 256 Keys in 256-Key Mode). With Lock Fit Range it stays fitted when the height changes; switching between the Track List and the Piano Roll keeps the view.",
            Item::Radio(DEFAULT_Y_VIEW),
        ),
        e(
            S::Rendering,
            "Ghost Outlines",
            "Onion Skin ghosts are drawn with outlines.",
            Item::Toggle(|s| s.onion_outlines, Command::ToggleOnionOutlines),
        ),
        e(
            S::Rendering,
            "Ghost Outline Darkness",
            "0 % = white, 50 % = the ghost's color, 100 % = black (not an opacity).",
            Item::Percent(|s| s.onion_outline_darkness, Action::OutlineDarkness),
        ),
        e(
            S::Rendering,
            "Onion Layer Order for New Projects",
            "Which ghost layer is drawn in front in a new project or an opened MIDI file: the track list's order, the densest source or the sparsest. A saved project keeps its own; View → Onion Skin changes it per project.",
            Item::Radio(NEW_ONION_ORDER),
        ),
        e(
            S::Rendering,
            "Onion Skin for New Projects",
            "What a new project or an opened MIDI file starts with: the Onion Skin you last chose in any project (mode, layer order, density, opacity, Per-Track Settings; a Specific list belongs to its project, so it starts as Previous and Next), or Onion Skin off. A saved project keeps its own.",
            Item::Radio(NEW_ONION),
        ),
        e(
            S::Interface,
            "Hide Text Overlays",
            "Hides the text drawn over the notes (the Progressor and Selection Analytics readouts) without turning them off.",
            Item::Toggle(|s| s.hide_text_overlays, Command::ToggleTextOverlays),
        ),
        e(
            S::PianoRoll,
            "88-Key Guide",
            "Lines at A0 and C8, the range of an 88-key piano.",
            Item::Toggle(|s| s.key_guide_88, Command::ToggleKeyGuide),
        ),
        e(
            S::PianoRoll,
            "Scale Handles: Duplicate Notes",
            "Dragging a scale handle to stretch the rows duplicates the notes into the added rows (the last Scale Y Duplicate Notes options). Alt on an edge reverses it for one drag (Alt on a corner rotates). Default off: N notes stay N notes.",
            Item::Toggle(
                |s| s.scale_y.handles_duplicate,
                Command::ToggleScaleYDuplicate,
            ),
        ),
        e(
            S::PianoRoll,
            "Rotation Pivot",
            "Where Alt + a corner handle drag turns the selection around: its center (default) or the corner opposite the dragged one. Shift snaps the angle to 15° steps.",
            Item::Radio(ROTATION_PIVOTS),
        ),
        e(
            S::PianoRoll,
            "Lock Fit Range",
            "On by default. After View → Fit 128 Keys / Fit All 256 Keys the keys stay fitted when the Event Graph panels or the window change the roll's height; a Y zoom ends the fit.",
            Item::Toggle(|s| s.lock_fit_range, Command::ToggleLockFitRange),
        ),
        e(
            S::PianoRoll,
            "Y Zoom Beyond the Keys",
            "Off by default: zooming out vertically stops where every key fills the roll's height, so no empty space shows below the keys. On: you can zoom out further (earlier versions always allowed it).",
            Item::Toggle(|s| s.y_zoom_beyond_keys, Command::ToggleYZoomBeyondKeys),
        ),
        e(
            S::PianoRoll,
            "Note Overlap Prevention",
            "Off by default. On: tools, scale gestures, Scale X / Y, Flip, Quantize, Paste and Draw remove the note overlaps they would create (Edit → Note Overlap Prevention); moves, Undo / Redo and imports are never changed.",
            Item::Toggle(|s| s.overlap.on, Command::ToggleOverlapPrevention),
        ),
        e(
            S::PianoRoll,
            "Note Overlap Prevention: What Counts",
            "Exact duplicates (same start, length and key), the same key and start, or same-key overlaps in time — shortening the earlier note or removing the later one (default).",
            Item::Radio(OVERLAP_MODES),
        ),
        e(
            S::PianoRoll,
            "Note Overlap Prevention: Only Notes the Operation Made or Changed",
            "On (default): a note the operation left exactly as it was is never removed.",
            Item::Toggle(
                |s| s.overlap.general.policy.generated_only,
                Command::ToggleOverlapGeneratedOnly,
            ),
        ),
        e(
            S::PianoRoll,
            "Note Overlap Prevention: Across Subchannels and MIDI Channels",
            "On: one key overlaps across subchannels and channels. Off (default): each subchannel and channel is its own layer.",
            Item::Toggle(
                |s| s.overlap.general.policy.across,
                Command::ToggleOverlapAcross,
            ),
        ),
        e(
            S::PianoRoll,
            "Note Overlap Prevention: Also Against Notes Already There",
            "On (default): results are compared with the notes already in the track, which are never changed. Off: results only with each other.",
            Item::Toggle(
                |s| s.overlap.general.existing,
                Command::ToggleOverlapExisting,
            ),
        ),
        e(
            S::PianoRoll,
            "Scale Y: Prevent Overlaps",
            "Scale Y and handle drags that change the rows remove the overlaps they create on one key; chords and the selection's own overlaps stay. Default on.",
            Item::Toggle(|s| s.scale_y.prevent, Command::ToggleScaleYOverlap),
        ),
        e(
            S::PianoRoll,
            "Live Keyboard Colors",
            "Piano keys light up in the colors of the notes sounding on them.",
            Item::Toggle(|s| s.live_keys, Command::ToggleLiveKeys),
        ),
        e(
            S::PianoRoll,
            "Note Audition",
            "Clicking keys and placing notes plays them.",
            Item::Toggle(|s| s.audition, Command::ToggleAudition),
        ),
        e(
            S::PianoRoll,
            "Keyboard Colors Show",
            "Which tracks' sounding notes color the piano keys (Live Keyboard Colors). By default what the Piano Roll shows: with the Onion Skin off, only the active track and its subtracks.",
            Item::Radio(KEY_SCOPE),
        ),
        e(
            S::PianoRoll,
            "Placed Note Sound Length",
            "How long a placed note is auditioned.",
            Item::Radio(AUDITION),
        ),
        e(
            S::Interface,
            "Progressor",
            "The analytics readout (Notes, NPS, Poly, PPS) over the roll.",
            Item::Toggle(|s| s.progressor.enabled, Command::ToggleProgressor),
        ),
        e(
            S::Interface,
            "Respect Track Mute/Solo in Progressor",
            "On (default): the Progressor counts only what plays — muted tracks are left out and, while any track is soloed, only soloed tracks count, exactly as playback applies Mute / Solo. Off: all tracks count. The readout says Audible Tracks or All Tracks. Also View menu.",
            Item::Toggle(
                |s| s.progressor.scope == crate::app::progressor::ScopeSetting::Audible,
                Command::ToggleProgressorMuteSolo,
            ),
        ),
        e(
            S::Interface,
            "Progressor Settings…",
            "The readout's text, position and scope.",
            Item::Open(Action::Run(Command::ProgressorSettings)),
        ),
        e(
            S::Interface,
            "Selection Analytics",
            "The Progressor also shows statistics of the selected notes.",
            Item::Toggle(|s| s.selection_analytics, Command::ToggleSelectionAnalytics),
        ),
        e(
            S::Interface,
            "Selection Analytics Settings…",
            "The selection readout's text and its compact form.",
            Item::Open(Action::Run(Command::SelectionAnalyticsSettings)),
        ),
        e(
            S::Interface,
            "Selection Analytics Charts",
            "A window of charts over the selected notes (NPS, polyphony, note counter, tempo); also the caret on the readout.",
            Item::Toggle(|s| s.selection_advanced, Command::ToggleSelectionCharts),
        ),
        // --------------------------------------------------------- Track List
        e(
            S::NotesColors,
            "Display Note Colors",
            "Clip mini notes in their note colors (subchannel / velocity); off: one color per clip.",
            Item::Toggle(|s| s.display_note_colors, Command::ToggleDisplayNoteColors),
        ),
        e(
            S::TrackList,
            "Clip Body Transparency",
            "How see-through a clip's body is behind its mini notes, as transparency: 80 % transparent = 20 % opacity (the default), 0 % = the solid track color, 100 % = only the notes show. The header bar with the clip's name always uses the track color at 100 % opacity.",
            Item::Transparency(|s| s.clip_body_transparency, Action::ClipTransparency),
        ),
        // ----------------------------------------------- Playback / MIDI Output
        e(
            S::Playback,
            "Follow Playhead",
            "The view follows the playhead during playback.",
            Item::Toggle(|s| s.follow_playhead, Command::ToggleFollow),
        ),
        e(
            S::Playback,
            "Smooth Follow",
            "Continuous scrolling instead of page turns.",
            Item::Toggle(|s| s.smooth_follow, Command::ToggleSmoothFollow),
        ),
        e(
            S::Playback,
            "Follow Position…",
            "Where the playhead is kept while following, in percent of the width.",
            Item::Open(Action::Run(Command::FollowPosition)),
        ),
        e(
            S::Playback,
            "Edit / Anchor Playhead",
            "A second, stationary playhead that only you move; Play starts there and Stop returns to it.",
            Item::Toggle(|s| s.anchor_playhead, Command::ToggleAnchorPlayhead),
        ),
        e(
            S::Playback,
            "After Pause",
            "With the Edit / Anchor playhead: what Pause does with the playback playhead.",
            Item::Radio(PAUSE),
        ),
        e(
            S::Playback,
            "Audio Settings…",
            "Audio device, sample rate, buffer, Realtime / Pre-Render, voices, headroom, the MIDI output (Internal, Windows MIDI devices, OmniMIDI KDMAPI), resume behavior and diagnostics.",
            Item::Open(Action::AudioSettings),
        ),
        e(
            S::Playback,
            "MIDI Player",
            "The built-in player view.",
            Item::Toggle(|s| s.player.enabled, Command::ToggleMidiPlayer),
        ),
        e(
            S::Playback,
            "MIDI Player Settings…",
            "Fall time, effects and the player's text.",
            Item::Open(Action::Run(Command::MidiPlayerSettings)),
        ),
        e(
            S::Playback,
            "MIDI Inspector",
            "The event inspector window.",
            Item::Toggle(|s| s.inspector, Command::ToggleMidiInspector),
        ),
        e(
            S::Playback,
            "Inspector Follow Playhead",
            "Follow the NPS, Polyphony and Tempo charts with page turns; manual navigation pauses following until Resume or a transport navigation.",
            Item::Radio(&[
                (
                    "On",
                    |s| s.inspector_follow_playhead,
                    Action::InspectorFollow(true),
                ),
                (
                    "Off",
                    |s| !s.inspector_follow_playhead,
                    Action::InspectorFollow(false),
                ),
            ]),
        ),
        // ---------------------------------------------------------- SoundFont
        e(
            S::Playback,
            "Choose SoundFont…",
            "The SF2 SoundFont for this project (also the default for new projects).",
            Item::Open(Action::Run(Command::ChooseSoundFont)),
        ),
        e(
            S::Playback,
            "SoundFont library and effects",
            "The SoundFont list, reverb and chorus are in Audio Settings → SoundFonts.",
            Item::Note(
                "Audio Settings… (Playback / MIDI Output) holds the SoundFont list and the reverb / chorus switch.",
            ),
        ),
        // ----------------------------------------------- Performance / Rendering
        e(
            S::Rendering,
            "Renderer",
            "OpenGL works on older and integrated graphics; Vulkan needs a Vulkan driver and falls back to OpenGL. Takes effect at the next start.",
            Item::Radio(RENDERERS),
        ),
        e(
            S::Rendering,
            "GPU (Vulkan)",
            "Which graphics card the Vulkan renderer asks for. Takes effect at the next start.",
            Item::Radio(GPUS),
        ),
        e(
            S::Rendering,
            "Renderer in Use",
            "What draws PlumSpirit now: the API, the graphics card and its driver.",
            Item::RendererStatus,
        ),
        e(
            S::Rendering,
            "Rendering Quality",
            "How many visible notes the Piano Roll and Onion Skin draw as exact rectangles before they switch to the dense note tiles (the same colors and layer order, drawn per pixel). Fast keeps frames light on slow PCs; Detailed shows individual notes further out but costs more per frame.",
            Item::Radio(QUALITY),
        ),
        e(
            S::Performance,
            "Tile Cache Budget (MB)",
            "Memory for the zoomed-out note tiles of the Piano Roll, Onion Skin, Track List and MIDI Player. 0 = automatic (24 MB on PCs with up to 3 GB of RAM, else 48 MB); 8 to 2,048 MB otherwise. More keeps more of the song ready when you scroll back; less saves memory. Press Enter to apply.",
            Item::Text(|s| s.tile_cache_mb.to_string(), set_tile_cache),
        ),
        e(
            S::Performance,
            "Preload Distance (tiles)",
            "Tiles built beyond the view while it is idle: this many before it and one more after it (default 1; 0 = only what is visible; up to 8). More makes scrolling and playback show ready notes sooner, at the cost of background work and cache memory. Press Enter to apply.",
            Item::Text(|s| s.prefetch_tiles.to_string(), set_prefetch),
        ),
        // ---------------------------------------------------- Audio Reference
        e(
            S::Plugins,
            "Audio Reference…",
            "An audio file played with the project for reference (saved with the project).",
            Item::Open(Action::Run(Command::OpenPlugin(PluginId::AudioReference))),
        ),
        // ------------------------------------------------------------ Keybinds
        e(
            S::Shortcuts,
            "Keyboard Shortcuts…",
            "Every shortcut, by menu. Change… or Add… a shortcut by pressing it (a shortcut another command uses asks before it moves), Clear, Reset one command or Reset All. Reset section here brings back every default shortcut.",
            Item::Open(Action::Run(Command::ShowKeybinds)),
        ),
        // --------------------------------------------------------- Lua Plugins
        e(
            S::Plugins,
            "Lua Plugins…",
            "Install, enable, allow and remove community Lua plugins, read their logs (also in the Plugins menu).",
            Item::Open(Action::Run(Command::ShowLuaPlugins)),
        ),
        e(
            S::Plugins,
            "Run Lua Plugins",
            "Off: no plugin runs (installed plugins and what you allowed them are kept). On by default.",
            Item::Toggle(|s| s.lua_plugins.run_plugins, Command::ToggleRunLuaPlugins),
        ),
        e(
            S::Plugins,
            "Enable Plugins When Installed",
            "On: a plugin you install is enabled at once with everything it asks for. Off (default): it waits until you review what it asks to do and enable it.",
            Item::Toggle(
                |s| s.lua_plugins.enable_on_install,
                Command::ToggleLuaEnableOnInstall,
            ),
        ),
        e(
            S::Plugins,
            "Plugins Folder",
            "Where installed plugins are kept: an absolute folder, or empty for \"plugins\" in PlumSpirit's settings folder. Plugins in the old folder stay there.",
            Item::Text(plugins_folder_text, set_plugins_folder),
        ),
        e(
            S::Plugins,
            "Trust",
            "What plugins can and cannot do.",
            Item::Note(
                "Plugins run in a sandbox: no programs, no network, no files except ones you pick, and only the capabilities you allow each plugin. A sandbox is not a perfect wall; install plugins only from people you trust.",
            ),
        ),
        // ------------------------------------------------ Privacy / Diagnostics
        e(
            S::Diagnostics,
            "CPU / RAM in Status Bar",
            "PlumSpirit's CPU and memory use in the status bar.",
            Item::Toggle(|s| s.show_metrics, Command::ToggleMetrics),
        ),
        e(
            S::Diagnostics,
            "Audio diagnostics report",
            "Audio Settings → Diagnostics → Copy report puts the audio state on the clipboard as text.",
            Item::Open(Action::AudioSettings),
        ),
        e(
            S::Diagnostics,
            "Save Diagnostics Report…",
            "One text file for a bug report: system and renderer, the open project's size (no notes, names or paths), the audio diagnostics, the settings and the end of this session's log. Also in the Help menu.",
            Item::Open(Action::Run(Command::SaveDiagnostics)),
        ),
        e(
            S::Diagnostics,
            "Privacy",
            "PlumSpirit sends nothing over the network.",
            Item::Note(
                "PlumSpirit sends nothing over the network; settings and logs stay in %APPDATA%\\PlumSpirit.",
            ),
        ),
    ];
    LIST
}

/// Entries matching `query` (case-insensitive, in label, tooltip or section
/// name); every entry of `section` when the query is empty.
pub fn matching(query: &str, section: Section) -> Vec<&'static Entry> {
    let q = query.trim().to_lowercase();
    entries()
        .iter()
        .filter(|en| {
            if q.is_empty() {
                en.section == section
            } else {
                en.label.to_lowercase().contains(&q)
                    || en.tip.to_lowercase().contains(&q)
                    || en.section.name().to_lowercase().contains(&q)
            }
        })
        .collect()
}

/// What resetting `en` must do to bring back the default (none if it is
/// already at the default or has nothing to reset).
pub fn reset_action(en: &Entry, now: &Settings, default: &Settings) -> Option<Action> {
    match en.item {
        Item::Toggle(on, cmd) => (on(now) != on(default)).then_some(Action::Run(cmd)),
        Item::Radio(opts) => {
            let want = opts.iter().find(|o| (o.1)(default))?;
            (!(want.1)(now)).then_some(want.2)
        }
        Item::Percent(get, set) | Item::Transparency(get, set) => {
            (get(now) != get(default)).then(|| set(get(default)))
        }
        Item::UiScale => (now.ui_scale_percent != default.ui_scale_percent)
            .then_some(Action::UiScale(default.ui_scale_percent)),
        Item::UiAccent => (now.ui_accent_color != default.ui_accent_color)
            .then_some(Action::UiAccent(default.ui_accent_color)),
        Item::Open(_) | Item::Note(_) | Item::Text(..) | Item::RendererStatus => None,
    }
}

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    /// The search field takes the keyboard when the window opens.
    pub focus_search: bool,
    pub(super) section: Option<Section>,
    search: String,
    confirm_defaults: bool,
    /// Text fields being edited: (label, typed text, error).
    drafts: Vec<(&'static str, String, Option<String>)>,
}

impl SettingsWindow {
    /// L-F04: shows `section` (the search cleared), as a contextual link does.
    pub fn show_section(&mut self, section: Section) {
        self.section = Some(section);
        self.search.clear();
        self.confirm_defaults = false;
    }

    /// Drops the typed text of `label` (its field shows the stored value).
    pub fn forget_draft(&mut self, label: &str) {
        self.drafts.retain(|d| d.0 != label);
    }
}

impl PlumSpiritApp {
    /// Runs one entry's action through the same path as its menu item.
    pub(super) fn settings_action(
        &mut self,
        a: Action,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        match a {
            Action::Run(c) => {
                if self.enabled(c) {
                    self.execute(c, ctx, frame);
                }
            }
            Action::Renderer(b) => self.set_renderer(b, None),
            Action::Gpu(g) => self.set_renderer(self.settings.renderer, Some(g)),
            Action::PauseRewinds(v) => {
                self.settings.pause_rewinds = v;
                self.apply_playhead_policy();
                self.save_settings();
            }
            Action::InspectorFollow(v) => self.set_inspector_follow(v),
            Action::SelectionOpacity(v) => self.set_selection_opacity(v, ctx, false),
            Action::OutlineDarkness(v) => {
                self.settings.onion_outline_darkness = v.min(100);
                self.save_settings();
            }
            Action::ClipTransparency(v) => {
                self.settings.clip_body_transparency = v.min(100);
                self.save_settings();
            }
            Action::AudioSettings => self.open_audio_settings(),
            Action::NoteColors => self.open_note_colors(),
            Action::NewProjectKeys256(v) => {
                self.settings.new_project_keys256 = v;
                self.save_settings();
            }
            Action::Quality(q) => {
                self.settings.render_quality = q;
                self.save_settings();
            }
            Action::NewOnionOrder(o) => {
                self.settings.new_project_onion_order = o;
                self.save_settings();
            }
            Action::OnionRemember(on) => {
                self.settings.onion_remember = on;
                self.save_settings();
            }
            Action::DefaultFitKeys(on) => {
                self.settings.default_fit_keys = on;
                self.save_settings();
            }
            Action::UiScale(p) => self.set_ui_scale(p, ctx),
            Action::UiAccent(c) => {
                self.settings.ui_accent_color =
                    c.filter(|c| *c != crate::ui::theme::DEFAULT_UI_ACCENT);
                crate::ui::theme::apply(
                    ctx,
                    self.settings.theme,
                    self.settings.high_contrast,
                    self.settings.ui_accent_color,
                );
                self.settings_window.forget_draft(UI_ACCENT_LABEL);
                self.save_settings();
            }
        }
    }

    /// Brings back the defaults of `section` (every section when `None`),
    /// one entry at a time through its own path. Returns how many changed.
    pub(super) fn reset_settings(
        &mut self,
        section: Option<Section>,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) -> usize {
        let default = Settings::default();
        let mut n = 0;
        for en in entries() {
            if section.is_some_and(|s| s != en.section) {
                continue;
            }
            if let Item::Text(get, set) = en.item {
                if get(&self.settings) != get(&default)
                    && set(&mut self.settings, &get(&default)).is_ok()
                {
                    self.save_settings();
                    n += 1;
                }
                self.settings_window.drafts.retain(|d| d.0 != en.label);
                continue;
            }
            if let Some(a) = reset_action(en, &self.settings, &default) {
                self.settings_action(a, ctx, frame);
                n += 1;
            }
        }
        // B1-0060: the user's keybinds belong to the Keybinds section.
        if section.is_none_or(|s| s == Section::Shortcuts) && !self.settings.keybinds.is_empty() {
            self.set_keybinds(Vec::new());
            n += 1;
        }
        n
    }

    pub(super) fn settings_window_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.settings_window.open {
            return;
        }
        let mut open = true;
        let mut actions: Vec<Action> = Vec::new();
        let mut texts_changed = false;
        let mut reset: Option<Option<Section>> = None;
        let section = self.settings_window.section.unwrap_or(Section::Appearance);
        let mut want_help = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(true)
            .default_width(620.0)
            .default_height(480.0)
            .max_height((ctx.content_rect().height() - 16.0).max(80.0))
            .vscroll(ctx.content_rect().height() < 450.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Search");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.settings_window.search)
                            .hint_text("a setting, e.g. follow, opacity, renderer")
                            .desired_width(280.0),
                    );
                    if std::mem::take(&mut self.settings_window.focus_search) {
                        r.request_focus();
                    }
                    if !self.settings_window.search.is_empty() && ui.small_button("×").on_hover_text("Clear the search").clicked()
                    {
                        self.settings_window.search.clear();
                    }
                    // B1-0050: context help.
                    want_help |= super::help::help_button(ui, "settings");
                });
                ui.separator();
                let searching = !self.settings_window.search.trim().is_empty();
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(170.0);
                        for s in Section::ALL {
                            if ui
                                .add_enabled(
                                    !searching,
                                    egui::Button::selectable(section == s && !searching, s.name()),
                                )
                                .clicked()
                            {
                                self.settings_window.section = Some(s);
                                self.settings_window.confirm_defaults = false;
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        let list = matching(&self.settings_window.search, section);
                        // B1-0062: the list shrinks with the window in points
                        // (a large UI scale), so the buttons below stay on screen.
                        let list_h = (ctx.content_rect().height() - 190.0).clamp(80.0, 400.0);
                        egui::ScrollArea::vertical()
                            .max_height(list_h)
                            .show(ui, |ui| {
                                if searching && list.is_empty() {
                                    ui.label(RichText::new("No setting matches.").weak());
                                }
                                let mut last: Option<Section> = None;
                                for en in list {
                                    if searching && last != Some(en.section) {
                                        ui.label(RichText::new(en.section.name()).small().weak());
                                        last = Some(en.section);
                                    }
                                    if let Item::Text(get, set) = en.item {
                                        texts_changed |= text_ui(
                                            ui,
                                            en,
                                            get,
                                            set,
                                            &mut self.settings,
                                            &mut self.settings_window.drafts,
                                        );
                                    } else if let Item::UiAccent = en.item {
                                        ui_accent_ui(ui, en, &self.settings, &mut self.settings_window.drafts, &mut actions);
                                    } else if let Item::RendererStatus = en.item {
                                        if self.settings.renderer != self.renderer_run.running {
                                            ui.label(
                                                RichText::new(format!(
                                                    "Restart Required: {} is used from the next start.",
                                                    self.settings.renderer.label()
                                                ))
                                                .small()
                                                .strong(),
                                            );
                                        }
                                        ui.label(
                                            RichText::new(format!(
                                                "In use: {}",
                                                self.renderer_info.line()
                                            ))
                                            .small()
                                            .weak(),
                                        )
                                        .on_hover_text(en.tip);
                                        ui.add_space(4.0);
                                    } else if let Item::UiScale = en.item {
                                        ui_scale_ui(
                                            ui,
                                            en,
                                            &self.settings,
                                            &mut self.settings_window.drafts,
                                            &mut actions,
                                        );
                                    } else {
                                        entry_ui(ui, en, &self.settings, &mut actions);
                                    }
                                }
                            });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if !searching
                                && ui
                                    .button("Reset section")
                                    .on_hover_text("Brings back the default of every option in this section.")
                                    .clicked()
                            {
                                reset = Some(Some(section));
                            }
                            let label = if self.settings_window.confirm_defaults {
                                "Click again to restore every default"
                            } else {
                                "Restore all defaults"
                            };
                            if ui
                                .button(label)
                                .on_hover_text("Every option on these pages back to its default (projects are not changed).")
                                .clicked()
                            {
                                if self.settings_window.confirm_defaults {
                                    reset = Some(None);
                                    self.settings_window.confirm_defaults = false;
                                } else {
                                    self.settings_window.confirm_defaults = true;
                                }
                            }
                        });
                        ui.label(
                            RichText::new("Changes apply at once and are kept for the next start.")
                                .small()
                                .weak(),
                        );
                    });
                });
            });
        self.settings_window.open = open;
        if want_help {
            self.open_help("settings");
        }
        if texts_changed {
            self.save_settings();
        }
        for a in actions {
            self.settings_action(a, ctx, frame);
        }
        if let Some(s) = reset {
            let n = self.reset_settings(s, ctx, frame);
            self.set_status(
                if n == 0 {
                    "Every option was already at its default.".to_owned()
                } else {
                    format!("{n} option(s) back to the default.")
                },
                crate::ui::status_bar::StatusKind::Info,
            );
        }
    }
}

/// A validated text field; true when a new value was stored.
fn text_ui(
    ui: &mut egui::Ui,
    en: &Entry,
    get: fn(&Settings) -> String,
    set: fn(&mut Settings, &str) -> Result<(), String>,
    s: &mut Settings,
    drafts: &mut Vec<(&'static str, String, Option<String>)>,
) -> bool {
    let i = match drafts.iter().position(|d| d.0 == en.label) {
        Some(i) => i,
        None => {
            drafts.push((en.label, get(s), None));
            drafts.len() - 1
        }
    };
    let mut stored = false;
    ui.horizontal(|ui| {
        ui.label(en.label).on_hover_text(en.tip);
        let r = ui
            .add(egui::TextEdit::singleline(&mut drafts[i].1).desired_width(140.0))
            .on_hover_text(en.tip);
        if r.lost_focus() {
            match apply_text(s, set, &drafts[i].1) {
                Ok(changed) => {
                    stored = changed;
                    drafts[i].1 = get(s);
                    drafts[i].2 = None;
                }
                Err(e) => drafts[i].2 = Some(e),
            }
        }
    });
    if let Some(e) = &drafts[i].2 {
        ui.label(RichText::new(e).small().color(ui.visuals().error_fg_color));
    }
    ui.add_space(4.0);
    stored
}

/// B1-0062: the UI scale row — a typed percent (checked on Enter / focus
/// loss), the presets, Reset to 100 % and, under 60 %, the warning.
fn ui_scale_ui(
    ui: &mut egui::Ui,
    en: &Entry,
    s: &Settings,
    drafts: &mut Vec<(&'static str, String, Option<String>)>,
    actions: &mut Vec<Action>,
) {
    use crate::settings::{UI_SCALE_PRESETS, UI_SCALE_SMALL_BELOW, parse_ui_scale};
    let now = s.ui_scale_percent;
    let i = match drafts.iter().position(|d| d.0 == en.label) {
        Some(i) => i,
        None => {
            drafts.push((en.label, format!("{now} %"), None));
            drafts.len() - 1
        }
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(en.label).on_hover_text(en.tip);
        let r = ui
            .add(egui::TextEdit::singleline(&mut drafts[i].1).desired_width(64.0))
            .on_hover_text(en.tip);
        if r.lost_focus() {
            match parse_ui_scale(&drafts[i].1) {
                Ok(p) => {
                    drafts[i].1 = format!("{p} %");
                    drafts[i].2 = None;
                    if p != now {
                        actions.push(Action::UiScale(p));
                    }
                }
                Err(e) => drafts[i].2 = Some(e),
            }
        }
        for p in UI_SCALE_PRESETS {
            if ui
                .add(egui::Button::selectable(now == p, format!("{p} %")))
                .on_hover_text(en.tip)
                .clicked()
                && now != p
            {
                actions.push(Action::UiScale(p));
            }
        }
        if ui
            .add_enabled(now != 100, egui::Button::new("Reset to 100 %"))
            .on_hover_text(format!(
                "The system's own size{}.",
                super::keybinds::hint(super::commands::Command::ResetUiScale)
            ))
            .clicked()
        {
            actions.push(Action::UiScale(100));
        }
    });
    if let Some(e) = &drafts[i].2 {
        ui.label(RichText::new(e).small().color(ui.visuals().error_fg_color));
    }
    if now < UI_SCALE_SMALL_BELOW {
        ui.label(
            RichText::new(format!(
                "Below {UI_SCALE_SMALL_BELOW} %, controls may become too small to read or click. {} goes back to 100 %.",
                super::keybinds::key_or_menu(super::commands::Command::ResetUiScale, "Reset to 100 %")
            ))
            .small()
            .color(ui.visuals().warn_fg_color),
        );
    }
    ui.add_space(4.0);
}

/// Checks and stores a typed value: Ok(true) when the setting changed,
/// Ok(false) when it was the same, Err with the sentence (nothing stored).
pub fn apply_text(
    s: &mut Settings,
    set: fn(&mut Settings, &str) -> Result<(), String>,
    text: &str,
) -> Result<bool, String> {
    let mut next = s.clone();
    set(&mut next, text)?;
    let changed = next != *s;
    *s = next;
    Ok(changed)
}

fn ui_accent_ui(
    ui: &mut egui::Ui,
    en: &Entry,
    s: &Settings,
    drafts: &mut Vec<(&'static str, String, Option<String>)>,
    actions: &mut Vec<Action>,
) {
    use crate::settings::{hex_rgb, parse_hex_rgb};
    let mut color = s
        .ui_accent_color
        .unwrap_or(crate::ui::theme::DEFAULT_UI_ACCENT);
    let i = match drafts.iter().position(|d| d.0 == en.label) {
        Some(i) => i,
        None => {
            drafts.push((en.label, hex_rgb(color), None));
            drafts.len() - 1
        }
    };
    ui.label(en.label).on_hover_text(en.tip);
    ui.horizontal_wrapped(|ui| {
        if egui::color_picker::color_edit_button_srgb(ui, &mut color)
            .on_hover_text(en.tip)
            .changed()
        {
            drafts[i].1 = hex_rgb(color);
            drafts[i].2 = None;
            actions.push(Action::UiAccent(Some(color)));
        }
        let r = ui
            .add(
                egui::TextEdit::singleline(&mut drafts[i].1)
                    .id_salt(UI_ACCENT_LABEL)
                    .desired_width(92.0),
            )
            .on_hover_text(en.tip);
        if r.lost_focus() {
            if let Some(c) = parse_hex_rgb(&drafts[i].1) {
                drafts[i].1 = hex_rgb(c);
                drafts[i].2 = None;
                actions.push(Action::UiAccent(Some(c)));
            } else {
                drafts[i].2 = Some("Type a color as #RRGGBB (six hexadecimal digits).".into());
            }
        }
        if ui
            .button("Reset Color")
            .on_hover_text("Restore the original purple in each theme.")
            .clicked()
        {
            drafts[i].1 = hex_rgb(crate::ui::theme::DEFAULT_UI_ACCENT);
            drafts[i].2 = None;
            actions.push(Action::UiAccent(None));
        }
        if s.ui_accent_color.is_none() {
            ui.label(RichText::new("Default plum").small().weak());
        }
    });
    if let Some(e) = &drafts[i].2 {
        ui.label(RichText::new(e).small().color(ui.visuals().error_fg_color));
    }
    ui.add_space(4.0);
}

fn entry_ui(ui: &mut egui::Ui, en: &Entry, s: &Settings, actions: &mut Vec<Action>) {
    match en.item {
        Item::Toggle(on, cmd) => {
            let mut v = on(s);
            if ui
                .checkbox(&mut v, en.label)
                .on_hover_text(en.tip)
                .clicked()
            {
                actions.push(Action::Run(cmd));
            }
        }
        Item::Radio(opts) => {
            ui.label(en.label).on_hover_text(en.tip);
            ui.horizontal_wrapped(|ui| {
                for (label, sel, a) in opts {
                    if ui.radio(sel(s), *label).on_hover_text(en.tip).clicked() && !sel(s) {
                        actions.push(*a);
                    }
                }
            });
        }
        Item::Percent(get, set) => {
            ui.horizontal(|ui| {
                ui.label(en.label).on_hover_text(en.tip);
                let mut v = get(s);
                if ui
                    .add(egui::Slider::new(&mut v, 0..=100).suffix(" %"))
                    .on_hover_text(en.tip)
                    .changed()
                {
                    actions.push(set(v));
                }
            });
        }
        Item::Transparency(get, set) => {
            ui.horizontal(|ui| {
                ui.label(en.label).on_hover_text(en.tip);
                let mut v = get(s);
                if ui
                    .add(egui::Slider::new(&mut v, 0..=100).suffix(" % transparent"))
                    .on_hover_text(en.tip)
                    .changed()
                {
                    actions.push(set(v));
                }
                ui.label(RichText::new(opacity_words(v)).small().weak());
            });
        }
        Item::Open(a) => {
            if ui.button(en.label).on_hover_text(en.tip).clicked() {
                actions.push(a);
            }
        }
        Item::Note(text) => {
            ui.label(RichText::new(text).small().weak())
                .on_hover_text(en.tip);
        }
        Item::Text(..) => {}       // drawn by `text_ui`
        Item::UiScale => {}        // drawn by `ui_scale_ui`
        Item::UiAccent => {}       // drawn by `ui_accent_ui`
        Item::RendererStatus => {} // drawn with the app's renderer state
    }
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::model::PlumDocument;

    /// L-F05 (contract §012): check boxes in a menu toggle without closing
    /// it — two in a row in the same open menu, each applied once — while a
    /// command closes it.
    #[test]
    fn menu_check_boxes_keep_the_menu_open_and_commands_close_it() {
        // 1920 × 1080: the Edit menu's submenu is below the fold at 768.
        let screen = egui::vec2(1920.0, 1080.0);
        let mut h = Harness::new(PlumDocument::new_project(
            "m".into(),
            Default::default(),
            960,
        ));
        h.app_frame(screen, 1.0);
        let find = |h: &mut Harness, text: &str| -> Option<egui::Pos2> {
            let shapes = h.app_frame(screen, 1.0);
            crate::ui::testkit::texts(&shapes)
                .into_iter()
                .find(|t| t.0 == text)
                .map(|t| t.1)
        };
        let click = |h: &mut Harness, at: egui::Pos2| {
            let b = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(at)]);
            h.app_frame_events(screen, 1.0, vec![b(true), b(false)]);
        };
        let view = find(&mut h, "View").unwrap();
        click(&mut h, view);
        let (lane, hide) = (
            h.app.settings.velocity_lane_visible,
            h.app.settings.hide_text_overlays,
        );
        let at = find(&mut h, "Event Graphs").expect("the View menu is open");
        click(&mut h, at);
        assert_eq!(h.app.settings.velocity_lane_visible, !lane, "toggled once");
        let at = find(&mut h, "Hide Text Overlays").expect("still open after a check box");
        click(&mut h, at);
        assert_eq!(h.app.settings.hide_text_overlays, !hide, "toggled once");
        assert_eq!(
            h.app.settings.velocity_lane_visible, !lane,
            "the first kept"
        );
        assert!(find(&mut h, "Follow Playhead").is_some(), "still open");
        // The keyboard: in a freshly opened View menu, Tab k times and Enter;
        // the first k that lands on one of its check boxes toggles it once
        // and the menu stays open (items before it may be commands that
        // close it: the menu is opened again for the next k).
        let key = |k: egui::Key| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let state = |h: &Harness| {
            (
                h.app.settings.velocity_lane_visible,
                h.app.settings.hide_text_overlays,
                h.app.settings.selection_analytics,
                h.app.settings.progressor.enabled,
                h.app.settings.inspector,
            )
        };
        let mut keyboard = None;
        for k in 1..40 {
            h.app.main_view = crate::app::track_list::MainView::TrackList;
            if find(&mut h, "Follow Playhead").is_none() {
                let view = find(&mut h, "View").unwrap();
                click(&mut h, view);
            }
            let before = state(&h);
            for _ in 0..k {
                h.app_frame_events(screen, 1.0, vec![key(egui::Key::Tab)]);
            }
            h.app_frame_events(screen, 1.0, vec![key(egui::Key::Enter)]);
            if state(&h) != before {
                keyboard = Some(find(&mut h, "Follow Playhead").is_some());
                break;
            }
            // Esc closes whatever is open before the next try.
            h.app_frame_events(screen, 1.0, vec![key(egui::Key::Escape)]);
        }
        assert_eq!(
            keyboard,
            Some(true),
            "Tab + Enter toggles a check box, menu open"
        );
        // A command closes the menu.
        let at = find(&mut h, "Fit Song").unwrap();
        click(&mut h, at);
        assert!(
            find(&mut h, "Follow Playhead").is_none(),
            "closed by a command"
        );
        // A submenu's check box (Edit → Note Overlap Prevention) too.
        let edit = find(&mut h, "Edit").unwrap();
        click(&mut h, edit);
        let sub = find(&mut h, "Note Overlap Prevention (Off)").expect("the Edit menu");
        h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(sub)]);
        let on = h.app.settings.overlap.on;
        let label = Command::ToggleOverlapPrevention.label();
        let at = (0..10)
            .find_map(|_| find(&mut h, label))
            .expect("the submenu opens on hover");
        click(&mut h, at);
        assert_eq!(h.app.settings.overlap.on, !on);
        assert!(
            find(&mut h, "Note Overlap Prevention (On)").is_some(),
            "the menu stays open and shows the new state"
        );
    }

    /// L-F04 (contract §011): View holds view actions only; every option it
    /// used to hold is in Settings, once; the View menu fits a 1366 × 768
    /// screen without scrolling, and its last item opens Settings.
    #[test]
    fn the_view_menu_holds_view_actions_only_and_fits_1366_by_768() {
        const MENU: &str = include_str!("../ui/menu.rs");
        let start = MENU.find("ui.menu_button(\"View\"").unwrap();
        let end = start + MENU[start..].find("ui.menu_button(\"Playback\"").unwrap();
        let view = &MENU[start..end];
        let moved: &[(&str, &str)] = &[
            ("Command::Theme", "Theme"),
            ("Command::ToggleHighContrast", "High Contrast"),
            ("UI_SCALE_PRESETS", UI_SCALE_LABEL),
            ("General Color Preferences", "General Color Preferences…"),
            ("Command::ToggleDisplayNoteColors", "Display Note Colors"),
            ("Command::SelectionOpacity", "Selection Opacity"),
            ("Command::ToggleSmoothFollow", "Smooth Follow"),
            ("Command::FollowPosition", "Follow Position…"),
            ("Command::ToggleLockFitRange", "Lock Fit Range"),
            ("Command::ToggleYZoomBeyondKeys", "Y Zoom Beyond the Keys"),
            ("Command::TogglePreview", "Preview Batch Edits"),
            (
                "Command::ToggleProgressorMuteSolo",
                "Respect Track Mute/Solo in Progressor",
            ),
            ("Command::ProgressorSettings", "Progressor Settings…"),
            ("Command::MidiPlayerSettings", "MIDI Player Settings…"),
            (
                "Command::SelectionAnalyticsSettings",
                "Selection Analytics Settings…",
            ),
            ("Command::ToggleLiveKeys", "Live Keyboard Colors"),
            ("Command::ToggleKeyGuide", "88-Key Guide"),
            ("Command::KeyScope", "Keyboard Colors Show"),
            ("Command::ToggleAudition", "Note Audition"),
            ("Command::AuditionLength", "Placed Note Sound Length"),
            ("Backend::", "Renderer"),
            ("GpuPreference", "GPU (Vulkan)"),
            ("Command::ToggleMetrics", "CPU / RAM in Status Bar"),
        ];
        for (token, label) in moved {
            assert!(!view.contains(token), "View still holds {token}");
            let n = entries().iter().filter(|e| e.label == *label).count();
            assert_eq!(n, 1, "{label}: once in Settings");
        }
        // Every section of §011, in order, and each entry in one of them.
        assert_eq!(
            Section::ALL.map(Section::name),
            [
                "Appearance",
                "Interface & Accessibility",
                "Piano Roll",
                "Track List",
                "Playback & External MIDI",
                "Rendering / Onion",
                "Notes / Colors",
                "Import / Export",
                "Performance / Memory",
                "Plugins",
                "Shortcuts",
                "Advanced Diagnostics",
            ]
        );
        for s in Section::ALL {
            assert!(!matching("", s).is_empty(), "{s:?} has entries");
        }
        // The menu, open on a 1366 × 768 screen: every item visible.
        let screen = egui::vec2(1366.0, 768.0);
        let mut h = Harness::new(PlumDocument::new_project(
            "v".into(),
            Default::default(),
            960,
        ));
        h.app_frame(screen, 1.0);
        let shapes = h.app_frame(screen, 1.0);
        let at = crate::ui::testkit::texts(&shapes)
            .into_iter()
            .find(|t| t.0 == "View")
            .expect("the menu bar")
            .1;
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(at)]);
        h.app_frame_events(screen, 1.0, vec![button(true), button(false)]);
        let shapes = h.app_frame(screen, 1.0);
        let mut visible = Vec::new();
        for c in &shapes {
            if let egui::Shape::Text(t) = &c.shape {
                let r = t.galley.rect.translate(t.pos.to_vec2());
                if c.clip_rect.contains_rect(r) && r.bottom() <= screen.y {
                    visible.push(t.galley.text().to_owned());
                }
            }
        }
        for want in [
            "Follow Playhead",
            "Fit Song",
            "Zoom",
            "Reset Piano Roll View",
            "View Settings…",
        ] {
            assert!(
                visible.iter().any(|t| t.starts_with(want)),
                "{want} visible without scrolling: {visible:?}"
            );
        }
        // Its last item opens Settings at Appearance.
        let shapes = h.app_frame(screen, 1.0);
        let link = crate::ui::testkit::texts(&shapes)
            .into_iter()
            .find(|t| t.0 == "View Settings…")
            .unwrap()
            .1;
        let click = |pressed| egui::Event::PointerButton {
            pos: link,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.app_frame_events(screen, 1.0, vec![egui::Event::PointerMoved(link)]);
        h.app_frame_events(screen, 1.0, vec![click(true), click(false)]);
        assert!(h.app.settings_window.open);
        assert_eq!(h.app.settings_window.section, Some(Section::Appearance));
        // What View → Renderer used to show is in Rendering / Onion.
        h.app.settings_window.show_section(Section::Rendering);
        let shapes = h.app_frame(screen, 1.0);
        assert!(
            crate::ui::testkit::texts(&shapes)
                .iter()
                .any(|t| t.0.starts_with("In use: ")),
            "the renderer in use"
        );
    }

    #[test]
    fn accent_hex_entry_validates_commits_and_the_reset_button_restores_plum() {
        use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
        let ctx = egui::Context::default();
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        let en = matching(UI_ACCENT_LABEL, Section::Appearance)[0];
        let mut drafts = Vec::new();
        let mut s = Settings::default();
        let mut draw = |events: Vec<Event>, s: &Settings| {
            let mut actions = Vec::new();
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 200.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui_accent_ui(ui, en, s, &mut drafts, &mut actions);
                    });
                },
            );
            out.textures_delta.clear();
            (actions, crate::ui::testkit::texts(&out.shapes))
        };
        let key = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let pointer = |p, pressed| Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        draw(vec![], &s);
        let (_, labels) = draw(vec![], &s);
        let field = labels.iter().find(|(t, _)| t == "#A378BA").unwrap().1;
        for text in ["#XYZ123", "#00cc88"] {
            draw(
                vec![
                    Event::Key {
                        key: Key::A,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: ctrl,
                    },
                    Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                ],
                &s,
            );
            draw(vec![Event::PointerMoved(field), pointer(field, true)], &s);
            draw(vec![pointer(field, false)], &s);
            let (a, labels) = draw(
                vec![
                    key(Key::A, ctrl),
                    Event::Text(text.into()),
                    key(Key::Enter, Modifiers::NONE),
                ],
                &s,
            );
            if text == "#XYZ123" {
                assert!(a.is_empty(), "invalid color is never applied");
                assert!(
                    labels
                        .iter()
                        .any(|(t, _)| t.contains("six hexadecimal digits"))
                );
            } else {
                assert_eq!(
                    a,
                    [Action::UiAccent(Some([0, 204, 136]))],
                    "shown {labels:?}; focus {:?}",
                    ctx.memory(|m| m.focused())
                );
                s.ui_accent_color = Some([0, 204, 136]);
            }
        }
        let (_, labels) = draw(vec![], &s);
        let reset = labels.iter().find(|(t, _)| t == "Reset Color").unwrap().1;
        draw(vec![Event::PointerMoved(reset), pointer(reset, true)], &s);
        let (a, _) = draw(vec![pointer(reset, false)], &s);
        assert_eq!(a.last(), Some(&Action::UiAccent(None)));
    }

    #[test]
    fn accent_changes_apply_live_persist_and_never_dirty_or_recolor_the_project() {
        use crate::ui::theme;
        use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
        let mut doc = PlumDocument::new_project("accent".into(), Default::default(), 480);
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[NoteRecord::new(
                    0,
                    240,
                    NoteAttrs::new(60, 90, 0, 0, NoteOffKind::NoteOnZero).unwrap(),
                )],
                false,
            )
            .unwrap();
        let mut h = Harness::new(doc);
        let dir =
            std::env::temp_dir().join(format!("plumspirit-accent-action-{}", std::process::id()));
        h.app.config_dir = Some(dir.clone());
        let before = plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap();
        let notes = theme::note_palette(&h.ctx);
        let rev = h.doc().analysis_revision();
        let track_props = h.doc().track(0).unwrap().props().clone();
        let prefs = h.app.settings.note_colors.clone();
        h.settings_action(Action::UiAccent(Some([0, 204, 136])));
        assert_eq!(
            theme::ui_palette(&h.ctx).accent,
            h.ctx.style_of(h.ctx.theme()).visuals.selection.stroke.color
        );
        assert_ne!(
            theme::ui_palette(&h.ctx).accent,
            theme::palette(h.ctx.theme()).accent
        );
        assert_eq!(
            crate::settings::load(Some(&dir)).settings.ui_accent_color,
            Some([0, 204, 136])
        );
        assert_eq!(h.app.settings.note_colors, prefs);
        assert_eq!(theme::note_palette(&h.ctx), notes);
        assert_eq!(
            plumspirit_engine::midi::export_to_bytes(h.doc()).unwrap(),
            before
        );
        assert_eq!(*h.doc().track(0).unwrap().props(), track_props);
        assert_eq!(h.doc().analysis_revision(), rev);
        assert!(!h.app.open.as_ref().unwrap().dirty());
        assert!(h.command(Command::Theme(ThemeSetting::Light)));
        assert_ne!(theme::ui_palette(&h.ctx).accent, theme::LIGHT.accent);
        assert!(h.command(Command::ToggleHighContrast));
        assert_eq!(h.app.settings.ui_accent_color, Some([0, 204, 136]));
        h.settings_reset(Some(Section::Appearance));
        assert_eq!(h.app.settings.ui_accent_color, None);
        assert_eq!(theme::ui_palette(&h.ctx), *theme::palette(h.ctx.theme()));
        assert_eq!(
            crate::settings::load(Some(&dir)).settings.ui_accent_color,
            None
        );
        h.settings_action(Action::UiAccent(Some([20, 60, 240])));
        h.settings_reset(None);
        assert_eq!(h.app.settings.ui_accent_color, None);
    }

    #[test]
    fn search_finds_options_by_label_tooltip_or_section() {
        let by = |q: &str| -> Vec<&str> {
            matching(q, Section::Appearance)
                .iter()
                .map(|e| e.label)
                .collect()
        };
        assert!(by("running status").contains(&"MIDI Export: Use Running Status Compression"));
        assert_eq!(
            by("page turns"),
            ["Smooth Follow", "Inspector Follow Playhead"],
            "found by its tooltip"
        );
        assert!(
            by("piano roll").contains(&"88-Key Guide"),
            "found by section"
        );
        assert!(by("zzz-nothing").is_empty());
        // Without a query a section lists exactly its own entries.
        assert!(
            matching("", Section::Playback)
                .iter()
                .all(|e| e.section == Section::Playback)
        );
        assert!(!matching("", Section::Playback).is_empty());
    }

    #[test]
    fn every_command_and_label_appears_once() {
        let mut cmds: Vec<Command> = Vec::new();
        let mut labels: Vec<&str> = Vec::new();
        for en in entries() {
            assert!(!labels.contains(&en.label), "{} twice", en.label);
            labels.push(en.label);
            if let Item::Toggle(_, c) = en.item {
                assert!(!cmds.contains(&c), "{c:?} twice");
                cmds.push(c);
            }
        }
        for s in Section::ALL {
            assert!(!matching("", s).is_empty(), "{} is empty", s.name());
        }
    }

    /// B1-0059: New Project defaults — validated, stored only when valid,
    /// used by the dialog, reset with the section, persisted.
    #[test]
    fn new_project_defaults_are_validated_and_used_by_the_dialog() {
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        assert_eq!(
            apply_text(&mut h.app.settings, set_new_bpm, "174.5"),
            Ok(true)
        );
        assert_eq!(
            apply_text(&mut h.app.settings, set_new_ppqn, "3,840"),
            Ok(true)
        );
        assert_eq!(
            apply_text(&mut h.app.settings, set_new_ppqn, "3840"),
            Ok(false)
        );
        let e = apply_text(&mut h.app.settings, set_new_bpm, "-3").unwrap_err();
        assert!(!e.is_empty());
        assert!(apply_text(&mut h.app.settings, set_new_ppqn, "0").is_err());
        assert_eq!(
            h.app.settings.new_project_bpm.to_string(),
            "174.5",
            "kept on error"
        );
        assert_eq!(h.app.settings.new_project_ppqn, 3_840);
        h.app.show_new_project_dialog();
        match &h.app.dialog {
            Some(super::super::dialogs::Dialog::NewProject { bpm, ppqn, .. }) => {
                assert_eq!((bpm.as_str(), ppqn.as_str()), ("174.5", "3840"));
            }
            _ => panic!("New Project dialog"),
        }
        h.app.dialog = None;
        assert_eq!(h.settings_reset(Some(Section::ImportExport)), 2);
        assert_eq!(
            h.app.settings.new_project_ppqn,
            crate::settings::DEFAULT_NEW_PPQN
        );
        assert_eq!(
            h.app.settings.new_project_bpm,
            plumspirit_engine::model::Bpm::DEFAULT
        );
    }

    /// B1-0059 batch 2: the key range New Project starts with (dialog and
    /// created project) and the default zoom (validated text, read back as
    /// typed, kept on error), each reset with its own section.
    #[test]
    fn new_project_keys_and_default_zoom_are_validated_settings() {
        use plumspirit_engine::model::project::KeyRange;
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        h.settings_action(Action::NewProjectKeys256(true));
        assert!(h.app.settings.new_project_keys256);
        h.app.show_new_project_dialog();
        match &h.app.dialog {
            Some(super::super::dialogs::Dialog::NewProject { keys256, .. }) => assert!(*keys256),
            _ => panic!("New Project dialog"),
        }
        h.app.dialog = None;
        h.app.create_project("Wide", "120", "960", true).unwrap();
        assert_eq!(h.doc().project().keys, KeyRange::Extended256);
        assert_eq!(h.doc().project().name, "Wide");
        h.app.create_project("Std", "120", "960", false).unwrap();
        assert_eq!(h.doc().project().keys, KeyRange::Standard);
        assert_eq!(
            apply_text(&mut h.app.settings, set_zoom_x, "250 %"),
            Ok(true)
        );
        assert_eq!(h.app.settings.default_zoom_x, 250.0);
        let shown = zoom_text(h.app.settings.default_zoom_x);
        assert_eq!(
            apply_text(&mut h.app.settings, set_zoom_x, &shown),
            Ok(false)
        );
        assert!(apply_text(&mut h.app.settings, set_zoom_x, "0").is_err());
        assert!(apply_text(&mut h.app.settings, set_zoom_x, "abc").is_err());
        // B1-0090: beyond the row-height limit it is clamped, not refused.
        assert!(apply_text(&mut h.app.settings, set_zoom_y, "-5").is_err());
        assert_eq!(apply_text(&mut h.app.settings, set_zoom_y, "1e9"), Ok(true));
        let max = plumspirit_engine::view::MAX_ROW_HEIGHT
            / crate::piano_roll::viewport::DEFAULT_ROW_HEIGHT
            * 100.0;
        assert!((h.app.settings.default_zoom_y - max).abs() < 1e-6);
        assert_eq!(apply_text(&mut h.app.settings, set_zoom_y, "50"), Ok(true));
        assert_eq!(h.app.settings.default_zoom_x, 250.0, "kept on error");
        assert_eq!(h.settings_reset(Some(Section::ImportExport)), 1);
        assert!(!h.app.settings.new_project_keys256);
        assert_eq!(h.settings_reset(Some(Section::PianoRoll)), 2);
        assert_eq!(
            (h.app.settings.default_zoom_x, h.app.settings.default_zoom_y),
            (100.0, 100.0)
        );
    }

    /// B1-0059 batch 4: a new project and an opened MIDI file start with
    /// the Onion layer order from Settings (and are not dirty for it); Hide
    /// Text Overlays is one toggle that keeps the overlays' own settings.
    #[test]
    fn new_projects_take_the_onion_order_and_text_overlays_hide() {
        use plumspirit_engine::model::project::OnionOrder;
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        h.settings_action(Action::NewOnionOrder(OnionOrder::DenseFirst));
        assert_eq!(
            h.app.settings.new_project_onion_order,
            OnionOrder::DenseFirst
        );
        h.app.create_project("A", "120", "960", false).unwrap();
        assert_eq!(h.doc().project().onion.order, OnionOrder::DenseFirst);
        // (A new project is unsaved anyway; the order is part of its
        // starting state, not a pending change.)
        let o = h.app.open.as_ref().unwrap();
        assert_eq!(o.saved_settings.0.order, OnionOrder::DenseFirst);
        // A minimal format-0 file: one note.
        let mut mid = b"MThd\0\0\0\x06\0\0\0\x01\x01\xe0MTrk\0\0\0\x0d".to_vec();
        mid.extend_from_slice(&[0, 0x90, 60, 64, 0x83, 0x60, 0x80, 60, 0, 0, 0xff, 0x2f, 0]);
        let read = plumspirit_engine::midi::import_bytes(&mid).unwrap();
        h.app.finish_import(
            std::path::PathBuf::from("in.mid"),
            None,
            (plumspirit_engine::model::project::KeyRange::Standard, false),
            Ok(read),
        );
        assert_eq!(h.doc().note_count(), 1);
        assert_eq!(h.doc().project().onion.order, OnionOrder::DenseFirst);
        assert!(!h.app.open.as_ref().unwrap().dirty());
        let progressor = h.app.settings.progressor.enabled;
        let analytics = h.app.settings.selection_analytics;
        assert!(h.command(Command::ToggleTextOverlays));
        assert!(h.app.settings.hide_text_overlays);
        assert_eq!(
            (
                h.app.settings.progressor.enabled,
                h.app.settings.selection_analytics
            ),
            (progressor, analytics),
            "the overlays' own settings are kept"
        );
        // L-F03: Hide Text Overlays is in Interface, the order in Rendering.
        assert_eq!(h.settings_reset(Some(Section::Interface)), 1);
        assert_eq!(h.settings_reset(Some(Section::Rendering)), 1);
        assert!(!h.app.settings.hide_text_overlays);
        assert_eq!(h.app.settings.new_project_onion_order, OnionOrder::Project);
    }

    /// B1-0059: the Performance options are validated and reach the tile
    /// renderer the frame after they change; Reset section brings them back.
    #[test]
    fn performance_options_are_validated_and_reset() {
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        assert_eq!(
            apply_text(&mut h.app.settings, set_tile_cache, "96 MB"),
            Ok(true)
        );
        assert_eq!(apply_text(&mut h.app.settings, set_prefetch, "3"), Ok(true));
        assert!(apply_text(&mut h.app.settings, set_tile_cache, "4").is_err());
        assert!(apply_text(&mut h.app.settings, set_prefetch, "9").is_err());
        assert_eq!(
            (h.app.settings.tile_cache_mb, h.app.settings.prefetch_tiles),
            (96, 3)
        );
        let (mb, pf) = (h.app.settings.tile_cache_mb, h.app.settings.prefetch_tiles);
        h.app.roll.dense_mut().configure(mb, pf);
        assert_eq!(h.app.roll.dense_mut().budget(), 96 << 20);
        h.settings_action(Action::Quality(
            crate::piano_roll::dense::RenderQuality::Detailed,
        ));
        assert_eq!(
            h.app.settings.render_quality,
            crate::piano_roll::dense::RenderQuality::Detailed
        );
        // L-F03: the quality is in Rendering / Onion, the cache in Performance.
        assert_eq!(h.settings_reset(Some(Section::Performance)), 2);
        assert_eq!(h.settings_reset(Some(Section::Rendering)), 1);
        assert_eq!(h.app.settings.render_quality, Default::default());
        assert_eq!(
            (h.app.settings.tile_cache_mb, h.app.settings.prefetch_tiles),
            (0, crate::piano_roll::dense::DEFAULT_PREFETCH_TILES)
        );
    }

    /// Every section and a search draw without panicking (all item kinds).
    #[test]
    fn every_section_and_a_search_draw() {
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        assert!(h.command(Command::OpenSettings));
        for s in Section::ALL {
            h.app.settings_window.section = Some(s);
            h.settings_frame();
        }
        h.app.settings_window.search = "follow".into();
        h.settings_frame();
        h.app.settings_window.search = "zzz".into();
        h.settings_frame();
        assert!(h.app.settings_window.open);
        assert_eq!(
            super::super::keybinds::shortcuts_of(Command::OpenSettings),
            ["Ctrl+,"]
        );
    }

    /// Reset section restores the defaults of that section only, through the
    /// same commands as the menus (persisted like any other change).
    #[test]
    fn reset_section_brings_back_defaults_of_that_section_only() {
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        let d = Settings::default();
        assert!(h.command(Command::ToggleFollow)); // Playback
        assert!(h.command(Command::ToggleSmoothFollow)); // Playback
        h.settings_action(Action::InspectorFollow(false));
        assert!(h.command(Command::ToggleMetrics)); // Diagnostics
        assert!(h.command(Command::AuditionLength(800))); // Piano Roll
        assert_ne!(h.app.settings.follow_playhead, d.follow_playhead);
        let n = h.settings_reset(Some(Section::Playback));
        assert_eq!(n, 3);
        assert_eq!(h.app.settings.follow_playhead, d.follow_playhead);
        assert_eq!(h.app.settings.smooth_follow, d.smooth_follow);
        assert!(h.app.settings.inspector_follow_playhead);
        assert_ne!(
            h.app.settings.show_metrics, d.show_metrics,
            "other sections untouched"
        );
        assert_eq!(h.app.settings.audition_ms, 800);
        // Restore all: everything listed is back; a second reset changes nothing.
        h.app.settings.selection_opacity = 40;
        assert!(h.settings_reset(None) >= 3);
        assert_eq!(h.app.settings.show_metrics, d.show_metrics);
        assert_eq!(h.app.settings.audition_ms, d.audition_ms);
        assert_eq!(h.app.settings.selection_opacity, d.selection_opacity);
        assert_eq!(h.settings_reset(None), 0);
    }

    /// B1-0098: the Track List's Clip Body Transparency states both
    /// readings (transparent and opacity), defaults to 80 % transparent =
    /// 20 % opacity, persists through its action and resets with its section.
    #[test]
    fn clip_body_transparency_says_transparent_and_opacity_and_resets() {
        let en = matching("Clip Body Transparency", Section::TrackList);
        let en = en
            .iter()
            .find(|e| e.label == "Clip Body Transparency")
            .expect("entry");
        assert_eq!(en.section, Section::TrackList);
        assert!(
            en.tip.contains("80 % transparent = 20 % opacity"),
            "{}",
            en.tip
        );
        assert!(en.tip.contains("100 % opacity"), "{}", en.tip);
        assert!(matches!(en.item, Item::Transparency(..)));
        assert_eq!(opacity_words(80), "= 20 % opacity");
        assert_eq!(opacity_words(0), "= 100 % opacity");
        assert_eq!(opacity_words(200), "= 0 % opacity");
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        assert_eq!(h.app.settings.clip_body_transparency, 80);
        h.settings_action(Action::ClipTransparency(35));
        assert_eq!(h.app.settings.clip_body_transparency, 35);
        assert_eq!(h.settings_reset(Some(Section::TrackList)), 1);
        assert_eq!(h.app.settings.clip_body_transparency, 80);
    }
}
