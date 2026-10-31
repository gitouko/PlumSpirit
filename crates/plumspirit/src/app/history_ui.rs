//! Persistent undo history in the app (v0.8 §084–§093): one journal per
//! project in `<config>/history/` (named by the project path), restored when
//! the project opens, following it on Save As; the Undo History window (every
//! step, jump back / forward, disk usage, Compact / Clear).

use super::{PlumSpiritApp, StatusKind};
use crate::diagnostics::format_bytes;
use egui::RichText;
use plumspirit_engine::history::journal::{self, Journal, JournalProblem};
use std::path::{Path, PathBuf};

/// Above this the window suggests Compact / Clear (nothing is deleted
/// automatically: the default keeps all work).
const LARGE_HISTORY: u64 = 1 << 30;

fn journal_path(dir: &Path, key: u64) -> PathBuf {
    dir.join(format!("{key:016x}.plh"))
}

fn is_untitled(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("untitled-"))
}

/// Keeps an unusable journal next to the new one (nothing is deleted).
fn set_aside(path: &Path, problem: &JournalProblem) -> String {
    let secs = crate::recent::now_secs();
    let aside = path.with_extension(format!("plh.old-{secs}"));
    let kept = match std::fs::rename(path, &aside) {
        Ok(()) => format!(
            " It was kept as “{}”.",
            aside
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ),
        Err(_) => String::new(),
    };
    format!("The undo history could not be restored: {problem}.{kept} A new history starts now.")
}

