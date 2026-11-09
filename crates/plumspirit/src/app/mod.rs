//! Application state and orchestration.
//!
//! # Source of truth
//!
//! The open document (`OpenDoc::doc`, an `Arc<PlumDocument>`) is the only
//! authoritative state. Widgets render it and return intents; this module applies
//! them. Every change to musical content goes through the document's undo
//! [`History`]; mutation needs `Arc::get_mut`, which only succeeds when no
//! background job shares the document, so a job can never race an edit. Playback
//! reads immutable note snapshots, never the live document.
//!
//! The implementation is split by concern: `project` (new / open / save / export),
//! `editing` (note edits, undo/redo, selection), `song` (Conductor, erase,
//! quantize, numeric fields), `graphs` + `curves` + `functions` (the Event
//! Graphs, curve generation, custom functions), `tracks` (Track Properties,
//! Mute / Solo, track end), `timing` (Select Range, loop, speed, Playing Time)
//! and `playback` (transport, SoundFont, Follow Playhead).

pub mod analysis;
mod analytics_charts;
mod archive_open;
mod audio_settings;
mod autobackup;
mod bench;
mod canvas;
mod clip_keys;
mod clip_merge_ui;
mod clip_repeat_ui;
#[cfg(test)]
mod clip_scale;
mod clip_session;
pub mod commands;
#[cfg(test)]
mod context_matrix;
pub mod curves;
mod custom_tools;
mod diagnostics_report;
pub mod dialogs;
mod editing;
pub mod files;
mod folders;
mod functions;
mod graphs;
mod handles;
pub mod help;
pub(crate) mod hierarchy;
mod history_ui;
mod image_midi;
mod inspector_view;
pub(crate) mod jobs;
pub mod keybinds;
pub mod keys;
#[cfg(test)]
mod layout_audit;
mod live;
mod lua_file_dialog;
mod lua_formats;
#[cfg(test)]
mod lua_metadata_tests;
mod lua_midi_input;
mod lua_overlays;
mod lua_panels;
mod lua_playback;
mod lua_plugins_ui;
mod lua_views;
mod musical_clipboard;
mod plugin_commands;
mod reactive_observation;
pub use lua_plugins_ui::MenuPlugin as LuaMenuPlugin;
mod color_presets;
mod merge;
mod note_colors;
pub mod note_menu;
mod onion_defaults;
mod paste_ui;
#[cfg(all(test, windows))]
mod perf_api02_tests;
mod playback;
mod player_view;
pub(crate) mod plugin_memory;
mod plugins;
mod ppqn;
mod preview;
pub mod progressor;
mod project;
mod recent_files;
mod reference;
mod scores;
mod scrub;
mod selection_charts;
mod selection_stats;
mod settings_window;
mod song;
pub(crate) mod spiderweb;
mod subchannels;
pub mod tasks;
#[cfg(test)]
mod test_support;
mod timing;
mod tools;
pub mod track_list;
mod track_ops;
mod tracks;
mod ui_scale;
#[cfg(test)]
mod us_english_audit;
#[cfg(test)]
mod views_matrix;
mod zoom;

pub use autobackup::{
    DEFAULT_INTERVAL_MINUTES as AUTOBACKUP_DEFAULT_MINUTES,
    DEFAULT_KEEP as AUTOBACKUP_DEFAULT_KEEP, parse_folder as parse_autobackup_folder,
    parse_interval as parse_autobackup_interval, parse_keep as parse_autobackup_keep,
};
pub use hierarchy::TreeRow as TreeRowView;

/// Readable BPM: at most three decimals, no trailing zeros ("120", "120.5",
/// "172.125"); the exact value stays in the Conductor.
pub fn format_bpm(v: f64) -> String {
    if !v.is_finite() {
        return "?".into();
    }
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_owned()
    }
}

/// The Conductor BPM shown in the track panel for `tick` (the transport
/// position while playing or paused, the cursor while stopped) and its tooltip.
/// Reads the Conductor's tempo points directly (the same points the tempo map,
/// playback and export use): O(log points) per call, ramps interpolated exactly.
/// Playback Speed is an audition setting and never changes the shown BPM.
pub fn live_bpm(
    doc: &plumspirit_engine::model::PlumDocument,
    tick: u64,
    speed: plumspirit_engine::model::Speed,
) -> (String, String) {
    let c = doc.conductor();
    if !c.timing_editable() {
        return (
            plumspirit_engine::model::conductor::format_tempo(c.start_tempo()),
            c.scope()
                .timing_note()
                .unwrap_or("The tempo comes from the file's timing.")
                .to_owned(),
        );
    }
    let b = format_bpm(c.bpm_at(tick));
    let mut tip = format!("Project BPM: {b} at the current position.");
    if !speed.is_normal() {
        tip.push_str(&format!(
            " Playback Speed: {speed} (audition only; the project's BPM is unchanged)."
        ));
    }
    (b, tip)
}

use crate::audio::{Transport, TransportState};
use crate::diagnostics::metrics::Sampler;
use crate::diagnostics::{self, format_bytes, format_duration};
use crate::piano_roll::graph::{CurvePreview, GraphHost, HostInput};
use crate::piano_roll::render::OnionSource;
use crate::piano_roll::velocity;
use crate::piano_roll::viewport::FollowStyle;
use crate::piano_roll::{PianoRoll, RollInput};
use crate::settings::{self, Settings, ThemeSetting};
use crate::ui::fonts::{UiFont, UiFontKind};
use crate::ui::status_bar::{StatusInput, StatusKind};
use crate::ui::theme;
use crate::ui::toolbar::NumericField;
use crate::ui::{
    empty_state, graph_bar, menu, onion_chooser, status_bar, toolbar, track_panel, transport_bar,
};
use commands::Command;
use dialogs::{Dialog, ScaleAxis};
use egui::{Event, Key, KeyboardShortcut, ViewportCommand};
use jobs::{JobKind, RunningJob, Then};
use plumspirit_engine::editor::Clipboard;
use plumspirit_engine::editor::expr::Expr;
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::duration::{TimeBasis, TimeMap};
use plumspirit_engine::model::{
    GraphTarget, OnionConfig, OnionMode, PlumDocument, SoundFontRef, TempoMap, TrackId,
};
use plumspirit_engine::view::grid::MeterMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Where the open document came from, which decides what Save does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocSource {
    /// Created with New Project; Save asks for a `.lup` destination.
    New,
    /// Imported MIDI file; Save asks for a `.lup` destination (the MIDI file is
    /// never overwritten by Save).
    Midi(PathBuf),
    /// A native project; Save writes back to it.
    Project(PathBuf),
}

pub struct OpenDoc {
    pub doc: Arc<PlumDocument>,
    pub source: DocSource,
    pub history: History,
    /// Project settings outside the undo history (Onion Skin, SoundFont; mute /
    /// solo, loop and graph slots as a key) as last saved or opened, for the
    /// unsaved-changes check.
    saved_settings: (OnionConfig, Option<SoundFontRef>, u64),
    pub active_track: usize,
    /// The Conductor is selected in the track list (tempo, time signatures and
    /// markers are edited; the active track's notes are shown for orientation).
    pub conductor_selected: bool,
    /// v0.9 V09-S11: the clip open in the Piano Roll (`app/clip_session.rs`).
    pub clip_session: Option<clip_session::ClipSession>,
    /// v0.9 V09-S12 (§218): Track List clip selections remembered per
    /// history state (a merge: before → its sources, after → the merged
    /// clip), restored by Undo / Redo (`app/clip_merge_ui.rs`).
    pub clip_selections: Vec<(
        plumspirit_engine::history::StateId,
        Vec<plumspirit_engine::model::ClipId>,
    )>,
    pub generation: u64,
    /// v0.8: history identity of the project file at the saved state (the
    /// journal's checkpoint), when known.
    pub saved_identity: Option<u64>,
}

impl OpenDoc {
    fn new(
        doc: PlumDocument,
        source: DocSource,
        limits: HistoryLimits,
        saved: bool,
        generation: u64,
    ) -> Self {
        let mut history = History::new(limits);
        history.reset(saved);
        let saved_settings = (
            doc.project().onion.clone(),
            doc.project().soundfont.clone(),
            settings_key(&doc),
        );
        // Start on the first track that has notes (Format 1 files usually begin with
        // a note-less conductor track).
        let active_track = doc
            .tracks()
            .iter()
            .position(|t| !t.notes.is_empty())
            .unwrap_or(0);
        OpenDoc {
            doc: Arc::new(doc),
            source,
            history,
            saved_settings,
            active_track,
            conductor_selected: false,
            clip_session: None,
            clip_selections: Vec::new(),
            generation,
            saved_identity: None,
        }
    }

    /// The document differs from its last saved (or opened) state.
    pub fn dirty(&self) -> bool {
        let p = self.doc.project();
        !self.history.is_at_saved_state()
            || self.saved_settings.0 != p.onion
            || self.saved_settings.1 != p.soundfont
            || self.saved_settings.2 != settings_key(&self.doc)
    }

    /// What "Don't Save" means for this document (v0.8): edits stay in the
    /// persistent Undo History when its journal is working (they can be redone
    /// after reopening), project settings outside the history are lost.
    pub fn discard_note(&self) -> &'static str {
        let p = self.doc.project();
        let settings = self.saved_settings.0 != p.onion
            || self.saved_settings.1 != p.soundfont
            || self.saved_settings.2 != settings_key(&self.doc);
        let kept = matches!(self.source, DocSource::Project(_))
            && !self.history.is_at_saved_state()
            && self.history.journal().is_some_and(|j| j.failures().0 == 0);
        match (kept, settings) {
            (true, false) => {
                "If you don't save, the file keeps its saved state; your edits stay in its Undo History (Edit > Undo History…) and can be redone after reopening it."
            }
            (true, true) => {
                "If you don't save, the file keeps its saved state; your edits stay in its Undo History (Edit > Undo History…) and can be redone after reopening it. Changed Onion Skin, loop, Mute / Solo or SoundFont settings are lost."
            }
            _ => "Your changes will be lost if you don't save them.",
        }
    }

    fn mark_saved(&mut self, state: plumspirit_engine::history::StateId) {
        self.history.mark_saved(state);
        let p = self.doc.project();
        self.saved_settings = (
            p.onion.clone(),
            p.soundfont.clone(),
            settings_key(&self.doc),
        );
    }

    pub fn name(&self) -> &str {
        &self.doc.project().name
    }
}

/// v0.9 §081 / §225: the main window's title — the open file's name with
/// its extension (`song.mid` and `song.lup` stay apart; Save As renames it at
/// once), the project name for a new project never saved, `* ` while there
/// are unsaved changes; the release name alone on the start screen.
fn window_title(open: Option<(&DocSource, &str, bool)>) -> String {
    let Some((source, name, dirty)) = open else {
        return crate::RELEASE_NAME.to_owned();
    };
    let what = match source {
        DocSource::Project(p) | DocSource::Midi(p) => files::display_name(p),
        DocSource::New if name.trim().is_empty() => {
            plumspirit_engine::model::project::DEFAULT_PROJECT_NAME.to_owned()
        }
        DocSource::New => name.to_owned(),
    };
    let star = if dirty { "* " } else { "" };
    format!("{star}{what} — {}", crate::RELEASE_NAME)
}

/// Project playback / view settings kept outside the undo history (mute, solo,
/// loop, Event Graph slots): a key that changes when any of them does.
fn settings_key(doc: &PlumDocument) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in doc.tracks() {
        (t.id(), t.mute, t.solo).hash(&mut h);
    }
    doc.project().looping.hash(&mut h);
    doc.project().graphs.hash(&mut h);
    doc.project().zoom_regions.hash(&mut h);
    doc.project().reference.hash(&mut h);
    doc.project().folders_collapsed.hash(&mut h);
    doc.project().plugin_data.hash(&mut h);
    h.finish()
}

struct Status {
    text: String,
    kind: StatusKind,
    details: Vec<String>,
}

/// Everything prepared before the window exists.
pub struct Boot {
    pub config_dir: Option<PathBuf>,
    pub settings: Settings,
    pub font: UiFont,
    pub startup_file: Option<PathBuf>,
    pub dev_overlay: bool,
    /// v0.8: the renderer of this run and its fallback notices.
    pub renderer: crate::renderer::RunInfo,
    /// v0.8: `--bench-render N`.
    pub bench_frames: Option<usize>,
}

pub struct PlumSpiritApp {
    /// v0.8: what the running renderer is (Diagnostics), this run's
    /// renderer and notices, frames drawn so far (the Vulkan start marker is
    /// removed once a frame has been drawn).
    renderer_info: crate::renderer::RendererInfo,
    renderer_run: crate::renderer::RunInfo,
    renderer_frames: u8,
    bench: Option<bench::RenderBench>,
    /// v0.8 Playback → Audio Settings window.
    audio_window: audio_settings::AudioWindow,
    /// v0.9 View → Note Color Preferences.
    note_color_window: note_colors::NoteColorWindow,
    config_dir: Option<PathBuf>,
    settings: Settings,
    font: UiFont,
    open: Option<OpenDoc>,
    project_releases: Vec<project::ReleaseTask>,
    /// v1.0-beta1 (B1-0059): automatic backups of the open project.
    autobackup: autobackup::AutoBackup,
    clipboard: Option<Clipboard>,
    /// B1-0117: what the Note Overlap Prevention did on the last prepared
    /// edit (Paste words its own status line with it).
    last_overlap: Option<plumspirit_engine::transform::overlap::Report>,
    roll: PianoRoll,
    /// v0.9 (V09-S08): which view the central area shows, for which
    /// document generation (every project opens in the Track List).
    main_view: track_list::MainView,
    main_view_generation: Option<u64>,
    /// v0.9 (V09-S08): the Track List (the primary window), where the left
    /// panel drew its rows last frame, and the list scroll it asked for.
    track_list: track_list::TrackList,
    row_geometry: track_panel::RowGeometry,
    track_list_scroll: Option<f32>,
    graphs: GraphHost,
    /// The Event Graph (0 or 1) that receives drawing, selection and Delete.
    active_graph: usize,
    /// The graphs were used last (Delete / Copy / Paste act on the graph
    /// selection instead of the notes).
    graph_focus: bool,
    event_list_open: bool,
    /// The chosen custom function's text and parse result.
    custom_expr: Option<(String, Result<Expr, String>)>,
    event_clipboard: Option<graphs::EventClipboard>,
    clipboard_ctx: egui::Context,
    clipboard_backend: musical_clipboard::Backend,
    clipboard_origin: Option<(u32, u64)>,
    /// Tracks Ctrl+clicked in the track list (Select Range scope).
    selected_tracks: BTreeSet<TrackId>,
    /// v0.8 §081–§082: THE time selection `[start, end)` — set by a drag on
    /// the ruler or by Select Range; read by Select Range (prefill), Loop
    /// Selected Time, tool windows ("Select its notes"), Playing Time and the
    /// ruler menu. No other range state exists.
    time_selection: Option<(u64, u64)>,
    time_cache: Option<(TimeKey, Arc<TimeMap>)>,
    /// v0.8 Open File History (`recent.json`), which entries are missing and
    /// when that was last checked.
    recent: crate::recent::RecentFiles,
    recent_missing: Vec<bool>,
    recent_checked: Option<std::time::Instant>,
    /// v0.8: the folder being renamed in the track list, with the typed name.
    folder_rename: Option<(u32, String)>,
    /// B1-0187: the Diagnostics Report preview window.
    diag_window: Option<diagnostics_report::DiagWindow>,
    /// v0.8 Undo History window and its disk-usage cache.
    history_window: bool,
    history_confirm_clear: bool,
    /// v0.8 §209: Compact History running on a worker.
    history_compaction: Option<history_ui::Compaction>,
    /// v0.8 §216: (document generation, journal write failures) last reported.
    journal_failures_seen: (u64, u64),
    history_disk_all: u64,
    history_disk_checked: Option<std::time::Instant>,
    /// CPU / RAM sampler, running while the status bar shows it.
    metrics: Option<Sampler>,
    /// v1.0-beta1 (B1-0024): every Lua host the app runs scripts in;
    /// closing PlumSpirit cancels them without waiting.
    scripts: crate::lua::Scripts,
    /// B1-0037 / B1-0038: the Lua plugin manager (created after the first
    /// frames), whether it was started, the document generation plugins
    /// last saw (project open / close), and the Lua Plugins window.
    lua_plugins: Option<crate::lua::plugins::PluginManager>,
    lua_plugins_started: bool,
    lua_plugins_generation: Option<u64>,
    lua_observation: reactive_observation::ObservationState,
    lua_playback: lua_playback::Sampling,
    lua_playback_controls: crate::lua::playback::control::Admission,
    lua_views: crate::lua::visual::Capture,
    lua_automation: crate::lua::response::Gate,
    /// B1-0035: plugin project data waiting for the document
    /// (generation, plugin id, JSON; empty JSON removes it).
    lua_project_data: Vec<(u64, String, String)>,
    lua_plugins_window: lua_plugins_ui::LuaPluginsWindow,
    lua_file_dialog: Option<lua_file_dialog::NativeFileDialog>,
    lua_formats: lua_formats::State,
    lua_midi_input: lua_midi_input::State,
    lua_panels: Vec<lua_panels::View>,
    lua_overlays: lua_overlays::Overlays,
    lua_upload_pixels: usize,
    lua_upload_frame: Option<u64>,
    /// B1-0050: the Help window (F1).
    pub(crate) help: help::HelpWindow,
    /// B1-0045: the open custom tool window (Tools → Custom).
    custom_tool: Option<custom_tools::CustomToolSession>,
    /// B1-0034: a plugin command's options dialog and its last options.
    lua_command_dialog: Option<plugin_commands::CommandDialog>,
    lua_command_memory: plugin_commands::CommandMemory,
    /// The theme last frame (tool runs started outside a frame use it).
    dark_mode: bool,
    speed_field: NumericField,
    graph2_drag_start: Option<f32>,
    /// Settings changed in a menu: saved at this time (debounced).
    settings_save_at: Option<std::time::Instant>,
    /// Export applies the track corrections (chosen in the export dialog).
    export_options: plumspirit_engine::midi::ExportOptions,
    /// Follow Playhead is on but paused: the user scrolled during playback.
    follow_suspended: bool,
    seek_follow_view: Option<(plumspirit_engine::view::TimeAxis, i128)>,
    zoom_x_field: NumericField,
    zoom_y_field: NumericField,
    gate_field: NumericField,
    velocity_field: NumericField,
    custom_snap_field: NumericField,
    /// (track, notes revision) → selection summary for the Gate / Vel fields.
    summary: Option<(
        (plumspirit_engine::model::ContentRef, u64),
        song::SelectionSummary,
    )>,
    meter: Option<((u64, u64, usize, u64), MeterMap)>,
    display_tempo: Option<((u64, u64, u64), Arc<TempoMap>)>,
    job: Option<RunningJob>,
    retired: Vec<JoinHandle<()>>,
    /// L-D02: the unpacked archive the open MIDI file is chosen from (owns
    /// the temporary spool; `archive_open.rs`).
    archive_session: Option<archive_open::ArchiveSession>,
    next_job_id: u64,
    next_generation: u64,
    dialog: Option<Dialog>,
    status: Option<Status>,
    allowed_to_close: bool,
    close_after_job: bool,
    title: String,
    dev_overlay: bool,
    /// B1-0175: the official logo and wordmark, decoded on first use.
    brand: crate::assets::BrandTextures,
    last_dir: Option<PathBuf>,
    last_export: Option<PathBuf>,
    startup_file: Option<PathBuf>,
    window_size: Option<[f32; 2]>,
    /// B1-0064: the Onion mode Onion Skin On / Off brings back (this session).
    onion_last_mode: OnionMode,
    /// B1-0064: the active track the Per-Track Settings last saw.
    onion_track_seen: Option<TrackId>,
    /// B1-0065: where the Progressor (0) and Selection Analytics (1)
    /// readouts were drawn this frame.
    overlay_views: [Option<progressor::OverlayView>; 2],
    /// L-F01: where the Selection Analytics readout was last drawn (Detailed
    /// Hover: Alt held over it shows every line).
    sel_readout_last: Option<egui::Rect>,
    roll_width: f32,
    /// A Ctrl+V `Paste` event arrived and its key release is still pending.
    paste_event_seen: bool,
    /// A widget had the keyboard when this frame began (v0.9, S08 GUI
    /// journey): the keys of this frame are its own even if it lets the
    /// focus go during the frame — the Enter that commits a text field (a
    /// folder name, the tempo, …) must not also open the selected clip.
    focus_at_frame_start: bool,
    /// Recent frame timings for the developer overlay.
    perf: diagnostics::FramePerf,
    history_limits: HistoryLimits,
    /// L-H04: the low-memory note of the project just opened (taken by the
    /// opening's status, `project::low_memory_limits`).
    low_memory: Option<String>,
    transport: Transport,
    /// SoundFont the user chose that becomes the project's once it loads.
    pending_soundfont: Option<PathBuf>,
    /// Text of the tempo field while it is being edited.
    tempo_text: String,
    tempo_editing: bool,
    /// An edit waiting for "Apply Without Undo".
    pending_operation: Option<Box<Operation>>,
    /// B1-0103: the Change PPQN Preview, kept for Apply.
    ppqn_prepared: Option<ppqn::PreparedPpqn>,
    /// B1-0097: clips copied in the Track List (Copy / Cut).
    clip_clipboard: Option<plumspirit_engine::editor::clips::ClipClipboard>,
    /// The export the user saw (and possibly approved), waiting for the
    /// destination (v0.9: one snapshot is shown, written and validated).
    pending_export: Option<std::sync::Arc<plumspirit_engine::midi::ExportSnapshot>>,
    /// v1.0-beta1 (B1-0121): the pending export is a strict one from the
    /// source file, with these status bytes.
    export_strict: Option<plumspirit_engine::midi::strict::StatusBytes>,
    /// Why the pending export is written from the project's contents (B1-0122).
    export_note: Option<String>,
    /// The source the strict writer refused (changed or unreadable): exports
    /// of this source are written from the contents (B1-0121).
    strict_refused: Option<plumspirit_engine::model::project::SourceProvenance>,
    /// Onion Skin chooser open with this selection.
    onion_choice: Option<Vec<TrackId>>,
    /// L-F06: the open project's Onion Skin as last seen (generation and
    /// the remembered fields): a change after it is the user's choice.
    onion_seen: Option<(u64, onion_defaults::OnionSeen)>,
    lane_drag_start: Option<f32>,
    // ------------------------------------------------------------ v0.5
    /// How long significant operations took (bottom status bar).
    tasks: tasks::TaskLog,
    /// v0.8 §210: a Pre-Render start buffer being filled (Play / seek while
    /// playing in Pre-Render): when it started and whether it was seen
    /// filling (the engine resets the flag on its own thread).
    preroll_watch: Option<(std::time::Instant, bool)>,
    /// Subchannel that receives new notes, per track (view state, not saved).
    active_subs: std::collections::HashMap<TrackId, plumspirit_engine::model::SubchannelId>,
    /// Collapsed tracks in the track tree (view state only: never affects
    /// playback, export or the document).
    collapsed: std::collections::HashSet<TrackId>,
    /// Visible track-tree rows, cached per (structure revision, collapse state).
    tree_rows: hierarchy::TreeCache,
    note_counts: hierarchy::NoteCountCache,
    /// Live Preview of the open Scale / Quantize dialog.
    preview: Option<preview::PreviewState>,
    /// Import MIDI Into Track in progress (source loaded, options chosen).
    merge: Option<merge::MergeState>,
    /// Onion Skin layer order (Dense First), cached per view and content.
    onion_order: hierarchy::OnionOrderCache,
    /// The Subchannels window is open.
    subchannel_window: bool,
    /// Text of the "go to subchannel number" field.
    sub_goto: String,
    /// Note counts per subchannel for the Subchannels window, per revision.
    sub_counts: Option<(
        (plumspirit_engine::model::ContentRef, u64),
        std::collections::HashMap<u32, usize>,
    )>,
    /// Subchannels window: search text, filtered rows (cached), Move option.
    sub_filter: String,
    sub_filtered: Option<((TrackId, u64, String), Vec<usize>)>,
    sub_move_sets_channel: bool,
    /// V09-S11: the opened clip's resolved target, cached per document
    /// state (`app/clip_session.rs`; read many times a frame).
    session_cache: std::cell::Cell<Option<clip_session::SessionCache>>,
    /// Kind of dialog shown last frame (Enter is ignored on a dialog's first frame).
    dialog_seen: Option<std::mem::Discriminant<Dialog>>,
    /// Frames the current dialog has been shown (its layout settles over the
    /// first few frames: they are repainted at once so nothing jumps under
    /// the pointer).
    dialog_frames: u8,
    // ------------------------------------------------------------ v0.6
    /// Project analytics (Progressor and friends).
    analysis: analysis::AnalysisService,
    /// v0.7 selection analytics (View → Selection Analytics, F6).
    sel_stats: selection_stats::SelectionStats,
    /// L-E01 … L-E08: the selection charts (the advanced section).
    sel_charts: selection_charts::SelectionCharts,
    /// Live keyboard colours.
    keys: keys::KeyboardTracker,
    /// v1.0-beta1 B1-0110: the roll keyboard's one colour per key.
    roll_keys: keys::RollKeys,
    /// Parsed Progressor template (re-parsed when the text changes).
    progressor_template: Option<(String, progressor::Template)>,
    /// The Progressor Settings window is open (with the settings to restore
    /// on Cancel).
    progressor_window: Option<progressor::SettingsWindow>,
    /// v0.9 (V09-S17): the MIDI Player's snapshot, frame rate and template.
    player: crate::player::PlayerView,
    /// v0.9 (V09-S18): the MIDI Inspector's state while it is shown.
    inspector: crate::inspector::Inspector,
    /// The MIDI Player Settings window is open (with the settings to restore
    /// on Cancel).
    player_window: Option<crate::player::PlayerSettings>,
    /// v0.8: Selection Analytics Settings window open (the template and the
    /// on / off state when it opened; Cancel restores them).
    selection_window: Option<(String, bool, bool)>,
    /// Audition note held by a piano-key press: (channel, key).
    audition_held: Option<(u8, u8)>,
    /// "No SoundFont for audition" was already said (once per project).
    audition_warned: bool,
    /// The window had focus last frame (a held audition ends when it is lost).
    audition_focus: bool,
    /// (pitch, tick, length) of an audition waiting for the SoundFont to load.
    audition_pending: Option<(u8, u64, Option<std::time::Duration>)>,
    // ------------------------------------------------------------ v0.7
    /// The open transformation tool window.
    tool: Option<tools::ToolSession>,
    /// The last tool window opened (F5 reopens it).
    last_tool: Option<&'static str>,
    /// v0.8 §196–§198: what Repeat Last Tool runs again (a recipe).
    last_recipe: Option<tools::Recipe>,
    /// v0.8 §157: tools used recently (newest first) for the note menu.
    recent_tools: Vec<&'static str>,
    /// v0.9 V09-S13 / S15: the Track List Paste Repeat dialog's last
    /// fields, Linked choice and functions.
    clip_repeat_memory: clip_repeat_ui::RepeatMemory,
    /// The Zoom tool (window, history, lens, animation).
    zoom: zoom::ZoomTool,
    /// Play Selected / Scrub (window, playback plan, A/B markers).
    scrub: scrub::Scrub,
    /// The Keyboard Shortcuts window is open, and its filter text.
    keybinds_window: bool,
    /// v1.0-beta1 (B1-0058): the Settings window.
    settings_window: settings_window::SettingsWindow,
    /// v0.8 Image to MIDI plugin window.
    image_midi: image_midi::ImageMidi,
    /// v0.8 Audio Reference window and recording.
    reference_win: reference::ReferenceWin,
    spiderweb: spiderweb::Spiderweb,
    /// Help → About PlumSpirit and Built-in Plugins.
    about_window: bool,
    keybinds_filter: String,
    /// B1-0060: the keybind editor's state (capture, conflict, message).
    keybind_edit: keybinds::EditState,
    /// The Insert Score window (a `.psc` being placed).
    score_insert: Option<scores::ScoreInsert>,
    /// Transient preview of scale handles / Scale Y Fill (v0.7).
    transient: Option<handles::Transient>,
    /// v0.8: (Image to MIDI, Spiderweb, plugin tool) windows open last frame
    /// (closing one saves the settings, `plugin_memory`).
    plugin_windows_open: (bool, bool, bool),
    /// Last settings per tool in this session (plugin tools' are saved in
    /// settings.json, `plugin_memory`).
    tool_memory:
        std::collections::HashMap<&'static str, (plumspirit_engine::transform::ParamSet, usize)>,
    /// V09-S39: Select's stored selection slots 1 – 9 (this session; Select
    /// n10 stores and combines them).
    selection_slots: std::sync::Arc<Vec<Vec<plumspirit_engine::transform::SlotNote>>>,
}

