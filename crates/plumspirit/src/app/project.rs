//! Project lifecycle: New Project, Open (.lup / .mid), Save / Save As, Export MIDI,
//! Project Properties, unsaved-change protection, background job results and
//! dialog outcomes.

use super::dialogs::{self, Dialog, DialogOutcome, OverwriteFor, PromptPurpose};
use super::jobs::{self, JobKind, JobOutput, Then};
use super::{DocSource, OpenDoc, PlumSpiritApp, Status, files};
use crate::diagnostics::{self, format_bytes, format_duration};
use crate::recent::RecentKind;
use crate::ui::status_bar::StatusKind;
use egui::ViewportCommand;
use plumspirit_engine::editor::EditError;
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::provenance::{SourceStatus, strict_candidate};
use plumspirit_engine::midi::strict::{StatusBytes, StrictReport};
use plumspirit_engine::midi::{
    ExportError, ExportOptions, ExportReport, ExportSnapshot, ImportSummary, ImportWarning,
    MidiError, group_digits, same_key_disclosure,
};
use plumspirit_engine::model::project::{DEFAULT_PROJECT_NAME, KeyRange, parse_project_name};
use plumspirit_engine::model::timing::parse_ppqn;
use plumspirit_engine::model::{Bpm, PlumDocument, Snap};
use plumspirit_engine::native::{LupError, LupLoadReport, SaveReport, has_lup_signature};
use std::path::{Path, PathBuf};

impl PlumSpiritApp {
    /// Makes `doc` the open document (any previous one is dropped).
    pub(super) fn install_document(&mut self, doc: PlumDocument, source: DocSource, saved: bool) {
        if self.open.is_some() {
            self.close_document();
        }
        // The previous document's undo history is closed first (v0.8).
        self.close_history();
        self.autobackup.document_changed();
        // B1-0064: the new document's active track brings back its own
        // Per-Track Onion settings (nothing is stored for the old one).
        self.onion_track_seen = None;
        let generation = self.next_generation;
        self.next_generation += 1;
        let snap_valid = self.roll.snap.applies_to(doc.division());
        self.roll.document_changed(snap_valid);
        if self.roll.snap == Snap::Off && doc.division().musical_ppqn().is_some() {
            self.roll.snap = Snap::Sixteenth;
        }
        self.graphs.cancel();
        self.graphs.clear_point_selection();
        self.active_graph = 0;
        self.selected_tracks.clear();
        self.time_selection = None;
        self.time_cache = None;
        self.graph_focus = false;
        self.transport.reset_for_new_document();
        self.audition_held = None;
        self.audition_warned = false;
        self.analysis.reset();
        self.sel_stats.reset();
        self.sel_charts.clear();
        self.keys.clear();
        self.pending_operation = None;
        self.pending_export = None;
        self.onion_choice = None;
        // L-H04: little memory left with this document → low-memory mode.
        let (limits, low) = low_memory_limits(
            self.history_limits,
            crate::platform::commit_headroom_bytes(),
        );
        if let Some(n) = &low {
            log::warn!("{n}");
        }
        self.low_memory = low;
        let old = self
            .open
            .replace(OpenDoc::new(doc, source, limits, saved, generation));
        // B1-0170: a huge previous project is freed off the GUI thread.
        reap(old);
        self.meter = None;
        self.display_tempo = None;
        self.last_export = None;
        // The project's loop (saved with it) and the audition speed (a user
        // setting) apply to the new document's playback.
        self.sync_loop();
        if let Some(o) = self.open.as_ref() {
            let (d, g) = (o.doc.clone(), o.generation);
            let _ = self.transport.set_speed(self.settings.speed, &d, g);
        }
    }

    /// Runs `then` once unsaved changes have been saved or discarded.
    fn continue_with(&mut self, then: Then, frame: &eframe::Frame, ctx: &egui::Context) {
        match then {
            Then::Nothing => {}
            Then::OpenDialog => self.open_dialog(frame, ctx),
            Then::OpenPath(p) => self.start_open(p, ctx),
            Then::NewProject => self.show_new_project_dialog(),
            Then::Close => self.close_document(),
            Then::Exit => {
                self.allowed_to_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// File → Close Project (Ctrl+W): back to the start screen; unsaved
    /// changes are offered Save / Don't Save / Cancel first. The application
    /// stays open.
    pub(super) fn request_close_project(&mut self) {
        if self.open.is_some() && self.check_unsaved(Then::Close) {
            self.close_document();
        }
    }

    /// Drops the open document and every view state tied to it (playback
    /// stops first: nothing keeps sounding).
    fn close_document(&mut self) {
        let Some(name) = self.open.as_ref().map(|o| o.name().to_owned()) else {
            return;
        };
        self.close_history();
        self.autobackup.document_changed();
        self.scrub_stop();
        self.scrub.a = None;
        self.scrub.b = None;
        self.selected_tracks.clear();
        self.time_selection = None;
        self.time_cache = None;
        self.graph_focus = false;
        let audio = self.transport.project_release();
        self.pending_soundfont = None;
        self.preroll_watch = None;
        self.strict_refused = None;
        self.export_strict = None;
        self.export_note = None;
        self.audition_held = None;
        self.keys.clear();
        self.onion_choice = None;
        self.subchannel_window = false;
        self.meter = None;
        self.display_tempo = None;
        self.last_export = None;
        self.scripts.cancel_all();
        let stamp = self.lua_observation_stamp();
        if let Some(m) = self.lua_plugins.as_mut() {
            m.observation_context(stamp);
            m.project_closed();
        }
        self.lua_plugins_generation = None;
        let estimate = self
            .open
            .as_ref()
            .map_or(0, |o| o.doc.heap_bytes() + o.history.bytes())
            + self.analysis.heap_bytes();
        let document = std::sync::Arc::downgrade(&self.open.as_ref().expect("open project").doc);
        let analysis = std::mem::take(&mut self.analysis);
        // L-H03: the project's background work stops now, not when the
        // reaper reaches it (measured: a 60M-note close waited ~300 ms for
        // the Player's snapshot, every worker competing with the GUI).
        analysis.cancel_jobs();
        let selection = std::mem::take(&mut self.sel_stats);
        let sel_charts = std::mem::take(&mut self.sel_charts);
        let player = std::mem::take(&mut self.player);
        player.cancel_job();
        let inspector = std::mem::take(&mut self.inspector);
        let backup = std::mem::take(&mut self.autobackup);
        let reference = std::mem::take(&mut self.reference_win);
        let merge = self.merge.take();
        let mut preview = self.preview.take();
        let mut tool = self.tool.take();
        let roll = std::mem::take(&mut self.roll);
        self.roll.tool = roll.tool;
        self.roll.snap = roll.snap;
        let mut workers = std::mem::take(&mut self.retired);
        if let Some(j) = self.job.take() {
            workers.extend(j.retire());
        }
        let compaction = self.history_compaction.take();
        // Every buffer tied to this project travels together. TextureHandle
        // drops enqueue their GPU frees in egui's manager for the next frame.
        let data = (
            self.open.take(),
            std::mem::take(&mut self.graphs),
            std::mem::take(&mut self.track_list),
            self.transient.take(),
            self.custom_tool.take(),
            self.pending_operation.take(),
            self.pending_export.take(),
            self.ppqn_prepared.take(),
            self.score_insert.take(),
            self.dialog.take(),
            self.clipboard.take(),
            self.clip_clipboard.take(),
            self.event_clipboard.take(),
            std::mem::take(&mut self.selection_slots),
            std::mem::take(&mut self.tree_rows),
            std::mem::take(&mut self.note_counts),
            std::mem::take(&mut self.onion_order),
            self.sub_counts.take(),
            self.sub_filtered.take(),
            std::mem::take(&mut self.active_subs),
            std::mem::take(&mut self.collapsed),
            std::mem::take(&mut self.lua_project_data),
        );
        self.session_cache.set(None);
        self.summary = None;
        self.main_view_generation = None;
        self.row_geometry = Default::default();
        self.audition_pending = None;
        let done = reap_work(move || {
            audio();
            // The Piano Roll's tile workers stop (their jobs check the quit
            // flag) before the reaper waits for anything else.
            drop(roll);
            if let Some(p) = preview.as_mut() {
                p.stop_worker();
            }
            if let Some(t) = tool.as_mut() {
                t.stop_worker();
            }
            for w in workers {
                let _ = w.join();
            }
            if let Some(c) = compaction {
                c.release();
            }
            analysis.release();
            selection.release();
            sel_charts.release();
            player.release();
            inspector.release();
            backup.release();
            reference.release();
            if let Some(m) = merge {
                m.release();
            }
            drop((preview, tool, data));
        });
        let now = std::time::Instant::now();
        self.project_releases.push(ReleaseTask {
            done,
            reaped: false,
            document,
            started: now,
            estimate,
            last_poll: now,
            worst_gap: std::time::Duration::ZERO,
        });
        self.set_status(
            format!("Closed {name}. Releasing project memory…"),
            StatusKind::Info,
        );
    }

    /// Completion is polled, never joined by the GUI. Opening another file
    /// while cleanup runs neither waits nor receives an old completion status.
    pub(super) fn poll_project_releases(&mut self, ctx: &egui::Context) {
        let mut finished = None;
        let mut failed = false;
        let now = std::time::Instant::now();
        self.project_releases.retain_mut(|r| {
            r.worst_gap = r.worst_gap.max(now.saturating_duration_since(r.last_poll));
            r.last_poll = now;
            if !r.reaped {
                match r.done.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        failed = true;
                        return false;
                    }
                    Ok(()) => r.reaped = true,
                }
            }
            // Lua jobs/hooks are canceled and pumped by the normal frame
            // loop. A still-held document must not be reported as released.
            if r.document.strong_count() != 0 {
                return true;
            }
            log::info!(
                "project release complete in {} (project data estimate {}); longest time between two frames meanwhile {}",
                format_duration(r.started.elapsed()),
                format_bytes(r.estimate as u64),
                format_duration(r.worst_gap)
            );
            finished = Some((r.estimate, r.started.elapsed(), r.worst_gap));
            false
        });
        if failed {
            self.set_status("The project cleanup worker stopped without confirming completion. See the log for details.", StatusKind::Warning);
        } else if !self.project_releases.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        } else if let Some((estimate, took, gap)) = finished
            && self.open.is_none()
        {
            // Only the Windows build appends to it (private committed memory).
            #[cfg_attr(not(windows), allow(unused_mut))]
            let mut details = vec![
                format!("Released project data estimate: {} (shared buffers can be counted in more than one cache).", format_bytes(estimate as u64)),
                "SoundFonts, the audio device, settings and application resources remain available. Project clipboards and selection slots are cleared; saved files and persistent project history stay on disk.".into(),
                "Freed allocations can remain in allocator or operating-system caches. The process working set is not a count of live project data and need not return to zero.".into(),
                format!("Process working set after cleanup: {}.", diagnostics::process_memory_bytes().map_or_else(|| "unavailable".into(), format_bytes)),
                format!(
                    "Cleanup took {}; the longest time between two frames meanwhile was {}.",
                    format_duration(took),
                    format_duration(gap)
                ),
            ];
            #[cfg(windows)]
            if let Some(bytes) = crate::platform::private_commit_bytes() {
                details.push(format!(
                    "Process private committed memory after cleanup: {}.",
                    format_bytes(bytes)
                ));
            }
            self.set_status_details("Project data released.", StatusKind::Info, details);
        }
    }

    /// Asks about unsaved changes before `then`; returns true when it may proceed now.
    pub(super) fn check_unsaved(&mut self, then: Then) -> bool {
        match self.open.as_ref().filter(|o| o.dirty()) {
            Some(o) => {
                self.dialog = Some(Dialog::Dirty {
                    name: o.name().to_owned(),
                    then,
                    asks_path: !matches!(o.source, DocSource::Project(_)),
                    note: o.discard_note(),
                });
                false
            }
            None => true,
        }
    }

    // ------------------------------------------------------------ new project

    pub(super) fn request_new_project(&mut self) {
        if self.check_unsaved(Then::NewProject) {
            self.show_new_project_dialog();
        }
    }

    /// Opens New Project with the defaults from Settings (B1-0059).
    pub(super) fn show_new_project_dialog(&mut self) {
        self.dialog = Some(Dialog::NewProject {
            name: DEFAULT_PROJECT_NAME.into(),
            bpm: self.settings.new_project_bpm.to_string(),
            ppqn: self.settings.new_project_ppqn.to_string(),
            keys256: self.settings.new_project_keys256,
            error: None,
            focused: false,
        });
    }

    /// Validates the New Project fields; returns an error to show in the dialog.
    pub(super) fn create_project(
        &mut self,
        name: &str,
        bpm: &str,
        ppqn: &str,
        keys256: bool,
    ) -> Result<(), String> {
        let name = parse_project_name(name)?;
        // B1-0101: a tempo a MIDI file can store (60,000,000 BPM at most).
        let bpm = Bpm::parse_midi(bpm)?;
        let ppqn = parse_ppqn(ppqn)?;
        let mut doc = PlumDocument::new_project(name.clone(), bpm, ppqn);
        if keys256 {
            // An empty project: no note or track setting can conflict.
            doc.project_mut().keys = KeyRange::Extended256;
        }
        // B1-0059 / L-F06: the Onion Skin from Settings (a display setting
        // of the project; part of its saved state, so it is not dirty).
        self.apply_onion_defaults(&mut doc);
        log::info!("new project “{name}”: {bpm} BPM, {ppqn} PPQN, keys256 {keys256}");
        self.install_document(doc, DocSource::New, false);
        let _ = self.attach_history(None, None);
        self.set_status(
            format!(
                "New project “{name}” — {bpm} BPM, {} PPQN, 4/4{}.",
                group_digits(u64::from(ppqn)),
                if keys256 { ", 256-Key Mode" } else { "" }
            ),
            StatusKind::Info,
        );
        Ok(())
    }

    // ------------------------------------------------------------ properties

    pub(super) fn open_properties(&mut self) {
        let Some(d) = self.doc() else {
            return;
        };
        let ppqn = d
            .division()
            .musical_ppqn()
            .map_or_else(|| d.division().describe(), group_digits);
        self.dialog = Some(Dialog::ProjectProperties {
            name: d.project().name.clone(),
            bpm: self.tempo_display(),
            ppqn,
            keys256: d.project().keys == KeyRange::Extended256,
            error: None,
            focused: false,
        });
    }

