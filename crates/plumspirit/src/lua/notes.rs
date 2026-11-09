//! The Notes API (v1.0-beta1 B1-0029, `docs/LUA_API.md` "Notes").
//!
//! `t:notes{...}` (a track's own notes) and `c:notes{...}` (a clip content's)
//! make a **query**: a filter evaluated when it is used, against the run's
//! current world (the transaction's working copy while one is open). Reads
//! stream through an immutable snapshot of the notes (shared chunks, no copy):
//! `q:count()` counts exactly, `q:batches(n)` hands out struct-of-arrays
//! tables of up to `n` notes, `q:each()` one note table at a time,
//! `q:handles(n)` note handles. Native loops check the run's cancellation and
//! time budget every [`CHECK_NOTES`] notes. Writes take a query (`lua/tx.rs`).

use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::model::{
    ContentId, ContentRef, NoteRecord, NoteStore, PlumDocument, SubchannelId, TrackId,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::handles::{Handle, LuaHandle, Resolved, World, current_world, with_world};
use super::{Category, stop, ticks};

/// Notes between two budget / cancellation checks in native loops.
pub const CHECK_NOTES: usize = 65_536;

/// Batch size when a script gives none.
pub const DEFAULT_BATCH: usize = 8192;

/// Largest batch a script may ask for.
pub const MAX_BATCH: usize = 1 << 20;

/// The run's cancellation flag and deadline, for native loops (app data).
pub(super) struct Budget {
    pub cancel: Arc<AtomicBool>,
    pub deadline: Arc<AtomicU64>,
    pub epoch: Instant,
    pub seconds: f64,
    /// Notes one run's transactions may create.
    pub max_output_notes: u64,
}

/// Fails with the `budget` category when `more` notes would take the run's
/// transactions past their output budget (`already`: created so far).
pub fn check_output(lua: &Lua, already: u64, more: u64) -> mlua::Result<()> {
    let mut max = lua
        .app_data_ref::<Budget>()
        .map_or(u64::MAX, |b| b.max_output_notes);
    if super::reactive::responding(lua) {
        max = max.min(super::response::MAX_NOTES);
    }
    if already.saturating_add(more) > max {
        return Err(stop(
            Category::Budget,
            format!(
                "This would create {} notes, more than the {max} a script may create in one run; nothing was changed.",
                already.saturating_add(more)
            ),
        ));
    }
    Ok(())
}

/// Fails with `canceled` / `time_limit` when the run must stop.
pub fn check_budget(lua: &Lua) -> mlua::Result<()> {
    let Some(b) = lua.app_data_ref::<Budget>() else {
        return Ok(());
    };
    if b.cancel.load(Ordering::Relaxed) {
        return Err(stop(Category::Canceled, "the script was canceled"));
    }
    let d = b.deadline.load(Ordering::Relaxed);
    let now = u64::try_from(b.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
    if d != 0 && now > d {
        return Err(stop(
            Category::TimeLimit,
            format!(
                "the script ran longer than {:.1} s and was stopped",
                b.seconds
            ),
        ));
    }
    Ok(())
}

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// Which notes a query selects.
#[derive(Clone, Debug)]
pub struct Filter {
    /// First start tick (inclusive).
    pub t0: u64,
    /// End of the start range (exclusive); `None`: to the end.
    pub t1: Option<u64>,
    pub keys: (u8, u8),
    pub channel: Option<u8>,
    pub subchannel: Option<u32>,
    pub velocity: (u8, u8),
    pub selected_only: bool,
    pub group: Option<u32>,
    pub muted: Option<bool>,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            t0: 0,
            t1: None,
            keys: (0, 255),
            channel: None,
            subchannel: None,
            velocity: (1, 127),
            selected_only: false,
            group: None,
            muted: None,
        }
    }
}

impl Filter {
    /// Only the start range: counts come from chunk lengths.
    fn range_only(&self) -> bool {
        self.group.is_none()
            && self.muted.is_none()
            && self.keys == (0, 255)
            && self.channel.is_none()
            && self.subchannel.is_none()
            && self.velocity == (1, 127)
            && !self.selected_only
    }

