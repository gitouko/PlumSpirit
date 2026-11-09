//! Worker-authored immutable editor display lists. Native owners never execute Lua.
use super::{
    Capability, Category, Host,
    bridge::{NativeBudget, Reservation, Session},
    notes, stop,
    ui::{images, schema},
    visual,
};
use mlua::{Lua, Table, Value};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

pub const MAX_FRAMES: usize = 4;
pub const MAX_VISIBLE: usize = 8;
const FRAME_BYTES: usize = schema::SNAPSHOT_BYTES;
const MIN_REFRESH: Duration = Duration::from_millis(50);
struct Owner {
    active: AtomicBool,
    epoch: Arc<AtomicU64>,
}
pub struct Frame {
    pub id: String,
    pub view: visual::Snapshot,
    pub primitives: Option<schema::PrimitiveList>,
    owner: Arc<Owner>,
    epoch: u64,
    _allocation: Reservation,
}
impl Frame {
    pub fn active(&self) -> bool {
        self.epoch != u64::MAX
            && self.owner.active.load(Ordering::Acquire)
            && self.owner.epoch.load(Ordering::Acquire) == self.epoch
    }
}
pub struct Batch {
    pub frames: Vec<Arc<Frame>>,
}
pub(super) struct Mailbox {
    receiver: Receiver<Batch>,
    pending: Arc<AtomicBool>,
}
impl Mailbox {
    pub(super) fn pending_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.pending)
    }
    pub(super) fn try_recv(&self) -> Result<Batch, mpsc::TryRecvError> {
        let batch = self.receiver.try_recv()?;
        self.pending.store(false, Ordering::Release);
        Ok(batch)
    }
}
impl Drop for Mailbox {
    fn drop(&mut self) {
        self.pending.store(false, Ordering::Release);
    }
}
pub(super) struct Slot {
    sender: Option<SyncSender<Batch>>,
    native_pending: Arc<AtomicBool>,
    owner: Arc<Owner>,
    pending: Vec<Arc<Frame>>,
    ids: BTreeSet<(usize, String)>,
    next_ids: BTreeSet<(usize, String)>,
    attempts: usize,
    last: Option<Instant>,
    project: Option<(u64, u64)>,
}
impl Slot {
    fn new(sender: Option<SyncSender<Batch>>, epoch: Arc<AtomicU64>) -> Self {
        Self {
            sender,
            native_pending: Arc::new(AtomicBool::new(false)),
            owner: Arc::new(Owner {
                active: AtomicBool::new(true),
                epoch,
            }),
            pending: Vec::new(),
            ids: BTreeSet::new(),
            next_ids: BTreeSet::new(),
            attempts: 0,
            last: None,
            project: None,
        }
    }
}
impl Default for Slot {
    fn default() -> Self {
        Self::new(None, Arc::new(AtomicU64::new(0)))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.owner.active.store(false, Ordering::Release);
    }
}
pub(super) fn channel(epoch: Arc<AtomicU64>) -> (Slot, Mailbox) {
    let (sender, receiver) = mpsc::sync_channel(1);
    let slot = Slot::new(Some(sender), epoch);
    let pending = Arc::clone(&slot.native_pending);
    (slot, Mailbox { receiver, pending })
}

