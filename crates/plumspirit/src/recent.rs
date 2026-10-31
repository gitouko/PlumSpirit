//! Open File History (v0.8 §075–§076): `recent.json` next to `settings.json`.
//!
//! Each entry keeps the canonical path, the display name, the file type, when
//! it was last opened (attempted) and last opened / saved successfully, and a
//! pin. Pinned entries are listed first and are never trimmed or removed by
//! Clear History; at most [`MAX_UNPINNED`] other entries are kept (most recent
//! first). The list never touches the files themselves: Remove, Clear Missing
//! and Clear History only edit the list. A moved or renamed file shows as
//! missing until it is located again (Locate… keeps the pin) or removed.
//!
//! The file is parsed leniently entry by entry (a bad entry is skipped and
//! logged, the rest survive) and written atomically (temporary file + rename),
//! like the settings.

use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const RECENT_VERSION: u64 = 1;
const FILE_NAME: &str = "recent.json";
/// Unpinned entries kept (pinned ones are always kept).
pub const MAX_UNPINNED: usize = 25;
/// Hard limit on pinned entries (a sane bound for a hand-edited file).
pub const MAX_PINNED: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecentKind {
    /// A PlumSpirit project (`.lup`).
    Project,
    /// A MIDI file (opened by import).
    Midi,
}

impl RecentKind {
    pub fn key(self) -> &'static str {
        match self {
            RecentKind::Project => "project",
            RecentKind::Midi => "midi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RecentKind::Project => "Project",
            RecentKind::Midi => "MIDI",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "project" => Some(RecentKind::Project),
            "midi" => Some(RecentKind::Midi),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentEntry {
    /// Canonical absolute path (Windows: without the `\\?\` prefix).
    pub path: PathBuf,
    /// File name shown in the lists.
    pub name: String,
    pub kind: RecentKind,
    /// Unix seconds of the last open attempt.
    pub last_opened: u64,
    /// Unix seconds of the last successful open or save (None: never).
    pub last_success: Option<u64>,
    pub pinned: bool,
}

/// What a Recent Files list asks the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentAction {
    Open(PathBuf),
    Remove(PathBuf),
    Pin(PathBuf, bool),
    /// Choose the new location of a moved / renamed file (then open it).
    Locate(PathBuf),
    ClearMissing,
    Clear,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentFiles {
    /// Display order: pinned first, then by last open (newest first).
    entries: Vec<RecentEntry>,
}

/// The path as it is stored and compared: canonical when the file exists,
/// otherwise made absolute; Windows' verbatim prefix is removed.
pub fn canonical(path: &Path) -> PathBuf {
    let p = fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(p)
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    p
}

/// Two stored paths name the same file (case-insensitive on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// "just now", "12 min ago", "3 h ago", "yesterday", "5 days ago", "7 weeks ago".
pub fn age_text(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        172_800..1_209_600 => format!("{} days ago", s / 86_400),
        _ => format!("{} weeks ago", s / 604_800),
    }
}

impl RecentFiles {
    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }

