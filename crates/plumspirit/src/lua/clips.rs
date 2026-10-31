//! The Clips API (B1-0028, `docs/LUA_API.md` "Clips"): clips on the real
//! v0.9 clip model — a placement (where a content plays: track, song start,
//! the window of content ticks it shows, mute, name, color) of a content
//! (a track's own notes for a primary clip, or an owned content that linked
//! clips share).
//!
//! Reads find clips by track and time and convert between the clip's local
//! (content) ticks and song ticks; writes go through transactions to the
//! same planners the Track List uses (`plumspirit_engine::editor::clips`,
//! `clip_merge`) — one engine operation each, no second playlist model, one
//! Undo step per transaction. Placements have stable ids (handles stay valid
//! across moves and resizes; a deleted clip's handle expires).

use super::handles::{
    Handle, LuaHandle, Resolved, World, handle_list, handle_or_nil, not_for, with_world,
};
use super::ticks;
use super::{Category, stop};
use mlua::{Lua, Table, UserDataMethods, Value};
use plumspirit_engine::editor::clip_merge::{MergeKind, merge_clips, plan_merge};
use plumspirit_engine::editor::clips as planner;
use plumspirit_engine::history::Operation;
use plumspirit_engine::model::{ClipId, ClipPlacement, ContentRef, PlumDocument, TrackId};

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// A clip edit inside a transaction (built against the working copy each
/// time it is applied, so the same step gives the same operation).
#[derive(Clone, Debug)]
pub(super) enum ClipStep {
    Add {
        track: TrackId,
        start: u64,
        length: u64,
    },
    Move {
        moves: Vec<(ClipId, TrackId, u64)>,
    },
    Duplicate {
        copies: Vec<(ClipId, TrackId, u64)>,
        linked: bool,
    },
    Resize {
        id: ClipId,
        start: u64,
        offset: u64,
        length: Option<u64>,
    },
    Slice {
        ids: Vec<ClipId>,
        at: u64,
    },
    Mute {
        ids: Vec<ClipId>,
        muted: bool,
    },
    Rename {
        id: ClipId,
        name: Option<String>,
    },
    Color {
        ids: Vec<ClipId>,
        color: Option<[u8; 3]>,
    },
    Delete {
        ids: Vec<ClipId>,
    },
    Merge {
        ids: Vec<ClipId>,
        kind: MergeKind,
        dest: Option<TrackId>,
    },
}

fn index(doc: &PlumDocument, id: TrackId) -> Result<usize, String> {
    doc.index_of(id)
        .ok_or_else(|| "That track no longer exists.".to_owned())
}

/// The engine operation for `step` on `doc`.
pub(super) fn build(step: &ClipStep, doc: &PlumDocument) -> Result<Operation, String> {
    let e = |r: Result<Operation, planner::ClipEditError>| r.map_err(|e| e.0);
    match step {
        ClipStep::Add {
            track,
            start,
            length,
        } => e(planner::add_clip(doc, index(doc, *track)?, *start, *length)),
        ClipStep::Move { moves } => {
            let mut m = Vec::with_capacity(moves.len());
            for &(id, t, s) in moves {
                m.push((id, index(doc, t)?, s));
            }
            e(planner::move_clips(doc, &m))
        }
        ClipStep::Duplicate { copies, linked } => {
            let mut c = Vec::with_capacity(copies.len());
            for &(id, t, s) in copies {
                c.push((id, index(doc, t)?, s));
            }
            e(planner::duplicate_clips(doc, &c, *linked))
        }
        ClipStep::Resize {
            id,
            start,
            offset,
            length,
        } => e(planner::resize_clip(doc, *id, *start, *offset, *length)),
        ClipStep::Slice { ids, at } => e(planner::slice_clips(doc, ids, *at)),
        ClipStep::Mute { ids, muted } => e(planner::set_muted(doc, ids, *muted)),
        ClipStep::Rename { id, name } => e(planner::rename_clip(doc, *id, name.clone())),
        ClipStep::Color { ids, color } => e(planner::recolor_clips(doc, ids, *color)),
        ClipStep::Delete { ids } => e(planner::delete_clips(doc, ids)),
        ClipStep::Merge { ids, kind, dest } => {
            let dest = match dest {
                Some(t) => Some(index(doc, *t)?),
                None => None,
            };
            let plan = plan_merge(doc, ids, *kind, dest).map_err(|e| e.0)?;
            e(merge_clips(doc, &plan))
        }
    }
}

