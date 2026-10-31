//! v0.7 PlumSpirit Score files (`.psc`) in the app: Save Selection as Score,
//! Save Graph Selection as Score (automation), Insert Score (menu, or drag a
//! `.psc` onto the Piano Roll) with a modeless window: insertion anchor,
//! transposition, deterministic subchannel mapping, automation, Preview, Apply
//! (one undo step) / Cancel.

use super::PlumSpiritApp;
use super::StatusKind;
use egui::RichText;
use plumspirit_engine::editor::PreparedEdit;
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::{PlumDocument, Subchannel, SubchannelId};
use plumspirit_engine::transform::psc::{
    self, LaneKind, MAPPING_NAMES, Score, ScoreLane, SubMapping, SubPlan,
};
use plumspirit_engine::transform::{Source, TNote};
use std::path::{Path, PathBuf};

pub const PSC_EXTENSION: &str = "psc";

/// An Insert Score window.
pub struct ScoreInsert {
    pub score: Score,
    pub name: String,
    pub anchor: u64,
    pub semis: i32,
    pub mapping: usize,
    pub automation: bool,
    pub preview: bool,
    key: String,
    pub placed: Option<Vec<TNote>>,
    plan: Option<SubPlan>,
    pub error: Option<String>,
}

impl ScoreInsert {
    pub fn new(score: Score, name: String, anchor: u64) -> ScoreInsert {
        ScoreInsert {
            score,
            name,
            anchor,
            semis: 0,
            mapping: 0,
            automation: true,
            preview: true,
            key: String::new(),
            placed: None,
            plan: None,
            error: None,
        }
    }
}

/// Reads and validates a score file (size-checked before reading).
pub fn read_score(path: &Path) -> Result<Score, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Could not open the score: {e}."))?;
    if meta.len() > psc::MAX_FILE {
        return Err("The score file is too large.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read the score: {e}."))?;
    Score::from_bytes(&bytes)
}

/// Writes atomically (temporary file, sync, verify, rename).
pub fn write_score(path: &Path, score: &Score) -> Result<(), String> {
    let bytes = score.to_bytes();
    let tmp = path.with_extension("psc.tmp");
    let r = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Could not save the score: {e}."));
    }
    match std::fs::read(&tmp) {
        Ok(back) if back == bytes => {}
        _ => {
            let _ = std::fs::remove_file(&tmp);
            return Err("The saved score could not be verified; nothing was written.".into());
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not save the score: {e}.")
    })
}

/// A score of the selected notes of track `ti` (with subchannel names).
pub fn score_of_selection(
    doc: &PlumDocument,
    target: impl Into<plumspirit_engine::editor::NoteTarget>,
) -> Result<Score, String> {
    let target = target.into();
    let ti = target.host_track();
    let scope = [plumspirit_engine::transform::TrackScope::of(target, false)];
    let src =
        Source::collect(doc, &scope, psc::MAX_NOTES as usize).map_err(|e| e.user_message())?;
    if src.notes.is_empty() {
        return Err("Select the notes to save as a score first.".into());
    }
    let ppq = doc.division().musical_ppqn().unwrap_or(960) as u32;
    let t = doc.track(ti).ok_or("The track no longer exists.")?;
    let mut s = Score::from_notes(&src.notes, ppq, |_, id| {
        let e = t.subchannels().get(SubchannelId(id));
        (
            e.map_or(0, |e| e.midi_channel),
            e.and_then(|e| e.name.clone()),
        )
    });
    s.meta.push(("track".into(), doc.track_label(ti)));
    s.normalize();
    Ok(s)
}

fn mapping_of(i: usize, active_sub: u32) -> SubMapping {
    match i {
        1 => SubMapping::CreateNew,
        2 => SubMapping::ById,
        3 => SubMapping::Single(active_sub),
        _ => SubMapping::ByChannel,
    }
}

/// `path` with the `.psc` extension when it has none.
pub fn with_psc(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(PSC_EXTENSION)
    }
}