pub(super) fn begin_run(lua: &Lua) {
    let project = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.as_ref().and_then(|l| l.project.map(|p| p.serial)))
        .zip(images::retained_run_epoch(lua));
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.pending.clear();
        s.attempts = 0;
        if project.is_some() && s.project != project {
            s.ids.clear();
            s.project = project;
            s.last = None;
        }
        s.next_ids = s.ids.clone();
    }
}
pub(super) fn end_run(lua: &Lua, ok: bool) -> mlua::Result<()> {
    let mut s = lua
        .app_data_mut::<Slot>()
        .ok_or_else(|| bad("Overlay scope unavailable."))?;
    let frames = std::mem::take(&mut s.pending);
    if !ok || frames.is_empty() {
        return Ok(());
    }
    notes::check_budget(lua)?;
    if frames.iter().any(|f| !f.active()) {
        return Err(stop(
            Category::Expired,
            "The overlay project or plugin lifecycle expired.",
        ));
    }
    let sender = s
        .sender
        .as_ref()
        .ok_or_else(|| bad("No native editor overlay consumer is attached."))?;
    let full = || {
        stop(
            Category::Budget,
            "The native overlay mailbox is full or closed; no frame was published.",
        )
    };
    s.native_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| full())?;
    if sender.try_send(Batch { frames }).is_err() {
        s.native_pending.store(false, Ordering::Release);
        return Err(full());
    }
    s.ids = s.next_ids.clone();
    s.last = Some(Instant::now());
    Ok(())
}
fn bad(message: &str) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn id(value: String) -> mlua::Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(bad(
            "Overlay id must contain1–64ASCII letters, digits, underscores, hyphens or dots.",
        ));
    }
    Ok(value)
}
fn id_value(value: Value) -> mlua::Result<String> {
    let Value::String(value) = value else {
        return Err(bad("Overlay id must be a string."));
    };
    id(value.to_str()?.to_string())
}
fn admit(lua: &Lua) -> mlua::Result<()> {
    super::reactive::check_read(lua)?;
    notes::check_budget(lua)?;
    let mut s = lua
        .app_data_mut::<Slot>()
        .ok_or_else(|| bad("Overlay scope unavailable."))?;
    if s.attempts >= MAX_FRAMES {
        return Err(stop(
            Category::Budget,
            "At most4overlay publications/clears are allowed per job.",
        ));
    }
    s.attempts += 1;
    if s.last.is_some_and(|t| t.elapsed() < MIN_REFRESH) {
        return Err(stop(
            Category::Budget,
            "Overlay publication is limited to20successful batches per second per plugin.",
        ));
    }
    Ok(())
}
fn stage(
    lua: &Lua,
    view: visual::Snapshot,
    id: String,
    primitives: Option<schema::PrimitiveList>,
) -> mlua::Result<()> {
    super::reactive::check_read(lua)?;
    notes::check_budget(lua)?;
    let grants = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&grants, Capability::UiOverlay)?;
    Host::require(
        &grants,
        if view.facts.index() == 0 {
            Capability::ReadProject
        } else {
            Capability::ReadPlayback
        },
    )?;
    let lease = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| bad("No overlay job is active."))?;
    lease.check().map_err(|e| e.lua())?;
    if lease.project != Some(view.stamp) {
        return Err(stop(
            Category::Expired,
            "Overlay view belongs to a different project revision.",
        ));
    }
    let epoch = images::retained_run_epoch(lua)
        .ok_or_else(|| stop(Category::Expired, "Overlay lifecycle generation expired."))?;
    let mut s = lua
        .app_data_mut::<Slot>()
        .ok_or_else(|| bad("Overlay scope unavailable."))?;
    let key = (view.facts.index(), id.clone());
    if primitives.is_some() && !s.next_ids.contains(&key) && s.next_ids.len() >= MAX_FRAMES {
        return Err(stop(
            Category::Budget,
            "A plugin may own at most4overlay ids across both surfaces.",
        ));
    }
    let allocation = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Overlay accounting unavailable."))?
        .reserve(2048)
        .map_err(|e| e.lua())?;
    if primitives.is_some() {
        s.next_ids.insert(key);
    } else {
        s.next_ids.remove(&key);
    }
    let frame = Arc::new(Frame {
        id,
        view,
        primitives,
        owner: Arc::clone(&s.owner),
        epoch,
        _allocation: allocation,
    });
    if let Some(i) = s
        .pending
        .iter()
        .position(|f| f.id == frame.id && f.view.facts.index() == frame.view.facts.index())
    {
        s.pending[i] = frame;
    } else {
        s.pending.push(frame);
    }
    Ok(())
}
pub(super) fn install(lua: &Lua, ui: &Table) -> mlua::Result<()> {
    ui.set(
        "publish_overlay",
        lua.create_function(|lua, (view, spec): (Value, Value)| {
            admit(lua)?;
            let Value::UserData(view) = view else {
                return Err(bad("Publish using an opaque native view snapshot."));
            };
            let Value::Table(spec) = spec else {
                return Err(bad("Overlay spec must be a plain table."));
            };
            let view = visual::reference(lua, &view)?;
            schema::fields(&spec, &["id", "primitives"])?;
            let id = id_value(spec.raw_get("id")?)?;
            let primitives = schema::parse_primitives(lua, spec.raw_get("primitives")?)?;
            for p in &primitives.items {
                if let schema::Primitive::Image { resource, .. } = p
                    && !resource.retained()
                {
                    return Err(bad(
                        "Editor overlay images must be promoted with plum.ui.retain_image.",
                    ));
                }
            }
            stage(lua, view, id, Some(primitives))
        })?,
    )?;
    ui.set(
        "clear_overlay",
        lua.create_function(|lua, (view, name): (Value, Value)| {
            admit(lua)?;
            let Value::UserData(view) = view else {
                return Err(bad("Clear using an opaque native view snapshot."));
            };
            stage(lua, visual::reference(lua, &view)?, id_value(name)?, None)
        })?,
    )?;
    ui.set("max_overlay_frames", MAX_FRAMES)?;
    ui.set("max_visible_overlays", MAX_VISIBLE)?;
    Ok(())
}
#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
