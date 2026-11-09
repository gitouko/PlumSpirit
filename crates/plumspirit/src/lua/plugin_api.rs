//! The `plum.plugin` table of a plugin's Lua state (B1-0038, `docs/LUA_API.md`
//! "Plugins"): the plugin's identity, its registrations and its own files.
//!
//! Registrations (commands now; custom tools and the UI families build on the
//! same table) are allowed while the plugin loads — at the top level of its
//! entry script, in `on_load()` and in `register()` — and refused afterwards,
//! so a plugin's menu entries are fixed once it is ready. A reload is a new
//! Lua state with an empty table: nothing is registered twice. The host keeps
//! only the declarations (ids, names, tooltips); the functions stay in the
//! plugin's Lua state.

use super::{Category, stop};
use mlua::{Function, Lua, Table, Value};
use std::path::PathBuf;

/// What the host knows about the plugin it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The installed package folder (`plum.plugin.include` reads inside it).
    pub root: PathBuf,
    /// The plugin's data folder (`<settings>\plugin-data\<id>`; settings and
    /// presets, B1-0035); `None`: no settings folder.
    pub data_dir: Option<PathBuf>,
    /// The manifest's `config_version` (the settings layout).
    pub config_version: u64,
}

/// A command a plugin registered.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandDecl {
    pub id: String,
    pub name: String,
    pub tooltip: String,
    /// B1-0034: options asked in a native dialog before it runs (the custom
    /// tools' parameter kinds); empty: it runs at once.
    pub params: Vec<super::custom::ToolParam>,
}

/// How a notification reads (`plum.ui.notify`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warning,
    Error,
}

/// Notifications one run may post (the rest are dropped).
pub const MAX_NOTICES: usize = 5;

/// Everything a plugin registered (Send: no Lua values).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decls {
    pub commands: Vec<CommandDecl>,
    /// Custom note tools (Tools → Custom, B1-0045).
    pub tools: Vec<super::custom::ToolDecl>,
    pub observers: Vec<super::reactive::ObserverDecl>,
    pub services: Vec<super::services::ServiceDecl>,
    pub formats: Vec<super::formats::FormatDecl>,
}

impl Decls {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.tools.is_empty()
            && self.observers.is_empty()
            && self.services.is_empty()
            && self.formats.is_empty()
    }
}

/// When registrations are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The entry script's top level and `on_load()`.
    Load,
    /// `register()`.
    Register,
    /// Loaded: registrations are refused.
    Ready,
}

/// The plugin state of one Lua state (app data).
pub(super) struct PluginSlot {
    pub meta: PluginMeta,
    pub phase: Phase,
    pub extensibility_ready: bool,
    pub decls: Decls,
    /// The declarations changed since the host last reported them.
    pub changed: bool,
}

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// The Lua registry table holding command functions by id.
const COMMANDS: &str = "plumspirit.plugin.commands";
/// The Lua registry table holding custom tool handlers by id.
pub(super) const TOOLS: &str = "plumspirit.plugin.tools";

/// A command / tool id: 1 – 64 characters `a-z 0-9 . _ -`.
pub fn check_decl_id(id: &str, what: &str) -> mlua::Result<()> {
    let ok = (1..=64).contains(&id.len())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
        });
    if ok {
        Ok(())
    } else {
        Err(runtime(format!(
            "The {what} id \u{201C}{id}\u{201D} must be 1 – 64 characters of a – z, 0 – 9, \u{201C}.\u{201D}, \u{201C}_\u{201D} or \u{201C}-\u{201D}."
        )))
    }
}

/// A display text field: required, 1 – `max` characters, no control
/// characters.
pub fn text_field(
    t: &Table,
    key: &str,
    what: &str,
    max: usize,
    required: bool,
) -> mlua::Result<String> {
    let v: Option<String> = t
        .get(key)
        .map_err(|_| runtime(format!("{what}: {key} must be a text.")))?;
    let v = v.unwrap_or_default().trim().to_owned();
    if required && v.is_empty() {
        return Err(runtime(format!("{what}: {key} is missing.")));
    }
    if v.chars().count() > max || v.chars().any(|c| c.is_control() && c != '\n') {
        return Err(runtime(format!(
            "{what}: {key} must be at most {max} characters without control characters."
        )));
    }
    Ok(v)
}

