//! Native mask-conditioned edits through existing transaction primitives.
use super::super::{bridge::NativeBudget, images::note_masks};
use super::*;
use std::mem::size_of;
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Select,
    Delete,
    Duplicate,
    Modify,
}
struct Options {
    action: Action,
    mode: String,
    changes: Changes,
}
fn word(table: &Table, key: &str, default: Option<&str>) -> mlua::Result<String> {
    match table.raw_get::<Value>(key)? {
        Value::Nil => default
            .map(str::to_owned)
            .ok_or_else(|| runtime(format!("Mask edit {key} is required."))),
        Value::String(s) if s.as_bytes().len() <= 32 => Ok(s.to_str()?.to_string()),
        _ => Err(runtime(format!("Mask edit {key} must be bounded text."))),
    }
}
fn parse(table: &Table) -> mlua::Result<Options> {
    if table.metatable().is_some() {
        return Err(runtime("Mask edit options must be a plain table."));
    }
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        if !matches!(key,Value::String(ref s)if matches!(s.as_bytes().as_ref(),b"action"|b"mode"|b"changes"))
        {
            return Err(runtime("Mask edit options are action, mode and changes."));
        }
    }
    let action = match word(table, "action", None)?.as_str() {
        "select" => Action::Select,
        "delete" => Action::Delete,
        "duplicate" => Action::Duplicate,
        "modify" => Action::Modify,
        _ => {
            return Err(runtime(
                "Mask edit action must be select, delete, duplicate or modify.",
            ));
        }
    };
    let mode = word(table, "mode", Some("replace"))?;
    if !matches!(mode.as_str(), "replace" | "add" | "remove" | "toggle")
        || (action != Action::Select && !table.raw_get::<Value>("mode")?.is_nil())
    {
        return Err(runtime(
            "Mask edit mode is replace/add/remove/toggle and applies only to select.",
        ));
    }
    let changes = match table.raw_get::<Value>("changes")? {
        Value::Nil if action != Action::Modify => Changes::default(),
        Value::Table(t) if matches!(action, Action::Modify | Action::Duplicate) => {
            if t.metatable().is_some() {
                return Err(runtime("Mask edit changes must be a plain scalar table."));
            }
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, v) = pair?;
                if !matches!(k,Value::String(ref s)if matches!(s.as_bytes().as_ref(),b"shift"|b"transpose"|b"velocity"|b"velocity_add"|b"velocity_scale"|b"gate"|b"gate_add"|b"gate_scale"|b"channel"|b"subchannel"|b"outside"|b"muted"|b"off_kind"|b"off_velocity"|b"group"))
                {
                    return Err(runtime("Unknown mask edit note-change field."));
                }
                if !matches!(
                    v,
                    Value::Integer(_) | Value::Number(_) | Value::Boolean(_) | Value::String(_)
                ) {
                    return Err(runtime("Mask edit changes accept only scalar note fields."));
                }
                if matches!(&v,Value::String(s)if s.as_bytes().len()>64) {
                    return Err(runtime(
                        "Mask edit scalar strings may contain at most64bytes.",
                    ));
                }
            }
            for key in ["gate_scale", "velocity_scale"] {
                match t.raw_get::<Value>(key)? {
                    Value::Nil => {}
                    Value::Integer(n) if n >= 0 => {}
                    Value::Number(n) if n.is_finite() && n >= 0. => {}
                    _ => {
                        return Err(runtime(
                            "Mask edit scales must be finite nonnegative numbers.",
                        ));
                    }
                }
            }
            Changes::from_lua(&t)?
        }
        _ => {
            return Err(runtime(
                "Mask modify requires changes; only modify/duplicate accept a plain changes table.",
            ));
        }
    };
    Ok(Options {
        action,
        mode,
        changes,
    })
}
fn selection(
    lua: &Lua,
    host: NoteHost,
    hits: &[super::super::notes::Hit],
    mode: &str,
) -> mlua::Result<Vec<usize>> {
    if mode == "replace" {
        let mut out = Vec::new();
        out.try_reserve_exact(hits.len())
            .map_err(|_| stop(Category::Memory, "Mask selection allocation failed."))?;
        for (i, hit) in hits.iter().enumerate() {
            if i.is_multiple_of(4096) {
                check_budget(lua)?;
            }
            out.push(hit.rank);
        }
        return Ok(out);
    }
    let current = read(lua, |w| {
        Ok(host
            .notes(&w.doc)
            .ok_or_else(|| runtime("The source disappeared."))?
            .selected_ranks())
    })?;
    let mut out = Vec::new();
    out.try_reserve_exact(current.len().saturating_add(hits.len()))
        .map_err(|_| {
            stop(
                Category::Memory,
                "Mask selection storage allocation failed.",
            )
        })?;
    let mut a = current.iter().peekable();
    let mut b = hits.iter().map(|h| h.rank).peekable();
    let mut steps = 0usize;
    while a.peek().is_some() || b.peek().is_some() {
        if steps.is_multiple_of(4096) {
            check_budget(lua)?;
        }
        steps += 1;
        let (rank, was, picked) = match (a.peek().copied(), b.peek().copied()) {
            (Some(x), Some(y)) if x == y => {
                a.next();
                b.next();
                (x, true, true)
            }
            (Some(x), Some(y)) if x < y => {
                a.next();
                (x, true, false)
            }
            (Some(_), Some(y)) => {
                b.next();
                (y, false, true)
            }
            (Some(x), None) => {
                a.next();
                (x, true, false)
            }
            (None, Some(y)) => {
                b.next();
                (y, false, true)
            }
            _ => break,
        };
        if match mode {
            "replace" => picked,
            "add" => was || picked,
            "remove" => was && !picked,
            "toggle" => was != picked,
            _ => false,
        } {
            out.push(rank);
        }
    }
    Ok(out)
}
fn edit(
    lua: &Lua,
    tx: &LuaTx,
    query: Value,
    sampler: AnyUserData,
    options: Table,
) -> mlua::Result<usize> {
    edit_controlled(lua, tx, query, sampler, options, || {})
}
fn edit_controlled(
    lua: &Lua,
    tx: &LuaTx,
    query: Value,
    sampler: AnyUserData,
    options: Table,
    after_first: impl FnOnce(),
) -> mlua::Result<usize> {
    require(lua, Capability::ReadProject)?;
    require(lua, Capability::EditNotes)?;
    check_budget(lua)?;
    if !lua
        .app_data_ref::<TxSlot>()
        .is_some_and(|s| s.open.as_ref().is_some_and(|t| t.serial == tx.0))
    {
        return Err(runtime("This mask edit transaction is closed."));
    }
    let options = parse(&options)?;
    if options.action == Action::Select {
        super::super::response::explicit_only(lua, "Image-mask selection changes")?;
    }
    let query = query_arg(&query)?;
    note_masks::with_native(lua, &sampler, |mask| {
        let host = super::super::notes::host_of(lua, &query.host)?;
        let mut emitted = 0usize;
        let matched = read(lua, |w| {
            let track = w
                .doc
                .track_by_id(host.track)
                .ok_or_else(|| runtime("The source track disappeared."))?;
            note_masks::queries::visit(lua, mask, &query, |_, record, _| {
                if options.action != Action::Select {
                    validate_group(&w.doc, record.group)?;
                }
                if matches!(options.action, Action::Modify | Action::Duplicate)
                    && let Some(r) = options
                        .changes
                        .apply(&record, w.doc.project().keys.max_pitch())
                        .map_err(runtime)?
                {
                    validate_group(&w.doc, r.group)?;
                    if !track.subchannels().contains(r.sub) {
                        return Err(runtime("A mask edit uses a missing target Subchannel."));
                    }
                    emitted += 1;
                }
                Ok(true)
            })
            .map(|(_, count)| count)
        })?;
        if matched == 0 && !(options.action == Action::Select && options.mode == "replace") {
            return Ok(0);
        }
        super::super::notes::check_output(lua, output_so_far(lua), emitted as u64)?;
        let deleted = match options.action {
            Action::Delete => matched,
            Action::Modify => matched - emitted,
            _ => 0,
        };
        let removed = lua
            .app_data_ref::<TxSlot>()
            .and_then(|s| s.open.as_ref().map(|t| t.removed_notes))
            .unwrap_or(0);
        super::super::response::check_removed(lua, removed, deleted as u64)?;
        let budget = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| runtime("Native mask edit budget unavailable."))?
            .clone();
        let staging_bytes = matched
            .saturating_mul(size_of::<super::super::notes::Hit>())
            .saturating_add(emitted.saturating_mul(size_of::<NoteRecord>()))
            .saturating_add(4096);
        let _staging = budget.reserve(staging_bytes).map_err(|e| e.lua())?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(matched)
            .map_err(|_| stop(Category::Memory, "Mask note staging allocation failed."))?;
        let mut records = Vec::new();
        records.try_reserve_exact(emitted).map_err(|_| {
            stop(
                Category::Memory,
                "Mask changed-note staging allocation failed.",
            )
        })?;
        let max_key = read(lua, |w| Ok(w.doc.project().keys.max_pitch()))?;
        note_masks::queries::visit(lua, mask, &query, |rank, record, selected| {
            hits.push(super::super::notes::Hit {
                rank,
                rec: record,
                selected,
            });
            if matches!(options.action, Action::Modify | Action::Duplicate)
                && let Some(r) = options.changes.apply(&record, max_key).map_err(runtime)?
            {
                records.push(r);
            }
            Ok(true)
        })?;
        if hits.len() != matched || records.len() != emitted {
            return Err(runtime("The mask source changed during staging."));
        }
        // Fixed uniform Changes preserve time order, including clamp-to-zero.
        if records
            .windows(2)
            .any(|w| w[0].start_tick > w[1].start_tick)
        {
            return Err(runtime("Mask changes did not preserve native note order."));
        }
        let (workspace, selection_bytes) = read(lua, |w| {
            let store = host
                .notes(&w.doc)
                .ok_or_else(|| runtime("The source disappeared."))?;
            let (lo, hi) = mask.domain();
            let mut touched = 0usize;
            for (i, (_, _, n)) in store.chunk_spans(lo, hi - 1).enumerate() {
                if i.is_multiple_of(1024) {
                    check_budget(lua)?;
                }
                touched = touched.saturating_add(n);
            }
            let insertion = store.insertion_workspace_bytes(&records, || check_budget(lua))?;
            Ok((
                touched
                    .saturating_mul(64)
                    .saturating_add(store.chunk_count().saturating_mul(128))
                    .saturating_add(insertion),
                store.selected_count().saturating_mul(32),
            ))
        })?;
        let peak = workspace
            .saturating_add(selection_bytes)
            .saturating_add(matched.saturating_mul(64))
            .saturating_add(emitted.saturating_mul(size_of::<NoteRecord>() * 2 + 160))
            .saturating_add(32768);
        let mut native = budget.reserve(peak).map_err(|e| e.lua())?;
        let selected = if options.action == Action::Select {
            Some(selection(lua, host, &hits, &options.mode)?)
        } else {
            None
        };
        check_budget(lua)?;
        let result = (|| {
            match options.action {
                Action::Select => set_selection(lua, tx.0, host, selected.as_ref().unwrap())?,
                Action::Delete => erase(lua, tx.0, host, &hits, true)?,
                Action::Duplicate => {
                    if !records.is_empty() {
                        apply(
                            lua,
                            tx.0,
                            Step::InsertNotes {
                                host,
                                records: std::mem::take(&mut records),
                            },
                            0,
                        )?;
                    }
                }
                Action::Modify => erase(lua, tx.0, host, &hits, false)?,
            }
            after_first();
            check_budget(lua)?;
            if options.action == Action::Modify && !records.is_empty() {
                apply(
                    lua,
                    tx.0,
                    Step::InsertNotes {
                        host,
                        records: std::mem::take(&mut records),
                    },
                    0,
                )?;
            }
            check_budget(lua)?;
            let mut slot = lua
                .app_data_mut::<TxSlot>()
                .ok_or_else(|| runtime("Transactions unavailable."))?;
            let open = slot
                .open
                .as_mut()
                .filter(|t| t.serial == tx.0)
                .ok_or_else(|| runtime("The mask transaction was canceled."))?;
            if options.action == Action::Modify {
                open.removed_notes = open.removed_notes.saturating_add(deleted as u64);
            }
            native.shrink_to(
                workspace
                    .saturating_add(matched.saturating_mul(64))
                    .saturating_add(emitted.saturating_mul(size_of::<NoteRecord>() + 32))
                    .saturating_add(1024),
            );
            open.native.push(native);
            Ok(matched)
        })();
        if result.is_err() {
            writer::discard_transaction(lua, tx.0);
        }
        result
    })
}
pub(super) fn add_methods<M: UserDataMethods<LuaTx>>(m: &mut M) {
    m.add_method(
        "mask_notes",
        |lua, tx, (query, sampler, options): (Value, AnyUserData, Table)| {
            edit(lua, tx, query, sampler, options)
        },
    );
}
#[cfg(test)]
#[path = "tx_image_mask_tests.rs"]
mod tests;
