//! Transport commands and SoundFont selection.

use super::dialogs::{Dialog, PromptPurpose};
use super::tasks::TaskOutcome;
use super::{PlumSpiritApp, files};
use crate::audio::TransportEvent;
use crate::diagnostics::format_bytes;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::model::{PlumDocument, SoundFontRef};
use std::path::{Path, PathBuf};

/// File name of the bundled default SoundFont (next to the executable).
pub const BUNDLED_SOUNDFONT: &str = "GeneralUser-GS.sf2";

/// The bundled SoundFont, if it was installed next to the executable.
pub fn bundled_soundfont() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("SoundFonts").join(BUNDLED_SOUNDFONT),
        dir.join(BUNDLED_SOUNDFONT),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

impl PlumSpiritApp {
    /// The SoundFont playback uses: the project's own (if the file exists), else
    /// the default from the settings, else the bundled one.
    pub(super) fn effective_soundfont(&self) -> Option<PathBuf> {
        let project = self
            .doc()
            .and_then(|d| d.project().soundfont.as_ref())
            .map(|s| PathBuf::from(&s.path))
            .filter(|p| p.is_file());
        project
            .or_else(|| {
                self.settings
                    .soundfont
                    .as_ref()
                    .map(PathBuf::from)
                    .filter(|p| p.is_file())
            })
            .or_else(bundled_soundfont)
    }

    pub(super) fn play_pause(&mut self) {
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let generation = o.generation;
        // L-B06: without a SoundFont the transport still plays a project
        // whose audible tracks are all on external outputs; it reports a
        // missing SoundFont itself when the internal synthesizer is needed.
        if !self.transport.state().is_running()
            && let Some(p) = self.effective_soundfont()
            && !self.transport.has_soundfont_for(&p)
        {
            self.transport.load_soundfont(p);
        }
        let doc = self.open.as_ref().map(|o| o.doc.clone());
        let starting = !self.transport.state().is_running();
        if let Some(doc) = doc
            && let Some(ev) = self.transport.toggle(&doc, generation)
        {
            self.report_transport_event(ev);
        }
        // Play is an explicit transport action: Follow Playhead resumes from the
        // current position.
        if starting && self.transport.state().is_running() {
            self.seek_follow_view = None;
            if self.settings.audio.prerender {
                self.preroll_watch = Some((std::time::Instant::now(), false));
            }
            self.follow_suspended = false;
            self.inspector.follow_suspended = false;
            self.sel_charts.follow_suspended = false;
            self.sel_charts.follow_suspended = false;
            if self.settings.follow_playhead {
                self.reveal_playhead();
            }
        }
    }

    /// Stop (B1-0080): ends playback; the playback playhead returns to the
    /// anchor (the Edit playhead, or where this playback started).
    pub(super) fn stop_playback(&mut self) {
        self.transport.stop();
        self.seek_follow_view = None;
        self.preroll_watch = None;
        self.follow_suspended = false;
        self.inspector.follow_suspended = false;
        self.sel_charts.follow_suspended = false;
        if self.settings.follow_playhead {
            self.reveal_playhead();
        }
    }

    /// Sends the playhead preferences (Playback menu) to the transport.
    pub(super) fn apply_playhead_policy(&mut self) {
        self.transport
            .set_playhead_policy(crate::audio::PlayheadPolicy {
                anchor: self.settings.anchor_playhead,
                pause_rewinds: self.settings.pause_rewinds,
            });
    }

    /// Moves the playhead / edit position. A seek is an explicit navigation, so
    /// the new position is revealed (and a paused Follow Playhead resumes).
    pub(super) fn seek_to(&mut self, tick: u64) {
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let generation = o.generation;
        let doc = o.doc.clone();
        self.roll.cursor_tick = tick;
        if let Some(ev) = self.transport.seek(tick, &doc, generation) {
            self.report_transport_event(ev);
        }
        if self.settings.audio.prerender && self.transport.state().is_running() {
            self.preroll_watch = Some((std::time::Instant::now(), false));
        }
        self.follow_suspended = false;
        self.inspector.follow_suspended = false;
        self.sel_charts.follow_suspended = false;
        self.seek_follow_view = None;
        self.reveal_song_tick(tick, doc.duration_ticks());
    }