    pub(super) fn accepts(&self, r: &NoteRecord) -> bool {
        let a = r.attrs;
        let (k, v) = (a.pitch(), a.velocity());
        self.group.is_none_or(|g| r.group == g)
            && self.muted.is_none_or(|m| a.muted() == m)
            && k >= self.keys.0
            && k <= self.keys.1
            && v >= self.velocity.0
            && v <= self.velocity.1
            && self.channel.is_none_or(|c| a.channel() == c)
            && self.subchannel.is_none_or(|s| r.sub.0 == s)
    }

    /// From a script's table (`nil`: every note).
    pub fn from_lua(t: Option<&Table>) -> mlua::Result<Filter> {
        let mut f = Filter::default();
        let Some(t) = t else {
            return Ok(f);
        };
        let tick = |name: &str| -> mlua::Result<Option<u64>> {
            match t.get::<Value>(name)? {
                Value::Nil => Ok(None),
                v => ticks::from_lua(&v, name).map(Some),
            }
        };
        if let Some(t0) = tick("start_tick")? {
            f.t0 = t0;
        }
        f.t1 = tick("end_tick")?;
        let pair = |name: &str, lo: i64, hi: i64| -> mlua::Result<Option<(u8, u8)>> {
            match t.get::<Value>(name)? {
                Value::Nil => Ok(None),
                Value::Integer(k) => {
                    if k < lo || k > hi {
                        return Err(runtime(format!("{name} must be {lo} - {hi}; {k} is not.")));
                    }
                    Ok(Some((k as u8, k as u8)))
                }
                Value::Table(p) => {
                    let a: i64 = p.get(1)?;
                    let b: i64 = p.get(2)?;
                    if a < lo || b > hi || a > b {
                        return Err(runtime(format!(
                            "{name} must be {{low, high}} within {lo} - {hi}; {{{a}, {b}}} is not."
                        )));
                    }
                    Ok(Some((a as u8, b as u8)))
                }
                other => Err(runtime(format!(
                    "{name} must be a number or {{low, high}}, not {}.",
                    other.type_name()
                ))),
            }
        };
        if let Some(k) = pair("keys", 0, 255)? {
            f.keys = k;
        }
        if let Some(v) = pair("velocity", 1, 127)? {
            f.velocity = v;
        }
        if let Some(c) = t.get::<Option<i64>>("channel")? {
            f.channel = Some(
                u8::try_from(c)
                    .ok()
                    .filter(|&c| c <= 15)
                    .ok_or_else(|| runtime(format!("channel must be 0 - 15; {c} is not.")))?,
            );
        }
        if let Some(s) = t.get::<Option<i64>>("subchannel")? {
            f.subchannel = Some(u32::try_from(s).map_err(|_| {
                runtime(format!("subchannel must be a subchannel id; {s} is not."))
            })?);
        }
        f.group = t
            .raw_get::<Option<i64>>("group")?
            .map(|g| u32::try_from(g).map_err(|_| runtime("group must be a nonnegative group ID.")))
            .transpose()?;
        f.muted = match t.raw_get::<Value>("muted")? {
            Value::Nil => None,
            Value::Boolean(b) => Some(b),
            _ => return Err(runtime("muted must be a boolean.")),
        };
        f.selected_only = t.get::<Option<bool>>("selected")?.unwrap_or(false);
        Ok(f)
    }
}

/// The notes a query reads: a track's own notes or a clip content's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    /// The track (for a content: a track one of its clips is on, whose
    /// subchannel table interprets the content's subchannel ids).
    pub track: TrackId,
    pub content: Option<ContentId>,
}

impl Host {
    pub fn content_ref(self) -> ContentRef {
        match self.content {
            Some(c) => ContentRef::Owned(c),
            None => ContentRef::Track(self.track),
        }
    }

