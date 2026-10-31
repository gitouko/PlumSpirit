//! v1.0-beta1 (B1-0059): Help → Save Diagnostics Report… — one plain-text
//! file a playtester can attach to a bug report: the system and renderer, the
//! open project's size (no notes, names or paths), the audio diagnostics
//! (the same text as Audio Settings → Copy report), the settings and the end
//! of this session's log. Nothing is sent anywhere; the file is only written
//! where the user chooses (on Windows with the Save dialog; elsewhere into
//! the settings folder).
//!
//! B1-0187: the command opens a preview window first. Every section can be
//! left out, the user name / home folder / folder names are hidden by default
//! and file names can be hidden too; the text shown is exactly the text saved
//! or copied. The report never holds notes or the project file: sharing the
//! song is the user's own choice (File → Export MIDI…).

use super::{PlumSpiritApp, autobackup, files};
use crate::ui::status_bar::StatusKind;
use eframe::egui;
use std::path::{Path, PathBuf};

/// What a report holds (B1-0187).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReportOptions {
    pub system: bool,
    pub project: bool,
    pub audio: bool,
    pub settings: bool,
    pub log: bool,
    /// Hide the user name, the home folder and every folder of a path.
    pub hide_user: bool,
    /// Hide file names too (their extensions stay).
    pub hide_files: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        ReportOptions {
            system: true,
            project: true,
            audio: true,
            settings: true,
            log: true,
            hide_user: true,
            hide_files: true,
        }
    }
}

#[cfg(test)]
impl ReportOptions {
    /// Every section, nothing hidden.
    pub const FULL: ReportOptions = ReportOptions {
        system: true,
        project: true,
        audio: true,
        settings: true,
        log: true,
        hide_user: false,
        hide_files: false,
    };
}

/// The preview window's state.
#[derive(Default)]
pub struct DiagWindow {
    pub opts: ReportOptions,
    /// The text for `built_for` (what Save / Copy use).
    pub text: String,
    built_for: Option<ReportOptions>,
}

/// The user's home folder and name, from the environment.
fn home_and_user() -> (Option<String>, Option<String>) {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok();
    (home, user)
}

/// Characters that end a Windows path (they cannot be in a file name).
fn ends_win(c: char) -> bool {
    matches!(c, '"' | '\n' | '\r' | '\t' | '<' | '>' | '|' | '*' | '?')
}

/// Characters that end a Unix path in a report line.
fn ends_unix(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '|' | ';' | ',' | '(' | ')')
}

/// The first extension in `chars[from..to]`: a dot, a letter and up to 7
/// more letters / digits, not followed by a letter or digit — as (dot, end).
fn find_ext(chars: &[char], from: usize, to: usize) -> Option<(usize, usize)> {
    (from..to).find_map(|d| {
        if chars[d] != '.' || !chars.get(d + 1).is_some_and(char::is_ascii_alphabetic) {
            return None;
        }
        let mut e = d + 2;
        while e < to && chars[e].is_ascii_alphanumeric() && e - d <= 8 {
            e += 1;
        }
        (e == to || !chars[e].is_alphanumeric()).then_some((d, e))
    })
}

