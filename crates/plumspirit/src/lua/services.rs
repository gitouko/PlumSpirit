//! S05A provider foundation. Cross-plugin routing/dependencies belong to the
//! next slice; no public client-call API is claimed here.
use super::reactive::Fence;
use super::structured::Payload;
use super::{Capability, Category, Host, stop};
use mlua::{Function, Lua, Table, Value};
use std::sync::Arc;
const HANDLERS: &str = "plumspirit.plugin.services";
pub const MAX_SERVICES: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}
impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > 11 {
            return None;
        }
        let (major, minor) = text.split_once('.')?;
        if major.is_empty()
            || minor.is_empty()
            || !major.bytes().all(|b| b.is_ascii_digit())
            || !minor.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
    pub fn supports(self, requested: Self) -> bool {
        self.major == requested.major && self.minor >= requested.minor
    }
    pub fn text(self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceDecl {
    pub id: String,
    pub namespace: String,
    pub version: Version,
}
#[derive(Clone)]
pub struct Job {
    pub id: String,
    pub version: Version,
    pub data: Payload,
    pub lifecycle: Arc<Fence>,
    pub seed: i64,
    pub call: Option<super::service_calls::Frame>,
}
#[derive(Default)]
pub(super) struct Scope {
    pub job: Option<Job>,
    reply: Option<Payload>,
    saved_phase: Option<super::plugin_api::Phase>,
    dispatching: bool,
    base_grants: Option<Arc<std::collections::BTreeSet<Capability>>>,
}
pub(super) fn active(lua: &Lua) -> bool {
    lua.app_data_ref::<Scope>().is_some_and(|s| s.job.is_some())
}
pub(super) fn check_read(lua: &Lua) -> mlua::Result<()> {
    if lua.app_data_ref::<Scope>().is_some_and(|s| {
        s.job
            .as_ref()
            .is_some_and(|j| !j.lifecycle.active() || j.call.as_ref().is_some_and(|c| !c.valid()))
    }) {
        return Err(stop(
            Category::Expired,
            "The service provider lifecycle expired.",
        ));
    }
    Ok(())
}
pub(super) fn check_write(lua: &Lua) -> mlua::Result<()> {
    check_read(lua)?;
    if active(lua) {
        return Err(stop(
            Category::Capability,
            "Service providers process plain data; project, UI and settings writes are not allowed.",
        ));
    }
    Ok(())
}
pub(super) fn enter(lua: &Lua, job: Option<Job>) {
    let saved_phase = job.as_ref().and_then(|_| {
        lua.app_data_ref::<super::plugin_api::PluginSlot>()
            .map(|s| s.phase)
    });
    let base_grants = job.as_ref().and_then(|_| {
        lua.app_data_ref::<super::handles::Granted>()
            .map(|g| Arc::clone(&g.0))
    });
    if let Some(frame) = job.as_ref().and_then(|j| j.call.as_ref()) {
        lua.set_app_data(super::handles::Granted(Arc::clone(&frame.authority)));
    }
    lua.set_app_data(Scope {
        job,
        reply: None,
        saved_phase,
        dispatching: false,
        base_grants,
    });
}
pub(super) fn finish(lua: &Lua, ok: bool) -> Option<Payload> {
    let mut scope = lua.app_data_mut::<Scope>()?;
    scope.job = None;
    scope.dispatching = false;
    let base_grants = scope.base_grants.take();
    let phase = scope.saved_phase.take();
    let reply = scope.reply.take();
    drop(scope);
    if let Some(base) = base_grants {
        lua.set_app_data(super::handles::Granted(base));
    }
    if let Some(phase) = phase
        && let Some(mut slot) = lua.app_data_mut::<super::plugin_api::PluginSlot>()
    {
        slot.phase = phase;
    }
    if ok { reply } else { None }
}
fn permission(lua: &Lua) -> mlua::Result<()> {
    // Providing an entrypoint is the provider's own role; delegated read/call
    // capabilities are separately intersected for the service body.
    let caps = lua
        .app_data_ref::<Scope>()
        .and_then(|s| s.base_grants.clone())
        .or_else(|| {
            lua.app_data_ref::<super::handles::Granted>()
                .map(|c| Arc::clone(&c.0))
        })
        .unwrap_or_default();
    Host::require(&caps, Capability::ProvideServices)
}
pub(super) fn install(lua: &Lua, plugin: &Table) -> mlua::Result<()> {
    let plum: Table = lua.globals().get("plum")?;
    super::service_calls::install(lua, &plum)?;
    lua.set_named_registry_value(HANDLERS, lua.create_table()?)?;
    plugin.set(
        "service",
        lua.create_function(|lua, opts: Table| {
            permission(lua)?;
            super::reactive::check_read(lua)?;
            if active(lua) || super::reactive::observing(lua) {
                return Err(stop(
                    Category::Capability,
                    "Service declarations cannot change during automatic delivery.",
                ));
            }
            super::plugin_api::require_loading(lua, "Services")?;
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
                return Err(stop(
                    Category::Runtime,
                    "Service options must be a plain table.",
                ));
            }
            let id: mlua::LuaString = opts.raw_get("id")?;
            if id.as_bytes().len() > 64 {
                return Err(stop(
                    Category::Runtime,
                    "Service IDs allow at most 64 bytes.",
                ));
            }
            let id = id.to_str()?.to_owned();
            super::plugin_api::check_decl_id(&id, "service")?;
            let version: mlua::LuaString = opts.raw_get("version")?;
            let version = Version::parse(&version.to_str()?).ok_or_else(|| {
                stop(
                    Category::Runtime,
                    "Service version must be major.minor with each component 0–65535.",
                )
            })?;
            let run: Function = opts.raw_get("run")?;
            let mut slot = lua
                .app_data_mut::<super::plugin_api::PluginSlot>()
                .ok_or_else(|| stop(Category::Runtime, "No provider plugin is active."))?;
            if slot.decls.services.len() >= MAX_SERVICES {
                return Err(stop(
                    Category::Budget,
                    "A plugin may provide at most 32 services.",
                ));
            }
            if slot.decls.services.iter().any(|d| d.id == id) {
                return Err(stop(
                    Category::Runtime,
                    "This service ID is already registered.",
                ));
            }
            let namespace = format!("{}/{}", slot.meta.id, id);
            slot.decls.services.push(ServiceDecl {
                id: id.clone(),
                namespace,
                version,
            });
            slot.changed = true;
            drop(slot);
            lua.named_registry_value::<Table>(HANDLERS)?
                .raw_set(id, run)?;
            Ok(())
        })?,
    )?;
    plugin.set(
        "__service",
        lua.create_function(|lua, ()| {
            permission(lua)?;
            check_read(lua)?;
            let job = lua
                .app_data_ref::<Scope>()
                .and_then(|s| s.job.clone())
                .ok_or_else(|| stop(Category::Capability, "No native service job is active."))?;
            let decl = lua
                .app_data_ref::<super::plugin_api::PluginSlot>()
                .and_then(|s| s.decls.services.iter().find(|d| d.id == job.id).cloned())
                .ok_or_else(|| {
                    stop(
                        Category::Runtime,
                        "This provider service is not registered.",
                    )
                })?;
            if !decl.version.supports(job.version) {
                return Err(stop(
                    Category::Compatibility,
                    "The provider does not support the requested service version.",
                ));
            }
            let callback: Function = lua
                .named_registry_value::<Table>(HANDLERS)?
                .raw_get(job.id.as_str())?;
            let context = lua.create_table()?;
            context.set("service", decl.namespace)?;
            context.set("version", decl.version.text())?;
            context.set("seed", job.seed)?;
            if lua.app_data_ref::<Scope>().is_some_and(|s| s.dispatching) {
                return Err(stop(
                    Category::Runtime,
                    "Native service dispatch cannot reenter itself.",
                ));
            }
            lua.app_data_mut::<Scope>()
                .expect("service scope")
                .dispatching = true;
            let result = callback.call::<Value>((job.data.to_lua(lua)?, context));
            lua.app_data_mut::<Scope>()
                .expect("service scope")
                .dispatching = false;
            let value = result?;
            check_read(lua)?;
            let reply = Payload::read(lua, value)?;
            lua.app_data_mut::<Scope>()
                .ok_or_else(|| stop(Category::Internal, "Service job disappeared."))?
                .reply = Some(reply);
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