/// Notes a clip deletion removes from the project: owned contents left
/// without any placement (a primary clip's notes stay with its track).
pub(super) fn notes_removed_by_delete(doc: &PlumDocument, ids: &[ClipId]) -> u64 {
    let clips = doc.clips();
    let mut gone: Vec<plumspirit_engine::model::ContentId> = Vec::new();
    for &id in ids {
        if let Some(p) = clips.placement(id)
            && let ContentRef::Owned(c) = p.content
            && !gone.contains(&c)
            && clips
                .placements_of(p.content)
                .iter()
                .all(|q| ids.contains(&q.id))
        {
            gone.push(c);
        }
    }
    gone.iter()
        .filter_map(|&c| clips.content(c))
        .map(|c| c.notes.len() as u64)
        .sum()
}

// ------------------------------------------------------------- reads

/// The placement behind a clip handle.
pub(super) fn placement(lua: &Lua, h: &Handle) -> mlua::Result<ClipPlacement> {
    with_world(lua, h, |_, r| match r {
        Resolved::Clip(p) => Ok(p),
        _ => Err(runtime(format!(
            "A clip handle is needed here, not a {} handle.",
            h.family_words()
        ))),
    })
}

/// The song span `[start, end)` of `p`.
fn span(doc: &PlumDocument, p: &ClipPlacement) -> (u64, u64) {
    planner::clip_span(doc, p)
}

/// Clips of `placements` whose span meets `[t0, t1)` (`at`: covering it).
fn filtered(
    doc: &PlumDocument,
    placements: Vec<ClipPlacement>,
    range: Option<(u64, Option<u64>)>,
) -> Vec<ClipPlacement> {
    match range {
        None => placements,
        Some((t0, t1)) => placements
            .into_iter()
            .filter(|p| {
                let (s, e) = span(doc, p);
                e > t0 && t1.is_none_or(|t1| s < t1)
            })
            .collect(),
    }
}

/// `{ start_tick =, end_tick = }` or `{ at = }` as a range.
fn range_arg(opts: Option<&Table>) -> mlua::Result<Option<(u64, Option<u64>)>> {
    let Some(t) = opts else { return Ok(None) };
    let at = t.get::<Value>("at")?;
    if !at.is_nil() {
        let a = ticks::from_lua(&at, "at")?;
        return Ok(Some((a, Some(a.saturating_add(1)))));
    }
    let s = t.get::<Value>("start_tick")?;
    let e = t.get::<Value>("end_tick")?;
    if s.is_nil() && e.is_nil() {
        return Ok(None);
    }
    let s = if s.is_nil() {
        0
    } else {
        ticks::from_lua(&s, "start_tick")?
    };
    let e = if e.is_nil() {
        None
    } else {
        Some(ticks::from_lua(&e, "end_tick")?)
    };
    Ok(Some((s, e)))
}

fn clip_handles(lua: &Lua, w: &World, ps: Vec<ClipPlacement>) -> mlua::Result<Table> {
    handle_list(lua, ps.into_iter().filter_map(|p| w.clip(p.id)))
}

fn clip<R>(
    lua: &Lua,
    this: &LuaHandle,
    name: &str,
    f: impl FnOnce(&World, &ClipPlacement) -> mlua::Result<R>,
) -> mlua::Result<R> {
    with_world(lua, &this.0, |w, r| match r {
        Resolved::Clip(p) => f(w, &p),
        _ => Err(not_for(name, &this.0)),
    })
}