/// Removes what identifies the user from a diagnostics report (B1-0187): the
/// home folder becomes `%USERPROFILE%`, then every absolute path (`C:\…`,
/// `%USERPROFILE%\…`, `/…/…`, also with doubled JSON backslashes and with
/// spaces in folder names) keeps only its file name as `<path>\name.ext`
/// (`<path>\<file>.ext` with `hide_files`), and the user name becomes
/// `<user>`. Text that is not a path is unchanged.
pub fn redact_report(
    text: &str,
    home: Option<&str>,
    user: Option<&str>,
    hide_files: bool,
) -> String {
    use crate::lua::plugins::replace_ci;
    let mut s = text.to_owned();
    if let Some(h) = home.filter(|h| h.len() > 3) {
        for variant in [
            h.replace('/', "\\").replace('\\', "\\\\"),
            h.to_owned(),
            h.replace('\\', "/"),
            h.replace('/', "\\"),
        ] {
            s = replace_ci(&s, &variant, "%USERPROFILE%");
        }
    }
    const HOME: &str = "%USERPROFILE%";
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let home_chars: Vec<char> = HOME.chars().collect();
    let is_sep = |c: char| c == '\\' || c == '/';
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        let boundary = i == 0 || !chars[i - 1].is_alphanumeric();
        let drive = boundary
            && i + 2 < n
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && is_sep(chars[i + 2]);
        let at_home = chars[i..].starts_with(&home_chars)
            && chars.get(i + home_chars.len()).is_some_and(|&c| is_sep(c));
        // A Unix path: '/' after a boundary, a name, and another '/' before
        // the token ends (so "NPS / Poly" or "1/2" are not paths).
        let unix = chars[i] == '/'
            && (i == 0 || chars[i - 1].is_whitespace() || "\"'(=:".contains(chars[i - 1]))
            && chars
                .get(i + 1)
                .is_some_and(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
            && chars[i + 1..]
                .iter()
                .take_while(|c| !ends_unix(**c))
                .any(|&c| c == '/');
        if !(drive || at_home || unix) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let win = drive || at_home;
        let ends = |c: char| if win { ends_win(c) } else { ends_unix(c) };
        // Walk the segments: a segment followed by a separator is a folder
        // (Windows folders may hold spaces) unless it already holds a file
        // name with an extension followed by more text.
        let mut k = i + if drive {
            2
        } else if at_home {
            home_chars.len()
        } else {
            0
        };
        let (sep_start, name_at, end, dot) = loop {
            let sep_start = k;
            while k < n && is_sep(chars[k]) {
                k += 1;
            }
            let seg = k;
            let mut j = seg;
            while j < n && !is_sep(chars[j]) && !ends(chars[j]) {
                j += 1;
            }
            let ext = find_ext(&chars, seg, j);
            let folder = j < n && is_sep(chars[j]) && ext.is_none_or(|(_, e)| e == j);
            if folder {
                k = j;
                continue;
            }
            match ext {
                Some((d, e)) => break (sep_start, seg, e, Some(d)),
                None => {
                    let e = (seg..j).find(|&x| chars[x].is_whitespace()).unwrap_or(j);
                    break (sep_start, seg, e, None);
                }
            }
        };
        out.push_str("<path>");
        out.extend(&chars[sep_start..name_at]);
        match (hide_files, dot) {
            (true, Some(d)) if d > name_at => {
                out.push_str("<file>");
                out.extend(&chars[d..end]);
            }
            (true, _) if end > name_at => out.push_str("<name>"),
            _ => out.extend(&chars[name_at..end]),
        }
        i = end.max(i + 1);
    }
    if let Some(u) = user.filter(|u| u.len() >= 3) {
        out = replace_ci(&out, u, "<user>");
    }
    out
}

/// How much of the end of the log goes into a report.
pub const LOG_TAIL_BYTES: usize = 256 << 10;

/// The last `max` bytes of the file at `path` as text (from a line start
/// when one is near), or why it could not be read.
pub fn log_tail(path: &Path, max: usize) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(max);
            let mut tail = &bytes[start..];
            if start > 0
                && let Some(nl) = tail.iter().position(|&b| b == b'\n')
            {
                tail = &tail[nl + 1..];
            }
            let mut t = String::from_utf8_lossy(tail).into_owned();
            if start > 0 {
                t.insert_str(0, "[… earlier lines left out …]\n");
            }
            t
        }
        Err(e) => format!("(the log could not be read: {e})\n"),
    }
}

impl PlumSpiritApp {
    /// The report's text: every section, nothing hidden.
    #[cfg(test)]
    pub(super) fn diagnostics_text(&self) -> String {
        self.diagnostics_text_with(ReportOptions::FULL)
    }

