//! Small modal dialogs. Each dialog is plain data; `show` draws it and returns what
//! the user chose. The application decides what happens next.

use super::functions::{FunctionsDraft, functions_ui};
use super::jobs::Then;
use super::timing::{RangeDraft, TimeDraft};
use super::tracks::{TrackPropsDraft, track_properties_ui};
use crate::ui::theme::{self, space};
use egui::{Id, Key, Modal, RichText};
use plumspirit_engine::editor::controllers::{self, ControllerKind};
use plumspirit_engine::midi::merge::{ConductorPolicy, MergeMode};
use plumspirit_engine::midi::{WideKeys, group_digits};
use plumspirit_engine::model::SubchannelId;
use plumspirit_engine::model::tempo::Speed;
use plumspirit_engine::view::grid::MeterMap;
use std::path::PathBuf;

/// Parses a tick position typed by the user: a whole number ≥ 0 (separators
/// `,` and `_` allowed). Nothing is wrapped or clamped.
pub fn parse_tick(input: &str, what: &str) -> Result<u64, String> {
    let t = input.trim();
    let s = t.replace([',', '_'], "");
    if s.is_empty() {
        return Err(format!("Enter the {what} in ticks."));
    }
    if s.starts_with('-') {
        return Err(format!("The {what} cannot be negative."));
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "\"{}\" is not a whole number of ticks.",
            t.chars().take(40).collect::<String>()
        ));
    }
    s.parse::<u64>()
        .map_err(|_| format!("The {what} is beyond the largest possible tick."))
}

fn whole_percent(text: &str) -> Result<u8, String> {
    crate::ui::numeric::integer(text.trim().trim_end_matches('%'))
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|v| *v <= 100)
        .ok_or_else(|| "Enter a whole percentage from 0 to 100.".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleAxis {
    X,
    Y,
}

impl ScaleAxis {
    pub fn title(self) -> &'static str {
        match self {
            ScaleAxis::X => "Scale X",
            ScaleAxis::Y => "Scale Y",
        }
    }
}

/// What a typed path (platforms without native dialogs) is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptPurpose {
    Open,
    Export,
    SaveProject(Then),
    SoundFont,
    /// Import MIDI Into Current Track (this track).
    ImportInto(plumspirit_engine::model::TrackId),
    /// v0.7 score files.
    SaveScore,
    InsertScore,
    SaveAutomation,
    /// v0.8 Image to MIDI.
    OpenImage,
    /// v0.8 Audio Reference.
    OpenReference,
    /// v0.8 Recent Files: the new place of this (missing) entry.
    Locate(std::path::PathBuf),
}

impl PromptPurpose {
    fn title(&self) -> &'static str {
        match self {
            PromptPurpose::Open => "Open",
            PromptPurpose::Export => "Export MIDI",
            PromptPurpose::SaveProject(_) => "Save Project",
            PromptPurpose::SoundFont => "Choose SoundFont",
            PromptPurpose::ImportInto(_) => "Import MIDI Into Current Track",
            PromptPurpose::SaveScore => "Save Selection as Score",
            PromptPurpose::InsertScore => "Insert Score",
            PromptPurpose::SaveAutomation => "Save Graph Selection as Score",
            PromptPurpose::OpenImage => "Open Picture (Image to MIDI)",
            PromptPurpose::OpenReference => "Load Reference Audio",
            PromptPurpose::Locate(_) => "Locate Moved File",
        }
    }
}

/// What a confirmed overwrite continues with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverwriteFor {
    Export,
    Save(Then),
}

#[derive(Clone, Debug)]
pub enum Dialog {
    /// v0.9 V09-S12 (§048): the clip merge Preview (`app/clip_merge_ui.rs`).
    MergeClips(Box<super::clip_merge_ui::MergeDialog>),
    /// v0.9 V09-S13 (§089): Paste Repeat on the selected clips
    /// (`app/clip_repeat_ui.rs`).
    RepeatClips(Box<super::clip_repeat_ui::RepeatDialog>),
    /// Unsaved changes: Save / Don't Save / Cancel.
    Dirty {
        name: String,
        then: Then,
        /// Saving will ask where to save (new or imported project).
        asks_path: bool,
        /// What "Don't Save" keeps (`OpenDoc::discard_note`).
        note: &'static str,
    },
    DeleteTrack {
        /// Subtree roots to delete (document order).
        roots: Vec<usize>,
        name: String,
        notes: usize,
        events: usize,
        /// Subtracks deleted together with it (the whole subtree).
        subtracks: usize,
        /// v0.8: a folder deleted together with its tracks (0: none).
        folder: u32,
    },
    Scale {
        axis: ScaleAxis,
        text: String,
        error: Option<String>,
        focused: bool,
        /// Live Preview on (the global preference).
        preview: bool,
        /// What Apply would do (filled by the app's Preview).
        summary: String,
        /// Scale Y: Duplicate Notes (v0.7 §139, B1-0115) and its options —
        /// off = Transform existing notes; `fill.mode` = Fill added rows or
        /// Copy by pitch step.
        fill_on: bool,
        fill: plumspirit_engine::transform::geom::Fill,
        /// B1-0116: Scale Y overlap prevention.
        prevent: bool,
        cleanup: plumspirit_engine::transform::geom::ScaleYCleanup,
    },
    Message {
        title: String,
        body: String,
        is_error: bool,
    },
    ConfirmOverwrite {
        path: PathBuf,
        purpose: OverwriteFor,
    },
    /// Path entry used on platforms without a native file dialog in this build.
    PathPrompt {
        purpose: PromptPurpose,
        text: String,
        focused: bool,
    },
    NewProject {
        name: String,
        bpm: String,
        ppqn: String,
        /// B1-0059: 256-Key Mode from the start (default from Settings).
        keys256: bool,
        error: Option<String>,
        focused: bool,
    },
    ProjectProperties {
        name: String,
        bpm: String,
        ppqn: String,
        /// v0.9 §058: 256-Key Mode (keys 0–255).
        keys256: bool,
        error: Option<String>,
        focused: bool,
    },
    /// B1-0103: Project Properties → Change PPQN (Experimental). `preview`
    /// holds the report of the inputs it was computed for.
    ChangePpqn(ChangePpqnDialog),
    /// The MIDI file cannot represent the project exactly, or tracks have
    /// corrections: `corrections` is whether they are applied (`None` when no
    /// track has any); `blocked` when the file cannot be written this way.
    ExportApproval {
        lines: Vec<String>,
        corrections: Option<bool>,
        blocked: bool,
        /// How subtracks and subchannels are written (v0.5): the summary and the
        /// two options, when the project has any structure MIDI cannot hold.
        structure: Option<ExportStructure>,
        /// v0.9 §061 / §062: keys above 127 and the chosen policy, when the
        /// export meets any.
        wide: Option<ExportWideKeys>,
    },
    /// L-D02: File → Open found several MIDI files in an archive.
    ArchiveChooser(Box<super::archive_open::ArchiveChooser>),
    /// v0.9 §062: File → Open met a non-standard 256-key MIDI file; it can be
    /// opened again as a 256-Key Mode project.
    OpenWideKeys {
        path: PathBuf,
        /// The import's explanation (which key, where).
        message: String,
        /// L-D05: the reading that met it was in Compatibility Mode.
        compat: bool,
    },
    /// L-D05: File → Open met a MIDI file outside the specification that the
    /// Compatibility Mode repairs; it can be opened again that way.
    OpenCompatible {
        path: PathBuf,
        /// The strict reading's explanation (what, where).
        message: String,
        keys: plumspirit_engine::model::project::KeyRange,
    },
    /// Add / edit a marker or chapter.
    Marker {
        id: Option<u32>,
        title: String,
        chapter: bool,
        start: String,
        /// End tick, or the duration when `by_duration`.
        end: String,
        by_duration: bool,
        /// Readable position of the start (shown under the fields).
        hint: String,
        error: Option<String>,
        focused: bool,
    },
    TimeSignature {
        id: Option<u32>,
        tick: String,
        numerator: String,
        denominator: String,
        /// The signature at tick 0 (its position cannot change).
        first: bool,
        hint: String,
        error: Option<String>,
        focused: bool,
    },
    Tempo {
        id: Option<u32>,
        tick: String,
        bpm: String,
        linear: bool,
        /// The tempo at tick 0 (its position cannot change, it cannot be deleted).
        first: bool,
        hint: String,
        error: Option<String>,
        focused: bool,
    },
    Controller {
        kind: ControllerKind,
        /// MIDI channel 0–15.
        channel: u8,
        /// Event index (for editing), `None` to add.
        index: Option<usize>,
        tick: String,
        value: String,
        error: Option<String>,
        focused: bool,
    },
    /// An edit too large for the undo memory (the operation waits in the app).
    TooLargeForUndo {
        label: String,
        message: String,
    },
    /// Add / edit a Master Volume point.
    MasterVolume {
        id: Option<u32>,
        tick: String,
        value: String,
        error: Option<String>,
        focused: bool,
    },
    TrackProperties(Box<TrackPropsDraft>, Option<MeterMap>),
    SelectRange(Box<RangeDraft>, bool),
    /// Playing Time: the app refreshes `lines` and `converted` every frame.
    PlayingTime {
        draft: Box<TimeDraft>,
        lines: Vec<(String, String)>,
        converted: (String, String),
        speed: Speed,
    },
    CurveFunctions(Box<FunctionsDraft>),
    // ------------------------------------------------------------- v0.5
    /// Add several subtracks / subchannels at once.
    AddMany {
        what: AddManyKind,
        /// Display name of the track the subtracks or subchannels go into.
        target: String,
        count: String,
        error: Option<String>,
        focused: bool,
    },
    /// B2-F03: Add Tracks… — a count of empty tracks at a place.
    AddTracks(Box<AddTracksDraft>),
    /// A percentage setting (Onion Skin opacity, Follow position).
    Percent {
        what: PercentKind,
        text: String,
        error: Option<String>,
        focused: bool,
    },
    /// "Subchannel N does not exist yet: create it?"
    CreateSubchannel {
        track: usize,
        id: SubchannelId,
        channel: u8,
    },
    EditSubchannel {
        track: usize,
        id: SubchannelId,
        name: String,
        /// MIDI channel 0–15.
        channel: u8,
        move_notes: bool,
        error: Option<String>,
    },
    DeleteSubchannel {
        track: usize,
        id: SubchannelId,
        name: String,
        notes: usize,
    },
    MoveToSubchannel {
        number: String,
        set_channel: bool,
        error: Option<String>,
        focused: bool,
    },
    /// Import MIDI Into Current Track: options and the structure preview.
    ImportIntoTrack(Box<MergeDraft>),
}