impl PlumSpiritApp {
    pub fn new(cc: &eframe::CreationContext<'_>, boot: Boot) -> Self {
        let ctx = &cc.egui_ctx;
        // L-S49: times each pass for the F12 overlay's frame split.
        ctx.add_plugin(crate::diagnostics::FrameClock::default());
        // egui's built-in Ctrl+Plus / Ctrl+Minus / Ctrl+0 interface zoom is
        // off: it is not in the keybind table (Help → Keyboard Shortcuts), is
        // not saved, and Ctrl+0 is PlumSpirit's Reset Zoom for the Piano Roll.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        crate::ui::fonts::install(ctx, &boot.font);
        theme::apply(
            ctx,
            boot.settings.theme,
            boot.settings.high_contrast,
            boot.settings.ui_accent_color,
        );
        // B1-0062: the UI scale before the first frame.
        ctx.set_zoom_factor(ui_scale::zoom_of(boot.settings.ui_scale_percent));
        // B1-0060: the user's keybinds over the defaults.
        for p in keybinds::set_overrides(&boot.settings.keybinds) {
            log::warn!("keybinds: {p}");
        }
        // v0.9 §064–§068: the note palette from the preferences.
        theme::set_note_palette(
            ctx,
            std::sync::Arc::new(note_colors::palette_of(&boot.settings)),
        );
        log::info!("theme preference: {}", boot.settings.theme.as_str());
        match boot.font.kind {
            UiFontKind::Helvetica => log::info!("UI font: {}", boot.font.describe()),
            _ => log::info!(
                "Helvetica is not installed; fallback typography in use: {}",
                boot.font.describe()
            ),
        }
        let mut history_limits =
            HistoryLimits::for_system_memory(crate::platform::total_memory_bytes());
        // Developer/testing override of the undo memory budget (docs/TESTING.md).
        if let Some(mb) = std::env::var("PLUMSPIRIT_UNDO_BUDGET_MB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            history_limits.max_bytes = mb.clamp(1, 1 << 20) * 1024 * 1024;
        }
        let boot_speed = boot.settings.speed;
        log::info!(
            "undo history budget: {} ({} entries max)",
            format_bytes(history_limits.max_bytes as u64),
            history_limits.max_entries
        );
        if let Some(dir) = boot.config_dir.as_deref() {
            history_ui::remove_untitled(dir);
        }
        let (recent_files, recent_problems) = crate::recent::load(boot.config_dir.as_deref());
        for p in &recent_problems {
            log::warn!("recent files: {p}");
        }
        let renderer_info = crate::renderer::RendererInfo::from_creation(cc);
        log::info!("renderer in use: {}", renderer_info.line());
        let mut renderer_run = boot.renderer.clone();
        if let Some(n) = crate::renderer::take_adapter_note() {
            renderer_run.notices.push(n);
        }
        let mut app = PlumSpiritApp {
            renderer_info,
            renderer_run,
            renderer_frames: 0,
            bench: boot.bench_frames.map(bench::RenderBench::new),
            audio_window: Default::default(),
            note_color_window: Default::default(),
            config_dir: boot.config_dir,
            settings: boot.settings,
            font: boot.font,
            open: None,
            project_releases: Vec::new(),
            autobackup: Default::default(),
            clipboard: None,
            last_overlap: None,
            roll: PianoRoll::default(),
            main_view: track_list::MainView::default(),
            main_view_generation: None,
            track_list: track_list::TrackList::default(),
            row_geometry: track_panel::RowGeometry::default(),
            track_list_scroll: None,
            graphs: GraphHost::default(),
            active_graph: 0,
            graph_focus: false,
            event_list_open: false,
            custom_expr: None,
            event_clipboard: None,
            clipboard_ctx: ctx.clone(),
            clipboard_backend: musical_clipboard::Backend::default(),
            clipboard_origin: None,
            selected_tracks: BTreeSet::new(),
            time_selection: None,
            time_cache: None,
            recent: recent_files,
            recent_missing: Vec::new(),
            recent_checked: None,
            folder_rename: None,
            diag_window: None,
            history_window: false,
            history_confirm_clear: false,
            history_compaction: None,
            journal_failures_seen: (0, 0),
            history_disk_all: 0,
            history_disk_checked: None,
            metrics: None,
            scripts: crate::lua::Scripts::default(),
            lua_plugins: None,
            lua_plugins_started: false,
            lua_plugins_generation: None,
            lua_observation: Default::default(),
            lua_playback: Default::default(),
            lua_playback_controls: Default::default(),
            lua_views: Default::default(),
            lua_automation: Default::default(),
            lua_project_data: Vec::new(),
            lua_plugins_window: lua_plugins_ui::LuaPluginsWindow::default(),
            lua_file_dialog: None,
            lua_formats: Default::default(),
            lua_midi_input: Default::default(),
            lua_panels: Vec::new(),
            lua_overlays: Default::default(),
            lua_upload_pixels: crate::lua::ui::images::MAX_UPLOAD_PIXELS,
            lua_upload_frame: None,
            help: help::HelpWindow::default(),
            custom_tool: None,
            lua_command_dialog: None,
            lua_command_memory: Default::default(),
            dark_mode: true,
            speed_field: NumericField::default(),
            dialog_seen: None,
            dialog_frames: 0,
            analysis: analysis::AnalysisService::default(),
            sel_stats: selection_stats::SelectionStats::default(),
            sel_charts: selection_charts::SelectionCharts::default(),
            keys: keys::KeyboardTracker::default(),
            roll_keys: keys::RollKeys::default(),
            progressor_template: None,
            progressor_window: None,
            player: crate::player::PlayerView::default(),
            inspector: crate::inspector::Inspector::default(),
            player_window: None,
            selection_window: None,
            audition_held: None,
            audition_warned: false,
            audition_focus: true,
            audition_pending: None,
            tool: None,
            last_tool: None,
            last_recipe: None,
            recent_tools: Vec::new(),
            clip_repeat_memory: Default::default(),
            zoom: zoom::ZoomTool::new(),
            scrub: scrub::Scrub::default(),
            keybinds_window: false,
            settings_window: Default::default(),
            image_midi: Default::default(),
            reference_win: Default::default(),
            spiderweb: Default::default(),
            about_window: false,
            keybinds_filter: String::new(),
            keybind_edit: Default::default(),
            tool_memory: Default::default(),
            selection_slots: std::sync::Arc::new(vec![Vec::new(); 9]),
            plugin_windows_open: (false, false, false),
            score_insert: None,
            transient: None,
            graph2_drag_start: None,
            settings_save_at: None,
            export_options: plumspirit_engine::midi::ExportOptions::default(),
            follow_suspended: false,
            seek_follow_view: None,
            zoom_x_field: NumericField::default(),
            zoom_y_field: NumericField::default(),
            gate_field: NumericField::default(),
            velocity_field: NumericField::default(),
            custom_snap_field: NumericField::default(),
            summary: None,
            meter: None,
            display_tempo: None,
            job: None,
            retired: Vec::new(),
            archive_session: None,
            next_job_id: 1,
            next_generation: 1,
            dialog: None,
            status: None,
            allowed_to_close: false,
            close_after_job: false,
            title: String::new(),
            dev_overlay: boot.dev_overlay,
            brand: crate::assets::BrandTextures::default(),
            last_dir: None,
            last_export: None,
            startup_file: boot.startup_file,
            window_size: None,
            onion_last_mode: OnionMode::Off,
            onion_track_seen: None,
            overlay_views: [None, None],
            sel_readout_last: None,
            roll_width: 800.0,
            paste_event_seen: false,
            focus_at_frame_start: false,
            perf: diagnostics::FramePerf::default(),
            history_limits,
            low_memory: None,
            transport: {
                let mut t = Transport::default();
                let d = PlumDocument::new_empty(plumspirit_engine::model::Division::ppqn(480));
                let _ = t.set_speed(boot_speed, &d, 0);
                t
            },
            pending_soundfont: None,
            tempo_text: String::new(),
            tempo_editing: false,
            pending_operation: None,
            ppqn_prepared: None,
            clip_clipboard: None,
            pending_export: None,
            export_strict: None,
            export_note: None,
            strict_refused: None,
            onion_choice: None,
            onion_seen: None,
            lane_drag_start: None,
            tasks: tasks::TaskLog::default(),
            preroll_watch: None,
            active_subs: std::collections::HashMap::new(),
            collapsed: std::collections::HashSet::new(),
            tree_rows: hierarchy::TreeCache::default(),
            note_counts: hierarchy::NoteCountCache::default(),
            preview: None,
            merge: None,
            onion_order: hierarchy::OnionOrderCache::default(),
            subchannel_window: false,
            sub_goto: String::new(),
            sub_counts: None,
            sub_filter: String::new(),
            sub_filtered: None,
            sub_move_sets_channel: true,
            session_cache: std::cell::Cell::new(None),
        };
        app.apply_audio_settings();
        app.apply_playhead_policy();
        app.restore_plugin_memory();
        app
    }

    // ------------------------------------------------------------ state helpers

    fn doc(&self) -> Option<&PlumDocument> {
        self.open.as_ref().map(|o| &*o.doc)
    }

    /// Mutable document access for changes outside the undo history (selection,
    /// Onion Skin, SoundFont association); `None` while a job shares the document.
    fn doc_mut(&mut self) -> Option<&mut PlumDocument> {
        if self.job.is_some() {
            return None;
        }
        self.open.as_mut().and_then(|o| Arc::get_mut(&mut o.doc))
    }

    fn active_track(&self) -> usize {
        self.open.as_ref().map_or(0, |o| o.active_track)
    }

    /// Selected notes of the active track (0 while the Conductor is selected:
    /// note commands do not apply to it).
    fn selected_count(&self) -> usize {
        if self.conductor_selected() {
            return 0;
        }
        // B2-F18: over the clips of the Track Aggregate.
        let Some(doc) = self.doc() else {
            return 0;
        };
        self.selection_targets()
            .into_iter()
            .filter_map(|t| plumspirit_engine::editor::note_view(doc, t).ok())
            .map(|v| v.notes.selected_count())
            .sum()
    }

