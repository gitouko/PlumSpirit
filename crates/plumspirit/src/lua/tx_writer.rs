//! Transaction-owned, bounded input staging. Lua handles never own note buffers.
use super::*;
use crate::lua::bridge::{JobLease, NativeBudget, Reservation, Session};
use std::rc::{Rc, Weak};

const MAX_BATCH: usize = 65_536;
const MAX_WRITERS: usize = 32;
const CHECK: usize = 4096;
const RECORD_BYTES: usize = std::mem::size_of::<NoteRecord>();

pub(super) struct Entry {
    state: State,
    life: Weak<()>,
    count: usize,
    bytes: usize,
    _metadata: Reservation,
}

enum State {
    Ready(Builder),
    Busy,
    Finished,
    Canceled,
    Poisoned,
}

struct Builder {
    target: Handle,
    host: NoteHost,
    revision: u64,
    channel: u8,
    records: Vec<NoteRecord>,
    allocation: Option<Reservation>,
    sorted: bool,
}

struct Writer {
    serial: u64,
    index: usize,
    lease: Arc<JobLease>,
    _life: Rc<()>,
}

fn budget(lua: &Lua) -> mlua::Result<NativeBudget> {
    lua.app_data_ref::<NativeBudget>()
        .map(|b| b.clone())
        .ok_or_else(|| runtime("Native note staging is unavailable."))
}

fn scope(lua: &Lua, lease: &Arc<JobLease>) -> mlua::Result<()> {
    lease.check().map_err(|e| e.lua())?;
    check_budget(lua)?;
    let same = lua
        .app_data_ref::<Session>()
        .is_some_and(|s| s.lease.as_ref().is_some_and(|l| Arc::ptr_eq(l, lease)));
    if !same {
        return Err(stop(
            Category::Expired,
            "This note writer belongs to a finished job.",
        ));
    }
    Ok(())
}

fn entry_mut<'a>(slot: &'a mut TxSlot, writer: &Writer) -> mlua::Result<&'a mut Entry> {
    slot.open
        .as_mut()
        .filter(|t| t.serial == writer.serial)
        .and_then(|t| t.writers.get_mut(writer.index))
        .ok_or_else(|| runtime("This note writer's transaction is closed."))
}

fn take(lua: &Lua, writer: &Writer) -> mlua::Result<Builder> {
    scope(lua, &writer.lease)?;
    require(lua, Capability::ReadProject)?;
    require(lua, Capability::EditNotes)?;
    let mut slot = lua
        .app_data_mut::<TxSlot>()
        .ok_or_else(|| runtime("Transactions are unavailable."))?;
    let entry = entry_mut(&mut slot, writer)?;
    match std::mem::replace(&mut entry.state, State::Busy) {
        State::Ready(builder) => Ok(builder),
        other => {
            entry.state = other;
            Err(runtime(
                "The note writer is finished, canceled, busy or failed; cancel a failed writer or its transaction.",
            ))
        }
    }
}

fn poison(lua: &Lua, writer: &Writer) {
    if let Some(mut slot) = lua.app_data_mut::<TxSlot>()
        && let Ok(entry) = entry_mut(&mut slot, writer)
        && matches!(entry.state, State::Busy)
    {
        entry.state = State::Poisoned;
        entry.bytes = 0;
    }
}

pub(super) fn discard_transaction(lua: &Lua, serial: u64) {
    if let Some(mut slot) = lua.app_data_mut::<TxSlot>()
        && slot.open.as_ref().is_some_and(|t| t.serial == serial)
    {
        slot.open = None;
    }
}