    fn find(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| same_path(&e.path, path))
    }

    /// Records an open attempt (`success`: the file was read) or a successful
    /// save of `path` (already canonical).
    pub fn record(&mut self, path: &Path, kind: RecentKind, success: bool, now: u64) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match self.find(path) {
            Some(i) => {
                let e = &mut self.entries[i];
                e.path = path.to_path_buf();
                e.name = name;
                e.kind = kind;
                e.last_opened = now;
                if success {
                    e.last_success = Some(now);
                }
            }
            None => self.entries.push(RecentEntry {
                path: path.to_path_buf(),
                name,
                kind,
                last_opened: now,
                last_success: success.then_some(now),
                pinned: false,
            }),
        }
        self.normalize();
    }

    pub fn remove(&mut self, path: &Path) -> bool {
        match self.find(path) {
            Some(i) => {
                self.entries.remove(i);
                true
            }
            None => false,
        }
    }

    pub fn set_pinned(&mut self, path: &Path, pinned: bool) {
        if let Some(i) = self.find(path) {
            self.entries[i].pinned = pinned;
            self.normalize();
        }
    }

    /// A moved / renamed file was found at `new`: the entry follows it (pin
    /// kept). An existing entry for `new` is merged into it.
    pub fn relocate(&mut self, old: &Path, new: &Path) {
        let Some(i) = self.find(old) else {
            return;
        };
        let mut e = self.entries.remove(i);
        if let Some(j) = self.find(new) {
            let other = self.entries.remove(j);
            e.pinned |= other.pinned;
            e.last_opened = e.last_opened.max(other.last_opened);
            e.last_success = e.last_success.max(other.last_success);
        }
        e.path = new.to_path_buf();
        e.name = new.file_name().map_or_else(
            || new.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.entries.push(e);
        self.normalize();
    }

    /// Clear History: every unpinned entry (pinned ones stay until unpinned).
    pub fn clear(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.pinned);
        before - self.entries.len()
    }

    /// Clear Missing: entries whose file does not exist (pinned ones too).
    pub fn clear_missing(&mut self, exists: impl Fn(&Path) -> bool) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| exists(&e.path));
        before - self.entries.len()
    }

    /// Display order and limits.
    fn normalize(&mut self) {
        self.entries.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.last_opened.cmp(&a.last_opened))
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut unpinned = 0usize;
        let mut pinned = 0usize;
        self.entries.retain(|e| {
            if e.pinned {
                pinned += 1;
                pinned <= MAX_PINNED
            } else {
                unpinned += 1;
                unpinned <= MAX_UNPINNED
            }
        });
    }
}

/// Lenient parse: bad entries are skipped and reported.
pub fn parse(text: &str) -> (RecentFiles, Vec<String>) {
    let mut problems = Vec::new();
    let mut out = RecentFiles::default();
    let root: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!(
                "recent files list is not valid JSON ({e}); starting empty"
            ));
            return (out, problems);
        }
    };
    match root.get("version").and_then(Value::as_u64) {
        Some(v) if v > RECENT_VERSION => problems.push(format!(
            "recent files version {v} is newer than this build supports ({RECENT_VERSION}); known fields are still read"
        )),
        _ => {}
    }
    let Some(list) = root.get("files").and_then(Value::as_array) else {
        problems.push("recent files list has no \"files\" array; starting empty".into());
        return (out, problems);
    };
    for (i, v) in list.iter().enumerate() {
        let Some(o) = v.as_object() else {
            problems.push(format!("recent file {i} is not an object; skipped"));
            continue;
        };
        let Some(path) = o
            .get("path")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
        else {
            problems.push(format!("recent file {i} has no path; skipped"));
            continue;
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            problems.push(format!("recent file {i} has a relative path; skipped"));
            continue;
        }
        if out.find(&path).is_some() {
            problems.push(format!("recent file {i} is a duplicate; skipped"));
            continue;
        }
        let kind = o
            .get("kind")
            .and_then(Value::as_str)
            .and_then(RecentKind::parse)
            .unwrap_or(RecentKind::Project);
        let name = o
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| path.display().to_string());
        out.entries.push(RecentEntry {
            name,
            kind,
            last_opened: o.get("last_opened").and_then(Value::as_u64).unwrap_or(0),
            last_success: o.get("last_success").and_then(Value::as_u64),
            pinned: o.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            path,
        });
    }
    out.normalize();
    (out, problems)
}

