//! Bounded developer log.
//!
//! Log lines go to `plumspirit.log` in the configuration directory (the previous
//! session's log is kept as `plumspirit.previous.log`). Each session writes at most
//! [`MAX_LOG_BYTES`]; after that only errors are written, so a runaway warning can
//! never create a multi-gigabyte log. Nothing per-note is ever logged.
//!
//! Debug builds also mirror log lines to stderr.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct FileLogger {
    inner: Mutex<Inner>,
    started: Instant,
}

struct Inner {
    file: Option<File>,
    written: u64,
    capped: bool,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let target = metadata.target();
        // The binary crate is named after the executable ("PlumSpirit"), the engine
        // is "plumspirit_engine".
        let ours =
            target.starts_with(env!("CARGO_CRATE_NAME")) || target.starts_with("plumspirit_engine");
        // egui-winit reports every Ctrl+V while the system clipboard holds no text as
        // an error; PlumSpirit's note clipboard is internal, so that is expected.
        if target == "egui_winit::clipboard" {
            return false;
        }
        // Our own messages at info+, dependencies only at warn+.
        if ours {
            metadata.level() <= Level::Info
        } else {
            metadata.level() <= Level::Warn
        }
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{:>9.3}s {:<5} {}] {}\n",
            self.started.elapsed().as_secs_f64(),
            record.level(),
            record.target(),
            record.args()
        );
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Inner {
            file,
            written,
            capped,
        } = &mut *inner;
        let Some(f) = file.as_mut() else {
            return;
        };
        if *written + line.len() as u64 > MAX_LOG_BYTES && record.level() > Level::Error {
            if !*capped {
                *capped = true;
                let _ = f
                    .write_all(b"[log size limit reached; only errors are recorded from now on]\n");
            }
            return;
        }
        if f.write_all(line.as_bytes()).is_ok() {
            *written += line.len() as u64;
        }
    }

    fn flush(&self) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(f) = inner.file.as_mut()
        {
            let _ = f.flush();
        }
    }
}

/// Rotates the previous log and installs the logger. Returns the log path if a
/// file could be opened (logging still works to stderr in debug builds otherwise).
pub fn init(config_dir: Option<&Path>) -> Option<PathBuf> {
    let mut path = None;
    let file = config_dir.and_then(|dir| {
        fs::create_dir_all(dir).ok()?;
        let current = dir.join("plumspirit.log");
        let previous = dir.join("plumspirit.previous.log");
        if current.exists() {
            let _ = fs::rename(&current, &previous);
        }
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&current)
            .ok()?;
        path = Some(current);
        Some(f)
    });
    let logger = FileLogger {
        inner: Mutex::new(Inner {
            file,
            written: 0,
            capped: false,
        }),
        started: Instant::now(),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logger() -> FileLogger {
        FileLogger {
            inner: Mutex::new(Inner {
                file: None,
                written: 0,
                capped: false,
            }),
            started: Instant::now(),
        }
    }

    fn enabled(target: &str, level: Level) -> bool {
        logger().enabled(&Metadata::builder().target(target).level(level).build())
    }

    #[test]
    fn own_info_messages_are_logged() {
        // Targets are module paths; this crate's is its own module path.
        let own = module_path!().split("::").next().unwrap_or_default();
        assert!(enabled(&format!("{own}::app"), Level::Info));
        assert!(enabled("plumspirit_engine::midi", Level::Info));
        assert!(!enabled(&format!("{own}::app"), Level::Debug));
    }

    #[test]
    fn dependencies_only_warn_and_up() {
        assert!(!enabled("winit::platform", Level::Info));
        assert!(enabled("winit::platform", Level::Warn));
        assert!(!enabled("egui_winit::clipboard", Level::Error));
    }
}
