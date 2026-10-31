//! Open File History in the app (v0.8 §075–§076): recording opens and saves,
//! the missing-file status, and the Recent Files actions.

use super::dialogs::{Dialog, PromptPurpose};
use super::jobs::Then;
use super::{PlumSpiritApp, StatusKind, files};
use crate::recent::{self, RecentAction, RecentKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often the lists re-check which files exist (only while frames run).
const STATUS_INTERVAL: Duration = Duration::from_secs(2);

impl PlumSpiritApp {
    /// Re-checks which entries' files exist (at most every 2 s unless forced).
    pub(super) fn refresh_recent_status(&mut self, force: bool) {
        if !force
            && self
                .recent_checked
                .is_some_and(|t| t.elapsed() < STATUS_INTERVAL)
            && self.recent_missing.len() == self.recent.entries().len()
        {
            return;
        }
        self.recent_missing = self
            .recent
            .entries()
            .iter()
            .map(|e| !e.path.is_file())
            .collect();
        self.recent_checked = Some(Instant::now());
    }

    fn save_recent(&mut self) {
        if let Some(dir) = &self.config_dir
            && let Err(e) = recent::save(dir, &self.recent)
        {
            log::error!(
                "could not save the recent files list to {}: {e}",
                dir.display()
            );
        }
        self.refresh_recent_status(true);
    }

    /// An open attempt (`success` false) or a successful open / save.
    pub(super) fn note_recent(&mut self, path: &Path, kind: RecentKind, success: bool) {
        let p = recent::canonical(path);
        self.recent.record(&p, kind, success, recent::now_secs());
        self.save_recent();
    }

    pub(super) fn apply_recent_action(
        &mut self,
        a: RecentAction,
        frame: &eframe::Frame,
        ctx: &egui::Context,
    ) {
        match a {
            RecentAction::Open(p) => {
                if !p.is_file() {
                    self.refresh_recent_status(true);
                    self.set_status(
                        format!(
                            "{} is missing (moved, renamed or deleted). Locate it from Recent Files or remove it from the list.",
                            p.display()
                        ),
                        StatusKind::Warning,
                    );
                    return;
                }
                if self.check_unsaved(Then::OpenPath(p.clone())) {
                    self.start_open(p, ctx);
                }
            }
            RecentAction::Locate(old) => {
                let dir = old
                    .parent()
                    .filter(|d| d.is_dir())
                    .map(Path::to_path_buf)
                    .or_else(|| self.last_dir.clone());
                if !files::NATIVE_DIALOGS {
                    self.dialog = Some(Dialog::PathPrompt {
                        purpose: PromptPurpose::Locate(old),
                        text: dir.map(|d| format!("{}/", d.display())).unwrap_or_default(),
                        focused: false,
                    });
                    return;
                }
                self.perf.skip_current_frame();
                if let Some(new) = files::pick_open(frame, files::FileKind::Open, dir.as_deref()) {
                    self.located(&old, new, ctx);
                }
            }
            RecentAction::Pin(p, on) => {
                self.recent.set_pinned(&p, on);
                self.save_recent();
            }
            RecentAction::Remove(p) => {
                if self.recent.remove(&p) {
                    self.save_recent();
                    self.set_status(
                        format!(
                            "Removed {} from Recent Files (the file is not touched).",
                            p.display()
                        ),
                        StatusKind::Info,
                    );
                }
            }
            RecentAction::ClearMissing => {
                let n = self.recent.clear_missing(Path::is_file);
                self.save_recent();
                self.set_status(
                    format!("Removed {n} missing file(s) from Recent Files."),
                    StatusKind::Info,
                );
            }
            RecentAction::Clear => {
                let n = self.recent.clear();
                self.save_recent();
                self.set_status(
                    format!("Cleared {n} entr{} from Recent Files (pinned ones stay; no file is touched).", if n == 1 { "y" } else { "ies" }),
                    StatusKind::Info,
                );
            }
        }
    }

    /// Locate…: the entry follows the file to `new`, which is then opened.
    pub(super) fn located(&mut self, old: &Path, new: PathBuf, ctx: &egui::Context) {
        if !new.is_file() {
            self.set_status(
                format!("{} does not exist.", new.display()),
                StatusKind::Warning,
            );
            return;
        }
        self.recent.relocate(old, &recent::canonical(&new));
        self.save_recent();
        if self.check_unsaved(Then::OpenPath(new.clone())) {
            self.start_open(new, ctx);
        }
    }
}
