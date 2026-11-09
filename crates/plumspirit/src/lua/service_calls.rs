//! Host-mediated plain-data calls. No Lua references cross worker boundaries.
use super::bridge::{JobLease, ProjectStamp};
use super::reactive::Fence;
use super::services::Version;
use super::structured::Payload;
use super::{Capability, Category, Host, stop};
use mlua::{Lua, Table, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
};
use std::time::{Duration, Instant};

pub const MAX_PENDING: usize = 64;
pub const MAX_PER_PROVIDER: usize = 8;
pub const MAX_DEPTH: usize = 8;
pub const MAX_TREE_CALLS: usize = 64;
pub const MAX_TIME: Duration = Duration::from_millis(250);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub namespace: String,
    pub version: Version,
}
impl Dependency {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 141 {
            return Err("A service dependency may contain at most141 bytes.".into());
        }
        let (namespace, version) = text
            .split_once('@')
            .ok_or("Use provider.id/service@major.minor for a service dependency.")?;
        split_namespace(namespace)?;
        let version = Version::parse(version).ok_or("Invalid service dependency version.")?;
        Ok(Self {
            namespace: namespace.into(),
            version,
        })
    }
    pub fn text(&self) -> String {
        format!("{}@{}", self.namespace, self.version.text())
    }
}
pub fn split_namespace(value: &str) -> Result<(&str, &str), String> {
    if value.len() > 129 {
        return Err("A service namespace may contain at most129 bytes.".into());
    }
    let (owner, id) = value
        .split_once('/')
        .ok_or("A service namespace must be provider.id/service.")?;
    super::package::check_id(owner)?;
    super::plugin_api::check_decl_id(id, "service").map_err(|e| e.to_string())?;
    Ok((owner, id))
}
#[derive(Clone)]
pub struct Frame {
    pub leases: Vec<Arc<JobLease>>,
    pub lives: Vec<Arc<Fence>>,
    pub path: Vec<String>,
    pub deadline: Instant,
    pub project: Option<ProjectStamp>,
    pub authority: Arc<BTreeSet<Capability>>,
    pub calls: Arc<AtomicUsize>,
    call_id: u64,
}
impl Frame {
    pub(super) fn calls_id(&self) -> u64 {
        self.call_id
    }
    pub(super) fn set_calls_id(&mut self, id: u64) {
        self.call_id = id;
    }
    pub fn valid(&self) -> bool {
        Instant::now() < self.deadline
            && self.leases.iter().all(|l| l.check().is_ok())
            && self.lives.iter().all(|l| l.active())
    }
    pub fn matches(&self, current: Option<ProjectStamp>) -> bool {
        self.valid() && (self.project.is_none() || self.project == current)
    }
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub category: Category,
    pub message: String,
}
impl Failure {
    pub fn new(category: Category, message: impl AsRef<str>) -> Self {
        Self {
            category,
            message: message.as_ref().chars().take(1024).collect(),
        }
    }
    pub fn lua(self) -> mlua::Error {
        stop(self.category, self.message)
    }
}
pub struct Request {
    pub caller: Arc<str>,
    pub namespace: String,
    pub requested: Option<Version>,
    pub seed: i64,
    pub data: Payload,
    pub frame: Frame,
    reply: Option<SyncSender<Result<Payload, Failure>>>,
    pub fence: Arc<Fence>,
}
impl Request {
    pub fn respond(mut self, result: Result<Payload, Failure>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(result);
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            self.fence.invalidate();
            let _ = reply.try_send(Err(Failure::new(
                Category::Expired,
                "The service request was retired.",
            )));
        }
    }
}
#[derive(Clone)]
pub struct Client {
    plugin: Arc<str>,
    sender: SyncSender<Request>,
}
pub fn channel(plugin: &str) -> (Client, Receiver<Request>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    (
        Client {
            plugin: Arc::from(plugin),
            sender,
        },
        receiver,
    )
}
#[derive(Default)]
pub(super) struct Calls {
    pub client: Option<Client>,
    pub used: Arc<AtomicUsize>,
}
pub(super) fn begin_run(lua: &Lua) {
    if let Some(mut calls) = lua.app_data_mut::<Calls>() {
        calls.used = Arc::new(AtomicUsize::new(0));
    }
}
struct Expire(Arc<Fence>);
impl Drop for Expire {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}
fn call(
    lua: &Lua,
    namespace: String,
    data: Payload,
    requested: Option<Version>,
    seed: i64,
    time: Duration,
) -> mlua::Result<Payload> {
    super::reactive::check_read(lua)?;
    let base = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&base, Capability::CallServices)?;
    let (client, used) = lua
        .app_data_ref::<Calls>()
        .and_then(|c| c.client.clone().map(|client| (client, Arc::clone(&c.used))))
        .ok_or_else(|| {
            stop(
                Category::Capability,
                "No native service client is available.",
            )
        })?;
    let lease = lua
        .app_data_ref::<super::bridge::Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| stop(Category::Expired, "The service caller job expired."))?;
    lease.check().map_err(|e| e.lua())?;
    if lease.plugin.as_ref() != client.plugin.as_ref() {
        return Err(stop(
            Category::Expired,
            "The service caller identity changed.",
        ));
    }
    let parent = lua
        .app_data_ref::<super::services::Scope>()
        .and_then(|s| s.job.as_ref().and_then(|j| j.call.clone()));
    let authority: BTreeSet<_> = match &parent {
        Some(p) => base.intersection(&p.authority).copied().collect(),
        None => (*base).clone(),
    };
    Host::require(&authority, Capability::CallServices)?;
    let mut frame = parent.unwrap_or_else(|| Frame {
        leases: Vec::new(),
        lives: Vec::new(),
        path: vec![client.plugin.to_string()],
        deadline: lease.deadline(),
        project: lease.project,
        authority: Arc::new(authority.clone()),
        calls: used,
        call_id: 0,
    });
    if frame
        .path
        .last()
        .is_none_or(|p| p != client.plugin.as_ref())
        || frame.path.len() >= MAX_DEPTH
    {
        return Err(stop(
            Category::Budget,
            "The service call chain exceeds its depth budget.",
        ));
    }
    if frame.calls.fetch_add(1, Ordering::AcqRel) >= MAX_TREE_CALLS {
        return Err(stop(
            Category::Budget,
            "A root job may make at most64 service calls, including nested calls.",
        ));
    }
    frame.authority = Arc::new(authority);
    frame.deadline = frame
        .deadline
        .min(lease.deadline())
        .min(Instant::now() + time);
    frame.leases.push(lease);
    if let Some(d) = lua
        .app_data_ref::<super::reactive::Scope>()
        .and_then(|s| s.delivery.clone())
    {
        if d.event.kind != super::reactive::Kind::ProjectClosed {
            frame.project = Some(d.event.stamp);
        }
        frame.lives.push(d.fence);
        frame.lives.push(d.lifecycle);
    }
    let fence = Fence::live();
    let _expire = Expire(Arc::clone(&fence));
    frame.lives.push(Arc::clone(&fence));
    let (reply, receiver) = mpsc::sync_channel(1);
    let request = Request {
        caller: Arc::clone(&client.plugin),
        namespace,
        requested,
        seed,
        data,
        frame: frame.clone(),
        reply: Some(reply),
        fence,
    };
    match client.sender.try_send(request) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            return Err(stop(
                Category::Budget,
                "The caller's service request channel is full.",
            ));
        }
        Err(TrySendError::Disconnected(_)) => {
            return Err(stop(Category::Expired, "The service broker stopped."));
        }
    }
    loop {
        super::notes::check_budget(lua)?;
        super::reactive::check_read(lua)?;
        if !frame.valid() {
            return Err(stop(
                Category::TimeLimit,
                "The service call expired or exceeded its deadline.",
            ));
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => {
                if !frame.valid() {
                    return Err(stop(Category::Expired, "The service reply expired."));
                }
                return result.map_err(Failure::lua);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(stop(Category::Expired, "The service reply channel closed."));
            }
        }
    }
}
pub(super) fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let table = lua.create_table()?;
    table.set(
        "call",
        lua.create_function(
            |lua, (name, data, opts): (mlua::LuaString, Value, Option<Table>)| {
                if name.as_bytes().len() > 129 {
                    return Err(stop(
                        Category::Runtime,
                        "Service names allow at most129 bytes.",
                    ));
                }
                let namespace = name.to_str()?.to_owned();
                split_namespace(&namespace).map_err(|e| stop(Category::Runtime, e))?;
                let mut requested = None;
                let mut seed = 0i64;
                let mut timeout = MAX_TIME.as_millis() as u64;
                if let Some(opts) = opts {
                    if opts.metatable().is_some() {
                        return Err(stop(
                            Category::Runtime,
                            "Service call options must be plain.",
                        ));
                    }
                    if let Some(version) = opts.raw_get::<Option<mlua::LuaString>>("version")? {
                        requested = Some(Version::parse(&version.to_str()?).ok_or_else(|| {
                            stop(Category::Runtime, "Invalid requested service version.")
                        })?);
                    }
                    seed = opts.raw_get::<Option<i64>>("seed")?.unwrap_or(0);
                    timeout = opts.raw_get::<Option<u64>>("timeout_ms")?.unwrap_or(250);
                }
                if !(10..=250).contains(&timeout) {
                    return Err(stop(Category::Runtime, "Service timeout must be10–250 ms."));
                }
                let data = Payload::read(lua, data)?;
                call(
                    lua,
                    namespace,
                    data,
                    requested,
                    seed,
                    Duration::from_millis(timeout),
                )?
                .to_lua(lua)
            },
        )?,
    )?;
    plum.set("services", table)
}

