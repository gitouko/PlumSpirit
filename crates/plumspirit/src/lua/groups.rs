//! Native project-wide group table and authoritative Lua edit guards.
//! Reuses GroupTable, incarnation handles and Operation::SetGroups/Undo.
use super::*;
use crate::lua::bridge::NativeBudget;
use crate::lua::handles::not_for;
use plumspirit_engine::model::{ContentRef, GroupInfo, GroupTable, NoteStore};

const MAX_LIST: usize = 4096;
const MAX_EDITS: usize = 1024;

fn group_id(w: &World, h: &Handle) -> mlua::Result<u32> {
    match h.resolve(w).map_err(|e| stop(Category::Expired, e))? {
        Resolved::Group(g) => Ok(g.id),
        _ => Err(runtime("A note group handle is required.")),
    }
}

pub(super) fn unlocked(table: &GroupTable, id: u32) -> mlua::Result<()> {
    if table.locked(id) {
        return Err(runtime(
            "The note group or an ancestor is locked; unlock it explicitly first.",
        ));
    }
    Ok(())
}

fn scan(
    lua: &Lua,
    store: &NoteStore,
    mut accept: impl FnMut(usize, &plumspirit_engine::model::NoteView) -> bool,
) -> mlua::Result<()> {
    for (i, n) in store.iter().enumerate() {
        if i.is_multiple_of(4096) {
            check_budget(lua)?;
        }
        if !accept(i, &n) {
            return Err(runtime(
                "This operation would change notes in a locked group or ancestor.",
            ));
        }
    }
    Ok(())
}

fn check_store(
    lua: &Lua,
    doc: &PlumDocument,
    store: &NoteStore,
    sub: Option<SubchannelId>,
) -> mlua::Result<()> {
    scan(lua, store, |_, n| {
        sub.is_some_and(|s| s != n.sub) || !doc.project().groups.locked(n.group)
    })
}

fn check_track(
    lua: &Lua,
    doc: &PlumDocument,
    track: TrackId,
    sub: Option<SubchannelId>,
) -> mlua::Result<()> {
    let t = doc
        .track_by_id(track)
        .ok_or_else(|| runtime("The track no longer exists."))?;
    check_store(lua, doc, &t.notes, sub)?;
    let count = doc
        .clips()
        .explicit_placements()
        .iter()
        .filter(|p| p.track == track && matches!(p.content, ContentRef::Owned(_)))
        .count();
    let _workspace = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| runtime("Native group accounting is unavailable."))?
        .reserve(count.saturating_mul(32))
        .map_err(|e| e.lua())?;
    let mut seen = HashSet::new();
    // A linked content is scanned once per track, not once per instance.
    for p in doc
        .clips()
        .explicit_placements()
        .iter()
        .filter(|p| p.track == track)
    {
        if let ContentRef::Owned(c) = p.content {
            if !seen.insert(c) {
                continue;
            }
            let store = &doc
                .clips()
                .content(c)
                .ok_or_else(|| runtime("The content no longer exists."))?
                .notes;
            check_store(lua, doc, store, sub)?;
        }
    }
    Ok(())
}

/// Every Lua musical/structural write passes here, not just the new APIs.
pub(super) fn check_step(lua: &Lua, doc: &PlumDocument, step: &Step) -> mlua::Result<()> {
    let table = &doc.project().groups;
    if table.is_empty() {
        return Ok(());
    }
    if !matches!(step, Step::InsertNotes { .. }) && !table.entries().iter().any(|g| g.locked) {
        return Ok(());
    }
    match step {
        Step::InsertNotes { records, .. } => {
            for (i, r) in records.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    check_budget(lua)?;
                }
                validate_group(doc, r.group)?;
            }
        }
        Step::EraseNotes { host, ranks } => {
            let store = host
                .notes(doc)
                .ok_or_else(|| runtime("The note target no longer exists."))?;
            store.try_visit_ranks(
                ranks.iter(),
                || check_budget(lua),
                |_, note| {
                    let note = note.ok_or_else(|| runtime("The notes changed before editing."))?;
                    unlocked(table, note.group)
                },
            )?;
        }
        Step::DeleteTrack { id } => {
            let start = doc
                .index_of(*id)
                .ok_or_else(|| runtime("The track no longer exists."))?;
            for t in &doc.tracks()[start..doc.subtree_end(start)] {
                check_track(lua, doc, t.id(), None)?;
            }
        }
        Step::DeleteSubchannel { track, id } => check_track(lua, doc, *track, Some(*id))?,
        Step::EditSubchannel {
            track,
            sub,
            move_notes: true,
        } => check_track(lua, doc, *track, Some(sub.id))?,
        Step::Clips(c) => {
            use crate::lua::clips::ClipStep;
            let check = |id| -> mlua::Result<()> {
                let p = doc
                    .clips()
                    .placement(id)
                    .ok_or_else(|| runtime("The clip no longer exists."))?;
                let store = doc
                    .notes_of(p.content)
                    .ok_or_else(|| runtime("The content no longer exists."))?;
                check_store(lua, doc, store, None)
            };
            match c {
                ClipStep::Move { moves } => {
                    for (id, _, _) in moves {
                        check(*id)?;
                    }
                }
                ClipStep::Duplicate { copies, .. } => {
                    for (id, _, _) in copies {
                        check(*id)?;
                    }
                }
                ClipStep::Resize { id, .. } => check(*id)?,
                ClipStep::Slice { ids, .. }
                | ClipStep::Mute { ids, .. }
                | ClipStep::Delete { ids }
                | ClipStep::Merge { ids, .. } => {
                    for id in ids {
                        check(*id)?;
                    }
                }
                ClipStep::Add { .. } | ClipStep::Rename { .. } | ClipStep::Color { .. } => {}
            }
        }
        _ => {}
    }
    Ok(())
}