    fn apply_properties(&mut self, name: &str, bpm: &str, keys256: bool) -> Result<(), String> {
        let name = parse_project_name(name)?;
        let Some(d) = self.doc() else {
            return Ok(());
        };
        // v0.9 §058: checked before anything changes, so a refusal keeps the
        // dialog open with the reason and the project untouched.
        let keys = if keys256 {
            KeyRange::Extended256
        } else {
            KeyRange::Standard
        };
        let new_keys = (d.project().keys != keys).then_some(keys);
        if let Some(why) = new_keys.and_then(|k| d.key_range_conflict(k)) {
            return Err(why);
        }
        let smpte = d.division().musical_ppqn().is_none();
        let new_tempo = if smpte || bpm.trim() == self.tempo_display() {
            None
        } else {
            Some(Bpm::parse_midi(bpm)?)
        };
        if d.project().name != name {
            self.apply_op(Operation::SetName(name));
        }
        if let Some(b) = new_tempo {
            self.set_start_tempo(b);
        }
        if let Some(keys) = new_keys
            && self.apply_op(Operation::SetKeyRange { keys }).is_some()
        {
            self.set_status(
                if keys == KeyRange::Extended256 {
                    "256-Key Mode is on: the project uses keys 0–255 (Undo turns it off)."
                } else {
                    "256-Key Mode is off: the project uses the MIDI keys 0–127."
                },
                StatusKind::Info,
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------ open

    pub(super) fn request_open(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        if self.check_unsaved(Then::OpenDialog) {
            self.open_dialog(frame, ctx);
        }
    }

    pub(super) fn open_dialog(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        if !files::NATIVE_DIALOGS {
            self.dialog = Some(Dialog::PathPrompt {
                purpose: PromptPurpose::Open,
                text: self
                    .last_dir
                    .as_ref()
                    .map(|d| format!("{}/", d.display()))
                    .unwrap_or_default(),
                focused: false,
            });
            return;
        }
        // The native dialog blocks this frame; keep it out of the frame statistics.
        self.perf.skip_current_frame();
        if let Some(path) = files::pick_open(frame, files::FileKind::Open, self.last_dir.as_deref())
        {
            self.start_open(path, ctx);
        }
    }

    /// Opens a project or MIDI file (decided by content, then by extension).
    pub(super) fn start_open(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.start_open_with(path, KeyRange::Standard, ctx);
    }

    /// [`Self::start_open`] with the key reading of a MIDI file (v0.9 §062:
    /// [`KeyRange::Extended256`] opens a non-standard 256-key file as a
    /// 256-Key Mode project; ignored for projects).
    pub(super) fn start_open_with(&mut self, path: PathBuf, keys: KeyRange, ctx: &egui::Context) {
        self.start_open_as(path, keys, false, ctx);
    }

    /// [`Self::start_open_with`], in Compatibility Mode when `compat`
    /// (L-D05: deterministic repairs of an out-of-spec MIDI file, each listed).
    pub(super) fn start_open_as(
        &mut self,
        path: PathBuf,
        keys: KeyRange,
        compat: bool,
        ctx: &egui::Context,
    ) {
        // A newer load replaces an older one that is still running; the older job is
        // cancelled and its result can never reach the new document.
        if let Some(old) = self.job.take() {
            if matches!(
                old.kind,
                JobKind::Import { .. } | JobKind::LoadProject { .. } | JobKind::OpenArchive { .. }
            ) {
                if let Some(h) = old.retire() {
                    self.retired.push(h);
                }
            } else {
                self.job = Some(old);
                self.set_status(
                    "Please wait for the current operation to finish.",
                    StatusKind::Warning,
                );
                return;
            }
        }
        // L-D02: a spooled file of the open archive keeps its origin (also
        // when it is opened again with 256 keys); any other file closes it.
        let origin = self.archive_origin_for(&path);
        if origin.is_none() {
            self.archive_session = None;
            if !has_lup_signature(&path) && super::archive_open::is_archive_file(&path) {
                self.start_open_archive(path, ctx);
                return;
            }
        }
        let shown = origin
            .as_ref()
            .map_or_else(|| path.clone(), |o| o.archive.clone());
        if let Some(dir) = shown.parent() {
            self.last_dir = Some(dir.to_path_buf());
        }
        let project = has_lup_signature(&path)
            || (!files::is_midi_path(&path)
                && path.extension().is_some_and(|e| {
                    e.eq_ignore_ascii_case(plumspirit_engine::native::LUP_EXTENSION)
                }));
        log::info!(
            "opening {} as {}",
            path.display(),
            if project { "project" } else { "MIDI" }
        );
        self.note_recent(
            &shown,
            if project {
                RecentKind::Project
            } else {
                RecentKind::Midi
            },
            false,
        );
        // B1-0166: say before reading when the file will likely not fit in
        // the free memory (measured: a document takes about 3.5 times its MIDI
        // or .lup file), instead of letting a 2 GB PC run out silently.
        // L-H04: the memory guard stops the reading safely if it runs out.
        if let (Some(avail), Ok(meta)) = (
            crate::platform::available_memory_bytes(),
            std::fs::metadata(&path),
        ) {
            let need = meta.len().saturating_mul(MEMORY_PER_FILE_BYTE_X2) / 2;
            if need > avail {
                let gb = |b: u64| format!("{:.1} GB", b as f64 / 1e9);
                let text = format!(
                    "Opening {} needs about {} of memory and only {} is free: it may fail or slow the computer down (PlumSpirit stops safely if memory runs out). Close other programs, or cancel and open a smaller file.",
                    origin
                        .as_ref()
                        .map_or_else(|| files::display_name(&path), |o| o.file_name.clone()),
                    gb(need),
                    gb(avail)
                );
                self.set_status(text, StatusKind::Warning);
            }
        }
        let id = self.next_id();
        let spawned = if project {
            jobs::spawn_load_project(id, path, ctx)
        } else {
            jobs::spawn_import_from(id, path, keys, compat, origin, ctx)
        };
        match spawned {
            Ok(job) => self.job = Some(job),
            Err(e) => self.show_error(
                "Could not open the file.",
                format!("A worker thread could not be started: {e}"),
            ),
        }
    }

    /// L-H04: an opening's status with the low-memory note of the document
    /// it installed, if any (a warning, the note first in its details).
    fn with_low_memory(&mut self, mut status: Status) -> Status {
        if let Some(n) = self.low_memory.take() {
            status.text.push_str(" · memory nearly full");
            status.kind = StatusKind::Warning;
            status.details.insert(0, n);
        }
        status
    }

    /// L-H04: the honest report of an import / load the memory guard
    /// stopped (`jobs::RunningJob::guard_memory`): what was left, why it
    /// stopped there, that nothing changed, what a note costs.
    fn show_memory_stop(&mut self, name: &str, free: u64, floor: u64, file_bytes: u64) {
        let gb = |b: u64| format!("{:.1} GB", b as f64 / 1e9);
        let mut body = format!(
            "{name}\n\nPlumSpirit stopped opening it when the computer had only {} of memory left to give out. It stops at {}, before the computer runs out of memory and programs start to fail. Nothing was changed.",
            gb(free),
            gb(floor)
        );
        if self.open.is_some() {
            body.push_str(
                " The project that was open is still open; closing it first frees its memory.",
            );
        }
        if file_bytes > 0 {
            body.push_str(&format!(
                "\n\nOpening this file needs about {} (about {} bytes of memory for each byte of a MIDI file: every note takes about 20 bytes; a 302-million-note file needs about 6 GB).",
                gb(file_bytes.saturating_mul(MEMORY_PER_FILE_BYTE_X2) / 2),
                MEMORY_PER_FILE_BYTE_X2 as f64 / 2.0
            ));
        }
        body.push_str(
            " Close other programs, make the Windows page file larger, or open a smaller file.",
        );
        log::warn!(
            "opening {name} stopped by the memory guard: {} MB left, floor {} MB",
            free >> 20,
            floor >> 20
        );
        self.show_error("Not enough memory to open the file.", body);
    }

    pub(super) fn finish_import(
        &mut self,
        path: PathBuf,
        origin: Option<jobs::ImportOrigin>,
        (keys, compat): (KeyRange, bool),
        result: Result<(PlumDocument, ImportSummary), MidiError>,
    ) {
        // L-D02: a file from an archive is shown and remembered by its origin;
        // `path` (the spooled copy) is only read.
        let shown = origin
            .as_ref()
            .map_or_else(|| path.clone(), |o| o.shown_path());
        let recent = origin
            .as_ref()
            .map_or_else(|| path.clone(), |o| o.archive.clone());
        match result {
            Ok((mut doc, summary)) => {
                let name = files::display_name(&shown);
                log::info!(
                    "imported {}: {} in {} ({}; source {}); document heap {}; process memory {}",
                    path.display(),
                    summary.describe(),
                    format_duration(summary.duration),
                    summary.stages.describe(),
                    format_bytes(summary.source_bytes),
                    format_bytes(doc.heap_bytes() as u64),
                    diagnostics::process_memory_bytes().map_or_else(|| "n/a".into(), format_bytes),
                );
                for w in &summary.warnings {
                    log::warn!("import warning: {}", w.user_message());
                }
                // B1-0059 / L-F06: a MIDI file carries no Onion settings; the
                // defaults from Settings apply (before the saved state is
                // taken).
                self.apply_onion_defaults(&mut doc);
                doc.project_mut().name = shown
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_PROJECT_NAME.into());
                let warnings: Vec<String> = summary
                    .warnings
                    .iter()
                    .map(ImportWarning::user_message)
                    .collect();
                let mut text = format!(
                    "Opened {name}{} — {} → {}",
                    if summary.compatibility {
                        " in Compatibility Mode"
                    } else {
                        ""
                    },
                    summary.describe(),
                    summary.structure_short()
                );
                if !warnings.is_empty() {
                    text.push_str(&format!(" · {} warning(s)", warnings.len()));
                }
                let kind = if warnings.is_empty() {
                    StatusKind::Info
                } else {
                    StatusKind::Warning
                };
                // The structural mapping (§72) comes first in Details, then warnings.
                let mut details: Vec<String> = summary.structure_lines().into();
                details.extend(warnings);
                if let Some(o) = &origin {
                    details.insert(0, format!("Opened from the archive: {}", o.logical));
                    details.extend(self.archive_notes());
                }
                self.note_recent(&recent, RecentKind::Midi, true);
                self.install_document(doc, DocSource::Midi(shown), true);
                let _ = self.attach_history(None, None);
                let status = Status {
                    text,
                    kind,
                    details,
                };
                self.status = Some(self.with_low_memory(status));
            }
            Err(e) if e.is_cancelled() => self.set_status("Loading canceled.", StatusKind::Info),
            Err(e) if e.is_wide_key_file() => {
                // v0.9 §062: named, never misread; offered as 256-Key Mode.
                log::warn!("{} is a non-standard 256-key file: {e:?}", shown.display());
                self.dialog = Some(Dialog::OpenWideKeys {
                    message: format!("{}\n\n{}", files::display_name(&shown), e.user_message()),
                    path,
                    compat,
                });
            }
            Err(e) if e.is_repairable() && !compat => {
                // L-D05: offered, never applied silently.
                log::warn!(
                    "{} is outside the MIDI specification: {e:?}",
                    shown.display()
                );
                self.dialog = Some(Dialog::OpenCompatible {
                    message: format!("{}\n\n{}", files::display_name(&shown), e.user_message()),
                    path,
                    keys,
                });
            }
            Err(e) => {
                log::error!("import of {} failed: {e:?}", shown.display());
                let mut body = format!("{}\n\n{}", files::display_name(&shown), e.user_message());
                if compat {
                    body.push_str("\n\nCompatibility Mode cannot repair this damage.");
                }
                self.show_error("Could not open MIDI file.", body);
            }
        }
    }

    fn finish_load_project(
        &mut self,
        path: PathBuf,
        result: Result<(PlumDocument, LupLoadReport, u64), LupError>,
    ) {
        match result {
            Ok((doc, report, identity)) => {
                let name = files::display_name(&path);
                log::info!(
                    "opened project {}: {} notes, {} events, {} tracks in {}; document heap {}",
                    path.display(),
                    report.notes,
                    report.events,
                    doc.track_count(),
                    format_duration(report.duration),
                    format_bytes(doc.heap_bytes() as u64)
                );
                let mut details = report.warnings.clone();
                if let Some(sf) = &doc.project().soundfont
                    && !Path::new(&sf.path).is_file()
                {
                    details.push(format!(
                        "The project's SoundFont was not found: {}. Playback uses the default SoundFont; choose a replacement with Browse….",
                        sf.path
                    ));
                }
                let mut text = format!(
                    "Opened {name} — {} notes on {} track(s) in {}",
                    group_digits(report.notes),
                    doc.track_count(),
                    format_duration(report.duration)
                );
                if !details.is_empty() {
                    text.push_str(&format!(" · {} note(s)", details.len()));
                }
                let kind = if details.is_empty() {
                    StatusKind::Info
                } else {
                    StatusKind::Warning
                };
                self.note_recent(&path, RecentKind::Project, true);
                self.install_document(doc, DocSource::Project(path.clone()), true);
                let (note, warn) = self.attach_history(Some(&path), Some(identity));
                if let Some(n) = note {
                    text.push_str(&format!(" · {n}"));
                }
                if let Some(w) = warn {
                    details.push(w);
                }
                let kind = if details.is_empty() {
                    kind
                } else {
                    StatusKind::Warning
                };
                let status = Status {
                    text,
                    kind,
                    details,
                };
                self.status = Some(self.with_low_memory(status));
            }
            Err(LupError::Cancelled) => self.set_status("Loading canceled.", StatusKind::Info),
            Err(e) => {
                log::error!("loading project {} failed: {e:?}", path.display());
                self.show_error(
                    "Could not open project.",
                    format!("{}\n\n{}", files::display_name(&path), e.user_message()),
                );
            }
        }
    }

    // ------------------------------------------------------------------ save

    /// Save: writes back to the open project; new projects and imported MIDI files
    /// ask for a `.lup` destination (the MIDI file itself is never overwritten).
    pub(super) fn save(&mut self, frame: &eframe::Frame, ctx: &egui::Context, then: Then) {
        match self.open.as_ref().map(|o| o.source.clone()) {
            Some(DocSource::Project(path)) => self.start_save(path, then, ctx),
            Some(_) => self.save_as(frame, ctx, then),
            None => {}
        }
    }

    pub(super) fn save_as(&mut self, frame: &eframe::Frame, ctx: &egui::Context, then: Then) {
        let Some(o) = &self.open else {
            return;
        };
        let dir = match &o.source {
            DocSource::Project(p) | DocSource::Midi(p) => p.parent().map(Path::to_path_buf),
            DocSource::New => None,
        }
        .or_else(|| self.last_dir.clone());
        let name = match &o.source {
            DocSource::Project(p) => files::display_name(p),
            _ => format!("{}.lup", files::safe_file_stem(o.name())),
        };
        if !files::NATIVE_DIALOGS {
            let default = dir
                .map(|d| d.join(&name))
                .unwrap_or_else(|| PathBuf::from(&name));
            self.dialog = Some(Dialog::PathPrompt {
                purpose: PromptPurpose::SaveProject(then),
                text: default.display().to_string(),
                focused: false,
            });
            return;
        }
        self.perf.skip_current_frame();
        match files::pick_save(frame, files::FileKind::Project, dir.as_deref(), &name) {
            Some(chosen) => self.save_chosen(chosen, true, then, ctx),
            // Cancelled: an exit/open that waited for this save does not happen.
            None => self.close_after_job = false,
        }
    }

    fn save_chosen(
        &mut self,
        chosen: PathBuf,
        native_confirmed: bool,
        then: Then,
        ctx: &egui::Context,
    ) {
        let (path, changed) = files::normalize_project_path(chosen);
        let is_current = matches!(&self.open.as_ref().map(|o| &o.source), Some(DocSource::Project(p)) if *p == path);
        if path.exists() && !is_current && (changed || !native_confirmed) {
            self.dialog = Some(Dialog::ConfirmOverwrite {
                path,
                purpose: OverwriteFor::Save(then),
            });
            return;
        }
        self.start_save(path, then, ctx);
    }

    fn start_save(&mut self, path: PathBuf, then: Then, ctx: &egui::Context) {
        if self.job.is_some() {
            self.set_status(
                "Please wait for the current operation to finish.",
                StatusKind::Warning,
            );
            return;
        }
        // V09-0118: the Track List's view goes with the project (a display
        // setting: storing it is not an edit and never dirties).
        let generation = self.doc_generation();
        if let Some(view) = self.track_list.view_for(generation)
            && let Some(o) = self.open.as_mut()
            && let Some(d) = std::sync::Arc::get_mut(&mut o.doc)
        {
            d.project_mut().track_list = Some(view);
        }
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let doc = o.doc.clone();
        let state = o.history.current_state();
        self.roll.cancel_gesture();
        self.graphs.cancel();
        log::info!("saving project to {}", path.display());
        let id = self.next_id();
        match jobs::spawn_save(id, doc, path, then, state, ctx) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.show_error(
                "Could not save project.",
                format!("A worker thread could not be started: {e}"),
            ),
        }
    }

    fn finish_save(
        &mut self,
        path: PathBuf,
        then: Then,
        state: plumspirit_engine::history::StateId,
        result: Result<(SaveReport, u64), LupError>,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        match result {
            Ok((r, identity)) => {
                if let Some(o) = self.open.as_mut() {
                    o.mark_saved(state);
                    o.source = DocSource::Project(path.clone());
                }
                self.history_saved(&path, state, identity);
                if let Some(dir) = path.parent() {
                    self.last_dir = Some(dir.to_path_buf());
                }
                self.note_recent(&path, RecentKind::Project, true);
                log::info!(
                    "saved {}: {} bytes, {} notes, write {}, verify {}",
                    path.display(),
                    r.write.bytes,
                    r.write.notes,
                    format_duration(r.write_duration),
                    format_duration(r.verify_duration)
                );
                self.set_status(
                    format!(
                        "Saved {} ({} notes, {}), verified in {}",
                        files::display_name(&path),
                        group_digits(r.write.notes),
                        format_bytes(r.write.bytes),
                        format_duration(r.write_duration + r.verify_duration)
                    ),
                    StatusKind::Success,
                );
                // v0.9 (V09-S18, `docs/INSPECTOR.md` I7).
                self.inspector_saved();
                self.publish_lua_observation(
                    crate::lua::reactive::Kind::Saved,
                    crate::lua::reactive::Data::Saved { midi: false },
                );
                self.continue_with(then, frame, ctx);
                if self.close_after_job {
                    self.close_after_job = false;
                    self.allowed_to_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            Err(e) => {
                self.close_after_job = false;
                log::error!("saving {} failed: {e:?}", path.display());
                self.show_error("Could not save project.", e.user_message());
            }
        }
    }

    // ---------------------------------------------------------------- export

    pub(super) fn request_export(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        self.export_options = ExportOptions {
            explicit_status: self.settings.export_explicit_status,
            player_safe_start: self.settings.export_player_safe_start,
            loudness_reference: self.settings.export_loudness_reference,
            ..ExportOptions::default()
        };
        let Some(doc) = self.doc() else {
            return;
        };
        // v1.0-beta1 (B1-0121): an imported project without MIDI edits is
        // written from its source file (strict); nothing to approve.
        let status = strict_candidate(doc);
        let refused = status == SourceStatus::Unchanged
            && self.strict_refused.is_some()
            && self.strict_refused.as_ref() == doc.project().source.as_ref();
        if status == SourceStatus::Unchanged && !refused {
            self.export_strict = Some(if self.settings.export_explicit_status {
                StatusBytes::Explicit
            } else {
                StatusBytes::Running
            });
            self.export_note = None;
            self.pending_export = None;
            self.choose_export_path(frame, ctx);
            return;
        }
        self.export_strict = None;
        self.export_note = Some(if refused {
            SourceStatus::SourceChanged.sentence()
        } else {
            status.sentence()
        });
        let Some(doc) = self.doc() else {
            return;
        };
        let corrections = doc.tracks().iter().any(|t| t.props().corrects());
        // v0.9: the dialog shows, and the job writes, one export snapshot.
        let snapshot = ExportSnapshot::take(doc, self.export_options);
        let plan = snapshot.plan();
        let structure = export_structure(plan, self.export_options);
        let wide = export_wide_keys(plan);
        let blocked = !plan.blockers.is_empty();
        let needs = plan.needs_approval()
            || corrections
            || structure.is_some()
            || wide.is_some()
            || blocked;
        let lines = plan.disclosures();
        self.pending_export = Some(snapshot);
        if needs {
            self.dialog = Some(Dialog::ExportApproval {
                lines,
                corrections: corrections.then_some(true),
                blocked,
                structure,
                wide,
            });
        } else {
            self.choose_export_path(frame, ctx);
        }
    }

    /// Export dialog: an option changed (the plan is recomputed).
    pub(super) fn set_export_options(&mut self, options: ExportOptions) {
        let Some(doc) = self.doc() else {
            return;
        };
        let snapshot = ExportSnapshot::take(doc, options);
        let plan = snapshot.plan();
        let lines = plan.disclosures();
        let blocked = !plan.blockers.is_empty();
        let st = export_structure(plan, options);
        let w = export_wide_keys(plan);
        self.pending_export = Some(snapshot);
        self.export_options = options;
        if let Some(Dialog::ExportApproval {
            lines: l,
            corrections,
            blocked: b,
            structure,
            wide,
        }) = self.dialog.as_mut()
        {
            *l = lines;
            // Shown while the export meets keys above 127 (switching the
            // corrections off can bring every key back into 0–127); it sits
            // below the checkboxes, so they never move.
            *wide = w;
            if corrections.is_some() {
                *corrections = Some(options.apply_corrections);
            }
            *b = blocked;
            if structure.is_some() || st.is_some() {
                *structure = st.or_else(|| structure.clone());
                if let Some(s) = structure.as_mut() {
                    s.split = options.split_subchannels;
                    s.metadata = options.structure_metadata;
                }
            }
        }
    }

    /// Export dialog: corrections on / off.
    pub(super) fn set_export_corrections(&mut self, apply: bool) {
        let o = ExportOptions {
            apply_corrections: apply,
            ..self.export_options
        };
        self.set_export_options(o);
    }

    fn choose_export_path(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        let Some(o) = &self.open else {
            return;
        };
        let source = match &o.source {
            DocSource::Project(p) | DocSource::Midi(p) => Some(p.as_path()),
            DocSource::New => None,
        };
        let dir = self
            .last_export
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| source.and_then(|p| p.parent().map(Path::to_path_buf)))
            .or_else(|| self.last_dir.clone());
        let name = self
            .last_export
            .as_deref()
            .map(files::display_name)
            .unwrap_or_else(|| files::suggested_export_name(source, o.name()));
        if !files::NATIVE_DIALOGS {
            let default = dir
                .map(|d| d.join(&name))
                .unwrap_or_else(|| PathBuf::from(&name));
            self.dialog = Some(Dialog::PathPrompt {
                purpose: PromptPurpose::Export,
                text: default.display().to_string(),
                focused: false,
            });
            return;
        }
        self.perf.skip_current_frame();
        if let Some(chosen) = files::pick_save(frame, files::FileKind::Midi, dir.as_deref(), &name)
        {
            self.export_chosen(chosen, true, ctx);
        }
    }

    /// `native_confirmed`: the native save dialog already asked about overwriting
    /// the exact path it returned.
    fn export_chosen(&mut self, chosen: PathBuf, native_confirmed: bool, ctx: &egui::Context) {
        let (path, changed) = files::normalize_export_path(chosen);
        if path.exists() && (changed || !native_confirmed) {
            self.dialog = Some(Dialog::ConfirmOverwrite {
                path,
                purpose: OverwriteFor::Export,
            });
            return;
        }
        self.start_export(path, ctx);
    }

    fn start_export(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.job.is_some() {
            self.set_status(
                "Please wait for the current operation to finish.",
                StatusKind::Warning,
            );
            return;
        }
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        if let Some(status) = self.export_strict.take() {
            self.roll.cancel_gesture();
            self.graphs.cancel();
            log::info!("strict export to {} ({status:?})", path.display());
            let id = self.next_id();
            match jobs::spawn_export_strict(id, doc, path, status, ctx) {
                Ok(job) => self.job = Some(job),
                Err(e) => self.show_error(
                    "Export failed.",
                    format!("A worker thread could not be started: {e}"),
                ),
            }
            return;
        }
        // The snapshot the user saw (and possibly approved); taken now if
        // there is none or the project changed since.
        let snapshot = match self.pending_export.take() {
            Some(s) if s.is_current(&doc, self.export_options) => s,
            _ => ExportSnapshot::take(&doc, self.export_options),
        };
        drop(doc);
        self.roll.cancel_gesture();
        self.graphs.cancel();
        log::info!("exporting to {}", path.display());
        let id = self.next_id();
        match jobs::spawn_export(id, snapshot, path, ctx) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.show_error(
                "Export failed.",
                format!("A worker thread could not be started: {e}"),
            ),
        }
    }

    fn finish_export(
        &mut self,
        path: PathBuf,
        result: Result<ExportReport, ExportError>,
        ctx: &egui::Context,
    ) {
        let report = match result {
            Ok(r) => r,
            Err(e) => {
                self.close_after_job = false;
                log::error!("export to {} failed: {e:?}", path.display());
                self.show_error("Export failed.", e.user_message());
                return;
            }
        };
        self.publish_lua_observation(
            crate::lua::reactive::Kind::Saved,
            crate::lua::reactive::Data::Saved { midi: true },
        );
        self.last_export = Some(path.clone());
        let name = files::display_name(&path);
        log::info!(
            "exported {}: {} bytes, {} notes, {} other events, format {}, division {}, write {}, validate {}, {} events verified",
            path.display(),
            report.write.bytes,
            report.write.notes,
            report.write.preserved_events,
            report.write.format_word,
            report.plan.division_word,
            format_duration(report.write_duration),
            format_duration(report.validate_duration),
            report.validation.events_checked
        );
        let (mut text, kind, mut details) = export_status(&name, &report);
        // B1-0122: a normal export says that it was written from the
        // project's contents, and why it was not strict.
        if let Some(why) = self.export_note.take() {
            text.push_str(" · written from the project's contents");
            details.insert(0, why);
        }
        if report.write.status_bytes_saved > 0 {
            details.push(format!(
                "Running Status Compression left out {} status bytes.",
                group_digits(report.write.status_bytes_saved)
            ));
        }
        // The source file was replaced by a file written from the contents:
        // the project no longer is that file.
        if self.is_source_path(&path)
            && let Some(o) = self.open.as_mut()
            && let Some(d) = std::sync::Arc::get_mut(&mut o.doc)
        {
            d.project_mut().source = None;
        }
        self.set_status_details(text, kind, details);
        if self.close_after_job {
            self.close_after_job = false;
            if self.open.as_ref().is_some_and(|o| o.dirty()) {
                self.handle_close_request_after_job(ctx);
            } else {
                self.allowed_to_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// Whether `path` is the file the project was imported from.
    fn is_source_path(&self, path: &Path) -> bool {
        let Some(src) = self.doc().and_then(|d| d.project().source.as_ref()) else {
            return false;
        };
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        !src.path.is_empty() && Path::new(&src.path) == abs
    }

    fn finish_export_strict(
        &mut self,
        path: PathBuf,
        result: Result<StrictReport, ExportError>,
        ctx: &egui::Context,
    ) {
        let report = match result {
            Ok(r) => r,
            Err(e) => {
                self.close_after_job = false;
                log::error!("strict export to {} failed: {e:?}", path.display());
                let mut msg = e.user_message();
                if matches!(e, ExportError::Blocked(_)) {
                    // The source changed or cannot be read: the next export
                    // writes the project from its contents.
                    self.strict_refused = self.doc().and_then(|d| d.project().source.clone());
                    msg.push_str(" Export again to write the project from its contents.");
                }
                self.show_error("Export failed.", msg);
                return;
            }
        };
        self.publish_lua_observation(
            crate::lua::reactive::Kind::Saved,
            crate::lua::reactive::Data::Saved { midi: true },
        );
        self.last_export = Some(path.clone());
        let name = files::display_name(&path);
        log::info!(
            "strict export {}: {} bytes, {} tracks, {} events, identical {}, status bytes saved {}, write {}, validate {}",
            path.display(),
            report.bytes,
            report.tracks,
            report.events,
            report.identical_to_source,
            report.status_bytes_saved,
            format_duration(report.write_duration),
            format_duration(report.validate_duration)
        );
        // A strict export over the source file keeps the project its file.
        if self.is_source_path(&path)
            && let Some(o) = self.open.as_mut()
            && let Some(d) = std::sync::Arc::get_mut(&mut o.doc)
            && let Some(src) = d.project_mut().source.as_mut()
        {
            src.size = report.bytes;
            src.hash = report.hash;
        }
        let (text, kind, details) = strict_status(&name, &report);
        self.set_status_details(text, kind, details);
        if self.close_after_job {
            self.close_after_job = false;
            if self.open.as_ref().is_some_and(|o| o.dirty()) {
                self.handle_close_request_after_job(ctx);
            } else {
                self.allowed_to_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn handle_close_request_after_job(&mut self, _ctx: &egui::Context) {
        if let Some(o) = &self.open {
            self.dialog = Some(Dialog::Dirty {
                name: o.name().to_owned(),
                then: Then::Exit,
                asks_path: !matches!(o.source, DocSource::Project(_)),
                note: o.discard_note(),
            });
        }
    }

    // ------------------------------------------------------------------ jobs

    pub(super) fn poll_job(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let output = match job.poll() {
            Ok(Some(o)) => o,
            Ok(None) => {
                // Throttled progress refresh (the worker also wakes the UI when done).
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(()) => {
                let label = job.label();
                self.job = None;
                self.close_after_job = false;
                self.show_error(
                    "An internal error occurred.",
                    format!("{label} stopped unexpectedly. See the log for details."),
                );
                return;
            }
        };
        let Some(job) = self.job.take() else {
            return;
        };
        // Task duration (bottom status bar): recorded once per job, with its outcome.
        if let Some((name, outcome)) = task_of(&job.kind, &output) {
            self.tasks.finish(name, job.started, outcome);
        }
        // L-H04: stopped by the memory guard, not by the user.
        if let Some((free, floor)) = job.stopped_for_memory()
            && let Some(name) = memory_stopped(&job.kind, &output)
        {
            self.show_memory_stop(&name, free, floor, job.total);
            return;
        }
        let cancelled = job.cancelled();
        match (job.kind, output) {
            (JobKind::Clipboard { generation, .. }, JobOutput::Clipboard(result)) => {
                if cancelled {
                    self.set_status("Clipboard operation canceled.", StatusKind::Info);
                } else if generation == self.doc_generation() {
                    self.finish_clipboard(result, job.started);
                }
            }
            (JobKind::ImportInto { path, dest, .. }, JobOutput::Import(r)) => {
                self.finish_import_into(path, dest, r, job.started)
            }
            (
                JobKind::Import {
                    path,
                    origin,
                    keys,
                    compat,
                    ..
                },
                JobOutput::Import(r),
            ) => self.finish_import(path, origin, (keys, compat), r),
            (JobKind::OpenArchive { path, .. }, JobOutput::Archive(r)) => {
                self.finish_open_archive(path, r, ctx)
            }
            (JobKind::LoadProject { path, .. }, JobOutput::LoadProject(r)) => {
                self.finish_load_project(path, r)
            }
            (JobKind::Save { path, then, state }, JobOutput::Save(r)) => {
                self.finish_save(path, then, state, r, ctx, frame)
            }
            (JobKind::Export { path }, JobOutput::Export(r)) => self.finish_export(path, r, ctx),
            (JobKind::Export { path }, JobOutput::ExportStrict(r)) => {
                self.finish_export_strict(path, r, ctx)
            }
            (JobKind::Edit { label }, JobOutput::Edit(r)) => match r {
                Ok(p) => {
                    self.commit_prepared(label, p, job.started);
                    log::info!(
                        "{label} (job {}) finished in {}",
                        job.id,
                        format_duration(job.started.elapsed())
                    );
                }
                Err(EditError::Cancelled) => {}
                Err(e) => self.show_error(&format!("{label} was not applied."), e.user_message()),
            },
            (JobKind::Tool { label, finish }, JobOutput::Tool(r)) => {
                log::info!(
                    "{label} (job {}) computed in {}",
                    job.id,
                    format_duration(job.started.elapsed())
                );
                self.finish_tool(label, finish, r, job.started);
            }
            (
                JobKind::Ppqn {
                    to,
                    mode,
                    apply,
                    key,
                },
                JobOutput::Ppqn(r),
            ) => {
                log::info!(
                    "Change PPQN to {to} (job {}) computed in {}",
                    job.id,
                    format_duration(job.started.elapsed())
                );
                self.finish_ppqn_job(to, mode, apply, key, r);
            }
            _ => log::error!("job output did not match its kind; ignored"),
        }
    }

    // --------------------------------------------------------------- dialogs

    pub(super) fn handle_dialog(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // B1-0103: a Change PPQN Preview holds a whole copy of the notes;
        // it goes as soon as its dialog closes.
        if self.ppqn_prepared.is_some() && !matches!(self.dialog, Some(Dialog::ChangePpqn(_))) {
            self.ppqn_prepared = None;
        }
        let Some(dialog) = self.dialog.as_mut() else {
            self.dialog_seen = None;
            return;
        };
        // An Enter that opened a dialog (e.g. in a text field) must not also
        // confirm it: buttons react to Enter only from the dialog's second frame.
        let kind = std::mem::discriminant(&*dialog);
        let accept_enter = self.dialog_seen == Some(kind);
        self.dialog_seen = Some(kind);
        // A new dialog sizes itself over its first frames (scroll areas learn
        // their content height a frame late). Repaint them at once, so it never
        // waits for the pointer to move and then jumps under the click.
        self.dialog_frames = if accept_enter {
            self.dialog_frames.saturating_add(1)
        } else {
            0
        };
        if self.dialog_frames < 3 {
            ctx.request_repaint();
        }
        let outcome = dialogs::show(dialog, ctx, accept_enter);
        self.apply_dialog_outcome(outcome, ctx, frame);
    }

    /// What a dialog's outcome does (the window's buttons; tests feed
    /// outcomes directly).
    pub(super) fn apply_dialog_outcome(
        &mut self,
        outcome: DialogOutcome,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        match outcome {
            DialogOutcome::Open => {
                // The merge Preview's destination choice re-plans (§048).
                if matches!(self.dialog, Some(Dialog::MergeClips(ref d)) if d.want_dest != d.plan.dest)
                {
                    self.replan_clip_merge();
                }
                // Paste Repeat: a changed field plans again (§089).
                if matches!(self.dialog, Some(Dialog::RepeatClips(ref d)) if d.stale()) {
                    self.replan_clip_repeat();
                }
            }
            DialogOutcome::ClipRepeatApply => {
                if let Err(msg) = self.apply_clip_repeat() {
                    if let Some(Dialog::RepeatClips(d)) = self.dialog.as_mut() {
                        d.error = Some(msg);
                    }
                } else {
                    self.dialog = None;
                }
            }
            DialogOutcome::ClipMergeApply => {
                if let Err(msg) = self.apply_clip_merge() {
                    if let Some(Dialog::MergeClips(d)) = self.dialog.as_mut() {
                        d.error = Some(msg);
                    }
                } else {
                    self.dialog = None;
                }
            }
            DialogOutcome::Closed => {
                if matches!(self.dialog, Some(Dialog::Dirty { .. })) {
                    // Cancelled: an exit that waited for this decision does not happen.
                    self.close_after_job = false;
                }
                if matches!(self.dialog, Some(Dialog::ImportIntoTrack(_))) {
                    self.cancel_merge();
                }
                self.dialog = None;
                self.pending_operation = None;
                self.pending_export = None;
                self.stop_preview();
            }
            DialogOutcome::DirtySave(then) => {
                self.dialog = None;
                if then == Then::Exit {
                    // Close once the save has completed.
                    self.close_after_job = true;
                    self.save(frame, ctx, Then::Nothing);
                } else {
                    self.save(frame, ctx, then);
                }
            }
            DialogOutcome::DirtyDiscard(then) => {
                self.dialog = None;
                if let Some(o) = self.open.as_ref() {
                    log::info!("discarding unsaved changes to {}", o.name());
                }
                if then == Then::Exit {
                    self.allowed_to_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                } else {
                    self.continue_with(then, frame, ctx);
                }
            }
            DialogOutcome::DeleteTrackConfirmed(roots, folder) => {
                self.dialog = None;
                if folder == 0 {
                    self.delete_tracks(&roots);
                } else {
                    self.delete_folder_with_tracks(folder);
                }
            }
            DialogOutcome::ScaleApply { axis, text, opts } => {
                // The Preview worker shares the document; stop it before editing.
                self.stop_preview();
                self.transient = None;
                if let Err(msg) = self.apply_scale(axis, &text, opts, ctx) {
                    if let Some(Dialog::Scale { error, focused, .. }) = self.dialog.as_mut() {
                        *error = Some(msg);
                        // Enter took the focus away from the field; give it back so the
                        // factor can be corrected straight away.
                        *focused = false;
                    }
                } else if matches!(self.dialog, Some(Dialog::Scale { .. })) {
                    self.dialog = None;
                }
            }
            DialogOutcome::OverwriteConfirmed { path, purpose } => {
                self.dialog = None;
                match purpose {
                    OverwriteFor::Export => self.start_export(path, ctx),
                    OverwriteFor::Save(then) => self.start_save(path, then, ctx),
                }
            }
            DialogOutcome::PathEntered { purpose, path } => {
                self.dialog = None;
                match purpose {
                    PromptPurpose::Open => self.start_open(path, ctx),
                    PromptPurpose::Export => self.export_chosen(path, false, ctx),
                    PromptPurpose::SaveProject(then) => self.save_chosen(path, false, then, ctx),
                    PromptPurpose::SoundFont => self.soundfont_chosen(path),
                    PromptPurpose::ImportInto(dest) => self.start_import_into(path, dest, ctx),
                    PromptPurpose::SaveScore => self.save_score_to(super::scores::with_psc(path)),
                    PromptPurpose::SaveAutomation => {
                        self.save_automation_to(super::scores::with_psc(path))
                    }
                    PromptPurpose::InsertScore => {
                        let at = self.roll.cursor_tick;
                        self.open_score_insert(&path, at);
                    }
                    PromptPurpose::OpenImage => self.load_image(&path),
                    PromptPurpose::OpenReference => self.reference_win.pending = Some(path),
                    PromptPurpose::Locate(old) => self.located(&old, path, ctx),
                }
            }
            DialogOutcome::NewProjectCreate {
                name,
                bpm,
                ppqn,
                keys256,
            } => match self.create_project(&name, &bpm, &ppqn, keys256) {
                Ok(()) => self.dialog = None,
                Err(msg) => {
                    if let Some(Dialog::NewProject { error, .. }) = self.dialog.as_mut() {
                        *error = Some(msg);
                    }
                }
            },
            DialogOutcome::PropertiesApply { name, bpm, keys256 } => {
                match self.apply_properties(&name, &bpm, keys256) {
                    Ok(()) => {
                        if matches!(self.dialog, Some(Dialog::ProjectProperties { .. })) {
                            self.dialog = None;
                        }
                    }
                    Err(msg) => {
                        if let Some(Dialog::ProjectProperties { error, .. }) = self.dialog.as_mut()
                        {
                            *error = Some(msg);
                        }
                    }
                }
            }
            DialogOutcome::OpenChangePpqn => self.open_change_ppqn(),
            DialogOutcome::PpqnPreview => self.ppqn_request(false, ctx),
            DialogOutcome::PpqnApply => self.ppqn_request(true, ctx),
            DialogOutcome::ExportApproved => {
                self.dialog = None;
                self.choose_export_path(frame, ctx);
            }
            DialogOutcome::ApplyWithoutUndo => {
                self.dialog = None;
                if let Some(op) = self.pending_operation.take() {
                    self.apply_without_undo(*op);
                }
            }
            DialogOutcome::Apply => {
                self.stop_preview();
                match self.apply_song_dialog(ctx) {
                    Ok(()) => {
                        // A too-large edit replaced the dialog with its own question.
                        if !matches!(self.dialog, Some(Dialog::TooLargeForUndo { .. })) {
                            self.dialog = None;
                        }
                    }
                    Err(msg) => match self.dialog.as_mut() {
                        Some(
                            Dialog::Marker { error, focused, .. }
                            | Dialog::TimeSignature { error, focused, .. }
                            | Dialog::Tempo { error, focused, .. }
                            | Dialog::Controller { error, focused, .. }
                            | Dialog::MasterVolume { error, focused, .. },
                        ) => {
                            *error = Some(msg);
                            *focused = false;
                        }
                        Some(Dialog::TrackProperties(d, _)) => d.error = Some(msg),
                        Some(Dialog::SelectRange(d, _)) => d.error = Some(msg),
                        Some(Dialog::CurveFunctions(d)) => d.error = Some(msg),
                        _ => {}
                    },
                }
            }
            DialogOutcome::GoTo(t) => self.seek_to(t),
            DialogOutcome::ExportCorrections(a) => self.set_export_corrections(a),
            DialogOutcome::ExportWideKeys(w) => {
                let o = ExportOptions {
                    wide_keys: w,
                    ..self.export_options
                };
                self.set_export_options(o);
            }
            DialogOutcome::OpenWideKeys(path, compat) => {
                self.dialog = None;
                self.start_open_as(path, KeyRange::Extended256, compat, ctx);
            }
            DialogOutcome::OpenCompatible(path, keys) => {
                self.dialog = None;
                self.start_open_as(path, keys, true, ctx);
            }
            DialogOutcome::ArchiveOpen(i) => {
                self.dialog = None;
                self.open_archive_entry(i, ctx);
            }
            DialogOutcome::Delete => {
                self.delete_from_dialog();
                self.dialog = None;
            }
            DialogOutcome::ExportStructure { split, metadata } => {
                let o = ExportOptions {
                    split_subchannels: split,
                    structure_metadata: metadata,
                    ..self.export_options
                };
                self.set_export_options(o);
            }
            DialogOutcome::AddMany(kind, n) => {
                self.dialog = None;
                match kind {
                    dialogs::AddManyKind::Subtracks(parent) => self.add_subtracks(parent, n),
                    dialogs::AddManyKind::Subchannels => self.add_subchannels(n),
                }
            }
            DialogOutcome::AddTracks(at, n) => {
                self.dialog = None;
                self.add_tracks(at, n);
            }
            DialogOutcome::Percent(kind, v) => {
                self.dialog = None;
                match kind {
                    dialogs::PercentKind::OnionOpacity => self.set_onion_opacity(v),
                    dialogs::PercentKind::SelectionOpacity => {
                        self.set_selection_opacity(v, ctx, true);
                    }
                    dialogs::PercentKind::FollowPosition => {
                        self.settings.follow_position = v.min(100);
                        self.save_settings();
                        self.set_status(
                            format!("Follow position: {v} % of the piano roll's width."),
                            StatusKind::Info,
                        );
                    }
                }
            }
            DialogOutcome::CreateSubchannel { track, id, channel } => {
                self.dialog = None;
                self.create_subchannel(track, id, channel);
            }
            DialogOutcome::EditSubchannel {
                track,
                id,
                name,
                channel,
                move_notes,
            } => match self.apply_subchannel_edit(track, id, &name, channel, move_notes) {
                Ok(()) => self.dialog = None,
                Err(msg) => {
                    if let Some(Dialog::EditSubchannel { error, .. }) = self.dialog.as_mut() {
                        *error = Some(msg);
                    }
                }
            },
            DialogOutcome::DeleteSubchannel { track, id } => {
                self.dialog = None;
                self.delete_subchannel(track, id);
            }
            DialogOutcome::MoveToSubchannel {
                number,
                set_channel,
            } => {
                let ti = self.active_track();
                let id = number
                    .trim()
                    .trim_start_matches('#')
                    .parse::<u64>()
                    .ok()
                    .and_then(plumspirit_engine::model::SubchannelId::from_number)
                    .filter(|id| {
                        self.doc()
                            .and_then(|d| d.track(ti))
                            .is_some_and(|t| t.subchannels().contains(*id))
                    });
                match id {
                    Some(id) => {
                        self.dialog = None;
                        self.move_selection_to_sub(id, set_channel);
                    }
                    None => {
                        if let Some(Dialog::MoveToSubchannel { error, .. }) = self.dialog.as_mut() {
                            *error = Some(
                                "This track has no subchannel with that number. Add it first (Tracks → Add Subchannel or the Subchannels window).".into(),
                            );
                        }
                    }
                }
            }
            DialogOutcome::PreviewToggled(on) => {
                self.settings.preview = on;
                self.save_settings();
            }
            DialogOutcome::MergeApply => self.apply_merge(),
        }
    }
}

/// The export dialog's structure section (v0.5): shown when the project has
/// subtracks or subchannels a plain MIDI file cannot hold.
fn export_structure(
    plan: &plumspirit_engine::midi::ExportPlan,
    options: ExportOptions,
) -> Option<super::dialogs::ExportStructure> {
    let r = &plan.structure;
    let relevant = r.subtracks > 0
        || r.extra_chunks > 0
        || r.merged_tracks > 0
        || r.metadata
        || (!options.structure_metadata && r.subchannels > 0 && r.tracks_over_16 > 0);
    if !relevant {
        return None;
    }
    Some(super::dialogs::ExportStructure {
        lines: plan.structure_summary(),
        split: options.split_subchannels,
        metadata: options.structure_metadata,
        has_shared_channels: r.extra_chunks > 0 || r.merged_tracks > 0,
    })
}

/// The key policy choice of the export dialog (v0.9 §061 / §062): shown when
/// the export meets keys above 127.
fn export_wide_keys(plan: &plumspirit_engine::midi::ExportPlan) -> Option<dialogs::ExportWideKeys> {
    let w = &plan.wide;
    w.any().then_some(dialogs::ExportWideKeys {
        notes: w.notes,
        raw_events: w.raw_events,
        highest: w.highest,
        policy: w.policy,
    })
}

/// Name and outcome of a finished job for the task log (`None`: recorded
/// elsewhere).
fn task_of(kind: &JobKind, output: &JobOutput) -> Option<(String, super::tasks::TaskOutcome)> {
    use super::tasks::TaskOutcome::{Cancelled, Done, Failed};
    let cancelled_text = |s: &str| s.to_ascii_lowercase().contains("cancel");
    Some(match (kind, output) {
        (JobKind::Import { name, .. }, JobOutput::Import(r)) => match r {
            Ok(_) => (format!("Imported {name}"), Done),
            Err(e) if matches!(e.kind, plumspirit_engine::midi::MidiErrorKind::Cancelled) => {
                ("Import".into(), Cancelled)
            }
            Err(_) => ("Import".into(), Failed),
        },
        (JobKind::OpenArchive { name, .. }, JobOutput::Archive(r)) => match r {
            Ok(_) => (format!("Unpacked {name}"), Done),
            Err(crate::archive::ArchiveError::Cancelled) => ("Unpack".into(), Cancelled),
            Err(_) => ("Unpack".into(), Failed),
        },
        (JobKind::LoadProject { name, .. }, JobOutput::LoadProject(r)) => match r {
            Ok(_) => (format!("Opened {name}"), Done),
            Err(e) if cancelled_text(&e.to_string()) => ("Open".into(), Cancelled),
            Err(_) => ("Open".into(), Failed),
        },
        (JobKind::Save { .. }, JobOutput::Save(r)) => match r {
            Ok(_) => ("Saved project".into(), Done),
            Err(e) if cancelled_text(&e.to_string()) => ("Save".into(), Cancelled),
            Err(_) => ("Save".into(), Failed),
        },
        (JobKind::Export { .. }, JobOutput::Export(r)) => match r {
            Ok(_) => ("Exported MIDI".into(), Done),
            Err(plumspirit_engine::midi::ExportError::Cancelled) => ("Export".into(), Cancelled),
            Err(_) => ("Export".into(), Failed),
        },
        (JobKind::Export { .. }, JobOutput::ExportStrict(r)) => match r {
            Ok(_) => ("Exported MIDI (strict)".into(), Done),
            Err(plumspirit_engine::midi::ExportError::Cancelled) => ("Export".into(), Cancelled),
            Err(_) => ("Export".into(), Failed),
        },
        // A successful edit is recorded when it is committed (with its note count).
        (JobKind::Edit { label }, JobOutput::Edit(r)) => match r {
            Ok(_) => return None,
            Err(EditError::Cancelled) => ((*label).to_owned(), Cancelled),
            Err(_) => ((*label).to_owned(), Failed),
        },
        // A tool is recorded when it is committed; cancelled / failed here.
        (JobKind::Tool { label, .. }, JobOutput::Tool(r)) => match r {
            Ok(_) => return None,
            Err(plumspirit_engine::transform::TransformError::Cancelled) => {
                ((*label).to_owned(), Cancelled)
            }
            Err(_) => ((*label).to_owned(), Failed),
        },
        // Import Into Track records its own task (reading is only the first step).
        _ => return None,
    })
}

/// The status line, its kind and its Details after a successful export of
/// `name`: what the plan converted, and (v0.9 V09-0102) the notes of one
/// channel and key that overlap, which readers may pair differently.
/// The status line and Details of a strict export (B1-0121 / B1-0123).
fn strict_status(name: &str, r: &StrictReport) -> (String, StatusKind, Vec<String>) {
    let what = if r.identical_to_source {
        "byte for byte the original file"
    } else {
        "the original file's events in their order"
    };
    let text = format!(
        "Export complete: {name} ({} notes, {}), strict — {what}, validated in {}",
        group_digits(r.notes),
        format_bytes(r.bytes),
        format_duration(r.write_duration + r.validate_duration)
    );
    let mut details = vec![
        "The project has no MIDI edits since it was imported, so it was written from the original file: every track, event and chunk in its order.".to_owned(),
    ];
    if r.status_bytes_saved > 0 {
        details.push(format!(
            "Running Status Compression left out {} status bytes.",
            group_digits(r.status_bytes_saved)
        ));
    }
    let kind = if r.normalised.is_empty() {
        StatusKind::Success
    } else {
        details.push("What the original file had that a MIDI writer cannot keep:".into());
        details.extend(r.normalised.iter().cloned());
        StatusKind::Info
    };
    (text, kind, details)
}

/// v1.0-Launch L-C02: Piano From Above multiplies every Note On velocity by
/// up to 127 ÷ the file's loudest velocity (its volume correction, at its
/// full volume), so a file whose loudest note is quiet plays loud there. Said
/// when that factor is 2 or more (loudest velocity 1–63); the file's bytes
/// are exact either way.
fn pfa_loudness_note(max_velocity: u8) -> Option<String> {
    if !(1..=63).contains(&max_velocity) {
        return None;
    }
    let factor = 127.0 / f64::from(max_velocity);
    Some(format!(
        "The loudest note has velocity {max_velocity}, and the file stores every velocity exactly. Piano From Above scales a file's velocities so that its loudest note plays at 127 (its volume correction, ×{factor:.1} here), so this file sounds much louder there than in PlumSpirit, which (like Kiva) plays the velocities as written. Give one note a higher velocity if Piano From Above should keep these notes quiet."
    ))
}

fn export_status(name: &str, report: &ExportReport) -> (String, StatusKind, Vec<String>) {
    let mut text = format!(
        "Export complete: {name} ({} notes, {}), validated in {}",
        group_digits(report.write.notes as u64),
        format_bytes(report.write.bytes),
        format_duration(report.write_duration + report.validate_duration)
    );
    let mut kind = StatusKind::Success;
    let mut details = report.plan.disclosures();
    if report.plan.wide.non_standard() {
        // v0.9 §062: never mistaken for a Standard MIDI file.
        kind = StatusKind::Warning;
        text.push_str(" · EXPERIMENTAL non-standard 256-key file");
    } else if !details.is_empty() {
        text.push_str(" · converted (see Details)");
    }
    if report.promoted_to_format1 {
        text.push_str(" · written as Format 1");
        details.push(
            "The source was a Format 0 file. Because the project now has more than one track, it was written as Format 1 (Format 0 can only hold a single track)."
                .into(),
        );
    }
    let reopen = &report.validation.tracks_with_different_pairing;
    let overlaps = same_key_disclosure(&report.write.same_key, reopen);
    if !reopen.is_empty() {
        // The validation re-paired the written file as PlumSpirit's import
        // does and found other lengths: say so on the status line.
        kind = StatusKind::Warning;
        text.push_str(" · some overlapping notes will reopen with other lengths");
    } else if overlaps.is_some() {
        text.push_str(" · overlapping notes on one key (see Details)");
    }
    // v1.0-Launch L-C04: the Program Change the player-safe start added.
    if let Some(ch) = report.plan.player_safe_start {
        text.push_str(" · Program Change 0 added for Kiva (see Details)");
        details.push(format!(
            "The project has no controller, program change or pitch bend, so one Program Change 0 (the General MIDI default instrument, which changes no sound) was written at the start on MIDI channel {}. Without any such message, Kiva's default Pre-render audio engine stops responding on a file whose notes are all below its Audio Note Threshold. No note, velocity or timing was changed. Settings → Import & Export turns this off.",
            ch + 1
        ));
    }
    // v1.0-Launch L-S51: the silent loudness reference; otherwise L-C02's
    // note on Piano From Above's volume correction.
    if let Some(r) = report.plan.loudness_reference {
        text.push_str(" · silent loudness reference added (see Details)");
        details.push(format!(
            "The song's loudest note has velocity {}, so one silent loudness reference was added for Piano From Above: a zero-length velocity-127 note (key {}, MIDI channel {}, a channel the song does not use) at the end of the first track, with that channel's volume (CC 7) set to 0 at the start. Piano From Above then plays every velocity as written instead of scaling the loudest note to 127. Players count it as one extra note; your project is not changed. Settings → Import & Export turns this off.",
            r.loudest,
            r.key,
            r.channel + 1
        ));
    } else if let Some(d) = pfa_loudness_note(report.write.max_note_velocity) {
        details.push(d);
    }
    if let Some(why) = &report.plan.loudness_reference_skipped {
        details.push(why.clone());
    }
    match overlaps {
        Some(s) => details.push(s),
        None if !reopen.is_empty() => {
            let tracks: Vec<String> = reopen.iter().map(|t| (t + 1).to_string()).collect();
            details.push(format!(
                "Track(s) {} contain overlapping notes with the same key and channel that a MIDI file cannot represent exactly. The file holds exactly the note events PlumSpirit wrote, but when it is reopened those notes are paired first-in-first-out, so some lengths change.",
                tracks.join(", ")
            ));
        }
        None => {}
    }
    (text, kind, details)
}

#[cfg(test)]
mod task_tests {
    use super::task_of;
    use crate::app::jobs::{self, RunningJob, Then};
    use crate::app::tasks::{TaskLog, TaskOutcome};
    use plumspirit_engine::history::History;
    use plumspirit_engine::model::project::KeyRange;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("plumspirit-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn doc(n: u64) -> PlumDocument {
        let mut d = PlumDocument::new_project("tasks".into(), Bpm::DEFAULT, 960);
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let recs: Vec<NoteRecord> = (0..n).map(|i| NoteRecord::new(i * 10, 30, a)).collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, true)
            .unwrap();
        d
    }

    /// Runs a job to completion and records it the way the app does.
    fn finish(mut job: RunningJob, log: &mut TaskLog) -> Option<TaskOutcome> {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(out) = job.poll().unwrap() {
                let r = task_of(&job.kind, &out);
                if let Some((name, outcome)) = r.clone() {
                    log.finish(name, job.started, outcome);
                }
                return r.map(|x| x.1);
            }
            assert!(std::time::Instant::now() < deadline, "job hung");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn load_save_export_import_and_edits_report_their_duration_and_outcome() {
        let ctx = egui::Context::default();
        let d = dir();
        let mut log = TaskLog::default();
        let doc = Arc::new(doc(200_000));
        // Save (success), then Open it (success).
        let lup = d.join("p.lup");
        let state = History::default().current_state();
        let job =
            jobs::spawn_save(1, doc.clone(), lup.clone(), Then::Nothing, state, &ctx).unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Done));
        assert!(
            log.last_text()
                .unwrap()
                .starts_with("Last: Saved project — ")
        );
        let job = jobs::spawn_load_project(2, lup.clone(), &ctx).unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Done));
        assert!(log.last().unwrap().name.starts_with("Opened p.lup"));
        // Export (success), then Import the exported file (success).
        let mid = d.join("p.mid");
        let snapshot = plumspirit_engine::midi::ExportSnapshot::take(&doc, Default::default());
        let job = jobs::spawn_export(3, snapshot, mid.clone(), &ctx).unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Done));
        assert_eq!(log.last().unwrap().name, "Exported MIDI");
        let job = jobs::spawn_import(4, mid.clone(), KeyRange::Standard, &ctx).unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Done));
        // Failures: opening a file that is not a project, importing garbage.
        let bad = d.join("bad.lup");
        std::fs::write(&bad, b"not a project").unwrap();
        let job = jobs::spawn_load_project(5, bad, &ctx).unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Failed));
        assert!(log.last().unwrap().text().starts_with("Open failed after "));
        let garbage = d.join("bad.mid");
        std::fs::write(&garbage, b"MThd garbage").unwrap();
        let job = jobs::spawn_import(6, garbage, KeyRange::Standard, &ctx).unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Failed));
        // Cancelling an import is reported as cancelled.
        let job = jobs::spawn_import(7, mid.clone(), KeyRange::Standard, &ctx).unwrap();
        job.cancel();
        let outcome = finish(job, &mut log);
        assert!(
            matches!(outcome, Some(TaskOutcome::Cancelled | TaskOutcome::Done)),
            "{outcome:?}"
        );
        // A large background transform: success is recorded at commit (None
        // here), a refusal is recorded as failed.
        let job = jobs::spawn_edit(
            8,
            doc.clone(),
            "Flip X",
            200_000,
            Box::new(|d, c| plumspirit_engine::editor::transforms::prepare_flip_x(d, 0, c)),
            &ctx,
        )
        .unwrap();
        assert_eq!(finish(job, &mut log), None);
        let job = jobs::spawn_edit(
            9,
            doc.clone(),
            "Scale X",
            200_000,
            Box::new(|d, c| {
                let f = plumspirit_engine::editor::ExactFactor::parse("-1").unwrap();
                plumspirit_engine::editor::transforms::prepare_scale_x(d, 0, &f, c)
            }),
            &ctx,
        )
        .unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Failed));
        assert!(
            log.last()
                .unwrap()
                .text()
                .starts_with("Scale X failed after ")
        );
        // Every record carries a real, monotonic duration.
        assert!(log.recent().all(|r| r.elapsed < Duration::from_secs(60)));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// §209: a tool computed as a job returns its run (committed and
    /// recorded by the app), reports Cancelled when stopped and Failed for a
    /// refused input — each with its duration.
    #[test]
    fn tool_jobs_return_their_run_and_report_cancelled_and_failed() {
        use crate::app::jobs::JobOutput;
        use crate::app::tools::{ToolFinish, ToolScope, compute, ctx_for, scopes_for};
        use plumspirit_engine::transform::ParamSet;
        let ctx = egui::Context::default();
        let mut log = TaskLog::default();
        let mut d = doc(100_000);
        d.track_mut(0).unwrap().notes.select_all();
        let doc = Arc::new(d);
        let def = plumspirit_engine::transform::command_tool("mute_notes").unwrap();
        let values = ParamSet::defaults(&(def.spec)().params);
        let scopes = scopes_for(&doc, 0, ToolScope::Selected);
        assert_eq!(scopes.len(), 1);
        let finish_kind = || ToolFinish::Recipe {
            recipe: crate::app::tools::Recipe::tool(def.id, values.clone(), 0),
            repeated: true,
            select: false,
        };
        let make = |cancel_first: bool| -> crate::app::jobs::ToolWork {
            let (values, scopes) = (values.clone(), scopes.clone());
            let kctx = ctx_for(&doc, None, 0, 0);
            Box::new(move |d, c| {
                if cancel_first {
                    c.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                compute(d, def, &values, &scopes, &kctx, Some(c))
            })
        };
        // Done: the run comes back (recorded later, at commit).
        let mut job = jobs::spawn_tool(
            1,
            doc.clone(),
            "Mute Notes",
            finish_kind(),
            100_000,
            make(false),
            &ctx,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let out = loop {
            if let Some(out) = job.poll().unwrap() {
                break out;
            }
            assert!(std::time::Instant::now() < deadline, "job hung");
            std::thread::sleep(Duration::from_millis(2));
        };
        match &out {
            JobOutput::Tool(Ok(run)) => {
                assert_eq!(run.source_count, 100_000);
                assert_eq!(run.notes.len(), 100_000);
            }
            _ => panic!("expected a tool run"),
        }
        assert!(task_of(&job.kind, &out).is_none());
        // Cancelled.
        let job = jobs::spawn_tool(
            2,
            doc.clone(),
            "Mute Notes",
            finish_kind(),
            100_000,
            make(true),
            &ctx,
        )
        .unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Cancelled));
        // Failed: a tool whose input is refused.
        let failing: crate::app::jobs::ToolWork = Box::new(|_, _| {
            Err(plumspirit_engine::transform::TransformError::Invalid(
                "refused".into(),
            ))
        });
        let job = jobs::spawn_tool(
            3,
            doc.clone(),
            "Mute Notes",
            finish_kind(),
            1,
            failing,
            &ctx,
        )
        .unwrap();
        assert_eq!(finish(job, &mut log), Some(TaskOutcome::Failed));
        assert!(
            log.last()
                .unwrap()
                .text()
                .starts_with("Mute Notes failed after ")
        );
    }
}

