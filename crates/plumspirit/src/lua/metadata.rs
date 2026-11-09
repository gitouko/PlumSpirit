//! Sparse native metadata on Project/Track/Subtrack/permanent Group; staged by tx.
use super::*;
use crate::lua::bridge::NativeBudget;
use crate::lua::structured::Payload;
use plumspirit_engine::model::metadata::{Record, Target, Value as NativeValue};
use serde_json::Value as Json;

fn owner(lua: &Lua) -> mlua::Result<String> {
    lua.app_data_ref::<crate::lua::plugin_api::PluginSlot>()
        .map(|s| s.meta.id.clone())
        .ok_or_else(|| runtime("Metadata requires an installed plugin identity."))
}
fn native(j: &Json) -> NativeValue {
    match j {
        Json::Null => NativeValue::Null,
        Json::Bool(b) => NativeValue::Bool(*b),
        Json::Number(n) => n
            .as_i64()
            .map(NativeValue::Integer)
            .unwrap_or_else(|| NativeValue::Number(n.as_f64().unwrap().to_bits())),
        Json::String(s) => NativeValue::Text(s.clone()),
        Json::Array(a) => NativeValue::Array(a.iter().map(native).collect()),
        Json::Object(a) => {
            NativeValue::Object(a.iter().map(|(k, v)| (k.clone(), native(v))).collect())
        }
    }
}
fn lua_value(lua: &Lua, v: &NativeValue) -> mlua::Result<Value> {
    Ok(match v {
        NativeValue::Null => Value::Nil,
        NativeValue::Bool(b) => Value::Boolean(*b),
        NativeValue::Integer(n) => Value::Integer(*n),
        NativeValue::Number(n) => Value::Number(f64::from_bits(*n)),
        NativeValue::Text(s) => Value::String(lua.create_string(s)?),
        NativeValue::Array(a) => {
            let t = lua.create_table_with_capacity(a.len(), 0)?;
            for (i, v) in a.iter().enumerate() {
                t.raw_set(i + 1, lua_value(lua, v)?)?;
            }
            Value::Table(t)
        }
        NativeValue::Object(a) => {
            let t = lua.create_table_with_capacity(0, a.len())?;
            for (k, v) in a {
                t.raw_set(k.as_str(), lua_value(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}
fn target(lua: &Lua, value: &Value) -> mlua::Result<Target> {
    let h = handle_arg(value, "The metadata target")?;
    with_world(lua, &h, |_, r| match r {
        Resolved::Project(_) => Ok(Target::Project),
        Resolved::Track { track, .. } => Ok(Target::Track(track.id())),
        Resolved::Group(group) if !group.temporary => Ok(Target::Group(group.id)),
        Resolved::Group(_) => Err(runtime(
            "Temporary Groups cannot store persistent metadata.",
        )),
        _ => Err(runtime(
            "Metadata supports Project, Track, Subtrack and permanent Group handles.",
        )),
    })
}
fn get(lua: &Lua, t: Target) -> mlua::Result<(Value, Option<u32>)> {
    crate::lua::handles::require_read(lua)?;
    check_budget(lua)?;
    let id = owner(lua)?;
    read(lua, |w| match w.doc.project().metadata.get_at(&id, t) {
        Some(r) => Ok((lua_value(lua, &r.value)?, Some(r.version))),
        None => Ok((Value::Nil, None)),
    })
}
pub(super) fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let table = lua.create_table()?;
    table.set(
        "project",
        lua.create_function(|lua, ()| get(lua, Target::Project))?,
    )?;
    table.set(
        "get",
        lua.create_function(|lua, value: Value| {
            let t = target(lua, &value)?;
            get(lua, t)
        })?,
    )?;
    plum.set("metadata", table)
}
fn update(
    lua: &Lua,
    serial: u64,
    target: Target,
    record: Option<Record>,
    expected: Option<u32>,
) -> mlua::Result<()> {
    crate::lua::response::explicit_only(lua, "Custom metadata writes")?;
    require(lua, Capability::ReadProject)?;
    require(lua, Capability::EditMetadata)?;
    check_budget(lua)?;
    let id = owner(lua)?;
    {
        let mut slot = lua
            .app_data_mut::<TxSlot>()
            .ok_or_else(|| runtime("Transactions unavailable."))?;
        if !slot.open.as_ref().is_some_and(|t| t.serial == serial) {
            return Err(runtime("This transaction is closed."));
        }
        if slot.metadata_edits >= 32 {
            return Err(stop(Category::Budget, "At most32 metadata writes per job."));
        }
        slot.metadata_edits += 1;
    }
    let (table, mut guard) = read(lua, |w| {
        if let Target::Group(id) = target {
            super::groups::unlocked(&w.doc.project().groups, id)?;
        }
        let current = &w.doc.project().metadata;
        if expected.is_some_and(|v| current.get_at(&id, target).map_or(0, |r| r.version) != v) {
            return Err(runtime(
                "Metadata schema version changed; migration refused.",
            ));
        }
        // Reserve clone/history/staging overlap before creating any native copy.
        let bytes = current
            .heap_bytes()
            .saturating_mul(8)
            .saturating_add(8 * 1024 * 1024);
        let guard = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| runtime("Metadata accounting unavailable."))?
            .reserve(bytes)
            .map_err(|e| e.lua())?;
        let mut table = current.clone();
        table.set_at(&id, target, record).map_err(runtime)?;
        check_budget(lua)?;
        Ok((table, guard))
    })?;
    let retained = table.heap_bytes().saturating_mul(4).saturating_add(1024);
    apply(
        lua,
        serial,
        Step::SetMetadata {
            table: Box::new(table),
        },
        0,
    )?;
    guard.shrink_to(retained);
    let mut slot = lua
        .app_data_mut::<TxSlot>()
        .ok_or_else(|| runtime("Transactions unavailable."))?;
    slot.open
        .as_mut()
        .filter(|t| t.serial == serial)
        .ok_or_else(|| runtime("This transaction is closed."))?
        .native
        .push(guard);
    Ok(())
}
fn parse_opts(opts: &Table) -> mlua::Result<(u32, Option<u32>)> {
    if opts.metatable().is_some() {
        return Err(runtime("Metadata options must be plain."));
    }
    let version = opts.raw_get::<u32>("version")?;
    if version == 0 {
        return Err(runtime(
            "Metadata version must be a positive32-bit integer.",
        ));
    }
    Ok((version, opts.raw_get::<Option<u32>>("expect_version")?))
}
pub(super) fn add_tx_methods<M: UserDataMethods<LuaTx>>(m: &mut M) {
    m.add_method(
        "set_metadata",
        |lua, tx, (object, value, opts): (Value, Value, Table)| {
            let t = target(lua, &object)?;
            let (version, expected) = parse_opts(&opts)?;
            let data = Payload::read(lua, value)?;
            update(
                lua,
                tx.0,
                t,
                Some(Record {
                    version,
                    value: native(data.json()),
                }),
                expected,
            )
        },
    );
    m.add_method(
        "remove_metadata",
        |lua, tx, (object, opts): (Value, Option<Table>)| {
            let t = target(lua, &object)?;
            if opts.as_ref().is_some_and(|t| t.metatable().is_some()) {
                return Err(runtime("Metadata options must be plain."));
            }
            let expected = opts
                .map(|t| t.raw_get::<Option<u32>>("expect_version"))
                .transpose()?
                .flatten();
            update(lua, tx.0, t, None, expected)
        },
    );
    m.add_method(
        "set_project_metadata",
        |lua, tx, (value, opts): (Value, Table)| {
            let (version, expected) = parse_opts(&opts)?;
            let data = Payload::read(lua, value)?;
            update(
                lua,
                tx.0,
                Target::Project,
                Some(Record {
                    version,
                    value: native(data.json()),
                }),
                expected,
            )
        },
    );
    m.add_method("remove_project_metadata", |lua, tx, opts: Option<Table>| {
        if opts.as_ref().is_some_and(|t| t.metatable().is_some()) {
            return Err(runtime("Metadata options must be plain."));
        }
        let expected = opts
            .map(|t| t.raw_get::<Option<u32>>("expect_version"))
            .transpose()?
            .flatten();
        update(lua, tx.0, Target::Project, None, expected)
    });
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
