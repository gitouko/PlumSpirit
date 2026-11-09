//! The Events / CC API (v1.0-beta1 B1-0030, `docs/LUA_API.md` "Events").
//!
//! `t:events{...}` / `c:events{...}` make an event query over a track's (or a
//! clip content's) preserved events: controllers, pitch bend, program
//! changes, pressure, unpaired note messages, and — read only, as opaque
//! bytes — meta events, SysEx and system messages. Tempo and time
//! signatures are not track events: they live in the Conductor
//! (`project:tempos()`, `tx:set_tempo`). Writes (`lua/tx.rs`) change channel
//! messages only; everything else is kept exactly as it is.

use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use plumspirit_engine::model::{EventStore, RawEvent, RawKind};

use super::handles::{Handle, LuaHandle, current_world};
use super::notes::{CHECK_NOTES, Host, check_budget, host_of};
use super::{Category, stop, ticks};

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// The kinds of events, as scripts name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    NoteOff,
    NoteOn,
    PolyPressure,
    Cc,
    Program,
    ChannelPressure,
    PitchBend,
    Meta,
    SysEx,
    SysExEscape,
    System,
}

impl Kind {
    pub const ALL: [Kind; 11] = [
        Kind::NoteOff,
        Kind::NoteOn,
        Kind::PolyPressure,
        Kind::Cc,
        Kind::Program,
        Kind::ChannelPressure,
        Kind::PitchBend,
        Kind::Meta,
        Kind::SysEx,
        Kind::SysExEscape,
        Kind::System,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::NoteOff => "note_off",
            Kind::NoteOn => "note_on",
            Kind::PolyPressure => "poly_pressure",
            Kind::Cc => "cc",
            Kind::Program => "program",
            Kind::ChannelPressure => "channel_pressure",
            Kind::PitchBend => "pitch_bend",
            Kind::Meta => "meta",
            Kind::SysEx => "sysex",
            Kind::SysExEscape => "sysex_escape",
            Kind::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    pub fn of(e: &RawEvent) -> Kind {
        match e.kind {
            RawKind::Channel { status, .. } => match status & 0xF0 {
                0x80 => Kind::NoteOff,
                0x90 => Kind::NoteOn,
                0xA0 => Kind::PolyPressure,
                0xB0 => Kind::Cc,
                0xC0 => Kind::Program,
                0xD0 => Kind::ChannelPressure,
                _ => Kind::PitchBend,
            },
            RawKind::Meta { .. } => Kind::Meta,
            RawKind::SysEx { .. } => Kind::SysEx,
            RawKind::SysExEscape { .. } => Kind::SysExEscape,
            RawKind::System { .. } => Kind::System,
        }
    }

    /// Kinds a script may add or change.
    pub fn writable(self) -> bool {
        matches!(
            self,
            Kind::PolyPressure | Kind::Cc | Kind::Program | Kind::ChannelPressure | Kind::PitchBend
        )
    }

    fn status(self) -> u8 {
        match self {
            Kind::PolyPressure => 0xA0,
            Kind::Cc => 0xB0,
            Kind::Program => 0xC0,
            Kind::ChannelPressure => 0xD0,
            _ => 0xE0,
        }
    }

    /// The range of the script-facing `value`.
    pub fn value_range(self) -> (i64, i64) {
        match self {
            Kind::PitchBend => (-8192, 8191),
            _ => (0, 127),
        }
    }
}

/// The script-facing value of a channel event (pitch bend: −8192 – 8191).
pub fn value_of(e: &RawEvent) -> Option<i64> {
    let RawKind::Channel { status, data } = e.kind else {
        return None;
    };
    Some(match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 => i64::from(data[1]),
        0xC0 | 0xD0 => i64::from(data[0]),
        _ => (i64::from(data[1]) << 7 | i64::from(data[0])) - 8192,
    })
}

/// A channel event of `kind` with the script-facing `value`.
pub fn make(kind: Kind, tick: u64, channel: u8, number: u8, value: i64) -> RawEvent {
    let status = kind.status() | (channel & 0x0F);
    let data = match kind {
        Kind::PitchBend => {
            let raw = (value + 8192).clamp(0, 16383) as u16;
            [(raw & 0x7F) as u8, (raw >> 7) as u8]
        }
        Kind::Program | Kind::ChannelPressure => [value.clamp(0, 127) as u8, 0],
        _ => [number & 0x7F, value.clamp(0, 127) as u8],
    };
    RawEvent {
        tick,
        anchor: 0,
        kind: RawKind::Channel { status, data },
    }
}

/// Which events a query selects.
#[derive(Clone, Debug, Default)]
pub struct EventFilter {
    pub kinds: Option<Vec<Kind>>,
    pub channel: Option<u8>,
    /// Controller number (cc) or key (poly pressure, note messages).
    pub number: Option<u8>,
    pub t0: u64,
    pub t1: Option<u64>,
}