/// Fails unless the plugin is still loading.
pub(super) fn require_loading(lua: &Lua, what: &str) -> mlua::Result<()> {
    super::services::check_write(lua)?;
    let phase = lua
        .app_data_ref::<PluginSlot>()
        .map(|s| s.phase)
        .ok_or_else(|| runtime("Only plugins can register commands and tools."))?;
    if phase == Phase::Ready {
        return Err(runtime(format!(
            "{what} must be registered while the plugin loads (at the top of its script, in on_load() or in register()), not later."
        )));
    }
    Ok(())
}

/// Installs `plum.plugin` for the plugin `meta`.
pub(super) fn install(lua: &Lua, meta: PluginMeta) -> mlua::Result<()> {
    let plum: Table = lua.globals().get("plum")?;
    let p = lua.create_table()?;
    super::files::install_assets(lua, &p)?;
    super::config::install(
        lua,
        &plum,
        super::config::ConfigSlot::new(&meta.id, meta.data_dir.clone(), meta.config_version),
    )?;
    let ui = plum.raw_get::<Table>("ui")?;
    ui.set(
        "notify",
        lua.create_function(
            |lua, (text, kind): (mlua::LuaString, Option<mlua::LuaString>)| {
                super::services::check_write(lua)?;
                let kind = kind.as_ref().map(mlua::LuaString::to_str).transpose()?;
                let kind = match kind.as_deref().unwrap_or("info") {
                    "info" => NoticeKind::Info,
                    "warning" => NoticeKind::Warning,
                    "error" => NoticeKind::Error,
                    other => {
                        let other = super::diagnostic_text(other, 64);
                        return Err(runtime(format!(
                            "notify's kind is \"info\", \"warning\" or \"error\", not \"{other}\"."
                        )));
                    }
                };
                let text: String = text
                    .to_str()?
                    .chars()
                    .filter(|c| !c.is_control() || *c == ' ')
                    .take(300)
                    .collect();
                if let Some(mut n) = lua.app_data_mut::<Notices>()
                    && n.0.len() < MAX_NOTICES
                    && !text.trim().is_empty()
                {
                    n.0.push((text, kind));
                }
                Ok(())
            },
        )?,
    )?;
    plum.set("ui", ui)?;
    lua.set_app_data(Notices::default());
    p.set("id", meta.id.clone())?;
    p.set("name", meta.name.clone())?;
    p.set("version", meta.version.clone())?;
    lua.set_named_registry_value(COMMANDS, lua.create_table()?)?;
    lua.set_named_registry_value(TOOLS, lua.create_table()?)?;
    lua.set_app_data(PluginSlot {
        meta,
        phase: Phase::Load,
        extensibility_ready: false,
        decls: Decls::default(),
        changed: false,
    });
    p.set(
        "command",
        lua.create_function(|lua, t: Table| {
            require_loading(lua, "A command")?;
            let id: String = t
                .get::<Option<String>>("id")
                .map_err(|_| runtime("command{}: id must be a text."))?
                .ok_or_else(|| runtime("command{}: id is missing."))?;
            check_decl_id(&id, "command")?;
            let name = text_field(&t, "name", "command{}", 80, true)?;
            let tooltip = text_field(&t, "tooltip", "command{}", 400, false)?;
            let params = super::custom::params_from_lua(&t, "command{}")?;
            let run: Function = match t.get::<Value>("run")? {
                Value::Function(f) => f,
                _ => return Err(runtime("command{}: run must be a function.")),
            };
            let mut slot = lua
                .app_data_mut::<PluginSlot>()
                .ok_or_else(|| runtime("Only plugins can register commands."))?;
            if slot.decls.commands.iter().any(|c| c.id == id) {
                return Err(runtime(format!(
                    "The command \u{201C}{id}\u{201D} is already registered by this plugin."
                )));
            }
            slot.decls.commands.push(CommandDecl {
                id: id.clone(),
                name,
                tooltip,
                params,
            });
            slot.changed = true;
            drop(slot);
            let fns: Table = lua.named_registry_value(COMMANDS)?;
            fns.set(id, run)?;
            Ok(())
        })?,
    )?;
    p.set(
        "include",
        lua.create_function(|lua, path: String| -> mlua::Result<mlua::MultiValue> {
            let root = lua
                .app_data_ref::<PluginSlot>()
                .map(|s| s.meta.root.clone())
                .ok_or_else(|| runtime("include() is only available to plugins."))?;
            let rel = super::package::safe_relative(&path).map_err(runtime)?;
            if !path.to_ascii_lowercase().ends_with(".lua") {
                return Err(runtime(format!(
                    "include() reads .lua files only (not {path})."
                )));
            }
            let full = root.join(rel);
            let meta = std::fs::metadata(&full)
                .map_err(|_| runtime(format!("{path} is not in the plugin's folder.")))?;
            if meta.len() > super::package::MAX_FILE_BYTES {
                return Err(runtime(format!("{path} is too large to include.")));
            }
            let src = std::fs::read_to_string(&full)
                .map_err(|e| runtime(format!("{path} could not be read ({e}).")))?;
            lua.load(src)
                .set_name(format!("@{path}"))
                .set_mode(mlua::chunk::ChunkMode::Text)
                .call(())
        })?,
    )?;
    // Lifecycle plumbing for the host (not part of the documented API).
    p.set(
        "__phase",
        lua.create_function(|lua, name: String| {
            let phase = match name.as_str() {
                "load" => Phase::Load,
                "register" => Phase::Register,
                "ready" => Phase::Ready,
                _ => return Err(runtime("unknown phase")),
            };
            if let Some(mut s) = lua.app_data_mut::<PluginSlot>() {
                s.phase = phase;
                if phase == Phase::Ready {
                    s.extensibility_ready = true;
                }
            }
            Ok(())
        })?,
    )?;
    p.set(
        "__run",
        lua.create_function(
            |lua, (id, raw): (String, Option<Table>)| -> mlua::Result<Value> {
                invoke(lua, &id, raw, None)
            },
        )?,
    )?;
    super::custom::install(lua, &p)?;
    super::reactive::install(lua, &p)?;
    super::response::install(lua, &p)?;
    super::services::install(lua, &p)?;
    super::formats::install(lua, &p)?;
    plum.set("plugin", p)?;
    Ok(())
}

