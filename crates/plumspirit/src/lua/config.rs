//! Plugin settings and presets (B1-0035, `docs/LUA_API.md` "Settings and
//! presets"): namespaced per plugin, in two scopes.
//!
//! * **User** (`plum.config`, `plum.presets`): files in the plugin's data
//!   folder (`<settings>\plugin-data\<id>\config.json`, `presets\<name>.json`),
//!   written atomically (temporary file, then rename) at the end of the run
//!   that changed them.
//! * **Project** (`plum.config.project`): saved inside the open project
//!   (`.lup` section `PLUG`); the run hands the change to the app, which
//!   stores it outside the undo history (it marks the project changed, like
//!   Mute / Solo).
//!
//! Stored data is `{"version": n, "values": {…}}`. The manifest's
//! `config_version` is the plugin's current layout; older data is passed to
//! the plugin's `migrate_config(old_version, values)` (when it has one) and
//! saved again with the new version; data written by a newer version of the
//! plugin is left untouched and not used. A damaged file is set aside
//! (`config.json.damaged-<time>`) and the plugin starts empty — each of these
//! says so in the plugin's log.

use super::{Category, stop};
use mlua::{Lua, Table, Value};
use serde_json::{Map, Value as Json};
use std::path::{Path, PathBuf};

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// Largest stored data of one scope (serialized).
pub const MAX_BYTES: usize = 1024 * 1024;
/// Deepest nesting of a stored value.
pub const MAX_DEPTH: usize = 16;
/// Most presets of one plugin.
pub const MAX_PRESETS: usize = 1000;

/// One scope's data in a Lua state.
#[derive(Default)]
struct Scope {
    loaded: bool,
    values: Map<String, Json>,
    dirty: bool,
}

/// The settings state of one plugin's Lua state (app data).
pub(super) struct ConfigSlot {
    /// `<settings>\plugin-data\<id>` (None: no settings folder: values live
    /// for the session only).
    dir: Option<PathBuf>,
    plugin: String,
    /// The plugin's current layout (manifest `config_version`).
    version: u64,
    user: Scope,
    project: Scope,
    /// Lines for the plugin's log.
    pub notes: Vec<String>,
}

impl ConfigSlot {
    pub fn new(plugin: &str, dir: Option<PathBuf>, version: u64) -> ConfigSlot {
        ConfigSlot {
            dir,
            plugin: plugin.to_owned(),
            version: version.max(1),
            user: Scope::default(),
            project: Scope::default(),
            notes: Vec::new(),
        }
    }
}

