//! Job-scoped declarative panels. Only bounded Rust data crosses to egui.
use super::bridge::{BridgeError, JobLease, NativeBudget, ProjectStamp, Reservation, Session};
use super::{Capability, Category, Host, notes, stop};
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{
    Arc, Condvar, Mutex, Weak,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
use std::time::Duration;
#[path = "ui_images.rs"]
pub(crate) mod images;
#[path = "ui_schema.rs"]
pub(crate) mod schema;

const MAX_PANELS: usize = 4;
const MAX_EVENTS: usize = 64;
const MAX_DELIVERED: u64 = 65536;
const SHARED_BYTES: usize = 512 * 1024;
const HANDLE_BYTES: usize = 2048;
const MAX_UPDATES: usize = 4096;
const MAX_JOB_TEXT: usize = 1024 * 1024;
//0 active,1 programmatic close; fatal reasons survive pcall and freeze at job end.
pub const CANCELED: u8 = 2;
pub const EXPIRED: u8 = 3;
pub const DENIED: u8 = 4;
pub const OVERFLOW: u8 = 5;
pub const TIMEOUT: u8 = 6;
pub const IMAGE_LIMIT: u8 = 7;
fn error(reason: u8) -> mlua::Error {
    match reason {
        CANCELED => stop(Category::Canceled, "The plugin panel job was canceled."),
        EXPIRED => stop(
            Category::Expired,
            "The plugin panel's project or lifecycle expired.",
        ),
        TIMEOUT => stop(
            Category::TimeLimit,
            "The plugin panel job reached its time limit.",
        ),
        IMAGE_LIMIT => stop(
            Category::Budget,
            "The UI image exceeds this renderer's texture limit; create it with a smaller max_side.",
        ),
        DENIED => stop(
            Category::Capability,
            "The plugin panel permission was revoked.",
        ),
        _ => stop(
            Category::Budget,
            "The plugin panel event or update budget was exhausted.",
        ),
    }
}
fn require(lua: &Lua) -> mlua::Result<()> {
    let granted = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, Capability::UiPanels)
}
#[derive(Clone, Debug, PartialEq)]
pub enum InputValue {
    None,
    Bool(bool),
    Number(f64),
    Text(String),
    Choice(usize),
    Color([u8; 4]),
    Expansion { item: String, expanded: bool },
}
#[derive(Clone, Debug)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub buttons: u8,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub command: bool,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub scroll_unit: &'static str,
    pub time: f64,
}
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: &'static str,
    pub id: String,
    pub value: InputValue,
    pub pointer: Option<Pointer>,
    pub revision: u64,
    pub sequence: u64,
}
impl Event {
    fn coalesces(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.id == other.id
            && self.revision == other.revision
            && matches!(self.kind, "change" | "pointer_move" | "resize" | "scroll")
            && self.pointer.as_ref().map(|p| (p.buttons, p.scroll_unit))
                == other.pointer.as_ref().map(|p| (p.buttons, p.scroll_unit))
    }
}
pub struct Shared {
    pub plugin: Arc<str>,
    pub job: u64,
    pub index: usize,
    lease: Weak<JobLease>,
    fault: Arc<AtomicU8>,
    closed: AtomicU8,
    revision: AtomicU64,
    delivered: AtomicU64,
    coalesced: AtomicU64,
    snapshot: Mutex<Option<Arc<schema::Snapshot>>>,
    queue: Mutex<VecDeque<Event>>,
    wake: Condvar,
    allocation: Mutex<Reservation>,
}
impl Shared {
    pub fn active(&self) -> bool {
        !self.closed() && self.lease.upgrade().is_some_and(|l| l.check().is_ok())
    }
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) != 0
    }
    pub fn invalid_reason(&self, current: Option<ProjectStamp>) -> Option<u8> {
        let Some(lease) = self.lease.upgrade() else {
            return Some(EXPIRED);
        };
        if self.closed() {
            return Some(EXPIRED);
        }
        if let Err(e) = lease.check() {
            return Some(match e {
                BridgeError::Canceled => CANCELED,
                BridgeError::Deadline => TIMEOUT,
                _ => EXPIRED,
            });
        }
        if lease.project != current {
            Some(EXPIRED)
        } else {
            None
        }
    }
    pub fn valid(&self, current: Option<ProjectStamp>) -> bool {
        self.lease.upgrade().is_some() && self.invalid_reason(current).is_none()
    }
    pub fn snapshot(&self) -> Option<Arc<schema::Snapshot>> {
        self.snapshot.try_lock().ok().and_then(|s| s.clone())
    }
    /// The queue lock covers only bounded native push/pop; no Lua/parse/wait holds it.
    pub fn push(&self, mut event: Event) -> bool {
        if self.closed() {
            return false;
        }
        let valid = event.id.len() <= 64
            && matches!(
                event.kind,
                "opened"
                    | "activate"
                    | "change"
                    | "expand"
                    | "resize"
                    | "pointer_down"
                    | "pointer_move"
                    | "pointer_up"
                    | "pointer_cancel"
                    | "scroll"
            )
            && match &event.value {
                InputValue::None | InputValue::Bool(_) => true,
                InputValue::Number(n) => n.is_finite() && n.abs() <= 1e12,
                InputValue::Text(s) => s.len() <= 4096,
                InputValue::Choice(n) => *n < 512,
                InputValue::Color(_) => true,
                InputValue::Expansion { item, .. } => {
                    !item.is_empty()
                        && item.len() <= 64
                        && item
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                }
            }
            && event.pointer.as_ref().is_none_or(|p| {
                [p.x, p.y, p.width, p.height, p.scroll_x, p.scroll_y, p.time]
                    .iter()
                    .all(|n| n.is_finite())
                    && p.x.abs() <= 1e6
                    && p.y.abs() <= 1e6
                    && (1.0..=2048.0).contains(&p.width)
                    && (1.0..=1024.0).contains(&p.height)
                    && p.buttons <= 7
                    && p.scroll_x.abs() <= 1e6
                    && p.scroll_y.abs() <= 1e6
            });
        if !valid {
            self.fail(OVERFLOW);
            return false;
        }
        let Ok(mut queue) = self.queue.lock() else {
            self.fail(OVERFLOW);
            return false;
        };
        if self.closed() {
            return false;
        }
        let sequence = self.delivered.fetch_add(1, Ordering::Relaxed) + 1;
        if sequence > MAX_DELIVERED {
            drop(queue);
            self.fail(OVERFLOW);
            return false;
        }
        event.sequence = sequence;
        if let Some(last) = queue.back_mut()
            && last.coalesces(&event)
        {
            if event.kind == "scroll"
                && let (Some(old), Some(new)) = (&last.pointer, &mut event.pointer)
            {
                new.scroll_x += old.scroll_x;
                new.scroll_y += old.scroll_y;
                if new.scroll_x.abs() > 1e6 || new.scroll_y.abs() > 1e6 {
                    drop(queue);
                    self.fail(OVERFLOW);
                    return false;
                }
            }
            *last = event;
            self.coalesced.fetch_add(1, Ordering::Relaxed);
        } else {
            if queue.len() == MAX_EVENTS {
                drop(queue);
                self.fail(OVERFLOW);
                return false;
            }
            queue.push_back(event);
        }
        drop(queue);
        self.wake.notify_all();
        true
    }
    pub fn fail(&self, reason: u8) {
        if self
            .closed
            .compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = self
                .fault
                .compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire);
            if let Some(lease) = self.lease.upgrade() {
                lease.cancel_from_surface();
            }
            self.wake.notify_all();
        }
    }
    fn release(&self) {
        let _ = self
            .closed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        if let Ok(mut s) = self.snapshot.lock() {
            s.take();
        }
        if let Ok(mut q) = self.queue.lock() {
            *q = VecDeque::new();
        }
        if let Ok(mut allocation) = self.allocation.lock() {
            allocation.shrink_to(HANDLE_BYTES);
        }
        self.wake.notify_all();
    }
}
#[derive(Clone)]
pub struct Client {
    plugin: Arc<str>,
    sender: SyncSender<Arc<Shared>>,
}
pub fn channel(plugin: &str) -> (Client, Receiver<Arc<Shared>>) {
    let (sender, receiver) = mpsc::sync_channel(MAX_PANELS);
    (
        Client {
            plugin: Arc::from(plugin),
            sender,
        },
        receiver,
    )
}
#[derive(Default)]
pub(super) struct Slot {
    pub client: Option<Client>,
    states: Vec<Weak<Shared>>,
    fault: Arc<AtomicU8>,
    updates: usize,
    text: usize,
    images: Vec<Weak<images::Resource>>,
    image_pixels: usize,
}
pub(super) fn begin_run(lua: &Lua) {
    let _ = end_run(lua);
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.fault = Arc::new(AtomicU8::new(0));
        s.updates = 0;
        s.text = 0;
    }
}
/// Freeze every surface before deciding whether this job may publish its commits.
pub(super) fn end_run(lua: &Lua) -> mlua::Result<()> {
    images::release(lua);
    let mut reason = 0;
    if let Some(mut slot) = lua.app_data_mut::<Slot>() {
        for shared in slot.states.drain(..).filter_map(|w| w.upgrade()) {
            shared.release();
            let closed = shared.closed.load(Ordering::Acquire);
            if reason < 2 && closed > 1 {
                reason = closed;
            }
        }
        let fault = slot.fault.load(Ordering::Acquire);
        if fault > 1 {
            reason = fault;
        }
    }
    if reason > 1 {
        Err(error(reason))
    } else {
        Ok(())
    }
}
fn scope(lua: &Lua, panel: &Shared) -> mlua::Result<Arc<JobLease>> {
    require(lua)?;
    let reason = panel.closed.load(Ordering::Acquire);
    if reason > 1 {
        return Err(error(reason));
    }
    notes::check_budget(lua)?;
    let lease = panel.lease.upgrade().ok_or_else(|| error(EXPIRED))?;
    let current = lua.app_data_ref::<Session>().and_then(|s| s.lease.clone());
    if !current.is_some_and(|s| Arc::ptr_eq(&s, &lease)) {
        return Err(error(EXPIRED));
    }
    lease.check().map_err(|e| e.lua())?;
    let reason = panel.closed.load(Ordering::Acquire);
    if reason > 1 {
        return Err(error(reason));
    }
    Ok(lease)
}
fn account(lua: &Lua, text: usize) -> mlua::Result<()> {
    let mut slot = lua
        .app_data_mut::<Slot>()
        .ok_or_else(|| stop(Category::Runtime, "UI scope unavailable."))?;
    if slot.updates >= MAX_UPDATES || slot.text.saturating_add(text) > MAX_JOB_TEXT {
        return Err(error(OVERFLOW));
    }
    slot.updates += 1;
    slot.text += text;
    Ok(())
}
struct Panel(Arc<Shared>);
fn open(lua: &Lua, spec: Table) -> mlua::Result<Panel> {
    require(lua)?;
    notes::check_budget(lua)?;
    let lease = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| error(EXPIRED))?;
    lease.check().map_err(|e| e.lua())?;
    if !lease.user_invoked() {
        return Err(stop(
            Category::Capability,
            "Custom panels require an explicitly invoked plugin command or tool, not loading/lifecycle/automatic jobs.",
        ));
    }
    let (client, index, fault) = {
        let slot = lua
            .app_data_ref::<Slot>()
            .ok_or_else(|| stop(Category::Runtime, "UI scope unavailable."))?;
        if slot.states.len() >= MAX_PANELS {
            return Err(error(OVERFLOW));
        }
        (
            slot.client
                .clone()
                .ok_or_else(|| stop(Category::Runtime, "Application UI unavailable."))?,
            slot.states.len(),
            Arc::clone(&slot.fault),
        )
    };
    if client.plugin != lease.plugin {
        return Err(error(EXPIRED));
    }
    let mut snapshot = schema::parse(lua, spec)?;
    snapshot.revision = 1;
    account(lua, snapshot.text_bytes)?;
    let allocation = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| stop(Category::Runtime, "UI accounting unavailable."))?
        .reserve(SHARED_BYTES)
        .map_err(|e| e.lua())?;
    let shared = Arc::new(Shared {
        plugin: Arc::clone(&lease.plugin),
        job: lease.job,
        index,
        lease: Arc::downgrade(&lease),
        fault,
        closed: AtomicU8::new(0),
        revision: AtomicU64::new(1),
        delivered: AtomicU64::new(0),
        coalesced: AtomicU64::new(0),
        snapshot: Mutex::new(Some(Arc::new(snapshot))),
        queue: Mutex::new(VecDeque::with_capacity(MAX_EVENTS)),
        wake: Condvar::new(),
        allocation: Mutex::new(allocation),
    });
    client.sender.try_send(Arc::clone(&shared)).map_err(|_| {
        stop(
            Category::Budget,
            "A panel request is already pending or the application UI closed.",
        )
    })?;
    lua.app_data_mut::<Slot>()
        .unwrap()
        .states
        .push(Arc::downgrade(&shared));
    Ok(Panel(shared))
}
fn event_table(lua: &Lua, e: Event) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("kind", e.kind)?;
    t.set("id", e.id)?;
    t.set("revision", e.revision)?;
    t.set("sequence", e.sequence)?;
    match e.value {
        InputValue::None => {}
        InputValue::Bool(v) => t.set("value", v)?,
        InputValue::Number(v) => t.set("value", v)?,
        InputValue::Text(v) => t.set("value", v)?,
        InputValue::Choice(v) => t.set("value", v + 1)?,
        InputValue::Color(v) => {
            let color = lua.create_sequence_from(v)?;
            t.set("value", color)?;
        }
        InputValue::Expansion { item, expanded } => {
            let value = lua.create_table()?;
            value.set("id", item)?;
            value.set("expanded", expanded)?;
            t.set("value", value)?;
        }
    }
    if let Some(p) = e.pointer {
        let pointer = lua.create_table()?;
        for (k, v) in [
            ("x", p.x),
            ("y", p.y),
            ("width", p.width),
            ("height", p.height),
            ("scroll_x", p.scroll_x),
            ("scroll_y", p.scroll_y),
            ("time", p.time),
        ] {
            pointer.set(k, v)?;
        }
        pointer.set("buttons", p.buttons)?;
        pointer.set("scroll_unit", p.scroll_unit)?;
        for (k, v) in [
            ("shift", p.shift),
            ("ctrl", p.ctrl),
            ("alt", p.alt),
            ("command", p.command),
        ] {
            pointer.set(k, v)?;
        }
        t.set("pointer", pointer)?;
    }
    Ok(t)
}
impl UserData for Panel {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("update", |lua, this, spec: Table| {
            scope(lua, &this.0)?;
            if this.0.closed() {
                return Err(error(EXPIRED));
            }
            let mut snapshot = schema::parse(lua, spec)?;
            account(lua, snapshot.text_bytes)?;
            scope(lua, &this.0)?;
            let revision = this
                .0
                .revision()
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| error(OVERFLOW))?;
            snapshot.revision = revision;
            let old = this
                .0
                .snapshot
                .lock()
                .map_err(|_| error(EXPIRED))?
                .replace(Arc::new(snapshot));
            this.0.revision.store(revision, Ordering::Release);
            drop(old);
            Ok(revision)
        });
        m.add_method("next_event", |lua, this, timeout: Value| {
            let timeout = match timeout {
                Value::Nil => None,
                Value::Integer(n) if (0..=60000).contains(&n) => {
                    Some(Duration::from_millis(n as u64))
                }
                _ => {
                    return Err(stop(
                        Category::Runtime,
                        "Panel timeout must be an integer0–60000milliseconds.",
                    ));
                }
            };
            let start = std::time::Instant::now();
            loop {
                scope(lua, &this.0)?;
                let mut queue = this.0.queue.lock().map_err(|_| error(EXPIRED))?;
                if let Some(event) = queue.pop_front() {
                    drop(queue);
                    return event_table(lua, event).map(Some);
                }
                if this.0.closed() || timeout.is_some_and(|t| start.elapsed() >= t) {
                    return Ok(None);
                }
                let wait = timeout.map_or(Duration::from_millis(10), |t| {
                    t.saturating_sub(start.elapsed())
                        .min(Duration::from_millis(10))
                });
                let _guard = this
                    .0
                    .wake
                    .wait_timeout(queue, wait)
                    .map_err(|_| error(EXPIRED))?;
            }
        });
        m.add_method("info", |lua, this, ()| {
            scope(lua, &this.0)?;
            let t = lua.create_table()?;
            t.set("revision", this.0.revision())?;
            t.set("closed", this.0.closed())?;
            let queued = { this.0.queue.lock().map_err(|_| error(EXPIRED))?.len() };
            t.set("queued", queued)?;
            t.set("coalesced", this.0.coalesced.load(Ordering::Acquire))?;
            Ok(t)
        });
        m.add_method("close", |_, this, ()| {
            let was = !this.0.closed();
            this.0.release();
            Ok(was)
        });
        m.add_method("cancel", |_, this, ()| {
            this.0.fail(CANCELED);
            Ok(())
        });
    }
}
pub(super) fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    lua.set_app_data(Slot::default());
    let t = lua.create_table()?;
    t.set("open_panel", lua.create_function(open)?)?;
    images::install(lua, &t)?;
    super::visual::install(lua, &t)?;
    super::overlays::install(lua, &t)?;
    for (name, n) in [
        ("max_panels", MAX_PANELS),
        ("max_panel_events", MAX_EVENTS),
        ("max_ui_nodes", schema::MAX_NODES),
        ("max_canvas_primitives", schema::MAX_PRIMITIVES),
        ("max_canvas_points", schema::MAX_POINTS),
        ("max_ui_options", schema::MAX_OPTIONS),
        ("max_list_options", 512),
        ("max_tabs", 16),
        ("max_tree_items", schema::MAX_TREE_ITEMS),
    ] {
        t.set(name, n)?;
    }
    plum.set("ui", t)
}
#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::native_panel;
#[cfg(test)]
impl Shared {
    pub(crate) fn test_events(&self) -> Vec<Event> {
        self.queue.lock().unwrap().drain(..).collect()
    }
}

#[cfg(test)]
#[path = "ui_graph_tests.rs"]
mod graph_tests;

#[cfg(test)]
#[path = "ui_tree_tests.rs"]
mod tree_tests;
