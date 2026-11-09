//! Bounded worker-owned MIDI preparation. No output authority or audio-thread Lua.
use crate::lua::bridge::{JobLease, NativeBudget, Reservation, Session};
use crate::lua::{Category, notes, stop, ticks};
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use plumspirit_engine::model::events::channel_data_len;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

pub const MAX_BATCH: usize = 4096;
pub use plumspirit_engine::playback::midi_plan::{Event, MAX_EVENTS};
pub const MAX_JOB_EVENTS: usize = 1_048_576;
pub const MAX_RESOURCES: usize = 32;
const METADATA_BYTES: usize = 512;

struct Stored {
    events: Vec<Event>,
    capacity: usize,
    finished: bool,
    _allocation: Reservation,
}
struct Resource {
    lease: Arc<JobLease>,
    stored: RefCell<Option<Stored>>,
    _metadata: Reservation,
}
struct Writer(Rc<Resource>);
struct Plan(Rc<Resource>);

#[derive(Default)]
pub(crate) struct Slot {
    resources: Vec<Weak<Resource>>,
    reserved_events: usize,
}
pub(crate) fn end_run(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<Slot>() {
        for resource in slot.resources.drain(..).filter_map(|r| r.upgrade()) {
            resource.stored.borrow_mut().take();
        }
        slot.reserved_events = 0;
    }
}
fn bad(message: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn quota(message: &str) -> mlua::Error {
    stop(Category::Budget, message)
}
fn check(lua: &Lua, resource: &Resource) -> mlua::Result<()> {
    notes::check_budget(lua)?;
    resource.lease.check().map_err(|e| e.lua())?;
    if !lua.app_data_ref::<Session>().is_some_and(|s| {
        s.lease
            .as_ref()
            .is_some_and(|lease| Arc::ptr_eq(lease, &resource.lease))
    }) {
        return Err(stop(
            Category::Expired,
            "This MIDI plan belongs to a finished job.",
        ));
    }
    Ok(())
}
fn budget(lua: &Lua) -> mlua::Result<NativeBudget> {
    lua.app_data_ref::<NativeBudget>()
        .map(|b| b.clone())
        .ok_or_else(|| bad("Native MIDI plan accounting is unavailable."))
}
fn plain(table: &Table) -> mlua::Result<()> {
    if table.metatable().is_some() {
        return Err(bad(
            "MIDI plan data must be plain tables without metatables.",
        ));
    }
    Ok(())
}
fn fields(table: &Table, allowed: &[&str]) -> mlua::Result<()> {
    plain(table)?;
    for pair in table.pairs::<Value, Value>() {
        let (key, _) = pair?;
        let Value::String(key) = key else {
            return Err(bad("MIDI plan options require named fields."));
        };
        if !allowed.contains(&key.to_str()?.as_ref()) {
            return Err(bad("Unknown MIDI plan field."));
        }
    }
    Ok(())
}
fn integer(value: Value, max: usize, label: &str) -> mlua::Result<usize> {
    match value {
        Value::Integer(n) if n >= 0 && n as u64 <= max as u64 => Ok(n as usize),
        _ => Err(bad(format!("{label} must be an integer from 0 to {max}."))),
    }
}
fn dense(lua: &Lua, table: &Table, count: usize) -> mlua::Result<()> {
    plain(table)?;
    if table.raw_len() != count {
        return Err(bad("MIDI plan columns must have equal dense lengths."));
    }
    for (i, pair) in table.pairs::<Value, Value>().enumerate() {
        if i.is_multiple_of(256) {
            notes::check_budget(lua)?;
        }
        let (key, _) = pair?;
        if !matches!(key, Value::Integer(n) if n >= 1 && n as u64 <= count as u64) {
            return Err(bad("MIDI plan columns require only dense integer indices."));
        }
    }
    // raw_len alone does not prove an array has no holes.
    for i in 1..=count {
        if matches!(table.raw_get::<Value>(i)?, Value::Nil) {
            return Err(bad("MIDI plan columns cannot contain holes."));
        }
    }
    Ok(())
}
fn append(lua: &Lua, resource: &Resource, columns: Table) -> mlua::Result<usize> {
    check(lua, resource)?;
    fields(&columns, &["tick", "status", "data1", "data2"])?;
    let tick: Table = columns.raw_get("tick")?;
    let status: Table = columns.raw_get("status")?;
    let data1: Table = columns.raw_get("data1")?;
    let data2: Table = columns.raw_get("data2")?;
    let count = tick.raw_len();
    if count == 0 || count > MAX_BATCH {
        return Err(quota("Append between 1 and 4096 MIDI events per batch."));
    }
    for column in [&tick, &status, &data1, &data2] {
        dense(lua, column, count)?;
    }
    let mut stored = resource.stored.borrow_mut();
    let stored = stored
        .as_mut()
        .ok_or_else(|| bad("This MIDI writer is closed."))?;
    if stored.finished {
        return Err(bad("This MIDI writer is already finished."));
    }
    if count > stored.capacity - stored.events.len() {
        return Err(quota("This batch exceeds the MIDI writer's max_events."));
    }
    // Validate a bounded batch in accounted scratch space before changing the writer.
    let _scratch = budget(lua)?
        .reserve(count * size_of::<Event>())
        .map_err(|e| e.lua())?;
    let mut batch = Vec::new();
    batch
        .try_reserve_exact(count)
        .map_err(|_| quota("MIDI batch allocation failed."))?;
    let mut previous = stored.events.last().map(|e| e.tick);
    for i in 1..=count {
        if i.is_multiple_of(256) {
            check(lua, resource)?;
        }
        let tick = ticks::from_lua(&tick.raw_get(i)?, "MIDI event tick")?;
        if previous.is_some_and(|p| tick < p) {
            return Err(bad(
                "MIDI events must be appended in nondecreasing tick order.",
            ));
        }
        let status = integer(status.raw_get(i)?, 255, "MIDI status")? as u8;
        let data1 = integer(data1.raw_get(i)?, 127, "MIDI data1")? as u8;
        let data2 = integer(data2.raw_get(i)?, 127, "MIDI data2")? as u8;
        if !(0x80..=0xEF).contains(&status) {
            return Err(bad(
                "Only channel-voice MIDI statuses 0x80 through 0xEF are supported.",
            ));
        }
        if channel_data_len(status) == 1 && data2 != 0 {
            return Err(bad("Program change and channel pressure require data2=0."));
        }
        if status & 0xF0 == 0xB0 && data1 >= 120 {
            return Err(bad(
                "Channel-mode controllers 120 through 127 are not supported.",
            ));
        }
        batch.push(Event {
            tick,
            status,
            data1,
            data2,
        });
        previous = Some(tick);
    }
    check(lua, resource)?;
    // Capacity was reserved at creation. This bounded, infallible copy is the commit.
    stored.events.extend_from_slice(&batch);
    Ok(stored.events.len())
}
fn info(lua: &Lua, resource: &Resource, finished: bool) -> mlua::Result<Table> {
    check(lua, resource)?;
    let stored = resource.stored.borrow();
    let stored = stored
        .as_ref()
        .filter(|s| s.finished == finished)
        .ok_or_else(|| bad("This MIDI plan or writer is closed."))?;
    let result = lua.create_table()?;
    result.set("count", stored.events.len())?;
    result.set("max_events", stored.capacity)?;
    result.set("finished", finished)?;
    result.set(
        "first_tick",
        stored
            .events
            .first()
            .map(|e| ticks::to_lua(e.tick))
            .transpose()?,
    )?;
    result.set(
        "last_tick",
        stored
            .events
            .last()
            .map(|e| ticks::to_lua(e.tick))
            .transpose()?,
    )?;
    Ok(result)
}
impl UserData for Writer {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("append", |lua, writer, columns: Table| {
            append(lua, &writer.0, columns)
        });
        methods.add_method("info", |lua, writer, ()| info(lua, &writer.0, false));
        methods.add_method("finish", |lua, writer, ()| {
            check(lua, &writer.0)?;
            let mut stored = writer.0.stored.borrow_mut();
            let stored = stored
                .as_mut()
                .filter(|s| !s.finished)
                .ok_or_else(|| bad("This MIDI writer is closed or already finished."))?;
            stored.finished = true;
            Ok(Plan(Rc::clone(&writer.0)))
        });
        methods.add_method("cancel", |lua, writer, ()| {
            check(lua, &writer.0)?;
            let mut stored = writer.0.stored.borrow_mut();
            if stored.as_ref().is_some_and(|s| s.finished) {
                return Err(bad("The writer is finished; close its MIDI plan instead."));
            }
            stored.take();
            Ok(())
        });
    }
}
impl UserData for Plan {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("stage", |lua, plan, options: Table| {
            check(lua, &plan.0)?;
            let stored = plan.0.stored.borrow();
            let stored = stored
                .as_ref()
                .filter(|s| s.finished)
                .ok_or_else(|| bad("This MIDI plan is closed."))?;
            super::midi::stage(lua, &stored.events, options)
        });
        methods.add_method("info", |lua, plan, ()| info(lua, &plan.0, true));
        methods.add_method("read", |lua, plan, options: Option<Table>| {
            check(lua, &plan.0)?;
            let options = options.unwrap_or(lua.create_table()?);
            fields(&options, &["offset", "limit"])?;
            let stored = plan.0.stored.borrow();
            let stored = stored
                .as_ref()
                .filter(|s| s.finished)
                .ok_or_else(|| bad("This MIDI plan is closed."))?;
            let offset = match options.raw_get("offset")? {
                Value::Nil => 0,
                v => integer(v, stored.events.len(), "Page offset")?,
            };
            let limit = match options.raw_get("limit")? {
                Value::Nil => MAX_BATCH,
                v => integer(v, MAX_BATCH, "Page limit")?,
            };
            if limit == 0 {
                return Err(bad("Page limit must be at least 1."));
            }
            let end = (offset + limit).min(stored.events.len());
            let result = lua.create_table()?;
            let tick = lua.create_table_with_capacity(end - offset, 0)?;
            let status = lua.create_table_with_capacity(end - offset, 0)?;
            let data1 = lua.create_table_with_capacity(end - offset, 0)?;
            let data2 = lua.create_table_with_capacity(end - offset, 0)?;
            for (i, event) in stored.events[offset..end].iter().enumerate() {
                if i.is_multiple_of(256) {
                    check(lua, &plan.0)?;
                }
                tick.raw_set(i + 1, ticks::to_lua(event.tick)?)?;
                status.raw_set(i + 1, event.status)?;
                data1.raw_set(i + 1, event.data1)?;
                data2.raw_set(i + 1, event.data2)?;
            }
            result.set("tick", tick)?;
            result.set("status", status)?;
            result.set("data1", data1)?;
            result.set("data2", data2)?;
            result.set("offset", offset)?;
            result.set("count", end - offset)?;
            result.set("total", stored.events.len())?;
            result.set("next_offset", (end < stored.events.len()).then_some(end))?;
            Ok(result)
        });
        methods.add_method("close", |lua, plan, ()| {
            check(lua, &plan.0)?;
            plan.0.stored.borrow_mut().take();
            Ok(())
        });
    }
}
pub(super) fn install(lua: &Lua, playback: &Table) -> mlua::Result<()> {
    playback.set(
        "plan_writer",
        lua.create_function(|lua, options: Table| {
            notes::check_budget(lua)?;
            fields(&options, &["max_events"])?;
            let capacity = integer(options.raw_get("max_events")?, MAX_EVENTS, "max_events")?;
            if capacity == 0 {
                return Err(bad("max_events must be at least 1."));
            }
            let lease = lua
                .app_data_ref::<Session>()
                .and_then(|s| s.lease.clone())
                .ok_or_else(|| stop(Category::Expired, "No MIDI plan job is active."))?;
            lease.check().map_err(|e| e.lua())?;
            {
                let slot = lua
                    .app_data_ref::<Slot>()
                    .ok_or_else(|| bad("MIDI plans are unavailable."))?;
                if slot.resources.len() >= MAX_RESOURCES
                    || capacity > MAX_JOB_EVENTS - slot.reserved_events
                {
                    return Err(quota(
                        "This job exceeded 32 MIDI resources or 1048576 reserved events.",
                    ));
                }
            }
            let budget = budget(lua)?;
            let metadata = budget.reserve(METADATA_BYTES).map_err(|e| e.lua())?;
            let allocation = budget
                .reserve(capacity * size_of::<Event>())
                .map_err(|e| e.lua())?;
            let mut events = Vec::new();
            events
                .try_reserve_exact(capacity)
                .map_err(|_| quota("MIDI plan allocation failed."))?;
            let resource = Rc::new(Resource {
                lease,
                stored: RefCell::new(Some(Stored {
                    events,
                    capacity,
                    finished: false,
                    _allocation: allocation,
                })),
                _metadata: metadata,
            });
            let mut slot = lua
                .app_data_mut::<Slot>()
                .ok_or_else(|| bad("MIDI plans are unavailable."))?;
            slot.resources.push(Rc::downgrade(&resource));
            slot.reserved_events += capacity;
            Ok(Writer(resource))
        })?,
    )
}

#[cfg(test)]
mod tests;