    fn conductor_selected(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.conductor_selected)
    }

    /// The row the Onion Skin's sources are relative to (B2-D01): the active
    /// track, or `None` while the Conductor is selected — it owns no notes,
    /// so no musical track is excluded (`OnionConfig::sources_at`).
    fn onion_row(&self) -> Option<usize> {
        (!self.conductor_selected()).then(|| self.active_track())
    }

    /// The Onion Skin's source tracks (indices, track order) for the row
    /// selected now.
    fn onion_ghosts(&self) -> Vec<usize> {
        self.doc()
            .map(|d| {
                let ids: Vec<TrackId> = d.tracks().iter().map(|t| t.id()).collect();
                d.project().onion.sources_at(&ids, self.onion_row())
            })
            .unwrap_or_default()
    }

    fn select_conductor(&mut self, on: bool) {
        if let Some(o) = self.open.as_mut()
            && o.conductor_selected != on
        {
            o.conductor_selected = on;
            self.roll.cancel_gesture();
            self.graphs.cancel();
            self.graphs.clear_point_selection();
        }
    }

    fn doc_generation(&self) -> u64 {
        self.open.as_ref().map_or(0, |o| o.generation)
    }

    fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        let text = text.into();
        match kind {
            StatusKind::Error => log::error!("{text}"),
            StatusKind::Warning => log::warn!("{text}"),
            _ => log::info!("{text}"),
        }
        self.status = Some(Status {
            text,
            kind,
            details: Vec::new(),
        });
    }

    fn set_status_details(
        &mut self,
        text: impl Into<String>,
        kind: StatusKind,
        details: Vec<String>,
    ) {
        self.set_status(text, kind);
        if let Some(s) = self.status.as_mut() {
            s.details = details;
        }
    }

    fn show_error(&mut self, title: &str, body: String) {
        log::error!("{title} {body}");
        self.dialog = Some(Dialog::Message {
            title: title.to_owned(),
            body,
            is_error: true,
        });
    }

    fn busy_with_writer(&self) -> bool {
        matches!(
            self.job.as_ref().map(|j| &j.kind),
            Some(
                JobKind::Export { .. }
                    | JobKind::Edit { .. }
                    | JobKind::Save { .. }
                    | JobKind::Clipboard { .. }
            )
        )
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        id
    }

    /// Why a command shown in a menu cannot run now (`Some`: the menu shows
    /// it disabled with this sentence as its tooltip). Cheap: asked only
    /// while a menu is open, and no note is visited (V09-S12, §047).
    pub fn why_disabled(&self, cmd: Command) -> Option<String> {
        match cmd {
            Command::MergeClips(kind) => self.merge_unavailable(kind),
            Command::PasteRepeat => self.paste_repeat_unavailable(),
            Command::NudgeClipsLeft => self.nudge_unavailable(false),
            Command::NudgeClipsRight => self.nudge_unavailable(true),
            Command::ZoomToSelection => self.zoom_selection_unavailable(),
            _ => None,
        }
    }

    /// Whether a command can act right now. Menus, toolbar and shortcuts all use this.
    pub fn enabled(&self, cmd: Command) -> bool {
        if self.lua_formats.reviewing() {
            return false;
        }
        let has_doc = self.open.is_some();
        let idle = self.job.is_none();
        let selected = self.selected_count() > 0;
        let conductor = self.conductor_selected();
        let (timing, markers) = self.doc().map_or((false, false), |d| {
            let c = d.conductor();
            (c.timing_editable(), c.markers_editable())
        });
        // B1-0097: the Track List's clip commands (selected clips, the clip
        // clipboard).
        if has_doc && self.main_view == track_list::MainView::TrackList && !self.player_shown() {
            let clips = !self.track_list.selected.is_empty();
            match cmd {
                // B2-F29: Select All selects the clips there.
                Command::SelectAll => return true,
                Command::Copy if clips => return true,
                Command::Cut | Command::Delete | Command::SliceNotes if clips => return idle,
                Command::Paste if self.uses_system_clipboard() || self.clip_clipboard.is_some() => {
                    return idle && !conductor;
                }
                _ => {}
            }
        }
        match cmd {
            Command::NewProject | Command::Open => !self.busy_with_writer(),
            Command::Save | Command::SaveAs | Command::Export | Command::ProjectProperties => {
                has_doc && idle
            }
            Command::MainView(_) | Command::SwitchView => has_doc,
            Command::OpenClip => {
                has_doc
                    && self.main_view == track_list::MainView::TrackList
                    && !self.track_list.selected.is_empty()
            }
            Command::PreviousTrack | Command::NextTrack => {
                has_doc && self.main_view == track_list::MainView::TrackList
            }
            // From the Conductor it says why it cannot (a status sentence).
            Command::AddClip => has_doc && idle,
            // In the Track List: a chord on a selection that cannot be merged
            // says why in the status bar; the menus grey the entry with the
            // same sentence (`why_disabled`, §047).
            Command::MergeClips(_) => {
                has_doc && idle && self.main_view == track_list::MainView::TrackList
            }
            // In both views (notes or clips); without a selection the chord
            // says why and the menus grey the entry (`why_disabled`).
            Command::PasteRepeat => has_doc && idle,
            // B2-F29: the nudges are Track List commands (the Piano Roll
            // leaves their keys alone); in the Track List the key says why.
            Command::NudgeClipsLeft | Command::NudgeClipsRight => {
                has_doc && idle && self.track_list_shown()
            }
            // B2-F29: both views; without a selection the key says why.
            Command::ZoomToSelection | Command::JumpToPlayhead => has_doc && !self.player_shown(),
            Command::Exit
            | Command::Theme(_)
            | Command::ToggleDevOverlay
            | Command::ToggleVelocityLane
            | Command::ToggleSmoothFollow
            | Command::ToggleDisplayNoteColors
            | Command::ChooseSoundFont => true,
            Command::Undo => {
                has_doc && idle && self.open.as_ref().is_some_and(|o| o.history.can_undo())
            }
            Command::Redo => {
                has_doc && idle && self.open.as_ref().is_some_and(|o| o.history.can_redo())
            }
            Command::Copy => {
                has_doc
                    && (selected
                        || self.graphs.views[self.active_graph]
                            .has_point_selection(self.shown_target(self.active_graph)))
            }
            Command::Paste if self.uses_system_clipboard() => {
                has_doc
                    && idle
                    && (!conductor
                        || self.shown_target(self.active_graph)
                            == plumspirit_engine::model::GraphTarget::MasterVolume)
            }
            Command::Paste => {
                // v0.8 §204: notes never paste into the Conductor.
                has_doc
                    && idle
                    && ((!conductor && self.clipboard.as_ref().is_some_and(|c| !c.is_empty()))
                        || self.event_clipboard.is_some())
            }
            Command::Delete => {
                has_doc
                    && idle
                    && (selected
                        || self.roll.selected_marker.is_some()
                        || self.roll.selected_signature.is_some()
                        || self.graphs.views[self.active_graph]
                            .has_point_selection(self.shown_target(self.active_graph)))
            }
            Command::FlipX | Command::FlipY | Command::ScaleX | Command::ScaleY => {
                has_doc && idle && selected
            }
            Command::OpenTool(_) | Command::InsertScore => has_doc && idle,
            Command::SaveScoreAs => has_doc && selected,
            Command::ToggleOnionOutlines
            | Command::ToggleSkipChannel10
            | Command::ToggleSelectionAnalytics
            | Command::ToggleSelectionCharts
            | Command::ToggleKeyGuide
            | Command::ToggleProgressorMuteSolo
            | Command::ToggleScaleYDuplicate
            | Command::ToggleScaleYOverlap
            | Command::ToggleOverlapPrevention
            | Command::RotatePivot(_)
            | Command::OverlapMode(_)
            | Command::ToggleOverlapGeneratedOnly
            | Command::ToggleOverlapAcross
            | Command::ToggleOverlapExisting
            | Command::ToggleTextOverlays
            | Command::ToggleHighContrast
            | Command::ResetUiScale
            | Command::SelectionAnalyticsSettings
            | Command::ShowKeybinds
            | Command::ShowHelp
            | Command::ShowQuickStart
            | Command::ToggleOrientation
            | Command::ShowAbout
            | Command::ShowLuaPlugins
            | Command::ToggleRunLuaPlugins
            | Command::ToggleLuaEnableOnInstall
            | Command::SaveDiagnostics => true,
            Command::OpenPlugin(_) => has_doc && idle,
            Command::CloseProject => has_doc && idle,
            Command::ZoomTool | Command::ZoomBack | Command::ZoomForward | Command::ScrubTool => {
                has_doc
            }
            Command::RepeatLastTool | Command::RepeatLastToolNewSeed => {
                has_doc && idle && self.last_recipe.is_some()
            }
            Command::ReopenLastTool => has_doc && idle && self.last_tool.is_some(),
            Command::MuteNotes => {
                let s = self.summary.map(|(_, s)| s).unwrap_or_default();
                has_doc && idle && s.count > s.muted
            }
            Command::UnmuteNotes => {
                let s = self.summary.map(|(_, s)| s).unwrap_or_default();
                has_doc && idle && s.muted > 0
            }
            Command::Cut => {
                has_doc
                    && idle
                    && (selected
                        || (self.graph_focus
                            && self.graphs.views[self.active_graph]
                                .has_point_selection(self.shown_target(self.active_graph))))
            }
            Command::ZoomInY | Command::ZoomOutY | Command::FitKeys128 => has_doc,
            Command::FitAllKeys => {
                has_doc
                    && self.doc().is_some_and(|d| {
                        d.project().keys == plumspirit_engine::model::project::KeyRange::Extended256
                    })
            }
            Command::ToggleLockFitRange | Command::ToggleYZoomBeyondKeys => true,
            Command::MoveTrackUp
            | Command::MoveTrackDown
            | Command::IndentTrack
            | Command::OutdentTrack
            | Command::DeleteSelectedTracks => has_doc && idle && !conductor,
            Command::GroupNotes
            | Command::UngroupNotes
            | Command::ToggleMuteNotes
            | Command::SliceNotes
            | Command::DuplicateOctaveUp
            | Command::DuplicateOctaveDown => has_doc && idle && selected,
            Command::SaveAutomationAs => {
                has_doc
                    && self.graphs.views[self.active_graph]
                        .has_point_selection(self.shown_target(self.active_graph))
            }
            Command::SelectAll | Command::Quantize => {
                has_doc && idle && !conductor && self.edit_notes().is_some_and(|n| !n.is_empty())
            }
            Command::AddMarker => has_doc && idle && markers,
            Command::AddTimeSignature | Command::AddTempoChange => has_doc && idle && timing,
            Command::SelectConductor
            | Command::ResetZoom
            | Command::FitSong
            | Command::ToggleFollow => has_doc,
            Command::AddTrack | Command::AddTracks | Command::DeleteTrack => has_doc && idle,
            Command::Onion(_)
            | Command::ChooseOnionTracks
            | Command::ToggleOnion
            | Command::OnionSelectedTracks
            | Command::OnionShowOnlySelected
            | Command::OnionAddSelected
            | Command::OnionRemoveSelected
            | Command::ToggleOnionPerTrack => has_doc && idle,
            Command::OnionUseForEveryTrack => {
                has_doc && idle && self.doc().is_some_and(|d| d.project().onion.per_track)
            }
            Command::ResetView
            | Command::Tool(_)
            | Command::ZoomIn
            | Command::ZoomOut
            | Command::PlayPause
            | Command::Stop
            | Command::GoToStart
            | Command::PreviousMeasure
            | Command::NextMeasure
            | Command::GoToEnd => has_doc,
            Command::ToggleAnchorPlayhead | Command::OpenSettings => true,
            Command::SelectRange
            | Command::ToggleLoop
            | Command::LoopToSelection
            | Command::PlayingTime
            | Command::PlayAll
            | Command::PlayOnlySelected
            | Command::ToggleGraph2
            | Command::ToggleEventList => has_doc && idle,
            Command::RestoreAllTracks => {
                has_doc && idle && self.doc().is_some_and(|d| d.project().monitor.is_some())
            }
            Command::MuteCurrentTrack | Command::SoloCurrentTrack => has_doc && idle && !conductor,
            Command::ClearLoop => has_doc && idle && self.loop_config().range.is_some(),
            Command::TrackProperties | Command::FitTrackEnd => has_doc && idle && !conductor,
            Command::CustomFunctions
            | Command::ToggleMetrics
            | Command::ToggleExplicitStatus
            | Command::TogglePlayerSafeStart
            | Command::ToggleLoudnessReference => true,
            Command::ToggleProgressor
            | Command::ProgressorSettings
            | Command::ToggleMidiPlayer
            | Command::MidiPlayerSettings
            | Command::ToggleMidiInspector
            | Command::ToggleLiveKeys
            | Command::KeyScope(_)
            | Command::KeyFollowRoll
            | Command::ToggleAudition
            | Command::AuditionLength(_) => true,
            Command::ImportIntoTrack => has_doc && idle && !conductor && self.merge.is_none(),
            Command::AddSubtrack
            | Command::AddSubtracks
            | Command::AddSubchannel
            | Command::AddSubchannels
            | Command::Subchannels => has_doc && idle && !conductor,
            Command::MoveToSubchannel | Command::PaintCurrentSubchannel => {
                has_doc && idle && !conductor && selected
            }
            Command::OnionOrder(_) | Command::OnionDensity(_) | Command::OnionOpacity => {
                has_doc && idle
            }
            Command::TogglePreview
            | Command::FollowPosition
            | Command::RecentTasks
            | Command::SelectionOpacity => true,
            Command::UndoHistory => has_doc,
        }
    }

    /// Applies a track-panel / Track List menu action (v0.9 V09-S10: one
    /// dispatcher for every place a track, folder or the Conductor is the
    /// context); a returned command is run by the caller like a menu command.
    pub(super) fn apply_track_action(
        &mut self,
        action: track_panel::TrackAction,
        rows: &[TreeRowView],
    ) -> Option<Command> {
        match action {
            track_panel::TrackAction::Select(i) => {
                self.selected_tracks.clear();
                self.select_track(i);
            }
            track_panel::TrackAction::ToggleSelected(i) => {
                if let Some(id) = self.doc().and_then(|d| d.track(i)).map(|t| t.id())
                    && !self.selected_tracks.remove(&id)
                {
                    self.selected_tracks.insert(id);
                }
            }
            track_panel::TrackAction::SelectRange(i) => {
                self.select_track_range(i, rows);
            }
            track_panel::TrackAction::AddClip(i) if self.job.is_none() => {
                self.add_clip_at_cursor(i)
            }
            track_panel::TrackAction::ConductorAdd(what, at) if self.job.is_none() => {
                self.conductor_add(what, at)
            }
            track_panel::TrackAction::ConductorAdd(..) => {}
            track_panel::TrackAction::AddBelow(i) if self.job.is_none() => self.add_track_below(i),
            track_panel::TrackAction::AddTracks(at) if self.job.is_none() => {
                self.request_add_tracks(at)
            }
            track_panel::TrackAction::DeleteTracks(i) if self.job.is_none() => {
                self.request_delete_tracks(i)
            }
            track_panel::TrackAction::Move {
                dragged,
                target,
                place,
            } if self.job.is_none() => self.move_tracks_to(dragged, target, place),
            track_panel::TrackAction::Nudge(i, down) if self.job.is_none() => {
                self.nudge_tracks(i, down)
            }
            track_panel::TrackAction::Indent(i, on) if self.job.is_none() => {
                self.indent_tracks(i, on)
            }
            track_panel::TrackAction::AddBelow(_)
            | track_panel::TrackAction::AddTracks(_)
            | track_panel::TrackAction::AddClip(_)
            | track_panel::TrackAction::DeleteTracks(_)
            | track_panel::TrackAction::Move { .. }
            | track_panel::TrackAction::Nudge(..)
            | track_panel::TrackAction::Indent(..) => {}
            track_panel::TrackAction::Properties(i) if self.job.is_none() => {
                self.open_track_properties(i);
            }
            track_panel::TrackAction::Properties(_) => {}
            track_panel::TrackAction::FitEnd(i) => self.fit_track_end(i),
            track_panel::TrackAction::ToggleMute(i) => self.toggle_mute(i),
            track_panel::TrackAction::OnionSet(tracks, on) => self.set_onion_tracks(&tracks, on),
            track_panel::TrackAction::OnionOnly(tracks) => self.set_onion_only(&tracks),
            track_panel::TrackAction::ToggleSolo(i, add) => self.toggle_solo(i, add),
            track_panel::TrackAction::MuteSelection(i, on) => self.mute_selection(i, on),
            track_panel::TrackAction::SoloSelection(i) => self.solo_selection(i),
            track_panel::TrackAction::PlayAll => self.play_all(),
            track_panel::TrackAction::PlayOnlySelected => self.play_only_selected(),
            track_panel::TrackAction::RestoreAllTracks => self.restore_all_tracks(),
            track_panel::TrackAction::SelectConductor => return Some(Command::SelectConductor),
            track_panel::TrackAction::Add => return Some(Command::AddTrack),
            track_panel::TrackAction::Delete => return Some(Command::DeleteTrack),
            track_panel::TrackAction::ToggleCollapse(id) => self.toggle_collapsed(id),
            track_panel::TrackAction::AddSubtrack(i) if self.job.is_none() => {
                self.add_subtracks(i, 1)
            }
            track_panel::TrackAction::AddSubtracks(i) if self.job.is_none() => {
                self.dialog = Some(Dialog::AddMany {
                    target: self.track_display_name(i),
                    what: dialogs::AddManyKind::Subtracks(i),
                    count: "10".into(),
                    error: None,
                    focused: false,
                });
            }
            track_panel::TrackAction::AddSubtrack(_)
            | track_panel::TrackAction::AddSubtracks(_) => {}
            track_panel::TrackAction::MuteSubtree(i, on) => self.mute_subtree(i, on),
            track_panel::TrackAction::SoloSubtree(i) => self.solo_subtree(i),
            track_panel::TrackAction::Sub(a) => match a {
                track_panel::SubAction::Select(id) => self.set_active_sub(id),
                track_panel::SubAction::Add => return Some(Command::AddSubchannel),
                track_panel::SubAction::AddMany => return Some(Command::AddSubchannels),
                track_panel::SubAction::GoTo(t) => self.go_to_subchannel(&t),
                track_panel::SubAction::OpenWindow => self.subchannel_window = true,
            },
            a @ (track_panel::TrackAction::FolderToggle(_)
            | track_panel::TrackAction::FolderNew(_)
            | track_panel::TrackAction::FolderRenameStart(_)
            | track_panel::TrackAction::FolderRename(..)
            | track_panel::TrackAction::FolderDelete(_)
            | track_panel::TrackAction::FolderAddTrack(_)
            | track_panel::TrackAction::FolderDeleteWithTracks(_)
            | track_panel::TrackAction::MoveToFolder(..)
            | track_panel::TrackAction::RemoveFromFolder(_)
            | track_panel::TrackAction::DropOnFolder { .. }
            | track_panel::TrackAction::MoveFolder { .. }) => self.folder_action(a),
        }
        None
    }

    // ------------------------------------------------------------- commands

    fn execute(&mut self, cmd: Command, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.enabled(cmd) {
            return;
        }
        // v0.9 (V09-S17): a note command run from the menu while the MIDI
        // Player is shown returns to the editor (the Player shows no
        // selection to act on).
        if cmd.acts_on_notes() && self.player_shown() {
            self.set_player(false);
        }
        // B1-0097: Edit → Copy / Cut / Paste / Delete / Slice in the Track
        // List act on its selected clips (and the clip clipboard), as their
        // keys do.
        if self.main_view == track_list::MainView::TrackList
            && self.open.is_some()
            && !self.player_shown()
        {
            // B2-F29: Edit → Select All selects every clip there.
            if cmd == Command::SelectAll {
                self.select_all_clips();
                return;
            }
            if let Some(intent) = self.track_list.key_intent(cmd) {
                self.apply_clip_intent(intent);
                return;
            }
            if cmd == Command::Paste
                && (self.uses_system_clipboard() || self.clip_clipboard.is_some())
            {
                self.apply_clip_intent(track_list::ClipIntent::Paste {
                    track: None,
                    at: None,
                });
                return;
            }
        }
        // v0.9 (V09-S08): a note command run from the Track List (menu) opens
        // the Piano Roll, where its notes, preview and selection are shown.
        if cmd.acts_on_notes() && self.main_view == track_list::MainView::TrackList {
            self.set_main_view(track_list::MainView::PianoRoll);
        }
        let in_track_list = self.main_view == track_list::MainView::TrackList;
        match cmd {
            // The Track List's own zoom (time) and row height (Y).
            Command::FitSong
            | Command::ZoomIn
            | Command::ZoomOut
            | Command::ZoomInY
            | Command::ZoomOutY
            | Command::ResetZoom
                if in_track_list =>
            {
                if let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) {
                    self.track_list.zoom_command(cmd, &doc);
                }
            }
            Command::NewProject => self.request_new_project(),
            Command::Open => self.request_open(frame, ctx),
            Command::Save => self.save(frame, ctx, Then::Nothing),
            Command::SaveAs => self.save_as(frame, ctx, Then::Nothing),
            Command::Export => self.request_export(frame, ctx),
            Command::ProjectProperties => self.open_properties(),
            Command::Exit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::UndoHistory => self.history_window = true,
            Command::Copy => self.request_musical_copy(false),
            Command::Paste if self.uses_system_clipboard() => self.paste_music(),
            Command::Paste => {
                let graph_paste = self.graph_focus || self.clipboard.is_none();
                if !(graph_paste && self.paste_graph_events()) {
                    self.paste(ctx);
                }
            }
            Command::Delete => {
                if !self.delete_focused() {
                    self.delete_selected();
                }
            }
            Command::Quantize => self.open_quantize(),
            Command::OpenTool(id) => self.open_tool(id),
            Command::SaveScoreAs => {
                self.request_score_path(frame, dialogs::PromptPurpose::SaveScore)
            }
            Command::InsertScore => {
                self.request_score_path(frame, dialogs::PromptPurpose::InsertScore)
            }
            Command::SaveAutomationAs => {
                self.request_score_path(frame, dialogs::PromptPurpose::SaveAutomation)
            }
            Command::ToggleOnionOutlines => {
                self.settings.onion_outlines = !self.settings.onion_outlines;
                self.save_settings();
            }
            Command::ToggleSkipChannel10 => {
                self.settings.skip_channel_10 = !self.settings.skip_channel_10;
                self.save_settings();
            }
            Command::ToggleSelectionAnalytics => {
                self.settings.selection_analytics = !self.settings.selection_analytics;
                self.save_settings();
            }
            Command::ToggleSelectionCharts => {
                self.set_selection_charts(!self.settings.selection_advanced);
            }
            Command::ToggleKeyGuide => {
                self.settings.key_guide_88 = !self.settings.key_guide_88;
                self.save_settings();
            }
            Command::ToggleProgressorMuteSolo => {
                use progressor::ScopeSetting;
                let p = &mut self.settings.progressor;
                p.scope = if p.scope == ScopeSetting::Audible {
                    ScopeSetting::Project
                } else {
                    ScopeSetting::Audible
                };
                self.save_settings();
            }
            Command::ToggleScaleYDuplicate => {
                let p = &mut self.settings.scale_y;
                p.handles_duplicate = !p.handles_duplicate;
                let on = p.handles_duplicate;
                self.save_settings();
                self.set_status(
                    if on {
                        "Scale handles duplicate notes when they stretch the rows (Alt on an edge: not for one drag)."
                    } else {
                        "Scale handles keep N notes as N notes (Alt on an edge: duplicate for one drag)."
                    },
                    StatusKind::Info,
                );
            }
            Command::ToggleScaleYOverlap => {
                self.settings.scale_y.prevent = !self.settings.scale_y.prevent;
                self.save_settings();
            }
            Command::RotatePivot(center) => {
                self.settings.rotate_about_center = center;
                self.save_settings();
            }
            Command::ToggleOverlapPrevention => {
                let o = &mut self.settings.overlap;
                o.on = !o.on;
                let on = o.on;
                self.save_settings();
                self.set_status(
                    if on {
                        "Note Overlap Prevention is on: tools, scale gestures, Scale X / Y, Flip, Quantize, Paste and Draw remove the overlaps they would create."
                    } else {
                        "Note Overlap Prevention is off."
                    },
                    StatusKind::Info,
                );
            }
            Command::OverlapMode(m) => {
                self.settings.overlap.general.policy.mode =
                    plumspirit_engine::transform::overlap::Mode::from_index(usize::from(m));
                self.save_settings();
            }
            Command::ToggleOverlapGeneratedOnly => {
                let p = &mut self.settings.overlap.general.policy;
                p.generated_only = !p.generated_only;
                self.save_settings();
            }
            Command::ToggleOverlapAcross => {
                let p = &mut self.settings.overlap.general.policy;
                p.across = !p.across;
                self.save_settings();
            }
            Command::ToggleOverlapExisting => {
                let g = &mut self.settings.overlap.general;
                g.existing = !g.existing;
                self.save_settings();
            }
            Command::ToggleHighContrast => {
                self.settings.high_contrast = !self.settings.high_contrast;
                theme::apply(
                    ctx,
                    self.settings.theme,
                    self.settings.high_contrast,
                    self.settings.ui_accent_color,
                );
                self.save_settings();
            }
            Command::ToggleTextOverlays => {
                self.settings.hide_text_overlays = !self.settings.hide_text_overlays;
                self.save_settings();
            }
            Command::ResetUiScale => self.set_ui_scale(100, ctx),
            Command::SelectionAnalyticsSettings => {
                if self.selection_window.is_none() {
                    self.selection_window = Some((
                        self.settings.progressor.selection_template.clone(),
                        self.settings.selection_analytics,
                        self.settings.progressor.selection_collapsed,
                    ));
                }
            }
            Command::CloseProject => self.request_close_project(),
            Command::ReopenLastTool => {
                if let Some(id) = self.last_tool {
                    self.open_tool(id);
                }
            }
            Command::RepeatLastTool => self.repeat_last_tool(false, ctx),
            Command::RepeatLastToolNewSeed => self.repeat_last_tool(true, ctx),
            Command::MuteNotes | Command::UnmuteNotes => self.quick_tool(
                "mute_notes",
                &[(
                    "action",
                    plumspirit_engine::transform::ParamValue::Choice(u32::from(
                        cmd == Command::UnmuteNotes,
                    )),
                )],
                Some(cmd.label()),
                ctx,
            ),
            Command::SliceNotes => self.slice_notes(ctx),
            Command::PaintCurrentSubchannel => self.paint_selection_with_current_sub(),
            Command::DuplicateOctaveUp | Command::DuplicateOctaveDown => self.quick_tool(
                "duplicate_octave",
                &[(
                    "semis",
                    plumspirit_engine::transform::ParamValue::Int(
                        if cmd == Command::DuplicateOctaveUp {
                            12
                        } else {
                            -12
                        },
                    ),
                )],
                Some(cmd.label()),
                ctx,
            ),
            Command::Cut => self.request_musical_copy(true),
            Command::FitKeys128 | Command::FitAllKeys => {
                self.set_main_view(track_list::MainView::PianoRoll);
                let hi = if cmd == Command::FitKeys128 { 127 } else { 255 };
                self.roll.fit_keys(0, hi);
            }
            Command::ToggleLockFitRange => {
                self.settings.lock_fit_range = !self.settings.lock_fit_range;
                self.roll.lock_fit = self.settings.lock_fit_range;
                self.save_settings();
            }
            Command::ToggleYZoomBeyondKeys => {
                self.settings.y_zoom_beyond_keys = !self.settings.y_zoom_beyond_keys;
                self.roll.y_beyond_keys = self.settings.y_zoom_beyond_keys;
                self.save_settings();
            }
            Command::ZoomInY | Command::ZoomOutY => {
                let step = crate::piano_roll::viewport::Y_ZOOM_STEP;
                self.roll.zoom_y_by(if cmd == Command::ZoomInY {
                    step
                } else {
                    1.0 / step
                });
            }
            Command::ShowKeybinds => self.keybinds_window = true,
            Command::ShowHelp => {
                let topic = self.help_context();
                self.help.show_topic(topic);
            }
            Command::ShowQuickStart => self.help.show_topic("quick-start"),
            Command::ToggleOrientation => {
                self.settings.show_orientation = !self.settings.show_orientation;
                self.save_settings();
            }
            Command::OpenSettings => {
                self.settings_window.focus_search = !self.settings_window.open;
                self.settings_window.open = true;
            }
            Command::ShowAbout => self.about_window = true,
            Command::ShowLuaPlugins => self.lua_plugins_window.open = true,
            Command::ToggleRunLuaPlugins => {
                self.settings.lua_plugins.run_plugins = !self.settings.lua_plugins.run_plugins;
                let prefs = self.settings.lua_plugins.clone();
                if prefs.run_plugins {
                    self.lua_plugins().start(&prefs);
                } else {
                    self.lua_plugins().stop_all(&prefs);
                }
                self.save_settings();
            }
            Command::ToggleLuaEnableOnInstall => {
                self.settings.lua_plugins.enable_on_install =
                    !self.settings.lua_plugins.enable_on_install;
                self.save_settings();
            }
            Command::SaveDiagnostics => self.open_diagnostics_window(),
            Command::OpenPlugin(id) => self.open_plugin(id),
            Command::ZoomTool => self.zoom.open = !self.zoom.open,
            Command::ScrubTool => {
                self.scrub.open = !self.scrub.open;
                if !self.scrub.open {
                    self.scrub_stop();
                }
            }
            Command::ZoomBack => self.zoom_back(),
            Command::ZoomForward => self.zoom_forward(),
            Command::MoveTrackUp => self.nudge_tracks(self.active_track(), false),
            Command::MoveTrackDown => self.nudge_tracks(self.active_track(), true),
            Command::IndentTrack => self.indent_tracks(self.active_track(), true),
            Command::OutdentTrack => self.indent_tracks(self.active_track(), false),
            Command::DeleteSelectedTracks => self.request_delete_tracks(self.active_track()),
            Command::GroupNotes => self.quick_tool("group", &[], None, ctx),
            Command::UngroupNotes => self.quick_tool(
                "group",
                &[(
                    "action",
                    plumspirit_engine::transform::ParamValue::Choice(1),
                )],
                Some(Command::UngroupNotes.label()),
                ctx,
            ),
            Command::ToggleMuteNotes => self.quick_tool(
                "mute_notes",
                &[(
                    "action",
                    plumspirit_engine::transform::ParamValue::Choice(2),
                )],
                Some(Command::ToggleMuteNotes.label()),
                ctx,
            ),
            Command::ResetZoom => {
                let pos = self.transport.position();
                if let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) {
                    self.roll.reset_zoom(&doc, pos);
                }
            }
            Command::FitSong => {
                if let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) {
                    self.roll.fit_song(&doc);
                }
            }
            Command::ToggleFollow => {
                if self.settings.follow_playhead && !self.follow_suspended {
                    self.settings.follow_playhead = false;
                } else {
                    self.settings.follow_playhead = true;
                    self.follow_suspended = false;
                    self.reveal_playhead();
                }
                self.save_settings();
            }
            Command::ToggleSmoothFollow => {
                self.settings.smooth_follow = !self.settings.smooth_follow;
                self.save_settings();
            }
            Command::ToggleDisplayNoteColors => {
                self.settings.display_note_colors = !self.settings.display_note_colors;
                self.save_settings();
            }
            Command::SelectConductor => self.select_conductor(true),
            Command::AddMarker => self.add_at_position(song::SongAdd::Marker),
            Command::AddTimeSignature => self.add_at_position(song::SongAdd::TimeSignature),
            Command::AddTempoChange => self.add_at_position(song::SongAdd::Tempo),
            // The roll's notes (an opened clip's content; V09-S11; every
            // clip of the track in Track Aggregate, B2-F18).
            Command::SelectAll => self.select_all_notes(),
            Command::FlipX => self.transform(editing::Transform::FlipX, ctx),
            Command::FlipY => self.transform(editing::Transform::FlipY, ctx),
            Command::ScaleX | Command::ScaleY => {
                let axis = if cmd == Command::ScaleX {
                    ScaleAxis::X
                } else {
                    ScaleAxis::Y
                };
                self.dialog = Some(Dialog::Scale {
                    axis,
                    text: String::new(),
                    error: None,
                    focused: false,
                    preview: self.settings.preview,
                    summary: String::new(),
                    // §191: Duplicate Notes is chosen each time; its options
                    // and the overlap prevention come from the last Scale Y.
                    fill_on: false,
                    fill: self.settings.scale_y.fill,
                    prevent: self.settings.scale_y.prevent,
                    cleanup: self.settings.scale_y.cleanup,
                });
            }
            Command::Theme(t) => self.set_theme(t, ctx),
            Command::MainView(v) => self.set_main_view(v),
            Command::SwitchView => {
                self.set_main_view(match self.main_view {
                    track_list::MainView::TrackList => track_list::MainView::PianoRoll,
                    track_list::MainView::PianoRoll => track_list::MainView::TrackList,
                });
                // Tab is egui's focus key too: no widget takes the focus.
                ctx.memory_mut(|m| {
                    m.move_focus(egui::FocusDirection::None);
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            }
            Command::OpenClip => {
                if let Some(id) = self.track_list.open_target() {
                    self.apply_clip_intent(track_list::ClipIntent::Open(id));
                }
            }
            Command::AddClip => {
                if self.conductor_selected() {
                    self.set_status(
                        "The Conductor holds no notes: select a track or subtrack to add a Piano Roll Clip to it.",
                        StatusKind::Info,
                    );
                } else {
                    // The clip is shown where it lands (no invisible edits).
                    self.set_main_view(track_list::MainView::TrackList);
                    self.add_clip_at_cursor(self.active_track());
                }
            }
            Command::MergeClips(kind) => self.start_clip_merge(kind),
            Command::PasteRepeat => self.start_paste_repeat(),
            Command::NudgeClipsLeft => self.nudge_clips(false),
            Command::NudgeClipsRight => self.nudge_clips(true),
            Command::ZoomToSelection => self.zoom_to_selection(),
            Command::JumpToPlayhead => self.jump_to_playhead(),
            Command::PreviousTrack => self.step_track(false),
            Command::NextTrack => self.step_track(true),
            Command::ResetView => self.roll.request_reset_view(),
            Command::ToggleVelocityLane => {
                self.settings.velocity_lane_visible = !self.settings.velocity_lane_visible;
                self.graphs.cancel();
                self.save_settings();
            }
            Command::Onion(mode) => self.set_onion_mode(mode),
            Command::ToggleOnion => self.toggle_onion(),
            Command::ToggleOnionPerTrack => {
                let active = self.active_track_id();
                if let Some(doc) = self.doc_mut() {
                    let o = &mut doc.project_mut().onion;
                    o.per_track = !o.per_track;
                }
                // The track active now is the one the next switch stores.
                self.onion_track_seen = active;
            }
            Command::OnionSelectedTracks
            | Command::OnionShowOnlySelected
            | Command::OnionAddSelected
            | Command::OnionRemoveSelected => {
                let Some(doc) = self.doc() else {
                    return;
                };
                let row = self.onion_row();
                let tracks: Vec<usize> = (0..doc.track_count())
                    .filter(|&i| {
                        Some(i) != row
                            && doc
                                .track(i)
                                .is_some_and(|t| self.selected_tracks.contains(&t.id()))
                    })
                    .collect();
                if tracks.is_empty() {
                    self.set_status(
                        "Select the tracks first (Ctrl+click or Shift+click in the track list); the active track is never its own ghost.",
                        StatusKind::Info,
                    );
                    return;
                }
                match cmd {
                    Command::OnionShowOnlySelected => self.set_onion_only(&tracks),
                    Command::OnionAddSelected => self.set_onion_tracks(&tracks, true),
                    Command::OnionRemoveSelected => self.set_onion_tracks(&tracks, false),
                    _ => {
                        let ids: Vec<TrackId> = doc.tracks().iter().map(|t| t.id()).collect();
                        let shown = doc.project().onion.sources_at(&ids, row);
                        let all_shown = tracks.iter().all(|t| shown.contains(t));
                        self.set_onion_tracks(&tracks, !all_shown);
                    }
                }
            }
            Command::OnionUseForEveryTrack => {
                let dropped = self
                    .doc_mut()
                    .is_some_and(|d| d.project_mut().onion.use_for_every_track());
                self.set_status(
                    if dropped {
                        "Every track now uses this Onion Skin setting (their own were reset)."
                    } else {
                        "Every track already uses this Onion Skin setting."
                    },
                    StatusKind::Info,
                );
            }
            Command::ChooseOnionTracks => {
                self.onion_choice = self.doc().map(|d| d.project().onion.specific.clone());
            }
            Command::AddTrack => self.add_track(),
            Command::AddTracks => self.request_add_tracks(None),
            Command::DeleteTrack => self.request_delete_track(),
            Command::ImportIntoTrack => self.request_import_into(frame, ctx),
            Command::AddSubtrack => {
                let i = self.active_track();
                self.add_subtracks(i, 1);
            }
            Command::AddSubtracks => {
                let i = self.active_track();
                self.dialog = Some(Dialog::AddMany {
                    target: self.track_display_name(i),
                    what: dialogs::AddManyKind::Subtracks(i),
                    count: "10".into(),
                    error: None,
                    focused: false,
                });
            }
            Command::AddSubchannel => self.add_subchannels(1),
            Command::AddSubchannels => {
                self.dialog = Some(Dialog::AddMany {
                    target: self.track_display_name(self.active_track()),
                    what: dialogs::AddManyKind::Subchannels,
                    count: "16".into(),
                    error: None,
                    focused: false,
                });
            }
            Command::Subchannels => self.subchannel_window = true,
            Command::MoveToSubchannel => {
                let ti = self.active_track();
                if let Some(sc) = self.active_sub_of(ti) {
                    self.dialog = Some(Dialog::MoveToSubchannel {
                        number: sc.id.number().to_string(),
                        set_channel: true,
                        error: None,
                        focused: false,
                    });
                }
            }
            Command::OnionOrder(order) => {
                if let Some(d) = self.doc_mut() {
                    d.project_mut().onion.order = order;
                }
            }
            Command::OnionDensity(basis) => {
                if let Some(d) = self.doc_mut() {
                    d.project_mut().onion.density = basis;
                }
            }
            Command::OnionOpacity => {
                let v = self.doc().map_or(35, |d| d.project().onion.opacity);
                self.dialog = Some(Dialog::Percent {
                    what: dialogs::PercentKind::OnionOpacity,
                    text: v.to_string(),
                    error: None,
                    focused: false,
                });
            }
            Command::TogglePreview => {
                self.settings.preview = !self.settings.preview;
                self.save_settings();
            }
            Command::ToggleProgressor => {
                self.settings.progressor.enabled = !self.settings.progressor.enabled;
                self.save_settings();
            }
            Command::ToggleMidiPlayer => self.set_player(!self.settings.player.enabled),
            Command::ToggleMidiInspector => self.set_inspector(!self.settings.inspector),
            Command::MidiPlayerSettings => {
                if self.player_window.is_none() {
                    self.player_window = Some(self.settings.player.clone());
                }
            }
            Command::ProgressorSettings => {
                if self.progressor_window.is_none() {
                    self.progressor_window = Some(progressor::SettingsWindow {
                        original: self.settings.progressor.clone(),
                    });
                }
            }
            Command::ToggleLiveKeys => {
                self.settings.live_keys = !self.settings.live_keys;
                self.save_settings();
            }
            Command::KeyScope(s) => {
                self.settings.key_scope = s;
                self.settings.key_follow_roll = false;
                self.save_settings();
            }
            Command::KeyFollowRoll => {
                self.settings.key_follow_roll = true;
                self.save_settings();
            }
            Command::AuditionLength(ms) => {
                self.settings.audition_ms =
                    ms.clamp(settings::MIN_AUDITION_MS, settings::MAX_AUDITION_MS);
                self.save_settings();
            }
            Command::ToggleAudition => {
                self.settings.audition = !self.settings.audition;
                if !self.settings.audition {
                    self.audition_release_all();
                }
                self.save_settings();
            }
            Command::SelectionOpacity => {
                self.dialog = Some(Dialog::Percent {
                    what: dialogs::PercentKind::SelectionOpacity,
                    text: self.settings.selection_opacity.to_string(),
                    error: None,
                    focused: false,
                });
            }
            Command::FollowPosition => {
                self.dialog = Some(Dialog::Percent {
                    what: dialogs::PercentKind::FollowPosition,
                    text: self.settings.follow_position.to_string(),
                    error: None,
                    focused: false,
                });
            }
            Command::RecentTasks => {
                let mut lines: Vec<String> = self.tasks.recent().map(|r| r.text()).collect();
                lines.reverse();
                self.dialog = Some(Dialog::Message {
                    title: "Recent Operations".into(),
                    body: if lines.is_empty() {
                        "No operation has been timed yet.".into()
                    } else {
                        lines.join("\n")
                    },
                    is_error: false,
                });
            }
            Command::Tool(t) => {
                self.roll.cancel_gesture();
                self.roll.tool = t;
            }
            Command::ZoomIn | Command::ZoomOut => {
                let f = if cmd == Command::ZoomIn { 1.25 } else { 0.8 };
                if let Some(o) = &self.open {
                    let doc = o.doc.clone();
                    self.roll.zoom_by(f, &doc, self.roll_width);
                }
            }
            Command::PlayPause => self.play_pause(),
            Command::Stop => self.stop_playback(),
            Command::ToggleAnchorPlayhead => {
                self.settings.anchor_playhead = !self.settings.anchor_playhead;
                self.apply_playhead_policy();
                self.save_settings();
            }
            Command::GoToStart => self.seek_to(0),
            Command::GoToEnd => {
                if let Some(end) = self.doc().map(PlumDocument::duration_ticks) {
                    self.seek_to(end);
                }
            }
            Command::PreviousMeasure => {
                let pos = self.transport.position();
                if let Some(t) = self.meter_map().map(|m| m.previous_bar(pos)) {
                    self.seek_to(t);
                }
            }
            Command::NextMeasure => {
                let pos = self.transport.position();
                if let Some(t) = self.meter_map().and_then(|m| m.next_bar(pos)) {
                    self.seek_to(t);
                }
            }
            Command::ChooseSoundFont => self.choose_soundfont(frame),
            Command::ToggleDevOverlay => self.dev_overlay = !self.dev_overlay,
            Command::SelectRange => self.open_select_range(),
            Command::ToggleLoop => self.toggle_loop(),
            Command::LoopToSelection => self.loop_to_selection(),
            Command::ClearLoop => self.set_loop_range(None, false),
            Command::PlayingTime => self.open_playing_time(),
            Command::TrackProperties => {
                let i = self.active_track();
                self.open_track_properties(i);
            }
            Command::FitTrackEnd => {
                let i = self.active_track();
                self.fit_track_end(i);
            }
            Command::PlayAll => self.play_all(),
            Command::PlayOnlySelected => self.play_only_selected(),
            Command::RestoreAllTracks => self.restore_all_tracks(),
            Command::MuteCurrentTrack => self.toggle_mute(self.active_track()),
            Command::SoloCurrentTrack => self.toggle_solo(self.active_track(), false),
            Command::ToggleGraph2 => {
                let v = !self.graph_slot(1).visible;
                self.update_graph_slot(1, |s| s.visible = v);
                if v {
                    self.settings.velocity_lane_visible = true;
                    self.activate_graph(1);
                } else {
                    self.activate_graph(0);
                }
            }
            Command::ToggleEventList => self.event_list_open = !self.event_list_open,
            Command::CustomFunctions => {
                self.dialog = Some(Dialog::CurveFunctions(Box::new(
                    functions::FunctionsDraft::new(
                        &self.settings.curve_presets,
                        &self.settings.curve.custom,
                    ),
                )));
            }
            Command::ToggleMetrics => {
                self.settings.show_metrics = !self.settings.show_metrics;
                self.save_settings();
            }
            Command::ToggleExplicitStatus => {
                self.settings.export_explicit_status = !self.settings.export_explicit_status;
                self.save_settings();
            }
            Command::TogglePlayerSafeStart => {
                self.settings.export_player_safe_start = !self.settings.export_player_safe_start;
                self.save_settings();
            }
            Command::ToggleLoudnessReference => {
                self.settings.export_loudness_reference = !self.settings.export_loudness_reference;
                self.save_settings();
            }
        }
    }

    fn set_theme(&mut self, t: ThemeSetting, ctx: &egui::Context) {
        self.settings.theme = t;
        theme::apply(
            ctx,
            t,
            self.settings.high_contrast,
            self.settings.ui_accent_color,
        );
        log::info!("theme changed to {}", t.as_str());
        self.save_settings();
    }

    /// B1-0117: the General Note Overlap Prevention to run, when it is on.
    pub(super) fn general_overlap(&self) -> Option<plumspirit_engine::transform::overlap::General> {
        self.settings.overlap.active()
    }

    fn save_settings(&self) {
        let Some(dir) = &self.config_dir else {
            return;
        };
        let mut s = self.settings.clone();
        if let Some(size) = self.window_size {
            s.window_size = Some(size);
        }
        s.plugins = self.plugin_memory();
        if let Err(e) = settings::save(dir, &s) {
            log::error!("could not save settings to {}: {e}", dir.display());
        }
    }

    // ------------------------------------------------------------- Onion Skin

    fn set_onion_mode(&mut self, mode: OnionMode) {
        let Some(doc) = self.doc_mut() else {
            return;
        };
        doc.project_mut().onion.mode = mode;
        let empty = doc.project().onion.specific.is_empty();
        if mode == OnionMode::Specific && empty {
            self.onion_choice = Some(Vec::new());
        }
    }

    /// B1-0064: Onion Skin off, or back on with the last mode (Specific
    /// Tracks when a list exists, else Previous + Next).
    fn toggle_onion(&mut self) {
        let last = self.onion_last_mode;
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let o = &mut doc.project_mut().onion;
        if o.mode != OnionMode::Off {
            let was = o.mode;
            o.mode = OnionMode::Off;
            self.onion_last_mode = was;
            return;
        }
        let has_list = !o.specific.is_empty();
        o.mode = match last {
            OnionMode::Off | OnionMode::Specific if has_list => OnionMode::Specific,
            OnionMode::Off | OnionMode::Specific => OnionMode::PreviousAndNext,
            m => m,
        };
    }

    /// B1-0065: the readouts' chips — a click collapses / expands the
    /// readout, a drag moves it (its offset from its corner), saved.
    fn overlay_chips(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        for (k, view) in self.overlay_views.into_iter().enumerate() {
            let Some(v) = view else { continue };
            let r = ui
                .interact(
                    v.chip,
                    ui.id().with(("overlay-chip", k)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_text(if k == 1 {
                    "Click: every line / only the first (compact, the default). Hold Alt over the readout: every line while held. Drag: move it."
                } else {
                    "Click: show all lines / only the first. Drag: move it."
                });
            let corner = if k == 0 {
                self.settings.progressor.corner
            } else {
                self.selection_corner()
            };
            let ps = &mut self.settings.progressor;
            let (collapsed, offset) = if k == 0 {
                (&mut ps.collapsed, &mut ps.offset)
            } else {
                (&mut ps.selection_collapsed, &mut ps.selection_offset)
            };
            if r.clicked() {
                *collapsed = !*collapsed;
                self.save_settings();
            } else if r.dragged() {
                *offset = progressor::drag_offset(*offset, corner, r.drag_delta(), area);
                self.settings_save_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(800));
            }
            // L-E01: the Selection Analytics caret shows / hides its charts.
            if let Some(caret) = v.advanced {
                let r = ui
                    .interact(
                        caret,
                        ui.id().with(("overlay-caret", k)),
                        egui::Sense::click(),
                    )
                    .on_hover_text(if self.settings.selection_advanced {
                        "Hide the selection charts (the counters stay)."
                    } else {
                        "Show the selection charts: NPS, polyphony, note counter and tempo over the selected notes."
                    });
                if r.clicked() {
                    self.set_selection_charts(!self.settings.selection_advanced);
                }
            }
        }
    }

    fn active_track_id(&self) -> Option<TrackId> {
        let o = self.open.as_ref()?;
        o.doc.track(o.active_track).map(|t| t.id())
    }

    /// B1-0064 Per-Track Settings: when the active track changed (by any
    /// path — clicks, keys, undo), the Onion mode and list in force are kept
    /// for the previous track and the new track's own come back. Called at
    /// the start of every frame; cheap when nothing changed.
    pub(super) fn sync_onion_track(&mut self) {
        let current = self.active_track_id();
        if current == self.onion_track_seen {
            return;
        }
        let Some(new) = current else {
            self.onion_track_seen = None;
            return;
        };
        let prev = self.onion_track_seen;
        if self.doc().is_some_and(|d| d.project().onion.per_track) {
            let Some(doc) = self.doc_mut() else {
                return; // a background job holds the document: next frame
            };
            let mut c = doc.project().onion.clone();
            if c.switch_track(prev, new) {
                doc.project_mut().onion = c;
            }
        }
        self.onion_track_seen = Some(new);
    }

    /// L-G03: `tracks` become the only ghosts (Replace; see
    /// `OnionConfig::with_only_tracks_at`).
    fn set_onion_only(&mut self, tracks: &[usize]) {
        let row = self.onion_row();
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let ids: Vec<TrackId> = doc.tracks().iter().map(|t| t.id()).collect();
        if let Some(next) = doc.project().onion.with_only_tracks_at(&ids, row, tracks) {
            doc.project_mut().onion = next;
        }
    }

    /// B1-0064: shows / hides `tracks` as ghosts (see `OnionConfig::with_tracks`).
    fn set_onion_tracks(&mut self, tracks: &[usize], on: bool) {
        let row = self.onion_row();
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let ids: Vec<TrackId> = doc.tracks().iter().map(|t| t.id()).collect();
        let before = doc.project().onion.mode;
        let next = doc.project().onion.with_tracks_at(&ids, row, tracks, on);
        let turned_off = before != OnionMode::Off && next.mode == OnionMode::Off;
        doc.project_mut().onion = next;
        if turned_off {
            self.onion_last_mode = OnionMode::Specific;
        }
    }

    fn set_onion_opacity(&mut self, v: u8) {
        if let Some(doc) = self.doc_mut() {
            doc.project_mut().onion.opacity = OnionConfig::clamp_opacity(v);
        }
    }

    // ------------------------------------------------------------- UI helpers

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = window_title(self.open.as_ref().map(|o| (&o.source, o.name(), o.dirty())));
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn meter_map(&mut self) -> Option<&MeterMap> {
        let o = self.open.as_ref()?;
        let key = (
            o.generation,
            o.doc.structure_revision(),
            if o.doc.source_format() == plumspirit_engine::model::SmfFormat::Sequential {
                o.active_track
            } else {
                0
            },
            o.doc.conductor().meter_revision(),
        );
        if self.meter.as_ref().is_none_or(|(k, _)| *k != key) {
            let events = o.doc.meter_events(o.active_track);
            self.meter = Some((key, MeterMap::new(o.doc.division(), &events)));
        }
        self.meter.as_ref().map(|(_, m)| m)
    }

    /// Tempo map for time readouts (display only; playback builds its own).
    fn display_tempo(&mut self) -> Option<Arc<TempoMap>> {
        let o = self.open.as_ref()?;
        let key = (
            o.generation,
            o.doc.structure_revision(),
            o.doc.tempo_revision(),
        );
        if self.display_tempo.as_ref().is_none_or(|(k, _)| *k != key) {
            self.display_tempo = Some((key, Arc::new(TempoMap::for_document(&o.doc, 48_000))));
        }
        self.display_tempo.as_ref().map(|(_, m)| Arc::clone(m))
    }

    fn status_line(&self) -> String {
        match &self.open {
            None => "No project open".into(),
            Some(o) if o.conductor_selected => {
                let c = o.doc.conductor();
                format!(
                    "Conductor · {} tempo point(s) · {} time signature(s) · {} marker(s) · {} notes in project",
                    c.tempo_points().len(),
                    c.time_signatures().len(),
                    c.markers().len(),
                    group_digits(o.doc.stored_note_count() as u64)
                )
            }
            Some(o) => {
                let d = &o.doc;
                let name = d.track_label(o.active_track);
                format!(
                    "Track {} of {} ({name}) · {} notes in project · {} selected",
                    o.active_track + 1,
                    d.track_count(),
                    // v0.9: with independent clip copies, as many as a save writes.
                    group_digits(d.stored_note_count() as u64),
                    // The roll's notes (an opened clip's content; V09-S11).
                    group_digits(self.edit_notes().map_or(0, |n| n.selected_count()) as u64),
                )
            }
        }
    }

    fn position_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = self.roll.pointer.pitch {
            parts.push(plumspirit_engine::model::note::key_label(p));
        }
        if let (Some(t), Some((_, m))) = (self.roll.pointer.song_tick, &self.meter) {
            parts.push(format!("{} · tick {}", m.describe(t), group_digits(t)));
        }
        parts.join(" · ")
    }

    /// Renderer / GPU choice (applies at the next start, §102).
    fn set_renderer(
        &mut self,
        b: crate::renderer::Backend,
        gpu: Option<crate::renderer::GpuPreference>,
    ) {
        let changed =
            b != self.settings.renderer || gpu.is_some_and(|g| g != self.settings.gpu_preference);
        self.settings.renderer = b;
        if let Some(g) = gpu {
            self.settings.gpu_preference = g;
        }
        self.save_settings();
        if changed {
            let msg = if b == self.renderer_run.running && gpu.is_none() {
                format!("{} is the renderer in use.", b.label())
            } else {
                format!(
                    "Restart Required: PlumSpirit uses {} from the next start.",
                    b.label()
                )
            };
            self.set_status(msg, StatusKind::Info);
        }
    }

    /// Once the first frame has been drawn: the Vulkan start marker goes
    /// (the start worked) and fallback notices are shown.
    fn renderer_frame(&mut self) {
        if self.renderer_frames >= 2 {
            return;
        }
        self.renderer_frames += 1;
        if self.renderer_frames < 2 {
            return;
        }
        if let Some(dir) = self.config_dir.as_deref() {
            crate::renderer::clear_marker(dir);
        }
        // §210: GPU initialisation (window, context, first frames) in the
        // task list, once per run.
        if let Some(t0) = self.renderer_run.started.take() {
            let api = &self.renderer_info.api;
            let name = if api.is_empty() {
                "Started the renderer".to_owned()
            } else {
                format!("Started the renderer ({api})")
            };
            self.tasks
                .record(name, t0.elapsed(), tasks::TaskOutcome::Done);
        }
        if !self.renderer_run.notices.is_empty() && self.dialog.is_none() {
            self.dialog = Some(Dialog::Message {
                title: "Renderer".into(),
                body: self.renderer_run.notices.join("\n\n"),
                is_error: false,
            });
        }
    }

    fn dev_overlay_ui(&self, ui: &mut egui::Ui, rect: egui::Rect, pal: &theme::Palette) {
        let s = &self.roll.stats;
        let doc_heap = self.doc().map_or(0, |d| d.heap_bytes() as u64);
        // L-S48: where the slowest paint of the window went (its largest
        // parts), e.g. a 765 ms frame after a 224M-note Import Into.
        let slowest_paint = match self.perf.slowest_paint() {
            Some((ms, parts)) if ms >= 1.0 => {
                let mut named: Vec<(f32, &str)> = parts
                    .iter()
                    .copied()
                    .zip(crate::diagnostics::PAINT_PARTS)
                    .filter(|(v, _)| *v >= 0.05)
                    .collect();
                named.sort_by(|a, b| b.0.total_cmp(&a.0));
                let list: Vec<String> = named
                    .iter()
                    .take(3)
                    .map(|(v, n)| format!("{n} {v:.1}"))
                    .collect();
                (
                    format!("slowest paint (last 120): {ms:.1} ms"),
                    format!("  {}", list.join(" · ")),
                )
            }
            _ => (
                "slowest paint (last 120): under 1 ms".to_owned(),
                String::new(),
            ),
        };
        // L-S49: the frame of the window with the most CPU time, split into
        // the update, egui's end of pass and eframe's work after the pass
        // (tessellation, texture uploads, painting).
        let slowest_frame = match self.perf.slowest_split() {
            Some(f) => (
                format!(
                    "slowest frame (last 120): {:.1} ms · outside the update {:.1}",
                    f.cpu_ms,
                    f.after_pass_ms()
                ),
                format!(
                    "  update {:.1} (begin {:.1}, exit {:.1}) · egui end {:.1} · {} pass{}",
                    f.pass.ui_ms,
                    f.pass.begin_ms,
                    f.pass.exit_ms,
                    f.pass.end_ms,
                    f.pass.passes,
                    if f.pass.passes == 1 { "" } else { "es" },
                ),
                match (f.pass.tessellate_ms, f.pass.draw_ms, f.rest_ms()) {
                    (Some(t), Some(d), Some(r)) => {
                        format!("  tessellate {t:.1} · draw {d:.1} · rest {r:.1}")
                    }
                    _ => "  no paint probes (Vulkan)".to_owned(),
                },
                format!(
                    "  {} shapes ({} mesh vertices) · textures +{} ({}) −{}",
                    group_digits(f.pass.shapes as u64),
                    group_digits(f.pass.mesh_vertices as u64),
                    f.pass.textures_set,
                    format_bytes(f.pass.texture_bytes),
                    f.pass.textures_freed
                ),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        let slowest_update = match self.perf.slowest_update() {
            Some((ms, parts)) => (
                format!("slowest update (last 120): {ms:.1} ms"),
                format!(
                    "  {}",
                    parts
                        .iter()
                        .take(3)
                        .map(|(n, v)| format!("{n} {v:.1}"))
                        .collect::<Vec<_>>()
                        .join(" · ")
                ),
            ),
            None => (String::new(), String::new()),
        };
        let perf = match self.perf.summary() {
            Some((last, max_ui, max_paint)) => (
                format!("UI frame CPU: {last:.1} ms · max {max_ui:.1} ms"),
                format!("roll paint max: {max_paint:.1} ms (last 120 frames)"),
            ),
            None => ("UI frame CPU: n/a".to_owned(), String::new()),
        };
        let history = self.open.as_ref().map_or_else(String::new, |o| {
            format!(
                "undo: {} entries · {} of {} · evicted {}",
                o.history.undo_len(),
                format_bytes(o.history.bytes() as u64),
                format_bytes(o.history.limits().max_bytes as u64),
                o.history.evicted()
            )
        });
        let out = self.transport.output_status();
        let mut lines = vec![
            format!(
                "midi output: {} ({}) · {}",
                out.target.label(),
                out.target.kind().name(),
                out.health_text()
            ),
            format!(
                "render: {}",
                if s.lod {
                    dense_line(&s.dense)
                } else {
                    "L0 exact rectangles".to_owned()
                }
            ),
            format!(
                "onion: {}",
                if s.onion_dense.visible_tiles > 0 {
                    dense_line(&s.onion_dense)
                } else {
                    "exact".to_owned()
                }
            ),
            format!(
                "tiles: {} cached ({}) · generation {} · last build {} µs · backend {}",
                s.dense.cached.max(s.onion_dense.cached),
                format_bytes(s.dense.cache_bytes.max(s.onion_dense.cache_bytes) as u64),
                s.dense.generation.max(s.onion_dense.generation),
                s.dense
                    .last_build_micros
                    .max(s.onion_dense.last_build_micros),
                self.renderer_info.line()
            ),
            format!(
                "visible estimate: {}",
                group_digits(s.visible_estimate as u64)
            ),
            format!(
                "queried / scanned: {} / {}",
                group_digits(s.notes_queried as u64),
                group_digits(s.notes_scanned as u64)
            ),
            format!("chunks visited: {} of {}", s.chunks_visited, s.chunks_total),
            format!(
                "primitives: {} · grid lines: {}",
                group_digits(s.primitives as u64),
                s.grid_lines
            ),
            slowest_paint.0,
            slowest_paint.1,
            slowest_update.0,
            slowest_update.1,
            slowest_frame.0,
            slowest_frame.1,
            slowest_frame.2,
            slowest_frame.3,
            format!(
                "notes pass: {} · roll paint: {}{}",
                format_duration(s.query_time),
                format_duration(s.frame_time),
                if s.lod && s.dense.pending == 0 {
                    " (tiles cached)"
                } else {
                    ""
                }
            ),
            format!(
                "onion: {}{} visible · {} prims · {}",
                if s.onion_estimate_partial { "≥ " } else { "" },
                group_digits(s.onion_estimate as u64),
                group_digits(s.onion_primitives as u64),
                format_duration(s.onion_time)
            ),
            format!(
                "velocity lane: {}{}",
                format_duration(s.lane_time),
                if s.lane_aggregated {
                    " (aggregated)"
                } else {
                    ""
                }
            ),
            perf.0,
            perf.1,
            format!(
                "document heap: {} · process: {}",
                format_bytes(doc_heap),
                diagnostics::process_memory_bytes().map_or_else(|| "n/a".into(), format_bytes)
            ),
            history,
            format!(
                "analysis: {}{} · key rebuilds: {}",
                format_bytes(self.analysis.heap_bytes() as u64),
                if self.analysis.busy() {
                    " (updating)"
                } else {
                    ""
                },
                self.keys.rebuilds
            ),
        ];
        if let Some(st) = self.sel_stats.update_stats() {
            // What the last Selection Analytics analysis re-did (V09-S19).
            let blocks =
                |b: Option<usize>| b.map_or_else(|| "rebuilt".to_owned(), |n| n.to_string());
            lines.push(if st.full {
                format!(
                    "selection: full build · {} pieces · {}",
                    st.pieces_built,
                    format_bytes(self.sel_stats.heap_bytes() as u64)
                )
            } else {
                format!(
                    "selection: update · {} of {} pieces built · blocks {} / {} · {}",
                    st.pieces_built,
                    st.pieces_built + st.pieces_reused,
                    blocks(st.profile_blocks),
                    blocks(st.unique_blocks),
                    format_bytes(self.sel_stats.heap_bytes() as u64)
                )
            });
        }
        if let Some(a) = self.transport.diagnostics() {
            lines.extend(a);
        }
        lines.push(format!("font: {}", self.font.family));
        let font = egui::FontId::proportional(theme::size::SMALL);
        let h = lines.len() as f32 * 15.0 + 10.0;
        let w = 360.0;
        let r = egui::Rect::from_min_size(
            egui::pos2(rect.right() - w - 16.0, rect.top() + 30.0),
            egui::vec2(w, h),
        );
        let p = ui.painter_at(rect);
        p.rect_filled(r, 0.0, pal.overlay_bg);
        p.rect_stroke(
            r,
            0.0,
            egui::Stroke::new(1.0, pal.panel_border),
            egui::StrokeKind::Inside,
        );
        for (i, l) in lines.iter().enumerate() {
            p.text(
                egui::pos2(r.left() + 8.0, r.top() + 5.0 + i as f32 * 15.0),
                egui::Align2::LEFT_TOP,
                l,
                font.clone(),
                pal.text,
            );
        }
    }

    /// B1-0170: closing never waits on the project's size. Settings are
    /// saved, playback stops with every note off, the undo journal is
    /// closed; workers are told to stop and left (never joined — a build of
    /// a huge index could take seconds); the document, its history, the
    /// analysis, the tiles and the other caches are left to the system,
    /// which frees the whole process at once, instead of being freed
    /// allocation by allocation while the window stays on screen. Each stage
    /// is timed in the log. Returns the stage timings (tests).
    pub(super) fn shutdown_bounded(&mut self) -> Vec<(&'static str, std::time::Duration)> {
        use std::time::Instant;
        let mut stages = Vec::new();
        let mut t = Instant::now();
        let mut lap = |name: &'static str, t: &mut Instant| {
            stages.push((name, t.elapsed()));
            *t = Instant::now();
        };
        log::info!("shutting down");
        self.lua_file_dialog = None;
        self.lua_midi_input.clear();
        for view in self.lua_panels.drain(..) {
            view.cancel();
        }
        self.save_settings();
        lap("settings saved", &mut t);
        self.transport.shutdown();
        lap("playback stopped (all notes off)", &mut t);
        if let Some(o) = self.open.as_mut()
            && let Some(j) = o.history.detach_journal()
        {
            drop(j);
        }
        lap("undo journal closed", &mut t);
        if let Some(job) = self.job.take() {
            // Cancelled; its thread ends on its own.
            drop(job.retire());
        }
        self.retired.clear();
        self.graphs.cancel();
        // A running Lua script stops at its next check (never joined here).
        self.scripts.cancel_all();
        lap("workers told to stop", &mut t);
        std::mem::forget(self.open.take());
        std::mem::forget(std::mem::take(&mut self.analysis));
        std::mem::forget(std::mem::take(&mut self.sel_stats));
        std::mem::forget(std::mem::take(&mut self.sel_charts));
        std::mem::forget(std::mem::take(&mut self.inspector));
        std::mem::forget(std::mem::take(&mut self.player));
        std::mem::forget(std::mem::take(&mut self.track_list));
        self.roll.abandon_caches();
        lap("project memory left to the system", &mut t);
        // Stops and joins the CPU / RAM sampler (it wakes every second).
        self.metrics = None;
        lap("sampler stopped", &mut t);
        let total: std::time::Duration = stages.iter().map(|s| s.1).sum();
        log::info!(
            "shut down in {:.1} ms ({})",
            total.as_secs_f64() * 1e3,
            stages
                .iter()
                .map(|(n, d)| format!("{n} {:.1} ms", d.as_secs_f64() * 1e3))
                .collect::<Vec<_>>()
                .join(", ")
        );
        stages
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allowed_to_close {
            return;
        }
        if matches!(
            self.job.as_ref().map(|j| &j.kind),
            Some(JobKind::Export { .. } | JobKind::Save { .. })
        ) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.close_after_job = true;
            self.set_status("Finishing before closing…", StatusKind::Info);
            return;
        }
        if let Some(o) = self.open.as_ref().filter(|o| o.dirty()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Dirty {
                name: o.name().to_owned(),
                then: Then::Exit,
                asks_path: !matches!(o.source, DocSource::Project(_)),
                note: o.discard_note(),
            });
        }
    }

    /// Transport keys are read before any widget runs so a focused button cannot
    /// also react to Space.
    fn transport_shortcuts(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // L-F05: while a menu is open its keys move the focus, toggle
        // (Space / Enter) and close it (Esc); no shortcut runs underneath.
        if self.dialog.is_some()
            || egui::Popup::is_any_open(ctx)
            || ctx.egui_wants_keyboard_input()
            || self.onion_choice.is_some()
            || self.track_list.has_window()
            || self.keybind_edit.capturing()
        {
            return;
        }
        let pairs: Vec<(KeyboardShortcut, Command)> = keybinds::ordered(true)
            .into_iter()
            .map(|k| (KeyboardShortcut::new(k.mods, k.key), k.command))
            .collect();
        let mut fired = Vec::new();
        ctx.input_mut(|i| {
            for (s, c) in &pairs {
                if keybinds::consume(i, s) {
                    fired.push(*c);
                }
            }
        });
        for c in fired {
            self.execute(c, ctx, frame);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let guarded = self.dialog.is_some()
            || egui::Popup::is_any_open(ctx)
            || self.keybind_edit.capturing()
            || self.spiderweb.captures_keys()
            || self.onion_choice.is_some()
            // v0.9 (V09-S08): the Track List's Rename / Clip Color windows.
            || self.track_list.has_window()
            || ctx.egui_wants_keyboard_input()
            || self.focus_at_frame_start
            || self.roll.is_dragging()
            || self.track_list.is_dragging()
            || self.graphs.is_dragging();
        // v0.9 (V09-S08 / V09-S17): note commands' keys do nothing while the
        // Track List or the MIDI Player is shown (they would act on notes it
        // does not show) — `run_chord` decides, for the clipboard events too.
        // egui-winit turns Ctrl+C / Ctrl+V into `Event::Copy` / `Event::Paste(text)`
        // instead of key presses, and emits nothing at all for Ctrl+V while the system
        // clipboard holds no text. Both the Paste event and a Ctrl+V release
        // without that event request the current musical payload. Text fields
        // keep their regular Paste event and are handled by the focus guard.
        let (mut copy, mut paste, mut cut) = (false, false, false);
        let held = ctx.input(|i| i.modifiers);
        let mut seen = self.paste_event_seen;
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    Event::Copy => copy = true,
                    Event::Cut => cut = true,
                    Event::Paste(_) => {
                        paste = true;
                        seen = true;
                    }
                    Event::Key {
                        key,
                        pressed: false,
                        modifiers,
                        ..
                    } if matches!(key, Key::V | Key::Paste)
                        || (cfg!(windows) && *key == Key::Insert && modifiers.shift) =>
                    {
                        if (*key != Key::V || modifiers.command) && !seen {
                            paste = true;
                        }
                        seen = false;
                    }
                    _ => {}
                }
            }
        });
        self.paste_event_seen = seen;
        if guarded {
            return;
        }
        // Esc leaves the MIDI Player (P10), as F10 does.
        if self.player_shown()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.set_player(false);
            ctx.request_repaint();
        }
        // B2-F29: the clipboard events go where the chords go (`run_chord`):
        // in the Track List Ctrl+C / Ctrl+V / Ctrl+X act on the clips (they
        // were dropped there before), in the Piano Roll on the notes.
        if copy {
            self.run_chord(Command::Copy, ctx, frame);
        }
        if paste {
            self.run_chord(Command::Paste, ctx, frame);
        }
        // v0.9 §197: on Windows Ctrl+Shift+Delete (Delete Selected Tracks)
        // also arrives as Cut, never as a key press.
        let cut_cmd = keybinds::cut_command(held, cfg!(target_os = "windows"));
        if cut {
            self.run_chord(cut_cmd, ctx, frame);
        }
        // From the central registry; rows with Shift come first: egui matches
        // Shift loosely, so Ctrl+S would otherwise also consume Ctrl+Shift+S.
        let pairs: Vec<(KeyboardShortcut, Command)> = keybinds::ordered(false)
            .into_iter()
            .map(|k| (KeyboardShortcut::new(k.mods, k.key), k.command))
            .collect();
        let mut fired = Vec::new();
        ctx.input_mut(|i| {
            for (s, c) in &pairs {
                if keybinds::consume(i, s) {
                    fired.push(*c);
                }
            }
        });
        // Ctrl+X arrives as the Cut event above; never run it twice.
        fired.retain(|c| {
            !((cut && *c == cut_cmd)
                || (copy && *c == Command::Copy)
                || (paste && *c == Command::Paste))
        });
        if copy || paste || cut || !fired.is_empty() {
            // The roll was drawn before the shortcut acted; show the result now.
            ctx.request_repaint();
        }
        for c in fired {
            self.run_chord(c, ctx, frame);
        }
        // B1-0047: custom tools' shortcuts (never one PlumSpirit uses).
        self.custom_tool_shortcuts(ctx);
    }

    /// A keyboard chord's command: v0.9 §198 — Delete and Ctrl+D act on the
    /// selected clips while the Track List is shown, and note commands do
    /// nothing there (they would act on notes it does not show).
    fn run_chord(&mut self, c: Command, ctx: &egui::Context, frame: &eframe::Frame) {
        let player = self.player_shown();
        let in_track_list =
            !player && self.main_view == track_list::MainView::TrackList && self.open.is_some();
        if in_track_list && c == Command::SelectAll {
            // B2-F29: Ctrl+A selects every clip.
            self.select_all_clips();
        } else if in_track_list && let Some(intent) = self.track_list.key_intent(c) {
            self.apply_clip_intent(intent);
        } else if in_track_list
            && c == Command::Paste
            && (self.uses_system_clipboard() || self.clip_clipboard.is_some())
        {
            // B1-0097: Ctrl+V pastes the copied clips at the edit cursor.
            self.apply_clip_intent(track_list::ClipIntent::Paste {
                track: None,
                at: None,
            });
        } else if in_track_list && let Some(why) = self.clip_key_refusal(c) {
            // B2-F29: a clip key with nothing to act on says why (never
            // falls through to the notes the Track List does not show).
            self.set_status(why, StatusKind::Info);
        } else if !((in_track_list || player) && c.acts_on_notes()) {
            self.execute(c, ctx, frame);
        }
    }

    /// What the central area shows over either view: a running job's
    /// progress, the loading label, the developer overlay.
    fn central_overlays(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        pal: &theme::Palette,
        loading_label: Option<&str>,
    ) {
        if let Some(label) = self
            .job
            .as_ref()
            .filter(|j| !matches!(j.kind, JobKind::Import { .. } | JobKind::LoadProject { .. }))
            .map(RunningJob::label)
        {
            let p = ui.painter_at(rect);
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(260.0, 36.0));
            p.rect_filled(r, 0.0, pal.overlay_bg);
            p.rect_stroke(
                r,
                0.0,
                egui::Stroke::new(1.0, pal.panel_border),
                egui::StrokeKind::Inside,
            );
            let pct = self
                .job
                .as_ref()
                .and_then(RunningJob::fraction)
                .map(|f| format!(" {:.0}%", f * 100.0))
                .unwrap_or_default();
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                format!("{label}{pct}"),
                egui::FontId::proportional(theme::size::BODY),
                pal.text,
            );
        } else if let Some(label) = loading_label {
            let p = ui.painter_at(rect);
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(300.0, 36.0));
            p.rect_filled(r, 0.0, pal.overlay_bg);
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(theme::size::BODY),
                pal.text,
            );
        }
        if self.dev_overlay {
            self.dev_overlay_ui(ui, rect, pal);
        }
    }

    /// Tempo shown in the transport row: the Conductor's tempo at tick 0 (the MIDI
    /// default for a file without a tempo event there).
    fn tempo_display(&self) -> String {
        self.doc()
            .map(|d| plumspirit_engine::model::conductor::format_tempo(d.conductor().start_tempo()))
            .unwrap_or_default()
    }
}

