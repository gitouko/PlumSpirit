//! Bounded project observation. Native metadata crosses the worker boundary;
//! author functions stay in their own Lua VM. No UI/audio-thread Lua dispatch.
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, Table, Value};

use super::bridge::ProjectStamp;
use super::bridge::{NativeBudget, Reservation};
use super::plugin_api::{Decls, PluginSlot};
use super::{Capability, Category, Host, stop};

pub const MAX_SUBSCRIPTIONS: usize = 32;
pub const MAX_PENDING: usize = 64;
pub const MAX_DEBOUNCE_MS: u32 = 5000;
pub const MIN_DEBOUNCE_MS: u32 = 20;
pub const MAX_LATENCY: Duration = Duration::from_millis(500);
pub const RUN_TIME: Duration = Duration::from_millis(250);
pub const MIN_JOB_INTERVAL: Duration = Duration::from_millis(125);
const HANDLERS: &str = "plumspirit.plugin.observers";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    ProjectOpened,
    ProjectClosed,
    ContentChanged,
    SelectionChanged,
    TracksChanged,
    ClipsChanged,
    Undo,
    Redo,
    Saved,
    ViewChanged,
    TransportChanged,
    SurfaceChanged,
}
impl Kind {
    pub const ALL: [Kind; 12] = [
        Self::ProjectOpened,
        Self::ProjectClosed,
        Self::ContentChanged,
        Self::SelectionChanged,
        Self::TracksChanged,
        Self::ClipsChanged,
        Self::Undo,
        Self::Redo,
        Self::Saved,
        Self::ViewChanged,
        Self::TransportChanged,
        Self::SurfaceChanged,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::ProjectOpened => "project_opened",
            Self::ProjectClosed => "project_closed",
            Self::ContentChanged => "content_changed",
            Self::SelectionChanged => "selection_changed",
            Self::TracksChanged => "tracks_changed",
            Self::ClipsChanged => "clips_changed",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Saved => "saved",
            Self::ViewChanged => "view_changed",
            Self::TransportChanged => "transport_changed",
            Self::SurfaceChanged => "surface_changed",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserverDecl {
    pub id: String,
    pub kinds: Vec<Kind>,
    pub debounce_ms: u32,
}

/// Each event kind has one specific finite payload, supplied by the native owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    Surface {
        index: u8,
        view_revision: Option<u64>,
    },
    Transport(super::playback::Snapshot),
    Project {
        tracks: u32,
        notes: u64,
        keys: u16,
    },
    Closed,
    Content {
        previous_revision: u64,
    },
    Selection {
        notes: u64,
        tracks: u32,
        clips: u32,
    },
    Tracks {
        structure_revision: u64,
        tracks: u32,
    },
    Clips {
        layout_revision: u64,
        placements: u32,
    },
    History {
        previous_revision: u64,
    },
    Saved {
        midi: bool,
    },
    View {
        active_track: Option<u32>,
        keys: u16,
        viewport: Viewport,
    },
}
/// Finite viewport facts stored as bits so comparison never rounds tick origins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub origin_tick: u64,
    pub pixels_per_tick: u64,
    pub scroll_rows: u64,
    pub row_height: u64,
    pub top: u8,
}
impl Viewport {
    pub fn new(
        origin_tick: u64,
        pixels_per_tick: f64,
        scroll_rows: f64,
        row_height: f64,
        top: u8,
    ) -> Option<Self> {
        (pixels_per_tick.is_finite()
            && pixels_per_tick > 0.0
            && scroll_rows.is_finite()
            && row_height.is_finite()
            && row_height > 0.0)
            .then_some(Self {
                origin_tick,
                pixels_per_tick: pixels_per_tick.to_bits(),
                scroll_rows: scroll_rows.to_bits(),
                row_height: row_height.to_bits(),
                top,
            })
    }
}
impl Data {
    pub fn matches(&self, kind: Kind) -> bool {
        matches!(
            (kind, self),
            (Kind::TransportChanged, Self::Transport(_))
                | (Kind::SurfaceChanged, Self::Surface { .. })
                | (Kind::ProjectOpened, Self::Project { .. })
                | (Kind::ProjectClosed, Self::Closed)
                | (Kind::ContentChanged, Self::Content { .. })
                | (Kind::SelectionChanged, Self::Selection { .. })
                | (Kind::TracksChanged, Self::Tracks { .. })
                | (Kind::ClipsChanged, Self::Clips { .. })
                | (Kind::Undo | Kind::Redo, Self::History { .. })
                | (Kind::Saved, Self::Saved { .. })
                | (Kind::ViewChanged, Self::View { .. })
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub stamp: ProjectStamp,
    pub sequence: u64,
    pub first_sequence: u64,
    pub coalesced: u32,
    pub data: Data,
    pub transaction_allowed: bool,
}
impl Event {
    pub fn new(kind: Kind, stamp: ProjectStamp, sequence: u64, data: Data) -> Option<Self> {
        (data.matches(kind)
            && match &data {
                Data::Transport(s) => s.stamp == stamp && s.valid(),
                Data::Surface {
                    index,
                    view_revision,
                } => {
                    *index < 2
                        && view_revision.is_none_or(|r| r > 0 && r <= super::ticks::MAX_LUA_TICK)
                }
                _ => true,
            })
        .then_some(Self {
            kind,
            stamp,
            sequence,
            first_sequence: sequence,
            coalesced: 0,
            data,
            transaction_allowed: !matches!(
                kind,
                Kind::ProjectClosed
                    | Kind::Undo
                    | Kind::Redo
                    | Kind::TransportChanged
                    | Kind::SurfaceChanged
            ),
        })
    }
    pub(super) fn table(&self, lua: &Lua, id: &str) -> mlua::Result<Table> {
        let out = lua.create_table()?;
        let data = match &self.data {
            Data::Transport(s) => s.table(lua)?,
            _ => lua.create_table()?,
        };
        let exact = |value| super::ticks::to_lua(value);
        out.set("schema", 1)?;
        out.set("context", "metadata")?;
        out.set("subscription", id)?;
        out.set("kind", self.kind.name())?;
        out.set("project", exact(self.stamp.serial)?)?;
        out.set("revision", exact(self.stamp.revision)?)?;
        out.set("sequence", exact(self.sequence)?)?;
        out.set("first_sequence", exact(self.first_sequence)?)?;
        out.set("coalesced", self.coalesced)?;
        out.set("read_only", true)?;
        out.set("transaction_allowed", self.transaction_allowed)?;
        match self.data {
            Data::Surface {
                index,
                view_revision,
            } => {
                data.set("surface", if index == 0 { "piano_roll" } else { "player" })?;
                data.set("visible", view_revision.is_some())?;
                data.set("view_revision", view_revision.map(exact).transpose()?)?;
            }
            Data::Transport(_) => {}
            Data::Project {
                tracks,
                notes,
                keys,
            } => {
                data.set("tracks", tracks)?;
                data.set("notes", exact(notes)?)?;
                data.set("keys", keys)?;
            }
            Data::Closed => {}
            Data::Content { previous_revision } | Data::History { previous_revision } => {
                data.set("previous_revision", exact(previous_revision)?)?;
            }
            Data::Selection {
                notes,
                tracks,
                clips,
            } => {
                data.set("notes", exact(notes)?)?;
                data.set("tracks", tracks)?;
                data.set("clips", clips)?;
            }
            Data::Tracks {
                structure_revision,
                tracks,
            } => {
                data.set("structure_revision", exact(structure_revision)?)?;
                data.set("tracks", tracks)?;
            }
            Data::Clips {
                layout_revision,
                placements,
            } => {
                data.set("layout_revision", exact(layout_revision)?)?;
                data.set("placements", placements)?;
            }
            Data::Saved { midi } => {
                data.set("format", if midi { "midi" } else { "lup" })?;
            }
            Data::View {
                active_track,
                keys,
                viewport,
            } => {
                data.set("active_track", active_track)?;
                data.set("keys", keys)?;
                let v = lua.create_table()?;
                // Decimal text stays exact even beyond Lua's signed integer domain.
                v.set("origin_tick", viewport.origin_tick.to_string())?;
                v.set("pixels_per_tick", f64::from_bits(viewport.pixels_per_tick))?;
                v.set("scroll_rows", f64::from_bits(viewport.scroll_rows))?;
                v.set("row_height", f64::from_bits(viewport.row_height))?;
                v.set("top", viewport.top)?;
                data.set("viewport", v)?;
            }
        }
        out.set("data", data)?;
        Ok(out)
    }
}

#[derive(Default, Debug)]
pub struct Fence(Arc<AtomicBool>);
impl Fence {
    pub fn live() -> Arc<Self> {
        Arc::new(Self(Arc::new(AtomicBool::new(true))))
    }
    pub fn invalidate(&self) {
        self.0.store(false, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn native_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}
#[derive(Clone, Debug)]
pub struct Delivery {
    pub views: [Option<super::visual::Snapshot>; 2],
    pub playback: Option<super::playback::Snapshot>,
    pub id: String,
    pub event: Event,
    pub fence: Arc<Fence>,
    pub lifecycle: Arc<Fence>,
    pub dropped: u64,
}

impl Delivery {
    pub fn valid(&self) -> bool {
        self.fence.active()
            && self.lifecycle.active()
            && self
                .playback
                .is_none_or(|s| s.stamp == self.event.stamp && s.valid())
            && self
                .views
                .iter()
                .flatten()
                .all(|v| v.stamp == self.event.stamp && v.facts.valid())
            && match self.event.data {
                Data::Surface {
                    index,
                    view_revision,
                } => {
                    usize::from(index) < self.views.len()
                        && self.views[usize::from(index)].map(|v| v.revision) == view_revision
                }
                _ => true,
            }
    }
}

struct Waiting {
    id: String,
    event: Event,
    first: Instant,
    due: Instant,
    epoch: u64,
}
#[derive(Default)]
pub struct Inbox {
    pending: VecDeque<Waiting>,
    epoch: u64,
    active: Option<Arc<Fence>>,
    last_started: Option<Instant>,
    pub dropped: u64,
    allocation: Option<Reservation>,
}
impl Inbox {
    pub fn len(&self) -> usize {
        self.pending.len()
    }
    pub fn clear(&mut self) {
        self.pending = VecDeque::new();
        self.allocation = None;
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(fence) = self.active.take() {
            fence.invalidate();
        }
    }
    pub fn sync(&mut self, decls: &Decls) {
        self.pending.retain(|w| {
            decls
                .observers
                .iter()
                .any(|d| d.id == w.id && d.kinds.contains(&w.event.kind))
        });
    }
    pub fn invalidate_running(&mut self) {
        if let Some(f) = self.active.take() {
            f.invalidate();
        }
    }
    pub fn push(
        &mut self,
        decls: &Decls,
        event: &Event,
        now: Instant,
        budget: &NativeBudget,
    ) -> Result<(), super::bridge::BridgeError> {
        if !decls
            .observers
            .iter()
            .any(|d| d.kinds.contains(&event.kind))
        {
            return Ok(());
        }
        if self.allocation.is_none() {
            self.allocation =
                Some(budget.reserve(MAX_PENDING * (std::mem::size_of::<Waiting>() + 64) + 4096)?);
        }
        for decl in decls
            .observers
            .iter()
            .filter(|d| d.kinds.contains(&event.kind))
        {
            let delay = Duration::from_millis(u64::from(decl.debounce_ms));
            if let Some(w) = self.pending.iter_mut().find(|w| {
                w.id == decl.id
                    && w.event.kind == event.kind
                    && w.event.stamp.serial == event.stamp.serial
                    && match (&w.event.data, &event.data) {
                        (Data::Surface { index: a, .. }, Data::Surface { index: b, .. }) => a == b,
                        _ => true,
                    }
            }) {
                let previous = match w.event.data {
                    Data::Content { previous_revision } | Data::History { previous_revision } => {
                        Some(previous_revision)
                    }
                    _ => None,
                };
                let first_sequence = w.event.first_sequence;
                let count = w.event.coalesced.saturating_add(1);
                w.event = event.clone();
                w.event.first_sequence = first_sequence;
                w.event.coalesced = count;
                if let Some(first) = previous {
                    match &mut w.event.data {
                        Data::Content { previous_revision }
                        | Data::History { previous_revision } => *previous_revision = first,
                        _ => {}
                    }
                }
                w.due = (now + delay).min(w.first + MAX_LATENCY.max(delay));
            } else {
                if self.pending.len() == MAX_PENDING {
                    self.pending.pop_front();
                    self.dropped = self.dropped.saturating_add(1);
                }
                self.pending.push_back(Waiting {
                    id: decl.id.clone(),
                    event: event.clone(),
                    first: now,
                    due: now + delay,
                    epoch: self.epoch,
                });
            }
        }
        Ok(())
    }
    pub fn due(&self, now: Instant) -> bool {
        self.last_started
            .is_none_or(|last| now.saturating_duration_since(last) >= MIN_JOB_INTERVAL)
            && self.pending.iter().any(|w| w.due <= now)
    }
    pub fn take(&mut self, now: Instant, current: Option<ProjectStamp>) -> Option<Delivery> {
        if !self.due(now) {
            return None;
        }
        while let Some(index) = self.pending.iter().position(|w| w.due <= now) {
            let w = self.pending.remove(index)?;
            let valid = if w.event.kind == Kind::ProjectClosed {
                current.is_none()
            } else {
                current == Some(w.event.stamp)
            };
            if w.epoch != self.epoch || !valid {
                self.dropped = self.dropped.saturating_add(1);
                continue;
            }
            let fence = Fence::live();
            self.active = Some(Arc::clone(&fence));
            self.last_started = Some(now);
            return Some(Delivery {
                views: [None, None],
                playback: None,
                id: w.id,
                event: w.event,
                fence,
                lifecycle: Fence::live(),
                dropped: self.dropped,
            });
        }
        None
    }
}

#[derive(Default)]
pub(super) struct Scope {
    pub delivery: Option<Delivery>,
    pub request: Option<super::response::Request>,
}
pub(super) fn view_project(lua: &Lua) -> Option<ProjectStamp> {
    lua.app_data_ref::<Scope>().and_then(|s| {
        s.delivery
            .as_ref()
            .filter(|d| {
                d.valid()
                    && (d.event.kind == Kind::SurfaceChanged || d.views.iter().any(Option::is_some))
            })
            .map(|d| d.event.stamp)
    })
}
pub(super) fn valid(lua: &Lua) -> bool {
    lua.app_data_ref::<Scope>()
        .is_none_or(|s| s.delivery.as_ref().is_none_or(|d| d.valid()))
}
pub(super) fn check_read(lua: &Lua) -> mlua::Result<()> {
    super::services::check_read(lua)?;
    if !valid(lua) {
        return Err(stop(
            Category::Expired,
            "This observation belongs to an expired project revision or plugin lifecycle.",
        ));
    }
    Ok(())
}
pub(super) fn check_write(lua: &Lua) -> mlua::Result<()> {
    super::services::check_write(lua)?;
    check_read(lua)?;
    if lua
        .app_data_ref::<Scope>()
        .is_some_and(|s| s.delivery.is_some())
    {
        return Err(stop(
            Category::Capability,
            "Observation handlers are read-only; use an explicitly invoked command or tool to edit.",
        ));
    }
    Ok(())
}
pub(super) fn responding(lua: &Lua) -> bool {
    lua.app_data_ref::<Scope>()
        .is_some_and(|s| s.request.is_some())
}
pub(super) fn check_transaction_write(lua: &Lua) -> mlua::Result<()> {
    super::services::check_write(lua)?;
    check_read(lua)?;
    if responding(lua) {
        return super::response::permission(lua);
    }
    check_write(lua)
}
pub(super) fn observing(lua: &Lua) -> bool {
    lua.app_data_ref::<Scope>()
        .is_some_and(|s| s.delivery.is_some())
}

fn permission(lua: &Lua, kinds: &[Kind]) -> mlua::Result<()> {
    let granted = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    if kinds.contains(&Kind::TransportChanged) {
        Host::require(&granted, Capability::ReadPlayback)?;
    }
    if kinds.contains(&Kind::SurfaceChanged) {
        Host::require(&granted, Capability::UiOverlay)?;
        if !granted.contains(&Capability::ReadProject)
            && !granted.contains(&Capability::ReadPlayback)
        {
            Host::require(&granted, Capability::ReadProject)?;
        }
    }
    if kinds
        .iter()
        .any(|k| !matches!(k, Kind::TransportChanged | Kind::SurfaceChanged))
    {
        Host::require(&granted, Capability::ReadProject)?;
        Host::require(&granted, Capability::ObserveProject)?;
    }
    Ok(())
}
fn runtime(message: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn observer_id(value: Value) -> mlua::Result<String> {
    let Value::String(id) = value else {
        return Err(runtime("An observer ID must be text."));
    };
    if id.as_bytes().len() > 64 {
        return Err(runtime("An observer ID may contain at most 64 bytes."));
    }
    let id = id.to_str()?.to_owned();
    super::plugin_api::check_decl_id(&id, "observer")?;
    Ok(id)
}
pub(super) fn install(lua: &Lua, plugin: &Table) -> mlua::Result<()> {
    lua.set_named_registry_value(HANDLERS, lua.create_table()?)?;
    plugin.set(
        "observe",
        lua.create_function(|lua, opts: Table| {
            check_read(lua)?;
            if observing(lua) {
                return Err(runtime(
                    "Observers cannot register new observers during delivery.",
                ));
            }
            super::plugin_api::require_loading(lua, "Project observers")?;
            if lua
                .app_data_ref::<super::plugin_api::PluginSlot>()
                .is_some_and(|s| s.extensibility_ready)
            {
                return Err(stop(
                    Category::Runtime,
                    "These declarations are frozen until plugin reload.",
                ));
            }
            if opts.metatable().is_some() {
                return Err(runtime("Observer options must be a plain table."));
            }
            let id = observer_id(opts.raw_get("id")?)?;
            let events: Table = opts.raw_get("events")?;
            if events.metatable().is_some()
                || events.raw_len() == 0
                || events.raw_len() > Kind::ALL.len()
            {
                return Err(runtime(
                    "Observer events must list 1 through 12 known event names.",
                ));
            }
            let len = events.raw_len();
            let mut count = 0;
            for pair in events.clone().pairs::<Value, Value>() {
                let (key, _) = pair?;
                count += 1;
                if count > len || !matches!(key,Value::Integer(i) if i>=1 && i as u64<=len as u64) {
                    return Err(runtime("Observer events must be a dense array."));
                }
            }
            if count != len {
                return Err(runtime("Observer events cannot contain holes."));
            }
            let mut kinds = Vec::new();
            for value in events.sequence_values::<Value>() {
                let Value::String(name) = value? else {
                    return Err(runtime("Observer event names must be text."));
                };
                if name.as_bytes().len() > 32 {
                    return Err(runtime(
                        "Observer event names may contain at most 32 bytes.",
                    ));
                }
                let name = name.to_str()?;
                let kind = Kind::parse(&name)
                    .ok_or_else(|| runtime(format!("Unknown project event: {name}.")))?;
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
            }
            if kinds.is_empty() {
                return Err(runtime("An observer needs at least one event."));
            }
            permission(lua, &kinds)?;
            let debounce = opts.raw_get::<Option<u32>>("debounce_ms")?.unwrap_or(100);
            if !(MIN_DEBOUNCE_MS..=MAX_DEBOUNCE_MS).contains(&debounce) {
                return Err(runtime("Observer debounce_ms must be 20 through 5000."));
            }
            let run: Function = opts.raw_get("run")?;
            {
                let slot = lua
                    .app_data_ref::<PluginSlot>()
                    .ok_or_else(|| runtime("Only plugins may register observers."))?;
                if slot.decls.observers.len() >= MAX_SUBSCRIPTIONS {
                    return Err(stop(
                        Category::Budget,
                        "A plugin may register at most 32 project observers.",
                    ));
                }
                if slot.decls.observers.iter().any(|d| d.id == id) {
                    return Err(runtime("This observer ID is already registered."));
                }
            }
            let handlers: Table = lua.named_registry_value(HANDLERS)?;
            handlers.raw_set(id.as_str(), run)?;
            let mut slot = lua
                .app_data_mut::<PluginSlot>()
                .ok_or_else(|| runtime("Plugin registration disappeared."))?;
            slot.decls.observers.push(ObserverDecl {
                id,
                kinds,
                debounce_ms: debounce,
            });
            slot.changed = true;
            Ok(())
        })?,
    )?;
    plugin.set(
        "unobserve",
        lua.create_function(|lua, id: Value| {
            super::services::check_write(lua)?;
            check_read(lua)?;
            let id = observer_id(id)?;
            let handlers: Table = lua.named_registry_value(HANDLERS)?;
            handlers.raw_set(id.as_str(), Value::Nil)?;
            if let Some(mut s) = lua.app_data_mut::<PluginSlot>() {
                s.decls.observers.retain(|d| d.id != id);
                s.changed = true;
            }
            Ok(())
        })?,
    )?;
    plugin.set(
        "__observe",
        lua.create_function(|lua, ()| {
            check_read(lua)?;
            let delivery = lua
                .app_data_ref::<Scope>()
                .and_then(|s| s.delivery.clone())
                .ok_or_else(|| runtime("No native observation delivery is active."))?;
            permission(lua, &[delivery.event.kind])?;
            if let Data::Surface { index, .. } = delivery.event.data {
                let granted = lua
                    .app_data_ref::<super::handles::Granted>()
                    .map(|g| Arc::clone(&g.0))
                    .unwrap_or_default();
                Host::require(
                    &granted,
                    if index == 0 {
                        Capability::ReadProject
                    } else {
                        Capability::ReadPlayback
                    },
                )?;
            }
            let handlers: Table = lua.named_registry_value(HANDLERS)?;
            let callback = handlers.raw_get::<Option<Function>>(delivery.id.as_str())?;
            if let Some(callback) = callback {
                let event = delivery.event.table(lua, &delivery.id)?;
                event.set(
                    "dropped",
                    super::ticks::to_lua(delivery.dropped.min(i64::MAX as u64))?,
                )?;
                callback.call::<()>(event)?;
            }
            check_read(lua)
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "reactive_tests.rs"]
mod tests;
