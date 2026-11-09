//! The Selection API (v1.0-beta1 B1-0031, `docs/LUA_API.md` "Selection").
//!
//! Note selection is part of the project (each note's selected flag): a
//! script changes it in a transaction (`tx:select_notes`, `tx:clear_selection`),
//! without an Undo step — like clicking in the Piano Roll. The active track,
//! the Track List's selected tracks and clips and the ruler's time selection
//! are the app's view state: `plum.selection.set_*` asks for them, reads in the
//! same run see the request, and the app applies it when the run succeeds
//! (`Host::take_ui_selection`), so the UI highlight follows.

use mlua::{Lua, Table, Value};
use plumspirit_engine::model::{ClipId, TrackId};

use super::handles::{
    Handle, LuaHandle, Resolved, current_world, handle_list, handle_or_nil, with_world,
};
use super::{Category, stop, ticks};

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// What a run asks the app to select (`None`: unchanged).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiSelection {
    pub active_track: Option<Option<TrackId>>,
    pub tracks: Option<Vec<TrackId>>,
    pub clips: Option<Vec<ClipId>>,
    pub time: Option<Option<(u64, u64)>>,
}

impl UiSelection {
    pub fn is_empty(&self) -> bool {
        *self == UiSelection::default()
    }
}

/// The run's pending UI selection request (app data).
#[derive(Default)]
pub(super) struct UiSlot(pub UiSelection);

fn arg_handle(v: &Value, what: &str) -> mlua::Result<Handle> {
    match v {
        Value::UserData(ud) => ud
            .borrow::<LuaHandle>()
            .map(|h| h.0.clone())
            .map_err(|_| runtime(format!("{what} must be a handle."))),
        other => Err(runtime(format!(
            "{what} must be a handle, not {}.",
            other.type_name()
        ))),
    }
}

fn handles_arg(v: &Value, what: &str) -> mlua::Result<Vec<Handle>> {
    match v {
        Value::Table(t) => t
            .clone()
            .sequence_values::<Value>()
            .map(|x| arg_handle(&x?, what))
            .collect(),
        Value::Nil => Ok(Vec::new()),
        other => Ok(vec![arg_handle(other, what)?]),
    }
}

fn track_of(lua: &Lua, h: &Handle) -> mlua::Result<TrackId> {
    with_world(lua, h, |_, r| match r {
        Resolved::Track { track, .. } => Ok(track.id()),
        _ => Err(runtime(format!(
            "A track handle is needed here, not a {} handle.",
            h.family_words()
        ))),
    })
}

fn clip_of(lua: &Lua, h: &Handle) -> mlua::Result<ClipId> {
    with_world(lua, h, |_, r| match r {
        Resolved::Clip(p) => Ok(p.id),
        _ => Err(runtime(format!(
            "A clip handle is needed here, not a {} handle.",
            h.family_words()
        ))),
    })
}

fn pending(lua: &Lua) -> UiSelection {
    lua.app_data_ref::<UiSlot>()
        .map(|s| s.0.clone())
        .unwrap_or_default()
}

fn edit_pending(lua: &Lua, f: impl FnOnce(&mut UiSelection)) {
    if let Some(mut s) = lua.app_data_mut::<UiSlot>() {
        f(&mut s.0);
    }
}

/// Called before a run: no request yet.
pub(super) fn begin_run(lua: &Lua) {
    edit_pending(lua, |p| *p = UiSelection::default());
}

/// Called after a run: the request, when the run succeeded and asked for one.
pub(super) fn end_run(lua: &Lua, ok: bool) -> Option<UiSelection> {
    let p = lua
        .app_data_mut::<UiSlot>()
        .map(|mut s| std::mem::take(&mut s.0))?;
    (ok && !p.is_empty()).then_some(p)
}

