//! Native geometry capture only for enabled, authorized overlay plugins.
use super::{PlumSpiritApp, track_list::MainView};
use crate::lua::{
    Capability,
    plugins::Status,
    visual::{Facts, Snapshot},
};
impl PlumSpiritApp {
    pub(super) fn capture_lua_views(&mut self) {
        let enabled = self.settings.lua_plugins.run_plugins
            && self.settings.lua_plugins.installed.iter().any(|(id, s)| {
                s.enabled
                    && s.granted.iter().any(|c| c == "ui.overlay")
                    && self
                        .lua_plugins
                        .as_ref()
                        .and_then(|m| m.plugin(id))
                        .is_some_and(|p| {
                            if p.status != Status::Ready {
                                return false;
                            }
                            let caps = p.effective(Some(s));
                            caps.contains(&Capability::UiOverlay)
                                && (caps.contains(&Capability::ReadProject)
                                    || caps.contains(&Capability::ReadPlayback))
                        })
            });
        self.lua_views.enable(enabled);
        self.roll.capture_view(enabled);
    }
    /// Fixed native metadata only; scope changes invalidate delivery every frame,
    /// while observation publication is sampled at20Hz and worker dispatch at8Hz.
    pub(super) fn sample_lua_surfaces(&mut self) {
        if self.lua_plugins.is_none() {
            return;
        }
        let views = self.lua_view_snapshots();
        let playback = if views.iter().any(Option::is_some)
            && self
                .lua_plugins
                .as_ref()
                .unwrap()
                .observing_overlay_playback()
        {
            self.lua_playback_snapshot()
        } else {
            None
        };
        self.lua_plugins
            .as_mut()
            .unwrap()
            .set_observation_playback(playback);

        self.lua_plugins
            .as_mut()
            .unwrap()
            .set_observation_views(views);
        if !self.lua_plugins.as_ref().unwrap().observing_surfaces() {
            self.lua_views.notified_project = None;
            self.lua_views.notified_at = None;
            return;
        }
        let Some(stamp) = self.lua_observation_stamp() else {
            self.lua_views.notified_project = None;
            self.lua_views.notified_at = None;
            return;
        };
        let now = std::time::Instant::now();
        if self
            .lua_views
            .notified_at
            .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_millis(50))
        {
            return;
        }
        let first = self.lua_views.notified_project != Some(stamp.serial);
        self.lua_views.notified_at = Some(now);
        self.lua_views.notified_project = Some(stamp.serial);
        for (i, view) in views.into_iter().enumerate() {
            if first || self.lua_views.notified[i] != view {
                self.publish_lua_observation(
                    crate::lua::reactive::Kind::SurfaceChanged,
                    crate::lua::reactive::Data::Surface {
                        index: i as u8,
                        view_revision: view.map(|v| v.revision),
                    },
                );
                self.lua_views.notified[i] = view;
            }
        }
    }
    pub(super) fn lua_view_snapshots(&mut self) -> [Option<Snapshot>; 2] {
        let Some(stamp) = self.lua_observation_stamp() else {
            return [None, None];
        };
        let player = self.player_shown();
        let roll = if !player && self.main_view == MainView::PianoRoll {
            self.roll
                .view_frame()
                .filter(|frame| self.open.as_ref().is_some_and(|o| frame.matches(&o.doc)))
                .map(Facts::Roll)
        } else {
            None
        };
        let player = if player {
            self.lua_views
                .player
                .filter(|frame| frame.stamp == stamp)
                .map(Facts::Player)
        } else {
            None
        };
        [
            self.lua_views.sample(0, stamp, roll),
            self.lua_views.sample(1, stamp, player),
        ]
    }
}
#[cfg(test)]
#[path = "lua_views_tests.rs"]
pub(super) mod tests;
