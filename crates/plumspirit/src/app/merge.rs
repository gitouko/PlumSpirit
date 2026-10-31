//! Import MIDI Into Current Track (v0.5, community merges).
//!
//! 1. The file is read on a worker (the project is untouched; Cancel stops it).
//! 2. The dialog shows the options; every change prepares the import again on a
//!    worker ([`prepare_merge`]): the structure summary and the notes that would
//!    be added (drawn as highlighted ghosts) come from that prepared result, so
//!    the preview is exactly what Apply commits.
//! 3. Apply commits the prepared operation through the undo history as one step.
//!    Cancel drops everything: project, history and dirty state are unchanged.

use super::PlumSpiritApp;
use super::dialogs::{Dialog, MergeDraft};
use super::files::{self, FileKind};
use super::jobs::{self, Then};
use super::tasks::TaskOutcome;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::PreparedEdit;
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::merge::{
    ConductorPolicy, MergeMode, MergeOptions, PreparedMerge, prepare_merge,
};
use plumspirit_engine::midi::{ImportSummary, MidiError};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{NoteRecord, NoteStore, PlumDocument, TrackId};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

/// A loaded source waiting for the user's choices.
pub struct MergeState {
    pub source: Arc<PlumDocument>,
    pub file: String,
    pub dest: TrackId,
    pub options: MergeOptions,
    pub prepared: Option<PreparedMerge>,
    pub error: Option<String>,
    /// Options the running preparation is for.
    pending: Option<(MergeOptions, Receiver<PrepareResult>)>,
    /// Longest added note (for drawing), computed with the prepared result.
    longest: u64,
    /// Work time so far: reading the file plus preparing the shown result. The
    /// time the dialog waits for the user is not part of the task's duration.
    read_time: Duration,
    prep_time: Duration,
}

/// A preparation's result and how long it took.
type PrepareResult = (Result<PreparedMerge, String>, Duration);

impl MergeState {
    /// Frees the import on the reaper (L-S48: the source document and the
    /// prepared result hold every note of the imported file — 224M notes,
    /// ~4 GB, cost the Apply frame ~0.5 s on the owner's PC when freed on
    /// the UI thread).
    /// A preparation still running sends into a closed channel and frees its
    /// result on its own thread.
    pub(super) fn release(self) {
        super::project::reap(self);
    }
}

/// The notes an import would add, borrowed from the prepared operation.
pub struct MergeGhosts<'a> {
    stores: Vec<&'a NoteStore>,
    records: &'a [NoteRecord],
    longest: u64,
}

impl<'a> MergeGhosts<'a> {
    pub fn stores(&self) -> Vec<&'a NoteStore> {
        self.stores.clone()
    }

    pub fn records(&self) -> &'a [NoteRecord] {
        self.records
    }

    pub fn longest(&self) -> u64 {
        self.longest
    }
}

fn added_parts(op: &Operation) -> (Vec<&NoteStore>, &[NoteRecord]) {
    let mut stores = Vec::new();
    let mut records: &[NoteRecord] = &[];
    if let Operation::Group { ops, .. } = op {
        for o in ops {
            match o {
                Operation::InsertTracks { tracks, .. } => {
                    stores.extend(tracks.iter().map(|t| &t.notes));
                }
                Operation::Prepared {
                    edit: PreparedEdit::InsertSelected { records: r, .. },
                    ..
                } => records = r,
                _ => {}
            }
        }
    }
    (stores, records)
}

impl MergeState {
    /// Ghosts to draw (only while the dialog's Preview is on).
    pub fn ghosts(&self) -> Option<MergeGhosts<'_>> {
        let p = self.prepared.as_ref()?;
        let (stores, records) = added_parts(&p.operation);
        Some(MergeGhosts {
            stores,
            records,
            longest: self.longest,
        })
    }
}

impl PlumSpiritApp {
    /// File → Import MIDI Into Current Track…
    pub(super) fn request_import_into(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        let Some(dest) = self
            .doc()
            .and_then(|d| d.track(self.active_track()))
            .map(|t| t.id())
        else {
            return;
        };
        if files::NATIVE_DIALOGS {
            if let Some(path) = files::pick_open(frame, FileKind::Open, self.last_dir.as_deref()) {
                self.start_import_into(path, dest, ctx);
            }
        } else {
            self.dialog = Some(Dialog::PathPrompt {
                purpose: super::dialogs::PromptPurpose::ImportInto(dest),
                text: String::new(),
                focused: false,
            });
        }
    }