impl Builder {
    fn validate(&self, lua: &Lua, check: &impl Fn() -> mlua::Result<()>) -> mlua::Result<()> {
        if super::super::notes::host_of(lua, &self.target)? != self.host {
            return Err(runtime("The note writer's target changed."));
        }
        read(lua, |w| {
            let view = plumspirit_engine::editor::note_view(
                &w.doc,
                self.host.target(&w.doc).map_err(runtime)?,
            )
            .map_err(|e| runtime(e.user_message()))?;
            let track = w
                .doc
                .track_by_id(self.host.track)
                .ok_or_else(|| runtime("The target track no longer exists."))?;
            if view.notes.revision() != self.revision
                || track.props().default_channel != self.channel
            {
                return Err(runtime(
                    "The note writer's target changed after it opened; cancel it and open a new writer.",
                ));
            }
            let max_key = w.doc.project().keys.max_pitch();
            for (i, r) in self.records.iter().enumerate() {
                if i.is_multiple_of(CHECK) {
                    check()?;
                }
                validate_group(&w.doc, r.group)?;
                if r.attrs.pitch() > max_key {
                    return Err(runtime(
                        "A staged key exceeds this project's key mode; enable 256-Key Mode first.",
                    ));
                }
                if !track.subchannels().contains(r.sub) {
                    return Err(runtime(
                        "A staged note uses a subchannel that does not exist on the target track.",
                    ));
                }
            }
            Ok(())
        })
    }

    fn grow(
        &mut self,
        required: usize,
        budget: &NativeBudget,
        check: &impl Fn() -> mlua::Result<()>,
    ) -> mlua::Result<()> {
        if required <= self.records.capacity() {
            return Ok(());
        }
        let capacity = required
            .max(self.records.capacity().saturating_mul(2))
            .max(64);
        let guard = budget
            .reserve(capacity.saturating_mul(RECORD_BYTES))
            .map_err(|e| e.lua())?;
        let mut replacement = Vec::new();
        replacement
            .try_reserve_exact(capacity)
            .map_err(|_| stop(Category::Budget, "Native note staging allocation failed."))?;
        for chunk in self.records.chunks(CHECK) {
            check()?;
            replacement.extend_from_slice(chunk);
        }
        self.records = replacement;
        self.allocation = Some(guard);
        Ok(())
    }
}

// Only plain dense data: no __index, __len, iterator or conversion callbacks.
fn plain(table: &Table) -> mlua::Result<()> {
    if table.metatable().is_some() {
        return Err(runtime(
            "Note writer batches must be plain tables without metatables.",
        ));
    }
    Ok(())
}

fn dense(
    table: &Table,
    expected: Option<usize>,
    check: &impl Fn() -> mlua::Result<()>,
) -> mlua::Result<usize> {
    plain(table)?;
    let len = table.raw_len();
    if len > MAX_BATCH || expected.is_some_and(|n| n != len) {
        return Err(runtime(
            "Note writer columns must have equal lengths and batches may contain at most 65536 notes.",
        ));
    }
    let mut count = 0usize;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        count += 1;
        if count.is_multiple_of(CHECK) {
            check()?;
        }
        if count > len || !matches!(key, Value::Integer(i) if i >= 1 && i as u64 <= len as u64) {
            return Err(runtime(
                "Note writer arrays must be dense, with integer indices 1 through their length and no extra fields.",
            ));
        }
    }
    if count != len {
        return Err(runtime("Note writer arrays cannot contain holes."));
    }
    Ok(len)
}

fn fields(table: &Table) -> mlua::Result<()> {
    plain(table)?;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        if !matches!(key, Value::String(ref s) if matches!(s.to_str()?.as_ref(), "start" | "gate" | "key" | "velocity" | "channel" | "subchannel" | "muted" | "off_kind" | "off_velocity" | "group"))
        {
            return Err(runtime(
                "A note writer accepts only start, gate, key, velocity, channel, subchannel, muted, off_kind, off_velocity and group fields.",
            ));
        }
    }
    Ok(())
}

fn scalar(value: Value) -> mlua::Result<()> {
    match value {
        Value::Nil | Value::Boolean(_) | Value::Integer(_) | Value::Number(_) => Ok(()),
        Value::String(s) if s.as_bytes().len() <= 64 => Ok(()),
        _ => Err(runtime(
            "Note writer scalar values must be numbers or decimal strings of at most 64 bytes.",
        )),
    }
}