    /// The engine's edit target in `doc`.
    pub fn target(self, doc: &PlumDocument) -> Result<NoteTarget, String> {
        let host = doc
            .index_of(self.track)
            .ok_or_else(|| "That track no longer exists.".to_owned())?;
        Ok(match self.content {
            None => NoteTarget::Track(host),
            Some(content) => NoteTarget::Content { host, content },
        })
    }

    pub fn notes(self, doc: &PlumDocument) -> Option<&NoteStore> {
        doc.notes_of(self.content_ref())
    }
}

/// The host of a track or clip content handle.
pub fn host_of(lua: &Lua, h: &Handle) -> mlua::Result<Host> {
    with_world(lua, h, |w, r| match r {
        Resolved::Track { track, .. } => Ok(Host {
            track: track.id(),
            content: None,
        }),
        Resolved::Content(c) => {
            let track = h
                .content_host()
                .or_else(|| {
                    w.doc
                        .clips()
                        .explicit_placements()
                        .iter()
                        .find(|p| p.content == ContentRef::Owned(c.id))
                        .map(|p| p.track)
                })
                .ok_or_else(|| runtime("This clip content is on no track."))?;
            Ok(Host {
                track,
                content: Some(c.id),
            })
        }
        _ => Err(runtime(format!(
            "notes() is available on track and clip content handles, not on a {} handle.",
            h.family_words()
        ))),
    })
}

/// A matching note: its rank (0-based position in stored order), values and
/// selection.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub rank: usize,
    pub rec: NoteRecord,
    pub selected: bool,
}

/// A resumable walk over the notes a filter selects, on an immutable
/// snapshot (shared chunks).
pub struct Cursor {
    snap: plumspirit_engine::model::NoteSnapshot,
    pos: plumspirit_engine::model::note_store::SnapshotPos,
    end: Option<plumspirit_engine::model::note_store::SnapshotPos>,
    rank: usize,
    /// Selected ranks as (start, len) runs, and the run being looked at.
    selected: Vec<(usize, usize)>,
    run: usize,
    filter: Filter,
    /// The notes' state the ranks belong to.
    pub content_revision: u64,
    _native: Option<super::bridge::Reservation>,
}

impl Cursor {
    pub fn new(store: &NoteStore, filter: Filter) -> Cursor {
        let snap = store.snapshot();
        let pos = snap.lower_bound(filter.t0);
        let end = filter.t1.map(|t1| snap.lower_bound(t1.max(filter.t0)));
        let rank = snap.count_in(0, filter.t0);
        let selected = if filter.selected_only {
            store.selected_ranks().runs().collect()
        } else {
            store_selected_runs(store)
        };
        Cursor {
            snap,
            pos,
            end,
            rank,
            selected,
            run: 0,
            filter,
            content_revision: store.content_revision(),
            _native: None,
        }
    }

    fn accounted(lua: &Lua, store: &NoteStore, filter: Filter) -> mlua::Result<Self> {
        if !super::reactive::responding(lua) {
            return Ok(Self::new(store, filter));
        }
        check_budget(lua)?;
        let bytes = store
            .chunk_count()
            .saturating_mul(32)
            .saturating_add(store.selected_count().saturating_mul(64))
            .saturating_add(16 * 1024);
        let native = lua
            .app_data_ref::<super::bridge::NativeBudget>()
            .ok_or_else(|| stop(Category::Internal, "Native cursor budget is unavailable."))?
            .reserve(bytes)
            .map_err(|e| e.lua())?;
        let mut cursor = Self::new(store, filter);
        cursor._native = Some(native);
        check_budget(lua)?;
        Ok(cursor)
    }
    fn is_selected(&mut self, rank: usize) -> bool {
        while let Some(&(s, l)) = self.selected.get(self.run) {
            if rank < s {
                return false;
            }
            if rank < s + l {
                return true;
            }
            self.run += 1;
        }
        false
    }

