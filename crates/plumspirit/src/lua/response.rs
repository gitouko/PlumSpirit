//! Separately authorized transaction responses to read-only observation.
//! Native snapshots are detached before author code runs. Exact native counters
//! validate the source again before its one transaction is bound to the live root.
use super::bridge::{NativeBudget, ProjectStamp, Reservation};
use super::reactive::Delivery;
use super::structured::Payload;
use super::{Category, ScriptError, World};
use plumspirit_engine::model::{ContentId, PlumDocument, TrackId};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MAX_OBJECTS: usize = 4096;
pub const MAX_NOTES: u64 = 100_000;
pub const MAX_OPERATIONS: usize = 8;
pub const MIN_INTERVAL: Duration = Duration::from_millis(500);
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub command: String,
    pub data: Payload,
}
#[derive(Default)]
pub(super) struct RequestSlot(pub Option<Request>);

#[derive(Debug, PartialEq, Eq)]
struct TrackState {
    id: TrackId,
    notes: u64,
    events: u64,
    props: u64,
    subs: u64,
    mute: bool,
    solo: bool,
}
#[derive(Debug, PartialEq, Eq)]
struct ContentState {
    id: ContentId,
    notes: u64,
    events: u64,
}
pub struct SnapshotGuard {
    pub stamp: ProjectStamp,
    tracks: Vec<TrackState>,
    contents: Vec<ContentState>,
    structure: u64,
    layout: u64,
    clips: u64,
    conductor: u64,
    keys: u16,
    pub info: Arc<super::handles::ProjectInfo>,
    _native: Reservation,
}
fn failure(category: Category, message: impl Into<String>) -> ScriptError {
    ScriptError::new(category, message)
}
impl SnapshotGuard {
    pub fn matches(&self, doc: &PlumDocument, stamp: ProjectStamp) -> bool {
        if self.stamp != stamp
            || self.structure != doc.structure_revision()
            || self.layout != doc.layout_revision()
            || self.clips != doc.clips().revision()
            || self.conductor != doc.conductor().revision()
            || self.keys != doc.project().keys.keys() as u16
            || self.tracks.len() != doc.track_count()
            || self.contents.len() != doc.clips().contents().len()
        {
            return false;
        }
        self.tracks
            .iter()
            .zip(doc.tracks())
            .all(|(a, t)| *a == track_state(t))
            && self
                .contents
                .iter()
                .zip(doc.clips().contents())
                .all(|(a, c)| *a == content_state(c))
    }
}
fn track_state(t: &plumspirit_engine::model::Track) -> TrackState {
    TrackState {
        id: t.id(),
        notes: t.notes.revision(),
        events: t.events_revision(),
        props: t.props_revision(),
        subs: t.subchannels_revision(),
        mute: t.mute,
        solo: t.solo,
    }
}
fn content_state(c: &plumspirit_engine::model::OwnedContent) -> ContentState {
    ContentState {
        id: c.id,
        notes: c.notes.revision(),
        events: c.events_revision(),
    }
}
/// Cheap admission before the App captures history/UI metadata. Native IDs
/// (including Subchannels/placements/groups) are bounded independently of notes.
pub fn metadata_eligible(doc: &PlumDocument) -> bool {
    let mut objects = doc
        .track_count()
        .saturating_add(doc.clips().contents().len())
        .saturating_add(doc.clips().explicit_placements().len())
        .saturating_add(doc.project().groups.len())
        .saturating_add(doc.project().folders.folders().len());
    if objects > MAX_OBJECTS {
        return false;
    }
    for t in doc.tracks() {
        objects = objects.saturating_add(t.subchannels().len());
        if objects > MAX_OBJECTS {
            return false;
        }
    }
    true
}
/// Cost follows the native derived clone: payload Arcs are shared, selection and
/// ordinary metadata are copied. Deliberately conservative for B-tree/allocator cost.
fn snapshot_bytes<E>(
    doc: &PlumDocument,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<usize, E> {
    use std::mem::size_of_val;
    let mut bytes = std::mem::size_of::<PlumDocument>();
    macro_rules! add {
        ($n:expr) => {
            bytes = bytes.saturating_add($n)
        };
    }
    add!(size_of_val(doc.tracks()));
    for t in doc.tracks() {
        check()?;
        add!(t.notes.snapshot_workspace_bytes(&mut check)?);
        add!(t.subchannels().heap_bytes());
        add!(t.name().map_or(0, str::len));
        if let plumspirit_engine::model::track::OutputRoute::External(s) = &t.props().route {
            add!(s.len());
        }
    }
    add!(doc.conductor().heap_bytes());
    add!(size_of_val(doc.extra_chunks()));
    for c in doc.extra_chunks() {
        check()?;
        add!(c.data.len());
    }
    let clips = doc.clips();
    add!(size_of_val(clips.contents()));
    for c in clips.contents() {
        check()?;
        add!(c.notes.snapshot_workspace_bytes(&mut check)?);
        add!(c.name.as_ref().map_or(0, String::len));
    }
    add!(size_of_val(clips.explicit_placements()));
    for c in clips.explicit_placements() {
        check()?;
        add!(c.name.as_ref().map_or(0, String::len));
    }
    add!(size_of_val(clips.removed_primaries()));
    let p = doc.project();
    add!(p.name.len());
    add!(p.soundfont.as_ref().map_or(0, |s| s.path.len()));
    add!(p.reference.as_ref().map_or(0, |s| s.path.len()));
    add!(p.source.as_ref().map_or(0, |s| s.path.len()));
    add!(size_of_val(p.onion.specific.as_slice()));
    add!(size_of_val(p.onion.track_entries.as_slice()));
    for e in &p.onion.track_entries {
        check()?;
        add!(size_of_val(e.specific.as_slice()));
    }
    add!(p.groups.heap_bytes());
    add!(p.metadata.heap_bytes());
    add!(p.folders.heap_bytes().saturating_mul(2));
    add!(p.folders_collapsed.len().saturating_mul(64));
    add!(size_of_val(p.zoom_regions.as_slice()));
    for z in &p.zoom_regions {
        check()?;
        add!(z.name.len());
    }
    add!(size_of_val(p.plugin_data.as_slice()));
    for (id, data) in &p.plugin_data {
        check()?;
        add!(id.len().saturating_add(data.len()));
    }
    add!(size_of_val(p.unknown_sections.as_slice()));
    for section in &p.unknown_sections {
        check()?;
        add!(section.data.len());
    }
    add!(p.monitor.as_ref().map_or(0, |m| size_of_val(m.as_slice())));
    Ok(bytes.saturating_add(16 * 1024))
}

/// The live Arc is dropped before returning: Lua calculation never pins it.
pub fn detach(
    mut world: World,
    delivery: &Delivery,
    budget: &NativeBudget,
    mut check: impl FnMut() -> Result<(), ScriptError>,
) -> Result<(World, SnapshotGuard), ScriptError> {
    check()?;
    let doc = &world.doc;
    let objects = doc
        .track_count()
        .saturating_add(doc.clips().contents().len());
    if !metadata_eligible(doc) {
        return Err(failure(
            Category::Budget,
            "Automatic responses support at most 4,096 native identity objects (Tracks, Subchannels, Clips, owned contents, groups and folders); use an explicit command for larger layouts.",
        ));
    }
    let bytes = snapshot_bytes(doc, &mut check)?;
    let guard_bytes = objects.saturating_mul(96);
    let mut payload = 0usize;
    for t in doc.tracks() {
        check()?;
        payload = payload
            .saturating_add(t.notes.heap_bytes())
            .saturating_add(t.events.heap_bytes());
    }
    for c in doc.clips().contents() {
        check()?;
        payload = payload
            .saturating_add(c.notes.heap_bytes())
            .saturating_add(c.events.heap_bytes());
    }
    // Source clone, private transaction clone/result and conservative allocator overlap.
    let native = budget
        .reserve(
            bytes
                .saturating_mul(3)
                .saturating_add(guard_bytes)
                .saturating_add(payload.saturating_mul(MAX_OPERATIONS + 2))
                .saturating_add(64 * 1024 * 1024),
        )
        .map_err(|_| {
            failure(
                Category::Budget,
                "The automatic response snapshot exceeds its native memory quota.",
            )
        })?;
    let guard = SnapshotGuard {
        stamp: delivery.event.stamp,
        tracks: doc.tracks().iter().map(track_state).collect(),
        contents: doc.clips().contents().iter().map(content_state).collect(),
        structure: doc.structure_revision(),
        layout: doc.layout_revision(),
        clips: doc.clips().revision(),
        conductor: doc.conductor().revision(),
        keys: doc.project().keys.keys() as u16,
        info: Arc::clone(&world.info),
        _native: native,
    };
    check()?;
    world.doc = Arc::new((**doc).clone());
    check()?;
    Ok((world, guard))
}

#[derive(Default)]
pub struct Gate {
    blocked: Option<ProjectStamp>,
    last_attempt: Option<Instant>,
}
impl Gate {
    pub fn clear(&mut self) {
        self.blocked = None;
        self.last_attempt = None;
    }
    pub fn block(&mut self, stamp: Option<ProjectStamp>) {
        self.blocked = stamp;
    }
    pub fn external_selection(&mut self, stamp: ProjectStamp) {
        if self.blocked == Some(stamp) {
            self.blocked = None;
        }
    }
    pub fn blocked(&self, stamp: ProjectStamp) -> bool {
        self.blocked == Some(stamp)
    }
    pub fn request(&mut self, stamp: ProjectStamp, now: Instant) -> bool {
        if self.blocked(stamp)
            || self
                .last_attempt
                .is_some_and(|last| now.saturating_duration_since(last) < MIN_INTERVAL)
        {
            return false;
        }
        self.last_attempt = Some(now);
        true
    }
}

pub(super) fn permission(lua: &mlua::Lua) -> mlua::Result<()> {
    let caps = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|c| Arc::clone(&c.0))
        .unwrap_or_default();
    for cap in [
        super::Capability::ReadProject,
        super::Capability::ObserveProject,
        super::Capability::AutomateProject,
        super::Capability::EditNotes,
    ] {
        super::Host::require(&caps, cap)?;
    }
    Ok(())
}
pub(super) fn begin_run(lua: &mlua::Lua) {
    lua.set_app_data(RequestSlot::default());
}
pub(super) fn end_run(lua: &mlua::Lua, ok: bool) -> Option<Request> {
    let request = lua
        .app_data_mut::<RequestSlot>()
        .and_then(|mut r| r.0.take());
    if ok { request } else { None }
}
pub(super) fn install(lua: &mlua::Lua, plugin: &mlua::Table) -> mlua::Result<()> {
    use mlua::{Table, Value};
    plugin.set(
        "request_transaction",
        lua.create_function(|lua, options: Table| {
            permission(lua)?;
            super::reactive::check_read(lua)?;
            if options.metatable().is_some() {
                return Err(super::stop(
                    Category::Runtime,
                    "Transaction requests need a plain options table.",
                ));
            }
            let scope = lua
                .app_data_ref::<super::reactive::Scope>()
                .ok_or_else(|| {
                    super::stop(Category::Runtime, "No project observation is active.")
                })?;
            let delivery = scope.delivery.as_ref().ok_or_else(|| {
                super::stop(
                    Category::Runtime,
                    "Only observation handlers can request a transaction response.",
                )
            })?;
            if scope.request.is_some() {
                return Err(super::stop(
                    Category::Capability,
                    "A transaction response cannot request another response.",
                ));
            }
            if !delivery.event.transaction_allowed {
                return Ok(false);
            }
            drop(scope);
            let id: mlua::LuaString = options.raw_get("command")?;
            if id.as_bytes().len() > 64 {
                return Err(super::stop(
                    Category::Runtime,
                    "A response command ID may contain at most 64 bytes.",
                ));
            }
            let id = id.to_str()?.to_owned();
            super::plugin_api::check_decl_id(&id, "response command")?;
            if !lua
                .app_data_ref::<super::plugin_api::PluginSlot>()
                .is_some_and(|s| s.decls.commands.iter().any(|c| c.id == id))
            {
                return Err(super::stop(
                    Category::Runtime,
                    "The requested response command is not registered by this plugin.",
                ));
            }
            if lua
                .app_data_ref::<RequestSlot>()
                .is_some_and(|s| s.0.is_some())
            {
                return Err(super::stop(
                    Category::Budget,
                    "One event delivery may request at most one transaction response.",
                ));
            }
            let value: Value = options.raw_get("data")?;
            let data = Payload::read(lua, value)?;
            super::reactive::check_read(lua)?;
            lua.app_data_mut::<RequestSlot>()
                .ok_or_else(|| {
                    super::stop(Category::Internal, "Response request scope disappeared.")
                })?
                .0 = Some(Request { command: id, data });
            Ok(true)
        })?,
    )?;
    plugin.set(
        "__respond",
        lua.create_function(|lua, ()| {
            permission(lua)?;
            super::reactive::check_read(lua)?;
            let scope = lua
                .app_data_ref::<super::reactive::Scope>()
                .ok_or_else(|| {
                    super::stop(Category::Capability, "No native response scope is active.")
                })?;
            let request = scope.request.clone().ok_or_else(|| {
                super::stop(Category::Capability, "No native response scope is active.")
            })?;
            let delivery = scope.delivery.clone().ok_or_else(|| {
                super::stop(Category::Capability, "No native event context is active.")
            })?;
            drop(scope);
            let ctx = lua.create_table()?;
            ctx.set("event", delivery.event.table(lua, &delivery.id)?)?;
            ctx.set("request", request.data.to_lua(lua)?)?;
            ctx.set("reactive", true)?;
            super::plugin_api::invoke(lua, &request.command, None, Some(ctx))
        })?,
    )?;
    Ok(())
}

pub(super) fn check_removed(lua: &mlua::Lua, already: u64, more: u64) -> mlua::Result<()> {
    if super::reactive::responding(lua) && already.saturating_add(more) > MAX_NOTES {
        return Err(super::stop(
            Category::Budget,
            "An automatic response may remove at most 100,000 notes.",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;

pub(super) fn explicit_only(lua: &mlua::Lua, what: &str) -> mlua::Result<()> {
    if super::reactive::responding(lua) {
        return Err(super::stop(
            Category::Capability,
            format!(
                "{what} require an explicit command; automatic responses currently support bounded note edits."
            ),
        ));
    }
    Ok(())
}