// One bounded stack adapter (248 bytes with all optional columns) per append.
// Boxing it would add allocator traffic to every streamed batch.
#[allow(clippy::large_enum_variant)]
enum Batch {
    Rows(Table, usize),
    Columns(note_input::Columns, usize),
}
impl Batch {
    fn parse(table: Table, check: &impl Fn() -> mlua::Result<()>) -> mlua::Result<Self> {
        plain(&table)?;
        if let Value::Table(starts) = table.raw_get::<Value>("start")? {
            fields(&table)?;
            let len = dense(&starts, None, check)?;
            for name in [
                "gate",
                "key",
                "velocity",
                "channel",
                "subchannel",
                "muted",
                "off_kind",
                "off_velocity",
                "group",
            ] {
                let value = table.raw_get::<Value>(name)?;
                if !value.is_nil() {
                    let Value::Table(column) = value else {
                        return Err(runtime("Each note writer column must be an array."));
                    };
                    dense(&column, Some(len), check)?;
                }
            }
            Ok(Self::Columns(
                note_input::Columns::raw(&table, starts)?,
                len,
            ))
        } else {
            let len = dense(&table, None, check)?;
            Ok(Self::Rows(table, len))
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::Rows(_, n) | Self::Columns(_, n) => *n,
        }
    }
    fn record(&self, i: usize, channel: u8) -> mlua::Result<NoteRecord> {
        match self {
            Self::Rows(rows, _) => {
                let row: Table = rows.raw_get(i)?;
                fields(&row)?;
                for key in [
                    "start",
                    "gate",
                    "key",
                    "velocity",
                    "channel",
                    "subchannel",
                    "muted",
                    "off_kind",
                    "off_velocity",
                    "group",
                ] {
                    scalar(row.raw_get(key)?)?;
                }
                record_from_input(note_input::Input::Row(&row), None, channel)
            }
            Self::Columns(columns, _) => {
                for key in [
                    "start",
                    "gate",
                    "key",
                    "velocity",
                    "channel",
                    "subchannel",
                    "muted",
                    "off_kind",
                    "off_velocity",
                    "group",
                ] {
                    scalar(columns.value(key, i)?)?;
                }
                record_from_input(note_input::Input::Column(columns, i), None, channel)
            }
        }
    }
}

// Stable bottom-up merges: equal starts retain append order across batch sizes.
fn sort(
    records: &mut Vec<NoteRecord>,
    budget: &NativeBudget,
    check: &impl Fn() -> mlua::Result<()>,
) -> mlua::Result<()> {
    let len = records.len();
    if len < 2 {
        return Ok(());
    }
    let _scratch = budget
        .reserve(records.capacity().saturating_mul(RECORD_BYTES))
        .map_err(|e| e.lua())?;
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(records.capacity())
        .map_err(|_| stop(Category::Budget, "Native note ordering allocation failed."))?;
    let mut width = 1usize;
    while width < len {
        scratch.clear();
        for start in (0..len).step_by(width.saturating_mul(2)) {
            let middle = (start + width).min(len);
            let end = middle.saturating_add(width).min(len);
            let (mut left, mut right) = (start, middle);
            while left < middle || right < end {
                if scratch.len().is_multiple_of(CHECK) {
                    check()?;
                }
                if right == end
                    || (left < middle && records[left].start_tick <= records[right].start_tick)
                {
                    scratch.push(records[left]);
                    left += 1;
                } else {
                    scratch.push(records[right]);
                    right += 1;
                }
            }
        }
        std::mem::swap(records, &mut scratch);
        width = width.saturating_mul(2);
    }
    check()
}

fn append(lua: &Lua, writer: &Writer, value: Value) -> mlua::Result<usize> {
    let mut builder = take(lua, writer)?;
    let result = (|| {
        let check = || scope(lua, &writer.lease);
        let Value::Table(table) = value else {
            return Err(runtime(
                "A note writer batch must be a plain row or column table.",
            ));
        };
        let batch = Batch::parse(table, &check)?;
        super::super::notes::check_output(lua, output_so_far(lua), batch.len() as u64)?;
        let old_len = builder.records.len();
        builder.grow(old_len.saturating_add(batch.len()), &budget(lua)?, &check)?;
        for i in 1..=batch.len() {
            if (i - 1).is_multiple_of(CHECK) {
                check()?;
            }
            let r = batch.record(i, builder.channel)?;
            builder.sorted &= builder
                .records
                .last()
                .is_none_or(|p| p.start_tick <= r.start_tick);
            builder.records.push(r);
        }
        accept(lua, writer, builder, old_len, batch.len())
    })();
    if result.is_err() {
        poison(lua, writer);
    }
    result
}

