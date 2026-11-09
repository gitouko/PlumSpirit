//! Immutable worker-owned image-note plans. Author palette/mapping, no project mutation.
use super::*;
use plumspirit_engine::{model::NoteRecord, transform::image_midi::runs};
use std::mem::size_of;

pub const MAX_PLANS: usize = 16;
pub const MAX_BATCH: usize = 4096;
pub const MAX_WORK: u64 = 256 * 1024 * 1024;
const METADATA_BYTES: usize = 8192;
pub(super) struct Stored {
    indices: Vec<u32>,
    cursor: runs::Cursor,
    initial: runs::Cursor,
    width: u32,
    height: u32,
    settings: Mapping,
    palette: Vec<[u8; 3]>,
    count: u64,
    read: u64,
    end_tick: u64,
    _allocation: Reservation,
}
#[derive(Clone, Copy)]
struct Mapping {
    origin: u64,
    num: u64,
    den: u64,
    first: u8,
    keys: u16,
    orientation: im::Orientation,
    velocity: u8,
}
impl Mapping {
    fn tick(self, cell: u32) -> mlua::Result<u64> {
        let tick = u128::from(self.origin)
            + u128::from(cell) * u128::from(self.num) / u128::from(self.den);
        if tick > u128::from(crate::lua::ticks::MAX_LUA_TICK) {
            return Err(bad(
                "Image note mapping extends beyond the exact signed MIDI tick range.",
            ));
        }
        Ok(tick as u64)
    }
    fn row(self, run: runs::Run, lines: u32) -> mlua::Result<Row> {
        let start = self.tick(run.start)?;
        let end = self.tick(run.start + run.length)?;
        let pitch = u32::from(self.first)
            + if self.orientation == im::Orientation::PianoRoll {
                lines - 1 - run.line
            } else {
                run.line
            };
        if pitch >= u32::from(self.keys) || end <= start {
            return Err(bad(
                "Image note mapping produced an invalid key or nonpositive gate.",
            ));
        }
        Ok(Row {
            start,
            gate: end - start,
            key: pitch as u8,
            velocity: self.velocity,
            palette: run.color + 1,
        })
    }
}
pub(in crate::lua) struct Row {
    pub start: u64,
    pub gate: u64,
    pub key: u8,
    pub velocity: u8,
    pub palette: u32,
}
struct Plan {
    job: Arc<JobLease>,
    data: Rc<RefCell<Option<Stored>>>,
    _metadata: Reservation,
}

/// A private, copied cursor. Its progress is published only if the native
/// consumer accepts all staged output. Neither the image buffer nor Lua escapes.
pub(in crate::lua) struct NativeReader<'a> {
    stored: &'a Stored,
    cursor: runs::Cursor,
    read: u64,
}
impl NativeReader<'_> {
    pub fn remaining(&self) -> usize {
        (self.stored.count - self.read) as usize
    }
    pub fn palette_count(&self) -> usize {
        self.stored.palette.len()
    }
    pub fn count_mapped(&self, included: &[bool], lua: &Lua) -> mlua::Result<usize> {
        if included.iter().all(|v| *v) {
            return Ok(self.remaining());
        }
        let mut copy = Self {
            stored: self.stored,
            cursor: self.cursor,
            read: self.read,
        };
        let mut count = 0;
        copy.visit(lua, |row| {
            count += usize::from(included[row.palette as usize - 1]);
            Ok(())
        })?;
        Ok(count)
    }
    pub fn visit(
        &mut self,
        lua: &Lua,
        mut emit: impl FnMut(Row) -> mlua::Result<()>,
    ) -> mlua::Result<()> {
        while self.read < self.stored.count {
            let mut error = None;
            let mut interrupted = None;
            let (lines, _) = self.cursor.dimensions();
            let result = self.cursor.visit_controlled(
                &self.stored.indices,
                runs::MAX_BATCH,
                |run| match self.stored.settings.row(run, lines).and_then(&mut emit) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        error = Some(e);
                        Err("Native image note staging failed.".into())
                    }
                },
                || match notes::check_budget(lua) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        interrupted = Some(e);
                        Err("Native image note staging interrupted.".into())
                    }
                },
            );
            if let Some(e) = error.or(interrupted) {
                return Err(e);
            }
            let count = result.map_err(native_error)?;
            if count == 0 {
                return Err(bad(
                    "Image note plan count disagrees with its immutable data.",
                ));
            }
            self.read += count as u64;
        }
        notes::check_budget(lua)
    }
}