/// v0.9 §061 / §062: the export's key policy choice and the 256-key open.
#[cfg(test)]
mod wide_key_tests {
    use super::export_wide_keys;
    use crate::app::dialogs::{self, Dialog, DialogOutcome, ExportWideKeys};
    use crate::app::jobs::{self, JobOutput, RunningJob};
    use plumspirit_engine::midi::{ExportOptions, WideKeys, plan_export_with};
    use plumspirit_engine::model::project::KeyRange;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::time::Duration;

    /// Keys 60, 200 and 255 in a 256-Key Mode project (or `keys` only when
    /// `wide` is false).
    fn doc(wide: bool) -> PlumDocument {
        let mut d = PlumDocument::new_project("wide".into(), Bpm::DEFAULT, 480);
        d.project_mut().keys = KeyRange::Extended256;
        let keys: &[u8] = if wide { &[60, 200, 255] } else { &[60, 127] };
        let recs: Vec<NoteRecord> = keys
            .iter()
            .enumerate()
            .map(|(i, &k)| {
                let a = NoteAttrs::new_wide(k, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
                NoteRecord::new(i as u64 * 480, 240, a)
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    fn with(w: WideKeys) -> ExportOptions {
        ExportOptions {
            wide_keys: w,
            ..ExportOptions::default()
        }
    }

    #[test]
    fn the_key_choice_appears_only_when_the_export_meets_keys_above_127() {
        let std = PlumDocument::new_project("std".into(), Bpm::DEFAULT, 480);
        assert_eq!(
            export_wide_keys(&plan_export_with(&std, ExportOptions::default())),
            None
        );
        assert_eq!(
            export_wide_keys(&plan_export_with(&doc(false), ExportOptions::default())),
            None
        );
        let d = doc(true);
        let c = export_wide_keys(&plan_export_with(&d, ExportOptions::default())).unwrap();
        assert_eq!(
            (c.notes, c.raw_events, c.highest, c.policy),
            (2, 0, 255, WideKeys::Reject)
        );
        assert_eq!(
            c.summary(),
            "2 note(s) are above key 127 (the highest is 255). A Standard MIDI file holds keys 0–127 only. What happens to them?"
        );
        assert_eq!(c.button(), "Export…");
        for w in WideKeys::ALL {
            let plan = plan_export_with(&d, with(w));
            let c = export_wide_keys(&plan).unwrap();
            assert_eq!(c.policy, w);
            // Reject blocks; every other choice is written after approval.
            assert_eq!(plan.blockers.is_empty(), w != WideKeys::Reject, "{w:?}");
            assert_eq!(plan.needs_approval(), w != WideKeys::Reject, "{w:?}");
            assert!(!ExportWideKeys::tip(w).is_empty());
        }
        let e = export_wide_keys(&plan_export_with(&d, with(WideKeys::Experimental))).unwrap();
        assert_eq!(e.button(), "Export Experimental File…");
        assert!(ExportWideKeys::tip(WideKeys::Experimental).contains("NOT a Standard MIDI file"));
    }

    /// The dialogs draw without input (headless egui) and wait for a choice.
    #[test]
    fn the_dialogs_draw_and_wait_for_a_choice() {
        let d = doc(true);
        let plan = plan_export_with(&d, with(WideKeys::Experimental));
        let mut dialogs = [
            Dialog::ExportApproval {
                lines: plan.disclosures(),
                corrections: None,
                blocked: false,
                structure: None,
                wide: export_wide_keys(&plan),
            },
            Dialog::OpenWideKeys {
                path: "a.mid".into(),
                message: "A note at key 200 is above 127.".into(),
                compat: false,
            },
            Dialog::OpenCompatible {
                path: "a.mid".into(),
                message: "A length or delta-time value is longer than MIDI allows at byte 42,572 (track 2).".into(),
                keys: KeyRange::Standard,
            },
            Dialog::ArchiveChooser(Box::new(super::super::archive_open::ArchiveChooser {
                archive: "pack.7z".into(),
                rows: (0..40)
                    .map(|i| super::super::archive_open::ChooserRow {
                        logical: format!("songs/{i}.mid"),
                        size: 1 << 20,
                        info: crate::archive::SmfInfo {
                            format: Some(1),
                            tracks: Some(16),
                            notes: (i % 2 == 0).then_some(1_234_567),
                        },
                    })
                    .collect(),
                selected: 3,
                notes: vec![
                    "“pack.7z › x.mid” is encrypted (password protected) and was skipped.".into(),
                ],
            })),
        ];
        let ctx = egui::Context::default();
        for dialog in &mut dialogs {
            for _ in 0..2 {
                let mut outcome = None;
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 700.0),
                    )),
                    ..Default::default()
                };
                let mut full = ctx.run_ui(input, |ui| {
                    outcome = Some(dialogs::show(dialog, ui.ctx(), true));
                });
                full.textures_delta.clear();
                assert!(matches!(outcome, Some(DialogOutcome::Open)));
            }
        }
    }

