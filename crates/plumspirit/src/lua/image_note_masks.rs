//! Job-scoped note-domain sampling over existing compact A8 masks.
use super::*;

pub const MAX_SAMPLERS: usize = 16;
pub const MAX_BATCH: usize = 4096;
const METADATA: usize = 2048;

#[path = "image_note_queries.rs"]
pub(in crate::lua) mod queries;
#[derive(Clone, Copy)]
enum Policy {
    Start,
    Center,
    Any,
    All,
    Average,
}
impl Policy {
    fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::Any => "any",
            Self::All => "all",
            Self::Average => "average",
        }
    }
}
#[derive(Clone, Copy)]
struct Mapping {
    start: u64,
    end: u64,
    first: u8,
    last: u8,
    orientation: im::Orientation,
    policy: Policy,
    threshold: u8,
}
pub(in crate::lua) struct Stored {
    width: u32,
    height: u32,
    time: usize,
    lines: usize,
    mapping: Mapping,
    sums: Vec<u32>,
    counts: Vec<u32>,
    _allocation: Reservation,
}
pub(super) struct Sampler {
    job: Arc<JobLease>,
    data: Rc<RefCell<Option<Stored>>>,
    _metadata: Reservation,
}
fn option(t: &Table, key: &str, default: i64) -> mlua::Result<Value> {
    Ok(match t.raw_get::<Value>(key)? {
        Value::Nil => Value::Integer(default),
        v => v,
    })
}
fn word(t: &Table, key: &str, default: &str) -> mlua::Result<String> {
    match t.raw_get::<Value>(key)? {
        Value::Nil => Ok(default.into()),
        Value::String(s) if s.as_bytes().len() <= 32 => Ok(s.to_str()?.to_string()),
        _ => Err(bad(format!("Note mask {key} must be text."))),
    }
}
fn parse(t: &Table) -> mlua::Result<Mapping> {
    if t.metatable().is_some() {
        return Err(bad("Note mask options must be a plain table."));
    }
    ops::keys(
        t,
        &[
            b"start_tick",
            b"end_tick",
            b"first_key",
            b"last_key",
            b"orientation",
            b"sample",
            b"threshold",
        ],
        "Note mask",
    )?;
    let start = crate::lua::ticks::from_lua(&option(t, "start_tick", 0)?, "Note mask start tick")?;
    let end = crate::lua::ticks::from_lua(&t.raw_get::<Value>("end_tick")?, "Note mask end tick")?;
    if end <= start {
        return Err(bad("Note mask end_tick must be greater than start_tick."));
    }
    let first = integer(option(t, "first_key", 0)?, 255, "Note mask first key")? as u8;
    let last = integer(option(t, "last_key", 127)?, 255, "Note mask last key")? as u8;
    if first > last {
        return Err(bad("Note mask first_key must not exceed last_key."));
    }
    let orientation = match word(t, "orientation", "piano_roll")?.as_str() {
        "piano_roll" => im::Orientation::PianoRoll,
        "upstream" => im::Orientation::Upstream,
        _ => return Err(bad("Note mask orientation must be piano_roll or upstream.")),
    };
    let policy = match word(t, "sample", "start")?.as_str() {
        "start" => Policy::Start,
        "center" => Policy::Center,
        "any" => Policy::Any,
        "all" => Policy::All,
        "average" => Policy::Average,
        _ => {
            return Err(bad(
                "Note mask sample must be start, center, any, all or average.",
            ));
        }
    };
    let threshold = integer(option(t, "threshold", 128)?, 255, "Note mask threshold")? as u8;
    Ok(Mapping {
        start,
        end,
        first,
        last,
        orientation,
        policy,
        threshold,
    })
}
impl Stored {
    pub(in crate::lua) fn domain(&self) -> (u64, u64) {
        (self.mapping.start, self.mapping.end)
    }
    /// Coverage is the mean A8 value of sampled cells. `any`/`all` compare
    /// individual cells; average compares the mean. Outside is always unmatched.
    pub(in crate::lua) fn sample(&self, start: u64, gate: u64, key: u8) -> (f64, bool) {
        let m = self.mapping;
        if key < m.first || key > m.last {
            return (0., false);
        }
        let pitch = if m.orientation == im::Orientation::PianoRoll {
            m.last - key
        } else {
            key - m.first
        };
        let keys = usize::from(m.last) - usize::from(m.first) + 1;
        let line = (2 * usize::from(pitch) + 1) * self.lines / (2 * keys);
        let span = u128::from(m.end - m.start);
        let first = u128::from(m.start);
        let last = u128::from(m.end);
        let (left, right) = match m.policy {
            Policy::Start | Policy::Center => {
                let tick = u128::from(start)
                    + if matches!(m.policy, Policy::Center) {
                        u128::from(gate) / 2
                    } else {
                        0
                    };
                if tick < first || tick >= last {
                    return (0., false);
                }
                let cell = ((tick - first) * self.time as u128 / span) as usize;
                (cell, cell + 1)
            }
            _ => {
                let left = u128::from(start).max(first);
                let right = (u128::from(start) + u128::from(gate.max(1))).min(last);
                if right <= left {
                    return (0., false);
                }
                (
                    ((left - first) * self.time as u128 / span) as usize,
                    ((right - first) * self.time as u128).div_ceil(span) as usize,
                )
            }
        };
        let base = line * (self.time + 1);
        let sum = self.sums[base + right] - self.sums[base + left];
        let count = self.counts[base + right] - self.counts[base + left];
        let cells = (right - left) as u32;
        let matched = match m.policy {
            Policy::Any => count > 0,
            Policy::All => count == cells,
            _ => u64::from(sum) >= u64::from(m.threshold) * u64::from(cells),
        };
        (f64::from(sum) / f64::from(cells), matched)
    }
}
fn build(
    width: u32,
    height: u32,
    pixels: &[u8],
    mapping: Mapping,
    allocation: Reservation,
    check: impl Fn() -> mlua::Result<()>,
) -> mlua::Result<Stored> {
    let (time, lines) = if mapping.orientation == im::Orientation::PianoRoll {
        (width as usize, height as usize)
    } else {
        (height as usize, width as usize)
    };
    let len = (time + 1) * lines;
    let mut sums = Vec::new();
    let mut counts = Vec::new();
    sums.try_reserve_exact(len)
        .map_err(|_| stop(Category::Memory, "Note mask sums could not be allocated."))?;
    counts
        .try_reserve_exact(len)
        .map_err(|_| stop(Category::Memory, "Note mask counts could not be allocated."))?;
    for line in 0..lines {
        sums.push(0u32);
        counts.push(0u32);
        let mut sum = 0u32;
        let mut count = 0u32;
        for time_cell in 0..time {
            if (line * time + time_cell).is_multiple_of(4096) {
                check()?;
            }
            let pixel = if mapping.orientation == im::Orientation::PianoRoll {
                pixels[line * width as usize + time_cell]
            } else {
                pixels[(height as usize - 1 - time_cell) * width as usize + line]
            };
            sum += u32::from(pixel);
            count += u32::from(pixel >= mapping.threshold);
            sums.push(sum);
            counts.push(count);
        }
    }
    check()?;
    Ok(Stored {
        width,
        height,
        time,
        lines,
        mapping,
        sums,
        counts,
        _allocation: allocation,
    })
}
pub(super) fn create(
    lua: &Lua,
    job: Arc<JobLease>,
    width: u32,
    height: u32,
    pixels: &[u8],
    options: Table,
) -> mlua::Result<Sampler> {
    check_scope(lua, &job, "mask")?;
    {
        let mut slot = lua
            .app_data_mut::<ImageSlot>()
            .ok_or_else(|| bad("Image scope unavailable."))?;
        if slot.sampler_attempts >= MAX_SAMPLERS {
            return Err(stop(
                Category::Budget,
                "At most16note-mask sampler attempts are allowed per job.",
            ));
        }
        slot.sampler_attempts += 1;
    }
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Native mask accounting unavailable."))?
        .clone();
    let metadata = budget.reserve(METADATA).map_err(|e| e.lua())?;
    let mapping = parse(&options)?;
    let cells = u64::from(width) * u64::from(height);
    {
        let mut slot = lua.app_data_mut::<ImageSlot>().unwrap();
        if slot.sampler_pixels.saturating_add(cells) > MAX_PIXELS {
            return Err(stop(
                Category::Budget,
                "Note-mask samplers may index at most16M cells cumulatively per job.",
            ));
        }
        slot.sampler_pixels += cells;
    }
    let lines = if mapping.orientation == im::Orientation::PianoRoll {
        height
    } else {
        width
    };
    let bytes = (cells + u64::from(lines)) as usize * 8;
    let allocation = budget.reserve(bytes).map_err(|e| e.lua())?;
    let stored = build(width, height, pixels, mapping, allocation, || {
        notes::check_budget(lua)
    })?;
    let data = Rc::new(RefCell::new(Some(stored)));
    lua.app_data_mut::<ImageSlot>()
        .unwrap()
        .samplers
        .push(Rc::downgrade(&data));
    Ok(Sampler {
        job,
        data,
        _metadata: metadata,
    })
}
impl Sampler {
    fn with<T>(&self, lua: &Lua, f: impl FnOnce(&Stored) -> mlua::Result<T>) -> mlua::Result<T> {
        check_scope(lua, &self.job, "note mask sampler")?;
        let data = self
            .data
            .try_borrow()
            .map_err(|_| bad("Note mask sampler is busy."))?;
        f(data
            .as_ref()
            .ok_or_else(|| stop(Category::Expired, "This note mask sampler is closed."))?)
    }
}