/// Export MIDI: the notes above key 127 the export meets and what happens to
/// them (v0.9 §061 / §062).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportWideKeys {
    pub notes: u64,
    /// Unmatched Note On / Off events above key 127.
    pub raw_events: u64,
    pub highest: u8,
    pub policy: WideKeys,
}

impl ExportWideKeys {
    /// The sentence above the choice.
    pub fn summary(&self) -> String {
        let what = match (self.notes, self.raw_events) {
            (n, 0) => format!("{} note(s) are", group_digits(n)),
            (0, r) => format!("{} unmatched Note On / Off event(s) are", group_digits(r)),
            (n, r) => format!(
                "{} note(s) and {} unmatched Note On / Off event(s) are",
                group_digits(n),
                group_digits(r)
            ),
        };
        format!(
            "{what} above key 127 (the highest is {}). A Standard MIDI file holds keys 0–127 only. What happens to them?",
            self.highest
        )
    }

    /// The hover text of a choice.
    pub fn tip(policy: WideKeys) -> &'static str {
        match policy {
            WideKeys::Reject => "Nothing is exported while the project has notes above key 127.",
            WideKeys::Discard => "Those notes are left out of the file. Your project keeps them.",
            WideKeys::Fold => {
                "Each moves down by whole octaves into keys 116–127 and keeps its pitch class (128 → 116, 200 → 116, 255 → 123). Your project keeps the original keys."
            }
            WideKeys::Clamp => "Each is written at key 127. Your project keeps the original keys.",
            WideKeys::Experimental => {
                "Keys above 127 are written as 8-bit key numbers, the way Kiva reads 256-key MIDI files. This is NOT a Standard MIDI file: other programs may refuse it or misread those notes. PlumSpirit opens it as a 256-Key Mode project."
            }
        }
    }

    /// The export button's text for the chosen policy.
    pub fn button(&self) -> &'static str {
        if self.policy.writes_wide() {
            "Export Experimental File…"
        } else {
            "Export…"
        }
    }
}

/// Export options about the track tree and subchannels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportStructure {
    pub lines: Vec<String>,
    pub split: bool,
    pub metadata: bool,
    /// Some tracks have subchannels sharing a MIDI channel (the split option
    /// matters).
    pub has_shared_channels: bool,
}

/// B2-F03: the Add Tracks… dialog's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct AddTracksDraft {
    pub at: plumspirit_engine::editor::folders::AddTracksAt,
    /// Where the tracks go, as a phrase ("below “Bass” and its subtracks").
    pub place: String,
    pub count: String,
    /// Tracks and subtracks the project has now.
    pub existing: usize,
    /// Free physical memory when the dialog opened (None: unknown).
    pub available: Option<u64>,
    pub error: Option<String>,
    pub focused: bool,
    /// A large count Add was pressed for once: pressing it again adds them.
    pub confirm: Option<usize>,
}

/// From this many tracks on, Add Tracks… asks once more before it adds them.
pub const ADD_TRACKS_CONFIRM: usize = 1_000;

/// Add Tracks…' preflight for the typed `text`: the count and the line
/// saying what it does (resulting track count, memory, rows), or the
/// sentence refusing it — all before anything changes.
pub fn add_tracks_preflight(
    text: &str,
    existing: usize,
    available: Option<u64>,
) -> Result<(usize, String), String> {
    use plumspirit_engine::editor::folders::{MAX_TRACKS, new_track_bytes};
    let n = match text
        .trim()
        .replace([',', '_', ' ', '\u{a0}'], "")
        .parse::<u64>()
    {
        Ok(n) if n >= 1 => n,
        _ => return Err("Enter a whole number of 1 or more.".into()),
    };
    let room = MAX_TRACKS.saturating_sub(existing);
    if n > room as u64 {
        return Err(format!(
            "A project can hold at most 65,535 tracks and subtracks (the limit of a MIDI file); {} more can be added.",
            group_digits(room as u64)
        ));
    }
    let n = n as usize;
    let bytes = (n * new_track_bytes()) as u64;
    if let Some(free) = available
        && bytes > free / 2
    {
        return Err(format!(
            "{} tracks need about {}, more than half of the memory this PC has free ({}). Add fewer tracks or close other programs.",
            group_digits(n as u64),
            crate::diagnostics::format_bytes(bytes),
            crate::diagnostics::format_bytes(free)
        ));
    }
    let mut line = format!(
        "The project will have {} tracks; the new ones take about {}.",
        group_digits((existing + n) as u64),
        crate::diagnostics::format_bytes(bytes)
    );
    if n >= ADD_TRACKS_CONFIRM {
        line.push_str(&format!(
            " The track list gets {} more rows (drawn as you scroll).",
            group_digits(n as u64)
        ));
    }
    Ok((n, line))
}

impl AddTracksDraft {
    /// The count Add was pressed for once and waits for a second press (it
    /// lapses when the typed count changes).
    pub fn pending(&self) -> Option<usize> {
        let n = add_tracks_preflight(&self.count, self.existing, self.available)
            .ok()?
            .0;
        self.confirm.filter(|&c| c == n)
    }