/// Installs `plum.selection`.
pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let s = lua.create_table()?;
    s.set(
        "active_track",
        lua.create_function(|lua, ()| {
            super::handles::require_read(lua)?;
            let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
            let id = pending(lua).active_track.unwrap_or(w.info.active_track);
            handle_or_nil(lua, id.and_then(|id| w.track(id)))
        })?,
    )?;
    s.set(
        "tracks",
        lua.create_function(|lua, ()| {
            super::handles::require_read(lua)?;
            let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
            let ids = pending(lua)
                .tracks
                .unwrap_or_else(|| w.info.selected_tracks.clone());
            handle_list(lua, ids.into_iter().filter_map(|id| w.track(id)))
        })?,
    )?;
    s.set(
        "clips",
        lua.create_function(|lua, ()| {
            super::handles::require_read(lua)?;
            let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
            let ids = pending(lua)
                .clips
                .unwrap_or_else(|| w.info.selected_clips.clone());
            handle_list(lua, ids.into_iter().filter_map(|id| w.clip(id)))
        })?,
    )?;
    s.set(
        "time",
        lua.create_function(|lua, ()| {
            super::handles::require_read(lua)?;
            let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
            match pending(lua).time.unwrap_or(w.info.time_selection) {
                Some((a, b)) => {
                    let t = lua.create_table()?;
                    t.set("start_tick", ticks::to_lua(a)?)?;
                    t.set("end_tick", ticks::to_lua(b)?)?;
                    Ok(Value::Table(t))
                }
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    s.set(
        "notes",
        lua.create_function(|lua, t: Value| {
            let h = arg_handle(&t, "The track")?;
            super::notes::host_of(lua, &h)?;
            Ok(super::notes::LuaNoteQuery {
                host: h,
                filter: super::notes::Filter {
                    selected_only: true,
                    ..super::notes::Filter::default()
                },
            })
        })?,
    )?;
    s.set(
        "analytics",
        lua.create_function(|lua, opts: Option<Table>| {
            super::handles::require_read(lua)?;
            super::timing::selection_analytics(lua, opts.as_ref())
        })?,
    )?;
    s.set(
        "set_tracks",
        lua.create_function(|lua, (tracks, opts): (Value, Option<Table>)| {
            super::reactive::check_write(lua)?;
            let ids = handles_arg(&tracks, "Each track")?
                .iter()
                .map(|h| track_of(lua, h))
                .collect::<mlua::Result<Vec<_>>>()?;
            let active = match &opts {
                Some(t) => match t.get::<Value>("active")? {
                    Value::Nil => None,
                    v => Some(track_of(lua, &arg_handle(&v, "active")?)?),
                },
                None => None,
            };
            edit_pending(lua, |p| {
                p.tracks = Some(ids);
                if let Some(a) = active {
                    p.active_track = Some(Some(a));
                }
            });
            Ok(())
        })?,
    )?;
    s.set(
        "set_active_track",
        lua.create_function(|lua, t: Value| {
            super::reactive::check_write(lua)?;
            let id = track_of(lua, &arg_handle(&t, "The track")?)?;
            edit_pending(lua, |p| p.active_track = Some(Some(id)));
            Ok(())
        })?,
    )?;
    s.set(
        "set_clips",
        lua.create_function(|lua, clips: Value| {
            super::reactive::check_write(lua)?;
            let ids = handles_arg(&clips, "Each clip")?
                .iter()
                .map(|h| clip_of(lua, h))
                .collect::<mlua::Result<Vec<_>>>()?;
            edit_pending(lua, |p| p.clips = Some(ids));
            Ok(())
        })?,
    )?;
    s.set(
        "set_time",
        lua.create_function(|lua, (a, b): (Value, Value)| {
            super::reactive::check_write(lua)?;
            let range = match (&a, &b) {
                (Value::Nil, Value::Nil) => None,
                _ => {
                    let a = ticks::from_lua(&a, "The start tick")?;
                    let b = ticks::from_lua(&b, "The end tick")?;
                    if b <= a {
                        return Err(runtime(format!(
                            "A time selection ends after it starts ({a} - {b} does not)."
                        )));
                    }
                    Some((a, b))
                }
            };
            edit_pending(lua, |p| p.time = Some(range));
            Ok(())
        })?,
    )?;
    plum.set("selection", s)?;
    Ok(())
}
