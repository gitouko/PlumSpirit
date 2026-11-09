//! Native completed Piano Roll strokes; no Lua runs during pointer capture.
use super::bridge::{JobLease, NativeBudget, Reservation, Session};
use super::{Capability, Category, Host, notes, stop, ticks};
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use plumspirit_engine::transform::{MAX_STROKE_POINTS, Stroke, StrokePoint};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

const HANDLE_BYTES: usize = 512;
const MAX_BATCH: usize = 1024;

/// Constructed only from host capture, never from a Lua descriptor.
#[derive(Debug)]
pub struct Captured {
    pub stroke: Stroke,
    pub track: u32,
    pub content: Option<u32>,
    pub shift: i128,
    pub keys: u16,
    pub ppq: u64,
    pub snap: u64,
    pub modifiers: [bool; 4],
    lo: u64,
    hi: u64,
}
impl Captured {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        points: &[(u64, f64, f64)],
        track: u32,
        content: Option<u32>,
        shift: i128,
        keys: u16,
        ppq: u64,
        snap: u64,
        modifiers: [bool; 4],
    ) -> Result<Arc<Self>, String> {
        if points.is_empty() || points.len() > MAX_STROKE_POINTS {
            return Err("A completed drawing stroke needs1–65536samples.".into());
        }
        if !matches!(keys, 128 | 256)
            || ppq == 0
            || snap == 0
            || ppq > ticks::MAX_LUA_TICK
            || snap > ticks::MAX_LUA_TICK
            || shift.unsigned_abs() > u64::MAX as u128
        {
            return Err("Invalid native drawing coordinate context.".into());
        }
        let mut previous = 0.;
        let mut lo = u64::MAX;
        let mut hi = 0;
        for &(tick, pitch, time) in points {
            if tick > ticks::MAX_LUA_TICK
                || !pitch.is_finite()
                || pitch.abs() > 4096.
                || !time.is_finite()
                || !(previous..=86400.).contains(&time)
            {
                return Err("Drawing samples require exact ticks, finite pitch and ordered bounded timestamps.".into());
            }
            previous = time;
            lo = lo.min(tick);
            hi = hi.max(tick);
        }
        Ok(Arc::new(Self {
            stroke: Stroke {
                points: points
                    .iter()
                    .map(|&(tick, pitch, t)| StrokePoint { tick, pitch, t })
                    .collect(),
            },
            track,
            content,
            shift,
            keys,
            ppq,
            snap,
            modifiers,
            lo,
            hi,
        }))
    }
}
type Cell = RefCell<Option<(Arc<Captured>, Reservation)>>;
type Data = Rc<Cell>;
#[derive(Default)]
pub(super) struct Slot {
    pending: Option<Arc<Captured>>,
    active: Option<Arc<Captured>>,
    handles: Vec<Weak<Cell>>,
}
pub(super) fn set_input(lua: &Lua, input: Option<Arc<Captured>>) {
    lua.app_data_mut::<Slot>()
        .expect("drawing input slot")
        .pending = input;
}
pub(super) fn begin_run(lua: &Lua) {
    end_run(lua);
    let mut s = lua.app_data_mut::<Slot>().expect("drawing input slot");
    s.active = s.pending.take();
}
pub(super) fn end_run(lua: &Lua) {
    if let Some(mut s) = lua.app_data_mut::<Slot>() {
        s.active = None;
        for h in s.handles.drain(..).filter_map(|w| w.upgrade()) {
            h.borrow_mut().take();
        }
    }
}
pub(super) fn require(lua: &Lua) -> mlua::Result<()> {
    let granted = lua
        .app_data_ref::<super::handles::Granted>()
        .map(|g| Arc::clone(&g.0))
        .unwrap_or_default();
    Host::require(&granted, Capability::DrawingTools)
}
struct Input {
    job: std::sync::Weak<JobLease>,
    data: Data,
    _allocation: Reservation,
}
impl Input {
    fn get(&self, lua: &Lua) -> mlua::Result<Arc<Captured>> {
        require(lua)?;
        let job = self
            .job
            .upgrade()
            .ok_or_else(|| stop(Category::Expired, "This drawing stroke expired."))?;
        job.check().map_err(|e| e.lua())?;
        if !lua
            .app_data_ref::<Session>()
            .and_then(|s| s.lease.clone())
            .is_some_and(|j| Arc::ptr_eq(&j, &job))
        {
            return Err(stop(
                Category::Expired,
                "This drawing stroke belongs to an earlier job.",
            ));
        }
        notes::check_budget(lua)?;
        self.data
            .borrow()
            .as_ref()
            .map(|(s, _)| Arc::clone(s))
            .ok_or_else(|| stop(Category::Expired, "This drawing stroke is closed."))
    }
}
pub(super) fn for_tool(
    lua: &Lua,
    track: u32,
    content: Option<u32>,
    keys: u16,
) -> mlua::Result<Value> {
    require(lua)?;
    notes::check_budget(lua)?;
    let input = lua
        .app_data_ref::<Slot>()
        .and_then(|s| s.active.clone())
        .ok_or_else(|| {
            stop(
                Category::Runtime,
                "Draw a completed stroke in the Piano Roll before running this tool.",
            )
        })?;
    if input.track != track || input.content != content || input.keys != keys {
        return Err(stop(
            Category::Expired,
            "The drawing stroke's editing target or key mode changed.",
        ));
    }
    if lua
        .app_data_ref::<Slot>()
        .is_some_and(|s| !s.handles.is_empty())
    {
        return Err(stop(
            Category::Budget,
            "Only one drawing stroke handle is available per tool job.",
        ));
    }
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .expect("native budget")
        .clone();
    let allocation = budget.reserve(HANDLE_BYTES).map_err(|e| e.lua())?;
    let bytes = input
        .stroke
        .points
        .len()
        .checked_mul(std::mem::size_of::<StrokePoint>())
        .and_then(|n| n.checked_add(512))
        .ok_or_else(|| stop(Category::Budget, "Drawing stroke allocation overflow."))?;
    let data = Rc::new(RefCell::new(Some((
        input,
        budget.reserve(bytes).map_err(|e| e.lua())?,
    ))));
    {
        let mut slot = lua.app_data_mut::<Slot>().unwrap();
        slot.active = None;
        slot.handles.push(Rc::downgrade(&data));
    }
    let job = lua
        .app_data_ref::<Session>()
        .and_then(|s| s.lease.as_ref().map(Arc::downgrade))
        .ok_or_else(|| stop(Category::Expired, "No drawing job is active."))?;
    Ok(Value::UserData(lua.create_userdata(Input {
        job,
        data,
        _allocation: allocation,
    })?))
}
fn index(v: Value, max: usize, name: &str) -> mlua::Result<usize> {
    match v {
        Value::Integer(n) if n >= 1 && n as u128 <= max as u128 => Ok(n as usize),
        _ => Err(stop(
            Category::Runtime,
            format!("{name} must be a Lua integer in1–{max}."),
        )),
    }
}
fn sample(lua: &Lua, s: &Captured, i: usize) -> mlua::Result<Table> {
    let p = s.stroke.points[i];
    let previous = s.stroke.points[i.saturating_sub(1)];
    // Subtract integers before conversion, preserving large-tick relative precision.
    let dx = (i128::from(p.tick) - i128::from(previous.tick)) as f64 / s.ppq as f64;
    let dy = p.pitch - previous.pitch;
    let distance = dx.hypot(dy);
    let dt = p.t - previous.t;
    let song = i128::from(p.tick) + s.shift;
    let t = lua.create_table()?;
    t.set("tick", ticks::to_lua(p.tick)?)?;
    t.set("song_tick_text", song.to_string())?;
    if (0..=i128::from(i64::MAX)).contains(&song) {
        t.set("song_tick", song as i64)?;
    }
    t.set("pitch", p.pitch)?;
    t.set("time", p.t)?;
    t.set(
        "x",
        if s.hi == s.lo {
            0.
        } else {
            (p.tick - s.lo) as f64 / (s.hi - s.lo) as f64
        },
    )?;
    t.set("y", p.pitch / f64::from(s.keys))?;
    t.set("distance", distance)?;
    t.set(
        "speed",
        if dt > 0. {
            distance / dt.max(0.001)
        } else {
            0.
        },
    )?;
    t.set(
        "direction_x",
        if distance > 0. { dx / distance } else { 0. },
    )?;
    t.set(
        "direction_y",
        if distance > 0. { dy / distance } else { 0. },
    )?;
    t.set("start", i == 0)?;
    t.set("finish", i + 1 == s.stroke.points.len())?;
    Ok(t)
}
impl UserData for Input {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("info", |lua, this, ()| {
            let s = this.get(lua)?;
            let t = lua.create_table()?;
            t.set("count", s.stroke.points.len())?;
            t.set("track_id", s.track)?;
            t.set("content_id", s.content)?;
            t.set("space", "target_local")?;
            t.set("song_shift", s.shift.to_string())?;
            t.set("keys", s.keys)?;
            t.set("ppq", ticks::to_lua(s.ppq)?)?;
            t.set("snap", ticks::to_lua(s.snap)?)?;
            t.set("min_tick", ticks::to_lua(s.lo)?)?;
            t.set("max_tick", ticks::to_lua(s.hi)?)?;
            t.set(
                "duration",
                s.stroke.points.last().unwrap().t - s.stroke.points[0].t,
            )?;
            for (k, v) in ["shift", "ctrl", "alt", "command"]
                .into_iter()
                .zip(s.modifiers)
            {
                t.set(k, v)?;
            }
            t.set("modifiers_at", "completion")?;
            t.set("pressure_supported", false)?;
            t.set("tilt_supported", false)?;
            t.set("sampling", "native_frame")?;
            t.set("ordered_events", true)?;
            t.set("max_events_per_frame", 256)?;
            t.set(
                "at_sample_limit",
                s.stroke.points.len() == MAX_STROKE_POINTS,
            )?;
            Ok(t)
        });
        m.add_method("point", |lua, this, v: Value| {
            let s = this.get(lua)?;
            sample(lua, &s, index(v, s.stroke.points.len(), "Point index")? - 1)
        });
        m.add_method("batch", |lua, this, (first, count): (Value, Value)| {
            let s = this.get(lua)?;
            let first = index(first, s.stroke.points.len(), "First sample")? - 1;
            let count = index(count, MAX_BATCH, "Sample count")?;
            if first + count > s.stroke.points.len() {
                return Err(stop(
                    Category::Runtime,
                    "The sample batch extends beyond this stroke.",
                ));
            }
            let t = lua.create_table_with_capacity(count, 0)?;
            for i in 0..count {
                if i % 64 == 0 {
                    notes::check_budget(lua)?;
                }
                t.raw_set(i + 1, sample(lua, &s, first + i)?)?;
            }
            Ok(t)
        });
        m.add_method("close", |_, this, ()| {
            Ok(this.data.borrow_mut().take().is_some())
        });
    }
}

#[cfg(test)]
#[path = "drawing_tests.rs"]
mod tests;
