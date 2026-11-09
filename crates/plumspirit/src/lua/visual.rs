//! Immutable native view facts. No rendering, pointer hooks or musical edits.
use super::{
    Capability, Category, Host,
    bridge::{JobLease, NativeBudget, ProjectStamp, Reservation, Session},
    notes, stop, ticks,
};
use crate::piano_roll::viewport::ViewFrame;
use egui::Rect;
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use std::sync::Arc;

const MAX_SNAPSHOTS: usize = 32;
const SNAPSHOT_BYTES: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerFrame {
    pub rect: Rect,
    pub rain: Rect,
    pub keyboard: Rect,
    pub keys: u16,
    pub mapping: crate::player::rain::Mapping,
    pub seconds_ahead: f64,
    pub pixels_per_point: f32,
    pub stamp: ProjectStamp,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Facts {
    Roll(ViewFrame),
    Player(PlayerFrame),
}
impl Facts {
    pub fn rect(self) -> Rect {
        match self {
            Self::Roll(f) => f.rect,
            Self::Player(f) => f.rect,
        }
    }
    pub fn scale(self) -> f32 {
        match self {
            Self::Roll(f) => f.pixels_per_point,
            Self::Player(f) => f.pixels_per_point,
        }
    }
    pub fn index(self) -> usize {
        match self {
            Self::Roll(_) => 0,
            Self::Player(_) => 1,
        }
    }
    pub fn valid(self) -> bool {
        let rect = self.rect();
        let rect_valid = |r: Rect| {
            r.is_finite()
                && r.width() > 0.0
                && r.height() > 0.0
                && r.width() <= 1_000_000.0
                && r.height() <= 1_000_000.0
        };
        if !rect_valid(rect)
            || !self.scale().is_finite()
            || self.scale() <= 0.0
            || self.scale() > 32.0
        {
            return false;
        }
        match self {
            Self::Roll(f) => {
                matches!(f.view.pitch.top, 127 | 255)
                    && (-i128::from(u64::MAX)..=i128::from(u64::MAX)).contains(&f.shift)
                    && f.view.time.pixels_per_tick.is_finite()
                    && (plumspirit_engine::view::MIN_PIXELS_PER_TICK
                        ..=plumspirit_engine::view::MAX_PIXELS_PER_TICK)
                        .contains(&f.view.time.pixels_per_tick)
                    && f.view.pitch.scroll_rows.is_finite()
                    && f.view.pitch.row_height.is_finite()
                    && (plumspirit_engine::view::MIN_ROW_HEIGHT
                        ..=plumspirit_engine::view::MAX_ROW_HEIGHT)
                        .contains(&f.view.pitch.row_height)
            }
            Self::Player(f) => {
                rect_valid(f.rain)
                    && rect_valid(f.keyboard)
                    && rect.contains_rect(f.rain)
                    && rect.contains_rect(f.keyboard)
                    && matches!(f.keys, 128 | 256)
                    && f.seconds_ahead.is_finite()
                    && f.seconds_ahead > 0.0
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub stamp: ProjectStamp,
    pub revision: u64,
    pub facts: Facts,
}
#[derive(Default)]
pub struct Capture {
    pub enabled: bool,
    pub player: Option<PlayerFrame>,
    last: [Option<Snapshot>; 2],
    sequence: u64,
    pub(crate) notified: [Option<Snapshot>; 2],
    pub(crate) notified_project: Option<u64>,
    pub(crate) notified_at: Option<std::time::Instant>,
}
impl Capture {
    pub fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.last = [None, None];
            self.player = None;
        }
    }
    pub fn sample(
        &mut self,
        index: usize,
        stamp: ProjectStamp,
        facts: Option<Facts>,
    ) -> Option<Snapshot> {
        if index >= self.last.len() {
            return None;
        }
        let Some(facts) = facts.filter(|f| self.enabled && f.index() == index && f.valid()) else {
            self.last[index] = None;
            return None;
        };
        if let Some(previous) = self.last[index].filter(|s| s.stamp == stamp && s.facts == facts) {
            return Some(previous);
        }
        let Some(sequence) = self
            .sequence
            .checked_add(1)
            .filter(|n| *n <= ticks::MAX_LUA_TICK)
        else {
            self.last = [None, None];
            self.enabled = false;
            return None;
        };
        self.sequence = sequence;
        let snapshot = Snapshot {
            stamp,
            revision: self.sequence,
            facts,
        };
        self.last[index] = Some(snapshot);
        Some(snapshot)
    }
}
#[derive(Default)]
pub(super) struct Slot {
    count: usize,
}
pub(super) fn begin_run(lua: &Lua) {
    if let Some(mut slot) = lua.app_data_mut::<Slot>() {
        slot.count = 0;
    }
}
struct View {
    snapshot: Snapshot,
    lease: Arc<JobLease>,
    _metadata: Reservation,
}
pub(super) fn reference(lua: &Lua, value: &mlua::AnyUserData) -> mlua::Result<Snapshot> {
    let view = value
        .borrow::<View>()
        .map_err(|_| bad("Publish using an opaque native view snapshot."))?;
    check(lua, &view)?;
    Ok(view.snapshot)
}
fn bad(message: &str) -> mlua::Error {
    stop(Category::Runtime, message)
}
fn finite(value: f64) -> mlua::Result<f64> {
    if !value.is_finite() || value.abs() > 1e12 {
        Err(bad("View coordinates must be finite and within 1e12."))
    } else {
        Ok(value)
    }
}
fn check(lua: &Lua, view: &View) -> mlua::Result<()> {
    super::reactive::check_read(lua)?;
    notes::check_budget(lua)?;
    view.lease.check().map_err(|e| e.lua())?;
    if !lua.app_data_ref::<Session>().is_some_and(|s| {
        s.lease
            .as_ref()
            .is_some_and(|l| Arc::ptr_eq(l, &view.lease))
    }) {
        return Err(stop(
            Category::Expired,
            "This view snapshot belongs to a finished job.",
        ));
    }
    Ok(())
}
fn rect_table(lua: &Lua, rect: Rect) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("x", rect.left())?;
    t.set("y", rect.top())?;
    t.set("width", rect.width())?;
    t.set("height", rect.height())?;
    Ok(t)
}
fn point(lua: &Lua, x: f64, y: f64) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("x", finite(x)?)?;
    t.set("y", finite(y)?)?;
    Ok(t)
}
fn info(lua: &Lua, s: Snapshot) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("schema", 1)?;
    t.set("project", ticks::to_lua(s.stamp.serial)?)?;
    t.set("project_revision", ticks::to_lua(s.stamp.revision)?)?;
    t.set("view_revision", ticks::to_lua(s.revision)?)?;
    t.set("bounds", rect_table(lua, s.facts.rect())?)?;
    t.set("pixels_per_point", s.facts.scale())?;
    match s.facts {
        Facts::Roll(f) => {
            t.set("surface", "piano_roll")?;
            t.set("track", f.track.0)?;
            t.set("content", f.content.map(|c| c.0))?;
            t.set("song_shift", f.shift.to_string())?;
            t.set("locked", f.locked)?;
            t.set("conductor", f.conductor)?;
            t.set("keys", u16::from(f.view.pitch.top) + 1)?;
            t.set("origin_tick", ticks::to_lua(f.view.time.origin_tick)?)?;
            t.set("points_per_tick", f.view.time.pixels_per_tick)?;
            t.set("row_height", f.view.pitch.row_height)?;
            t.set("scroll_rows", f.view.pitch.scroll_rows)?;
            let (a, b) = f.view.time.visible_range(f64::from(f.rect.width()));
            t.set("first_tick", ticks::to_lua(a)?)?;
            t.set("last_tick", ticks::to_lua(b)?)?;
        }
        Facts::Player(f) => {
            t.set("surface", "player")?;
            t.set("keys", f.keys)?;
            t.set("mapping", f.mapping.key())?;
            t.set("seconds_ahead", f.seconds_ahead)?;
            t.set("rainfall_bounds", rect_table(lua, f.rain)?)?;
            t.set("keyboard_bounds", rect_table(lua, f.keyboard)?)?;
        }
    }
    Ok(t)
}
fn roll(view: &View) -> mlua::Result<ViewFrame> {
    match view.snapshot.facts {
        Facts::Roll(f) => Ok(f),
        _ => Err(bad(
            "Note-space conversion is only available for a Piano Roll view; Player coordinates use its own normalized surface.",
        )),
    }
}
fn shifted(tick: u64, shift: i128) -> Option<i64> {
    let result = i128::from(tick).checked_add(shift)?;
    (0..=i128::from(i64::MAX))
        .contains(&result)
        .then_some(result as i64)
}
impl UserData for View {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("info", |lua, view, ()| {
            check(lua, view)?;
            info(lua, view.snapshot)
        });
        m.add_method("to_screen", |lua, view, (u, v): (f64, f64)| {
            check(lua, view)?;
            let r = view.snapshot.facts.rect();
            point(
                lua,
                f64::from(r.left()) + finite(u)? * f64::from(r.width()),
                f64::from(r.top()) + finite(v)? * f64::from(r.height()),
            )
        });
        m.add_method("from_screen", |lua, view, (x, y): (f64, f64)| {
            check(lua, view)?;
            let r = view.snapshot.facts.rect();
            point(
                lua,
                (finite(x)? - f64::from(r.left())) / f64::from(r.width()),
                (finite(y)? - f64::from(r.top())) / f64::from(r.height()),
            )
        });
        m.add_method("note_to_uv", |lua, view, (tick, key): (Value, Value)| {
            check(lua, view)?;
            let f = roll(view)?;
            let tick = ticks::from_lua(&tick, "Local view tick")?;
            let Value::Integer(key) = key else {
                return Err(bad("View key must be an integer."));
            };
            if !(0..=i64::from(f.view.pitch.top)).contains(&key) {
                return Err(bad("View key exceeds the captured project key mode."));
            }
            point(
                lua,
                f.view.time.tick_to_x(tick) / f64::from(f.rect.width()),
                (f.view.pitch.pitch_top_y(key as u8) + f.view.pitch.row_height / 2.0)
                    / f64::from(f.rect.height()),
            )
        });
        m.add_method("uv_to_note", |lua, view, (u, v): (f64, f64)| {
            check(lua, view)?;
            let f = roll(view)?;
            let u = finite(u)?;
            let v = finite(v)?;
            let x = u * f64::from(f.rect.width());
            let y = v * f64::from(f.rect.height());
            let tick = f.view.time.x_to_tick(x);
            let t = lua.create_table()?;
            t.set("local_tick", ticks::to_lua(tick)?)?;
            t.set("song_tick", shifted(tick, f.shift))?;
            t.set("key", f.view.pitch.y_to_pitch(y))?;
            t.set(
                "pitch_position",
                f64::from(f.view.pitch.top) + 0.5
                    - f.view.pitch.scroll_rows
                    - y / f.view.pitch.row_height,
            )?;
            t.set(
                "inside",
                (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v),
            )?;
            Ok(t)
        });
        m.add_method("local_to_song", |lua, view, tick: Value| {
            check(lua, view)?;
            Ok(shifted(
                ticks::from_lua(&tick, "Local tick")?,
                roll(view)?.shift,
            ))
        });
        m.add_method("song_to_local", |lua, view, tick: Value| {
            check(lua, view)?;
            Ok(shifted(
                ticks::from_lua(&tick, "Song tick")?,
                -roll(view)?.shift,
            ))
        });
    }
}
pub(super) fn install(lua: &Lua, ui: &Table) -> mlua::Result<()> {
    ui.set(
        "view_snapshot",
        lua.create_function(|lua, surface: String| {
            super::reactive::check_read(lua)?;
            notes::check_budget(lua)?;
            let index = match surface.as_str() {
                "piano_roll" => 0,
                "player" => 1,
                _ => return Err(bad("View surface must be 'piano_roll' or 'player'.")),
            };
            let grants = lua
                .app_data_ref::<super::handles::Granted>()
                .map(|g| Arc::clone(&g.0))
                .unwrap_or_default();
            Host::require(&grants, Capability::UiOverlay)?;
            Host::require(
                &grants,
                if index == 0 {
                    Capability::ReadProject
                } else {
                    Capability::ReadPlayback
                },
            )?;
            let lease = lua
                .app_data_ref::<Session>()
                .and_then(|s| s.lease.clone())
                .ok_or_else(|| stop(Category::Expired, "No view job is active."))?;
            lease.check().map_err(|e| e.lua())?;
            let snapshot = lua
                .app_data_ref::<super::handles::WorldSlot>()
                .and_then(|w| w.0.as_ref().and_then(|w| w.info.views[index]))
                .or_else(|| {
                    lua.app_data_ref::<super::reactive::Scope>().and_then(|s| {
                        s.delivery
                            .as_ref()
                            .filter(|d| d.valid())
                            .and_then(|d| d.views[index])
                    })
                });
            let Some(snapshot) = snapshot else {
                return Ok(None);
            };
            if lease.project != Some(snapshot.stamp) || !snapshot.facts.valid() {
                return Err(stop(
                    Category::Expired,
                    "This native view no longer matches the project snapshot.",
                ));
            }
            let mut slot = lua
                .app_data_mut::<Slot>()
                .ok_or_else(|| bad("View snapshots are unavailable."))?;
            if slot.count >= MAX_SNAPSHOTS {
                return Err(stop(
                    Category::Budget,
                    "At most32view snapshots may be captured in one job.",
                ));
            }
            let budget = lua
                .app_data_ref::<NativeBudget>()
                .map(|b| b.clone())
                .ok_or_else(|| bad("View accounting is unavailable."))?;
            let metadata = budget.reserve(SNAPSHOT_BYTES).map_err(|e| e.lua())?;
            slot.count += 1;
            Ok(Some(View {
                snapshot,
                lease,
                _metadata: metadata,
            }))
        })?,
    )
}

#[cfg(test)]
#[path = "visual_tests.rs"]
mod tests;