/// Registers `clips` / `clips_at` (project and track handles) and the clip
/// methods.
pub fn add_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    m.add_method("clips", |lua, this, opts: Option<Table>| {
        let range = range_arg(opts.as_ref())?;
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Track { index, .. } => {
                let ps = filtered(&w.doc, w.doc.placements_on(index), range);
                clip_handles(lua, w, ps)
            }
            Resolved::Project(d) => {
                let only = match opts.as_ref().map(|t| t.get::<Value>("track")).transpose()? {
                    Some(v) if !v.is_nil() => Some(track_index(w, &v)?),
                    _ => None,
                };
                let mut all = Vec::new();
                for i in 0..d.tracks().len() {
                    if only.is_none_or(|o| o == i) {
                        all.extend(d.placements_on(i));
                    }
                }
                let ps = filtered(&w.doc, all, range);
                clip_handles(lua, w, ps)
            }
            _ => Err(not_for("clips", &this.0)),
        })
    });
    m.add_method("clips_at", |lua, this, tick: Value| {
        let t = ticks::from_lua(&tick, "The tick")?;
        let range = Some((t, Some(t.saturating_add(1))));
        with_world(lua, &this.0, |w, r| match r {
            Resolved::Track { index, .. } => {
                let ps = filtered(&w.doc, w.doc.placements_on(index), range);
                clip_handles(lua, w, ps)
            }
            Resolved::Project(d) => {
                let mut all = Vec::new();
                for i in 0..d.tracks().len() {
                    all.extend(d.placements_on(i));
                }
                clip_handles(lua, w, filtered(&w.doc, all, range))
            }
            _ => Err(not_for("clips_at", &this.0)),
        })
    });
    m.add_method("start_tick", |lua, this, ()| {
        clip(lua, this, "start_tick", |_, p| ticks::to_lua(p.start))
    });
    m.add_method("end_tick", |lua, this, ()| {
        clip(lua, this, "end_tick", |w, p| {
            ticks::to_lua(span(&w.doc, p).1)
        })
    });
    m.add_method("length", |lua, this, ()| {
        // nil: the clip shows its content to the end (it grows with it).
        clip(lua, this, "length", |_, p| match p.length {
            Some(l) => ticks::to_lua(l).map(Value::Integer),
            None => Ok(Value::Nil),
        })
    });
    m.add_method("song_length", |lua, this, ()| {
        clip(lua, this, "song_length", |w, p| {
            let (s, e) = span(&w.doc, p);
            ticks::to_lua(e - s)
        })
    });
    m.add_method("offset", |lua, this, ()| {
        clip(lua, this, "offset", |_, p| ticks::to_lua(p.offset))
    });
    m.add_method("window", |lua, this, ()| {
        // Content ticks [from, to) the clip shows; to = nil: to the content's end.
        clip(lua, this, "window", |_, p| {
            let to = match p.length {
                Some(l) => Value::Integer(ticks::to_lua(p.offset.saturating_add(l))?),
                None => Value::Nil,
            };
            Ok((ticks::to_lua(p.offset)?, to))
        })
    });
    m.add_method("content", |lua, this, ()| {
        clip(lua, this, "content", |w, p| match p.content {
            // A primary clip shows its track's own notes: the track handle
            // reaches them (`track:notes{}`).
            ContentRef::Track(t) => handle_or_nil(lua, w.track(t)),
            ContentRef::Owned(c) => handle_or_nil(lua, w.content(c)),
        })
    });
    m.add_method("content_kind", |lua, this, ()| {
        clip(lua, this, "content_kind", |_, p| {
            Ok(match p.content {
                ContentRef::Track(_) => "track",
                ContentRef::Owned(_) => "owned",
            })
        })
    });
    m.add_method("is_primary", |lua, this, ()| {
        clip(lua, this, "is_primary", |_, p| {
            Ok(matches!(p.id, ClipId::Primary(_)))
        })
    });
    m.add_method("linked_count", |lua, this, ()| {
        // Placements showing the same content, this one included.
        clip(lua, this, "linked_count", |w, p| {
            Ok(w.doc.clips().placements_of(p.content).len() as i64)
        })
    });
    m.add_method("linked_clips", |lua, this, ()| {
        clip(lua, this, "linked_clips", |w, p| {
            let others: Vec<ClipPlacement> = w
                .doc
                .clips()
                .placements_of(p.content)
                .into_iter()
                .filter(|q| q.id != p.id)
                .collect();
            clip_handles(lua, w, others)
        })
    });
    m.add_method("to_song", |lua, this, local: Value| {
        let c = ticks::from_lua(&local, "The content tick")?;
        clip(lua, this, "to_song", |_, p| match p.song_tick(c) {
            Some(t) => ticks::to_lua(t),
            None => Err(runtime(format!(
                "Content tick {c} is before this clip's window (it starts at content tick {}).",
                p.offset
            ))),
        })
    });
    m.add_method("to_local", |lua, this, song: Value| {
        let s = ticks::from_lua(&song, "The song tick")?;
        clip(lua, this, "to_local", |_, p| match s.checked_sub(p.start) {
            Some(d) => ticks::to_lua(p.offset.checked_add(d).ok_or_else(|| {
                runtime("That tick is past the largest tick PlumSpirit can hold.")
            })?),
            None => Err(runtime(format!(
                "Song tick {s} is before this clip (it starts at tick {}).",
                p.start
            ))),
        })
    });
    m.add_method("contains", |lua, this, song: Value| {
        let s = ticks::from_lua(&song, "The song tick")?;
        clip(lua, this, "contains", |w, p| {
            let (a, b) = span(&w.doc, p);
            Ok(s >= a && s < b)
        })
    });
}