    /// The next matching note, visiting at most `budget` notes (`Err(())`
    /// when the budget ran out first: call again).
    pub fn next_hit(&mut self, budget: &mut usize) -> Result<Option<Hit>, ()> {
        loop {
            if self.end == Some(self.pos) {
                return Ok(None);
            }
            if *budget == 0 {
                return Err(());
            }
            *budget -= 1;
            let Some((rec, next)) = self.snap.next_at(self.pos) else {
                return Ok(None);
            };
            self.pos = next;
            let rank = self.rank;
            self.rank += 1;
            if !self.filter.accepts(&rec) {
                continue;
            }
            let selected = self.is_selected(rank);
            if self.filter.selected_only && !selected {
                continue;
            }
            return Ok(Some(Hit {
                rank,
                rec,
                selected,
            }));
        }
    }
}

/// The selected ranks of a store as runs (for the `selected` field).
fn store_selected_runs(store: &NoteStore) -> Vec<(usize, usize)> {
    if store.selected_count() == 0 {
        Vec::new()
    } else {
        store.selected_ranks().runs().collect()
    }
}

/// Only automatic iterators are job-scoped. Legacy API01 iterators retain
/// their established semantics. Weak entries never retain a cursor themselves.
#[derive(Default)]
struct AutomaticCursors(Vec<std::sync::Weak<Mutex<Option<Cursor>>>>);
pub(super) fn end_automatic_run(lua: &Lua) {
    if let Some(mut cursors) = lua.app_data_mut::<AutomaticCursors>() {
        for weak in cursors.0.drain(..) {
            if let Some(state) = weak.upgrade()
                && let Ok(mut cursor) = state.lock()
            {
                *cursor = None;
            }
        }
    }
}
fn register_automatic_cursor(lua: &Lua, state: &Arc<Mutex<Option<Cursor>>>) -> mlua::Result<()> {
    if !super::reactive::responding(lua) {
        return Ok(());
    }
    if lua.app_data_ref::<AutomaticCursors>().is_none() {
        lua.set_app_data(AutomaticCursors::default());
    }
    let mut cursors = lua
        .app_data_mut::<AutomaticCursors>()
        .expect("automatic cursor registry");
    if cursors.0.len() >= 32 {
        return Err(stop(
            Category::Budget,
            "An automatic response may create at most 32 note iterators.",
        ));
    }
    cursors.0.push(Arc::downgrade(state));
    Ok(())
}

/// Every note `filter` selects in `store` (budget-checked).
pub fn collect(lua: &Lua, store: &NoteStore, filter: &Filter) -> mlua::Result<Vec<Hit>> {
    let mut c = Cursor::accounted(lua, store, filter.clone())?;
    let mut out = Vec::new();
    loop {
        let mut budget = CHECK_NOTES;
        loop {
            match c.next_hit(&mut budget) {
                Ok(Some(h)) => {
                    if super::reactive::responding(lua)
                        && out.len() >= super::response::MAX_NOTES as usize
                    {
                        return Err(stop(
                            Category::Budget,
                            "An automatic note mutation query may match at most 100,000 notes.",
                        ));
                    }
                    out.push(h);
                }
                Ok(None) => return Ok(out),
                Err(()) => break,
            }
        }
        check_budget(lua)?;
    }
}

/// Exact count of the notes `filter` selects.
pub fn count(lua: &Lua, store: &NoteStore, filter: &Filter) -> mlua::Result<usize> {
    if filter.range_only() {
        let snap = store.snapshot();
        return Ok(match filter.t1 {
            Some(t1) => snap.count_in(filter.t0, t1),
            None => store.len() - snap.count_in(0, filter.t0),
        });
    }
    let mut c = Cursor::accounted(lua, store, filter.clone())?;
    let mut n = 0;
    loop {
        let mut budget = CHECK_NOTES;
        loop {
            match c.next_hit(&mut budget) {
                Ok(Some(_)) => n += 1,
                Ok(None) => return Ok(n),
                Err(()) => break,
            }
        }
        check_budget(lua)?;
    }
}