pub(in crate::lua) fn with_native<T>(
    lua: &Lua,
    value: &AnyUserData,
    f: impl FnOnce(&Stored) -> mlua::Result<T>,
) -> mlua::Result<T> {
    value.borrow::<Sampler>()?.with(lua, f)
}

fn dense(t: &Table, max: usize, expected: Option<usize>) -> mlua::Result<usize> {
    if t.metatable().is_some() {
        return Err(bad("Note mask columns must be plain tables."));
    }
    let len = t.raw_len();
    if len > max || expected.is_some_and(|n| n != len) {
        return Err(bad(
            "Note mask columns must have equal lengths of at most4096.",
        ));
    }
    let mut count = 0;
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        count += 1;
        if count > len || !matches!(k,Value::Integer(i)if i>=1 && i as u64<=len as u64) {
            return Err(bad(
                "Note mask columns must be dense arrays with no extra fields.",
            ));
        }
    }
    if count != len {
        return Err(bad("Note mask columns cannot have holes."));
    }
    Ok(len)
}
impl UserData for Sampler {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("count_notes", queries::count);
        m.add_method(
            "sample",
            |lua, this, (start, gate, key): (Value, Value, Value)| {
                let start = crate::lua::ticks::from_lua(&start, "Note mask sample start")?;
                let gate = crate::lua::ticks::from_lua(&gate, "Note mask sample gate")?;
                let key = integer(key, 255, "Note mask sample key")? as u8;
                this.with(lua, |s| Ok(s.sample(start, gate, key)))
            },
        );
        m.add_method("sample_batch", |lua, this, table: Table| {
            if table.metatable().is_some() {
                return Err(bad("Note mask batch must be a plain table."));
            }
            ops::keys(&table, &[b"start", b"gate", b"key"], "Note mask batch")?;
            let start: Table = table.raw_get("start")?;
            let gate: Table = table.raw_get("gate")?;
            let key: Table = table.raw_get("key")?;
            let len = dense(&start, MAX_BATCH, None)?;
            dense(&gate, MAX_BATCH, Some(len))?;
            dense(&key, MAX_BATCH, Some(len))?;
            this.with(lua, |s| {
                let out = lua.create_table()?;
                let coverage = lua.create_table_with_capacity(len, 0)?;
                let matched = lua.create_table_with_capacity(len, 0)?;
                for i in 1..=len {
                    if i.is_multiple_of(256) {
                        notes::check_budget(lua)?;
                    }
                    let tick = crate::lua::ticks::from_lua(
                        &start.raw_get::<Value>(i)?,
                        "Note mask sample start",
                    )?;
                    let duration = crate::lua::ticks::from_lua(
                        &gate.raw_get::<Value>(i)?,
                        "Note mask sample gate",
                    )?;
                    let pitch = integer(key.raw_get(i)?, 255, "Note mask sample key")? as u8;
                    let (value, yes) = s.sample(tick, duration, pitch);
                    coverage.raw_set(i, value)?;
                    matched.raw_set(i, yes)?;
                }
                notes::check_budget(lua)?;
                out.set("coverage", coverage)?;
                out.set("matched", matched)?;
                Ok(out)
            })
        });
        m.add_method("info", |lua, this, ()| {
            this.with(lua, |s| {
                let t = lua.create_table()?;
                let p = s.mapping;
                t.set("width", s.width)?;
                t.set("height", s.height)?;
                t.set("time_pixels", s.time)?;
                t.set("pitch_pixels", s.lines)?;
                t.set("start_tick", p.start)?;
                t.set("end_tick", p.end)?;
                t.set("first_key", p.first)?;
                t.set("last_key", p.last)?;
                t.set("sample", p.policy.name())?;
                t.set("threshold", p.threshold)?;
                t.set(
                    "orientation",
                    if p.orientation == im::Orientation::PianoRoll {
                        "piano_roll"
                    } else {
                        "upstream"
                    },
                )?;
                t.set("index_bytes", (s.sums.capacity() + s.counts.capacity()) * 4)?;
                Ok(t)
            })
        });
        m.add_method("close", |_, this, ()| {
            Ok(this
                .data
                .try_borrow_mut()
                .ok()
                .and_then(|mut d| d.take())
                .is_some())
        });
    }
}
#[cfg(test)]
#[path = "image_note_mask_tests.rs"]
mod tests;