/// A track handle value as its row index in `w`.
fn track_index(w: &World, v: &Value) -> mlua::Result<usize> {
    let h = match v {
        Value::UserData(ud) => ud
            .borrow::<LuaHandle>()
            .map(|h| h.0)
            .map_err(|_| runtime("track must be a track handle."))?,
        _ => return Err(runtime("track must be a track handle.")),
    };
    match h.resolve(w) {
        Ok(Resolved::Track { index, .. }) => Ok(index),
        Ok(_) => Err(runtime("track must be a track handle.")),
        Err(why) => Err(stop(Category::Expired, why)),
    }
}

/// The content handle and content-tick window `[from, to)` of a clip handle
/// (`clip:notes{}` queries the content inside the window).
pub(super) fn notes_window(
    lua: &Lua,
    h: &Handle,
) -> mlua::Result<Option<(Handle, u64, Option<u64>)>> {
    with_world(lua, h, |w, r| match r {
        Resolved::Clip(p) => {
            let content = match p.content {
                ContentRef::Track(t) => w.track(t),
                ContentRef::Owned(c) => w.content(c),
            }
            .ok_or_else(|| runtime("This clip's content no longer exists."))?;
            let to = p.length.map(|l| p.offset.saturating_add(l));
            Ok(Some((content, p.offset, to)))
        }
        _ => Ok(None),
    })
}

// ------------------------------------------------------- transactions

/// A clip handle argument (or a list of them) as ids.
pub(super) fn clip_ids(lua: &Lua, v: &Value, what: &str) -> mlua::Result<Vec<ClipId>> {
    let one = |v: &Value| -> mlua::Result<ClipId> {
        let h = super::tx::handle_arg(v, what)?;
        placement(lua, &h).map(|p| p.id)
    };
    match v {
        Value::Table(t) => {
            let mut out = Vec::new();
            for item in t.clone().sequence_values::<Value>() {
                out.push(one(&item?)?);
            }
            if out.is_empty() {
                return Err(runtime(format!("{what}: the list is empty.")));
            }
            Ok(out)
        }
        other => Ok(vec![one(other)?]),
    }
}

/// The handles of the clips the last step created (placed ids from `first`).
pub(super) fn new_clips(lua: &Lua, first: u32) -> mlua::Result<Table> {
    super::tx::read(lua, |w| {
        let (next, _) = w.doc.clips().next_ids();
        handle_list(lua, (first..next).filter_map(|n| w.clip(ClipId::Placed(n))))
    })
}

