//! v1.0-beta1 (B1-0059): automatic backups of the open project.
//!
//! While a project has unsaved changes, a copy is written every
//! `autobackup_minutes` (Settings → Import / Export; 0 = off) into the backup folder
//! (`autobackup_folder`, empty = `Backups` in the settings folder) with the
//! same transactional writer as Save (`native::save_project`: temporary
//! file → sync → verify → rename). The newest `autobackup_keep` backups of
//! each project are kept; older ones of that project are deleted.
//!
//! A backup never changes the project: its path, its saved state, its undo
//! history and its dirty flag stay as they were. The worker writes a clone of
//! the document that shares the note chunks (no second copy of the notes),
//! so editing goes on while it runs — an edit only copies the chunks it
//! touches. One backup runs at a time; a failure is reported in the status
//! bar and tried again after the next interval.

use super::{DocSource, PlumSpiritApp, files};
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::native::{self, LupControl};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Minutes between backups unless the settings say otherwise (0 = off).
pub const DEFAULT_INTERVAL_MINUTES: u32 = 10;
/// The longest interval accepted.
pub const MAX_INTERVAL_MINUTES: u32 = 240;
/// Backups kept per project unless the settings say otherwise.
pub const DEFAULT_KEEP: u32 = 3;
/// The most backups kept per project.
pub const MAX_KEEP: u32 = 100;
/// The default folder's name inside the settings folder.
pub const FOLDER_NAME: &str = "Backups";

/// Checks a typed interval ("10", "10 min"; 0 = off).
pub fn parse_interval(text: &str) -> Result<u32, String> {
    let t = text.trim();
    let t = t.strip_suffix("min").unwrap_or(t).trim();
    match t.parse::<u32>() {
        Ok(m) if m <= MAX_INTERVAL_MINUTES => Ok(m),
        _ => Err(format!(
            "Enter whole minutes from 0 (off) to {MAX_INTERVAL_MINUTES}."
        )),
    }
}

/// Checks a typed number of backups to keep.
pub fn parse_keep(text: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(n) if (1..=MAX_KEEP).contains(&n) => Ok(n),
        _ => Err(format!("Enter a whole number from 1 to {MAX_KEEP}.")),
    }
}

/// Checks a typed backup folder: empty (the default folder) or a full path
/// that is not a file.
pub fn parse_folder(text: &str) -> Result<String, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let p = Path::new(t);
    if !p.is_absolute() {
        return Err("Enter a full folder path (for example C:\\PlumSpirit Backups), or leave it empty for the default folder.".into());
    }
    if p.is_file() {
        return Err("That path is a file, not a folder.".into());
    }
    Ok(t.to_owned())
}

/// The automatic backup's timer and the backup being written.
#[derive(Default)]
pub struct AutoBackup {
    /// When the current stretch of unsaved changes began, or the last backup
    /// (or failed try) ended.
    since: Option<Instant>,
    /// (document generation, undo-history state) of the last backup written.
    done: Option<(u64, u64)>,
    running: Option<Running>,
    /// Tells apart the untitled projects of different runs.
    session: u64,
}

struct Running {
    key: (u64, u64),
    rx: mpsc::Receiver<Result<PathBuf, String>>,
}

impl AutoBackup {
    /// Let an already-started transactional backup finish off the UI thread.
    pub fn release(mut self) {
        if let Some(j) = self.running.take() {
            drop(j.rx.into_iter().last());
        }
    }