    /// Add pressed: the place and count to add now, or nothing — a refusal
    /// shown as the error, or a large count that asks once more first.
    pub fn press(&mut self) -> Option<(plumspirit_engine::editor::folders::AddTracksAt, usize)> {
        match add_tracks_preflight(&self.count, self.existing, self.available) {
            Ok((n, _)) if n >= ADD_TRACKS_CONFIRM && self.pending() != Some(n) => {
                self.confirm = Some(n);
                None
            }
            Ok((n, _)) => Some((self.at, n)),
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddManyKind {
    /// Subtracks under the track at this index.
    Subtracks(usize),
    Subchannels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercentKind {
    OnionOpacity,
    FollowPosition,
    /// v0.9 §119.
    SelectionOpacity,
}

/// The Import MIDI Into Current Track dialog's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeDraft {
    pub file: String,
    pub dest: String,
    pub mode: MergeMode,
    pub policy: ConductorPolicy,
    /// Insert at the current position instead of the project start.
    pub at_cursor: bool,
    pub cursor_text: String,
    pub preview: bool,
    /// Structure summary of the prepared import.
    pub lines: Vec<String>,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DialogOutcome {
    /// Keep showing the dialog.
    Open,
    /// v0.9 V09-S12: Merge in the clip merge Preview.
    ClipMergeApply,
    /// v0.9 V09-S13: OK in the clip Paste Repeat dialog.
    ClipRepeatApply,
    /// Close without action.
    Closed,
    DirtySave(Then),
    DirtyDiscard(Then),
    /// Roots, and the folder deleted with them (0: none).
    DeleteTrackConfirmed(Vec<usize>, u32),
    ScaleApply {
        axis: ScaleAxis,
        text: String,
        opts: plumspirit_engine::editor::ScaleY,
    },
    OverwriteConfirmed {
        path: PathBuf,
        purpose: OverwriteFor,
    },
    PathEntered {
        purpose: PromptPurpose,
        path: PathBuf,
    },
    NewProjectCreate {
        name: String,
        bpm: String,
        ppqn: String,
        keys256: bool,
    },
    PropertiesApply {
        name: String,
        bpm: String,
        keys256: bool,
    },
    /// B1-0103: Project Properties → Change PPQN (Experimental)….
    OpenChangePpqn,
    /// B1-0103: compute the Preview of the dialog's inputs.
    PpqnPreview,
    /// B1-0103: apply the dialog's change (one undo step).
    PpqnApply,
    ExportApproved,
    ApplyWithoutUndo,
    /// OK / Apply in one of the editing dialogs (the app reads its fields).
    Apply,
    /// Delete in one of the editing dialogs.
    Delete,
    /// Move the edit position here (Playing Time).
    GoTo(u64),
    /// Export: apply the track corrections or not.
    ExportCorrections(bool),
    /// v0.9 Export: what happens to keys above 127.
    ExportWideKeys(WideKeys),
    /// v0.9: open this non-standard 256-key file as a 256-Key Mode project.
    OpenWideKeys(PathBuf, bool),
    /// L-D05: open this file again in Compatibility Mode with these keys.
    OpenCompatible(PathBuf, plumspirit_engine::model::project::KeyRange),
    /// L-D02: open candidate `i` of the archive chooser.
    ArchiveOpen(usize),
    // v0.5
    /// Export: keep subchannels apart / write PlumSpirit structure data.
    ExportStructure {
        split: bool,
        metadata: bool,
    },
    AddMany(AddManyKind, usize),
    /// B2-F03: add this many tracks at this place.
    AddTracks(plumspirit_engine::editor::folders::AddTracksAt, usize),
    Percent(PercentKind, u8),
    CreateSubchannel {
        track: usize,
        id: SubchannelId,
        channel: u8,
    },
    EditSubchannel {
        track: usize,
        id: SubchannelId,
        name: String,
        channel: u8,
        move_notes: bool,
    },
    DeleteSubchannel {
        track: usize,
        id: SubchannelId,
    },
    MoveToSubchannel {
        number: String,
        set_channel: bool,
    },
    /// The Preview checkbox of a dialog changed (global preference).
    PreviewToggled(bool),
    /// Import Into Track: apply the prepared import.
    MergeApply,
}

/// B1-0103: the Change PPQN (Experimental) dialog's state.
#[derive(Clone, Debug)]
pub struct ChangePpqnDialog {
    pub from: u32,
    pub to: String,
    pub mode: plumspirit_engine::editor::ppqn::PpqnMode,
    /// The inputs the Preview below was computed for, and its lines.
    pub preview: Option<(
        (u32, plumspirit_engine::editor::ppqn::PpqnMode),
        Vec<String>,
    )>,
    /// A Preview / Apply is being computed.
    pub busy: bool,
    pub error: Option<String>,
    pub focused: bool,
}

impl ChangePpqnDialog {
    pub fn new(from: u32) -> ChangePpqnDialog {
        ChangePpqnDialog {
            from,
            to: String::new(),
            mode: plumspirit_engine::editor::ppqn::PpqnMode::Beats,
            preview: None,
            busy: false,
            error: None,
            focused: false,
        }
    }

    /// The typed PPQN and the mode (`Err`: the reason the input is refused).
    pub fn inputs(&self) -> Result<(u32, plumspirit_engine::editor::ppqn::PpqnMode), String> {
        let to = plumspirit_engine::model::timing::parse_ppqn(&self.to)?;
        Ok((to, self.mode))
    }

    fn preview_is_current(&self) -> bool {
        matches!((&self.preview, self.inputs()), (Some((k, _)), Ok(i)) if *k == i)
    }
}

fn change_ppqn_dialog(
    ui: &mut egui::Ui,
    d: &mut ChangePpqnDialog,
    accept_enter: bool,
    outcome: &mut DialogOutcome,
) {
    use plumspirit_engine::editor::ppqn::PpqnMode;
    ui.label(
        RichText::new("Change PPQN (Experimental)")
            .size(theme::size::EMPHASIS)
            .strong(),
    );
    ui.add_space(space::NORMAL);
    let mut enter = false;
    egui::Grid::new("change-ppqn")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Current PPQN");
            ui.label(plumspirit_engine::midi::group_digits(u64::from(d.from)));
            ui.end_row();
            enter |= field(ui, "New PPQN", &mut d.to, "e.g. 480", !d.focused, !d.busy);
        });
    d.focused = true;
    ui.add_space(space::MICRO);
    ui.add_enabled_ui(!d.busy, |ui| {
        ui.radio_value(&mut d.mode, PpqnMode::Beats, PpqnMode::Beats.label())
            .on_hover_text("Every note, event, tempo change, time signature, marker, clip, loop and view moves to the same bar and beat at the new resolution. Tempos stay, so the song plays the same; positions that fall between new ticks are rounded to the nearest one.");
        ui.radio_value(&mut d.mode, PpqnMode::Seconds, PpqnMode::Seconds.label())
            .on_hover_text("Every tick number stays the same and every tempo is scaled, so every note still plays at the same time in seconds. The bar and beat grid moves: a quarter note is now the new PPQN ticks long.");
    });
    ui.add_space(space::MICRO);
    let current = d.preview_is_current();
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| match (&d.preview, current, d.busy) {
            (_, _, true) => {
                ui.label("Computing…");
            }
            (Some((_, lines)), true, _) => {
                for l in lines {
                    ui.label(l.as_str());
                    ui.add_space(space::MICRO);
                }
            }
            _ => {
                ui.label(
                    RichText::new("Preview shows what the change does before anything changes: rounded positions, notes that would share a tick, merged tempo changes.")
                        .weak(),
                );
            }
        });
    ui.label(
        RichText::new("Experimental: one undo step; Undo restores the project exactly. Saved projects keep the new PPQN.")
            .size(theme::size::SMALL)
            .weak(),
    );
    if let Some(e) = &d.error {
        ui.add_space(space::MICRO);
        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
    } else if !d.busy
        && !d.to.trim().is_empty()
        && let Err(e) = d.inputs()
    {
        ui.add_space(space::MICRO);
        ui.colored_label(ui.visuals().warn_fg_color, e);
    }
    let ok = d.inputs().is_ok() && !d.busy;
    buttons_right(ui, |ui| {
        if ui.button("Cancel").clicked() {
            *outcome = DialogOutcome::Closed;
        }
        if ui.add_enabled(ok, egui::Button::new("Apply")).clicked()
            || (ok && current && accept_enter && enter)
        {
            *outcome = DialogOutcome::PpqnApply;
        }
        if ui.add_enabled(ok, egui::Button::new("Preview")).clicked()
            || (ok && !current && accept_enter && enter)
        {
            *outcome = DialogOutcome::PpqnPreview;
        }
    });
}

/// "☑ Preview" and what Apply would do (one compact row).
fn preview_row(ui: &mut egui::Ui, on: &mut bool, summary: &str, outcome: &mut DialogOutcome) {
    ui.add_space(space::MICRO);
    ui.horizontal_wrapped(|ui| {
        if ui
            .checkbox(on, "Preview")
            .on_hover_text("Shows the result in the piano roll before Apply (nothing changes until you apply). Off: no preview is drawn; Apply works the same.")
            .changed()
        {
            *outcome = DialogOutcome::PreviewToggled(*on);
        }
        if !summary.is_empty() {
            ui.label(RichText::new(summary).size(theme::size::SMALL));
        }
    });
}

/// L-D02: the MIDI files of an archive; one is chosen exactly (click,
/// arrows, double-click / Enter / Open) or none (Cancel / Esc).
fn archive_chooser(
    ui: &mut egui::Ui,
    d: &mut super::archive_open::ArchiveChooser,
    accept_enter: bool,
    outcome: &mut DialogOutcome,
) {
    ui.label(
        RichText::new(format!("Open a MIDI file from “{}”", d.archive))
            .size(theme::size::EMPHASIS)
            .strong(),
    );
    ui.add_space(space::NORMAL);
    ui.label(format!(
        "The archive holds {} MIDI files. Choose the one to open:",
        group_digits(d.rows.len() as u64)
    ));
    ui.add_space(space::MICRO);
    let last = d.rows.len().saturating_sub(1);
    let (down, up) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp)));
    let moved = down || up;
    if down {
        d.selected = (d.selected + 1).min(last);
    }
    if up {
        d.selected = d.selected.saturating_sub(1);
    }
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, row) in d.rows.iter().enumerate() {
                let r = ui.selectable_label(i == d.selected, row.logical.as_str());
                if r.clicked() {
                    d.selected = i;
                }
                if r.double_clicked() {
                    *outcome = DialogOutcome::ArchiveOpen(i);
                }
                if moved && i == d.selected {
                    r.scroll_to_me(None);
                }
                ui.label(RichText::new(row.details()).size(theme::size::SMALL).weak());
                ui.add_space(space::MICRO);
            }
        });
    if !d.notes.is_empty() {
        ui.add_space(space::MICRO);
        ui.collapsing(format!("Skipped entries ({})", d.notes.len()), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("archive-notes")
                .max_height(120.0)
                .show(ui, |ui| {
                    for n in &d.notes {
                        ui.label(RichText::new(n.as_str()).size(theme::size::SMALL));
                    }
                });
        });
    }
    buttons_right(ui, |ui| {
        if ui.button("Cancel").clicked() {
            *outcome = DialogOutcome::Closed;
        }
        if ui.button("Open").clicked() || (accept_enter && ui.input(|i| i.key_pressed(Key::Enter)))
        {
            *outcome = DialogOutcome::ArchiveOpen(d.selected);
        }
    });
}

fn buttons_right(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(space::SECTION);
    // Top-aligned so the row is only as tall as its buttons (a centered cross axis
    // makes the row take the modal's whole remaining height).
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), add);
}

/// A labelled single-line field in a two-column grid row. Returns true when Enter
/// was pressed in it.
fn field(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    focus: bool,
    enabled: bool,
) -> bool {
    ui.label(label);
    let resp = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(text)
            .desired_width(220.0)
            .hint_text(hint),
    );
    if focus {
        resp.request_focus();
    }
    ui.end_row();
    resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
}

/// The clip merge Preview (§048): what Merge will do — timing, destination
/// (a choice for Stacked), subchannel mapping, counts, controller duplicates
/// dropped, memory — then Merge (one undo step) or Cancel (no change).
fn merge_preview(
    ui: &mut egui::Ui,
    d: &mut super::clip_merge_ui::MergeDialog,
    accept_enter: bool,
    outcome: &mut DialogOutcome,
) {
    use plumspirit_engine::editor::clip_merge::MergeKind;
    let p = &d.plan;
    let what = match p.kind {
        MergeKind::Sequence => format!(
            "The {} selected clips on {} become one clip, their timing kept relative to the first.",
            group_digits(p.sources.len() as u64),
            d.dest_name
        ),
        MergeKind::Stack => format!(
            "The {} selected clips on {} tracks become one clip on the destination track.",
            group_digits(p.sources.len() as u64),
            group_digits(p.rows.len() as u64)
        ),
    };
    ui.label(what);
    ui.add_space(space::NORMAL);
    egui::Grid::new("merge_preview")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Timing");
            ui.label(&d.span);
            ui.end_row();
            ui.label("Destination");
            if p.kind == MergeKind::Stack && d.dests.len() > 1 {
                let current = d
                    .dests
                    .iter()
                    .find(|x| x.0 == d.want_dest)
                    .map_or_else(String::new, |x| x.1.clone());
                egui::ComboBox::from_id_salt("merge_dest")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (row, name) in &d.dests {
                            ui.selectable_value(&mut d.want_dest, *row, name);
                        }
                    });
            } else {
                ui.label(&d.dest_name);
            }
            ui.end_row();
            ui.label("Notes");
            ui.label(group_digits(p.notes as u64));
            ui.end_row();
            ui.label("Events");
            ui.label(if p.dropped_events == 0 {
                format!(
                    "{} controller / channel event(s), moved with their clips",
                    group_digits(p.events as u64)
                )
            } else {
                format!(
                    "{} kept; {} dropped where a later clip has the same controller on the same channel at the same tick",
                    group_digits(p.events as u64),
                    group_digits(p.dropped_events as u64)
                )
            });
            ui.end_row();
            ui.label("Memory");
            ui.label(format!(
                "about {} (the undo history keeps it while the step is in it)",
                crate::diagnostics::format_bytes(p.bytes)
            ));
            ui.end_row();
        });
    ui.add_space(space::NORMAL);
    if d.mapping.is_empty() {
        ui.label("Subchannels: unchanged (every note keeps its subchannel).");
    } else {
        ui.label(format!(
            "Subchannels: {} mapped onto {} — the others keep theirs.",
            group_digits(d.mapping.len() as u64),
            d.dest_name
        ));
        egui::ScrollArea::vertical()
            .max_height(140.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for line in &d.mapping {
                    ui.label(RichText::new(line).size(theme::size::SMALL));
                }
            });
    }
    buttons_right(ui, |ui| {
        if ui.button("Cancel").clicked() {
            *outcome = DialogOutcome::Closed;
        }
        let enter = accept_enter && ui.input(|i| i.key_pressed(Key::Enter));
        if ui.button("Merge").clicked() || enter {
            *outcome = DialogOutcome::ClipMergeApply;
        }
    });
}