/// A note as a script table.
pub fn note_table(lua: &Lua, h: &Hit) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let a = h.rec.attrs;
    t.set("start", ticks::to_lua(h.rec.start_tick)?)?;
    t.set("gate", ticks::to_lua(h.rec.duration_ticks)?)?;
    t.set("key", i64::from(a.pitch()))?;
    t.set("velocity", i64::from(a.velocity()))?;
    t.set("channel", i64::from(a.channel()))?;
    t.set("subchannel", i64::from(h.rec.sub.0))?;
    t.set("selected", h.selected)?;
    t.set("index", h.rank as i64 + 1)?;
    Ok(t)
}

/// A query as Lua userdata.
pub struct LuaNoteQuery {
    pub host: Handle,
    pub filter: Filter,
}

/// The query's host and notes in the current world.
fn resolve<R>(
    lua: &Lua,
    q: &LuaNoteQuery,
    f: impl FnOnce(&World, Host, &NoteStore) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let host = host_of(lua, &q.host)?;
    let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
    let store = host
        .notes(&w.doc)
        .ok_or_else(|| runtime("Those notes no longer exist."))?;
    f(&w, host, store)
}

/// The hits of a query in the current world, with its host (writes use it).
pub fn evaluate(lua: &Lua, q: &LuaNoteQuery) -> mlua::Result<(Host, Vec<Hit>)> {
    resolve(lua, q, |_, host, store| {
        Ok((host, collect(lua, store, &q.filter)?))
    })
}