/// The block span `[start, end)` of `ids` (Ctrl+D duplicates after it).
pub(super) fn block_span(doc: &PlumDocument, ids: &[ClipId]) -> (u64, u64) {
    let mut s = u64::MAX;
    let mut e = 0;
    for &id in ids {
        if let Some(p) = doc.clips().placement(id) {
            let (a, b) = span(doc, &p);
            s = s.min(a);
            e = e.max(b);
        }
    }
    if s == u64::MAX { (0, 0) } else { (s, e) }
}

/// Parses `mode` of `merge_clips`.
pub(super) fn merge_kind(mode: Option<&str>) -> mlua::Result<MergeKind> {
    match mode.unwrap_or("sequence") {
        "sequence" => Ok(MergeKind::Sequence),
        "stacked" | "stack" => Ok(MergeKind::Stack),
        other => Err(runtime(format!(
            "mode is \"sequence\" or \"stacked\", not \"{other}\"."
        ))),
    }
}

/// A color argument: `{ r =, g =, b = }`, `{ r, g, b }` or nil (the track's).
pub(super) fn color_arg(v: &Value) -> mlua::Result<Option<[u8; 3]>> {
    let t = match v {
        Value::Nil => return Ok(None),
        Value::Table(t) => t,
        other => {
            return Err(runtime(format!(
                "A color is a table {{ r =, g =, b = }} (0 - 255) or nil, not {}.",
                other.type_name()
            )));
        }
    };
    let mut c = [0u8; 3];
    for (i, k) in ["r", "g", "b"].into_iter().enumerate() {
        let v = match t.get::<Value>(k)? {
            Value::Nil => t.raw_get::<Value>(i + 1)?,
            v => v,
        };
        let n = match v {
            Value::Integer(n) => n,
            Value::Number(f) if f.fract() == 0.0 => f as i64,
            _ => return Err(runtime(format!("The color's {k} must be a whole number."))),
        };
        c[i] = u8::try_from(n)
            .map_err(|_| runtime(format!("The color's {k} is 0 - 255 (it is {n}).")))?;
    }
    Ok(Some(c))
}

/// The placement of `id` in the current (working) world.
pub(super) fn placement_of(lua: &Lua, id: ClipId) -> mlua::Result<ClipPlacement> {
    super::tx::read(lua, |w| {
        w.doc
            .clips()
            .placement(id)
            .ok_or_else(|| runtime("That clip no longer exists."))
    })
}

/// A move / duplicate destination start: `start =` (song tick) or `shift =`
/// (signed ticks) in `t`, else `current`.
fn target_start(t: &Table, current: u64) -> mlua::Result<u64> {
    let start = t.get::<Value>("start")?;
    if !start.is_nil() {
        return ticks::from_lua(&start, "start");
    }
    let shift = t.get::<Value>("shift")?;
    if shift.is_nil() {
        return Ok(current);
    }
    shifted(current, &shift)
}

/// `start` moved by a signed tick amount (an integer or a decimal string).
fn shifted(start: u64, shift: &Value) -> mlua::Result<u64> {
    let (neg, mag) = match shift {
        Value::Integer(n) => (*n < 0, n.unsigned_abs()),
        Value::String(s) => {
            let s = s.to_string_lossy();
            let (neg, digits) = match s.strip_prefix('-') {
                Some(d) => (true, d.to_owned()),
                None => (false, s.clone()),
            };
            (neg, ticks::parse_decimal(&digits, "shift")?)
        }
        Value::Number(f) if f.fract() == 0.0 && f.abs() < 9.007e15 => (*f < 0.0, f.abs() as u64),
        other => {
            return Err(runtime(format!(
                "shift must be a whole number of ticks, not {}.",
                other.type_name()
            )));
        }
    };
    if neg {
        start
            .checked_sub(mag)
            .ok_or_else(|| runtime("That would move the clip before the song's start."))
    } else {
        start
            .checked_add(mag)
            .ok_or_else(|| runtime("A clip cannot be placed that far."))
    }
}