/// The Track List Paste Repeat dialog (§089, the owner's reference: Number
/// of times to paste, Paste spacing End to end / Even spacing, Paste every
/// value + unit — greyed under End to end) plus Linked / Independent copies
/// and the plan in words (or why the fields make none). OK = one undo step.
fn repeat_dialog(
    ui: &mut egui::Ui,
    d: &mut super::clip_repeat_ui::RepeatDialog,
    accept_enter: bool,
    outcome: &mut DialogOutcome,
) {
    use plumspirit_engine::transform::repeat::{MAX_COPIES, RepeatUnit};
    // Enter while a number or text field is being typed in commits that
    // field only; a second Enter (nothing being typed) is OK (GUI journey
    // M2, V09-S15: typing −2 and Enter in a function's field applied).
    let typing = ui.ctx().egui_wants_keyboard_input();
    egui::Grid::new("paste_repeat")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Number of times to paste:");
            ui.add(crate::ui::stepper::Stepper::new(&mut d.inputs.count).range(1..=MAX_COPIES));
            ui.end_row();
            ui.label("Paste spacing:");
            ui.vertical(|ui| {
                ui.radio_value(&mut d.inputs.even, false, "End to end");
                ui.radio_value(&mut d.inputs.even, true, "Even spacing");
            });
            ui.end_row();
            ui.label("Paste every:");
            ui.add_enabled_ui(d.inputs.even, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.inputs.every).desired_width(80.0));
                    egui::ComboBox::from_id_salt("paste_repeat_unit")
                        .selected_text(RepeatUnit::ALL.get(d.inputs.unit).map_or("", |u| u.label()))
                        .show_ui(ui, |ui| {
                            for (i, u) in RepeatUnit::ALL.iter().enumerate() {
                                ui.selectable_value(&mut d.inputs.unit, i, u.label());
                            }
                        });
                });
            });
            ui.end_row();
            ui.label("Copies:");
            // Function 4 (on) chooses per generation instead.
            let per_generation =
                d.fx.get_raw("a4") == Some(&plumspirit_engine::transform::ParamValue::Bool(true));
            ui.add_enabled_ui(!per_generation, |ui| {
                ui.vertical(|ui| {
                    ui.radio_value(&mut d.linked, true, "Linked (∞: the same notes)");
                    ui.radio_value(&mut d.linked, false, "Independent copies");
                });
            })
            .response
            .on_disabled_hover_text(
                "Linked-vs-Independent Repeat Mode (Advanced) chooses per generation.",
            );
            ui.end_row();
        });
    ui.add_space(space::NORMAL);
    // S15 (§094): the ten clip functions, drawn with the tool windows' rows.
    egui::CollapsingHeader::new("Advanced")
        .id_salt("paste_repeat_advanced")
        .show(ui, |ui| {
            let palette = theme::note_palette(ui.ctx());
            let dark = ui.visuals().dark_mode;
            let colors: Vec<egui::Color32> = d
                .sub_ids
                .iter()
                .map(|&id| theme::subchannel_color(&palette, id, dark))
                .collect();
            let ectx = crate::ui::param_editors::EditorCtx {
                sub_colors: &colors,
                tracks: &[],
                keys: d.keys,
            };
            egui::ScrollArea::vertical()
                .id_salt("paste_repeat_functions")
                .max_height(320.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    super::tools::function_sections(
                        ui,
                        plumspirit_engine::editor::clip_repeat::TOOL_ID,
                        &plumspirit_engine::editor::clip_repeat::FUNCTIONS,
                        super::clip_repeat_ui::param_defs(),
                        &mut d.fx,
                        &ectx,
                    );
                });
        });
    ui.add_space(space::NORMAL);
    match &d.plan {
        Ok(_) => {
            ui.label(&d.summary);
        }
        Err(why) if !why.is_empty() => {
            ui.colored_label(ui.visuals().warn_fg_color, why);
        }
        Err(_) => {}
    }
    let ok = d.plan.is_ok() && !d.stale();
    buttons_right(ui, |ui| {
        if ui.button("Cancel").clicked() {
            *outcome = DialogOutcome::Closed;
        }
        let enter = accept_enter && !typing && ui.input(|i| i.key_pressed(Key::Enter));
        if ui.add_enabled(ok, egui::Button::new("OK")).clicked() || (ok && enter) {
            *outcome = DialogOutcome::ClipRepeatApply;
        }
    });
}