/// (document generation, structure, tempo revision, basis) of a cached time map.
type TimeKey = (u64, u64, u64, TimeBasis);

/// The Onion Skin's ghost sources in drawing order (back to front). Before
/// clips (`clips_are_identity`) every Onion track is its own notes where
/// they are, exactly as in v0.8; otherwise (V09-S11, §042) each clip of an
/// Onion track is a source where it plays — its content moved by the
/// placement's `start − offset`, only the notes its window plays — so the
/// ghosts are the song as the Track List shows it and playback plays it.
/// `song_range` is the view in song ticks (layer order by density). The
/// placements are resolved once per clip layout (`OnionOrderCache::places`).
fn onion_sources<'a>(
    doc: &'a PlumDocument,
    generation: u64,
    ghosts: &[usize],
    order: &mut hierarchy::OnionOrderCache,
    cfg: &plumspirit_engine::model::OnionConfig,
    song_range: (u64, u64),
) -> Vec<OnionSource<'a>> {
    use plumspirit_engine::model::project::OnionOrder;
    use plumspirit_engine::view::tiles::SourcePlace;
    if ghosts.is_empty() {
        return Vec::new();
    }
    if doc.clips_are_identity() {
        let ordered = order.order(doc, ghosts, cfg, song_range, None);
        return ordered
            .iter()
            .filter_map(|&i| doc.track(i))
            .map(|t| OnionSource {
                id: t.id(),
                store: &t.notes,
                color: t.props().onion_color,
                place: SourcePlace::IDENTITY,
            })
            .collect();
    }
    // Every ghost track's clips, in track order.
    let mut flat: Vec<OnionSource<'a>> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(ghosts.len());
    {
        let places = order.places(doc, generation);
        for &i in ghosts {
            let from = flat.len();
            if let (Some(t), Some(ps)) = (doc.track(i), places.get(i)) {
                flat.extend(ps.iter().filter_map(|p| {
                    Some(OnionSource {
                        id: t.id(),
                        store: p.content.notes(doc)?,
                        color: t.props().onion_color,
                        place: p.place,
                    })
                }));
            }
            spans.push((from, flat.len()));
        }
    }
    if cfg.order == OnionOrder::Project || ghosts.len() < 2 {
        return flat;
    }
    let groups: Vec<&[OnionSource<'a>]> = spans.iter().map(|&(a, b)| &flat[a..b]).collect();
    let ordered = order.order(doc, ghosts, cfg, song_range, Some(&groups));
    let at: std::collections::HashMap<usize, usize> =
        ghosts.iter().enumerate().map(|(k, &i)| (i, k)).collect();
    ordered
        .iter()
        .filter_map(|i| at.get(i))
        .flat_map(|&k| groups[k].iter().copied())
        .collect()
}