impl PlumSpiritApp {
    /// Asks for a score path (native dialog, or the in-app prompt).
    pub(super) fn request_score_path(
        &mut self,
        frame: &eframe::Frame,
        purpose: super::dialogs::PromptPurpose,
    ) {
        use super::dialogs::{Dialog, PromptPurpose};
        use super::files;
        if !files::NATIVE_DIALOGS {
            self.dialog = Some(Dialog::PathPrompt {
                purpose,
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
        match purpose {
            PromptPurpose::InsertScore => {
                if let Some(p) = files::pick_open(frame, files::FileKind::Score, dir.as_deref()) {
                    let at = self.edit_cursor();
                    self.open_score_insert(&p, at);
                }
            }
            PromptPurpose::SaveAutomation => {
                if let Some(p) = files::pick_save(
                    frame,
                    files::FileKind::Score,
                    dir.as_deref(),
                    "automation.psc",
                ) {
                    self.save_automation_to(with_psc(p));
                }
            }
            _ => {
                if let Some(p) =
                    files::pick_save(frame, files::FileKind::Score, dir.as_deref(), "score.psc")
                {
                    self.save_score_to(with_psc(p));
                }
            }
        }
    }

    pub(super) fn save_score_to(&mut self, path: PathBuf) {
        let ti = self.edit_target();
        let r = self
            .doc()
            .ok_or_else(|| "Open a project first.".to_string())
            .and_then(|d| score_of_selection(d, ti));
        match r.and_then(|s| write_score(&path, &s).map(|_| s)) {
            Ok(s) => self.set_status(
                format!(
                    "Saved {} note(s) in {} subchannel(s) as {}.",
                    group_digits(s.notes.len() as u64),
                    s.subs.len(),
                    path.display()
                ),
                StatusKind::Info,
            ),
            Err(e) => self.set_status(e, StatusKind::Error),
        }
    }

    /// The selected events of the active graph as an automation score.
    pub(super) fn save_automation_to(&mut self, path: PathBuf) {
        let saved = self.event_clipboard.clone();
        if !self.copy_graph_selection() {
            self.set_status(
                "Select points in the Event Graph (Control Change or Pitch Bend) first.",
                StatusKind::Warning,
            );
            return;
        }
        let clip = std::mem::replace(&mut self.event_clipboard, saved);
        let channel = self.graph_slot(self.active_graph).channel;
        let ppq = self
            .doc()
            .and_then(|d| d.division().musical_ppqn())
            .unwrap_or(960) as u32;
        let lane = match clip {
            Some(super::graphs::EventClipboard::Controller(c)) => match c.kind {
                Some(plumspirit_engine::editor::controllers::ControllerKind::Cc(n)) => {
                    Some(ScoreLane {
                        kind: LaneKind::Cc(n),
                        channel,
                        points: c.points.clone(),
                    })
                }
                Some(plumspirit_engine::editor::controllers::ControllerKind::PitchBend) => {
                    Some(ScoreLane {
                        kind: LaneKind::PitchBend,
                        channel,
                        points: c.points.clone(),
                    })
                }
                None => None,
            },
            _ => None,
        };
        let Some(lane) = lane else {
            self.set_status(
                "Only Control Change and Pitch Bend lanes can be saved as automation scores.",
                StatusKind::Warning,
            );
            return;
        };
        let n = lane.points.len();
        let s = Score {
            ppq,
            meta: vec![(
                "created-by".into(),
                format!("PlumSpirit {}", env!("CARGO_PKG_VERSION")),
            )],
            lanes: vec![lane],
            ..Score::default()
        };
        match write_score(&path, &s) {
            Ok(()) => self.set_status(
                format!("Saved {n} automation point(s) as {}.", path.display()),
                StatusKind::Info,
            ),
            Err(e) => self.set_status(e, StatusKind::Error),
        }
    }

    /// Opens the Insert Score window for `path` at `anchor`.
    pub(super) fn open_score_insert(&mut self, path: &Path, anchor: u64) {
        match read_score(path) {
            Ok(s) => {
                let name = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Score")
                    .to_string();
                self.score_insert = Some(ScoreInsert::new(s, name, anchor));
            }
            Err(e) => self.set_status(e, StatusKind::Error),
        }
    }

    /// Dropped `.psc` files: insert at the pointer (snapped) or the cursor.
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        // v0.8: a recording dropped on the window becomes the reference audio.
        if let Some(rec) = dropped.iter().find(|p| super::reference::is_audio_file(p)) {
            if self.open.is_none() {
                self.set_status(
                    "Open a project before loading reference audio.",
                    StatusKind::Warning,
                );
            } else {
                self.reference_win.pending = Some(rec.clone());
            }
            return;
        }
        // v0.8: a picture dropped on the window opens in Image to MIDI.
        if let Some(pic) = dropped.iter().find(|p| {
            p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                ["png", "jpg", "jpeg", "bmp", "gif", "webp"]
                    .iter()
                    .any(|x| e.eq_ignore_ascii_case(x))
            })
        }) {
            if self.open.is_none() {
                self.set_status(
                    "Open a project before converting a picture.",
                    StatusKind::Warning,
                );
            } else {
                let pic = pic.clone();
                self.open_image_to_midi();
                self.load_image(&pic);
            }
            return;
        }
        let Some(path) = dropped.into_iter().find(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(PSC_EXTENSION))
        }) else {
            return;
        };
        if self.open.is_none() {
            self.set_status(
                "Open a project before inserting a score.",
                StatusKind::Warning,
            );
            return;
        }
        let snap = self.snap_unit();
        // On the roll's axis, snapped to the song's grid (V09-S11).
        let anchor = match self.roll.pointer.tick {
            Some(t) => self.to_content(plumspirit_engine::model::timing::snap_floor(
                self.to_song(t),
                snap,
            )),
            None => self.edit_cursor(),
        };
        self.open_score_insert(&path, anchor);
    }

    fn snap_unit(&self) -> u64 {
        let Some(div) = self.doc().map(|d| d.division()) else {
            return 1;
        };
        self.roll.snap.unit_ticks(div).unwrap_or(1).max(1)
    }

    /// Recomputes the placed notes when anything they depend on changed.
    pub(super) fn update_score_insert(&mut self) {
        let ti = self.edit_target().host_track();
        let active_sub = self.active_sub_of(ti).map_or(0, |s| s.id.0);
        let Some(o) = self.open.as_ref() else {
            self.score_insert = None;
            return;
        };
        let doc = o.doc.clone();
        let Some(si) = self.score_insert.as_mut() else {
            return;
        };
        let Some(t) = doc.track(ti) else {
            return;
        };
        let key = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            si.anchor,
            si.semis,
            si.mapping,
            t.id().0,
            t.subchannels_revision(),
            active_sub,
            si.automation
        );
        if key == si.key {
            return;
        }
        si.key = key;
        let ppq = doc.division().musical_ppqn().unwrap_or(960);
        let dest: Vec<(u32, u8)> = t
            .subchannels()
            .entries()
            .iter()
            .map(|e| (e.id.0, e.midi_channel))
            .collect();
        let plan = psc::plan_subs(&si.score, &dest, mapping_of(si.mapping, active_sub));
        match psc::place(
            &si.score,
            si.anchor,
            ppq,
            si.semis,
            &plan,
            doc.project().keys,
        ) {
            Ok(mut v) => {
                if si.automation {
                    psc::apply_velocity_lanes(&si.score, &mut v, si.anchor, ppq);
                }
                si.placed = Some(v);
                si.plan = Some(plan);
                si.error = None;
            }
            Err(e) => {
                si.placed = None;
                si.plan = None;
                si.error = Some(e.user_message());
            }
        }
    }

    fn apply_score_insert(&mut self) {
        let Some(si) = self.score_insert.take() else {
            return;
        };
        // The track's own notes, or the opened clip's content (V09-S11).
        let target = self.edit_target();
        let (Some(placed), Some(plan)) = (si.placed.as_ref(), si.plan.as_ref()) else {
            self.score_insert = Some(si);
            return;
        };
        let budget = self.event_budget_for_scores();
        let Some(doc) = self.doc() else {
            return;
        };
        let Ok(t) = plumspirit_engine::editor::note_view(doc, target) else {
            return;
        };
        let ppq = doc.division().musical_ppqn().unwrap_or(960);
        let records: Vec<_> = placed.iter().map(TNote::to_record).collect();
        let new_subs: Vec<Subchannel> = plan
            .create
            .iter()
            .map(|(id, ch, name)| {
                let mut s = Subchannel::new(SubchannelId(*id), *ch);
                s.name = name.clone();
                s
            })
            .collect();
        let mut ops = Vec::new();
        if !records.is_empty() {
            ops.push(Operation::Prepared {
                edit: PreparedEdit::InsertSelected {
                    host: t.host(),
                    base_revision: t.notes.revision(),
                    records,
                    subs_track: t.track.id(),
                    new_subs,
                },
                label: "Insert Score",
            });
        }
        if si.automation {
            match psc::lane_edits(t, &si.score, si.anchor, ppq, budget) {
                Ok(edits) => {
                    for e in edits {
                        ops.push(Operation::Events {
                            edit: e,
                            label: "Insert Score",
                        });
                    }
                }
                Err(e) => {
                    self.set_status(e.user_message(), StatusKind::Error);
                    self.score_insert = Some(si);
                    return;
                }
            }
        }
        if ops.is_empty() {
            self.set_status("The score is empty.", StatusKind::Warning);
            return;
        }
        let n = placed.len();
        let op = Operation::Group {
            ops,
            label: "Insert Score",
        };
        if self.apply_op(op).is_some() {
            self.set_status(
                format!(
                    "Inserted \"{}\": {} note(s){}.",
                    si.name,
                    group_digits(n as u64),
                    if plan.create.is_empty() {
                        String::new()
                    } else {
                        format!(", {} new subchannel(s)", plan.create.len())
                    }
                ),
                StatusKind::Info,
            );
        } else {
            self.score_insert = Some(si);
        }
    }

    fn event_budget_for_scores(&self) -> usize {
        let bytes = self
            .open
            .as_ref()
            .map_or(64 << 20, |o| o.history.limits().max_bytes);
        (bytes / 16).max(1 << 16)
    }

    /// The Insert Score window (modeless).
    pub(super) fn score_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut si) = self.score_insert.take() else {
            return;
        };
        let meter = self.meter.as_ref().map(|m| m.1.clone());
        let cursor = self.roll.cursor_tick;
        // v0.9: transpose across the project's keys (±255 in 256-Key Mode;
        // `psc::place` refuses a result outside them).
        let top = i32::from(
            self.doc()
                .map_or(plumspirit_engine::model::note::MAX_PITCH, |d| {
                    d.project().keys.max_pitch()
                }),
        );
        let (mut apply, mut close) = (false, false);
        let mut open = true;
        egui::Window::new(format!("Insert Score — {}", si.name))
            .id(egui::Id::new("plumspirit-score-insert"))
            .open(&mut open)
            .resizable(true)
            .default_width(340.0)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-380.0, 80.0))
            .show(ctx, |ui| {
                let s = &si.score;
                ui.label(format!(
                    "{} note(s) · {} subchannel(s) · {} group(s) · {} automation lane(s) · {} PPQN",
                    group_digits(s.notes.len() as u64),
                    s.subs.len(),
                    s.groups.len(),
                    s.lanes.len(),
                    s.ppq
                ));
                ui.horizontal(|ui| {
                    ui.label("Insert at");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut si.anchor)
                            .speed(10.0)
                            .suffix(" ticks"),
                    );
                    if let Some(m) = &meter {
                        ui.label(RichText::new(m.format_position(si.anchor)).weak());
                    }
                    if ui.button("At cursor").clicked() {
                        si.anchor = cursor;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Transpose");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut si.semis)
                            .range(-top..=top)
                            .suffix(" st"),
                    );
                });
                egui::ComboBox::from_label("Subchannels")
                    .selected_text(MAPPING_NAMES[si.mapping.min(MAPPING_NAMES.len() - 1)])
                    .show_ui(ui, |ui| {
                        for (i, n) in MAPPING_NAMES.iter().enumerate() {
                            ui.selectable_value(&mut si.mapping, i, *n);
                        }
                    });
                if !si.score.lanes.is_empty() {
                    ui.checkbox(&mut si.automation, "Insert automation lanes");
                }
                if let Some(p) = &si.plan
                    && !p.create.is_empty()
                {
                    ui.label(
                        RichText::new(format!(
                            "{} new subchannel(s) will be created.",
                            p.create.len()
                        ))
                        .weak(),
                    );
                }
                ui.checkbox(&mut si.preview, "Preview");
                if let Some(e) = &si.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(si.error.is_none(), egui::Button::new("Apply"))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if !open || close {
            ctx.request_repaint();
            return;
        }
        self.score_insert = Some(si);
        if apply {
            self.apply_score_insert();
        }
    }
}