fn accept(
    lua: &Lua,
    writer: &Writer,
    builder: Builder,
    old_len: usize,
    added: usize,
) -> mlua::Result<usize> {
    let check = || scope(lua, &writer.lease);
    // Validate only new records; revalidate the complete stage at finish.
    validate_target(lua, &builder)?;
    read(lua, |w| {
        let track = w
            .doc
            .track_by_id(builder.host.track)
            .ok_or_else(|| runtime("The target track no longer exists."))?;
        for (i, r) in builder.records[old_len..].iter().enumerate() {
            if i.is_multiple_of(CHECK) {
                check()?;
            }
            validate_group(&w.doc, r.group)?;
            if r.attrs.pitch() > w.doc.project().keys.max_pitch()
                || !track.subchannels().contains(r.sub)
            {
                return Err(runtime(
                    "A staged note exceeds the project's key mode or uses a missing subchannel.",
                ));
            }
        }
        Ok(())
    })?;
    check()?;
    super::super::notes::check_output(lua, output_so_far(lua), added as u64)?;
    let mut slot = lua
        .app_data_mut::<TxSlot>()
        .ok_or_else(|| runtime("Transactions are unavailable."))?;
    let entry = entry_mut(&mut slot, writer)?;
    if !matches!(entry.state, State::Busy) {
        return Err(runtime("The note writer was canceled during staging."));
    }
    entry.count = builder.records.len();
    entry.bytes = builder.records.capacity() * RECORD_BYTES;
    entry.state = State::Ready(builder);
    slot.writer_accepted = slot.writer_accepted.saturating_add(added as u64);
    Ok(added)
}

fn append_plan(lua: &Lua, writer: &Writer, value: Value, palette: Value) -> mlua::Result<usize> {
    let mut builder = take(lua, writer)?;
    let result = (|| {
        let check = || scope(lua, &writer.lease);
        let Value::UserData(plan) = value else {
            return Err(runtime("append_plan needs an image note plan."));
        };
        super::super::images::plans::with_native(lua, &plan, |reader| {
            // Fixed bounded stack adapters, never a per-note Lua table or scratch Vec.
            let n = reader.palette_count();
            let mut mapped = [Some((SubchannelId::MAIN, builder.channel)); 1024];
            if !palette.is_nil() {
                let Value::Table(table) = &palette else {
                    return Err(runtime(
                        "An image palette map must be a dense plain array of Subchannel handles or false.",
                    ));
                };
                dense(table, Some(n), &check)?;
                for (i, target) in mapped[..n].iter_mut().enumerate() {
                    check()?;
                    *target = match table.raw_get::<Value>(i + 1)? {
                        Value::Boolean(false) => None,
                        value => {
                            let handle = handle_arg(&value, "The image palette target")?;
                            Some(with_world(lua, &handle, |_, resolved| match resolved {
                                Resolved::Subchannel { track, sub, .. }
                                    if track.id() == builder.host.track =>
                                {
                                    Ok((sub.id, sub.midi_channel))
                                }
                                _ => Err(runtime(
                                    "Every mapped image color must use a Subchannel handle of the writer's hosting track.",
                                )),
                            })?)
                        }
                    };
                }
            }
            validate_target(lua, &builder)?;
            let included = mapped.map(|m| m.is_some());
            let added = reader.count_mapped(&included[..n], lua)?;
            super::super::notes::check_output(lua, output_so_far(lua), added as u64)?;
            let old_len = builder.records.len();
            let required = old_len
                .checked_add(added)
                .ok_or_else(|| runtime("Image note staging count overflowed."))?;
            builder.grow(required, &budget(lua)?, &check)?;
            reader.visit(lua, |row| {
                if let Some((sub, channel)) = mapped[row.palette as usize - 1] {
                    let record = NoteRecord {
                        start_tick: row.start,
                        duration_ticks: row.gate,
                        attrs: NoteAttrs::new_wide(
                            row.key,
                            row.velocity,
                            channel,
                            0,
                            NoteOffKind::NoteOff,
                        )
                        .map_err(|e| runtime(format!("That image note is invalid: {e}.")))?,
                        sub,
                        group: 0,
                    };
                    builder.sorted &= builder
                        .records
                        .last()
                        .is_none_or(|r| r.start_tick <= record.start_tick);
                    builder.records.push(record);
                }
                Ok(())
            })?;
            accept(lua, writer, builder, old_len, added)
        })
    })();
    if result.is_err() {
        poison(lua, writer);
    }
    result
}