fn controllers_global(t: GraphTarget) -> bool {
    plumspirit_engine::editor::controllers::graph_is_global(t)
}

/// Formats seconds as m:ss.mmm (h:mm:ss for long songs).
pub(crate) fn format_clock(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "—".into();
    }
    let total_ms = (secs * 1000.0).round() as u64;
    let (h, rem) = (total_ms / 3_600_000, total_ms % 3_600_000);
    let (m, rem) = (rem / 60_000, rem % 60_000);
    let (s, ms) = (rem / 1000, rem % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

impl eframe::App for PlumSpiritApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.reset_lua_upload_budget(ui.ctx());
        // L-S48: where this update's time goes (the F12 overlay's slowest
        // update of the last 120).
        crate::diagnostics::FrameClock::app_started(ui.ctx());
        let mut clock = crate::diagnostics::PhaseClock::start();
        let ctx = ui.ctx().clone();
        self.sync_track_colors(&ctx);
        let pal = &theme::ui_palette(&ctx);
        let dark = ctx.theme() == egui::Theme::Dark;
        // L-S49: OpenGL paint probes around this pass's shapes (F12 frame
        // split: tessellation / uploads vs draw calls).
        crate::diagnostics::FrameClock::add_paint_probes(&ctx, frame.gl().is_some());
        // Once a frame: a pass egui runs again (a discarded sizing pass)
        // would record the previous frame twice (L-S49).
        if ctx.current_pass_index() == 0 {
            self.perf.record(
                frame.info().cpu_usage,
                &self.roll.stats,
                crate::diagnostics::FrameClock::take_last(&ctx),
            );
        }
        self.renderer_frame();
        self.bench_frame(&ctx);
        clock.lap("start");
        self.poll_job(&ctx, frame);
        clock.lap("jobs");
        self.release_unused_archive();
        if let Some(path) = self.startup_file.take() {
            self.start_open(path, &ctx);
        }
        self.refresh_recent_status(false);
        let recent_now = crate::recent::now_secs();
        self.handle_close_request(&ctx);
        // B1-0062: the window size is kept at 100 % UI scale (points of the
        // system's own DPI), so a scaled UI does not resize the next start.
        if let Some(r) = ctx.input(|i| i.viewport().inner_rect) {
            self.window_size = Some(ui_scale::unscaled_size(r.size(), ctx.zoom_factor()));
        }
        self.apply_ui_scale(&ctx);
        // Finished retired jobs are joined opportunistically.
        self.retired.retain(|h| !h.is_finished());
        clock.lap("startup");
        self.update_transport(&ctx);
        clock.lap("transport");
        self.sync_clip_session();
        self.sync_onion_track();
        self.poll_history_compaction(&ctx);
        self.autobackup_tick(&ctx);
        // B1-0059: Settings → Performance (cheap assignments).
        let d = self.roll.dense_mut();
        d.configure(self.settings.tile_cache_mb, self.settings.prefetch_tiles);
        d.set_quality(self.settings.render_quality);
        self.watch_journal_failures();
        self.watch_plugin_windows();
        clock.lap("sessions / backup");
        self.update_live(&ctx, dark);
        clock.lap("analysis / live");
        self.update_selection_stats(&ctx);
        clock.lap("selection");
        // Before any widget runs: the focus left over from the last frame.
        self.focus_at_frame_start = ctx.egui_wants_keyboard_input();
        self.transport_shortcuts(&ctx, frame);

        let logo = self
            .brand
            .sized(&ctx, crate::assets::Brand::Logo, menu::LOGO_SIZE);
        let enabled = |c: Command| self.enabled(c);
        let why_disabled = |c: Command| self.why_disabled(c);
        let onion = self.doc().map(|d| d.project().onion.clone());
        let lua_menu = self.lua_menu();
        let custom_menu = self.custom_tool_menu();
        let menu_input = menu::MenuInput {
            enabled: &enabled,
            why_disabled: &why_disabled,
            logo: Some(logo),
            undo_label: self.open.as_ref().and_then(|o| o.history.undo_label()),
            redo_label: self.open.as_ref().and_then(|o| o.history.redo_label()),
            velocity_lane: self.settings.velocity_lane_visible,
            follow: self.settings.follow_playhead,
            onion_mode: onion.as_ref().map(|o| o.mode),
            onion_per_track: onion.as_ref().is_some_and(|o| o.per_track),
            onion_opacity: onion.as_ref().map_or(35, |o| o.opacity),
            loop_on: self.loop_config().active().is_some(),
            graph2: self.graph_slot(1).visible,
            event_list: self.event_list_open,
            explicit_status: self.settings.export_explicit_status,
            onion_order: onion.as_ref().map(|o| o.order).unwrap_or_default(),
            onion_density: onion.as_ref().map(|o| o.density).unwrap_or_default(),
            progressor: self.settings.progressor.enabled,
            midi_player: self.settings.player.enabled,
            midi_inspector: self.settings.inspector,
            scale_y_duplicate: self.settings.scale_y.handles_duplicate,
            scale_y_prevent: self.settings.scale_y.prevent,
            overlap: self.settings.overlap,
            rotate_about_center: self.settings.rotate_about_center,
            onion_outlines: self.settings.onion_outlines,
            onion_outline_darkness: self.settings.onion_outline_darkness,
            main_view: self.main_view,
            skip_channel_10: self.settings.skip_channel_10,
            selection_analytics: self.settings.selection_analytics,
            selection_charts: self.settings.selection_advanced,
            hide_text_overlays: self.settings.hide_text_overlays,
            show_orientation: self.settings.show_orientation,
            recent: self.recent.entries(),
            recent_missing: &self.recent_missing,
            now: recent_now,
            anchor_playhead: self.settings.anchor_playhead,
            pause_rewinds: self.settings.pause_rewinds,
            lua_commands: &lua_menu,
            custom_tools: &custom_menu,
        };
        let mut command: Option<Command> = None;
        let mut menu_out = menu::MenuOutput::default();
        clock.lap("menu input");
        egui::Panel::top("menu")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(8, 2)),
            )
            .show(ui, |ui| {
                menu_out = menu::menu_bar(ui, &menu_input);
            });
        if let Some(c) = menu_out.command {
            command = Some(c);
        }
        if let Some(v) = menu_out.onion_opacity {
            self.set_onion_opacity(v);
        }
        if let Some(v) = menu_out.onion_outline_darkness {
            self.settings.onion_outline_darkness = v.min(100);
            self.save_settings();
        }
        if let Some(v) = menu_out.pause_rewinds {
            self.settings.pause_rewinds = v;
            self.apply_playhead_policy();
            self.save_settings();
        }
        if menu_out.audio_settings {
            self.open_audio_settings();
        }
        if menu_out.view_settings {
            // L-F04: the contextual link of the View menu.
            self.settings_window
                .show_section(settings_window::Section::Appearance);
            self.execute(Command::OpenSettings, &ctx, frame);
        }
        if let Some(a) = menu_out.recent.take() {
            self.apply_recent_action(a, frame, &ctx);
        }
        if let Some((plugin, command)) = menu_out.lua_command.take() {
            self.run_lua_command(&plugin, &command, dark);
        }
        if let Some((plugin, tool)) = menu_out.custom_tool.take() {
            self.open_custom_tool(&plugin, &tool);
        }
        let has_doc = self.open.is_some();
        let summary = self.selection_summary();
        // Default Gate at the edit position (presets follow the time signature there).
        let cursor = self.roll.cursor_tick;
        let gate_info = match (
            self.meter_map().cloned(),
            self.doc().map(PlumDocument::division),
        ) {
            (Some(m), Some(div)) => {
                let presets: Vec<_> = plumspirit_engine::editor::gate::GatePreset::ALL
                    .into_iter()
                    .map(|p| (p, p.ticks(&m, cursor)))
                    .collect();
                (
                    self.settings
                        .default_gate
                        .ticks(div, self.roll.snap, &m, cursor),
                    presets,
                )
            }
            _ => (0, Vec::new()),
        };
        let (zoom_x, zoom_y) = match self.doc() {
            Some(d) if self.main_view == track_list::MainView::TrackList => (
                crate::piano_roll::viewport::format_percent(self.track_list.zoom_x_percent(d)),
                crate::piano_roll::viewport::format_percent(self.track_list.row_percent()),
            ),
            Some(d) => (
                crate::piano_roll::viewport::format_percent(self.roll.zoom_x_percent(d)),
                crate::piano_roll::viewport::format_percent(self.roll.zoom_y_percent()),
            ),
            None => (String::new(), String::new()),
        };
        let can_undo = self.enabled(Command::Undo);
        let can_redo = self.enabled(Command::Redo);
        let can_mute = self.enabled(Command::MuteNotes);
        let can_unmute = self.enabled(Command::UnmuteNotes);
        let tb_input = toolbar::ToolbarInput {
            main_view: self.main_view,
            tool: self.roll.tool,
            snap: self.roll.snap,
            division: self.doc().map(PlumDocument::division),
            has_doc,
            note_tools: !self.conductor_selected()
                && self.job.is_none()
                && self.main_view == track_list::MainView::PianoRoll,
            zoom_x: &zoom_x,
            zoom_y: &zoom_y,
            selected: summary.count,
            gate: summary.gate,
            velocity: summary.velocity,
            default_gate: self.settings.default_gate,
            default_gate_ticks: gate_info.0,
            link_gate: self.settings.link_gate,
            preset_ticks: &gate_info.1,
            can_undo,
            can_redo,
            can_mute,
            can_unmute,
            onion: self.doc().map(|d| {
                let o = &d.project().onion;
                crate::ui::menu::OnionMenuState {
                    mode: o.mode,
                    order: o.order,
                    density: o.density,
                    outlines: self.settings.onion_outlines,
                    outline_darkness: self.settings.onion_outline_darkness,
                    opacity: o.opacity,
                    choose_enabled: self.enabled(Command::ChooseOnionTracks),
                    per_track: o.per_track,
                }
            }),
        };
        let mut tb_out = toolbar::ToolbarOutput::default();
        {
            let fields = toolbar::ToolbarFields {
                zoom_x: &mut self.zoom_x_field,
                zoom_y: &mut self.zoom_y_field,
                gate: &mut self.gate_field,
                velocity: &mut self.velocity_field,
                custom_snap: &mut self.custom_snap_field,
            };
            egui::Panel::top("toolbar")
                .frame(
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .stroke(egui::Stroke::new(1.0, pal.panel_border)),
                )
                .show(ui, |ui| {
                    tb_out = toolbar::toolbar(ui, &tb_input, fields, pal);
                });
        }
        if let Some(c) = tb_out.command.or(tb_out.onion.command) {
            command = Some(c);
        }
        if let Some(v) = tb_out.onion.opacity {
            self.set_onion_opacity(v);
        }
        if let Some(v) = tb_out.onion.outline_darkness {
            self.settings.onion_outline_darkness = v.min(100);
            self.save_settings();
        }
        if let Some(s) = tb_out.snap {
            self.roll.snap = s;
        }
        if let Some(t) = tb_out.custom_snap
            && let Some(d) = self.doc()
        {
            match plumspirit_engine::model::timing::parse_custom_snap(&t, d.division()) {
                Ok(ticks) => {
                    self.roll.snap = plumspirit_engine::model::Snap::Custom(ticks);
                    self.set_status(
                        format!("Snap: {} ticks.", group_digits(ticks)),
                        StatusKind::Info,
                    );
                }
                Err(m) => {
                    // Keep the entry open with the typed text for correction.
                    self.custom_snap_field.text = t;
                    self.custom_snap_field.editing = true;
                    self.custom_snap_field.focus_next = true;
                    self.set_status(m, StatusKind::Error);
                }
            }
        }
        if let Some(t) = tb_out.zoom_x {
            self.commit_zoom_text(&t, true);
        }
        if let Some(t) = tb_out.zoom_y {
            self.commit_zoom_text(&t, false);
        }
        if let Some(t) = tb_out.gate {
            if summary.count == 0 && self.settings.link_gate {
                self.commit_default_gate_text(&t);
            } else {
                self.commit_gate_text(&t);
            }
        }
        if let Some(g) = tb_out.default_gate {
            self.settings.default_gate = g;
            self.save_settings();
            self.set_status(format!("New notes: {}.", g.label()), StatusKind::Info);
        }
        if let Some(l) = tb_out.link_gate {
            self.settings.link_gate = l;
            self.save_settings();
        }
        if let Some(t) = tb_out.set_selected_gate {
            self.commit_gate_text(&t.to_string());
        }
        if let Some(t) = tb_out.velocity {
            self.commit_velocity_text(&t);
        }

        // Transport / SoundFont row.
        if !self.tempo_editing {
            self.tempo_text = self.tempo_display();
        }
        let position = self.transport.position();
        let position_text = self
            .meter_map()
            .map(|m| m.describe(position))
            .unwrap_or_else(|| "—".into());
        let time_text = self
            .display_tempo()
            .map(|t| format_clock(t.seconds_at_tick(position)))
            .unwrap_or_else(|| "—".into());
        let (sf_name, sf_tip) = self.soundfont_label();
        let status = self.transport_status_text();
        let timing_editable = self.doc().is_some_and(|d| d.conductor().timing_editable());
        let follow = match (self.settings.follow_playhead, self.follow_suspended) {
            (false, _) => transport_bar::FollowShown::Off,
            (true, false) => transport_bar::FollowShown::On,
            (true, true) => transport_bar::FollowShown::Paused,
        };
        let loop_cfg = self.loop_config();
        let loop_text = loop_cfg.range.and_then(|(a, b)| {
            self.meter_map()
                .map(|m| format!("{} – {}", m.format_position(a), m.format_position(b)))
        });
        let speed_text = self.settings.speed.to_string();
        self.audio_window.poll_outputs(ui.ctx());
        let (output_label, output_tip, output_warn, output_open) = self.output_chip();
        let output_choices = self
            .audio_window
            .output_choices(&self.settings.audio.default_output);
        let output_current = self.settings.audio.default_output.clone();
        let tr_input = transport_bar::TransportInput {
            has_doc,
            state: self.transport.state(),
            follow,
            position_text: &position_text,
            time_text: &time_text,
            tempo_enabled: timing_editable,
            tempo_tip: if timing_editable {
                "Tempo at the start of the song in BPM (any positive value, e.g. 120, 174.5, 1e6). Press Enter to apply; later tempo points (BPM lane of the Conductor) are kept."
            } else {
                self.doc()
                    .and_then(|d| d.conductor().scope().timing_note())
                    .unwrap_or("The tempo cannot be edited.")
            },
            soundfont_name: &sf_name,
            soundfont_tip: &sf_tip,
            status: status.as_ref().map(|(t, w)| (t.as_str(), *w)),
            loop_on: loop_cfg.active().is_some(),
            loop_text: loop_text.clone(),
            speed: &speed_text,
            output_label: &output_label,
            output_tip: &output_tip,
            output_warn,
            output_open,
            outputs: &output_choices,
            output_current: &output_current,
            outputs_listing: self.audio_window.outputs_listing(),
            anchor_on: self.settings.anchor_playhead,
        };
        let mut tempo_text = std::mem::take(&mut self.tempo_text);
        let mut tr_out = transport_bar::TransportOutput::default();
        egui::Panel::top("transport")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .stroke(egui::Stroke::new(1.0, pal.panel_border)),
            )
            .show(ui, |ui| {
                tr_out = transport_bar::transport_bar(
                    ui,
                    &tr_input,
                    &mut tempo_text,
                    &mut self.speed_field,
                    pal,
                );
            });
        self.tempo_text = tempo_text;
        self.tempo_editing = tr_out.tempo_focused;
        if tr_out.tempo_committed {
            self.commit_tempo_text();
        }
        if let Some(t) = tr_out.speed {
            self.commit_speed_text(&t);
        }
        if let Some(t) = tr_out.output_chosen {
            self.choose_default_output(t);
        }
        if tr_out.outputs_refresh {
            self.audio_window.refresh_outputs(ui.ctx());
        }
        if tr_out.audio_settings {
            self.open_audio_settings();
        }
        if let Some(c) = tr_out.command {
            command = Some(c);
        }

        // Status bar.
        if self.settings.show_metrics != self.metrics.is_some() {
            self.metrics = self.settings.show_metrics.then(|| {
                let ctx = ui.ctx().clone();
                Sampler::start(move || ctx.request_repaint())
            });
        }
        let job_status = self
            .job
            .as_ref()
            .map(|j| (tasks::running_text(&j.label(), j.started), j.fraction()))
            .or_else(|| {
                (!self.project_releases.is_empty())
                    .then(|| ("Releasing project memory…".into(), None))
            });
        let job_cancelable = self.job.as_ref().is_some_and(|j| {
            matches!(
                j.kind,
                JobKind::Import { .. } | JobKind::LoadProject { .. } | JobKind::OpenArchive { .. }
            )
        });
        let status_input = StatusInput {
            left: self.status_line(),
            position: self.position_line(),
            job: job_status,
            job_cancelable,
            message: self.status.as_ref().map(|s| (s.text.as_str(), s.kind)),
            has_details: self.status.as_ref().is_some_and(|s| !s.details.is_empty()),
            metrics: self
                .metrics
                .as_ref()
                .map(|m| m.text(ui.ctx().cumulative_frame_nr())),
            last_task: self.tasks.last_text(),
        };
        let mut status_out = status_bar::StatusOut::default();
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .stroke(egui::Stroke::new(1.0, pal.panel_border)),
            )
            .exact_size(24.0)
            .show(ui, |ui| {
                status_out = status_bar::status_bar(ui, &status_input, pal);
            });
        if status_out.cancel_job
            && job_cancelable
            && let Some(j) = &self.job
        {
            // L-H02: the job ends as canceled; nothing was written.
            j.cancel();
        }
        if status_out.details
            && let Some(s) = &self.status
        {
            self.dialog = Some(Dialog::Message {
                title: "Details".into(),
                body: s.details.join("\n\n"),
                is_error: false,
            });
        }

        // Track panel.
        let editable = self.job.is_none();
        let conductor_active = self.conductor_selected();
        let ghosts = self.onion_ghosts();
        let mut track_action = None;
        // Live Conductor BPM at the transport / edit position (the canonical
        // tempo points: ramps are interpolated; Playback Speed is not applied).
        let (bpm_text, bpm_tip) = match self.doc() {
            Some(d) => live_bpm(d, self.transport.position(), self.settings.speed),
            None => (String::new(), String::new()),
        };
        let generation = self.doc_generation();
        let rows: Vec<TreeRowView> = match self.open.as_ref() {
            Some(o) => self
                .tree_rows
                .rows(&o.doc, generation, &self.collapsed)
                .to_vec(),
            None => Vec::new(),
        };
        let note_counts: Vec<usize> = match self.open.as_ref() {
            Some(o) => self.note_counts.counts(&o.doc, generation).to_vec(),
            None => Vec::new(),
        };
        let active_sub = if conductor_active {
            None
        } else {
            self.active_sub_of(self.active_track())
        };
        let mut goto = std::mem::take(&mut self.sub_goto);
        let mut renaming = self.folder_rename.take();
        // v0.9 (V09-S08): every project opens in the Track List (the render
        // benchmark measures the Piano Roll).
        if self.open.is_some() && self.main_view_generation != Some(generation) {
            self.main_view_generation = Some(generation);
            self.main_view = if self.bench.is_some() {
                track_list::MainView::PianoRoll
            } else {
                track_list::MainView::TrackList
            };
        }
        let in_track_list = self.main_view == track_list::MainView::TrackList;
        let row_layout = track_panel::RowLayout {
            row_h: if in_track_list {
                self.track_list.row_h
            } else {
                track_panel::ROW_H
            },
            scroll_to: self.track_list_scroll.take(),
        };
        let mut row_geometry = track_panel::RowGeometry::default();
        clock.lap("menus / bars");
        egui::Panel::left("tracks")
            .resizable(false)
            .exact_size(track_panel::PANEL_WIDTH)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(8, 0))
                    .stroke(egui::Stroke::new(1.0, pal.panel_border)),
            )
            .show(ui, |ui| {
                let doc = self.doc();
                let sub = match (doc, active_sub.as_ref()) {
                    (Some(d), Some(sc)) => {
                        d.track(self.active_track())
                            .map(|t| track_panel::SubchannelView {
                                active: sc,
                                table: t.subchannels(),
                                goto: &mut goto,
                            })
                    }
                    _ => None,
                };
                (track_action, row_geometry) = track_panel::track_panel(
                    ui,
                    doc,
                    self.active_track(),
                    conductor_active,
                    editable,
                    pal,
                    &ghosts,
                    dark,
                    &self.selected_tracks,
                    &rows,
                    &note_counts,
                    &bpm_text,
                    &bpm_tip,
                    sub,
                    track_panel::FolderPanel {
                        renaming: &mut renaming,
                    },
                    row_layout,
                );
            });
        self.row_geometry = row_geometry;
        self.sub_goto = goto;
        self.folder_rename = renaming;
        if let Some(a) = track_action
            && let Some(c) = self.apply_track_action(a, &rows)
        {
            command = Some(c);
        }

        // Central area: piano roll + lane, or the empty state.
        let mut actions = Vec::new();
        let wordmark = self.open.is_none().then(|| {
            self.brand.sized(
                &ctx,
                crate::assets::Brand::wordmark(dark),
                empty_state::WORDMARK_HEIGHT,
            )
        });
        let loading_label = self
            .job
            .as_ref()
            .filter(|j| matches!(j.kind, JobKind::Import { .. } | JobKind::LoadProject { .. }))
            .map(RunningJob::label);
        let running = self.transport.state().is_running();
        let playhead = running.then(|| self.transport.position());
        if !self.roll.is_scrubbing() {
            self.roll.cursor_tick = self.transport.position();
        }
        self.roll.anchor = self.transport.anchor();
        // Graph slots and what they show (project settings).
        let slots = [self.graph_slot(0), self.graph_slot(1)];
        let shown = [self.shown_target(0), self.shown_target(1)];
        let graph2 = slots[1].visible;
        if !graph2 && self.active_graph == 1 {
            self.active_graph = 0;
        }
        self.refresh_custom_expr();
        self.sync_loop();
        let loop_cfg = self.loop_config();
        let mut graph_actions: Vec<(usize, crate::piano_roll::graph::GraphAction)> = Vec::new();
        let mut graph_bar_out = graph_bar::GraphBarOutput::default();
        let mut graph_updates: Vec<(usize, Option<GraphTarget>, Option<u8>, bool)> = Vec::new();
        let mut activate: Option<usize> = None;
        let mut tl_activate: Option<usize> = None;
        let mut tl_edits: Vec<track_list::ClipIntent> = Vec::new();
        let mut tl_track: Option<track_panel::TrackAction> = None;
        let mut tl_command: Option<Command> = None;
        let mut roll_pressed = false;
        // v0.8 §156: what the right-click note menu offers now.
        let note_menu = note_menu::NoteMenu {
            repeat: self
                .last_recipe
                .as_ref()
                .map(|r| format!("Repeat {}", r.name())),
            recent: self
                .recent_tools
                .iter()
                .filter_map(|id| {
                    plumspirit_engine::transform::tool(id).map(|d| (d.id, (d.spec)().name))
                })
                .collect(),
            can_mute: self.enabled(Command::MuteNotes),
            can_unmute: self.enabled(Command::UnmuteNotes),
            can_slice: self.enabled(Command::SliceNotes),
            can_copy: self.selected_count() > 0,
            custom: self.custom_tool_menu(),
        };
        clock.lap("track panel");
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let rect = ui.max_rect();
            self.roll_width =
                (rect.width() - crate::piano_roll::KEYS_WIDTH - crate::piano_roll::SCROLLBAR)
                    .max(1.0);
            if self.open.is_none() {
                let picked = empty_state::empty_state(
                    ui,
                    wordmark,
                    pal,
                    loading_label.as_deref(),
                    empty_state::RecentInput {
                        entries: self.recent.entries(),
                        missing: &self.recent_missing,
                        now: recent_now,
                    },
                    self.settings.show_orientation,
                );
                match picked {
                    empty_state::EmptyAction::Open => command = Some(Command::Open),
                    empty_state::EmptyAction::New => command = Some(Command::NewProject),
                    empty_state::EmptyAction::QuickStart => {
                        command = Some(Command::ShowQuickStart)
                    }
                    empty_state::EmptyAction::HideOrientation => {
                        command = Some(Command::ToggleOrientation)
                    }
                    empty_state::EmptyAction::Recent(a) => {
                        self.apply_recent_action(a, frame, &ctx)
                    }
                    empty_state::EmptyAction::None => {}
                }
                return;
            }
            if self.player_shown() {
                // v0.9 (V09-S17): the MIDI Player replaces the editor area.
                self.player_frame(ui, rect, pal, dark, running);
                self.central_overlays(ui, rect, pal, loading_label.as_deref());
                return;
            }
            if self.main_view == track_list::MainView::TrackList {
                // v0.9 (V09-S08): the Track List, the primary window.
                if let Some(out) = self.track_list_frame(ui, &rows, pal, dark, running, playhead) {
                    actions.extend(out.roll);
                    tl_activate = out.activate;
                    tl_edits = out.edits;
                    tl_track = out.track;
                    tl_command = out.command;
                    self.track_list_scroll = out.scroll_to;
                }
                self.central_overlays(ui, rect, pal, loading_label.as_deref());
                return;
            }
            // Split: roll on top, splitter, then the graph tool row and Event
            // Graph 1 (and Graph 2 when shown).
            let lane_visible = self.settings.velocity_lane_visible;
            // The graphs never take more than 45% (55% with two) of the area, so the
            // roll stays usable.
            let share = if graph2 { 0.55 } else { 0.45 };
            let max_lane =
                (rect.height() * share).clamp(settings::MIN_LANE_HEIGHT, settings::MAX_LANE_HEIGHT * 2.0);
            let bar_h = graph_bar::HEIGHT;
            let g2_h = if graph2 {
                self.settings
                    .graph2_height
                    .clamp(settings::MIN_LANE_HEIGHT * 0.75, max_lane * 0.5)
            } else {
                0.0
            };
            let g1_h = if lane_visible {
                self.settings
                    .velocity_lane_height
                    .clamp(settings::MIN_LANE_HEIGHT * 0.75, (max_lane - g2_h).max(40.0))
            } else {
                0.0
            };
            let lane_h = if lane_visible {
                bar_h + g1_h + if graph2 { 5.0 + g2_h } else { 0.0 }
            } else {
                velocity::COLLAPSED_HEIGHT
            };
            let splitter_h = if lane_visible { 5.0 } else { 0.0 };
            let roll_rect = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.right(), rect.bottom() - lane_h - splitter_h),
            );
            let splitter = egui::Rect::from_min_max(
                egui::pos2(rect.left(), roll_rect.bottom()),
                egui::pos2(rect.right(), roll_rect.bottom() + splitter_h),
            );
            let lane_rect =
                egui::Rect::from_min_max(egui::pos2(rect.left(), splitter.bottom()), rect.max);
            let keyboard = self.dialog.is_none()
                && !ctx.egui_wants_keyboard_input()
                && !self.focus_at_frame_start;
            let generation = self.doc_generation();
            // V09-S11: the opened clip's content and shift (none: the active
            // track's own notes at the identity, as before clips). B2-F17: a
            // clip of another track's content is edited as that track (its
            // notes, subchannels and new-note channel).
            let session = self.session_view();
            let roll_target = self.edit_target();
            let track_index = roll_target.host_track();
            let roll_title = session.and_then(|_| self.session_title());
            // B2-F15: locked (no clip; the Conductor has its own banner).
            let roll_lock = self.lua_formats.reviewing().then_some("Review the import, then Apply or Cancel.").or_else(||self
                .note_lock()
                .filter(|r| *r != clip_session::LockReason::Conductor)
                .map(clip_session::LockReason::hint));
            let (roll_content, roll_shift) = match session.map(|v| (v.target, v.shift)) {
                Some((plumspirit_engine::editor::NoteTarget::Content { content, .. }, sh)) => {
                    (Some(content), sh)
                }
                Some((_, sh)) => (None, sh),
                None => (None, 0),
            };
            let aggregate_focus = self.aggregate_focus();
            let meter = self.meter_map().cloned();
            let (Some(meter), Some(o)) = (meter, self.open.as_ref()) else {
                return;
            };
            let doc = o.doc.clone();
            // B2-F18: the picked track's other clips (Track Aggregate).
            let roll_parts = match aggregate_focus {
                Some(f) => clip_session::aggregate_parts(&doc, self.active_track(), f),
                None => Vec::new(),
            };
            // Follow Playhead: a view feature driven by the transport position
            // (never by a timer), paused while the user scrolls by hand.
            let roll_playhead = clip_session::shifted(self.transport.position(), -roll_shift);
            if !running
                || self.follow_suspended
                || self.seek_follow_view != Some((self.roll.view.time, roll_shift))
                || !(0.0..=self.roll.view_width()).contains(&self.roll.view.time.tick_to_x(roll_playhead))
            {
                self.seek_follow_view = None;
            }
            if running
                && self.settings.follow_playhead
                && !self.follow_suspended
                && self.seek_follow_view.is_none()
                && !self.roll.is_dragging()
            {
                let style = if self.settings.smooth_follow {
                    FollowStyle::Smooth
                } else {
                    FollowStyle::Page
                };
                self.roll.follow_playhead(
                    roll_playhead,
                    clip_session::shifted(doc.duration_ticks(), -roll_shift),
                    style,
                    f64::from(self.settings.follow_position.min(100)) / 100.0,
                );
            }
            let onion_cfg = &doc.project().onion;
            let onion_alpha = (u32::from(onion_cfg.opacity) * 255 / 100) as u8;
            // Layer order (render order only; Dense First puts the densest
            // visible source in front), from the view's song-time range.
            let (vt0, vt1) = self
                .roll
                .view
                .time
                .visible_range(f64::from(self.roll_width));
            let song_range = (
                clip_session::shifted(vt0, roll_shift),
                clip_session::shifted(vt1, roll_shift),
            );
            let onion_sources = onion_sources(
                &doc,
                generation,
                &ghosts,
                &mut self.onion_order,
                onion_cfg,
                song_range,
            );
            // B1-0151: a moved view gets its Onion order once it rests.
            if let Some(at) = self.onion_order.rerank_due() {
                ctx.request_repaint_after(at.saturating_duration_since(std::time::Instant::now()));
            }
            let active_t = doc.track(track_index);
            let corrections_note = active_t
                .filter(|t| t.props().corrects() && !conductor_active)
                .map(|t| {
                    format!(
                        "Track corrections active ({}): playback and export differ from the notes shown.",
                        plumspirit_engine::model::correction::Correction::describe(t.props())
                    )
                });
            let (new_sub, new_channel) = match self.active_sub_of(track_index) {
                Some(sc) => (sc.id, sc.midi_channel),
                None => (
                    plumspirit_engine::model::SubchannelId::MAIN,
                    active_t.map_or(0, |t| t.props().default_channel),
                ),
            };
            let preview_kernel = self.preview_kernel();
            let merge_view = if self.merge_preview_on() {
                self.merge.as_ref().and_then(merge::MergeState::ghosts)
            } else {
                None
            };
            let added_stores: Vec<&plumspirit_engine::model::NoteStore> = merge_view
                .as_ref()
                .map(|v| v.stores())
                .unwrap_or_default();
            let key_colors_on = self.key_colors_on();
            // v1.0-beta1 B1-0110: one colour per key — the note the roll
            // shows in front at the playhead, in the roll's own layers.
            let key_display = if key_colors_on {
                let onion_ids: Vec<TrackId> = onion_sources.iter().map(|s| s.id).collect();
                let scope = self.key_tracks(&doc);
                let palette = crate::ui::theme::note_palette(ui.ctx());
                Some(self.roll_keys.update(
                    &doc,
                    self.analysis.corrections(),
                    scope.as_ref(),
                    (!self.conductor_selected()).then_some(self.active_track()),
                    &onion_ids,
                    self.transport.position(),
                    self.keys.display(),
                    &palette,
                    dark,
                ))
            } else {
                None
            };
            // B1-0059: a new view (open / create / Reset View) starts at the
            // default zoom from Settings.
            self.roll.default_zoom = (self.settings.default_zoom_x, self.settings.default_zoom_y);
            self.roll.default_fit = self.settings.default_fit_keys;
            self.roll.rotate_about_center = self.settings.rotate_about_center;
            self.roll.y_beyond_keys = self.settings.y_zoom_beyond_keys;
            self.roll.lock_fit = self.settings.lock_fit_range;
            // B1-0115: what a scale-handle drag does with the notes.
            self.roll.scale_duplicate = (
                self.settings.scale_y.handles_duplicate,
                match self.settings.scale_y.fill.mode {
                    plumspirit_engine::transform::geom::FillMode::Rows => "fill added rows",
                    plumspirit_engine::transform::geom::FillMode::Step { .. } => {
                        "copy by pitch step"
                    }
                },
            );
            let input = RollInput {
                doc: &doc,
                track_index,
                content: roll_content,
                shift: roll_shift,
                title: roll_title.as_deref(),
                locked: roll_lock,
                parts: &roll_parts,
                meter: &meter,
                palette: pal,
                dark,
                editable: editable && self.dialog.is_none(),
                keyboard,
                onion: &onion_sources,
                onion_alpha,
                onion_outlines: self
                    .settings
                    .onion_outlines
                    .then_some(self.settings.onion_outline_darkness),
                playhead,
                conductor_active,
                new_note_channel: new_channel,
                new_note_gate: self.settings.default_gate,
                new_note_sub: new_sub,
                preview: preview_kernel.as_ref().map(|(k, scope)| (k.as_ref(), *scope)),
                capture: self.tool.as_ref().and_then(tools::ToolSession::capture_mode)
                    .or_else(||self.custom_tool.as_ref().and_then(custom_tools::CustomToolSession::capture_mode)),
                stroke_overlay: self
                    .tool
                    .as_ref()
                    .map(|s| s.stroke_overlay.as_slice())
                    .or_else(||self.custom_tool.as_ref().map(|s|s.stroke_overlay.as_slice())).unwrap_or(&[]),
                tool_preview: lua_formats::preview(&self.lua_formats,&doc,roll_target)
                    .or_else(|| tools::tool_preview(&self.tool, roll_target))
                    .or_else(|| custom_tools::custom_preview(&self.custom_tool, roll_target))
                    .or_else(|| scores::score_preview(&self.score_insert))
                    .or_else(|| handles::transient_preview(&self.transient, roll_target))
                    .or_else(|| image_midi::image_preview(&self.image_midi))
                    .or_else(|| spiderweb::spiderweb_preview(&self.spiderweb)),
                added: merge_view.as_ref().map(|v| crate::piano_roll::AddedGhosts {
                    stores: &added_stores,
                    records: v.records(),
                    longest: v.longest(),
                }),
                loop_range: loop_cfg.range,
                loop_enabled: loop_cfg.enabled,
                corrections_note: corrections_note.as_deref(),
                range: self.time_selection,
                key_colors: key_display,
                key_guide: self.settings.key_guide_88,
                audition: self.settings.audition && editable,
                note_menu: (editable && !conductor_active).then_some(&note_menu),
                overlay_controls: progressor::overlay_controls(&self.overlay_views),
            };
            if ui.input(|i| i.pointer.any_pressed())
                && ui
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| roll_rect.contains(p))
            {
                roll_pressed = true;
            }
            let mut roll_ui = ui.new_child(egui::UiBuilder::new().max_rect(roll_rect));
            actions = self.roll.show(&mut roll_ui, input, generation);
            if let Some((painter, slot)) = self.roll.take_overlay_layer() {
                self.paint_lua_overlays(&roll_ui, &painter, 0, Some(slot));
            }
            // B1-0059: Hide Text Overlays leaves the notes unobstructed.
            let overlays = !self.settings.hide_text_overlays;
            // B1-0065: the readouts fade while the pointer is over them and
            // have a chip (click: collapse / expand, drag: move).
            let pointer = ui.input(|i| i.pointer.hover_pos());
            self.overlay_views = [None, None];
            if overlays
                && let Some(area) = self.roll.notes_rect()
                && let Some((text, stale)) = self.progressor_text()
            {
                self.overlay_views[0] = progressor::paint_overlay(
                    &ui.painter_at(area),
                    area,
                    &text,
                    &self.settings.progressor,
                    pal,
                    stale,
                    progressor::OverlayState {
                        advanced: None,
                        pointer,
                        collapsed: self.settings.progressor.collapsed,
                        chip: true,
                    },
                );
            }
            self.overlay_views[1] = match self.roll.notes_rect() {
                Some(area) if overlays => {
                    self.paint_selection_stats(&ui.painter_at(area), area, pal, pointer)
                }
                _ => None,
            };
            self.sel_readout_last = self.overlay_views[1].map(|v| v.rect);
            if let Some(area) = self.roll.notes_rect() {
                self.overlay_chips(ui, area);
            }
            if self.roll.user_panned && running && self.settings.follow_playhead {
                self.follow_suspended = true;
            }
            // Splitter (resizes Graph 1).
            if lane_visible {
                let resp =
                    ui.interact(splitter, ui.id().with("lane-splitter"), egui::Sense::drag());
                let p = ui.painter_at(splitter);
                p.rect_filled(splitter, 0.0, pal.panel_bg);
                p.hline(
                    splitter.x_range(),
                    splitter.center().y,
                    egui::Stroke::new(
                        1.0,
                        if resp.hovered() || resp.dragged() {
                            pal.accent
                        } else {
                            pal.panel_border
                        },
                    ),
                );
                if resp.hovered() || resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                if resp.drag_started() {
                    self.lane_drag_start = Some(g1_h);
                }
                if resp.dragged()
                    && let Some(h0) = self.lane_drag_start
                {
                    let total = resp.total_drag_delta().map_or(0.0, |d| d.y);
                    self.settings.velocity_lane_height =
                        (h0 - total).clamp(settings::MIN_LANE_HEIGHT * 0.75, (max_lane - g2_h).max(40.0));
                }
                if resp.drag_stopped() {
                    self.lane_drag_start = None;
                    self.save_settings();
                }
                let lane_started = std::time::Instant::now();
                // Tool row.
                let bar_rect = egui::Rect::from_min_size(lane_rect.min, egui::vec2(lane_rect.width(), bar_h));
                {
                    let p = ui.painter_at(bar_rect);
                    p.rect_filled(bar_rect, 0.0, pal.panel_bg);
                    p.hline(bar_rect.x_range(), bar_rect.bottom() - 0.5, egui::Stroke::new(1.0, pal.panel_border));
                }
                let names: Vec<&str> = self.settings.curve_presets.iter().map(|p| p.name.as_str()).collect();
                let custom_error = self
                    .custom_expr
                    .as_ref()
                    .and_then(|(_, r)| r.as_ref().err().map(String::as_str));
                let gb_input = graph_bar::GraphBarInput {
                    tool: self.settings.graph_tool,
                    active: self.active_graph,
                    active_target: shown[self.active_graph],
                    graph2,
                    list: self.event_list_open,
                    editable: editable && self.dialog.is_none(),
                    anchors_pending: self.graphs.views[self.active_graph].pending_anchors().is_some(),
                    ppqn: meter.ppqn(),
                    custom_names: names,
                    custom_error,
                };
                let mut bar_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(bar_rect.shrink2(egui::vec2(4.0, 0.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                graph_bar_out = graph_bar::graph_bar(&mut bar_ui, &gb_input, &mut self.settings.curve, pal);
                // Curve preview: the same generation as Apply, with a small budget.
                // L-G05: one palette for every ghost (not a context read each).
                let ghost_palette = crate::ui::theme::note_palette(&ctx);
                let ghost_tracks: Vec<(&plumspirit_engine::model::Track, egui::Color32)> = ghosts
                    .iter()
                    .filter_map(|&i| doc.track(i))
                    .map(|t| {
                        (
                            t,
                            ghost_palette.track_color(
                                t.id().0,
                                t.props().onion_color,
                                dark,
                                onion_alpha.max(60),
                            ),
                        )
                    })
                    .collect();
                // V09-S11: the opened clip's content (its notes and events).
                let track_for_graph = (!conductor_active)
                    .then(|| plumspirit_engine::editor::note_view(&doc, roll_target).ok())
                    .flatten();
                let snap = self.roll.snap.unit_ticks(doc.division());
                let tool = self.settings.graph_tool;
                let mut y = bar_rect.bottom();
                for slot in 0..2usize {
                    if slot == 1 && !graph2 {
                        break;
                    }
                    let h = if slot == 0 { g1_h } else { g2_h };
                    if slot == 1 {
                        // Splitter between the graphs (resizes Graph 2).
                        let sp = egui::Rect::from_min_size(egui::pos2(lane_rect.left(), y), egui::vec2(lane_rect.width(), 5.0));
                        let resp = ui.interact(sp, ui.id().with("graph2-splitter"), egui::Sense::drag());
                        let p = ui.painter_at(sp);
                        p.rect_filled(sp, 0.0, pal.panel_bg);
                        p.hline(
                            sp.x_range(),
                            sp.center().y,
                            egui::Stroke::new(1.0, if resp.hovered() || resp.dragged() { pal.accent } else { pal.panel_border }),
                        );
                        if resp.hovered() || resp.dragged() {
                            ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                        }
                        if resp.drag_started() {
                            self.graph2_drag_start = Some(g2_h);
                        }
                        if resp.dragged()
                            && let Some(h0) = self.graph2_drag_start
                        {
                            let total = resp.total_drag_delta().map_or(0.0, |d| d.y);
                            self.settings.graph2_height =
                                (h0 - total).clamp(settings::MIN_LANE_HEIGHT * 0.75, max_lane * 0.5);
                        }
                        if resp.drag_stopped() {
                            self.graph2_drag_start = None;
                            self.save_settings();
                        }
                        y += 5.0;
                    }
                    let grect = egui::Rect::from_min_size(egui::pos2(lane_rect.left(), y), egui::vec2(lane_rect.width(), h));
                    y += h;
                    let target = shown[slot];
                    let needs_track = !controllers_global(target);
                    let note = (needs_track && track_for_graph.is_none())
                        .then_some("The Conductor is selected: choose a track to see its events here.");
                    let ctx_curve = curves::CurveContext {
                        doc: &doc,
                        track: track_for_graph,
                        track_index: roll_target,
                        channel: slots[slot].channel,
                        meter: &meter,
                        settings: &self.settings.curve,
                        custom: self.custom_expr.as_ref().map(|c| &c.1),
                        snap,
                    };
                    let shape = tool.shape().unwrap_or(plumspirit_engine::editor::curves::Shape::Line);
                    let previewer = |t: GraphTarget, a: (u64, f64), b: (u64, f64)| -> CurvePreview {
                        ctx_curve.preview(t, shape, a, b)
                    };
                    let mut gui = ui.new_child(egui::UiBuilder::new().max_rect(grect).id_salt(("graph-slot", slot)));
                    let out = self.graphs.show(
                        &mut gui,
                        grect,
                        HostInput {
                            slot,
                            target,
                            channel: slots[slot].channel,
                            active: self.active_graph == slot,
                            tool,
                            anchors: self.settings.curve.anchors,
                            track: track_for_graph,
                            conductor: doc.conductor(),
                            // A track's (or an opened clip's) events are on
                            // the roll's axis; the Conductor's lanes are song
                            // time (V09-S11: shifted like the roll's ruler).
                            time: if needs_track {
                                self.roll.view.time
                            } else {
                                plumspirit_engine::view::TimeAxis {
                                    origin_tick: clip_session::shifted(
                                        self.roll.view.time.origin_tick,
                                        roll_shift,
                                    ),
                                    ..self.roll.view.time
                                }
                            },
                            meter: &meter,
                            palette: pal,
                            dark,
                            editable: editable && self.dialog.is_none(),
                            playhead: if needs_track {
                                playhead.map(|p| clip_session::shifted(p, -roll_shift))
                            } else {
                                playhead
                            },
                            snap,
                            ghosts: if needs_track { &ghost_tracks } else { &[] },
                            preview: &previewer,
                            note,
                        },
                    );
                    if out.activate {
                        activate = Some(slot);
                    }
                    if self.active_graph == slot || out.activate {
                        actions.extend(out.actions);
                    }
                    graph_actions.extend(out.graph_actions.into_iter().map(|a| (slot, a)));
                    if slot == 0 {
                        self.roll.stats.lane_aggregated = out.aggregated;
                    }
                    if out.target.is_some() || out.channel.is_some() || out.hide {
                        graph_updates.push((slot, out.target, out.channel, out.hide));
                    }
                }
                self.roll.stats.lane_time = lane_started.elapsed();
            } else if velocity::show_collapsed(ui, lane_rect, pal, "Event Graphs") {
                command = Some(Command::ToggleVelocityLane);
            }
            drop(onion_sources);
            drop(doc);
            self.central_overlays(ui, rect, pal, loading_label.as_deref());
        });
        // The document clones above were dropped before applying actions, so the
        // document is exclusively owned again.
        if let Some(i) = tl_activate {
            self.select_track(i);
        }
        for e in tl_edits {
            self.apply_clip_intent(e);
        }
        if let Some(a) = tl_track
            && let Some(c) = self.apply_track_action(a, &rows)
        {
            command = command.or(Some(c));
        }
        command = command.or(tl_command);
        if let Some(slot) = activate {
            self.activate_graph(slot);
            self.graph_focus = true;
        } else if roll_pressed {
            self.graph_focus = false;
        }
        for (slot, target, channel, hide) in graph_updates {
            if hide {
                self.update_graph_slot(slot, |s| s.visible = false);
                self.activate_graph(0);
            }
            if let Some(t) = target {
                self.update_graph_slot(slot, |s| s.target = t);
            }
            if let Some(c) = channel {
                self.update_graph_slot(slot, |s| s.channel = c);
            }
        }
        self.apply_graph_bar(graph_bar_out);
        // Note-menu commands run like menu commands (they may open windows).
        let (runs, actions): (Vec<_>, Vec<_>) = actions.into_iter().partition(|a| {
            matches!(
                a,
                crate::piano_roll::RollAction::Run(_)
                    | crate::piano_roll::RollAction::CustomTool(..)
            )
        });
        let (roll_actions, loop_actions): (Vec<_>, Vec<_>) = actions.into_iter().partition(|a| {
            !matches!(
                a,
                crate::piano_roll::RollAction::SetLoop(..)
                    | crate::piano_roll::RollAction::SetLoopStart(_)
                    | crate::piano_roll::RollAction::SetLoopEnd(_)
                    | crate::piano_roll::RollAction::ClearLoop
            )
        });
        clock.lap("central view");
        self.apply_roll_actions(roll_actions, &ctx);
        for a in runs {
            match a {
                crate::piano_roll::RollAction::Run(c) if self.enabled(c) => {
                    self.execute(c, &ctx, frame);
                }
                crate::piano_roll::RollAction::CustomTool(plugin, tool) => {
                    self.open_custom_tool(&plugin, &tool);
                }
                _ => {}
            }
        }
        self.update_transient(&ctx);
        for a in loop_actions {
            self.apply_loop_action(a);
        }
        for (slot, a) in graph_actions {
            self.apply_graph_action(slot, a);
        }
        // Shortcuts run after the piano roll has seen this frame's input, so a click
        // released in the same frame as Ctrl+C / Delete has already finished its
        // gesture (and committed any move) instead of blocking the shortcut.
        self.shortcuts(&ctx, frame);
        if let Some(c) = command {
            self.execute(c, &ctx, frame);
        }
        clock.lap("edits / commands");
        self.update_preview(&ctx);
        self.update_tool(&ctx);
        self.tool_window_ui(&ctx);
        self.image_midi_ui(&ctx, frame);
        self.audio_settings_ui(&ctx, frame);
        self.note_colors_ui(&ctx);
        self.history_window_ui(&ctx);
        self.diagnostics_window_ui(&ctx, frame);
        self.reference_frame(&ctx);
        self.reference_ui(&ctx, frame);
        self.spiderweb_ui(&ctx);
        self.handle_dropped_files(&ctx);
        self.update_score_insert();
        self.score_window_ui(&ctx);
        clock.lap("windows");
        self.update_merge(&ctx);
        clock.lap("import into");
        self.update_playing_time();
        self.handle_dialog(&ctx, frame);
        clock.lap("dialogs");
        self.event_list_ui(&ctx);
        self.subchannel_window_ui(&ctx, dark);
        self.keybinds_window_ui(&ctx, frame);
        self.settings_window_ui(&ctx, frame);
        self.about_window_ui(&ctx);
        self.help_window_ui(&ctx, frame);
        self.lua_plugins_ui(&ctx, frame);
        self.poll_lua_plugins(&ctx);
        self.lua_panel_windows(&ctx);
        self.custom_tool_ui(&ctx);
        self.plugin_commands_ui(&ctx);
        self.lua_format_ui(&ctx);
        self.zoom_frame(&ctx);
        self.zoom_window_ui(&ctx);
        self.draw_zoom_lens(&ctx);
        self.scrub_frame(&ctx);
        self.scrub_window_ui(&ctx);
        self.draw_scrub_markers(&ctx);
        self.handle_onion_chooser(&ctx, pal, dark);
        self.update_title(&ctx);
        if self.transport.state().is_running() || self.transport.loading_soundfont().is_some() {
            ctx.request_repaint();
        } else if let Some(d) = self.transport.idle_pause_due_in() {
            // Wake once more to pause the idle audio stream.
            ctx.request_repaint_after(d);
        }
        if let Some(t) = self.settings_save_at {
            let now = std::time::Instant::now();
            if now >= t {
                self.settings_save_at = None;
                self.save_settings();
            } else {
                ctx.request_repaint_after(t - now);
            }
        }
        clock.lap("other windows");
        self.perf.record_update(clock.finish());
        crate::diagnostics::FrameClock::app_finished(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown_bounded();
    }
}