/// A Lua value as JSON (nil, booleans, integers, finite numbers, texts,
/// arrays 1..n, tables with text keys), or why it cannot be stored.
pub fn to_json(v: &Value, depth: usize) -> Result<Json, String> {
    if depth > MAX_DEPTH {
        return Err(format!("values nest deeper than {MAX_DEPTH} levels"));
    }
    Ok(match v {
        Value::Nil => Json::Null,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(n) => Json::from(*n),
        Value::Number(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .ok_or("numbers must be finite")?,
        Value::String(s) => Json::String(
            s.to_str()
                .map_err(|_| "texts must be UTF-8".to_owned())?
                .to_string(),
        ),
        Value::Table(t) => {
            let n = t.raw_len();
            let mut count = 0usize;
            for pair in t.clone().pairs::<Value, Value>() {
                let _ = pair.map_err(|e| e.to_string())?;
                count += 1;
            }
            if n > 0 && count == n {
                let mut a = Vec::with_capacity(n);
                for i in 1..=n {
                    a.push(to_json(
                        &t.raw_get::<Value>(i).map_err(|e| e.to_string())?,
                        depth + 1,
                    )?);
                }
                Json::Array(a)
            } else {
                let mut m = Map::new();
                for pair in t.clone().pairs::<Value, Value>() {
                    let (k, v) = pair.map_err(|e| e.to_string())?;
                    let Value::String(k) = k else {
                        return Err("a table to store has text keys (or is a list 1..n)".into());
                    };
                    let k = k
                        .to_str()
                        .map_err(|_| "keys must be UTF-8".to_owned())?
                        .to_string();
                    m.insert(k, to_json(&v, depth + 1)?);
                }
                Json::Object(m)
            }
        }
        other => return Err(format!("a {} cannot be stored", other.type_name())),
    })
}

/// JSON as a Lua value (integers stay integers).
pub fn from_json(lua: &Lua, j: &Json) -> mlua::Result<Value> {
    Ok(match j {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(a) => {
            let t = lua.create_table()?;
            for (i, v) in a.iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
        Json::Object(m) => {
            let t = lua.create_table()?;
            for (k, v) in m {
                t.raw_set(k.as_str(), from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

/// A settings key / preset name: 1 – 64 characters `a-z A-Z 0-9 . _ -`
/// and spaces (presets), no leading dot.
fn check_key(k: &str, what: &str) -> mlua::Result<()> {
    let ok = !k.is_empty()
        && k.len() <= 64
        && !k.starts_with('.')
        && !k.ends_with([' ', '.'])
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '));
    if ok {
        Ok(())
    } else {
        Err(runtime(format!(
            "The {what} \u{201C}{k}\u{201D} must be 1 – 64 characters of letters, digits, \u{201C}.\u{201D}, \u{201C}_\u{201D}, \u{201C}-\u{201D} or spaces."
        )))
    }
}

/// Writes `text` to `path` through a temporary file (never half a file).
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// A preset name: a settings key that is also a file name on every system
/// (no `con`, `nul`, …).
fn check_preset(name: &str) -> mlua::Result<()> {
    check_key(name, "preset name")?;
    super::package::safe_component(&format!("{name}.json")).map_err(|_| {
        runtime(format!(
            "The preset name \u{201C}{name}\u{201D} is reserved by Windows; choose another."
        ))
    })
}

/// `{"version", "values"}` from stored text, or why it is not usable.
fn parse_stored(text: &str) -> Result<(u64, Map<String, Json>), String> {
    let j: Json = serde_json::from_str(text).map_err(|e| format!("not valid JSON ({e})"))?;
    let version = j
        .get("version")
        .and_then(Json::as_u64)
        .ok_or("no version")?;
    let values = match j.get("values") {
        Some(Json::Object(m)) => m.clone(),
        _ => return Err("no values".into()),
    };
    Ok((version, values))
}

fn stored_text(version: u64, values: &Map<String, Json>) -> String {
    serde_json::to_string(&serde_json::json!({ "version": version, "values": values }))
        .unwrap_or_else(|_| "{}".into())
}

/// Older data through the plugin's `migrate_config(old, values)` (when it
/// has one); the result must be a table.
fn migrate(lua: &Lua, old: u64, values: Map<String, Json>) -> Result<Map<String, Json>, String> {
    let f = match lua.globals().get::<Value>("migrate_config") {
        Ok(Value::Function(f)) => f,
        _ => return Ok(values),
    };
    let arg = from_json(lua, &Json::Object(values)).map_err(|e| e.to_string())?;
    let r: Value = f
        .call((i64::try_from(old).unwrap_or(i64::MAX), arg))
        .map_err(|e| super::error_text(&e))?;
    match to_json(&r, 0)? {
        Json::Object(m) => Ok(m),
        Json::Array(a) if a.is_empty() => Ok(Map::new()),
        _ => Err("migrate_config must return a table with text keys".into()),
    }
}

/// Loads the user scope once per Lua state.
fn ensure_user(lua: &Lua) -> mlua::Result<()> {
    let (dir, version, loaded) = {
        let s = lua
            .app_data_ref::<ConfigSlot>()
            .ok_or_else(|| runtime("Settings are available to plugins only."))?;
        (s.dir.clone(), s.version, s.user.loaded)
    };
    if loaded {
        return Ok(());
    }
    let mut notes = Vec::new();
    let mut values = Map::new();
    let mut dirty = false;
    if let Some(d) = &dir {
        let path = d.join("config.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            match parse_stored(&text) {
                Ok((v, m)) if v == version => values = m,
                Ok((v, m)) if v < version => match migrate(lua, v, m) {
                    Ok(m) => {
                        values = m;
                        dirty = true;
                        notes.push(format!("Settings migrated from layout {v} to {version}."));
                    }
                    Err(e) => notes.push(format!(
                        "migrate_config failed ({e}); the settings of layout {v} are kept in the file but not used this time."
                    )),
                },
                Ok((v, _)) => notes.push(format!(
                    "The settings were saved by a newer version of the plugin (layout {v}); they are kept but not used."
                )),
                Err(why) => {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let aside = d.join(format!("config.json.damaged-{secs}"));
                    let _ = std::fs::rename(&path, &aside);
                    notes.push(format!(
                        "The settings file was damaged ({why}); it was set aside as {} and the plugin starts with empty settings.",
                        aside.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    ));
                }
            }
        }
    }
    let mut s = lua
        .app_data_mut::<ConfigSlot>()
        .ok_or_else(|| runtime("Settings are available to plugins only."))?;
    s.user = Scope {
        loaded: true,
        values,
        dirty,
    };
    s.notes.extend(notes);
    Ok(())
}

/// Loads the project scope from the current world once per run.
fn ensure_project(lua: &Lua) -> mlua::Result<()> {
    let (plugin, version, loaded) = {
        let s = lua
            .app_data_ref::<ConfigSlot>()
            .ok_or_else(|| runtime("Settings are available to plugins only."))?;
        (s.plugin.clone(), s.version, s.project.loaded)
    };
    if loaded {
        return Ok(());
    }
    let w = super::handles::current_world(lua)
        .ok_or_else(|| runtime("No project is open, so there are no project settings."))?;
    let stored = w
        .doc
        .project()
        .plugin_data
        .iter()
        .find(|(id, _)| *id == plugin)
        .map(|(_, t)| t.clone());
    let mut notes = Vec::new();
    let mut values = Map::new();
    let mut dirty = false;
    if let Some(text) = stored {
        match parse_stored(&text) {
            Ok((v, m)) if v == version => values = m,
            Ok((v, m)) if v < version => match migrate(lua, v, m) {
                Ok(m) => {
                    values = m;
                    dirty = true;
                }
                Err(e) => notes.push(format!("migrate_config failed for the project settings ({e}).")),
            },
            Ok((v, _)) => notes.push(format!(
                "The project's settings were saved by a newer version of the plugin (layout {v}); they are kept but not used."
            )),
            Err(why) => notes.push(format!(
                "The project's settings of this plugin are damaged ({why}); it starts with empty ones (the old data stays until they are changed)."
            )),
        }
    }
    let mut s = lua
        .app_data_mut::<ConfigSlot>()
        .ok_or_else(|| runtime("Settings are available to plugins only."))?;
    s.project = Scope {
        loaded: true,
        values,
        dirty,
    };
    s.notes.extend(notes);
    Ok(())
}

/// Validated JSON of a value to store, within the size limit.
fn storable(v: &Value) -> mlua::Result<Json> {
    to_json(v, 0).map_err(|why| runtime(format!("That value cannot be saved: {why}.")))
}

fn scope_fns(lua: &Lua, project: bool) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let ensure = move |lua: &Lua| {
        if project {
            ensure_project(lua)
        } else {
            ensure_user(lua)
        }
    };
    t.set(
        "get",
        lua.create_function(move |lua, (key, default): (String, Value)| {
            ensure(lua)?;
            let s = lua.app_data_ref::<ConfigSlot>().expect("ensured");
            let scope = if project { &s.project } else { &s.user };
            match scope.values.get(&key) {
                Some(j) => {
                    let j = j.clone();
                    drop(s);
                    from_json(lua, &j)
                }
                None => Ok(default),
            }
        })?,
    )?;
    t.set(
        "set",
        lua.create_function(move |lua, (key, value): (String, Value)| {
            check_key(&key, "settings key")?;
            ensure(lua)?;
            let j = storable(&value)?;
            let mut s = lua.app_data_mut::<ConfigSlot>().expect("ensured");
            let scope = if project { &mut s.project } else { &mut s.user };
            if j.is_null() {
                scope.values.remove(&key);
            } else {
                scope.values.insert(key, j);
            }
            let size = serde_json::to_string(&scope.values).map_or(usize::MAX, |t| t.len());
            if size > MAX_BYTES {
                return Err(runtime(format!(
                    "The settings would be larger than {} KB; nothing was saved.",
                    MAX_BYTES / 1024
                )));
            }
            scope.dirty = true;
            Ok(())
        })?,
    )?;
    t.set(
        "remove",
        lua.create_function(move |lua, key: String| {
            ensure(lua)?;
            let mut s = lua.app_data_mut::<ConfigSlot>().expect("ensured");
            let scope = if project { &mut s.project } else { &mut s.user };
            if scope.values.remove(&key).is_some() {
                scope.dirty = true;
            }
            Ok(())
        })?,
    )?;
    t.set(
        "keys",
        lua.create_function(move |lua, ()| {
            ensure(lua)?;
            let keys: Vec<String> = {
                let s = lua.app_data_ref::<ConfigSlot>().expect("ensured");
                let scope = if project { &s.project } else { &s.user };
                scope.values.keys().cloned().collect()
            };
            lua.create_sequence_from(keys)
        })?,
    )?;
    Ok(t)
}

/// The preset folder (`None`: no settings folder).
fn preset_dir(lua: &Lua) -> mlua::Result<PathBuf> {
    lua.app_data_ref::<ConfigSlot>()
        .and_then(|s| s.dir.clone())
        .map(|d| d.join("presets"))
        .ok_or_else(|| runtime("Presets need PlumSpirit's settings folder, which is unavailable."))
}

/// Installs `plum.config` and `plum.presets` (plugins only).
pub(super) fn install(lua: &Lua, plum: &Table, slot: ConfigSlot) -> mlua::Result<()> {
    lua.set_app_data(slot);
    let config = scope_fns(lua, false)?;
    config.set("project", scope_fns(lua, true)?)?;
    let version = lua.app_data_ref::<ConfigSlot>().map_or(1, |s| s.version);
    config.set("version", i64::try_from(version).unwrap_or(1))?;
    plum.set("config", config)?;
    let presets = lua.create_table()?;
    presets.set(
        "save",
        lua.create_function(|lua, (name, value): (String, Value)| {
            check_preset(&name)?;
            let j = storable(&value)?;
            let dir = preset_dir(lua)?;
            let count = std::fs::read_dir(&dir).map_or(0, Iterator::count);
            let path = dir.join(format!("{name}.json"));
            if !path.exists() && count >= MAX_PRESETS {
                return Err(runtime(format!(
                    "A plugin can keep at most {MAX_PRESETS} presets."
                )));
            }
            let version = lua.app_data_ref::<ConfigSlot>().map_or(1, |s| s.version);
            let text =
                serde_json::to_string(&serde_json::json!({ "version": version, "value": j }))
                    .map_err(|e| runtime(e.to_string()))?;
            if text.len() > MAX_BYTES {
                return Err(runtime("That preset is too large to save."));
            }
            write_atomic(&path, &text)
                .map_err(|e| runtime(format!("The preset could not be saved ({e}).")))
        })?,
    )?;
    presets.set(
        "load",
        lua.create_function(|lua, name: String| {
            check_preset(&name)?;
            let path = preset_dir(lua)?.join(format!("{name}.json"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                return Ok(Value::Nil);
            };
            let j: Json = serde_json::from_str(&text)
                .map_err(|_| runtime(format!("The preset \u{201C}{name}\u{201D} is damaged.")))?;
            from_json(lua, j.get("value").unwrap_or(&Json::Null))
        })?,
    )?;
    presets.set(
        "list",
        lua.create_function(|lua, ()| {
            let mut names: Vec<String> = std::fs::read_dir(preset_dir(lua)?)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| {
                            e.file_name()
                                .to_str()
                                .and_then(|n| n.strip_suffix(".json"))
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            lua.create_sequence_from(names)
        })?,
    )?;
    presets.set(
        "delete",
        lua.create_function(|lua, name: String| {
            check_preset(&name)?;
            let path = preset_dir(lua)?.join(format!("{name}.json"));
            Ok(std::fs::remove_file(path).is_ok())
        })?,
    )?;
    plum.set("presets", presets)?;
    Ok(())
}

/// After a run: writes changed user settings, hands a project change over,
/// resets the project scope (each run reads the project afresh). `ok`:
/// the run succeeded (a failed run saves nothing).
pub(super) fn end_run(lua: &Lua, ok: bool) -> (Option<String>, Vec<String>) {
    let Some(mut s) = lua.app_data_mut::<ConfigSlot>() else {
        return (None, Vec::new());
    };
    let mut notes = std::mem::take(&mut s.notes);
    let version = s.version;
    if ok && s.user.dirty {
        s.user.dirty = false;
        if let Some(d) = &s.dir {
            let text = stored_text(version, &s.user.values);
            if let Err(e) = write_atomic(&d.join("config.json"), &text) {
                notes.push(format!("The settings could not be saved ({e})."));
            }
        }
    } else if !ok && s.user.dirty {
        // A failed run's changes are dropped: read the file again next time.
        s.user = Scope::default();
    }
    let project = if ok && s.project.dirty {
        Some(if s.project.values.is_empty() {
            String::new()
        } else {
            stored_text(version, &s.project.values)
        })
    } else {
        None
    };
    s.project = Scope::default();
    (project, notes)
}