    /// The report's text with the sections `o` keeps, redacted as `o` asks.
    pub(super) fn diagnostics_text_with(&self, o: ReportOptions) -> String {
        let text = self.diagnostics_sections(o);
        if o.hide_user || o.hide_files {
            let (home, user) = home_and_user();
            redact_report(&text, home.as_deref(), user.as_deref(), o.hide_files)
        } else {
            text
        }
    }

    fn diagnostics_sections(&self, o: ReportOptions) -> String {
        let mut r = format!(
            "PlumSpirit {} diagnostics report — {}\n\
             Contains: system and renderer, the open project's size (no notes, names or paths), \
             the audio diagnostics, the settings (they include the paths of SoundFonts and folders) \
             and the end of this session's log. PlumSpirit sends nothing; share this file yourself.\n",
            env!("CARGO_PKG_VERSION"),
            autobackup::utc_stamp(std::time::SystemTime::now()),
        );
        let mb =
            |b: Option<u64>| b.map_or_else(|| "unknown".to_owned(), |b| format!("{} MB", b >> 20));
        let left_out: Vec<&str> = [
            (o.system, "system"),
            (o.project, "project"),
            (o.audio, "audio"),
            (o.settings, "settings"),
            (o.log, "log"),
        ]
        .iter()
        .filter(|(on, _)| !on)
        .map(|(_, n)| *n)
        .collect();
        if !left_out.is_empty() {
            r.push_str(&format!("Left out by the user: {}.\n", left_out.join(", ")));
        }
        if o.hide_user || o.hide_files {
            r.push_str(&format!(
                "Hidden: {}.\n",
                match (o.hide_user, o.hide_files) {
                    (true, true) => "user name, home folder, folder and file names",
                    (true, false) => "user name, home folder and folder names",
                    _ => "folder and file names",
                }
            ));
        }
        if o.system {
            r.push_str("\n== System\n");
            r.push_str(&format!(
                "OS: {} {}\nRenderer: {}\nRAM: {} total, {} available; PlumSpirit uses {}\n",
                std::env::consts::OS,
                std::env::consts::ARCH,
                self.renderer_info.line(),
                mb(crate::platform::total_memory_bytes()),
                mb(crate::platform::available_memory_bytes()),
                mb(crate::diagnostics::process_memory_bytes()),
            ));
        }
        if o.project {
            r.push_str("\n== Project\n");
            match &self.open {
                None => r.push_str("No project open.\n"),
                Some(o) => {
                    let d = &o.doc;
                    r.push_str(&format!(
                        "Tracks: {} · notes: {} · timing: {} · keys: {} · unsaved changes: {}\n",
                        d.track_count(),
                        d.note_count(),
                        d.division().describe(),
                        d.project().keys.keys(),
                        if o.dirty() { "yes" } else { "no" },
                    ));
                }
            }
        }
        if o.audio {
            r.push_str("\n== Audio\n");
            let sf = self
                .transport
                .soundfont()
                .map(|s| s.path.to_string_lossy().into_owned());
            r.push_str(&super::audio_settings::diagnostics_report(
                &self.settings.audio,
                self.transport.audio_diagnostics().as_ref(),
                &self.transport.output_status(),
                sf.as_deref(),
            ));
        }
        if o.settings {
            r.push_str("\n== Settings (settings.json)\n");
            r.push_str(&crate::settings::to_json(&self.settings));
            r.push('\n');
        }
        if o.log {
            r.push_str("\n== Log (plumspirit.log, end)\n");
            match &self.config_dir {
                Some(dir) => r.push_str(&log_tail(&dir.join("plumspirit.log"), LOG_TAIL_BYTES)),
                None => r.push_str("(no settings folder: logging to a file is off)\n"),
            }
        }
        r
    }

    /// Help → Save Diagnostics Report…: opens the preview (B1-0187).
    pub(super) fn open_diagnostics_window(&mut self) {
        let w = self.diag_window.get_or_insert_with(DiagWindow::default);
        w.built_for = None; // the log and settings may have changed
    }

