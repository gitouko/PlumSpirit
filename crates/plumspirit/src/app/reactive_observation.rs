//! UI-thread metadata sampling only. Lua work is scheduled on plugin workers.
//! No note payload traversal, document clone or project root retained here.
use super::PlumSpiritApp;
use crate::lua::bridge::ProjectStamp;
use crate::lua::reactive::{Data, Delivery, Kind, Viewport};
use plumspirit_engine::model::{ClipId, TrackId};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Bound::{Excluded, Unbounded};
use std::time::{Duration, Instant};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(50);
const SAMPLE_STEP_ITEMS: usize = 2048;
const SAMPLE_STEP_TIME: Duration = Duration::from_millis(2);
#[derive(Default)]
pub(super) struct ObservationState {
    last: Option<Facts>,
    sampled: Option<Instant>,
    collecting: Option<Sampler>,
}
struct Facts {
    stamp: ProjectStamp,
    content: u64,
    tracks: u64,
    clips: u64,
    selection: u64,
    view: u64,
    selected_notes: u64,
    track_count: u32,
    clip_count: u32,
    structure_revision: u64,
    layout_revision: u64,
    active_track: Option<u32>,
    keys: u16,
    viewport: Viewport,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Context {
    stamp: ProjectStamp,
    structure: u64,
    layout: u64,
    clips: u64,
    conductor: u64,
    tracks: usize,
    contents: usize,
}
impl Context {
    fn read(app: &PlumSpiritApp) -> Option<Self> {
        let o = app.open.as_ref()?;
        Some(Self {
            stamp: app.lua_observation_stamp()?,
            structure: o.doc.structure_revision(),
            layout: o.doc.layout_revision(),
            clips: o.doc.clips().revision(),
            conductor: o.doc.conductor().revision(),
            tracks: o.doc.track_count(),
            contents: o.doc.clips().contents().len(),
        })
    }
}
/// Fixed-size progress only: root/store/selection iterators are reacquired each step.
struct Sampler {
    context: Context,
    track: usize,
    clip: usize,
    track_selection: Option<TrackId>,
    clip_selection: Option<ClipId>,
    stage: u8,
    content: DefaultHasher,
    tracks: DefaultHasher,
    clips: DefaultHasher,
    selection: DefaultHasher,
    selected_notes: u64,
    visited: usize,
}
impl Sampler {
    fn new(context: Context) -> Self {
        let mut out = Self {
            context,
            track: 0,
            clip: 0,
            track_selection: None,
            clip_selection: None,
            stage: 0,
            content: DefaultHasher::new(),
            tracks: DefaultHasher::new(),
            clips: DefaultHasher::new(),
            selection: DefaultHasher::new(),
            selected_notes: 0,
            visited: 0,
        };
        context.structure.hash(&mut out.tracks);
        context.layout.hash(&mut out.tracks);
        context.clips.hash(&mut out.clips);
        context.conductor.hash(&mut out.content);
        out
    }
    fn step(&mut self, app: &PlumSpiritApp) -> Option<Facts> {
        let o = app.open.as_ref()?;
        let doc = &o.doc;
        let start = Instant::now();
        self.visited = 0;
        while self.stage < 4 {
            if self.visited >= SAMPLE_STEP_ITEMS
                || (self.visited.is_multiple_of(64) && start.elapsed() >= SAMPLE_STEP_TIME)
            {
                return None;
            }
            match self.stage {
                0 => {
                    let Some(t) = doc.tracks().get(self.track) else {
                        self.stage = 1;
                        continue;
                    };
                    (
                        t.id(),
                        t.parent(),
                        t.props_revision(),
                        t.subchannels_revision(),
                        t.mute,
                        t.solo,
                    )
                        .hash(&mut self.tracks);
                    (t.id(), t.notes.content_revision(), t.events_revision())
                        .hash(&mut self.content);
                    (t.id(), t.notes.revision(), t.notes.selected_count())
                        .hash(&mut self.selection);
                    self.selected_notes = self
                        .selected_notes
                        .saturating_add(t.notes.selected_count() as u64);
                    self.track += 1;
                }
                1 => {
                    let Some(c) = doc.clips().contents().get(self.clip) else {
                        self.stage = 2;
                        continue;
                    };
                    (c.id, c.notes.content_revision(), c.events_revision()).hash(&mut self.content);
                    (c.id, c.notes.revision(), c.notes.selected_count()).hash(&mut self.selection);
                    self.selected_notes = self
                        .selected_notes
                        .saturating_add(c.notes.selected_count() as u64);
                    self.clip += 1;
                }
                2 => {
                    let next = if let Some(last) = self.track_selection {
                        app.selected_tracks
                            .range((Excluded(last), Unbounded))
                            .next()
                    } else {
                        app.selected_tracks.iter().next()
                    };
                    let Some(id) = next else {
                        self.stage = 3;
                        continue;
                    };
                    id.hash(&mut self.selection);
                    self.track_selection = Some(*id);
                }
                3 => {
                    let next = if let Some(last) = self.clip_selection {
                        app.track_list
                            .selected
                            .range((Excluded(last), Unbounded))
                            .next()
                    } else {
                        app.track_list.selected.iter().next()
                    };
                    let Some(id) = next else {
                        self.stage = 4;
                        continue;
                    };
                    id.hash(&mut self.selection);
                    self.clip_selection = Some(*id);
                }
                _ => unreachable!(),
            }
            self.visited += 1;
        }
        let content = self.content.finish();
        content.hash(&mut self.clips);
        app.time_selection.hash(&mut self.selection);
        let roll = app.roll.view;
        let viewport = Viewport::new(
            roll.time.origin_tick,
            roll.time.pixels_per_tick,
            roll.pitch.scroll_rows,
            roll.pitch.row_height,
            roll.pitch.top,
        )?;
        let keys = doc.project().keys.keys() as u16;
        let active_track = doc.track(o.active_track).map(|t| t.id().0);
        let mut view = DefaultHasher::new();
        (
            viewport.origin_tick,
            viewport.pixels_per_tick,
            viewport.scroll_rows,
            viewport.row_height,
            viewport.top,
            active_track,
            keys,
            o.conductor_selected,
        )
            .hash(&mut view);
        o.clip_session.map(|s| s.clip).hash(&mut view);
        Some(Facts {
            stamp: self.context.stamp,
            content,
            tracks: self.tracks.finish(),
            clips: self.clips.finish(),
            selection: self.selection.finish(),
            view: view.finish(),
            selected_notes: self.selected_notes,
            track_count: self.context.tracks as u32,
            clip_count: doc.clips().explicit_placements().len() as u32,
            structure_revision: self.context.structure,
            layout_revision: self.context.clips,
            active_track,
            keys,
            viewport,
        })
    }
}
impl PlumSpiritApp {
    pub(super) fn lua_observation_stamp(&self) -> Option<ProjectStamp> {
        self.open.as_ref().map(|o| ProjectStamp {
            serial: o.history.incarnations().project(),
            revision: o.history.current_state(),
        })
    }
    pub(super) fn lua_observation_delivery_valid(&self, d: &Delivery) -> bool {
        d.valid()
            && if d.event.kind == Kind::ProjectClosed {
                self.open.is_none()
            } else {
                self.lua_observation_stamp() == Some(d.event.stamp)
            }
    }
    pub(super) fn publish_lua_observation(&mut self, kind: Kind, data: Data) {
        let stamp = self.lua_observation_stamp();
        if let (Some(manager), Some(stamp)) = (&mut self.lua_plugins, stamp) {
            manager.set_automation_barrier(self.lua_automation.blocked(stamp).then_some(stamp));
            manager.observation_context(Some(stamp));
            manager.publish_observation(kind, stamp, data);
        }
    }
    pub(super) fn sample_lua_observation(&mut self) {
        let stamp = self.lua_observation_stamp();
        let Some(manager) = self.lua_plugins.as_mut() else {
            return;
        };
        manager.set_automation_barrier(stamp.filter(|s| self.lua_automation.blocked(*s)));
        manager.observation_context(stamp);
        if !manager.observing() || stamp.is_none() {
            self.lua_observation = ObservationState::default();
            return;
        }
        let now = Instant::now();
        if self
            .lua_observation
            .sampled
            .is_some_and(|t| now.duration_since(t) < SAMPLE_INTERVAL)
            && self.lua_observation.collecting.is_none()
        {
            return;
        }
        let Some(context) = Context::read(self) else {
            return;
        };
        let mut sampler = self
            .lua_observation
            .collecting
            .take()
            .filter(|s| s.context == context)
            .unwrap_or_else(|| Sampler::new(context));
        let Some(facts) = sampler.step(self) else {
            self.lua_observation.collecting = Some(sampler);
            return;
        };
        self.lua_observation.sampled = Some(now);
        if let Some(old) = self.lua_observation.last.as_ref()
            && old.stamp.serial == facts.stamp.serial
        {
            // Automatic jobs cannot publish explicit selection changes. Once a
            // child's content sample has settled, a later selection-only change
            // is a new external cause, including an explicit user plugin command.
            if old.stamp == facts.stamp
                && old.content == facts.content
                && old.selection != facts.selection
            {
                self.lua_automation.external_selection(facts.stamp);
            }
            let mut pending = Vec::with_capacity(5);
            if old.content != facts.content || old.stamp.revision != facts.stamp.revision {
                pending.push((
                    Kind::ContentChanged,
                    Data::Content {
                        previous_revision: old.stamp.revision,
                    },
                ));
            }
            if old.tracks != facts.tracks {
                pending.push((
                    Kind::TracksChanged,
                    Data::Tracks {
                        structure_revision: facts.structure_revision,
                        tracks: facts.track_count,
                    },
                ));
            }
            if old.clips != facts.clips {
                pending.push((
                    Kind::ClipsChanged,
                    Data::Clips {
                        layout_revision: facts.layout_revision,
                        placements: facts.clip_count,
                    },
                ));
            }
            if old.selection != facts.selection {
                pending.push((
                    Kind::SelectionChanged,
                    Data::Selection {
                        notes: facts.selected_notes,
                        tracks: self.selected_tracks.len() as u32,
                        clips: self.track_list.selected.len() as u32,
                    },
                ));
            }
            if old.view != facts.view {
                pending.push((
                    Kind::ViewChanged,
                    Data::View {
                        active_track: facts.active_track,
                        keys: facts.keys,
                        viewport: facts.viewport,
                    },
                ));
            }
            for (kind, data) in pending {
                self.publish_lua_observation(kind, data);
            }
        }
        self.lua_observation.last = Some(facts);
    }
    fn response_log(&mut self, plugin: &str, text: impl Into<String>) {
        if let Some(manager) = self.lua_plugins.as_mut() {
            manager.response_log(plugin, text);
        }
    }
    pub(super) fn start_lua_response(
        &mut self,
        plugin: &str,
        delivery: Delivery,
        request: crate::lua::response::Request,
        dark: bool,
    ) {
        if !self.lua_observation_delivery_valid(&delivery)
            || !delivery.event.transaction_allowed
            || !self
                .lua_plugins
                .as_ref()
                .is_some_and(|m| m.response_permitted(plugin, &self.settings.lua_plugins))
        {
            return;
        }
        let stamp = delivery.event.stamp;
        if !self.lua_automation.request(stamp, Instant::now()) {
            self.response_log(
                plugin,
                "Automatic response skipped by the shared causal/cooldown guard.",
            );
            return;
        }
        if self
            .open
            .as_ref()
            .is_none_or(|o| !crate::lua::response::metadata_eligible(&o.doc))
        {
            self.response_log(plugin,"Automatic response metadata exceeds 4,096 identity objects; use an explicit command.");
            return;
        }
        if self.selected_tracks.len() > crate::lua::response::MAX_OBJECTS
            || self.track_list.selected.len() > crate::lua::response::MAX_OBJECTS
        {
            self.response_log(plugin,"Automatic response selection metadata exceeds 4,096 objects; use an explicit command.");
            return;
        }
        let Some(world) = self.lua_world(dark) else {
            return;
        };
        let prefs = self.settings.lua_plugins.clone();
        if let Some(manager) = self.lua_plugins.as_mut()
            && let Err(error) = manager.queue_response(plugin, delivery, request, world, &prefs)
        {
            self.response_log(plugin, error);
        }
    }
    pub(super) fn apply_lua_response(
        &mut self,
        plugin: &str,
        delivery: Delivery,
        commits: Option<crate::lua::tx::Commits>,
        guard: crate::lua::response::SnapshotGuard,
        notices: Vec<(String, bool)>,
    ) {
        if !self.lua_observation_delivery_valid(&delivery)
            || self.lua_automation.blocked(delivery.event.stamp)
            || !self
                .lua_plugins
                .as_ref()
                .is_some_and(|m| m.response_permitted(plugin, &self.settings.lua_plugins))
        {
            return;
        }
        let Some(o) = self.open.as_mut() else {
            return;
        };
        if !guard.matches(&o.doc, delivery.event.stamp)
            || guard.info.active_track
                != if o.conductor_selected {
                    None
                } else {
                    o.doc.track(o.active_track).map(|t| t.id())
                }
            || guard.info.time_selection != self.time_selection
            || !guard
                .info
                .selected_tracks
                .iter()
                .eq(self.selected_tracks.iter())
            || !guard
                .info
                .selected_clips
                .iter()
                .eq(self.track_list.selected.iter())
        {
            self.response_log(
                plugin,
                "Automatic response discarded: the captured project or selection changed.",
            );
            return;
        }
        let modifies = commits.is_some();
        let result = if let Some(mut commits) = commits {
            if commits.needs_confirmation() {
                self.response_log(
                    plugin,
                    "Automatic response requires explicit confirmation; nothing was applied.",
                );
                return;
            }
            commits
                .bind_response_base(&o.doc)
                .and_then(|()| commits.apply(&mut o.history, &mut o.doc))
        } else {
            Ok(0)
        };
        match result {
            Ok(_) => {
                let stamp = self.lua_observation_stamp();
                if modifies {
                    self.lua_automation.block(stamp);
                    if let Some(manager) = self.lua_plugins.as_mut() {
                        manager.set_automation_barrier(stamp);
                        manager.observation_context(stamp);
                    }
                }
                for (text, error) in notices {
                    self.set_status(
                        text,
                        if error {
                            crate::ui::status_bar::StatusKind::Error
                        } else {
                            crate::ui::status_bar::StatusKind::Info
                        },
                    );
                }
            }
            Err(error) => self.response_log(plugin, error.user_message()),
        }
    }
    pub(super) fn lua_observation_sampling(&self) -> bool {
        self.lua_observation.collecting.is_some()
    }
    #[cfg(test)]
    fn lua_observation_facts(&self) -> Option<Facts> {
        let mut sampler = Sampler::new(Context::read(self)?);
        loop {
            if let Some(facts) = sampler.step(self) {
                return Some(facts);
            }
        }
    }
}

#[cfg(test)]
#[path = "reactive_observation_tests.rs"]
mod tests;