/// The insertion preview on the active track (always the active track).
pub fn score_preview(si: &Option<ScoreInsert>) -> Option<crate::piano_roll::ToolPreview<'_>> {
    let si = si.as_ref()?;
    if !si.preview || si.error.is_some() {
        return None;
    }
    let notes = si.placed.as_deref()?;
    Some(crate::piano_roll::ToolPreview::Run {
        notes,
        extra: &[],
        max_dur: notes.iter().map(|n| n.dur).max().unwrap_or(0),
        all: false,
        rev: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    #[test]
    fn a_selection_roundtrips_through_a_score_file() {
        let mut d = PlumDocument::new_project("s".into(), Bpm::DEFAULT, 480);
        for i in 0..10u64 {
            let r = NoteRecord {
                start_tick: 1000 + i * 120,
                duration_ticks: 60,
                attrs: NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: SubchannelId(0),
                group: 0,
            };
            d.track_mut(0).unwrap().notes.insert(r, i % 2 == 0).unwrap();
        }
        let s = score_of_selection(&d, 0).unwrap();
        assert_eq!(s.notes.len(), 5);
        assert_eq!(s.ppq, 480);
        let dir = std::env::temp_dir().join(format!("ps_psc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.psc");
        write_score(&p, &s).unwrap();
        assert_eq!(read_score(&p).unwrap(), {
            let mut c = s.clone();
            c.normalize();
            c
        });
        std::fs::write(&p, b"garbage").unwrap();
        assert!(read_score(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mapping_of(3, 7), SubMapping::Single(7));
    }
}