    /// Builds the preview's text when its options changed.
    pub(super) fn refresh_diagnostics_preview(&mut self) {
        let Some(o) = self.diag_window.as_ref().map(|w| w.opts) else {
            return;
        };
        if self.diag_window.as_ref().and_then(|w| w.built_for) == Some(o) {
            return;
        }
        let text = self.diagnostics_text_with(o);
        if let Some(w) = self.diag_window.as_mut() {
            w.text = text;
            w.built_for = Some(o);
        }
    }

    /// The Diagnostics Report window: sections, privacy, the exact text.
    pub(super) fn diagnostics_window_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.diag_window.is_none() {
            return;
        }
        self.refresh_diagnostics_preview();
        let mut open = true;
        let (mut save, mut copy, mut close) = (false, false, false);
        let Some(w) = self.diag_window.as_mut() else {
            return;
        };
        egui::Window::new("Diagnostics Report")
            .id(egui::Id::new("plumspirit-diagnostics-report"))
            .open(&mut open)
            .resizable(true)
            .default_size([620.0, 560.0])
            .show(ctx, |ui| {
                ui.label(
                    "A text file to attach to a bug report. PlumSpirit sends nothing: you choose \
                     where to save it and whether to share it. It never holds notes or the project \
                     file — to share the song itself, export it yourself (File → Export MIDI…) and \
                     attach it only if you want to.",
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Include:");
                    ui.checkbox(&mut w.opts.system, "System and renderer")
                        .on_hover_text("Windows version, graphics renderer and memory.");
                    ui.checkbox(&mut w.opts.project, "Project size")
                        .on_hover_text("Track and note counts, timing and keys — no names or notes.");
                    ui.checkbox(&mut w.opts.audio, "Audio")
                        .on_hover_text("The same text as Audio Settings → Diagnostics → Copy report.");
                    ui.checkbox(&mut w.opts.settings, "Settings")
                        .on_hover_text("PlumSpirit's settings.json.");
                    ui.checkbox(&mut w.opts.log, "Log")
                        .on_hover_text("The end of this session's log (plumspirit.log).");
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Privacy:");
                    ui.checkbox(&mut w.opts.hide_user, "Hide my user name and folders")
                        .on_hover_text("The home folder becomes %USERPROFILE%, your user name <user>, and every folder of a path <path>.");
                    ui.checkbox(&mut w.opts.hide_files, "Hide file names")
                        .on_hover_text("File names become <file>; their extensions (.mid, .sf2 …) stay.");
                });
                ui.add_space(4.0);
                let lines = w.text.lines().count();
                ui.label(
                    egui::RichText::new(format!(
                        "{} lines · {:.1} KB — exactly what will be saved or copied:",
                        lines,
                        w.text.len() as f64 / 1024.0
                    ))
                    .weak(),
                );
                let h = (ui.available_height() - 40.0).max(120.0);
                egui::ScrollArea::vertical()
                    .max_height(h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut w.text.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                    });
                ui.horizontal(|ui| {
                    save = ui.button("Save Report…").clicked();
                    copy = ui.button("Copy to Clipboard").clicked();
                    close = ui.button("Close").clicked();
                });
            });
        if copy {
            ctx.copy_text(w.text.clone());
            self.set_status(
                "Diagnostics report copied to the clipboard.",
                StatusKind::Info,
            );
        }
        if save {
            self.save_diagnostics_report(frame);
        }
        if close || !open {
            self.diag_window = None;
        }
    }

    /// The window's Save Report… button.
    pub(super) fn save_diagnostics_report(&mut self, frame: &eframe::Frame) {
        let name = format!(
            "PlumSpirit diagnostics {}.txt",
            autobackup::utc_stamp(std::time::SystemTime::now())
        );
        let path: Option<PathBuf> = if files::NATIVE_DIALOGS {
            self.perf.skip_current_frame();
            files::pick_save(
                frame,
                files::FileKind::Report,
                self.config_dir.as_deref(),
                &name,
            )
        } else {
            match &self.config_dir {
                Some(d) => Some(d.join(&name)),
                None => {
                    self.set_status(
                        "There is no settings folder to save the report in.",
                        StatusKind::Warning,
                    );
                    return;
                }
            }
        };
        let Some(path) = path else {
            return; // cancelled
        };
        self.write_diagnostics_report(&path);
    }

    /// Writes the report to `path` and says where it went: the preview's
    /// text when the window is open (exactly what the user saw), else the
    /// default redacted report.
    pub(super) fn write_diagnostics_report(&mut self, path: &Path) {
        self.refresh_diagnostics_preview();
        let text = match &self.diag_window {
            Some(w) => w.text.clone(),
            None => self.diagnostics_text_with(ReportOptions::default()),
        };
        match std::fs::write(path, text.as_bytes()) {
            Ok(()) => self.set_status(
                format!("Diagnostics report saved: {}.", path.display()),
                StatusKind::Info,
            ),
            Err(e) => self.set_status(
                format!(
                    "The diagnostics report could not be saved to {} ({}).",
                    path.display(),
                    crate::audio::engine::sentence(&e)
                ),
                StatusKind::Error,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::model::{Bpm, PlumDocument};

    #[test]
    fn the_log_tail_starts_at_a_line_and_says_what_was_left_out() {
        let dir = std::env::temp_dir().join(format!("plumspirit-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plumspirit.log");
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, &text).unwrap();
        assert_eq!(log_tail(&p, 1 << 20), text);
        let t = log_tail(&p, 100);
        assert!(t.starts_with("[… earlier lines left out …]\nline 9"), "{t}");
        assert!(t.ends_with("line 999\n"));
        assert!(log_tail(&dir.join("missing.log"), 10).starts_with("(the log could not be read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The report has every section, the project's size but not its name,
    /// the settings as saved, and the log; saving says where it went.
    #[test]
    fn the_report_holds_every_section_but_no_project_content() {
        let dir =
            std::env::temp_dir().join(format!("plumspirit-report-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plumspirit.log"), "INFO started\n").unwrap();
        let mut h = Harness::new(PlumDocument::new_project(
            "Secret Song".into(),
            Bpm::DEFAULT,
            960,
        ));
        h.app.config_dir = Some(dir.clone());
        let t = h.app.diagnostics_text();
        for s in [
            "== System",
            "== Project",
            "== Audio",
            "== Settings",
            "== Log",
        ] {
            assert!(t.contains(s), "{s}");
        }
        assert!(t.contains("Tracks: 1 · notes: 0"), "{t}");
        assert!(!t.contains("Secret Song"), "no project name");
        assert!(t.contains("\"autobackup_minutes\""), "the settings");
        assert!(t.contains("INFO started"), "the log");
        let out = dir.join("report.txt");
        h.app.write_diagnostics_report(&out);
        // Without the window the default (redacted) report is written: the
        // full text plus its "Hidden: …" line.
        assert_eq!(
            std::fs::read_to_string(&out).unwrap().lines().count(),
            t.lines().count() + 1
        );
        let status = h.app.status.as_ref().unwrap().text.clone();
        assert!(status.starts_with("Diagnostics report saved: "), "{status}");
        h.app
            .write_diagnostics_report(&dir.join("no-such-folder").join("r.txt"));
        assert!(
            h.app
                .status
                .as_ref()
                .unwrap()
                .text
                .starts_with("The diagnostics report could not be saved"),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B1-0187: the user name, the home folder and folders vanish from paths
    /// written plainly, JSON-escaped, with spaces and with forward slashes;
    /// file names stay unless asked; text that is not a path is unchanged.
    #[test]
    fn redaction_hides_the_user_and_folders_and_optionally_file_names() {
        let home = Some(r"C:\Users\ouko_");
        let user = Some("ouko_");
        let line = r"INFO opened C:\Users\ouko_\Music\Ouko's BMC Programs\PlumSpirit ~ Black MIDI Editor\Hypernova.mid in 4.41 s (3/4 tracks, NPS / Poly)";
        assert_eq!(
            redact_report(line, home, user, false),
            r"INFO opened <path>\Hypernova.mid in 4.41 s (3/4 tracks, NPS / Poly)"
        );
        assert_eq!(
            redact_report(line, home, user, true),
            r"INFO opened <path>\<file>.mid in 4.41 s (3/4 tracks, NPS / Poly)"
        );
        let json = r#"  "soundfont": "C:\\Users\\Ouko_\\SoundFonts\\GeneralUser-GS v2.0.3.sf2","#;
        assert_eq!(
            redact_report(json, home, user, false),
            r#"  "soundfont": "<path>\\GeneralUser-GS v2.0.3.sf2","#
        );
        assert_eq!(
            redact_report(json, home, user, true),
            r#"  "soundfont": "<path>\\<file>.sf2","#
        );
        // Another drive, a folder with a dot and brackets, forward slashes.
        let other = r"D:/Black MIDI/PlumSpirit v0.9 (Windows x64)/tau2.5.9(1).mid saved";
        assert_eq!(
            redact_report(other, home, user, false),
            r"<path>/tau2.5.9(1).mid saved"
        );
        assert_eq!(
            redact_report(other, home, user, true),
            r"<path>/<file>.mid saved"
        );
        // Unix paths stop at whitespace; the user name goes everywhere.
        let unix = "log /home/ouko_/plum/x.lup, then ouko_ again; 1/2 and a/b stay";
        assert_eq!(
            redact_report(unix, Some("/home/ouko_"), user, false),
            "log <path>/x.lup, then <user> again; 1/2 and a/b stay"
        );
        assert_eq!(redact_report("C:\\", home, user, true), "<path>\\");
        let plain = "Tracks: 3 · notes: 1,000 · timing: 960 PPQN · keys: 128";
        assert_eq!(redact_report(plain, home, user, true), plain);
    }

    /// B1-0187: the command opens the preview; sections can be left out; the
    /// text saved is exactly the text previewed; closing the window clears it.
    #[test]
    fn the_report_previews_redacts_and_saves_exactly_what_is_shown() {
        let dir =
            std::env::temp_dir().join(format!("plumspirit-report-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plumspirit.log"),
            "INFO opened /tmp/x/Secret Song.mid\n",
        )
        .unwrap();
        let mut h = Harness::new(PlumDocument::new_project(
            "Secret Song".into(),
            Bpm::DEFAULT,
            960,
        ));
        h.app.config_dir = Some(dir.clone());
        assert!(h.command(super::super::commands::Command::SaveDiagnostics));
        assert!(h.app.diag_window.is_some(), "the command opens the preview");
        assert!(!dir.join("report.txt").exists());
        h.app.refresh_diagnostics_preview();
        let text = h.app.diag_window.as_ref().unwrap().text.clone();
        assert!(
            text.contains("Hidden: user name, home folder, folder and file names."),
            "{text}"
        );
        assert!(!text.contains("Secret Song"), "{text}");
        for sec in [
            "== System",
            "== Project",
            "== Audio",
            "== Settings",
            "== Log",
        ] {
            assert!(text.contains(sec), "{sec}");
        }
        {
            let w = h.app.diag_window.as_mut().unwrap();
            w.opts.settings = false;
            w.opts.log = false;
        }
        h.app.refresh_diagnostics_preview();
        let text = h.app.diag_window.as_ref().unwrap().text.clone();
        assert!(
            text.contains("Left out by the user: settings, log."),
            "{text}"
        );
        assert!(!text.contains("== Settings") && !text.contains("== Log"));
        let out = dir.join("report.txt");
        h.app.write_diagnostics_report(&out);
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            text,
            "saved = previewed"
        );
        // One drawn frame with the window open, then Close.
        h.app_frame(egui::vec2(1280.0, 800.0), 1.0);
        h.app.diag_window = None;
        h.app_frame(egui::vec2(1280.0, 800.0), 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
