//! Indexed note-mask queries; share the native NoteStore overlap index.
use super::*;
use crate::lua::{
    handles,
    notes::{Host as NoteHost, LuaNoteQuery},
};
use plumspirit_engine::model::NoteRecord;

pub(in crate::lua) fn visit(
    lua: &Lua,
    sampler: &Stored,
    query: &LuaNoteQuery,
    mut emit: impl FnMut(usize, NoteRecord, bool) -> mlua::Result<bool>,
) -> mlua::Result<(NoteHost, usize)> {
    handles::require_read(lua)?;
    let host = notes::host_of(lua, &query.host)?;
    let world = handles::current_world(lua).ok_or_else(|| bad("No project is open."))?;
    let store = host
        .notes(&world.doc)
        .ok_or_else(|| bad("The note target no longer exists."))?;
    let filter = &query.filter;
    let mut matched = 0usize;
    let m = sampler.mapping;
    store.try_query_ranked(
        m.start,
        m.end - 1,
        || notes::check_budget(lua),
        |rank, note| {
            let record = note.record();
            if record.start_tick < filter.t0
                || filter.t1.is_some_and(|end| record.start_tick >= end)
                || (filter.selected_only && !note.selected)
                || !filter.accepts(&record)
                || !sampler
                    .sample(
                        record.start_tick,
                        record.duration_ticks,
                        record.attrs.pitch(),
                    )
                    .1
            {
                return Ok(true);
            }
            matched += 1;
            emit(rank, record, note.selected)
        },
    )?;
    Ok((host, matched))
}
pub(super) fn count(lua: &Lua, this: &Sampler, query: AnyUserData) -> mlua::Result<usize> {
    handles::require_read(lua)?;
    let query = query.borrow::<LuaNoteQuery>()?;
    this.with(lua, |sampler| {
        visit(lua, sampler, &query, |_, _, _| Ok(true)).map(|(_, n)| n)
    })
}
