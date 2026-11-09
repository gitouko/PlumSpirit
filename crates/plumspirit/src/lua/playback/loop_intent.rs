//! A bounded root-free loop intent, sealed only after a successful worker run.
use super::{
    Snapshot,
    control::{self, Error},
};
use crate::lua::{
    Capability, Category, Host,
    bridge::{JobLease, Reservation},
};
use mlua::{Lua, Table, Value};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    pub range: Option<(u64, u64)>,
    pub enabled: bool,
}
struct Staged {
    setting: Setting,
    lease: Arc<JobLease>,
    charge: Reservation,
}
struct Data {
    setting: Setting,
    guard: Snapshot,
    lease: Arc<JobLease>,
    epoch: Arc<AtomicU64>,
    version: u64,
    used: AtomicBool,
    _charge: Reservation,
}
#[derive(Clone)]
pub struct Intent(Arc<Data>);
impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopIntent")
            .field("job", &self.0.lease.job)
            .field("setting", &self.0.setting)
            .field("guard", &self.0.guard)
            .finish()
    }
}
impl PartialEq for Intent {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Intent {
    pub fn valid(&self) -> bool {
        !self.0.used.load(Ordering::Acquire)
            && self.0.epoch.load(Ordering::Acquire) == self.0.version
            && !self.0.lease.cancellation_requested()
            && std::time::Instant::now() < self.0.lease.deadline()
    }
    pub fn claim(&self) -> bool {
        self.valid()
            && self
                .0
                .used
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
    pub fn guard(&self) -> Snapshot {
        self.0.guard
    }
    pub fn setting(&self) -> Setting {
        self.0.setting
    }
    pub fn plugin(&self) -> &Arc<str> {
        &self.0.lease.plugin
    }
}
#[derive(Default)]
pub(crate) struct Slot {
    pub epoch: Arc<AtomicU64>,
    version: u64,
    staged: Option<Staged>,
}
pub(crate) fn begin_run(lua: &Lua) {
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.staged = None;
        s.version = s.epoch.fetch_add(1, Ordering::AcqRel).saturating_add(1);
    }
}
pub(crate) fn end_run(lua: &Lua, ok: bool) -> Option<Intent> {
    let expected = lua.app_data_ref::<control::Slot>().and_then(|s| s.expected);
    let mut s = lua.app_data_mut::<Slot>()?;
    let staged = s.staged.take()?;
    if !ok || staged.lease.check().is_err() {
        return None;
    }
    let guard = expected?;
    if staged.lease.project != Some(guard.stamp) {
        return None;
    }
    Some(Intent(Arc::new(Data {
        setting: staged.setting,
        guard,
        lease: staged.lease,
        epoch: Arc::clone(&s.epoch),
        version: s.version,
        used: AtomicBool::new(false),
        _charge: staged.charge,
    })))
}
pub(crate) fn check_transaction(lua: &Lua) -> mlua::Result<()> {
    if lua
        .app_data_ref::<Slot>()
        .is_some_and(|s| s.staged.is_some())
    {
        return Err(bad(
            "Musical transactions cannot follow a staged loop intent; use separate commands.",
        ));
    }
    Ok(())
}
fn bad(text: &str) -> mlua::Error {
    crate::lua::stop(Category::Runtime, text)
}
fn parse(t: Table) -> mlua::Result<Setting> {
    if t.metatable().is_some() {
        return Err(bad("Loop options must be a plain table."));
    }
    let mut count = 0;
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        count += 1;
        let Value::String(k) = k else {
            return Err(bad("Loop option names must be text."));
        };
        if count > 4
            || k.as_bytes().len() > 7
            || !["start", "finish", "enabled", "clear"].contains(&k.to_str()?.as_ref())
        {
            return Err(bad("Unknown loop option."));
        }
    }
    match t.raw_get::<Value>("clear")? {
        Value::Boolean(true) => {
            if count != 1 {
                return Err(bad("Clear loop accepts only {clear=true}."));
            }
            return Ok(Setting {
                range: None,
                enabled: false,
            });
        }
        Value::Nil => {}
        _ => return Err(bad("The optional clear flag must be true.")),
    }
    let start = crate::lua::ticks::from_lua(&t.raw_get("start")?, "loop start")?;
    let finish = crate::lua::ticks::from_lua(&t.raw_get("finish")?, "loop finish")?;
    if start >= finish {
        return Err(bad("Loop finish must be greater than start."));
    }
    let enabled = match t.raw_get::<Value>("enabled")? {
        Value::Nil => true,
        Value::Boolean(v) => v,
        _ => return Err(bad("Loop enabled must be a boolean.")),
    };
    Ok(Setting {
        range: Some((start, finish)),
        enabled,
    })
}
pub(crate) fn install(lua: &Lua, playback: &Table) -> mlua::Result<()> {
    playback.set(
        "stage_loop",
        lua.create_function(|lua, t: Table| {
            crate::lua::reactive::check_write(lua)?;
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
            lease.check().map_err(|e| e.lua())?;
            if !lease.user_invoked() {
                return Err(Error::NotUserInvoked.lua());
            }
            if crate::lua::tx::has_pending_work(lua) {
                return Err(bad(
                    "Stage loop in a standalone command without musical transactions.",
                ));
            }
            super::midi::check_transaction(lua)?;
            let setting = parse(t)?;
            {
                let mut c = lua
                    .app_data_mut::<control::Slot>()
                    .ok_or_else(|| Error::Unavailable.lua())?;
                if c.count >= control::MAX_REQUESTS {
                    return Err(Error::Budget.lua());
                }
                c.count += 1;
                let expected = c.expected.ok_or_else(|| Error::Expired.lua())?;
                if lease.project != Some(expected.stamp) {
                    return Err(Error::Expired.lua());
                }
            }
            if lua
                .app_data_ref::<Slot>()
                .is_some_and(|s| s.staged.is_some())
            {
                return Err(bad("Only one loop intent may be staged per job."));
            }
            let budget = lua
                .app_data_ref::<crate::lua::bridge::NativeBudget>()
                .ok_or_else(|| Error::Unavailable.lua())?
                .clone();
            let charge = budget.reserve(4096).map_err(|_| Error::Budget.lua())?;
            lua.app_data_mut::<Slot>()
                .ok_or_else(|| Error::Unavailable.lua())?
                .staged = Some(Staged {
                setting,
                lease,
                charge,
            });
            let out = lua.create_table()?;
            out.set("staged", true)?;
            out.set("enabled", setting.enabled)?;
            if let Some((a, b)) = setting.range {
                out.set("start", crate::lua::ticks::to_lua(a)?)?;
                out.set("finish", crate::lua::ticks::to_lua(b)?)?;
            }
            Ok(out)
        })?,
    )
}

#[cfg(test)]
#[path = "../playback_loop_tests.rs"]
mod tests;