impl UserData for LuaNoteQuery {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("count", |lua, this, ()| {
            resolve(lua, this, |_, _, store| {
                Ok(count(lua, store, &this.filter)? as i64)
            })
        });
        m.add_method("batches", |lua, this, arg: Value| {
            // `q:batches(n)` or `q:batches{size = n, columns = {...}}`.
            let (size, columns) = match &arg {
                Value::Nil => (None, Columns::ALL),
                Value::Integer(n) => (Some(*n), Columns::ALL),
                Value::Table(t) => (
                    t.get::<Option<i64>>("size")?,
                    match t.get::<Option<Table>>("columns")? {
                        Some(c) => Columns::parse(&c)?,
                        None => Columns::ALL,
                    },
                ),
                other => {
                    return Err(runtime(format!(
                        "batches takes a size or {{size = n, columns = {{...}}}}, not {}.",
                        other.type_name()
                    )));
                }
            };
            let size = match size {
                None => DEFAULT_BATCH,
                Some(n) => usize::try_from(n)
                    .ok()
                    .filter(|&n| (1..=MAX_BATCH).contains(&n))
                    .ok_or_else(|| {
                        runtime(format!("A batch holds 1 - {MAX_BATCH} notes; {n} is not."))
                    })?,
            };
            let host = host_of(lua, &this.host)?;
            let cursor = resolve(lua, this, |_, _, store| {
                Cursor::accounted(lua, store, this.filter.clone())
            })?;
            let state = Arc::new(Mutex::new(Some(cursor)));
            register_automatic_cursor(lua, &state)?;
            lua.create_function(move |lua, ()| {
                let mut guard = state
                    .lock()
                    .map_err(|_| stop(Category::Internal, "the note iterator failed"))?;
                let Some(c) = guard.as_mut() else {
                    return Ok(Value::Nil);
                };
                let mut hits = Vec::with_capacity(size.min(DEFAULT_BATCH));
                let mut done = false;
                while hits.len() < size {
                    let mut budget = CHECK_NOTES;
                    loop {
                        if hits.len() == size {
                            break;
                        }
                        match c.next_hit(&mut budget) {
                            Ok(Some(h)) => hits.push(h),
                            Ok(None) => {
                                done = true;
                                break;
                            }
                            Err(()) => break,
                        }
                    }
                    if done {
                        break;
                    }
                    check_budget(lua)?;
                }
                if done {
                    *guard = None;
                }
                if hits.is_empty() {
                    return Ok(Value::Nil);
                }
                drop(guard);
                batch_table(lua, &hits, columns, host).map(Value::Table)
            })
        });
        m.add_method("each", |lua, this, opts: Option<Table>| {
            let columns = match opts
                .as_ref()
                .map(|t| t.get::<Option<Table>>("columns"))
                .transpose()?
                .flatten()
            {
                Some(c) => Some(Columns::parse(&c)?),
                None => None,
            };
            let host = host_of(lua, &this.host)?;
            let cursor = resolve(lua, this, |_, _, store| {
                Cursor::accounted(lua, store, this.filter.clone())
            })?;
            let state = Arc::new(Mutex::new(Some(cursor)));
            register_automatic_cursor(lua, &state)?;
            lua.create_function(move |lua, ()| {
                let mut guard = state
                    .lock()
                    .map_err(|_| stop(Category::Internal, "the note iterator failed"))?;
                let Some(c) = guard.as_mut() else {
                    return Ok(Value::Nil);
                };
                loop {
                    let mut budget = CHECK_NOTES;
                    match c.next_hit(&mut budget) {
                        Ok(Some(h)) => {
                            drop(guard);
                            if let Some(columns) = columns {
                                let batch = batch_table(lua, &[h], columns, host)?;
                                let row = lua.create_table()?;
                                for (i, name) in Columns::NAMES.iter().enumerate() {
                                    if columns.has(i) {
                                        let col: Table = batch.raw_get(*name)?;
                                        row.raw_set(*name, col.raw_get::<Value>(1)?)?;
                                    }
                                }
                                return Ok(Value::Table(row));
                            }
                            return note_table(lua, &h).map(Value::Table);
                        }
                        Ok(None) => {
                            *guard = None;
                            return Ok(Value::Nil);
                        }
                        Err(()) => check_budget(lua)?,
                    }
                }
            })
        });
        m.add_method("handles", |lua, this, limit: Option<i64>| {
            let limit = limit.unwrap_or(100_000).max(0) as usize;
            resolve(lua, this, |w, host, store| {
                let mut c = Cursor::accounted(lua, store, this.filter.clone())?;
                // Ranks → places: walk the store once in order.
                let mut ranks = Vec::new();
                while ranks.len() < limit {
                    let mut budget = CHECK_NOTES;
                    match c.next_hit(&mut budget) {
                        Ok(Some(h)) => ranks.push(h.rank),
                        Ok(None) => break,
                        Err(()) => check_budget(lua)?,
                    }
                }
                let mut out = Vec::with_capacity(ranks.len());
                let mut want = ranks.iter().peekable();
                for (i, (loc, _)) in store.iter_with_loc().enumerate() {
                    let Some(&&r) = want.peek() else { break };
                    if i == r {
                        out.extend(w.note(host.content_ref(), loc));
                        want.next();
                    }
                }
                super::handles::handle_list(lua, out)
            })
        });
        super::timing::add_query_methods(m);
        m.add_method("filter", |lua, this, ()| {
            let t = lua.create_table()?;
            let f = &this.filter;
            t.set("start_tick", ticks::to_lua(f.t0)?)?;
            if let Some(t1) = f.t1 {
                t.set("end_tick", ticks::to_lua(t1)?)?;
            }
            t.set("selected", f.selected_only)?;
            Ok(t)
        });
    }
}

/// Which columns a batch holds (fewer columns: faster batches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Columns(u16);

impl Columns {
    const NAMES: [&'static str; 16] = [
        "start",
        "gate",
        "key",
        "velocity",
        "channel",
        "subchannel",
        "selected",
        "index",
        "muted",
        "off_kind",
        "off_velocity",
        "group",
        "locked",
        "track_id",
        "content_id",
        "host_kind",
    ];
    const ALL: Columns = Columns(0xFF);

    fn has(self, i: usize) -> bool {
        self.0 & (1 << i) != 0
    }

    fn parse(t: &Table) -> mlua::Result<Columns> {
        let mut bits = 0u16;
        for name in t.clone().sequence_values::<String>() {
            let name = name?;
            let i = Columns::NAMES
                .iter()
                .position(|&n| n == name)
                .ok_or_else(|| {
                    runtime(format!(
                        "There is no column \u{201C}{name}\u{201D} ({}).",
                        Columns::NAMES.join(", ")
                    ))
                })?;
            bits |= 1 << i;
        }
        Ok(Columns(bits))
    }
}

