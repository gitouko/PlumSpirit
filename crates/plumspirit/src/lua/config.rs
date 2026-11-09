//! Plugin settings and presets (B1-0035, `docs/LUA_API.md` "Settings and
//! presets"): namespaced per plugin, in two scopes.
//!
//! * **User** (`plum.config`, `plum.presets`): files in the plugin's data
//!   folder (`<settings>\plugin-data\<id>\config.json`, `presets\<name>.json`),
//!   written atomically (temporary file, then rename). Configuration is published
//!   at successful run completion; explicit preset save/delete act immediately.
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
use std::io::{Read, Write};
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
const MAX_FILE_BYTES: usize = MAX_BYTES + 128; // Versioned configuration envelope.
const MAX_PRESET_ENTRIES: usize = 4096;

// Count the real JSON encoding without allocating a serialized copy. This also
// accounts for quotes, control-character escapes and structural punctuation.
struct EncodedBudget(usize);
impl Write for EncodedBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("stored data exceeds the one MiB size limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl EncodedBudget {
    fn charge(&mut self, n: usize) -> Result<(), String> {
        self.0 = self
            .0
            .checked_sub(n)
            .ok_or("stored data exceeds the one MiB size limit")?;
        Ok(())
    }
    fn string(&mut self, s: &str) -> Result<(), String> {
        if s.len() > self.0 {
            return Err("stored data exceeds the one MiB size limit".into());
        }
        serde_json::to_writer(self, s).map_err(|e| e.to_string())
    }
}

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
    bounded_json(v, depth, &mut EncodedBudget(MAX_BYTES))
}