    /// The transport speaks song ticks; the roll speaks content ticks.
    fn reveal_song_tick(&mut self, tick: u64, end: u64) {
        let shift = self.edit_shift();
        self.roll.reveal(
            super::clip_session::shifted(tick, -shift),
            super::clip_session::shifted(end, -shift),
        );
    }

    /// A ruler seek past the follow anchor should keep the current view until
    /// playback leaves it. A new axis or a manual view change ends this hold.
    pub(super) fn seek_from_roll(&mut self, tick: u64) {
        use crate::piano_roll::viewport::{FollowStyle, follow_origin};

        let time = self.roll.view.time;
        self.seek_to(tick);
        if self.transport.state().is_running()
            && self.settings.follow_playhead
            && time == self.roll.view.time
        {
            let width = self.roll.view_width();
            let local = self.to_content(tick);
            let end = self.to_content(self.doc().map_or(0, PlumDocument::duration_ticks));
            let style = if self.settings.smooth_follow {
                FollowStyle::Smooth
            } else {
                FollowStyle::Page
            };
            let anchor = f64::from(self.settings.follow_position.min(100)) / 100.0;
            if (0.0..=width).contains(&time.tick_to_x(local))
                && follow_origin(&time, width, local, end, style, anchor).is_some()
            {
                self.seek_follow_view = Some((time, self.edit_shift()));
            }
        }
    }

    /// Scrolls so the playhead / edit position is visible.
    pub(super) fn reveal_playhead(&mut self) {
        let end = self.doc().map_or(0, PlumDocument::duration_ticks);
        let pos = self.transport.position();
        self.seek_follow_view = None;
        self.reveal_song_tick(pos, end);
    }

    pub(super) fn update_transport(&mut self, _ctx: &egui::Context) {
        let generation = self.doc_generation();
        let doc = self.open.as_ref().map(|o| o.doc.clone());
        let events = self.transport.update(doc.as_deref(), generation);
        drop(doc);
        for ev in events {
            self.report_transport_event(ev);
        }
        self.watch_preroll();
    }

    /// §210: "Pre-rendered … s ahead" in the task list once the start buffer
    /// is full (only after the engine was seen filling it).
    fn watch_preroll(&mut self) {
        let Some((t0, filling)) = self.preroll_watch else {
            return;
        };
        match self.transport.prerolled() {
            Some(false) if self.transport.state().is_running() => {
                self.preroll_watch = Some((t0, true));
            }
            Some(true) if filling => {
                self.preroll_watch = None;
                let secs = f64::from(self.settings.audio.render_ahead_ms) / 1000.0;
                self.tasks.record(
                    format!("Pre-rendered {secs:.1} s of audio ahead"),
                    t0.elapsed(),
                    super::tasks::TaskOutcome::Done,
                );
            }
            // Never filled (Realtime, song end) or stopped: nothing to report.
            Some(true) if t0.elapsed() > std::time::Duration::from_secs(3) => {
                self.preroll_watch = None;
            }
            Some(true) => {}
            _ => self.preroll_watch = None,
        }
    }