/// Struct-of-arrays table of `hits` with the asked `columns` (each built in
/// one pass with a pre-sized array).
fn batch_table(lua: &Lua, hits: &[Hit], columns: Columns, host: Host) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(0, 9)?;
    // Ticks past 2^63 - 1 cannot reach a script: refuse the batch once.
    if hits.iter().any(|h| {
        h.rec.start_tick > ticks::MAX_LUA_TICK || h.rec.duration_ticks > ticks::MAX_LUA_TICK
    }) {
        return Err(runtime(
            "A note tick is past 2^63 - 1, the largest Lua integer, and cannot be given to a script exactly.",
        ));
    }
    t.raw_set("count", hits.len() as i64)?;
    let world = if columns.has(12) {
        current_world(lua)
    } else {
        None
    };
    let ints = |f: &dyn Fn(&Hit) -> i64| lua.create_sequence_from(hits.iter().map(f));
    for (i, name) in Columns::NAMES.iter().enumerate() {
        if !columns.has(i) {
            continue;
        }
        let col = match i {
            0 => ints(&|h| h.rec.start_tick as i64)?,
            1 => ints(&|h| h.rec.duration_ticks as i64)?,
            2 => ints(&|h| i64::from(h.rec.attrs.pitch()))?,
            3 => ints(&|h| i64::from(h.rec.attrs.velocity()))?,
            4 => ints(&|h| i64::from(h.rec.attrs.channel()))?,
            5 => ints(&|h| i64::from(h.rec.sub.0))?,
            6 => lua.create_sequence_from(hits.iter().map(|h| h.selected))?,
            7 => ints(&|h| h.rank as i64 + 1)?,
            8 => lua.create_sequence_from(hits.iter().map(|h| h.rec.attrs.muted()))?,
            9 => lua.create_sequence_from(hits.iter().map(|h| match h.rec.attrs.off_kind() {
                plumspirit_engine::model::NoteOffKind::NoteOff => "note_off",
                plumspirit_engine::model::NoteOffKind::NoteOnZero => "note_on_zero",
            }))?,
            10 => ints(&|h| i64::from(h.rec.attrs.off_velocity()))?,
            11 => ints(&|h| i64::from(h.rec.group))?,
            12 => lua.create_sequence_from(hits.iter().map(|h| {
                world
                    .as_ref()
                    .is_some_and(|w| w.doc.project().groups.locked(h.rec.group))
            }))?,
            13 => ints(&|_| i64::from(host.track.0))?,
            14 => ints(&|_| host.content.map_or(0, |c| i64::from(c.0)))?,
            _ => lua.create_sequence_from(hits.iter().map(|_| {
                if host.content.is_some() {
                    "clip_content"
                } else {
                    "track"
                }
            }))?,
        };
        t.raw_set(*name, col)?;
    }
    Ok(t)
}

/// Registers `notes` on track and clip content handles.
pub fn add_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    m.add_method("notes", |lua, this, opts: Option<Table>| {
        let mut filter = Filter::from_lua(opts.as_ref())?;
        // A clip: its content's notes inside the clip's window (content
        // ticks; writes change the content, so every linked clip).
        if let Some((content, from, to)) = super::clips::notes_window(lua, &this.0)? {
            filter.t0 = filter.t0.max(from);
            filter.t1 = match (filter.t1, to) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            host_of(lua, &content)?;
            return Ok(LuaNoteQuery {
                host: content,
                filter,
            });
        }
        // Checks the handle and the family now, not at first use.
        host_of(lua, &this.0)?;
        Ok(LuaNoteQuery {
            host: this.0.clone(),
            filter,
        })
    });
}

/// `SubchannelId` of a script value.
pub fn subchannel_id(v: i64) -> mlua::Result<SubchannelId> {
    u32::try_from(v)
        .map(SubchannelId)
        .map_err(|_| runtime(format!("A subchannel id is 0 - 4294967295; {v} is not.")))
}