/// What `plum.ui.notify` posted in the current run (app data).
#[derive(Default)]
pub(super) struct Notices(pub Vec<(String, NoticeKind)>);

/// The run's notifications (taken).
pub(super) fn take_notices(lua: &Lua) -> Vec<(String, NoticeKind)> {
    lua.app_data_mut::<Notices>()
        .map(|mut n| std::mem::take(&mut n.0))
        .unwrap_or_default()
}

/// The declarations, when they changed since the last call.
pub(super) fn take_decls(lua: &Lua) -> Option<Decls> {
    let mut s = lua.app_data_mut::<PluginSlot>()?;
    if !s.changed {
        return None;
    }
    s.changed = false;
    Some(s.decls.clone())
}

pub(super) fn invoke(
    lua: &Lua,
    id: &str,
    raw: Option<Table>,
    extra: Option<Table>,
) -> mlua::Result<Value> {
    let decl = lua
        .app_data_ref::<PluginSlot>()
        .and_then(|s| s.decls.commands.iter().find(|c| c.id == id).cloned());
    let fns: Table = lua.named_registry_value(COMMANDS)?;
    let f: Function = match fns.get::<Value>(id)? {
        Value::Function(f) => f,
        _ => {
            return Err(runtime(format!(
                "The command \u{201C}{id}\u{201D} is not registered by this plugin."
            )));
        }
    };
    let ctx = lua.create_table()?;
    if let Some(d) = decl
        && !d.params.is_empty()
    {
        let raw = match raw {
            Some(t) => t,
            None => lua.create_table()?,
        };
        ctx.set(
            "params",
            super::custom::params_to_ctx(lua, &d.params, &raw)?,
        )?;
    }
    ctx.set("command", id)?;
    if let Some(extra) = extra {
        for pair in extra.pairs::<String, Value>() {
            let (k, v) = pair?;
            ctx.set(k, v)?;
        }
    }
    f.call::<Value>(ctx)
}