/// Bytes used by every journal (all projects).
pub fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Removes journals of unsaved projects left by an earlier session.
pub fn remove_untitled(config: &Path) {
    let dir = config.join("history");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(Result::ok) {
            if is_untitled(&e.path()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

impl PlumSpiritApp {
    fn history_dir(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|d| d.join("history"))
    }

    /// Attaches (and for a project file restores) the open document's
    /// journal. Returns a note for the status line and a warning for its
    /// details.
    pub(super) fn attach_history(
        &mut self,
        project: Option<&Path>,
        identity: Option<u64>,
    ) -> (Option<String>, Option<String>) {
        // §210: a restored history is a user-visible task with its time.
        let started = std::time::Instant::now();
        let (note, warn) = self.attach_history_inner(project, identity);
        if let Some(n) = &note {
            let mut name = n.clone();
            if let Some(first) = name.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            self.tasks
                .record(name, started.elapsed(), super::tasks::TaskOutcome::Done);
        }
        (note, warn)
    }

    fn attach_history_inner(
        &mut self,
        project: Option<&Path>,
        identity: Option<u64>,
    ) -> (Option<String>, Option<String>) {
        let Some(dir) = self.history_dir() else {
            return (None, None);
        };
        let Some(o) = self.open.as_mut() else {
            return (None, None);
        };
        o.saved_identity = identity;
        let Some(p) = project else {
            let path = dir.join(format!(
                "untitled-{}-{}.plh",
                std::process::id(),
                o.generation
            ));
            match Journal::create(&path, 0) {
                Ok(j) => {
                    if let Err(e) = o.history.attach_new_journal(j, None) {
                        log::warn!("undo history journal: {e}");
                    }
                }
                Err(e) => log::warn!(
                    "could not create the undo history at {}: {e}",
                    path.display()
                ),
            }
            return (None, None);
        };
        let key = journal::path_key(&crate::recent::canonical(p));
        let path = journal_path(&dir, key);
        let id = identity.unwrap_or(0);
        let mut warn = None;
        if path.is_file() {
            match Journal::open(&path, key) {
                Ok(Ok((j, scan))) => match o.history.restore_journal(j, scan, id) {
                    Ok(r) => {
                        log::info!("undo history restored from {}: {r:?}", path.display());
                        let note = (r.undo_steps + r.redo_steps > 0).then(|| {
                            let mut n = format!("undo history restored ({} step(s)", r.undo_steps);
                            if r.unsaved_steps > 0 {
                                n.push_str(&format!(
                                    "; {} unsaved step(s) can be redone",
                                    r.unsaved_steps
                                ));
                            }
                            n.push(')');
                            n
                        });
                        let warn = (r.repaired_bytes > 0).then(|| {
                            format!(
                                "The end of the undo history was damaged (an interrupted write) and was repaired: {} cut off. A copy of the damaged file was kept next to it.",
                                format_bytes(r.repaired_bytes)
                            )
                        });
                        return (note, warn);
                    }
                    Err((j, problem)) => {
                        drop(j);
                        log::warn!("undo history {}: {problem}", path.display());
                        warn = Some(set_aside(&path, &problem));
                    }
                },
                Ok(Err(problem)) => {
                    log::warn!("undo history {}: {problem}", path.display());
                    warn = Some(set_aside(&path, &problem));
                }
                Err(e) => {
                    log::warn!("could not read the undo history {}: {e}", path.display());
                    warn = Some(set_aside(&path, &JournalProblem::Corrupt(e.to_string())));
                }
            }
        }
        match Journal::create(&path, key) {
            Ok(j) => {
                if let Err(e) = o.history.attach_new_journal(j, identity) {
                    log::warn!("undo history journal: {e}");
                    warn = Some(format!(
                        "The undo history cannot be kept on disk ({e}); it works for this session only."
                    ));
                }
            }
            Err(e) => {
                log::warn!(
                    "could not create the undo history at {}: {e}",
                    path.display()
                );
                warn = Some(format!(
                    "The undo history cannot be kept on disk ({e}); it works for this session only."
                ));
            }
        }
        (None, warn)
    }

    /// A save of state `state` to `path` completed: the journal follows the
    /// project (first save, Save As) and records the save point.
    pub(super) fn history_saved(
        &mut self,
        path: &Path,
        state: plumspirit_engine::history::StateId,
        identity: u64,
    ) {
        let Some(dir) = self.history_dir() else {
            return;
        };
        let Some(o) = self.open.as_mut() else {
            return;
        };
        o.saved_identity = Some(identity);
        let key = journal::path_key(&crate::recent::canonical(path));
        let target = journal_path(&dir, key);
        match o.history.journal_mut() {
            Some(j) => {
                if j.path() != target {
                    // The file was overwritten: its old history no longer
                    // describes it.
                    if target.exists() {
                        let _ = std::fs::remove_file(&target);
                    }
                    if let Err(e) = j.rekey(&target, key) {
                        log::warn!(
                            "could not move the undo history to {}: {e}",
                            target.display()
                        );
                    }
                }
            }
            None => {
                if let Ok(j) = Journal::create(&target, key) {
                    let _ = o.history.attach_new_journal(j, Some(identity));
                }
            }
        }
        o.history.persist_saved(state, identity);
    }

    /// Closes the open document's journal (an unsaved project's is removed).
    pub(super) fn close_history(&mut self) {
        if let Some(o) = self.open.as_mut()
            && let Some(j) = o.history.detach_journal()
        {
            let p = j.path().to_path_buf();
            drop(j);
            if is_untitled(&p) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    /// Goes to step `k` of the History window (0 = the start).
    fn history_jump(&mut self, k: usize) {
        let Some(cur) = self.open.as_ref().map(|o| o.history.undo_len()) else {
            return;
        };
        if k < cur {
            for _ in k..cur {
                let before = self.open.as_ref().map_or(0, |o| o.history.undo_len());
                self.undo();
                if self.open.as_ref().map_or(0, |o| o.history.undo_len()) == before {
                    break;
                }
            }
        } else {
            for _ in cur..k {
                let before = self.open.as_ref().map_or(0, |o| o.history.undo_len());
                self.redo();
                if self.open.as_ref().map_or(0, |o| o.history.undo_len()) == before {
                    break;
                }
            }
        }
        if let Some(o) = self.open.as_ref() {
            self.set_status(
                format!(
                    "History: step {} of {}.",
                    o.history.undo_len(),
                    o.history.undo_len() + o.history.redo_len()
                ),
                StatusKind::Info,
            );
        }
    }

    /// Edit → Undo History… (Ctrl+H).
    pub(super) fn history_window_ui(&mut self, ctx: &egui::Context) {
        if !self.history_window {
            return;
        }
        let Some(o) = self.open.as_ref() else {
            self.history_window = false;
            return;
        };
        let (undo, redo) = o.history.step_labels();
        let disk = o.history.journal().map(|j| j.disk_bytes());
        let saved = o.history.is_at_saved_state();
        if self
            .history_disk_checked
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
        {
            self.history_disk_all = self.history_dir().map_or(0, |d| dir_bytes(&d));
            self.history_disk_checked = Some(std::time::Instant::now());
        }
        let all = self.history_disk_all;
        let (mut jump, mut compact, mut clear) = (None, false, false);
        let mut confirm = self.history_confirm_clear;
        let mut open = true;
        egui::Window::new("Undo History")
            .open(&mut open)
            .resizable(true)
            .default_size([340.0, 460.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Every step since the history began, kept on disk with the project: it survives saving, closing and restarting. Click a step to go back to it, or forward again.",
                    )
                    .weak(),
                );
                ui.add_space(4.0);
                let cur = undo.len();
                let total = undo.len() + redo.len() + 1;
                let row_h = ui.spacing().interact_size.y;
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 90.0).max(120.0))
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, total, |ui, range| {
                        for k in range {
                            let label = if k == 0 {
                                "Start"
                            } else if k <= undo.len() {
                                undo[k - 1]
                            } else {
                                redo[k - 1 - undo.len()]
                            };
                            let mut text = format!("{k:>5}   {label}");
                            if k == cur {
                                text.push_str(if saved { "   ← now (saved)" } else { "   ← now" });
                            }
                            let rt = if k == cur {
                                RichText::new(text).strong()
                            } else if k > cur {
                                RichText::new(text).weak()
                            } else {
                                RichText::new(text)
                            };
                            let r = ui.add(egui::Button::selectable(k == cur, rt));
                            if r.clicked() && k != cur {
                                jump = Some(k);
                            }
                        }
                    });
                ui.separator();
                match disk {
                    Some(b) => ui.label(format!(
                        "On disk: {} for this project · {} for all projects",
                        format_bytes(b),
                        format_bytes(all)
                    )),
                    None => ui.label("This history is not kept on disk (no settings folder)."),
                };
                if disk.is_some_and(|b| b > LARGE_HISTORY) {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "This project's history is large. Compact History frees the space of discarded steps; Clear History removes all steps.",
                    );
                }
                ui.horizontal(|ui| {
                    let compacting = self.history_compaction.is_some();
                    if ui
                        .add_enabled(
                            disk.is_some() && !compacting,
                            egui::Button::new(if compacting {
                                "Compacting…"
                            } else {
                                "Compact History"
                            }),
                        )
                        .on_hover_text("Rewrites the history file with only the steps you can still reach (discarded redo branches go). No step in the list is lost.")
                        .clicked()
                    {
                        compact = true;
                    }
                    if confirm {
                        ui.label("Remove every step?");
                        if ui.button("Clear").clicked() {
                            clear = true;
                            confirm = false;
                        }
                        if ui.button("Keep").clicked() {
                            confirm = false;
                        }
                    } else if ui
                        .add_enabled(total > 1, egui::Button::new("Clear History…"))
                        .on_hover_text("Removes every undo / redo step of this project (memory and disk). The project itself is not changed.")
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        self.history_confirm_clear = confirm;
        if !open {
            self.history_window = false;
            self.history_confirm_clear = false;
        }
        if let Some(k) = jump {
            self.history_jump(k);
        }
        if compact {
            self.start_history_compaction(ctx);
        }
        if clear && let Some(o) = self.open.as_mut() {
            let id = o.saved_identity;
            match o.history.clear_persistent(id) {
                Ok(()) => self.set_status("Undo history cleared.", StatusKind::Info),
                Err(e) => self.set_status(
                    format!("Could not clear the history file: {e}."),
                    StatusKind::Error,
                ),
            }
            self.history_disk_checked = None;
        }
    }

    /// Compact History on a worker (§209: it copies every live record, which
    /// can take longer than a frame). Editing goes on meanwhile; a history
    /// that changed before the copy finished keeps its file (and says so).
    fn start_history_compaction(&mut self, ctx: &egui::Context) {
        if self.history_compaction.is_some() {
            return;
        }
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let Some(job) = o.history.plan_compaction(o.saved_identity) else {
            return;
        };
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let flag = cancel.clone();
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("history-compact".into())
            .spawn(move || {
                let _ = tx.send(job.run(&flag));
                repaint.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.history_compaction = Some(Compaction {
                    rx,
                    cancel,
                    started: std::time::Instant::now(),
                    generation: o.generation,
                });
                self.set_status("Compacting the undo history…", StatusKind::Info);
            }
            Err(e) => self.set_status(
                format!("Could not compact the history: {e}."),
                StatusKind::Error,
            ),
        }
    }

    /// §216: a journal write that failed during the session (disk full,
    /// permission, removed drive) is reported once per new failure; editing
    /// and undo go on in memory.
    pub(super) fn watch_journal_failures(&mut self) {
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let (n, last) = o.history.journal().map_or((0, None), |j| j.failures());
        let key = (o.generation, n);
        if n == 0 || self.journal_failures_seen == key {
            return;
        }
        let msg = format!(
            "The undo history could not be written to disk ({}); recent steps are kept in memory only and will not survive a restart.",
            last.unwrap_or("unknown error")
        );
        self.journal_failures_seen = key;
        log::warn!("{msg}");
        self.set_status(msg, StatusKind::Warning);
    }

    /// Installs a finished compaction (or reports why not). L-S48: also
    /// collects an undo step the journal wrote in the background (a large
    /// track block) — on frames that happen anyway; every history step and
    /// closing collect it too, so nothing wakes the window for it.
    pub(super) fn poll_history_compaction(&mut self, ctx: &egui::Context) {
        if let Some(o) = self.open.as_mut() {
            o.history.poll_journal();
        }
        let Some(c) = &self.history_compaction else {
            return;
        };
        let generation = self.open.as_ref().map(|o| o.generation);
        if generation != Some(c.generation) {
            // Another project is open: stop and drop the copy.
            c.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let result = match c.rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(std::io::Error::other(
                "the compaction worker stopped unexpectedly",
            )),
        };
        let c = self.history_compaction.take().expect("checked");
        let took = c.started.elapsed();
        self.history_disk_checked = None;
        let installed = match (result, self.open.as_mut()) {
            (Ok(done), Some(o)) if o.generation == c.generation => {
                o.history.finish_compaction(done)
            }
            (Ok(done), _) => {
                done.discard();
                return;
            }
            (Err(e), _) => Err(e),
        };
        match installed {
            Ok((b, a)) => {
                self.tasks.record(
                    "Compacted the undo history",
                    took,
                    super::tasks::TaskOutcome::Done,
                );
                self.set_status(
                    format!(
                        "History compacted: {} → {}.",
                        format_bytes(b),
                        format_bytes(a)
                    ),
                    StatusKind::Success,
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                self.tasks.record(
                    "Compact undo history",
                    took,
                    super::tasks::TaskOutcome::Failed,
                );
                self.set_status(
                    format!("Could not compact the history: {e}."),
                    StatusKind::Warning,
                );
            }
        }
    }
}

/// Compact History running on a worker.
pub(super) struct Compaction {
    rx: std::sync::mpsc::Receiver<
        std::io::Result<plumspirit_engine::history::journal::CompactDone>,
    >,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    started: std::time::Instant,
    generation: u64,
}

impl Compaction {
    pub(super) fn release(self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        drop(self.rx.into_iter().last());
    }
}