fn validate_target(lua: &Lua, builder: &Builder) -> mlua::Result<()> {
    if super::super::notes::host_of(lua, &builder.target)? != builder.host {
        return Err(runtime("The writer target changed."));
    }
    read(lua, |w| {
        let notes = builder
            .host
            .notes(&w.doc)
            .ok_or_else(|| runtime("The writer target no longer exists."))?;
        let track = w
            .doc
            .track_by_id(builder.host.track)
            .ok_or_else(|| runtime("The writer track no longer exists."))?;
        if notes.revision() != builder.revision || track.props().default_channel != builder.channel
        {
            return Err(runtime(
                "The note writer's target changed after it opened; cancel it and open a new writer.",
            ));
        }
        plumspirit_engine::editor::note_view(&w.doc, builder.host.target(&w.doc).map_err(runtime)?)
            .map_err(|e| runtime(e.user_message()))?;
        Ok(())
    })
}

fn finish(lua: &Lua, writer: &Writer) -> mlua::Result<usize> {
    finish_controlled(lua, writer, || {})
}

fn finish_controlled(
    lua: &Lua,
    writer: &Writer,
    after_apply: impl FnOnce(),
) -> mlua::Result<usize> {
    let mut builder = take(lua, writer)?;
    let mut attempted_apply = false;
    let result = (|| {
        let check = || scope(lua, &writer.lease);
        builder.validate(lua, &check)?;
        let budget = budget(lua)?;
        if !builder.sorted {
            sort(&mut builder.records, &budget, &check)?;
        }
        let count = builder.records.len();
        let workspace = if count == 0 {
            0
        } else {
            read(lua, |w| {
                builder
                    .host
                    .notes(&w.doc)
                    .ok_or_else(|| runtime("The target disappeared."))?
                    .insertion_workspace_bytes(&builder.records, &check)
            })?
        };
        // Existing planner builds work/recorded ops and compact Undo, plus new
        // packed chunks/ranks. Preflight the peak while staging is still charged.
        let peak = if count == 0 {
            0
        } else {
            count
                .saturating_mul(RECORD_BYTES * 2 + 160)
                .saturating_add(workspace)
                .saturating_add(4096)
        };
        let mut guard = budget.reserve(peak).map_err(|e| e.lua())?;
        check()?;
        validate_target(lua, &builder)?;
        if count != 0 {
            attempted_apply = true;
            apply(
                lua,
                writer.serial,
                Step::InsertNotes {
                    host: builder.host,
                    records: std::mem::take(&mut builder.records),
                },
                0,
            )?;
        }
        after_apply();
        check()?;
        let mut slot = lua
            .app_data_mut::<TxSlot>()
            .ok_or_else(|| runtime("Transactions are unavailable."))?;
        let entry = entry_mut(&mut slot, writer)?;
        if !matches!(entry.state, State::Busy) {
            return Err(runtime("The writer was canceled while finishing."));
        }
        entry.state = State::Finished;
        entry.bytes = 0;
        let tx = slot
            .open
            .as_mut()
            .expect("entry belongs to open transaction");
        tx.writer_notes = tx.writer_notes.saturating_add(count as u64);
        guard.shrink_to(
            count
                .saturating_mul(RECORD_BYTES + 32)
                .saturating_add(workspace),
        );
        if count != 0 {
            tx.native.push(guard);
        }
        Ok(count)
    })();
    if result.is_err() {
        if attempted_apply {
            discard_transaction(lua, writer.serial);
        } else {
            poison(lua, writer);
        }
    }
    result
}

pub(super) fn staged_notes(tx: &OpenTx) -> usize {
    tx.writers
        .iter()
        .filter(|e| matches!(e.state, State::Ready(_) | State::Busy))
        .map(|e| e.count)
        .sum()
}

pub(super) fn check_commit(tx: &OpenTx) -> mlua::Result<()> {
    for entry in &tx.writers {
        if !matches!(entry.state, State::Finished | State::Canceled) {
            return Err(runtime(if entry.life.strong_count() == 0 {
                "An unfinished note writer was abandoned; cancel this transaction."
            } else {
                "Finish or cancel every note writer before committing; failed writers must be canceled explicitly."
            }));
        }
    }
    Ok(())
}

