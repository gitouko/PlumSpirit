//! Native file dialogs and file-name rules.
//!
//! Windows (and macOS) use the operating system's file dialogs through `rfd`. Other
//! platforms in this prototype use a simple in-app path prompt instead (Linux is not
//! a release target; the prompt keeps development builds usable and testable).

use plumspirit_engine::native::LUP_EXTENSION;
use std::path::{Path, PathBuf};

/// True when native dialogs are available in this build. Headless unit tests
/// use the in-app path prompt on every platform.
pub const NATIVE_DIALOGS: bool = cfg!(all(not(test), any(windows, target_os = "macos")));

/// What a file dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Open a project (.lup) or a MIDI file.
    Open,
    /// Save a native project.
    Project,
    /// Export a MIDI file.
    Midi,
    /// Choose an SF2 SoundFont.
    SoundFont,
    /// Open / save a PlumSpirit score (.psc).
    Score,
    /// A picture for Image to MIDI.
    Image,
    /// A recording for Audio Reference.
    Audio,
    /// B1-0059: a diagnostics report (.txt).
    Report,
    /// B1-0060: exported / imported keyboard shortcuts (.json).
    Keybinds,
}

impl FileKind {
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    pub fn title(self) -> &'static str {
        match self {
            FileKind::Open => "Open",
            FileKind::Project => "Save Project",
            FileKind::Midi => "Export MIDI",
            FileKind::SoundFont => "Choose SoundFont",
            FileKind::Score => "PlumSpirit Score",
            FileKind::Image => "Open Picture",
            FileKind::Audio => "Load Reference Audio",
            FileKind::Report => "Save Diagnostics Report",
            FileKind::Keybinds => "Keyboard Shortcuts",
        }
    }
}

/// File names the Open dialog lists as archives (L-D02; the content decides).
#[cfg(any(windows, target_os = "macos"))]
pub const ARCHIVE_EXTENSIONS: &[&str] = &[
    "zip", "ZIP", "7z", "7Z", "rar", "RAR", "gz", "GZ", "tgz", "TGZ", "xz", "XZ", "txz", "TXZ",
    "zst", "ZST", "tzst", "TZST", "tar", "TAR",
];

#[cfg(any(windows, target_os = "macos"))]
pub fn pick_open(frame: &eframe::Frame, kind: FileKind, dir: Option<&Path>) -> Option<PathBuf> {
    crate::diagnostics::native_dialog_shown();
    let mut d = rfd::FileDialog::new()
        .set_title(kind.title())
        .set_parent(frame);
    d = match kind {
        FileKind::SoundFont => d.add_filter("SoundFont 2 (*.sf2)", &["sf2", "SF2"]),
        FileKind::Score => d.add_filter("PlumSpirit Score (*.psc)", &["psc", "PSC"]),
        FileKind::Image => d.add_filter(
            "Pictures (*.png; *.jpg; *.jpeg; *.bmp; *.gif; *.webp; *.tif; *.tiff)",
            &[
                "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "bmp", "BMP", "gif", "GIF", "webp",
                "WEBP", "tif", "TIF", "tiff", "TIFF",
            ],
        ),
        FileKind::Keybinds => d.add_filter("PlumSpirit Shortcuts (*.json)", &["json", "JSON"]),
        FileKind::Audio => d.add_filter(
            "Recordings (*.wav; *.flac; *.ogg; *.mp3)",
            &[
                "wav", "WAV", "wave", "flac", "FLAC", "ogg", "OGG", "mp3", "MP3",
            ],
        ),
        _ => {
            // L-D02: archives are opened by content; these are their usual names.
            let mut all = vec!["lup", "LUP", "mid", "midi", "MID", "MIDI"];
            all.extend_from_slice(ARCHIVE_EXTENSIONS);
            d.add_filter(
                "PlumSpirit Projects, MIDI Files and Archives (*.lup; *.mid; *.midi; *.zip; *.7z; …)",
                &all,
            )
            .add_filter("PlumSpirit Projects (*.lup)", &["lup", "LUP"])
            .add_filter(
                "MIDI Files (*.mid; *.midi)",
                &["mid", "midi", "MID", "MIDI"],
            )
            .add_filter(
                "Archives (*.zip; *.7z; *.rar; *.gz; *.tgz; *.xz; *.txz; *.zst; *.tzst; *.tar)",
                ARCHIVE_EXTENSIONS,
            )
        }
    };
    d = d.add_filter("All Files", &["*"]);
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    d.pick_file()
}