pub fn to_json(r: &RecentFiles) -> String {
    let files: Vec<Value> = r
        .entries
        .iter()
        .map(|e| {
            let mut m = Map::new();
            m.insert("path".into(), json!(e.path.to_string_lossy()));
            m.insert("name".into(), json!(e.name));
            m.insert("kind".into(), json!(e.kind.key()));
            m.insert("last_opened".into(), json!(e.last_opened));
            if let Some(s) = e.last_success {
                m.insert("last_success".into(), json!(s));
            }
            m.insert("pinned".into(), json!(e.pinned));
            Value::Object(m)
        })
        .collect();
    let mut s = serde_json::to_string_pretty(&json!({"version": RECENT_VERSION, "files": files}))
        .unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

pub fn load(dir: Option<&Path>) -> (RecentFiles, Vec<String>) {
    let Some(dir) = dir else {
        return (RecentFiles::default(), Vec::new());
    };
    let path = dir.join(FILE_NAME);
    match fs::read(&path) {
        Ok(bytes) => parse(&String::from_utf8_lossy(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (RecentFiles::default(), Vec::new()),
        Err(e) => (
            RecentFiles::default(),
            vec![format!("could not read {}: {e}", path.display())],
        ),
    }
}

/// Atomic write (temporary file + rename in the same directory).
pub fn save(dir: &Path, r: &RecentFiles) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(to_json(r).as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("plumspirit-recent-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn abs(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn order_pins_limits_and_success_times() {
        let mut r = RecentFiles::default();
        for i in 0..40u64 {
            r.record(
                &abs(&format!("f{i}.lup")),
                RecentKind::Project,
                i % 3 != 1,
                1000 + i,
            );
        }
        assert_eq!(r.entries().len(), MAX_UNPINNED);
        assert_eq!(r.entries()[0].name, "f39.lup");
        // A failed open keeps the previous success time.
        let p = abs("f39.lup");
        r.record(&p, RecentKind::Project, false, 5000);
        assert_eq!(r.entries()[0].last_opened, 5000);
        assert_eq!(r.entries()[0].last_success, Some(1039));
        // Pinned entries come first and survive trimming and Clear History.
        let old = abs("f20.lup");
        r.set_pinned(&old, true);
        assert_eq!(r.entries()[0].path, old);
        for i in 100..140u64 {
            r.record(
                &abs(&format!("g{i}.mid")),
                RecentKind::Midi,
                true,
                10_000 + i,
            );
        }
        assert!(r.entries().iter().any(|e| e.path == old && e.pinned));
        assert_eq!(r.entries().len(), MAX_UNPINNED + 1);
        assert_eq!(r.clear(), MAX_UNPINNED);
        assert_eq!(r.entries().len(), 1);
        assert!(r.remove(&old));
        assert!(r.entries().is_empty());
    }

    #[test]
    fn missing_and_moved_files_never_touch_the_files() {
        let d = temp_dir("moved");
        let a = d.join("song.lup");
        fs::write(&a, b"x").unwrap();
        let mut r = RecentFiles::default();
        r.record(&canonical(&a), RecentKind::Project, true, 1);
        r.set_pinned(&canonical(&a), true);
        r.record(&canonical(&d.join("gone.mid")), RecentKind::Midi, false, 2);
        // Renamed on disk: the entry is missing, the file is untouched.
        let b = d.join("song renamed.lup");
        fs::rename(&a, &b).unwrap();
        let missing: Vec<_> = r.entries().iter().filter(|e| !e.path.exists()).collect();
        assert_eq!(missing.len(), 2);
        // Locate… follows the file and keeps the pin.
        r.relocate(&canonical(&a), &canonical(&b));
        assert_eq!(r.entries()[0].name, "song renamed.lup");
        assert!(r.entries()[0].pinned);
        assert_eq!(r.clear_missing(|p| p.exists()), 1);
        assert_eq!(r.entries().len(), 1);
        assert!(b.exists(), "the list never deletes files");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn save_load_round_trip_and_lenient_parse() {
        let d = temp_dir("io");
        let mut r = RecentFiles::default();
        r.record(&abs("a.lup"), RecentKind::Project, true, 10);
        r.record(&abs("b.mid"), RecentKind::Midi, false, 20);
        r.set_pinned(&abs("a.lup"), true);
        save(&d, &r).unwrap();
        let (back, problems) = load(Some(&d));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back, r);
        // Bad entries are skipped, good ones kept.
        let p = abs("ok.lup").to_string_lossy().replace('\\', "\\\\");
        let text = format!(
            r#"{{"version": 9, "files": [3, {{"name": "x"}}, {{"path": "rel.lup"}},
                {{"path": "{p}", "kind": "weird", "last_opened": 5}}, {{"path": "{p}"}}]}}"#
        );
        let (r2, problems) = parse(&text);
        assert_eq!(r2.entries().len(), 1);
        assert_eq!(r2.entries()[0].kind, RecentKind::Project);
        assert_eq!(problems.len(), 5, "{problems:?}");
        let (r3, problems) = parse("not json");
        assert!(r3.entries().is_empty() && problems.len() == 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn verbatim_prefix_is_removed() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Music\a.lup")),
            PathBuf::from(r"C:\Music\a.lup")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\server\share\a.lup")),
            PathBuf::from(r"\\server\share\a.lup")
        );
        assert_eq!(age_text(1000, 1000), "just now");
        assert_eq!(age_text(100_000, 100_000 - 7200), "2 h ago");
    }
}