impl PlumSpiritApp {
    fn handle_onion_chooser(&mut self, ctx: &egui::Context, pal: &theme::Palette, dark: bool) {
        let Some(mut sel) = self.onion_choice.take() else {
            return;
        };
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let doc = o.doc.clone();
        let ordinals = doc.track_ordinals();
        match onion_chooser::show(ctx, &doc, &ordinals, o.active_track, &mut sel, pal, dark) {
            onion_chooser::ChooserOutcome::Open => self.onion_choice = Some(sel),
            onion_chooser::ChooserOutcome::Cancel => {}
            onion_chooser::ChooserOutcome::Done => {
                drop(doc);
                if let Some(d) = self.doc_mut() {
                    let o = &mut d.project_mut().onion;
                    o.specific = sel;
                    o.mode = OnionMode::Specific;
                }
            }
        }
    }

    fn soundfont_label(&self) -> (String, String) {
        if let Some(p) = self.transport.loading_soundfont() {
            return (
                format!("Loading {}…", files::display_name(p)),
                p.display().to_string(),
            );
        }
        let project_ref = self.doc().and_then(|d| d.project().soundfont.clone());
        let effective = self.effective_soundfont();
        // The project names a SoundFont that is not on this computer.
        if let (Some(r), Some(p)) = (&project_ref, &effective)
            && Path::new(&r.path) != p.as_path()
        {
            return (
                format!(
                    "{} · project's {} missing",
                    files::display_name(p),
                    files::display_name(Path::new(&r.path))
                ),
                format!(
                    "The project's SoundFont was not found:\n{}\nPlaying with {} instead. Choose a replacement with Browse….",
                    r.path,
                    p.display()
                ),
            );
        }
        match (&effective, self.transport.soundfont()) {
            (Some(p), Some(sf)) if sf.path == *p => (
                files::display_name(p),
                format!(
                    "{}{}\n{} file · {} presets · {} of samples in memory",
                    p.display(),
                    if sf.name.is_empty() {
                        String::new()
                    } else {
                        format!("\n“{}”", sf.name)
                    },
                    format_bytes(sf.file_bytes),
                    sf.presets,
                    format_bytes(sf.sample_bytes)
                ),
            ),
            (Some(p), _) => (
                files::display_name(p),
                format!("{}\nLoads when playback starts.", p.display()),
            ),
            (None, _) => match project_ref {
                Some(r) => (
                    format!("{} (missing)", files::display_name(Path::new(&r.path))),
                    format!(
                        "{}\nThis file was not found. Choose a replacement with Browse….",
                        r.path
                    ),
                ),
                None => (
                    "None".into(),
                    "No SoundFont selected. Choose an SF2 file with Browse….".into(),
                ),
            },
        }
    }