/// V09-S47: a picture for a tool parameter (Displacement Map's map), from
/// a parameter row that has no frame at hand.
#[cfg(any(windows, target_os = "macos"))]
pub fn pick_picture() -> Option<PathBuf> {
    crate::diagnostics::native_dialog_shown();
    rfd::FileDialog::new()
        .set_title("Map Picture")
        .add_filter(
            "Pictures (*.png; *.jpg; *.jpeg; *.bmp; *.gif; *.webp; *.tif; *.tiff)",
            &[
                "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "bmp", "BMP", "gif", "GIF", "webp",
                "WEBP", "tif", "TIF", "tiff", "TIFF",
            ],
        )
        .add_filter("All Files", &["*"])
        .pick_file()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_picture() -> Option<PathBuf> {
    None
}

#[cfg(any(windows, target_os = "macos"))]
pub fn pick_save(
    frame: &eframe::Frame,
    kind: FileKind,
    dir: Option<&Path>,
    file_name: &str,
) -> Option<PathBuf> {
    crate::diagnostics::native_dialog_shown();
    let mut d = rfd::FileDialog::new()
        .set_title(kind.title())
        .set_file_name(file_name)
        .set_parent(frame);
    d = match kind {
        FileKind::Midi => d.add_filter("MIDI File (*.mid)", &["mid"]),
        FileKind::Score => d.add_filter("PlumSpirit Score (*.psc)", &["psc"]),
        FileKind::Report => d.add_filter("Text File (*.txt)", &["txt"]),
        FileKind::Keybinds => d.add_filter("PlumSpirit Shortcuts (*.json)", &["json"]),
        _ => d.add_filter("PlumSpirit Project (*.lup)", &["lup"]),
    };
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    d.save_file()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_open(_frame: &eframe::Frame, _kind: FileKind, _dir: Option<&Path>) -> Option<PathBuf> {
    None
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_save(
    _frame: &eframe::Frame,
    _kind: FileKind,
    _dir: Option<&Path>,
    _file_name: &str,
) -> Option<PathBuf> {
    None
}

fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn add_ext(p: PathBuf, ext: &str) -> PathBuf {
    let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".");
    name.push(ext);
    p.with_file_name(name)
}

/// Adds `.mid` when the chosen name has no MIDI extension. Returns the path and
/// whether it was changed (then the native dialog did not check for an existing file
/// with the final name, so PlumSpirit must confirm the overwrite itself).
pub fn normalize_export_path(p: PathBuf) -> (PathBuf, bool) {
    if has_ext(&p, &["mid", "midi"]) {
        (p, false)
    } else {
        (add_ext(p, "mid"), true)
    }
}

/// Adds `.lup` when missing (same contract as [`normalize_export_path`]).
pub fn normalize_project_path(p: PathBuf) -> (PathBuf, bool) {
    if has_ext(&p, &[LUP_EXTENSION]) {
        (p, false)
    } else {
        (add_ext(p, LUP_EXTENSION), true)
    }
}

/// Whether a path names a MIDI file by extension.
pub fn is_midi_path(p: &Path) -> bool {
    has_ext(p, &["mid", "midi", "smf", "kar"])
}

/// Suggested export file name for a source file ("song.mid" -> "song-edit.mid").
pub fn suggested_export_name(source: Option<&Path>, project_name: &str) -> String {
    let stem = source
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| safe_file_stem(project_name));
    if source.is_some_and(is_midi_path) {
        format!("{stem}-edit.mid")
    } else {
        format!("{stem}.mid")
    }
}

/// A file-name-safe version of a project name ("My: Song?" -> "My Song").
pub fn safe_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let t = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim_matches(['.', ' ']).to_owned();
    if t.is_empty() { "Untitled".into() } else { t }
}