struct Patch {
    parent: Option<Option<Handle>>,
    name: Option<Option<String>>,
    color: Option<Option<[u8; 3]>>,
    locked: Option<bool>,
    temporary: Option<bool>,
}
fn plain(t: &Table) -> mlua::Result<()> {
    if t.metatable().is_some() {
        return Err(runtime("Group options must be plain tables."));
    }
    Ok(())
}
impl Patch {
    fn parse(opts: Option<Table>) -> mlua::Result<Self> {
        let t = opts;
        if let Some(t) = &t {
            plain(t)?;
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, _) = pair?;
                if !matches!(k,Value::String(ref k) if matches!(k.as_bytes().as_ref(),b"parent"|b"name"|b"color"|b"locked"|b"temporary"))
                {
                    return Err(runtime(
                        "Group options are parent, name, color, locked and temporary.",
                    ));
                }
            }
        }
        let get =
            |key| -> mlua::Result<Value> { t.as_ref().map_or(Ok(Value::Nil), |t| t.raw_get(key)) };
        let boolean = |key| -> mlua::Result<Option<bool>> {
            match get(key)? {
                Value::Nil => Ok(None),
                Value::Boolean(b) => Ok(Some(b)),
                _ => Err(runtime("Group flags must be booleans.")),
            }
        };
        let parent = match get("parent")? {
            Value::Nil => None,
            Value::Boolean(false) => Some(None),
            v => Some(Some(handle_arg(&v, "The parent group")?)),
        };
        let name = match get("name")? {
            Value::Nil => None,
            Value::Boolean(false) => Some(None),
            Value::String(s) if s.as_bytes().len() <= 256 => {
                let name = s.to_str()?;
                if name.chars().count() > 64 {
                    return Err(runtime("A group name may contain at most 64 characters."));
                }
                Some(Some(name.to_owned()))
            }
            _ => {
                return Err(runtime(
                    "A group name must be UTF-8 text of at most 64 characters, or false to clear it.",
                ));
            }
        };
        let color = match get("color")? {
            Value::Nil => None,
            Value::Boolean(false) => Some(None),
            Value::Table(c) => {
                plain(&c)?;
                let byte = |k| -> mlua::Result<u8> {
                    let v: i64 = c.raw_get(k)?;
                    u8::try_from(v).map_err(|_| runtime("Group color components must be 0 - 255."))
                };
                Some(Some([byte("r")?, byte("g")?, byte("b")?]))
            }
            _ => {
                return Err(runtime(
                    "Group color must be {r,g,b}, or false to clear it.",
                ));
            }
        };
        Ok(Self {
            parent,
            name,
            color,
            locked: boolean("locked")?,
            temporary: boolean("temporary")?,
        })
    }
    fn apply(&self, w: &World, table: &mut GroupTable, id: u32, create: bool) -> mlua::Result<()> {
        let old = table
            .get(id)
            .cloned()
            .ok_or_else(|| runtime("The group no longer exists."))?;
        // Clearing one's lock is explicit; changing ancestry/temporary persistence
        // cannot piggyback on that unlock to escape an existing effective lock.
        if !create && (self.parent.is_some() || self.temporary.is_some_and(|b| b != old.temporary))
        {
            unlocked(table, id)?;
        }
        let parent = match &self.parent {
            None => old.parent,
            Some(None) => 0,
            Some(Some(h)) => group_id(w, h)?,
        };
        if self.parent.is_some() {
            unlocked(table, parent)?;
        }
        let g = table.get_mut(id).expect("checked group");
        g.parent = parent;
        if let Some(name) = &self.name {
            g.name = name.clone();
        }
        if let Some(color) = self.color {
            g.color = color;
        }
        if let Some(locked) = self.locked {
            g.locked = locked;
        }
        if let Some(temporary) = self.temporary {
            g.temporary = temporary;
        }
        Ok(())
    }
}