    fn transport_status_text(&self) -> Option<(String, bool)> {
        match self.transport.state() {
            TransportState::Seeking if self.transport.loading_soundfont().is_some() => {
                Some(("Loading SoundFont…".into(), false))
            }
            TransportState::Seeking => Some(("Starting…".into(), false)),
            TransportState::AudioUnavailable => Some((
                self.transport
                    .unavailable_reason()
                    .unwrap_or("Audio unavailable")
                    .to_owned(),
                true,
            )),
            _ => None,
        }
    }
}

/// One line of dense-renderer diagnostics (developer overlay, §207).
fn dense_line(d: &crate::piano_roll::dense::DenseStats) -> String {
    format!(
        "L{} tiles · level {} · {}/{} ready · {} stand-in · {} coarse · {} pending · picture {:04x}",
        d.lod,
        d.level,
        d.ready,
        d.visible_tiles,
        d.stand_ins,
        d.coarse,
        d.pending,
        d.picture & 0xFFFF
    )
}

#[cfg(test)]
mod discard_tests {
    use super::{DocSource, OpenDoc};
    use plumspirit_engine::history::journal::{Journal, identity};
    use plumspirit_engine::history::{HistoryLimits, Operation};
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::sync::Arc;

    /// "Don't Save" says what survives: edits stay in a working journal
    /// (redoable after reopening); settings outside the history are lost;
    /// without a journal, or for an unsaved MIDI import, everything is lost.
    #[test]
    fn the_dont_save_note_says_what_the_undo_history_keeps() {
        let dir = std::env::temp_dir().join(format!("plumspirit-discard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = PlumDocument::new_project("d".into(), Bpm::DEFAULT, 960);
        let file = dir.join("d.lup");
        let mut o = OpenDoc::new(
            doc,
            DocSource::Project(file.clone()),
            HistoryLimits::default(),
            true,
            1,
        );
        let edit = |o: &mut OpenDoc| {
            let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
            let doc = Arc::get_mut(&mut o.doc).unwrap();
            o.history
                .apply(
                    doc,
                    Operation::InsertNote {
                        track: 0.into(),
                        record: NoteRecord::new(0, 240, a),
                    },
                )
                .unwrap();
        };
        // No journal: everything is lost.
        edit(&mut o);
        assert!(o.discard_note().starts_with("Your changes will be lost"));
        // A working journal keeps the edits.
        let j = Journal::create(&dir.join("d.plh"), 7).unwrap();
        let id = identity(&o.doc);
        o.history.attach_new_journal(j, Some(id)).unwrap();
        edit(&mut o);
        let n = o.discard_note();
        assert!(
            n.contains("stay in its Undo History") && !n.contains("Onion"),
            "{n}"
        );
        // A setting outside the history changed too: it is named as lost.
        Arc::get_mut(&mut o.doc).unwrap().project_mut().onion.mode =
            plumspirit_engine::model::OnionMode::AllOthers;
        assert!(
            o.discard_note().contains("Onion Skin"),
            "{}",
            o.discard_note()
        );
        // An imported MIDI file has no project file to keep a history for.
        o.source = DocSource::Midi(dir.join("d.mid"));
        assert!(o.discard_note().starts_with("Your changes will be lost"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::{DocSource, format_bpm, format_clock, live_bpm, window_title};
    use crate::audio::Transport;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::conductor::Interp;
    use plumspirit_engine::model::{Bpm, PlumDocument, Speed, TempoValue};

    fn set_tempo(d: &mut PlumDocument, h: &mut History, tick: u64, bpm: &str, interp: Interp) {
        let edit = d
            .conductor()
            .prepare_set_tempo(
                None,
                tick,
                TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                interp,
            )
            .unwrap();
        h.apply(
            d,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        )
        .unwrap();
    }

    #[test]
    fn the_live_bpm_follows_steps_ramps_seeks_and_undo_but_not_playback_speed() {
        let normal = Speed::parse("1").unwrap();
        let mut d = PlumDocument::new_project("bpm".into(), Bpm::parse("120").unwrap(), 960);
        let mut h = History::default();
        // Constant tempo, anywhere (start, far end).
        assert_eq!(live_bpm(&d, 0, normal).0, "120");
        assert_eq!(live_bpm(&d, u64::MAX / 2, normal).0, "120");
        // Step at 1920: before / at / after.
        set_tempo(&mut d, &mut h, 1920, "172.5", Interp::Step);
        assert_eq!(live_bpm(&d, 1919, normal).0, "120");
        assert_eq!(live_bpm(&d, 1920, normal).0, "172.5");
        // Linear ramp 3840 (100) -> 5760 (200): exact value inside, not the start value.
        set_tempo(&mut d, &mut h, 3840, "100", Interp::Linear);
        set_tempo(&mut d, &mut h, 5760, "200", Interp::Step);
        assert_eq!(live_bpm(&d, 3840, normal).0, "100");
        assert_eq!(live_bpm(&d, 4320, normal).0, "125");
        assert_eq!(live_bpm(&d, 4800, normal).0, "150");
        assert_eq!(live_bpm(&d, 5760, normal).0, "200");
        // Playback Speed is shown only in the tooltip.
        let fast = Speed::parse("2").unwrap();
        let (text, tip) = live_bpm(&d, 4800, fast);
        assert_eq!(text, "150");
        assert!(
            tip.contains("Project BPM: 150") && tip.contains("Playback Speed"),
            "{tip}"
        );
        assert!(!live_bpm(&d, 4800, normal).1.contains("Playback Speed"));
        // Seek (stopped): the cursor position is used, not tick 0.
        let mut t = Transport::default();
        t.seek(4800, &d, 1);
        assert_eq!(live_bpm(&d, t.position(), normal).0, "150");
        // A loop wrapping back to its start shows the start's BPM again.
        t.seek(1920, &d, 1);
        assert_eq!(live_bpm(&d, t.position(), normal).0, "172.5");
        // Undo / Redo of the automation.
        h.undo(&mut d).unwrap();
        assert_eq!(live_bpm(&d, 5760, normal).0, "100");
        h.redo(&mut d).unwrap();
        assert_eq!(live_bpm(&d, 5760, normal).0, "200");
        // Project reload keeps the same values.
        let bytes = plumspirit_engine::native::project_to_bytes(&d).unwrap();
        let (back, _) = plumspirit_engine::native::project_from_bytes(&bytes).unwrap();
        for tick in [0, 1920, 4320, 4800, 5760] {
            assert_eq!(live_bpm(&back, tick, normal), live_bpm(&d, tick, normal));
        }
        // Readable precision.
        assert_eq!(format_bpm(120.125), "120.125");
        assert_eq!(format_bpm(120.0000000001), "120");
        assert_eq!(format_bpm(99.99999), "100");
    }

    /// v0.9 §081 / §225: the title names the open file (with its
    /// extension), a new project by its name, marks unsaved changes and is the
    /// release name alone without a project.
    #[test]
    fn the_window_title_names_the_open_file() {
        use std::path::PathBuf;
        let r = crate::RELEASE_NAME;
        assert_eq!(window_title(None), r);
        let lup = DocSource::Project(PathBuf::from("/music/My Song.lup"));
        let mid = DocSource::Midi(PathBuf::from("C:/x/My Song.mid"));
        // The file, not the stored project name (a renamed file, Save As).
        assert_eq!(
            window_title(Some((&lup, "Old Name", false))),
            format!("My Song.lup — {r}")
        );
        assert_eq!(
            window_title(Some((&mid, "My Song", false))),
            format!("My Song.mid — {r}")
        );
        assert_eq!(
            window_title(Some((&lup, "x", true))),
            format!("* My Song.lup — {r}")
        );
        assert_eq!(
            window_title(Some((&DocSource::New, "Black MIDI 1", false))),
            format!("Black MIDI 1 — {r}")
        );
        assert_eq!(
            window_title(Some((&DocSource::New, "  ", true))),
            format!(
                "* {} — {r}",
                plumspirit_engine::model::project::DEFAULT_PROJECT_NAME
            )
        );
    }

    #[test]
    fn clock_format() {
        assert_eq!(format_clock(0.0), "0:00.000");
        assert_eq!(format_clock(61.5), "1:01.500");
        assert_eq!(format_clock(3723.0), "1:02:03");
        assert_eq!(format_clock(f64::NAN), "—");
    }
}