pub fn show(dialog: &mut Dialog, ctx: &egui::Context, accept_enter: bool) -> DialogOutcome {
    let mut outcome = DialogOutcome::Open;
    let error_line = |ui: &mut egui::Ui, error: &Option<String>| {
        if let Some(e) = error {
            ui.add_space(space::MICRO);
            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
        }
    };
    // Dialogs with a live Preview sit at the top right with a light backdrop, so
    // the piano roll (and the preview in it) stays visible.
    let previewing = matches!(dialog, Dialog::Scale { preview: true, .. })
        || matches!(dialog, Dialog::ImportIntoTrack(d) if d.preview)
        || matches!(dialog, Dialog::TrackProperties(d, _) if d.preview)
        || matches!(dialog, Dialog::MergeClips(_) | Dialog::RepeatClips(_));
    // v0.7 (contract §123): a dialog with a live Preview is a movable,
    // modeless window without a backdrop — the Piano Roll stays visible and
    // can be panned and zoomed while previewing (editing stays off while a
    // dialog is open, and Apply refuses a changed document as stale). Its
    // title bar is the handle to move it.
    // A window's title bar already names it.
    let heading = |ui: &mut egui::Ui, text: &str| {
        if !previewing {
            ui.label(RichText::new(text).size(theme::size::EMPHASIS).strong());
            ui.add_space(space::NORMAL);
        }
    };
    let window_title = match &*dialog {
        Dialog::Scale { axis, .. } => axis.title(),
        Dialog::ImportIntoTrack(_) => "Import Into Track",
        Dialog::TrackProperties(..) => "Track Properties",
        Dialog::MergeClips(d) => d.plan.kind.label(),
        Dialog::RepeatClips(_) => "Paste Repeat",
        _ => "PlumSpirit",
    };
    let mut body = |ui: &mut egui::Ui| {
        ui.set_min_width(340.0);
        ui.set_max_width(480.0);
        match dialog {
            Dialog::MergeClips(d) => {
                merge_preview(ui, d, accept_enter, &mut outcome);
                error_line(ui, &d.error);
            }
            Dialog::ChangePpqn(d) => change_ppqn_dialog(ui, d, accept_enter, &mut outcome),
            Dialog::RepeatClips(d) => {
                repeat_dialog(ui, d, accept_enter, &mut outcome);
                error_line(ui, &d.error);
            }
            Dialog::Dirty {
                name,
                then,
                asks_path,
                note,
            } => {
                heading(ui, &format!("Save changes to “{name}”?"));
                ui.label(*note);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Don't Save").clicked() {
                        outcome = DialogOutcome::DirtyDiscard(then.clone());
                    }
                    if ui
                        .button(if *asks_path { "Save…" } else { "Save" })
                        .clicked()
                    {
                        outcome = DialogOutcome::DirtySave(then.clone());
                    }
                });
            }
            Dialog::DeleteTrack {
                roots,
                name,
                notes,
                events,
                subtracks,
                folder,
            } => {
                // v0.8: wording for a folder and for several tracks (it
                // said "Delete “2 tracks”? This removes the track …").
                let with_subs = if *subtracks > 0 {
                    format!(
                        " with their {} subtrack(s)",
                        group_digits(*subtracks as u64)
                    )
                } else {
                    String::new()
                };
                if *folder != 0 {
                    heading(ui, &format!("Delete {name} and its tracks?"));
                    ui.label(format!(
                        "This removes the folder, its {} track(s){with_subs} and everything in them: {} note(s) and {} other MIDI event(s). You can undo this with {undo}.",
                        group_digits(roots.len() as u64),
                        group_digits(*notes as u64),
                        group_digits(*events as u64),
                        undo = super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                    ));
                } else if roots.len() > 1 {
                    heading(
                        ui,
                        &format!("Delete {} tracks?", group_digits(roots.len() as u64)),
                    );
                    ui.label(format!(
                        "This removes the {} selected tracks{with_subs} and everything in them: {} note(s) and {} other MIDI event(s). You can undo this with {undo}.",
                        group_digits(roots.len() as u64),
                        group_digits(*notes as u64),
                        group_digits(*events as u64),
                        undo = super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                    ));
                } else if *subtracks > 0 {
                    heading(
                        ui,
                        &format!(
                            "Delete “{name}” and its {} subtrack(s)?",
                            group_digits(*subtracks as u64)
                        ),
                    );
                    ui.label(format!(
                        "This removes the track, all of its subtracks and everything in them: {} note(s) and {} other MIDI event(s). You can undo this with {undo}.",
                        group_digits(*notes as u64),
                        group_digits(*events as u64),
                        undo = super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                    ));
                } else {
                    heading(ui, &format!("Delete “{name}”?"));
                    ui.label(format!(
                        "This removes the track and everything in it: {} note(s) and {} other MIDI event(s). You can undo this with {undo}.",
                        group_digits(*notes as u64),
                        group_digits(*events as u64),
                        undo = super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                    ));
                }
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    let label = if *folder != 0 {
                        "Delete Folder and Tracks"
                    } else if roots.len() > 1 {
                        "Delete Tracks"
                    } else {
                        "Delete Track"
                    };
                    if ui.button(label).clicked() {
                        outcome = DialogOutcome::DeleteTrackConfirmed(roots.clone(), *folder);
                    }
                });
            }
            Dialog::Scale {
                axis,
                text,
                error,
                focused,
                preview,
                summary,
                fill_on,
                fill,
                prevent,
                cleanup,
            } => {
                let opts = plumspirit_engine::editor::ScaleY {
                    fill: (*axis == ScaleAxis::Y && *fill_on).then_some(*fill),
                    prevent_overlap: *axis == ScaleAxis::Y && *prevent,
                    cleanup: *cleanup,
                };
                heading(ui, axis.title());
                ui.label(match axis {
                    ScaleAxis::X => {
                        "Scales the selected notes in time around the earliest selected note."
                    }
                    ScaleAxis::Y => {
                        "Scales the selected pitches around the center of the selected pitch range."
                    }
                });
                ui.add_space(space::NORMAL);
                ui.horizontal(|ui| {
                    ui.label("Factor");
                    let resp = ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(160.0)
                            .hint_text("e.g. 2, 0.5, -1, 1e3"),
                    );
                    if !*focused {
                        resp.request_focus();
                        *focused = true;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        outcome = DialogOutcome::ScaleApply {
                            axis: *axis,
                            text: text.clone(),
                            opts,
                        };
                    }
                });
                if *axis == ScaleAxis::Y {
                    ui.add_space(space::MICRO);
                    scale_y_options(ui, fill_on, fill, prevent, cleanup);
                }
                let opts = plumspirit_engine::editor::ScaleY {
                    fill: (*axis == ScaleAxis::Y && *fill_on).then_some(*fill),
                    prevent_overlap: *axis == ScaleAxis::Y && *prevent,
                    cleanup: *cleanup,
                };
                ui.label(
                    RichText::new("Any decimal is accepted; negative values mirror. Results outside the MIDI range are refused, never clipped.")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                preview_row(ui, preview, summary, &mut outcome);
                error_line(ui, error);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Apply").clicked() {
                        outcome = DialogOutcome::ScaleApply {
                            axis: *axis,
                            text: text.clone(),
                            opts,
                        };
                    }
                });
            }
            Dialog::Message {
                title,
                body,
                is_error,
            } => {
                heading(ui, title);
                if *is_error {
                    ui.colored_label(ui.visuals().error_fg_color, body.as_str());
                } else {
                    ui.label(body.as_str());
                }
                buttons_right(ui, |ui| {
                    if ui.button("OK").clicked()
                        || (accept_enter && ui.input(|i| i.key_pressed(Key::Enter)))
                    {
                        outcome = DialogOutcome::Closed;
                    }
                });
            }
            Dialog::ArchiveChooser(d) => archive_chooser(ui, d, accept_enter, &mut outcome),
            Dialog::OpenCompatible {
                path,
                message,
                keys,
            } => {
                heading(ui, "MIDI file outside the MIDI specification");
                ui.label(message.as_str());
                ui.add_space(space::MICRO);
                ui.label(
                    RichText::new("Compatibility Mode repairs only known, recoverable deviations — delta-times or lengths longer than 4 bytes (read exactly), an event cut at the end of its track or a file cut inside a track (the incomplete event is dropped) — and lists every repair with its track and byte in the status Details. The file itself is not changed.")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Open in Compatibility Mode").clicked()
                        || (accept_enter && ui.input(|i| i.key_pressed(Key::Enter)))
                    {
                        outcome = DialogOutcome::OpenCompatible(path.clone(), *keys);
                    }
                });
            }
            Dialog::OpenWideKeys {
                path,
                message,
                compat,
            } => {
                heading(ui, "Non-standard 256-key MIDI file");
                ui.label(message.as_str());
                ui.add_space(space::MICRO);
                ui.label(
                    RichText::new("Opened as a 256-Key Mode project, each such note keeps its key (128–255). Exporting it as Standard MIDI later asks what happens to the keys above 127.")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Open as 256-Key Mode Project").clicked() {
                        outcome = DialogOutcome::OpenWideKeys(path.clone(), *compat);
                    }
                });
            }
            Dialog::ConfirmOverwrite { path, purpose } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                heading(ui, &format!("Replace “{name}”?"));
                ui.label(match purpose {
                    OverwriteFor::Export => "A file with this name already exists. It is replaced only after the new MIDI has been written and validated.",
                    OverwriteFor::Save(_) => "A file with this name already exists. It is replaced only after the project has been written and verified.",
                });
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Replace").clicked() {
                        outcome = DialogOutcome::OverwriteConfirmed {
                            path: path.clone(),
                            purpose: purpose.clone(),
                        };
                    }
                });
            }
            Dialog::PathPrompt {
                purpose,
                text,
                focused,
            } => {
                heading(ui, purpose.title());
                ui.label("Path of the file:");
                let resp = ui.add(egui::TextEdit::singleline(text).desired_width(440.0));
                if !*focused {
                    resp.request_focus();
                    *focused = true;
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if (ui.button("OK").clicked() || enter) && !text.trim().is_empty() {
                        outcome = DialogOutcome::PathEntered {
                            purpose: purpose.clone(),
                            path: PathBuf::from(text.trim()),
                        };
                    }
                });
            }
            Dialog::NewProject {
                name,
                bpm,
                ppqn,
                keys256,
                error,
                focused,
            } => {
                heading(ui, "New Project");
                let mut enter = false;
                egui::Grid::new("new-project")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(ui, "Project Name", name, "Untitled", !*focused, true);
                        enter |= field(ui, "BPM", bpm, "e.g. 120, 174.5, 1e6", false, true);
                        enter |= field(ui, "PPQN", ppqn, "e.g. 960", false, true);
                        ui.label("Keys");
                        ui.checkbox(keys256, "256-Key Mode (keys 0–255)")
                            .on_hover_text("Off: the standard MIDI keys 0–127. On: the project stores keys 0–255 (the Piano Roll shows 256 rows); keys above 127 are not Standard MIDI notes. Project Properties can change it later. The default is in Settings → Import / Export.");
                        ui.end_row();
                    });
                *focused = true;
                ui.label(
                    RichText::new("Starts with one empty track in 4/4. BPM: 3.5762788 – 60,000,000 (what a MIDI file can store), kept exactly. PPQN: 1 – 32,767 is standard MIDI; up to 2,147,483,647 is kept in the project and MIDI export rescales it to at most 32,767 (said before it writes).")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                error_line(ui, error);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Create").clicked() || enter {
                        outcome = DialogOutcome::NewProjectCreate {
                            name: name.clone(),
                            bpm: bpm.clone(),
                            ppqn: ppqn.clone(),
                            keys256: *keys256,
                        };
                    }
                });
            }
            Dialog::ProjectProperties {
                name,
                bpm,
                ppqn,
                keys256,
                error,
                focused,
            } => {
                heading(ui, "Project Properties");
                let mut enter = false;
                egui::Grid::new("project-props")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(ui, "Project Name", name, "Untitled", !*focused, true);
                        enter |= field(ui, "BPM", bpm, "e.g. 120", false, true);
                        ui.label("PPQN");
                        ui.horizontal(|ui| {
                            ui.add_enabled(
                                false,
                                egui::TextEdit::singleline(ppqn).desired_width(110.0),
                            );
                            if ui
                                .button("Change PPQN (Experimental)…")
                                .on_hover_text("Rescales the whole project to another resolution (ticks per quarter note), with a Preview of what is rounded. One undo step.")
                                .clicked()
                            {
                                outcome = DialogOutcome::OpenChangePpqn;
                            }
                        });
                        ui.end_row();
                        ui.label("Keys");
                        ui.checkbox(keys256, "256-Key Mode (keys 0–255)")
                            .on_hover_text("Off: the standard MIDI keys 0–127. On: the project really stores keys 0–255 (the Piano Roll shows 256 rows). Keys above 127 are not Standard MIDI notes. It can be turned off again only while no note or track setting uses keys above 127. Undo restores the previous mode.");
                        ui.end_row();
                    });
                *focused = true;
                ui.label(
                    RichText::new("The BPM is the tempo at the start of the song; later tempo changes are kept. Change PPQN rescales the project to another resolution.")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                error_line(ui, error);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::PropertiesApply {
                            name: name.clone(),
                            bpm: bpm.clone(),
                            keys256: *keys256,
                        };
                    }
                });
            }
            Dialog::ExportApproval {
                lines,
                corrections,
                blocked,
                structure,
                wide,
            } => {
                heading(
                    ui,
                    if corrections.is_some() || wide.is_some() {
                        "Export MIDI"
                    } else {
                        "Export with conversion?"
                    },
                );
                if let Some(w) = wide.as_ref() {
                    ui.label(w.summary());
                    let mut policy = w.policy;
                    for p in WideKeys::ALL {
                        let text = if p.writes_wide() {
                            RichText::new(p.label()).color(ui.visuals().warn_fg_color)
                        } else {
                            RichText::new(p.label())
                        };
                        ui.radio_value(&mut policy, p, text)
                            .on_hover_text(ExportWideKeys::tip(p));
                    }
                    if policy != w.policy {
                        outcome = DialogOutcome::ExportWideKeys(policy);
                    }
                    ui.add_space(space::MICRO);
                }
                if let Some(apply) = corrections {
                    let mut a = *apply;
                    if ui
                        .checkbox(&mut a, "Apply Track Properties corrections (the file plays like PlumSpirit)")
                        .on_hover_text("Off: the original notes are exported and the corrections are left out. The project keeps both in either case; corrections are never applied twice.")
                        .changed()
                    {
                        outcome = DialogOutcome::ExportCorrections(a);
                    }
                    ui.add_space(space::MICRO);
                }
                if let Some(st) = structure.as_mut() {
                    let (mut split, mut metadata) = (st.split, st.metadata);
                    let mut changed = false;
                    if st.has_shared_channels {
                        changed |= ui
                            .checkbox(&mut split, "Keep subchannels that share a MIDI channel in separate MIDI tracks")
                            .on_hover_text("Standard MIDI has 16 channels. On: other programs can tell those subchannels apart too (extra MIDI tracks). Off: they share their track's MIDI track. No note is lost either way.")
                            .changed();
                    }
                    changed |= ui
                        .checkbox(&mut metadata, "Add PlumSpirit data that restores subtracks and subchannels")
                        .on_hover_text("Other programs ignore it; opening the file in PlumSpirit restores the track tree and the subchannels exactly.")
                        .changed();
                    if changed {
                        outcome = DialogOutcome::ExportStructure { split, metadata };
                    }
                    ui.add_space(space::MICRO);
                }
                // Fixed height: toggling an option changes the text below, and a
                // dialog that resized would move the checkboxes under the pointer.
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .min_scrolled_height(300.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(st) = structure.as_ref() {
                            for l in &st.lines {
                                ui.label(l.as_str());
                                ui.add_space(space::MICRO);
                            }
                        }
                        for l in lines.iter() {
                            ui.label(l.as_str());
                            ui.add_space(space::MICRO);
                        }
                    });
                ui.label(
                    RichText::new("Your project keeps its exact values; only the exported MIDI file is converted.")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                let blocked = *blocked;
                let export = wide.as_ref().map_or("Export…", ExportWideKeys::button);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui
                        .add_enabled(!blocked, egui::Button::new(export))
                        .on_disabled_hover_text("This cannot be written as described above; change the option or the project first.")
                        .clicked()
                    {
                        outcome = DialogOutcome::ExportApproved;
                    }
                });
            }
            Dialog::Marker {
                id,
                title,
                chapter,
                start,
                end,
                by_duration,
                hint,
                error,
                focused,
            } => {
                heading(
                    ui,
                    if id.is_some() {
                        "Edit Marker"
                    } else {
                        "Add Marker"
                    },
                );
                let mut enter = false;
                egui::Grid::new("marker")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(
                            ui,
                            "Title",
                            title,
                            "e.g. Intro, Verse, Drop",
                            !*focused,
                            true,
                        );
                        ui.label("Type");
                        ui.horizontal(|ui| {
                            ui.radio_value(chapter, false, "Point")
                                .on_hover_text("One position with a label.");
                            ui.radio_value(chapter, true, "Chapter")
                                .on_hover_text("A labeled span from Start to End.");
                        });
                        ui.end_row();
                        enter |= field(ui, "Start (ticks)", start, "0", false, true);
                        if *chapter {
                            ui.label(if *by_duration {
                                "Duration (ticks)"
                            } else {
                                "End (ticks)"
                            });
                            ui.horizontal(|ui| {
                                let resp =
                                    ui.add(egui::TextEdit::singleline(end).desired_width(120.0));
                                enter |=
                                    resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                                let before = *by_duration;
                                ui.checkbox(by_duration, "as duration");
                                // Switching keeps the same span.
                                if before != *by_duration
                                    && let (Ok(s), Ok(v)) =
                                        (parse_tick(start, "start"), parse_tick(end, "end"))
                                {
                                    let other = if *by_duration {
                                        v.checked_sub(s)
                                    } else {
                                        s.checked_add(v)
                                    };
                                    if let Some(o) = other {
                                        *end = o.to_string();
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
                *focused = true;
                if !hint.is_empty() {
                    ui.label(RichText::new(hint.as_str()).size(theme::size::SMALL).weak());
                }
                error_line(ui, error);
                let editing = id.is_some();
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                    if editing && ui.button("Delete").clicked() {
                        outcome = DialogOutcome::Delete;
                    }
                });
            }
            Dialog::TimeSignature {
                id,
                tick,
                numerator,
                denominator,
                first,
                hint,
                error,
                focused,
            } => {
                heading(
                    ui,
                    if id.is_some() {
                        "Edit Time Signature"
                    } else {
                        "Add Time Signature"
                    },
                );
                let mut enter = false;
                egui::Grid::new("signature")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(ui, "Beats per bar", numerator, "4", !*focused, true);
                        ui.label("Beat note");
                        ui.horizontal(|ui| {
                            let current = denominator.trim().to_owned();
                            egui::ComboBox::from_id_salt("sig-denominator")
                                .width(64.0)
                                .selected_text(if current.is_empty() {
                                    "4".to_owned()
                                } else {
                                    current.clone()
                                })
                                .show_ui(ui, |ui| {
                                    for d in [1u32, 2, 4, 8, 16, 32, 64, 128] {
                                        if ui
                                            .selectable_label(
                                                current == d.to_string(),
                                                d.to_string(),
                                            )
                                            .clicked()
                                        {
                                            *denominator = d.to_string();
                                        }
                                    }
                                });
                            ui.label(RichText::new("(1, 2, 4 … 128)").weak());
                        });
                        ui.end_row();
                        enter |= field(ui, "At (ticks)", tick, "0", false, !*first);
                    });
                *focused = true;
                ui.label(
                    RichText::new("A change always starts a new bar; a bar it interrupts ends early. Bars keep counting from the start of the song.")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                if !hint.is_empty() {
                    ui.label(RichText::new(hint.as_str()).size(theme::size::SMALL).weak());
                }
                error_line(ui, error);
                let can_delete = id.is_some() && !*first;
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                    if can_delete && ui.button("Delete").clicked() {
                        outcome = DialogOutcome::Delete;
                    }
                });
            }
            Dialog::Tempo {
                id,
                tick,
                bpm,
                linear,
                first,
                hint,
                error,
                focused,
            } => {
                heading(
                    ui,
                    if id.is_some() {
                        "Edit Tempo"
                    } else {
                        "Add Tempo Change"
                    },
                );
                let mut enter = false;
                egui::Grid::new("tempo")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(ui, "BPM", bpm, "e.g. 120, 174.5, 1e6", !*focused, true);
                        enter |= field(ui, "At (ticks)", tick, "0", false, !*first);
                        ui.label("Until the next point");
                        ui.vertical(|ui| {
                            ui.radio_value(linear, false, "Step: hold this tempo");
                            ui.radio_value(linear, true, "Linear: ramp to the next point's tempo");
                        });
                        ui.end_row();
                    });
                *focused = true;
                if !hint.is_empty() {
                    ui.label(RichText::new(hint.as_str()).size(theme::size::SMALL).weak());
                }
                error_line(ui, error);
                let can_delete = id.is_some() && !*first;
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                    if can_delete && ui.button("Delete").clicked() {
                        outcome = DialogOutcome::Delete;
                    }
                });
            }
            Dialog::Controller {
                kind,
                channel,
                index,
                tick,
                value,
                error,
                focused,
            } => {
                let verb = if index.is_some() { "Edit" } else { "Add" };
                heading(ui, &format!("{verb} {} Event", kind.label()));
                let hint = kind.range_text();
                let mut enter = false;
                egui::Grid::new("controller")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(ui, "Value", value, hint, !*focused, true);
                        enter |= field(ui, "At (ticks)", tick, "0", false, true);
                        ui.label("Channel");
                        ui.label(format!("{}", *channel + 1));
                        ui.end_row();
                    });
                *focused = true;
                let about = match *kind {
                    ControllerKind::PitchBend => "Pitch Bend is a 14-bit MIDI message; how far it bends depends on the instrument's bend range.".to_owned(),
                    ControllerKind::Cc(n) => {
                        let mut t = format!("{} is MIDI Control Change {n}.", kind.label());
                        if !controllers::internal_synth_responds(*kind) {
                            t.push_str(" The built-in synthesizer ignores it (an external device may use it); it is saved and exported.");
                        }
                        t
                    }
                };
                ui.label(RichText::new(about).size(theme::size::SMALL).weak());
                if let Some(c) = kind.caution() {
                    ui.label(
                        RichText::new(c)
                            .size(theme::size::SMALL)
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                error_line(ui, error);
                let editing = index.is_some();
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                    if editing && ui.button("Delete").clicked() {
                        outcome = DialogOutcome::Delete;
                    }
                });
            }
            Dialog::MasterVolume {
                id,
                tick,
                value,
                error,
                focused,
            } => {
                let verb = if id.is_some() { "Edit" } else { "Add" };
                heading(ui, &format!("{verb} Master Volume"));
                let mut enter = false;
                egui::Grid::new("master")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        enter |= field(
                            ui,
                            "Level",
                            value,
                            "0% … 100% (or #0 … #16383)",
                            !*focused,
                            true,
                        );
                        enter |= field(ui, "At (ticks)", tick, "0", false, true);
                    });
                *focused = true;
                ui.label(
                    RichText::new("Master Volume is global (Conductor): it scales the whole output from this point on. It is exported as a Universal Master Volume SysEx message (F0 7F 7F 04 01 … F7).")
                        .size(theme::size::SMALL)
                        .weak(),
                );
                error_line(ui, error);
                let editing = id.is_some();
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                    if editing && ui.button("Delete").clicked() {
                        outcome = DialogOutcome::Delete;
                    }
                });
            }
            Dialog::TrackProperties(draft, meter) => {
                let room = (ui.ctx().content_rect().width() - 32.0).max(200.0);
                ui.set_min_width(room.min(460.0));
                ui.set_max_width(room.min(560.0));
                heading(ui, &format!("Track Properties — Track {}", draft.index + 1));
                // Small windows (960 × 600): the body scrolls, the buttons stay.
                let max_h = (ui.ctx().content_rect().height() - 150.0).max(200.0);
                let enter = egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .show(ui, |ui| track_properties_ui(ui, draft, meter.as_ref()))
                    .inner;
                preview_row(ui, &mut draft.preview, &draft.preview_text, &mut outcome);
                error_line(ui, &draft.error);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                });
            }
            Dialog::SelectRange(draft, musical) => {
                ui.set_min_width(420.0);
                ui.set_max_width(520.0);
                heading(ui, "Select Range");
                let max_h = (ui.ctx().content_rect().height() - 150.0).max(200.0);
                let enter = egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .show(ui, |ui| super::timing::select_range_ui(ui, draft, *musical))
                    .inner;
                error_line(ui, &draft.error);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Select").clicked() || enter {
                        outcome = DialogOutcome::Apply;
                    }
                });
            }
            Dialog::PlayingTime {
                draft,
                lines,
                converted,
                speed,
            } => {
                ui.set_min_width(520.0);
                ui.set_max_width(640.0);
                heading(ui, "Playing Time");
                let conv = converted.clone();
                if let Some(t) =
                    super::timing::playing_time_ui(ui, draft, lines, *speed, &|_| conv.clone())
                {
                    outcome = DialogOutcome::GoTo(t);
                }
                buttons_right(ui, |ui| {
                    if ui.button("Close").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                });
            }
            Dialog::CurveFunctions(draft) => {
                ui.set_min_width(460.0);
                ui.set_max_width(560.0);
                heading(ui, "Custom Functions");
                functions_ui(ui, draft, &theme::ui_palette(ui.ctx()));
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui
                        .button("Apply")
                        .on_hover_text(
                            "Save the list and use the selected function with the f(t) tool",
                        )
                        .clicked()
                    {
                        outcome = DialogOutcome::Apply;
                    }
                });
            }
            Dialog::AddMany {
                what,
                target,
                count,
                error,
                focused,
            } => {
                let (title, text, unit) = match what {
                    AddManyKind::Subtracks(_) => (
                        "Add Subtracks",
                        format!("Adds empty subtracks under “{target}”, in one step (one Undo)."),
                        "subtrack(s)",
                    ),
                    AddManyKind::Subchannels => (
                        "Add Subchannels",
                        format!(
                            "Adds subchannels to “{target}”, numbered after the highest one, in one step (one Undo). Each gets its own color and the active subchannel's MIDI channel (change it per subchannel in Song → Subchannels…)."
                        ),
                        "subchannel(s)",
                    ),
                };
                heading(ui, title);
                ui.label(text);
                ui.add_space(space::NORMAL);
                let mut enter = false;
                ui.horizontal(|ui| {
                    ui.label("Count");
                    let r = ui.add(
                        egui::TextEdit::singleline(count)
                            .desired_width(100.0)
                            .hint_text("e.g. 10, 1000"),
                    );
                    if !*focused {
                        r.request_focus();
                        *focused = true;
                    }
                    enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.label(unit);
                });
                error_line(ui, error);
                let mut apply = enter;
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    apply |= ui.button("Add").clicked();
                });
                if apply {
                    match count.trim().replace([',', '_', ' '], "").parse::<usize>() {
                        Ok(n) if n >= 1 => outcome = DialogOutcome::AddMany(*what, n),
                        _ => *error = Some("Enter a whole number of 1 or more.".into()),
                    }
                }
            }
            Dialog::AddTracks(d) => {
                heading(ui, "Add Tracks");
                ui.label(format!(
                    "Adds empty tracks {}, in one step (one Undo). They have no clips yet: add a clip to edit notes in the Piano Roll.",
                    d.place
                ));
                ui.add_space(space::NORMAL);
                let mut enter = false;
                ui.horizontal(|ui| {
                    ui.label("Count");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut d.count)
                            .desired_width(100.0)
                            .hint_text("e.g. 16, 100"),
                    );
                    if !d.focused {
                        r.request_focus();
                        d.focused = true;
                    }
                    if r.changed() {
                        d.error = None;
                        d.confirm = None;
                    }
                    enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.label("track(s)");
                });
                let check = add_tracks_preflight(&d.count, d.existing, d.available);
                ui.add_space(space::MICRO);
                match &check {
                    Ok((_, line)) => {
                        ui.label(RichText::new(line).weak());
                    }
                    Err(e) if !d.count.trim().is_empty() => {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    Err(_) => {}
                }
                let pending = d.pending();
                if let Some(n) = pending {
                    ui.add_space(space::MICRO);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Add {} tracks? Nothing changes until you press Add {} Tracks; Cancel leaves the project as it is.",
                            group_digits(n as u64),
                            group_digits(n as u64)
                        ),
                    );
                }
                error_line(ui, &d.error);
                let mut apply = enter && accept_enter;
                let label = match pending {
                    Some(n) => format!("Add {} Tracks", group_digits(n as u64)),
                    None => "Add".to_owned(),
                };
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    apply |= ui.button(label).clicked();
                });
                if apply && let Some((at, n)) = d.press() {
                    outcome = DialogOutcome::AddTracks(at, n);
                }
            }
            Dialog::Percent {
                what,
                text,
                error,
                focused,
            } => {
                let (title, body) = match what {
                    PercentKind::OnionOpacity => (
                        "Ghost Opacity",
                        "How opaque Onion Skin ghost notes are: 0 % (invisible) to 100 % (fully opaque). Ghost notes are never editable, even at 100 %.",
                    ),
                    PercentKind::SelectionOpacity => (
                        "Selection Opacity",
                        "How strongly selected notes are highlighted: 0 % (they look like the other notes) to 100 % (the full selection color and border, the default). Kept for every project; Ghost Opacity is a separate setting.",
                    ),
                    PercentKind::FollowPosition => (
                        "Follow Position",
                        "While playing with Follow on, the view scrolls so the playhead stays at this point of the piano roll's width: 0 % = left edge, 50 % = center (default), 100 % = right edge.",
                    ),
                };
                heading(ui, title);
                ui.label(body);
                ui.add_space(space::NORMAL);
                let mut value = whole_percent(text).unwrap_or(50);
                let mut enter = false;
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Slider::new(&mut value, 0..=100).show_value(false))
                        .changed()
                    {
                        *text = value.to_string();
                    }
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(48.0));
                    if !*focused {
                        r.request_focus();
                        *focused = true;
                    }
                    enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.label("%");
                });
                error_line(ui, error);
                let mut apply = enter;
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    apply |= ui.button("OK").clicked();
                });
                if apply {
                    match whole_percent(text) {
                        Ok(v) => outcome = DialogOutcome::Percent(*what, v),
                        Err(e) => *error = Some(e),
                    }
                }
            }
            Dialog::CreateSubchannel { track, id, channel } => {
                heading(ui, &format!("Create Subchannel {}?", id.number()));
                ui.label(format!(
                    "This track has no Subchannel {}. Create it now (MIDI channel {}) and make it the active subchannel?",
                    id.number(),
                    u16::from(*channel) + 1
                ));
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Create").clicked()
                        || (accept_enter && ui.input(|i| i.key_pressed(Key::Enter)))
                    {
                        outcome = DialogOutcome::CreateSubchannel {
                            track: *track,
                            id: *id,
                            channel: *channel,
                        };
                    }
                });
            }
            Dialog::EditSubchannel {
                track,
                id,
                name,
                channel,
                move_notes,
                error,
            } => {
                heading(ui, &format!("Subchannel {}", id.number()));
                ui.label("The subchannel number and color are PlumSpirit's own. The MIDI channel is the real channel (1–16) its notes play and export on.");
                ui.add_space(space::NORMAL);
                let mut ch = u16::from(*channel) + 1;
                egui::Grid::new("sub-edit")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(name)
                                .desired_width(200.0)
                                .hint_text(format!("Subchannel {}", id.number())),
                        );
                        ui.end_row();
                        ui.label("MIDI channel");
                        ui.add(crate::ui::stepper::Stepper::new(&mut ch).range(1..=16));
                        ui.end_row();
                    });
                *channel = (ch.clamp(1, 16) - 1) as u8;
                ui.checkbox(
                    move_notes,
                    "Also move this subchannel's existing notes to that MIDI channel",
                )
                .on_hover_text(
                    "Off: only new notes use the new channel; existing notes keep theirs.",
                );
                error_line(ui, error);
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("OK").clicked() {
                        outcome = DialogOutcome::EditSubchannel {
                            track: *track,
                            id: *id,
                            name: name.clone(),
                            channel: *channel,
                            move_notes: *move_notes,
                        };
                    }
                });
            }
            Dialog::DeleteSubchannel {
                track,
                id,
                name,
                notes,
            } => {
                heading(ui, &format!("Delete “{name}”?"));
                ui.label(format!(
                    "Subchannel {} contains {} note(s); they are deleted with it. Controller events stay (they belong to the track's MIDI channels). You can undo this with {undo}.",
                    id.number(),
                    group_digits(*notes as u64),
                        undo = super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                ));
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Delete Subchannel").clicked() {
                        outcome = DialogOutcome::DeleteSubchannel {
                            track: *track,
                            id: *id,
                        };
                    }
                });
            }
            Dialog::MoveToSubchannel {
                number,
                set_channel,
                error,
                focused,
            } => {
                heading(ui, "Move Selected Notes to Subchannel");
                let mut enter = false;
                ui.horizontal(|ui| {
                    ui.label("Subchannel");
                    let r = ui.add(
                        egui::TextEdit::singleline(number)
                            .desired_width(90.0)
                            .hint_text("number"),
                    );
                    if !*focused {
                        r.request_focus();
                        *focused = true;
                    }
                    enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                });
                ui.checkbox(
                    set_channel,
                    "Also give the notes that subchannel's MIDI channel",
                );
                error_line(ui, error);
                let mut apply = enter;
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    apply |= ui.button("Move").clicked();
                });
                if apply {
                    outcome = DialogOutcome::MoveToSubchannel {
                        number: number.clone(),
                        set_channel: *set_channel,
                    };
                }
            }
            Dialog::ImportIntoTrack(d) => {
                ui.set_min_width(420.0);
                heading(ui, "Import MIDI Into Current Track");
                ui.label(format!("“{}” into “{}”.", d.file, d.dest));
                ui.add_space(space::NORMAL);
                egui::Grid::new("merge").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Place source tracks");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut d.mode, MergeMode::AsSubtracks, "As subtracks of this track")
                            .on_hover_text("Each MIDI track becomes a subtrack (with one subchannel per MIDI channel it uses).");
                        ui.radio_value(&mut d.mode, MergeMode::AsSubchannels, "As subchannels in this track")
                            .on_hover_text("Everything goes into this track; each source track and MIDI channel gets its own subchannel. MIDI channels stay as they are.");
                    });
                    ui.end_row();
                    ui.label("Tempo and markers");
                    ui.vertical(|ui| {
                        for p in ConductorPolicy::ALL {
                            ui.radio_value(&mut d.policy, p, p.label()).on_hover_text(p.describe());
                        }
                    });
                    ui.end_row();
                    ui.label("Insert at");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut d.at_cursor, false, "Project start");
                        ui.radio_value(&mut d.at_cursor, true, format!("Current position ({})", d.cursor_text));
                    });
                    ui.end_row();
                });
                ui.add_space(space::MICRO);
                if ui.checkbox(&mut d.preview, "Preview").on_hover_text("Shows the notes that will be added in the piano roll (highlighted). Nothing changes until Apply.").changed() {
                    outcome = DialogOutcome::PreviewToggled(d.preview);
                }
                ui.add_space(space::MICRO);
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for l in &d.lines {
                            ui.label(RichText::new(l.as_str()).size(theme::size::SMALL));
                        }
                        if d.busy {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("Preparing…").size(theme::size::SMALL));
                            });
                        }
                    });
                error_line(ui, &d.error);
                let can = !d.busy && d.error.is_none();
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.add_enabled(can, egui::Button::new("Apply")).clicked() {
                        outcome = DialogOutcome::MergeApply;
                    }
                });
            }
            Dialog::TooLargeForUndo { label, message } => {
                heading(ui, &format!("Apply “{label}” without undo?"));
                ui.label(message.as_str());
                ui.add_space(space::MICRO);
                ui.label(
                    "If you continue, this edit cannot be undone and the undo history is cleared.",
                );
                buttons_right(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Closed;
                    }
                    if ui.button("Apply Without Undo").clicked() {
                        outcome = DialogOutcome::ApplyWithoutUndo;
                    }
                });
            }
        }
    };
    let should_close = if previewing {
        let mut open = true;
        egui::Window::new(window_title)
            .id(Id::new("plumspirit-preview-dialog"))
            .movable(true)
            .resizable(false)
            .collapsible(false)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-500.0, 96.0))
            .open(&mut open)
            .show(ctx, |ui| body(ui));
        !open || ctx.input(|i| i.key_pressed(Key::Escape))
    } else {
        Modal::new(Id::new("plumspirit-dialog"))
            .show(ctx, |ui| body(ui))
            .should_close()
    };
    if should_close && outcome == DialogOutcome::Open {
        outcome = DialogOutcome::Closed;
    }
    outcome
}