    pub(super) fn start_import_into(&mut self, path: PathBuf, dest: TrackId, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        self.last_dir = path.parent().map(PathBuf::from);
        let id = self.next_id();
        let keys = self.doc().map_or(KeyRange::Standard, |d| d.project().keys);
        match jobs::spawn_import_into(id, path, dest, keys, ctx) {
            Ok(j) => self.job = Some(j),
            Err(e) => self.show_error("Could not start reading the file.", e.to_string()),
        }
    }

    /// The source file has been read (on a worker).
    pub(super) fn finish_import_into(
        &mut self,
        path: PathBuf,
        dest: TrackId,
        result: Result<(PlumDocument, ImportSummary), MidiError>,
        started: Instant,
    ) {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (source, summary) = match result {
            Ok(r) => r,
            Err(e) => {
                let cancelled = matches!(e.kind, plumspirit_engine::midi::MidiErrorKind::Cancelled);
                self.tasks.finish(
                    "Import Into Track",
                    started,
                    if cancelled {
                        TaskOutcome::Cancelled
                    } else {
                        TaskOutcome::Failed
                    },
                );
                if e.is_wide_key_file() {
                    // v0.9 §062: only a 256-Key Mode project can take those notes.
                    self.show_error(
                        "The MIDI file could not be imported.",
                        format!(
                            "{} To import it into this project, turn on 256-Key Mode in Project Properties first.",
                            e.user_message()
                        ),
                    );
                } else if !cancelled {
                    self.show_error("The MIDI file could not be imported.", e.user_message());
                }
                return;
            }
        };
        log::info!("import into track: read {file}: {}", summary.describe());
        let dest_name = self
            .doc()
            .and_then(|d| d.index_of(dest).map(|i| (d, i)))
            .map(|(d, i)| d.track_label(i))
            .unwrap_or_default();
        let cursor = self.transport.position();
        let cursor_text = self
            .meter_map()
            .map(|m| m.describe(cursor))
            .unwrap_or_else(|| cursor.to_string());
        self.merge = Some(MergeState {
            source: Arc::new(source),
            file: file.clone(),
            dest,
            options: MergeOptions {
                mode: MergeMode::AsSubtracks,
                policy: ConductorPolicy::Keep,
                at: 0,
            },
            prepared: None,
            error: None,
            pending: None,
            longest: 0,
            read_time: started.elapsed(),
            prep_time: Duration::ZERO,
        });
        self.dialog = Some(Dialog::ImportIntoTrack(Box::new(MergeDraft {
            file,
            dest: dest_name,
            mode: MergeMode::AsSubtracks,
            policy: ConductorPolicy::Keep,
            at_cursor: false,
            cursor_text,
            preview: self.settings.preview,
            lines: Vec::new(),
            busy: true,
            error: None,
        })));
    }