    /// A new document: the timer starts again with its first change.
    pub fn document_changed(&mut self) {
        self.since = None;
        self.done = None;
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

/// FNV-1a, stable across builds (it names files).
fn fnv(text: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in text.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The start of every backup file name of one project:
/// `"<name> [<id>] backup "`. The id comes from the project's file (or, for a
/// project not saved yet, from this run), so two projects with the same name
/// never prune each other's backups.
pub fn backup_prefix(source: &DocSource, name: &str, generation: u64, session: u64) -> String {
    let (stem, id) = match source {
        DocSource::Project(p) | DocSource::Midi(p) => (
            p.file_stem()
                .map(|s| files::safe_file_stem(&s.to_string_lossy()))
                .unwrap_or_else(|| files::safe_file_stem(name)),
            fnv(&p.to_string_lossy()),
        ),
        DocSource::New => (
            files::safe_file_stem(name),
            fnv(&format!("new {session} {generation}")),
        ),
    };
    format!("{stem} [{id:08x}] backup ")
}

/// `2026-10-22 23-12-42Z` (UTC; sorts in time order, valid in file names).
pub fn utc_stamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 → civil date (H. Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}Z",
        rem / 3_600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Writes `doc` as `<folder>/<prefix><stamp>.lup` with the transactional
/// writer, then keeps only the newest `keep` backups with this prefix.
pub fn write_backup(
    doc: &PlumDocument,
    folder: &Path,
    prefix: &str,
    stamp: &str,
    keep: u32,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| {
        format!(
            "The backup folder {} could not be created ({}).",
            folder.display(),
            crate::audio::engine::sentence(&e)
        )
    })?;
    let path = folder.join(format!("{prefix}{stamp}.lup"));
    native::save_project(
        doc,
        &path,
        LupControl {
            cancel: None,
            progress_notes: None,
        },
    )
    .map_err(|e| format!("{}.", crate::audio::engine::sentence(&e)))?;
    prune(folder, prefix, keep);
    Ok(path)
}

/// Deletes the oldest backups with `prefix` beyond `keep`; returns how many.
pub fn prune(folder: &Path, prefix: &str, keep: u32) -> usize {
    let Ok(dir) = std::fs::read_dir(folder) else {
        return 0;
    };
    let mut names: Vec<String> = dir
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(prefix) && n.ends_with(".lup"))
        .collect();
    names.sort();
    let extra = names.len().saturating_sub(keep.max(1) as usize);
    names[..extra]
        .iter()
        .filter(|n| std::fs::remove_file(folder.join(n)).is_ok())
        .count()
}

impl PlumSpiritApp {
    /// Where backups go: the folder from Settings, else `Backups` in the
    /// settings folder (none without a settings folder).
    pub(super) fn backup_folder(&self) -> Option<PathBuf> {
        let f = self.settings.autobackup_folder.trim();
        if f.is_empty() {
            self.config_dir.as_ref().map(|d| d.join(FOLDER_NAME))
        } else {
            Some(PathBuf::from(f))
        }
    }