/// File name for display ("song.mid"), lossless for Unicode names.
pub fn display_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// B1-0037: a plugin folder to install.
#[cfg(any(windows, target_os = "macos"))]
pub fn pick_folder(frame: &eframe::Frame, title: &str) -> Option<PathBuf> {
    crate::diagnostics::native_dialog_shown();
    rfd::FileDialog::new()
        .set_title(title)
        .set_parent(frame)
        .pick_folder()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_folder(_frame: &eframe::Frame, _title: &str) -> Option<PathBuf> {
    None
}

/// B1-0037: a single `.lua` file to install as a plugin.
#[cfg(any(windows, target_os = "macos"))]
pub fn pick_lua(frame: &eframe::Frame) -> Option<PathBuf> {
    crate::diagnostics::native_dialog_shown();
    rfd::FileDialog::new()
        .set_title("Install Lua Plugin File")
        .set_parent(frame)
        .add_filter("Lua Script (*.lua)", &["lua", "LUA"])
        .pick_file()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_lua(_frame: &eframe::Frame) -> Option<PathBuf> {
    None
}

/// Color presets use their own small JSON file; never a project-file prompt.
#[cfg(any(windows, target_os = "macos"))]
pub fn pick_color_preset(save: bool, default_name: &str) -> Option<PathBuf> {
    crate::diagnostics::native_dialog_shown();
    let d = rfd::FileDialog::new()
        .set_title(if save {
            "Export Color Preset"
        } else {
            "Import Color Preset"
        })
        .add_filter("Color Preset (*.json)", &["json"]);
    if save {
        d.set_file_name(default_name).save_file()
    } else {
        d.pick_file()
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_color_preset(_save: bool, _default_name: &str) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_rules() {
        assert_eq!(
            normalize_export_path(PathBuf::from("a/song")),
            (PathBuf::from("a/song.mid"), true)
        );
        assert_eq!(
            normalize_export_path(PathBuf::from("a/song.mid")),
            (PathBuf::from("a/song.mid"), false)
        );
        assert_eq!(
            normalize_export_path(PathBuf::from("a/SONG.MIDI")),
            (PathBuf::from("a/SONG.MIDI"), false)
        );
        assert_eq!(
            normalize_export_path(PathBuf::from("a/my.song")),
            (PathBuf::from("a/my.song.mid"), true)
        );
        assert_eq!(
            normalize_export_path(PathBuf::from("ノート/曲 1")),
            (PathBuf::from("ノート/曲 1.mid"), true)
        );
        assert_eq!(
            normalize_project_path(PathBuf::from("a/p")),
            (PathBuf::from("a/p.lup"), true)
        );
        assert_eq!(
            normalize_project_path(PathBuf::from("a/p.LUP")),
            (PathBuf::from("a/p.LUP"), false)
        );
        assert_eq!(
            normalize_project_path(PathBuf::from("a/p.mid")),
            (PathBuf::from("a/p.mid.lup"), true)
        );
        assert_eq!(
            suggested_export_name(Some(Path::new("x/Black Song.mid")), "x"),
            "Black Song-edit.mid"
        );
        assert_eq!(
            suggested_export_name(Some(Path::new("x/Proj.lup")), "x"),
            "Proj.mid"
        );
        assert_eq!(suggested_export_name(None, "My: Song?"), "My Song.mid");
        assert_eq!(safe_file_stem(" .. "), "Untitled");
        assert!(is_midi_path(Path::new("a.MID")));
        assert!(!is_midi_path(Path::new("a.lup")));
    }
}