    fn run(mut job: RunningJob) -> JobOutput {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(out) = job.poll().unwrap() {
                return out;
            }
            assert!(std::time::Instant::now() < deadline, "job hung");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// File → Open names an experimental file (the app then offers the
    /// 256-key open); the 256-key open gives a 256-Key Mode project with the
    /// same keys; Import Into Track reads it only for a 256-Key Mode project.
    #[test]
    fn an_experimental_file_is_named_on_open_and_opens_as_a_256_key_project() {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!("plumspirit-wide-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = doc(true);
        let path = dir.join("wide.mid");
        let snapshot =
            plumspirit_engine::midi::ExportSnapshot::take(&d, with(WideKeys::Experimental));
        let job = jobs::spawn_export(1, snapshot, path.clone(), &ctx).unwrap();
        assert!(matches!(run(job), JobOutput::Export(Ok(_))));
        let job = jobs::spawn_import(2, path.clone(), KeyRange::Standard, &ctx).unwrap();
        match run(job) {
            JobOutput::Import(Err(e)) => assert!(e.is_wide_key_file(), "{e:?}"),
            _ => panic!("the standard open must refuse the file"),
        }
        let job = jobs::spawn_import(3, path.clone(), KeyRange::Extended256, &ctx).unwrap();
        let JobOutput::Import(Ok((back, summary))) = run(job) else {
            panic!("the 256-key open must read the file");
        };
        assert_eq!(back.project().keys, KeyRange::Extended256);
        assert_eq!(summary.wide_notes, 2);
        let keys: Vec<u8> = back.tracks()[0]
            .notes
            .iter()
            .map(|n| n.attrs.pitch())
            .collect();
        assert_eq!(keys, vec![60, 200, 255]);
        let dest = back.tracks()[0].id();
        for (keys, ok) in [(KeyRange::Standard, false), (KeyRange::Extended256, true)] {
            let job = jobs::spawn_import_into(4, path.clone(), dest, keys, &ctx).unwrap();
            match run(job) {
                JobOutput::Import(r) => assert_eq!(r.is_ok(), ok, "{keys:?}"),
                _ => panic!("an import job returns an import"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod snapshot_tests {
    //! V09-S16 (§053): the Export dialog shows, and the export job writes and
    //! validates, one `ExportSnapshot`; an edit after the dialog opened
    //! takes a new one.
    use crate::app::commands::Command;
    use crate::app::dialogs::Dialog;
    use crate::app::jobs::JobOutput;
    use crate::app::test_support::Harness;
    use crate::ui::status_bar::StatusKind;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::midi::ExportOptions;
    use plumspirit_engine::model::track::TrackProps;
    use plumspirit_engine::model::{Bpm, ClipId, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::time::Duration;

    /// Two tracks; track 1 moved later and corrected (so the dialog opens).
    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("snap".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        for i in 0..2usize {
            let recs: Vec<NoteRecord> = (0..8u64)
                .map(|k| {
                    NoteRecord::new(
                        k * 480,
                        400,
                        NoteAttrs::new(50 + i as u8 + k as u8, 100, 0, 0, NoteOffKind::NoteOff)
                            .unwrap(),
                    )
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let mut h = History::default();
        let t0 = d.track(0).unwrap().id();
        let op =
            plumspirit_engine::editor::clips::move_clips(&d, &[(ClipId::Primary(t0), 0, 1920)])
                .unwrap();
        h.apply(&mut d, op).unwrap();
        h.apply(
            &mut d,
            Operation::TrackProps {
                track: t0,
                props: TrackProps {
                    tick_correction: 11,
                    ..TrackProps::default()
                },
                end: None,
                label: "Track Properties",
            },
        )
        .unwrap();
        d
    }

    #[test]
    fn the_export_job_writes_the_snapshot_the_dialog_showed() {
        let mut h = Harness::new(song());
        assert!(h.command(Command::Export));
        assert!(matches!(h.app.dialog, Some(Dialog::ExportApproval { .. })));
        let shown = h.app.pending_export.clone().expect("the dialog's snapshot");
        // The app's defaults: the engine's, with the player-safe start on
        // (v1.0-Launch L-C04, Settings → Import & Export).
        let app_defaults = ExportOptions {
            player_safe_start: true,
            ..ExportOptions::default()
        };
        assert!(shown.is_current(h.doc(), app_defaults));
        let dir = std::env::temp_dir().join(format!("plumspirit-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snap.mid");
        let ctx = h.ctx.clone();
        h.app.dialog = None;
        h.app.start_export(path.clone(), &ctx);
        assert!(h.app.pending_export.is_none());
        let mut job = h.app.job.take().expect("the export job");
        let out = loop {
            if let Some(out) = job.poll().unwrap() {
                break out;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let JobOutput::Export(Ok(report)) = out else {
            panic!("the export succeeds");
        };
        assert_eq!(report.plan, *shown.plan());
        assert_eq!(std::fs::read(&path).unwrap(), shown.to_bytes().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn finish(mut job: crate::app::jobs::RunningJob) -> JobOutput {
        loop {
            if let Some(out) = job.poll().unwrap() {
                return out;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// v1.0-beta1 B1-0121 / B1-0122 / B1-0123: an imported project without
    /// MIDI edits exports strictly — no approval dialog, the source's own
    /// bytes, Running Status Compression as set — and after an edit the
    /// normal export runs and says why it is not strict; a source that
    /// changed switches strict export off with a sentence.
    #[test]
    fn an_unchanged_import_exports_strictly_and_an_edit_labels_the_normal_export() {
        use plumspirit_engine::midi::provenance::{SourceStatus, strict_candidate};
        use plumspirit_engine::midi::strict::StatusBytes;
        let dir = std::env::temp_dir().join(format!("plumspirit-strict-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.mid");
        let mut plain = PlumDocument::new_project("plain".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..64u64)
            .map(|k| {
                NoteRecord::new(
                    k * 120,
                    100,
                    NoteAttrs::new(40 + (k % 30) as u8, 90, 0, 0, NoteOffKind::NoteOnZero).unwrap(),
                )
            })
            .collect();
        plain
            .track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let bytes = plumspirit_engine::midi::export_to_bytes(&plain).unwrap();
        std::fs::write(&src, &bytes).unwrap();
        let (doc, _) = plumspirit_engine::midi::import_file(&src, Default::default()).unwrap();
        let mut h = Harness::new(doc);
        let ctx = h.ctx.clone();

        assert!(h.command(Command::Export));
        assert_eq!(h.app.export_strict, Some(StatusBytes::Running));
        assert!(h.app.pending_export.is_none());
        assert!(!matches!(h.app.dialog, Some(Dialog::ExportApproval { .. })));
        h.app.dialog = None;
        let out = dir.join("out.mid");
        h.app.start_export(out.clone(), &ctx);
        let JobOutput::ExportStrict(Ok(report)) = finish(h.app.job.take().unwrap()) else {
            panic!("the strict export succeeds");
        };
        assert!(report.identical_to_source);
        assert_eq!(std::fs::read(&out).unwrap(), bytes);
        h.app.finish_export_strict(out.clone(), Ok(report), &ctx);
        let st = h.app.status.as_ref().unwrap();
        assert!(st.text.contains("strict — byte for byte"), "{}", st.text);

        // Running Status Compression off: the strict export writes every status.
        assert!(h.command(Command::ToggleExplicitStatus));
        assert!(h.command(Command::Export));
        assert_eq!(h.app.export_strict, Some(StatusBytes::Explicit));
        h.app.dialog = None;
        h.app.start_export(out.clone(), &ctx);
        let JobOutput::ExportStrict(Ok(r)) = finish(h.app.job.take().unwrap()) else {
            panic!("the strict export succeeds");
        };
        assert!(!r.identical_to_source && r.bytes > bytes.len() as u64);
        assert!(h.command(Command::ToggleExplicitStatus));

        // An edit: the normal export, labelled with the reason.
        let rec = NoteRecord::new(
            7,
            9,
            NoteAttrs::new(90, 64, 0, 0, NoteOffKind::NoteOff).unwrap(),
        );
        h.app
            .doc_mut()
            .unwrap()
            .track_mut(0)
            .unwrap()
            .notes
            .insert(rec, false)
            .unwrap();
        assert!(h.command(Command::Export));
        assert_eq!(h.app.export_strict, None);
        assert!(h.app.export_note.as_deref().unwrap().contains("MIDI edits"));
        h.app.dialog = None;
        h.app.start_export(out.clone(), &ctx);
        let JobOutput::Export(r) = finish(h.app.job.take().unwrap()) else {
            panic!("a normal export");
        };
        h.app.finish_export(out.clone(), r, &ctx);
        let st = h.app.status.as_ref().unwrap();
        assert!(
            st.text.contains("written from the project's contents"),
            "{}",
            st.text
        );
        assert!(st.details[0].contains("MIDI edits"), "{:?}", st.details);
        assert!(h.app.export_note.is_none());

        // The source changing (same size) is found by the strict writer:
        // the export fails with the reason, and the next one is normal.
        h.app.doc_mut().unwrap().track_mut(0).unwrap().notes = {
            let (d, _) = plumspirit_engine::midi::import_bytes(&bytes).unwrap();
            d.track(0).unwrap().notes.clone()
        };
        assert_eq!(strict_candidate(h.doc()), SourceStatus::Unchanged);
        let mut other = bytes.clone();
        let i = other.len() - 6;
        other[i] ^= 1;
        std::fs::write(&src, &other).unwrap();
        assert!(h.command(Command::Export));
        assert!(h.app.export_strict.is_some());
        h.app.dialog = None;
        h.app.start_export(out.clone(), &ctx);
        let JobOutput::ExportStrict(r) = finish(h.app.job.take().unwrap()) else {
            panic!("a strict export");
        };
        assert!(r.is_err());
        h.app.finish_export_strict(out.clone(), r, &ctx);
        assert!(h.app.strict_refused.is_some());
        assert!(h.command(Command::Export));
        assert_eq!(h.app.export_strict, None);
        assert!(
            h.app
                .export_note
                .as_deref()
                .unwrap()
                .contains("has changed")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_menu_switches_exports_to_explicit_status() {
        let mut h = Harness::new(song());
        assert!(!h.app.settings.export_explicit_status);
        assert!(h.command(Command::ToggleExplicitStatus));
        assert!(h.app.settings.export_explicit_status);
        assert!(h.command(Command::Export));
        let s = h.app.pending_export.clone().unwrap();
        assert!(s.options().explicit_status);
        assert!(s.plan().explicit_status);
        h.app.dialog = None;
        assert!(h.command(Command::ToggleExplicitStatus));
        assert!(h.command(Command::Export));
        assert!(
            !h.app
                .pending_export
                .clone()
                .unwrap()
                .options()
                .explicit_status
        );
    }

    /// v1.0-Launch L-C04 / L-C02: a project of quiet notes only (the owner's
    /// Test.lup kind) exports with one Program Change 0 at the start — on by
    /// default, said on the status line and in its details, with Piano From
    /// Above's volume correction explained; off, nothing is added. The notes
    /// and velocities are the same either way.
    #[test]
    fn a_quiet_notes_only_project_exports_kiva_safe_and_explains_piano_from_above() {
        use plumspirit_engine::model::RawKind;
        let dir = std::env::temp_dir().join(format!("plumspirit-kiva-safe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = PlumDocument::new_project("quiet".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..96u64)
            .map(|k| {
                NoteRecord::new(
                    k * 60,
                    60,
                    NoteAttrs::new((k % 128) as u8, 1, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let mut h = Harness::new(d);
        let ctx = h.ctx.clone();
        assert!(h.app.settings.export_player_safe_start, "on by default");
        let mut notes = Vec::new();
        for on in [true, false] {
            assert!(h.command(Command::Export));
            let shown = h.app.pending_export.clone().unwrap();
            assert_eq!(shown.options().player_safe_start, on);
            h.app.dialog = None;
            let out = dir.join(format!("quiet-{on}.mid"));
            h.app.start_export(out.clone(), &ctx);
            let JobOutput::Export(r) = finish(h.app.job.take().unwrap()) else {
                panic!("a normal export");
            };
            h.app.finish_export(out.clone(), r, &ctx);
            let (back, _) =
                plumspirit_engine::midi::import_bytes(&std::fs::read(&out).unwrap()).unwrap();
            let programs: Vec<(u64, u8, u8)> = back
                .tracks()
                .iter()
                .flat_map(|t| t.events.events().iter())
                .filter_map(|e| match e.kind {
                    RawKind::Channel { status, data } => Some((e.tick, status, data[0])),
                    _ => None,
                })
                .collect();
            assert_eq!(programs, if on { vec![(0, 0xC0, 0)] } else { vec![] });
            notes.push(back.track(0).unwrap().notes.iter().collect::<Vec<_>>());
            let st = h.app.status.as_ref().unwrap();
            assert_eq!(
                st.text.contains("Program Change 0 added for Kiva"),
                on,
                "{}",
                st.text
            );
            assert_eq!(
                st.details.iter().any(|x| x.contains("MIDI channel 1")),
                on,
                "{:?}",
                st.details
            );
            assert!(
                st.details
                    .iter()
                    .any(|x| x.contains("Piano From Above") && x.contains("×127.0")),
                "{:?}",
                st.details
            );
            assert!(h.command(Command::TogglePlayerSafeStart));
        }
        assert_eq!(notes[0], notes[1], "the same notes and velocities");
        assert_eq!(notes[0].len(), 96);
        assert!(h.app.settings.export_player_safe_start);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1.0-Launch L-S51: the loudness reference is off by default; on, a
    /// quiet project's export gets Channel Volume 0 on channel 16 and one
    /// zero-length velocity-127 note there (no Program Change: the reference
    /// is a control and an audio event), the status says so and the
    /// details name the channel instead of Piano From Above's factor.
    #[test]
    fn the_loudness_reference_is_opt_in_and_reported() {
        use plumspirit_engine::model::RawKind;
        let dir = std::env::temp_dir().join(format!("plumspirit-loudref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = PlumDocument::new_project("quiet".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..48u64)
            .map(|k| {
                NoteRecord::new(
                    k * 60,
                    60,
                    NoteAttrs::new(40 + (k % 24) as u8, 2, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let mut h = Harness::new(d);
        let ctx = h.ctx.clone();
        assert!(!h.app.settings.export_loudness_reference, "off by default");
        assert!(h.command(Command::ToggleLoudnessReference));
        assert!(h.app.settings.export_loudness_reference);
        assert!(h.command(Command::Export));
        assert!(
            h.app
                .pending_export
                .clone()
                .unwrap()
                .options()
                .loudness_reference
        );
        h.app.dialog = None;
        let out = dir.join("quiet.mid");
        h.app.start_export(out.clone(), &ctx);
        let JobOutput::Export(r) = finish(h.app.job.take().unwrap()) else {
            panic!("a normal export");
        };
        h.app.finish_export(out.clone(), r, &ctx);
        let (back, _) =
            plumspirit_engine::midi::import_bytes(&std::fs::read(&out).unwrap()).unwrap();
        let events: Vec<(u64, u8, u8, u8)> = back
            .tracks()
            .iter()
            .flat_map(|t| t.events.events().iter())
            .filter_map(|e| match e.kind {
                RawKind::Channel { status, data } => Some((e.tick, status, data[0], data[1])),
                _ => None,
            })
            .collect();
        assert_eq!(events, vec![(0, 0xBF, 7, 0)], "CC 7 = 0 on channel 16 only");
        let notes: Vec<_> = back
            .tracks()
            .iter()
            .flat_map(|t| t.notes.iter().collect::<Vec<_>>())
            .collect();
        assert_eq!(notes.len(), 49);
        let loud: Vec<_> = notes.iter().filter(|n| n.attrs.velocity() == 127).collect();
        assert_eq!(loud.len(), 1);
        assert_eq!(
            (
                loud[0].duration_ticks,
                loud[0].attrs.channel(),
                loud[0].attrs.pitch()
            ),
            (0, 15, 40)
        );
        let st = h.app.status.as_ref().unwrap();
        assert!(
            st.text.contains("silent loudness reference added"),
            "{}",
            st.text
        );
        assert!(!st.text.contains("Program Change 0 added"), "{}", st.text);
        assert!(
            st.details.iter().any(|x| x.contains("MIDI channel 16")),
            "{:?}",
            st.details
        );
        assert!(
            !st.details.iter().any(|x| x.contains("×63.5")),
            "{:?}",
            st.details
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_export_after_an_edit_takes_a_new_snapshot() {
        let mut h = Harness::new(song());
        assert!(h.command(Command::Export));
        let shown = h.app.pending_export.clone().unwrap();
        // The project changes before the destination is chosen.
        let d = h.app.doc_mut().unwrap();
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[NoteRecord::new(
                    9_000,
                    100,
                    NoteAttrs::new(90, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )],
                false,
            )
            .unwrap();
        assert!(!shown.is_current(h.doc(), ExportOptions::default()));
        let dir = std::env::temp_dir().join(format!("plumspirit-snap2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snap.mid");
        let ctx = h.ctx.clone();
        h.app.dialog = None;
        h.app.start_export(path.clone(), &ctx);
        let mut job = h.app.job.take().unwrap();
        let out = loop {
            if let Some(out) = job.poll().unwrap() {
                break out;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let JobOutput::Export(Ok(report)) = out else {
            panic!("the export succeeds");
        };
        assert_eq!(report.write.notes, shown.note_count() as usize + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_export_with_overlapping_notes_on_one_key_says_what_readers_get() {
        // V09-0102: track 1 has a note nested in a longer one on key 60,
        // track 2 a staggered pair on key 64.
        let mut d = PlumDocument::new_project("overlaps".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        let n = |s: u64, len: u64, key: u8| {
            NoteRecord::new(
                s,
                len,
                NoteAttrs::new(key, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            )
        };
        for (t, recs) in [
            (0, [n(0, 960, 60), n(240, 240, 60)]),
            (1, [n(0, 480, 64), n(240, 480, 64)]),
        ] {
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let mut h = Harness::new(d);
        let dir = std::env::temp_dir().join(format!("plumspirit-snap3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = h.ctx.clone();
        h.app.start_export(dir.join("overlaps.mid"), &ctx);
        let mut job = h.app.job.take().unwrap();
        let out = loop {
            if let Some(out) = job.poll().unwrap() {
                break out;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let JobOutput::Export(Ok(report)) = out else {
            panic!("the export succeeds");
        };
        let _ = std::fs::remove_dir_all(&dir);
        let (text, kind, details) = super::export_status("overlaps.mid", &report);
        assert_eq!(kind, StatusKind::Warning);
        assert!(
            text.ends_with(" · some overlapping notes will reopen with other lengths"),
            "{text}"
        );
        let s = details
            .iter()
            .find(|s| s.contains("same key and channel"))
            .expect("the overlap disclosure");
        assert!(
            s.starts_with("2 note(s) in tracks 1 and 2 start while"),
            "{s}"
        );
        assert!(s.contains(" 1 of them end before the note"), "{s}");
        assert!(
            s.ends_with(
                "PlumSpirit pairs some notes of track 1 differently, so their lengths change; the other tracks reopen exactly."
            ),
            "{s}"
        );
    }
}

/// B1-0166 / L-H04: memory a document takes for each byte of its MIDI
/// file, times two (3.5: measured — 20 bytes a note, Black MIDI files take
/// about 6 bytes a note; `docs/PERFORMANCE.md` "Huge files").
const MEMORY_PER_FILE_BYTE_X2: u64 = 7;

/// L-H04: under this much memory left to give out (an eighth of the
/// limit, at least 1 GiB) a just-opened project runs in low-memory mode.
fn low_memory_threshold(limit: u64) -> u64 {
    (limit / 8).max(1 << 30)
}

/// L-H04 low-memory mode: with less than [`low_memory_threshold`] left once
/// the document is in memory, the undo history keeps at most a quarter of
/// what is left (at least 16 MiB, never more than usual) and the opening
/// says so; `headroom` is `platform::commit_headroom_bytes`.
fn low_memory_limits(
    normal: plumspirit_engine::history::HistoryLimits,
    headroom: Option<(u64, u64)>,
) -> (plumspirit_engine::history::HistoryLimits, Option<String>) {
    let Some((left, limit)) = headroom.filter(|&(l, t)| l < low_memory_threshold(t)) else {
        return (normal, None);
    };
    let undo = usize::try_from(left / 4)
        .unwrap_or(usize::MAX)
        .max(16 << 20)
        .min(normal.max_bytes);
    let limits = plumspirit_engine::history::HistoryLimits {
        max_bytes: undo,
        ..normal
    };
    let gb = |b: u64| format!("{:.1} GB", b as f64 / 1e9);
    let note = format!(
        "Memory is nearly full: {} of {} is left with this project open. PlumSpirit keeps working in low-memory mode: Undo keeps at most {} of history. Very large edits, the MIDI Player or a second big file may not fit; save your work, close other programs, or open a smaller file.",
        gb(left),
        gb(limit),
        format_bytes(undo as u64)
    );
    (limits, Some(note))
}

/// L-H04: the name of an import / load that ended canceled (the memory
/// guard's stop is reported as such, not as "Loading canceled.").
fn memory_stopped(kind: &JobKind, output: &JobOutput) -> Option<String> {
    match (kind, output) {
        (
            JobKind::Import { name, .. } | JobKind::ImportInto { name, .. },
            JobOutput::Import(Err(e)),
        ) if e.is_cancelled() => Some(name.clone()),
        (JobKind::LoadProject { name, .. }, JobOutput::LoadProject(Err(LupError::Cancelled))) => {
            Some(name.clone())
        }
        _ => None,
    }
}

/// B1-0170: frees `x` on a short-lived thread of its own: a 44M-note
/// project is hundreds of thousands of allocations, and the window never
/// waits for them (closing a project, opening another).
pub(super) fn reap<T: Send + 'static>(x: T) {
    drop(reap_work(move || drop(x)));
}

pub(super) struct ReleaseTask {
    done: std::sync::mpsc::Receiver<()>,
    reaped: bool,
    document: std::sync::Weak<PlumDocument>,
    started: std::time::Instant,
    estimate: usize,
    /// L-H03: the GUI's frames while the cleanup runs — the last poll and
    /// the longest time between two (the field measure of "the UI lags
    /// while memory is released"; idle frames come every 30 ms).
    last_poll: std::time::Instant,
    worst_gap: std::time::Duration,
}

fn reap_work(work: impl FnOnce() + Send + 'static) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("plumspirit-reaper".into())
        .spawn(move || {
            work();
            let _ = tx.send(());
        });
    if let Err(e) = spawned {
        // The closure (and `x`) was dropped here, inline.
        log::warn!("could not free the previous project in the background ({e})");
    }
    rx
}

/// L-H04: an import the memory guard stops is reported honestly and leaves
/// the open project as it was.
#[cfg(test)]
mod memory_guard_tests {
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::fixtures::{FixtureShape, generate};
    use std::time::{Duration, Instant};

    #[test]
    fn low_memory_mode_shrinks_undo_and_says_so_only_when_memory_is_short() {
        use plumspirit_engine::history::HistoryLimits;
        let normal = HistoryLimits::for_system_memory(Some(8 << 30));
        assert_eq!(normal.max_bytes, 512 << 20);
        // Plenty (or unknown): unchanged, nothing said.
        assert_eq!(
            low_memory_limits(normal, Some((4 << 30, 8 << 30))),
            (normal, None)
        );
        assert_eq!(low_memory_limits(normal, None), (normal, None));
        // 715 MiB of 5,979 MiB (the 250M probe): a quarter of what is left.
        let (l, note) = low_memory_limits(normal, Some((715 << 20, 5_979 << 20)));
        assert_eq!(l.max_bytes, (715 << 20) / 4);
        assert_eq!(l.max_entries, normal.max_entries);
        let note = note.unwrap();
        assert!(
            note.starts_with("Memory is nearly full: 0.7 GB of 6.3 GB is left"),
            "{note}"
        );
        assert!(note.contains("low-memory mode"), "{note}");
        // Almost nothing: at least 16 MiB; never more than usual.
        assert_eq!(
            low_memory_limits(normal, Some((10 << 20, 8 << 30)))
                .0
                .max_bytes,
            16 << 20
        );
        let small = HistoryLimits {
            max_bytes: 8 << 20,
            ..normal
        };
        assert_eq!(
            low_memory_limits(small, Some((100 << 20, 8 << 30)))
                .0
                .max_bytes,
            8 << 20
        );
        // 32 GB machines: under 4 GB left.
        assert_eq!(low_memory_threshold(32 << 30), 4 << 30);
        assert_eq!(low_memory_threshold(4 << 30), 1 << 30);
    }

    #[test]
    fn an_import_stopped_for_memory_says_so_and_keeps_the_open_project() {
        let mut h = Harness::new(generate(FixtureShape::Dense, 2_000, 960, 3));
        let before = h.doc().export_key();
        let dir = std::env::temp_dir().join(format!("plumspirit-l-h04-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.mid");
        std::fs::write(
            &path,
            plumspirit_engine::midi::export_to_bytes(&generate(
                FixtureShape::Dense,
                2_000_000,
                960,
                4,
            ))
            .unwrap(),
        )
        .unwrap();
        h.app.job = Some(
            jobs::spawn_import(9, path.clone(), KeyRange::Standard, &h.ctx)
                .unwrap()
                .guard_memory(|| Some((300 << 20, 32 << 30))),
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        while h.app.job.is_some() {
            h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
            assert!(Instant::now() < deadline, "the import never ended");
            std::thread::sleep(Duration::from_millis(2));
        }
        let Some(Dialog::Message {
            title,
            body,
            is_error,
        }) = &h.app.dialog
        else {
            panic!(
                "an error message, got {:?}",
                h.app.status.as_ref().map(|s| &s.text)
            );
        };
        assert!(*is_error);
        assert_eq!(title, "Not enough memory to open the file.");
        for words in [
            "big.mid",
            "0.3 GB of memory left",
            "It stops at 0.7 GB",
            "Nothing was changed.",
            "still open",
            "every note takes about 20 bytes",
        ] {
            assert!(body.contains(words), "{words:?} in {body}");
        }
        assert_eq!(
            h.doc().export_key(),
            before,
            "the open project is unchanged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L-H04 probe: opens `PLUMSPIRIT_BENCH_HUGE_FILE` the way File → Open
    /// does (the real memory guard) and reports the outcome — opened (bytes
    /// a note of the document, the analysis index and the process) or
    /// stopped for memory — with the time and the peak resident memory.
    #[test]
    #[ignore = "measurement: PLUMSPIRIT_BENCH_HUGE_FILE=a.mid cargo test --release -p plumspirit huge_file_memory_probe -- --ignored --nocapture"]
    fn huge_file_memory_probe() {
        let path = std::path::PathBuf::from(
            std::env::var("PLUMSPIRIT_BENCH_HUGE_FILE").expect("PLUMSPIRIT_BENCH_HUGE_FILE"),
        );
        let status = |k: &str| {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find(|l| l.starts_with(k))
                        .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                })
                .map_or(0.0, |kib| kib as f64 / 1024.0)
        };
        let mut h = Harness::new(PlumDocument::new_project("idle".into(), Bpm::DEFAULT, 960));
        h.app.settings.inspector = true;
        h.app.settings.progressor.enabled = true;
        let size = egui::vec2(1280.0, 900.0);
        h.app_frame(size, 1.0);
        let base = status("VmRSS:");
        let t = Instant::now();
        let ctx = h.ctx.clone();
        h.app.start_open(path.clone(), &ctx);
        while h.app.job.is_some() {
            h.app_frame(size, 1.0);
            std::thread::sleep(Duration::from_millis(5));
        }
        let took = t.elapsed();
        let headroom = crate::platform::commit_headroom_bytes().unwrap_or((0, 0));
        if let Some(Dialog::Message { title, body, .. }) = &h.app.dialog {
            println!(
                "huge probe {}: {title} after {:.1} s; peak RSS {:.0} MiB (baseline {:.0}); headroom now {} MiB of {} MiB\n{body}",
                path.display(),
                took.as_secs_f64(),
                status("VmHWM:"),
                base,
                headroom.0 >> 20,
                headroom.1 >> 20
            );
            return;
        }
        let notes = h.doc().note_count().max(1) as f64;
        let doc = h.doc().heap_bytes() as f64;
        let loaded = status("VmRSS:");
        let deadline = Instant::now() + Duration::from_secs(600);
        while h.app.analysis.index().is_none() && Instant::now() < deadline {
            h.app_frame(size, 1.0);
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..20 {
            h.app_frame(size, 1.0);
        }
        let analysis = h.app.analysis.heap_bytes() as f64;
        if let Some(st) = &h.app.status {
            println!(
                "huge probe status [{:?}]: {} | {}",
                st.kind,
                st.text,
                st.details.first().map_or("", String::as_str)
            );
        }
        println!(
            "huge probe {}: opened {} notes in {:.1} s; document {:.2} B/note ({:.0} MiB), analysis index {:.2} B/note; RSS loaded {:.0} MiB = {:.2} B/note, ready {:.0} MiB, peak {:.0} MiB (baseline {:.0}); headroom now {} MiB of {} MiB",
            path.display(),
            notes,
            took.as_secs_f64(),
            doc / notes,
            doc / 1048576.0,
            analysis / notes,
            loaded,
            (loaded - base) * 1048576.0 / notes,
            status("VmRSS:"),
            status("VmHWM:"),
            base,
            headroom.0 >> 20,
            headroom.1 >> 20
        );
    }
}

/// B1-0170: closing never waits on the project's size.
#[cfg(test)]
mod close_tests {
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::fixtures::{FixtureShape, generate};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn ready(h: &mut Harness) {
        h.app.settings.inspector = true;
        h.app.settings.progressor.enabled = true;
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
            if h.app
                .inspector
                .drawn
                .as_ref()
                .is_some_and(|(_, busy)| !busy)
            {
                break;
            }
            assert!(Instant::now() < deadline, "analysis never ready");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn released(h: &mut Harness) {
        let deadline = Instant::now() + Duration::from_secs(300);
        while !h.app.project_releases.is_empty() {
            h.app.poll_project_releases(&h.ctx);
            assert!(Instant::now() < deadline, "project release never completed");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !h.app
                .status
                .as_ref()
                .is_some_and(|s| s.kind == StatusKind::Warning),
            "{:?}",
            h.app.status.as_ref().map(|s| &s.text)
        );
        h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
    }

    #[test]
    fn close_project_releases_document_indexes_snapshots_history_and_textures() {
        let mut h = Harness::new(generate(FixtureShape::Dense, 30_000, 960, 7));
        ready(&mut h);
        let doc = Arc::downgrade(&h.app.open.as_ref().unwrap().doc);
        let index = Arc::downgrade(h.app.analysis.index().unwrap());
        let snapshot = Arc::downgrade(&h.app.player.current().unwrap().snapshot);
        let profile = Arc::downgrade(
            &h.app
                .analysis
                .profile(
                    &plumspirit_engine::analysis::Scope::Project,
                    crate::inspector::WINDOW_NS,
                )
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let tile_ids = loop {
            let tiles: Vec<_> = h
                .ctx
                .tex_manager()
                .read()
                .allocated()
                .filter(|(_, meta)| meta.name.starts_with("tile-"))
                .map(|(id, _)| *id)
                .collect();
            if !tiles.is_empty() {
                break tiles;
            }
            assert!(
                Instant::now() < deadline,
                "fixture must populate the actual tile cache"
            );
            h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
            std::thread::sleep(Duration::from_millis(2));
        };
        h.app.close_document();
        assert!(h.app.open.is_none());
        assert!(h.app.player.current().is_none());
        assert!(h.app.analysis.index().is_none());
        assert!(h.app.inspector.last.is_none());
        assert!(h.app.clipboard.is_none() && h.app.clip_clipboard.is_none());
        released(&mut h);
        assert_eq!(
            (
                doc.strong_count(),
                index.strong_count(),
                snapshot.strong_count(),
                profile.strong_count()
            ),
            (0, 0, 0, 0)
        );
        assert!(
            h.ctx
                .tex_manager()
                .read()
                .allocated()
                .all(|(id, _)| !tile_ids.contains(id)),
            "every project tile texture must be freed"
        );
        let status = h.app.status.as_ref().unwrap();
        assert_eq!(status.text, "Project data released.");
        assert!(status.details.iter().any(|s| s.contains("allocator")));
        assert!(
            status
                .details
                .iter()
                .any(|s| s.contains("longest time between two frames")),
            "{:?}",
            status.details
        );
    }

    #[test]
    fn close_cancels_analysis_and_loading_and_reopening_keeps_the_new_document() {
        let mut h = Harness::new(generate(FixtureShape::Dense, 200_000, 960, 3));
        let doc = Arc::downgrade(&h.app.open.as_ref().unwrap().doc);
        // Start real analysis/snapshot workers and close without waiting for them.
        h.app.settings.inspector = true;
        h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
        h.app.close_document();
        h.app.install_document(
            PlumDocument::new_project("next".into(), Bpm::DEFAULT, 960),
            DocSource::New,
            true,
        );
        released(&mut h);
        assert_eq!(h.doc().project().name, "next");
        assert_eq!(doc.strong_count(), 0);
        assert!(
            !h.app
                .status
                .as_ref()
                .is_some_and(|s| s.text == "Project data released.")
        );
    }

    /// L-H03: a project opened while the closed one is still being
    /// released gets only its own results — the closed project's Player
    /// snapshot, analysis and Inspector work never land in it — and the
    /// release reports its frame gaps.
    #[test]
    fn a_project_opened_during_the_release_gets_only_its_own_results() {
        let mut h = Harness::new(generate(FixtureShape::Dense, 400_000, 960, 5));
        h.app.settings.inspector = true;
        h.app.settings.progressor.enabled = true;
        let old = Arc::downgrade(&h.app.open.as_ref().unwrap().doc);
        // Workers started, not finished.
        h.app_frame(egui::vec2(1280.0, 900.0), 1.0);
        h.app.close_document();
        h.app.install_document(
            generate(FixtureShape::Dense, 3_000, 480, 9),
            DocSource::New,
            true,
        );
        ready(&mut h);
        released(&mut h);
        assert_eq!(old.strong_count(), 0);
        let (key, notes) = (h.doc().export_key(), h.doc().note_count());
        assert_eq!(h.app.player.current().unwrap().snapshot.key(), key);
        assert_eq!(notes, 3_000);
        assert_eq!(h.app.analysis.index().unwrap().total_notes(), notes as u64);
        assert!(
            !h.app
                .status
                .as_ref()
                .is_some_and(|s| s.text == "Project data released.")
        );
    }

    #[test]
    fn cleanup_completion_waits_for_outstanding_project_owners() {
        let mut h = Harness::new(generate(FixtureShape::Dense, 2_000, 960, 3));
        let held = h.app.open.as_ref().unwrap().doc.clone();
        h.app.close_document();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !h.app.project_releases[0].reaped {
            h.app.poll_project_releases(&h.ctx);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(h.app.project_releases.len(), 1);
        assert!(h.app.status.as_ref().unwrap().text.contains("Releasing"));
        drop(held);
        released(&mut h);
        assert_eq!(
            h.app.status.as_ref().unwrap().text,
            "Project data released."
        );
    }

    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit close_project_memory_cycles -- --ignored --nocapture --test-threads=1"]
    fn close_project_memory_cycles() {
        let mut h = Harness::new(PlumDocument::new_project("idle".into(), Bpm::DEFAULT, 960));
        ready(&mut h);
        h.app.close_document();
        released(&mut h);
        let mb = |v: Option<u64>| v.unwrap_or(0) as f64 / 1048576.0;
        let memory = || {
            #[cfg(windows)]
            {
                (
                    mb(diagnostics::process_memory_bytes()),
                    mb(crate::platform::private_commit_bytes()),
                )
            }
            #[cfg(not(windows))]
            {
                (mb(diagnostics::process_memory_bytes()), 0.0)
            }
        };
        let baseline = memory();
        for n in [1_000_000, 10_000_000, 40_000_000] {
            for cycle in 1..=2 {
                h.app.install_document(
                    generate(FixtureShape::Dense, n, 960, 7),
                    DocSource::New,
                    true,
                );
                ready(&mut h);
                let doc = Arc::downgrade(&h.app.open.as_ref().unwrap().doc);
                let index = Arc::downgrade(h.app.analysis.index().unwrap());
                let snapshot = Arc::downgrade(&h.app.player.current().unwrap().snapshot);
                let loaded = memory();
                let t = Instant::now();
                h.app.close_document();
                let ui = t.elapsed();
                released(&mut h);
                let complete = t.elapsed();
                assert_eq!(
                    (
                        doc.strong_count(),
                        index.strong_count(),
                        snapshot.strong_count()
                    ),
                    (0, 0, 0)
                );
                std::thread::sleep(Duration::from_millis(200));
                let closed = memory();
                println!(
                    "project close {n} cycle {cycle}: UI {:.3} ms, cleanup {:.3} ms; RSS baseline/loaded/closed {:.2}/{:.2}/{:.2} MiB; private commit {:.2}/{:.2}/{:.2} MiB; live document/index/snapshot 0/0/0",
                    ui.as_secs_f64() * 1000.0,
                    complete.as_secs_f64() * 1000.0,
                    baseline.0,
                    loaded.0,
                    closed.0,
                    baseline.1,
                    loaded.1,
                    closed.1
                );
            }
        }
    }

    /// L-H03 ("ui lags pretty noticeably while memory is being released"):
    /// the GUI's frames while a closed project is released, against the same
    /// frames once it is gone. A Black-MIDI-shaped project: `TRACKS` tracks of
    /// `NOTES` notes, or `PLUMSPIRIT_BENCH_CLOSE_FILE` (a MIDI file) imported.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit close_frame_cost_while_releasing -- --ignored --nocapture --test-threads=1"]
    fn close_frame_cost_while_releasing() {
        let env = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let doc = || match std::env::var("PLUMSPIRIT_BENCH_CLOSE_FILE") {
            Ok(path) => {
                plumspirit_engine::midi::import_file(
                    std::path::Path::new(&path),
                    plumspirit_engine::midi::ImportControl::default(),
                )
                .expect("the MIDI file")
                .0
            }
            Err(_) => {
                let tracks = env("PLUMSPIRIT_BENCH_CLOSE_TRACKS", 2_000);
                let per = env("PLUMSPIRIT_BENCH_CLOSE_NOTES", 10_000) as u64;
                let mut d = PlumDocument::new_project("c".into(), Bpm::DEFAULT, 960);
                for _ in 1..tracks {
                    d.add_track().unwrap();
                }
                for ti in 0..tracks {
                    let recs: Vec<_> = (0..per)
                        .map(|k| {
                            use plumspirit_engine::model::note::{
                                NoteAttrs, NoteOffKind, NoteRecord,
                            };
                            NoteRecord::new(
                                k * 8 + (ti as u64 % 8),
                                4,
                                NoteAttrs::new(
                                    20 + ((k + ti as u64) % 90) as u8,
                                    1 + (ti % 126) as u8,
                                    (ti % 16) as u8,
                                    0,
                                    NoteOffKind::NoteOff,
                                )
                                .unwrap(),
                            )
                        })
                        .collect();
                    d.track_mut(ti)
                        .unwrap()
                        .notes
                        .insert_sorted_batch(&recs, false)
                        .unwrap();
                }
                d
            }
        };
        let size = egui::vec2(1280.0, 900.0);
        let mut h = Harness::new(PlumDocument::new_project("idle".into(), Bpm::DEFAULT, 960));
        ready(&mut h);
        for cycle in 1..=env("PLUMSPIRIT_BENCH_CLOSE_CYCLES", 2) {
            let d = doc();
            let notes = d.note_count();
            h.app.install_document(d, DocSource::New, true);
            // `_EARLY`: close while the project's workers (analysis, player
            // snapshot, tiles, Inspector) are still busy, as right after an
            // open; otherwise once everything is ready.
            let early = env("PLUMSPIRIT_BENCH_CLOSE_EARLY", 0) != 0;
            if !early {
                ready(&mut h);
            }
            for _ in 0..if early { 3 } else { 30 } {
                h.app_frame(size, 1.0);
            }
            let t = Instant::now();
            h.app.close_document();
            let close = t.elapsed();
            let mut during = Vec::new();
            while !h.app.project_releases.is_empty() {
                let f = Instant::now();
                h.app_frame(size, 1.0);
                during.push(f.elapsed());
                assert!(t.elapsed() < Duration::from_secs(600), "never released");
            }
            let release = t.elapsed();
            let mut after: Vec<_> = (0..200)
                .map(|_| {
                    let f = Instant::now();
                    h.app_frame(size, 1.0);
                    f.elapsed()
                })
                .collect();
            during.sort();
            after.sort();
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            let pct = |v: &[Duration], p: f64| {
                v.get(((v.len() as f64 - 1.0) * p).round() as usize)
                    .copied()
                    .map_or(0.0, ms)
            };
            println!(
                "close {notes} notes cycle {cycle}: close() {:.2} ms, released in {:.0} ms; frames during {} p50 {:.2} p95 {:.2} max {:.2} ms; after p50 {:.2} p95 {:.2} max {:.2} ms",
                ms(close),
                ms(release),
                during.len(),
                pct(&during, 0.5),
                pct(&during, 0.95),
                pct(&during, 1.0),
                pct(&after, 0.5),
                pct(&after, 0.95),
                pct(&after, 1.0),
            );
        }
    }

    /// `reap` frees on a thread of its own, never the caller's.
    #[test]
    fn reap_frees_on_its_own_thread() {
        struct Probe(Arc<Mutex<Option<String>>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = std::thread::current().name().map(str::to_owned);
            }
        }
        let seen = Arc::new(Mutex::new(None));
        reap(Probe(Arc::clone(&seen)));
        let deadline = Instant::now() + Duration::from_secs(10);
        while seen.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "never freed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(seen.lock().unwrap().as_deref(), Some("plumspirit-reaper"));
    }

    /// The exit path: settings saved, playback stopped, the journal closed,
    /// workers told to stop, the project and caches handed to the system —
    /// in that order, every stage timed; nothing is left to free when the
    /// app is dropped after it.
    #[test]
    fn shutting_down_stops_everything_and_frees_nothing_on_the_gui_thread() {
        let mut h = Harness::new(generate(FixtureShape::Dense, 200_000, 960, 3));
        h.app.settings.progressor.enabled = true;
        let screen = egui::Vec2::new(1280.0, 720.0);
        for _ in 0..3 {
            h.app_frame(screen, 1.0);
        }
        let weak = Arc::downgrade(&h.app.open.as_ref().unwrap().doc);
        let stages = h.app.shutdown_bounded();
        let names: Vec<&str> = stages.iter().map(|s| s.0).collect();
        assert_eq!(
            names,
            [
                "settings saved",
                "playback stopped (all notes off)",
                "undo journal closed",
                "workers told to stop",
                "project memory left to the system",
                "sampler stopped"
            ]
        );
        assert!(h.app.open.is_none());
        // The document was not freed (left to the system at exit).
        assert!(weak.upgrade().is_some(), "not torn down on the GUI thread");
        let t = Instant::now();
        drop(h);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    /// B1-0170 measurement: closing a huge, fully analysed project the old
    /// way (everything freed and every worker joined while the window is
    /// still there) against the bounded exit.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit close_cost -- --ignored --nocapture"]
    fn close_cost() {
        let settled = |n: usize| -> Harness {
            let mut h = Harness::new(generate(FixtureShape::Dense, n, 960, 7));
            h.app.settings.progressor.enabled = true;
            let screen = egui::Vec2::new(1280.0, 720.0);
            let deadline = Instant::now() + Duration::from_secs(600);
            loop {
                h.app_frame(screen, 1.0);
                let (doc, generation) = {
                    let o = h.app.open.as_ref().unwrap();
                    (o.doc.clone(), o.generation)
                };
                h.app.analysis.sync(&doc, generation);
                let ready = h.app.analysis.index().is_some()
                    && h.app
                        .analysis
                        .profile(
                            &plumspirit_engine::analysis::Scope::Project,
                            crate::inspector::WINDOW_NS,
                        )
                        .is_some();
                if ready {
                    return h;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        for n in [1_000_000usize, 10_000_000, 40_000_000] {
            let d = generate(FixtureShape::Dense, n, 960, 7);
            let t = Instant::now();
            drop(d);
            let freeing = t.elapsed();
            let h = settled(n);
            let t = Instant::now();
            drop(h);
            let old = t.elapsed();
            let mut h = settled(n);
            let t = Instant::now();
            let stages = h.app.shutdown_bounded();
            let exit = t.elapsed();
            let t = Instant::now();
            drop(h);
            let rest = t.elapsed();
            println!(
                "close cost {n} notes (analyzed): document alone {:.1} ms; old teardown (all freed, workers joined) {:.1} ms; bounded exit {:.2} ms ({}) + app drop {:.2} ms",
                freeing.as_secs_f64() * 1e3,
                old.as_secs_f64() * 1e3,
                exit.as_secs_f64() * 1e3,
                stages
                    .iter()
                    .map(|(s, d)| format!("{s} {:.2}", d.as_secs_f64() * 1e3))
                    .collect::<Vec<_>>()
                    .join(", "),
                rest.as_secs_f64() * 1e3
            );
        }
    }
}