pub(super) fn install_limits(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let notes = lua.create_table()?;
    notes.set("max_write_batch", MAX_BATCH)?;
    notes.set("max_writers", MAX_WRITERS)?;
    plum.set("notes", notes)
}

pub(super) fn add_methods<M: UserDataMethods<LuaTx>>(m: &mut M) {
    m.add_method("note_writer", |lua, tx, value: Value| {
        require(lua, Capability::ReadProject)?;
        require(lua, Capability::EditNotes)?;
        let lease = lua
            .app_data_ref::<Session>()
            .and_then(|s| s.lease.clone())
            .ok_or_else(|| runtime("No plugin job is active."))?;
        scope(lua, &lease)?;
        let target = handle_arg(&value, "The note writer target")?;
        let host = super::super::notes::host_of(lua, &target)?;
        let (revision, channel) = read(lua, |w| {
            plumspirit_engine::editor::note_view(&w.doc, host.target(&w.doc).map_err(runtime)?)
                .map_err(|e| runtime(e.user_message()))?;
            Ok((
                host.notes(&w.doc)
                    .ok_or_else(|| runtime("The target has no notes."))?
                    .revision(),
                w.doc
                    .track_by_id(host.track)
                    .ok_or_else(|| runtime("The track disappeared."))?
                    .props()
                    .default_channel,
            ))
        })?;
        let metadata = budget(lua)?
            // Vec's first push may allocate four entry slots. Later doubling
            // stays covered by cumulative entry reservations, plus small owners.
            .reserve(std::mem::size_of::<Entry>() * 4 + 128)
            .map_err(|e| e.lua())?;
        let life = Rc::new(());
        let mut slot = lua
            .app_data_mut::<TxSlot>()
            .ok_or_else(|| runtime("Transactions are unavailable."))?;
        let open = slot
            .open
            .as_mut()
            .filter(|t| t.serial == tx.0)
            .ok_or_else(|| runtime("This transaction is closed."))?;
        if open.writers.len() >= MAX_WRITERS {
            return Err(stop(
                Category::Budget,
                "A transaction may create at most 32 note writers.",
            ));
        }
        let index = open.writers.len();
        open.writers.push(Entry {
            state: State::Ready(Builder {
                target,
                host,
                revision,
                channel,
                records: Vec::new(),
                allocation: None,
                sorted: true,
            }),
            life: Rc::downgrade(&life),
            count: 0,
            bytes: 0,
            _metadata: metadata,
        });
        Ok(Writer {
            serial: tx.0,
            index,
            lease,
            _life: life,
        })
    });
}

impl UserData for Writer {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("append", append);
        m.add_method(
            "append_plan",
            |lua, this, (plan, palette): (Value, Value)| append_plan(lua, this, plan, palette),
        );
        m.add_method("finish", |lua, this, ()| finish(lua, this));
        m.add_method("cancel", |lua, this, ()| {
            if let Some(mut slot) = lua.app_data_mut::<TxSlot>()
                && let Ok(entry) = entry_mut(&mut slot, this)
            {
                if matches!(entry.state, State::Finished) { return Err(runtime("The writer is already finished; cancel its transaction to discard those notes.")); }
                entry.state = State::Canceled;
                entry.bytes = 0;
            }
            Ok(())
        });
        m.add_method("info", |lua, this, ()| {
            scope(lua, &this.lease)?;
            let (state, count, bytes) = {
                let mut slot = lua
                    .app_data_mut::<TxSlot>()
                    .ok_or_else(|| runtime("Transactions are unavailable."))?;
                let entry = entry_mut(&mut slot, this)?;
                let state = match entry.state {
                    State::Ready(_) => "open",
                    State::Busy => "busy",
                    State::Finished => "finished",
                    State::Canceled => "canceled",
                    State::Poisoned => "failed",
                };
                (state, entry.count, entry.bytes)
            };
            let info = lua.create_table()?;
            info.set("state", state)?;
            info.set("notes", count)?;
            info.set("staged_bytes", bytes)?;
            info.set("max_batch", MAX_BATCH)?;
            Ok(info)
        });
    }
}

#[cfg(test)]
#[path = "tx_writer_tests.rs"]
mod tests;