impl EventFilter {
    pub fn from_lua(t: Option<&Table>) -> mlua::Result<EventFilter> {
        let mut f = EventFilter::default();
        let Some(t) = t else {
            return Ok(f);
        };
        f.kinds = match t.get::<Value>("kind")? {
            Value::Nil => None,
            Value::String(s) => Some(vec![parse_kind(&s.to_string_lossy())?]),
            Value::Table(list) => {
                let mut v = Vec::new();
                for k in list.sequence_values::<String>() {
                    v.push(parse_kind(&k?)?);
                }
                Some(v)
            }
            other => {
                return Err(runtime(format!(
                    "kind must be a name or a list of names, not {}.",
                    other.type_name()
                )));
            }
        };
        if let Some(c) = t.get::<Option<i64>>("channel")? {
            f.channel = Some(
                u8::try_from(c)
                    .ok()
                    .filter(|&c| c <= 15)
                    .ok_or_else(|| runtime(format!("channel must be 0 - 15; {c} is not.")))?,
            );
        }
        let number = match t.get::<Option<i64>>("controller")? {
            Some(n) => Some(n),
            None => t.get::<Option<i64>>("key")?,
        };
        if let Some(n) = number {
            f.number =
                Some(u8::try_from(n).ok().filter(|&n| n <= 127).ok_or_else(|| {
                    runtime(format!("A controller or key is 0 - 127; {n} is not."))
                })?);
        }
        if let Value::Nil = t.get::<Value>("start_tick")? {
        } else {
            f.t0 = ticks::from_lua(&t.get::<Value>("start_tick")?, "start_tick")?;
        }
        f.t1 = match t.get::<Value>("end_tick")? {
            Value::Nil => None,
            v => Some(ticks::from_lua(&v, "end_tick")?),
        };
        Ok(f)
    }

    pub fn accepts(&self, e: &RawEvent) -> bool {
        if e.tick < self.t0 || self.t1.is_some_and(|t1| e.tick >= t1) {
            return false;
        }
        let kind = Kind::of(e);
        if self.kinds.as_ref().is_some_and(|ks| !ks.contains(&kind)) {
            return false;
        }
        match e.kind {
            RawKind::Channel { status, data } => {
                self.channel.is_none_or(|c| status & 0x0F == c)
                    && self.number.is_none_or(|n| {
                        matches!(status & 0xF0, 0x80 | 0x90 | 0xA0 | 0xB0) && data[0] == n
                    })
            }
            _ => self.channel.is_none() && self.number.is_none(),
        }
    }
}

fn parse_kind(s: &str) -> mlua::Result<Kind> {
    match s {
        "tempo" | "time_signature" => Err(runtime(format!(
            "{s} is not a track event: tempo and time signatures belong to the Conductor (project:tempos(), tx:set_tempo)."
        ))),
        _ => Kind::parse(s).ok_or_else(|| {
            runtime(format!(
                "There is no event kind \u{201C}{s}\u{201D} (cc, pitch_bend, program, channel_pressure, poly_pressure, note_on, note_off, meta, sysex, sysex_escape, system)."
            ))
        }),
    }
}

/// Indices (0-based) of the events `f` selects (budget-checked).
pub fn matching(lua: &Lua, store: &EventStore, f: &EventFilter) -> mlua::Result<Vec<usize>> {
    let ev = store.events();
    let start = ev.partition_point(|e| e.tick < f.t0);
    let end =
        f.t1.map_or(ev.len(), |t1| ev.partition_point(|e| e.tick < t1));
    let mut out = Vec::new();
    for (k, i) in (start..end.max(start)).enumerate() {
        if k % CHECK_NOTES == CHECK_NOTES - 1 {
            check_budget(lua)?;
        }
        if f.accepts(&ev[i]) {
            out.push(i);
        }
    }
    Ok(out)
}

/// An event as a script table.
pub fn event_table(lua: &Lua, store: &EventStore, index: usize) -> mlua::Result<Table> {
    let e = store.events()[index];
    let t = lua.create_table()?;
    t.set("tick", ticks::to_lua(e.tick)?)?;
    t.set("index", index as i64 + 1)?;
    let kind = Kind::of(&e);
    t.set("kind", kind.name())?;
    match e.kind {
        RawKind::Channel { status, data } => {
            t.set("channel", i64::from(status & 0x0F))?;
            match kind {
                Kind::NoteOn | Kind::NoteOff => {
                    t.set("key", i64::from(data[0]))?;
                    t.set("velocity", i64::from(data[1]))?;
                }
                Kind::PolyPressure => t.set("key", i64::from(data[0]))?,
                Kind::Cc => t.set("controller", i64::from(data[0]))?,
                Kind::PitchBend => {
                    t.set("raw", i64::from(data[1]) << 7 | i64::from(data[0]))?;
                }
                _ => {}
            }
            if !matches!(kind, Kind::NoteOn | Kind::NoteOff) {
                t.set("value", value_of(&e).unwrap_or(0))?;
            }
        }
        RawKind::Meta { meta_type, payload } => {
            t.set("meta_type", i64::from(meta_type))?;
            t.set("data", lua.create_string(store.payload(payload))?)?;
        }
        RawKind::SysEx { payload } | RawKind::SysExEscape { payload } => {
            t.set("data", lua.create_string(store.payload(payload))?)?;
        }
        RawKind::System { status, data } => {
            t.set("status", i64::from(status))?;
            t.set("data1", i64::from(data[0]))?;
            t.set("data2", i64::from(data[1]))?;
        }
    }
    Ok(t)
}