struct Edge {
    from: String,
    to: String,
    frame: Frame,
}
#[derive(Default)]
pub struct Graph {
    next: u64,
    edges: BTreeMap<u64, Edge>,
}
impl Graph {
    pub fn clean(&mut self, current: Option<ProjectStamp>) {
        self.edges.retain(|_, e| e.frame.matches(current));
    }
    pub fn waiting(&self, owner: &str) -> bool {
        self.edges
            .values()
            .any(|e| e.from == owner && e.frame.valid())
    }
    pub fn complete(&mut self, id: u64) {
        self.edges.remove(&id);
    }
    pub fn admit(&mut self, from: &str, to: &str, frame: &Frame) -> Result<u64, Failure> {
        if self.edges.len() >= MAX_PENDING {
            return Err(Failure::new(
                Category::Budget,
                "The global64-call service budget is full.",
            ));
        }
        if from == to || frame.path.iter().any(|p| p == to) {
            return Err(Failure::new(
                Category::Runtime,
                "A service call would reenter a waiting plugin worker.",
            ));
        }
        let mut pending = vec![to];
        let mut seen = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            if owner == from {
                return Err(Failure::new(
                    Category::Runtime,
                    "The service wait graph would form a deadlock cycle.",
                ));
            }
            if seen.insert(owner) {
                for edge in self.edges.values().filter(|e| e.from == owner) {
                    pending.push(edge.to.as_str());
                }
            }
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| Failure::new(Category::Budget, "Service call IDs exhausted."))?;
        self.edges.insert(
            self.next,
            Edge {
                from: from.into(),
                to: to.into(),
                frame: frame.clone(),
            },
        );
        Ok(self.next)
    }
}

#[cfg(test)]
#[path = "service_calls_tests.rs"]
mod tests;