/// The track a `{ track = h }` entry names, else `current`.
fn target_track(lua: &Lua, t: &Table, current: TrackId) -> mlua::Result<TrackId> {
    match t.get::<Value>("track")? {
        Value::Nil => Ok(current),
        v => super::tx::track_id(lua, &super::tx::handle_arg(&v, "track")?),
    }
}

/// Registers the clip writes on transactions (capability `tracks.edit`).
pub(super) fn add_tx_methods<M: UserDataMethods<super::tx::LuaTx>>(m: &mut M) {
    use super::tx::{Step, apply, handle_arg, read, track_id};
    m.add_method(
        "add_clip",
        |lua, this, (track, start, length): (Value, Value, Value)| {
            let track = track_id(lua, &handle_arg(&track, "The track")?)?;
            let start = ticks::from_lua(&start, "start")?;
            let length = ticks::from_lua(&length, "length")?;
            let first = read(lua, |w| Ok(w.doc.clips().next_ids().0))?;
            apply(
                lua,
                this.0,
                Step::Clips(ClipStep::Add {
                    track,
                    start,
                    length,
                }),
                0,
            )?;
            new_clips(lua, first)?.raw_get::<Value>(1)
        },
    );
    m.add_method("move_clips", |lua, this, (cs, opts): (Value, Table)| {
        // Every clip moves by the same amount (`shift =`), or the block's
        // first clip goes to `start =` and the others keep their distance;
        // `track =` puts them all on one track.
        let ids = clip_ids(lua, &cs, "Each clip")?;
        let (bs, _) = read(lua, |w| Ok(block_span(&w.doc, &ids)))?;
        let to = target_start(&opts, bs)?;
        let mut moves = Vec::with_capacity(ids.len());
        for &id in &ids {
            let p = placement_of(lua, id)?;
            let start = (p.start - bs)
                .checked_add(to)
                .ok_or_else(|| runtime("A clip cannot be placed that far."))?;
            moves.push((id, target_track(lua, &opts, p.track)?, start));
        }
        apply(lua, this.0, Step::Clips(ClipStep::Move { moves }), 0)
    });
    m.add_method(
        "duplicate_clips",
        |lua, this, (cs, opts): (Value, Option<Table>)| {
            // linked = true (default): more placements of the same content;
            // false: independent copies. Default place: right after the
            // block, like Ctrl+D; or `start =` / `shift =`, `track =`.
            let ids = clip_ids(lua, &cs, "Each clip")?;
            let (bs, be) = read(lua, |w| Ok(block_span(&w.doc, &ids)))?;
            let empty = lua.create_table()?;
            let o = opts.as_ref().unwrap_or(&empty);
            let linked = o.get::<Option<bool>>("linked")?.unwrap_or(true);
            let to = target_start(o, be)?;
            let mut copies = Vec::with_capacity(ids.len());
            for &id in &ids {
                let p = placement_of(lua, id)?;
                let start = (p.start - bs)
                    .checked_add(to)
                    .ok_or_else(|| runtime("A clip cannot be placed that far."))?;
                copies.push((id, target_track(lua, o, p.track)?, start));
            }
            let first = read(lua, |w| Ok(w.doc.clips().next_ids().0))?;
            apply(
                lua,
                this.0,
                Step::Clips(ClipStep::Duplicate { copies, linked }),
                0,
            )?;
            new_clips(lua, first)
        },
    );
    m.add_method("resize_clip", |lua, this, (c, opts): (Value, Table)| {
        // start = trims / extends the left edge with the notes kept where
        // they play (offset follows); offset = sets the window's first content
        // tick; length = / end_tick = / open_ended = true set the right edge.
        let id = clip_ids(lua, &c, "The clip")?[0];
        let p = placement_of(lua, id)?;
        let (_, cur_e) = read(lua, |w| Ok(span(&w.doc, &p)))?;
        let get = |k: &str| -> mlua::Result<Option<u64>> {
            match opts.get::<Value>(k)? {
                Value::Nil => Ok(None),
                v => ticks::from_lua(&v, k).map(Some),
            }
        };
        let (start, offset) = match (get("start")?, get("offset")?) {
            (Some(s), None) => {
                let off = if s >= p.start {
                    p.offset.checked_add(s - p.start)
                } else {
                    p.offset.checked_sub(p.start - s)
                }
                .ok_or_else(|| runtime("The clip cannot start before its content's first tick."))?;
                (s, off)
            }
            (s, o) => (s.unwrap_or(p.start), o.unwrap_or(p.offset)),
        };
        let length = if opts.get::<Option<bool>>("open_ended")?.unwrap_or(false) {
            None
        } else if let Some(l) = get("length")? {
            Some(l)
        } else if let Some(e) = get("end_tick")? {
            Some(
                e.checked_sub(start)
                    .filter(|&l| l > 0)
                    .ok_or_else(|| runtime("end_tick must be after the clip's start."))?,
            )
        } else if p.length.is_none() && start == p.start && offset == p.offset {
            None
        } else {
            // The right edge stays where it is.
            Some(
                cur_e
                    .checked_sub(start)
                    .filter(|&l| l > 0)
                    .ok_or_else(|| runtime("The clip would end before it starts."))?,
            )
        };
        apply(
            lua,
            this.0,
            Step::Clips(ClipStep::Resize {
                id,
                start,
                offset,
                length,
            }),
            0,
        )
    });
    m.add_method("slice_clips", |lua, this, (cs, at): (Value, Value)| {
        let ids = clip_ids(lua, &cs, "Each clip")?;
        let at = ticks::from_lua(&at, "at")?;
        let first = read(lua, |w| Ok(w.doc.clips().next_ids().0))?;
        apply(lua, this.0, Step::Clips(ClipStep::Slice { ids, at }), 0)?;
        // The right parts (the left parts keep their handles).
        new_clips(lua, first)
    });
    m.add_method(
        "set_muted",
        |lua, this, (cs, muted): (Value, Option<bool>)| {
            let ids = clip_ids(lua, &cs, "Each clip")?;
            apply(
                lua,
                this.0,
                Step::Clips(ClipStep::Mute {
                    ids,
                    muted: muted.unwrap_or(true),
                }),
                0,
            )
        },
    );
    m.add_method(
        "rename_clip",
        |lua, this, (c, name): (Value, Option<String>)| {
            let id = clip_ids(lua, &c, "The clip")?[0];
            apply(lua, this.0, Step::Clips(ClipStep::Rename { id, name }), 0)
        },
    );
    m.add_method("recolor_clips", |lua, this, (cs, color): (Value, Value)| {
        let ids = clip_ids(lua, &cs, "Each clip")?;
        let color = color_arg(&color)?;
        apply(lua, this.0, Step::Clips(ClipStep::Color { ids, color }), 0)
    });
    m.add_method("delete_clips", |lua, this, cs: Value| {
        let ids = clip_ids(lua, &cs, "Each clip")?;
        let removed = read(lua, |w| Ok(notes_removed_by_delete(&w.doc, &ids)))?;
        apply(lua, this.0, Step::Clips(ClipStep::Delete { ids }), removed)
    });
    m.add_method(
        "merge_clips",
        |lua, this, (cs, opts): (Value, Option<Table>)| {
            let ids = clip_ids(lua, &cs, "Each clip")?;
            let (mode, dest) = match &opts {
                Some(t) => (t.get::<Option<String>>("mode")?, t.get::<Value>("track")?),
                None => (None, Value::Nil),
            };
            let kind = merge_kind(mode.as_deref())?;
            let dest = match dest {
                Value::Nil => None,
                v => Some(track_id(lua, &handle_arg(&v, "track")?)?),
            };
            let first = read(lua, |w| Ok(w.doc.clips().next_ids().0))?;
            apply(
                lua,
                this.0,
                Step::Clips(ClipStep::Merge { ids, kind, dest }),
                0,
            )?;
            new_clips(lua, first)?.raw_get::<Value>(1)
        },
    );
}