    /// Keeps the prepared import in step with the dialog (once per frame).
    pub(super) fn update_merge(&mut self, ctx: &egui::Context) {
        let Some(Dialog::ImportIntoTrack(draft)) = self.dialog.as_ref() else {
            if self.merge.is_some() && !matches!(self.dialog, Some(Dialog::ImportIntoTrack(_))) {
                // The dialog was closed some other way: cancel.
                if let Some(m) = self.merge.take() {
                    m.release();
                }
            }
            return;
        };
        let at = if draft.at_cursor {
            self.transport.position()
        } else {
            0
        };
        let wanted = MergeOptions {
            mode: draft.mode,
            policy: draft.policy,
            at,
        };
        let preview_on = draft.preview;
        let Some(dest_doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let Some(m) = self.merge.as_mut() else {
            return;
        };
        let Some(dest_index) = dest_doc.index_of(m.dest) else {
            if let Some(m) = self.merge.take() {
                m.release();
            }
            self.dialog = None;
            self.set_status("The destination track no longer exists.", StatusKind::Error);
            return;
        };
        // Collect a finished preparation.
        if let Some((opts, rx)) = &m.pending {
            match rx.try_recv() {
                Ok((r, took)) => {
                    if *opts == wanted {
                        m.prep_time = took;
                        match r {
                            Ok(p) => {
                                let (stores, records) = added_parts(&p.operation);
                                let mut longest =
                                    records.iter().map(|r| r.duration_ticks).max().unwrap_or(0);
                                for s in stores {
                                    longest = longest.max(s.snapshot().max_duration());
                                }
                                m.longest = longest;
                                m.options = *opts;
                                m.prepared = Some(p);
                                m.error = None;
                            }
                            Err(e) => {
                                m.prepared = None;
                                m.error = Some(e);
                                m.options = *opts;
                            }
                        }
                    }
                    m.pending = None;
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                Err(TryRecvError::Disconnected) => m.pending = None,
            }
        }
        let current = m.prepared.is_some() || m.error.is_some();
        let up_to_date = current && m.options == wanted;
        if !up_to_date && m.pending.as_ref().is_none_or(|(o, _)| *o != wanted) {
            // (Re)prepare on a worker; an older preparation's result is ignored.
            m.prepared = None;
            m.error = None;
            let (tx, rx) = channel();
            let source = m.source.clone();
            let dd = dest_doc.clone();
            let ctx2 = ctx.clone();
            let opts = wanted;
            let _ = std::thread::Builder::new()
                .name("import-into-prepare".into())
                .spawn(move || {
                    let t = Instant::now();
                    let r = prepare_merge(&dd, dest_index, &source, opts);
                    let _ = tx.send((r, t.elapsed()));
                    ctx2.request_repaint();
                });
            m.pending = Some((opts, rx));
        }
        let busy = m.pending.is_some();
        let lines = m
            .prepared
            .as_ref()
            .map(|p| p.summary.lines())
            .unwrap_or_default();
        let error = m.error.clone();
        let _ = preview_on;
        if let Some(Dialog::ImportIntoTrack(d)) = self.dialog.as_mut() {
            d.busy = busy;
            d.lines = lines;
            d.error = error;
        }
    }

    /// Whether the import ghosts should be drawn.
    pub(super) fn merge_preview_on(&self) -> bool {
        matches!(&self.dialog, Some(Dialog::ImportIntoTrack(d)) if d.preview)
    }

    /// Apply: commits the prepared import as one undo step.
    pub(super) fn apply_merge(&mut self) {
        let Some(mut m) = self.merge.take() else {
            return;
        };
        let Some(p) = m.prepared.take() else {
            self.merge = Some(m);
            return;
        };
        // No worker may share the document while it changes.
        m.pending = None;
        let started = Instant::now();
        let summary = p.summary.clone();
        let dest = m.dest;
        let file = m.file.clone();
        let work = m.read_time + m.prep_time;
        // L-S48: the source document is freed on the reaper.
        m.release();
        match self.apply_op(p.operation) {
            Some(_) => {
                self.dialog = None;
                if let Some(i) = self.doc().and_then(|d| d.index_of(dest)) {
                    self.select_track(i);
                }
                let what = match summary.mode {
                    MergeMode::AsSubtracks => {
                        format!("{} subtrack(s)", summary.new_subtracks)
                    }
                    MergeMode::AsSubchannels => {
                        format!("{} subchannel(s)", summary.new_subchannels)
                    }
                };
                self.tasks.record(
                    format!("Imported {file} into the track ({what})"),
                    work + started.elapsed(),
                    TaskOutcome::Done,
                );
                self.set_status(
                    format!(
                        "Imported {} into the track: {what}, {} note(s). {} undoes the whole import.",
                        file,
                        plumspirit_engine::midi::group_digits(summary.source_notes as u64),
                        super::keybinds::key_or_menu(super::commands::Command::Undo, "Edit → Undo")
                    ),
                    StatusKind::Success,
                );
            }
            None => {
                self.tasks.record(
                    "Import Into Track",
                    work + started.elapsed(),
                    TaskOutcome::Failed,
                );
                self.dialog = None;
            }
        }
    }

    /// Cancel: nothing in the project changes.
    pub(super) fn cancel_merge(&mut self) {
        if let Some(m) = self.merge.take() {
            self.tasks.record(
                "Import Into Track",
                m.read_time + m.prep_time,
                TaskOutcome::Cancelled,
            );
            m.release();
        }
        let _ = Then::Nothing;
    }
}

#[cfg(test)]
mod tests {
    //! L-S46: Import Into Current Track driven through the window's own
    //! Apply button (pointer events on a whole app frame) with Preview on —
    //! the dialog docked at the top right, the ghosts drawn under it, the
    //! Progressor and the F12 overlay shown, as on the owner's PC.
    use super::super::test_support::Harness;
    use super::*;
    use crate::app::commands::Command;
    use crate::app::track_list::MainView;
    use plumspirit_engine::model::Bpm;
    use plumspirit_engine::model::note::{NoteAttrs, NoteOffKind};

    const SIZE: egui::Vec2 = egui::vec2(1904.0, 984.0);

    /// A Format 1 file: a tempo track and one track of `n` notes.
    fn smf(n: u16) -> Vec<u8> {
        let mut t = Vec::new();
        for k in 0..n {
            let p = 40 + (k % 40) as u8;
            t.extend_from_slice(&[0x00, 0x90, p, 100, 0x60, 0x80, p, 0]);
        }
        t.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        let mut b = b"MThd\0\0\0\x06\0\x01\0\x02\x01\xE0".to_vec();
        let tempo = [
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, 0x00, 0xFF, 0x2F, 0x00,
        ];
        for track in [&tempo[..], &t[..]] {
            b.extend_from_slice(b"MTrk");
            b.extend_from_slice(&(track.len() as u32).to_be_bytes());
            b.extend_from_slice(track);
        }
        b
    }

    fn project() -> PlumDocument {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        let recs: Vec<NoteRecord> = (0..64u64)
            .map(|k| {
                NoteRecord::new(
                    k * 120,
                    100,
                    NoteAttrs::new(60 + (k % 12) as u8, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    /// The centre of the button labelled `text` in a frame's shapes.
    fn button(shapes: &[egui::epaint::ClippedShape], text: &str) -> Option<egui::Pos2> {
        shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text() == text => {
                Some(t.galley.rect.translate(t.pos.to_vec2()).center())
            }
            _ => None,
        })
    }

    fn click(h: &mut Harness, at: egui::Pos2) {
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.app_frame_events(SIZE, 1.0, vec![egui::Event::PointerMoved(at)]);
        h.app_frame_events(SIZE, 1.0, vec![press(true)]);
        h.app_frame_events(SIZE, 1.0, vec![press(false)]);
    }

    #[test]
    fn apply_in_the_docked_import_into_preview_imports() {
        let preview = true;
        let mut h = Harness::new(project());
        h.app.settings.preview = preview;
        h.app.settings.progressor.enabled = true;
        h.app.dev_overlay = true;
        h.app.main_view = MainView::PianoRoll;
        let dest = h.doc().track(0).unwrap().id();
        let (src, summary) = plumspirit_engine::midi::import_bytes(&smf(50)).unwrap();
        h.app
            .finish_import_into("x.mid".into(), dest, Ok((src, summary)), Instant::now());
        let deadline = Instant::now() + Duration::from_secs(60);
        let shapes = loop {
            let shapes = h.app_frame(SIZE, 1.0);
            let ready = matches!(&h.app.dialog, Some(Dialog::ImportIntoTrack(d)) if !d.busy);
            if ready && h.app.merge.as_ref().is_some_and(|m| m.prepared.is_some()) {
                break h.app_frame(SIZE, 1.0);
            }
            assert!(Instant::now() < deadline, "never prepared");
            drop(shapes);
            std::thread::sleep(Duration::from_millis(2));
        };
        // On the owner's PC the window sat at the very top right (a position
        // egui remembered): drag it there by its title.
        if let Some(title) = button(&shapes, "Import Into Track") {
            let to = egui::pos2(title.x + 60.0, 12.0);
            let ev = |pressed| egui::Event::PointerButton {
                pos: title,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            h.app_frame_events(SIZE, 1.0, vec![egui::Event::PointerMoved(title)]);
            h.app_frame_events(SIZE, 1.0, vec![ev(true)]);
            for k in 1..=10 {
                let p = title + (to - title) * (k as f32 / 10.0);
                h.app_frame_events(SIZE, 1.0, vec![egui::Event::PointerMoved(p)]);
            }
            h.app_frame_events(
                SIZE,
                1.0,
                vec![egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        let shapes = h.app_frame(SIZE, 1.0);
        let at = button(&shapes, "Apply").expect("the Apply button is drawn");
        let before = h.doc().note_count();
        click(&mut h, at);
        h.app_frame(SIZE, 1.0);
        assert!(
            h.app.dialog.is_none(),
            "preview {preview}: Apply at {at:?} closed the dialog"
        );
        assert_eq!(h.doc().note_count(), before + 50, "preview {preview}");
    }

    /// Frames until the analysis is current and stays so.
    fn settle_analysis(h: &mut Harness) {
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut idle = 0;
        while idle < 5 {
            h.app_frame(SIZE, 1.0);
            idle = if h.app.analysis.busy() { 0 } else { idle + 1 };
            assert!(Instant::now() < deadline, "analysis never settled");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// L-S48 (Windows, 562M notes, Progressor on: the Undo of a 224M-note /
    /// 700-track Import Into held the UI 12.2 s; Redo 41 ms): Undo and Redo
    /// of an Import Into hand the analysis to the background — the frame after
    /// them never patches the removed tracks out of the timeline itself — and
    /// the settled analysis counts exactly what plays.
    #[test]
    fn undo_and_redo_of_a_large_import_into_analyse_in_the_background() {
        let mut h = Harness::new(big_doc(4, 50_000, 480));
        h.app.settings.progressor.enabled = true;
        h.app.main_view = MainView::TrackList;
        settle_analysis(&mut h);
        let plain = h.app.analysis.index().unwrap().total_notes();
        assert_eq!(plain, 200_000);
        let dest = h.doc().track(0).unwrap().id();
        let summary = plumspirit_engine::midi::import_bytes(&smf(1)).unwrap().1;
        h.app.finish_import_into(
            "x.mid".into(),
            dest,
            Ok((big_doc(30, 20_000, 960), summary)),
            Instant::now(),
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        while !h.app.merge.as_ref().is_some_and(|m| m.prepared.is_some()) {
            h.app_frame(SIZE, 1.0);
            assert!(Instant::now() < deadline, "never prepared");
            std::thread::sleep(Duration::from_millis(2));
        }
        h.app.apply_merge();
        settle_analysis(&mut h);
        let all = h.app.analysis.index().unwrap().total_notes();
        assert_eq!(all, plain + 600_000);
        for (cmd, total) in [(Command::Undo, plain), (Command::Redo, all)] {
            let builds = h.app.analysis.builds_started;
            assert!(h.command(cmd));
            h.app_frame(SIZE, 1.0);
            assert_eq!(
                h.app.analysis.builds_started,
                builds + 1,
                "{cmd:?}: the update runs in the background"
            );
            settle_analysis(&mut h);
            assert_eq!(
                h.app.analysis.index().unwrap().total_notes(),
                total,
                "{cmd:?}"
            );
        }
    }

    /// `tracks` tracks of `per` notes each (dense, 40 pitches).
    fn big_doc(tracks: usize, per: u64, ppqn: u32) -> PlumDocument {
        let mut d = PlumDocument::new_project("big".into(), Bpm::DEFAULT, ppqn);
        for k in 0..tracks {
            let i = d.add_track().unwrap();
            let recs: Vec<NoteRecord> = (0..per)
                .map(|n| {
                    let p = 30 + ((n + k as u64) % 40) as u8;
                    NoteRecord::new(
                        n * 30,
                        60,
                        NoteAttrs::new(p, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    /// L-S48 (562M notes on the owner's PC: Apply of a 224M-note Import Into
    /// held the window 5.4 s, its Undo 12.2 s): the window's frame cost of
    /// Apply, Undo and Redo of an Import Into, with the persistent undo
    /// history attached as for an opened MIDI file. Sizes from
    /// `PLUMSPIRIT_MERGE_COST` = "dest notes,source notes" (default 4M,4M).
    #[test]
    #[ignore = "timing: cargo test --release -p plumspirit import_into_frame_cost -- --ignored --nocapture"]
    fn import_into_frame_cost() {
        let (dn, sn) = std::env::var("PLUMSPIRIT_MERGE_COST")
            .ok()
            .and_then(|v| {
                let (a, b) = v.split_once(',')?;
                Some((a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?))
            })
            .unwrap_or((4_000_000, 4_000_000));
        let dir = std::env::temp_dir().join(format!("ps-merge-cost-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Source tracks (`PLUMSPIRIT_MERGE_TRACKS`, default 100; Toilet Story 3
        // has 700) and the Onion Skin on All Other Tracks with outlines
        // (`PLUMSPIRIT_MERGE_ONION=1`, the owner's setting in E0089).
        let st: u64 = std::env::var("PLUMSPIRIT_MERGE_TRACKS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(100);
        let onion = std::env::var("PLUMSPIRIT_MERGE_ONION").is_ok_and(|v| v == "1");
        // Destination tracks (`PLUMSPIRIT_MERGE_DEST_TRACKS`, default 10; the
        // owner's 338M-note document had 732).
        let dt: u64 = std::env::var("PLUMSPIRIT_MERGE_DEST_TRACKS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(10);
        let mut h = Harness::new(big_doc(dt as usize, dn / dt, 3840));
        h.app.config_dir = Some(dir.clone());
        let _ = h.app.attach_history(None, None);
        assert!(h.app.open.as_ref().unwrap().history.journal().is_some());
        // The owner's view in E0089: the Piano Roll (after the first frame,
        // which opens every project in the Track List).
        h.app_frame(SIZE, 1.0);
        h.app.main_view = MainView::PianoRoll;
        if onion {
            assert!(h.command(Command::Onion(
                plumspirit_engine::model::OnionMode::AllOthers
            )));
        }
        let dest = h.doc().track(0).unwrap().id();
        // Converted on import (960 -> 3840 PPQN), as Introverted.mid was.
        let src = big_doc(st as usize, sn / st, 960);
        let summary = plumspirit_engine::midi::import_bytes(&smf(1)).unwrap().1;
        h.app
            .finish_import_into("x.mid".into(), dest, Ok((src, summary)), Instant::now());
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            h.app_frame(SIZE, 1.0);
            if h.app.merge.as_ref().is_some_and(|m| m.prepared.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "never prepared");
            std::thread::sleep(Duration::from_millis(5));
        }
        h.app_frame(SIZE, 1.0);
        let settle = |h: &mut Harness| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(3) {
                h.app_frame(SIZE, 1.0);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        // A frame's wall time, with the Piano Roll's paint and Onion time.
        let frame = |h: &mut Harness| {
            let t = Instant::now();
            h.app_frame(SIZE, 1.0);
            let s = &h.app.roll.stats;
            (t.elapsed(), s.frame_time, s.onion_time)
        };
        settle(&mut h);
        let before = frame(&mut h);
        let t = Instant::now();
        h.app.apply_merge();
        let apply = t.elapsed();
        let after_apply = frame(&mut h);
        let next: Vec<_> = (0..4).map(|_| frame(&mut h)).collect();
        eprintln!("frame before apply {before:?}; after apply {after_apply:?}; next {next:?}");
        assert_eq!(h.doc().note_count() as u64, dn / dt * dt + sn / st * st);
        let journal = h
            .app
            .open
            .as_ref()
            .unwrap()
            .history
            .journal()
            .map(|j| j.disk_bytes());
        settle(&mut h);
        let t = Instant::now();
        h.app.undo();
        let undo = t.elapsed();
        let after_undo = frame(&mut h);
        settle(&mut h);
        let t = Instant::now();
        h.app.redo();
        let redo = t.elapsed();
        let after_redo = frame(&mut h);
        settle(&mut h);
        // Undo, then a new edit: the redo branch (the removed tracks) is
        // discarded.
        h.app.undo();
        settle(&mut h);
        let t = Instant::now();
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        h.app.apply_op(Operation::InsertNote {
            track: 0.into(),
            record: NoteRecord::new(0, 240, a),
        });
        let discard = t.elapsed();
        let after_discard = frame(&mut h);
        eprintln!(
            "import_into_frame_cost dest {dn} + source {sn}: apply {apply:?} (+frame {after_apply:?}), journal {journal:?} B; undo {undo:?} (+frame {after_undo:?}); redo {redo:?} (+frame {after_redo:?}); edit discarding the redo {discard:?} (+frame {after_discard:?})"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
