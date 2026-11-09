//! Explicitly approved, job-scoped MIDI input. Lua runs only on its existing worker.
use super::bridge::{BridgeError, JobLease, NativeBudget, ProjectStamp, Session};
use super::{Capability, Category, Host, stop};
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;
#[path = "midi_input_native.rs"]
pub(crate) mod native;

#[path = "midi_input_windows.rs"]
pub(crate) mod windows;

#[path = "midi_input_runtime.rs"]
pub(crate) mod runtime;

const MAX_ENUMERATIONS: usize = 4;
const MAX_OPENS: usize = 4;
const MAX_DELIVERED: u64 = 1_048_576;

pub struct ConsentRequest {
    pub lease: Arc<JobLease>,
    pub devices: Arc<runtime::Enumerated>,
    reply: SyncSender<Result<Option<windows::Approved>, BridgeError>>,
}
impl ConsentRequest {
    #[cfg(test)]
    pub(crate) fn fixture(
        lease: Arc<JobLease>,
        devices: Vec<windows::Device>,
        budget: &NativeBudget,
    ) -> (
        Self,
        Receiver<Result<Option<windows::Approved>, BridgeError>>,
    ) {
        let (reply, rx) = mpsc::sync_channel(1);
        (
            Self {
                lease,
                devices: Arc::new(runtime::Enumerated::fixture(devices, budget).unwrap()),
                reply,
            },
            rx,
        )
    }
    pub fn reject(self, e: BridgeError) {
        let _ = self.reply.try_send(Err(e));
    }
    /// Returns semantic acceptance; unlike file-picker ACK, an expired choice is Err.
    pub fn respond(
        self,
        current: Option<ProjectStamp>,
        allowed: bool,
        index: Option<usize>,
    ) -> Result<bool, BridgeError> {
        let result = self.lease.check().and_then(|()| {
            if self.lease.project != current {
                return Err(BridgeError::Expired);
            }
            if !allowed {
                return Err(BridgeError::NotUserInvoked);
            }
            index
                .map(|i| {
                    self.devices
                        .inventory
                        .devices
                        .get(i)
                        .cloned()
                        .map(windows::Approved::after_user_consent)
                        .ok_or(BridgeError::Unavailable)
                })
                .transpose()
        });
        match result {
            Ok(device) => {
                let accepted = device.is_some();
                self.reply
                    .try_send(Ok(device))
                    .map_err(|_| BridgeError::Expired)?;
                Ok(accepted)
            }
            Err(e) => {
                let _ = self.reply.try_send(Err(e.clone()));
                Err(e)
            }
        }
    }
}
#[derive(Clone)]
pub struct Client {
    plugin: Arc<str>,
    sender: SyncSender<ConsentRequest>,
}
pub fn channel(plugin: &str) -> (Client, Receiver<ConsentRequest>) {
    let (sender, rx) = mpsc::sync_channel(1);
    (
        Client {
            plugin: Arc::from(plugin),
            sender,
        },
        rx,
    )
}
impl Client {
    fn request(
        &self,
        lease: Arc<JobLease>,
        devices: Arc<runtime::Enumerated>,
    ) -> Result<Option<windows::Approved>, BridgeError> {
        lease.check()?;
        if self.plugin != lease.plugin {
            return Err(BridgeError::Expired);
        }
        lease.reserve_prompt()?;
        let (reply, rx) = mpsc::sync_channel(1);
        self.sender
            .try_send(ConsentRequest {
                lease: Arc::clone(&lease),
                devices,
                reply,
            })
            .map_err(|e| match e {
                TrySendError::Full(_) => BridgeError::Busy,
                TrySendError::Disconnected(_) => BridgeError::Unavailable,
            })?;
        loop {
            lease.check()?;
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => {
                    lease.check()?;
                    return result;
                }
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => return Err(BridgeError::Unavailable),
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Slot {
    pub client: Option<Client>,
    job: Option<Rc<Job>>,
    #[cfg(test)]
    simulation: Option<Arc<Simulation>>,
}
struct Job {
    lease: Arc<JobLease>,
    native: NativeBudget,
    enumerations: Cell<usize>,
    opens: Cell<usize>,
    delivered: Cell<u64>,
    states: RefCell<Vec<Weak<RefCell<Option<Reader>>>>>,
    #[cfg(test)]
    simulation: Option<Arc<Simulation>>,
}
struct Reader {
    input: runtime::Input,
    name: String,
}
pub(super) fn begin_run(lua: &Lua) {
    end_run(lua);
    // API0.1/ungranted jobs allocate no MIDI resource scope or native I/O task.
    if !lua
        .app_data_ref::<super::handles::Granted>()
        .is_some_and(|g| g.0.contains(&Capability::ReadMidiInput))
    {
        return;
    }
    let lease = lua.app_data_ref::<Session>().and_then(|s| s.lease.clone());
    let native = lua
        .app_data_ref::<NativeBudget>()
        .map(|b| b.clone())
        .unwrap_or_default();
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.job = lease.map(|lease| {
            Rc::new(Job {
                lease,
                native,
                enumerations: Cell::new(0),
                opens: Cell::new(0),
                delivered: Cell::new(0),
                states: RefCell::new(Vec::new()),
                #[cfg(test)]
                simulation: s.simulation.clone(),
            })
        });
    }
}
pub(super) fn end_run(lua: &Lua) {
    if let Some(job) = lua.app_data_mut::<Slot>().and_then(|mut s| s.job.take()) {
        for state in job.states.borrow().iter().filter_map(Weak::upgrade) {
            state.borrow_mut().take();
        }
    }
}
fn invalid(message: &str) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn runtime_error(e: runtime::Error) -> mlua::Error {
    use runtime::Error as E;
    match e {
        E::Bridge(e) => e.lua(),
        E::Busy => stop(
            Category::Budget,
            "All eight native MIDI I/O task slots are in use; finish another session first.",
        ),
        E::Native(windows::Error::Budget) | E::Open(windows::Error::Budget, None) => stop(
            Category::Budget,
            "The MIDI input native memory budget is exhausted.",
        ),
        E::Native(windows::Error::Slots) | E::Open(windows::Error::Slots, None) => stop(
            Category::Budget,
            "All four native MIDI input slots are in use or retained after driver cleanup failures.",
        ),
        E::Open(reason, cleanup) => {
            let suffix = if cleanup.is_some_and(|r| r.close != 0) {
                " Driver cleanup failed; its revoked callback memory and slot remain reserved until restart."
            } else {
                ""
            };
            invalid(&format!("MIDI input startup failed ({reason:?}).{suffix}"))
        }
        E::Native(e) => invalid(&format!("MIDI input native operation failed ({e:?}).")),
        E::Queue(e) => invalid(&format!("MIDI input queue failed ({e:?}).")),
        E::Spawn | E::Unavailable => invalid("The native MIDI input worker is unavailable."),
        E::Wait => invalid("MIDI read requires 1–256 events and 0–1000 milliseconds wait."),
    }
}
fn require(lua: &Lua) -> mlua::Result<()> {
    let grants = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&grants, Capability::ReadMidiInput)
}
fn job(lua: &Lua) -> mlua::Result<Rc<Job>> {
    require(lua)?;
    let job = lua
        .app_data_ref::<Slot>()
        .and_then(|s| s.job.clone())
        .ok_or_else(|| stop(Category::Expired, "This MIDI input scope has finished."))?;
    job.lease.check().map_err(|e| e.lua())?;
    Ok(job)
}
fn user_job(lua: &Lua) -> mlua::Result<Rc<Job>> {
    let job = job(lua)?;
    if !job.lease.user_invoked() {
        return Err(stop(
            Category::Capability,
            "MIDI input discovery and device access require an explicitly invoked plugin command.",
        ));
    }
    if !cfg!(windows) {
        #[cfg(test)]
        if job.simulation.is_some() {
            return Ok(job);
        }
        return Err(invalid(
            "Live MIDI input is currently available on Windows only.",
        ));
    }
    Ok(job)
}
fn count(value: Value, default: usize, min: usize, max: usize) -> mlua::Result<usize> {
    match value {
        Value::Nil => Ok(default),
        Value::Integer(n) if n >= min as i64 && n <= max as i64 => Ok(n as usize),
        _ => Err(invalid(&format!(
            "MIDI input count must be an integer from {min} to {max}."
        ))),
    }
}
fn charge(counter: &Cell<usize>, maximum: usize) -> mlua::Result<()> {
    if counter.get() >= maximum {
        return Err(stop(
            Category::Budget,
            "MIDI input job attempts are exhausted.",
        ));
    }
    counter.set(counter.get() + 1);
    Ok(())
}
fn enumerate(job: &Job) -> mlua::Result<runtime::Enumerated> {
    #[cfg(test)]
    if let Some(s) = &job.simulation {
        return runtime::Enumerated::fixture(s.devices.clone(), &job.native).map_err(runtime_error);
    }
    runtime::enumerate(&job.lease, &job.native).map_err(runtime_error)
}
fn statistics(lua: &Lua, s: native::Stats) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (key, n) in [
        ("seen", s.seen),
        ("accepted", s.accepted),
        ("queue_dropped", s.queue_dropped),
        ("rate_dropped", s.rate_dropped),
        ("invalid", s.invalid),
        ("contended", s.contended),
        ("driver_overruns", s.driver_overruns),
        ("clock_regressions", s.clock_regressions),
    ] {
        t.raw_set(key, super::ticks::to_lua(n)?)?;
    }
    t.raw_set("queued", s.queued)?;
    t.raw_set("capacity", s.capacity)?;
    t.raw_set("status", s.status.name())?;
    Ok(t)
}
fn batch(lua: &Lua, b: native::Batch) -> mlua::Result<Table> {
    let result = lua.create_table()?;
    result.raw_set("count", b.events.len())?;
    result.raw_set("stats", statistics(lua, b.stats)?)?;
    let columns = lua.create_table()?;
    let keys = [
        "sequence",
        "driver_ms",
        "timestamp_ms",
        "received_ns",
        "out_of_order",
        "driver_overrun",
        "status",
        "data1",
        "data2",
        "kind",
        "channel",
        "note_on_zero",
    ];
    let arrays: Vec<Table> = keys
        .iter()
        .map(|_| lua.create_table_with_capacity(b.events.len(), 0))
        .collect::<mlua::Result<_>>()?;
    for (i, e) in b.events.into_iter().enumerate() {
        let row = i + 1;
        arrays[0].raw_set(row, super::ticks::to_lua(e.sequence)?)?;
        arrays[1].raw_set(row, e.driver_ms)?;
        arrays[2].raw_set(row, super::ticks::to_lua(e.timestamp_ms)?)?;
        arrays[3].raw_set(row, super::ticks::to_lua(e.received_ns)?)?;
        arrays[4].raw_set(row, e.out_of_order)?;
        arrays[5].raw_set(row, e.driver_overrun)?;
        arrays[6].raw_set(row, e.message.status)?;
        arrays[7].raw_set(row, e.message.data1)?;
        arrays[8].raw_set(row, e.message.data2)?;
        arrays[9].raw_set(row, e.message.kind.name())?;
        arrays[10].raw_set(row, e.message.channel().map_or(-1, i32::from))?;
        arrays[11].raw_set(row, e.message.note_on_zero())?;
    }
    for (key, array) in keys.into_iter().zip(arrays) {
        columns.raw_set(key, array)?;
    }
    result.raw_set("columns", columns)?;
    Ok(result)
}
struct Stream {
    job: Rc<Job>,
    state: Rc<RefCell<Option<Reader>>>,
}
impl Stream {
    fn live(&self, lua: &Lua) -> mlua::Result<()> {
        require(lua)?;
        self.job.lease.check().map_err(|e| e.lua())
    }
}
impl UserData for Stream {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("close", |_, s, ()| {
            Ok(s.state.borrow_mut().take().is_some())
        });
        methods.add_method("info", |lua, s, ()| {
            s.live(lua)?;
            let state = s.state.borrow();
            let r = state
                .as_ref()
                .ok_or_else(|| stop(Category::Expired, "This MIDI input stream is closed."))?;
            let t = statistics(lua, r.input.statistics().map_err(runtime_error)?)?;
            t.raw_set("name", r.name.as_str())?;
            Ok(t)
        });
        methods.add_method("read", |lua, s, (maximum, wait): (Value, Value)| {
            s.live(lua)?;
            let maximum = count(maximum, native::MAX_BATCH, 1, native::MAX_BATCH)?;
            let wait = count(wait, 0, 0, 1000)?;
            let state = s.state.borrow();
            let r = state
                .as_ref()
                .ok_or_else(|| stop(Category::Expired, "This MIDI input stream is closed."))?;
            let b = r.input.read(maximum, wait as u64).map_err(runtime_error)?;
            let delivered = s
                .job
                .delivered
                .get()
                .checked_add(b.events.len() as u64)
                .filter(|n| *n <= MAX_DELIVERED);
            let Some(delivered) = delivered else {
                r.input.close();
                return Err(stop(
                    Category::Budget,
                    "This job reached the 1048576 MIDI input event delivery limit.",
                ));
            };
            s.job.delivered.set(delivered);
            batch(lua, b)
        });
    }
}
pub(super) fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    lua.set_app_data(Slot::default());
    let api = lua.create_table()?;
    api.set(
        "devices",
        lua.create_function(|lua, ()| {
            let job = user_job(lua)?;
            charge(&job.enumerations, MAX_ENUMERATIONS)?;
            let enumerated = enumerate(&job)?;
            let inv = &enumerated.inventory;
            let t = lua.create_table()?;
            let rows = lua.create_table_with_capacity(inv.devices.len(), 0)?;
            for (i, d) in inv.devices.iter().enumerate() {
                let r = lua.create_table()?;
                r.raw_set("index", d.slot + 1)?;
                r.raw_set("name", d.name())?;
                rows.raw_set(i + 1, r)?;
            }
            t.raw_set("devices", rows)?;
            t.raw_set("reported", inv.reported)?;
            t.raw_set("unavailable", inv.unavailable)?;
            t.raw_set("truncated", inv.truncated)?;
            Ok(t)
        })?,
    )?;
    api.set(
        "open",
        lua.create_function(|lua, options: Value| {
            let job = user_job(lua)?;
            let capacity = match options {
                Value::Nil => 1024,
                Value::Table(t) if t.metatable().is_none() => {
                    for p in t.clone().pairs::<Value, Value>() {
                        if !matches!(p?.0,Value::String(s) if s.as_bytes().as_ref()==b"capacity") {
                            return Err(invalid("MIDI input options only accept capacity."));
                        }
                    }
                    count(t.raw_get("capacity")?, 1024, 1, native::MAX_QUEUE)?
                }
                _ => return Err(invalid("MIDI input options must be a plain table.")),
            };
            charge(&job.opens, MAX_OPENS)?;
            let client = lua
                .app_data_ref::<Slot>()
                .and_then(|s| s.client.clone())
                .ok_or_else(|| BridgeError::Unavailable.lua())?;
            let devices = Arc::new(enumerate(&job)?);
            let approved = client
                .request(Arc::clone(&job.lease), devices)
                .map_err(|e| e.lua())?;
            let Some(approved) = approved else {
                return Ok(None);
            };
            require(lua)?;
            job.lease.check().map_err(|e| e.lua())?;
            let name = approved.device().name();
            #[cfg(test)]
            let input = if let Some(s) = &job.simulation {
                s.open(approved, capacity, Arc::clone(&job.lease), &job.native)
                    .map_err(runtime_error)?
            } else {
                runtime::open(approved, capacity, Arc::clone(&job.lease), &job.native)
                    .map_err(runtime_error)?
            };
            #[cfg(not(test))]
            let input = runtime::open(approved, capacity, Arc::clone(&job.lease), &job.native)
                .map_err(runtime_error)?;
            let state = Rc::new(RefCell::new(Some(Reader { input, name })));
            job.states.borrow_mut().push(Rc::downgrade(&state));
            Ok(Some(Stream { job, state }))
        })?,
    )?;
    plum.set("midi_input", api)
}

#[cfg(test)]
#[path = "midi_input_tests.rs"]
mod tests;
#[cfg(test)]
use tests::Simulation;