    /// Once per frame: reports a finished backup and starts the next one when
    /// it is due. Cheap; schedules one wake-up for the due time instead of
    /// repainting while idle.
    pub(super) fn autobackup_tick(&mut self, ctx: &egui::Context) {
        if let Some(r) = &self.autobackup.running {
            let res = match r.rx.try_recv() {
                Ok(res) => res,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("The backup worker stopped unexpectedly.".to_owned())
                }
            };
            let key = r.key;
            self.autobackup.running = None;
            self.autobackup.since = Some(Instant::now());
            match res {
                Ok(path) => {
                    self.autobackup.done = Some(key);
                    self.set_status(
                        format!(
                            "Automatic backup saved: {} (the project itself is unchanged).",
                            files::display_name(&path)
                        ),
                        StatusKind::Info,
                    );
                }
                Err(e) => self.set_status(
                    format!(
                        "The automatic backup could not be saved: {e} The project is unchanged; it is tried again in {} min.",
                        self.settings.autobackup_minutes
                    ),
                    StatusKind::Warning,
                ),
            }
        }
        let minutes = self.settings.autobackup_minutes;
        let Some(o) = self.open.as_ref() else {
            self.autobackup.since = None;
            return;
        };
        if minutes == 0 || !o.dirty() {
            self.autobackup.since = None;
            return;
        }
        let key = (o.generation, o.history.current_state());
        if self.autobackup.done == Some(key) {
            return;
        }
        let now = Instant::now();
        let since = *self.autobackup.since.get_or_insert(now);
        let due = since + Duration::from_secs(u64::from(minutes) * 60);
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }
        self.start_backup(key, ctx);
    }

    fn start_backup(&mut self, key: (u64, u64), ctx: &egui::Context) {
        let Some(folder) = self.backup_folder() else {
            self.autobackup.since = Some(Instant::now());
            return;
        };
        let Some(o) = self.open.as_ref() else {
            return;
        };
        if self.autobackup.session == 0 {
            self.autobackup.session = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64 | 1);
        }
        let prefix = backup_prefix(&o.source, o.name(), o.generation, self.autobackup.session);
        // Shares the note chunks: the app's own `Arc` stays unique, so edits
        // are never refused while the backup is written.
        let doc = Arc::new(PlumDocument::clone(&o.doc));
        let keep = self.settings.autobackup_keep;
        let stamp = utc_stamp(SystemTime::now());
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("plumspirit-backup".into())
            .spawn(move || {
                let r = write_backup(&doc, &folder, &prefix, &stamp, keep);
                drop(doc);
                let _ = tx.send(r);
                wake.request_repaint();
            });
        match spawned {
            Ok(_) => self.autobackup.running = Some(Running { key, rx }),
            Err(e) => {
                self.autobackup.since = Some(Instant::now());
                log::warn!("the backup worker could not be started: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::history::Operation;
    use plumspirit_engine::model::Bpm;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("plumspirit-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn stamps_are_utc_and_sort_in_time_order() {
        let t = |s| UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(utc_stamp(t(0)), "1970-01-01 00-00-00Z");
        assert_eq!(utc_stamp(t(1_792_710_762)), "2026-10-22 23-12-42Z");
        assert_eq!(utc_stamp(t(951_782_400)), "2000-02-29 00-00-00Z");
        assert!(utc_stamp(t(1_792_710_762)) < utc_stamp(t(1_792_710_763)));
    }

    #[test]
    fn typed_values_are_checked() {
        assert_eq!(parse_interval("10 min"), Ok(10));
        assert_eq!(parse_interval("0"), Ok(0));
        assert!(parse_interval("241").is_err());
        assert!(parse_interval("-1").is_err());
        assert_eq!(parse_keep("3"), Ok(3));
        assert!(parse_keep("0").is_err());
        assert!(parse_keep("101").is_err());
        assert_eq!(parse_folder("  "), Ok(String::new()));
        assert!(parse_folder("relative/dir").is_err());
        let abs = std::env::temp_dir().join("plumspirit-backup-folder-check");
        assert!(parse_folder(&abs.to_string_lossy()).is_ok());
    }

    /// Backups are full, verified projects; only the newest `keep` of the
    /// same project are kept and other projects' backups are left alone.
    #[test]
    fn backups_reopen_equal_and_only_the_newest_of_that_project_are_kept() {
        let dir = scratch("prune");
        let doc = PlumDocument::new_project("Song".into(), Bpm::DEFAULT, 960);
        let a = backup_prefix(
            &DocSource::Project(PathBuf::from("/x/Song.lup")),
            "Song",
            1,
            7,
        );
        let b = backup_prefix(
            &DocSource::Project(PathBuf::from("/y/Song.lup")),
            "Song",
            1,
            7,
        );
        assert_ne!(a, b, "same name, different files");
        assert!(a.starts_with("Song ["));
        let other = write_backup(&doc, &dir, &b, "2026-01-01 00-00-00Z", 3).unwrap();
        for s in 0..5 {
            let stamp = format!("2026-10-22 23-00-0{s}Z");
            write_backup(&doc, &dir, &a, &stamp, 3).unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let ours: Vec<&String> = names.iter().filter(|n| n.starts_with(&a)).collect();
        assert_eq!(ours.len(), 3, "{names:?}");
        assert!(
            ours[0].contains("23-00-02Z"),
            "the oldest two went: {ours:?}"
        );
        assert!(other.exists(), "another project's backup is untouched");
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
        let (back, _) = native::load_project(
            &dir.join(ours[2]),
            LupControl {
                cancel: None,
                progress_notes: None,
            },
        )
        .unwrap();
        assert_eq!(back.fingerprint(), doc.fingerprint());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app: nothing while saved or before the interval; when due, one
    /// backup of the current state while edits go on; the project keeps its
    /// source and unsaved state; no second backup of the same state.
    #[test]
    fn a_due_backup_is_written_without_touching_the_project_or_blocking_edits() {
        let dir = scratch("app");
        let mut h = Harness::new(PlumDocument::new_project("Live".into(), Bpm::DEFAULT, 960));
        h.app.settings.autobackup_folder = dir.to_string_lossy().into_owned();
        h.app.settings.autobackup_minutes = 1;
        h.app.autobackup_tick(&h.ctx);
        assert!(h.app.autobackup.since.is_none(), "saved: no timer");
        assert!(
            h.app
                .apply_op(Operation::SetName("Live 2".into()))
                .is_some()
        );
        h.app.autobackup_tick(&h.ctx);
        assert!(h.app.autobackup.since.is_some() && !h.app.autobackup.is_running());
        h.app.autobackup.since = Some(Instant::now() - Duration::from_secs(61));
        h.app.autobackup_tick(&h.ctx);
        assert!(h.app.autobackup.is_running());
        // Editing is not refused while the copy is written.
        assert!(
            h.app
                .apply_op(Operation::SetName("Live 3".into()))
                .is_some()
        );
        let t0 = Instant::now();
        while h.app.autobackup.is_running() {
            assert!(t0.elapsed() < Duration::from_secs(30), "backup finishes");
            std::thread::sleep(Duration::from_millis(5));
            h.app.autobackup_tick(&h.ctx);
        }
        let status = h
            .app
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default();
        assert!(
            status.starts_with("Automatic backup saved: Live"),
            "{status}"
        );
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let (back, _) = native::load_project(
            &files[0],
            LupControl {
                cancel: None,
                progress_notes: None,
            },
        )
        .unwrap();
        assert_eq!(back.project().name, "Live 2", "the state when it started");
        let o = h.app.open.as_ref().unwrap();
        assert!(o.dirty(), "still unsaved");
        assert!(
            matches!(o.source, DocSource::New),
            "the project's path is unchanged"
        );
        // The edit made during the backup is a new state: due again only
        // after another interval.
        h.app.autobackup_tick(&h.ctx);
        assert!(!h.app.autobackup.is_running());
        // Off: nothing more is written and the timer stops.
        h.app.settings.autobackup_minutes = 0;
        h.app.autobackup.since = Some(Instant::now() - Duration::from_secs(3_600));
        h.app.autobackup_tick(&h.ctx);
        assert!(!h.app.autobackup.is_running() && h.app.autobackup.since.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
