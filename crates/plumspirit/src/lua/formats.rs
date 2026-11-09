//! Registered author codecs. Native jobs own input selection, import transactions
//! and deferred export publication; no Lua callback executes on the UI thread.
use super::plugin_api::{PluginSlot, check_decl_id, require_loading};
use super::{Capability, Category, Host, stop};
use mlua::{AnyUserData, Function, Lua, ObjectLike, Table, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

pub const MAX_FORMATS: usize = 32;
const HANDLERS: &str = "plumspirit.plugin.formats";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Import,
    Export,
}
impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Export => "export",
        }
    }
    fn allowed(self, c: Capability) -> bool {
        // Plain-data services retain the caller's narrowed authority. Providers
        // stay noninteractive/pure under the existing native broker; no file or
        // transaction handle crosses its structured payload boundary.
        matches!(
            c,
            Capability::ReadProject | Capability::ListFonts | Capability::CallServices
        ) || match self {
            Self::Import => matches!(
                c,
                Capability::ReadUserFile
                    | Capability::EditNotes
                    | Capability::EditTracks
                    | Capability::EditConductor
                    | Capability::EditMetadata
            ),
            Self::Export => c == Capability::WriteUserFile,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatDecl {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub import: bool,
    pub export: bool,
}
impl FormatDecl {
    pub fn supports(&self, mode: Mode) -> bool {
        match mode {
            Mode::Import => self.import,
            Mode::Export => self.export,
        }
    }
}
#[derive(Clone)]
pub(super) struct Request {
    pub id: String,
    pub mode: Mode,
}
#[derive(Default)]
struct Scope {
    request: Option<Request>,
    dispatched: bool,
    opening_tx: bool,
    output: Option<AnyUserData>,
    grants: Option<Arc<BTreeSet<Capability>>>,
}
fn bad(s: &str) -> mlua::Error {
    stop(Category::Runtime, s)
}
pub(super) fn active(lua: &Lua) -> bool {
    lua.app_data_ref::<Scope>()
        .is_some_and(|s| s.request.is_some())
}
pub(super) fn check_open(lua: &Lua) -> mlua::Result<()> {
    if lua
        .app_data_ref::<Scope>()
        .is_some_and(|s| s.request.is_some() && !s.opening_tx)
    {
        return Err(stop(
            Category::Capability,
            "Format imports use only their host-owned ctx.tx; exports cannot open musical transactions.",
        ));
    }
    Ok(())
}
pub(super) fn check_write(lua: &Lua) -> mlua::Result<()> {
    if lua
        .app_data_ref::<Scope>()
        .is_some_and(|s| s.request.as_ref().is_some_and(|r| r.mode == Mode::Export))
    {
        return Err(stop(
            Category::Capability,
            "Format export reads the project without musical edits.",
        ));
    }
    Ok(())
}
pub(super) fn check_config_write(lua: &Lua) -> mlua::Result<()> {
    if active(lua) {
        return Err(stop(
            Category::Capability,
            "Format adapters read settings; change settings or presets from a separate explicit command.",
        ));
    }
    Ok(())
}
pub(super) fn enter(lua: &Lua, request: Option<Request>) {
    // Native workers call this before every job, including failed/replaced work.
    if let Some(grants) = lua
        .app_data_mut::<Scope>()
        .and_then(|mut s| s.grants.take())
    {
        lua.set_app_data(super::handles::Granted(grants));
    }
    let grants = request.as_ref().and_then(|_| {
        lua.app_data_ref::<super::handles::Granted>()
            .map(|g| Arc::clone(&g.0))
    });
    if let (Some(r), Some(g)) = (&request, &grants) {
        lua.set_app_data(super::handles::Granted(Arc::new(
            g.iter().copied().filter(|c| r.mode.allowed(*c)).collect(),
        )));
    }
    lua.set_app_data(Scope {
        request,
        grants,
        ..Default::default()
    });
}
pub(super) fn end_run(lua: &Lua, ok: bool) -> mlua::Result<()> {
    let output = lua
        .app_data_mut::<Scope>()
        .and_then(|mut s| s.output.take());
    let result = if ok {
        output.map_or(Ok(()), |file| super::files::publish_format(lua, &file))
    } else {
        Ok(())
    };
    enter(lua, None);
    result
}
fn strict(t: &Table, allowed: &[&str]) -> mlua::Result<()> {
    if t.metatable().is_some() {
        return Err(bad("Format declarations must be plain tables."));
    }
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        if !matches!(k,Value::String(s)if s.as_bytes().len()<=32 && allowed.contains(&s.to_str()?.as_ref()))
        {
            return Err(bad("Unknown format declaration field."));
        }
    }
    Ok(())
}
fn declaration(t: &Table) -> mlua::Result<(FormatDecl, Table)> {
    strict(t, &["id", "name", "extensions", "import", "export"])?;
    let id: Value = t.raw_get("id")?;
    let Value::String(id) = id else {
        return Err(bad("Format id must be a string."));
    };
    if id.as_bytes().len() > 64 {
        return Err(bad("Format id exceeds64ASCII bytes."));
    }
    let id = id.to_str()?.to_string();
    check_decl_id(&id, "format")?;
    let Value::String(name) = t.raw_get("name")? else {
        return Err(bad("Format name must be a string."));
    };
    if name.as_bytes().len() > 256 {
        return Err(bad("Format name exceeds256UTF8 bytes."));
    }
    let name = name.to_str()?;
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(bad("Format name needs1–64printable Unicode scalars."));
    }
    let name = name.to_string();
    let extensions: Table = t.raw_get("extensions")?;
    if extensions.metatable().is_some() || !(1..=16).contains(&extensions.raw_len()) {
        return Err(bad(
            "Format extensions need a plain dense array of1–16extensions.",
        ));
    }
    let mut count = 0;
    for p in extensions.clone().pairs::<Value, Value>() {
        let (key, _) = p?;
        count += 1;
        if !matches!(key,Value::Integer(n)if n>=1 && n<=extensions.raw_len() as i64) {
            return Err(bad("Format extensions must be a dense array."));
        }
    }
    if count != extensions.raw_len() {
        return Err(bad("Format extensions must be a dense array."));
    }
    let mut names = Vec::new();
    for value in extensions.sequence_values::<Value>() {
        let Value::String(s) = value? else {
            return Err(bad("Format extensions must be strings."));
        };
        let bytes = s.as_bytes();
        if bytes.is_empty() || bytes.len() > 16 || !bytes.iter().all(u8::is_ascii_alphanumeric) {
            return Err(bad(
                "Format extensions must contain1–16ASCII letters/digits, without dots or paths.",
            ));
        }
        let s = s.to_str()?.to_ascii_lowercase();
        if ["lup", "mid", "midi", "rmi"].contains(&s.as_str()) {
            return Err(bad(
                "Native LUP/MIDI extensions cannot be registered by plugins.",
            ));
        }
        if names.contains(&s) {
            return Err(bad("Format extensions must be unique ignoring case."));
        }
        names.push(s);
    }
    let handlers = t.clone();
    let callback = |key| -> mlua::Result<bool> {
        match t.raw_get::<Value>(key)? {
            Value::Nil => Ok(false),
            Value::Function(_) => Ok(true),
            _ => Err(bad(
                "Format import/export fields must be functions or absent.",
            )),
        }
    };
    let import = callback("import")?;
    let export = callback("export")?;
    if !import && !export {
        return Err(bad("A format needs an import or export function."));
    }
    Ok((
        FormatDecl {
            id,
            name,
            extensions: names,
            import,
            export,
        },
        handlers,
    ))
}
fn invoke(lua: &Lua) -> mlua::Result<bool> {
    super::notes::check_budget(lua)?;
    let request = {
        let mut s = lua
            .app_data_mut::<Scope>()
            .ok_or_else(|| bad("No native format job is active."))?;
        if s.dispatched {
            return Err(bad("A native format job cannot dispatch twice."));
        }
        let r = s.request.clone().ok_or_else(|| {
            stop(
                Category::Capability,
                "Only a native user-invoked format job can dispatch an adapter.",
            )
        })?;
        s.dispatched = true;
        r
    };
    let lease = lua
        .app_data_ref::<super::bridge::Session>()
        .and_then(|s| s.lease.clone())
        .ok_or_else(|| bad("No native format job lease."))?;
    lease.check().map_err(|e| e.lua())?;
    if !lease.user_invoked() {
        return Err(stop(
            Category::Capability,
            "Format selection requires explicit user invocation.",
        ));
    }
    let decl = lua
        .app_data_ref::<PluginSlot>()
        .and_then(|s| {
            s.decls
                .formats
                .iter()
                .find(|d| d.id == request.id && d.supports(request.mode))
                .cloned()
        })
        .ok_or_else(|| bad("The requested format operation is not registered."))?;
    let granted = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, Capability::ReadProject)?;
    Host::require(
        &granted,
        if request.mode == Mode::Import {
            Capability::ReadUserFile
        } else {
            Capability::WriteUserFile
        },
    )?;
    if request.mode == Mode::Import {
        Host::require(&granted, Capability::EditNotes)?;
    }
    let world = super::handles::current_world(lua)
        .ok_or_else(|| bad("Open a project before invoking a format adapter."))?;
    let file = super::files::choose_format(
        lua,
        request.mode == Mode::Export,
        &decl.name,
        &decl.extensions,
    )?;
    let Some(file) = file else {
        return Ok(false);
    };
    let handlers: Table = lua.named_registry_value(HANDLERS)?;
    let handlers: Table = handlers.raw_get(decl.id.as_str())?;
    let callback: Function = handlers.raw_get(request.mode.name())?;
    let ctx = lua.create_table()?;
    ctx.raw_set("format", decl.id.as_str())?;
    ctx.raw_set("mode", request.mode.name())?;
    ctx.raw_set("file", file.clone())?;
    ctx.raw_set(
        "project",
        lua.create_userdata(super::handles::LuaHandle(world.project()))?,
    )?;
    ctx.raw_set("keys", u16::from(world.doc.project().keys.max_pitch()) + 1)?;
    let tx = if request.mode == Mode::Import {
        lua.app_data_mut::<Scope>().unwrap().opening_tx = true;
        let opened = super::tx::open_owned(lua, &decl.name);
        lua.app_data_mut::<Scope>().unwrap().opening_tx = false;
        let tx = opened?;
        ctx.raw_set("tx", tx.clone())?;
        Some(tx)
    } else {
        None
    };
    callback.call::<()>(ctx)?;
    super::notes::check_budget(lua)?;
    lease.check().map_err(|e| e.lua())?;
    if let Some(tx) = tx {
        if tx.call_method::<bool>("is_open", ())? {
            tx.call_method::<()>("commit", ())?;
        }
    } else {
        lua.app_data_mut::<Scope>().unwrap().output = Some(file);
    }
    Ok(true)
}
pub(super) fn install(lua: &Lua, plugin: &Table) -> mlua::Result<()> {
    lua.set_app_data(Scope::default());
    lua.set_named_registry_value(HANDLERS, lua.create_table()?)?;
    plugin.set("max_formats", MAX_FORMATS)?;
    plugin.set(
        "format",
        lua.create_function(|lua, t: Table| {
            require_loading(lua, "A format adapter")?;
            let (decl, handlers) = declaration(&t)?;
            let mut slot = lua
                .app_data_mut::<PluginSlot>()
                .ok_or_else(|| bad("Formats belong to plugins."))?;
            if slot.decls.formats.len() == MAX_FORMATS {
                return Err(stop(
                    Category::Budget,
                    "At most32formats may be registered per plugin.",
                ));
            }
            if slot.decls.formats.iter().any(|d| d.id == decl.id) {
                return Err(bad("This format id is already registered."));
            }
            let id = decl.id.clone();
            slot.decls.formats.try_reserve(1).map_err(|_| {
                stop(
                    Category::Memory,
                    "Cannot reserve bounded format declaration.",
                )
            })?;
            drop(slot);
            let table: Table = lua.named_registry_value(HANDLERS)?;
            // Copy just the functions; later author mutation of its declaration has no effect.
            let callbacks = lua.create_table()?;
            for key in ["import", "export"] {
                callbacks.raw_set(key, handlers.raw_get::<Value>(key)?)?;
            }
            table.raw_set(id, callbacks)?;
            let mut slot = lua.app_data_mut::<PluginSlot>().unwrap();
            slot.decls.formats.push(decl);
            slot.changed = true;
            Ok(())
        })?,
    )?;
    plugin.set("__run_format", lua.create_function(|lua, ()| invoke(lua))?)
}

#[cfg(test)]
#[path = "formats_tests.rs"]
mod tests;
