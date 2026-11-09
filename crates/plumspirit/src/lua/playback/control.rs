//! Bounded explicit worker-to-native-owner transport requests.
use super::Snapshot;
use crate::lua::{
    Capability, Category, Host,
    bridge::{JobLease, Reservation},
};
use mlua::{Lua, Table, Value};
use plumspirit_engine::model::tempo::Speed;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
};
use std::time::{Duration, Instant};
pub const MAX_REQUESTS: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Play,
    Pause,
    Stop,
    Seek(u64),
    Speed(Speed),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Expired,
    Canceled,
    Deadline,
    Permission,
    NotUserInvoked,
    Unavailable,
    Busy,
    Budget,
    Conflict,
}
impl Error {
    pub fn lua(self) -> mlua::Error {
        let (c, m) = match self {
            Self::Expired => (
                Category::Expired,
                "The transport request belongs to an expired job or project revision.",
            ),
            Self::Canceled => (
                Category::Canceled,
                "The transport request was canceled before acknowledgement.",
            ),
            Self::Deadline => (
                Category::TimeLimit,
                "The transport request exceeded the plugin job deadline.",
            ),
            Self::Permission => (
                Category::Capability,
                "Transport requests require playback.read and playback.control.",
            ),
            Self::NotUserInvoked => (
                Category::Capability,
                "Transport control requires an explicitly invoked command or non-preview tool.",
            ),
            Self::Unavailable => (
                Category::Runtime,
                "The native transport owner is unavailable.",
            ),
            Self::Busy => (
                Category::Runtime,
                "Transport request is busy or rate-limited; try again after a user action.",
            ),
            Self::Budget => (
                Category::Budget,
                "The job exceeded its bounded transport request budget.",
            ),
            Self::Conflict => (
                Category::Expired,
                "A competing native or plugin transport action invalidated this request; invoke the command again.",
            ),
        };
        crate::lua::stop(c, m)
    }
    fn from_bridge(e: crate::lua::bridge::BridgeError) -> Self {
        use crate::lua::bridge::BridgeError as B;
        match e {
            B::Canceled => Self::Canceled,
            B::Deadline => Self::Deadline,
            B::Budget => Self::Budget,
            B::Busy => Self::Busy,
            B::NotUserInvoked => Self::NotUserInvoked,
            B::Unavailable => Self::Unavailable,
            B::Expired => Self::Expired,
        }
    }
}
pub struct Request {
    pub lease: Arc<JobLease>,
    pub id: usize,
    pub expected: Snapshot,
    pub action: Action,
    reply: SyncSender<Result<Snapshot, Error>>,
    _charge: Reservation,
}
impl Request {
    pub fn respond(self, result: Result<Snapshot, Error>) {
        let _ = self.reply.try_send(result);
    }
}
#[derive(Clone)]
pub struct Client {
    plugin: Arc<str>,
    sender: SyncSender<Request>,
}
pub fn channel(plugin: &str) -> (Client, Receiver<Request>) {
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
        id: usize,
        expected: Snapshot,
        action: Action,
        charge: Reservation,
    ) -> Result<Snapshot, Error> {
        lease.check().map_err(Error::from_bridge)?;
        if !lease.user_invoked() {
            return Err(Error::NotUserInvoked);
        }
        if lease.plugin != self.plugin || lease.project != Some(expected.stamp) {
            return Err(Error::Expired);
        }
        let (reply, rx) = mpsc::sync_channel(1);
        self.sender
            .try_send(Request {
                lease: Arc::clone(&lease),
                id,
                expected,
                action,
                reply,
                _charge: charge,
            })
            .map_err(|e| match e {
                TrySendError::Full(_) => Error::Busy,
                TrySendError::Disconnected(_) => Error::Unavailable,
            })?;
        loop {
            lease.check().map_err(Error::from_bridge)?;
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => {
                    lease.check().map_err(Error::from_bridge)?;
                    return result;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(Error::Unavailable),
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct Slot {
    pub client: Option<Client>,
    pub(crate) count: usize,
    pub(crate) expected: Option<Snapshot>,
}
pub(crate) fn begin_run(lua: &Lua) {
    let expected = lua
        .app_data_ref::<crate::lua::handles::WorldSlot>()
        .and_then(|w| w.0.as_ref().and_then(|w| w.info.playback));
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.count = 0;
        s.expected = expected;
    }
}
pub(crate) fn end_run(lua: &Lua) {
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.expected = None;
    }
}
fn invalid(message: &str) -> mlua::Error {
    crate::lua::stop(Category::Runtime, message)
}
fn parse(t: Table) -> mlua::Result<Action> {
    if t.metatable().is_some() {
        return Err(invalid("Transport options must be a plain table."));
    }
    let Value::String(name) = t.raw_get::<Value>("action")? else {
        return Err(invalid("Transport action must be text."));
    };
    if name.as_bytes().len() > 16 {
        return Err(invalid("Transport action name is too long."));
    }
    let name = name.to_str()?;
    let allowed: &[&str] = match name.as_ref() {
        "play" | "pause" | "stop" => &["action"],
        "seek" => &["action", "tick"],
        "speed" => &["action", "speed"],
        _ => {
            return Err(invalid(
                "Unknown transport action; supported: play, pause, stop, seek, speed.",
            ));
        }
    };
    let mut count = 0;
    for pair in t.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        count += 1;
        if count > allowed.len() {
            return Err(invalid("Unexpected transport option."));
        }
        let Value::String(key) = key else {
            return Err(invalid("Transport option names must be text."));
        };
        if key.as_bytes().len() > 16 || !allowed.contains(&key.to_str()?.as_ref()) {
            return Err(invalid("Unexpected transport option."));
        }
    }
    Ok(match name.as_ref() {
        "play" => Action::Play,
        "pause" => Action::Pause,
        "stop" => Action::Stop,
        "seek" => Action::Seek(crate::lua::ticks::from_lua(
            &t.raw_get("tick")?,
            "seek tick",
        )?),
        "speed" => {
            let Value::String(speed) = t.raw_get::<Value>("speed")? else {
                return Err(invalid("Transport speed must be native decimal text."));
            };
            if speed.as_bytes().len() > 64 {
                return Err(invalid("Transport speed text exceeds64bytes."));
            }
            Action::Speed(
                Speed::parse(&speed.to_str()?)
                    .map_err(|_| invalid("Invalid native playback speed."))?,
            )
        }
        _ => unreachable!(),
    })
}
pub(crate) fn install(lua: &Lua, playback: &Table) -> mlua::Result<()> {
    playback.set("max_requests_per_job", MAX_REQUESTS)?;
    playback.set(
        "request",
        lua.create_function(|lua, t: Table| {
            crate::lua::reactive::check_write(lua)?;
            super::midi::check_transaction(lua)?;
            let grants = lua
                .app_data_ref::<crate::lua::handles::Granted>()
                .map(|g| Arc::clone(&g.0))
                .unwrap_or_default();
            Host::require(&grants, Capability::ReadPlayback)?;
            Host::require(&grants, Capability::ControlPlayback)?;
            let lease = lua
                .app_data_ref::<crate::lua::bridge::Session>()
                .and_then(|s| s.lease.clone())
                .ok_or_else(|| Error::Expired.lua())?;
            lease.check().map_err(|e| Error::from_bridge(e).lua())?;
            if !lease.user_invoked() {
                return Err(Error::NotUserInvoked.lua());
            }
            let action = parse(t)?;
            let (client, id, expected) = {
                let mut s = lua
                    .app_data_mut::<Slot>()
                    .ok_or_else(|| Error::Unavailable.lua())?;
                if s.count >= MAX_REQUESTS {
                    return Err(Error::Budget.lua());
                }
                s.count += 1;
                (
                    s.client.clone().ok_or_else(|| Error::Unavailable.lua())?,
                    s.count,
                    s.expected.ok_or_else(|| Error::Expired.lua())?,
                )
            };
            let budget = lua
                .app_data_ref::<crate::lua::bridge::NativeBudget>()
                .ok_or_else(|| Error::Unavailable.lua())?
                .clone();
            let charge = budget.reserve(4096).map_err(|_| Error::Budget.lua())?;
            let snapshot = client
                .request(lease, id, expected, action, charge)
                .map_err(Error::lua)?;
            if let Some(mut s) = lua.app_data_mut::<Slot>() {
                s.expected = Some(snapshot);
            }
            let out = lua.create_table()?;
            out.set("request_id", id)?;
            out.set("snapshot", snapshot.table(lua)?)?;
            Ok(out)
        })?,
    )
}
/// Native owner admission. At most8recent entries can normally be created per second.
#[derive(Default)]
pub struct Admission {
    last: Option<Instant>,
    plugins: BTreeMap<Arc<str>, Instant>,
}
impl Admission {
    pub fn admit(&mut self, plugin: &Arc<str>, now: Instant) -> Result<(), Error> {
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_millis(125))
            || self
                .plugins
                .get(plugin)
                .is_some_and(|t| now.saturating_duration_since(*t) < Duration::from_millis(250))
        {
            return Err(Error::Busy);
        }
        self.plugins
            .retain(|_, t| now.saturating_duration_since(*t) < Duration::from_secs(1));
        if !self.plugins.contains_key(plugin) && self.plugins.len() >= 16 {
            return Err(Error::Budget);
        }
        self.last = Some(now);
        self.plugins.insert(Arc::clone(plugin), now);
        Ok(())
    }
}
#[cfg(test)]
#[path = "../playback_control_tests.rs"]
mod tests;
