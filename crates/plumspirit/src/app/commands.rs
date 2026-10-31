//! Every user command (menu item, toolbar button, keyboard shortcut) is a
//! [`Command`]. Enabled-state rules live in one place (`PlumSpiritApp::enabled`), so
//! a command is never shown as available when it would do nothing.

use crate::piano_roll::Tool;
use crate::settings::ThemeSetting;
use plumspirit_engine::model::OnionMode;
use plumspirit_engine::model::project::{DensityBasis, OnionOrder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewProject,
    Open,
    Save,
    SaveAs,
    Export,
    ProjectProperties,
    Exit,
    Undo,
    Redo,
    Copy,
    Paste,
    Delete,
    SelectAll,
    FlipX,
    FlipY,
    ScaleX,
    ScaleY,
    Quantize,
    Theme(ThemeSetting),
    /// v0.9 (V09-S08): View → Track List / Piano Roll.
    MainView(crate::app::track_list::MainView),
    /// B1-0061: View → Switch Track List / Piano Roll (Tab): the other main
    /// view; each keeps its scroll, zoom, selection and open clip.
    SwitchView,
    /// v0.9 (V09-0142): Track List — open the selected clip (Enter), the
    /// previous / next track (Ctrl+↑ / Ctrl+↓).
    OpenClip,
    PreviousTrack,
    NextTrack,
    /// v0.9 §041 (V09-S10): Add Piano Roll Clip on the active track at the
    /// edit cursor (Insert; Song menu).
    AddClip,
    /// v0.9 §088 (V09-S13): Paste Repeat… (Ctrl+B) — the selected notes in
    /// the Piano Roll (B1-0099: the Duplicate V2 tool window, id
    /// `paste_repeat_notes`; the Edit menu names it so there) or the
    /// selected clips in the Track List (its dialog).
    PasteRepeat,
    /// v0.9 §047 (V09-S12): Merge Clips in Sequence / Merge Clips Stacked —
    /// the Track List's selected clips, through the merge Preview.
    MergeClips(plumspirit_engine::editor::clip_merge::MergeKind),
    /// v1.0-beta2 (B2-F29, §023): move the Track List's selected clips one
    /// Snap step earlier / later (← / →; one beat with Snap off), one Undo
    /// step each; their ids stay.
    NudgeClipsLeft,
    NudgeClipsRight,
    /// v1.0-beta2 (B2-F29): View → Zoom to Selection — the selected clips
    /// across the Track List, or the selected notes across the Piano Roll.
    ZoomToSelection,
    /// v1.0-beta2 (B2-F29): View → Jump to Playhead — scrolls the view shown
    /// to the playhead (and resumes a paused Follow Playhead).
    JumpToPlayhead,
    ResetView,
    ResetZoom,
    FitSong,
    ToggleFollow,
    ToggleSmoothFollow,
    /// v0.9 §217: View → Display Note Colors (clip mini notes).
    ToggleDisplayNoteColors,
    ToggleVelocityLane,
    /// Select the Conductor (tempo, time signatures and markers).
    SelectConductor,
    AddMarker,
    AddTimeSignature,
    AddTempoChange,
    Onion(OnionMode),
    ChooseOnionTracks,
    /// B1-0064: Onion Skin on / off (the toolbar's Onion button, Ctrl+Shift+O);
    /// on brings back the last mode used.
    ToggleOnion,
    /// B1-0064: shows the selected tracks as ghosts, or hides them when they
    /// all are (Ctrl+Alt+O).
    OnionSelectedTracks,
    /// L-G03: the selected tracks become the only ghosts (Replace).
    OnionShowOnlySelected,
    /// L-G03: the selected tracks are added to the ghosts (Add).
    OnionAddSelected,
    /// L-G03: the selected tracks are no longer ghosts (Remove).
    OnionRemoveSelected,
    /// L-G03: Per-Track Settings — the setting in force for every track.
    OnionUseForEveryTrack,
    /// B1-0064: Onion Skin → Per-Track Settings (each track keeps its own).
    ToggleOnionPerTrack,
    AddTrack,
    /// B2-F03: Add Tracks… — a count of empty tracks below the active track
    /// (at the end from the Conductor), one undo step.
    AddTracks,
    DeleteTrack,
    Tool(Tool),
    ZoomIn,
    ZoomOut,
    PlayPause,
    /// v1.0-beta1 (B1-0080): stop and return to the anchor.
    Stop,
    /// v1.0-beta1 (B1-0080): the Edit / Anchor playhead on / off.
    ToggleAnchorPlayhead,
    /// v1.0-beta1 (B1-0058): the Settings window.
    OpenSettings,
    /// v1.0-beta1 (B1-0037): the Lua Plugins window.
    ShowLuaPlugins,
    /// B1-0059: run Lua plugins at all (Settings → Plugins).
    ToggleRunLuaPlugins,
    /// B1-0059: enable a plugin as soon as it is installed.
    ToggleLuaEnableOnInstall,
    GoToStart,
    PreviousMeasure,
    NextMeasure,
    GoToEnd,
    ChooseSoundFont,
    ToggleDevOverlay,
    // v0.4
    SelectRange,
    ToggleLoop,
    LoopToSelection,
    ClearLoop,
    PlayingTime,
    TrackProperties,
    FitTrackEnd,
    PlayAll,
    /// v1.0-Launch L-F08 / L-F10 (contract §016): a transient monitor set
    /// of the selected tracks; back to the song's Mute / Solo; the current
    /// track's Mute / Solo.
    PlayOnlySelected,
    RestoreAllTracks,
    MuteCurrentTrack,
    SoloCurrentTrack,
    ToggleGraph2,
    ToggleEventList,
    CustomFunctions,
    ToggleMetrics,
    /// v0.9 (V09-0050): MIDI export writes every status byte.
    ToggleExplicitStatus,
    /// v1.0-Launch L-C04: MIDI export adds Program Change 0 when the file
    /// would hold no control event (Kiva).
    TogglePlayerSafeStart,
    /// v1.0-Launch L-S51: MIDI export adds a silent loudness reference for
    /// Piano From Above.
    ToggleLoudnessReference,
    // v0.5
    ImportIntoTrack,
    AddSubtrack,
    AddSubtracks,
    AddSubchannel,
    AddSubchannels,
    Subchannels,
    MoveToSubchannel,
    OnionOrder(OnionOrder),
    OnionDensity(DensityBasis),
    OnionOpacity,
    TogglePreview,
    FollowPosition,
    RecentTasks,
    /// v0.8: Edit → Undo History… (the persistent history window).
    UndoHistory,
    // v0.6
    ToggleProgressor,
    ProgressorSettings,
    /// v0.9 (V09-S17): View → MIDI Player (F10): the Player replaces the
    /// editor area (`docs/PLAYER.md` P10).
    ToggleMidiPlayer,
    MidiPlayerSettings,
    /// v0.9 (V09-S18): View → MIDI Inspector (`docs/INSPECTOR.md`).
    ToggleMidiInspector,
    ToggleLiveKeys,
    KeyScope(crate::app::progressor::ScopeSetting),
    /// L-G02: the keys show what the Piano Roll shows (the default).
    KeyFollowRoll,
    ToggleAudition,
    /// Length of the sound when a note is placed (milliseconds).
    AuditionLength(u32),
    // v0.7
    /// Opens the window of a transformation tool (registry id).
    OpenTool(&'static str),
    /// Saves the selected notes as a `.psc` score.
    SaveScoreAs,
    /// Inserts a `.psc` score (Insert Score window).
    InsertScore,
    /// Saves the active Event Graph's selected points as an automation score.
    SaveAutomationAs,
    /// Puts the selected notes into a new group (Ctrl+G).
    GroupNotes,
    /// Takes the selected notes out of their innermost group (Ctrl+Shift+G).
    UngroupNotes,
    /// Mutes / unmutes the selected notes (toggle; Ctrl+M).
    ToggleMuteNotes,
    /// Scale handles fill rows when stretching vertically (setting).
    /// Close the project and return to the start screen (Ctrl+W).
    CloseProject,
    /// Onion Skin ghost outlines (setting, default on).
    ToggleOnionOutlines,
    /// New subchannels skip MIDI channel 10 (setting).
    ToggleSkipChannel10,
    /// Selection analytics overlay (setting, default on).
    ToggleSelectionAnalytics,
    /// v1.0-Launch L-E01: the selection's advanced section — its charts
    /// (setting; also the readout's caret).
    ToggleSelectionCharts,
    /// v0.8: View → Selection Analytics Settings… (the readout's text).
    SelectionAnalyticsSettings,
    /// Reopens the last used tool window (F5).
    RepeatLastTool,
    /// v0.8: reopen the last tool's window (F5 before v0.8).
    ReopenLastTool,
    /// v0.8 §198: Repeat Last Tool with fresh seeds.
    RepeatLastToolNewSeed,
    /// v0.8 §199: one-click Mute / Unmute of the selected notes.
    MuteNotes,
    UnmuteNotes,
    /// v0.9 §082: Duplicate Up / Down One Octave (Ctrl+Alt+↑ / ↓): a copy
    /// of every selected note ±12 semitones away, one undo step.
    DuplicateOctaveUp,
    DuplicateOctaveDown,
    /// v0.9 §083: Ctrl+P paints the selected notes with the current
    /// subchannel (the one new notes get).
    PaintCurrentSubchannel,
    /// v0.9 §119: View → Selection Opacity… (0–100 %, application setting).
    SelectionOpacity,
    /// v0.9 §118: one-click Slice Notes — cuts the selected notes at the
    /// edit cursor, or at a ruler time selection's start and end (Ctrl+K).
    SliceNotes,
    /// v0.8 §152: clipboard Cut (Copy, then Delete) — menu only; Ctrl+X is
    /// Delete Selection.
    Cut,
    /// v0.8 §153: Y-Zoom − / +.
    ZoomInY,
    ZoomOutY,
    /// B1-0088: View → Fit 128 Keys / Fit All 256 Keys (256-Key Mode).
    FitKeys128,
    FitAllKeys,
    /// B1-0089 / B1-0087: View → Lock Fit Range, Y Zoom Beyond the Keys.
    ToggleLockFitRange,
    ToggleYZoomBeyondKeys,
    /// Keyboard Shortcuts window (F1).
    ShowKeybinds,
    /// B1-0050: Help → PlumSpirit Help… (F1): the Help window, at the
    /// topic of what is shown now.
    ShowHelp,
    /// B1-0051: Help → Quick Start…
    ShowQuickStart,
    /// B1-0051: Help → Show the Quick Start on the Start Screen.
    ToggleOrientation,
    /// Help → About PlumSpirit (version, built-in plugin credits).
    ShowAbout,
    /// B1-0059: Help → Save Diagnostics Report…
    SaveDiagnostics,
    /// A built-in plugin with its own window (v0.8).
    OpenPlugin(plumspirit_engine::plugins::PluginId),
    /// Track list: move the active track (or selection) up / down.
    MoveTrackUp,
    MoveTrackDown,
    /// Track list: indent / outdent the active track.
    IndentTrack,
    OutdentTrack,
    /// Delete the selected tracks (with subtracks).
    DeleteSelectedTracks,
    /// The Zoom tool window (F8) and its view history.
    ZoomTool,
    ZoomBack,
    ZoomForward,
    /// Play Selected / Scrub window (F9).
    ScrubTool,
    /// v0.9 §057: View → 88-Key Guide.
    ToggleKeyGuide,
    /// B1-0106: View → Respect Track Mute/Solo in Progressor (Progressor
    /// scope Audible Tracks ↔ All Tracks).
    ToggleProgressorMuteSolo,
    /// B1-0115: Tools → Simple → the scale handles duplicate notes when they
    /// stretch the rows (Alt reverses it for one drag).
    ToggleScaleYDuplicate,
    /// B1-0116: Tools → Simple → Scale Y overlap prevention.
    ToggleScaleYOverlap,
    /// B1-0091: Tools → Simple → Rotation Pivot (true = the selection's center).
    RotatePivot(bool),
    /// B1-0117: Edit → Note Overlap Prevention (on / off, off by default).
    ToggleOverlapPrevention,
    /// B1-0117: what counts as an overlap (`overlap::Mode::ALL` index).
    OverlapMode(u8),
    /// B1-0117: only notes the operation created or changed may go.
    ToggleOverlapGeneratedOnly,
    /// B1-0117: notes on different subchannels / MIDI channels overlap too.
    ToggleOverlapAcross,
    /// B1-0117: results are compared with the notes already in the track.
    ToggleOverlapExisting,
    /// B1-0059: hides the Piano Roll's text overlays (Progressor, Selection
    /// Analytics) without changing their own settings.
    ToggleTextOverlays,
    /// B1-0059: View → Theme → High Contrast.
    ToggleHighContrast,
    /// B1-0062: View → UI Scale → Reset to 100 % (Ctrl+Alt+0).
    ResetUiScale,
}

impl Command {
    /// The command that opens a built-in plugin (None while it has no window).
    /// The command that opens a built-in plugin (v0.9 §024): a plugin that
    /// is a transform tool opens that tool, every other one its own window.
    /// Every registered plugin is user-openable, so every one has a command
    /// (v0.8 returned `None` for Spiderweb and Audio Reference, which hid
    /// them from the Plugins menu).
    pub fn for_plugin(id: plumspirit_engine::plugins::PluginId) -> Command {
        match plumspirit_engine::plugins::info(id).tool {
            Some(t) => Command::OpenTool(t),
            None => Command::OpenPlugin(id),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Command::NewProject => "New Project…",
            Command::Open => "Open…",
            Command::Save => "Save",
            Command::SaveAs => "Save As…",
            Command::Export => "Export MIDI…",
            Command::ProjectProperties => "Project Properties…",
            Command::Exit => "Exit",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::Copy => "Copy",
            Command::Paste => "Paste",
            Command::Delete => "Delete",
            Command::SelectAll => "Select All",
            Command::FlipX => "Flip X",
            Command::FlipY => "Flip Y",
            Command::ScaleX => "Scale X…",
            Command::ScaleY => "Scale Y…",
            Command::Quantize => "Quantizer",
            Command::ResetZoom => "Reset Zoom",
            Command::FitSong => "Fit Song",
            Command::ToggleFollow => "Follow Playhead",
            Command::ToggleSmoothFollow => "Smooth Follow",
            Command::ToggleDisplayNoteColors => "Display Note Colors",
            Command::SelectConductor => "Select Conductor",
            Command::AddMarker => "Add Marker…",
            Command::AddTimeSignature => "Add Time Signature…",
            Command::AddTempoChange => "Add Tempo Change…",
            Command::MainView(crate::app::track_list::MainView::TrackList) => "Track List",
            Command::MainView(crate::app::track_list::MainView::PianoRoll) => "Piano Roll",
            Command::SwitchView => "Switch Track List / Piano Roll",
            Command::OpenClip => "Open Clip in Piano Roll",
            Command::AddClip => "Add Piano Roll Clip",
            Command::PasteRepeat => "Paste Repeat…",
            Command::MergeClips(plumspirit_engine::editor::clip_merge::MergeKind::Sequence) => {
                "Merge Clips in Sequence…"
            }
            Command::MergeClips(plumspirit_engine::editor::clip_merge::MergeKind::Stack) => {
                "Merge Clips Stacked…"
            }
            Command::PreviousTrack => "Previous Track",
            Command::NextTrack => "Next Track",
            Command::NudgeClipsLeft => "Nudge Clips Left",
            Command::NudgeClipsRight => "Nudge Clips Right",
            Command::ZoomToSelection => "Zoom to Selection",
            Command::JumpToPlayhead => "Jump to Playhead",
            Command::Theme(ThemeSetting::Dark) => "Dark",
            Command::Theme(ThemeSetting::Light) => "Light",
            Command::Theme(ThemeSetting::System) => "System",
            Command::ResetView => "Reset Piano Roll View",
            Command::ToggleVelocityLane => "Event Graphs",
            Command::Onion(m) => m.label(),
            Command::ChooseOnionTracks => "Choose Tracks…",
            Command::ToggleOnion => "Onion Skin On / Off",
            Command::OnionSelectedTracks => "Onion Skin: Show / Hide Selected Tracks",
            Command::OnionShowOnlySelected => "Onion Skin: Show Only Selected Tracks",
            Command::OnionAddSelected => "Onion Skin: Add Selected Tracks",
            Command::OnionRemoveSelected => "Onion Skin: Remove Selected Tracks",
            Command::OnionUseForEveryTrack => "Onion Skin: Use This Setting for Every Track",
            Command::ToggleOnionPerTrack => "Per-Track Settings",
            Command::AddTrack => "Add Track",
            Command::AddTracks => "Add Tracks…",
            Command::DeleteTrack => "Delete Track",
            Command::Tool(Tool::Select) => "Select",
            Command::Tool(Tool::Draw) => "Draw",
            Command::Tool(Tool::Erase) => "Erase",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::PlayPause => "Play / Pause",
            Command::Stop => "Stop",
            Command::ToggleAnchorPlayhead => "Edit / Anchor Playhead",
            Command::OpenSettings => "Settings…",
            Command::GoToStart => "Go to Beginning",
            Command::PreviousMeasure => "Previous Measure",
            Command::NextMeasure => "Next Measure",
            Command::GoToEnd => "Go to End",
            Command::ChooseSoundFont => "Choose SoundFont…",
            Command::ToggleDevOverlay => "Developer Overlay",
            Command::SelectRange => "Select Range…",
            Command::ToggleLoop => "Loop",
            Command::LoopToSelection => "Loop Selected Time",
            Command::ClearLoop => "Clear Loop",
            Command::PlayingTime => "Playing Time…",
            Command::TrackProperties => "Track Properties…",
            Command::FitTrackEnd => "Fit Track End to Content",
            Command::PlayAll => "Play All (clear Mute / Solo)",
            Command::PlayOnlySelected => "Play Only Selected Tracks",
            Command::RestoreAllTracks => "Restore All Tracks",
            Command::MuteCurrentTrack => "Mute Current Track",
            Command::SoloCurrentTrack => "Solo Current Track",
            Command::ToggleGraph2 => "Event Graph 2",
            Command::ToggleEventList => "Event List",
            Command::CustomFunctions => "Custom Curve Functions…",
            Command::ToggleMetrics => "CPU / RAM in Status Bar",
            Command::ToggleExplicitStatus => "MIDI Export: Use Running Status Compression",
            Command::TogglePlayerSafeStart => {
                "MIDI Export: Add Program Change 0 When the File Has No Control Event"
            }
            Command::ToggleLoudnessReference => {
                "MIDI Export: Add a Silent Loudness Reference for Piano From Above"
            }
            Command::ImportIntoTrack => "Import MIDI Into Current Track…",
            Command::AddSubtrack => "Add Subtrack",
            Command::AddSubtracks => "Add Subtracks…",
            Command::AddSubchannel => "Add Subchannel",
            Command::AddSubchannels => "Add Subchannels…",
            Command::Subchannels => "Subchannels…",
            Command::MoveToSubchannel => "Move Selected Notes to Subchannel…",
            Command::OnionOrder(o) => o.label(),
            Command::OnionDensity(d) => d.label(),
            Command::OnionOpacity => "Ghost Opacity…",
            Command::TogglePreview => "Preview Batch Edits",
            Command::FollowPosition => "Follow Position…",
            Command::RecentTasks => "Recent Operations…",
            Command::UndoHistory => "Undo History…",
            Command::ToggleProgressor => "Progressor",
            Command::ProgressorSettings => "Progressor Settings…",
            Command::ToggleMidiPlayer => "MIDI Player",
            Command::MidiPlayerSettings => "MIDI Player Settings…",
            Command::ToggleMidiInspector => "MIDI Inspector",
            Command::ToggleLiveKeys => "Live Keyboard Colors",
            Command::KeyScope(s) => s.label(),
            Command::KeyFollowRoll => "What the Piano Roll Shows (Active Track + Onion Ghosts)",
            Command::ToggleAudition => "Note Audition",
            Command::SaveScoreAs => "Save Selection as Score (.psc)…",
            Command::InsertScore => "Insert Score (.psc)…",
            Command::SaveAutomationAs => "Save Graph Selection as Score…",
            Command::GroupNotes => "Group Notes",
            Command::UngroupNotes => "Ungroup Notes",
            Command::ToggleMuteNotes => "Mute / Unmute Notes",
            Command::CloseProject => "Close Project",
            Command::ToggleOnionOutlines => "Ghost Outlines",
            Command::ToggleSkipChannel10 => "New Subchannels Skip MIDI Channel 10",
            Command::ToggleSelectionAnalytics => "Selection Analytics",
            Command::ToggleSelectionCharts => "Selection Analytics Charts",
            Command::ToggleKeyGuide => "88-Key Guide",
            Command::ToggleProgressorMuteSolo => "Respect Track Mute/Solo in Progressor",
            Command::ToggleScaleYDuplicate => "Scale Handles: Duplicate Notes",
            Command::ToggleScaleYOverlap => "Scale Y: Prevent Overlaps",
            Command::RotatePivot(true) => "Rotate About the Selection's Center",
            Command::RotatePivot(false) => "Rotate About the Opposite Corner",
            Command::ToggleOverlapPrevention => "Note Overlap Prevention",
            Command::OverlapMode(m) => match m {
                0 => "Exact Duplicates",
                1 => "Same Key and Start",
                2 => "Same-Key Overlaps: Shorten the Earlier Note",
                _ => "Same-Key Overlaps: Remove the Later Note",
            },
            Command::ToggleOverlapGeneratedOnly => "Only Notes the Operation Made or Changed",
            Command::ToggleOverlapAcross => "Across Subchannels and MIDI Channels",
            Command::ToggleOverlapExisting => "Also Against Notes Already There",
            Command::ToggleTextOverlays => "Hide Text Overlays",
            Command::ToggleHighContrast => "High Contrast",
            Command::ResetUiScale => "Reset UI Scale to 100 %",
            Command::SelectionAnalyticsSettings => "Selection Analytics Settings…",
            Command::RepeatLastTool => "Repeat Last Tool",
            Command::ReopenLastTool => "Reopen Last Tool…",
            Command::RepeatLastToolNewSeed => "Repeat Last Tool (New Seed)",
            Command::MuteNotes => "Mute Notes",
            Command::UnmuteNotes => "Unmute Notes",
            Command::SliceNotes => "Slice Notes",
            Command::SelectionOpacity => "Selection Opacity…",
            Command::PaintCurrentSubchannel => "Paint Selection with Current Subchannel",
            Command::DuplicateOctaveUp => "Duplicate Up One Octave",
            Command::DuplicateOctaveDown => "Duplicate Down One Octave",
            Command::Cut => "Cut (Copy + Delete)",
            Command::ZoomInY => "Zoom In Y",
            Command::ZoomOutY => "Zoom Out Y",
            Command::FitKeys128 => "Fit 128 Keys",
            Command::FitAllKeys => "Fit All 256 Keys",
            Command::ToggleLockFitRange => "Lock Fit Range",
            Command::ToggleYZoomBeyondKeys => "Y Zoom Beyond the Keys",
            Command::ShowKeybinds => "Keyboard Shortcuts…",
            Command::ShowHelp => "PlumSpirit Help…",
            Command::ShowQuickStart => "Quick Start…",
            Command::ToggleOrientation => "Show the Quick Start on the Start Screen",
            Command::ShowAbout => "About PlumSpirit and Built-in Plugins…",
            Command::ShowLuaPlugins => "Lua Plugins…",
            Command::ToggleRunLuaPlugins => "Run Lua Plugins",
            Command::ToggleLuaEnableOnInstall => "Enable Plugins When Installed",
            Command::SaveDiagnostics => "Save Diagnostics Report…",
            Command::OpenPlugin(id) => match id {
                plumspirit_engine::plugins::PluginId::ImageToMidi => "Image to MIDI…",
                plumspirit_engine::plugins::PluginId::Spiderweb => "Spiderweb…",
                plumspirit_engine::plugins::PluginId::AudioReference => "Audio Reference…",
                plumspirit_engine::plugins::PluginId::HzChop => "HZ-Chop…",
                plumspirit_engine::plugins::PluginId::Safor => "SAFOR…",
            },
            Command::MoveTrackUp => "Move Track Up",
            Command::MoveTrackDown => "Move Track Down",
            Command::IndentTrack => "Indent Track (make subtrack)",
            Command::OutdentTrack => "Outdent Track",
            Command::DeleteSelectedTracks => "Delete Selected Tracks…",
            Command::ZoomTool => "Zoom Tool…",
            Command::ZoomBack => "Previous View",
            Command::ZoomForward => "Next View",
            Command::ScrubTool => "Play Selected / Scrub…",
            Command::OpenTool(id) => {
                plumspirit_engine::transform::tool(id).map_or("Tool", |t| (t.spec)().name)
            }
            Command::AuditionLength(ms) => match ms {
                100 => "100 ms",
                250 => "250 ms (default)",
                400 => "400 ms",
                800 => "800 ms",
                _ => "Custom",
            },
        }
    }

    /// v0.9 (V09-S08): the command edits or shows notes of the active track
    /// (the Piano Roll's work). Run from a menu while the Track List is
    /// shown, it opens the Piano Roll first; its keyboard shortcut does
    /// nothing there (the Track List's own keys come with its context
    /// matrix, V09-S10).
    pub fn acts_on_notes(self) -> bool {
        matches!(
            self,
            Command::Copy
                | Command::Paste
                | Command::Cut
                | Command::Delete
                | Command::SelectAll
                | Command::FlipX
                | Command::FlipY
                | Command::ScaleX
                | Command::ScaleY
                | Command::Quantize
                | Command::Tool(_)
                | Command::ResetView
                | Command::SelectRange
                | Command::CustomFunctions
                | Command::ImportIntoTrack
                | Command::MoveToSubchannel
                | Command::OpenTool(_)
                | Command::OpenPlugin(_)
                | Command::SaveScoreAs
                | Command::InsertScore
                | Command::SaveAutomationAs
                | Command::GroupNotes
                | Command::UngroupNotes
                | Command::ToggleMuteNotes
                | Command::RepeatLastTool
                | Command::ReopenLastTool
                | Command::RepeatLastToolNewSeed
                | Command::MuteNotes
                | Command::UnmuteNotes
                | Command::DuplicateOctaveUp
                | Command::DuplicateOctaveDown
                | Command::PaintCurrentSubchannel
                | Command::SliceNotes
                | Command::ZoomTool
                | Command::ZoomBack
                | Command::ZoomForward
                | Command::ScrubTool
        )
    }

    /// Shortcut text shown next to menu items (from the central keybind
    /// registry, `app/keybinds.rs`).
    /// Every chord of the command, for menus and tooltips (v0.8 §074):
    /// table rows first ("Del, Ctrl+X"), then the clipboard chords handled
    /// outside the table.
    pub fn shortcut(self) -> String {
        match self {
            Command::Exit => "Alt+F4".into(),
            Command::Copy => "Ctrl+C".into(),
            Command::Paste => "Ctrl+V".into(),
            _ => super::keybinds::shortcuts_of(self).join(", "),
        }
    }
}

#[cfg(test)]
mod plugin_menu_tests {
    use super::Command;
    use plumspirit_engine::plugins::{PLUGINS, PluginKind};

    #[test]
    fn every_registered_plugin_has_a_plugins_menu_command() {
        // v0.9 §024 / §261: the registry is the list the menu walks.
        let shown: Vec<Command> = [PluginKind::PianoRoll, PluginKind::General]
            .into_iter()
            .flat_map(crate::ui::menu::plugin_menu_commands)
            .collect();
        assert_eq!(shown.len(), PLUGINS.len(), "one menu entry per plugin");
        for p in PLUGINS {
            let c = Command::for_plugin(p.id);
            assert!(
                shown.contains(&c),
                "{:?} missing from the Plugins menu",
                p.id
            );
            match c {
                Command::OpenPlugin(id) => assert_eq!(id, p.id),
                Command::OpenTool(t) => assert_eq!(Some(t), p.tool),
                other => panic!("{:?} opens {other:?}", p.id),
            }
            assert!(!c.label().trim().is_empty(), "{:?} has no menu label", p.id);
        }
        use plumspirit_engine::plugins::PluginId::*;
        for id in [Spiderweb, AudioReference, ImageToMidi] {
            assert_eq!(Command::for_plugin(id), Command::OpenPlugin(id));
        }
    }
}

#[cfg(test)]
mod label_tests {
    use super::Command;

    /// B1-0067: every command label is in Title Case (short words lower
    /// inside; a parenthetical note and a file extension keep their case),
    /// and a command that opens a window ends with "…".
    #[test]
    fn command_labels_are_title_case() {
        const SMALL: &[&str] = &[
            "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "of", "off", "on",
            "or", "per", "the", "to", "via", "vs", "with",
        ];
        let mut cmds = crate::app::keybinds::bindable();
        cmds.extend([
            Command::NewProject,
            Command::Open,
            Command::SaveAs,
            Command::Export,
            Command::ShowKeybinds,
        ]);
        for c in cmds {
            let label = c.label();
            let outside: String = {
                let mut depth = 0;
                label
                    .chars()
                    .filter(|&ch| {
                        match ch {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        depth == 0 && ch != ')'
                    })
                    .collect()
            };
            for (i, w) in outside.split_whitespace().enumerate() {
                let first = w.chars().next().unwrap();
                if !first.is_alphabetic() || (i > 0 && SMALL.contains(&w)) {
                    continue;
                }
                assert!(first.is_uppercase(), "{c:?}: {label:?} ({w})");
            }
        }
        for c in [
            Command::NewProject,
            Command::Open,
            Command::SaveAs,
            Command::OpenSettings,
            Command::ShowKeybinds,
        ] {
            assert!(
                c.label().ends_with('…'),
                "{c:?} opens a window: {}",
                c.label()
            );
        }
    }
}