pub(in crate::lua) fn with_native<T>(
    lua: &Lua,
    value: &AnyUserData,
    f: impl FnOnce(&mut NativeReader<'_>) -> mlua::Result<T>,
) -> mlua::Result<T> {
    let plan = value.borrow::<Plan>()?;
    check_scope(lua, &plan.job, "image note plan")?;
    let mut data = plan
        .data
        .try_borrow_mut()
        .map_err(|_| bad("The image note plan is busy."))?;
    let stored = data
        .as_mut()
        .ok_or_else(|| stop(Category::Expired, "This image note plan is closed."))?;
    let mut reader = NativeReader {
        cursor: stored.cursor,
        read: stored.read,
        stored,
    };
    let result = f(&mut reader)?;
    // The consumer's successful acceptance is the commit point. No allocation,
    // callback or fallible check may separate these two private owner updates.
    let (cursor, read) = (reader.cursor, reader.read);
    stored.cursor = cursor;
    stored.read = read;
    Ok(result)
}
fn dense(table: &Table, max: usize, exact: Option<usize>) -> mlua::Result<usize> {
    if table.metatable().is_some() {
        return Err(bad("Image note arrays must be plain tables."));
    }
    let len = table.raw_len();
    if len > max || exact.is_some_and(|n| n != len) {
        return Err(bad("Image note array length is invalid."));
    }
    let mut count = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        count += 1;
        if count > len || !matches!(key,Value::Integer(i)if i>=1 && i as u64<=len as u64) {
            return Err(bad(
                "Image note arrays must be dense and have no extra fields.",
            ));
        }
    }
    if count != len {
        return Err(bad("Image note arrays cannot contain holes."));
    }
    Ok(len)
}
fn value(options: &Table, key: &str, default: i64) -> mlua::Result<Value> {
    match options.raw_get::<Value>(key)? {
        Value::Nil => Ok(Value::Integer(default)),
        v => Ok(v),
    }
}
fn word(options: &Table, key: &str, default: &str) -> mlua::Result<String> {
    match options.raw_get::<Value>(key)? {
        Value::Nil => Ok(default.into()),
        Value::String(s) if s.as_bytes().len() <= 32 => Ok(s.to_str()?.to_string()),
        _ => Err(bad(format!("Image note {key} must be text."))),
    }
}
fn parse(options: &Table) -> mlua::Result<(im::Settings, Mapping)> {
    if options.metatable().is_some() {
        return Err(bad("Image note options must be a plain table."));
    }
    ops::keys(
        options,
        &[
            b"palette",
            b"ignored",
            b"alpha_min",
            b"orientation",
            b"pixel_notes",
            b"max_length",
            b"split",
            b"keys",
            b"first_key",
            b"velocity",
            b"start_tick",
            b"tick_numerator",
            b"tick_denominator",
        ],
        "Image note plan",
    )?;
    let palette: Table = options.raw_get("palette")?;
    let n = dense(&palette, im::MAX_PALETTE, None)?;
    if n == 0 {
        return Err(bad(
            "Image note plans require1through1024authored palette colors.",
        ));
    }
    let mut colors = Vec::with_capacity(n);
    for i in 1..=n {
        let c: Table = palette.raw_get(i)?;
        dense(&c, 3, Some(3))?;
        colors.push(color::color_table(&c)?);
    }
    let ignored = match options.raw_get::<Value>("ignored")? {
        Value::Nil => vec![false; n],
        Value::Table(t) => {
            dense(&t, n, Some(n))?;
            let mut out = Vec::with_capacity(n);
            for i in 1..=n {
                let Value::Boolean(b) = t.raw_get::<Value>(i)? else {
                    return Err(bad("Image note ignored flags must be booleans."));
                };
                out.push(b);
            }
            out
        }
        _ => return Err(bad("Image note ignored must be a dense boolean array.")),
    };
    let keys = integer(value(options, "keys", 128)?, 256, "Image note keys")?;
    if !matches!(keys, 128 | 256) {
        return Err(bad("Image note keys must be128or256."));
    }
    let first = integer(
        value(options, "first_key", 0)?,
        keys - 1,
        "Image note first key",
    )? as u8;
    let velocity = integer(value(options, "velocity", 100)?, 127, "Image note velocity")? as u8;
    if velocity == 0 {
        return Err(bad("Image note velocity must be1through127."));
    }
    let origin =
        crate::lua::ticks::from_lua(&value(options, "start_tick", 0)?, "Image note start tick")?;
    let num = crate::lua::ticks::from_lua(
        &value(options, "tick_numerator", 5)?,
        "Image note tick numerator",
    )?;
    let den = crate::lua::ticks::from_lua(
        &value(options, "tick_denominator", 1)?,
        "Image note tick denominator",
    )?;
    if den == 0 || num < den {
        return Err(bad(
            "Image note tick scale must have positive denominator and at least one tick per cell; resize the image for coarser time resolution.",
        ));
    }
    let orientation = match word(options, "orientation", "piano_roll")?.as_str() {
        "piano_roll" => im::Orientation::PianoRoll,
        "upstream" => im::Orientation::Upstream,
        _ => {
            return Err(bad(
                "Image note orientation must be piano_roll or upstream.",
            ));
        }
    };
    let split = match word(options, "split", "note")?.as_str() {
        "note" => im::SplitFrom::NoteStart,
        "image" => im::SplitFrom::ImageStart,
        _ => return Err(bad("Image note split must be note or image.")),
    };
    let settings = im::Settings {
        palette: colors,
        ignored,
        alpha_min: integer(
            value(options, "alpha_min", 128)?,
            255,
            "Image note alpha minimum",
        )? as u8,
        orientation,
        split,
        pixel_notes: ops::flag(options, "pixel_notes")?,
        max_len: integer(
            value(options, "max_length", 0)?,
            im::MAX_TIME_PIXELS,
            "Image note maximum cell length",
        )?,
        ..Default::default()
    };
    Ok((
        settings,
        Mapping {
            origin,
            num,
            den,
            first,
            keys: keys as u16,
            orientation,
            velocity,
        },
    ))
}
fn native_error(message: String) -> mlua::Error {
    if message.starts_with("memory:") {
        stop(Category::Memory, message)
    } else {
        bad(message)
    }
}
fn create(lua: &Lua, image: &LuaImage, options: Table) -> mlua::Result<Plan> {
    let job = image.lease(lua)?;
    {
        let mut slot = lua
            .app_data_mut::<ImageSlot>()
            .ok_or_else(|| bad("Image scope unavailable."))?;
        if slot.plan_attempts >= MAX_PLANS {
            return Err(stop(
                Category::Budget,
                "At most16image-note plan attempts are allowed per job.",
            ));
        }
        slot.plan_attempts += 1;
    }
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Native image note accounting unavailable."))?
        .clone();
    let metadata = budget.reserve(METADATA_BYTES).map_err(|e| e.lua())?;
    let (settings, mapping) = parse(&options)?;
    image.with_pixels(lua, |rgba| {
        let (lines, len) = if mapping.orientation == im::Orientation::PianoRoll {
            (rgba.h, rgba.w)
        } else {
            (rgba.w, rgba.h)
        };
        if u32::from(mapping.first) + lines > u32::from(mapping.keys) {
            return Err(bad("Image note rows extend beyond the chosen key range; resize the image or change first_key/keys."));
        }
        let end_tick = mapping.tick(len)?;
        let cells = rgba.px.len() as u64;
        let work = cells.checked_mul(settings.palette.len() as u64)
            .ok_or_else(|| bad("Image note work estimate overflowed."))?;
        {
            let mut slot = lua.app_data_mut::<ImageSlot>().unwrap();
            if slot.plan_pixels.saturating_add(cells) > MAX_PIXELS
                || slot.plan_work.saturating_add(work) > MAX_WORK
            {
                return Err(stop(Category::Budget,"Image note plans exceed16,777,216cumulative pixels or268,435,456palette comparisons per job; reduce image/palette size."));
            }
            slot.plan_pixels += cells;
            slot.plan_work += work;
        }
        let mut allocation = None;
        let mut preflight_error = None;
        let mut checkpoint_error = None;
        let indices = runs::index_controlled(
            rgba,
            &settings,
            |bytes| match budget.reserve(bytes) {
                Ok(guard) => { allocation = Some(guard); Ok(()) }
                Err(error) => { preflight_error = Some(error.lua()); Err("Image note preflight refused.".into()) }
            },
            || match notes::check_budget(lua) {
                Ok(()) => Ok(()),
                Err(error) => { checkpoint_error = Some(error); Err("Image note indexing interrupted.".into()) }
            },
        );
        if let Some(error) = checkpoint_error.or(preflight_error) { return Err(error); }
        let indices = indices.map_err(native_error)?;
        let initial = runs::Cursor::new(&indices, rgba.w, rgba.h, &settings).map_err(native_error)?;
        let mut cursor = initial;
        let mut count = 0u64;
        loop {
            let mut interrupted = None;
            let n = cursor.visit_controlled(
                &indices,
                runs::MAX_BATCH,
                |_| { count += 1; Ok(()) },
                || match notes::check_budget(lua) {
                    Ok(()) => Ok(()),
                    Err(error) => { interrupted = Some(error); Err("Image note estimate interrupted.".into()) }
                },
            );
            if let Some(error) = interrupted { return Err(error); }
            if n.map_err(native_error)? == 0 { break; }
        }
        notes::check_budget(lua)?;
        let data = Rc::new(RefCell::new(Some(Stored {
            indices, cursor: initial, initial, width: rgba.w, height: rgba.h,
            settings: mapping, palette: settings.palette, count, read: 0,
            end_tick, _allocation: allocation.unwrap(),
        })));
        lua.app_data_mut::<ImageSlot>().unwrap().plans.push(Rc::downgrade(&data));
        Ok(Plan { job, data, _metadata: metadata })
    })
}
impl Plan {
    fn with<T>(&self, lua: &Lua, f: impl FnOnce(&Stored) -> mlua::Result<T>) -> mlua::Result<T> {
        check_scope(lua, &self.job, "image note plan")?;
        let data = self
            .data
            .try_borrow()
            .map_err(|_| bad("The image note plan is busy."))?;
        f(data
            .as_ref()
            .ok_or_else(|| stop(Category::Expired, "This image note plan is closed."))?)
    }
}
impl UserData for Plan {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("info", |lua, this, ()| {
            this.with(lua, |s| {
                let t = lua.create_table()?;
                t.set("width", s.width)?;
                t.set("height", s.height)?;
                t.set("keys", s.settings.keys)?;
                t.set("first_key", s.settings.first)?;
                t.set(
                    "orientation",
                    if s.settings.orientation == im::Orientation::PianoRoll {
                        "piano_roll"
                    } else {
                        "upstream"
                    },
                )?;
                t.set("note_count", s.count)?;
                t.set("order", "line_time")?;
                t.set("palette_base", 1)?;
                t.set("read", s.read)?;
                t.set("remaining", s.count - s.read)?;
                t.set("done", s.read == s.count)?;
                t.set("palette_count", s.palette.len())?;
                t.set("start_tick", s.settings.origin)?;
                t.set("end_tick", s.end_tick)?;
                t.set("tick_numerator", s.settings.num)?;
                t.set("tick_denominator", s.settings.den)?;
                t.set("index_bytes", s.indices.len() * 4)?;
                t.set(
                    "note_record_bytes",
                    s.count * size_of::<NoteRecord>() as u64,
                )?;
                Ok(t)
            })
        });
        m.add_method("palette", |lua, this, ()| {
            this.with(lua, |s| {
                let out = lua.create_table_with_capacity(s.palette.len(), 0)?;
                for (i, c) in s.palette.iter().enumerate() {
                    out.raw_set(i + 1, lua.create_sequence_from(c.iter().copied())?)?;
                }
                Ok(out)
            })
        });
        m.add_method("reset", |lua, this, ()| {
            check_scope(lua, &this.job, "image note plan")?;
            let mut data = this
                .data
                .try_borrow_mut()
                .map_err(|_| bad("The image note plan is busy."))?;
            let s = data
                .as_mut()
                .ok_or_else(|| stop(Category::Expired, "This image note plan is closed."))?;
            s.cursor = s.initial;
            s.read = 0;
            Ok(())
        });
        m.add_method("next", |lua, this, count: Option<Value>| {
            check_scope(lua, &this.job, "image note plan")?;
            let count = integer(
                count.unwrap_or(Value::Integer(MAX_BATCH as i64)),
                MAX_BATCH as u32,
                "Image note page count",
            )? as usize;
            if count == 0 {
                return Err(bad("Image note page count must be1through4096."));
            }
            let mut data = this
                .data
                .try_borrow_mut()
                .map_err(|_| bad("The image note plan is busy."))?;
            let s = data
                .as_mut()
                .ok_or_else(|| stop(Category::Expired, "This image note plan is closed."))?;
            let capacity = count.min((s.count - s.read) as usize);
            let _temporary = lua
                .app_data_ref::<NativeBudget>()
                .unwrap()
                .reserve(capacity * size_of::<Row>())
                .map_err(|e| e.lua())?;
            let mut rows = Vec::new();
            rows.try_reserve_exact(capacity).map_err(|_| {
                stop(
                    Category::Memory,
                    "Image note page storage could not be allocated.",
                )
            })?;
            let mut cursor = s.cursor;
            let mut mapping_error = None;
            let mut checkpoint_error = None;
            let (lines, _) = cursor.dimensions();
            let result = if capacity == 0 {
                Ok(0)
            } else {
                cursor.visit_controlled(
                    &s.indices,
                    count,
                    |run| match s.settings.row(run, lines) {
                        Ok(row) => {
                            rows.push(row);
                            Ok(())
                        }
                        Err(e) => {
                            mapping_error = Some(e);
                            Err("Image note mapping failed.".into())
                        }
                    },
                    || match notes::check_budget(lua) {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            checkpoint_error = Some(e);
                            Err("Image note read interrupted.".into())
                        }
                    },
                )
            };
            if let Some(e) = mapping_error.or(checkpoint_error) {
                return Err(e);
            }
            result.map_err(native_error)?;
            let out = lua.create_table()?;
            let start = lua.create_table_with_capacity(rows.len(), 0)?;
            let gate = lua.create_table_with_capacity(rows.len(), 0)?;
            let key = lua.create_table_with_capacity(rows.len(), 0)?;
            let velocity = lua.create_table_with_capacity(rows.len(), 0)?;
            let palette = lua.create_table_with_capacity(rows.len(), 0)?;
            for (i, row) in rows.iter().enumerate() {
                start.raw_set(i + 1, row.start)?;
                gate.raw_set(i + 1, row.gate)?;
                key.raw_set(i + 1, row.key)?;
                velocity.raw_set(i + 1, row.velocity)?;
                palette.raw_set(i + 1, row.palette)?;
            }
            out.set("start", start)?;
            out.set("gate", gate)?;
            out.set("key", key)?;
            out.set("velocity", velocity)?;
            out.set("palette", palette)?;
            notes::check_budget(lua)?;
            s.cursor = cursor;
            s.read += rows.len() as u64;
            Ok(out)
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
pub(super) fn add_methods<M: UserDataMethods<LuaImage>>(m: &mut M) {
    m.add_method("note_plan", |lua, this, options: Table| {
        create(lua, this, options)
    });
}
#[cfg(test)]
#[path = "image_note_tests.rs"]
mod tests;
