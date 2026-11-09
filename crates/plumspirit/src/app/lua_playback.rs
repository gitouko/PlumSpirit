//! Bounded native transport sampling; worker dispatch uses the existing event inbox.
use super::PlumSpiritApp;
use crate::lua::{
    playback::Snapshot,
    reactive::{Data, Kind},
};
use std::time::{Duration, Instant};
const SAMPLE_INTERVAL: Duration = Duration::from_millis(50);
#[derive(Default)]
pub(super) struct Sampling {
    sampled: Option<Instant>,
    last: Option<Snapshot>,
}
impl PlumSpiritApp {
    pub(super) fn poll_lua_playback_controls(&mut self) {
        let cancel = self.transport.midi_watch().is_some_and(|w| {
            w.alive.load(std::sync::atomic::Ordering::Acquire)
                && (self
                    .lua_observation_stamp()
                    .is_none_or(|s| (s.serial, s.revision) != w.project)
                    || !self.lua_midi_allowed(&w.owner))
        });
        if cancel {
            self.transport.cancel_plugin_midi();
        }
        // One admitted or rejected request per frame; other queues remain bounded.
        let Some(request) = self
            .lua_plugins
            .as_ref()
            .and_then(|m| m.poll_playback_request())
        else {
            return;
        };
        self.apply_lua_playback_control(request);
    }
    fn lua_midi_allowed(&self, plugin: &str) -> bool {
        use crate::lua::Capability;
        self.settings.lua_plugins.run_plugins
            && self
                .settings
                .lua_plugins
                .installed
                .get(plugin)
                .is_some_and(|s| {
                    s.enabled
                        && self
                            .lua_plugins
                            .as_ref()
                            .and_then(|m| m.plugin(plugin))
                            .is_some_and(|p| {
                                let caps = p.effective(Some(s));
                                p.status == crate::lua::plugins::Status::Ready
                                    && caps.contains(&Capability::ReadPlayback)
                                    && caps.contains(&Capability::EmitMidi)
                            })
                })
    }
    pub(super) fn apply_lua_midi_intent(
        &mut self,
        plugin: &str,
        intent: crate::lua::playback::midi::Intent,
        lifecycle: &std::sync::Arc<crate::lua::reactive::Fence>,
    ) {
        let guard = intent.guard();
        let error = if !intent.valid() || !lifecycle.active() || intent.plugin().as_ref() != plugin
        {
            Some("The MIDI submission expired.")
        } else if !self.lua_midi_allowed(plugin) {
            Some("MIDI output permission is no longer granted.")
        } else if self.lua_playback_snapshot().is_none_or(|s| {
            s.stamp != guard.stamp
                || s.action_revision != guard.action_revision
                || s.configured_loop != guard.configured_loop
                || s.action_revision >= i64::MAX as u64
        }) {
            Some("The project or native transport changed.")
        } else if self.job.is_some() || self.dialog.is_some() || self.lua_file_dialog.is_some() {
            Some("The editor is busy.")
        } else if self
            .transport
            .midi_watch()
            .is_some_and(|w| w.alive.load(std::sync::atomic::Ordering::Acquire))
        {
            Some("Another plugin MIDI stream is active.")
        } else if self.transport.midi_resources().is_none() {
            Some("Native internal MIDI resources are unavailable.")
        } else if self
            .lua_playback_controls
            .admit(intent.plugin(), Instant::now())
            .is_err()
        {
            Some("Playback request rate limit reached.")
        } else {
            None
        };
        let result = if let Some(error) = error {
            Err(error)
        } else if let Some(voice) = intent.take_voice() {
            self.transport.submit_plugin_midi(
                voice,
                std::sync::Arc::clone(intent.plugin()),
                (guard.stamp.serial, guard.stamp.revision),
                lifecycle.native_flag(),
                intent.cancel_epoch(),
                intent.version(),
            )
        } else {
            Err("The MIDI submission was already consumed or canceled.")
        };
        let refused = result.is_err();
        if refused {
            intent.refuse();
        }
        let (text, kind) = match result {
            Ok(()) => (
                "MIDI plan queued for native admission; playback.midi_output reports its state."
                    .to_owned(),
                crate::ui::status_bar::StatusKind::Info,
            ),
            Err(error) => (
                format!("Staged MIDI plan discarded: {error}"),
                crate::ui::status_bar::StatusKind::Warning,
            ),
        };
        if let Some(m) = self.lua_plugins.as_mut() {
            m.response_log(plugin, &text);
        }
        self.set_status(text, kind);
        if refused {
            super::project::reap(intent);
        }
    }
    pub(super) fn apply_lua_playback_control(
        &mut self,
        request: crate::lua::playback::control::Request,
    ) {
        use crate::lua::{
            Capability,
            playback::control::{Action, Error},
        };
        let current = self.lua_playback_snapshot();
        let error = if request.lease.check().is_err()
            || !request.lease.matches_project(self.lua_observation_stamp())
        {
            Some(Error::Expired)
        } else if !request.lease.user_invoked() {
            Some(Error::NotUserInvoked)
        } else if !self.settings.lua_plugins.run_plugins
            || !self
                .settings
                .lua_plugins
                .installed
                .get(request.lease.plugin.as_ref())
                .is_some_and(|s| {
                    s.enabled
                        && self
                            .lua_plugins
                            .as_ref()
                            .and_then(|m| m.plugin(&request.lease.plugin))
                            .is_some_and(|p| {
                                let caps = p.effective(Some(s));
                                p.status == crate::lua::plugins::Status::Ready
                                    && caps.contains(&Capability::ReadPlayback)
                                    && caps.contains(&Capability::ControlPlayback)
                            })
                })
        {
            Some(Error::Permission)
        } else if current.is_none_or(|s| {
            s.stamp != request.expected.stamp
                || s.configured_loop != request.expected.configured_loop
                || s.action_revision != request.expected.action_revision
                || s.action_revision >= i64::MAX as u64
        }) {
            Some(Error::Conflict)
        } else if self.job.is_some() || self.dialog.is_some() || self.lua_file_dialog.is_some() {
            Some(Error::Busy)
        } else {
            self.lua_playback_controls
                .admit(&request.lease.plugin, Instant::now())
                .err()
        };
        if let Some(error) = error {
            request.respond(Err(error));
            return;
        }
        match request.action {
            Action::Play => {
                if !self.transport.state().is_running() {
                    self.play_pause();
                }
            }
            Action::Pause => {
                self.transport.pause();
            }
            Action::Stop => self.stop_playback(),
            Action::Seek(tick) => self.seek_to(tick),
            Action::Speed(speed) => self.commit_speed_text(&speed.to_string()),
        }
        request.respond(self.lua_playback_snapshot().ok_or(Error::Expired));
    }
    pub(super) fn apply_lua_loop_intent(
        &mut self,
        plugin: &str,
        intent: crate::lua::playback::loop_intent::Intent,
        lifecycle: &crate::lua::reactive::Fence,
    ) {
        use crate::lua::{Capability, playback::control::Error};
        let guard = intent.guard();
        let allowed = self.settings.lua_plugins.run_plugins
            && self
                .settings
                .lua_plugins
                .installed
                .get(plugin)
                .is_some_and(|s| {
                    s.enabled
                        && self
                            .lua_plugins
                            .as_ref()
                            .and_then(|m| m.plugin(plugin))
                            .is_some_and(|p| {
                                let caps = p.effective(Some(s));
                                p.status == crate::lua::plugins::Status::Ready
                                    && caps.contains(&Capability::ReadPlayback)
                                    && caps.contains(&Capability::ControlPlayback)
                            })
                });
        let error = if !intent.claim() || !lifecycle.active() || intent.plugin().as_ref() != plugin
        {
            Some(Error::Expired)
        } else if !allowed {
            Some(Error::Permission)
        } else if self.lua_playback_snapshot().is_none_or(|s| {
            s.stamp != guard.stamp
                || s.configured_loop != guard.configured_loop
                || s.action_revision != guard.action_revision
                || s.action_revision >= i64::MAX as u64
        }) {
            Some(Error::Conflict)
        } else if self.job.is_some()
            || self.dialog.is_some()
            || self.lua_file_dialog.is_some()
            || self
                .open
                .as_ref()
                .is_none_or(|o| std::sync::Arc::strong_count(&o.doc) != 1)
        {
            Some(Error::Busy)
        } else {
            self.lua_playback_controls
                .admit(intent.plugin(), Instant::now())
                .err()
        };
        if let Some(error) = error {
            let text = format!("Staged loop discarded: {}", error.lua());
            if let Some(m) = self.lua_plugins.as_mut() {
                m.response_log(plugin, &text);
            }
            self.set_status(text, crate::ui::status_bar::StatusKind::Warning);
            return;
        }
        let setting = intent.setting();
        if !setting.enabled && self.loop_config().enabled {
            self.toggle_loop();
        }
        self.set_loop_range(setting.range, setting.enabled);
    }
    pub(super) fn lua_playback_snapshot(&self) -> Option<Snapshot> {
        Some(Snapshot::capture(
            &self.transport,
            &self.open.as_ref()?.doc,
            self.lua_observation_stamp()?,
        ))
    }
    pub(super) fn sample_lua_playback(&mut self) {
        if self.open.is_none()
            || !self
                .lua_plugins
                .as_ref()
                .is_some_and(|m| m.observing_playback())
        {
            self.lua_playback = Sampling::default();
            return;
        }
        let now = Instant::now();
        if self
            .lua_playback
            .sampled
            .is_some_and(|t| now.saturating_duration_since(t) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.lua_playback.sampled = Some(now);
        let snapshot = self.lua_playback_snapshot();
        if snapshot == self.lua_playback.last {
            return;
        }
        self.lua_playback.last = snapshot;
        if let Some(snapshot) = snapshot {
            self.publish_lua_observation(Kind::TransportChanged, Data::Transport(snapshot));
        }
    }
}

#[cfg(test)]
#[path = "lua_playback_tests.rs"]
mod tests;