/// An event query as Lua userdata.
pub struct LuaEventQuery {
    pub host: Handle,
    pub filter: EventFilter,
}

/// The query's host and the matching indices in the current world.
pub fn evaluate(lua: &Lua, q: &LuaEventQuery) -> mlua::Result<(Host, Vec<usize>)> {
    let host = host_of(lua, &q.host)?;
    let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
    let store = w
        .doc
        .events_of(host.content_ref())
        .ok_or_else(|| runtime("Those events no longer exist."))?;
    Ok((host, matching(lua, store, &q.filter)?))
}

impl UserData for LuaEventQuery {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("count", |lua, this, ()| {
            Ok(evaluate(lua, this)?.1.len() as i64)
        });
        m.add_method("list", |lua, this, limit: Option<i64>| {
            let limit = limit.unwrap_or(100_000).max(0) as usize;
            let (host, idx) = evaluate(lua, this)?;
            let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
            let store = w
                .doc
                .events_of(host.content_ref())
                .ok_or_else(|| runtime("Those events no longer exist."))?;
            let out = lua.create_table()?;
            for (k, &i) in idx.iter().take(limit).enumerate() {
                out.raw_set(k + 1, event_table(lua, store, i)?)?;
            }
            Ok(out)
        });
        m.add_method("handles", |lua, this, limit: Option<i64>| {
            let limit = limit.unwrap_or(100_000).max(0) as usize;
            let (host, idx) = evaluate(lua, this)?;
            let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
            super::handles::handle_list(
                lua,
                idx.iter()
                    .take(limit)
                    .filter_map(|&i| w.event(host.content_ref(), i)),
            )
        });
    }
}

/// Registers `events` on track and clip content handles.
pub fn add_methods<M: UserDataMethods<LuaHandle>>(m: &mut M) {
    m.add_method("events", |lua, this, opts: Option<Table>| {
        let filter = EventFilter::from_lua(opts.as_ref())?;
        host_of(lua, &this.0)?;
        Ok(LuaEventQuery {
            host: this.0.clone(),
            filter,
        })
    });
}

/// An event to add, from a script table (`base`: the event it replaces).
pub fn event_from_table(
    t: &Table,
    base: Option<&RawEvent>,
    default_channel: u8,
) -> mlua::Result<RawEvent> {
    let kind = match t.get::<Option<String>>("kind")? {
        Some(k) => parse_kind(&k)?,
        None => base
            .map(Kind::of)
            .ok_or_else(|| runtime("An event needs a kind (cc, pitch_bend, program, ...)."))?,
    };
    if !kind.writable() {
        return Err(runtime(format!(
            "{} events cannot be added or changed by scripts (only cc, pitch_bend, program, channel_pressure and poly_pressure).",
            kind.name()
        )));
    }
    let tick = match t.get::<Value>("tick")? {
        Value::Nil => base
            .map(|b| b.tick)
            .ok_or_else(|| runtime("An event needs a tick."))?,
        v => ticks::from_lua(&v, "tick")?,
    };
    let (base_channel, base_number) = match base.map(|b| b.kind) {
        Some(RawKind::Channel { status, data }) => (Some(status & 0x0F), Some(data[0])),
        _ => (None, None),
    };
    let channel = match t.get::<Option<i64>>("channel")? {
        Some(c) => u8::try_from(c).ok().filter(|&c| c <= 15).ok_or_else(|| {
            runtime(format!(
                "A MIDI channel is 0 - 15 (shown as 1 - 16); {c} is not."
            ))
        })?,
        None => base_channel.unwrap_or(default_channel),
    };
    let number = match kind {
        Kind::Cc | Kind::PolyPressure => {
            let n = match kind {
                Kind::Cc => t.get::<Option<i64>>("controller")?,
                _ => t.get::<Option<i64>>("key")?,
            };
            let n = n.or(base_number.map(i64::from)).ok_or_else(|| {
                runtime(match kind {
                    Kind::Cc => "A cc event needs a controller number (0 - 127).",
                    _ => "A poly_pressure event needs a key (0 - 127).",
                })
            })?;
            u8::try_from(n)
                .ok()
                .filter(|&n| n <= 127)
                .ok_or_else(|| runtime(format!("A controller or key is 0 - 127; {n} is not.")))?
        }
        _ => 0,
    };
    let (lo, hi) = kind.value_range();
    let value = match t.get::<Option<i64>>("value")? {
        Some(v) => v,
        None => base
            .and_then(value_of)
            .ok_or_else(|| runtime("An event needs a value."))?,
    };
    if value < lo || value > hi {
        return Err(runtime(format!(
            "A {} value is {lo} - {hi}; {value} is not.",
            kind.name()
        )));
    }
    Ok(make(kind, tick, channel, number, value))
}