/// B1-0115 / B1-0116: Scale Y's three modes (Transform existing notes, Fill
/// added rows, Copy by pitch step), their Duplicate Notes options and the
/// Scale Y overlap prevention. `fill_on` = Duplicate Notes (off: Transform).
pub(crate) fn scale_y_options(
    ui: &mut egui::Ui,
    fill_on: &mut bool,
    fill: &mut plumspirit_engine::transform::geom::Fill,
    prevent: &mut bool,
    cleanup: &mut plumspirit_engine::transform::geom::ScaleYCleanup,
) {
    use plumspirit_engine::transform::geom::{FillMode, MAX_FILL_COPIES, MAX_FILL_STEP};
    #[derive(PartialEq, Clone, Copy)]
    enum M {
        Transform,
        Rows,
        Step,
    }
    let mut m = match (*fill_on, fill.mode) {
        (false, _) => M::Transform,
        (true, FillMode::Rows) => M::Rows,
        (true, FillMode::Step { .. }) => M::Step,
    };
    let before = m;
    ui.label("Mode");
    ui.indent("scale_y_mode", |ui| {
        ui.radio_value(&mut m, M::Transform, "Transform existing notes")
            .on_hover_text("N notes stay N notes: the rows move apart (stretch) or together (shrink).");
        ui.radio_value(&mut m, M::Rows, "Duplicate Notes: fill added rows")
            .on_hover_text("Every row a note's stretched row covers gets a copy of it, so stretched art stays solid and no row is doubled.");
        ui.radio_value(&mut m, M::Step, "Duplicate Notes: copy by pitch step")
            .on_hover_text("Every note gets the same number of copies, each a pitch step above the last (Auto: as many as the stretch needs, inside the scaled box).");
    });
    if m != before {
        *fill_on = m != M::Transform;
        match m {
            M::Rows => fill.mode = FillMode::Rows,
            M::Step if !matches!(fill.mode, FillMode::Step { .. }) => {
                fill.mode = FillMode::Step { step: 1, copies: 0 };
            }
            _ => {}
        }
    }
    if *fill_on {
        ui.indent("scale_fill", |ui| {
            match &mut fill.mode {
                FillMode::Rows => {
                    ui.horizontal(|ui| {
                        ui.label("Fill every");
                        let mut r = u32::from(fill.res);
                        if ui
                            .add(crate::ui::stepper::Stepper::new(&mut r).range(1..=12).suffix(" row(s)"))
                            .on_hover_text("1 fills every added row, 2 every second piano row, and so on (the note's own row always stays).")
                            .changed()
                        {
                            fill.res = r.clamp(1, 12) as u8;
                        }
                    });
                }
                FillMode::Step { step, copies } => {
                    ui.horizontal(|ui| {
                        ui.label("Pitch step");
                        let mut v = u32::from(*step);
                        if ui
                            .add(crate::ui::stepper::Stepper::new(&mut v).range(1..=u32::from(MAX_FILL_STEP)).suffix(" semitone(s)"))
                            .on_hover_text("How far apart the copies are (12 = octave copies).")
                            .changed()
                        {
                            *step = v.clamp(1, u32::from(MAX_FILL_STEP)) as u8;
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut auto = *copies == 0;
                        if ui
                            .checkbox(&mut auto, "Auto copies")
                            .on_hover_text("As many copies as the stretch adds rows (⌈factor⌉ − 1), never above the scaled box.")
                            .changed()
                        {
                            *copies = if auto { 0 } else { 1 };
                        }
                        if !auto {
                            let mut v = u32::from(*copies);
                            if ui
                                .add(crate::ui::stepper::Stepper::new(&mut v).range(1..=u32::from(MAX_FILL_COPIES)).suffix(" copies"))
                                .on_hover_text("Copies of every note; those past the keyboard are dropped.")
                                .changed()
                            {
                                *copies = v.clamp(1, u32::from(MAX_FILL_COPIES)) as u8;
                            }
                        }
                    });
                }
            }
            ui.horizontal(|ui| {
                ui.label("Copies");
                ui.radio_value(&mut fill.fade, false, "Full velocity");
                ui.radio_value(&mut fill.fade, true, "Fade with distance");
            });
            ui.checkbox(&mut fill.keep_sub, "Copies keep the note's subchannel (otherwise one subchannel step per row)");
            ui.label(
                RichText::new("Copies keep the note's start and length (gate) and its MIDI channel.")
                    .size(theme::size::SMALL)
                    .weak(),
            );
        });
    }
    ui.checkbox(prevent, "Prevent overlaps: clean up the shrunk notes (destructive; default on)")
        .on_hover_text("On a shrink (and with Duplicate Notes), the selected notes that end up on one key are cleaned as chosen below: exact duplicates by default, including stacks the selection already had. Other keys, tracks, subchannels, MIDI channels and unselected notes are untouched. The Preview shows the cleaned result; Undo restores every original.");
    ui.add_enabled_ui(*prevent, |ui| {
        use plumspirit_engine::transform::geom::ScaleYCleanup;
        egui::ComboBox::from_id_salt("scale_y_cleanup")
            .selected_text(cleanup.label())
            .show_ui(ui, |ui| {
                for m in ScaleYCleanup::ALL {
                    ui.selectable_value(cleanup, m, m.label());
                }
            });
        ui.label(
            egui::RichText::new(match cleanup {
                ScaleYCleanup::ExactDuplicates => "Notes with the same start and length on one key become one (the first in source order). Overlapping notes with other starts or lengths stay.",
                ScaleYCleanup::TrimEarlier => "At equal starts the longest note stays; a note still sounding at the next attack on its key is cut there (never to zero length).",
                ScaleYCleanup::RemoveOverlapping => "The earliest note stays (then the longest, then source order); every note that starts before it ends is removed.",
            })
            .small()
            .weak(),
        );
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn whole_percent_does_not_silently_round_fractional_input() {
        assert_eq!(super::whole_percent("1e2 %"), Ok(100));
        for text in ["12.6", "NaN", "101", "-1", "1e400"] {
            assert!(super::whole_percent(text).is_err(), "{text}");
        }
    }

    use super::parse_tick;

    #[test]
    fn typed_ticks_are_checked() {
        assert_eq!(parse_tick(" 1,920 ", "start"), Ok(1920));
        assert_eq!(parse_tick("18446744073709551615", "start"), Ok(u64::MAX));
        for bad in ["", "-1", "1.5", "abc", "18446744073709551616"] {
            assert!(parse_tick(bad, "start").is_err(), "{bad}");
        }
    }
}