fn update(
    lua: &Lua,
    serial: u64,
    f: impl FnOnce(&World, &mut GroupTable) -> mlua::Result<u32>,
) -> mlua::Result<u32> {
    super::super::response::explicit_only(lua, "Group-table changes")?;
    require(lua, Capability::ReadProject)?;
    require(lua, Capability::EditNotes)?;
    check_budget(lua)?;
    {
        let mut slot = lua
            .app_data_mut::<TxSlot>()
            .ok_or_else(|| runtime("Transactions are unavailable."))?;
        if !slot.open.as_ref().is_some_and(|t| t.serial == serial) {
            return Err(runtime("This transaction is closed."));
        }
        if slot.group_edits >= MAX_EDITS {
            return Err(stop(
                Category::Budget,
                "A job may make at most 1024 group table edits.",
            ));
        }
        slot.group_edits += 1;
    }
    let (table, id, mut guard) = read(lua, |w| {
        let bytes = w
            .doc
            .project()
            .groups
            .heap_bytes()
            .saturating_mul(8)
            .saturating_add(8192);
        let guard = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| runtime("Native group accounting is unavailable."))?
            .reserve(bytes)
            .map_err(|e| e.lua())?;
        let mut table = w.doc.project().groups.clone();
        let id = f(w, &mut table)?;
        table.validate().map_err(runtime)?;
        check_budget(lua)?;
        Ok((table, id, guard))
    })?;
    let retained = table.heap_bytes().saturating_mul(2).saturating_add(1024);
    apply(
        lua,
        serial,
        Step::SetGroups {
            table: Box::new(table),
        },
        0,
    )?;
    if let Err(e) = check_budget(lua) {
        if let Some(mut slot) = lua.app_data_mut::<TxSlot>()
            && slot.open.as_ref().is_some_and(|t| t.serial == serial)
        {
            slot.open = None;
        }
        return Err(e);
    }
    guard.shrink_to(retained);
    let mut slot = lua
        .app_data_mut::<TxSlot>()
        .ok_or_else(|| runtime("Transactions are unavailable."))?;
    slot.open
        .as_mut()
        .filter(|t| t.serial == serial)
        .ok_or_else(|| runtime("This transaction is closed."))?
        .native
        .push(guard);
    Ok(id)
}