fn bounded_json(v: &Value, depth: usize, budget: &mut EncodedBudget) -> Result<Json, String> {
    if depth > MAX_DEPTH {
        return Err(format!("values nest deeper than {MAX_DEPTH} levels"));
    }
    Ok(match v {
        Value::Nil => {
            budget.charge(4)?;
            Json::Null
        }
        Value::Boolean(b) => {
            budget.charge(if *b { 4 } else { 5 })?;
            Json::Bool(*b)
        }
        Value::Integer(n) => {
            budget.charge(n.to_string().len())?;
            Json::from(*n)
        }
        Value::Number(f) => {
            let number = serde_json::Number::from_f64(*f)
                .map(Json::Number)
                .ok_or("numbers must be finite")?;
            budget.charge(number.to_string().len())?;
            number
        }
        Value::String(s) => {
            let s = s.to_str().map_err(|_| "texts must be UTF-8".to_owned())?;
            budget.string(&s)?; // Before copying repeated/aliased Lua strings.
            Json::String(s.to_string())
        }
        Value::Table(t) => {
            let n = t.raw_len();
            let mut count = 0usize;
            for pair in t.clone().pairs::<Value, Value>() {
                let _ = pair.map_err(|e| e.to_string())?;
                count += 1;
                if count > MAX_BYTES / 2 {
                    return Err("stored data exceeds the one MiB size limit".into());
                }
            }
            budget.charge(2 + count.saturating_sub(1))?;
            if n > 0 && count == n {
                // Every remaining value requires at least one encoded byte.
                if n > budget.0 {
                    return Err("stored data exceeds the one MiB size limit".into());
                }
                let mut a = Vec::new();
                a.try_reserve_exact(n)
                    .map_err(|_| "stored data allocation failed".to_owned())?;
                for i in 1..=n {
                    a.push(bounded_json(
                        &t.raw_get::<Value>(i).map_err(|e| e.to_string())?,
                        depth + 1,
                        budget,
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
                    let k = k.to_str().map_err(|_| "keys must be UTF-8".to_owned())?;
                    budget.string(&k)?;
                    budget.charge(1)?;
                    m.insert(k.to_string(), bounded_json(&v, depth + 1, budget)?);
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
            "The {what} must be 1 – 64 characters of letters, digits, \u{201C}.\u{201D}, \u{201C}_\u{201D}, \u{201C}-\u{201D} or spaces."
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

fn read_bounded(path: &Path, limit: usize) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored data exceeds the one MiB size limit",
        ));
    }
    let mut text = String::new();
    file.take(limit as u64 + 1).read_to_string(&mut text)?;
    if text.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored data exceeds the one MiB size limit",
        ));
    }
    Ok(text)
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
    if text.len() > MAX_FILE_BYTES {
        return Err("stored data exceeds the one MiB size limit".into());
    }
    let j: Json = serde_json::from_str(text).map_err(|e| format!("not valid JSON ({e})"))?;
    let version = j
        .get("version")
        .and_then(Json::as_u64)
        .ok_or("no version")?;
    let values = match j.get("values") {
        Some(Json::Object(m)) => {
            if serde_json::to_writer(&mut EncodedBudget(MAX_BYTES), m).is_err() {
                return Err("stored data exceeds the one MiB size limit".into());
            }
            m.clone()
        }
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
    super::services::check_write(lua)?;
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
        let stored = read_bounded(&path, MAX_FILE_BYTES);
        if let Ok(text) = stored {
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
                Err(why) if why.contains("size limit") => notes.push(format!(
                    "The settings file could not be used ({why}); it is kept and the plugin starts with empty settings."
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
        } else if let Err(e) = stored
            && e.kind() == std::io::ErrorKind::InvalidData
        {
            notes.push(format!("The settings file could not be used ({e}); it is kept and the plugin starts with empty settings."));
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
    super::services::check_write(lua)?;
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
        .map(|(_, t)| t.as_str());
    let mut notes = Vec::new();
    let mut values = Map::new();
    let mut dirty = false;
    if let Some(text) = stored {
        match parse_stored(text) {
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
fn storable(lua: &Lua, v: &Value) -> mlua::Result<Json> {
    super::notes::check_budget(lua)?;
    let value =
        to_json(v, 0).map_err(|why| runtime(format!("That value cannot be saved: {why}.")))?;
    super::notes::check_budget(lua)?;
    Ok(value)
}

/// Validate the replacement against the whole scope before mutating its map.
/// Rejected updates must not accumulate native values when Lua catches the error.
fn replacement_fits(values: &Map<String, Json>, key: &str, value: &Json) -> bool {
    let mut output = EncodedBudget(MAX_BYTES);
    let mut first = true;
    let entries = values
        .iter()
        .filter(|(k, _)| k.as_str() != key)
        .map(|(k, v)| (k.as_str(), v))
        .chain((!value.is_null()).then_some((key, value)));
    if output.write_all(b"{").is_err() {
        return false;
    }
    for (k, v) in entries {
        if !first && output.write_all(b",").is_err() {
            return false;
        }
        first = false;
        if serde_json::to_writer(&mut output, k).is_err()
            || output.write_all(b":").is_err()
            || serde_json::to_writer(&mut output, v).is_err()
        {
            return false;
        }
    }
    output.write_all(b"}").is_ok()
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
            super::formats::check_config_write(lua)?;
            super::services::check_write(lua)?;
            if project {
                super::reactive::check_write(lua)?;
            }
            check_key(&key, "settings key")?;
            ensure(lua)?;
            let j = storable(lua, &value)?;
            let mut s = lua.app_data_mut::<ConfigSlot>().expect("ensured");
            let scope = if project { &mut s.project } else { &mut s.user };
            if !replacement_fits(&scope.values, &key, &j) {
                return Err(runtime(format!(
                    "The settings would be larger than {} KB; nothing was saved.",
                    MAX_BYTES / 1024
                )));
            }
            if j.is_null() {
                scope.values.remove(&key);
            } else {
                scope.values.insert(key, j);
            }
            scope.dirty = true;
            Ok(())
        })?,
    )?;
    t.set(
        "remove",
        lua.create_function(move |lua, key: String| {
            super::formats::check_config_write(lua)?;
            super::services::check_write(lua)?;
            if project {
                super::reactive::check_write(lua)?;
            }
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
    super::services::check_write(lua)?;
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
            super::formats::check_config_write(lua)?;
            super::services::check_write(lua)?;
            check_preset(&name)?;
            let j = storable(lua, &value)?;
            let dir = preset_dir(lua)?;
            let path = dir.join(format!("{name}.json"));
            let count =
                std::fs::read_dir(&dir).map_or(0, |entries| entries.take(MAX_PRESETS).count());
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
            let text = match read_bounded(&path, MAX_BYTES) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    return Err(runtime(format!("The preset could not be loaded ({e}).")));
                }
                Err(_) => return Ok(Value::Nil),
            };
            let j: Json = serde_json::from_str(&text)
                .map_err(|_| runtime(format!("The preset \u{201C}{name}\u{201D} is damaged.")))?;
            from_json(lua, j.get("value").unwrap_or(&Json::Null))
        })?,
    )?;
    presets.set(
        "list",
        lua.create_function(|lua, ()| {
            let mut names = Vec::new();
            if let Ok(entries) = std::fs::read_dir(preset_dir(lua)?) {
                for (i, entry) in entries.take(MAX_PRESET_ENTRIES + 1).enumerate() {
                    if i == MAX_PRESET_ENTRIES {
                        return Err(runtime(
                            "The preset directory exceeds the bounded listing limit.",
                        ));
                    }
                    if i % 64 == 0 {
                        super::notes::check_budget(lua)?;
                    }
                    if let Ok(entry) = entry
                        && let Some(name) = entry
                            .file_name()
                            .to_str()
                            .and_then(|n| n.strip_suffix(".json"))
                        && check_preset(name).is_ok()
                    {
                        if names.len() == MAX_PRESETS {
                            return Err(runtime(
                                "The preset directory exceeds the bounded listing limit.",
                            ));
                        }
                        names.push(name.to_owned());
                    }
                }
            }
            names.sort();
            lua.create_sequence_from(names)
        })?,
    )?;
    presets.set(
        "delete",
        lua.create_function(|lua, name: String| {
            super::services::check_write(lua)?;
            super::formats::check_config_write(lua)?;
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