    pub(super) fn report_transport_event(&mut self, ev: TransportEvent) {
        match ev {
            TransportEvent::SoundFontLoaded {
                name,
                path,
                bytes,
                elapsed,
            } => {
                self.tasks
                    .record("Loaded SoundFont", elapsed, TaskOutcome::Done);
                if self.pending_soundfont.as_deref() == Some(path.as_path()) {
                    self.pending_soundfont = None;
                    let s = path.to_string_lossy().into_owned();
                    if let Some(d) = self.doc_mut() {
                        d.project_mut().soundfont = Some(SoundFontRef { path: s.clone() });
                    }
                    self.settings.audio.remember_soundfont(&s);
                    self.settings.soundfont = Some(s);
                    self.save_settings();
                }
                self.set_status(
                    format!(
                        "SoundFont ready: {name} ({} of samples).",
                        format_bytes(bytes)
                    ),
                    StatusKind::Info,
                );
            }
            TransportEvent::SoundFontFailed {
                path,
                message,
                elapsed,
            } => {
                self.tasks
                    .record("SoundFont load", elapsed, TaskOutcome::Failed);
                if self.pending_soundfont.as_deref() == Some(path.as_path()) {
                    self.pending_soundfont = None;
                    self.show_error(
                        "Could not load the SoundFont.",
                        format!(
                            "{}\n\n{message}\n\nThe previous SoundFont is still in use.",
                            files::display_name(&path)
                        ),
                    );
                } else {
                    self.set_status(
                        format!("SoundFont {}: {message}", files::display_name(&path)),
                        StatusKind::Warning,
                    );
                }
            }
            TransportEvent::Unavailable(reason) => self.set_status(
                format!("Could not start playback. {reason}"),
                StatusKind::Warning,
            ),
            TransportEvent::Notice(msg) => self.set_status(msg, StatusKind::Warning),
            TransportEvent::Ended => {}
        }
    }

    pub(super) fn choose_soundfont(&mut self, frame: &eframe::Frame) {
        let dir = self
            .effective_soundfont()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| self.last_dir.clone());
        if !files::NATIVE_DIALOGS {
            self.dialog = Some(Dialog::PathPrompt {
                purpose: PromptPurpose::SoundFont,
                text: dir.map(|d| format!("{}/", d.display())).unwrap_or_default(),
                focused: false,
            });
            return;
        }
        self.perf.skip_current_frame();
        if let Some(p) = files::pick_open(frame, files::FileKind::SoundFont, dir.as_deref()) {
            self.soundfont_chosen(p);
        }
    }

    /// Loads the chosen SoundFont in the background; it becomes the project's and
    /// the default only after it loaded and validated.
    pub(super) fn soundfont_chosen(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        self.pending_soundfont = Some(path.clone());
        self.transport.load_soundfont(path);
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use plumspirit_engine::model::PlumDocument;

    /// B1-0080: the menu / chord turns the Edit / Anchor playhead on at the
    /// edit position; seeks move it; Stop (Shift+Space) returns there; the
    /// Pause policy reaches the transport; one transport drives every view.
    #[test]
    fn the_anchor_playhead_is_one_toggle_away_and_stop_returns_to_it() {
        let mut h = Harness::new(PlumDocument::new_project(
            "t".into(),
            Default::default(),
            480,
        ));
        assert_eq!(h.app.transport.anchor(), None, "off by default");
        h.app.seek_to(480);
        assert!(h.command(Command::ToggleAnchorPlayhead));
        assert!(h.app.settings.anchor_playhead);
        assert_eq!(
            h.app.transport.anchor(),
            Some(480),
            "placed at the edit position"
        );
        h.app.seek_to(1_920);
        assert_eq!(h.app.transport.anchor(), Some(1_920));
        assert_eq!(
            h.app.roll.cursor_tick, 1_920,
            "the roll shows the transport's position"
        );
        assert!(h.command(Command::Stop));
        assert_eq!(h.app.transport.position(), 1_920);
        assert_eq!(
            super::super::keybinds::shortcuts_of(Command::Stop),
            ["Shift+Space"]
        );
        assert_eq!(
            super::super::keybinds::shortcuts_of(Command::ToggleAnchorPlayhead),
            ["Ctrl+Shift+Space"]
        );
        h.app.settings.pause_rewinds = true;
        h.app.apply_playhead_policy();
        assert!(h.app.transport.playhead_policy().pause_rewinds);
        assert!(h.command(Command::ToggleAnchorPlayhead));
        assert_eq!(h.app.transport.anchor(), None);
    }
}