pub(super) fn add_tx_methods<M: UserDataMethods<LuaTx>>(m: &mut M) {
    m.add_method("create_group", |lua, tx, opts: Option<Table>| {
        let patch = Patch::parse(opts)?;
        let id = update(lua, tx.0, |w, table| {
            if table.len() >= plumspirit_engine::model::group::MAX_GROUPS
                || table.next_id() == u32::MAX
            {
                return Err(stop(
                    Category::Budget,
                    "The project group limit was reached.",
                ));
            }
            let id = table.add(GroupInfo::default());
            patch.apply(w, table, id, true)?;
            Ok(id)
        })?;
        read(lua, |w| handle_or_nil(lua, w.group(id)))
    });
    m.add_method("edit_group", |lua, tx, (g, opts): (Value, Table)| {
        let h = handle_arg(&g, "The group")?;
        let patch = Patch::parse(Some(opts))?;
        update(lua, tx.0, |w, table| {
            let id = group_id(w, &h)?;
            patch.apply(w, table, id, false)?;
            Ok(id)
        })
        .map(|_| ())
    });
    m.add_method(
        "link_groups",
        |lua, tx, (list, linked): (Table, Option<bool>)| {
            plain(&list)?;
            let len = list.raw_len();
            if !(1..=MAX_LIST).contains(&len) {
                return Err(runtime("Link 1 through 4096 group handles at a time."));
            }
            let _workspace = lua
                .app_data_ref::<NativeBudget>()
                .ok_or_else(|| runtime("Native group accounting is unavailable."))?
                .reserve(len.saturating_mul(std::mem::size_of::<Handle>() + 16))
                .map_err(|e| e.lua())?;
            let mut fields = 0;
            for pair in list.clone().pairs::<Value, Value>() {
                let (key, _) = pair?;
                fields += 1;
                if fields > len || !matches!(key,Value::Integer(i) if i>=1 && i as u64<=len as u64)
                {
                    return Err(runtime("Group handles must form a dense plain array."));
                }
            }
            let mut handles = Vec::with_capacity(len);
            for value in list.sequence_values::<Value>() {
                handles.push(handle_arg(&value?, "The group")?);
            }
            if handles.len() != len || fields != len {
                return Err(runtime("Group handles must form a dense plain array."));
            }
            update(lua, tx.0, |w, table| {
                let mut ids = handles
                    .iter()
                    .map(|h| group_id(w, h))
                    .collect::<mlua::Result<Vec<_>>>()?;
                ids.sort_unstable();
                ids.dedup();
                if linked.unwrap_or(true) && ids.len() < 2 {
                    return Err(runtime("Linking requires at least two distinct groups."));
                }
                for &id in &ids {
                    unlocked(table, id)?;
                }
                let link = if linked.unwrap_or(true) { ids[0] } else { 0 };
                for &id in &ids {
                    table.get_mut(id).expect("checked group").link = link;
                }
                Ok(0)
            })
            .map(|_| ())
        },
    );
    m.add_method("delete_group", |lua, tx, g: Value| {
        let h = handle_arg(&g, "The group")?;
        update(lua, tx.0, |w, table| {
            let id = group_id(w, &h)?;
            unlocked(table, id)?;
            if table.entries().iter().any(|g| g.parent == id) {
                return Err(runtime(
                    "Reparent or remove child groups before deleting this group.",
                ));
            }
            let check = |store: &NoteStore| -> mlua::Result<()> {
                for (i, n) in store.iter().enumerate() {
                    if i.is_multiple_of(4096) {
                        check_budget(lua)?;
                    }
                    if n.group == id {
                        return Err(runtime(
                            "Remove all member notes from this group before deleting it.",
                        ));
                    }
                }
                Ok(())
            };
            for t in w.doc.tracks() {
                check(&t.notes)?;
            }
            for c in w.doc.clips().contents() {
                check(&c.notes)?;
            }
            table.remove(&[id]);
            Ok(0)
        })
        .map(|_| ())
    });
}

pub(in crate::lua) fn add_read_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    m.add_method("group", |lua, this, id: i64| {
        with_world(lua, &this.0, |w, r| {
            if !matches!(r, Resolved::Project(_)) {
                return Err(not_for("group", &this.0));
            }
            let id = u32::try_from(id)
                .map_err(|_| runtime("A group ID must be 0 through 4294967295."))?;
            handle_or_nil(lua, w.group(id))
        })
    });
    m.add_method("groups", |lua, this, opts: Option<Table>| {
        let (offset, limit) = if let Some(t) = opts {
            plain(&t)?;
            (
                t.raw_get::<Option<usize>>("offset")?.unwrap_or(0),
                t.raw_get::<Option<usize>>("limit")?.unwrap_or(256),
            )
        } else {
            (0, 256)
        };
        if limit > MAX_LIST {
            return Err(runtime("A group page may contain at most 4096 handles."));
        }
        with_world(lua, &this.0, |w, r| {
            if !matches!(r, Resolved::Project(_)) {
                return Err(not_for("groups", &this.0));
            }
            handle_list(
                lua,
                w.doc
                    .project()
                    .groups
                    .entries()
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .filter_map(|g| w.group(g.id)),
            )
        })
    });
    m.add_method("group_info", |lua, this, ()| {
        with_world(lua, &this.0, |w, r| {
            let Resolved::Group(g) = r else {
                return Err(not_for("group_info", &this.0));
            };
            let out = lua.create_table()?;
            out.set("id", g.id)?;
            out.set("parent", g.parent)?;
            out.set("name", g.name.clone())?;
            out.set("locked", g.locked)?;
            out.set("effective_locked", w.doc.project().groups.locked(g.id))?;
            out.set("temporary", g.temporary)?;
            out.set("link", g.link)?;
            if let Some([r, g, b]) = g.color {
                let c = lua.create_table()?;
                c.set("r", r)?;
                c.set("g", g)?;
                c.set("b", b)?;
                out.set("color", c)?;
            }
            Ok(out)
        })
    });
}

#[cfg(test)]
#[path = "groups_tests.rs"]
mod tests;
